# Compositor direction: advisor brief (status as of 2026-11-12 05:18 UTC; Spike A update 05:55)

> **CORRECTION 06:0x UTC: W54 found the real dialog cause** (`b845e8e`). It was **not** C1
> and **not** `destinationFrame`; Spike B's hypothesis is rejected for this bug. The
> trace shows `destination_frame` == the buffer size, crop empty and matrix identity.
> ACOS's **WMS** (the system_server role) ignored `LayoutParams.surfaceInsets`:
> relayout returned a 1024×513 surface where stock returns frame + 2·84 insets
> (1192×681), and placed the layer at the frame rather than at frame − insets.
> The guest's `ThreadedRenderer` therefore drew the card offset by the 84-px inset
> and clipped at the buffer edge. The bug predates W47. Fixed in WMS. Host-pixel
> assertions fail before the fix and pass after; 8 regression scenarios pass.
> **Implication: neither B1 nor A1 would have prevented this bug.** It sits in our
> WMS reimplementation, not the compositor.
>
> **UPDATE 05:55 UTC: Spike A is DONE** (`c3cd99c`, `results/art-runtime/SPIKE-A.md`). §5's
> "Spike A" section below describes the plan; these are the results.
> - The stock `surfaceflinger` **boots headless and stays alive** with the ACOS binder
>   driver + peer, the ACOS memfd gralloc shim and a **226-line fake EGL/GLES**
>   (150 symbols). It stops only at composer3 discovery.
> - Measured: Pss **20.6 MiB = 4.43 MiB private + 16.1 MiB shared file**; 5 threads;
>   < 5 ms to the stable state.
> - Mandatory in Android 14: the composer3 AIDL service, a **primary physical display
>   hotplug** (a virtual display alone is impossible), and vsync from the composer.
>   There is **no CPU RenderEngine** (GL or VK only), and RenderEngine is created
>   before the composer.
> - A fake GL is enough to initialise but can never draw. Client composition, blur,
>   HDR and SurfaceFlinger screenshots would be wrong; screenshots must keep coming
>   from C1.
> - Composer stub: ~40 of 97 `AidlComposer` methods sit on the boot + first-frame path.
>   It is decode-only, **750–1120 LOC with no AIDL codegen**. SurfaceFlinger batches
>   all per-frame geometry into `executeCommands`, and that batch is exactly C1's
>   missing geometry.
> - Remaining custom work: servicemanager `addService` for native processes
>   (100–200 LOC) and wiring (100–200 LOC). **Total ~1,100–1,550 LOC.** C1 still
>   blits the pixels.
> - Memory per session: **6–9 MiB private (estimated from the 4.43 measured floor),
>   plus 16 MiB shared once**. That is inside the owner's 10–20 MiB "preferred" band.
>   N=10 is ~60–90 MiB and N=20 ~120–180 MiB, plus 16 MiB shared, per-session
>   instances (estimate).
> - Proving experiment designed, ~11 h in five steps with kill points: composer boot
>   set, one DEVICE layer, dialog geometry == [28,976][1052,1489], C1 blit parity,
>   N=10 measurement.
> - Spike A's own verdict: memory does not reject stock SurfaceFlinger; effort and
>   correctness decide. It recommends Spike B's path now and reopening
>   SurfaceFlinger only if a composer3 service is funded.

## 1. What ACOS is (relevant parts)
- An agent-first Android APK execution and verification environment. Real ART runs
  inside a bwrap sandbox with a userspace binder emulation. A Rust "ServiceHost"
  plays AMS/ATMS/PMS/WMS.
- **The app renders with stock Android code**: real hwui and Skia (CPU) in the app
  process, drawing into ACOS software buffers (memfd, "acos-gralloc").
- **C1** is ACOS's own Rust compositor in the SurfaceFlinger role. It decodes the
  app's `ISurfaceComposer` transactions (`layer_state_t`), keeps the BLAST buffers,
  decides where each buffer lands, and blits on the CPU into the screenshot.
  - Code: `crates/acos-services/src/surface_client.rs` (3124 lines), `wms.rs`
    (695) and `window_session.rs` (990).
  - Caps: 16 layers, 64 MiB canvas, 256 MiB input. Composites in z-order and
    applies dim.
- Headline metric: M11 = verified tasks per GiB-minute at N=10, currently **67.81**
  (N=1, 2 rounds) against a bar of **24.4**. Today's per-task peak is ~209 MiB
  anon+shmem (job ~117 + guest ~100, T07).
- Engineering policy ladder: unchanged AOSP > AOSP plus configuration > small shim
  > AOSP subset > custom code. "Eliminating SurfaceFlinger is not a goal." A few
  MiB extra is acceptable to stay on AOSP.

