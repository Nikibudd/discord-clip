//! The Trim dialog (#13): a Discord-style modal with a large waveform, start
//! and end handles, Play selection, and Save as new / Replace / Copy.
//!
//! The dialog only picks the selection; the Clips page writes the files.

use std::time::Duration;

use eframe::egui::{self, pos2, vec2, Align, Color32, CornerRadius, CursorIcon, Frame, Layout, Margin, Rect, Sense, Stroke, Ui};

use super::clips::{clock, clock_precise, megabytes, play_button, today, when};
use super::theme::*;
use super::widgets::*;
use crate::library::{self, ClipFile};
use crate::player::Playback;

/// Bars in the dialog's waveform (cards use `library::WAVEFORM_BARS`).
const BARS: usize = 160;
/// The shortest selection, in seconds.
const MIN_SECONDS: f32 = 0.25;
/// Discord's upload limit without Nitro.
const DISCORD_LIMIT: u64 = 10 * 1024 * 1024;

pub struct TrimDialog {
    clip: ClipFile,
    peaks: Vec<f32>,
    seconds: f32,
    /// Selection, 0..=1 of the clip.
    start: f32,
    end: f32,
    player: Option<Playback>,
    error: Option<String>,
}

/// What the user chose in the dialog.
pub enum Choice {
    Cancel,
    SaveNew { start: f32, end: f32 },
    Replace { start: f32, end: f32 },
    Copy { start: f32, end: f32 },
}

#[derive(Clone, Copy)]
enum Handle {
    Start,
    End,
}

impl TrimDialog {
    pub fn open(clip: &ClipFile) -> anyhow::Result<TrimDialog> {
        let a = library::analyze_bars(&clip.path, BARS)?;
        Ok(TrimDialog {
            clip: clip.clone(),
            peaks: a.peaks,
            seconds: a.seconds.max(0.01),
            start: 0.0,
            end: 1.0,
            player: None,
            error: None,
        })
    }

    pub fn clip(&self) -> &ClipFile {
        &self.clip
    }

    /// Shown after a save/copy failed, so the user can try again.
    pub fn set_error(&mut self, e: String) {
        self.error = Some(e);
    }

    /// Draws the dialog; returns the user's choice once they make one.
    pub fn show(&mut self, ctx: &egui::Context) -> Option<Choice> {
        self.update_playback(ctx);
        let mut choice = None;
        let screen = ctx.content_rect();
        let width = (screen.width() - 80.0).clamp(320.0, 680.0);
        let modal = egui::Modal::new(egui::Id::unique("trim-dialog"))
            .backdrop_color(Color32::from_black_alpha(170))
            .frame(Frame::new().fill(BG_PRIMARY).corner_radius(CornerRadius::same(8)).inner_margin(Margin::same(20)))
            .show(ctx, |ui| {
                ui.set_width(width);
                ui.spacing_mut().item_spacing = vec2(8.0, 0.0);
                self.contents(ui, &mut choice);
            });
        if choice.is_none() && modal.should_close() {
            choice = Some(Choice::Cancel);
        }
        if choice.is_some() {
            self.player = None; // stop playing
        }
        choice
    }

    fn contents(&mut self, ui: &mut Ui, choice: &mut Option<Choice>) {
        text(ui, "Trim clip", 20.0, Weight::Bold, HEADER_PRIMARY);
        ui.add_space(4.0);
        let title = self.clip.name.clone().or_else(|| self.clip.saved.map(|s| when(s, today()))).unwrap_or_default();
        text(ui, format!("{title} · {}", clock(self.seconds)), 14.0, Weight::Regular, HEADER_SECONDARY);
        ui.add_space(16.0);

        self.waveform(ui);
        ui.add_space(14.0);

        // Play selection, and how long / big the result is.
        let selected = (self.end - self.start) * self.seconds;
        let bytes = ((self.end - self.start) as f64 * self.clip.size as f64) as u64;
        let width = ui.available_width();
        ui.allocate_ui_with_layout(vec2(width, 36.0), Layout::left_to_right(Align::Center), |ui| {
            let playing = self.player.as_ref().is_some_and(|p| !p.is_paused());
            let (rect, resp) = ui.allocate_exact_size(vec2(36.0, 36.0), Sense::click());
            let resp = resp.on_hover_cursor(CursorIcon::PointingHand);
            play_button(ui, rect.center(), playing, resp.hovered());
            let label = ui.add(egui::Label::new(egui::RichText::new(if playing { "Pause" } else { "Play selection" }).font(font(15.0, Weight::Semibold)).color(TEXT_NORMAL)).sense(Sense::click()));
            if resp.clicked() || label.clicked() {
                self.toggle_play();
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let (msg, color) = if bytes > DISCORD_LIMIT {
                    (format!("{} selected · {}: over Discord's 10 MB limit", clock(selected), megabytes(bytes)), YELLOW)
                } else {
                    (format!("{} selected · {}", clock(selected), megabytes(bytes)), HEADER_SECONDARY)
                };
                text(ui, msg, 14.0, Weight::Semibold, color);
            });
        });

