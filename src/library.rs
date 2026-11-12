//! The clips folder as a library: listing clips, reading their length and a
//! waveform for display, and moving them to the Recycle Bin. (Playback is
//! in `player.rs`.)

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::{bail, Context, Result};
use windows::core::PCWSTR;

/// Number of bars in a clip's waveform.
pub const WAVEFORM_BARS: usize = 48;

/// A WAV file in the clips folder (cheap to get: no file contents read).
#[derive(Clone, Debug, PartialEq)]
pub struct ClipFile {
    pub path: PathBuf,
    pub size: u64,
    pub modified: SystemTime,
    /// When it was saved, from the file name (local time).
    pub saved: Option<SavedAt>,
    /// The name the user gave it (or the whole name of a file that doesn't
    /// follow our naming).
    pub name: Option<String>,
}

/// Local date and time parsed from `discord_clip_YYYYMMDD_HHMMSS[ name].wav`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct SavedAt {
    pub year: u16,
    pub month: u8,
    pub day: u8,
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
}

/// What we read from inside a clip.
#[derive(Clone, Debug, PartialEq)]
pub struct Analysis {
    pub seconds: f32,
    /// Peak level per bar, 0..=1, `WAVEFORM_BARS` long.
    pub peaks: Vec<f32>,
}

/// Lists the `.wav` files in `dir`, newest first.
pub fn scan(dir: &Path) -> Vec<ClipFile> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut clips: Vec<ClipFile> = entries
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|x| x.eq_ignore_ascii_case("wav")))
        .filter_map(|e| {
            let meta = e.metadata().ok()?;
            if !meta.is_file() {
                return None;
            }
            let path = e.path();
            let (saved, name) = split_clip_name(&path.file_stem()?.to_string_lossy());
            Some(ClipFile { path, size: meta.len(), modified: meta.modified().unwrap_or(SystemTime::UNIX_EPOCH), saved, name })
        })
        .collect();
    clips.sort_by(|a, b| b.modified.cmp(&a.modified).then_with(|| b.path.cmp(&a.path)));
    clips
}

/// File names start with this, then `YYYYMMDD_HHMMSS`, then optionally a
/// custom name: `discord_clip_20261109_213151 Bob laughs.wav`.
const PREFIX: &str = "discord_clip_";

/// Splits a clip's file stem into when it was saved and its custom name.
/// Files that don't follow our naming are all name, no date.
pub fn split_clip_name(stem: &str) -> (Option<SavedAt>, Option<String>) {
    if let Some(rest) = stem.strip_prefix(PREFIX) {
        if let Some(at) = rest.get(..15).and_then(parse_stamp) {
            let name = rest[15..].trim_start_matches([' ', '_', '-']).trim();
            return (Some(at), (!name.is_empty()).then(|| name.to_string()));
        }
    }
    (None, Some(stem.to_string()))
}

/// `YYYYMMDD_HHMMSS`
fn parse_stamp(stamp: &str) -> Option<SavedAt> {
    let (date, time) = stamp.split_once('_')?;
    if date.len() != 8 || time.len() != 6 || !date.bytes().chain(time.bytes()).all(|b| b.is_ascii_digit()) {
        return None;
    }
    let n = |s: &str| s.parse::<u16>().ok();
    let at = SavedAt {
        year: n(&date[0..4])?,
        month: n(&date[4..6])? as u8,
        day: n(&date[6..8])? as u8,
        hour: n(&time[0..2])? as u8,
        minute: n(&time[2..4])? as u8,
        second: n(&time[4..6])? as u8,
    };
    ((1..=12).contains(&at.month) && (1..=31).contains(&at.day) && at.hour < 24 && at.minute < 60).then_some(at)
}

/// Makes a user-typed name safe for a Windows file name.
fn sanitize_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| if c.is_control() || r#"\/:*?"<>|"#.contains(c) { ' ' } else { c })
        .collect();
    let collapsed = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    // Windows doesn't allow names ending in a dot or space.
    collapsed.chars().take(100).collect::<String>().trim_end_matches(['.', ' ']).to_string()
}

