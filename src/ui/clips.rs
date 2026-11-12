//! The Clips page: the clips folder as a grid of cards, each with a
//! waveform player (play/pause, click or drag on the waveform to scrub) and
//! Copy / Show / Delete buttons.
//!
//! Listing the folder is cheap and redone every couple of seconds while the
//! page is open, so new clips appear on their own. Reading a clip's waveform
//! means reading the whole file, so that happens on a worker thread and the
//! card shows a placeholder until it's done.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use eframe::egui::{
    self, pos2, vec2, Align, Align2, Color32, CornerRadius, CursorIcon, Frame, Layout, Margin, Painter, Rect, Sense,
    Stroke, Ui,
};

use super::theme::*;
use super::widgets::*;
use super::{send, Command, Snapshot};
use crate::library::{self, Analysis, ClipFile, SavedAt};
use crate::player::Playback;

const RESCAN_EVERY: Duration = Duration::from_secs(2);
const TILE_MIN_WIDTH: f32 = 250.0;
const TILE_HEIGHT: f32 = 186.0;
const GAP: f32 = 16.0;
/// Bytes per second of our clips (48 kHz, stereo, 16-bit), to estimate
/// length before a clip is analysed.
const BYTES_PER_SEC: f32 = 48_000.0 * 2.0 * 2.0;

type Analyses = Arc<Mutex<HashMap<PathBuf, (SystemTime, Result<Analysis, String>)>>>;

#[derive(Default)]
pub struct ClipsPage {
    dir: Option<PathBuf>,
    clips: Vec<ClipFile>,
    last_scan: Option<Instant>,
    analyses: Analyses,
    /// Sent to the worker (path, modified) and not re-sent until it changes.
    queued: HashMap<PathBuf, SystemTime>,
    worker: Option<Sender<(PathBuf, SystemTime)>>,
    /// The clip that's playing or paused (only one at a time).
    player: Option<Playback>,
    /// Deleted from the page; hidden until the next scan confirms it.
    deleted: HashSet<PathBuf>,
    toast: Option<(String, Instant)>,
}

enum Action {
    /// Play button: play, pause or resume.
    Toggle,
    /// Clicked / let go on the waveform at this point (0..=1).
    Seek(f32),
    Copy,
    Show,
    Delete,
}

/// How a card's player looks this frame.
#[derive(Clone, Copy, Default)]
struct PlayerView {
    /// This clip is loaded in the player.
    active: bool,
    playing: bool,
    /// Playback position, 0..=1.
    position: f32,
}

impl ClipsPage {
    /// Stops playback (e.g. when the window is hidden).
    pub fn stop(&mut self) {
        // Dropping the playback stops the device.
        self.player = None;
    }

    fn is_loaded(&self, clip: &ClipFile) -> bool {
        self.player.as_ref().is_some_and(|p| p.path() == clip.path)
    }

    pub fn show(&mut self, ui: &mut Ui, snap: &Snapshot) {
        self.refresh(ui.ctx(), &snap.cfg.out_dir);
        self.update_playing(ui.ctx());

        let visible: Vec<ClipFile> = self.clips.iter().filter(|c| !self.deleted.contains(&c.path)).cloned().collect();

        // Header
        let width = ui.available_width();
        ui.allocate_ui_with_layout(vec2(width, 0.0), Layout::right_to_left(Align::Min), |ui| {
            if button(ui, "Open folder", ButtonKind::Secondary).clicked() {
                send(Command::OpenFolder);
            }
            ui.add_space(16.0);
            ui.with_layout(Layout::top_down(Align::Min), |ui| {
                text(ui, "Clips", 20.0, Weight::Bold, HEADER_PRIMARY);
                ui.add_space(4.0);
                let total: u64 = visible.iter().map(|c| c.size).sum();
                let summary = match visible.len() {
                    0 => format!("Saved in {}", snap.cfg.out_dir.display()),
                    1 => format!("1 clip · {} · {}", megabytes(total), snap.cfg.out_dir.display()),
                    n => format!("{n} clips · {} · {}", megabytes(total), snap.cfg.out_dir.display()),
                };
                text(ui, summary, 14.0, Weight::Regular, HEADER_SECONDARY);
            });
        });
        ui.add_space(20.0);

        if visible.is_empty() {
            empty_state(ui, snap);
        } else {
            self.grid(ui, &visible);
        }

        self.show_toast(ui);
    }

