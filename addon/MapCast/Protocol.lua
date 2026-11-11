-- Byte buffers, CRC16 and page framing for the MapCast pixel protocol.
-- See docs/PROTOCOL.md. All integers are little-endian.
local _, ns = ...

local floor = math.floor
local band, bxor, lshift, rshift = bit.band, bit.bxor, bit.lshift, bit.rshift

ns.PROTOCOL_VERSION = 1
ns.PAGE_OVERHEAD = 6 -- version, seq, length (2), crc (2)
ns.RECORD_OVERHEAD = 3 -- type, length (2)

local Buffer = {}
Buffer.__index = Buffer

function ns.NewBuffer()
	return setmetatable({ n = 0 }, Buffer)
end

function Buffer:u8(v)
	local n = self.n + 1
	self[n] = floor(v) % 256
	self.n = n
end

function Buffer:u16(v)
	v = floor(v)
	self:u8(v % 256)
	self:u8(floor(v / 256) % 256)
end

function Buffer:u32(v)
	v = floor(v)
	self:u8(v % 256)
	self:u8(floor(v / 256) % 256)
	self:u8(floor(v / 65536) % 256)
	self:u8(floor(v / 16777216) % 256)
end

-- Length-prefixed UTF-8 string, cut to 255 bytes on a character boundary.
function Buffer:str(s)
	s = s or ""
	local len = #s
	if len > 255 then
		len = 255
		while len > 0 do
			local nextByte = s:byte(len + 1)
			if nextByte < 128 or nextByte >= 192 then
				break
			end
			len = len - 1
		end
	end
	self:u8(len)
	for i = 1, len do
		self:u8(s:byte(i))
	end
end

function Buffer:append(other, first, last)
	local n = self.n
	for i = first or 1, last or other.n do
		n = n + 1
		self[n] = other[i]
	end
	self.n = n
end

function Buffer:record(recordType, body)
	self:u8(recordType)
	self:u16(body.n)
	self:append(body)
end

-- Cheap content signature, used to detect when a bulk blob really changed.
function Buffer:signature()
	local h = 0
	for i = 1, self.n do
		h = (h * 31 + self[i]) % 2147483629
	end
	return self.n .. ":" .. h
end

-- CRC16-CCITT-FALSE: poly 0x1021, init 0xFFFF, no reflection, no final xor.
local crcTable = {}
for i = 0, 255 do
	local c = lshift(i, 8)
	for _ = 1, 8 do
		if band(c, 0x8000) ~= 0 then
			c = bxor(lshift(c, 1), 0x1021)
		else
			c = lshift(c, 1)
		end
		c = band(c, 0xFFFF)
	end
	crcTable[i] = c
end

function ns.CRC16(buf, first, last)
	local crc = 0xFFFF
	for i = first, last do
		local index = bxor(rshift(crc, 8), buf[i])
		crc = band(bxor(lshift(crc, 8), crcTable[index]), 0xFFFF)
	end
	return crc
end

function ns.BuildPage(seq, payload)
	local page = ns.NewBuffer()
	page:u8(ns.PROTOCOL_VERSION)
	page:u8(seq)
	page:u16(payload.n)
	page:append(payload)
	page:u16(ns.CRC16(page, 1, page.n))
	return page
end
