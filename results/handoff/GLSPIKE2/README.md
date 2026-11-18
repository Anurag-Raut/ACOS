# GLSPIKE2 — ANGLE on SwiftShader's CPU Vulkan ICD in the guest: what it cost and where it stops

Branch `glspike2` (resumed from the WIP commit `2bfdab62`, base `01bbf963` =
GLSPIKE on `a11ypace 73879689`). Worktree
`.t3/worktrees/acos/t3-0a36d654-309dea7bb20cf030`, 2026-11-18. Private
`ACOS_RUNTIME_DIR`/`ACOS_SESSION_SOCKET` under `.cache/glspike2-run`,
`ACOS_VIEW_TEST_DIR=$PWD/.cache/glspike2-lv`, every ART run through
`tools/art-runtime/run-exclusive.sh` + `tools/guarded.sh`, release binaries in
`.cache/glspike2-target`.

**Measurement spike: `ACOS_GUEST_GLES` is OFF by default and the default
behaviour is unchanged** (§5: `props print` byte-identical with the flag off,
`cargo test -p acos-art --lib` 195 passed / 0 failed).

## 1. Status

| Brief item | State |
|---|---|
| 1. guest-side libs from an official image, URL/build id/licence/sha256 | **PARTIAL** — bytes are SwiftShader's CPU Vulkan ICD built for Android 34/bionic x86_64, Apache-2.0, sha256 `7af4f7c9…`, taken from the **local** acvm image the previous agent used; the upstream Cuttlefish/AOSP artifact is **not** pinned here (§4.1) |
| 2. loader finds the ICD (`ro.hardware.vulkan`), ANGLE uses it, AHardwareBuffer accepted | **FAILS at "ANGLE uses it"** — the loader side works (property + `hw/vulkan.<tag>.so` staged at the path the image's `libvulkan.so` searches), but staging the ICD leaves the guest at exactly GLSPIKE's wall: `EGL_NOT_INITIALIZED`. The ICD was never observed mapped in the guest (§4.2). AHardwareBuffer acceptance is therefore **not reached** |
| 3. prove on ART: wrapped a11y screenshot drawn, PNG→HARDWARE drawn, gaten past EGL | **NOT PROVED** — no pixels: hwui dies before a context exists (§3) |
| 4. RSS/PSS N=1/N=10, first `eglInitialize`, per-frame, unused-flag cost 0 | **DONE where reachable** — N=3 per arm (§6); first successful `eglInitialize` and per-frame cost are unreachable (no init, no frame); unused-flag cost = 0 within noise |
| default behaviour unchanged | **PASS** with one pre-existing exception that is *not* this change (§5) |
| fix the WIP's real bug | **DONE** — `ro.hardware.egl` was the provider *directory name*; it is now the manifest *tag* (§2) |

## 2. The bug the WIP left, and the fix

`ACOS_GUEST_GLES=angle-swiftshader` staged `lib64/egl/libEGL_angle.so` (the
manifest tag is `angle`) but wrote `ro.hardware.egl=angle-swiftshader` into the
per-run property table, because `props.rs` used the *provider name*. The image's
loader appends that property to the library name
(`frameworks/native/opengl/libs/EGL/Loader.cpp`, `attempt_to_load_system_driver`
→ `findLibrary("/vendor/lib64/egl", "libEGL_<prop>")`), so it looked for
`libEGL_angle-swiftshader.so`, found nothing and aborted:

```
?/libEGL: assertion failed: !hnd couldn't find an OpenGL ES implementation, make
          sure one of persist.graphics.egl, ro.hardware.egl and ro.board.platform is set
```

Measured with the same binary, same APK, same sysroot and both providers side
by side: `angle` (dir name == tag) got past the loader, `angle-swiftshader` did
not. After the fix (`gles::selected_tags()` resolves the provider once and
returns the manifest tag plus the ICD tag):

```
$ ACOS_GUEST_GLES=angle-swiftshader .cache/glspike2-target/release/acos-art props print
acos.guest.gles=angle
acos.guest.gles.vulkan=pastel
ro.hardware.egl=angle
ro.hardware.vulkan=pastel
```

Test: `props::tests::the_gles_properties_are_the_manifest_tag_and_its_vulkan_icd`
(flag off leaves both properties unset; flag on serves `angle`/`pastel` for the
`angle-swiftshader` directory). This is the test that fails before the fix and
passes after it.

## 3. The measured ladder (one step past GLSPIKE, then the same wall)

| Arm | hwui/libEGL/ANGLE | Evidence |
|---|---|---|
| flag **off** (default) | `couldn't find an OpenGL ES implementation` → SIGABRT (unchanged) | `off/` |
| `ACOS_GUEST_GLES=angle` (GLSPIKE's arm: driver, no ICD) | loader **loads** `/vendor/lib64/egl/libEGL_angle.so`; the fatal is gone; ANGLE returns `EGL_NOT_INITIALIZED` | `.cache/probe/verify2` (same as `results/handoff/GLSPIKE/memory/gl-touched`) |
| `ACOS_GUEST_GLES=angle-swiftshader` (driver **+** `lib64/hw/vulkan.pastel.so`, `ro.hardware.vulkan=pastel`) | **identical**: `EGL_NOT_INITIALIZED` | `on-angle-swiftshader/`, `memory/gl-touched/1..3` |

The stop line, verbatim from the guest's own logd (`on-angle-swiftshader/`):

```
I/AcosA11yProbe: gles_draw start=1 provider=angle
W/OpenGLRenderer: Failed to choose config with EGL_SWAP_BEHAVIOR_PRESERVED, retrying without...
W/OpenGLRenderer: Failed to initialize 101010-2 format, error = EGL_NOT_INITIALIZED
?/OpenGLRenderer: assertion failed: mEglContext == EGL_NO_CONTEXT Failed to create
                 context, error = EGL_NOT_INITIALIZED
Fatal signal 6 (SIGABRT) ... Thread: 26 "GrallocUploadTh"
```

So: **libEGL's loader is solved; the initializing driver is still the wall, and
the ICD alone does not break it.**

## 4. The ICD: what was staged, and why it did not help

### 4.1 Provenance (and the honest gap)

The provider installed at `<sysroot>/gles/angle-swiftshader/` (`tools/art-gles.sh
--egl-tag angle --vulkan-icd <file> --vulkan-tag pastel`):

| file | size | sha256 |
|---|---|---|
| `lib64/egl/libEGL_angle.so` (hardlinked at `lib64/`) | 705,784 | `e25a2eb569bd8c71263ed0dbb11d94d7a7beadcb88704ceaa14746f0fa595ca9` |
| `lib64/egl/libGLESv1_CM_angle.so` | 29,936 | `7d6f7af2131b4e66feac93f57b7b77a56c9bca98d1d7b0e19ac37494fdc74198` |
| `lib64/egl/libGLESv2_angle.so` | 8,402,728 | `6a65e517c52b7234cb5cf24d4cb51785626e7bb26777936d28c6debf5745d7e0` |
| **`lib64/hw/vulkan.pastel.so`** (the ICD) | **23,380,960** | **`7af4f7c905c72b28405e6deebc06e78ce692b6bc79f086f445d26b01c7cef3b9`** |

* ANGLE libs: the Android 14 SDK system image
  `system-images/android-34/google_apis/x86_64`'s `vendor.img` (12-Jul-2024),
  Apache-2.0 — same bytes GLSPIKE measured.
* ICD: `ELF 64-bit LSB shared object, x86-64, for Android 34`, SONAME
  `vulkan.pastel.so`, exports `vk_icdGetInstanceProcAddr`,
  `vk_icdNegotiateLoaderICDInterfaceVersion` **and** the hw-module `HMI` data
  symbol; strings show `external/swiftshader/src/...` and `Swiftshader Pastel`,
  i.e. it really is SwiftShader's CPU Vulkan ICD packaged the way Cuttlefish
  (`--gpu_mode=guest_swiftshader`, `ro.hardware.vulkan=pastel`) uses it,
  Apache-2.0. Bytes came from the **local** image
  `/home/Qui/.cache/acvm/mem-r1/img/vulkan.pastel.so` (docker `acvm-core:v9`
  sha256 `c61eaa85e417…`, base layer *redroid* 14.0.0 userspace, 22-May-2024),
  not from a download in this session.
* **Gap for the owner:** the official Cuttlefish/AOSP artifact URL + build id is
  not pinned here. Smallest next action: `curl` the Cuttlefish/AOSP-14
  prebuilt `vulkan.pastel.so` and compare sha256 with the value above; if it
  differs, reinstall the provider (`tools/art-gles.sh`, §8) — the mechanism is
  unchanged.

### 4.2 Why staging it changed nothing (measured)

* The image's `libvulkan.so` really does search `/vendor/lib64/hw`:
  `strings` → `/odm/lib64/hw`, `/vendor/lib64/hw`, `/system/lib64/hw`,
  `vulkan.`, `ro.hardware.vulkan`, `hwvulkan module open`. The staged file sits
  at exactly that path and the property is set (§2).
* The guest sees the whole provider (`/vendor/lib64` listing =
  `libGLESv2_angle.so, libEGL_angle.so, libGLESv1_CM_angle.so, hw, egl`,
  `/vendor/lib64/hw` = `vulkan.pastel.so`) — measured from inside the guest
  (`File.list()` in a dex run under the same launcher).
* **But the ICD is never mapped**: `maps-sample.sh` samples
  `/proc/<guest pid>/maps` every ~40 ms across a whole GL session
  (`maps-sample.txt`) and sees only the *system* libraries libhwui NEEDs
  (`/system/lib64/libEGL.so`, `libGLESv2.so`, `libvulkan.so`); neither
  `/vendor/lib64/hw/vulkan.pastel.so` nor `/vendor/lib64/egl/libEGL_angle.so`
  appears in any sample. libvulkan logs no ICD-discovery error either.

Read with §3: the load/init cycle that produces `EGL_NOT_INITIALIZED` either
fails *before* ANGLE's Vulkan ICD discovery, or completes in a window shorter
than the sampler. **It is not an ICD-presence problem any more** — the property
and the file are both correct — and the next step is ANGLE's own platform/device
selection for `EGL_DEFAULT_DISPLAY` in this build (the ANGLE here contains
`external/angle/src/common/vulkan/vulkan_icd.cpp` and
`.../renderer/vulkan/android/vk_android_utils.cpp`, and its
`EGL_ANGLE_platform_angle_device_type_swiftshader` is *not supported*, i.e. it
cannot take the in-process SwiftShader shortcut; it must use the system loader).

### 4.3 The gralloc question the brief asked about — not reached, and its size

`AHardwareBuffer` from ACOS's memfd gralloc interposer is never handed to
SwiftShader, because no `VkInstance`/`VkDevice` is ever created. If the ICD is
made to load, the *next* wall is exactly the one A11YPACE/GLSPIKE predicted:
SwiftShader's `VkSwapchain`/`VkImage` import path calls the **HIDL gralloc
mapper** (`android.hardware.graphics.mapper@3.0/@4.0`, both `DT_NEEDED` of the
ICD; both present in `/system/lib64` but with no HIDL service in this guest) and
`AHardwareBuffer_lockPlanes` — which ACOS *does* interpose
(`crates/acos-art/shim/gralloc_shim.c`) while the mapper service does not exist.
Sizes if that is the gap: a mapper HIDL stand-in over the existing memfd shim is
the "gralloc5 module + EGLImage path" the graphics track sized at *weeks*; the
cheap alternative stays GLSPIKE §6.1's option 2 (drop `USAGE_GPU_SAMPLED_IMAGE`
so `wrapHardwareBuffer` refuses cleanly and apps take their software path —
days, a documented image deviation).

## 5. Default behaviour (flag OFF) — unchanged

* `props print` with the variable unset is **byte-identical** to the pre-change
  binary (`diff` of the two tables: empty).
* `cargo test -p acos-art --lib` → **195 passed, 0 failed, 1 ignored**
  (includes the new property test).
* `scenarios/a11y-screenshot-wrap.yaml` → **3/3 pass** (`off/a11y-screenshot-wrap`).
* `scenarios/a11y-probe.yaml` → **6/7**, and `scenarios/counter.yaml` → **3/4**,
  both on the *same* pre-existing failure class:
  `expected text exactly "Increment", got text "INCREMENT"` (the theme's
  all-caps label). **Pre-existing, proved**: with my two files stashed (the
  unmodified branch tip `2bfdab62`) the counter run fails identically
  (`.cache/probe/counter-base/verdict.json`), and the *older* 01bbf963 binary
  passes 15/15 on the same APK/scenario/sysroot. It is the shared sysroot's
  theme/framework state that a freshly built binary sees, not this change
  (which touches nothing on the flag-off path).

## 6. Cost (shared slots; indicative, not release timing)

Per-session peak from the run's own cgroup (`memory.peak`,
`session-exit.json` → `cgroup: … peak=NM iB`), 3 sessions per arm
(`memory/<arm>/<n>/`, one lock acquisition per session, `run-memory.sh`):

