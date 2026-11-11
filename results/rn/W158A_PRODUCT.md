# W158-A — the React Native bridge becomes a library session: structured tree, in-place tap, honest `UNSUPPORTED_RN_*`

Worktree `.cache/wt-w158a` (detached HEAD `80cb4f5`, the W155-A commit) built with
`CARGO_TARGET_DIR=.cache/target-w158a`, `CARGO_PROFILE_DEV_DEBUG=0`,
`CARGO_BUILD_JOBS=2`, through `tools/guarded.sh`. Changes are uncommitted; nothing
was committed. Scratch worktree to be deleted.

## Result at a glance

| | W157-A (harness) | W158-A (`RnSession`) |
|---|---|---|
| host loop | `BridgeProbe.main` inside the test child | **`acos_runtime::RnSession`** owns it |
| op capture | parsed from guest stdout after the run | **live static op bag** read through the VM (`opsJson()`) |
| agent tree | none (raw op JSON lines) | **`RnTree`**: tag, class, testID, text, clickable; bounds absent |
| `tap` | one-shot `main` arg, re-runs the bridge | **in-place**: `TouchEvent` + `push_static_call` pump on the live session |
| `tap --text` | not available | **touchable resolution** (`TAP ME` → RCTText 15 → Button tag 17) |
| `observe --frame` | n/a | **`UNSUPPORTED_RN_NO_RENDERER`** (honest: no native view managers) |
| error model | stage strings | **structured `UNSUPPORTED_RN_*` / `WRONG_ABI`** |

Evidence: `results/rn/w158a-product.json` (mount tree, tap tree, summary).

## 1. What moved where

W157-A proved the real chain (real `android.view.Choreographer` over the runtime
`display` service, the real `AndroidInfoModule` over `uimode`, the real
`EventDispatcherImpl`, the real single-threaded `CatalystInstanceImpl`) but kept
it in `BridgeProbe.main` inside `tests/rn_runtime.rs`. W158-A splits it:

* **`acos_jni::react_native`** (new, VM-free): structural APK detection
  (`libreactnative*.so`/`libhermes.so` + `assets/index.android.bundle`) and the
  `UIManager` op stream → [`RnTree`]. Unit-tested.
* **`acos_runtime::RnSession`** (new): VM boot + `ServiceHost`/Binder transport,
  APK DEX + driver DEX loading, VFS staging, JNI attach, bundle extraction, the
  mount, the tap pump, and the tree.
* **`BridgeProbe`** (driver DEX): two product entry points — `opsJson()` reads
  the live op bag, `tapAndPump(tag, quitMs)` posts the synthetic touch and
  re-enters `Looper.loop()`. `sUiManager` is the live recording module.

The W157-A regression (`tests/rn_runtime.rs`) is unchanged and still passes.

## 2. Why the tap is in-place, not a re-run

The product session must keep app state across commands, so `tap` cannot re-run
`main` (a second `ReactChoreographer.initialize` would throw). It re-enters the
main looper instead:

1. `RnSession::tap_tag` pushes `BridgeProbe.tapAndPump(tag, 1500)` with
   `Vm::push_static_call` (it calls `Looper.loop()`, which parks, so it cannot go
   through the synchronous `call_static` bridge);
2. it re-arms thread 0 and runs `run_scheduler(true)` (virtual time);
3. `tapAndPump` dispatches the real `TouchEvent` START/END through
   `UIManagerModule.getEventDispatcher()` and posts a bounded `Looper.quit()`;
   the main looper's `quitAllowed=false` makes `quit()` throw *before* it can
   destroy the message queue, so the thrown exception ends `Looper.loop()` and
   leaves the queue intact and re-enterable (exactly what W157-A relied on);
4. `opsJson()` is read again and the tree rebuilt.

`tap_test_id`/`tap_text` resolve the target from the current tree first, so
`--text "TAP ME"` finds the `RCTText` and walks up to the clickable Button.

## 3. Honest limits

