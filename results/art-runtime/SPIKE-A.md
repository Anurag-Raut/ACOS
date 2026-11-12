# SPIKE-A — the smallest headless configuration of stock SurfaceFlinger

**Status: analysis spike, one 60-minute box. Measured result: stock `surfaceflinger` from the
API-34 image boots inside the ACOS sandbox up to the composer HAL and then stops — and it gets
that far only with two spike-scope stubs: ACOS's existing binder driver + software gralloc shim,
plus a 226-line fake EGL/GLES. At that point it is a live 5-thread process at **Pss 20.6 MiB
(4.4 MiB private anon + 16.1 MiB shared file)** — i.e. the memory question the owner asked is
answerable, and the answer is favourable. What is *not* small is the missing composer3 service
(~750–1120 LOC) and the fact that a synthetic RenderEngine can never draw, so C1 would remain the
blitter. Verdict: the memory thresholds do not rule stock SurfaceFlinger out; the effort and the
correctness ceiling do — take Spike B's rung-4 geometry kernel, and revisit SF only if the composer
stub is funded on its own merits.**

- Owner decisions obeyed: `docs/decisions/2026-11-12-compositor-spikes.md` + **Amendment** (no
  downloads, no Cuttlefish, no GPU/display driver work, no SwiftShader) + **Amendment 2**
  (memory thresholds; fixed-shared reported separately; N=1/10/20).
- UTC stamps: start **Thu Nov 12 05:17:11 AM UTC 2026**; evidence closed **Thu Nov 12
  05:47:18 AM UTC 2026**; report finished **Thu Nov 12 05:48:00 AM UTC 2026** (total ≈ 31 min). The 45-minute
  checkpoint (06:02) was not reached: the box closed on evidence at ~30 minutes, not on elapsed-time
  pressure.
- Wrote only `results/art-runtime/SPIKE-A.md`, `results/art-runtime/evidence/SPIKE-A/` (text) and
  `.cache/sa-spike/`. **No** edits under `crates/`, `scenarios/`, `tools/`; nothing staged or
  committed; **no downloads**; every guest run through
  `tools/art-runtime/run-exclusive.sh --task SA -- tools/guarded.sh --mem 2G --time … -- …`.
