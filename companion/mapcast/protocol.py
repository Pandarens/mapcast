"""Decoder for the MapCast pixel protocol (see docs/PROTOCOL.md)."""
from __future__ import annotations

import binascii
import time
from dataclasses import dataclass, field

import numpy as np

MAGENTA = np.array([255, 0, 255])
GREEN = np.array([0, 255, 0])
MARKER_TOLERANCE = 40
MAX_CELL_PX = 8

REC_STATE, REC_GROUP, REC_WORLD, REC_BULK = 0x01, 0x02, 0x03, 0x10
REC_SESSION, REC_MAPINFO, REC_EXPLORED, REC_ROSTER = 0x20, 0x21, 0x22, 0x23
REC_MAPRECT, REC_WORLDINFO = 0x24, 0x25
CH_WORLD = 4

STATE_FLAGS = ("hasPos", "hasFacing", "combat", "dead", "mounted", "flying", "instance", "resting")


def crc16(data: bytes) -> int:
    """CRC16-CCITT-FALSE (poly 0x1021, init 0xFFFF)."""
    return binascii.crc_hqx(data, 0xFFFF)


class Reader:
    def __init__(self, data: bytes):
        self.data = data
        self.pos = 0

    def u8(self) -> int:
        v = self.data[self.pos]
        self.pos += 1
        return v

    def u16(self) -> int:
        v = int.from_bytes(self.data[self.pos:self.pos + 2], "little")
        self.pos += 2
        return v

    def u32(self) -> int:
        v = int.from_bytes(self.data[self.pos:self.pos + 4], "little")
        self.pos += 4
        return v

    def str(self) -> str:
        n = self.u8()
        raw = self.data[self.pos:self.pos + n]
        self.pos += n
        return raw.decode("utf-8", errors="replace")

    def rest(self) -> bytes:
        v = self.data[self.pos:]
        self.pos = len(self.data)
        return v


def iter_records(buf: bytes):
    pos = 0
    while pos + 3 <= len(buf):
        rtype = buf[pos]
        length = buf[pos + 1] | buf[pos + 2] << 8
        body = buf[pos + 3:pos + 3 + length]
        if len(body) < length:
            return
        yield rtype, body
        pos += 3 + length


def _near(pixel, color) -> bool:
    return int(np.abs(pixel.astype(int) - color).max()) <= MARKER_TOLERANCE


def _exact(pixel, color) -> bool:
    return bool((pixel.astype(int) == color).all())


@dataclass
class Geometry:
    cell: int
    cols: int
    rows: int

    @property
    def size_px(self) -> tuple[int, int]:
        return self.cols * self.cell, self.rows * self.cell


def find_geometry(img: np.ndarray) -> tuple[Geometry | None, str]:
    """Locate the strip at the image's top-left. Returns (geometry, problem)."""
    if img.shape[0] < 1 or img.shape[1] < 3:
        return None, "capture too small"
    row = img[0]
    if not _near(row[0], MAGENTA):
        return None, "no strip in the top-left corner"
    cell = 1
    while cell < min(MAX_CELL_PX + 1, len(row)) and _near(row[cell], MAGENTA):
        cell += 1
    if cell > MAX_CELL_PX or 3 * cell > len(row) or not all(_near(row[cell + i], GREEN) for i in range(cell)):
        return None, "strip markers not recognized"
    if not (_exact(row[0], MAGENTA) and _exact(row[cell], GREEN)):
        return None, "colors are distorted: set in-game Gamma/Brightness/Contrast to defaults"
    c = cell // 2
    if img.shape[0] <= c:
        return None, "capture too small"
    geo_px = img[c, 2 * cell + c]
    cols = int(geo_px[0]) | int(geo_px[1]) << 8
    rows = int(geo_px[2])
    if cols < 4 or not 1 <= rows <= 16:
        return None, "bad strip geometry"
    return Geometry(cell, cols, rows), ""


def read_page_bytes(img: np.ndarray, geo: Geometry) -> bytes | None:
    w, h = geo.size_px
    if img.shape[1] < w or img.shape[0] < h:
        return None
    c = geo.cell // 2
    ys = np.arange(geo.rows) * geo.cell + c
    xs = np.arange(geo.cols) * geo.cell + c
    cells = img[np.ix_(ys, xs)].reshape(-1, 3)[3:]
    return cells.astype(np.uint8).tobytes()


