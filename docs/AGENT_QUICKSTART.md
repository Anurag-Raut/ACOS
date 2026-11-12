# ACOS agent quickstart

ACOS runs Android apps and reports what actually happened: a launch ladder, a
live UI tree, frames, logs, and a machine verdict. **Agents are the only users**,
so everything here is phrased as a decision the agent can act on. This is a
practical companion to the full contract in
[`docs/AGENT_API.md`](AGENT_API.md) and the MCP server details in
[`docs/MCP.md`](MCP.md).

The three honesty rules that everything else depends on:

* `status: "pass"` means the engine **ran** the thing and it held.
* `status: "fail"` means the app ran and a check did **not** hold. Fix the app
  (or the expectation).
* `status: "unsupported"` means the engine **cannot** do it. Do not rewrite the
  app to hide it, and never call it a pass.
* Branch on `error.code`, never on the message text.

---

## 0. Preflight (do this first)

```bash
acos doctor            # host toolchain: SDK, build-tools, JDK, helper, memory
acos doctor --json     # same report, machine-readable
```

`acos doctor` exits non-zero and prints a `fix:` line for every failing check
(e.g. a missing `d8`, `android.jar`, JDK, or a host over the 90% memory
ceiling). Fix those before building or launching; otherwise a later failure is a
host problem masquerading as an app problem.

For the tool surface, the MCP equivalents are:

* `acos_capabilities` — what the backend can do **right now** (call with
  `{"refresh": true}` after a rebuild);
* `acos_help` — the workflow + error-code playbook + supported/unsupported list.

---

## 1. MCP tools (what exists today)

`tools/list` returns 24 descriptors. Availability is probed from the `acos`
binary; `available=false` tools return a structured `unsupported` envelope, not
a false pass.

| Tool | Backed by | Available | Use it for |
|---|---|---|---|
| `acos_inspect` | `acos inspect` | yes | Static facts about an APK; **never** proof of runtime behaviour |
| `acos_run_dex` | `acos run-dex` | yes | Console-tier DEX execution (unit-level checks) |
| `acos_launch` | in-process session | yes | **Start here for an app.** Launch ladder + `session_id` |
| `acos_run` | in-process session | yes | Alias of `acos_launch` over the protocol command |
| `acos_observe_tree` | in-process session | yes | Live `View` hierarchy (class/text/id/bounds/children) |
| `acos_observe` | in-process session | yes | Latest completed frame metadata |
| `acos_tap` | in-process session | yes | Tap by `text` or `x`/`y` |
| `acos_key` | in-process session | yes | Key input (BACK/HOME/ENTER/...) |
| `acos_text` | in-process session | yes | Type text into the focused editor (`InputConnection.commitText`) |
| `acos_swipe` | in-process session | yes | Swipe `from`→`to` |
| `acos_action` | in-process session | yes | Protocol-level unified action (`tap`/`key`/`swipe`/`back`) |
| `acos_logs` | in-process session | yes | Guest log buffer |
| `acos_status` | in-process session | yes | launched / `stage_reached` / frame count |
| `acos_reset` | in-process session | yes | Reset session task state |
| `acos_destroy` | in-process session | yes | Tear down a session (idempotent) |
| `acos_verify` | `acos verify` | yes | YAML scenario → evidence-backed verdict |
| `acos_capabilities` | server tool | yes | Supported/unsupported set + backend version |
| `acos_help` | server tool | yes | Self-description / playbook |
| `acos_doctor` | server tool | yes | Host preflight JSON (SDK/JDK/keystore/memory) with `fix:` per failure |
| `acos_capture` | `acos capture` | no | (not implemented) |
| `acos_assert` | `acos assert` | no | (not implemented) |
| `acos_events` | `acos events` | no | (not implemented) |
| `acos_profile` | `acos profile` | no | (not implemented) |
| `acos_capability_report` | `acos capability-report` | no | (not implemented) |

