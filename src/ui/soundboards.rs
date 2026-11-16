//! The Soundboards page (#76): your soundboard servers that have the group's
//! bot, with their free slots and sounds, and adding the bot to more servers
//! from invite links. Only shown once a soundboard service is set up.
//!
//! Clicking a sound plays it (#81); right-clicking one the bot added opens a
//! menu to edit or delete it.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use eframe::egui::{self, pos2, vec2, Align, Align2, Color32, CornerRadius, CursorIcon, Frame, Layout, Margin, Pos2, Sense, Ui};

use super::confirm::ConfirmDialog;
use super::settings::{spawn_call, Pending};
use super::sound_edit::{self, SoundDialog};
use super::theme::*;
use super::upload::{load, Entry, Loaded};
use super::widgets::*;
use super::{send, Command, Snapshot};
use crate::player::Playback;
use crate::soundboard::{Board, Invites, Service, Sound};

/// A sound on one server.
#[derive(Clone, PartialEq)]
struct SoundRef {
    server: String,
    sound: String,
}

/// What was clicked on a server card.
enum Click {
    Play(String),
    Menu(SoundRef, Pos2),
}

#[derive(Default)]
pub struct SoundboardsPage {
    /// The servers and their soundboards; `None` = load them.
    loaded: Option<Arc<Mutex<Loaded>>>,
    /// The address they were loaded from (reload when it changes).
    loaded_from: String,
    invite_text: String,
    invites: Option<Pending<Invites>>,
    /// The sound playing, by id (#81).
    playing: Option<(String, Playback)>,
    /// A sound being downloaded to play.
    fetching: Option<(String, Pending<PathBuf>)>,
    /// The right-click menu of one of the bot's sounds, and where it opened.
    menu: Option<(SoundRef, Pos2)>,
    edit: Option<SoundDialog>,
    /// Asking before deleting this sound (its name for the dialog).
    confirm: Option<(SoundRef, ConfirmDialog)>,
    deleting: Option<(SoundRef, Pending<()>)>,
    /// The last error, shown above the servers.
    error: Option<String>,
}

impl SoundboardsPage {
    /// Stops the sound playing (another page opened, or the window hid).
    pub fn stop(&mut self) {
        self.playing = None;
        self.fetching = None;
    }

    /// The window is being hidden: stop playing and close the dialogs.
    pub fn on_hide(&mut self) {
        self.stop();
        self.menu = None;
        self.edit = None;
        self.confirm = None;
    }

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
        let loaded = loaded.clone();
        self.poll(ui.ctx(), &loaded);
        if let Some(e) = &self.error {
            status_line(ui, Some(pal().red), e, pal().red);
            ui.add_space(12.0);
        }
        let state = ChipState {
            playing: self.playing.as_ref().map(|(id, _)| id.clone()).or(self.fetching.as_ref().map(|(id, _)| id.clone())),
            busy: self.deleting.as_ref().map(|(r, _)| r.sound.clone()),
        };
        let click = servers(ui, &loaded.lock().unwrap(), &state);
        // The open menu first: a click outside closes it before anything new
        // opens (a menu opened this frame mustn't see its own click).
        self.show_menu(ui, &loaded);
        match click {
            Some(Click::Play(id)) => self.toggle_play(ui.ctx(), id),
            Some(Click::Menu(r, at)) => self.menu = Some((r, at)),
            None => {}
        }
        self.show_dialogs(ui.ctx(), &service, &loaded);

