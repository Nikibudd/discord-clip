//! Clip file formats (#8): which formats clips can be saved in, and reading
//! and writing them.
//!
//! * WAV: `hound`. Clips with separate tracks are 4-channel files labelled quad.
//! * Opus in Ogg (`.ogg`): `ruopus` (pure Rust) for the codec and `ogg` for
//!   the container. Separate tracks are 4 channels as channel mapping family 1
//!   (quad): two coupled stereo streams, Discord then mic.
//! * FLAC: `libflac-rs` (pure Rust), 4 channels natively.
//! * MP3: written with Windows Media Foundation's built-in encoder (stereo
//!   only), read with `symphonia`.
//!
//! Everything is read whole into memory as interleaved f32 (-1..=1): clips
//! are short, and compressed ones are small.

use std::path::Path;

use anyhow::{bail, Context, Result};

/// A format new clips can be saved in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Opus,
    Mp3Low,
    Mp3High,
    Flac,
    Wav,
}

/// File extensions the Clips page lists.
pub const EXTENSIONS: [&str; 4] = ["wav", "ogg", "flac", "mp3"];

/// Opus bitrate per stereo stream (a clip with separate tracks has two).
const OPUS_BITRATE: u32 = 64_000;

impl Format {
    pub const ALL: [Format; 5] = [Format::Opus, Format::Mp3Low, Format::Mp3High, Format::Flac, Format::Wav];

    /// How it's written in config.ini.
    pub fn key(self) -> &'static str {
        match self {
            Format::Opus => "opus",
            Format::Mp3Low => "mp3-128",
            Format::Mp3High => "mp3-192",
            Format::Flac => "flac",
            Format::Wav => "wav",
        }
    }

    pub fn from_key(s: &str) -> Option<Format> {
        Format::ALL.into_iter().find(|f| f.key().eq_ignore_ascii_case(s.trim()))
    }

    pub fn label(self) -> &'static str {
        match self {
            Format::Opus => "Opus (.ogg), 64 kbps",
            Format::Mp3Low => "MP3, 128 kbps",
            Format::Mp3High => "MP3, 192 kbps",
            Format::Flac => "FLAC (lossless)",
            Format::Wav => "WAV (uncompressed)",
        }
    }

    pub fn extension(self) -> &'static str {
        match self {
            Format::Opus => "ogg",
            Format::Mp3Low | Format::Mp3High => "mp3",
            Format::Flac => "flac",
            Format::Wav => "wav",
        }
    }

    /// Can hold Discord and the mic as separate tracks (4 channels).
    pub fn dual_track(self) -> bool {
        !matches!(self, Format::Mp3Low | Format::Mp3High)
    }

    /// Rough size of one second of stereo audio, for size hints. FLAC
    /// depends on the audio; game and voice chat usually compress to about
    /// 60% of WAV.
    pub fn stereo_bytes_per_sec(self) -> f32 {
        match self {
            Format::Opus => OPUS_BITRATE as f32 / 8.0 * 1.03, // + Ogg overhead
            Format::Mp3Low => 16_000.0,
            Format::Mp3High => 24_000.0,
            Format::Flac => 192_000.0 * 0.6,
            Format::Wav => 192_000.0,
        }
    }

    /// The format of an existing file, from its extension. MP3 files count
    /// as 128 kbps; see `for_copy_of` for keeping their bitrate.
    pub fn of_path(path: &Path) -> Option<Format> {
        let ext = path.extension()?.to_string_lossy().to_ascii_lowercase();
        match ext.as_str() {
            "ogg" | "opus" => Some(Format::Opus),
            "mp3" => Some(Format::Mp3Low),
            "flac" => Some(Format::Flac),
            "wav" => Some(Format::Wav),
            _ => None,
        }
    }

    /// The format to write a copy of `src` in (trimmed or mixed): the same
    /// one, at about the same MP3 bitrate.
    pub fn for_copy_of(src: &Path, audio: &Audio) -> Result<Format> {
        let format = Format::of_path(src).with_context(|| format!("unsupported file type: {}", src.display()))?;
        if format == Format::Mp3Low {
            let size = std::fs::metadata(src).map(|m| m.len()).unwrap_or(0) as f32;
            let kbps = size * 8.0 / audio.seconds().max(0.1) / 1000.0;
            return Ok(if kbps > 160.0 { Format::Mp3High } else { Format::Mp3Low });
        }
        Ok(format)
    }
}

