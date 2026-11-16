# A11YNATIVE — accessibility gestures and screenshots over the guest's own natives

Branch `a11ynative` (from `modules` @ `7c4317ab`, merged in; started at `58824da3`),
worktree `.t3/worktrees/acos/t3-f8938eb5-bb23bbe67b2ea29c`, 2026-11-16.

## Status

**PARTIAL.** The `dispatchGesture` path is finished and proven on ART (gesture
published, app's own listeners ran, `onPerformGestureResult` received). The
`takeScreenshot` path is finished **host-side and unit-tested** (capability
gate, 333 ms interval, invalid display, `FLAG_SECURE`, success `Bundle` with a
`HardwareBuffer` over a memfd copy of the composited frame — a live run logs
`ACOSA11Y takeScreenshot success display=0 1080x2400 stride=1080`), but the
**guest never reads the result**: no `TakeScreenshotCallback.onSuccess/onFailure`
runs in the app. The wall and the next action are in §4.

| Step | State |
|---|---|
| 1. screenshot failure paths (capability, 333 ms, invalid display, `FLAG_SECURE`) | **HOST DONE** (codes 3/4/6 + the `SecurityException` on the reply), **guest delivery unproven** — §3 |
| 2. `dispatchGesture` (decode the slice, gate on the capability, publish at the guest's event times, answer `onPerformGestureResult`) | **DONE, proven on ART** — §2 |
| 3. `HardwareBuffer` (`nReadHardwareBufferFromParcel` + getters, `Bitmap.wrapHardwareBuffer` via a memfd-backed flattened `GraphicBuffer`) | **HOST DONE + unit-tested**, guest read unproven — §3 |

## 1. What MODA11Y7 got wrong, and what the image actually says

MODA11Y7 concluded that the `ParceledListSlice` of `dispatchGesture` carries
`MotionEvent`s and that the guest needed `MotionEvent.nativeWriteToParcel`. Both
are wrong, and the image's own bytecode says so
(`.cache/boot/android-34/dex/framework/classes.dex`, dumped with
`cargo run -p acos-dex --example dexdump`):

```
AccessibilityService->dispatchGesture:
  MotionEventGenerator.getGestureStepsFromGestureDescription(...)Ljava/util/List;
  new-instance ParceledListSlice; <init>(List)
  IAccessibilityServiceConnection->dispatchGesture(mSequence, slice, displayId)
```

`getGestureStepsFromGestureDescription` returns `List<GestureStep>` — the AIDL
field name `gestureSteps` is literal. A step writes
`writeLong(timeSinceGestureStart)` + `writeParcelableArray(touchPoints)`; a
touch point writes `mStrokeId`, `mContinuedStrokeId`,
`mIsStartOfPath|(mIsEndOfPath?2:0)`, `mX`, `mY`. The **system** turns steps into
`MotionEvent`s (`MotionEventInjector.injectEvents`), so the host does that
conversion, not the guest.

The ART guest does not need any new native for this: it runs the image's own
`libandroid_runtime`/`libui` and registers the image's own
`register_android_view_MotionEvent`, so the guest side is the real
implementation (the Rust VM's `unsupported` refusal is the *frozen VM*, not the
ART guest).

Wire facts re-verified twice: the image's writer/reader bytecode **and** a real
captured parcel. `crates/acos-services/tests/a11ynative.rs` embeds the exact 460
bytes an ART run handed the module and asserts the decoder consumes them
completely (`the_captured_gesture_parcel_decodes_exactly`). That parcel also
settles a trap: `Parcel.writeString16` writes `len` + `(len+1)*2` bytes
(NUL-terminated, padded), so a touch point's class name costs 124 bytes on the
wire, not 120 — a hand-count that ignored the NUL lands mid-field.

## 2. `dispatchGesture` — done

* `crates/acos-services/src/a11y_native.rs` (new): `read_gesture_slice` /
  `read_gesture_step`, `GestureStep::gesture_events` (steps → `DOWN`/`MOVE`/
  `UP`, `POINTER_DOWN`/`POINTER_UP` for multi-pointer), and `channel_events`
  (one `InputChannel` publish per event, the guest's own `eventTime`s, the point
  in the window transform — the image's measured `InputMessage` contract, I2).
* `crates/acos-services/src/input_dispatch.rs`: `inject_motion_series` publishes
  a whole stroke, every event before the first ack can be lost.
* `crates/acos-services/src/a11y_bind.rs`: transaction **33**
  (`dispatchGesture`) — reads `int sequence`, the AIDL typed-object presence
  word, the slice, `int displayId`; gates on `can_perform_gestures()` exactly as
  `AccessibilityServiceConnection.dispatchGesture` does (no capability → neither
  inject nor answer); publishes on the focused window's channel; then answers
  `IAccessibilityServiceClient.onPerformGestureResult(sequence, true|false)`
  (code **11**) — `false` with a recorded reason when delivery fails, never a
  silent success.
* Probe (`fixtures/apps/a11y-probe`): the service taps the `gesture_target`
  button and swipes the `gesture_list` `ScrollView` with real
  `GestureDescription` strokes; `MainActivity` counts the button press and logs
  `list_scrolled`.

Evidence (ART, `--engine art`):

```
results/handoff/A11YNATIVE/gestures/logs/logcat.txt
  AcosA11yProbe: gesture_sent kind=tap accepted=true strokes=1
  AcosA11yProbe: gesture_sent kind=swipe accepted=true strokes=1
  AcosA11yProbe: gesture_target_pressed count=1
  AcosA11yProbe: gesture_result kind=tap completed=true
  AcosA11yProbe: gesture_result kind=swipe completed=true
.cache/a11ynative-run/trace.log (ACOS_TRACE_FILE)
  ACOSA11Y dispatchGesture decoded seq=1 events=2 display=0
  ACOSA11Y dispatchGesture decoded seq=2 events=2 display=0
```

`gesture_taps` goes 0 → 1 and the app's own `OnClickListener` logs the press, so
the injected `DOWN`/`UP` reached the normal touch path inside the app, not the
module's bookkeeping.

**Not proven:** the swipe does not move the `ScrollView` (`swipe_y=0`, no
`list_scrolled` line) although the app received and completed the gesture. The
list sits inside the focused window and the `MOVE`s ride in the window
transform; the next step is to check whether `ScrollView.onTouchEvent` sees the
`MOVE` chain (the same transform-vs-axis question I2 settled for a tap, for a
multi-MOVE stroke).

## 3. `takeScreenshot` — host done, guest delivery blocked

* `crates/acos-services/src/a11y_bind.rs` transaction **37**
  (`takeScreenshot`): AOSP's order and codes —
  interval 333 ms first (`ERROR_TAKE_SCREENSHOT_INTERVAL_TIME_SHORT` 3), then the
  capability (the only failure that **throws** `SecurityException`, written as a
  binder exception), then `displayId != 0` (`…INVALID_DISPLAY` 4; ACOS composites
  one display), then `SurfaceState::has_secure_window()`
  (`…SECURE_WINDOW` 6), success = the composited frame.
* `a11y_native::write_success_bundle` writes exactly the
  `AccessibilityService.KEY_ACCESSIBILITY_SCREENSHOT_*` `Bundle` the framework
  reads (status, `HardwareBuffer`, `ParcelableColorSpace`, timestamp; entries in
  `ArrayMap` order), and the `HardwareBuffer` is a flattened `GraphicBuffer`
  (`'GB01'`, 13 header words, 1 fd, 9 handle ints) over a **memfd copy of the
  composited frame** — no real gralloc HAL and no GPU exist in the guest; the
  guest's own `libacoshw.so` gralloc shim imports the handle on
  `GraphicBuffer::unflatten`, the same substitution it already documents for the
  app's window surfaces. `usage = USAGE_CPU_READ_OFTEN|USAGE_GPU_SAMPLED_IMAGE`
  is what `Bitmap.wrapHardwareBuffer` gates on.
* Guest side: the only thing missing was the registration —
  `register_android_hardware_HardwareBuffer` (the image's own native) added to
  `SessionMain.stepRuntimeExtras` (and `LaunchMain.registerNatives`). No
  substitute native was written.

Evidence that the host side really produces the frame:

```
.cache/a11ynative-run/trace.log
  ACOSA11Y takeScreenshot enter
  ACOSA11Y takeScreenshot success display=0 1080x2400 stride=1080
  ACOSA11Y takeScreenshot enter      (the immediate second call: interval path)
```

**The wall:** the app never runs `TakeScreenshotCallback.onSuccess/onFailure`,
so the `IRemoteCallback.sendResult` delivery never reaches it (no
`screenshot_result` line in the run's logcat, and the probe thread stops after the
first call). The gesture path proves this node's `PendingDelivery` mechanism
works for a guest binder, so the difference is in the payload or in the guest's
handling of it — most likely `nReadHardwareBufferFromParcel` is still
unregistered in the *running* guest (an `UnsatisfiedLinkError` inside
`Bundle.getParcelable(…, HardwareBuffer.class)` would kill the callback silently),
i.e. the guest dex that the ART run builds may not yet contain the
`SessionMain` registration.

**Smallest next action:** run the probe with the guest's stderr kept
(`acos verify … --artifacts` already keeps `session-exit.json`; also
`ACOS_ART_KEEP=1`-style retention) and grep for `UnsatisfiedLinkError` /
`HardwareBuffer`; if the registration is missing, confirm the guest dex
(`crates/acos-art/src/build_cache.rs`) was rebuilt after the `SessionMain`
change; if it is present, log inside the app's `RemoteCallback` listener before
it touches the bundle to separate "delivery not received" from "bundle unparcel
threw".

Unit tests for this half (`crates/acos-services/tests/a11ynative.rs`, 11/11):
the failure bundle's code, the success bundle's four entries in `ArrayMap`
order, the `[88][1]` flatten header and its fields, the single fd, that the
memfd's bytes are the frame's pixels (first pixel `#FF3366CC`), a frame whose
pixels disagree with its size being refused, and the two capability gates.

## 4. Probe and scenario

`fixtures/apps/a11y-probe` now has a `swatch` (`#FF3366CC`), a `gesture_target`
button and a 40-row `gesture_list`. `A11yProbeService`:

* probes the gestures **before** the overlay is added (an interaction query after
  the add goes through the service's own overlay window on a worker thread — the
  same-process wait MODA11Y4 documented);
* then takes a screenshot for the success case, immediately again (interval 3),
  for `display 5` (4), and with a `FLAG_SECURE` accessibility overlay on screen
  (6), reading one swatch pixel out of the returned `HardwareBuffer` through
  `Bitmap.wrapHardwareBuffer` + copy + `getPixel`.

`scenarios/a11y-probe.yaml` gained two assertions: the gesture one (passes) and
the screenshot one (fails on the missing callback, §3). The scenario currently
fails one step **earlier** than those: `assert_windows` sees three windows
(`application`, `accessibility_overlay`, `accessibility_overlay`) because the
probe's secure overlay is removed but the host's LAYER1 stack has not dropped it
when the step runs — the removal needs the app's main looper to finish the
remove transaction, and the probe's own thread is blocked in the screenshot
call. Fixing §3 removes that timing problem too.

## 5. Tests

```
cargo test -p acos-services            (shared lock) — all binaries green
cargo test -p acos-services --test a11ynative — 11 passed
```

(The three DEX-scanning tests, e.g. `net1_connectivity::the_connectivity_specs…`,
need `.cache/boot/android-34/dex`; in this worktree that symlink had to be
created — without it they fail with "boot DEX cache (run from the repo root)",
which is an environment failure, not a code one.)

## 6. Environment

Target `.cache/a11y-target` (dev+test debug=0, incremental off); every ART run
went through `tools/art-runtime/run-exclusive.sh` with `tools/guarded.sh --mem
12G`. Private `ACOS_RUNTIME_DIR=$PWD/.cache/a11ynative-run`, private
`ACOS_SESSION_SOCKET`, `ACOS_VIEW_TEST_DIR=$PWD/.cache/lv`,
`ACOS_TRACE_FILE=$PWD/.cache/a11ynative-run/trace.log`. `~/Projects/gaten` was
never touched. Disk 76 % at the start (stop line 86 %), host RAM stayed under
50 %.
