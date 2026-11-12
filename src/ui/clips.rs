//! The Clips page: the clips folder as a grid of cards, each with a
//! waveform you can click to play, and Copy / Show / Delete buttons.
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
    self, pos2, vec2, Align, Align2, Color32, CornerRadius, CursorIcon, Frame, Layout, Margin, Rect, Sense, Ui,
};

use super::theme::*;
use super::widgets::*;
use super::{send, Command, Snapshot};
use crate::library::{self, Analysis, ClipFile, SavedAt};

const RESCAN_EVERY: Duration = Duration::from_secs(2);
const TILE_MIN_WIDTH: f32 = 250.0;
const TILE_HEIGHT: f32 = 186.0;
const GAP: f32 = 16.0;
/// Bytes per second of our clips (48 kHz, stereo, 16-bit), to estimate
/// length before a clip is analysed.
const BYTES_PER_SEC: f32 = 48_000.0 * 2.0 * 2.0;

type Analyses = Arc<Mutex<HashMap<PathBuf, (SystemTime, Result<Analysis, String>)>>>;

struct Playing {
    path: PathBuf,
    started: Instant,
    seconds: f32,
}

#[derive(Default)]
pub struct ClipsPage {
    dir: Option<PathBuf>,
    clips: Vec<ClipFile>,
    last_scan: Option<Instant>,
    analyses: Analyses,
    /// Sent to the worker (path, modified) and not re-sent until it changes.
    queued: HashMap<PathBuf, SystemTime>,
    worker: Option<Sender<(PathBuf, SystemTime)>>,
    playing: Option<Playing>,
    /// Deleted from the page; hidden until the next scan confirms it.
    deleted: HashSet<PathBuf>,
    toast: Option<(String, Instant)>,
}

enum Action {
    Play,
    Copy,
    Show,
    Delete,
}

impl ClipsPage {
    /// Stops playback (e.g. when the window is hidden).
    pub fn stop(&mut self) {
        if self.playing.take().is_some() {
            library::stop();
        }
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

    fn update_playing(&mut self, ctx: &egui::Context) {
        if let Some(p) = &self.playing {
            if p.started.elapsed().as_secs_f32() >= p.seconds {
                self.playing = None;
            } else {
                ctx.request_repaint_after(Duration::from_millis(33));
            }
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
                let progress = self
                    .playing
                    .as_ref()
                    .filter(|p| p.path == clip.path)
                    .map(|p| (p.started.elapsed().as_secs_f32() / p.seconds.max(0.01)).clamp(0.0, 1.0));
                if let Some(action) = tile(ui, rect, clip, analysis, progress, today) {
                    self.act(action, clip, analysis);
                }
            }
            ui.add_space(GAP);
        }
    }

