# Flutter hello — first-frame attempt (W99-A)

**Date:** 2026-11-11. **Fixture:** `fixtures/build/apps/flutter-hello.apk`
(Flutter 3.47.9 engine, Dart 3.13.7 AOT, x86_64; `libflutter.so` 166,186,240 B,
`libapp.so` 721,800 B). **Harness:** `crates/acos-jni/tests/flutter_hello.rs`
driving the real `io.flutter.embedding.engine.FlutterJNI` from Java
(`tests/java/.../FlutterProbe.java` → `tests/data/flutter-probe.dex`).

Built in a clean worktree of `HEAD` (7360183) with
`CARGO_TARGET_DIR=.cache/target-w99a`, `CARGO_PROFILE_DEV_DEBUG=0`; applied back
after `git apply --check`.

## Result at a glance

| stage | W93-A | W99-A |
|---|---|---|
| confinement active | yes | yes (9 env vars incl. `ACOS_BOOT_DIR`) |
| `libflutter.so` map/relocate/`JNI_OnLoad` | 0 missing, 42 natives | same |
| `FlutterJNI.init` / `attachToNative` / `runBundle` | ok | ok |
| Dart `main()` runs | yes | yes (`ACOS_DART_MAIN entered`) |
| real `AssetManager` over the APK | **no** (`null`) | **yes** — `cookie=2`; `FontManifest.json`, `AssetManifest.bin` load |
| real Java `Surface`/`ANativeWindow` | no | **yes** — `SurfaceControl.Builder` → `Surface` → `ANativeWindow_fromSurface` (thread 2) |
| `nativeSetViewportMetrics` (27 args) | unreachable | **ok** |
| `dispatchPointerDataPacket` (576-byte tap) | unreachable | **ok** (Java `allocateDirect` + engine-format packet) |
| engine vsync callback | n/a | **not delivered** (blocker 13) |
| first frame in `FrameStore`/captured sink | no | **no** |
| `acos frame-diff` vs `screenshot_initial.png` | not run | **not run** (no candidate frame) |

No PNG was produced, so there is **no SSIM measurement** for this run: the
engine never rasterised, so there is nothing to compare. The frame *capture*
path is implemented and ready (`ndk_host::presented_frames()`; the harness
writes `results/flutter/w99a-frame-<n>.png` for every present) — it simply had
no input.

## Blockers 8/9/10

### 8 — JNI host: real `Context`/`AssetManager`  → **fixed (AssetManager)**

`FlutterProbe` now builds `new AssetManager(); addAssetPath(apk)` and passes it
both to `runBundleAndSnapshotFromLibrary` and (via the framework) to the
engine. Two further bugs had to be fixed for the assets to resolve:

1. **Wrong bundle path.** `bundlePath` in `runBundleAndSnapshotFromLibrary` is
   `FlutterApplicationInfo.flutterAssetsDir` (`"flutter_assets"`), *not* the
   AOT snapshot. W93-A passed `libapp.so`, so the engine asked
   `AAssetManager_open("libapp.so/FontManifest.json")`. The snapshot is named
   only by `--aot-shared-library-name=<libapp.so>`.
2. **`assets/` namespace.** Android's NDK asset namespace root is the APK's
   `assets/` directory: `AAssetManager_open("flutter_assets/x")` reads ZIP
   entry `assets/flutter_assets/x`. `acos-jni`'s NDK host matched entry names
   verbatim, so `asset_open`/`asset_dir_open` now retry with the `assets/`
   prefix before returning the documented error (`RES_ASSET_NOT_FOUND`).

`init()` still receives a `null` `Context`: the shell's `nativeInit` reaches the
Dart isolate with it, and every asset lookup in this fixture goes through the
`AssetManager` argument. A real `ContextImpl` needs the runtime's lifecycle
path (W84-A §5), which this isolated `acos-jni` test does not model.

### 9 — `handlePlatformMessage` JNI/VM marshalling  → **fixed**

The fault was `NewDirectByteBuffer` allocating an **empty `String` as a
marker**. `FlutterJNI.handlePlatformMessage(String, ByteBuffer, int, long)`
therefore received a `String` where a `ByteBuffer` was declared, and the
bridge's hard type check raised
`argument 1: expected object, got object` (both sides were "object"; only the
class differed).

`NewDirectByteBuffer` now builds the real carrier ART builds:

```
MemoryRef(allocatedAddress = addr, originalBufferObject = null)   // (J,Object)V
DirectByteBuffer(capacity, memoryRef)                            // (I,MemoryRef)V
```

