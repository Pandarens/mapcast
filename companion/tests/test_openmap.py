"""The seamless world: fast BLP decoding, OpenMap's files, the zone tile pyramid, the web routes."""
import asyncio
import io
import os
import random
import struct

import numpy as np
from aiohttp.test_utils import TestClient, TestServer
from PIL import Image

from mapcast.app import Hub, create_app
from mapcast.blp import open_blp
from mapcast.maps import MapImages
from mapcast.openmap import (OpenMapArt, WorldImages, find_openmap, parse_art_edits, parse_world_labels,
                             parse_zone_art)


def blp2(width, height, compression, alpha_depth, alpha_type, data):
    """A BLP2 file with one mipmap."""
    header = b"BLP2" + struct.pack("<I", 1) + bytes([compression, alpha_depth, alpha_type, 0])
    header += struct.pack("<II", width, height)
    offset = len(header) + 128 + 1024
    return header + struct.pack("<16I", offset, *[0] * 15) + struct.pack("<16I", len(data), *[0] * 15) + b"\0" * 1024 + data


def raw_blp(rgba: np.ndarray) -> bytes:
    h, w = rgba.shape[:2]
    return blp2(w, h, 3, 8, 8, rgba[..., [2, 1, 0, 3]].astype(np.uint8).tobytes())


def test_fast_blp_matches_pillow_on_dxt():
    rng = random.Random(1)
    for alpha_depth, alpha_type, block in ((0, 0, 8), (1, 0, 8), (8, 1, 16), (8, 7, 16)):
        data = bytes(rng.randrange(256) for _ in range(4 * block))  # 8 x 8 = 4 blocks, both DXT1 modes
        blp = blp2(8, 8, 2, alpha_depth, alpha_type, data)
        ours = np.asarray(open_blp(blp)).astype(int)
        pillow = np.asarray(Image.open(io.BytesIO(blp)).convert("RGBA")).astype(int)
        assert ours.shape == (8, 8, 4)
        assert np.abs(ours[..., 3] - pillow[..., 3]).max() == 0, (alpha_depth, alpha_type)
        # Pillow widens RGB565 without bit replication (255 -> 248), so allow that much
        assert np.abs(ours[..., :3] - pillow[..., :3]).max() <= 8, (alpha_depth, alpha_type)


def test_fast_blp_raw_and_fallback():
    rgba = np.random.default_rng(2).integers(0, 256, (4, 6, 4), dtype=np.uint8)
    assert (np.asarray(open_blp(raw_blp(rgba))) == rgba).all()
    out = io.BytesIO()
    Image.new("RGBA", (3, 3), (1, 2, 3, 4)).save(out, "PNG")
    assert open_blp(out.getvalue()).getpixel((1, 1)) == (1, 2, 3, 4)  # not BLP: Pillow reads it


ZONE_ART = """
local _, ns = ...
ns.MapZoneArt = {
	px = 16, size = 34, step = 32,   -- px per world unit; a tile's px
	desaturate = 0, colorMatch = 1,
	tiles = {   -- i * 256 + j: column i, row j
		0, 256, 1, 770,
	},
}
"""

ART_EDITS = """
ns.MapArtEdits = {
	[947] = { 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12 },
	[1414] = { 1, 2 },
}
"""

LABELS = """
ns.MapLabels = {
	{ map = 947, name = { map = 1414, text = "Kalimdor" }, fade = "land", ink = LAND_INK, glow = LAND_GLOW, glowA = 0.7,
		lines = { { 112, 332, 330, 20 } } },
	{ map = 947, name = { map = 1415, text = "Eastern Kingdoms" }, fade = "land", ink = LAND_INK, glow = LAND_GLOW, glowA = 0.7,
		lines = { { 660, 800, 387, 14 }, { 641, 816, 411.5, 14.5 } } },

	{ map = 1414, name = { area = 457, text = "The Veiled Sea" }, fade = "sea", ink = SEA_INK, glow = SEA_GLOW, glowA = 0.35,
		lines = { { 251, 287, 161, 11.5, true }, { 225, 314, 185, 17 } } },
}
"""