        divider(ui);
        // The form keeps the reading width; the cards above use the window.
        ui.scope(|ui| {
            ui.set_max_width(ui.available_width().min(super::READING_WIDTH));
            self.add_servers(ui, service);
        });
    }

    /// Checks the download, the playback and a delete in progress.
    fn poll(&mut self, ctx: &egui::Context, loaded: &Arc<Mutex<Loaded>>) {
        if let Some((id, pending)) = &self.fetching {
            let done = pending.lock().unwrap().take();
            if let Some(result) = done {
                let id = id.clone();
                self.fetching = None;
                match result.and_then(|path| Playback::start(&path, 0.0, Default::default()).map_err(|e| format!("{e:#}"))) {
                    Ok(p) => {
                        self.playing = Some((id, p));
                        self.error = None;
                    }
                    Err(e) => {
                        crate::log!("playing soundboard sound {id} failed: {e}");
                        self.error = Some(format!("Couldn't play the sound: {e}"));
                    }
                }
            }
        }
        if self.playing.as_ref().is_some_and(|(_, p)| p.finished()) {
            self.playing = None;
        }
        if self.playing.is_some() || self.fetching.is_some() {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
        if let Some((r, pending)) = &self.deleting {
            let done = pending.lock().unwrap().take();
            if let Some(result) = done {
                let r = r.clone();
                self.deleting = None;
                match result {
                    Ok(()) => {
                        if let Some(b) = board_mut(&mut loaded.lock().unwrap(), &r.server) {
                            b.sounds.retain(|s| s.id != r.sound);
                            b.used = b.used.saturating_sub(1);
                        }
                        self.error = None;
                    }
                    Err(e) => {
                        crate::log!("deleting soundboard sound {} failed: {e}", r.sound);
                        self.error = Some(format!("Couldn't delete the sound: {e}"));
                    }
                }
            }
        }
    }

    /// Plays a sound, or stops it when it's the one playing. Starting one
    /// stops the last.
    fn toggle_play(&mut self, ctx: &egui::Context, id: String) {
        let same = self.playing.as_ref().map(|(p, _)| p) == Some(&id) || self.fetching.as_ref().map(|(p, _)| p) == Some(&id);
        self.stop();
        if same {
            return;
        }
        let fetch_id = id.clone();
        self.fetching = Some((id, spawn_call(ctx, move || crate::soundboard::fetch_sound(&fetch_id))));
    }

    /// The right-click menu of one of the bot's sounds: Edit, Delete.
    fn show_menu(&mut self, ui: &Ui, loaded: &Arc<Mutex<Loaded>>) {
        let Some((r, at)) = self.menu.clone() else { return };
        let mut picked = None;
        let area = egui::Area::new(egui::Id::unique("sound-menu"))
            .order(egui::Order::Foreground)
            .fixed_pos(at)
            .show(ui.ctx(), |ui| {
                Frame::new()
                    .fill(pal().bg_floating)
                    .corner_radius(CornerRadius::same(4))
                    .inner_margin(Margin::same(6))
                    .show(ui, |ui| {
                        ui.set_width(160.0);
                        ui.spacing_mut().item_spacing.y = 2.0;
                        for (label, danger) in [("Edit sound…", false), ("Delete sound", true)] {
                            let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 32.0), Sense::click());
                            if resp.hovered() {
                                ui.painter().rect_filled(rect, CornerRadius::same(3), if danger { RED_BTN } else { BLURPLE });
                            }
                            let color = match (resp.hovered(), danger) {
                                (true, _) => Color32::WHITE,
                                (false, true) => pal().red,
                                (false, false) => pal().text_normal,
                            };
                            ui.painter().text(pos2(rect.left() + 10.0, rect.center().y), Align2::LEFT_CENTER, label, font(14.0, Weight::Semibold), color);
                            if resp.on_hover_cursor(CursorIcon::PointingHand).clicked() {
                                picked = Some(danger);
                            }
                        }
                    });
            });
        let escape = ui.input(|i| i.key_pressed(egui::Key::Escape));
        let outside = area.response.clicked_elsewhere() && ui.input(|i| i.pointer.any_click());
        if picked.is_none() && !escape && !outside {
            return;
        }
        self.menu = None;
        let l = loaded.lock().unwrap();
        let Some(entry) = l.entries.iter().find(|e| e.server.id == r.server) else { return };
        let Some(Ok(Some(board))) = &entry.board else { return };
        let Some(sound) = board.sounds.iter().find(|s| s.id == r.sound) else { return };
        match picked {
            Some(false) => self.edit = Some(SoundDialog::open(&r.server, &entry.server.name, sound)),
            Some(true) => {
                let dialog = ConfirmDialog::new(
                    format!("Delete \u{201c}{}\u{201d}?", sound.name.trim()),
                    format!("It's removed from {}'s soundboard for everyone. This can't be undone.", entry.server.name),
                    "Delete",
                    ButtonKind::Danger,
                );
                self.confirm = Some((r, dialog));
            }
            None => {}
        }
    }

    fn show_dialogs(&mut self, ctx: &egui::Context, service: &Service, loaded: &Arc<Mutex<Loaded>>) {
        let user = loaded.lock().unwrap().user.clone();
        if let Some(dialog) = &mut self.edit {
            match dialog.show(ctx, service, user.as_deref()) {
                Some(sound_edit::Outcome::Saved { name, emoji, volume }) => {
                    let (server, id) = (dialog.server.clone(), dialog.sound.id.clone());
                    if let Some(s) = board_mut(&mut loaded.lock().unwrap(), &server).and_then(|b| b.sounds.iter_mut().find(|s| s.id == id)) {
                        s.name = name;
                        s.emoji = Some(emoji).filter(|e| !e.is_empty());
                        s.volume = volume;
                    }
                    self.edit = None;
                    self.error = None;
                }
                Some(sound_edit::Outcome::Cancelled) => self.edit = None,
                None => {}
            }
        }
        if let Some((r, dialog)) = &self.confirm {
            match dialog.show(ctx) {
                Some(true) => {
                    let r = r.clone();
                    self.confirm = None;
                    match user {
                        Some(user) => {
                            let (service, server, sound) = (service.clone(), r.server.clone(), r.sound.clone());
                            let pending = spawn_call(ctx, move || service.delete_sound(&server, &sound, &user));
                            self.deleting = Some((r, pending));
                        }
                        None => self.error = Some("Open Discord first: the app needs it to know who you are.".into()),
                    }
                }
                Some(false) => self.confirm = None,
                None => {}
            }
        }
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

/// What the sound chips show: the sound playing (or loading to play), and
/// one being deleted.
struct ChipState {
    playing: Option<String>,
    busy: Option<String>,
}

/// One server's soundboard in `l`, to change after an edit or delete.
fn board_mut<'a>(l: &'a mut Loaded, server: &str) -> Option<&'a mut Board> {
    match &mut l.entries.iter_mut().find(|e| e.server.id == server)?.board {
        Some(Ok(Some(b))) => Some(b),
        _ => None,
    }
}

