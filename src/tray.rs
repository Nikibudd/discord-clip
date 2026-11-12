//! System tray: icon, menu, notifications, global hotkey and folder picker.
//! Everything in here runs on the main (STA) thread. The settings window
//! (`ui.rs`) runs on its own thread and talks to this one via `ui::Command`.

use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::sync::Arc;

use windows::core::{w, HSTRING, PCWSTR};
use windows::Win32::Foundation::{ERROR_CANCELLED, HINSTANCE, HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    CreateBitmap, CreateDIBSection, DeleteObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS,
};
use windows::Win32::System::Com::{CoCreateInstance, CoTaskMemFree, CLSCTX_INPROC_SERVER};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{RegisterHotKey, UnregisterHotKey, HOT_KEY_MODIFIERS, MOD_NOREPEAT};
use windows::Win32::UI::Shell::{
    FileOpenDialog, IFileOpenDialog, IShellItem, SHCreateItemFromParsingName, ShellExecuteW,
    Shell_NotifyIconW, FOS_FORCEFILESYSTEM, FOS_PATHMUSTEXIST, FOS_PICKFOLDERS, NIF_ICON, NIF_INFO,
    NIF_MESSAGE, NIF_TIP, NIIF_INFO, NIIF_NOSOUND, NIIF_WARNING, NIM_ADD, NIM_DELETE, NIM_MODIFY,
    NOTIFYICONDATAW, SIGDN_FILESYSPATH,
};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::capture::DiscordStatus;
use crate::config::{self, Config};
use crate::hotkey::Hotkey;
use crate::log;
use crate::ringbuffer::RingBuffer;
use crate::ui::{self, Command, Snapshot};

const WM_TRAY: u32 = WM_APP + 1;
const WM_STATUS: u32 = WM_APP + 2;
const WM_UI_COMMAND: u32 = WM_APP + 3;
const NIN_BALLOONUSERCLICK: u32 = WM_USER + 5;
const HOTKEY_ID: i32 = 1;
const TRAY_UID: u32 = 1;

const ID_SAVE: usize = 100;
const ID_OPEN_FOLDER: usize = 101;
const ID_WINDOW: usize = 102;
const ID_LOG: usize = 106;
const ID_QUIT: usize = 107;

/// Main window, for other threads to post status updates to.
static MAIN_HWND: AtomicIsize = AtomicIsize::new(0);

fn post(msg: u32) {
    let h = MAIN_HWND.load(Ordering::SeqCst);
    if h != 0 {
        unsafe {
            let _ = PostMessageW(HWND(h as *mut c_void), msg, WPARAM(0), LPARAM(0));
        }
    }
}

/// Tells the UI that `status` changed. Callable from any thread.
pub fn post_status_changed() {
    post(WM_STATUS);
}

/// Tells the tray thread the window has queued commands. Callable from any thread.
pub fn post_ui_command() {
    post(WM_UI_COMMAND);
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Exit {
    Quit,
    /// Restart (re-reading the config file) and reopen the settings window.
    Restart,
}

pub struct App {
    /// Effective settings (file + command-line overrides).
    pub cfg: RefCell<Config>,
    /// What's in the config file; UI changes are applied here and saved, so
    /// one-off command-line flags never get written to disk.
    pub file_cfg: RefCell<Config>,
    pub discord_ring: Arc<RingBuffer>,
    pub mic_ring: Option<Arc<RingBuffer>>,
    pub mic_status: String,
    pub discord_status: Arc<std::sync::Mutex<DiscordStatus>>,
    pub running: Arc<AtomicBool>,
    /// Shown as notifications once the tray icon exists.
    pub startup_warnings: Vec<String>,
    /// Open the settings window right away (`--settings`).
    pub open_window: bool,

    /// Clip length and mic state the capture threads were started with;
    /// changing them needs a restart.
    running_seconds: u32,
    running_mic: bool,
    hwnd: Cell<HWND>,
    hotkey_registered: Cell<bool>,
    /// Why the last hotkey change from the window failed.
    hotkey_error: RefCell<Option<String>>,
    last_clip: RefCell<Option<PathBuf>>,
    taskbar_created: Cell<u32>,
    icon_active: Cell<HICON>,
    icon_idle: Cell<HICON>,
    exit: Cell<Exit>,
}

impl App {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        cfg: Config,
        file_cfg: Config,
        discord_ring: Arc<RingBuffer>,
        mic_ring: Option<Arc<RingBuffer>>,
        mic_status: String,
        discord_status: Arc<std::sync::Mutex<DiscordStatus>>,
        running: Arc<AtomicBool>,
        startup_warnings: Vec<String>,
        open_window: bool,
    ) -> Self {
        App {
            running_seconds: cfg.seconds,
            running_mic: cfg.mic,
            cfg: RefCell::new(cfg),
            file_cfg: RefCell::new(file_cfg),
            discord_ring,
            mic_ring,
            mic_status,
            discord_status,
            running,
            startup_warnings,
            open_window,
            hwnd: Cell::new(HWND::default()),
            hotkey_registered: Cell::new(false),
            hotkey_error: RefCell::new(None),
            last_clip: RefCell::new(None),
            taskbar_created: Cell::new(0),
            icon_active: Cell::new(HICON::default()),
            icon_idle: Cell::new(HICON::default()),
            exit: Cell::new(Exit::Quit),
        }
    }
}

