# WR — giving the real `ViewRootImpl` a window and a frame clock on the ACOS stack

**Option study for an owner decision. READ-ONLY research: nothing run, `crates/` untouched,
nothing committed. Hard checkpoint 60 min, used ~55.**

Question: what is the *smallest compatible* way to hand the real `ViewRootImpl` a window and a
vsync source, on stock `libbinder` over the ACOS binder peer (`crates/acos-binder-peer` +
`crates/acos-services` Rust `ServiceHost`)?

Short answer: **three small Rust service surfaces (compositor-client stubs, a vsync
`BitTube`, `IWindowSession`) plus one interposed *software gralloc HAL* (~300 lines of C).**
The gralloc mapper — not `IGraphicBufferProducer` — is the wall. With it, every piece above it
(`BLASTBufferQueue`, `BufferQueue`, `Surface`, `libhwui`, Skia) is stock and unmodified, in the
app's own process, and `ViewRootImpl.drawSoftware` paints into memory we can read.

---

## 0. What the real code actually demands, in execution order

Numbers are `android-14.0.0_r75` line references in `.cache/aosp`, cross-checked against the
API-34 image where the image differs from the tree (P4b §3 already recorded one such
difference).

| # | moment | stock code | what it needs from ACOS | today |
|---|---|---|---|---|
| 1 | `new ViewRootImpl(...)` | `ViewRootImpl.java:762` `private final SurfaceSession mSurfaceSession = new SurfaceSession();` | `ISurfaceComposer.createConnection()` → `ISurfaceComposerClient` (`SurfaceComposerClient.cpp:2394–2404`, `onFirstRef`) | **absent** → blocks or nulls |
| 2 | same ctor | `ViewRootImpl.java:1169` `mChoreographer = Choreographer.getInstance()` | `createDisplayEventConnection` → `IDisplayEventConnection.stealReceiveChannel` (2 fds) + `requestNextVsync` | **absent** — this is Q3's SIGSEGV chain |
| 3 | `setView` → `performTraversals` | `ViewRootImpl.java:1476` `mWindowSession.addToDisplayAsUser(...)` | `IWindowSession` node; `IWindow` callback node (can start as a sink) | **absent**; AIDL tables *already* decode it |
| 4 | relayout reply | `ViewRootImpl.java:8929–8975` | out `SurfaceControl`; if invalid → `destroySurface()` and **no drawing at all** (§3 below) | — |
| 5 | `updateBlastSurfaceIfNeeded` | `ViewRootImpl.java:2500–2526` | valid `SurfaceControl` → `new BLASTBufferQueue(...)` → `createSurface()` → `mSurface.transferFrom(blastSurface)` | — |
| 6 | BLAST ctor | `BLASTBufferQueue.cpp:169–214` | `createBufferQueue()` **in-process** + `ComposerServiceAIDL::getComposerService()->getMaxAcquiredBufferCount()` | — |
| 7 | first `lockCanvas` | `ViewRootImpl.java:5364` → `android_view_Surface.cpp:188–241` → `Surface::lock` (`Surface.cpp:2381`) | `connect(API_CPU)`, `dequeueBuffer`, `requestBuffer`, **`GraphicBuffer::lockAsync` → `GraphicBufferMapper`** | **fatal** without gralloc |
| 8 | `unlockAndPost` | `Surface.cpp:2476` | `queueBuffer` (in-process BQ) → BLAST consumer → `Transaction.setBuffer` → `ISurfaceComposer.applyTransaction` | — |

Two facts decide everything, both verified in source this session:

**Fact 1 — `mSurface` only becomes valid through BLAST.** In API 34 the relayout reply's
`Surface` is gone; `updateBlastSurfaceIfNeeded()` (`:2500`) is guarded by
`if (!mSurfaceControl.isValid()) return;` and the `else` branch at `:8974` calls
`destroySurface()`. With an invalid `SurfaceControl`, `drawSoftware` still runs and
`mSurface.lockCanvas()` throws `IllegalArgumentException` (`isSurfaceValid(surface)` false,
`android_view_Surface.cpp:192`) → `Log.e("Could not lock surface")` and no pixels. That is the
P4b §5 "rung 0" exactly: traversal completes, frame does not exist. So **there is no way to
get pixels out of the live path without a valid `SurfaceControl` + a working BLAST BQ.**