@dataclass
class Page:
    seq: int
    crc: int
    payload: bytes


def parse_page(data: bytes) -> Page | None:
    if len(data) < 6 or data[0] != 1:
        return None
    length = data[2] | data[3] << 8
    end = 4 + length
    if end + 2 > len(data):
        return None
    crc = data[end] | data[end + 1] << 8
    if crc16(data[:end]) != crc:
        return None
    return Page(data[1], crc, data[4:end])


# ---------------------------------------------------------------------------
# Record bodies


def norm(v: int) -> float:
    return v / 65535.0


def world_coord(v: int) -> float:
    """World coordinates are u32 = (v + 0.5) * 2^24."""
    return v / 16777216.0 - 0.5


def parse_state(body: bytes) -> dict:
    r = Reader(body)
    map_id, x, y, facing, flags, time_ms = r.u32(), r.u16(), r.u16(), r.u16(), r.u8(), r.u32()
    state = {"mapID": map_id, "x": norm(x), "y": norm(y), "facing": facing / 65536.0 * 2 * np.pi, "timeMs": time_ms}
    for bit, name in enumerate(STATE_FLAGS):
        state[name] = bool(flags & (1 << bit))
    return state


def parse_world(body: bytes) -> tuple[float, float]:
    r = Reader(body)
    return world_coord(r.u32()), world_coord(r.u32())


def parse_maprect(body: bytes) -> dict:
    r = Reader(body)
    map_id = r.u32()
    left, right, top, bottom = (world_coord(r.u32()) for _ in range(4))
    return {"mapID": map_id, "rect": [left, right, top, bottom]}


def parse_worldinfo(body: bytes) -> dict:
    r = Reader(body)
    flags, world_map, version = r.u8(), r.u32(), r.str()
    return {"openmap": bool(flags & 1), "worldMapID": world_map, "openmapVersion": version}


def parse_group(body: bytes) -> list[dict]:
    r = Reader(body)
    members = []
    for _ in range(r.u8()):
        unit, class_id, x, y, flags = r.u8(), r.u8(), r.u16(), r.u16(), r.u8()
        members.append({"unit": unit, "classID": class_id, "x": norm(x), "y": norm(y),
                        "dead": bool(flags & 1), "offline": bool(flags & 2)})
    return members


def parse_session(body: bytes) -> dict:
    r = Reader(body)
    return {"addonVersion": r.u8(), "interface": r.u32(), "locale": r.str(), "player": r.str(),
            "realm": r.str(), "classID": r.u8()}


def parse_mapinfo(body: bytes) -> dict:
    r = Reader(body)
    info = {"mapID": r.u32(), "mapType": r.u8(), "parentMapID": r.u32(), "name": r.str(), "layers": []}
    for _ in range(r.u8()):
        layer = {"w": r.u16(), "h": r.u16(), "tileW": r.u16(), "tileH": r.u16()}
        layer["tiles"] = [r.u32() for _ in range(r.u16())]
        info["layers"].append(layer)
    return info


def parse_explored(body: bytes) -> dict:
    r = Reader(body)
    map_id = r.u32()
    overlays = []
    for _ in range(r.u16()):
        ov = {"w": r.u16(), "h": r.u16(), "x": r.u16(), "y": r.u16(), "flags": r.u8()}
        ov["files"] = [r.u32() for _ in range(r.u8())]
        overlays.append(ov)
    return {"mapID": map_id, "overlays": overlays}


def parse_roster(body: bytes) -> dict[int, dict]:
    r = Reader(body)
    roster = {}
    for _ in range(r.u8()):
        unit, class_id, name = r.u8(), r.u8(), r.str()
        roster[unit] = {"classID": class_id, "name": name}
    return roster


# ---------------------------------------------------------------------------
# Stateful decoder


@dataclass
class _Channel:
    gen: int
    count: int
    chunks: dict = field(default_factory=dict)
    done: bool = False