thread_local! {
    static APP: RefCell<Option<Rc<App>>> = const { RefCell::new(None) };
}

fn app() -> Option<Rc<App>> {
    APP.with(|a| a.borrow().clone())
}

// ---------------------------------------------------------------------------
// Entry point / message loop
// ---------------------------------------------------------------------------

pub fn run(app: App) -> anyhow::Result<Exit> {
    let app = Rc::new(app);
    APP.with(|a| *a.borrow_mut() = Some(Rc::clone(&app)));

    unsafe {
        let hinst: HINSTANCE = GetModuleHandleW(None)?.into();

        let main_class = WNDCLASSW {
            lpfnWndProc: Some(main_wndproc),
            hInstance: hinst,
            lpszClassName: w!("DiscordClipTray"),
            ..Default::default()
        };
        RegisterClassW(&main_class);

        app.taskbar_created.set(RegisterWindowMessageW(w!("TaskbarCreated")));
        let size = GetSystemMetrics(SM_CXSMICON).max(16);
        app.icon_active.set(make_icon(size, (0x58, 0x65, 0xF2))); // Discord blurple
        app.icon_idle.set(make_icon(size, (0x80, 0x84, 0x8E)));

        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("DiscordClipTray"),
            w!("Discord Clip"),
            WS_OVERLAPPED,
            0,
            0,
            0,
            0,
            None,
            None,
            hinst,
            None,
        )?;
        app.hwnd.set(hwnd);
        MAIN_HWND.store(hwnd.0 as isize, Ordering::SeqCst);

        tray_add(&app);
        register_hotkey(&app);

        let warnings = app.startup_warnings.join("\n");
        if !warnings.is_empty() {
            notify(&app, "Discord Clip: check your settings", &warnings, true);
        } else if app.hotkey_registered.get() && !app.open_window {
            let cfg = app.cfg.borrow().clone();
            notify(
                &app,
                "Discord Clip is running",
                &format!("Press {} to save the last {}s.", cfg.hotkey, cfg.seconds),
                false,
            );
        }
        if app.open_window {
            ui::open(snapshot(&app));
        }

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).0 > 0 {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }

        MAIN_HWND.store(0, Ordering::SeqCst);
        for icon in [app.icon_active.get(), app.icon_idle.get()] {
            let _ = DestroyIcon(icon);
        }
    }

    APP.with(|a| *a.borrow_mut() = None);
    Ok(app.exit.get())
}

unsafe extern "system" fn main_wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let Some(app) = app() else {
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    };
    match msg {
        WM_HOTKEY if wparam.0 as i32 == HOTKEY_ID => save_clip(&app),
        WM_TRAY => match lparam.0 as u32 & 0xFFFF {
            WM_LBUTTONUP => ui::open(snapshot(&app)),
            WM_RBUTTONUP | WM_CONTEXTMENU => show_menu(&app),
            NIN_BALLOONUSERCLICK => show_last_clip(&app),
            _ => {}
        },
        WM_STATUS => {
            tray_update(&app);
            ui::publish(snapshot(&app));
        }
        WM_UI_COMMAND => {
            for cmd in ui::take_commands() {
                handle_ui_command(&app, cmd);
            }
            ui::publish(snapshot(&app));
        }
        WM_DESTROY => {
            unregister_hotkey(&app);
            tray_remove(&app);
            PostQuitMessage(0);
        }
        m if m != 0 && m == app.taskbar_created.get() => {
            // Explorer restarted: our icon is gone, add it back.
            tray_add(&app);
        }
        _ => return DefWindowProcW(hwnd, msg, wparam, lparam),
    }
    LRESULT(0)
}

fn quit(app: &App, exit: Exit) {
    app.exit.set(exit);
    app.running.store(false, Ordering::SeqCst);
    unsafe {
        let _ = DestroyWindow(app.hwnd.get());
    }
}

// ---------------------------------------------------------------------------
// Tray icon
// ---------------------------------------------------------------------------

