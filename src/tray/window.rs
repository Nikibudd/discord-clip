//! The tray side of the window: builds the [`Snapshot`] it shows, carries out
//! its [`Command`]s, and hands over to downloaded updates.

use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;

use windows::core::{w, HSTRING};
use windows::Win32::Foundation::{ERROR_CANCELLED, HWND};
use windows::Win32::System::Com::{CoCreateInstance, CoTaskMemFree, CLSCTX_INPROC_SERVER};
use windows::Win32::UI::Shell::{
    FileOpenDialog, IFileOpenDialog, IShellItem, SHCreateItemFromParsingName, FOS_FORCEFILESYSTEM, FOS_PATHMUSTEXIST,
    FOS_PICKFOLDERS, SIGDN_FILESYSPATH,
};

use super::actions::{open_clips_folder, shell_open, show_in_folder};
use super::global_hotkey::{change_hotkey, unregister_hotkey, register_hotkey};
use super::notify_icon::{discord_line, notify, notify_always};
use super::{quit, App, Balloon, Exit};
use crate::config::{self, Config};
use crate::ui::{self, Command, Snapshot};
use crate::{log, update};

pub(super) fn snapshot(app: &App) -> Snapshot {
    let (discord, discord_active) = discord_line(app);
    let cfg = app.cfg.borrow().clone();
    Snapshot {
        restart_needed: cfg.seconds != app.running_seconds
            || cfg.mic != app.running_mic
            || cfg.mic_device != app.running_mic_device,
        mics: app.mics.borrow().clone(),
        cfg,
        discord: discord.trim_start_matches("Discord: ").to_string(),
        discord_active,
        mic: app.mic_status.clone(),
        hotkey_registered: app.hotkey_registered.get(),
        hotkey_error: app.hotkey_error.borrow().clone(),
        update: update::status(),
        installed: update::is_installed(),
        autostart: crate::autostart::state(),
    }
}

/// Once per new version: "update available" notification. Clicking it opens
/// the Updates page.
pub(super) fn notify_update_available(app: &App) {
    let update::State::Available(m) = update::status().state else { return };
    if app.notified_update.borrow().as_deref() == Some(m.version.as_str()) {
        return;
    }
    *app.notified_update.borrow_mut() = Some(m.version.clone());
    app.balloon.set(Balloon::OpenUpdates);
    notify_always(
        app,
        &format!("Discord Clip {} is available", m.version),
        &format!("You have {}. Click to update.", update::CURRENT),
    );
}

/// A downloaded update is verified: hand over to it and quit.
pub(super) fn install_update(app: &App) {
    match update::apply() {
        Ok(update::Applied::Installer) => quit(app, Exit::Quit),
        Ok(update::Applied::Replaced) => quit(app, Exit::Restart),
        Err(e) => notify(app, "Couldn't install the update", &format!("{e:#}"), true),
    }
}

