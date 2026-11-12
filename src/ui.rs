//! The Discord Clip window: a sidebar of pages styled after Discord's dark
//! theme. The first (and for now only) page is Settings.
//!
//! The window runs on its own thread because winit needs to own a message
//! loop. It never touches the running app directly: anything that changes
//! state (hotkey registration, the config file, the folder picker) is sent to
//! the tray thread as a [`Command`], and the tray thread publishes a fresh
//! [`Snapshot`] back for the window to draw.

use std::ffi::c_void;
use std::ops::RangeInclusive;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use eframe::egui::{
    self, pos2, vec2, Align, Align2, Color32, CornerRadius, CursorIcon, Event, FontData,
    FontDefinitions, FontFamily, FontId, Frame, Key, Layout, Margin, Rect, RichText, Sense, Stroke,
    StrokeKind, TextureHandle, Ui, Vec2, ViewportCommand,
};
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::WindowsAndMessaging::{
    AllowSetForegroundWindow, IsIconic, SetForegroundWindow, ShowWindow, ASFW_ANY, SW_HIDE,
    SW_RESTORE, SW_SHOW,
};

use crate::config::Config;
use crate::hotkey::{Hotkey, MOD_ALT, MOD_CONTROL, MOD_SHIFT};
use crate::log;

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
    CheckUpdates,
    /// Download and install the update found by the last check.
    InstallUpdate,
    /// Open an https link in the browser.
    OpenUrl(String),
}

/// Everything the window shows, published by the tray thread.
#[derive(Clone, Default)]
pub struct Snapshot {
    pub cfg: Config,
    /// e.g. "Capturing Discord.exe (pid 1234)".
    pub discord: String,
    pub discord_active: bool,
    /// "on", "off" or "unavailable".
    pub mic: String,
    pub hotkey_registered: bool,
    pub hotkey_error: Option<String>,
    /// Clip length or microphone changed; they apply after a restart.
    pub restart_needed: bool,
    pub update: crate::update::Status,
    /// Installed copy (updates via installer) rather than a portable exe.
    pub installed: bool,
}

struct Shared {
    snapshot: Mutex<Snapshot>,
    commands: Mutex<Vec<Command>>,
    ctx: Mutex<Option<egui::Context>>,
    hwnd: AtomicIsize,
    started: AtomicBool,
    /// Page to switch to the next time the window draws.
    requested_page: Mutex<Option<Page>>,
}

