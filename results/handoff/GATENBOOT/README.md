# GATENBOOT — process bootstrap fidelity + the Gaten bubble

Branch `gatenboot`, from `modules` @ `58824da3` (MODA11Y7), merged with `modules`
@ `7c4317ab` (NET5 + PIFILLIN). Worktree
`.t3/worktrees/acos/t3-20565a87-eeec9c328e189326`, 2026-11-16.

## Status

**PARTIAL — the bootstrap wall is fixed and proven; the bubble window is blocked
outside the bootstrap, on two named gaps (§2).** Everything this branch claims
has a test or a run under `results/handoff/GATENBOOT/`.

| Brief step | State |
|---|---|
| 1. Run AOSP's per-process mainline bootstrap for every guest process | **DONE** — `LaunchMain` now calls `ActivityThread.initializeMainlineModules()` at AOSP's own point |
| 2. Reconcile with `ModuleManifest::process_initializers` (telephony/bluetooth) | **DONE** — the modules no longer declare managers AOSP installs; the mechanism stays for managers it does not |
| 3. Services the managers look up answer honestly | **DONE** for the two the run actually reads (`media_session` = core tombstone, per the table; `IWindowManager.isKeyguardLocked/isKeyguardSecure` = `false`, this device has no keyguard) — §3 |
| 4. `scenarios/gaten-bubble.yaml` on ART: overlay window + content + tap | **BUTTON FOUND, WINDOW BLOCKED** — the service now binds and runs Gaten's own code; the bubble is gated behind a guest broadcast ACOS does not deliver (§2). The passing scenario proves the bind and the app's own view of it; `scenarios/gaten-bubble-overlay.yaml` pins the overlay assertion as a known wall |
| 5. Existing module scenarios still pass on ART | **DONE** (§4; `net-network-real` needs host tools this box lacks) |

## 1. The bootstrap wall (fixed)

MODA11Y7's stack stopped at `MediaSessionManager.<init>` →
`MediaFrameworkPlatformInitializer` → `MediaServiceManager` **null**. The cause was
not the module: ACOS's guest launcher builds the `ActivityThread` itself and never
runs `ActivityThread.main`, and the image dex shows the per-process bootstrap lives
there:

```
framework/classes.dex  ActivityThread.main @1c37c0, insn 001c
  invoke-static {}, Landroid/app/ActivityThread;.initializeMainlineModules:()V   (the only call site)
```

`handleBindApplication` does **not** call it (the orchestrator's premise said it
did; the image and r75 source both put it in `main()`, before
`Looper.prepareMainLooper()`/`attach`). `LaunchMain` now calls the same framework
method once per guest process, right after the runtime is up and before any app
code, and logs `ACOSLAUNCH MARK mainline-modules initialized`. The list is the
**image's own** (`framework/classes.dex` `ActivityThread.initializeMainlineModules`
body @`1c34e0`): Telephony, Stats, Media platform + Media (`MediaServiceManager`
×2), Bluetooth (+ binder-calls-stats initializer), NFC, DeviceConfig. The image has
no `Se`/`Profiling`/`Connectivity` initializer and no `android/server/Flags`, so
"include ConnectivityFrameworkInitializer" does not apply to this image — the
network module (NET5) declares none either and its off arm still passes (§4).

A failure of that call is **fatal**, not a warning: on a device it is unguarded,
and a process that boots with null mainline managers dies later in a constructor
that names neither the manager nor the call (that is exactly MODA11Y7).

Reconciliation with `ModuleManifest::process_initializers` (MODTAIL2): AOSP's call
already installs `TelephonyServiceManager` and `BluetoothServiceManager` in *every*
app process, and both setters throw `IllegalStateException` on a second call — so
`net.telephony` and `hw.bluetooth` now declare `process_initializers: &[]` and the
guest runs AOSP's call instead of the module's. The mechanism (manifest field, wire
form, guest loop, validation) is kept and tested for a manager AOSP's call does not
install; none is known today. Documented in `module.rs` (`ProcessInitializer`),
in `docs/AOSP_REFERENCE.md`, and guarded by
`crates/acos-art/src/launch.rs::the_process_initializer_install_is_driven_by_the_manifest`.

Evidence: `regression/gaten-bubble/` (the service connects), and the run that hit
the **next** wall proves the bootstrap did its job — the crash moved from the
manager constructor into Gaten's own `onServiceConnected`:

