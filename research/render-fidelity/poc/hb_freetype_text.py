#!/usr/bin/env python3
"""HarfBuzz-shaped + FreeType-rasterised text PoC (the Minikin-equivalent
pipeline: shape with HarfBuzz, raster each glyph with FreeType, compose with a
src-over coverage blend).

This is the candidate for "keep tiny-skia for geometry, replace only glyph
rasterisation" - it needs no Skia at all.

Usage: hb_freetype_text.py <font.ttf> <out_dir> [text]
"""
import os
import sys
import numpy as np
import freetype
import uharfbuzz as hb
from PIL import Image

W, H = 480, 140
BASELINE = 100.0
SIZE_PX = 36.75
SCALE = int(round(SIZE_PX * 64))  # 26.6


def shape(font_path, text):
    with open(font_path, "rb") as f:
        data = f.read()
    face = hb.Face(data)
    font = hb.Font(face)
    upem = face.upem
    font.scale = (SCALE, SCALE)
    buf = hb.Buffer()
    buf.add_str(text)
    buf.guess_segment_properties()
    hb.shape(font, buf)
    return list(zip(buf.glyph_infos, buf.glyph_positions)), upem


def render(font_path, text, flags, subpixel):
    infos, _ = shape(font_path, text)
    ftface = freetype.Face(font_path)
    ftface.set_char_size(SCALE)
    cov = np.zeros((H, W), dtype=np.float64)
    pen = 0.0
    identity = freetype.Matrix(0x10000, 0, 0, 0x10000)
    for info, pos in infos:
        x = pen + pos.x_offset / 64.0
        y = BASELINE - pos.y_offset / 64.0
        if subpixel:
            frac = x - np.floor(x)
            ftface.set_transform(identity, freetype.Vector(int(round(frac * 64)), 0))
            base_x = int(np.floor(x))
        else:
            ftface.set_transform(identity, freetype.Vector(0, 0))
            base_x = int(round(x))
        ftface.load_glyph(info.codepoint, flags | freetype.FT_LOAD_RENDER)
        slot = ftface.glyph
        bm = slot.bitmap
        arr = np.frombuffer(bytes(bm.buffer), dtype=np.uint8).reshape(bm.rows, bm.pitch)[:, :bm.width]
        x0 = base_x + slot.bitmap_left
        y0 = int(round(y)) - slot.bitmap_top
        for yy in range(bm.rows):
            ty = y0 + yy
            if 0 <= ty < H:
                for xx in range(bm.width):
                    tx = x0 + xx
                    if 0 <= tx < W:
                        v = arr[yy, xx] / 255.0
                        if v > cov[ty, tx]:
                            cov[ty, tx] = v
        pen += pos.x_advance / 64.0
    return cov


def save(cov, path):
    a = np.clip(cov * 255.0, 0, 255).astype(np.uint8)
    rgba = np.zeros((H, W, 4), dtype=np.uint8)
    for c in range(3):
        rgba[:, :, c] = a
    rgba[:, :, 3] = a
    Image.fromarray(rgba, "RGBA").save(path)


def main():
    font_path, out_dir = sys.argv[1], sys.argv[2]
    text = sys.argv[3] if len(sys.argv) > 3 else "Hello ACOS"
    os.makedirs(out_dir, exist_ok=True)
    configs = [
        ("hb_nohint", freetype.FT_LOAD_NO_HINTING, False),
        ("hb_nohint_sp", freetype.FT_LOAD_NO_HINTING, True),
        ("hb_light", freetype.FT_LOAD_TARGET_LIGHT, True),
        ("hb_default_sp", freetype.FT_LOAD_DEFAULT, True),
    ]
    for name, flags, sp in configs:
        cov = render(font_path, text, flags, sp)
        save(cov, f"{out_dir}/{name}.png")
        print(f"{name:14s} ink={cov.sum():.1f}")


if __name__ == "__main__":
    main()
