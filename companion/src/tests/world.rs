//! The seamless world: OpenMap's files, the zone tile pyramid, places, shades, pin icons, the web routes.

use super::casc::temp_dir;
use crate::hub::Hub;
use crate::icons::{cut_and_tint, PinIcons};
use crate::imaging::load_image;
use crate::maps::MapImages;
use crate::openmap::{find_openmap, parse_art_edits, parse_minimap, parse_world_labels, parse_zone_art, OpenMapArt, WorldImages};
use crate::places::{build_places, YardsToWorld};
use crate::shade::{parse_reveal, ZoneShades};
use crate::web::{router, App};
use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode};
use image::{Rgba, RgbaImage};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tower::ServiceExt;

const ZONE_ART: &str = r#"
local _, ns = ...
ns.MapZoneArt = {
	px = 16, size = 34, step = 32,   -- px per world unit; a tile's px
	desaturate = 0, colorMatch = 1,
	tiles = {   -- i * 256 + j: column i, row j
		0, 256, 1, 770,
	},
}
"#;

const ART_EDITS: &str = "ns.MapArtEdits = {\n\t[947] = { 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12 },\n\t[1414] = { 1, 2 },\n}\n";

const LABELS: &str = r#"
ns.MapLabels = {
	{ map = 947, name = { map = 1414, text = "Kalimdor" }, fade = "land", ink = LAND_INK, glow = LAND_GLOW, glowA = 0.7,
		lines = { { 112, 332, 330, 20 } } },
	{ map = 947, name = { map = 1415, text = "Eastern Kingdoms" }, fade = "land", ink = LAND_INK, glow = LAND_GLOW, glowA = 0.7,
		lines = { { 660, 800, 387, 14 }, { 641, 816, 411.5, 14.5 } } },

	{ map = 1414, name = { area = 457, text = "The Veiled Sea" }, fade = "sea", ink = SEA_INK, glow = SEA_GLOW, glowA = 0.35,
		lines = { { 251, 287, 161, 11.5, true }, { 225, 314, 185, 17 } } },
}
"#;

const FLIGHT_LUA: &str = "local _, ns = ...\nns.MapFlights = {\n\tnodes = {\n\t\t[2] = { \"Stormwind, Elwynn\", 0, -8832.8, 478.6, 1 },\n\t},\n\tpaths = {\n\t},\n}\n";
const PLACES_LUA: &str = "local _, ns = ...\nns.MapPlaces = {\n\tinstances = {\n\t\t{ 0, -11207.8, 1681.2, { { 36, \"Deadmines\", false, 16, 21 } } },\n\t},\n\tunplaced = {},\n\tbosses = {},\n\ttransports = {},\n\tsailings = {},\n}\n";
const REVEAL: &str = r#"
local _, ns = ...
ns.MapReveal = {
	[1228] = {["500:334:0:0"] = "1, 2", ["502:334:500:0"] = "3", ["1002:334:0:334"] = "4, 5, 6, 7"},
	[1235] = {["1002:668:0:0"] = "8"},
}
"#;

fn raw_blp(img: &RgbaImage) -> Vec<u8> {
    let (w, h) = img.dimensions();
    let mut out = b"BLP2".to_vec();
    out.extend(1u32.to_le_bytes());
    out.extend([3, 8, 8, 0]);
    out.extend(w.to_le_bytes());
    out.extend(h.to_le_bytes());
    out.extend(((20 + 128 + 1024) as u32).to_le_bytes());
    out.extend([0u8; 60]);
    out.extend((w * h * 4).to_le_bytes());
    out.extend([0u8; 60]);
    out.extend(vec![0u8; 1024]);
    for p in img.pixels() {
        out.extend([p[2], p[1], p[0], p[3]]);
    }
    out
}

const COLORS: [((u32, u32), [u8; 3]); 4] = [((0, 0), [200, 0, 0]), ((1, 0), [0, 200, 0]), ((0, 1), [0, 0, 200]), ((3, 2), [200, 200, 0])];

