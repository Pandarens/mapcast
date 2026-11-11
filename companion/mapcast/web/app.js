"use strict";

const CLASS_COLORS = {
  1: "#C69B6D", 2: "#F48CBA", 3: "#AAD372", 4: "#FFF468", 5: "#FFFFFF", 6: "#C41E3A", 7: "#0070DD",
  8: "#3FC7EB", 9: "#8788EE", 10: "#00FF98", 11: "#FF7C0A", 12: "#A330C9", 13: "#33937F",
};

const PROBLEMS = {
  "WoW window not found": "Окно WoW не найдено. Запусти игру в оконном режиме или «полноэкранном в окне».",
  "no strip in the top-left corner": "Не вижу полоску аддона в левом верхнем углу игры. Проверь, что аддон MapCast включён (/mapcast on) и угол окна ничем не закрыт.",
  "strip markers not recognized": "Полоска аддона читается неправильно. Попробуй /mapcast cell 4.",
  "colors are distorted: set in-game Gamma/Brightness/Contrast to defaults": "Цвета искажены: верни в игре гамму, яркость и контраст к значениям по умолчанию.",
  "the addon has not sent data yet": "Аддон запускается…",
  "page checksum failed": "Ошибки при чтении полоски. Проверь гамму и яркость в игре.",
  "no new data from the addon": "Нет новых данных от аддона. Игра свёрнута или окно перекрыто?",
  "capture region smaller than the strip": "Окно игры слишком маленькое для полоски.",
};

// The seamless world, faded in by zoom the way OpenMap does (Map/Stitch.lua). Its zoom 1 is the whole
// world across a map about 1000 px wide, so here the zoom is CSS px per world unit (x W / 1000).
const CONT_IN = [1.3, 1.7];
const DETAIL_HINT = [1.8, 3, 0.3];
const DETAIL_FADE = [3, 4.5];
const LABELS_OUT = [3, 4];
const DEFAULT_WORLD_PX = 5; // CSS px per world unit when the world view first opens: zones are in
const MAX_WORLD_PX = 24; // closest zoom (the zone art has 16 px per world unit)
const TILE_KEEP = 48; // fine zone tiles kept decoded (each is about 1 MB of memory)
const LAND_INK = "rgb(78,44,12)";
const LAND_GLOW = "rgba(243,189,81,0.8)";
// OpenMap's places, each kind from its own zoom (Stitch.lua's pin sets), and its route colours (Routes.lua)
const PLACES_FROM = { sailings: 1, dungeons: 1.5, docks: 2.5, flights: 2.5 };
const LABELS_FROM = 5; // dungeon names under their icons from this zoom
const ALLIANCE_ROUTE = [51, 107, 224];
const HORDE_ROUTE = [184, 33, 26];
const FLIGHT_LINE = "rgb(255,214,102)";
const FLIGHT_SHADE = "rgba(40,24,6,0.7)";
const KIND_NAMES = { boat: "Корабль", zeppelin: "Цеппелин", airship: "Дирижабль" };

const $ = (id) => document.getElementById(id);
const canvas = $("map");
const ctx = canvas.getContext("2d");

const game = {
  connected: false, status: {}, session: null, map: null, image: null, world: null,
  state: null, shown: null, group: [], roster: {}, zones: {}, travel: null, places: null,
};

const view = {
  cx: 0, cy: 0, scale: 1, rotation: 0, follow: true, headingUp: false, mode: "world",
  zooms: { world: 0, zone: 1 },
  layers: { flights: true, travel: true, dungeons: true },
};
let needsDraw = true;
let hits = []; // what the last frame drew that a tap can pick: { x, y, r, kind, item }
let lastSpace = null;

// ---------------------------------------------------------------------------
// Settings that survive reloads (per device, best effort)

function loadSettings() {
  try {
    const s = JSON.parse(localStorage.getItem("mapcast") || "{}");
    if (typeof s.follow === "boolean") view.follow = s.follow;
    if (typeof s.headingUp === "boolean") view.headingUp = s.headingUp;
    if (s.mode === "world" || s.mode === "zone") view.mode = s.mode;
    if (s.zooms && typeof s.zooms === "object") {
      for (const key of ["world", "zone"]) if (typeof s.zooms[key] === "number") view.zooms[key] = s.zooms[key];
    }
    if (s.layers && typeof s.layers === "object") {
      for (const key of Object.keys(view.layers)) if (typeof s.layers[key] === "boolean") view.layers[key] = s.layers[key];
    }
  } catch (_) { /* storage unavailable */ }
}

function saveSettings() {
  try {
    localStorage.setItem("mapcast", JSON.stringify({
      follow: view.follow, headingUp: view.headingUp, mode: view.mode, zooms: view.zooms, layers: view.layers,
    }));
  } catch (_) { /* storage unavailable */ }
}

// ---------------------------------------------------------------------------
// Spaces: the seamless world (world units, OpenMap's) or the current map (its art pixels)

function worldMode() {
  if (view.mode !== "world" || !game.world) return false;
  const st = game.state;
  // In a dungeon there is no world position: show the dungeon's own map instead.
  return !st || st.hasWorld || !game.map;
}

function spaceSize() {
  if (worldMode()) return [game.world.width, game.world.height];
  return game.map ? [game.map.width, game.map.height] : [1002, 668];
}

function fitScale() {
  const [w, h] = spaceSize();
  return Math.min(innerWidth / w, innerHeight / h);
}

function zoomKey() {
  return worldMode() ? "world" : "zone";
}

