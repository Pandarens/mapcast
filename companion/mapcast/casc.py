"""Minimal read-only reader for the local CASC storage of an installed WoW client.

Only what MapCast needs: read a file by FileDataID (map tiles, icons).
Formats: https://wowdev.wiki/CASC and https://wowdev.wiki/TACT
"""
from __future__ import annotations

import bisect
import glob
import hashlib
import os
import struct
import zlib

import numpy as np

LOCALE_ENUS, LOCALE_ENGB, LOCALE_RURU = 0x2, 0x200, 0x2000
CONTENT_LOW_VIOLENCE = 0x80
CONTENT_NO_NAME_HASH = 0x10000000
DATA_HEADER_SIZE = 30


class CascError(Exception):
    pass


def parse_table(text: str) -> list[dict]:
    """Parse a Blizzard 'Name!TYPE:n|...' table such as .build.info."""
    lines = [line for line in text.splitlines() if line.strip() and not line.startswith("#")]
    if not lines:
        return []
    header = [col.split("!")[0] for col in lines[0].split("|")]
    return [dict(zip(header, line.split("|"))) for line in lines[1:]]


def parse_config(text: str) -> dict[str, list[str]]:
    config = {}
    for line in text.splitlines():
        line = line.strip()
        if line and not line.startswith("#") and "=" in line:
            key, value = line.split("=", 1)
            config[key.strip()] = value.split()
    return config


def blte_decode(data: bytes) -> bytes:
    if data[:4] != b"BLTE":
        raise CascError("not a BLTE stream")
    header_size = struct.unpack_from(">I", data, 4)[0]
    if header_size == 0:
        return _decode_chunk(data[8:])
    count = int.from_bytes(data[9:12], "big")
    chunks = [struct.unpack_from(">II", data, 12 + 24 * i) for i in range(count)]
    out, pos = [], header_size
    for compressed_size, _ in chunks:
        out.append(_decode_chunk(data[pos:pos + compressed_size]))
        pos += compressed_size
    return b"".join(out)


def _decode_chunk(chunk: bytes) -> bytes:
    mode = chunk[:1]
    if mode == b"N":
        return chunk[1:]
    if mode == b"Z":
        return zlib.decompress(chunk[1:])
    if mode == b"F":
        return blte_decode(chunk[1:])
    if mode == b"E":
        raise CascError("file is encrypted")
    raise CascError(f"unsupported BLTE mode {mode!r}")


class _Index:
    """One local .idx bucket file: 9-byte EKey prefix -> (archive, offset, size)."""

    def __init__(self, path: str):
        with open(path, "rb") as f:
            data = f.read()
        header_size = struct.unpack_from("<I", data, 0)[0]
        _, _, _, size_len, offset_len, key_len, offset_bits = struct.unpack_from("<HBBBBBB", data, 8)
        pos = (8 + header_size + 15) & ~15
        entries_size = struct.unpack_from("<I", data, pos)[0]
        pos += 8
        entry_len = key_len + offset_len + size_len
        n = entries_size // entry_len
        raw = np.frombuffer(data, np.uint8, n * entry_len, pos).reshape(n, entry_len)

        keys = np.ascontiguousarray(raw[:, :key_len]).view(f"S{key_len}").ravel()
        location = np.zeros(n, np.int64)
        for i in range(offset_len):
            location = (location << 8) | raw[:, key_len + i]
        sizes = np.zeros(n, np.int64)
        for i in range(size_len):
            sizes |= raw[:, key_len + offset_len + i].astype(np.int64) << (8 * i)

        order = np.argsort(keys, kind="stable")
        self.key_len = key_len
        self.keys = keys[order]
        self.archives = (location >> offset_bits)[order]
        self.offsets = (location & ((1 << offset_bits) - 1))[order]
        self.sizes = sizes[order]

    def find(self, ekey: bytes):
        key = np.array([ekey[:self.key_len]], dtype=f"S{self.key_len}")
        i = int(np.searchsorted(self.keys, key)[0])
        if i < len(self.keys) and self.keys[i] == key[0]:
            return int(self.archives[i]), int(self.offsets[i]), int(self.sizes[i])
        return None


class _Encoding:
    """CKey -> EKey lookup over the paged encoding file."""

    def __init__(self, data: bytes):
        if data[:2] != b"EN":
            raise CascError("bad encoding file")
        self.ckey_len, self.ekey_len = data[3], data[4]
        page_kb = struct.unpack_from(">H", data, 5)[0]
        page_count = struct.unpack_from(">I", data, 9)[0]
        espec_size = struct.unpack_from(">I", data, 18)[0]
        pos = 22 + espec_size
        entry = self.ckey_len + 16
        self.first_keys = [data[pos + i * entry:pos + i * entry + self.ckey_len] for i in range(page_count)]
        self.pages_start = pos + page_count * entry
        self.page_size = page_kb * 1024
        self.data = data

    def ekey(self, ckey: bytes) -> bytes | None:
        page = bisect.bisect_right(self.first_keys, ckey) - 1
        if page < 0:
            return None
        data, ck, ek = self.data, self.ckey_len, self.ekey_len
        pos = self.pages_start + page * self.page_size
        end = pos + self.page_size
        while pos + 6 + ck <= end:
            key_count = data[pos]
            if key_count == 0:
                break
            if data[pos + 6:pos + 6 + ck] == ckey:
                return data[pos + 6 + ck:pos + 6 + ck + ek]
            pos += 6 + ck + key_count * ek
        return None


