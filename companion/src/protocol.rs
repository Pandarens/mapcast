//! Decoder for the MapCast pixel protocol (see docs/PROTOCOL.md).

use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, HashMap};
use std::time::Instant;

pub const REC_STATE: u8 = 0x01;
pub const REC_GROUP: u8 = 0x02;
pub const REC_WORLD: u8 = 0x03;
pub const REC_BULK: u8 = 0x10;
pub const REC_SESSION: u8 = 0x20;
pub const REC_MAPINFO: u8 = 0x21;
pub const REC_EXPLORED: u8 = 0x22;
pub const REC_ROSTER: u8 = 0x23;
pub const REC_MAPRECT: u8 = 0x24;
pub const REC_WORLDINFO: u8 = 0x25;
pub const REC_CONTYARDS: u8 = 0x26;
pub const REC_TAXINODES: u8 = 0x27;
pub const REC_INSTNAMES: u8 = 0x28;
pub const REC_ZONEEXPLORED: u8 = 0x29;
pub const REC_PINSOURCES: u8 = 0x30;
pub const REC_PINICONS: u8 = 0x31;
pub const REC_PINTEXTS: u8 = 0x32;
pub const REC_PINS: u8 = 0x33;

pub const CH_WORLD: u8 = 4;
pub const CH_ZONES: u8 = 5;
pub const CH_TRAVEL: u8 = 6;
pub const CH_EXPLORED: u8 = 7;
pub const CH_PINS: u8 = 10;

const NO_INDEX: u16 = 0xFFFF;
const MARKER_TOLERANCE: i32 = 40;
const MAX_CELL_PX: usize = 8;
const SHADES: usize = 16; // a colour channel carries 4 bits as one of 16 shades
const RAMP_AT: usize = 2; // cells 2..18: the 16 shades in order, to read them back as they arrive
const GEOMETRY_AT: usize = 18; // cells 18, 19: columns (3 half-bytes) and rows - 1
const HEADER_CELLS: usize = 20;
const MIN_SHADE_GAP: i32 = 4; // neighbouring shades closer than this cannot be told apart
const STATE_FLAGS: [&str; 8] = ["hasPos", "hasFacing", "combat", "dead", "mounted", "flying", "instance", "resting"];

pub type Event = (String, Value);

/// An RGB image, row-major: what a capture of the strip region gives.
#[derive(Clone)]
pub struct Frame {
    pub width: usize,
    pub height: usize,
    pub rgb: Vec<u8>,
}

impl Frame {
    pub fn new(width: usize, height: usize) -> Frame {
        Frame { width, height, rgb: vec![0; width * height * 3] }
    }

    pub fn pixel(&self, x: usize, y: usize) -> [u8; 3] {
        let i = (y * self.width + x) * 3;
        [self.rgb[i], self.rgb[i + 1], self.rgb[i + 2]]
    }
}

/// CRC16-CCITT-FALSE (poly 0x1021, init 0xFFFF).
pub fn crc16(data: &[u8]) -> u16 {
    let mut crc: u16 = 0xFFFF;
    for &b in data {
        crc ^= (b as u16) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 { (crc << 1) ^ 0x1021 } else { crc << 1 };
        }
    }
    crc
}

pub struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    pub fn new(data: &'a [u8]) -> Reader<'a> {
        Reader { data, pos: 0 }
    }
    fn byte(&mut self) -> u8 {
        let v = self.data.get(self.pos).copied().unwrap_or(0);
        self.pos += 1;
        v
    }
    pub fn u8(&mut self) -> u8 {
        self.byte()
    }
    pub fn u16(&mut self) -> u16 {
        self.byte() as u16 | (self.byte() as u16) << 8
    }
    pub fn u32(&mut self) -> u32 {
        self.u16() as u32 | (self.u16() as u32) << 16
    }
    pub fn i32(&mut self) -> i32 {
        self.u32() as i32
    }
    pub fn left(&self) -> usize {
        self.data.len().saturating_sub(self.pos)
    }
    pub fn str(&mut self) -> String {
        let n = self.u8() as usize;
        let end = (self.pos + n).min(self.data.len());
        let s = String::from_utf8_lossy(&self.data[self.pos.min(end)..end]).into_owned();
        self.pos += n;
        s
    }
    pub fn rest(&mut self) -> &'a [u8] {
        let v = &self.data[self.pos.min(self.data.len())..];
        self.pos = self.data.len();
        v
    }
}

