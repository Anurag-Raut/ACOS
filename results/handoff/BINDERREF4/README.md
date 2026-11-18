# BINDERREF4 — the strong/detached stub probe, the rooting trace, and the measured JavaBBinder edge

Branch `binderref4` from `binderref3` (`a0024b71`). Worktree env per
BRIEF_COMMON_0316 (private `ACOS_RUNTIME_DIR=/run/user/1000/binderref4`,
private `ACOS_SESSION_SOCKET`, every ART run under
`tools/art-runtime/run-exclusive.sh --task BINDERREF4` + `tools/guarded.sh`).
Target dir `.cache/br4-target`. Fixtures built with
`ACOS_APP_ONLY="tail-probe a11y-probe proc-a proc-b location-probe
notif-perm-probe sensor-probe multi-package-probe" tools/build-apps.sh`.

## TL;DR — read this first

1. **The brief's "no JavaBBinder-style global-ref edge" premise is falsified for
   the ART engine, and the probe that falsifies it is now the regression.** A
   `PhoneStateListener`'s callback **stub** (the Java `Binder` the registration
   parcel carries) survives a forced two-`System.gc()` collection *after the app
   dropped every Java reference to it*, exactly like Android: measured
   `PROBEGC strong=1 strong_stubs=1 detached=3 detached_listeners=0
   detached_stubs=3` and `CALLWEAK listeners=4 alive=0 stubs=4 refs=4`
   (`m-stub.json`, `m-weak.json`). The **listener** object is collected — and
   that is AOSP, not an ACOS bug: `PhoneStateListener.IPhoneStateListenerStub`
   keeps a `WeakReference<PhoneStateListener>`
   (`base/core/java/android/telephony/PhoneStateListener.java:1320-1325`).
   So §2a of BINDERREF/BINDERREF2 is "the listener is weakly held", not "the
   node's Java object was collected".
