# `rn-hello` native stack — confined run (W76-A, updated W79-A)

**Date:** 2026-11-11. **Fixture:** `fixtures/build/apps/rn-hello.apk`
(React Native 0.81.4, Hermes HBC96, x86_64). **Harness:**
`crates/acos-jni/tests/rn_native_stack.rs` — it does **not** use `acos launch`
(W72-A owns the runtime `System.loadLibrary` wiring). The parent spawns a child
copy of the test binary; the child installs the ADR-0022 confinement and then
loads the fixture `.so`s with `require_confinement = true` (fail closed) and
calls their `JNI_OnLoad`.

Run:

```
cargo test -p acos-jni --test rn_native_stack -- --nocapture
```

Raw stages/summary: `results/rn/rn-native-first-run.json`.

## 1. Result at a glance (W79-A)

| stage | outcome |
|---|---|
| confinement | **active** — `PR_SET_NO_NEW_PRIVS`, `close_fds`, `sanitize_env`, `rlimits`, Landlock (ABI 10), seccomp (92 insns, 61 denied syscalls) |
| core library | `CoreLib::Real` (Android 14 boot classpath, read-only under Landlock) |
| APK DEX loaded into the VM | **2** files (`classes.dex`, `classes2.dex`) |
| fbjni Java classes | **present in the APK** (19 `com.facebook.jni.*` classes in `classes2.dex`) |
| native libs mapped / relocated / constructed | **6 / 6** under confinement |
| missing imports | **0** |
| `JNI_OnLoad` | **all succeed** — `libfbjni`, `libreactnative`, `libhermestooling` all return `JNI_VERSION_1_6` (`0x00010006`) |
| SIGSEGV / abort | **none** |

Load order and results:

| library | size (B) | load (map+reloc+init) | `JNI_OnLoad` |
|---|---:|---|---|
| `libc++_shared.so` | 1,252,080 | ok | n/a |
| `libjsi.so` | 398,088 | ok | n/a |
| `libfbjni.so` | 172,952 | ok | **ok** `0x00010006` |
| `libhermes.so` | 2,221,664 | ok | no `JNI_OnLoad` export |
| `libreactnative.so` | 6,123,024 | ok | **ok** `0x00010006` (merged, 11 sub-libs) |
| `libhermestooling.so` | 134,232 | ok | **ok** `0x00010006` (merged, 3 sub-libs) |

`W76A_SUMMARY {"blockers":[],"confined":true,"loaded":[6 libs],"missing_imports":[]}`.

## 2. Root causes fixed in W79-A

### 2.1 fbjni `JNI_OnLoad` threw `JniException` — missing `java.lang.ref` + `GetEnv(1.2)` (JNI)

Under `CoreLib::Bootstrap` the first failing `FindClass` is
`com/facebook/jni/HybridData$Destructor`, because its superclass
`DestructorThread$Destructor` extends **`java.lang.ref.PhantomReference`**,
which the Rust-built Bootstrap core library does not define. fbjni then tried to
translate the C++ `JniException` to Java; that path needs
`Throwable.getStackTrace()`, also absent under Bootstrap, so it fell through to
`std::terminate`.

Two observations:

* `HybridData$Destructor` **is** in the fbjni classes (it is in the APK now,
  §2.3); the failure was the *superclass*, not the class itself. Real ART has
  `java.lang.ref.PhantomReference`, so this is a Bootstrap-corelib gap, not a
  loader bug.
* After switching the harness to `CoreLib::Real`, fbjni's `JNI_OnLoad` still
  failed: `libreactnative.so`'s merged `JNI_OnLoad` (`jni_lib_merge`) calls
  `(*vm)->GetEnv(vm, &env, JNI_VERSION_1_2)` and immediately returns `JNI_ERR`
  if it is not `JNI_OK`. `crates/acos-jni/src/jni.rs::vm_GetEnv` accepted only
  `JNI_VERSION_1_6` and `JNI_VERSION_1_4`, so it returned `JNI_EVERSION` for
  `1.2`. This is the JNI-semantics bug. **Fix:** a JNI 1.6 VM now accepts every
  earlier 1.x request (major 1, minor ≤ 6).

### 2.2 SIGSEGV in host `fwrite` — bionic `__sF` stride was wrong (bionic)

`libc++_shared.so` and `libfbjni.so` were built against **pre-M** bionic headers
and import the `__sF` **object**, spelling `stderr` as `&__sF[2]`. Bionic's LP64
`FILE` is **152** bytes (`bits/struct_file.h`: `char __private[152]`), so the
stderr slot is `__sF + 304`. The shim used a 256-byte stride, so
`acos_map_stream` missed `&__sF[2]`, passed the bogus pointer straight to glibc
`fwrite`, and the process SIGSEGV'd while libc++abi printed `"libc++abi: "`.

**Fix (`crates/acos-native-loader/src/bionic/shims.rs`,
`csrc/stdio_shims.c`):**

* the `__sF` stride is the real bionic `sizeof(FILE)` (152) and the fake array
  is `152 * 3` bytes;