pub fn iter_records(buf: &[u8]) -> Vec<(u8, &[u8])> {
    let mut out = Vec::new();
    let mut pos = 0;
    while pos + 3 <= buf.len() {
        let rtype = buf[pos];
        let len = buf[pos + 1] as usize | (buf[pos + 2] as usize) << 8;
        if pos + 3 + len > buf.len() {
            break;
        }
        out.push((rtype, &buf[pos + 3..pos + 3 + len]));
        pos += 3 + len;
    }
    out
}

fn near(p: [u8; 3], c: [i32; 3]) -> bool {
    (0..3).all(|i| (p[i] as i32 - c[i]).abs() <= MARKER_TOLERANCE)
}

const MAGENTA: [i32; 3] = [255, 0, 255];
const GREEN: [i32; 3] = [0, 255, 0];

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Geometry {
    pub cell: usize,
    pub cols: usize,
    pub rows: usize,
}

impl Geometry {
    pub fn size_px(&self) -> (usize, usize) {
        (self.cols * self.cell, self.rows * self.cell)
    }
}

/// The 16 shades of each colour channel as the capture shows them. HDR, gamma or a colour
/// profile shift them; values are read as the nearest shade, so the data survives.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Shades {
    levels: [[u8; SHADES]; 3],
}

impl Shades {
    /// The 4-bit value a channel's captured brightness stands for.
    fn value(&self, channel: usize, seen: u8) -> u8 {
        let mut best = 0;
        let mut best_gap = i32::MAX;
        for (k, level) in self.levels[channel].iter().enumerate() {
            let gap = (*level as i32 - seen as i32).abs();
            if gap < best_gap {
                best_gap = gap;
                best = k;
            }
        }
        best as u8
    }

    fn cell(&self, p: [u8; 3]) -> [u8; 3] {
        [self.value(0, p[0]), self.value(1, p[1]), self.value(2, p[2])]
    }
}

/// Locate the strip at the image's top-left. Returns its geometry and shades, or the problem.
pub fn find_geometry(img: &Frame) -> Result<(Geometry, Shades), &'static str> {
    if img.height < 1 || img.width < 3 {
        return Err("capture too small");
    }
    if !near(img.pixel(0, 0), MAGENTA) {
        return Err("no strip in the top-left corner");
    }
    let mut cell = 1;
    while cell < (MAX_CELL_PX + 1).min(img.width) && near(img.pixel(cell, 0), MAGENTA) {
        cell += 1;
    }
    if cell > MAX_CELL_PX || 3 * cell > img.width || !(0..cell).all(|i| near(img.pixel(cell + i, 0), GREEN)) {
        return Err("strip markers not recognized");
    }
    let c = cell / 2;
    if img.height <= c || img.width < HEADER_CELLS * cell {
        return Err("capture too small");
    }
    let at = |i: usize| img.pixel(i * cell + c, c);
    let mut levels = [[0u8; SHADES]; 3];
    for k in 0..SHADES {
        let p = at(RAMP_AT + k);
        for channel in 0..3 {
            levels[channel][k] = p[channel];
        }
    }
    let told_apart = levels.iter().all(|l| l.windows(2).all(|w| w[1] as i32 - w[0] as i32 >= MIN_SHADE_GAP));
    if !told_apart {
        return Err("colors are distorted: the strip's shades cannot be told apart");
    }
    let shades = Shades { levels };
    let g = shades.cell(at(GEOMETRY_AT));
    let cols = g[0] as usize | (g[1] as usize) << 4 | (g[2] as usize) << 8;
    let rows = shades.cell(at(GEOMETRY_AT + 1))[0] as usize + 1;
    if cols < HEADER_CELLS {
        return Err("bad strip geometry");
    }
    Ok((Geometry { cell, cols, rows }, shades))
}

