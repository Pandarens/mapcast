-- MapCast: collects map data and streams it through the pixel strip.
local addonName, ns = ...

local ADDON_VERSION = 2
local CHUNK_SIZE = 400 -- largest bulk chunk; smaller when the strip's page is small
local BULK_RESERVE = 100 -- page room kept for the per-page records (state, world position, party)
local FAST_CYCLES = 3 -- full repeats of a changed blob at full speed
local SLOW_EVERY = 5 -- afterwards, one chunk per channel every N pages

local REC_STATE, REC_GROUP, REC_WORLD, REC_BULK = 0x01, 0x02, 0x03, 0x10
local REC_SESSION, REC_MAPINFO, REC_EXPLORED, REC_ROSTER = 0x20, 0x21, 0x22, 0x23
local REC_MAPRECT, REC_WORLDINFO, REC_CONTYARDS, REC_TAXINODES, REC_INSTNAMES = 0x24, 0x25, 0x26, 0x27, 0x28
local CH_SESSION, CH_MAP, CH_ROSTER, CH_WORLD, CH_ZONES, CH_TRAVEL, CH_PINS = 1, 2, 3, 4, 5, 6, 10
local PINS_EVERY = 2 -- seconds between rereads of the map addons' pins (Pins.lua)

-- The seamless world is Azeroth's map (uiMapID 947), the same space OpenMap stitches its zones in.
local WORLD_MAP = 947
local CONTINENT_TYPE = 2 -- Enum.UIMapType.Continent
local ZONE_TYPE = 3 -- Enum.UIMapType.Zone
local FACTION_CODE = { Alliance = 1, Horde = 2 }

-- Kalimdor and the Eastern Kingdoms: the maps the game lists flight masters for.
local TAXI_MAPS = { 1414, 1415 }
-- The kinds of node a flight master lists that you have learned (DISTANT: one a route passes, not learned).
local LEARNED = { CURRENT = true, REACHABLE = true, UNREACHABLE = true }
-- The dungeons and raids in OpenMap's data (Data/Forever/Places.lua), sent with their names in the
-- game's language; the phone falls back to OpenMap's English name for any other.
local INSTANCES = {
	33, 34, 36, 43, 47, 48, 70, 90, 109, 129, 189, 209, 229, 230, 249, 289, 309, 329, 349, 389, 409,
	429, 469, 509, 531, 2959, 2998, 2999, 3065,
}

local floor, min, max, ceil, pi = math.floor, math.min, math.max, math.ceil, math.pi

local defaults = { enabled = true, cellPx = 3, cols = 256, rows = 2, interval = 0.1 }

local db, charDB
local seq, pageCounter, elapsedSinceTick = 0, 0, 0
local currentMapID = 0
local playerU, playerV -- the player on the world, when known
local pinsMap, pinsAt = nil, -math.huge
local mapDirty, rosterDirty, worldDirty, travelDirty = true, true, true, true
local lastError

---------------------------------------------------------------------------
-- Helpers

local function IsSecret(v)
	return v ~= nil and issecretvalue ~= nil and issecretvalue(v)
end

local function Safe(v)
	if IsSecret(v) then
		return nil
	end
	return v
end

local function Norm16(v)
	v = floor(v * 65535 + 0.5)
	if v < 0 then
		return 0
	elseif v > 65535 then
		return 65535
	end
	return v
end

local function Truthy(v)
	return Safe(v) and true or false
end

local function UnitMapXY(mapID, unit)
	local pos = C_Map.GetPlayerMapPosition(mapID, unit)
	if not pos then
		return nil
	end
	local x, y = pos:GetXY()
	x, y = Safe(x), Safe(y)
	if not x or not y or (x == 0 and y == 0) then
		return nil
	end
	return x, y
end

local function ClassID(unit)
	return Safe(select(3, UnitClass(unit))) or 0
end

local function IsNumber(...)
	for i = 1, select("#", ...) do
		local v = select(i, ...)
		if type(v) ~= "number" or IsSecret(v) or v ~= v then
			return false
		end
	end
	return true
end

-- World coordinates go out as u32 = (v + 0.5) * 2^24, so points a little outside the world fit too.
local function WorldFixed(v)
	v = floor((v + 0.5) * 16777216 + 0.5)
	if v < 0 then
		return 0
	elseif v > 4294967295 then
		return 4294967295
	end
	return v
