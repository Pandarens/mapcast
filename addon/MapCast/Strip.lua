-- The strip: a grid of solid-color squares in the top-left screen corner that
-- the companion app reads. Each square is exactly cellPx x cellPx physical pixels.
--
-- A square carries three 4-bit values, one per colour channel, as one of 16 evenly
-- spaced shades. The strip starts with all 16 shades in order, so the companion reads
-- them back as they really arrive: HDR, gamma or a colour profile may shift the shades,
-- and the data still decodes. See docs/PROTOCOL.md.
local _, ns = ...

local floor = math.floor

local SHADE = 17 -- 255 / 15: the step between two shades
local RAMP_AT, GEOMETRY_AT, HEADER = 2, 18, 20 -- cells: 2 markers, 16 shades, 2 of geometry

local Strip = {}
ns.Strip = Strip

function Strip:Init(cellPx, cols, rows)
	self.cellPx, self.cols, self.rows = cellPx, cols, rows
	if not self.frame then
		-- No parent: stays visible when the UI is hidden (Alt+Z) and ignores UI scale.
		local frame = CreateFrame("Frame", "MapCastStrip")
		frame:SetFrameStrata("TOOLTIP")
		frame:SetFrameLevel(9000)
		self.frame = frame
		self.textures = {}
	end
	self:Layout()
end

function Strip:CellCount()
	return self.cols * self.rows
end

-- Bytes available for one page: three half-bytes a cell after the header.
function Strip:PageCapacity()
	return floor((self:CellCount() - HEADER) * 3 / 2)
end

function Strip:Layout()
	local frame = self.frame
	local _, physicalHeight = GetPhysicalScreenSize()
	-- The root coordinate space is 768 units tall at scale 1.
	local pixel = 768 / physicalHeight / frame:GetEffectiveScale()
	local cell = pixel * self.cellPx
	self.pixelUnits = pixel

	frame:ClearAllPoints()
	frame:SetPoint("TOPLEFT")
	frame:SetSize(cell * self.cols, cell * self.rows)

	local count = self:CellCount()
	for i = 0, count - 1 do
		local texture = self.textures[i]
		if not texture then
			texture = frame:CreateTexture(nil, "OVERLAY")
			self.textures[i] = texture
		end
		texture:ClearAllPoints()
		texture:SetSize(cell, cell)
		texture:SetPoint("TOPLEFT", frame, "TOPLEFT", (i % self.cols) * cell, -floor(i / self.cols) * cell)
		texture:Show()
	end
	local extra = count
	while self.textures[extra] do
		self.textures[extra]:Hide()
		extra = extra + 1
	end

	self.colors = {}
	self:SetCell(0, 255, 0, 255)
	self:SetCell(1, 0, 255, 0)
	for k = 0, 15 do
		self:SetCell(RAMP_AT + k, k * SHADE, k * SHADE, k * SHADE)
	end
	local cols = self.cols
	self:SetCell(GEOMETRY_AT, (cols % 16) * SHADE, (floor(cols / 16) % 16) * SHADE, (floor(cols / 256) % 16) * SHADE)
	self:SetCell(GEOMETRY_AT + 1, (self.rows - 1) * SHADE, 0, 0)
end

function Strip:SetCell(i, r, g, b)
	local key = r * 65536 + g * 256 + b
	if self.colors[i] ~= key then
		self.colors[i] = key
		self.textures[i]:SetColorTexture(r / 255, g / 255, b / 255, 1)
	end
end

-- Half-byte j (from 0) of the page: a byte's low half first, then its high half.
local function Nibble(bytes, j)
	local byte = bytes[floor(j / 2) + 1] or 0
	if j % 2 == 0 then
		return byte % 16
	end
	return floor(byte / 16)
end

function Strip:Paint(bytes)
	local j = 0
	for i = HEADER, self:CellCount() - 1 do
		self:SetCell(i, Nibble(bytes, j) * SHADE, Nibble(bytes, j + 1) * SHADE, Nibble(bytes, j + 2) * SHADE)
		j = j + 3
	end
end

function Strip:SetShown(shown)
	self.frame:SetShown(shown)
end
