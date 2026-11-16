# MODSENS — the `sensors` service module (v0.4.0), evidence

Branch `modsens`, based on `modules` at `ec1724cf` (MOD0+MOD1, the module
framework tip when this track was dispatched), 2026-11-16. `modules` has since
advanced (`d5170146`, the layer1 merge); a trial merge into that tip conflicts
only in two append-only files (`EXPERIMENTS.md`, `tools/fetch-aosp-reference.sh`)
— resolve by keeping both sides.

**No architectural wall was hit.** The native sensor path needs exactly what this
build already has: an in-process `ServiceHost` that can hand a real descriptor to
the guest (the `BitTube`/`SOCK_SEQPACKET` pair `display_events.rs` already uses
for the frame clock) and a node `core` can publish before the guest boots.

## What landed

| Brief item | Where | State |
|---|---|---|
| The `sensors` module, profiles `off`/`stub`/`scripted`, **no `real`** (manifest says so) | `crates/acos-services/src/modules/sensors.rs` | DONE |
| The **native** protocol: `android.gui.ISensorServer` (getSensorList, getDynamicSensorList, getRuntimeSensorList, createSensorEventConnection, isDataInjectionEnabled, named gaps for the direct channel / operation parameter) + `android.gui.ISensorEventConnection` (getSensorChannel, enableDisable, setEventRate, flush, configureChannel, destroy) + 104-byte `ASensorEvent` packets on a real `BitTube` | `crates/acos-services/src/sensors.rs` (new, ~1900 lines incl. tests-facing API) | DONE |
| Framework-exact sensor list from config (accelerometer, gyroscope, magnetometer, light, proximity, step counter, rotation vector by default) | `sensors::default_sensors` / `SensorDef::flatten` (byte layout of `Sensor::flatten`), config in `modules/sensors.rs::build_feed` | DONE |
| Scripted events at the requested rate | `SensorHub::tick` (period from `enableDisable`/`setEventRate`, continuous sensors stream, on-change sensors report on change, a `play` timeline replaces that sensor's stream) | DONE |
| Control ops `set_value`, `play`, `shake`, `tilt`, `enable`, `disable`, `events`, `reset`, `status` | `modules/sensors.rs::control` | DONE |
| Hot attach | the module installs its feed into the node `core` publishes (same binder id), so an app's `ISensorServer` proxy stays valid; detach clears it (`ServiceHost::dynamic_object_mut` + the documented `as_any_mut` downcast) | DONE (with the documented `SensorManager` list-cache limit) |
| Conformance kit passes | `tests/sensors_module.rs::the_sensors_module_passes_the_conformance_kit` (quiet child, `detach-restores`/`honest-report` PASS) | DONE |
| Delivered events are recorded so a verdict can cite them | `SensorHub.log` ring + module `events` op; the scenario cites it in step 14 | DONE |
| Proof fixture `sensor-probe` + scenario | `fixtures/apps/sensor-probe/` + `scenarios/sensors-scripted.yaml`, `scenarios/sensors-off.yaml` | DONE |
| Scenario step for scripted control | `service_ctl` in `acos-protocol` (schema regenerated), `Backend::service_ctl` + executor branch in `acos-verify`, `TrustGuard`/`EngineBackend`/`ArtSession` forwards, `ctl` op in `acos-art`'s `services` command and in `acos_services` (serve) | DONE |
| Real ART run, guarded + exclusive | `verify.json` / `verify-off.json` (both `status: pass`) | DONE |
| Docs | `docs/AGENT_GUIDE.md` (sensors how-to + `service_ctl`), `docs/AOSP_REFERENCE.md`, `tools/fetch-aosp-reference.sh`, `AOSP_DECISIONS.md` | DONE |

## The native protocol (read from AOSP, verified against the image)

* `frameworks/native/libs/sensor/ISensorServer.cpp` (code order 1..9) and
  `ISensorEventConnection.cpp` (1..6). Both are **hand-written** binder
  interfaces: their proxies read the reply with **no** `binder::Status` header.
  The empty-list placeholder this build shipped for W26 wrote one anyway, which
  the client read as the count — harmless while the list was always empty, and
  the first defect a populated list exposed.
* `Sensor::flatten`/`unflatten` (`libs/sensor/Sensor.cpp:534-609`) and
  `Sensor::getFlattenedSize` (`:517`): `String8` fields (4-byte aligned each), a
  1-byte `bool`, and `Parcel::writeInplace`/`readInplace` padding every element
  to `pad_size(size)` (`libs/binder/Parcel.cpp:1147`, `:1898`). A default
  `Sensor` flattens to 145 bytes → 148 on the wire. **Without that padding every
  sensor after the first desynchronises and the guest logs `Failed to read reply
  from getSensorList` and drops the whole list** (observed in ART run 1, fixed,
  ART run 2 passes).
* `sizeof(ASensorEvent)` = **104** (`0x68`) from the image's
  `SensorEventQueue::read` (`libsensor.so` 0x11dbd: `mov $0x68,%ecx`), layout
  `version@0 sensor@4 type@8 pad@12 timestamp@16 data[16]@24 flags@88
  reserved1@92`; the field reads are `core/jni/android_hardware_SensorManager.cpp`
  (`Receiver::handleEvent`, `:403-450`), which is also where the step counter's
  `u64.step_counter` union member (not a float) comes from.
* `getSensorChannel`'s reply is `gui::BitTube::writeToParcel`: **two** fds
  (receive, send) and nothing else — no AIDL present flag. `BitTube` is the
  `SOCK_SEQPACKET` pair the display-event clock already used
  (`display_events.rs`), so the events are real `send(2)` messages the guest's
  `SensorEventQueue::read` receives with `objSize = 104`.
* `SensorManager.cpp` (`waitForSensorService`, `assertStateLocked`,
  `createEventQueue`) fixes the call order the guest makes; `Sensor.cpp:31-107`
  is where `requiredPermission`/reporting-mode flags come from (step counter →
  `ACTIVITY_RECOGNITION`, light/proximity → on-change).

### What ACOS answered before this change

`core` published a placeholder `sensorservice` (`misc::SensorServer`) that
answered an **empty** `getSensorList` (with an extra exception word in front of
the count) and refused `createSensorEventConnection` as a named
`UNSUPPORTED_ANDROID_API`. That is what the app saw: `getSensorList(TYPE_ALL)`
empty, `getDefaultSensor()` null, no `SensorEventListener` ever fires — see
`verify-off.json` (`sensors: 0`, and the fixture's `accel` row stays `-`).

## ART proof (real ART, `run-exclusive` + `guarded`)

```
tools/art-runtime/run-exclusive.sh --task MODSENS -- \
  tools/guarded.sh --mem 12G --time 900 -- \
  .cache/modules-target/debug/acos verify scenarios/sensors-scripted.yaml --engine art --json
  -> results/handoff/MODSENS/verify.json        status=pass, 16/16 steps
     step 2  sensors: 7
     step 3  accelerometer magnetometer gyroscope light proximity rotation_vector step_counter
     step 6  accel: 1.5 -2.0 9.8          (scripted accelerometer, one real sensor event)
     step 9  light: 1234                  (on-change sensor, reported on change)
     step 12 shake: 11                    (scripted shake, 12 samples -> 11 sign flips)
     step 13 shake ended: 11              (the app's own "shake over" state)
     step 14 delivered=317, events[]      (recorded events, cited in the verdict)
     step 15 detach -> attached = [core] only, sensors row `state: off`

tools/art-runtime/run-exclusive.sh --task MODSENS -- \
  tools/guarded.sh --mem 12G --time 900 -- \
  .cache/modules-target/debug/acos verify scenarios/sensors-off.yaml --engine art --json
  -> results/handoff/MODSENS/verify-off.json    status=pass, 5/5 steps
     sensors: 0, no accel value, `accel_seen` never visible
```

`engine-scripted.json` / `engine-off.json` / `verdict-*.json` are copies of the
run's own artifacts; `logcat-*.txt` is the guest log (the second run logs
`sensors=7 [...]` and the `Sensor` object the framework decoded, which is the
strongest statement that the flattened layout is byte-exact).

## Tests

```
cargo test -p acos-services                     -> 47 suites green (services-tests.log)
cargo test -p acos-services --test sensors_module -> 15 passed (sensors-module-tests.log)
cargo test -p acos-services --test module_kit     -> 10 passed (module-kit.log)
cargo test -p acos-verify -p acos-protocol        -> green (91 + 28 + 66 ...)
cargo test -p acos-mcp --test guide               -> 13 passed (the generated limits row)
```

New tests in `tests/sensors_module.rs`: the `ASensorEvent` layout, the `u64`
step-counter union, the framework sensor table decoded field by field with the
`Sensor::unflatten` reader, a value arriving over a real `BitTube` at the
requested rate, on-change suppression for light, `play`/`shake` timelines,
`flush` → `META_DATA_FLUSH_COMPLETE`, enable/disable/destroy, attach→detach→
stock-empty-list on the same binder, no `real` profile + honest report, `stub`
refusing every scripted op, config selection, and the conformance kit.

Two environment notes for a reviewer re-running this in a worktree:

* `cargo test -p acos-art --lib` and `-p acos-cli` contain Unix-socket tests
  whose scratch path is `<repo>/.cache/...`; a long worktree path exceeds
  `sun_path` and 7 + 2 tests fail with `path too long for a unix socket`
  (**pre-existing**, not this change). Both suites are green with
  `ACOS_VIEW_TEST_DIR=/home/Qui/.cache/acos-view-test` (`art-lib-tests.log`:
  153 passed, 1 ignored).
* `cargo test -p acos-cli` did not compile on the base branch (`annotate` gained
  a 12th argument in MOD1 and two test call sites were not updated); fixed here.

## Fail-before / pass-after

`fail-before.txt`: the module, the `service_ctl` step, the verify seam and the
native sensor protocol do not exist on `modules` (`git ls-tree`/`git grep`
transcripts), and the base's `sensorservice` serves an empty list (the W26 test
pins that), so `sensors: 7` cannot pass there. Every new test fails to compile or
assert on that tree.

