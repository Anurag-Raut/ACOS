# MODA11Y7 — `dispatchGesture`, `takeScreenshot`, the Gaten bubble

Branch `moda11y7` (from `modules` @ `c7514d0d` = MODA11Y6), 2026-11-16.
Worktree `.t3/worktrees/acos/t3-9106ddc7-1cc8e54c830c1b27`.

## Status

**PARTIAL — blocked outside this module on all three steps; the exact walls are
below, with the source/dex evidence for each.** No Rust behaviour was changed,
so nothing here claims a passing behaviour it did not prove.

| Brief step | State |
|---|---|
| 1. `dispatchGesture` (gate + stroke → real `MotionEvent`s + `onPerformGestureResult`) | **BLOCKED**: the guest cannot serialise the events it is required to send — §1 |
| 2. `takeScreenshot` (gate + composited frame + `RemoteCallback` shape) | **HOST SIDE SPECIFIED, SUCCESS PAYLOAD BLOCKED**: the guest cannot construct a `HardwareBuffer` — §2 |
| 3. Gaten bubble (`TYPE_ACCESSIBILITY_OVERLAY` owned by `GatenAccessibilityService`) | **PROGRESS, then BLOCKED outside the module**: the bind now really starts Gaten's service and runs the app's own `onCreate`, which needs the media mainline initializers this process never installs — §3 |

Session 7 found the two facts that decide steps 1–2. Both were assumed
differently by the brief, and the image is authoritative:

1. **`dispatchGesture` is transaction 33, not 14** (`takeScreenshot` is 37,
   `sendGesture` 32, `onPerformGestureResult` 11 on the client). Read out of the
   image's own `IAccessibilityServiceConnection$Stub.getDefaultTransactionName`
   packed-switch payload, not from the AIDL file order — see
   `wire-map.txt` and §4.
2. **The guest does not send strokes.** The image's
   `AccessibilityService.dispatchGesture` calls
   `GestureDescription$MotionEventGenerator.getGestureStepsFromGestureDescription(gesture, sampleTimeMs)`
   and puts the resulting **`List<MotionEvent>`** in the `ParceledListSlice`
   (disassembly quoted in §1). So the host would receive already-sampled
   `MotionEvent`s — and the guest has no native that can write one to a parcel.

## 1. `dispatchGesture` — the wall is the guest's `MotionEvent` parcel native

### What the image's app-side code does (authoritative)

`crates/…` disassembly of `Landroid/accessibilityservice/AccessibilityService;->dispatchGesture`:

```
0017: invoke-static {v9, v1}, GestureDescription$MotionEventGenerator;
        .getGestureStepsFromGestureDescription:(Landroid/accessibilityservice/GestureDescription;I)Ljava/util/List;
003f: new-instance v6, Landroid/content/pm/ParceledListSlice;
0041: invoke-direct {v6, v2}, Landroid/content/pm/ParceledListSlice;.<init>:(Ljava/util/List;)V
0048: invoke-interface {v0, v4, v6, v7}, IAccessibilityServiceConnection;
        .dispatchGesture:(ILandroid/content/pm/ParceledListSlice;I)V
```

`GestureDescription$StrokeDescription` has **no** `writeToParcel` in the image;
the slice carries `MotionEvent`s. `MotionEvent.writeToParcel` is
`writeInt(1)` + `nativeWriteToParcel(long, Parcel)` (classes4.dex, quoted in
`wire-map.txt`), and ACOS's guest native for it is a structured refusal:

`crates/acos-vm/src/natives/fwk_input.rs:1236`
```
fn me_write_to_parcel(..) -> VmResult<()> {
    // The MotionEvent parcel path is not exercised by the input stack; the
    // channel path carries `InputFrame`s instead.
    Err(unsupported("MotionEvent.nativeWriteToParcel"))
}
```

So an app's `dispatchGesture(...)` throws before the transaction leaves the
guest: **no host-side handler can be exercised, not even the capability gate**,
because the refusal happens while building the request parcel.

### What implementing it needs (all of it is available, none of it is invented)

* **Guest**: `MotionEvent.nativeWriteToParcel`/`nativeReadFromParcel` over the
  real layout — `base/core/jni/android_view_MotionEvent.cpp` calls
  `MotionEvent::writeToParcel`, and that body **is** in the cached AOSP tree:
  `.cache/aosp/native/libs/input/Input.cpp` (`MotionEvent::readFromParcel`
  :754, `writeToParcel`, `PointerCoords::writeToParcel` :464, the `ui::Transform`
  helper :744 — pointerCount/sampleCount, id, deviceId, source, displayId,
  32-byte hmac, per-pointer properties, per-sample times and coords, transforms).
  The `fwk_input.rs` struct pool already holds every one of those fields.
