//! WASAPI capture streams:
//!
//! * Per-process loopback (Windows 10 2004+ / build 19041+): taps only the
//!   audio rendered by a given process tree, without touching the real output
//!   device - Discord keeps playing to your speakers exactly as before.
//! * Microphone: the one picked in Settings, or by default the default
//!   *communications* capture device, i.e. normally the same mic you talk
//!   into on Discord.
//!
//! Both streams are opened in one fixed format (`FORMAT`) with
//! AUTOCONVERTPCM, so the two ring buffers line up sample-for-sample and can
//! be mixed without resampling.

use std::mem::size_of;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::sync_channel;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use windows::core::{implement, w, Interface, PCWSTR};
use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0};
use windows::Win32::Media::Audio::{
    eCapture, eCommunications, ActivateAudioInterfaceAsync, IActivateAudioInterfaceAsyncOperation,
    IActivateAudioInterfaceCompletionHandler, IActivateAudioInterfaceCompletionHandler_Impl,
    IAudioCaptureClient, IAudioClient, IMMDeviceEnumerator, MMDeviceEnumerator,
    AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM,
    AUDCLNT_STREAMFLAGS_EVENTCALLBACK, AUDCLNT_STREAMFLAGS_LOOPBACK,
    AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY, WAVEFORMATEX,
};
use windows::core::PROPVARIANT;
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, IAgileObject, IAgileObject_Impl, CLSCTX_ALL,
    COINIT_MULTITHREADED,
};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};
use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject};

use crate::ringbuffer::RingBuffer;

/// The format every stream is captured in (and clips are mixed in).
pub const SAMPLE_RATE: u32 = 48_000;
pub const CHANNELS: u16 = 2;

/// Executable names of the Discord flavors we know about.
pub const DISCORD_EXES: &[&str] = &["Discord.exe", "DiscordPTB.exe", "DiscordCanary.exe"];

const WAVE_FORMAT_PCM: u16 = 1;
const WAVE_FORMAT_IEEE_FLOAT: u16 = 3;

// ---------------------------------------------------------------------------
// Process discovery
// ---------------------------------------------------------------------------

struct ProcInfo {
    pid: u32,
    parent: u32,
    exe: String,
}

fn list_processes() -> Vec<ProcInfo> {
    let mut out = Vec::new();
    unsafe {
        let Ok(snapshot) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else {
            return out;
        };
        let mut entry = PROCESSENTRY32W {
            dwSize: size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        if Process32FirstW(snapshot, &mut entry).is_ok() {
            loop {
                out.push(ProcInfo {
                    pid: entry.th32ProcessID,
                    parent: entry.th32ParentProcessID,
                    exe: wide_to_string(&entry.szExeFile),
                });
                if Process32NextW(snapshot, &mut entry).is_err() {
                    break;
                }
            }
        }
        let _ = CloseHandle(snapshot);
    }
    out
}

/// Finds the *root* process of a running Discord client.
///
/// Discord is Electron, so there are several `Discord.exe` processes; voice
/// audio is rendered by one of the children. Process loopback with
/// INCLUDE_TARGET_PROCESS_TREE needs the top of the tree to catch it, so we
/// pick the Discord process whose parent is not itself a Discord process.
pub fn find_discord() -> Option<(u32, String)> {
    let procs = list_processes();
    let is_discord = |exe: &str| DISCORD_EXES.iter().any(|d| exe.eq_ignore_ascii_case(d));
    let discord_pids: Vec<u32> = procs
        .iter()
        .filter(|p| is_discord(&p.exe))
        .map(|p| p.pid)
        .collect();
    procs
        .iter()
        .find(|p| is_discord(&p.exe) && !discord_pids.contains(&p.parent))
        .map(|p| (p.pid, p.exe.clone()))
}

pub fn process_alive(pid: u32) -> bool {
    list_processes().iter().any(|p| p.pid == pid)
}

fn wide_to_string(buf: &[u16]) -> String {
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..len])
}

// ---------------------------------------------------------------------------
// Capture stream
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub enum Source {
    /// Everything the given process tree renders.
    ProcessLoopback(u32),
    /// A microphone by endpoint id, or `None` for the default
    /// communications microphone.
    Microphone(Option<String>),
}

impl Source {
    /// Short name for thread names and the log.
    fn label(&self) -> &'static str {
        match self {
            Source::ProcessLoopback(_) => "discord",
            Source::Microphone(_) => "mic",
        }
    }
}

/// A capture device for the microphone picker.
#[derive(Clone, Debug, PartialEq)]
pub struct Mic {
    /// Endpoint id: stable across restarts and renames; stored in config.
    pub id: String,
    /// "Microphone (Blue Yeti)"
    pub name: String,
    /// Currently the default communications microphone.
    pub is_default: bool,
}