- Read first: `results/art-runtime/SPIKE-B.md` (FrontEnd dependency closure, C1's semantic gaps),
  `C1.md`, `W12.md` §5, `WR-window-surface-options.md`, `ENGINEERING_POLICY.md`,
  `AOSP_REFERENCE.md`. Source at `android-14.0.0_r75` (`.cache/aosp/`), binary
  `.cache/art/sysroot/system/bin/surfaceflinger` (BuildID `3d77e6b5…`, 8 997 192 B, PIE, stripped,
  "for Android 34"; `libsurfaceflinger`/`RenderEngine` statically linked in).

---

## 0. The one-paragraph answer

`SurfaceFlinger::init()` is GPU-shaped in Android 14: it creates a `RenderEngine` **before** it
creates the composer HAL (`SurfaceFlinger.cpp:834-843` vs `:854-856`), and the only RenderEngine
types are SkiaGL (EGL) and SkiaVK (Vulkan) (`:795-820`). The stock `libEGL.so` in the sysroot is
ANGLE's loader with no driver to load, so without help SF aborts inside `libEGL` ~35 ms in
(measured). With a **226-line fake EGL+GLES** (150 exported symbols, all drawing a no-op, only the
capability queries answered) the abort disappears: SF constructs RenderEngine, spawns its thread,
and stays alive and stable — **Pss 20.56 MiB = 4.43 anon + 16.14 file, 5 threads, 76 fds,
465 mappings, reached in < 5 ms** — waiting on the one thing that genuinely does not exist: a
composer3 AIDL service that hotplugs a headless display. That stub is the whole remaining
dependency: SF batches every per-frame geometry command into a single
`executeCommands(DisplayCommand[])` transaction (`AidlComposerHal.cpp:1124-1160`), so a decode-only
stub hands us exactly the geometry C1 is missing, with the buffer handle attached. But the same
stub also means RenderEngine never draws: no client composition, no screenshots, no blur, no HDR
tone-mapping — so C1 stays the blitter either way. The honest trade is therefore **1 000–1 700 LOC
of new service + a synthetic GL** versus **~700 LOC of vendored FrontEnd geometry inside C1**.

---

## 1. How far the stock binary gets (measured, step by step)

Harness (in `.cache/sa-spike/`, reproduced in the evidence index): bwrap mini-Android root from
`.cache/art/sysroot` (`system` → `/system`, `system/apex` → `/apex`, generated `/linkerconfig`,
`/data` for the shims and the peer socket), the Rust `acos-binder-peer` on a unix socket, the ACOS
binder driver and gralloc shim `LD_PRELOAD`ed, a `smaps_rollup` sampler at 5 ms against the real
host pid.

| # | configuration | rc | where it stopped | log |
|---|---|---:|---|---|
| 0 | `/system` bound, `/apex` left empty | 1 | `bwrap: execvp /system/bin/surfaceflinger: No such file or directory` — `system/bin/linker64` **and** `system/lib64/libc.so` are symlinks into `/apex/com.android.runtime`, so the APEX bind is mandatory, not cosmetic | — |
| 1 | + `/apex`, no shims at all | 134 | `Binder driver could not be opened. Terminating.` (libc `abort`, from `ProcessState::self()`, `main_surfaceflinger.cpp:119`) | `01-sf-without-binder-driver.log` |
| 2 | + ACOS binder driver + `acos-binder-peer` | 134 | **binder transport works** (peer: `driver connected`); `init()` runs and dies inside `libEGL.so`: `couldn't find an OpenGL ES implementation, make sure one of persist.graphics.egl, ro.hardware.egl and ro.board.platform is set` → SIGABRT | `02-sf-binder-gralloc-egl-abort.log`, `02-peer.log` |
| 3 | + ACOS software gralloc shim (`libacoshw.so`) | 134 | identical wall; gralloc ctor proven live: `ACOSHWC[gralloc]: software gralloc shim loaded (1 fd / 9 ints)` | `02-…-egl-abort.log` |
| 4 | + fake EGL/GLES bound over `/system/lib64/lib{EGL,GLESv2,GLESv1_CM}.so` | **alive** | **RenderEngine constructed, process stable for the full 2.5 s window**; stops at composer discovery (zero unsupported transactions at the peer) | `05-fakegl-*` |

Measured memory, same sampler, same host:

| point | t | Pss | Pss_Anon | Pss_File | Private_Dirty | RSS | threads | fds | maps |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| step 2/3 (stock libEGL, ~40 ms in) | 5–30 ms | 19 496–21 144 kB | 4 564–5 460 | 14 924–16 092 | 4 564–5 960 | 19 676–24 592 | — | — | — |
| **step 4 (fake GL, stable)** | 5 ms → 2 495 ms | **20 564–20 660 kB** | **4 428–4 444** | **16 136–16 216** | 4 428–4 480 | 20 716 | **5** | **76** | **465** |

Threads at 1.5 s (`05-fakegl-samples-and-threads.txt`): main `surfaceflinger` (in userspace,
syscall state `running`), the driver reader in `recvmsg` on the peer socket, `binder:…_1` in
`futex`, one more SF thread in `futex`, and a **`RenderEngine` thread** in `futex`. The peer's
`unsupported.jsonl` is **empty**, i.e. every request SF made was one the ACOS service table could
answer (`isDeclared`/`getService` are supported; unknown ones would be recorded). SF then makes no
further progress and never reaches display configuration. **The exact blocking call is the one thing
this box did not pin down**: either SF stalls right after the composer `isDeclared` was answered false,
or it waits on a supported call whose reply the peer did not complete. It does not change any
conclusion — the composer3 node is absent either way, and it is what the stub
must supply.

---

## 2. Boot requirements (`SurfaceFlinger::init()` and what it needs)

`main_surfaceflinger.cpp`: `ProcessState::self()` + `startThreadPool()` (`:119-120`) →
`createSurfaceFlinger()` (`:130`) → `init()` (`:144`) → `addService("SurfaceFlinger")` (`:147-149`) →
`addService("SurfaceFlingerAIDL")` (`:152-157`) → `startDisplayService()` (`:159`) → `run()` (`:166`).
Inside `init()` (`SurfaceFlinger.cpp:821-941`), in order:

1. `chooseRenderEngineType()` + `RenderEngine::create()` (`:834-840`), dereferenced two lines later
   (`:843 getMaxTextureSize`) — **first**, before any HAL.
2. `setHwComposer(createHWComposer(…))` + `setCallback(*this)` (`:854-856`) → loads composer
   capabilities, then `registerCallback` (`HWComposer.cpp:104-117`).
3. `LOG_ALWAYS_FATAL_IF(!configureLocked(), "Initial display configuration failed: HWC did not
   hotplug")` (`:866-867`); `configureLocked()` (`:753-769`) drains `mPendingHotplugEvents` and
   returns `!events.empty()` — **the composer must have hotplugged a display during step 2**.
4. `LOG_ALWAYS_FATAL_IF(!display, "Failed to configure the primary display")` (`:881`) and
   `…"Primary display is disconnected"` (`:884-885`); `HWComposer::getPrimaryDisplayId()` itself is
   `LOG_ALWAYS_FATAL_IF(!mPrimaryHwcDisplayId, "Missing HWC primary display")`
   (`HWComposer.h:475-476`).
5. `initScheduler(display)` (`:888`, vsync source) → `initializeDisplays()` → `mPowerAdvisor->init()`
   → `primeCache()` (`:927-936`, **real shader compilation, default ON** via
   `service.sf.prime_shader_cache`).

| requirement | verdict | evidence | smallest stand-in |
|---|---|---|---|
| `/dev/binder` + servicemanager | **mandatory** | measured (step 1): aborts at `ProcessState::self()`; needs `addService` ×2 (`main:147,156`) | **exists** — `crates/acos-binder-peer/native/binder_driver.c` + `acos-services::ServiceHost` (measured working for SF) |
| composer3 AIDL `IComposer/default` | **mandatory** | `Composer::create` prefers AIDL, else HIDL 2.x (`ComposerHal.cpp:28-34`); `AidlComposer::isDeclared` = `AServiceManager_isDeclared` (`AidlComposerHal.cpp:234-236`), then `waitForService` + `createClient` or `LOG_ALWAYS_FATAL("Failed to get AIDL composer service")` (`:238-250`) | one AIDL service (2 nodes + callback) that hotplugs a display and answers DEVICE for every layer — §4, **750–1120 LOC** |
| HIDL `composer@2.x` fallback | **not viable** | needs `/dev/hwbinder` + a hwservicemanager *and* a HIDL composer implementation; the sysroot has only the client `.so`s (measured: after composer3 was absent, SF never issued a request and stopped) | declare composer3 instead |
| a **primary physical** display hotplug | **mandatory** | `:866`, `:881`, `:884`, `HWComposer.h:475` | stub calls `onHotplug(hwcDisplayId=1, CONNECTED)` and answers `getDisplayConfigs/getActiveConfig/getDisplayAttribute` for a synthetic mode |
| virtual-display-only operation | **impossible in 14** | the primary-display fatals are unconditional; `debug.sf.enable_hwc_vds` only routes virtual displays *through* the HAL, and VDS still needs the physical primary and per-VD output buffers (`VirtualDisplaySurface`/`FramebufferSurface`) | none |
| vsync source | **mandatory** | A14 SF has no software vsync source: `Scheduler/VSyncReactor.cpp` is the HWC-fed reactor and the scheduler's `VsyncSchedule` is driven by the composer callback | one 60 Hz timer thread in the stub emitting `onVsync` |
| servicemanager itself | **mandatory** | `addService` twice (`main:147,156`); peer already implements `getService`/`isDeclared`/`addService` | **exists** + native-node registration (§6) |
| hwservicemanager | optional | only the HIDL fallback, configstore, displayservice | omit |
| power HAL (`android.hardware.power-V4`) | **optional** | `PowerAdvisor` lazy-inits `PowerHalController` (`PowerAdvisor.cpp:595-598`), gates on `setMode` (`:123`) and `getHintSessionPreferredRate().isOk()` (`:202`); absence is logged | omit |
| display config HAL (`android.frameworks.displayservice@1.0`) | optional (**not verified in source**) | only `startDisplayService()` (`main:159`), after SF is registered; composition does not depend on it | omit |
| inputflinger (`IInputFlinger`) | **optional at boot** | `bootFinished()` does `waitForService("inputflinger")` and only logs `"Failed to link to input service"` when null (`SurfaceFlinger.cpp:757-762`) — after the first frame | omit |
| `ro.surface_flinger.*` / `debug.sf.*` props | optional but **behaviour-changing** | 60+ names in the binary, read with defaults; the image has no property area | **partly exists** (driver `kProps` table, `binder_driver.c:120-136`); add 3 keys |

Three properties must be *set* for a coherent headless boot (all currently read as empty ⇒ default):
`service.sf.prime_shader_cache=false` (otherwise real shader compilation at the end of `init()`),
`ro.surface_flinger.supports_background_blur=false` (blur is the one thing that forces client
composition for real content), `debug.sf.enable_hwc_vds=false`.

---

## 3. RenderEngine: no CPU type, but a null EGL is enough for initialisation

- **Types:** `GraphicsApi::GL` and `GraphicsApi::VK`, each ± `Threaded`, selected by
  `debug.renderengine.backend` ∈ {`skiagl`, `skiaglthreaded`, `skiavk`, `skiavkthreaded`}, else by
  the `vulkan_renderengine()` flag + `canSupport(VK)` (`SurfaceFlinger.cpp:795-820`). **No
  CPU/software RenderEngine exists in Android 14**; binary strings agree (`RenderEngine with
  SkiaGL Backend`, `…SkiaVK Backend`, `Unrecognized RenderEngineType %s; ignoring!`).
- **`create()` is unconditional at init** and the result is used immediately (`:834-843`). There is
  no "no RenderEngine" configuration.
- **EGL calls at init** (from the binary's imports: **25 `egl*`**, **110 `gl*`**):
  `eglGetDisplay` → `eglInitialize` → `eglQueryString` → `eglGetConfigs`/`eglChooseConfig`/
  `eglGetConfigAttrib` → `eglCreatePbufferSurface` → `eglCreateContext` → `eglMakeCurrent`, then
  Skia builds Ganesh's GL interface through `eglGetProcAddress`, and the buffer/fence path uses
  `eglCreateImageKHR`, `eglCreateSyncKHR`, `eglClientWaitSyncKHR`, `eglDupNativeFenceFDANDROID`,
  `eglWaitSyncKHR`. Note `eglCreatePbufferSurface`: RenderEngine is **already offscreen** even in
  production — no window system surface is involved.
- **Measured (this is the surprise): a null EGL/GL *does* work — for initialisation.** A 226-line
  stub (`05-fakegl-stub-source.c`, 150 exported symbols: egl configuration/pbuffer/context all
  succeed; `glGetString`/`glGetIntegerv`/`glGetShaderPrecisionFormat` answer plausible ES 3.2
  values; every drawing call is a no-op; `eglGetProcAddress` returns a generic stub) is enough for
  `RenderEngine::create` to succeed and for SF to continue. The init path was the *only* real
  requirement — nothing draws during it (the fake would have failed otherwise).
- **Does DEVICE-everywhere avoid RenderEngine drawing?** Yes: `CompositionEngine::Output` draws only
  when `usesClientComposition` is set (`Output::updateCompositionState`/`presentFrame`), which is
  decided by the composer's validate results. With DEVICE for every layer, no `drawLayers` call
  happens; the remaining RenderEngine consumers are `primeCache()` (property-disableable),
  screenshots (`captureDisplay`), background blur, and colour transforms/HDR tone mapping.
- **What the fake GL costs us:** those consumers become silent nonsense instead of a labelled gap.
  In particular **ACOS's own screenshot/verification path** would need C1's blit anyway, and any
  accidental client-composition request would render garbage rather than fail. `ENGINEERING_POLICY`
  ("no fake success and no silent fallback") makes this a correctness decision, not a technical one.
  The stub is small (~230 LOC, measured), so the cost is *honesty*, not effort.

---

## 4. Composer: the exact call surface, and why the stub is decode-only

`AidlComposer` buffers per-layer commands in a `ComposerClientWriter` and flushes them as **one
parcelable batch** per `execute()` (`AidlComposerHal.cpp:1124-1160`):
`IComposerClient::executeCommands(DisplayCommand[]) → CommandResultPayload[]`, with validate/present
results parsed by `ComposerClientReader`. A stub therefore does **not** implement 40 verbs as
separate transactions: it decodes one `DisplayCommand` union per frame — `setLayerBuffer` (slot,
`native_handle` with the memfd, acquire fence), `setLayerDisplayFrame`, `setLayerSourceCrop`,
`setLayerTransform`, `setLayerPlaneAlpha`, `setLayerZOrder`, `setLayerCompositionType`,
`setLayerDataspace`, `setLayerColor`, `setLayerBlendMode`, `setLayerSurfaceDamage`,
`setLayerVisibleRegion`, plus `validateDisplay`/`presentDisplay`. That decoded stream **is** Spike B's
`OutputLayer` geometry, keyed by layer id, with the buffer attached — exactly what C1's blitter needs.

Must be answered **before `init()` returns**:

| group | methods |
|---|---|
| service | `getInterfaceVersion`, `getCapabilities`, `createClient`, `registerCallback`, `getMaxVirtualDisplayCount` |
| hotplug | callback `onHotplug(display=1, connected=true)` — must arrive between `setCallback` and `configureLocked()` |
| display config | `getDisplayIdentificationData`, `getDisplayConnectionType`, `getDisplayConfigs`, `getActiveConfig`, `getDisplayAttribute`, `getDisplayConfigurations`, `getDisplayCapabilities`, `getHdrCapabilities`, `getColorModes`, `getDisplayVsyncPeriod` |
| display mode | `setActiveConfig`, `setActiveConfigWithConstraints`, `setPowerMode`, `setVsyncEnabled`, `setClientTargetSlotCount` |

Per-frame (`CompositionEngine::Output::prepare/present` + `OutputLayer`): `createLayer`, the 12
`setLayer*` commands above, `validateDisplay`, `getChangedCompositionTypes`, `getDisplayRequests`,
`acceptDisplayChanges`, `presentDisplay`, `getReleaseFences`, `setColorTransform`,
`setLayerBrightness`, `setIdleTimerEnabled`, `notifyExpectedPresent`.

Of the **97** `AidlComposer::*` methods in the binary, ~**40** lie on these paths; the rest can
return `Error::UNSUPPORTED` without touching composition.

**LOC estimate** (hand-decoded parcels — no AIDL/BpComposerClient codegen exists in the image, and
the composer AIDL is not in the `.cache/aosp` checkout):

| piece | LOC |
|---|---:|
| binder nodes (`IComposer`, `IComposerClient`, callback), version/interface plumbing | 250–350 |
| `DisplayCommand` / `CommandResultPayload` codec (~40 variants) | 300–450 |
| display config + capability answers, synthetic mode table | 80–120 |
| 60 Hz vsync generator + callback dispatch | 60–100 |
| layer bookkeeping | 60–100 |
| **total** | **750–1120** |

1.5–2.5 agent-days of careful work; it is the only irreducibly ours part of this design.

---

## 5. Allocator / mapper: already solved by an ACOS piece

- SF imports client buffers through `GraphicBufferMapper` (gralloc5 AIDL `IMapper/default`, else
  HIDL mapper 2.0–4.0 via hwservicemanager) and allocates through `GraphicBufferAllocator`; the
  mapper version also selects `IAllocator` (`WR-window-surface-options.md` §0 Fact 2,
  `libs/ui/GraphicBufferAllocator.cpp:51-72`).
- **Our memfd buffers fit a tiny mapper by construction — ACOS already ships it:**
  `crates/acos-art/shim/gralloc_shim.c` (620 LOC → `libacoshw.so`) interposes both constructors
  (skipping HAL discovery), `allocate` (memfd + 9-int `native_handle`), `free`, `importBuffer`,
  `lock`/`lockAsync`/`unlock`, `getTransportSize`. Measured loaded inside SF:
  `software gralloc shim loaded (1 fd / 9 ints)`. **Zero new code**, provided the composer never
  requests client composition.
- What SF itself allocates in this configuration: **nothing per frame**. Init allocates only the
  1×1 clear-slot buffer when the composer reports interface version ≤ 1
  (`AidlComposerHal.cpp:261-263`); the RenderEngine image cache is GPU-side; client-target buffers
  exist only when `usesClientComposition`; screenshots allocate on demand. HWC virtual displays are
  the exception (they need an output buffer) — another reason to keep VDS off.
- The layer's `native_handle` arrives inside `setLayerBuffer` with the memfd, and fd passing through
  the ACOS driver is already proven (`results/art-feasibility/E3G.md` §0 rung 4). C1 keeps the blit;
  SF never touches the pixels in a DEVICE-only configuration.

---

## 6. Binder: the transport works; the service *table* is what is missing

Measured: with `LD_PRELOAD=…/libacos_binder_driver.so` and `E3G_SOCK` → `acos-binder-peer`,
SurfaceFlinger's `ProcessState::self()` succeeds, the driver connects (`05-fakegl-peer.log`:
`driver connected`), and SF proceeds into `init()` — a native `libbinder`/`libbinder_ndk` process
served exactly like the app's libbinder, because the hook is the single `BINDER_WRITE_READ` ioctl and
`libbinder_ndk` sits on the same `libbinder`.

Missing, in order of importance:

1. **A composer3 node the peer can serve**: `AServiceManager_isDeclared("…composer3.IManager/default")`
   must answer true, `waitForService` must return our node — i.e. §4, served through the same peer.
   *Measured consequence of its absence:* SF makes no further request at all (peer receives zero
   transactions) and stalls.
2. **Native `addService`**: SF publishes `"SurfaceFlinger"` and `"SurfaceFlingerAIDL"`; the peer
   registers *Java* local binders today. Native `BBinder` nodes must be registered and be findable
   by the app's `libgui` (`SurfaceComposerClient`) through the same peer. ~100–200 LOC.
3. **`waitForService`/`linkToDeath` semantics** for `"window"` and `"inputflinger"` (only reached in
   `bootFinished()`, `SurfaceFlinger.cpp:740-757`): return not-found promptly or fire a death
   notification so the boot-finished path does not stall.
4. `/dev/hwbinder` emulation: **not** needed if composer3 is declared (only the HIDL fallback,
   configstore and displayservice use it).

Nothing architectural is missing: the gaps are service-table entries plus one AIDL service.

---

## Reused AOSP components (what stock SF gives us unchanged)

| component | what it does for us | mode |
|---|---|---|
| `FrontEnd` (`TransactionHandler`, `LayerLifecycleManager`, `LayerHierarchy`, `RequestedLayerState`, `LayerSnapshotBuilder`) | the whole layer model: hierarchy, relative z, `destinationFrame`, `bufferTransform`, matrix, crop intersection, `parent*local` transforms, parent-bounds clipping, alpha/colour inheritance, rounded corners, shadows, blur settings, `invalidTransform`, hidden-by-policy | binary reuse (`libsurfaceflinger` is statically linked into the binary) |
| `CompositionEngine` (`Output`, `OutputLayer`, `LayerFE`) | per-layer display frame / source crop / output-relative buffer transform, `forceClientComposition` rules, validate/present loop, colour/dataspace | binary reuse |
| `Scheduler` (`VsyncSchedule`, `VSyncReactor`, `FrameTimeline`, `LayerHistory`) | frame clock, commit timing, `finishDrawing`/`reportNextDraw` completion | binary reuse |
| `ClientCache`, `TimeStats`, `FrameTracer`, `TransactionTracing`, `LayerTracing` | buffer/texture cache, stats, tracing | binary reuse |
| `libgui`/`libui`/`libbinder`/`libinput` | BLAST producers/consumers, `SurfaceControl`, transactions, `FrameStats`, input wiring | binary reuse (already in the image) |

## Custom code that remains

| piece | why it cannot be AOSP | LOC | status |
|---|---|---:|---|
| composer3 stub (§4) | no vendor HAL in the image, and none may be downloaded | 750–1120 | **new — the bulk** |
| fake EGL/GLES (§3) | A14 has no CPU RenderEngine; no driver may be installed | 226 (measured) | **written this box**, spike-scope only |
| peer: native `addService` + composer node routing (§6) | ACOS-specific transport | 100–200 | extends an existing crate |
| SF launch/wiring (bwrap root, `/apex`, props, gralloc preload) | ACOS-specific | 100–200 | mostly exists (`.cache/sa-spike/`, `crates/acos-art` patterns) |
| **C1** | still the blitter | 0 new | unchanged |

Spike B's alternative writes **0** new services and vendors ~700 LOC of geometry into C1.

## Memory + startup

**Fixed shared (file-backed, page-cache, shared by every session): measured `Pss_File`
16.14–16.22 MiB** at the serving-adjacent state. Analytic closure (`03-elf-closure.txt`): **80 ELF
files, 23.4 MiB of file bytes, 14.4 MiB of `.text`** — dominated by the 8.8 MB executable, then
`libgui` 1.6 MB, `libhidlbase` 0.8 MB, `libunwindstack` 0.7 MB, `libvintf` 0.7 MB,
`libprotobuf-cpp-lite` 0.5 MB. Much of it is already resident for an ART session (libgui/libui/
libinput/libbinder/libbase/libutils/libc++/libEGL/libvulkan/liblog are mapped by the app too), so
the *incremental* shared cost is the SF-only libs (`android.hardware.*` shims, displayservice/HIDL,
`libSurfaceFlingerProp`, `liblayers_proto`, `libtimestats*`, `libvintf`, `libunwindstack`) —
**est. 6–9 MiB host-wide, once** — plus 8.8 MB of executable pages.

**Additional per session (private): measured 4.43–4.44 MiB anon / 4.43–4.48 MiB Private_Dirty** for
a booted, RenderEngine-initialised, display-less SF. The binary's own writable segments are small
(`PT_LOAD RW`: 0x032268 + 0x005a60 ≈ **223 KiB** of `.data`+`.bss`), so that 4.4 MiB is loader +
heap for 46 shared libraries. Missing (estimate, **not** measured): display/scheduler state 1–3 MiB;
FrontEnd state 30–60 kB per 12-layer window (Spike B, measured model); and, if `captureDisplay`/
client composition is ever used, a GL driver (0 on real hardware — none here; 30–60 MiB per process
for SwiftShader, forbidden by the Amendment).

**The service process that would host the composer stub: measured Pss 765 kB (168 kB anon,
597 kB file), Rss 2.67 MiB, 1 thread** — that is the existing Rust `acos-binder-peer` (bin
`target/release/acos-binder-peer`, `06-peer-process-cost.txt`), i.e. the composer stub's marginal
cost is its decode buffers, not another daemon. In production the peer is already counted inside the
session host, so this is not additive per session.

**Startup: measured < 5 ms from `execve` to the stable running state** (the t=5 ms sample is
already 20.5 MiB; the `RenderEngine` thread is present in the 1.5 s snapshot) — because the fake GL
does no work. With a real
driver, add EGL context creation (~50–150 ms) and first-frame work. C1 today costs **no extra
process and no extra startup** (it lives in the launcher).

### Totals at N=1 / N=10 / N=20, same measurement type

**(a) One SurfaceFlinger per session** — per-session private, plus the shared part once:

| | N=1 | N=10 | N=20 |
|---|---:|---:|---:|
| **measured** private floor (booted, RenderEngine up, no composer) | **4.43 MiB** | 44 MiB | 89 MiB |
| **measured** shared file-backed (page cache) | **16.2 MiB** | 16.2 MiB | 16.2 MiB |
| **measured** service process for the stub (the Rust peer) | +0.17 MiB anon each (already in the session host) | — | — |
| **estimate** with a composer stub + display/scheduler/layers | 6–9 MiB | 60–90 MiB | 120–180 MiB |
| **estimate** if client composition / screenshots are ever enabled | +30–60 MiB (software GL) | +300–600 MiB | +600–1200 MiB |

**(b) C1 today** (`W12.md` §5, same PSS/cgroup method, small-pool Option A):

| | N=1 | N=10 | N=20 |
|---|---:|---:|---:|
| PSS delta vs the non-windowed ART path | **+48.0 MiB** | **+18.1 MiB each** (shared file pages) | as N=10 |
| guest cgroup (honest per-session charge) | +47.0 MiB | +36.1 MiB each | +36.1 MiB each |
| of which gralloc buffer bytes | 18.2 MiB (2 × 1080×2209×4) | same per session | same |

Against the owner's rule: a per-session SF would land in **~6–9 MiB private + 16 MiB shared once**
(estimated from the measured 4.43 MiB floor), i.e. **inside the "10–20 MiB strongly preferred"
band**. It is also smaller than C1's measured +48 MiB/session window-stack delta, but that delta is
not the same quantity: it includes the app-side `ViewRootImpl`/BLAST/HWUI pages and the 18.2 MiB of
gralloc buffers that *either* design needs. Comparing only the compositor: C1's own share is
inside the launcher (no separate process, no GPU object), SF's is 4.4 MiB measured + ~16 MiB shared.
Either way the rule does **not** reject stock SF. What remains is effort and
correctness (§"Compatibility + correctness"), not memory.

## Compatibility + correctness

What stock SF buys: the semantics inventory in `SPIKE-B.md` §2 — `destinationFrame`,
`bufferTransform`, matrix, parent-bounds clipping, alpha inheritance, relative z, rounded corners,
`invalidTransform`, `isHiddenByPolicyFromParent` — exactly the W42/W54 defects.

What it does **not** buy:

- **The pixels.** In a DEVICE-only configuration the stub receives geometry + the buffer handle and
  something must still blit: Google decides placement, **C1 still composes**. So the architecture
  does not remove C1; it adds SF in front of it.
- **Real RenderEngine.** Client composition, background blur, screenshots, colour management and
  HDR tone-mapping would all run through the synthetic GL. A fake that draws would be a silent
  correctness failure (`ENGINEERING_POLICY`: "no fake success and no silent fallback"), and ACOS's
  verification depends on screenshots — those would have to keep coming from C1.
- **Frame truth.** Synthetic vsync means no real present time: `FrameTimeline`, `finishDrawing`/
  `reportNextDraw` and release fences are approximations (C1 already documents synthetic fences,
  G1 §3), and HWC capabilities (`PRESENT_FENCE_IS_NOT_RELIABLE`, `SKIP_CLIENT_COLOR_TRANSFORM`) do
  not exist.
- **Input.** No input path (acceptable: ACOS has its own).

## Effort + dependencies

| dependency | why | smallest stand-in | LOC |
|---|---|---|---:|
| `libc/libm/libdl/libc++/libutils/liblog/libbase` | bionic runtime | in the sysroot | 0 |
| `libbinder` + `libbinder_ndk` | SF is a binder service and client | **ACOS driver + peer** (exists, measured) | 0 |
| servicemanager semantics | `addService` ×2, lookups, `isDeclared` | peer service table + native-node registration | 100–200 |
| composer3 AIDL + callback | hotplug, display config, composition decisions, vsync | our stub (§4) | 750–1120 |
| `libEGL`/`libGLESv2` (+ `/vendor/lib64/egl`) | `RenderEngine::create` at init | fake EGL/GLES (§3) | **226, measured** |
| `libui`/`libgui`/`libinput`/`libnativewindow`/`libsync` | buffers, fences, transactions | in the sysroot | 0 |
| allocator + mapper (`IAllocator`/`IMapper`) | import/allocate buffers | **ACOS memfd gralloc shim** (exists, measured) | 0 |
| power HAL, display config HAL, inputflinger, hwservicemanager | optional services | omit (fatal-free by source for power/input) | 0 |
| SurfaceFlinger props | behaviour switches | driver `kProps` table + 3 keys | ~20 |
| `libtimestats`, `liblayers_proto`, `libprotobuf-cpp-lite`, `libpng`, `libz`, `libunwindstack`, `libvintf`, `libselinux`, `libprocessgroup`, 15 × `android.hardware.*` | statically required by the binary | in the sysroot (23.4 MiB closure) | 0 |

Total custom: **1 100–1 550 LOC** (composer 750–1120, peer 100–200, wiring 100–200, fake GL already
written at 226) versus Spike B's **~700 LOC vendored + ~200 LOC glue, no new services**.

## Scaling to N=10 / N=20 independently isolated sessions

**One SF per session.** Feasible because ACOS's binder transport is per-session already (each
session owns its driver + peer socket), so each peer can expose its own `SurfaceFlinger`/
`SurfaceFlingerAIDL` and composer node. Costs: 4.4 MiB private each (measured floor, more with a
composer stub), one scheduler/vsync thread set, one RenderEngine object, one display id space, and
one copy of the shared pages (shared, not per session). Measured floor scales linearly: 44 MiB
(N=10) / 89 MiB (N=20) *before* the composer stub's own state. Isolation is good (kill on crash),
page cache is shared, and each session's GL state is independent (because it is fake).