fn shared() -> &'static Shared {
    static SHARED: OnceLock<Shared> = OnceLock::new();
    SHARED.get_or_init(|| Shared {
        snapshot: Mutex::new(Snapshot::default()),
        commands: Mutex::new(Vec::new()),
        ctx: Mutex::new(None),
        hwnd: AtomicIsize::new(0),
        started: AtomicBool::new(false),
        requested_page: Mutex::new(None),
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

/// Tray thread: shows the window, creating it the first time. Closing the
/// window only hides it (winit can't create a second event loop).
pub fn open(snapshot: Snapshot) {
    publish(snapshot);
    unsafe {
        // We were just clicked in the tray, so we may hand focus to the
        // window thread.
        let _ = AllowSetForegroundWindow(ASFW_ANY);
    }
    if let Some(hwnd) = window_hwnd() {
        unsafe {
            let _ = ShowWindow(hwnd, if IsIconic(hwnd).as_bool() { SW_RESTORE } else { SW_SHOW });
            let _ = SetForegroundWindow(hwnd);
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

fn run_window() -> eframe::Result {
    let icon = egui::IconData { rgba: crate::tray::icon_rgba(64, BLURPLE_RGB), width: 64, height: 64 };
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
// Discord look
// ---------------------------------------------------------------------------

const fn rgb(r: u8, g: u8, b: u8) -> Color32 {
    Color32::from_rgb(r, g, b)
}

const BLURPLE_RGB: (u8, u8, u8) = (0x58, 0x65, 0xF2);
const IDLE_RGB: (u8, u8, u8) = (0x80, 0x84, 0x8E);

const BG_PRIMARY: Color32 = rgb(0x31, 0x33, 0x38);
const BG_SECONDARY: Color32 = rgb(0x2B, 0x2D, 0x31);
const BG_SECONDARY_ALT: Color32 = rgb(0x23, 0x24, 0x28);
const BG_TERTIARY: Color32 = rgb(0x1E, 0x1F, 0x22);
const BG_FLOATING: Color32 = rgb(0x11, 0x12, 0x14);
const BG_HOVER: Color32 = rgb(0x35, 0x37, 0x3C);
const BG_SELECTED: Color32 = rgb(0x40, 0x42, 0x49);
const DIVIDER: Color32 = rgb(0x3F, 0x41, 0x47);

const HEADER_PRIMARY: Color32 = rgb(0xF2, 0xF3, 0xF5);
const HEADER_SECONDARY: Color32 = rgb(0xB5, 0xBA, 0xC1);
const TEXT_NORMAL: Color32 = rgb(0xDB, 0xDE, 0xE1);
const TEXT_MUTED: Color32 = rgb(0x94, 0x9B, 0xA4);

const BLURPLE: Color32 = rgb(0x58, 0x65, 0xF2);
const BLURPLE_HOVER: Color32 = rgb(0x47, 0x52, 0xC4);
const BLURPLE_ACTIVE: Color32 = rgb(0x3C, 0x45, 0xA5);
const GRAY_BTN: Color32 = rgb(0x4E, 0x50, 0x58);
const GRAY_BTN_HOVER: Color32 = rgb(0x6D, 0x6F, 0x78);
const GRAY_BTN_ACTIVE: Color32 = rgb(0x80, 0x84, 0x8E);
const GREEN_BTN: Color32 = rgb(0x24, 0x80, 0x46);
const GREEN_BTN_HOVER: Color32 = rgb(0x1A, 0x63, 0x34);
const GREEN_BTN_ACTIVE: Color32 = rgb(0x15, 0x56, 0x2B);
const RED_BTN: Color32 = rgb(0xDA, 0x37, 0x3C);
const RED_BTN_HOVER: Color32 = rgb(0xA1, 0x28, 0x29);
const RED_BTN_ACTIVE: Color32 = rgb(0x8F, 0x22, 0x23);

const GREEN: Color32 = rgb(0x23, 0xA5, 0x5A);
const RED: Color32 = rgb(0xF2, 0x3F, 0x43);
const YELLOW: Color32 = rgb(0xF0, 0xB2, 0x32);
const TOGGLE_OFF: Color32 = rgb(0x80, 0x84, 0x8E);

#[derive(Clone, Copy)]
enum Weight {
    Regular,
    Semibold,
    Bold,
}

fn font(size: f32, weight: Weight) -> FontId {
    let family = match weight {
        Weight::Regular => FontFamily::Proportional,
        Weight::Semibold => FontFamily::Name("semibold".into()),
        Weight::Bold => FontFamily::Name("bold".into()),
    };
    FontId::new(size, family)
}

/// Discord's own font isn't available, so use Segoe UI (falling back to
/// egui's built-in font if it's missing).
fn setup_fonts(ctx: &egui::Context) {
    let mut defs = FontDefinitions::default();
    let dir = std::env::var_os("WINDIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Windows"))
        .join("Fonts");
    let mut load = |name: &str, file: &str| match std::fs::read(dir.join(file)) {
        Ok(bytes) => {
            defs.font_data.insert(name.to_string(), Arc::new(FontData::from_owned(bytes)));
            Some(name.to_string())
        }
        Err(_) => None,
    };
    let regular = load("segoe", "segoeui.ttf");
    let semibold = load("segoe-semibold", "seguisb.ttf");
    let bold = load("segoe-bold", "segoeuib.ttf");

    let mut base = defs.families.get(&FontFamily::Proportional).cloned().unwrap_or_default();
    if let Some(r) = &regular {
        base.insert(0, r.clone());
    }
    let with_first = |first: &Option<String>| {
        let mut list = base.clone();
        if let Some(f) = first {
            list.insert(0, f.clone());
        }
        list
    };
    defs.families.insert(FontFamily::Name("semibold".into()), with_first(&semibold));
    defs.families.insert(FontFamily::Name("bold".into()), with_first(&bold));
    defs.families.insert(FontFamily::Proportional, base);
    ctx.set_fonts(defs);
}

fn setup_style(ctx: &egui::Context) {
    ctx.set_theme(egui::Theme::Dark);
    ctx.all_styles_mut(|style| {
        style.spacing.item_spacing = vec2(8.0, 0.0);
        style.spacing.scroll.bar_width = 8.0;
        style.spacing.scroll.floating = false;
        let v = &mut style.visuals;
        v.dark_mode = true;
        v.panel_fill = BG_PRIMARY;
        v.window_fill = BG_PRIMARY;
        v.extreme_bg_color = BG_TERTIARY;
        v.faint_bg_color = BG_SECONDARY;
        v.hyperlink_color = rgb(0x00, 0xA8, 0xFC);
        v.selection.bg_fill = BLURPLE;
        v.selection.stroke = Stroke::new(1.0, Color32::WHITE);
        v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, TEXT_NORMAL);
        v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, DIVIDER);
        // Scrollbar handle.
        v.widgets.inactive.bg_fill = BG_TERTIARY;
        v.widgets.hovered.bg_fill = BG_TERTIARY;
        v.widgets.active.bg_fill = BG_TERTIARY;
        v.widgets.inactive.fg_stroke = Stroke::new(1.0, TEXT_NORMAL);
    });
}

/// Dark native title bar (Windows 10 20H1+), tinted to match on Windows 11.
fn style_title_bar(hwnd: HWND) {
    use windows::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWINDOWATTRIBUTE};
    const DWMWA_USE_IMMERSIVE_DARK_MODE: i32 = 20;
    const DWMWA_CAPTION_COLOR: i32 = 35;
    let dark: i32 = 1;
    let caption: u32 = 0x0022_1F1E; // COLORREF 0x00BBGGRR of #1E1F22
    unsafe {
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWINDOWATTRIBUTE(DWMWA_USE_IMMERSIVE_DARK_MODE),
            &dark as *const i32 as *const c_void,
            4,
        );
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWINDOWATTRIBUTE(DWMWA_CAPTION_COLOR),
            &caption as *const u32 as *const c_void,
            4,
        );
    }
}

fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let l = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgb(l(a.r(), b.r()), l(a.g(), b.g()), l(a.b(), b.b()))
}

// ---------------------------------------------------------------------------
// Widgets
// ---------------------------------------------------------------------------

fn text(ui: &mut Ui, s: impl Into<String>, size: f32, weight: Weight, color: Color32) -> egui::Response {
    ui.label(RichText::new(s.into()).font(font(size, weight)).color(color))
}

fn section_label(ui: &mut Ui, s: &str) {
    text(ui, s.to_uppercase(), 12.0, Weight::Bold, HEADER_SECONDARY);
    ui.add_space(8.0);
}

fn divider(ui: &mut Ui) {
    ui.add_space(24.0);
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), Sense::hover());
    ui.painter().rect_filled(rect, 0.0, DIVIDER);
    ui.add_space(24.0);
}

