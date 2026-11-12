//! The right-click menu.

use windows::core::{HSTRING, PCWSTR};
use windows::Win32::Foundation::{LPARAM, POINT, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::*;

use super::actions::{open_clips_folder, save_clip, shell_open};
use super::notify_icon::discord_line;
use super::window::snapshot;
use super::{quit, App, Exit};
use crate::{log, ui};

const ID_SAVE: usize = 100;
const ID_OPEN_FOLDER: usize = 101;
const ID_WINDOW: usize = 102;
const ID_LOG: usize = 106;
const ID_QUIT: usize = 107;

pub(super) fn show_menu(app: &App) {
    let (discord, _) = discord_line(app);
    let hotkey = app.cfg.borrow().hotkey.to_string();
    unsafe {
        let Ok(menu) = CreatePopupMenu() else { return };
        let item = |flags: MENU_ITEM_FLAGS, id: usize, text: &str| {
            let _ = AppendMenuW(menu, flags, id, &HSTRING::from(text));
        };
        let sep = || {
            let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        };
        item(MF_STRING | MF_GRAYED, 0, &discord);
        item(MF_STRING | MF_GRAYED, 0, &format!("Microphone: {}", app.mic_status));
        sep();
        item(MF_STRING, ID_WINDOW, "Open Discord Clip");
        sep();
        item(MF_STRING, ID_SAVE, &format!("Save clip now\t{hotkey}"));
        item(MF_STRING, ID_OPEN_FOLDER, "Open clips folder");
        item(MF_STRING, ID_LOG, "Open log");
        sep();
        item(MF_STRING, ID_QUIT, "Quit");
        let _ = SetMenuDefaultItem(menu, ID_WINDOW as u32, 0);

        let mut pt = POINT::default();
        let _ = GetCursorPos(&mut pt);
        let hwnd = app.hwnd.get();
        // Required so the menu closes when clicking elsewhere.
        let _ = SetForegroundWindow(hwnd);
        let cmd = TrackPopupMenu(
            menu,
            TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_NONOTIFY | TPM_BOTTOMALIGN,
            pt.x,
            pt.y,
            0,
            hwnd,
            None,
        )
        .0 as usize;
        let _ = PostMessageW(hwnd, WM_NULL, WPARAM(0), LPARAM(0));
        let _ = DestroyMenu(menu);
        handle_command(app, cmd);
    }
}

pub(super) fn handle_command(app: &App, cmd: usize) {
    match cmd {
        ID_WINDOW => ui::open(snapshot(app)),
        ID_SAVE => save_clip(app),
        ID_OPEN_FOLDER => open_clips_folder(app),
        ID_LOG => shell_open(&log::log_path()),
        ID_QUIT => {
            log!("quit requested");
            quit(app, Exit::Quit);
        }
        _ => {}
    }
}

