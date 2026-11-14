//! The Discord Clip window: a sidebar of pages styled after Discord's dark
//! theme.
//!
//! The window runs on its own thread because winit needs to own a message
//! loop. It never touches the running app directly: anything that changes
//! state (hotkey registration, the config file, the folder picker, updates)
//! is sent to the tray thread as a [`Command`], and the tray thread publishes
//! a fresh [`Snapshot`] back for the window to draw.
//!
//! * `theme` / `widgets`: Discord's look and the controls built on it
//! * `sidebar`: page list and status panel
//! * `clips`, `settings` (+ `keybind`), `updates`: the pages

mod clips;
mod confirm;
mod edit;
mod follow;
mod keybind;
mod settings;
mod sidebar;
mod theme;
mod updates;
mod widgets;

use std::ffi::c_void;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU32, Ordering};
use std::sync::{Mutex, OnceLock};

use eframe::egui::{self, Color32, Frame, Margin, TextureHandle, Ui, ViewportCommand};
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::WindowsAndMessaging::{
    AllowSetForegroundWindow, GetForegroundWindow, IsIconic, SetForegroundWindow, ShowWindow, ASFW_ANY, SW_HIDE,
    SW_RESTORE, SW_SHOW,
};

use crate::config::Config;
use crate::hotkey::Hotkey;
use crate::log;
use theme::*;

// ---------------------------------------------------------------------------
// Window <-> tray thread
// ---------------------------------------------------------------------------

/// Requests from the window, handled on the tray thread.
#[derive(Debug)]
pub enum Command {
    /// The user started recording a new hotkey: stop the current one firing.
    BeginHotkeyCapture,
    /// Recording finished; `None` means cancelled.
    EndHotkeyCapture(Option<Hotkey>),
    SetSeconds(u32),
    SetMic(bool),
    /// Microphone endpoint id; empty = default communications device.
    SetMicDevice(String),
    /// Re-read the list of microphones (the picker was opened).
    RefreshMics,
    SetMicGain(f32),
    SetClipboard(bool),
    SetNotifications(bool),
    PickFolder,
    OpenFolder,
    OpenLog,
    OpenConfigFile,
    /// Restart the app (re-reads the config file) and reopen this window.
    Restart,
    SetCheckUpdates(bool),
    SetSeparateTracks(bool),
    /// Format new clips are saved in (#8). Applies to the next clip.
    SetFormat(crate::audio::Format),
    /// Window colours (#37).
    SetTheme(crate::config::ThemeMode),
    CheckUpdates,
    /// Download and install the update found by the last check.
    InstallUpdate,
    /// Open an https link in the browser.
    OpenUrl(String),
    /// Copy a clip to the clipboard (to paste into Discord). Clips with
    /// separate tracks are copied as a stereo mix, or just one track.
    CopyClip(PathBuf, crate::library::Tracks),
    /// Show a clip in Explorer.
    ShowClip(PathBuf),
    /// Move a clip to the Recycle Bin.
    DeleteClip(PathBuf),
    /// Start this exe when signing in to Windows.
    SetAutostart(bool),
}

/// Everything the window shows, published by the tray thread.
#[derive(Clone, Default)]
pub struct Snapshot {
    pub cfg: Config,
    /// e.g. "Capturing Discord.exe (pid 1234)".
    pub discord: String,
    pub discord_active: bool,
    /// "on", "off", "unavailable", or "fallback" (the picked mic isn't
    /// connected, so the default one is recorded).
    pub mic: String,
    pub hotkey_registered: bool,
    pub hotkey_error: Option<String>,
    /// Clip length or microphone changed; they apply after a restart.
    pub restart_needed: bool,
    /// Capture devices for the microphone picker.
    pub mics: Vec<crate::capture::Mic>,
    pub update: crate::update::Status,
    /// Installed copy (updates via installer) rather than a portable exe.
    pub installed: bool,
    /// "Start with Windows", read from the registry.
    pub autostart: crate::autostart::State,
}

struct Shared {
    snapshot: Mutex<Snapshot>,
    commands: Mutex<Vec<Command>>,
    ctx: Mutex<Option<egui::Context>>,
    hwnd: AtomicIsize,
    started: AtomicBool,
    /// Page to switch to the next time the window draws.
    requested_page: Mutex<Option<Page>>,
    /// A clip for the Clips page to scroll to and highlight (#50).
    focus_clip: Mutex<Option<PathBuf>>,
    /// Frames left in which to bring the window to the front (see `raise`).
    raise_frames: AtomicU32,
}

/// Frames to keep trying to bring the window to the front: a new window
/// only becomes visible after its first frame or two.
const RAISE_FRAMES: u32 = 10;

fn shared() -> &'static Shared {
    static SHARED: OnceLock<Shared> = OnceLock::new();
    SHARED.get_or_init(|| Shared {
        snapshot: Mutex::new(Snapshot::default()),
        commands: Mutex::new(Vec::new()),
        ctx: Mutex::new(None),
        hwnd: AtomicIsize::new(0),
        started: AtomicBool::new(false),
        requested_page: Mutex::new(None),
        focus_clip: Mutex::new(None),
        raise_frames: AtomicU32::new(0),
    })
}

