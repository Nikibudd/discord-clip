//! The clips folder as a library: listing clips, reading their length and a
//! waveform for display, playing them, and moving them to the Recycle Bin.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::{Context, Result};
use windows::core::{HSTRING, PCWSTR};
use windows::Win32::Media::Audio::{PlaySoundW, SND_ASYNC, SND_FILENAME, SND_FLAGS, SND_NODEFAULT};

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
}

/// Local date and time parsed from `discord_clip_YYYYMMDD_HHMMSS.wav`.
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
            let saved = path.file_name().and_then(|n| parse_saved_at(&n.to_string_lossy()));
            Some(ClipFile { path, size: meta.len(), modified: meta.modified().unwrap_or(SystemTime::UNIX_EPOCH), saved })
        })
        .collect();
    clips.sort_by(|a, b| b.modified.cmp(&a.modified).then_with(|| b.path.cmp(&a.path)));
    clips
}

pub fn parse_saved_at(name: &str) -> Option<SavedAt> {
    let stamp = name.strip_prefix("discord_clip_")?.strip_suffix(".wav")?;
    let (date, time) = stamp.split_once('_')?;
    if date.len() != 8 || time.len() < 6 || !date.bytes().chain(time.bytes()).all(|b| b.is_ascii_digit()) {
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

/// Plays a WAV file (asynchronously; replaces whatever is playing).
pub fn play(path: &Path) -> Result<()> {
    let ok = unsafe { PlaySoundW(&HSTRING::from(path.as_os_str()), None, SND_FILENAME | SND_ASYNC | SND_NODEFAULT) };
    if ok.as_bool() {
        Ok(())
    } else {
        anyhow::bail!("couldn't play {}", path.display())
    }
}

pub fn stop() {
    unsafe {
        let _ = PlaySoundW(PCWSTR::null(), None, SND_FLAGS(0));
    }
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
        assert_eq!(
            parse_saved_at("discord_clip_20261109_213151.wav"),
            Some(SavedAt { year: 2026, month: 11, day: 9, hour: 21, minute: 31, second: 51 })
        );
        assert_eq!(parse_saved_at("discord_clip_20261309_213151.wav"), None);
        assert_eq!(parse_saved_at("my recording.wav"), None);
        assert_eq!(parse_saved_at("discord_clip_2026_1.wav"), None);
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
