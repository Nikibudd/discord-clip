//! Discord's look: colours (following the Discord theme, #37), fonts, egui
//! style and the title bar.

use std::ffi::c_void;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use eframe::egui::{self, vec2, Color32, FontData, FontDefinitions, FontFamily, FontId, Stroke};
use windows::Win32::Foundation::HWND;

use crate::discord_theme::{Base, DiscordTheme, Tint};

pub const fn rgb(r: u8, g: u8, b: u8) -> Color32 {
    Color32::from_rgb(r, g, b)
}

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


// ---------------------------------------------------------------------------
// Colours
// ---------------------------------------------------------------------------

pub const BLURPLE_RGB: (u8, u8, u8) = (0x58, 0x65, 0xF2);
pub const IDLE_RGB: (u8, u8, u8) = (0x80, 0x84, 0x8E);

// The same in every Discord theme. Text on these is always white.
pub const BLURPLE: Color32 = rgb(0x58, 0x65, 0xF2);
pub const BLURPLE_HOVER: Color32 = rgb(0x44, 0x52, 0xBB);
pub const BLURPLE_ACTIVE: Color32 = rgb(0x3A, 0x48, 0xA3);
pub const GREEN_BTN: Color32 = rgb(0x00, 0x85, 0x45);
pub const GREEN_BTN_HOVER: Color32 = rgb(0x00, 0x6C, 0x37);
pub const GREEN_BTN_ACTIVE: Color32 = rgb(0x00, 0x5F, 0x30);
pub const RED_BTN: Color32 = rgb(0xD2, 0x2D, 0x39);
pub const RED_BTN_HOVER: Color32 = rgb(0xA9, 0x23, 0x2E);
pub const RED_BTN_ACTIVE: Color32 = rgb(0x97, 0x1D, 0x28);

/// The colours that change with the theme, after Discord's own CSS
/// variables (Nov 2026; see #37 for which variable fits which role).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Palette {
    pub dark: bool,
    /// Content area.
    pub bg_primary: Color32,
    /// Sidebar, cards.
    pub bg_secondary: Color32,
    pub bg_secondary_alt: Color32,
    /// Inputs, waveform panels, the title bar.
    pub bg_tertiary: Color32,
    /// Menus, dialogs, tooltips.
    pub bg_floating: Color32,
    pub bg_hover: Color32,
    pub bg_selected: Color32,
    pub divider: Color32,
    pub header_primary: Color32,
    pub header_secondary: Color32,
    pub text_normal: Color32,
    pub text_muted: Color32,
    pub link: Color32,
    pub gray_btn: Color32,
    pub gray_btn_hover: Color32,
    pub gray_btn_active: Color32,
    pub green: Color32,
    pub red: Color32,
    pub yellow: Color32,
    pub toggle_off: Color32,
}

const fn hex(v: u32) -> Color32 {
    rgb((v >> 16) as u8, (v >> 8) as u8, v as u8)
}

const DARK_STATUS: (Color32, Color32, Color32) = (hex(0x23A55A), hex(0xF23F43), hex(0xF0B232));

impl Palette {
    pub const LIGHT: Palette = Palette {
        dark: false,
        bg_primary: hex(0xFBFBFB),
        bg_secondary: hex(0xF3F3F4),
        bg_secondary_alt: hex(0xFBFBFB),
        bg_tertiary: hex(0xE8E8EA),
        bg_floating: hex(0xFFFFFF),
        bg_hover: hex(0xEFEFF0),
        bg_selected: hex(0xE3E3E5),
        divider: hex(0xDFDFE1),
        header_primary: hex(0x28282D),
        header_secondary: hex(0x595A63),
        text_normal: hex(0x2E2E34),
        text_muted: hex(0x6C6D76),
        link: hex(0x006DD4),
        gray_btn: hex(0xE3E3E5),
        gray_btn_hover: hex(0xD9D9DC),
        gray_btn_active: hex(0xCFCFD3),
        green: hex(0x24934E),
        red: hex(0xDA373C),
        yellow: hex(0xAF7615),
        toggle_off: hex(0x9A9BA3),
    };

