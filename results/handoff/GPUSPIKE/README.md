# GPUSPIKE — Waydroid's Mesa on the host GPU inside the ACOS guest

Branch `gpuspike` from `glspike4` (`ffb485af`), 2026-11-18. Private
`ACOS_RUNTIME_DIR`/`ACOS_SESSION_SOCKET` per run (`.cache/gpur-<arm>`),
`ACOS_VIEW_TEST_DIR=$PWD/.cache/lv`, every ART run through
`tools/art-runtime/run-exclusive.sh` + `tools/guarded.sh`, release binaries in
`.cache/gpuspike-target`. Default OFF (`ACOS_GUEST_GLES=mesa-host`).

## 0. Answer (decision-ready)

* **Phase 1 offscreen GL on the real GPU: WORKS, no ABI wall.** The guest's
  Android-14 `libEGL` loads Waydroid's Android-13 `libEGL_mesa.so`; EGL 1.5,
  ES 3.2, `GL_RENDERER = AMD Ryzen 9 9950X 16-Core Processor (radeonsi,
  raphael_mendocino, ACO, DRM 3.64, 7.2.11-arch1-1)` (this *is* the Radeon 610M
  iGPU on `renderD129`), `glClear` + `glReadPixels` = `255,0,0,255`, a heavy
  1080×2400 shader renders correct, non-clear pixels. Reuse was total: **zero GL
  code written**; Waydroid's libs + its prop set (`ro.hardware.egl=mesa`,
  `ro.hardware.gralloc=gbm`, `ro.hardware.vulkan=radeon`,
  `gralloc.gbm.device=/dev/dri/renderD129`).
* **Numbers vs SwiftShader (same probe, same 1080×2400 pbuffer):** heavy-shader
  frame **2.8 ms vs 17.4 ms (6.2×)**, full-surface readback **0.16 ms vs 1.3 ms
  (≈8×)**, GL-used session cgroup peak **~84 MiB vs ~126 MiB**, first
  `eglInitialize` **5–10 ms vs 3.6–6.3 ms** (slightly slower), `eglCreateContext`
  **1.4–2.4 ms vs 0.07–0.14 ms**. Six concurrent GL sessions share the GPU
  cleanly (81–86 MiB each, frame time scales with contention, all pass).
* **The two buffer-backed arms of `gles-software.yaml` do NOT pass on the real
  GPU, and that is the real finding:** Mesa cannot import ACOS's memfd gralloc
  handles (`MESA: Unexpected gralloc handle version 2400` → `skia: Could not
  create EGL image`). The wrapped screenshot reads back a **blank `#101010`
  texture instead of the app's `#F8F8FA`** (probe line says `ok=true`; the
  scenario fails, correctly) and PNG→`HARDWARE` returns `null`. This is exactly
  the gralloc seam of Phase 2, not a GL problem: offscreen GL needs no gralloc,
  anything touching an `AHardwareBuffer` does.
* **Phase 2 (GPU-drawn windows) is a multi-week architectural project**, ~3–5
  weeks for one engineer, and is a STOP-and-decide item (gralloc model, C1
  compositor input, host-process model, per-session GPU state). §6. Recommended
  scope if the owner wants it: not "hwui GPU for every window" first, but
  **GPU-backed `AHardwareBuffer`s for HARDWARE bitmaps and offscreen use (≈1–1.5
  weeks)**, which is the owner's original goal (apps that decode to HARDWARE).

## 1. Provenance (Waydroid vendor image, read-only; never modified/started)

