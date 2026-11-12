# Owner decision 2026-11-12: compositor architecture feasibility spikes

**Context.** C1 (the Rust SurfaceFlinger-role compositor) keeps diverging from
stock placement semantics (W42 stale frame after Back; W54 dialog drawn at a
stale x offset and clipped while the tree was correct). The stock
`system/bin/surfaceflinger` binary *is* in the image (libsurfaceflinger and
RenderEngine are statically linked into it; W12's "not in the image" checked for
the `.so` only). Vendor HAL implementations (composer, allocator, SwiftShader)
are not in the image.

**Owner priority:** reuse Google's Android components rather than hand-reproduce
SurfaceFlinger behaviour (ENGINEERING_POLICY ladder rung 1 before rung 5).

**Approved:**
1. **Spike B** (now): reuse the Android 14 SurfaceFlinger FrontEnd
   (RequestedLayerState / LayerHierarchy / LayerSnapshotBuilder) and composition
   geometry inside C1. Find the real build dependencies and how much custom C1
   logic it removes. Do not assume extracting geometry functions reproduces
   stock behaviour: layer state, transactions, hierarchy, crop, transforms and
   buffer semantics must be investigated.
2. **Spike A** (next free slot; never interrupt W50/W52/W53/W54): run stock
   `surfaceflinger` in the sandbox. It must accept a real app's buffers,
   compose a dialog correctly and produce a usable screenshot. Measure fixed and
   incremental memory and list unimplemented dependencies.
3. Download of **≤ ~1 GB of official Cuttlefish graphics artifacts** into
   `.cache/` is conditionally approved: first inventory what is missing, download
   only what is needed, verify the source and integrity (official
   ci.android.com / Android build artifacts, recorded URL + sha256), and check
   disk headroom for download plus extraction. No host driver changes and no
   system-wide packages without approval.
4. Each spike ≤ 60 min. Neither expands into an implementation without a report
   and owner approval. **C1 is not replaced until the owner approves the final
   direction.**
5. W54's dialog fix and visual regression tests continue regardless. W50 (ART
   through `acos serve`) stays the priority.

**Amendment (owner, 2026-11-12, later the same morning) — supersedes item 3
and narrows item 2:**
- No full graphics driver stack and no SwiftShader unless it is genuinely
  necessary. **No Cuttlefish download and no GPU/display driver work yet.**
- First priority: reuse Google's SurfaceFlinger layer-placement and composition
  logic with ACOS's **existing software buffers** and minimal dependencies
  (Spike B).
- Spike A becomes an analysis spike: the **smallest headless configuration** of
  stock SurfaceFlinger (virtual display, lightweight allocator, software-only
  composition). Name the exact unavoidable dependencies and the estimated memory
  cost, and design the smallest experiment that would prove it. Do not run
  downloads or build drivers.
- C1 keeps working throughout. Reuse Google's code wherever practical.

**Amendment 2 (owner, 2026-11-12): memory thresholds for stock SurfaceFlinger.**
The default preference is to reuse Google's implementation over maintaining C1,
but memory efficiency is a primary ACOS requirement. Decision rule on the
**measured complete incremental cost per session** (graphics backend,
allocators, buffers and dependent services included):

| added per session | decision |
|---|---|
| ~10–20 MiB | strongly preferred: use stock SurfaceFlinger |
| 20–50 MiB | show the measured tradeoffs to the owner before deciding |
| 50–100 MiB | strong justification plus owner approval required |
| ≥ 100 MiB | rejected as the default architecture |

Report the fixed shared memory separately from the additional per-session
memory, and give totals at N=1, N=10 and N=20. First try stock SurfaceFlinger
with ACOS's existing lightweight allocator, software buffers and a minimal
headless display backend (spike-scope stubs under `.cache/` only). No
SwiftShader or Cuttlefish integration or download unless proven necessary and
approved. Investigate FrontEnd reuse inside C1 in parallel (Spike B). Existing
functionality stays intact. One-hour rule per task; architectural decisions are
escalated with evidence.

**Report required after both spikes:** the AOSP components actually reused,
the custom code that remains, memory and startup cost, compatibility and
correctness, effort and dependencies, and scaling to N=10 and N=20
independently isolated sessions. The recommendation must rest on evidence.
