//! The seamless world map, drawn the way OpenMap draws it.
//!
//! OpenMap (https://github.com/openmap-world/openmap) stitches every zone onto Azeroth's map
//! (uiMap 947). Its addon folder holds the zones' art already baked onto that world in 512 px
//! tiles (Media/Textures/<client>/MapZones, laid out by Data/<client>/ZoneArt.lua) and
//! hand-cleaned art for the world and continent maps (Media/Textures/<client>/MapArt, listed in
//! Data/<client>/ArtEdits.lua). This module reads those files and serves them to the phone: the
//! world and continent art as single images, the zone tiles as a pyramid of zoom levels.

use crate::imaging::{self, PARCHMENT};
use crate::luadata::{field, positional, read_table};
use crate::maps::{num, MapImages};
use crate::places::{build_places, YardsToWorld};
use crate::shade::{parse_reveal, Reveal};
use crate::util::{content_key, hex_key, write_atomic};
use image::{Rgba, RgbaImage};
use rayon::prelude::*;
use serde_json::{json, Value};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, RwLock};

const CLIENT: &str = "Forever";
pub const SEA_COLOR: [u8; 3] = [128, 125, 105]; // OpenMap's sea where no art paints one (Stitch.lua SEA)
pub const SEA_REACH: f64 = 1.5; // how far past the world art its outer sea runs, in world widths
const FEATHER_EDGE: f64 = 0.08; // share of each side faded out when OpenMap's feather mask is not at hand
const TEXTURE_EXTS: [&str; 3] = ["tga", "blp", "png"];

/// OpenMap's addon folder: the one given, or the one in the game's AddOns.
pub fn find_openmap(wow_root: Option<&Path>, explicit: Option<&str>) -> Option<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(e) = explicit {
        candidates.push(PathBuf::from(e));
        candidates.push(PathBuf::from(e).join("OpenMap"));
    }
    if let Some(root) = wow_root {
        candidates.push(root.join(crate::util::FLAVOR_DIR).join("Interface").join("AddOns").join("OpenMap"));
    }
    candidates.into_iter().find(|p| p.join("Data").join(CLIENT).join("ZoneArt.lua").exists())
        .map(|p| std::path::absolute(&p).unwrap_or(p))
}

fn read_text(path: &Path) -> Option<String> {
    std::fs::read(path).ok().map(|b| String::from_utf8_lossy(&b).into_owned())
}

pub struct ZoneArt {
    pub px: f64,
    pub size: u32,
    pub step: u32,
    pub tiles: Vec<(u32, u32)>,
}

/// ns.MapZoneArt: px per world unit, tile size and step, and the tiles (i * 256 + j).
pub fn parse_zone_art(text: &str) -> Result<ZoneArt, String> {
    let t = read_table(text, "MapZoneArt")?;
    let n = |k: &str| field(&t, k).and_then(Value::as_f64).ok_or(format!("ZoneArt.lua has no {k}"));
    let keys: BTreeSet<(u32, u32)> = positional(field(&t, "tiles").ok_or("ZoneArt.lua has no tiles")?)
        .iter()
        .filter_map(Value::as_u64)
        .map(|k| ((k / 256) as u32, (k % 256) as u32))
        .collect();
    Ok(ZoneArt { px: n("px")?, size: n("size")? as u32, step: n("step")? as u32, tiles: keys.into_iter().collect() })
}

/// ns.MapArtEdits: [uiMapID] = { tile, ... } (row-major, from 1).
pub fn parse_art_edits(text: &str) -> HashMap<u32, HashSet<u32>> {
    let mut out = HashMap::new();
    if let Ok(Value::Object(m)) = read_table(text, "MapArtEdits") {
        for (k, v) in m {
            if let Ok(id) = k.parse::<u32>() {
                out.insert(id, positional(&v).iter().filter_map(Value::as_u64).map(|n| n as u32).collect());
            }
        }
    }
    out
}

