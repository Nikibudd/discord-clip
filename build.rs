//! Embeds the app icon in the exe (#42), so Explorer, the Start menu
//! shortcut, the taskbar and Apps & features show it. The icon is drawn by
//! the same code as the window and tray icon (`src/icon.rs`).

#[allow(dead_code)]
#[path = "src/icon.rs"]
mod icon;

/// The app's blurple, as in `ui::theme::BLURPLE_RGB`.
const BLURPLE: (u8, u8, u8) = (0x58, 0x65, 0xF2);
/// The sizes Windows asks for at the usual DPI settings.
const SIZES: [i32; 8] = [16, 20, 24, 32, 40, 48, 64, 256];

fn main() {
    println!("cargo:rerun-if-changed=src/icon.rs");
    println!("cargo:rerun-if-changed=build.rs");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let out = std::path::PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR"));
    let ico = out.join("discord-clip.ico");
    std::fs::write(&ico, ico_file()).expect("writing the icon");
    let mut res = winresource::WindowsResource::new();
    res.set_icon(ico.to_str().expect("icon path"));
    res.set("FileDescription", "Discord Clip");
    res.set("ProductName", "Discord Clip");
    res.compile().expect("embedding the icon");
}

/// An `.ico` with one 32-bit image per size: BMP (DIB) entries, bottom-up
/// BGRA plus an all-zero AND mask, which every Windows version reads.
fn ico_file() -> Vec<u8> {
    let images: Vec<Vec<u8>> = SIZES.iter().map(|&s| dib(s)).collect();
    let mut out = Vec::new();
    out.extend_from_slice(&[0, 0, 1, 0]); // reserved, type 1 = icon
    out.extend_from_slice(&(SIZES.len() as u16).to_le_bytes());
    let mut offset = 6 + 16 * SIZES.len();
    for (&size, image) in SIZES.iter().zip(&images) {
        let dim = if size >= 256 { 0 } else { size as u8 }; // 0 means 256
        out.extend_from_slice(&[dim, dim, 0, 0]); // width, height, no palette
        out.extend_from_slice(&1u16.to_le_bytes()); // planes
        out.extend_from_slice(&32u16.to_le_bytes()); // bits per pixel
        out.extend_from_slice(&(image.len() as u32).to_le_bytes());
        out.extend_from_slice(&(offset as u32).to_le_bytes());
        offset += image.len();
    }
    for image in images {
        out.extend_from_slice(&image);
    }
    out
}

fn dib(size: i32) -> Vec<u8> {
    let s = size as usize;
    let pixels = icon::bgra(size, BLURPLE); // top-down BGRA, straight alpha
    let mask_row = s.div_ceil(32) * 4; // 1 bit per pixel, rows padded to 4 bytes
    let mut out = Vec::new();
    // BITMAPINFOHEADER; the height counts the colour image and the mask.
    out.extend_from_slice(&40u32.to_le_bytes());
    out.extend_from_slice(&(size).to_le_bytes());
    out.extend_from_slice(&(size * 2).to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&32u16.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes()); // BI_RGB
    out.extend_from_slice(&((s * s * 4 + mask_row * s) as u32).to_le_bytes());
    out.extend_from_slice(&[0; 16]); // resolution and palette: unused
    for row in (0..s).rev() {
        out.extend_from_slice(&pixels[row * s * 4..(row + 1) * s * 4]);
    }
    out.resize(out.len() + mask_row * s, 0);
    out
}