/// The active capture devices, default communications mic first.
pub fn list_microphones() -> Result<Vec<Mic>> {
    use windows::Win32::Media::Audio::DEVICE_STATE_ACTIVE;
    use windows::Win32::System::Com::{CoTaskMemFree, STGM_READ};
    use windows::Win32::UI::Shell::PropertiesSystem::PROPERTYKEY;
    // PKEY_Device_FriendlyName (from functiondiscoverykeys_devpkey.h).
    const FRIENDLY_NAME: PROPERTYKEY = PROPERTYKEY {
        fmtid: windows::core::GUID::from_u128(0xa45c254e_df1c_4efd_8020_67d146a850e0),
        pid: 14,
    };
    unsafe {
        let enumerator: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        let id_of = |device: &windows::Win32::Media::Audio::IMMDevice| -> Result<String> {
            let p = device.GetId()?;
            let s = p.to_string();
            CoTaskMemFree(Some(p.0 as *const _));
            Ok(s?)
        };
        let default_id = enumerator
            .GetDefaultAudioEndpoint(eCapture, eCommunications)
            .ok()
            .and_then(|d| id_of(&d).ok());
        let devices = enumerator.EnumAudioEndpoints(eCapture, DEVICE_STATE_ACTIVE)?;
        let mut mics = Vec::new();
        for i in 0..devices.GetCount()? {
            let device = devices.Item(i)?;
            let id = id_of(&device)?;
            let name = device
                .OpenPropertyStore(STGM_READ)
                .and_then(|store| store.GetValue(&FRIENDLY_NAME))
                .map(|v| v.to_string())
                .ok()
                .filter(|n| !n.is_empty())
                .unwrap_or_else(|| "Unknown microphone".to_string());
            mics.push(Mic { is_default: default_id.as_deref() == Some(id.as_str()), id, name });
        }
        mics.sort_by_key(|m| !m.is_default);
        Ok(mics)
    }
}

/// Initializes COM (MTA) for the current thread. Safe to call repeatedly.
pub fn init_com() {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }
}

struct Stream {
    audio_client: IAudioClient,
    capture_client: IAudioCaptureClient,
    event: HANDLE,
    float: bool,
}

impl Drop for Stream {
    fn drop(&mut self) {
        unsafe {
            let _ = self.audio_client.Stop();
            let _ = CloseHandle(self.event);
        }
    }
}

/// Spawns a thread that opens `source` and keeps `ring` filled until
/// `running` turns false. Returns once the stream has started (or failed to).
///
/// The COM objects are created on the capture thread itself, so nothing
/// thread-affine ever crosses threads.
pub fn spawn(
    source: Source,
    ring: Arc<RingBuffer>,
    running: Arc<AtomicBool>,
) -> Result<std::thread::JoinHandle<()>> {
    let (tx, rx) = sync_channel::<Result<()>>(1);
    let label = source.label();
    let handle = std::thread::Builder::new()
        .name(format!("capture-{label}"))
        .spawn(move || {
            init_com();
            let stream = match open(&source) {
                Ok(s) => {
                    let _ = tx.send(Ok(()));
                    s
                }
                Err(e) => {
                    let _ = tx.send(Err(e));
                    return;
                }
            };
            if let Err(e) = run(&stream, &ring, &running) {
                crate::log!("[{label}] capture stopped: {e:#}");
            }
        })?;
    rx.recv()
        .map_err(|_| anyhow!("capture thread exited unexpectedly"))??;
    Ok(handle)
}

fn open(source: &Source) -> Result<Stream> {
    let audio_client = match source {
        Source::ProcessLoopback(pid) => activate_process_loopback(*pid)?,
        Source::Microphone(id) => unsafe {
            let enumerator: IMMDeviceEnumerator =
                CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
            let device = match id {
                Some(id) => {
                    // A known but unplugged device is still found; it just
                    // isn't active (and would record silence).
                    let device = enumerator
                        .GetDevice(&windows::core::HSTRING::from(id.as_str()))
                        .context("the selected microphone isn't connected")?;
                    if device.GetState()? != windows::Win32::Media::Audio::DEVICE_STATE_ACTIVE {
                        return Err(anyhow!("the selected microphone isn't connected"));
                    }
                    device
                }
                None => enumerator
                    .GetDefaultAudioEndpoint(eCapture, eCommunications)
                    .context("no default communications microphone")?,
            };
            device.Activate::<IAudioClient>(CLSCTX_ALL, None)?
        },
    };

    let mut flags = AUDCLNT_STREAMFLAGS_EVENTCALLBACK
        | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM
        | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY;
    if matches!(source, Source::ProcessLoopback(_)) {
        flags |= AUDCLNT_STREAMFLAGS_LOOPBACK;
    }

    // 100ms engine buffer, polled via event. Prefer float; fall back to
    // 16-bit PCM, which is what Microsoft's process-loopback sample uses.
    const BUFFER_100NS: i64 = 1_000_000;
    let mut float = true;
    let init = |fmt: &WAVEFORMATEX| unsafe {
        audio_client.Initialize(AUDCLNT_SHAREMODE_SHARED, flags, BUFFER_100NS, 0, fmt, None)
    };
    if let Err(e) = init(&wave_format(true)) {
        float = false;
        init(&wave_format(false)).with_context(|| {
            format!("IAudioClient::Initialize failed (float attempt: {e})")
        })?;
    }

    let event = unsafe { CreateEventW(None, false, false, PCWSTR::null())? };
    unsafe {
        audio_client.SetEventHandle(event)?;
    }
    let capture_client: IAudioCaptureClient = unsafe { audio_client.GetService()? };
    unsafe { audio_client.Start()? };

    Ok(Stream {
        audio_client,
        capture_client,
        event,
        float,
    })
}

