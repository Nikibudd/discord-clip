//! "Upload to soundboard" (#76): pick one of your soundboard servers, name
//! the sound and upload a Discord-ready clip through the group's soundboard
//! service. Network work runs on worker threads; the dialog shows what they
//! have so far.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use eframe::egui::{self, pos2, vec2, Align, Align2, Color32, CornerRadius, CursorIcon, Frame, Layout, Margin, Sense, Stroke};

use super::theme::*;
use super::widgets::*;
use crate::soundboard::{Board, Server, Service, Upload, MAX_NAME, MIN_NAME};

/// How the dialog ended.
pub enum Outcome {
    Cancelled,
    /// Uploaded to this server.
    Uploaded(crate::uploaded::Server),
}

/// One server in the list. `board`: `None` while loading, `Ok(None)` when
/// you're not in it (hidden).
pub(super) struct Entry {
    pub(super) server: Server,
    pub(super) board: Option<Result<Option<Board>, String>>,
}

#[derive(Default)]
pub(super) struct Loaded {
    /// Your Discord id; `None` until known.
    pub(super) user: Option<String>,
    /// The list is still loading.
    pub(super) loading: bool,
    pub(super) error: Option<String>,
    pub(super) entries: Vec<Entry>,
}

pub struct UploadDialog {
    service: Service,
    clip: PathBuf,
    name: String,
    /// An emoji from the picker, or empty for none.
    emoji: String,
    emoji_picker: super::emoji_picker::EmojiPicker,
    /// 0..=100 (%).
    volume: f32,
    selected: Option<String>,
    loaded: Arc<Mutex<Loaded>>,
    /// Set while uploading; holds the answer once it's there.
    /// The upload in progress; gives the new sound's id.
    sending: Option<Arc<Mutex<Option<Result<String, String>>>>>,
    error: Option<String>,
}

impl UploadDialog {
    pub fn open(ctx: &egui::Context, service: Service, clip: &Path) -> UploadDialog {
        let stem = clip.file_stem().and_then(|s| s.to_str()).unwrap_or("clip");
        let loaded = Arc::new(Mutex::new(Loaded { loading: true, ..Default::default() }));
        load(ctx.clone(), service.clone(), loaded.clone());
        UploadDialog {
            service,
            clip: clip.to_path_buf(),
            name: crate::soundboard::default_name(stem),
            emoji: String::new(),
            emoji_picker: Default::default(),
            volume: 100.0,
            selected: None,
            loaded,
            sending: None,
            error: None,
        }
    }

    pub fn clip(&self) -> &Path {
        &self.clip
    }

