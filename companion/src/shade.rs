//! Unexplored land darkened on the seamless world, the way OpenMap's "dim" does it.
//!
//! OpenMap's data lists every zone's explored-area pieces (Data/<client>/Reveal.lua, by the zone
//! art's UiMapArtID); the addon says which of them you have explored. A zone you have not found
//! at all gets a shade over its land and the land round it that is no zone's (OpenMap's
//! MapZoneLand and MapZoneBorder masks), from every zoom; a zone you have been to gets a shade
//! over each piece you have not explored. Each shade is a dark image with an alpha mask.

use crate::imaging;
use crate::luadata::read_table;
use crate::maps::num;
use crate::openmap::OpenMapArt;
use crate::util::{content_key, write_atomic};
use image::{GrayImage, Luma, Rgba, RgbaImage};
use serde_json::{json, Value};
use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

const UNDISCOVERED: [u8; 3] = [13, 15, 20]; // OpenMap's shade colour (Stitch.lua UNDISCOVERED: 0.05, 0.06, 0.08)
const SHADE_ALPHA: f32 = 0.62; // ... and its strength
const REACH: f64 = 64.0; // world units round a zone not found that its shade covers (Stitch.lua Shade.REACH)
const SCALE: f64 = 0.5; // shade images at half the zone art's resolution: a soft mask needs no more
const FEATHER: f32 = 10.0; // px (in the zone art) a piece's shade fades over at its edges

/// artID -> its pieces: (x, y, w, h) in the zone art's pixels.
pub type Reveal = HashMap<u32, BTreeSet<(u32, u32, u32, u32)>>;

/// ns.MapReveal: [artID] = { ["w:h:x:y"] = "fileIDs" }.
pub fn parse_reveal(text: &str) -> Reveal {
    let mut out = Reveal::new();
    let Ok(Value::Object(m)) = read_table(text, "MapReveal") else { return out };
    for (art, pieces) in m {
        let (Ok(art), Some(pieces)) = (art.parse::<u32>(), pieces.as_object()) else { continue };
        let zone = out.entry(art).or_default();
        for key in pieces.keys() {
            let v: Vec<u32> = key.split(':').filter_map(|s| s.trim().parse().ok()).collect();
            if let [w, h, x, y] = v[..] {
                zone.insert((x, y, w, h));
            }
        }
    }
    out
}

/// The shades for the web page, from what the addon says you explored.
pub struct ZoneShades {
    cache_dir: PathBuf,
    specs: Mutex<HashMap<String, Value>>,
    build: Mutex<()>,
}

impl ZoneShades {
    pub fn new(cache_dir: &Path) -> ZoneShades {
        let cache_dir = cache_dir.join("shade");
        let _ = std::fs::create_dir_all(&cache_dir);
        ZoneShades { cache_dir, specs: Mutex::new(HashMap::new()), build: Mutex::new(()) }
    }

    /// {mapID: {url, rect, missing}}: a shade per zone with land you haven't explored.
    pub fn register(&self, explored: &Value, zones: &Value, world_size: Option<(f64, f64)>, om: Option<&Arc<OpenMapArt>>) -> Value {
        let (Some(om), Some((width, height)), Some(explored)) = (om, world_size, explored.as_object()) else { return json!({}) };
        let reveal = om.reveal();
        let mut out = serde_json::Map::new();
        for (id, zone) in explored {
            let Ok(map_id) = id.parse::<u32>() else { continue };
            let Some(pieces) = reveal.get(&(num(&zone["art"]) as u32)) else { continue }; // a city: no pieces
            let Some(rect) = zones[id.as_str()]["rect"].as_array() else { continue };
            let (lw, lh) = (num(&zone["layer"][0]), num(&zone["layer"][1]));
            if lw <= 0.0 || lh <= 0.0 || rect.len() < 4 {
                continue;
            }
            let have: BTreeSet<(u32, u32, u32, u32)> = zone["pieces"].as_array().into_iter().flatten().filter_map(|p| {
                Some((p[0].as_u64()? as u32, p[1].as_u64()? as u32, p[2].as_u64()? as u32, p[3].as_u64()? as u32))
            }).collect();
            let unexplored: Vec<_> = pieces.iter().filter(|p| !have.contains(p)).copied().collect();
            if unexplored.is_empty() {
                continue;
            }
            let explored_known: Vec<_> = pieces.iter().filter(|p| have.contains(p)).copied().collect();
            let missing = explored_known.is_empty();
            let [l, r, t, b] = [num(&rect[0]), num(&rect[1]), num(&rect[2]), num(&rect[3])];
            let mut spec = json!({"map": map_id, "layer": [lw, lh], "missing": missing,
                                  "pieces": unexplored, "explored": explored_known});
            let rect_out = if missing {
                // its rect grown by REACH world units each side, in its art's pixels
                spec["reach"] = json!([REACH * lw / ((r - l) * width).max(1e-9), REACH * lh / ((b - t) * height).max(1e-9)]);
                spec["land"] = json!(om.zone_mask("MapZoneLand", map_id).map(|p| p.to_string_lossy().into_owned()));
                spec["border"] = json!(om.zone_mask("MapZoneBorder", map_id).map(|p| p.to_string_lossy().into_owned()));
                let (du, dv) = (REACH / width, REACH / height);
                [l - du, r + du, t - dv, b + dv]
            } else {
                [l, r, t, b]
            };
            let key = content_key(&spec);
            self.specs.lock().unwrap().insert(key.clone(), spec);
            out.insert(map_id.to_string(), json!({"url": format!("/world/shade/{key}.png"), "rect": rect_out, "missing": missing}));
        }
        Value::Object(out)
    }