/// A small OpenMap folder: 34 px zone tiles (1 px border), world art tiles, data files.
fn make_openmap(root: &Path, with_places: bool) -> PathBuf {
    let addon = root.join("_forever_").join("Interface").join("AddOns").join("OpenMap");
    let data = addon.join("Data").join("Forever");
    let tex = addon.join("Media").join("Textures").join("Forever");
    for d in [&data, &tex.join("MapZones"), &tex.join("MapArt"), &tex.join("MapZoneLand")] {
        std::fs::create_dir_all(d).unwrap();
    }
    std::fs::write(data.join("ZoneArt.lua"), ZONE_ART).unwrap();
    std::fs::write(data.join("ArtEdits.lua"), ART_EDITS).unwrap();
    std::fs::write(data.join("Labels.lua"), LABELS).unwrap();
    std::fs::write(addon.join("OpenMap.toc"), "## Title: OpenMap\n## Version: 0.8.3\n").unwrap();
    if with_places {
        std::fs::write(data.join("FlightData.lua"), FLIGHT_LUA).unwrap();
        std::fs::write(data.join("Places.lua"), PLACES_LUA).unwrap();
        std::fs::write(data.join("Reveal.lua"), REVEAL).unwrap();
        let mut land = RgbaImage::from_pixel(8, 8, Rgba([255, 255, 255, 0]));
        for y in 0..8 {
            for x in 0..4 {
                land.put_pixel(x, y, Rgba([255, 255, 255, 255])); // the zone's land is its west half
            }
        }
        land.save(tex.join("MapZoneLand").join("1431.tga")).unwrap();
    }
    for ((i, j), c) in COLORS {
        let mut tile = RgbaImage::from_pixel(34, 34, Rgba([255, 0, 255, 255])); // the border: never shown
        for y in 1..33 {
            for x in 1..33 {
                tile.put_pixel(x, y, Rgba([c[0], c[1], c[2], 255]));
            }
        }
        std::fs::write(tex.join("MapZones").join(format!("{i}_{j}.blp")), raw_blp(&tile)).unwrap();
    }
    for n in 1..=12u8 {
        RgbaImage::from_pixel(256, 256, Rgba([n * 20, 100, 50, 255])).save(tex.join("MapArt").join(format!("947_{n}.tga"))).unwrap();
    }
    for n in 1..=2u8 {
        RgbaImage::from_pixel(256, 256, Rgba([10, n * 100, 10, 255])).save(tex.join("MapArt").join(format!("1414_{n}.tga"))).unwrap();
    }
    RgbaImage::from_pixel(16, 16, Rgba([130, 120, 90, 255])).save(tex.join("MapSeaFieldOut.tga")).unwrap();
    addon
}

fn world_event() -> Value {
    json!({
        "openmap": true, "worldMapID": 947, "openmapVersion": "0.8.3",
        "maps": [
            {"mapID": 947, "mapType": 1, "parentMapID": 0, "name": "Азерот", "rect": [0, 1, 0, 1],
             "layers": [{"w": 1002, "h": 668, "tileW": 256, "tileH": 256, "tiles": (1..=12).collect::<Vec<_>>()}]},
            {"mapID": 1414, "mapType": 2, "parentMapID": 947, "name": "Калимдор", "rect": [-0.17, 0.63, 0.05, 0.85],
             "layers": [{"w": 1002, "h": 668, "tileW": 256, "tileH": 256, "tiles": (101..=112).collect::<Vec<_>>()}]},
            {"mapID": 1415, "mapType": 2, "parentMapID": 947, "name": "Восточные королевства", "rect": [0.36, 1.16, 0.08, 0.88],
             "layers": [{"w": 1002, "h": 668, "tileW": 256, "tileH": 256, "tiles": (201..=212).collect::<Vec<_>>()}]},
        ],
        "yards": {"1415": corners(11176.34, 18171.97, 27149.69, 40741.18, 0)},
    })
}

fn corners(top: f64, left: f64, h: f64, w: f64, continent: i64) -> Value {
    let at = |mx: f64, my: f64| json!([top - my * h, left - mx * w]);
    json!({"continent": continent, "p00": at(0.0, 0.0), "p10": at(1.0, 0.0), "p01": at(0.0, 1.0)})
}

fn close_px(p: &Rgba<u8>, c: [u8; 4], tol: i32) -> bool {
    (0..4).all(|i| (p[i] as i32 - c[i] as i32).abs() <= tol)
}

