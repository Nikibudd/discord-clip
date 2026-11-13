//! The theme the user has in Discord (#37): Light, Ash, Dark or Onyx, plus
//! the tint of a Nitro gradient or custom theme and the Saturation setting.
//!
//! Two places, because Discord writes them at different times:
//!
//! * `%APPDATA%\discord\settings.json`: `BACKGROUND_COLOR` is rewritten
//!   within a moment of every theme switch, and tells the base theme.
//! * Local Storage (a LevelDB folder, read with [`crate::leveldb`]): the theme
//!   name, the gradient preset, the custom theme and Saturation. Chromium only
//!   writes it to disk about once a minute.
//!
//! When the two disagree, Local Storage hasn't caught up yet: the base theme
//! from `settings.json` is used without a tint until it does.
//!
//! Privacy: the same Local Storage holds the user's login token and other
//! private data. Only the exact keys below are read, only theme fields are
//! taken from them, and nothing read is logged or copied.
//!
//! None of this is documented by Discord and may change with any update;
//! anything unexpected just gives `None`, and the app keeps its last theme.

use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Base {
    Light,
    Ash,
    Dark,
    Onyx,
}

impl Base {
    /// Discord's own ids (`ThemeStore`).
    pub fn id(self) -> &'static str {
        match self {
            Base::Light => "light",
            Base::Ash => "dark",
            Base::Dark => "darker",
            Base::Onyx => "midnight",
        }
    }

    pub fn from_id(id: &str) -> Option<Base> {
        [Base::Light, Base::Ash, Base::Dark, Base::Onyx].into_iter().find(|b| b.id() == id)
    }

    /// `settings.json`'s `BACKGROUND_COLOR` is `--background-base-lowest`.
    fn from_background(hex: &str) -> Option<Base> {
        match hex.trim().to_ascii_lowercase().as_str() {
            "#f3f3f4" => Some(Base::Light),
            "#2c2d32" => Some(Base::Ash),
            "#121214" => Some(Base::Dark),
            "#000000" => Some(Base::Onyx),
            _ => None,
        }
    }
}

/// A Nitro gradient preset or custom theme: a CSS-style linear gradient,
/// and how strongly it tints the colours (Discord's "base mix").
#[derive(Clone, Debug, PartialEq)]
pub struct Tint {
    /// Colour stops: colour and position along the gradient (0–1).
    pub stops: Vec<([u8; 3], f32)>,
    /// CSS angle in degrees: 0 = towards the top, 90 = towards the right.
    pub angle: f32,
    /// A custom theme; Discord mixes those in differently than presets.
    pub custom: bool,
    /// Base mix of a custom theme, 0–1 (0.5 for presets).
    pub mix: f32,
}

