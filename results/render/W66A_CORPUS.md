# W66-A — renderer fidelity corpus and ranked gap report

**Status:** corpus built and measured against the ART oracle; all scenes render
with `unsupportedOps=[]` and pass reference-diff thresholds. This report is the
honest, measured state; nothing here is synthetic-identical or fabricated.

**Tier:** every reference below is **`art-reference`** (API-34 `google_apis`
x86_64 emulator, 1080x2400 @ 420 dpi, `tools/oracle/emulator.sh`). The
`physical-device` captures in `fixtures/reference-device/` are *not* mixed in
(ADR-0005).

## 1. What was built

| Artifact | Path |
|---|---|
| Scene corpus (one DisplayList/Canvas reconstruction per fixture app) | `crates/acos-render/tests/scenes/{hello-views,counter,two-activity,sqlite-crud,canvas-shapes}.json` |
| Reference-diff test + thresholds + per-scene metrics writer | `crates/acos-render/tests/corpus.rs` |
| New ART captures for the three apps that had none | `fixtures/reference/{canvas-shapes,sqlite-crud,two-activity}/art-api34-x86_64/{screenshot_initial.png,ui_initial.xml,logcat_initial.txt,install.txt,am_start_initial.txt}` |
| Machine-readable metrics | `results/render/corpus-metrics.json` |
| Candidate frames | `results/render/corpus/<app>.png` |
| Software GL/EGL plan (ADR-style) | `results/render/W66A_SOFTWARE_GLES_ADR.md` |

The three new captures were taken with the API-34 oracle emulator headless, one
instance, guest RAM 2048 MB, host `MemAvailable` was 39 GiB (> 16 GiB gate),
then the emulator was shut down. `hello-views` and `counter` reuse their
existing checked-in captures.

## 2. Per-scene metrics vs the ART reference

`candidate` = `acos-render` scene, `reference` = ART screenshot. Same
`acos_render::diff` engine as `acos frame-diff`. Reproduce:
`ACOS_FONT_DIR=<android-fonts> cargo test -p acos-render --test corpus -- --nocapture`.

| scene | pixels | MAE | RMSE | PSNR dB | differing >2 | mean SSIM | global SSIM | blocks changed |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| hello-views | 2,592,000 | 0.4554 | 8.1901 | 29.86 | 0.784% | 0.9938 | 0.9941 | 3.49% |
| counter | 2,592,000 | 0.3904 | 7.9981 | 30.07 | 0.712% | 0.9944 | 0.9944 | 2.82% |
| two-activity | 2,592,000 | 0.4218 | 8.0456 | 30.02 | 0.686% | 0.9946 | 0.9943 | 3.88% |
| sqlite-crud | 2,592,000 | 0.6950 | 10.5212 | 27.69 | 0.970% | 0.9891 | 0.9903 | 4.98% |
| canvas-shapes | 2,592,000 | 0.4632 | 8.4979 | 29.54 | 0.676% | 0.9953 | 0.9946 | 3.02% |

Thresholds asserted in `tests/corpus.rs` (MAE ceiling / SSIM floor / differing
ceiling): hello-views 1.0/0.985/3%, the other four 2.0/0.980/4% — generous
enough that a font or AA revision does not fail, tight enough that a real
regression does.

## 3. Region breakdown — where the remaining error lives

Mean absolute error by screen band (data: `corpus-metrics.json` + the
screenshot pair; bands are the framework chrome vs the app content).

| region (y) | hello-views | counter | two-activity | sqlite-crud | canvas-shapes | **mean** |
|---|---:|---:|---:|---:|---:|---:|
| status bar 0–128 | 4.401 | 4.580 | 4.678 | 4.678 | 4.860 | **4.639** |
| action bar 128–275 | 0.330 | 0.218 | 0.642 | 0.464 | 0.670 | **0.465** |
| app content 275–2337 | 0.220 | 0.135 | 0.115 | 0.446 | 0.150 | **0.213** |
| nav bar 2337–2400 | 0.452 | 0.643 | 1.299 | 1.299 | 1.299 | **0.998** |

## 4. Ranked remaining-gap list

Ranked by measured contribution to the per-scene MAE. "Where" names the
component that owns the gap — this matters: most of the residual is **system
chrome**, not the app renderer.

