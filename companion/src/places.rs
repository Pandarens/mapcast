//! Travel and dungeon places from OpenMap's data, placed on the seamless world.
//!
//! OpenMap ships the game's flight network (Data/<client>/FlightData.lua) and its dungeon
//! entrances, docks and sailing routes (Data/<client>/Places.lua) in world yards per continent,
//! the coordinates UnitPosition and the taxi tables use. The addon tells where each continent's
//! map lies in those yards (three corners from C_Map.GetWorldPosFromMapPos) and where the map
//! lies on the world; together they put every place on Azeroth's map, as OpenMap's S.FromYards.

use crate::luadata::{at, field, positional};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap, HashSet};

const ASPECT: f64 = 1.5; // Azeroth's map is 1002 x 668 units: a step in v is 1.5 times a step in u
// goblin ports with a flight master for each faction (OpenMap Map/Routes.lua); every other boat is the Alliance's
const NEUTRAL_ROUTES: [&str; 2] = ["Booty Bay - Ratchet", "Steamwheedle Port - Powderfuse Port"];

fn r5(v: f64) -> f64 {
    (v * 100000.0).round() / 100000.0
}

fn f(v: &Value) -> Option<f64> {
    v.as_f64()
}

struct Continent {
    x0: f64,
    y0: f64,
    e1: (f64, f64),
    e2: (f64, f64),
    det: f64,
    rect: [f64; 4],
}

/// World yards on a continent -> a point on Azeroth's map (0..1).
pub struct YardsToWorld {
    by_continent: HashMap<i64, Continent>,
}

impl YardsToWorld {
    /// `yards[mapID] = {continent, p00, p10, p01}` (the continent map's corners in yards),
    /// `rects[mapID]` its rect on the world.
    pub fn new(yards: &Value, rects: &HashMap<String, [f64; 4]>) -> YardsToWorld {
        let mut by_continent = HashMap::new();
        if let Some(m) = yards.as_object() {
            let mut ids: Vec<&String> = m.keys().collect();
            ids.sort_by_key(|k| k.parse::<i64>().unwrap_or(0));
            for id in ids {
                let c = &m[id];
                let Some(rect) = rects.get(id.as_str()) else { continue };
                let p = |k: &str| (c[k][0].as_f64().unwrap_or(0.0), c[k][1].as_f64().unwrap_or(0.0));
                let ((x0, y0), (x1, y1), (x2, y2)) = (p("p00"), p("p10"), p("p01"));
                let (e1, e2) = ((x1 - x0, y1 - y0), (x2 - x0, y2 - y0));
                let det = e1.0 * e2.1 - e1.1 * e2.0;
                if det.abs() < 1e-9 {
                    continue;
                }
                let continent = c["continent"].as_i64().unwrap_or(-1);
                by_continent.entry(continent).or_insert(Continent { x0, y0, e1, e2, det, rect: *rect });
            }
        }
        YardsToWorld { by_continent }
    }

    pub fn is_empty(&self) -> bool {
        self.by_continent.is_empty()
    }

    pub fn place(&self, continent: &Value, x: f64, y: f64) -> Option<(f64, f64)> {
        let c = self.by_continent.get(&continent.as_i64()?)?;
        let (dx, dy) = (x - c.x0, y - c.y0);
        let mx = (dx * c.e2.1 - dy * c.e2.0) / c.det;
        let my = (c.e1.0 * dy - c.e1.1 * dx) / c.det;
        let [l, r, t, b] = c.rect;
        Some((l + mx * (r - l), t + my * (b - t)))
    }
}

fn flat(points: &[(f64, f64)]) -> Vec<f64> {
    points.iter().flat_map(|&(u, v)| [r5(u), r5(v)]).collect()
}

