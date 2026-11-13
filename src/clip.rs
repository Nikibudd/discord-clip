//! Turning ring-buffer snapshots into a clip file you can drop into Discord
//! (in the format picked in Settings, see `audio.rs`).

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

use crate::audio::Audio;
use crate::capture::{CHANNELS, SAMPLE_RATE};
use crate::config::Config;
use crate::ringbuffer::RingBuffer;

/// Channels of a clip that keeps Discord and the mic apart: Discord L/R,
/// then mic L/R.
pub const TRACK_CHANNELS: u16 = 4;

/// Snapshots the buffers and writes a timestamped clip in the configured
/// format into the clips folder: with the mic and `separate_tracks` on (and
/// a format that can hold them), a 4-channel file (Discord L/R + mic L/R);
/// otherwise both mixed into stereo.
pub fn save(cfg: &Config, discord: &RingBuffer, mic: Option<&RingBuffer>) -> Result<PathBuf> {
    let discord = discord.snapshot();
    let mic_samples = mic.map(|m| m.snapshot()).unwrap_or_default();
    if discord.is_empty() && mic_samples.is_empty() {
        bail!("nothing captured yet");
    }

    std::fs::create_dir_all(&cfg.out_dir)
        .with_context(|| format!("creating {}", cfg.out_dir.display()))?;
    let path = timestamped_path(&cfg.out_dir, cfg.format.extension());
    let audio = if cfg.separate_tracks && mic.is_some() && cfg.format.dual_track() {
        Audio { samples: interleave_tracks(&discord, &mic_samples, cfg.mic_gain), channels: TRACK_CHANNELS, rate: SAMPLE_RATE }
    } else {
        Audio { samples: mix(&[(&discord, 1.0), (&mic_samples, cfg.mic_gain)]), channels: CHANNELS, rate: SAMPLE_RATE }
    };
    crate::audio::write(&path, &audio, cfg.format)?;
    Ok(path)
}

/// Puts two stereo tracks side by side as 4 channels (`a` L/R, `b` L/R),
/// aligned at their end like `mix()`. `b` is scaled by `b_gain`.
pub fn interleave_tracks(a: &[f32], b: &[f32], b_gain: f32) -> Vec<f32> {
    let frames = a.len().max(b.len()) / 2;
    let mut out = vec![0.0f32; frames * 4];
    for (track, gain, channel) in [(a, 1.0, 0), (b, b_gain, 2)] {
        let offset = frames - track.len() / 2;
        for (i, pair) in track.chunks_exact(2).enumerate() {
            let o = (offset + i) * 4 + channel;
            out[o] = (pair[0] * gain).clamp(-1.0, 1.0);
            out[o + 1] = (pair[1] * gain).clamp(-1.0, 1.0);
        }
    }
    out
}

/// Mixes tracks aligned at their *end* (the moment the hotkey was pressed).
/// Shorter tracks (e.g. a stream that started later) are padded with silence
/// at the front. Output is clamped to [-1, 1].
pub fn mix(tracks: &[(&[f32], f32)]) -> Vec<f32> {
    let len = tracks.iter().map(|(t, _)| t.len()).max().unwrap_or(0);
    let mut out = vec![0.0f32; len];
    for &(track, gain) in tracks {
        let offset = len - track.len();
        for (o, &s) in out[offset..].iter_mut().zip(track) {
            *o += s * gain;
        }
    }
    for s in &mut out {
        *s = s.clamp(-1.0, 1.0);
    }
    out
}

/// Writes 16-bit PCM: 30s of 48kHz stereo is ~5.8MB, which fits under
/// Discord's 10MB free upload limit (32-bit float would not).
#[cfg(test)]
pub fn write_wav(path: &Path, samples: &[f32]) -> Result<()> {
    write_wav_channels(path, samples, CHANNELS)
}

/// Speaker layout we label 4-channel clips with: quad (front L/R + back L/R),
/// i.e. SPEAKER_FRONT_LEFT | FRONT_RIGHT | BACK_LEFT | BACK_RIGHT. Players that
/// open the file directly mix that into stereo with both tracks audible.
const QUAD_CHANNEL_MASK: u32 = 0x33;

/// hound labels 4 channels as 3.1 (front L/R, centre, subwoofer), so a
/// player would send half the mic track to the subwoofer. Rewrites the
/// WAVE_FORMAT_EXTENSIBLE channel mask of a 4-channel WAV to quad.
pub fn fix_channel_mask(path: &Path) -> Result<()> {
    use std::io::{Read, Seek, SeekFrom, Write};
    let mut f = std::fs::OpenOptions::new().read(true).write(true).open(path)?;
    // hound writes RIFF/WAVE and then the fmt chunk first: the extensible
    // fields start at byte 36 (cbSize), the channel mask at byte 40.
    let mut head = [0u8; 44];
    f.read_exact(&mut head)?;
    let tag = u16::from_le_bytes([head[20], head[21]]);
    let channels = u16::from_le_bytes([head[22], head[23]]);
    let cb_size = u16::from_le_bytes([head[36], head[37]]);
    if &head[0..4] != b"RIFF" || &head[12..16] != b"fmt " || tag != 0xFFFE || cb_size < 22 {
        bail!("unexpected WAV header in {}", path.display());
    }
    if channels == TRACK_CHANNELS {
        f.seek(SeekFrom::Start(40))?;
        f.write_all(&QUAD_CHANNEL_MASK.to_le_bytes())?;
    }
    Ok(())
}