**Fact 2 — buffer allocation is gralloc, not `IGraphicBufferProducer`.**
`GraphicBufferAllocator::GraphicBufferAllocator()` (`libs/ui/GraphicBufferAllocator.cpp:51–72`)
ends with

```cpp
LOG_ALWAYS_FATAL_IF(!mAllocator->isLoaded(),
        "Failed to load matching allocator for mapper version %d", mMapper.getMapperVersion());
```

and the mapper version is chosen from gralloc5 (AIDL `IAllocator/default`, *declared* service,
`Gralloc5.cpp:53–60` `AServiceManager_isDeclared`) down to gralloc2/3/4 (HIDL → needs
`/dev/hwbinder` + `hwservicemanager`). ACOS has neither, so the first
`dequeueBuffer`/`requestBuffer` in `BLASTBufferQueue` is `LOG_ALWAYS_FATAL`, and `Surface::lock`
(`:2390` `connect`, `:2400` `dequeueBuffer`) can never return. **A Rust
`IGraphicBufferProducer` served over the ACOS peer does not help**: the producer decides *which*
slot, `GraphicBufferMapper::lockAsync` decides *what address*, and that is gralloc.

Consequence: the honest cheap lever is to **implement the software gralloc HAL** (memfd-backed
`native_handle`s + a mapper that mmaps them) *under* stock `libgui`, not to reimplement
`Surface`, `BufferQueue` or the producer.

---

## 1. Inventory: what we already have (this changes the estimates)

| asset | state | evidence |
|---|---|---|
| API-34 `IWindowSession.addToDisplayAsUser` / `relayout` wire layouts | **decoded and in-tree** (`cond(...)` out-param presence + `ClientWindowFrames`/`MergedConfiguration`/`SurfaceControl`/`InsetsState`/`InsetsSourceControl$Array`/`Bundle`) | `crates/acos-aidl/src/generated/tables.rs:19328–19425` |
| Parcelable codecs for those reply types | **in-tree**, written for the Rust-VM WMS | `crates/acos-services/src/parcelable.rs:1139–1220` |
| peer → `acos-services` typed dispatch | in-tree | `crates/acos-binder-peer/src/peer.rs:606–640` (`lookup` → `codec::read_args` → `ServiceObject::call` → `codec::write_reply`) |
| reverse delivery into a guest `Binder` (peer-initiated transaction) | proven 9/9, 5/5 runs | `results/art-runtime/P2f.md` |
| fd passing both directions (`BINDER_TYPE_FD`, `SCM_RIGHTS`) | proven, incl. receiver-side fd substitution | `results/art-feasibility/E3G.md` §0 rung 4 |
| `IWindowManager` scalars already answered (`getCurrentAnimatorScale` 42, `useBLAST` 5) | **`useBLAST` already returns true** — good, one less branch | `results/art-runtime/P4b.md` §5 |
| `android.gui.*` (native AIDL) tables | **0 entries** — must be generated from `.cache/aosp/native/libs/gui/aidl/` or hand-written | `grep -c android.gui tables.rs` → `0` |
| `INTERFACE_TRANSACTION` (0x5f494e54) on peer nodes | **not handled** (only `PING_TRANSACTION`, `peer.rs:372`) — AIDL `interface_cast` needs it | — |
| stock `libgui`/`libui`/`libhwui` in the sysroot | present | `.cache/art/sysroot/system/lib64/{libgui,libui,libhwui}.so` |
| per-session memory today | 16–21 MiB private (cold), shared file-backed 100–113 MiB host-wide | `results/art-runtime/BASELINE.md` |

The `acos-aidl` tables are the single biggest win: the `IWindowSession` codec is *already
written and tested* for the exact API-34 signatures (`relayout` with `ClientWindowFrames`,
`MergedConfiguration`, `SurfaceControl`, `InsetsState`, `InsetsSourceControl[]`, `Bundle`). The
new work is mostly "answer it from a peer node instead of the VM's WMS", plus checking that
the *real* ART parcel bytes match the codec's expectations (the P4a/P4b lesson: the image is
newer/older than r75 in places).

---

## 2. The four service surfaces (the actual unit of work)

