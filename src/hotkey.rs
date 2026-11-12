//! Hotkey description: parsing/formatting strings like `Ctrl+Alt+C`.

use std::fmt;
use std::str::FromStr;

// RegisterHotKey modifier bits.
pub const MOD_ALT: u32 = 0x1;
pub const MOD_CONTROL: u32 = 0x2;
pub const MOD_SHIFT: u32 = 0x4;
pub const MOD_WIN: u32 = 0x8;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hotkey {
    /// MOD_* bits.
    pub mods: u32,
    /// Virtual-key code.
    pub vk: u32,
}

impl Default for Hotkey {
    fn default() -> Self {
        Hotkey { mods: MOD_CONTROL | MOD_ALT, vk: 0x43 } // Ctrl+Alt+C
    }
}

/// (name, vk). First name for a vk is the one used when formatting.
const NAMED_KEYS: &[(&str, u32)] = &[
    ("Space", 0x20),
    ("PageUp", 0x21),
    ("PageDown", 0x22),
    ("End", 0x23),
    ("Home", 0x24),
    ("Left", 0x25),
    ("Up", 0x26),
    ("Right", 0x27),
    ("Down", 0x28),
    ("PrintScreen", 0x2C),
    ("Insert", 0x2D),
    ("Delete", 0x2E),
    ("Pause", 0x13),
    ("ScrollLock", 0x91),
    ("Numpad0", 0x60),
    ("Numpad1", 0x61),
    ("Numpad2", 0x62),
    ("Numpad3", 0x63),
    ("Numpad4", 0x64),
    ("Numpad5", 0x65),
    ("Numpad6", 0x66),
    ("Numpad7", 0x67),
    ("Numpad8", 0x68),
    ("Numpad9", 0x69),
    ("NumpadMultiply", 0x6A),
    ("NumpadAdd", 0x6B),
    ("NumpadSubtract", 0x6D),
    ("NumpadDecimal", 0x6E),
    ("NumpadDivide", 0x6F),
    ("MediaNext", 0xB0),
    ("MediaPrev", 0xB1),
    ("MediaStop", 0xB2),
    ("MediaPlayPause", 0xB3),
    ("`", 0xC0),
    ("-", 0xBD),
    ("=", 0xBB),
    ("[", 0xDB),
    ("]", 0xDD),
    ("\\", 0xDC),
    (";", 0xBA),
    ("'", 0xDE),
    (",", 0xBC),
    (".", 0xBE),
    ("/", 0xBF),
    // Aliases (parse only).
    ("PgUp", 0x21),
    ("PgDn", 0x22),
    ("Ins", 0x2D),
    ("Del", 0x2E),
    ("PrtSc", 0x2C),
];

fn key_from_name(name: &str) -> Option<u32> {
    let upper = name.to_ascii_uppercase();
    let b = upper.as_bytes();
    // Single letter or digit.
    if b.len() == 1 && (b[0].is_ascii_uppercase() || b[0].is_ascii_digit()) {
        return Some(b[0] as u32);
    }
    // F1..F24
    if let Some(n) = upper.strip_prefix('F').and_then(|n| n.parse::<u32>().ok()) {
        if (1..=24).contains(&n) {
            return Some(0x70 + n - 1);
        }
    }
    // Raw virtual-key code, e.g. 0x7B.
    if let Some(hex) = upper.strip_prefix("0X") {
        return u32::from_str_radix(hex, 16).ok().filter(|&v| v > 0 && v < 0xFF);
    }
    NAMED_KEYS
        .iter()
        .find(|(n, _)| n.eq_ignore_ascii_case(name))
        .map(|&(_, vk)| vk)
}

fn key_name(vk: u32) -> String {
    match vk {
        0x30..=0x39 | 0x41..=0x5A => char::from(vk as u8).to_string(),
        0x70..=0x87 => format!("F{}", vk - 0x70 + 1),
        _ => NAMED_KEYS
            .iter()
            .find(|&&(_, v)| v == vk)
            .map(|(n, _)| n.to_string())
            .unwrap_or_else(|| format!("0x{vk:02X}")),
    }
}

