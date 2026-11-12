//! "Start with Windows": the `Discord Clip` value under
//! `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`.
//!
//! The registry is the source of truth (not `config.ini`), so the toggle in
//! Settings always matches what Windows does, and the installer's "start
//! when I sign in" choice shows up there. The installer and uninstaller use
//! the same value name.

use std::ffi::c_void;
use std::path::{Path, PathBuf};

use anyhow::{bail, Result};
use windows::core::{w, HSTRING, PCWSTR};
use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS, WIN32_ERROR};
use windows::Win32::System::Registry::{
    RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW, HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_BINARY, RRF_RT_REG_SZ,
};

const RUN_KEY: PCWSTR = w!(r"Software\Microsoft\Windows\CurrentVersion\Run");
/// Task Manager's "Startup apps" on/off switches.
const APPROVED_KEY: PCWSTR = w!(r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run");
const VALUE: PCWSTR = w!("Discord Clip");

#[derive(Clone, Debug, Default, PartialEq)]
pub enum State {
    #[default]
    Off,
    /// Starts this exe at sign-in.
    On,
    /// Windows starts a different copy (e.g. a moved portable exe).
    OtherCopy(PathBuf),
    /// Our entry exists but is switched off in Task Manager → Startup apps.
    DisabledByWindows,
}

pub fn state() -> State {
    state_for(&crate::update::exe_path())
}

fn state_for(exe: &Path) -> State {
    let Some(command) = read_string(RUN_KEY, VALUE) else { return State::Off };
    let target = command_exe(&command);
    if !same_path(&target, exe) {
        return State::OtherCopy(target);
    }
    if disabled_in_task_manager() {
        return State::DisabledByWindows;
    }
    State::On
}

/// Turns starting this exe at sign-in on or off. Turning it on also clears a
/// "disabled" switch from Task Manager, so it really starts.
pub fn set(on: bool) -> Result<()> {
    unsafe {
        if on {
            let exe = crate::update::exe_path();
            let command: Vec<u16> = format!("\"{}\"", exe.display()).encode_utf16().chain([0]).collect();
            check(
                RegSetKeyValueW(
                    HKEY_CURRENT_USER,
                    RUN_KEY,
                    VALUE,
                    REG_SZ.0,
                    Some(command.as_ptr() as *const c_void),
                    (command.len() * 2) as u32,
                ),
                "turning on start with Windows",
            )?;
        } else {
            let e = RegDeleteKeyValueW(HKEY_CURRENT_USER, RUN_KEY, VALUE);
            if e != ERROR_FILE_NOT_FOUND {
                check(e, "turning off start with Windows")?;
            }
        }
        // No StartupApproved entry means "enabled"; a stale one would
        // otherwise keep it switched off (or linger after turning it off).
        let _ = RegDeleteKeyValueW(HKEY_CURRENT_USER, APPROVED_KEY, VALUE);
    }
    Ok(())
}

fn check(e: WIN32_ERROR, what: &str) -> Result<()> {
    if e == ERROR_SUCCESS {
        Ok(())
    } else {
        bail!("{what}: {}", windows::core::Error::from(e.to_hresult()).message())
    }
}

fn read_string(key: PCWSTR, value: PCWSTR) -> Option<String> {
    unsafe {
        let mut size: u32 = 0;
        if RegGetValueW(HKEY_CURRENT_USER, key, value, RRF_RT_REG_SZ, None, None, Some(&mut size)) != ERROR_SUCCESS {
            return None;
        }
        let mut buf = vec![0u16; (size as usize).div_ceil(2)];
        let ok = RegGetValueW(
            HKEY_CURRENT_USER,
            key,
            value,
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr() as *mut c_void),
            Some(&mut size),
        );
        if ok != ERROR_SUCCESS {
            return None;
        }
        let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        Some(String::from_utf16_lossy(&buf[..len]))
    }
}

/// Task Manager stores 12 bytes per app; an odd first byte (e.g. 03) means
/// switched off. No entry means on.
fn disabled_in_task_manager() -> bool {
    let mut data = [0u8; 16];
    let mut size = data.len() as u32;
    let e = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            APPROVED_KEY,
            VALUE,
            RRF_RT_REG_BINARY,
            None,
            Some(data.as_mut_ptr() as *mut c_void),
            Some(&mut size),
        )
    };
    e == ERROR_SUCCESS && size > 0 && data[0] & 1 == 1
}

