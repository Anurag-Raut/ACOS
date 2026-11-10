# `rn-hello` legacy RN bridge — confined run (W88-A)

**Date:** 2026-11-11. **Fixture:** `fixtures/build/apps/rn-hello.apk`
(React Native 0.81.4, Hermes HBC96, x86_64). **Harness:**
`crates/acos-jni/tests/rn_bridge.rs` + Java entry
`crates/acos-jni/tests/java/dev/acos/fixtures/rnhello/BridgeProbe.java`
(compiled to `crates/acos-jni/tests/data/rn-bridge-probe.dex` by
`tools/rn-bridge-probe.sh`).

This extends W84-A (`results/rn/W84A_HERMES.md`): instead of a bare Hermes
runtime, the Java entry builds the **real legacy (Paper) bridge** —
`BridgeReactContext`, a `NativeModuleRegistry` of real RN modules, a
`LoggingUIManagerModule` (real `UIManagerModule` subclass), the real
`JSBundleLoader`, the real `ReactQueueConfigurationSpec`, and the real
`CatalystInstanceImpl.Builder` (whose private ctor calls native
`initializeBridge`, installing `__fbBatchedBridgeConfig` / `nativeModuleProxy`).

## 1. Result at a glance (W88-A)

| stage | outcome |
|---|---|
| confinement | **active** — `PR_SET_NO_NEW_PRIVS`, `close_fds`, `sanitize_env`, `rlimits`, Landlock (ABI 10, 7 rules), seccomp (92 insns, 61 denied syscalls) |
| core library | `CoreLib::Real` (Android 14 boot classpath) |
| APK DEX into the VM | **2** files (`classes.dex`, `classes2.dex`) |
| native stack + merged onloads | **all ok** (same as W84-A; 338 natives) |
| real RN modules constructed | `UIManager`, `PlatformConstants`, `DeviceInfo`, `AppState` **ok**; `ExceptionsManager`, `Timing` **fail** (blocker 4) |
| real `UIManagerModule` | **constructed** (empty view-manager list; capture subclass active) |
| `HermesExecutorFactory().create()` | **blocked** — `ExceptionInInitializerError: SoLoader.init() not yet called` |
| `initializeBridge` / `runJSBundle` / `runApplication` | **not reached** (blocked at executor) |
| captured JS-originated UIManager ops | **none** (bridge never initialised) |

The run stops one call short of `CatalystInstanceImpl.Builder.build()`. Every
blocker below is recorded with a reproducer.

## 2. Blocker 1 — fixture DEX omitted `javax.inject` (FIXED)

**Category:** fixture tooling. `react-android`'s POM marks
`javax.inject:javax.inject:1` **optional**, and `tools/rn-hello-deps.py`
correctly skips optional transitives. But RN's `MainReactPackage` desugared
`invokedynamic` view-manager providers **implement** `javax.inject.Provider` at
runtime, so `classes2.dex` had only a *type reference*. Loading
`MainReactPackage` (or any desugared provider) failed with
`NoClassDefFoundError: javax.inject.Provider`.

**Fix:** `tools/rn-hello-deps.py` now lists `javax.inject:javax.inject:1` as an
explicit root (the same treatment as `fbjni`), and the fixture APK was rebuilt
(`ACOS_RN_ARM64=0 tools/build-rn-hello.sh build`). `javax.inject.Provider` is
now a class def in `classes2.dex`. **Owner:** fixture tooling (done).

## 3. Blocker 2 — `java.util.zip.ZipFile` natives missing (VM)

`SoLoader.init(context, flags, loader)` discovers `SoSource`s by opening the
APK. Android 14's `java.util.zip.ZipFile` calls the native
`ZipFile.open(String, int, long, boolean)`; the VM aborts with

```
UNSUPPORTED_JAVA_API: Ljava/util/zip/ZipFile;->open(Ljava/lang/String;IJZ)J
                       is not implemented (at Ljava/util/zip/ZipFile;.open pc=0)
```

`crates/acos-vm/src/natives/zip.rs` implements Inflater/Deflater/CRC32/Adler32
and UTF helpers only — there is **no `ZipFile`/`ZipInputStream` native set**.
The harness therefore primes `SoLoader`'s static state directly (blocker 3)
instead of calling `init`. **Owner:** W83-A (`acos-vm` natives). Adding
`ZipFile.open` (`(StringIJZ)J`) + its entry/read natives would let
`SoLoader.init` run for real.

## 4. Blocker 3 — reflection static write invisible to compiled `getstatic` (VM, reproducible)

To avoid blocker 2 the harness primes `SoLoader` by reflection:

```java
Field f = SoLoader.class.getDeclaredField("sSoSources");
f.setAccessible(true);
f.set(null, Array.newInstance(SoSource.class, 0));   // non-null empty array
f.get(null) != null;                                 // true  (readback works)
SoLoader.isInitialized();                             // false (compiled getstatic)
```

`SoLoader.isInitialized()` reads `sSoSources` with a normal `getstatic` and
returns **false**, i.e. the interpreter does not see the value that
`Field.set` wrote and `Field.get` reads back. The **same shape works** for
probe-DEX fields:

| field | set via reflection | read by compiled code |
|---|---|---|
| `BridgeProbe.PROBE_STATIC` (`Object`) | ok | `set-by-reflection` |
| `BridgeProbe.PROBE_ARR` (`Object[]`) | ok | length 1 |
| `BridgeProbe.PROBE_VOL` (`volatile Object`) | ok | `vol-set` |
| `SoLoader.TAG` (`String`) | ok | `SoLoader` |
| `SoLoader.sSoSources` (`private static volatile SoSource[]`) | `Field.get` non-null | **null** |

