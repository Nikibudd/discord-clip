//! discord-clip: keeps a rolling buffer of the last N seconds of Discord voice
//! audio and saves it to a WAV file when you press a hotkey. Lives in the
//! system tray.
//!
//! * Other people's voices: per-process WASAPI loopback of the Discord client
//!   (Windows 10 2004+). Discord keeps playing to your speakers as normal -
//!   this only taps the stream, so game/music audio is NOT included.
//! * Your voice: the default communications microphone, mixed in.
//!
//! The saved clip is also copied to the clipboard as a file, so you can paste
//! it straight into a Discord chat with Ctrl+V.

#![cfg_attr(not(test), windows_subsystem = "windows")]

mod capture;
mod clip;
mod config;
mod hotkey;
mod log;
mod ringbuffer;
mod tray;
mod ui;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use capture::{DiscordStatus, Source};
use config::Config;
use ringbuffer::RingBuffer;
use windows::core::{w, HSTRING};
use windows::Win32::Foundation::{CloseHandle, GetLastError, ERROR_ALREADY_EXISTS, HANDLE};
use windows::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONERROR, MB_ICONINFORMATION, MB_OK};

const USAGE: &str = "\
Usage: discord-clip [options]

Runs in the system tray. Settings are read from
%APPDATA%\\discord-clip\\config.ini; these options override them for one run:

  --hotkey <KEYS>      Hotkey that saves a clip, e.g. Ctrl+Alt+C or Ctrl+Shift+F9
  -s, --seconds <N>    Length of each clip in seconds (1-300)
      --no-mic         Don't record your microphone, only other people
      --mic-gain <X>   Microphone volume multiplier (0-10)
  -o, --out <DIR>      Folder to save clips in
      --no-clipboard   Don't copy saved clips to the clipboard
      --no-notifications  Don't show a notification for each saved clip
      --settings       Open the settings window on start
  -h, --help           Show this help";

/// Returns `(setting, value)` overrides from the command line, and whether
/// `--settings` was given.
fn parse_cli() -> Result<(Vec<(&'static str, String)>, bool)> {
    let mut out = Vec::new();
    let mut open_window = false;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut value = |name: &str| args.next().with_context(|| format!("{name} needs a value"));
        let pair = match arg.as_str() {
            "--hotkey" => ("hotkey", value(&arg)?),
            "-s" | "--seconds" => ("seconds", value(&arg)?),
            "--no-mic" => ("mic", "false".into()),
            "--mic-gain" => ("mic_gain", value(&arg)?),
            "-o" | "--out" => {
                // Relative paths are relative to where you ran the command.
                let p = std::path::absolute(value(&arg)?).context("bad --out path")?;
                ("out_dir", p.display().to_string())
            }
            "--no-clipboard" => ("clipboard", "false".into()),
            "--no-notifications" => ("notifications", "false".into()),
            "--settings" => {
                open_window = true;
                continue;
            }
            "-h" | "--help" => {
                show_text(USAGE, false);
                std::process::exit(0);
            }
            other => bail!("unknown argument '{other}'\n\n{USAGE}"),
        };
        out.push(pair);
    }
    Ok((out, open_window))
}

/// Prints to the console we were started from, if any; otherwise shows a
/// message box (a tray app has no console of its own).
fn show_text(text: &str, error: bool) {
    if has_parent_console() {
        if error {
            eprintln!("{text}");
        } else {
            println!("{text}");
        }
    } else {
        let icon = if error { MB_ICONERROR } else { MB_ICONINFORMATION };
        unsafe {
            MessageBoxW(None, &HSTRING::from(text), w!("Discord Clip"), icon | MB_OK);
        }
    }
}

fn has_parent_console() -> bool {
    use std::sync::OnceLock;
    use windows::Win32::System::Console::{AttachConsole, ATTACH_PARENT_PROCESS};
    static ATTACHED: OnceLock<bool> = OnceLock::new();
    *ATTACHED.get_or_init(|| unsafe { AttachConsole(ATTACH_PARENT_PROCESS).is_ok() })
}

fn fatal(text: &str) -> ! {
    log!("fatal: {text}");
    show_text(text, true);
    std::process::exit(1);
}

/// Returns None if another instance is already running.
fn single_instance() -> Option<HANDLE> {
    use windows::Win32::System::Threading::CreateMutexW;
    unsafe {
        let h = CreateMutexW(None, false, w!("Local\\discord-clip-single-instance")).ok()?;
        if GetLastError() == ERROR_ALREADY_EXISTS {
            let _ = CloseHandle(h);
            return None;
        }
        Some(h)
    }
}