    /// Draws the dialog; returns how it ended once it has.
    pub fn show(&mut self, ctx: &egui::Context) -> Option<Outcome> {
        // An upload finished?
        if let Some(done) = self.sending.as_ref().and_then(|s| s.lock().unwrap().take()) {
            self.sending = None;
            match done {
                Ok(sound) => {
                    let id = self.selected.clone().unwrap_or_default();
                    let name = self.server_name().unwrap_or_default();
                    // The sound id lets the Soundboards page tell it's in Clips (#91).
                    return Some(Outcome::Uploaded(crate::uploaded::Server { id, name, sound }));
                }
                Err(e) => self.error = Some(e),
            }
        }
        let busy = self.sending.is_some();
        // Clicks and Esc go to the emoji grid while it's open, not to the dialog.
        let picking = self.emoji_picker.open;
        let mut outcome = None;
        let width = (ctx.content_rect().width() - 80.0).clamp(320.0, 480.0);
        let modal = egui::Modal::new(egui::Id::unique("upload-dialog"))
            .backdrop_color(Color32::from_black_alpha(170))
            .frame(Frame::new().fill(pal().bg_primary).corner_radius(CornerRadius::same(8)).inner_margin(Margin::same(20)))
            .show(ctx, |ui| {
                ui.set_width(width);
                ui.spacing_mut().item_spacing = vec2(8.0, 0.0);
                text(ui, "Upload to soundboard", 20.0, Weight::Bold, pal().header_primary);
                ui.add_space(6.0);
                let file = self.clip.file_name().and_then(|s| s.to_str()).unwrap_or("").to_string();
                text(ui, file, 14.0, Weight::Regular, pal().text_muted);
                ui.add_space(16.0);

                section_label(ui, "Server");
                self.server_list(ui, busy);
                ui.add_space(16.0);

                section_label(ui, "Name");
                let w = ui.available_width();
                ui.add_enabled_ui(!busy, |ui| text_field(ui, &mut self.name, "Name of the sound", false, w));
                if let Some(note) = self.duplicate_note() {
                    ui.add_space(6.0);
                    ui.add(egui::Label::new(egui::RichText::new(note).font(font(14.0, Weight::Regular)).color(pal().yellow)).wrap());
                }
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

                let problem = self.problem();
                let shown_error = self.error.clone().or(problem.clone().filter(|_| self.selected.is_some()));
                if let Some(e) = shown_error {
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
                            let label = if busy { "Uploading…" } else { "Upload" };
                            if button_enabled(ui, label, ButtonKind::Primary, 32.0, problem.is_none() && !busy).clicked() {
                                self.start_upload(ctx);
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
        if busy || self.loaded.lock().unwrap().loading {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
        outcome
    }

    /// Why Upload can't be pressed yet, if it can't.
    fn problem(&self) -> Option<String> {
        let n = self.name.trim().chars().count();
        if self.selected.is_none() {
            Some("Pick a server.".into())
        } else if !(MIN_NAME..=MAX_NAME).contains(&n) {
            Some(format!("The name must be {MIN_NAME} to {MAX_NAME} characters."))
        } else if self.loaded.lock().unwrap().user.is_none() {
            Some("Discord isn't running, so the app can't tell who you are.".into())
        } else {
            None
        }
    }

    /// A warning when the picked server already has a sound with this name
    /// (#82). It never blocks the upload.
    fn duplicate_note(&self) -> Option<String> {
        let sel = self.selected.as_ref()?;
        let l = self.loaded.lock().unwrap();
        let e = l.entries.iter().find(|e| &e.server.id == sel)?;
        let Some(Ok(Some(board))) = &e.board else { return None };
        let sound = board.same_name(&self.name)?;
        Some(format!("{} already has a sound called \u{201c}{}\u{201d}.", e.server.name, sound.name.trim()))
    }

    fn server_name(&self) -> Option<String> {
        let sel = self.selected.as_ref()?;
        let l = self.loaded.lock().unwrap();
        l.entries.iter().find(|e| &e.server.id == sel).map(|e| e.server.name.clone())
    }

    fn server_list(&mut self, ui: &mut egui::Ui, busy: bool) {
        let l = self.loaded.lock().unwrap();
        if let Some(e) = &l.error {
            text(ui, e.as_str(), 14.0, Weight::Regular, pal().red);
            return;
        }
        let visible: Vec<&Entry> = l.entries.iter().filter(|e| !matches!(e.board, Some(Ok(None)))).collect();
        let pending = l.loading || l.entries.iter().any(|e| e.board.is_none());
        if visible.is_empty() {
            let msg = if pending {
                "Loading your soundboard servers…"
            } else {
                "None of your servers has the bot yet. Add it on the Soundboards page."
            };
            text(ui, msg, 14.0, Weight::Regular, pal().header_secondary);
            return;
        }
        let mut pick = None;
        egui::ScrollArea::vertical().max_height(200.0).auto_shrink([false, true]).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 4.0;
            for e in &visible {
                let (detail, usable) = describe(e);
                let selected = self.selected.as_deref() == Some(e.server.id.as_str());
                let sense = if usable && !busy { Sense::click() } else { Sense::hover() };
                let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 40.0), sense);
                let fill = if selected {
                    mix(pal().bg_secondary, BLURPLE, 0.25)
                } else if resp.hovered() && usable {
                    pal().bg_hover
                } else {
                    pal().bg_secondary
                };
                ui.painter().rect_filled(rect, CornerRadius::same(4), fill);
                if selected {
                    ui.painter().rect_stroke(rect, CornerRadius::same(4), Stroke::new(1.0, BLURPLE), egui::StrokeKind::Inside);
                }
                let name_color = if usable { pal().header_primary } else { pal().text_muted };
                ui.painter().text(
                    pos2(rect.left() + 12.0, rect.center().y),
                    Align2::LEFT_CENTER,
                    &e.server.name,
                    font(15.0, Weight::Semibold),
                    name_color,
                );
                ui.painter().text(
                    pos2(rect.right() - 12.0, rect.center().y),
                    Align2::RIGHT_CENTER,
                    detail,
                    font(13.0, Weight::Regular),
                    pal().text_muted,
                );
                if usable && !busy {
                    let resp = resp.on_hover_cursor(CursorIcon::PointingHand);
                    if resp.clicked() {
                        pick = Some(e.server.id.clone());
                    }
                }
            }
        });
        if pending {
            ui.add_space(4.0);
            text(ui, "Checking more servers…", 13.0, Weight::Regular, pal().text_muted);
        }
        drop(l);
        if let Some(p) = pick {
            self.selected = Some(p);
            self.error = None;
        }
    }

    fn start_upload(&mut self, ctx: &egui::Context) {
        let (Some(guild), Some(user)) = (self.selected.clone(), self.loaded.lock().unwrap().user.clone()) else { return };
        self.error = None;
        let slot = Arc::new(Mutex::new(None));
        self.sending = Some(slot.clone());
        let service = self.service.clone();
        let (file, name, emoji, volume) = (self.clip.clone(), self.name.trim().to_string(), self.emoji.trim().to_string(), self.volume / 100.0);
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let up = Upload { user: &user, guild: &guild, name: &name, emoji: &emoji, volume, file: &file };
            let result = service.upload(&up).map_err(|e| format!("Couldn't upload: {e:#}"));
            if let Err(e) = &result {
                crate::log!("soundboard upload of {} failed: {e}", file.display());
            } else {
                crate::log!("uploaded {} to a soundboard", file.display());
            }
            *slot.lock().unwrap() = Some(result);
            ctx.request_repaint();
        });
    }
}

