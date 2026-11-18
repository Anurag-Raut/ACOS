# GLSPIKE4 — ANGLE on SwiftShader initializes in the guest: the wall was one missing file name

Branch `glspike4` from `glspike3` (`3af8dfdc`), 2026-11-18. Private
`ACOS_RUNTIME_DIR`/`ACOS_SESSION_SOCKET` per run (`.cache/g4r-<arm>`),
`ACOS_VIEW_TEST_DIR=$PWD/.cache/lv`, every ART run through
`tools/art-runtime/run-exclusive.sh` + `tools/guarded.sh`, release binaries in
`.cache/glspike4-target`, fixtures built with `tools/build-apps.sh`.

## 0. Answer

**A small fix works.** `eglCreateContext` failed because ANGLE's `eglInitialize`
had *already* failed; GLSPIKE3 read libEGL's `EGL_TRUE` as ANGLE's. ANGLE's
Vulkan renderer failed at `vkCreateInstance` because the image's `libvulkan.so`
could not `dlopen` the SwiftShader ICD **by its bare name**:

```
vndksupport: Could not load vulkan.pastel.so from default namespace: dlopen failed: library "vulkan.pastel.so" not found.
libEGL: ANGLE Error:Display.cpp:1023 (initialize): ANGLE Display::initialize error 0: Internal Vulkan error (-3) ... RendererVk.cpp, initialize:1782
libEGL: eglInitialize(c340cf40) failed (EGL_NOT_INITIALIZED)
```

The guest has no `sphal` namespace, so that call is a default-namespace `dlopen`
over `namespace.default.search.paths` = `/system, /system_ext, /product,
/vendor/${LIB}, /odm` (`ld.config.txt`, `[legacy]` section) — `/vendor/lib64`, **not**
`/vendor/lib64/hw`, where GLSPIKE2 staged the ICD. The fix is the same
two-names-one-inode trick the provider already uses for the drivers (`lib64/` +
`lib64/egl/`): hardlink the ICD at `lib64/vulkan.pastel.so` as well. No new GL
code, no gralloc work, no namespace/linkerconfig change, same bytes from the same
source image (sha256 `7af4f7c9…` unchanged).

After it, in the guest: ANGLE `2.1.24303` on **Vulkan 1.3.0 SwiftShader Device
(LLVM 10)**, `OpenGL ES 3.1`, 50 EGL configs, `eglCreateContext`/pbuffer/
`eglMakeCurrent` succeed, `glClear`+`glReadPixels` returns `255,0,0,255`, and the
two HARDWARE-bitmap arms of `gles-software.yaml` read back real pixels (§4).
One app-level arm stays a wall and it is **not** a GL wall (§5).

## 1. Status

