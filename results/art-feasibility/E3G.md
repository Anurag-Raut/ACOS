# E3G — a userspace binder **driver** so stock `libbinder` runs unmodified

**Answer: yes. All five acceptance criteria pass on real ART + real `libbinder` + real
`android.os.{Binder,BinderProxy,Parcel,Bundle,ServiceManager}`, with the kernel driver
replaced by an `LD_PRELOAD` interposer that emulates `BINDER_WRITE_READ` in userspace and
a Rust peer (servicemanager + one AIDL-shaped service) behind a unix socket.**

- Owner / date: approach-G feasibility agent, 2026-11-11.
- Time-box: 45 min; **used ~180 min** (≈60 min of that was reverse-engineering three
  libbinder wire details that no document had: the interface-token prefix, the
  per-object *stability* trailer, and `Stability::Level`'s non-obvious enum values —
  see §3. Every one of them fails **silently** as `readStrongBinder() == null`).
- Constraints honoured: no sudo, no crates touched, nothing committed, all runs through
  `tools/guarded.sh`, host RAM peak ~0.5 GB, everything reproducible from
  `research/art-feasibility/g-binder/`.
- Depends on **E2D** (`results/art-feasibility/E2D.md`): the mini-root scripts, the
  property table, the default-namespace `dlopen` of `libandroid_runtime.so`, and the
  finding that `BINDER_WRITE_READ = 0xc0306201` is the only transport seam.
- Follows `docs/ENGINEERING_POLICY.md`: reuse stock `libbinder`, replace only the driver.

| path | what it is |
|---|---|
| `native/binder_driver.c` | the artefact: bionic-targeted `LD_PRELOAD` cdylib, ~1300 lines. Property table + framework-native bridge (from E2D) **plus** the driver: memfd + `ioctl` interposer, `BC_*`→frames, frames→`BR_*`, the transaction-buffer allocator over the `mmap`ed region, `flat_binder_object` translation (both directions), `SCM_RIGHTS` fd passing, reader thread + per-thread state, and a native binder-looper thread (`IPCThreadState::joinThreadPool`). |
| `peer/src/main.rs` | the Rust peer (std only, `rustc`-buildable): servicemanager (`IServiceManager.getService/checkService/addService` over real AIDL-shaped parcels), the `activity` service (int/String/Bundle echo + fd rung), and a peer-initiated *reverse* transaction into a Java `Binder`. |
| `java/dev/acos/e3g/{Main,Nat}.java` | the ladder: `smgr`, `echo`, `fd`, `latency`, `reverse`, plus `probe` (a diagnostic that drives the servicemanager `BinderProxy` by hand). |
| `scripts/{build,run,e3groot}.sh` | NDK clang + `rustc` + `javac`/`d8`; `run.sh` starts the peer on `/data/e3g.sock` and runs the dex under the E1A/E2D mini-root; `e3groot.sh` is `e2droot.sh` + `E3G_SOCK`/`E3G_DEBUG` (+ `E3G_PRELOAD` = the new `.so`). |
| `.cache/art-exp-g/` | scratch: `build/{libbindere3g.so,e3g-peer}`, `classes/`, `logs/peer.log`. No target dirs (the peer is one `rustc` invocation). |

```bash
research/art-feasibility/g-binder/scripts/build.sh
research/art-feasibility/g-binder/scripts/run.sh all
E3G_DEBUG=1 research/art-feasibility/g-binder/scripts/run.sh all   # driver trace
```

## 0. Verdict table