    /// Rescans the folder now and then, and queues new clips for analysis.
    fn refresh(&mut self, ctx: &egui::Context, dir: &Path) {
        let dir_changed = self.dir.as_deref() != Some(dir);
        if dir_changed || self.last_scan.is_none_or(|t| t.elapsed() >= RESCAN_EVERY) {
            self.dir = Some(dir.to_path_buf());
            self.clips = library::scan(dir);
            self.last_scan = Some(Instant::now());
            let present: HashSet<&PathBuf> = self.clips.iter().map(|c| &c.path).collect();
            self.deleted.retain(|p| present.contains(p));
            self.queued.retain(|p, _| present.contains(p));
            self.analyses.lock().unwrap().retain(|p, _| present.contains(p));
        }
        // Keep polling the folder while the page is open.
        ctx.request_repaint_after(RESCAN_EVERY);

        let worker = self.worker.get_or_insert_with(|| spawn_worker(ctx.clone(), Arc::clone(&self.analyses)));
        for clip in &self.clips {
            if self.queued.get(&clip.path) != Some(&clip.modified) {
                self.queued.insert(clip.path.clone(), clip.modified);
                let _ = worker.send((clip.path.clone(), clip.modified));
            }
        }
    }

    /// At the end of a clip, go back to the start like Discord's player.
    fn update_playing(&mut self, ctx: &egui::Context) {
        let Some(p) = &self.player else { return };
        if p.finished() {
            self.player = None;
        } else if !p.is_paused() {
            // Smooth playhead.
            ctx.request_repaint_after(Duration::from_millis(16));
        }
    }

    fn grid(&mut self, ui: &mut Ui, clips: &[ClipFile]) {
        let width = ui.available_width();
        let cols = (((width + GAP) / (TILE_MIN_WIDTH + GAP)).floor() as usize).max(1);
        let tile_w = (width - GAP * (cols - 1) as f32) / cols as f32;
        let analyses = self.analyses.lock().unwrap().clone();
        let today = today();

        for row in clips.chunks(cols) {
            let (row_rect, _) = ui.allocate_exact_size(vec2(width, TILE_HEIGHT), Sense::hover());
            for (i, clip) in row.iter().enumerate() {
                let x = row_rect.left() + i as f32 * (tile_w + GAP);
                let rect = Rect::from_min_size(pos2(x, row_rect.top()), vec2(tile_w, TILE_HEIGHT));
                let analysis = analyses
                    .get(&clip.path)
                    .filter(|(modified, _)| *modified == clip.modified)
                    .map(|(_, a)| a);
                let view = match &self.player {
                    Some(p) if p.path() == clip.path => {
                        PlayerView { active: true, playing: !p.is_paused(), position: p.position() }
                    }
                    _ => PlayerView::default(),
                };
                if let Some(action) = tile(ui, rect, clip, analysis, view, today) {
                    self.act(action, clip);
                }
            }
            ui.add_space(GAP);
        }
    }