#[test]
fn parsers() {
    let art = parse_zone_art(ZONE_ART).unwrap();
    assert!(art.px == 16.0 && art.size == 34 && art.step == 32);
    assert_eq!(art.tiles, vec![(0, 0), (0, 1), (1, 0), (3, 2)]);
    let edits = parse_art_edits(ART_EDITS);
    assert_eq!(edits[&947].len(), 12);
    assert_eq!(edits[&1414], [1, 2].into_iter().collect());
    let labels = parse_world_labels(LABELS);
    assert_eq!(labels.iter().map(|l| (l["mapID"].as_i64().unwrap(), l["text"].as_str().unwrap())).collect::<Vec<_>>(),
               vec![(1414, "Kalimdor"), (1415, "Eastern Kingdoms")]);
    assert!(labels[0]["x"] == 222.0 && labels[0]["y"] == 320.0 && labels[0]["size"] == 20.0);
    assert_eq!(labels[1]["y"], (373.0 + 411.5) / 2.0);
    assert_eq!(parse_reveal(REVEAL)[&1228].len(), 3);
}

#[test]
fn zone_tile_pyramid() {
    let root = temp_dir("pyramid");
    let addon = make_openmap(&root, false);
    assert_eq!(find_openmap(Some(&root), None).unwrap(), std::path::absolute(&addon).unwrap());
    let om = OpenMapArt::open(&addon, &root.join("cache")).unwrap();
    assert_eq!(om.version, "0.8.3");
    let mut levels: Vec<Vec<(u32, u32)>> = om.levels.iter().map(|l| { let mut v: Vec<_> = l.iter().copied().collect(); v.sort(); v }).collect();
    levels.iter_mut().for_each(|l| l.sort());
    assert_eq!(levels, vec![vec![(0, 0), (0, 1), (1, 0), (3, 2)], vec![(0, 0), (1, 1)], vec![(0, 0)]]);
    let desc = om.describe();
    assert!(desc["maxLevel"] == 2 && desc["unit"] == 2.0 && desc["tilePx"] == 32);
    let tile = load_image(&om.zone_tile(0, 1, 0).unwrap()).unwrap();
    assert_eq!(tile.dimensions(), (32, 32));
    for (x, y) in [(0, 0), (16, 16), (31, 31)] {
        assert!(close_px(tile.get_pixel(x, y), [0, 200, 0, 255], 14), "no border");
    }
    let up = load_image(&om.zone_tile(1, 0, 0).unwrap()).unwrap();
    assert!(close_px(up.get_pixel(8, 8), [200, 0, 0, 255], 14));
    assert!(close_px(up.get_pixel(24, 8), [0, 200, 0, 255], 14));
    assert!(close_px(up.get_pixel(8, 24), [0, 0, 200, 255], 14));
    assert_eq!(up.get_pixel(24, 24)[3], 0);
    assert!(om.zone_tile(0, 2, 2).is_none() && om.zone_tile(5, 0, 0).is_none());
    om.warm();
    assert!(om.zone_tile(2, 0, 0).is_some());
}

fn worlds(root: &Path, with_places: bool) -> (Arc<MapImages>, Arc<WorldImages>, Arc<OpenMapArt>) {
    let om = Arc::new(OpenMapArt::open(&make_openmap(root, with_places), &root.join("cache")).unwrap());
    let maps = Arc::new(MapImages::new(&root.join("cache")));
    let world = Arc::new(WorldImages::new(&root.join("cache"), maps.clone()));
    *world.openmap.write().unwrap() = Some(om.clone());
    (maps, world, om)
}

fn key_of(url: &str) -> String {
    url.rsplit('/').next().unwrap().split('.').next().unwrap().to_string()
}

