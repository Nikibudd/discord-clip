//! The clips folder as a library: listing clips, reading their length and a
//! waveform for display, and moving them to the Recycle Bin. (Playback is
//! in `player.rs`.)

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::{bail, Context, Result};
use windows::core::PCWSTR;

use crate::volumes::Volumes;

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
    /// 4 = Discord and mic as separate tracks (see `clip::TRACK_CHANNELS`).
    pub channels: u16,
}

/// Lists the `.wav` files in `dir`, newest first.
pub fn scan(dir: &Path) -> Vec<ClipFile> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut clips: Vec<ClipFile> = entries
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|x| x.eq_ignore_ascii_case("wav")))
        // Skip our own temporary files (e.g. while replacing a clip with a trim).
        .filter(|e| !e.file_name().to_string_lossy().starts_with('.'))
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

/// Reads a clip's length and its waveform for a card. Takes ~10 ms for a 30 s clip.
pub fn analyze(path: &Path) -> Result<Analysis> {
    analyze_bars(path, WAVEFORM_BARS)
}

/// Like `analyze`, with `bars` waveform bars (e.g. more for the trim view).
pub fn analyze_bars(path: &Path, bars: usize) -> Result<Analysis> {
    let bars = bars.max(1);
    let mut reader = hound::WavReader::open(path).with_context(|| format!("opening {}", path.display()))?;
    let spec = reader.spec();
    let frames = reader.duration() as usize;
    let seconds = frames as f32 / spec.sample_rate.max(1) as f32;
    let channels = spec.channels.max(1) as usize;
    let mut peaks = vec![0f32; bars];
    if frames > 0 {
        let scale = match spec.sample_format {
            hound::SampleFormat::Float => 1.0,
            hound::SampleFormat::Int => 1.0 / (1u64 << (spec.bits_per_sample.clamp(1, 32) - 1)) as f32,
        };
        let mut push = |i: usize, v: f32| {
            let bar = (i / channels) * bars / frames;
            let p = &mut peaks[bar.min(bars - 1)];
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
    Ok(Analysis { seconds, peaks: normalize(peaks), channels: spec.channels })
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

/// Fade at each cut of a trimmed clip, so it doesn't start or end with a click.
const TRIM_FADE_SECS: f32 = 0.01;

/// Writes the part of `src` between `start` and `end` (0..=1 of its length)
/// to `dest`, in the same WAV format (sample rate, channels, bit depth), with
/// a short fade at both cuts.
pub fn write_trimmed(src: &Path, start: f32, end: f32, dest: &Path) -> Result<()> {
    let mut reader = hound::WavReader::open(src).with_context(|| format!("opening {}", src.display()))?;
    let spec = reader.spec();
    let channels = spec.channels.max(1) as usize;
    let frames = reader.duration() as usize;
    let (start, end) = (start.clamp(0.0, 1.0), end.clamp(0.0, 1.0));
    let first = (start * frames as f32) as usize;
    let last = ((end * frames as f32) as usize).min(frames);
    if last <= first {
        bail!("nothing selected");
    }
    let fade = ((TRIM_FADE_SECS * spec.sample_rate as f32) as usize).min((last - first) / 2).max(1);
    let gain = |frame: usize| -> f32 {
        let from_start = frame - first;
        let to_end = last - 1 - frame;
        (from_start.min(to_end) as f32 / fade as f32).min(1.0)
    };
    let mut writer = hound::WavWriter::create(dest, spec).with_context(|| format!("writing {}", dest.display()))?;
    // Seek to the first frame instead of reading everything before it.
    reader.seek(first as u32)?;
    let count = (last - first) * channels;
    match spec.sample_format {
        hound::SampleFormat::Float => {
            for (i, s) in reader.samples::<f32>().take(count).enumerate() {
                writer.write_sample(s? * gain(first + i / channels))?;
            }
        }
        hound::SampleFormat::Int => {
            for (i, s) in reader.samples::<i32>().take(count).enumerate() {
                writer.write_sample((s? as f32 * gain(first + i / channels)).round() as i32)?;
            }
        }
    }
    writer.finalize()?;
    if spec.channels == crate::clip::TRACK_CHANNELS {
        crate::clip::fix_channel_mask(dest)?;
    }
    Ok(())
}

/// Saves the trimmed part as a new clip next to the original, keeping its
/// timestamp so it sorts next to it: `discord_clip_<stamp> Bob (trimmed).wav`.
pub fn save_trimmed_copy(src: &Path, start: f32, end: f32) -> Result<PathBuf> {
    let dir = src.parent().context("clip has no folder")?;
    let stem = src.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let ext = src.extension().map(|e| e.to_string_lossy().into_owned()).unwrap_or_else(|| "wav".into());
    let mut dest = dir.join(format!("{stem} (trimmed).{ext}"));
    let mut n = 2;
    while dest.exists() {
        dest = dir.join(format!("{stem} (trimmed {n}).{ext}"));
        n += 1;
    }
    write_trimmed(src, start, end, &dest)?;
    Ok(dest)
}

/// Replaces the clip with its trimmed part. The original goes to the
/// Recycle Bin, so this can be undone.
pub fn replace_with_trimmed(src: &Path, start: f32, end: f32) -> Result<()> {
    let dir = src.parent().context("clip has no folder")?;
    let tmp = dir.join(format!(".trimming-{}.wav", std::process::id()));
    write_trimmed(src, start, end, &tmp)?;
    if let Err(e) = recycle(src) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    std::fs::rename(&tmp, src).with_context(|| format!("saving {}", src.display()))
}

/// Writes the trimmed part to a temporary file with the clip's file name (so
/// that's the name Discord shows), for copying to the clipboard. A clip with
/// separate tracks is mixed at its `volumes`.
pub fn trimmed_temp_copy(src: &Path, start: f32, end: f32, volumes: Volumes) -> Result<PathBuf> {
    let dir = std::env::temp_dir().join("discord-clip-trimmed");
    std::fs::create_dir_all(&dir)?;
    let trimmed = dir.join(format!(".trimmed-{}.wav", std::process::id()));
    write_trimmed(src, start, end, &trimmed)?;
    // Discord gets a normal stereo mix, named like the clip.
    let dest = dir.join(src.file_name().context("clip has no file name")?);
    let (samples, rate) = read_stereo(&trimmed, Tracks::Both, volumes)?;
    let _ = std::fs::remove_file(&trimmed);
    write_stereo(&dest, &samples, rate)?;
    Ok(dest)
}

/// Which part of a clip with separate tracks to use.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tracks {
    /// Discord and mic mixed, like a normal clip.
    Both,
    /// Only other people (Discord audio).
    Discord,
    /// Only your microphone.
    Mic,
}

impl Tracks {
    /// Gains of the Discord and mic tracks for this choice.
    pub fn gains(self, v: Volumes) -> (f32, f32) {
        match self {
            Tracks::Both => (v.discord, v.mic),
            Tracks::Discord => (v.discord, 0.0),
            Tracks::Mic => (0.0, v.mic),
        }
    }
}

/// Reads a clip as interleaved stereo (-1..=1) and its sample rate. Clips
/// with separate tracks (4 channels: Discord L/R + mic L/R) are mixed at
/// `volumes`, or reduced to one track. Mono is doubled; other layouts keep
/// the first two channels. `tracks` and `volumes` only matter for 4-channel
/// clips.
pub fn read_stereo(path: &Path, tracks: Tracks, volumes: Volumes) -> Result<(Vec<f32>, u32)> {
    let mut reader = hound::WavReader::open(path).with_context(|| format!("opening {}", path.display()))?;
    let spec = reader.spec();
    let samples: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => reader.samples::<f32>().collect::<Result<_, _>>()?,
        hound::SampleFormat::Int => {
            let scale = 1.0 / (1u64 << (spec.bits_per_sample.clamp(1, 32) - 1)) as f32;
            reader.samples::<i32>().map(|s| s.map(|v| v as f32 * scale)).collect::<Result<_, _>>()?
        }
    };
    let channels = spec.channels.max(1) as usize;
    let (d, m) = tracks.gains(volumes);
    let out = match channels {
        1 => samples.iter().flat_map(|&s| [s, s]).collect(),
        2 => samples,
        4 => samples
            .chunks_exact(4)
            .flat_map(|f| [(f[0] * d + f[2] * m).clamp(-1.0, 1.0), (f[1] * d + f[3] * m).clamp(-1.0, 1.0)])
            .collect(),
        n => samples.chunks_exact(n).flat_map(|f| [f[0], f[1]]).collect(),
    };
    Ok((out, spec.sample_rate))
}

fn write_stereo(path: &Path, samples: &[f32], rate: u32) -> Result<()> {
    let spec = hound::WavSpec { channels: 2, sample_rate: rate, bits_per_sample: 16, sample_format: hound::SampleFormat::Int };
    let mut w = hound::WavWriter::create(path, spec).with_context(|| format!("writing {}", path.display()))?;
    for &s in samples {
        w.write_sample((s * 32767.0).round() as i16)?;
    }
    w.finalize()?;
    Ok(())
}

/// Number of channels in a WAV file (4 = separate tracks).
pub fn channels(path: &Path) -> Result<u16> {
    Ok(hound::WavReader::open(path).with_context(|| format!("opening {}", path.display()))?.spec().channels)
}

/// The file to hand to Discord (clipboard): the clip itself if it's a normal
/// stereo clip, otherwise a stereo mix (or one track) at the clip's
/// `volumes`, written to a temporary file named like the clip.
pub fn shareable_copy(src: &Path, tracks: Tracks, volumes: Volumes) -> Result<PathBuf> {
    if channels(src)? <= 2 {
        return Ok(src.to_path_buf());
    }
    let dir = std::env::temp_dir().join("discord-clip-share");
    std::fs::create_dir_all(&dir)?;
    let stem = src.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let name = match tracks {
        Tracks::Both => format!("{stem}.wav"),
        Tracks::Discord => format!("{stem} (Discord only).wav"),
        Tracks::Mic => format!("{stem} (mic only).wav"),
    };
    let dest = dir.join(name);
    let (samples, rate) = read_stereo(src, tracks, volumes)?;
    write_stereo(&dest, &samples, rate)?;
    Ok(dest)
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

    #[test]
    fn trims_clips() {
        let dir = std::env::temp_dir().join(format!("discord-clip-trim-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("discord_clip_20261109_213151 Bob.wav");
        // 2 s of a constant level, stereo.
        crate::clip::write_wav(&src, &vec![0.5f32; 48_000 * 2 * 2]).unwrap();

        let copy = save_trimmed_copy(&src, 0.25, 0.75).unwrap();
        assert_eq!(copy.file_name().unwrap(), "discord_clip_20261109_213151 Bob (trimmed).wav");
        let r = hound::WavReader::open(&copy).unwrap();
        assert_eq!(r.spec(), hound::WavReader::open(&src).unwrap().spec());
        assert_eq!(r.duration(), 48_000); // 1 s
        let samples: Vec<i16> = hound::WavReader::open(&copy).unwrap().samples::<i16>().map(|s| s.unwrap()).collect();
        assert_eq!(samples[0], 0, "fades in from silence");
        assert!(samples[samples.len() / 2] > 16_000, "full level in the middle");
        assert!(samples[samples.len() - 1].abs() < 100, "fades out");
        // The trimmed copy still has the date and a name.
        let (saved, name) = split_clip_name(&copy.file_stem().unwrap().to_string_lossy());
        assert!(saved.is_some());
        assert_eq!(name.as_deref(), Some("Bob (trimmed)"));
        // Saving again doesn't overwrite.
        let second = save_trimmed_copy(&src, 0.0, 0.5).unwrap();
        assert_eq!(second.file_name().unwrap(), "discord_clip_20261109_213151 Bob (trimmed 2).wav");

        let tmp = trimmed_temp_copy(&src, 0.0, 0.1, Volumes::default()).unwrap();
        assert_eq!(tmp.file_name(), src.file_name());
        assert_eq!(hound::WavReader::open(&tmp).unwrap().duration(), 9_600);
        assert!(write_trimmed(&src, 0.6, 0.6, &dir.join("empty.wav")).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn separate_tracks_mix_and_split() {
        let dir = std::env::temp_dir().join(format!("discord-clip-tracks-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // 1 frame: Discord L/R = 0.25/0.5, mic L/R = 0.125/-0.25.
        let src = dir.join("discord_clip_20261109_213151.wav");
        let frame = [0.25f32, 0.5, 0.125, -0.25];
        let samples: Vec<f32> = frame.iter().cycle().take(4 * 480).copied().collect();
        crate::clip::write_wav_channels(&src, &samples, crate::clip::TRACK_CHANNELS).unwrap();
        assert_eq!(channels(&src).unwrap(), 4);
        assert_eq!(analyze(&src).unwrap().channels, 4);

        let near = |a: f32, b: f32| (a - b).abs() < 0.001;
        let (both, rate) = read_stereo(&src, Tracks::Both, Volumes::default()).unwrap();
        assert_eq!(rate, 48_000);
        assert_eq!(both.len(), 2 * 480);
        assert!(near(both[0], 0.375) && near(both[1], 0.25), "{:?}", &both[..2]);
        let (discord, _) = read_stereo(&src, Tracks::Discord, Volumes::default()).unwrap();
        assert!(near(discord[0], 0.25) && near(discord[1], 0.5));
        let (mic, _) = read_stereo(&src, Tracks::Mic, Volumes::default()).unwrap();
        assert!(near(mic[0], 0.125) && near(mic[1], -0.25));

        // Per-clip volumes (#7): Discord at 50%, mic at 200%.
        let v = Volumes { discord: 0.5, mic: 2.0 };
        let (mixed, _) = read_stereo(&src, Tracks::Both, v).unwrap();
        assert!(near(mixed[0], 0.375) && near(mixed[1], -0.25), "{:?}", &mixed[..2]);
        let (mic, _) = read_stereo(&src, Tracks::Mic, v).unwrap();
        assert!(near(mic[0], 0.25) && near(mic[1], -0.5));
        let (muted, _) = read_stereo(&src, Tracks::Both, Volumes { discord: 0.0, mic: 1.0 }).unwrap();
        assert!(near(muted[0], 0.125) && near(muted[1], -0.25));
        let adjusted = shareable_copy(&src, Tracks::Both, v).unwrap();
        let (adjusted, _) = read_stereo(&adjusted, Tracks::Both, Volumes::default()).unwrap();
        assert!(near(adjusted[0], 0.375) && near(adjusted[1], -0.25));

        // Shared copies are stereo files named after the clip.
        let shared = shareable_copy(&src, Tracks::Both, Volumes::default()).unwrap();
        assert_eq!(shared.file_name(), src.file_name());
        assert_eq!(channels(&shared).unwrap(), 2);
        let mic_only = shareable_copy(&src, Tracks::Mic, Volumes::default()).unwrap();
        assert_eq!(mic_only.file_name().unwrap(), "discord_clip_20261109_213151 (mic only).wav");
        // A normal stereo clip is shared as itself.
        let stereo = dir.join("stereo.wav");
        crate::clip::write_wav(&stereo, &[0.1, 0.1]).unwrap();
        assert_eq!(shareable_copy(&stereo, Tracks::Both, Volumes::default()).unwrap(), stereo);

        // Trimming keeps both tracks; the trimmed copy for Discord is a stereo mix.
        let trimmed = save_trimmed_copy(&src, 0.0, 0.5).unwrap();
        assert_eq!(channels(&trimmed).unwrap(), 4);
        assert_eq!(channels(&trimmed_temp_copy(&src, 0.0, 0.5, Volumes::default()).unwrap()).unwrap(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
