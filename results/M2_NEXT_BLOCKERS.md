# M2 next blockers — after the VectorDrawable tree

Base: clean worktree of HEAD `9519f11` at `.cache/wt-w105a`
(`CARGO_TARGET_DIR=.cache/target-w105a`, `CARGO_PROFILE_DEV_DEBUG=0`).
This is offline analysis plus codec/handler work; it does not commit and it does
not touch `lifecycle.rs`, `session_pool.rs`/boot cache, or `natives/mod.rs` /
`gfx_*` (owned by W68-A/W104-A).

## Current position

`M2_LOG.md` §W65-A: the launch transaction reaches
`ActivityThread.performLaunchActivity` -> `Activity.attach` -> `PhoneWindow`
-> `DecorView.onResourcesLoaded` -> `screen_toolbar` -> `ToolbarWidgetWrapper`,
which inflates a `VectorDrawable` and blocks at
`android.graphics.drawable.VectorDrawable;->nCreateGroup()J` (W68-A owns the
native tree). Everything before that point (`bindApplication`,
`Application.onCreate`, the launch `ClientTransaction`, `TaskDescription`,
`@null` theme attrs) is green.

The ranking below assumes W68-A lands the VectorDrawable peers. It orders the
blockers the **first frame** will hit, by "distance from the current block ×
how much is already in place".

## Ranked next blockers

| # | Blocker | Where it bites | Why it is next | State / owner |
|---|---|---|---|---|
| 1 | **`ViewRootImpl.setView` → `IWindowSession.addToDisplayAsUser` reply decode** | `ViewRootImpl.setView` after `MainActivity.onCreate` → `setContentView` returns | The first WMS round trip. The request is `LayoutParams` + 4 ints; the reply is `int result`, `InputChannel`, `InsetsState`, `InsetsSourceControl[]`, `Rect`, `float[]`. The Rust handlers already emit this exact byte layout. | `crates/acos-runtime/src/wms_java.rs` + `acos-wm/src/codec.rs`; byte-layout test green. The Java `InputChannel.nativeReadFromParcel` path is implemented in `fwk_input`; the WMS Java-stub test still reports `pending-input-channel` only when the stub class path is unavailable. |
| 2 | **`relayout` reply decode (`ClientWindowFrames`, `MergedConfiguration`, `SurfaceControl`, `InsetsState`, `InsetsSourceControl[]`, `Bundle`)** | `ViewRootImpl.relayoutWindow` → `performTraversals` | The second WMS round trip. The `MergedConfiguration` carries the real 27-field `Configuration`; the `SurfaceControl` carries the native layer handle. | `acos-wm/src/codec.rs` already has the full `Configuration`/`MergedConfiguration`/`ClientWindowFrames`/`InsetsState`/`InsetsSourceControl` codecs (derived from DEX); `wms_java` emits the reply. The only gap is the `Bundle` trailing field (written as null). |
| 3 | **`InputChannel`/`InputEventReceiver` integration** | `ViewRootImpl.setView` installs the input channel; the app's `InputEventReceiver` starts reading | `addToDisplayAsUser` returns a real socketpair fd. The native read/write pair (`fwk_input::ic_read_from_parcel`/`ic_write_to_parcel`) exists; the WMS test’s Java stub path still needs the real `InputChannel` class on the test classpath. | `crates/acos-vm/src/natives/fwk_input.rs`, `crates/acos-runtime/src/input_java.rs`. |
| 4 | **`ViewRootImpl.performTraversals` → `measure`/`layout`/`draw`** | After `setView`/`relayout`, the first `performTraversals` calls `View.measure`, `View.layout`, `View.draw` | The next large surface after the VectorDrawable peers: the framework's own `View`/`ViewGroup` measurement and the `Canvas`/`RenderNode`/`GLES` draw path. The `acos-render` compositor and `acos-gles` are in place; the framework `Canvas` natives are partly stubbed. | `crates/acos-render`, `crates/acos-gles`, `crates/acos-vm/src/natives/fwk_graphics.rs`; the `Canvas` drawing natives are `UNSUPPORTED_JAVA_API` in places. |
| 5 | **`finishDrawing` → `SurfaceControl.Transaction` → compositor present** | `ViewRootImpl.reportDrawFinished` → `finishDrawing(IWindow, SurfaceControl$Transaction, seq)` | The frame is only published on a real buffer submission. The WMS records `finishDrawing` but never publishes a frame (advisor rule); the app's `Surface`/`SurfaceControl` transaction must submit a buffer. | `wms_java.rs` `finishDrawing` handler; `WmSurfaceBackend` publishes on `SurfaceOp::Buffer`. The remaining gap is the app-side `Surface.lockCanvas`/`unlockCanvasAndPost` native path. |
| 6 | **`onResume` / `onPostResume` callbacks** | `ActivityThread.handleResumeActivity` after `performTraversals` | The lifecycle gate (`m2_lifecycle_create_start_resume_matches_reference`) is still `#[ignore]`d; `onResume` is not reached until `onCreate`/`setContentView`/first traversal complete. | `crates/acos-runtime/tests/m2.rs`; the gate is the acceptance target. |
| 7 | **`WindowInsets`/`InsetsState` consumer** | `ViewRootImpl` reads `InsetsState` from `relayout` and computes `WindowInsets` | The reply carries the ten reference `InsetsSource` rows (status bar 128 px, nav bar 63 px, IME `[0,2274][1080,2400]`, cutout 128 px). The `InsetsState` codec is complete; the framework's `InsetsState.calculateInsets` runs on the real bytecode. | `acos-wm/src/codec.rs` `InsetsState`, `wms.rs` `reference_insets_state`; no Rust-side gap. |
| 8 | **`Configuration`/`MergedConfiguration` consumer** | `ViewRootImpl`/`Resources` reads the merged configuration from `relayout` | The `Configuration` codec now models the real 27-field order (see below). The `MergedConfiguration` writes global/override/merged. | `parcelable.rs` + `acos-wm/src/codec.rs`; `Configuration` is `Prefix 22/27` because the static scanner counts both `if/else` branches of `userSetLocale` as separate `writeInt`s. |
| 9 | **`LayoutParams`/`SurfaceControl` argument decode** | `addToDisplayAsUser`/`relayout` request | The request `LayoutParams` codec is derived from DEX; the `SurfaceControl` request field is the native handle. | `acos-wm/src/codec.rs` `LayoutParams`/`SurfaceControlInfo`; `wms_java` decodes both. |
| 10 | **`ResultInfo`/`PersistableBundle`/`ProfilerInfo`** | `LaunchActivityItem`/`ResumeActivityItem` extras | These are carried as null on the V1 launch path; the `ProfilerInfo` codec now models the real `writeString`/`writeInt`/nested `ParcelFileDescriptor` order (`Prefix 2/9` because the nested write is a direct `writeToParcel`). | `parcelable.rs`; no spec for `ResultInfo`/`PersistableBundle` yet (they are null on the launch path). |