/// Decoded audio: interleaved samples in -1..=1.
#[derive(Clone, Debug, PartialEq)]
pub struct Audio {
    pub samples: Vec<f32>,
    pub channels: u16,
    pub rate: u32,
}

impl Audio {
    pub fn frames(&self) -> usize {
        self.samples.len() / self.channels.max(1) as usize
    }

    pub fn seconds(&self) -> f32 {
        self.frames() as f32 / self.rate.max(1) as f32
    }
}

/// Reads a whole clip, whatever its format (by extension).
pub fn read(path: &Path) -> Result<Audio> {
    let ctx = || format!("reading {}", path.display());
    match Format::of_path(path) {
        Some(Format::Wav) => read_wav(path).with_context(ctx),
        Some(Format::Opus) => read_opus(path).with_context(ctx),
        Some(Format::Flac) => read_flac(path).with_context(ctx),
        Some(Format::Mp3Low | Format::Mp3High) => read_mp3(path).with_context(ctx),
        None => bail!("unsupported file type: {}", path.display()),
    }
}

/// Writes `audio` to `path` in `format`. The file is written next to `path`
/// first and then moved over it, so a failed encode never leaves half a clip.
/// (The Clips page skips files starting with a dot, like that one.)
pub fn write(path: &Path, audio: &Audio, format: Format) -> Result<()> {
    let name = path.file_name().context("no file name")?.to_string_lossy();
    let tmp = path.with_file_name(format!(".{name}.part"));
    let result = match format {
        Format::Wav => write_wav(&tmp, audio),
        Format::Opus => write_opus(&tmp, audio),
        Format::Flac => write_flac(&tmp, audio),
        Format::Mp3Low => write_mp3(&tmp, audio, 128),
        Format::Mp3High => write_mp3(&tmp, audio, 192),
    };
    match result.and_then(|()| Ok(std::fs::rename(&tmp, path)?)) {
        Ok(()) => Ok(()),
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(e.context(format!("writing {}", path.display())))
        }
    }
}

/// Number of channels of a clip (4 = separate tracks), from its header
/// where that's cheap.
pub fn channels(path: &Path) -> Result<u16> {
    match Format::of_path(path) {
        Some(Format::Wav) => Ok(hound::WavReader::open(path).with_context(|| format!("opening {}", path.display()))?.spec().channels),
        Some(Format::Opus | Format::Flac) => {
            use std::io::Read;
            let mut head = [0u8; 64];
            let mut f = std::fs::File::open(path).with_context(|| format!("opening {}", path.display()))?;
            let n = f.read(&mut head)?;
            header_channels(&head[..n]).map(Ok).unwrap_or_else(|| Ok(read(path)?.channels))
        }
        Some(Format::Mp3Low | Format::Mp3High) => Ok(2),
        None => bail!("unsupported file type: {}", path.display()),
    }
}

/// Channel count from the start of an Ogg Opus or FLAC file.
fn header_channels(head: &[u8]) -> Option<u16> {
    // Ogg: a 27-byte page header and a 1-entry segment table, then OpusHead.
    if head.starts_with(b"OggS") && head.get(28..36) == Some(b"OpusHead") {
        return head.get(37).map(|&c| c as u16);
    }
    // FLAC: "fLaC", a 4-byte block header, then STREAMINFO with the channel
    // count (minus one) in bits 1..=3 of its 13th byte.
    if head.starts_with(b"fLaC") && head.len() > 20 && head[4] & 0x7F == 0 {
        return Some(((head[20] >> 1) & 0x07) as u16 + 1);
    }
    None
}

// ---------------------------------------------------------------------------
// WAV
// ---------------------------------------------------------------------------

fn read_wav(path: &Path) -> Result<Audio> {
    let mut reader = hound::WavReader::open(path)?;
    let spec = reader.spec();
    let samples: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => reader.samples::<f32>().collect::<Result<_, _>>()?,
        hound::SampleFormat::Int => {
            let scale = 1.0 / (1u64 << (spec.bits_per_sample.clamp(1, 32) - 1)) as f32;
            reader.samples::<i32>().map(|s| s.map(|v| v as f32 * scale)).collect::<Result<_, _>>()?
        }
    };
    Ok(Audio { samples, channels: spec.channels.max(1), rate: spec.sample_rate })
}