2. **The edge is implemented in the userspace driver, not in a second ACOS
   mechanism**, and it was already there (BINDERREF's node model): a new node
   pends `BR_INCREFS` + `BR_ACQUIRE` on the exporting thread
   (`node_ref_cmds`), libbinder answers `incWeak`/`incStrong` on the
   `JavaBBinder` (`BC_INCREFS_DONE`/`BC_ACQUIRE_DONE`), and the last
   `F_REF(REF_RELEASE)` queues `BR_RELEASE` + `BR_DECREFS`
   (`node_apply_ref` → `node_unref_cmds`). `JavaBBinder`'s
   `env->NewGlobalRef(object)` is then the strong edge to the Java stub while
   the node has remote strong references, dropped on the last dec — the exact
   rule the brief asks for. Verified by the probe above; no new guest-side
   ownership object was added (that would be a second, redundant edge).
3. **What is still missing is the *release* half on the Rust-VM bridge**, not
   the strong half: `ServiceTransport::binder_id_of` roots every exported
   `Binder` in `Vm::native_roots` and **nothing releases it on the host's last
   reference** (there is no BR_RELEASE in that engine; the host ledger and the
   VM root set are not connected). That is BINDERREF2 option A (per-node owner
   + weak `locals`), i.e. the brief's "STOP and report a sized plan" case — §5
   has the plan. It does not affect the ART acceptance, and it is the *opposite*
   deviation from the one the brief describes (objects leak rather than being
   collected early).
4. **Rooting is observable now** (item 1): `ACOS_BINDER_TRACE=1` writes one
   `acos-binder` line per `BinderTransport::binder_id_of` export and one per
   `native_roots` insert, into the stream `acos logs` reads; unit tests prove
   both records reach the sink (§2). On ART the same edge is already visible in
   the driver trace (`node: local binder=… -> wire=…`, `node refs:
   BR_INCREFS+BR_ACQUIRE`, and `node unref: BR_RELEASE+BR_DECREFS`), see §3.

## 1. The probe (item 1 + item 3)

`fixtures/apps/tail-probe` gained four actions and a counter line; every one of
them is public-API only (no hidden class is referenced at compile time — the
callback stub is reached through `PhoneStateListener.callback` by reflection):

| action | tag | what it does |
|---|---|---|
| `Probe strong stub` | `probe-strong-stub` | registers a `PhoneStateListener` for `LISTEN_CALL_STATE` and keeps it in a **static** list (the normal app pattern) |
| `Probe detached stub` | `probe-detached-stub` | registers the same kind of listener and keeps **neither** the listener nor its stub: two `WeakReference`s (listener, `callback` binder) are the only record |
| `Probe detached receiver` | `probe-detached-receiver` | registers a `BroadcastReceiver` for `SMS_RECEIVED` without keeping it (the framework's `ReceiverDispatcher` does) |
| `Probe GC` | `probe-gc` | `System.gc()`, `System.runFinalization()`, `System.gc()`, then prints `PROBEGC …` |
| `Report callbacks` | (existing) | `CALLTOTAL` now also prints `stubs=`, `probe_strong_calls/ringing`, `probe_detached_calls/listeners/stubs/ringing` |

`Weaken listeners` (BINDERREF2's §2a action) now keeps a `WeakReference` to each
stub too and prints `CALLWEAK listeners=… alive=… stubs=… refs=…`.

The driver is `results/handoff/BINDERREF4/drive.py --mode stub` (and
`--mode biometric`, `--mode weak`). `--mode stub` registers one strong listener,
`--registers` detached listeners and the detached receiver, runs `Probe GC`,
injects the scripted ring **and** an SMS, then asserts:

* the strong arm's callback ran after the GC (`PROBECALL arm=strong
  state=RINGING`);
* every detached **stub** is still alive (`detached_stubs == detached`) — the
  JavaBBinder edge — while every detached **listener** is collected
  (`detached_listeners == 0`) and no detached callback ran (AOSP);
* the detached **receiver** ran after the GC (`PROBESMS …`), i.e. the
  registration, not the app, keeps its node alive.

## 2. The rooting trace (item 1)

Two new pure line builders and two call sites, both behind `ACOS_BINDER_TRACE`
(no flag → no cost, no line):

| where | line | why |
|---|---|---|
| `acos_runtime::transport::binder_id_of` (every export of a Java `Binder`) | `export bind id=<wire> handle=<java handle> rooted=strong` | the ACOS counterpart of `Parcel::writeStrongBinder` → `JavaBBinder`; names both identities of the root |
| `acos_vm::javabridge.rs::bridge_root` (every new `native_roots` entry) | `native_roots insert handle=<h> count=<n>` | the root set insert itself, with the size after it, so "rooted late" is distinguishable from "never rooted" |

Both go through `acos_vm::natives::os_log::push_record(3, "acos-binder", …)`, the
sink `acos logs` / `acos events` read, so they are indistinguishable from a guest
`Log.d` line — which is the point (it is the platform logging, not the app).
Tests (fail before the change):

* `acos_runtime::transport::tests::the_export_trace_reaches_the_vm_log_sink`
  and `…::the_export_trace_names_the_rooted_object`
* `acos_vm::javabridge::tests::the_native_root_trace_reaches_the_vm_log_sink`
  and `…::the_rooting_trace_names_the_native_root`

On the ART engine the same rooting is already in the driver trace (tag
`acos-binder`, `E3G_DEBUG=1`): `node: local binder=… cookie=… -> wire=…`,
`node refs: BR_INCREFS+BR_ACQUIRE`, `node INCREFS_DONE`/`ACQUIRE_DONE` (libbinder's
acknowledgement, i.e. the `incWeak`/`incStrong` on the `JavaBBinder`) and
`node unref: BR_RELEASE+BR_DECREFS`. The captures in this track's JSON are a
`acos_logs` **tail** and the export lines are earlier than the window; the
delivery/unref side is captured (`binder_log_lines`), and
`results/handoff/BINDERREF/README.md` §2 carries the export+ack sequence of the
same driver code. `root_trace_captured` in `m-stub.json` records this honestly
(`false` for this short run) instead of pretending the window held it.

## 3. The measurement (item 2) — the edge exists, and here is the proof

`m-stub.json` (tail-probe, `net.telephony` scripted, `--mode stub
--registers 3`):

```
PROBEGC strong=1 strong_stubs=1 detached=3 detached_listeners=0 detached_stubs=3
SMS from=+15550003333 body=binderref4 probe
PROBESMS detached receiver ran after GC
CALL state=RINGING number=+15550002222 listener=1
PROBECALL arm=strong state=RINGING
CALLTOTAL state_lines=2 ringing=1 listeners=1 held=1 alive=1 stubs=0
          probe_strong=1 probe_strong_calls=2 probe_strong_ringing=1
          probe_detached=3 probe_detached_calls=3 probe_detached_listeners=0
          probe_detached_stubs=3 probe_detached_ringing=0
```

`m-weak.json` (`--mode weak --registers 3`, the BINDERREF2 §2a arm, unchanged
behaviour):

```
CALLWEAK listeners=4 alive=0 stubs=4 refs=4
CALLTOTAL … ringing=0 alive=0 stubs=4
```

Read it as three separate facts:

1. **Stub alive (`detached_stubs=3`, `stubs=4`)** — the Java `Binder` objects
   behind registrations whose app references are gone survived two forced
   collections. That is the `JavaBBinder` global-reference edge, kept by the
   node's remote strong references (the host's `GuestRefs` ledger → `F_REF`
   acquire → `BR_ACQUIRE` → `incStrong(JavaBBinder)` → `NewGlobalRef(stub)`).
   A missing edge would show `detached_stubs < 3`.
2. **Listener collected (`alive=0`, `detached_listeners=0`)** — AOSP, for the
   reason quoted in §TL;DR 1. `alive` vs `stubs` is the distinction BINDERREF2
   could not make: it counted only the listener, so its `alive=0` read as "the
   Java object was collected", when the object that actually has to survive (the
   binder) did.
3. **No detached callback ran (`probe_detached_ringing=0`)** — correct for a
   listener whose Java object is gone; the stub's `onTransact` reaches the
   `WeakReference` and finds null. The strong arm proves the delivery path is
   live in the same session (`PROBECALL arm=strong state=RINGING`).

The release half is measured by the same driver in BINDERREF/BINDERREF2
(`m-unregister.json`: `node release wire=9` then `node unref:
BR_RELEASE+BR_DECREFS`), and the driver's `node_apply_ref` is the code that maps
the host's last reference onto those commands.

## 4. What was changed

| file | change |
|---|---|
| `crates/acos-runtime/src/transport.rs` | `binder_export_line` + `trace_export` (one `acos-binder` line per export, behind `ACOS_BINDER_TRACE`), two unit tests |
| `crates/acos-vm/src/javabridge.rs` | `native_root_insert_line` + the insert trace in `bridge_root`, two unit tests |
| `crates/acos-services/tests/binder_refs.rs` | `a_delivery_keeps_the_registration_that_roots_a_detached_listener` (host half of the probe: a delivery does not unroot a registration, `LISTEN_NONE` releases only that one) |
| `fixtures/apps/tail-probe/…/MainActivity.java` | the four probe actions, `stubs=` on `CALLWEAK`, the probe counters on `CALLTOTAL` |
| `results/handoff/BINDERREF4/drive.py` | `--mode stub`, `--mode biometric`, the stub counters on `--mode weak` |

No change was made to `GuestRefs`, to the driver's node model, or to the
`BinderId ↔ handle` map: the measurement says the edge is already correct, and a
second mechanism would be a second answer to "who keeps the Java object alive".

## 5. STOP-and-report: the Rust-VM rooting has no release (option A, sized)

**The gap.** `acos-runtime`'s `ServiceTransport` is the *Rust VM* engine's binder
bridge (not the ART engine's — ART uses libbinder + the userspace driver). It
does `vm.root_handle(obj)` on every export and `native_roots` is never popped.
On Android the analogous reference is dropped on `BR_RELEASE`/the last dec, so a
callback the app drops becomes collectable when its registration ends. In the
Rust VM it stays rooted for the session (bounded by `MAX_NATIVE_ROOTS = 1 MiB`
handles, and each distinct binder is one entry): a leak, not an early
collection. `locals: RefCell<HashMap<u64, Handle>>` is a strong map with an
8192-entry bounded eviction, so it does not hold objects alive by itself, but it
also has no notion of a node's reference count.

**Why it is not a one-liner.** There is no BR_RELEASE in this engine: the guest
is the same process as the transport, and the host's `ServiceHost` ledger is the
only place that knows "the last host reference to this binder went away"
(`release_binder` → `take_released_binders`). Connecting it to
`Vm::release_bridge_root` means a host→VM callback seam on the dispatch path
(the VM is `&mut` only while the host is borrowed) — i.e. an ownership decision,
not a bug fix.

**Options, sized:**

* **A1 — host-driven release (smallest correct).** Add a `BinderTransport`
  hook `fn released(&self, vm: &mut Vm, id: BinderId)` (default no-op), call it
  from `Runtime`'s dispatch loop for every `take_released_binders()` entry, and
  implement it in `ServiceTransport` as `vm.release_bridge_root(locals[id])`
  plus `locals.remove(id)` — one file (`acos-runtime/src/{transport.rs,lib.rs}`),
  ~30 lines + 2 tests (export→release→collected; release of an unknown id is a
  no-op). Owner stays the existing ledger; no new object.
* **A2 — per-node owner (BINDERREF2 option A, largest).** A
  `struct NodeOwner { handle, id }` in the transport, `locals` becomes
  `HashMap<u64, Weak<Handle>>`-like (a weak map keyed by id), exported objects
  are rooted only through the owner, and the owner is dropped on the release
  seam. Buys exact Android semantics (re-export after collection gets a fresh
  object) at the cost of touching every `locals` reader
  (`binder_object`, `descriptor`, `is_alive`, `seed_binder`), ~150-250 lines and
  a re-entrancy story for the `LOCAL_OBJECTS` mirror.
* **A3 — keep the leak, document it.** Zero code; the Rust VM engine is the
  frozen one (`engine_frozen` in its sessions) and no acceptance row needs the
  release. The `ACOS_BINDER_TRACE` lines from item 1 make the leak observable
  (`native_roots insert` with a growing `count`, no matching release line).

Recommendation: A1 if the Rust-VM engine is still a supported product surface;
otherwise A3 with the trace in place. A2 only if the engine must reproduce
Android's collection timing, which no scenario currently asserts.

## 6. Proof

```
CARGO_TARGET_DIR=$PWD/.cache/br4-target cargo test --offline \
  -p acos-binder-peer -p acos-services -p acos-art      # green (see the log line below)
CARGO_TARGET_DIR=$PWD/.cache/br4-target cargo test --offline -p acos-vm --lib javabridge
CARGO_TARGET_DIR=$PWD/.cache/br4-target cargo test --offline -p acos-runtime --lib transport
```

ART (`ACOS_RUNTIME_DIR=/run/user/1000/binderref4`, `E3G_DEBUG=1`, under
`run-exclusive.sh --task BINDERREF4`):

| run | command | verdict | file |
|---|---|---|---|
| strong/detached/receiver probe | `drive.py --mode stub --registers 3 --idle-s 5` | **pass** | `m-stub.json` |
| weak/AOSP arm (BINDERREF2 regression) | `drive.py --mode weak --registers 3` | **pass** | `m-weak.json` |
| biometric pending receiver | `drive.py --mode biometric` | **pass** (`AUTH:SUCCESS`) | `m-biometric.json` |
| a11y-probe | `acos verify scenarios/a11y-probe.yaml --engine art` | **pass** | `v-a11y-probe.json` |
| location-pending-intent | `… location-pending-intent.yaml` | **pass** | `v-location-pending-intent.json` |
| notif-perm-granted | `… notif-perm-granted.yaml` | **pass** | `v-notif-perm-granted.json` |
| mproc3-two-process | `… mproc3-two-process.yaml` | **pass** | `v-mproc3-two-process.json` |

`m-biometric.json` is the ART counterpart of BINDERREF3's host-level test: the
pending `authenticate` receiver is answered by the scripted `auth` control op
*after* the app stopped holding its callback, and the app's
`onAuthenticationSucceeded` ran (`AUTH:SUCCESS`).

## 7. Files

* `fixtures/apps/tail-probe/src/dev/acos/fixtures/tailprobe/MainActivity.java` —
  the probe actions, `callbackOf` (reflection on the public `callback` field),
  `collectTwice`, the counters.
* `results/handoff/BINDERREF4/drive.py` — `--mode stub|biometric|weak`,
  assertions above.
* `crates/acos-runtime/src/transport.rs`, `crates/acos-vm/src/javabridge.rs` —
  the trace lines and their tests.
* `crates/acos-services/tests/binder_refs.rs` — the host-half regression.
