"""Fast BLP2 decoding with numpy (Pillow decodes DXT in pure Python, ~0.1 s per 512 px tile).

Handles BLP2 with DXT1/3/5 or raw BGRA; anything else goes to Pillow.
"""
from __future__ import annotations

import io
import struct

import numpy as np
from PIL import Image

HEADER = struct.Struct("<4sIBBBBII16I16I")


def open_blp(data: bytes) -> Image.Image:
    """Decode a BLP file to an RGBA image."""
    try:
        image = _decode_fast(data)
    except (ValueError, struct.error, IndexError):
        image = None
    if image is None:
        with Image.open(io.BytesIO(data)) as im:
            image = im.convert("RGBA")
    return image


def _decode_fast(data: bytes) -> Image.Image | None:
    if len(data) < HEADER.size or data[:4] != b"BLP2":
        return None
    fields = HEADER.unpack_from(data)
    _, _, compression, alpha_depth, alpha_type, _, width, height = fields[:8]
    offset, size = fields[8], fields[24]
    mip = data[offset:offset + size]
    if compression == 2:  # DXT
        kind = 1 if alpha_depth <= 1 else (3 if alpha_type == 1 else 5)
        pixels = decode_dxt(mip, width, height, kind, alpha_depth > 0)
    elif compression == 3:  # raw BGRA
        bgra = np.frombuffer(mip, np.uint8, width * height * 4).reshape(height, width, 4)
        pixels = bgra[..., [2, 1, 0, 3]]
    else:
        return None
    return Image.fromarray(np.ascontiguousarray(pixels), "RGBA")


def _rgb565(c: np.ndarray) -> np.ndarray:
    r, g, b = (c >> 11) & 31, (c >> 5) & 63, c & 31
    return np.stack([(r << 3) | (r >> 2), (g << 2) | (g >> 4), (b << 3) | (b >> 2)], axis=-1).astype(np.int32)


def decode_dxt(data: bytes, width: int, height: int, kind: int, alpha1: bool = True) -> np.ndarray:
    """DXT1/3/5 blocks -> (height, width, 4) uint8 RGBA. `alpha1`: DXT1's fourth colour is transparent."""
    bw, bh = max(1, (width + 3) // 4), max(1, (height + 3) // 4)
    n = bw * bh
    block = 8 if kind == 1 else 16
    blocks = np.frombuffer(data, np.uint8, n * block).reshape(n, block)

    color = blocks[:, -8:].astype(np.uint32)
    c0 = color[:, 0] | color[:, 1] << 8
    c1 = color[:, 2] | color[:, 3] << 8
    bits = color[:, 4] | color[:, 5] << 8 | color[:, 6] << 16 | color[:, 7] << 24
    p0, p1 = _rgb565(c0), _rgb565(c1)
    four = (c0 > c1) if kind == 1 else np.ones(n, bool)
    p2 = np.where(four[:, None], (2 * p0 + p1) // 3, (p0 + p1) // 2)
    p3 = np.where(four[:, None], (p0 + 2 * p1) // 3, 0)
    palette = np.stack([p0, p1, p2, p3], axis=1)  # (n, 4, 3)
    index = (bits[:, None] >> (np.arange(16, dtype=np.uint32) * 2)) & 3  # (n, 16)
    rgb = np.take_along_axis(palette, index[..., None].astype(np.intp).repeat(3, axis=2), axis=1)

    if kind == 1:
        alpha = np.where(~four[:, None] & (index == 3) & alpha1, 0, 255)
    elif kind == 3:
        packed = blocks[:, :8].astype(np.int32)
        nibbles = np.stack([packed & 15, packed >> 4], axis=2).reshape(n, 16)
        alpha = nibbles * 17
    else:
        a0, a1 = blocks[:, 0].astype(np.int32), blocks[:, 1].astype(np.int32)
        steps = np.arange(1, 7)
        eight = ((7 - steps) * a0[:, None] + steps * a1[:, None]) // 7  # (n, 6)
        six = ((5 - steps[:4]) * a0[:, None] + steps[:4] * a1[:, None]) // 5  # (n, 4)
        six = np.concatenate([six, np.zeros((n, 1), np.int32), np.full((n, 1), 255, np.int32)], axis=1)
        table = np.concatenate([a0[:, None], a1[:, None], np.where((a0 > a1)[:, None], eight, six)], axis=1)
        packed = blocks[:, 2:8].astype(np.uint64)
        abits = np.zeros(n, np.uint64)
        for k in range(6):
            abits |= packed[:, k] << np.uint64(8 * k)
        aindex = (abits[:, None] >> (np.arange(16, dtype=np.uint64) * np.uint64(3))) & np.uint64(7)
        alpha = np.take_along_axis(table, aindex.astype(np.intp), axis=1)

    rgba = np.concatenate([rgb, alpha[..., None]], axis=2).astype(np.uint8)  # (n, 16, 4)
    image = rgba.reshape(bh, bw, 4, 4, 4).transpose(0, 2, 1, 3, 4).reshape(bh * 4, bw * 4, 4)
    return image[:height, :width]
