# M3 render/frame log — first rendered frame + frame-diff (W41-A)

**Status: renderer verified independently against the ART reference; the
framework-bound frame (ViewRootImpl → HWUI → FrameStore) is still blocked
upstream in M2.** There is a real, measured, non-black hello-views frame from
the DisplayList/Canvas path, a numeric per-pixel + structural diff tool
(`tools/frame-diff`, `acos frame-diff`), and an end-to-end synthetic frame that
reaches the `FrameStore` through the WMS compositor. The app cannot yet drive
that path itself: `Application.onCreate` is not reached (M2, W40-A).

Everything here is measured on this tree; no number is a placeholder.

## Deliverables

| Deliverable | Path | Evidence |
|---|---|---|
| Diff engine (per-pixel + structural SSIM + blocks + heatmap) | `crates/acos-render/src/diff.rs` | 6 unit tests + `tests/reference_diff.rs` |
| `acos frame-diff` subcommand | `crates/acos-cli/src/frame_diff.rs`, `main.rs` | runs, JSON envelope, threshold gates, heatmap |
| `tools/frame-diff` wrapper (`--default`) | `tools/frame-diff` | `./tools/frame-diff --default` |
| Synthetic DisplayList/Canvas frame test vs ART reference | `crates/acos-render/tests/reference_diff.rs` | `hello_views_scene_diff_vs_art_reference` passes |
| Frame pipeline → compositor → `FrameStore`, diffed vs reference | `crates/acos-runtime/tests/frame_reference_diff.rs` | `synthetic_frame_reaches_frame_store_and_matches_reference` passes |
| Measured diff artifact | `results/render/m3-hello-views-vs-art.json` | full `DiffReport` |
| Diff heat map | `results/render/m3-hello-views-vs-art.png` | grey = match, red = mismatch |
| Missing draw ops/natives ranked by impact | this file, §4 | derived from `gfx_*.rs` |

## 1. What a "frame" is here

The renderer's path is: `DisplayOp` display list on an `acos-render::RenderNode`
→ `RenderContext::surface_render_node` → RGBA8888 `Surface` → frame contract
(`frame_seq`, `cause_id`, `content_hash`). The framework's
`RecordingCanvas.nDraw*` natives append `DisplayOp`s; the Android
`HardwareRenderer.nSyncAndDrawFrame` native rasterises the tree and submits it to
a `FrameSink`; `frame_handoff::FrameHandoffSink` composites it through
`WmSurfaceBackend` and publishes a `CompletedFrame` into the `FrameStore`.

The synthetic test path is identical from the `DisplayOp` boundary onward; it
replaces only the Java `View` tree that would have produced those ops.

## 2. Measured diff vs the `art-reference` capture

Command (reproduces the artifact):

```sh
tools/frame-diff \
  fixtures/reference/hello-views/art-api34-x86_64/screenshot_initial.png \
  results/render/hello-views.png --json --heatmap results/render/m3-hello-views-vs-art.png
# or, using the checked-in defaults:
tools/frame-diff --default
```

Reference: `art-reference`, API-34 `google_apis` x86_64 emulator, 1080x2400 @
420 dpi. Candidate: the `hello-views` synthetic DisplayList/Canvas frame,
1080x2400.

| Metric | Value |
|---|---:|
| pixels | 2,592,000 (1080x2400) |
| overall RGB mean absolute error | **0.4554** |
| overall RGB RMSE | **8.1901** |
| max RGB channel error | 230 |
| PSNR | **29.86 dB** |
| differing pixels (max channel delta > 2) | 20,329 (0.784%) |
| changed bounding box | `[0,47,1080,2374]` |
| mean windowed SSIM (11x11 Gaussian) | **0.9938** |
| global (single-window) SSIM | **0.9941** |
| min windowed SSIM | −0.24 (near-constant windows; noise) |
| changed 32px blocks | 89 / 2550 (3.49%) |
| per-channel MAE (R/G/B/A) | 0.4602 / 0.4520 / 0.4542 / 0.0000 |

The same numbers come out of the frame-store path
(`crates/acos-runtime/tests/frame_reference_diff.rs`): the compositor round-trip
is pixel-faithful (only the PNG encode/decode of the direct render differs).

### Reading the numbers honestly

The W47-A scene fix reproduces the ART capture's Material surface colour
(`#FFF0F0F3`), system-bar colours (`#FF191C1E`, black nav bar), the action-bar
elevation gradient, the navigation gesture pill, and the app widgets' real
geometry (hint/underline, inset button). What remains is the system status-bar
clock/icons, which are not part of the app's own draw list, plus AA/font noise:
the synthetic scene is a layout reconstruction, not a framework render.