    fn act(&mut self, action: Action, clip: &ClipFile) {
        let result = match action {
            Action::Toggle => match &mut self.player {
                Some(p) if p.path() == clip.path => {
                    if p.is_paused() {
                        p.resume()
                    } else {
                        p.pause();
                        Ok(())
                    }
                }
                _ => self.load(clip, 0.0),
            },
            // Seeking a loaded clip keeps it playing or paused; seeking
            // another one starts it there.
            Action::Seek(at) => match &mut self.player {
                Some(p) if p.path() == clip.path => p.seek(at),
                _ => self.load(clip, at),
            },
            Action::Copy => {
                send(Command::CopyClip(clip.path.clone()));
                self.toast = Some(("Copied. Paste it into a Discord chat with Ctrl+V.".into(), Instant::now()));
                Ok(())
            }
            Action::Show => {
                send(Command::ShowClip(clip.path.clone()));
                Ok(())
            }
            Action::Delete => {
                if self.is_loaded(clip) {
                    self.stop();
                }
                self.deleted.insert(clip.path.clone());
                send(Command::DeleteClip(clip.path.clone()));
                self.toast = Some(("Moved to the Recycle Bin.".into(), Instant::now()));
                Ok(())
            }
        };
        if let Err(e) = result {
            crate::log!("playback failed: {e:#}");
            self.toast = Some((format!("Couldn't play the clip: {e:#}"), Instant::now()));
        }
    }

    /// Loads `clip` into the player (stopping any other) and plays from `at`.
    fn load(&mut self, clip: &ClipFile, at: f32) -> anyhow::Result<()> {
        self.player = None;
        self.player = Some(Playback::start(&clip.path, at)?);
        Ok(())
    }

    /// A short message floating at the bottom, like Discord's toasts.
    fn show_toast(&mut self, ui: &mut Ui) {
        let Some((msg, at)) = &self.toast else { return };
        let age = at.elapsed().as_secs_f32();
        if age > 3.0 {
            self.toast = None;
            return;
        }
        ui.ctx().request_repaint_after(Duration::from_millis(100));
        let alpha = ((3.0 - age) / 0.3).clamp(0.0, 1.0);
        let screen = ui.ctx().content_rect();
        egui::Area::new(egui::Id::unique("clips-toast"))
            .order(egui::Order::Foreground)
            .anchor(Align2::CENTER_BOTTOM, vec2(110.0, -24.0))
            .interactable(false)
            .show(ui.ctx(), |ui| {
                ui.set_max_width(screen.width() - 280.0);
                ui.multiply_opacity(alpha);
                Frame::new()
                    .fill(BG_FLOATING)
                    .corner_radius(CornerRadius::same(6))
                    .inner_margin(Margin::symmetric(14, 10))
                    .show(ui, |ui| {
                        text(ui, msg.as_str(), 14.0, Weight::Semibold, TEXT_NORMAL);
                    });
            });
    }
}

fn spawn_worker(ctx: egui::Context, analyses: Analyses) -> Sender<(PathBuf, SystemTime)> {
    let (tx, rx) = mpsc::channel::<(PathBuf, SystemTime)>();
    let _ = std::thread::Builder::new().name("clip-analysis".into()).spawn(move || {
        for (path, modified) in rx {
            let result = library::analyze(&path).map_err(|e| format!("{e:#}"));
            analyses.lock().unwrap().insert(path, (modified, result));
            ctx.request_repaint();
        }
    });
    tx
}

fn empty_state(ui: &mut Ui, snap: &Snapshot) {
    Frame::new()
        .fill(BG_SECONDARY)
        .corner_radius(CornerRadius::same(8))
        .inner_margin(Margin::same(24))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            text(ui, "No clips yet", 16.0, Weight::Semibold, HEADER_PRIMARY);
            ui.add_space(6.0);
            text(
                ui,
                format!(
                    "Press {} during a Discord call to save the last {} seconds. Your clips will show up here.",
                    snap.cfg.hotkey, snap.cfg.seconds
                ),
                14.0,
                Weight::Regular,
                HEADER_SECONDARY,
            );
        });
}

