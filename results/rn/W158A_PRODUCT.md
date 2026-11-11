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

## 6. W162-A follow-up — the product surface is wired

W158-A left the routing as the "next wave". W162-A lands it without touching any
`flutter_*` source: `acos launch <rn.apk>` auto-detects RN structurally (or
`--rn` forces it), implies native, and routes through the `acos serve` daemon
into the confined `acos --confined-child`. The child holds a new `RnSessions`
table (`crates/acos-mcp/src/rn_backend.rs`) built on the same
dedicated-worker-thread + bounded-channel pattern as `FlutterSessions`, so the
single-threaded Paper bridge is always driven from the thread that created it.
RN runtime `.so`s are extracted from the APK into the child's writable root; the
driver DEX travels as `--rn-dex` (re-exported as `ACOS_RN_DRIVER_DEX`).

### 6.1 What routes where

| surface | result |
|---|---|
| `acos launch <rn.apk>` | structural detect → native → daemon → confined child → `RnSession::launch`; ladder `apk_parsed → rn_bridge → ui_tree` |
| `acos observe --tree` / `--text` | `RnTree` JSON (nested `children`, tag/class/testID/text/clickable; **no bounds**) |
| `acos tap --text "TAP ME"` | resolves `RCTText` → clickable `RCTView` tag 17, real `TouchEvent` |
| `acos tap --testID hello-button` | same touchable via the JS `testID` prop |
| `acos observe --frame` / `--screenshot` | structured **`UNSUPPORTED_RN_NO_RENDERER`** (no native view managers) |
| `acos logs` / `acos status` | captured driver `ACOS_RN_*` lines / node+op+natives snapshot |
| `acos session reset` / `destroy` | drops the `RnSessions` entry (destroy also reaps the child) |
| `acos tap x y` on RN | structured unsupported: no layout bounds |

The MCP tools are the same table: `acos_launch`, `acos_observe`,
`acos_observe_tree`, `acos_tap { testID }`, `acos_logs`, `acos_status`,
`acos_reset`, `acos_destroy`. The protocol gained the `UNSUPPORTED_RN*` codes so
the envelope's `error.code` is the actual RN code, not `INTERNAL`.

### 6.2 A real defect the product e2e found

`RnTree`'s builder read `children` with `Value::as_i64()`, but the Java driver
serialises a `ReadableArray` as doubles (`[3.0]`). Every `setChildren` edge was
silently dropped, so the tree was a flat list of roots and `tap --text` could
not walk to a clickable ancestor (the W158-A unit test used integer JSON and
missed it). `react_native::json_tag` now accepts integral floats for `tag`,
`rootTag` and child arrays; `tree_accepts_float_encoded_tags` locks it. After
the fix the mount tree is nested (`44261 → 27 → 25 → {5,9,17,23}`) and both
selectors resolve tag 17.

### 6.3 Measured e2e

`crates/acos-mcp/tests/rn_session.rs` (`ACOS_REQUIRE_FIXTURES=1`) drives
`SessionBackend` → confined `acos-mcp` child → `RnSessions`:

| step | result |
|---|---|
| `acos_launch` | **pass**, `reached_stage=ui_tree`, wall **12.8 s**, 12 nodes / 19 ops, 45 JNI natives at launch, real boot classpath |
| `acos_observe_tree` | **pass**, `Hello ACOS` + `TAP ME`, `react_native=true`, `renderer=false` |
| `acos_tap { testID: "hello-button" }` | **pass**, tag **17**, tree → `count: 1` |
| `acos_tap { text: "TAP ME" }` | **pass**, tag **17**, tree → `count: 2` |
| `acos_observe` | **error `UNSUPPORTED_RN_NO_RENDERER`** (status `unsupported`) |
| `acos_status` / `acos_logs` | pass; `published_frames=0`, driver lines present |
| `acos_reset` / `acos_destroy` | pass (`state_reset=true`; destroy idempotent) |

Evidence: `results/rn/w162a-rn-session.json`. The library e2e
`crates/acos-runtime/tests/rn_product.rs` still passes and now also reports the
nested tree (`149` registered natives in that confined child).

### 6.4 Acceptance (W162-A worktree, clean HEAD `b467193`)

| check | result |
|---|---|
| `cargo test -p acos-mcp --test rn_session` | **1 passed** |
| `cargo test -p acos-runtime --test rn_product` | **2 passed** |
| `cargo test -p acos-jni --lib react_native::` | **4 passed** |
| `cargo test -p acos-mcp --lib rn_backend` | **3 passed** |
| `cargo test -p acos-protocol --lib` | **29 passed** |
| `cargo test -p acos-mcp --test native_session` | **1 passed** |
| `cargo test -p acos-mcp --test flutter_session` | **fails before/after W162-A** — the pre-existing `FlutterJNI.updateSemantics` `JNI_NATIVE_FAULT` reproduced on the untouched W159-A build against the current fixtures; not caused by this diff (no `flutter_*` file changed) |
| `cargo clippy --workspace --all-targets --no-deps -- -D warnings` | **clean** |
| `cargo check --workspace --all-targets` | **clean** |
| `tools/diff-dexprogs.sh --corelib real` | **27 pass / 0 fail / 0 unsupported of 27** |

No `flutter_*`, counter-M2 or boot-cache source changed. `session_backend.rs`,
`launch.rs`, `serve.rs` and `acos-mcp/src/main.rs` received only additive RN
branches/arguments; the Flutter branches are byte-for-byte unchanged.
