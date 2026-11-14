//! Persistent settings in `%APPDATA%\discord-clip\config.ini` (simple
//! `key = value` lines). Command-line flags override the file for one run.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

use crate::hotkey::Hotkey;

#[derive(Clone, Debug, PartialEq)]
pub struct Config {
    pub hotkey: Hotkey,
    pub seconds: u32,
    pub mic: bool,
    pub mic_gain: f32,
    /// Microphone endpoint id; empty = default communications device.
    pub mic_device: String,
    /// Format new clips are saved in (#8).
    pub format: crate::audio::Format,
    /// Save Discord and the mic as separate tracks (a 4-channel clip).
    pub separate_tracks: bool,
    pub out_dir: PathBuf,
    pub clipboard: bool,
    pub notifications: bool,
    pub check_updates: bool,
    /// Record who talks when in each clip (#66).
    pub speakers: bool,
    /// Window colours (#37).
    pub theme: ThemeMode,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            hotkey: Hotkey::default(),
            seconds: 30,
            mic: true,
            mic_gain: 1.0,
            mic_device: String::new(),
            format: crate::audio::Format::Opus,
            separate_tracks: true,
            out_dir: default_out_dir(),
            clipboard: true,
            notifications: true,
            check_updates: true,
            speakers: true,
            theme: ThemeMode::Discord,
        }
    }
}

/// Window colours (#37).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ThemeMode {
    /// The theme the user has in Discord, as far as it can be read.
    #[default]
    Discord,
    Dark,
    Light,
}

impl ThemeMode {
    pub const ALL: [ThemeMode; 3] = [ThemeMode::Discord, ThemeMode::Dark, ThemeMode::Light];

    pub fn key(self) -> &'static str {
        match self {
            ThemeMode::Discord => "discord",
            ThemeMode::Dark => "dark",
            ThemeMode::Light => "light",
        }
    }

    pub fn from_key(key: &str) -> Option<ThemeMode> {
        ThemeMode::ALL.into_iter().find(|t| t.key().eq_ignore_ascii_case(key))
    }

    pub fn label(self) -> &'static str {
        match self {
            ThemeMode::Discord => "Follow Discord",
            ThemeMode::Dark => "Dark",
            ThemeMode::Light => "Light",
        }
    }
}

/// `%APPDATA%\discord-clip`
pub fn app_dir() -> PathBuf {
    std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join("discord-clip")
}

pub fn config_path() -> PathBuf {
    app_dir().join("config.ini")
}

/// The user's Music folder (respects OneDrive/known-folder redirection),
/// plus `Discord Clips`.
fn default_out_dir() -> PathBuf {
    use windows::Win32::System::Com::CoTaskMemFree;
    use windows::Win32::UI::Shell::{FOLDERID_Music, SHGetKnownFolderPath, KF_FLAG_DEFAULT};
    let music = unsafe {
        SHGetKnownFolderPath(&FOLDERID_Music, KF_FLAG_DEFAULT, None)
            .ok()
            .map(|p| {
                let s = p.to_string().unwrap_or_default();
                CoTaskMemFree(Some(p.0 as *const _));
                PathBuf::from(s)
            })
    };
    music
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| PathBuf::from("."))
        .join("Discord Clips")
}

fn parse_bool(key: &str, v: &str) -> Result<bool> {
    match v.to_ascii_lowercase().as_str() {
        "true" | "yes" | "on" | "1" => Ok(true),
        "false" | "no" | "off" | "0" => Ok(false),
        _ => bail!("{key}: expected true/false, got '{v}'"),
    }
}

/// Clips always go to one fixed place, so the folder must be a full path -
/// a relative one would silently depend on where the app was started from.
/// `%VARIABLES%` like `%USERPROFILE%` are expanded.
pub fn resolve_out_dir(v: &str) -> Result<PathBuf> {
    let v = v.trim().trim_matches('"');
    if v.is_empty() {
        bail!("out_dir can't be empty");
    }
    let expanded = expand_env(v);
    let path = PathBuf::from(&expanded);
    if !path.is_absolute() {
        bail!("out_dir must be a full path like D:\\Clips or %USERPROFILE%\\Music\\Discord Clips (got '{v}')");
    }
    Ok(path)
}

