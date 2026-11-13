//! Discord-style widgets drawn with egui's painter.

use std::ops::RangeInclusive;

use eframe::egui::{self, pos2, vec2, Align, Color32, CornerRadius, CursorIcon, Layout, Rect, RichText, Sense, Stroke, Ui};

use super::theme::*;

pub fn text(ui: &mut Ui, s: impl Into<String>, size: f32, weight: Weight, color: Color32) -> egui::Response {
    ui.label(RichText::new(s.into()).font(font(size, weight)).color(color))
}

pub fn section_label(ui: &mut Ui, s: &str) {
    text(ui, s.to_uppercase(), 12.0, Weight::Bold, HEADER_SECONDARY);
    ui.add_space(8.0);
}

pub fn divider(ui: &mut Ui) {
    ui.add_space(24.0);
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), Sense::hover());
    ui.painter().rect_filled(rect, 0.0, DIVIDER);
    ui.add_space(24.0);
}

#[derive(Clone, Copy)]
pub enum ButtonKind {
    Primary,
    Secondary,
    Success,
    Danger,
}

pub fn button(ui: &mut Ui, label: &str, kind: ButtonKind) -> egui::Response {
    button_sized(ui, label, kind, 32.0)
}

pub fn button_sized(ui: &mut Ui, label: &str, kind: ButtonKind, height: f32) -> egui::Response {
    button_enabled(ui, label, kind, height, true)
}