#[test]
fn world_art_from_openmap() {
    let root = temp_dir("worldart");
    let (_, world, _) = worlds(&root, false);
    let desc = world.register(&world_event());
    assert!(desc["width"] == 1002 && desc["height"] == 668 && desc["openmapInGame"] == true);
    let kinds: Vec<(i64, &str)> = desc["art"].as_array().unwrap().iter().map(|a| (a["mapID"].as_i64().unwrap(), a["kind"].as_str().unwrap())).collect();
    assert_eq!(kinds, vec![(947, "world"), (1414, "continent"), (1415, "continent")]);
    let labels: Vec<&str> = desc["labels"].as_array().unwrap().iter().map(|l| l["text"].as_str().unwrap()).collect();
    assert_eq!(labels, vec!["Калимдор", "Восточные королевства"]); // the game's names
    assert!(desc["zones"]["maxLevel"] == 2 && desc["sea"]["url"].as_str().unwrap().starts_with("/world/sea.png"));
    let image = load_image(&world.art_path(&key_of(desc["art"][0]["url"].as_str().unwrap())).unwrap()).unwrap();
    assert_eq!(image.dimensions(), (1002, 668));
    let p = image.get_pixel(500, 334);
    assert!(p[0] == 120 && p[1] == 100 && p[2] == 50 && p[3] == 255); // tile 6
    assert_eq!(image.get_pixel(0, 0)[3], 0); // the torn edge fades out
    let continent = load_image(&world.art_path(&key_of(desc["art"][1]["url"].as_str().unwrap())).unwrap()).unwrap();
    let p = continent.get_pixel(300, 128);
    assert_eq!([p[0], p[1], p[2]], [10, 200, 10]);
    assert_eq!(load_image(&world.sea_path().unwrap()).unwrap().dimensions(), (16, 16));
    assert!(desc["places"].is_null()); // this OpenMap has no flight data
}

#[test]
fn world_without_openmap_or_game_data() {
    let root = temp_dir("bare");
    let maps = Arc::new(MapImages::new(&root));
    let world = WorldImages::new(&root, maps);
    let desc = world.register(&world_event());
    assert!(desc["zones"].is_null() && desc["labels"] == json!([]) && desc["sea"]["url"].is_null());
    let img = load_image(&world.art_path(&key_of(desc["art"][0]["url"].as_str().unwrap())).unwrap()).unwrap();
    assert_eq!(img.dimensions(), (1002, 668));
    assert!(world.register(&json!({"maps": [], "worldMapID": 947})).is_null());
}

#[test]
fn places_placed_by_the_continents_yards() {
    let root = temp_dir("places");
    let (_, world, _) = worlds(&root, true);
    let desc = world.register(&world_event());
    assert!(desc["places"]["url"].as_str().unwrap().starts_with("/world/places.json?v="));
    let places: Value = serde_json::from_slice(&world.places_json().unwrap()).unwrap();
    assert_eq!(places["flights"]["nodes"][0]["id"], 2);
    assert_eq!(places["dungeons"][0]["insts"][0]["name"], "Deadmines");
    let mut no_yards = world_event();
    no_yards["yards"] = json!({});
    assert!(world.register(&no_yards)["places"].is_null());
}

