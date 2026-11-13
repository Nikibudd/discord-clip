//! A Discord-style confirm dialog (#30): a title, a short text, and Cancel
//! plus a confirm button in a footer strip. Esc or a click outside cancels,
//! Enter confirms. Used before actions that change a clip's file.

use eframe::egui::{self, vec2, Align, Color32, CornerRadius, Frame, Layout, Margin};

use super::theme::*;
use super::widgets::*;

pub struct ConfirmDialog {
    title: String,
    text: String,
    /// Label of the confirm button, and its kind (e.g. Danger for deleting).
    confirm: String,
    kind: ButtonKind,
}

impl ConfirmDialog {
    pub fn new(title: impl Into<String>, text: impl Into<String>, confirm: impl Into<String>, kind: ButtonKind) -> ConfirmDialog {
        ConfirmDialog { title: title.into(), text: text.into(), confirm: confirm.into(), kind }
    }

    /// Draws the dialog; returns `Some(true)` once confirmed, `Some(false)`
    /// once cancelled.
    pub fn show(&self, ctx: &egui::Context) -> Option<bool> {
        let mut answer = None;
        let width = (ctx.content_rect().width() - 80.0).clamp(280.0, 440.0);
        let modal = egui::Modal::new(egui::Id::unique("confirm-dialog"))
            .backdrop_color(Color32::from_black_alpha(170))
            .frame(Frame::new().fill(BG_PRIMARY).corner_radius(CornerRadius::same(8)).inner_margin(Margin::same(20)))
            .show(ctx, |ui| {
                ui.set_width(width);
                ui.spacing_mut().item_spacing = vec2(8.0, 0.0);
                text(ui, self.title.as_str(), 20.0, Weight::Bold, HEADER_PRIMARY);
                ui.add_space(10.0);
                text(ui, self.text.as_str(), 15.0, Weight::Regular, TEXT_NORMAL);
                ui.add_space(20.0);
                Frame::new()
                    .fill(BG_SECONDARY)
                    .corner_radius(CornerRadius::same(6))
                    .inner_margin(Margin::symmetric(12, 10))
                    .show(ui, |ui| {
                        let w = ui.available_width();
                        ui.allocate_ui_with_layout(vec2(w, 32.0), Layout::right_to_left(Align::Center), |ui| {
                            if button(ui, &self.confirm, self.kind).clicked() {
                                answer = Some(true);
                            }
                            ui.add_space(4.0);
                            if button(ui, "Cancel", ButtonKind::Secondary).clicked() {
                                answer = Some(false);
                            }
                        });
                    });
            });
        if answer.is_none() && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Enter)) {
            answer = Some(true);
        }
        if answer.is_none() && modal.should_close() {
            answer = Some(false);
        }
        answer
    }
}
