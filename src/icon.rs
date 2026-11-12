//! The app icon, drawn at runtime: a filled circle with a white waveform.
//! Used for the tray icon and the window.

/// The app icon as top-down RGBA pixels (for the window).
pub fn rgba(size: i32, rgb: (u8, u8, u8)) -> Vec<u8> {
    let mut px = bgra(size, rgb);
    for p in px.chunks_exact_mut(4) {
        p.swap(0, 2);
    }
    px
}

/// Draws the app icon, a filled circle in `rgb` with a white waveform, as
/// top-down BGRA pixels with straight alpha.
pub fn bgra(size: i32, rgb: (u8, u8, u8)) -> Vec<u8> {
    const SS: usize = 4; // supersampling per axis
    const BARS: [(f32, f32); 5] = [(8.5, 3.5), (12.25, 7.0), (16.0, 10.0), (19.75, 7.0), (23.5, 3.5)];
    let scale = 32.0 / size as f32;
    let mut px = vec![0u8; (size * size * 4) as usize];
    for y in 0..size {
        for x in 0..size {
            let (mut circle, mut white) = (0usize, 0usize);
            for sy in 0..SS {
                for sx in 0..SS {
                    let fx = (x as f32 + (sx as f32 + 0.5) / SS as f32) * scale;
                    let fy = (y as f32 + (sy as f32 + 0.5) / SS as f32) * scale;
                    let (dx, dy) = (fx - 16.0, fy - 16.0);
                    if dx * dx + dy * dy <= 15.5 * 15.5 {
                        circle += 1;
                        if BARS.iter().any(|&(cx, hh)| (fx - cx).abs() <= 1.4 && dy.abs() <= hh) {
                            white += 1;
                        }
                    }
                }
            }
            if circle == 0 {
                continue;
            }
            let w = white as f32 / circle as f32;
            let mix = |c: u8| (255.0 * w + c as f32 * (1.0 - w)).round() as u8;
            let i = ((y * size + x) * 4) as usize;
            px[i] = mix(rgb.2);
            px[i + 1] = mix(rgb.1);
            px[i + 2] = mix(rgb.0);
            px[i + 3] = (255.0 * circle as f32 / (SS * SS) as f32).round() as u8;
        }
    }
    px
}