/// Draws one card and returns what the user clicked.
fn tile(
    ui: &mut Ui,
    rect: Rect,
    clip: &ClipFile,
    analysis: Option<&Result<Analysis, String>>,
    view: PlayerView,
    today: i32,
) -> Option<Action> {
    let id = ui.make_persistent_id(("clip", &clip.path));
    let card = ui.interact(rect, id, Sense::hover());
    let fill = if card.hovered() { mix(BG_SECONDARY, BG_HOVER, 0.6) } else { BG_SECONDARY };
    ui.painter().rect_filled(rect, CornerRadius::same(8), fill);
    let inner = rect.shrink(12.0);
    let mut action = None;
    let seconds = match analysis {
        Some(Ok(a)) => a.seconds,
        _ => clip.size as f32 / BYTES_PER_SEC,
    };

    // Player: play/pause button and the waveform to scrub on.
    let panel = Rect::from_min_size(inner.min, vec2(inner.width(), 64.0));
    ui.painter().rect_filled(panel, CornerRadius::same(6), BG_TERTIARY);

    let center = pos2(panel.left() + 30.0, panel.center().y);
    let play = ui
        .interact(Rect::from_center_size(center, vec2(36.0, 36.0)), id.with("play"), Sense::click())
        .on_hover_cursor(CursorIcon::PointingHand)
        .on_hover_text(if view.playing { "Pause" } else { "Play" });
    if play.clicked() {
        action = Some(Action::Toggle);
    }
    play_button(ui, center, view.playing, play.hovered());

    let bars_area = Rect::from_min_max(pos2(panel.left() + 60.0, panel.top() + 10.0), pos2(panel.right() - 14.0, panel.bottom() - 10.0));
    // The whole panel height reacts, so the waveform is easy to grab.
    let scrub_rect = Rect::from_x_y_ranges(bars_area.x_range(), panel.y_range());
    let scrub = ui.interact(scrub_rect, id.with("scrub"), Sense::click_and_drag()).on_hover_cursor(CursorIcon::PointingHand);
    let frac_at = |x: f32| ((x - bars_area.left()) / bars_area.width()).clamp(0.0, 1.0);
    // While dragging, the playhead follows the mouse; playback jumps there on release.
    let dragging = scrub.dragged() || scrub.is_pointer_button_down_on();
    let drag_at = scrub.interact_pointer_pos().map(|p| frac_at(p.x)).filter(|_| dragging);
    if scrub.drag_stopped() || scrub.clicked() {
        if let Some(p) = scrub.interact_pointer_pos() {
            action = Some(Action::Seek(frac_at(p.x)));
        }
    }
    let hover_at = scrub.hover_pos().map(|p| frac_at(p.x)).filter(|_| drag_at.is_none());
    let head = drag_at.or(view.active.then_some(view.position));

    let painter = ui.painter();
    let peaks: Vec<f32> = match analysis {
        Some(Ok(a)) => a.peaks.clone(),
        _ => vec![0.06; library::WAVEFORM_BARS],
    };
    let step = bars_area.width() / peaks.len() as f32;
    let bar_w = (step * 0.6).max(1.5);
    for (i, p) in peaks.iter().enumerate() {
        let x = bars_area.left() + step * (i as f32 + 0.5);
        let h = (p * bars_area.height()).max(3.0);
        let bar = Rect::from_center_size(pos2(x, bars_area.center().y), vec2(bar_w, h));
        let done = head.is_some_and(|f| (i as f32 + 0.5) / peaks.len() as f32 <= f);
        let color = if done {
            Color32::WHITE
        } else if analysis.is_some() {
            TEXT_MUTED
        } else {
            GRAY_BTN
        };
        painter.rect_filled(bar, CornerRadius::same(1), color);
    }

    // Hover: a faint line and the time under the mouse.
    if let Some(f) = hover_at {
        let x = bars_area.left() + f * bars_area.width();
        painter.line_segment([pos2(x, panel.top() + 6.0), pos2(x, panel.bottom() - 6.0)], Stroke::new(1.0, HEADER_SECONDARY.gamma_multiply(0.6)));
        time_bubble(ui, x, panel, &clock_precise(f * seconds));
    }
    // Playhead: a line with a round handle.
    if let Some(f) = head {
        let x = bars_area.left() + f * bars_area.width();
        painter.line_segment([pos2(x, panel.top() + 4.0), pos2(x, panel.bottom() - 4.0)], Stroke::new(2.0, Color32::WHITE));
        painter.circle_filled(pos2(x, panel.top() + 6.0), 4.5, Color32::WHITE);
        if drag_at.is_some() {
            time_bubble(ui, x, panel, &clock_precise(f * seconds));
        }
    }

    // Title and details.
    let title = clip.saved.map(|s| when(s, today)).unwrap_or_else(|| {
        clip.path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()
    });
    let title_y = panel.bottom() + 12.0;
    let title_rect = Rect::from_min_max(pos2(inner.left(), title_y), pos2(inner.right() - 96.0, title_y + 20.0));
    // One line, ending in "…" when it doesn't fit (e.g. long custom names).
    let mut job = egui::text::LayoutJob::simple_singleline(title, font(15.0, Weight::Semibold), HEADER_PRIMARY);
    job.wrap = egui::text::TextWrapping {
        max_width: title_rect.width(),
        max_rows: 1,
        break_anywhere: true,
        overflow_character: Some('…'),
    };
    painter.galley(title_rect.left_top(), painter.layout_job(job), HEADER_PRIMARY);
    // "0:12 / 0:30" while this clip is in the player, else just the length.
    let length = match head.filter(|_| view.active || drag_at.is_some()) {
        Some(f) => format!("{} / {}", clock(f * seconds), clock(seconds)),
        None => clock(seconds),
    };
    painter.text(pos2(inner.right(), title_y), Align2::RIGHT_TOP, length, font(14.0, Weight::Semibold), HEADER_SECONDARY);
    let detail = match analysis {
        Some(Err(_)) => format!("{} · can't read this file", megabytes(clip.size)),
        _ => megabytes(clip.size),
    };
    painter.text(pos2(inner.left(), title_y + 23.0), Align2::LEFT_TOP, detail, font(13.0, Weight::Regular), TEXT_MUTED);

    // Buttons.
    let buttons = Rect::from_min_max(pos2(inner.left(), inner.bottom() - 30.0), inner.max);
    let mut row = ui.new_child(egui::UiBuilder::new().max_rect(buttons).layout(Layout::left_to_right(Align::Center)));
    row.spacing_mut().item_spacing.x = 8.0;
    if button_sized(&mut row, "Copy", ButtonKind::Primary, 30.0).on_hover_text("Copy to paste into Discord").clicked() {
        action = Some(Action::Copy);
    }
    if button_sized(&mut row, "Show", ButtonKind::Secondary, 30.0).on_hover_text("Show in Explorer").clicked() {
        action = Some(Action::Show);
    }
    row.with_layout(Layout::right_to_left(Align::Center), |row| {
        if button_sized(row, "Delete", ButtonKind::Secondary, 30.0).on_hover_text("Move to the Recycle Bin").clicked() {
            action = Some(Action::Delete);
        }
    });
    action
}