/// A button that can be greyed out: dimmed, and clicks do nothing (hover
/// text still shows, to say why).
pub fn button_enabled(ui: &mut Ui, label: &str, kind: ButtonKind, height: f32, enabled: bool) -> egui::Response {
    let text_color = if enabled { Color32::WHITE } else { TEXT_MUTED };
    let galley = ui.painter().layout_no_wrap(label.to_string(), font(14.0, Weight::Semibold), text_color);
    let size = vec2((galley.size().x + 32.0).max(60.0), height);
    if !enabled {
        let (rect, resp) = ui.allocate_exact_size(size, Sense::hover());
        ui.painter().rect_filled(rect, CornerRadius::same(4), GRAY_BTN.gamma_multiply(0.6));
        ui.painter().galley(rect.center() - galley.size() / 2.0, galley, text_color);
        return resp;
    }
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
pub fn toggle(ui: &mut Ui, on: bool) -> egui::Response {
    toggle_enabled(ui, on, true)
}

/// A toggle that can be greyed out: dimmed, and clicks do nothing.
pub fn toggle_enabled(ui: &mut Ui, on: bool, enabled: bool) -> egui::Response {
    let sense = if enabled { Sense::click() } else { Sense::hover() };
    let (rect, resp) = ui.allocate_exact_size(vec2(40.0, 24.0), sense);
    if !enabled {
        ui.painter().rect_filled(rect, CornerRadius::same(12), GRAY_BTN);
        let c = pos2(rect.left() + 12.0, rect.center().y);
        ui.painter().circle_filled(c, 9.0, TEXT_MUTED);
        return resp;
    }
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
pub fn row<R>(ui: &mut Ui, title: &str, desc: &str, control: impl FnOnce(&mut Ui) -> R) -> R {
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
pub fn slider(ui: &mut Ui, value: &mut f32, range: RangeInclusive<f32>, step: f32, enabled: bool) -> egui::Response {
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

/// A status message, optionally with a colored dot in front.
pub fn status_line(ui: &mut Ui, dot: Option<Color32>, msg: &str, color: Color32) {
    ui.horizontal(|ui| {
        if let Some(c) = dot {
            let (rect, _) = ui.allocate_exact_size(vec2(10.0, 20.0), Sense::hover());
            ui.painter().circle_filled(rect.center(), 5.0, c);
            ui.add_space(2.0);
        }
        text(ui, msg, 15.0, Weight::Semibold, color);
    });
}

pub fn progress_bar(ui: &mut Ui, frac: f32) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 8.0), Sense::hover());
    let painter = ui.painter();
    painter.rect_filled(rect, CornerRadius::same(4), BG_TERTIARY);
    let fill = Rect::from_min_max(rect.min, pos2(rect.left() + rect.width() * frac.clamp(0.0, 1.0), rect.max.y));
    painter.rect_filled(fill, CornerRadius::same(4), BLURPLE);
}

/// "just now", "5 minutes ago", "3 hours ago", "2 days ago".
pub fn ago(t: std::time::SystemTime) -> String {
    let secs = t.elapsed().map(|d| d.as_secs()).unwrap_or(0);
    let plural = |n: u64, unit: &str| format!("{n} {unit}{} ago", if n == 1 { "" } else { "s" });
    match secs {
        0..=59 => "just now".to_string(),
        60..=3599 => plural(secs / 60, "minute"),
        3600..=86399 => plural(secs / 3600, "hour"),
        _ => plural(secs / 86400, "day"),
    }
}


/// Discord's select: a box showing the current choice with a chevron; click
/// it to open the list underneath. Returns the box's response (e.g. to
/// refresh the options when it opens) and the option picked this frame.
pub fn dropdown(
    ui: &mut Ui,
    id_salt: &str,
    options: &[String],
    selected: Option<usize>,
    enabled: bool,
) -> (egui::Response, Option<usize>) {
    dropdown_with_badges(ui, id_salt, options, &[], selected, enabled)
}

/// A small colored pill with a label, like Discord's "NEW" tags.
pub type Badge = (&'static str, Color32);

/// Like `dropdown`, with an optional badge at the right of each option
/// (`badges[i]` for `options[i]`; missing entries have none).
pub fn dropdown_with_badges(
    ui: &mut Ui,
    id_salt: &str,
    options: &[String],
    badges: &[Option<Badge>],
    selected: Option<usize>,
    enabled: bool,
) -> (egui::Response, Option<usize>) {
    let badge_of = |i: usize| badges.get(i).copied().flatten();
    let id = ui.make_persistent_id(id_salt);
    let mut open = enabled && ui.data(|d| d.get_temp::<bool>(id).unwrap_or(false));
    let sense = if enabled { Sense::click() } else { Sense::hover() };
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 40.0), sense);
    let resp = if enabled { resp.on_hover_cursor(CursorIcon::PointingHand) } else { resp };
    if resp.clicked() {
        open = !open;
    }

    let painter = ui.painter();
    painter.rect_filled(rect, CornerRadius::same(4), BG_TERTIARY);
    if open {
        painter.rect_stroke(rect, CornerRadius::same(4), Stroke::new(1.0, BLURPLE), egui::StrokeKind::Inside);
    } else if resp.hovered() {
        painter.rect_stroke(rect, CornerRadius::same(4), Stroke::new(1.0, BG_FLOATING), egui::StrokeKind::Inside);
    }
    let label = selected.and_then(|i| options.get(i)).map(String::as_str).unwrap_or("");
    let color = if enabled { TEXT_NORMAL } else { TEXT_MUTED };
    let mut text_rect = Rect::from_min_max(rect.min + vec2(12.0, 0.0), rect.max - vec2(36.0, 0.0));
    if let Some(b) = selected.and_then(badge_of) {
        let left = badge(painter, b, pos2(text_rect.right(), rect.center().y));
        text_rect.set_right(left - 8.0);
    }
    let galley = one_line(ui, label, font(15.0, Weight::Regular), color, text_rect.width());
    painter.galley(pos2(text_rect.left(), rect.center().y - galley.size().y / 2.0), galley, color);
    chevron(painter, pos2(rect.right() - 18.0, rect.center().y), open, color);

    let mut picked = None;
    if open {
        // Open upwards when the list doesn't fit below the box (otherwise
        // egui pushes it up over the box itself).
        let list_height = options.len() as f32 * 38.0 + 10.0;
        let screen = ui.ctx().content_rect();
        let upwards = rect.bottom() + 4.0 + list_height > screen.bottom() && rect.top() - 4.0 - list_height >= screen.top();
        let (pivot, at) = if upwards {
            (egui::Align2::LEFT_BOTTOM, rect.left_top() - vec2(0.0, 4.0))
        } else {
            (egui::Align2::LEFT_TOP, rect.left_bottom() + vec2(0.0, 4.0))
        };
        let popup = egui::Area::new(id.with("popup"))
            .order(egui::Order::Foreground)
            .pivot(pivot)
            .fixed_pos(at)
            .show(ui.ctx(), |ui| {
                egui::Frame::new()
                    .fill(BG_SECONDARY_ALT)
                    .stroke(Stroke::new(1.0, BG_FLOATING))
                    .corner_radius(CornerRadius::same(4))
                    .inner_margin(egui::Margin::same(4))
                    .show(ui, |ui| {
                        ui.set_width(rect.width() - 8.0);
                        ui.spacing_mut().item_spacing.y = 2.0;
                        for (i, option) in options.iter().enumerate() {
                            let (r, row) = ui.allocate_exact_size(vec2(ui.available_width(), 36.0), Sense::click());
                            let is_selected = selected == Some(i);
                            let fill = if row.hovered() {
                                BG_HOVER
                            } else if is_selected {
                                BG_SELECTED
                            } else {
                                Color32::TRANSPARENT
                            };
                            ui.painter().rect_filled(r, CornerRadius::same(3), fill);
                            let color = if is_selected || row.hovered() { Color32::WHITE } else { TEXT_NORMAL };
                            let mut text_right = r.right() - 34.0;
                            if let Some(b) = badge_of(i) {
                                text_right = badge(ui.painter(), b, pos2(text_right, r.center().y)) - 8.0;
                            }
                            let galley = one_line(ui, option, font(15.0, Weight::Regular), color, text_right - r.left() - 10.0);
                            ui.painter().galley(pos2(r.left() + 10.0, r.center().y - galley.size().y / 2.0), galley, color);
                            if is_selected {
                                // Blurple circle with a check, like Discord's selected option.
                                let c = pos2(r.right() - 18.0, r.center().y);
                                ui.painter().circle_filled(c, 8.0, BLURPLE);
                                let s = Stroke::new(1.8, Color32::WHITE);
                                ui.painter().line_segment([c + vec2(-3.5, 0.0), c + vec2(-1.0, 2.5)], s);
                                ui.painter().line_segment([c + vec2(-1.0, 2.5), c + vec2(3.5, -2.5)], s);
                            }
                            if row.on_hover_cursor(CursorIcon::PointingHand).clicked() {
                                picked = Some(i);
                            }
                        }
                    });
            });
        // Close on a pick, or a click anywhere outside the box and the list.
        if picked.is_some() || (popup.response.clicked_elsewhere() && !resp.clicked() && ui.input(|i| i.pointer.any_click())) {
            open = false;
        }
    }
    ui.data_mut(|d| d.insert_temp(id, open));
    (resp, picked)
}

