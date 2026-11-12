//! The Updates page: current version, update status, Update now.

use eframe::egui::{self, pos2, Color32, CornerRadius, Frame, Margin, Rect, Sense, TextureHandle, Ui, Vec2};

use super::theme::*;
use super::widgets::*;
use super::{send, set, Command, Snapshot};
use crate::update::{State, CURRENT};

/// `icon`: the app icon shown on the card.
pub fn show(ui: &mut Ui, snap: &Snapshot, icon: &TextureHandle) {
    text(ui, "Updates", 20.0, Weight::Bold, HEADER_PRIMARY);
    ui.add_space(20.0);

    // Card: app, version and what's going on.
    Frame::new()
        .fill(BG_SECONDARY)
        .corner_radius(CornerRadius::same(8))
        .inner_margin(Margin::same(16))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                let (rect, _) = ui.allocate_exact_size(Vec2::splat(48.0), Sense::hover());
                let uv = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
                ui.painter().image(icon.id(), rect, uv, Color32::WHITE);
                ui.add_space(8.0);
                ui.vertical(|ui| {
                    ui.add_space(4.0);
                    text(ui, "Discord Clip", 16.0, Weight::Semibold, HEADER_PRIMARY);
                    ui.add_space(2.0);
                    let kind = if snap.installed { "installed" } else { "portable" };
                    text(ui, format!("Version {CURRENT} · {kind}"), 14.0, Weight::Regular, HEADER_SECONDARY);
                });
            });
            ui.add_space(16.0);

            let checked = snap
                .update
                .last_checked
                .map(|t| format!("Last checked {}.", ago(t)))
                .unwrap_or_default();
            match &snap.update.state {
                State::Idle => {
                    status_line(ui, None, "Not checked for updates yet.", TEXT_NORMAL);
                    ui.add_space(12.0);
                    if button(ui, "Check for updates", ButtonKind::Primary).clicked() {
                        send(Command::CheckUpdates);
                    }
                }
                State::Checking => {
                    ui.horizontal(|ui| {
                        ui.add(egui::Spinner::new().size(16.0).color(BLURPLE));
                        ui.add_space(4.0);
                        text(ui, "Checking for updates…", 15.0, Weight::Semibold, TEXT_NORMAL);
                    });
                }
                State::UpToDate => {
                    status_line(ui, Some(GREEN), "You're up to date.", TEXT_NORMAL);
                    ui.add_space(4.0);
                    text(ui, checked, 14.0, Weight::Regular, TEXT_MUTED);
                    ui.add_space(12.0);
                    if button(ui, "Check again", ButtonKind::Secondary).clicked() {
                        send(Command::CheckUpdates);
                    }
                }
                State::Available(m) => {
                    status_line(ui, Some(BLURPLE), &format!("Version {} is available.", m.version), HEADER_PRIMARY);
                    ui.add_space(4.0);
                    let released = m.published.get(..10).map(|d| format!("Released {d}. ")).unwrap_or_default();
                    let how = if snap.installed {
                        "Discord Clip closes for a few seconds while it installs, then starts again."
                    } else {
                        "This exe is replaced with the new version and restarted."
                    };
                    text(ui, format!("{released}{how}"), 14.0, Weight::Regular, HEADER_SECONDARY);
                    ui.add_space(12.0);
                    ui.horizontal(|ui| {
                        if button(ui, "Update now", ButtonKind::Success).clicked() {
                            send(Command::InstallUpdate);
                        }
                        if !m.notes_url.is_empty() && button(ui, "Release notes", ButtonKind::Secondary).clicked() {
                            send(Command::OpenUrl(m.notes_url.clone()));
                        }
                    });
                }
                State::Downloading { manifest, done, total } => {
                    text(ui, format!("Downloading version {}…", manifest.version), 15.0, Weight::Semibold, TEXT_NORMAL);
                    ui.add_space(10.0);
                    let frac = if *total > 0 { *done as f32 / *total as f32 } else { 0.0 };
                    progress_bar(ui, frac);
                    ui.add_space(6.0);
                    text(
                        ui,
                        format!("{:.1} of {:.1} MB", *done as f32 / 1e6, *total as f32 / 1e6),
                        14.0,
                        Weight::Regular,
                        TEXT_MUTED,
                    );
                }
                State::Ready { manifest, .. } => {
                    ui.horizontal(|ui| {
                        ui.add(egui::Spinner::new().size(16.0).color(GREEN));
                        ui.add_space(4.0);
                        text(ui, format!("Installing version {}…", manifest.version), 15.0, Weight::Semibold, TEXT_NORMAL);
                    });
                }
                State::Failed { error, manifest } => {
                    let what = if manifest.is_some() { "Couldn't install the update." } else { "Couldn't check for updates." };
                    status_line(ui, Some(RED), what, HEADER_PRIMARY);
                    ui.add_space(4.0);
                    text(ui, error, 14.0, Weight::Regular, HEADER_SECONDARY);
                    ui.add_space(12.0);
                    ui.horizontal(|ui| {
                        if manifest.is_some() {
                            if button(ui, "Try again", ButtonKind::Success).clicked() {
                                send(Command::InstallUpdate);
                            }
                        } else if button(ui, "Try again", ButtonKind::Primary).clicked() {
                            send(Command::CheckUpdates);
                        }
                        if button(ui, "Open downloads page", ButtonKind::Secondary).clicked() {
                            send(Command::OpenUrl(crate::update::RELEASES_URL.to_string()));
                        }
                    });
                }
            }
        });

    divider(ui);

    section_label(ui, "Automatic updates");
    if row(
        ui,
        "Check for updates automatically",
        "Looks for a new version when Discord Clip starts and every few hours, and lets you know. \
         Nothing is installed until you click Update now.",
        |ui| toggle(ui, snap.cfg.check_updates),
    )
    .clicked()
    {
        set(Command::SetCheckUpdates(!snap.cfg.check_updates));
    }
}
