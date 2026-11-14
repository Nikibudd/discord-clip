//! Update checks and in-app updates.
//!
//! Every release publishes an update manifest (`latest.json`, see the README)
//! at a fixed URL. We poll it, and when it names a newer version the user can
//! update from the window:
//!
//! * Installed copy: download the installer, check its SHA-256, run it
//!   silently and quit. The installer replaces the exe and starts it again.
//! * Portable copy: download the new exe, check it, swap it in for the
//!   running one (Windows allows renaming a running exe) and restart.
//!
//! Network work happens on short-lived worker threads; the tray thread is
//! told about changes through `tray::post_status_changed` and reads [`status`].

use std::ffi::c_void;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, SystemTime};

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use windows::core::{w, HSTRING, PCWSTR};
use windows::Win32::Networking::WinHttp::*;

use crate::log;

pub const MANIFEST_URL: &str = "https://github.com/Nikibudd/discord-clip/releases/latest/download/latest.json";
pub const RELEASES_URL: &str = "https://github.com/Nikibudd/discord-clip/releases";
pub const CURRENT: &str = env!("CARGO_PKG_VERSION");

const FIRST_CHECK_AFTER: Duration = Duration::from_secs(20);
const CHECK_EVERY: Duration = Duration::from_secs(6 * 60 * 60);

#[derive(Clone, Debug, Deserialize)]
pub struct Asset {
    pub name: String,
    pub url: String,
    pub size: u64,
    pub sha256: String,
}

/// `latest.json`. Only the fields we use; unknown ones are ignored.
#[derive(Clone, Debug, Deserialize)]
pub struct Manifest {
    pub schema: u32,
    pub version: String,
    #[serde(default)]
    pub published: String,
    #[serde(default)]
    pub notes_url: String,
    pub installer: Asset,
    pub portable: Asset,
}

#[derive(Clone, Debug, Default)]
pub enum State {
    #[default]
    Idle,
    Checking,
    UpToDate,
    Available(Manifest),
    Downloading {
        manifest: Manifest,
        done: u64,
        total: u64,
    },
    /// Downloaded and verified; the tray thread hands over to it.
    Ready {
        manifest: Manifest,
        file: PathBuf,
    },
    /// `manifest` is set when the check worked but the download failed.
    Failed {
        error: String,
        manifest: Option<Manifest>,
    },
}

#[derive(Clone, Debug, Default)]
pub struct Status {
    pub state: State,
    pub last_checked: Option<SystemTime>,
    /// Set on the first start after an update: the version we came from.
    pub updated_from: Option<String>,
}

static STATUS: Mutex<Status> = Mutex::new(Status { state: State::Idle, last_checked: None, updated_from: None });
/// A check or download is running.
static BUSY: AtomicBool = AtomicBool::new(false);
/// Automatic checks enabled (the `check_updates` setting).
pub static AUTO: AtomicBool = AtomicBool::new(true);
/// Path of our exe, recorded at startup (before a portable update renames it).
static EXE: OnceLock<PathBuf> = OnceLock::new();

pub fn status() -> Status {
    STATUS.lock().unwrap().clone()
}

fn set_state(state: State) {
    STATUS.lock().unwrap().state = state;
    crate::tray::post_status_changed();
}

/// Call once at startup.
pub fn init() {
    let exe = std::env::current_exe().unwrap_or_default();
    // Left behind by a portable update.
    let _ = std::fs::remove_file(old_exe_path(&exe));
    let _ = EXE.set(exe);
}

pub fn exe_path() -> PathBuf {
    EXE.get().cloned().unwrap_or_else(|| std::env::current_exe().unwrap_or_default())
}

fn old_exe_path(exe: &Path) -> PathBuf {
    let mut s = exe.as_os_str().to_owned();
    s.push(".old");
    PathBuf::from(s)
}

/// True if we're the copy the installer put in
/// `%LOCALAPPDATA%\Programs\discord-clip` (else we're a portable exe).
pub fn is_installed() -> bool {
    let Some(local) = std::env::var_os("LOCALAPPDATA") else { return false };
    let dir = PathBuf::from(local).join("Programs").join("discord-clip");
    exe_path()
        .parent()
        .is_some_and(|p| p.as_os_str().to_string_lossy().eq_ignore_ascii_case(&dir.to_string_lossy()))
}

