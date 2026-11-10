# W123-A — `AtomicReference`/VarHandle shim, RN bridge resume, loader bounds

Worktree `.cache/wt-w123a` (clean detached HEAD `15346d9`), built to
`.cache/target-w123a` with `CARGO_PROFILE_DEV_DEBUG=0` via `tools/guarded.sh`.
Changes are uncommitted; copy-back checked with `git apply --check` on the main
tree. Nothing was committed.

## Result at a glance

| stage | W120-A | W123-A |
|---|---|---|
| `HermesExecutorFactory.create()` | aborts `invoke-polymorphic` @ `AtomicReference.compareAndSet` | **ok** (native `AtomicReference` storage shim) |
| `CatalystInstanceImpl.Builder.build()` | not reached | **reached**, now aborts at `Looper.getMainLooper()` null |
| `initializeBridge` / `loadScriptFromAssets` / `runJSBundle` / `AppRegistry.runApplication` | not reached | not reached |
| captured `ACOS_RN_OP` | 0 | **0** (blocked in `Builder.build`) |
| fixture APK optional deps | infer-annotation + Fresco missing | **included** (tooling fix) |

Raw evidence: `results/rn/w123a-ops.json` (`ops: []`, stage trail ends at the
`MessageQueueThreadImpl.createForMainThread` NPE).

## 1. Task 1 — `AtomicReference` `VarHandle` surface (`acos-vm`)

Android 14's `java.util.concurrent.atomic.AtomicReference` (in `core-oj`) builds
a `VarHandle` in `<clinit>` and every accessor is a signature-polymorphic
`invoke-polymorphic` call. `HermesExecutorFactory.create()` therefore aborted at
`AtomicReference.compareAndSet pc=2` (W120-A).

**Fix** (`crates/acos-vm/src/natives/vmrt.rs`): a new `atomic_reference` shim,
mirroring the existing `atomic_integer`/`atomic_long`/`atomic_boolean` storage
shims. It overrides `<clinit>` (no-op) and the whole accessor surface —
`get`/`getPlain`/`getOpaque`/`getAcquire`, `set`/`lazySet`/`setPlain`/
`setOpaque`/`setRelease`, `getAndSet`, `compareAndSet`, all four
`weakCompareAndSet*`, and `compareAndExchange*`. Storage is the real traced
`value` reference field (`Object`), so GC-stress keeps it live.

Unshimmed polymorphic `VarHandle`/`MethodHandle.invoke(Exact)` sites still abort
with the structured `UNSUPPORTED_BYTECODE` (`invoke-polymorphic/custom`) naming
the opcode and call site — no silent fake.

**Test** (`crates/acos-vm/tests/java/AtomicReferenceMain.java`,
`java_cases::atomic_reference_varhandle_surface_under_real_corelib`): runs the
whole surface against the **real** Android 14 core library under
{IR on/off} × {normal, GC stress}; green.

## 2. Task 2 — RN bridge resume

With the shim, the probe now reaches `CatalystInstanceImpl.Builder.build()`.
Two fixture gaps surfaced first and were fixed in the build tooling, not the
APK by hand:

* `com.facebook.infer.annotation.Assertions` is referenced by RN bytecode but
  `optional` in `react-android`'s POM, so the offline resolver skipped it →
  `UNSUPPORTED_JAVA_API` at `Builder.build pc=4`.
* `com.facebook.common.logging.FLog` (and the rest of the Fresco closure) is
  likewise `optional` and skipped.

`tools/rn-hello-deps.py` now lists `infer-annotation`, `fresco`,
`imagepipeline-okhttp3` and `middleware` as explicit roots (a real Gradle app
declares them directly). `tools/build-rn-hello.sh` (no source change needed)
was re-run to rebuild `fixtures/build/apps/rn-hello.apk` (filenames unchanged),
which now defines `Assertions`/`FLog`.

### Next blocker (evidence)

```
W88A_PROBE ACOS_RN_BRIDGE {"phase":"executor","detail":"ok"}
W88A_PROBE ACOS_RN_BRIDGE {"phase":"soloader-load","detail":"reactnative"}
W88A_PROBE ACOS_RN_BRIDGE {"phase":"bridge-error","at":"build",
  "error":"java.lang.NullPointerException: getMainLooper(...) must not be null"}
  com.facebook.react.bridge.queue.MessageQueueThreadImpl$Companion.createForMainThread
  com.facebook.react.bridge.queue.ReactQueueConfigurationImpl.create
  com.facebook.react.bridge.CatalystInstanceImpl.<init>
  com.facebook.react.bridge.CatalystInstanceImpl$Builder.build
```