/// Draws a badge whose right edge is at `right_center`; returns its left edge.
fn badge(painter: &egui::Painter, (label, color): Badge, right_center: egui::Pos2) -> f32 {
    let galley = painter.layout_no_wrap(label.to_uppercase(), font(11.0, Weight::Bold), Color32::WHITE);
    let size = galley.size() + vec2(12.0, 4.0);
    let rect = Rect::from_min_size(pos2(right_center.x - size.x, right_center.y - size.y / 2.0), size);
    painter.rect_filled(rect, CornerRadius::same(4), color);
    painter.galley(rect.center() - galley.size() / 2.0, galley, Color32::WHITE);
    rect.left()
}

/// A single line of text, cut off with "…" if wider than `max_width`.
fn one_line(ui: &Ui, text: &str, font: egui::FontId, color: Color32, max_width: f32) -> std::sync::Arc<egui::Galley> {
    let mut job = egui::text::LayoutJob::simple_singleline(text.to_string(), font, color);
    job.wrap = egui::text::TextWrapping {
        max_width: max_width.max(10.0),
        max_rows: 1,
        break_anywhere: true,
        overflow_character: Some('…'),
    };
    ui.painter().layout_job(job)
}

/// A small down (or, when open, up) chevron.
fn chevron(painter: &egui::Painter, c: egui::Pos2, up: bool, color: Color32) {
    let dy = if up { -2.5 } else { 2.5 };
    let s = Stroke::new(2.0, color);
    painter.line_segment([c + vec2(-5.0, -dy), c + vec2(0.0, dy)], s);
    painter.line_segment([c + vec2(0.0, dy), c + vec2(5.0, -dy)], s);
}