## 2. Why this question came up
C1 keeps diverging from stock placement semantics, and each divergence has been
hand-fixed:
- **W42** (fixed, `22f82ed`): after Back, the destroyed Activity's last frame stayed
  on screen.
- **W54** (in progress): on `form-validation`, the "Success" AlertDialog's
  **pixels** are drawn about 126 px too far right and clipped at the right and
  bottom, while the UI tree reports the correct bounds `[28,976][1052,1489]`. The
  scenario **passed 26/26** because its assertions only read the tree, so this is
  a false pass. It appeared after W47 corrected the dialog width from 776 to
  1024 px. The drawn x of ~154 ≈ (1080−776)/2, which suggests stale geometry.
- The owner asked: why not reuse AOSP's SurfaceFlinger instead of matching it
  piece by piece?

## 3. Owner decisions and constraints
Recorded in `docs/decisions/2026-11-12-compositor-spikes.md`.
- Prefer reusing Google's implementation over maintaining C1, but **memory
  efficiency is a primary requirement**.
- Decision rule on the **measured complete incremental cost per session** (graphics
  backend, allocators, buffers and dependent services included):

  | added per session | decision |
  |---|---|
  | 10–20 MiB | use stock SurfaceFlinger (strongly preferred) |
  | 20–50 MiB | show measured tradeoffs to the owner |
  | 50–100 MiB | strong justification plus owner approval |
  | ≥ 100 MiB | rejected as the default architecture |

- Report fixed shared memory separately from per-session memory, with totals at
  N=1, N=10 and N=20.
- **No GPU driver stack, no SwiftShader, no Cuttlefish, and no downloads** unless
  proven necessary and approved. Try our existing allocator, software buffers and
  a minimal headless display backend first.
- C1 keeps working. **C1 is not replaced until the owner approves**, based on
  measured memory, compatibility and integration results.
- One-hour rule per task. Architectural decisions are escalated with evidence.

## 4. Facts established
- The image **does contain** stock `system/bin/surfaceflinger`, with
  libsurfaceflinger and RenderEngine statically linked. An earlier note (W12)
  wrongly said it was absent because it looked for the `.so`.
- The image contains **no vendor HAL implementations**: no composer service, no
  allocator service and no GL/Vulkan driver (SwiftShader). Only the HIDL/AIDL
  interface libraries are present.
- The Android 14 sources for frameworks/native (SurfaceFlinger FrontEnd,
  CompositionEngine, libui, libgui) are in `.cache/aosp`.
- W12 estimated stock SurfaceFlinger at +120–300 MiB per host. That figure was
  **never measured**.

## 5. Spikes: which ran and which did not

### Spike B: reuse SurfaceFlinger's layer-placement logic inside C1 — DONE (05:12 UTC, `1028d10`)
Report: `results/art-runtime/SPIKE-B.md`; evidence in `results/art-runtime/evidence/SPIKE-B/`.
- **Linking the FrontEnd library whole is infeasible.** Its 6 translation units
  pull in Layer/HWC2/HAL/TimeStats/perfetto/renderengine/composer3 AIDL/libbinder.
  Closure: 162 files resolved, **197 headers missing**.
- **The geometry kernel is reusable.** That is `RequestedLayerState`
  transform/crop algebra (~120 LOC), `LayerSnapshotBuilder::updateLayerBounds`
  (~55) and rounded-corner/bounds code (~200), about ~700 LOC with the needed
  pieces. It depends only on `ui::Transform`/`Rect`/`Region`, which Android itself
  builds for the host (`libui_host_common`).
- **Compile probe, run for real.** 4 AOSP files compiled, with 18 stub headers
  (226 lines). The real `Rect.cpp`/`Transform.cpp` were used unmodified. A 3-layer
  hierarchy (root window → BLAST child → dialog) computed its snapshots.
  - Per-layer cost: **70.6 ns**.
  - Memory: **+2.2 kB per session measured** in a reduced model; 30–60 kB per
    session estimated at real struct sizes; 150–400 KiB of shared `.text`
    estimated.
- **Buffers.** Geometry only asks for width/height/bounds. ACOS memfd buffers map
  onto a `{w,h}` descriptor; no GraphicBuffer or EGL is needed.
- **Semantics gaps in C1** (35-row table, §2):
  - Missing: `destinationFrame`, `matrix`, `bufferTransform`,
    `transformToDisplayInverse`, parent-bounds clipping, relative-z, layer stack,
    transparent region, corner/shadow/blur, inherited alpha and opaque flags.
  - Different: crop conflation, alpha inheritance and transaction apply timing (no
    queue, barriers or sync groups).
  - Matching: absolute z and buffer latching.
- **The W54 explanation is a hypothesis.** The probe used **synthetic** geometry
  (a window 466 px wide at 57,640 under a root at 400,200). It showed stock
  rescales the buffer to the destination frame, so the on-screen bounds are
  identical whether the buffer is 776 or 1024 wide, whereas C1 drops
  `destinationFrame` (`surface_client.rs:~2600`) and has no parent clip. This
  matches the W54 symptoms, but **W54's live traces have not confirmed it yet**.
