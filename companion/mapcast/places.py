"""Travel and dungeon places from OpenMap's data, placed on the seamless world.

OpenMap ships the game's flight network (Data/<client>/FlightData.lua) and its
dungeon entrances, docks and sailing routes (Data/<client>/Places.lua) in world
yards per continent, the coordinates UnitPosition and the taxi tables use. The
addon tells where each continent's map lies in those yards (three corners from
C_Map.GetWorldPosFromMapPos) and where the map lies on the world; together they
put every place on Azeroth's map, the way OpenMap's S.FromYards does.
"""
from __future__ import annotations

import math

from .luadata import field, positional

ASPECT = 1.5  # Azeroth's map is 1002 x 668 units: a step in v is 1.5 times a step in u
# goblin ports with a flight master for each faction (OpenMap Map/Routes.lua); every other boat is the Alliance's
NEUTRAL_ROUTES = {"Booty Bay - Ratchet", "Steamwheedle Port - Powderfuse Port"}
SAILING_KINDS = ("boat", "zeppelin")
DOCK_KINDS = ("boat", "zeppelin", "airship")


def _round(v: float) -> float:
    return round(v, 5)


class YardsToWorld:
    """World yards on a continent -> a point on Azeroth's map (0..1).

    `continents[mapID] = {"continent": id, "p00": (x, y), "p10": (x, y), "p01": (x, y)}`: the
    yards of the continent map's corners (0, 0), (1, 0) and (0, 1); `rects[mapID]` its rect on
    the world (left, right, top, bottom).
    """

    def __init__(self, continents: dict, rects: dict):
        self._by_continent = {}
        for map_id, c in continents.items():
            rect = rects.get(map_id)
            if not rect:
                continue
            (x0, y0), (x1, y1), (x2, y2) = c["p00"], c["p10"], c["p01"]
            e1, e2 = (x1 - x0, y1 - y0), (x2 - x0, y2 - y0)
            det = e1[0] * e2[1] - e1[1] * e2[0]
            if abs(det) < 1e-9:
                continue
            self._by_continent.setdefault(c["continent"], (x0, y0, e1, e2, det, rect))

    def __bool__(self) -> bool:
        return bool(self._by_continent)

    def __call__(self, continent, x: float, y: float) -> tuple[float, float] | None:
        found = self._by_continent.get(continent)
        if found is None:
            return None
        x0, y0, e1, e2, det, (l, r, t, b) = found
        dx, dy = x - x0, y - y0
        mx = (dx * e2[1] - dy * e2[0]) / det
        my = (e1[0] * dy - e1[1] * dx) / det
        return l + mx * (r - l), t + my * (b - t)


def _flat(points) -> list[float]:
    return [_round(c) for p in points for c in p]


def _flights(data, to_world: YardsToWorld) -> dict:
    nodes, at = [], {}
    for node_id, node in sorted((field(data, "nodes") or {}).items()):
        name, continent, x, y, factions = (positional(node) + [None] * 5)[:5]
        p = to_world(continent, x, y) if isinstance(x, (int, float)) else None
        if p is None:
            continue
        at[node_id] = continent
        nodes.append({"id": node_id, "name": name or "", "u": _round(p[0]), "v": _round(p[1]),
                      "factions": factions or 3})
    paths, seen = [], set()
    for path in positional(field(data, "paths")):
        start, end, _, _, pts = (positional(path) + [None] * 5)[:5]
        pair = (min(start, end), max(start, end)) if start in at and end in at else None
        if pair is None or pair in seen:
            continue  # each way is listed both ways, nearly the same: draw one
        way = [to_world(at[start], pts[k], pts[k + 1]) for k in range(0, len(pts or []) - 1, 2)]
        way = [p for p in way if p]
        if len(way) >= 2:
            seen.add(pair)
            paths.append({"a": start, "b": end, "pts": _flat(way)})
    return {"nodes": nodes, "paths": paths}


