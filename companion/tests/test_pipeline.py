"""End-to-end: the real addon Lua (in LuaJIT with a mock WoW API) -> pixels -> decoder."""
import math

import pytest

from mapcast.places import YardsToWorld
from mapcast.protocol import Decoder, crc16
from mapcast.sim import SimGame

BULK = {"session", "map", "roster", "world", "zones", "travel"}


def run(game, decoder, frames, width=1024, height=48):
    latest = {}
    for _ in range(frames):
        game.step(1 / 30)
        for name, data in decoder.feed(game.render(width, height)):
            latest[name] = data
    return latest


def test_crc_matches_ccitt_false():
    assert crc16(b"123456789") == 0x29B1


def test_full_chain_decodes_everything():
    game, decoder = SimGame(), Decoder()
    got = run(game, decoder, 90)

    assert decoder.bad_pages == 0
    assert decoder.geometry.cell == 3 and decoder.geometry.cols == 256 and decoder.geometry.rows == 2

    assert got["session"] == {"addonVersion": 2, "interface": 16001, "locale": "ruRU", "player": "Тестер",
                              "realm": "Тестовый мир", "classID": 8, "faction": "Alliance", "level": 30}
    info = got["map"]["info"]
    assert info["mapID"] == 1429 and info["name"] == "Элвиннский лес" and info["parentMapID"] == 1415
    assert info["layers"][0]["tiles"] == [100000 + i for i in range(1, 13)]
    # the mouse-over-only overlay is not sent
    assert [o["files"] for o in got["map"]["explored"]] == [[200001, 200002]]
    assert got["roster"] == {101: {"classID": 1, "name": "Воин"}, 102: {"classID": 5, "name": "Жрица"}}

    mock, state = game.mock, got["state"]
    assert state["hasPos"] and state["hasFacing"] and state["mounted"] and not state["combat"]
    # the last decoded page is at most one tick (0.1 s) behind the mock
    assert abs(state["x"] - mock.px) < 0.01 and abs(state["y"] - mock.py) < 0.01
    assert abs(math.remainder(state["facing"] - mock.facing, 2 * math.pi)) < 0.1
    assert [m["unit"] for m in got["group"]] == [101, 102]


def test_position_precision_is_16_bit():
    game, decoder = SimGame(), Decoder()
    run(game, decoder, 3)
    game.mock.step = game.lua.eval("function(dt) end")  # freeze the walk
    for unit_x in (0.0, 0.123456, 1.0):
        game.mock.px = unit_x
        game.lua.globals().MockWoW.time = game.mock.time + 1
        game.ns.Tick()
        state = next(d for n, d in decoder.feed(game.render()) if n == "state")
        assert abs(state["x"] - unit_x) <= 1 / 65535


@pytest.mark.parametrize("commands", [["cell 1"], ["cell 4", "cols 128", "rows 4"], ["cell 8", "cols 64"]])
def test_strip_settings_still_decode(commands):
    game = SimGame()
    for command in commands:
        game.slash(command)
    decoder = Decoder()
    got = run(game, decoder, 200, width=1024, height=64)
    assert decoder.bad_pages == 0
    assert BULK | {"state"} <= got.keys()


def test_late_companion_still_gets_bulk_data():
    game = SimGame()
    run(game, Decoder(), 600)  # 20 s: the blobs are past their fast cycles
    decoder = Decoder()
    got = run(game, decoder, 240)  # 8 s of slow repeats
    assert BULK <= got.keys()


def test_reload_with_same_generation_is_detected():
    decoder = Decoder()

    def chunk(gen, data):
        body = bytes([2]) + gen.to_bytes(2, "little") + (0).to_bytes(2, "little") + (1).to_bytes(2, "little") + data
        return bytes([0x10]) + len(body).to_bytes(2, "little") + body

    def mapinfo(name):
        body = (5).to_bytes(4, "little") + bytes([3]) + (0).to_bytes(4, "little") + bytes([len(name)]) + name + bytes([0])
        return bytes([0x21]) + len(body).to_bytes(2, "little") + body

    first = decoder.handle_payload(chunk(7, mapinfo(b"A")))
    again = decoder.handle_payload(chunk(7, mapinfo(b"A")))
    changed = decoder.handle_payload(chunk(7, mapinfo(b"B")))
    names = lambda events: [d["info"]["name"] for n, d in events if n == "map"]
    assert names(first) == ["A"] and names(again) == [] and names(changed) == ["B"]


