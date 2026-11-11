//! End-to-end: the real addon Lua (in LuaJIT with a mock WoW API) -> pixels -> decoder.

use crate::places::YardsToWorld;
use crate::protocol::{Decoder, Frame};
use crate::sim::SimGame;
use serde_json::{json, Value};
use std::collections::{BTreeSet, HashMap};

mod casc;
mod world;

const BULK: [&str; 7] = ["session", "map", "roster", "world", "zones", "travel", "explored"];

fn game() -> SimGame {
    SimGame::new(true, true).unwrap()
}

fn run(game: &SimGame, decoder: &mut Decoder, frames: usize) -> HashMap<String, Value> {
    run_sized(game, decoder, frames, 1024, 48)
}

fn run_sized(game: &SimGame, decoder: &mut Decoder, frames: usize, w: usize, h: usize) -> HashMap<String, Value> {
    let mut latest = HashMap::new();
    for _ in 0..frames {
        game.step(1.0 / 30.0);
        for (name, data) in decoder.feed(&game.render(w, h)) {
            latest.insert(name, data);
        }
    }
    latest
}

fn has_all(got: &HashMap<String, Value>, names: &[&str]) -> bool {
    names.iter().all(|n| got.contains_key(*n))
}

fn rect(game: &SimGame, map_id: u32) -> [f64; 4] {
    let r: Vec<f64> = (1..=4).map(|i| game.eval(&format!("MockWoW.maps[{map_id}].rect[{i}]")).unwrap()).collect();
    [r[0], r[1], r[2], r[3]]
}

fn close(a: f64, b: f64, tol: f64) -> bool {
    (a - b).abs() <= tol
}

fn f(v: &Value) -> f64 {
    v.as_f64().unwrap_or(f64::NAN)
}

#[test]
fn full_chain_decodes_everything() {
    let game = game();
    let mut decoder = Decoder::new();
    let got = run(&game, &mut decoder, 90);
    assert_eq!(decoder.bad_pages, 0);
    let g = decoder.geometry.unwrap();
    assert_eq!((g.cell, g.cols, g.rows), (1, 1024, 1));
    assert_eq!(got["session"], json!({"addonVersion": 2, "interface": 16001, "locale": "ruRU", "player": "Тестер",
                                       "realm": "Тестовый мир", "classID": 8, "faction": "Alliance", "level": 30}));
    let info = &got["map"]["info"];
    assert_eq!((info["mapID"].clone(), info["name"].clone(), info["parentMapID"].clone()), (json!(1429), json!("Элвиннский лес"), json!(1415)));
    let firsts: Vec<u64> = got["map"]["explored"].as_array().unwrap().iter().map(|o| o["files"][0].as_u64().unwrap()).collect();
    assert_eq!(firsts, vec![2212713, 271572, 271560, 271567, 271576]); // not the mouse-over one
    assert_eq!(got["roster"], json!({"101": {"classID": 1, "name": "Воин"}, "102": {"classID": 5, "name": "Жрица"}}));
    let state = &got["state"];
    assert!(state["hasPos"] == true && state["hasFacing"] == true && state["mounted"] == true && state["combat"] == false);
    let (px, py): (f64, f64) = (game.eval("MockWoW.px").unwrap(), game.eval("MockWoW.py").unwrap());
    assert!(close(f(&state["x"]), px, 0.01) && close(f(&state["y"]), py, 0.01));
    let units: Vec<u64> = got["group"].as_array().unwrap().iter().map(|m| m["unit"].as_u64().unwrap()).collect();
    assert_eq!(units, vec![101, 102]);
}

#[test]
fn position_precision_is_16_bit() {
    let game = game();
    let mut decoder = Decoder::new();
    run(&game, &mut decoder, 3);
    game.exec("MockWoW.step = function() end").unwrap();
    for x in [0.0, 0.123456, 1.0] {
        game.exec(&format!("MockWoW.px = {x}; MockWoW.time = MockWoW.time + 1")).unwrap();
        game.tick().unwrap();
        let events = decoder.feed(&game.render(1024, 48));
        let state = &events.iter().find(|(n, _)| n == "state").unwrap().1;
        assert!(close(f(&state["x"]), x, 1.0 / 65535.0));
    }
}