def _sailing_way(pieces, to_world: YardsToWorld) -> list[tuple[float, float]]:
    """The whole way of a vessel, sea crossings between continents bridged by a bowed arc (as OpenMap)."""
    way: list[tuple[float, float]] = []
    for piece in pieces:
        continent, pts = (positional(piece) + [None, None])[:2]
        first = len(way)
        for k in range(0, len(pts or []) - 1, 2):
            p = to_world(continent, pts[k], pts[k + 1])
            if p:
                way.append(p)
        if len(way) == first:
            return []  # half a route would stop out at sea
        if first > 0:
            (u0, v0), (u2, v2) = way[first - 1], way[first]
            du, dv = u2 - u0, v2 - v0
            length = math.hypot(du, dv * ASPECT)
            if length > 0:
                side = -1 if (v0 + v2) / 2 < 0.5 else 1
                nu, nv = -dv * ASPECT / length, du / length / ASPECT
                if nv * side < 0:
                    nu, nv = -nu, -nv
                cu, cv = (u0 + u2) / 2 + nu * 0.12 * length, (v0 + v2) / 2 + nv * 0.12 * length
                arc = []
                for k in range(1, 24):
                    t = k / 24
                    arc.append(((1 - t) ** 2 * u0 + 2 * (1 - t) * t * cu + t * t * u2,
                                (1 - t) ** 2 * v0 + 2 * (1 - t) * t * cv + t * t * v2))
                way[first:first] = arc
    return way


def _sailing_faction(kind: str, route: str) -> str:
    if kind == "zeppelin":
        return "Horde"
    return "Neutral" if route in NEUTRAL_ROUTES else "Alliance"


def _travel(data, to_world: YardsToWorld) -> tuple[list, list]:
    docks = []
    for dock in positional(field(data, "transports")):
        kind, route, continent, x, y, to_cont, to_x, to_y = (positional(dock) + [None] * 8)[:8]
        if kind not in DOCK_KINDS:
            continue
        p = to_world(continent, x, y)
        if p is None:
            continue
        dest = to_world(to_cont, to_x, to_y) if to_cont is not None else None
        docks.append({"kind": kind, "route": route or "", "u": _round(p[0]), "v": _round(p[1]),
                      "to": [_round(dest[0]), _round(dest[1])] if dest else None,
                      "faction": field(dock, "faction")})
    sailings = []
    for sail in positional(field(data, "sailings")):
        kind, route, pieces = (positional(sail) + [None] * 3)[:3]
        if kind not in SAILING_KINDS:
            continue
        way = _sailing_way(positional(pieces), to_world)
        if len(way) >= 2:
            sailings.append({"kind": kind, "route": route or "", "faction": _sailing_faction(kind, route or ""),
                             "pts": _flat(way)})
    return docks, sailings


def _dungeons(data, to_world: YardsToWorld) -> list:
    bosses = field(data, "bosses") or {}
    out = []
    for group in positional(field(data, "instances")):
        continent, x, y, insts = (positional(group) + [None] * 4)[:4]
        entry: dict = {}
        zone_map = field(group, "map")
        if zone_map:
            entry.update({"map": zone_map, "x": x, "y": y})  # a spot on a zone's map: the page places it
        else:
            p = to_world(continent, x, y)
            if p is None:
                continue
            entry.update({"u": _round(p[0]), "v": _round(p[1])})
        entry["insts"] = []
        for inst in positional(insts):
            inst_id, name, raid, lo, hi = (positional(inst) + [None] * 5)[:5]
            entry["insts"].append({"id": inst_id or 0, "name": name or "", "raid": bool(raid),
                                   "min": lo, "max": hi or lo,
                                   "bosses": [b for b in positional(bosses.get(inst_id)) if isinstance(b, str)]})
        if entry["insts"]:
            out.append(entry)
    return out


def build_places(flights, places, to_world: YardsToWorld) -> dict:
    """Everything the page draws over the world besides the art, in world coordinates (0..1)."""
    docks, sailings = _travel(places, to_world) if places else ([], [])
    return {
        "flights": _flights(flights, to_world) if flights else {"nodes": [], "paths": []},
        "docks": docks,
        "sailings": sailings,
        "dungeons": _dungeons(places, to_world) if places else [],
    }