fn flights(data: &Value, to_world: &YardsToWorld) -> Value {
    let mut nodes = Vec::new();
    let mut continent_of: HashMap<i64, Value> = HashMap::new();
    let mut ids: BTreeMap<i64, &Value> = BTreeMap::new();
    if let Some(m) = field(data, "nodes").and_then(Value::as_object) {
        for (k, v) in m {
            if let Ok(id) = k.parse::<i64>() {
                ids.insert(id, v);
            }
        }
    } else if let Some(a) = field(data, "nodes").and_then(Value::as_array) {
        for (i, v) in a.iter().enumerate() {
            ids.insert(i as i64 + 1, v);
        }
    }
    for (id, node) in ids {
        let (name, continent, x, y, factions) = (at(node, 1), at(node, 2), at(node, 3), at(node, 4), at(node, 5));
        let (Some(x), Some(y)) = (f(&x), f(&y)) else { continue };
        let Some((u, v)) = to_world.place(&continent, x, y) else { continue };
        continent_of.insert(id, continent);
        nodes.push(json!({"id": id, "name": name.as_str().unwrap_or(""), "u": r5(u), "v": r5(v),
                          "factions": factions.as_i64().unwrap_or(3)}));
    }
    let mut paths = Vec::new();
    let mut seen = HashSet::new();
    for path in positional(field(data, "paths").unwrap_or(&Value::Null)) {
        let (Some(a), Some(b)) = (at(&path, 1).as_i64(), at(&path, 2).as_i64()) else { continue };
        let (Some(ca), true) = (continent_of.get(&a), continent_of.contains_key(&b)) else { continue };
        let pair = (a.min(b), a.max(b));
        if seen.contains(&pair) {
            continue; // each way is listed both ways, nearly the same: draw one
        }
        let pts = positional(&at(&path, 5));
        let way: Vec<(f64, f64)> = pts
            .chunks(2)
            .filter_map(|p| match p {
                [x, y] => to_world.place(ca, f(x)?, f(y)?),
                _ => None,
            })
            .collect();
        if way.len() >= 2 {
            seen.insert(pair);
            paths.push(json!({"a": a, "b": b, "pts": flat(&way)}));
        }
    }
    json!({"nodes": nodes, "paths": paths})
}

/// The whole way of a vessel, sea crossings between continents bridged by a bowed arc (as OpenMap).
fn sailing_way(pieces: &[Value], to_world: &YardsToWorld) -> Vec<(f64, f64)> {
    let mut way: Vec<(f64, f64)> = Vec::new();
    for piece in pieces {
        let (continent, pts) = (at(piece, 1), positional(&at(piece, 2)));
        let first = way.len();
        for p in pts.chunks(2) {
            if let [x, y] = p {
                if let (Some(x), Some(y)) = (f(x), f(y)) {
                    if let Some(w) = to_world.place(&continent, x, y) {
                        way.push(w);
                    }
                }
            }
        }
        if way.len() == first {
            return vec![]; // half a route would stop out at sea
        }
        if first > 0 {
            let ((u0, v0), (u2, v2)) = (way[first - 1], way[first]);
            let (du, dv) = (u2 - u0, v2 - v0);
            let length = (du * du + (dv * ASPECT).powi(2)).sqrt();
            if length > 0.0 {
                let side = if (v0 + v2) / 2.0 < 0.5 { -1.0 } else { 1.0 };
                let (mut nu, mut nv) = (-dv * ASPECT / length, du / length / ASPECT);
                if nv * side < 0.0 {
                    nu = -nu;
                    nv = -nv;
                }
                let (cu, cv) = ((u0 + u2) / 2.0 + nu * 0.12 * length, (v0 + v2) / 2.0 + nv * 0.12 * length);
                let arc: Vec<(f64, f64)> = (1..24)
                    .map(|k| {
                        let t = k as f64 / 24.0;
                        ((1.0 - t).powi(2) * u0 + 2.0 * (1.0 - t) * t * cu + t * t * u2,
                         (1.0 - t).powi(2) * v0 + 2.0 * (1.0 - t) * t * cv + t * t * v2)
                    })
                    .collect();
                way.splice(first..first, arc);
            }
        }
    }
    way
}

fn travel(data: &Value, to_world: &YardsToWorld) -> (Vec<Value>, Vec<Value>) {
    let mut docks = Vec::new();
    for dock in positional(field(data, "transports").unwrap_or(&Value::Null)) {
        let kind = at(&dock, 1);
        let kind = kind.as_str().unwrap_or("");
        if !["boat", "zeppelin", "airship"].contains(&kind) {
            continue;
        }
        let (Some(x), Some(y)) = (f(&at(&dock, 4)), f(&at(&dock, 5))) else { continue };
        let Some((u, v)) = to_world.place(&at(&dock, 3), x, y) else { continue };
        let dest = match (f(&at(&dock, 7)), f(&at(&dock, 8))) {
            (Some(tx), Some(ty)) => to_world.place(&at(&dock, 6), tx, ty),
            _ => None,
        };
        docks.push(json!({"kind": kind, "route": at(&dock, 2).as_str().unwrap_or(""), "u": r5(u), "v": r5(v),
                          "to": dest.map(|(a, b)| json!([r5(a), r5(b)])).unwrap_or(Value::Null),
                          "faction": field(&dock, "faction").cloned().unwrap_or(Value::Null)}));
    }
    let mut sailings = Vec::new();
    for sail in positional(field(data, "sailings").unwrap_or(&Value::Null)) {
        let kind = at(&sail, 1);
        let kind = kind.as_str().unwrap_or("");
        if kind != "boat" && kind != "zeppelin" {
            continue;
        }
        let route = at(&sail, 2).as_str().unwrap_or("").to_string();
        let way = sailing_way(&positional(&at(&sail, 3)), to_world);
        if way.len() >= 2 {
            let faction = if kind == "zeppelin" { "Horde" } else if NEUTRAL_ROUTES.contains(&route.as_str()) { "Neutral" } else { "Alliance" };
            sailings.push(json!({"kind": kind, "route": route, "faction": faction, "pts": flat(&way)}));
        }
    }
    (docks, sailings)
}

