# A11YNATIVE2 — a11y-probe green on ART: screenshot, gestures, label case

Branch `a11ynative2` (from `a11ynative` @ `511a74f0`, which is `modules` @ `7c4317ab`
+ the A11YNATIVE work), worktree `.t3/worktrees/acos/t3-04d8599f-aa13af09980146e7`,
2026-11-16. `modules` was merged again at the end (`bcc5f8db`, BINDERREF2), and the
probe was re-run on the merged tree — that run is the evidence below.

## Status

**DONE for the four briefed items.** `scenarios/a11y-probe.yaml` **passes on ART
end to end (15/15 steps)**, on the merged tree, and `cargo test -p acos-services
-p acos-wm` is green. One item is closed **with a wall** (`takeScreenshot`'s
app-side *pixel* read, §4): everything the framework can hand an app is asserted
in the guest, and the missing piece is a GPU, not a wire format.

| Item | State |
|---|---|
| 1. `takeScreenshot` guest delivery (success + codes 3/4/6) | **DONE** — three wire bugs found and fixed; the app's own `TakeScreenshotCallback` runs |
| 2. swipe scrolls the `ScrollView` | **DONE at the cause** (display refresh rate); residual pacing gap documented in §3 |
| 3. text case (`getText` vs drawn label) | **DONE** — both APIs are correct and identical here; exact-case assertions restored |
| 4. merge `modules`, re-run | **DONE** — merged (`bcc5f8db`), probe passes on the merged tree |

Evidence: `results/handoff/A11YNATIVE2/merged-tree-run/` (the post-merge ART run)
and `results/handoff/A11YNATIVE2/host-trace.log` (the host's `ACOS_TRACE_FILE`
of the same run).

```
merged-tree-run/verdict.json      "status": "pass", 15 steps, fidelity art-reference
merged-tree-run/logs/logcat.txt
  I AcosA11yProbe: display id=0 refresh_rate=60.000004 mode_id=1 mode_rate=60.000004 modes=1
  I AcosA11yProbe: button_text a11y=Click target drawn=Click target
  I AcosA11yProbe: swatch bounds=[277,867][802,1077]
  I AcosA11yProbe: list_scrolled y=11
  I AcosA11yProbe: screenshot_result kind=capture status=0 width=1080 height=2400 format=1 usage=257 color_space=sRGB IEC61966-2.1 (Linear) timestamp=29715854
  I AcosA11yProbe: screenshot_result kind=interval status=3
  I AcosA11yProbe: screenshot_result kind=display status=4
  I AcosA11yProbe: screenshot_result kind=secure status=6
  I AcosA11yProbe: screenshot_pixel unavailable=hardware_bitmap_needs_egl   (see §4)
host-trace.log
  ACOSA11Y takeScreenshot enter / "takeScreenshot success display=0 1080x2400 stride=1080"
  ACOSA11Y dispatchGesture seq=2 t=0 action=0 at=(540.0,1991.0)
  ACOSA11Y dispatchGesture seq=2 t=16..192 action=2 at=(540.0,1959.0)…(540.0,1607.0)
  ACOSA11Y dispatchGesture seq=2 t=200 action=1 at=(540.0,1591.0)
```

`merged-tree-run/art-frames/unsupported.jsonl` holds **no**
`REVERSE_DELIVERY_EXCEPTION` any more (the pre-fix runs recorded
`-Bundle length is not aligned by 4: …` on every `sendResult`).

## 1. `takeScreenshot`: why the callback never ran — three independent wire bugs

A11YNATIVE's README called this the wall and guessed a missing
`register_android_hardware_HardwareBuffer`. The registration was **already
correct** (it is in both `SessionMain.stepRuntimeExtras` and
`LaunchMain.registerNatives`, and the guest dex is rebuilt from source on every
launch). The real reasons the guest never ran `onSuccess`/`onFailure` were in the
host's bytes, and they were stacked, so each one hid the next:

1. **`Bundle` magic, swapped halves.** `a11y_native` carried
   `BUNDLE_MAGIC = 0x4e44_4c42`; the framework's is `0x4c44_4e42` (`"BNDL"`,
   one definition, now `parcelable::BUNDLE_MAGIC`). `BaseBundle
   .readFromParcelInner` reads that word as the **payload length**, so the guest
   threw `-Bundle length is not aligned by 4: 1313098818` (exactly
   `0x4e444c42`) and the exception came back to the host as
   `REVERSE_DELIVERY_EXCEPTION` in `art-frames/unsupported.jsonl`.