function clampZoom() {
  const key = zoomKey();
  const fit = fitScale();
  let zoom = view.zooms[key] || (key === "world" ? DEFAULT_WORLD_PX / fit : 1);
  const [lo, hi] = key === "world" ? [1, Math.max(2, MAX_WORLD_PX / fit)] : [0.5, 16];
  zoom = Math.min(hi, Math.max(lo, zoom));
  view.zooms[key] = zoom;
  view.scale = fit * zoom;
}

function screenToWorld(sx, sy) {
  const dx = sx - innerWidth / 2;
  const dy = sy - innerHeight / 2;
  const c = Math.cos(-view.rotation);
  const s = Math.sin(-view.rotation);
  return [view.cx + (dx * c - dy * s) / view.scale, view.cy + (dx * s + dy * c) / view.scale];
}

function worldToScreen(x, y) {
  const dx = (x - view.cx) * view.scale;
  const dy = (y - view.cy) * view.scale;
  const c = Math.cos(view.rotation);
  const s = Math.sin(view.rotation);
  return [innerWidth / 2 + dx * c - dy * s, innerHeight / 2 + dx * s + dy * c];
}

function visibleBox() {
  const corners = [[0, 0], [innerWidth, 0], [0, innerHeight], [innerWidth, innerHeight]].map(([x, y]) => screenToWorld(x, y));
  const xs = corners.map((p) => p[0]);
  const ys = corners.map((p) => p[1]);
  return [Math.min(...xs), Math.min(...ys), Math.max(...xs), Math.max(...ys)];
}

function zoomAt(sx, sy, factor) {
  const before = screenToWorld(sx, sy);
  view.zooms[zoomKey()] *= factor;
  clampZoom();
  if (!view.follow) {
    const after = screenToWorld(sx, sy);
    view.cx += before[0] - after[0];
    view.cy += before[1] - after[1];
  }
  saveSettings();
  needsDraw = true;
}

function resize() {
  const dpr = window.devicePixelRatio || 1;
  canvas.width = Math.round(innerWidth * dpr);
  canvas.height = Math.round(innerHeight * dpr);
  clampZoom();
  needsDraw = true;
}

// The current map's rect on the world (as the addon sends it), to move a point between spaces.
function mapToWorld(x, y) {
  const r = game.map && game.map.rect;
  if (!r || !game.world) return null;
  return [(r[0] + x * (r[1] - r[0])) * game.world.width, (r[2] + y * (r[3] - r[2])) * game.world.height];
}

function worldToMap(wx, wy) {
  const r = game.map && game.map.rect;
  if (!r || !game.world) return null;
  return [(wx / game.world.width - r[0]) / (r[1] - r[0]), (wy / game.world.height - r[2]) / (r[3] - r[2])];
}

// Called when the drawn space changes (world <-> zone, or another zone map): keep the view on the same place.
function onSpaceChange(space, previous) {
  const center = previous ? [view.cx, view.cy] : null;
  clampZoom();
  const [w, h] = spaceSize();
  let next = (!previous && playerPoint()) || [w / 2, h / 2]; // a fresh page opens on the player, followed or not
  if (center && previous.kind === "zone" && space.kind === "world" && previous.mapID === (game.map && game.map.mapID)) {
    const p = mapToWorld(center[0] / previous.w, center[1] / previous.h);
    if (p) next = p;
  } else if (center && previous.kind === "world" && space.kind === "zone") {
    const p = worldToMap(center[0], center[1]);
    if (p && p[0] > -0.5 && p[0] < 1.5 && p[1] > -0.5 && p[1] < 1.5) next = [p[0] * w, p[1] * h];
  }
  view.cx = next[0];
  view.cy = next[1];
}

// ---------------------------------------------------------------------------
// Incoming data

function loadImage(url, onload) {
  const img = new Image();
  img.onload = () => {
    onload(img);
    needsDraw = true;
  };
  img.src = url;
  return img;
}

function setMap(map) {
  if (!map) return;
  if (!game.map || game.map.mapID !== map.mapID) game.image = null; // else keep it until the new one loads
  game.map = map;
  loadImage(map.url, (img) => {
    if (game.map === map) game.image = img;
  });
}

function setWorld(world) {
  game.world = world;
  game.places = null;
  tiles.clear();
  if (!world) return;
  if (world.places && world.places.url) {
    fetch(world.places.url)
      .then((r) => (r.ok ? r.json() : null))
      .then((places) => {
        if (game.world !== world || !places) return;
        game.places = places;
        updateBar();
        needsDraw = true;
      })
      .catch(() => {});
  }
  world.images = {};
  for (const art of world.art) {
    loadImage(art.url, (img) => {
      if (game.world === world) world.images[art.mapID] = img;
    });
  }
  if (world.sea && world.sea.url) {
    loadImage(world.sea.url, (img) => {
      if (game.world !== world) return;
      world.seaImage = img;
      // past the sea image, carry on with the colour of its edge
      const probe = document.createElement("canvas");
      probe.width = probe.height = 1;
      const pctx = probe.getContext("2d");
      pctx.drawImage(img, 0, 0, 1, 1, 0, 0, 1, 1);
      world.seaFill = `rgb(${[...pctx.getImageData(0, 0, 1, 1).data.slice(0, 3)].join(",")})`;
    });
  }
  const zones = world.zones;
  if (zones) {
    // which tiles exist at each level of the pyramid (level L halves L - 1)
    zones.sets = [];
    let set = new Set(zones.tiles);
    for (let level = 0; level <= zones.maxLevel; level++) {
      zones.sets.push(set);
      const next = new Set();
      for (const key of set) next.add(((key >> 8) >> 1) * 256 + ((key & 255) >> 1));
      set = next;
    }
    // the coarse levels stay loaded: they stand in while finer tiles come
    zones.coarse = Math.max(0, zones.maxLevel - 2);
    for (let level = zones.coarse; level <= zones.maxLevel; level++) {
      for (const key of zones.sets[level]) tileImage(level, key >> 8, key & 255, true);
    }
  }
}