impl Hotkey {
    /// Rejects combos that would hijack normal typing, e.g. a bare letter or
    /// Shift+letter. Function keys, media keys etc. are fine on their own.
    pub fn check_sensible(&self) -> Result<(), String> {
        if self.mods & (MOD_CONTROL | MOD_ALT | MOD_WIN) != 0 {
            return Ok(());
        }
        let standalone_ok = matches!(self.vk, 0x70..=0x87 | 0x13 | 0x91 | 0x2D | 0xB0..=0xB3);
        if standalone_ok {
            Ok(())
        } else {
            Err(format!(
                "{self} would block normal typing - add Ctrl, Alt or Win, or use an F-key"
            ))
        }
    }
}

impl fmt::Display for Hotkey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (bit, name) in [(MOD_CONTROL, "Ctrl"), (MOD_ALT, "Alt"), (MOD_SHIFT, "Shift"), (MOD_WIN, "Win")] {
            if self.mods & bit != 0 {
                write!(f, "{name}+")?;
            }
        }
        f.write_str(&key_name(self.vk))
    }
}

impl FromStr for Hotkey {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        let mut mods = 0;
        let mut vk = None;
        for part in s.split('+').map(str::trim) {
            if part.is_empty() {
                return Err(format!("invalid hotkey '{s}'"));
            }
            let m = match part.to_ascii_lowercase().as_str() {
                "ctrl" | "control" => MOD_CONTROL,
                "alt" => MOD_ALT,
                "shift" => MOD_SHIFT,
                "win" | "windows" | "super" | "meta" => MOD_WIN,
                _ => 0,
            };
            if m != 0 {
                mods |= m;
                continue;
            }
            if vk.is_some() {
                return Err(format!("hotkey '{s}' has more than one non-modifier key"));
            }
            vk = Some(key_from_name(part).ok_or_else(|| format!("unknown key '{part}' in hotkey '{s}'"))?);
        }
        let vk = vk.ok_or_else(|| format!("hotkey '{s}' has no key, only modifiers"))?;
        Ok(Hotkey { mods, vk })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hk(s: &str) -> Hotkey {
        s.parse().unwrap()
    }

    #[test]
    fn parse_basic() {
        assert_eq!(hk("Ctrl+Alt+C"), Hotkey::default());
        assert_eq!(hk(" ctrl + alt + c "), Hotkey::default());
        assert_eq!(hk("Shift+F9"), Hotkey { mods: MOD_SHIFT, vk: 0x78 });
        assert_eq!(hk("Win+Numpad5"), Hotkey { mods: MOD_WIN, vk: 0x65 });
        assert_eq!(hk("Alt+0x7B"), Hotkey { mods: MOD_ALT, vk: 0x7B });
        assert_eq!(hk("Ctrl+`"), Hotkey { mods: MOD_CONTROL, vk: 0xC0 });
    }

    #[test]
    fn parse_errors() {
        assert!("".parse::<Hotkey>().is_err());
        assert!("Ctrl+Alt".parse::<Hotkey>().is_err());
        assert!("Ctrl+A+B".parse::<Hotkey>().is_err());
        assert!("Ctrl+Bogus".parse::<Hotkey>().is_err());
        assert!("Ctrl++".parse::<Hotkey>().is_err());
        assert!("F25".parse::<Hotkey>().is_err());
    }

    #[test]
    fn roundtrip() {
        for s in ["Ctrl+Alt+C", "Ctrl+Shift+Win+F12", "Alt+PageUp", "F9", "Ctrl+0x07", "Ctrl+Alt+Shift+7", "Win+/"] {
            assert_eq!(hk(s).to_string(), s);
            assert_eq!(hk(&hk(s).to_string()), hk(s));
        }
        // Aliases normalize.
        assert_eq!(hk("ctrl+pgdn").to_string(), "Ctrl+PageDown");
    }

    #[test]
    fn sensible() {
        assert!(hk("Ctrl+Alt+C").check_sensible().is_ok());
        assert!(hk("F9").check_sensible().is_ok());
        assert!(hk("Shift+F9").check_sensible().is_ok());
        assert!(hk("C").check_sensible().is_err());
        assert!(hk("Shift+C").check_sensible().is_err());
    }
}