Every tool's `arguments` are the ACOS request object; only
`base.request_id` is required, and `base.session_id` selects the session.
The result is the ACOS envelope as text (plus `structuredContent` on protocol
`2025-06-18`). `isError` is `true` for every non-`pass` status.

> **Sessions persist in a local daemon.** `acos serve` holds live
> `AgentSession`s (one worker thread or confined native child each) behind a
> private Unix socket: runtime dir `0700`, `SO_PEERCRED` checked, idle timeout,
> and session / RAM caps. It serves the same session-tool protocol and ACOS
> envelopes as the MCP `SessionBackend`, so the CLI and the MCP server are two
> front-ends over one session registry (`ACOS_SESSION_DAEMON=1` points the MCP
> server at a running daemon).
>
> **F-08 is fixed structurally.** `acos launch <apk> --keep` starts the daemon
> if it is not already running and leaves the session live. Separate processes
> then address that *same* session:
>
> ```bash
> acos launch fixtures/build/apps/native-hello.apk --native --keep --json
> acos observe --json            # the daemon's session, not a fresh one
> acos tap --text "+1" --json
> acos logs --json
> acos session list --json       # every session the daemon holds
> acos session status --json
> acos session reset --json
> acos session destroy --json    # daemon idles out afterwards
> ```
>
> `acos session {list,status,observe,tap,key,text,logs,reset,destroy}` always
> talks to the daemon (auto-starting it on demand). The top-level
> `observe/tap/key/swipe/logs` use a *running* daemon when one exists and fall
> back to the old one-shot in-process session otherwise; `--apk` keeps the
> one-shot path. Set `ACOS_NO_DAEMON=1` to disable the daemon entirely, and
> `ACOS_SESSION_IDLE_SECS` / `ACOS_RUNTIME_DIR` / `ACOS_SESSION_SOCKET` to
> tune it. Native sessions run in the same confined child the MCP server uses
> (`acos --confined-child`).
>
> **Startup prewarm (P8).** When `acos serve` is the ART engine it reads the
> launch working set — boot image, ART runtime libraries, boot-classpath jars,
> the guest dex/shims and the default fonts — into the host page cache on a
> **background thread**, after the socket is already listening. P7 measured
> that a first launch after those pages were evicted costs +198 ms
> (660.9 vs 462.7 ms p50); the prewarm removes that. It is bounded (default
> 64 MiB, `ACOS_PREWARM_MAX_MB`), never locks memory (`mlock`), never writes,
> and is off the launch/observation/verification path, so it cannot change a
> verdict. One summary line is logged per daemon start:
> `acos serve: prewarm: warmed 46 files, 46.1 MiB in 34 ms (...)`. Set
> `ACOS_NO_PREWARM=1` to disable it and `ACOS_PREWARM_VERBOSE=1` to log every
> file. It does not apply to `--engine rust-vm`.

---

## 2. The develop → run → observe → tap → verify loop

```
doctor → inspect → launch → status → observe_tree → tap/key/swipe → logs → verify
```

1. **Preflight.** `acos doctor` (CLI) or `acos_capabilities` (MCP).
2. **Know the APK.** `acos_inspect` / `acos inspect <apk>`. Confirms package and
   launcher activity; static only.
3. **Run it.** `acos_launch { apk }` (MCP) or `acos launch <apk> --json`.
   Read the ladder:
   `apk_parsed → process_booted → bind_application → application_on_create →
   client_transaction → activity_on_create → on_resume → first_frame`.
   The first non-`ok` rung carries the blocker; `reached_stage` is the honest
   answer to "how far did it get?".

   **Current ceiling (2026-11-11, HEAD `fd1a887`, verify with
   `tools/status-check.sh`).** A `TextView`/`Button` activity reaches
   `first_frame` for real: `counter.apk` and the W166-A fixtures
   (`dialogs-menus`, `list-adapter`, `fragments-nav`) return
   `reached_stage: first_frame`. A themed activity that inflates a `ListView`
   or `EditText` in the older fixtures (`todo-dogfood`, `list-scroll`,
   `text-input`) still stops at `activity_on_create` with
   `UNSUPPORTED_JAVA_API` whose `root_cause` is
   `android.content.IContentProvider code 0 (LIST_config)`. When nothing has
   been launched yet, `first_frame` is `UNSUPPORTED_BACKEND` (`ui.frame`,
   `not_ready`), so `acos observe --tree` answers `not_ready` too. Read
   `reached_stage` rather than assuming. See §6.