/// The server cards.
fn servers(ui: &mut Ui, l: &Loaded, state: &ChipState) -> Option<Click> {
    if let Some(e) = &l.error {
        status_line(ui, Some(pal().red), e, pal().red);
        return None;
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
        return None;
    }
    let mut click = None;
    for e in shown {
        click = server_card(ui, e, state).or(click);
        ui.add_space(12.0);
    }
    if pending {
        text(ui, "Checking more servers…", 13.0, Weight::Regular, pal().text_muted);
    }
    click
}

fn server_card(ui: &mut Ui, e: &Entry, state: &ChipState) -> Option<Click> {
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
                    sounds(ui, &e.server.id, b, state)
                }
                Some(Err(err)) => {
                    ui.add_space(6.0);
                    text(ui, err.as_str(), 14.0, Weight::Regular, pal().red);
                    None
                }
                _ => None,
            }
        })
        .inner
}

fn slots_text(b: &Board) -> String {
    match b.total {
        Some(t) if b.used >= t => format!("Full · {}/{t}", b.used),
        Some(t) => format!("{} of {t} slots free", t - b.used),
        None => format!("{} sounds", b.used),
    }
}

/// The server's sounds as small chips: click to play (#81), right-click the
/// bot's own to edit or delete them. The bot's sounds and people's are in
/// two labelled groups, so it's clear which can be right-clicked; with no
/// bot sounds there are no labels.
fn sounds(ui: &mut Ui, server: &str, b: &Board, state: &ChipState) -> Option<Click> {
    if b.sounds.is_empty() {
        text(ui, "No sounds yet.", 14.0, Weight::Regular, pal().text_muted);
        return None;
    }
    let (bot, people): (Vec<&Sound>, Vec<&Sound>) = b.sounds.iter().partition(|s| s.by_bot);
    if bot.is_empty() {
        return chips(ui, server, &people, state);
    }
    let mut click = None;
    group_label(ui, "Added with Discord Clip", Some("right-click to edit or delete"));
    click = chips(ui, server, &bot, state).or(click);
    if !people.is_empty() {
        ui.add_space(14.0);
        group_label(ui, "Added in Discord", None);
        click = chips(ui, server, &people, state).or(click);
    }
    click
}

