"""OpenMap's flights, docks, sailings and dungeon entrances: reading its Lua data and placing it on the world."""
import asyncio
import json
import os

import pytest
from aiohttp.test_utils import TestClient, TestServer

from mapcast.app import Hub, create_app
from mapcast.luadata import field, positional, read_table
from mapcast.maps import MapImages
from mapcast.openmap import OpenMapArt, WorldImages
from mapcast.places import YardsToWorld, build_places
from test_openmap import WORLD_EVENT, make_openmap


def test_lua_tables():
    text = r'''
    local _, ns = ...
    --[[ a block comment { not a table } ]]
    ns.Data = {
        list = { 1, -2.5, 0x10, 1e3, "a\"b\n", 'single', true, false, nil },   -- a comment
        [7] = { "x", y = SOME_CONSTANT, ["key with space"] = 3; },
        nested = { { 1, 2 }, { } },
        escaped = "\208\144",
    }
    ns.Other = { 1 }
    '''
    data = read_table(text, "Data")
    assert data["list"] == [1, -2.5, 16, 1000.0, 'a"b\n', "single", True, False, None]
    assert data[7] == {1: "x", "y": None, "key with space": 3}
    assert data["nested"] == [[1, 2], []]
    assert data["escaped"] == "А"  # \ddd are UTF-8 bytes
    assert read_table(text, "Other") == [1]
    assert positional(data[7]) == ["x"] and positional([4, 5]) == [4, 5] and positional(None) == []
    assert field(data, "missing", 9) == 9 and field([1], "x") is None
    with pytest.raises(ValueError):
        read_table(text, "Nope")


def corners(rect_yards, continent, swap=False):
    """The yards of a continent map's corners (0,0), (1,0), (0,1), as GetWorldPosFromMapPos gives them.

    `rect_yards` = (top worldX, left worldY, height, width): map x runs along -worldY, y along -worldX.
    """
    top, left, h, w = rect_yards

    def at(mx, my):
        return (top - my * h, left - mx * w) if not swap else (left - mx * w, top - my * h)

    return {"continent": continent, "p00": at(0, 0), "p10": at(1, 0), "p01": at(0, 1)}


CONTINENTS = {1415: corners((10000, 20000, 20000, 30000), 0), 1414: corners((10000, 20000, 20000, 30000), 1)}
RECTS = {1415: [0.5, 1.0, 0.0, 0.5], 1414: [0.0, 0.5, 0.25, 0.75]}


def test_yards_to_world():
    to_world = YardsToWorld(CONTINENTS, RECTS)
    assert to_world(0, 10000, 20000) == pytest.approx((0.5, 0.0))  # the top-left corner
    assert to_world(0, -10000, -10000) == pytest.approx((1.0, 0.5))  # the bottom-right one
    assert to_world(1, 0, 5000) == pytest.approx((0.25, 0.5))  # the middle
    assert to_world(5, 0, 0) is None
    # any order of the corners' axes works: they are solved, not assumed
    swapped = YardsToWorld({1415: corners((10000, 20000, 20000, 30000), 0, swap=True)}, RECTS)
    assert swapped(0, 20000 - 15000, 10000 - 5000) == pytest.approx((0.75, 0.125))
    assert not YardsToWorld(CONTINENTS, {})  # without the rects nothing can be placed


FLIGHTS = {
    "nodes": {
        2: ["Stormwind", 0, 10000, 20000, 1],
        4: ["Sentinel Hill", 0, 0, 5000, 1],
        23: ["Orgrimmar", 1, 0, 5000, 2],
        59: ["Dun Baldar", 30, 0, 0, 1],  # Alterac Valley: not on either continent
    },
    "paths": [
        [2, 4, 100, 10, [10000, 20000, 5000, 12000, 0, 5000]],
        [4, 2, 100, 10, [0, 5000, 10000, 20000]],  # the way back: drawn once
        [2, 59, 100, 10, [10000, 20000, 0, 0]],  # to a node off the map: not drawn
    ],
}