/// The continents' names on the world map (ns.MapLabels entries with map = 947), in art pixels.
pub fn parse_world_labels(text: &str) -> Vec<Value> {
    let Ok(table) = read_table(text, "MapLabels") else { return vec![] };
    let mut labels = Vec::new();
    for entry in positional(&table) {
        if field(&entry, "map").and_then(Value::as_i64) != Some(947) {
            continue;
        }
        let Some(name) = field(&entry, "name") else { continue };
        let (Some(map_id), Some(text)) = (field(name, "map").and_then(Value::as_i64), field(name, "text").and_then(Value::as_str)) else { continue };
        let lines: Vec<Vec<f64>> = positional(field(&entry, "lines").unwrap_or(&Value::Null))
            .iter()
            .map(|l| positional(l).iter().filter_map(Value::as_f64).collect::<Vec<f64>>())
            .filter(|l| l.len() >= 4)
            .collect();
        if lines.is_empty() {
            continue;
        }
        let left = lines.iter().map(|l| l[0]).fold(f64::INFINITY, f64::min);
        let right = lines.iter().map(|l| l[1]).fold(f64::NEG_INFINITY, f64::max);
        let top = lines.iter().map(|l| l[2] - l[3]).fold(f64::INFINITY, f64::min);
        let bottom = lines.iter().map(|l| l[2]).fold(f64::NEG_INFINITY, f64::max);
        let cap = lines.iter().map(|l| l[3]).sum::<f64>() / lines.len() as f64;
        labels.push(json!({"mapID": map_id, "text": text, "x": (left + right) / 2.0, "y": (top + bottom) / 2.0, "size": cap}));
    }
    labels
}

fn toc_version(dir: &Path) -> String {
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let name = entry.file_name().to_string_lossy().to_lowercase();
        if name.ends_with(".toc") {
            if let Some(text) = read_text(&entry.path()) {
                for line in text.lines() {
                    if let Some(rest) = line.strip_prefix("##") {
                        if let Some(v) = rest.trim_start().strip_prefix("Version:") {
                            return v.trim().to_string();
                        }
                    }
                }
            }
        }
    }
    String::new()
}

/// OpenMap's files on disk and the zone tile pyramid built from them.
pub struct OpenMapArt {
    data_dir: PathBuf,
    tex: PathBuf,
    pub zone_art: ZoneArt,
    pub art_edits: HashMap<u32, HashSet<u32>>,
    pub labels: Vec<Value>,
    pub version: String,
    pub feather: Option<PathBuf>,
    pub sea: Option<PathBuf>,
    pub stamp: String,
    pub cache_dir: PathBuf,
    pub levels: Vec<HashSet<(u32, u32)>>,
    locks: Mutex<HashMap<(usize, u32, u32), Arc<Mutex<()>>>>,
    places: OnceLock<Option<(Value, Value)>>,
    reveal: OnceLock<Reveal>,
}

