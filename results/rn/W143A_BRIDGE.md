# W143-A — RN legacy bridge runs the bundle, mounts the root, captures the first UIManager batch

Worktree `.cache/wt-w143a` (detached HEAD `5259240`, the W140-A commit; the
main tree had already moved on to `21a633e`). Built with
`CARGO_TARGET_DIR=.cache/target-w143a`, `CARGO_PROFILE_DEV_DEBUG=0`, through
`tools/guarded.sh`. Changes are uncommitted; nothing was committed. Scratch
worktree to be deleted.

## Result at a glance

| stage | W140-A | W143-A |
|---|---|---|
| `runJSBundle` -> bundle evaluation | abort: `__fbBatchedBridgeConfig is not set` | **bundle runs** (`setUpDefaltReactNativeEnvironment` completes) |
| `initializeRuntime` (`nativeModuleProxy`) | posted but never observed as run | **runs first** on the main-looper JS queue |
| `AppRegistry.runApplication('rnhello')` | not reached | **reached**, Paper renderer commits |
| captured `ACOS_RN_OP` | 0 | **19** (`createView` + `setChildren`), full view tree |
| registered natives | 45 | **85** (fbjni also loaded) |

Raw evidence: `results/rn/w143a-ops.json` — `ops` (19), the full
`ACOS_RN_BRIDGE` stage trail, and the summary.

## 1. Root cause of `__fbBatchedBridgeConfig is not set`

The fixture is **legacy Paper**: `MainApplication.isNewArchEnabled() == false`
and `isHermesEnabled() == true`. The bundle therefore expects the legacy
`global.nativeModuleProxy` (set by `JSIExecutor::initializeRuntime`) and, only
if that is absent, `global.__fbBatchedBridgeConfig`.

`CatalystInstanceImpl.Builder.build()` -> `Instance::initializeBridge` does:

```cpp
jsQueue->runOnQueueSync([...] {
    nativeToJsBridge_ = std::make_shared<NativeToJsBridge>(...);
    nativeToJsBridge_->initializeRuntime();   // POSTS to the JS queue
});
```

`NativeToJsBridge::initializeRuntime()` -> `runOnExecutorQueue(...)` posts the
`JSIExecutor::initializeRuntime` task; it does **not** run inline. The probe was
building its `JSBundleLoader` with `loadSynchronously = true`:

```java
JSBundleLoader.createFileLoader(bundlePath, bundlePath, /* loadSynchronously */ true);
```

`loadSynchronously=true` routes `runJSBundle` to
`NativeToJsBridge::loadBundleSync`, which evaluates the bundle **directly on the
calling (main) Java thread**, i.e. before the queued `initializeRuntime` has
run. The bundle's `setUpDefaltReactNativeEnvironment` then sees neither
`global.nativeModuleProxy` nor `__fbBatchedBridgeConfig` and aborts in
`Libraries/BatchedBridge/NativeModules.js`.