PLACES = {
    "instances": [
        [0, 0, 5000, [[36, "Deadmines", False, 16, 21]]],
        {1: None, 2: 0.2, 3: 0.3, 4: [[0, "The Drowned City", False, 35, 40]], "map": 1434},
        [1, 0, 5000, [[409, "Molten Core", True, 60, 60], [469, "Blackwing Lair", True, 60, 60]]],
        [9, 0, 0, [[1, "Nowhere", False, 1, 2]]],
    ],
    "bosses": {36: ["Rhahk'Zor", "Edwin VanCleef"], 409: ["Ragnaros"]},
    "transports": [
        ["boat", "Menethil - Theramore", 0, 0, 5000, 1, 0, 5000],
        ["airship", "Alterac - Zephras Isle", 0, 10000, 20000, 2991, 1, 1],
        {1: "zeppelin", 2: "Undercity", 3: 0, 4: 0, 5: 0, 6: 0, 7: 0, 8: 1000, "faction": "Horde"},
    ],
    "sailings": [
        ["boat", "Menethil - Theramore", [[0, [0, 5000, -5000, 0]], [1, [5000, 10000, 0, 5000]]]],
        ["zeppelin", "Grom'gol - Orgrimmar", [[0, [0, 0, 100, 100]]]],
        ["boat", "Booty Bay - Ratchet", [[0, [0, 0, 1, 1]], [7, [0, 0, 1, 1]]]],  # half off the map: dropped
        ["airship", "Alterac - Zephras Isle", [[0, [0, 0, 1, 1]]]],
    ],
}


def test_build_places():
    places = build_places(FLIGHTS, PLACES, YardsToWorld(CONTINENTS, RECTS))
    nodes = {n["id"]: n for n in places["flights"]["nodes"]}
    assert set(nodes) == {2, 4, 23}
    assert (nodes[2]["u"], nodes[2]["v"], nodes[2]["factions"]) == (0.5, 0.0, 1)
    assert [(p["a"], p["b"], len(p["pts"])) for p in places["flights"]["paths"]] == [(2, 4, 6)]

    dungeons = places["dungeons"]
    assert len(dungeons) == 3
    assert dungeons[0]["u"] == pytest.approx(0.75) and dungeons[0]["insts"][0]["bosses"] == ["Rhahk'Zor", "Edwin VanCleef"]
    assert dungeons[1] == {"map": 1434, "x": 0.2, "y": 0.3, "insts": [
        {"id": 0, "name": "The Drowned City", "raid": False, "min": 35, "max": 40, "bosses": []}]}
    assert [i["raid"] for i in dungeons[2]["insts"]] == [True, True]

    docks = places["docks"]
    assert [(d["kind"], d["faction"]) for d in docks] == [("boat", None), ("airship", None), ("zeppelin", "Horde")]
    assert docks[0]["to"] == pytest.approx([0.25, 0.5]) and docks[1]["to"] is None

    sailings = places["sailings"]
    assert [(s["route"], s["faction"]) for s in sailings] == [("Menethil - Theramore", "Alliance"),
                                                              ("Grom'gol - Orgrimmar", "Horde")]
    # two points on each continent and a bowed arc of 23 points over the sea between them
    assert len(sailings[0]["pts"]) == 2 * (2 + 23 + 2)
    assert build_places(None, None, YardsToWorld(CONTINENTS, RECTS)) == {
        "flights": {"nodes": [], "paths": []}, "docks": [], "sailings": [], "dungeons": []}


FLIGHT_LUA = """
local _, ns = ...
ns.MapFlights = {
	nodes = {
		[2] = { "Stormwind, Elwynn", 0, -8832.8, 478.6, 1 },
	},
	paths = {
	},
}
"""