```
run-before-keyguard.log  → LOOPER_THREW UnsupportedOperationException:
  unsupported Android API: android.view.IWindowManager code 31 (isKeyguardLocked)
  at com.gaten.app.GatenAccessibilityService.a(Unknown Source:39)
  ← AccessibilityService.dispatchServiceConnected
```

## 2. The bubble wall (not fixed; orchestrator decision, §6)

`scenarios/gaten-bubble-overlay.yaml` (known wall, `expectation: fail`,
`expect_error: ASSERTION_FAILED`) ends with
`window kinds are ["application"], expected ["application", "accessibility_overlay"]`.
The steps before it all pass, including the floating-button switch (the row's own
switch then reports `checked: true`, i.e. the preference is stored).

Gaten shows the bubble only from `GatenAccessibilityService.syncFloatingButton()`,
which runs on `onServiceConnected`, on a screen/display callback, and on the
runtime-registered broadcast `com.gaten.app.action.FLOATING_BUTTON_CHANGED`
(`GatenTriggerModule.setFloatingButtonEnabled` → `sendBroadcast` → the service's
`registerTriggerReceiver`). ACOS's AMS accepts and records `sendBroadcast` but
delivers nothing to the guest — `docs/IMAGE_ABI_NOTEBOOK.md`: *"it does not deliver
broadcasts to the guest: there is no receiver scheduler on the ART host"*. So the
switch writes the preference and the service is never told; the window is never
added.

Proven en route (so the next agent starts one step from the finish):

* the accessibility module binds Gaten's service through the framework's own path
  and the app publishes its `IAccessibilityServiceClient`
  (`regression/gaten-bubble/engine.json` → `service_bound: true`, `binding: "bound:
  … so onServiceConnected ran"`, `enabled_services`, `event_count`);
* with the preference **on before the bind**, `onServiceConnected →
  syncFloatingButton()` is the code that adds the window — `FloatingTriggerButton.show()`
  → `windowManager.addView(bubble, TYPE_ACCESSIBILITY_OVERLAY)`;
* window stacking/reporting for a second window works: probe5 (`.cache` scratch)
  added Gaten's own "Turn on lookup mode" dialog as a second window and the tree
  reported `["application","application"]`, so `assert_windows` is not the blocker.

Two exits (both outside the process bootstrap):

* **(a) guest broadcast delivery** — AMS → runtime-registered receivers. A
  subsystem (a scheduler in the guest or a host-side delivery path with a receiver
  registry), i.e. an architectural decision.
* **(b) the accessibility module binding an enabled service when the *enable set*
  changes**, the way `AccessibilityManagerService.setEnabledServices` →
  `bindServices` does, instead of only from the app's
  `IAccessibilityManager.addClient` (`crates/acos-services/src/misc.rs` `addClient`
  → `a11y_bind::plan_bind`). With (b), the sequence already written in
  `scenarios/gaten-bubble-overlay.yaml` (switch on → detach → attach) rebinds the
  service, `onServiceConnected` runs again with the preference on, and the bubble
  is added. Measured today: after detach+attach the module reports
  `service_bound: false`, `binding: "no bind yet: it starts when the app's
  AccessibilityManager calls addClient…"`. This is the smaller, AOSP-faithful
  change and it belongs to MODA11Y's module.

Drag needs `dispatchGesture` (MODA11Y7 §1: the guest's `MotionEvent` parcel native
is a structured refusal) — **not attempted**, as the brief said.

## 3. Honest answers for what the managers look up

* `media_session` — decision unchanged, and it is the tombstone table's
  (`crates/acos-services/src/module.rs:1095-1103`, `AospReaction::Tombstone`):
  `MediaSessionManager.<init>` stores
  `MediaFrameworkPlatformInitializer.getMediaServiceManager().getMediaSessionServiceRegisterer().get()`
  **without transacting**, so a present-but-throwing binder cannot break
  `getSystemService`; a session that does not attach `media.session` registers that
  tombstone (`ModuleRegistry::install_tombstones`), so the name answers
  `UNSUPPORTED_ANDROID_API` at first use instead of NPE-ing. Pinned by
  `acos-services --test module_kit::a_gap_name_answers_a_structured_tombstone_naming_the_module_and_the_attach_call`
  (green in `tests.log`).