**One shared SF for all sessions.** Memory-attractive: 16 MiB shared + ~6–9 MiB private **once**,
plus ~1–3 MiB of layer state and each session's own buffers per session; N=20 stays inside the
preferred band. The costs are the ones ACOS exists to avoid: a single point of failure for every
session, one composition thread serialising all sessions' frames, and "reset a session" becoming
"destroy and recreate a display + layer tree inside a shared object". It does not dodge either real
dependency (composer stub, synthetic GL) — it only amortises them.

Recommendation for scaling: if SF is adopted at all, **one instance per session** (isolation first,
memory is affordable); a shared instance should be considered only after the composer stub exists
and only if N=10 measurements show contention.

## Smallest proving experiment (design only, do not implement)

Ordered, cheapest kill points first; each step has an abort rule. (Steps 0–1 are *done* in this
box and are therefore not part of the estimate.)

0. *(done)* Headless boot with the ACOS binder driver, the ACOS gralloc shim and the fake EGL/GLES:
   SF stays alive at 20.6 MiB Pss. **Pass** ✔.
1. **Composer3 stub, boot set only (~3 h).** Implement the §4 "before `init()`" table plus
   `onHotplug(CONNECTED)` and a 60 Hz vsync; everything per-frame returns `Error::UNSUPPORTED`.
   Set the three props. *Pass:* log reaches `Initializing graphics H/W…` → `Done initializing`, both
   services published, process alive 60 s, `Pss` ≤ 30 MiB (kill the attempt if it exceeds 50).