* **Host**: read `int sequence`, the `ParceledListSlice` (the repo already
  decodes that shape: `parcelable::read_parceled_list_slice`), the element class
  (`android.view.MotionEvent`) and `int displayId`; then publish each decoded
  event on `A11yState::input`'s channel with its own `downTime`/`eventTime`
  (`InputDispatch` needs one new `inject_motion_series`; today only
  `inject_tap`/`inject_key`/`inject_text` exist), and answer
  `IAccessibilityServiceClient.onPerformGestureResult(sequence, completed)` —
  code **11** — only after the events were published. The gate is
  `service_config.can_perform_gestures()` (capability `1 << 5`, already parsed
  by MODA11Y6).
* **Stroke timing** comes free: the sample times are the guest's own
  (`sampleTimeMs` from `calculateGestureSampleTimeMs`), so "honour stroke
  timing" is honoured by publishing the events with their times, not by
  sleeping on the host.

That is guest-native work in `acos-vm` (a new/expanded `fwk_input` native) plus
a host handler in `a11y_bind.rs`. It is **not** an architectural change, but it
crosses into the guest native layer and rewrites the guest→host MotionEvent
wire, so the orchestrator should schedule it rather than have this session
start it at the 60-minute mark.

## 2. `takeScreenshot` — the gate and the refusals are host-side; the success payload needs guest natives

