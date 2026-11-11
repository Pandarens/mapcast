-- Just enough of the map addons MapCast reads pins from, for the simulator and the tests:
-- LibStub and AceAddon, HereBeDragons (and Questie's copy of it), TomTom, HandyNotes with two
-- plugins, GatherMate2, Questie and RestedXP Guide, with a few pins around Elwynn Forest.
local M = MockWoW
local unpack = unpack or table.unpack

---------------------------------------------------------------------------
-- LibStub, AceAddon

local libs = {}
LibStub = setmetatable({ libs = libs }, {
	__call = function(_, name, silent)
		if not libs[name] and not silent then error("LibStub: no " .. name) end
		return libs[name]
	end,
})
function LibStub:NewLibrary(name)
	libs[name] = libs[name] or {}
	return libs[name]
end

local Ace = LibStub:NewLibrary("AceAddon-3.0")
Ace.addons = {}
function Ace:GetAddon(name, silent)
	if not self.addons[name] and not silent then error("AceAddon: no " .. name) end
	return self.addons[name]
end

function InCombatLockdown() return M.combat or false end
UIParent = UIParent or CreateFrame("Frame", "UIParent")

---------------------------------------------------------------------------
-- The tooltip HandyNotes plugins write to

GameTooltip = { lines = {}, shown = false }
function GameTooltip:SetOwner(owner) self.owner, self.lines = owner, {} end
function GameTooltip:SetText(text) self.lines = { text } end
function GameTooltip:AddLine(text) self.lines[#self.lines + 1] = text end
function GameTooltip:Show() self.shown = true end
function GameTooltip:Hide() self.shown = false end
function GameTooltip:IsShown() return self.shown end
GameTooltipTextLeft1 = { GetText = function() return GameTooltip.lines[1] end }

---------------------------------------------------------------------------
-- HereBeDragons: world coordinates here are simply Azeroth's (u, v), enough for its pins

local function Rect(mapID)
	local map = M.maps[mapID]
	if not map then return nil end
	return unpack(map.rect)
end

local function NewHBD(core, pinsName)
	local hbd = LibStub:NewLibrary(core)
	function hbd:GetWorldCoordinatesFromZone(x, y, mapID)
		local l, r, t, b = Rect(mapID)
		if not l then return nil end
		return l + x * (r - l), t + y * (b - t), 0
	end
	function hbd:GetZoneCoordinatesFromWorld(u, v, mapID)
		local l, r, t, b = Rect(mapID)
		if not l then return nil end
		return (u - l) / (r - l), (v - t) / (b - t)
	end
	local pins = LibStub:NewLibrary(pinsName)
	pins.worldmapPins, pins.worldmapPinRegistry = {}, {}
	function pins:AddWorldMapIconMap(ref, icon, mapID, x, y)
		local u, v, instance = hbd:GetWorldCoordinatesFromZone(x, y, mapID)
		if not u then return false end
		self.worldmapPinRegistry[ref] = self.worldmapPinRegistry[ref] or {}
		self.worldmapPinRegistry[ref][icon] = true
		self.worldmapPins[icon] = { instanceID = instance, x = u, y = v, uiMapID = mapID, worldMapShowFlag = 0 }
		return true
	end
	return pins
end

local HBDPins = NewHBD("HereBeDragons-2.0", "HereBeDragons-Pins-2.0")
local QuestiePins = NewHBD("HereBeDragonsQuestie-2.0", "HereBeDragonsQuestie-Pins-2.0")

local function IconFrame(file, size)
	local f = CreateFrame("Button")
	f:Hide()
	f:SetSize(size or 16, size or 16)
	f.texture = f:CreateTexture()
	f.texture:SetTexture(file)
	return f
end

---------------------------------------------------------------------------
-- TomTom: a waypoint (its HereBeDragons pin is skipped: the list has the title)

TomTom = { waypoints = { [1429] = { ["1429:4200:6600:Ферма Брекенридж"] = { 1429, 0.42, 0.66, title = "Ферма Брекенридж" } } } }
HBDPins:AddWorldMapIconMap(TomTom, IconFrame("Interface\\AddOns\\TomTom\\Images\\GoldGreenDot", 16), 1429, 0.42, 0.66)

---------------------------------------------------------------------------
-- Questie: a quest giver and kill objectives (one icon faded out by Questie)

Questie = { name = "Questie" }
local function QuestieIcon(file, mapID, x, y, data, hidden)
	local f = IconFrame(file, 16)
	f.data, f.hidden = data, hidden
	QuestiePins:AddWorldMapIconMap(Questie, f, mapID, x, y)
	return f
end
local wolves = { name = "Охота на волков", level = 6 }
QuestieIcon("Interface\\Addons\\Questie\\Icons\\available.blp", 1429, 0.43, 0.65,
	{ Name = "Маршал Дуган", Type = "available", QuestData = { name = "Кобольды в шахте", level = 7 } })
QuestieIcon("Interface\\Addons\\Questie\\Icons\\slay.blp", 1429, 0.52, 0.58, { Name = "Волк", Type = "monster", QuestData = wolves })
QuestieIcon("Interface\\Addons\\Questie\\Icons\\slay.blp", 1429, 0.55, 0.61, { Name = "Волк", Type = "monster", QuestData = wolves })
QuestieIcon("Interface\\Addons\\Questie\\Icons\\loot.blp", 1429, 0.6, 0.5, { Name = "Свеча", Type = "item", QuestData = wolves }, true)
QuestieIcon("Interface\\Addons\\Questie\\Icons\\available.blp", 1440, 0.5, 0.5,
	{ Name = "Далеко", Type = "available", QuestData = { name = "В Ясеневом лесу", level = 20 } })

---------------------------------------------------------------------------
-- RestedXP Guide: the active step and the next one, numbered circles

RXPGuides = {}
local rxp = { RXPGuides = RXPGuides }
local function Step(index, x, y, text, active)
	local f = CreateFrame("Button")
	f:Hide()
	f:SetSize(18, 18)
	f.text = f:CreateFontString()
	f.text:SetText(tostring(index))
	f.activeObject = { elements = { { text = text, step = { index = index, active = active } } } }
	HBDPins:AddWorldMapIconMap(rxp, f, 1429, x, y)
end
Step(12, 0.48, 0.42, "Поговорите с Маршалом Дуганом", true)
Step(13, 0.4, 0.8, "Убейте 8 молодых волков", false)

-- an addon M doesn't know: its pin comes as it is, by its texture
local other = { name = "SilverDragon" }
HBDPins:AddWorldMapIconMap(other, IconFrame(132320, 14), 1429, 0.7, 0.3)

---------------------------------------------------------------------------
-- HandyNotes: your own notes, a plugin that names its nodes only in the tooltip, a disabled one

local function Coord(x, y) return math.floor(x * 10000 + 0.5) * 10000 + math.floor(y * 10000 + 0.5) end

local function NodesOf(points)
	return function(_, mapID)
		local list = points[mapID] or {}
		return function(t, prev)
			local coord, node = next(t, prev)
			if coord == nil then return nil end
			return coord, nil, node.icon, node.scale or 1, 1
		end, list, nil
	end
end

local notes = { [1429] = { [Coord(0.3, 0.55)] = { title = "Мой тайник", icon = 1 } } }
local HN = {
	plugins = {},
	db = { profile = { icon_scale = 1.2, enabledPlugins = setmetatable({ Disabled = false }, { __index = function() return true end }) } },
	modules = { HandyNotes = { db = { global = notes } } },
}
function HN:GetModule(name) return self.modules[name] end
Ace.addons.HandyNotes = HN

local starIcon = { icon = "Interface\\TargetingFrame\\UI-RaidTargetingIcons", tCoordLeft = 0, tCoordRight = 0.25, tCoordTop = 0, tCoordBottom = 0.25 }
HN.plugins.HandyNotes = { GetNodes2 = NodesOf({ [1429] = { [Coord(0.3, 0.55)] = { icon = starIcon } } }) }

local treasures = {
	[1429] = { [Coord(0.62, 0.7)] = { icon = "Interface\\Icons\\INV_Misc_Bag_10", label = "Сундук с сокровищами" } },
	[1436] = { [Coord(0.5, 0.2)] = { icon = "Interface\\Icons\\INV_Misc_Bag_10", label = "Сундук в Западном Крае" } },
}
HN.plugins.Treasures = {
	GetNodes2 = NodesOf(treasures),
	OnEnter = function(pin, mapID, coord)
		GameTooltip:SetOwner(pin)
		GameTooltip:SetText(treasures[mapID][coord].label)
		GameTooltip:Show()
	end,
	OnLeave = function() GameTooltip:Hide() end,
}
HN.plugins.Disabled = { GetNodes2 = NodesOf({ [1429] = { [Coord(0.5, 0.5)] = { icon = 134400 } } }) }

---------------------------------------------------------------------------
-- GatherMate2: herbs and ore it remembered

local GM_NODES = {
	["Herb Gathering"] = { [1429] = { [Coord(0.35, 0.6) * 100] = 401, [Coord(0.58, 0.72) * 100] = 402 } },
	["Mining"] = { [1429] = { [Coord(0.66, 0.45) * 100] = 201 } },
}
local GM = {
	db_types = { "Herb Gathering", "Mining" },
	Visible = { ["Herb Gathering"] = true, Mining = true },
	db = { profile = { scale = 1, showWorldMap = true } },
	nodeTextures = {
		["Herb Gathering"] = { [401] = "Interface\\AddOns\\GatherMate2\\Artwork\\Herb\\peacebloom.tga",
			[402] = "Interface\\AddOns\\GatherMate2\\Artwork\\Herb\\silverleaf.tga" },
		Mining = { [201] = "Interface\\AddOns\\GatherMate2\\Artwork\\Mine\\copper.tga" },
	},
	names = { [401] = "Мироцвет", [402] = "Сребролист", [201] = "Медная жила" },
}
function GM:GetNodesForZone(zone, nodeType) return next, (GM_NODES[nodeType] or {})[zone] or {}, nil end
function GM:DecodeLoc(id) return math.floor(id / 1000000) / 10000, math.floor(id % 1000000 / 100) / 10000 end
function GM:GetNameForNode(_, nodeID) return self.names[nodeID] end
Ace.addons.GatherMate2 = GM