/// The page's bytes: three half-bytes a cell after the header, a byte's low half first.
pub fn read_page_bytes(img: &Frame, geo: &Geometry, shades: &Shades) -> Option<Vec<u8>> {
    let (w, h) = geo.size_px();
    if img.width < w || img.height < h {
        return None;
    }
    let c = geo.cell / 2;
    let cells = geo.cols * geo.rows;
    let mut out = Vec::with_capacity((cells - HEADER_CELLS) * 3 / 2);
    let mut low: Option<u8> = None;
    for i in HEADER_CELLS..cells {
        let p = img.pixel((i % geo.cols) * geo.cell + c, (i / geo.cols) * geo.cell + c);
        for half in shades.cell(p) {
            match low.take() {
                None => low = Some(half),
                Some(l) => out.push(l | half << 4),
            }
        }
    }
    Some(out)
}

pub struct Page {
    pub seq: u8,
    pub crc: u16,
    pub payload: Vec<u8>,
}

pub fn parse_page(data: &[u8]) -> Option<Page> {
    if data.len() < 6 || data[0] != 1 {
        return None;
    }
    let len = data[2] as usize | (data[3] as usize) << 8;
    let end = 4 + len;
    if end + 2 > data.len() {
        return None;
    }
    let crc = data[end] as u16 | (data[end + 1] as u16) << 8;
    if crc16(&data[..end]) != crc {
        return None;
    }
    Some(Page { seq: data[1], crc, payload: data[4..end].to_vec() })
}

// ---------------------------------------------------------------------------
// Record bodies

fn norm(v: u16) -> f64 {
    v as f64 / 65535.0
}

/// World coordinates are u32 = (v + 0.5) * 2^24.
pub fn world_coord(v: u32) -> f64 {
    v as f64 / 16777216.0 - 0.5
}

pub fn parse_state(body: &[u8]) -> Map<String, Value> {
    let mut r = Reader::new(body);
    let (map_id, x, y, facing, flags, time_ms) = (r.u32(), r.u16(), r.u16(), r.u16(), r.u8(), r.u32());
    let mut state = Map::new();
    state.insert("mapID".into(), json!(map_id));
    state.insert("x".into(), json!(norm(x)));
    state.insert("y".into(), json!(norm(y)));
    state.insert("facing".into(), json!(facing as f64 / 65536.0 * 2.0 * std::f64::consts::PI));
    state.insert("timeMs".into(), json!(time_ms));
    for (bit, name) in STATE_FLAGS.iter().enumerate() {
        state.insert((*name).into(), json!(flags & (1 << bit) != 0));
    }
    state
}

pub fn parse_maprect(body: &[u8]) -> (u32, [f64; 4]) {
    let mut r = Reader::new(body);
    let id = r.u32();
    let rect = [world_coord(r.u32()), world_coord(r.u32()), world_coord(r.u32()), world_coord(r.u32())];
    (id, rect)
}

pub fn parse_group(body: &[u8]) -> Value {
    let mut r = Reader::new(body);
    let n = r.u8();
    let members: Vec<Value> = (0..n)
        .map(|_| {
            let (unit, class_id, x, y, flags) = (r.u8(), r.u8(), r.u16(), r.u16(), r.u8());
            json!({"unit": unit, "classID": class_id, "x": norm(x), "y": norm(y),
                   "dead": flags & 1 != 0, "offline": flags & 2 != 0})
        })
        .collect();
    Value::Array(members)
}

