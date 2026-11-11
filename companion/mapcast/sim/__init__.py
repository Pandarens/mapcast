"""Runs the real addon Lua against a mock WoW API and renders its strip to pixels.

Used by the tests and by `python -m mapcast --sim` to try the whole chain
without the game.
"""
from __future__ import annotations

import os
import time

import numpy as np

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.abspath(os.path.join(HERE, "..", "..", ".."))
ADDON_DIR = os.path.join(REPO, "addon", "MapCast")
ADDON_FILES = ("Protocol.lua", "Strip.lua", "MapCast.lua")


class SimGame:
    def __init__(self, phys=(1920, 1080), addon_dir: str = ADDON_DIR, openmap: bool = True):
        from lupa import luajit21

        self.lua = luajit21.LuaRuntime(unpack_returned_tuples=True)
        with open(os.path.join(HERE, "wowmock.lua"), encoding="utf-8") as f:
            self.lua.execute(f.read())
        self.mock = self.lua.globals().MockWoW
        self.mock.physW, self.mock.physH = phys
        if not openmap:
            self.lua.execute("OpenMapAPI = nil")
        self.ns = self.lua.table()
        load = self.lua.eval("function(src, name, ns) assert(loadstring(src, '@' .. name))('MapCast', ns) end")
        for name in ADDON_FILES:
            with open(os.path.join(addon_dir, name), encoding="utf-8") as f:
                load(f.read(), name, self.ns)
        self.mock.fire("ADDON_LOADED", "MapCast")
        self.mock.fire("PLAYER_ENTERING_WORLD")

    def step(self, dt: float) -> None:
        self.mock.step(dt)

    def slash(self, text: str) -> None:
        self.lua.globals().SlashCmdList.MAPCAST(text)

    def render(self, width: int = 1024, height: int = 48) -> np.ndarray:
        """The top-left screen corner as RGB, with the strip drawn the way the GPU would."""
        img = np.zeros((height, width, 3), np.uint8)
        img[:] = (40, 60, 40)  # "game world" behind the strip
        scale = self.mock.physH / 768.0
        for rect in self.mock.rects().values():
            x, y, w, h, r, g, b = (rect[i] for i in range(1, 8))
            x0, y0 = int(round(x * scale)), int(round(y * scale))
            x1, y1 = int(round((x + w) * scale)), int(round((y + h) * scale))
            img[y0:y1, x0:x1] = (round(r * 255), round(g * 255), round(b * 255))
        return img


class SimSource:
    """Drop-in replacement for capture.ScreenSource driven by SimGame."""

    def __init__(self, game: SimGame | None = None):
        self.game = game or SimGame()
        self.region = (1024, 48)
        self.window_found = True
        self._last = time.monotonic()

    def resize(self, width: int, height: int) -> None:
        self.region = (max(width, 16), max(height, 8))

    def grab(self) -> np.ndarray:
        now = time.monotonic()
        self.game.step(now - self._last)
        self._last = now
        return self.game.render(*self.region)