so Java sees a genuine `java.nio.DirectByteBuffer` (`get`/`position`/`limit`,
`isDirect`, and any Java method declaring `ByteBuffer`). `GetDirectBufferAddress`
/`GetDirectBufferCapacity` were also made correct: the JNI side table is
authoritative for JNI-created buffers (synthetic `native_mem` addresses), while
Java-created direct buffers (`ByteBuffer.allocateDirect`, used for the pointer
packet) are read from the real `DirectByteBuffer.address`/`capacity` fields. A
bootstrap/partial corelib without `java.nio` falls back to the historic marker
so the JNI table still answers (documented in `jni.rs`).

After the fix the engine's platform-message callback completes with no JNI
error (`W93A_SUMMARY … "error":null`).

### 10 — real `Surface`/`ANativeWindow` + viewport + vsync  → **partly fixed**

Fixed and reachable:

* `SurfaceControl.Builder(...).setBufferSize(1080, 2400)` + `new Surface(sc)`
  (public API 34; `nativeCreateFromSurfaceControl` already implemented in the
  VM) → `valid=true`.
* `FlutterJNI.onSurfaceCreated(surface)` → `nativeSurfaceCreated` →
  `ANativeWindow_fromSurface` runs on the platform thread and creates a real
  NDK window over the surface's layer (`W99A_DIAG window_from_surface`).
* `setViewportMetrics(dpr=2.625, 1080×2400, …)` — all **27** arguments marshal.
* `dispatchPointerDataPacket(direct buffer, 0)` — a 2-record packet
  (`PointerChange.down=4`, `up=6`, 36×8-byte fields each) reaches the engine.

**Still missing: the vsync pulse, therefore no frame.**

* `nm -D libflutter.so` shows **no `AChoreographer_*` import**; the engine's
  Android vsync waiter calls the embedder back through
  `FlutterJNI.asyncWaitForVsync(long)` (the real `io.flutter.view.VsyncWaiter`
  installs that delegate from `FlutterLoader.startInitialization` and forwards
  it to the Java `Choreographer`). The `AChoreographer_*` strings in the binary
  belong to Impeller's toolkit and are resolved dynamically, not used by the
  shell vsync path.
* This harness never called `FlutterLoader.startInitialization`, so no delegate
  was installed and the engine never received a vsync (no
  `window_lock`/`unlockAndPost`, no `ACOS_DART_FRAME draw`).
* Two delivery strategies were tried and **both fault the process** (~2.7 s in,
  with no probe output after `init`):
  1. delegate → `jni.onVsync(...)` inline on the requesting (UI) thread;
  2. delegate records the request; the Rust pump calls a Java static
     `deliverVsync()` on the platform thread, which calls `jni.onVsync(...)`.
  The engine is thread-affine here: `nativeOnVsync` must arrive on the platform
  thread with the shell fully live, and neither attempt survives. This is the
  remaining blocker (see §1 below).
* The harness now *always* prepares the thread's `ALooper` before pumping
  (`alooper_prepare`), so platform tasks are really drained. This exposed that
  W93-A's Dart `main()` did **not** depend on the pump (the root isolate runs on
  the UI thread).

Root cause is in the VM/JNI ↔ engine vsync handoff, not in the renderer: the
software renderer is never asked to draw.

## 1. New blockers found and their status

| # | category | blocker | status |
|---|---|---|---|
| 13 | vsync/threads | `FlutterJNI.asyncWaitForVsync` delegate → `nativeOnVsync` faults the process (inline on UI thread, and via platform-thread delivery) | **open** — needs the real platform-thread `Choreographer`/`Handler` dispatch in the VM (or a Java main `Looper` loop), not a harness-side shortcut |
| 14 | JNI ABI | `nativeSetViewportMetrics(long, float, 15 int, 3 arrays, 8 int)` = 27 integer args | **fixed** — `acos_jni_call_abi` in `dispatch.rs`: a `global_asm!` System V trampoline that spills **all** integer args past the 6th (the old version handled 4 words / 10 args and returned `JNI_UNSUPPORTED: native ABI trampoline`) |
| 15 | asset path | engine reads `flutter_assets/…` as `assets/flutter_assets/…` | **fixed** — NDK host retries with the `assets/` prefix |
| 16 | engine settings | bundle path must be `flutter_assets`, not `libapp.so` | **fixed in the harness** |
| 17 | fonts | Skia's `SkFontMgr Android Parser` opened `/system/etc/fonts.xml` + `/system/fonts/*` and got ENOENT (empty font manager) | **fixed** — native-loader `fopen` shim translates `/system/...` to the extracted boot tree (`ACOS_BOOT_DIR`; `/system/etc/fonts*.xml` → `<boot>/fonts/`). Only `/product/etc/fonts_customization.xml` remains missing (optional) |
| 18 | NDK threads | `ANativeWindow` registry was **thread-local**, but the engine creates the window on the platform thread and locks/posts on the raster thread | **fixed** — process-wide `Mutex<HashMap>` window table (+ process-wide presented-frame capture) |
| 19 | assets | fixture APK has no `assets/flutter_assets/NativeAssetsManifest.json` (built before native assets) | **open, non-fatal** — structured `RES_ASSET_NOT_FOUND`, engine continues |
| 11 | threads/priority | `Failed to set raster task runner priority` | non-fatal diagnostic (unchanged) |
| 12 | Impeller | `Impeller validation: Could not open Android libraries` | non-fatal (Impeller disabled) |