| Brief item | State |
|---|---|
| (1) register EGL14 (+GLES20) natives, default-inert | **DONE** — `LaunchMain.registerNatives`: `register_android_opengl_jni_{EGL14,EGL15,GLES20}`, **only when `acos.guest.gles` is set** (`guestGlesSelected()`), so flag-off apps still get a catchable `UnsatisfiedLinkError` instead of libEGL's `couldn't find an OpenGL ES implementation` SIGABRT |
| (2) run an EGL probe with the flag | **DONE** — new diagnostic fixture `fixtures/apps/egl-probe` + `scenarios/egl-probe.yaml` (a plain `acos-art run --dex` cannot register natives; the probe must be a launched app) — `probe1/` (before), `probe2/`,`probe3/` (after) |
| (3) decide backend vs Vulkan path | **DONE** — Vulkan path, ICD name (§2); ANGLE's backend selection was right (Vulkan, default) |
| (4) small fix → arms, pixels, RSS | **DONE** — fix in `tools/art-gles.sh` + `gles.rs`; arms (§4), memory (§6) |
| (5) size the remaining gap | **DONE** — gralloc/`AHardwareBuffer` import is **not** a gap (it works, §4); the remaining gap is the window renderer (§5), sized and stopped |
| default behaviour unchanged | **PASS** — §3 |

## 2. Evidence chain (decision (3))

1. `register_android_opengl_jni_EGL14` etc. are exported by the image's
   `libandroid_runtime.so` at **global** scope; `render_shim.c`'s
   `RenderNat.register` already tries the global mangling, so adding the names was
   enough (`nm -D`).
2. The probe is a real launched app, so it also shows what GLSPIKE3 could not:
   `client EGL_EXTENSIONS=EGL_ANDROID_GLES_layers EGL_ANGLE_platform_angle
   EGL_EXT_client_extensions EGL_KHR_platform_android`.
3. **Before the fix** (`probe1/`, `key-lines.txt`): `eglInitialize -> true` but
   `version=-1.-1`, `EGL_VERSION=1.4 Android META-EGL`, `eglChooseConfig ->
   false, EGL_NOT_INITIALIZED`, `eglGetConfigs total=0`. `-1.-1` is the loader's
   own `cnx->major/minor` left at their initial value: the driver's
   `eglInitialize` failed (`libEGL: eglInitialize(...) failed`), while the
   loader still returned true because its own extension string was non-empty.
   **GLSPIKE3's "eglInitialize succeeds" was the loader, not ANGLE**, and the "two
   calls, same display" mystery disappears (nothing was ever initialized; hwui's
   `EglManager` just saw `eglChooseConfig` fail the retry path and `eglCreateContext`
   fail). ANGLE's own ERR line *is* logged on the **release** build
   (`E/libEGL: ANGLE Error:Display.cpp:1023 …`); GLSPIKE3 concluded it was
   compiled out because its arm died by SIGABRT, and the guest's logd snapshot at
   death did not carry those records. The probe app survives, so the complete
   logcat shows them.
4. The line above it is the cause: `vndksupport: Could not load vulkan.pastel.so
   from default namespace`. The log tag/message are libvndksupport's
   `android_load_sphal_library` fallback ("Could not load %s from default
   namespace") and the name it was given is the bare `vulkan.pastel.so`; with no
   sphal namespace that is a default-namespace `dlopen`. (GLSPIKE3's "a failed `dlopen`
   maps nothing, so the ICD is never asked for" was right about the symptom and
   wrong that no lookup happened: the lookup ran and failed by name.)
5. **Backend selection was never the problem**: with the ICD findable, the
   default (`EGL_DEFAULT_DISPLAY`) display initializes ANGLE's Vulkan backend on
   SwiftShader straight away. The Android loader does not forward explicit ANGLE
   platform displays at all: `eglGetPlatformDisplay(EGL_PLATFORM_ANGLE_ANGLE, …)`
   returns `EGL_BAD_PARAMETER` (`libEGL: eglGetPlatformDisplayTmpl:254`) for every
   variant tried (default, Vulkan, Vulkan+SwiftShader device type, OpenGLES,
   OpenGL, Null), before and after the fix — so none of "ANGLE config / `debug.angle.backend` /
   `ro.gfx.angle.supported` / platform-type attribute" is available or needed on
   this path. (Props read in the guest: `ro.hardware.egl=angle`,
   `ro.hardware.vulkan=pastel`, `debug.angle.backend`/`ro.gfx.angle.supported`
   unset.)
6. Isolation of the change: private provider copy (hardlinks of the shared one)
   plus **one** extra hardlink `lib64/vulkan.pastel.so`, via `ACOS_GUEST_GLES_DIR`
   (`probe2/`); then the same through the product path after `tools/art-gles.sh`
   re-staged the shared provider (`gles-prod/`, `final-gles/`).

## 3. Default behaviour (flag OFF) — unchanged

* No guest code path changes without the flag: the EGL registrations are inside
  `if (guestGlesSelected())`; `gles.rs` is reached only when `ACOS_GUEST_GLES` is
  set; `art-gles.sh` is a staging tool.
* `cargo test --release -p acos-art --lib`: **198 passed, 0 failed, 1 ignored**
  (195 before; +3 new: `an_icd_staged_only_at_the_hal_path_is_rejected` — the test
  that fails before the fix and passes after —,
  `the_icd_soname_path_is_the_bare_name_in_the_bound_lib_dir`,
  `hardlinked_names_are_hashed_once_and_each_checked`). One earlier full run had
  `zygote_prod::tests::the_keeper_protocol_spawn_kill_reset_and_shutdown` fail
  once; it passed in 3/3 reruns including after the final change (flake, not touched
  by this branch).
* Flag-off ART arms on the final binary: `final-off-counter` **24/24**,
  `final-off-a11y-probe` **15/15**, `final-off-a11y-wrap` **3/3** (all `pass`).
  The all-caps `INCREMENT` finding of GLSPIKE2 did not reproduce (as in GLSPIKE3).
* Shared sysroot: `<sysroot>/gles/angle-swiftshader` was re-staged with the fixed
  `tools/art-gles.sh` (same bytes, one more hardlink + manifest entry). Binaries
  from older branches accept the superset manifest; binaries from this branch
  reject a manifest without `lib64/vulkan.<tag>.so`, loudly and naming the tool.

## 4. The arms with the flag ON (product path)

`scenarios/gles-software.yaml`, `final-gles/` (**pass 3/3**), guest lines
(`key-lines.txt`):

```
gles_draw start=1 provider=angle
gles_draw kind=hwbuffer_screenshot ok=true src=1080x2400 config=HARDWARE dst=64x64 mean=248 samples 16,16=#FFF8F8FA …
gles_png_hardware ok=true config=HARDWARE width=64 height=64
gles_draw kind=png_hardware ok=true … 16,16=#FFFF0000 48,16=#FF0000FF 16,48=#FFFF0000 48,48=#FF0000FF
gles_view skipped=software_window_cannot_draw_hardware_bitmap (opt-in acos.probe.gles_view=1)
```

* **(i) wrapped a11y screenshot** — the real 1080×2400 `HardwareBuffer` from
  `AccessibilityService.takeScreenshot`, `Bitmap.wrapHardwareBuffer`, read back
  through hwui's GPU path (`Bitmap.copy(ARGB_8888)` → `EglManager` →
  ANGLE/SwiftShader), pixels `#FFF8F8FA` (the app's light background).
  `screenshot_pixel unavailable=hardware_bitmap_needs_egl` was the pre-change
  state.