## Honest limits

* **No `real` profile.** ACOS has no sensor hardware and no sensor HAL; the
  manifest lists `off`/`stub`/`scripted` only and says so in its summary,
  `ModuleReport.reason` and the verdict row (`fidelity: scripted`).
* Values are agent-chosen, never measured.
* Delivery is **not permission-gated**: the table carries AOSP's
  `requiredPermission` (step counter → `ACTIVITY_RECOGNITION`) but the feed
  reports every enabled sensor to every connection, unlike a phone's
  `sensorservice`. Recorded in the module report and the guide.
* No batching/FIFO: `maxBatchReportLatencyNs` is accepted and ignored (the
  flush-complete event is still delivered, as `SensorService` does).
* A **hot** attach cannot change the list an app already read
  (`SystemSensorManager` caches `mSensorList` per process). The proof attaches at
  launch; a live `acos_services attach sensors:scripted` changes what the *next*
  `SensorManager` in that process (or a new process) sees.
* A `reset` re-applies the scenario's `services:` by design, so the app-visible
  "module off" state is proven by `scenarios/sensors-off.yaml` (a session that
  never attached it) rather than by detach+reset in one run.
* The `sensors` budget (2 MiB / 5 ms) is an estimate, marked `basis:
  estimated` like every other module's.
* RAM/startup A/B was not run (the brief batches benchmarks).

## Not done / next

* `acos_service_ctl` as its own MCP tool: the op is reachable today as
  `acos_services {op: "ctl", id, ctl_op, args}` (and as the `service_ctl`
  scenario step). A dedicated descriptor is a docs/descriptor change for the
  module-API owner.
* `createSensorDirectConnection` (a shared-memory `NativeHandle` channel) stays a
  named gap: it needs a HAL and gralloc, which ACOS does not have.
* The direct-connection/`setOperationParameter` gaps are named, never guessed.
