# MODA11Y — the `accessibility` service module, first slice (ADR-0029)

Branch `moda11y` (from `modules` = d5170146), 2026-11-16.

## Status

**PARTIAL.** The module exists, is real for the parts it claims, passes the
conformance kit, and is proven on real ART for the *enabled state + service
lists + event dispatch path*. It does **not** bind the app's
`AccessibilityService`, so the Gaten floating bubble proof (5b) is **not
reachable**; proof 5a is also limited to the same surface. The missing piece is
a feature gap on the ART path, not a sandbox/compositor architecture change
(see "Why the bind is not here").

## What landed

| Item | Where | State |
|---|---|---|
| module `accessibility` at `real`, config `{enable: ["pkg/.Class"]}` | `crates/acos-services/src/modules/accessibility.rs` | DONE |
| writes `Settings.Secure.ACCESSIBILITY_ENABLED` + `ENABLED_ACCESSIBILITY_SERVICES` into the session's settings provider | `modules/accessibility.rs`, `misc.rs::A11yState::enable_services` | DONE |
| `AccessibilityManager` state: `addClient` returns `IntPair.of(flags, events)` (the high word is the state on this image), so `isEnabled()` is true | `misc.rs` | DONE |
| installed/enabled lists as real `AccessibilityServiceInfo` parcels (new `ParcelableSpec`, `AccessibilityServiceInfo.writeToParcel` field order) | `parcelable.rs`, `misc.rs` | DONE |
| `sendAccessibilityEvent` recorded (type + time, bounded 256) through a `codec::read_args_override`; the event parcel has no codec | `codec.rs`, `misc.rs` | DONE |
| `addAccessibilityInteractionConnection` / remove recorded | `misc.rs` | DONE |
| module report details: enabled, enabled services, clients, connections, event counts/types, `service_bound:false`, binding reason | `module.rs::ModuleReport.details`, `modules/accessibility.rs` | DONE |
| control ops `list_events`, `global_action`, `status` | `modules/accessibility.rs` | DONE (BACK/home/recents/notifications refuse with the reason; no fake success) |
| `core` no longer claims the `accessibility` name; it still installs the disabled fallback service | `core_module.rs` | DONE |
| conformance kit | `tests/module_kit.rs` | PASS (11/11, includes `the_accessibility_module_passes_the_conformance_kit`) |
| `a11y-probe` fixture | `fixtures/apps/a11y-probe/`, `tools/build-apps.sh` | DONE |

## ART proof (real framework, shared lock)

`scenarios/a11y-probe.yaml` (5 steps): launch with
`services: [{id: accessibility, profile: real, config: {enable:
["dev.acos.fixtures.a11yprobe/.A11yProbeService"]}}]`; assert the real
`AccessibilityManager` reports `enabled=true installed=1 enabled_list=1`; tap
"Send event"; assert `sent=1`; assert the app log `event_sent`.

```
results/handoff/MODA11Y/artifacts/verdict.json   status=pass, 5/5 steps
results/handoff/MODA11Y/a11y-verdict.json        envelope status=pass
results/handoff/MODA11Y/a11y-launch.json         launch reply, module details
```

Launch reply evidence:

```json
{"id":"accessibility","profile":"real","state":"attached",
 "details":{"enabled":true,
            "enabled_services":["dev.acos.fixtures.a11yprobe/.A11yProbeService"],
            "service_bound":false,
            "event_count":0,
            "clients":0,
            "connections":[]}}
```

`event_count:0` in that row is the **launch-time** snapshot: the launch reply is
built before the scenario taps. The event path is proven by the scenario
(`sent=1` + `event_sent`) and by
`an_event_reaches_the_record_through_the_transaction_path`, which feeds the
real oneway transaction through `ServiceHost::transact` and reads back
`event_count:1, type:1`.

Important environment note: the CLI routes to a shared `acos serve` daemon. An
unrelated older daemon answers the real `IAccessibilityManager` otherwise
(observed: `code 5 unsupported` from the old service). The proof above used a
private daemon:

```bash
export ACOS_RUNTIME_DIR=$PWD/.cache/moda11y-run ACOS_SESSION_SOCKET=$ACOS_RUNTIME_DIR/session.sock
export ACOS_ART_BIN=$PWD/.cache/moda11y-target/release/acos-art
```

## Why the bind is not here (the honest gap)

AOSP's `AccessibilityManagerService` binds the app's service and calls
`IAccessibilityServiceClient.init(IAccessibilityServiceConnection, ...)`. The
ACOS ART path has no system-initiated bind: it has no
`IActivityManager.publishService` handling, no `IApplicationThread.scheduleBindService`
delivery built by the host, and no `IAccessibilityServiceConnection` /
interaction-connection round trip. That is a sizeable feature (a typed `Intent`
delivery, the service-client surface, and the oneway interaction callback
routing), not a forbidden architecture change: it fits the existing
`PendingDelivery` + dynamic-node design. It is **not** implemented here, so:

* `onServiceConnected` has not run;
* `getWindows`, `findAccessibilityNodeInfosBy*`, `performAction`,
  `dispatchGesture`, `takeScreenshot`, `performGlobalAction` from a service are
  not reachable;
* the Gaten APK proof (bubble window type/z, drag, edge clamp, idle fade,
  remove target, screenshot) was **not attempted** and is **not claimed**;
* no Gaten file was modified.

## Tests

```
results/handoff/MODA11Y/unit-tests.log   8/8 accessibility tests
results/handoff/MODA11Y/module-kit.log  11/11 kit tests (adds accessibility)
cargo test -p acos-services --lib        102/102
```

## Honest limits

* `getInstalledAccessibilityServiceList` answers from the launch config, not
  from the APK's manifest: module attach happens before the APK is installed in
  the PMS. The reply's `enabled_services` names exactly what was enabled, so it
  is not a claim about every installed service.
* `clients` is 0: the image's `addClient` carries the app's
  `IAccessibilityManagerClient` as a local binder that the peer currently
  classifies as a host node, so no callable client target is recorded. The state
  still reaches the app through `addClient`'s return value.
* `ACCESSIBILITY_OVERLAY` is listed as a window type the module will own, but
  there is no window code in this slice (LAYER1 already draws application
  overlays; an accessibility overlay needs the bound service to add it).
* detach is **at-launch only in practice**: the module is `hot`, but the settings
  are the only per-session state it owns; the disabled fallback service stays
  registered (owned by `core`), so detaching does not remove the service name.
* `global_action` returns a structured refusal for BACK/HOME/RECENTS/
  NOTIFICATIONS; it never reports an action it did not perform.
