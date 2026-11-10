# Flutter hello engine — first confined run (W93-A)

**Date:** 2026-11-11. **Fixture:** `fixtures/build/apps/flutter-hello.apk`
(Flutter 3.47.9 engine `89c39edf…`, Dart 3.13.7 AOT, x86_64; `libflutter.so`
166,186,240 B + `libapp.so` 721,800 B). **Harness:**
`crates/acos-jni/tests/flutter_hello.rs`, driving the real
`io.flutter.embedding.engine.FlutterJNI` surface from Java
(`crates/acos-jni/tests/data/flutter-probe.dex`, source
`tests/java/dev/acos/fixtures/flutterhello/FlutterProbe.java`).

Work was done in a clean worktree of `HEAD` (be834a3) with
`CARGO_TARGET_DIR=.cache/target-w93a`, then applied to the working tree after
`git apply --check`.

## Result at a glance

| stage | outcome |
|---|---|
| confinement | **active** — `PR_SET_NO_NEW_PRIVS`, `close_fds`, `sanitize_env`, rlimits, Landlock (ABI 10, 6 rules), seccomp (92 insns, 61 denied syscalls). Harness raises the bounded `RLIMIT_AS` to 12 GiB (see §4). |
| APK DEX into the VM | 1 (`classes.dex`) |
| `libflutter.so` map + relocate + init | **ok** — 510 imports, **0 missing**, 32,962 relocations |
| `JNI_OnLoad` | **ok** — `rc = 0x10004` (JNI 1.4) |
| `RegisterNatives` on `FlutterJNI` | **42** entries |
| `System.loadLibrary("flutter")` from Java | **ok** |
| `FlutterJNI.init(..., "--enable-software-rendering --enable-impeller=false --aot-shared-library-name=…")` | **ok** |
| `FlutterJNI.attachToNative()` | **ok** |
| `FlutterJNI.runBundleAndSnapshotFromLibrary(libapp.so, "main", …)` | **ok** |
| Dart `main()` executes | **yes** — `[android-log 4] flutter: ACOS_DART_MAIN entered` after the harness pumps the engine's `ALooper` platform runner (`results/flutter/w93a-dart-main.txt`) |
| first frame | **not reached** — no Java `Surface`/`ANativeWindow` was delivered, so the software rasteriser has no target to publish into |

Run:

```
tools/flutter-hello-probe.sh          # build the FlutterProbe dex
tools/flutter-hello-snapshot.sh       # optional: instrumented libapp_probe.so (Dart print)
cargo test -p acos-jni --test flutter_hello
```

## 1. What the loader was missing, and what was added

`results/flutter/native-inventory.json` flagged 136 missing imports: 48
libc/other, 3 OpenSSL, 3 EGL, 82 GLESv2.

