# Text-metric parity: ACOS `RenderContext` vs ART reference (T-33)

**Tier:** `art-reference (emulator, API 34, google_apis x86_64, 420 dpi, density 2.625)`.
Reference bounds are read from `fixtures/reference/<app>/art-api34-x86_64/ui_initial.xml`
at test time, so the numbers below are sourced, not hard-coded. The
`fixtures/reference-device/` (API 35, 400 dpi) captures are **not** used here:
a different density is only a sanity check, never pixel parity.

Measured by `cargo test -p acos-render --test text_metrics_parity`.

For a `wrap_content` TextView the View width equals the shaped advance; the
View height is `descent - ascent` plus the framework font padding, so the
height column is expected to be a bounded underestimate. `tight` is the
glyph-ink bounding box (`TextEngine::text_bounds`); M3 targets *exact text
bounds* and this column is the current state of that goal.

| app | label | text | size (sp) | ref bounds | ref W | ACOS advance | ΔW | ACOS ascent | descent | leading | W+H | ref H | tight (w×h) |
|---|---|---|---:|---|---:|---:|---:|---:|---:|---:|---:|---:|---|
| hello-views | title | `Hello ACOS` | 14 | [42,317][231,368] | 189 | 188.45 | 0.3% | -34.09 | 8.97 | 0.00 | 43.07 | 51 | 184.0×27.9 |
| hello-views | result | `clicks: 0` | 14 | [42,612][175,663] | 133 | 132.61 | 0.3% | -34.09 | 8.97 | 0.00 | 43.07 | 51 | 128.9×27.9 |
| counter | count | `0` | 14 | [42,317][63,368] | 21 | 20.65 | 1.6% | -34.09 | 8.97 | 0.00 | 43.07 | 51 | 16.5×26.8 |
| hello-views | app-title | `Hello Views` | 20 | [42,166][321,237] | 279 | 274.37 | 1.7% | -48.71 | 12.82 | 0.00 | 61.52 | 71 | 267.4×39.9 |
| counter | app-title | `Counter` | 20 | [42,166][231,237] | 189 | 184.85 | 2.2% | -48.71 | 12.82 | 0.00 | 61.52 | 71 | 181.0×38.3 |
| todo-list | app-title | `Todo List` | 20 | [42,166][262,237] | 220 | 216.61 | 1.5% | -48.71 | 12.82 | 0.00 | 61.52 | 71 | 213.5×39.9 |

## Reading the table

* At 14sp the advance matches the device to ~0.3% (`Hello ACOS` 188.45 vs 189 px,
`clicks: 0` 132.61 vs 133 px). This is the FreeType-mask + rustybuzz path of
ADR-0019 and the basis of the accepted per-glyph coverage error.
* The `W+H` column is `descent - ascent` from `Paint.FontMetrics`; the View H
is larger because Android adds font padding and, for `includeFontPadding`,
the `top`/`bottom` extent. The current `font_metrics_for` sets
`top = ascent` and `bottom = descent`, which is the known height gap.
* Subpixel positioning: advances stay fractional (linear metrics, Android's
default `subpixel positioning on`); A8 masks are placed on the rounded integer
pen (`(x).round() + bitmap_left`), matching the measured PoC in
`REPORT.md` §3.2. No advance rounding is applied.
* The 20sp app-title rows are the ActionBar title (inferred from the 71 px
reference height); the title is framework decor, not an ACOS `TextView`, so
it is reported as a size sanity check.

## Variable-font axes

`wght`/`wdth`/`ital` from `fonts.xml` plus `Paint.setFontVariationSettings` are
now applied to FreeType rasterisation with `FT_Set_Var_Design_Coordinates`, using
the same design-space values `rustybuzz` shapes with (`effective_variations`).
The A8 mask cache key includes the axis coordinates, and the shaping cache key
now includes the effective variations, so `wght=400` and `wght=700` never
collide. `cargo test -p acos-render` covers strictly-increasing coverage for
`wght` 100..900 and instance-cache correctness.

## Text-draw speed (T-33 regression check)

Axis support must not cost steady-state draw speed. Same host, 3 runs each,
`"Hello ACOS"` at 36.75px, 60 iterations:

| Path | before p50 | after p50 | delta |
|---|---:|---:|---:|
| FreeType A8 mask | 12.20 us | 12.31 us | +0.9% |
| `ttf-parser` outline + tiny-skia | 61.65 us | 66.20 us | +7.4% |

The mask path is the ADR-0019 text path and stays at ~12 us (**5.4x** the
outline path; ADR-0019 recorded 4.8x within host noise). Raw runs and method:
`../../results/render/text-draw-bench.md`.

## Known gaps (honest tier labels)

* **Fallback selection** is script-agnostic: `FontCollection::fallback_chain`
  puts the requested family first and then walks the `fonts.xml` order, and
  `font_for_char` takes the first family with a glyph. The `<family lang="...">`
  attribute is parsed into `Family::lang` but not used to order the chain by
  script/locale. The ART fixtures are all en-US Latin, so the golden values do
  not exercise fallback; CJK/Indic locale ordering is untested here.
* **Vertical metrics under variation**: `font_metrics_for` reads the default
  instance's `hhea`/`OS/2` values and does not re-read them after
  `FT_Set_Var_Design_Coordinates`. For Roboto's `ital`/`wdth`/`wght` axes the
  vertical metrics do not change, so the table above is unaffected; an `opsz`
  face would need instance-aware metrics.
* **Height**: `top`/`bottom` are set equal to `ascent`/`descent`, so the View
  height delta (13-16%) is the known font-padding gap, not a shaping error.