function setState(state) {
  game.state = state;
  if (!state || !state.hasPos) return;
  const s = game.shown;
  const jump = !s || s.mapID !== state.mapID || Math.hypot(s.x - state.x, s.y - state.y) > 0.05
    || (state.hasWorld && Math.hypot(s.u - state.u, s.v - state.v) > 0.01);
  if (jump) game.shown = { x: state.x, y: state.y, u: state.u, v: state.v, facing: state.facing, mapID: state.mapID };
}

function apply(type, data) {
  switch (type) {
    case "map": setMap(data); break;
    case "world": setWorld(data); break;
    case "state": setState(data); break;
    case "group": game.group = data || []; break;
    case "roster": game.roster = data || {}; break;
    case "session": game.session = data; break;
    case "zones": game.zones = data || {}; break;
    case "travel": game.travel = data; break;
    case "status": game.status = data || {}; break;
    default: return;
  }
  updateBar();
  needsDraw = true;
}

function handle(message) {
  if (message.type === "snapshot") {
    for (const key of ["session", "map", "world", "zones", "travel", "roster", "group", "state", "status"]) {
      apply(key, message.data[key]);
    }
  } else {
    apply(message.type, message.data);
  }
}

let retryDelay = 500;

function connect() {
  const token = new URLSearchParams(location.search).get("t");
  const scheme = location.protocol === "https:" ? "wss://" : "ws://";
  const ws = new WebSocket(scheme + location.host + "/ws" + (token ? "?t=" + encodeURIComponent(token) : ""));
  ws.onopen = () => {
    game.connected = true;
    retryDelay = 500;
    updateBar();
  };
  ws.onmessage = (e) => handle(JSON.parse(e.data));
  ws.onclose = () => {
    game.connected = false;
    updateBar();
    setTimeout(connect, retryDelay);
    retryDelay = Math.min(retryDelay * 2, 5000);
  };
}

// ---------------------------------------------------------------------------
// UI chrome

function updateBar() {
  const st = game.state;
  const streaming = game.connected && game.status.streaming;
  $("dot").className = "dot " + (streaming ? "ok" : game.connected ? "warn" : "");
  $("zone").textContent = game.map ? game.map.name : "Ждём данные из игры…";
  $("coords").textContent = st && st.hasPos ? `${(st.x * 100).toFixed(1)}, ${(st.y * 100).toFixed(1)}` : "";
  $("worldmode").hidden = !game.world;
  setButton("worldmode", view.mode === "world");
  $("layers").hidden = !game.places;
  if (!game.places) $("layerpanel").hidden = true;

  let notice = "";
  if (!game.connected) notice = "Нет связи с компаньоном на ПК. Он запущен? Телефон в той же Wi-Fi сети?";
  else if (game.status.problem) notice = PROBLEMS[game.status.problem] || game.status.problem;
  else if (st && !st.hasPos) notice = "Игра не даёт координаты здесь (подземелье, рейд или поле боя).";
  else if (worldMode() && !game.world.zones) notice = "Компаньон не нашёл OpenMap на ПК, поэтому на карте мира нет зон. Запусти его с --openmap \"путь к папке OpenMap\" или переключись на карту зоны.";
  else if (!worldMode() && game.map && game.map.placeholder) notice = "Нет картинки карты: компаньон не нашёл папку WoW (запусти его с --wow \"путь\").";
  const box = $("notice");
  box.hidden = !notice;
  box.textContent = notice;
}

function setButton(id, on) {
  $(id).classList.toggle("on", on);
}

function bindControls() {
  $("follow").onclick = () => {
    view.follow = !view.follow;
    setButton("follow", view.follow);
    saveSettings();
    needsDraw = true;
  };
  $("rotate").onclick = () => {
    view.headingUp = !view.headingUp;
    if (!view.headingUp) view.rotation = 0;
    setButton("rotate", view.headingUp);
    saveSettings();
    needsDraw = true;
  };
  $("worldmode").onclick = () => {
    view.mode = view.mode === "world" ? "zone" : "world";
    saveSettings();
    updateBar();
    needsDraw = true;
  };
  $("layers").onclick = () => {
    $("layerpanel").hidden = !$("layerpanel").hidden;
    setButton("layers", !$("layerpanel").hidden);
  };
  for (const button of document.querySelectorAll("#layerpanel button")) {
    const key = button.dataset.layer;
    button.classList.toggle("on", view.layers[key]);
    button.onclick = () => {
      view.layers[key] = !view.layers[key];
      button.classList.toggle("on", view.layers[key]);
      saveSettings();
      needsDraw = true;
    };
  }
  $("zoomin").onclick = () => zoomAt(innerWidth / 2, innerHeight / 2, 1.5);
  $("zoomout").onclick = () => zoomAt(innerWidth / 2, innerHeight / 2, 1 / 1.5);
  $("fullscreen").onclick = () => {
    const el = document.documentElement;
    if (document.fullscreenElement) document.exitFullscreen();
    else if (el.requestFullscreen) el.requestFullscreen().catch(() => {});
  };
  setButton("follow", view.follow);
  setButton("rotate", view.headingUp);
}

// ---------------------------------------------------------------------------
// Gestures: drag to pan, pinch or wheel to zoom, double tap to zoom in

