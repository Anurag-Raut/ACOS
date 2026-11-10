# ADR-0019 text-draw benchmark (FreeType mask path vs outline path)

Host: AMD Ryzen 9 9950X (16C/32T), Linux, **single-threaded**, `--release`.
String `"Hello ACOS"` at 14sp = 36.75px, `canvas_draw_text` on a 512×128
surface, **60 iterations**, p50/p95. After the first (cold) draw the shaping
cache and the FreeType glyph-mask cache are warm, so the steady-state number is
the A8 mask blit + cached shape lookup.

Both paths are run from the *same* release binary: the outline path is selected
with `ACOS_RENDER_FORCE_TEXT_PATH=1` (a diagnostic switch added for this
comparison). Both have `Paint::default().anti_alias == true`, i.e. this is the
honest before/after of the text *rasterisation* change, not of the AA default
(which is a separate, strictly-correctness fix). p50 varies by roughly ±5%
between runs on this host.

| Path | p50 | p95 | min | max | cold (first draw) |
|---|---:|---:|---:|---:|---:|
| **FreeType A8 mask** (`FT_LOAD_NO_HINTING\|FT_LOAD_RENDER`) | **12.61 µs** | 12.84 µs | 12.57 µs | 18.16 µs | 200 µs |
| `ttf-parser` outline + tiny-skia fill (old) | 60.35 µs | 72.35 µs | 56.48 µs | 75.57 µs | 188 µs |

**Mask path is ~4.8× faster** for a steady-state 9-glyph draw (and matches the
device ~3.5× closer, see `text-coverage.txt`).

## RSS impact

| Point | FreeType mask | Outline path |
|---|---:|---:|
| before fonts | 3 332 KB | 3 300 KB |
| after fonts + context | 30 256 KB | 29 764 KB |
| after cold draw | 31 616 KB | 30 900 KB |
| after 60 warm draws | 31 616 KB | 30 900 KB |

The ~26 MB step is the process-wide `mmap` of the Android font set (unchanged,
shared by every context). The glyph-mask cache and FreeType faces add
**~0.7 MB** over the outline path for this string; the cache is capped at 4096
masks (`MASK_CACHE_CAPACITY`) and dropped wholesale when full, so it is bounded
per process. `bench_render`'s heavy scene (200 text rows) dropped from
**24.0 ms → 11.9 ms** warm (p50) with the cached masks.

Raw data: `text-draw-mask.json`, `text-draw-path.json`.

## Build

The dependency is `freetype-sys` with its `bundled` feature: it compiles the
pinned **FreeType 2.13.2** C sources shipped with the crate (AOSP QPR
`android-14.0.0_r75` is 2.13.2; GA `android-14.0.0_r1` is 2.13.0). No host
`libfreetype` is required. If the vendored C cannot compile, the build fails in
`freetype-sys`'s `build.rs`. Remove the `bundled` feature to link the host
library via `pkg-config` (documented in `crates/acos-render/Cargo.toml`).