/// The program part of a Run command: `"C:\x\app.exe" --flag` → `C:\x\app.exe`.
fn command_exe(command: &str) -> PathBuf {
    let c = command.trim();
    let path = match c.strip_prefix('"') {
        Some(rest) => rest.split('"').next().unwrap_or(rest),
        None => c.split(" -").next().unwrap_or(c).trim(),
    };
    PathBuf::from(path)
}

fn same_path(a: &Path, b: &Path) -> bool {
    let norm = |p: &Path| HSTRING::from(p.as_os_str()).to_string_lossy().to_lowercase().replace('/', "\\");
    norm(a) == norm(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_run_commands() {
        assert_eq!(command_exe(r#""C:\Program Files\App\app.exe" --min"#), PathBuf::from(r"C:\Program Files\App\app.exe"));
        assert_eq!(command_exe(r#""C:\x\discord-clip.exe""#), PathBuf::from(r"C:\x\discord-clip.exe"));
        assert_eq!(command_exe(r"C:\x\app.exe -silent"), PathBuf::from(r"C:\x\app.exe"));
        assert_eq!(command_exe(r"C:\x\app.exe"), PathBuf::from(r"C:\x\app.exe"));
    }

    #[test]
    fn compares_paths_loosely() {
        assert!(same_path(Path::new(r"C:\Users\A\App.exe"), Path::new(r"c:/users/a/app.EXE")));
        assert!(!same_path(Path::new(r"C:\a\app.exe"), Path::new(r"C:\b\app.exe")));
    }

    /// Touches the real registry (HKCU only): `cargo test -- --ignored autostart`.
    /// Saves an existing "Discord Clip" startup entry first and puts it back
    /// afterwards, even if an assertion fails.
    #[test]
    #[ignore]
    fn autostart_round_trip() {
        struct Restore(Option<String>);
        impl Drop for Restore {
            fn drop(&mut self) {
                unsafe {
                    let _ = RegDeleteKeyValueW(HKEY_CURRENT_USER, APPROVED_KEY, VALUE);
                    match &self.0 {
                        Some(cmd) => {
                            let w: Vec<u16> = cmd.encode_utf16().chain([0]).collect();
                            let _ = RegSetKeyValueW(HKEY_CURRENT_USER, RUN_KEY, VALUE, REG_SZ.0, Some(w.as_ptr() as *const c_void), (w.len() * 2) as u32);
                        }
                        None => {
                            let _ = RegDeleteKeyValueW(HKEY_CURRENT_USER, RUN_KEY, VALUE);
                        }
                    }
                }
            }
        }
        let _restore = Restore(read_string(RUN_KEY, VALUE));
        set(false).unwrap();
        let exe = crate::update::exe_path();
        assert_eq!(state_for(&exe), State::Off);

        set(true).unwrap();
        assert_eq!(state_for(&exe), State::On);
        assert_eq!(command_exe(&read_string(RUN_KEY, VALUE).unwrap()), exe);

        // Switched off in Task Manager: 03 + timestamp.
        let disabled = [3u8, 0, 0, 0, 1, 2, 3, 4, 5, 6, 7, 8];
        unsafe {
            RegSetKeyValueW(
                HKEY_CURRENT_USER,
                APPROVED_KEY,
                VALUE,
                windows::Win32::System::Registry::REG_BINARY.0,
                Some(disabled.as_ptr() as *const c_void),
                disabled.len() as u32,
            );
        }
        assert_eq!(state_for(&exe), State::DisabledByWindows);
        set(true).unwrap(); // turning it on again clears Task Manager's switch
        assert_eq!(state_for(&exe), State::On);

        assert_eq!(state_for(Path::new(r"C:\elsewhere\discord-clip.exe")), State::OtherCopy(exe.clone()));

        set(false).unwrap();
        assert_eq!(state_for(&exe), State::Off);
        assert!(read_string(RUN_KEY, VALUE).is_none());
        assert!(!disabled_in_task_manager());
        set(false).unwrap(); // already off: no error
    }
}
