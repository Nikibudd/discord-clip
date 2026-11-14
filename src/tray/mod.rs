//! The system tray app: owns the settings, the global hotkey and the tray
//! icon. Everything in here runs on the main (STA) thread; other threads
//! (capture supervisor, update checks, the window) post messages to it.
//!
//! * `notify_icon`: tray icon, tooltip, notifications
//! * `menu`: right-click menu
//! * `global_hotkey`: registering the save-clip hotkey
//! * `actions`: saving clips, opening folders
//! * `window`: the window's snapshot and commands, folder picker, updates

mod actions;
mod global_hotkey;
mod menu;
mod notify_icon;
mod window;

use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::sync::Arc;

use windows::core::w;
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::capture::DiscordStatus;
use crate::config::Config;
use crate::log;
use crate::ringbuffer::RingBuffer;
use crate::ui;
use actions::{finish_clip, open_last_clip, request_clip};
use global_hotkey::{register_hotkey, unregister_hotkey, HOTKEY_ID};
use notify_icon::{make_icon, notify, notify_always, tray_add, tray_remove, tray_update};
use window::{handle_ui_command, install_update, notify_update_available, snapshot};

const WM_TRAY: u32 = WM_APP + 1;
const WM_STATUS: u32 = WM_APP + 2;
const WM_UI_COMMAND: u32 = WM_APP + 3;
const WM_UPDATE_READY: u32 = WM_APP + 4;
const WM_LIBRARY_CHANGED: u32 = WM_APP + 5;
/// Refreshes the tooltip's clips line ("last saved 2 min ago", and changes
/// made in Explorer) once a minute (#48).
const TOOLTIP_TIMER: usize = 1;
/// Fires `clip::AFTER_SECONDS` after the hotkey to save the clip (#64).
const CLIP_TIMER: usize = 2;
const NIN_BALLOONUSERCLICK: u32 = WM_USER + 5;

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

/// Tells the tray thread the window changed the clips (renamed one, changed
/// an uploaded mark, ...), so the tooltip's count is updated. Callable from any thread.
pub fn post_library_changed() {
    post(WM_LIBRARY_CHANGED);
}

/// Tells the tray thread a downloaded update is ready to install. Callable from any thread.
pub fn post_update_ready() {
    post(WM_UPDATE_READY);
}

/// What clicking the last notification does.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Balloon {
    ShowClip,
    OpenUpdates,
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
    running_mic_device: String,
    /// Capture devices for the microphone picker (refreshed on demand).
    mics: RefCell<Vec<crate::capture::Mic>>,
    hwnd: Cell<HWND>,
    hotkey_registered: Cell<bool>,
    /// Why the last hotkey change from the window failed.
    hotkey_error: RefCell<Option<String>>,
    last_clip: RefCell<Option<PathBuf>>,
    /// The hotkey was pressed and the clip is saved when `CLIP_TIMER` fires (#64).
    clip_pending: Cell<bool>,
    balloon: Cell<Balloon>,
    /// Version we already showed an "update available" notification for.
    notified_update: RefCell<Option<String>>,
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
            running_mic_device: cfg.mic_device.clone(),
            mics: RefCell::new(Vec::new()),
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
            clip_pending: Cell::new(false),
            balloon: Cell::new(Balloon::ShowClip),
            notified_update: RefCell::new(None),
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
        SetTimer(hwnd, TOOLTIP_TIMER, 60_000, None);
        register_hotkey(&app);
        window::refresh_mics(&app);

        let updated_from = crate::update::note_version();
        // First start after an update (from the window, or a newer installer
        // run by hand): open the window so the user sees it worked.
        let reopen = updated_from.is_some();
        let warnings = app.startup_warnings.join("\n");
        if !warnings.is_empty() {
            notify(&app, "Discord Clip: check your settings", &warnings, true);
        } else if let Some(prev) = updated_from {
            app.balloon.set(Balloon::OpenUpdates);
            log!("updated from {prev} to {}", crate::update::CURRENT);
            notify_always(
                &app,
                &format!("Discord Clip updated to {}", crate::update::CURRENT),
                &format!("Updated from {prev}. Click to see the release notes."),
            );
        } else if app.hotkey_registered.get() && !app.open_window && !reopen {
            let cfg = app.cfg.borrow().clone();
            notify(
                &app,
                "Discord Clip is running",
                &format!("Press {} to save the last {}s.", cfg.hotkey, cfg.seconds),
                false,
            );
        }
        if reopen {
            log!("reopening the window after the update");
            ui::open_page(snapshot(&app), ui::Page::Updates);
        } else if app.open_window {
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
        WM_HOTKEY if wparam.0 as i32 == HOTKEY_ID => request_clip(&app),
        WM_TRAY => match lparam.0 as u32 & 0xFFFF {
            WM_LBUTTONUP => ui::open(snapshot(&app)),
            WM_RBUTTONUP | WM_CONTEXTMENU => menu::show_menu(&app),
            NIN_BALLOONUSERCLICK => match app.balloon.get() {
                Balloon::ShowClip => open_last_clip(&app),
                Balloon::OpenUpdates => ui::open_page(snapshot(&app), ui::Page::Updates),
            },
            _ => {}
        },
        WM_STATUS => {
            tray_update(&app);
            notify_update_available(&app);
            ui::publish(snapshot(&app));
        }
        WM_UPDATE_READY => install_update(&app),
        WM_UI_COMMAND => {
            for cmd in ui::take_commands() {
                handle_ui_command(&app, cmd);
            }
            ui::publish(snapshot(&app));
            tray_update(&app); // e.g. a clip was deleted
        }
        WM_LIBRARY_CHANGED => tray_update(&app),
        WM_TIMER if wparam.0 == TOOLTIP_TIMER => tray_update(&app),
        WM_TIMER if wparam.0 == CLIP_TIMER => finish_clip(&app),
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
    // A clip waiting out its last seconds (#64) is saved now, with what
    // was recorded so far, rather than lost.
    finish_clip(app);
    app.exit.set(exit);
    app.running.store(false, Ordering::SeqCst);
    unsafe {
        let _ = DestroyWindow(app.hwnd.get());
    }
}

