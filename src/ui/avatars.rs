//! Discord profile pictures as textures (#67), for the people in a clip.
//!
//! The image comes from Discord's public CDN (the avatar id is what
//! `GET_USER` gave), is cached on disk in `%LOCALAPPDATA%\discord-clip\avatars`
//! so it's downloaded once per picture, and decoded on a worker thread.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};

use eframe::egui::{self, ColorImage, TextureHandle, TextureOptions};

use crate::log;

/// Pixels to download; drawn at about 20 px, sharp at 200% scaling.
const SIZE: u32 = 64;

enum Slot {
    Loading(Receiver<Option<ColorImage>>),
    Ready(TextureHandle),
    Failed,
}

#[derive(Default)]
pub struct Avatars {
    slots: HashMap<String, Slot>,
}

impl Avatars {
    /// The picture of `user_id` (avatar id `hash`, or Discord's default
    /// picture without one), once it's loaded; starts loading it otherwise.
    pub fn get(&mut self, ctx: &egui::Context, user_id: &str, hash: Option<&str>) -> Option<&TextureHandle> {
        let key = format!("{user_id}-{}", hash.unwrap_or("default"));
        let slot = self.slots.entry(key.clone()).or_insert_with(|| {
            let (tx, rx) = mpsc::channel();
            let (id, hash, ctx) = (user_id.to_string(), hash.map(str::to_string), ctx.clone());
            let spawned = std::thread::Builder::new().name("avatar".into()).spawn(move || {
                let image = load(&id, hash.as_deref());
                let _ = tx.send(image);
                ctx.request_repaint();
            });
            if spawned.is_err() {
                return Slot::Failed;
            }
            Slot::Loading(rx)
        });
        if let Slot::Loading(rx) = slot {
            match rx.try_recv() {
                Ok(Some(image)) => *slot = Slot::Ready(ctx.load_texture(key, image, TextureOptions::LINEAR)),
                Ok(None) | Err(mpsc::TryRecvError::Disconnected) => *slot = Slot::Failed,
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
        match slot {
            Slot::Ready(t) => Some(t),
            _ => None,
        }
    }
}

fn cache_dir() -> PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join("discord-clip")
        .join("avatars")
}

/// The CDN address of a user's picture. Animated ones (`a_…`) give their
/// still frame as PNG.
pub fn url(user_id: &str, hash: Option<&str>) -> String {
    match hash {
        Some(h) => format!("https://cdn.discordapp.com/avatars/{user_id}/{h}.png?size={SIZE}"),
        // Discord's default pictures, picked from the id like Discord does.
        None => {
            let n = user_id.parse::<u64>().map_or(0, |id| (id >> 22) % 6);
            format!("https://cdn.discordapp.com/embed/avatars/{n}.png")
        }
    }
}

/// From the disk cache, or downloaded into it.
fn load(user_id: &str, hash: Option<&str>) -> Option<ColorImage> {
    let dir = cache_dir();
    let file = dir.join(format!("{user_id}-{}.png", hash.unwrap_or("default")));
    if let Some(image) = std::fs::read(&file).ok().and_then(|b| decode(&b)) {
        return Some(image);
    }
    let mut bytes = Vec::new();
    let got = crate::update::http_get(&url(user_id, hash), |chunk| {
        bytes.extend_from_slice(chunk);
        anyhow::ensure!(bytes.len() < 2 << 20, "picture too big");
        Ok(())
    });
    if let Err(e) = got {
        log!("couldn't load a profile picture: {e:#}");
        return None;
    }
    let image = decode(&bytes)?;
    // Keep it, and drop this user's older pictures.
    let _ = std::fs::create_dir_all(&dir);
    if let Ok(entries) = std::fs::read_dir(&dir) {
        let prefix = format!("{user_id}-");
        for e in entries.flatten() {
            if e.file_name().to_string_lossy().starts_with(&prefix) {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
    let _ = std::fs::write(&file, &bytes);
    Some(image)
}

/// A PNG as RGBA.
fn decode(bytes: &[u8]) -> Option<ColorImage> {
    let mut decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    decoder.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = decoder.read_info().ok()?;
    let mut buf = vec![0; reader.output_buffer_size()?];
    let info = reader.next_frame(&mut buf).ok()?;
    let px = &buf[..info.buffer_size()];
    let rgba: Vec<u8> = match info.color_type {
        png::ColorType::Rgba => px.to_vec(),
        png::ColorType::Rgb => px.chunks_exact(3).flat_map(|c| [c[0], c[1], c[2], 255]).collect(),
        png::ColorType::GrayscaleAlpha => px.chunks_exact(2).flat_map(|c| [c[0], c[0], c[0], c[1]]).collect(),
        png::ColorType::Grayscale => px.iter().flat_map(|&g| [g, g, g, 255]).collect(),
        png::ColorType::Indexed => return None, // expanded by normalize_to_color8
    };
    let size = [info.width as usize, info.height as usize];
    (rgba.len() == size[0] * size[1] * 4).then(|| ColorImage::from_rgba_unmultiplied(size, &rgba))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_cdn_urls() {
        assert_eq!(url("80351110224678912", Some("abc")), "https://cdn.discordapp.com/avatars/80351110224678912/abc.png?size=64");
        // Default picture: (id >> 22) % 6.
        let n = (725726203008516108u64 >> 22) % 6;
        assert_eq!(url("725726203008516108", None), format!("https://cdn.discordapp.com/embed/avatars/{n}.png"));
    }

    #[test]
    fn decodes_a_png() {
        // A 1×1 red pixel, RGB.
        let mut bytes = Vec::new();
        {
            let mut enc = png::Encoder::new(&mut bytes, 1, 1);
            enc.set_color(png::ColorType::Rgb);
            enc.set_depth(png::BitDepth::Eight);
            let mut w = enc.write_header().unwrap();
            w.write_image_data(&[255, 0, 0]).unwrap();
        }
        let image = decode(&bytes).unwrap();
        assert_eq!(image.size, [1, 1]);
        assert_eq!(image.pixels[0], egui::Color32::from_rgb(255, 0, 0));
        assert!(decode(b"not a png").is_none());
    }
}
