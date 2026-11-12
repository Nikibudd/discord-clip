//! System tray UI: icon, menu, notifications, hotkey picker and folder picker.
//! Everything in here runs on the main (STA, UI) thread.

use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::sync::Arc;

use windows::core::{w, HSTRING, PCWSTR};
use windows::Win32::Foundation::{
    ERROR_CANCELLED, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM,
};
use windows::Win32::Graphics::Gdi::{
    CreateBitmap, CreateDIBSection, CreateFontIndirectW, DeleteObject, BITMAPINFO,
    BITMAPINFOHEADER, BI_RGB, COLOR_BTNFACE, DIB_RGB_COLORS, HBRUSH, HFONT,
};
use windows::Win32::System::Com::{CoCreateInstance, CoTaskMemFree, CLSCTX_INPROC_SERVER};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::{InitCommonControlsEx, ICC_HOTKEY_CLASS, ICC_STANDARD_CLASSES, INITCOMMONCONTROLSEX};
use windows::Win32::UI::HiDpi::GetDpiForSystem;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    RegisterHotKey, SetFocus, UnregisterHotKey, HOT_KEY_MODIFIERS, MOD_NOREPEAT,
};
use windows::Win32::UI::Shell::{
    FileOpenDialog, IFileOpenDialog, IShellItem, SHCreateItemFromParsingName, ShellExecuteW,
    Shell_NotifyIconW, FOS_FORCEFILESYSTEM, FOS_PATHMUSTEXIST, FOS_PICKFOLDERS, NIF_ICON, NIF_INFO,
    NIF_MESSAGE, NIF_TIP, NIIF_INFO, NIIF_NOSOUND, NIIF_WARNING, NIM_ADD, NIM_DELETE, NIM_MODIFY,
    NOTIFYICONDATAW, SIGDN_FILESYSPATH,
};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::capture::DiscordStatus;
use crate::config::{self, Config};
use crate::hotkey::{Hotkey, MOD_ALT, MOD_CONTROL, MOD_SHIFT};
use crate::log;
use crate::ringbuffer::RingBuffer;

const WM_TRAY: u32 = WM_APP + 1;
const WM_STATUS: u32 = WM_APP + 2;
const NIN_BALLOONUSERCLICK: u32 = WM_USER + 5;
const HOTKEY_ID: i32 = 1;
const TRAY_UID: u32 = 1;

const ID_SAVE: usize = 100;
const ID_OPEN_FOLDER: usize = 101;
const ID_HOTKEY: usize = 102;
const ID_FOLDER: usize = 103;
const ID_SETTINGS: usize = 104;
const ID_RELOAD: usize = 105;
const ID_LOG: usize = 106;
const ID_QUIT: usize = 107;

// Hotkey dialog.
const IDOK: i32 = 1;
const IDCANCEL: i32 = 2;
const IDC_HOTKEY: i32 = 201;
const IDC_INFO: i32 = 202;
const HKM_SETHOTKEY: u32 = WM_USER + 1;
const HKM_GETHOTKEY: u32 = WM_USER + 2;
const HOTKEYF_SHIFT: u32 = 0x1;
const HOTKEYF_CONTROL: u32 = 0x2;
const HOTKEYF_ALT: u32 = 0x4;

/// Main window, for other threads to post status updates to.
static MAIN_HWND: AtomicIsize = AtomicIsize::new(0);