impl Tint {
    /// The colours the backgrounds are tinted with: (dark themes, Light).
    /// Like Discord: presets use their first and last colour, custom themes
    /// their darkest and lightest; a grey dark colour gives way to the other.
    pub fn base_colors(&self) -> Option<([u8; 3], [u8; 3])> {
        let luminance = |c: &[u8; 3]| {
            let lin = |v: u8| {
                let v = v as f32 / 255.0;
                if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
            };
            0.2126 * lin(c[0]) + 0.7152 * lin(c[1]) + 0.0722 * lin(c[2])
        };
        let colors: Vec<[u8; 3]> = self.stops.iter().map(|(c, _)| *c).collect();
        let (dark, light) = if self.custom {
            let by = |a: &&[u8; 3], b: &&[u8; 3]| luminance(a).total_cmp(&luminance(b));
            (*colors.iter().min_by(by)?, *colors.iter().max_by(by)?)
        } else {
            let (first, last) = (*colors.first()?, *colors.last()?);
            if luminance(&first) > luminance(&last) { (last, first) } else { (first, last) }
        };
        let grey = |c: [u8; 3]| c.iter().max() == c.iter().min();
        Some((if grey(dark) && !self.custom { light } else { dark }, light))
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct DiscordTheme {
    pub base: Base,
    pub tint: Option<Tint>,
    /// Discord's Saturation accessibility setting, 0–1.
    pub saturation: f32,
}

/// Discord's default "base mix" of custom themes, also used for presets.
const DEFAULT_MIX: f32 = 0.5;

/// The Local Storage keys we read, nothing else.
const KEY_PREFIX: &[u8] = b"_https://discord.com\x00\x01";
const KEYS: [&str; 4] = ["ThemeStore", "ClientThemesBackgroundStore", "AccessibilityStore", "UserSettingsProtoStore-Cache"];

/// Discord's data folder: stable, PTB or Canary, whichever was used last.
pub fn discord_dir() -> Option<PathBuf> {
    let appdata = PathBuf::from(std::env::var_os("APPDATA")?);
    ["discord", "discordptb", "discordcanary"]
        .iter()
        .map(|name| appdata.join(name))
        .filter_map(|dir| Some((std::fs::metadata(dir.join("settings.json")).ok()?.modified().ok()?, dir)))
        .max_by_key(|(modified, _)| *modified)
        .map(|(_, dir)| dir)
}

fn leveldb_dir(dir: &Path) -> PathBuf {
    dir.join("Local Storage").join("leveldb")
}

/// Reads the current theme; `None` if Discord isn't installed or its files
/// look different than expected.
pub fn read() -> Option<DiscordTheme> {
    read_from(&discord_dir()?)
}

fn read_from(dir: &Path) -> Option<DiscordTheme> {
    let settings_base = std::fs::read_to_string(dir.join("settings.json"))
        .ok()
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
        .and_then(|v| Base::from_background(v.get("BACKGROUND_COLOR")?.as_str()?));

    let wanted = |key: &[u8]| key.strip_prefix(KEY_PREFIX).is_some_and(|name| KEYS.iter().any(|k| k.as_bytes() == name));
    let store = crate::leveldb::read(&leveldb_dir(dir), &wanted).unwrap_or_default();
    let json = |name: &str| -> Option<serde_json::Value> {
        let mut key = KEY_PREFIX.to_vec();
        key.extend_from_slice(name.as_bytes());
        serde_json::from_str(&storage_text(store.get(&key)?)?).ok()
    };

    let stored_base = json("ThemeStore").and_then(|v| Base::from_id(v.pointer("/_state/theme")?.as_str()?));
    let base = settings_base.or(stored_base)?;
    // If Local Storage is behind settings.json, its tint is from an older theme.
    let tint = if stored_base == Some(base) {
        let proto = json("UserSettingsProtoStore-Cache")
            .and_then(|v| Some(v.pointer("/_state/1/proto")?.as_str()?.to_string()))
            .and_then(|b64| base64(&b64))
            .and_then(|bytes| client_theme(&bytes));
        match proto {
            Some(theme) => theme,
            None => json("ClientThemesBackgroundStore")
                .and_then(|v| v.pointer("/_state/gradientPresetId")?.as_u64())
                .and_then(preset),
        }
    } else {
        None
    };
    let saturation = json("AccessibilityStore")
        .and_then(|v| v.pointer("/_state/saturation")?.as_f64())
        .map_or(1.0, |s| s.clamp(0.0, 1.0) as f32);
    Some(DiscordTheme { base, tint, saturation })
}

/// Chromium stores Local Storage values with a leading format byte: 1 =
/// Latin-1, 0 = UTF-16LE.
fn storage_text(value: &[u8]) -> Option<String> {
    let (&format, rest) = value.split_first()?;
    match format {
        1 => Some(rest.iter().map(|&b| char::from(b)).collect()),
        0 => {
            let units: Vec<u16> = rest.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
            String::from_utf16(&units).ok()
        }
        _ => None,
    }
}

fn base64(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len() * 3 / 4);
    let (mut acc, mut bits) = (0u32, 0);
    for c in text.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            b'=' | b'\n' | b'\r' | b' ' => continue,
            _ => return None,
        };
        acc = (acc << 6) | u32::from(v);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Some(out)
}

// ---------------------------------------------------------------------------
// Synced settings (protobuf)
// ---------------------------------------------------------------------------

/// One protobuf field: its number, and its value as a varint, fixed32 or bytes.
enum Field<'a> {
    Varint(u64),
    Fixed32(u32),
    Bytes(&'a [u8]),
}

