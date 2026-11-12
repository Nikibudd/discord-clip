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
        .frame(Frame::new().fill(BG_SECONDARY_ALT).inner_margin(Margin::symmetric(8, 0)))
        .show(ui, |ui| status_panel(ui, snap, icons));

    Frame::new().inner_margin(Margin { left: 10, right: 10, top: 24, bottom: 8 }).show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.add_space(10.0);
            text(ui, "DISCORD CLIP", 12.0, Weight::Bold, TEXT_MUTED);
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

/// `badge`: Discord's red notification pill on the right.
fn nav_item(ui: &mut Ui, label: &str, selected: bool, badge: bool) -> egui::Response {
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
fn status_panel(ui: &mut Ui, snap: &Snapshot, icons: &Icons) {
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
    let tex = if snap.discord_active { &icons.active } else { &icons.idle };
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