## Parcelable/AIDL codec diff (real API-34 `writeToParcel`)

`cargo test -p acos-services --test parcelable -- --nocapture` diffs every spec
against `.cache/boot/android-34/dex/framework/classes{1..5}.dex`. The scanner
now records nested `writeToParcel` calls as a `writeParcelable` op so a
`FieldOp::Nested` can verify its position. Current result (39 specs, 7 full):

| class | verification | matched/spec | real ops | change |
|---|---|---:|---:|---|
| `android.content.res.Configuration` | Prefix | 22/27 | 28 | **rewritten** to the real order: `fontScale, mcc, mnc, mLocaleList (writeTypedObject), userSetLocale, touchscreen…compatSmallestScreenWidthDp, windowConfiguration.writeToParcel, assetsSeq, seq, fontWeightAdjustment, mGrammaticalGender`. The `if/else` on `userSetLocale` is one write at runtime but two static branches; the scanner sees 28. |
| `android.app.WindowConfiguration` | Prefix | 9/9 | 9 | **new**: `mBounds.writeToParcel`, `writeTypedObject(mAppBounds)`, `mMaxBounds.writeToParcel`, 6 ints. |
| `android.os.LocaleList` | Full | 1/1 | 1 | **new**: single `writeString8`. |
| `android.window.SizeConfigurationBuckets` | Full | 5/5 | 5 | **rewritten**: `writeByte` + 4 × `writeIntArray`. |
| `android.app.servertransaction.ClientTransaction` | Full | 5/5 | 5 | **rewritten**: `writeBoolean`, `writeStrongBinder`, `writeParcelable`, `writeBoolean`, `writeParcelableList`. |
| `android.content.res.CompatibilityInfo` | Full | 4/4 | 4 | **rewritten**: `int, int, float, float`. |
| `android.app.ProfilerInfo` | Prefix | 2/9 | 10 | **rewritten**: `writeString`, presence marker + nested `ParcelFileDescriptor.writeToParcel`, 4 ints, `writeString`, `writeBoolean`, `writeInt`. The nested write is a direct `writeToParcel`, so the scanner cannot consume it with the `Cond` marker; `Prefix 2/9` is the honest coverage. |
| `android.app.ActivityManager$TaskDescription` | Curated | 2/13 | 19 | W65-A conditional layout; the scanner only sees the three presence markers (the icon branch is `Unsupported`). |
| `android.view.DisplayInfo` | Prefix | 21/21 | 51 | leading scalars match; the nested `DisplayCutout`/`DisplayShape`/`Mode[]` are placeholders. |
| `android.content.pm.ApplicationInfo` | Prefix | 14/33 | 84 | leading fields match; `maybeWriteSquashed` + trailing `SparseArray`/`Parcelling` are gaps. |
| `android.content.pm.ActivityInfo` | Prefix | 9/23 | 45 | ComponentInfo prefix + ActivityInfo fields; the trailing `ActivityInfo` fields are a gap. |
| `android.content.pm.PackageInfo` | Prefix | 10/21 | 45 | leading fields match; arrays are placeholders. |
| `android.content.pm.ProviderInfo` | Prefix | 7/14 | 28 | leading fields match; the wire copy is Java-bridged in `ams_java`. |
| `android.view.InputDevice` | Curated | 0/15 | 28 | not on the launch path; the real layout has motion ranges and a `String8` descriptor. |
| `android.os.ParcelFileDescriptor` | Curated | 0/2 | 6 | the real layout is a presence marker + fd pair; the V1 codec models the no-`mCommFd` case. |
| `android.content.AttributionSource` | Curated | 0/8 | 1 | the real `writeToParcel` delegates to `AttributionSourceState`; the curated spec is the service-side value. |
| `android.app.ActivityManager$RunningAppProcessInfo` | Curated | 0/2 | 16 | not on the launch path. |