4. **Check the state before observing.** `acos_status { session_id }`. Tells
   "never started" from "started but no window yet". `acos_logs` now also
   returns `launched` and `stage_reached`: an empty log buffer is only a clean
   pass if the app actually reached `onCreate`.
5. **Observe.** `acos_observe_tree { session_id }` returns nodes with
   `resource_id`, `text`, `content_desc`, `bounds`, and
   `clickable/enabled/focused/scrollable`. Use those bounds/ids for the next
   step. Before the app adds a window this is `unsupported` with
   `details.reason = "not_ready"` and `details.stage_reached` — never an empty
   tree.
6. **Act.** `acos_tap { text }` (preferred) or `acos_tap { x, y }`;
   `acos_key { code }`; `acos_text { text }` to type into the focused editor;
   `acos_swipe { from, to }`.

   `acos_text` / `acos session text` edits the field the way a soft keyboard
   does: it finds the focused view, asks the framework for its own
   `InputConnection` (`View.onCreateInputConnection`, the real
   `EditableInputConnection` for a `TextView`/`EditText`) and calls
   `commitText`, so `TextWatcher`s, the selection and the cursor update as on
   device. The result carries `text_path: "input-connection"`. If the focused
   view has no editable connection it falls back to a `KeyEvent` sequence
   (`text_path: "key-events"`, mapping `a-z 0-9 space newline`); characters
   that fallback cannot represent return a structured `UNSUPPORTED_BACKEND`,
   never a dropped character. Composing text and IME actions are not modelled.
   The field must already have input focus — observe the tree and tap it first;
   an unfocused field returns `ELEMENT_NOT_FOUND` with
   `details.kind = NoFocusedEditor`.

   **Read `text_path` and `state_changed`.** `CommitText` needs a live editor;
   if the focused view is *not* an editor (e.g. a `Button`), the command still
   succeeds with `text_path: "key-events"` and sends the key sequence to
   whatever is focused. `status: "pass"` there means "input was delivered",
   **not** "the text was inserted" — check `text_path` before trusting it.
   Likewise a `tap` before `first_frame` returns `status: "pass"`,
   `accepted: true`, but `state_changed: false` and the click does not land;
   the frame that would route the event has not been published. When
   `accepted: true` and `state_changed: false`, the result carries a non-empty
   `note` saying the event may not have landed; a `key-events` fallback gets
   its own note. Treat `note` as the in-band reason, not as an error.
7. **Read logs.** `acos_logs { session_id }` for the app's own `Log.i` lines.
8. **Assert.** For scenario-driven runs use `acos_verify` with a YAML scenario
   and read `verdict.json`; evidence (frames, tree, events) must exist for a
   pass to count.

### CLI equivalent

```bash
acos inspect fixtures/build/apps/todo-dogfood.apk --json
# --keep starts `acos serve` and leaves the session live:
acos launch  fixtures/build/apps/todo-dogfood.apk --keep --json
# each of the following talks to that same daemon session:
acos observe --json
acos tap     --text "+1" --json
acos session text --text "hello" --json   # focused editor
acos logs    --json
acos session status --json
```

Without a running daemon the CLI still supports the one-shot path
(`acos observe --apk <apk>` / `acos launch <apk>` without `--keep`), which
relaunches; with `--keep` the session survives between calls. MCP clients use
the in-process session registry exactly as before.

---

