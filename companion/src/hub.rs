//! The latest game state, turned into what the web page needs, and its listeners.

use crate::icons::PinIcons;
use crate::maps::{num, MapImages};
use crate::openmap::WorldImages;
use crate::shade::ZoneShades;
use serde_json::{json, Map, Value};
use std::sync::{Arc, Mutex};
use tokio::sync::broadcast;

pub struct Hub {
    pub maps: Arc<MapImages>,
    pub world: Arc<WorldImages>,
    pub icons: Arc<PinIcons>,
    pub shades: Arc<ZoneShades>,
    state: Mutex<State>,
    pub tx: broadcast::Sender<String>,
}

struct State {
    snapshot: Map<String, Value>,
    raw: Vec<(String, Value)>, // what the image descriptions were made from, to remake them
    zones: Value,
}

impl Hub {
    pub fn new(maps: Arc<MapImages>, world: Arc<WorldImages>, icons: Arc<PinIcons>, shades: Arc<ZoneShades>) -> Hub {
        let mut snapshot = Map::new();
        for (k, v) in [("session", Value::Null), ("map", Value::Null), ("world", Value::Null), ("zones", json!({})),
                       ("travel", Value::Null), ("pins", Value::Null), ("explored", json!({})), ("state", Value::Null),
                       ("group", json!([])), ("roster", json!({})), ("status", json!({}))] {
            snapshot.insert(k.into(), v);
        }
        let (tx, _) = broadcast::channel(256);
        Hub { maps, world, icons, shades, state: Mutex::new(State { snapshot, raw: vec![], zones: json!({}) }), tx }
    }

    fn keep_raw(state: &mut State, name: &str, data: &Value) {
        match state.raw.iter_mut().find(|(n, _)| n == name) {
            Some(entry) => entry.1 = data.clone(),
            None => state.raw.push((name.to_string(), data.clone())),
        }
    }

    pub fn publish(&self, name: &str, data: Value) {
        let mut queue = vec![(name.to_string(), data)];
        while let Some((name, data)) = queue.pop() {
            let mut state = self.state.lock().unwrap();
            let described = match name.as_str() {
                "map" => {
                    Self::keep_raw(&mut state, &name, &data);
                    self.maps.register(&data)
                }
                "world" => {
                    Self::keep_raw(&mut state, &name, &data);
                    self.world.register(&data)
                }
                "pins" => {
                    Self::keep_raw(&mut state, &name, &data);
                    self.icons.describe(&data)
                }
                "explored" => {
                    Self::keep_raw(&mut state, &name, &data);
                    let world = &state.snapshot["world"];
                    let size = if world.is_null() { None } else { Some((num(&world["width"]), num(&world["height"]))) };
                    self.shades.register(&data, &state.zones, size, self.world.openmap().as_ref())
                }
                "zones" => {
                    state.zones = data.clone();
                    data
                }
                _ => data,
            };
            if state.snapshot.get(&name) == Some(&described) {
                continue;
            }
            state.snapshot.insert(name.clone(), described.clone());
            let _ = self.tx.send(json!({"type": name, "data": described}).to_string());
            if name == "zones" || name == "world" {
                // the shades need the zones' rects and the world's size
                if let Some((_, raw)) = state.raw.iter().find(|(n, _)| n == "explored") {
                    queue.push(("explored".into(), raw.clone()));
                }
            }
        }
    }

    /// Remake the descriptions, e.g. after game data or OpenMap finished loading.
    pub fn refresh(&self) {
        let raw = self.state.lock().unwrap().raw.clone();
        for (name, data) in raw {
            self.publish(&name, data);
        }
    }

    pub fn snapshot(&self) -> Value {
        Value::Object(self.state.lock().unwrap().snapshot.clone())
    }
}
