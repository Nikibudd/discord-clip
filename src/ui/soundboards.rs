//! The Soundboards page (#76): your soundboard servers that have the group's
//! bot, with their free slots and sounds, and adding the bot to more servers
//! from invite links. Only shown once a soundboard service is set up.

use std::sync::{Arc, Mutex};

use eframe::egui::{self, vec2, Align, CornerRadius, Frame, Layout, Margin, Ui};

use super::settings::{spawn_call, Pending};
use super::theme::*;
use super::upload::{load, Entry, Loaded};
use super::widgets::*;
use super::{send, Command, Snapshot};
use crate::soundboard::{Board, Invites, Service};

#[derive(Default)]
pub struct SoundboardsPage {
    /// The servers and their soundboards; `None` = load them.
    loaded: Option<Arc<Mutex<Loaded>>>,
    /// The address they were loaded from (reload when it changes).
    loaded_from: String,
    invite_text: String,
    invites: Option<Pending<Invites>>,
}

impl SoundboardsPage {
    pub fn show(&mut self, ui: &mut Ui, snap: &Snapshot) {
        let cfg = &snap.cfg;
        let width = ui.available_width();
        ui.allocate_ui_with_layout(vec2(width, 32.0), Layout::right_to_left(Align::Center), |ui| {
            if button(ui, "Refresh", ButtonKind::Secondary).clicked() {
                self.loaded = None;
            }
            ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                text(ui, "Soundboards", 20.0, Weight::Bold, pal().header_primary);
            });
        });
        ui.add_space(8.0);
        text(
            ui,
            "Your soundboard servers that have your group's bot. Upload clips from the Clips page: Discord-ready \
             clips have an Upload to soundboard button.",
            14.0,
            Weight::Regular,
            pal().header_secondary,
        );
        ui.add_space(20.0);

        let Some(service) = Service::configured(cfg) else {
            text(ui, "Set up the soundboard service in Settings first.", 15.0, Weight::Regular, pal().header_secondary);
            return;
        };
        if self.loaded_from != cfg.soundboard_url {
            self.loaded = None;
            self.invites = None;
        }
        let loaded = self.loaded.get_or_insert_with(|| {
            let l = Arc::new(Mutex::new(Loaded { loading: true, ..Default::default() }));
            load(ui.ctx().clone(), service.clone(), l.clone());
            l
        });
        self.loaded_from.clone_from(&cfg.soundboard_url);
        servers(ui, &loaded.lock().unwrap());

        divider(ui);
        // The form keeps the reading width; the cards above use the window.
        ui.scope(|ui| {
            ui.set_max_width(ui.available_width().min(super::READING_WIDTH));
            self.add_servers(ui, service);
        });
    }

    /// Paste invite links -> an "Add bot" button per server without the bot.
    fn add_servers(&mut self, ui: &mut Ui, service: Service) {
        section_label(ui, "Add more servers");
        text(
            ui,
            "Paste invite links to soundboard servers that don't have the bot yet. Each one gets a link to add \
             it; that needs Manage Server in that server.",
            14.0,
            Weight::Regular,
            pal().header_secondary,
        );
        ui.add_space(8.0);
        Frame::new()
            .fill(pal().bg_tertiary)
            .corner_radius(CornerRadius::same(4))
            .inner_margin(Margin::same(10))
            .show(ui, |ui| {
                ui.add(
                    egui::TextEdit::multiline(&mut self.invite_text)
                        .frame(Frame::NONE)
                        .font(font(15.0, Weight::Regular))
                        .text_color(pal().text_normal)
                        .hint_text(egui::RichText::new("https://discord.gg/…").color(pal().text_muted))
                        .desired_rows(3)
                        .desired_width(f32::INFINITY),
                );
            });
        ui.add_space(8.0);
        let busy = self.invites.as_ref().is_some_and(|p| p.lock().unwrap().is_none());
        let has_text = !self.invite_text.trim().is_empty();
        let label = if busy { "Looking…" } else { "Find servers" };
        if button_enabled(ui, label, ButtonKind::Primary, 32.0, has_text && !busy).clicked() {
            let text = self.invite_text.clone();
            // The bot may have been added somewhere since the list loaded.
            self.loaded = None;
            self.invites = Some(spawn_call(ui.ctx(), move || service.invites(&text)));
        }
        let Some(invites) = &self.invites else { return };
        let answer = invites.lock().unwrap();
        match &*answer {
            None => {}
            Some(Err(e)) => {
                ui.add_space(8.0);
                status_line(ui, Some(pal().red), e, pal().red);
            }
            Some(Ok(found)) => {
                ui.add_space(8.0);
                for s in &found.servers {
                    let width = ui.available_width();
                    ui.allocate_ui_with_layout(vec2(width, 40.0), Layout::right_to_left(Align::Center), |ui| {
                        match &s.install_url {
                            Some(link) if !s.has_bot => {
                                if button(ui, "Add bot", ButtonKind::Primary).clicked() {
                                    send(Command::OpenUrl(link.clone()));
                                }
                            }
                            _ => {
                                text(ui, "Bot added", 14.0, Weight::Regular, pal().text_muted);
                            }
                        }
                        ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                            text(ui, s.name.as_str(), 15.0, Weight::Semibold, pal().header_primary);
                        });
                    });
                }
                if !found.invalid.is_empty() {
                    let n = found.invalid.len();
                    let msg = if n == 1 {
                        "1 invite link didn't work (expired or wrong).".to_string()
                    } else {
                        format!("{n} invite links didn't work (expired or wrong).")
                    };
                    text(ui, msg, 14.0, Weight::Regular, pal().yellow);
                }
                if found.servers.iter().any(|s| !s.has_bot) {
                    ui.add_space(4.0);
                    text(ui, "After adding the bot, press Find servers again to check.", 13.0, Weight::Regular, pal().text_muted);
                }
            }
        }
    }
}