    fn act(&mut self, action: Action, clip: &ClipFile, analysis: Option<&Result<Analysis, String>>) {
        match action {
            Action::Play => {
                if self.playing.as_ref().is_some_and(|p| p.path == clip.path) {
                    self.stop();
                } else {
                    let seconds = match analysis {
                        Some(Ok(a)) => a.seconds,
                        _ => clip.size as f32 / BYTES_PER_SEC,
                    };
                    match library::play(&clip.path) {
                        Ok(()) => self.playing = Some(Playing { path: clip.path.clone(), started: Instant::now(), seconds }),
                        Err(e) => self.toast = Some((format!("{e:#}"), Instant::now())),
                    }
                }
            }
            Action::Copy => {
                send(Command::CopyClip(clip.path.clone()));
                self.toast = Some(("Copied. Paste it into a Discord chat with Ctrl+V.".into(), Instant::now()));
            }
            Action::Show => send(Command::ShowClip(clip.path.clone())),
            Action::Delete => {
                if self.playing.as_ref().is_some_and(|p| p.path == clip.path) {
                    self.stop();
                }
                self.deleted.insert(clip.path.clone());
                send(Command::DeleteClip(clip.path.clone()));
                self.toast = Some(("Moved to the Recycle Bin.".into(), Instant::now()));
            }
        }
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
    progress: Option<f32>,
    today: i32,
) -> Option<Action> {
    let id = ui.make_persistent_id(("clip", &clip.path));
    let card = ui.interact(rect, id, Sense::hover());
    let painter = ui.painter();
    let fill = if card.hovered() { mix(BG_SECONDARY, BG_HOVER, 0.6) } else { BG_SECONDARY };
    painter.rect_filled(rect, CornerRadius::same(8), fill);
    let inner = rect.shrink(12.0);
    let mut action = None;

    // Waveform panel with the play button; clicking anywhere on it plays.
    let panel = Rect::from_min_size(inner.min, vec2(inner.width(), 64.0));
    let panel_resp = ui.interact(panel, id.with("play"), Sense::click()).on_hover_cursor(CursorIcon::PointingHand);
    if panel_resp.clicked() {
        action = Some(Action::Play);
    }
    painter.rect_filled(panel, CornerRadius::same(6), BG_TERTIARY);
    let center = pos2(panel.left() + 30.0, panel.center().y);
    let btn = if panel_resp.hovered() { BLURPLE_HOVER } else { BLURPLE };
    painter.circle_filled(center, 18.0, btn);
    if progress.is_some() {
        // Stop: a square.
        painter.rect_filled(Rect::from_center_size(center, vec2(11.0, 11.0)), CornerRadius::same(2), Color32::WHITE);
    } else {
        // Play: a triangle, nudged right so it looks centred.
        let c = center + vec2(2.0, 0.0);
        painter.add(egui::Shape::convex_polygon(
            vec![c + vec2(-5.0, -7.0), c + vec2(7.0, 0.0), c + vec2(-5.0, 7.0)],
            Color32::WHITE,
            egui::Stroke::NONE,
        ));
    }

    let bars_area = Rect::from_min_max(pos2(panel.left() + 60.0, panel.top() + 10.0), pos2(panel.right() - 12.0, panel.bottom() - 10.0));
    let peaks: Vec<f32> = match analysis {
        Some(Ok(a)) => a.peaks.clone(),
        _ => vec![0.06; library::WAVEFORM_BARS],
    };
    let step = bars_area.width() / peaks.len() as f32;
    let bar_w = (step * 0.6).max(1.5);
    let played = progress.unwrap_or(0.0);
    for (i, p) in peaks.iter().enumerate() {
        let x = bars_area.left() + step * (i as f32 + 0.5);
        let h = (p * bars_area.height()).max(3.0);
        let bar = Rect::from_center_size(pos2(x, bars_area.center().y), vec2(bar_w, h));
        let done = progress.is_some() && (i as f32 + 0.5) / peaks.len() as f32 <= played;
        let color = if done { Color32::WHITE } else if analysis.is_some() { TEXT_MUTED } else { GRAY_BTN };
        painter.rect_filled(bar, CornerRadius::same(1), color);
    }

    // Title and details.
    let seconds = match analysis {
        Some(Ok(a)) => a.seconds,
        _ => clip.size as f32 / BYTES_PER_SEC,
    };
    let title = clip.saved.map(|s| when(s, today)).unwrap_or_else(|| {
        clip.path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()
    });
    let title_y = panel.bottom() + 12.0;
    let title_rect = Rect::from_min_max(pos2(inner.left(), title_y), pos2(inner.right() - 48.0, title_y + 20.0));
    // One line, ending in "…" when it doesn't fit (e.g. long custom names).
    let mut job = egui::text::LayoutJob::simple_singleline(title, font(15.0, Weight::Semibold), HEADER_PRIMARY);
    job.wrap = egui::text::TextWrapping {
        max_width: title_rect.width(),
        max_rows: 1,
        break_anywhere: true,
        overflow_character: Some('…'),
    };
    painter.galley(title_rect.left_top(), painter.layout_job(job), HEADER_PRIMARY);
    painter.text(pos2(inner.right(), title_y), Align2::RIGHT_TOP, clock(seconds), font(14.0, Weight::Semibold), HEADER_SECONDARY);
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

// ---------------------------------------------------------------------------
// Formatting
// ---------------------------------------------------------------------------

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
