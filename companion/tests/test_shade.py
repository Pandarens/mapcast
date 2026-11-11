"""Unexplored land darkened: OpenMap's pieces, what the addon says you explored, the shade images."""
import asyncio
import os

import numpy as np
from aiohttp.test_utils import TestClient, TestServer
from PIL import Image

from mapcast.app import Hub, create_app
from mapcast.maps import MapImages
from mapcast.openmap import OpenMapArt, WorldImages
from mapcast.protocol import Decoder
from mapcast.shade import ZoneShades, parse_reveal
from mapcast.sim import SimGame
from test_openmap import WORLD_EVENT, make_openmap

REVEAL = """
local _, ns = ...
ns.MapReveal = {
	[1228] = {["500:334:0:0"] = "1, 2", ["502:334:500:0"] = "3", ["1002:334:0:334"] = "4, 5, 6, 7"},
	[1235] = {["1002:668:0:0"] = "8"},
}
"""


def test_parse_reveal():
    reveal = parse_reveal(REVEAL)
    assert reveal[1228][(500, 0, 502, 334)] == [3]
    assert reveal[1228][(0, 334, 1002, 334)] == [4, 5, 6, 7]


def openmap_with_reveal(root) -> str:
    addon = make_openmap(root)
    with open(os.path.join(addon, "Data", "Forever", "Reveal.lua"), "w", encoding="utf-8") as f:
        f.write(REVEAL)
    land = os.path.join(addon, "Media", "Textures", "Forever", "MapZoneLand")
    os.makedirs(land)
    mask = Image.new("RGBA", (8, 8), (255, 255, 255, 0))
    mask.paste((255, 255, 255, 255), (0, 0, 4, 8))  # the zone's land is its west half
    mask.save(os.path.join(land, "1431.tga"))
    return addon


ZONES = {1429: {"name": "Элвиннский лес", "parentMapID": 1415, "rect": [0.70, 0.78, 0.60, 0.68]},
         1431: {"name": "Сумеречный лес", "parentMapID": 1415, "rect": [0.71, 0.77, 0.66, 0.72]},
         1453: {"name": "Штормград", "parentMapID": 1415, "rect": [0.69, 0.71, 0.60, 0.62]}}
EXPLORED = {
    1429: {"art": 1228, "layer": [1002, 668], "pieces": [[0, 0, 500, 334]]},  # the north-west explored
    1431: {"art": 1235, "layer": [1002, 668], "pieces": []},  # not found at all
    1453: {"art": 9999, "layer": [1002, 668], "pieces": []},  # a city: no pieces, never shaded
}


def alpha_at(path, fx, fy):
    with Image.open(path) as im:
        im = im.convert("RGBA")
        return im.getpixel((min(im.width - 1, int(fx * im.width)), min(im.height - 1, int(fy * im.height))))[3]


def test_shades(tmp_path):
    shades = ZoneShades(str(tmp_path))
    shades.openmap = OpenMapArt(openmap_with_reveal(str(tmp_path)), str(tmp_path / "cache"))
    out = shades.register(EXPLORED, ZONES, (1002, 668))
    assert set(out) == {1429, 1431}
    elwynn, duskwood = out[1429], out[1431]
    assert not elwynn["missing"] and elwynn["rect"] == ZONES[1429]["rect"]
    assert duskwood["missing"] and duskwood["rect"][0] < 0.71 and duskwood["rect"][1] > 0.77  # and the land round it

    path = shades.shade_path(elwynn["url"].split("/")[-1][:-4])
    assert alpha_at(path, 0.2, 0.2) == 0  # explored: clear
    assert abs(alpha_at(path, 0.8, 0.2) - 158) <= 2 and abs(alpha_at(path, 0.5, 0.8) - 158) <= 2  # 0.62 of 255
    path = shades.shade_path(duskwood["url"].split("/")[-1][:-4])
    # the zone sits in the middle (REACH round it: 64 of the image's 188 units each side); its land is its west half
    assert alpha_at(path, 0.42, 0.5) > 150 and alpha_at(path, 0.6, 0.5) == 0 and alpha_at(path, 0.1, 0.5) == 0
    assert shades.shade_path("0" * 16) is None
    assert ZoneShades(str(tmp_path)).register(EXPLORED, ZONES, (1002, 668)) == {}  # without OpenMap: nothing


def test_shades_reach_the_page(tmp_path):
    om = OpenMapArt(openmap_with_reveal(str(tmp_path)), str(tmp_path / "cache"))
    maps = MapImages(str(tmp_path / "cache"), None)
    hub = Hub(maps, WorldImages(str(tmp_path / "cache"), maps, om))

    async def scenario():
        await hub.publish("explored", EXPLORED)
        assert hub.snapshot["explored"] == {}  # no world or zones yet
        await hub.publish("world", WORLD_EVENT)
        await hub.publish("zones", ZONES)
        shade = hub.snapshot["explored"]
        assert set(shade) == {"1429", "1431"}
        async with TestClient(TestServer(create_app(hub, "secret", "http://x"))) as client:
            reply = await client.get(shade["1429"]["url"] + "?t=secret")
            assert reply.status == 200 and reply.content_type == "image/png"

    asyncio.run(scenario())


def test_explored_channel_from_the_addon():
    game, decoder = SimGame(), Decoder()
    got = {}
    for _ in range(200):
        game.step(1 / 30)
        for name, data in decoder.feed(game.render()):
            got[name] = data
    explored = got["explored"]
    assert explored[1429]["art"] == 1228 and explored[1429]["layer"] == [1002, 668]
    assert explored[1429]["pieces"][0] == [0, 0, 485, 405] and len(explored[1429]["pieces"]) == 5  # not the mouse-over one
    assert explored[1431] == {"art": 1235, "layer": [1002, 668], "pieces": []}
    assert explored[1436]["art"] == 0

    game.lua.execute("table.insert(MockWoW.explored[1431], { textureWidth = 200, textureHeight = 200, offsetX = 10,"
                     " offsetY = 10, fileDataIDs = { 1 } })")
    game.mock.fire("MAP_EXPLORATION_UPDATED")
    for _ in range(60):
        game.step(1 / 30)
        for name, data in decoder.feed(game.render()):
            got[name] = data
    assert got["explored"][1431]["pieces"] == [[10, 10, 200, 200]]
