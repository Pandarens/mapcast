-- MapCast: collects map data and streams it through the pixel strip.
local addonName, ns = ...

local ADDON_VERSION = 1
local CHUNK_SIZE = 400 -- largest bulk chunk; smaller when the strip's page is small
local BULK_RESERVE = 100 -- page room kept for the per-page records (state, world position, party)
local FAST_CYCLES = 3 -- full repeats of a changed blob at full speed
local SLOW_EVERY = 5 -- afterwards, one chunk per channel every N pages

local REC_STATE, REC_GROUP, REC_WORLD, REC_BULK = 0x01, 0x02, 0x03, 0x10
local REC_SESSION, REC_MAPINFO, REC_EXPLORED, REC_ROSTER = 0x20, 0x21, 0x22, 0x23
local REC_MAPRECT, REC_WORLDINFO = 0x24, 0x25
local CH_SESSION, CH_MAP, CH_ROSTER, CH_WORLD = 1, 2, 3, 4

-- The seamless world is Azeroth's map (uiMapID 947), the same space OpenMap stitches its zones in.
local WORLD_MAP = 947
local CONTINENT_TYPE = 2 -- Enum.UIMapType.Continent

local floor, min, max, ceil, pi = math.floor, math.min, math.max, math.ceil, math.pi

local defaults = { enabled = true, cellPx = 3, cols = 256, rows = 2, interval = 0.1 }

local db
local seq, pageCounter, elapsedSinceTick = 0, 0, 0
local currentMapID = 0
local mapDirty, rosterDirty, worldDirty = true, true, true
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

local function BuildSession()
	local body = ns.NewBuffer()
	body:u8(ADDON_VERSION)
	body:u32(select(4, GetBuildInfo()) or 0)
	body:str(GetLocale())
	body:str(Safe(UnitName("player")) or "")
	body:str(GetRealmName() or "")
	body:u8(ClassID("player"))
	local blob = ns.NewBuffer()
	blob:record(REC_SESSION, body)
	return blob
end

local function AppendMapInfo(blob, mapID)
	local info = mapID > 0 and C_Map.GetMapInfo(mapID)
	if not info then
		return false
	end
	local body = ns.NewBuffer()
	body:u32(mapID)
	body:u8(info.mapType or 0)
	body:u32(info.parentMapID or 0)
	body:str(info.name or "")
	local layers = C_Map.GetMapArtLayers(mapID) or {}
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
		end
	end
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
		for k, v in pairs(defaults) do
			if db[k] == nil then
				db[k] = v
			end
		end
		ApplySettings()
	elseif event == "PLAYER_ENTERING_WORLD" then
		Guard(function() SetChannel(CH_SESSION, BuildSession()) end)
		mapDirty, rosterDirty, worldDirty = true, true, true
		if ns.Strip.frame then
			ns.Strip:Layout()
		end
	elseif event == "DISPLAY_SIZE_CHANGED" or event == "UI_SCALE_CHANGED" then
		if ns.Strip.frame then
			ns.Strip:Layout()
		end
	elseif event == "GROUP_ROSTER_UPDATE" then
		rosterDirty = true
	else
		mapDirty = true
	end
end)

for _, event in ipairs({
	"ADDON_LOADED", "PLAYER_ENTERING_WORLD", "DISPLAY_SIZE_CHANGED", "UI_SCALE_CHANGED",
	"GROUP_ROSTER_UPDATE", "ZONE_CHANGED", "ZONE_CHANGED_INDOORS", "ZONE_CHANGED_NEW_AREA",
	"MAP_EXPLORATION_UPDATED",
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