/// Renames a clip to `name`, keeping its timestamp so it still sorts and
/// shows its date: `discord_clip_<stamp> <name>.wav`. An empty name goes
/// back to just the timestamp. Returns the new path.
pub fn rename(path: &Path, name: &str) -> Result<PathBuf> {
    let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let ext = path.extension().map(|e| e.to_string_lossy().into_owned()).unwrap_or_else(|| "wav".into());
    let dir = path.parent().context("clip has no folder")?;
    let name = sanitize_name(name);
    let stamp = stem.strip_prefix(PREFIX).and_then(|r| r.get(..15)).filter(|s| parse_stamp(s).is_some());
    let new_stem = match (stamp, name.is_empty()) {
        (Some(s), true) => format!("{PREFIX}{s}"),
        (Some(s), false) => format!("{PREFIX}{s} {name}"),
        (None, true) => bail!("the name can't be empty"),
        (None, false) => name,
    };
    let same = |p: &Path| p.as_os_str().to_string_lossy().eq_ignore_ascii_case(&path.as_os_str().to_string_lossy());
    let mut target = dir.join(format!("{new_stem}.{ext}"));
    if target == path {
        return Ok(target);
    }
    // Don't overwrite another clip (but a change of case is the same file).
    let mut n = 2;
    while target.exists() && !same(&target) {
        target = dir.join(format!("{new_stem} ({n}).{ext}"));
        n += 1;
    }
    std::fs::rename(path, &target).with_context(|| format!("renaming {}", path.display()))?;
    Ok(target)
}

/// Reads a clip's length and waveform. Takes ~10 ms for a 30 s clip.
pub fn analyze(path: &Path) -> Result<Analysis> {
    let mut reader = hound::WavReader::open(path).with_context(|| format!("opening {}", path.display()))?;
    let spec = reader.spec();
    let frames = reader.duration() as usize;
    let seconds = frames as f32 / spec.sample_rate.max(1) as f32;
    let channels = spec.channels.max(1) as usize;
    let mut peaks = vec![0f32; WAVEFORM_BARS];
    if frames > 0 {
        let scale = match spec.sample_format {
            hound::SampleFormat::Float => 1.0,
            hound::SampleFormat::Int => 1.0 / (1u64 << (spec.bits_per_sample.clamp(1, 32) - 1)) as f32,
        };
        let mut push = |i: usize, v: f32| {
            let bar = (i / channels) * WAVEFORM_BARS / frames;
            let p = &mut peaks[bar.min(WAVEFORM_BARS - 1)];
            *p = p.max(v.abs());
        };
        match spec.sample_format {
            hound::SampleFormat::Float => {
                for (i, s) in reader.samples::<f32>().enumerate() {
                    push(i, s?);
                }
            }
            hound::SampleFormat::Int => {
                for (i, s) in reader.samples::<i32>().enumerate() {
                    push(i, s? as f32 * scale);
                }
            }
        }
    }
    Ok(Analysis { seconds, peaks: normalize(peaks) })
}

/// Scales the loudest bar to 1 (quiet clips still show their shape) and
/// keeps every bar at least a sliver tall.
fn normalize(mut peaks: Vec<f32>) -> Vec<f32> {
    let max = peaks.iter().cloned().fold(0.0f32, f32::max);
    for p in &mut peaks {
        *p = if max > 1e-4 { (*p / max).clamp(0.0, 1.0) } else { 0.0 };
        *p = p.max(0.06);
    }
    peaks
}