* `acos_map_stream` maps the three `__sF` slots and host std streams, and
  **returns `0` for anything it does not recognise** instead of forwarding it;
* every stdio trampoline checks for `0` and fails closed with `errno = EBADF`
  (`fwrite`/`fread` return 0, `fprintf`/`fputs`/`fclose` return `EOF`/`-1`)
  and emits a structured `ACOS_STDIO_INVALID_STREAM {...}` line;
* `fopen`/`fdopen`/`tmpfile`/`popen` register the host `FILE*` they hand out,
  `fclose` unregisters it, and `__fwrite_chk` now also fails closed.
* Unit tests cover the stride, the old wrong stride, and the registry.

### 2.3 Fixture DEX omitted `com.facebook.jni.*` (fixture tooling)

`react-android`'s POM marks `com.facebook.fbjni:fbjni` **optional**, and a Maven
walk correctly skips optional transitives, so `classes.jar` never reached the
APK. `tools/rn-hello-deps.py` now lists `com.facebook.fbjni:fbjni:0.7.0` as an
explicit root (a real Gradle app declares it as a runtime dependency). The
fixture was rebuilt offline (`tools/build-rn-hello.sh build`); `classes2.dex`
now carries 19 `com.facebook.jni.*` classes. The old 10 KiB
`crates/acos-jni/tests/data/fbjni-classes.dex` harness workaround was **deleted**
and the APK is self-contained.

## 3. Blockers remaining

| # | category | blocker | what it needs |
|---|---|---|---|
| 1 | **Hermes init** | The harness calls only the top-level `JNI_OnLoad`s. Hermes' runtime entry point (`libhermes_executor_so`, `libhermesinstancejni_so`, `libjsijniprofiler_so`) is registered as merged natives on `com/facebook/react/soloader/OpenSourceMergedSoMapping` and is invoked by `SoLoader` at app start, not by this harness. | **Resolved in W84-A** (`results/rn/W84A_HERMES.md`): real Java calls `System.loadLibrary` and the merged `_so()` natives; Hermes initialises and executes the bundled HBC up to the RN bridge (`__fbBatchedBridgeConfig`). 338 JNI natives registered. |
| 2 | **JSI / bridge** | Bare Hermes has no `__fbBatchedBridgeConfig` / `nativeModuleProxy`, so the bundle stops at RN's `MessageQueue`. | Build a `JSIExecutor` (or `CatalystInstanceImpl.initializeBridge` / `ReactInstanceManager`) so the bundle reaches `AppRegistry` and the first `UIManager`/Fabric call. |
| 3 | **graphics** | Not reached. No `ANativeWindow`/`AndroidBitmap`/`EGL`/`GLES` call yet. EGL/GLES remain structured traps by design. | After #2. |
| 4 | **threads** | Not separately reached; Hermes ran single-threaded. `pthread_create`, TLS and the pthread shims are present; seccomp keeps threads/futex/`tgkill` self allowed. | After #2. |

## 4. What the run proves

* ADR-0022 confinement is real and fail-closed: the loader's
  `require_confinement` gate is satisfied only because Landlock + seccomp +
  no-new-privs + fd/env/rlimit hygiene are active in the child.
* Six real RN/Hermes x86_64 shared objects (10 MB of C++ with IFUNCs, TLS and
  `init_array` constructors) map, relocate, bind against the Rust bionic table
  and run their constructors **inside** that confinement.
* `libfbjni.so` and both merged RN libraries (`libreactnative`, `libhermestooling`)
  initialise JNI successfully, registering their native tables
  (`HybridData$Destructor.deleteNative`, `NativeRunnable.run`,
  `ThreadScopeSupport.runStdFunctionImpl`, `OpenSourceMergedSoMapping.*_so`).
* The previous JNI_OnLoad blocker and the host-stdio SIGSEGV are gone.

## 5. Next

1. **W84-A** drove the merged per-library onloads from real Java, initialised a
   Hermes runtime and executed the bundled HBC (`results/rn/W84A_HERMES.md`).
2. Install the RN bridge globals (`__fbBatchedBridgeConfig`, `nativeModuleProxy`)
   or build a `JSIExecutor`/`CatalystInstance`, so the bundle passes
   `BatchedBridge` and reaches `AppRegistry` and the first `UIManager`/Fabric
   call.
3. Re-run to reach the graphics/threads blockers (EGL/GLES traps, Hermes GC).
4. W72-A wires `System.loadLibrary` into the runtime (`native_bridge.rs` /
   `acos-cli` / `acos-mcp`) on top of this loader + confined NDK host.

## 6. Acceptance (W79-A)

| check | result |
|---|---|
| `cargo test -p acos-jni --test rn_native_stack` | 2 passed; `W76A_SUMMARY` has **0 blockers** |
| `cargo test -p acos-native-loader --lib` | 9 passed (incl. the two new stdio-map tests) |
| `cargo clippy -p acos-jni -p acos-native-loader --all-targets --no-deps -- -D warnings` | clean |
| `cargo check --workspace --all-targets` | clean |
| `tools/diff-dexprogs.sh --corelib real` | **27 pass / 0 fail / 0 unsupported** |