/// The top-level fields of a protobuf message; stops at anything unexpected.
fn fields(mut data: &[u8]) -> Vec<(u64, Field<'_>)> {
    fn varint(data: &mut &[u8]) -> Option<u64> {
        let mut v = 0u64;
        for shift in (0..64).step_by(7) {
            let (&b, rest) = data.split_first()?;
            *data = rest;
            v |= u64::from(b & 0x7F) << shift;
            if b & 0x80 == 0 {
                return Some(v);
            }
        }
        None
    }
    fn take<'a>(data: &mut &'a [u8], n: usize) -> Option<&'a [u8]> {
        let out = data.get(..n)?;
        *data = &data[n..];
        Some(out)
    }
    let mut out = Vec::new();
    while !data.is_empty() {
        let Some(tag) = varint(&mut data) else { break };
        let field = match tag & 7 {
            0 => varint(&mut data).map(Field::Varint),
            1 => take(&mut data, 8).map(|_| Field::Varint(0)), // fixed64: not needed
            2 => varint(&mut data).and_then(|n| take(&mut data, usize::try_from(n).ok()?)).map(Field::Bytes),
            5 => take(&mut data, 4).map(|b| Field::Fixed32(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))),
            _ => None,
        };
        let Some(field) = field else { break };
        out.push((tag >> 3, field));
    }
    out
}

fn message<'a>(data: &'a [u8], number: u64) -> Option<&'a [u8]> {
    fields(data).into_iter().rev().find_map(|(n, f)| match f {
        Field::Bytes(b) if n == number => Some(b),
        _ => None,
    })
}

/// The active gradient or custom theme from the cached synced settings
/// (`PreloadedUserSettings`): field 13 appearance → 3 client theme settings
/// → 2 gradient preset id (wrapped), or 4 custom theme (1 colours, 3 angle,
/// 4 base mix). Only that part of the message is looked at.
///
/// `Some(None)` = the settings say there's no gradient or custom theme;
/// `None` = they don't say (fall back to `ClientThemesBackgroundStore`).
fn client_theme(settings: &[u8]) -> Option<Option<Tint>> {
    let appearance = message(settings, 13)?;
    let Some(client) = message(appearance, 3) else { return Some(None) };
    if let Some(custom) = message(client, 4) {
        let number = |f: &Field| match *f {
            Field::Varint(v) => Some(v as i32 as f32),
            Field::Fixed32(v) => Some(f32::from_bits(v)),
            Field::Bytes(_) => None,
        };
        let mut colors = Vec::new();
        let (mut angle, mut mix) = (0.0, DEFAULT_MIX);
        for (n, f) in fields(custom) {
            match n {
                1 => {
                    if let Field::Bytes(b) = f {
                        colors.extend(std::str::from_utf8(b).ok().and_then(hex_color));
                    }
                }
                3 => angle = number(&f).filter(|a| a.is_finite()).unwrap_or(0.0),
                // Stored in percent (an older version may have used 0–1).
                4 => {
                    if let Some(v) = number(&f) {
                        mix = if v > 1.0 { v / 100.0 } else { v }.clamp(0.0, 1.0);
                    }
                }
                _ => {}
            }
        }
        if !colors.is_empty() {
            return Some(Some(Tint { stops: spread(&colors), angle, custom: true, mix }));
        }
    }
    if let Some(wrapped) = message(client, 2) {
        let id = fields(wrapped).into_iter().find_map(|(n, f)| match f {
            Field::Varint(v) if n == 1 => Some(v),
            _ => None,
        });
        // A wrapper without a value is preset 0.
        return Some(preset(id.unwrap_or(0)));
    }
    Some(None)
}

fn hex_color(s: &str) -> Option<[u8; 3]> {
    let s = s.trim().strip_prefix('#')?;
    if s.len() != 6 {
        return None;
    }
    let v = u32::from_str_radix(s, 16).ok()?;
    Some([(v >> 16) as u8, (v >> 8) as u8, v as u8])
}

