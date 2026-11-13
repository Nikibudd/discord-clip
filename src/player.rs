//! Clip playback with pause and seeking, on the classic `waveOut` API.
//!
//! The whole clip is decoded into memory as stereo (a 30 s clip is ~5.8 MB)
//! and handed to the sound card in one buffer starting at the play position.
//! Seeking resets the device and queues the buffer again from the new
//! position; `waveOutGetPosition` tells us how far it has played since.
//!
//! Clips with separate tracks are mixed at their volumes (#7). Both tracks
//! are kept, so a volume change re-mixes the buffer in place and is heard
//! straight away, without stopping.

use std::path::{Path, PathBuf};

use anyhow::{bail, Result};

use crate::library::{self, Tracks};
use crate::volumes::Volumes;
use windows::core::PSTR;
use windows::Win32::Media::Audio::{
    waveOutClose, waveOutGetPosition, waveOutOpen, waveOutPause, waveOutPrepareHeader, waveOutReset,
    waveOutRestart, waveOutUnprepareHeader, waveOutWrite, CALLBACK_NULL, HWAVEOUT, WAVEFORMATEX, WAVEHDR,
    WAVE_FORMAT_PCM, WAVE_MAPPER,
};
use windows::Win32::Media::{MMTIME, TIME_SAMPLES};

/// One clip loaded and playing (or paused).
pub struct Playback {
    path: PathBuf,
    device: HWAVEOUT,
    /// Interleaved 16-bit samples of the whole clip.
    samples: Vec<i16>,
    /// Clips with separate tracks: the Discord and mic tracks (stereo each),
    /// to re-mix `samples` from when the volumes change.
    tracks: Option<(Vec<i16>, Vec<i16>)>,
    volumes: Volumes,
    channels: usize,
    sample_rate: u32,
    /// Frame the queued buffer starts at.
    start_frame: usize,
    /// Boxed so its address stays put while the device uses it.
    header: Box<WAVEHDR>,
    queued: bool,
    paused: bool,
}

fn to_i16(samples: &[f32]) -> Vec<i16> {
    samples.iter().map(|&s| (s.clamp(-1.0, 1.0) * i16::MAX as f32).round() as i16).collect()
}

/// Mixes two tracks into `out` (all the same length) at `v`.
fn mix_into(out: &mut [i16], discord: &[i16], mic: &[i16], v: Volumes) {
    for ((o, &d), &m) in out.iter_mut().zip(discord).zip(mic) {
        *o = (d as f32 * v.discord + m as f32 * v.mic).round().clamp(i16::MIN as f32, i16::MAX as f32) as i16;
    }
}

