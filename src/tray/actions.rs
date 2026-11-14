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
                // Clips with separate tracks are shared as a normal stereo mix.
                let shared = crate::library::shareable_copy(&path, crate::library::Tracks::Both, crate::volumes::Volumes::default());
                match shared.and_then(|p| crate::clip::copy_file_to_clipboard(&p)) {
                    Ok(()) => text.push_str("\nCopied: press Ctrl+V in Discord to send it."),
                    Err(e) => {
                        log!("clipboard copy failed: {e:#}");
                        text.push_str("\n(Couldn't copy it to the clipboard.)");
                    }
                }
            }
            text.push_str("\nClick to open it in Discord Clip.");
            *app.last_clip.borrow_mut() = Some(path);
            app.balloon.set(Balloon::ShowClip);
            notify(app, "Clip saved", &text, false);
            super::tray_update(app); // the tooltip's clip count (#48)
        }
        Err(e) => {
            beep(false);
            log!("failed to save clip: {e:#}");
            notify(app, "Couldn't save clip", &format!("{e:#}"), true);
        }
    }
}

/// Clicked the "Clip saved" notification: open the window on the Clips
/// page, pointing out the clip (#50). Show in Explorer is in its ⋯ menu.
pub(super) fn open_last_clip(app: &App) {
    let snap = super::window::snapshot(app);
    match app.last_clip.borrow().clone() {
        Some(path) => crate::ui::open_clip(snap, path),
        None => crate::ui::open_page(snap, crate::ui::Page::Clips),
    }
}

/// Opens Explorer with `path` selected.
pub(super) fn show_in_folder(path: &Path) {
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