/// 16-bit PCM. 4-channel files are labelled quad (see `fix_channel_mask`).
fn write_wav(path: &Path, audio: &Audio) -> Result<()> {
    let spec = hound::WavSpec {
        channels: audio.channels,
        sample_rate: audio.rate,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut w = hound::WavWriter::create(path, spec)?;
    for &s in &audio.samples {
        w.write_sample(to_i16(s))?;
    }
    w.finalize()?;
    if audio.channels == crate::clip::TRACK_CHANNELS {
        crate::clip::fix_channel_mask(path)?;
    }
    Ok(())
}

fn to_i16(s: f32) -> i16 {
    (s.clamp(-1.0, 1.0) * 32767.0).round() as i16
}

// ---------------------------------------------------------------------------
// Opus in Ogg
// ---------------------------------------------------------------------------

/// Opus always runs at 48 kHz.
const OPUS_RATE: u32 = 48_000;
/// 20 ms frames.
const OPUS_FRAME: usize = 960;
/// The encoder's delay at our bitrate (CELT; ruopus picks CELT above
/// 40 kb/s): players skip this many samples at the start.
const OPUS_PRE_SKIP: usize = 120;

fn write_opus(path: &Path, audio: &Audio) -> Result<()> {
    use ruopus::ogg::{ChannelMapping, OpusHead, OpusTags};
    if audio.rate != OPUS_RATE {
        bail!("Opus needs 48 kHz audio, this is {} Hz", audio.rate);
    }
    let ch = audio.channels as usize;
    // Mono is stored as stereo; 4 channels are two stereo streams.
    let (streams, mapping) = match ch {
        1 | 2 => (1, ChannelMapping::Family0),
        4 => (2, ChannelMapping::Table { family: 1, stream_count: 2, coupled_count: 2, mapping: vec![0, 1, 2, 3] }),
        n => bail!("Opus clips can't have {n} channels"),
    };
    let out_channels = if ch == 1 { 2 } else { ch };
    let head = OpusHead {
        version: 1,
        channel_count: out_channels as u8,
        pre_skip: OPUS_PRE_SKIP as u16,
        input_sample_rate: OPUS_RATE,
        output_gain_q8: 0,
        channel_mapping: mapping,
    };
    let tags = OpusTags { vendor: format!("discord-clip {}", env!("CARGO_PKG_VERSION")).into_bytes(), comments: Vec::new() };

    let mut encoders: Vec<ruopus::OpusEncoder> = (0..streams)
        .map(|_| {
            let mut e = ruopus::OpusEncoder::new(2);
            e.set_bandwidth(ruopus::Bandwidth::FullBand);
            e.set_bitrate(Some(OPUS_BITRATE));
            e
        })
        .collect();

    let file = std::io::BufWriter::new(std::fs::File::create(path)?);
    let mut ogg = ogg::PacketWriter::new(file);
    const SERIAL: u32 = 0x4443_4C50; // "DCLP"
    use ogg::PacketWriteEndInfo::{EndPage, EndStream, NormalPacket};
    ogg.write_packet(head.to_bytes(), SERIAL, EndPage, 0)?;
    ogg.write_packet(tags.to_bytes(), SERIAL, EndPage, 0)?;

    // Encode the pre-skip's worth of extra (silent) samples at the end, so
    // the last real sample comes out of the decoder.
    let frames = audio.frames();
    let packets = (frames + OPUS_PRE_SKIP).div_ceil(OPUS_FRAME).max(1);
    let end_granule = (OPUS_PRE_SKIP + frames) as u64;
    let mut pcm = vec![0f32; OPUS_FRAME * 2];
    for p in 0..packets {
        let mut packet = Vec::new();
        for (s, enc) in encoders.iter_mut().enumerate() {
            for i in 0..OPUS_FRAME {
                let f = p * OPUS_FRAME + i;
                let (l, r) = if f >= frames {
                    (0.0, 0.0)
                } else if ch == 1 {
                    (audio.samples[f], audio.samples[f])
                } else {
                    (audio.samples[f * ch + s * 2], audio.samples[f * ch + s * 2 + 1])
                };
                pcm[i * 2] = l;
                pcm[i * 2 + 1] = r;
            }
            let encoded = enc.encode_auto(&pcm, 1275).map_err(|e| anyhow::anyhow!("Opus encoder: {e:?}"))?;
            if s + 1 < streams {
                packet.extend(self_delimited(&encoded)?);
            } else {
                packet.extend(encoded);
            }
        }
        let last = p + 1 == packets;
        let granule = if last { end_granule } else { (OPUS_PRE_SKIP + (p + 1) * OPUS_FRAME) as u64 };
        ogg.write_packet(packet, SERIAL, if last { EndStream } else { NormalPacket }, granule)?;
    }
    use std::io::Write;
    ogg.into_inner().flush()?;
    Ok(())
}

/// Re-frames an Opus packet with self-delimiting framing (RFC 6716
/// Appendix B), as every stream but the last of a multistream packet must
/// be: a code 3 packet whose frame lengths are all coded, the last one too.
fn self_delimited(packet: &[u8]) -> Result<Vec<u8>> {
    let parsed = ruopus::Packet::parse(packet).map_err(|e| anyhow::anyhow!("bad Opus packet: {e:?}"))?;
    let frames = parsed.frames();
    let toc = (packet[0] & !0x03) | 0x03;
    let mut out = vec![toc, 0x80 | frames.len() as u8]; // VBR, no padding
    for f in frames {
        put_length(&mut out, f.len());
    }
    for f in frames {
        out.extend_from_slice(f);
    }
    Ok(out)
}

/// Opus frame length coding (RFC 6716 §3.1): one byte below 252, else two.
fn put_length(out: &mut Vec<u8>, n: usize) {
    if n < 252 {
        out.push(n as u8);
    } else {
        let first = 252 + (n & 0x03);
        out.push(first as u8);
        out.push(((n - first) / 4) as u8);
    }
}

fn read_opus(path: &Path) -> Result<Audio> {
    let data = std::fs::read(path)?;
    let (samples, head) = ruopus::decode_ogg_opus(&data).map_err(|e| anyhow::anyhow!("not a readable Opus file ({e:?})"))?;
    Ok(Audio { samples, channels: head.channel_count.max(1) as u16, rate: OPUS_RATE })
}

// ---------------------------------------------------------------------------
// FLAC
// ---------------------------------------------------------------------------

fn write_flac(path: &Path, audio: &Audio) -> Result<()> {
    if !(1..=8).contains(&audio.channels) {
        bail!("FLAC can't have {} channels", audio.channels);
    }
    let ints: Vec<i32> = audio.samples.iter().map(|&s| to_i16(s) as i32).collect();
    // Level 5: libFLAC's default, a good balance of size and speed.
    let config = libflac_rs::EncoderConfig::new(audio.channels as u32, 16, audio.rate).with_compression_level(5);
    std::fs::write(path, libflac_rs::Encoder::new(config).encode(&ints))?;
    Ok(())
}

fn read_flac(path: &Path) -> Result<Audio> {
    let data = std::fs::read(path)?;
    let d = libflac_rs::decode(&data).context("not a readable FLAC file")?;
    let scale = 1.0 / (1u64 << (d.bits_per_sample.clamp(1, 32) - 1)) as f32;
    Ok(Audio {
        samples: d.interleaved.iter().map(|&s| s as f32 * scale).collect(),
        channels: d.channels.max(1) as u16,
        rate: d.sample_rate,
    })
}

// ---------------------------------------------------------------------------
// MP3
// ---------------------------------------------------------------------------

fn read_mp3(path: &Path) -> Result<Audio> {
    use symphonia::core::codecs::audio::AudioDecoderOptions;
    use symphonia::core::formats::probe::Hint;
    use symphonia::core::formats::TrackType;
    use symphonia::core::io::MediaSourceStream;

    let file = Box::new(std::fs::File::open(path)?);
    let stream = MediaSourceStream::new(file, Default::default());
    let mut hint = Hint::new();
    hint.with_extension("mp3");
    let mut format = symphonia::default::get_probe()
        .probe(&hint, stream, Default::default(), Default::default())
        .map_err(|e| anyhow::anyhow!("not a readable MP3 file ({e})"))?;
    let track = format.default_track(TrackType::Audio).context("no audio in the file")?;
    let params = track.codec_params.as_ref().and_then(|p| p.audio()).context("no audio in the file")?;
    let mut decoder = symphonia::default::get_codecs()
        .make_audio_decoder(params, &AudioDecoderOptions::default())
        .map_err(|e| anyhow::anyhow!("can't decode this MP3 ({e})"))?;
    let track_id = track.id;

    let mut samples = Vec::new();
    let mut buf: Vec<f32> = Vec::new();
    let (mut channels, mut rate) = (0u16, 0u32);
    // The end of the file shows up as an error; anything decoded so far counts.
    while let Ok(Some(packet)) = format.next_packet() {
        if packet.track_id != track_id {
            continue;
        }
        let Ok(decoded) = decoder.decode(&packet) else { continue }; // skip a corrupt frame
        channels = decoded.spec().channels().count() as u16;
        rate = decoded.spec().rate();
        buf.resize(decoded.samples_interleaved(), 0.0);
        decoded.copy_to_slice_interleaved(&mut buf);
        samples.extend_from_slice(&buf);
    }
    if channels == 0 {
        bail!("no audio in the file");
    }
    Ok(Audio { samples, channels, rate })
}

/// Encodes with Media Foundation's MP3 encoder (Windows 8 and later).
fn write_mp3(path: &Path, audio: &Audio, kbps: u32) -> Result<()> {
    use windows::core::HSTRING;
    use windows::Win32::Media::MediaFoundation::*;
    use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_MULTITHREADED};

    if audio.channels > 2 {
        bail!("MP3 can't hold separate tracks");
    }
    let channels = audio.channels as u32;
    // The writer picks the container from the file extension.
    let target = path.with_extension("part.mp3");
    unsafe {
        let com = CoInitializeEx(None, COINIT_MULTITHREADED).is_ok();
        MFStartup(MF_VERSION, MFSTARTUP_LITE).context("starting Media Foundation")?;
        let result = (|| -> Result<()> {
            let writer = MFCreateSinkWriterFromURL(&HSTRING::from(target.as_os_str()), None, None)
                .context("creating the MP3 writer")?;

            let out = MFCreateMediaType()?;
            out.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio)?;
            out.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_MP3)?;
            out.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, audio.rate)?;
            out.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, channels)?;
            out.SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, kbps * 1000 / 8)?;
            let stream = writer.AddStream(&out).context("the MP3 encoder doesn't support this audio")?;

            let block = channels * 2;
            let input = MFCreateMediaType()?;
            input.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio)?;
            input.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_PCM)?;
            input.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16)?;
            input.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, audio.rate)?;
            input.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, channels)?;
            input.SetUINT32(&MF_MT_AUDIO_BLOCK_ALIGNMENT, block)?;
            input.SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, block * audio.rate)?;
            writer.SetInputMediaType(stream, &input, None).context("setting the MP3 input format")?;
            writer.BeginWriting()?;

            // Hand it over in 100 ms pieces.
            let ch = audio.channels as usize;
            let chunk_frames = (audio.rate / 10).max(1) as usize;
            let hns = |frames: usize| (frames as i64) * 10_000_000 / audio.rate.max(1) as i64;
            let mut at = 0usize;
            for chunk in audio.samples.chunks(chunk_frames * ch) {
                let bytes = (chunk.len() * 2) as u32;
                let buffer = MFCreateMemoryBuffer(bytes)?;
                let mut ptr = std::ptr::null_mut();
                buffer.Lock(&mut ptr, None, None)?;
                let dst = std::slice::from_raw_parts_mut(ptr as *mut i16, chunk.len());
                for (d, &s) in dst.iter_mut().zip(chunk) {
                    *d = to_i16(s);
                }
                buffer.Unlock()?;
                buffer.SetCurrentLength(bytes)?;
                let sample = MFCreateSample()?;
                sample.AddBuffer(&buffer)?;
                let frames = chunk.len() / ch;
                sample.SetSampleTime(hns(at))?;
                sample.SetSampleDuration(hns(frames))?;
                writer.WriteSample(stream, &sample)?;
                at += frames;
            }
            writer.Finalize().context("finishing the MP3 file")?;
            Ok(())
        })();
        let _ = MFShutdown();
        if com {
            CoUninitialize();
        }
        result?;
    }
    std::fs::rename(&target, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("discord-clip-audio-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// 0.5 s of a different tone on each channel.
    fn tones(channels: u16) -> Audio {
        let frames = 24_000;
        let mut samples = Vec::with_capacity(frames * channels as usize);
        for i in 0..frames {
            for c in 0..channels {
                let f = 220.0 * (c + 1) as f32;
                samples.push(0.4 * (2.0 * std::f32::consts::PI * f * i as f32 / 48_000.0).sin());
            }
        }
        Audio { samples, channels, rate: 48_000 }
    }

    /// Correlation of channel `c` of `a` and `b` (1 = same signal).
    fn similarity(a: &Audio, b: &Audio, c: usize) -> f32 {
        let ch = a.channels as usize;
        let (mut ab, mut aa, mut bb) = (0f64, 0f64, 0f64);
        for f in 0..a.frames().min(b.frames()) {
            let (x, y) = (a.samples[f * ch + c] as f64, b.samples[f * ch + c] as f64);
            ab += x * y;
            aa += x * x;
            bb += y * y;
        }
        (ab / (aa.sqrt() * bb.sqrt()).max(1e-9)) as f32
    }

    #[test]
    fn formats_have_keys_and_extensions() {
        for f in Format::ALL {
            assert_eq!(Format::from_key(f.key()), Some(f));
        }
        assert_eq!(Format::from_key(" FLAC "), Some(Format::Flac));
        assert_eq!(Format::from_key("aac"), None);
        assert_eq!(Format::of_path(Path::new(r"C:\x\a b.OGG")), Some(Format::Opus));
        assert_eq!(Format::of_path(Path::new("a.m4a")), None);
        assert!(!Format::Mp3Low.dual_track() && Format::Opus.dual_track() && Format::Flac.dual_track());
    }

    #[test]
    fn self_delimited_lengths() {
        let mut v = Vec::new();
        put_length(&mut v, 251);
        put_length(&mut v, 252);
        put_length(&mut v, 1275);
        // 1275 = 255 + 4*255
        assert_eq!(v, vec![251, 252, 0, 255, 255]);
    }

    #[test]
    fn wav_and_flac_round_trip_exactly() {
        let dir = test_dir("lossless");
        for n in [2u16, 4] {
            let audio = tones(n);
            for format in [Format::Wav, Format::Flac] {
                let path = dir.join(format!("clip{n}.{}", format.extension()));
                write(&path, &audio, format).unwrap();
                assert_eq!(channels(&path).unwrap(), n, "{format:?}");
                let back = read(&path).unwrap();
                assert_eq!((back.channels, back.rate, back.frames()), (n, 48_000, audio.frames()));
                // 16-bit: exact to within one step.
                assert!(back.samples.iter().zip(&audio.samples).all(|(a, b)| (a - b).abs() < 1.0 / 16_000.0), "{format:?}");
            }
        }
        // FLAC is smaller than WAV.
        let size = |e: &str| std::fs::metadata(dir.join(format!("clip2.{e}"))).unwrap().len();
        assert!(size("flac") < size("wav") / 2, "{} vs {}", size("flac"), size("wav"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn opus_round_trip_keeps_length_and_tracks() {
        let dir = test_dir("opus");
        for n in [1u16, 2, 4] {
            let audio = tones(n);
            let path = dir.join(format!("clip{n}.ogg"));
            write(&path, &audio, Format::Opus).unwrap();
            let expected_channels = n.max(2);
            assert_eq!(channels(&path).unwrap(), expected_channels);
            let back = read(&path).unwrap();
            assert_eq!(back.channels, expected_channels);
            assert_eq!(back.frames(), audio.frames(), "exact length ({n} ch)");
            if n != 1 {
                // Each channel still carries its own tone (tracks don't swap or mix).
                for c in 0..n as usize {
                    let s = similarity(&audio, &back, c);
                    assert!(s > 0.95, "channel {c} of {n}: {s}");
                }
            }
            // ~64 kbps per stereo stream.
            let kbps = std::fs::metadata(&path).unwrap().len() as f32 * 8.0 / 0.5 / 1000.0;
            let streams = if n == 4 { 2.0 } else { 1.0 };
            assert!(kbps < 90.0 * streams, "{kbps} kbps");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn mp3_round_trip() {
        let dir = test_dir("mp3");
        let audio = tones(2);
        let path = dir.join("clip.mp3");
        write(&path, &audio, Format::Mp3Low).unwrap();
        let files: Vec<_> = std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(files, vec![std::ffi::OsString::from("clip.mp3")], "no temporary files left");
        let back = read(&path).unwrap();
        assert_eq!((back.channels, back.rate), (2, 48_000));
        // The encoder adds a little delay and padding (~50 ms).
        assert!(back.frames() >= audio.frames() && back.frames() < audio.frames() + 4_800, "{}", back.frames());
        assert_eq!(Format::for_copy_of(&path, &back).unwrap(), Format::Mp3Low);
        assert!(write(&dir.join("quad.mp3"), &tones(4), Format::Mp3High).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reads_channel_counts_from_headers() {
        assert_eq!(header_channels(b"nonsense"), None);
    }
}