AOSP's shape (`.cache/aosp/base/services/accessibility/java/com/android/server/
accessibility/AbstractAccessibilityServiceConnection.java:1418-1553`):

* `takeScreenshot(int displayId, RemoteCallback callback)` (transaction **37**);
* no capability → `throw new SecurityException("Services don't have the
  capability of taking the screenshot.")`;
* two calls inside `ACCESSIBILITY_TAKE_SCREENSHOT_REQUEST_INTERVAL_TIMES_MS`
  (333 ms) → failure `ERROR_TAKE_SCREENSHOT_INTERVAL_TIME_SHORT` (3);
* a private virtual / unknown display → `ERROR_TAKE_SCREENSHOT_INVALID_DISPLAY` (4);
* success → a `Bundle` with `screenshot_status` = 0, `screenshot_hardwareBuffer`
  (a `HardwareBuffer`), `screenshot_colorSpace` (`ParcelableColorSpace`) and
  `screenshot_timestamp`, sent through `IRemoteCallback.sendResult` — the
  `AccessibilityService.TakeScreenshotCallback` reads exactly those keys
  (`AccessibilityService.java:2699-2719`).

The host can do all of that with what is already installed: the composited
frame is `SurfaceState::capture_composite()` (the C1 store `registry::install`
keeps), `SurfaceState::has_secure_window()` already answers the `FLAG_SECURE`
question, and `GraphicBuffer::flatten` — the body
`android_hardware_HardwareBuffer_write` uses (`parcel->write(*buffer)`) — is in
the cache (`.cache/aosp/native/libs/ui/GraphicBuffer.cpp:458-495`: `'GB01'`,
width, height, stride, format, layerCount, usage lo/hi, id, generationNumber,
`mTransportNumFds`/`mTransportNumInts`, the fds, then the handle ints).

**What blocks it**: the guest end. The app reads the payload with
`result.getParcelable(KEY_ACCESSIBILITY_SCREENSHOT_HARDWAREBUFFER,
HardwareBuffer.class)`, which is `nReadHardwareBufferFromParcel`
(`android_hardware_HardwareBuffer.cpp:201`), and it uses the result with
`Bitmap.wrapHardwareBuffer(hardwareBuffer, colorSpace)`, i.e.
`Bitmap.nativeWrapHardwareBufferBitmap` + `ColorSpace.getNativeInstance`. ACOS's
guest has **none** of those (`crates/acos-vm/src/natives/` has no
HardwareBuffer class and `gfx_bitmap.rs` has no hardware-bitmap path); a
`HardwareBuffer` there is only a Java field the guest never populates
(`nativeSetBuffer` in `fwk_surface.rs:1254` ignores the object). `HardwareBuffer`
itself has no Java-side fallback: `getWidth()`/`getHeight()` are
`@FastNative nGetWidth`/`nGetHeight` over the native object, so even
"dimensions + one pixel colour" cannot be read without that native.

So step 2 needs a decision the brief did not anticipate: either
(a) implement `nReadHardwareBufferFromParcel` + `nGetWidth`/`nGetHeight`/
`nGetFormat`/`nGetUsage`/`nClose` + `Bitmap.nativeWrapHardwareBufferBitmap` in
the guest (a software bitmap over the fd's pixels, since the rasteriser is
`acos-render`), or (b) have the probe read the raw pixels through a
HardwareBuffer-shaped path ACOS owns (a deviation from AOSP's app API).
(a) is the faithful one and every wire it needs is in the cache
(`GraphicBuffer::flatten`/`unflatten` + `Parcel::write(Flattenable)`).

The failure paths (capability `SecurityException`, 333 ms interval, invalid
display, **`FLAG_SECURE`** = `ERROR_TAKE_SCREENSHOT_SECURE_WINDOW` 6, which
`AccessibilityService.ScreenshotErrorCode` defines) are all reachable and
testable **today**, because the request parcel (`int displayId` +
`RemoteCallback`) needs no missing native. That is the smallest slice worth
landing next, and it is what a probe can assert on ART without the
HardwareBuffer work.

## 3. The Gaten bubble — the bind now runs Gaten's own service; the next wall is the process bootstrap

Scenario: `scenarios/gaten-bubble.yaml` (same APK path the other Gaten scenarios
use), `services: [{id: accessibility, profile: real, config: {enable:
["com.gaten/com.gaten.app.GatenAccessibilityService"]}}]` — the component is
Gaten's manifest's own (`com.gaten.app.GatenAccessibilityService`, guarded by
`android.permission.BIND_ACCESSIBILITY_SERVICE`, which the manifest declares).

Run (shared lock, private runtime dir):

```
results/handoff/MODA11Y7/discover/     the run's artifacts (art-frames/, run.json)
```

`art-frames/session-exit.json` is the evidence for both halves:

```
"application_created": true, "activity_resumed": true,
"guest_crash": {"outcome":"crash","code":"LOOPER_THREW", ...
  "message":"java.lang.RuntimeException: Unable to create service
   com.gaten.app.GatenAccessibilityService: java.lang.NullPointerException:
   Attempt to invoke virtual method
   'android.media.MediaServiceManager$ServiceRegisterer
    android.media.MediaServiceManager.getMediaSessionServiceRegisterer()'
   on a null object reference", ...}
   causes[0].stack:
     android.media.session.MediaSessionManager.<init>(MediaSessionManager.java:130)
     android.media.MediaFrameworkPlatformInitializer.lambda$registerServiceWrappers$0(:75)
     android.app.SystemServiceRegistry$140.createService(SystemServiceRegistry.java:1897)
     android.app.SystemServiceRegistry.getSystemService(SystemServiceRegistry.java:1645)
     android.accessibilityservice.AccessibilityService.getSystemService(AccessibilityService.java:2616)
     B2.l.<init>(SourceFile:45)
     W1.b.<init>(SourceFile:19)
     com.gaten.app.GatenAccessibilityService.onCreate(Unknown Source:5)
     android.app.ActivityThread.handleCreateService(ActivityThread.java:4651)
```

Read it in two steps:

* **The 2026-11-15 feedback is addressed**: `ActivityThread.handleCreateService`
  ran and Gaten's own `GatenAccessibilityService.onCreate` executed. The
  service is started, in the app's own process, through the framework's own
  path — the module's bind (`scheduleCreateService` → `scheduleBindService` →
  `publishService` → `IAccessibilityServiceClient.init`) is what put it there.
* **The next wall is not the accessibility module.** Gaten's `onCreate` asks for
  `getSystemService(MEDIA_SESSION_SERVICE)` (obfuscated `B2.l`/`W1.b`); the
  guest's `SystemServiceRegistry` creates `MediaSessionManager`, whose
  constructor needs `MediaServiceManager` — installed in every real app process
  by `ActivityThread.initializeMainlineModules()`
  (`ActivityThread.java:8561` calls it; `:8601-8610` sets the media/telephony/
  bluetooth/nfc managers). ACOS's guest launcher never calls it: it runs the
  *module-supplied* `ACOSLAUNCH_INITIALIZERS` instead
  (`crates/acos-art/guest/java/dev/acos/art/LaunchMain.java:344-371`,
  `ModuleManifest::process_initializers`, ADR-0029 item 1), and no module in a
  Gaten session declares
  `android.media.MediaFrameworkPlatformInitializer#setMediaServiceManager#android.media.MediaServiceManager`.
  `media_session` itself is already a documented tombstone
  (`crates/acos-services/src/module.rs:1097-1103`), so once the manager is
  installed `MediaSessionManager.<init>` stores the throwing proxy and does not
  transact, which is exactly the behaviour that tombstone's evidence predicts.

**Smallest next action for the bubble** (needs an owner decision — it is a
`media`/process-bootstrap concern, not an accessibility one): add that
`ProcessInitializer` to a module that ships in a Gaten session (a `media.session`
module, or `core` if the managers are treated as process-wide), then re-run
`scenarios/gaten-bubble.yaml`. The scenario's `assert_windows` step
(`order: [application, accessibility_overlay]`, `types: [TYPE_BASE_APPLICATION,
TYPE_ACCESSIBILITY_OVERLAY]`) is the assertion that would then have to pass. Its
`wait_for` on the launch screen already passes on this branch's engine, i.e. the
run got as far as a resumed, first-frame activity before the service crash.

## 4. The transaction map (brief correction)

`results/handoff/MODA11Y7/wire-map.txt` is generated from the image dex. Method:
the `$Stub.getDefaultTransactionName(int)` body is a packed switch
(`ident 0x0100, size 54, first_key 1`, at `code_off+16+0xc4*2` of
`classes.dex`), each target a `const-string`; the payload's
`key → target → name` is the generated `TRANSACTION_*` numbering. It agrees with
`getMaxTransactionId()` = 53 for the service→system interface.

```
1 setServiceInfo            8 performAccessibilityAction   32 sendGesture
2 setAttributionTag         9 getWindow                      33 dispatchGesture
3 findAccessibilityNode…   10 getWindows                     34 isFingerprintGestureDetectionAvailable
4 find…ByText              11 getServiceInfo                 35 getOverlayWindowToken
5 find…ByViewId            12 performGlobalAction            36 getWindowIdForLeashToken
6 findFocus                13 getSystemActions               37 takeScreenshot
7 focusSearch              14 disableSelf                    38 takeScreenshotOfWindow
```

The `MODA11Y4` table in `a11y_bind.rs::CONNECTION_METHODS` (1–14) is therefore
**correct**, and the brief's "`dispatchGesture` is code 14" is not: 14 is
`disableSelf`. Client side (host → guest), from the same method on
`IAccessibilityServiceClient$Stub`: `1 init`, `2 onAccessibilityEvent`,
`4 onGesture`, `6 onKeyEvent`, **`11 onPerformGestureResult`**, and so on —
`onAccessibilityEvent` = 2 matches what MODA11Y6 delivers today.

## Tests

`cargo test -p acos-services -p acos-wm --no-fail-fast` (shared lock, this
branch): `results/handoff/MODA11Y7/tests.log` — **69 test binaries green** plus
one unrelated flake:

```
acos-services --test sensors_module::the_sensors_module_passes_the_conformance_kit
  detach-restores: FAIL baseline threads: 2 vs 3
```

That is the known thread-count baseline race (a live thread from the previous
section of the same process), not this branch: `--test sensors_module` passes
**15/15 on two consecutive re-runs**. No Rust source changed on this branch, so
there is no behaviour claim here — it is the `modules`@MODA11Y6 baseline
re-verified.

Fixture note: `tests/moda11y6_gate.rs` reads
`fixtures/build/apps/a11y-probe.apk`, which the shared `fixtures/build` copy did
**not** contain (the previous session deleted its fixture build). It was rebuilt
from `fixtures/apps/a11y-probe` with the same `tools/build-apps.sh` recipe
(`build_apk a11y-probe`) into this worktree's hardlink copy; before that, all six
gate tests failed with "fixtures/build/apps/a11y-probe.apk is built", after it
they pass 6/6. Any agent running the a11y tests needs that APK present.

ART baseline of the same branch (no new scenario assertions, just the step-1..2
harness still working after the fixture rebuild):
`scenarios/a11y-probe.yaml` → **pass, 13/13 steps**
(`results/handoff/MODA11Y7/probe-baseline/verdict.json`).

## 5. Environment notes

* Target `.cache/a11y-target` (dev+test debug=0, incremental off), release
  `acos`/`acos-art` built from it; `fixtures/build` is the hardlink copy.
* Every ART/gate command went through `tools/art-runtime/run-exclusive.sh`
  (`--mem 10G/12G`). Disk 74 % throughout (stop line 86 %), host RAM stayed
  under 45 GB used of 60 GB.
* Private `ACOS_RUNTIME_DIR=$PWD/.cache/moda11y7-run`, private
  `ACOS_SESSION_SOCKET`, `ACOS_VIEW_TEST_DIR=$PWD/.cache/lv`; `~/Projects/gaten`
  was never touched (the built APK from `.cache/gaten` is the only input).
