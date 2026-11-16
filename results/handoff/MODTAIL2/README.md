# MODTAIL2 — the modules/modtail merge, the Bluetooth proof, and generic
# process initializers (v0.4.0)

Branch `modtail2` = `modules` @ `e1b9e3b9` (MOD0+MOD1+LAYER0/1+MODSENS) with
`modtail` @ `4adef5f0` merged in, then the four follow-ups. Worktree
`t3-57df215b`. All ART runs under `run-exclusive` + `guarded`, private
`ACOS_RUNTIME_DIR`/`ACOS_SESSION_SOCKET` under the worktree.

## 1. The merge — one module-control path

MODSENS and MODTAIL both built live module control. The merge keeps **one** of
each, and every module's ops keep working:

| surface | before the merge | after |
|---|---|---|
| engine trait (`acos-cli`) | `service_op(request)` (MODSENS) **and** `service_ctl(module, op, args)` (MODTAIL) | `SessionBackend::service_op(request)` only; a control op is the request `{op: "ctl", id, ctl_op, args}` |
| verify seam | `Backend::service_ctl(ServiceCtlRequest)` **and** `EnvironmentChange::ServiceCtl` | `Backend::service_ctl(ServiceCtlRequest)` only (`EnvironmentChange::ServiceCtl` removed, its executor arm was unreachable) |
| MCP | `acos_services {op: "ctl", id, ctl_op}` **and** `acos_service_ctl {module, op, args}` | the two shapes ADR-0029 item 4 names: `acos_services {op: list\|status\|attach\|detach}` for the lifecycle, `acos_service_ctl {module, op, args}` for a control op. `acos_services {op: "ctl"}` is refused with a message naming `acos_service_ctl` |
| wire field | `ctl_op` (MODSENS) / `module_op` (MODTAIL) | `ctl_op` |
| scenario step | MODSENS superset (module/op/profile/config/args) / MODTAIL (module/op/args) | the superset: `attach`/`detach` (profile/config) and every module control op (args). Validation also refuses a control step whose module nothing in the run attaches (unless an earlier `service_ctl {op: attach}` step does) |
| `Module::control` | `control(op, args)` (MODSENS) / `control(host, op, args)` (MODTAIL) | `control(&mut ServiceHost, op, args)` — the module gets the host, so a control that changes what the app observes can deliver it (the ADR-0029 "core publishes, module fills" note) |

Regenerated `crates/acos-protocol/schema/scenario.json`
(`UPDATE_SCHEMA=1 cargo test -p acos-protocol`; `schemas_are_up_to_date` passes).

Sensor/location/telephony/bluetooth/biometric/vibrator all still answer: the
module suites are green (below) and the ART runs below are the merged tree.

## 2. The Bluetooth ART proof

`results/handoff/MODTAIL/tail-bluetooth.yaml`, 14 steps. **13 pass, step 08
(`BONDED device=ACOS Headset …`) still times out** — see "What remains".

### The root cause is not `waitForService` looping

The dispatch note said `ServiceManager.waitForService("bluetooth_manager")`
loops on `checkService`. The ART run shows `bluetooth_manager` resolving: the
app gets a non-null `BluetoothAdapter` (`BluetoothAdapter.createAdapter` returns
null only when the manager is null) and reads the scripted name/address through
`IBluetoothManager`. What actually blocked was the **next** call, and the chain
had three links:

1. **`IBluetooth.getState` was answered as a blocking reply on a oneway call.**
   Every `IBluetooth` method in the android-34 `framework-bluetooth` AIDL is
   `oneway (..., in SynchronousResultReceiver)`
   (`packages/modules/Bluetooth/android/app/aidl/android/bluetooth/IBluetooth.aidl:52-103`),
   and `BluetoothAdapter.getStateInternal` builds a `SynchronousResultReceiver`
   and calls `getState(receiver)` (image dex,
   `BluetoothAdapter$3`/`getStateInternal`). A decoded reply is dropped by the
   caller, so the app waited out `getSyncTimeout()` on the UI thread:
   first fix.
2. **The imported request had an empty object table** — `Parcel::unmarshall`
   copies bytes only, so `import_parcel` lost the driver's offsets and the module
   could not see the client's receiver at all. Evidence from the run:
   `code 1: no ISynchronousResultReceiver binder in the request (0 object(s), 104 byte(s))`
   twice per session (read back from `hw.bluetooth`'s own `state` control op in
   `v-probe/steps/03-service_ctl.json`). Fixed in `acos-binder-peer`.
