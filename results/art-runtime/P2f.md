# P2f — peer-initiated reverse deliveries into a *parcel-only* Java binder

**Status: done. The P2d SIGSEGV is fixed in the driver, and the parcel-only case
now has its own rung.** 5/5 consecutive ART runs pass all 9 checks, including 10
deliveries to a binder that was never `addService`-registered. `cargo test -p
acos-binder-peer` is green (12 tests). Work is uncommitted, as instructed.

- Owner / date: P2f implementer/debugger agent, 2026-11-11.
- Scope: `crates/acos-binder-peer/**` + this file only.
- Runs under `tools/guarded.sh` (`ACOS_GUARD_TIME`), no sudo, no commit.
- Host RAM during the runs: **35.0 / 61.9 GB used (57 %)**, guard cap 4 GB/session.

## Acceptance

| check | result |
|---|---|
| `scripts/run-art.sh`, 5 consecutive runs (after a clean build) | **5/5**, each `9 PASS / 0 FAIL`, `RESULT failures=0`, wall **~0.9 s** |
| reverse rung | `PASS 20 reverse IResultReceiver.send deliveries (10 addService-registered, 10 parcel-only binder), no race failure` |
| peer log of every run | `closed (unsupported=1 reverse_replies=20 reverse_failed=0)` |
| all prior rungs (substrate, 4× `getService`, `hasNavigationBar`, `checkPermission`, unknown code) | unchanged PASS |
| `cargo test -p acos-binder-peer` | 3 protocol + 9 peer tests, **ok** |
| ablation: `E3G_NO_NODE_REFS=1` | reproduces the P2d failure **on demand** (below) |

## The real cause

Two facts, found in this order; the first one is why P2d mis-diagnosed the
severity, the second is the bug.

### 1. "20 replies" was never "20 deliveries"

The peer counted every `F_REP` frame as a delivered callback. libbinder sets
`TF_STATUS_CODE` (0x08) in `binder_transaction_data.flags` when the transaction
**could not be dispatched at all** — the reply then carries a status word
instead of a parcel body, and `Binder.onTransact` never ran. With `E3G_DEBUG=1`:

```
BR_TRANSACTION seq=1..20 code=1 node=…e9c0/…12f0 size=104/0     (20 delivered)
REPLY seq=1  code=0 flags=0x0 size=4/0    <- onTransact ran
REPLY seq=2..20 code=0 flags=0x8 size=4/0 <- onTransact NEVER ran
```

`0x8` is `TF_STATUS_CODE` and the body is the error word, i.e.
`UNKNOWN_TRANSACTION` — which `IPCThreadState::executeCommand` produces from
exactly one place:

```c
if (tr.target.ptr) {
    if (((RefBase::weakref_type*)tr.target.ptr)->attemptIncStrong(this)) {
        error = ((BBinder*)tr.cookie)->transact(...);   // <- onTransact
        ((BBinder*)tr.cookie)->decStrong(this);
    } else {
        error = UNKNOWN_TRANSACTION;                    // <- what we saw
    }
}
```

So `attemptIncStrong()` was failing for every delivery after the first one, and
the Java test's own counter (`1/20 arrived`) was right all along.

### 2. The exported node was never rooted, so `target.ptr`/`cookie` were stale

The kernel *acquires* a node when it first becomes visible to another process: it
sends `BR_INCREFS` + `BR_ACQUIRE` to the **owner**, libbinder answers with
`weakref->incWeak(this)` / `((RefBase*)cookie)->incStrong(this)` and acks with
`BC_INCREFS_DONE` / `BC_ACQUIRE_DONE`. The P2d driver had both commands as
"acknowledged by doing nothing" stubs (E3G §1) and never emitted the `BR_*`
half, so nothing rooted the node.

That matters because a Java binder's `BBinder` is **not** kept alive by the Java
object:

```c
sp<IBinder> ibinderForJavaObject(...) {          // libandroid_runtime
    JavaBBinderHolder* jbh = …; return jbh->get(env, obj);   // sp<JavaBBinder>
}
sp<JavaBBinder> JavaBBinderHolder::get(...) {
    sp<JavaBBinder> b = mBinder.promote();        // mBinder is a *weak* pointer
    if (b == NULL) { b = new JavaBBinder(env, obj); mBinder = b; }
    return b;
}
```

The `sp<>` returned by `ibinderForJavaObject()` dies at the end of
`Parcel::writeStrongBinder()`, and the holder only keeps a `wp`. Without the
node's strong ref the `JavaBBinder` is destroyed the moment the write returns, so
the `cookie` (BBinder\*) and `target.ptr` (weakref) cached in the driver's node
table point into freed memory from the next delivery onwards.

