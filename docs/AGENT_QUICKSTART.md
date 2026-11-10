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

`tools/list` returns 22 descriptors. Availability is probed from the `acos`
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

> **Sessions persist in MCP, not in the CLI.** The MCP server holds
> `AgentSession`s keyed by `session_id`, so `acos_launch` → `acos_observe_tree`
> → `acos_tap` operate on the same live VM. The CLI has no daemon: every
> `acos observe --apk ...` invocation seeds a **fresh** session (a fresh launch),
> which is why multi-step driving belongs in MCP.
>
> **Why the CLI cannot share a session across processes (F-08).** A session is
> a live `AgentSession` — an in-process VM, its class loader, its `View`
> hierarchy and its frame store. None of that is serialisable, and the engine
> has no host daemon to attach to, so two independent `acos` processes cannot
> address the same session. Persisting only the *metadata* (id, apk, launch
> ladder) would let `acos session status` report a session that no running VM
> backs, which is worse than honesty. A future `acos serve` daemon (or the
> existing MCP server's in-process registry, already keyed by `session_id`)
> is the structural fix; until then the CLI relaunches per invocation and says
> so. `docs/MCP.md` §4 documents the same boundary for MCP clients.

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
   `acos_key { code }`; `acos_swipe { from, to }`.
7. **Read logs.** `acos_logs { session_id }` for the app's own `Log.i` lines.
8. **Assert.** For scenario-driven runs use `acos_verify` with a YAML scenario
   and read `verdict.json`; evidence (frames, tree, events) must exist for a
   pass to count.

### CLI equivalent

```bash
acos inspect fixtures/build/apps/todo-dogfood.apk --json
acos launch  fixtures/build/apps/todo-dogfood.apk --json
# each of the following relaunches (the CLI is stateless):
acos observe --apk fixtures/build/apps/todo-dogfood.apk --tree --json
acos tap     --apk fixtures/build/apps/todo-dogfood.apk --text "+1" --json
acos logs    --apk fixtures/build/apps/todo-dogfood.apk --json
acos session status --apk fixtures/build/apps/todo-dogfood.apk --json
```

For anything with more than one interaction, drive it over MCP so the session
survives between calls.

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
| App crashed/hung | `APP_CRASHED`, `APP_NOT_RESPONDING`, `JAVA_EXCEPTION` | Read the crash/ANR event and last actions. |
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

## 6. Known state (as of this wave)

`tools/build-dogfood-apps.sh` builds the framework-only fixtures, and
`acos launch` currently stops at:

```
[blocked] activity_on_create
  UNSUPPORTED_JAVA_API
  api: Landroid/graphics/drawable/VectorDrawable;->nCreateGroup()J
  at:  Landroid/graphics/drawable/VectorDrawable;.nCreateGroup
```

That is the `VectorDrawable` native vector-tree surface (`gfx_vector`), tracked
separately. Until it lands, themed activities stop at `activity_on_create`; the
fixtures are otherwise indistinguishable at the launch level. `acos launch`
now reports the exact missing API in `details.api`/`details.at`, so an agent can
link the blocker without reading the `java-tail` by hand.
