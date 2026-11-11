"""Unexplored land darkened on the seamless world, the way OpenMap's "dim" does it.

OpenMap's data lists every zone's explored-area pieces (Data/<client>/Reveal.lua,
by the zone art's UiMapArtID); the addon says which of them you have explored. A
zone you have not found at all gets a shade over its land and the land round it
that is no zone's (OpenMap's MapZoneLand and MapZoneBorder masks), from every zoom;
a zone you have been to gets a shade over each piece you have not explored. Each
shade is a dark image with an alpha mask, drawn over the zone art on the phone.
"""
from __future__ import annotations

import hashlib
import json
import os
import threading

import numpy as np
from PIL import Image, ImageFilter

from .luadata import read_table

UNDISCOVERED = (13, 15, 20)  # OpenMap's shade colour (Stitch.lua UNDISCOVERED: 0.05, 0.06, 0.08)
SHADE_ALPHA = 0.62  # ... and its strength
REACH = 64  # world units round a zone not found that its shade covers (Stitch.lua Shade.REACH)
SCALE = 0.5  # shade images at half the zone art's resolution: a soft mask needs no more
FEATHER = 10  # px (in the zone art) a piece's shade fades over at its edges


def parse_reveal(text: str) -> dict[int, dict[tuple[int, int, int, int], list[int]]]:
    """ns.MapReveal: [artID] = { ["w:h:x:y"] = "fileIDs" } -> {artID: {(x, y, w, h): [fileIDs]}}."""
    out = {}
    for art_id, pieces in read_table(text, "MapReveal").items():
        if not isinstance(pieces, dict):
            continue
        zone = {}
        for key, files in pieces.items():
            try:
                w, h, x, y = (int(v) for v in str(key).split(":"))
            except ValueError:
                continue
            zone[(x, y, w, h)] = [int(f) for f in str(files).replace(" ", "").split(",") if f.isdigit()]
        out[int(art_id)] = zone
    return out


class ZoneShades:
    """The shades for the web page, from what the addon says you explored."""

    def __init__(self, cache_dir: str):
        self.cache_dir = os.path.join(cache_dir, "shade")
        self.openmap = None
        self._specs: dict[str, dict] = {}
        self._lock = threading.Lock()
        os.makedirs(self.cache_dir, exist_ok=True)

    def register(self, explored: dict, zones: dict, world_size: tuple[int, int] | None) -> dict:
        """{mapID: {url, rect, missing}}: a shade per zone with land you haven't explored."""
        reveal = self.openmap.reveal() if self.openmap is not None else {}
        if not reveal or not world_size:
            return {}
        width, height = world_size
        out = {}
        for map_id, zone in explored.items():
            map_id = int(map_id)
            pieces = reveal.get(zone.get("art") or 0)
            info = zones.get(map_id) or zones.get(str(map_id))
            layer_w, layer_h = zone.get("layer") or (0, 0)
            if not pieces or not info or not info.get("rect") or layer_w <= 0 or layer_h <= 0:
                continue  # a city (no pieces), or not on the world
            have = {tuple(p) for p in zone.get("pieces", [])}
            unexplored = [key for key in pieces if key not in have]
            if not unexplored:
                continue
            l, r, t, b = info["rect"]
            missing = not (have & set(pieces))
            spec = {"map": map_id, "layer": [layer_w, layer_h], "missing": missing,
                    "pieces": sorted(unexplored), "explored": sorted(have & set(pieces))}
            if missing:
                # its rect grown by REACH world units each side, in its art's pixels
                spec["reach"] = [REACH * layer_w / max(1e-9, (r - l) * width),
                                 REACH * layer_h / max(1e-9, (b - t) * height)]
                spec["land"] = self.openmap.zone_mask("MapZoneLand", map_id)
                spec["border"] = self.openmap.zone_mask("MapZoneBorder", map_id)
                du, dv = REACH / width, REACH / height
                rect = [l - du, r + du, t - dv, b + dv]
            else:
                rect = [l, r, t, b]
            key = hashlib.sha1(json.dumps(spec, sort_keys=True).encode()).hexdigest()[:16]
            self._specs[key] = spec
            out[map_id] = {"url": f"/world/shade/{key}.png", "rect": rect, "missing": missing}
        return out

    def shade_path(self, key: str) -> str | None:
        path = os.path.join(self.cache_dir, f"{key}.png")
        if os.path.exists(path):
            return path
        spec = self._specs.get(key)
        if spec is None:
            return None
        with self._lock:
            if not os.path.exists(path):
                image = missing_shade(spec) if spec["missing"] else pieces_shade(spec)
                image.save(path + ".tmp", "PNG", optimize=True)
                os.replace(path + ".tmp", path)
        return path


def _shade(alpha: np.ndarray) -> Image.Image:
    """The shade colour over an alpha mask (0..1)."""
    h, w = alpha.shape
    rgba = np.empty((h, w, 4), np.uint8)
    rgba[..., :3] = UNDISCOVERED
    rgba[..., 3] = np.clip(alpha * SHADE_ALPHA * 255 + 0.5, 0, 255).astype(np.uint8)
    return Image.fromarray(rgba, "RGBA")


def _mask(path: str | None, size: tuple[int, int]) -> np.ndarray | None:
    if not path:
        return None
    with Image.open(path) as im:
        band = im.getchannel("A") if "A" in im.getbands() else im.convert("L")
        return np.asarray(band.resize(size, Image.BILINEAR), np.float32) / 255


def missing_shade(spec: dict) -> Image.Image:
    """A zone not found at all: its land, and its share of the land round it, shaded."""
    layer_w, layer_h = spec["layer"]
    reach_x, reach_y = spec["reach"]
    full = (layer_w + 2 * reach_x, layer_h + 2 * reach_y)
    size = (max(1, round(full[0] * SCALE)), max(1, round(full[1] * SCALE)))
    alpha = np.zeros((size[1], size[0]), np.float32)
    x0, y0 = round(reach_x * SCALE), round(reach_y * SCALE)
    x1, y1 = min(size[0], x0 + max(1, round(layer_w * SCALE))), min(size[1], y0 + max(1, round(layer_h * SCALE)))
    land = _mask(spec.get("land"), (x1 - x0, y1 - y0))
    alpha[y0:y1, x0:x1] = land if land is not None else 1.0
    border = _mask(spec.get("border"), size)
    if border is not None:
        alpha = np.maximum(alpha, border)
    return _shade(alpha)


def pieces_shade(spec: dict) -> Image.Image:
    """A zone you have been to: its unexplored pieces shaded, softly, never over an explored one."""
    layer_w, layer_h = spec["layer"]
    size = (max(1, round(layer_w * SCALE)), max(1, round(layer_h * SCALE)))

    def paint(pieces) -> np.ndarray:
        mask = Image.new("L", size, 0)
        for x, y, w, h in pieces:
            box = [round(x * SCALE), round(y * SCALE), round((x + w) * SCALE) - 1, round((y + h) * SCALE) - 1]
            mask.paste(255, box)
        blur = mask.filter(ImageFilter.GaussianBlur(FEATHER * SCALE / 2))
        return np.asarray(blur, np.float32) / 255

    shade = paint(spec["pieces"])
    if spec["explored"]:
        shade = shade * (1 - paint(spec["explored"]))
    return _shade(shade)
