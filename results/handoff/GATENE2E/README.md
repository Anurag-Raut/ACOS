# GATENE2E — the Gaten floating bubble, end to end (v0.4.0 exit criterion)

Branch `gatene2e` from `glship` (`f0e15daa`, software GL default-on), 2026-11-18.
Private `ACOS_RUNTIME_DIR`/`ACOS_SESSION_SOCKET` per run (`.cache/ger-<tag>`),
every ART run through `tools/art-runtime/run-exclusive.sh` + `tools/guarded.sh`,
release binaries in `.cache/gatene2e-target`, fixtures from `tools/build-apps.sh`
+ `tools/build-dogfood-apps.sh`. `~/Projects/gaten` was never touched (the APK is
`.cache/gaten/gaten-dev-f344272-x86_64-release-unsigned.apk`; its source at
`.cache/gaten/src-dev` was only read).

## 0. Answer

`scenarios/gaten-bubble-overlay.yaml` is now an honest end-to-end check of the
bubble feedback and passes **10/10 (28/28 steps each)**: overlay windows present,
bubble visible, tap delivered, screenshot taken by the accessibility module (record
present, **awaited with a deadline**), the app's HARDWARE-bitmap wrap + CPU copy ran,
its OCR pipeline received the captured 1080x2400 frame, the lookup overlay is a new
full-screen accessibility-overlay window, and the bubble is hidden while it is up.

The failure at step 21 was **not** a timing flake alone. After the tap, Gaten's
lookup **did** reach the screenshot, the wrap and the GL read-back worked (GLSPIKE4's
claim holds), and then **the guest aborted** in the RenderThread. The old assertion
read the module's record before the app's worker thread asked for it (~150 ms),
which hid the crash: GLSPIKE4 saw "step 21 fails with the flag on and off" and
attributed it to the scenario's timing. Both were true; the second was masking a
real chain of three guest-side causes.

## 1. What actually happens after the bubble tap (measured)

| moment | what | evidence |
|---|---|---|
| tap delivered | the service gets the gesture, hides the bubble windows (`remove`) | `base1/steps/20-tap.json` (2 acks) |
| +~0 ms | `GatenAccessibilityService.takeScreenshot` → module `ACOSA11Y takeScreenshot success display=0 1080x2400` | `wait1` trace; record appears ~150 ms after the tap (`final/steps/21-service_ctl.json`, 151 ms) |
| +~1–5 s | worker thread `Bitmap.wrapHardwareBuffer` + `copy(ARGB_8888)` through hwui + ANGLE/SwiftShader (`libEGL: loaded … libEGL_angle.so`, `ANGLE … SwiftShader Device`) | `wait3/logs/logcat.txt` |
| then (before the fixes) | **SIGABRT in `RenderThread`** (`__android_log_assert`) — three different assertions in a row, §2 | `wait1`…`wait5` |
| now | lookup overlay window added (full-screen `TYPE_ACCESSIBILITY_OVERLAY`, focused), screen frozen + dimmed with the lookup frame, ML Kit OCR receives the frame (`scheduler.cc: ImageMetadata: 1080x2400`) | `final/frames/47-gaten-lookup-overlay.png`, `exp1` window stack |

Not what the brief feared: **no** crash from a HARDWARE bitmap drawn into a View
(Gaten copies to ARGB_8888 first, `GatenAccessibilityService.deliverScreenshot`), and
**no** missing ML Kit: the OCR models are bundled in this APK (`mlkit-google-ocr-models`
load lines); only the *Play asset-pack state read* fails (`PLAY_STORE_NOT_FOUND`, a
warning in `GatenOcr`, not a lookup failure). The word sheet is the next step and
needs the user to tap a recognised word, so the scenario stops at the overlay (§4).

## 2. Root causes found and fixed (ACOS-side, each with a failing-before / passing-after proof)

The abort was one chain; each link only showed once the previous one was fixed
(`wait4` → `wait5` → `wait6`).

1. **No memfd-backed ashmem in the guest** (`crates/acos-art/src/props.rs`).
   libcutils' `has_memfd_support()` is false without `sys.use_memfd=true` and
   `ro.vndk.version` (the image logs `memfd: ro.vndk.version not defined or invalid`),
   so every ashmem call takes the `/dev/ashmem` ioctl path — and the guest has no
   `/dev/ashmem`; every buffer ACOS hands out is a memfd. hwui's
   `ProfileDataContainer::switchStorageToAshmem` called `ashmem_get_size_region` on the
   `IGraphicsStats` memfd and libcutils' `__ashmem_check_failure` raised
   `LOG_ALWAYS_FATAL("illegal fd=71 mode=0100777 …")` (`wait3`). Both properties are
   served now (`Props::android34`, asserted in `defaults_are_servable`); this fixes the
   whole class (`SharedMemory`, `MemoryIntArray`, …), not only hwui.
