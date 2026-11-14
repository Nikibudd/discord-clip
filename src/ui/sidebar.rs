//! The left sidebar: page list, and a status panel at the bottom like
//! Discord's user panel.

use eframe::egui::{self, pos2, vec2, Align2, Color32, CornerRadius, CursorIcon, Frame, Margin, Rect, Sense, Ui, Vec2};

use super::theme::*;
use super::widgets::*;
use super::{Icons, Page, Snapshot};
use crate::update::State;

pub fn show(ui: &mut Ui, page: &mut Page, snap: &Snapshot, icons: &Icons) {
    egui::Panel::bottom("status")
        .exact_size(52.0)
        .resizable(false)
        .show_separator_line(false)
        .frame(Frame::new().fill(panel_fill()).inner_margin(Margin::symmetric(8, 0)))
        .show(ui, |ui| {
            // A Nitro gradient shows through, a little lighter than the
            // sidebar, like Discord's user panel (#46).
            if let Some(b) = backdrop() {
                let rect = ui.max_rect().expand2(vec2(8.0, 0.0));
                b.paint(&ui.ctx().layer_painter(ui.layer_id()), ui.ctx().content_rect(), rect, b.panel);
            }
            status_panel(ui, snap, icons)
        });

    Frame::new().inner_margin(Margin { left: 10, right: 10, top: 24, bottom: 8 }).show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.add_space(10.0);
            text(ui, "DISCORD CLIP", 12.0, Weight::Bold, pal().text_muted);
        });
        ui.add_space(8.0);
        for p in Page::ALL {
            let badge = p == Page::Updates
                && matches!(snap.update.state, State::Available(_) | State::Downloading { .. } | State::Ready { .. });
            if nav_item(ui, p.title(), *page == p, badge).clicked() {
                *page = p;
            }
            ui.add_space(2.0);
        }
    });
}

/// The status panel's own fill: none when a gradient is painted behind it.
fn panel_fill() -> Color32 {
    if backdrop().is_some() { Color32::TRANSPARENT } else { pal().bg_secondary_alt }
}

/// `badge`: Discord's red notification pill on the right.
fn nav_item(ui: &mut Ui, label: &str, selected: bool, badge: bool) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 32.0), Sense::click());
    let (fill, color) = if selected {
        (pal().bg_selected, pal().header_primary)
    } else if resp.hovered() {
        (pal().bg_hover, pal().text_normal)
    } else {
        (Color32::TRANSPARENT, pal().header_secondary)
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
        painter.rect_filled(pill, CornerRadius::same(8), pal().red);
        painter.text(pill.center(), Align2::CENTER_CENTER, "1", font(12.0, Weight::Bold), Color32::WHITE);
    }
    resp.on_hover_cursor(CursorIcon::PointingHand)
}

/// Like Discord's user panel: avatar with a status dot, name and status.
fn status_panel(ui: &mut Ui, snap: &Snapshot, icons: &Icons) {
    let (status, dot) = if snap.discord_active {
        ("Capturing Discord", pal().green)
    } else if snap.discord.starts_with("error") {
        ("Capture error", pal().red)
    } else {
        ("Waiting for Discord", pal().toggle_off)
    };
    let width = ui.available_width();
    let (rect, resp) = ui.allocate_exact_size(vec2(width, 52.0), Sense::hover());
    let painter = ui.painter();
    let avatar = Rect::from_center_size(pos2(rect.left() + 18.0, rect.center().y), Vec2::splat(32.0));
    let tex = if snap.discord_active { &icons.active } else { &icons.idle };
    painter.image(tex.id(), avatar, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
    let dot_c = avatar.right_bottom() - vec2(4.0, 4.0);
    // The ring around the dot matches the panel behind it.
    let ring = match backdrop() {
        Some(b) => b.at(ui.ctx().content_rect(), dot_c, b.panel),
        None => pal().bg_secondary_alt,
    };
    painter.circle_filled(dot_c, 7.0, ring);
    painter.circle_filled(dot_c, 5.0, dot);
    let x = avatar.right() + 8.0;
    painter.text(pos2(x, rect.center().y - 1.0), Align2::LEFT_BOTTOM, "Discord Clip", font(14.0, Weight::Semibold), pal().header_primary);
    let mic = match snap.mic.as_str() {
        "on" => "mic on",
        "off" => "mic off",
        "fallback" => "default mic",
        _ => "mic unavailable",
    };
    painter.text(
        pos2(x, rect.center().y + 1.0),
        Align2::LEFT_TOP,
        format!("{status} · {mic}"),
        font(12.0, Weight::Regular),
        pal().header_secondary,
    );
    resp.on_hover_text(&snap.discord);
}
