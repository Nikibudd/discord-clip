//! The Edit dialog (#31): a Discord-style modal to trim a clip and set the
//! volume of its tracks. Clips with separate tracks show a waveform per track
//! (Discord, Mic) with a volume slider each; the trim handles span both.
//! Save replaces the clip, Save as new clip keeps it.
//!
//! The dialog only picks the selection and volumes; the Clips page writes
//! the files.

use std::time::Duration;

use eframe::egui::{self, pos2, vec2, Align, Color32, CornerRadius, CursorIcon, Frame, Layout, Margin, Rect, Sense, Stroke, Ui};

use super::clips::{clock, clock_precise, megabytes, play_button, today, when};
use super::theme::*;
use super::widgets::*;
use crate::library::{self, ClipFile};
use crate::player::Playback;
use crate::volumes::Volumes;

/// Bars in the dialog's waveforms (cards use `library::WAVEFORM_BARS`).
const BARS: usize = 160;
/// The shortest selection, in seconds.
const MIN_SECONDS: f32 = 0.25;
/// Discord's upload limit without Nitro.
const DISCORD_LIMIT: u64 = 10 * 1024 * 1024;
/// Height of the waveform of a clip with one track, and of each of two.
const ONE_TRACK_HEIGHT: f32 = 104.0;
const TWO_TRACK_HEIGHT: f32 = 68.0;
const TRACK_GAP: f32 = 6.0;
const TRACK_NAMES: [&str; 2] = ["Discord", "Mic"];

pub struct EditDialog {
    clip: ClipFile,
    /// Waveform per track: one, or two (Discord, mic).
    tracks: Vec<Vec<f32>>,
    seconds: f32,
    /// Selection, 0..=1 of the clip.
    start: f32,
    end: f32,
    /// Volumes of the tracks (#7) as set in the dialog, and as stored.
    volumes: Volumes,
    stored_volumes: Volumes,
    player: Option<Playback>,
    error: Option<String>,
    /// Opened from Make Discord-ready on a clip that's too long (#25).
    soundboard: bool,
}

/// What the user chose in the dialog.
pub enum Choice {
    Cancel,
    /// Replace the clip with the selection (if it's trimmed) and store the volumes.
    Save { start: f32, end: f32, volumes: Volumes },
    /// Save the selection as a new clip with these volumes.
    SaveNew { start: f32, end: f32, volumes: Volumes },
}

#[derive(Clone, Copy)]
enum Handle {
    Start,
    End,
}

impl EditDialog {
    pub fn open(clip: &ClipFile, volumes: Volumes) -> anyhow::Result<EditDialog> {
        let a = library::analyze_tracks(&clip.path, BARS)?;
        Ok(EditDialog {
            clip: clip.clone(),
            tracks: a.tracks,
            seconds: a.seconds.max(0.01),
            start: 0.0,
            end: 1.0,
            volumes,
            stored_volumes: volumes,
            player: None,
            error: None,
            soundboard: false,
        })
    }

    /// For a clip too long for the soundboard (#25): the selection starts as
    /// its last few seconds (where the moment usually is, since a clip ends
    /// when the hotkey is pressed), with a note on what to do.
    pub fn open_for_soundboard(clip: &ClipFile, volumes: Volumes) -> anyhow::Result<EditDialog> {
        let mut d = EditDialog::open(clip, volumes)?;
        d.soundboard = true;
        d.start = (1.0 - library::SOUNDBOARD_MAX_SECONDS / d.seconds).max(0.0);
        Ok(d)
    }

    pub fn clip(&self) -> &ClipFile {
        &self.clip
    }

    /// Shown after a save failed, so the user can try again.
    pub fn set_error(&mut self, e: String) {
        self.error = Some(e);
    }

    fn trimmed(&self) -> bool {
        self.start > 0.0 || self.end < 1.0
    }

    /// Anything to save?
    fn changed(&self) -> bool {
        self.trimmed() || self.volumes != self.stored_volumes
    }

    /// Draws the dialog; returns the user's choice once they make one.
    pub fn show(&mut self, ctx: &egui::Context) -> Option<Choice> {
        self.update_playback(ctx);
        let mut choice = None;
        let screen = ctx.content_rect();
        let width = (screen.width() - 80.0).clamp(320.0, 680.0);
        let modal = egui::Modal::new(egui::Id::unique("edit-dialog"))
            .backdrop_color(Color32::from_black_alpha(170))
            .frame(Frame::new().fill(BG_PRIMARY).corner_radius(CornerRadius::same(8)).inner_margin(Margin::same(20)))
            .show(ctx, |ui| {
                ui.set_width(width);
                ui.spacing_mut().item_spacing = vec2(8.0, 0.0);
                self.contents(ui, &mut choice);
            });
        // Esc always cancels; a click outside only when there's nothing to
        // lose, so a stray click doesn't throw away edits. (`should_close`
        // consumes the Esc key press, so it can't be checked afterwards.)
        let stray_click = modal.backdrop_response.clicked() && self.changed();
        if choice.is_none() && !stray_click && modal.should_close() {
            choice = Some(Choice::Cancel);
        }
        if choice.is_some() {
            self.player = None; // stop playing
        }
        choice
    }

