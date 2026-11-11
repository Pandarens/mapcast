# MapCast pixel protocol v1

The WoW addon sandbox has no network or file I/O at runtime, so the addon
paints a small grid of colored squares ("the strip") in the top-left corner of
the game window. The companion app captures that region and decodes it.

## Strip geometry

- `W x R` cells, each `S x S` physical pixels, anchored at the top-left pixel
  of the WoW client area. Cells are row-major.
- Cell 0 is magenta `(255,0,255)`, cell 1 is green `(0,255,0)`. The decoder
  finds `S` from the length of the magenta run on pixel row 0.
- Cell 2 is geometry: `(W & 255, W >> 8, R)`.
- Cells 3.. carry the page bytes, 3 bytes per cell in R, G, B order. Unused
  cells are black.

Defaults: `S = 3`, `W = 256`, `R = 2` (a 768 x 6 px strip, about 1.5 KB per page).

## Page

All integers are little-endian.

| offset | size | field |
|---|---|---|
| 0 | 1 | version (1) |
| 1 | 1 | seq, increments on every new page |
| 2 | 2 | payload length `L` |
| 4 | L | payload: a list of records |
| 4+L | 2 | CRC16-CCITT-FALSE (poly 0x1021, init 0xFFFF) of bytes `0 .. 4+L-1` |

The addon repaints at most every `interval` seconds (default 0.1). The
companion captures faster and drops pages whose `(seq, crc)` it already saw.

## Records

`type u8 | len u16 | body[len]`. Unknown types are skipped.

Strings are `len u8 | utf8 bytes`. Positions are `u16` = `round(v * 65535)`
of the normalized map coordinate. Facing is `u16` of `facing / 2pi` (0 = north,
counter-clockwise, same as `GetPlayerFacing`).

World coordinates are a point on Azeroth's map (uiMapID 947), 0..1 across and
down: the space OpenMap stitches its zones in (`OpenMapAPI.WorldFromMap`, else
`C_Map.GetMapRectOnMap(map, 947)`). They are sent as `u32` =
`round((v + 0.5) * 2^24)`, so points a little outside the map fit too.

### 0x01 STATE (every page)

`mapID u32 | x u16 | y u16 | facing u16 | flags u8 | timeMs u32`

flags: 1 hasPos, 2 hasFacing, 4 combat, 8 dead/ghost, 16 mounted, 32 flying,
64 in instance, 128 resting.

### 0x02 GROUP (every page while grouped)

`count u8`, then per member on the player's map:
`unit u8 | classID u8 | x u16 | y u16 | flags u8`

unit: `1..40` = raidN, `101..104` = partyN. flags: 1 dead, 2 offline.

### 0x03 WORLD (every page while the position is on Azeroth's map)

`u u32 | v u32`: the player on the seamless world (world coordinates).

### 0x10 BULK chunk

`channel u8 | gen u16 | index u16 | count u16 | data[]`

Large, slowly changing data is a per-channel blob (itself a list of records)
split into chunks of up to 400 bytes (smaller when the page is small, so a
chunk always fits next to the per-page records). When a blob changes, `gen`
increments. Chunks are sent round-robin and repeated forever (fast for the
first 3 cycles, then slowly), so a companion started later still gets
everything without any back-channel. The companion applies a blob once it holds all `count` chunks
of one `gen`.

Channels: 1 session, 2 map, 3 roster, 4 world, 5 zones, 6 travel, 10+ pins (phase 2).

## Blob records

### 0x20 SESSION (channel 1)

`addonVersion u8 | interface u32 | locale str | player str | realm str | classID u8 | faction u8 | level u8`

faction: 0 other, 1 Alliance, 2 Horde. `faction` and `level` came with addon version 2 (0.3.0); the
companion reads them only when present. The blob is rebuilt on a level up.

### 0x21 MAPINFO (channels 2, 4 and 5)

`mapID u32 | mapType u8 | parentMapID u32 | name str | nLayers u8`, then per layer:
`layerW u16 | layerH u16 | tileW u16 | tileH u16 | nTiles u16 | fileID u32 * nTiles`

Tiles are row-major, `ceil(layerW / tileW)` per row (from
`C_Map.GetMapArtLayerTextures`). The zones channel sends no layers (`nLayers` 0): only the names.

### 0x22 EXPLORED (channel 2)

`mapID u32 | count u16`, then per overlay:
`texW u16 | texH u16 | offX u16 | offY u16 | flags u8 | nFiles u8 | fileID u32 * nFiles`

Same layout rules as Blizzard's `MapExplorationPinMixin`: 256 px tiles,
the last row/column is cropped. flags: 1 shown only on mouse-over (skipped).

### 0x23 ROSTER (channel 3)

`count u8`, then `unit u8 | classID u8 | name str`.

### 0x24 MAPRECT (channels 2, 4 and 5)

`mapID u32 | left u32 | right u32 | top u32 | bottom u32`: where a map sits on
the seamless world (world coordinates). Channel 2 sends it for the current map,
so party positions can be placed on the world too.

### 0x25 WORLDINFO (channel 4)

`flags u8 | worldMapID u32 | openmapVersion str`. flags: 1 OpenMap's API is
present. The world channel holds WORLDINFO, then MAPINFO for the world map,
then MAPINFO, MAPRECT and CONTYARDS for each continent on it.

### 0x26 CONTYARDS (channel 4)

`mapID u32 | continent u16 | x00 i32 | y00 i32 | x10 i32 | y10 i32 | x01 i32 | y01 i32`: where a
continent's map lies in the game's world yards (the coordinates of `UnitPosition` and the taxi
tables): `C_Map.GetWorldPosFromMapPos` of its map points (0, 0), (1, 0) and (0, 1), in hundredths
of a yard (signed). `continent` is the instance it is on (0 the Eastern Kingdoms, 1 Kalimdor). With
MAPRECT this places any point in yards on the world; the companion places OpenMap's flight
masters, docks and dungeon entrances by it.

### Zones (channel 5)

MAPRECT and then MAPINFO (no layers) for every zone of every continent
(`C_Map.GetMapChildrenInfo(continent, Enum.UIMapType.Zone)`) that has a place on the world: the
zones' names in the game's language and their rects.

### 0x27 TAXINODES (channel 6)

`count u16`, then `nodeID u16 | flags u8 | name str`: the flight masters
`C_TaxiMap.GetTaxiNodesForMap` lists for Kalimdor and the Eastern Kingdoms, named in the game's
language. flags: 1 learned. WoW Forever lists every flight master as found, learned or not, so a
node counts as learned when the addon saw it as learned at a flight master (`TAXIMAP_OPENED`,
saved per character in `MapCastCharDB`) or OpenMap did (`OpenMapCharDB.worldmap.flightsKnown`),
or when the game's list marks it found while hiding others.

### 0x28 INSTNAMES (channel 6)

`count u8`, then `instanceID u16 | name str`: the dungeons and raids of OpenMap's data, named by
`GetRealZoneText(instanceID)` in the game's language. Empty when the client answers the same
name for every ID.