impl OpenMapArt {
    pub fn open(addon_dir: &Path, cache_dir: &Path) -> Result<OpenMapArt, String> {
        let data_dir = addon_dir.join("Data").join(CLIENT);
        let tex = addon_dir.join("Media").join("Textures").join(CLIENT);
        let zone_art_path = data_dir.join("ZoneArt.lua");
        let zone_art = parse_zone_art(&read_text(&zone_art_path).ok_or("no ZoneArt.lua")?)?;
        let art_edits = read_text(&data_dir.join("ArtEdits.lua")).map(|t| parse_art_edits(&t)).unwrap_or_default();
        let labels = read_text(&data_dir.join("Labels.lua")).map(|t| parse_world_labels(&t)).unwrap_or_default();
        let version = toc_version(addon_dir);
        let meta = std::fs::metadata(&zone_art_path).map_err(|e| e.to_string())?;
        let mtime = meta.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_secs()).unwrap_or(0);
        let stamp = hex_key(&format!("{version}|{}|{mtime}|webp", meta.len()), 12);
        let cache_dir = cache_dir.join("openmap").join(&stamp);
        let _ = std::fs::create_dir_all(&cache_dir);
        let mut levels = vec![zone_art.tiles.iter().copied().collect::<HashSet<_>>()];
        while levels.last().unwrap().len() > 1 {
            let next = levels.last().unwrap().iter().map(|&(i, j)| (i >> 1, j >> 1)).collect();
            levels.push(next);
        }
        let mut art = OpenMapArt {
            data_dir, tex, zone_art, art_edits, labels, version, feather: None, sea: None,
            stamp, cache_dir, levels, locks: Mutex::new(HashMap::new()), places: OnceLock::new(), reveal: OnceLock::new(),
        };
        art.feather = art.texture_path("MapFeatherWide");
        art.sea = art.texture_path("MapSeaFieldOut");
        Ok(art)
    }

    fn texture_path(&self, name: &str) -> Option<PathBuf> {
        TEXTURE_EXTS.iter().map(|e| self.tex.join(format!("{name}.{e}"))).find(|p| p.exists())
    }

    /// One of OpenMap's per-zone masks (MapZoneLand: its land; MapZoneBorder: the land round it).
    pub fn zone_mask(&self, folder: &str, map_id: u32) -> Option<PathBuf> {
        TEXTURE_EXTS.iter().map(|e| self.tex.join(folder).join(format!("{map_id}.{e}"))).find(|p| p.exists())
    }

    /// Pixels of a tile that belong to it (the rest of the file repeats its neighbours' edge).
    pub fn tile_px(&self) -> u32 {
        self.zone_art.step
    }

    pub fn describe(&self) -> Value {
        let za = &self.zone_art;
        json!({"version": self.version, "unit": za.step as f64 / za.px, "tilePx": self.tile_px(),
               "maxLevel": self.levels.len() - 1, "tiles": za.tiles.iter().map(|(i, j)| i * 256 + j).collect::<Vec<_>>(),
               "url": format!("/world/zone/{{level}}/{{i}}_{{j}}?v={}", self.stamp)})
    }

    /// OpenMap's hand-cleaned copy of an art tile (row-major, from 1), if it has one.
    pub fn edited_art(&self, map_id: u32, number: u32) -> Option<PathBuf> {
        if !self.art_edits.get(&map_id).is_some_and(|s| s.contains(&number)) {
            return None;
        }
        TEXTURE_EXTS.iter().map(|e| self.tex.join("MapArt").join(format!("{map_id}_{number}.{e}"))).find(|p| p.exists())
    }

    /// OpenMap's flight network and its places (dungeon entrances, docks, sailings), read once.
    pub fn places_data(&self) -> Option<(Value, Value)> {
        self.places
            .get_or_init(|| {
                let read = |file: &str, table: &str| -> Value {
                    let Some(text) = read_text(&self.data_dir.join(file)) else { return Value::Null };
                    read_table(&text, table).unwrap_or_else(|e| {
                        log::warn!("OpenMap {file}: {e}");
                        Value::Null
                    })
                };
                let (f, p) = (read("FlightData.lua", "MapFlights"), read("Places.lua", "MapPlaces"));
                if f.is_null() && p.is_null() { None } else { Some((f, p)) }
            })
            .clone()
    }

    /// Every zone art's explored-area pieces (Data/<client>/Reveal.lua), read once.
    pub fn reveal(&self) -> &Reveal {
        self.reveal.get_or_init(|| {
            read_text(&self.data_dir.join("Reveal.lua")).map(|t| parse_reveal(&t)).unwrap_or_default()
        })
    }

    pub fn has_tile(&self, level: usize, i: u32, j: u32) -> bool {
        self.levels.get(level).is_some_and(|s| s.contains(&(i, j)))
    }

    /// Path of a pyramid tile, building it (and what it needs) on first use. Blocking.
    pub fn zone_tile(&self, level: usize, i: u32, j: u32) -> Option<PathBuf> {
        if !self.has_tile(level, i, j) {
            return None;
        }
        let path = self.cache_dir.join(format!("z{level}")).join(format!("{i}_{j}.webp"));
        if path.exists() {
            return Some(path);
        }
        let lock = self.locks.lock().unwrap().entry((level, i, j)).or_default().clone();
        let _guard = lock.lock().unwrap();
        if !path.exists() {
            let image = self.build_tile(level, i, j)?;
            write_atomic(&path, &imaging::webp_bytes(&image, 86.0)).ok()?;
        }
        Some(path)
    }

    /// Build every tile ahead of time, finest level first (each level reuses the one below).
    pub fn warm(&self) {
        for (level, tiles) in self.levels.iter().enumerate() {
            let mut keys: Vec<&(u32, u32)> = tiles.iter().collect();
            keys.sort();
            keys.par_iter().for_each(|&&(i, j)| {
                if self.zone_tile(level, i, j).is_none() {
                    log::warn!("OpenMap tile {level}/{i}_{j} could not be built");
                }
            });
        }
    }

    fn build_tile(&self, level: usize, i: u32, j: u32) -> Option<RgbaImage> {
        let px = self.tile_px();
        if level == 0 {
            let size = self.zone_art.size;
            let inset = (size - px) / 2;
            let file = self.tex.join("MapZones").join(format!("{i}_{j}.blp"));
            let mut tile = imaging::load_image(&file)?;
            if tile.dimensions() != (size, size) {
                tile = imaging::lanczos(&tile, size, size);
            }
            return Some(imaging::crop(&tile, inset, inset, px, px));
        }
        let half = px / 2;
        let mut out = RgbaImage::new(half * 2, half * 2);
        for dx in 0..2 {
            for dy in 0..2 {
                let Some(child) = self.zone_tile(level - 1, i * 2 + dx, j * 2 + dy) else { continue };
                let Some(img) = imaging::load_image(&child) else { continue };
                let small = if img.dimensions() == (half * 2, half * 2) { imaging::box_half(&img) } else { imaging::resize(&img, half, half, image::imageops::FilterType::Triangle) };
                imaging::paste(&mut out, &small, (dx * half) as i64, (dy * half) as i64);
            }
        }
        Some(if out.dimensions() == (px, px) { out } else { imaging::lanczos(&out, px, px) })
    }
}

