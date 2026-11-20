// FlashValidator icon artwork, drawn procedurally.
//
// Shared by the GUI (window icon) and the build scripts (Windows .ico).
// `assets/flashvalidator.svg` draws the same geometry. This file is pulled in
// with `include!` / `#[path]`, so it contains only plain items with outer
// doc comments and no dependencies.

/// Grid of cells drawn inside the icon (4 x 4, like the region map).
const ICON_GRID: usize = 4;

/// RGBA pixels (row-major, top-down, straight alpha) of the icon at `size` x `size`.
#[allow(dead_code)]
pub fn icon_rgba(size: u32) -> Vec<u8> {
    const SS: u32 = 4; // supersampling per axis
    let bg = [10.0, 10.0, 12.0];
    let border = [38.0, 38.0, 44.0];
    let light = [94.0, 234.0, 212.0];
    let dim = [15.0, 118.0, 110.0];

    let mut out = vec![0u8; (size * size * 4) as usize];
    let n = size as f32;
    for py in 0..size {
        for px in 0..size {
            let mut acc = [0.0f32; 4]; // premultiplied
            for sy in 0..SS {
                for sx in 0..SS {
                    let x = (px as f32 + (sx as f32 + 0.5) / SS as f32) / n;
                    let y = (py as f32 + (sy as f32 + 0.5) / SS as f32) / n;
                    if let Some(rgb) = icon_sample(x, y, bg, border, light, dim) {
                        acc[0] += rgb[0];
                        acc[1] += rgb[1];
                        acc[2] += rgb[2];
                        acc[3] += 1.0;
                    }
                }
            }
            let samples = (SS * SS) as f32;
            let i = ((py * size + px) * 4) as usize;
            if acc[3] > 0.0 {
                out[i] = (acc[0] / acc[3]).round() as u8;
                out[i + 1] = (acc[1] / acc[3]).round() as u8;
                out[i + 2] = (acc[2] / acc[3]).round() as u8;
                out[i + 3] = (acc[3] / samples * 255.0).round() as u8;
            }
        }
    }
    out
}

/// Colour at normalised coordinates (0..1), or `None` outside the icon.
fn icon_sample(x: f32, y: f32, bg: [f32; 3], border: [f32; 3], light: [f32; 3], dim: [f32; 3]) -> Option<[f32; 3]> {
    // Background tile with subtle rounded corners and a hairline border.
    let (lo, hi, radius, stroke) = (0.04, 0.96, 0.14, 0.025);
    if !icon_in_rounded_rect(x, y, lo, lo, hi, hi, radius) {
        return None;
    }
    let inner = icon_in_rounded_rect(x, y, lo + stroke, lo + stroke, hi - stroke, hi - stroke, radius - stroke);
    if !inner {
        return Some(border);
    }
    // 4 x 4 grid of cells; colour fades from light (top-left) to dim (bottom-right).
    let (g0, g1, gap) = (0.21, 0.79, 0.045);
    let cell = (g1 - g0 - gap * (ICON_GRID as f32 - 1.0)) / ICON_GRID as f32;
    for r in 0..ICON_GRID {
        for c in 0..ICON_GRID {
            let cx = g0 + c as f32 * (cell + gap);
            let cy = g0 + r as f32 * (cell + gap);
            if icon_in_rounded_rect(x, y, cx, cy, cx + cell, cy + cell, 0.018) {
                let t = (r + c) as f32 / (2.0 * (ICON_GRID as f32 - 1.0));
                return Some([
                    light[0] + (dim[0] - light[0]) * t,
                    light[1] + (dim[1] - light[1]) * t,
                    light[2] + (dim[2] - light[2]) * t,
                ]);
            }
        }
    }
    Some(bg)
}

fn icon_in_rounded_rect(x: f32, y: f32, x0: f32, y0: f32, x1: f32, y1: f32, r: f32) -> bool {
    if x < x0 || x > x1 || y < y0 || y > y1 {
        return false;
    }
    let cx = x.clamp(x0 + r, x1 - r);
    let cy = y.clamp(y0 + r, y1 - r);
    let (dx, dy) = (x - cx, y - cy);
    dx * dx + dy * dy <= r * r
}

/// Encodes 32-bit BMP-based icon images (one per size) as a Windows `.ico` file.
#[allow(dead_code)]
pub fn icon_ico(sizes: &[u32]) -> Vec<u8> {
    let images: Vec<Vec<u8>> = sizes
        .iter()
        .map(|&s| {
            let rgba = icon_rgba(s);
            let mask_row = (s as usize).div_ceil(32) * 4;
            let mut img = Vec::new();
            // BITMAPINFOHEADER: height is doubled (colour + AND mask).
            for v in [40u32, s, s * 2] {
                img.extend_from_slice(&v.to_le_bytes());
            }
            img.extend_from_slice(&1u16.to_le_bytes());
            img.extend_from_slice(&32u16.to_le_bytes());
            for v in [0u32, s * s * 4 + (mask_row * s as usize) as u32, 0, 0, 0, 0] {
                img.extend_from_slice(&v.to_le_bytes());
            }
            // Pixels bottom-up, BGRA.
            for y in (0..s).rev() {
                for x in 0..s {
                    let i = ((y * s + x) * 4) as usize;
                    img.extend_from_slice(&[rgba[i + 2], rgba[i + 1], rgba[i], rgba[i + 3]]);
                }
            }
            img.extend(std::iter::repeat_n(0u8, mask_row * s as usize));
            img
        })
        .collect();

    let mut ico = Vec::new();
    ico.extend_from_slice(&[0, 0, 1, 0]);
    ico.extend_from_slice(&(sizes.len() as u16).to_le_bytes());
    let mut offset = 6 + 16 * sizes.len();
    for (&s, img) in sizes.iter().zip(&images) {
        let dim = if s >= 256 { 0 } else { s as u8 };
        ico.extend_from_slice(&[dim, dim, 0, 0]);
        ico.extend_from_slice(&1u16.to_le_bytes());
        ico.extend_from_slice(&32u16.to_le_bytes());
        ico.extend_from_slice(&(img.len() as u32).to_le_bytes());
        ico.extend_from_slice(&(offset as u32).to_le_bytes());
        offset += img.len();
    }
    for img in images {
        ico.extend_from_slice(&img);
    }
    ico
}
