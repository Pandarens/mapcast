//! BLP2 textures: DXT1/3/5 and raw BGRA, as the game and addons ship them.

use image::RgbaImage;

fn u32_at(d: &[u8], at: usize) -> Option<u32> {
    d.get(at..at + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

/// Decode a BLP2 file to RGBA (the first mipmap).
pub fn open_blp(data: &[u8]) -> Option<RgbaImage> {
    if data.len() < 148 || &data[..4] != b"BLP2" {
        return None;
    }
    let (compression, alpha_depth, alpha_type) = (data[8], data[9], data[10]);
    let width = u32_at(data, 12)?;
    let height = u32_at(data, 16)?;
    let offset = u32_at(data, 20)? as usize;
    let size = u32_at(data, 84)? as usize;
    let mip = data.get(offset..offset.checked_add(size)?.min(data.len()))?;
    let rgba = match compression {
        2 => {
            let kind = if alpha_depth <= 1 { 1 } else if alpha_type == 1 { 3 } else { 5 };
            decode_dxt(mip, width as usize, height as usize, kind, alpha_depth > 0)?
        }
        3 => {
            let n = (width * height) as usize;
            let src = mip.get(..n * 4)?;
            let mut out = vec![0u8; n * 4];
            for (o, s) in out.chunks_exact_mut(4).zip(src.chunks_exact(4)) {
                o.copy_from_slice(&[s[2], s[1], s[0], s[3]]);
            }
            out
        }
        1 => return decode_palette(data, mip, width, height, alpha_depth),
        _ => return None,
    };
    RgbaImage::from_raw(width, height, rgba)
}

fn decode_palette(data: &[u8], mip: &[u8], width: u32, height: u32, alpha_depth: u8) -> Option<RgbaImage> {
    let palette = data.get(148..148 + 1024)?;
    let n = (width * height) as usize;
    let indices = mip.get(..n)?;
    let alphas = &mip[n.min(mip.len())..];
    let mut out = vec![0u8; n * 4];
    for i in 0..n {
        let p = indices[i] as usize * 4;
        let a = match alpha_depth {
            0 => 255,
            1 => if alphas.get(i / 8).is_some_and(|b| b >> (i % 8) & 1 != 0) { 255 } else { 0 },
            4 => alphas.get(i / 2).map(|b| ((b >> ((i % 2) * 4)) & 15) * 17).unwrap_or(255),
            _ => alphas.get(i).copied().unwrap_or(255),
        };
        out[i * 4..i * 4 + 4].copy_from_slice(&[palette[p + 2], palette[p + 1], palette[p], a]);
    }
    RgbaImage::from_raw(width, height, out)
}

fn rgb565(c: u16) -> [i32; 3] {
    let (r, g, b) = ((c >> 11) & 31, (c >> 5) & 63, c & 31);
    [((r << 3) | (r >> 2)) as i32, ((g << 2) | (g >> 4)) as i32, ((b << 3) | (b >> 2)) as i32]
}

/// DXT1/3/5 blocks -> RGBA bytes. `alpha1`: DXT1's fourth colour is transparent.
pub fn decode_dxt(data: &[u8], width: usize, height: usize, kind: u8, alpha1: bool) -> Option<Vec<u8>> {
    let bw = ((width + 3) / 4).max(1);
    let bh = ((height + 3) / 4).max(1);
    let block = if kind == 1 { 8 } else { 16 };
    if data.len() < bw * bh * block {
        return None;
    }
    let mut out = vec![0u8; width * height * 4];
    for by in 0..bh {
        for bx in 0..bw {
            let b = &data[(by * bw + bx) * block..(by * bw + bx + 1) * block];
            let color = &b[block - 8..];
            let c0 = u16::from_le_bytes([color[0], color[1]]);
            let c1 = u16::from_le_bytes([color[2], color[3]]);
            let bits = u32::from_le_bytes([color[4], color[5], color[6], color[7]]);
            let (p0, p1) = (rgb565(c0), rgb565(c1));
            let four = kind != 1 || c0 > c1;
            let mut palette = [p0, p1, [0; 3], [0; 3]];
            for c in 0..3 {
                if four {
                    palette[2][c] = (2 * p0[c] + p1[c]) / 3;
                    palette[3][c] = (p0[c] + 2 * p1[c]) / 3;
                } else {
                    palette[2][c] = (p0[c] + p1[c]) / 2;
                    palette[3][c] = 0;
                }
            }
            let mut alpha = [255u8; 16];
            match kind {
                3 => {
                    for i in 0..16 {
                        alpha[i] = ((b[i / 2] >> ((i % 2) * 4)) & 15) * 17;
                    }
                }
                5 => {
                    let (a0, a1) = (b[0] as i32, b[1] as i32);
                    let mut table = [a0, a1, 0, 0, 0, 0, 0, 0];
                    if a0 > a1 {
                        for s in 1..7 {
                            table[s + 1] = ((7 - s as i32) * a0 + s as i32 * a1) / 7;
                        }
                    } else {
                        for s in 1..5 {
                            table[s + 1] = ((5 - s as i32) * a0 + s as i32 * a1) / 5;
                        }
                        table[6] = 0;
                        table[7] = 255;
                    }
                    let mut abits: u64 = 0;
                    for k in 0..6 {
                        abits |= (b[2 + k] as u64) << (8 * k);
                    }
                    for i in 0..16 {
                        alpha[i] = table[((abits >> (3 * i)) & 7) as usize] as u8;
                    }
                }
                _ => {}
            }
            for i in 0..16 {
                let (x, y) = (bx * 4 + i % 4, by * 4 + i / 4);
                if x >= width || y >= height {
                    continue;
                }
                let index = ((bits >> (2 * i)) & 3) as usize;
                let rgb = palette[index];
                let a = if kind == 1 { if !four && index == 3 && alpha1 { 0 } else { 255 } } else { alpha[i] };
                let o = (y * width + x) * 4;
                out[o..o + 4].copy_from_slice(&[rgb[0] as u8, rgb[1] as u8, rgb[2] as u8, a]);
            }
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn blp2(width: u32, height: u32, compression: u8, alpha_depth: u8, alpha_type: u8, data: &[u8]) -> Vec<u8> {
        let mut out = b"BLP2".to_vec();
        out.extend(1u32.to_le_bytes());
        out.extend([compression, alpha_depth, alpha_type, 0]);
        out.extend(width.to_le_bytes());
        out.extend(height.to_le_bytes());
        let offset = (out.len() + 128 + 1024) as u32;
        out.extend(offset.to_le_bytes());
        out.extend([0u8; 60]);
        out.extend((data.len() as u32).to_le_bytes());
        out.extend([0u8; 60]);
        out.extend(vec![0u8; 1024]);
        out.extend(data);
        out
    }

    #[test]
    fn raw_bgra() {
        let px = [10u8, 20, 30, 40, 50, 60, 70, 80];
        let img = open_blp(&blp2(2, 1, 3, 8, 8, &px)).unwrap();
        assert_eq!(img.get_pixel(0, 0).0, [30, 20, 10, 40]);
        assert_eq!(img.get_pixel(1, 0).0, [70, 60, 50, 80]);
    }

    #[test]
    fn dxt1_solid_and_transparent() {
        // c0 = white, c1 = black; c0 > c1: four colours, all index 0 -> white
        let block = [0xFF, 0xFF, 0x00, 0x00, 0, 0, 0, 0];
        let img = open_blp(&blp2(4, 4, 2, 0, 0, &block)).unwrap();
        assert_eq!(img.get_pixel(3, 3).0, [255, 255, 255, 255]);
        // c0 <= c1: three colours and transparent; index 3 everywhere
        let block = [0x00, 0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF];
        let img = open_blp(&blp2(4, 4, 2, 1, 0, &block)).unwrap();
        assert_eq!(img.get_pixel(0, 0).0[3], 0);
    }

    #[test]
    fn dxt5_alpha_table() {
        let mut block = vec![255u8, 0];
        block.extend([0u8; 6]); // every alpha index 0 -> a0 = 255
        block.extend([0xFF, 0xFF, 0x00, 0x00, 0, 0, 0, 0]);
        let img = open_blp(&blp2(4, 4, 2, 8, 7, &block)).unwrap();
        assert_eq!(img.get_pixel(1, 1).0, [255, 255, 255, 255]);
    }
}
