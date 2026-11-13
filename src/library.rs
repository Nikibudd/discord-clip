//! The clips folder as a library: listing clips, reading their length and a
//! waveform for display, and moving them to the Recycle Bin. (Playback is
//! in `player.rs`.)

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::{bail, Context, Result};
use windows::core::PCWSTR;

use crate::audio::{Audio, Format};
use crate::volumes::Volumes;

/// Number of bars in a clip's waveform.
pub const WAVEFORM_BARS: usize = 48;

/// A clip file in the clips folder (cheap to get: no file contents read).
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

/// Lists the clips (`.wav`, `.ogg`, `.flac`, `.mp3`) in `dir`, newest first.
pub fn scan(dir: &Path) -> Vec<ClipFile> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut clips: Vec<ClipFile> = entries
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.path()
                .extension()
                .is_some_and(|x| crate::audio::EXTENSIONS.iter().any(|ext| x.eq_ignore_ascii_case(ext)))
        })
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
    if Format::of_path(path) != Some(Format::Wav) {
        // Compressed clips are small: decode them whole.
        let audio = crate::audio::read(path)?;
        let channels = audio.channels.max(1) as usize;
        let frames = audio.frames();
        let mut peaks = vec![0f32; bars];
        for (i, s) in audio.samples.iter().enumerate() {
            let bar = (i / channels) * bars / frames.max(1);
            let p = &mut peaks[bar.min(bars - 1)];
            *p = p.max(s.abs());
        }
        return Ok(Analysis { seconds: audio.seconds(), peaks: normalize(peaks), channels: audio.channels });
    }
    // WAV: stream it, without holding a long clip in memory.
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

/// The frames `first..last` that `start..end` (0..=1) selects, and a gain per
/// frame that fades in and out at the cuts. The clip's own start and end
/// aren't cuts, so they don't fade (e.g. Save as new with only new volumes).
fn trim_range(frames: usize, rate: u32, start: f32, end: f32) -> Result<(usize, usize, impl Fn(usize) -> f32)> {
    let (start, end) = (start.clamp(0.0, 1.0), end.clamp(0.0, 1.0));
    let first = (start * frames as f32) as usize;
    let last = ((end * frames as f32) as usize).min(frames);
    if last <= first {
        bail!("nothing selected");
    }
    let fade = ((TRIM_FADE_SECS * rate as f32) as usize).min((last - first) / 2).max(1);
    let (cut_start, cut_end) = (first > 0, last < frames);
    let gain = move |frame: usize| -> f32 {
        let from_start = if cut_start { frame - first } else { usize::MAX };
        let to_end = if cut_end { last - 1 - frame } else { usize::MAX };
        (from_start.min(to_end) as f32 / fade as f32).min(1.0)
    };
    Ok((first, last, gain))
}

/// The part of `audio` between `start` and `end` (0..=1), faded at the cuts.
fn trimmed(audio: &Audio, start: f32, end: f32) -> Result<Audio> {
    let ch = audio.channels.max(1) as usize;
    let (first, last, gain) = trim_range(audio.frames(), audio.rate, start, end)?;
    let samples = audio.samples[first * ch..last * ch]
        .iter()
        .enumerate()
        .map(|(i, &s)| s * gain(first + i / ch))
        .collect();
    Ok(Audio { samples, channels: audio.channels, rate: audio.rate })
}