2. **Zero-sized graphics-stats buffer** (`crates/acos-services/src/misc.rs`,
   `crates/acos-parcel/src/fd.rs`). The stub returned a 0-byte memfd; hwui requires
   `sizeof(ProfileData)` (584) — `Ashmem region is too small! Received 0, required 584`
   (`wait5`). New `ParcelFd::memfd_sized`; the stub returns one page, like the real
   service's ashmem region.
3. **A window that asks for hardware acceleration itself started a
   `ThreadedRenderer` the guest cannot run** (`crates/acos-art/guest/java/dev/acos/art/LaunchMain.java`).
   ACOS clears `FLAG_HARDWARE_ACCELERATED` on the app and activities, but Gaten's lookup
   overlay sets it on its own window (`OverlayWindowController.kt:452`), so
   `ViewRootImpl.enableHardwareAcceleration` → `ThreadedRenderer.create` →
   `ProcessInitializer.initDisplayInfo` → `DeviceInfo::updateDisplayInfo` →
   `LOG_ALWAYS_FATAL("Failed to acquire physical displays for WCG support!")` (no
   SurfaceFlinger; `wait5`). The guest now sets the framework's own switch
   `ThreadedRenderer.sRendererEnabled=false` once (the documented "this process must not
   accelerate" switch for low-end persistent processes): such windows fall back to the
   software path and `Canvas.isHardwareAccelerated()` is false — what the guest already
   claims for activities. It is **not** a new architecture decision: ACOS windows were
   already software canvases (GLSHIP §0, GLSPIKE4 §5); this closes the one hole. GL stays
   available for bitmap read-backs (`RenderProxy`), which do not use it. Guest line:
   `ACOSLAUNCH MARK window-renderer software (ThreadedRenderer.sRendererEnabled=false)`.
4. **The verdict's logcat dropped everything after the last between-step flush**
   (`crates/acos-art/src/launch.rs`, `crates/acos-cli/src/verify.rs`). The verifier reads
   the guest log between steps only, so the guest's last words — here the three
   assertions above — were absent from `logs/logcat.txt` (260 of 364 entries, `base1`).
   That is why the crash was invisible. The launcher now writes the whole sink to
   `art-frames/logd-final.jsonl` (absolute ordinals, drop-oldest aware) and `acos verify`
   appends the missing tail to `logs/logcat.txt` (trusted runs only; guest-asserted).
5. **Harness: a racy immediate assert** (`crates/acos-protocol/src/scenario.rs`,
   `crates/acos-verify/src/executor.rs`). `service_ctl … timeout_ms` (with `expect`, op
   `status` only) re-reads the read-only status until it holds `expect`, and fails with
   the **last** reply after the deadline; a wait never turns a miss into a pass. The
   scenario uses it for the screenshot record (30 s deadline, measured 151 ms) and
   `assert_log` (awaited) for the OCR hand-off. No assertion was weakened; two were
   added (`assert_log`, `assert_windows`/`assert_not_visible` after the lookup).

## 3. Acceptance

| criterion | result | evidence |
|---|---|---|
| `gaten-bubble-overlay` 10/10 deterministic, honest expectations | **PASS** 10/10, 28 steps each | `rep-1…rep-10/verdict.json`, `rep-*.log`; `final/` keeps the pictures |
| `gaten-bubble` | **PASS** (14 steps) | `gate-gaten-bubble/` |
| `a11y-probe` | **PASS** (15) | `gate-a11y-probe/` |
| `a11y-screenshot-wrap` | **PASS** (3) | `gate-a11y-wrap/` |
| `gles-software` | **PASS** (3) | `gate-gles-software/` |
| regression for the property/renderer changes: `counter`, `form-validation`, `webview-probe` | **PASS** (24, 28, 19) | `gate-counter/`, `gate-form/`, `gate-webview-probe/` |
| `cargo test -p acos-art -p acos-services` | **PASS** 900 passed, 0 failed (92 result lines) | `cargo-test-art-services.log` |
| also `-p acos-protocol` (schema regenerated), `-p acos-verify` | PASS 96 + 119/42 | run in the worktree |

New teeth (each fails before, passes after): `defaults_are_servable` (+ `sys.use_memfd`,
`ro.vndk.version`), `gatene2e_service_ctl_timeout_rules` (protocol),
`service_ctl_without_timeout_samples_once_and_fails_on_a_not_yet_state`,
`service_ctl_timeout_waits_for_the_state_the_app_reaches_later`,
`service_ctl_timeout_never_turns_a_miss_into_a_pass` (verify),
`dump_seq_follows_the_sinks_ordinal_even_after_drop_oldest` (art),
`merge_appends_only_the_missing_tail` (cli). The scenario itself is the end-to-end
tooth: `base1` (before) fails at step 21, `final`/`rep-*` (after) pass.

Known pre-existing flake: `acos-art` lib `tests::run_root_makes_shared_inputs_read_only`
failed once in 3 runs because a props test sets `ACOS_GUEST_GLES` in the same process
(env-var race: `ACOS_GUEST_GLES=angle-swiftshader: … sha256 … != manifest 00`); it
passed in the other two runs and in the final one. Not caused by this change (the test
does not read any property I touched); the smallest fix is a process-wide env lock in
the two tests.

## 4. What is not covered, and the HARDWARE-bitmap-into-View answer

* **The word sheet** (Gaten's result UI after tapping a recognised word) needs a second
  tap on OCR output; the scenario stops at "overlay up, OCR handed the frame" and
  asserts no words it has not seen. Smallest next action: a follow-up scenario that
  waits for the OCR pass and taps the first recognised word (needs a tree dump of the
  overlay's Compose nodes, which the accessibility module can already read).
* **HARDWARE bitmap drawn into a View** (the known-unsupported case, `docs/LATER.md`):
  this path is **not hit by Gaten**. For the record, the exact failure for an app that
  does hit it: `ViewRootImpl.drawSoftware` → `Surface.lockCanvas` software `Canvas` →
  `BaseCanvas.throwIfHwBitmapInSwMode` → `IllegalArgumentException: Software rendering
  doesn't support hardware bitmaps` (GLSPIKE4 `gles-sw/`, `gles-sw2/`; process dies
  with `LOOPER_THREW`). The copy-to-CPU interim (GLSPIKE4 §5 option B, *not
  implemented*): patch `BaseCanvas.throwIfHwBitmapInSwMode` to copy instead of throw;
  size = the bitmap's `ARGB_8888` copy — **1080 x 2400 x 4 = 10,368,000 bytes
  (9.9 MiB) per screenshot-sized bitmap** (the measured `wrap_hardware_buffer
  allocation=10368000`), plus one hwui GL read-back per draw of that bitmap through
  ANGLE/SwiftShader (CPU-rasterised).
* **Drag** (A11YPACE §1b) is unchanged.

## 5. Decision for the orchestrator

Fix 3 sets `ThreadedRenderer.sRendererEnabled=false` for every guest. Everything the
guest ran before was already software, so no scenario changed, but an app that *wants*
a GPU window now silently gets a software one (as on an emulator with no GPU). If the
owner prefers "fail loudly" for a hardware-window request, the alternative is leaving
`sRendererEnabled` alone and making the abort a structured `UNSUPPORTED` — a
launcher-side change in the guest crash classifier; I did not choose it because the
framework's own fallback is the stock behaviour for "this process cannot accelerate".
GPU-drawn windows stay the LATER item.

## 6. Repro

```sh
export ACOS_FONT_DIR=/home/Qui/Projects/acos/.cache/boot/android-34/fonts
tools/build-apps.sh && tools/build-dogfood-apps.sh
export CARGO_TARGET_DIR=$PWD/.cache/gatene2e-target CARGO_PROFILE_DEV_DEBUG=0 CARGO_INCREMENTAL=0
cargo build --release --offline -p acos-cli -p acos-art
tools/art-runtime/run-exclusive.sh --task GATENE2E -- tools/guarded.sh --mem 10G --time 560 -- \
    results/handoff/GATENE2E/run-rep.sh 1 5      # then 6 10
tools/art-runtime/run-exclusive.sh --task GATENE2E -- tools/guarded.sh --mem 12G --time 580 -- \
    results/handoff/GATENE2E/run-gates.sh
```

## 7. Evidence index

`base1` (before: fails step 21), `trace1` (module/WMS trace of the same), `wait1`
(+12 s wait, the abort appears: `art-frames/native-crash.txt` was pruned, the stack is
the single frame `libartshim.so __android_log_assert+452` = the guest's own
`LOG_ALWAYS_FATAL`), `wait3` (first complete logcat: `illegal fd=71`), `wait4`
(`ro.vndk.version` only: same assertion — it needs `sys.use_memfd` too), `wait5`
(memfd on: stats buffer too small, then the WCG fatal), `wait6` (all three fixed: pass),
`exp1` (+ window stack and `frames/47-after-lookup.png`), `rep-1…10`, `final`
(`frames/14-gaten-screen-lookup.png`, `frames/47-gaten-lookup-overlay.png`),
`gate-*`, `cargo-test-art-services.log`, `run-arm.sh`, `run-rep.sh`, `run-gates.sh`,
`wait-probe-scenario.yaml` / `exp-probe-scenario.yaml` (the probe variants).
Frames and trees were pruned to the named pictures (350 MB → the size of this directory).

## 8. Environment / cleanup

Host RAM peak well under 90 % (per-run cgroup `memory.peak` ≈ 540 MiB, guarded 10–14 GiB
builds); disk 80 → 85 % at the debug test build, back to ~83 % after deleting
`.cache/gatene2e-target/debug`. At the end: `.cache/gatene2e-target`, `.cache/ger-*`,
`.cache/lv`, `.cache/trace*.txt`, `.cache/wait*.txt`, `.cache/gb-*.yaml` and the
`fixtures/build` copy are deleted.
