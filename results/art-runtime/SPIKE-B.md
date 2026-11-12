# SPIKE-B — can C1 reuse Google's SurfaceFlinger FrontEnd + composition geometry?

**Status: done, 1 hard-60-minute box, read-mostly. Probe compiled and ran; the answer is
"the geometry is reusable, the layer is not — and the reusable part is ~700 lines, not
libsurfaceflinger".**

- Owner decision obeyed: `docs/decisions/2026-11-12-compositor-spikes.md` (rung 1 before
  rung 5; do not assume extracted geometry reproduces stock behaviour; C1 keeps working).
- UTC stamps: start `Thu Nov 12 05:04:59 AM UTC 2026`, 45-min checkpoint
  `Thu Nov 12 05:14:32 AM UTC 2026` (host clock; see §7 note), end `Thu Nov 12 05:16:15 AM UTC 2026`. No edits under `crates/`, `scenarios/`, `tools/`; nothing committed; no downloads.
- Everything I built is in `.cache/sb-spike/` (`probe.sh`, `closure.py`,
  `includes/` 18 stubs / 226 lines, `src/probe_main.cpp` 303 lines, `out/geom-probe`
  42 KiB). Evidence in `results/art-runtime/evidence/SPIKE-B/` (7 text files).
- Sources: `.cache/aosp/native/services/surfaceflinger/{FrontEnd,CompositionEngine}`,
  `.cache/aosp/native/{include,libs/ui,libs/gui}` at android-14.0.0_r75.

---

## 0. The one-paragraph answer

Google's FrontEnd is **not a layer you can link**: `LayerSnapshotBuilder.cpp` includes
`Client.h` (→ `Layer.h`), `DisplayHardware/HWC2.h`, `DisplayHardware/Hal.h`,
`TimeStats/TimeStats.h` and `Tracing/TransactionTracing.h`, and
`RequestedLayerState.h` includes `aidl/.../composer3/Composition.h` and
`renderengine/ExternalTexture.h`. The transitive closure of the 6 FrontEnd TUs needs
**197 headers that are not in our checkout** and pulls in libbinder, the HWC HAL and
RenderEngine. What *is* reusable is the **geometry kernel**: ~300 lines of
`RequestedLayerState` + `LayerSnapshotBuilder` whose only real dependency is
`libui`'s `Transform`/`Rect` (the one part of libui that Android itself builds for the
host — `libui_host_common`). I compiled that kernel with host clang++ against the real
`Rect.cpp`+`Transform.cpp` and it reproduced W54's numbers exactly. The right shape is
therefore **rung 4 (a subset of AOSP)**: keep C1, replace its hand-written placement with
the stock transform/bounds algebra, keep the CPU blit.

---

## 1. Reused AOSP components

| Component | Reusable as-is? | Evidence |
|---|---|---|
| `RequestedLayerState::getTransform` / `getUnrotatedBufferSize` / `getBufferSize` / `getCroppedBufferSize` / `getBufferCrop` / `reduce` | **yes**, ~120 LOC, depends only on `ui::Transform`/`ui::Rect`/`ui::Region` (`RequestedLayerState.cpp:347,362,447,471,481,524`) | compiled and ran in the probe |
| `LayerSnapshotBuilder::updateLayerBounds` (transform composition `parent*local`, parent-bounds clipping, `transformedBounds`) | **yes**, ~55 LOC (`LayerSnapshotBuilder.cpp:942-996`) | compiled and ran in the probe |
| `LayerSnapshotBuilder::updateRoundedCorner` (~34 LOC, `:902`) | yes, needs `RoundedCornerState` only | read |
| `LayerSnapshot::isTransformValid` / `invalidTransform` handling | yes, ~20 LOC (`:944-976`) | read |
| `RequestedLayerState::Changes` merge classification + `merge(ResolvedComposerState)` | **conditional**: the `Changes` enum + `merge` body is ~120 LOC of pure `layer_state_t` arithmetic, but `ResolvedComposerState` lives in `TransactionHandler.h` → `LayerLifecycleManager.h` → `TransactionProto.h`/`perfetto` | `06-compile-stop.txt` |
| `TransactionHandler` (queue per applyToken, readiness filters, barrier logic) | **no** — `TransactionHandler.cpp` reaches `layerproto/TransactionProto.h` → perfetto pbzero generated headers (`06-compile-stop.txt`) | |
| `LayerHierarchy` (parent/child, relative-z, offscreen reparent, z sorting) | **partly** — `LayerHierarchy.cpp:35-112` (z comparator + `sortChildrenByZOrder`) is ~80 LOC and self-contained; the traversal half wants `LayerLifecycleManager`/`RequestedLayerState` whole | read |
| `LayerLifecycleManager` (create/destroy, handle→layer, mirror ids) | **no** — `LayerHandle` is a `BBinder` (`LayerHandle.h`), i.e. libbinder | read |
| `CompositionEngine::OutputLayer` geometry (`calculateOutputDisplayFrame`/`SourceCrop`/`OutputRelativeBufferTransform`, `OutputLayer.cpp:240-310`) | **read and copy the formulas, do not link**: the result is written straight into HWC commands (`hwcLayer->setSourceCrop`, `Hwc2::Transform`, `hal::Error`), and the state type lives in `OutputLayerCompositionState` which owns `DisplayColorProfile`, `hwcLayer`, `Output` | `OutputLayer.cpp:432-473` |
| `LayerFECompositionState` (the "what the geometry produced" struct) | no — `compositionengine/LayerFECompositionState.h:21` needs `android/gui/CachingHint.h` (aidl) and `renderengine/ExternalTexture` (`06-compile-stop.txt`) | |