/// Colours spread evenly over the gradient, like CSS stops without
/// positions (one colour fills the whole gradient).
fn spread(colors: &[[u8; 3]]) -> Vec<([u8; 3], f32)> {
    match colors {
        [] => vec![],
        [c] => vec![(*c, 0.0), (*c, 1.0)],
        _ => {
            let last = (colors.len() - 1) as f32;
            colors.iter().enumerate().map(|(i, c)| (*c, i as f32 / last)).collect()
        }
    }
}

/// Nitro gradient presets, by Discord's id: colour stops (colour, position
/// in percent) and angle, from Discord's web client (Nov 2026).
fn preset(id: u64) -> Option<Tint> {
    let (stops, angle): (&[(&str, f32)], f32) = match id {
        0 => (&[("#56b69f", 6.15), ("#63bc61", 48.7), ("#9eca67", 93.07)], 180.0),
        1 => (&[("#f3b336", 31.1), ("#ee8558", 67.09)], 180.0),
        2 => (&[("#3a7ca1", 5.64), ("#7f7eb9", 26.38), ("#7f7eb9", 49.92), ("#3a7ca1", 73.12)], 148.71),
        3 => (&[("#efaab3", 3.56), ("#efd696", 35.49), ("#a6daa2", 68.78)], 38.08),
        4 => (&[("#9f4175", 8.62), ("#c49064", 48.07), ("#a6953d", 76.04)], 154.19),
        5 => (&[("#f4abb8", 8.5), ("#b1c2fc", 94.28)], 180.14),
        6 => (&[("#a4c0f7", 7.08), ("#a9e4e8", 34.94), ("#b0e2b8", 65.12), ("#cfdfa2", 96.23)], 179.52),
        7 => (&[("#e7dbd0", 12.92), ("#dfd0b2", 32.92), ("#e0d6a3", 52.11)], 38.99),
        8 => (&[("#48288c", 27.57), ("#db7f4b", 71.25)], 141.68),
        9 => (
            &[("#0eb5bf", 3.94), ("#4c0ce0", 26.1), ("#a308a7", 39.82), ("#9a53ff", 56.89), ("#218be0", 76.45)],
            128.92,
        ),
        10 => (
            &[("#142215", 11.2), ("#2d4d39", 29.93), ("#454c32", 48.64), ("#5a7c58", 67.85), ("#a98e4b", 83.54)],
            162.27,
        ),
        11 => (&[("#950909", 16.17), ("#000000", 72.0)], 64.92),
        12 => (&[("#5348ca", 11.21), ("#140730", 61.92)], 48.17),
        13 => (&[("#895240", 14.61), ("#8f4343", 74.62)], 170.82),
        14 => (&[("#665069", 12.84), ("#91a3d1", 85.99)], 180.0),
        15 => (&[("#647962", 1.91), ("#588575", 48.99), ("#6a8482", 96.35)], 179.14),
        16 => (&[("#7289da", 4.0), ("#4e5d94", 96.0)], 180.0),
        17 => (&[("#3a7ca1", 5.64), ("#58579a", 26.38), ("#58579a", 49.92), ("#3a7ca1", 73.12)], 148.71),
        18 => (&[("#01a89e", 0.0), ("#7d60ba", 50.0), ("#b43898", 100.0)], 180.0),
        19 => (&[("#857664", 14.14), ("#5b4421", 60.35)], 69.98),
        20 => (&[("#af1a6c", 18.79), ("#c26b20", 49.76), ("#e7a525", 80.72)], 161.03),
        21 => (
            &[("#062053", 10.39), ("#191fbb", 26.87), ("#13929a", 48.31), ("#218573", 64.98), ("#051a81", 92.5)],
            239.16,
        ),
        22 => (&[("#2c3fe7", 11.18), ("#261d83", 64.54)], 47.61),
        23 => (&[("#4645ff", 0.0), ("#af01f3", 33.33), ("#ff6698", 66.67), ("#f89a03", 100.0)], 135.0),
        _ => return None,
    };
    let stops = stops.iter().filter_map(|(h, at)| Some((hex_color(h)?, at / 100.0))).collect();
    Some(Tint { stops, angle, custom: false, mix: DEFAULT_MIX })
}

