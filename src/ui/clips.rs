//! The Clips page: the clips folder as a grid of cards, each with a
//! waveform player (play/pause, click or drag on the waveform to scrub) and
//! a Make Discord-ready button and a ⋯ menu (Copy, Edit, Rename, ...).
//!
//! Listing the folder is cheap and redone every couple of seconds while the
//! page is open, so new clips appear on their own. Reading a clip's waveform
//! means reading the whole file, so that happens on a worker thread and the
//! card shows a placeholder until it's done.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use eframe::egui::{
    self, pos2, vec2, Align, Align2, Color32, CornerRadius, CursorIcon, Frame, Layout, Margin, Painter, Rect, Sense,
    Stroke, Ui,
};

use super::theme::*;
use super::widgets::*;
use super::{send, Command, Snapshot};
use crate::library::{self, Analysis, ClipFile, SavedAt};
use crate::player::Playback;
use crate::uploaded;
use crate::volumes::{self, Volumes};

const RESCAN_EVERY: Duration = Duration::from_secs(2);
const TILE_MIN_WIDTH: f32 = 250.0;
const TILE_HEIGHT: f32 = 186.0;
const GAP: f32 = 16.0;
/// The page's widest layout: six cards of 300 px. A wider window leaves the
/// rest empty rather than stretch the cards (#45).
pub(super) const MAX_WIDTH: f32 = 6.0 * 300.0 + 5.0 * GAP;

type Analyses = Arc<Mutex<HashMap<PathBuf, (SystemTime, Result<Analysis, String>)>>>;

#[derive(Default)]
pub struct ClipsPage {
    dir: Option<PathBuf>,
    clips: Vec<ClipFile>,
    last_scan: Option<Instant>,
    analyses: Analyses,
    /// Sent to the worker (path, modified) and not re-sent until it changes.
    queued: HashMap<PathBuf, SystemTime>,
    worker: Option<Sender<(PathBuf, SystemTime)>>,
    /// The clip that's playing or paused (only one at a time).
    player: Option<Playback>,
    /// Deleted from the page; hidden until the next scan confirms it.
    deleted: HashSet<PathBuf>,
    toast: Option<(String, Instant)>,
    /// Text in the search box.
    search: String,
    /// The clip whose name is being edited, and the text so far.
    renaming: Option<(PathBuf, String)>,
    /// The Edit dialog, while it's open.
    edit: Option<super::edit::EditDialog>,
    /// The clip whose ⋯ menu is open.
    menu: Option<PathBuf>,
    /// Stored track volumes of all clips (#7), reloaded with each scan.
    volumes: volumes::Table,
    /// Clips marked as uploaded (#28), reloaded with each scan.
    uploaded: uploaded::Set,
    /// A confirm dialog (#30), and the clip to make Discord-ready once confirmed.
    confirm: Option<(super::confirm::ConfirmDialog, ClipFile)>,
    /// Which clips are shown (#29). Not saved: every start shows the ones
    /// not uploaded yet.
    filter: Filter,
}

/// Which clips the Clips page shows, by their uploaded mark (#29).
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
enum Filter {
    #[default]
    NotUploaded,
    Uploaded,
    All,
}

impl Filter {
    const ALL: [Filter; 3] = [Filter::NotUploaded, Filter::Uploaded, Filter::All];

    fn label(self) -> &'static str {
        match self {
            Filter::NotUploaded => "Not uploaded",
            Filter::Uploaded => "Uploaded",
            Filter::All => "All clips",
        }
    }

    fn shows(self, uploaded: bool) -> bool {
        match self {
            Filter::NotUploaded => !uploaded,
            Filter::Uploaded => uploaded,
            Filter::All => true,
        }
    }
}

enum Action {
    /// Play button: play, pause or resume.
    Toggle,
    /// Clicked / let go on the waveform at this point (0..=1).
    Seek(f32),
    /// Copy to the clipboard: mixed, or one track of a clip with separate tracks.
    Copy(library::Tracks),
    Show,
    Delete,
    /// Clicked the title: start editing the name.
    StartRename,
    /// Enter / clicked away while editing.
    FinishRename,
    /// Esc while editing.
    CancelRename,
    /// Open or close the card's ⋯ menu.
    ToggleMenu,
    /// Clicked outside the open menu.
    CloseMenu,
    /// Open the Edit dialog (trim and track volumes).
    Edit,
    /// Turn the clip into a soundboard MP3 in its place (#25), after a
    /// confirm (#30). What the card knows about the clip so far: whether it's
    /// too long for the soundboard, and whether it has separate tracks.
    DiscordReady { too_long: bool, tracks: bool },
    /// Mark or unmark as uploaded to a soundboard (#28).
    SetUploaded(bool),
}

/// How a card's player looks this frame.
#[derive(Clone, Copy, Default)]
struct PlayerView {
    /// This clip is loaded in the player.
    active: bool,
    playing: bool,
    /// Playback position, 0..=1.
    position: f32,
}

impl ClipsPage {
    /// Stops playback (e.g. when the window is hidden).
    pub fn stop(&mut self) {
        // Dropping the playback stops the device.
        self.player = None;
    }

    /// The window is being hidden: stop all playback and close the dialog.
    pub fn on_hide(&mut self) {
        self.stop();
        self.edit = None;
        self.confirm = None;
        self.renaming = None;
        self.menu = None;
    }

    fn is_loaded(&self, clip: &ClipFile) -> bool {
        self.player.as_ref().is_some_and(|p| p.path() == clip.path)
    }

    /// A clip's stored track volumes.
    fn volumes_of(&self, path: &Path) -> Volumes {
        volumes::lookup(&self.volumes, path)
    }