    fn contents(&mut self, ui: &mut Ui, choice: &mut Option<Choice>) {
        let max = library::SOUNDBOARD_MAX_SECONDS;
        text(ui, "Edit clip", 20.0, Weight::Bold, HEADER_PRIMARY);
        ui.add_space(4.0);
        let title = self.clip.name.clone().or_else(|| self.clip.saved.map(|s| when(s, today()))).unwrap_or_default();
        text(ui, format!("{title} · {}", clock(self.seconds)), 14.0, Weight::Regular, HEADER_SECONDARY);
        if self.soundboard {
            ui.add_space(8.0);
            text(
                ui,
                format!("Discord's soundboard takes {max} seconds at most. Select the part to keep and Save, then Make Discord-ready."),
                14.0,
                Weight::Regular,
                TEXT_NORMAL,
            );
        }
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
                let summary = format!("{} selected · {}", clock_precise(selected), megabytes(bytes));
                let (msg, color) = if self.soundboard && selected > max + 0.005 {
                    (format!("{summary}: over the soundboard's {max} s"), YELLOW)
                } else if bytes > DISCORD_LIMIT {
                    (format!("{summary}: over Discord's 10 MB limit"), YELLOW)
                } else {
                    (summary, HEADER_SECONDARY)
                };
                text(ui, msg, 14.0, Weight::Semibold, color);
            });
        });

        if self.tracks.len() > 1 {
            ui.add_space(12.0);
            self.volume_sliders(ui);
        }

        if let Some(e) = &self.error {
            ui.add_space(10.0);
            text(ui, e.as_str(), 14.0, Weight::Regular, RED);
        }

        // Footer, like Discord's modals: darker strip with the buttons on the right.
        ui.add_space(20.0);
        let changed = self.changed();
        Frame::new()
            .fill(BG_SECONDARY)
            .corner_radius(CornerRadius::same(6))
            .inner_margin(Margin::symmetric(12, 10))
            .show(ui, |ui| {
                let w = ui.available_width();
                ui.allocate_ui_with_layout(vec2(w, 32.0), Layout::right_to_left(Align::Center), |ui| {
                    let (start, end, volumes) = (self.start, self.end, self.volumes);
                    let save_hint = if self.trimmed() {
                        "Replaces this clip with the selection; the original goes to the Recycle Bin"
                    } else {
                        "Saves the volumes; the file stays as it is"
                    };
                    let hint = if changed { save_hint } else { "Nothing changed yet" };
                    if button_enabled(ui, "Save", ButtonKind::Primary, 32.0, changed).on_hover_text(hint).clicked() {
                        *choice = Some(Choice::Save { start, end, volumes });
                    }
                    let hint = if changed { "Keeps the original" } else { "Nothing changed yet" };
                    if button_enabled(ui, "Save as new clip", ButtonKind::Secondary, 32.0, changed).on_hover_text(hint).clicked() {
                        *choice = Some(Choice::SaveNew { start, end, volumes });
                    }
                    ui.add_space(4.0);
                    if button(ui, "Cancel", ButtonKind::Secondary).clicked() {
                        *choice = Some(Choice::Cancel);
                    }
                });
            });
    }

    /// A volume slider per track, side by side. Playback follows straight away.
    fn volume_sliders(&mut self, ui: &mut Ui) {
        const GAP: f32 = 24.0;
        let w = (ui.available_width() - GAP) / 2.0;
        let before = self.volumes;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            for (track, name) in TRACK_NAMES.iter().enumerate() {
                if track > 0 {
                    ui.add_space(GAP);
                }
                ui.allocate_ui_with_layout(vec2(w, 48.0), Layout::top_down(Align::Min), |ui| {
                    let value = if track == 0 { &mut self.volumes.discord } else { &mut self.volumes.mic };
                    ui.allocate_ui_with_layout(vec2(w, 18.0), Layout::left_to_right(Align::Center), |ui| {
                        text(ui, *name, 14.0, Weight::Semibold, TEXT_NORMAL);
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            text(ui, format!("{}%", (*value * 100.0).round() as u32), 14.0, Weight::Semibold, HEADER_PRIMARY);
                        });
                    });
                    slider(ui, value, 0.0..=Volumes::MAX, 0.05, true);
                });
            }
        });
        if self.volumes != before {
            if let Some(p) = &mut self.player {
                p.set_volumes(self.volumes);
            }
        }
    }

    /// The waveforms with the selection, handles, times and playhead.
    fn waveform(&mut self, ui: &mut Ui) {
        let n = self.tracks.len().max(1);
        let track_h = if n > 1 { TWO_TRACK_HEIGHT } else { ONE_TRACK_HEIGHT };
        let height = track_h * n as f32 + TRACK_GAP * (n - 1) as f32;
        let (panel, _) = ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::hover());
        let (left, right) = (panel.left() + 14.0, panel.right() - 14.0);
        let x_at = |f: f32| left + f * (right - left);
        let frac_at = |x: f32| ((x - left) / (right - left)).clamp(0.0, 1.0);
        let min_gap = (MIN_SECONDS / self.seconds).min(1.0);

        // Scrubbing, like on the cards: click or drag on the waveforms to move
        // the playhead (inside the selection). While dragging, the playhead
        // follows the mouse; playback jumps there on release. Registered
        // before the handles so they win where they overlap.
        let (sel_start, sel_end) = (self.start, self.end);
        let in_selection = |f: f32| f.min(sel_end - 0.001).max(sel_start);
        let scrub = ui
            .interact(panel, ui.make_persistent_id("edit-scrub"), Sense::click_and_drag())
            .on_hover_cursor(CursorIcon::PointingHand);
        let pointer_at = scrub.interact_pointer_pos().map(|p| in_selection(frac_at(p.x)));
        let dragging = scrub.dragged() || scrub.is_pointer_button_down_on();
        let drag_at = pointer_at.filter(|_| dragging);
        let released_at = pointer_at.filter(|_| scrub.drag_stopped() || scrub.clicked());

        // Handles: drag to move. They span all tracks.
        let mut hovered_handle = false;
        for handle in [Handle::Start, Handle::End] {
            let f = match handle {
                Handle::Start => self.start,
                Handle::End => self.end,
            };
            let grab = Rect::from_center_size(pos2(x_at(f), panel.center().y), vec2(18.0, panel.height()));
            let id = ui.make_persistent_id(("edit-handle", handle as u8));
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

        if let Some(at) = released_at {
            self.seek(at);
        }
        // Where it was let go, not the old spot (see #12).
        let head = drag_at.or(released_at).or(self.player.as_ref().map(|p| p.position()));
        let painter = ui.painter();
        for (t, peaks) in self.tracks.iter().enumerate() {
            let top = panel.top() + t as f32 * (track_h + TRACK_GAP);
            let rect = Rect::from_x_y_ranges(panel.x_range(), top..=top + track_h);
            painter.rect_filled(rect, CornerRadius::same(6), BG_TERTIARY);
            let sel = Rect::from_x_y_ranges(x_at(self.start)..=x_at(self.end), rect.y_range());
            painter.rect_filled(sel, CornerRadius::ZERO, mix(BG_TERTIARY, BLURPLE, 0.14));
            // Two tracks: name in the corner, bars below it, sized by volume.
            let (bars, gain) = if n > 1 {
                painter.text(rect.left_top() + vec2(10.0, 5.0), egui::Align2::LEFT_TOP, TRACK_NAMES[t], font(12.0, Weight::Bold), HEADER_SECONDARY);
                let gain = if t == 0 { self.volumes.discord } else { self.volumes.mic };
                (Rect::from_min_max(pos2(left, rect.top() + 22.0), pos2(right, rect.bottom() - 6.0)), gain)
            } else {
                (Rect::from_min_max(pos2(left, rect.top() + 12.0), pos2(right, rect.bottom() - 12.0)), 1.0)
            };
            let step = bars.width() / peaks.len() as f32;
            let bar_w = (step * 0.6).max(1.5);
            for (i, p) in peaks.iter().enumerate() {
                let f = (i as f32 + 0.5) / peaks.len() as f32;
                let x = bars.left() + step * (i as f32 + 0.5);
                let h = ((p * gain).min(1.0) * bars.height()).max(3.0);
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
            None => match Playback::start(&self.clip.path, self.start, self.volumes) {
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

    /// Moves the playhead to `at` (0..=1 of the clip): keeps playing or
    /// paused, or starts playing there if nothing was loaded.
    fn seek(&mut self, at: f32) {
        let result = match &mut self.player {
            Some(p) => p.seek(at),
            None => Playback::start(&self.clip.path, at, self.volumes).map(|p| self.player = Some(p)),
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