3. **The frame then still failed to decode**: (a) the receiver's interface
   descriptor is the *module-jar* name the client enforces
   (`com.android.bluetooth.x.com.android.modules.utils.ISynchronousResultReceiver`,
   read from `framework-bluetooth/classes.dex`), not the shared library name;
   (b) `Result.writeToParcel` writes **`mObject` first, then `mException`**
   (image dex `SynchronousResultReceiver$Result.writeToParcel`), and the reverse
   order throws inside the client:
   `java.lang.ClassCastException: java.lang.Integer cannot be cast to java.lang.RuntimeException`
   at `SynchronousResultReceiver$Result.<init>(SynchronousResultReceiver.java:137)`.

Effect, same scenario, before → after: the Bluetooth tab's tap settle goes from
**no frame at all (probe-floor timeout, 3.3 s)** to **first change in 119 ms**
(`v-bluetooth/steps/02-tap.json`), `BLUETOOTH state=OFF name=ACOS Scripted
address=02:00:00:00:00:01` appears, `acos_service_ctl {op:set, args:{on:true}}`
delivers `BTSTATE ON` (a real `ACTION_STATE_CHANGED`), and the re-tap shows
`BLUETOOTH state=ON`.

### ServiceManager (the class the dispatch note named)

Implemented for the whole class, per AOSP
(`frameworks/native/cmds/servicemanager/ServiceManager.cpp:345-380, 480-610`):

