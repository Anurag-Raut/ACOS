# W127-A — Real main-thread Looper, `ReactChoreographer`, and the RN bridge past `initializeBridge`

Worktree `.cache/wt-w127a` (clean detached HEAD `1aead8a`), built to
`.cache/target-w127a` with `CARGO_PROFILE_DEV_DEBUG=0` via `tools/guarded.sh`.
Changes are uncommitted; copy-back checked with `git apply --check` on the main
tree. Nothing was committed.

## Result at a glance

| stage | W123-A | W127-A |
|---|---|---|
| `Looper.getMainLooper()` | `null` → `MessageQueueThreadImpl.createForMainThread` NPE | **prepared** (`Looper.prepareMainLooper()`), `Looper (main, tid 1)` |
| `ReactChoreographer.initialize` | not called (`Timing` module skipped) | **ok**; `TimingModule` constructs |
| `ReactQueueConfigurationImpl.create` (real JS/native background queues) | not reached | reaches it, then **deadlocks** in native `runOnQueueSync` |
| `CatalystInstanceImpl.Builder.build()` | NPE before native `initializeBridge` | **ok** (single-threaded main queues) |
| `initializeWithInstance` | not reached | **ok** |
| `runJSBundle` → `loadScriptFromAssets` | not reached | reached; asset loader fails, file loader loads the HBC |
| captured `ACOS_RN_OP` | 0 | **0** (next VM opcode gap, below) |

Raw evidence: `results/rn/w127a-ops.json` (`ops: []`, stage trail ends at
`initializeWithInstance`). The probe emits each stage three ways — captured
stdout, the VM log sink, and (added here) `android.util.Log.i` — so the first
failing stage survives any abort.

## 1. Task 1 — prepare the real main-thread Looper (`BridgeProbe.java`)

Real Android builds the main `Looper` in `ActivityThread.main`
(`Looper.prepareMainLooper()`); the probe runs `main` on the VM's thread 0, so it
now calls `Looper.prepareMainLooper()` before constructing anything. The looper
is a real `MessageQueue` (VM native `nativeInit`), so the `acos-vm`
`natives::looper` pump drives it like any other queue.

`getMainLooper()` is then non-null and `MessageQueueThreadImpl.createForMainThread`
succeeds. Evidence (stage trail): `{"phase":"main-looper","detail":"Looper (main, tid 1) {3d2}"}`.

## 2. Task 2 — `ReactChoreographer.initialize`

`TimingModule`'s constructor calls `ReactChoreographer.Companion.getInstance()`
before the choreographer is initialized (W120-A). The probe now calls
`ReactChoreographer.initialize(AndroidChoreographerProvider.INSTANCE)` right
after the main looper. `AndroidChoreographerProvider` is a Kotlin `object`, so
its instance is used directly; the `ReactChoreographer` constructor's
`UiThreadUtil.runOnUiThread` sees the current thread is the main looper and runs
inline, and `Choreographer.getInstance()` resolves against the prepared main
looper.

Result: `{"phase":"module","detail":"Timing ok"}`; the module registry is now
`count=6` (all real modules) instead of 5.

## 3. Root fix — the bridge runs single-threaded on the main looper

`CatalystInstanceImpl.Builder.build()` reached the native `initializeBridge`
once the main looper existed, and then **hung**. gdb shows the VM owner thread
in `pthread_cond_wait` reached from `libc++_shared.so`
(`acos_native_loader::bionic::pthread::pthread_cond_wait` ← libc++
`condition_variable::wait`).

Root cause (RN source, `ReactCommon/cxxreact/Instance.cpp:64`):

```cpp
jsQueue->runOnQueueSync([this, &jsef, jsQueue]() mutable {
  nativeToJsBridge_ = std::make_shared<NativeToJsBridge>(...);
});
```

`JMessageQueueThread::runOnQueueSync` (JNI, `JMessageQueueThread.cpp:66`) posts
the runnable to the JS queue and blocks on a C++ `condition_variable` until the
JS queue runs it. Real Android's JS queue is an OS thread, so it runs
concurrently. ACOS's queues are **green** threads on the single VM host thread;
the host is blocked inside the native call, so the JS green thread can never
run — a genuine deadlock, not a probe bug.

Fix: construct the `ReactQueueConfigurationSpec` with the **main thread spec for
all three roles** (UI, JS, native modules):

```java
new ReactQueueConfigurationSpec(
    MessageQueueThreadSpec.mainThreadSpec(),
    MessageQueueThreadSpec.mainThreadSpec());
```

