# MODTAIL — long-tail service modules (v0.4.0), evidence

Branch `modtail` (from `modules`), 2026-11-16. Worktree `t3-eeb71ddf`.

Modules built on the ADR-0029 framework, each with a manifest, the conformance
kit, and an honest report: `net.telephony`, `hw.bluetooth`, `hw.biometric`,
`feedback.vibrator` (scripted profile), and the long-tail stubs/off modules
(`hw.usb`, `system.dropbox`, `hw.nfc`, `system.wallpaper`, `system.print`,
`system.search`, `system.usage`, `system.dpm`, `system.role`, `geo.country`).

Two framework gaps had to be closed for the modules to be reachable at all; both
are in ADR-0029's interface, not new architecture:

1. **`Module::control` now receives `&mut ServiceHost`.** A control op that
   changes what the app observes (an incoming SMS, a call-state change, an
   authentication result) has to deliver it. Without the host, a module could
   only mutate state the app would have to poll for. The peer thread flushes the
   `system -> app` frames a control produced before it serves the next request.
2. **`ModuleManifest.features`.** A module that makes a system feature real
   (`android.hardware.telephony`) publishes it at attach and withdraws it at
   detach, so `PackageManager.hasSystemFeature` cannot disagree with the attached
   set. `SmsManager` is feature-gated on `FEATURE_TELEPHONY_MESSAGING`, so
   without this `getSystemService(SmsManager.class)` is null.

## What landed, per module

| module | profile(s) | provides | control ops |
|---|---|---|---|
| `net.telephony` | off/stub/scripted (at-launch) | `phone` (`com.android.internal.telephony.ITelephony`), `iphonesubinfo`, `telephony.registry`, `isms`; features `android.hardware.telephony[.messaging|.subscription]` | `state`, `set`, `call_state`, `sms`, `reset` |
| `hw.bluetooth` | off/stub/scripted (hot) | `bluetooth_manager` (`android.bluetooth.IBluetoothManager`), `bluetooth` (`android.bluetooth.IBluetooth`); feature `android.hardware.bluetooth` | `state`, `set`, `bonded`, `scan`, `reset` |
| `hw.biometric` | off/stub/scripted (hot) | `auth` (`android.hardware.biometrics.IAuthService`) | `state`, `set`, `auth`, `reset` |
| `feedback.vibrator` | off/stub/scripted (hot) | `vibrator_manager` (unchanged) | `calls`, `patterns` (**new**), `reset` |
| `hw.usb` | off/stub (hot) | `usb` (`android.hardware.usb.IUsbManager`): `getDeviceList` → empty Bundle, `getCurrentFunctions` → 0 | — |
| `system.dropbox` | off/stub (hot) | `dropbox` (`com.android.internal.os.IDropBoxManagerService`): `isTagEnabled` → false, `getNextEntry[WithAttribution]` → null | — |
| `hw.nfc`, `system.wallpaper`, `system.print`, `system.search`, `system.usage`, `system.dpm`, `system.role`, `geo.country` | **off only** | the name each owns (manifest) | — (attach refuses with the AOSP reason) |

Every transaction code is the **image's own**, extracted from
`.cache/boot/android-34/dex/framework*/classes*.dex` (`$Stub` static fields), not
the AOSP tag's (see `results/handoff/MOD0/AOSP_DECISIONS.md` for the table rule).
A method the module does not implement is a structured
`UNSUPPORTED_ANDROID_API`, never a fabricated answer.

## New agent surface

* `acos_service_ctl {module, op, args}` (MCP) and
  `acos_services {op: "ctl", id, module_op, args}` (CLI/engine route).
* The scenario step `service_ctl: {module, op, args}` (schema regenerated).
  `README`-level shape:

```yaml
services:
  - id: net.telephony
    profile: scripted
steps:
  - launch: {activity: dev.acos.fixtures.tailprobe/.MainActivity}
  - service_ctl: {module: net.telephony, op: call_state, args: {state: ringing, number: "+1555…"}}
  - wait_for: {selector: {text: {contains: "CALL state=RINGING"}}, timeout_ms: 20000}
```

Note from writing the proofs: `assert_text` inspects the **current** tree;
`wait_for` is the only primitive that waits for an event. A step that drives an
asynchronous event and then asserts must use `wait_for` (the first version of
these scenarios used `assert_text` and failed while the event did arrive).

## Acceptance evidence

### Tests (host level, no ART)