    pub const ASH: Palette = Palette {
        dark: true,
        bg_primary: hex(0x323339),
        bg_secondary: hex(0x2C2D32),
        bg_secondary_alt: hex(0x36373E),
        bg_tertiary: hex(0x25262A),
        bg_floating: hex(0x393A41),
        bg_hover: hex(0x42434A),
        bg_selected: hex(0x494A51),
        divider: hex(0x42434A),
        header_primary: hex(0xFFFFFF),
        header_secondary: hex(0xC5C6CA),
        text_normal: hex(0xF3F3F4),
        text_muted: hex(0xABACB2),
        link: hex(0x76AFF6),
        gray_btn: hex(0x4A4B53),
        gray_btn_hover: hex(0x55565E),
        gray_btn_active: hex(0x5E5F67),
        green: DARK_STATUS.0,
        red: DARK_STATUS.1,
        yellow: DARK_STATUS.2,
        toggle_off: hex(0x80848E),
    };

    pub const DARK: Palette = Palette {
        dark: true,
        bg_primary: hex(0x1A1A1E),
        bg_secondary: hex(0x121214),
        bg_secondary_alt: hex(0x202024),
        bg_tertiary: hex(0x070709),
        bg_floating: hex(0x242429),
        bg_hover: hex(0x2E2E33),
        bg_selected: hex(0x38383D),
        divider: hex(0x2E2E33),
        header_primary: hex(0xFBFBFB),
        header_secondary: hex(0xABACB2),
        text_normal: hex(0xEFEFF1),
        text_muted: hex(0x96979E),
        link: hex(0x4D96EE),
        gray_btn: hex(0x38383D),
        gray_btn_hover: hex(0x424247),
        gray_btn_active: hex(0x4C4C52),
        green: DARK_STATUS.0,
        red: DARK_STATUS.1,
        yellow: DARK_STATUS.2,
        toggle_off: hex(0x80848E),
    };

    pub const ONYX: Palette = Palette {
        dark: true,
        bg_primary: hex(0x000000),
        bg_secondary: hex(0x000000),
        bg_secondary_alt: hex(0x000000),
        bg_tertiary: hex(0x000000),
        bg_floating: hex(0x0A0A0C),
        bg_hover: hex(0x121213),
        bg_selected: hex(0x242426),
        divider: hex(0x1E1E20),
        header_primary: hex(0xDCDCDF),
        header_secondary: hex(0x96979E),
        text_normal: hex(0xD4D5D8),
        text_muted: hex(0x81828A),
        link: hex(0x2781E7),
        gray_btn: hex(0x2A2A2D),
        gray_btn_hover: hex(0x343437),
        gray_btn_active: hex(0x3E3E42),
        green: DARK_STATUS.0,
        red: DARK_STATUS.1,
        yellow: DARK_STATUS.2,
        toggle_off: hex(0x80848E),
    };

    pub fn of(base: Base) -> Palette {
        match base {
            Base::Light => Palette::LIGHT,
            Base::Ash => Palette::ASH,
            Base::Dark => Palette::DARK,
            Base::Onyx => Palette::ONYX,
        }
    }

    /// The palette for a theme read from Discord: its base theme, tinted
    /// like a Nitro gradient / custom theme, with Discord's Saturation.
    pub fn from_discord(theme: &DiscordTheme) -> Palette {
        let mut p = Palette::of(theme.base);
        if let Some(tint) = &theme.tint {
            p.tint(tint);
        }
        if theme.saturation < 0.999 {
            for c in p.colors_mut() {
                *c = saturate(*c, theme.saturation);
            }
        }
        p
    }

    fn colors_mut(&mut self) -> [&mut Color32; 20] {
        [
            &mut self.bg_primary,
            &mut self.bg_secondary,
            &mut self.bg_secondary_alt,
            &mut self.bg_tertiary,
            &mut self.bg_floating,
            &mut self.bg_hover,
            &mut self.bg_selected,
            &mut self.divider,
            &mut self.gray_btn,
            &mut self.gray_btn_hover,
            &mut self.gray_btn_active,
            &mut self.header_primary,
            &mut self.header_secondary,
            &mut self.text_normal,
            &mut self.text_muted,
            &mut self.link,
            &mut self.green,
            &mut self.red,
            &mut self.yellow,
            &mut self.toggle_off,
        ]
    }

    /// Like Discord: the darkest theme colour (lightest on Light) at full
    /// saturation and 5 % (90 %) lightness is mixed into every background,
    /// `color-mix(in oklab, <background> 100%, <tint> <mix>%)`.
    fn tint(&mut self, tint: &Tint) {
        let luma = |c: &[u8; 3]| 0.299 * c[0] as f32 + 0.587 * c[1] as f32 + 0.114 * c[2] as f32;
        let pick = if self.dark {
            tint.colors.iter().min_by(|a, b| luma(a).total_cmp(&luma(b)))
        } else {
            tint.colors.iter().max_by(|a, b| luma(a).total_cmp(&luma(b)))
        };
        let Some(&[r, g, b]) = pick else { return };
        let (h, _, _) = to_hsl(rgb(r, g, b));
        let color = from_hsl(h, 1.0, if self.dark { 0.05 } else { 0.90 });
        // CSS scales percentages that add up to more than 100 %.
        let w = tint.mix / (1.0 + tint.mix);
        // The first 11 colours are the backgrounds.
        for c in self.colors_mut().into_iter().take(11) {
            *c = mix_oklab(*c, color, w);
        }
    }

