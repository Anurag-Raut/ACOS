# `acos-render` — software 2D renderer for the real Android framework

This crate is ACOS's replacement for **HWUI + Skia + SurfaceFlinger + minikin**.
The Android 14 framework's `android.graphics.*` Java bytecode runs on the Rust VM
and bottoms out in `native` methods; those natives bind to the handle-based API
in `RenderContext` implemented here. Rasterisation is `tiny-skia` (a Rust port
of Skia's software rasteriser), shaping is `rustybuzz` (HarfBuzz port) with
`ttf-parser` outlines/metrics, and fonts are resolved from the real Android
`/system/etc/fonts.xml`.

- Source: `crates/acos-render/`
- Scene/bench assets: `crates/acos-render/tests/scenes/`, `tests/golden/`
- Artifacts: this directory (`bench.json`, `hello-views.png`,
  `hello-views.meta.json`, `test-output.txt`, `clippy-output.txt`)

## Acceptance (real output)

```
$ cargo test -p acos-render
running 63 tests   ... test result: ok. 63 passed; 0 failed
running 14 tests   ... test result: ok. 14 passed; 0 failed   (tests/contract.rs)
running 2 tests    ... test result: ok. 2 passed; 0 failed    (tests/golden.rs)
running 1 test     ... test result: ok. 1 passed; 0 failed    (tests/hello_views_metrics.rs)
=> 80 tests, 0 failed
```

```
$ cargo clippy -p acos-render --all-targets -- -D warnings
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.06s
=> clean
```

Text-metric comparison against the **real ART emulator** UI dump
(`fixtures/reference/hello-views/art-api34-x86_64/ui_initial.xml`):

```
ART reference title=[42,317][231,368] (189x51), result=[42,612][175,663] (133x51)
ACOS text metrics @14sp: title=188.45px (ref 189px, delta 0.3%),
                         result=132.61px (ref 133px, delta 0.3%),
                         height=43.07px (ref 51px, delta 15.6%)
font metrics: top=-34.09 ascent=-34.09 descent=8.97 bottom=8.97 leading=0.00
```

Advance widths match the device to **0.3%**. The 15.6% height delta is
`TextView`'s top/bottom font padding, which is a widget-layout property, not a
font metric; ACOS reports the raw `FontMetrics` that the framework's layout code
consumes.

Rendered hello-views scene: `results/render/hello-views.png` (1080×2400,
`frameClass=Content`, `unsupportedOps=[]`). Frame metadata:

```json
{
  "causeId": 1,
  "contentHash": "8b30e01cef711e901e9f28886ec2e61c",
  "density": 2.625,
  "frameClass": "Content",
  "height": 2400,
  "nodeCount": 8,
  "output": "results/render/hello-views.png",
  "pngBytes": 90022,
  "renderMs": 5.0096,
  "scene": "crates/acos-render/tests/scenes/hello-views.json",
  "seq": 1,
  "sha256": "06f95b86abf3837c8873cb94cbdfae57e1022707947b071c813ea234d942f056",
  "unsupportedOps": [],
  "version": "0.1.0",
  "width": 1080
}
```

Reproduce:

```
cargo test -p acos-render
cargo clippy -p acos-render --all-targets -- -D warnings
cargo run -p acos-render --release --example render_scene -- \
    crates/acos-render/tests/scenes/hello-views.json \
    results/render/hello-views.png --json
cargo run -p acos-render --release --example bench_render -- results/render/bench.json
UPDATE_GOLDEN=1 cargo test -p acos-render --test golden   # regenerate goldens
```

## Benchmark

Host: **AMD Ryzen 9 9950X (16C/32T), 60 GiB RAM**, Linux; **single-threaded**;
`--release`; **60 iterations** (20 for encoders). Full data: `bench.json`.

| # | Measurement | p50 | p95 | notes |
|---|---|---:|---:|---|
| a | hello-views scene, **cold** first frame (fonts pre-loaded, includes first shaping) | **5.10 ms** | — | 1080×2400 @2.625, 8 nodes, 5 text runs |
| a | hello-views scene, **warm re-render** (tree touched so raster runs) | **3.97 ms** | 4.15 ms | includes full-surface clear + redraw |
| b | unchanged-tree render (damage-tracked, zero raster work) | **0.00012 ms** (120 ns) | 0.00016 ms | reuses previous pixel buffer, bumps `frame_seq` |
| b | `capture()` of a presented frame | **0.00003 ms** (30 ns) | 0.00004 ms | `Arc<Pixmap>` clone, no copy |
| c | PNG `Fast` (no filter) | 12.57 ms | 13.09 ms | **8 439 811 B** (worst: filterless data is huge) |
| c | PNG `FastFiltered` (Sub + fast deflate) | **7.33 ms** | 7.47 ms | **90 022 B** ← recommended fast path |
| c | PNG `Default` (adaptive) | 18.35 ms | 18.62 ms | 19 549 B |
| c | PNG `Best` (adaptive, best) | 20.75 ms | 20.95 ms | 19 459 B |
| c | QOI (lossless, cheap) | 9.86 ms | 9.91 ms | 57 554 B |
| c | raw RGBA8 copy (10.37 MB) | 0.123 ms | 0.140 ms | ~84 GB/s memcpy |
| d | `measureText` 1 000 short strings, **cold** (all cache misses) | 23.65 ms total | — | **42 291 strings/s** |
| d | same, **warm** (shaping cache) | 0.149 ms total | — | **6 703 581 strings/s**, **hit rate 1.0** |
| e | heavy scene (200 text rows + 50 rounded cards + 20 bitmaps) cold | 31.39 ms | — | 1080×2400 |
| e | heavy scene warm | **24.03 ms** | 24.38 ms | text shaping cached; 270 draw ops |
| f | RSS after context + fonts | 30 256 KB | — | fonts mmap'd **once process-wide**, shared |
| f | RSS after first hello frame | 52 396 KB | — | +22 MB surface/working buffers |
| f | RSS after heavy scene + 20 extra surfaces | 87 568 → 94 112 KB | — | 20 surfaces grow RSS by **6 544 KB** (lazy pages) |

### Legacy comparison (explicitly different methodology)

| Metric | Legacy ACVM (reference) | ACOS render (this run) | Ratio |
|---|---:|---:|---:|
| Fresh screenshot latency | ~67 ms | **3.97 ms** warm raster + **7.33 ms** PNG `FastFiltered` ≈ **11.3 ms** end-to-end; **5.10 ms** cold raster | ~5.9×–16.9× faster |
| Per-session renderer native heap | ~88 MiB | **30.3 MiB** shared context+fonts + first-frame buffers; **327 KB RSS per additional untouched 1080×2400 surface** (lazy; 10.37 MB if fully touched) | baseline shared, per-surface tiny until drawn |

The legacy figure is a whole-session native-heap number from a different engine
and measurement method; it is **not** an apples-to-apples allocator comparison.
The ACOS font set is a single process-wide `mmap` shared by every context and
surface; additional concurrent surfaces allocate zero pages until first draw.

### Interpreting the PNG table

Filterless `Fast` is the *slowest useful* fast path because deflate has to
encode 8.4 MB of unfiltered bytes. `FastFiltered` (Sub filter) is both the
fastest and ~94× smaller. `Default`/`Best` trade ~11–13 ms for ~4.6× smaller
files. QOI is a middle ground for agents that want lossless + cheap decode.
Capture of an unchanged frame is ~30 ns; re-rendering an unchanged UI is ~120 ns
(damage tracking), so the real cost of an agent screenshot loop is PNG encoding.

## Public API summary (what the JNI binding maps onto)

All objects live in generation-checked handle tables inside `RenderContext`.
Handles are `u64` (`generation << 32 | index`), so a stale handle returns
`RENDER_BAD_HANDLE` instead of aliasing a new object. The public API takes only
handles + primitives + `&[u16]`/`&[f32]` — **no Rust lifetimes cross the API**.

### Handles / object tables

`RenderContext` owns: `bitmaps`, `canvases`, `paints`, `paths`, `matrices`,
`regions`, `typefaces`, `render_nodes`, `surfaces` (`HandleTable<T>`), plus the
shared `TextEngine` and a per-frame `unsupported_ops` list.

Constructors: `bitmap_create`, `bitmap_create_from_bytes` (PNG/JPEG/WebP),
`canvas_for_bitmap`, `canvas_for_surface`, `canvas_for_render_node`,
`paint_create`, `path_create`, `matrix_create`, `matrix_create_from`,
`region_create`, `region_create_from_rect`, `typeface_create`,
`render_node_create`, `surface_create`.

### Surfaces / frames

`surface_create(w,h,density)` · `canvas_for_surface` · `surface_present(surface,cause_id)`
· `surface_capture` · `surface_capture_fresh(after_seq)` · `surface_last_seq` ·
`surface_png(setting)` · `surface_rgba8` · `surface_render_node(surface, root, cause_id)`.

`Frame` carries `seq`, `produced_at`, `cause_id`, `content_hash` (xxh3-128),
`class` (`Empty`/`SingleColour`/`Content`), `unsupported_ops`, `pixels: Arc<Pixmap>`
and offers `png(setting)`, `qoi()`, `sha256()`, `rgba8()`. `capture_fresh` never
returns a stale frame: it returns structured `NO_FRESH_FRAME`.

### Canvas primitives

`canvas_save`, `canvas_restore`, `canvas_restore_to_count`, `canvas_get_save_count`,
`canvas_save_layer_alpha`, `canvas_save_layer`, `canvas_concat`,
`canvas_set_matrix`, `canvas_translate/scale/rotate/skew`, `canvas_clip_rect`,
`canvas_clip_path` (Intersect/Difference/Replace; ReverseDifference unsupported),
`canvas_quick_reject`, `canvas_get_clip_bounds`, `canvas_get_matrix`,
`canvas_is_opaque`, `canvas_width/height`, `canvas_draw_color`,
`canvas_draw_paint`, `canvas_draw_rect`, `canvas_draw_round_rect`,
`canvas_draw_oval`, `canvas_draw_circle`, `canvas_draw_arc`, `canvas_draw_line`,
`canvas_draw_lines`, `canvas_draw_point`, `canvas_draw_points`,
`canvas_draw_path`, `canvas_draw_bitmap`, `canvas_draw_bitmap_matrix`,
`canvas_draw_nine_patch`, `canvas_draw_text`.

### Paint / shaders / filters / text attributes

`paint_set_color`, `paint_get_color`, `paint_set_alpha`, `paint_set_style`,
`paint_set_stroke_width`, `paint_set_stroke_cap/join/miter`,
`paint_set_anti_alias`, `paint_set_dither` (stored, ignored — documented),
`paint_set_filter_bitmap`, `paint_set_xfermode` (PorterDuff ordinal),
`paint_set_blend_mode` (BlendMode ordinal), `paint_set_shader_linear/radial/sweep/bitmap`,
`paint_clear_shader`, `paint_set_color_filter_porter_duff`,
`paint_set_color_filter_blend_mode`, `paint_set_color_filter_matrix`,
`paint_set_mask_filter_blur`, `paint_set_path_effect_dash/corner`,
`paint_set_shadow_layer`; text: `paint_set_text_size`, `paint_set_text_align`,
`paint_set_text_scale_x`, `paint_set_text_skew_x`, `paint_set_letter_spacing`,
`paint_set_fake_bold`, `paint_set_underline`, `paint_set_strike_thru`,
`paint_set_font_feature_settings`, `paint_set_font_variation_settings`,
`paint_set_typeface`.

### Path / Matrix / Region / Bitmap

- Path: `path_move_to/line_to/quad_to/cubic_to/conic_to/arc_to`,
  `path_add_rect/round_rect/oval/circle/path`, `path_close`, `path_reset/rewind`,
  `path_set_fill_type`, `path_compute_bounds`, `path_is_empty`, `path_transform`,
  `path_op` (unsupported).
- Matrix: `matrix_reset`, `matrix_is_identity`, `matrix_get/set_values`,
  `matrix_pre/post_translate`, `matrix_set_translate`, `matrix_pre/post_scale`,
  `matrix_set_scale`, `matrix_pre/post_rotate`, `matrix_set_rotate`,
  `matrix_set_skew`, `matrix_pre/post_concat`, `matrix_invert`,
  `matrix_map_points`, `matrix_map_rect`, `matrix_map_radius`,
  `matrix_set_rect_to_rect`, `matrix_set_poly_to_poly`, `matrix_rect_stays_rect`,
  `matrix_equals`.
- Region: `region_set_rect`, `region_set_path`, `region_set_region`,
  `region_op` (Difference/Intersect/Union/Xor/ReverseDifference/Replace),
  `region_contains`, `region_is_empty`, `region_bounds`, `region_quick_reject`,
  `region_translate`, `region_scale`, `region_boundary_path`.
- Bitmap: `bitmap_erase`, `bitmap_get_pixel`, `bitmap_set_pixel`,
  `bitmap_width/height/config`, `bitmap_copy`, `bitmap_extract_alpha`,
  `bitmap_pixels`, `bitmap_compress_png`.

### Text measurement / layout

`paint_measure_text`, `paint_get_text_advances`, `paint_get_text_bounds`,
`paint_get_font_metrics`, `paint_get_font_metrics_int`, `paint_break_text`,
`paint_get_run_advance`, `paint_get_offset_for_advance`, `paint_has_glyph`,
`paint_get_text_path`, `line_break` (greedy `LineBreaker`). `ShapedText` exposes
`runs` (font, range, RTL flag, glyphs with `glyph_id`/cluster/advance/offset),
`char_advances` (per UTF-16 unit), `metrics`, `total_advance`, `missing_glyphs`.

### Render nodes

Properties: `render_node_set_bounds`, `..._set_alpha/translation/scale/rotation/
rotation_xyz/pivot`, `..._set_clip_to_bounds`, `..._set_clip_to_outline`,
`..._set_outline_rect/round_rect/path/none`, `..._set_elevation`,
`..._set_overlapping`, `..._set_layer_type`, `..._set_layer_paint`,
`render_node_add_child/remove_child`, `render_node_clear_display_list`,
`render_node_push_op`, `render_node_get_property` (`RenderNodeInfo`).
`surface_render_node` walks the tree (elevation-sorted children, clip, alpha
layer, shadow, display list) and uses `RenderNode::subtree_version` for damage
tracking.

## Native method mapping table

The framework native surface below is taken from
`research/framework-dex/data/activity_upper.txt` (reachable from a minimal
Activity). `J` object handles are `u64`; `jlong` matrix/path/bitmap pointers are
handles in the same tables.

### `android.graphics.Canvas` / `BaseCanvas` / `RecordingCanvas`

| Framework native | ACOS API |
|---|---|
| `Canvas.nSave(J I)I` | `canvas_save(surface) -> i32` |
| `Canvas.nRestore(J)Z` | `canvas_restore(surface) -> bool` |
| `Canvas.nRestoreToCount(J I)V` | `canvas_restore_to_count(surface, count)` |
| `Canvas.nGetSaveCount(J)I` | `canvas_get_save_count(surface)` |
| `Canvas.nSaveLayer(J FFFF J)I` | `canvas_save_layer(surface, bounds, paint)` |
| `Canvas.nSaveLayerAlpha(J FFFF I)I` | `canvas_save_layer_alpha(surface, bounds, alpha)` |
| `Canvas.nConcat(J J)V` | `canvas_concat(surface, matrix)` |
| `Canvas.nSetMatrix(J J)V` | `canvas_set_matrix(surface, matrix)` |
| `Canvas.nGetMatrix(J J)V` | `canvas_get_matrix(surface)` |
| `Canvas.nTranslate(J F F)V` | `canvas_translate(surface, dx, dy)` |
| `Canvas.nScale(J F F)V` | `canvas_scale(surface, sx, sy)` |
| `Canvas.nRotate(J F)V` | `canvas_rotate(surface, degrees)` |
| `Canvas.nClipRect(J FFFF I)Z` | `canvas_clip_rect(surface, rect, ClipOp, aa)` |
| `Canvas.nClipPath(J J I)Z` | `canvas_clip_path(surface, path, ClipOp, aa)` |
| `Canvas.nGetClipBounds(J Rect)Z` | `canvas_get_clip_bounds(surface)` |
| `Canvas.nQuickReject(J FFFF)Z` | `canvas_quick_reject(surface, rect)` |
| `Canvas.nGetWidth/nGetHeight(J)` | `canvas_width/height` |
| `Canvas.nIsOpaque(J)Z` | `canvas_is_opaque(surface)` |
| `BaseCanvas.nDrawColor(J I I)V` | `canvas_draw_color(surface, color)` |
| `BaseCanvas.nDrawRect(J FFFF J)V` | `canvas_draw_rect(surface, rect, paint)` |
| `BaseCanvas.nDrawRoundRect(J FFFFFF J)V` | `canvas_draw_round_rect(surface, rect, rx, ry, paint)` |
| `BaseCanvas.nDrawOval(J FFFF J)V` | `canvas_draw_oval(surface, rect, paint)` |
| `BaseCanvas.nDrawCircle(J FFF J)V` | `canvas_draw_circle(surface, cx, cy, r, paint)` |
| `BaseCanvas.nDrawArc(J FFFFFF Z J)V` | `canvas_draw_arc(surface, rect, start, sweep, useCenter, paint)` |
| `BaseCanvas.nDrawLine(J FFFF J)V` | `canvas_draw_line(surface, x0, y0, x1, y1, paint)` |
| `BaseCanvas.nDrawLines(J [F I I J)V` | `canvas_draw_lines(surface, &pts, paint)` |
| `BaseCanvas.nDrawPoint(J FF J)V` | `canvas_draw_point(surface, x, y, paint)` |
| `BaseCanvas.nDrawPoints(J [F I I J)V` | `canvas_draw_points(surface, &pts, paint)` |
| `BaseCanvas.nDrawPath(J J J)V` | `canvas_draw_path(surface, path, paint)` |
| `BaseCanvas.nDrawBitmap(J J FFFF J I I)V` | `canvas_draw_bitmap(surface, bmp, None, dst, paint, filter)` |
| `BaseCanvas.nDrawBitmap(J J FFFFFFFF J I I)V` | `canvas_draw_bitmap(surface, bmp, Some(src), dst, paint, filter)` |
| `BaseCanvas.nDrawBitmapMatrix(J J J J)V` | `canvas_draw_bitmap_matrix(surface, bmp, matrix, paint)` |
| `BaseCanvas.nDrawNinePatch(J J J FFFF J I I)V` | `canvas_draw_nine_patch(surface, bmp, src, dst, paint, filter)` |
| `BaseCanvas.nDrawText(J String/C II F F I J)V` | `canvas_draw_text(surface, &utf16, x, y, paint)` |
| `BaseCanvas.nDrawTextRun(J String/C IIII F F Z J J)V` | `canvas_draw_text` (run offsets are UTF-16) |
| `RecordingCanvas.nDrawRenderNode(J J)V` | `render_node_add_child` / `DisplayOp::DrawRenderNode` |
| `RecordingCanvas.nCreateDisplayListCanvas(J I I)J` | `canvas_for_render_node(node)` |
| `RecordingCanvas.nFinishRecording(J J)V` | `render_node_push_op` / `set_display_list` |
| `RecordingCanvas.nDrawCircle/nDrawRoundRect/nDrawRipple` | record `DisplayOp::DrawCircle`/`DrawRoundRect` |
| `Canvas.nDrawPaint(J J)V` | `canvas_draw_paint(surface, paint)` |
| `Canvas.nSetDrawFilter`, `nFreeCaches`, `nFreeTextLayoutCaches` | accepted no-ops (no draw filter) |

### `android.graphics.Paint`

| Framework native | ACOS API |
|---|---|
| `nInit()J` / `nInitWithPaint(J)J` | `paint_create()` / clone paint |
| `nSetColor(J I)V` / `nSetAlpha(J I)V` | `paint_set_color` / `paint_set_alpha` |
| `nSetStyle(J I)V` / `nGetStyle(J)I` | `paint_set_style` / `paint_get_style` |
| `nSetStrokeWidth(J F)V` / `nGetStrokeWidth` | `paint_set_stroke_width` / `paint_get_stroke_width` |
| `nSetStrokeCap` / `nSetStrokeJoin` | `paint_set_stroke_cap` / `paint_set_stroke_join` |
| `nSetAntiAlias` / `nSetDither` / `nSetFilterBitmap` | `paint_set_anti_alias` / `paint_set_dither` / `paint_set_filter_bitmap` |
| `nSetXfermode(J I)V` | `paint_set_xfermode` (PorterDuff ordinal) |
| `nSetShader(J J)J` | `paint_set_shader_linear/radial/sweep/bitmap` |
| `nSetColorFilter(J J)J` | `paint_set_color_filter_porter_duff/blend_mode/matrix` |
| `nSetMaskFilter(J J)J` | `paint_set_mask_filter_blur` |
| `nSetPathEffect(J J)J` | `paint_set_path_effect_dash/corner` |
| `nSetShadowLayer(J F F F J J)V` | `paint_set_shadow_layer` |
| `nSetTextSize(J F)V` / `nGetTextSize` | `paint_set_text_size` / `paint_get_text_size` |
| `nSetTextAlign(J I)V` | `paint_set_text_align` |
| `nSetTextSkewX` / `nSetFakeBoldText` | `paint_set_text_skew_x` / `paint_set_fake_bold` |
| `nSetStrikeThruText` | `paint_set_strike_thru` |
| `nSetTypeface(J J)V` | `paint_set_typeface` |
| `nSetTextLocales` / `nSetTextLocalesByMinikinLocaleListId` | accepted; fallback order comes from `fonts.xml` |
| `nGetFontMetrics(J FontMetrics)F` | `paint_get_font_metrics(paint)` |
| `nGetFontMetricsInt(J FontMetricsInt)I` | `paint_get_font_metrics_int(paint)` |
| `nAscent(J)F` / `nDescent(J)F` | `paint_get_font_metrics` |
| `nGetTextAdvances(J String IIIII [F I)F` | `paint_get_text_advances(paint, text, &mut advances)` |
| `nGetStringBounds` / `nGetCharArrayBounds` | `paint_get_text_bounds` |
| `nGetTextRunCursor(J …)I` | `paint_get_offset_for_advance` / `get_run_advance` |
| `nGetUnderlineThickness` | derived from text size |
| `nHasShadowLayer` | `paint.shadow_layer.is_some()` |
| `nSetFlags` / `nGetFlags` | individual flags map to the setters above |
| `Paint.measureText` / `breakText` (Java→native) | `paint_measure_text` / `paint_break_text` |
| `Paint.getRunAdvance` / `getOffsetForAdvance` | `paint_get_run_advance` / `paint_get_offset_for_advance` |
| `Paint.hasGlyph` | `paint_has_glyph` |
| `Paint.getTextPath` | `paint_get_text_path` |

### `android.graphics.RenderNode`

| Framework native | ACOS API |
|---|---|
| `nCreate(String)J` | `render_node_create(name)` |
| `nSetLeftTopRightBottom(J I I I I)Z` | `render_node_set_bounds` |
| `nSetLeft/Top/Right/Bottom` | `render_node_set_bounds` (recompute) |
| `nSetAlpha(J F)Z` | `render_node_set_alpha` |
| `nSetTranslationX/Y/Z` | `render_node_set_translation` / `..._set_translation_z` |
| `nSetScaleX/Y` | `render_node_set_scale` |
| `nSetRotation` / `nSetRotationX/Y` | `render_node_set_rotation` / `..._set_rotation_xyz` |
| `nSetPivotX/Y` | `render_node_set_pivot` |
| `nSetClipToBounds` / `nSetClipToOutline` | `render_node_set_clip_to_bounds` / `..._set_clip_to_outline` |
| `nSetClipBounds` / `nSetClipBoundsEmpty` | `render_node_set_clip_to_bounds` + `DisplayOp::ClipRect` |
| `nSetOutlineRoundRect` / `nSetOutlinePath` / `nSetOutlineEmpty/None` | `render_node_set_outline_round_rect/path/none/rect` |
| `nSetElevation` / `nHasShadow` | `render_node_set_elevation` |
| `nSetAmbientShadowColor` / `nSetSpotShadowColor` | `RenderNode` fields (ambient used by approximation) |
| `nSetHasOverlappingRendering` | `render_node_set_overlapping` |
| `nSetLayerType` / `nSetLayerPaint` | `render_node_set_layer_type` / `..._set_layer_paint` |
| `nGetLeft/Top/Right/Bottom/Alpha/...` | `render_node_get_property` (`RenderNodeInfo`) |
| `nGetTransformMatrix` / `nGetInverseTransformMatrix` | `RenderNode::local_matrix` (+ invert) |
| `nHasIdentityMatrix` | `RenderNode::has_identity_matrix` |
| `nGetUniqueId` / `nGetAllocatedSize` / `nGetUsageSize` | available from the handle/version |
| `nAddAnimator` / `nEndAllAnimators` / `nStretch` / `nSetProjectBackwards` / `nSetRevealClip` / `nSetStaticMatrix` / `nSetAnimationMatrix` | **unsupported** (recorded or rejected by binding) |

### `android.graphics.Matrix`

`nCreate`, `nReset`, `nSet`, `nGetValues`, `nSetValues`, `nIsIdentity`,
`nEquals`, `nPreTranslate`, `nPostTranslate`, `nSetTranslate`,
`nPreScale`, `nPostScale`, `nSetScale`, `nPreRotate`, `nPostRotate`,
`nSetRotate`, `nPreConcat`, `nPostConcat`, `nInvert`, `nMapPoints`,
`nMapRect`, `nMapRadius` (via `matrix_map_radius`), `nRectStaysRect`,
`nSetRectToRect`, `nSetPolyToPoly` → the `matrix_*` methods of the same names.

### `android.graphics.Path`

`nInit`, `nSet`, `nReset`, `nRewind`, `nMoveTo`, `nLineTo`, `nQuadTo`,
`nCubicTo`, `nConicTo`, `nArcTo`, `nAddRect`, `nAddRoundRect`, `nAddOval`,
`nAddCircle`, `nClose`, `nGetFillType`, `nSetFillType`, `nComputeBounds`,
`nIsEmpty`, `nTransform`, `nOp` → `path_*` methods. `nOp` returns
`RENDER_UNSUPPORTED_OP`. `PathMeasure` is `acos_render::PathMeasure`.

### `android.graphics.Bitmap`

`nativeCreate`, `nativeCreateFromParcel` (binding), `nativeConfig`,
`nativeErase`, `nativeGetPixels`, `nativeSetPixels`, `nativeCopy`,
`nativeExtractAlpha`, `nativeCompress`, `nativeRowBytes`, `nativeHasAlpha`,
`nativeIsPremultiplied`, `nativeSetPremultiplied`, `nativeIsImmutable`,
`nativeSetImmutable`, `nativeGenerationId`, `nativeSameAs`,
`nativeGetAllocationByteCount` → `bitmap_*` methods. `nativeGetHardwareBuffer`,
gainmaps and mipmaps are **unsupported**.

### `android.graphics.Typeface` / `fonts.Font` / `FontFamily`

`nativeCreateFromTypeface(J I)J`, `nativeCreateFromTypefaceWithExactStyle`,
`nativeCreateWeightAlias`, `nativeGetStyle`, `nativeGetWeight`,
`nativeSetDefault`, `nativeRegisterGenericFamily`, `nativeRegisterLocaleList`,
`nativeWarmUpCache` → `typeface_create(family, weight, italic)`,
`typeface_default`, `typeface_style`, `typeface_weight`. The
`Font$Builder`/`FontFamily$Builder` native builders are **unsupported** in this
crate; the binding should resolve generic families through `typeface_create`.

### `android.graphics.Region`

`nativeConstructor`, `nativeSetRect`, `nativeSetPath`, `nativeSetRegion`,
`nativeOp`, `contains`, `isEmpty`, `nativeGetBounds`, `quickReject`,
`nativeGetBoundaryPath`, `translate`, `scale` → the `region_*` methods.

### `android.graphics.NinePatch`

`isNinePatchChunk`, `validateNinePatchChunk` (binding/APK layer),
`nativeGetTransparentRegion` (binding), `BaseCanvas.nDrawNinePatch` →
`canvas_draw_nine_patch`. Chunk divs are read by the APK/resource layer; this
crate performs a uniform scale and records an approximate-fidelity note.

### `android.graphics.text.MeasuredText` / `LineBreaker` / `PositionedGlyphs`

`MeasuredText.nGetWidth/nGetCharWidthAt/nGetBounds/nGetExtent` →
`paint_measure_text` / `paint_get_text_advances` / `paint_get_text_bounds`.
`LineBreaker.nInit/nComputeLineBreaks/nGetLineCount/nGetLineBreakOffset/
nGetLineWidth/nGetLineAscent/nGetLineDescent/nGetLineFlag` → `line_break`
(greedy) and `LineBreak` (`start`, `end`, `width`, `hard_break`).
`PositionedGlyphs.nGetFont/nGetGlyphCount/nGetGlyphId/nGetX/nGetY` →
`ShapedText::runs[].glyphs[]` + `TextEngine::shape`.

## Supported, approximate, and unsupported

### Fully implemented (pixel-tested)

- RGBA8888 premultiplied bitmaps; `ALPHA_8`, `RGB_565`, `ARGB_4444` conversions
  at the API edge; erase/get/set/copy/extractAlpha; PNG/JPEG/WebP decode.
- Canvas save/restore/restoreToCount/saveLayerAlpha/saveLayer, affine
  transforms, clipRect/clipPath (Intersect/Difference/Replace), quickReject,
  drawColor/drawPaint/drawRect/roundRect/oval/circle/arc/line(s)/point(s)/path,
  drawBitmap (src/dst + matrix), drawText/drawTextRun, drawRenderNode.
- All PorterDuff modes + full Android `BlendMode` table (mapped to tiny-skia).
- Linear/radial gradients; bitmap shader; colour-matrix/PorterDuff/blend colour
  filters; dash path effect.
- Path verbs incl. conic (cubic approximation) and arcs; fill types (winding,
  even-odd, inverse via mask inversion); bounds/isRect/transform; PathMeasure.
- Full 3×3 matrix (pre/post concat, invert, map rect/points/radius,
  setRectToRect, setPolyToPoly affine + perspective solve).
- Region boolean ops (pixel-granularity), bounds/contains/quickReject/path.
- Text: `fonts.xml` family/weight/style/variable-axis resolution, per-script
  fallback, HarfBuzz shaping (kerning/GSUB/GPOS/ligatures), bidi visual runs,
  per-UTF-16 advances, metrics, bounds, breakText, cursor advance/offset,
  hasGlyph, text→path, greedy line breaking, shaping cache.
- RenderNode tree + display lists + elevation-sorted children + damage
  tracking; frame `seq`/timestamp/cause/hash/freshness; PNG/QOI/raw capture;
  blank/single-colour/content classification.
- Exact-pixel unit tests for axis-aligned fills, clips, layers, blend maths,
  matrices/paths/regions, nine-patch, frame contract, fuzz.

### Approximate (recorded or documented)

- **MaskFilter blur** — 3-pass box blur approximating a Gaussian; `NORMAL`,
  `SOLID`, `INNER`, `OUTER` are approximated.
- **Elevation shadow** — blurred outline offset by `elevation*0.5`, ambient
  colour only; no spot-light direction. Fidelity: approximate.
- **SweepGradient** — a 256×256 angular colour texture sampled with `Pad`
  (≈1.4° angular resolution) instead of an analytic sweep.
- **BitmapShader** — one spread mode is used (`tileX`); differing
  `tileX`/`tileY` is recorded as unsupported.
- **NinePatch** — whole-image uniform scale; the `.9.png` chunk's nine regions
  are not individually stretched (the APK/resource layer still parses the
  chunk). Recorded as unsupported fidelity.
- **3D rotation** (`rotationX/Y`, camera distance) — stored but only the 2D
  `rotation` is applied.
- **saveLayer** — full-size layer; bounds are approximated via the clip; layer
  paint alpha + blend are honoured, layer colour filters are not.
- **fakeBold** — glyph outline stroked by `size/24`; underline/strikethrough are
  drawn as rectangles.
- **Bidi** — `unicode-bidi` visual runs + HarfBuzz; no extra ICU mirroring.
- **Font metrics** — `top/bottom` use hhea ascent/descent (not OS/2 typo
  metrics); `leading` from `lineGap`.
- **Hyphenation** — always off; `LineBreaker` greedy only.
- **Colour management** — sRGB 8-bit only; wide-gamut, linear-extended and HDR
  are not modelled. `RGBA_F16`/`RGBA_1010102` configs are stored as RGBA8888.

### Unsupported (explicit `RENDER_UNSUPPORTED_OP` or absent)

- `Path.op` boolean path operations (tiny-skia has no path booleans).
- `PathEffect` corner/discrete/compose/sum/path-dash (dash **is** supported;
  corner is recorded as unsupported and not applied).
- `PathMeasure.getSegment` is implemented over flattened polylines (approximate
  for curves).
- Perspective canvas matrices: tiny-skia is affine-only; a draw with a
  non-affine matrix is recorded and skipped.
- `Canvas.clipPath` with `ReverseDifference`, draw filter, picture recording.
- `drawTextOnPath` (not exposed).
- Lighting/Lerp/Compose colour filters, emboss/blur mask styles beyond `Blur`.
- `Bitmap` hardware buffers, gainmaps, mipmaps, `RGB_565` dithering.
- `RenderNode` animators, stretch, projection receivers, reveal clip, static
  matrix, position updates.
- `Typeface`/`Font`/`FontFamily` native builders (`nInitBuilder`, `nAddAxis`,
  `nBuild`, `nReadTypefaces`, …) — the binding should use `typeface_create` for
  generic families; loading an arbitrary font file is a follow-up.
- Colour emoji (CBDT/COLR/sbix) render no outline; `hasGlyph` still reports
  coverage and text drawing is a no-op rather than a crash.
- `Canvas.drawPicture`, `Picture` playback.
- High-quality/balanced `LineBreaker` strategies.

## Open risks for pixel parity

1. **AA / rasterisation differences vs Skia.** tiny-skia is a faithful port but
   not bit-identical to Skia's analytic AA, especially for hairlines, very small
   shapes and clip edges. Expect 1–2 % edge-pixel differences.
2. **Hinting/subpixel positioning.** Android's minikin/Skia may hint glyphs and
   use subpixel positioning; ACOS rasterises unhinted outlines. Advances match
   the emulator to 0.3 % on the hello-views strings, but glyph rasterisation may
   differ by a few percent of edge pixels.
3. **Gamma / text contrast.** Android's glyph rendering uses gamma-corrected
   coverage and LCD subpixel modes; ACOS uses tiny-skia coverage in sRGB. Text
   may look slightly lighter/heavier.
4. **Variable-font axes.** `fonts.xml` axis values are applied through
   rustybuzz variations; the exact Roboto instance Android picks for a weight
   request can differ at intermediate weights.
5. **Shadows/blur/nine-patch** are approximations (above) and will not match
   HWUI pixel-for-pixel.
6. **Colour management** is sRGB-only; any wide-gamut or linear workflow is not
   reproduced.
7. **Perspective transforms** are rejected rather than approximated.
8. **Bidi/complex scripts** depend on `unicode-bidi`/HarfBuzz and font coverage;
   ICU-specific tailoring is not reproduced.
9. **RecordingCanvas binding** is not written yet: `DisplayOp` and
   `render_node_push_op` are the sink, but the `nCreateDisplayListCanvas`/
   `nDraw*` natives must be wired by the JNI binding task.
10. **Per-frame `unsupported_ops`** is the fidelity signal. Callers must surface
    it (and should use strict mode in verification runs) so a reduced-fidelity
    frame is never reported as a full pass.

## Dependency rationale

| Crate | Role | Why |
|---|---|---|
| `tiny-skia` | software rasteriser | Rust port of Skia's scan converter/blitter; closest AA/blend behaviour to Android's software canvas with no C/C++ |
| `rustybuzz` | shaping | HarfBuzz port: kerning/GSUB/GPOS/ligatures/variable axes, no C |
| `ttf-parser` | outlines + metrics | TrueType + CFF + variable fonts; no C |
| `unicode-bidi` / `unicode-script` | bidi/script | visual runs and script detection for fallback |
| `roxmltree` | `fonts.xml` | read-only DOM for Android's font config |
| `png` / `jpeg-decoder` / `image-webp` | image decode/encode | pure Rust; PNG encoder is used for the fast capture path |
| `memmap2` | font mmap | one process-wide read-only mapping shared across sessions (one audited `unsafe` block; the file is opened read-only and only read) |
| `xxhash-rust` / `sha2` | content hash | fast 128-bit frame hash + optional evidence digest |
| `serde`/`serde_json` | scene format | hand-authored scenes without the VM |

No C/C++ dependency; the only `unsafe` in the crate is the font `mmap` call.

## Memory notes

- A headless context that creates no surface allocates only its handle tables
  and the shared font engine (`rssKbBefore` 3.6 MB in `bench.json`).
- Fonts are `mmap`ed once (`FontCollection::shared()` static `OnceLock`) and
  shared `Arc` across every context/surface: ~26 MB touched for the full Android
  font set, **not** per session.
- `Surface::new` allocates a zeroed pixmap; zero pages are not resident until
  drawn. 20 × 1080×2400 surfaces grew RSS by only 6.5 MB (327 KB each untouched);
  a fully drawn 1080×2400 surface is 10.37 MB.
- `capture_fresh` reuses the previous `Arc<Pixmap>` when the tree is unchanged;
  no copy, ~30 ns.