```
tools/art-runtime/run-exclusive.sh --task MODTAIL -- tools/guarded.sh --mem 12G --time 1600 -- \
  cargo test -p acos-services --offline
  -> 96 passed (lib) + every integration suite (see the transcript below)
cargo test -p acos-services --test module_kit --offline
  -> 12 passed (core, feedback.vibrator, net.telephony, hw.bluetooth, hw.biometric)
cargo test -p acos-services --test telephony_module --offline
  -> 7 passed (scripted answers, SMS send recording, SMS_RECEIVED delivery with a
     real PDU through IIntentReceiver, call-state for PhoneStateListener and
     TelephonyCallback, detach, config)
cargo test -p acos-services --test tail_modules --offline
  -> 7 passed (bluetooth registerAdapter/state/bonded list as a List<BluetoothDevice>
     through ISynchronousResultReceiver/ACTION_FOUND with a BluetoothDevice parcel,
     biometric canAuthenticate/authenticate/cancel, vibrator pattern identity,
     honest reports + `real` refusal)
```

`cargo test -p acos-services --lib telephony` also covers the GSM 03.40 encoder:
an SMS-DELIVER PDU has the expected SCA/OA/BCD/SCTS/UDL fields and the septet
packing round-trips.

### ART scenarios (real ART, `acos verify --engine art`)

| scenario | result | evidence |
|---|---|---|
| `tail-telephony.yaml` | **pass** | `v-telephony.json` (16/16 steps) |
| `tail-biometric.yaml` | **pass** | `v-biometric.json` (14/14) |
| `tail-vibrator.yaml` | **pass** | `v-vibrator.json` |
| `tail-detached.yaml` | **pass** | `v-detached.json` (8/8) |
| `tail-bluetooth.yaml` | **BLOCKED** (step 2 tap) | `v-bluetooth.json` |

What the passing telephony run proves, in the app's own log (tree text in the
verdict):

* `TELEPHONY sim=READY phone=GSM network=LTE call=IDLE line1=+15551234567 operator=""`
  — `TelephonyManager` binder answers, and the operator is the honest empty
  string (a guest *property* no module can set).
* `CALL state=RINGING number=+15550002222` — a scripted call state reached a real
  `PhoneStateListener` callback.
* `SMS from=+15550001111 body=ACOS SMS` — the injected `SMS_RECEIVED` broadcast
  decoded by `Telephony.Sms.Intents.getMessagesFromIntent` →
  `SmsMessage.createFromPdu` (a real GSM 03.40 PDU).
* `SMS-SENT queued to=+15557654321` — `SmsManager.sendTextMessage` through
  `ISms.sendTextForSubscriber`, recorded by the module.

The biometric run proves `BiometricManager.canAuthenticate` and two real
`BiometricPrompt.authenticate` round trips (scripted success `type=2`, scripted
lockout → `AUTH:ERROR code=7`) plus a `set {can_authenticate: none_enrolled}`
re-query. The vibrator run proves `hasVibrator=true` and two recorded patterns.
The detached run proves the missing path: the `phone` lookup NPEs (AOSP's own
client has no telephony module), there is no Bluetooth adapter, `auth`'s
tombstone surfaces as `UnsupportedOperationException`, and
`vibrator_manager`'s tombstone likewise — each reported by the probe, none faked.

Commands (each under the shared lock and guard, `env -u ACOS_HOME`,
`ACOS_RUNTIME_DIR=/run/user/1000/amt`):

```
tools/art-runtime/run-exclusive.sh --task MODTAIL -- tools/guarded.sh --mem 12G --time 800 -- \
  env -u ACOS_HOME .cache/modtail-target/release/acos verify \
  results/handoff/MODTAIL/tail-telephony.yaml --engine art \
  --artifacts results/handoff/MODTAIL/v-telephony --json
```

## Honest limits and open items

* **`hw.bluetooth` does not reach the app yet (BLOCKED).** With the module
  attached, tapping the app's Bluetooth tab blocks before any binder transaction
  the host can see: `BluetoothAdapter.createAdapter` calls
  `ServiceManager.waitForService("bluetooth_manager")` (native →
  `BpServiceManager::waitForService`, an endless `checkService` loop) and the
  guest never observes the name the host registered at launch. The same session
  resolves `phone`, `isms`, `iphonesubinfo`, `telephony.registry` and `auth` and
  transacts with them, so the module's registrations are not the difference; the
  regression is in the guest ServiceManager path for `waitForService`
  (reproducer: `tail-bluetooth.yaml`, step 2). The module's own behaviour is
  proven at the host level (`tail_modules.rs`: registerAdapter hands out the
  adapter binder, state/name/address answers, the bonded list as a real
  `List<BluetoothDevice>` through `ISynchronousResultReceiver`, and
  `ACTION_FOUND` with a `BluetoothDevice` parcel extra). **Next action:** make
  `checkService` answer for a module name over `waitForService` (or return the
  `NotFoundException` instead of looping) and re-run the scenario.
