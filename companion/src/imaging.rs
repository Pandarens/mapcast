//! Image helpers on RGBA buffers: load (BLP, TGA, PNG, JPEG), crop, paste, composite, resize, save.

use crate::blp;
use image::imageops::{self, FilterType};
use image::{Rgba, RgbaImage};
use std::path::Path;

pub fn load_image(path: &Path) -> Option<RgbaImage> {
    let bytes = std::fs::read(path).ok()?;
    let ext = path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
    if ext == "blp" || bytes.starts_with(b"BLP2") {
        return blp::open_blp(&bytes);
    }
    if ext == "webp" || (bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP")) {
        return webp::Decoder::new(&bytes).decode().map(|i| i.to_image().to_rgba8());
    }
    let format = match ext.as_str() {
        "tga" => image::ImageFormat::Tga,
        "png" => image::ImageFormat::Png,
        "jpg" | "jpeg" => image::ImageFormat::Jpeg,
        _ => image::guess_format(&bytes).ok()?,
    };
    image::load_from_memory_with_format(&bytes, format).ok().map(|i| i.to_rgba8())
}

pub fn png_bytes(img: &RgbaImage) -> Vec<u8> {
    let mut out = std::io::Cursor::new(Vec::new());
    let _ = img.write_to(&mut out, image::ImageFormat::Png);
    out.into_inner()
}

/// Lossy WebP with alpha (OpenMap's zone tiles: a quarter of PNG's size, no visible loss).
pub fn webp_bytes(img: &RgbaImage, quality: f32) -> Vec<u8> {
    let encoder = webp::Encoder::from_rgba(img.as_raw(), img.width(), img.height());
    match webp::WebPConfig::new() {
        Ok(mut config) => {
            config.quality = quality;
            config.method = 2;
            match encoder.encode_advanced(&config) {
                Ok(mem) => mem.to_vec(),
                Err(_) => encoder.encode(quality).to_vec(),
            }
        }
        Err(_) => encoder.encode(quality).to_vec(),
    }
}

pub fn crop(img: &RgbaImage, x: u32, y: u32, w: u32, h: u32) -> RgbaImage {
    let x = x.min(img.width());
    let y = y.min(img.height());
    let w = w.min(img.width() - x);
    let h = h.min(img.height() - y);
    imageops::crop_imm(img, x, y, w, h).to_image()
}

pub fn resize(img: &RgbaImage, w: u32, h: u32, filter: FilterType) -> RgbaImage {
    if img.width() == w && img.height() == h {
        return img.clone();
    }
    imageops::resize(img, w.max(1), h.max(1), filter)
}

pub fn lanczos(img: &RgbaImage, w: u32, h: u32) -> RgbaImage {
    resize(img, w, h, FilterType::Lanczos3)
}

pub fn bilinear(img: &RgbaImage, w: u32, h: u32) -> RgbaImage {
    resize(img, w, h, FilterType::Triangle)
}

/// Half the size, each pixel the mean of the 2 x 2 it covers (Pillow's BOX filter at exactly 1/2).
pub fn box_half(img: &RgbaImage) -> RgbaImage {
    let (w, h) = ((img.width() / 2).max(1), (img.height() / 2).max(1));
    let mut out = RgbaImage::new(w, h);
    for y in 0..h {
        for x in 0..w {
            let mut acc = [0u32; 4];
            for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                let p = img.get_pixel((2 * x + dx).min(img.width() - 1), (2 * y + dy).min(img.height() - 1));
                for c in 0..4 {
                    acc[c] += p[c] as u32;
                }
            }
            out.put_pixel(x, y, Rgba([((acc[0] + 2) / 4) as u8, ((acc[1] + 2) / 4) as u8,
                                     ((acc[2] + 2) / 4) as u8, ((acc[3] + 2) / 4) as u8]));
        }
    }
    out
}

/// Copies `src` onto `dst` at (x, y), replacing what is there, clipped to `dst`.
pub fn paste(dst: &mut RgbaImage, src: &RgbaImage, x: i64, y: i64) {
    for sy in 0..src.height() as i64 {
        let dy = y + sy;
        if dy < 0 || dy >= dst.height() as i64 {
            continue;
        }
        for sx in 0..src.width() as i64 {
            let dx = x + sx;
            if dx < 0 || dx >= dst.width() as i64 {
                continue;
            }
            dst.put_pixel(dx as u32, dy as u32, *src.get_pixel(sx as u32, sy as u32));
        }
    }
}

/// `src` over `dst` at (x, y) (Porter-Duff over, as Pillow's alpha_composite), clipped to `dst`.
pub fn alpha_composite(dst: &mut RgbaImage, src: &RgbaImage, x: i64, y: i64) {
    for sy in 0..src.height() as i64 {
        let dy = y + sy;
        if dy < 0 || dy >= dst.height() as i64 {
            continue;
        }
        for sx in 0..src.width() as i64 {
            let dx = x + sx;
            if dx < 0 || dx >= dst.width() as i64 {
                continue;
            }
            let s = src.get_pixel(sx as u32, sy as u32);
            let d = dst.get_pixel_mut(dx as u32, dy as u32);
            let sa = s[3] as f32 / 255.0;
            if sa <= 0.0 {
                continue;
            }
            let da = d[3] as f32 / 255.0;
            let oa = sa + da * (1.0 - sa);
            for c in 0..3 {
                let v = (s[c] as f32 * sa + d[c] as f32 * da * (1.0 - sa)) / oa;
                d[c] = v.round().clamp(0.0, 255.0) as u8;
            }
            d[3] = (oa * 255.0).round() as u8;
        }
    }
}

pub const PARCHMENT: [u8; 4] = [214, 196, 158, 255];

/// A map without its art: parchment and a grid.
pub fn placeholder(width: u32, height: u32) -> RgbaImage {
    let mut img = RgbaImage::from_pixel(width.max(1), height.max(1), Rgba(PARCHMENT));
    let line = Rgba([170, 150, 110, 255]);
    for i in 1..10 {
        let x = width * i / 10;
        let y = height * i / 10;
        for yy in 0..height {
            img.put_pixel(x.min(width - 1), yy, line);
        }
        for xx in 0..width {
            img.put_pixel(xx, y.min(height - 1), line);
        }
    }
    let border = Rgba([120, 100, 70, 255]);
    for t in 0..3 {
        for xx in 0..width {
            img.put_pixel(xx, t.min(height - 1), border);
            img.put_pixel(xx, (height - 1).saturating_sub(t), border);
        }
        for yy in 0..height {
            img.put_pixel(t.min(width - 1), yy, border);
            img.put_pixel((width - 1).saturating_sub(t), yy, border);
        }
    }
    img
}