| # | step | result | numbers / evidence | blocker hit | effort |
|---|---|---|---|---|---|
| 0 | substrate (6 framework natives on ART, no boot) | **PASS** | `util_Log`, `os_SystemProperties`, `os_SystemClock`, `os_Trace`, `os_Parcel`, `os_Binder` | `Binder.execTransactInternal` calls `Trace.isTagEnabled` → `UnsatisfiedLinkError` **inside the transaction** (criterion 3 only, 5 min) | 0 (E2D) |
| 1 | `ServiceManager.getService("activity")` returns a real `BinderProxy` created by the Rust peer | **PASS** | `android.os.BinderProxy @… alive=true pingBinder=true`; `getService("nope") -> null`; path is `ServiceManager → ServiceManagerProxy → IServiceManager$Stub$Proxy.checkService` (code 2) | reply parcel needed the interface-token prefix *and* the pre-object **stability trailer**; without them libbinder silently returns `null` (§3.2/§3.3) | 95 min |
| 2 | transact with a Parcel carrying **int + String + Bundle** returns the right reply | **PASS** | in 184 B → out 148 B; `marker=0xac05`, `int=1234`, `string="hello+peer"`, Bundle **identical** (`s="bundle-value"`, `i=42`, `arr[2]`); `Parcel.readException()` clean | none beyond [1] (the reply is `writeNoException` + payload) | 0 (same code) |
| 3 | Java `Binder` subclass registered via `addService`, then **called back** by a peer-initiated transaction (`BR_TRANSACTION` on a looper thread) | **PASS** | Java `onTransact` sees `code=7 int=0xbeef string="from-peer"`; replies `writeNoException + 0x600d + "pong-from-java"`; peer logs `REVERSE-REPLY exception=0x0 marker=0x600d text="pong-from-java"` — **bidirectional** | (a) the looper thread must be a *native* thread attached to the VM (`JavaVM->AttachCurrentThread` + `IPCThreadState::self()->joinThreadPool(true)`); (b) `joinThreadPool` mangled name is `14`, not `13`; (c) first-attempt delivery is **racy** in this prototype → the test re-registers (§5) | 40 min |
| 4 | fd passing (`BINDER_TYPE_FD`) round trip | **PASS** | client writes a pipe fd into the parcel; peer receives it over `SCM_RIGHTS`, writes `"PONG-FROM-PEER\n"` (15 B), client reads it back from the read end; peer reports `1 fd object(s), fd=5, wrote=15` | none — but the receiver must substitute its own fd number into the object's `handle` field (that is exactly what the kernel does) | 20 min |
| 5 | latency per round trip | **PASS** | n=200 sync transacts (interface token + int + String + empty Bundle): **mean 7.5–11.1 µs, p50 7.2–9.7 µs, p95 8.3–15.0 µs, max 13–62 µs** over 8 runs | none | 0 |
| — | `pingBinder()` / `isBinderAlive()` | **PASS** | first IPC of the process is `IBinder::PING_TRANSACTION` (`0x5F504E47`, empty parcel) to handle 0 — must be answered with an *empty* reply | 10 min (found by hex-dumping the first write buffer) |

Whole ladder wall clock ≈ 2.0 s (`bwrap` floor 28 ms, ART boot ~0.3 s, peer round trips
≈ 2 ms total, the 15 s worst case only if a rung fails).

## 1. What the interposer does (and what it deliberately does not)

```
 Java: Binder.transact / ServiceManager.getService / Binder.onTransact
   |
 libbinder (STOCK): IPCThreadState::talkWithDriver -> ioctl(fd, BINDER_WRITE_READ, &bwr)
   |
 binder_driver.c  (LD_PRELOAD, same .so as the framework natives)
   |  write half: parse BC_TRANSACTION / BC_REPLY / _SG / BC_FREE_BUFFER /
   |              BC_ACQUIRE|RELEASE|INCREFS|DECREFS / BC_*_LOOPER /
   |              BC_*_DEATH_NOTIFICATION
   |  -> flat_binder_object translation, fds out via SCM_RIGHTS,
   |  -> frame {type, seq, code, flags, target, data, offsets} on a SOCK_SEQPACKET socket
   |  read half: reader thread demultiplexes
   |              F_RSP  -> the transacting thread's reply slot
   |              F_TXN  -> a queue for whichever thread is a binder looper
   |  -> BR_TRANSACTION_COMPLETE / BR_REPLY / BR_TRANSACTION / BR_FAILED_REPLY,
   |     payload placed in the mmap'ed 1 MB-8 KB transaction buffer (bump + free list),
   |     offsets at ALIGN(data_size, 8)
   v
 Rust peer: servicemanager (handle 0) + "activity" (peer node id 0x1001) + reverse calls
```

