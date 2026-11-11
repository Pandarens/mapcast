"""The seamless world map, drawn the way OpenMap draws it.

OpenMap (https://github.com/openmap-world/openmap) stitches every zone onto
Azeroth's map (uiMap 947). Its addon folder holds the zones' art already baked
onto that world in 512 px tiles (Media/Textures/<client>/MapZones, laid out by
Data/<client>/ZoneArt.lua) and hand-cleaned art for the world and continent
maps (Media/Textures/<client>/MapArt, listed in Data/<client>/ArtEdits.lua).
This module reads those files and serves them to the phone: the world and
continent art as single images, the zone tiles as a pyramid of zoom levels so
a zoomed-out phone never loads hundreds of full-size tiles.
"""
from __future__ import annotations

import hashlib
import json
import logging
import math
import os
import re
import threading
from concurrent.futures import ThreadPoolExecutor

import numpy as np
from PIL import Image, features

from .blp import open_blp
from .luadata import read_table
from .maps import PARCHMENT, MapImages, placeholder
from .places import YardsToWorld, build_places
from .shade import parse_reveal

log = logging.getLogger(__name__)

CLIENT = "Forever"
ZONE_FORMAT = ("WEBP", "webp") if features.check("webp") else ("PNG", "png")
SEA_COLOR = (128, 125, 105)  # OpenMap's sea where no art paints one (Stitch.lua SEA)
SEA_REACH = 1.5  # how far past the world art its outer sea runs, in world widths (Stitch.lua)
FEATHER_EDGE = 0.08  # share of each side faded out when OpenMap's feather mask is not at hand


def find_openmap(wow_root: str | None, explicit: str | None = None, flavor_dir: str = "_forever_") -> str | None:
    """OpenMap's addon folder: the one given, or the one in the game's AddOns."""
    candidates = []
    if explicit:
        candidates += [explicit, os.path.join(explicit, "OpenMap")]
    if wow_root:
        candidates.append(os.path.join(wow_root, flavor_dir, "Interface", "AddOns", "OpenMap"))
    for path in candidates:
        if os.path.exists(os.path.join(path, "Data", CLIENT, "ZoneArt.lua")):
            return os.path.abspath(path)
    return None


def _read(path: str) -> str:
    with open(path, encoding="utf-8", errors="replace") as f:
        return f.read()


def _number(text: str, name: str) -> float:
    match = re.search(rf"\b{name}\s*=\s*([\d.]+)", text)
    if not match:
        raise ValueError(f"ZoneArt.lua has no {name}")
    return float(match.group(1))