| id | surface | stock caller | wire | new lines |
|---|---|---|---|---|
| **S1** | `android.gui.ISurfaceComposer` + `ISurfaceComposerClient` | `SurfaceComposerClient::onFirstRef`, `BLASTBufferQueue` ctor, `SurfaceControl.Builder`, `Transaction::apply` | `createConnection()`→binder; `createSurface(SurfaceCreationArgs)`→`CreateSurfaceResult{binder handle,…}`; `applyTransaction(TransactionParcel)` (sync reply, oneway=false); `getMaxAcquiredBufferCount()`→i32; `destroySurface`/`clearLayerFrameStats` | 200–280 Rust + generated metadata |
| **S2** | `android.gui.ISurfaceComposer.createDisplayEventConnection` + `IDisplayEventConnection` + BitTube | `DisplayEventReceiver.cpp:36–58` | reply = 1 binder; then `stealReceiveChannel(out BitTube)` = **2 dup'd fds in one parcel** (`BitTube.cpp:117–146` writes *both* ends, `SOCK_SEQPACKET`), `setVsyncRate(i32)`, `requestNextVsync()` oneway; events written by us as `DisplayEventReceiver::Event` structs | 180–260 Rust + 1 timer thread |
| **S3** | `android.view.IWindowSession` + `android.view.IWindow` (+ `InputChannel`) | `ViewRootImpl` `:1476`, `:8900` | codec **exists**; new: handler + `IWindow` sink node + a vsync-paced nothing; `InputChannel` = `socketpair` fds in the `addToDisplayAsUser` reply | 350–500 Rust |
| **S4** | **software gralloc HAL** (interposed): `GraphicBufferAllocator::allocate`, `GraphicBufferMapper::{lock,lockAsync,unlock,unlockAsync,registerBuffer}` | `BufferQueueProducer` (libgui→libui), `Surface::lock`→`GraphicBuffer::lockAsync` | memfd + a hand-built `native_handle_t` (ints: width/height/format/stride/usage/pixel_stride) | 250–400 C |

Notes that matter for the estimate:

* **S1's `applyTransaction` can reply "applied" and drop the ops — that is not a simulation of
  `ViewRootImpl`.** It is the compositor's half, which ACOS genuinely does not have yet. But the
  *parcel contains the buffer fd* (`Transaction::setBuffer` flattens a `GraphicBuffer` with the
  memfd as `BINDER_TYPE_FD`), so the frame can be lifted out of the real transaction — see §5
  task T5. `apply()` at `SurfaceControl.java:2775` is `apply(sync=false)` →
  `Transaction::apply(false, /*oneWay=*/false)`: it **waits for our reply**, so the sink must
  answer promptly or the app thread stalls. `sync=true` (from `reportNextDraw`) needs
  `ITransactionCompletedListener.onTransactionCompleted` callbacks — a *reverse delivery*
  (P2f machinery) — and is deliberately deferred.
* **S2 must be exact, not approximate.** The event layout is
  `Header{uint32 type; PhysicalDisplayId displayId __attribute__((aligned(8))); nsecs_t timestamp}`
  plus a `union{ VSync{uint32 count; VsyncEventData vsyncData} … }`
  (`native/libs/gui/include/gui/DisplayEventReceiver.h:71–115`). Get `VsyncEventData`'s inline
  size wrong and `Choreographer` reads garbage frame times — a wrong-vsync bug that looks like a
  scheduling bug. This is the one place where a 16-byte mistake costs a day.
* **S3 can start as a sink.** For a first frame, `ViewRootImpl` does not need any `IWindow`
  callback: it sizes itself from the `relayout` reply it asked for. `IWindow` only needs to
  *accept* transactions (no reverse calls) until input work starts.

---

## 3. Options, costed

Line counts are for new ACOS code; memory is the *added* resident cost on top of today's
16–21 MiB private / 100–113 MiB shared per host.

### (A) Rust `IWindowSession`/`IWindow` + shmem software surface, **stock BLAST/BufferQueue/libui above a shmem gralloc**

