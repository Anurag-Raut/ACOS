# W101-A — pipeline-generated render evidence (real `View.draw` → `RecordingCanvas` → `acos-render`)

Originally a clean git worktree of HEAD `9c70fa4` (`.cache/wt-w101a`),
`CARGO_TARGET_DIR=.cache/target-w101a`, `CARGO_PROFILE_DEV_DEBUG=0`; refreshed for
W104-A on HEAD `71cec4e` (`.cache/wt-w104a`, `CARGO_TARGET_DIR=.cache/target-w104a`).
Refreshed for W122-A on HEAD `2398fa9` (`.cache/wt-w122a`,
`CARGO_TARGET_DIR=.cache/target-w122a`).

**Motivation (W94-A finding 12 / honesty #11).** The W66/W82/W90 corpus scenes
are hand-authored reconstructions of the ART reference UI dumps, so their SSIM
measures how well the scenes were authored, not the real draw pipeline. This
wave builds the recorder path that produces the display list from the framework
itself and records, per scene, whether the candidate came from the real
pipeline or a hand-authored scene.

## 1. What landed

| Artifact | Purpose |
|---|---|
| `crates/acos-vm/tests/java/RenderViewsMain.java` | Java harness: real `AssetManager`/`Resources`/`Theme` over the fixture APK **+ `framework-res.apk`**, real `PhoneLayoutInflater.inflate`, real `View.measure`/`View.layout` at 1080×2400 @ 420 dpi, then `View.draw(RecordingCanvas)`. No `Activity`, no `ViewRootImpl`. |
| `crates/acos-vm/tests/render_views.rs` | Runs the harness on `--corelib real`, reads the frame the real `HardwareRenderer` submitted to the `FrameSink` (`gfx_hwui::last_frame`), diffs against the ART screenshot, writes `results/render/corpus/corpus-pipeline-metrics.json` and `results/render/pipeline/*.png`. |
| `tools/render-views.sh` | One-shot: pipeline frames, then the hand-authored corpus re-stamped with provenance. |
| `crates/acos-render/tests/corpus.rs` | `corpus-metrics.json` now carries a per-scene `provenance` (`pipeline-generated` when a real-pipeline frame exists for that app, else `hand-authored`) plus the pipeline `pipelineSsim`/`pipelineMae`/`pipelineStatus`. |

Reproduce:

```
tools/render-views.sh
# or
CARGO_TARGET_DIR=.cache/target-w104a CARGO_PROFILE_DEV_DEBUG=0 \
ACOS_FONT_DIR=$PWD/.cache/boot/android-34/fonts \
cargo test -p acos-vm --test render_views -- --nocapture
```

## 2. Framework-side gaps fixed (small)

| Fix | File | Effect |
|---|---|---|
| Register `Paint.nSet(JJ)V` (API 34 `nSet(long dst, long src)` copy constructor). The legacy `(J)V` form was already present. | `crates/acos-vm/src/natives/gfx_paint.rs` | `Paint(Paint)` copy no longer raises `UNSUPPORTED_JAVA_API`; unblocked the canvas-shapes draw. |
| `nGetRunCharacterAdvance` read the advances array/index from args 7/8 instead of 8/9 (`(J [C I I I I Z I [F I)F`). | `crates/acos-vm/src/natives/gfx_paint.rs` | Text measurement no longer aborts with `VM_INTERNAL: expected float[]`; a real off-by-one in the JNI argument mapping. |

## 3. Honest results (pipeline vs hand-authored)

`candidate` = real framework frame; `reference` = ART API-34 capture; scored with
`acos_render::diff` (same engine as `acos frame-diff`). Machine-readable:
`results/render/corpus/corpus-pipeline-metrics.json`.

### W104-A update (HEAD `71cec4e` worktree, `.cache/target-w104a`)

| app | pipeline status | pipeline SSIM | hand-authored scene SSIM | provenance stamped |
|---|---|---:|---:|---|
| canvas-shapes | frame produced | **0.8631** (was 0.0212) | 0.9953 | pipeline-generated |
| counter | frame produced | **0.2964** (was inflation-failed) | 0.9944 | pipeline-generated |
| form-validation | inflation failed (`EditText` NPE, no `Application`) | — | 0.9924 | hand-authored |
| text-input | inflation failed (`EditText` NPE, no `Application`) | — | 0.9906 | hand-authored |
| widget-gallery | inflation failed at line #19 (`StateListAnimator` → `Choreographer.getRefreshRate`, no display service) | — | 0.9700 | hand-authored |

`canvas-shapes` masked SSIM is **0.8949**; the residual is the pipeline frame's
text glyph coverage and `ShapeView` anti-aliasing, not missing geometry. The
`View.draw` hierarchy recording is no longer lossy (the `ShapeView`
`drawColor`/rect/circle/line/path/arc ops now reach the surface); the W104-A root
cause was `Canvas.save()` returning the **post-save** stack depth, which made
`View.draw`'s `restoreToCount(saveCount)` a no-op and leaked the first child's
clip into every later op. Returning `c.stack.len()` (Skia semantics) restored the
tree.

`counter` is now a produced frame, but its SSIM stays low because the harness
inflates the layout alone while the ART reference includes the activity
`windowBackground`; the harness now draws the theme `windowBackground` through
the real drawable (raising counter 0.0276 → 0.2964 and canvas-shapes
0.8203 → 0.8631), but the remaining gap is window/`DecorView` composition, not
natives.

### W122-A update (HEAD `2398fa9` worktree, `.cache/target-w122a`)

| app | pipeline status | pipeline SSIM | masked SSIM | hand-authored SSIM | provenance |
|---|---|---:|---:|---:|---|
| canvas-shapes | frame produced | 0.8305 | **0.8949** | 0.9953 | pipeline-generated |
| counter | frame produced | **0.8783** (was 0.2964) | **0.9425** (was 0.3214) | 0.9944 | pipeline-generated |
| form-validation | inflation failed (`EditText` NPE, no `Application`) | — | — | 0.9924 | hand-authored |
| text-input | inflation failed (`EditText` NPE, no `Application`) | — | — | 0.9906 | hand-authored |
| widget-gallery | inflation failed at line #19 (`StateListAnimator` → `Choreographer`, no display service) | — | — | 0.9700 | hand-authored |

Two principled framework/harness fixes drove the counter jump:

| Fix | File | Effect |
|---|---|---|
| The harness applied the **dark** `Theme.DeviceDefault`, but the ART oracle was captured in light UI mode (`device-config.json`: `"dark_light": "no"`), so every frame's window background was `#191C1E` instead of the light device default. When the manifest declares no `android:theme`, `deviceDefaultTheme()` now applies `Theme.DeviceDefault_Light_DarkActionBar`. | `crates/acos-vm/tests/java/RenderViewsMain.java` | counter window background now matches the reference exactly (`#F0F0F3`); mean SSIM 0.2964 → **0.8783**. |
| Registered `Canvas.nDrawTextRun` (both API-34 signatures: `(JLjava/lang/String;IIIIFFZJ)V` and `(J[CIIIIFFZJJ)V`). `Layout`/`TextLine` reach the canvas through this entry point, which had no native at all. | `crates/acos-vm/src/natives/gfx_canvas.rs` | latent gap closed; not yet exercised by the counter/TextView path below. |

`canvas-shapes` dips 0.8631 → 0.8305 because its ART reference also has a dark
status/action bar, and the light theme (correct for the light-content frame) now
shows there. Its SystemUI-masked SSIM is unchanged at **0.8949**. The remaining
counter delta is the `DecorView` composition: the harness inflates the layout
alone, so the status bar and the 147 px action bar (title `Counter`) are still
not drawn, and the content is not offset by the 275 px decor height. That is a
real gap (build the `DecorView`/`PhoneWindow` and draw its children), not a
native bug, and deliberately not hand-painted so the frame stays honest.

Text draw gap found while diffing counter: its `TextView`/`Button` widgets emit
**no** `DrawText` op (`RecordingCanvas.finishRecording` ops=38, text=0) even
though `getText()` and `textColor` are set and the equal-geometry
`canvas-shapes` status `TextView` does emit `nDrawText`. This is the dominant
residual for counter and is unsolved.

### W129-A update (HEAD `b7eb230` worktree, `.cache/wt-w129a`)

| app | pipeline status | pipeline SSIM | masked SSIM | hand-authored SSIM | provenance |
|---|---|---:|---:|---:|---|
| canvas-shapes | frame produced | 0.8305 | **0.8949** | 0.9953 | pipeline-generated |
| counter | frame produced | **0.8782** | **0.9424** | 0.9944 | pipeline-generated |
| form-validation | inflation failed (`EditText` NPE, no `Application`) | — | — | 0.9924 | hand-authored |
| text-input | inflation failed (`EditText` NPE, no `Application`) | — | — | 0.9906 | hand-authored |
| widget-gallery | inflation failed at line #19 (`StateListAnimator` → `Choreographer`, no display service) | — | — | 0.9700 | hand-authored |

**Root cause of the zero-`DrawText` gap (fixed).** `Canvas.getClipBounds` and
the tracked clip were kept in **device/root** coordinates, but the framework
passes clip rectangles in the canvas's **current local** coordinate space. For
a `TextView` reached through a translated parent (`View.draw(canvas, parent,
long)` translates by `mLeft, mTop`), `ViewGroup.drawChild` clips to the child's
local `[0,0,w,h]`; intersecting that with the device-space padding clip produced
the nonsensical `[42,42,9,18]` (`right < left`, `bottom < top`).
`Layout.getLineRangeForDraw` then saw an empty clip and returned `lastLine < 0`,
so `TextView.onDraw` → `Layout.draw` produced **zero** text ops. `canvas-shapes`
was unaffected because its `TextView` sits at `(0,0)` (identity translation) and
its text op came from `ShapeView`'s direct `Canvas.drawText`, not from a
`Layout`.

| Fix | File | Effect |
|---|---|---|
| `nClipRect` now maps the local rect through the current matrix before intersecting the device-space tracked clip; `nGetClipBounds`, `nQuickReject`, `nDrawPaint`, and `nSaveUnclippedLayer` map accordingly. | `crates/acos-vm/src/natives/gfx_canvas.rs` | counter's three `TextView`/`Button` labels now record `DrawText` ops (`finishRecording ops=41 text=3`, was `ops=38 text=0`); `canvas-shapes` stays `text=2`. |
| The harness `DisplayMetrics` now sets `scaledDensity`/`xdpi`/`ydpi`/`widthPixels`/`heightPixels` to the device values. | `crates/acos-vm/tests/java/RenderViewsMain.java` | honest `DisplayMetrics` (was leaving `scaledDensity` at its default); no SSIM change on its own. |

**Why SSIM did not move: text is drawn but at 15 px, not 36.75 px.** The
harness now records the counter labels, but `TextView.getTextSize()` is `15.0`
(its constructor default, in px) instead of the theme's `14sp` = `36.75` at
density `2.625`. The theme's `?attr/textAppearanceDeviceDefault` (a **style
bag**) is resolved by `AssetManager` to `TYPE_NULL` rather than kept as a
`TYPE_REFERENCE` to the bag, so `TypedArray.getResourceId` returns `-1` and
`TextView.applyTextAppearance` never runs. That is a resource-resolution gap
(`resolve_references` chasing a bag into its empty `Res_value`), not a canvas
gap; a local fix to return the bag reference was attempted and correctly failed
`ColorStateList`/`GradientDrawable` inflation, so it was reverted rather than
landed half-working. This is the dominant remaining counter/canvas-shapes
residual and is recorded here honestly.

