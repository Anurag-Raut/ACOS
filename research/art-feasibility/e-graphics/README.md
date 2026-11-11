# e-graphics — the drawing seam under real ART (E2)

**Report: [`results/art-feasibility/E2E.md`](../../../results/art-feasibility/E2E.md).**

Short version: with real ART + the real `framework.jar` + real `libhwui.so`
(Skia is statically linked into it; there is no `libskia.so` in the image), a
Bitmap-backed `Canvas` already draws correct pixels on this host — 0.30 µs per
`drawRect`, 163 MB peak RSS, no boot, no GPU.  The seam is
`Bitmap.createBitmap` → `Canvas` → `getPixels` (the framework's own software
draw path), plus two environment facts:

1. `libhwui.so` exports `register_android_graphics_classes(JNIEnv*)`, and
   `libandroid_runtime.so` exports 131 `register_android_*` — call them; do not
   hand-register extracted tables (the native bodies need the class-global refs
   those functions cache).
2. A `/data`-loaded bridge lives in bionic namespace `clns-1`
   (`permitted_paths=/data`) and cannot `dlopen` `/system/lib64/*` unless the
   library is listed in `/system/etc/public.libraries.txt`.

## Run it

```bash
scripts/build-run.sh                 # NDK clang + javac/d8 + apply namespace fix + run under tools/guarded.sh
scripts/public-libs.sh --restore     # undo the (scratch-sysroot) namespace fix
```

## Contents

| path | purpose |
|---|---|
| `scripts/jninatives.py` | extract `JNINativeMethod` tables from a symbol-less Android `.so` (unpacks `SHT_ANDROID_RELA`/APS2) |
| `scripts/attrib.py` | attribute a table to its Java class via the RIP-relative `lea` in `.text` |
| `natives_full.json` | 888 natives recovered from `libhwui.so` |
| `jninatives.json` | all 288 tables: `libhwui.so` (848) + `libandroid_runtime.so` (2358) |
| `tables-libhwui.so.json` | per-table grouping + class guesses |
| `e2/bridge.c`, `e2/natives.h`, `e2/classes.h` | the JNI bridge (real registration entry points + table re-registration fallback) |
| `e2/java/dev/acos/e2/` | the probe (`Main`, `Bridge`) |
| `e2/evidence-run.log` | the successful run's log |

Scratch (gitignored): `.cache/art-exp-e/`.