def parse_zone_art(text: str) -> dict:
    """ns.MapZoneArt: px per world unit, tile size and step, and the tiles (i * 256 + j)."""
    tiles_block = re.search(r"\btiles\s*=\s*\{([^}]*)\}", text)
    if not tiles_block:
        raise ValueError("ZoneArt.lua has no tiles")
    keys = [int(k) for k in re.findall(r"\d+", re.sub(r"--[^\n]*", "", tiles_block.group(1)))]
    return {"px": _number(text, "px"), "size": int(_number(text, "size")), "step": int(_number(text, "step")),
            "tiles": sorted({(k // 256, k % 256) for k in keys})}


def parse_art_edits(text: str) -> dict[int, set[int]]:
    """ns.MapArtEdits: [uiMapID] = { tile, ... } (row-major, from 1)."""
    edits = {}
    for map_id, body in re.findall(r"\[(\d+)\]\s*=\s*\{([^}]*)\}", text):
        edits[int(map_id)] = {int(n) for n in re.findall(r"\d+", body)}
    return edits


def parse_world_labels(text: str) -> list[dict]:
    """The continents' names on the world map (ns.MapLabels entries with map = 947).

    Positions are the art's pixels (1002 x 668), so world units. The name itself
    is taken from the game later (its language); `text` is OpenMap's English one.
    """
    labels = []
    starts = [m.start() for m in re.finditer(r"\{\s*map\s*=\s*\d+\s*,\s*name\s*=", text)] + [len(text)]
    for start, end in zip(starts, starts[1:]):
        entry = text[start:end]
        head = re.match(r"\{\s*map\s*=\s*947\s*,\s*name\s*=\s*\{\s*map\s*=\s*(\d+)\s*,\s*text\s*=\s*\"([^\"]*)\"", entry)
        lines_at = entry.find("lines")
        if not head or lines_at < 0:
            continue
        lines = [tuple(float(v) for v in m) for m in
                 re.findall(r"\{\s*([\d.]+)\s*,\s*([\d.]+)\s*,\s*([\d.]+)\s*,\s*([\d.]+)", entry[lines_at:])]
        if not lines:
            continue
        left = min(line[0] for line in lines)
        right = max(line[1] for line in lines)
        top = min(line[2] - line[3] for line in lines)
        bottom = max(line[2] for line in lines)
        cap = sum(line[3] for line in lines) / len(lines)
        labels.append({"mapID": int(head.group(1)), "text": head.group(2),
                       "x": (left + right) / 2, "y": (top + bottom) / 2, "size": cap})
    return labels


def _toc_version(addon_dir: str) -> str:
    for name in os.listdir(addon_dir):
        if name.lower().endswith(".toc"):
            match = re.search(r"^##\s*Version:\s*(.+)$", _read(os.path.join(addon_dir, name)), re.M)
            if match:
                return match.group(1).strip()
    return ""


class OpenMapArt:
    """OpenMap's files on disk and the zone tile pyramid built from them."""

    def __init__(self, addon_dir: str, cache_dir: str):
        self.dir = addon_dir
        data = os.path.join(addon_dir, "Data", CLIENT)
        self.tex = os.path.join(addon_dir, "Media", "Textures", CLIENT)
        zone_art_path = os.path.join(data, "ZoneArt.lua")
        self.zone_art = parse_zone_art(_read(zone_art_path))
        edits_path = os.path.join(data, "ArtEdits.lua")
        self.art_edits = parse_art_edits(_read(edits_path)) if os.path.exists(edits_path) else {}
        labels_path = os.path.join(data, "Labels.lua")
        self.labels = parse_world_labels(_read(labels_path)) if os.path.exists(labels_path) else []
        self.version = _toc_version(addon_dir)
        self.feather = self._texture_path("MapFeatherWide")
        self.sea = self._texture_path("MapSeaFieldOut")

        stat = os.stat(zone_art_path)
        stamp = f"{self.version}|{stat.st_size}|{int(stat.st_mtime)}|{ZONE_FORMAT[1]}"
        self.stamp = hashlib.sha1(stamp.encode()).hexdigest()[:12]
        self.cache_dir = os.path.join(cache_dir, "openmap", self.stamp)
        os.makedirs(self.cache_dir, exist_ok=True)

        self.levels = [set(self.zone_art["tiles"])]
        while len(self.levels[-1]) > 1:
            self.levels.append({(i >> 1, j >> 1) for i, j in self.levels[-1]})
        self._locks: dict[tuple, threading.Lock] = {}
        self._locks_guard = threading.Lock()
        self._data_dir = data
        self._places = None
        self._reveal = None

    def _texture_path(self, name: str) -> str | None:
        for ext in (".tga", ".blp", ".png"):
            path = os.path.join(self.tex, name + ext)
            if os.path.exists(path):
                return path
        return None

    # -- description for the web page ---------------------------------------

    @property
    def tile_px(self) -> int:
        """Pixels of a tile that belong to it (the rest of the file repeats its neighbours' edge)."""
        return self.zone_art["step"]

    def describe(self) -> dict:
        za = self.zone_art
        return {"version": self.version, "unit": za["step"] / za["px"], "tilePx": self.tile_px,
                "maxLevel": len(self.levels) - 1, "tiles": [i * 256 + j for i, j in za["tiles"]],
                "url": "/world/zone/{level}/{i}_{j}?v=" + self.stamp}

    def places_data(self) -> tuple | None:
        """OpenMap's flight network and its places (dungeon entrances, docks, sailings), read once."""
        if self._places is None:
            found = []
            for file, table in (("FlightData.lua", "MapFlights"), ("Places.lua", "MapPlaces")):
                path = os.path.join(self._data_dir, file)
                try:
                    found.append(read_table(_read(path), table) if os.path.exists(path) else None)
                except (ValueError, IndexError) as e:
                    log.warning("OpenMap %s: %s", file, e)
                    found.append(None)
            self._places = tuple(found) if any(found) else False
        return self._places or None

    def reveal(self) -> dict:
        """Every zone art's explored-area pieces (Data/<client>/Reveal.lua), read once."""
        if self._reveal is None:
            path = os.path.join(self._data_dir, "Reveal.lua")
            try:
                self._reveal = parse_reveal(_read(path)) if os.path.exists(path) else {}
            except (ValueError, IndexError) as e:
                log.warning("OpenMap Reveal.lua: %s", e)
                self._reveal = {}
        return self._reveal

    def zone_mask(self, folder: str, map_id: int) -> str | None:
        """One of OpenMap's per-zone masks (MapZoneLand: its land; MapZoneBorder: the land round it)."""
        return self._texture_path(os.path.join(folder, str(map_id)))

    def edited_art(self, map_id: int, number: int) -> str | None:
        """OpenMap's hand-cleaned copy of an art tile (row-major, from 1), if it has one."""
        if number not in self.art_edits.get(map_id, ()):
            return None
        for ext in (".tga", ".blp", ".png"):
            path = os.path.join(self.tex, "MapArt", f"{map_id}_{number}{ext}")
            if os.path.exists(path):
                return path
        return None

    # -- the zone tile pyramid ----------------------------------------------

    def has_tile(self, level: int, i: int, j: int) -> bool:
        return 0 <= level < len(self.levels) and (i, j) in self.levels[level]

    def zone_tile(self, level: int, i: int, j: int) -> str | None:
        """Path of a pyramid tile, building it (and what it needs) on first use. Blocking."""
        if not self.has_tile(level, i, j):
            return None
        path = os.path.join(self.cache_dir, f"z{level}", f"{i}_{j}.{ZONE_FORMAT[1]}")
        if os.path.exists(path):
            return path
        with self._lock_for((level, i, j)):
            if not os.path.exists(path):
                image = self._build(level, i, j)
                os.makedirs(os.path.dirname(path), exist_ok=True)
                tmp = path + ".tmp"
                if ZONE_FORMAT[0] == "WEBP":
                    image.save(tmp, "WEBP", quality=86, method=2)
                else:
                    image.save(tmp, "PNG")
                os.replace(tmp, path)
        return path

    def warm(self, workers: int = 4) -> None:
        """Build every tile ahead of time, finest level first (each level reuses the one below)."""
        def build(level: int, i: int, j: int) -> None:
            try:
                self.zone_tile(level, i, j)
            except (OSError, ValueError) as e:
                log.warning("OpenMap tile %d/%d_%d: %s", level, i, j, e)

        with ThreadPoolExecutor(workers, thread_name_prefix="openmap") as pool:
            for level, tiles in enumerate(self.levels):
                list(pool.map(lambda key: build(level, *key), sorted(tiles)))

    def _lock_for(self, key: tuple) -> threading.Lock:
        with self._locks_guard:
            return self._locks.setdefault(key, threading.Lock())

    def _build(self, level: int, i: int, j: int) -> Image.Image:
        px = self.tile_px
        if level == 0:
            size = self.zone_art["size"]
            inset = (size - px) // 2
            with open(os.path.join(self.tex, "MapZones", f"{i}_{j}.blp"), "rb") as f:
                tile = open_blp(f.read())
            if tile.size != (size, size):
                tile = tile.resize((size, size), Image.LANCZOS)
            return tile.crop((inset, inset, inset + px, inset + px))
        half = px // 2
        out = Image.new("RGBA", (half * 2, half * 2), (0, 0, 0, 0))
        for dx in (0, 1):
            for dy in (0, 1):
                child = self.zone_tile(level - 1, i * 2 + dx, j * 2 + dy)
                if child is None:
                    continue
                with Image.open(child) as im:
                    out.paste(im.convert("RGBA").resize((half, half), Image.BOX), (dx * half, dy * half))
        return out if out.size == (px, px) else out.resize((px, px), Image.LANCZOS)


class WorldImages:
    """The seamless world for the web page: art images plus OpenMap's zone tiles when present."""

    def __init__(self, cache_dir: str, maps: MapImages, openmap: OpenMapArt | None = None):
        self.root_cache = cache_dir
        self.cache_dir = os.path.join(cache_dir, "world")
        self.maps = maps
        self.openmap = openmap
        self._specs: dict[str, dict] = {}
        self._lock = threading.Lock()
        self._places: tuple[str, bytes] | None = None
        os.makedirs(self.cache_dir, exist_ok=True)

    def register(self, world: dict) -> dict | None:
        """The web description of the world; images are built on first request."""
        maps = [m for m in world.get("maps", []) if m.get("rect") and m.get("layers")]
        base = next((m for m in maps if m["mapID"] == world.get("worldMapID")), None)
        if base is None:
            return None
        layer = base["layers"][0]
        art = []
        feather = self.openmap.feather if self.openmap else None
        for info in sorted(maps, key=lambda m: m["mapID"] != base["mapID"]):
            spec = {"info": info, "edited": self._edited_tiles(info), "casc": self.maps.casc is not None,
                    "feather": feather}
            key = hashlib.sha1(json.dumps(spec, sort_keys=True).encode()).hexdigest()[:16]
            self._specs[key] = spec
            art.append({"mapID": info["mapID"], "name": info["name"], "rect": info["rect"],
                        "kind": "world" if info is base else "continent", "url": f"/world/art/{key}.png"})
        names = {m["mapID"]: m["name"] for m in maps}
        labels = []
        if self.openmap is not None:
            for label in self.openmap.labels:
                labels.append({**label, "text": names.get(label["mapID"]) or label["text"]})
        sea = {"color": SEA_COLOR, "reach": SEA_REACH,
               "url": f"/world/sea.png?v={self.openmap.stamp}" if self.openmap and self.openmap.sea else None}
        return {"worldMapID": base["mapID"], "width": layer["w"], "height": layer["h"], "art": art, "sea": sea,
                "labels": labels, "zones": self.openmap.describe() if self.openmap else None,
                "places": self._register_places(world, maps),
                "openmapInGame": world.get("openmap", False), "openmapVersion": world.get("openmapVersion", "")}

    def _register_places(self, world: dict, maps: list[dict]) -> dict | None:
        """OpenMap's flights, docks, sailings and dungeon entrances, placed by the continents' yards."""
        data = self.openmap.places_data() if self.openmap else None
        to_world = YardsToWorld(world.get("yards") or {}, {m["mapID"]: m["rect"] for m in maps})
        if not data or not to_world:
            self._places = None
            return None
        blob = json.dumps(build_places(*data, to_world), separators=(",", ":")).encode()
        key = hashlib.sha1(blob).hexdigest()[:12]
        self._places = (key, blob)
        return {"url": f"/world/places.json?v={key}"}

    def places_json(self) -> bytes | None:
        return self._places[1] if self._places else None

    def _edited_tiles(self, info: dict) -> dict[int, str]:
        if self.openmap is None:
            return {}
        count = len(info["layers"][0]["tiles"])
        found = {n: self.openmap.edited_art(info["mapID"], n) for n in range(1, count + 1)}
        return {n: path for n, path in found.items() if path}

    def art_path(self, key: str) -> str | None:
        """Path of a world or continent art PNG, composing it if needed. Blocking."""
        path = os.path.join(self.cache_dir, f"art_{key}.png")
        if os.path.exists(path):
            return path
        spec = self._specs.get(key)
        if spec is None:
            return None
        with self._lock:
            if not os.path.exists(path):
                image = self._compose(spec)
                tmp = path + ".tmp"
                image.save(tmp, "PNG")
                os.replace(tmp, path)
        return path

    def _compose(self, spec: dict) -> Image.Image:
        info = spec["info"]
        layer = info["layers"][0]
        tile_w, tile_h = layer["tileW"], layer["tileH"]
        cols = max(1, math.ceil(layer["w"] / tile_w))
        rows = max(1, math.ceil(len(layer["tiles"]) / cols))
        canvas = Image.new("RGBA", (cols * tile_w, rows * tile_h), PARCHMENT)
        drawn = 0
        for index, file_id in enumerate(layer["tiles"]):
            tile = None
            edited = spec["edited"].get(index + 1)
            if edited:
                with Image.open(edited) as im:
                    tile = im.convert("RGBA")
            else:
                tile = self.maps.texture(file_id)
            if tile is not None:
                if tile.size != (tile_w, tile_h):
                    tile = tile.resize((tile_w, tile_h), Image.LANCZOS)
                canvas.paste(tile, ((index % cols) * tile_w, (index // cols) * tile_h))
                drawn += 1
        if drawn == 0:
            return placeholder(layer["w"], layer["h"], info["name"])
        image = canvas.crop((0, 0, layer["w"], layer["h"]))
        # the torn paper edge fades out, as OpenMap draws the world and continent art
        alpha = feather_mask(image.size, spec.get("feather"))
        image.putalpha(Image.fromarray((np.asarray(image.getchannel("A"), np.float32) *
                                        np.asarray(alpha, np.float32) / 255).astype(np.uint8)))
        return image

    def sea_path(self) -> str | None:
        """OpenMap's outer sea (the world's sea colour carried past its edges) as PNG."""
        if self.openmap is None or self.openmap.sea is None:
            return None
        path = os.path.join(self.openmap.cache_dir, "sea.png")
        with self._lock:
            if not os.path.exists(path):
                with Image.open(self.openmap.sea) as im:
                    im.convert("RGB").save(path + ".tmp", "PNG")
                os.replace(path + ".tmp", path)
        return path


def feather_mask(size: tuple[int, int], path: str | None) -> Image.Image:
    """An alpha mask fading a map's edges: OpenMap's own when given, else a smooth ramp."""
    if path:
        with Image.open(path) as im:
            return im.getchannel("A").resize(size, Image.BILINEAR)
    w, h = size

    def ramp(n: int) -> np.ndarray:
        edge = max(1.0, n * FEATHER_EDGE)
        t = np.clip(np.minimum(np.arange(n) + 0.5, n - np.arange(n) - 0.5) / edge, 0, 1)
        return t * t * (3 - 2 * t)

    return Image.fromarray((np.outer(ramp(h), ramp(w)) * 255).astype(np.uint8))