#[test]
fn build_places_from_tables() {
    let continents = json!({"1415": corners(10000.0, 20000.0, 20000.0, 30000.0, 0), "1414": corners(10000.0, 20000.0, 20000.0, 30000.0, 1)});
    let rects = HashMap::from([("1415".to_string(), [0.5, 1.0, 0.0, 0.5]), ("1414".to_string(), [0.0, 0.5, 0.25, 0.75])]);
    let to_world = YardsToWorld::new(&continents, &rects);
    let flights = json!({"nodes": {"2": ["Stormwind", 0, 10000, 20000, 1], "4": ["Sentinel Hill", 0, 0, 5000, 1],
                                   "23": ["Orgrimmar", 1, 0, 5000, 2], "59": ["Dun Baldar", 30, 0, 0, 1]},
                         "paths": [[2, 4, 100, 10, [10000, 20000, 5000, 12000, 0, 5000]], [4, 2, 100, 10, [0, 5000, 10000, 20000]],
                                   [2, 59, 100, 10, [10000, 20000, 0, 0]]]});
    let places = json!({
        "instances": [[0, 0, 5000, [[36, "Deadmines", false, 16, 21]]],
                      {"1": null, "2": 0.2, "3": 0.3, "4": [[0, "The Drowned City", false, 35, 40]], "map": 1434},
                      [1, 0, 5000, [[409, "Molten Core", true, 60, 60], [469, "Blackwing Lair", true, 60, 60]]],
                      [9, 0, 0, [[1, "Nowhere", false, 1, 2]]]],
        "bosses": {"36": ["Rhahk'Zor", "Edwin VanCleef"], "409": ["Ragnaros"]},
        "transports": [["boat", "Menethil - Theramore", 0, 0, 5000, 1, 0, 5000],
                       ["airship", "Alterac - Zephras Isle", 0, 10000, 20000, 2991, 1, 1],
                       {"1": "zeppelin", "2": "Undercity", "3": 0, "4": 0, "5": 0, "6": 0, "7": 0, "8": 1000, "faction": "Horde"}],
        "sailings": [["boat", "Menethil - Theramore", [[0, [0, 5000, -5000, 0]], [1, [5000, 10000, 0, 5000]]]],
                     ["zeppelin", "Grom'gol - Orgrimmar", [[0, [0, 0, 100, 100]]]],
                     ["boat", "Booty Bay - Ratchet", [[0, [0, 0, 1, 1]], [7, [0, 0, 1, 1]]]],
                     ["airship", "Alterac - Zephras Isle", [[0, [0, 0, 1, 1]]]]],
    });
    let out = build_places(&flights, &places, &to_world);
    let ids: Vec<i64> = out["flights"]["nodes"].as_array().unwrap().iter().map(|n| n["id"].as_i64().unwrap()).collect();
    assert_eq!(ids, vec![2, 4, 23]);
    assert!(out["flights"]["nodes"][0]["u"] == 0.5 && out["flights"]["nodes"][0]["v"] == 0.0);
    let paths = out["flights"]["paths"].as_array().unwrap();
    assert!(paths.len() == 1 && paths[0]["pts"].as_array().unwrap().len() == 6);
    let d = out["dungeons"].as_array().unwrap();
    assert_eq!(d.len(), 3);
    assert_eq!(d[0]["insts"][0]["bosses"], json!(["Rhahk'Zor", "Edwin VanCleef"]));
    assert_eq!(d[1], json!({"map": 1434, "x": 0.2, "y": 0.3, "insts": [{"id": 0, "name": "The Drowned City", "raid": false, "min": 35, "max": 40, "bosses": []}]}));
    let docks: Vec<(String, Value)> = out["docks"].as_array().unwrap().iter().map(|x| (x["kind"].as_str().unwrap().to_string(), x["faction"].clone())).collect();
    assert_eq!(docks, vec![("boat".into(), Value::Null), ("airship".into(), Value::Null), ("zeppelin".into(), json!("Horde"))]);
    assert!(out["docks"][1]["to"].is_null());
    let sails: Vec<(&str, &str)> = out["sailings"].as_array().unwrap().iter().map(|s| (s["route"].as_str().unwrap(), s["faction"].as_str().unwrap())).collect();
    assert_eq!(sails, vec![("Menethil - Theramore", "Alliance"), ("Grom'gol - Orgrimmar", "Horde")]);
    assert_eq!(out["sailings"][0]["pts"].as_array().unwrap().len(), 2 * (2 + 23 + 2));
}

fn alpha_at(path: &Path, fx: f64, fy: f64) -> u8 {
    let img = load_image(path).unwrap();
    let (w, h) = img.dimensions();
    img.get_pixel(((fx * w as f64) as u32).min(w - 1), ((fy * h as f64) as u32).min(h - 1))[3]
}

fn shade_zones() -> (Value, Value) {
    let zones = json!({"1429": {"name": "Элвиннский лес", "parentMapID": 1415, "rect": [0.70, 0.78, 0.60, 0.68]},
                       "1431": {"name": "Сумеречный лес", "parentMapID": 1415, "rect": [0.71, 0.77, 0.66, 0.72]},
                       "1453": {"name": "Штормград", "parentMapID": 1415, "rect": [0.69, 0.71, 0.60, 0.62]}});
    let explored = json!({"1429": {"art": 1228, "layer": [1002, 668], "pieces": [[0, 0, 500, 334]]},
                          "1431": {"art": 1235, "layer": [1002, 668], "pieces": []},
                          "1453": {"art": 9999, "layer": [1002, 668], "pieces": []}});
    (zones, explored)
}