PLACES_LUA = """
local _, ns = ...
ns.MapPlaces = {
	instances = {
		{ 0, -11207.8, 1681.2, { { 36, "Deadmines", false, 16, 21 } } },
	},
	unplaced = {},
	bosses = {},
	transports = {},
	sailings = {},
}
"""

YARDS = {1415: corners((11176.34, 18171.97, 27149.69, 40741.18), 0)}


def openmap_with_places(root) -> str:
    addon = make_openmap(root)
    data = os.path.join(addon, "Data", "Forever")
    for name, text in (("FlightData.lua", FLIGHT_LUA), ("Places.lua", PLACES_LUA)):
        with open(os.path.join(data, name), "w", encoding="utf-8") as f:
            f.write(text)
    return addon


def test_world_description_links_the_places(tmp_path):
    om = OpenMapArt(openmap_with_places(str(tmp_path)), str(tmp_path / "cache"))
    maps = MapImages(str(tmp_path / "cache"), None)
    world = WorldImages(str(tmp_path / "cache"), maps, om)
    event = {**WORLD_EVENT, "maps": WORLD_EVENT["maps"] + [
        {"mapID": 1415, "mapType": 2, "parentMapID": 947, "name": "Восточные королевства",
         "rect": [0.36, 1.16, 0.08, 0.88], "layers": WORLD_EVENT["maps"][1]["layers"]}]}

    assert world.register(event)["places"] is None  # an addon without the continents' yards
    desc = world.register({**event, "yards": YARDS})
    assert desc["places"]["url"].startswith("/world/places.json?v=")
    places = json.loads(world.places_json())
    assert [n["id"] for n in places["flights"]["nodes"]] == [2]
    assert places["dungeons"][0]["insts"][0]["name"] == "Deadmines"

    hub = Hub(maps, WorldImages(str(tmp_path / "cache"), maps, om))

    async def scenario():
        async with TestClient(TestServer(create_app(hub, "secret", "http://x"))) as client:
            assert (await client.get("/world/places.json?t=secret")).status == 404  # nothing registered yet
            await hub.publish("world", {**event, "yards": YARDS})
            reply = await client.get(hub.snapshot["world"]["places"]["url"] + "&t=secret")
            assert reply.status == 200 and (await reply.json())["flights"]["nodes"][0]["name"] == "Stormwind, Elwynn"
            await hub.publish("zones", {1429: {"name": "Элвиннский лес", "parentMapID": 1415, "rect": [0, 1, 0, 1]}})
            await hub.publish("travel", {"nodes": {2: {"name": "Штормград", "known": True}}, "instances": {36: "Мертвые копи"}})
            assert hub.snapshot["zones"]["1429"]["name"] == "Элвиннский лес"
            assert hub.snapshot["travel"] == {"nodes": {"2": {"name": "Штормград", "known": True}},
                                              "instances": {"36": "Мертвые копи"}}

    asyncio.run(scenario())


def test_real_openmap_data_parses_when_present():
    """OpenMap's own data files, when a copy is at hand (OPENMAP_DIR), all read and place."""
    addon = os.environ.get("OPENMAP_DIR")
    if not addon or not os.path.isdir(addon):
        pytest.skip("set OPENMAP_DIR to an OpenMap folder to run this")
    om = OpenMapArt(addon, os.path.join(os.environ.get("TEMP", "/tmp"), "mapcast-test-cache"))
    flights, places = om.places_data()
    built = build_places(flights, places, YardsToWorld(
        {1415: corners((11176.34, 18171.97, 27149.69, 40741.18), 0),
         1414: corners((12799.9, 17066.6, 24533.2, 36799.81), 1)},
        {1415: [0.32, 1.23, -0.04, 0.87], 1414: [-0.19, 0.63, 0.07, 0.89]}))
    assert len(built["flights"]["nodes"]) > 60 and len(built["flights"]["paths"]) > 100
    assert len(built["dungeons"]) > 20 and len(built["sailings"]) >= 8 and len(built["docks"]) > 15
