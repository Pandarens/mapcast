-- The strip: a grid of solid-color squares in the top-left screen corner that
-- the companion app reads. Each square is exactly cellPx x cellPx physical
-- pixels so captured colors stay exact.
local _, ns = ...

local floor = math.floor

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

-- Bytes available for one page (cells 0-2 are markers and geometry).
function Strip:PageCapacity()
	return (self:CellCount() - 3) * 3
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
	self:SetCell(2, self.cols % 256, floor(self.cols / 256), self.rows)
end

function Strip:SetCell(i, r, g, b)
	local key = r * 65536 + g * 256 + b
	if self.colors[i] ~= key then
		self.colors[i] = key
		self.textures[i]:SetColorTexture(r / 255, g / 255, b / 255, 1)
	end
end

function Strip:Paint(bytes)
	local k = 1
	for i = 3, self:CellCount() - 1 do
		self:SetCell(i, bytes[k] or 0, bytes[k + 1] or 0, bytes[k + 2] or 0)
		k = k + 3
	end
end

function Strip:SetShown(shown)
	self.frame:SetShown(shown)
end