## 3. Building a fixture / new app

The Gradle-free toolchain builds `fixtures/apps/<name>/` into
`fixtures/build/apps/<name>.apk`:

```bash
tools/build-dogfood-apps.sh --list          # available app names
tools/build-dogfood-apps.sh todo-dogfood    # build one
tools/build-dogfood-apps.sh                 # build all
```

Each app needs `AndroidManifest.xml`, `res/`, and `src/`. On a missing tool or
directory the script names the exact path and suggests `acos doctor`. To drive
a whole set and collect evidence:

```bash
tools/e2e-dogfood.sh todo-dogfood
# evidence: results/dogfood/launch/<app>.json + .summary.txt + SUMMARY.md
```

`tools/build-apps.sh` builds every fixture app (`fixtures/apps/*`) into
`fixtures/build/apps/`, which is what `tools/verify-all.sh` uses. Three apps
added at W166-A reach `first_frame` and are the best starting point for a new
UI fixture: `dialogs-menus` (dialogs + option menus), `list-adapter`
(`ListView`/adapter) and `fragments-nav` (fragments). Check the ladder after a
launch instead of assuming: `tools/build-apps.sh && acos launch
fixtures/build/apps/list-adapter.apk --json` should report
`reached_stage: first_frame`.

---

## 4. Parity check (device ↔ ACOS UI tree)

`acos parity` compares a device's UI capture against ACOS's tree.

```bash
# Offline against the recorded captures in fixtures/reference-device/
acos parity --recorded hello-views --acos-tree <acos-tree.json> --json

# Live against a real device (only installs dev.acos.fixtures.* packages)
acos parity --device <adb-serial> fixtures/build/apps/hello-views.apk --output out/
```

Differences are classified (`missing_in_acos`, `extra_in_acos`, `text_differs`,
`class_differs`, `bounds_differ`, `state_differs`) and normalized by
resource-id → `class|text` → class, with optional `--bounds-scale` and
`--bounds-tolerance`. A missing `adb`/device yields a non-zero report with
`NEEDS_REAL_DEVICE`; it never silently "passes".

---

## 5. Error codes: what to do next

Branch on `error.code`. The envelope always carries `code`, `message`, `hint`,
`retryable`, and optional `details`.

| Category | Codes | Next action |
|---|---|---|
| Engine cannot do it (`unsupported`) | `UNSUPPORTED_JAVA_API`, `UNSUPPORTED_ANDROID_API`, `UNSUPPORTED_BYTECODE`, `UNSUPPORTED_NATIVE_SYMBOL`, `UNSUPPORTED_RELOCATION`, `UNSUPPORTED_BACKEND`, `NEEDS_REAL_DEVICE`, `REQUIRES_GMS`, `WRONG_ABI`, `NO_DISPLAY`, `NO_TEXT`, `RENDER_UNSUPPORTED_OP` | Read `details.api`/`details.at` (the exact missing native), `details.symbol`/`details.library`, or `details.reason`; avoid the API, pick another backend, or report unsupported. **Never a pass.** |
| App is wrong (`fail`) | `ASSERTION_FAILED`, `ELEMENT_NOT_FOUND` | Read the assertion diff / re-observe the tree, then fix the app or the expectation. |
| App crashed/hung | `APP_CRASHED`, `APP_NOT_RESPONDING`, `JAVA_EXCEPTION` | Read `details.root_cause` (the deepest `caused by:` line, now surfaced), the crash/ANR event and last actions. A framework `RuntimeException` (e.g. an `InflateException`) hides the real engine blocker several causes deep. |
| Bad artifact | `APK_*`, `AXML_MALFORMED`, `ARSC_MALFORMED`, `DEX_*`, `ELF_MALFORMED`, `NATIVE_*` | Rebuild/re-sign/repackage, then `acos_inspect` again. Not an app verdict. |
| Session/resource | `SESSION_NOT_FOUND`, `SESSION_BUSY`, `STALE_REF`, `TIMEOUT`, `RESOURCE_LIMIT`, `RAM_GUARD`, `RAM_ABORT` | Create the session first, serialise mutations, re-observe after mutations, reduce concurrency/raise a budget. |
| Evidence/fidelity | `NO_FRESH_FRAME`, `UNVERIFIED_FIDELITY`, `ARTIFACTS_INVALID`, `UNSAFE_PATH` | Wait for a new frame, use `after_seq`/`after_cause`, rerun at a qualifying fidelity, keep artifacts inside the artifacts dir. |
| Invalid input | `INVALID_ARGUMENT`, `INVALID_SCENARIO` | Fix the request/scenario. |