const pointers = new Map();
let gesture = null;
let tap = null; // a single finger down that may still turn out a tap: { x, y, t }

function startGesture() {
  const pts = [...pointers.values()];
  if (pts.length === 1) {
    gesture = { kind: "pan", last: pts[0], moved: 0 };
  } else if (pts.length >= 2) {
    const [a, b] = pts;
    gesture = { kind: "pinch", dist: Math.hypot(a.x - b.x, a.y - b.y), mid: [(a.x + b.x) / 2, (a.y + b.y) / 2] };
  } else {
    gesture = null;
  }
}

canvas.addEventListener("pointerdown", (e) => {
  canvas.setPointerCapture(e.pointerId);
  pointers.set(e.pointerId, { x: e.clientX, y: e.clientY });
  tap = pointers.size === 1 ? { x: e.clientX, y: e.clientY, t: performance.now() } : null;
  startGesture();
});

canvas.addEventListener("pointermove", (e) => {
  if (!pointers.has(e.pointerId) || !gesture) return;
  pointers.set(e.pointerId, { x: e.clientX, y: e.clientY });
  if (tap && Math.hypot(e.clientX - tap.x, e.clientY - tap.y) > 8) tap = null;
  const pts = [...pointers.values()];
  if (gesture.kind === "pan" && pts.length === 1) {
    const p = pts[0];
    gesture.moved += Math.hypot(p.x - gesture.last.x, p.y - gesture.last.y);
    if (gesture.moved > 6 && view.follow) {
      view.follow = false;
      setButton("follow", false);
      saveSettings();
    }
    if (!view.follow) {
      const a = screenToWorld(gesture.last.x, gesture.last.y);
      const b = screenToWorld(p.x, p.y);
      view.cx += a[0] - b[0];
      view.cy += a[1] - b[1];
    }
    gesture.last = p;
    needsDraw = true;
  } else if (gesture.kind === "pinch" && pts.length >= 2) {
    const [a, b] = pts;
    const dist = Math.hypot(a.x - b.x, a.y - b.y);
    if (gesture.dist > 0) zoomAt(gesture.mid[0], gesture.mid[1], dist / gesture.dist);
    gesture.dist = dist;
  }
});

function endPointer(e) {
  pointers.delete(e.pointerId);
  if (tap && e.type === "pointerup" && pointers.size === 0 && performance.now() - tap.t < 400) onTap(tap.x, tap.y);
  if (pointers.size === 0) tap = null;
  startGesture();
}
canvas.addEventListener("pointerup", endPointer);
canvas.addEventListener("pointercancel", endPointer);

canvas.addEventListener("wheel", (e) => {
  e.preventDefault();
  zoomAt(e.clientX, e.clientY, Math.exp(-e.deltaY * 0.0015));
}, { passive: false });

canvas.addEventListener("dblclick", (e) => zoomAt(e.clientX, e.clientY, 2));

// ---------------------------------------------------------------------------
// Zone tiles: OpenMap's stitched zone art as a pyramid (level 0 finest, each level up halves it)

const tiles = new Map();
let tileClock = 0;

function tileImage(level, i, j, load) {
  const key = level + "/" + i + "/" + j;
  let t = tiles.get(key);
  if (!t && load) {
    const zones = game.world.zones;
    t = { level, ready: false, used: 0, img: new Image() };
    t.img.onload = () => {
      t.ready = true;
      needsDraw = true;
    };
    t.img.src = zones.url.replace("{level}", level).replace("{i}", i).replace("{j}", j);
    tiles.set(key, t);
  }
  if (!t) return null;
  t.used = tileClock;
  return t.ready ? t.img : null;
}

function evictTiles() {
  const zones = game.world && game.world.zones;
  if (!zones || tiles.size <= TILE_KEEP) return;
  const old = [...tiles.entries()]
    .filter(([, t]) => t.level < zones.coarse && t.used < tileClock)
    .sort((a, b) => a[1].used - b[1].used);
  for (const [key] of old.slice(0, tiles.size - TILE_KEEP)) tiles.delete(key);
}

function drawZoneTiles(alpha) {
  const zones = game.world.zones;
  const dpr = window.devicePixelRatio || 1;
  const pxPerUnit = zones.tilePx / zones.unit;
  const level = Math.min(zones.maxLevel, Math.max(0, Math.floor(Math.log2(pxPerUnit / (view.scale * dpr)))));
  const size = zones.unit * 2 ** level;
  const pad = alpha > 0.99 ? 0.75 / view.scale : 0; // hide hairline seams between tiles
  const [x0, y0, x1, y1] = visibleBox();
  const set = zones.sets[level];
  ctx.globalAlpha = alpha;
  for (let i = Math.max(0, Math.floor(x0 / size)); i <= Math.floor(x1 / size); i++) {
    for (let j = Math.max(0, Math.floor(y0 / size)); j <= Math.floor(y1 / size); j++) {
      if (!set.has(i * 256 + j)) continue;
      const x = i * size;
      const y = j * size;
      const img = tileImage(level, i, j, true);
      if (img) {
        ctx.drawImage(img, x, y, size + pad, size + pad);
        continue;
      }
      for (let up = level + 1; up <= zones.maxLevel; up++) { // a coarser tile stands in meanwhile
        const d = up - level;
        const parent = tileImage(up, i >> d, j >> d, false);
        if (!parent) continue;
        const part = zones.tilePx / 2 ** d;
        ctx.drawImage(parent, (i - ((i >> d) << d)) * part, (j - ((j >> d) << d)) * part, part, part, x, y, size + pad, size + pad);
        break;
      }
    }
  }
  ctx.globalAlpha = 1;
}