/// Compares versions; `false` if either doesn't parse.
pub fn is_newer(candidate: &str, current: &str) -> bool {
    match (semver::Version::parse(candidate), semver::Version::parse(current)) {
        (Ok(a), Ok(b)) => a > b,
        _ => false,
    }
}

/// Remembers which version ran last. Returns the previous version if this
/// start is the first one after an update.
pub fn note_version() -> Option<String> {
    let path = crate::config::app_dir().join("last_version");
    let previous = std::fs::read_to_string(&path).ok().map(|s| s.trim().to_string());
    if previous.as_deref() != Some(CURRENT) {
        let _ = std::fs::create_dir_all(crate::config::app_dir());
        let _ = std::fs::write(&path, CURRENT);
    }
    let updated_from = previous.filter(|p| is_newer(CURRENT, p));
    STATUS.lock().unwrap().updated_from = updated_from.clone();
    updated_from
}

/// Release notes of the version we're running.
pub fn current_notes_url() -> String {
    format!("{RELEASES_URL}/tag/v{CURRENT}")
}

// ---------------------------------------------------------------------------
// Checking
// ---------------------------------------------------------------------------

/// Background thread: checks shortly after startup, then every few hours,
/// while automatic checks are on.
pub fn spawn_auto_checker(running: Arc<AtomicBool>) {
    let spawned = std::thread::Builder::new().name("update-checker".into()).spawn(move || {
        let mut wait = FIRST_CHECK_AFTER;
        loop {
            if !sleep_while(&running, wait) {
                return;
            }
            if AUTO.load(Ordering::SeqCst) {
                run_check();
            }
            wait = CHECK_EVERY;
        }
    });
    if let Err(e) = spawned {
        log!("couldn't start update checker: {e}");
    }
}

/// Returns false if we should stop.
fn sleep_while(running: &AtomicBool, total: Duration) -> bool {
    let step = Duration::from_millis(250);
    let mut slept = Duration::ZERO;
    while slept < total {
        if !running.load(Ordering::SeqCst) {
            return false;
        }
        std::thread::sleep(step);
        slept += step;
    }
    running.load(Ordering::SeqCst)
}

/// "Check for updates" button.
pub fn check_now() {
    let _ = std::thread::Builder::new().name("update-check".into()).spawn(run_check);
}

fn run_check() {
    if BUSY.swap(true, Ordering::SeqCst) {
        return;
    }
    // Don't interrupt an update that's already under way.
    if !matches!(status().state, State::Downloading { .. } | State::Ready { .. }) {
        set_state(State::Checking);
        let state = match fetch_manifest() {
            Ok(m) if is_newer(&m.version, CURRENT) => {
                log!("update available: {} (running {CURRENT})", m.version);
                State::Available(m)
            }
            Ok(m) => {
                log!("up to date ({CURRENT}, latest {})", m.version);
                State::UpToDate
            }
            Err(e) => {
                log!("update check failed: {e:#}");
                State::Failed { error: format!("{e:#}"), manifest: None }
            }
        };
        let mut s = STATUS.lock().unwrap();
        s.state = state;
        s.last_checked = Some(SystemTime::now());
        drop(s);
        crate::tray::post_status_changed();
    }
    BUSY.store(false, Ordering::SeqCst);
}

fn fetch_manifest() -> Result<Manifest> {
    let mut body = Vec::new();
    http_get(MANIFEST_URL, |chunk| {
        body.extend_from_slice(chunk);
        if body.len() > 256 * 1024 {
            bail!("update manifest is too large");
        }
        Ok(())
    })?;
    let m: Manifest = serde_json::from_slice(&body).context("reading update manifest")?;
    if m.schema != 1 {
        bail!("this version can't read the update info; download the new version from {RELEASES_URL}");
    }
    Ok(m)
}

// ---------------------------------------------------------------------------
// Downloading and installing
// ---------------------------------------------------------------------------