    pub fn show(&mut self, ui: &mut Ui, snap: &Snapshot) {
        self.refresh(ui.ctx(), &snap.cfg.out_dir);
        self.update_playing(ui.ctx());

        let all: Vec<ClipFile> = self.clips.iter().filter(|c| !self.deleted.contains(&c.path)).cloned().collect();
        let today = today();
        let query = self.search.trim().to_lowercase();
        // The filter (#29) first, then search within what it shows.
        let shown_by_filter = |c: &ClipFile| self.filter.shows(uploaded::is_marked(&self.uploaded, &c.path));
        let shown: Vec<&ClipFile> = all.iter().filter(|c| shown_by_filter(c)).collect();
        let hidden = all.len() - shown.len();
        let visible: Vec<ClipFile> = shown.iter().filter(|c| matches_search(c, &query, today)).map(|c| (*c).clone()).collect();
        // Search matches among the hidden clips, to point them out.
        let hidden_matches = if query.is_empty() {
            0
        } else {
            all.iter().filter(|c| !shown_by_filter(c) && matches_search(c, &query, today)).count()
        };

        // Header
        let width = ui.available_width();
        ui.allocate_ui_with_layout(vec2(width, 0.0), Layout::right_to_left(Align::Min), |ui| {
            if button(ui, "Open folder", ButtonKind::Secondary).clicked() {
                send(Command::OpenFolder);
            }
            ui.add_space(16.0);
            ui.with_layout(Layout::top_down(Align::Min), |ui| {
                text(ui, "Clips", 20.0, Weight::Bold, pal().header_primary);
                ui.add_space(4.0);
                let total: u64 = all.iter().map(|c| c.size).sum();
                let dir = snap.cfg.out_dir.display();
                // "12 clips · 3 uploaded hidden · 1.4 MB · <folder>"
                let hidden_note = match (hidden, self.filter) {
                    (0, _) | (_, Filter::All) => String::new(),
                    (n, Filter::NotUploaded) => format!(" · {n} uploaded hidden"),
                    (n, Filter::Uploaded) => format!(" · {n} not uploaded hidden"),
                };
                let summary = match all.len() {
                    0 => format!("Saved in {dir}"),
                    1 => format!("1 clip{hidden_note} · {} · {dir}", megabytes(total)),
                    n => format!("{n} clips{hidden_note} · {} · {dir}", megabytes(total)),
                };
                text(ui, summary, 14.0, Weight::Regular, pal().header_secondary);
            });
        });
        ui.add_space(16.0);

        if !all.is_empty() {
            ui.allocate_ui_with_layout(vec2(width, 40.0), Layout::left_to_right(Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 12.0;
                // In its own area: its text field is placed with `put`, which
                // would otherwise move the row's cursor back over the box.
                let search_w = (ui.available_width() - 182.0).clamp(160.0, 360.0);
                ui.allocate_ui(vec2(search_w, 40.0), |ui| search_box(ui, &mut self.search));
                ui.allocate_ui(vec2(170.0, 40.0), |ui| {
                    let options: Vec<String> = Filter::ALL.iter().map(|f| f.label().to_string()).collect();
                    let selected = Filter::ALL.iter().position(|f| *f == self.filter);
                    if let (_, Some(i)) = dropdown(ui, "clip-filter", &options, selected, true) {
                        self.filter = Filter::ALL[i];
                    }
                });
            });
            ui.add_space(16.0);
        }

        if all.is_empty() {
            empty_state(ui, snap);
        } else if shown.is_empty() {
            // The filter hides every clip.
            let (msg, show) = match self.filter {
                Filter::NotUploaded => ("All your clips are uploaded.", "Show them"),
                _ => ("No clips are marked as uploaded yet.", "Show all clips"),
            };
            if notice(ui, msg, show) {
                self.filter = if self.filter == Filter::NotUploaded { Filter::Uploaded } else { Filter::All };
            }
        } else if visible.is_empty() {
            let mut msg = format!("No clips match \"{}\".", self.search.trim());
            let hidden_kind = if self.filter == Filter::NotUploaded { "uploaded" } else { "hidden" };
            match hidden_matches {
                0 => {}
                1 => msg.push_str(&format!(" 1 {hidden_kind} clip matches.")),
                n => msg.push_str(&format!(" {n} {hidden_kind} clips match.")),
            }
            if hidden_matches == 0 {
                text(ui, msg, 15.0, Weight::Regular, pal().header_secondary);
            } else if notice(ui, &msg, "Show all clips") {
                self.filter = Filter::All;
            }
        } else {
            self.grid(ui, &visible, today);
        }

        self.show_edit(ui.ctx());
        self.show_confirm(ui.ctx());
        self.show_toast(ui);
    }

    /// Rescans the folder now and then, and queues new clips for analysis.
    fn refresh(&mut self, ctx: &egui::Context, dir: &Path) {
        let dir_changed = self.dir.as_deref() != Some(dir);
        if dir_changed || self.last_scan.is_none_or(|t| t.elapsed() >= RESCAN_EVERY) {
            self.dir = Some(dir.to_path_buf());
            self.clips = library::scan(dir);
            self.last_scan = Some(Instant::now());
            self.volumes = volumes::load();
            self.uploaded = uploaded::load();
            let present: HashSet<&PathBuf> = self.clips.iter().map(|c| &c.path).collect();
            self.deleted.retain(|p| present.contains(p));
            self.queued.retain(|p, _| present.contains(p));
            self.analyses.lock().unwrap().retain(|p, _| present.contains(p));
        }
        // Keep polling the folder while the page is open.
        ctx.request_repaint_after(RESCAN_EVERY);

        let worker = self.worker.get_or_insert_with(|| spawn_worker(ctx.clone(), Arc::clone(&self.analyses)));
        for clip in &self.clips {
            if self.queued.get(&clip.path) != Some(&clip.modified) {
                self.queued.insert(clip.path.clone(), clip.modified);
                let _ = worker.send((clip.path.clone(), clip.modified));
            }
        }
    }

    /// At the end of a clip, go back to the start like Discord's player.
    fn update_playing(&mut self, ctx: &egui::Context) {
        let Some(p) = &self.player else { return };
        if p.finished() {
            self.player = None;
        } else if !p.is_paused() {
            // Smooth playhead.
            ctx.request_repaint_after(Duration::from_millis(16));
        }
    }

    fn grid(&mut self, ui: &mut Ui, clips: &[ClipFile], today: i32) {
        let width = ui.available_width();
        let cols = (((width + GAP) / (TILE_MIN_WIDTH + GAP)).floor() as usize).max(1);
        let tile_w = (width - GAP * (cols - 1) as f32) / cols as f32;
        let analyses = self.analyses.lock().unwrap().clone();

        for row in clips.chunks(cols) {
            let (row_rect, _) = ui.allocate_exact_size(vec2(width, TILE_HEIGHT), Sense::hover());
            for (i, clip) in row.iter().enumerate() {
                let x = row_rect.left() + i as f32 * (tile_w + GAP);
                let rect = Rect::from_min_size(pos2(x, row_rect.top()), vec2(tile_w, TILE_HEIGHT));
                let analysis = analyses
                    .get(&clip.path)
                    .filter(|(modified, _)| *modified == clip.modified)
                    .map(|(_, a)| a);
                let view = match &self.player {
                    Some(p) if p.path() == clip.path => {
                        PlayerView { active: true, playing: !p.is_paused(), position: p.position() }
                    }
                    _ => PlayerView::default(),
                };
                let editing = self.renaming.as_mut().filter(|(p, _)| *p == clip.path).map(|(_, text)| text);
                let menu_open = self.menu.as_deref() == Some(clip.path.as_path());
                let custom_mix = !volumes::lookup(&self.volumes, &clip.path).is_default();
                let uploaded = uploaded::is_marked(&self.uploaded, &clip.path);
                let card = Card { view, menu_open, custom_mix, uploaded };
                if let Some(action) = tile(ui, rect, clip, analysis, card, editing, today) {
                    self.act(action, clip);
                }
            }
            ui.add_space(GAP);
        }
    }