* **(ii) PNG → `Bitmap.Config.HARDWARE`** — decoded, uploaded by hwui's
  `GrallocUploadThread` (the very thread that aborted in GLSPIKE/2/3), read back:
  exact `#FFFF0000` / `#FF0000FF` corners. `egl-probe` shows the same with a
  `HardwareBuffer.create` + `wrapHardwareBuffer` (`probe3/`: `hw: wrapped copy ok`).
* The **fixture's draw method was changed** (`A11yProbeService.drawAndSample`): it
  drew a HARDWARE bitmap straight onto a software `Canvas`, which the framework
  refuses on a real device too (`IllegalArgumentException: Software rendering
  doesn't support hardware bitmaps`; `gles-sw/`). The app-level operation is the
  GPU readback above, and that is what the scenario now measures. The flag-off
  line `gles_draw skipped=no_software_gles` is unchanged.
* **Gaten** (`gaten-bubble-overlay`, flag on with the fixed provider,
  `gaten-on/`): **fails at step 21** (`screenshots: expected 1 array element,
  got 0`) and **fails identically with the flag off** (`gaten-off/`, same step,
  same assertion). That step is the scenario's own documented timing-sensitive
  tap, so this run neither shows nor refutes Gaten's lookup getting past EGL
  (the artifact logcat is a prefix that ends before it). Not attributable to GL;
  it needs a clean re-run of the baseline (20-run protocol in the scenario
  header) to say flake vs regression.

## 5. The remaining gap, sized — and a STOP

**(iii) a HARDWARE bitmap inside a real window is not a GL gap, it is the
window renderer.** `ViewRootImpl.drawSoftware` paints ACOS windows on a software
`Canvas` (`Surface.lockCanvas`). `BaseCanvas.throwIfHwBitmapInSwMode` then throws
`IllegalArgumentException: Software rendering doesn't support hardware bitmaps`
inside the looper → `LOOPER_THREW`, process dies (`gles-sw/`, `gles-sw2/`: same
crash with a working EGL). Any app that puts a Glide/Coil-default HARDWARE bitmap
in an `ImageView` hits it **regardless of the GL provider**. The probe step is
therefore opt-in (`acos.probe.gles_view=1`) and the scenario asserts the explicit
skip line, not a pass for it.