## 4. W104-A framework-side fixes

| Fix | File | Effect |
|---|---|---|
| `Canvas.save`/`saveLayer` returned `stack.len() + 1`; now returns the post-save depth (`stack.len()`). | `crates/acos-vm/src/natives/gfx_canvas.rs` | `restoreToCount` works; child clip no longer leaks; canvas-shapes SSIM 0.0212 → 0.8203. |
| `resolve_references_with` materialises a `@null` (`TYPE_REFERENCE`, `data = 0`) entry as `TYPE_NULL`. | `crates/acos-framework-natives/src/res/manager.rs` | `ScrollView` inflation no longer throws `NotFoundException: config_scrollbarTrackVertical is not a Drawable`. Regression: `null_reference_entry_resolves_to_type_null`. |
| `android.graphics.Interpolator` native surface (`nativeConstructor`, `nativeDestructor`, `nativeReset`, `nativeSetKeyFrame`, `nativeSetRepeatMirror`, `nativeTimeToValues`). | `crates/acos-vm/src/natives/fwk_graphics.rs` | `widget-gallery` gets past `Interpolator.nativeConstructor`. |
| `android.graphics.ImageDecoder` native surface (`nCreate` ×2, `nDecodeBitmap`, `nGetSampledSize`, `nGetMimeType`, `nGetColorSpace`, `nGetPadding`, `nClose`) + `BitmapFactory.nativeDecodeStream`/`nativeDecodeByteArray`, backed by `acos_render::bitmap_create_from_bytes`. | `crates/acos-vm/src/natives/gfx_bitmap.rs` | `PhoneWindow.installDecor`/`getDefaultActivityIcon` decodes; `counter.apk` reaches `activity_on_create`. |
| `AtomicBoolean` natives (`get`/`set`/`getAndSet`/`compareAndSet`/plain/opaque). | `crates/acos-vm/src/natives/vmrt.rs` | `ImageDecoder.close()`'s `invoke-polymorphic AtomicBoolean.compareAndSet` no longer hits unimplemented opcode `0xfa`. |
| `SurfaceControl.nativeCopyFromSurfaceControl(J)J` clones the layer record. | `crates/acos-vm/src/natives/fwk_surface.rs` | `InsetsSourceControl` construction from `ViewRootImpl.dispatchInsetsControlChanged`. |
| Harness draws the theme `windowBackground` before `root.draw`, through the real drawable. | `crates/acos-vm/tests/java/RenderViewsMain.java` | Matches `DecorView` composition; counter/canvas-shapes SSIM. |