Fix: use the asynchronous file loader (`loadSynchronously = false`). `runJSBundle`
then posts `loadBundle` behind `initializeRuntime` on the same main-looper queue
(the probe's single-threaded `ReactQueueConfigurationSpec`), and the probe's
`Looper.loop()` drains both in order. This is exactly the order real Android
gets from `ReactInstanceManager`: the runtime is installed on the JS thread
first, then the bundle is queued behind it. No native change is needed, and no
`__fbBatchedBridgeConfig` is produced in this RN version (the config JSON only
exists for the old executor; JSI uses `nativeModuleProxy`).

## 2. Subsequent blockers (and the probe paths around them)

Driving the async path surfaced four more blockers. Each one stops the bundle
before any UI op, and each is either a probe-harness gap or a VM/JNI gap
outside this work item; the probe now documents and works around them so the
op capture can proceed.

| # | blocker | evidence | disposition |
|---|---|---|---|
| 1 | `AndroidChoreographerProvider` -> `Choreographer.<init>` -> `DisplayManagerGlobal` -> `ServiceManager.rawGetService("display")` NPE | NPE frames `ServiceManager.rawGetService`, `Choreographer.getRefreshRate`, `ReactChoreographer._init_$lambda$2` | probe-injected `VirtualChoreographerProvider` that posts each requested frame back to the main looper (16 ms synthetic clock). The display/binder service is future display work. |
| 2 | `ReadableNativeMap.localTypeMap` casts a null enum value: `null cannot be cast to non-null type com.facebook.react.bridge.ReadableType` (first hit via `DeviceInfoModule` -> `WritableMap.toHashMap()`) | `js-exception` for `DeviceInfo` | `ReadableType.<clinit>` is never run: fbjni reads the enum constants with JNI `GetStaticObjectField`, but the VM's `jni_GetStaticFieldID` does not initialise the class (the JNI spec requires it). Probe now calls `ReadableType.values()` early, which runs `<clinit>`. **Real fix belongs in `acos-jni` `jni_GetStaticFieldID`.** |
| 3 | `AndroidInfoModule.uiMode()` -> `ContextWrapper.getSystemService` NPE (null base `ProbeContext`) | NPE frames `ContextWrapper.getSystemService`, `AndroidInfoModule.uiMode` | probe-side `PlatformConstants` `BaseJavaModule` returning the real `Build` constants and `uiMode="normal"`. |
| 4 | `JNI_UNSUPPORTED: FromReflectedMethod is not implemented` | `vm-error` after `UIManager.getConstants` | only triggered by a **sync** `@ReactMethod` (Java `MethodInvoker` construction). The probe's `getAndroidID` was the sole sync method, and `Platform.android.js` never calls it, so it was omitted. **Real fix is a `FromReflectedMethod` implementation in `acos-jni`.** |

`ExceptionsManager` was also the point where the above JS errors were being
silently swallowed (`ReleaseDevSupportManager`); the probe now registers a
logging `BaseJavaModule` under the same name so JS errors appear as
`ACOS_RN_BRIDGE {"phase":"js-exception",...}` stages.

`ACOS_RN_OP` is emitted by the recording `UIManagerModule` subclass. Its five
overrides carry `@ReactMethod` (Java reflection does not inherit method
annotations, and `JavaModuleWrapper.findMethods` uses `declaredMethods`), and
they do not forward to the real `UIManager`, because the probe registers no view
managers.

## 3. Captured UI ops vs the fixture tree

`0xACE5 = 44261` is the root tag passed to `runApplication`. The 19 ops replay
the fixture's element tree (`fixtures/apps/rn-hello/index.js`):

```
44261 RCTView (AppContainer)            <- setChildren [27]
  27  RCTView  flex:1, pointerEvents     <- setChildren [25]
    25  RCTView  testID=hello-root       <- setChildren [5, 9, 17, 23]
      5  RCTText testID=hello-text       "Hello ACOS"        (raw text 3)
      9  RCTText subtitle                "React Native + Hermes" (raw text 7)
      17 RCTView (Button)                <- setChildren [15]
        15 RCTText "TAP ME"              (raw text 13)
      23 RCTText testID=count-text       "count: 0"          (raw text 19)
```

That is `View(style.container, testID hello-root)` containing the two heading
`Text`s, the `Button` (an `RCTView` + `RCTText`), and the count `Text` — exactly
the structure in the fixture source. The full op list is `results/rn/w143a-ops.json`.

## 4. Files touched (worktree, uncommitted)

* `crates/acos-jni/tests/java/dev/acos/fixtures/rnhello/BridgeProbe.java` —
  async file loader; `VirtualChoreographerProvider`; `ReadableType.<clinit>`
  pre-init; logging `ExceptionsManager`; service-free `PlatformConstants`;
  `@ReactMethod` on the recording UIManager overrides (no forwarding);
  drain diagnostics and the `quit()` sentinel handling; bisect `diag` stages.
* `crates/acos-jni/tests/data/rn-bridge-probe.dex` — rebuilt from the above.
* `crates/acos-jni/tests/rn_bridge.rs` — artifact renamed
  `results/rn/w140a-ops.json` -> `w143a-ops.json`, op extractor fixed to split on
  the `ACOS_RN_OP ` marker (the child worker mirrors guest stdout as
  `W88A_PROBE <line>`, so `strip_prefix` never matched).
* `results/rn/w143a-ops.json` — new evidence artifact.

No VM, `acos-sandbox`, `acos-native-loader`, lifecycle/session, resource-native,
Flutter, or boot-cache source was changed (the temporary `throw_new` NPE
diagnostic in `crates/acos-vm/src/vm.rs` used only for diagnosis was reverted).

## 5. Acceptance

| check | result |
|---|---|
| `rn_bridge` | **2 passed** (19 ops captured; `results/rn/w143a-ops.json`) |
| `rn_hermes` | **2 passed** |
| `rn_native_stack` | **2 passed** |
| `flutter_hello` | **2 passed** (verified in the main tree, 27.8 s). The W143-A worktree lacked a `.cache/flutter-1` symlink, so it ran the *fixture* snapshot (`"source":"fixture"`) and the unconditional `ACOS_DART_POINTER` assert failed; the main tree has `libapp_probe.so` (721800 B), the instrumented snapshot is selected, and the test passes. No code change was involved. |
| `cargo clippy --workspace --all-targets --no-deps -- -D warnings` | **clean** (`Finished`) |
| `cargo check --workspace --all-targets` | **clean** (`Finished`) |
| `tools/diff-dexprogs.sh --corelib real` | **27 pass / 0 fail / 0 unsupported** (27) |

## 6. Next (real, non-probe work)

1. `acos-jni`: make `GetStaticFieldID` (and `GetStaticMethodID`) initialise the
   class, per the JNI spec. This removes the `ReadableType.<clinit>` probe hack.
2. `acos-jni`: implement `FromReflectedMethod`/`FromReflectedField` (and the
   `ToReflected*` counterparts) so sync `@ReactMethod`s can be registered.
3. Display/ServiceManager: provide a `display` service so a real
   `android.view.Choreographer` can be constructed, replacing the virtual
   provider.
4. `ProbeContext`: provide the framework system services (`UI_MODE_SERVICE`,
   `TELEPHONY_SERVICE`, ...) instead of module-level shims, so the real
   `PlatformConstants`/`DeviceInfo` modules run.