#[test]
fn shades_of_unexplored_land() {
    let root = temp_dir("shade");
    let (_, _, om) = worlds(&root, true);
    let shades = ZoneShades::new(&root);
    let (zones, explored) = shade_zones();
    let out = shades.register(&explored, &zones, Some((1002.0, 668.0)), Some(&om));
    let ids: Vec<&String> = out.as_object().unwrap().keys().collect();
    assert_eq!(ids, vec!["1429", "1431"]);
    assert!(out["1429"]["missing"] == false && out["1429"]["rect"] == zones["1429"]["rect"]);
    assert!(out["1431"]["missing"] == true && out["1431"]["rect"][0].as_f64().unwrap() < 0.71);
    let path = shades.shade_path(&key_of(out["1429"]["url"].as_str().unwrap())).unwrap();
    assert_eq!(alpha_at(&path, 0.2, 0.2), 0);
    assert!((alpha_at(&path, 0.8, 0.2) as i32 - 158).abs() <= 2 && (alpha_at(&path, 0.5, 0.8) as i32 - 158).abs() <= 2);
    let path = shades.shade_path(&key_of(out["1431"]["url"].as_str().unwrap())).unwrap();
    assert!(alpha_at(&path, 0.42, 0.5) > 150 && alpha_at(&path, 0.6, 0.5) == 0 && alpha_at(&path, 0.1, 0.5) == 0);
    assert!(shades.shade_path(&"0".repeat(16)).is_none());
    assert_eq!(shades.register(&explored, &zones, Some((1002.0, 668.0)), None), json!({}));
}

fn addons_folder(root: &Path) -> PathBuf {
    let folder = root.join("AddOns").join("Questie").join("Icons");
    std::fs::create_dir_all(&folder).unwrap();
    let mut img = RgbaImage::from_pixel(4, 2, Rgba([0, 0, 0, 255]));
    for x in 0..2 {
        img.put_pixel(x, 0, Rgba([255, 0, 0, 255]));
    }
    for x in 2..4 {
        img.put_pixel(x, 0, Rgba([255, 255, 255, 255]));
    }
    img.save(folder.join("Slay.TGA")).unwrap();
    root.join("AddOns")
}

#[test]
fn find_addon_file_ignores_case_slashes_and_extension() {
    let root = temp_dir("icons");
    let maps = Arc::new(MapImages::new(&root.join("cache")));
    let icons = PinIcons::new(&root.join("cache"), maps);
    *icons.addons_dir.write().unwrap() = Some(addons_folder(&root));
    let want = root.join("AddOns").join("Questie").join("Icons").join("Slay.TGA").to_string_lossy().to_lowercase();
    let found = icons.find_addon_file(r"Interface\AddOns\questie\icons\slay").unwrap();
    assert_eq!(found.to_string_lossy().to_lowercase(), want);
    assert!(icons.find_addon_file("interface/addons/Questie/Icons/slay.tga").is_some());
    assert!(icons.find_addon_file(r"Interface\AddOns\Questie\Icons\missing.blp").is_none());
    assert!(icons.find_addon_file(r"Interface\Icons\INV_Misc_Bag_10").is_none());
}

#[test]
fn cut_and_tint_icons() {
    let mut image = RgbaImage::from_pixel(4, 4, Rgba([255, 255, 255, 255]));
    image.put_pixel(0, 0, Rgba([255, 0, 0, 255]));
    let lt = cut_and_tint(&image, &[0.0, 0.5, 0.0, 0.5], &[255; 4]);
    assert!(lt.dimensions() == (2, 2) && lt.get_pixel(0, 0).0 == [255, 0, 0, 255]);
    let flipped = cut_and_tint(&image, &[0.5, 0.0, 0.0, 0.5], &[255; 4]);
    assert_eq!(flipped.get_pixel(1, 0).0, [255, 0, 0, 255]);
    let tinted = cut_and_tint(&image, &[0.0, 1.0, 0.0, 1.0], &[255, 128, 0, 128]);
    assert_eq!(tinted.get_pixel(3, 3).0, [255, 128, 0, 128]);
    assert_eq!(cut_and_tint(&RgbaImage::new(256, 128), &[0.0, 1.0, 0.0, 1.0], &[255; 4]).dimensions(), (64, 32));
}

async fn get(app: &axum::Router, uri: &str) -> (StatusCode, Vec<u8>) {
    let mut req = Request::builder().uri(uri).body(Body::empty()).unwrap();
    req.extensions_mut().insert(ConnectInfo(SocketAddr::from(([192, 168, 1, 5], 5555))));
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let body = axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap().to_vec();
    (status, body)
}