    fn act(&mut self, action: Action, clip: &ClipFile) {
        let this_menu_open = self.menu.as_deref() == Some(clip.path.as_path());
        match action {
            Action::ToggleMenu => {
                self.menu = if this_menu_open { None } else { Some(clip.path.clone()) };
                return;
            }
            Action::CloseMenu => {
                if this_menu_open {
                    self.menu = None;
                }
                return;
            }
            // Anything else (including a menu item) closes the menu.
            _ => self.menu = None,
        }
        let result = match action {
            Action::ToggleMenu | Action::CloseMenu => Ok(()),
            Action::Toggle => match &mut self.player {
                Some(p) if p.path() == clip.path => {
                    if p.is_paused() {
                        p.resume()
                    } else {
                        p.pause();
                        Ok(())
                    }
                }
                _ => self.load(clip, 0.0),
            },
            // Seeking a loaded clip keeps it playing or paused; seeking
            // another one starts it there.
            Action::Seek(at) => match &mut self.player {
                Some(p) if p.path() == clip.path => p.seek(at),
                _ => self.load(clip, at),
            },
            Action::Copy(tracks) => {
                send(Command::CopyClip(clip.path.clone(), tracks));
                let what = match tracks {
                    library::Tracks::Both => "Copied",
                    library::Tracks::Discord => "Copied Discord audio only",
                    library::Tracks::Mic => "Copied your mic only",
                };
                self.toast = Some((format!("{what}. Paste it into a Discord chat with Ctrl+V."), Instant::now()));
                Ok(())
            }
            Action::Show => {
                send(Command::ShowClip(clip.path.clone()));
                Ok(())
            }
            Action::Delete => {
                if self.is_loaded(clip) {
                    self.stop();
                }
                self.deleted.insert(clip.path.clone());
                send(Command::DeleteClip(clip.path.clone()));
                // Its mark goes with it (also if it's restored later).
                self.set_uploaded(&clip.path, false);
                self.toast = Some(("Moved to the Recycle Bin.".into(), Instant::now()));
                Ok(())
            }
            Action::SetUploaded(on) => {
                self.set_uploaded(&clip.path, on);
                let msg = if on { "Marked as uploaded." } else { "Marked as not uploaded." };
                self.toast = Some((msg.into(), Instant::now()));
                Ok(())
            }
            Action::StartRename => {
                self.renaming = Some((clip.path.clone(), clip.name.clone().unwrap_or_default()));
                Ok(())
            }
            Action::Edit => {
                self.stop(); // the dialog has its own playback
                let v = self.volumes_of(&clip.path);
                super::edit::EditDialog::open(clip, v).map(|d| self.edit = Some(d))
            }
            // Too long: pick the part to keep in the Edit dialog first (it
            // only changes the clip on Save, so there's nothing to confirm).
            Action::DiscordReady { too_long: true, .. } => self.open_for_soundboard(clip),
            Action::DiscordReady { too_long: false, tracks } => {
                let what = if tracks {
                    "Mixes Discord and your mic into one track, saves it as an MP3 for the soundboard and replaces this clip."
                } else {
                    "Saves this clip as an MP3 for the soundboard and replaces it."
                };
                let dialog = super::confirm::ConfirmDialog::new(
                    "Make Discord-ready?",
                    format!("{what} The original goes to the Recycle Bin."),
                    "Make Discord-ready",
                    ButtonKind::Primary,
                );
                self.confirm = Some((dialog, clip.clone()));
                Ok(())
            }
            Action::CancelRename => {
                self.renaming = None;
                Ok(())
            }
            Action::FinishRename => {
                if let Some((path, name)) = self.renaming.take() {
                    self.rename(&path, &name, clip);
                }
                Ok(())
            }
        };
        if let Err(e) = result {
            crate::log!("clip action failed: {e:#}");
            self.toast = Some((format!("Couldn't open the clip: {e:#}"), Instant::now()));
        }
    }

    /// Renames the file right away (not via the tray thread), so the card,
    /// its waveform and a playing clip can follow it to its new name.
    fn rename(&mut self, path: &Path, name: &str, clip: &ClipFile) {
        if name.trim() == clip.name.as_deref().unwrap_or("") {
            return; // unchanged
        }
        match library::rename(path, name) {
            Ok(new) if new == path => {}
            Ok(new) => {
                crate::log!("renamed {} to {}", path.display(), new.display());
                if let Some(p) = self.player.as_mut().filter(|p| p.path() == path) {
                    p.set_path(new.clone());
                }
                // The volumes and the uploaded mark follow the clip.
                if let Err(e) = volumes::rename(path, &new) {
                    crate::log!("moving the volumes of {} failed: {e:#}", path.display());
                }
                self.volumes = volumes::load();
                self.move_uploaded(path, &new);
                // Keep the waveform instead of reading the file again.
                let mut analyses = self.analyses.lock().unwrap();
                if let Some(a) = analyses.remove(path) {
                    analyses.insert(new.clone(), a);
                }
                if let Some(m) = self.queued.remove(path) {
                    self.queued.insert(new, m);
                }
                self.last_scan = None; // rescan now
            }
            Err(e) => {
                crate::log!("rename failed: {e:#}");
                self.toast = Some((format!("Couldn't rename the clip: {e:#}"), Instant::now()));
            }
        }
    }

    /// Marks or unmarks a clip as uploaded (#28).
    fn set_uploaded(&mut self, path: &Path, on: bool) {
        if let Err(e) = uploaded::set(path, on) {
            crate::log!("saving the uploaded mark of {} failed: {e:#}", path.display());
            self.toast = Some((format!("Couldn't save the mark: {e:#}"), Instant::now()));
        }
        self.uploaded = uploaded::load();
    }