2. **`Parcel.writeValue` tags, invented.** The module used
   `VAL_INTEGER = 0`, `VAL_LONG = 1`, `VAL_PARCELABLE = 2`; the image's
   `Parcel.getValueType` says **1 / 6 / 4** (`VAL_STRING` is 0, `VAL_MAP` is 2).
   On top of that `VAL_PARCELABLE` is **length-prefixed**: the image's
   `Parcel.isLengthPrefixed` sparse-switch keys, decoded from
   `framework/classes3.dex` (`Landroid/os/Parcel;->isLengthPrefixed(I)Z`,
   code item at `0x202b20`), are `2, 4, 11, 12, 16, 17, 21` = `VAL_MAP`,
   `VAL_PARCELABLE`, `VAL_LIST`, `VAL_SPARSEARRAY`, `VAL_PARCELABLEARRAY`,
   `VAL_OBJECTARRAY`, `VAL_SERIALIZABLE`. So each parcelable entry is
   `tag(4)`, a **value length**, then `writeParcelable`'s class name + payload.
3. **The AIDL presence word.** `IRemoteCallback.sendResult(in Bundle)` is a
   typed-object argument: the image's `IRemoteCallback$Stub.onTransact` (code 1)
   reads it with `Parcel.readTypedObject(Bundle.CREATOR)` and then
   `enforceNoDataAvail()`, i.e. the payload is `interface token`, **`int 1`**,
   `Bundle.writeToParcel`. Without that word the framework took the Bundle's
   length as the flag and its magic as the length — the *same* `-Bundle length is
   not aligned by 4` message, now with the *correct* magic `1279544898 =
   0x4c444e42`, which is how the second bug was identified.

All three now go through one place each: `parcelable::BUNDLE_MAGIC`,
`parcelable::val`, `a11y_native::begin_send_result` +
`write_parcelable_value`. Byte layout of the success payload (after the token):

```
1                                  presence (readTypedObject)
<len><BNDL><4>                     Bundle header: payload length, magic, count
  "screenshot_status"  VAL_INTEGER(1) 0
  "screenshot_colorSpace"  VAL_PARCELABLE(4) <len> "android.graphics.ParcelableColorSpace" 1
  "screenshot_hardwareBuffer"  VAL_PARCELABLE(4) <len> "android.hardware.HardwareBuffer"
        [88][1][GB01][w][h][stride][format][layers][usageLo][idHi][idLo][gen][1][9][usageHi]
        [w][h][stride][format][usageLo][usageHi][layers][bpp][dataspace] <fd>
  "screenshot_timestamp"  VAL_LONG(6) <i64>      (entries in ArrayMap = ascending Java hash order)
```

Teeth: `crates/acos-services/tests/a11ynative.rs` (12 tests) now decodes the
failure `Bundle` with `parcelable::read_bundle` — **the framework's own reader**,
which fails on the old magic with `bad Bundle magic 0x4e444c42` — and asserts the
tags, the two parcelable value lengths and the flattened `GraphicBuffer` body
(`[88][1]`, `'GB01'`, 13 header words, 1 fd, 9 handle ints, single fd in the
parcel).

## 2. `dispatchGesture`: the swipe now produces MOVEs — the guest reported `0 Hz`

