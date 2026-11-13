//! Follows the Discord theme (#37): Windows tells us when Discord writes its
//! settings or Local Storage, then the theme is read on a background thread
//! (only while the window is shown) and the palette is switched.
//!
//! Until the first read, and whenever Discord's files can't be read, the
//! last theme read successfully is used (Dark if there never was one).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};

use eframe::egui;

use super::theme::{apply, Palette};
use crate::config::ThemeMode;
use crate::discord_theme::{self, Base, DiscordTheme};

/// Discord wrote something since the last read (or we haven't read yet).
static CHANGED: AtomicBool = AtomicBool::new(true);

pub struct Follower {
    /// The newest theme read, or remembered from an earlier run.
    discord: Option<DiscordTheme>,
    /// Whether the last read worked (`None` = not read yet).
    last_read_ok: Option<bool>,
    reading: Option<Receiver<Option<DiscordTheme>>>,
    applied: Option<Palette>,
}

impl Follower {
    pub fn start() -> Follower {
        discord_theme::watch(|| {
            CHANGED.store(true, Ordering::SeqCst);
            if let Some(ctx) = super::shared().ctx.lock().unwrap().as_ref() {
                ctx.request_repaint();
            }
        });
        Follower { discord: discord_theme::remembered(), last_read_ok: None, reading: None, applied: None }
    }

    /// Call every frame: starts a read if Discord changed something, and
    /// switches the colours when the theme (or the setting) changed.
    pub fn update(&mut self, ctx: &egui::Context, mode: ThemeMode) {
        if mode == ThemeMode::Discord && self.reading.is_none() && window_visible() && CHANGED.swap(false, Ordering::SeqCst) {
            let (tx, rx) = mpsc::channel();
            let ctx = ctx.clone();
            let spawned = std::thread::Builder::new().name("read-discord-theme".into()).spawn(move || {
                let _ = tx.send(discord_theme::read());
                ctx.request_repaint();
            });
            if spawned.is_ok() {
                self.reading = Some(rx);
            }
        }
        if let Some(rx) = &self.reading {
            match rx.try_recv() {
                Ok(theme) => {
                    self.reading = None;
                    self.last_read_ok = Some(theme.is_some());
                    if let Some(theme) = theme {
                        if self.discord.as_ref() != Some(&theme) {
                            discord_theme::remember(&theme);
                            self.discord = Some(theme);
                        }
                    }
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => self.reading = None,
            }
        }

        let palette = match mode {
            ThemeMode::Dark => Palette::DARK,
            ThemeMode::Light => Palette::LIGHT,
            ThemeMode::Discord => self.discord.as_ref().map_or(Palette::DARK, Palette::from_discord),
        };
        if self.applied != Some(palette) {
            apply(ctx, palette);
            self.applied = Some(palette);
        }
    }

    /// For the Theme setting: what was read from Discord.
    pub fn describe(&self) -> String {
        let Some(theme) = &self.discord else {
            return "Discord's theme couldn't be read, so Dark is used.".into();
        };
        let name = match theme.base {
            Base::Light => "Light",
            Base::Ash => "Ash",
            Base::Dark => "Dark",
            Base::Onyx => "Onyx",
        };
        let extra = if theme.tint.is_some() { " with a Nitro theme" } else { "" };
        if self.last_read_ok == Some(false) {
            format!("Discord's theme couldn't be read just now, so the last one is used: {name}{extra}.")
        } else {
            format!("Discord uses {name}{extra}.")
        }
    }
}

fn window_visible() -> bool {
    use windows::Win32::UI::WindowsAndMessaging::IsWindowVisible;
    super::window_hwnd().is_some_and(|h| unsafe { IsWindowVisible(h) }.as_bool())
}