#[derive(Clone, Copy)]
enum ButtonKind {
    Primary,
    Secondary,
    Success,
    Danger,
}

fn button(ui: &mut Ui, label: &str, kind: ButtonKind) -> egui::Response {
    button_sized(ui, label, kind, 32.0)
}

fn button_sized(ui: &mut Ui, label: &str, kind: ButtonKind, height: f32) -> egui::Response {
    let galley = ui.painter().layout_no_wrap(label.to_string(), font(14.0, Weight::Semibold), Color32::WHITE);
    let size = vec2((galley.size().x + 32.0).max(60.0), height);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    let (base, hover, active) = match kind {
        ButtonKind::Primary => (BLURPLE, BLURPLE_HOVER, BLURPLE_ACTIVE),
        ButtonKind::Secondary => (GRAY_BTN, GRAY_BTN_HOVER, GRAY_BTN_ACTIVE),
        ButtonKind::Success => (GREEN_BTN, GREEN_BTN_HOVER, GREEN_BTN_ACTIVE),
        ButtonKind::Danger => (RED_BTN, RED_BTN_HOVER, RED_BTN_ACTIVE),
    };
    let fill = if resp.is_pointer_button_down_on() {
        active
    } else if resp.hovered() {
        hover
    } else {
        base
    };
    let painter = ui.painter();
    painter.rect_filled(rect, CornerRadius::same(4), fill);
    painter.galley(rect.center() - galley.size() / 2.0, galley, Color32::WHITE);
    resp.on_hover_cursor(CursorIcon::PointingHand)
}

/// Discord's pill switch, with a check / cross in the knob.
fn toggle(ui: &mut Ui, on: bool) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(40.0, 24.0), Sense::click());
    let t = ui.ctx().animate_bool_responsive(resp.id, on);
    let bg = mix(TOGGLE_OFF, GREEN, t);
    let painter = ui.painter();
    painter.rect_filled(rect, CornerRadius::same(12), bg);
    let r = 9.0;
    let x = rect.left() + 3.0 + r + (rect.width() - 6.0 - 2.0 * r) * t;
    let c = pos2(x, rect.center().y);
    painter.circle_filled(c, r, Color32::WHITE);
    let stroke = Stroke::new(2.0, bg);
    if t > 0.5 {
        painter.line_segment([c + vec2(-4.0, 0.0), c + vec2(-1.0, 3.0)], stroke);
        painter.line_segment([c + vec2(-1.0, 3.0), c + vec2(4.0, -3.0)], stroke);
    } else {
        painter.line_segment([c + vec2(-3.0, -3.0), c + vec2(3.0, 3.0)], stroke);
        painter.line_segment([c + vec2(3.0, -3.0), c + vec2(-3.0, 3.0)], stroke);
    }
    resp.on_hover_cursor(CursorIcon::PointingHand)
}

/// A title + description on the left and a control on the right.
fn row<R>(ui: &mut Ui, title: &str, desc: &str, control: impl FnOnce(&mut Ui) -> R) -> R {
    let width = ui.available_width();
    ui.allocate_ui_with_layout(vec2(width, 0.0), Layout::right_to_left(Align::Min), |ui| {
        let r = control(ui);
        ui.add_space(24.0);
        ui.with_layout(Layout::top_down(Align::Min), |ui| {
            text(ui, title, 16.0, Weight::Semibold, HEADER_PRIMARY);
            if !desc.is_empty() {
                ui.add_space(4.0);
                text(ui, desc, 14.0, Weight::Regular, HEADER_SECONDARY);
            }
        });
        r
    })
    .inner
}

/// Discord-style slider: thin track, blurple fill, white grabber.
fn slider(ui: &mut Ui, value: &mut f32, range: RangeInclusive<f32>, step: f32, enabled: bool) -> egui::Response {
    let sense = if enabled { Sense::click_and_drag() } else { Sense::hover() };
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 28.0), sense);
    let (lo, hi) = (*range.start(), *range.end());
    let track = Rect::from_center_size(rect.center(), vec2(rect.width() - 12.0, 8.0));
    if enabled {
        if let Some(p) = resp.interact_pointer_pos() {
            if resp.is_pointer_button_down_on() || resp.clicked() {
                let t = ((p.x - track.left()) / track.width()).clamp(0.0, 1.0);
                *value = ((lo + t * (hi - lo)) / step).round() * step;
            }
        }
    }
    *value = value.clamp(lo, hi);
    let t = (*value - lo) / (hi - lo);
    let x = track.left() + t * track.width();
    let painter = ui.painter();
    painter.rect_filled(track, CornerRadius::same(4), GRAY_BTN);
    let fill = Rect::from_min_max(track.min, pos2(x, track.max.y));
    painter.rect_filled(fill, CornerRadius::same(4), if enabled { BLURPLE } else { TOGGLE_OFF });
    let grab = Rect::from_center_size(pos2(x, track.center().y), vec2(10.0, 24.0));
    painter.rect_filled(grab.translate(vec2(0.0, 1.0)), CornerRadius::same(3), Color32::from_black_alpha(80));
    painter.rect_filled(grab, CornerRadius::same(3), if enabled { Color32::WHITE } else { HEADER_SECONDARY });
    if enabled {
        resp.on_hover_cursor(CursorIcon::PointingHand)
    } else {
        resp
    }
}