#[test]
fn strip_settings_still_decode() {
    for commands in [vec!["cell 1"], vec!["cols 128", "rows 4", "cell 4"], vec!["cols 128", "rows 3", "cell 8"]] {
        let game = game();
        for c in &commands {
            game.slash(c).unwrap();
        }
        let mut decoder = Decoder::new();
        let got = run_sized(&game, &mut decoder, 200, 1024, 64);
        assert_eq!(decoder.bad_pages, 0, "{commands:?}");
        assert!(has_all(&got, &BULK) && got.contains_key("state"), "{commands:?}");
    }
}

#[test]
fn late_companion_still_gets_bulk_data() {
    let game = game();
    run(&game, &mut Decoder::new(), 600); // 20 s: the blobs are past their fast cycles
    let mut decoder = Decoder::new();
    let got = run(&game, &mut decoder, 240); // 8 s of slow repeats
    assert!(has_all(&got, &BULK));
}

#[test]
fn reload_with_same_generation_is_detected() {
    let mut decoder = Decoder::new();
    let chunk = |gen: u16, data: Vec<u8>| -> Vec<u8> {
        let mut body = vec![2];
        body.extend(gen.to_le_bytes());
        body.extend(0u16.to_le_bytes());
        body.extend(1u16.to_le_bytes());
        body.extend(data);
        let mut out = vec![0x10];
        out.extend((body.len() as u16).to_le_bytes());
        out.extend(body);
        out
    };
    let mapinfo = |name: &[u8]| -> Vec<u8> {
        let mut body = 5u32.to_le_bytes().to_vec();
        body.push(3);
        body.extend(0u32.to_le_bytes());
        body.push(name.len() as u8);
        body.extend(name);
        body.push(0);
        let mut out = vec![0x21];
        out.extend((body.len() as u16).to_le_bytes());
        out.extend(body);
        out
    };
    let names = |events: Vec<(String, Value)>| -> Vec<String> {
        events.into_iter().filter(|(n, _)| n == "map").map(|(_, d)| d["info"]["name"].as_str().unwrap().to_string()).collect()
    };
    assert_eq!(names(decoder.handle_payload(&chunk(7, mapinfo(b"A")))), vec!["A"]);
    assert!(names(decoder.handle_payload(&chunk(7, mapinfo(b"A")))).is_empty());
    assert_eq!(names(decoder.handle_payload(&chunk(7, mapinfo(b"B")))), vec!["B"]);
}

#[test]
fn blank_or_missing_strip_is_reported_not_counted() {
    let mut decoder = Decoder::new();
    assert!(decoder.feed(&Frame::new(1024, 48)).is_empty());
    assert_eq!(decoder.problem, "no strip in the top-left corner");
    assert_eq!(decoder.bad_pages, 0);
}

#[test]
fn distorted_colors_are_reported() {
    let game = game();
    let mut decoder = Decoder::new();
    game.step(0.2);
    let mut img = game.render(1024, 48);
    for v in img.rgb.iter_mut() {
        *v = (*v / 64) * 64; // crushed to four shades a channel: nothing left to tell apart
    }
    img.rgb[..6].copy_from_slice(&[255, 0, 255, 0, 255, 0]);
    decoder.feed(&img);
    assert!(decoder.problem.contains("distorted"));
}

#[test]
fn shifted_colors_still_decode() {
    // what HDR, gamma or a colour profile do: every shade moves, their order stays
    let game = game();
    let mut decoder = Decoder::new();
    let mut latest = HashMap::new();
    for _ in 0..200 {
        game.step(1.0 / 30.0);
        let mut img = game.render(1024, 48);
        for v in img.rgb.iter_mut() {
            *v = ((*v as f64 / 255.0).powf(0.7) * 235.0 + 12.0).round() as u8;
        }
        for (name, data) in decoder.feed(&img) {
            latest.insert(name, data);
        }
    }
    assert_eq!(decoder.bad_pages, 0);
    assert!(has_all(&latest, &BULK) && latest.contains_key("state"));
}