fn dungeons(data: &Value, to_world: &YardsToWorld) -> Vec<Value> {
    let empty = json!({});
    let bosses = field(data, "bosses").unwrap_or(&empty);
    let mut out = Vec::new();
    for group in positional(field(data, "instances").unwrap_or(&Value::Null)) {
        let mut entry = serde_json::Map::new();
        if let Some(zone_map) = field(&group, "map").filter(|m| !m.is_null()) {
            entry.insert("map".into(), zone_map.clone());
            entry.insert("x".into(), at(&group, 2));
            entry.insert("y".into(), at(&group, 3));
        } else {
            let (Some(x), Some(y)) = (f(&at(&group, 2)), f(&at(&group, 3))) else { continue };
            let Some((u, v)) = to_world.place(&at(&group, 1), x, y) else { continue };
            entry.insert("u".into(), json!(r5(u)));
            entry.insert("v".into(), json!(r5(v)));
        }
        let mut insts = Vec::new();
        for inst in positional(&at(&group, 4)) {
            let id = at(&inst, 1).as_i64().unwrap_or(0);
            let lo = at(&inst, 4);
            let hi = at(&inst, 5);
            let boss_list: Vec<Value> = match bosses {
                Value::Object(m) => positional(m.get(&id.to_string()).unwrap_or(&Value::Null)),
                _ => vec![],
            }
            .into_iter()
            .filter(Value::is_string)
            .collect();
            insts.push(json!({"id": id, "name": at(&inst, 2).as_str().unwrap_or(""),
                              "raid": at(&inst, 3).as_bool().unwrap_or(false),
                              "min": lo, "max": if hi.is_null() { lo.clone() } else { hi }, "bosses": boss_list}));
        }
        if !insts.is_empty() {
            entry.insert("insts".into(), Value::Array(insts));
            out.push(Value::Object(entry));
        }
    }
    out
}

/// Everything the page draws over the world besides the art, in world coordinates (0..1).
pub fn build_places(flight_data: &Value, place_data: &Value, to_world: &YardsToWorld) -> Value {
    let (docks, sailings) = if place_data.is_null() { (vec![], vec![]) } else { travel(place_data, to_world) };
    json!({
        "flights": if flight_data.is_null() { json!({"nodes": [], "paths": []}) } else { flights(flight_data, to_world) },
        "docks": docks,
        "sailings": sailings,
        "dungeons": if place_data.is_null() { vec![] } else { dungeons(place_data, to_world) },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn corners(top: f64, left: f64, h: f64, w: f64, continent: i64) -> Value {
        let at = |mx: f64, my: f64| json!([top - my * h, left - mx * w]);
        json!({"continent": continent, "p00": at(0.0, 0.0), "p10": at(1.0, 0.0), "p01": at(0.0, 1.0)})
    }

    #[test]
    fn yards_to_world() {
        let yards = json!({"1415": corners(10000.0, 20000.0, 20000.0, 30000.0, 0)});
        let rects = HashMap::from([("1415".to_string(), [0.5, 1.0, 0.0, 0.5])]);
        let w = YardsToWorld::new(&yards, &rects);
        let (u, v) = w.place(&json!(0), 10000.0, 20000.0).unwrap();
        assert!((u - 0.5).abs() < 1e-9 && v.abs() < 1e-9);
        let (u, v) = w.place(&json!(0), -10000.0, -10000.0).unwrap();
        assert!((u - 1.0).abs() < 1e-9 && (v - 0.5).abs() < 1e-9);
        assert!(w.place(&json!(5), 0.0, 0.0).is_none());
    }
}