    /// The clip moved to `to` (renamed or replaced): its uploaded mark follows.
    fn move_uploaded(&mut self, from: &Path, to: &Path) {
        if let Err(e) = uploaded::rename(from, to) {
            crate::log!("moving the uploaded mark of {} failed: {e:#}", from.display());
        }
        self.uploaded = uploaded::load();
    }

    /// Opens the Edit dialog to pick the part for the soundboard.
    fn open_for_soundboard(&mut self, clip: &ClipFile) -> anyhow::Result<()> {
        self.stop(); // the dialog has its own playback
        let v = self.volumes_of(&clip.path);
        super::edit::EditDialog::open_for_soundboard(clip, v).map(|d| self.edit = Some(d))
    }

    /// Shows the confirm dialog if it's open, and makes the clip
    /// Discord-ready once confirmed.
    fn show_confirm(&mut self, ctx: &egui::Context) {
        let Some((dialog, _)) = &self.confirm else { return };
        let Some(confirmed) = dialog.show(ctx) else { return };
        let Some((_, clip)) = self.confirm.take() else { return };
        if !confirmed {
            return;
        }
        let result = match self.make_discord_ready(&clip.path) {
            Ok(Some(msg)) => {
                self.toast = Some((msg, Instant::now()));
                Ok(())
            }
            // Turned out too long (it wasn't analysed yet): pick a part first.
            Ok(None) => self.open_for_soundboard(&clip),
            Err(e) => Err(e),
        };
        if let Err(e) = result {
            crate::log!("making {} Discord-ready failed: {e:#}", clip.path.display());
            self.toast = Some((format!("Couldn't make it Discord-ready: {e:#}"), Instant::now()));
        }
    }

    /// Replaces the clip with its Discord-ready version (#25). Returns the
    /// message to show, or `None` when the clip is too long (nothing changed).
    fn make_discord_ready(&mut self, path: &Path) -> anyhow::Result<Option<String>> {
        let v = self.volumes_of(path);
        if self.player.as_ref().is_some_and(|p| p.path() == path) {
            self.stop();
        }
        let new = match library::make_discord_ready(path, 0.0, 1.0, v)? {
            library::DiscordReady::TooLong(_) => return Ok(None),
            library::DiscordReady::Done(new) => new,
        };
        crate::log!("made {} Discord-ready: {}", path.display(), new.display());
        // The mix is in the file now: forget the clip's volumes.
        if let Err(e) = volumes::set(path, Volumes::default()) {
            crate::log!("clearing the volumes of {} failed: {e:#}", path.display());
        }
        // An uploaded mark moves to the new file.
        self.move_uploaded(path, &new);
        self.volumes = volumes::load();
        self.last_scan = None; // show the new file now
        Ok(Some(format!("Discord-ready: {}. The original is in the Recycle Bin.", file_name(&new))))
    }

    /// Shows the Edit dialog if it's open, and carries out what was chosen.
    fn show_edit(&mut self, ctx: &egui::Context) {
        use super::edit::Choice;
        let Some(dialog) = &mut self.edit else { return };
        let Some(choice) = dialog.show(ctx) else { return };
        let path = dialog.clip().path.clone();
        let result = match choice {
            Choice::Cancel => Ok(None),
            Choice::Save { start, end, volumes: v } => {
                if self.player.as_ref().is_some_and(|p| p.path() == path) {
                    self.stop();
                }
                let trimmed = start > 0.0 || end < 1.0;
                let written = if trimmed { library::replace_with_trimmed(&path, start, end) } else { Ok(()) };
                written.and_then(|()| volumes::set(&path, v)).map(|()| {
                    Some(if trimmed { "Saved. The original is in the Recycle Bin." } else { "Saved." }.to_string())
                })
            }
            // The new clip gets these volumes; the original keeps its own.
            Choice::SaveNew { start, end, volumes: v } => library::save_edited_copy(&path, start, end)
                .and_then(|p| volumes::set(&p, v).map(|()| Some(format!("Saved as {}.", file_name(&p))))),
        };
        match result {
            Ok(msg) => {
                self.edit = None;
                if let Some(msg) = msg {
                    crate::log!("edit of {}: {msg}", path.display());
                    self.toast = Some((msg, Instant::now()));
                    self.volumes = volumes::load();
                    self.last_scan = None; // show the new / changed clip now
                }
            }
            Err(e) => {
                crate::log!("edit of {} failed: {e:#}", path.display());
                // Keep the dialog open so the user can try again.
                if let Some(d) = &mut self.edit {
                    d.set_error(format!("{e:#}"));
                }
            }
        }
    }

    /// Loads `clip` into the player (stopping any other) and plays from `at`.
    fn load(&mut self, clip: &ClipFile, at: f32) -> anyhow::Result<()> {
        self.player = None;
        self.player = Some(Playback::start(&clip.path, at, self.volumes_of(&clip.path))?);
        Ok(())
    }

    /// A short message floating at the bottom, like Discord's toasts.
    fn show_toast(&mut self, ui: &mut Ui) {
        let Some((msg, at)) = &self.toast else { return };
        let age = at.elapsed().as_secs_f32();
        if age > 3.0 {
            self.toast = None;
            return;
        }
        ui.ctx().request_repaint_after(Duration::from_millis(100));
        let alpha = ((3.0 - age) / 0.3).clamp(0.0, 1.0);
        let screen = ui.ctx().content_rect();
        egui::Area::new(egui::Id::unique("clips-toast"))
            .order(egui::Order::Foreground)
            .anchor(Align2::CENTER_BOTTOM, vec2(110.0, -24.0))
            .interactable(false)
            .show(ui.ctx(), |ui| {
                ui.set_max_width(screen.width() - 280.0);
                ui.multiply_opacity(alpha);
                Frame::new()
                    .fill(pal().bg_floating)
                    .corner_radius(CornerRadius::same(6))
                    .inner_margin(Margin::symmetric(14, 10))
                    .show(ui, |ui| {
                        text(ui, msg.as_str(), 14.0, Weight::Semibold, pal().text_normal);
                    });
            });
    }
}

fn spawn_worker(ctx: egui::Context, analyses: Analyses) -> Sender<(PathBuf, SystemTime)> {
    let (tx, rx) = mpsc::channel::<(PathBuf, SystemTime)>();
    let _ = std::thread::Builder::new().name("clip-analysis".into()).spawn(move || {
        for (path, modified) in rx {
            let result = library::analyze(&path).map_err(|e| format!("{e:#}"));
            analyses.lock().unwrap().insert(path, (modified, result));
            ctx.request_repaint();
        }
    });
    tx
}