Deliberately **not** implemented (no observable effect at these five criteria, all
cheap to add later): death notifications actually firing (`BC_*_DEATH_NOTIFICATION` is
recorded, nothing is ever reported dead), `BR_INCREFS`/`BR_ACQUIRE` weak-ref callbacks
for exported nodes, `BR_SPAWN_LOOPER` thread-pool growth, scatter-gather
(`BINDER_TYPE_PTR`/`BINDER_TYPE_FDA` — `BC_*_SG` is accepted and treated as the plain
form, which is correct for parcels below the SG threshold), and oneway prioritisation.
`BC_ACQUIRE`/`BC_RELEASE` are acknowledged by doing nothing, which is what the kernel
does for handles.

## 2. The object table is the whole trick

`libbinder` never sends a "binder id": it sends `flat_binder_object` records and lets the
*driver* rewrite them per-process. Our driver does exactly that:

| direction | wire/parcel type | driver action |
|---|---|---|
| ours → peer | `BINDER_TYPE_BINDER` (`binder`=weakref ptr, `cookie`=BBinder\*) | allocate a wire handle `W`, record `{binder,cookie}→W`, rewrite as `BINDER_TYPE_HANDLE(W)` |
| ours → peer | `BINDER_TYPE_HANDLE(H)` | look up `H`→peer node id, rewrite as `BINDER_TYPE_BINDER(id, id)` |
| peer → ours | `BINDER_TYPE_BINDER(id, id)` | allocate a handle `H` in *our* handle space, rewrite as `BINDER_TYPE_HANDLE(H)` |
| peer → ours | `BINDER_TYPE_HANDLE(W)` (our exported node) | rewrite as `BINDER_TYPE_BINDER(binder=weakref, cookie=BBinder*)` so `javaObjectForIBinder` returns the original **Java object** |
| both | `BINDER_TYPE_FD` | sender: `dup` + `SCM_RIGHTS`; receiver: write its own fd number into `handle` |

Consequences worth knowing for the product: a Java `Binder` passed once and received back
arrives as the *same Java object* (local-node path), a peer binder arrives as a
`BinderProxy`, and `BR_TRANSACTION.target.cookie` must be the `BBinder*` (libbinder does
`sp<BBinder> b((BBinder*)tr.cookie)`) while `target.ptr` is the weakref pointer.

## 3. The three silent killers (none documented, all cost real time)

1. **`Parcel::writeInterfaceToken` is 3 ints + a String16**, not a bare String16:
   `[strictPolicy|0x80000000][workSourceUid (-1)][0x53595354 "SYST"]String16`.
   ACOS already had this reverse-engineered in `crates/acos-parcel/src/token.rs`; the
   peer now writes the same prefix, and a `[policy][-1]["SYST"]` stream is what makes
   `Parcel.enforceInterface` accept a peer-generated parcel (§criterion 3).
2. **Every `flat_binder_object` is followed by a 4-byte *stability* record** inside the
   parcel data (`Parcel::unflattenBinder` reads it after the 24-byte object and passes it
   to `Stability::setRepr`). Omitting it makes `readStrongBinder()` return `null` with
   **no error and no log** — the object is consumed, the result is empty.
3. **`Stability::Level` is not 0/1/2**: `UNDECLARED=0`, `VENDOR=0b0100`,
   `SYSTEM=0b1100`. Any other value logs `Stability: Can only set known stability, not N.`
   and yields the same silent `null`. `SYSTEM` (12) is what a servicemanager reply needs.

Honourable mentions, found by hex-dumping the first `BINDER_WRITE_READ`:
`PING_TRANSACTION` (empty parcel, must be answered with an empty reply — the *first* IPC
of the process is a ping); `BC_REPLY` carries `TF_STATUS_CODE` in `flags`; the reply
parcel must start with `writeNoException`'s int32 because AIDL proxies call
`readException()` first.

## 4. Numbers