/// Tells the UI that `status` changed. Callable from any thread.
pub fn post_status_changed() {
    let h = MAIN_HWND.load(Ordering::SeqCst);
    if h != 0 {
        unsafe {
            let _ = PostMessageW(HWND(h as *mut c_void), WM_STATUS, WPARAM(0), LPARAM(0));
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Exit {
    Quit,
    Reload,
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

    hwnd: Cell<HWND>,
    hotkey_dialog: Cell<HWND>,
    hotkey_registered: Cell<bool>,
    last_clip: RefCell<Option<PathBuf>>,
    taskbar_created: Cell<u32>,
    icon_active: Cell<HICON>,
    icon_idle: Cell<HICON>,
    font: Cell<HFONT>,
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
    ) -> Self {
        App {
            cfg: RefCell::new(cfg),
            file_cfg: RefCell::new(file_cfg),
            discord_ring,
            mic_ring,
            mic_status,
            discord_status,
            running,
            startup_warnings,
            hwnd: Cell::new(HWND::default()),
            hotkey_dialog: Cell::new(HWND::default()),
            hotkey_registered: Cell::new(false),
            last_clip: RefCell::new(None),
            taskbar_created: Cell::new(0),
            icon_active: Cell::new(HICON::default()),
            icon_idle: Cell::new(HICON::default()),
            font: Cell::new(HFONT::default()),
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
        let _ = InitCommonControlsEx(&INITCOMMONCONTROLSEX {
            dwSize: std::mem::size_of::<INITCOMMONCONTROLSEX>() as u32,
            dwICC: ICC_HOTKEY_CLASS | ICC_STANDARD_CLASSES,
        });
        let hinst: HINSTANCE = GetModuleHandleW(None)?.into();

        let main_class = WNDCLASSW {
            lpfnWndProc: Some(main_wndproc),
            hInstance: hinst,
            lpszClassName: w!("DiscordClipTray"),
            ..Default::default()
        };
        RegisterClassW(&main_class);
        let dlg_class = WNDCLASSW {
            lpfnWndProc: Some(dialog_wndproc),
            hInstance: hinst,
            lpszClassName: w!("DiscordClipHotkeyDialog"),
            hbrBackground: HBRUSH((COLOR_BTNFACE.0 + 1) as usize as *mut c_void),
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            ..Default::default()
        };
        RegisterClassW(&dlg_class);

        app.taskbar_created.set(RegisterWindowMessageW(w!("TaskbarCreated")));
        let size = GetSystemMetrics(SM_CXSMICON).max(16);
        app.icon_active.set(make_icon(size, (0x58, 0x65, 0xF2))); // Discord blurple
        app.icon_idle.set(make_icon(size, (0x80, 0x84, 0x8E)));
        app.font.set(message_font());

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
        } else if app.hotkey_registered.get() {
            let cfg = app.cfg.borrow().clone();
            notify(
                &app,
                "Discord Clip is running",
                &format!("Press {} to save the last {}s.", cfg.hotkey, cfg.seconds),
                false,
            );
        }

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).0 > 0 {
            let dlg = app.hotkey_dialog.get();
            if !dlg.is_invalid() && IsDialogMessageW(dlg, &msg).as_bool() {
                continue;
            }
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }

        MAIN_HWND.store(0, Ordering::SeqCst);
        for icon in [app.icon_active.get(), app.icon_idle.get()] {
            let _ = DestroyIcon(icon);
        }
        let _ = DeleteObject(app.font.get());
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
            WM_LBUTTONUP | WM_RBUTTONUP | WM_CONTEXTMENU => show_menu(&app),
            NIN_BALLOONUSERCLICK => show_last_clip(&app),
            _ => {}
        },
        WM_STATUS => tray_update(&app),
        WM_DESTROY => {
            let dlg = app.hotkey_dialog.get();
            if !dlg.is_invalid() {
                let _ = DestroyWindow(dlg);
            }
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
        &format!("Discord Clip\n{} saves the last {}s\n{short}", cfg.hotkey, cfg.seconds),
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

/// Draws the tray icon: a filled circle in `rgb` with a white waveform.
fn make_icon(size: i32, rgb: (u8, u8, u8)) -> HICON {
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
    let (hotkey, folder) = {
        let cfg = app.cfg.borrow();
        (cfg.hotkey.to_string(), cfg.out_dir.display().to_string())
    };
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
        item(MF_STRING, ID_SAVE, &format!("Save clip now\t{hotkey}"));
        item(MF_STRING, ID_OPEN_FOLDER, "Open clips folder");
        sep();
        item(MF_STRING, ID_HOTKEY, &format!("Change hotkey...\t{hotkey}"));
        item(MF_STRING, ID_FOLDER, &format!("Change clips folder...\t{}", shorten(&folder, 40)));
        item(MF_STRING, ID_SETTINGS, "Edit settings file...");
        item(MF_STRING, ID_RELOAD, "Reload settings");
        item(MF_STRING, ID_LOG, "Open log");
        sep();
        item(MF_STRING, ID_QUIT, "Quit");
        let _ = SetMenuDefaultItem(menu, ID_SAVE as u32, 0);

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

fn shorten(s: &str, max: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= max {
        s.to_string()
    } else {
        let tail: String = chars[chars.len() - (max - 3)..].iter().collect();
        format!("...{tail}")
    }
}

fn handle_command(app: &App, cmd: usize) {
    match cmd {
        ID_SAVE => save_clip(app),
        ID_OPEN_FOLDER => {
            let dir = app.cfg.borrow().out_dir.clone();
            let _ = std::fs::create_dir_all(&dir);
            shell_open(&dir);
        }
        ID_HOTKEY => open_hotkey_dialog(app),
        ID_FOLDER => change_folder(app),
        ID_SETTINGS => {
            let path = config::config_path();
            if !path.exists() {
                if let Err(e) = app.file_cfg.borrow().save(&path) {
                    log!("couldn't create settings file: {e:#}");
                }
            }
            shell_open(&path);
            notify(app, "Editing settings", "Save the file, then choose \"Reload settings\" from the tray menu.", false);
        }
        ID_RELOAD => {
            log!("reloading settings");
            quit(app, Exit::Reload);
        }
        ID_LOG => shell_open(&log::log_path()),
        ID_QUIT => {
            log!("quit requested");
            quit(app, Exit::Quit);
        }
        _ => {}
    }
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
            &format!("{hk} is already used by another app.\nRight-click the tray icon and choose \"Change hotkey...\"."),
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
// Hotkey dialog
// ---------------------------------------------------------------------------

fn scale(v: i32) -> i32 {
    let dpi = unsafe { GetDpiForSystem() } as i32;
    v * dpi.max(96) / 96
}

fn message_font() -> HFONT {
    unsafe {
        let mut ncm = NONCLIENTMETRICSW {
            cbSize: std::mem::size_of::<NONCLIENTMETRICSW>() as u32,
            ..Default::default()
        };
        if SystemParametersInfoW(
            SPI_GETNONCLIENTMETRICS,
            ncm.cbSize,
            Some(&mut ncm as *mut _ as *mut c_void),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        )
        .is_ok()
        {
            CreateFontIndirectW(&ncm.lfMessageFont)
        } else {
            HFONT::default()
        }
    }
}

fn open_hotkey_dialog(app: &App) {
    unsafe {
        let existing = app.hotkey_dialog.get();
        if !existing.is_invalid() {
            let _ = SetForegroundWindow(existing);
            return;
        }
        // While picking, the current hotkey must not fire (or be swallowed).
        unregister_hotkey(app);

        let Ok(hinst) = GetModuleHandleW(None) else { return };
        let hinst: HINSTANCE = hinst.into();
        let (cw, ch) = (scale(380), scale(150));
        let style = WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU;
        let ex = WS_EX_DLGMODALFRAME | WS_EX_TOPMOST | WS_EX_CONTROLPARENT;
        let mut rc = RECT { left: 0, top: 0, right: cw, bottom: ch };
        let _ = AdjustWindowRectEx(&mut rc, style, false, ex);
        let (ww, wh) = (rc.right - rc.left, rc.bottom - rc.top);
        let x = (GetSystemMetrics(SM_CXSCREEN) - ww) / 2;
        let y = (GetSystemMetrics(SM_CYSCREEN) - wh) / 2;

        let Ok(dlg) = CreateWindowExW(
            ex,
            w!("DiscordClipHotkeyDialog"),
            w!("Discord Clip - Change hotkey"),
            style,
            x,
            y,
            ww,
            wh,
            None,
            None,
            hinst,
            None,
        ) else {
            register_hotkey(app);
            return;
        };
        app.hotkey_dialog.set(dlg);

        let current = app.cfg.borrow().hotkey;
        let font = app.font.get();
        let child = |class: PCWSTR, text: &str, style: WINDOW_STYLE, id: i32, x: i32, y: i32, w: i32, h: i32| {
            let h = CreateWindowExW(
                WINDOW_EX_STYLE(0),
                class,
                &HSTRING::from(text),
                WS_CHILD | WS_VISIBLE | style,
                scale(x),
                scale(y),
                scale(w),
                scale(h),
                dlg,
                HMENU(id as isize as *mut c_void),
                hinst,
                None,
            )
            .unwrap_or_default();
            if !font.is_invalid() {
                SendMessageW(h, WM_SETFONT, WPARAM(font.0 as usize), LPARAM(1));
            }
            h
        };
        child(w!("STATIC"), "Click the box and press the key combination you want:", WINDOW_STYLE(0), -1, 14, 14, 352, 20);
        let hk = child(w!("msctls_hotkey32"), "", WS_TABSTOP | WS_BORDER, IDC_HOTKEY, 14, 38, 352, 26);
        child(
            w!("STATIC"),
            &format!("Current: {current}. Use Ctrl, Alt or Shift+key, or an F-key on its own."),
            WINDOW_STYLE(0),
            IDC_INFO,
            14,
            72,
            352,
            34,
        );
        child(w!("BUTTON"), "OK", WS_TABSTOP | WINDOW_STYLE(BS_DEFPUSHBUTTON as u32), IDOK, 196, 112, 80, 28);
        child(w!("BUTTON"), "Cancel", WS_TABSTOP | WINDOW_STYLE(BS_PUSHBUTTON as u32), IDCANCEL, 286, 112, 80, 28);

        let mut flags = 0;
        if current.mods & MOD_SHIFT != 0 {
            flags |= HOTKEYF_SHIFT;
        }
        if current.mods & MOD_CONTROL != 0 {
            flags |= HOTKEYF_CONTROL;
        }
        if current.mods & MOD_ALT != 0 {
            flags |= HOTKEYF_ALT;
        }
        SendMessageW(hk, HKM_SETHOTKEY, WPARAM(((flags << 8) | (current.vk & 0xFF)) as usize), LPARAM(0));

        let _ = ShowWindow(dlg, SW_SHOW);
        let _ = SetForegroundWindow(dlg);
        let _ = SetFocus(hk);
    }
}

unsafe extern "system" fn dialog_wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let Some(app) = app() else {
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    };
    match msg {
        WM_COMMAND => match (wparam.0 & 0xFFFF) as i32 {
            IDOK => hotkey_dialog_ok(&app, hwnd),
            IDCANCEL => {
                let _ = DestroyWindow(hwnd);
            }
            _ => {}
        },
        WM_CLOSE => {
            let _ = DestroyWindow(hwnd);
        }
        WM_DESTROY => {
            app.hotkey_dialog.set(HWND::default());
            // Cancelled (or failed): put the old hotkey back.
            register_hotkey(&app);
        }
        _ => return DefWindowProcW(hwnd, msg, wparam, lparam),
    }
    LRESULT(0)
}

fn hotkey_dialog_ok(app: &App, dlg: HWND) {
    unsafe {
        let set_info = |text: &str| {
            if let Ok(info) = GetDlgItem(dlg, IDC_INFO) {
                let _ = SetWindowTextW(info, &HSTRING::from(text));
            }
        };
        let Ok(hk_ctl) = GetDlgItem(dlg, IDC_HOTKEY) else { return };
        let v = SendMessageW(hk_ctl, HKM_GETHOTKEY, WPARAM(0), LPARAM(0)).0 as u32;
        let vk = v & 0xFF;
        let f = (v >> 8) & 0xFF;
        if vk == 0 {
            set_info("Press a key combination in the box first.");
            return;
        }
        let mut mods = 0;
        if f & HOTKEYF_SHIFT != 0 {
            mods |= MOD_SHIFT;
        }
        if f & HOTKEYF_CONTROL != 0 {
            mods |= MOD_CONTROL;
        }
        if f & HOTKEYF_ALT != 0 {
            mods |= MOD_ALT;
        }
        let new = Hotkey { mods, vk };
        if let Err(e) = new.check_sensible() {
            set_info(&e);
            return;
        }
        if !try_register(app, new) {
            set_info(&format!("{new} is already used by another app. Pick a different combination."));
            return;
        }
        app.hotkey_registered.set(true);
        update_settings(app, |c| c.hotkey = new);
        log!("hotkey changed to {new}");
        let _ = DestroyWindow(dlg);
        tray_update(app);
        let secs = app.cfg.borrow().seconds;
        notify(app, "Hotkey changed", &format!("Press {new} to save the last {secs}s."), false);
    }
}

// ---------------------------------------------------------------------------
// Clips folder picker
// ---------------------------------------------------------------------------

fn change_folder(app: &App) {
    let current = app.cfg.borrow().out_dir.clone();
    match pick_folder(app.hwnd.get(), &current) {
        Ok(Some(dir)) => {
            if let Err(e) = std::fs::create_dir_all(&dir) {
                notify(app, "Can't use that folder", &format!("{e}"), true);
                return;
            }
            update_settings(app, |c| c.out_dir = dir.clone());
            log!("clips folder changed to {}", dir.display());
            notify(app, "Clips folder changed", &format!("Clips will be saved in\n{}", dir.display()), false);
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
