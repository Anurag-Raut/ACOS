# W157-A — The RN legacy bridge runs on a real `acos-runtime` session, and a synthetic tap drives the Button to `count: 1`

Worktree `.cache/wt-w157a` (detached HEAD `b845c69`, the W153-A commit) built with
`CARGO_TARGET_DIR=.cache/target-w157a`, `CARGO_PROFILE_DEV_DEBUG=0`,
`CARGO_BUILD_JOBS=2`, through `tools/guarded.sh`. Changes are uncommitted; nothing
was committed. Scratch worktree to be deleted.

## Result at a glance

| | W153-A (raw `Vm`) | W157-A (`acos-runtime` session) |
|---|---|---|
| VM + services | raw `Vm`, no service registry | `Runtime::standard_with` (real `ServiceHost` + Binder transport) |
| `ChoreographerProvider` | probe `VirtualChoreographerProvider` (16 ms synthetic clock) | **real `AndroidChoreographerProvider`** |
| `Choreographer` | virtual | **real `android.view.Choreographer`** (reads the `display` service, real vsync source) |
| `ReactChoreographer` | probe-initialised with the virtual provider | **real**, initialised with the real provider |
| `PlatformConstants` | probe `SimplePlatformConstantsModule` (`uiMode="normal"`) | **real `AndroidInfoModule`**, `uiMode` via the real `UiModeManager` + `uimode` service |
| `IUiModeManager` | not present | **added** to the AIDL subset + `ServiceHost` (`getCurrentModeType` = `UI_MODE_TYPE_NORMAL`) |
| first UIManager batch | 19 ops | **19 ops** |
| tap on the Button | not attempted | **8-op second batch, ending in `updateView tag 19 {"text":"count: 1"}`** |

Evidence: `results/rn/w157a-ops.json` — `first_batch` (19), `tap_marker`
(`{"tag":17}`), `tap_batch` (8), plus the full `ACOS_RN_BRIDGE` stage trail.

## 1. Why a runtime session removes the probe overrides

W143-A/W153-A documented four blockers that the raw `acos-jni` test VM papered
over. Two of them are exactly what `acos-runtime` installs:

* **`Choreographer` needs the `display` service.** `android.view.Choreographer`'s
  constructor calls `DisplayManagerGlobal.getInstance()` →
  `ServiceManager.rawGetService("display")`. `Runtime::with_host` installs the
  `ServiceHost` + `ServiceTransport` and registers `display`
  (`acos-services/src/display.rs`), so `AndroidChoreographerProvider` builds a
  **real** `Choreographer` whose vsync source is the runtime's
  `DisplayEventReceiver` (`fwk_vsync`). No synthetic clock.
* **`AndroidInfoModule.uiMode()` needs `UI_MODE_SERVICE`.** The real module calls
  `getReactApplicationContext().getSystemService("uimode")` and reads
  `UiModeManager.getCurrentModeType()`. The runtime now answers `uimode`.

The probe's two overrides remain available for the default raw-`Vm` mode; the
runtime test selects the real path with a `real` argument to `BridgeProbe.main`
(args `[bundle, libdir, sourceDir, apk, "real", "tap"]`). The raw-`Vm` tests pass
4 arguments and keep the virtual provider / service-free constants, so
`rn_bridge`/`rn_hermes`/`rn_native_stack` are unchanged.

## 2. The `uimode` service

`android.app.IUiModeManager` was not in the decoded AIDL subset, so the framework
proxy had no wire table and every transaction would have been
`UNSUPPORTED_ANDROID_API`. It was added to `DECLARED_INTERFACES`
(`crates/acos-aidl/src/generate.rs`) and the table was regenerated from the real
Android 14 boot classpath
(`cargo run -p acos-aidl --example gen -- --boot .cache/boot/android-34`):
`interfaces=37 methods=107 fully_decoded=106 (99.1%)`.

`crates/acos-services/src/declared.rs` gains `UiModeService`:

* `getCurrentModeType` → `UI_MODE_TYPE_NORMAL` (1) — the measured device config;
* `getNightMode` → `MODE_NIGHT_NO` (1), `getContrast` → `0.0f`;
* `isNightModeLocked` / `isUiModeLocked` / `isUiModeLockedForUser` → `false`;
* `addCallback`/`removeCallback` → recorded no-op success (the real
  `UiModeManager(Context)` constructor registers its contrast callback; ACOS has
  no contrast controller, so it is never invoked);
* every mutator (`setNightMode`, `enableCarMode`, the contrast-listener
  registration, …) → structured `UnsupportedAndroidApi`.

The registry maps `("uimode", "android.app.IUiModeManager")` and installs the
service in `ServiceHost::standard`.

The probe reaches it reflectively — `ServiceManager.getService("uimode")` +
`new UiModeManager(Context)` (the `(Context, IUiModeManager)` constructor does not
exist on this boot classpath) — because the probe is compiled against the public
`android.jar` and `UiModeManager(Context)` is `@hide`. This is the framework's own
service path, not a `PlatformConstants` shim.

## 3. The tap → second UIManager batch