pub(super) fn handle_ui_command(app: &App, cmd: Command) {
    match cmd {
        Command::BeginHotkeyCapture => {
            // While picking, the current hotkey must not fire (or be swallowed).
            *app.hotkey_error.borrow_mut() = None;
            unregister_hotkey(app);
        }
        Command::EndHotkeyCapture(None) => register_hotkey(app),
        Command::EndHotkeyCapture(Some(new)) => change_hotkey(app, new),
        Command::SetSeconds(s) => match config::validate_seconds(s) {
            Ok(s) => {
                update_settings(app, |c| c.seconds = s);
                log!("clip length set to {s}s (applies after restart)");
            }
            Err(e) => log!("ignoring clip length {s}: {e:#}"),
        },
        Command::SetMic(on) => {
            update_settings(app, |c| c.mic = on);
            log!("microphone set to {on} (applies after restart)");
        }
        Command::SetMicDevice(id) => {
            log!("microphone device set to {:?} (applies after restart)", if id.is_empty() { "default" } else { &id });
            update_settings(app, |c| c.mic_device = id.clone());
        }
        Command::RefreshMics => refresh_mics(app),
        Command::SetMicGain(g) => update_settings(app, |c| c.mic_gain = g.clamp(0.0, 10.0)),
        Command::SetClipboard(on) => update_settings(app, |c| c.clipboard = on),
        Command::SetNotifications(on) => update_settings(app, |c| c.notifications = on),
        Command::SetSeparateTracks(on) => update_settings(app, |c| c.separate_tracks = on),
        Command::SetTheme(t) => update_settings(app, |c| c.theme = t),
        Command::SetFormat(f) => {
            update_settings(app, |c| c.format = f);
            log!("clip format set to {}", f.key());
        }
        Command::PickFolder => change_folder(app),
        Command::OpenFolder => open_clips_folder(app),
        Command::OpenLog => shell_open(&log::log_path()),
        Command::OpenConfigFile => {
            let path = config::config_path();
            if !path.exists() {
                if let Err(e) = app.file_cfg.borrow().save(&path) {
                    log!("couldn't create settings file: {e:#}");
                }
            }
            shell_open(&path);
        }
        Command::Restart => {
            log!("restarting to apply settings");
            quit(app, Exit::Restart);
        }
        Command::SetCheckUpdates(on) => {
            update_settings(app, |c| c.check_updates = on);
            update::AUTO.store(on, Ordering::SeqCst);
            if on && update::status().last_checked.is_none() {
                update::check_now();
            }
        }
        Command::CheckUpdates => update::check_now(),
        Command::InstallUpdate => update::start_download(),
        Command::OpenUrl(url) => {
            if url.starts_with("https://") {
                shell_open(Path::new(&url));
            }
        }
        Command::CopyClip(path, tracks) => {
            let shared = crate::library::shareable_copy(&path, tracks, crate::volumes::get(&path));
            if let Err(e) = shared.and_then(|p| crate::clip::copy_file_to_clipboard(&p)) {
                log!("clipboard copy failed: {e:#}");
                notify(app, "Couldn't copy the clip", &format!("{e:#}"), true);
            }
        }
        Command::ShowClip(path) => show_in_folder(&path),
        Command::SetAutostart(on) => match crate::autostart::set(on) {
            Ok(()) => log!("start with Windows: {on}"),
            Err(e) => {
                log!("{e:#}");
                notify(app, "Couldn't change start with Windows", &format!("{e:#}"), true);
            }
        },
        Command::DeleteClip(path) => match crate::library::recycle(&path) {
            Ok(()) => log!("moved {} to the Recycle Bin", path.display()),
            Err(e) => {
                log!("{e:#}");
                notify(app, "Couldn't delete the clip", &format!("{e:#}"), true);
            }
        },
    }
}


/// Applies a UI change to both the live and the on-disk settings.
pub(super) fn update_settings(app: &App, f: impl Fn(&mut Config)) {
    f(&mut app.cfg.borrow_mut());
    let mut file = app.file_cfg.borrow_mut();
    f(&mut file);
    if let Err(e) = file.save(&config::config_path()) {
        log!("couldn't save settings: {e:#}");
    }
}

// ---------------------------------------------------------------------------
// Clips folder picker
// ---------------------------------------------------------------------------

pub(super) fn change_folder(app: &App) {
    let current = app.cfg.borrow().out_dir.clone();
    let owner = ui::window_hwnd().unwrap_or(app.hwnd.get());
    match pick_folder(owner, &current) {
        Ok(Some(dir)) => {
            if let Err(e) = std::fs::create_dir_all(&dir) {
                notify(app, "Can't use that folder", &format!("{e}"), true);
                return;
            }
            update_settings(app, |c| c.out_dir = dir.clone());
            log!("clips folder changed to {}", dir.display());
            ui::publish(snapshot(app));
        }
        Ok(None) => {}
        Err(e) => {
            log!("folder picker failed: {e:#}");
            notify(app, "Couldn't open folder picker", &format!("{e:#}"), true);
        }
    }
}

pub(super) fn pick_folder(owner: HWND, current: &Path) -> anyhow::Result<Option<PathBuf>> {
    unsafe {
        let dialog: IFileOpenDialog = CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER)?;
        dialog.SetOptions(dialog.GetOptions()? | FOS_PICKFOLDERS | FOS_FORCEFILESYSTEM | FOS_PATHMUSTEXIST)?;
        dialog.SetTitle(w!("Choose where Discord Clip saves clips"))?;
        let _ = std::fs::create_dir_all(current);
        if let Ok(item) = SHCreateItemFromParsingName::<_, _, IShellItem>(&HSTRING::from(current.as_os_str()), None) {
            let _ = dialog.SetFolder(&item);
        }
        if let Err(e) = dialog.Show(owner) {
            if e.code() == ERROR_CANCELLED.to_hresult() {
                return Ok(None);
            }
            return Err(e.into());
        }
        let item = dialog.GetResult()?;
        let name = item.GetDisplayName(SIGDN_FILESYSPATH)?;
        let path = name.to_string();
        CoTaskMemFree(Some(name.0 as *const c_void));
        Ok(Some(PathBuf::from(path?)))
    }
}

/// Re-reads the capture devices for the microphone picker.
pub(super) fn refresh_mics(app: &App) {
    match crate::capture::list_microphones() {
        Ok(mics) => *app.mics.borrow_mut() = mics,
        Err(e) => log!("couldn't list microphones: {e:#}"),
    }
}
