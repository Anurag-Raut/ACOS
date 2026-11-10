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

---

# W108-A — real `AChoreographer` delivery via the caller's `ALooper`; next blocker

**Date:** 2026-11-11 (wave W108-A). **Base:** clean worktree of `HEAD` =
`791f14d` (W102-A boot-cache commit; the W103-A Flutter work is its parent
`415809c`). Built with `CARGO_TARGET_DIR=.cache/target-w108a`,
`CARGO_PROFILE_DEV_DEBUG=0`. Not committed.

## 1. Engine symbol/dwarf facts (offline, no source needed)

`.cache/flutter-1/lib-x86_64/libflutter.so` is the same engine hash the
harness runs (`89c39edfdfae2949f6806e4a315c0c08522238b7`), **not stripped, with
DWARF**. `ptype`/`objdump -dl` pin the relevant contract exactly:

| symbol | file offset | shape |
|---|---|---|
| `VsyncWaiter::FireCallback(TimePoint,TimePoint,bool)` | `0x9d0010` | moves `callback_` (`+0x80`, invoker `+0xa0`) under `callback_mutex_` (`+0x50`); a null invoker emits the `MismatchedFrameCallback` trace |
| `Animator::RequestFrame(bool)::$_0::operator()` | `0x9acf08` | sets `vsync_waiter_->callback_` then virtual-calls `AwaitVSync()` (vtable `+0x10`) |
| `VsyncWaiterAndroid::AwaitVSync()` | `0x5387d6` | native path = `RunNowOrPostTask(task_runners_.ui_, $_0)`; `$_0` (`0x538a74`) resolves `AChoreographer_postFrameCallback(64)` from the proc table and posts `Choreographer::PostFrameCallback(...)::$_0::__invoke` (`0x9ab0f6`) with an `InFlightData*` |
| `VsyncWaiterAndroid::ConsumePendingCallback(weak_ptr,TimePoint,TimePoint)` | `0x538d42` | `FireCallback(this, start, target, /*pause_rendering=*/true)` |

`InFlightData::__invoke` (`0x9ab0f6`) calls the `std::function` at `data+0x20`
and then `free(data, 0x30)` — the callback/data pointers are one-shot and must
be round-tripped verbatim. Our shim already does this. The native callback is
the only AChoreographer entry the engine resolves (`getInstance`,
`postFrameCallback`, `postFrameCallback64`; no refresh-rate callback).

## 2. The fix: deliver from the looper, never inline

W103-A invoked the frame callback **synchronously inside
`AChoreographer_postFrameCallback`**. Real bionic queues the callback on the
calling thread's `ALooper` and runs it at the next `ALooper_pollOnce`; running
it inline re-enters the engine while `Animator::RequestFrame`'s task is still
on the stack. Implemented (native-loader, additive):

* `bionic/ndk/ndk_looper.rs` — each `Looper` gains a
  `posted: Mutex<VecDeque<PostedFrame>>`. New
  `post_frame_callback(frame_time, cb, data)` enqueues on the **calling
  thread's** looper and wakes its `eventfd`. `poll_once` runs one queued frame
  callback (before the wait and again after a woken/empty wait) on the polling
  thread; `ALooper_pollAll` picks it up through `pollOnce` unchanged.
* `acos-jni/src/ndk_host.rs` — `choreographer_post` computes the monotonic
  60 Hz frame time exactly as before, then hands the callback to the looper
  instead of invoking it; only when the thread has no prepared `ALooper` does
  it fall back to a direct call.
* Loader-level bounded trace, no env gate (confinement sanitises the
  environment so a debug env var cannot reach the child):
  `AChoreographer_postFrameCallback … cb=… data=… tid=…`,
  `choreographer_post … queued looper=…`, `choreographer_deliver …`.

This was confirmed end-to-end: the post is enqueued on `ThreadId(2)`
looper `0x1` and the callback runs later from that looper, on `ThreadId(2)`,
with the same `cb`/`data`/`frame_time`. The synchronous re-entrancy is gone.