#[test]
fn world_channel_describes_azeroth_and_continents() {
    let game = game();
    let got = run(&game, &mut Decoder::new(), 150);
    let world = &got["world"];
    assert!(world["openmap"] == true && world["worldMapID"] == 947 && world["openmapVersion"] == "0.8.3");
    let maps = world["maps"].as_array().unwrap();
    assert_eq!(maps.iter().map(|m| m["name"].as_str().unwrap()).collect::<Vec<_>>(), vec!["Азерот", "Калимдор", "Восточные королевства"]);
    for m in maps {
        let id = m["mapID"].as_u64().unwrap() as u32;
        let want = if id == 947 { [0.0, 1.0, 0.0, 1.0] } else { rect(&game, id) };
        for i in 0..4 {
            assert!(close(f(&m["rect"][i]), want[i], 1e-6));
        }
        assert_eq!(m["layers"][0]["tiles"].as_array().unwrap().len(), 12);
    }
    let r = rect(&game, 1429);
    for i in 0..4 {
        assert!(close(f(&got["map"]["rect"][i]), r[i], 1e-6)); // the current map's place on the world
    }
    assert_eq!(got["state"]["hasWorld"], true);
}

#[test]
fn world_position_is_exact() {
    for openmap in [true, false] {
        let game = SimGame::new(openmap, true).unwrap();
        let mut decoder = Decoder::new();
        let got = run(&game, &mut decoder, 150);
        assert_eq!(got["world"]["openmap"], openmap); // without OpenMap: Blizzard's GetMapRectOnMap
        game.exec("MockWoW.step = function() end").unwrap();
        let [l, r, t, b] = rect(&game, 1429);
        for (px, py) in [(0.0, 0.5), (0.123456, 0.654321), (1.0, 1.0)] {
            game.exec(&format!("MockWoW.px, MockWoW.py = {px}, {py}")).unwrap();
            game.tick().unwrap();
            let events = decoder.feed(&game.render(1024, 48));
            let state = &events.iter().find(|(n, _)| n == "state").unwrap().1;
            assert!(close(f(&state["u"]), l + px * (r - l), 1e-6) && close(f(&state["v"]), t + py * (b - t), 1e-6));
        }
    }
}

#[test]
fn no_world_position_where_the_game_hides_it() {
    let game = game();
    let mut decoder = Decoder::new();
    run(&game, &mut decoder, 10);
    game.exec("C_Map.GetPlayerMapPosition = function() return nil end").unwrap();
    let got = run(&game, &mut decoder, 10);
    assert!(got["state"]["hasPos"] == false && got["state"]["hasWorld"] == false);
}

#[test]
fn shrinking_the_strip_mid_stream_recuts_the_blobs() {
    let game = game();
    run(&game, &mut Decoder::new(), 90);
    game.slash("cols 128").unwrap();
    game.slash("rows 2").unwrap(); // a 354 byte page: the 400 byte chunks no longer fit
    game.slash("cell 8").unwrap();
    let mut decoder = Decoder::new();
    let got = run_sized(&game, &mut decoder, 200, 1024, 64);
    assert_eq!(decoder.bad_pages, 0);
    assert!(has_all(&got, &BULK));
}

fn mock_yards(game: &SimGame, continent: u32, wx: f64, wy: f64) -> (f64, f64) {
    let k = |n: &str| -> f64 { game.eval(&format!("MockWoW.yards[{continent}].{n}")).unwrap() };
    (k("a") - k("b") * wy, k("c") - k("d") * wx)
}

#[test]
fn world_channel_places_the_continents_in_yards() {
    let game = game();
    let world = run(&game, &mut Decoder::new(), 150).remove("world").unwrap();
    let yards = &world["yards"];
    assert_eq!(yards["1414"]["continent"], 1);
    assert_eq!(yards["1415"]["continent"], 0);
    let rects: HashMap<String, [f64; 4]> = world["maps"].as_array().unwrap().iter()
        .map(|m| (m["mapID"].to_string(), [f(&m["rect"][0]), f(&m["rect"][1]), f(&m["rect"][2]), f(&m["rect"][3])])).collect();
    let to_world = YardsToWorld::new(yards, &rects);
    for (c, wx, wy) in [(0, -8832.8, 478.6), (1, 1677.6, -4315.7), (0, -14444.3, 509.6)] {
        let (u, v) = to_world.place(&json!(c), wx, wy).unwrap();
        let (eu, ev) = mock_yards(&game, c, wx, wy);
        assert!(close(u, eu, 1e-6) && close(v, ev, 1e-6));
    }
    let [l, r, t, b] = rect(&game, 1429);
    let (u, v) = to_world.place(&json!(0), -8832.8, 478.6).unwrap(); // Stormwind's flight master: Elwynn's north-west
    assert!(l < u && u < (l + r) / 2.0 && t < v && v < (t + b) / 2.0);
    assert!(to_world.place(&json!(2991), 1848.3, 569.1).is_none());
}