/// The round blurple button: a triangle to play, two bars to pause.
fn play_button(ui: &Ui, center: egui::Pos2, playing: bool, hovered: bool) {
    let painter = ui.painter();
    painter.circle_filled(center, 18.0, if hovered { BLURPLE_HOVER } else { BLURPLE });
    if playing {
        for dx in [-4.0, 4.0] {
            let bar = Rect::from_center_size(center + vec2(dx, 0.0), vec2(4.0, 13.0));
            painter.rect_filled(bar, CornerRadius::same(1), Color32::WHITE);
        }
    } else {
        // Nudged right so it looks centred.
        let c = center + vec2(2.0, 0.0);
        painter.add(egui::Shape::convex_polygon(
            vec![c + vec2(-5.0, -7.0), c + vec2(7.0, 0.0), c + vec2(-5.0, 7.0)],
            Color32::WHITE,
            Stroke::NONE,
        ));
    }
}

/// A small time label just above the waveform at `x`. Drawn on the tooltip
/// layer so the scroll area never clips it.
fn time_bubble(ui: &Ui, x: f32, panel: Rect, label: &str) {
    let painter: Painter = ui.ctx().layer_painter(egui::LayerId::new(egui::Order::Tooltip, egui::Id::unique("scrub-time")));
    let galley = painter.layout_no_wrap(label.to_string(), font(12.0, Weight::Bold), Color32::WHITE);
    let size = galley.size() + vec2(10.0, 4.0);
    let left = (x - size.x / 2.0).clamp(panel.left() + 2.0, panel.right() - size.x - 2.0);
    let rect = Rect::from_min_size(pos2(left, panel.top() - size.y - 4.0), size);
    painter.rect_filled(rect, CornerRadius::same(4), BG_FLOATING);
    painter.galley(rect.center() - galley.size() / 2.0, galley, Color32::WHITE);
}

