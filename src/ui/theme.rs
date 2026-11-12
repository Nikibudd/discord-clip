//! Discord's look: colors, fonts, egui style and the dark title bar.

use std::ffi::c_void;
use std::path::PathBuf;
use std::sync::Arc;

use eframe::egui::{self, vec2, Color32, FontData, FontDefinitions, FontFamily, FontId, Stroke};
use windows::Win32::Foundation::HWND;

pub const fn rgb(r: u8, g: u8, b: u8) -> Color32 {
    Color32::from_rgb(r, g, b)
}

pub const BLURPLE_RGB: (u8, u8, u8) = (0x58, 0x65, 0xF2);
pub const IDLE_RGB: (u8, u8, u8) = (0x80, 0x84, 0x8E);

pub const BG_PRIMARY: Color32 = rgb(0x31, 0x33, 0x38);
pub const BG_SECONDARY: Color32 = rgb(0x2B, 0x2D, 0x31);
pub const BG_SECONDARY_ALT: Color32 = rgb(0x23, 0x24, 0x28);
pub const BG_TERTIARY: Color32 = rgb(0x1E, 0x1F, 0x22);
pub const BG_FLOATING: Color32 = rgb(0x11, 0x12, 0x14);
pub const BG_HOVER: Color32 = rgb(0x35, 0x37, 0x3C);
pub const BG_SELECTED: Color32 = rgb(0x40, 0x42, 0x49);
pub const DIVIDER: Color32 = rgb(0x3F, 0x41, 0x47);

pub const HEADER_PRIMARY: Color32 = rgb(0xF2, 0xF3, 0xF5);
pub const HEADER_SECONDARY: Color32 = rgb(0xB5, 0xBA, 0xC1);
pub const TEXT_NORMAL: Color32 = rgb(0xDB, 0xDE, 0xE1);
pub const TEXT_MUTED: Color32 = rgb(0x94, 0x9B, 0xA4);

pub const BLURPLE: Color32 = rgb(0x58, 0x65, 0xF2);
pub const BLURPLE_HOVER: Color32 = rgb(0x47, 0x52, 0xC4);
pub const BLURPLE_ACTIVE: Color32 = rgb(0x3C, 0x45, 0xA5);
pub const GRAY_BTN: Color32 = rgb(0x4E, 0x50, 0x58);
pub const GRAY_BTN_HOVER: Color32 = rgb(0x6D, 0x6F, 0x78);
pub const GRAY_BTN_ACTIVE: Color32 = rgb(0x80, 0x84, 0x8E);
pub const GREEN_BTN: Color32 = rgb(0x24, 0x80, 0x46);
pub const GREEN_BTN_HOVER: Color32 = rgb(0x1A, 0x63, 0x34);
pub const GREEN_BTN_ACTIVE: Color32 = rgb(0x15, 0x56, 0x2B);
pub const RED_BTN: Color32 = rgb(0xDA, 0x37, 0x3C);
pub const RED_BTN_HOVER: Color32 = rgb(0xA1, 0x28, 0x29);
pub const RED_BTN_ACTIVE: Color32 = rgb(0x8F, 0x22, 0x23);

pub const GREEN: Color32 = rgb(0x23, 0xA5, 0x5A);
pub const RED: Color32 = rgb(0xF2, 0x3F, 0x43);
pub const YELLOW: Color32 = rgb(0xF0, 0xB2, 0x32);
pub const TOGGLE_OFF: Color32 = rgb(0x80, 0x84, 0x8E);

#[derive(Clone, Copy)]
pub enum Weight {
    Regular,
    Semibold,
    Bold,
}

pub fn font(size: f32, weight: Weight) -> FontId {
    let family = match weight {
        Weight::Regular => FontFamily::Proportional,
        Weight::Semibold => FontFamily::Name("semibold".into()),
        Weight::Bold => FontFamily::Name("bold".into()),
    };
    FontId::new(size, family)
}

/// Discord's own font isn't available, so use Segoe UI (falling back to
/// egui's built-in font if it's missing).
pub fn setup_fonts(ctx: &egui::Context) {
    let mut defs = FontDefinitions::default();
    let dir = std::env::var_os("WINDIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Windows"))
        .join("Fonts");
    let mut load = |name: &str, file: &str| match std::fs::read(dir.join(file)) {
        Ok(bytes) => {
            defs.font_data.insert(name.to_string(), Arc::new(FontData::from_owned(bytes)));
            Some(name.to_string())
        }
        Err(_) => None,
    };
    let regular = load("segoe", "segoeui.ttf");
    let semibold = load("segoe-semibold", "seguisb.ttf");
    let bold = load("segoe-bold", "segoeuib.ttf");

    let mut base = defs.families.get(&FontFamily::Proportional).cloned().unwrap_or_default();
    if let Some(r) = &regular {
        base.insert(0, r.clone());
    }
    let with_first = |first: &Option<String>| {
        let mut list = base.clone();
        if let Some(f) = first {
            list.insert(0, f.clone());
        }
        list
    };
    defs.families.insert(FontFamily::Name("semibold".into()), with_first(&semibold));
    defs.families.insert(FontFamily::Name("bold".into()), with_first(&bold));
    defs.families.insert(FontFamily::Proportional, base);
    ctx.set_fonts(defs);
}

pub fn setup_style(ctx: &egui::Context) {
    ctx.set_theme(egui::Theme::Dark);
    ctx.all_styles_mut(|style| {
        style.spacing.item_spacing = vec2(8.0, 0.0);
        style.spacing.scroll.bar_width = 8.0;
        style.spacing.scroll.floating = false;
        let v = &mut style.visuals;
        v.dark_mode = true;
        v.panel_fill = BG_PRIMARY;
        v.window_fill = BG_PRIMARY;
        v.extreme_bg_color = BG_TERTIARY;
        v.faint_bg_color = BG_SECONDARY;
        v.hyperlink_color = rgb(0x00, 0xA8, 0xFC);
        v.selection.bg_fill = BLURPLE;
        v.selection.stroke = Stroke::new(1.0, Color32::WHITE);
        v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, TEXT_NORMAL);
        v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, DIVIDER);
        // Scrollbar handle.
        v.widgets.inactive.bg_fill = BG_TERTIARY;
        v.widgets.hovered.bg_fill = BG_TERTIARY;
        v.widgets.active.bg_fill = BG_TERTIARY;
        v.widgets.inactive.fg_stroke = Stroke::new(1.0, TEXT_NORMAL);
    });
}

/// Dark native title bar (Windows 10 20H1+), tinted to match on Windows 11.
pub fn style_title_bar(hwnd: HWND) {
    use windows::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWINDOWATTRIBUTE};
    const DWMWA_USE_IMMERSIVE_DARK_MODE: i32 = 20;
    const DWMWA_CAPTION_COLOR: i32 = 35;
    let dark: i32 = 1;
    let caption: u32 = 0x0022_1F1E; // COLORREF 0x00BBGGRR of #1E1F22
    unsafe {
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWINDOWATTRIBUTE(DWMWA_USE_IMMERSIVE_DARK_MODE),
            &dark as *const i32 as *const c_void,
            4,
        );
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWINDOWATTRIBUTE(DWMWA_CAPTION_COLOR),
            &caption as *const u32 as *const c_void,
            4,
        );
    }
}

pub fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let l = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgb(l(a.r(), b.r()), l(a.g(), b.g()), l(a.b(), b.b()))
}