end

local rectCache = {}

local function MapRectOnWorld(mapID)
	local rect = rectCache[mapID]
	if rect == nil then
		local ok, l, r, t, b = pcall(C_Map.GetMapRectOnMap, mapID, WORLD_MAP)
		rect = ok and IsNumber(l, r, t, b) and r > l and b > t and { l, r, t, b } or false
		rectCache[mapID] = rect
	end
	return rect
end

-- A point on a map, on the seamless world (0..1 across Azeroth's map). OpenMap's own answer when it is
-- installed, so the phone puts the arrow exactly where OpenMap does.
local function WorldXY(mapID, x, y)
	if mapID == WORLD_MAP then
		return x, y
	end
	local api = _G.OpenMapAPI
	if api and api.WorldFromMap then
		local ok, u, v = pcall(api.WorldFromMap, mapID, x, y)
		if ok and IsNumber(u, v) then
			return u, v
		end
	end
	local rect = MapRectOnWorld(mapID)
	if rect then
		return rect[1] + x * (rect[2] - rect[1]), rect[3] + y * (rect[4] - rect[3])
	end
end

ns.WorldXY, ns.MapRectOnWorld, ns.WorldFixed = WorldXY, MapRectOnWorld, WorldFixed

local function AppendMapRect(blob, mapID)
	local l, t = WorldXY(mapID, 0, 0)
	local r, b = WorldXY(mapID, 1, 1)
	if not l or not r or r <= l or b <= t then
		return false
	end
	local body = ns.NewBuffer()
	body:u32(mapID)
	body:u32(WorldFixed(l))
	body:u32(WorldFixed(r))
	body:u32(WorldFixed(t))
	body:u32(WorldFixed(b))
	blob:record(REC_MAPRECT, body)
	return true
end

---------------------------------------------------------------------------
-- Bulk channels: blobs repeated round-robin so a late companion catches up.

local channels, channelOrder = {}, {}
local roundRobin = 1

-- A chunk has to fit in one page next to the per-page records.
local function ChunkSize()
	return max(32, min(CHUNK_SIZE, ns.Strip:PageCapacity() - ns.PAGE_OVERHEAD - BULK_RESERVE))
end

-- (Re)cuts a channel's blob into chunks under a new generation.
local function Split(ch)
	ch.gen = (ch.gen + 1) % 65536
	ch.chunkSize = ChunkSize()
	ch.count = max(1, ceil(ch.blob.n / ch.chunkSize))
	ch.nextIndex, ch.cycles = 0, 0
end

local function SetChannel(id, blob)
	local ch = channels[id]
	local signature = blob:signature()
	if ch and ch.signature == signature then
		return
	end
	if not ch then
		-- Random start so a /reload does not repeat generation numbers the companion already holds.
		ch = { id = id, gen = math.random(0, 65535) }
		channels[id] = ch
		channelOrder[#channelOrder + 1] = id
		table.sort(channelOrder)
	end
	ch.blob, ch.signature = blob, signature
	Split(ch)
end

local function AppendBulk(payload, capacity)
	local n = #channelOrder
	if n == 0 then
		return
	end
	local slowTurn = pageCounter % SLOW_EVERY == 0
	local sent = {}
	local progressed = true
	while progressed do
		progressed = false
		for _ = 1, n do
			if roundRobin > n then
				roundRobin = 1
			end
			local ch = channels[channelOrder[roundRobin]]
			roundRobin = roundRobin + 1
			if ch.cycles < FAST_CYCLES or (slowTurn and not sent[ch.id]) then
				local first = ch.nextIndex * ch.chunkSize + 1
				local last = min(first + ch.chunkSize - 1, ch.blob.n)
				local size = ns.RECORD_OVERHEAD + 7 + max(0, last - first + 1)
				if payload.n + size <= capacity then
					local body = ns.NewBuffer()
					body:u8(ch.id)
					body:u16(ch.gen)
					body:u16(ch.nextIndex)
					body:u16(ch.count)
					if last >= first then
						body:append(ch.blob, first, last)
					end
					payload:record(REC_BULK, body)
					ch.nextIndex = ch.nextIndex + 1
					if ch.nextIndex >= ch.count then
						ch.nextIndex = 0
						ch.cycles = ch.cycles + 1
					end
					sent[ch.id] = true
					progressed = true
				end
			end
		end
	end
end

---------------------------------------------------------------------------
-- Collectors

local function BuildSession(level)
	local body = ns.NewBuffer()
	body:u8(ADDON_VERSION)
	body:u32(select(4, GetBuildInfo()) or 0)
	body:str(GetLocale())
	body:str(Safe(UnitName("player")) or "")
	body:str(GetRealmName() or "")
	body:u8(ClassID("player"))
	body:u8(FACTION_CODE[Safe(UnitFactionGroup("player"))] or 0)
	level = IsNumber(level) and level or Safe(UnitLevel("player"))
	body:u8(IsNumber(level) and max(0, min(255, level)) or 0)
	local blob = ns.NewBuffer()
	blob:record(REC_SESSION, body)
	return blob
end

local function AppendMapInfo(blob, mapID, nameOnly)
	local info = mapID > 0 and C_Map.GetMapInfo(mapID)
	if not info then
		return false
	end
	local body = ns.NewBuffer()
	body:u32(mapID)
	body:u8(info.mapType or 0)
	body:u32(info.parentMapID or 0)
	body:str(info.name or "")
	local layers = not nameOnly and C_Map.GetMapArtLayers(mapID) or {}
	body:u8(#layers)
	for index, layer in ipairs(layers) do
		local tiles = C_Map.GetMapArtLayerTextures(mapID, index) or {}
		body:u16(layer.layerWidth)
		body:u16(layer.layerHeight)
		body:u16(layer.tileWidth)
		body:u16(layer.tileHeight)
		body:u16(#tiles)
		for _, fileID in ipairs(tiles) do
			body:u32(fileID)
		end
	end
	blob:record(REC_MAPINFO, body)
	return true
end

-- Where a continent's map lies in the game's world yards (UnitPosition's and the taxi tables'), from
-- three of its corners: the companion places OpenMap's flight masters, docks and dungeon entrances by it.
local CORNERS = { { 0, 0 }, { 1, 0 }, { 0, 1 } }

local function AppendYards(blob, mapID)
	if not (C_Map.GetWorldPosFromMapPos and CreateVector2D) then
		return false
	end
	local continent, coords = nil, {}
	for _, corner in ipairs(CORNERS) do
		local ok, cont, pos = pcall(C_Map.GetWorldPosFromMapPos, mapID, CreateVector2D(corner[1], corner[2]))
		if not ok or type(pos) ~= "table" or not pos.GetXY then
			return false
		end
		local okXY, x, y = pcall(pos.GetXY, pos)
		if not okXY or not IsNumber(cont, x, y) or (continent and cont ~= continent) then
			return false
		end
		continent = cont
		coords[#coords + 1] = x
		coords[#coords + 1] = y
	end
	local body = ns.NewBuffer()
	body:u32(mapID)
	body:u16(continent)
	for _, v in ipairs(coords) do
		body:u32(floor(v * 100 + 0.5)) -- signed: a negative number wraps to its two's complement
	end
	blob:record(REC_CONTYARDS, body)
	return true
end

-- What the companion needs to draw the seamless world: Azeroth's art, each continent's art and
-- where it sits, and whether OpenMap (whose stitched zone art the companion reads) is running.
local function BuildWorld()
	local blob = ns.NewBuffer()
	local api = _G.OpenMapAPI
	local info = ns.NewBuffer()
	info:u8(api and api.WorldFromMap and 1 or 0)
	info:u32(WORLD_MAP)
	local version = ""
	local getMeta = (C_AddOns and C_AddOns.GetAddOnMetadata) or _G.GetAddOnMetadata
	if api and getMeta then
		local okMeta, v = pcall(getMeta, "OpenMap", "Version")
		if okMeta and type(v) == "string" then
			version = v
		end
	end
	info:str(version)
	blob:record(REC_WORLDINFO, info)
	if not AppendMapInfo(blob, WORLD_MAP) then
		return blob
	end
	local ok, children = pcall(C_Map.GetMapChildrenInfo, WORLD_MAP, CONTINENT_TYPE)
	for _, child in ipairs(ok and children or {}) do
		local mapID = Safe(child.mapID)
		if mapID and MapRectOnWorld(mapID) then
			AppendMapInfo(blob, mapID)
			AppendMapRect(blob, mapID)
			AppendYards(blob, mapID)
		end
	end
	return blob
end

-- Every zone of every continent: its name in the game's language and where it sits on the world.
local function BuildZones()
	local blob = ns.NewBuffer()
	local ok, continents = pcall(C_Map.GetMapChildrenInfo, WORLD_MAP, CONTINENT_TYPE)
	for _, continent in ipairs(ok and continents or {}) do
		local contID = Safe(continent.mapID)
		local okZones, zones = false, nil
		if contID then
			okZones, zones = pcall(C_Map.GetMapChildrenInfo, contID, ZONE_TYPE)
		end
		for _, zone in ipairs(okZones and zones or {}) do
			local mapID = Safe(zone.mapID)
			if mapID and AppendMapRect(blob, mapID) then
				AppendMapInfo(blob, mapID, true)
			end
		end
	end
	return blob
end

---------------------------------------------------------------------------
-- Travel: flight masters (which are learned) and dungeon names in the game's language

-- The flight masters the game lists: nodeID -> { name, undiscovered }, in the list's order.
local function TaxiNodes()
	local nodes, order = {}, {}
	if not (C_TaxiMap and C_TaxiMap.GetTaxiNodesForMap) then
		return nodes, order
	end
	for _, mapID in ipairs(TAXI_MAPS) do
		local ok, list = pcall(C_TaxiMap.GetTaxiNodesForMap, mapID)
		for _, info in ipairs(ok and type(list) == "table" and list or {}) do
			local id = type(info) == "table" and Safe(info.nodeID)
			if IsNumber(id) and not nodes[id] then
				local name = Safe(info.name)
				nodes[id] = { name = type(name) == "string" and name or "", undiscovered = Safe(info.isUndiscovered) }
				order[#order + 1] = id
			end
		end
	end
	return nodes, order
end

-- Names of the flight masters seen as learned at a flight master: ours, and OpenMap's (per character).
local function KnownFlightNames()
	local known = {}
	for name in pairs(charDB and charDB.flights or {}) do
		known[name] = true
	end
	local om = type(_G.OpenMapCharDB) == "table" and _G.OpenMapCharDB.worldmap
	if type(om) == "table" and om.flightsKnownV == 2 and type(om.flightsKnown) == "table" then
		for name, on in pairs(om.flightsKnown) do
			if on and type(name) == "string" then
				known[name] = true
			end
		end
	end
	return known
end

-- At a flight master: remember the learned ones by name (the game's own list can't tell on WoW Forever).
local function LearnFlights()
	if not (charDB and NumTaxiNodes and TaxiNodeName and TaxiNodeGetType) then
		return
	end
	charDB.flights = charDB.flights or {}
	for i = 1, Safe(NumTaxiNodes()) or 0 do
		local name, kind = Safe(TaxiNodeName(i)), Safe(TaxiNodeGetType(i))
		if type(name) == "string" and name ~= "" and LEARNED[kind] then
			charDB.flights[name] = true
		end
	end
end

local function InstanceNames()
	local names, count, distinct, seen = ns.NewBuffer(), 0, 0, {}
	for _, id in ipairs(INSTANCES) do
		local ok, name = pcall(GetRealZoneText, id)
		name = ok and Safe(name)
		if type(name) == "string" and name ~= "" then
			names:u16(id)
			names:str(name)
			count = count + 1
			if not seen[name] then
				seen[name], distinct = true, distinct + 1
			end
		end
	end
	local body = ns.NewBuffer()
	-- (a client whose GetRealZoneText ignores the ID answers the current zone for all of them)
	if count > 1 and distinct == 1 then
		count = 0
	end
	body:u8(count)
	if count > 0 then
		body:append(names)
	end
	return body
end

local function BuildTravel()
	local nodes, order = TaxiNodes()
	local known = KnownFlightNames()
	-- WoW Forever lists every flight master as found, learned or not: trust the list only when it hides some
	local listTells = false
	for _, id in ipairs(order) do
		if nodes[id].undiscovered == true then
			listTells = true
			break
		end
	end
	local body = ns.NewBuffer()
	body:u16(#order)
	for _, id in ipairs(order) do
		local node = nodes[id]
		local learned = known[node.name] or (listTells and node.undiscovered == false)
		body:u16(id)
		body:u8(learned and 1 or 0)
		body:str(node.name)
	end
	local blob = ns.NewBuffer()
	blob:record(REC_TAXINODES, body)
	blob:record(REC_INSTNAMES, InstanceNames())
	return blob
end

local function BuildMap(mapID)
	local blob = ns.NewBuffer()
	if not AppendMapInfo(blob, mapID) then
		return blob
	end
	AppendMapRect(blob, mapID)

	local explored = C_MapExplorationInfo and C_MapExplorationInfo.GetExploredMapTextures(mapID)
	if explored then
		local shown = {}
		for _, overlay in ipairs(explored) do
			if not overlay.isShownByMouseOver then
				shown[#shown + 1] = overlay
			end
		end
		local ex = ns.NewBuffer()
		ex:u32(mapID)
		ex:u16(#shown)
		for _, overlay in ipairs(shown) do
			local files = overlay.fileDataIDs or {}
			ex:u16(overlay.textureWidth)
			ex:u16(overlay.textureHeight)
			ex:u16(overlay.offsetX)
			ex:u16(overlay.offsetY)
			ex:u8(0)
			ex:u8(#files)
			for _, fileID in ipairs(files) do
				ex:u32(fileID)
			end
		end
		blob:record(REC_EXPLORED, ex)
	end
	return blob
end

local function GroupUnits()
	local units = {}
	if IsInRaid() then
		for i = 1, GetNumGroupMembers() do
			local unit = "raid" .. i
			if not UnitIsUnit(unit, "player") then
				units[#units + 1] = { unit = unit, code = i }
			end
		end
	elseif IsInGroup() then
		for i = 1, GetNumSubgroupMembers() do
			units[#units + 1] = { unit = "party" .. i, code = 100 + i }
		end
	end
	return units
end

local function BuildRoster()
	local units = GroupUnits()
	local body = ns.NewBuffer()
	body:u8(min(#units, 40))
	for i = 1, min(#units, 40) do
		local u = units[i]
		body:u8(u.code)
		body:u8(ClassID(u.unit))
		body:str(Safe(GetUnitName(u.unit, true)) or "")
	end
	local blob = ns.NewBuffer()
	blob:record(REC_ROSTER, body)
	return blob
end

local function AppendState(payload)
	local mapID = Safe(C_Map.GetBestMapForUnit("player")) or 0
	if mapID ~= currentMapID then
		currentMapID = mapID
		mapDirty = true
	end

	local x, y, facing, flags = 0, 0, 0, 0
	local u, v
	playerU, playerV = nil, nil
	if mapID > 0 then
		local px, py = UnitMapXY(mapID, "player")
		if px then
			x, y = Norm16(px), Norm16(py)
			flags = flags + 1
			u, v = WorldXY(mapID, px, py)
		end
	end
	local f = Safe(GetPlayerFacing())
	if f then
		facing = floor((f % (2 * pi)) / (2 * pi) * 65535 + 0.5) % 65536
		flags = flags + 2
	end
	if Truthy(UnitAffectingCombat("player")) then flags = flags + 4 end
	if Truthy(UnitIsDeadOrGhost("player")) then flags = flags + 8 end
	if Truthy(IsMounted()) then flags = flags + 16 end
	if Truthy(IsFlying()) then flags = flags + 32 end
	if Truthy(IsInInstance()) then flags = flags + 64 end
	if Truthy(IsResting()) then flags = flags + 128 end

	local body = ns.NewBuffer()
	body:u32(mapID)
	body:u16(x)
	body:u16(y)
	body:u16(facing)
	body:u8(flags)
	body:u32(floor(GetTime() * 1000) % 4294967296)
	payload:record(REC_STATE, body)

	if u then
		playerU, playerV = u, v
		local world = ns.NewBuffer()
		world:u32(WorldFixed(u))
		world:u32(WorldFixed(v))
		payload:record(REC_WORLD, world)
	end
end

local function AppendGroup(payload)
	if currentMapID == 0 or not IsInGroup() then
		return
	end
	local body = ns.NewBuffer()
	body:u8(0)
	local count = 0
	for _, u in ipairs(GroupUnits()) do
		local px, py = UnitMapXY(currentMapID, u.unit)
		if px then
			local memberFlags = 0
			if Truthy(UnitIsDeadOrGhost(u.unit)) then memberFlags = memberFlags + 1 end
			if not Truthy(UnitIsConnected(u.unit)) then memberFlags = memberFlags + 2 end
			body:u8(u.code)
			body:u8(ClassID(u.unit))
			body:u16(Norm16(px))
			body:u16(Norm16(py))
			body:u8(memberFlags)
			count = count + 1
			if count >= 40 then
				break
			end
		end
	end
	if count > 0 then
		body[1] = count
		payload:record(REC_GROUP, body)
	end
end

---------------------------------------------------------------------------
-- Main loop

local function Guard(fn, ...)
	local ok, err = pcall(fn, ...)
	if not ok then
		lastError = err
	end
	return ok
end

local function Tick()
	local capacity = ns.Strip:PageCapacity() - ns.PAGE_OVERHEAD
	local payload = ns.NewBuffer()
	Guard(AppendState, payload)
	if mapDirty then
		mapDirty = false
		Guard(function() SetChannel(CH_MAP, BuildMap(currentMapID)) end)
	end
	if rosterDirty then
		rosterDirty = false
		Guard(function() SetChannel(CH_ROSTER, BuildRoster()) end)
	end
	if worldDirty then
		worldDirty = false
		Guard(function() SetChannel(CH_WORLD, BuildWorld()) end)
		Guard(function() SetChannel(CH_ZONES, BuildZones()) end)
	end
	if travelDirty then
		travelDirty = false
		Guard(function() SetChannel(CH_TRAVEL, BuildTravel()) end)
	end
	local now = GetTime()
	if ns.BuildPins and (currentMapID ~= pinsMap or now - pinsAt >= PINS_EVERY) then
		pinsMap, pinsAt = currentMapID, now
		Guard(function() SetChannel(CH_PINS, ns.BuildPins(currentMapID, playerU, playerV)) end)
	end
	Guard(AppendGroup, payload)
	AppendBulk(payload, capacity)

	seq = (seq + 1) % 256
	pageCounter = pageCounter + 1
	ns.Strip:Paint(ns.BuildPage(seq, payload))
end

local function ApplySettings()
	ns.Strip:Init(db.cellPx, db.cols, db.rows)
	ns.Strip:SetShown(db.enabled)
	for _, ch in pairs(channels) do
		Split(ch) -- the page size may have changed
	end
end

local driver = CreateFrame("Frame")

driver:SetScript("OnUpdate", function(_, elapsed)
	if not db or not db.enabled then
		return
	end
	elapsedSinceTick = elapsedSinceTick + elapsed
	if elapsedSinceTick >= db.interval then
		elapsedSinceTick = 0
		Tick()
	end
end)

driver:SetScript("OnEvent", function(_, event, arg1)
	if event == "ADDON_LOADED" then
		if arg1 ~= addonName then
			return
		end
		MapCastDB = MapCastDB or {}
		db = MapCastDB
		MapCastCharDB = MapCastCharDB or {}
		charDB = MapCastCharDB
		for k, v in pairs(defaults) do
			if db[k] == nil then
				db[k] = v
			end
		end
		ApplySettings()
	elseif event == "PLAYER_ENTERING_WORLD" then
		Guard(function() SetChannel(CH_SESSION, BuildSession()) end)
		mapDirty, rosterDirty, worldDirty, travelDirty = true, true, true, true
		if ns.Strip.frame then
			ns.Strip:Layout()
		end
	elseif event == "DISPLAY_SIZE_CHANGED" or event == "UI_SCALE_CHANGED" then
		if ns.Strip.frame then
			ns.Strip:Layout()
		end
	elseif event == "GROUP_ROSTER_UPDATE" then
		rosterDirty = true
	elseif event == "TAXIMAP_OPENED" then
		Guard(LearnFlights)
		travelDirty = true
	elseif event == "PLAYER_LEVEL_UP" then
		Guard(function() SetChannel(CH_SESSION, BuildSession(arg1)) end)
	else
		mapDirty = true
	end
end)

for _, event in ipairs({
	"ADDON_LOADED", "PLAYER_ENTERING_WORLD", "DISPLAY_SIZE_CHANGED", "UI_SCALE_CHANGED",
	"GROUP_ROSTER_UPDATE", "ZONE_CHANGED", "ZONE_CHANGED_INDOORS", "ZONE_CHANGED_NEW_AREA",
	"MAP_EXPLORATION_UPDATED", "TAXIMAP_OPENED", "PLAYER_LEVEL_UP",
}) do
	driver:RegisterEvent(event)
end

---------------------------------------------------------------------------
-- Slash commands

local function Print(msg)
	print("|cff33ccffMapCast|r: " .. msg)
end

local function Diagnose()
	local mapID = C_Map.GetBestMapForUnit("player")
	Print(("map %s, secret=%s"):format(tostring(mapID), tostring(IsSecret(mapID))))
	if mapID and not IsSecret(mapID) then
		local pos = C_Map.GetPlayerMapPosition(mapID, "player")
		if pos then
			local x, y = pos:GetXY()
			Print(("position %s, %s secret=%s"):format(tostring(x), tostring(y), tostring(IsSecret(x))))
			if IsNumber(x, y) then
				local u, v = WorldXY(mapID, x, y)
				Print(("on the world map %s, %s via %s"):format(tostring(u), tostring(v),
					_G.OpenMapAPI and "OpenMap" or "Blizzard"))
			end
		else
			Print("position unavailable here (instances hide it)")
		end
		local layers = C_Map.GetMapArtLayers(mapID)
		Print(("map art layers: %d"):format(layers and #layers or 0))
	end
	local nodes, order = TaxiNodes()
	local known, learned = KnownFlightNames(), 0
	for _, id in ipairs(order) do
		if known[nodes[id].name] then
			learned = learned + 1
		end
	end
	Print(("flight masters listed %d, learned %d"):format(#order, learned))
	local counts = {}
	for name, n in pairs(ns.pinStats or {}) do
		counts[#counts + 1] = ("%s %d"):format(name, n)
	end
	table.sort(counts)
	Print("pins sent: " .. (#counts > 0 and table.concat(counts, ", ") or "none"))
	local f = GetPlayerFacing()
	Print(("facing %s secret=%s"):format(tostring(f), tostring(IsSecret(f))))
	local w, h = GetPhysicalScreenSize()
	Print(("screen %dx%d, strip %dx%d px, page %d bytes"):format(w, h,
		db.cols * db.cellPx, db.rows * db.cellPx, ns.Strip:PageCapacity()))
	Print("last error: " .. tostring(lastError))
end

local HELP = {
	"/mapcast on | off - show or hide the strip",
	"/mapcast cell <1-8> - square size in pixels (default 3)",
	"/mapcast cols <16-1024>, rows <1-16> - strip size (default 256 x 2)",
	"/mapcast rate <0.03-1> - seconds between pages (default 0.1)",
	"/mapcast diag - check what the game lets the addon read",
}

SLASH_MAPCAST1 = "/mapcast"
SlashCmdList.MAPCAST = function(msg)
	local cmd, arg = (msg or ""):lower():match("^%s*(%S*)%s*(.-)%s*$")
	local num = tonumber(arg)
	if cmd == "on" or cmd == "off" then
		db.enabled = cmd == "on"
		ns.Strip:SetShown(db.enabled)
	elseif cmd == "cell" and num and num >= 1 and num <= 8 then
		db.cellPx = floor(num)
		ApplySettings()
	elseif cmd == "cols" and num and num >= 16 and num <= 1024 then
		db.cols = floor(num)
		ApplySettings()
	elseif cmd == "rows" and num and num >= 1 and num <= 16 then
		db.rows = floor(num)
		ApplySettings()
	elseif cmd == "rate" and num and num >= 0.03 and num <= 1 then
		db.interval = num
	elseif cmd == "diag" then
		Diagnose()
		return
	elseif cmd ~= "" then
		for _, line in ipairs(HELP) do
			Print(line)
		end
		return
	end
	Print(("%s, %d x %d cells of %d px, every %.2fs"):format(db.enabled and "on" or "off",
		db.cols, db.rows, db.cellPx, db.interval))
end

ns.Tick = Tick -- used by the test harness