// ---------------------------------------------------------------------------
// Rendering

function smooth(from, to, zoom) {
  const t = Math.max(0, Math.min(1, (zoom - from) / (to - from)));
  return t * t * (3 - 2 * t);
}

function openMapZoom() {
  return view.scale * game.world.width / 1000;
}

function drawWorldArt() {
  const world = game.world;
  const W = world.width;
  const H = world.height;
  const zoom = openMapZoom();
  const sea = world.sea || {};
  if (world.seaImage) {
    const reach = sea.reach || 0;
    ctx.drawImage(world.seaImage, -reach * W, -reach * H, (1 + 2 * reach) * W, (1 + 2 * reach) * H);
  }
  const contAlpha = smooth(CONT_IN[0], CONT_IN[1], zoom);
  for (const art of world.art) {
    const img = world.images[art.mapID];
    if (!img) continue;
    const alpha = art.kind === "world" ? 1 : contAlpha;
    if (alpha < 0.003) continue;
    const [l, r, t, b] = art.rect;
    ctx.globalAlpha = alpha;
    ctx.drawImage(img, l * W, t * H, (r - l) * W, (b - t) * H);
  }
  ctx.globalAlpha = 1;
  if (world.zones) {
    const d = DETAIL_HINT[2] * smooth(DETAIL_HINT[0], DETAIL_HINT[1], zoom) + (1 - DETAIL_HINT[2]) * smooth(DETAIL_FADE[0], DETAIL_FADE[1], zoom);
    const alpha = 1 - (1 - d) * (1 - d);
    if (alpha > 0.003) drawZoneTiles(alpha);
  }
}

function drawLabels() {
  const world = game.world;
  const alpha = 1 - smooth(LABELS_OUT[0], LABELS_OUT[1], openMapZoom());
  if (alpha < 0.01 || !world.labels) return;
  ctx.save();
  ctx.globalAlpha = alpha;
  ctx.textAlign = "center";
  ctx.textBaseline = "middle";
  ctx.lineJoin = "round";
  for (const label of world.labels) {
    const [x, y] = worldToScreen(label.x * world.width / 1002, label.y * world.height / 668);
    const px = Math.min(30, Math.max(11, label.size * view.scale * 1.3));
    ctx.font = `700 ${px.toFixed(1)}px "Palatino Linotype", "Book Antiqua", Georgia, serif`;
    ctx.lineWidth = Math.max(3, px * 0.22);
    ctx.strokeStyle = LAND_GLOW;
    ctx.strokeText(label.text, x, y);
    ctx.fillStyle = LAND_INK;
    ctx.fillText(label.text, x, y);
  }
  ctx.restore();
}

// ---------------------------------------------------------------------------
// OpenMap's places: flight masters and their network, docks and sailings, dungeon entrances

function placesZoom() {
  if (worldMode()) return openMapZoom();
  const r = game.map && game.map.rect;
  if (!r || r[1] <= r[0]) return null; // a dungeon's map: not on the world
  return view.scale * game.map.width / ((r[1] - r[0]) * 1000);
}

function placeToScreen(u, v) {
  const W = game.world.width;
  const H = game.world.height;
  if (worldMode()) return worldToScreen(u * W, v * H);
  const p = worldToMap(u * W, v * H);
  return p ? worldToScreen(p[0] * game.map.width, p[1] * game.map.height) : null;
}

function onScreen(p, margin) {
  return p && p[0] > -margin && p[1] > -margin && p[0] < innerWidth + margin && p[1] < innerHeight + margin;
}

function factionBit() {
  const f = game.session && game.session.faction;
  return f === "Alliance" ? 1 : f === "Horde" ? 2 : 3;
}

function travelNode(id) {
  return (game.travel && game.travel.nodes && game.travel.nodes[String(id)]) || null;
}

function instanceName(inst) {
  const names = game.travel && game.travel.instances;
  return (names && inst.id && names[String(inst.id)]) || inst.name;
}

// the zone at a point of the world (the smallest whose rect holds it), for "where it goes"
function zoneAt(u, v) {
  let best = null;
  let area = Infinity;
  for (const zone of Object.values(game.zones || {})) {
    const [l, r, t, b] = zone.rect;
    if (u >= l && u <= r && v >= t && v <= b && (r - l) * (b - t) < area) {
      best = zone;
      area = (r - l) * (b - t);
    }
  }
  return best;
}

// an entrance's place on the world: its own, or a spot on a zone's map
function entranceAt(d) {
  if (typeof d.u === "number") return [d.u, d.v];
  const zone = game.zones && game.zones[String(d.map)];
  if (!zone) return null;
  const [l, r, t, b] = zone.rect;
  return [l + d.x * (r - l), t + d.y * (b - t)];
}

// quest difficulty colours by the player's level (the game's GetQuestDifficultyColor)
function difficultyColor(level) {
  const player = (game.session && game.session.level) || 0;
  if (!player) return "#ffd100";
  const d = level - player;
  if (d >= 5) return "#ff1a1a";
  if (d >= 3) return "#ff8040";
  if (d >= -2) return "#ffff00";
  const green = player <= 5 ? 5 : player <= 39 ? 5 + Math.floor(player / 10) : player <= 59 ? 1 + Math.floor(player / 5) : 9;
  return -d <= green ? "#40c040" : "#9d9d9d";
}

function rangeColor(lo, hi) {
  const player = (game.session && game.session.level) || 0;
  if (!player) return "#ffd100";
  if (player < lo) return difficultyColor(lo);
  if (player > hi) return difficultyColor(hi - 2);
  return "#ffff00";
}

