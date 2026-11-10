# `rn-hello` Hermes runtime — confined run (W84-A)

**Date:** 2026-11-11. **Fixture:** `fixtures/build/apps/rn-hello.apk`
(React Native 0.81.4, Hermes HBC96, x86_64). **Harness:**
`crates/acos-jni/tests/rn_hermes.rs`. It runs real Java inside the VM and lets
Java drive the *runtime* `System.loadLibrary` path that `SoLoader` uses, then
calls the SoLoader-style merged-native initialisers and executes the bundled
Hermes bytecode.

This builds on `results/rn/FIRST_RUN.md` (W76-A/W79-A), which proved the raw
loader maps/relocates/`JNI_OnLoad`s all six `.so`s under confinement.

## 1. Result at a glance (W84-A)

| stage | outcome |
|---|---|
| confinement | **active** — `PR_SET_NO_NEW_PRIVS`, `close_fds`, `sanitize_env`, `rlimits`, Landlock (ABI 10, 7 rules), seccomp (92 insns, 61 denied syscalls) |
| core library | `CoreLib::Real` (Android 14 boot classpath) |
| APK DEX into the VM | **2** files (`classes.dex`, `classes2.dex`) |
| `System.loadLibrary` (from Java) | **6 / 6** succeed in RN dependency order |
| merged `OpenSourceMergedSoMapping.*_so()` | **14 / 16** return `0`; `jsinspector`/`mapbufferjni` are not in the OSS merged libs |
| registered JNI natives after merged onloads | **338** |
| Hermes runtime init | **ok** (`facebook::hermes::makeHermesRuntime`) |
| trivial JS eval | **ok** — `acosLog('hello from hermes ' + (1 + 1))` → `hello from hermes 2`; expression value `42` |
| bundled HBC (`assets/index.android.bundle`, 1 501 379 B) | **executed** until the first native-module call |
| SIGSEGV / abort | none |

Hermes ran the real Metro bundle and stopped at the React Native bridge
boundary — the JS `MessageQueue` invariant:

```
__fbBatchedBridgeConfig is not set, cannot invoke native modules
Invariant Violation: __fbBatchedBridgeConfig is not set, cannot invoke native modules
    at invariant (out/index.android.bundle:2339:26)
    at anonymous (out/index.android.bundle:2454:35)
    at loadModuleImplementation (out/index.android.bundle:161:14)
    at guardedLoadModule (out/index.android.bundle:99:38)
    at metroRequire (out/index.android.bundle:41:92)
    ...
    at anonymous (out/index.android.bundle:16207:59)   // NativeModules / BatchedBridge
```

That is the point where RN JS asks for the native-module proxy that
`JSIExecutor::initializeRuntime` installs — i.e. Hermes executed the bundle up
to the first native call, which is the W84-A success boundary. Fabric /
`UIManager` are not reached (graphics stays a structured trap).

## 2. What was added

| file | role |
|---|---|
| `crates/acos-jni/tests/rn_hermes.rs` | confined child + runtime-path driver |
| `crates/acos-jni/tests/java/dev/acos/fixtures/rnhello/HermesProbe.java` | the Java harness entry (`System.loadLibrary`, merged `_so()`, probe native) |
| `crates/acos-jni/tests/data/rn-hermes-probe.dex` | compiled `HermesProbe` (checked in) |
| `fixtures/apps/rn-hello/native/hermes_probe.cpp` | our NDK fixture lib: Hermes runtime + JS eval + HBC |
| `tools/rn-hermes-probe.sh` | builds the probe `.so` and the driver dex from the cached AARs |
| `results/rn/w84a-hermes-probe.json` | raw stages/summary |

The probe `.so` links against the **same** `libhermes.so` / `libjsi.so` /
`libc++_shared.so` mappings the app loaded; the JNI runtime de-duplicates
`DT_NEEDED` by SONAME, so there is no second copy of the runtime.

Run:

```
tools/rn-hermes-probe.sh
cargo test -p acos-jni --test rn_hermes -- --nocapture
```

## 3. The path, in RN's own terms

