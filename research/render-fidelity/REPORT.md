# Render fidelity feasibility: Skia/Minikin vs FreeType vs tiny-skia

**Author:** research/render-fidelity spike (subagent) · **Date:** 2026-11-10 ·
**Base commit:** `797208e` (`main`) · **Scope:** research/render-fidelity/**, .cache/spike.
**Question:** can ACOS replace the tiny-skia software renderer with version-pinned
Skia CPU rasterisation plus AOSP Minikin/HarfBuzz/ICU/FreeType matching Android 14,
to reach pixel/text parity with real Android for agent verification?

**Verdict (short):** The Skia part is *not* the win and is a poor use of the
budget. The win is **glyph rasterisation by FreeType with HarfBuzz
(`rustybuzz`) shaping**, keeping the existing Rust path/geometry rasteriser.
That stack reproduces the reference text pixels ~3.5× closer than the current
tiny-skia renderer and ~3.4× closer than Skia m157 CPU, matches total glyph ink
to 0.8–6.6%, and costs no large dependency. Separately, the current renderer has
a real bug: `Paint::default().anti_alias == false`, while Android's
`Paint()` defaults to `ANTI_ALIAS_FLAG` — the fixture text is rendered with no
antialiasing at all.

All numbers below were measured on this host; raw evidence commands are at the
end. Nothing was built from Skia source (would exceed the ADR-0018 download/RAM
budget); that option is analysed, not executed, and marked as such.

---

## 1. Reference identification (the oracle we compare against)

| Fact | Value | Source |
|---|---|---|
| Device | Android 14, API 34, x86_64 emulator, `google/sdk_gphone64_x86_64/emu64xa:14/UE1A.230829.050/12077443:userdebug/dev-keys` | `.cache/boot/android-34/build.prop` |
| Matching AOSP branch | `android-14.0.0_r1` (GA Android 14) | build id `UE1A.230829.050` |
| Skia milestone in that branch | **m112** (`#define SK_MILESTONE 112`); later QPR `android-14.0.0_r75` = m124 | `external/skia/include/core/SkMilestone.h` at the tag |
| AOSP FreeType (`external/freetype`) | **2.13.0** at r1 (QPR r75 = 2.13.2); host is 2.13.2 | `external/freetype/include/freetype/freetype.h` at the tag |
| AOSP HarfBuzz (`external/harfbuzz_ng`) | **6.0.0** | `src/hb-version.h` at the tag |
| AOSP ICU | **72.1** | `icu4c/source/common/unicode/uvernum.h` at the tag |
| Skia's own bundled FreeType | chromium mirror `82ce1726` = 2.12.1 (not what HWUI uses) | Skia `DEPS` |
| Display | 1080×2400 @ 420 dpi, density 2.625 | `display.txt` |
| Title | `Hello ACOS`, `android.widget.TextView`, bounds `[42,317][231,368]`, 14sp = **36.75 px** | `ui_initial.xml` |
| Font | `sans-serif` weight 400 → `Roboto-Regular.ttf` (variable: wght 100–900 default 400, wdth 75–100, ital 0–1); `unitsPerEm=2048` | `.cache/boot/android-34/fonts/fonts.xml`, font `fvar` table |
| Reference text colour | full-coverage pixels `(112,119,124)` over background `(240,240,243)`; the screenshot carries a small device colour cast (sRGB, no ICC profile) | pixel probe |
| Android `Paint` default | `public Paint() { this(ANTI_ALIAS_FLAG); }` → **anti-alias is ON by default** (S and above) | `frameworks/base/graphics/java/android/graphics/Paint.java` @ r1, line 586 |

The reference screenshot is 1080×2400 RGBA. Because the whole frame has a small
colour transform, all comparisons below are done on **alpha/coverage maps**, not
raw RGB: `coverage = (green_bg − green) / (green_bg − green_fg)` with `green_bg`
and `green_fg` auto-detected per region. This isolates glyph shape and coverage
from the device colour cast.

---

## 2. Method

Three independent renderers were built as throwaway PoCs (all under
`research/render-fidelity/poc/`, none in the Cargo workspace):

1. **skia-safe 0.157 (Skia m157), prebuilt binary** — `poc/skia-text/` (Rust).
   Renders `Hello ACOS` with `Typeface` loaded from the reference
   `Roboto-Regular.ttf`, sweeping `FontHinting {None,Slight,Normal,Full}` ×
   `subpixel {off,on}` × `Edging {AntiAlias, SubpixelAntiAlias}` ×
   `force_auto_hinting`, 32 configurations, white-on-transparent.
2. **Direct FreeType** — `poc/freetype_text.py` (ctypes via `freetype-py`,
   system FreeType 2.13.2). Raw A8 glyph masks, load targets
   `DEFAULT / NO_HINTING / TARGET_LIGHT`, integer and sub-pixel pen.
3. **HarfBuzz + FreeType** — `poc/hb_freetype_text.py`. Shape with **HarfBuzz**
   (`uharfbuzz`) at 26.6 scale, raster each glyph with FreeType
   `FT_LOAD_NO_HINTING | FT_LOAD_RENDER`, compose src-over. This is the
   Minikin-equivalent pipeline without Skia.

Scoring (`research/render-fidelity/diff_text.py`): place the candidate into the
reference frame by ink bbox, refine with an integer shift search, then report
mean |Δcoverage|, RMSE, per-pixel coverage-difference thresholds, total ink
ratio, and a per-glyph analysis that finds each glyph's optimal sub-pixel shift
(separating **positioning** error from **rasterisation** error).

Baseline for `Hello ACOS` in the reference (box `[30,300][300,390]`):
ink bbox `(15,29,200,58)`, total coverage ink `1526.0`.

---

## 3. Measured results

### 3.1 Whole-string coverage error (title region)

| Stack | mean \|Δcov\| | RMSE | max | >0.10 | >0.20 | ink ratio | best γ |
|---|---|---|---|---|---|---|---|
| tiny-skia, scene default (`antiAlias:false`) | 0.01536 | 0.09395 | 1.000 | 3.49% | 2.45% | 0.975 | (none helps) |
| tiny-skia, scene patched `antiAlias:true` | 0.01248 | 0.06914 | 0.868 | 3.67% | 2.57% | 0.981 | – |
| FreeType `NO_HINTING` (FT advances) | 0.00842 | 0.06291 | 0.950 | 2.29% | 0.95% | 0.992 | 1.00 |
| HarfBuzz + FreeType `NO_HINTING` | 0.00997 | 0.07050 | 1.000 | 2.53% | 1.35% | 0.992 | 0.96 |
| Skia m157, hinting None, grayscale AA | 0.00817 | 0.04602 | 0.475 | 3.12% | 1.99% | **1.125** | 1.80 |
| Skia m157 + fitted coverage γ≈1.8 | **0.00366** | **0.02149** | – | – | – | 1.028 | – |

### 3.2 Per-glyph rasterisation error (positioning removed)

Each glyph was aligned independently with sub-pixel translation, so this table
is **rasterisation fidelity only**. `maxDrift` is the largest per-glyph optimal
shift across the 9 glyphs (positioning/advance error).

| Stack | per-glyph mean \|Δcov\| | per-glyph RMSE | ink ratio | max drift |
|---|---|---|---|---|
| tiny-skia (`antiAlias:false`) | 0.05823 | 0.13165 | 0.975 | 0.75 px |
| tiny-skia (`antiAlias:true`) | 0.04577 | 0.08216 | 0.981 | 0.75 px |
| **FreeType `NO_HINTING`** | **0.01630** | **0.04151** | **0.992** | 1.00 px |
| **HarfBuzz + FreeType `NO_HINTING`** | **0.01630** | **0.04151** | **0.992** | 1.00 px |
| Skia m157, hinting None, grayscale AA | 0.05492 | 0.11678 | 1.125 | 0.00 px |

Skia aligns every glyph perfectly (0 drift) but its mask carries **12.5 % more
ink** than the device; after a fitted coverage gamma it drops to 0.0037 mean but
that gamma is a *fit*, not a derived constant. Direct FreeType matches the
device ink without any fudge but its naive advances drift up to 1 px.

### 3.3 Generalisation to a second string (`clicks: 0`, same 14sp)

| Stack | mean \|Δcov\| | RMSE | max | >0.10 | >0.20 |
|---|---|---|---|---|---|
| HarfBuzz + FreeType `NO_HINTING` | **0.00495** | **0.01982** | 0.254 | 0.77% | 0.05% |
| Skia m157 (default) | 0.00935 | 0.04284 | 0.491 | 2.44% | 1.73% |
| FreeType `NO_HINTING` (naive advances) | 0.01240 | 0.07296 | 0.988 | 2.06% | 1.39% |

**HarfBuzz + FreeType wins decisively here (RMSE 0.020 vs 0.043 Skia).** The
`Hello ACOS` anomaly (naive FT beating HB-FT) is an advance/rounding artefact
for that glyph run; across both strings the HB-FT stack is the most consistent,
and it reproduces Android without a fitted parameter. `>0.10 = 0.77 %` and
`>0.20 = 0.05 %` of pixels is the closest to pixel parity measured in this
spike.

### 3.4 What the current renderer is actually doing wrong

* Probe row through the `H` stem: reference green `[…,125,119,119,175,…]` (5
  coverage levels); current acos-render `[…,117,117,117,245,…]`. The title
  region of the current render contains **2 unique grey levels (117, 245)**. The
  scene was rendered with **no antialiasing** because
  `crates/acos-render/src/paint.rs::Paint::default()` sets `anti_alias: false`
  and the scenario JSON does not override it. Android's `Paint()` sets
  `ANTI_ALIAS_FLAG`. This alone accounts for ~19 % of the coverage error
  (0.01536 → 0.01248 mean).
* Even after enabling AA, tiny-skia's glyph-as-path fill is 2.8× worse per glyph
  than FreeType (0.0458 vs 0.0163) and its per-glyph advances accumulate up to a
  0.75 px horizontal drift; shape outlines are also subtly different because
  tiny-skia fills quadratic outlines analytically rather than using the hinting
  / rasterisation grid Android uses.

---

## 4. Options compared

### Option A — `skia-safe` with prebuilt binaries (exercised)

| Item | Measured |
|---|---|
| Crate / Skia milestone | `skia-safe 0.157` → **Skia m157** (`skia_safe::MILESTONE == 157`) |
| Android 14 mapping | GA `android-14.0.0_r1` = **m112**; QPR `android-14.0.0_r75` = m124. **No skia-safe release maps to m112.** Nearest: 0.62 → m113, 0.63 → m114, 0.73 → m124 |
| Prebuilt download | `skia-binaries-…-x86_64-unknown-linux-gnu-jpegd-jpege-pdf.tar.gz` = **4.31 MB** (compressed), `libskia.a` = **14.8 MB** extracted |
| Clean build (cached prebuilt) | debug **10.5 s**, release **6.1 s**; peak build RSS **~0.48 GiB** |
| Linked libraries | system **FreeType** (60 undefined `FT_*`) and **fontconfig** (undefined `Fc*`); **no HarfBuzz, no SkParagraph/textlayout** (0 `hb_*`, 0 paragraph symbols) |
| Release binary (PoC) | **3.92 MB** |
| Runtime | process RSS **24.0 MiB** for load-font + 32 renders; 32 renders + PNG writes in **0.40 s** wall (~12 ms/render) |
| Licence | Skia **BSD-3-Clause** (`LICENSE_SKIA`), wrapper **MIT** |
| Text parity | per-glyph **0.0549** (worse than FreeType 0.0163); ink ratio **1.125**; fixes only with a fitted γ |

**Can we pin m112?** Not from the prebuilt cache: it is built per *release*, and
no release corresponds to m112. Pinning requires building Skia from source at a
chosen commit — which also means shipping our own Skia build and losing the
prebuilt convenience. Android's own `external/skia` at m112 is not what skia-safe
builds (skia-safe tracks `chrome/m*`, Android tracks its own branch).

### Option B — build AOSP `external/skia` + `frameworks/minikin` as host libs (analysed, not executed)

* **Source size.** `google/skia` git objects ≈ **1.13 GB** (GitHub mirror); a
  shallow working tree is a few hundred MB. `third_party/externals` fetched by
  `tools/git-sync-deps` adds HarfBuzz (118 MB git), FreeType (32 MB), expat
  (18 MB), zlib/libpng/libjpeg-turbo and **ICU** (multi-GB upstream). This
  alone would exceed the ADR-0018 "no downloads > 1 GB without asking" rule.
* **Build system reality.** `frameworks/minikin` @ r1 contains only
  **`Android.bp` (Soong)** — there is no `CMakeLists.txt` and no `BUILD.gn`
  (verified from the gitiles tree listing). Minikin also depends on Android
  internals (`libbase`, `liblog`, `libutils`, `libui`, `libcutils`) plus Skia,
  HarfBuzz, ICU and FreeType. Building it on the host means porting `Android.bp`
  to CMake/GN and stubbing the Android logging/base libs.
* `external/skia` builds with **GN + ninja** (`bin/gn gen`), requires `gn`
  (not installed) and a full `git-sync-deps`; the Android `.bp` variant is not a
  supported host build.
* **Cost estimate.** A CPU-only GN Skia build is typically 5–20 min on 8–32
  cores and several GiB of RAM; Minikin adds porting days. Exact m112 pinning
  is theoretically possible, but the effort is a multi-day project and does not
  buy text fidelity that direct FreeType already provides.
* **Licences.** Skia BSD-3-Clause, Minikin Apache-2.0, HarfBuzz MIT, ICU
  Unicode-3.0, FreeType FTL/GPLv2 (FTL chosen) — all compatible with ACOS's
  Apache-2.0.

### Option C — keep the Rust rasteriser, replace only glyph rasterisation (recommended)

* Add a **FreeType** backend for glyph masks (bundle a pinned FreeType, ideally
  AOSP's 2.12.1 commit) + keep **rustybuzz** for shaping (already a dependency),
  and blit A8 masks with src-over.
* Measured text parity: **best of all candidates** (per-glyph 0.0163; `clicks: 0`
  RMSE 0.0198, >0.20 coverage diff on 0.05 % of pixels).
* No Skia. Dependency is `freetype-rs`/`freetype-sys` (system link or `bundled`)
  plus `libc`/`cc` at build time. The mask raster is tiny and cacheable
  (glyph atlas keyed by `(font_id, glyph_id, size, subpixel-x bucket)`).
* Fix the two concrete bugs found: `Paint::default().anti_alias = true`, and
  align advances/layout with Minikin metrics (the 1 px drift in §3.2).

---

## 5. Licence / size / build / runtime summary

| Dimension | Skia m157 prebuilt | AOSP m112 from source (est.) | FreeType path |
|---|---|---|---|
| Licence | BSD-3-Clause (Skia) + MIT (wrapper) | BSD-3-Clause + Apache-2.0 + MIT + Unicode-3.0 + FTL | **FTL** (FreeType) + MIT (HarfBuzz) |
| Extra download | 4.3 MB prebuilt | ≥ 1 GB (estimate) | 0 (system link) or ~2 MB (bundled crate) |
| Build time | 6–11 s | 5–20 min + Minikin port | seconds (system) / ~1 min (bundled) |
| Build peak RSS | ~0.48 GiB | multi-GiB | < 0.2 GiB |
| Extra binary size | ~3.9 MB PoC (Skia static) | large | ~0.5–1 MB (FreeType) |
| Runtime RSS | 24 MiB (font + 32 renders) | — | comparable / lower |
| Text parity | worse than FreeType, needs fitted γ | unknown (would be exact if port succeeds) | **best measured** |

`gn` is not installed on the host; `cmake`, `ninja`, `clang`, `gcc`,
`pkg-config` are. FreeType 2.13.2, HarfBuzz 14.6 (libtool), ICU 78, fontconfig
2.18 are present as system libraries.

---

## 6. Recommendation and migration plan

**Recommendation: do not adopt Skia. Implement option C — FreeType glyph
rasterisation behind the existing `acos-render` API — and fix the AA default.**
Re-open the Skia/AOSP question only if a full-canvas construct (colour spaces,
advanced blend modes, image filters, `SkParagraph` line breaking, GPU paths)
becomes a measured blocker, which the current evidence does not show.

Rationale, tied to the measured numbers:

1. Direct FreeType matches the device glyph mask ~3.4× closer than Skia m157
   (per-glyph 0.0163 vs 0.0549) and matches ink without any fitted parameter;
   Skia needs a coverage gamma of ~1.8 that is not a measured AOSP constant.
   Host FreeType 2.13.2 already matches Android 14's `external/freetype`
   (2.13.0 at r1, 2.13.2 at QPR), so the rasteriser version is aligned too.
2. Both candidate stacks need HarfBuzz for advances/kerning (rustybuzz is
   already in `acos-render`), so Skia adds little on the text path.
3. Milestone pinning to m112 is not available in skia-safe and the AOSP
   source build is a multi-day, >1 GB effort against ADR-0018.
4. Option C is the smallest correct change and leaves the Rust-owned renderer
   architecture (ADR-0008/0011) intact: only the glyph rasteriser changes.

### 6.1 Migration plan

| Step | Change | Files |
|---|---|---|
| 1 | Fix `Paint::default().anti_alias = true` (matches `android.graphics.Paint()`), add a regression test that the fixture title has >2 grey levels | `crates/acos-render/src/paint.rs` |
| 2 | Add glyph-mask backend: `freetype-rs`/`freetype-sys`; bundle/pin FreeType **2.13.0** (AOSP `external/freetype` @ r1) and HarfBuzz **6.0.0**/rustybuzz equivalent; cache A8 masks keyed by `(font_id, glyph_id, size, x-subsample)` | new `crates/acos-render/src/text/freetype.rs`, `crates/acos-render/Cargo.toml` |
| 3 | Route `canvas_draw_text` through `rustybuzz` advances + FreeType masks instead of `text_path` + `draw_path_with_paint`; keep the path fallback for fonts FreeType cannot load | `crates/acos-render/src/context.rs`, `crates/acos-render/src/text/mod.rs` |
| 4 | Add the same `diff_text.py` coverage metric as a golden test over the five reference strings, with the measured thresholds (per-glyph RMSE, ink ratio, >0.2 %), not a scene JSON | `crates/acos-render/tests/` |
| 5 | Wire the framework natives (T-31 `gfx/canvas.rs`, `gfx/paint.rs`) — no native change needed; they keep calling `RenderContext::canvas_draw_text` | `crates/acos-framework-natives/src/gfx/` |
| 6 | (Optional, only if a construct is measured wrong) evaluate Skia/Pango for line breaking; until then Minikin line breaking is tracked in T-33 | — |

