# W101-A — pipeline-generated render evidence (real `View.draw` → `RecordingCanvas` → `acos-render`)

Originally a clean git worktree of HEAD `9c70fa4` (`.cache/wt-w101a`),
`CARGO_TARGET_DIR=.cache/target-w101a`, `CARGO_PROFILE_DEV_DEBUG=0`; refreshed for
W104-A on HEAD `71cec4e` (`.cache/wt-w104a`, `CARGO_TARGET_DIR=.cache/target-w104a`).

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

## 6. Acceptance (W104-A worktree, measured)

```
cargo test -p acos-vm --test render_views                       -> ok (1 passed; canvas-shapes 0.8631, counter 0.2964)
cargo test -p acos-framework-natives --test theme_resolution    -> ok (5 passed)
cargo clippy -p acos-vm -p acos-render -p acos-framework-natives --all-targets --no-deps -- -D warnings -> 0 warnings
cargo check --workspace --all-targets                           -> clean
tools/diff-dexprogs.sh --corelib real                           -> 27 pass / 0 fail / 0 unsupported
```

Nothing committed.