The gesture conversion was right (A11YNATIVE's decoder, `GestureStep ->
DOWN/MOVE/UP`); the **guest** was producing a two-step stroke. Root cause, read
out of the image's own bytecode:

* `AccessibilityService.calculateGestureSampleTimeMs` is
  `(int)(1000 / Display.getRefreshRate())` for `targetSdk > Q`;
* `DisplayInfo.getRefreshRate()` returns `refreshRateOverride > 0 ? it :
  (supportedModes.length == 0 ? 0 : getMode().getRefreshRate())` — the *mode*
  array is what it reads, not the `refreshRate` field;
* the ART guest's `IDisplayManager.getDisplayInfo` reply
  (`LaunchMain.buildDisplayInfoReply`) set `refreshRate` but left
  `supportedModes` **empty**, so every app saw `0 Hz`,
  `(int)(1000 / 0.0f) == Integer.MAX_VALUE`, and
  `MotionEventGenerator` emitted only the stroke's *key points*: press, release,
  **no MOVE**.

Fixed at the cause: the reply now carries the mode the display is driving
(`Display.Mode(1, 1080, 2400, 60.000004f)`, `modeId`/`defaultModeId` 1,
`refreshRateOverride` 0), exactly like a device. The probe asserts it
(`display id=0 refresh_rate=60.000004 mode_id=1 mode_rate=60.000004 modes=1`), so
this cannot regress silently — it is the difference between a scrolling list and
a stationary one.

Result: the trace above shows `DOWN t=0`, `MOVE ×12 t=16…192`, `UP t=200` for
the 400 px swipe, and the app's own `ScrollView` reports `list_scrolled y=11`.

## 3. What is still not faithful about the swipe: publish pacing (not a wire bug)

`y=11`, not ~400: the `ScrollView` only applies the delta of the *slop-crossing*
move because the host publishes the whole stroke in **one** turn
(`inject_motion_series` writes all 15 `InputMessage`s back to back), so the
guest's `InputEventReceiver` batches them into one or two dispatches and
`ScrollView.onInterceptTouchEvent` sees a single `MOVE`. AOSP's
`MotionEventInjector` instead posts each event on a handler with
`delay = eventTime - now`, so each one arrives in its own frame.

Evidence for batching rather than a coordinate problem: the events carry the
right transform coordinates (traced), the DOWN/UP work (the tap gesture's
`OnClickListener` runs), and only *one* `list_scrolled` line is logged for 13
MOVE events.

The fix is a paced producer — one thread per session that publishes each
`MotionEvent` at its `event_time` (the AOSP `MotionEventInjector` shape), with
`onPerformGestureResult` answered from that thread after the last event instead
of inline. That needs the module's delivery queue to be reachable from that
thread (`A11yState` currently owns `deliveries` inside `A11yConnectionNode`, and
the peer flushes deliveries only on a guest frame or a module deadline). It is a
service-layer design change inside the accessibility module — the brief's
architectural-wall rule — so it is reported, not improvised here.

## 4. The wall: reading a pixel out of the returned `HardwareBuffer` needs a GPU

The brief asks the probe to assert one pixel of the returned buffer. With the
delivery fixed, `Bitmap.wrapHardwareBuffer(buffer, colorSpace)` (the only
app-side route to a `HardwareBuffer`'s pixels) aborts the guest:

```
?/libEGL: assertion failed: !hnd couldn't find an OpenGL ES implementation,
          make sure one of persist.graphics.egl, ro.hardware.egl and
          ro.board.platform is set
Fatal signal 6 (SIGABRT) … Thread: 25 "RenderThread"
```

That is the guest's real state: ACOS renders with a software canvas/C1, and
libhwui's hardware-bitmap path (`SkImage::MakeFromAHardwareBuffer` + EGL) has no
implementation to talk to. Two things came out of it:

* `Bitmap.wrapHardwareBuffer` is **fatal, not catchable** in the guest, so the
  probe must not call it (it aborted the whole session, step 1). It now logs
  `screenshot_pixel unavailable=hardware_bitmap_needs_egl` and the scenario
  asserts that line, so the gap is visible on every run instead of being
  silently dropped.
* getting one step further did expose a real, fixable gap on the way:
  `GraphicBufferMapper::getAllocationSize` (the gralloc5 mapper virtual libhwui
  asks for the buffer's byte size) had no implementation behind it, so it
  dereferenced null (`SIGSEGV in GraphicBufferMapper::getAllocationSize+3`). The
  software shim now implements it from the handle's own geometry
  (`crates/acos-art/shim/gralloc_shim.c`), the same substitution it already
  makes for `importBuffer`/`lock`.

Options for the orchestrator (pick one; I did not decide it):
1. **Accept the host-side pixel claim** (recommended, no new architecture): the
   app asserts the buffer's description (`1080x2400`, `format=1` RGBA_8888,
   `usage=257`, sRGB, a timestamp); the pixels are asserted where they are
   observable — `a11ynative.rs`'s
   `the_memfd_behind_the_hardware_buffer_holds_the_composited_pixels` checks the
   memfd behind the `HardwareBuffer` holds the compositor frame byte for byte
   (`#FF3366CC` first pixel), and the scenario's `swatch bounds=[277,867][802,1077]`
   + frames are the same compositor capture. Cost: no *app-side* pixel read.
2. **Add a software EGL/GLES shim** so HWUI's hardware-bitmap path can wrap a
   memfd-backed `GraphicBuffer` (a real GL/EGL surface, or a stub that returns a
   CPU-backed SkImage). Cost: a graphics-stack track, weeks, and it changes the
   compositor story.
3. Teach `Bitmap.wrapHardwareBuffer` an ACOS-only software path — rejected as a
   deviation from the image's behaviour (the app must not see an API no device
   has).

## 5. Text case: which API reports which label

MODA11Y7's baseline logged `text=Click target`; A11YNATIVE made the pattern
case-insensitive with a comment claiming the theme's `textAllCaps` transforms
the label. Measured on the merged tree, with both values logged side by side:

```
I AcosA11yProbe: button_text a11y=Click target drawn=Click target
I AcosA11yProbe: node class=android.widget.Button text=Click target view_id=null clickable=true
```

