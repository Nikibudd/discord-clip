//! An emoji picker like Discord's (#76): a search box, every emoji grouped
//! by category, and the hovered emoji's name at the bottom. Used for a
//! soundboard sound's emoji, so only real emoji can be picked.

use std::sync::OnceLock;

use eframe::egui::{self, pos2, vec2, Align2, Color32, CornerRadius, CursorIcon, Frame, Margin, Sense, Stroke, Ui};

use super::theme::*;
use super::widgets::*;

const COLS: usize = 9;
const CELL: f32 = 36.0;
const LIST_HEIGHT: f32 = 288.0;

/// The picker's state while it's open.
#[derive(Default)]
pub struct EmojiPicker {
    pub open: bool,
    query: String,
}

/// Emoji the font draws as one picture: a single character (plus an
/// optional variation selector) from Unicode 15 or older (Segoe UI Emoji on
/// Windows 11). Sequences (families, flags, keycaps) would show as several
/// pictures, since the text renderer doesn't join them.
fn drawable(e: &emojis::Emoji) -> bool {
    e.as_str().chars().filter(|c| *c != '\u{FE0F}').count() == 1 && e.unicode_version() <= emojis::UnicodeVersion::new(15, 0)
}

fn group_name(g: emojis::Group) -> &'static str {
    use emojis::Group::*;
    match g {
        SmileysAndEmotion => "Smileys & Emotion",
        PeopleAndBody => "People",
        AnimalsAndNature => "Nature",
        FoodAndDrink => "Food & Drink",
        TravelAndPlaces => "Travel & Places",
        Activities => "Activities",
        Objects => "Objects",
        Symbols => "Symbols",
        Flags => "Flags",
    }
}

/// All pickable emoji, by group.
fn all() -> &'static [(&'static str, Vec<&'static emojis::Emoji>)] {
    static ALL: OnceLock<Vec<(&'static str, Vec<&'static emojis::Emoji>)>> = OnceLock::new();
    ALL.get_or_init(|| {
        emojis::Group::iter()
            .map(|g| (group_name(g), g.emojis().filter(|e| drawable(e)).collect::<Vec<_>>()))
            .filter(|(_, list)| !list.is_empty())
            .collect()
    })
}

fn matches(e: &emojis::Emoji, q: &str) -> bool {
    e.name().contains(q) || e.shortcodes().any(|s| s.contains(q))
}

