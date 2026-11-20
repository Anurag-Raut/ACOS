# GLSPIKE3 — why ANGLE ends in `EGL_NOT_INITIALIZED` without ever mapping the staged ICD

Branch `glspike3` from `glspike2` (`f5a0de79`). Worktree
`.t3/worktrees/acos/t3-8c04f655-c7948af7d2b7cf6d`, 2026-11-18. Private
`ACOS_RUNTIME_DIR`/`ACOS_SESSION_SOCKET` under `.cache/glspike3-run`,
`ACOS_VIEW_TEST_DIR=$PWD/.cache/lv`, every ART run through
`tools/art-runtime/run-exclusive.sh` + `tools/guarded.sh`, release binaries in
`.cache/glspike3-target`, fixtures freshly built with `tools/build-apps.sh`
(`fixtures/build/apps/a11y-probe.apk` exists again — see §5).

**Measurement only. `ACOS_GUEST_GLES` is OFF by default and no GL/provider code
changed in this spike.** The only files this branch adds are evidence, the maps
sampler, the EGL client probe (§4.3) and this report. Default behaviour: §5.

## 0. Answer in one paragraph

ANGLE *is* loaded and driven (`/vendor/lib64/egl/libEGL_angle.so` +
`libGLESv2_angle.so` + `libGLESv1_CM_angle.so` really are mapped in the guest app
process, 350 ms into the session) and the failure is **not** at the EGL loader
any more. The abort comes from hwui's `HardwareBitmapUploader` thread
(`GrallocUploadThread`), whose own `EglManager` got a **successful
`eglInitialize` and a successful `eglChooseConfig`** and then got
`EGL_NOT_INITIALIZED` back from a **single** call: `eglCreateContext`. Between
those two calls ANGLE never asks the system Vulkan loader for anything —
`/vendor/lib64/hw/vulkan.pastel.so` is never mapped in any sample of any guest
process, and neither `vkCreateInstance` nor the Android loader's
`hw_get_module(...)`/`OpenHAL()` can have run (had they run, the file would be
mapped; a *failed* dlopen maps nothing, but the property the lookup needs is
known-good in the same process, §3). So the missing piece is **ANGLE's own
Vulkan init step between `eglInitialize` and `eglCreateContext`**: this ANGLE
reports the display as initialized to the Google loader and then refuses to
create a context because its Vulkan renderer/device was never brought up. The
gralloc mapper (HIDL/AIDL) is *behind* that wall, not at it: with no
`VkInstance` there is no `VkImage`/`AHardwareBuffer` import to fail.

## 1. Status