fn expand_env(s: &str) -> String {
    use windows::core::HSTRING;
    use windows::Win32::System::Environment::ExpandEnvironmentStringsW;
    let src = HSTRING::from(s);
    let mut buf = vec![0u16; 1024];
    let n = unsafe { ExpandEnvironmentStringsW(&src, Some(&mut buf)) } as usize;
    if n == 0 || n > buf.len() {
        return s.to_string();
    }
    String::from_utf16_lossy(&buf[..n - 1])
}

pub fn validate_seconds(n: u32) -> Result<u32> {
    if (1..=300).contains(&n) {
        Ok(n)
    } else {
        bail!("seconds must be between 1 and 300")
    }
}

impl Config {
    /// Applies one `key = value` setting. Shared by the file parser and CLI.
    pub fn set(&mut self, key: &str, value: &str) -> Result<()> {
        let v = value.trim();
        match key {
            "hotkey" => {
                let hk: Hotkey = v.parse().map_err(anyhow::Error::msg)?;
                hk.check_sensible().map_err(anyhow::Error::msg)?;
                self.hotkey = hk;
            }
            "seconds" => {
                self.seconds = validate_seconds(v.parse().context("seconds must be a whole number")?)?
            }
            "mic" => self.mic = parse_bool(key, v)?,
            "mic_device" => self.mic_device = v.to_string(),
            "separate_tracks" => self.separate_tracks = parse_bool(key, v)?,
            "format" => {
                self.format = crate::audio::Format::from_key(v)
                    .with_context(|| format!("format must be one of: {}", crate::audio::Format::ALL.map(|f| f.key()).join(", ")))?
            }
            "mic_gain" => {
                let g: f32 = v.parse().context("mic_gain must be a number")?;
                if !(0.0..=10.0).contains(&g) {
                    bail!("mic_gain must be between 0 and 10");
                }
                self.mic_gain = g;
            }
            "out_dir" => self.out_dir = resolve_out_dir(v)?,
            "clipboard" => self.clipboard = parse_bool(key, v)?,
            "notifications" => self.notifications = parse_bool(key, v)?,
            "speakers" => self.speakers = parse_bool(key, v)?,
            "check_updates" => self.check_updates = parse_bool(key, v)?,
            "theme" => {
                self.theme = ThemeMode::from_key(v)
                    .with_context(|| format!("theme must be one of: {}", ThemeMode::ALL.map(|t| t.key()).join(", ")))?
            }
            _ => bail!("unknown setting '{key}'"),
        }
        Ok(())
    }

