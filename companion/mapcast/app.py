"""Glue: capture thread -> decoder -> hub -> web clients."""
from __future__ import annotations

import asyncio
import base64
import io
import json
import logging
import os
import re
import secrets
import threading
import time

from aiohttp import WSMsgType, web

from .capture import DEFAULT_REGION
from .icons import PinIcons
from .maps import MapImages
from .openmap import WorldImages
from .protocol import Decoder
from .shade import ZoneShades

log = logging.getLogger(__name__)

WEB_DIR = os.path.join(os.path.dirname(os.path.abspath(__file__)), "web")
COOKIE = "mapcast"
STALE_AFTER = 2.0


class Hub:
    """Latest game state plus the connected web clients."""

    def __init__(self, maps: MapImages, world: WorldImages, icons: PinIcons | None = None):
        self.maps = maps
        self.world = world
        self.icons = icons or PinIcons(maps.cache_dir, maps)
        self.shades = ZoneShades(maps.cache_dir)
        self._zones: dict = {}
        self.snapshot: dict = {"session": None, "map": None, "world": None, "zones": {}, "travel": None,
                               "pins": None, "explored": {}, "state": None, "group": [], "roster": {}, "status": {}}
        self.clients: set[web.WebSocketResponse] = set()
        self._raw: dict[str, dict] = {}

    async def publish(self, name: str, data) -> None:
        if name == "map":
            self._raw[name] = data
            data = self.maps.register(data)
        elif name == "world":
            self._raw[name] = data
            data = self.world.register(data)
        elif name == "pins":
            self._raw[name] = data
            data = self.icons.describe(data)
        elif name == "explored":
            self._raw[name] = data
            world = self.snapshot.get("world")
            self.shades.openmap = self.world.openmap
            shades = self.shades.register(data, self._zones, (world["width"], world["height"]) if world else None)
            data = {str(key): value for key, value in shades.items()}
        elif name in ("roster", "zones"):
            if name == "zones":
                self._zones = data
            data = {str(key): value for key, value in data.items()}
        elif name == "travel":
            data = {"nodes": {str(k): v for k, v in data["nodes"].items()},
                    "instances": {str(k): v for k, v in data["instances"].items()}}
        if self.snapshot.get(name) == data:
            return
        self.snapshot[name] = data
        await self._broadcast({"type": name, "data": data})
        if name in ("zones", "world") and "explored" in self._raw:  # the shades need the zones' rects
            await self.publish("explored", self._raw["explored"])

    async def refresh(self) -> None:
        """Rebuild the map and world descriptions, e.g. after game data or OpenMap finished loading."""
        for name, raw in list(self._raw.items()):
            await self.publish(name, raw)

    async def _broadcast(self, message: dict) -> None:
        text = json.dumps(message)
        for ws in list(self.clients):
            try:
                await ws.send_str(text)
            except (ConnectionError, RuntimeError):
                self.clients.discard(ws)


class Pipeline(threading.Thread):
    """Grabs frames, decodes pages and hands events to the hub's event loop."""

    def __init__(self, source, hub: Hub, loop: asyncio.AbstractEventLoop, fps: int):
        super().__init__(daemon=True, name="capture")
        self.source, self.hub, self.loop = source, hub, loop
        self.period = 1.0 / fps
        self.stopping = threading.Event()

    def _emit(self, name: str, data) -> None:
        asyncio.run_coroutine_threadsafe(self.hub.publish(name, data), self.loop)

    def run(self) -> None:
        decoder = Decoder()
        next_status, pages_at_status = 0.0, 0
        while not self.stopping.is_set():
            started = time.monotonic()
            try:
                img = self.source.grab()
                if img is not None:
                    for name, data in decoder.feed(img):
                        self._emit(name, data)
                    if decoder.geometry:
                        self.source.resize(*decoder.geometry.size_px)
                    else:
                        self.source.resize(*DEFAULT_REGION)
            except Exception:  # keep capturing whatever happens
                log.exception("capture failed")
            now = time.monotonic()
            if now >= next_status:
                streaming = now - decoder.last_page_time < STALE_AFTER
                if not self.source.window_found:
                    problem = "WoW window not found"
                elif not streaming:
                    problem = decoder.problem or "no new data from the addon"
                else:
                    problem = ""
                self._emit("status", {"window": self.source.window_found, "streaming": streaming,
                                      "problem": problem, "pagesPerSec": decoder.pages - pages_at_status,
                                      "badPages": decoder.bad_pages})
                next_status, pages_at_status = now + 1.0, decoder.pages
            self.stopping.wait(max(0.0, self.period - (time.monotonic() - started)))


def _qr_png(url: str) -> bytes:
    import qrcode

    out = io.BytesIO()
    qrcode.make(url, box_size=10, border=2).save(out, "PNG")
    return out.getvalue()


LOGIN_PAGE = """<!doctype html><html lang="ru"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1"><title>MapCast</title>
<style>body{font:16px system-ui;background:#16130f;color:#eee;display:grid;place-items:center;height:100vh;margin:0}
form{display:grid;gap:12px;padding:16px}input,button{font:inherit;padding:12px;border-radius:8px;border:1px solid #555}
button{background:#c9a24a;border:0;color:#111}</style></head><body>
<form method="get" action="/"><b>MapCast</b><label>Токен из окна компаньона на ПК</label>
<input name="t" autocomplete="off" autofocus><button>Открыть карту</button></form></body></html>"""