Two session-specific hints worth memorizing:

* `UNSUPPORTED_BACKEND` + `details.reason = "not_ready"` → the app has no
  window/frame yet. Run `acos launch` and read the ladder; a tree/frame only
  exists after `activity_on_create` / the render handoff completes.
  **This is the code `tap --text` returns when there is no tree yet**: it
  resolves a node by label, so with no `View` hierarchy it answers
  `UNSUPPORTED_BACKEND` / `reason: not_ready` / `capability: ui.tree`
  (`crates/acos-cli/tests/session.rs::tap_text_without_a_tree_is_a_structured_error`).
  There is no `ELEMENT_NOT_FOUND`/`NoUiTree` for that case any more.
* `ELEMENT_NOT_FOUND` + `details.kind = NoWindowAtPoint` / `NoFocusedWindow` →
  the session has no window at that point / no focused window, so a coordinate
  `tap`, `key` or `swipe` cannot be delivered. Launch and observe before acting.
  (`NoUiTree` is no longer produced for the no-tree case above.)

If you set `ACOS_SESSION_ROOT`, `ACOS_CACHE_DIR` or `ACOS_BOOT_DIR`, they must be
real directories you own that are **not** group/world-writable; otherwise the
command fails with `UNSAFE_PATH` and `details.root_error`
(`ROOT_WRITABLE_BY_OTHERS`, `ROOT_NOT_OWNED`, `ROOT_NOT_A_DIRECTORY`). See
`docs/SECURITY.md`.

---

## 6. Known state (as of 2026-11-11, HEAD `fd1a887`)

`tools/build-dogfood-apps.sh` builds the framework-only fixtures. On this HEAD:

* **`counter.apk` reaches `first_frame` for real.** `acos launch
  fixtures/build/apps/counter.apk --json` returns `status: pass` /
  `reached_stage: first_frame`; a frame from the real `ThreadedRenderer.draw`
  lands in the `FrameStore` (W133-A), `acos observe --apk … --tree` returns the
  live `View` hierarchy (`count=0`, `Increment`, `Reset`), and
  `acos observe --apk … --screenshot out.png` writes the frame. On a kept
  daemon session the same `acos observe --screenshot out.png` (no `--apk`)
  also writes the PNG and reports `result.screenshot` with the
  `ui.screenshot` capability; `result.screenshot_error` is set when no frame
  exists yet.
* **Boot-image restore works (fixed W154-A/W160-A).** A cold launch reaches
  `first_frame` (~0.9 s) and a *warm* launch restores the cached snapshot and
  also reaches `first_frame` — 5/5 at 0.806 s p50 with a 25.58 MB cache entry
  (`BENCHMARKS.md` §9.1 M3). Do **not** force `ACOS_NO_BOOT_CACHE=1` to "work
  around" the old `DatPackageReader` crash; that bug is closed. The regression
  guard is
  `crates/acos-cli/tests/session.rs::warm_boot_cache_reaches_first_frame_on_every_launch`
  (1 cold + 5 warm processes). If a warm launch fails there, that is a *new*
  bug — report it with the launch ladder, don't disable the cache silently.
