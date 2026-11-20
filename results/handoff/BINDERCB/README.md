# BINDERCB — long-lived host→guest callbacks (status: PARTIAL, root cause narrowed)

Branch `bindercb` from `modtail` (`4adef5f0`). Worktree env per BRIEF_COMMON_0316
(private `ACOS_RUNTIME_DIR=/run/user/1000/bindercb`, private
`ACOS_SESSION_SOCKET`), every ART run under `tools/art-runtime/run-exclusive.sh
--task BINDERCB` + `tools/guarded.sh`.

## What was reproduced, and how

The MODTAIL report (`results/handoff/MODTAIL/README.md`, "A long-lived
`PhoneStateListener` is not delivered to") is right that a registered callback
can silently stop being reached. It does **not** reproduce by simply letting the
session sit: a listener registered in `onCreate` and a scripted call-state event
**65 s** later is delivered, before and after this change
(`v-longlived-60s.json`, `ringing_before: 0`, `ringing_after: 1`).

It reproduces with the same defect *deterministically* when several callbacks are
registered for the same event (`--mode repeat`): each `PhoneStateListener`
registration is a new Java `Binder` and therefore a new driver node, and the host
queues one delivery per registered listener. Measured, current code:

| run | host queued | guest dispatched | evidence |
|---|---|---|---|
| 26 registered listeners, one injected ring | 26 (`ctl … "delivered": 26`) | **1** (the newest) | `v-repeat25.json` |
| 45 registered listeners | 44 | **0** | `v-repeat.json` |

The host's delivery targets were the 26 distinct node wires (`deliveries=26
targets=[(3, 6, 9), (3, 6, 22) … (3, 6, 46)]`, `ACOS_SERVICE_CTL_LOG`), so the
frames were written; the 25 older listeners never ran their `onTransact` and the
guest sent **no** reply for them (no `REVERSE_DELIVERY_*` record, `reverse_failed`
unchanged), i.e. the callback was lost without the host ever being told.

## What this change fixes (driver + host, tested)

1. **A transaction to a node that is gone is never dropped silently.**
   `deliver_incoming`/`node_by_wire`: the old code logged `E3G[warn]: incoming
   transaction for unknown node` (guest stderr only) and freed the frame. It now
   answers the host with `F_REP` + `TF_STATUS_CODE` + `-32` (`DEAD_OBJECT`) — what
   the kernel's `BR_DEAD_REPLY` gives a transaction to a dead node — and the peer
   records `REVERSE_DELIVERY_DEAD_OBJECT` (`seq`, `status`) in the session's
   unsupported/evidence JSONL next to `REVERSE_DELIVERY_EXCEPTION`, so a verdict
   can see that a callback never ran.
2. **Warnings reach logd.** Guest stderr is not kept for a live session (the
   daemon discards it, `session-exit.json` keeps a 16-line tail), so the driver
   now mirrors every `WARN` into logd (`tag=acos-binder`), resolved lazily so a
   host build works without liblog. This is what made the measurement above
   possible at all.
3. **The driver core is testable without ART.** `native/driver_test.c` (compiled
   for the host with `-DACOS_DRIVER_HOST_TEST`) drives the driver through the same
   entries libbinder uses (`open("/dev/binder")`, `ioctl(BINDER_WRITE_READ)` with
   `BC_*`, reading `BR_*`) against a scripted peer; `tests/driver.rs` builds and
   runs it from `cargo test -p acos-binder-peer`. Eight behaviour checks:
   node export → wire handle, `BR_INCREFS` + `BR_ACQUIRE` rooting, single
   delivery, delivery **after `BC_FREE_BUFFER` + `ACQUIRE/RELEASE/INCREFS/DECREFS`
   churn** (node lifetime does *not* follow the buffer — already true), eight
   back-to-back callbacks all delivered, and `DEAD_OBJECT` for an unknown node.

Before/after for the one behaviour the fix changes (same test binary, driver
source with only the `deliver_incoming` hunk reverted):

```
not ok 8 - a transaction to an unknown node answers a DEAD_OBJECT reply: dead_object replies=0
```

after:

```
ok 8 - a transaction to an unknown node answers a DEAD_OBJECT reply
RESULT failures=0
```

## What is NOT fixed (the rest of the report)

The 25-of-26 loss is a *node liveness* defect, not a buffer-lifetime one, and it
is not fixed here:

* The driver finds the node (no `unknown node` warning, no `DEAD_OBJECT` reply
  from the driver in that run), so it emits `BR_TRANSACTION` with the node's
  cached `target.ptr` (weakref) / `cookie`. libbinder then calls
  `((RefBase::weakref_type*)tr.target.ptr)->attemptIncStrong(this)` and dispatches
  only if that succeeds (`IPCThreadState.cpp:1400`), which is why an older
  listener's callback can vanish while the newest one works: the `JavaBBinder`
  behind a Java `Binder` was replaced (`JavaBBinderHolder::get` recreates it when
  its `wp` no longer promotes, `android_util_Binder.cpp:463`), so the node's
  cached weakref is stale. `node_wire_for()` already detects the replacement
  (`node wire=%u … weakref changed %llx -> %llx`) but keeps the stale pointers.
* Fixing that needs a real node/ref model across all three layers, which is the
  architectural work this track did not get to (see the report): a driver-side
  per-node strong/weak refcount fed by the remote side (a `F_REL`/refcount message
  the host sends when it drops a reference), re-rooting a node when its object is
  re-created (`node_wire_for` must refresh `binder`/`cookie` and re-send
  `BR_INCREFS`/`BR_ACQUIRE`), and the host holding a strong reference for every
  registered listener (`acos-services` `TelephonyRegistryService.listeners`,
  sensors/event connection, location, `IIntentReceiver`,
  `IAccessibilityManagerClient`) released on unregister/death.

Also observed and unfixed (same session, one turn): after ~45 registrations the
app's `listenWithEventList` starts failing with `parcel underflow: need 144
byte(s) at 224, have 4` on the host side — the guest→host frame is short — and
the app's main thread reports `ACOSPROBE-STALL … BinderProxy.transactNative
(listenWithEventList)`. Both point at the same per-thread reply/buffer accounting
in the driver (a wedged `T.reply_seq`/`pend` path); it is the second item for the
next agent, with the evidence below.

## ART evidence (real ART, `acos` MCP session engine, `fixtures/build/apps/tail-probe.apk`)

`results/handoff/BINDERCB/drive_mcp.py` drives a live session (the scenario
format has no sleep step, so the 65 s idle and the per-listener assertions live
here): launch with `services: [{id: net.telephony, profile: scripted}]`, then
idle, then `acos_service_ctl call_state`, then read the app's own printed lines
out of the live UI tree.

| run | command | result | evidence |
|---|---|---|---|
| long-lived listener, 65 s idle, no re-register | `--mode longlived --idle-s 65` | **pass** (`CALL state=RINGING number=+15550002222`) | `v-longlived-60s.json` |
| unregister (`listen(…, LISTEN_NONE)`), then 65 s, then inject | `--mode unregister --idle-s 65` | **pass** (no callback after unregister) | `v-unregister.json` |
| 26 registered listeners, one injected ring | `--mode repeat --registers 25` | **fail**: 1 of 26 called (the defect above) | `v-repeat25.json` |
| 45 registered listeners | `--mode repeat --registers 70` | **fail**: 0 of 71 called | `v-repeat.json` |

`v-longlived/` is the scenario run (`acos verify
results/handoff/BINDERCB/tail-telephony-longlived.yaml --engine art`), which also
passes: the `onCreate` listener gets the registry's initial state and the scripted
ring ~2 s later with no re-registration tap.

## Files

* `crates/acos-binder-peer/native/binder_driver.c` — `DEAD_OBJECT` for an
  unresolvable node, `warn_log` (stderr + logd), `ACOS_DRIVER_HOST_TEST` seam.
* `crates/acos-binder-peer/native/driver_test.c`, `crates/acos-binder-peer/tests/driver.rs`
  — the driver-level test (no ART).
* `crates/acos-binder-peer/src/peer.rs` — `REVERSE_DELIVERY_DEAD_OBJECT` record.
* `fixtures/apps/tail-probe/src/dev/acos/fixtures/tailprobe/MainActivity.java` —
  a "Stop listening" action (`listen(…, LISTEN_NONE)`) for the unregister flow.
* `results/handoff/BINDERCB/drive_mcp.py`, `tail-telephony-longlived.yaml`, the
  `v-*.json` evidence.

## Commands

```
export ACOS_FONT_DIR=/home/Qui/Projects/acos/.cache/boot/android-34/fonts
CARGO_TARGET_DIR=$PWD/.cache/bindercb-target cargo test --release --offline -p acos-binder-peer
ACOS_BIN=$PWD/.cache/bindercb-target/release/acos ACOS_ALLOWED_ROOTS=$PWD \
tools/art-runtime/run-exclusive.sh --task BINDERCB -- tools/guarded.sh --mem 12G --time 500 -- \
  env -u ACOS_HOME ACOS_RUNTIME_DIR=/run/user/1000/bindercb \
  ACOS_SESSION_SOCKET=/run/user/1000/bindercb/session.sock \
  python3 results/handoff/BINDERCB/drive_mcp.py --apk fixtures/build/apps/tail-probe.apk \
  --mode longlived --idle-s 65 --artifacts results/handoff/BINDERCB/v-longlived-60s \
  --out results/handoff/BINDERCB/v-longlived-60s.json
```
