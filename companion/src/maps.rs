//! Builds map background images from game textures read out of CASC.

use crate::casc::LocalCasc;

/// Where game files come from: the client's CASC storage (or a fake one in the tests).
pub trait GameFiles: Send + Sync {
    fn read_file(&self, file_data_id: u32) -> Result<Vec<u8>, String>;
}

impl GameFiles for LocalCasc {
    fn read_file(&self, file_data_id: u32) -> Result<Vec<u8>, String> {
        LocalCasc::read_file(self, file_data_id)
    }
}
use crate::imaging::{self, PARCHMENT};
use crate::util::{content_key, write_atomic};
use image::{Rgba, RgbaImage};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};

const EXPLORED_TILE: u32 = 256;

/// Composes and caches map PNGs. Works without CASC by drawing a grid placeholder.
pub struct MapImages {
    pub cache_dir: PathBuf,
    pub casc: RwLock<Option<Arc<dyn GameFiles>>>,
    specs: Mutex<HashMap<String, Value>>,
    build: Mutex<()>,
}

impl MapImages {
    pub fn new(cache_dir: &Path) -> MapImages {
        let _ = std::fs::create_dir_all(cache_dir);
        MapImages { cache_dir: cache_dir.to_path_buf(), casc: RwLock::new(None), specs: Mutex::new(HashMap::new()),
                    build: Mutex::new(()) }
    }

    pub fn has_casc(&self) -> bool {
        self.casc.read().unwrap().is_some()
    }

    /// The web description of a map; the image is built on first request.
    pub fn register(&self, map_event: &Value) -> Value {
        let info = &map_event["info"];
        let layer = info["layers"].get(0).cloned()
            .unwrap_or(json!({"w": 1002, "h": 668, "tileW": 256, "tileH": 256, "tiles": []}));
        let placeholder = !self.has_casc() || layer["tiles"].as_array().is_none_or(|t| t.is_empty());
        let spec = json!({"info": info, "layer": layer, "explored": map_event.get("explored").cloned().unwrap_or(json!([])),
                          "placeholder": placeholder});
        let key = content_key(&spec);
        self.specs.lock().unwrap().insert(key.clone(), spec);
        json!({"mapID": info["mapID"], "name": info["name"], "mapType": info["mapType"],
               "parentMapID": info["parentMapID"], "width": layer["w"], "height": layer["h"],
               "url": format!("/map/{key}.png"), "placeholder": placeholder,
               "rect": map_event.get("rect").cloned().unwrap_or(Value::Null)})
    }

    /// Path of the PNG for a registered key, composing it if needed (blocking).
    pub fn image_path(&self, key: &str) -> Option<PathBuf> {
        let path = self.cache_dir.join(format!("map_{key}.png"));
        if path.exists() {
            return Some(path);
        }
        let spec = self.specs.lock().unwrap().get(key).cloned()?;
        let _guard = self.build.lock().unwrap();
        if !path.exists() {
            let image = self.compose(&spec);
            write_atomic(&path, &imaging::png_bytes(&image)).ok()?;
        }
        Some(path)
    }

    fn compose(&self, spec: &Value) -> RgbaImage {
        let layer = &spec["layer"];
        let (w, h) = (num(&layer["w"]) as u32, num(&layer["h"]) as u32);
        if spec["placeholder"].as_bool().unwrap_or(true) {
            return imaging::placeholder(w, h);
        }
        let (tw, th) = (num(&layer["tileW"]).max(1.0) as u32, num(&layer["tileH"]).max(1.0) as u32);
        let tiles: Vec<u32> = layer["tiles"].as_array().map(|a| a.iter().map(|t| num(t) as u32).collect()).unwrap_or_default();
        let cols = (w as f64 / tw as f64).ceil().max(1.0) as u32;
        let rows = ((tiles.len() as f64) / cols as f64).ceil().max(1.0) as u32;
        let mut canvas = RgbaImage::from_pixel(cols * tw, rows * th, Rgba(PARCHMENT));
        for (i, file_id) in tiles.iter().enumerate() {
            if let Some(tile) = self.texture(*file_id) {
                imaging::paste(&mut canvas, &tile, ((i as u32 % cols) * tw) as i64, ((i as u32 / cols) * th) as i64);
            }
        }
        for overlay in spec["explored"].as_array().cloned().unwrap_or_default() {
            self.draw_explored(&mut canvas, &overlay);
        }
        imaging::crop(&canvas, 0, 0, w, h)
    }

    /// Same tiling as Blizzard's MapExplorationPinMixin: 256 px pieces, last row/column cropped.
    fn draw_explored(&self, canvas: &mut RgbaImage, overlay: &Value) {
        let (w, h) = (num(&overlay["w"]) as u32, num(&overlay["h"]) as u32);
        let (x, y) = (num(&overlay["x"]) as i64, num(&overlay["y"]) as i64);
        let files: Vec<u32> = overlay["files"].as_array().map(|a| a.iter().map(|f| num(f) as u32).collect()).unwrap_or_default();
        let wide = w.div_ceil(EXPLORED_TILE);
        let tall = h.div_ceil(EXPLORED_TILE);
        for j in 0..tall {
            let piece_h = if j < tall - 1 { EXPLORED_TILE } else { match h % EXPLORED_TILE { 0 => EXPLORED_TILE, r => r } };
            for k in 0..wide {
                let piece_w = if k < wide - 1 { EXPLORED_TILE } else { match w % EXPLORED_TILE { 0 => EXPLORED_TILE, r => r } };
                let Some(file) = files.get((j * wide + k) as usize) else { continue };
                let Some(tile) = self.texture(*file) else { continue };
                let piece = imaging::crop(&tile, 0, 0, piece_w, piece_h);
                imaging::alpha_composite(canvas, &piece, x + (k * EXPLORED_TILE) as i64, y + (j * EXPLORED_TILE) as i64);
            }
        }
    }

    /// A game texture as RGBA, cached as PNG on disk.
    pub fn texture(&self, file_id: u32) -> Option<RgbaImage> {
        let casc = self.casc.read().unwrap().clone()?;
        if file_id == 0 {
            return None;
        }
        let path = self.cache_dir.join("tex").join(format!("{file_id}.png"));
        if path.exists() {
            if let Some(img) = imaging::load_image(&path) {
                return Some(img);
            }
        }
        let image = match casc.read_file(file_id) {
            Ok(data) if data.starts_with(b"BLP2") => crate::blp::open_blp(&data),
            Ok(data) => image::load_from_memory(&data).ok().map(|i| i.to_rgba8()),
            Err(e) => {
                log::warn!("texture {file_id}: {e}");
                None
            }
        }?;
        let _ = write_atomic(&path, &imaging::png_bytes(&image));
        Some(image)
    }
}

pub fn num(v: &Value) -> f64 {
    v.as_f64().unwrap_or(0.0)
}