| Arm | GL touched? | peak MiB (3 samples) | median | wall ms | guest EGL outcome |
|---|---|---|---|---|---|
| `gl-touched` (`angle-swiftshader`, `gles-software.yaml`) | yes — driver loaded, `eglInitialize` fails | 69 69 70 | 69 | 482–489 | `driver_not_initialized` |
| `gl-unused-on` (`angle-swiftshader`, `counter.yaml`) | **no** | 61 66 59 | 61 | 347–367 | none |
| `gl-unused-off` (flag unset, `counter.yaml`) | no | 60 60 66 | 60 | 335–359 | none |

* **Laziness: Δ(on − off) = 0 within noise** (59–66 vs 60–66 MiB): the staged
  provider costs nothing in a session that never touches EGL — one read-only
  bind, nothing mapped. This is the property the brief asked for.
* A GL-touching session costs **69 MiB** peak *without* a working driver; that
  is the app plus the mapped driver pages, **not** a working-GL number.
* **First successful `eglInitialize` latency: not measurable** — init fails, so
  there is no success to time. The only launch-side cost that is measured is the
  provider verify: **100–107 ms for 41.6 MB** (18.3 MB without the ICD: 44 ms),
  paid once per launch, only with the flag on.
* **Per-frame cost: not reachable** (no frame is drawn).
* Disk: the provider is 32 MB on disk (23 MB of it the ICD), shared by every
  session.

