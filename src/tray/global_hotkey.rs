//! The global hotkey that saves a clip.

use windows::Win32::UI::Input::KeyboardAndMouse::{RegisterHotKey, UnregisterHotKey, HOT_KEY_MODIFIERS, MOD_NOREPEAT};

use super::notify_icon::{notify, tray_update};
use super::window::update_settings;
use super::App;
use crate::hotkey::Hotkey;
use crate::log;

/// Our id for RegisterHotKey; WM_HOTKEY carries it.
pub(super) const HOTKEY_ID: i32 = 1;

pub(super) fn try_register(app: &App, hk: Hotkey) -> bool {
    unsafe {
        RegisterHotKey(app.hwnd.get(), HOTKEY_ID, HOT_KEY_MODIFIERS(hk.mods) | MOD_NOREPEAT, hk.vk).is_ok()
    }
}

pub(super) fn register_hotkey(app: &App) {
    if app.hotkey_registered.get() {
        return;
    }
    let hk = app.cfg.borrow().hotkey;
    if try_register(app, hk) {
        app.hotkey_registered.set(true);
        log!("hotkey {hk} registered");
    } else {
        log!("hotkey {hk} is already in use by another app");
        notify(
            app,
            "Hotkey unavailable",
            &format!("{hk} is already used by another app.\nClick the tray icon to pick a different one."),
            true,
        );
    }
}

pub(super) fn unregister_hotkey(app: &App) {
    if app.hotkey_registered.replace(false) {
        unsafe {
            let _ = UnregisterHotKey(app.hwnd.get(), HOTKEY_ID);
        }
    }
}


pub(super) fn change_hotkey(app: &App, new: Hotkey) {
    let old = app.cfg.borrow().hotkey;
    if new == old {
        register_hotkey(app);
        return;
    }
    if let Err(e) = new.check_sensible() {
        *app.hotkey_error.borrow_mut() = Some(e);
        register_hotkey(app);
        return;
    }
    if !try_register(app, new) {
        log!("hotkey {new} is already in use by another app");
        *app.hotkey_error.borrow_mut() =
            Some(format!("{new} is already used by another app. Pick a different combination."));
        register_hotkey(app);
        return;
    }
    app.hotkey_registered.set(true);
    update_settings(app, |c| c.hotkey = new);
    log!("hotkey changed to {new}");
    tray_update(app);
}

