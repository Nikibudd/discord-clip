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
    pub name: String,
    pub emoji: Option<String>,
    /// False when the server lost the boosts for this slot.
    #[serde(default = "yes")]
    pub available: bool,
}

fn yes() -> bool {
    true
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
    fn names_from_files() {
        assert_eq!(default_name("discord_clip_20261113_211451 AI ception"), "AI ception");
        assert_eq!(default_name("discord_clip_20261113_211451"), "clip");
        assert_eq!(default_name("my sound"), "my sound");
        assert_eq!(default_name(&format!("discord_clip_1_2 {}", "a".repeat(40))).len(), 32);
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