`isOnThread()` is then true, so `runOnQueueSync` runs the setup inline (no
native wait) and the JS executor / `NativeToJsBridge` are created on the main
green thread. Asynchronous work posted by the bridge goes to the main looper's
`Handler`.

To drain that asynchronous work deterministically, the probe posts a delayed
quit and runs the looper:

```java
new Handler(Looper.getMainLooper()).postDelayed(() -> {
  stage("drain", "quit");
  Looper.myLooper().quit();
}, 1000);
Looper.loop();
```

The Rust harness runs the probe with `vm.run_virtual(...)` so the
`acos-vm` virtual-time scheduler (`natives::looper::pump_virtual`, W118-A)
advances the guest clock to the quit message instead of waiting in real time.

Evidence with real background queues (the deadlock): the child never returns;
gdb backtrace at `libc++_shared` text mapping + `pthread_cond_wait`. With
main-thread queues: `{"phase":"catalyst","detail":"ok"}` and
`{"phase":"initializeWithInstance","detail":"ok"}` are emitted and the run
returns.

## 4. Loader — file loader over the extracted HBC (`BridgeProbe.java`)

`runJSBundle` reaches `loadScriptFromAssets` first and the asset loader throws
RN's `Unable to load script ... bundle 'roid.bundle'`: the native
`AAssetManager_open` (NDK `AssetManager`) cannot stream
`assets/index.android.bundle` out of the fixture APK in the VM. The harness
already extracts the same bytes to the scratch `bundlePath`, and
`JSBundleLoader.createFileLoader`'s native `JSBigFileString::fromPath` runs in
the host process and opens that host path directly. The probe now prefers the
file loader when `bundlePath` is non-empty (the same Hermes bytecode, just via
`std::ifstream` rather than `AAssetManager`). Trailing evidence:
`{"phase":"loader","detail":"file:/…/index.android.bundle"}`.

## 5. Next blocker (VM-owned, not harness)

With the file loader, `runJSBundle` enters the native `loadScriptFromString` →
JS queue and aborts inside a JNI `Call<Type>Method`:

```
W88A_PROBE_ERR acos-vm error: VM_INTERNAL: internal VM error: JNI_NATIVE_FAULT:
  JNI_NATIVE_FAULT: Call<Type>Method: vm error: opcode invoke-polymorphic
  (invoke-polymorphic/custom) is not implemented
  (at Ljava/util/concurrent/ConcurrentLinkedQueue$Node;.<init> pc=5)
```

`ConcurrentLinkedQueue$Node.<init>` uses a `VarHandle` (`ITEM.set(this, item)`,
a signature-polymorphic `invoke-polymorphic` call). W123-A shimmed
`java.util.concurrent.atomic.AtomicReference`'s accessor surface; this is a
different, unshimmed `VarHandle` site. Generic `MethodHandle`/`VarHandle`
`invoke` support (or a targeted `ConcurrentLinkedQueue` shim) is the next work
item; it is a VM opcode gap, not a `BridgeProbe`/harness bug. `ACOS_RN_OP`
remains 0.

## 6. Acceptance

| check | result |
|---|---|
| `rn_bridge` | 2 passed |
| `rn_hermes` | 2 passed |
| `rn_native_stack` | 2 passed |
| `flutter_hello` | 2 passed |
| `cargo clippy -p acos-jni --all-targets --no-deps -- -D warnings` | clean |
| `cargo check --workspace --all-targets` | clean |
| `tools/diff-dexprogs.sh --corelib real` | **27 pass / 0 fail / 0 unsupported** |

## Files touched

* `crates/acos-jni/tests/java/dev/acos/fixtures/rnhello/BridgeProbe.java` —
  `prepareMainThread` (main looper + `ReactChoreographer`), single-threaded main
  queue spec, file bundle loader, `Looper.loop()` drain, `Log.i` stage mirror.
* `crates/acos-jni/tests/rn_bridge.rs` — `vm.run_virtual(...)`, artifact renamed
  `w127a-ops.json`.
* `crates/acos-jni/tests/data/rn-bridge-probe.dex` — rebuilt with
  `tools/rn-bridge-probe.sh` (no tooling change needed).
* `results/rn/w127a-ops.json` — new evidence artifact.

No VM/`acos-sandbox`/`acos-native-loader` source file was changed (the temporary
live-log `eprintln!` used only for diagnosis was reverted). No commit.