COLORS = {(0, 0): (200, 0, 0), (1, 0): (0, 200, 0), (0, 1): (0, 0, 200), (3, 2): (200, 200, 0)}


def make_openmap(root) -> str:
    """A small OpenMap folder: 34 px zone tiles (1 px border), world art tiles, data files."""
    addon = os.path.join(root, "_forever_", "Interface", "AddOns", "OpenMap")
    data = os.path.join(addon, "Data", "Forever")
    tex = os.path.join(addon, "Media", "Textures", "Forever")
    for sub in (data, os.path.join(tex, "MapZones"), os.path.join(tex, "MapArt")):
        os.makedirs(sub, exist_ok=True)
    for name, text in (("ZoneArt.lua", ZONE_ART), ("ArtEdits.lua", ART_EDITS), ("Labels.lua", LABELS)):
        with open(os.path.join(data, name), "w", encoding="utf-8") as f:
            f.write(text)
    with open(os.path.join(addon, "OpenMap.toc"), "w", encoding="utf-8") as f:
        f.write("## Title: OpenMap\n## Version: 0.8.3\n")
    for (i, j), color in COLORS.items():
        tile = np.zeros((34, 34, 4), np.uint8)
        tile[:] = (255, 0, 255, 255)  # the border: the neighbours' edge, never shown
        tile[1:33, 1:33] = (*color, 255)
        with open(os.path.join(tex, "MapZones", f"{i}_{j}.blp"), "wb") as f:
            f.write(raw_blp(tile))
    for n in range(1, 13):
        Image.new("RGB", (256, 256), (n * 20, 100, 50)).save(os.path.join(tex, "MapArt", f"947_{n}.tga"))
    for n in (1, 2):
        Image.new("RGB", (256, 256), (10, n * 100, 10)).save(os.path.join(tex, "MapArt", f"1414_{n}.tga"))
    Image.new("RGB", (16, 16), (130, 120, 90)).save(os.path.join(tex, "MapSeaFieldOut.tga"))
    return addon


def close(pixel, color, tolerance=14):
    return all(abs(a - b) <= tolerance for a, b in zip(pixel, color))


def test_parsers():
    art = parse_zone_art(ZONE_ART)
    assert art["px"] == 16 and art["size"] == 34 and art["step"] == 32
    assert art["tiles"] == [(0, 0), (0, 1), (1, 0), (3, 2)]
    assert parse_art_edits(ART_EDITS) == {947: set(range(1, 13)), 1414: {1, 2}}
    labels = parse_world_labels(LABELS)
    assert [(l["mapID"], l["text"]) for l in labels] == [(1414, "Kalimdor"), (1415, "Eastern Kingdoms")]
    assert labels[0]["x"] == 222 and labels[0]["y"] == 320 and labels[0]["size"] == 20
    assert labels[1]["y"] == (373 + 411.5) / 2


def test_zone_tile_pyramid(tmp_path):
    addon = make_openmap(str(tmp_path))
    assert find_openmap(str(tmp_path)) == os.path.abspath(addon)
    om = OpenMapArt(addon, str(tmp_path / "cache"))
    assert om.version == "0.8.3"
    assert [sorted(level) for level in om.levels] == [[(0, 0), (0, 1), (1, 0), (3, 2)], [(0, 0), (1, 1)], [(0, 0)]]
    desc = om.describe()
    assert desc["maxLevel"] == 2 and desc["unit"] == 2 and desc["tilePx"] == 32
    assert sorted(desc["tiles"]) == [0, 1, 256, 770] and desc["url"].endswith("?v=" + om.stamp)

    tile = Image.open(om.zone_tile(0, 1, 0)).convert("RGBA")
    assert tile.size == (32, 32)
    assert all(close(tile.getpixel((x, y)), (0, 200, 0, 255)) for x in (0, 16, 31) for y in (0, 16, 31))  # no border

    up = Image.open(om.zone_tile(1, 0, 0)).convert("RGBA")
    assert up.size == (32, 32)
    assert close(up.getpixel((8, 8)), (200, 0, 0, 255))  # (0, 0) top left
    assert close(up.getpixel((24, 8)), (0, 200, 0, 255))  # (1, 0) top right
    assert close(up.getpixel((8, 24)), (0, 0, 200, 255))  # (0, 1) bottom left
    assert up.getpixel((24, 24))[3] == 0  # (1, 1) has no art

    assert om.zone_tile(0, 2, 2) is None and om.zone_tile(5, 0, 0) is None
    om.warm(workers=2)
    assert os.path.exists(om.zone_tile(2, 0, 0))


