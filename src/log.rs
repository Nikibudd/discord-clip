//! Tiny logger: timestamped lines to `%LOCALAPPDATA%\discord-clip\discord-clip.log`
//! and to stderr (visible when started from a terminal).

use std::fs::File;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

static FILE: Mutex<Option<File>> = Mutex::new(None);

pub fn log_path() -> PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join("discord-clip")
        .join("discord-clip.log")
}

/// Starts a fresh log file (the previous run's is kept as `.old.log`).
pub fn init() {
    let path = log_path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::rename(&path, path.with_extension("old.log"));
    *FILE.lock().unwrap() = File::create(&path).ok();
}

pub fn write(args: std::fmt::Arguments) {
    let t = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() };
    let line = format!("[{:02}:{:02}:{:02}] {args}", t.wHour, t.wMinute, t.wSecond);
    eprintln!("{line}");
    if let Some(f) = FILE.lock().unwrap().as_mut() {
        let _ = writeln!(f, "{line}");
    }
}

#[macro_export]
macro_rules! log {
    ($($t:tt)*) => { $crate::log::write(format_args!($($t)*)) };
}
