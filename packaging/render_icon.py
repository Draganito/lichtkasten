#!/usr/bin/env python3
"""Rasterize the Lichtkasten mark to PNG without extra packages."""

from __future__ import annotations

import struct
import zlib
from pathlib import Path

SIZE = 256
SCALE = SIZE / 128.0

# Same geometry as packaging/lichtkasten.svg (viewBox 128).
LAYERS = [
    (0, 0, 128, 128, 28, (0x12, 0x0F, 0x0A, 255)),
    (12, 12, 104, 104, 20, (0x2A, 0x27, 0x22, 255)),
    (20, 20, 88, 88, 14, (0x1A, 0x17, 0x12, 255)),
    (26, 26, 76, 76, 8, (0x3D, 0x34, 0x20, 255)),
    (30, 30, 68, 68, 6, (0xE8, 0xD5, 0xA3, 255)),
    (30, 30, 68, 24, 6, (0xF6, 0xED, 0xD2, 255)),
    (30, 78, 68, 20, 6, (0xC9, 0xB2, 0x7A, 115)),
]


def rounded_cover(px: float, py: float, x: float, y: float, w: float, h: float, r: float) -> float:
    r = min(r, w * 0.5, h * 0.5)
    cx = min(max(px, x + r), x + w - r)
    cy = min(max(py, y + r), y + h - r)
    if x + r <= px <= x + w - r or y + r <= py <= y + h - r:
        inside = x <= px <= x + w and y <= py <= y + h
        return 1.0 if inside else 0.0
    dx = px - cx
    dy = py - cy
    dist = (dx * dx + dy * dy) ** 0.5
    return max(0.0, min(1.0, r + 0.55 - dist))


def blend(dst: list[int], src: tuple[int, int, int, int], cover: float) -> None:
    if cover <= 0:
        return
    sr, sg, sb, sa = src
    a = (sa / 255.0) * cover
    if a >= 1:
        dst[0], dst[1], dst[2], dst[3] = sr, sg, sb, 255
        return
    ia = 1.0 - a
    dst[0] = round(sr * a + dst[0] * ia)
    dst[1] = round(sg * a + dst[1] * ia)
    dst[2] = round(sb * a + dst[2] * ia)
    dst[3] = round(255 * a + dst[3] * ia)


def raster() -> bytes:
    pixels = bytearray(SIZE * SIZE * 4)
    for y in range(SIZE):
        py = (y + 0.5) / SCALE
        for x in range(SIZE):
            px = (x + 0.5) / SCALE
            pix = [0, 0, 0, 0]
            for rx, ry, rw, rh, rr, color in LAYERS:
                blend(pix, color, rounded_cover(px, py, rx, ry, rw, rh, rr))
            i = (y * SIZE + x) * 4
            pixels[i : i + 4] = bytes(pix)
    return bytes(pixels)


def write_png(path: Path, rgba: bytes) -> None:
    def chunk(tag: bytes, data: bytes) -> bytes:
        raw = tag + data
        return struct.pack(">I", len(data)) + raw + struct.pack(">I", zlib.crc32(raw) & 0xFFFFFFFF)

    rows = b"".join(b"\x00" + rgba[y * SIZE * 4 : (y + 1) * SIZE * 4] for y in range(SIZE))
    ihdr = struct.pack(">IIBBBBB", SIZE, SIZE, 8, 6, 0, 0, 0)
    path.write_bytes(
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", ihdr)
        + chunk(b"IDAT", zlib.compress(rows, 9))
        + chunk(b"IEND", b"")
    )


if __name__ == "__main__":
    out = Path(__file__).with_name("lichtkasten.png")
    write_png(out, raster())
    print(out)