### 6.2 Exact crates / APIs

* **Shaping:** `rustybuzz` (already used by `acos-render/src/text/mod.rs`) —
  `ShapeContext`, `Face::from_slice`, `UnicodeBuffer`, `shape()` → per-glyph
  `glyph_id` + `x_advance`/`x_offset` in 26.6. No new crate.
* **Rasterisation:** `freetype-rs` 0.38 (`freetype_sys` 0.23). Feature
  `bundled` builds a pinned FreeType from source; without it,
  `pkg-config freetype2` links the system library. Preferred: bundle the AOSP
  `external/freetype` 2.13.0 source to match the oracle exactly (host 2.13.2 is
  one patch ahead and measured a good match).
  API used by the PoC: `FT_Init_FreeType`, `FT_New_Memory_Face`,
  `FT_Set_Char_Size(0, 36.75*64)`, `FT_Load_Glyph(gid,
  FT_LOAD_NO_HINTING | FT_LOAD_RENDER)`, read `glyph.bitmap`
  (`width/rows/pitch/buffer`, `pixel_mode == GRAY`), `bitmap_left`,
  `bitmap_top`, `advance.x` (26.6) / `linearHoriAdvance` (16.16).
  To match Android 14 exactly, use `FT_LOAD_NO_HINTING` + grayscale
  (`FT_LOAD_TARGET_NORMAL`); `TARGET_LIGHT` and hinting-on were both measured
  worse.