* **EGL/GLES — no code change needed.** Every missing `egl*`/`gl*` name is
  already present in `crates/acos-gles/src/ffi.rs` (`table()`; 25 EGL + 106 GL
  in the union of the RN and Flutter inventories, per W86-A). The inventory
  tool only consults `acos-native-loader`'s bionic table, which still holds the
  fail-closed EGL/GLES traps by default; `acos-gles::install_loader_table()`
  (called from `acos-jni`'s native path) replaces them. The harness installs it
  before loading.
* **libc/other (48):** added in the additive module
  `crates/acos-native-loader/src/bionic/engine_gaps.rs`. Most are ABI-identical
  to glibc and are *forwarded* (`fma`, `llround[f]`, `scalbnf`, `epoll_*`,
  `inotify_*`, `timerfd_*`, `sendfile64`, `ftruncate64`, `readlinkat`,
  `renameat`, `symlinkat`, `mkdirat`, `rewinddir`, `posix_madvise`,
  `pthread_condattr_setclock`, `malloc_usable_size`, `__*_chk`, the
  `getaddrinfo`/`getifaddrs`/`inet_*` family, `close_range`, `wait`,
  `environ`). Translating shims: `__openat_2` (fortified `openat`), `feof` /
  `setbuf` (bionic `FILE*` translation), and `sigprocmask` (the bionic 8-byte
  `sigset_t` vs glibc's 128-byte one). Fail-closed returning shims: `fork`,
  `execvp` (log a structured `FAIL_CLOSED` line, set `errno`, return `-1`).
* **OpenSSL (3):** `OPENSSL_memory_alloc`/`_free`/`_get_size`. In `libflutter.so`
  these are **weak** imports (`w` in `nm -D`) — BoringSSL's embedder hooks —
  so the module supplies the intended default allocator
  (`malloc`/`free`/`malloc_usable_size`) rather than a trap.
* **`setjmp`/`longjmp`:** bionic x86_64 `jmp_buf` is 88 bytes (`_JBLEN = 11`)
  with a different layout from glibc's mangled, 200-byte `__jmp_buf_tag`;
  forwarding corrupts the guest stack. `engine_gaps.rs` now contains a real
  `setjmp`/`longjmp` pair in `global_asm!` that saves/restores the callee-saved
  GPRs plus `rsp`/`rip` in the first 64 bytes — self-consistent and within the
  guest's 88-byte buffer. The Dart VM's `LongJump` needs exactly this. On other
  architectures a structured trap remains.

After this, the engine maps with **0 missing imports**.

## 2. The `FlutterJNI` drive path

`FlutterProbe` (only the public embedding surface) runs:

1. `System.loadLibrary("flutter")` — the real `Runtime.nativeLoad` seam; the
   runtime maps the engine (already mapped by the harness's raw loader, so the
   SONAME dedup path is used) and runs `JNI_OnLoad`.
2. `new FlutterJNI()`; `jni.init(null, shellArgs, …)`.
3. `jni.attachToNative()` → `nativeAttach(this)` → `nativeShellHolderId`.
4. `jni.runBundleAndSnapshotFromLibrary("…/libapp.so", "main", null, null, [], 0)`
   → `nativeRunBundleAndSnapshotFromLibrary`.

`shellArgs` are taken from the embedding's own constants:
`--enable-software-rendering` and `--enable-impeller=false` (Impeller has no
software path; `flutter_main.cc` fatal-checks `!settings.enable_impeller`), plus
`--aot-shared-library-name=<abs libapp.so>` (added by `FlutterLoader` for AOT).

`runBundleAndSnapshotFromLibrary` is asynchronous: `main()` and the first frame
are posted to the engine's platform task runner. The harness drains the same
per-thread `ALooper` the engine prepared
(`acos_native_loader::bionic::ndk::ndk_looper::alooper_pollonce`). With that
pump, the instrumented `libapp.so` logs `ACOS_DART_MAIN entered`.

## 3. Blockers found and their status

| # | category | blocker | status |
|---|---|---|---|
| 1 | GLES/EGL coverage | inventory counted 82 GLESv2 + 3 EGL as "missing" | **not a bug** — served by `acos-gles` once `install_loader_table()` runs (W86-A). Verified name-by-name against `ffi.rs`. |
| 2 | bionic shim | 48 libc/other imports unbound (Strict load fails) | **fixed** — additive `bionic/engine_gaps.rs`. |
| 3 | bionic shim | `setjmp`/`longjmp` ABI differs from glibc; Dart's `LongJump` calls it | **fixed** — x86_64 `global_asm!` pair. |
| 4 | OpenSSL | 3 weak BoringSSL allocator hooks | **fixed** — trivial `malloc`/`free`/`malloc_usable_size`. |
| 5 | Flutter settings | `--enable-software-rendering` aborts with Impeller on | **fixed in the harness** — add `--enable-impeller=false`. |
| 6 | Flutter AOT | "VM snapshot invalid and could not be inferred from settings" | **fixed in the harness** — pass `--aot-shared-library-name=<abs libapp.so>`. |
| 7 | sandbox policy | Dart `virtual_memory_compressed.cc: Failed to reserve region for compressed heap: 12` | **fixed in the harness** (12 GiB `RLIMIT_AS`); product default unchanged, note added to ADR-0022. |
| 8 | JNI host | `AAssetManager_fromJava: object has no readable native handle` (the embedding has no Java `Context`/`AssetManager`; `nativeInit` got `null`) | **open** — assets/font manifests are unavailable. The engine logs the structured refusal and continues; the `dart:ui`-only fixture needs no real asset. A Java `Context` is the `acos launch` lifecycle path (W84-A §5). |
| 9 | JNI/VM | during the looper pump the engine calls back `FlutterJNI.handlePlatformMessage(String, ByteBuffer, I, J)` and the VM raises `JNI_NATIVE_FAULT: … argument 1: expected object, got object` | **open** — platform-message marshalling bug in the VM↔JNI broker (owned by the VM workstream). `main()` has already run. |
| 10 | surface | no `nativeSurfaceCreated` / `ANativeWindow`, so no frame reaches `eglSwapBuffers`/`FrameStore` | **open** — the embedding needs a Java `Surface`; the NDK host's `ANativeWindow_fromSurface` requires a live framework surface. The task's frame observation needs a fixture that registers a window (`register_native_window`) and a Java `Surface` carrying it. |
| 11 | threads/priority | `Failed to set raster task runner priority` | non-fatal diagnostic — `sched_setscheduler`/`nice` is denied by rlimits/seccomp; the runner continues at normal priority. |
| 12 | Impeller | `Impeller validation: Could not open Android libraries` | non-fatal — Impeller is disabled; the validation probe cannot read `/system/lib64`. |

## 4. What the Dart AOT runtime needs from the sandbox

* **Address space:** the Dart VM reserves a large, `MAP_NORESERVE` compressed
  heap region during startup. `RLIMIT_AS = 4 GiB` (the product default) is
  already consumed by the 166 MiB engine mapping plus the loader's relocation
  tables, so the reservation fails `ENOMEM`. The harness uses a bounded 12 GiB
  `address_space_bytes`; the product recommendation is 8-16 GiB or an
  engine-aware profile. Risk: only more address space can be reserved before
  `ENOMEM` (still a finite DoS bound), recorded in the ADR-0022 W93-A note.
* **`/dev/urandom` and `/dev`:** read-only (`READ_FILE`) as in W84-A. Landlock
  rejects character devices, so the whole `/dev` tree is granted read-only.
* **executable memory:** the loader's `mmap`/`mprotect(PROT_EXEC)` stay allowed
  by seccomp; the engine's own JIT/AOT mappings (Dart AOT uses `PROT_EXEC` for
  the snapshot text) worked with no extra policy.
* **seccomp deny-list:** none of W71-A's 61 denied syscalls blocked the run.
  The only sandbox knob that had to move for the engine was `RLIMIT_AS`.
* **threads/futex/epoll:** untouched by the deny-list; the engine spawned its
  platform/raster/IO threads and the `ALooper` pump worked.

## 5. Acceptance (W93-A)

| check | result |
|---|---|
| `cargo test -p acos-jni --test flutter_hello` | 2 passed (engine 0 missing, `runBundle ok`, Dart `main` when the instrumented snapshot is present) |
| `cargo test -p acos-jni` | all suites pass (incl. `rn_hermes`, `rn_native_stack`, `gles_fixture`) |
| `cargo test -p acos-native-loader` | all pass (incl. `policy`, `ndk`) |
| `cargo clippy -p acos-native-loader -p acos-jni --all-targets --no-deps -- -D warnings` | clean |
| `cargo check --workspace --all-targets` | clean |
| `tools/diff-dexprogs.sh --corelib real` | **27 pass / 0 fail / 0 unsupported of 27** |

## 6. Next

1. Provide the engine a real `AssetManager` (or a harness NDK host backed by
   the APK's `flutter_assets`) so the font/asset manifests load.
2. Fix the `handlePlatformMessage` JNI argument marshalling (VM/JNI workstream).
3. Deliver a Java `Surface`/`ANativeWindow` and call `nativeSurfaceCreated` so a
   frame reaches `eglSwapBuffers`/`FrameStore`; then compare against the phone
   reference screenshot.