fn empty_state(ui: &mut Ui, snap: &Snapshot) {
    Frame::new()
        .fill(pal().bg_secondary)
        .corner_radius(CornerRadius::same(8))
        .inner_margin(Margin::same(24))
        .show(ui, |ui| {
            // A box of text: keep the reading width of the other pages.
            ui.set_width(ui.available_width().min(super::READING_WIDTH - 48.0));
            text(ui, "No clips yet", 16.0, Weight::Semibold, pal().header_primary);
            ui.add_space(6.0);
            text(
                ui,
                format!(
                    "Press {} during a Discord call to save the last {} seconds. Your clips will show up here.",
                    snap.cfg.hotkey, snap.cfg.seconds
                ),
                14.0,
                Weight::Regular,
                pal().header_secondary,
            );
        });
}

/// A line of text with a button after it; returns whether it was clicked.
fn notice(ui: &mut Ui, msg: &str, button_label: &str) -> bool {
    let width = ui.available_width();
    ui.allocate_ui_with_layout(vec2(width, 32.0), Layout::left_to_right(Align::Center), |ui| {
        ui.spacing_mut().item_spacing.x = 12.0;
        text(ui, msg, 15.0, Weight::Regular, pal().header_secondary);
        button(ui, button_label, ButtonKind::Secondary).clicked()
    })
    .inner
}

/// The state of a card that lives on the page, not in the clip.
struct Card {
    view: PlayerView,
    menu_open: bool,
    /// The clip has stored volumes other than 100%.
    custom_mix: bool,
    /// The clip is marked as uploaded (#28).
    uploaded: bool,
}

