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
pub fn toggle(ui: &mut Ui, on: bool) -> egui::Response {
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