| | |
|---|---|
| **stock code reused** | `BLASTBufferQueue`, `BufferQueue{,Producer,Consumer,Core}`, `Surface`, `SurfaceControl`, `SurfaceComposerClient` (client half), `GraphicBuffer`, `libhwui`+Skia software canvas, `android_view_Surface`, `DisplayEventReceiver`, `DisplayEventDispatcher`, `Choreographer`, `ViewRootImpl` — **zero AOSP forks** |
| **new ACOS code** | S1 200–280 + S2 180–260 + S3 350–500 + S4 250–400 C ≈ **1000–1450 lines** |
| **memory** | 2–3 × 8.9 MB memfd buffers (1080×2062 RGBA) = **~27 MB per session, file-backed** (not private); peer code/data ~4–8 MiB once; 1 vsync thread ~128 KiB |
| **fidelity** | high, with three named gaps: (i) no compositor, so `applyTransaction` is a sink and `finishDrawing`/`reportNextDraw` cannot complete; (ii) `startActivity`-style sync transactions (`sync=true`) unsupported ⇒ `ITransactionCompletedListener` deferred; (iii) input channel exists but nothing is dispatched into it |
| **first test** | T1–T2 alone: build `ViewRootImpl` with `--until activity-resumed`; pass = no `Choreographer` crash, `Dispatching … : 159` still clean, `FrameDisplayEventReceiver` constructed with a live fd, `requestNextVsync` observed in the peer log |
| **kill criterion** | if S1's `createSurface` reply cannot produce a `SurfaceControl` that `isValid()` accepts (handle flattening mismatch), stop and take (A′) — 1 session, not a week |

Addendum — **what a Rust `IGraphicBufferProducer` would have bought: nothing.** It is invoked
by `Surface::connect/dequeue/queue/cancel` only after the mapper is reachable; a peer-side
producer forces `GraphicBuffer` flatten/unflatten across the wire *and* still needs S4. It is
strictly more work than letting stock `BufferQueue` run in-process (which is where BLAST puts it
anyway, `BLASTBufferQueue.cpp:177`).

### (A′) Rust `IWindowSession` + **overridden `android.view.Surface` JNI natives**

Replace `nativeLockCanvas`/`nativeUnlockCanvasAndPost`/`nativeIsValid` with a shmem `Bitmap`
(`android_view_Surface.cpp:188` writes exactly this shape: `Surface::lock`→`canvas.setBuffer`).
Skips S4 entirely.

| | |
|---|---|
| new code | S1 + S2 + S3 + **~120 C** = **850–1150 lines** |
| why not recommended | (i) it *substitutes framework behaviour* — the `Surface`↔compositor contract, dirty-region copy-back (`Surface.cpp:2414–2436`), posted-buffer aliasing — which is the thing this task says not to simulate; (ii) it does **not** avoid S1: `mSurface.transferFrom(blastSurface)` still needs a valid `SurfaceControl`, so you keep the whole BLAST stub surface anyway and only save the allocator; (iii) any app that calls `Surface` directly (video, `SurfaceView` children) diverges silently |
| **role** | emergency fallback if S4 stalls >2 sessions |

### (B) stock `libgui` `BufferQueue` in-process + a **Rust consumer**

**This option dissolves on inspection, and that is a finding.** In the BLAST world the consumer
is *already* in the app process: `BLASTBufferQueue::BLASTBufferQueue` calls
`createBufferQueue(&mProducer, &mConsumer)` (`BLASTBufferQueue.cpp:177`) and wraps it in
`BLASTBufferItemConsumer` (`:184`). `ViewRootImpl` then does
`mSurface.transferFrom(blastSurface)` (`:2524`), i.e. the producer the app draws into is the
local `BBQSurface`. There is no binder hop and **no consumer slot for a Rust side to fill** —
a Rust consumer would have to *replace* `BLASTBufferItemConsumer`, i.e. fork stock `libgui`,
which is the opposite of reuse. Its only useful residue is the *frame-out* mechanism (steal the
queued buffer from the transaction parcel, §5 T5), which belongs inside (A).

| | |
|---|---|
| stock reused | same as (A) minus `SurfaceControl`/`Transaction` |
| new code | U/S1 (still needed — `ViewRootImpl` requires a valid `SurfaceControl` before BLAST) + S4 |
| verdict | **not a separate option**; contributes T5 to (A) |

### (C) stock `SurfaceFlinger` binary in the sandbox