* `IWindowManager.isKeyguardLocked()` / `isKeyguardSecure(int)` — **minimal real
  answer**: `false`, because ACOS shows no lock screen
  (`KeyguardManager.isKeyguardLocked` reads
  `WindowManagerService.mKeyguardController.isKeyguardShowing()`, false while no
  keyguard exists; the SIM-lock half of `isKeyguardSecure` needs a SIM lock state
  ACOS does not have). This is the call that killed Gaten's service: `KeyguardManager`
  catches only `RemoteException`, so a structured refusal propagates out of
  `onServiceConnected`. Tests: `acos-services --test gatenboot_keyguard` (three
  cases: both transactions through the real `window` node, plus the error naming).
* Also fixed while there: `WmsService`'s unsupported error named
  `android.view.IWindowSession` for **every** `IWindowManager` method (hard-coded),
  which is how the crash read. It now names the interface that was called; pinned by
  the third test above.

## 4. Regression (ART, shared lock, private runtime dir)

All green (`results/handoff/GATENBOOT/regression/<name>/verdict.json`):

```
a11y-probe            pass   sensors-scripted   pass   sensors-off        pass
notif-perm-granted    pass   notif-perm-denied  pass   tail-telephony     pass
tail-bluetooth        pass   location-probe     pass   net-network-off    pass
gaten-bubble          pass
```

`net-network-real` → `GUEST_EXITED` exit 127, `sh: line 1: exec: acos-netd: not
found` — NET5's relay binary and `pasta` are not installed on this host, so that
arm cannot run here. It fails before any guest code and is unrelated to this
branch (the module declares no process initializer; the off arm passes).

`cargo test -p acos-services -p acos-art` — **79 test binaries green, 0 failed**
(`results/handoff/GATENBOOT/tests.log`), including `process_initializers` (rewritten
to the new invariant), `gatenboot_keyguard`, `module_kit`, `sensors_module`,
`moda11y6_gate`, and the launch source guard. Note: run it with
`ACOS_VIEW_TEST_DIR=$PWD/.cache/lv`; without it the `live_view` unit tests build a
unix socket path from the (long) worktree path and fail with `path is too long`,
which is an environment artifact, not a code failure.

`cargo test -p acos-protocol` → `schemas_are_up_to_date` fails with
`["artifacts-validation.json", "envelope.json", "protocol-error.json",
"verdict.json"]` stale; pre-existing on `modules` @ `7c4317ab` (this branch changes
no protocol struct), left alone to avoid colliding with other tracks.

## 5. Repro

```sh
export ACOS_FONT_DIR=/home/Qui/Projects/acos/.cache/boot/android-34/fonts
export ACOS_RUNTIME_DIR=$PWD/.cache/gatenboot-run ACOS_SESSION_SOCKET=$PWD/.cache/gatenboot-run/session.sock
export ACOS_VIEW_TEST_DIR=$PWD/.cache/lv ACOS_ART_BIN=$PWD/.cache/gatenboot-target/release/acos-art
tools/art-runtime/run-exclusive.sh --task GATENBOOT -- tools/guarded.sh --mem 10G --time 600 -- \
  .cache/gatenboot-target/release/acos verify --engine art \
  --scenario scenarios/gaten-bubble.yaml --json --artifacts results/handoff/GATENBOOT/regression/gaten-bubble
```

## 6. What the orchestrator must decide

The bubble needs one of §2 (a) or (b). (b) is smaller and AOSP-faithful and is the
accessibility module's track; (a) is a broadcast subsystem and an architecture
decision (also the reason `ACTION_FLOATING_BUTTON_CHANGED`, `ACTION_SCREEN_ON/OFF`,
`TRIGGER_LOOKUP` and every other runtime-registered receiver is dead on ART today).
Nothing in this branch fakes the overlay: `scenarios/gaten-bubble-overlay.yaml`
stays red with the exact expected failure until one of them lands.

## 7. Environment

Target `.cache/gatenboot-target` (dev/test/release debug=0, incremental off), peak
RSS ~250 MB for the Gaten runs, ~46 GB on disk while the release build existed;
disk 74 % throughout (stop line 86 %), host RAM stayed under 20 GB used. The
`fixtures/build` copy is a hardlink copy of the shared tree; `a11y-probe`,
`net-check`, `sensor-probe`, `tail-probe`, `location-probe`, `notif-perm-probe`
were built into it with `tools/build-apps.sh` (the shared copy had lost them).
`~/Projects/gaten` was never modified and the APK is the one the existing scenarios
use.