- Code that would be deleted from C1: ~215–290 LOC of placement logic. New glue:
  ~200 LOC, using a cxx bridge.
- One genuine blocker: `ui/RegionHelper.h` is missing from the sparse AOSP
  checkout (region boolean ops). Either fetch that one source file from AOSP git
  (owner approval pending) or go without `transparentRegion`.
- Spike B verdict: do not link FrontEnd; reuse the geometry kernel with the real
  libui.

### Spike A: smallest headless stock SurfaceFlinger — STARTED 05:16 UTC, NOT DONE
Packet: `.cache/spike-a-packet.md`. Results: `results/art-runtime/SPIKE-A.md`, due
by about 06:16 UTC. Questions being answered from source and the binary:
1. Does `SurfaceFlinger::init` require a composer HAL and a primary physical
   display? Can it run with only a virtual display?
2. Is there any CPU RenderEngine in Android 14? Does RenderEngine init need working
   EGL/Vulkan? If every layer is given DEVICE composition, does SurfaceFlinger skip
   all GPU drawing? Would a "null EGL" satisfy init?
3. **Key idea under test:** a tiny composer3 stub (one headless display, DEVICE
   composition for all layers) would receive SurfaceFlinger's per-layer display
   frame, crop, transform, alpha and z. That hands **all of stock SurfaceFlinger's
   placement and transaction semantics** to C1's existing CPU blitter, with no GPU.
4. Allocator/mapper: can ACOS memfd buffers be served by a tiny mapper? Which
   allocations does SurfaceFlinger make itself?
5. Can ACOS's userspace binder adapter serve a native SurfaceFlinger the way it
   serves the app's native libbinder?
6. A table of unavoidable dependencies.
7. **Measured** memory: run the stock binary headless as far as it gets, with
   spike-only stubs in `.cache/`. Fixed shared vs. per-session; totals at N=1, 10
   and 20 against C1. Estimates must be labelled.
8. The smallest experiment that proves it end to end: a real app's dialog buffers
   arrive, the composer stub receives geometry equal to `[28,976][1052,1489]`, and
   C1's blit gives a correct screenshot.
9. Scaling: one SurfaceFlinger per session vs. a shared one, and the isolation
   implications.

### Not run (by owner decision)
- Downloading Cuttlefish HALs or SwiftShader, and any GPU/display driver.

## 6. Work in flight that touches this area
- **W54**: fixing the dialog pixels in C1 by transliterating the stock
  `destinationFrame` and parent-clip algebra, citing AOSP file:line, plus a
  host-pixel assertion that fails before the fix. Ongoing regardless of direction.
- **W50**: making `acos serve` (the agent path) use ART. It currently runs the
  frozen Rust VM. Top priority, independent of the compositor.

## 7. Options on the table
| | Option | AOSP reuse | Per-session memory | Status of evidence |
|---|---|---|---|---|
| B1 | C1 plus the stock geometry kernel (~700 LOC compiled from AOSP, cxx bridge) | geometry algebra + libui | ~30–60 kB (estimated), 2.2 kB (measured, reduced model) | Spike B: compiled and run |
| A1 | stock `surfaceflinger` per session + composer3 DEVICE-composition stub + memfd mapper (+ null EGL if needed), C1 reduced to a blitter | the whole SurfaceFlinger: transactions, hierarchy, latching, geometry, vsync | **unknown**; this is what Spike A measures | Spike A running |
| A2 | one stock SurfaceFlinger shared by N sessions (a display per session) | as A1 | amortised | Isolation concerns for untrusted apps; not studied |
| C | keep hand-matching C1 | none | ~0 | Status quo; produced the W42/W54 false passes |
| D | any option plus a nightly differential oracle against stock SurfaceFlinger | — | ~0 in the product | Needs A1's setup |

## 8. Questions for the advisor
1. Given B1 (cheap, partial reuse) and A1 (full reuse, cost unknown), what
   evidence threshold should decide between them, beyond the owner's memory
   bands?
2. Is "SurfaceFlinger plus a DEVICE-composition composer stub with no GPU" sound in
   Android 14? Any known blockers (RenderEngine init, client-composition fallbacks
   for screenshots or `captureLayers`, protected/secure layers, color management)?
3. Do B1's remaining gaps (transaction apply timing, sync groups, barriers) matter
   for agent verification correctness, or only for animation fidelity?
4. Is A2 (a shared SurfaceFlinger) acceptable under CERTIFIED-WITH-LIMITS
   sandboxing for untrusted APKs?
5. Should B1 be done now as a correctness fix regardless of A1, since it may be
   throwaway if A1 wins?