No `acos-gles`/Skia GLSL blocker was reached: with
`--enable-software-rendering` the engine would present through
`ANativeWindow_lock`/`unlockAndPost`, not EGL, so the GL shader path is not on
the critical path for this fixture. Nothing in `acos-gles` was changed.

## 2. What a frame needs next (concrete)

1. **Deliver the vsync on the platform thread.** The robust option is the real
   embedding path: run `FlutterLoader.startInitialization(context)` (needs a
   real `Context`) so `io.flutter.view.VsyncWaiter` installs the delegate, and
   give the VM a Java main `Looper` loop whose `Choreographer` callback calls
   `FlutterJNI.onVsync(frameTimeNanos, refreshPeriodNanos, cookie)` on the
   platform thread. The ACOS VM already models `DisplayEventReceiver`
   (`fwk_vsync.rs`); the missing piece is that this harness has no Java main
   looper. Until then, do **not** call `nativeOnVsync` from the delegate.
2. Once `ACOS_DART_FRAME draw` appears, the engine's `ANativeWindow_lock` →
   Skia CPU draw → `ANativeWindow_unlockAndPost` → `gfx_hwui::submit_frame`
   path is already wired to `ndk_host::presented_frames()` and the harness
   writes `results/flutter/w99a-frame-<n>.png`.
3. Then run
   `tools/frame-diff fixtures/reference-device/flutter-hello/screenshot_initial.png results/flutter/w99a-frame-0.png --json`
   and record SSIM. Text ("hello acos", "counter: 0") should now render: the
   font manager loads `<boot>/fonts/fonts.xml` through the path shim.
4. The tap is already encoded and dispatched; a second frame showing
   `counter: 1` should follow the same vsync fix.

## 3. Acceptance (W99-A, worktree of HEAD 7360183)

| check | result |
|---|---|
| `cargo test -p acos-jni` | **pass** (all suites; flutter 2/2, `jni_native_fixture` 2/2) |
| `cargo test -p acos-native-loader` | **pass** |
| `cargo clippy -p acos-jni -p acos-native-loader --all-targets --no-deps -- -D warnings` | **clean** |
| `cargo check --workspace --all-targets` | **clean** |
| `tools/diff-dexprogs.sh --corelib real` | **27 pass / 0 fail / 0 unsupported of 27** |
| confined flutter run | `confined:true`, `error:null`, exit 0 |
| `libflutter.so` imports | 510 / 0 missing, 32,962 relocations |

## 4. Files

* `crates/acos-jni/src/jni.rs` — real `DirectByteBuffer` carrier; address/
  capacity for Java-created direct buffers; corelib fallback.
* `crates/acos-jni/src/dispatch.rs` — System V ABI trampoline with unbounded
  integer stack arguments.
* `crates/acos-jni/src/ndk_host.rs` — process-wide `ANativeWindow` registry,
  process-wide presented-frame capture, `assets/` retry, monotonic vsync clock.
* `crates/acos-jni/tests/flutter_hello.rs`, `tests/java/…/FlutterProbe.java`,
  `tests/data/flutter-probe.dex` — AssetManager, Surface, viewport (27 args),
  pointer packet, `ALooper_prepare`, frame PNG capture.
* `crates/acos-native-loader/src/bionic/engine_gaps.rs`,
  `csrc/stdio_shims.c` — `/system` → boot-tree path translation for `fopen`.
* `tools/flutter-hello-probe.sh` — compile @hide `AssetManager` stub from
  classpath, d8 all nest-mate classes.