## 3. Remaining blocker: the primary frame request never re-arms

The async delivery is necessary but **not sufficient** in this harness. With
the fix in place the run still produces no frame:

* exactly **one** `AChoreographer_postFrameCallback` (delay 0) is ever made,
  it is delivered asynchronously, and no second post follows — not even when
  the harness calls `scheduleFrame()` from the platform thread every 20 pump
  iterations for the whole run;
* Dart `main()` runs (`ACOS_DART_MAIN entered`), but `onBeginFrame`,
  `onDrawFrame` and `onPointerDataPacket` **never** run (verified by adding
  `ACOS_DART_BEGINFRAME` / `ACOS_DART_POINTER` / existing
  `ACOS_DART_FRAME draw` prints to the instrumented snapshot);
* the UI/raster task-queue fds (`ThreadId(3) fd=8`, `ThreadId(4) fd=11`) and
  the platform fd (`ThreadId(2) fd=5`) are all serviced by the loader looper,
  so the engine's message loops are pumping — the missing piece is upstream of
  the looper.

`Animator::RequestFrame` early-returns when its `Semaphore::TryWait()` fails
(`animator.cc:250`); that semaphore is re-signalled only by the frame callback
(`Animator::AwaitVSync::$_0`). One post/one delivery therefore means the first
`RequestFrame` consumed the permit and its
`AsyncWaitForVsync` → `FireCallback` chain did not invoke the primary
`callback_`: the most consistent reading of the evidence (W103-A already saw
the same `MismatchedFrameCallback`) is that the single native post is the
`AwaitVSyncForSecondaryCallback` path, which bypasses `callback_`, while the
primary `Animator::RequestFrame` never reaches `AsyncWaitForVsync` on the
thread that owns the posting looper. The next wave needs a breakpoint/trace on
`VsyncWaiter::FireCallback` `+0xa0` (or on
`Animator::RequestFrame(bool)::$_0::operator()` `0x9acf08`) to settle whether
the primary lambda runs and which runner posts it; then the Dart frame
dispatch (`onBeginFrame`) and only after that the software rasterizer
(`acos-gles` GL/Skia) and `ANativeWindow_lock`/`unlockAndPost` become the
blockers.

## 4. Acceptance (W108-A worktree)

| check | result |
|---|---|
| `cargo test -p acos-jni --test flutter_hello` | **pass** (2/2) |
| `cargo clippy -p acos-jni -p acos-native-loader --all-targets --no-deps -- -D warnings` | **clean** |
| `cargo check -p acos-jni -p acos-native-loader --all-targets` | **clean** |
| `cargo check --workspace --all-targets` | **fails in `acos-runtime`** — 9 errors, all references to uncommitted W102-A boot-cache symbols (`BinderTransport::seed_binder`, `acos_vm::snapshot::boot_dir_fingerprint`, `Vm::reseed_after_restore`, `ServiceHost::import/export_binder_layout`, `seed_attached_process`, …) that are absent at `791f14d`. Pre-existing, unrelated to W108-A (my diff touches only `acos-native-loader` and `acos-jni`). |
| `tools/diff-dexprogs.sh --corelib real` | not run — needs a release `acos-cli`; blocked by the same incomplete `acos-runtime` at `791f14d` |
| confined flutter run | `confined:true`, `error:null`, exit 0 |
| native `AChoreographer` callback | queued + delivered **asynchronously** from the caller's `ALooper` (proven by trace) |
| Dart `onBeginFrame` / `onDrawFrame` / pointer | **still never run** (see §3) |
| PNG / `acos frame-diff` | not reached |

## 5. W108-A files

* `crates/acos-native-loader/src/bionic/ndk/ndk_looper.rs` — per-looper posted
  frame-callback queue; `post_frame_callback`; delivery from
  `ALooper_pollOnce`; bounded symbolised trace.
