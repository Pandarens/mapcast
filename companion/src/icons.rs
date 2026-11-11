//! Pin icons for the phone: the textures map addons' pins use, cut and tinted as the game draws them.
//!
//! A pin's icon is a game texture (a file ID, read through CASC) or a file of an addon
//! (`Interface\AddOns\<addon>\...`, read from the game's AddOns folder). Each distinct icon
//! becomes a small PNG served at /icon/<key>.png.

use crate::imaging;
use crate::maps::{num, MapImages};
use crate::util::{child_ci, content_key, round_to, write_atomic};
use image::RgbaImage;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};

const ICON_MAX: u32 = 64; // px: pins are drawn at about 16 px, twice that on a dense screen
const EXTENSIONS: [&str; 5] = [".blp", ".tga", ".png", ".jpg", ".jpeg"];

pub struct PinIcons {
    cache_dir: PathBuf,
    maps: Arc<MapImages>,
    pub addons_dir: RwLock<Option<PathBuf>>,
    specs: Mutex<HashMap<String, Value>>,
    found: Mutex<HashMap<String, Option<PathBuf>>>,
    build: Mutex<()>,
}

impl PinIcons {
    pub fn new(cache_dir: &Path, maps: Arc<MapImages>) -> PinIcons {
        let cache_dir = cache_dir.join("icons");
        let _ = std::fs::create_dir_all(&cache_dir);
        PinIcons { cache_dir, maps, addons_dir: RwLock::new(None), specs: Mutex::new(HashMap::new()),
                   found: Mutex::new(HashMap::new()), build: Mutex::new(()) }
    }

    /// The pins event for the web page: each icon replaced by its URL (null: the page draws a dot).
    pub fn describe(&self, pins: &Value) -> Value {
        let urls: Vec<Value> = pins["icons"].as_array().into_iter().flatten().map(|icon| {
            let Some(source) = self.source(icon) else { return Value::Null };
            let coords: Vec<f64> = icon["coords"].as_array().into_iter().flatten().map(|c| round_to(num(c), 4)).collect();
            let spec = json!({"source": source, "coords": coords, "color": icon["color"]});
            let key = content_key(&spec);
            self.specs.lock().unwrap().insert(key.clone(), spec);
            json!(format!("/icon/{key}.png"))
        }).collect();
        json!({"sources": pins.get("sources").cloned().unwrap_or(json!([])), "icons": urls,
               "pins": pins.get("pins").cloned().unwrap_or(json!([]))})
    }

    fn source(&self, icon: &Value) -> Option<Value> {
        if let Some(id) = icon["fileID"].as_u64().filter(|&i| i > 0) {
            if self.maps.has_casc() {
                return Some(json!(["casc", id]));
            }
        }
        let path = self.find_addon_file(icon["path"].as_str()?)?;
        Some(json!(["file", path.to_string_lossy()]))
    }

    /// An addon's file on disk from its game path (any case, either slash, extension optional).
    pub fn find_addon_file(&self, game_path: &str) -> Option<PathBuf> {
        if let Some(hit) = self.found.lock().unwrap().get(game_path) {
            return hit.clone();
        }
        let found = self.lookup(game_path);
        self.found.lock().unwrap().insert(game_path.to_string(), found.clone());
        found
    }

    fn lookup(&self, game_path: &str) -> Option<PathBuf> {
        let parts: Vec<&str> = game_path.trim().split(['\\', '/']).filter(|p| !p.is_empty()).collect();
        if parts.len() < 3 || !parts[0].eq_ignore_ascii_case("interface") || !parts[1].eq_ignore_ascii_case("addons") {
            return None;
        }
        let mut folder = self.addons_dir.read().unwrap().clone()?;
        for part in &parts[2..parts.len() - 1] {
            folder = child_ci(&folder, part, true)?;
        }
        let name = parts[parts.len() - 1];
        let lower = name.to_lowercase();
        let names: Vec<String> = if EXTENSIONS.iter().any(|e| lower.ends_with(e)) {
            vec![name.to_string()]
        } else {
            EXTENSIONS.iter().map(|e| format!("{name}{e}")).collect()
        };
        names.iter().find_map(|n| child_ci(&folder, n, false))
    }

    /// Path of an icon PNG, building it on first use. Blocking.
    pub fn icon_path(&self, key: &str) -> Option<PathBuf> {
        let path = self.cache_dir.join(format!("{key}.png"));
        if path.exists() {
            return Some(path);
        }
        let spec = self.specs.lock().unwrap().get(key).cloned()?;
        let _guard = self.build.lock().unwrap();
        if !path.exists() {
            let image = self.build(&spec)?;
            write_atomic(&path, &imaging::png_bytes(&image)).ok()?;
        }
        Some(path)
    }

    fn build(&self, spec: &Value) -> Option<RgbaImage> {
        let source = &spec["source"];
        let image = match source[0].as_str()? {
            "casc" => self.maps.texture(source[1].as_u64()? as u32),
            _ => imaging::load_image(Path::new(source[1].as_str()?)),
        }?;
        let coords: Vec<f64> = spec["coords"].as_array()?.iter().map(num).collect();
        let color: Vec<u8> = spec["color"].as_array()?.iter().map(|c| num(c) as u8).collect();
        Some(cut_and_tint(&image, &coords, &color))
    }
}

/// The part of a texture its coordinates (left, right, top, bottom) select, times its vertex colour.
pub fn cut_and_tint(image: &RgbaImage, coords: &[f64], color: &[u8]) -> RgbaImage {
    let (l, r, t, b) = (coords[0], coords[1], coords[2], coords[3]);
    let (w, h) = (image.width() as f64, image.height() as f64);
    let bx = ((l.min(r) * w).round() as u32, (t.min(b) * h).round() as u32,
              (l.max(r) * w).round() as u32, (t.max(b) * h).round() as u32);
    let mut img = if bx.2 > bx.0 && bx.3 > bx.1 && (bx.0, bx.1, bx.2, bx.3) != (0, 0, image.width(), image.height()) {
        imaging::crop(image, bx.0, bx.1, bx.2 - bx.0, bx.3 - bx.1)
    } else {
        image.clone()
    };
    if l > r {
        img = image::imageops::flip_horizontal(&img);
    }
    if t > b {
        img = image::imageops::flip_vertical(&img);
    }
    if color.iter().any(|&c| c != 255) {
        for p in img.pixels_mut() {
            for c in 0..4 {
                p[c] = ((p[c] as f32) * (color[c] as f32) / 255.0).round() as u8;
            }
        }
    }
    if img.width().max(img.height()) > ICON_MAX {
        let k = ICON_MAX as f64 / img.width().max(img.height()) as f64;
        img = imaging::lanczos(&img, ((img.width() as f64 * k).round() as u32).max(1), ((img.height() as f64 * k).round() as u32).max(1));
    }
    img
}
