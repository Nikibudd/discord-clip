//! "Edit sound" (#81): change the name, emoji and volume of a sound the
//! group's bot added. The change is sent through the soundboard service on a
//! worker thread; the dialog shows Saving… until it's done.

use eframe::egui::{self, vec2, Align, Color32, CornerRadius, Frame, Layout, Margin};

use super::settings::{spawn_call, Pending};
use super::theme::*;
use super::widgets::*;
use crate::soundboard::{Service, Sound, SoundEdit, MAX_NAME, MIN_NAME};

/// How the dialog ended.
pub enum Outcome {
    Cancelled,
    /// Saved: the sound's new name, emoji (empty for none) and volume (0..=1).
    Saved { name: String, emoji: String, volume: f32 },
}

pub struct SoundDialog {
    pub server: String,
    server_name: String,
    pub sound: Sound,
    name: String,
    emoji: String,
    emoji_picker: super::emoji_picker::EmojiPicker,
    /// 0..=100 (%).
    volume: f32,
    sending: Option<Pending<()>>,
    error: Option<String>,
}

impl SoundDialog {
    pub fn open(server: &str, server_name: &str, sound: &Sound) -> SoundDialog {
        let emoji = sound.emoji.clone().filter(|e| !e.chars().all(|c| c.is_ascii_digit())).unwrap_or_default();
        SoundDialog {
            server: server.into(),
            server_name: server_name.into(),
            sound: sound.clone(),
            name: sound.name.clone(),
            emoji,
            emoji_picker: Default::default(),
            volume: (sound.volume * 100.0).round(),
            sending: None,
            error: None,
        }
    }

    /// Draws the dialog; returns how it ended once it has.
    pub fn show(&mut self, ctx: &egui::Context, service: &Service, user: Option<&str>) -> Option<Outcome> {
        if let Some(done) = self.sending.as_ref().and_then(|s| s.lock().unwrap().take()) {
            self.sending = None;
            match done {
                Ok(()) => {
                    return Some(Outcome::Saved {
                        name: self.name.trim().to_string(),
                        emoji: self.emoji.trim().to_string(),
                        volume: self.volume / 100.0,
                    })
                }
                Err(e) => self.error = Some(format!("Couldn't save: {e}")),
            }
        }
        let busy = self.sending.is_some();
        let picking = self.emoji_picker.open;
        let problem = self.problem(user);
        let mut outcome = None;
        let width = (ctx.content_rect().width() - 80.0).clamp(320.0, 440.0);
        let modal = egui::Modal::new(egui::Id::unique("sound-edit-dialog"))
            .backdrop_color(Color32::from_black_alpha(170))
            .frame(Frame::new().fill(pal().bg_primary).corner_radius(CornerRadius::same(8)).inner_margin(Margin::same(20)))
            .show(ctx, |ui| {
                ui.set_width(width);
                ui.spacing_mut().item_spacing = vec2(8.0, 0.0);
                text(ui, "Edit sound", 20.0, Weight::Bold, pal().header_primary);
                ui.add_space(6.0);
                text(ui, self.server_name.as_str(), 14.0, Weight::Regular, pal().text_muted);
                ui.add_space(16.0);

                section_label(ui, "Name");
                let w = ui.available_width();
                ui.add_enabled_ui(!busy, |ui| text_field(ui, &mut self.name, "Name of the sound", false, w));
                ui.add_space(16.0);

                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.set_width(80.0);
                        section_label(ui, "Emoji");
                        self.emoji_picker.show(ui, &mut self.emoji, !busy);
                    });
                    ui.add_space(16.0);
                    ui.vertical(|ui| {
                        section_label(ui, &format!("Volume · {}%", self.volume.round() as u32));
                        ui.add_space(6.0);
                        slider(ui, &mut self.volume, 0.0..=100.0, 1.0, !busy);
                    });
                });

                if let Some(e) = self.error.clone().or(problem.clone()) {
                    ui.add_space(12.0);
                    text(ui, e, 14.0, Weight::Regular, pal().red);
                }
                ui.add_space(20.0);
                Frame::new()
                    .fill(pal().bg_secondary)
                    .corner_radius(CornerRadius::same(6))
                    .inner_margin(Margin::symmetric(12, 10))
                    .show(ui, |ui| {
                        let w = ui.available_width();
                        ui.allocate_ui_with_layout(vec2(w, 32.0), Layout::right_to_left(Align::Center), |ui| {
                            let label = if busy { "Saving…" } else { "Save" };
                            if button_enabled(ui, label, ButtonKind::Primary, 32.0, problem.is_none() && !busy).clicked() {
                                if let Some(user) = user {
                                    self.start_save(ctx, service.clone(), user.to_string());
                                }
                            }
                            ui.add_space(4.0);
                            if button_enabled(ui, "Cancel", ButtonKind::Secondary, 32.0, !busy).clicked() {
                                outcome = Some(Outcome::Cancelled);
                            }
                        });
                    });
            });
        if outcome.is_none() && !busy && !picking && modal.should_close() {
            outcome = Some(Outcome::Cancelled);
        }
        outcome
    }

    /// Why Save can't be pressed, if it can't.
    fn problem(&self, user: Option<&str>) -> Option<String> {
        let n = self.name.trim().chars().count();
        if !(MIN_NAME..=MAX_NAME).contains(&n) {
            Some(format!("The name must be {MIN_NAME} to {MAX_NAME} characters."))
        } else if user.is_none() {
            Some("Discord isn't running, so the app can't tell who you are.".into())
        } else {
            None
        }
    }

    /// Sends only what changed.
    fn start_save(&mut self, ctx: &egui::Context, service: Service, user: String) {
        self.error = None;
        let old_emoji = self.sound.emoji.clone().unwrap_or_default();
        let edit = SoundEdit {
            name: Some(self.name.trim().to_string()).filter(|n| *n != self.sound.name),
            emoji: Some(self.emoji.trim().to_string()).filter(|e| *e != old_emoji),
            volume: Some(self.volume / 100.0).filter(|v| (v - self.sound.volume).abs() > 0.004),
        };
        let (server, sound) = (self.server.clone(), self.sound.id.clone());
        self.sending = Some(spawn_call(ctx, move || {
            if edit.name.is_none() && edit.emoji.is_none() && edit.volume.is_none() {
                return Ok(()); // nothing changed
            }
            service.edit_sound(&server, &sound, &user, &edit)
        }));
    }
}