#[test]
fn zones_channel_names_and_places_each_zone() {
    let game = game();
    let zones = run(&game, &mut Decoder::new(), 150).remove("zones").unwrap();
    let ids: BTreeSet<u32> = zones.as_object().unwrap().keys().map(|k| k.parse().unwrap()).collect();
    assert_eq!(ids, BTreeSet::from([1411, 1412, 1413, 1426, 1429, 1431, 1432, 1434, 1436, 1440]));
    assert!(zones["1429"]["name"] == "Элвиннский лес" && zones["1429"]["parentMapID"] == 1415);
    for (id, zone) in zones.as_object().unwrap() {
        let want = rect(&game, id.parse().unwrap());
        for i in 0..4 {
            assert!(close(f(&zone["rect"][i]), want[i], 1e-6));
        }
    }
}

fn known(travel: &Value) -> Vec<u32> {
    let mut k: Vec<u32> = travel["nodes"].as_object().unwrap().iter().filter(|(_, n)| n["known"] == true)
        .map(|(id, _)| id.parse().unwrap()).collect();
    k.sort();
    k
}

#[test]
fn travel_channel_learns_flight_masters() {
    let game = game();
    let mut decoder = Decoder::new();
    let travel = run(&game, &mut decoder, 150).remove("travel").unwrap();
    assert_eq!(travel["nodes"].as_object().unwrap().len(), 16);
    assert_eq!(travel["nodes"]["12"]["name"], "Темнолесье, Сумеречный лес");
    assert_eq!(known(&travel), vec![2, 4, 6]); // WoW Forever lists them all: only those OpenMap saw as learned
    assert_eq!(travel["instances"], json!({"34": "Тюрьма Штормграда", "36": "Мертвые копи", "90": "Гномреган", "409": "Огненные Недра"}));
    game.exec(r#"MockWoW.taxi = { { "Штормград, Элвиннский лес", "CURRENT" },
        { "Темнолесье, Сумеречный лес", "REACHABLE" }, { "Телсамар, Лок Модан", "DISTANT" } }
        MockWoW.fire("TAXIMAP_OPENED")"#).unwrap();
    let travel = run(&game, &mut decoder, 60).remove("travel").unwrap();
    assert_eq!(known(&travel), vec![2, 4, 6, 12]);
    assert!(game.eval::<bool>("MapCastCharDB.flights['Темнолесье, Сумеречный лес']").unwrap());
}