/// Tray thread: updates what the window shows.
pub fn publish(snapshot: Snapshot) {
    *shared().snapshot.lock().unwrap() = snapshot;
    if let Some(ctx) = shared().ctx.lock().unwrap().as_ref() {
        ctx.request_repaint();
    }
}

/// Tray thread: takes the window's pending requests.
pub fn take_commands() -> Vec<Command> {
    std::mem::take(&mut *shared().commands.lock().unwrap())
}

/// The window's handle once it exists, e.g. to own the folder picker.
pub fn window_hwnd() -> Option<HWND> {
    match shared().hwnd.load(Ordering::SeqCst) {
        0 => None,
        h => Some(HWND(h as *mut c_void)),
    }
}

fn send(cmd: Command) {
    shared().commands.lock().unwrap().push(cmd);
    crate::tray::post_ui_command();
}

/// Tray thread: shows the window on a given page.
pub fn open_page(snapshot: Snapshot, page: Page) {
    *shared().requested_page.lock().unwrap() = Some(page);
    open(snapshot);
}

/// Tray thread: shows the window on the Clips page, scrolled to `clip` and
/// highlighting it (#50: clicking the "Clip saved" notification).
pub fn open_clip(snapshot: Snapshot, clip: PathBuf) {
    *shared().focus_clip.lock().unwrap() = Some(clip);
    open_page(snapshot, Page::Clips);
}

/// Tray thread: shows the window, creating it the first time. Closing the
/// window only hides it (winit can't create a second event loop).
pub fn open(snapshot: Snapshot) {
    publish(snapshot);
    unsafe {
        // We were just clicked in the tray, so we may hand focus to the
        // window thread.
        let _ = AllowSetForegroundWindow(ASFW_ANY);
    }
    // Clicking a notification (#50) doesn't give us the right to take the
    // foreground like a tray click does, so the window also asks winit to
    // focus it, which works around that.
    shared().raise_frames.store(RAISE_FRAMES, Ordering::SeqCst);
    if let Some(hwnd) = window_hwnd() {
        unsafe {
            let _ = ShowWindow(hwnd, if IsIconic(hwnd).as_bool() { SW_RESTORE } else { SW_SHOW });
            let _ = SetForegroundWindow(hwnd);
        }
        if let Some(ctx) = shared().ctx.lock().unwrap().as_ref() {
            ctx.request_repaint();
        }
        return;
    }
    if shared().started.swap(true, Ordering::SeqCst) {
        return; // still starting up
    }
    let spawned = std::thread::Builder::new().name("window".into()).spawn(|| {
        if let Err(e) = run_window() {
            log!("window failed: {e}");
        }
        shared().hwnd.store(0, Ordering::SeqCst);
        *shared().ctx.lock().unwrap() = None;
        shared().started.store(false, Ordering::SeqCst);
    });
    if let Err(e) = spawned {
        log!("couldn't start window thread: {e}");
        shared().started.store(false, Ordering::SeqCst);
    }
}

/// Window thread, each frame: while `open` asked for it, bring the window to
/// the front, until it is or the frames run out.
fn raise(ctx: &egui::Context) {
    let left = shared().raise_frames.load(Ordering::SeqCst);
    if left == 0 {
        return;
    }
    let in_front = window_hwnd().is_some_and(|h| unsafe { GetForegroundWindow() } == h);
    if in_front {
        shared().raise_frames.store(0, Ordering::SeqCst);
        return;
    }
    shared().raise_frames.store(left - 1, Ordering::SeqCst);
    ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
    ctx.request_repaint();
}

fn run_window() -> eframe::Result {
    let icon = egui::IconData { rgba: crate::icon::rgba(64, BLURPLE_RGB), width: 64, height: 64 };
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Discord Clip")
            .with_inner_size([920.0, 640.0])
            .with_min_inner_size([720.0, 480.0])
            .with_icon(icon),
        centered: true,
        event_loop_builder: Some(Box::new(|builder| {
            use winit::platform::windows::EventLoopBuilderExtWindows;
            builder.with_any_thread(true);
        })),
        ..Default::default()
    };
    eframe::run_native("Discord Clip", options, Box::new(|cc| Ok(Box::new(ClipWindow::new(cc)))))
}

// ---------------------------------------------------------------------------
// The window
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Page {
    Clips,
    Settings,
    Updates,
}

impl Page {
    const ALL: [Page; 3] = [Page::Clips, Page::Settings, Page::Updates];

    fn title(self) -> &'static str {
        match self {
            Page::Clips => "Clips",
            Page::Settings => "Settings",
            Page::Updates => "Updates",
        }
    }
}

/// The app icon as textures, active (blurple) and idle (gray).
struct Icons {
    active: TextureHandle,
    idle: TextureHandle,
}

struct ClipWindow {
    page: Page,
    clips: clips::ClipsPage,
    settings: settings::SettingsPage,
    icons: Icons,
    theme: follow::Follower,
}

