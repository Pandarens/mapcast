"""MapCast companion: reads the addon's strip from the screen and serves the map page."""
from __future__ import annotations

import argparse
import asyncio
import json
import logging
import os
import secrets
import socket
import sys
import threading
import webbrowser

from aiohttp import web

from .app import Hub, Pipeline, create_app
from .casc import CascError, LocalCasc, flavor_product
from .icons import PinIcons
from .maps import MapImages
from .openmap import OpenMapArt, WorldImages, find_openmap

log = logging.getLogger("mapcast")

FLAVOR_DIR = "_forever_"


def data_dir(kind: str) -> str:
    base = os.environ.get("LOCALAPPDATA" if kind == "cache" else "APPDATA") or os.path.expanduser("~")
    path = os.path.join(base, "MapCast", kind)
    os.makedirs(path, exist_ok=True)
    return path


def load_token() -> str:
    path = os.path.join(data_dir("config"), "config.json")
    try:
        with open(path, encoding="utf-8") as f:
            token = json.load(f).get("token")
        if token:
            return token
    except (OSError, ValueError):
        pass
    token = secrets.token_urlsafe(6)
    with open(path, "w", encoding="utf-8") as f:
        json.dump({"token": token}, f)
    return token


def lan_ip() -> str:
    """The address phones on the same network can reach (no packets are sent)."""
    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    try:
        sock.connect(("10.255.255.255", 1))
        return sock.getsockname()[0]
    except OSError:
        return "127.0.0.1"
    finally:
        sock.close()


def find_wow_root(explicit: str | None) -> str | None:
    candidates = []
    if explicit:
        candidates.append(explicit)
    try:
        import winreg

        for hive_path in (r"SOFTWARE\WOW6432Node\Blizzard Entertainment\World of Warcraft",
                          r"SOFTWARE\Blizzard Entertainment\World of Warcraft"):
            try:
                with winreg.OpenKey(winreg.HKEY_LOCAL_MACHINE, hive_path) as key:
                    candidates.append(winreg.QueryValueEx(key, "InstallPath")[0])
            except OSError:
                pass
    except ImportError:
        pass
    for drive in "CDEFGH":
        for sub in (r"Program Files (x86)\World of Warcraft", r"Program Files\World of Warcraft",
                    r"World of Warcraft", r"Games\World of Warcraft"):
            candidates.append(f"{drive}:\\{sub}")
    for path in candidates:
        path = os.path.abspath(path)
        for _ in range(3):  # InstallPath may point at a flavor folder such as _retail_
            if os.path.exists(os.path.join(path, ".build.info")):
                return path
            path = os.path.dirname(path)
    return None


def open_casc(root: str | None) -> LocalCasc | None:
    if root is None:
        log.warning("WoW folder not found: maps will be grid placeholders (use --wow PATH)")
        return None
    product = flavor_product(os.path.join(root, FLAVOR_DIR))
    try:
        casc = LocalCasc(root, product)
    except (CascError, OSError, KeyError, ValueError) as e:
        log.warning("cannot read game data in %s: %s", root, e)
        return None
    log.info("game data: %s %s", casc.product, casc.version)
    return casc


def open_openmap(root: str | None, explicit: str | None, cache_dir: str) -> OpenMapArt | None:
    path = find_openmap(root, explicit)
    if path is None:
        log.info("OpenMap not found: the phone shows one zone at a time (use --openmap PATH)")
        return None
    try:
        art = OpenMapArt(path, cache_dir)
    except (OSError, ValueError) as e:
        log.warning("cannot read OpenMap in %s: %s", path, e)
        return None
    log.info("OpenMap %s: seamless world map from %s", art.version or "?", path)
    return art


def print_banner(url: str, token: str) -> None:
    link = f"{url}/?t={token}"
    print(f"\n  MapCast: open {link}\n  token: {token}\n")
    try:
        import qrcode

        qr = qrcode.QRCode(border=1)
        qr.add_data(link)
        sys.stdout.reconfigure(encoding="utf-8")
        qr.print_ascii(invert=True)
    except Exception:  # the QR in the terminal is a convenience only
        pass


def main() -> None:
    parser = argparse.ArgumentParser(prog="mapcast", description=__doc__)
    parser.add_argument("--wow", help="WoW install folder (the one with .build.info)")
    parser.add_argument("--openmap", help="OpenMap addon folder, if not in the game's AddOns")
    parser.add_argument("--addons", help="the game's AddOns folder, for map addons' pin icons (found by itself)")
    parser.add_argument("--port", type=int, default=8765)
    parser.add_argument("--fps", type=int, default=30, help="screen captures per second")
    parser.add_argument("--sim", action="store_true", help="simulate the game, no WoW needed")
    parser.add_argument("--no-browser", action="store_true", help="do not open the QR page on this PC")
    parser.add_argument("-v", "--verbose", action="store_true")
    args = parser.parse_args()
    logging.basicConfig(level=logging.DEBUG if args.verbose else logging.INFO,
                        format="%(asctime)s %(levelname)s %(message)s", datefmt="%H:%M:%S")

    token = load_token()
    url = f"http://{lan_ip()}:{args.port}"
    cache_dir = data_dir("cache")
    maps = MapImages(cache_dir, None)
    world = WorldImages(cache_dir, maps)
    icons = PinIcons(cache_dir, maps)
    hub = Hub(maps, world, icons)

    if args.sim:
        from .sim import SimSource

        source = SimSource()
    else:
        from .capture import ScreenSource

        source = ScreenSource()

    async def on_startup(app: web.Application) -> None:
        loop = asyncio.get_running_loop()
        pipeline = Pipeline(source, hub, loop, args.fps)
        pipeline.start()
        app["pipeline"] = pipeline

        def load_game_data() -> None:
            root = None if args.sim else find_wow_root(args.wow)
            if not args.sim:
                maps.casc = open_casc(root)
            world.openmap = open_openmap(root, args.openmap, cache_dir)
            icons.addons_dir = args.addons or (os.path.join(root, FLAVOR_DIR, "Interface", "AddOns") if root else None)
            asyncio.run_coroutine_threadsafe(hub.refresh(), loop)
            if world.openmap is not None:
                world.openmap.warm()  # builds the zone tiles once; later runs read them from the cache

        threading.Thread(target=load_game_data, daemon=True, name="game-data").start()

        print_banner(url, token)
        if not args.no_browser:
            webbrowser.open(f"http://127.0.0.1:{args.port}/qr")

    async def on_cleanup(app: web.Application) -> None:
        app["pipeline"].stopping.set()

    app = create_app(hub, token, url)
    app.on_startup.append(on_startup)
    app.on_cleanup.append(on_cleanup)
    web.run_app(app, host="0.0.0.0", port=args.port, print=None,
                access_log=logging.getLogger("aiohttp.access") if args.verbose else None)


if __name__ == "__main__":
    main()