pub fn parse_session(body: &[u8]) -> Value {
    let mut r = Reader::new(body);
    let mut s = json!({"addonVersion": r.u8(), "interface": r.u32(), "locale": r.str(), "player": r.str(),
                       "realm": r.str(), "classID": r.u8(), "faction": null, "level": 0});
    if r.left() >= 2 {
        let faction = match r.u8() {
            1 => json!("Alliance"),
            2 => json!("Horde"),
            _ => Value::Null,
        };
        s["faction"] = faction;
        s["level"] = json!(r.u8());
    }
    s
}

pub fn parse_mapinfo(body: &[u8]) -> Value {
    let mut r = Reader::new(body);
    let (map_id, map_type, parent, name) = (r.u32(), r.u8(), r.u32(), r.str());
    let layers: Vec<Value> = (0..r.u8())
        .map(|_| {
            let (w, h, tw, th) = (r.u16(), r.u16(), r.u16(), r.u16());
            let tiles: Vec<u32> = (0..r.u16()).map(|_| r.u32()).collect();
            json!({"w": w, "h": h, "tileW": tw, "tileH": th, "tiles": tiles})
        })
        .collect();
    json!({"mapID": map_id, "mapType": map_type, "parentMapID": parent, "name": name, "layers": layers})
}

pub fn parse_explored(body: &[u8]) -> Value {
    let mut r = Reader::new(body);
    let _map_id = r.u32();
    let overlays: Vec<Value> = (0..r.u16())
        .map(|_| {
            let (w, h, x, y, flags) = (r.u16(), r.u16(), r.u16(), r.u16(), r.u8());
            let files: Vec<u32> = (0..r.u8()).map(|_| r.u32()).collect();
            json!({"w": w, "h": h, "x": x, "y": y, "flags": flags, "files": files})
        })
        .collect();
    Value::Array(overlays)
}

pub fn parse_roster(body: &[u8]) -> Value {
    let mut r = Reader::new(body);
    let mut roster = Map::new();
    for _ in 0..r.u8() {
        let (unit, class_id, name) = (r.u8(), r.u8(), r.str());
        roster.insert(unit.to_string(), json!({"classID": class_id, "name": name}));
    }
    Value::Object(roster)
}

/// Where a continent's map lies in the game's world yards: its corners (0, 0), (1, 0), (0, 1).
pub fn parse_contyards(body: &[u8]) -> (u32, Value) {
    let mut r = Reader::new(body);
    let (map_id, continent) = (r.u32(), r.u16());
    let mut corners = Vec::new();
    for _ in 0..3 {
        corners.push(json!([r.i32() as f64 / 100.0, r.i32() as f64 / 100.0]));
    }
    (map_id, json!({"continent": continent, "p00": corners[0], "p10": corners[1], "p01": corners[2]}))
}

pub fn parse_zone_explored(body: &[u8]) -> Value {
    let mut r = Reader::new(body);
    let mut zones = Map::new();
    for _ in 0..r.u16() {
        let (map_id, art, lw, lh) = (r.u32(), r.u32(), r.u16(), r.u16());
        let pieces: Vec<Value> = (0..r.u8()).map(|_| json!([r.u16(), r.u16(), r.u16(), r.u16()])).collect();
        zones.insert(map_id.to_string(), json!({"art": art, "layer": [lw, lh], "pieces": pieces}));
    }
    Value::Object(zones)
}