* **Compositing:** the existing `canvas` blitter. Add
  `canvas::blit_coverage(target, clip, x, y, width, height, &[u8], &tiny_skia::Paint, transform)`.
* **Coverage gamma:** none required with raw FreeType masks (measured best
  γ≈1.0 for `Hello ACOS`, ink ratio 0.99). Do **not** copy Skia's default mask
  gamma — it is the source of Skia's +12.5 % ink.

### 6.3 What this does *not* prove

* Only two short Latin strings on one 14sp instance were scored. Long text,
  bidi, fallback chains, variable-font instances, and non-Latin scripts need the
  same treatment (the advisor's "don't overfit one trace" warning stands).
* The device screenshot has a small colour cast; parity was measured on
  coverage, so raw RGB equality also requires matching Android's display
  transform (out of scope here).
* AOSP `external/skia` + Minikin was not built; its cost estimates are from
  repository sizes and build-system inspection, not a local build.
* The `>1 px` per-glyph advance drift with naive FT advances is not yet fixed;
  HB-FT removes most of it but Minikin's exact advance/layout rules remain the
  T-33 work item.

---

## 7. Reproduction

```bash
# 1. current renderer + AA fix
tools/guard/guarded.sh --mem 4G --time 120 -- ./target/debug/examples/render_scene \
  crates/acos-render/tests/scenes/hello-views.json .cache/spike/current_hello_views.png --json

# 2. Skia m157 sweep (prebuilt)
tools/guard/guarded.sh --mem 8G --time 900 -- bash -c \
  'cd research/render-fidelity/poc/skia-text && cargo run --release -- \
   /home/Qui/Projects/acos/.cache/boot/android-34/fonts/Roboto-Regular.ttf \
   /home/Qui/Projects/acos/.cache/spike/skia_out "Hello ACOS"'

# 3. HarfBuzz + FreeType sweep
.cache/spike/ftenv/bin/python research/render-fidelity/poc/hb_freetype_text.py \
  .cache/boot/android-34/fonts/Roboto-Regular.ttf .cache/spike/hbft_out "Hello ACOS"

# 4. score against the device screenshot
python3 research/render-fidelity/diff_text.py --gamma \
  .cache/spike/current_hello_views.png .cache/spike/hbft_out/*.png
```

Evidence files kept (small): `research/render-fidelity/coverage_panels.png`,
`title_panels.png`. All scratch (Skia build targets, ~0.9 GB) was removed after
the spike.