QR_PAGE = """<!doctype html><html lang="ru"><head><meta charset="utf-8"><title>MapCast</title>
<style>body{{font:18px system-ui;background:#16130f;color:#eee;text-align:center;padding:24px}}
img{{background:#fff;padding:12px;border-radius:12px}}code{{font-size:22px;color:#c9a24a}}</style></head><body>
<h2>Отсканируй телефоном или планшетом</h2><img src="data:image/png;base64,{qr}" alt="QR">
<p>или открой <code>{url}</code></p><p>токен: <code>{token}</code></p>
<p>Телефон должен быть в той же Wi-Fi сети, что и этот ПК.</p></body></html>"""


def create_app(hub: Hub, token: str, public_url: str) -> web.Application:
    def authorized(request: web.Request) -> bool:
        given = request.query.get("t") or request.cookies.get(COOKIE) or ""
        return secrets.compare_digest(given, token)

    def is_local(request: web.Request) -> bool:
        return request.remote in ("127.0.0.1", "::1")

    @web.middleware
    async def auth(request: web.Request, handler):
        path = request.path
        if path == "/qr":
            if not is_local(request):
                raise web.HTTPForbidden()
            return await handler(request)
        if path.startswith("/static/") or path == "/favicon.ico":
            return await handler(request)
        if not authorized(request):
            if path == "/":
                return web.Response(text=LOGIN_PAGE, content_type="text/html")
            raise web.HTTPForbidden()
        response = await handler(request)
        if request.query.get("t"):
            response.set_cookie(COOKIE, token, max_age=365 * 24 * 3600, samesite="Lax")
        return response

    async def _file(build, *args) -> web.FileResponse:
        """Serve a cached image, building it off the event loop. URLs change with their content."""
        path = await asyncio.get_running_loop().run_in_executor(None, build, *args)
        if path is None:
            raise web.HTTPNotFound()
        return web.FileResponse(path, headers={"Cache-Control": "max-age=31536000, immutable"})

    async def no_icon(_: web.Request):
        return web.Response(status=204)

    async def index(_: web.Request):
        return web.FileResponse(os.path.join(WEB_DIR, "index.html"), headers={"Cache-Control": "no-cache"})

    async def qr(_: web.Request):
        link = f"{public_url}/?t={token}"
        page = QR_PAGE.format(qr=base64.b64encode(_qr_png(link)).decode(), url=public_url, token=token)
        return web.Response(text=page, content_type="text/html")

    async def ws_handler(request: web.Request):
        ws = web.WebSocketResponse(heartbeat=20)
        await ws.prepare(request)
        hub.clients.add(ws)
        try:
            await ws.send_str(json.dumps({"type": "snapshot", "data": hub.snapshot}))
            async for msg in ws:
                if msg.type == WSMsgType.ERROR:
                    break
        finally:
            hub.clients.discard(ws)
        return ws

    async def map_image(request: web.Request):
        key = request.match_info["key"]
        if not re.fullmatch(r"[0-9a-f]{16}", key):
            raise web.HTTPNotFound()
        return await _file(hub.maps.image_path, key)

    async def world_art(request: web.Request):
        key = request.match_info["key"]
        if not re.fullmatch(r"[0-9a-f]{16}", key):
            raise web.HTTPNotFound()
        return await _file(hub.world.art_path, key)

    async def world_sea(_: web.Request):
        return await _file(hub.world.sea_path)

    async def world_shade(request: web.Request):
        key = request.match_info["key"]
        if not re.fullmatch(r"[0-9a-f]{16}", key):
            raise web.HTTPNotFound()
        return await _file(hub.shades.shade_path, key)

    async def pin_icon(request: web.Request):
        key = request.match_info["key"]
        if not re.fullmatch(r"[0-9a-f]{16}", key):
            raise web.HTTPNotFound()
        return await _file(hub.icons.icon_path, key)

    async def world_places(_: web.Request):
        blob = hub.world.places_json()
        if blob is None:
            raise web.HTTPNotFound()
        return web.Response(body=blob, content_type="application/json",
                            headers={"Cache-Control": "max-age=31536000, immutable"})

    async def zone_tile(request: web.Request):
        openmap = hub.world.openmap
        if openmap is None:
            raise web.HTTPNotFound()
        level, i, j = (int(request.match_info[k]) for k in ("level", "i", "j"))
        return await _file(openmap.zone_tile, level, i, j)

    app = web.Application(middlewares=[auth])
    app.router.add_get("/", index)
    app.router.add_get("/qr", qr)
    app.router.add_get("/ws", ws_handler)
    app.router.add_get("/map/{key}.png", map_image)
    app.router.add_get("/world/art/{key}.png", world_art)
    app.router.add_get("/world/sea.png", world_sea)
    app.router.add_get("/world/places.json", world_places)
    app.router.add_get("/icon/{key}.png", pin_icon)
    app.router.add_get("/world/shade/{key}.png", world_shade)
    app.router.add_get(r"/world/zone/{level:\d+}/{i:\d+}_{j:\d+}", zone_tile)
    app.router.add_get("/favicon.ico", no_icon)
    app.router.add_static("/static/", WEB_DIR)
    return app