fn copy_wide(dst: &mut [u16], s: &str) {
    let mut n = 0;
    let max = dst.len() - 1;
    for (d, c) in dst.iter_mut().zip(s.encode_utf16()).take(max) {
        *d = c;
        n += 1;
    }
    dst[n] = 0;
}

fn base_nid(app: &App) -> NOTIFYICONDATAW {
    NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: app.hwnd.get(),
        uID: TRAY_UID,
        ..Default::default()
    }
}

fn discord_line(app: &App) -> (String, bool) {
    match &*app.discord_status.lock().unwrap() {
        DiscordStatus::Waiting => ("Discord: not running (waiting)".into(), false),
        DiscordStatus::Capturing { pid, exe } => (format!("Discord: capturing {exe} (pid {pid})"), true),
        DiscordStatus::Failed(e) => (format!("Discord: error - {e}"), false),
    }
}

fn tray_nid(app: &App) -> NOTIFYICONDATAW {
    let (_, active) = discord_line(app);
    let cfg = app.cfg.borrow();
    let mut nid = base_nid(app);
    nid.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
    nid.uCallbackMessage = WM_TRAY;
    nid.hIcon = if active { app.icon_active.get() } else { app.icon_idle.get() };
    let short = if active { "Discord: capturing" } else { "Discord: waiting" };
    copy_wide(
        &mut nid.szTip,
        &format!("Discord Clip\n{} saves the last {}s\n{short}", cfg.hotkey, app.running_seconds),
    );
    nid
}

fn tray_add(app: &App) {
    unsafe {
        let _ = Shell_NotifyIconW(NIM_ADD, &tray_nid(app));
    }
}

fn tray_update(app: &App) {
    unsafe {
        let _ = Shell_NotifyIconW(NIM_MODIFY, &tray_nid(app));
    }
}

fn tray_remove(app: &App) {
    unsafe {
        let _ = Shell_NotifyIconW(NIM_DELETE, &base_nid(app));
    }
}

/// Shows a tray notification. Warnings are shown even when the user turned
/// notifications off. Silent: we beep ourselves (and Windows mutes
/// notification sounds while gaming anyway).
fn notify(app: &App, title: &str, text: &str, warning: bool) {
    if !warning && !app.cfg.borrow().notifications {
        return;
    }
    let mut nid = base_nid(app);
    nid.uFlags = NIF_INFO;
    copy_wide(&mut nid.szInfoTitle, title);
    copy_wide(&mut nid.szInfo, text);
    nid.dwInfoFlags = if warning { NIIF_WARNING } else { NIIF_INFO } | NIIF_NOSOUND;
    unsafe {
        let _ = Shell_NotifyIconW(NIM_MODIFY, &nid);
    }
}

/// The app icon as top-down RGBA pixels (for the settings window).
pub fn icon_rgba(size: i32, rgb: (u8, u8, u8)) -> Vec<u8> {
    let mut px = icon_bgra(size, rgb);
    for p in px.chunks_exact_mut(4) {
        p.swap(0, 2);
    }
    px
}

/// Draws the app icon, a filled circle in `rgb` with a white waveform, as
/// top-down BGRA pixels with straight alpha.
fn icon_bgra(size: i32, rgb: (u8, u8, u8)) -> Vec<u8> {
    const SS: usize = 4; // supersampling per axis
    const BARS: [(f32, f32); 5] = [(8.5, 3.5), (12.25, 7.0), (16.0, 10.0), (19.75, 7.0), (23.5, 3.5)];
    let scale = 32.0 / size as f32;
    let mut px = vec![0u8; (size * size * 4) as usize];
    for y in 0..size {
        for x in 0..size {
            let (mut circle, mut white) = (0usize, 0usize);
            for sy in 0..SS {
                for sx in 0..SS {
                    let fx = (x as f32 + (sx as f32 + 0.5) / SS as f32) * scale;
                    let fy = (y as f32 + (sy as f32 + 0.5) / SS as f32) * scale;
                    let (dx, dy) = (fx - 16.0, fy - 16.0);
                    if dx * dx + dy * dy <= 15.5 * 15.5 {
                        circle += 1;
                        if BARS.iter().any(|&(cx, hh)| (fx - cx).abs() <= 1.4 && dy.abs() <= hh) {
                            white += 1;
                        }
                    }
                }
            }
            if circle == 0 {
                continue;
            }
            let w = white as f32 / circle as f32;
            let mix = |c: u8| (255.0 * w + c as f32 * (1.0 - w)).round() as u8;
            let i = ((y * size + x) * 4) as usize;
            px[i] = mix(rgb.2);
            px[i + 1] = mix(rgb.1);
            px[i + 2] = mix(rgb.0);
            px[i + 3] = (255.0 * circle as f32 / (SS * SS) as f32).round() as u8;
        }
    }
    px
}