fn main() {
    has_parent_console(); // attach early so --help / errors reach the terminal
    let (overrides, open_window) = match parse_cli() {
        Ok(o) => o,
        Err(e) => {
            show_text(&format!("{e:#}"), true);
            std::process::exit(2);
        }
    };

    unsafe {
        use windows::Win32::UI::HiDpi::{SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_SYSTEM_AWARE};
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_SYSTEM_AWARE);
        // The UI thread must be STA (folder picker); capture threads use MTA.
        use windows::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
    }

    let Some(instance) = single_instance() else {
        show_text(
            "Discord Clip is already running.\n\nLook for its icon in the system tray \
             (you may need to click the ^ arrow next to the clock).",
            false,
        );
        return;
    };

    log::init();
    log!("discord-clip {} starting", env!("CARGO_PKG_VERSION"));

    let cfg_path = config::config_path();
    let (file_cfg, mut warnings) = Config::load_or_create(&cfg_path);
    for w in &warnings {
        log!("config: {w}");
    }
    let mut cfg = file_cfg.clone();
    for (k, v) in &overrides {
        if let Err(e) = cfg.set(k, v) {
            fatal(&format!("Invalid command-line option for {k}: {e:#}"));
        }
    }
    log!("settings: {} | hotkey {} | {}s | mic {} | clips -> {}", cfg_path.display(), cfg.hotkey, cfg.seconds, cfg.mic, cfg.out_dir.display());
    if let Err(e) = std::fs::create_dir_all(&cfg.out_dir) {
        let msg = format!("Can't create clips folder {}: {e}", cfg.out_dir.display());
        log!("{msg}");
        warnings.push(msg);
    }

    let ring_len = capture::SAMPLE_RATE as usize * capture::CHANNELS as usize * cfg.seconds as usize;
    let running = Arc::new(AtomicBool::new(true));
    let discord_ring = Arc::new(RingBuffer::new(ring_len));

    let (mic_ring, mic_status) = if cfg.mic {
        let ring = Arc::new(RingBuffer::new(ring_len));
        match capture::spawn(Source::Microphone, Arc::clone(&ring), Arc::clone(&running)) {
            Ok(_) => {
                log!("microphone: capturing default communications device");
                (Some(ring), "on".to_string())
            }
            Err(e) => {
                log!("microphone unavailable: {e:#}");
                (None, "unavailable".to_string())
            }
        }
    } else {
        (None, "off".to_string())
    };

    let discord_status = Arc::new(Mutex::new(DiscordStatus::Waiting));
    let supervisor = {
        let ring = Arc::clone(&discord_ring);
        let running = Arc::clone(&running);
        let status = Arc::clone(&discord_status);
        std::thread::Builder::new()
            .name("discord-supervisor".into())
            .spawn(move || supervise_discord(ring, running, status))
            .expect("spawn supervisor")
    };

    let app = tray::App::new(
        cfg,
        file_cfg,
        discord_ring,
        mic_ring,
        mic_status,
        discord_status,
        Arc::clone(&running),
        warnings,
        open_window,
    );
    let exit = match tray::run(app) {
        Ok(e) => e,
        Err(e) => fatal(&format!("Couldn't create the tray icon: {e:#}")),
    };

    running.store(false, Ordering::SeqCst);
    let _ = supervisor.join();
    unsafe {
        let _ = CloseHandle(instance);
    }

    if exit == tray::Exit::Restart {
        // Same command-line overrides, and bring the settings window back.
        let exe = std::env::current_exe().expect("current exe");
        let args = std::env::args_os().skip(1).filter(|a| a != "--settings");
        if let Err(e) = std::process::Command::new(exe).args(args).arg("--settings").spawn() {
            fatal(&format!("Couldn't restart: {e}"));
        }
    }
    log!("bye");
}

/// Keeps a process-loopback stream attached to Discord: waits for Discord to
/// start, and re-attaches if it is restarted or updated (new PID).
fn supervise_discord(ring: Arc<RingBuffer>, running: Arc<AtomicBool>, status: Arc<Mutex<DiscordStatus>>) {
    capture::init_com();
    let set_status = |s: DiscordStatus| {
        *status.lock().unwrap() = s;
        tray::post_status_changed();
    };
    let mut waiting_logged = false;
    // (pid, per-stream run flag)
    let mut current: Option<(u32, Arc<AtomicBool>)> = None;

    while running.load(Ordering::SeqCst) {
        let alive = current.as_ref().is_some_and(|(pid, _)| capture::process_alive(*pid));
        if !alive {
            if let Some((pid, stream_flag)) = current.take() {
                log!("Discord (pid {pid}) exited - waiting for it to come back");
                stream_flag.store(false, Ordering::SeqCst);
                set_status(DiscordStatus::Waiting);
            }
            match capture::find_discord() {
                Some((pid, exe)) => {
                    let stream_flag = Arc::new(AtomicBool::new(true));
                    match capture::spawn(Source::ProcessLoopback(pid), Arc::clone(&ring), Arc::clone(&stream_flag)) {
                        Ok(_) => {
                            log!("Discord: capturing {exe} (pid {pid})");
                            waiting_logged = false;
                            current = Some((pid, stream_flag));
                            set_status(DiscordStatus::Capturing { pid, exe });
                        }
                        Err(e) => {
                            log!("Discord: failed to start capture for pid {pid}: {e:#}");
                            set_status(DiscordStatus::Failed(format!("{e:#}")));
                            sleep_while(&running, Duration::from_secs(10));
                            continue;
                        }
                    }
                }
                None if !waiting_logged => {
                    log!("Discord: not running - waiting for it to start");
                    waiting_logged = true;
                    set_status(DiscordStatus::Waiting);
                }
                None => {}
            }
        }
        sleep_while(&running, Duration::from_secs(2));
    }
    if let Some((_, stream_flag)) = current {
        stream_flag.store(false, Ordering::SeqCst);
    }
}

fn sleep_while(running: &AtomicBool, total: Duration) {
    let step = Duration::from_millis(100);
    let mut slept = Duration::ZERO;
    while slept < total && running.load(Ordering::SeqCst) {
        std::thread::sleep(step);
        slept += step;
    }
}
