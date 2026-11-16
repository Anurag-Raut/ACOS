# BINDERREF2 — the binder ref model integrated, one ledger for every module, and §2a measured

Branch `t3/binderref2` from `modules` (`78e9c955`, then merged `modules` =
`7c4317ab` PIFILLIN). Worktree env per BRIEF_COMMON_0316: private
`ACOS_RUNTIME_DIR=/run/user/1000/binderref2`, private `ACOS_SESSION_SOCKET`,
every ART run under `tools/art-runtime/run-exclusive.sh --task BINDERREF2` +
`tools/guarded.sh`. Target dir `.cache/br2-target` (deleted at the end).

Read `results/handoff/BINDERREF/README.md` first (the node ref model, the wire
v2 handshake, the one `txn_heap`); this report is the integration of it.

## TL;DR

1. **`git merge binderref` is in, and it was cheap**: two conflicts, both
   additive, both sides kept. All service code is `modules`'; the driver, peer,
   wire protocol and `txn_heap` are `binderref`'s. §1 has the exact hunks.
2. **The reference model is one ledger now** (`acos-services/src/refs.rs`) and
   one trait seam (`ServiceObject::drain_retained`/`drain_released`/
   `release_all_refs`) drained after **both** dispatch paths. Every module that
   stores a callback binder is wired: telephony, location (listener **and**
   PendingIntent form), broadcast `IIntentReceiver` (including the PIFILLIN
   mirroring), `IAccessibilityManagerClient` + interaction connections + the
   bound service's client, connectivity network callbacks, bluetooth adapter
   callbacks. Releases happen on unregister, on module detach (`end_scope` and
   `release_object_refs`), and on peer disconnect (`Peer::release_all_refs`).
3. **Two host-side corrections were needed to make the model real** (both with
   tests and both a real bug before): `retain_binder` refused every guest binder
   (the `BinderRegistry` never has the handle, so the reference silently did not
   exist and only the release ever reached the driver), and `release_binder`
   reported a release for a reference it never held.
4. **§2a is reproduced and narrowed, not fixed.** 11 registered listeners, the
   app drops them and forces a GC: `alive=0`, `0` callbacks, while the driver
   dispatched every delivery with `flags=0x0`. The callback body never runs (a
   `try/catch` around it prints nothing), so the object behind the node is gone
   — the report's claim stands. What I found in the guest: the parcel's
   strong-binder mapping had its own identity sink and a `set_binder_sink` that
   nothing ever calls ("Called by T-19 once the transport exists"), i.e. a second
   mapping that never roots; I unified it onto the transport mapping and the
   measurement did **not** change, because in a live session
   `natives::binder::install` already overrides the parcel write with the
   transport-rooting version. The remaining cause is upstream of that, and it is
   an architectural question, not a bug I may decide: ACOS has no `JavaBBinder`
   — the only thing that keeps a guest Java callback alive is `Vm::native_roots`,
   a root set nothing ever releases, so the "node → Java object" edge does not
   exist at all in this binder bridge. §4 has the measurement, the two code
   findings, and the decision the orchestrator has to take.
5. **Acceptance:** `cargo check --workspace --all-targets` clean; `cargo test -p
   acos-binder-peer -p acos-services` green (69 test binaries, 0 failures); ART
   proofs 26/26, 46/46, 101/101, unregister with the release in the trace, and
   four module scenarios pass. §3 is the table.

## 1. The merge, and the two conflicts

`git merge binderref` (`595a28e1`) into `modules`: **2 conflicted files**, both
purely additive, so nothing had to be chosen between two implementations:

| file | conflict | resolution |
|---|---|---|
| `crates/acos-services/src/host.rs` | `ServiceObject`'s method list: binderref added `drain_raw_retained`/`drain_raw_released` where modules added `next_delivery_deadline`/`advance_timed`; `ServiceHost::new` initialisers (`dead` vs `refs`/`released_binders`) | **both** trait hooks kept, **both** fields kept |
| `crates/acos-binder-peer/tests/peer.rs` | both branches appended a test block at the end of the file (modules: ADR-0029 idle-wait tests; binderref: the `F_REF` tests) | both blocks kept, the file's final `}` restored |