## 7. Environment notes for the orchestrator

1. **Shared-sysroot shim staleness produced a false wall.** The first two arm
   runs of this session aborted at `couldn't find an OpenGL ES implementation`
   for *both* providers; the same binary, same provider, minutes later, reached
   ANGLE. The only thing that ran in between was three `acos-art run`/`launch`
   sessions, which re-stamp/rebuild `<sysroot>/shim/` (the property server).
   Nothing in the GLES code changed for the `angle` arm. Any agent measuring a
   GL/loader path on the shared sysroot should run one throwaway session first
   and check `props print` *in the guest* before recording a wall.
2. The all-caps label failure (§5) is live on `2bfdab62` for freshly built
   binaries — it will make `a11y-probe`/`counter` look red for every agent that
   rebuilds right now. Worth a fix in the theme/resource path (not this track).

## 8. Repro

```sh
export ACOS_FONT_DIR=/home/Qui/Projects/acos/.cache/boot/android-34/fonts
# 1. install the provider (writes <sysroot>/gles/angle-swiftshader/, nothing else)
tools/art-gles.sh --tag angle-swiftshader --egl-tag angle \
  --vulkan-icd /home/Qui/.cache/acvm/mem-r1/img/vulkan.pastel.so --vulkan-tag pastel \
  --from-emulator-vendor ~/Android/Sdk/system-images/android-34/google_apis/x86_64/vendor.img \
  --provider "ANGLE (Vulkan) on SwiftShader's CPU Vulkan ICD ('pastel')" \
  --version "API 34 bionic x86_64" --licence "Apache-2.0 (ANGLE, SwiftShader)" \
  --source "<Cuttlefish/AOSP-14 URL + build id, pin sha256 7af4f7c9…>"
# 2. default arm (must stay green)
results/handoff/GLSPIKE2/run-spike.sh off
# 3. the software-GLES arm (fails today at EGL_NOT_INITIALIZED)
GLES=angle-swiftshader results/handoff/GLSPIKE2/run-spike.sh on
GLES=angle-swiftshader results/handoff/GLSPIKE2/run-spike.sh gaten
# 4. cost samples (3 per arm)
for a in gl-touched gl-unused-on gl-unused-off; do
  results/handoff/GLSPIKE2/run-memory.sh $a 3
done
# 5. is the ICD ever mapped?
results/handoff/GLSPIKE2/maps-sample.sh
```

Evidence: `off/`, `on-angle-swiftshader/`, `gaten-angle-swiftshader/`,
`memory/<arm>/<n>/`, `maps-sample.txt`, `run-spike.sh`, `run-memory.sh`,
`maps-sample.sh` (each run dir has the verdict, `art-frames/session-exit.json`
with the guest's `logd_tail`, logcat, launcher stderr).

## 9. Environment / cleanup

Builds in `.cache/glspike2-target` (release), peak host RAM 8 GiB used of 60,
disk 78 % of `/` (700 GB used) at the end. Deleted at the end: the target dir,
`.cache/probe`, the run dirs, and the `fixtures/build` hardlink copy. Kept: the
installed provider under `<sysroot>/gles/angle-swiftshader/` (and the older
`angle`/`emulation` providers GLSPIKE installed). **Not** deleted:
`/home/Qui/Projects/acos/.cache/glspike-scratch` (334 MB of Mesa source and the
extracted `vendor.img`) — it is GLSPIKE's scratch in the *main* checkout's
`.cache`, outside this worktree; safe to remove.