| | |
|---|---|
| what it needs beyond today | composer HAL (`android.hardware.graphics.composer@2.x` HIDL **or** the AIDL `IComposer` over hwbinder) — `HWComposer` is mandatory for `CompositionEngine`/`Scheduler`; gralloc5 allocator + mapper HAL; `RenderEngine` (threaded EGL/GLES — SwiftShader or llvmpipe inside bwrap); `servicemanager` + `addService` reachability for the name `SurfaceFlinger`; `/dev/binder`-equivalent already exists via the LD_PRELOAD driver, so this part is genuinely feasible |
| new ACOS code | HWC HAL stand-in 600–1200 + gralloc HAL (shared with S4, but *as a service*, not interposed) 400–700 + scheduler/vsync glue 200–400 ≈ **1200–2300 lines**, plus EGL stack work |
| memory | a second process, realistically **120–300 MiB RSS** (SF + RenderEngine + buffers) — 6–15× today's per-session cost |
| fidelity | highest in principle, *unverifiable in practice*: with no display and no HWC, "did SF composite correctly?" has no oracle; and it buys nothing `ViewRootImpl` needs that (A) does not already give |
| verdict | **reject now.** Revisit only if multi-window/app-composited screenshots become a product requirement |

### (D) stock `WindowManagerService` / `system_server` components

`system_server` is one process built from ~150 services that assume zygote fork semantics, a
real `servicemanager` with VINTF declarations, `PackageManagerService` scanning `/system` +
APEX (we have the image, so this is at least *possible*), `InputFlinger`, `DisplayManager`
(HWC again), and — unavoidably — `SurfaceFlinger` for every `SurfaceControl` it hands out.

| | |
|---|---|
| new code | effectively "boot Android": zygote+preload, VINTF/servicemanager, HAL set (HWC/gralloc/input/audio stubs), then WMS/ATMS wiring ≈ **5k–15k lines** and the (C) dependency |
| memory | **400–800 MiB** |
| what it would replace | our `ServiceHost` AMS/ATMS/PMS stand-ins, and the P4a/P4b seams (`attachApplication`, `ClientTransaction`, `IContentProvider`) would become real |
| verdict | **reject.** It is the sum of (C) plus everything ACOS deliberately replaced, and it destroys the 16–21 MiB session budget and the 60-minute agent loop |

### Comparison

| | stock reused | new lines | memory | fidelity | risk |
|---|---|---|---|---|---|
| **A** (shmem gralloc + stock BLAST) | all of `libgui`/`libui`/`libhwui` | **1000–1450** | +27 MB file-backed/session | high, 3 named gaps | medium: gralloc handle metadata, vsync struct layout |
| A′ (override `Surface` natives) | `libhwui` only | 850–1150 | +18 MB/session | **substitutes framework behaviour** | low code risk, high fidelity debt |
| B | — | dissolves into A | — | — | — |
| C (SF) | SF itself | 1200–2300 + EGL | +120–300 MiB | unverifiable | high |
| D (`system_server`) | all of Android | 5k–15k | +400–800 MiB | highest | very high |

---

## 4. Vsync specifically: yes, it is a small Rust service

Chain (`native/libs/gui/DisplayEventReceiver.cpp:36–58`):

```
Choreographer.<init>            (ViewRootImpl.java:1169, thread-local)
  → FrameDisplayEventReceiver → android_view_DisplayEventReceiver.nativeInit
  → NativeDisplayEventReceiver::initialize
  → DisplayEventDispatcher::DisplayEventDispatcher
  → DisplayEventReceiver::DisplayEventReceiver
  → ComposerServiceAIDL::getComposerService()          [servicemanager getService("SurfaceFlinger")]
  → ISurfaceComposer.createDisplayEventConnection(VsyncSource, EventRegistration, IBinder layer)
  → IDisplayEventConnection.stealReceiveChannel(out BitTube)
  → DisplayEventDispatcher::initialize → initCheck / setVsyncRate / requestNextVsync
```

What the peer must do, in full:

1. Answer `getService("SurfaceFlinger")` with a peer node whose descriptor is
   `android.gui.ISurfaceComposer` (and answer `INTERFACE_TRANSACTION`).
2. `createDisplayEventConnection` → return a second peer node (`IDisplayEventConnection`).
3. `stealReceiveChannel(out BitTube)` → create `socketpair(AF_UNIX, SOCK_SEQPACKET)`, write
   **both** fds with `writeDupFileDescriptor` (receive first, then send — `BitTube.cpp:117–128`;
   the app dups both, `:130–146`), keep our own dup of the **send** end.
