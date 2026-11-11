"""Captures the top-left corner of the WoW window, where the addon draws the strip."""
from __future__ import annotations

import ctypes
import ctypes.wintypes as wt
import sys
import time

import numpy as np

WOW_TITLE = "World of Warcraft"
WOW_CLASSES = ("waApplication Window", "GxWindowClass", "GxWindowClassD3d")
DEFAULT_REGION = (1024, 48)


def make_dpi_aware() -> None:
    if sys.platform != "win32":
        return
    try:
        ctypes.windll.shcore.SetProcessDpiAwareness(2)
    except (AttributeError, OSError):
        ctypes.windll.user32.SetProcessDPIAware()


def find_wow_window() -> int | None:
    user32 = ctypes.windll.user32
    found: list[int] = []

    @ctypes.WINFUNCTYPE(wt.BOOL, wt.HWND, wt.LPARAM)
    def callback(hwnd, _):
        if not user32.IsWindowVisible(hwnd) or user32.IsIconic(hwnd):
            return True
        title = ctypes.create_unicode_buffer(256)
        user32.GetWindowTextW(hwnd, title, 256)
        cls = ctypes.create_unicode_buffer(256)
        user32.GetClassNameW(hwnd, cls, 256)
        if title.value == WOW_TITLE or cls.value in WOW_CLASSES:
            found.append(hwnd)
        return True

    user32.EnumWindows(callback, 0)
    return found[0] if found else None


def client_rect(hwnd: int) -> tuple[int, int, int, int] | None:
    """Screen position and size of a window's client area."""
    user32 = ctypes.windll.user32
    rect = wt.RECT()
    if not user32.GetClientRect(hwnd, ctypes.byref(rect)):
        return None
    origin = wt.POINT(0, 0)
    user32.ClientToScreen(hwnd, ctypes.byref(origin))
    return origin.x, origin.y, rect.right, rect.bottom


class ScreenSource:
    """Grabs the strip region of the WoW window as an RGB numpy array."""

    def __init__(self):
        import mss  # imported here so the simulator works without it

        make_dpi_aware()
        self._mss = mss.mss()
        self._hwnd: int | None = None
        self._next_search = 0.0
        self.region = DEFAULT_REGION
        self.window_found = False

    def resize(self, width: int, height: int) -> None:
        self.region = (max(width, 16), max(height, 8))

    def grab(self) -> np.ndarray | None:
        if self._hwnd is None or not ctypes.windll.user32.IsWindow(self._hwnd):
            self._hwnd = None
            if time.monotonic() >= self._next_search:
                self._next_search = time.monotonic() + 1.0
                self._hwnd = find_wow_window()
        rect = client_rect(self._hwnd) if self._hwnd else None
        self.window_found = rect is not None
        if rect is None:
            return None
        x, y, w, h = rect
        width, height = min(self.region[0], w), min(self.region[1], h)
        if width <= 0 or height <= 0:
            return None
        shot = self._mss.grab({"left": x, "top": y, "width": width, "height": height})
        return np.frombuffer(shot.raw, np.uint8).reshape(height, width, 4)[:, :, 2::-1]