        if let Some(e) = &self.error {
            ui.add_space(10.0);
            text(ui, e.as_str(), 14.0, Weight::Regular, RED);
        }

        // Footer, like Discord's modals: darker strip with the buttons on the right.
        ui.add_space(20.0);
        Frame::new()
            .fill(BG_SECONDARY)
            .corner_radius(CornerRadius::same(6))
            .inner_margin(Margin::symmetric(12, 10))
            .show(ui, |ui| {
                let w = ui.available_width();
                ui.allocate_ui_with_layout(vec2(w, 32.0), Layout::right_to_left(Align::Center), |ui| {
                    let (start, end) = (self.start, self.end);
                    if button(ui, "Save as new clip", ButtonKind::Success).on_hover_text("Keeps the original").clicked() {
                        *choice = Some(Choice::SaveNew { start, end });
                    }
                    if button(ui, "Replace", ButtonKind::Secondary)
                        .on_hover_text("Replaces this clip; the original goes to the Recycle Bin")
                        .clicked()
                    {
                        *choice = Some(Choice::Replace { start, end });
                    }
                    if button(ui, "Copy trimmed", ButtonKind::Primary)
                        .on_hover_text("Copies just the selection to paste into Discord, without saving it")
                        .clicked()
                    {
                        *choice = Some(Choice::Copy { start, end });
                    }
                    ui.add_space(4.0);
                    if button(ui, "Cancel", ButtonKind::Secondary).clicked() {
                        *choice = Some(Choice::Cancel);
                    }
                });
            });
    }

    /// The waveform with the selection, handles, times and playhead.
    fn waveform(&mut self, ui: &mut Ui) {
        let (panel, _) = ui.allocate_exact_size(vec2(ui.available_width(), 104.0), Sense::hover());
        let bars = Rect::from_min_max(panel.min + vec2(14.0, 12.0), panel.max - vec2(14.0, 12.0));
        let x_at = |f: f32| bars.left() + f * bars.width();
        let frac_at = |x: f32| ((x - bars.left()) / bars.width()).clamp(0.0, 1.0);
        let min_gap = (MIN_SECONDS / self.seconds).min(1.0);

        // Handles: drag to move.
        let mut hovered_handle = false;
        for handle in [Handle::Start, Handle::End] {
            let f = match handle {
                Handle::Start => self.start,
                Handle::End => self.end,
            };
            let grab = Rect::from_center_size(pos2(x_at(f), panel.center().y), vec2(18.0, panel.height()));
            let id = ui.make_persistent_id(("trim-handle", handle as u8));
            let resp = ui.interact(grab, id, Sense::drag()).on_hover_cursor(CursorIcon::ResizeHorizontal);
            hovered_handle |= resp.hovered() || resp.dragged();
            if let Some(p) = resp.interact_pointer_pos().filter(|_| resp.dragged()) {
                let at = frac_at(p.x);
                match handle {
                    Handle::Start => self.start = at.min(self.end - min_gap).max(0.0),
                    Handle::End => self.end = at.max(self.start + min_gap).min(1.0),
                }
                // The selection changed under the playhead: start over next time.
                self.player = None;
            }
        }

        let painter = ui.painter();
        painter.rect_filled(panel, CornerRadius::same(6), BG_TERTIARY);
        let sel = Rect::from_x_y_ranges(x_at(self.start)..=x_at(self.end), panel.y_range());
        painter.rect_filled(sel, CornerRadius::ZERO, mix(BG_TERTIARY, BLURPLE, 0.14));

        let step = bars.width() / self.peaks.len() as f32;
        let bar_w = (step * 0.6).max(1.5);
        let head = self.player.as_ref().map(|p| p.position());
        for (i, p) in self.peaks.iter().enumerate() {
            let f = (i as f32 + 0.5) / self.peaks.len() as f32;
            let x = bars.left() + step * (i as f32 + 0.5);
            let h = (p * bars.height()).max(3.0);
            let inside = f >= self.start && f <= self.end;
            let color = if !inside {
                GRAY_BTN
            } else if head.is_some_and(|h| f <= h) {
                Color32::WHITE
            } else {
                TEXT_NORMAL
            };
            painter.rect_filled(Rect::from_center_size(pos2(x, bars.center().y), vec2(bar_w, h)), CornerRadius::same(1), color);
        }

        // Handles: blurple bars with a grip.
        for f in [self.start, self.end] {
            let x = x_at(f);
            let bar = Rect::from_center_size(pos2(x, panel.center().y), vec2(6.0, panel.height()));
            painter.rect_filled(bar, CornerRadius::same(3), if hovered_handle { BLURPLE_HOVER } else { BLURPLE });
            for dy in [-6.0, 0.0, 6.0] {
                let c = pos2(x, panel.center().y + dy);
                painter.line_segment([c + vec2(-1.5, 0.0), c + vec2(1.5, 0.0)], Stroke::new(1.5, Color32::WHITE));
            }
        }

        if let Some(h) = head {
            let x = x_at(h);
            painter.line_segment([pos2(x, panel.top() + 4.0), pos2(x, panel.bottom() - 4.0)], Stroke::new(2.0, Color32::WHITE));
        }

        // Times under the handles, kept apart so they don't overlap.
        ui.add_space(6.0);
        let (row, _) = ui.allocate_exact_size(vec2(panel.width(), 18.0), Sense::hover());
        let font_id = font(13.0, Weight::Semibold);
        let start_label = ui.painter().layout_no_wrap(clock_precise(self.start * self.seconds), font_id.clone(), HEADER_SECONDARY);
        let end_label = ui.painter().layout_no_wrap(clock_precise(self.end * self.seconds), font_id, HEADER_SECONDARY);
        let (sx, ex) = place_labels(
            x_at(self.start),
            x_at(self.end),
            start_label.size().x,
            end_label.size().x,
            row.left(),
            row.right(),
        );
        ui.painter().galley(pos2(sx, row.top()), start_label, HEADER_SECONDARY);
        ui.painter().galley(pos2(ex, row.top()), end_label, HEADER_SECONDARY);
    }

    fn toggle_play(&mut self) {
        let result = match &mut self.player {
            Some(p) if !p.is_paused() => {
                p.pause();
                Ok(())
            }
            Some(p) => p.resume(),
            None => match Playback::start(&self.clip.path, self.start) {
                Ok(p) => {
                    self.player = Some(p);
                    Ok(())
                }
                Err(e) => Err(e),
            },
        };
        if let Err(e) = result {
            self.error = Some(format!("Couldn't play the clip: {e:#}"));
        }
    }

    /// Stops at the end handle, ready to play the selection again.
    fn update_playback(&mut self, ctx: &egui::Context) {
        let Some(p) = &mut self.player else { return };
        if p.position() >= self.end || p.finished() {
            self.player = None;
        } else if !p.is_paused() {
            ctx.request_repaint_after(Duration::from_millis(16));
        }
    }
}