| Brief item | State |
|---|---|
| reproduce the arm with full logs / raised caps | **DONE** — `gl-arm/` (fresh fixtures, flag on); the wall is identical to GLSPIKE2 (§2) |
| VK_LOADER_DEBUG / ANGLE debug props | **N/A + measured** — the guest env is cleared except an allowlist (`launch.rs:2312`), so `LD_DEBUG`/`VK_LOADER_DEBUG` cannot be injected without a code change; and this ANGLE build has **no** `ANGLE_DEFAULT_PLATFORM`/`debug.angle.*` knobs except `debug.angle.overlay` (`strings` in §3.3) — no ERR/WARN logging in the release build. Compensated with maps sampling + image-source analysis (§3, §4) |
| strace on the guest | **NOT FEASIBLE** — `strace` is not installed on the host and `kernel.yama.ptrace_scope=1`; the guest is a grandchild of `acos-art` inside `bwrap`, so an attach from outside is refused. Replaced by `/proc/<pid>/maps` deltas at 10 ms (§2.2), which answers the same question (was the file opened by the guest's linker) |
| does ANGLE call `vkCreateInstance`? | **NO** (evidence chain, §3.4) |
| does libvulkan look for the hwvulkan HAL, where? | **Static, yes, and it never got there**: `libvulkan.so` DT_NEEDEDs `libhardware.so`, strings `ro.hardware.vulkan`, `vulkan.`, `hwvulkan module open`, `/vendor/lib64/hw`; the HAL lookup happens in `OpenHAL()` on the first *device* enumeration, i.e. inside `vkCreateInstance`'s path |
| is ANGLE on a non-Vulkan backend / missing feature lib / namespace? | **Ruled out for the namespace** (the driver loads through the default namespace; all three ANGLE libs mapped) and **not decided for the backend** — needs the EGL-attribute probe (§4.3), which is built but blocked on one guest-side JNI registration (§4.3) |
| gralloc mapper before Vulkan? | **No** — not reached (§4.2) |
| small fix that makes `eglInitialize` succeed | **NONE FOUND in the box**; the next call already succeeds, the wall is `eglCreateContext`, and no prop/symlink/namespace change is missing (the loader path is complete and proven) |
| acceptance arms (i)(ii)(iii) + memory N=1/N=3 | **NOT REACHED** — no EGL context, hence no pixels; measuring memory of an arm that cannot draw would not be decision-relevant (§6) |
| note: all-caps `INCREMENT` on fresh builds | **CHECKED, does not reproduce here** (§5.2) — `counter` 24/24 and `a11y-probe` 15/15 on this branch, flag off, freshly built fixtures |
| default behaviour unchanged | **PASS** — no provider/props code touched at all; `cargo test -p acos-art --lib` green (§5.1) |

## 2. The arm, reproduced on this branch

Fresh release build from `f5a0de79`, freshly built fixtures, shared sysroot
provider `<sysroot>/gles/angle-swiftshader/` (installed by GLSPIKE2 §8, bytes
verified by the launcher: `gles: provider=angle-swiftshader tag=angle
vulkan=pastel … staged 41657856 bytes`, `gl-arm/launcher.stderr.log:18`).

`gl-arm/verdict.json`: **0/1 steps, `GUEST_EXITED` (`status 134`)** at `launch`
— the app aborts before the tree is bound. The guest's own tail
(`gl-arm/art-frames/session-exit.json` → `logd_tail`, verbatim):

```
W/OpenGLRenderer: Failed to choose config with EGL_SWAP_BEHAVIOR_PRESERVED, retrying without...
W/OpenGLRenderer: Failed to initialize 101010-2 format, error = EGL_NOT_INITIALIZED
?/OpenGLRenderer: assertion failed: mEglContext == EGL_NO_CONTEXT Failed to create context, error = EGL_NOT_INITIALIZED
F/: Fatal signal 6 (SIGABRT), code -6 (SI_TKILL) … Thread: 26 "GrallocUploadTh"
```

`gl-arm/art-frames/native-crash.txt` records the abort at
`__android_log_assert+452` in `/data/shim/libartshim.so` on thread
`GrallocUploadTh` — i.e. hwui's `LOG_ALWAYS_FATAL`, not a driver fault.

The artifact logcat (`gl-arm/logs/logcat.txt`) is a **prefix**, not a tail: 90
records ending at
`V/GraphicsEnvironment: ANGLE GameManagerService for dev.acos.fixtures.a11yprobe: false`
/ `Neither updatable production driver nor prerelease driver is supported.`
The GL/EGL records live in `logd_tail` instead, because the logd snapshot is
fetched when the session dies. (GLSPIKE2 §3's quotes came from there too; the
"raise the artifact line cap" the brief asked for is unnecessary — the cap is
50 000 entries and this run used 179. Raising the cap would not have added a
line.)

## 3. The evidence chain

### 3.1 Where the failure is raised (image's own hwui, `android-14.0.0_r75`)

`base/libs/hwui/HardwareBitmapUploader.cpp:124` starts a thread named
`GrallocUploadThread`; `onBeginUpload()` (line 151) calls **its own**
`EglManager::initialize()` (line 154) — this upload thread's context is what
hwui uses to import the wrapped `HardwareBuffer`. The first HARDWARE-bitmap use
in this scenario is exactly that (`gles_draw kind=hwbuffer_screenshot`), which
is why the crash is on `GrallocUploadTh`.

`base/libs/hwui/renderthread/EglManager.cpp:105-135`:

```cpp
mEglDisplay = eglGetDisplay(EGL_DEFAULT_DISPLAY);
LOG_ALWAYS_FATAL_IF(mEglDisplay == EGL_NO_DISPLAY, "Failed to get EGL_DEFAULT_DISPLAY! err=%s", …);
LOG_ALWAYS_FATAL_IF(eglInitialize(mEglDisplay, &major, &minor) == EGL_FALSE,
                    "Failed to initialize display %p! err=%s", mEglDisplay, eglErrorString());
…
initExtensions();          // eglQueryString(EGL_EXTENSIONS)
loadConfigs();             // → the "EGL_SWAP_BEHAVIOR_PRESERVED" warning is emitted *inside* here
createContext();           // → "Failed to create context, error = %s"
```

Measured facts against that source, in order:

1. **neither `Failed to get EGL_DEFAULT_DISPLAY` nor `Failed to initialize
   display` appears** → `eglGetDisplay` returned a display and **`eglInitialize`
   returned `EGL_TRUE`**;
2. `initExtensions()` returned (no `Missing required extension
   EGL_KHR_swap_buffers_with_damage` fatal, which ANGLE would also trip if it
   had no extensions at all);
3. `loadConfigs()`: the **8-bit config with `EGL_SWAP_BEHAVIOR_PRESERVED`
   failed, the retry without it succeeded** — that warning is only printed when
   the second `eglChooseConfig` succeeds; had it failed too, the following
   `LOG_ALWAYS_FATAL("Failed to choose config, error = %s")` would have fired.
   **So EGL configs exist for this display**;
4. `load1010102Config` failed and the warning printed `eglErrorString()` =
   `EGL_NOT_INITIALIZED` — that string is hwui's `eglGetError()` **stale**
   value, not this call's error (the call simply returned
   `EGL_NO_CONFIG_KHR`);