2. **One DEVICE-composed layer reaches the stub (~3 h).** Send an existing C1 fixture transaction
   with a memfd buffer and a `destinationFrame`. *Pass:* the stub decodes one `executeCommands`
   batch containing `setLayerBuffer`(1 fd), `setLayerDisplayFrame`, `setLayerSourceCrop`,
   `setLayerTransform`, `setLayerZOrder`, `setLayerCompositionType=DEVICE`, and SF reports no client
   composition.
3. **The dialog geometry is Google's (~2 h).** Run the W54 scenario. *Pass:* the decoded display
   frame for the dialog's BLAST layer equals the stock bounds **[28,976][1052,1489]**, and
   transform/sourceCrop correspond to the 1024-wide buffer being scaled into that frame (Spike B's
   probe predicts these numbers exactly).
4. **C1 blits exactly what the stub decoded (~2 h).** Feed the decoded geometry into C1's existing
   `DrawSpec`/blit instead of `placement_of`. *Pass:* the dialog region is pixel-identical to the
   oracle screenshot and W54's regression stays green.
5. **Measure at serving state (~1 h).** `cgroup memory.stat` + `smaps_rollup`, N=1 then N=10 with 10
   instances. *Pass/report:* per-session private stays inside the owner's band or the design is
   rejected on the number (Amendment 2).