/// Maps an egui key to a Windows virtual-key code that [`Hotkey`] can use.
fn key_to_vk(key: Key) -> Option<u32> {
    let name = key.name();
    if name.len() == 1 {
        let c = name.as_bytes()[0];
        if c.is_ascii_uppercase() || c.is_ascii_digit() {
            return Some(c as u32);
        }
    }
    if let Some(n) = name.strip_prefix('F').and_then(|n| n.parse::<u32>().ok()) {
        if (1..=24).contains(&n) {
            return Some(0x70 + n - 1);
        }
    }
    use Key::*;
    Some(match key {
        Space => 0x20,
        PageUp => 0x21,
        PageDown => 0x22,
        End => 0x23,
        Home => 0x24,
        ArrowLeft => 0x25,
        ArrowUp => 0x26,
        ArrowRight => 0x27,
        ArrowDown => 0x28,
        PrintScreen => 0x2C,
        Insert => 0x2D,
        Delete => 0x2E,
        Pause => 0x13,
        ScrollLock => 0x91,
        Backtick => 0xC0,
        Minus => 0xBD,
        Equals | Plus => 0xBB,
        OpenBracket => 0xDB,
        CloseBracket => 0xDD,
        Backslash => 0xDC,
        Semicolon => 0xBA,
        Quote => 0xDE,
        Comma => 0xBC,
        Period => 0xBE,
        Slash => 0xBF,
        MediaPlayPause => 0xB3,
        MediaTrackNext => 0xB0,
        MediaTrackPrevious => 0xB1,
        MediaStop => 0xB2,
        _ => return None,
    })
}

/// A status message, optionally with a colored dot in front.
fn status_line(ui: &mut Ui, dot: Option<Color32>, msg: &str, color: Color32) {
    ui.horizontal(|ui| {
        if let Some(c) = dot {
            let (rect, _) = ui.allocate_exact_size(vec2(10.0, 20.0), Sense::hover());
            ui.painter().circle_filled(rect.center(), 5.0, c);
            ui.add_space(2.0);
        }
        text(ui, msg, 15.0, Weight::Semibold, color);
    });
}

fn progress_bar(ui: &mut Ui, frac: f32) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 8.0), Sense::hover());
    let painter = ui.painter();
    painter.rect_filled(rect, CornerRadius::same(4), BG_TERTIARY);
    let fill = Rect::from_min_max(rect.min, pos2(rect.left() + rect.width() * frac.clamp(0.0, 1.0), rect.max.y));
    painter.rect_filled(fill, CornerRadius::same(4), BLURPLE);
}

/// "just now", "5 minutes ago", "3 hours ago", "2 days ago".
fn ago(t: std::time::SystemTime) -> String {
    let secs = t.elapsed().map(|d| d.as_secs()).unwrap_or(0);
    let plural = |n: u64, unit: &str| format!("{n} {unit}{} ago", if n == 1 { "" } else { "s" });
    match secs {
        0..=59 => "just now".to_string(),
        60..=3599 => plural(secs / 60, "minute"),
        3600..=86399 => plural(secs / 3600, "hour"),
        _ => plural(secs / 86400, "day"),
    }
}

// ---------------------------------------------------------------------------
// The window
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Page {
    Settings,
    Updates,
}

impl Page {
    const ALL: [Page; 2] = [Page::Settings, Page::Updates];

    fn title(self) -> &'static str {
        match self {
            Page::Settings => "Settings",
            Page::Updates => "Updates",
        }
    }
}

struct ClipWindow {
    page: Page,
    /// Waiting for the user to press a new hotkey.
    recording: bool,
    record_hint: Option<String>,
    /// Slider values while dragging (committed on release).
    seconds_drag: Option<f32>,
    gain_drag: Option<f32>,
    icon_active: TextureHandle,
    icon_idle: TextureHandle,
}