/// The seamless world for the web page: art images plus OpenMap's zone tiles when present.
pub struct WorldImages {
    cache_dir: PathBuf,
    pub maps: Arc<MapImages>,
    pub openmap: RwLock<Option<Arc<OpenMapArt>>>,
    specs: Mutex<HashMap<String, Value>>,
    build: Mutex<()>,
    places: Mutex<Option<(String, Vec<u8>)>>,
}

impl WorldImages {
    pub fn new(cache_dir: &Path, maps: Arc<MapImages>) -> WorldImages {
        let cache_dir = cache_dir.join("world");
        let _ = std::fs::create_dir_all(&cache_dir);
        WorldImages { cache_dir, maps, openmap: RwLock::new(None), specs: Mutex::new(HashMap::new()),
                      build: Mutex::new(()), places: Mutex::new(None) }
    }

    pub fn openmap(&self) -> Option<Arc<OpenMapArt>> {
        self.openmap.read().unwrap().clone()
    }

    /// The web description of the world; images are built on first request.
    pub fn register(&self, world: &Value) -> Value {
        let maps: Vec<&Value> = world["maps"].as_array().map(|a| a.iter()
            .filter(|m| !m["rect"].is_null() && m["layers"].as_array().is_some_and(|l| !l.is_empty())).collect()).unwrap_or_default();
        let world_id = &world["worldMapID"];
        let Some(base) = maps.iter().find(|m| &m["mapID"] == world_id).copied() else { return Value::Null };
        let layer = &base["layers"][0];
        let om = self.openmap();
        let feather = om.as_ref().and_then(|o| o.feather.clone()).map(|p| p.to_string_lossy().into_owned());
        let mut ordered = maps.clone();
        ordered.sort_by_key(|m| &m["mapID"] != world_id);
        let mut art = Vec::new();
        for info in ordered {
            let spec = json!({"info": info, "edited": self.edited_tiles(om.as_deref(), info), "casc": self.maps.has_casc(),
                              "feather": feather});
            let key = content_key(&spec);
            self.specs.lock().unwrap().insert(key.clone(), spec);
            art.push(json!({"mapID": info["mapID"], "name": info["name"], "rect": info["rect"],
                            "kind": if std::ptr::eq(info, base) { "world" } else { "continent" },
                            "url": format!("/world/art/{key}.png")}));
        }
        let names: HashMap<String, Value> = maps.iter().map(|m| (m["mapID"].to_string(), m["name"].clone())).collect();
        let labels: Vec<Value> = om.as_ref().map(|o| o.labels.iter().map(|l| {
            let mut l = l.clone();
            if let Some(n) = names.get(&l["mapID"].to_string()).filter(|n| n.as_str().is_some_and(|s| !s.is_empty())) {
                l["text"] = n.clone();
            }
            l
        }).collect()).unwrap_or_default();
        let sea_url = om.as_ref().filter(|o| o.sea.is_some()).map(|o| json!(format!("/world/sea.png?v={}", o.stamp))).unwrap_or(Value::Null);
        json!({"worldMapID": base["mapID"], "width": layer["w"], "height": layer["h"], "art": art,
               "sea": {"color": SEA_COLOR, "reach": SEA_REACH, "url": sea_url},
               "labels": labels, "zones": om.as_ref().map(|o| o.describe()).unwrap_or(Value::Null),
               "places": self.register_places(world, &maps, om.as_deref()),
               "openmapInGame": world.get("openmap").cloned().unwrap_or(json!(false)),
               "openmapVersion": world.get("openmapVersion").cloned().unwrap_or(json!(""))})
    }

