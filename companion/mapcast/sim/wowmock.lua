-- Just enough of the WoW API to run the MapCast addon outside the game.
-- A fake player walks loops around Elwynn Forest with two party members.
local M = {
	time = 0,
	physW = 1920,
	physH = 1080,
	frames = {},
	mapID = 1429,
	px = 0.5,
	py = 0.5,
	facing = 0,
	party = {},
	secret = false,
}
MockWoW = M

local unpack = unpack or table.unpack

---------------------------------------------------------------------------
-- Frames and textures

local Texture = {}
Texture.__index = Texture

function Texture:ClearAllPoints() self.points = {} end
function Texture:SetPoint(point, rel, relPoint, x, y)
	self.points[#self.points + 1] = { point, rel, relPoint, x or 0, y or 0 }
end
function Texture:SetSize(w, h) self.w, self.h = w, h end
function Texture:Show() self.shown = true end
function Texture:Hide() self.shown = false end
function Texture:SetColorTexture(r, g, b, a) self.color = { r, g, b, a or 1 } end

local Frame = {}
Frame.__index = Frame

function CreateFrame(kind, name, parent)
	local f = setmetatable({ kind = kind, name = name, parent = parent, shown = true, points = {},
		scripts = {}, events = {}, textures = {}, w = 0, h = 0 }, Frame)
	if name then
		_G[name] = f
	end
	M.frames[#M.frames + 1] = f
	return f
end

function Frame:SetFrameStrata(s) self.strata = s end
function Frame:SetFrameLevel(l) self.level = l end
function Frame:GetEffectiveScale() return 1 end
function Frame:ClearAllPoints() self.points = {} end
function Frame:SetPoint(point, rel, relPoint, x, y)
	self.points[#self.points + 1] = { point, rel, relPoint, x or 0, y or 0 }
end
function Frame:SetSize(w, h) self.w, self.h = w, h end
function Frame:SetShown(s) self.shown = not not s end
function Frame:Show() self.shown = true end
function Frame:Hide() self.shown = false end
function Frame:IsShown() return self.shown end
function Frame:SetScript(n, fn) self.scripts[n] = fn end
function Frame:RegisterEvent(e) self.events[e] = true end
function Frame:CreateTexture()
	local t = setmetatable({ points = {}, shown = true, w = 0, h = 0, color = { 0, 0, 0, 1 } }, Texture)
	self.textures[#self.textures + 1] = t
	return t
end

function M.fire(event, ...)
	for _, f in ipairs(M.frames) do
		if f.events[event] and f.scripts.OnEvent then
			f.scripts.OnEvent(f, event, ...)
		end
	end
end

-- Flat list of {x, y, w, h, r, g, b} in UI units for every visible strip texture.
-- Only TOPLEFT anchoring is supported, which is all the addon uses.
function M.rects()
	local out = {}
	for _, f in ipairs(M.frames) do
		if f.shown and #f.textures > 0 then
			local fx, fy = 0, 0
			local p = f.points[1]
			if p and p[2] then
				fx, fy = p[4], -p[5]
			end
			for _, t in ipairs(f.textures) do
				if t.shown and t.points[1] then
					local tp = t.points[1]
					local r, g, b = unpack(t.color)
					out[#out + 1] = { fx + tp[4], fy - tp[5], t.w, t.h, r, g, b }
				end
			end
		end
	end
	return out
end

---------------------------------------------------------------------------
-- Game world

local function Vector(x, y)
	return { GetXY = function() return x, y end }
end

-- Maps the mock knows: name, type, parent, and rect on Azeroth's map (947). The continent rects
-- are fitted to OpenMap's art; Elwynn's is read off its stitched zone tiles (close, not exact).
M.maps = {
	[947] = { name = "Азерот", type = 1, parent = 0, rect = { 0, 1, 0, 1 } },
	[1414] = { name = "Калимдор", type = 2, parent = 947, rect = { -0.1679, 0.6338, 0.0497, 0.8514 } },
	[1415] = { name = "Восточные королевства", type = 2, parent = 947, rect = { 0.3621, 1.1638, 0.0819, 0.8836 } },
	[1429] = { name = "Элвиннский лес", type = 3, parent = 1415, rect = { 0.7086, 0.7685, 0.6138, 0.6737 } },
}

C_Map = {}
function C_Map.GetBestMapForUnit() return M.mapID end
function C_Map.GetMapRectOnMap(mapID, topMapID)
	local map = M.maps[mapID]
	if topMapID ~= 947 or not map then return nil end
	return unpack(map.rect)
end
function C_Map.GetMapChildrenInfo(mapID, mapType)
	local out = {}
	for id, map in pairs(M.maps) do
		if map.parent == mapID and (not mapType or map.type == mapType) then
			out[#out + 1] = { mapID = id, name = map.name, mapType = map.type, parentMapID = map.parent }
		end
	end
	table.sort(out, function(a, b) return a.mapID < b.mapID end)
	return out
end
function C_Map.GetPlayerMapPosition(mapID, unit)
	if mapID ~= M.mapID then return nil end
	if unit == "player" then return Vector(M.px, M.py) end
	local member = M.party[unit]
	if member then return Vector(member.x, member.y) end
	return nil
end
function C_Map.GetMapInfo(mapID)
	local map = M.maps[mapID]
	if not map then return nil end
	return { mapID = mapID, name = map.name, mapType = map.type, parentMapID = map.parent }
end
function C_Map.GetMapArtLayers()
	return { { layerWidth = 1002, layerHeight = 668, tileWidth = 256, tileHeight = 256 } }
end
function C_Map.GetMapArtLayerTextures(mapID)
	local base = mapID == 1429 and 100000 or mapID * 1000
	local tiles = {}
	for i = 1, 12 do tiles[i] = base + i end
	return tiles
end

-- OpenMap's public API, as far as MapCast uses it (SimGame(openmap=False) removes it).
OpenMapAPI = { major = 1, version = 4 }
function OpenMapAPI.WorldFromMap(mapID, x, y)
	local map = M.maps[mapID]
	if not map then return nil end
	local l, r, t, b = unpack(map.rect)
	return l + x * (r - l), t + y * (b - t)
end
C_AddOns = {}
function C_AddOns.GetAddOnMetadata(name, field)
	if name == "OpenMap" and field == "Version" then return "0.8.3" end
end

C_MapExplorationInfo = {}
function C_MapExplorationInfo.GetExploredMapTextures()
	return {
		{ textureWidth = 300, textureHeight = 200, offsetX = 400, offsetY = 300, isShownByMouseOver = false,
			fileDataIDs = { 200001, 200002 } },
		{ textureWidth = 128, textureHeight = 128, offsetX = 10, offsetY = 10, isShownByMouseOver = true,
			fileDataIDs = { 200003 } },
	}
end

function GetPlayerFacing() return M.facing end
function GetPhysicalScreenSize() return M.physW, M.physH end
function GetTime() return M.time end
function GetBuildInfo() return "1.15.8", "60000", "Nov 1 2026", 16001 end
function GetLocale() return "ruRU" end
function GetRealmName() return "Тестовый мир" end
function UnitName(unit) return unit == "player" and "Тестер" or unit end
function GetUnitName(unit) return M.party[unit] and M.party[unit].name or unit end
function UnitClass(unit)
	if unit == "player" then return "Mage", "MAGE", 8 end
	local member = M.party[unit]
	return "x", "x", member and member.class or 1
end
function UnitAffectingCombat() return false end
function UnitIsDeadOrGhost() return false end
function UnitIsConnected() return true end
function UnitIsUnit(a, b) return a == b end
function IsMounted() return true end
function IsFlying() return false end
function IsInInstance() return false end
function IsResting() return false end
function IsInRaid() return false end
function IsInGroup() return next(M.party) ~= nil end
function GetNumGroupMembers() return 3 end
function GetNumSubgroupMembers() return 2 end

SlashCmdList = {}
function print(...) M.lastPrint = table.concat({ ... }, " ") end

---------------------------------------------------------------------------
-- Simulation step: walk a loop, party members trail behind, run OnUpdate.

function M.step(dt)
	M.time = M.time + dt
	local t = M.time * 0.05
	local nx = 0.45 + 0.18 * math.cos(t)
	local ny = 0.55 + 0.15 * math.sin(t)
	local dx, dy = nx - M.px, ny - M.py
	if dx ~= 0 or dy ~= 0 then
		-- WoW facing: 0 = north (up), counter-clockwise.
		M.facing = math.atan2(-dx, -dy) % (2 * math.pi)
	end
	M.px, M.py = nx, ny
	M.party.party1 = { x = 0.45 + 0.18 * math.cos(t - 0.15), y = 0.55 + 0.15 * math.sin(t - 0.15), class = 1, name = "Воин" }
	M.party.party2 = { x = 0.45 + 0.18 * math.cos(t - 0.3), y = 0.55 + 0.15 * math.sin(t - 0.3), class = 5, name = "Жрица" }
	for _, f in ipairs(M.frames) do
		if f.shown and f.scripts.OnUpdate then
			f.scripts.OnUpdate(f, dt)
		end
	end
end
