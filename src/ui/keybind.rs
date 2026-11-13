//! Discord's keybind recorder: the current combo in a dark box with an
//! "Edit Keybind" button. While recording, the next key combination pressed
//! in the window becomes the new hotkey.

use eframe::egui::{self, pos2, vec2, Align2, Color32, CornerRadius, CursorIcon, Event, Key, Rect, Sense, Stroke, StrokeKind, Ui};

use super::theme::*;
use super::widgets::*;
use super::{send, Command};
use crate::hotkey::{Hotkey, MOD_ALT, MOD_CONTROL, MOD_SHIFT};

#[derive(Default)]
pub struct KeybindRecorder {
    /// Waiting for the user to press a new hotkey.
    recording: bool,
    /// Why the last combination pressed was rejected.
    hint: Option<String>,
}

impl KeybindRecorder {
    pub fn is_recording(&self) -> bool {
        self.recording
    }

    /// While recording, the last rejected combination's explanation.
    pub fn hint(&self) -> Option<&str> {
        self.hint.as_deref().filter(|_| self.recording)
    }

    fn start(&mut self) {
        if !self.recording {
            self.recording = true;
            self.hint = None;
            send(Command::BeginHotkeyCapture);
        }
    }

    /// Stops recording; `result` is the new hotkey, or `None` to cancel.
    pub fn stop(&mut self, result: Option<Hotkey>) {
        if self.recording {
            self.recording = false;
            self.hint = None;
            send(Command::EndHotkeyCapture(result));
        }
    }

    /// Call every frame (before drawing): while recording, the next key
    /// combination becomes the hotkey. Esc or leaving the window cancels.
    pub fn poll(&mut self, ui: &Ui) {
        if !self.recording {
            return;
        }
        let ctrl = egui::Modifiers::CTRL;
        let (events, focused) = ui.input(|i| (i.events.clone(), i.viewport().focused));
        if focused == Some(false) {
            self.stop(None);
            return;
        }
        for event in events {
            // Ctrl+C/X/V arrive as clipboard events, not key presses.
            let (key, m) = match event {
                Event::Key { key, pressed: true, modifiers, .. } => (key, modifiers),
                Event::Copy => (Key::C, ctrl),
                Event::Cut => (Key::X, ctrl),
                Event::Paste(_) => (Key::V, ctrl),
                _ => continue,
            };
            if key == Key::Escape && !m.any() {
                self.stop(None);
                return;
            }
            let Some(vk) = key_to_vk(key) else { continue };
            let mut mods = 0;
            if m.ctrl {
                mods |= MOD_CONTROL;
            }
            if m.alt {
                mods |= MOD_ALT;
            }
            if m.shift {
                mods |= MOD_SHIFT;
            }
            let hk = Hotkey { mods, vk };
            match hk.check_sensible() {
                Ok(()) => {
                    self.stop(Some(hk));
                    return;
                }
                Err(e) => self.hint = Some(e),
            }
        }
    }

    /// The box showing `current`, with the Edit Keybind / Stop Recording button.
    pub fn show(&mut self, ui: &mut Ui, current: Hotkey) {
        let width = ui.available_width().min(420.0);
        let (rect, resp) = ui.allocate_exact_size(vec2(width, 44.0), Sense::click());
        let painter = ui.painter();
        painter.rect_filled(rect, CornerRadius::same(4), pal().bg_tertiary);
        if self.recording {
            painter.rect_stroke(rect, CornerRadius::same(4), Stroke::new(1.0, pal().red), StrokeKind::Inside);
            painter.rect_stroke(rect.expand(2.0), CornerRadius::same(6), Stroke::new(2.0, pal().red.gamma_multiply(0.3)), StrokeKind::Inside);
        } else if resp.hovered() {
            painter.rect_stroke(rect, CornerRadius::same(4), Stroke::new(1.0, pal().bg_floating), StrokeKind::Inside);
        }
        let (label, color) = if self.recording {
            ("Press a key combination…".to_string(), pal().red)
        } else {
            (current.to_string(), pal().header_primary)
        };
        painter.text(pos2(rect.left() + 12.0, rect.center().y), Align2::LEFT_CENTER, label, font(16.0, Weight::Semibold), color);

        // Button inside the box, on the right.
        let (btn_label, kind) = if self.recording {
            ("Stop Recording", ButtonKind::Danger)
        } else {
            ("Edit Keybind", ButtonKind::Secondary)
        };
        let btn_galley = painter.layout_no_wrap(btn_label.to_string(), font(14.0, Weight::Semibold), Color32::WHITE);
        let btn_w = (btn_galley.size().x + 32.0).max(60.0); // same as button_sized
        let btn_rect = Rect::from_min_max(pos2(rect.right() - 6.0 - btn_w, rect.top() + 6.0), pos2(rect.right() - 6.0, rect.bottom() - 6.0));
        let mut btn_ui = ui.new_child(egui::UiBuilder::new().max_rect(btn_rect));
        let btn = button_sized(&mut btn_ui, btn_label, kind, btn_rect.height());

        if btn.clicked() || resp.clicked() {
            if self.recording {
                self.stop(None);
            } else {
                self.start();
            }
        }
        resp.on_hover_cursor(CursorIcon::PointingHand);
    }
}

/// Maps an egui key to a Windows virtual-key code that [`Hotkey`] can use.
fn key_to_vk(key: Key) -> Option<u32> {
    let name = key.name();
    if name.len() == 1 {
        let c = name.as_bytes()[0];
        if c.is_ascii_uppercase() || c.is_ascii_digit() {
            return Some(c as u32);
        }
    }
    if let Some(n) = name.strip_prefix('F').and_then(|n| n.parse::<u32>().ok()) {
        if (1..=24).contains(&n) {
            return Some(0x70 + n - 1);
        }
    }
    use Key::*;
    Some(match key {
        Space => 0x20,
        PageUp => 0x21,
        PageDown => 0x22,
        End => 0x23,
        Home => 0x24,
        ArrowLeft => 0x25,
        ArrowUp => 0x26,
        ArrowRight => 0x27,
        ArrowDown => 0x28,
        PrintScreen => 0x2C,
        Insert => 0x2D,
        Delete => 0x2E,
        Pause => 0x13,
        ScrollLock => 0x91,
        Backtick => 0xC0,
        Minus => 0xBD,
        Equals | Plus => 0xBB,
        OpenBracket => 0xDB,
        CloseBracket => 0xDD,
        Backslash => 0xDC,
        Semicolon => 0xBA,
        Quote => 0xDE,
        Comma => 0xBC,
        Period => 0xBE,
        Slash => 0xBF,
        MediaPlayPause => 0xB3,
        MediaTrackNext => 0xB0,
        MediaTrackPrevious => 0xB1,
        MediaStop => 0xB2,
        _ => return None,
    })
}