    /// OpenMap's flights, docks, sailings and dungeon entrances, placed by the continents' yards.
    fn register_places(&self, world: &Value, maps: &[&Value], om: Option<&OpenMapArt>) -> Value {
        let rects: HashMap<String, [f64; 4]> = maps.iter().filter_map(|m| {
            let r = m["rect"].as_array()?;
            Some((m["mapID"].to_string(), [num(&r[0]), num(&r[1]), num(&r[2]), num(&r[3])]))
        }).collect();
        let to_world = YardsToWorld::new(&world["yards"], &rects);
        let data = om.and_then(|o| o.places_data());
        let (Some((flights, places)), false) = (data, to_world.is_empty()) else {
            *self.places.lock().unwrap() = None;
            return Value::Null;
        };
        let blob = serde_json::to_vec(&build_places(&flights, &places, &to_world)).unwrap_or_default();
        let mut h = sha1_smol::Sha1::new();
        h.update(&blob);
        let key = h.digest().to_string()[..12].to_string();
        *self.places.lock().unwrap() = Some((key.clone(), blob));
        json!({"url": format!("/world/places.json?v={key}")})
    }

    pub fn places_json(&self) -> Option<Vec<u8>> {
        self.places.lock().unwrap().as_ref().map(|p| p.1.clone())
    }

    fn edited_tiles(&self, om: Option<&OpenMapArt>, info: &Value) -> Value {
        let Some(om) = om else { return json!({}) };
        let count = info["layers"][0]["tiles"].as_array().map(Vec::len).unwrap_or(0) as u32;
        let id = num(&info["mapID"]) as u32;
        let mut out = serde_json::Map::new();
        for n in 1..=count {
            if let Some(p) = om.edited_art(id, n) {
                out.insert(n.to_string(), json!(p.to_string_lossy()));
            }
        }
        Value::Object(out)
    }