The regression signal is the **structural** one: mean SSIM 0.9938 / global SSIM
0.9941, with text advances already verified to 0.3% in
`results/render/README.md`. `tests/reference_diff.rs` asserts the W47-A target
(<5% differing pixels, mean/global SSIM > 0.99) so a real regression fails while
a font/AA revision does not.

The windowed SSIM moments are accumulated in `f64` and use the standard
`0.01*255` / `0.03*255` stabilisers (`diff.rs`); an earlier f32 accumulation
lost the covariance at the `C2` scale and produced negative mean SSIM on
near-identical frames.

`counter` is **not** measured: no framework-bound counter frame exists yet
(M2/M4), and the task makes it conditional on M2 reaching Application/Activity
callbacks. This log records the gap rather than fabricating a scene result.

## 3. The gap: why the app cannot drive this yet

The renderer and frame handoff are exercised, but the *framework* cannot reach
drawing:

* M2 blocks in real `ActivityThread.handleBindApplication` in
  `android.graphics.Typeface.<clinit>` → ICU regex / `/system` file substrate
  (`results/m2/M2_LOG.md`); W40-A is clearing this.
* `ClientTransaction` delivery is still a structured `Unsupported`, so
  `MainActivity.onCreate/onStart/onResume` do not run and there is no
  `ViewRootImpl` to schedule a traversal or call
  `HardwareRenderer.nSyncAndDrawFrame`.
* `HardwareRenderer.preload()` / `setupShadersDiskCache` are recorded no-ops
  (declared divergence) and the frame callbacks are stored but not dispatched.

Accordingly the M3 gate itself (a non-black framework-bound frame with
`frame_seq`/cause/hash from a launched Activity) is **not met**. What is met:
the renderer is verified independently, the frame handoff publishes a real
composited frame, and the numeric comparison tool is in place so the moment M2
lands the framework frame the same command produces the gate number.

## 4. Missing draw ops / natives, ranked by impact

Derived from the natives registry (`crates/acos-vm/src/natives/gfx_*.rs`), not
from a framework-bound run (impossible today). "Impact" is for the View /
`ViewRootImpl` draw path and the fixtures; everything unsupported raises a
structured `UNSUPPORTED_JAVA_API` naming the method — nothing is silently
skipped.

**W47-A update.** Gaps **#1 (raster `Canvas(bitmap)`), #2
(`nSaveUnclippedLayer`/`nRestoreUnclippedLayer`), #3 (`LineBreaker`
BALANCED/HIGH_QUALITY), #4 (`Paint.nGetFillPath` stroke-to-fill) and #8
(`clipRect`/`clipPath` `ReverseDifference`)** are now implemented, each with a
unit test; the rows below are kept for history with their new state noted.