function rangeText(lo, hi) {
  return lo === hi ? String(hi) : `${lo}–${hi}`;
}

function strokePath(pts, color, width, dash) {
  ctx.beginPath();
  let started = false;
  for (let k = 0; k + 1 < pts.length; k += 2) {
    const p = placeToScreen(pts[k], pts[k + 1]);
    if (!p) continue;
    if (started) ctx.lineTo(p[0], p[1]);
    else ctx.moveTo(p[0], p[1]);
    started = true;
  }
  ctx.strokeStyle = color;
  ctx.lineWidth = width;
  ctx.setLineDash(dash);
  ctx.stroke();
  ctx.setLineDash([]);
}

function disk(x, y, r, fill, ring) {
  ctx.beginPath();
  ctx.arc(x, y, r, 0, 2 * Math.PI);
  ctx.fillStyle = fill;
  ctx.fill();
  ctx.lineWidth = 1.6;
  ctx.strokeStyle = ring;
  ctx.stroke();
}

function drawFlightIcon(x, y, known, horde) {
  const r = known ? 8.5 : 6.5;
  ctx.shadowColor = "rgba(0,0,0,0.55)";
  ctx.shadowBlur = 4;
  disk(x, y, r, known ? (horde ? "#b8281c" : "#2f62c9") : "#8d8579", "#1d1408");
  ctx.shadowBlur = 0;
  ctx.beginPath(); // a wing
  ctx.moveTo(x - r * 0.55, y + r * 0.25);
  ctx.quadraticCurveTo(x - r * 0.1, y - r * 0.15, x + r * 0.5, y - r * 0.5);
  ctx.quadraticCurveTo(x + r * 0.15, y + r * 0.05, x + r * 0.2, y + r * 0.4);
  ctx.closePath();
  ctx.fillStyle = known ? "#fff" : "#e8e2d6";
  ctx.fill();
}

function drawDockIcon(x, y, kind) {
  ctx.shadowColor = "rgba(0,0,0,0.55)";
  ctx.shadowBlur = 4;
  disk(x, y, 9.5, "#3b2a16", "#e8d3a0");
  ctx.shadowBlur = 0;
  ctx.fillStyle = "#f4ead2";
  ctx.beginPath();
  if (kind === "zeppelin") {
    ctx.ellipse(x, y - 1.5, 6, 3.4, 0, 0, 2 * Math.PI);
    ctx.fill();
    ctx.fillRect(x - 2, y + 2.6, 4, 2.2);
  } else {
    ctx.moveTo(x - 5.5, y + 1.5); // hull
    ctx.lineTo(x + 5.5, y + 1.5);
    ctx.lineTo(x + 3.2, y + 4.8);
    ctx.lineTo(x - 3.2, y + 4.8);
    ctx.closePath();
    ctx.moveTo(x + 0.6, y - 6); // sail
    ctx.lineTo(x + 4.6, y + 0.5);
    ctx.lineTo(x + 0.6, y + 0.5);
    ctx.closePath();
    ctx.fill();
    ctx.fillRect(x - 0.6, y - 6, 1.2, 7.5); // mast
  }
}

function drawDungeonIcon(x, y, raid) {
  const r = raid ? 10 : 8.5;
  ctx.shadowColor = "rgba(0,0,0,0.55)";
  ctx.shadowBlur = 4;
  disk(x, y, r, raid ? "#6b1717" : "#2b2117", "#d9b871");
  ctx.shadowBlur = 0;
  const w = r * 0.42; // a doorway
  ctx.beginPath();
  ctx.moveTo(x - w, y + r * 0.5);
  ctx.lineTo(x - w, y - r * 0.05);
  ctx.arc(x, y - r * 0.05, w, Math.PI, 0);
  ctx.lineTo(x + w, y + r * 0.5);
  ctx.closePath();
  ctx.fillStyle = "#f4ead2";
  ctx.fill();
}

function drawDungeonLabel(x, y, d) {
  const insts = d.insts;
  const first = instanceName(insts[0]);
  const name = insts.length === 1 ? first : insts.length === 2 ? `${first} · ${instanceName(insts[1])}` : `${first} +${insts.length - 1}`;
  const lo = Math.min(...insts.map((i) => i.min || 0));
  const hi = Math.max(...insts.map((i) => i.max || 0));
  ctx.font = "600 12px system-ui, sans-serif";
  ctx.textAlign = "center";
  ctx.textBaseline = "top";
  ctx.lineJoin = "round";
  ctx.lineWidth = 3;
  ctx.strokeStyle = "rgba(20,14,6,0.85)";
  ctx.strokeText(name, x, y + 13);
  ctx.fillStyle = "#f4ead2";
  ctx.fillText(name, x, y + 13);
  if (hi > 0) {
    const levels = rangeText(lo, hi);
    ctx.strokeText(levels, x, y + 27);
    ctx.fillStyle = rangeColor(lo, hi);
    ctx.fillText(levels, x, y + 27);
  }
}

