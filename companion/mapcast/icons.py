"""Pin icons for the phone: the textures map addons' pins use, cut and tinted as the game draws them.

A pin's icon is a game texture (a file ID, read through CASC) or a file of an
addon (`Interface\\AddOns\\<addon>\\...`, read from the game's AddOns folder).
Each distinct icon becomes a small PNG served at /icon/<key>.png.
"""
from __future__ import annotations

import hashlib
import json
import logging
import os
import re
import threading

from PIL import Image

from .blp import open_blp
from .maps import MapImages

log = logging.getLogger(__name__)

ICON_MAX = 64  # px: pins are drawn at about 16 px, twice that on a dense screen
EXTENSIONS = (".blp", ".tga", ".png", ".jpg", ".jpeg")


class PinIcons:
    def __init__(self, cache_dir: str, maps: MapImages, addons_dir: str | None = None):
        self.cache_dir = os.path.join(cache_dir, "icons")
        self.maps = maps
        self.addons_dir = addons_dir
        self._specs: dict[str, dict] = {}
        self._found: dict[str, str | None] = {}
        self._lock = threading.Lock()
        os.makedirs(self.cache_dir, exist_ok=True)

    def describe(self, pins: dict) -> dict:
        """The pins event for the web page: each icon replaced by its URL (None: the page draws a dot)."""
        urls = []
        for icon in pins.get("icons", []):
            source = self._source(icon)
            url = None
            if source is not None:
                spec = {"source": source, "coords": [round(c, 4) for c in icon["coords"]], "color": icon["color"]}
                key = hashlib.sha1(json.dumps(spec, sort_keys=True).encode()).hexdigest()[:16]
                self._specs[key] = spec
                url = f"/icon/{key}.png"
            urls.append(url)
        return {"sources": pins.get("sources", []), "icons": urls, "pins": pins.get("pins", [])}

    def _source(self, icon: dict) -> list | None:
        if icon.get("fileID") and self.maps.casc is not None:
            return ["casc", icon["fileID"]]
        if icon.get("path"):
            path = self.find_addon_file(icon["path"])
            if path:
                return ["file", path]
        return None

    def find_addon_file(self, game_path: str) -> str | None:
        """An addon's file on disk from its game path (any case, either slash, extension optional)."""
        if game_path in self._found:
            return self._found[game_path]
        found = None
        parts = [p for p in re.split(r"[\\/]+", game_path.strip()) if p]
        if self.addons_dir and len(parts) >= 3 and parts[0].lower() == "interface" and parts[1].lower() == "addons":
            folder = self.addons_dir
            for part in parts[2:-1]:
                folder = _child(folder, part, want_dir=True)
                if folder is None:
                    break
            if folder is not None:
                name = parts[-1]
                names = [name] if os.path.splitext(name)[1].lower() in EXTENSIONS else [name + e for e in EXTENSIONS]
                for candidate in names:
                    found = _child(folder, candidate, want_dir=False)
                    if found:
                        break
        self._found[game_path] = found
        return found

    def icon_path(self, key: str) -> str | None:
        """Path of an icon PNG, building it on first use. Blocking."""
        path = os.path.join(self.cache_dir, f"{key}.png")
        if os.path.exists(path):
            return path
        spec = self._specs.get(key)
        if spec is None:
            return None
        with self._lock:
            if not os.path.exists(path):
                image = self._build(spec)
                if image is None:
                    return None
                image.save(path + ".tmp", "PNG")
                os.replace(path + ".tmp", path)
        return path

    def _build(self, spec: dict) -> Image.Image | None:
        kind, ref = spec["source"]
        try:
            if kind == "casc":
                image = self.maps.texture(ref)
            elif ref.lower().endswith(".blp"):
                with open(ref, "rb") as f:
                    image = open_blp(f.read())
            else:
                with Image.open(ref) as im:
                    image = im.convert("RGBA")
        except (OSError, ValueError) as e:
            log.warning("pin icon %s: %s", ref, e)
            return None
        if image is None:
            return None
        return cut_and_tint(image, spec["coords"], spec["color"])


def cut_and_tint(image: Image.Image, coords: list[float], color: list[int]) -> Image.Image:
    """The part of a texture its coordinates (left, right, top, bottom) select, times its vertex colour."""
    left, right, top, bottom = coords
    w, h = image.size
    box = (round(min(left, right) * w), round(min(top, bottom) * h),
           round(max(left, right) * w), round(max(top, bottom) * h))
    if box[2] - box[0] >= 1 and box[3] - box[1] >= 1 and box != (0, 0, w, h):
        image = image.crop(box)
    if left > right:
        image = image.transpose(Image.FLIP_LEFT_RIGHT)
    if top > bottom:
        image = image.transpose(Image.FLIP_TOP_BOTTOM)
    if any(c != 255 for c in color):
        r, g, b, a = image.split()
        tint = [c / 255.0 for c in color]
        r, g, b, a = (band.point(lambda v, k=k: round(v * k)) for band, k in zip((r, g, b, a), tint))
        image = Image.merge("RGBA", (r, g, b, a))
    if max(image.size) > ICON_MAX:
        scale = ICON_MAX / max(image.size)
        image = image.resize((max(1, round(image.width * scale)), max(1, round(image.height * scale))), Image.LANCZOS)
    return image


def _child(folder: str, name: str, want_dir: bool) -> str | None:
    """A file or folder in `folder` by name, ignoring case (the game's paths are case-insensitive)."""
    exact = os.path.join(folder, name)
    if (os.path.isdir(exact) if want_dir else os.path.isfile(exact)):
        return exact
    try:
        entries = os.listdir(folder)
    except OSError:
        return None
    lower = name.lower()
    for entry in entries:
        path = os.path.join(folder, entry)
        if entry.lower() == lower and (os.path.isdir(path) if want_dir else os.path.isfile(path)):
            return path
    return None