/// A row of the list: a group heading or up to `COLS` emoji.
enum Row {
    Heading(&'static str),
    Emoji(Vec<&'static emojis::Emoji>),
}

fn rows(query: &str) -> Vec<Row> {
    let q = query.trim().trim_matches(':').to_lowercase();
    let mut out = Vec::new();
    if q.is_empty() {
        for (name, list) in all() {
            out.push(Row::Heading(name));
            out.extend(list.chunks(COLS).map(|c| Row::Emoji(c.to_vec())));
        }
    } else {
        let found: Vec<_> = all().iter().flat_map(|(_, l)| l.iter().copied()).filter(|e| matches(e, &q)).collect();
        out.extend(found.chunks(COLS).map(|c| Row::Emoji(c.to_vec())));
    }
    out
}

impl EmojiPicker {
    /// A square button showing the picked emoji (a dimmed smiley for none)
    /// that opens the picker below it. `value` is an emoji or empty.
    pub fn show(&mut self, ui: &mut Ui, value: &mut String, enabled: bool) {
        let sense = if enabled { Sense::click() } else { Sense::hover() };
        let (rect, resp) = ui.allocate_exact_size(vec2(56.0, 40.0), sense);
        let fill = if self.open || (resp.hovered() && enabled) { pal().bg_hover } else { pal().bg_tertiary };
        ui.painter().rect_filled(rect, CornerRadius::same(4), fill);
        if self.open {
            ui.painter().rect_stroke(rect, CornerRadius::same(4), Stroke::new(1.0, BLURPLE), egui::StrokeKind::Inside);
        }
        let (glyph, color) = if value.is_empty() { ("🙂", Color32::from_white_alpha(70)) } else { (value.as_str(), Color32::WHITE) };
        ui.painter().text(rect.center(), Align2::CENTER_CENTER, glyph, font(22.0, Weight::Regular), color);
        let resp = if enabled { resp.on_hover_cursor(CursorIcon::PointingHand) } else { resp };
        if resp.clicked() {
            self.open = !self.open;
            self.query.clear();
        }
        if !enabled {
            self.open = false;
        }
        if !self.open {
            return;
        }
        if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.open = false;
            return;
        }

        let width = CELL * COLS as f32;
        let mut hovered: Option<&'static emojis::Emoji> = None;
        let mut picked: Option<String> = None;
        let area = egui::Area::new(egui::Id::unique("emoji-picker"))
            .order(egui::Order::Tooltip)
            .fixed_pos(rect.left_bottom() + vec2(0.0, 4.0))
            .show(ui.ctx(), |ui| {
                Frame::new()
                    .fill(pal().bg_floating)
                    .corner_radius(CornerRadius::same(8))
                    .inner_margin(Margin::same(8))
                    .show(ui, |ui| {
                        ui.set_width(width);
                        ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
                        // Search, focused as it opens.
                        ui.horizontal(|ui| {
                            let r = text_field(ui, &mut self.query, "Find the perfect emoji", false, width - 96.0);
                            if !r.has_focus() && !ui.ctx().memory(|m| m.focused().is_some()) {
                                r.request_focus();
                            }
                            ui.add_space(8.0);
                            let (r, none) = ui.allocate_exact_size(vec2(88.0, 40.0), Sense::click());
                            if none.hovered() {
                                ui.painter().rect_filled(r, CornerRadius::same(4), pal().bg_hover);
                            }
                            ui.painter().text(
                                r.center(),
                                Align2::CENTER_CENTER,
                                "No emoji",
                                font(14.0, Weight::Semibold),
                                if value.is_empty() { BLURPLE } else { pal().text_normal },
                            );
                            if none.on_hover_cursor(CursorIcon::PointingHand).clicked() {
                                picked = Some(String::new());
                            }
                        });
                        ui.add_space(8.0);

                        let rows = rows(&self.query);
                        if rows.is_empty() {
                            let (r, _) = ui.allocate_exact_size(vec2(width, LIST_HEIGHT), Sense::hover());
                            ui.painter().text(
                                r.center(),
                                Align2::CENTER_CENTER,
                                "No emoji match that.",
                                font(14.0, Weight::Regular),
                                pal().text_muted,
                            );
                        } else {
                            egui::ScrollArea::vertical()
                                .id_salt(("emoji-list", self.query.is_empty()))
                                .max_height(LIST_HEIGHT)
                                .min_scrolled_height(LIST_HEIGHT)
                                .auto_shrink([false, false])
                                .show_rows(ui, CELL, rows.len(), |ui, range| {
                                    for row in &rows[range] {
                                        match row {
                                            Row::Heading(name) => {
                                                let (r, _) = ui.allocate_exact_size(vec2(width, CELL), Sense::hover());
                                                ui.painter().text(
                                                    pos2(r.left() + 4.0, r.center().y + 4.0),
                                                    Align2::LEFT_CENTER,
                                                    name.to_uppercase(),
                                                    font(12.0, Weight::Bold),
                                                    pal().text_muted,
                                                );
                                            }
                                            Row::Emoji(list) => {
                                                ui.horizontal(|ui| {
                                                    for e in list {
                                                        let (r, cell) = ui.allocate_exact_size(vec2(CELL, CELL), Sense::click());
                                                        let selected = value.as_str() == e.as_str();
                                                        if selected || cell.hovered() {
                                                            let fill = if selected {
                                                                mix(pal().bg_floating, BLURPLE, 0.35)
                                                            } else {
                                                                pal().bg_hover
                                                            };
                                                            ui.painter().rect_filled(r, CornerRadius::same(4), fill);
                                                        }
                                                        ui.painter().text(
                                                            r.center(),
                                                            Align2::CENTER_CENTER,
                                                            e.as_str(),
                                                            font(22.0, Weight::Regular),
                                                            Color32::WHITE,
                                                        );
                                                        if cell.hovered() {
                                                            hovered = Some(e);
                                                        }
                                                        if cell.on_hover_cursor(CursorIcon::PointingHand).clicked() {
                                                            picked = Some(e.as_str().to_string());
                                                        }
                                                    }
                                                });
                                            }
                                        }
                                    }
                                });
                        }

                        // Footer: the hovered (or picked) emoji and its name.
                        ui.add_space(6.0);
                        let shown = hovered.or_else(|| emojis::get(value));
                        let (r, _) = ui.allocate_exact_size(vec2(width, 36.0), Sense::hover());
                        ui.painter().rect_filled(r, CornerRadius::same(4), pal().bg_secondary);
                        match shown {
                            Some(e) => {
                                ui.painter().text(
                                    pos2(r.left() + 10.0, r.center().y),
                                    Align2::LEFT_CENTER,
                                    e.as_str(),
                                    font(22.0, Weight::Regular),
                                    Color32::WHITE,
                                );
                                let code = e.shortcode().map(|s| format!(":{s}:")).unwrap_or_else(|| e.name().to_string());
                                ui.painter().text(
                                    pos2(r.left() + 44.0, r.center().y),
                                    Align2::LEFT_CENTER,
                                    code,
                                    font(14.0, Weight::Semibold),
                                    pal().header_primary,
                                );
                            }
                            None => {
                                ui.painter().text(
                                    pos2(r.left() + 10.0, r.center().y),
                                    Align2::LEFT_CENTER,
                                    "Pick an emoji for the sound",
                                    font(14.0, Weight::Regular),
                                    pal().text_muted,
                                );
                            }
                        }
                    });
            });
        if let Some(p) = picked {
            *value = p;
            self.open = false;
        } else if area.response.clicked_elsewhere() && !resp.clicked() && ui.input(|i| i.pointer.any_click()) {
            self.open = false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn has_emoji_and_search_finds_them() {
        let count: usize = all().iter().map(|(_, l)| l.len()).sum();
        assert!(count > 1000, "only {count} emoji");
        // Single pictures only: no flags, families or keycaps.
        assert!(all().iter().all(|(_, l)| l.iter().all(|e| drawable(e))));
        let found = |q: &str| rows(q).iter().any(|r| matches!(r, Row::Emoji(l) if l.iter().any(|e| e.as_str() == "🤖")));
        assert!(found("robot"));
        assert!(found(":robot:"));
        assert!(found("ROB"));
        assert!(rows("definitely not an emoji").is_empty());
        assert!(matches!(rows("")[0], Row::Heading("Smileys & Emotion")));
    }
}