---

# W103-A — vsync delivery: crash diagnosis + native `AChoreographer` path

**Date:** 2026-11-11 (wave W103-A). **Base:** `HEAD` = `ba69fab` (the W99-A
commit). Built in a clean worktree (`.cache/wt-w103a`) with
`CARGO_TARGET_DIR=.cache/target-w103a`, `CARGO_PROFILE_DEV_DEBUG=0`. Not
committed.

## Result at a glance

| stage | W99-A | W103-A |
|---|---|---|
| confinement active | yes | yes (9 env vars, Landlock+seccomp) |
| `libflutter.so` map/relocate/`JNI_OnLoad` | ok | ok (510/0 missing, 32,962 relocs) |
| `init` / `attach` / `runBundle` | ok | ok |
| Dart `main()` runs | yes | yes (`ACOS_DART_MAIN entered`) |
| `AssetManager` / `Surface` / viewport (27 args) / pointer packet | ok | ok |
| process crash on vsync delivery | **yes** (~2.7 s) | **no** |
| engine vsync path | Java delegate, not driven | **native `AChoreographer`** (see §2) |
| vsync callback reaches `VsyncWaiter::FireCallback` | n/a | **yes — callback invoked** |
| Dart `onBeginFrame` / `onDrawFrame` | no | **no** |
| `ANativeWindow_lock` / `unlockAndPost` | no | **no** |
| PNG / `acos frame-diff` SSIM | not produced | **not produced** |
| second frame (counter: 1) | no | **no** |

## 1. Crash diagnosis (blocker 13)

The W99-A report claimed two delivery strategies both faulted the process.
W103-A rebuilt the harmonic with the **broker-correct** shape and **no crash
was observed** (exit 0, every run):

1. **The delegate never touches the VM again.** `FlutterJNI.setAsyncWaitForVsyncDelegate`
   installs a Java object whose `asyncWaitForVsync(long cookie)` only stores the
   cookie. The engine invokes it through the native→Java JNI slot, so it always
   runs on the VM owner (platform) thread — never a raw worker thread.
2. **`nativeOnVsync` is delivered by the owner.** The Rust pump calls
   `FlutterProbe.deliverVsync(delay)` with `vm.call_static`; that Java method
   calls `jni.onVsync(frameDelay, 16 666 667, cookie)` → `nativeOnVsync` on the
   owner/platform thread. The cookie is cleared before the call, so a stale
   (double-freed) cookie can never be replayed — the likely cause of the W99-A
   `~2.7 s` fault.
3. **Diagnostics added:** an async-signal handler for `SIGSEGV`/`SIGBUS`/
   `SIGABRT`/`SIGILL`/`SIGFPE` that prints the fault address and the
   instruction pointer resolved to `module+offset (symbol)` via the raw
   loader's own `bionic::dl::dladdr` (the host `dladdr` cannot see a manually
   mapped ELF). It exits `134` so the parent records the last lines. Also a JNI
   entry trace (Java→native and native→Java method calls) was used during
   development.

No `W103A_CRASH` line was produced by any run. The container crash is therefore
**not reproduced by the brokered bridge**; we record it as fixed-by-construction
(Java only on the VM thread, cookie lifetime owned by the owner).

## 2. Native `AChoreographer` path enabled (blocker 13/10)

