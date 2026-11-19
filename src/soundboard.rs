//! Talks to a friend group's soundboard service (#76, see `worker/`): lists
//! the soundboard servers you share with its bot and uploads clips.
//!
//! Every call blocks (network), so the window runs them on worker threads.

use std::path::Path;

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use serde_json::{json, Value};

/// Discord's soundboard limits.
pub const MAX_BYTES: u64 = 512 * 1024;
pub const MAX_NAME: usize = 32;
pub const MIN_NAME: usize = 2;

/// Enough for any answer the service gives.
const MAX_ANSWER: usize = 1024 * 1024;

#[derive(Clone, Debug)]
pub struct Service {
    url: String,
    key: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Server {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub can_upload: bool,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Board {
    pub used: u32,
    /// `None` when Discord gave the server extra slots it doesn't report.
    pub total: Option<u32>,
    #[serde(default)]
    pub sounds: Vec<Sound>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Sound {
    /// Empty from a Worker older than #81.
    #[serde(default)]
    pub id: String,
    pub name: String,
    pub emoji: Option<String>,
    /// 0..=1.
    #[serde(default = "one")]
    pub volume: f32,
    /// False when the server lost the boosts for this slot.
    #[serde(default = "yes")]
    pub available: bool,
    /// Added by the group's bot, so it can change or delete it (#81). Always
    /// false from a Worker older than #81.
    #[serde(default)]
    pub by_bot: bool,
}

fn yes() -> bool {
    true
}

fn one() -> f32 {
    1.0
}

/// What changing a sound changes (#81); `None` leaves it as it is. An empty
/// emoji removes it.
#[derive(Debug, Default)]
pub struct SoundEdit {
    pub name: Option<String>,
    pub emoji: Option<String>,
    pub volume: Option<f32>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct InviteServer {
    pub name: String,
    pub has_bot: bool,
    pub install_url: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Invites {
    pub servers: Vec<InviteServer>,
    #[serde(default)]
    pub invalid: Vec<String>,
}

/// What to upload.
pub struct Upload<'a> {
    pub user: &'a str,
    pub guild: &'a str,
    pub name: &'a str,
    pub emoji: &'a str,
    /// 0..=1.
    pub volume: f32,
    pub file: &'a Path,
}

impl Service {
    /// The configured service, if a URL is set.
    pub fn new(url: &str, key: Option<String>) -> Option<Service> {
        let url = url.trim().trim_end_matches('/');
        (!url.is_empty()).then(|| Service { url: url.to_string(), key: key.filter(|k| !k.is_empty()) })
    }

    /// From the config and the saved key.
    pub fn configured(cfg: &crate::config::Config) -> Option<Service> {
        Service::new(&cfg.soundboard_url, crate::secret::upload_key())
    }

    /// Checks the service answers; returns its bot's name.
    pub fn check(&self) -> Result<String> {
        let v = self.call("GET", "/", None)?;
        Ok(v["bot"]["name"].as_str().unwrap_or("?").to_string())
    }

    /// All the bot's servers; `board` tells which ones you're in.
    pub fn servers(&self) -> Result<Vec<Server>> {
        let v = self.call("GET", "/servers", None)?;
        Ok(serde_json::from_value(v["servers"].clone()).context("reading the server list")?)
    }

    /// A server's soundboard, or `None` when you're not in that server.
    pub fn board(&self, server: &str, user: &str) -> Result<Option<Board>> {
        let (status, v) = self.call_raw("GET", &format!("/servers/{server}?user={user}"), None)?;
        if status == 403 && v["not_member"] == true {
            return Ok(None);
        }
        let v = check(status, v)?;
        Ok(Some(serde_json::from_value(v).context("reading the soundboard")?))
    }

    /// Pasted text with invite links -> the servers, with install links for
    /// the ones the bot isn't in.
    pub fn invites(&self, text: &str) -> Result<Invites> {
        let v = self.call("POST", "/invites", Some(json!({ "links": text })))?;
        Ok(serde_json::from_value(v).context("reading the invite answer")?)
    }

    pub fn upload(&self, u: &Upload) -> Result<()> {
        let bytes = std::fs::read(u.file).with_context(|| format!("reading {}", u.file.display()))?;
        if bytes.len() as u64 > MAX_BYTES {
            bail!("the clip is larger than Discord's 512 KB limit");
        }
        let mime = match u.file.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase).as_deref() {
            Some("mp3") => "audio/mpeg",
            Some("ogg") => "audio/ogg",
            _ => bail!("only MP3 clips can be uploaded; make it Discord-ready first"),
        };
        let body = json!({
            "user": u.user,
            "guild": u.guild,
            "name": u.name,
            "emoji": u.emoji,
            "volume": u.volume.clamp(0.0, 1.0),
            "sound": format!("data:{mime};base64,{}", base64(&bytes)),
        });
        self.call("POST", "/upload", Some(body)).map(|_| ())
    }

    /// Changes one of the bot's sounds (#81).
    pub fn edit_sound(&self, server: &str, sound: &str, user: &str, edit: &SoundEdit) -> Result<()> {
        let mut body = serde_json::Map::new();
        if let Some(n) = &edit.name {
            body.insert("name".into(), json!(n.trim()));
        }
        if let Some(e) = &edit.emoji {
            body.insert("emoji".into(), json!(e.trim()));
        }
        if let Some(v) = edit.volume {
            body.insert("volume".into(), json!(v.clamp(0.0, 1.0)));
        }
        self.call("PATCH", &format!("/sounds/{server}/{sound}?user={user}"), Some(Value::Object(body))).map(|_| ())
    }

    /// Deletes one of the bot's sounds (#81).
    pub fn delete_sound(&self, server: &str, sound: &str, user: &str) -> Result<()> {
        self.call("DELETE", &format!("/sounds/{server}/{sound}?user={user}"), None).map(|_| ())
    }

    fn call(&self, method: &str, path: &str, body: Option<Value>) -> Result<Value> {
        let (status, v) = self.call_raw(method, path, body)?;
        check(status, v)
    }

    fn call_raw(&self, method: &str, path: &str, body: Option<Value>) -> Result<(u32, Value)> {
        let auth = self.key.as_ref().map(|k| format!("Key {k}"));
        let mut headers = vec![("Content-Type", "application/json")];
        if let Some(a) = &auth {
            headers.push(("Authorization", a));
        }
        let body = body.map(|b| b.to_string());
        let r = crate::http::request(
            method,
            &format!("{}{path}", self.url),
            &headers,
            body.as_deref().map(str::as_bytes),
            MAX_ANSWER,
        )
        .context("couldn't reach the soundboard service")?;
        Ok((r.status, serde_json::from_slice(&r.body).unwrap_or(Value::Null)))
    }
}

fn check(status: u32, v: Value) -> Result<Value> {
    if (200..300).contains(&status) {
        return Ok(v);
    }
    bail!("{}", error_text(status, &v))
}

/// A readable message for a failed call.
fn error_text(status: u32, v: &Value) -> String {
    let msg = v["error"].as_str().map(str::to_string);
    // Discord's own reason, e.g. "Maximum number of soundboard sounds reached".
    let discord = first_field_error(&v["discord"]["errors"]).or(v["discord"]["message"].as_str());
    match (status, msg) {
        (401, _) => "the upload key is wrong or missing (Settings)".into(),
        (404, None) => "the soundboard service address looks wrong (Settings)".into(),
        (_, Some(m)) => match discord {
            Some(d) if d != m => format!("{m} ({d})"),
            _ => m,
        },
        (s, None) => format!("the soundboard service answered HTTP {s}"),
    }
}

/// The first message in Discord's nested `errors` object.
fn first_field_error(v: &Value) -> Option<&str> {
    match v {
        Value::Object(map) => {
            if let Some(Value::Array(list)) = map.get("_errors") {
                if let Some(m) = list.first().and_then(|e| e["message"].as_str()) {
                    return Some(m);
                }
            }
            map.values().find_map(first_field_error)
        }
        _ => None,
    }
}

/// Standard base64 with padding.
fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = (b[0] as u32) << 16 | (b[1] as u32) << 8 | b[2] as u32;
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 { T[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if chunk.len() > 2 { T[n as usize & 63] as char } else { '=' });
    }
    out
}

/// A default sound name from a clip's file name: the part after the
/// timestamp, cut to Discord's 32 characters.
pub fn default_name(file_stem: &str) -> String {
    let rest = file_stem.strip_prefix("discord_clip_").map(|s| {
        // "20261113_211451 AI ception" -> "AI ception"
        s.split_once(' ').map(|(_, n)| n).unwrap_or("")
    });
    let name = match rest {
        Some(n) if n.trim().chars().count() >= MIN_NAME => n.trim(),
        Some(_) => "clip",
        None => file_stem.trim(),
    };
    name.chars().take(MAX_NAME).collect::<String>().trim_end().to_string()
}

/// Downloads a soundboard sound from Discord's CDN (sounds are public) into a
/// temporary folder, once, and returns the file (#81). The extension follows
/// the content: Discord keeps MP3 and Ogg sounds.
pub fn fetch_sound(id: &str) -> Result<std::path::PathBuf> {
    if id.is_empty() || !id.chars().all(|c| c.is_ascii_digit()) {
        bail!("this sound has no id; update the soundboard service (redeploy the Worker)");
    }
    let dir = std::env::temp_dir().join("discord-clip-sounds");
    for ext in ["mp3", "ogg"] {
        let p = dir.join(format!("{id}.{ext}"));
        if p.exists() {
            return Ok(p);
        }
    }
    let r = crate::http::request("GET", &format!("https://cdn.discordapp.com/soundboard-sounds/{id}"), &[], None, 2 * MAX_BYTES as usize)
        .context("couldn't reach Discord")?;
    if r.status != 200 {
        bail!("Discord answered HTTP {} for this sound", r.status);
    }
    let ext = if r.body.starts_with(b"OggS") { "ogg" } else { "mp3" };
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{id}.{ext}"));
    let tmp = dir.join(format!("{id}.tmp"));
    std::fs::write(&tmp, &r.body)?;
    std::fs::rename(&tmp, &path)?;
    Ok(path)
}

/// When a Discord id (a snowflake) was made, in local time: for a sound,
/// when it was added to the server (#91).
pub fn snowflake_time(id: &str) -> Option<crate::library::SavedAt> {
    use windows::Win32::Foundation::{FILETIME, SYSTEMTIME};
    use windows::Win32::System::Time::{FileTimeToSystemTime, SystemTimeToTzSpecificLocalTime};

    const DISCORD_EPOCH_MS: u64 = 1_420_070_400_000;
    const FILETIME_UNIX_EPOCH: u64 = 116_444_736_000_000_000;
    let ms = (id.parse::<u64>().ok()? >> 22).checked_add(DISCORD_EPOCH_MS)?;
    let ticks = ms.checked_mul(10_000)?.checked_add(FILETIME_UNIX_EPOCH)?;
    let ft = FILETIME { dwLowDateTime: ticks as u32, dwHighDateTime: (ticks >> 32) as u32 };
    let (mut utc, mut local) = (SYSTEMTIME::default(), SYSTEMTIME::default());
    unsafe {
        FileTimeToSystemTime(&ft, &mut utc).ok()?;
        SystemTimeToTzSpecificLocalTime(None, &utc, &mut local).ok()?;
    }
    Some(crate::library::SavedAt {
        year: local.wYear,
        month: local.wMonth as u8,
        day: local.wDay as u8,
        hour: local.wHour as u8,
        minute: local.wMinute as u8,
        second: local.wSecond as u8,
    })
}

/// Saves a server's soundboard sound into the clips folder as a normal clip
/// (#91): `discord_clip_<when it was added> <name>`, in the format Discord
/// serves it (MP3 or Ogg Opus), or converted to `format` when the app can't
/// read that one. The clip is marked as uploaded to `server`, remembering
/// the sound so it isn't saved twice. Returns the new clip.
pub fn save_to_clips(sound: &Sound, server: &crate::uploaded::Server, dir: &Path, format: crate::audio::Format) -> Result<std::path::PathBuf> {
    let src = fetch_sound(&sound.id)?;
    let at = snowflake_time(&sound.id).unwrap_or_else(crate::library::SavedAt::now);
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let name = sound.name.trim();
    let path = if crate::audio::read(&src).is_ok() {
        let ext = src.extension().and_then(|e| e.to_str()).unwrap_or("mp3");
        let path = crate::library::new_clip_path(dir, at, name, ext);
        std::fs::copy(&src, &path).with_context(|| format!("copying the sound to {}", path.display()))?;
        path
    } else {
        // e.g. Ogg Vorbis: only Opus is read as Ogg.
        let mut audio = crate::audio::read_any(&src).context("the app can't read this sound's format")?;
        if format == crate::audio::Format::Opus {
            audio = crate::audio::resample(&audio, 48_000); // Opus is always 48 kHz
        }
        let path = crate::library::new_clip_path(dir, at, name, format.extension());
        crate::audio::write(&path, &audio, format)?;
        path
    };
    let mark = crate::uploaded::Server { sound: sound.id.clone(), ..server.clone() };
    if let Err(e) = crate::uploaded::add_server(&path, mark) {
        crate::log!("marking saved sound {} as uploaded failed: {e:#}", sound.id);
    }
    Ok(path)
}

impl Board {
    /// The sound already called `name`, ignoring case and surrounding spaces
    /// (#82). Only used for a warning: Discord allows duplicate names.
    pub fn same_name(&self, name: &str) -> Option<&Sound> {
        let name = name.trim().to_lowercase();
        if name.is_empty() {
            return None;
        }
        self.sounds.iter().find(|s| s.name.trim().to_lowercase() == name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_base64() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
        assert_eq!(base64(&[0xff, 0xfe, 0xfd, 0x00]), "//79AA==");
    }

    #[test]
    fn reads_sounds_old_and_new() {
        // A Worker from before #81: no id, volume or by_bot.
        let old: Sound = serde_json::from_value(json!({ "name": "a", "emoji": null })).unwrap();
        assert_eq!((old.id.as_str(), old.volume, old.by_bot, old.available), ("", 1.0, false, true));
        let new: Sound =
            serde_json::from_value(json!({ "id": "123", "name": "a", "emoji": "🤖", "volume": 0.5, "by_bot": true })).unwrap();
        assert_eq!((new.id.as_str(), new.volume, new.by_bot), ("123", 0.5, true));
    }

    /// Downloads real sounds from Discord's CDN and decodes them: one the
    /// bot uploaded (MP3 from the app) and one a person added in Discord.
    #[test]
    #[ignore]
    fn live_fetches_and_decodes_sounds() {
        for id in ["1571576358625345536", "1569822572294574112", "1570180160811044924"] {
            let path = fetch_sound(id).unwrap();
            let audio = crate::audio::read(&path);
            println!("{id}: {} -> {:?}", path.display(), audio.as_ref().map(|a| (a.channels, a.rate, a.seconds())).map_err(|e| format!("{e:#}")));
            audio.unwrap();
        }
    }

    #[test]
    fn reads_when_sounds_were_added() {
        // Discord's docs example: 2016-04-30 11:18:25.796 UTC, shown in local time.
        let at = snowflake_time("175928847299117063").unwrap();
        assert_eq!(at.year, 2016);
        // Time zones shift by whole quarter hours.
        assert_eq!((at.minute % 15, at.second), (18 % 15, 25), "{at:?}");
        assert!(snowflake_time("").is_none());
        assert!(snowflake_time("abc").is_none());
    }

    /// Saves a real sound a person added in Discord (Ogg Opus) into a
    /// temporary clips folder. (Its uploaded mark goes into the real store
    /// and is dropped there on the next write, as the clip is gone.)
    #[test]
    #[ignore]
    fn live_saves_a_sound_to_clips() {
        let dir = std::env::temp_dir().join(format!("discord-clip-save-{}", std::process::id()));
        let sound = Sound { id: "1569822572294574112".into(), name: "test: sound".into(), emoji: None, volume: 1.0, available: true, by_bot: false };
        let server = crate::uploaded::Server { id: "1".into(), name: "Test".into(), sound: String::new() };
        let path = save_to_clips(&sound, &server, &dir, crate::audio::Format::Wav).unwrap();
        println!("{}", path.display());
        assert!(path.file_name().unwrap().to_string_lossy().ends_with(" test sound.ogg"));
        crate::audio::read(&path).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn names_from_files() {
        assert_eq!(default_name("discord_clip_20261113_211451 AI ception"), "AI ception");
        assert_eq!(default_name("discord_clip_20261113_211451"), "clip");
        assert_eq!(default_name("my sound"), "my sound");
        assert_eq!(default_name(&format!("discord_clip_1_2 {}", "a".repeat(40))).len(), 32);
    }

    #[test]
    fn finds_same_names_loosely() {
        let sound = |name: &str| Sound { id: String::new(), name: name.into(), emoji: None, volume: 1.0, available: true, by_bot: false };
        let b = Board { used: 2, total: Some(8), sounds: vec![sound("AI ception"), sound(" Bruh ")] };
        assert_eq!(b.same_name("ai CEPTION").map(|s| s.name.as_str()), Some("AI ception"));
        assert_eq!(b.same_name("  AI ception  ").map(|s| s.name.as_str()), Some("AI ception"));
        assert_eq!(b.same_name("bruh").map(|s| s.name.as_str()), Some(" Bruh "));
        assert!(b.same_name("AI ceptio").is_none());
        assert!(b.same_name("AIception").is_none());
        assert!(b.same_name("   ").is_none());
    }

    #[test]
    fn readable_errors() {
        let v = json!({ "error": "Discord refused.", "discord": { "message": "Invalid Form Body",
            "errors": { "name": { "_errors": [{ "message": "Must be between 2 and 32 in length." }] } } } });
        assert_eq!(error_text(400, &v), "Discord refused. (Must be between 2 and 32 in length.)");
        assert_eq!(error_text(401, &Value::Null), "the upload key is wrong or missing (Settings)");
        assert_eq!(error_text(502, &Value::Null), "the soundboard service answered HTTP 502");
        assert_eq!(first_field_error(&v["discord"]["errors"]), Some("Must be between 2 and 32 in length."));
    }
}