    /// Text on a button of this kind's colour.
    pub fn on_button(&self, secondary: bool) -> Color32 {
        if secondary {
            self.header_primary
        } else {
            Color32::WHITE
        }
    }
}

/// The palette in use; the window changes it when the theme changes.
static PALETTE: RwLock<Palette> = RwLock::new(Palette::DARK);

/// The current colours.
pub fn pal() -> Palette {
    *PALETTE.read().unwrap_or_else(|e| e.into_inner())
}

/// Switches to `palette`: colours, egui's style and the title bar.
pub fn apply(ctx: &egui::Context, palette: Palette) {
    *PALETTE.write().unwrap_or_else(|e| e.into_inner()) = palette;
    setup_style(ctx, &palette);
    if let Some(hwnd) = super::window_hwnd() {
        style_title_bar(hwnd, &palette);
    }
    ctx.request_repaint();
}

fn setup_style(ctx: &egui::Context, p: &Palette) {
    ctx.set_theme(if p.dark { egui::Theme::Dark } else { egui::Theme::Light });
    ctx.all_styles_mut(|style| {
        style.spacing.item_spacing = vec2(8.0, 0.0);
        style.spacing.scroll.bar_width = 8.0;
        style.spacing.scroll.floating = false;
        let v = &mut style.visuals;
        v.dark_mode = p.dark;
        v.panel_fill = p.bg_primary;
        v.window_fill = p.bg_primary;
        v.extreme_bg_color = p.bg_tertiary;
        v.faint_bg_color = p.bg_secondary;
        v.hyperlink_color = p.link;
        v.selection.bg_fill = BLURPLE;
        v.selection.stroke = Stroke::new(1.0, Color32::WHITE);
        v.text_cursor.stroke = Stroke::new(2.0, p.header_primary);
        v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, p.text_normal);
        v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, p.divider);
        // Scrollbar handle.
        let handle = if p.dark { p.bg_tertiary } else { p.bg_selected };
        v.widgets.inactive.bg_fill = handle;
        v.widgets.hovered.bg_fill = handle;
        v.widgets.active.bg_fill = handle;
        v.widgets.inactive.fg_stroke = Stroke::new(1.0, p.text_normal);
    });
}

/// Native title bar in the theme's colours (dark mode on Windows 10 20H1+,
/// the caption colour on Windows 11).
pub fn style_title_bar(hwnd: HWND, p: &Palette) {
    use windows::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWINDOWATTRIBUTE};
    const DWMWA_USE_IMMERSIVE_DARK_MODE: i32 = 20;
    const DWMWA_CAPTION_COLOR: i32 = 35;
    const DWMWA_TEXT_COLOR: i32 = 36;
    let colorref = |c: Color32| u32::from(c.r()) | u32::from(c.g()) << 8 | u32::from(c.b()) << 16;
    let dark: i32 = p.dark.into();
    let caption = colorref(p.bg_tertiary);
    let text = colorref(p.header_primary);
    let set = |attr: i32, value: *const c_void| unsafe {
        let _ = DwmSetWindowAttribute(hwnd, DWMWINDOWATTRIBUTE(attr), value, 4);
    };
    set(DWMWA_USE_IMMERSIVE_DARK_MODE, &dark as *const i32 as *const c_void);
    set(DWMWA_CAPTION_COLOR, &caption as *const u32 as *const c_void);
    set(DWMWA_TEXT_COLOR, &text as *const u32 as *const c_void);
}

pub fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let l = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgb(l(a.r(), b.r()), l(a.g(), b.g()), l(a.b(), b.b()))
}

// ---------------------------------------------------------------------------
// Colour maths for tints and Saturation
// ---------------------------------------------------------------------------

fn to_hsl(c: Color32) -> (f32, f32, f32) {
    let [r, g, b] = [c.r(), c.g(), c.b()].map(|v| v as f32 / 255.0);
    let (max, min) = (r.max(g).max(b), r.min(g).min(b));
    let l = (max + min) / 2.0;
    let d = max - min;
    if d < 1e-6 {
        return (0.0, 0.0, l);
    }
    let s = d / (1.0 - (2.0 * l - 1.0).abs());
    let h = if max == r {
        ((g - b) / d).rem_euclid(6.0)
    } else if max == g {
        (b - r) / d + 2.0
    } else {
        (r - g) / d + 4.0
    } * 60.0;
    (h, s, l)
}