def test_blank_or_missing_strip_is_reported_not_counted():
    decoder = Decoder()
    import numpy as np

    assert decoder.feed(np.zeros((48, 1024, 3), np.uint8)) == []
    assert decoder.problem == "no strip in the top-left corner"
    assert decoder.bad_pages == 0


def test_distorted_colors_are_reported():
    game, decoder = SimGame(), Decoder()
    game.step(0.2)
    img = game.render().astype(int)
    img = (img * 0.92 + 8).clip(0, 255).astype("uint8")  # e.g. gamma / brightness changed
    decoder.feed(img)
    assert "distorted" in decoder.problem


def lua_rect(game, map_id):
    rect = game.mock.maps[map_id].rect
    return [rect[i] for i in range(1, 5)]


def test_world_channel_describes_azeroth_and_continents():
    game, decoder = SimGame(), Decoder()
    got = run(game, decoder, 150)
    world = got["world"]
    assert world["openmap"] and world["worldMapID"] == 947 and world["openmapVersion"] == "0.8.3"
    rects = {m["mapID"]: m["rect"] for m in world["maps"]}
    assert set(rects) == {947, 1414, 1415} and rects[947] == [0.0, 1.0, 0.0, 1.0]
    for map_id in (1414, 1415):
        assert rects[map_id] == pytest.approx(lua_rect(game, map_id), abs=1e-6)
    assert all(len(m["layers"][0]["tiles"]) == 12 for m in world["maps"])
    assert [m["name"] for m in world["maps"]] == ["Азерот", "Калимдор", "Восточные королевства"]
    # the current map's place on the world comes with the map
    assert got["map"]["rect"] == pytest.approx(lua_rect(game, 1429), abs=1e-6)
    assert got["state"]["hasWorld"]


@pytest.mark.parametrize("openmap", [True, False])
def test_world_position_is_exact(openmap):
    game, decoder = SimGame(openmap=openmap), Decoder()
    got = run(game, decoder, 150)
    assert got["world"]["openmap"] is openmap  # without OpenMap: Blizzard's GetMapRectOnMap
    game.mock.step = game.lua.eval("function(dt) end")  # freeze the walk
    left, right, top, bottom = lua_rect(game, 1429)
    for px, py in ((0.0, 0.5), (0.123456, 0.654321), (1.0, 1.0)):  # (0, 0) is WoW's 'no position'
        game.mock.px, game.mock.py = px, py
        game.ns.Tick()
        state = next(d for n, d in decoder.feed(game.render()) if n == "state")
        assert state["u"] == pytest.approx(left + px * (right - left), abs=1e-6)
        assert state["v"] == pytest.approx(top + py * (bottom - top), abs=1e-6)


def test_no_world_position_where_the_game_hides_it():
    game, decoder = SimGame(), Decoder()
    run(game, decoder, 10)
    game.lua.execute("C_Map.GetPlayerMapPosition = function() return nil end")
    got = run(game, decoder, 10)
    assert not got["state"]["hasPos"] and not got["state"]["hasWorld"]


def test_shrinking_the_strip_mid_stream_recuts_the_blobs():
    game = SimGame()
    run(game, Decoder(), 90)  # every blob already cut for the default 1.5 KB page
    game.slash("cell 8")
    game.slash("cols 64")  # a 375 byte page: the 400 byte chunks no longer fit
    decoder = Decoder()
    got = run(game, decoder, 200, width=1024, height=64)
    assert decoder.bad_pages == 0
    assert BULK <= got.keys()


def mock_yards(game, continent, wx, wy):
    """Where the mock puts a point in world yards on Azeroth's map."""
    k = game.mock.yards[continent]
    return k.a - k.b * wy, k.c - k.d * wx


