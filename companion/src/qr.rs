//! The QR code with the page's address: as a PNG for the page on the PC, and in the terminal.

use image::{GrayImage, Luma};
use qrcode::{Color, QrCode};

pub fn png(text: &str) -> Vec<u8> {
    let Ok(code) = QrCode::new(text.as_bytes()) else { return Vec::new() };
    let (width, border, scale) = (code.width() as u32, 2u32, 10u32);
    let size = (width + 2 * border) * scale;
    let colors = code.to_colors();
    let img = GrayImage::from_fn(size, size, |x, y| {
        let (cx, cy) = ((x / scale) as i64 - border as i64, (y / scale) as i64 - border as i64);
        let dark = cx >= 0 && cy >= 0 && (cx as u32) < width && (cy as u32) < width
            && colors[(cy as u32 * width + cx as u32) as usize] == Color::Dark;
        Luma([if dark { 0 } else { 255 }])
    });
    let mut out = std::io::Cursor::new(Vec::new());
    let _ = img.write_to(&mut out, image::ImageFormat::Png);
    out.into_inner()
}

/// Two rows of modules per line with half-block characters, light on dark like the Python one.
pub fn terminal(text: &str) -> String {
    let Ok(code) = QrCode::new(text.as_bytes()) else { return String::new() };
    let width = code.width() as i64;
    let colors = code.to_colors();
    let dark = |x: i64, y: i64| x >= 0 && y >= 0 && x < width && y < width && colors[(y * width + x) as usize] == Color::Dark;
    let mut out = String::new();
    let mut y = -1;
    while y < width + 1 {
        for x in -1..width + 1 {
            // inverted: dark modules print as spaces, so the code reads on a dark terminal
            out.push(match (!dark(x, y), !dark(x, y + 1)) {
                (true, true) => '█',
                (true, false) => '▀',
                (false, true) => '▄',
                (false, false) => ' ',
            });
        }
        out.push('\n');
        y += 2;
    }
    out
}