fn from_hsl(h: f32, s: f32, l: f32) -> Color32 {
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let x = c * (1.0 - ((h / 60.0).rem_euclid(2.0) - 1.0).abs());
    let m = l - c / 2.0;
    let (r, g, b) = match (h.rem_euclid(360.0) / 60.0) as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let u = |v: f32| ((v + m).clamp(0.0, 1.0) * 255.0).round() as u8;
    rgb(u(r), u(g), u(b))
}

/// Discord's Saturation setting scales the S of every HSL colour.
fn saturate(c: Color32, factor: f32) -> Color32 {
    let (h, s, l) = to_hsl(c);
    from_hsl(h, s * factor, l)
}

fn to_oklab(c: Color32) -> [f32; 3] {
    let lin = |v: u8| {
        let v = v as f32 / 255.0;
        if v <= 0.04045 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    };
    let (r, g, b) = (lin(c.r()), lin(c.g()), lin(c.b()));
    let l = (0.412_221_47 * r + 0.536_332_55 * g + 0.051_445_995 * b).cbrt();
    let m = (0.211_903_5 * r + 0.680_699_5 * g + 0.107_396_96 * b).cbrt();
    let s = (0.088_302_46 * r + 0.281_718_85 * g + 0.629_978_7 * b).cbrt();
    [
        0.210_454_26 * l + 0.793_617_8 * m - 0.004_072_047 * s,
        1.977_998_5 * l - 2.428_592_2 * m + 0.450_593_7 * s,
        0.025_904_037 * l + 0.782_771_77 * m - 0.808_675_77 * s,
    ]
}

fn from_oklab([l, a, b]: [f32; 3]) -> Color32 {
    let l_ = (l + 0.396_337_78 * a + 0.215_803_76 * b).powi(3);
    let m_ = (l - 0.105_561_346 * a - 0.063_854_17 * b).powi(3);
    let s_ = (l - 0.089_484_18 * a - 1.291_485_5 * b).powi(3);
    let r = 4.076_741_7 * l_ - 3.307_711_6 * m_ + 0.230_969_94 * s_;
    let g = -1.268_438 * l_ + 2.609_757_4 * m_ - 0.341_319_38 * s_;
    let bl = -0.004_196_086_3 * l_ - 0.703_418_6 * m_ + 1.707_614_7 * s_;
    let enc = |v: f32| {
        let v = v.clamp(0.0, 1.0);
        let v = if v <= 0.003_130_8 { v * 12.92 } else { 1.055 * v.powf(1.0 / 2.4) - 0.055 };
        (v * 255.0).round() as u8
    };
    rgb(enc(r), enc(g), enc(bl))
}

/// `b` mixed into `a` by `t` (0–1), in Oklab like CSS `color-mix`.
fn mix_oklab(a: Color32, b: Color32, t: f32) -> Color32 {
    let (x, y) = (to_oklab(a), to_oklab(b));
    from_oklab([0, 1, 2].map(|i| x[i] + (y[i] - x[i]) * t))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colour_maths_round_trips() {
        for c in [hex(0x5865F2), hex(0x1A1A1E), hex(0xFBFBFB), hex(0x000000), hex(0x331895)] {
            assert_eq!(from_oklab(to_oklab(c)), c);
            let (h, s, l) = to_hsl(c);
            assert_eq!(from_hsl(h, s, l), c);
        }
        let grey = saturate(hex(0x5865F2), 0.0);
        assert!(grey.r() == grey.g() && grey.g() == grey.b(), "{grey:?}");
    }

    #[test]
    fn custom_theme_tints_backgrounds_only() {
        let theme = DiscordTheme {
            base: Base::Dark,
            tint: Some(Tint { colors: vec![[0x33, 0x18, 0x95], [0x29, 0x0A, 0x42]], mix: 0.53 }),
            saturation: 1.0,
        };
        let p = Palette::from_discord(&theme);
        assert_ne!(p.bg_primary, Palette::DARK.bg_primary);
        // Purple-ish: more blue than green.
        assert!(p.bg_primary.b() > p.bg_primary.g(), "{:?}", p.bg_primary);
        assert_eq!(p.text_normal, Palette::DARK.text_normal);
        assert_eq!(Palette::from_discord(&DiscordTheme { tint: None, ..theme }), Palette::DARK);
    }
}
