-- MapCast pins: what map addons put on the world map (HandyNotes, TomTom, GatherMate2, Questie,
-- RestedXP Guide and anything else drawing through HereBeDragons), around where you are.
-- They are read from the addons' own data, so the phone shows them with the game's map closed.
-- The quests in your log are pins too ("Quests"): where the game's own map marks them.
local _, ns = ...

local REC_PINSOURCES, REC_PINICONS, REC_PINTEXTS, REC_PINS = 0x30, 0x31, 0x32, 0x33
local MAX_PINS = 400 -- the most sent at once: the nearest of each kind first
local NEAR = 0.5 -- how far past the current map pins are sent, in the map's width and height
local MAX_NODES = 3000 -- a plugin's nodes read per map at most
local TITLE_BUDGET = 15 -- HandyNotes tooltips read per build (each one runs the plugin's OnEnter)
local NONE = 65535
local ZONE_TYPE, CONTINENT_TYPE = 3, 2

local floor, min, max = math.floor, math.min, math.max

-- HereBeDragons-Pins and the copy Questie embeds under its own name, each with its core library.
local HBD_LIBS = {
	{ "HereBeDragons-Pins-2.0", "HereBeDragons-2.0" },
	{ "HereBeDragonsQuestie-Pins-2.0", "HereBeDragonsQuestie-2.0" },
}
local TOMTOM_ICON = "Interface\\AddOns\\TomTom\\Images\\GoldGreenDot"
-- the order pins are kept in when there are too many
local PRIORITY = { TomTom = 1, RestedXP = 1, Quests = 2, HandyNotes = 2, Questie = 3, GatherMate2 = 4 }
local QUEST_SAME = 0.002 -- one quest's marks this close on the world are one (a city's and its zone's)

local function IsSecret(v)
	return v ~= nil and issecretvalue ~= nil and issecretvalue(v)
end

local function Safe(v)
	if IsSecret(v) then
		return nil
	end
	return v
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

local function Lib(name)
	return _G.LibStub and _G.LibStub(name, true)
end

local function Addon(name)
	local ace = Lib("AceAddon-3.0")
	local ok, addon = false, nil
	if ace and ace.GetAddon then
		ok, addon = pcall(ace.GetAddon, ace, name, true)
	end
	return ok and type(addon) == "table" and addon or nil
end

-- Plain text: no colour codes, textures or line breaks, at most 120 bytes.
local function Clean(s)
	s = Safe(s)
	if type(s) ~= "string" then
		return nil
	end
	s = s:gsub("|c%x%x%x%x%x%x%x%x", ""):gsub("|r", ""):gsub("|T.-|t", ""):gsub("|A.-|a", "")
	s = s:gsub("[\r\n]+", " "):gsub("^%s+", ""):gsub("%s+$", "")
	if s == "" then
		return nil
	end
	if #s > 120 then
		local cut = 120
		while cut > 0 and s:byte(cut + 1) and s:byte(cut + 1) >= 128 and s:byte(cut + 1) < 192 do
			cut = cut - 1
		end
		s = s:sub(1, cut)
	end
	return s
end

---------------------------------------------------------------------------
-- Textures: a file ID where the game knows the file, else its path (an addon's own file)

local probe

local function FileOf(tex)
	local id = tex.GetTextureFileID and Safe(tex:GetTextureFileID())
	if IsNumber(id) and id > 0 then
		return id, nil
	end
	local path = tex.GetTextureFilePath and Safe(tex:GetTextureFilePath())
	if IsNumber(path) and path > 0 then
		return path, nil
	elseif type(path) == "string" and path ~= "" then
		return nil, path
	end
	local any = Safe(tex:GetTexture())
	if IsNumber(any) and any > 0 then
		return any, nil
	elseif type(any) == "string" and any ~= "" then
		return nil, any
	end
end

-- An icon as a texture would draw it: file, texture coordinates (l, r, t, b) and colour.
local function IconFromTexture(tex)
	local fileID, path = FileOf(tex)
	if not fileID and not path then
		return nil
	end
	local icon = { fileID = fileID, path = path, l = 0, r = 1, t = 0, b = 1, cr = 1, cg = 1, cb = 1, ca = 1 }
	if tex.GetTexCoord then
		local ulx, uly, _, _, _, _, lrx, lry = tex:GetTexCoord()
		if IsNumber(ulx, uly, lrx, lry) then
			icon.l, icon.t, icon.r, icon.b = ulx, uly, lrx, lry
		end
	end
	if tex.GetVertexColor then
		local r, g, b, a = tex:GetVertexColor()
		if IsNumber(r, g, b) then
			icon.cr, icon.cg, icon.cb, icon.ca = r, g, b, IsNumber(a) and a or 1
		end
	end
	return icon
end

-- An icon given as HandyNotes does: a path, a file ID, or { icon, tCoordLeft.., r, g, b, a }.
local function IconFromSpec(spec)
	if spec == nil then
		return nil
	end
	probe = probe or CreateFrame("Frame"):CreateTexture()
	local file = type(spec) == "table" and spec.icon or spec
	if type(file) ~= "string" and type(file) ~= "number" then
		return nil
	end
	probe:SetTexture(file)
	local fileID, path = FileOf(probe)
	if not fileID and not path then
		if type(file) == "number" then
			fileID = file
		else
			path = file
		end
	end
	local icon = { fileID = fileID, path = path, l = 0, r = 1, t = 0, b = 1, cr = 1, cg = 1, cb = 1, ca = 1 }
	if type(spec) == "table" then
		if IsNumber(spec.tCoordLeft, spec.tCoordRight, spec.tCoordTop, spec.tCoordBottom) then
			icon.l, icon.r, icon.t, icon.b = spec.tCoordLeft, spec.tCoordRight, spec.tCoordTop, spec.tCoordBottom
		end
		if IsNumber(spec.r, spec.g, spec.b) then
			icon.cr, icon.cg, icon.cb, icon.ca = spec.r, spec.g, spec.b, IsNumber(spec.a) and spec.a or 1
		end
	end
	return icon
end

---------------------------------------------------------------------------
-- A collection: the pins near a box on the world, with shared icon, text and source tables

local Collection = {}
Collection.__index = Collection

local function NewCollection(box)
	return setmetatable({ box = box, pins = {}, sources = {}, sourceIndex = {}, icons = {}, iconIndex = {},
		texts = {}, textIndex = {} }, Collection)
end

function Collection:Near(u, v)
	local b = self.box
	return b and u >= b[1] and u <= b[2] and v >= b[3] and v <= b[4]
end

function Collection:Source(name)
	local i = self.sourceIndex[name]
	if not i and #self.sources < 255 then
		self.sources[#self.sources + 1] = name
		i = #self.sources - 1
		self.sourceIndex[name] = i
	end
	return i
end

function Collection:Icon(icon)
	if not icon then
		return NONE
	end
	local key = table.concat({ tostring(icon.fileID or icon.path), floor(icon.l * 1000), floor(icon.r * 1000),
		floor(icon.t * 1000), floor(icon.b * 1000), floor(icon.cr * 100), floor(icon.cg * 100), floor(icon.cb * 100),
		floor(icon.ca * 100) }, ":")
	local i = self.iconIndex[key]
	if not i and #self.icons < NONE then
		self.icons[#self.icons + 1] = icon
		i = #self.icons - 1
		self.iconIndex[key] = i
	end
	return i or NONE
end

function Collection:Text(s)
	s = Clean(s)
	if not s then
		return NONE
	end
	local i = self.textIndex[s]
	if not i and #self.texts < NONE then
		self.texts[#self.texts + 1] = s
		i = #self.texts - 1
		self.textIndex[s] = i
	end
	return i or NONE
end

-- pin: { source = name, u, v, icon (table), size, title, sub, label, active, anywhere }
function Collection:Add(pin)
	if not IsNumber(pin.u, pin.v) or not (pin.anywhere or self:Near(pin.u, pin.v)) then
		return
	end
	pin.size = max(4, min(64, floor((IsNumber(pin.size) and pin.size or 12) + 0.5)))
	pin.priority = PRIORITY[pin.source:match("^[^:]+")] or 3
	self.pins[#self.pins + 1] = pin
end

---------------------------------------------------------------------------
-- Sources

local function CollectTomTom(c)
	local tomtom = _G.TomTom
	if type(tomtom) ~= "table" or type(tomtom.waypoints) ~= "table" then
		return false
	end
	local icon = IconFromSpec(TOMTOM_ICON)
	for mapID, points in pairs(tomtom.waypoints) do
		if type(points) == "table" then
			for _, wp in pairs(points) do
				if type(wp) == "table" then
					local m, x, y = wp[1] or wp.m or mapID, wp[2] or wp.x, wp[3] or wp.y
					if IsNumber(m, x, y) then
						local u, v = ns.WorldXY(m, x, y)
						c:Add({ source = "TomTom", u = u, v = v, icon = icon, size = 16, title = wp.title, anywhere = true })
					end
				end
			end
		end
	end
	return true
end

-- Who registered a HereBeDragons pin, by its `ref`. TomTom's own are read from its list instead.
local function RefName(ref, tomtomRead)
	if ref ~= nil and ref == _G.TomTom then
		return "TomTom", tomtomRead
	end
	if type(ref) == "table" then
		if ref == _G.Questie or ref.name == "Questie" then
			return "Questie"
		end
		if _G.RXPGuides and ref.RXPGuides == _G.RXPGuides then
			return "RestedXP"
		end
		if type(ref.name) == "string" then
			return ref.name
		end
	elseif type(ref) == "string" then
		return ref
	end
	return "?"
end

local function FirstRegion(frame, kind)
	if not frame.GetRegions then
		return nil
	end
	for _, region in ipairs({ frame:GetRegions() }) do
		if region.GetObjectType and region:GetObjectType() == kind then
			if kind ~= "FontString" or Clean(region:GetText()) then
				return region
			end
		end
	end
end

local function HBDPin(c, source, frame, u, v)
	local pin = { source = source, u = u, v = v, size = frame.GetWidth and Safe(frame:GetWidth()) or 12 }
	if source == "Questie" then
		local data = type(frame.data) == "table" and frame.data or {}
		local quest = type(data.QuestData) == "table" and data.QuestData or nil
		pin.title = data.Name or (quest and quest.name)
		pin.sub = quest and quest.name ~= pin.title and quest.name or nil
		pin.icon = frame.texture and IconFromTexture(frame.texture)
	elseif source == "RestedXP" then
		local text = frame.text and Safe(frame.text:GetText())
		local path = type(text) == "string" and text:match("|T([^:|]+)")
		local object = type(frame.activeObject) == "table" and frame.activeObject or {}
		local element = type(object.elements) == "table" and object.elements[1] or {}
		local step = type(element.step) == "table" and element.step or {}
		if path then
			pin.icon = IconFromSpec(path)
		else
			pin.label = text -- the step's number: drawn as a numbered circle
		end
		pin.title = element.text or element.tooltipText or step.text
		pin.active = step.active and true or false
		pin.anywhere = pin.active
	else
		local tex = frame.texture or frame.icon or FirstRegion(frame, "Texture")
		pin.icon = tex and IconFromTexture(tex)
		local fs = FirstRegion(frame, "FontString")
		pin.label = fs and fs:GetText()
	end
	if pin.icon or pin.label then
		c:Add(pin)
	end
end

local function CollectHereBeDragons(c, currentMapID, tomtomRead)
	for _, names in ipairs(HBD_LIBS) do
		local pins, core = Lib(names[1]), Lib(names[2])
		if pins and core and core.GetZoneCoordinatesFromWorld and type(pins.worldmapPins) == "table" then
			local owner = {}
			for ref, icons in pairs(pins.worldmapPinRegistry or {}) do
				for icon in pairs(icons) do
					owner[icon] = ref
				end
			end
			for frame, data in pairs(pins.worldmapPins) do
				local source, skip = RefName(owner[frame], tomtomRead)
				if not skip and type(frame) == "table" and not frame.hidden and IsNumber(data.x, data.y) then
					local mapID = data.uiMapID or currentMapID
					local ok, x, y = pcall(core.GetZoneCoordinatesFromWorld, core, data.x, data.y, mapID, true)
					if ok and IsNumber(x, y) then
						local u, v = ns.WorldXY(mapID, x, y)
						if u and (c:Near(u, v) or source == "RestedXP") then
							HBDPin(c, source, frame, u, v)
						end
					end
				end
			end
		end
	end
end

-- HandyNotes plugins don't say what a node is; their tooltip does. Read the first line of it,
-- out of combat and only while no tooltip is up, a few per build, and remember it.
local titles = {}
local titleBudget = 0
local owner

local function TooltipTitle(handler, pluginName, mapID, coord)
	local key = pluginName .. ":" .. mapID .. ":" .. coord
	if titles[key] ~= nil then
		return titles[key] or nil
	end
	local tip = _G.GameTooltip
	if titleBudget <= 0 or type(handler.OnEnter) ~= "function" or not tip or tip:IsShown()
		or (InCombatLockdown and InCombatLockdown()) then
		return nil
	end
	titleBudget = titleBudget - 1
	if not owner then
		owner = CreateFrame("Frame", nil, UIParent)
		owner:SetSize(1, 1)
		owner:SetPoint("TOPLEFT", UIParent, "TOPLEFT", -50, 50)
	end
	owner.pluginName, owner.uiMapID, owner.coord = pluginName, mapID, coord
	local title = false
	if pcall(handler.OnEnter, owner, mapID, coord) then
		local line = _G.GameTooltipTextLeft1
		title = line and Clean(line:GetText()) or false
	end
	if type(handler.OnLeave) == "function" then
		pcall(handler.OnLeave, owner, mapID, coord)
	end
	tip:Hide()
	titles[key] = title
	return title or nil
end

local function CollectHandyNotes(c, maps)
	local hn = Addon("HandyNotes")
	if not hn or type(hn.plugins) ~= "table" then
		return
	end
	local profile = hn.db and hn.db.profile or {}
	local enabled = profile.enabledPlugins
	local scale = IsNumber(profile.icon_scale) and profile.icon_scale or 1
	local own = hn.GetModule and select(2, pcall(hn.GetModule, hn, "HandyNotes", true))
	local notes = type(own) == "table" and own.db and own.db.global or nil
	for pluginName, handler in pairs(hn.plugins) do
		if type(handler) == "table" and handler.GetNodes2 and (not enabled or enabled[pluginName]) then
			local source = pluginName == "HandyNotes" and "HandyNotes" or ("HandyNotes: " .. pluginName)
			for _, mapID in ipairs(maps) do
				local ok, iter, state, value = pcall(handler.GetNodes2, handler, mapID, false)
				local count = 0
				while ok and type(iter) == "function" and count < MAX_NODES do
					local okStep, coord, map2, spec, iconScale = pcall(iter, state, value)
					if not okStep or coord == nil then
						break
					end
					value, count = coord, count + 1
					local m = IsNumber(map2) and map2 or mapID
					if IsNumber(coord) then
						local u, v = ns.WorldXY(m, floor(coord / 10000) / 10000, (coord % 10000) / 10000)
						if u and c:Near(u, v) then
							local title
							if pluginName == "HandyNotes" then
								local note = notes and notes[m] and notes[m][coord]
								title = type(note) == "table" and note.title or nil
							else
								title = TooltipTitle(handler, pluginName, m, coord)
							end
							c:Add({ source = source, u = u, v = v, icon = IconFromSpec(spec),
								size = 12 * scale * (IsNumber(iconScale) and iconScale or 1), title = title })
						end
					end
				end
			end
		end
	end
end

local GATHER_NAMES = {}

local function CollectGatherMate(c, maps)
	local gm = Addon("GatherMate2")
	if not gm or type(gm.GetNodesForZone) ~= "function" or type(gm.db_types) ~= "table" then
		return
	end
	local profile = gm.db and gm.db.profile or {}
	if profile.showWorldMap == false then
		return
	end
	local scale = IsNumber(profile.scale) and profile.scale or 1
	local textures = type(gm.nodeTextures) == "table" and gm.nodeTextures or {}
	for _, dbType in pairs(gm.db_types) do
		if type(dbType) == "string" and (type(gm.Visible) ~= "table" or gm.Visible[dbType]) then
			GATHER_NAMES[dbType] = GATHER_NAMES[dbType] or ("GatherMate2: " .. dbType)
			for _, mapID in ipairs(maps) do
				local zone = type(gm.phasing) == "table" and gm.phasing[mapID] or mapID
				local ok, iter, state, value = pcall(gm.GetNodesForZone, gm, zone, dbType)
				local count = 0
				while ok and type(iter) == "function" and count < MAX_NODES do
					local okStep, coord, nodeID = pcall(iter, state, value)
					if not okStep or coord == nil then
						break
					end
					value, count = coord, count + 1
					local x, y = gm:DecodeLoc(coord)
					local u, v = ns.WorldXY(mapID, x, y)
					if u and c:Near(u, v) then
						nodeID = type(gm.nodeIDReplacementMap) == "table" and gm.nodeIDReplacementMap[nodeID] or nodeID
						local okName, name = pcall(gm.GetNameForNode, gm, dbType, nodeID)
						local file = textures[dbType] and textures[dbType][nodeID]
						c:Add({ source = GATHER_NAMES[dbType], u = u, v = v, icon = file and IconFromSpec(file),
							size = 12 * scale, title = okName and name or nil })
					end
				end
			end
		end
	end
end

---------------------------------------------------------------------------
-- The maps around you: the current one and the zones of its continent whose rect meets the box

local function ContinentOf(mapID)
	for _ = 1, 8 do
		local info = C_Map.GetMapInfo(mapID)
		if not info then
			return nil
		end
		if info.mapType == CONTINENT_TYPE then
			return mapID
		end
		mapID = info.parentMapID
		if not mapID or mapID == 0 then
			return nil
		end
	end
end

local function NearbyMaps(mapID, box)
	local maps, seen = { mapID }, { [mapID] = true }
	local continent = ContinentOf(mapID)
	local ok, zones = false, nil
	if continent then
		ok, zones = pcall(C_Map.GetMapChildrenInfo, continent, ZONE_TYPE)
	end
	for _, zone in ipairs(ok and zones or {}) do
		local id = Safe(zone.mapID)
		local rect = id and not seen[id] and ns.MapRectOnWorld(id)
		if rect and rect[1] < box[2] and rect[2] > box[1] and rect[3] < box[4] and rect[4] > box[3] then
			maps[#maps + 1] = id
			seen[id] = true
		end
	end
	return maps
end

---------------------------------------------------------------------------
-- The quests in your log, where the game's map marks them (as OpenMap's Map/Quests.lua reads them)

local function QuestTitle(id)
	if C_QuestLog.GetTitleForQuestID then
		local ok, title = pcall(C_QuestLog.GetTitleForQuestID, id)
		return ok and title or nil
	end
end

-- What is left to do, as the quest log words it.
local function QuestObjectives(id)
	local count, line = _G.GetNumQuestLeaderBoards, _G.GetQuestLogLeaderBoard
	if not (C_QuestLog.GetLogIndexForQuestID and count and line) then
		return nil
	end
	local ok, index = pcall(C_QuestLog.GetLogIndexForQuestID, id)
	index = ok and Safe(index)
	if not IsNumber(index) then
		return nil
	end
	local okCount, n = pcall(count, index)
	n = okCount and Safe(n)
	local left = {}
	for i = 1, IsNumber(n) and min(n, 8) or 0 do
		local okLine, text, _, finished = pcall(line, i, index)
		text = okLine and Safe(text)
		if type(text) == "string" and text ~= "" and not Safe(finished) then
			left[#left + 1] = text
		end
	end
	return #left > 0 and table.concat(left, "; ") or nil
end

local function CollectQuests(c, maps)
	if not (C_QuestLog and C_QuestLog.GetQuestsOnMap) then
		return
	end
	local seen = {}
	for _, mapID in ipairs(maps) do
		local ok, quests = pcall(C_QuestLog.GetQuestsOnMap, mapID)
		for _, q in ipairs(ok and type(quests) == "table" and quests or {}) do
			local id, x, y = Safe(q.questID), Safe(q.x), Safe(q.y)
			if IsNumber(id, x, y) and not Safe(q.isMapIndicatorQuest) then
				local u, v = ns.WorldXY(mapID, x, y)
				local key = IsNumber(u, v) and (id .. ":" .. floor(u / QUEST_SAME) .. ":" .. floor(v / QUEST_SAME))
				if key and not seen[key] then
					seen[key] = true
					local done = false
					if C_QuestLog.IsComplete then
						local okDone, complete = pcall(C_QuestLog.IsComplete, id)
						done = okDone and Safe(complete) and true or false
					end
					-- "?" where it is handed in, "..." while there is something left to do
					c:Add({ source = "Quests", u = u, v = v, size = 22, label = done and "?" or "...",
						title = QuestTitle(id), sub = not done and QuestObjectives(id) or nil, active = done })
				end
			end
		end
	end
end

---------------------------------------------------------------------------
-- The blob

local function Unit16(v)
	return max(0, min(65535, floor(v * 65535 + 0.5)))
end

function ns.BuildPins(mapID, playerU, playerV)
	local blob = ns.NewBuffer()
	local l, t = ns.WorldXY(mapID, 0, 0)
	local r, b = ns.WorldXY(mapID, 1, 1)
	if not l or not r or r <= l or b <= t then
		return blob -- a dungeon's map: not on the world
	end
	local w, h = (r - l) * NEAR, (b - t) * NEAR
	local box = { l - w, r + w, t - h, b + h }
	local c = NewCollection(box)
	titleBudget = TITLE_BUDGET
	local maps = NearbyMaps(mapID, box)
	local tomtomRead = pcall(CollectTomTom, c) and _G.TomTom and type(_G.TomTom.waypoints) == "table"
	pcall(CollectHereBeDragons, c, mapID, tomtomRead)
	pcall(CollectHandyNotes, c, maps)
	pcall(CollectGatherMate, c, maps)
	pcall(CollectQuests, c, maps)

	local pins = c.pins
	if #pins > MAX_PINS then
		local cu, cv = playerU or (l + r) / 2, playerV or (t + b) / 2
		for _, pin in ipairs(pins) do
			pin.dist = (pin.u - cu) ^ 2 + ((pin.v - cv) / 1.5) ^ 2
		end
		table.sort(pins, function(x, y)
			if x.priority ~= y.priority then
				return x.priority < y.priority
			end
			return x.dist < y.dist
		end)
		for i = #pins, MAX_PINS + 1, -1 do
			pins[i] = nil
		end
	end

	-- the tables hold only what the pins kept use
	local kept, stats = {}, {}
	for _, pin in ipairs(pins) do
		stats[pin.source] = (stats[pin.source] or 0) + 1
		local source = c:Source(pin.source)
		if source then
			kept[#kept + 1] = { source = source, u = pin.u, v = pin.v, icon = c:Icon(pin.icon), size = pin.size,
				title = c:Text(pin.title), sub = c:Text(pin.sub), label = c:Text(pin.label), flags = pin.active and 1 or 0 }
		end
	end
	pins = kept
	ns.pinStats = stats

	local sources = ns.NewBuffer()
	sources:u8(#c.sources)
	for _, name in ipairs(c.sources) do
		sources:str(name)
	end
	blob:record(REC_PINSOURCES, sources)

	local icons = ns.NewBuffer()
	icons:u16(#c.icons)
	for _, icon in ipairs(c.icons) do
		icons:u32(icon.fileID or 0)
		icons:str(icon.fileID and "" or icon.path)
		icons:u16(Unit16(icon.l))
		icons:u16(Unit16(icon.r))
		icons:u16(Unit16(icon.t))
		icons:u16(Unit16(icon.b))
		for _, k in ipairs({ "cr", "cg", "cb", "ca" }) do
			icons:u8(max(0, min(255, floor(icon[k] * 255 + 0.5))))
		end
	end
	blob:record(REC_PINICONS, icons)

	local texts = ns.NewBuffer()
	texts:u16(#c.texts)
	for _, s in ipairs(c.texts) do
		texts:str(s)
	end
	blob:record(REC_PINTEXTS, texts)

	local body = ns.NewBuffer()
	body:u16(#pins)
	for _, pin in ipairs(pins) do
		body:u8(pin.source)
		body:u32(ns.WorldFixed(pin.u))
		body:u32(ns.WorldFixed(pin.v))
		body:u16(pin.icon)
		body:u8(pin.size)
		body:u16(pin.title)
		body:u16(pin.sub)
		body:u16(pin.label)
		body:u8(pin.flags)
	end
	blob:record(REC_PINS, body)
	return blob
end