function drawPlaces() {
  const places = game.places;
  const zoom = places && game.world ? placesZoom() : null;
  if (zoom === null) return;
  const shown = (kind) => {
    const from = PLACES_FROM[kind];
    return worldMode() ? smooth(from * 0.85, from, zoom) : 1;
  };
  const bit = factionBit();
  ctx.save();
  if (!worldMode()) { // on a zone's map: only over the map itself
    const w = game.map.width;
    const h = game.map.height;
    ctx.beginPath();
    [[0, 0], [w, 0], [w, h], [0, h]].forEach(([x, y], k) => {
      const p = worldToScreen(x, y);
      if (k) ctx.lineTo(p[0], p[1]);
      else ctx.moveTo(p[0], p[1]);
    });
    ctx.clip();
  }

  if (view.layers.travel && shown("sailings") > 0.01) {
    ctx.globalAlpha = 0.5 * shown("sailings");
    ctx.lineCap = "round";
    for (const s of places.sailings) {
      const [r, g, b] = s.faction === "Alliance" ? ALLIANCE_ROUTE : HORDE_ROUTE;
      strokePath(s.pts, `rgb(${r},${g},${b})`, 3, [0.1, 8]);
    }
    ctx.lineCap = "butt";
  }

  const nodes = new Map();
  if (view.layers.flights) {
    for (const n of places.flights.nodes) if (n.factions & bit) nodes.set(n.id, n);
  }
  const flightAlpha = shown("flights");
  if (nodes.size && flightAlpha > 0.01) {
    // the network you can fly: paths between learned flight masters
    ctx.globalAlpha = flightAlpha;
    for (const path of places.flights.paths) {
      const a = travelNode(path.a);
      const b = travelNode(path.b);
      if (!(nodes.has(path.a) && nodes.has(path.b) && a && a.known && b && b.known)) continue;
      strokePath(path.pts, FLIGHT_SHADE, 4, [6, 4]); // dark under the gold, or it is lost on the parchment
      strokePath(path.pts, FLIGHT_LINE, 2, [6, 4]);
    }
  }

  const dungeonAlpha = view.layers.dungeons ? shown("dungeons") : 0;
  if (dungeonAlpha > 0.01) {
    ctx.globalAlpha = dungeonAlpha;
    for (const d of places.dungeons) {
      const at = entranceAt(d);
      const p = at && placeToScreen(at[0], at[1]);
      if (!onScreen(p, 60)) continue;
      drawDungeonIcon(p[0], p[1], d.insts.some((i) => i.raid));
      if (zoom >= LABELS_FROM || !worldMode()) drawDungeonLabel(p[0], p[1], d);
      hits.push({ x: p[0], y: p[1], r: 12, kind: "dungeon", item: d });
    }
  }

  const dockAlpha = view.layers.travel ? shown("docks") : 0;
  if (dockAlpha > 0.01) {
    ctx.globalAlpha = dockAlpha;
    const faction = game.session && game.session.faction;
    for (const d of places.docks) {
      if (d.faction && faction && d.faction !== faction) continue;
      const p = placeToScreen(d.u, d.v);
      if (!onScreen(p, 30)) continue;
      drawDockIcon(p[0], p[1], d.kind);
      hits.push({ x: p[0], y: p[1], r: 12, kind: "dock", item: d });
    }
  }

  if (nodes.size && flightAlpha > 0.01) {
    ctx.globalAlpha = flightAlpha;
    const horde = bit === 2;
    for (const n of nodes.values()) {
      const p = placeToScreen(n.u, n.v);
      if (!onScreen(p, 30)) continue;
      const info = travelNode(n.id);
      drawFlightIcon(p[0], p[1], !!(info && info.known), horde);
      hits.push({ x: p[0], y: p[1], r: 11, kind: "flight", item: n });
    }
  }
  ctx.restore();
}

// ---------------------------------------------------------------------------
// The card a tap on a place opens

function cardLine(text, cls, color) {
  const div = document.createElement("div");
  div.textContent = text;
  if (cls) div.className = cls;
  if (color) div.style.color = color;
  return div;
}

function showCard(hit) {
  const card = $("card");
  card.replaceChildren();
  const item = hit.item;
  if (hit.kind === "flight") {
    const info = travelNode(item.id);
    card.append(cardLine((info && info.name) || item.name, "title"));
    const state = info ? (info.known ? "изучена" : "не изучена") : "";
    card.append(cardLine(state ? `Точка полёта · ${state}` : "Точка полёта", "sub"));
  } else if (hit.kind === "dock") {
    card.append(cardLine(KIND_NAMES[item.kind] || "Транспорт", "title"));
    const zone = item.to ? zoneAt(item.to[0], item.to[1]) : null;
    card.append(cardLine(zone ? `Куда: ${zone.name}` : item.route, "sub"));
  } else {
    item.insts.forEach((inst, k) => {
      card.append(cardLine(instanceName(inst), "title"));
      const lo = inst.min || 0;
      const hi = inst.max || lo;
      const kind = inst.raid ? "Рейд" : "Подземелье";
      card.append(cardLine(hi ? `${kind} · ${rangeText(lo, hi)}` : kind, "sub", hi ? rangeColor(lo, hi) : null));
      // wings of one dungeon share its bosses: listed once, after the last wing
      const next = item.insts[k + 1];
      if (inst.bosses && inst.bosses.length && !(next && next.id === inst.id)) card.append(cardLine(inst.bosses.join(", "), "bosses"));
    });
  }
  card.hidden = false;
}

function onTap(x, y) {
  let best = null;
  let dist = Infinity;
  for (const hit of hits) {
    const d = Math.hypot(hit.x - x, hit.y - y);
    if (d <= hit.r + 10 && d < dist) {
      best = hit;
      dist = d;
    }
  }
  if (best) showCard(best);
  else $("card").hidden = true;
}

function angleLerp(a, b, t) {
  let d = (b - a) % (2 * Math.PI);
  if (d > Math.PI) d -= 2 * Math.PI;
  if (d < -Math.PI) d += 2 * Math.PI;
  return a + d * t;
}