/// Draws one card and returns what the user clicked.
fn tile(
    ui: &mut Ui,
    rect: Rect,
    clip: &ClipFile,
    analysis: Option<&Result<Analysis, String>>,
    card_state: Card,
    editing: Option<&mut String>,
    today: i32,
) -> Option<Action> {
    let Card { view, menu_open, custom_mix, uploaded } = card_state;
    let id = ui.make_persistent_id(("clip", &clip.path));
    let card = ui.interact(rect, id, Sense::hover());
    let fill = if card.hovered() { mix(pal().bg_secondary, pal().bg_hover, 0.6) } else { pal().bg_secondary };
    ui.painter().rect_filled(rect, CornerRadius::same(8), fill);
    let inner = rect.shrink(12.0);
    let mut action = None;
    let seconds = match analysis {
        Some(Ok(a)) => a.seconds,
        // Until it's analysed: a rough guess from the file size and format.
        _ => {
            let rate = crate::audio::Format::of_path(&clip.path).map_or(192_000.0, |f| f.stereo_bytes_per_sec());
            clip.size as f32 / rate
        }
    };

    // Player: play/pause button and the waveform to scrub on.
    let panel = Rect::from_min_size(inner.min, vec2(inner.width(), 64.0));
    ui.painter().rect_filled(panel, CornerRadius::same(6), pal().bg_tertiary);

    let center = pos2(panel.left() + 30.0, panel.center().y);
    let play = ui
        .interact(Rect::from_center_size(center, vec2(36.0, 36.0)), id.with("play"), Sense::click())
        .on_hover_cursor(CursorIcon::PointingHand)
        .on_hover_text(if view.playing { "Pause" } else { "Play" });
    if play.clicked() {
        action = Some(Action::Toggle);
    }
    play_button(ui, center, view.playing, play.hovered());

    let bars_area = Rect::from_min_max(pos2(panel.left() + 60.0, panel.top() + 10.0), pos2(panel.right() - 14.0, panel.bottom() - 10.0));
    // The whole panel height reacts, so the waveform is easy to grab.
    let scrub_rect = Rect::from_x_y_ranges(bars_area.x_range(), panel.y_range());
    let scrub = ui.interact(scrub_rect, id.with("scrub"), Sense::click_and_drag()).on_hover_cursor(CursorIcon::PointingHand);
    let frac_at = |x: f32| ((x - bars_area.left()) / bars_area.width()).clamp(0.0, 1.0);
    // While dragging, the playhead follows the mouse; playback jumps there on release.
    let dragging = scrub.dragged() || scrub.is_pointer_button_down_on();
    let drag_at = scrub.interact_pointer_pos().map(|p| frac_at(p.x)).filter(|_| dragging);
    // The frame the mouse is let go: the seek only happens after this card is
    // drawn, so `view.position` is still the old spot. Draw the playhead where
    // it was dropped, or it flashes back for a frame (#12).
    let released_at = scrub
        .interact_pointer_pos()
        .map(|p| frac_at(p.x))
        .filter(|_| scrub.drag_stopped() || scrub.clicked());
    if let Some(at) = released_at {
        action = Some(Action::Seek(at));
    }
    let hover_at = scrub.hover_pos().map(|p| frac_at(p.x)).filter(|_| drag_at.is_none() && released_at.is_none());
    let head = drag_at.or(released_at).or(view.active.then_some(view.position));

    let painter = ui.painter();
    let peaks: Vec<f32> = match analysis {
        Some(Ok(a)) => a.peaks.clone(),
        _ => vec![0.06; library::WAVEFORM_BARS],
    };
    let step = bars_area.width() / peaks.len() as f32;
    let bar_w = (step * 0.6).max(1.5);
    for (i, p) in peaks.iter().enumerate() {
        let x = bars_area.left() + step * (i as f32 + 0.5);
        let h = (p * bars_area.height()).max(3.0);
        let bar = Rect::from_center_size(pos2(x, bars_area.center().y), vec2(bar_w, h));
        let done = head.is_some_and(|f| (i as f32 + 0.5) / peaks.len() as f32 <= f);
        let color = if done {
            pal().header_primary
        } else if analysis.is_some() {
            pal().text_muted
        } else {
            pal().gray_btn
        };
        painter.rect_filled(bar, CornerRadius::same(1), color);
    }

    // Hover: a faint line and the time under the mouse.
    if let Some(f) = hover_at {
        let x = bars_area.left() + f * bars_area.width();
        painter.line_segment([pos2(x, panel.top() + 6.0), pos2(x, panel.bottom() - 6.0)], Stroke::new(1.0, pal().header_secondary.gamma_multiply(0.6)));
        time_bubble(ui, x, panel, &clock_precise(f * seconds));
    }
    // Playhead: a line with a round handle.
    if let Some(f) = head {
        let x = bars_area.left() + f * bars_area.width();
        painter.line_segment([pos2(x, panel.top() + 4.0), pos2(x, panel.bottom() - 4.0)], Stroke::new(2.0, pal().header_primary));
        painter.circle_filled(pos2(x, panel.top() + 6.0), 4.5, pal().header_primary);
        if drag_at.is_some() {
            time_bubble(ui, x, panel, &clock_precise(f * seconds));
        }
    }

    // Title: the clip's name, or when it was saved. Click it to rename.
    let date = clip.saved.map(|s| when(s, today));
    let title = clip.name.clone().or_else(|| date.clone()).unwrap_or_default();
    let title_y = panel.bottom() + 12.0;
    let title_rect = Rect::from_min_max(pos2(inner.left(), title_y), pos2(inner.right() - 96.0, title_y + 20.0));
    match editing {
        Some(text) => {
            if let Some(a) = name_editor(ui, title_rect.expand2(vec2(4.0, 3.0)), text, id.with("rename")) {
                action = Some(a);
            }
        }
        None => {
            let resp = ui
                .interact(title_rect, id.with("title"), Sense::click())
                .on_hover_cursor(CursorIcon::Text)
                .on_hover_text("Click to rename");
            if resp.clicked() {
                action = Some(Action::StartRename);
            }
            // One line, ending in "…" when it doesn't fit (e.g. long names).
            let mut job = egui::text::LayoutJob::simple_singleline(title, font(15.0, Weight::Semibold), pal().header_primary);
            job.wrap = egui::text::TextWrapping {
                max_width: title_rect.width() - 20.0,
                max_rows: 1,
                break_anywhere: true,
                overflow_character: Some('…'),
            };
            let galley = ui.painter().layout_job(job);
            let end = title_rect.left() + galley.size().x;
            ui.painter().galley(title_rect.left_top(), galley, pal().header_primary);
            if resp.hovered() {
                pencil(ui.painter(), pos2(end + 11.0, title_rect.top() + 10.0), pal().header_secondary);
            }
        }
    }
    let painter = ui.painter();
    // "0:12 / 0:30" while this clip is in the player, else just the length.
    let length = match head.filter(|_| view.active || drag_at.is_some() || released_at.is_some()) {
        Some(f) => format!("{} / {}", clock(f * seconds), clock(seconds)),
        None => clock(seconds),
    };
    painter.text(pos2(inner.right(), title_y), Align2::RIGHT_TOP, length, font(14.0, Weight::Semibold), pal().header_secondary);
    // Named clips show their date here instead.
    let mut detail = match (&clip.name, &date) {
        (Some(_), Some(d)) => format!("{d} · {}", megabytes(clip.size)),
        _ => megabytes(clip.size),
    };
    match analysis {
        Some(Err(_)) => detail.push_str(" · can't read this file"),
        Some(Ok(a)) if a.channels == crate::clip::TRACK_CHANNELS => {
            detail.push_str(if custom_mix { " · 2 tracks · custom mix" } else { " · 2 tracks" })
        }
        _ => {}
    }
    if uploaded {
        detail.push_str(" · Uploaded");
    }
    painter.text(pos2(inner.left(), title_y + 23.0), Align2::LEFT_TOP, detail, font(13.0, Weight::Regular), pal().text_muted);

    // Buttons.
    let buttons = Rect::from_min_max(pos2(inner.left(), inner.bottom() - 30.0), inner.max);
    let mut row = ui.new_child(egui::UiBuilder::new().max_rect(buttons).layout(Layout::left_to_right(Align::Center)));
    row.spacing_mut().item_spacing.x = 8.0;
    // The main action is the soundboard (#25); Copy (for chats) is in the ⋯ menu.
    let ready = matches!(analysis, Some(Ok(a)) if library::is_discord_ready(clip, a.seconds, a.channels, custom_mix));
    if ready {
        if button_sized(&mut row, "Show in Explorer", ButtonKind::Secondary, 30.0)
            .on_hover_text("This clip is ready for Discord's soundboard: pick this file when you upload a sound")
            .clicked()
        {
            action = Some(Action::Show);
        }
    } else if button_sized(&mut row, "Make Discord-ready", ButtonKind::Primary, 30.0)
        .on_hover_text("Mixes the tracks into one MP3 of 5 s or less for the soundboard and replaces this clip; the original goes to the Recycle Bin")
        .clicked()
    {
        action = Some(Action::DiscordReady {
            too_long: matches!(analysis, Some(Ok(a)) if library::too_long_for_soundboard(a.seconds)),
            tracks: matches!(analysis, Some(Ok(a)) if a.channels == crate::clip::TRACK_CHANNELS),
        });
    }
    // Everything else is in the ⋯ menu, so the card never runs out of room.
    let more = row
        .with_layout(Layout::right_to_left(Align::Center), |row| more_button(row, menu_open))
        .inner;
    if more.clicked() {
        action = Some(Action::ToggleMenu);
    }
    if menu_open {
        let has_tracks = matches!(analysis, Some(Ok(a)) if a.channels == crate::clip::TRACK_CHANNELS);
        if let Some(a) = card_menu(ui, id, &more, has_tracks, uploaded) {
            action = Some(a);
        }
    }
    action
}

/// The square "⋯" button at the right of a card.
fn more_button(ui: &mut Ui, open: bool) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(30.0, 30.0), Sense::click());
    let resp = resp.on_hover_cursor(CursorIcon::PointingHand).on_hover_text("More");
    let fill = if open || resp.is_pointer_button_down_on() {
        pal().gray_btn_active
    } else if resp.hovered() {
        pal().gray_btn_hover
    } else {
        pal().gray_btn
    };
    ui.painter().rect_filled(rect, CornerRadius::same(4), fill);
    for dx in [-6.0, 0.0, 6.0] {
        ui.painter().circle_filled(rect.center() + vec2(dx, 0.0), 2.0, pal().header_primary);
    }
    resp
}