* `crates/acos-native-loader/src/bionic/ndk/ndk_misc.rs` — bounded
  `AChoreographer_postFrameCallback*` entry trace.
* `crates/acos-jni/src/ndk_host.rs` — queue instead of synchronous invoke.
* `crates/acos-jni/tests/flutter_hello.rs` — output PNGs named
  `w108a-frame-{i}.png`.
* `crates/acos-jni/tests/data/flutter_probe_main.dart` — `onBeginFrame` /
  pointer heartbeat prints used as the diagnosis above.

---

# W113-A — where the primary frame task stops (looper/timerfd/clock evidence)

**Date:** 2026-11-11 (wave W113-A). **Base:** clean worktree of `HEAD`
(`71cec4e`, the commit the W108-A work sits on). Built with
`CARGO_TARGET_DIR=.cache/target-w113a`, `CARGO_PROFILE_DEV_DEBUG=0`; applied
back with `git apply --check`. Not committed. The wave is **diagnostic**: no
frame was produced, and the changes shipped are an *inert-by-default* trace
(`const TRACE: bool = false`, `const TIMER_TRACE: bool = false`) in
`acos-native-loader`.

## Result at a glance

| stage | W108-A | W113-A |
|---|---|---|
| `libflutter.so` load/reloc/`JNI_OnLoad` | ok | ok (510/0, 32 962 relocs) |
| Dart `main()` | ok | ok (`ACOS_DART_MAIN entered`) |
| `AChoreographer_postFrameCallback` | 1 post, async via `ALooper` | same: **tid 2, looper 0x1**, delivered async from looper 0x1 |
| engine fml loops (ui/raster/platform) | assumed | **measured: all 3 exist and poll** (`tid 2 looper 0x1 fd 5`, `tid 3 looper 0x2 fd 8`, `tid 4 looper 0x3 fd 11`) |
| `timerfd_create/settime` + `read` | untested | **real**: `TFD_TIMER_ABSTIME` on `CLOCK_MONOTONIC`, `read` returns 8 B, `expirations=1` |
| clock source | assumed | **same epoch**: `TimePoint::Now` → libc++ `steady_clock` → `clock_gettime(CLOCK_MONOTONIC)` (0x53cab4); shim forwards id 1 |
| Dart `onBeginFrame`/`onDrawFrame`/pointer | no | **no** |
| PNG / SSIM | no | no |
| flaky crash in `VsyncWaiter::ScheduleSecondaryCallback` | not seen | **seen with tracing on** (`libflutter.so+0x9cffee`) |
| tests / clippy / check / dexprogs | — | see §5 |

## 1. Hypothesis (a) — "the engine's loops are not pumped" → **falsified, narrowed**

The 400-line bounded looper trace (`W113A` log in
`results/flutter/w113a-trace-run.log`) shows the engine's own
`fml::MessageLoopAndroid` instances are alive and polling:

```
tid=2 prepare id=1 ; addfd looper=0x1 fd=5  ident=-2 events=1   (platform)
tid=3 prepare id=2 ; addfd looper=0x2 fd=8  ident=-2 events=1   (ui)
tid=4 prepare id=3 ; addfd looper=0x3 fd=11 ident=-2 events=1   (raster)
tid=2/3/4 pollOnce(... -1) -> -2 fd=5/8/11 events=1             (steady)
```

So each runner has a working `ALooper` + `timerfd` + `ALooper_addFd` callback,
and our shim dispatches readiness. The platform thread **is** the UI thread in
this engine (Flutter 3.47 merged platform/UI threads): the single
`AChoreographer_postFrameCallback` is made on `tid 2` with `looper=0x1` and the
W108-A async delivery from that same looper still holds.

## 2. Hypothesis (b) — "clock / timers are wrong" → **falsified**

Disassembly facts (no source needed):