// ---------------------------------------------------------------------------
// Formatting
// ---------------------------------------------------------------------------

/// 0:12.4, for scrubbing.
fn clock_precise(seconds: f32) -> String {
    let tenths = (seconds.max(0.0) * 10.0).round() as u32;
    format!("{}:{:02}.{}", tenths / 600, tenths / 10 % 60, tenths % 10)
}

fn megabytes(bytes: u64) -> String {
    format!("{:.1} MB", bytes as f64 / 1_000_000.0)
}

/// 0:30, 2:05
fn clock(seconds: f32) -> String {
    let s = seconds.round().max(0.0) as u32;
    format!("{}:{:02}", s / 60, s % 60)
}

/// "Today, 19:31", "Yesterday, 08:02", "Nov 9, 21:31", "Nov 9 2025, 21:31".
fn when(s: SavedAt, today: i32) -> String {
    const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    let time = format!("{:02}:{:02}", s.hour, s.minute);
    let day = days_from_civil(s.year as i32, s.month as u32, s.day as u32);
    let month = MONTHS[(s.month.clamp(1, 12) - 1) as usize];
    match today - day {
        0 => format!("Today, {time}"),
        1 => format!("Yesterday, {time}"),
        _ if today - day < 300 => format!("{month} {}, {time}", s.day),
        _ => format!("{month} {} {}, {time}", s.day, s.year),
    }
}

/// Today's local date as a day number (see `days_from_civil`).
fn today() -> i32 {
    let t = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() };
    days_from_civil(t.wYear as i32, t.wMonth as u32, t.wDay as u32)
}

/// Days since 1970-01-01 for a proleptic Gregorian date (Howard Hinnant's
/// algorithm), so dates can be compared and subtracted.
fn days_from_civil(y: i32, m: u32, d: u32) -> i32 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as u32;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe as i32 - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn day_numbers() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(2000, 3, 1) - days_from_civil(2000, 2, 28), 2); // leap year
        assert_eq!(days_from_civil(2027, 1, 1) - days_from_civil(2026, 12, 31), 1);
    }

    #[test]
    fn formats_when() {
        let at = SavedAt { year: 2026, month: 11, day: 9, hour: 21, minute: 31, second: 5 };
        let day = days_from_civil(2026, 11, 9);
        assert_eq!(when(at, day), "Today, 21:31");
        assert_eq!(when(at, day + 1), "Yesterday, 21:31");
        assert_eq!(when(at, day + 3), "Nov 9, 21:31");
        assert_eq!(when(at, day + 400), "Nov 9 2026, 21:31");
        assert_eq!(clock(29.6), "0:30");
        assert_eq!(clock(125.0), "2:05");
    }
}