/// The pins channel: map addons' pins near the player, their icons and texts.
pub fn parse_pins_blob(blob: &[u8]) -> Value {
    let (mut sources, mut icons, mut texts) = (Vec::new(), Vec::new(), Vec::new());
    let mut raw = Vec::new();
    for (rtype, body) in iter_records(blob) {
        let mut r = Reader::new(body);
        match rtype {
            REC_PINSOURCES => sources = (0..r.u8()).map(|_| r.str()).collect(),
            REC_PINICONS => {
                for _ in 0..r.u16() {
                    let (file_id, path) = (r.u32(), r.str());
                    let coords: Vec<f64> = (0..4).map(|_| r.u16() as f64 / 65535.0).collect();
                    let color: Vec<u8> = (0..4).map(|_| r.u8()).collect();
                    icons.push(json!({"fileID": if file_id > 0 { json!(file_id) } else { Value::Null },
                                      "path": if path.is_empty() { Value::Null } else { json!(path) },
                                      "coords": coords, "color": color}));
                }
            }
            REC_PINTEXTS => texts = (0..r.u16()).map(|_| r.str()).collect::<Vec<String>>(),
            REC_PINS => {
                for _ in 0..r.u16() {
                    raw.push((r.u8(), world_coord(r.u32()), world_coord(r.u32()), r.u16(), r.u8(), r.u16(), r.u16(),
                              r.u16(), r.u8()));
                }
            }
            _ => {}
        }
    }
    let text = |i: u16| -> Value {
        if i != NO_INDEX && (i as usize) < texts.len() { json!(texts[i as usize]) } else { Value::Null }
    };
    let pins: Vec<Value> = raw
        .into_iter()
        .filter(|p| (p.0 as usize) < sources.len())
        .map(|(source, u, v, icon, size, title, sub, label, flags)| {
            json!({"source": source, "u": u, "v": v,
                   "icon": if (icon as usize) < icons.len() { json!(icon) } else { Value::Null },
                   "size": size, "title": text(title), "sub": text(sub), "label": text(label),
                   "active": flags & 1 != 0})
        })
        .collect();
    json!({"sources": sources, "icons": icons, "pins": pins})
}

// ---------------------------------------------------------------------------
// Stateful decoder

struct Channel {
    gen: u16,
    count: u16,
    chunks: HashMap<u16, Vec<u8>>,
    done: bool,
}

/// Turns captured frames into events: (name, data).
pub struct Decoder {
    pub geometry: Option<Geometry>,
    pub problem: String,
    last_page: Option<(u8, u16)>,
    channels: HashMap<u8, Channel>,
    pub pages: u64,
    pub bad_pages: u64,
    pub last_page_time: Option<Instant>,
}

impl Default for Decoder {
    fn default() -> Self {
        Decoder::new()
    }
}

impl Decoder {
    pub fn new() -> Decoder {
        Decoder {
            geometry: None,
            problem: "waiting for the first frame".into(),
            last_page: None,
            channels: HashMap::new(),
            pages: 0,
            bad_pages: 0,
            last_page_time: None,
        }
    }

    pub fn feed(&mut self, img: &Frame) -> Vec<Event> {
        let (geo, shades) = match find_geometry(img) {
            Ok(found) => found,
            Err(problem) => {
                self.geometry = None;
                self.problem = problem.into();
                return vec![];
            }
        };
        self.geometry = Some(geo);
        let data = match read_page_bytes(img, &geo, &shades) {
            Some(d) => d,
            None => {
                self.problem = "capture region smaller than the strip".into();
                return vec![];
            }
        };
        let page = match parse_page(&data) {
            Some(p) => p,
            None => {
                if data.first() != Some(&1) {
                    self.problem = "the addon has not sent data yet".into();
                } else {
                    self.bad_pages += 1;
                    self.problem = "page checksum failed".into();
                }
                return vec![];
            }
        };
        self.problem.clear();
        let key = (page.seq, page.crc);
        if self.last_page == Some(key) {
            return vec![];
        }
        self.last_page = Some(key);
        self.pages += 1;
        self.last_page_time = Some(Instant::now());
        self.handle_payload(&page.payload)
    }