Everything else auto-merged and the result builds: the driver
(`native/binder_driver.c`, `native/driver_test.c`, `native/txn_heap.c`) and the
peer/wire (`protocol.rs`, `peer.rs`) are binderref's; every
`crates/acos-services/src/**` service body is modules'. One heap, `txn_heap`,
called from `binder_driver.c` — no `mm_alloc` left anywhere
(`grep mm_alloc crates/acos-binder-peer/native` is empty).

The later `git merge modules` (`7c4317ab`, PIFILLIN) merged clean; the only
follow-up it needed is in §2 (`Sink::PendingIntent`).

## 2. One ledger, every module

### The ledger (`crates/acos-services/src/refs.rs`)

`GuestRefs` is a **set**, not a counter: a registration *is* one reference, so
re-registering the same binder (`listen()` twice with one listener, a receiver
registered again after a config change) cannot double-count and cannot
under-release. It keeps the currently-held set plus two pending queues, and
`release_all()` is what a detach needs (drop everything, report it).

`ServiceObject` gained three default methods; the host drains them after **both**
`call()` and `raw_transact()`, so a module uses the same mechanism whether its
interface is a decoded `acos-aidl` table or a hand-written wire form:

```rust
fn drain_retained(&mut self) -> Vec<BinderId>;   // "I stored this binder"
fn drain_released(&mut self) -> Vec<BinderId>;   // "I dropped it"
fn release_all_refs(&mut self) -> Vec<BinderId>; // module detach / teardown
```

The host applies `retained` before `released` in a turn, and the peer now writes
**deliveries before reference frames** (a release must never overtake a delivery
to the same node — `a_delivery_is_written_before_the_reference_release_of_the_same_turn`).

### What each module holds