* **Tap lands on `counter.apk` (fixed W161-A).** `acos tap --text Increment
  --json` on a kept session returns `accepted: true`, `state_changed: true`, the
  app's own `onClick` runs (`AcosCounter: increment value=1`) and `acos observe
  --tree` then shows `count=1`. Evidence:
  `crates/acos-runtime/tests/counter_interaction.rs` (both tests un-ignored),
  `results/m2/M2_LOG.md` §W161-A. The older "acked but does not land" text and
  `BENCHMARKS.md` §9.5 item 3 describe the pre-W161-A binary.
* **The pressed-state ripple is a declared flat-colour fallback (W167-A).** The
  AGSL/`RippleShader` natives now draw the *declared flat colour* instead of the
  real shader animation, and a post-tap repaint invalidation makes the state
  visible. Do not read the interaction frame as pixel-identical to a device: the
  ripple shading is a named divergence, not a faked pass. `CanvasProperty` and
  `RenderNodeAnimator` coverage is partial — an unregistered op still raises a
  structured `UNSUPPORTED_JAVA_API`, never a silent default.
* **`hello-views.apk` reaches `on_resume`** (W138-A) and `first_frame`
  (W147-A); its frame/correctness is not ART-scored yet.
* `acos observe --tree` on a kept daemon session returns the live tree in
  ~40 ms; with no session it still answers `UNSUPPORTED_BACKEND not_ready`.
* Flutter: the separate engine path reaches a first frame and a tap on
  `FlutterSession` (masked SSIM 0.9987); `acos launch --flutter` is wired but
  the confined-child hold is partial (W142-A). See §7.
* A themed activity that inflates `ListView` or `EditText` stops at
  `activity_on_create`. The message is a framework `InflateException`; the
  `details.root_cause` names the real blocker:

  ```
  [blocked] activity_on_create
    UNSUPPORTED_JAVA_API
    root_cause: java.lang.UnsupportedOperationException: unsupported Android API: \
                android.content.IContentProvider code 0 (LIST_config)
  ```

  i.e. the settings provider does not answer the `LIST_config` call. This gates
  every list/text input app (list-scroll, text-input, todo-dogfood, and a
  `ListView`/`EditText` notes app). It is a runtime/provider gap, not an app or
  tooling bug.

The previous `VectorDrawable.nCreateGroup` blocker is fixed; if you still see
it, your binary predates the `gfx_vector` work and should be rebuilt.
`acos launch` reports the exact missing API in `details.api`/`details.at` for a
shallow one-line blocker, and now also `details.root_cause` for a wrapped Java
exception, so an agent can link the blocker without reading the `java-tail` by
hand.

---

## 7. Flutter APKs (`flutter` field, semantics, the current boundary)

Flutter apps ship no framework `View` tree; the engine draws everything itself.
ACOS recognises them structurally and (in W137-A) exposes the pieces needed for
the same `launch → observe → tap` loop, but the driver is not wired end to end
yet. Read this before promising a Flutter result.

### 7.1 Recognise one

```bash
acos inspect fixtures/build/apps/flutter-hello.apk --json
# result.flutter = {
#   "detected": true,
#   "engine_abis": ["x86_64"],
#   "snapshot_abis": ["x86_64"],
#   "has_flutter_assets": true
# }
```

Detection is structural (`lib/<abi>/libflutter.so`, `lib/<abi>/libapp.so`,
`assets/flutter_assets/`), never a manifest guess. A framework APK reports
`"detected": false`.

### 7.2 What exists in the runtime

The `acos-jni` crate now owns the two agent-facing primitives ([`results/flutter/FLUTTER_PARITY.md`](../results/flutter/FLUTTER_PARITY.md) §W137-A has the
full wire format):

* `acos_jni::flutter::detect*` — classification;
* `acos_jni::flutter::decode_semantics_update(bytes, strings, attr_buffers)` —
  a byte-for-byte port of the engine's `AccessibilityBridge` decoder, yielding
  `SemanticsNode { id, rect, label, flags, actions, children, … }` with
  `text()`, `bounds()`, `clickable()`, `enabled()`. This is the Flutter UI tree;
  there is no `View` hierarchy to read;
* `acos_jni::flutter::tap_packet(x, y, ts)` — the 576-byte `down`+`up`
  `PointerData` packet; the caller scales by `devicePixelRatio` and passes
  `packet.len()` as `position`.

### 7.3 Flutter product path (2026-11-11, W149-A)

`acos launch <flutter.apk>` now drives the engine end to end. A Flutter APK is
detected structurally (`--flutter` or auto-detect), implied native, and routed
through the `acos serve` daemon into a confined `acos --confined-child`, where a
`FlutterSessions` table owns one `acos_jni::FlutterSession` per session on a
**dedicated long-lived worker thread** (64 MiB stack). Every MCP/CLI tool
(`acos_launch/observe/observe_tree/tap/logs/status/reset/destroy`) marshals a
command to that worker and waits with a bounded timeout, so the engine's
`ALooper`/platform thread identity is stable across calls.

```bash
# Launch (daemon-held; separate observe/tap invocations address the same session)
acos launch fixtures/build/apps/flutter-hello.apk --flutter
acos observe --screenshot /tmp/flutter.png
acos tap --text Increment      # or: acos tap 375 600
acos logs
acos session list
acos destroy