| # | Gap | Where | Measured impact | Why it remains |
|---|---|---|---|---:|
| 1 | **Status-bar contents** (clock `1:51`, notification glyph, wifi/signal/battery icons) are absent; the scene draws only the flat `#191C1E` band. | SystemUI (`SystemUI`/`com.android.systemui`), not an app draw op | region MAE **4.64** (~0.23 of the ~0.4–0.7 overall) | The clock is a live value and the icons are SystemUI vector drawables. Reproducing them in an app scene would be fabricating a capture; the honest fix is a SystemUI surface that does not exist yet. |
| 2 | **Gesture-pill colour differs between capture sessions** (`#D0D0D0` in the hello-views/counter captures vs `#EBEBEB` in the three fresh captures). Geometry is identical (x 398–681, y 26–36). | SystemUI nav bar | region MAE **1.0–1.3** | System setting/opacity variance between capture runs, not a renderer defect. Scenes use `#D0D0D0`. |
| 3 | **Glyph rasterisation parity** — unhinted FreeType A8 masks + integer pen vs Skia's hinted, subpixel-positioned grayscale. | `acos-render` text | content+action-bar MAE **0.1–0.7**; per-glyph coverage RMSE 0.039 (ADR-0019) | Advances already match to 0.3%; what is left is per-pixel AA and subpixel positioning. |
| 4 | **Emoji colour fonts** (COLRv1/CBDT) render as a monochrome A8 mask; the status bar shows a colour emoji. | `acos-render` text (`NotoColorEmoji.ttf` is in the font set) | not in app content of these five | Needs a COLR/CBDT paint path in the FreeType backend; scheduled but out of W66-A scope. |
| 5 | **Material drawables** (`RippleDrawable`, `StateListDrawable`, `GradientDrawable` shapes/gradients, `LayerDrawable`, `VectorDrawable` paths + tint) are not exercised by this corpus: all five apps' buttons are flat `#D6D7D7` and the only shape is the EditionText underline. | `acos-vm` gfx natives + `acos-render` | 0 measured here | The apps do not use them; they remain an unmeasured gap (see `M3_LOG.md` §4). |
| 6 | **Elevation shadows** — `render_node_set_elevation` is implemented as a single offset box-blur (`draw_node_shadow`), not Skia's two-lobe Gaussian; all corpus buttons are elevation 0 so it is not measured. | `acos-render` | 0 measured here | No elevated node in the corpus; needs a card/button with `elevation>0` in a fixture. |
| 7 | **`RenderNode.setRenderEffect` (blur)** is a structured `UNSUPPORTED_JAVA_API`. | `gfx_node.rs` | 0 measured here | Needs a node-level render-effect model (offscreen layer + blur); ADR-0023 does not cover it. |
| 8 | **Bitmap shader** exists on the native/render side (`paint_set_shader_bitmap`) but the scene format exposes only linear/radial/sweep, so it is not in the corpus. | scene format / test harness | 0 measured here | Harness gap, not a renderer gap. |
| 9 | **`sqlite-crud` ListView content** (`(no notes)`, list background/dividers) is the one app-content outlier (MAE 0.446 vs 0.1–0.2 elsewhere). | scene reconstruction | content MAE **0.446** | The ListView item metrics are approximate; a real ListView render from the VM is required to close it. |

Already covered (not gaps): raster `Canvas(bitmap)`, unclipped layers,
`nGetFillPath`, `LineBreaker` BALANCED/HIGH_QUALITY, `clipRect`
`ReverseDifference`, `drawArc`/`Path`/`RoundRect`/`Oval`/`Circle`, text
metrics/advances, underline/strikethrough, letter-spacing, linear/radial/sweep
shaders, PorterDuff/`BlendMode`, color filters. Nothing unsupported is silently
skipped: `scenes render unsupportedOps=[]` and any unsupported op is a
structured error, asserted in `tests/corpus.rs`.

## 5. Acceptance (this worktree, measured)

```
cargo test -p acos-render                      -> lib 75, contract 18, corpus 1,
                                                  golden 2, hello_views 1,
                                                  present_damage 4, reference_diff 2,
                                                  text_coverage 1, text_metrics 1,
                                                  variable_axes 2  (all ok, 0 failed)
cargo clippy -p acos-render --all-targets -D warnings -> 0 warnings
cargo test -p acos-vm --test gfx_canvas --test gfx_natives -> 5 passed
tools/diff-dexprogs.sh --corelib real          -> 27 pass / 0 fail / 0 unsupported
```

## 6. Honest caveats

* The candidates are **scene reconstructions** of each app's View hierarchy, not
  frames the fixture APK's bytecode produced. The VM still cannot drive
  `ViewRootImpl`→`HardwareRenderer` (M2/M3, see `M3_LOG.md` §3); this measures
  the renderer's geometry/text/drawable/AA pipeline in isolation.
* Most of the residual MAE is **SystemUI chrome** (ranked #1/#2) that is not part
  of the app's draw list. The app-content bands already sit at MAE 0.1–0.4.
* `canvas-shapes`'s `drawArc` is reconstructed with cubic Bézier segments because
  the scene format has no arc command; the native `nDrawArc` path is unchanged.