Direct evidence, from the driver's node table:

| build | nodes created |
|---|---|
| P2d / no rooting | `wire=1 cookie=0x…411530` (the `addService` write) **and** `wire=2 cookie=0x…4112f0` (the first parcel write) — the *same* Java `Binder` produced **two** `JavaBBinder`s, i.e. the first one had died between the two writes |
| P2f (rooted) | `wire=1` only; all 20 parcel writes reuse it |

It also explains the P2d note "same `BBinder*` cookie but different weakref
pointers": that is the same heap address handed out to a *new* `JavaBBinder`
whose `weakref_impl` is a separate allocation. The new canary in
`node_wire_for()` prints it verbatim in the ablation build:

```
E3G[warn]: node wire=2 cookie=0x…1f90: weakref changed 0x…16c0 -> 0x…1330
           (object replaced under a live node?)
```

### 3. The crash itself (native backtrace)

The sandbox has no tombstoned, and **gdb hides the fault** (under gdb the run
never segfaults — it only shows `1/20`, because the extra scheduling delay lets
`attemptIncStrong` fail cleanly instead of winning against recycled memory). So
the driver grew an optional in-process crash reporter (`E3G_CRASH=1`) that dumps
the fault address, `ip`/registers, every stack word that resolves to a module,
the `/proc/self/maps` line for each, and the node table. Ablation build
(`E3G_NO_NODE_REFS=1 E3G_CRASH=1`), 2 of 3 runs:

```
=== ACOS binder driver: fatal signal 11 (code=1) ===
fault_addr=0x0 ip=0x7fe1e995b79e rsp=… rbp=0x7fe3b78339a0
  ip        0x7fe1e995b79e /system/lib64/libutils.so+0x1079e
                          _ZNK7android7RefBase9decStrongEPKv
stack scan (module + offset, most recent first):
  [rsp+ 24] …/libbinder.so+0x6fe5b  _ZN7android6Parcel14freeDataNoInitEv
  [rsp+104] …/libbinder.so+0x912cd  _ZN7android6Parcel8freeDataEv
  [rsp+ 24] …/libbinder.so+0x65baf  _ZN7android14IPCThreadState14executeCommandEi
  [rsp+744] …/libbinder.so+0x8d230  _ZN7android14IPCThreadState10freeBufferEPKhmPKym
  [rsp+824] …/libbinder.so+0x893e8  _ZN7android14IPCThreadState14joinThreadPoolEb
  [rsp+920] /data/lib/libacos_binder_driver.so+0xace8   (our ioctl interposer)
  [rsp+968] /data/lib/libacos_binder_driver.so+0x6968   (looper thread start)
driver node table (2):
  wire=1 weakref=0x7fe3b78339a0 cookie=0x7fe327802390
  wire=2 weakref=0x7fe3b7834600 cookie=0x7fe327803290
  this thread: looper=1 have_reply_to=1 reply_to_seq=2 pend=0 txn_bufs=1
```

Read it bottom-up: our looper thread → `joinThreadPool` → `executeCommand`
(BR_TRANSACTION) → `Parcel::freeData/freeDataNoInit` (the transaction `Parcel`'s
destructor) → **`RefBase::decStrong` dereferencing a null `mRefs`**
(`fault_addr=0x0`): `attemptIncStrong()` had succeeded against a *recycled*
weakref, `BBinder::transact()` ran on the destroyed object, and `decStrong()`
then read `mRefs == nullptr` out of the freed `BBinder`. First delivery is fine
(the object is still alive inside the `writeStrongBinder()` call on the main
thread), delivery #2 dies — matching P2d's "0–2 callbacks".

## The fix

### Driver (`native/binder_driver.c`)

1. **`node_ref_cmds()` — root the node at creation, like the kernel.** When
   `node_wire_for()` creates a node it appends
   `BR_INCREFS{binder_ptr_cookie{ptr=weakref, cookie=BBinder*}}` followed by
   `BR_ACQUIRE{…}` to the *creating* thread's pending `BR_*` queue, so libbinder
   does `incWeak` + `incStrong` on the object before anybody can be delivered to
   it. `BC_INCREFS_DONE`/`BC_ACQUIRE_DONE` were already parsed (and are still
   ignored: the driver does not need the kernel's `has_weak_ref/has_strong_ref`
   state machine). The refs are held for the process lifetime — which is exactly
   what the kernel does while another process holds the node, and the peer in
   this prototype never drops it.
2. **Use-after-free canary.** A known `cookie` seen with a different `weakref`
   now warns loudly (`object replaced under a live node?`) and keeps the rooted
   node instead of silently switching to an unrooted pointer.