    pub fn handle_payload(&mut self, payload: &[u8]) -> Vec<Event> {
        let mut events = Vec::new();
        let mut has_group = false;
        let mut state: Option<Map<String, Value>> = None;
        let mut world: Option<(f64, f64)> = None;
        let mut state_at = None;
        for (rtype, body) in iter_records(payload) {
            match rtype {
                REC_STATE => {
                    state = Some(parse_state(body));
                    state_at = Some(events.len());
                    events.push(("state".to_string(), Value::Null));
                }
                REC_WORLD => {
                    let mut r = Reader::new(body);
                    world = Some((world_coord(r.u32()), world_coord(r.u32())));
                }
                REC_GROUP => {
                    has_group = true;
                    events.push(("group".into(), parse_group(body)));
                }
                REC_BULK => {
                    if let Some((channel, blob)) = self.on_bulk(body) {
                        events.extend(self.handle_blob(channel, &blob));
                    }
                }
                _ => {}
            }
        }
        if let (Some(mut s), Some(i)) = (state, state_at) {
            s.insert("hasWorld".into(), json!(world.is_some()));
            let (u, v) = world.unwrap_or((0.0, 0.0));
            s.insert("u".into(), json!(u));
            s.insert("v".into(), json!(v));
            events[i].1 = Value::Object(s);
        }
        if !has_group {
            events.push(("group".into(), json!([])));
        }
        events
    }

    fn on_bulk(&mut self, body: &[u8]) -> Option<(u8, Vec<u8>)> {
        let mut r = Reader::new(body);
        let (channel, gen, index, count) = (r.u8(), r.u16(), r.u16(), r.u16());
        let data = r.rest().to_vec();
        let mut fresh = true;
        if let Some(ch) = self.channels.get(&channel) {
            if ch.gen == gen && ch.count == count {
                // same gen but different content: the addon restarted
                fresh = ch.chunks.get(&index).is_some_and(|old| *old != data);
            }
        }
        if fresh {
            self.channels.insert(channel, Channel { gen, count, chunks: HashMap::new(), done: false });
        }
        let ch = self.channels.get_mut(&channel)?;
        if ch.done {
            return None;
        }
        ch.chunks.insert(index, data);
        if ch.chunks.len() < count as usize {
            return None;
        }
        ch.done = true;
        let mut blob = Vec::new();
        for i in 0..count {
            blob.extend_from_slice(ch.chunks.get(&i)?);
        }
        Some((channel, blob))
    }

    fn handle_blob(&self, channel: u8, blob: &[u8]) -> Vec<Event> {
        match channel {
            CH_WORLD => return vec![("world".into(), parse_world_blob(blob))],
            CH_ZONES => return vec![("zones".into(), parse_zones_blob(blob))],
            CH_TRAVEL => return vec![("travel".into(), parse_travel_blob(blob))],
            CH_PINS => return vec![("pins".into(), parse_pins_blob(blob))],
            CH_EXPLORED => {
                let mut explored = json!({});
                for (rtype, body) in iter_records(blob) {
                    if rtype == REC_ZONEEXPLORED {
                        explored = parse_zone_explored(body);
                    }
                }
                return vec![("explored".into(), explored)];
            }
            _ => {}
        }
        let mut events = Vec::new();
        let mut map_event = Map::new();
        for (rtype, body) in iter_records(blob) {
            match rtype {
                REC_SESSION => events.push(("session".into(), parse_session(body))),
                REC_MAPINFO => {
                    map_event.insert("info".into(), parse_mapinfo(body));
                }
                REC_EXPLORED => {
                    map_event.insert("explored".into(), parse_explored(body));
                }
                REC_MAPRECT => {
                    map_event.insert("rect".into(), json!(parse_maprect(body).1));
                }
                REC_ROSTER => events.push(("roster".into(), parse_roster(body))),
                _ => {}
            }
        }
        if map_event.contains_key("info") {
            map_event.entry("explored").or_insert(json!([]));
            map_event.entry("rect").or_insert(Value::Null);
            events.push(("map".into(), Value::Object(map_event)));
        }
        events
    }
}

