# FLAKEWIN — the v0.3.0 launch flake (`Adding window failed`)

Track: FLAKEWIN (branch `flakewin` off `rel-030` = `0e3719bf`).
Scope: `addToDisplayAsUser` fails at launch with
`RuntimeException: Adding window failed`, the guest exits 1 after resume
(`GUEST_EXIT_AFTER_RESUME`) and the same launch passes on a re-run.

## 1. What the shim actually answered — the crash is a transport corruption

`ViewRootImpl.setView` throws `Adding window failed` when the `int res` it reads
from `IWindowSession.addToDisplayAsUser` is negative (other than the named
cases), or when the binder call itself failed. The captured guest report
(`results/handoff/REL3H2/rel3-art-tests-run1.log`, and the preserved S3ab
occurrence `results/bench/S3ab-evidence/repro-n20-binder-parcel/session-exit.json`)
says which:

```
E/Parcel: ipcSetDataReference: unsupported type object (7209057) at offset 16
Caused by: java.lang.RuntimeException: Unknown exception code: -1040187388 msg null
    at android.view.IWindowSession$Stub$Proxy.addToDisplayAsUser(IWindowSession.java:1250)
Caused by: android.os.RemoteException: Remote stack trace:
android.view.IWindow
```

Decoding the reply parcel the guest received:

| read | value | what it is |
|---|---|---|
| exception header | `-1040187388` = `0xC2000004` | **not** an exception code (`-1..-9`, `-127`, `-128`); it is the first word of a `Parcel::writeInterfaceToken` prelude (`W8.md` §1: `[0xc2000004][-1]["TSYS"][27][UTF-16 descriptor]`) |
| message (`readString`) | `null` | the next int (`-1`, `workSourceUid`) read as a negative String16 length |
| remote-stack size | `20` | the token's String16 **length** |
| remote stack (`readString`) | `android.view.IWindow` | the token body — an **`IWindow` request**, not an `IWindowSession` reply |
| object table [0] | `16`, type `7209057` = `0x006E0061` = `'a',0,'n',0` | the object table points at offset 16, which now holds the *request token's* first UTF-16 characters |

So the guest did not get a negative `res` from the shim: **it read another
transaction's request parcel as its add reply**. Offset 16 is the tell. In the
shim's own reply the first object is exactly there (`write_no_exception` → 4,
`int result` → +4, `InputChannel` presence → +4, `isInitialized` → +4, then the
`InputChannel` token `flat_binder_object` at **16**, `write_add_result` /
`write_input_channel` in `crates/acos-services/src/window_session.rs`). In an
`IWindow` **request** the token body also starts at 16 (policy, workSourceUid,
`TSYS`, length = 16 bytes of prelude). The guest therefore held a parcel whose
**object table is the add reply's** and whose **data is a request parcel**: two
writers, one buffer.

The `IWindow` request in flight at that moment is the one the shim sends right
after the add reply: `IWindow.dispatchAppVisibility(true)`
(`WindowSessionService::drain_raw_deliveries`, W6/W9). The two parcels are
produced back to back on the wire, and on the guest they are installed by two
different threads.

## 2. Root cause: the driver's transaction-buffer allocator had no lock

`crates/acos-binder-peer/native/binder_driver.c` allocated transaction buffers
from a bump heap (`mm_alloc`) with **no lock**, while the driver allocates from
two guest threads at once *by design*:

* `e3g_write_read` → `build_reply` — the reply for a synchronous transaction, on
  the thread that is blocked in `transact` (here: the app's main thread, waiting
  for `addToDisplayAsUser`);
* `deliver_incoming` — a peer-initiated `BR_TRANSACTION`, on the receiving
  binder-looper thread (here: `IWindow.dispatchAppVisibility`).

Both call sites reach `mm_alloc` with `g.mu` **released**, so two callers could
read the same `g_bump`, get **the same offset**, and the two `memcpy`s wrote the
same block in place. `build_reply` writes the data at `buf` and the object table
at `buf + align(data_size)`; when a later parcel overwrites `buf`, the reply keeps
its own object table and loses its data — exactly the mixture observed in §1
(`table[0] = 16`, data = an `IWindow` request). No other code path in the driver
or the peer writes into a buffer it has handed out (the heap is monotonic, blocks
are never re-used, `BC_FREE_BUFFER` only clears bookkeeping).

AOSP's kernel driver allocates under `binder_alloc`'s `alloc->mutex` for the same
reason. That mutex is what was missing.

## 3. Fix