So it is specific to this APK class/field, not to static reflection or to array
or volatile fields in general. Prime suspect: the mirror's `artFieldIndex`
(`natives/reflect.rs` `field_slot`) resolving to a different slot than the
interpreter's `Vm::resolve_field` for this field — but the general cases above
say the mechanism works, so the exact divergence needs W83-A's attention.
**Owner:** W83-A (`acos-vm` reflect/interp). Reproducer:
`tools/rn-bridge-probe.sh && cargo test -p acos-jni --test rn_bridge -- --nocapture`.

**Consequence:** `HermesExecutor` has a `static { loadLibrary(); }` that calls
`SoLoader.loadLibrary("hermes")`, and `SoLoader.loadLibrary(String)` calls
`assertInitialized()`. With the static prime ineffective, the executor creation
throws `ExceptionInInitializerError: SoLoader.init() not yet called`, so
`initializeBridge` is never reached.

## 5. Blocker 4 — `ExceptionsManager` / `Timing` constructors (VM)

With the minimal module set, `ExceptionsManagerModule(null)` and
`TimingModule(context, null)` both throw
`ArrayIndexOutOfBoundsException: Index 0 out of bounds for length 0`, while
`UIManagerModule`, `AndroidInfoModule`, `DeviceInfoModule` and `AppStateModule`
construct successfully. The frames are empty on the VM, so the exact read was
not pinned; both classes construct a `JavaTimerManager`/`DevSupportManager`
path. **Owner:** W83-A (VM) / RN environment. Not on the critical path for
`initializeBridge` (the registry tolerates their absence).

## 6. What was added

| file | role |
|---|---|
| `crates/acos-jni/tests/rn_bridge.rs` | confined child + real-bridge driver |
| `crates/acos-jni/tests/java/dev/acos/fixtures/rnhello/BridgeProbe.java` | Java harness entry (context, registry, `CatalystInstanceImpl`, op capture) |
| `crates/acos-jni/tests/data/rn-bridge-probe.dex` | compiled `BridgeProbe` (checked in) |
| `tools/rn-bridge-probe.sh` | builds the probe DEX from the cached AARs |
| `tools/rn-hello-deps.py` | + `javax.inject` root (blocker 1 fix) |
| `results/rn/w88a-bridge.json` | raw stages/ops |

`BridgeProbe` uses only real RN/Android Java classes; the only substitution is
`LoggingUIManagerModule`, a `UIManagerModule` subclass that records every
JS-originated `createView`/`updateView`/`setChildren`/`manageChildren` as an
`ACOS_RN_OP {...}` JSON line before forwarding to the real implementation. The
empty view-manager list is deliberate: the real managers are blocked by the
same `SoLoader` path, and the op is captured before the real lookup, so an
empty registry still proves the JS→native command path.

Run:

```
tools/rn-bridge-probe.sh
cargo test -p acos-jni --test rn_bridge -- --nocapture
```

## 7. Next

1. W83-A: implement `java.util.zip.ZipFile` natives (blocker 2) **and/or** fix
   the static-reflection slot divergence (blocker 3). Either unblocks
   `SoLoader.loadLibrary("hermes")` and therefore `HermesExecutor`.
2. Once `HermesExecutor` constructs, `CatalystInstanceImpl.Builder.build()`
   calls native `initializeBridge`; expect the next blockers at the JSI/JNI
   boundary (`CatalystInstanceImpl` callbacks, `JavaModuleWrapper`).
3. Re-add the real view managers (`ReactViewManager`, `ReactTextViewManager`,
   …) once `SoLoader` is fixed; note they also need
   `ReactNativeFeatureFlagsCxxInterop` and `com.facebook.common.logging.FLog`
   (blocker 1 shows the fixture's optional-dep closure matters).
4. Then `runJSBundle` → `AppRegistry.runApplication` → assert the captured
   `createView` op list.

## 8. Acceptance (W88-A)

| check | result |
|---|---|
| `cargo test -p acos-jni --test rn_bridge` | **2 passed** (evidence; blockers recorded) |
| `cargo test -p acos-jni --test rn_hermes` | **2 passed** (regression) |
| `cargo test -p acos-jni --test rn_native_stack` | **2 passed** (regression) |
| `cargo clippy -p acos-jni -p acos-native-loader --all-targets --no-deps -- -D warnings` | **clean** |
| `cargo check --workspace --all-targets` | **clean** |
| `tools/diff-dexprogs.sh --corelib real` | **27 pass / 0 fail / 0 unsupported** |

All checks were run in a clean worktree of `6d4b389` with
`CARGO_TARGET_DIR=.cache/target-w88a CARGO_PROFILE_DEV_DEBUG=0`; the patch was
`git apply`-ed back into the main checkout (uncommitted).

### Files

```
crates/acos-jni/tests/rn_bridge.rs                                   (new)
crates/acos-jni/tests/java/dev/acos/fixtures/rnhello/BridgeProbe.java (new)
crates/acos-jni/tests/data/rn-bridge-probe.dex                       (new, binary)
tools/rn-bridge-probe.sh                                             (new)
tools/rn-hello-deps.py                                               (+javax.inject)
results/rn/W88A_BRIDGE.md, results/rn/w88a-bridge.json               (evidence)
```
