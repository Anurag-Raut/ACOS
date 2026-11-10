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

   **Current ceiling (2026-11-11, HEAD `1041c21`).** A `TextView`/`Button`
   activity reaches `on_resume` for real (`counter.apk`); a themed activity that
   inflates a `ListView` or `EditText` still stops at `activity_on_create` with
   `UNSUPPORTED_JAVA_API` whose `root_cause` is
   `android.content.IContentProvider code 0 (LIST_config)`. `first_frame` is
   `UNSUPPORTED_BACKEND` (`ui.frame`, `not_ready`) even when `on_resume` ran, so
   `acos observe --tree` works but `acos observe` (frame) does not. See §6.
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
   the frame that would route the event has not been published.
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
* `ELEMENT_NOT_FOUND` + `details.kind = NoUiTree` / `NoWindowAtPoint` /
  `NoFocusedWindow` → the session has no window to receive input. Launch and
  observe before tapping/typing.

---

## 6. Known state (as of 2026-11-11, HEAD `2c682c7`)

`tools/build-dogfood-apps.sh` builds the framework-only fixtures. On this HEAD:

* **`counter.apk` reaches `first_frame` for real.** `acos launch
  fixtures/build/apps/counter.apk --json` returns `status: pass` /
  `reached_stage: first_frame`; a frame from the real `ThreadedRenderer.draw`
  lands in the `FrameStore` (W133-A), `acos observe --apk … --tree` returns the
  live `View` hierarchy (`count=0`, `Increment`, `Reset`), and
  `acos observe --apk … --screenshot out.png` writes the frame.
* **⚠ Boot-image restore is broken for launch.** The first (cold) launch
  reaches `first_frame` (~0.9 s), but with the on-disk cache already populated
  the warm launch fails at `activity_on_create`
  (`java.lang.ExceptionInInitializerError` → ICU `DatPackageReader` index error)
  **9/10 times**. Force the cold path with `ACOS_NO_BOOT_CACHE=1` (5/5 there).
  This is an open runtime bug, not a tooling/tree problem; do not quote
  "counter launches" without it.
* **Tap is acked but does not land.** `acos tap --text Increment --json` returns
  `accepted: true`, `state_changed: false`; the count stays `0`. Input delivery
  is pending the W144-A leash fix.
* **`hello-views.apk` reaches `on_resume`** (W138-A); its frame/correctness is
  not yet scored. `counter` is the app that reaches `first_frame`.
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

### 7.3 Current boundary (2026-11-11, HEAD `a202c87`, W141-A)

`acos launch <flutter.apk> --native`, `acos observe`, `acos tap --text` and the
MCP session tools do **not** drive the Flutter engine yet: the product wiring is
still missing. W141-A moved the W130-A driver into a library type,
`acos_jni::FlutterSession` (`crates/acos-jni/src/flutter_session.rs`):
`launch(apk, opts)` → `pump()` / `pump_until_frames(n)` → `frame()` (RGBA/PNG) →
`semantics()` → `tap(x,y)` / `tap_text(label)` → `shutdown()`, with bounded
pumping, geometry/density from `opts` (default 1080×2400 @ 2.5) and structured
`UNSUPPORTED_*`/`ENGINE_*` errors. Semantics capture is wired through the
`FlutterJNI.AccessibilityDelegate` (`updateSemantics` is private, so the
delegate is the supported hook) into the W137-A decoder. It is **not** yet held
by the confined child, so `acos launch --native` on a Flutter APK still runs the
framework ladder. The `tests/flutter_hello.rs` harness (Vm + raw loader +
software GLES + `ALooper` pump + `FlutterJNI` host) remains the working evidence
for the engine path (counter `0 → 1`, masked SSIM 0.9987 / 0.9968). Also note the
raw `dart:ui` probe declares no semantics, so the engine's semantics stream is
empty until the snapshot calls `PlatformDispatcher.updateSemantics`. Do not
report a Flutter `pass` until the confined-child wiring and the fixture-gated
product e2e land (see FLUTTER_PARITY.md §W141-A).