    /// Parses config text. Bad lines are skipped and reported, so one typo
    /// doesn't throw away the whole file.
    pub fn parse(text: &str) -> (Config, Vec<String>) {
        let mut cfg = Config::default();
        // Settings files from before #8 have no `format`: those clips were
        // WAV, so keep it that way until it's changed.
        cfg.format = crate::audio::Format::Wav;
        let mut problems = Vec::new();
        for (i, line) in text.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') || line.starts_with(';') || line.starts_with('[') {
                continue;
            }
            let Some((k, v)) = line.split_once('=') else {
                problems.push(format!("line {}: expected 'key = value'", i + 1));
                continue;
            };
            if let Err(e) = cfg.set(&k.trim().to_ascii_lowercase(), v) {
                problems.push(format!("line {}: {e:#}", i + 1));
            }
        }
        (cfg, problems)
    }

    pub fn to_text(&self) -> String {
        format!(
            "\
# discord-clip settings. Use \"Reload\" in the tray menu after editing.

# Key combination that saves a clip, e.g. Ctrl+Alt+C, Ctrl+Shift+F9, F9, Win+Alt+R
hotkey = {}

# How many seconds of audio each clip contains (1-300)
seconds = {}

# Record your own microphone too (default communications device)
mic = {}

# Microphone volume multiplier (0-10)
mic_gain = {}

# Microphone to record (pick it in the app). Empty = Windows' default
# communications microphone.
mic_device = {}

# Format new clips are saved in: opus (small, .ogg), mp3-128, mp3-192,
# flac (lossless) or wav (uncompressed)
format = {}

# Keep Discord audio and your mic as separate tracks in each clip (a
# 4-channel file, about twice the size). Copying a clip still gives Discord
# a normal stereo mix. Not possible with MP3.
separate_tracks = {}

# Folder clips are saved in
out_dir = {}

# Copy each saved clip to the clipboard so Ctrl+V in Discord attaches it
clipboard = {}

# Show a notification when a clip is saved
notifications = {}

# Check GitHub for new versions (nothing is installed without asking)
check_updates = {}

# Record who talks when in each clip, from Discord's voice status (reads
# only its voice connection log lines and speaking events, never your login)
speakers = {}

# Window colours: discord (follow the theme you use in Discord), dark or light
theme = {}
",
            self.hotkey,
            self.seconds,
            self.mic,
            self.mic_gain,
            self.mic_device,
            self.format.key(),
            self.separate_tracks,
            self.out_dir.display(),
            self.clipboard,
            self.notifications,
            self.check_updates,
            self.speakers,
            self.theme.key()
        )
    }

    /// Loads the config file, creating it with defaults if missing.
    /// Returns the config plus any problems found while parsing.
    pub fn load_or_create(path: &Path) -> (Config, Vec<String>) {
        match std::fs::read_to_string(path) {
            Ok(text) => Config::parse(&text),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let cfg = Config::default();
                let problems = match cfg.save(path) {
                    Ok(()) => vec![],
                    Err(e) => vec![format!("couldn't create {}: {e:#}", path.display())],
                };
                (cfg, problems)
            }
            Err(e) => (Config::default(), vec![format!("couldn't read {}: {e}", path.display())]),
        }
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(path, self.to_text()).with_context(|| format!("writing {}", path.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let mut cfg = Config::default();
        cfg.hotkey = "Ctrl+Shift+F9".parse().unwrap();
        cfg.seconds = 45;
        cfg.mic = false;
        cfg.mic_gain = 1.5;
        cfg.mic_device = "{0.0.1.00000000}.{8d2f5c8e-1b2a-4c3d-9e8f-123456789abc}".into();
        cfg.out_dir = PathBuf::from(r"D:\Clips Here");
        cfg.clipboard = false;
        cfg.notifications = false;
        cfg.check_updates = false;
        cfg.theme = ThemeMode::Light;
        cfg.separate_tracks = false;
        cfg.format = crate::audio::Format::Mp3High;
        let (back, problems) = Config::parse(&cfg.to_text());
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(back, cfg);
        // The default (a new settings file) round-trips too.
        assert_eq!(Config::parse(&Config::default().to_text()).0.format, crate::audio::Format::Opus);
    }

    #[test]
    fn old_settings_files_keep_wav() {
        let (cfg, problems) = Config::parse("seconds = 30\n");
        assert!(problems.is_empty());
        assert_eq!(cfg.format, crate::audio::Format::Wav);
        let (cfg, problems) = Config::parse("format = aac\n");
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert_eq!(cfg.format, crate::audio::Format::Wav);
    }

    #[test]
    fn bad_lines_are_skipped() {
        let (cfg, problems) = Config::parse("seconds = 20\nhotkey = Ctrl+Nope\nwhat\nmic = maybe\nfoo = 1\n");
        assert_eq!(cfg.seconds, 20);
        assert_eq!(cfg.hotkey, Hotkey::default());
        assert!(cfg.mic);
        assert_eq!(problems.len(), 4, "{problems:?}");
    }

    #[test]
    fn out_dir_must_be_absolute() {
        assert!(resolve_out_dir("clips").is_err());
        assert!(resolve_out_dir("").is_err());
        assert_eq!(resolve_out_dir(r#""D:\My Clips""#).unwrap(), PathBuf::from(r"D:\My Clips"));
        let home = std::env::var("USERPROFILE").unwrap();
        assert_eq!(
            resolve_out_dir(r"%USERPROFILE%\Clips").unwrap(),
            PathBuf::from(home).join("Clips")
        );
    }

    #[test]
    fn rejects_typing_hotkey() {
        let (cfg, problems) = Config::parse("hotkey = K");
        assert_eq!(cfg.hotkey, Hotkey::default());
        assert_eq!(problems.len(), 1);
    }
}