class _Root:
    """FileDataID -> CKey from the TSFM ('MFST') root file."""

    def __init__(self, data: bytes, locale_mask: int):
        version, pos, allow_nameless = 0, 0, False
        if data[:4] == b"TSFM":
            first, second = struct.unpack_from("<II", data, 4)
            if first == 0x18:  # header with explicit size and version (11.x)
                version = second
                total, named = struct.unpack_from("<II", data, 12)
                pos = first
            else:
                total, named = first, second
                pos = 12
            allow_nameless = total != named

        ids_parts, key_parts = [], []
        while pos + 12 <= len(data):
            if version >= 2:
                count, locale, flags1, flags2 = struct.unpack_from("<IIII", data, pos)
                content = flags1 | flags2 | (data[pos + 16] << 17)
                pos += 17
            else:
                count, content, locale = struct.unpack_from("<III", data, pos)
                pos += 12
            deltas = np.frombuffer(data, "<i4", count, pos)
            pos += 4 * count
            ckeys = np.frombuffer(data, np.uint8, count * 16, pos).reshape(count, 16)
            pos += 16 * count
            if not (allow_nameless and content & CONTENT_NO_NAME_HASH):
                pos += 8 * count
            if content & CONTENT_LOW_VIOLENCE or not locale & locale_mask:
                continue
            ids_parts.append(np.cumsum(deltas.astype(np.int64) + 1) - 1)
            key_parts.append(ckeys)

        if not ids_parts:
            raise CascError("root file has no usable entries")
        ids = np.concatenate(ids_parts)
        keys = np.concatenate(key_parts)
        order = np.argsort(ids, kind="stable")  # stable: earlier blocks win on duplicates
        ids, keys = ids[order], keys[order]
        first = np.ones(len(ids), bool)
        first[1:] = ids[1:] != ids[:-1]
        self.ids, self.keys = ids[first], keys[first]

    def ckey(self, file_data_id: int) -> bytes | None:
        i = int(np.searchsorted(self.ids, file_data_id))
        if i < len(self.ids) and self.ids[i] == file_data_id:
            return self.keys[i].tobytes()
        return None


class LocalCasc:
    """Read files by FileDataID from an installed client."""

    def __init__(self, game_root: str, product: str | None = None):
        self.root = game_root
        self.data_dir = os.path.join(game_root, "Data")
        rows = parse_table(_read_text(os.path.join(game_root, ".build.info")))
        row = _pick_build(rows, product)
        if row is None:
            raise CascError(f"no build for product {product!r} in .build.info")
        self.product = row.get("Product", "")
        self.version = row.get("Version", "")
        build_key = row["Build Key"]
        config = parse_config(_read_text(os.path.join(self.data_dir, "config", build_key[:2], build_key[2:4], build_key)))

        self._indices = _load_indices(os.path.join(self.data_dir, "data"))
        self._encoding = _Encoding(self.read_ekey(bytes.fromhex(config["encoding"][1])))
        root_ckey = bytes.fromhex(config["root"][0])
        root_ekey = self._encoding.ekey(root_ckey)
        if root_ekey is None:
            raise CascError("root file not found in encoding")
        self._root = _Root(self.read_ekey(root_ekey), LOCALE_ENUS | LOCALE_ENGB | LOCALE_RURU)

    def read_ekey(self, ekey: bytes) -> bytes:
        index = self._indices.get(_bucket(ekey))
        location = index.find(ekey) if index else None
        if location is None:
            raise CascError(f"ekey {ekey.hex()} not in local storage")
        archive, offset, size = location
        with open(os.path.join(self.data_dir, "data", f"data.{archive:03d}"), "rb") as f:
            f.seek(offset)
            blob = f.read(size)
        return blte_decode(blob[DATA_HEADER_SIZE:])

    def read_file(self, file_data_id: int) -> bytes:
        ckey = self._root.ckey(file_data_id)
        if ckey is None:
            raise CascError(f"FileDataID {file_data_id} not in root")
        ekey = self._encoding.ekey(ckey)
        if ekey is None:
            raise CascError(f"FileDataID {file_data_id} not in encoding")
        data = self.read_ekey(ekey)
        if hashlib.md5(data).digest() != ckey:
            raise CascError(f"FileDataID {file_data_id} failed its checksum")
        return data


def _read_text(path: str) -> str:
    with open(path, encoding="utf-8", errors="replace") as f:
        return f.read()


def _pick_build(rows: list[dict], product: str | None) -> dict | None:
    active = [r for r in rows if r.get("Active", "1") == "1"] or rows
    if product:
        for r in active:
            if r.get("Product") == product:
                return r
        return None
    for r in active:
        if "forever" in r.get("Product", ""):
            return r
    return active[0] if active else None


def _bucket(ekey: bytes) -> int:
    h = 0
    for b in ekey[:9]:
        h ^= b
    return (h & 0xF) ^ (h >> 4)


def _load_indices(data_dir: str) -> dict[int, _Index]:
    latest: dict[int, tuple[int, str]] = {}
    for path in glob.glob(os.path.join(data_dir, "*.idx")):
        name = os.path.basename(path)[:-4]
        if len(name) != 10:
            continue
        bucket, version = int(name[:2], 16), int(name[2:], 16)
        if bucket not in latest or version > latest[bucket][0]:
            latest[bucket] = (version, path)
    if not latest:
        raise CascError(f"no CASC index files in {data_dir}")
    return {bucket: _Index(path) for bucket, (_, path) in latest.items()}


def flavor_product(flavor_dir: str) -> str | None:
    """Product code from a flavor folder such as _forever_/.flavor.info."""
    path = os.path.join(flavor_dir, ".flavor.info")
    if not os.path.exists(path):
        return None
    rows = parse_table(_read_text(path))
    if rows:
        return next(iter(rows[0].values()), None)
    return None