| module / service | registration | release |
|---|---|---|
| `net.telephony` `ITelephonyRegistry` | `listenWithEventList` with a non-empty event list | `LISTEN_NONE`, `removeOnSubscriptionsChangedListener`, module detach |
| `location` `ILocationManager` (listener form) | `registerLocationListener` | `unregisterLocationListener`, module detach (`release_object_refs`: the node is `core`'s) |
| `location` (one-shot forms) | `getCurrentLocation`, `getFromLocation(Name)` | the moment the delivery is drained (a `one_shot` list in `LocationState`) |
| `location` (PendingIntent form) | `registerLocationPendingIntent` — `Sink::PendingIntent` (PIFILLIN) | `unregisterLocationPendingIntent`, module detach |
| `IActivityManager` receivers | `registerReceiver`/`registerReceiverWithFeature` **and** the PIFILLIN runtime mirror (`AtmsService::register_receiver`) | `unregisterReceiver`, session teardown |
| `accessibility` `IAccessibilityManager` | `addClient`, `addAccessibilityInteractionConnection`, `publishService` (the bound service's `IAccessibilityServiceClient`) | `removeClient`, `removeAccessibilityInteractionConnection`, unbind/detach, session teardown |
| `connectivity` `IConnectivityManager` | `registerNetworkCallback`/`listenForNetwork` (the `IMessenger`) | `releaseNetworkRequest` |
| `hw.bluetooth` `IBluetoothManager` | `registerAdapter`, `registerStateChangeCallback` | module detach (the node is the module's own, so `end_scope` reports it) |
| `hw.biometric`, `system.autofill`, `inputmethod` | store a callback per request/session | **not wired** — see §5 item 3 |

`sensors` is the one named module with **no guest callback binder**: the app's
events arrive on the `BitTube` fd (`SSensorEventConnection.getSensorChannel`),
the connection itself is a *host* node the guest references, and the app's
`destroy()` is the unregister. There is nothing for the host to root on the
guest side; `grep -n read_strong_binder crates/acos-services/src/sensors.rs` is
empty by construction. Proof for the module is its scenario (§3), not a ref
trace.

### Release on detach and on death

* `end_scope` releases every reference of the objects it drops (added entries,
  overwritten entries, dynamic nodes) **before** dropping them, because `Drop`
  has no host to report to.
* A module that fills a node it does not own (`location` fills `core`'s
  `ILocationManager`) reports through `ServiceHost::release_object_refs`.
* `Peer::release_all_refs()` is the session-teardown hook; `acos-binder-peer`'s
  serve loop calls it on guest EOF and logs how many were released. (The MPROC
  death path is not merged in this tree; when it is, this is the call it makes.)

### The two host-side corrections

Both were latent defects that made the model a no-op:

```rust
// before: every guest callback was refused a reference, because the host's
// BinderRegistry never registered the handle (the peer tags guest binders
// `JAVA_TAG|wire` and does not add them to the registry).
if !self.registry.contains(id) { return 0 }

// after: a guest binder is a node the *driver* owns; rooting it is the point.
if id.0 == 0 || (!self.registry.contains(id) && id.0 & GUEST_BINDER_TAG == 0) { return 0 }
```

and `release_binder` no longer reports a release for a reference the host never
held (that is how an unregister could tell the driver to unroot a node that was
still the app's).

## 3. Proofs

### Tests (the acceptance gate)

```
cargo check --workspace --all-targets      # clean
cargo test -p acos-binder-peer -p acos-services
  # 69 test binaries, every `test result: ok`, 0 failures
```

New in this track:

| test | what it fails on |
|---|---|
| `refs.rs` unit tests (4) | the ledger being a counter instead of a set, a release for a registration never made, double-reporting after `release_all` |
| `tests/binder_refs.rs` (9) | one case per module (telephony, location listener, location PendingIntent, `IIntentReceiver`, a11y client + interaction connection, network callback) plus module detach ×2 and `release_all_refs` |
| `natives/parcel.rs::a_written_callback_is_mapped_and_rooted_by_the_transport` | the parcel mapping not going through the transport (the §2a seam, §4) |
| `peer.rs::a_delivery_is_written_before_the_reference_release_of_the_same_turn` | a release frame emitted before the delivery of the same turn |

### ART (real session, `fixtures/build/apps/tail-probe.apk`, scripted telephony)

| run | executed | dispatched (app's own counter) | verdict | file |
|---|---|---|---|---|
| 26 registered listeners | 26 | **26** | pass | `m-repeat25.json` |
| 46 registered listeners | 46 | **46** | pass | `m-repeat45.json` |
| 101 registered listeners | 101 | **101** (`state_lines=202`) | pass | `m-repeat100.json` |
| long-lived listener, 30 s idle, then inject | 1 | **1** | pass | `m-longlived.json` |
| unregister (`LISTEN_NONE`), idle, inject | 0 | **0**, and the trace shows `node release wire=9` + `node unref: BR_RELEASE+BR_DECREFS` | pass | `m-unregister.json` |

Module scenarios on ART (`acos verify scenarios/<name>.yaml --engine art`), all
`status: pass` / `verdict: pass`: `sensors-scripted`, `a11y-probe`,
`location-pending-intent`, `notif-perm-granted` (`v-*.json`).

The unregister trace is the item-2 proof end to end:

```
tid=14 node release wire=9 weakref=… cookie=…      # the peer's F_REF release
tid=13 node unref: BR_RELEASE+BR_DECREFS weakref=…  # the driver unroots the guest node
CALLTOTAL state_lines=1 ringing=0 listeners=1 held=1
```

**Detach at ART level is blocked, and the refusal is recorded rather than
worked around:** `net.telephony` is an `at-launch` module, so a live session
refuses `acos_services {"op":"detach"}` (`SERVICE_MODULE_REJECTED`), and the
callbacks of the module that *can* be detached live in `core`'s nodes
(`accessibility`'s clients belong to the a11y manager, not to the module). The
detach path is therefore proven twice at the host level (`telephony` via
`end_scope`, `location` via `release_object_refs`), and the ART-level run needs a
hot module whose *own* node holds a callback that a fixture registers — the
follow-up in §5.

## 4. §2a — "a rooted node dispatches but the Java object behind it is collected"

### Reproduction (kept in the fixture, so it stays measurable)

`fixtures/apps/tail-probe` gained a `Weaken listeners` action: every registered
`PhoneStateListener` moves into a `WeakReference` list, the strong references are
dropped and `System.gc()` is forced; `Report callbacks` now prints
`alive=<listeners a GC left reachable>`. `results/handoff/BINDERREF2/drive.py
--mode weak` runs it and reports both numbers.

Measured (11 registered listeners, `m-weak.json`):

```
CALLWEAK listeners=11 alive=0 refs=11
CALLTOTAL state_lines=11 ringing=0 listeners=11 held=0 alive=0
driver: 31 node exports, 655 deliveries, 655 `REPLY … flags=0x0`   # nothing was dropped by the driver
```

and with a `try/catch` around the callback body (`m-weak-err.json`): **no
`CALL-ERR` line at all**, i.e. the callback method never ran — the difference
between "the framework dispatched to a listener that is gone" and "libbinder
never reached the stub". So the object behind the node is collected, exactly as
`results/handoff/BINDERREF/README.md` §2a reported.

### What I found in the guest, and what it does not explain

* `crates/acos-vm/src/natives/parcel.rs` had **its own** `IBinder → BinderId`
  mapping (`DefaultBinderObjectSink`, an identity map) installed by
  `set_binder_sink`, whose only caller is nobody — the function is
  `#[allow(dead_code)]` with the comment *"Called by T-19 once the transport
  exists"*. A mapping that never touches the transport cannot root anything.
  **Fixed**: `Parcel.nativeWriteStrongBinder`/`nativeReadStrongBinder` now use the
  transport mapping (`strong_binder_id`/`strong_binder_object`), with a unit test
  that asserts the transport is consulted and roots the handle.
* That fix does **not** change the measurement (`m-weak-after.json` is identical:
  `alive=0`, `ringing=0`), because in a live session
  `natives::binder::install` already re-registers `nativeWriteStrongBinder` with
  the transport-rooting version (*"Must be called after `parcel::install` so the
  strong-binder overrides win"*). So the dead sink was real but is not the cause.
* The rooting that *would* keep the object alive is
  `Vm::root_handle` inside the transport (the ACOS counterpart of
  `JavaBBinder`'s `env->NewGlobalRef(object)`), and `native_roots` **is** a GC
  root (`Vm::gc` marks it). Whatever path the app's write takes, it does not end
  there — and my attempt to instrument it (`push_record` from the transport, tag
  `acos-binder`) produced no line in `acos_logs`, so the guest-side mapping runs
  in a process/log path that `acos_logs` does not read (the app runs in the
  sandbox child; the driver's own `WARN` mirror uses the liblog shim, which is
  why its lines appear). **The instrumentation channel is the next step, not a
  guess:** mirror to logd through the shim (`__android_log_print`) from
  `BinderTransport::binder_id_of`, one line per export, and compare it with the
  driver's `node: local binder=…` lines for the same session.

### Why I did not "just fix it" (architectural)

On Android the edge is: driver node → `JavaBBinder` (kept alive by `BR_ACQUIRE`)
→ `mObject` (a JNI **global** reference) → the Java object, and `BR_RELEASE`
drops the last one, so the object becomes collectable exactly when the node is
unrooted. ACOS's guest has no such object: the guest binder bridge is
`BinderId ↔ Java handle` maps (`ServiceTransport::locals`, `LOCAL_OBJECTS`) plus
`Vm::native_roots`, which is a *leak-forever* root — nothing releases it when the
driver unroots a node. So the model has no place to hang "the Java object lives
while the node is rooted", and adding one means deciding the binder bridge's
ownership model (per-node owner object in the transport, released on the
driver's `BR_RELEASE`/`BR_DECREFS`, plus the `BinderId ↔ handle` map becoming a
weak one for callbacks). That is the binder-bridge design, which BRIEF_COMMON §
"Architectural walls" says to stop and report — so this is the report:

* **Option A (correct, largest):** give the transport a per-node owner
  (`Arc<CallbackOwner>` holding the Java handle, rooted on export, released on
  `BR_RELEASE`/`BR_DECREFS` or on peer death) and make `locals` a weak map. Then
  the Android rule holds exactly: a registered callback stays alive while the
  node is rooted, and is collectable after the unregister.
* **Option B (accept):** document that a callback whose Java object the app
  drops is not reachable (Android apps must hold their listeners). Nothing in the
  acceptance table needs more, and `m-weak.json` is the honest evidence.
* **Option C (cheap, wrong-ish):** root every binder forever (`native_roots`
  already does) and never release — that is what the tree does today for the
  paths that reach `root_handle`; it leaks one Java object per callback for the
  session's life and *still* misses the objects written through the other path.

Measured inputs for that decision are in `m-weak.json`, `m-weak-after.json`,
`m-weak-err.json`, `dbg-weak*.json`.

## 5. Open items

1. **§2a**: needs the decision in §4 and (either way) the liblog instrumentation
   to identify the write path the app's callback binder takes.
2. **ART-level module-detach proof** for a hot module with a callback in its own
   node (bluetooth adapter callback, or a `location` fixture that registers a
   listener): the harness exists (`drive.py --mode detach`), the module does not.
3. **Three modules still hold callbacks without a reference**: `hw.biometric`
   (the `IBiometricServiceReceiver` per authenticate), `system.autofill`
   (`IAutoFillManagerClient`), `inputmethod` (`IInputMethodClient` /
   `IInputContext`). They are one `GuestRefs` field plus one hold/release each —
   the mechanism is in place; they were left because their unregister paths
   (finish/cancel, session end, `unbindInput`) are one-shot and need the same
   one-shot release the location `one_shot` list implements.
4. `ParcelStore::binder_id_of/binder_object_of` (used by `fwk_input` for input
   *tokens*) still go through the identity sink; tokens are host-side ids, not
   guest callbacks, so the ref model does not apply — noted so nobody "fixes" it
   into rooting host nodes.
5. `native_roots` has no release path at all (`MAX_NATIVE_ROOTS = 1 MiB`
   handles). Whatever §2a decides, the leak is there today.

## 6. Commands

```
export ACOS_FONT_DIR=/home/Qui/Projects/acos/.cache/boot/android-34/fonts
CARGO_TARGET_DIR=$PWD/.cache/br2-target cargo check --workspace --all-targets
CARGO_TARGET_DIR=$PWD/.cache/br2-target cargo test --offline -p acos-binder-peer -p acos-services
CARGO_TARGET_DIR=$PWD/.cache/br2-target cargo build --offline --release -p acos-cli -p acos-art -p acos-mcp
ACOS_APP_ONLY=tail-probe tools/build-apps.sh

ACOS_BIN=$PWD/.cache/br2-target/release/acos \
ACOS_ART_BIN=$PWD/.cache/br2-target/release/acos-art \
ACOS_ALLOWED_ROOTS=$PWD \
tools/art-runtime/run-exclusive.sh --task BINDERREF2 -- tools/guarded.sh --mem 14G --time 900 -- \
  env -u ACOS_HOME ACOS_RUNTIME_DIR=/run/user/1000/binderref2 \
  ACOS_SESSION_SOCKET=/run/user/1000/binderref2/session.sock E3G_DEBUG=1 \
  python3 results/handoff/BINDERREF2/drive.py \
  --binary $PWD/.cache/br2-target/release/acos-mcp \
  --apk fixtures/build/apps/tail-probe.apk --mode repeat --registers 25 --idle-s 2 \
  --min-priority I --artifacts $PWD/.cache/br2-run/r25 \
  --out results/handoff/BINDERREF2/m-repeat25.json
```

`--mode` is `repeat | unregister | longlived | weak | detach`; the last two are
this track's (the §2a experiment and the detach probe).

## 7. Files

* `crates/acos-services/src/refs.rs` — `GuestRefs`, the one registration ledger.
* `crates/acos-services/src/host.rs` — `drain_retained`/`drain_released`/
  `release_all_refs` on `ServiceObject`, drained on both dispatch paths;
  `release_object_refs`, `release_all_refs`, `GUEST_BINDER_TAG`,
  `retain_binder`/`release_binder` corrections; `end_scope` releases.
* `crates/acos-services/src/modules/telephony.rs`, `location.rs`,
  `bluetooth.rs`; `atms.rs`; `misc.rs` + `a11y_bind.rs`; `connectivity.rs`.
* `crates/acos-binder-peer/src/peer.rs` (delivery-before-release ordering,
  `release_all_refs`), `src/main.rs` (release on guest EOF).
* `crates/acos-vm/src/natives/parcel.rs` (`strong_binder_id`,
  `strong_binder_object`, the transport-mapping test),
  `crates/acos-vm/src/natives/binder.rs` (`pub(crate)` mapping).
* `fixtures/apps/tail-probe/.../MainActivity.java` — `Weaken listeners`,
  `alive=` in `CALLTOTAL`, `CALL-ERR` in the callback.
* `results/handoff/BINDERREF2/drive.py` — `--mode weak|detach`.
* Evidence: `m-repeat25/45/100.json`, `m-unregister.json`, `m-weak*.json`,
  `dbg-weak*.json`, `m-detach.json`, `v-*.json`.