/// The ⋯ menu, opening below its button like Discord's context menus.
/// Returns the picked item, or `CloseMenu` on a click outside it.
fn card_menu(ui: &Ui, id: egui::Id, button: &egui::Response, has_tracks: bool, uploaded: bool) -> Option<Action> {
    use library::Tracks;
    let mut picked = None;
    // Groups, separated by a line: copying (for chats; clips with separate
    // tracks can copy one), editing, deleting.
    let mut copy: Vec<(&str, fn() -> Action)> = vec![("Copy", || Action::Copy(Tracks::Both))];
    if has_tracks {
        copy.push(("Copy Discord only", || Action::Copy(Tracks::Discord)));
        copy.push(("Copy mic only", || Action::Copy(Tracks::Mic)));
    }
    let mark: (&str, fn() -> Action) = if uploaded {
        ("Mark as not uploaded", || Action::SetUploaded(false))
    } else {
        ("Mark as uploaded", || Action::SetUploaded(true))
    };
    let edit: Vec<(&str, fn() -> Action)> = vec![
        ("Edit…", || Action::Edit),
        ("Rename", || Action::StartRename),
        mark,
        ("Show in Explorer", || Action::Show),
    ];
    let groups = [copy, edit, vec![("Delete", || Action::Delete)]];
    let area = egui::Area::new(id.with("menu"))
        .order(egui::Order::Foreground)
        .pivot(Align2::RIGHT_TOP)
        .fixed_pos(button.rect.right_bottom() + vec2(0.0, 4.0))
        .show(ui.ctx(), |ui| {
            Frame::new()
                .fill(pal().bg_floating)
                .corner_radius(CornerRadius::same(4))
                .inner_margin(Margin::same(6))
                .show(ui, |ui| {
                    ui.set_width(180.0);
                    ui.spacing_mut().item_spacing.y = 2.0;
                    for (g, group) in groups.iter().enumerate() {
                        if g > 0 {
                            let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 9.0), Sense::hover());
                            ui.painter().line_segment(
                                [pos2(r.left() + 4.0, r.center().y), pos2(r.right() - 4.0, r.center().y)],
                                Stroke::new(1.0, pal().divider),
                            );
                        }
                        for (label, make) in group {
                            let danger = *label == "Delete";
                            let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 32.0), Sense::click());
                            // Hover like Discord: blurple, or red for Delete.
                            if resp.hovered() {
                                ui.painter().rect_filled(r, CornerRadius::same(3), if danger { RED_BTN } else { BLURPLE });
                            }
                            let color = match (resp.hovered(), danger) {
                                (true, _) => Color32::WHITE,
                                (false, true) => pal().red,
                                (false, false) => pal().text_normal,
                            };
                            ui.painter().text(pos2(r.left() + 10.0, r.center().y), Align2::LEFT_CENTER, *label, font(14.0, Weight::Semibold), color);
                            if resp.on_hover_cursor(CursorIcon::PointingHand).clicked() {
                                picked = Some(make());
                            }
                        }
                    }
                });
        });
    if picked.is_none() && area.response.clicked_elsewhere() && !button.clicked() && ui.input(|i| i.pointer.any_click()) {
        picked = Some(Action::CloseMenu);
    }
    picked
}

/// Inline name editor over a card's title. Enter or clicking away saves,
/// Esc cancels.
fn name_editor(ui: &mut Ui, rect: Rect, text: &mut String, id: egui::Id) -> Option<Action> {
    ui.painter().rect_filled(rect, CornerRadius::same(4), pal().bg_tertiary);
    ui.painter().rect_stroke(rect, CornerRadius::same(4), Stroke::new(1.0, BLURPLE), egui::StrokeKind::Inside);
    let edit = egui::TextEdit::singleline(text)
        .id(id)
        .frame(Frame::NONE)
        .font(font(15.0, Weight::Semibold))
        .text_color(pal().header_primary)
        .hint_text(egui::RichText::new("Name this clip").color(pal().text_muted))
        .desired_width(rect.width() - 12.0);
    let resp = ui.put(rect.shrink2(vec2(6.0, 2.0)), edit);
    if resp.lost_focus() {
        return Some(if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            Action::CancelRename
        } else {
            Action::FinishRename
        });
    }
    if !resp.has_focus() {
        resp.request_focus(); // just opened
    }
    None
}

/// A small pencil, shown next to a title on hover (it can be renamed).
fn pencil(painter: &Painter, c: egui::Pos2, color: Color32) {
    let s = Stroke::new(1.6, color);
    let (a, b) = (c + vec2(-4.0, 4.0), c + vec2(3.5, -3.5));
    painter.line_segment([a, b], s);
    painter.line_segment([b + vec2(-1.5, -1.5), b + vec2(1.5, 1.5)], s);
    painter.line_segment([a, a + vec2(2.5, 0.0)], s);
    painter.line_segment([a, a + vec2(0.0, -2.5)], s);
}

/// Discord-style search field with a magnifier and a clear button.
/// 40 px tall, like the filter dropdown next to it.
fn search_box(ui: &mut Ui, query: &mut String) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width().min(360.0), 40.0), Sense::hover());
    ui.painter().rect_filled(rect, CornerRadius::same(4), pal().bg_tertiary);
    // Magnifier.
    let c = pos2(rect.right() - 18.0, rect.center().y);
    if query.is_empty() {
        ui.painter().circle_stroke(c + vec2(-1.5, -1.5), 5.0, Stroke::new(1.6, pal().text_muted));
        ui.painter().line_segment([c + vec2(2.0, 2.0), c + vec2(6.0, 6.0)], Stroke::new(1.8, pal().text_muted));
    } else {
        // Clear (x).
        let clear = ui.interact(Rect::from_center_size(c, vec2(22.0, 22.0)), ui.make_persistent_id("search-clear"), Sense::click());
        let col = if clear.hovered() { pal().header_primary } else { pal().text_muted };
        let s = Stroke::new(1.8, col);
        ui.painter().line_segment([c + vec2(-4.5, -4.5), c + vec2(4.5, 4.5)], s);
        ui.painter().line_segment([c + vec2(4.5, -4.5), c + vec2(-4.5, 4.5)], s);
        if clear.on_hover_cursor(CursorIcon::PointingHand).clicked() {
            query.clear();
        }
    }
    let edit = egui::TextEdit::singleline(query)
        .id(ui.make_persistent_id("clip-search"))
        .frame(Frame::NONE)
        .font(font(15.0, Weight::Regular))
        .text_color(pal().text_normal)
        .hint_text(egui::RichText::new("Search clips").color(pal().text_muted))
        .desired_width(rect.width() - 44.0);
    let resp = ui.put(Rect::from_min_max(rect.min + vec2(10.0, 10.0), rect.max - vec2(34.0, 10.0)), edit);
    if resp.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Escape)) {
        query.clear();
    }
}