5. `createContext()` → `eglCreateContext` returned `EGL_NO_CONTEXT` **with
   `EGL_NOT_INITIALIZED`** → fatal.

So: the same display both *is* initialized enough to enumerate and choose
configs and *is not* initialized enough to create a context. That is the whole
mystery, and it is a property of the driver (ANGLE), not of hwui: hwui only
reports two consecutive EGL results.

### 3.2 ANGLE is really loaded (correcting GLSPIKE2 §4.2)

`maps-watch.txt` — dense sampler (§7), every process whose `/proc/<pid>/maps`
mentions the app, first-appearance timestamps:

```
pid=393826 cmdline=/system/apex/com.android.art/bin/dalvikvm64 -Ximage:/data/img/boot.art
           -Xnorelocate -cp /data/app/launch-guest/classes.dex dev.acos.art.LaunchMain
           /data/app/a11y…                       ← the app process
  0.180 pid=393826 first libvulkan.so
  0.180 pid=393826 first libhardware.so
  0.180 pid=393826 first libgraphicsenv.so
  0.180 pid=393826 first libEGL.so
  0.180 pid=393826 first libGLESv2.so
  0.180 pid=393826 first libvndksupport.so
  0.530 pid=393826 first libEGL_angle.so          ← the staged driver, mapped
  0.530 pid=393826 first libGLESv2_angle.so
  0.530 pid=393826 first libGLESv1_CM_angle.so
   (vulkan.pastel.so: never, in any pid, in any sample)
```

Two corrections to the GLSPIKE2 report follow from this:

* **`libvulkan.so` being mapped proves nothing.** `libhwui.so` has
  `DT_NEEDED libvulkan.so` and `libvulkan.so` has
  `DT_NEEDED libhardware.so` (`readelf -d` on the sysroot's copies), so
  libvulkan + libhardware + libgraphicsenv are mapped 180 ms into the process
  **before any EGL call**. GLSPIKE2 §4.2 read "libvulkan is mapped" as evidence
  that ANGLE had reached the loader; it is not.
* **the ANGLE libs are mapped.** GLSPIKE2's sampler never saw
  `libEGL_angle.so` because it matched only three path patterns and sampled by
  `ps … /dalvikvm64/` (its `maps-sample.txt` holds 3 counted lines: 712×
  libvulkan, 712× libEGL, 534× libGLESv2 — i.e. one process's worth of
  repeated hits). The driver is loaded and **stays** loaded.

### 3.3 What this ANGLE build can and cannot be told to log or do

`readelf -d` on `libEGL_angle.so`: NEEDED = `libGLESv2_angle.so, liblog.so,
libm.so, libnativewindow.so, libc.so, libdl.so` — **no `libvulkan.so`**: ANGLE
dlopens the system loader at runtime (`angle::vk::OpenLibVulkan`, present in the
symbol table) and tries `libvulkan.so.1` **first**, then `libvulkan.so` (both
strings present in `libGLESv2_angle.so`). Neither name is versioned in bionic,
so the `.so.1` attempt is expected to fail and the plain name must carry it.

Strings of `libGLESv2_angle.so` relevant to the question: `libvulkan.so.1`,
`libvulkan.so`, `angle::vk::OpenLibVulkan`, `ScopedVkLoaderEnvironment`
(`setICDEnvironment`, i.e. `VK_ICD_FILENAMES`/`VK_DRIVER_FILES` support),
`ChoosePhysicalDevice`, `external/angle/src/common/vulkan/vulkan_icd.cpp`,
`renderer/vulkan/android/...`, `debug.angle.overlay` — and **no
`ANGLE_DEFAULT_PLATFORM`**, i.e. this build honours no environment/`debug.*`
switch for backend selection or logging. Its `WARN()`/`ERR()` output is
compiled out of the release image. That is why the direct answer needs an EGL
*client* (an explicit `EGL_PLATFORM_ANGLE_TYPE` attribute) rather than a
property (§4.3).

### 3.4 Does ANGLE call `vkCreateInstance`? — No

The chain that would have to run is: `eglCreateContext` → ANGLE
`DisplayVk::createContext` → `RendererVk` present (created during
`initialize`) → if it is *not* created, ANGLE returns `EGL_NOT_INITIALIZED` and
never needs Vulkan at all. Observables:

* the ICD file is **never mapped** — and if ANGLE's Vulkan instance existed,
  the Android loader's `OpenHAL()` would have `dlopen`ed
  `/vendor/lib64/hw/vulkan.pastel.so` (hw_get_module maps the module to read its
  `HMI`), so *something* in the maps would change;
* the property that lookup consumes is known-good **in the same process**
  (GLSPIKE2 §2 `props print`; and the framework's own GraphicsEnvironment lines
  in this run read the ANGLE selection table successfully);
* no `EGL_NOT_INITIALIZED` from `eglInitialize` (which is where a
  "no Vulkan device" would surface if the renderer were created eagerly).

Conclusion: **`vkCreateInstance` is never called.** The divergence is inside
ANGLE: `eglInitialize` returns `EGL_TRUE` while the display's Vulkan renderer is
never made current/valid, and the first `eglCreateContext` on that display
reports `EGL_NOT_INITIALIZED`. (Same shape as GLSPIKE's `angle`-without-ICD arm,
which is consistent: there, a *missing* ICD and a *present but never-opened*
ICD produce the same EGL error — which is exactly why staging the ICD changed
nothing.)

## 4. The three remaining hypotheses, with what decides each

### 4.1 "ANGLE picked a non-Vulkan backend" — plausible, undecided

With no `ANGLE_DEFAULT_PLATFORM` and no `debug.angle.*` in the build, the
backend is chosen from the EGL display attributes, and `EGL_DEFAULT_DISPLAY`
maps to ANGLE's compile-time default. This AOSP-shipped ANGLE contains the
Vulkan renderer (`ContextVk`, `RendererVk`) **and** the GL renderer symbols —
the *one* decisive call is `eglGetPlatformDisplayEXT(EGL_PLATFORM_ANGLE_ANGLE,
dpy, {EGL_PLATFORM_ANGLE_TYPE_ANGLE, EGL_PLATFORM_ANGLE_TYPE_VULKAN_ANGLE})`
followed by `eglInitialize`/`eglCreateContext` on that display. If the Vulkan
display initializes and maps the ICD, the wall is ANGLE's *default* selection;
if it fails too, the wall is the Vulkan path itself and the gralloc question
(§4.2) becomes live.

### 4.2 "gralloc mapper before Vulkan" — measured: not reached

Nothing Vulkan happens (§3.4), so `AHardwareBuffer` is never handed to the ICD,
so neither `android.hardware.graphics.mapper@3.0/@4.0` (both DT_NEEDED of the
ICD) nor `AHardwareBuffer_lockPlanes` (which ACOS *does* interpose,
`crates/acos-art/shim/gralloc_shim.c`) is exercised. It remains the **next**
wall after §4.1/§4.3 is solved, and GLSPIKE2's sizing stands (mapper HIDL
stand-in: weeks; or days for the documented image deviation that drops
`USAGE_GPU_SAMPLED_IMAGE`).

### 4.3 "missing lib / namespace / feature lib" — the honest blocker

* Namespace: ruled out for the driver (all three ANGLE libs map through the
  default namespace, which is what `gles.rs`'s double hardlink
  (`lib64/` + `lib64/egl/`) exists for). Ruled out for the ICD's *path*: the
  file sits at `/vendor/lib64/hw/vulkan.pastel.so` inside the guest (verified by
  the guest itself, `dir /vendor/lib64/hw names=vulkan.pastel.so` in the EGL
  probe run, §7.2) and `libvulkan.so` searches exactly `/vendor/lib64/hw`
  (`strings`).