* **No pixel frame.** The bridge registers no native view managers, so
  `UIImplementation` never lays the tree out or draws. `observe --frame` returns
  `UNSUPPORTED_RN_NO_RENDERER`; the tree reports **no bounds** (absent, not a
  zero rectangle).
* **MCP/CLI product routing is not wired in this wave.** `acos launch <rn.apk>`
  still falls through to the framework ladder; the daemon does not yet hold an
  `RnSessions` table. The library and its fixture-gated e2e are the delivered
  surface; the routing is the next wave.
* The driver is still the `BridgeProbe` harness (it constructs
  `CatalystInstanceImpl` directly rather than through `ReactInstanceManager`),
  and the `ProbeContext`/`ProbeSoFileLoader` are unchanged from W143-A/W153-A.
  "As far as the single-threaded bridge allows" is literal: UI, JS and native
  modules all share the main looper.
* `measure`/`dispatchViewManagerCommand`/`setJSResponder` are recorded answers
  (no native shadow tree), as in W157-A.

## 4. Files touched (worktree, uncommitted)

* `crates/acos-jni/src/react_native.rs` — **new**: detection, `RnNode`/`RnTree`,
  op replay, text/testID touchable resolution, `to_json`. 3 unit tests.
* `crates/acos-jni/src/lib.rs`, `crates/acos-jni/Cargo.toml` — module + re-export;
  `serde_json` is now a regular dependency (it was already a dev-dependency, so
  the lock is unchanged).
* `crates/acos-runtime/src/rn_session.rs` — **new**: `RnSession` /
  `RnSessionOptions` / `RnSessionError`.
* `crates/acos-runtime/src/lib.rs`, `crates/acos-runtime/Cargo.toml` — module +
  re-export; `zip` is now a regular dependency (bundle extraction).
* `crates/acos-jni/tests/java/.../BridgeProbe.java` — `sUiManager`, `opsJson()`,
  `dispatchTapForTag()`, `tapAndPump()` (the bounded-quit `Looper.loop()`).
* `crates/acos-jni/tests/data/rn-bridge-probe.dex` — rebuilt.
* `crates/acos-runtime/tests/rn_product.rs` — **new** fixture-gated e2e
  (confined child).
* `results/rn/w158a-product.json` — **new** evidence.
* `docs/AGENT_QUICKSTART.md` — RN section (§8).

No Flutter (`flutter_*`), counter-M2 or boot-cache file was touched; no
`acos-wm` source changed.

**Baseline note.** Committed HEAD `80cb4f5` does not build: `acos-vm`'s
`fwk_annotations`/`native_mem` modules are private but `javabridge.rs` re-exports
through them (the W153-A leftover the W157-A report documented). The worktree
applied the two `pub(crate)` changes to build; they are **excluded** from the
copy-back because the main tree already carries them as uncommitted changes.

## 5. Acceptance

| check | result |
|---|---|
| `cargo test -p acos-runtime --test rn_product` | **1 passed** (`launched`, `hello`, `tap_me`, `count: 1`, `UNSUPPORTED_RN_NO_RENDERER`; `results/rn/w158a-product.json`) |
| `cargo test -p acos-jni --lib react_native::` | **3 passed** |
| `cargo test -p acos-runtime --test rn_runtime` | **2 passed** (W157-A regression) |
| `cargo test -p acos-jni --test rn_bridge` | **2 passed** |
| `cargo test -p acos-jni --test rn_hermes` | **2 passed** |
| `cargo test -p acos-jni --test rn_native_stack` | **2 passed** |
| `cargo clippy --workspace --all-targets --no-deps -- -D warnings` | **clean** |
| `cargo check --workspace --all-targets` | **clean** |
| `tools/diff-dexprogs.sh --corelib real` | **27 pass / 0 fail / 0 unsupported** (27) |

Measured e2e (confined child): mount 19 ops → 12 nodes, `Hello ACOS` (tag 3),
`TAP ME` (tag 13), `hello-button` → tag 17; tap → raw text tag 19 moves from
`count: 0` to `count: 1`; 149 registered natives.