def test_world_channel_places_the_continents_in_yards():
    game, decoder = SimGame(), Decoder()
    world = run(game, decoder, 150)["world"]
    yards = world["yards"]
    assert {m: y["continent"] for m, y in yards.items()} == {1414: 1, 1415: 0}
    to_world = YardsToWorld(yards, {m["mapID"]: m["rect"] for m in world["maps"]})
    for continent, wx, wy in ((0, -8832.8, 478.6), (1, 1677.6, -4315.7), (0, -14444.3, 509.6)):
        assert to_world(continent, wx, wy) == pytest.approx(mock_yards(game, continent, wx, wy), abs=1e-6)
    left, right, top, bottom = lua_rect(game, 1429)
    u, v = to_world(0, -8832.8, 478.6)  # Stormwind's flight master is in Elwynn's north-west
    assert left < u < (left + right) / 2 and top < v < (top + bottom) / 2
    assert to_world(2991, 1848.3, 569.1) is None  # a continent off the stitched world


def test_zones_channel_names_and_places_each_zone():
    game, decoder = SimGame(), Decoder()
    zones = run(game, decoder, 150)["zones"]
    assert set(zones) == {1411, 1412, 1413, 1426, 1429, 1431, 1432, 1434, 1436, 1440}
    assert zones[1429]["name"] == "Элвиннский лес" and zones[1429]["parentMapID"] == 1415
    for map_id, zone in zones.items():
        assert zone["rect"] == pytest.approx(lua_rect(game, map_id), abs=1e-6)


def test_travel_channel_learns_flight_masters():
    game, decoder = SimGame(), Decoder()
    travel = run(game, decoder, 150)["travel"]
    assert len(travel["nodes"]) == 16 and travel["nodes"][12]["name"] == "Темнолесье, Сумеречный лес"
    # WoW Forever lists them all as found: only those OpenMap saw as learned count
    assert sorted(i for i, n in travel["nodes"].items() if n["known"]) == [2, 4, 6]
    assert travel["instances"] == {34: "Тюрьма Штормграда", 36: "Мертвые копи", 90: "Гномреган",
                                   409: "Огненные Недра"}

    game.lua.execute("""MockWoW.taxi = { { "Штормград, Элвиннский лес", "CURRENT" },
        { "Темнолесье, Сумеречный лес", "REACHABLE" }, { "Телсамар, Лок Модан", "DISTANT" } }""")
    game.mock.fire("TAXIMAP_OPENED")
    travel = run(game, decoder, 60)["travel"]
    assert sorted(i for i, n in travel["nodes"].items() if n["known"]) == [2, 4, 6, 12]
    assert game.lua.eval("MapCastCharDB.flights['Темнолесье, Сумеречный лес']")


def test_travel_trusts_the_game_list_when_it_hides_some():
    game = SimGame()
    game.lua.execute("""OpenMapCharDB = nil
        local list = C_TaxiMap.GetTaxiNodesForMap
        C_TaxiMap.GetTaxiNodesForMap = function(mapID)
            local out = list(mapID)
            for _, node in ipairs(out) do node.isUndiscovered = node.nodeID ~= 5 end
            return out
        end""")
    travel = run(game, Decoder(), 150)["travel"]
    assert [i for i, n in travel["nodes"].items() if n["known"]] == [5]


def test_no_dungeon_names_from_a_client_that_ignores_the_id():
    game = SimGame()
    game.lua.execute('GetRealZoneText = function() return "Элвиннский лес" end')
    assert run(game, Decoder(), 150)["travel"]["instances"] == {}


def test_level_up_updates_the_session():
    game, decoder = SimGame(), Decoder()
    run(game, decoder, 30)
    game.mock.fire("PLAYER_LEVEL_UP", 31)
    assert run(game, decoder, 30)["session"]["level"] == 31


def world_of(game, map_id, x, y):
    left, right, top, bottom = lua_rect(game, map_id)
    return left + x * (right - left), top + y * (bottom - top)


def pins_by_source(pins):
    out = {}
    for pin in pins["pins"]:
        out.setdefault(pins["sources"][pin["source"]], []).append(pin)
    return out