* Feature lib: `libGLESv2_angle.so` NEEDs only `liblog, libm, libnativewindow,
  libc, libdl`; the guest maps all of them at start. Nothing is reported missing
  by the linker (a missing `DT_NEEDED` would abort the *dlopen* of the ANGLE
  libs, which succeed).
* **The blocker is instrumentation, not evidence**: the exact ANGLE call that
  returns `EGL_NOT_INITIALIZED` is only visible from a controlled client, and
  the guest cannot run one yet —
  `android.opengl.EGL14`'s native methods are **not registered** in the ACOS
  guest: the probe (`results/handoff/GLSPIKE3/eglprobe/`, built and committed)
  dies at `EGL14.<clinit>` → `UnsatisfiedLinkError: No implementation found for
  void android.opengl.EGL14._nativeClassInit()`. The image's
  `libandroid_runtime.so` exports `register_android_opengl_jni_EGL14`
  (`nm -D`), and the guest launcher registers a fixed allowlist
  (`crates/acos-art/guest/java/dev/acos/art/LaunchMain.java` +
  `AppMain.java`, `String[] names = {…}`) that omits it. Adding one name to that
  list is the smallest possible next action (§8).

## 5. Default behaviour (flag OFF)

### 5.1 Unchanged

* **No provider, props or gles code was touched by this spike** (`git diff
  --stat f5a0de79..HEAD -- crates/` is empty), so the flag-off path is
  byte-identical by construction.
* `cargo test -p acos-art --lib` (release, shared lock):
  `test result: ok. 195 passed; 0 failed; 1 ignored; 0 measured; 0 filtered out`.

### 5.2 The GLSPIKE2 all-caps finding does **not** reproduce here

