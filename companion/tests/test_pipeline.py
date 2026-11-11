"""End-to-end: the real addon Lua (in LuaJIT with a mock WoW API) -> pixels -> decoder."""
import math

import pytest

from mapcast.protocol import Decoder, crc16
from mapcast.sim import SimGame


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

    assert got["session"] == {"addonVersion": 1, "interface": 16001, "locale": "ruRU",
                              "player": "Тестер", "realm": "Тестовый мир", "classID": 8}
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
    assert {"session", "map", "roster", "state", "world"} <= got.keys()


def test_late_companion_still_gets_bulk_data():
    game = SimGame()
    run(game, Decoder(), 600)  # 20 s: the blobs are past their fast cycles
    decoder = Decoder()
    got = run(game, decoder, 150)  # 5 s of slow repeats
    assert {"session", "map", "roster", "world"} <= got.keys()


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
    assert {"session", "map", "roster", "world"} <= got.keys()
