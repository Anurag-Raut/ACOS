# P2d — the real `ServiceHost` behind the userspace binder driver

**Status: partial (hard 60-minute checkpoint).** The crate, its tests, and 7 of the
8 Java acceptance checks pass on real ART + stock `libbinder`. The remaining check
(20 peer-initiated deliveries) **crashes the dalvikvm process**; no fake pass is
claimed for it. Work is uncommitted, as instructed.

- Owner / date: P2d implementer agent, 2026-11-11.
- Time-box: 60 min hard checkpoint; stopped there (no loop).
- New files only: `crates/acos-binder-peer/**`, `results/art-runtime/P2d.md`.
  No existing crate touched (`acos-services`/`acos-parcel` are read-only path
  deps; `crates/acos-art` untouched).
- Runs under `tools/guarded.sh` (`ACOS_GUARD_TIME`), no sudo, no commit.

## What is implemented

`crates/acos-binder-peer` is a workspace crate (lib + `acos-binder-peer` binary):

| file | role |
|---|---|
| `native/binder_driver.c` | E3G's bionic `LD_PRELOAD` driver, copied and fixed in three places (below). |
| `src/protocol.rs` | E3G frame transport: `SOCK_SEQPACKET`, 48-byte header, parcel data + object offsets, `SCM_RIGHTS` fds. |
| `src/peer.rs` | the real `ServiceHost` peer: servicemanager, transaction forwarding, object-table translation, deliveries, structured `UNSUPPORTED_ANDROID_API` JSON lines. |
| `src/main.rs` | listen on a socket, serve one driver connection. |
| `build.rs` | best-effort NDK build of the driver (peer tests do not need it). |
| `tests/peer.rs` | 8 tests: getService, `IWindowManager.hasNavigationBar`, a real `IActivityManager.checkPermission`, addService, unknown code (JSONL evidence), 20 reverse deliveries at the `Frame` level, host error paths. |
| `tests/java/**`, `scripts/*.sh` | the ART acceptance dex (reuses E3G's `e3groot.sh`) and build/run scripts. |

### The service path

- `getService`/`checkService`/`listServices` go through
  `ServiceHost::transact(IServiceManager, …)` — i.e. the host's **real** service
  table, lazily constructed. `addService` is answered by the peer (it registers a
  binder that lives in the app process) and is logged.
- Every other transaction: driver peer-node id -> host `BinderId` ->
  `ServiceHost::transact(binder, code, data, reply, flags)`, with the request/reply
  object tables translated both ways.
- `IWindowManager` is listed as `acos-wm`-owned in `registry::NOT_OWNED`, so
  `ServiceHost::transact` rejects it before dispatch. The host *does* register a
  real `WmsService` under the ServiceManager name `window`; the peer routes that
  descriptor to the registered service object (same read_args/call/write_reply
  sequence as `transact_inner`, after the ownership check). `hasNavigationBar`
  therefore returns the real `WmsState` answer — not a proxy/default.
- `ServiceHost::take_deliveries()` -> `F_TXN` to the driver wire handle of the
  Java local binder. Only `JAVA_TAG|W` targets are delivered; anything else is a
  structured record, never silently dropped.
- Unknown `(descriptor, code)`: `ServiceHost` writes the exception reply
  (`EX_UNSUPPORTED_OPERATION`); the peer appends one JSON line
  `{"kind":"UNSUPPORTED_ANDROID_API","descriptor":…,"code":…,"method":…,…}` and
  returns that error status. Verified in `tests/peer.rs` and on ART.

### Driver changes vs E3G (the caveats E3G §5 named)

1. **Target translation**: a `BC_TRANSACTION` to BpBinder handle `H` now reaches
   the peer as the peer *node id* (the value in the parcel object), not the local
   handle number. E3G's single-service peer never noticed; with `window`,
   `activity_task`, `package` it is required. (`tr.target.handle` must be tested
   before `tr.target.ptr`: they are a union, so `ptr` reads non-zero for a handle.)
2. **Incoming transactions only go to a looper thread.** A thread that is merely
   transacting no longer consumes (and fails to dispatch) another thread's
   callback. This removes one E3G first-delivery race.
3. **Per-thread transaction-buffer lifetime**: the buffer behind a delivered
   `BR_TRANSACTION` is recorded on the receiving thread; `BC_FREE_BUFFER` only
   frees a buffer that thread owns. Stale frees are ignored.
4. `mm_alloc` is monotonic (no reuse of freed transaction buffers). E3G's stray
   `BC_REPLY`/already-freed-buffer symptom is consistent with premature reuse.

The in-process parcel format (`acos-parcel`) has no binder stability record;
`peer.rs` **strips** it on import and **inserts** `Stability::Level::SYSTEM` (=12)
4 bytes after every binder object on export, rebasing object offsets. Without
this, `readStrongBinder()` returns `null` with no error (E3G §3.2/§3.3).

## Evidence

```bash
cargo test -p acos-binder-peer
#   3 protocol tests + 8 peer tests: ok, 0 failed

crates/acos-binder-peer/scripts/build-art.sh
crates/acos-binder-peer/scripts/run-art.sh
```

ART run (dalvikvm64 in the E1A/E2D/E3G mini-root, `tools/guarded.sh`,
`/data/lib/libacos_binder_driver.so` preloaded, peer = real ServiceHost):

```
PASS substrate: 6 framework natives registered
PASS getService("window") -> android.os.BinderProxy alive=true
PASS getService("activity_task") -> android.os.BinderProxy alive=true
PASS getService("package") -> android.os.BinderProxy alive=true
PASS getService(unknown) -> null
PASS IWindowManager.hasNavigationBar(0) -> true (real WmsService)
PASS IActivityManager.checkPermission(INTERNET) -> PERMISSION_GRANTED
PASS unknown code 4000000 -> EX_UNSUPPORTED_OPERATION (structured)
<process dies before the reverse-delivery PASS>
```

Peer log from that run: one `UNSUPPORTED_ANDROID_API` JSON line for code 4000000;
`addService("acos.java") -> java binder wire=1`; driver connected/closed cleanly
from the peer's point of view. The JSONL file is written under
`.cache/art-exp-p2d/logs/`.

## Blocker (reverse deliveries)

`stepReverseDeliveries(20)` calls the real `IAutoFillManager.addClient` (oneway)
20 times with the Java `Receiver` as both binders; the `ServiceHost` queues
`IResultReceiver.send` to that binder each time (the in-memory test proves all 20
frames are produced, target = the receiver wire, code 1). On ART the process
**SIGSEGVs after 0–2 deliveries** (`libc: failed to connect to tombstoned`, no
tombstone file in the sandbox). Nothing is delivered after the crash.

Measured facts:

- `addService(receiver)` and the two `writeStrongBinder(receiver)` calls produce
  **three** driver nodes: wire 1 (cookie `…41d0`, the addService registration) and
  wires 2/3 (cookie `…42f0`, the two parcel writes). The weakref `binder` pointer
  differs between the two writes while the `BBinder*` cookie is the same.
- Keying nodes by cookie (instead of `(binder,cookie)`) makes every delivery
  target one stable wire (observed wire=3 every time), so the *choosing* of the
  target is now deterministic — **but the process still dies before the first
  callback is acknowledged** (`reverse_replies=0` in the peer log).
- E3G delivered to a binder registered through `addService` and needed a
  re-registration retry loop; here the failing target is a binder that was only
  ever written into a parcel, never registered as a service node. The E3G
  reverse path (and its fixes) exercised the *registered* node.
- No kernel-side evidence is available because the sandbox has no tombstoned.

Most likely cause: the delivered node's `BR_TRANSACTION.target.ptr`/`cookie`
lifetime. The driver treats the Java `BBinder*` as rooted, but a binder that only
appears inside a parcel is not acquired/rooted by the peer; when the transaction
is delivered on the looper thread, libbinder dereferences a `BBinder*` whose
holder may already be gone (the addService registration, cookie `…41d0`, is the
one that is actually rooted). This matches the crash-before-reply shape and the
E3G note that reverse delivery to a registered binder worked while ad-hoc
delivery was racy.

### Smallest next step (≤30 min)

1. Make the peer deliver to the **registered** Java binder rather than a
   parcel-only reference: keep `addService` name -> wire, and for
   `IApplicationThread`/`IResultReceiver` deliveries resolve the target through
   that registration (or have the driver `node_wire_for` reuse the registered
   node when `cookie` matches). Re-run the 20-delivery rung.
2. If it still dies, add `BR_ACQUIRE`/`BR_INCREFS` for exported nodes and a
   strong root for the `BBinder*` while a delivery is outstanding (E3G §1 lists
   both as stubs; this is the productisation item E3G §6 priced at 2–3 d for the
   full driver, but the reverse path alone is small).

Why this is worth continuing: everything else in the acceptance list now runs
against the real `ServiceHost` on real ART, including the one `IWindowManager`
call E4H needed; the remaining defect is confined to peer-initiated delivery
routing/rooting, not to the service layer or the transaction path.

## Not done / out of scope

- No transaction-buffer ownership protocol beyond the per-thread free check
  (monotonic allocation hides reuse).
- `BC_*_SG`, death notifications, weak refs, `BR_SPAWN_LOOPER` remain E3G stubs.
- One driver connection / one peer, as in E3G.
- Not committed.