class Decoder:
    """Turns captured frames into events: (name, data) tuples."""

    def __init__(self):
        self.geometry: Geometry | None = None
        self.problem = "waiting for the first frame"
        self._last_page: tuple[int, int] | None = None
        self._channels: dict[int, _Channel] = {}
        self.pages = 0
        self.bad_pages = 0
        self.last_page_time = 0.0

    def feed(self, img: np.ndarray) -> list[tuple[str, object]]:
        geo, problem = find_geometry(img)
        if geo is None:
            self.geometry, self.problem = None, problem
            return []
        self.geometry = geo
        data = read_page_bytes(img, geo)
        if data is None:
            self.problem = "capture region smaller than the strip"
            return []
        page = parse_page(data)
        if page is None:
            if data[0] != 1:
                self.problem = "the addon has not sent data yet"
            else:
                self.bad_pages += 1
                self.problem = "page checksum failed"
            return []
        self.problem = ""
        key = (page.seq, page.crc)
        if key == self._last_page:
            return []
        self._last_page = key
        self.pages += 1
        self.last_page_time = time.monotonic()
        return self.handle_payload(page.payload)

    def handle_payload(self, payload: bytes) -> list[tuple[str, object]]:
        events: list[tuple[str, object]] = []
        has_group = False
        state, world = None, None
        for rtype, body in iter_records(payload):
            if rtype == REC_STATE:
                state = parse_state(body)
                events.append(("state", state))
            elif rtype == REC_WORLD:
                world = parse_world(body)
            elif rtype == REC_GROUP:
                has_group = True
                events.append(("group", parse_group(body)))
            elif rtype == REC_BULK:
                blob = self._on_bulk(body)
                if blob is not None:
                    events.extend(self._handle_blob(*blob))
        if state is not None:
            state["hasWorld"] = world is not None
            state["u"], state["v"] = world if world is not None else (0.0, 0.0)
        if not has_group:
            events.append(("group", []))
        return events

    def _on_bulk(self, body: bytes) -> tuple[int, bytes] | None:
        r = Reader(body)
        channel_id, gen, index, count = r.u8(), r.u16(), r.u16(), r.u16()
        data = r.rest()
        ch = self._channels.get(channel_id)
        if ch is not None and ch.gen == gen and ch.count == count and ch.chunks.get(index, data) != data:
            ch = None  # same gen but different content: the addon restarted
        if ch is None or ch.gen != gen or ch.count != count:
            ch = _Channel(gen, count)
            self._channels[channel_id] = ch
        if ch.done:
            return None
        ch.chunks[index] = data
        if len(ch.chunks) < count:
            return None
        ch.done = True
        return channel_id, b"".join(ch.chunks[i] for i in range(count))

    def _handle_blob(self, channel_id: int, blob: bytes) -> list[tuple[str, object]]:
        if channel_id == CH_WORLD:
            return [("world", self._parse_world_blob(blob))]
        events: list[tuple[str, object]] = []
        map_event: dict = {}
        for rtype, body in iter_records(blob):
            if rtype == REC_SESSION:
                events.append(("session", parse_session(body)))
            elif rtype == REC_MAPINFO:
                map_event["info"] = parse_mapinfo(body)
            elif rtype == REC_EXPLORED:
                map_event["explored"] = parse_explored(body)["overlays"]
            elif rtype == REC_MAPRECT:
                map_event["rect"] = parse_maprect(body)["rect"]
            elif rtype == REC_ROSTER:
                events.append(("roster", parse_roster(body)))
        if map_event.get("info"):
            map_event.setdefault("explored", [])
            map_event.setdefault("rect", None)
            events.append(("map", map_event))
        return events

    @staticmethod
    def _parse_world_blob(blob: bytes) -> dict:
        """The world channel: Azeroth's map and each continent's map with its rect on it."""
        world = {"openmap": False, "worldMapID": 0, "openmapVersion": "", "maps": []}
        rects = {}
        for rtype, body in iter_records(blob):
            if rtype == REC_WORLDINFO:
                world.update(parse_worldinfo(body))
            elif rtype == REC_MAPINFO:
                world["maps"].append(parse_mapinfo(body))
            elif rtype == REC_MAPRECT:
                rect = parse_maprect(body)
                rects[rect["mapID"]] = rect["rect"]
        for info in world["maps"]:
            info["rect"] = [0.0, 1.0, 0.0, 1.0] if info["mapID"] == world["worldMapID"] else rects.get(info["mapID"])
        return world