#[tokio::test]
async fn web_routes_serve_the_world() {
    let root = temp_dir("web");
    let (maps, world, _) = worlds(&root, true);
    let icons = Arc::new(PinIcons::new(&root.join("cache"), maps.clone()));
    *icons.addons_dir.write().unwrap() = Some(addons_folder(&root));
    let hub = Arc::new(Hub::new(maps, world, icons, Arc::new(ZoneShades::new(&root.join("cache")))));
    hub.publish("world", world_event());
    let (zones, explored) = shade_zones();
    hub.publish("zones", zones);
    hub.publish("explored", explored);
    hub.publish("pins", json!({"sources": ["Questie", "SilverDragon"],
        "icons": [{"fileID": null, "path": r"Interface\AddOns\Questie\Icons\slay", "coords": [0, 0.5, 0, 0.5], "color": [255, 255, 255, 255]},
                  {"fileID": 132320, "path": null, "coords": [0, 1, 0, 1], "color": [255, 255, 255, 255]}],
        "pins": [{"source": 0, "u": 0.7, "v": 0.6, "icon": 0, "size": 16, "title": "Волк", "sub": null, "label": null, "active": false}]}));
    let snap = hub.snapshot();
    let app = router(App { hub: hub.clone(), token: "secret".into(), public_url: "http://x".into() });

    assert_eq!(get(&app, "/world/zone/0/0_0").await.0, StatusCode::FORBIDDEN);
    let (status, body) = get(&app, "/world/zone/0/0_0?t=secret").await;
    assert!(status == StatusCode::OK && !body.is_empty());
    assert_eq!(get(&app, "/world/zone/0/2_2?t=secret").await.0, StatusCode::NOT_FOUND);
    assert_eq!(get(&app, "/world/zone/9/0_0?t=secret").await.0, StatusCode::NOT_FOUND);
    let art = snap["world"]["art"][0]["url"].as_str().unwrap();
    assert_eq!(get(&app, &format!("{art}?t=secret")).await.0, StatusCode::OK);
    let sea = snap["world"]["sea"]["url"].as_str().unwrap();
    assert_eq!(get(&app, &format!("{sea}&t=secret")).await.0, StatusCode::OK);
    let places = snap["world"]["places"]["url"].as_str().unwrap();
    let (status, body) = get(&app, &format!("{places}&t=secret")).await;
    assert_eq!(status, StatusCode::OK);
    let places: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(places["flights"]["nodes"][0]["name"], "Stormwind, Elwynn");
    let shade = snap["explored"]["1429"]["url"].as_str().unwrap();
    assert_eq!(get(&app, &format!("{shade}?t=secret")).await.0, StatusCode::OK);
    let icon = snap["pins"]["icons"][0].as_str().unwrap();
    let (status, body) = get(&app, &format!("{icon}?t=secret")).await;
    assert_eq!(status, StatusCode::OK);
    let img = image::load_from_memory(&body).unwrap().to_rgba8();
    assert!(img.dimensions() == (2, 1) && img.get_pixel(0, 0).0 == [255, 0, 0, 255]);
    assert!(snap["pins"]["icons"][1].is_null()); // a game texture without the game's files: the page draws a dot
    assert_eq!(get(&app, "/icon/0123456789abcdef.png?t=secret").await.0, StatusCode::NOT_FOUND);
    let (status, body) = get(&app, "/").await;
    assert!(status == StatusCode::OK && String::from_utf8_lossy(&body).contains("Токен"));
    assert_eq!(get(&app, "/qr").await.0, StatusCode::FORBIDDEN); // only on the PC itself
    let (status, body) = get(&app, "/static/app.js").await;
    assert!(status == StatusCode::OK && body.len() > 1000);
}

#[test]
fn minimap_tiles_are_read_per_continent() {
    let text = "local _, ns = ...\nns.MapMinimap = {\n\t[0] = {\n\t\t[1527] = 5313059, [1526] = 5313057,\n\t},\n\t[1] = { [65] = 7 },\n}\n";
    let minimap = parse_minimap(text);
    assert_eq!(minimap[&0], vec![(1526, 5313057), (1527, 5313059)]);
    assert_eq!(minimap[&1], vec![(65, 7)]);
}