fn wave_format(float: bool) -> WAVEFORMATEX {
    let bits: u16 = if float { 32 } else { 16 };
    let block_align = CHANNELS * bits / 8;
    WAVEFORMATEX {
        wFormatTag: if float { WAVE_FORMAT_IEEE_FLOAT } else { WAVE_FORMAT_PCM },
        nChannels: CHANNELS,
        nSamplesPerSec: SAMPLE_RATE,
        nAvgBytesPerSec: SAMPLE_RATE * block_align as u32,
        nBlockAlign: block_align,
        wBitsPerSample: bits,
        cbSize: 0,
    }
}

/// Capture loop.
///
/// Neither process loopback nor some mic drivers deliver packets while
/// nothing is playing, so we keep the buffer anchored to wall-clock time:
/// whenever we fall behind real time by more than `GAP_SLACK`, the missing
/// span is filled with silence. That way "last 30 seconds" really means the
/// last 30 seconds, quiet stretches included.
fn run(stream: &Stream, ring: &RingBuffer, running: &AtomicBool) -> Result<()> {
    let channels = CHANNELS as usize;
    let rate = SAMPLE_RATE as f64;
    let gap_slack = (SAMPLE_RATE / 10) as u64; // 100ms
    let start = Instant::now();
    let mut frames_written: u64 = 0;
    let mut scratch: Vec<f32> = Vec::new();

    while running.load(Ordering::Relaxed) {
        let woke = unsafe { WaitForSingleObject(stream.event, 100) } == WAIT_OBJECT_0;

        if woke {
            loop {
                let packet = unsafe { stream.capture_client.GetNextPacketSize()? };
                if packet == 0 {
                    break;
                }
                let mut data: *mut u8 = std::ptr::null_mut();
                let mut frames: u32 = 0;
                let mut flags: u32 = 0;
                unsafe {
                    stream
                        .capture_client
                        .GetBuffer(&mut data, &mut frames, &mut flags, None, None)?;
                }
                let n = frames as usize * channels;
                let silent = flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0;

                if silent || data.is_null() {
                    ring.push_silence(n);
                } else if stream.float {
                    ring.push(unsafe { std::slice::from_raw_parts(data as *const f32, n) });
                } else {
                    let src = unsafe { std::slice::from_raw_parts(data as *const i16, n) };
                    scratch.clear();
                    scratch.extend(src.iter().map(|&s| s as f32 / 32768.0));
                    ring.push(&scratch);
                }
                unsafe { stream.capture_client.ReleaseBuffer(frames)? };
                frames_written += frames as u64;
            }
        }

        let expected = (start.elapsed().as_secs_f64() * rate) as u64;
        if frames_written > expected {
            // Device clock slightly fast vs. ours - re-anchor so it doesn't
            // eat into a future gap.
            frames_written = expected;
        } else if expected - frames_written > gap_slack {
            let missing = expected - frames_written;
            ring.push_silence(missing as usize * channels);
            frames_written = expected;
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Process-loopback activation
// ---------------------------------------------------------------------------

#[repr(C)]
struct AudioclientProcessLoopbackParams {
    target_process_id: u32,
    process_loopback_mode: u32,
}

#[repr(C)]
struct AudioclientActivationParams {
    activation_type: u32,
    loopback: AudioclientProcessLoopbackParams,
}

const AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK: u32 = 1;
const PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE: u32 = 0;
const VT_BLOB: u16 = 65;

/// PROPVARIANT laid out as VT_BLOB, matching the C struct on x64.
#[repr(C)]
struct PropvariantBlob {
    vt: u16,
    reserved: [u16; 3],
    cb_size: u32,
    blob_data: *const u8,
}

/// Raw COM pointer wrapper so the activation result can cross threads. Audio
/// client objects are free-threaded and we are in the MTA on both ends.
struct SendClient(windows::core::Result<IAudioClient>);
unsafe impl Send for SendClient {}

// The completion callback is invoked on an arbitrary MTA worker thread, so
// the handler must be agile or activation fails.
#[implement(IActivateAudioInterfaceCompletionHandler, IAgileObject)]
struct ActivationHandler {
    sender: Mutex<Option<std::sync::mpsc::SyncSender<SendClient>>>,
}

impl IAgileObject_Impl for ActivationHandler_Impl {}

impl IActivateAudioInterfaceCompletionHandler_Impl for ActivationHandler_Impl {
    fn ActivateCompleted(
        &self,
        op: Option<&IActivateAudioInterfaceAsyncOperation>,
    ) -> windows::core::Result<()> {
        let result = (|| -> windows::core::Result<IAudioClient> {
            let op = op.ok_or_else(|| windows::core::Error::from(windows::Win32::Foundation::E_POINTER))?;
            let mut hr = windows::Win32::Foundation::S_OK;
            let mut iface: Option<windows::core::IUnknown> = None;
            unsafe { op.GetActivateResult(&mut hr, &mut iface)? };
            hr.ok()?;
            iface
                .ok_or_else(|| windows::core::Error::from(windows::Win32::Foundation::E_POINTER))?
                .cast::<IAudioClient>()
        })();
        if let Some(tx) = self.sender.lock().unwrap().take() {
            let _ = tx.send(SendClient(result));
        }
        Ok(())
    }
}

fn activate_process_loopback(pid: u32) -> Result<IAudioClient> {
    let params = AudioclientActivationParams {
        activation_type: AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK,
        loopback: AudioclientProcessLoopbackParams {
            target_process_id: pid,
            process_loopback_mode: PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE,
        },
    };
    let blob = PropvariantBlob {
        vt: VT_BLOB,
        reserved: [0; 3],
        cb_size: size_of::<AudioclientActivationParams>() as u32,
        blob_data: &params as *const _ as *const u8,
    };
    const _: () = assert!(size_of::<PropvariantBlob>() <= size_of::<PROPVARIANT>());
    // Copy into a real PROPVARIANT. The blob points at stack memory, so it
    // must never be PropVariantClear'ed - and windows-core's PROPVARIANT does
    // exactly that in its Drop impl (-> heap corruption), hence ManuallyDrop.
    let mut prop = std::mem::ManuallyDrop::new(unsafe { std::mem::zeroed::<PROPVARIANT>() });
    unsafe {
        std::ptr::write(&mut *prop as *mut PROPVARIANT as *mut PropvariantBlob, blob);
    }

    let (tx, rx) = sync_channel::<SendClient>(1);
    let handler: IActivateAudioInterfaceCompletionHandler = ActivationHandler {
        sender: Mutex::new(Some(tx)),
    }
    .into();

    let device_id: PCWSTR = w!("VAD\\Process_Loopback");
    let _op = unsafe {
        ActivateAudioInterfaceAsync(device_id, &IAudioClient::IID, Some(&*prop as *const PROPVARIANT), &handler)?
    };

    let SendClient(result) = rx
        .recv_timeout(Duration::from_secs(5))
        .map_err(|_| anyhow!("timed out waiting for audio interface activation"))?;
    // Keep `params` alive until activation is done, to be safe.
    let _ = &params;
    result.context("process-loopback activation failed (needs Windows 10 2004 / build 19041+)")
}

/// What the Discord supervisor is currently doing (shown in the tray).
#[derive(Clone, Debug)]
pub enum DiscordStatus {
    Waiting,
    Capturing { pid: u32, exe: String },
    Failed(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Needs real audio devices: `cargo test -- --ignored microphones --nocapture`.
    #[test]
    #[ignore]
    fn lists_microphones() {
        init_com();
        let mics = list_microphones().unwrap();
        for m in &mics {
            println!("{}{} [{}]", if m.is_default { "* " } else { "  " }, m.name, m.id);
        }
        assert!(!mics.is_empty(), "no active microphones");
        assert!(mics[0].is_default, "default communications mic should be listed first");
        // Every listed mic can be opened by id.
        for m in &mics {
            open(&Source::Microphone(Some(m.id.clone()))).unwrap_or_else(|e| panic!("{}: {e:#}", m.name));
        }
        // An id that doesn't exist fails cleanly (the app then falls back).
        assert!(open(&Source::Microphone(Some("{0.0.1.00000000}.{00000000-0000-0000-0000-000000000000}".into()))).is_err());
    }
}