    /// Path of a world or continent art PNG, composing it if needed. Blocking.
    pub fn art_path(&self, key: &str) -> Option<PathBuf> {
        let path = self.cache_dir.join(format!("art_{key}.png"));
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
        let info = &spec["info"];
        let layer = &info["layers"][0];
        let (w, h) = (num(&layer["w"]) as u32, num(&layer["h"]) as u32);
        let (tw, th) = (num(&layer["tileW"]).max(1.0) as u32, num(&layer["tileH"]).max(1.0) as u32);
        let tiles: Vec<u32> = layer["tiles"].as_array().map(|a| a.iter().map(|t| num(t) as u32).collect()).unwrap_or_default();
        let cols = (w as f64 / tw as f64).ceil().max(1.0) as u32;
        let rows = (tiles.len() as f64 / cols as f64).ceil().max(1.0) as u32;
        let mut canvas = RgbaImage::from_pixel(cols * tw, rows * th, Rgba(PARCHMENT));
        let mut drawn = 0;
        for (index, file_id) in tiles.iter().enumerate() {
            let edited = spec["edited"].get((index + 1).to_string()).and_then(Value::as_str);
            let tile = match edited {
                Some(p) => imaging::load_image(Path::new(p)),
                None => self.maps.texture(*file_id),
            };
            if let Some(mut tile) = tile {
                if tile.dimensions() != (tw, th) {
                    tile = imaging::lanczos(&tile, tw, th);
                }
                imaging::paste(&mut canvas, &tile, ((index as u32 % cols) * tw) as i64, ((index as u32 / cols) * th) as i64);
                drawn += 1;
            }
        }
        if drawn == 0 {
            return imaging::placeholder(w, h);
        }
        let mut image = imaging::crop(&canvas, 0, 0, w, h);
        // the torn paper edge fades out, as OpenMap draws the world and continent art
        let mask = feather_mask(image.width(), image.height(), spec["feather"].as_str().map(Path::new));
        for (p, m) in image.pixels_mut().zip(mask.iter()) {
            p[3] = ((p[3] as f32) * (*m as f32) / 255.0) as u8;
        }
        image
    }

    /// OpenMap's outer sea (the world's sea colour carried past its edges) as PNG.
    pub fn sea_path(&self) -> Option<PathBuf> {
        let om = self.openmap()?;
        let sea = om.sea.as_ref()?;
        let path = om.cache_dir.join("sea.png");
        let _guard = self.build.lock().unwrap();
        if !path.exists() {
            let img = imaging::load_image(sea)?;
            let rgb = image::DynamicImage::ImageRgba8(img).to_rgb8();
            let mut out = std::io::Cursor::new(Vec::new());
            rgb.write_to(&mut out, image::ImageFormat::Png).ok()?;
            write_atomic(&path, &out.into_inner()).ok()?;
        }
        Some(path)
    }
}

/// An alpha mask fading a map's edges: OpenMap's own when given, else a smooth ramp.
pub fn feather_mask(w: u32, h: u32, path: Option<&Path>) -> Vec<u8> {
    if let Some(img) = path.and_then(imaging::load_image) {
        let a = imaging::bilinear(&img, w, h);
        return a.pixels().map(|p| p[3]).collect();
    }
    let ramp = |n: u32| -> Vec<f64> {
        let edge = (n as f64 * FEATHER_EDGE).max(1.0);
        (0..n).map(|i| {
            let d = (i as f64 + 0.5).min(n as f64 - i as f64 - 0.5) / edge;
            let t = d.clamp(0.0, 1.0);
            t * t * (3.0 - 2.0 * t)
        }).collect()
    };
    let (rx, ry) = (ramp(w), ramp(h));
    let mut out = Vec::with_capacity((w * h) as usize);
    for y in 0..h as usize {
        for x in 0..w as usize {
            out.push((rx[x] * ry[y] * 255.0) as u8);
        }
    }
    out
}
