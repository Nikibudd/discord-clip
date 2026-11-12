//! Turning ring-buffer snapshots into a WAV file you can drop into Discord.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

use crate::capture::{CHANNELS, SAMPLE_RATE};
use crate::config::Config;
use crate::ringbuffer::RingBuffer;

/// Snapshots the buffers, mixes them and writes a timestamped WAV into the
/// configured clips folder.
pub fn save(cfg: &Config, discord: &RingBuffer, mic: Option<&RingBuffer>) -> Result<PathBuf> {
    let discord = discord.snapshot();
    let mic = mic.map(|m| m.snapshot()).unwrap_or_default();
    if discord.is_empty() && mic.is_empty() {
        bail!("nothing captured yet");
    }
    let mixed = mix(&[(&discord, 1.0), (&mic, cfg.mic_gain)]);

    std::fs::create_dir_all(&cfg.out_dir)
        .with_context(|| format!("creating {}", cfg.out_dir.display()))?;
    let path = timestamped_path(&cfg.out_dir);
    write_wav(&path, &mixed)?;
    Ok(path)
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
pub fn write_wav(path: &Path, samples: &[f32]) -> Result<()> {
    let spec = hound::WavSpec {
        channels: CHANNELS,
        sample_rate: SAMPLE_RATE,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut w = hound::WavWriter::create(path, spec)?;
    for &s in samples {
        w.write_sample((s * 32767.0).round() as i16)?;
    }
    w.finalize()?;
    Ok(())
}

pub fn timestamped_path(dir: &Path) -> PathBuf {
    let t = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() };
    dir.join(format!(
        "discord_clip_{:04}{:02}{:02}_{:02}{:02}{:02}.wav",
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
}