impl ClipWindow {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};
        if let Ok(handle) = cc.window_handle() {
            if let RawWindowHandle::Win32(h) = handle.as_raw() {
                let hwnd = h.hwnd.get();
                shared().hwnd.store(hwnd, Ordering::SeqCst);
                style_title_bar(HWND(hwnd as *mut c_void));
            }
        }
        *shared().ctx.lock().unwrap() = Some(cc.egui_ctx.clone());
        setup_fonts(&cc.egui_ctx);
        setup_style(&cc.egui_ctx);

        let texture = |name: &str, rgb| {
            let img = egui::ColorImage::from_rgba_unmultiplied([64, 64], &crate::tray::icon_rgba(64, rgb));
            cc.egui_ctx.load_texture(name, img, egui::TextureOptions::LINEAR)
        };
        ClipWindow {
            page: Page::Settings,
            recording: false,
            record_hint: None,
            seconds_drag: None,
            gain_drag: None,
            icon_active: texture("icon-active", BLURPLE_RGB),
            icon_idle: texture("icon-idle", IDLE_RGB),
        }
    }

    fn hide(&mut self) {
        self.stop_recording(None);
        if let Some(hwnd) = window_hwnd() {
            unsafe {
                let _ = ShowWindow(hwnd, SW_HIDE);
            }
        }
    }

    fn start_recording(&mut self) {
        if !self.recording {
            self.recording = true;
            self.record_hint = None;
            send(Command::BeginHotkeyCapture);
        }
    }

    fn stop_recording(&mut self, result: Option<Hotkey>) {
        if self.recording {
            self.recording = false;
            self.record_hint = None;
            send(Command::EndHotkeyCapture(result));
        }
    }

    /// While recording, the next key combination becomes the hotkey.
    fn poll_recording(&mut self, ui: &Ui) {
        if !self.recording {
            return;
        }
        let ctrl = egui::Modifiers::CTRL;
        let (events, focused) = ui.input(|i| (i.events.clone(), i.viewport().focused));
        if focused == Some(false) {
            self.stop_recording(None);
            return;
        }
        for event in events {
            // Ctrl+C/X/V arrive as clipboard events, not key presses.
            let (key, m) = match event {
                Event::Key { key, pressed: true, modifiers, .. } => (key, modifiers),
                Event::Copy => (Key::C, ctrl),
                Event::Cut => (Key::X, ctrl),
                Event::Paste(_) => (Key::V, ctrl),
                _ => continue,
            };
            if key == Key::Escape && !m.any() {
                self.stop_recording(None);
                return;
            }
            let Some(vk) = key_to_vk(key) else { continue };
            let mut mods = 0;
            if m.ctrl {
                mods |= MOD_CONTROL;
            }
            if m.alt {
                mods |= MOD_ALT;
            }
            if m.shift {
                mods |= MOD_SHIFT;
            }
            let hk = Hotkey { mods, vk };
            match hk.check_sensible() {
                Ok(()) => {
                    self.stop_recording(Some(hk));
                    return;
                }
                Err(e) => self.record_hint = Some(e),
            }
        }
    }

    // --- sidebar ---------------------------------------------------------

    fn sidebar(&mut self, ui: &mut Ui, snap: &Snapshot) {
        egui::Panel::bottom("status")
            .exact_size(52.0)
            .resizable(false)
            .show_separator_line(false)
            .frame(Frame::new().fill(BG_SECONDARY_ALT).inner_margin(Margin::symmetric(8, 0)))
            .show(ui, |ui| self.status_panel(ui, snap));

        Frame::new().inner_margin(Margin { left: 10, right: 10, top: 24, bottom: 8 }).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.add_space(10.0);
                text(ui, "DISCORD CLIP", 12.0, Weight::Bold, TEXT_MUTED);
            });
            ui.add_space(8.0);
            for page in Page::ALL {
                let badge = page == Page::Updates
                    && matches!(
                        snap.update.state,
                        crate::update::State::Available(_)
                            | crate::update::State::Downloading { .. }
                            | crate::update::State::Ready { .. }
                    );
                if self.nav_item(ui, page.title(), self.page == page, badge).clicked() {
                    self.page = page;
                }
                ui.add_space(2.0);
            }
        });
    }

    /// `badge`: Discord's red notification pill on the right.
    fn nav_item(&self, ui: &mut Ui, label: &str, selected: bool, badge: bool) -> egui::Response {
        let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 32.0), Sense::click());
        let (fill, color) = if selected {
            (BG_SELECTED, Color32::WHITE)
        } else if resp.hovered() {
            (BG_HOVER, TEXT_NORMAL)
        } else {
            (Color32::TRANSPARENT, HEADER_SECONDARY)
        };
        let painter = ui.painter();
        painter.rect_filled(rect, CornerRadius::same(4), fill);
        painter.text(
            pos2(rect.left() + 10.0, rect.center().y),
            Align2::LEFT_CENTER,
            label,
            font(16.0, Weight::Semibold),
            color,
        );
        if badge {
            let pill = Rect::from_center_size(pos2(rect.right() - 18.0, rect.center().y), vec2(16.0, 16.0));
            painter.rect_filled(pill, CornerRadius::same(8), RED);
            painter.text(pill.center(), Align2::CENTER_CENTER, "1", font(12.0, Weight::Bold), Color32::WHITE);
        }
        resp.on_hover_cursor(CursorIcon::PointingHand)
    }

    /// Like Discord's user panel: avatar with a status dot, name and status.
    fn status_panel(&self, ui: &mut Ui, snap: &Snapshot) {
        let (status, dot) = if snap.discord_active {
            ("Capturing Discord", GREEN)
        } else if snap.discord.starts_with("error") {
            ("Capture error", RED)
        } else {
            ("Waiting for Discord", TOGGLE_OFF)
        };
        let width = ui.available_width();
        let (rect, resp) = ui.allocate_exact_size(vec2(width, 52.0), Sense::hover());
        let painter = ui.painter();
        let avatar = Rect::from_center_size(pos2(rect.left() + 18.0, rect.center().y), Vec2::splat(32.0));
        let tex = if snap.discord_active { &self.icon_active } else { &self.icon_idle };
        painter.image(tex.id(), avatar, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
        let dot_c = avatar.right_bottom() - vec2(4.0, 4.0);
        painter.circle_filled(dot_c, 7.0, BG_SECONDARY_ALT);
        painter.circle_filled(dot_c, 5.0, dot);
        let x = avatar.right() + 8.0;
        painter.text(pos2(x, rect.center().y - 1.0), Align2::LEFT_BOTTOM, "Discord Clip", font(14.0, Weight::Semibold), HEADER_PRIMARY);
        let mic = match snap.mic.as_str() {
            "on" => "mic on",
            "off" => "mic off",
            _ => "mic unavailable",
        };
        painter.text(
            pos2(x, rect.center().y + 1.0),
            Align2::LEFT_TOP,
            format!("{status} · {mic}"),
            font(12.0, Weight::Regular),
            HEADER_SECONDARY,
        );
        resp.on_hover_text(&snap.discord);
    }

    // --- settings page ---------------------------------------------------

    fn settings_page(&mut self, ui: &mut Ui, snap: &Snapshot) {
        let cfg = &snap.cfg;
        text(ui, "Settings", 20.0, Weight::Bold, HEADER_PRIMARY);
        ui.add_space(20.0);

        // Hotkey
        section_label(ui, "Hotkey");
        self.keybind(ui, snap);
        ui.add_space(8.0);
        let hint = if let Some(h) = self.record_hint.clone().filter(|_| self.recording) {
            (h, YELLOW)
        } else if let Some(e) = snap.hotkey_error.clone().filter(|_| !self.recording) {
            (e, RED)
        } else if !snap.hotkey_registered && !self.recording {
            (format!("{} is already used by another app. Pick a different combination.", cfg.hotkey), RED)
        } else {
            (
                format!(
                    "Saves the last {}s of Discord audio. Use Ctrl, Alt or Shift with a key, or an F-key on its own.",
                    cfg.seconds
                ),
                HEADER_SECONDARY,
            )
        };
        text(ui, hint.0, 14.0, Weight::Regular, hint.1);

        divider(ui);

        // Recording
        section_label(ui, "Recording");
        let mut secs = self.seconds_drag.unwrap_or(cfg.seconds as f32);
        row(ui, "Clip length", "How much audio each clip contains. Applies after a restart.", |ui| {
            text(ui, format!("{} s", secs.round() as u32), 16.0, Weight::Semibold, HEADER_PRIMARY)
        });
        ui.add_space(8.0);
        let r = slider(ui, &mut secs, 5.0..=300.0, 5.0, true);
        if r.is_pointer_button_down_on() {
            self.seconds_drag = Some(secs);
        }
        if r.drag_stopped() || r.clicked() {
            self.seconds_drag = None;
            set(Command::SetSeconds(secs.round() as u32));
        }
        ui.add_space(6.0);
        let bytes = secs.round() as u64 * (crate::capture::SAMPLE_RATE as u64) * (crate::capture::CHANNELS as u64) * 2;
        let mb = bytes as f32 / 1_000_000.0;
        if bytes > 10 * 1024 * 1024 {
            text(ui, format!("≈ {mb:.1} MB per clip: over Discord's 10 MB upload limit without Nitro."), 14.0, Weight::Regular, YELLOW);
        } else {
            text(ui, format!("≈ {mb:.1} MB per clip."), 14.0, Weight::Regular, TEXT_MUTED);
        }

        ui.add_space(24.0);
        let mic_desc = if snap.mic == "unavailable" {
            "Mix your own voice into clips. No microphone was found when the app started."
        } else {
            "Mix your own voice (your default communications microphone) into clips. Applies after a restart."
        };
        if row(ui, "Record my microphone", mic_desc, |ui| toggle(ui, cfg.mic)).clicked() {
            set(Command::SetMic(!cfg.mic));
        }

        ui.add_space(24.0);
        let mut gain = self.gain_drag.unwrap_or(cfg.mic_gain.min(4.0));
        row(ui, "Microphone volume", "How loud your voice is in clips, relative to everyone else.", |ui| {
            text(ui, format!("{}%", (gain * 100.0).round() as u32), 16.0, Weight::Semibold, HEADER_PRIMARY)
        });
        ui.add_space(8.0);
        let r = slider(ui, &mut gain, 0.0..=4.0, 0.05, cfg.mic);
        if r.is_pointer_button_down_on() {
            self.gain_drag = Some(gain);
        }
        if r.drag_stopped() || r.clicked() {
            self.gain_drag = None;
            set(Command::SetMicGain(gain));
        }

        divider(ui);

        // Saving
        section_label(ui, "Clips folder");
        self.folder_field(ui, cfg);
        ui.add_space(24.0);
        if row(
            ui,
            "Copy clips to the clipboard",
            "Press Ctrl+V in a Discord chat right after saving to send the clip.",
            |ui| toggle(ui, cfg.clipboard),
        )
        .clicked()
        {
            set(Command::SetClipboard(!cfg.clipboard));
        }
        ui.add_space(24.0);
        if row(
            ui,
            "Show notifications",
            "Pop up a notification each time a clip is saved. Problems are always shown.",
            |ui| toggle(ui, cfg.notifications),
        )
        .clicked()
        {
            set(Command::SetNotifications(!cfg.notifications));
        }

        divider(ui);

        // Advanced
        section_label(ui, "Advanced");
        text(
            ui,
            "Settings are stored in config.ini. If you edit it by hand, reload it to apply your changes.",
            14.0,
            Weight::Regular,
            HEADER_SECONDARY,
        );
        ui.add_space(12.0);
        ui.horizontal(|ui| {
            if button(ui, "Open settings file", ButtonKind::Secondary).clicked() {
                send(Command::OpenConfigFile);
            }
            if button(ui, "Reload settings file", ButtonKind::Secondary).clicked() {
                send(Command::Restart);
            }
            if button(ui, "Open log", ButtonKind::Secondary).clicked() {
                send(Command::OpenLog);
            }
        });
    }

    /// Discord's keybind recorder: the combo in a dark box with an
    /// "Edit Keybind" button that turns into a red "Stop Recording".
    fn keybind(&mut self, ui: &mut Ui, snap: &Snapshot) {
        let width = ui.available_width().min(420.0);
        let (rect, resp) = ui.allocate_exact_size(vec2(width, 44.0), Sense::click());
        let painter = ui.painter();
        painter.rect_filled(rect, CornerRadius::same(4), BG_TERTIARY);
        if self.recording {
            painter.rect_stroke(rect, CornerRadius::same(4), Stroke::new(1.0, RED), StrokeKind::Inside);
            painter.rect_stroke(rect.expand(2.0), CornerRadius::same(6), Stroke::new(2.0, RED.gamma_multiply(0.3)), StrokeKind::Inside);
        } else if resp.hovered() {
            painter.rect_stroke(rect, CornerRadius::same(4), Stroke::new(1.0, BG_FLOATING), StrokeKind::Inside);
        }
        let (label, color) = if self.recording {
            ("Press a key combination…".to_string(), RED)
        } else {
            (snap.cfg.hotkey.to_string(), HEADER_PRIMARY)
        };
        painter.text(pos2(rect.left() + 12.0, rect.center().y), Align2::LEFT_CENTER, label, font(16.0, Weight::Semibold), color);

        // Button inside the box, on the right.
        let (btn_label, kind) = if self.recording {
            ("Stop Recording", ButtonKind::Danger)
        } else {
            ("Edit Keybind", ButtonKind::Secondary)
        };
        let btn_galley = painter.layout_no_wrap(btn_label.to_string(), font(14.0, Weight::Semibold), Color32::WHITE);
        let btn_w = (btn_galley.size().x + 32.0).max(60.0); // same as button_sized
        let btn_rect = Rect::from_min_max(pos2(rect.right() - 6.0 - btn_w, rect.top() + 6.0), pos2(rect.right() - 6.0, rect.bottom() - 6.0));
        let mut btn_ui = ui.new_child(egui::UiBuilder::new().max_rect(btn_rect));
        let btn = button_sized(&mut btn_ui, btn_label, kind, btn_rect.height());

        if btn.clicked() || resp.clicked() {
            if self.recording {
                self.stop_recording(None);
            } else {
                self.start_recording();
            }
        }
        resp.on_hover_cursor(CursorIcon::PointingHand);
    }

    fn folder_field(&self, ui: &mut Ui, cfg: &Config) {
        let width = ui.available_width();
        ui.allocate_ui_with_layout(vec2(width, 40.0), Layout::right_to_left(Align::Center), |ui| {
            if button_sized(ui, "Open", ButtonKind::Secondary, 40.0).clicked() {
                send(Command::OpenFolder);
            }
            if button_sized(ui, "Change", ButtonKind::Primary, 40.0).clicked() {
                send(Command::PickFolder);
            }
            let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 40.0), Sense::hover());
            let path = cfg.out_dir.display().to_string();
            let painter = ui.painter();
            painter.rect_filled(rect, CornerRadius::same(4), BG_TERTIARY);
            let inner = rect.shrink2(vec2(10.0, 0.0));
            let galley = painter.layout_no_wrap(path.clone(), font(15.0, Weight::Regular), TEXT_NORMAL);
            // Too long: show the end of the path, which is the useful part.
            let x = if galley.size().x > inner.width() { inner.right() - galley.size().x } else { inner.left() };
            painter
                .with_clip_rect(inner)
                .galley(pos2(x, rect.center().y - galley.size().y / 2.0), galley, TEXT_NORMAL);
            resp.on_hover_text(path);
        });
    }

    // --- updates page ----------------------------------------------------

    fn updates_page(&mut self, ui: &mut Ui, snap: &Snapshot) {
        use crate::update::{State, CURRENT};

        text(ui, "Updates", 20.0, Weight::Bold, HEADER_PRIMARY);
        ui.add_space(20.0);

        // Card: app, version and what's going on.
        Frame::new()
            .fill(BG_SECONDARY)
            .corner_radius(CornerRadius::same(8))
            .inner_margin(Margin::same(16))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    let (rect, _) = ui.allocate_exact_size(Vec2::splat(48.0), Sense::hover());
                    let uv = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
                    ui.painter().image(self.icon_active.id(), rect, uv, Color32::WHITE);
                    ui.add_space(8.0);
                    ui.vertical(|ui| {
                        ui.add_space(4.0);
                        text(ui, "Discord Clip", 16.0, Weight::Semibold, HEADER_PRIMARY);
                        ui.add_space(2.0);
                        let kind = if snap.installed { "installed" } else { "portable" };
                        text(ui, format!("Version {CURRENT} · {kind}"), 14.0, Weight::Regular, HEADER_SECONDARY);
                    });
                });
                ui.add_space(16.0);

                let checked = snap
                    .update
                    .last_checked
                    .map(|t| format!("Last checked {}.", ago(t)))
                    .unwrap_or_default();
                match &snap.update.state {
                    State::Idle => {
                        status_line(ui, None, "Not checked for updates yet.", TEXT_NORMAL);
                        ui.add_space(12.0);
                        if button(ui, "Check for updates", ButtonKind::Primary).clicked() {
                            send(Command::CheckUpdates);
                        }
                    }
                    State::Checking => {
                        ui.horizontal(|ui| {
                            ui.add(egui::Spinner::new().size(16.0).color(BLURPLE));
                            ui.add_space(4.0);
                            text(ui, "Checking for updates…", 15.0, Weight::Semibold, TEXT_NORMAL);
                        });
                    }
                    State::UpToDate => {
                        status_line(ui, Some(GREEN), "You're up to date.", TEXT_NORMAL);
                        ui.add_space(4.0);
                        text(ui, checked, 14.0, Weight::Regular, TEXT_MUTED);
                        ui.add_space(12.0);
                        if button(ui, "Check again", ButtonKind::Secondary).clicked() {
                            send(Command::CheckUpdates);
                        }
                    }
                    State::Available(m) => {
                        status_line(ui, Some(BLURPLE), &format!("Version {} is available.", m.version), HEADER_PRIMARY);
                        ui.add_space(4.0);
                        let released = m.published.get(..10).map(|d| format!("Released {d}. ")).unwrap_or_default();
                        let how = if snap.installed {
                            "Discord Clip closes for a few seconds while it installs, then starts again."
                        } else {
                            "This exe is replaced with the new version and restarted."
                        };
                        text(ui, format!("{released}{how}"), 14.0, Weight::Regular, HEADER_SECONDARY);
                        ui.add_space(12.0);
                        ui.horizontal(|ui| {
                            if button(ui, "Update now", ButtonKind::Success).clicked() {
                                send(Command::InstallUpdate);
                            }
                            if !m.notes_url.is_empty() && button(ui, "Release notes", ButtonKind::Secondary).clicked() {
                                send(Command::OpenUrl(m.notes_url.clone()));
                            }
                        });
                    }
                    State::Downloading { manifest, done, total } => {
                        text(ui, format!("Downloading version {}…", manifest.version), 15.0, Weight::Semibold, TEXT_NORMAL);
                        ui.add_space(10.0);
                        let frac = if *total > 0 { *done as f32 / *total as f32 } else { 0.0 };
                        progress_bar(ui, frac);
                        ui.add_space(6.0);
                        text(
                            ui,
                            format!("{:.1} of {:.1} MB", *done as f32 / 1e6, *total as f32 / 1e6),
                            14.0,
                            Weight::Regular,
                            TEXT_MUTED,
                        );
                    }
                    State::Ready { manifest, .. } => {
                        ui.horizontal(|ui| {
                            ui.add(egui::Spinner::new().size(16.0).color(GREEN));
                            ui.add_space(4.0);
                            text(ui, format!("Installing version {}…", manifest.version), 15.0, Weight::Semibold, TEXT_NORMAL);
                        });
                    }
                    State::Failed { error, manifest } => {
                        let what = if manifest.is_some() { "Couldn't install the update." } else { "Couldn't check for updates." };
                        status_line(ui, Some(RED), what, HEADER_PRIMARY);
                        ui.add_space(4.0);
                        text(ui, error, 14.0, Weight::Regular, HEADER_SECONDARY);
                        ui.add_space(12.0);
                        ui.horizontal(|ui| {
                            if manifest.is_some() {
                                if button(ui, "Try again", ButtonKind::Success).clicked() {
                                    send(Command::InstallUpdate);
                                }
                            } else if button(ui, "Try again", ButtonKind::Primary).clicked() {
                                send(Command::CheckUpdates);
                            }
                            if button(ui, "Open downloads page", ButtonKind::Secondary).clicked() {
                                send(Command::OpenUrl(crate::update::RELEASES_URL.to_string()));
                            }
                        });
                    }
                }
            });

        divider(ui);

        section_label(ui, "Automatic updates");
        if row(
            ui,
            "Check for updates automatically",
            "Looks for a new version when Discord Clip starts and every few hours, and lets you know. \
             Nothing is installed until you click Update now.",
            |ui| toggle(ui, snap.cfg.check_updates),
        )
        .clicked()
        {
            set(Command::SetCheckUpdates(!snap.cfg.check_updates));
        }
    }

    /// Floating bar like Discord's "You have unsaved changes".
    fn restart_bar(&self, ui: &mut Ui) {
        egui::Panel::bottom("restart")
            .resizable(false)
            .show_separator_line(false)
            .frame(Frame::new().inner_margin(Margin { left: 40, right: 40, top: 0, bottom: 20 }))
            .show(ui, |ui| {
                ui.set_max_width(ui.available_width().min(700.0));
                Frame::new()
                    .fill(BG_FLOATING)
                    .corner_radius(CornerRadius::same(6))
                    .inner_margin(Margin { left: 16, right: 10, top: 10, bottom: 10 })
                    .show(ui, |ui| {
                        let width = ui.available_width();
                        ui.allocate_ui_with_layout(vec2(width, 32.0), Layout::right_to_left(Align::Center), |ui| {
                            if button(ui, "Restart now", ButtonKind::Success).clicked() {
                                send(Command::Restart);
                            }
                            ui.add_space(8.0);
                            ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                                text(
                                    ui,
                                    "Restart to apply your clip length and microphone changes.",
                                    15.0,
                                    Weight::Semibold,
                                    TEXT_NORMAL,
                                );
                            });
                        });
                    });
            });
    }
}