`JSTouchDispatcher` resolves a target from a native `ViewGroup` hierarchy; the
probe registers no view managers, so it emits the same `TouchEvent`s
`JSTouchDispatcher` would (`START` then `END`) through the real
`UIManagerModule.getEventDispatcher()` (a real `EventDispatcherImpl`, whose flush
runs on the real `ReactChoreographer`). The probe finds the Button's tag by
scanning its recorded ops for `"testID":"hello-button"` (tag 17) and emits
`ACOS_RN_TAP_START {"tag":17}`.

The resulting second batch is 8 ops:

```
measure tag 17
measure tag 17
dispatchViewManagerCommand tag 17
dispatchViewManagerCommand tag 17
setJSResponder tag 17
clearJSResponder
dispatchViewManagerCommand tag 17
updateView tag 19 RCTRawText {"text":"count: 1"}      <- the fixture's setCount
```

`measure` is the JS `Pressability._measureResponderRegion` call; with no native
shadow tree the probe answers a synthetic 100×100 frame that contains the touch.
`dispatchViewManagerCommand` / `setJSResponder` are the Button's
ripple/responder-blocking native calls, recorded no-ops. The `updateView` is the
real JS `setCount` → `UIManager` path; it is a normal `updateView` of the existing
`count-text` raw text, exactly the fixture's `'count: ' + count`.

### The VM fix this required: `Method.invoke` must dispatch virtually

Before the fix the tap reached `onResponderRelease` but died with
`java.lang.AbstractMethodError: Lcom/facebook/fbreact/specs/NativeTimingSpec;.createTimer(DDDZ)V`.
`JavaMethodWrapper` invokes through `java.lang.reflect.Method.invoke`, and
`Class.getMethods()` returns the *declaring* method, so an inherited abstract
declaration was invoked directly. `fwk_method_invoke::method_invoke` now resolves
the effective method with `Vm::dispatch` on the receiver for non-static methods
(falling back to the declaring method for private/static methods). This is the
JLS `Method.invoke` contract; the concrete `TimingModule.createTimer` then runs
and the React update flushes.

## 4. Files touched (worktree, uncommitted)

* `crates/acos-aidl/src/generate.rs` — add `android.app.IUiModeManager` to
  `DECLARED_INTERFACES`.
* `crates/acos-aidl/src/generated/tables.rs` — regenerated (37 interfaces).
* `crates/acos-services/src/registry.rs` — `uimode` name mapping + service.
* `crates/acos-services/src/declared.rs` — `UiModeState` / `UiModeService`.
* `crates/acos-vm/src/natives/fwk_method_invoke.rs` — virtual dispatch in
  `Method.invoke`.
* `crates/acos-vm/src/natives/mod.rs` — `pub(crate)` visibility for
  `fwk_annotations` / `native_mem` (the W153-A commit left this uncommitted and
  HEAD does not build without it).
* `crates/acos-jni/tests/java/.../BridgeProbe.java` — `real`/`tap` modes, real
  provider/`AndroidInfoModule`, reflective `UiModeManager`, tap dispatch, and the
  responder `measure`/`dispatchViewManagerCommand`/`setJSResponder` recordings.
* `crates/acos-jni/tests/data/rn-bridge-probe.dex` — rebuilt from the above.
* `crates/acos-runtime/Cargo.toml` — dev-deps `acos-sandbox`, `zip`.
* `crates/acos-runtime/tests/rn_runtime.rs` — **new** integration test.
* `results/rn/w157a-ops.json` — **new** evidence.
* `Cargo.lock` — the new dev-dependency edges.

No Flutter or counter-M2 code was touched (W152-A/W154-A/W156-A own those), and
no `acos-wm` source was changed.

## 5. Acceptance

| check | result |
|---|---|
| `cargo test -p acos-runtime --test rn_runtime` | **2 passed** (19-op mount + 8-op tap batch ending `count: 1`; `results/rn/w157a-ops.json`) |
| `cargo test -p acos-jni --test rn_bridge` | **2 passed** (19 ops) |
| `cargo test -p acos-jni --test rn_hermes` | **2 passed** |
| `cargo test -p acos-jni --test rn_native_stack` | **2 passed** |
| `cargo clippy --workspace --all-targets --no-deps -- -D warnings` | **clean** (`Finished`) |
| `cargo check --workspace --all-targets` | **clean** (`Finished`) |
| `tools/diff-dexprogs.sh --corelib real` | **27 pass / 0 fail / 0 unsupported** (27) |

## 6. Honest limits

* The probe still runs the RN bridge from a Java harness, not from
  `ActivityThread`; the `ProbeContext` and `ProbeSoFileLoader` are unchanged from
  W143-A/W153-A. Only the `ChoreographerProvider`, `PlatformConstants` and
  `UiModeManager` paths are now the real ones.
* `measure`/`dispatchViewManagerCommand`/`setJSResponder` are recorded probe
  answers because the probe registers no view managers; a full native view
  hierarchy would answer them from the real `UIImplementation`.
* The `uimode` mutators and contrast listeners are structured
  `UNSUPPORTED_ANDROID_API`; only the read-only queries the RN module uses are
  answered.