1. `System.loadLibrary("hermes" | "reactnative" | "hermestooling" | …)` from
   Java. This is the real `Runtime.nativeLoad` seam (`crates/acos-jni/src/load.rs`)
   that `SoLoader` calls; each top-level `JNI_OnLoad` runs from Java and
   registers the merged `lib*_so` natives on
   `com/facebook/react/soloader/OpenSourceMergedSoMapping`.
2. `OpenSourceMergedSoMapping.INSTANCE.lib<sub>_so()` for every folded
   sub-library. These are the exact entry points `SoLoader.invokeJniOnload`
   calls; each runs the original `JNI_OnLoad` folded in by `jni_lib_merge`
   (`pre_merge_jni_libraries`), including `hermes_executor`,
   `hermesinstancejni`, `jsijniprofiler`, `reactnativejni`, `uimanagerjni`.
3. Our `libhermesprobe.so` (loaded with `System.loadLibrary("hermesprobe")`)
   calls `facebook::hermes::makeHermesRuntime()`, installs a host `acosLog`
   function, evaluates JS, then evaluates the app's HBC.

## 4. Blockers found and fixes

| # | category | blocker | fix / status |
|---|---|---|---|
| 1 | **sandbox/entropy** | `makeHermesRuntime` threw `random_device failed to open /dev/urandom: Permission denied`. libc++'s `std::random_device` opens `/dev/urandom`; Landlock had no rule for it. | Landlock rejects character devices (`EINVAL`), so the harness grants `/dev` **read-only** (READ_FILE only). Recorded as a sandbox recommendation: the product `ConfineProfile` should grant entropy access. |
| 2 | **merged natives** | `libreactnative_so()` / `libhermestooling_so()` return `UnsatisfiedLinkError`. | **Not a bug**: the *parent* names are the merged libs' own top-level `JNI_OnLoad`, already run by `System.loadLibrary`; the merge section only carries the folded sub-libraries. `SoLoader` only ever invokes sub-library names. |
| 3 | **merged natives** | `libjsinspector_so()` / `libmapbufferjni_so()` absent. | These sub-libs are not built into the OSS 0.81 merged `libreactnative` (new-arch-only); the rest of the mapping registers cleanly. |
| 4 | **JSI / bridge** | HBC throws `__fbBatchedBridgeConfig is not set, cannot invoke native modules`. | **Next blocker.** Running JS from a bare Hermes runtime cannot satisfy RN's `MessageQueue`; the bundle needs the `JSIExecutor`-installed globals (`__fbBatchedBridgeConfig`, `nativeModuleProxy`, `__turboModuleProxy`). Reaching that means driving `CatalystInstanceImpl.initializeBridge` / `ReactInstanceManager` (legacy Paper), which needs a `Context` and the `ReactApplicationContext`. |
| 5 | **threads** | Not separately reached; Hermes' GC/`random_device` ran single-threaded. | `pthread`/TLS shims present; seccomp keeps threads/futex/`tgkill` self allowed. |
| 6 | **graphics** | Not reached. | EGL/GLES remain structured traps by design. |

## 5. `acos launch --native` note

`tools/rn-hello`'s Activity chain is **still blocked before `Application`** on
HEAD: `acos launch --native` reports
`ClassNotFoundException: dev.acos.fixtures.rnhello.MainApplication … path:
DexPathList[[], …]` (1651 boot classes, no app DEX). That is the runtime
lifecycle/`ActivityThread` path owned by other workstreams, so W84-A drives the
same Java classes from the in-VM harness instead (this file).

## 6. Acceptance (W84-A)

| check | result |
|---|---|
| `cargo test -p acos-jni --test rn_native_stack` | 2 passed, 0 blockers |
| `cargo test -p acos-jni --test rn_hermes` | 2 passed; 338 merged natives registered |
| `cargo clippy -p acos-jni -p acos-native-loader --all-targets --no-deps -- -D warnings` | clean |
| `cargo check --workspace --all-targets` | clean |
| `tools/diff-dexprogs.sh --corelib real` | see §6 in the run log |

## 7. Next

1. Install the RN bridge globals into the Hermes runtime (or build a
   `JSIExecutor` around it) so the bundle passes `BatchedBridge`, then
   `AppRegistry.registerComponent('rnhello', …)` and the first
   `UIManagerModule`/Fabric call.
2. Wire the same merged-onload sequence into `System.loadLibrary` at the
   product level once `acos launch --native` can instantiate the Application.