// ---------------------------------------------------------------------------
// The last theme read, remembered for when Discord's files can't be read
// ---------------------------------------------------------------------------

fn remembered_path() -> PathBuf {
    crate::config::app_dir().join("discord-theme.txt")
}

impl DiscordTheme {
    fn to_text(&self) -> String {
        let mut out = format!("base = {}\nsaturation = {}\n", self.base.id(), self.saturation);
        if let Some(t) = &self.tint {
            let stops: Vec<String> = t.stops.iter().map(|([r, g, b], at)| format!("#{r:02x}{g:02x}{b:02x}@{at}")).collect();
            out += &format!("stops = {}\nangle = {}\ncustom = {}\nmix = {}\n", stops.join(","), t.angle, t.custom, t.mix);
        }
        out
    }

    fn from_text(text: &str) -> Option<DiscordTheme> {
        let get = |key: &str| {
            text.lines().find_map(|l| {
                let (k, v) = l.split_once('=')?;
                (k.trim() == key).then(|| v.trim())
            })
        };
        let base = Base::from_id(get("base")?)?;
        let saturation = get("saturation").and_then(|s| s.parse::<f32>().ok()).unwrap_or(1.0).clamp(0.0, 1.0);
        let number = |key: &str| get(key).and_then(|v| v.parse::<f32>().ok()).filter(|v| v.is_finite());
        let tint = get("stops").map(|stops| Tint {
            stops: stops
                .split(',')
                .filter_map(|s| {
                    let (color, at) = s.split_once('@')?;
                    Some((hex_color(color)?, at.trim().parse::<f32>().ok()?.clamp(0.0, 1.0)))
                })
                .collect(),
            angle: number("angle").unwrap_or(0.0),
            custom: get("custom") == Some("true"),
            mix: number("mix").unwrap_or(DEFAULT_MIX).clamp(0.0, 1.0),
        });
        Some(DiscordTheme { base, tint: tint.filter(|t| !t.stops.is_empty()), saturation })
    }
}

/// The last theme read from Discord, if any.
pub fn remembered() -> Option<DiscordTheme> {
    DiscordTheme::from_text(&std::fs::read_to_string(remembered_path()).ok()?)
}

pub fn remember(theme: &DiscordTheme) {
    let path = remembered_path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Err(e) = std::fs::write(&path, theme.to_text()) {
        crate::log!("couldn't save the Discord theme: {e}");
    }
}

// ---------------------------------------------------------------------------
// Watching for changes
// ---------------------------------------------------------------------------

/// Calls `changed` whenever Discord writes its settings or Local Storage,
/// on a thread of its own. Windows tells us about the writes, so nothing is
/// polled; if Discord isn't installed yet, it looks again every minute.
pub fn watch(changed: impl Fn() + Send + 'static) {
    let spawned = std::thread::Builder::new().name("discord-theme".into()).spawn(move || loop {
        match discord_dir() {
            Some(dir) => {
                wait_for_changes(&dir, &changed);
                std::thread::sleep(Duration::from_secs(5)); // the folders went away
            }
            None => std::thread::sleep(Duration::from_secs(60)),
        }
    });
    if let Err(e) = spawned {
        crate::log!("couldn't watch the Discord theme: {e}");
    }
}

