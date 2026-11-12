//! Things the user can trigger from the hotkey, menu or notifications.

use std::path::Path;

use windows::core::{w, HSTRING, PCWSTR};
use windows::Win32::UI::WindowsAndMessaging::{MB_ICONHAND, MB_OK, SW_SHOWNORMAL};
use windows::Win32::UI::Shell::ShellExecuteW;

use super::notify_icon::notify;
use super::{App, Balloon};
use crate::log;

pub(super) fn open_clips_folder(app: &App) {
    let dir = app.cfg.borrow().out_dir.clone();
    let _ = std::fs::create_dir_all(&dir);
    shell_open(&dir);
}

pub(super) fn save_clip(app: &App) {
    let cfg = app.cfg.borrow().clone();
    match crate::clip::save(&cfg, &app.discord_ring, app.mic_ring.as_deref()) {
        Ok(path) => {
            beep(true);
            log!("saved clip {}", path.display());
            let name = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
            let mut text = name;
            if cfg.clipboard {
                match crate::clip::copy_file_to_clipboard(&path) {
                    Ok(()) => text.push_str("\nCopied: press Ctrl+V in Discord to send it."),
                    Err(e) => {
                        log!("clipboard copy failed: {e:#}");
                        text.push_str("\n(Couldn't copy it to the clipboard.)");
                    }
                }
            }
            text.push_str("\nClick to show it in its folder.");
            *app.last_clip.borrow_mut() = Some(path);
            app.balloon.set(Balloon::ShowClip);
            notify(app, "Clip saved", &text, false);
        }
        Err(e) => {
            beep(false);
            log!("failed to save clip: {e:#}");
            notify(app, "Couldn't save clip", &format!("{e:#}"), true);
        }
    }
}

pub(super) fn show_last_clip(app: &App) {
    let Some(path) = app.last_clip.borrow().clone() else { return };
    let args = HSTRING::from(format!("/select,\"{}\"", path.display()));
    unsafe {
        ShellExecuteW(None, w!("open"), w!("explorer.exe"), &args, PCWSTR::null(), SW_SHOWNORMAL);
    }
}

pub(super) fn shell_open(path: &Path) {
    let p = HSTRING::from(path.as_os_str());
    unsafe {
        ShellExecuteW(None, w!("open"), &p, PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL);
    }
}

pub(super) fn beep(ok: bool) {
    use windows::Win32::System::Diagnostics::Debug::MessageBeep;
    unsafe {
        let _ = MessageBeep(if ok { MB_OK } else { MB_ICONHAND });
    }
}