#[test]
fn travel_trusts_the_game_list_when_it_hides_some() {
    let game = game();
    game.exec(r#"OpenMapCharDB = nil
        local list = C_TaxiMap.GetTaxiNodesForMap
        C_TaxiMap.GetTaxiNodesForMap = function(mapID)
            local out = list(mapID)
            for _, node in ipairs(out) do node.isUndiscovered = node.nodeID ~= 5 end
            return out
        end"#).unwrap();
    let travel = run(&game, &mut Decoder::new(), 150).remove("travel").unwrap();
    assert_eq!(known(&travel), vec![5]);
}

#[test]
fn no_dungeon_names_from_a_client_that_ignores_the_id() {
    let game = game();
    game.exec(r#"GetRealZoneText = function() return "Элвиннский лес" end"#).unwrap();
    assert_eq!(run(&game, &mut Decoder::new(), 150)["travel"]["instances"], json!({}));
}

#[test]
fn level_up_updates_the_session() {
    let game = game();
    let mut decoder = Decoder::new();
    run(&game, &mut decoder, 30);
    game.exec(r#"MockWoW.fire("PLAYER_LEVEL_UP", 31)"#).unwrap();
    assert_eq!(run(&game, &mut decoder, 30)["session"]["level"], 31);
}

fn pins_by_source(pins: &Value) -> HashMap<String, Vec<Value>> {
    let sources = pins["sources"].as_array().unwrap();
    let mut out: HashMap<String, Vec<Value>> = HashMap::new();
    for pin in pins["pins"].as_array().unwrap() {
        let name = sources[pin["source"].as_u64().unwrap() as usize].as_str().unwrap().to_string();
        out.entry(name).or_default().push(pin.clone());
    }
    out
}

fn world_of(game: &SimGame, map_id: u32, x: f64, y: f64) -> (f64, f64) {
    let [l, r, t, b] = rect(game, map_id);
    (l + x * (r - l), t + y * (b - t))
}

#[test]
fn pins_channel_mirrors_the_map_addons() {
    let game = game();
    let pins = run(&game, &mut Decoder::new(), 200).remove("pins").unwrap();
    let by = pins_by_source(&pins);
    let names: BTreeSet<&str> = by.keys().map(String::as_str).collect();
    assert_eq!(names, BTreeSet::from(["TomTom", "SilverDragon", "RestedXP", "Questie", "HandyNotes", "HandyNotes: Treasures",
                                      "GatherMate2: Herb Gathering", "GatherMate2: Mining"])); // not the disabled plugin
    let icon = |pin: &Value| pins["icons"][pin["icon"].as_u64().unwrap() as usize].clone();

    let tomtom = &by["TomTom"];
    assert!(tomtom.len() == 1 && tomtom[0]["title"] == "Ферма Брекенридж"); // its HereBeDragons pin is not doubled
    let (u, v) = world_of(&game, 1429, 0.42, 0.66);
    assert!(close(f(&tomtom[0]["u"]), u, 1e-6) && close(f(&tomtom[0]["v"]), v, 1e-6));
    assert_eq!(icon(&tomtom[0])["path"], r"Interface\AddOns\TomTom\Images\GoldGreenDot");

    let mut questie = by["Questie"].clone();
    questie.sort_by(|a, b| f(&a["u"]).partial_cmp(&f(&b["u"])).unwrap());
    let titles: Vec<(Value, Value)> = questie.iter().map(|p| (p["title"].clone(), p["sub"].clone())).collect();
    assert_eq!(titles, vec![(json!("Маршал Дуган"), json!("Кобольды в шахте")), (json!("Волк"), json!("Охота на волков")),
                            (json!("Волк"), json!("Охота на волков"))]);
    assert!(icon(&questie[1])["path"].as_str().unwrap().ends_with("slay.blp"));

    let mut steps = by["RestedXP"].clone();
    steps.sort_by_key(|p| p["label"].as_str().unwrap().to_string());
    assert!(steps[0]["label"] == "12" && steps[0]["active"] == true && steps[0]["icon"].is_null());
    assert!(steps[1]["label"] == "13" && steps[1]["active"] == false);
    assert_eq!(steps[0]["title"], "Поговорите с Маршалом Дуганом");

    assert_eq!(by["HandyNotes"][0]["title"], "Мой тайник");
    let star = icon(&by["HandyNotes"][0]);
    assert!(close(f(&star["coords"][1]), 0.25, 1e-4) && close(f(&star["coords"][3]), 0.25, 1e-4));
    assert_eq!(by["HandyNotes"][0]["size"], 14);
    let mut treasures: Vec<&str> = by["HandyNotes: Treasures"].iter().map(|p| p["title"].as_str().unwrap()).collect();
    treasures.sort();
    assert_eq!(treasures, vec!["Сундук в Западном Крае", "Сундук с сокровищами"]);
    assert!(!game.eval::<bool>("GameTooltip:IsShown()").unwrap());

    let mut herbs = by["GatherMate2: Herb Gathering"].clone();
    herbs.sort_by(|a, b| f(&a["u"]).partial_cmp(&f(&b["u"])).unwrap());
    assert_eq!(herbs.iter().map(|p| p["title"].as_str().unwrap()).collect::<Vec<_>>(), vec!["Мироцвет", "Сребролист"]);
    assert_eq!(by["GatherMate2: Mining"][0]["title"], "Медная жила");
    assert_eq!(icon(&by["SilverDragon"][0])["fileID"], 132320);
}

#[test]
fn pins_follow_the_addons_every_two_seconds() {
    let game = game();
    let mut decoder = Decoder::new();
    run(&game, &mut decoder, 90);
    game.exec(r#"TomTom.waypoints[1429]["new"] = { 1429, 0.5, 0.5, title = "Новая точка" }"#).unwrap();
    let pins = run(&game, &mut decoder, 150).remove("pins").unwrap();
    let mut titles: Vec<String> = pins_by_source(&pins)["TomTom"].iter().map(|p| p["title"].as_str().unwrap().to_string()).collect();
    titles.sort();
    assert_eq!(titles, vec!["Новая точка", "Ферма Брекенридж"]);
}

#[test]
fn no_tooltips_read_in_combat() {
    let game = game();
    game.exec("MockWoW.combat = true").unwrap();
    let pins = run(&game, &mut Decoder::new(), 200).remove("pins").unwrap();
    assert!(pins_by_source(&pins)["HandyNotes: Treasures"].iter().all(|p| p["title"].is_null()));
}

#[test]
fn too_many_pins_keep_the_guide_and_waypoints() {
    let game = game();
    game.exec(r#"
        local nodes = {}
        for i = 1, 600 do
            local x, y = 0.2 + (i % 30) / 50, 0.2 + math.floor(i / 30) / 40
            nodes[math.floor(x * 10000 + 0.5) * 1000000 + math.floor(y * 10000 + 0.5) * 100] = 401
        end
        local gm = LibStub("AceAddon-3.0"):GetAddon("GatherMate2")
        local get = gm.GetNodesForZone
        gm.GetNodesForZone = function(self, zone, t)
            if t == "Herb Gathering" and zone == 1429 then return next, nodes, nil end
            return get(self, zone, t)
        end"#).unwrap();
    let pins = run(&game, &mut Decoder::new(), 300).remove("pins").unwrap();
    let by = pins_by_source(&pins);
    assert_eq!(pins["pins"].as_array().unwrap().len(), 400);
    assert!(by["TomTom"].len() == 1 && by["RestedXP"].len() == 2 && by["Questie"].len() == 3);
}

#[test]
fn no_map_addons_no_pins() {
    let game = SimGame::new(true, false).unwrap();
    let pins = run(&game, &mut Decoder::new(), 150).remove("pins").unwrap();
    assert_eq!(pins, json!({"sources": [], "icons": [], "pins": []}));
}

#[test]
fn explored_channel_from_the_addon() {
    let game = game();
    let mut decoder = Decoder::new();
    let explored = run(&game, &mut decoder, 200).remove("explored").unwrap();
    assert!(explored["1429"]["art"] == 1228 && explored["1429"]["layer"] == json!([1002, 668]));
    assert_eq!(explored["1429"]["pieces"][0], json!([0, 0, 485, 405]));
    assert_eq!(explored["1429"]["pieces"].as_array().unwrap().len(), 5);
    assert_eq!(explored["1431"], json!({"art": 1235, "layer": [1002, 668], "pieces": []}));
    assert_eq!(explored["1436"]["art"], 0);
    game.exec(r#"table.insert(MockWoW.explored[1431], { textureWidth = 200, textureHeight = 200, offsetX = 10,
        offsetY = 10, fileDataIDs = { 1 } })
        MockWoW.fire("MAP_EXPLORATION_UPDATED")"#).unwrap();
    let explored = run(&game, &mut decoder, 60).remove("explored").unwrap();
    assert_eq!(explored["1431"]["pieces"], json!([[10, 10, 200, 200]]));
}

#[test]
fn late_companion_gets_every_channel() {
    // the game has been running for a while and nothing changes: a companion started now
    // must still be sent every blob, the last channels too
    let game = game();
    run(&game, &mut Decoder::new(), 600);
    game.exec("MockWoW.step = function() end").unwrap();
    let mut decoder = Decoder::new();
    let mut latest = HashMap::new();
    for _ in 0..400 {
        game.tick().unwrap();
        for (name, data) in decoder.feed(&game.render(1024, 48)) {
            latest.insert(name, data);
        }
    }
    assert!(has_all(&latest, &BULK), "{:?}", latest.keys().collect::<Vec<_>>());
    assert!(latest.contains_key("pins"), "{:?}", latest.keys().collect::<Vec<_>>());
}