Total **~11 h** after this spike, with the risk concentrated in step 2 (parcelable codec without
AIDL codegen) and step 4 (screenshot parity: pixels come from C1, geometry from SF — two sources of
truth that must agree).

## Verdict (evidence, not preference)

1. **Stock SurfaceFlinger is closer to usable than expected, but not close enough to be the
   default.** Measured: it boots to a stable, 5-thread, 20.6 MiB (4.4 private) process with the ACOS
   binder transport, the ACOS memfd allocator/mapper and a 226-line null EGL/GL; it stops only
   because composer3 is absent. Every other dependency is either in the sysroot or already an ACOS
   piece.
2. **The owner's memory thresholds do not rule it out.** Per session: 4.43 MiB private measured,
   ~6–9 MiB estimated with a composer stub, plus ~16 MiB of shared file pages once. That is inside
   "10–20 MiB strongly preferred" and below C1's current +48 MiB (N=1) / +18.1 MiB PSS
   (N=10) — because SF allocates no buffers and (with a fake GL) draws nothing.
3. **What decides against it is effort and correctness, not size.** The composer stub is
   750–1120 LOC of hand-decoded AIDL with no codegen available (~1.5–2.5 days), SF still needs C1 to
   blit the pixels, and the synthetic RenderEngine makes client composition, screenshots, blur and
   HDR silently wrong — which `ENGINEERING_POLICY` forbids, and which ACOS's verification cannot
   afford.