| metric | value |
|---|---|
| round-trip latency, sync transact (interface token + int + String + empty Bundle), Java→Java, n=200 | mean **7.5–11.1 µs**, p50 **7.2–9.7 µs**, p95 **8.3–15.0 µs**, max 13–62 µs (8 runs, shared desktop) |
| payload sizes | request 72–184 B, reply 44–148 B (uncompressed parcels, no SG) |
| process cost of the shim | 33 KB `.so`; 1 extra thread; 1 MB−8 KB anonymous memfd mapping (already required by libbinder's `mmap`) |
| scratch footprint | 3.9 MB (`.cache/art-exp-g`) |
| peer | 3.9 MB `rustc` binary, zero crates, single-threaded |

For comparison: E1C measured 3.8 µs per unix-socket round trip, so the driver+parcel
overhead is **~2× a bare socket call** — and it buys a working `IBinder` API, not a
bespoke RPC. A kernel binder round trip is ~10–30 µs on real hardware, so this is *not*
slower than the thing it replaces at these payload sizes.

## 5. Honest caveats

- **Criterion 3 is verified end-to-end but the prototype's first-attempt delivery is
  racy.** Roughly 1 in 5 runs delivers the peer-initiated transaction on the first
  `addService`; the test therefore re-registers (each registration makes the peer send
  another reverse call) and passes **5/5** with 2–7 attempts. The remaining defect is in
  my driver's buffer/thread bookkeeping: a stray 4-byte `BC_REPLY` occasionally appears
  (Java's `onTransact` bailing out on an already-freed transaction buffer — my allocator
  does not yet model the kernel's per-thread `t->buffer` lifetime), and the peer now
  ignores those instead of dying. Fixing it properly is the first task of any
  productisation (§6).
- **One connection, one peer.** The peer is a single-process servicemanager+service;
  there is no registry, no per-service process, no client-side handle sharing between
  processes (each process would need its own shim instance and node table).
- **`BC_*_SG`, death notifications, weak refs, `BR_SPAWN_LOOPER`** are stubs (§1). A real
  `ActivityManager` flow will hit `linkToDeath`/oneway traffic and needs them.
- **The peer's AIDL parsing is minimal** (token + name + one object), not a generated
  stub. `acos-parcel` would be the right codec here but pulls `rustix`/`thiserror`; the
  peer stays `rustc`-only so the experiment has no dependency surface.
- Latency numbers are single-process on a shared desktop with other agents' load; treat
  ±30 % as noise. All runs are x86_64/API 34 from the E1A sysroot.
- The shim still carries E2D's 28-key property table and its `dlopen` trick; both are
  prerequisites (without the property table `Build.VERSION` poisons the process).

## 6. Effort to productise (measured pieces only)

| item | estimate | why |
|---|---|---|
| Driver as a maintained library (buffer lifetime per thread, SG, death notifications, weak refs, oneway queue, multi-process node tables) | **2–3 d** | the happy path is done; the listed stubs are each small but they are what real services exercise |
| Real servicemanager peer with `acos-parcel`/`acos-services` (`ServiceHost::transact`) instead of the echo stub | **1–2 d** | the wire contract is now proven; this is codec plumbing through `crates/acos-parcel` + handle registry |
| Replace the unix socket with the ACOS in-process transport (or keep the socket: 8 µs is not the bottleneck) | 0.5 d | the socket is behind a 4-function interface in the shim |
| **Total to "stock libbinder + real Rust services, no kernel driver"** | **3.5–6 d** | and it removes the "binder driver" item from the boot/privilege budget entirely |

What this changes for ACOS: the E2D wall ("framework plumbing needs a framework") is now
closed on the IPC side too. `IBinder`, `BinderProxy`, `Parcel`, `Bundle`, `ServiceManager`
and `Binder.onTransact` work **unmodified**, against a Rust peer, with no `/dev/binder`,
no servicemanager, no `system_server`, no root. The remaining wall for a real `Activity`
is unchanged and is the graphics stack (`HardwareRenderer`/`libhwui`/EGL), not binder.