def test_pins_channel_mirrors_the_map_addons():
    game, decoder = SimGame(), Decoder()
    pins = run(game, decoder, 200)["pins"]
    by = pins_by_source(pins)
    assert set(by) == {"TomTom", "SilverDragon", "RestedXP", "Questie", "HandyNotes", "HandyNotes: Treasures",
                       "GatherMate2: Herb Gathering", "GatherMate2: Mining"}  # not the disabled plugin

    tomtom = by["TomTom"]
    assert len(tomtom) == 1 and tomtom[0]["title"] == "Ферма Брекенридж"  # its HereBeDragons pin is not doubled
    assert (tomtom[0]["u"], tomtom[0]["v"]) == pytest.approx(world_of(game, 1429, 0.42, 0.66), abs=1e-6)
    assert pins["icons"][tomtom[0]["icon"]]["path"] == r"Interface\AddOns\TomTom\Images\GoldGreenDot"

    # Questie: the faded-out icon and the one in Ashenvale are left out; the quest is the subtitle
    questie = sorted(by["Questie"], key=lambda p: p["u"])
    assert [(p["title"], p["sub"]) for p in questie] == [
        ("Маршал Дуган", "Кобольды в шахте"), ("Волк", "Охота на волков"), ("Волк", "Охота на волков")]
    assert pins["icons"][questie[1]["icon"]]["path"].endswith("slay.blp")

    steps = sorted(by["RestedXP"], key=lambda p: p["label"])
    assert [(p["label"], p["active"], p["icon"]) for p in steps] == [("12", True, None), ("13", False, None)]
    assert steps[0]["title"] == "Поговорите с Маршалом Дуганом"

    # HandyNotes: your note's title from its save, a plugin's from its tooltip, a node of the next zone too
    assert by["HandyNotes"][0]["title"] == "Мой тайник"
    star = pins["icons"][by["HandyNotes"][0]["icon"]]
    assert star["coords"] == pytest.approx([0, 0.25, 0, 0.25], abs=1e-4)
    assert sorted(p["title"] for p in by["HandyNotes: Treasures"]) == ["Сундук в Западном Крае", "Сундук с сокровищами"]
    assert by["HandyNotes"][0]["size"] == 14  # 12 x its icon scale 1.2
    assert not game.lua.eval("GameTooltip:IsShown()")

    herbs = sorted(by["GatherMate2: Herb Gathering"], key=lambda p: p["u"])
    assert [p["title"] for p in herbs] == ["Мироцвет", "Сребролист"]
    assert (herbs[0]["u"], herbs[0]["v"]) == pytest.approx(world_of(game, 1429, 0.35, 0.6), abs=1e-6)
    assert by["GatherMate2: Mining"][0]["title"] == "Медная жила"
    assert pins["icons"][by["SilverDragon"][0]["icon"]]["fileID"] == 132320


def test_pins_follow_the_addons_every_two_seconds():
    game, decoder = SimGame(), Decoder()
    run(game, decoder, 90)
    game.lua.execute('TomTom.waypoints[1429]["new"] = { 1429, 0.5, 0.5, title = "Новая точка" }')
    pins = run(game, decoder, 150)["pins"]
    assert sorted(p["title"] for p in pins_by_source(pins)["TomTom"]) == ["Новая точка", "Ферма Брекенридж"]


def test_no_tooltips_read_in_combat():
    game = SimGame()
    game.lua.execute("MockWoW.combat = true")
    by = pins_by_source(run(game, Decoder(), 200)["pins"])
    assert all(p["title"] is None for p in by["HandyNotes: Treasures"])


def test_too_many_pins_keep_the_guide_and_waypoints():
    game = SimGame()
    game.lua.execute("""
        local nodes = {}
        for i = 1, 600 do
            local x, y = 0.2 + (i % 30) / 50, 0.2 + math.floor(i / 30) / 40
            nodes[math.floor(x * 10000 + 0.5) * 1000000 + math.floor(y * 10000 + 0.5) * 100] = 401
        end
        local get = LibStub("AceAddon-3.0"):GetAddon("GatherMate2").GetNodesForZone
        LibStub("AceAddon-3.0"):GetAddon("GatherMate2").GetNodesForZone = function(self, zone, t)
            if t == "Herb Gathering" and zone == 1429 then return next, nodes, nil end
            return get(self, zone, t)
        end""")
    pins = run(game, Decoder(), 300)["pins"]
    by = pins_by_source(pins)
    assert len(pins["pins"]) == 400
    assert len(by["TomTom"]) == 1 and len(by["RestedXP"]) == 2 and len(by["Questie"]) == 3


def test_no_map_addons_no_pins():
    pins = run(SimGame(map_addons=False), Decoder(), 150)["pins"]
    assert pins == {"sources": [], "icons": [], "pins": []}