/// The world channel: Azeroth's map and each continent's map with its rect on it.
pub fn parse_world_blob(blob: &[u8]) -> Value {
    let mut world = json!({"openmap": false, "worldMapID": 0, "openmapVersion": "", "maps": [], "yards": {}});
    let mut rects: BTreeMap<u32, [f64; 4]> = BTreeMap::new();
    let mut maps = Vec::new();
    for (rtype, body) in iter_records(blob) {
        match rtype {
            REC_WORLDINFO => {
                let mut r = Reader::new(body);
                let (flags, world_map, version) = (r.u8(), r.u32(), r.str());
                world["openmap"] = json!(flags & 1 != 0);
                world["worldMapID"] = json!(world_map);
                world["openmapVersion"] = json!(version);
            }
            REC_MAPINFO => maps.push(parse_mapinfo(body)),
            REC_MAPRECT => {
                let (id, rect) = parse_maprect(body);
                rects.insert(id, rect);
            }
            REC_CONTYARDS => {
                let (id, yards) = parse_contyards(body);
                world["yards"][id.to_string()] = yards;
            }
            _ => {}
        }
    }
    let world_id = world["worldMapID"].as_u64().unwrap_or(0) as u32;
    for info in maps.iter_mut() {
        let id = info["mapID"].as_u64().unwrap_or(0) as u32;
        info["rect"] = if id == world_id {
            json!([0.0, 1.0, 0.0, 1.0])
        } else {
            rects.get(&id).map(|r| json!(r)).unwrap_or(Value::Null)
        };
    }
    world["maps"] = Value::Array(maps);
    world
}

/// The zones channel: each zone of each continent, its name and its rect on the world.
pub fn parse_zones_blob(blob: &[u8]) -> Value {
    let mut names: BTreeMap<u32, Value> = BTreeMap::new();
    let mut rects: BTreeMap<u32, [f64; 4]> = BTreeMap::new();
    for (rtype, body) in iter_records(blob) {
        if rtype == REC_MAPINFO {
            let info = parse_mapinfo(body);
            let id = info["mapID"].as_u64().unwrap_or(0) as u32;
            names.insert(id, json!({"name": info["name"], "parentMapID": info["parentMapID"]}));
        } else if rtype == REC_MAPRECT {
            let (id, rect) = parse_maprect(body);
            rects.insert(id, rect);
        }
    }
    let mut zones = Map::new();
    for (id, mut zone) in names {
        if let Some(rect) = rects.get(&id) {
            zone["rect"] = json!(rect);
            zones.insert(id.to_string(), zone);
        }
    }
    Value::Object(zones)
}

/// The travel channel: flight masters in the game's language and which are learned, dungeon names.
pub fn parse_travel_blob(blob: &[u8]) -> Value {
    let mut travel = json!({"nodes": {}, "instances": {}});
    for (rtype, body) in iter_records(blob) {
        let mut r = Reader::new(body);
        if rtype == REC_TAXINODES {
            let mut nodes = Map::new();
            for _ in 0..r.u16() {
                let (id, flags, name) = (r.u16(), r.u8(), r.str());
                nodes.insert(id.to_string(), json!({"name": name, "known": flags & 1 != 0}));
            }
            travel["nodes"] = Value::Object(nodes);
        } else if rtype == REC_INSTNAMES {
            let mut names = Map::new();
            for _ in 0..r.u8() {
                let (id, name) = (r.u16(), r.str());
                names.insert(id.to_string(), json!(name));
            }
            travel["instances"] = Value::Object(names);
        }
    }
    travel
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc_matches_ccitt_false() {
        assert_eq!(crc16(b"123456789"), 0x29B1);
    }

    #[test]
    fn world_coordinates_round_trip() {
        let v = ((0.123456_f64 + 0.5) * 16777216.0).round() as u32;
        assert!((world_coord(v) - 0.123456).abs() < 1e-7);
    }
}