* **A long-lived `PhoneStateListener` is not delivered to.** A listener
  registered at `onCreate` receives the registry's initial (`notifyNow`) state but
  not a scripted change 2+ seconds later; the *same* listener re-registered
  immediately before the change receives it. The fixture therefore registers the
  listener from its "Listen for calls" action (a real app flow) and the scenario
  taps it. Reported as a userspace-binder-driver defect
  (`crates/acos-binder-peer/native/binder_driver.c`: a node exported a while ago
  is not found by `deliver_incoming`/`node_by_wire` — silent drop with only a
  guest-side `WARN`), not as a module limitation.
* **`net.telephony` is `at-launch`.** `TelephonyFrameworkInitializer`'s
  `sTelephonyServiceManager` is installed by the telephony mainline module's
  process on a device; ACOS has no such process, so the session installs it once
  in the guest before the app boots (`ACOSLAUNCH_TELEPHONY=1`, set only when the
  module is attached). A live attach could serve `phone` but every
  `TelephonyManager` call would still NPE on the null manager, hence at-launch.
  The same applies to `hw.bluetooth` (`ACOSLAUNCH_BLUETOOTH=1` installs
  `BluetoothServiceManager`).
* **Telephony gaps, in every report:** no radio; `getSimOperator*`,
  `getSimOperatorName`, `getSimCountryIso` read `gsm.sim.operator.*` *system
  properties*, which a module cannot set (the guest property table is written at
  launch) — they stay `""`; no `isub` (binder calls arrive with `subId = -1` and
  are answered by slot 0); sent/delivery `PendingIntent`s are recorded but never
  fired (no scheduler); 7-bit GSM only (a body outside the default alphabet or
  longer than 160 septets is refused, a `pdu` hex override exists); no
  `SubscriptionInfo`, no cell info.
* **Biometrics:** no prompt UI (window layer stack gap, said in the report and in
  every `auth` reply: `prompt_ui: "none …"`); `CryptoObject` operation ids are
  recorded, never unlocked; `fingerprint`/`face`/`iris` stay tombstones.
* **Bluetooth:** no radio/BLE/GATT (`BluetoothLeScanner` answers
  `UNSUPPORTED_ANDROID_API`), no profiles, no pairing. Discovery is the classic
  `startDiscovery` + `ACTION_FOUND` path.
* **Vibrator:** patterns are recorded as `CombinedVibration` token + payload
  length + FNV-1a hash of the request parcel; the nested
  `VibrationEffect`/`VibrationAttributes` parcelables are not decoded (the report
  says so).
* **Long tail:** `hw.usb` and `system.dropbox` answer only the empty/false
  values; the other eight names stay `off` with the AOSP reason (NFC throws
  before any binder, wallpaper's null is `DisabledWallpaperManager`, print is
  feature-gated, search/usage/dpm have no honest empty answer, `IRoleManager`'s
  table is not in the image dex, a country would be a location claim).
* **`role`** could be a real `stub` (`isRoleAvailable=false`,
  `getRoleHolders=[]`) if the Permission APEX's dex is added to the reference
  cache; today the codes would be a guess.
* Budgets are `estimated` (1 MiB / 5 ms per scripted module, 1/1 for off-only
  modules because the manifest floor is 1); no RAM measurement was run (batched
  by the bench owner).
* `tools/build-apps.sh` fails on `keystore-probe` in this checkout (`dev.acos.art`
  classes are not on the javac classpath; it needs the acos-art guest sources).
  `tail-probe.apk` is built before that step; the failure is pre-existing and
  unrelated.

## Files

* `crates/acos-services/src/modules/{telephony,bluetooth,biometric,tail_stubs}.rs`
* `crates/acos-services/src/modules/vibrator.rs` (scripted patterns)
* `crates/acos-services/src/{module,host,ams,atms,parcelable,value}.rs` (control
  host seam, deliveries, `VAL_LIST`/`VAL_OBJECTARRAY`, module features)
* `crates/acos-art/src/launch.rs`, `crates/acos-art/guest/java/dev/acos/art/LaunchMain.java`
  (ctl flush, mainline initializer install, guest env)
* `crates/acos-binder-peer/src/peer.rs` (delivery flush, reverse-delivery
  exception record)
* `crates/acos-cli/src/engine/{art,serve,mod}.rs`, `crates/acos-verify/src/{backend,executor}.rs`,
  `crates/acos-protocol/src/scenario.rs` + `schema/scenario.json`
* `crates/acos-mcp/src/{descriptors,help}.rs`, `crates/acos-cli/src/engine/serve.rs`
  (`acos_service_ctl`)
* `fixtures/apps/tail-probe/` (the tab-per-module probe), `tools/build-apps.sh`