impl Playback {
    /// Loads `path` and starts playing at `fraction` (0..=1) of the clip. A
    /// clip with separate tracks is mixed at `volumes`.
    pub fn start(path: &Path, fraction: f32, volumes: Volumes) -> Result<Playback> {
        // Always stereo: clips with separate tracks are mixed for listening.
        let (samples, tracks, sample_rate) = if library::channels(path)? == crate::clip::TRACK_CHANNELS {
            let (discord, rate) = library::read_stereo(path, Tracks::Discord, Volumes::default())?;
            let (mic, _) = library::read_stereo(path, Tracks::Mic, Volumes::default())?;
            let (discord, mic) = (to_i16(&discord), to_i16(&mic));
            let mut samples = vec![0i16; discord.len()];
            mix_into(&mut samples, &discord, &mic, volumes);
            (samples, Some((discord, mic)), rate)
        } else {
            let (mixed, rate) = library::read_stereo(path, Tracks::Both, volumes)?;
            (to_i16(&mixed), None, rate)
        };
        let channels = 2usize;
        let block_align = (channels * 2) as u16;
        let format = WAVEFORMATEX {
            wFormatTag: WAVE_FORMAT_PCM as u16,
            nChannels: channels as u16,
            nSamplesPerSec: sample_rate,
            nAvgBytesPerSec: sample_rate * block_align as u32,
            nBlockAlign: block_align,
            wBitsPerSample: 16,
            cbSize: 0,
        };
        let mut device = HWAVEOUT::default();
        let code = unsafe { waveOutOpen(Some(&mut device), WAVE_MAPPER, &format, 0, 0, CALLBACK_NULL) };
        if code != 0 {
            bail!("couldn't open the audio output (error {code})");
        }
        let mut playback = Playback {
            path: path.to_path_buf(),
            device,
            samples,
            tracks,
            volumes,
            channels,
            sample_rate: sample_rate.max(1),
            start_frame: 0,
            header: Box::new(WAVEHDR::default()),
            queued: false,
            paused: false,
        };
        playback.seek(fraction)?;
        Ok(playback)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Changes the volumes of a clip with separate tracks while it plays.
    /// The buffer the device is reading is re-mixed in place (same length,
    /// same address), so the change is heard almost at once and playback
    /// doesn't stop or jump.
    pub fn set_volumes(&mut self, v: Volumes) {
        if v == self.volumes {
            return;
        }
        self.volumes = v;
        if let Some((discord, mic)) = &self.tracks {
            mix_into(&mut self.samples, discord, mic, v);
        }
    }

    /// The file was renamed. Playback is unaffected (the samples are in memory).
    pub fn set_path(&mut self, path: PathBuf) {
        self.path = path;
    }

    pub fn is_paused(&self) -> bool {
        self.paused
    }

    fn total_frames(&self) -> usize {
        self.samples.len() / self.channels
    }

    /// Frames played since the buffer was queued.
    fn played_frames(&self) -> usize {
        if !self.queued {
            return 0;
        }
        let mut t = MMTIME { wType: TIME_SAMPLES, ..Default::default() };
        let code = unsafe { waveOutGetPosition(self.device, &mut t, std::mem::size_of::<MMTIME>() as u32) };
        if code != 0 {
            return 0;
        }
        // Drivers may report another unit than asked for.
        unsafe {
            match t.wType {
                TIME_SAMPLES => t.u.sample as usize,
                windows::Win32::Media::TIME_BYTES => t.u.cb as usize / (self.channels * 2),
                windows::Win32::Media::TIME_MS => t.u.ms as usize * self.sample_rate as usize / 1000,
                _ => 0,
            }
        }
    }

    /// Playback position, 0..=1.
    pub fn position(&self) -> f32 {
        let total = self.total_frames().max(1);
        ((self.start_frame + self.played_frames()) as f32 / total as f32).clamp(0.0, 1.0)
    }

    pub fn finished(&self) -> bool {
        self.start_frame + self.played_frames() >= self.total_frames()
    }

    /// Jumps to `fraction` (0..=1) of the clip. Keeps the paused state.
    pub fn seek(&mut self, fraction: f32) -> Result<()> {
        self.dequeue();
        let total = self.total_frames();
        self.start_frame = ((fraction.clamp(0.0, 1.0) * total as f32) as usize).min(total);
        if self.start_frame >= total {
            return Ok(()); // at the end: nothing to play
        }
        let data = &self.samples[self.start_frame * self.channels..];
        *self.header = WAVEHDR {
            lpData: PSTR(data.as_ptr() as *mut u8),
            dwBufferLength: (data.len() * 2) as u32,
            ..Default::default()
        };
        let size = std::mem::size_of::<WAVEHDR>() as u32;
        unsafe {
            if self.paused {
                // Queue it without starting; Restart resumes from here.
                let _ = waveOutPause(self.device);
            }
            let code = waveOutPrepareHeader(self.device, &mut *self.header, size);
            if code != 0 {
                bail!("couldn't prepare audio (error {code})");
            }
            let code = waveOutWrite(self.device, &mut *self.header, size);
            if code != 0 {
                let _ = waveOutUnprepareHeader(self.device, &mut *self.header, size);
                bail!("couldn't play audio (error {code})");
            }
        }
        self.queued = true;
        Ok(())
    }

    pub fn pause(&mut self) {
        if !self.paused {
            unsafe {
                let _ = waveOutPause(self.device);
            }
            self.paused = true;
        }
    }

    /// Resumes; at the end of the clip, starts over.
    pub fn resume(&mut self) -> Result<()> {
        if self.finished() {
            self.paused = false;
            return self.seek(0.0);
        }
        if self.paused {
            unsafe {
                let _ = waveOutRestart(self.device);
            }
            self.paused = false;
        }
        Ok(())
    }

    /// Stops the device and releases the queued buffer.
    fn dequeue(&mut self) {
        if self.queued {
            unsafe {
                // Reset also returns the buffer, so it can be unprepared.
                let _ = waveOutReset(self.device);
                let _ = waveOutUnprepareHeader(self.device, &mut *self.header, std::mem::size_of::<WAVEHDR>() as u32);
            }
            self.queued = false;
        }
    }
}

impl Drop for Playback {
    fn drop(&mut self) {
        self.dequeue();
        unsafe {
            let _ = waveOutClose(self.device);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn mixes_tracks_at_volumes() {
        let mut out = [0i16; 4];
        mix_into(&mut out, &[1000, -1000, 30000, 0], &[500, 500, 30000, -20000], Volumes { discord: 0.5, mic: 2.0 });
        // Clipped at the 16-bit limits.
        assert_eq!(out, [1500, 500, i16::MAX, i16::MIN]);
    }

    /// Needs a sound device: `cargo test -- --ignored playback`. Silent.
    /// Changing the volumes of a 2-track clip while it plays re-mixes the
    /// buffer without stopping or moving the playhead.
    #[test]
    #[ignore]
    fn playback_volume_change() {
        let dir = std::env::temp_dir().join(format!("discord-clip-player-vol-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("tracks.wav");
        // 1 s of 4 channels: Discord at 0.0001, mic at 0.0002 (inaudible).
        let frame = [0.0001f32, 0.0001, 0.0002, 0.0002];
        let samples: Vec<f32> = frame.iter().cycle().take(4 * 48_000).copied().collect();
        crate::clip::write_wav_channels(&path, &samples, crate::clip::TRACK_CHANNELS).unwrap();

        let mut p = Playback::start(&path, 0.0, Volumes { discord: 1.0, mic: 0.0 }).unwrap();
        assert_eq!(p.samples[0], 3, "Discord only");
        std::thread::sleep(Duration::from_millis(200));
        let before = p.position();
        let ptr = p.samples.as_ptr();
        p.set_volumes(Volumes { discord: 0.0, mic: 1.0 });
        assert_eq!(p.samples.as_ptr(), ptr, "buffer stays put");
        assert_eq!(p.samples[0], 7, "mic only");
        assert!(!p.is_paused() && p.position() >= before, "still playing");
        drop(p);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Needs a sound device (none on CI): `cargo test -- --ignored playback`.
    /// Plays a silent clip, so it makes no noise.
    #[test]
    #[ignore]
    fn playback_seek_pause() {
        let dir = std::env::temp_dir().join(format!("discord-clip-player-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("silence.wav");
        crate::clip::write_wav(&path, &vec![0.0f32; 48_000 * 2 * 2]).unwrap(); // 2 s

        let mut p = Playback::start(&path, 0.5, Volumes::default()).unwrap();
        assert!((p.position() - 0.5).abs() < 0.02, "start {}", p.position());
        std::thread::sleep(Duration::from_millis(300));
        let moved = p.position();
        assert!(moved > 0.55 && moved < 0.75, "after 300 ms {moved}");

        // Seeking while playing reports the new position straight away (no
        // stale value from before the seek, see #12).
        p.seek(0.25).unwrap();
        assert!((p.position() - 0.25).abs() < 0.02, "seek while playing {}", p.position());
        std::thread::sleep(Duration::from_millis(100));
        assert!(p.position() > 0.25 && p.position() < 0.4, "after seek {}", p.position());

        p.pause();
        let paused_at = p.position();
        std::thread::sleep(Duration::from_millis(200));
        assert!((p.position() - paused_at).abs() < 0.01, "paused moved {} -> {}", paused_at, p.position());

        p.seek(0.9).unwrap();
        assert!(p.is_paused());
        assert!((p.position() - 0.9).abs() < 0.02, "seek {}", p.position());
        p.resume().unwrap();
        std::thread::sleep(Duration::from_millis(400));
        assert!(p.finished(), "should have played the last 0.2 s: {}", p.position());

        drop(p);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