### What the fix buys

* `Configuration`/`MergedConfiguration`/`SizeConfigurationBuckets` now encode
  the same bytes the API-34 framework reads. The `relayout` and
  `reportSizeConfigurations` replies no longer desynchronise the parcel.
* `ClientTransaction` now matches the real `writeToParcel` (the V1 `atms`
  helper still carries a curated `IntArray`; the real launch path builds the
  Java object through `javabridge`, so the codec is only for the AIDL wire).
* `CompatibilityInfo` now round-trips the real four fields (the previous
  one-int spec would have truncated the `float` scale).
* The scanner now records nested `writeToParcel` calls; the `Configuration`
  spec's `windowConfiguration` field is visible to the verifier.

### Remaining codec gaps (honest)

* The scanner counts both branches of `if/else` writes (`Configuration.userSetLocale`,
  `ProfilerInfo.profileFd`), so those specs report `Prefix` even though the
  runtime order is correct.
* `TaskDescription`'s icon branch is `Unsupported` (an `Icon` is not on the V1
  launch path).
* `ResultInfo`, `PersistableBundle` and `WindowInsets` have no spec yet; they are
  carried as null on the launch path.

## WindowManager/Session handlers surveyed from the real bytecode

`crates/acos-wm/examples/scan_viewroot` (temporary offline scan) lists the
`IWindowSession`/`IWindowManager` call sites in
`Landroid/view/ViewRootImpl;` (`classes4.dex`). The handlers below are the
`ViewRootImpl.setView`/`performTraversals`/`draw` surface:

| call site | method | reply | handler |
|---|---|---|---|
| `setView` | `addToDisplayAsUser(IWindow, LayoutParams, int, int, int, int, InputChannel, InsetsState, InsetsSourceControl[], Rect, float[]) → int` | `int, InputChannel?, InsetsState?, InsetsSourceControl[]?, Rect?, float[]` | `handle_session` `addToDisplayAsUser` (real 1080×2400@420 frame, insets, fd-backed `InputChannel`) |
| `relayoutWindow` | `relayout(...) → int` | `int, ClientWindowFrames?, MergedConfiguration?, SurfaceControl?, InsetsState?, InsetsSourceControl[]?, Bundle?` | `handle_session` `relayout` |
| `relayoutWindow` (async branch) | `relayoutAsync(...) → void` | — | **new** oneway arm: runs the same `relayout`, drops the reply |
| `reportDrawFinished`/`doDie` | `finishDrawing(IWindow, SurfaceControl$Transaction, int) → void` | — | `handle_session` `finishDrawing` |
| `performTraversals` | `setInsets(...)`, `clearTouchableRegion(IWindow)` | — | `setInsets`; **new** `clearTouchableRegion` no-op |
| `performTraversals` | `cancelDraw(IWindow) → boolean` | `bool` | **new** `cancelDraw` returns `false` |
| `dispatchFocusEvent`/`handleOutOfResourcesException` | `outOfMemory(IWindow) → boolean` | `bool` | **new** `outOfMemory` returns `false` |
| `performHapticFeedback` | `performHapticFeedbackAsync(int, boolean)` | — | **new** oneway no-op |
| `reportKeepClearAreasChanged` | `reportKeepClearAreasChanged(IWindow, List, List)` | — | **new** oneway no-op |
| `handleDragEvent` | `dragRecipientEntered`/`dragRecipientExited`/`reportDropResult` | — | **new** oneway no-ops |
| `registerBackCallbackOnWindow` | `attachToWindow` (not on `IWindowSession`) | — | n/a |
| `requestChildRectangleOnScreen` | `onRectangleOnScreenRequested(IBinder, Rect)` | — | existing |
| `systemGestureExclusionChanged` | `reportSystemGestureExclusionChanged(IWindow, List)` | — | existing |
| `pokeDrawLockIfNeeded` | `pokeDrawLock(IBinder)` | — | existing |
| `isInTouchMode` | `IWindowManager.isInTouchMode(int) → boolean` | `bool` | existing |
| `ensureTouchMode` | `IWindowManager.setInTouchMode(boolean, int)` | — | existing |

The handlers return faithful values for the 1080×2400@420 reference display and
raise a structured `UNSUPPORTED` for anything else. The per-handler unit test is
`crates/acos-runtime/tests/wms.rs::viewrootimpl_handlers_return_faithful_scalars`
(plus the existing `add_to_display_as_user_*`/`relayout_*`/`finish_drawing_*`
tests).

## Acceptance evidence

```text
$ cargo test -p acos-services --test parcelable -- --nocapture
  4 passed; 0 failed  (39 specs, 7 full; Configuration Prefix 22/27,
                       SizeConfigurationBuckets Full 5/5,
                       ClientTransaction Full 5/5,
                       CompatibilityInfo Full 4/4,
                       WindowConfiguration Prefix 9/9,
                       LocaleList Full 1/1)

$ cargo test -p acos-services
  all pass (including replay with the real 5-field SizeConfigurationBuckets)

$ cargo test -p acos-wm
  75 tests pass

$ cargo test -p acos-runtime --test wms
  8 passed; 0 failed (new viewrootimpl_handlers_return_faithful_scalars)

$ cargo check --workspace --all-targets
  clean

$ cargo clippy --workspace --all-targets -- -D warnings
  clean

$ tools/diff-dexprogs.sh --corelib real
  dexprogs: 27 pass / 0 fail / 0 unsupported of 27
```

## Handoff / ordering

1. **W68-A** lands `VectorDrawable.nCreateGroup` + the rest of the vector tree.
2. **WMS `addToDisplayAsUser`** is already encoded; the next failure will be the
   app-side `InputChannel`/`InsetsState` read or `relayout` reply decode. If the
   failure names a parcelable class not in the table above, add a spec and a
   round-trip sample first.
3. **`performTraversals`/`draw`** is the large surface after the WMS round trips:
   the framework `Canvas`/`RenderNode`/`GLES` natives are the likely next
   `UNSUPPORTED_JAVA_API`.
4. **`finishDrawing`/`SurfaceControl`** is where the first frame is published;
   the WMS records it but the compositor only publishes on a real buffer.
5. **`onResume`** is the M2 gate; it stays `#[ignore]`d until steps 1–4 complete.
