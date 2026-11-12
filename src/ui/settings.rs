//! The Settings page.

use eframe::egui::{self, pos2, vec2, Align, CornerRadius, Frame, Layout, Margin, Sense, Ui};

use super::keybind::KeybindRecorder;
use super::theme::*;
use super::widgets::*;
use super::{send, set, Command, Snapshot};
use crate::config::Config;

#[derive(Default)]
pub struct SettingsPage {
    pub keybind: KeybindRecorder,
    /// Slider values while dragging (committed on release).
    seconds_drag: Option<f32>,
    gain_drag: Option<f32>,
}

impl SettingsPage {
    pub fn show(&mut self, ui: &mut Ui, snap: &Snapshot) {
        let cfg = &snap.cfg;
        text(ui, "Settings", 20.0, Weight::Bold, HEADER_PRIMARY);
        ui.add_space(20.0);

        // Hotkey
        section_label(ui, "Hotkey");
        self.keybind.show(ui, cfg.hotkey);
        ui.add_space(8.0);
        let hint = if let Some(h) = self.keybind.hint().map(str::to_string) {
            (h, YELLOW)
        } else if let Some(e) = snap.hotkey_error.clone().filter(|_| !self.keybind.is_recording()) {
            (e, RED)
        } else if !snap.hotkey_registered && !self.keybind.is_recording() {
            (format!("{} is already used by another app. Pick a different combination.", cfg.hotkey), RED)
        } else {
            (
                format!(
                    "Saves the last {}s of Discord audio. Use Ctrl, Alt or Shift with a key, or an F-key on its own.",
                    cfg.seconds
                ),
                HEADER_SECONDARY,
            )
        };
        text(ui, hint.0, 14.0, Weight::Regular, hint.1);

        divider(ui);

        // Recording
        section_label(ui, "Recording");
        let mut secs = self.seconds_drag.unwrap_or(cfg.seconds as f32);
        row(ui, "Clip length", "How much audio each clip contains. Applies after a restart.", |ui| {
            text(ui, format!("{} s", secs.round() as u32), 16.0, Weight::Semibold, HEADER_PRIMARY)
        });
        ui.add_space(8.0);
        let r = slider(ui, &mut secs, 5.0..=300.0, 5.0, true);
        if r.is_pointer_button_down_on() {
            self.seconds_drag = Some(secs);
        }
        if r.drag_stopped() || r.clicked() {
            self.seconds_drag = None;
            set(Command::SetSeconds(secs.round() as u32));
        }
        ui.add_space(6.0);
        let bytes = secs.round() as u64 * (crate::capture::SAMPLE_RATE as u64) * (crate::capture::CHANNELS as u64) * 2;
        let mb = bytes as f32 / 1_000_000.0;
        if bytes > 10 * 1024 * 1024 {
            text(ui, format!("≈ {mb:.1} MB per clip: over Discord's 10 MB upload limit without Nitro."), 14.0, Weight::Regular, YELLOW);
        } else {
            text(ui, format!("≈ {mb:.1} MB per clip."), 14.0, Weight::Regular, TEXT_MUTED);
        }

        ui.add_space(24.0);
        let mic_desc = if snap.mic == "unavailable" {
            "Mix your own voice into clips. No microphone was found when the app started."
        } else {
            "Mix your own voice (your default communications microphone) into clips. Applies after a restart."
        };
        if row(ui, "Record my microphone", mic_desc, |ui| toggle(ui, cfg.mic)).clicked() {
            set(Command::SetMic(!cfg.mic));
        }

        ui.add_space(24.0);
        let mut gain = self.gain_drag.unwrap_or(cfg.mic_gain.min(4.0));
        row(ui, "Microphone volume", "How loud your voice is in clips, relative to everyone else.", |ui| {
            text(ui, format!("{}%", (gain * 100.0).round() as u32), 16.0, Weight::Semibold, HEADER_PRIMARY)
        });
        ui.add_space(8.0);
        let r = slider(ui, &mut gain, 0.0..=4.0, 0.05, cfg.mic);
        if r.is_pointer_button_down_on() {
            self.gain_drag = Some(gain);
        }
        if r.drag_stopped() || r.clicked() {
            self.gain_drag = None;
            set(Command::SetMicGain(gain));
        }

        divider(ui);

        // Saving
        section_label(ui, "Clips folder");
        folder_field(ui, cfg);
        ui.add_space(24.0);
        if row(
            ui,
            "Copy clips to the clipboard",
            "Press Ctrl+V in a Discord chat right after saving to send the clip.",
            |ui| toggle(ui, cfg.clipboard),
        )
        .clicked()
        {
            set(Command::SetClipboard(!cfg.clipboard));
        }
        ui.add_space(24.0);
        if row(
            ui,
            "Show notifications",
            "Pop up a notification each time a clip is saved. Problems are always shown.",
            |ui| toggle(ui, cfg.notifications),
        )
        .clicked()
        {
            set(Command::SetNotifications(!cfg.notifications));
        }

        divider(ui);

        // Startup
        section_label(ui, "Startup");
        use crate::autostart::State;
        let on = snap.autostart == State::On;
        let other;
        let desc = match &snap.autostart {
            State::OtherCopy(path) => {
                other = format!(
                    "Windows currently starts a different copy: {}. Turn this on to start this one instead.",
                    path.display()
                );
                other.as_str()
            }
            State::DisabledByWindows => {
                "Switched off in Task Manager's Startup apps. Turn it on here to start Discord Clip again."
            }
            _ => "Start Discord Clip in the tray when you sign in, so it's always ready to save a clip.",
        };
        if row(ui, "Start with Windows", desc, |ui| toggle(ui, on)).clicked() {
            set(Command::SetAutostart(!on));
        }

        divider(ui);

        // Advanced
        section_label(ui, "Advanced");
        text(
            ui,
            "Settings are stored in config.ini. If you edit it by hand, reload it to apply your changes.",
            14.0,
            Weight::Regular,
            HEADER_SECONDARY,
        );
        ui.add_space(12.0);
        ui.horizontal(|ui| {
            if button(ui, "Open settings file", ButtonKind::Secondary).clicked() {
                send(Command::OpenConfigFile);
            }
            if button(ui, "Reload settings file", ButtonKind::Secondary).clicked() {
                send(Command::Restart);
            }
            if button(ui, "Open log", ButtonKind::Secondary).clicked() {
                send(Command::OpenLog);
            }
        });
    }
}

