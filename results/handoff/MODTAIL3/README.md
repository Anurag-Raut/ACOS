# MODTAIL3 — the Bluetooth bonded list, the short-extra gap, and the tail re-runs

Branch `modtail3` = `modules` @ `cfed3eea` (MODTAIL2) + three commits.
Worktree `t3-92c2445d`. Every ART run under `run-exclusive.sh` + `guarded.sh`
with a private `ACOS_RUNTIME_DIR`/`ACOS_SESSION_SOCKET` under the worktree.

## 1. `hw.bluetooth` — the bonded list never decoded (root cause, not a codec guess)

MODTAIL2 left step 08 failing with the app printing `BONDED null`. The run it
installed (`results/handoff/MODTAIL2/v-bonded/logs/logcat.txt:121`) already had
the answer in it:

```
E BluetoothAdapter: java.util.concurrent.TimeoutException
    at android.bluetooth.BluetoothAdapter.getBondedDevices(BluetoothAdapter.java:2941)
```

`BluetoothAdapter.getBondedDevices` catches that timeout itself and returns
**null** (`const/4 v0, #0; return-object v0` at the end of the image method's
catch block), which is the `BONDED null` the fixture printed. The timeout is the
consequence: the client's `Result(Parcel)` threw while decoding our frame, the
exception died in the receiver's `onTransact`, and the waiter expired.

Two independent bugs produced that, and both are now fixed at their class:

1. **`BluetoothDevice.writeToParcel` writes `writeString`, not `writeString8`.**
   The parcelable spec had `FieldOp::Str8` for `mAddress`, so the address went
   on the wire as 8-bit bytes where the image's `Parcel.writeString`/`readString`
   is UTF-16 (image dex `BluetoothDevice.writeToParcel` → `Parcel.writeString`
   → `BluetoothDevice(Parcel)` → `Parcel.readString`). Every byte after the
   address desynced, so the whole `Result` was abandoned. The spec is now
   `[FieldOp::Str16, FieldOp::Int]` with `Verification::Full`, and the new
   real-Java differential proves the bytes.
2. **`BluetoothDevice.getName()` needs `IBluetooth.getRemoteName`.** With the
   list decoding, the app printed `BONDED device=null 11:22:33:44:55:66`: the
   address was right and the name was not, because `getName()` is a
   `SynchronousResultReceiver` call of its own (transaction code **32**,
   declaration order in `packages/modules/Bluetooth/android/app/aidl/android/
   bluetooth/IBluetooth.aidl`: `getState` 1 … `getBondedDevices` 23,
   `getBondState` 27, `getRemoteName` 32). The module now answers it from the
   scripted bonded/scan sets; a device it does not know answers a **null** name,
   the same answer AOSP's `BluetoothService.getRemoteName` gives.
3. **`BluetoothDevice.EXTRA_RSSI` is a short extra** — found by the same
   scenario one step later (`FOUND name=ACOS Beacon … rssi=0`). The fixture reads
   it with `Intent.getShortExtra`, i.e. `Bundle.getShort` cast to
   `java.lang.Short`; an `Integer` value is a `ClassCastException` warning and
   the caller's default. The codec had **no `VAL_SHORT` at all**, so this was a
   gap for the whole class of short extras, not for Bluetooth: `val::SHORT`,
   `DynamicValue::Short`, `Value::Short` and `ams::Extra::Short` are now
   modelled (`write_dynamic`/`read_dynamic`, `Bundle`, intents, and the VM
   extra re-materialisation path), and `EXTRA_RSSI` travels as a short.

### The real-Java byte differential (same harness as MODNOTIF4)

`crates/acos-runtime/tests/java/ParcelableCompareMain.java` gains the two
`SynchronousResultReceiver.Result` shapes the module delivers — `writeValue(
mObject)` then `writeValue(mException)`, exactly the image's
`Result.writeToParcel`:

* `bluetooth_result`: a `List<BluetoothDevice>` of two devices,
* `bluetooth_state`: an `Integer` (12 = `STATE_ON`),
* `bundle_short`: a `Bundle` with `"android.bluetooth.device.extra.RSSI" =
  (short) -40`.

`crates/acos-runtime/tests/parcelable_java.rs` encodes the same logical values
with `acos-services::parcelable`, asserts **byte equality** with the real
framework writer, feeds the Rust bytes back to the real reader
(`read.bluetooth_result=ok`, `read.bundle_short=ok`) and compares the real
`writeToParcel` re-emission. Before the fix the differential showed the exact
diff (class name `21 00 00 00 61 00 6e 00 …` UTF-16 vs `31 31 3a 32 32 …`
8-bit), after it the two streams are identical.

```
cargo test -p acos-runtime --test parcelable_java -> 1 passed, 0 failed
```

## 2. The tail scenario runs (ART, this tree)