3. **`E3G_NO_NODE_REFS=1`** disables (1), so the P2d failure stays reproducible
   on demand — the ablation in §3 uses it.
4. **`E3G_CRASH=1`** installs the fault reporter described above (off by
   default; it is the only way to get a native backtrace in this sandbox).

### Peer (`src/peer.rs`, `src/protocol.rs`, `src/main.rs`)

5. `TF_STATUS_CODE` replies are no longer counted as deliveries:
   `reverse_failed` is tracked separately, logged per occurrence
   (`reverse delivery seq=N was NOT dispatched`), and printed in the close line
   (`reverse_replies=20 reverse_failed=0`). This is the reporting defect that
   made P2d's evidence look ambiguous.
6. `protocol::TF_STATUS_CODE` is documented as part of the wire contract.

### ART acceptance dex (`tests/java/dev/acos/binderpeer/Main.java`)

7. The reverse rung is now **20 deliveries split 10/10**: half to the
   `addService`-registered `Receiver`, half to a second `Receiver` that is never
   registered and *only* ever travels inside a parcel — the exact P2d case. Both
   latches must reach zero.
8. `register_android_util_EventLog` added to the substrate list. Without it a
   cold-dalvik-cache process can throw
   `InvocationTargetException`/`UnsatisfiedLinkError` for
   `EventLog.writeEvent` out of the first `ServiceManager.getService()` (seen
   once in 5 runs; it is not binder-related, it is a hole in the mini-root's
   registered-native list).

### Test / debug tooling

9. `tests/peer.rs::reverse_reply_status_codes_are_not_counted_as_deliveries`
   pins (5).
10. `scripts/dbg-art.sh` runs the same dex **under host gdb** in the same mount
    namespace (it re-creates `e3groot.sh`'s bwrap args plus a host `/usr` bind;
    `NO_GDB=1` runs without gdb). It found the `TF_STATUS_CODE` pattern; note
    that it *cannot* reproduce the SIGSEGV (see §3).

## Reproduce

```bash
crates/acos-binder-peer/scripts/build-art.sh          # driver + peer + dex
crates/acos-binder-peer/scripts/run-art.sh            # under tools/guarded.sh

# bug reproduced on demand (P2d behaviour):
E3G_NO_NODE_REFS=1 E3G_CRASH=1 crates/acos-binder-peer/scripts/run-art.sh
#   -> FAIL reverse deliveries: registered 1/10, parcel-only 0/10
#   -> and/or the decStrong backtrace above

# driver trace / node table:
E3G_DEBUG=1 crates/acos-binder-peer/scripts/run-art.sh | grep 'node:'
# native gdb (masks the race, still useful):
crates/acos-binder-peer/scripts/dbg-art.sh
```

Ablation results (3 runs, this machine):
`signal 6`, `signal 11`, `signal 11`; delivery success `registered 1/10,
parcel-only 0/10` in every run.

## Not done / unchanged limits

- **`BR_RELEASE`/`BR_DECREFS` are never emitted.** An exported node keeps its
  strong+weak ref for the process lifetime. Correct for a peer that holds the
  node forever (ours does); a peer that drops references needs the kernel's
  refcount state machine, otherwise the binder leaks.
- Death notifications (`BR_DEAD_BINDER`), `BR_SPAWN_LOOPER`, SG transactions,
  and `BC_ATTEMPT_ACQUIRE` remain E3G/P2d stubs.
- One driver connection / one peer; one node table per process.
- `target.ptr`/`cookie` are now always the rooted pair, but
  `tr.sender_pid`/`sender_euid` are still 0 (`Binder.getCallingUid()` reports the
  app itself).
- gdb is *not* a usable reproduction path for this bug: it changes the timing
  enough that `attemptIncStrong` fails cleanly instead of faulting.

## Files

| file | change |
|---|---|
| `native/binder_driver.c` | +186/−4: `node_ref_cmds()` (BR_INCREFS+BR_ACQUIRE at node creation), stale-weakref canary, `E3G_NO_NODE_REFS`, `E3G_CRASH` fault reporter |
| `src/protocol.rs` | `TF_STATUS_CODE` constant + docs for the frame constants |
| `src/peer.rs` | `reverse_failed`, per-frame failure log, `Debug` |
| `src/main.rs` | close line reports `reverse_failed` |
| `src/lib.rs` | re-export |
| `tests/peer.rs` | new status-code accounting test; warning-free |
| `tests/java/.../Main.java` | 10+10 parcel-only/registered reverse rung, `register_android_util_EventLog` |
| `scripts/dbg-art.sh` | **new**: run the dex under host gdb inside the mini-root |