fn folder_field(ui: &mut Ui, cfg: &Config) {
    let width = ui.available_width();
    ui.allocate_ui_with_layout(vec2(width, 40.0), Layout::right_to_left(Align::Center), |ui| {
        if button_sized(ui, "Open", ButtonKind::Secondary, 40.0).clicked() {
            send(Command::OpenFolder);
        }
        if button_sized(ui, "Change", ButtonKind::Primary, 40.0).clicked() {
            send(Command::PickFolder);
        }
        let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 40.0), Sense::hover());
        let path = cfg.out_dir.display().to_string();
        let painter = ui.painter();
        painter.rect_filled(rect, CornerRadius::same(4), BG_TERTIARY);
        let inner = rect.shrink2(vec2(10.0, 0.0));
        let galley = painter.layout_no_wrap(path.clone(), font(15.0, Weight::Regular), TEXT_NORMAL);
        // Too long: show the end of the path, which is the useful part.
        let x = if galley.size().x > inner.width() { inner.right() - galley.size().x } else { inner.left() };
        painter
            .with_clip_rect(inner)
            .galley(pos2(x, rect.center().y - galley.size().y / 2.0), galley, TEXT_NORMAL);
        resp.on_hover_text(path);
    });
}

/// Floating bar like Discord's "You have unsaved changes".
pub fn restart_bar(ui: &mut Ui) {
    egui::Panel::bottom("restart")
        .resizable(false)
        .show_separator_line(false)
        .frame(Frame::new().inner_margin(Margin { left: 40, right: 40, top: 0, bottom: 20 }))
        .show(ui, |ui| {
            ui.set_max_width(ui.available_width().min(700.0));
            Frame::new()
                .fill(BG_FLOATING)
                .corner_radius(CornerRadius::same(6))
                .inner_margin(Margin { left: 16, right: 10, top: 10, bottom: 10 })
                .show(ui, |ui| {
                    let width = ui.available_width();
                    ui.allocate_ui_with_layout(vec2(width, 32.0), Layout::right_to_left(Align::Center), |ui| {
                        if button(ui, "Restart now", ButtonKind::Success).clicked() {
                            send(Command::Restart);
                        }
                        ui.add_space(8.0);
                        ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                            text(
                                ui,
                                "Restart to apply your clip length and microphone changes.",
                                15.0,
                                Weight::Semibold,
                                TEXT_NORMAL,
                            );
                        });
                    });
                });
        });
}
