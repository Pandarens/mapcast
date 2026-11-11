"""CASC reader against a tiny synthetic local storage, plus map composition."""
import hashlib
import io
import os
import struct
import zlib

import pytest
from PIL import Image

from mapcast.casc import LocalCasc, _bucket, blte_decode
from mapcast.maps import MapImages


def blte_single(data: bytes) -> bytes:
    return b"BLTE" + struct.pack(">I", 0) + b"N" + data


def blte_chunked(data: bytes) -> bytes:
    parts = [data[: len(data) // 2], data[len(data) // 2:]]
    chunks = [b"Z" + zlib.compress(parts[0]), b"N" + parts[1]]
    header_size = 12 + 24 * len(chunks)
    table = b"".join(struct.pack(">II", len(c), len(p)) + hashlib.md5(c).digest() for c, p in zip(chunks, parts))
    return b"BLTE" + struct.pack(">I", header_size) + bytes([0x0F]) + len(chunks).to_bytes(3, "big") + table + b"".join(chunks)


def test_blte_modes():
    payload = os.urandom(5000)
    assert blte_decode(blte_single(payload)) == payload
    assert blte_decode(blte_chunked(payload)) == payload


def build_storage(root, files: dict[int, bytes]):
    """Write .build.info, config, encoding, root, one data archive and index files."""
    data_dir = os.path.join(root, "Data", "data")
    os.makedirs(data_dir)
    archive = bytearray()
    index_entries: dict[int, list] = {}

    def store(content: bytes, chunked=False) -> bytes:
        ekey = os.urandom(16)
        blte = blte_chunked(content) if chunked else blte_single(content)
        entry = ekey[::-1] + struct.pack("<I", 30 + len(blte)) + b"\0" * 10 + blte
        index_entries.setdefault(_bucket(ekey), []).append((ekey[:9], len(archive), len(entry)))
        archive.extend(entry)
        return ekey

    ckey_to_ekey = {}
    for fdid, content in files.items():
        ckey_to_ekey[hashlib.md5(content).digest()] = store(content, chunked=fdid % 2 == 0)

    # root: MFST v1, one block, enUS
    ids = sorted(files)
    deltas, prev = [], -1
    for fdid in ids:
        deltas.append(fdid - prev - 1)
        prev = fdid
    root_file = b"TSFM" + struct.pack("<II", len(ids), len(ids))
    root_file += struct.pack("<III", len(ids), 0, 0x2)
    root_file += struct.pack(f"<{len(ids)}i", *deltas)
    root_file += b"".join(hashlib.md5(files[i]).digest() for i in ids)
    root_file += b"\0" * 8 * len(ids)
    root_ckey = hashlib.md5(root_file).digest()
    ckey_to_ekey[root_ckey] = store(root_file)

    # encoding: one 4 KB CE page
    page = bytearray()
    for ckey in sorted(ckey_to_ekey):
        page += bytes([1]) + (123).to_bytes(5, "big") + ckey + ckey_to_ekey[ckey]
    page += b"\0" * (4096 - len(page))
    first_key = sorted(ckey_to_ekey)[0]
    encoding = b"EN" + bytes([1, 16, 16]) + struct.pack(">HHIIBI", 4, 4, 1, 0, 0, 0)
    encoding += first_key + hashlib.md5(page).digest() + bytes(page)
    encoding_ekey = store(encoding)

    with open(os.path.join(data_dir, "data.000"), "wb") as f:
        f.write(archive)
    for bucket, entries in index_entries.items():
        body = b"".join(k + ((0 << 30) | off).to_bytes(5, "big") + struct.pack("<I", size) for k, off, size in entries)
        header = struct.pack("<II", 0x10, 0) + struct.pack("<HBBBBBBQ", 7, bucket, 0, 4, 5, 9, 30, 1 << 30)
        header += b"\0" * (32 - len(header))
        with open(os.path.join(data_dir, f"{bucket:02x}00000001.idx"), "wb") as f:
            f.write(header + struct.pack("<II", len(body), 0) + body)

    build_key = "ab" * 16
    config_dir = os.path.join(root, "Data", "config", build_key[:2], build_key[2:4])
    os.makedirs(config_dir)
    with open(os.path.join(config_dir, build_key), "w") as f:
        f.write(f"# Build Configuration\nroot = {root_ckey.hex()}\nencoding = {'00' * 16} {encoding_ekey.hex()}\n")
    with open(os.path.join(root, ".build.info"), "w") as f:
        f.write("Branch!STRING:0|Active!DEC:1|Build Key!HEX:16|Product!STRING:0|Version!STRING:0\n")
        f.write(f"eu|1|{'cd' * 16}|wow|12.0.5.1\neu|1|{build_key}|wow_forever|1.15.8.1\n")


def png(color, size=(256, 256)) -> bytes:
    out = io.BytesIO()
    Image.new("RGBA", size, color).save(out, "PNG")
    return out.getvalue()


def test_local_casc_reads_files_by_file_data_id(tmp_path):
    files = {100: b"hello", 101: os.urandom(3000), 5000: b"x" * 10000}
    build_storage(str(tmp_path), files)
    casc = LocalCasc(str(tmp_path))
    assert casc.product == "wow_forever"
    for fdid, content in files.items():
        assert casc.read_file(fdid) == content
    with pytest.raises(Exception):
        casc.read_file(4242)


def test_pillow_reads_blp2_dxt1():
    # 4x4 BLP2, DXT1, one block: color0 = pure red, all indices 0
    header = b"BLP2" + struct.pack("<I", 1) + bytes([2, 0, 0, 0]) + struct.pack("<II", 4, 4)
    offsets = [1172] + [0] * 15
    sizes = [8] + [0] * 15
    blp = header + struct.pack("<16I", *offsets) + struct.pack("<16I", *sizes) + b"\0" * 1024
    blp += struct.pack("<HHI", 0xF800, 0x0000, 0)
    image = Image.open(io.BytesIO(blp)).convert("RGBA")
    r, g, b, _ = image.getpixel((0, 0))
    assert image.size == (4, 4) and r >= 248 and g == 0 and b == 0  # RGB565 red, no bit replication


class FakeCasc:
    def __init__(self, files):
        self.files = files

    def read_file(self, fdid):
        return self.files[fdid]


def test_map_composition_tiles_and_explored_overlays(tmp_path):
    files = {i: png((i * 20, 0, 0, 255)) for i in range(1, 13)}
    files[50] = png((0, 255, 0, 255))
    files[51] = png((0, 0, 255, 255), size=(64, 256))  # last column: 300 - 256 = 44 px used of a 64 px file
    maps = MapImages(str(tmp_path), FakeCasc(files))
    info = {"mapID": 1, "mapType": 3, "parentMapID": 0, "name": "Test",
            "layers": [{"w": 1002, "h": 668, "tileW": 256, "tileH": 256, "tiles": list(range(1, 13))}]}
    explored = [{"w": 300, "h": 200, "x": 400, "y": 300, "flags": 0, "files": [50, 51]}]
    desc = maps.register({"info": info, "explored": explored})
    assert desc["width"] == 1002 and not desc["placeholder"]

    image = Image.open(maps.image_path(desc["url"][5:-4]))
    assert image.size == (1002, 668)
    assert image.getpixel((10, 10))[:3] == (20, 0, 0)  # tile 1
    assert image.getpixel((1000, 600))[:3] == (240, 0, 0)  # tile 12, cropped
    assert image.getpixel((500, 350))[:3] == (0, 255, 0)  # overlay piece 1
    assert image.getpixel((680, 350))[:3] == (0, 0, 255)  # overlay piece 2
    assert image.getpixel((710, 350))[:3] != (0, 0, 255)  # beyond the 44 px used


def test_placeholder_without_game_data(tmp_path):
    maps = MapImages(str(tmp_path), None)
    info = {"mapID": 1, "mapType": 3, "parentMapID": 0, "name": "Test",
            "layers": [{"w": 1002, "h": 668, "tileW": 256, "tileH": 256, "tiles": [1, 2]}]}
    desc = maps.register({"info": info, "explored": []})
    assert desc["placeholder"]
    assert Image.open(maps.image_path(desc["url"][5:-4])).size == (1002, 668)