* `getService`/`checkService` answer every registered name and always return
  `Status::ok` with a possibly-null binder (AOSP: "returns ok regardless of
  result for legacy reasons").
* `registerForNotifications(name, callback)` keeps the callback, fires it at once
  when the service already exists (AOSP does), and fires it on the live module
  attach that registers the name — the `waitForService` path.
* `unregisterForNotifications` answers `EX_ILLEGAL_STATE` when nothing is
  pending (AOSP: "Nothing to unregister.").
* `isDeclared` now answers for module-declared names (AOSP answers from the VINTF
  manifest), so `waitForDeclaredService` has something to wait on.
* A name that is neither registered nor declared by a shipped module is refused
  with `EX_ILLEGAL_ARGUMENT` naming it (deliberate difference from AOSP, which
  accepts a registration that can never fire; ACOS has no VINTF manifest and no
  lazy service start, so accepting it can only hang the caller — documented in
  the code and `results/handoff/MODTAIL2/README.md`).
* `IServiceCallback.onRegistration` travels as `@utf8InCpp String` +
  `IBinder` (`IServiceCallback.aidl`), i.e. `writeString8`.

Tests: `crates/acos-services/tests/service_manager_notifications.rs` (7).

## 3. Generic process initializers (`ModuleManifest.process_initializers`)

`ModuleManifest` gains `process_initializers: &'static [ProcessInitializer]`
(class, static setter, manager class, AOSP evidence). The launcher forwards the
**attached** set as `ACOSLAUNCH_INITIALIZERS` (`class#method#manager;…`); the
guest runs one reflective loop before `Application.onCreate` and logs
`ACOSLAUNCH MARK process-initializer installed …`. `ACOSLAUNCH_TELEPHONY` and
`ACOSLAUNCH_BLUETOOTH` are gone, as are their two hand-written guest blocks.

* `net.telephony` → `android.telephony.TelephonyFrameworkInitializer#setTelephonyServiceManager`
* `hw.bluetooth` → `android.bluetooth.BluetoothFrameworkInitializer#setBluetoothServiceManager`
* every other module declares none (`modules_that_need_no_initializer_declare_none`).

Tests: `crates/acos-services/tests/process_initializers.rs` (7) +
`launch::skipped_traversal_tests::the_process_initializer_install_is_driven_by_the_manifest`
(a guest-source guard against reintroducing a per-module switch).

ART proof that the generic path works: `tail-telephony.yaml` 14/14 **pass**
against the merged tree with `net.telephony` attached (`v-telephony/`) — its
`phone`/`isms`/`iphonesubinfo` binder resolution and the delivered
`SMS_RECEIVED`/call-state events depend on that initializer.

## 4. `tools/build-apps.sh` (the pre-existing keystore-probe failure)

The ACOS guest classes are a runtime classpath; `keystore-probe` calls
`dev.acos.art.SoftwareKeyStore`, which javac could not see. `build_apk` now
compiles with `-sourcepath crates/acos-art/guest/java -implicit:none`: the guest
sources are type-checked, no class files are emitted, and the APK dex keeps
exactly the fixture's own classes (verified with `dexdump`: the only
`dev/acos/...` class descriptors are the fixture's). `tools/build-apps.sh` runs
to completion and builds every fixture, `tail-probe.apk` and
`keystore-probe.apk` included.

## 5. Long-tail off-only names

Unchanged, as the dispatch says: `hw.nfc`, `system.wallpaper`, `system.print`,
`system.search`, `system.usage`, `system.dpm`, `system.role`, `geo.country` stay
`off` with their AOSP reason (`module.rs::TOMBSTONES` /
`tail_stubs`), reported in every verdict row.

## Verification

```
cargo test -p acos-services                     -> 51 suites green, 0 failed
                                                   results/handoff/MODTAIL2/services-tests.log
cargo test -p acos-services --test module_kit --test sensors_module \
  --test telephony_module --test tail_modules \
  --test process_initializers --test service_manager_notifications
                                                -> 12 + 15 + 7 + 7 + 7 + 7 passed
                                                   results/handoff/MODTAIL2/module-kits.log
cargo test -p acos-verify -p acos-protocol      -> 92 + 28 + schemas 2 passed
cargo test -p acos-mcp                          -> green (incl. docs table == descriptors)
cargo test -p acos-binder-peer -p acos-parcel   -> 16 + 15 + 3 + 12 + 9 passed
cargo test -p acos-art --lib the_process_initializer -> 1 passed
```

ART (`acos verify --engine art`, merged tree):

| scenario | result | evidence |
|---|---|---|
| `results/handoff/MODTAIL/tail-telephony.yaml` | **pass** 14/14 | `v-telephony/` |
| `scenarios/sensors-scripted.yaml` | **pass** 16/16 | `v-sensors-scripted/` |
| `scenarios/sensors-off.yaml` | **pass** 5/5 | `v-sensors-off/` |
| `results/handoff/MODTAIL/tail-bluetooth.yaml` | **FAIL** 13/14, step 08 | `v-bluetooth/` (step 8's timeout) |
| `probe-bluetooth-hang.yaml` (diagnostic) | pass (tap settles) | `v-probe/` |
| `probe-bluetooth-bonded.yaml` (diagnostic) | pass; `bonded_calls: 1`, app prints `BONDED null` | `v-bonded/` |

Known environment failure, not from this work:
`cargo test -p acos-cli --test fb1_live_input` fails
`ambiguous_assertion_is_refused_with_candidates` ("serve socket never
appeared") because the test's scratch tag makes the serve socket path 109 bytes
and `bind(2)` answers `AF_UNIX path too long` in this worktree
(`python3 -c 'socket.bind(...)'` reproduces it). The other three tests in the
file pass.

## What remains (smallest next action)

`IBluetooth.getBondedDevices` is answered (the module's `bonded_calls` counter
is 1 and `unsupported` is empty — `v-bonded/steps/04-service_ctl.json`), the
frame is delivered, but the app reads `Result.mObject` as null and prints
`BONDED null` (`v-bonded/frames/10.png`). So the last link is the **typed-list
payload** inside `Result`: `List<BluetoothDevice>` must be written the way the
client's `readValue` reads a list of parcelables (and `BluetoothDevice`'s own
field encoding checked against `BluetoothDevice.CREATOR`). Everything else in
`tail-bluetooth.yaml` (state, state-changed broadcast, name, address, the second
tap) now passes. Next action: fix `parcelable::write_dynamic(List)` / the
`BluetoothDevice` parcelfield layout and re-run the scenario; no architectural
change is involved.

## Files

* `crates/acos-services/src/module.rs` (`ProcessInitializer`,
  `process_initializers`, `provided_names`, `declare_modules`),
  `src/host.rs` (`registerForNotifications`/`unregisterForNotifications`,
  `declare_service_name`, `notify_registered`, `registration_delivery`),
  `src/modules/{telephony,bluetooth,sensors,biometric,tail_stubs,vibrator,core_module}.rs`
* `crates/acos-binder-peer/src/peer.rs` (`import_parcel` object table),
  `crates/acos-parcel/src/parcel.rs` (`register_object`)
* `crates/acos-art/src/launch.rs`, `crates/acos-art/guest/java/dev/acos/art/LaunchMain.java`
* `crates/acos-cli/src/engine/{mod,art,serve}.rs`, `crates/acos-verify/src/{backend,executor}.rs`,
  `crates/acos-protocol/src/scenario.rs` + `schema/scenario.json`
* tests: `process_initializers.rs`, `service_manager_notifications.rs`, `tail_modules.rs`,
  `acos-binder-peer/tests/peer.rs`
* `tools/build-apps.sh`, `tools/fetch-aosp-reference.sh`