fn make_icon(size: i32, rgb: (u8, u8, u8)) -> HICON {
    let px = icon_bgra(size, rgb);
    unsafe {
        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: size,
                biHeight: -size, // top-down
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits: *mut c_void = std::ptr::null_mut();
        let Ok(color) = CreateDIBSection(None, &bmi, DIB_RGB_COLORS, &mut bits, None, 0) else {
            return LoadIconW(None, IDI_APPLICATION).unwrap_or_default();
        };
        std::ptr::copy_nonoverlapping(px.as_ptr(), bits as *mut u8, px.len());
        let mask_bits = vec![0u8; ((size + 15) / 16 * 2 * size) as usize];
        let mask = CreateBitmap(size, size, 1, 1, Some(mask_bits.as_ptr() as *const c_void));
        let info = ICONINFO {
            fIcon: true.into(),
            xHotspot: 0,
            yHotspot: 0,
            hbmMask: mask,
            hbmColor: color,
        };
        let icon = CreateIconIndirect(&info).unwrap_or_else(|_| LoadIconW(None, IDI_APPLICATION).unwrap_or_default());
        let _ = DeleteObject(color);
        let _ = DeleteObject(mask);
        icon
    }
}

// ---------------------------------------------------------------------------
// Menu & actions
// ---------------------------------------------------------------------------

fn show_menu(app: &App) {
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

fn handle_command(app: &App, cmd: usize) {
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

fn open_clips_folder(app: &App) {
    let dir = app.cfg.borrow().out_dir.clone();
    let _ = std::fs::create_dir_all(&dir);
    shell_open(&dir);
}

// ---------------------------------------------------------------------------
// Settings window
// ---------------------------------------------------------------------------

fn snapshot(app: &App) -> Snapshot {
    let (discord, discord_active) = discord_line(app);
    let cfg = app.cfg.borrow().clone();
    Snapshot {
        restart_needed: cfg.seconds != app.running_seconds || cfg.mic != app.running_mic,
        cfg,
        discord: discord.trim_start_matches("Discord: ").to_string(),
        discord_active,
        mic: app.mic_status.clone(),
        hotkey_registered: app.hotkey_registered.get(),
        hotkey_error: app.hotkey_error.borrow().clone(),
    }
}

fn handle_ui_command(app: &App, cmd: Command) {
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
        Command::SetMicGain(g) => update_settings(app, |c| c.mic_gain = g.clamp(0.0, 10.0)),
        Command::SetClipboard(on) => update_settings(app, |c| c.clipboard = on),
        Command::SetNotifications(on) => update_settings(app, |c| c.notifications = on),
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
    }
}

fn change_hotkey(app: &App, new: Hotkey) {
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

fn save_clip(app: &App) {
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
            notify(app, "Clip saved", &text, false);
        }
        Err(e) => {
            beep(false);
            log!("failed to save clip: {e:#}");
            notify(app, "Couldn't save clip", &format!("{e:#}"), true);
        }
    }
}

fn show_last_clip(app: &App) {
    let Some(path) = app.last_clip.borrow().clone() else { return };
    let args = HSTRING::from(format!("/select,\"{}\"", path.display()));
    unsafe {
        ShellExecuteW(None, w!("open"), w!("explorer.exe"), &args, PCWSTR::null(), SW_SHOWNORMAL);
    }
}

fn shell_open(path: &Path) {
    let p = HSTRING::from(path.as_os_str());
    unsafe {
        ShellExecuteW(None, w!("open"), &p, PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL);
    }
}

fn beep(ok: bool) {
    use windows::Win32::System::Diagnostics::Debug::MessageBeep;
    unsafe {
        let _ = MessageBeep(if ok { MB_OK } else { MB_ICONHAND });
    }
}

// ---------------------------------------------------------------------------
// Hotkey registration
// ---------------------------------------------------------------------------

fn try_register(app: &App, hk: Hotkey) -> bool {
    unsafe {
        RegisterHotKey(app.hwnd.get(), HOTKEY_ID, HOT_KEY_MODIFIERS(hk.mods) | MOD_NOREPEAT, hk.vk).is_ok()
    }
}

fn register_hotkey(app: &App) {
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

fn unregister_hotkey(app: &App) {
    if app.hotkey_registered.replace(false) {
        unsafe {
            let _ = UnregisterHotKey(app.hwnd.get(), HOTKEY_ID);
        }
    }
}

/// Applies a UI change to both the live and the on-disk settings.
fn update_settings(app: &App, f: impl Fn(&mut Config)) {
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

fn change_folder(app: &App) {
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

fn pick_folder(owner: HWND, current: &Path) -> anyhow::Result<Option<PathBuf>> {
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
