# BINDERCB2 — binder driver, session 2 (status: PARTIAL — item 2 fixed, item 1 at an architectural wall)

Branch `bindercb2` from `bindercb` (`db0cf59d`). Worktree env per
BRIEF_COMMON_0316: private `ACOS_RUNTIME_DIR=/run/user/1000/bindercb2`, private
`ACOS_SESSION_SOCKET`, every ART run under
`tools/art-runtime/run-exclusive.sh --task BINDERCB2` + `tools/guarded.sh`.
Target dir `.cache/bindercb2-target` (deleted at the end).

Read `results/handoff/BINDERCB/README.md` first for the repro drivers
(`drive_mcp.py --mode repeat|longlived|unregister`) and the session setup.

## Item 1 — stale weakref: implemented as decided, but it is NOT sufficient (architectural)

**Done, tested, no wire change.** `node_wire_for()` (native/binder_driver.c) no
longer just warns when an existing node's object changed: it re-points the node
at the object that is registered *now* (`n->binder`/`n->cookie`), issues
`BR_INCREFS` + `BR_ACQUIRE` for it, and keeps `n->wire` stable (the remote side
holds a reference to the wire, not to the pointer). Driver test checks 9 and 10
prove it: eight distinct objects are registered for one event, each is
re-exported with a new weakref under the same cookie; every wire must stay, every
new object must be rooted, and every callback must be addressed at the object
registered now. Before the fix `re-rooted=0 of 8, addressed=0 of 8`.

**But the real 25-of-26 loss is a different defect, and the driver cannot fix it.**
New ART evidence with this fix in place (`--mode repeat`):

| run | registrations that reached the registry | callbacks dispatched | file |
|---|---|---|---|
| `--registers 25` | 26 of 26 | **1** of 26 | `repeat25.json` |
| `--registers 45` | 46 of 46 | **1** of 46 | `repeat45.json` |
| `--registers 100` | 60 of 101 (see item 2) | **0** of 101 | `repeat100.json` |

Every registration's *initial* state callback is delivered (`IDLE` count ==
registered count: 26, 46, 60) and the driver logs **no** warning at all (no
`unknown node`, no `DEAD_OBJECT` reply, no `object replaced`): the deliveries were
queued and addressed to the right nodes, and libbinder dropped them.

Root cause, from `base/core/jni/android_util_Binder.cpp` +
`libs/binder/IPCThreadState.cpp` in `.cache/aosp`:

* `ibinderForJavaObject()` -> `JavaBBinderHolder::get()` returns a *temporary*
  `sp<JavaBBinder>`; the holder keeps only a `wp` (the file even notes "maybe
  make JavaBBinder an sp here (avoid recreating it)").
* So the `JavaBBinder` is destroyed at the end of
  `Parcel.writeStrongBinder(...)`, **before** the driver ever sees the flat
  object (the driver only runs at `BC_TRANSACTION`). The `weakref_impl` survives
  only through the holder's weak pointer, at an address that the *next*
  `new JavaBBinder` reuses.
* The driver's `BR_INCREFS`/`BR_ACQUIRE` therefore lands on a zombie: it does not
  keep the object alive, and the pointers cached for listener *N* are overwritten
  by the `JavaBBinder` created for listener *N+1*. At delivery time libbinder
  runs `((RefBase::weakref_type*)tr.target.ptr)->attemptIncStrong(this)`
  (`IPCThreadState.cpp:1404`) and takes the `UNKNOWN_TRANSACTION` branch, so the
  transaction is dropped inside the app process. That is why exactly the newest
  listener answers, and older ones only while their memory has not been recycled
  (which is also why the single long-lived listener always works: the app keeps
  its Java object, and nothing recycles the block in between).

Why this is a wall (stopped here, per the brief):

* The reference must be taken **while the owner still holds the object**, i.e. in
  libbinder's flatten path (`writeStrongBinder`), and released when the peer
  drops the node. That is the node/ref model the orchestrator deferred to
  BINDERREF: a host-held strong reference per registered callback plus a
  `F_REL`-style release when the host unregisters/deaths, and a driver-side
  per-node refcount fed by it. A driver-only "refresh on re-export" cannot help,
  because a listener that is registered once is never re-written.
* Options for the orchestrator:
  1. BINDERREF as planned (correct, largest): host strong ref per registered
     callback + release message + driver node refcount; then this fix's refresh
     path becomes the re-root hook.
  2. Interpose `android_os_Parcel_writeStrongBinder` (the driver already
     interposes `__system_property_get`, `open`, `ioctl` and JNI registration) and
     hold an `sp` per flattened local binder, released on peer node death. Small
     diff, but it *is* a ref model, just smuggled into the substrate.
  3. Accept 1 call per event per app (document the limitation) — not acceptable
     for the PHONE2 follow-up.

## Item 2 — the ~45-registration wedge: found and fixed (no wire/threading change)

**Root cause: the transaction-buffer heap was shared without a lock.**
`mm_alloc()`/`mm_free()` (and the reply-side fd set) mutate `g_bump`, `g_blk` and
`g_nblk` with no mutex, but they are reached from two threads at once: the
transacting (UI) thread's reply path (`build_reply`) and the looper thread's
delivery path (`deliver_incoming`). Two threads read the same `g_bump` and get
the **same offset**: one transaction's parcel is written over another one while
libbinder is still decoding it. That is exactly the reported symptoms — the short
guest->host frame decoded as `parcel underflow: need 144 byte(s) at 224, have 4`
on the host, the silent registrations that reached the registry with no binder
(a corrupted parcel flattens to a null object, so `listen()` "succeeds" and the
registry records nothing), and the wedge once the corruption hits a request whose
reply the peer then cannot match.

**Fix (driver-internal only).** Three new locks, never held across `g.mu` (which
stays transport-only), so there is no lock order to get wrong:

* `g_slab_mu` — the transaction-buffer heap (`mm_alloc`/`mm_free`, block table
  growth) and the reply-side fd sets (`remember_fds`/`release_fds`). The block
  table now grows (`realloc`, capped at 262144 entries, warned instead of
  silently stopped at the old fixed 512) and the per-free `qsort`+coalesce walk
  is gone: allocation is monotonic, so nothing is ever re-allocated and there is
  no free list to coalesce.
* `g_obj_mu` — the node/handle tables (any thread can export a local object or
  resolve a peer node). `node_by_wire()` became `node_for_wire(wire, &out)`, a
  copy under the lock, so a delivery uses the object that is current when the
  delivery is built instead of a pointer that another thread may refresh.
* `E3G_MAX_NODE` 64 -> 1024 and `E3G_MAX_HANDLE` 64 -> 512, driver memory only
  (nodes are released only by the host, which is BINDERREF's job).

Driver test proves it (checks 11 and 12): 16 threads x 2000 concurrent
`mm_alloc` must produce 32000 non-overlapping buffers (before:
`overlapping=31..4164` in every run), and 96 registered objects must all reach
their callback (before: `exported=55 of 96`, because the node table was 64).

ART effect at 45 registrations: **46 of 46 `listen()` calls now reach the
registry** (BINDERCB measured 44 of 70, with the rest silently lost), no stall of
the app main thread, `IDLE` count 46. The ring still only reaches 1 listener —
that is item 1's wall, not this one.

**Still open (item 2's `--registers 100`):** registrations stop at **60** of 101
(`repeat100.json`, 60 registry listeners, 60 IDLE, no ring). This is a *different*
capacity wall from the one fixed here; it reproduces deterministically with
`--registers 100` and needs the buffer/node-lifetime model (1 MB monotonic
transaction-buffer map and/or the same rooting defect), i.e. BINDERREF. Not
worked around.

## Regression proofs (unchanged, both pass with these changes)

| run | result | file |
|---|---|---|
| `--mode longlived --idle-s 65` | pass (`CALL state=RINGING number=+15550002222`) | `longlived.json` |
| `--mode unregister --idle-s 65` | pass (no callback after `listen(..., LISTEN_NONE)`) | `unregister.json` |

## Tests

`cargo test -p acos-binder-peer -p acos-services` green (12 driver checks, all
other suites unchanged). `driver-test-before.txt` / `driver-test-after.txt` are
the same test binary against the driver from `HEAD` and the fixed driver.

## Commands

```
export ACOS_FONT_DIR=/home/Qui/Projects/acos/.cache/boot/android-34/fonts
CARGO_TARGET_DIR=$PWD/.cache/bindercb2-target cargo test --offline -p acos-binder-peer -p acos-services
ACOS_APP_ONLY=tail-probe tools/build-apps.sh          # fixtures/build/apps/tail-probe.apk

ACOS_BIN=$PWD/.cache/bindercb2-target/release/acos \
ACOS_ART_BIN=$PWD/.cache/bindercb2-target/release/acos-art \
ACOS_ALLOWED_ROOTS=$PWD \
tools/art-runtime/run-exclusive.sh --task BINDERCB2 -- tools/guarded.sh --mem 14G --time 900 -- \
  env -u ACOS_HOME ACOS_FONT_DIR=... ACOS_RUNTIME_DIR=/run/user/1000/bindercb2 \
  ACOS_SESSION_SOCKET=/run/user/1000/bindercb2/session.sock \
  python3 results/handoff/BINDERCB/drive_mcp.py \
  --binary $PWD/.cache/bindercb2-target/release/acos-mcp \
  --apk fixtures/build/apps/tail-probe.apk --mode repeat --registers 45 --idle-s 1 \
  --artifacts $PWD/.cache/bindercb2-run/a45 --out <out>.json
```

## Files

* `crates/acos-binder-peer/native/binder_driver.c` — node refresh + re-root,
  `g_slab_mu`/`g_obj_mu`, growing block table, `node_for_wire`, bigger node/handle
  tables.
* `crates/acos-binder-peer/native/driver_test.c`, `tests/driver.rs` — checks 9-12
  (fails before, passes after), librarier reply draining (`guest_wait_reply`) and
  the looper's real `BC_REPLY` + `BC_FREE_BUFFER`.
* Evidence in this directory.