impl ClipWindow {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};
        if let Ok(handle) = cc.window_handle() {
            if let RawWindowHandle::Win32(h) = handle.as_raw() {
                let hwnd = h.hwnd.get();
                shared().hwnd.store(hwnd, Ordering::SeqCst);
            }
        }
        *shared().ctx.lock().unwrap() = Some(cc.egui_ctx.clone());
        setup_fonts(&cc.egui_ctx);

        let texture = |name: &str, rgb| {
            let img = egui::ColorImage::from_rgba_unmultiplied([64, 64], &crate::icon::rgba(64, rgb));
            cc.egui_ctx.load_texture(name, img, egui::TextureOptions::LINEAR)
        };
        ClipWindow {
            page: Page::Clips,
            clips: clips::ClipsPage::default(),
            settings: settings::SettingsPage::default(),
            icons: Icons { active: texture("icon-active", BLURPLE_RGB), idle: texture("icon-idle", IDLE_RGB) },
            theme: follow::Follower::start(),
        }
    }

    /// Closing only hides the window (winit can't create a second event loop).
    fn hide(&mut self) {
        self.settings.keybind.stop(None);
        self.clips.on_hide();
        if let Some(hwnd) = window_hwnd() {
            unsafe {
                let _ = ShowWindow(hwnd, SW_HIDE);
            }
        }
    }
}

/// Sends a setting change, and shows it straight away instead of waiting
/// for the tray thread to publish it back.
fn set(cmd: Command) {
    {
        let snap = &mut *shared().snapshot.lock().unwrap();
        match &cmd {
            Command::SetSeconds(s) => snap.cfg.seconds = *s,
            Command::SetMic(m) => snap.cfg.mic = *m,
            Command::SetMicDevice(id) => snap.cfg.mic_device = id.clone(),
            Command::SetMicGain(g) => snap.cfg.mic_gain = *g,
            Command::SetClipboard(c) => snap.cfg.clipboard = *c,
            Command::SetNotifications(n) => snap.cfg.notifications = *n,
            Command::SetCheckUpdates(c) => snap.cfg.check_updates = *c,
            Command::SetSeparateTracks(s) => snap.cfg.separate_tracks = *s,
            Command::SetFormat(f) => snap.cfg.format = *f,
            Command::SetTheme(t) => snap.cfg.theme = *t,
            Command::SetAutostart(on) => {
                snap.autostart = if *on { crate::autostart::State::On } else { crate::autostart::State::Off }
            }
            _ => {}
        }
    }
    send(cmd);
}

const SIDEBAR_WIDTH: f32 = 220.0;
/// Pages of text stay this narrow: long lines are hard to read.
const READING_WIDTH: f32 = 700.0;

impl eframe::App for ClipWindow {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        pal().bg_primary.to_normalized_gamma_f32()
    }

    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        if ctx.input(|i| i.viewport().close_requested()) {
            ctx.send_viewport_cmd(ViewportCommand::CancelClose);
            self.hide();
            return;
        }
        self.settings.keybind.poll(ui);
        if let Some(page) = shared().requested_page.lock().unwrap().take() {
            self.page = page;
        }
        if let Some(clip) = shared().focus_clip.lock().unwrap().take() {
            self.clips.focus(clip);
        }
        raise(&ctx);
        let snap = shared().snapshot.lock().unwrap().clone();
        self.theme.update(&ctx, snap.cfg.theme);

        // A Nitro theme's gradient goes behind see-through panels (#39).
        let backdrop = backdrop();
        if let Some(b) = &backdrop {
            let window = ctx.content_rect();
            let split = window.left() + SIDEBAR_WIDTH;
            let (side, rest) = (window.with_max_x(split), window.with_min_x(split));
            b.paint(ui.painter(), window, side, b.sidebar);
            b.paint(ui.painter(), window, rest, b.content);
        }
        let fill = |solid: Color32| if backdrop.is_some() { Color32::TRANSPARENT } else { solid };

        egui::Panel::left("sidebar")
            .exact_size(SIDEBAR_WIDTH)
            .resizable(false)
            .show_separator_line(false)
            .frame(Frame::new().fill(fill(pal().bg_secondary)))
            .show(ui, |ui| sidebar::show(ui, &mut self.page, &snap, &self.icons));

        egui::CentralPanel::default().frame(Frame::new().fill(fill(pal().bg_primary))).show(ui, |ui| {
            if snap.restart_needed {
                settings::restart_bar(ui);
            }
            // Each page keeps its own scroll position.
            egui::ScrollArea::vertical().id_salt(self.page).auto_shrink(false).show(ui, |ui| {
                Frame::new().inner_margin(Margin { left: 40, right: 40, top: 40, bottom: 40 }).show(ui, |ui| {
                    // Text pages keep a reading width; the clips grid uses the window (#45).
                    if self.page != Page::Clips {
                        ui.set_max_width(ui.available_width().min(READING_WIDTH));
                    }
                    match self.page {
                        Page::Clips => self.clips.show(ui, &snap),
                        Page::Settings => self.settings.show(ui, &snap, &self.theme),
                        Page::Updates => updates::show(ui, &snap, &self.icons.active),
                    }
                });
            });
        });
    }
}