/// "Update now" button: downloads the update for the version found by the
/// last check. When it's verified, the tray thread is told to install it.
pub fn start_download() {
    let manifest = match status().state {
        State::Available(m) | State::Failed { manifest: Some(m), .. } => m,
        _ => return,
    };
    if BUSY.swap(true, Ordering::SeqCst) {
        return;
    }
    let spawned = std::thread::Builder::new().name("update-download".into()).spawn(move || {
        let asset = if is_installed() { &manifest.installer } else { &manifest.portable };
        log!("downloading update {} ({})", manifest.version, asset.name);
        match download(&manifest, asset) {
            Ok(file) => {
                log!("update downloaded and verified: {}", file.display());
                set_state(State::Ready { manifest: manifest.clone(), file });
                crate::tray::post_update_ready();
            }
            Err(e) => {
                log!("update download failed: {e:#}");
                set_state(State::Failed { error: format!("{e:#}"), manifest: Some(manifest.clone()) });
            }
        }
        BUSY.store(false, Ordering::SeqCst);
    });
    if spawned.is_err() {
        BUSY.store(false, Ordering::SeqCst);
    }
}

fn download(manifest: &Manifest, asset: &Asset) -> Result<PathBuf> {
    // Only ever a plain file name from our own manifest, but be safe.
    if asset.name.contains(['/', '\\']) || !asset.name.ends_with(".exe") {
        bail!("unexpected file name in update info: {}", asset.name);
    }
    let dir = std::env::temp_dir().join("discord-clip-update");
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let path = dir.join(&asset.name);
    let mut file = std::fs::File::create(&path).with_context(|| format!("creating {}", path.display()))?;

    let total = asset.size;
    let mut done = 0u64;
    let mut hasher = Sha256::new();
    let mut last_report = 0u64;
    set_state(State::Downloading { manifest: manifest.clone(), done, total });
    let result = http_get(&asset.url, |chunk| {
        file.write_all(chunk)?;
        hasher.update(chunk);
        done += chunk.len() as u64;
        if done > total {
            bail!("download is bigger than expected");
        }
        if done - last_report >= 128 * 1024 || done == total {
            last_report = done;
            set_state(State::Downloading { manifest: manifest.clone(), done, total });
        }
        Ok(())
    });
    drop(file);
    let verify = result.and_then(|()| {
        if done != total {
            bail!("download incomplete ({done} of {total} bytes)");
        }
        let hash: String = hasher.finalize().iter().map(|b| format!("{b:02x}")).collect();
        if !hash.eq_ignore_ascii_case(&asset.sha256) {
            bail!("downloaded file is corrupt (checksum mismatch)");
        }
        Ok(())
    });
    if let Err(e) = verify {
        let _ = std::fs::remove_file(&path);
        return Err(e);
    }
    Ok(path)
}

/// How an update was handed over.
pub enum Applied {
    /// The installer is running; quit and let it restart us.
    Installer,
    /// The exe was replaced; restart it.
    Replaced,
}

/// Tray thread: hands over to the downloaded update. On success the caller
/// must quit (and restart for [`Applied::Replaced`]).
pub fn apply() -> Result<Applied> {
    let State::Ready { manifest, file } = status().state else {
        bail!("no update is ready");
    };
    let result = if is_installed() {
        std::process::Command::new(&file)
            .args(["/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART"])
            .spawn()
            .with_context(|| format!("starting {}", file.display()))
            .map(|_| Applied::Installer)
    } else {
        replace_exe(&file).map(|()| Applied::Replaced)
    };
    match &result {
        Ok(_) => log!("installing update {}", manifest.version),
        Err(e) => {
            log!("couldn't install update: {e:#}");
            set_state(State::Failed { error: format!("{e:#}"), manifest: Some(manifest) });
        }
    }
    result
}

/// Portable update: move the running exe aside and put the new one in its
/// place. The `.old` file is deleted on the next start.
fn replace_exe(new: &Path) -> Result<()> {
    let exe = exe_path();
    let old = old_exe_path(&exe);
    let _ = std::fs::remove_file(&old);
    std::fs::rename(&exe, &old).with_context(|| format!("moving {} aside", exe.display()))?;
    if let Err(e) = std::fs::copy(new, &exe) {
        let _ = std::fs::rename(&old, &exe);
        return Err(e).with_context(|| format!("writing {}", exe.display()));
    }
    let _ = std::fs::remove_file(new);
    Ok(())
}

// ---------------------------------------------------------------------------
// HTTPS via WinHTTP (uses the system's TLS, proxy settings and certificates)
// ---------------------------------------------------------------------------

struct Handle(*mut c_void);

