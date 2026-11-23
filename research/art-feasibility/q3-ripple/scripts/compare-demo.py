#!/usr/bin/env python3
"""compare-demo.py -- SSIM/MAE of the Q3 pressed+focused demo frame against the
ART oracle, using compare.py's own helpers.

There is no oracle *pressed/focused* screenshot (the fixture captures
screenshot_initial + post-tap screenshots, all unpressed), so this compares the
demo frame with the oracle's initial frame. That is the right comparison on
Android 14: at t=0 a ripple has radius 0 and opacity 0, so a pressed/focused
frame and an unpressed frame are pixel-identical (measured: identical md5
237e5fd1... on both).
"""
import importlib.util
import pathlib
import sys

HERE = pathlib.Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location("cmp", HERE / "compare.py")
cmp = importlib.util.module_from_spec(spec)
spec.loader.exec_module(cmp)

demo = HERE.parent / "artifacts" / "q3-demo-pressed-focused.png"
ours = cmp.crop(demo, already_content=True)
o = cmp.crop(cmp.ORACLE_PNG)
import numpy as np

a = np.asarray(ours.convert("RGB"), dtype=np.int16)
b = np.asarray(o.convert("RGB"), dtype=np.int16)
exact = float(np.all(a == b, axis=2).mean())
print("demo png      %s  content %dx%d" % (demo.name, ours.width, ours.height))
print("art oracle    %s  %dx%d" % (cmp.ORACLE_PNG.relative_to(cmp.ROOT), o.width, o.height))
print("SSIM=%.5f  MAE=%.3f/255  identical_pixels=%.2f%%"
      % (cmp.ssim(ours, o), float(np.abs(a - b).mean()), 100 * exact))
nz = np.argwhere(np.any(a != b, axis=2))
if len(nz):
    print("differing rows: y=%d..%d (of 0..%d), n=%d" % (nz[:, 0].min(), nz[:, 0].max(), a.shape[0], len(nz)))