## 5. Blockers (ranked) with owner

1. **`EditText` inflation needs an `Application`** (dominant). `SelectionActionModeHelper.<init>` → `TextClassificationConstants.isSmartSelectionAnimationEnabled` → `DeviceConfig.getBoolean` → `Settings$Config.getStrings` → `Settings$Config.getContentResolver` → `ActivityThread.currentApplication()` is null, and `TestContext.getContentResolver()` is null, so the NPE propagates. The VM-level harness has no `ActivityThread`; a real launch does. Fix belongs in the harness (stub `Application` + `ContentResolver`) or by booting a minimal `ActivityThread`.
2. **`widget-gallery` needs a display service.** A disabled `Button`'s `StateListAnimator` constructs `Choreographer`, whose `getRefreshRate` dereferences a null `DisplayInfo` (`NoServices` answers every binder lookup null). Fix: answer `IDisplayManager.getDisplayInfo` in the test transport, or wire the `acos-runtime` display service into the VM-level harness.
3. **`BitmapFactory` natives still missing**: `nativeDecodeAsset`, `nativeDecodeFileDescriptor`, `nativeIsSeekable`, `nativeColorSpace`, `nativeInBitmap`, and the `BitmapRegionDecoder` surface. The `ImageDecoder` `FileDescriptor`/`ByteBuffer`/`J`-ptr `nCreate` overloads return `null`; only stream + `byte[]` decode today.
4. **`invoke-polymorphic`/`invoke-custom` (`0xfa..=0xfd`)** remain unimplemented in `interp.rs`; worked around for `AtomicInteger`/`AtomicLong`/`AtomicBoolean` only.
5. **`acos launch` next blocker** (`counter.apk`, `ACOS_BOOT_DIR=.cache/boot/android-34`): `java.lang.IllegalStateException: Main thread not allowed to quit` at `MessageQueue.next` ← `Looper.loop`, blocking `on_resume`/`first_frame`.
6. **`DecorView`/status/action bar are not composed** in the VM-level harness (W122-A, still open in W129-A). The layout is measured at the full 2400 px instead of the 275 px decor offset, and the status bar + action bar are absent from every pipeline frame. Fix: instantiate the real `com.android.internal.policy.PhoneWindow` (with a `Window.Callback` stub) and draw `getDecorView()`, or add a VM-level harness mode that does. Owner: `render_views.rs` / `RenderViewsMain.java`.
7. **Text draws at 15 px instead of the theme's 14sp** (W129-A, narrowed). The zero-`DrawText` clip bug is fixed (item 7 of W122-A); the labels are now recorded, but `TextView.getTextSize()` is the 15 px constructor default because `?attr/textAppearanceDeviceDefault` resolves to a bag that `AssetManager` reports as `TYPE_NULL`. Fix: return a `TYPE_REFERENCE` to the bag from `resolve_references` (AOSP `ResolveReference` semantics) without breaking `ColorStateList`/`GradientDrawable` inflation. Owner: `acos-framework-natives/src/res/{manager,theme}.rs`.
8. **`TextView`/`Button` text emits no op on the `counter`/`widget` path** (W122-A) — **FIXED** in W129-A: `Canvas.getClipBounds`/clip tracking were in device coordinates while the framework passes local rectangles. `counter` now records `ops=41 text=3`.

## 6. Acceptance (W129-A worktree, measured)

```
cargo test -p acos-vm --test render_views                       -> ok (1 passed; canvas-shapes 0.8305, counter 0.8782)
cargo clippy -p acos-vm -p acos-render -p acos-framework-natives --all-targets --no-deps -- -D warnings -> 0 warnings
cargo check --workspace --all-targets                           -> clean
tools/diff-dexprogs.sh --corelib real                           -> see results/vm/dexprogs-real-normal.json
```

W129-A scope: `crates/acos-vm/tests/{render_views.rs,java/RenderViewsMain.java}`,
`crates/acos-vm/src/natives/gfx_*`, `crates/acos-render`. Items (b) `DecorView`
composition, (c) `EditText` `Application`, and (d) `IDisplayManager` reply for
`widget-gallery` were **not** landed in this wave.

Nothing committed.