| # | Gap | Site | Error / state | Impact | Why |
|---|---|---|---|---|---|
| 1 | **Raster `Canvas(bitmap)` path** — `Canvas.nSetBitmap` and every raster draw op | `gfx_canvas.rs:273`, `nInitRaster` compat handle | structured `UNSUPPORTED_JAVA_API` / stale canvas | **High** | `BitmapDrawable`, `NinePatchDrawable`, `LayerDrawable`, `Picture`, `Surface` compatibility and offscreen caching all render into a bitmap-backed canvas. Recording covers the View hierarchy only. |
| 2 | **`Canvas.nSaveUnclippedLayer` / `nRestoreUnclippedLayer`** | `gfx_canvas.rs:279` | structured unsupported | **High** | HWUI uses unclipped layers for elevation/shadow and `View` overlay drawing; a `View` with `clipToOutline`/elevation can hit it. |
| 3 | **`LineBreaker` strategies other than SIMPLE** | `gfx_text.rs:534` | structured unsupported | **High for general apps, low for fixtures** | Any wrapping `TextView` uses `StaticLayout` → `LineBreaker`; the fixtures are single-line labels so M2–M4 may pass without it. |
| 4 | **`Paint.nGetFillPath` (stroke-to-fill)** | `gfx_paint.rs:841` | structured unsupported | **Medium-High** | Stroked text/dividers and `Paint.getFillPath`; `hello-views` uses only fills, so not hit yet. |
| 5 | **`Paint.nGetTextRunCursor` (bidi cursor)** | `gfx_paint.rs:834` | structured unsupported | **Medium** (M4 input) | `EditText`/`TextView` cursor movement and bidi runs. |
| 6 | **`RenderNode.setRenderEffect`** | `gfx_node.rs:650` | structured unsupported | **Medium** | Blur/render effects; modern Material UI uses them. |
| 7 | **Variable-font axes on the A8 mask** | `gfx_text.rs:464`, ADR-0019 | axes shape but default instance renders | **Medium** | Weight/width axes are stored + used for shaping; the FreeType A8 mask renders the default instance. |
| 8 | **`Canvas.clipRect(ReverseDifference)`** | `gfx_canvas.rs:421` | structured unsupported | **Low-Medium** | `clipChildren=false` / `Region.Op.REPLACE` style clips. |
| 9 | **`Typeface` family not present in `/system/fonts`** | `gfx_text.rs:166/177/198` | structured unsupported | **Environment** | Missing font config makes *all* text shaping raise; the renderer needs the mapped `/system/fonts`. |
| 10 | **`Path.nOp` boolean path ops (some), `Path.nSetLastPoint`, `Path.nInterpolate`** | `gfx_path.rs:449/476/518/525` | structured unsupported | **Low** | Path morphing and exotic boolean ops. |
| 11 | **`Region` ops / parcel** — unsupported `Region.op` variants, `nativeWriteToParcel`/`nativeCreateFromParcel` | `gfx_path.rs:596/673/680/726` | structured unsupported | **Low** | `Region` parceling and rare ops. |
| 12 | **`MeasuredText.Builder.nAddReplacementRun`** | `gfx_text.rs:799` | structured unsupported | **Low** | Emoji/replacement spans. |
| 13 | **`BlendModeColorFilter` for advanced `BlendMode` ordinals** | `gfx_paint.rs:573` | structured unsupported | **Low** | Exotic color filters. |
| 14 | **`Bitmap.Config` unknown / non-positive dims** | `gfx_bitmap.rs:110/113` | structured unsupported | **Low** (bad input) | Only errors on invalid input. |
| — | **frame callbacks not dispatched** | `gfx_hwui.rs` (registered, stored) | recorded, not dispatched | **Medium** | `FrameDrawingCallback`/`FrameCompleteCallback` are needed for `ViewRootImpl` frame accounting once drawing starts. |

Already covered (not gaps): `nDrawRect/RoundRect/Circle/Oval/Arc/Line/Point/Path/
Color/Paint/Text/Bitmap/NinePatch/RenderNode`, save/restore/clip/concat/saveLayer,
text metrics/advances/break/bounds (`gfx_canvas.rs`, `gfx_text.rs`,
`gfx_paint.rs`). `DrawText`/`DrawBitmap` are recorded as `DisplayOp`s and
rasterised, not stubbed.

## 5. Reproduce

```sh
# in a clean worktree of HEAD (W39-A owns the shared exec.rs)
export CARGO_TARGET_DIR=$PWD/.cache/target-w41a CARGO_PROFILE_DEV_DEBUG=0

# renderer + diff tests
tools/guarded.sh --mem 12G --time 900 -- cargo test -p acos-render --test reference_diff -- --nocapture
tools/guarded.sh --mem 12G --time 900 -- cargo test -p acos-render --lib diff

# frame pipeline -> compositor -> FrameStore
tools/guarded.sh --mem 12G --time 900 -- cargo test -p acos-runtime --test frame_reference_diff -- --nocapture

# the measured diff + heatmap
tools/guarded.sh --mem 8G --time 300 -- tools/frame-diff --default \
  --heatmap results/render/m3-hello-views-vs-art.png
```

## 6. Honest caveats

* The candidate frame is **synthetic** (DisplayList/Canvas reconstruction of the
  `hello-views` layout), not a frame produced by the fixture APK's own bytecode.
  It verifies the renderer and the handoff, not the framework integration.
* The diff is not a pixel-equality gate and must not be read as one: the
  background/inset mismatch is expected and labelled here. The structural SSIM
  and the text-advance parity in `results/render/README.md` are the fidelity
  signal.
* `min_ssim` is negative because near-constant SSIM windows are numerically
  unstable; mean/global SSIM are the reported structural numbers.
* The missing-op ranking is a static registry reading; the definitive ranking
  will come from a framework-bound run once W40-A lands M2.
* `counter` remains unmeasured until M2 delivers `Application`/`Activity`
  callbacks; this log does not claim otherwise.