/// Like `write_wav` with any number of interleaved channels. More than two
/// channels are written as WAVE_FORMAT_EXTENSIBLE (by hound); 4 are
/// labelled quad.
#[cfg(test)]
pub fn write_wav_channels(path: &Path, samples: &[f32], channels: u16) -> Result<()> {
    let audio = Audio { samples: samples.to_vec(), channels, rate: SAMPLE_RATE };
    crate::audio::write(path, &audio, crate::audio::Format::Wav)
}

/// `discord_clip_YYYYMMDD_HHMMSS.<extension>` in `dir`.
pub fn timestamped_path(dir: &Path, extension: &str) -> PathBuf {
    let t = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() };
    dir.join(format!(
        "discord_clip_{:04}{:02}{:02}_{:02}{:02}{:02}.{extension}",
        t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond
    ))
}

/// Puts the file on the clipboard as a file drop (CF_HDROP), so Ctrl+V in a
/// Discord chat box attaches it directly.
pub fn copy_file_to_clipboard(path: &Path) -> Result<()> {
    use windows::Win32::Foundation::{HANDLE, HGLOBAL};
    use windows::Win32::System::DataExchange::{
        CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData,
    };
    use windows::Win32::Foundation::GlobalFree;
    use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE, GMEM_ZEROINIT};
    use windows::Win32::UI::Shell::DROPFILES;

    const CF_HDROP: u32 = 15;

    let abs = std::fs::canonicalize(path)?;
    // canonicalize yields a \\?\ verbatim path; Explorer/Discord want a plain one.
    let abs = abs.to_string_lossy();
    let abs = abs.strip_prefix(r"\\?\").unwrap_or(&abs);
    // Path followed by the double-NUL terminator of the file list.
    let wide: Vec<u16> = abs.encode_utf16().chain([0, 0]).collect();

    let header = std::mem::size_of::<DROPFILES>();
    let total = header + wide.len() * 2;

    unsafe {
        let hmem: HGLOBAL = GlobalAlloc(GMEM_MOVEABLE | GMEM_ZEROINIT, total)?;
        let ptr = GlobalLock(hmem) as *mut u8;
        if ptr.is_null() {
            let _ = GlobalFree(hmem);
            anyhow::bail!("GlobalLock failed");
        }
        let df = ptr as *mut DROPFILES;
        (*df).pFiles = header as u32;
        (*df).fWide = true.into();
        std::ptr::copy_nonoverlapping(wide.as_ptr() as *const u8, ptr.add(header), wide.len() * 2);
        let _ = GlobalUnlock(hmem);

        OpenClipboard(None)?;
        let result = EmptyClipboard().and_then(|_| SetClipboardData(CF_HDROP, HANDLE(hmem.0)));
        let _ = CloseClipboard();
        if let Err(e) = result {
            // Ownership only passes to the clipboard on success.
            let _ = GlobalFree(hmem);
            return Err(e.into());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mix_aligns_at_end_and_applies_gain() {
        let a = [0.1, 0.2, 0.3, 0.4];
        let b = [1.0, 1.0];
        let out = mix(&[(&a, 1.0), (&b, 0.5)]);
        assert_eq!(out, vec![0.1, 0.2, 0.8, 0.9]);
    }

    #[test]
    fn mix_clamps() {
        let out = mix(&[(&[0.9, -0.9], 1.0), (&[0.9, -0.9], 1.0)]);
        assert_eq!(out, vec![1.0, -1.0]);
    }

    #[test]
    fn mix_empty() {
        assert!(mix(&[]).is_empty());
        assert!(mix(&[(&[], 1.0)]).is_empty());
    }

    #[test]
    fn interleaves_tracks_aligned_at_end() {
        // Discord: 2 frames, mic: 1 frame (started later) at gain 2.
        let discord = [0.1, 0.2, 0.3, 0.4];
        let mic = [0.25, -0.25];
        let out = interleave_tracks(&discord, &mic, 2.0);
        assert_eq!(out, vec![0.1, 0.2, 0.0, 0.0, 0.3, 0.4, 0.5, -0.5]);
        // Clamped like mix().
        assert_eq!(interleave_tracks(&[], &[0.9, -0.9], 2.0), vec![0.0, 0.0, 1.0, -1.0]);
    }
}