/// A clip matches when every word of the query appears in its name, its
/// date as shown ("Today, 20:22", "Nov 9") or its file name.
fn matches_search(clip: &ClipFile, query: &str, today: i32) -> bool {
    if query.is_empty() {
        return true;
    }
    let mut haystack = String::new();
    if let Some(n) = &clip.name {
        haystack.push_str(n);
        haystack.push(' ');
    }
    if let Some(s) = clip.saved {
        haystack.push_str(&when(s, today));
        haystack.push(' ');
    }
    if let Some(f) = clip.path.file_name() {
        haystack.push_str(&f.to_string_lossy());
    }
    let haystack = haystack.to_lowercase();
    query.split_whitespace().all(|word| haystack.contains(word))
}

/// The round blurple button: a triangle to play, two bars to pause.
pub(super) fn play_button(ui: &Ui, center: egui::Pos2, playing: bool, hovered: bool) {
    let painter = ui.painter();
    painter.circle_filled(center, 18.0, if hovered { BLURPLE_HOVER } else { BLURPLE });
    if playing {
        for dx in [-4.0, 4.0] {
            let bar = Rect::from_center_size(center + vec2(dx, 0.0), vec2(4.0, 13.0));
            painter.rect_filled(bar, CornerRadius::same(1), Color32::WHITE);
        }
    } else {
        // Nudged right so it looks centred.
        let c = center + vec2(2.0, 0.0);
        painter.add(egui::Shape::convex_polygon(
            vec![c + vec2(-5.0, -7.0), c + vec2(7.0, 0.0), c + vec2(-5.0, 7.0)],
            Color32::WHITE,
            Stroke::NONE,
        ));
    }
}

/// A small time label just above the waveform at `x`. Drawn on the tooltip
/// layer so the scroll area never clips it.
fn time_bubble(ui: &Ui, x: f32, panel: Rect, label: &str) {
    let painter: Painter = ui.ctx().layer_painter(egui::LayerId::new(egui::Order::Tooltip, egui::Id::unique("scrub-time")));
    let galley = painter.layout_no_wrap(label.to_string(), font(12.0, Weight::Bold), pal().header_primary);
    let size = galley.size() + vec2(10.0, 4.0);
    // min/max instead of clamp: clamp panics if the panel is narrower than the label.
    let left = (x - size.x / 2.0).min(panel.right() - size.x - 2.0).max(panel.left() + 2.0);
    let rect = Rect::from_min_size(pos2(left, panel.top() - size.y - 4.0), size);
    painter.rect_filled(rect, CornerRadius::same(4), pal().bg_floating);
    painter.galley(rect.center() - galley.size() / 2.0, galley, pal().header_primary);
}

// ---------------------------------------------------------------------------
// Formatting
// ---------------------------------------------------------------------------

/// 0:12.4, for scrubbing.
pub(super) fn clock_precise(seconds: f32) -> String {
    let tenths = (seconds.max(0.0) * 10.0).round() as u32;
    format!("{}:{:02}.{}", tenths / 600, tenths / 10 % 60, tenths % 10)
}

fn file_name(path: &Path) -> String {
    path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

pub(super) fn megabytes(bytes: u64) -> String {
    format!("{:.1} MB", bytes as f64 / 1_000_000.0)
}

/// 0:30, 2:05
pub(super) fn clock(seconds: f32) -> String {
    let s = seconds.round().max(0.0) as u32;
    format!("{}:{:02}", s / 60, s % 60)
}

/// "Today, 19:31", "Yesterday, 08:02", "Nov 9, 21:31", "Nov 9 2025, 21:31".
pub(super) fn when(s: SavedAt, today: i32) -> String {
    const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    let time = format!("{:02}:{:02}", s.hour, s.minute);
    let day = days_from_civil(s.year as i32, s.month as u32, s.day as u32);
    let month = MONTHS[(s.month.clamp(1, 12) - 1) as usize];
    match today - day {
        0 => format!("Today, {time}"),
        1 => format!("Yesterday, {time}"),
        _ if today - day < 300 => format!("{month} {}, {time}", s.day),
        _ => format!("{month} {} {}, {time}", s.day, s.year),
    }
}

/// Today's local date as a day number (see `days_from_civil`).
pub(super) fn today() -> i32 {
    let t = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() };
    days_from_civil(t.wYear as i32, t.wMonth as u32, t.wDay as u32)
}

/// Days since 1970-01-01 for a proleptic Gregorian date (Howard Hinnant's
/// algorithm), so dates can be compared and subtracted.
fn days_from_civil(y: i32, m: u32, d: u32) -> i32 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as u32;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe as i32 - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filter_shows_by_uploaded_mark() {
        assert_eq!(Filter::default(), Filter::NotUploaded, "starts hiding uploaded clips");
        let shows = |f: Filter| (f.shows(false), f.shows(true));
        assert_eq!(shows(Filter::NotUploaded), (true, false));
        assert_eq!(shows(Filter::Uploaded), (false, true));
        assert_eq!(shows(Filter::All), (true, true));
    }

    #[test]
    fn day_numbers() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(2000, 3, 1) - days_from_civil(2000, 2, 28), 2); // leap year
        assert_eq!(days_from_civil(2027, 1, 1) - days_from_civil(2026, 12, 31), 1);
    }

    #[test]
    fn formats_when() {
        let at = SavedAt { year: 2026, month: 11, day: 9, hour: 21, minute: 31, second: 5 };
        let day = days_from_civil(2026, 11, 9);
        assert_eq!(when(at, day), "Today, 21:31");
        assert_eq!(when(at, day + 1), "Yesterday, 21:31");
        assert_eq!(when(at, day + 3), "Nov 9, 21:31");
        assert_eq!(when(at, day + 400), "Nov 9 2026, 21:31");
        assert_eq!(clock(29.6), "0:30");
        assert_eq!(clock(125.0), "2:05");
    }

    #[test]
    fn searches_names_and_dates() {
        let (saved, name) = crate::library::split_clip_name("discord_clip_20261109_213151 Bob laughs");
        let clip = ClipFile {
            path: PathBuf::from(r"C:\clips\discord_clip_20261109_213151 Bob laughs.wav"),
            size: 1,
            modified: SystemTime::UNIX_EPOCH,
            saved,
            name,
        };
        let today = days_from_civil(2026, 11, 9);
        assert!(matches_search(&clip, "", today));
        assert!(matches_search(&clip, "bob", today));
        assert!(matches_search(&clip, "laughs bob", today)); // any word order
        assert!(matches_search(&clip, "today", today)); // the date as shown
        assert!(matches_search(&clip, "nov 9", today + 5));
        assert!(matches_search(&clip, "20261109", today)); // file name
        assert!(!matches_search(&clip, "alice", today));
    }
}
