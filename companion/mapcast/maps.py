"""Builds map background images from game textures read out of CASC."""
from __future__ import annotations

import hashlib
import json
import logging
import math
import os
import threading

from PIL import Image, ImageDraw

from .blp import open_blp
from .casc import CascError, LocalCasc

log = logging.getLogger(__name__)

EXPLORED_TILE = 256
PARCHMENT = (214, 196, 158, 255)


class MapImages:
    """Composes and caches map PNGs. Works without CASC by drawing a grid placeholder."""

    def __init__(self, cache_dir: str, casc: LocalCasc | None):
        self.cache_dir = cache_dir
        self.casc = casc
        self._specs: dict[str, dict] = {}
        self._lock = threading.Lock()
        os.makedirs(cache_dir, exist_ok=True)

    def register(self, map_event: dict) -> dict:
        """Return the web description of a map; the image is built on first request."""
        info = map_event["info"]
        layer = info["layers"][0] if info["layers"] else {"w": 1002, "h": 668, "tileW": 256, "tileH": 256, "tiles": []}
        spec = {"info": info, "layer": layer, "explored": map_event.get("explored", []),
                "placeholder": self.casc is None or not layer["tiles"]}
        key = hashlib.sha1(json.dumps(spec, sort_keys=True).encode()).hexdigest()[:16]
        self._specs[key] = spec
        return {"mapID": info["mapID"], "name": info["name"], "mapType": info["mapType"],
                "parentMapID": info["parentMapID"], "width": layer["w"], "height": layer["h"],
                "url": f"/map/{key}.png", "placeholder": spec["placeholder"],
                "rect": map_event.get("rect")}  # where the map sits on the seamless world, if it does

    def image_path(self, key: str) -> str | None:
        """Path of the PNG for a registered key, composing it if needed (blocking)."""
        path = os.path.join(self.cache_dir, f"map_{key}.png")
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
        layer = spec["layer"]
        if spec["placeholder"]:
            return placeholder(layer["w"], layer["h"], spec["info"]["name"])
        tile_w, tile_h = layer["tileW"], layer["tileH"]
        cols = max(1, math.ceil(layer["w"] / tile_w))
        rows = max(1, math.ceil(len(layer["tiles"]) / cols))
        canvas = Image.new("RGBA", (cols * tile_w, rows * tile_h), PARCHMENT)
        for i, file_id in enumerate(layer["tiles"]):
            tile = self.texture(file_id)
            if tile is not None:
                canvas.paste(tile, ((i % cols) * tile_w, (i // cols) * tile_h))
        for overlay in spec["explored"]:
            self._draw_explored(canvas, overlay)
        return canvas.crop((0, 0, layer["w"], layer["h"]))

    def _draw_explored(self, canvas: Image.Image, overlay: dict) -> None:
        # Same tiling as Blizzard's MapExplorationPinMixin: 256 px pieces, last row/column cropped.
        wide = math.ceil(overlay["w"] / EXPLORED_TILE)
        tall = math.ceil(overlay["h"] / EXPLORED_TILE)
        for j in range(tall):
            piece_h = EXPLORED_TILE if j < tall - 1 else (overlay["h"] % EXPLORED_TILE or EXPLORED_TILE)
            for k in range(wide):
                piece_w = EXPLORED_TILE if k < wide - 1 else (overlay["w"] % EXPLORED_TILE or EXPLORED_TILE)
                index = j * wide + k
                if index >= len(overlay["files"]):
                    continue
                tile = self.texture(overlay["files"][index])
                if tile is None:
                    continue
                piece = tile.crop((0, 0, piece_w, piece_h))
                _composite(canvas, piece, overlay["x"] + k * EXPLORED_TILE, overlay["y"] + j * EXPLORED_TILE)

    def texture(self, file_id: int) -> Image.Image | None:
        """A game texture as RGBA, cached as PNG on disk."""
        if not file_id or self.casc is None:
            return None
        path = os.path.join(self.cache_dir, "tex", f"{file_id}.png")
        if os.path.exists(path):
            return Image.open(path).convert("RGBA")
        try:
            image = open_blp(self.casc.read_file(file_id))
        except (CascError, OSError, ValueError) as e:
            log.warning("texture %s: %s", file_id, e)
            return None
        os.makedirs(os.path.dirname(path), exist_ok=True)
        image.save(path, "PNG")
        return image


def _composite(canvas: Image.Image, piece: Image.Image, x: int, y: int) -> None:
    left, top = max(0, x), max(0, y)
    right, bottom = min(canvas.width, x + piece.width), min(canvas.height, y + piece.height)
    if right <= left or bottom <= top:
        return
    piece = piece.crop((left - x, top - y, right - x, bottom - y))
    canvas.alpha_composite(piece, (left, top))


def placeholder(width: int, height: int, name: str) -> Image.Image:
    image = Image.new("RGBA", (width, height), PARCHMENT)
    draw = ImageDraw.Draw(image)
    for i in range(1, 10):
        x, y = width * i // 10, height * i // 10
        draw.line([(x, 0), (x, height)], fill=(170, 150, 110, 255), width=1)
        draw.line([(0, y), (width, y)], fill=(170, 150, 110, 255), width=1)
    draw.rectangle([0, 0, width - 1, height - 1], outline=(120, 100, 70, 255), width=3)
    draw.text((16, 12), name, fill=(80, 60, 30, 255))
    return image