4. `requestNextVsync()` (oneway) / `setVsyncRate(i32)` → record the request; a 60 Hz timer thread
   writes `Event{type=DISPLAY_EVENT_VSYNC, displayId, timestamp}` + `VSync{count, vsyncData}`
   into the send fd. Note `Choreographer` only asks for the *next* vsync, so the timer must be
   request-driven (write only when asked), not a free-running stream — a free-running stream
   makes `FrameDisplayEventReceiver` fire continuously and re-enters `doFrame` hot.
5. Do **not** send `MODE_CHANGED`/`HOTPLUG` unless asked; nothing in this path needs them.

Size: **180–260 lines of Rust**, one thread, no AIDL table generation beyond three methods and
one parcelable. Memory: a 16 KiB socketpair buffer plus one 8 MiB thread stack, i.e. nothing.
Risk concentrated in exactly one place: the `Event`/`VsyncEventData` inline layout (§2).

This is also Q3's fix for defect 2 in a different shape: once `DisplayEventReceiver` *succeeds*,
the `~NativeDisplayEventReceiver` error path that dereferences the null `AndroidRuntime::mJavaVM`
is never entered, so the Q3 SIGSEGV stops being reachable on this path — **but the null
`mJavaVM` remains a latent crash for every other JNI upcall from a native callback**, so Q3 §8
option 4 (start the guest with `AndroidRuntime::start` semantics) still belongs on the list.

---

## 5. Recommendation: (A), phased, cheapest-first

Rationale in one line: **(A) is the only option where the code the owner wants to trust —
`ViewRootImpl`, `Choreographer`, `BLASTBufferQueue`, `Surface`, `libhwui`, Skia — is the real
thing, unmodified, and the only new code implements layers ACOS genuinely lacks (a compositor's
client bookkeeping, a clock, a software allocator).**

Ordered tasks, each ≤45 min, each with a pass/fail you can see in one run:

| # | task | output | pass |
|---|---|---|---|
| T0 | `grep`-free spike: `LD_PRELOAD` a stub `libacoshw.so` exporting `android::GraphicBufferAllocator::allocate` and log it; run a 30-line C program that links `libgui` from the sysroot and allocates a `GraphicBuffer` | proof the interposition *wins* against the image's `libgui`/`libui` (check for `-Bsymbolic`, `protected` visibility) | stub line printed, no `LOG_ALWAYS_FATAL` |
| T1 | S1 minimal: peer node for `android.gui.ISurfaceComposer` with `createConnection` → `ISurfaceComposerClient` node answering `createSurface`/`destroySurface`/`clearLayerFrameStats`; `INTERFACE_TRANSACTION` on all peer nodes | `ViewRootImpl` ctor passes `new SurfaceSession()` | `--until activity-resumed` run shows no `SurfaceFlinger` miss, `ViewRootImpl` constructed |
| T2 | S2: `createDisplayEventConnection` + `IDisplayEventConnection` + BitTube (2 fds) + request-driven vsync timer | `Choreographer` alive | `FrameDisplayEventReceiver` init ok, `requestNextVsync` in peer log, no Q3 SIGSEGV |
| T3 | S3 rung 0: `addToDisplayAsUser` + `relayout` answering **position/size only, null SurfaceControl**, plus `remove`/`setInTouchMode`/`getDisplayFrame`/`onRectangleOnScreenRequested` sinks; `IWindow` sink node | traversal completes without a surface | `Activity.onCreate/onStart/onResume` markers, `performTraversals` runs, `relayout` reply decoded by real ART, `Could not lock surface` **expected** here |
| T4 | S4: shmem gralloc (`allocate` + `lock/lockAsync/unlock` over memfd, real `native_handle` metadata) | `Surface::lock` returns a CPU address | unit C program: allocate → lock → write → unlock; then in-app: `drawSoftware` gets a non-null `Canvas` |
| T5 | valid `SurfaceControl` in the `relayout` reply + `applyTransaction` sink that **reads the buffer fd out of the transaction parcel** and writes a PNG | first frame drawn by the live `ViewRootImpl` | SSIM vs P3c frame ≥ 0.99; `mSurface.isValid()` true; `unlockCanvasAndPost` completes |
| T6 | `InputChannel` (socketpair fds in the `addToDisplayAsUser` reply) + `dispatchPointerEvent` path | M-C's "tap" prerequisite | a synthetic `MotionEvent` reaches the fixture's `OnClickListener` |
| T7 | record structured `UNSUPPORTED_ANDROID_API` for `finishDrawing`/sync transactions instead of silence | honest gap record | `unsupported.jsonl` line, not a hang |