WORLD_EVENT = {
    "openmap": True, "worldMapID": 947, "openmapVersion": "0.8.3",
    "maps": [
        {"mapID": 947, "mapType": 1, "parentMapID": 0, "name": "Азерот", "rect": [0, 1, 0, 1],
         "layers": [{"w": 1002, "h": 668, "tileW": 256, "tileH": 256, "tiles": list(range(1, 13))}]},
        {"mapID": 1414, "mapType": 2, "parentMapID": 947, "name": "Калимдор", "rect": [-0.17, 0.63, 0.05, 0.85],
         "layers": [{"w": 1002, "h": 668, "tileW": 256, "tileH": 256, "tiles": list(range(101, 113))}]},
    ],
}


def test_world_art_from_openmap(tmp_path):
    om = OpenMapArt(make_openmap(str(tmp_path)), str(tmp_path / "cache"))
    world = WorldImages(str(tmp_path / "cache"), MapImages(str(tmp_path / "cache"), None), om)
    desc = world.register(WORLD_EVENT)
    assert desc["width"] == 1002 and desc["height"] == 668 and desc["openmapInGame"]
    assert [(a["mapID"], a["kind"]) for a in desc["art"]] == [(947, "world"), (1414, "continent")]
    assert desc["art"][1]["rect"] == [-0.17, 0.63, 0.05, 0.85]
    assert [l["text"] for l in desc["labels"]] == ["Калимдор", "Eastern Kingdoms"]  # the game's name when it has one
    assert desc["zones"]["maxLevel"] == 2 and desc["sea"]["url"].startswith("/world/sea.png")

    image = Image.open(world.art_path(desc["art"][0]["url"].split("/")[-1][:-4]))
    assert image.size == (1002, 668)
    assert image.getpixel((500, 334))[:3] == (6 * 20, 100, 50) and image.getpixel((500, 334))[3] == 255  # tile 6
    assert image.getpixel((0, 0))[3] == 0  # the torn edge fades out
    continent = Image.open(world.art_path(desc["art"][1]["url"].split("/")[-1][:-4]))
    assert continent.getpixel((300, 128))[:3] == (10, 200, 10)  # OpenMap's tile 2; the rest has no art here
    assert world.sea_path() and Image.open(world.sea_path()).size == (16, 16)


def test_world_without_openmap_or_game_data(tmp_path):
    world = WorldImages(str(tmp_path), MapImages(str(tmp_path), None))
    desc = world.register(WORLD_EVENT)
    assert desc["zones"] is None and desc["labels"] == [] and desc["sea"]["url"] is None
    assert Image.open(world.art_path(desc["art"][0]["url"].split("/")[-1][:-4])).size == (1002, 668)
    assert world.register({"maps": [], "worldMapID": 947}) is None


def test_web_routes_serve_the_world(tmp_path):
    om = OpenMapArt(make_openmap(str(tmp_path)), str(tmp_path / "cache"))
    maps = MapImages(str(tmp_path / "cache"), None)
    hub = Hub(maps, WorldImages(str(tmp_path / "cache"), maps, om))

    async def scenario():
        await hub.publish("world", WORLD_EVENT)
        desc = hub.snapshot["world"]
        async with TestClient(TestServer(create_app(hub, "secret", "http://x"))) as client:
            assert (await client.get("/world/zone/0/0_0")).status == 403
            ok = await client.get("/world/zone/0/0_0?t=secret")
            assert ok.status == 200 and len(await ok.read()) > 0
            assert (await client.get("/world/zone/0/2_2?t=secret")).status == 404
            assert (await client.get("/world/zone/9/0_0?t=secret")).status == 404
            assert (await client.get(desc["art"][0]["url"] + "?t=secret")).status == 200
            assert (await client.get(desc["sea"]["url"] + "&t=secret")).status == 200

    asyncio.run(scenario())
