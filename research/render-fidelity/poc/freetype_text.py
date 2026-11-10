#!/usr/bin/env python3
"""FreeType-only text PoC: rasterise "Hello ACOS" exactly as Android's FreeType
backend would, without any Skia canvas.

Sweeps FT load targets and subpixel positioning, writes one RGBA PNG per config
(white glyphs, straight alpha = coverage on transparent), so it can be scored by
../diff_text.py just like the Skia PoC.

Usage: freetype_text.py <font.ttf> <out_dir> [text]
"""
import os
import sys
import numpy as np
import freetype
from PIL import Image

W, H = 480, 140
BASELINE = 100.0
SIZE_PX = 36.75


def render(face, text, flags, subpixel):
    cov = np.zeros((H, W), dtype=np.float64)
    pen = 0.0
    for ch in text:
        identity = freetype.Matrix(0x10000, 0, 0, 0x10000)
        if subpixel:
            delta = freetype.Vector(int(round((pen - int(pen)) * 64)), 0)
            face.set_transform(identity, delta)
        else:
            face.set_transform(identity, freetype.Vector(0, 0))
        gid = face.get_char_index(ord(ch))
        face.load_glyph(gid, flags | freetype.FT_LOAD_RENDER)
        slot = face.glyph
        bm = slot.bitmap
        pitch = bm.pitch
        rows = bm.rows
        width = bm.width
        raw = bytes(bm.buffer)
        arr = np.frombuffer(raw, dtype=np.uint8)
        arr = arr.reshape(rows, pitch)[:, :width]
        x0 = int(np.floor(pen))
        y0 = int(round(BASELINE)) - slot.bitmap_top
        for y in range(rows):
            ty = y0 + y
            if 0 <= ty < H:
                for x in range(width):
                    tx = x0 + slot.bitmap_left + x
                    if 0 <= tx < W:
                        v = arr[y, x] / 255.0
                        if v > cov[ty, tx]:
                            cov[ty, tx] = v
        pen += (slot.linearHoriAdvance / 65536.0) if subpixel else (slot.advance.x / 64.0)
    return cov


def save(cov, path):
    a = np.clip(cov * 255.0, 0, 255).astype(np.uint8)
    rgba = np.zeros((H, W, 4), dtype=np.uint8)
    rgba[:, :, 0] = a
    rgba[:, :, 1] = a
    rgba[:, :, 2] = a
    rgba[:, :, 3] = a
    Image.fromarray(rgba, "RGBA").save(path)


def main():
    font_path = sys.argv[1]
    out_dir = sys.argv[2]
    text = sys.argv[3] if len(sys.argv) > 3 else "Hello ACOS"
    os.makedirs(out_dir, exist_ok=True)
    configs = [
        ("default", freetype.FT_LOAD_DEFAULT, False),
        ("nohint", freetype.FT_LOAD_NO_HINTING, False),
        ("light", freetype.FT_LOAD_TARGET_LIGHT, False),
        ("nohint_sp", freetype.FT_LOAD_NO_HINTING, True),
        ("light_sp", freetype.FT_LOAD_TARGET_LIGHT, True),
        ("default_sp", freetype.FT_LOAD_DEFAULT, True),
    ]
    for name, flags, sp in configs:
        face = freetype.Face(font_path)
        face.set_char_size(int(round(SIZE_PX * 64)))
        cov = render(face, text, flags, sp)
        save(cov, f"{out_dir}/{name}.png")
        print(f"{name:10s} ink={cov.sum():.1f} max={cov.max():.3f}")


if __name__ == "__main__":
    main()