`VsyncWaiterAndroid::AwaitVSync` chooses the native path when
`impeller::android::Choreographer::IsAvailableOnPlatform()` is true. That
function `dlopen`s `libandroid.so` and resolves `AChoreographer_getInstance`,
`AChoreographer_postFrameCallback` (32-bit) and
`AChoreographer_postFrameCallback64`. The raw loader refused
`dlopen("libandroid.so")` because no APK file exists for it, so the engine fell
back to the Java delegate (W99-A's path).

**Fix (native-loader, additive):** `bionic::dl::dlopen` now hands out a
*psuedo-handle* (bit 62 set) for the bionic system libraries recognised by
`is_shim_system_lib` (`libandroid.so`, `liblog.so`, `libEGL.so`, …), and
`dlsym` on such a handle resolves from the process-wide bionic shim table
(`bionic::resolve`). `dlclose` forgets the pseudo-handle. No ELF is mapped and
no host library is touched.

After the fix the engine takes the native path:

```
AChoreographer_getInstance
AChoreographer_postFrameCallback delay=0 cb=0x7f…
choreographer_post now=70 frame_time=16666737   # 60 Hz monotonic, fwk_vsync model
invoke_frame_callback begin / end               # engine callback returned
```

`ndk_host::choreographer_post` invokes the engine callback synchronously with
`frame_time = mono_nanos() + delay + 16 666 667`, i.e. the same 60 Hz,
monotonic grid `fwk_vsync` uses. This is the "synthetic Choreographer frame
time" the task asked for.

## 3. Remaining blocker: the frame callback does not reach Dart

The engine's callback (`VsyncWaiterAndroid::…UlT_E_`) calls
`VsyncWaiterAndroid::ConsumePendingCallback(weak_ptr, frame_start, target)`
→ `VsyncWaiter::FireCallback`. `FireCallback` consumes the waiter's
`std::function` at `VsyncWaiter+0x80..0xa0`; if `0xa0` (the invoker) is null it
emits the `MismatchedFrameCallback` trace and returns. Instrumentation shows:

* `AChoreographer_postFrameCallback` is called **once**; our
  `invoke_frame_callback` runs to completion and returns normally.
* The engine then **never** calls `asyncWaitForVsync` again, never calls
  `AChoreographer_postFrameCallback` again, and a subsequent
  `nativeScheduleFrame` is coalesced (the `Animator`'s `Semaphore` is not
  signalled — `Animator::AwaitVSync`'s `$_0` is the only place that signals it).
* Dart `onBeginFrame`/`onDrawFrame` never run (instrumented snapshot), so
  `ANativeWindow_lock`/`unlockAndPost` are never reached and no frame exists.

Conclusion: `VsyncWaiter::FireCallback` runs but does **not** invoke the
primary frame callback. The most likely causes, in order, are (a) the callback
is consumed/cleared before the native callback fires (e.g. the waiter used by
`Animator::RequestFrame` and the one whose `weak_ptr` is fired differ, or a
prior `FireCallback` already consumed it) and (b) `FireCallback`'s
`pause_rendering=true` argument (hard-coded by both
`VsyncWaiterAndroid::OnVsyncFromJava` and the native `UlT_E_` lambda) taking a
different branch than expected. This needs a debugger/trace on
`VsyncWaiter::FireCallback` (base = `nativeOnVsync_ptr - 0x538dc2`, verified
page-aligned) plus the engine's `fml::tracing` "MismatchedFrameCallback" event
to settle.

The Java bridge from §1 remains wired as the fallback for engines where the
native path is unavailable; the harness delivers on the 60 Hz cadence whenever
`deliverVsync` reports a pending cookie.

## 4. Acceptance (W103-A, worktree of `ba69fab`)

| check | result |
|---|---|
| `cargo test -p acos-jni --test flutter_hello` | **pass** (2/2) |
| other `acos-jni` suites | **pass** |
| `rn_bridge` | **1 fail** — pre-existing, environment-only: Landlock `landlock_add_rule(/dev/urandom)` returns `EINVAL` (char devices are not Landlock paths); untouched by W103-A |
| `cargo test -p acos-native-loader` | **pass** |
| `cargo clippy -p acos-jni -p acos-native-loader --all-targets --no-deps -- -D warnings` | **clean** |
| `cargo check --workspace --all-targets` | **clean** |
| `tools/diff-dexprogs.sh --corelib real` | **27 pass / 0 fail / 0 unsupported of 27** |
| confined flutter run | `confined:true`, `error:null`, exit 0 |
| `libflutter.so` imports | 510 / 0 missing, 32,962 relocations |

## 5. W103-A files (worktree)

* `crates/acos-native-loader/src/bionic/dl.rs` — pseudo-`dlopen` handles for the
  bionic system libraries; `dlsym` resolves them from the shim table;
  `dlclose` forgets them.
* `crates/acos-native-loader/src/loader.rs` — `is_shim_system_lib` made
  `pub(crate)`.
* `crates/acos-jni/tests/flutter_hello.rs` — `install_crash_handler`
  (fault address + loader `dladdr` module+offset), 60 Hz `deliverVsync` pump on
  the owner thread, frame-PNG capture unchanged.
* `crates/acos-jni/tests/java/…/FlutterProbe.java` — `AsyncWaitForVsyncDelegate`
  bridge, `deliverVsync`, `setRefreshRateFPS(60)`/`updateRefreshRate`.
* `crates/acos-jni/tests/data/flutter-probe.dex` — rebuilt.

No `acos-gles`/Skia GLSL blocker was reached: the engine is still in the
software-renderer configuration and never issued a GL draw, so no shader
issue could be recorded.