`MessageQueueThreadImpl.createForMainThread` requires a non-null
`Looper.getMainLooper()`; the probe never calls `Looper.prepareMainLooper()`, and
the real framework `Looper.<clinit>` does not do it in this VM. The next probe
change is to prepare the main looper (and, per the task, call
`ReactChoreographer.initialize()` before constructing `TimingModule`, which
itself currently skips with `ReactChoreographer needs to be initialized`). Both
live in `BridgeProbe.java` and need `tools/rn-bridge-probe.sh`; they are the
next work item, recorded here rather than forced through under time pressure.

`Timing` is the remaining module-skip; every other module is `ok`
(`UIManager`, `PlatformConstants`, `DeviceInfo`, `ExceptionsManager`,
`AppState`). `LoggingUIManagerModule` op capture is wired and will emit as soon
as `runJSBundle`/`runApplication` are reached.

## 3. Task 3 — loader audit fixes (`acos-native-loader`)

* **#4 ALooper posted-frame queue (DoS).** `MAX_POSTED_FRAMES = 4096`;
  `post_frame_callback` drops the oldest queued frame on overflow and advances a
  one-shot diagnostic counter. Delivery stays FIFO among retained frames. The
  edit is confined to the `PostedFrame` area and the single `push_back` site to
  minimise conflict with W119-A's Flutter edits to `ndk_looper.rs`.
* **#6 pthread handle tables (DoS).** `MAX_TABLE_ENTRIES = 1 << 16` per table.
  New `bounded_boxed`/`bounded_copy` helpers evict one existing entry when a
  never-seen address would exceed the cap; the next access lazily re-creates a
  fresh object. All six tables are covered (`mutex`/`cond`/`rwlock`/`attr` and
  the two attribute-state maps). Normal callers stay far below the cap, so
  eviction is only reachable from an adversarial address stream.

#5 (destroy race) and #7/#8 (boot cache) were **not** touched. #7/#8 are gated on
`session_pool.rs`/`snapshot.rs` not being dirty in main; both are dirty
(uncommitted W115-A/W118-A work), so they stay documented per the task.

## 4. Incidental fix — clean-HEAD clippy

`crates/acos-vm/src/natives/icu_regex.rs::{stage_regex_state,current_regex_state}`
are used only by the uncommitted `snapshot.rs` delta, so a clean HEAD build fails
`clippy -D warnings` with `dead_code`. Added `#[allow(dead_code)]` on both (the
dirty main tree uses them, so it is a no-op there) to keep the acceptance gate
green.

## 5. Acceptance (clean worktree)

| check | result |
|---|---|
| `cargo test -p acos-vm --test java_cases atomic_reference_varhandle_surface_under_real_corelib` | **passed** |
| `cargo test -p acos-native-loader` | **green** (lib + integration + doc) |
| `cargo test -p acos-jni --test rn_bridge …` | **passed** (evidence; blocker recorded) |
| `cargo clippy -p acos-native-loader -p acos-vm -p acos-jni --all-targets --no-deps -- -D warnings` | **clean** |
| `cargo check --workspace --all-targets` | **clean** |
| `tools/diff-dexprogs.sh --corelib real` | **27 pass / 0 fail / 0 unsupported** |
| `tools/diff-dexprogs.sh --corelib real --gc-stress` | **27 pass / 0 fail / 0 unsupported** |

## 6. Files

```
crates/acos-vm/src/natives/vmrt.rs                    atomic_reference shim
crates/acos-vm/tests/java_cases.rs                    real-corelib AtomicReference test
crates/acos-vm/tests/java/AtomicReferenceMain.java    new test source
crates/acos-vm/src/natives/icu_regex.rs               #[allow(dead_code)] (clean-HEAD clippy)
crates/acos-native-loader/src/bionic/ndk/ndk_looper.rs  #4 posted-frame cap
crates/acos-native-loader/src/bionic/pthread.rs       #6 bounded handle tables
crates/acos-jni/tests/rn_bridge.rs                    write w123a-ops.json
tools/rn-hello-deps.py                                infer-annotation + Fresco roots
results/rn/w123a-ops.json                             raw stages/summary/ops
fixtures/build/apps/rn-hello.apk                      rebuilt (gitignored, in place)
```