* `crates/acos-binder-peer/native/txn_heap.{c,h}` (new): the heap as its own
  translation unit, guarded by a leaf mutex (`txn_alloc`/`txn_free`,
  monotonic policy unchanged, `txn_heap_init` takes the region). Being free of
  bionic/JNI, it is compiled and raced **on the host** by a test.
* `binder_driver.c`: uses `txn_alloc`/`txn_free`; `driver_init` initialises the
  heap over the memfd mapping (`map + 4096`, first page never handed out).
* Resource limit, named instead of silent: heap exhaustion logs
  `ART_RESOURCE_LIMIT: transaction buffer exhausted (...)`, and a reply that
  cannot be allocated now emits `BR_FAILED_REPLY` (libbinder:
  `RemoteException("Transaction failed")`, which AOSP's `setView` handles)
  instead of appending only `BR_TRANSACTION_COMPLETE` and leaving the guest
  blocked in `waitForResponse` for a reply that can never come.
* `crates/acos-binder-peer/build.rs` and `crates/acos-art/src/launch.rs`
  (`build_driver`): compile the new unit; the heap sources are part of the
  driver hash **and** the content-addressed build-cache key (`binder-driver/v3`),
  so no cached `.so` can carry the old allocator.
* `crates/acos-binder-peer/tests/txn_heap.rs` (new, regression): 4 threads ×
  50 000 allocations through the real `native/txn_heap.c`, requires zero
  duplicated/overlapping blocks; plus the foreign-free and exhaustion edge
  cases.

## 4. Proof

| evidence | result |
|---|---|
| regression test against the pre-fix allocator | **FAILS** — `duplicates=7979 overlaps=101657` / 200 000 allocations (`regression-before-fix.log`) |
| regression test against the fix | **PASSES** — `duplicates=0 overlaps=0` (`cargo test -p acos-binder-peer`, 17/17) |
| launch loop, fixed driver, concurrency 1 | **800/800** (`loops/loop-c1-800.jsonl`), plus 160/160 and a 12-launch smoke |
| launch loop, fixed driver, concurrency 4 | **800/800** (`loops/loop-c4-800.jsonl`) and **400/400** on the final committed tree (`loops/final-loop-c4-400.jsonl`) |
| launch loop, pre-fix driver (racy heap) | **800/800** at concurrency 1 (`loops/before-loop-c1-800.jsonl`) and **3000/3000** at concurrency 4 (`loops/before-loop-c4-3000.jsonl`) — see the caveat below |
| pre-fix driver with a 2 ms widened allocation window (fault injection, never shipped) | 6/6 launches pass; the injected ordering corrupts the *delivery* (later writer), not the reply |
| `cargo test --release -p acos-services -p acos-art` | 63 suites, **575 passed, 0 failed** (`cargo-test-services-art.log`) |

Cohort table: `launch-loops-summary.txt`. Loop driver:
`launch-loop.sh` (one fresh ART guest per launch, `counter.apk` +
`hello-views.apk`, one daemon per (worker, app) so concurrency 4 is four
independent sessions).

### The honest caveat about the before rate

The real-world before rate is **2 occurrences in ~3 000 resets** (S3ab,
`results/bench/S3ab-evidence/`) plus 1 in the REL3H2 ART run — i.e. ~1e-3 per
launch. My loop is a *single* allocation pair per launch, and the collision needs
the two threads inside a ~2 ns window, so the loop is far too weak to resolve
1e-3: **0/3 800 launches with the racy allocator is expected** (P(0) ≈ e^-2.5)
and therefore is **not** evidence against the mechanism, and the 1 600/1 600
after the fix is not, on its own, proof that the flake is gone. What is proven is
the defect: the allocator hands the same block to two live transactions (4 % of
allocations in a tight two-thread race), and that is the only way the §1 mixture
(reply's object table + another transaction's data in one buffer) can exist.

## 5. Open / next

* End-to-end reproduction needs a *deterministic* trigger, not a loop: either a
  driver test hook that forces the second allocation of a chosen pair onto the
  same offset, or a guest-side probe that dumps `addToDisplayAsUser`'s reply
  bytes and object table on every launch and asserts `table[0] == 16 &&
  data[0] == 0`. The second is the smallest next action and would have caught
  both historical occurrences.
* The heap is still monotonic, so a long-lived guest (thousands of transactions)
  eventually exhausts it; that is now a named `ART_RESOURCE_LIMIT` +
  `BR_FAILED_REPLY`, but a safe re-use policy (epoch/`BC_FREE_BUFFER`-driven)
  remains a separate piece of work.