**Reused, in practice, after this spike:** the formulas and the ~700 LOC of FrontEnd
geometry/composition algebra that are expressible over `ui::Transform`/`Rect`/`Region`
alone — compiled *into* C1, not linked from libsurfaceflinger.

---

## 2. Semantics inventory (C1 today vs stock FrontEnd/CompositionEngine)

C1 decode: `crates/acos-services/src/surface_client.rs:2463-2663` (`decode_transaction_state`),
record: `LayerState` `:1450-1505`, `LayerRecord` `:440-479`. Composite:
`capture_composite` `:840-1030`, `placement_of` `:742-767`, `drawn_region` `:1987-2001`.
Stock: `FrontEnd/RequestedLayerState.cpp` (`RLS`), `FrontEnd/LayerSnapshotBuilder.cpp`
(`LSB`), `CompositionEngine/src/OutputLayer.cpp` (`OL`).

| `layer_state_t` field | C1 does | stock does | verdict |
|---|---|---|---|
| `x`,`y` | decoded `:2465-2466`; used only as a fallback (`placement_of` `:760-766`); the real origin comes from WMS `relayout` (`wms.rs:264-266`, `window_session.rs:774-790`) and is inherited up to 8 ancestors (`:742-757`) | `RLS::getTransform:362` builds a `ui::Transform` from `destinationFrame`+buffer size, **or** `requestedTransform` (position+matrix) when no destination frame; `LSB::updateLayerBounds:948` composes it with the parent (`parent*local`) | **differs** — C1 has no transform composition, no destination-frame path, and a heuristic ancestor walk instead of `parentId` |
| `crop` | only via `buffer.crop.or(buffer_crop)` `:1849` → `drawn_region:1987` | `RLS::getCroppedBufferSize:471` intersects buffer size with `crop`; `LSB:988-993` then intersects with **parent bounds** | **differs** — no parent clipping, no separate buffer-vs-window crop |
| `bufferCrop` | folded into the same `crop` (`:1849`, `:2599`) | `RLS::getBufferCrop:481`: intersect with `externalTexture->getBounds()`; kept distinct from `crop` | **differs** (conflated) |
| `destinationFrame` | **decoded and dropped** (`:2600`, `LayerState::destination_frame` never read) | the primary geometry input: `RLS::getTransform:377-397` → `sx = destW/bufW`, `sy = destH/bufH`, `tx/ty = destRect.left/top`, negative origin clamped to 0 | **missing in C1 — this is W54** |
| `matrix` (4 floats) | decoded `:2476-2481`, never applied | folded into `requestedTransform` (`RLS::merge`) | **missing in C1** |
| `bufferTransform` (`ui::Transform` rot/flip) | decoded `:2502`, never applied | `RLS::getUnrotatedBufferSize:351-359` swaps w/h on `ROT_90`; `OL::calculateOutputRelativeBufferTransform:240-283` composes display×layer×buffer | **missing in C1** |
| `transformToDisplayInverse` | decoded `:2503`, never applied | `RLS:355-359` swaps on display `ROT_90`; `OL:264-281` substitutes the inverse display transform | **missing in C1** |
| alpha | from `WindowInfo.alpha` only (`:1514` doc, `:466-468`), applied at blit `:1002` | `LSB:757-759` multiplies down the tree: `snapshot.color.a = parent.color.a * requested.color.a` | **differs** — C1 does not inherit/accumulate; and stock also uses the product for input and shadow |
| `flags & eLayerHidden` | `:1697-1704` → `record.hidden`, excluded from the visible set `:845-849` | `LSB:717-718` `isHiddenByPolicyFromParent = parent…\|\| invalidTransform \|\| requested.isHiddenByPolicy()`; hidden-ness is inherited and re-evaluated | same for the simple case, **differs** for inheritance/invalid-transform |
| `flags & eLayerOpaque` / `isContentOpaque` | not used | `LSB:250,1020` — drives blur/shadow/occlusion and the client-composition decision | **missing in C1** |
| `flags & eLayerSecure` | not used | `OL:306-309` `forceClientComposition` when secure layer on non-secure output | missing (irrelevant for a software blit) |
| `z` | `:324`, sorted `(z, layer_id)` `:866` | `LayerHierarchy.cpp:35-36` `lhs->z < rhs->z`, then relative-parent recursion `:78`, `:242-243` | **same** for absolute z (both ascending, stable tie-break); **differs** for relative z |
| relative z (`relativeParentId`) | not implemented (`:2495` reads `relativeLayerSurfaceControl` and discards it — `_relative_parent`) | `LayerHierarchy.cpp:64,242-243,369-371` — relative children sort against their relative parent, loop detection | **missing in C1** |
| `reparent` | `:2488`, `:1713` — used only for destroy (reparent-to-null removes the layer) | `LayerHierarchy` insert/remove + `Changes::Hierarchy`/`Parent` | differs (C1 has no hierarchy, only an optional ancestor pointer) |
| `layerStack` | decoded `:2470`, never filtered | layers are matched to a display by `layerStackToMirror`/display `layerStack` in the Display/ composition stage | **missing in C1** (harmless today: one display) |
| `color` (4 floats) | decoded and discarded `:2494-2496` | `RLS::getColor:440` (0,0,0,a for buffer layers), `LSB:757` colour × alpha down the tree | missing (solid-colour layers) |
| `cornerRadius` | decoded `:2538`, never applied | `LSB::updateRoundedCorner:902-935` — with parent-corner inheritance and scale compensation | **missing in C1** |
| `shadowRadius` | decoded as `_shadow_radius` `:2567`, dropped | `LSB::updateShadows:1005-1024` + `ShadowSettings` global state | **missing in C1** |
| `backgroundBlurRadius` | decoded `:2539`, never applied | `LSB:856-861` scaled by inherited alpha, gated on `args.supportsBlur` (RenderEngine) | **missing in C1**, and stock needs RenderEngine for it |
| `input info` (`WindowInfo`) | 25 fields → `:1513+`; alpha/flags/dim/layout params used for placement and dim (`:1002`, `:828`) | `LSB:214-250` input bounds = crop/buffer, clamped by parent, `surfaceInset`, touchable region | differs (C1 uses WMS's frame directly, which is correct for its use) |
| `transparentRegion` | size skipped `:2506-2508`, body skipped | subtracted from the bounds: `RLS::reduce:524`, `LSB:997` | missing |
| `surfaceDamageRegion` | skipped | passed to HWC/RenderEngine | missing (no damage-driven blit yet) |
| `dataspace` | decoded `:2510`? (`dataspace: u32` `:1482`), unused at blit | colour space / HDR decisions in `OL` | missing |
| frame rate / FrameTimeline | decoded `:2567-2585`, `FrameTimelineInfo` skipped | `Scheduler/LayerInfo.h`, `RLS::requestedFrameRate` | missing (irrelevant: no vsync consumer) |
| `buffer` (BLAST `BufferData`) | real: fd dup'd, `latest`/`latest_listener` replaced `:1810-1855`; previous buffer released with `ReleaseCallbackId(bufferId, frameNumber)` | `RequestedLayerState::externalTexture` + `willReleaseBufferOnLatch` + `BufferStateLayer` | same for the single-buffer case |

### Transaction semantics

| aspect | C1 | stock | verdict |
|---|---|---|---|
| merge order | applied immediately, in parcel order, per composer (`:1660-1760`) | `TransactionHandler::queueTransaction` `:44` groups by `applyToken`; `merge` folds `Changes` flags; barriers and `applyFilters:127` decide readiness | differs (C1 has no queue, no sync groups, no barriers) |
| apply timing | synchronous inside the transaction; `sync=true` replies at once (C1 §2) | applied at vsync commit; `finishDrawing`/`reportNextDraw` waits on a real commit | differs — the one thing that still forces a synthetic `onTransactionCompleted` |
| BLAST latching | latching = "newest buffer wins", previous released (`tests/surface_client.rs` case 3) | latch at commit with an acquire fence; `willReleaseBufferOnLatch` | same in effect for one buffer per transaction |
| release fence / callback | always `Fence::NO_FENCE`, `onReleaseBuffer(prev)` queued (`:1830-1848`) | release after the *display* consumes the buffer | contract shape same, fence value synthetic (documented C1 §2/G1 §3) |
| hierarchy / clipping | no clipping at all; only hidden layers drop out | `LSB:988-993` bounds ∩ parent bounds (this is what clips a dialog to its parent) | **missing — second half of W54** |
| offscreen layers | not distinguished | `LSB` builds every snapshot; offscreen ones come out of `snapshotEngine`/visibility | missing, harmless at N=1 display |
| layer destroy | reparent-to-null → record removed (`:1705-1720`) | `canBeDestroyed`, `Changes::Destroyed`, handle keeps the id alive | same effect |

---

## 3. Dependency walk (steer: split (a)/(b)/(c))

### (a) Usable as-is — pure `layer_state_t` + `ui::` geometry
`RequestedLayerState.{h,cpp}` geometry part, `LayerSnapshot.{h,cpp}` (`geomLayerTransform`,
`geomLayerBounds`, `transformedBounds`, `RoundedCornerState`, `isTransformValid`),
`LayerSnapshotBuilder::updateLayerBounds/updateRoundedCorner`, `SwapErase.h`,
`LayerHierarchy`'s z comparator. Coding cost ~700 LOC to reuse, 0 to write.
Deps: `ui/Transform.h`, `ui/Rect.h`, `ui/Region.h`, `ui/FloatRect.h`, `ui/Size.h`,
`ui/Rotation.h`, `math/{vec2,vec3,mat4,half}.h`, `utils/{RefBase,Flattenable,TypeHelpers}.h`,
`log/log.h`, `android-base/macros.h`.

### (b) Touches RenderEngine / GL / HWC / binder — cut or stub
| stub | why | LOC |
|---|---|---|
| `renderengine::ExternalTexture` → `{w,h,getBounds()}` | stock only ever asks it for width/height/bounds (`RLS:348,449,470,482`). **Yes, FrontEnd runs fine with a thin buffer descriptor** — a 5-line struct is enough; nothing in the geometry path dereferences GL/EGL | 8 |
| `aidl::…::composer3::Composition::getCompositionType` | `RLS:getCompositionType` is only used for `forceClientComposition`; a `Composition` enum of 5 values suffices | 6 |
| `scheduler::LayerInfo::FrameRate` | `RLS::requestedFrameRate`; never read by geometry | 4 |
| `ftl::Flags` / `ftl::SmallVector` / `ftl::SmallMap` | `Changes` bitset and small containers; `std::bitset`/`std::vector` substitutes | 25 (measured) |
| `gui::LayerState.h` field set (`layer_state_t`) | we **already decode this ourselves** (`surface_client.rs:2463`); the FrontEnd only needs the *fields*, so a POD instead of the real header removes libgui, libbinder and aidl entirely | 0 (reuse C1's decode) |
| `Client.h`/`Layer.h` | `LayerSnapshotBuilder.cpp:23,28` needs them for `LayerCreationArgs` and 2 `Layer::FrameRate` constants; `Layer.h` is ~4 kLOC welded to HWC/RenderEngine/Scheduler | must be cut (blocker) |
| `HWC2.h`, `Hal.h`, `TimeStats`, `TransactionTracing`, `perfetto` | only for logging/tracing in `LayerSnapshotBuilder` | must be cut |
| `LayerHandle` (a `BBinder`) | `LayerLifecycleManager` identity; C1 already has `BinderId` handles | must be cut |
| libbinder / HAL types | not on the geometry path at all — only via the above headers | n/a |

### (c) Our existing buffers → what FrontEnd expects
Stock: `RequestedLayerState::externalTexture` is a `renderengine::ExternalTexture`
(GraphicBuffer + EGL image). Geometry uses **only** `getWidth()`, `getHeight()`,
`getBounds()` (`RLS:348-349,449-450,471-483`, `LSB:934,945`). Our `LatestBuffer`
(`surface_client.rs:460-470`: `fd, width, height, stride, format, frame_number, buffer_id,
producer_id, crop`) already carries everything. So the mapping is
`ExternalTexture{width, height}` fed from `LatestBuffer` — **no GraphicBuffer, no EGL, no
fd** enters the FrontEnd. The fd never leaves C1's blit.

### Android.bp `host_supported` (`05-android-bp-host.txt`)
- `libui-types` (`libs/ui/Android.bp:57`) — `host_supported: true`.
- `libui` (`:106`) — **no** `host_supported`; the host gets only `libui_host_common`
  (`:285` = `Rect.cpp Region.cpp PixelFormat.cpp Transform.cpp`). Exactly the four files I
  needed, and they compile unmodified with host clang → **verified**.
- `libgui` (`libs/gui/Android.bp:284`) — device-only (libbinder). The host-buildable
  pieces are `libguiflags`, `libgui_window_info_static`.
- `libsurfaceflinger*` (`services/surfaceflinger/Android.bp`) — no host variant anywhere.
- `libmath`, `ftl`, `libbase/include`, `liblog`, `renderengine` — **not in the sparse
  checkout at all**, and none can be built host-side from what we have.

---

## 4. The compile probe (core evidence)

Directory `.cache/sb-spike/`, host `clang++ 23.1.3` (also `/usr/bin/g++` present), C++20,
60 GiB RAM / 32 cpus, -O1.

**Tier 1 — full FrontEnd attempt (failed, quantified).** Transitive closure from the 6
FrontEnd TUs: **162 files resolved** from our checkout, **197 headers missing**
(`01-deps-closure.txt`). Distribution: 79 unqualified (`ComposerHal.h`, `HWC2.h`, `Hal.h`,
`EventThread.h`, `Hal.h`, `FatVector.h`, `FloatRect.h`…), 34 `android/*`, 25 `aidl/*`,
14 `ftl/*`, 11 `scheduler/*`, 6 `perfetto/*`, 4 `android-base/*`, 4 `renderengine/*`,
2 `EGL/*`, `hardware/*`, `system/*`, `timestatsproto/*`… Where each TU stops
(`06-compile-stop.txt`):

```
RequestedLayerState.cpp   -> scheduler/Fps.h                                  (absent)
LayerHierarchy.cpp        -> ftl/cast.h (after android-base/borrowed_fd, std::optional)
LayerSnapshotBuilder.cpp  -> ftl/small_map.h                                  (absent)
LayerLifecycleManager.cpp -> aidl/.../composer3/Composition.h                 (generated)
TransactionHandler.cpp    -> perfetto/.../surfaceflinger_transactions_config.pbzero.h
LayerSnapshot.cpp         -> android/gui/CachingHint.h + compositionengine/LayerFECompositionState.h
```

**Conclusion from Tier 1: a straight reuse of FrontEnd is rung 1/2 and is not achievable**
without libftl + libbase + liblog + libmath + aidl codegen + libnativewindow +
renderengine headers + HWC HAL + perfetto + libbinder — i.e. most of the SurfaceFlinger
tree and its generated-code toolchain. Estimated remaining work in the time box if pursued:
~1–2 h just to vendor `ftl` (header-only, ~40 files) and `libbase`/`liblog` headers, then
per-TU surgery on `Layer.h`/`Client.h`, then `RegionHelper.h` (below), then ~15 stubs — with
the real risk that `RequestedLayerState` cannot be separated from `layer_state_t`'s
binder-bearing fields (`sp<IBinder>`, `SurfaceControl`) without rewriting them.

**Tier 2 — geometry kernel (succeeded).** Compiled and **linked and ran**:
`ui/Rect.cpp` + `ui/Transform.cpp` (real AOSP, unmodified, `-ffunction-sections`/`--gc-sections`
to drop the `Region`-taking overload) plus the AOSP geometry bodies copied verbatim into
`src/probe_main.cpp`:

- files compiled: **4** (`probe_main.cpp`, `Rect.cpp`, `Transform.cpp`, plus 18 headers I wrote)
- stubs: **18 headers, 226 lines total** (`04-probe-build.txt`) — `log/log.h` 20,
  `android/rect.h` 9, `android/native_window.h` 3, `system/graphics.h` 12,
  `android-base/{logging,macros,stringprintf,strings,thread_annotations,unique_fd}` 73,
  `math/{vec2,vec3,vec4,mat4,half,HashCombine,TVecHelpers}` 105, `ui/RegionHelper.h` 4.
- residual hack: `Region.cpp` does **not** compile — `ui/RegionHelper.h` (the
  `region_operator<>` boolean-op templates, ~350 LOC upstream) is **absent from the
  sparse checkout**, so region clipping currently comes free only while we ignore
  `transparentRegion`. This is the one genuine AOSP file we would have to supply (or
  `git sparse-checkout add native/libs/ui/include/ui/RegionHelper.h` — no download of
  binaries, but it is a network fetch, which this spike did not do).
- **it runs**: `03-probe-run.txt`, 3-layer hierarchy (root window → BLAST child → dialog
  window + dialog BLAST child), prints `localTransform`, `geomTransform` (parent∘local),
  `croppedBufferSize`, `geomLayerBounds` (parent-clipped), `transformedBounds`.

---

## 5. Would it have prevented C1's real bugs? (W54, run in the probe)

W54: dialog window layer at the frame from WMS `relayout`, a BLAST child carrying a
`destinationFrame`/crop, buffer size changing **776 → 1024** wide. Probe output
(`03-probe-run.txt`), with the window frame 466 wide at 57,640 under a root at 400,200:

| | buffer 776 | buffer 1024 |
|---|---|---|
| `localTransform` sx | 0.6005 | **0.4551** = 466/1024 |
| `geomTransform` tx,ty | 457, 840 | **457, 840 (unchanged)** |
| `croppedBufferSize` | 776×400 | 1024×400 |
| `transformedBounds` | **[457,840,923,1240]** | **[457,840,923,1240] (identical)** |

Stock therefore **rescales the buffer to the destination frame**, so the dialog's on-screen
rectangle is invariant to the buffer width, and the x origin is the *composed* transform
(root 400 + window 57 = 457), not the raw `layer_state_t.x`.

C1 does the opposite: it drops `destinationFrame` (`:2600`), blits at the WMS frame origin,
and draws the buffer at its own size (`drawn_region:1987`, `DrawSpec` at `:998-1002`).
Both W54 symptoms fall out of that one missing step: the 1024-wide buffer covers 1024 px
starting at the *stale* x (the smaller 776-wide frame no longer matches), and everything
past the window frame is **not clipped** because C1 has no parent-bounds clip
(`LSB:988-993`). W42's stale frame is the sibling gap: `hidden` is honoured (`:1697`) but
`invalidTransform`/`isHiddenByPolicyFromParent` and hierarchy-driven removal are not.

**So: yes — LayerSnapshotBuilder would have computed the correct answer for both, and the
numbers above are the proof, not an assertion.**

---

## 6. How it would integrate

- **Boundary:** `cxx` bridge (`crates/acos-services/build.rs` + `bridge.rs`), one call:
  `apply(transaction_state, &mut FfiLayerStore) -> Vec<FfiSnapshot{id, geom[9], bounds[4],
  cropped[4], alpha, hidden, z, parent, buffer_crop[4], rounded[5]}>`. Input is C1's
  existing `LayerState` (`:1450`, already decoded byte-exactly) marshalled into the POD
  `layer_state_t` subset — **decode stays in Rust, no libbinder**.
- **Ownership:** snapshots are rebuilt per transaction into a Rust-owned `Vec` reused
  across frames (no per-frame allocation after warmup). Buffers never cross the boundary:
  C1 keeps the fd/`LatestBuffer`; the bridge passes only `(w,h)` for
  `externalTexture`. Release callbacks stay in Rust (`:1830`), because they must fire when
  *our* blit has consumed the pixels, not when a display did.
- **Threading:** the FrontEnd state is not internally synchronised; it would live behind
  C1's existing `&mut SurfaceState` (single-threaded transaction applier). No new locks.
- **Measured cost** (`--bench`, 20 in-process "sessions"):
  - per transaction, 3 layers: **211.8 ns** (70.6 ns/layer) for snapshot geometry. That is
    ~4 orders of magnitude below a single 1080p memcpy blit → geometry is free; the blit
    remains the cost.
  - **fixed shared**: probe binary 42 KiB on disk / 22 KiB `.text`; `Pss` baseline 334 kB,
    `Private_Dirty` 260 kB. Scaled to the real subset (~700 LOC + `libui`'s
    `Rect/Transform/Region` ≈ 8–15 kLOC compiled in) I estimate **~150–400 kB of shared
    `.text` per process** if statically linked into C1, i.e. shared page-cache resident
    across N sessions.
  - **additional per session**: measured +44 kB `Private_Dirty` for 20 sessions × 12 layers
    in the reduced model (**2.2 kB/session**); the real `RequestedLayerState` +
    `LayerSnapshot` are ~10–20× larger than my reduced structs (`sizeof(Snapshot)` = 260 B
    here vs. the real one carrying `Region`, vectors and `gui` state), so **estimate
    30–60 kB per session for a ~12-layer window**, labelled estimate.
  - Startup: no threads, no HAL, no fd, no GL — the only cost is `dlopen`-free static init
    of `libui`'s globals (`Rect::INVALID_RECT`, `Transform` constants), sub-millisecond.

### C1 code that would be deleted, and what remains

| delete | file:lines | LOC |
|---|---|---|
| `LayerPlacement` struct + `placement_of` ancestor walk + `set_layer_placement` | `surface_client.rs:302-313`, `:742-767`, `:1060-1068` | ~60 |
| canvas bounds / min-max / sort / pass-through special case | `surface_client.rs:841-935` | ~95 |
| `drawn_region` (buffer-crop guessing) | `surface_client.rs:1987-2001` | 15 |
| WMS-side placement publication | `wms.rs:250-275`, `window_session.rs:771-790` | ~45 |
| **total** | | **~215-290** (the "which pixels go where" decision) |

**Remains custom (correctly):** `decode_transaction_state` (`:2298-2663`, ~365 LOC — the
image's wire shapes, C1 §3/§4), the buffer store + release callbacks (`:1760-1860`, ~100),
the blit + `dim_canvas` + `DrawSpec` (`:960-1030`, ~70), the snapshot->blit adapter
(~60), and the FFI glue (~120). Net: C1 grows by ~150 LOC of glue but loses ~250 LOC of
*hand-derived placement semantics* — i.e. the part that keeps being wrong.

---

## 7. Scaling to N=10 / N=20 isolated sessions

Per-session, per-process (no sharing between sessions; each C1 owns its FrontEnd state):

| | today (C1) | with FrontEnd subset |
|---|---|---|
| geometry state, 12 layers | ~0 (flat `LayerPlacement` per layer) | est. **30–60 kB** anon |
| fixed code (shared, page-cache) | — | est. **150–400 kB `.text`**, shared across all N |
| per-transaction CPU | ~0 (no geometry) | **+212 ns / 3 layers** |
| live frame buffers | unchanged (the real cost: 3–8 MB per 1080p buffer) | unchanged |

N=10: +0.3–0.6 MB anon total, +1.5–4 MB shared once. N=20: +0.6–1.2 MB anon,
same shared. Against a 280 MiB fixed baseline and multi-MB buffer working sets this is
**noise (<0.5 %)**; the metric (verified tasks per GiB-minute) cannot move on memory either
way. What moves is correctness, which the rewrite gate exempts from the thresholds
(`ENGINEERING_POLICY.md`, "Correctness and compatibility fixes are exempt").

Clock note: the host clock advanced 11m16s during this spike (`05:04:59` → `05:16:15`) while
`date`-stamped compile/bench runs took the bulk of the wall time; the box was closed on
evidence, not on elapsed-time pressure.

---

## 8. Minimal-dependency reuse plan (steer request)

**Smallest set of AOSP files that must be vendored/compiled into C1:**

1. `native/libs/ui/{Transform.cpp, Rect.cpp, Region.cpp}` + headers `ui/{Transform,Rect,
   Region,FloatRect,Size,Point,Rotation,LayerStack}.h`, `ui/FatVector.h` — 4 `.cpp`,
   ~750 LOC, and the two of them I compiled needed **zero edits**.
2. `native/libs/ui/include/ui/RegionHelper.h` — **the one missing file** (region boolean
   ops, used for `transparentRegion` and clipping). Either add it via
   `git -C .cache/aosp sparse-checkout add native/libs/ui/include/ui/RegionHelper.h`
   (a fetch — needs owner approval under the spike rules) or drop `transparentRegion`
   support (rung 4 subset; it is empty for every app layer we have seen).
3. The geometry bodies from `FrontEnd/RequestedLayerState.cpp:347-500,524` and
   `LayerSnapshotBuilder.cpp:902-1035,942-996` — ~300 LOC, kept as a vendored file with
   the AOSP header and line provenance (Apache-2.0, see §9).
4. `math` (`vec2/vec3/vec4/mat4/half`): **write the 5 tiny headers** (105 lines measured)
   instead of vendoring libmath's ~3 kLOC of templates — `ui::Transform` uses only
   `mat4` for `asMatrix4()`.
5. `ftl/flags.h` → `std::bitset` (25 lines); no `ftl/small_*`.
6. `android-base/*`, `log/log.h`, `android/rect.h`, `system/graphics.h` → the 18 stubs
   already written (226 lines).

**Stubs each justified:** `ExternalTexture` (geometry reads only w/h/bounds — libguarded
out), `Composition` enum (only `forceClientComposition`), `scheduler::LayerInfo::FrameRate`
(never read by geometry), `LayerHandle`/`Client.h`/`Layer.h` (binder + HWC + RenderEngine),
HWC2/Hal/TimeStats/Tracing/perfetto (logging only).

**Custom glue: ~200 LOC** (POD `layer_state_t`, hierarchy map, snapshot→blit adapter,
cxx bridge) vs. **~250 LOC of C1 placement logic deleted** and ~700 LOC of AOSP geometry
reused instead of hand-written.

**Smallest experiment that proves geometry parity on W54:** the probe already built —
`.cache/sb-spike/src/probe_main.cpp` (303 lines, `out/geom-probe`). One extension (≈40
lines, outside the time box) makes it a regression test: feed it a recorded C1 transaction
dump (`ACOSC1_TRACE=1`, `ACOS_W5_LS=1` already print the values at `:1628`, `:2467`) and
assert `transformedBounds == the dialog's WMS frame`. That is the smallest honest proof and
it runs in microseconds without a device.

## 9. Licence

Apache 2.0 throughout (`MODULE_LICENSE_APACHE2`, per-file headers). Compatible with C1's
licence and with vendoring unter the "Source is for understanding" caveat in
`docs/AOSP_REFERENCE.md` — vendoring *is* a change of that policy from "read only" to
"subset compiled in" (rung 4), which the owner has explicitly opened for
investigation; keep the AOSP headers and record the tag (`android-14.0.0_r75`) in the
vendor directory.

---

## Verdict

1. **Do not link FrontEnd.** It is welded to `Layer.h`, HWC2, RenderEngine, libbinder and
   perfetto; 197 missing headers, no host variant declared in `Android.bp`. Tier-1 evidence
   is decisive and cheap.
2. **Do reuse the geometry kernel** (~700 LOC of `RequestedLayerState` + `LayerSnapshot`
   + `LayerSnapshotBuilder` bounds/transform algebra) compiled into C1 with the real
   `libui` `Transform`/`Rect` and a `{w,h}` buffer descriptor. It is rung 4, removes
   ~250 LOC of C1's most bug-prone logic, costs ~212 ns per transaction and 30–60 kB per
   session, and it *is* what produced the correct W54 answer in the probe.
3. **One blocker, one decision needed:** `ui/RegionHelper.h` is absent from the sparse
   checkout (region clipping). Either approve that single-file sparse-checkout add, or
   accept rung-4 without `transparentRegion`.
4. **Recommendation:** approve a follow-up (≤60 min, separate box) that lands
   `destinationFrame`+`bufferTransform`+`matrix`+parent clipping from the stock formulas
   behind the existing FFI shape — the W54 fix and the W42 fix both live in that one
   transform, and visual regression tests already exist. That is smaller than the FFI
   build-out and clears the rewrite gate on correctness grounds.