GLSPIKE2 §5/§7.2 reported `counter` 3/4 and `a11y-probe` 6/7 on fresh builds
with `expected text exactly "Increment", got text "INCREMENT"`. On this branch,
flag off, with **freshly built fixtures** (`tools/build-apps.sh`, all 112 APKs
including `a11y-probe.apk`, which the worktree's copy was missing — see §5.3):

```
results/handoff/GLSPIKE3/off-counter/verdict.json      pass   24/24
results/handoff/GLSPIKE3/off-a11y-probe/verdict.json   pass   15/15
```

**Not reproduced.** The relevant difference from GLSPIKE2's run is the fixture
build: mine came from a full `tools/build-apps.sh` on this branch, and the
APKs' theme/resource state is therefore rebuilt, not inherited. It was **not**
checked against the modules commit `45a1feab` (out of the time box); the exact
command is in §8.4. Note also that GLSPIKE2's own "pre-existing" claim rested on
`2bfdab62` *without* rebuilt fixtures — the same confound.

### 5.3 Fixture trap worth knowing (this cost one cycle)

`cp -al /home/Qui/Projects/acos/fixtures/build fixtures/build` (BRIEF_COMMON)
gave a tree **without** `apps/a11y-probe.apk` (main's build tree was incomplete),
so the first two arm runs died at `APK_IO: APK not found` and produced no
evidence. `tools/build-apps.sh` fixes it in ~40 s and must be run before the
arms — as the brief says.

## 6. What was deliberately not measured

Memory N=1/N=3 per session (GL-used vs unused) is meaningless until an EGL
context exists: the arm cannot draw, and GLSPIKE2 already measured the two
reachable points (69 MiB peak for a GL-touching session that fails init, Δ(on −
off) = 0 within noise for a session that never touches EGL). No new numbers are
claimed here.

## 7. Evidence index

| file | what it is |
|---|---|
| `maps-watch.txt` | 60 s dense (`sampler` lines and guest pids `/system/apex/.../dalvikvm64` only; host/shell pids the `cmdline` filter over-matched were dropped) — 93 guest pids, 561 first-appearance lines, **0** occurrences of `vulkan.pastel` |
| (raw) | 60 s dense `/proc/<pid>/maps` delta sampler over the GL arm: app pid, per-lib first-appearance time, and the per-pid union at the end (`gl-libs=` count) — the "ICD is never mapped" fact |
| `gl-arm/` | the arm run: `verdict.json` (`GUEST_EXITED`, 0/1), `art-frames/session-exit.json` (`logd_tail`, the three EGL lines + the abort), `art-frames/native-crash.txt` (thread, signal, `__android_log_assert`), `logs/logcat.txt` (logd prefix), `launcher.stderr.log:18` (provider verification line) |
| `off-counter/`, `off-a11y-probe/` | the flag-off reproductions of §5.2 |
| `mapswatch.py` | the sampler (copied here so the measurement is reproducible) |
| `eglprobe/` | the EGL client probe (§4.3) — `EglProbe.java`, `classes.dex`, `build.sh`, and `run1.log` with the `UnsatisfiedLinkError` that blocks it |

## 8. Repro

```sh
export ACOS_FONT_DIR=/home/Qui/Projects/acos/.cache/boot/android-34/fonts
# 0. fixtures (required: the hardlink copy can lack a11y-probe.apk)
tools/build-apps.sh
# 1. the GL arm with the maps sampler + the flag-off arms (one lock acquisition)
tools/art-runtime/run-exclusive.sh --task GLSPIKE3 -- \
  tools/guarded.sh --mem 10G --time 900 -- bash -c \
  '.cache/run-gl-arm.sh gl; .cache/run-gl-arm.sh off'      # scripts in .cache/, copied in §7
# 2. press the missing link (30-60 min): register EGL14's natives in the guest
#    launch allowlist, then
tools/art-runtime/run-exclusive.sh --task GLSPIKE3 -- \
  tools/guarded.sh --mem 8G --time 300 -- env -u ACOS_HOME ACOS_GUEST_GLES=angle-swiftshader \
  .cache/glspike3-target/release/acos-art run --dex results/handoff/GLSPIKE3/eglprobe/classes.dex \
  --class EglProbe --timeout 120
# 3. (same probe, once it runs) read EGL_DISPLAY_EXTENSIONS + the per-call errors,
#    and the maps delta after eglInitialize / eglCreateContext — that answers
#    "which backend, and is the ICD mapped the moment Vulkan is asked for".
# 4. the modules check GLSPIKE2 left open:
git checkout 45a1feab && tools/build-apps.sh && <run counter + a11y-probe as in §1>
```

## 9. How far it got, and what is missing (sized)

* **Proven (new)**: the driver loads and stays loaded; `eglInitialize` and
  `eglChooseConfig` succeed on the ANGLE display; the abort is one
  `eglCreateContext` returning `EGL_NOT_INITIALIZED`; the ICD is never mapped;
  therefore `vkCreateInstance` never runs; therefore gralloc is not the current
  wall. GLSPIKE2's "libvulkan mapped" and "ANGLE never mapped" observations are
  both corrected.
* **Missing piece**: one guest-side JNI registration
  (`register_android_opengl_jni_EGL14`, already exported by the image's
  `libandroid_runtime.so`) so the committed EGL probe can run — **30-60
  minutes**. It then returns, in one run: the EGL extension list, the per-call
  error at each step, whether `EGL_PLATFORM_ANGLE_TYPE_VULKAN` initializes, and
  whether `vulkan.pastel.so` maps the moment ANGLE is asked for a Vulkan
  display. **That single run decides between §4.1 (backend selection: hours to
  fix, an EGL attribute/profile decision and a props/`ACOS_GUEST_GLES` knob) and
  §4.2 (Vulkan path broken: days-weeks, the mapper stand-in or the image
  deviation).**
* Not attempted inside the box, and out of scope by the brief: implementing a
  gralloc mapper HAL, and any new GL code.

## 10. Environment / cleanup

Built with `CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0
CARGO_INCREMENTAL=0` into `.cache/glspike3-target`; peak host RAM ≈ 8 GiB of 60;
disk 79 % of `/` (703 GB used) — under the 85 % stop line throughout. Deleted at
the end: `.cache/glspike3-target`, `.cache/glspike3-run`, `.cache/lv`,
`.cache/eglprobe/{src,classes}`, `.cache/eglprobe/run1.log`'s build inputs,
`.cache/build.log`/`.cache/build-apps.log`/`.cache/cargo-test.log`, the
run-exclusive data dirs this spike created, and the `fixtures/build` copy.
Kept: the sysroot provider `<sysroot>/gles/angle-swiftshader/` (GLSPIKE2's, not
mine to delete) and `results/handoff/GLSPIKE3/` (evidence + the probe).
