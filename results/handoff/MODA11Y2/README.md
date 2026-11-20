# MODA11Y2 — the accessibility module's system → app bind (step 1 of 4)

Branch `moda11y2` (from `moda11y` = 4c2a1111), 2026-11-16.
Worktree `.t3/worktrees/acos/t3-f9587ae1-3f15c4b68f82fc39`.

## Status

**PARTIAL — step 1 passes on real ART; steps 2, 3 and the Gaten proof are
NOT done.** The `AccessibilityService` is now bound by the system the way
`AccessibilityManagerService` binds it, and `onServiceConnected` provably runs
on the ART guest with the `AccessibilityServiceInfo` the framework was handed.
The service-facing **interaction** surface (`getWindows`,
`getRootInActiveWindow`/node queries, `performAction`, `dispatchGesture`,
`takeScreenshot`), the `TYPE_ACCESSIBILITY_OVERLAY` window and the Gaten bubble
proof are not implemented (they are the next steps, see "Remaining").

Per the orchestrator's decisions:

* `getInstalledAccessibilityServiceList` now reads the **package manager**
  (`android.permission.BIND_ACCESSIBILITY_SERVICE` services from the installed
  APK's manifest, lazily), not the launch config. The launch config only says
  what is *enabled*.
* the bind is a feature inside the existing `PendingDelivery` + dynamic-node
  design. No architecture change was needed and none was made.

## What landed

| Item | Where | State |
|---|---|---|
| installed list from the package manager (lazy, after install) | `misc.rs::A11yState::installed_services`, `A11yService::call` | DONE |
| `IApplicationThread.scheduleCreateService` + `scheduleBindService` delivery, host-built `Intent` (`action=android.accessibilityservice.AccessibilityService`, enabled component), `ServiceInfo` from the APK, `CompatibilityInfo.DEFAULT` | `a11y_bind.rs::plan_bind` | DONE |
| app thread captured from `attachApplication` (the bind has no target before it) | `host.rs::transact_inner` | DONE |
| `IActivityManager.publishService` answered by the host: token must match the pending bind, null binder refused | `host.rs` + `a11y_bind.rs::on_publish_service` | DONE |
| `IAccessibilityServiceClient.init(IAccessibilityServiceConnection, connectionId, windowToken)` delivered to the app's `onBind` binder | `a11y_bind.rs::init_delivery` | DONE |
| `IAccessibilityServiceConnection` as a **dynamic node** (`setServiceInfo`, `setAttributionTag`, `getServiceInfo`, `getSystemActions`, `disableSelf`; everything else a structured refusal) | `a11y_bind.rs::A11yConnectionNode` | DONE (5 methods of ~70) |
| `IActivityManager.serviceDoneExecuting` void no-op (the call `ActivityThread` makes right after `Service.onCreate`) | `ams.rs` | DONE |
| `A11yState` client/bind bookkeeping in the module report (`service_bound`, `bound_service`, `bind_pending`, `installed_services`, `clients`) | `misc.rs::to_json`, `modules/accessibility.rs` | DONE |

## ART proof (shared lock, private runtime dir)

Environment (the shared `acos serve` daemon answers with stale code — see the
MODA11Y README):

```bash
export ACOS_FONT_DIR=/home/Qui/Projects/acos/.cache/boot/android-34/fonts
export ACOS_RUNTIME_DIR=$PWD/.cache/moda11y2-run
export ACOS_SESSION_SOCKET=$ACOS_RUNTIME_DIR/session.sock
export ACOS_ART_BIN=$PWD/.cache/moda11y2-target/release/acos-art
tools/art-runtime/run-exclusive.sh --task MODA11Y2 -- \
  tools/guarded.sh --mem 8G --time 600 -- \
  .cache/moda11y2-target/release/acos verify scenarios/a11y-probe.yaml \
  --engine art --artifacts results/handoff/MODA11Y2/artifacts
```

```
results/handoff/MODA11Y2/artifacts/verdict.json   status=pass, 6/6 steps
results/handoff/MODA11Y2/a11y-verdict.json        copy of the verdict
results/handoff/MODA11Y2/a11y-verify.err|.out      the run's stderr/stdout
```

The step that matters (new in MODA11Y2, `scenarios/a11y-probe.yaml`):

```yaml
  - assert_log:
      tag: AcosA11yProbe
      patterns: ["service_connected"]
```

```
results/handoff/MODA11Y2/artifacts/logs/logcat.txt
I AcosA11yProbe: service_connected id=dev.acos.fixtures.a11yprobe/.A11yProbeService eventTypes=0 flags=0
```

That line is printed by the fixture's own `AccessibilityService.onServiceConnected`,
and `getServiceInfo().getId()` is the component the system connected it with —
i.e. it came back from the host's `IAccessibilityServiceConnection.getServiceInfo`
(transaction 11 of the dynamic node), not from the app.

The 5 pre-existing steps (isEnabled, installed/enabled lists, event path) still
pass: the module's `installed=1` in the app's status line now comes from the
**package manager** (`PmsState` parsed the APK's manifest), which is what the
launch config no longer supplies.

`unsupported.jsonl` after the run holds 2 records, both unrelated to the bind
and both pre-existing classes:

```
android.app.IActivityClientController 10 reportSizeConfigurations   SERVICE_BAD_REQUEST
android.view.accessibility.IAccessibilityManager 31 setAccessibilityWindowAttributes  (no AccessibilityWindowAttributes codec)
```

## Tests

```
crates/acos-services/tests/moda11y2_bind.rs   4/4 pass
  the_installed_list_is_read_from_the_package_manager
  add_client_creates_and_binds_the_enabled_service          (2 deliveries, Intent action+component+rebind)
  publish_service_answers_init_with_a_connection_node       (init target/connection id/window token, getServiceInfo)
  a_foreign_publish_service_is_refused
cargo test -p acos-services --lib              102/102 pass
cargo test -p acos-services --test module_kit  11/11 pass
cargo test -p acos-services --test parcelable  3/4 — the 4th is PRE-EXISTING on
  moda11y (AccessibilityServiceInfo: "consumed 22/22 calls, spec 23 fields");
  the field list matches AOSP's writeToParcel, the dex scanner misses the last
  call. Unchanged by this branch (verified by stashing the branch).
```

The first failing step of this task was a real one: `AccessibilityService`'s
wrapper calls `IAccessibilityServiceConnection.setAttributionTag` immediately
after `init`, and refusing it aborted the wrapper **before** `onServiceConnected`
(`LOOPER_THREW … IAccessibilityServiceConnection code 2`, kept as evidence in
`artifacts` history? no — the run that produced it was overwritten; the fix is
`a11y_bind.rs` `setAttributionTag`).

## Honest limits (what is NOT claimed)

* **Interaction surface (step 2) is not implemented.** A bound service can call
  `getServiceInfo`/`setServiceInfo`/`getSystemActions`/`disableSelf` and nothing
  else; `getWindows`, node queries, `performAction`, `performGlobalAction`,
  `dispatchGesture`, `takeScreenshot` are structured `UNSUPPORTED_ANDROID_API`
  refusals. No partial success is reported anywhere.
* **No `TYPE_ACCESSIBILITY_OVERLAY` window (step 3).** The module still lists
  the type it will own; LAYER1 draws application overlays, the a11y overlay is
  not wired.
* **Gaten (step 4b) was not run.** Its manifest (`gaten-dev-current-x86_64-release.apk`,
  read-only) declares `com.gaten.app.GatenAccessibilityService` with
  `android.permission.BIND_ACCESSIBILITY_SERVICE` and the accessibility intent
  filter, and **no `android:process`**, so nothing in it needs mproc1's second
  process; it is blocked only on steps 2/3 (the bubble is an
  `AccessibilityService`-owned `TYPE_ACCESSIBILITY_OVERLAY` window and the
  remove target needs node queries/gestures).
* `AccessibilityServiceInfo` is built from the manifest's scalar
  `<accessibility-service>` attributes that the parser records
  (`accessibilityEventTypes`, `accessibilityFeedbackType`,
  `accessibilityFlags`, `notificationTimeout`); `android:description`,
  `packageNames` and `canRetrieveWindowContent` are not applied yet.
* the module report in a **launch reply** is a pre-install snapshot
  (`installed_services: []`, `service_bound: false`) because the reply is built
  before the APK is installed and before the app calls `addClient`. The bind
  state is visible through the `status` control op / a later report, and is
  asserted end-to-end by the scenario instead.

## Remaining (smallest next action first)

1. **(step 2, smallest first)** `getWindows`/`getWindow`: the LAYER1 window
   stack already has the windows; answer `AccessibilityWindowInfo.WindowListSparseArray`
   from it and give each window its registered
   `IAccessibilityInteractionConnection` (already recorded in
   `state.connections`). Then route `findAccessibilityNodeInfoByAccessibilityId`
   /`ByText`/`ByViewId` to the window's connection with a reverse delivery and
   let the app's callback arrive (both binders are guest-local, so the callback
   needs no host round trip). This needs `AccessibilityWindowInfo` +
   `AccessibilityNodeInfo` parcel writers and the throttling interaction ids.
2. **(step 2 continued)** `performAction` (same route, `boolean` reply),
   `performGlobalAction` (BACK through the session's existing Back path;
   HOME/RECENTS/NOTIFICATIONS stay structured unsupported until system apps),
   `dispatchGesture` (no REL3S gesture stream in this base: taps through the
   existing input dispatch, state that), `takeScreenshot` from the compositor
   frame honouring `FLAG_SECURE`.
3. **(step 3)** `TYPE_ACCESSIBILITY_OVERLAY` windows added by the bound service
   drawn above the app on the LAYER1 stack, window-tagged in the tree, taps
   routed to them.
4. **(step 4)** the a11y-probe bubble scenario (window type/z via
   `assert_windows`, swipe, tap handled by the overlay, `takeScreenshot`
   returns a frame), then Gaten: enable, `onServiceConnected`, bubble, drag,
   edge clamp, idle fade, remove target.

## Environment notes

* `fixtures/build` in this worktree is a **copy** (not a hardlink farm) because
  the fixture APK had to be rebuilt; `tools/build-apps.sh` stops at
  `keystore-probe` (pre-existing compile error in that fixture, unrelated).
* the target directory is `.cache/moda11y2-target` (deleted after the last run).