* `AccessibilityNodeInfo.getText()` is the **original** string
  (`TextView.onInitializeAccessibilityNodeInfoInternal` →
  `getTextForAccessibility()` → `getText()`), and that is what
  `findAccessibilityNodeInfosByViewId` → `describe()` logs;
* the **drawn** label is the text after the `TextView`'s transformation method —
  the probe now reads it explicitly
  (`getTransformationMethod().getTransformation(text, view)`), which on this
  image is `null`/identity, so both values are `Click target`;
* the observe tree (`tree/*.json`) reports `Click target` too, because it is
  built from `View.createAccessibilityNodeInfo()`.

So there is **no fidelity regression** and **no `CLICK TARGET`** anywhere on this
tree: the a11y path and the drawn path agree. The case-insensitive pattern is
gone; the assertions are exact case (`text=Click target`,
`button_text a11y=Click target drawn=…`), and the probe pins *which* API each
value came from so a future "the button says CLICK TARGET" claim has to name its
API. (The `acos-art` `AllCaps` knob — `crates/acos-art/src/main.rs` — is the
only thing that can make the drawn label upper-case, and it is not what a
`verify` run selects.)

## 6. Tests

```
tools/art-runtime/run-exclusive.sh --task A11YNATIVE2 -- tools/guarded.sh --mem 20G --time 1500 -- \
  cargo test -p acos-services -p acos-wm          # all suites ok, 0 failed
  … --test a11ynative                             # 12 passed
```

New/updated tests in `crates/acos-services/tests/a11ynative.rs`:

* `the_failure_bundle_decodes_with_the_framework_bundle_reader` — the Bundle
  decodes with `parcelable::read_bundle`; fails on the swapped magic.
* `the_failure_bundle_carries_the_error_code_the_callback_reads` and
  `the_success_bundle_carries_a_flattened_graphic_buffer_over_a_memfd` — assert
  the `sendResult` prefix (token + presence word), the `Parcel.writeValue` tags
  from `parcelable::val`, the two parcelable value lengths and the flattened
  buffer body.

## 7. Files

| File | Change |
|---|---|
| `crates/acos-services/src/a11y_native.rs` | shared `BUNDLE_MAGIC`/`val` constants, `begin_send_result`, `write_parcelable_value`, `backpatch_bundle_length` |
| `crates/acos-services/src/a11y_bind.rs` | deliveries no longer write the token twice; `GestureStep` trace lines; real `CLOCK_MONOTONIC` `down_time`; the record counts the published `MotionEvent`s |
| `crates/acos-services/src/input_dispatch.rs` | one allocated `eventId` per published event |
| `crates/acos-services/tests/a11ynative.rs` | the three regression tests above |
| `crates/acos-art/guest/java/dev/acos/art/LaunchMain.java` | `buildDisplayInfoReply` carries `supportedModes` (and `REFRESH_RATE_HZ`) |
| `crates/acos-art/shim/gralloc_shim.c` | `GraphicBufferMapper::getAllocationSize` |
| `fixtures/apps/a11y-probe/.../A11yProbeService.java` | logs the display facts, the swatch bounds and the pixel-API wall; drops the fatal `Bitmap.wrapHardwareBuffer` read |
| `fixtures/apps/a11y-probe/.../MainActivity.java` | logs `button_text a11y=… drawn=…` |
| `scenarios/a11y-probe.yaml` | exact-case label patterns, the display-facts pattern, the screenshot metadata + codes 3/4/6, the pixel-wall line |

## 8. Environment

Target `.cache/a11y-target` (dev/test debug=0, incremental off; release for the
`acos`/`acos-art` binaries). `modules` (`bcc5f8db`) merged into the branch; the
one conflict (`A11yState`) was the union of the a11y screenshot/gesture fields
and BINDERREF2's `refs` registry. Every ART run went through
`tools/art-runtime/run-exclusive.sh --task A11YNATIVE2` with `tools/guarded.sh
--mem 16G`, private `ACOS_RUNTIME_DIR=$PWD/.cache/a11ynative2-run`,
`ACOS_SESSION_SOCKET`, `ACOS_VIEW_TEST_DIR=$PWD/.cache/lv`,
`ACOS_TRACE_FILE`, and a **private sysroot** (`cp -al` hardlink copy at
`.cache/a11ynative2-sysroot`) so the new gralloc shim could be built without
touching the shared `.cache/art/sysroot`. `~/Projects/gaten` was never touched;
disk peaked at 76 % (stop line 86 %), host RAM stayed under 40 % of 60 GiB.