# Frame parity (status bar 0..108 and navigation bar 2304..2400 masked)
acos parity --flutter-frame /tmp/flutter.png
```

Measured (fixture-gated e2e `crates/acos-mcp/tests/flutter_session.rs`,
`ACOS_REQUIRE_FIXTURES=1`): launch reaches `first_frame` in **~2.8 s**, the
confined child's RSS is ~500 MiB, `acos observe` returns a 1080×2400 frame+PNG,
and `acos tap` produces `ACOS_DART_COUNTER counter=1` plus a new frame sequence.

**Current boundary:** the engine's `AccessibilityBridge` is not established by
the `FlutterJNI`-only driver, so `acos observe --tree` is empty even though the
raw `dart:ui` snapshot now publishes an `Increment` node; `acos tap --text` is
wired but the e2e falls back to the physical centre until that lands. The
software GLES sink in the child also republishes the first RGBA buffer, so
tap-frame pixels are identical (the counter is proven by the Dart log). See
`results/flutter/FLUTTER_PARITY.md` §W149-A.

## 8. React Native APKs (W158-A/W162-A)

React Native's legacy (Paper) renderer builds the Android `View` tree in
JavaScript and ships every mutation to the host as a `UIManagerModule`
operation. ACOS captures that operation stream instead of reading a native view
hierarchy, because no native view managers are registered yet.

### 8.1 Recognise one

Detection is structural (`lib/<abi>/libreactnative*.so` or `lib/<abi>/libhermes.so`
**plus** `assets/index.android.bundle`), never a manifest guess:

`RnSession::detect` (and `acos_jni::react_native::detect`) returns the RN ABIs
and whether the bundle is present; a framework APK is never misclassified.

### 8.2 The library (`acos_runtime::RnSession`)

W158-A moved the W157-A harness host loop into a library type. The confined
child used by the product path owns one `RnSession`:

```text
RnSession::launch(apk, opts)     mount the bridge, capture the UIManager batch
session.tree()                   &RnTree (tag/class/testID/text; no bounds)
session.tap_tag(tag)             real TouchEvent path + Looper pump
session.tap_test_id("hello-button")
session.tap_text("TAP ME")       resolves the RCTText to its touchable parent
session.frame()                  UNSUPPORTED_RN_NO_RENDERER (no native views)
```

The host loop is the real one: `ReactChoreographer` over the real
`android.view.Choreographer` (the runtime `display` service), the real
`AndroidInfoModule` (`uimode` service), the real `EventDispatcherImpl`, and the
single-threaded main-looper bridge W143-A proved. A later `tap` re-enters
`Looper.loop()` on the live session (the main looper refuses to quit, so its
queue survives) — it does **not** re-run the harness; the counter state is
preserved.

### 8.3 The agent tree

`RnTree`/`RnNode` replay `createView`/`updateView`/`setChildren` into
tag/class/testID/text nodes. `RCTText` text is the concatenated child
`RCTRawText`; `clickable` comes from the JS responder props. **Bounds are
absent**, not zero: no view managers means `UIImplementation` never lays the
tree out. `tap --text`/`--testID` resolve to the nearest clickable ancestor, so
`TAP ME` targets the Button's `RCTView` (tag 17), the tag W157-A proved drives
`onPress` → `setCount`.

### 8.4 Structured errors

* `UNSUPPORTED_RN` — not a React Native APK;
* `UNSUPPORTED_RN_SETUP` / `UNSUPPORTED_RN_DRIVER` — boot/bridge stage failed;
* `UNSUPPORTED_RN_NO_RENDERER` — `observe --frame` (missing native view
  managers ⇒ no pixel frame yet);
* `UNSUPPORTED_RN_NO_TARGET` — `tap --text`/`--testID` found nothing;
* `UNSUPPORTED_ABI` — no RN/Hermes library for the host ABI (an APK-level
  ABI miss; `WRONG_ABI` is the loader's per-ELF `e_machine` mismatch).

### 8.5 Product path (W162-A)

`acos launch <rn.apk>` auto-detects the APK structurally (or `--rn` forces it),
implies native, and routes through the `acos serve` daemon into the same
confined `acos --confined-child` used by Flutter. The child holds an
`RnSessions` table: one `RnSession` per session on a dedicated long-lived
worker thread (64 MiB stack), so the single-threaded Paper bridge is always
driven from the thread that created it. RN runtime libraries are extracted from
the APK into the child's writable session root; the Java driver DEX is passed
as `--rn-dex` (re-exported as `ACOS_RN_DRIVER_DEX` because the sandbox clears
the environment).

```bash
acos launch fixtures/build/apps/rn-hello.apk
acos observe --tree                 # UI tree JSON (Hello ACOS, testID, text)
acos tap --testID hello-button      # real TouchEvent -> onPress -> setCount
acos observe --tree                 # count: 1
acos tap --text "TAP ME"            # resolves RCTText -> touchable RCTView
acos observe --frame                # UNSUPPORTED_RN_NO_RENDERER (structured)
acos logs
acos session status
acos session reset
acos session destroy
```

Every command goes through the same MCP tools (`acos_launch`,
`acos_observe`, `acos_observe_tree`, `acos_tap { testID }`, `acos_logs`,
`acos_status`, `acos_reset`, `acos_destroy`).

### 8.6 Evidence

* `crates/acos-runtime/tests/rn_product.rs` — library e2e (confined child):
  mount → `Hello ACOS`/`TAP ME` → `tap --testID hello-button` → `count: 1`;
  `frame()` returns `UNSUPPORTED_RN_NO_RENDERER`. Evidence:
  `results/rn/w158a-product.json`.
* `crates/acos-mcp/tests/rn_session.rs` — **product** e2e through
  `SessionBackend` → confined `acos-mcp` child → `RnSessions`: launch →
  `observe_tree` contains `Hello ACOS` → `tap --testID hello-button` →
  `count: 1` → `tap --text "TAP ME"` → `count: 2` → `observe --frame` returns
  `UNSUPPORTED_RN_NO_RENDERER` → status/logs/reset/destroy. Evidence:
  `results/rn/w162a-rn-session.json`.

The only honest limitation is unchanged: there is **no pixel frame** (the
bridge registers no native view managers) and no layout bounds, so point taps
are rejected and the tree is the product surface. Key/text input are structured
`UNSUPPORTED_RN_KEY`/`UNSUPPORTED_RN_TEXT`. See
`results/rn/W158A_PRODUCT.md` §6 (W162-A follow-up).