Sequencing rule: **T1+T2 before anything else.** If `ViewRootImpl` cannot even be constructed
with a live `Choreographer`, every later estimate is fiction — and T1+T2 are ~450 lines, the
cheapest falsification available. T3 alone already buys `Activity.onCreate → onResume` on the
real path (P4b's stated "next agent" step (b)), independent of whether T4/T5 land.

Deferred, explicitly: sync transactions and `ITransactionCompletedListener` reverse callbacks
(needed only for `reportNextDraw`/screenshot/animations), `IWindow` callbacks (`resized`,
`moved`, focus), `CaptureDisplay`, and anything requiring a real `Layer` list.

---

## 6. Escalation (owner decision required)

**Problem.** The real `ViewRootImpl` cannot exist without two compositor services it calls
*from its constructor* (`ISurfaceComposer.createConnection` for `SurfaceSession`, and
`createDisplayEventConnection` for `Choreographer`), and it cannot produce a single pixel
without (a) a valid `SurfaceControl` so BLAST builds a `BufferQueue`, and (b) a working gralloc
mapper, because `Surface::lock` = `GraphicBufferMapper::lockAsync` and stock `libui`
`LOG_ALWAYS_FATAL`s when no gralloc HAL is loaded
(`libs/ui/GraphicBufferAllocator.cpp:51–72`). Q3 already showed the price of the missing clock:
a hang or a SIGSEGV at `0x0`, not a graceful degradation.

**Existing AOSP option.** Run the real thing: stock `SurfaceFlinger` (C) and, at the limit,
stock `system_server`/WMS (D). Both are "no new abstractions", and both need an HWC HAL, a
gralloc HAL, an EGL `RenderEngine`, hwbinder or AIDL HALs, and a display. (C) is +120–300 MiB
and 1200–2300 lines of HAL stubs with **no oracle** for correctness; (D) is +400–800 MiB and
5k–15k lines, i.e. booting Android, which is the thing ACOS exists not to do. For vsync alone,
AOSP's own answer is `SurfaceFlinger`'s `EventThread` — 3000+ lines to produce what is
structurally a `socketpair` and a timer.

**ACOS option.** Three Rust peer nodes (S1 compositor-client stubs, S2 vsync `BitTube`, S3
`IWindowSession`/`IWindow`) + one interposed software gralloc HAL (S4). ~1000–1450 new lines;
+27 MB of file-backed buffers per session; every line of `ViewRootImpl`, `Choreographer`,
`BLASTBufferQueue`, `BufferQueue`, `Surface` and `libhwui` stays stock and unmodified. The
`IWindowSession` AIDL codec and the reply parcelables **already exist in the tree**
(`crates/acos-aidl/src/generated/tables.rs:19328–19425`,
`crates/acos-services/src/parcelable.rs:1139–1220`), so this is mostly *answering* code, not
*encoding* code.

**Tradeoff.** (A) accepts three explicit gaps — no compositing (transactions are applied to a
sink, so nothing is ever "on screen", only "posted"), no sync/`finishDrawing` completion
(deferred), and a gralloc whose handle metadata we own (a stride/format mistake shows up as a
skewed or skewed-stride frame, i.e. a *visible, cheap-to-diagnose* failure). In exchange it
keeps the 16–21 MiB session budget, needs no HAL daemon, no hwbinder, no EGL, no display, and
every step has a pass/fail you can read in one `tools/guarded.sh` run. (A′) is ~150–250 lines
cheaper but buys that saving by reimplementing the `Surface` JNI contract — substituting
framework behaviour, which the owner's direction forbids; it is kept only as the fallback if
T0/T4 stalls. (C) and (D) are not cheaper in any currency ACOS is short of *except* "new
concepts", which is the one ACOS has budget for.

**Recommendation.** Approve **(A)**, as seven ≤45-minute tasks (T0–T6 above), with T1+T2 as the
go/no-go gate and T7 as the honesty record. Do not start (C) or (D); do not spend a session on
a Rust `IGraphicBufferProducer` — the source says the producer is already in-process and the
wall is the mapper. Approve the `A′` deviation *only* if T0 or T4 shows the interposition cannot
beat the image's `libgui`/`libui` symbol binding, and record it as a fidelity exception if so.