/// The right-hand text for a server, and whether it can be picked.
fn describe(e: &Entry) -> (String, bool) {
    if !e.server.can_upload {
        return ("bot can't add sounds here".into(), false);
    }
    match &e.board {
        None => ("…".into(), false),
        Some(Err(_)) => ("couldn't load".into(), false),
        Some(Ok(None)) => (String::new(), false),
        Some(Ok(Some(b))) => match b.total {
            Some(t) if b.used >= t => (format!("full · {}/{t}", b.used), false),
            Some(t) => (format!("{} of {t} slots free", t - b.used), true),
            None => (format!("{} sounds", b.used), true),
        },
    }
}

/// Loads who you are, the bot's servers, then each server's soundboard (which
/// also tells whether you're in it), all in the background.
pub(super) fn load(ctx: egui::Context, service: Service, loaded: Arc<Mutex<Loaded>>) {
    std::thread::spawn(move || {
        let user = crate::discord_voice::ask_own_id();
        let servers = service.servers();
        let mut l = loaded.lock().unwrap();
        l.user = user.clone();
        match servers {
            Err(e) => {
                l.error = Some(format!("Couldn't load your servers: {e:#}"));
                l.loading = false;
            }
            Ok(list) => {
                l.entries = list.into_iter().map(|server| Entry { server, board: None }).collect();
                l.loading = false;
                let Some(user) = user else {
                    l.error = Some("Open Discord first: the app needs it to know who you are.".into());
                    return;
                };
                let ids: Vec<String> = l.entries.iter().map(|e| e.server.id.clone()).collect();
                drop(l);
                for id in ids {
                    let (service, loaded, ctx, user) = (service.clone(), loaded.clone(), ctx.clone(), user.clone());
                    std::thread::spawn(move || {
                        let board = service.board(&id, &user).map_err(|e| format!("{e:#}"));
                        if let Some(e) = loaded.lock().unwrap().entries.iter_mut().find(|e| e.server.id == id) {
                            e.board = Some(board);
                        }
                        ctx.request_repaint();
                    });
                }
            }
        }
        ctx.request_repaint();
    });
}