* `fml::TimePoint::Now()` (`0x557526`) → libc++ `steady_clock::now()`
  (`0x53cab4`) → `clock_gettime(1)` (`CLOCK_MONOTONIC`).
* `fml::MessageLoopAndroid::WakeUp` (`0x559bb0`) → `TimerRearm` (`timerfd.cc:43`)
  → `timerfd_settime(fd, TFD_TIMER_ABSTIME, …)`.
* bionic and glibc agree on `CLOCK_MONOTONIC`=1, `TFD_TIMER_ABSTIME`=1 and
  `struct itimerspec`/`struct timespec` layout on x86_64, so the forwarded
  entries are ABI-identical.

Measured with the timer trace:

```
tid=2 create clock=1 flags=526336 -> 5
tid=2 settime fd=5 flags=1 value=35935.039218077 interval=35935.039218077 -> 0
tid=2 read  fd=5 count=8 -> 8 expirations=1
```

The absolute deadline is `CLOCK_MONOTONIC` (~uptime), the arm succeeds, and the
expiry is consumed. `timerfd`/`epoll`/`read` are functioning, not fake success.
(The engine sets `it_interval == it_value`, i.e. a ~uptime-long period; the
practical effect is one-shot.)

## 3. What the evidence now says about the primary callback

`Animator::RequestFrame` posts its `$_0` lambda to the **UI runner**
(`task_runners_+0x30`, `animator.cc:263`); the lambda calls `Animator::AwaitVSync`
→ `VsyncWaiter::AsyncWaitForVsync`, which early-returns if `callback_+0xa0` is
already set (`vsync_waiter.cc:35-40`), otherwise stores `callback_` and calls
`waiter->AwaitVSync()` (`+0x10`). `VsyncWaiterAndroid::AwaitVSync` then
`RunNowOrPostTask(ui_runner)` → in merged mode this runs inline on tid 2 and
posts `AChoreographer_postFrameCallback` (the single post we see).

`FireCallback` (`0x9d0010`) then: moves `callback_` out, **pauses the Dart
event-loop (secondary) task source** when `pause_rendering` is true
(`vsync_waiter.cc:115`), and `PostTask`s the frame callback on the UI runner
(`vsync_waiter.cc:129-131`). `TaskRunner::PostTask` → `MessageLoopImpl::PostTask`
→ `RegisterTask(..., TaskSourceGrade=2)` (`0x554da8`), and grade 2 indexes the
**primary** queue (`TaskSource+8`; `TaskSource` layout from DWARF:
`primary_task_queue_` +8, `secondary_task_queue_` +0x40,
`secondary_pause_requests_` +0x78). So the frame task is *primary* and should
survive the secondary pause.

The trace nonetheless shows that after delivery the platform timerfd is
re-armed **to a stale absolute time that predates the post** and fires/reads in
a tight loop:

```
read  fd=5 -> 8 expirations=1
settime fd=5 value=<the same pre-post timestamp>   (repeats)
```

`RunExpiredTasksNow` (`0x554db4`) re-arms with `GetNextWakeTimeUnlocked()` =
`PeekNextTaskUnlocked().task_time +0x40`; a task with that stale time is
therefore at the head of the queue for the rest of the run, while the fresh
frame task (posted later) never runs. This is a **head-of-line / re-arm loop**,
not a timer or clock failure and not an unpumped runner. Whether
`PeekNextTaskUnlocked` (`message_loop_task_queues.cc:363-393`) skips a
`secondary_pause_requests_>0` source, and what the stale task is, cannot be
settled from the dispatcher side; it needs the engine source or a breakpoint on
`TaskSource::TopTask`/`TaskSource+0x78`.

## 4. New: a timing-sensitive corruption in `ScheduleSecondaryCallback`

With the trace compiled in (`TRACE/TIMER_TRACE = true`), the confined run
intermittently dies in `VsyncWaiter::ScheduleSecondaryCallback`:

```
W103A_CRASH signal=11 pc=libflutter.so+0x9cffee fault=0x7f6e00000018
```

`0x9cffee` is `call *0x18(%rax)` with `rax = *(rbx)`, i.e. an object's vtable
pointer read back as `0x7f6e_0000_0000` — **the low 32 bits zeroed**, a
partial/torn 64-bit pointer write. It reproduced in 4 of 5 traced runs (and
under gdb, at the same PC) and was **not** observed in 3/3 untraced runs of HEAD
(15.3 s each, 2/2 tests), so it is a latent race exposed by timing, not a
consequence of the trace logic itself. Because the single vsync post comes from
the secondary-callback machinery, this is plausibly part of the same defect.
The trace is therefore committed **off** (`const TRACE: bool = false`) and this
is recorded as a real finding for the next wave.

## 5. Acceptance (W113-A worktree of `71cec4e`)

| check | result |
|---|---|
| `cargo test -p acos-jni --test flutter_hello` | **pass 2/2**, three consecutive runs (untraced, 15.3 s) |
| `cargo test -p acos-native-loader` | **pass** (6 + 1) |
| `cargo clippy -p acos-jni -p acos-native-loader --all-targets --no-deps -- -D warnings` | **clean** |
| `cargo check --workspace --all-targets` | **clean** (`check=0`) |
| `tools/diff-dexprogs.sh --corelib real` | **27 pass / 0 fail / 0 unsupported of 27** |
| confined flutter run | `confined:true`, exit 0, `error:null` |
| PNG / `acos frame-diff` SSIM | **not reached** (no presented frame) |

Files changed (worktree → applied with `git apply --check`):

* `crates/acos-native-loader/src/bionic/ndk/ndk_looper.rs` — inert-by-default
  `W113A` looper trace (`prepare`/`addFd`/`pollOnce` result/`epoll-ready`/
  `wake`, with thread id and looper id).
* `crates/acos-native-loader/src/bionic/engine_gaps.rs` — inert-by-default
  `timerfd_create`/`timerfd_settime` trace shims (forwarding, no behaviour
  change).
* Evidence log: `results/flutter/w113a-trace-run.log`.

## 6. Precise question

**In `VsyncWaiter::FireCallback` → `PauseDartEventLoopTasks()` (merged
platform/UI thread, `pause_rendering=true`), what keeps the freshly posted
primary frame task from being popped while `RunExpiredTasksNow` keeps re-arming
`fd=5` to an *earlier* pending task time?** Specifically:

1. Does `MessageLoopTaskQueues::PeekNextTaskUnlocked` (`message_loop_task_queues.cc:363-393`)
   exclude a `TaskSource` with `secondary_pause_requests_ > 0` (the
   `GetNumPendingTasks` path at `task_source.cc:53-60` does), or can it return
   that paused task's `task_time` and thereby pin `GetNextWakeTimeUnlocked` to a
   stale deadline?
2. If it does exclude it, then the stale head task at ≈39 ms of uptime (the
   first `WakeUp` of the platform queue, before `Dart main` runs) is a *primary*
   task that is never popped — what posts it (`TaskRunner::PostTask` from where,
   `vsync_waiter.cc:84` tail?), and under what condition would it be
   skipped?
3. Is the `0x9cffee` vtable corruption (`call *0x18(%rax)`, low 32 bits zeroed)
   a known engine race in `ScheduleSecondaryCallback` on merged threads, or
   should we treat it as an ACOS shim/ABI bug (most likely candidate: a 32-bit
   write into a 64-bit slot, e.g. a `TimePoint`/`TaskQueueId`/`std::function`
   field)?

A one-line pointer to the engine source for `vsync_waiter.cc:89-152`,
`message_loop_task_queues.cc:340-400` and `task_source.cc:20-100` (or a note
that `TopTask` ignores the pause depth) would let the next wave fix the re-arm
loop instead of re-deriving it.