/// The server cards.
fn servers(ui: &mut Ui, l: &Loaded) {
    if let Some(e) = &l.error {
        status_line(ui, Some(pal().red), e, pal().red);
        return;
    }
    let mut shown: Vec<&Entry> = l.entries.iter().filter(|e| !matches!(e.board, Some(Ok(None)))).collect();
    shown.sort_by_key(|e| e.server.name.to_lowercase());
    let pending = l.loading || l.entries.iter().any(|e| e.board.is_none());
    if shown.is_empty() {
        let msg = if pending {
            "Loading your soundboard servers…"
        } else {
            "None of your servers has the bot yet. Add it to one below."
        };
        text(ui, msg, 15.0, Weight::Regular, pal().header_secondary);
        return;
    }
    for e in shown {
        server_card(ui, e);
        ui.add_space(12.0);
    }
    if pending {
        text(ui, "Checking more servers…", 13.0, Weight::Regular, pal().text_muted);
    }
}

fn server_card(ui: &mut Ui, e: &Entry) {
    Frame::new()
        .fill(pal().bg_secondary)
        .corner_radius(CornerRadius::same(8))
        .inner_margin(Margin::same(16))
        .show(ui, |ui| {
            let width = ui.available_width();
            ui.set_width(width);
            let slots = match &e.board {
                Some(Ok(Some(b))) => slots_text(b),
                Some(Err(_)) => "couldn't load".into(),
                _ => "…".into(),
            };
            ui.allocate_ui_with_layout(vec2(width, 22.0), Layout::right_to_left(Align::Center), |ui| {
                text(ui, slots, 14.0, Weight::Regular, pal().header_secondary);
                ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                    text(ui, e.server.name.as_str(), 17.0, Weight::Semibold, pal().header_primary);
                });
            });
            if !e.server.can_upload {
                ui.add_space(6.0);
                text(
                    ui,
                    "The bot can't add sounds here: give it the Create Expressions permission.",
                    14.0,
                    Weight::Regular,
                    pal().yellow,
                );
            }
            match &e.board {
                Some(Ok(Some(b))) => {
                    if let Some(total) = b.total {
                        ui.add_space(10.0);
                        progress_bar(ui, b.used as f32 / total.max(1) as f32);
                    }
                    ui.add_space(12.0);
                    sounds(ui, b);
                }
                Some(Err(err)) => {
                    ui.add_space(6.0);
                    text(ui, err.as_str(), 14.0, Weight::Regular, pal().red);
                }
                _ => {}
            }
        });
}

fn slots_text(b: &Board) -> String {
    match b.total {
        Some(t) if b.used >= t => format!("Full · {}/{t}", b.used),
        Some(t) => format!("{} of {t} slots free", t - b.used),
        None => format!("{} sounds", b.used),
    }
}

/// The server's sounds as small chips.
fn sounds(ui: &mut Ui, b: &Board) {
    if b.sounds.is_empty() {
        text(ui, "No sounds yet.", 14.0, Weight::Regular, pal().text_muted);
        return;
    }
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
        for s in &b.sounds {
            let label = match &s.emoji {
                Some(e) if !e.chars().all(|c| c.is_ascii_digit()) => format!("{e} {}", s.name),
                _ => s.name.clone(),
            };
            let color = if s.available { pal().text_normal } else { pal().text_muted };
            // Each label stays on one line; a full row continues on the next.
            let galley = ui.painter().layout_no_wrap(label, font(14.0, Weight::Regular), color);
            let (rect, resp) = ui.allocate_exact_size(galley.size() + vec2(20.0, 8.0), egui::Sense::hover());
            ui.painter().rect_filled(rect, CornerRadius::same(12), pal().bg_tertiary);
            ui.painter().galley(rect.center() - galley.size() / 2.0, galley, color);
            if !s.available {
                resp.on_hover_text("Turned off: the server lost the boost for this slot");
            }
        }
    });
}