| field | value |
|---|---|
| image | `/var/lib/waydroid/images/vendor.img`, 562,388,992 B, sha256 `c49c75c0e703bac7f1495e6db0e9232390a6539b3d732999dd601c63cb01bdd3`, raw ext2/4 (not GPT) |
| waydroid.cfg | `vendor_type=MAINLINE`, `arch=x86_64`, **`vendor_datetime=1792667170` = 2026-10-22T11:06:10Z** (`ro.vendor.build.date`: Thu Oct 22 13:06:10 CEST 2026), `system_datetime=1790539062` (2026-09-27), `binder_protocol=aidl3` |
| build | `waydroid/lineage_waydroid_x86_64/waydroid_x86_64:13/TQ3A.230901.001/aleasto10221306:userdebug/test-keys` — **Android 13 (SDK 33)**, our guest is Android 14 |
| Mesa | **26.0.8** (`GL_VERSION=OpenGL ES 3.2 Mesa 26.0.8`; radeonsi uses **ACO**, not LLVM, for shaders); LLVM **22.0.0** (`libLLVM22.so`, a mandatory DT_NEEDED of the driver, 105 MB) |
| licences | Mesa, libdrm: MIT; LLVM: Apache-2.0 WITH LLVM-exception; minigbm / gbm gralloc: BSD-3-Clause expected (ChromiumOS / Android-x86; per-file notices are in the image's `/etc/NOTICE.xml.gz`, not individually audited here); (`hwcomposer.waydroid`: licence not checked, **not** staged) |
| extraction | `debugfs rdump /lib64` into `.cache/gpu-scratch/vendor` (no loop mount, no root, no waydroid interaction); props from the image's `/waydroid.prop` + `/var/lib/waydroid/waydroid_base.prop` (read-only) |

Staged file list, size and sha256 for every file: `manifest.json` (19 entries =
15 distinct inodes, 158 MiB on disk; the EGL libs and the ICD are hardlinked under
two names). Stager command: `stage-mesa-host.sh` (uses `tools/art-gles.sh` with
the new `--extra-lib/--extra-hw/--prop/--render-node`).

| staged at | file | bytes | role |
|---|---|---|---|
| `lib64/egl/` + `lib64/` | `libEGL_mesa.so` `libGLESv1_CM_mesa.so` `libGLESv2_mesa.so` | 447,224 / 25,672 / 65,032 | what the guest `Loader.cpp` dlopens (`ro.hardware.egl=mesa`) |
| `lib64/` | `libgallium_dri.so` | 41,484,400 | Mesa gallium incl. **radeonsi** (+ zink, llvmpipe, softpipe, …) |
| `lib64/` | `libLLVM22.so` | 105,563,488 | NEEDED by `libgallium_dri` and RADV; not used for radeonsi shaders (ACO) |
| `lib64/` | `libdrm_amdgpu.so` `libdrm_radeon.so` `libdrm_intel.so` | 41,184 / 44,816 / 139,984 | NEEDED by `libgallium_dri` (intel/radeon only dlopen'd, never used) |
| `lib64/` | `libgbm_mesa.so` `libgbm_mesa_wrapper.so` `dri_gbm.so` | 13,752 / 28,792 / 219,064 | GBM for gralloc.gbm |
| `lib64/` | `libminigbm_gralloc_gbm_mesa.so` | 84,824 | cros_gralloc/minigbm core (mapper impl NEEDs it) |
| `lib64/hw/` | `gralloc.gbm.so` | 41,912 | Waydroid's `ro.hardware.gralloc=gbm` HAL module |
| `lib64/hw/` | `android.hardware.graphics.mapper@4.0-impl.minigbm_gbm_mesa.so` | 96,256 | passthrough mapper (loaded by libhidl by prefix; **not reachable yet**, §3) |
| `lib64/hw/` + `lib64/` | `vulkan.radeon.so` (RADV) | 17,140,048 | `ro.hardware.vulkan=radeon`; staged, **not exercised** (hwui is GLES here) |

Not staged (not needed for Phase 1): `libdrm.so` (the guest's own
`/system/lib64/libdrm.so` resolves first and satisfies every symbol — verified
in `abi-check.txt`), `gralloc.minigbm_gbm_mesa.so`, `allocator@2.0-impl`,
`hwcomposer.waydroid.so`, `vulkan.{intel,lvp,virtio,pastel}.so`, `libEGL_angle`.
Waydroid's vendor also ships `libEGL_angle` and the allocator@4.0 service binary
(`/vendor/bin/hw/android.hardware.graphics.allocator@4.0-service.minigbm_gbm_mesa`).

## 2. ABI check: Android-13 vendor libs vs the Android-14 guest — no wall

`abi-check.txt` (`symcheck.py`: every undefined dynamic symbol of every staged
file vs the exports of its transitive DT_NEEDED closure over the guest sysroot +
staged dir); `dt_needed.txt` (the DT_NEEDED lists).

* **Every NEEDED resolves** (guest: `libc/libm/libdl` from the runtime APEX,
  `libc++`, `liblog`, `libcutils`, `libutils`, `libz`, `libsync`,
  `libnativewindow`, `libhardware`, `libhidlbase`, `libgralloctypes`,
  `android.hardware.graphics.mapper@4.0`, `libdrm`, `libEGL`, `libGLESv{1_CM,2}`;
  staged: the rows above). Nothing missing, no bionic API-level symbol gap
  (`__memcpy_chk`, `arc4random_buf`, `aligned_alloc`, `__sched_cpucount` all in
  the guest libc).
* **Exactly one unresolved symbol in the whole closure:**
  `_ZTH23_mesa_glapi_tls_Context` (a *weak* TLS-init wrapper; benign, it is the
  C++ thread_local init stub, never called).
* libc++ / libhidl / mapper: Mesa binds `android::hardware::graphics::mapper::V4_0::IMapper::getService`
  and `libgralloctypes` decoders from the guest's own libs — present; the ABI
  holds (libc++ is `std::__1` on both). The only A13↔A14 *behavioural* difference
  seen is that `getService` needs a hwservicemanager that ACOS does not have (§3).
* The bare-name lesson of GLSPIKE4 held: drivers + closure in `lib64/` (the
  guest default namespace searches `/vendor/${LIB}`); `libEGL_mesa.so` NEEDs
  `libEGL.so` (the guest loader), not the other way round.

## 3. Evidence chain (what had to change, in order)

1. **Stage + props, no sandbox change** (`p1/`): `libEGL_mesa` loads, EGL 1.5
   initializes, but Mesa logs `EGL-MAIN: failed to open /dev/dri/renderD129:
   Permission denied` ×3 and falls back to **`GL_RENDERER=llvmpipe (LLVM 22.0.0)`**.
   (The fallback is the safety net: a mis-staged GPU provider yields correct
   software pixels, never an abort. A probe MUST check `GL_RENDERER`.)
2. The node is `0666` and bwrap `--dev-bind` opens it fine (tested with the exact
   flag set: `--unshare-user --uid … --cap-drop ALL`), so the denial is **Landlock**:
   the guest ruleset handles `WRITE_FILE` + `IOCTL_DEV` and grants them only on
   `/data/*` and a few `/dev` nodes (`crates/acos-art/src/landlock.rs`).
3. **Fix (flag-only):** `landlock::write_gpu_plan` writes
   `<data>/seccomp/landlock.gpu.renderD<N>.plan` = the default plan + exactly one
   extra rw root, bound over `/data/seccomp/landlock.plan` **for that run only**;
   `Sandbox::command` adds `--dev-bind /dev/dri/renderD<N>` only when the
   provider's manifest has a validated `render_node`
   (`/dev/dri/renderD<digits>` only: no card nodes, no modesetting, no other
   device; `$ACOS_GUEST_GLES_RENDER_NODE` overrides the number per machine).
   Seccomp needed no change (it is a syscall denylist; DRM ioctls pass).
   Result (`p2/`): **`GL_RENDERER=AMD Ryzen 9 9950X 16-Core Processor (radeonsi,
   raphael_mendocino, ACO, DRM 3.64, 7.2.11-arch1-1)`**, `glClear+glReadPixels
   rgba=255,0,0,255 glErr=0x0`.
4. Leftover Mesa warnings that are harmless for offscreen GL:
   `HidlServiceManagement: getService: defaultServiceManager() is null` (Mesa
   asks for the IMapper v4 *service*; ACOS has no hwservicemanager) → Mesa's
   documented "fallback gralloc" (`Using gralloc header from libdrm/android/gralloc_handle.h`),
   and `failed to get driver name for fd -1` (an extra swrast-probe pass).
5. **Arms (`mesa-gles/`, scenario fails honestly):**
   `gles_draw start=1 provider=mesa` ✔; then
   `MESA: Unexpected gralloc handle version 2400` / `64` (the buffer height: Mesa's
   fallback reads the ACOS shim handle as a libdrm `gralloc_handle_t`; the shim's
   handle is `{ver 12, 1 fd (memfd), 9 ints}`), `skia: Could not create EGL image`,
   → `hwbuffer_screenshot ok=true … mean=16 #FF101010` (**wrong pixels**, expected
   `#FFF8F8FA`) and `OpenGLRenderer: Failed to allocate a hardware bitmap` →
   `gles_png_hardware ok=false`. Same scenario on ANGLE/SwiftShader passes (GLSPIKE4)
   (why it works there was not re-derived in this spike; GLSPIKE4 §4 records that the memfd import works through ANGLE).
   `scenarios/gles-mesa-host.yaml` = `gles-software.yaml` with `provider=mesa`.

## 4. Measurements (indicative: shared slots, host load ~6, 3 interleaved reps, not exclusive)

Probe: `fixtures/apps/egl-probe` (new `bench`: 1080×2400 pbuffer, `glFinish` per
frame, 1.5 s time-boxed, ≤400 frames). Runs `bench-mesa-{1,2,3}`,
`bench-swiftshader-{1,2,3}`.

| metric | host GPU (`mesa-host`) | SwiftShader (`angle-swiftshader`) | ratio |
|---|---|---|---|
| first `eglInitialize` | 5.3 / 7.1 / 9.8 ms (p2: 4.8) | 3.6 / 3.8 / 6.3 ms | 1.5× slower |
| `eglCreateContext` | 1.4 / 1.4 / 2.4 ms | 0.07 / 0.07 / 0.14 ms | 20× slower |
| shader link | 1.4–1.55 ms | 2.6–4.4 ms | 2× faster |
| clear-only frame | 0.025–0.035 ms | ≈0 (lazy, `glFinish` is a no-op) | n/a |
| **alu24 full-screen frame** (p50) | **2.80 / 2.80 / 2.81 ms** | 17.4 / 17.5 / 16.5 ms | **6.2× faster** |
| **1080×2400 `glReadPixels`** | **0.16–0.22 ms** | 1.19–1.48 ms | ≈8× faster |
| session cgroup peak, GL used | **82 / 85 / 85 MiB** | 123 / 126 / 128 MiB | −41 MiB |
| 6 concurrent GL sessions | 81–86 MiB each; alu24 13.7 ms p50 each (GPU shared 6 ways ≈ 6×2.8), all pass | — | — |
| launcher provider verify (flag on, per launch) | **433–867 ms** (183 MB, 15 distinct inodes sha256'd) | 81–175 ms (65 MB) | see note |
| unused-flag cost, `counter` (GL never touched) | peak 113 / 119 MiB vs 111 / 111 off; 24/24 steps both | — | **+2…+8 MiB (noise-level, not nil this time)** + the verify time above |

Notes: (a) GPU/driver memory (GTT/VRAM, the radeonsi context) is kernel-side and
is **not** in the cgroup number; it was not separately measured. (b) the
verify cost is the one avoidable regression: hashing 160 MB per launch; a
per-cohort (mtime+size+ino) cache or verify-once would make it ~0 (not done — it
would weaken the "verify on every launch" rule of GLSPIKE; owner call). (c) the
alu24 pixel differs by 12 in R (`118` vs `130`: `mediump sin`) — both non-clear,
valid; not a defect. (d) frame cost numbers are GPU-side `glFinish` times for a
synthetic shader, **not** an app-frame benchmark.

## 5. Default behaviour (flag OFF) — unchanged

* No code path changes without the flag: `gles.rs` is reached only when
  `ACOS_GUEST_GLES` is set; the render-node bind, props and Landlock plan swap are
  inside the provider block; the default `landlock.plan` is untouched.
* `cargo test --release -p acos-art --lib`: **203 passed, 0 failed, 1 ignored**
  (198 before; +5 new: `only_a_dri_render_node_can_be_bound`,
  `a_manifest_cannot_override_the_derived_properties`,
  `a_hand_edited_render_node_is_rejected_at_load_and_a_missing_one_is_named`,
  `extra_props_and_render_node_round_trip_through_the_manifest`,
  `landlock::…the_gpu_plan_adds_exactly_the_render_node_and_nothing_else`).
* Flag-off ART gates on the final binary: `off-counter` **24/24**, `off-a11y-probe`
  **15/15**, `off-a11y-wrap` **3/3**, `off-counter2` 24/24 (all `pass`).
* Existing providers still load (manifest fields are `serde(default)`).

## 6. Phase 2 — sizing: GPU-drawn app windows (no build)

**Today:** the host clears `FLAG_HARDWARE_ACCELERATED` (`LaunchMain.java`
~L101/L4196, "surface-less ACOS window only draws through the software
`ViewRootImpl` path"), so `ViewRootImpl.drawSoftware` → `Surface.lockCanvas` →
in-process `BLASTBufferQueue` → buffers allocated by **`gralloc_shim.c`** (memfd,
handle `{1 fd, 9 ints}`, fences always −1) → `SurfaceComposer.applyTransaction`
over the ACOS binder peer → **C1** (`crates/acos-services/src/surface_client.rs`,
3124 lines) keeps the newest `GraphicBuffer` per layer as a memfd, CPU-blits on
screenshot.

**Target (Waydroid-like):** hwui `ThreadedRenderer` → `eglCreateWindowSurface` on
the BLAST `ANativeWindow` → Mesa `droid_create_surface` dequeues
GPU-renderable buffers → radeonsi draws → `queueBuffer` + native fence →
BLAST → C1. Waydroid does the last hop as `hwcomposer.waydroid` → Wayland dmabuf;
ACOS's last hop is C1, so the equivalent is "C1 reads/waits dmabufs".

**The exact seams** (each is a real work item):

| # | seam | what is needed | size |
|---|---|---|---|
| 1 | **Buffer allocation** — `crates/acos-art/shim/gralloc_shim.c` | a GPU mode allocating via cros_gralloc/minigbm (`libminigbm_gralloc_gbm_mesa.so`, the very lib Waydroid's `allocator@4.0-service` wraps, or `gbm_bo_create` on `libgbm_mesa`) **linear** dmabufs on `renderD129`, handles in the format the matching mapper validates (multi-fd/ints; today `getTransportSize` is a fixed `(1,9)`) | 4–6 d |
| 2 | **Import in Mesa** — `libEGL_mesa` asks `IMapper::getService("default", true)` which fails (`defaultServiceManager() is null`) | interpose that symbol (LD_PRELOAD, like the shim) to return the passthrough mapper from the staged `…mapper@4.0-impl.minigbm_gbm_mesa.so` (`HIDL_FETCH_IMapper`); also `GraphicBufferMapper` entry points in the shim route to the same impl in GPU mode | 3–4 d (the fix to the `Unexpected gralloc handle version` failure above, and what makes HARDWARE bitmaps work) |
| 3 | **Fences** — shim reports acquire/release `-1` ("software, already done") | real `sync_file` fds from Mesa's `eglSwapBuffers` (`EGL_ANDROID_native_fence_sync`) must flow through `queueBuffer`/BLAST transactions (`BINDER_TYPE_FD` already proven) | 2–3 d |
| 4 | **C1 input** — `surface_client.rs` lines ~125–140/305–335 (`GraphicBufferMeta`, caps "1 fd", `stride*4*h`, memfd `mmap`) | accept multi-fd GPU handles; `mmap` the dmabuf with `DMA_BUF_IOCTL_SYNC`, wait on the acquire fence before reading; honour real stride/format from the handle; keep memfd path for flag-off | 4–6 d |
| 5 | **Window flag + WMS** — `FLAG_HARDWARE_ACCELERATED` clearing (app.rs/launcher, `LaunchMain.java`) | per-flag keep the bit so `ViewRootImpl.enableHardwareAcceleration` creates the `ThreadedRenderer`; relayout/insets already right (W54) | 1–2 d + fixing newly reachable paths (surface insets, `RenderThread` threads under seccomp, `CanvasContext` first-frame stalls) 3–5 d |
| 6 | **Process/host model** — a render node and DRM context per session (GPU/GTT memory is kernel-side and was not measured here); the host RAM/guard model (`guarded.sh`, cgroup) does not see GPU memory | per-session GPU memory budget + quota check, device-loss handling (what happens when the GPU resets: today a hung guest, needs `EGL_CONTEXT_LOST`/watchdog policy) | 3–5 d |
| 7 | **Screenshot/observation parity** — `art-frames`, `acos shotz`, md5 frame tests (`md5_frame_tests.rs`) | GPU output is not bit-identical to the Skia-CPU reference (GPU `mediump`, AA, filtering differ by small deltas); the verify/assert-pixel contract needs a tolerance or a software-reference mode | 3–5 d + owner policy |
| 8 | **Test/gate matrix** | all gates ×{GPU window, software window}, N=10 memory, device without a GPU/render-node fallback | 4–6 d |

**Reusable from Waydroid as reference (no rewrite):** `libgallium_dri`, EGL libs,
`libminigbm_gralloc_gbm_mesa` + `mapper@4.0-impl.minigbm_gbm_mesa` +
`allocator@4.0-service.minigbm_gbm_mesa` (allocate/import/lock semantics, plane
layout, modifiers), `gralloc.gbm.so` (simple GBM gralloc, reference for the
handle layout Mesa's fallback gralloc validates), `hwcomposer.waydroid.so`
(reference for fence handling and dmabuf export to the host compositor; **not**
loadable as-is: it speaks Waydroid's Wayland-host HIDL protocol and needs
hwservicemanager). Not reusable: anything that needs hwservicemanager/hwbinder
(ACOS has neither) unless interposed as in seam 2.

**Total: ≈ 5–6 engineer-weeks** for hwui-GPU windows with parity, **≈ 1–1.5
weeks** for the narrower "GPU-backed AHardwareBuffer / HARDWARE bitmap" scope
(seams 1+2 only, windows stay software, fences stay −1 because Mesa renders
synchronously into the buffer before the app reads it via a GPU readback).
Architectural-wall status: seams 1, 4, 6 are compositor / gralloc model / process
model changes → **owner decision required** before any build (brief's STOP rule).
Also contradicts the standing 2026-11-12 compositor rule "no GPU driver stack
unless proven necessary and approved" — hence a decision, not a default.

## 7. Options (owner decision)

| | what | when it pays | size |
|---|---|---|---|
| A | **Ship `mesa-host` as the offscreen GL provider as-is** (opt-in) | agents that run GL-heavy offscreen work (TextureView/GLSurfaceView/Skia GPU readback of *non-AHB* surfaces) on a machine with a render node; 6× faster than SwiftShader, −41 MiB/session | done; + verify-cost fix (0.5 d) |
| B | **A + GPU-backed AHardwareBuffers** (seams 1+2) | makes `gles-software` arms (screenshot wrap, PNG→HARDWARE) pass on the GPU; unblocks Glide/Coil HARDWARE bitmaps *off-window* | 1–1.5 wk |
| C | **hwui-GPU windows** (all seams) | only if the owner's apps need hwui-only GPU features or software Skia frames are a measured bottleneck; software Skia at 1080×2400 is already the shipped, bit-exact path | 5–6 wk |
| D | **Do nothing beyond SwiftShader (GLSPIKE4)** | correctness-first (bit-exact frames); GPU absent on CI machines | 0 |

Recommendation: **A now (it is free and correct), B only if HARDWARE-bitmap
apps matter, C not without a measured software-frame bottleneck** — the
verifier's value is determinism, which GPU rasterisation weakens (seam 7).

## 8. Hazards found (worth knowing even if nothing ships)

* **Silent wrong pixels:** with the flag on, a HARDWARE-bitmap draw whose AHB
  cannot be imported yields a blank texture (`#101010`) with no exception. The
  probe's `ok=true` is its own flag, not a pixel claim: assert pixels.
* A mis-permitted GPU provider **silently falls back to llvmpipe** (LLVM 22 CPU
  rasteriser, 105 MB mapped) — always assert `GL_RENDERER`.
* `ro.hardware.egl=mesa` must be the manifest tag (not the provider dir name) —
  unchanged from GLSPIKE2.

## 9. What changed

| file | change |
|---|---|
| `tools/art-gles.sh` | `--extra-lib`, `--extra-hw`, `--prop`, `--render-node` → manifest `props` / `render_node` |
| `crates/acos-art/src/gles.rs` | manifest `props`, `render_node`; `valid_render_node`, `valid_prop_key`, `Provider::render_node`, `selected_props`; 4 tests |
| `crates/acos-art/src/props.rs` | applies manifest props (never `ro.hardware.egl/vulkan`) |
| `crates/acos-art/src/lib.rs` | `--dev-bind` of the validated render node + per-run Landlock plan bind, flag-on only |
| `crates/acos-art/src/landlock.rs` | `write_gpu_plan` + test |
| `fixtures/apps/egl-probe` | `bench` (frame/readback cost) |
| `scenarios/gles-mesa-host.yaml` | the GLSPIKE4 arms with `provider=mesa` |
| `results/handoff/GPUSPIKE/` | evidence, scripts (`run-arm.sh`, `run-bench.sh`, `run-gates.sh`, `run-n.sh`, `run-all.sh`, `stage-mesa-host.sh`, `symcheck.py`) |

## 10. Evidence index

`p1/` (render node denied → llvmpipe) · `p2/` (radeonsi proof) · `mesa-probe3/`,
`mesa-gles/` (arms: probe pass, gles scenario **fail** with the Mesa import
lines) · `bench-mesa-{1,2,3}/`, `bench-swiftshader-{1,2,3}/` · `n6-{1..6}/` ·
`off-*`, `on-counter*`, `off-counter2/` (gates) · `manifest.json`,
`abi-check.txt`, `dt_needed.txt`.

## 11. Environment / cleanup

Host RAM peak ≈ 16 GiB of 60 (6 concurrent sessions); disk 75–83 % of `/`. At
the end: deleted `.cache/gpu-scratch` (vendor extract ≈ 263 MB), `.cache/gpuspike-target`,
`.cache/gpur-*`, `.cache/lv`, the `fixtures/build` copy. Kept: the staged
provider `<sysroot>/gles/mesa-host/` (158 MiB) and this directory.