function animate(dt) {
  const st = game.state;
  const s = game.shown;
  if (!st || !s || !st.hasPos) return;
  const t = Math.min(1, dt * 12);
  const before = s.x + s.y + s.u + s.v + s.facing;
  s.x += (st.x - s.x) * t;
  s.y += (st.y - s.y) * t;
  if (st.hasWorld) {
    s.u += (st.u - s.u) * t;
    s.v += (st.v - s.v) * t;
  }
  if (st.hasFacing) s.facing = angleLerp(s.facing, st.facing, t);
  if (Math.abs(s.x + s.y + s.u + s.v + s.facing - before) > 1e-7) needsDraw = true;
}

function playerPoint() {
  const s = game.shown;
  const st = game.state;
  if (!s || !st || !st.hasPos) return null;
  if (worldMode()) return st.hasWorld ? [s.u * game.world.width, s.v * game.world.height] : null;
  if (!game.map || s.mapID !== game.map.mapID) return null;
  return [s.x * game.map.width, s.y * game.map.height];
}

function memberPoint(m) {
  if (!game.map || !game.state || game.state.mapID !== game.map.mapID) return null;
  if (worldMode()) return mapToWorld(m.x, m.y);
  return [m.x * game.map.width, m.y * game.map.height];
}

function drawArrow(x, y, angle, dead) {
  ctx.save();
  ctx.translate(x, y);
  ctx.rotate(angle);
  const s = 13;
  ctx.beginPath();
  ctx.moveTo(0, -s * 1.3);
  ctx.lineTo(s * 0.9, s);
  ctx.lineTo(0, s * 0.45);
  ctx.lineTo(-s * 0.9, s);
  ctx.closePath();
  ctx.lineJoin = "round";
  ctx.shadowColor = "rgba(0,0,0,0.6)";
  ctx.shadowBlur = 6;
  ctx.fillStyle = dead ? "#9a9a9a" : "#ffd257";
  ctx.fill();
  ctx.shadowBlur = 0;
  ctx.lineWidth = 2.5;
  ctx.strokeStyle = "#2b1d05";
  ctx.stroke();
  ctx.restore();
}

function drawMember(m) {
  const p = memberPoint(m);
  if (!p) return;
  const [x, y] = worldToScreen(p[0], p[1]);
  ctx.beginPath();
  ctx.arc(x, y, 7, 0, 2 * Math.PI);
  ctx.fillStyle = m.offline || m.dead ? "#777" : CLASS_COLORS[m.classID] || "#ddd";
  ctx.fill();
  ctx.lineWidth = 2;
  ctx.strokeStyle = "#111";
  ctx.stroke();
  const info = game.roster[String(m.unit)];
  if (info && info.name) {
    ctx.font = "600 12px system-ui, sans-serif";
    ctx.textAlign = "center";
    ctx.textBaseline = "alphabetic";
    ctx.lineWidth = 3;
    ctx.strokeStyle = "rgba(0,0,0,0.75)";
    ctx.strokeText(info.name, x, y - 12);
    ctx.fillStyle = "#fff";
    ctx.fillText(info.name, x, y - 12);
  }
}

function currentSpace() {
  if (worldMode()) return { kind: "world", w: game.world.width, h: game.world.height };
  if (!game.map) return null;
  return { kind: "zone", mapID: game.map.mapID, w: game.map.width, h: game.map.height };
}

function draw() {
  const dpr = window.devicePixelRatio || 1;
  ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
  const space = currentSpace();
  const world = space && space.kind === "world";
  const sea = world && game.world.sea && game.world.sea.color;
  ctx.fillStyle = world && game.world.seaFill ? game.world.seaFill : sea ? `rgb(${sea.join(",")})` : "#16130f";
  ctx.fillRect(0, 0, innerWidth, innerHeight);
  if (!space) return;
  if (!lastSpace || lastSpace.kind !== space.kind || lastSpace.mapID !== space.mapID) {
    onSpaceChange(space, lastSpace);
    lastSpace = space;
    updateBar();
  }

  const player = playerPoint();
  if (player) {
    if (view.follow) {
      view.cx = player[0];
      view.cy = player[1];
    }
    if (view.headingUp && game.state.hasFacing) view.rotation = game.shown.facing;
  }

  tileClock++;
  ctx.save();
  ctx.translate(innerWidth / 2, innerHeight / 2);
  ctx.rotate(view.rotation);
  ctx.scale(view.scale, view.scale);
  ctx.translate(-view.cx, -view.cy);
  ctx.imageSmoothingQuality = "high";
  if (world) {
    drawWorldArt();
  } else if (game.image) {
    ctx.drawImage(game.image, 0, 0, game.map.width, game.map.height);
  } else {
    ctx.fillStyle = "#2a241b";
    ctx.fillRect(0, 0, game.map.width, game.map.height);
  }
  ctx.restore();
  if (world) {
    drawLabels();
    evictTiles();
  }
  hits = [];
  drawPlaces();

  for (const m of game.group) drawMember(m);

  if (player) {
    const [x, y] = worldToScreen(player[0], player[1]);
    if (game.state.hasFacing) {
      drawArrow(x, y, view.rotation - game.shown.facing, game.state.dead);
    } else {
      ctx.beginPath();
      ctx.arc(x, y, 8, 0, 2 * Math.PI);
      ctx.fillStyle = "#ffd257";
      ctx.fill();
    }
  }
}

let lastFrame = performance.now();
function frame(now) {
  const dt = (now - lastFrame) / 1000;
  lastFrame = now;
  animate(dt);
  if (needsDraw) {
    needsDraw = false;
    draw();
  }
  requestAnimationFrame(frame);
}

loadSettings();
bindControls();
window.addEventListener("resize", resize);
resize();
connect();
requestAnimationFrame(frame);