/// A small heading above a group of sounds, with an optional hint.
fn group_label(ui: &mut Ui, title: &str, hint: Option<&str>) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        text(ui, title.to_uppercase(), 12.0, Weight::Bold, pal().header_secondary);
        if let Some(h) = hint {
            text(ui, format!("· {h}"), 12.0, Weight::Regular, pal().text_muted);
        }
    });
    ui.add_space(8.0);
}

fn chips(ui: &mut Ui, server: &str, sounds: &[&Sound], state: &ChipState) -> Option<Click> {
    let mut click = None;
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
        for s in sounds {
            if let Some(c) = chip(ui, server, s, state) {
                click = Some(c);
            }
        }
    });
    click
}

fn chip(ui: &mut Ui, server: &str, s: &Sound, state: &ChipState) -> Option<Click> {
    let playing = state.playing.as_deref() == Some(s.id.as_str()) && !s.id.is_empty();
    let busy = state.busy.as_deref() == Some(s.id.as_str()) && !s.id.is_empty();
    let label = match &s.emoji {
        Some(e) if !e.chars().all(|c| c.is_ascii_digit()) => format!("{e} {}", s.name),
        _ => s.name.clone(),
    };
    let color = if playing {
        Color32::WHITE
    } else if s.available && !busy {
        pal().text_normal
    } else {
        pal().text_muted
    };
    // Each label stays on one line; a full row continues on the next.
    let galley = ui.painter().layout_no_wrap(label, font(14.0, Weight::Regular), color);
    let can_play = !s.id.is_empty() && !busy;
    let sense = if can_play { Sense::click() } else { Sense::hover() };
    let (rect, resp) = ui.allocate_exact_size(galley.size() + vec2(20.0, 8.0), sense);
    let fill = if playing {
        BLURPLE
    } else if resp.hovered() && can_play {
        pal().bg_hover
    } else {
        pal().bg_tertiary
    };
    ui.painter().rect_filled(rect, CornerRadius::same(12), fill);
    ui.painter().galley(rect.center() - galley.size() / 2.0, galley, color);
    let mut tip = Vec::new();
    if busy {
        tip.push("Deleting…");
    } else if can_play {
        tip.push(if playing { "Click to stop" } else { "Click to play" });
        if s.by_bot {
            tip.push("Right-click to edit or delete");
        }
    }
    if !s.available {
        tip.push("Turned off: the server lost the boost for this slot");
    }
    let resp = if can_play { resp.on_hover_cursor(CursorIcon::PointingHand) } else { resp };
    let resp = if tip.is_empty() { resp } else { resp.on_hover_text(tip.join("\n")) };
    if resp.clicked() {
        return Some(Click::Play(s.id.clone()));
    }
    if resp.secondary_clicked() && s.by_bot && !busy {
        let at = resp.interact_pointer_pos().unwrap_or(rect.left_bottom());
        return Some(Click::Menu(SoundRef { server: server.into(), sound: s.id.clone() }, at));
    }
    None
}