/// Moves a file to the Recycle Bin, so deleting a clip can be undone.
pub fn recycle(path: &Path) -> Result<()> {
    use windows::Win32::UI::Shell::{
        SHFileOperationW, FOF_ALLOWUNDO, FOF_NOCONFIRMATION, FOF_NOERRORUI, FOF_SILENT, FO_DELETE, SHFILEOPSTRUCTW,
    };
    // pFrom is a list of paths, ended by an extra NUL.
    let mut from: Vec<u16> = path.as_os_str().to_string_lossy().encode_utf16().collect();
    from.extend([0, 0]);
    let mut op = SHFILEOPSTRUCTW {
        wFunc: FO_DELETE,
        pFrom: PCWSTR(from.as_ptr()),
        fFlags: (FOF_ALLOWUNDO | FOF_NOCONFIRMATION | FOF_NOERRORUI | FOF_SILENT).0 as u16,
        ..Default::default()
    };
    let code = unsafe { SHFileOperationW(&mut op) };
    if code != 0 || op.fAnyOperationsAborted.as_bool() {
        anyhow::bail!("couldn't move {} to the Recycle Bin (error {code})", path.display());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_clip_names() {
        let at = SavedAt { year: 2026, month: 11, day: 9, hour: 21, minute: 31, second: 51 };
        assert_eq!(split_clip_name("discord_clip_20261109_213151"), (Some(at), None));
        assert_eq!(split_clip_name("discord_clip_20261109_213151 Bob laughs"), (Some(at), Some("Bob laughs".into())));
        // Older hand-made suffixes still get their date.
        assert_eq!(split_clip_name("discord_clip_20261109_213151_backup"), (Some(at), Some("backup".into())));
        assert_eq!(split_clip_name("discord_clip_20261309_213151"), (None, Some("discord_clip_20261309_213151".into())));
        assert_eq!(split_clip_name("my recording"), (None, Some("my recording".into())));
        assert_eq!(split_clip_name("discord_clip_2026_1"), (None, Some("discord_clip_2026_1".into())));
    }

    #[test]
    fn sanitizes_names() {
        assert_eq!(sanitize_name("  Bob: \"lol\"?  "), "Bob lol");
        assert_eq!(sanitize_name("a/b\\c"), "a b c");
        assert_eq!(sanitize_name("trailing dots..."), "trailing dots");
        assert_eq!(sanitize_name("   "), "");
    }

    #[test]
    fn renames_clips() {
        let dir = std::env::temp_dir().join(format!("discord-clip-rename-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let original = dir.join("discord_clip_20261109_213151.wav");
        std::fs::write(&original, b"x").unwrap();

        let named = rename(&original, "Bob: laughs?").unwrap();
        assert_eq!(named.file_name().unwrap(), "discord_clip_20261109_213151 Bob laughs.wav");
        assert!(!original.exists() && named.exists());

        // Same name for another clip from the same second: no overwrite.
        let twin = dir.join("discord_clip_20261109_213151 x.wav");
        std::fs::write(&twin, b"y").unwrap();
        let twin_named = rename(&twin, "Bob laughs").unwrap();
        assert_eq!(twin_named.file_name().unwrap(), "discord_clip_20261109_213151 Bob laughs (2).wav");

        // Only the case changes: same file.
        let recased = rename(&named, "bob laughs").unwrap();
        assert_eq!(recased.file_name().unwrap(), "discord_clip_20261109_213151 bob laughs.wav");

        // Empty name: back to the plain timestamp.
        let plain = rename(&recased, "  ").unwrap();
        assert_eq!(plain.file_name().unwrap(), "discord_clip_20261109_213151.wav");

        // A file without our timestamp needs a name.
        let other = dir.join("other.wav");
        std::fs::write(&other, b"z").unwrap();
        assert!(rename(&other, "").is_err());
        assert_eq!(rename(&other, "Renamed").unwrap().file_name().unwrap(), "Renamed.wav");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn analyzes_a_wav() {
        let dir = std::env::temp_dir().join(format!("discord-clip-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("discord_clip_20260101_120000.wav");
        // 1 s stereo: silent first half, loud second half.
        let rate = 48_000usize;
        let samples: Vec<f32> = (0..rate * 2).map(|i| if i < rate { 0.0 } else { 0.5 }).collect();
        crate::clip::write_wav(&path, &samples).unwrap();

        let a = analyze(&path).unwrap();
        assert!((a.seconds - 1.0).abs() < 0.01, "{}", a.seconds);
        assert_eq!(a.peaks.len(), WAVEFORM_BARS);
        assert!(a.peaks[0] < 0.1 && a.peaks[WAVEFORM_BARS - 1] > 0.99, "{:?}", a.peaks);

        let listed = scan(&dir);
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].saved.unwrap().hour, 12);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