/// Writes the part of `src` between `start` and `end` (0..=1 of its length)
/// to `dest` in the same format (for WAV: sample rate, channels and bit
/// depth too), with a short fade at both cuts.
pub fn write_trimmed(src: &Path, start: f32, end: f32, dest: &Path) -> Result<()> {
    if Format::of_path(src) != Some(Format::Wav) {
        let audio = crate::audio::read(src)?;
        let format = Format::for_copy_of(src, &audio)?;
        return crate::audio::write(dest, &trimmed(&audio, start, end)?, format);
    }
    let mut reader = hound::WavReader::open(src).with_context(|| format!("opening {}", src.display()))?;
    let spec = reader.spec();
    let channels = spec.channels.max(1) as usize;
    let (first, last, gain) = trim_range(reader.duration() as usize, spec.sample_rate, start, end)?;
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
/// timestamp so it sorts next to it: `discord_clip_<stamp> Bob (edited).wav`.
pub fn save_edited_copy(src: &Path, start: f32, end: f32) -> Result<PathBuf> {
    let dir = src.parent().context("clip has no folder")?;
    let stem = src.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let ext = src.extension().map(|e| e.to_string_lossy().into_owned()).unwrap_or_else(|| "wav".into());
    let mut dest = dir.join(format!("{stem} (edited).{ext}"));
    let mut n = 2;
    while dest.exists() {
        dest = dir.join(format!("{stem} (edited {n}).{ext}"));
        n += 1;
    }
    write_trimmed(src, start, end, &dest)?;
    Ok(dest)
}

/// Replaces the clip with its trimmed part. The original goes to the
/// Recycle Bin, so this can be undone.
pub fn replace_with_trimmed(src: &Path, start: f32, end: f32) -> Result<()> {
    let dir = src.parent().context("clip has no folder")?;
    let ext = src.extension().map(|e| e.to_string_lossy().into_owned()).unwrap_or_else(|| "wav".into());
    let tmp = dir.join(format!(".trimming-{}.{ext}", std::process::id()));
    write_trimmed(src, start, end, &tmp)?;
    if let Err(e) = recycle(src) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    std::fs::rename(&tmp, src).with_context(|| format!("saving {}", src.display()))
}

/// A clip's waveform per track, for the Edit dialog (#31).
#[derive(Clone, Debug, PartialEq)]
pub struct TrackPeaks {
    pub seconds: f32,
    /// Peak level per bar, 0..=1: one set for a normal clip, two (Discord,
    /// mic) for a clip with separate tracks. All sets share one scale, so a
    /// quiet track looks quieter than a loud one.
    pub tracks: Vec<Vec<f32>>,
}

/// Reads a clip's waveform with `bars` bars per track.
pub fn analyze_tracks(path: &Path, bars: usize) -> Result<TrackPeaks> {
    let bars = bars.max(1);
    let audio = crate::audio::read(path)?;
    let ch = audio.channels.max(1) as usize;
    // Which channels make up each track.
    let groups: Vec<std::ops::Range<usize>> =
        if audio.channels == crate::clip::TRACK_CHANNELS { vec![0..2, 2..4] } else { vec![0..ch] };
    let frames = audio.frames();
    let mut tracks = vec![vec![0f32; bars]; groups.len()];
    for (f, frame) in audio.samples.chunks_exact(ch).enumerate() {
        let bar = (f * bars / frames.max(1)).min(bars - 1);
        for (t, range) in groups.iter().enumerate() {
            let p = &mut tracks[t][bar];
            for &s in &frame[range.clone()] {
                *p = p.max(s.abs());
            }
        }
    }
    let max = tracks.iter().flatten().fold(0f32, |m, &p| m.max(p));
    for p in tracks.iter_mut().flatten() {
        *p = if max > 1e-4 { (*p / max).clamp(0.0, 1.0) } else { 0.0 };
    }
    Ok(TrackPeaks { seconds: audio.seconds(), tracks })
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
    let audio = crate::audio::read(path)?;
    Ok((to_stereo(&audio, tracks, volumes), audio.rate))
}

/// `audio` as interleaved stereo; see `read_stereo`.
fn to_stereo(audio: &Audio, tracks: Tracks, volumes: Volumes) -> Vec<f32> {
    let mut samples = stereo_unclamped(audio, tracks, volumes);
    if audio.channels == crate::clip::TRACK_CHANNELS {
        for s in &mut samples {
            *s = s.clamp(-1.0, 1.0);
        }
    }
    samples
}

/// Like `to_stereo`, but a loud mix of two tracks can go past -1..=1 (so it
/// can be scaled down instead of clipped).
fn stereo_unclamped(audio: &Audio, tracks: Tracks, volumes: Volumes) -> Vec<f32> {
    let (d, m) = tracks.gains(volumes);
    match audio.channels.max(1) as usize {
        1 => audio.samples.iter().flat_map(|&s| [s, s]).collect(),
        2 => audio.samples.clone(),
        4 => audio.samples.chunks_exact(4).flat_map(|f| [f[0] * d + f[2] * m, f[1] * d + f[3] * m]).collect(),
        n => audio.samples.chunks_exact(n).flat_map(|f| [f[0], f[1]]).collect(),
    }
}

/// Discord's soundboard limits (#25), in one place in case Discord changes
/// them. The length is confirmed; the size isn't (512 KB is the figure
/// usually given). A 5 s MP3 at 192 kbps is about 120 KB, far below it.
pub const SOUNDBOARD_MAX_SECONDS: f32 = 5.0;
pub const SOUNDBOARD_MAX_BYTES: u64 = 512 * 1024;
/// Discord-ready clips are MP3, which the soundboard takes.
const SOUNDBOARD_FORMAT: Format = Format::Mp3High;
/// Discord-ready clips are scaled so their loudest peak is here (-1 dBFS).
const SOUNDBOARD_PEAK: f32 = 0.891;
/// Selections up to this much over the limit (rounding) are cut to the limit.
const SOUNDBOARD_SLACK_SECONDS: f32 = 0.05;
/// The MP3 encoder adds up to ~50 ms of silence (its delay at the start,
/// padding to a whole frame at the end). The audio is cut this much shorter
/// so the file itself still plays for at most `SOUNDBOARD_MAX_SECONDS`.
const MP3_PADDING_SECONDS: f32 = 0.05;

/// What `make_discord_ready` did.
#[derive(Debug, PartialEq)]
pub enum DiscordReady {
    /// Replaced the clip with this file.
    Done(PathBuf),
    /// Nothing changed: the clip (or selection) is this long, over the limit.
    TooLong(f32),
}

/// Is this clip already soundboard-ready as it is: a stereo (or mono) MP3
/// within the limits, without a custom mix?
pub fn is_discord_ready(clip: &ClipFile, seconds: f32, channels: u16, custom_mix: bool) -> bool {
    Format::of_path(&clip.path).is_some_and(|f| f.extension() == "mp3")
        && channels <= 2
        && seconds <= SOUNDBOARD_MAX_SECONDS + SOUNDBOARD_SLACK_SECONDS
        && clip.size <= SOUNDBOARD_MAX_BYTES
        && !custom_mix
}

/// Turns the part of `src` between `start` and `end` (0..=1) into a file for
/// Discord's soundboard (#25) and replaces the clip with it: the tracks mixed
/// into one stereo track at `volumes`, peak-normalised, as an MP3 within the
/// soundboard's limits. The original goes to the Recycle Bin; the new file
/// has the clip's name with `.mp3`. Longer than the limit: nothing changes.
pub fn make_discord_ready(src: &Path, start: f32, end: f32, volumes: Volumes) -> Result<DiscordReady> {
    make_discord_ready_with(src, start, end, volumes, recycle)
}

/// `make_discord_ready`, with `remove` getting rid of the original.
fn make_discord_ready_with(
    src: &Path,
    start: f32,
    end: f32,
    volumes: Volumes,
    remove: impl Fn(&Path) -> Result<()>,
) -> Result<DiscordReady> {
    let audio = crate::audio::read(src)?;
    let part = if start <= 0.0 && end >= 1.0 { audio } else { trimmed(&audio, start, end)? };
    let mut stereo = Audio { samples: stereo_unclamped(&part, Tracks::Both, volumes), channels: 2, rate: part.rate };
    let seconds = stereo.seconds();
    if seconds > SOUNDBOARD_MAX_SECONDS + SOUNDBOARD_SLACK_SECONDS {
        return Ok(DiscordReady::TooLong(seconds));
    }
    let max_frames = ((SOUNDBOARD_MAX_SECONDS - MP3_PADDING_SECONDS) * stereo.rate as f32) as usize;
    stereo.samples.truncate(max_frames * 2);
    normalize_peak(&mut stereo.samples, SOUNDBOARD_PEAK);

    let dir = src.parent().context("clip has no folder")?;
    let tmp = dir.join(format!(".discord-ready-{}.mp3", std::process::id()));
    crate::audio::write(&tmp, &stereo, SOUNDBOARD_FORMAT)?;
    let size = std::fs::metadata(&tmp).map(|m| m.len()).unwrap_or(0);
    if size > SOUNDBOARD_MAX_BYTES {
        let _ = std::fs::remove_file(&tmp);
        bail!("the MP3 is {} KB, over the soundboard's {} KB", size / 1024, SOUNDBOARD_MAX_BYTES / 1024);
    }
    let dest = discord_ready_path(src)?;
    if let Err(e) = remove(src) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    std::fs::rename(&tmp, &dest).with_context(|| format!("saving {}", dest.display()))?;
    Ok(DiscordReady::Done(dest))
}

/// Where the Discord-ready file of `src` goes: the same name with `.mp3`
/// (the same file if it's an MP3 already), without overwriting another clip.
fn discord_ready_path(src: &Path) -> Result<PathBuf> {
    let dir = src.parent().context("clip has no folder")?;
    let stem = src.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let same = |p: &Path| p.as_os_str().to_string_lossy().eq_ignore_ascii_case(&src.as_os_str().to_string_lossy());
    let mut dest = dir.join(format!("{stem}.mp3"));
    let mut n = 2;
    while dest.exists() && !same(&dest) {
        dest = dir.join(format!("{stem} ({n}).mp3"));
        n += 1;
    }
    Ok(dest)
}

/// Scales `samples` so the loudest one is at `peak`. Silence stays silent.
fn normalize_peak(samples: &mut [f32], peak: f32) {
    let max = samples.iter().fold(0f32, |m, s| m.max(s.abs()));
    if max < 1e-4 {
        return;
    }
    let gain = peak / max;
    for s in samples {
        *s *= gain;
    }
}

/// Number of channels in a clip (4 = separate tracks).
pub fn channels(path: &Path) -> Result<u16> {
    crate::audio::channels(path)
}

/// The file to hand to Discord (clipboard): the clip itself if it's a normal
/// stereo clip, otherwise a stereo mix (or one track) at the clip's
/// `volumes`, in the clip's format, written to a temporary file named like
/// the clip.
pub fn shareable_copy(src: &Path, tracks: Tracks, volumes: Volumes) -> Result<PathBuf> {
    if channels(src)? <= 2 {
        return Ok(src.to_path_buf());
    }
    let dir = std::env::temp_dir().join("discord-clip-share");
    std::fs::create_dir_all(&dir)?;
    let stem = src.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let ext = src.extension().map(|e| e.to_string_lossy().into_owned()).unwrap_or_else(|| "wav".into());
    let name = match tracks {
        Tracks::Both => format!("{stem}.{ext}"),
        Tracks::Discord => format!("{stem} (Discord only).{ext}"),
        Tracks::Mic => format!("{stem} (mic only).{ext}"),
    };
    let dest = dir.join(name);
    let audio = crate::audio::read(src)?;
    let format = Format::for_copy_of(src, &audio)?;
    let stereo = Audio { samples: to_stereo(&audio, tracks, volumes), channels: 2, rate: audio.rate };
    crate::audio::write(&dest, &stereo, format)?;
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

        let copy = save_edited_copy(&src, 0.25, 0.75).unwrap();
        assert_eq!(copy.file_name().unwrap(), "discord_clip_20261109_213151 Bob (edited).wav");
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
        assert_eq!(name.as_deref(), Some("Bob (edited)"));
        // Saving again doesn't overwrite.
        let second = save_edited_copy(&src, 0.0, 0.5).unwrap();
        assert_eq!(second.file_name().unwrap(), "discord_clip_20261109_213151 Bob (edited 2).wav");
        // The clip's own ends aren't cuts: a full-length copy doesn't fade.
        let full = save_edited_copy(&src, 0.0, 1.0).unwrap();
        let samples: Vec<i16> = hound::WavReader::open(&full).unwrap().samples::<i16>().map(|s| s.unwrap()).collect();
        assert!(samples[0] > 16_000 && samples[samples.len() - 1] > 16_000);

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

        // Trimming keeps both tracks.
        let trimmed = save_edited_copy(&src, 0.0, 0.5).unwrap();
        assert_eq!(channels(&trimmed).unwrap(), 4);

        // The Edit dialog's waveforms (#31): one per track, on one scale
        // (Discord peaks at 0.5, the mic at 0.25).
        let peaks = analyze_tracks(&src, 10).unwrap();
        assert_eq!(peaks.tracks.len(), 2);
        assert!(peaks.tracks.iter().all(|t| t.len() == 10));
        assert!(near(peaks.tracks[0][0], 1.0) && near(peaks.tracks[1][0], 0.5), "{:?}", peaks.tracks);
        assert!((peaks.seconds - 0.01).abs() < 0.001);
        assert_eq!(analyze_tracks(&stereo, 10).unwrap().tracks.len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// #25: one click turns a clip into a soundboard MP3 in its place.
    #[test]
    fn makes_clips_discord_ready() {
        let dir = std::env::temp_dir().join(format!("discord-clip-ready-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // 4 s, separate tracks: a quiet tone on Discord, a quieter one on the mic.
        let tone = |i: usize, f: f32, a: f32| a * (2.0 * std::f32::consts::PI * f * i as f32 / 48_000.0).sin();
        let samples: Vec<f32> = (0..48_000 * 4)
            .flat_map(|i| {
                let (d, m) = (tone(i, 440.0, 0.2), tone(i, 330.0, 0.1));
                [d, d, m, m]
            })
            .collect();
        let src = dir.join("discord_clip_20261113_120000 Bob.ogg");
        crate::audio::write(&src, &Audio { samples, channels: 4, rate: 48_000 }, Format::Opus).unwrap();
        // Tests delete the original instead of filling the Recycle Bin.
        let ready = |p: &Path, start: f32, end: f32, v: Volumes| {
            make_discord_ready_with(p, start, end, v, |p| Ok(std::fs::remove_file(p)?)).unwrap()
        };

        let v = Volumes { discord: 1.0, mic: 0.0 };
        let DiscordReady::Done(out) = ready(&src, 0.0, 1.0, v) else { panic!("too long?") };
        assert_eq!(out.file_name().unwrap(), "discord_clip_20261113_120000 Bob.mp3");
        assert!(!src.exists(), "the original is gone");
        let back = crate::audio::read(&out).unwrap();
        assert_eq!(back.channels, 2);
        let len = back.seconds();
        assert!((4.0..4.1).contains(&len), "{len}");
        // Peak-normalised to about -1 dBFS (MP3 can overshoot a little).
        let peak = back.samples.iter().fold(0f32, |m, s| m.max(s.abs()));
        assert!((0.8..=1.0).contains(&peak), "{peak}");
        let size = std::fs::metadata(&out).unwrap().len();
        assert!(size <= SOUNDBOARD_MAX_BYTES, "{size}");
        let clip = scan(&dir).into_iter().find(|c| c.path == out).unwrap();
        assert!(is_discord_ready(&clip, len, 2, false));
        assert!(!is_discord_ready(&clip, len, 2, true), "a custom mix still needs baking in");

        // A selection of exactly 5 s fits, even as an MP3 already.
        let wav = dir.join("long.wav");
        crate::clip::write_wav(&wav, &vec![0.25f32; 48_000 * 2 * 10]).unwrap();
        assert_eq!(ready(&wav, 0.0, 1.0, Volumes::default()), DiscordReady::TooLong(10.0));
        assert!(wav.exists(), "too long: nothing changed");
        let DiscordReady::Done(five) = ready(&wav, 0.5, 1.0, Volumes::default()) else { panic!() };
        let five_len = crate::audio::read(&five).unwrap().seconds();
        assert!((4.9..=SOUNDBOARD_MAX_SECONDS).contains(&five_len), "{five_len}");
        // Running it on an MP3 replaces it in place.
        let DiscordReady::Done(again) = ready(&five, 0.0, 1.0, Volumes::default()) else { panic!() };
        assert_eq!(again, five);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn normalizes_peaks() {
        let mut s = vec![0.1, -0.5, 0.25];
        normalize_peak(&mut s, 1.0);
        assert_eq!(s, vec![0.2, -1.0, 0.5]);
        let mut quiet = vec![0.0, 0.00001];
        normalize_peak(&mut quiet, 1.0);
        assert_eq!(quiet, vec![0.0, 0.00001]);
    }

    /// #8: Opus and FLAC clips with separate tracks are listed, analysed,
    /// trimmed and shared like WAV ones, staying in their own format.
    #[test]
    fn compressed_clips_work_like_wav() {
        let dir = std::env::temp_dir().join(format!("discord-clip-formats-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // 1 s, 4 channels: a tone on Discord, silence on the mic.
        let samples: Vec<f32> = (0..48_000)
            .flat_map(|i| {
                let s = 0.5 * (2.0 * std::f32::consts::PI * 440.0 * i as f32 / 48_000.0).sin();
                [s, s, 0.0, 0.0]
            })
            .collect();
        let audio = Audio { samples, channels: 4, rate: 48_000 };
        for format in [Format::Opus, Format::Flac] {
            let ext = format.extension();
            let src = dir.join(format!("discord_clip_20261113_101500 Bob.{ext}"));
            crate::audio::write(&src, &audio, format).unwrap();

            let a = analyze(&src).unwrap();
            assert_eq!(a.channels, 4, "{format:?}");
            assert!((a.seconds - 1.0).abs() < 0.01, "{format:?} {}", a.seconds);

            let copy = save_edited_copy(&src, 0.25, 0.75).unwrap();
            assert_eq!(copy.extension().unwrap(), ext);
            let t = crate::audio::read(&copy).unwrap();
            assert_eq!((t.channels, t.frames()), (4, 24_000), "{format:?}");

            // Discord gets stereo in the same format; muting Discord leaves silence.
            let shared = shareable_copy(&src, Tracks::Both, Volumes::default()).unwrap();
            assert_eq!(shared.extension().unwrap(), ext);
            assert_eq!(channels(&shared).unwrap(), 2);
            let muted = shareable_copy(&src, Tracks::Both, Volumes { discord: 0.0, mic: 1.0 }).unwrap();
            let quiet = crate::audio::read(&muted).unwrap();
            assert!(quiet.samples.iter().all(|s| s.abs() < 0.01), "{format:?}");
        }
        // Both formats show up on the Clips page, temporary files don't.
        std::fs::write(dir.join(".discord_clip_x.ogg.part"), b"x").unwrap();
        let listed: Vec<String> = scan(&dir).iter().map(|c| c.path.extension().unwrap().to_string_lossy().into_owned()).collect();
        assert!(listed.contains(&"ogg".to_string()) && listed.contains(&"flac".to_string()), "{listed:?}");
        assert!(scan(&dir).iter().all(|c| !c.path.to_string_lossy().contains(".part")));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