| scenario | result | evidence |
|---|---|---|
| `results/handoff/MODTAIL/tail-bluetooth.yaml` | **pass 14/14** | `v-bluetooth/` |
| `results/handoff/MODTAIL/tail-biometric.yaml` | **pass 14/14** | `v-biometric/` |
| `results/handoff/MODTAIL/tail-vibrator.yaml` | **pass 10/10** | `v-vibrator/` |
| `results/handoff/MODTAIL/tail-detached.yaml` | **pass 8/8** | `v-detached/` |
| `results/handoff/MODTAIL/tail-telephony.yaml` | **pass 14/14** | `v-telephony/` |
| `scenarios/sensors-scripted.yaml` | **pass 16/16** | `v-sensors-scripted/` |
| `scenarios/sensors-off.yaml` | **pass 5/5** | `v-sensors-off/` |

The bluetooth run is the one that changed: `BONDED device=ACOS Headset
11:22:33:44:55:66`, `FOUND name=ACOS Beacon address=AA:BB:CC:DD:EE:01 rssi=-40`
and `FOUND name=ACOS Tag … rssi=-66` are all real client reads (the tree at
`v-bluetooth/tree/34.json`). The other six scenarios were re-run on this tree and
were green without changes — the `VAL_SHORT` and `BluetoothDevice` fixes are
additive and none of them touched another module's wire.

## 3. The two documented ServiceManager deviations (kept, now in `docs/`)

The orchestrator decided to keep both deviations from AOSP. They are now
documented as intentional, in the code and in `docs/`:

| deviation | AOSP | ACOS | where |
|---|---|---|---|
| `registerForNotifications` for a name nothing can ever provide | accepts it, never fires the callback | refuses with `EX_ILLEGAL_ARGUMENT` naming the name (ACOS has no VINTF manifest and no lazy service start, so waiting would hang forever) | `docs/FINDINGS.md` **F-0015**, `crates/acos-services/src/host.rs` module docs + the arm |
| `isDeclared` | answers from the device's VINTF manifest | answers from the loaded module manifests' `provided_names` | same |

## 4. The `fb1_live_input` path-length failure (INTEG030's fix does not cover it)

`git show integ030 --stat` shows `b8dba361` "acos-art live_view tests use a
sun_path-safe scratch root" — that is `crates/acos-art/src/live_view.rs` only
(the `live_view` tests' `stream.sock`). It does **not** cover the `acos-cli`
test, so the same pattern was applied there: `crates/acos-cli/tests/
fb1_live_input.rs` now picks its scratch root short (`ACOS_FB1_TEST_DIR` when it
fits the budget, else `/dev/shm/acos-fb1-tests`, else the checkout when short
enough), and the serve socket is `<root>/fb1-test-<tag>-<pid>/serve.sock`.

```
cargo test -p acos-cli --test fb1_live_input -> 4 passed, 0 failed
```
(`ambiguous_assertion_is_refused_with_candidates` used to fail with "serve
socket never appeared" — `bind(2)`: AF_UNIX path too long.)

## Verification

```
cargo test -p acos-services -p acos-parcel -p acos-binder-peer  -> exit 0, 61 suites green
                                                                  acceptance-tests.log
cargo test -p acos-runtime --test parcelable_java               -> 1 passed (the differential)
cargo test -p acos-cli --test fb1_live_input                    -> 4 passed
cargo test -p acos-services --test tail_modules                 -> 8 passed (2 new/strengthened:
                                                                  getRemoteName, short RSSI extra)
tools/build-apps.sh                                             -> every fixture incl. tail-probe.apk
```

Tests added or strengthened: `get_remote_name_answers_the_scripted_device_name`
(new, `tail_modules.rs`), the `EXTRA_RSSI` short assertion in
`a_scripted_scan_broadcasts_action_found_with_the_device_parcel`, and the three
`ParcelableCompareMain` cases above.

## Files

* `crates/acos-services/src/parcelable.rs` — `BluetoothDevice` spec `Str16`,
  `val::SHORT`, `write_dynamic`/`read_dynamic`/`value_to_dynamic`/
  `dynamic_to_value` for shorts
* `crates/acos-services/src/value.rs` — `Value::Short`, `DynamicValue::Short`
* `crates/acos-services/src/ams.rs`, `crates/acos-runtime/src/ams_java.rs` —
  `Extra::Short` through intent extras and the VM re-materialisation
* `crates/acos-services/src/modules/bluetooth.rs` — `getRemoteName` (code 32),
  `request_device`, `scripted_name`, short `EXTRA_RSSI`
* `crates/acos-services/src/host.rs`, `docs/FINDINGS.md` — the two deviations
* `crates/acos-cli/tests/fb1_live_input.rs` — short scratch root
* `crates/acos-runtime/tests/java/ParcelableCompareMain.java`,
  `crates/acos-runtime/tests/parcelable_java.rs` — the differential

## What remains

Nothing blocking. The honest gaps unchanged: no radio/BLE/GATT, no pairing, no
profiles; `BluetoothDevice.getBondState`/`getRemoteType`/`getRemoteAlias` are
still unmodelled (`BluetoothDevice.getName()` was the one the fixture needed).