    pub fn shade_path(&self, key: &str) -> Option<PathBuf> {
        let path = self.cache_dir.join(format!("{key}.png"));
        if path.exists() {
            return Some(path);
        }
        let spec = self.specs.lock().unwrap().get(key).cloned()?;
        let _guard = self.build.lock().unwrap();
        if !path.exists() {
            let image = if spec["missing"].as_bool().unwrap_or(false) { missing_shade(&spec) } else { pieces_shade(&spec) };
            write_atomic(&path, &imaging::png_bytes(&image)).ok()?;
        }
        Some(path)
    }
}

/// The shade colour over an alpha mask (0..1).
fn shade(alpha: &[f32], w: u32, h: u32) -> RgbaImage {
    let mut img = RgbaImage::new(w, h);
    for (p, a) in img.pixels_mut().zip(alpha) {
        *p = Rgba([UNDISCOVERED[0], UNDISCOVERED[1], UNDISCOVERED[2], (a * SHADE_ALPHA * 255.0 + 0.5).clamp(0.0, 255.0) as u8]);
    }
    img
}

fn mask(path: Option<&str>, w: u32, h: u32) -> Option<Vec<f32>> {
    let img = imaging::load_image(Path::new(path?))?;
    let resized = imaging::bilinear(&img, w, h);
    Some(resized.pixels().map(|p| p[3] as f32 / 255.0).collect())
}

/// A zone not found at all: its land, and its share of the land round it, shaded.
fn missing_shade(spec: &Value) -> RgbaImage {
    let (lw, lh) = (num(&spec["layer"][0]), num(&spec["layer"][1]));
    let (rx, ry) = (num(&spec["reach"][0]), num(&spec["reach"][1]));
    let (w, h) = (((lw + 2.0 * rx) * SCALE).round().max(1.0) as u32, ((lh + 2.0 * ry) * SCALE).round().max(1.0) as u32);
    let mut alpha = vec![0f32; (w * h) as usize];
    let (x0, y0) = ((rx * SCALE).round() as u32, (ry * SCALE).round() as u32);
    let x1 = (x0 + (lw * SCALE).round().max(1.0) as u32).min(w);
    let y1 = (y0 + (lh * SCALE).round().max(1.0) as u32).min(h);
    let (zw, zh) = (x1.saturating_sub(x0).max(1), y1.saturating_sub(y0).max(1));
    let land = mask(spec["land"].as_str(), zw, zh);
    for y in y0..y1 {
        for x in x0..x1 {
            alpha[(y * w + x) as usize] = land.as_ref().map(|m| m[((y - y0) * zw + (x - x0)) as usize]).unwrap_or(1.0);
        }
    }
    if let Some(border) = mask(spec["border"].as_str(), w, h) {
        for (a, b) in alpha.iter_mut().zip(border) {
            *a = a.max(b);
        }
    }
    shade(&alpha, w, h)
}

/// A zone you have been to: its unexplored pieces shaded, softly, never over an explored one.
fn pieces_shade(spec: &Value) -> RgbaImage {
    let (lw, lh) = (num(&spec["layer"][0]), num(&spec["layer"][1]));
    let (w, h) = ((lw * SCALE).round().max(1.0) as u32, (lh * SCALE).round().max(1.0) as u32);
    let paint = |pieces: &Value| -> Vec<f32> {
        let mut m = GrayImage::new(w, h);
        for p in pieces.as_array().into_iter().flatten() {
            let (x, y, pw, ph) = (num(&p[0]), num(&p[1]), num(&p[2]), num(&p[3]));
            let (x0, y0) = ((x * SCALE).round() as u32, (y * SCALE).round() as u32);
            let (x1, y1) = ((((x + pw) * SCALE).round() as u32).min(w), (((y + ph) * SCALE).round() as u32).min(h));
            for yy in y0..y1 {
                for xx in x0..x1 {
                    m.put_pixel(xx, yy, Luma([255]));
                }
            }
        }
        let blurred = image::imageops::blur(&m, FEATHER * SCALE as f32 / 2.0);
        blurred.pixels().map(|p| p[0] as f32 / 255.0).collect()
    };
    let mut alpha = paint(&spec["pieces"]);
    if spec["explored"].as_array().is_some_and(|e| !e.is_empty()) {
        for (a, e) in alpha.iter_mut().zip(paint(&spec["explored"])) {
            *a *= 1.0 - e;
        }
    }
    shade(&alpha, w, h)
}