/// Sends a setting change, and shows it straight away instead of waiting
/// for the tray thread to publish it back.
fn set(cmd: Command) {
    {
        let mut snap = shared().snapshot.lock().unwrap();
        let cfg = &mut snap.cfg;
        match &cmd {
            Command::SetSeconds(s) => cfg.seconds = *s,
            Command::SetMic(m) => cfg.mic = *m,
            Command::SetMicGain(g) => cfg.mic_gain = *g,
            Command::SetClipboard(c) => cfg.clipboard = *c,
            Command::SetNotifications(n) => cfg.notifications = *n,
            Command::SetCheckUpdates(c) => cfg.check_updates = *c,
            _ => {}
        }
    }
    send(cmd);
}

impl eframe::App for ClipWindow {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        BG_PRIMARY.to_normalized_gamma_f32()
    }

    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        if ctx.input(|i| i.viewport().close_requested()) {
            ctx.send_viewport_cmd(ViewportCommand::CancelClose);
            self.hide();
            return;
        }
        self.poll_recording(ui);
        if let Some(page) = shared().requested_page.lock().unwrap().take() {
            self.page = page;
        }
        let snap = shared().snapshot.lock().unwrap().clone();

        egui::Panel::left("sidebar")
            .exact_size(220.0)
            .resizable(false)
            .show_separator_line(false)
            .frame(Frame::new().fill(BG_SECONDARY))
            .show(ui, |ui| self.sidebar(ui, &snap));

        egui::CentralPanel::default().frame(Frame::new().fill(BG_PRIMARY)).show(ui, |ui| {
            if snap.restart_needed {
                self.restart_bar(ui);
            }
            egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
                Frame::new().inner_margin(Margin { left: 40, right: 40, top: 40, bottom: 40 }).show(ui, |ui| {
                    ui.set_max_width(ui.available_width().min(700.0));
                    match self.page {
                        Page::Settings => self.settings_page(ui, &snap),
                        Page::Updates => self.updates_page(ui, &snap),
                    }
                });
            });
        });
    }
}