/// Left edges for the start and end time labels: centred under their handles
/// where possible, inside `left..right`, and not overlapping (when the
/// handles are close, they push each other apart, inward at the edges).
/// Never panics, whatever the inputs (#6 crash: `clamp` with min > max).
fn place_labels(start_x: f32, end_x: f32, start_w: f32, end_w: f32, left: f32, right: f32) -> (f32, f32) {
    const GAP: f32 = 8.0;
    let within = |x: f32, w: f32| x.min(right - w).max(left);
    let mut sx = within(start_x - start_w / 2.0, start_w);
    let mut ex = within(end_x - end_w / 2.0, end_w);
    if ex < sx + start_w + GAP {
        // Overlap: put the pair side by side around their middle, then
        // shift the pair back inside the row.
        let total = start_w + GAP + end_w;
        let middle = (start_x + end_x) / 2.0;
        sx = (middle - total / 2.0).min(right - total).max(left);
        ex = sx + start_w + GAP;
    }
    (sx, ex)
}

#[cfg(test)]
mod tests {
    use super::place_labels;

    #[test]
    fn labels_never_overlap_or_leave_the_row() {
        let (left, right, w) = (100.0, 700.0, 46.0);
        // Every combination of handle positions, including both at the far
        // right (the crash) and both at the far left.
        for s in 0..=60 {
            for e in s..=60 {
                let sx_handle = left + (right - left) * s as f32 / 60.0;
                let ex_handle = left + (right - left) * e as f32 / 60.0;
                let (sx, ex) = place_labels(sx_handle, ex_handle, w, w, left, right);
                assert!(sx >= left - 0.01, "start label left of the row: {s} {e}");
                assert!(ex + w <= right + 0.01, "end label right of the row: {s} {e}");
                assert!(ex >= sx + w + 7.99, "labels overlap: {s} {e}");
            }
        }
        // Far apart: centred under the handles.
        assert_eq!(place_labels(200.0, 600.0, w, w, left, right), (177.0, 577.0));
        // A row too narrow for both still doesn't panic.
        let _ = place_labels(110.0, 120.0, w, w, 100.0, 140.0);
    }
}