impl Handle {
    fn new(h: *mut c_void, what: &str) -> Result<Self> {
        if h.is_null() {
            Err(windows::core::Error::from_win32()).with_context(|| what.to_string())
        } else {
            Ok(Handle(h))
        }
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        unsafe {
            let _ = WinHttpCloseHandle(self.0);
        }
    }
}

/// GETs an https URL (following redirects), passing the body to `on_chunk`.
pub(crate) fn http_get(url: &str, mut on_chunk: impl FnMut(&[u8]) -> Result<()>) -> Result<()> {
    let rest = url.strip_prefix("https://").context("URLs must use https")?;
    let (host, path) = match rest.split_once('/') {
        Some((h, p)) => (h, format!("/{p}")),
        None => (rest, "/".to_string()),
    };
    unsafe {
        let agent = HSTRING::from(format!("discord-clip/{CURRENT}"));
        let session = Handle::new(
            WinHttpOpen(&agent, WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, PCWSTR::null(), PCWSTR::null(), 0),
            "starting WinHTTP",
        )?;
        WinHttpSetTimeouts(session.0, 15_000, 15_000, 30_000, 30_000)?;
        let connect = Handle::new(
            WinHttpConnect(session.0, &HSTRING::from(host), 443, 0),
            "connecting",
        )?;
        let request = Handle::new(
            WinHttpOpenRequest(
                connect.0,
                w!("GET"),
                &HSTRING::from(path),
                PCWSTR::null(),
                PCWSTR::null(),
                std::ptr::null(),
                WINHTTP_FLAG_SECURE,
            ),
            "creating request",
        )?;
        WinHttpSendRequest(request.0, None, None, 0, 0, 0).with_context(|| format!("connecting to {host}"))?;
        WinHttpReceiveResponse(request.0, std::ptr::null_mut()).with_context(|| format!("waiting for {host}"))?;

        let mut code: u32 = 0;
        let mut len = std::mem::size_of::<u32>() as u32;
        WinHttpQueryHeaders(
            request.0,
            WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
            PCWSTR::null(),
            Some(&mut code as *mut u32 as *mut c_void),
            &mut len,
            std::ptr::null_mut(),
        )?;
        match code {
            200 => {}
            404 => bail!("no release found on GitHub (HTTP 404)"),
            _ => bail!("GitHub answered HTTP {code}"),
        }

        let mut buf = vec![0u8; 64 * 1024];
        loop {
            let mut n: u32 = 0;
            WinHttpReadData(request.0, buf.as_mut_ptr() as *mut c_void, buf.len() as u32, &mut n)
                .context("downloading")?;
            if n == 0 {
                break;
            }
            on_chunk(&buf[..n as usize])?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_comparison() {
        assert!(is_newer("0.4.0", "0.3.0"));
        assert!(is_newer("0.3.1", "0.3.0"));
        assert!(is_newer("1.0.0", "0.9.9"));
        assert!(!is_newer("0.3.0", "0.3.0"));
        assert!(!is_newer("0.2.9", "0.3.0"));
        // A dev build of 0.3.0 is older than the 0.3.0 release.
        assert!(is_newer("0.3.0", "0.3.0-dev.gabc1234"));
        assert!(!is_newer("garbage", "0.3.0"));
    }

    /// Hits GitHub: `cargo test -- --ignored live`.
    #[test]
    #[ignore]
    fn live_manifest_and_download() {
        let m = fetch_manifest().expect("fetch manifest");
        assert!(semver::Version::parse(&m.version).is_ok(), "{}", m.version);
        let file = download(&m, &m.installer).expect("download + verify installer");
        assert_eq!(std::fs::metadata(&file).unwrap().len(), m.installer.size);
        let _ = std::fs::remove_file(file);
    }

    #[test]
    fn parses_manifest() {
        let m: Manifest = serde_json::from_str(
            r#"{"schema":1,"version":"0.3.0","tag":"v0.3.0","published":"2026-11-12T18:00:00Z",
                "notes_url":"https://x/y","min_windows":"10.0.19041",
                "installer":{"name":"a-setup.exe","url":"https://x/a","size":1,"sha256":"ab"},
                "portable":{"name":"a-portable.exe","url":"https://x/b","size":2,"sha256":"cd"}}"#,
        )
        .unwrap();
        assert_eq!(m.version, "0.3.0");
        assert_eq!(m.portable.size, 2);
    }
}