4. **Recommendation:** take **Spike B's rung-4 path** — ~700 LOC of FrontEnd geometry inside C1
   (212 ns/transaction, 30–60 kB/session), keeping C1's blitter and the existing screenshot path.
   Re-open stock SurfaceFlinger only if the owner wants a **composer3 AIDL service** as a deliverable
   in its own right (it is reusable for anything that wants to be an Android compositor HIDL/AIDL
   client), in which case the ~11 h experiment above is the correct next box. Nothing in this spike
   changed C1, and no existing functionality was touched.

---

### Evidence index (`results/art-runtime/evidence/SPIKE-A/`)

| file | what it shows |
|---|---|
| `01-sf-without-binder-driver.log`, `01-peer.log` | SF aborts at `ProcessState::self()` with no binder transport |
| `02-sf-binder-gralloc-egl-abort.log` | driver + peer + gralloc shim: binder OK, gralloc OK, **abort inside stock libEGL** |
| `02-peer.log` | the Rust peer accepted SF's driver connection (native libbinder over the ACOS transport) |
| `02-samples.txt` | `smaps_rollup` every 5 ms on the real SF pid, stock-libEGL run: 19.5–21.1 MiB Pss |
| `03-elf-closure.txt` | 80-file ELF closure, 23.4 MiB file bytes, 14.4 MiB `.text`, per-file breakdown |
| `04-source-citations.txt` | the exact `android-14.0.0_r75` lines cited above (init order, hotplug gate, primary-display fatals, composer discovery, RenderEngine types, boot-finished waits) |
| `05-fakegl-sf-alive.log` | with the null EGL/GL: no abort, ProcessState + RenderEngine up |
| `05-fakegl-peer.log` | driver connected; **no** unsupported records ⇒ SF never asks for the composer |
| `05-fakegl-samples-and-threads.txt` | stable 20.56 MiB Pss, 4.43 anon / 16.14 file, 5 threads, 76 fds, 465 maps, per-thread syscall/wchan |
| `05-fakegl-stub-source.c` | the 226-line null EGL/GLES (150 exported symbols) that satisfies `RenderEngine::create` |
| `06-peer-process-cost.txt` | the Rust peer that would host the composer3 node: Pss 765 kB (168 kB anon), 1 thread |

Reproduce: `.cache/sa-spike/run2.sh` (stock libEGL), `.cache/sa-spike/run3.sh` (with the fake
EGL/GL bound over `/system/lib64/lib{EGL,GLESv2,GLESv1_CM}.so`), `.cache/sa-spike/bwrap-root.sh`,
`.cache/sa-spike/fakegl/`, inside
`tools/art-runtime/run-exclusive.sh --task SA -- tools/guarded.sh --mem 2G --time 90 -- …`.