/// Waits for writes in Discord's folder (settings.json) and its Local
/// Storage; returns if they can't be watched (any more).
fn wait_for_changes(dir: &Path, changed: &dyn Fn()) {
    use windows::core::HSTRING;
    use windows::Win32::Foundation::{HANDLE, WAIT_OBJECT_0};
    use windows::Win32::Storage::FileSystem::{
        FindCloseChangeNotification, FindFirstChangeNotificationW, FindNextChangeNotification,
        FILE_NOTIFY_CHANGE_FILE_NAME, FILE_NOTIFY_CHANGE_LAST_WRITE,
    };
    use windows::Win32::System::Threading::{WaitForMultipleObjects, INFINITE};

    let filter = FILE_NOTIFY_CHANGE_LAST_WRITE | FILE_NOTIFY_CHANGE_FILE_NAME;
    let open = |path: &Path| unsafe { FindFirstChangeNotificationW(&HSTRING::from(path.as_os_str()), false, filter).ok() };
    let handles: Vec<HANDLE> = [dir.to_path_buf(), leveldb_dir(dir)].iter().filter_map(|p| open(p)).collect();
    if handles.is_empty() {
        return;
    }
    loop {
        let r = unsafe { WaitForMultipleObjects(&handles, false, INFINITE) };
        let i = r.0.wrapping_sub(WAIT_OBJECT_0.0) as usize;
        let Some(&h) = handles.get(i) else { break };
        // Discord writes a few files in a row: let it finish first.
        std::thread::sleep(Duration::from_millis(250));
        if unsafe { FindNextChangeNotification(h) }.is_err() {
            break;
        }
        changed();
    }
    for h in handles {
        unsafe {
            let _ = FindCloseChangeNotification(h);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proto_field(n: u64, wire: u64, out: &mut Vec<u8>) {
        let mut tag = (n << 3) | wire;
        while tag >= 0x80 {
            out.push((tag as u8) | 0x80);
            tag >>= 7;
        }
        out.push(tag as u8);
    }

    fn proto_bytes(n: u64, bytes: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        proto_field(n, 2, &mut out);
        out.push(bytes.len() as u8);
        out.extend_from_slice(bytes);
        out
    }

    #[test]
    fn reads_custom_and_preset_themes_from_settings() {
        // Private fields around the appearance are ignored.
        let mut custom = proto_bytes(1, b"#331895");
        custom.extend(proto_bytes(1, b"#290A42"));
        proto_field(3, 0, &mut custom);
        custom.push(97);
        proto_field(4, 0, &mut custom);
        custom.push(53);
        let mut settings = proto_bytes(2, b"private stuff");
        settings.extend(proto_bytes(13, &proto_bytes(3, &proto_bytes(4, &custom))));
        let tint = client_theme(&settings).unwrap().unwrap();
        assert_eq!(tint.stops, vec![([0x33, 0x18, 0x95], 0.0), ([0x29, 0x0A, 0x42], 1.0)]);
        assert_eq!((tint.angle, tint.custom), (97.0, true));
        assert!((tint.mix - 0.53).abs() < 1e-6);

        let mut wrapped = Vec::new();
        proto_field(1, 0, &mut wrapped);
        wrapped.push(12);
        let settings = proto_bytes(13, &proto_bytes(3, &proto_bytes(2, &wrapped)));
        let preset = client_theme(&settings).unwrap().unwrap();
        assert_eq!(preset.stops[0], ([0x53, 0x48, 0xca], 0.1121));
        assert_eq!((preset.angle, preset.custom), (48.17, false));

        // Appearance without a client theme: no tint. No appearance: unknown.
        assert_eq!(client_theme(&proto_bytes(13, &[0x08, 0x03])), Some(None));
        assert_eq!(client_theme(&proto_bytes(2, b"x")), None);
    }

    #[test]
    fn remembered_theme_round_trips() {
        let theme = DiscordTheme {
            base: Base::Dark,
            tint: Some(Tint { stops: spread(&[[0x33, 0x18, 0x95], [0x29, 0x0A, 0x42]]), angle: 97.0, custom: true, mix: 0.53 }),
            saturation: 0.8,
        };
        assert_eq!(DiscordTheme::from_text(&theme.to_text()), Some(theme));
        assert_eq!(DiscordTheme::from_text("base = nonsense"), None);
    }

    #[test]
    fn decodes_storage_values() {
        assert_eq!(storage_text(b"\x01{\"a\":1}").as_deref(), Some("{\"a\":1}"));
        assert_eq!(storage_text(&[0, b'h', 0, b'i', 0]).as_deref(), Some("hi"));
        assert_eq!(base64("aGk=").as_deref(), Some(b"hi".as_slice()));
    }

    /// Reads the real Discord theme on this PC. Prints only theme fields.
    #[test]
    #[ignore]
    fn live_discord_theme() {
        let dir = discord_dir().expect("Discord isn't installed");
        let start = std::time::Instant::now();
        let theme = read_from(&dir);
        println!("read in {:?}: {theme:?}", start.elapsed());
        assert!(theme.is_some());
    }
}