This is an architectural decision (window rendering / compositor, a STOP under
the brief's rule), so here are the options with sizes — estimates, not measured:

| Option | What | Size | Cost/risk |
|---|---|---|---|
| A. keep software windows | document the wall; apps that need HW bitmaps in views read them back first | 0 | the crash stays for the common Glide/Coil default |
| B. image deviation | make `BaseCanvas.throwIfHwBitmapInSwMode` copy-instead-of-throw (patch the image's `framework.jar`/boot image class) | days | deviates from the image; wrong in subtle ways (stale pixels, memory) |
| C. hwui `ThreadedRenderer` for windows on ANGLE/SwiftShader | `eglCreateWindowSurface` on the BLAST `ANativeWindow`, GPU-usage gralloc allocations through the memfd shim, host compositor reading GL-written buffers | weeks | compositor/gralloc change; per-frame CPU rasteriser cost; per-session GL memory (§6) |

Recommendation: **A or B first** (the owner's goal is *drawing/reading* HARDWARE
bitmaps, which is done); C only if apps need hwui-only features.

Not a gap (checked): the gralloc/`AHardwareBuffer` import into SwiftShader
works for the screenshot and uploader paths through the existing memfd shim; no
HAL mapper stand-in was needed for them (§4 (i)/(ii)). The
`GraphicBufferMapper::getAllocationSize` segfault named in `gaten-bubble-overlay.yaml`
is the **strict-observation race** documented there (null mapper object in
`libui`, reached from a probe request), flag-independent, and not touched here.

## 6. Cost (cgroup `memory.peak` per session; indicative; `run-all.sh`)

| Arm | peak MiB | note |
|---|---|---|
| flag off, `counter` | **109** | baseline |
| flag **on**, `counter` (GL never touched) | **108** | **Δ = −1 MiB (0 within noise): unused-flag cost is nil** |
| flag off, `a11y-probe` / `a11y-screenshot-wrap` | 81 / 71 | same app, GL unused |
| flag on, `gles-software` GL-used **N=1** | **135** (final binary run) | +54 over the same app GL-unused |
| flag on, `gles-software` GL-used **N=3** concurrent | **138 / 134 / 134** | ≈ 406 MiB charged across 3 sessions; no per-session growth with N |

Notes: the GL-used delta includes the 1080×2400 screenshot buffer and its
readback copy (≈ 10 MB each) and the ~31 MB of ANGLE+ICD pages mapped
(`memory.peak` charges file pages to the cgroup; the 65 MB provider is shared
page cache on the host, so the *host* marginal cost of N sessions is below N × the
charged number — not separately measured). Per-launch cost with the flag on:
provider verify **81 ms** for 65 MB staged (hardlinked names hashed once; it was
175 ms with the extra name before the inode dedupe, 105 ms in GLSPIKE3's
layout); flag off: none. `egl-probe`: first `eglInitialize` (Vulkan device
creation) 6.7 ms measured in-app, `eglCreateContext` 0.1 ms (lazy), first GPU
readback of a 64×64 PNG bitmap 54 ms (`probe3/`).

## 7. What changed

| file | change |
|---|---|
| `tools/art-gles.sh` | ICD also hardlinked at `lib64/vulkan.<tag>.so` |
| `crates/acos-art/src/gles.rs` | `load()` requires both ICD names; `icd_soname_path()`; `verify()` hashes each inode once; 3 tests |
| `crates/acos-art/src/props.rs` | test manifest carries the second ICD name |
| `crates/acos-art/guest/java/dev/acos/art/LaunchMain.java` | flag-gated `register_android_opengl_jni_{EGL14,EGL15,GLES20}` |
| `fixtures/apps/egl-probe/`, `scenarios/egl-probe.yaml`, `tools/build-apps.sh` | the diagnostic EGL probe app + scenario |
| `fixtures/apps/a11y-probe/.../A11yProbeService.java`, `scenarios/gles-software.yaml` | GPU readback instead of software-canvas draw; (iii) explicit opt-in skip; exact-pixel assertion for (ii) |

## 8. Repro

```sh
export ACOS_FONT_DIR=/home/Qui/Projects/acos/.cache/boot/android-34/fonts
tools/build-apps.sh                      # (or ACOS_APP_ONLY="egl-probe a11y-probe counter")
# re-stage the provider with the second ICD name (same args as GLSPIKE2 §8)
tools/art-gles.sh --tag angle-swiftshader --egl-tag angle --vulkan-tag pastel \
  --vulkan-icd /home/Qui/.cache/acvm/mem-r1/img/vulkan.pastel.so \
  --from-emulator-vendor ~/Android/Sdk/system-images/android-34/google_apis/x86_64/vendor.img …
tools/art-runtime/run-exclusive.sh --task GLSPIKE4 -- tools/guarded.sh --mem 10G --time 400 -- \
  results/handoff/GLSPIKE4/run-arm.sh probe scenarios/egl-probe.yaml ACOS_GUEST_GLES=angle-swiftshader
tools/art-runtime/run-exclusive.sh --task GLSPIKE4 -- tools/guarded.sh --mem 14G --time 580 -- \
  results/handoff/GLSPIKE4/run-all.sh     # off gates + unused + GL-used N=3
```

## 9. Evidence index

`key-lines.txt` (extracts) · `probe1/` (before) · `probe2/`, `probe3/` (after) ·
`gles-sw/`, `gles-sw2/` (the (iii) crash, ICD fixed) · `gles-prod/`, `final-gles/`
(gles-software pass, product path) · `off-*`, `final-off-*`, `on-counter/`, `gl3-1..3/`
(gates + memory) · `gaten-on/`, `gaten-off/` · `run-arm.sh`, `run-all.sh`.
Each run dir keeps `verdict.json`, `steps/`, `logs/logcat.txt`,
`art-frames/session-exit.json`, `launcher.stderr.log` (frames/tree dumps pruned).

## 10. Environment / cleanup

Peak host RAM ≈ 15 GiB of 60 (3 concurrent sessions + build); disk 81 % of `/`
throughout (stop line 85 %). Deleted at the end: `.cache/glspike4-target`,
`.cache/g4r-*`, `.cache/lv`, `.cache/prov`, `.cache/scratch-sysroot`, the
`fixtures/build` copy. Kept: the re-staged shared provider
`<sysroot>/gles/angle-swiftshader/` and this directory.
