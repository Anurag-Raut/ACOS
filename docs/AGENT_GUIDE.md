# ACOS agent guide

ACOS runs real Android apps on Google's real ART (API 34, x86_64) inside an
unprivileged `bwrap` sandbox and reports what actually happened: a launch ladder,
a live UI tree, frames, logs, and an evidence-backed verdict. Agents are the
only users. This file is the complete operating guide; the MCP tool
`acos_help` returns it (and `acos_help {"topic": ...}` returns one section).

Three rules decide everything:

* `status: "pass"` — the engine **ran** the thing and the check held. Only a
  pass with its evidence files present counts.
* `status: "fail"` — the app ran and a check did **not** hold. Fix the app or
  the expectation.
* `status: "unsupported"` / `"blocked"` — the engine or the host cannot do it
  right now. Never a pass, never a reason to rewrite the app to hide the gap.

**Branch on `error.code`, never on `error.message`.** Every envelope has `ok`,
`status`, `backend`, `fidelity`, `capabilities_used`, `metrics`; a non-pass
status always carries `error.code`, `error.hint`, `error.retryable` and optional
`error.details`.

<!-- topic:workflow -->
## The loop

```
doctor -> inspect -> launch -> observe_tree -> tap/text/key/swipe -> logs -> verify -> reset -> destroy
```

1. **Preflight the host.** `acos doctor --json` (CLI) or `acos_doctor` (MCP).
   It checks the Android SDK, `d8`, `android.jar`, the JDK, the workspace
   helper, the signing keystore, the boot-classpath cache and host memory, and
   every failing check carries a `fix:` line. Fix those first: without them a
   later failure is a host problem wearing an app's clothes.
2. **Know the APK before running it.** `acos_inspect` / `acos inspect <apk>`
   is static only (package, launcher activity, min/target SDK, DEX and native
   inventory). It never executes the app and is never proof of runtime
   behaviour.
3. **Run it.** `acos_launch {"apk": "..."}` (MCP, session persists) or
   `acos launch <apk> --engine art --json`. **The MCP session tools always run
   real ART** (the server starts its own private `acos serve --engine art`
   daemon); the `acos` CLI's own default route is the frozen Rust VM, so pass
   `--engine art` there. Read the ladder:
   `apk_parsed -> process_booted -> bind_application -> application_on_create ->
   client_transaction -> activity_on_create -> on_resume -> first_frame`.
   `reached_stage` is the honest answer to "how far did it get"; the first
   rung that is not `ok` carries the blocker.
4. **Observe before acting.** `acos_observe_tree` returns nodes with
   `resource_id`, `text`, `content_desc`, `class`, `bounds`,
   `clickable/enabled/focused/scrollable`. Use those bounds and ids for the
   next step. Before the app has a window the reply is `unsupported` with
   `details.reason = "not_ready"` — never an empty tree.
5. **Act.** `acos_tap {"text": "Increment"}` (preferred), `acos_tap {"x":..,
   "y":..}`, `acos_key {"code": "back"}`, `acos_text {"text": "hello"}` (types
   into the focused editor through the real `InputConnection`),
   `acos_swipe {"from":[x1,y1], "to":[x2,y2]}`. Re-observe after every action;
   `state_changed: false` with `accepted: true` means the event was delivered
   but the screen did not change — read the in-band `note`.
6. **Read the app's own words.** `acos_logs` returns the guest log buffer and
   also `launched` / `stage_reached`, so an empty buffer is only clean if the
   app actually reached `onCreate`.
7. **Prove the flow.** `acos_verify {"apk": ..., "scenario": ...}` (or
   `acos verify <scenario.yaml> --artifacts out/<name>`) runs a YAML scenario and
   writes `verdict.json`, per-step JSON and the frames. The tool runs **real
   ART by default** (`engine: "art"`); pass `"engine": "rust-vm"` only to
   reproduce the frozen Rust VM, and the reply's `fidelity` then says
   `native-runtime`. With `required_fidelity: "art-reference"` a non-ART run is
   refused as `UNSUPPORTED_BACKEND` (achieved fidelity does not satisfy), never
   passed. A pass is valid only when its evidence exists on disk.
8. **Clean up.** `acos_reset {"scope": "app"}` (relaunch in a fresh guest with
   an empty app-data directory) and `acos_destroy` (idempotent). Sessions also
   die with the `acos-mcp` process.

<!-- /topic:workflow -->

<!-- topic:cli -->
## CLI reference (runnable examples)

```
acos doctor --json
acos inspect fixtures/build/apps/counter.apk --json
acos launch fixtures/build/apps/counter.apk --engine art --json
acos launch fixtures/build/apps/counter.apk --engine art --keep --json   # session survives in the daemon
acos observe --tree --json                        # the daemon's live session
acos observe --tree --apk fixtures/build/apps/counter.apk --json         # one-shot: launch then observe
acos observe --screenshot out/counter.png --json
acos tap --text Increment --json
acos tap --id inc --json
acos tap 100 400 --json
acos key back --json
acos swipe 540 1800 540 600 --duration-ms 300 --json
acos scroll 540 1800 600 --json                   # x, from-y, to-y
acos type --text "Ada" --json                     # focused editor only
acos logs --tag AcosCounter --min-priority I --json
acos session list --json
acos session status --json
acos session reset --json
acos session destroy --json
acos verify scenarios/counter.yaml --artifacts out/counter --json
acos verify scenarios/form-validation.yaml --engine art --json
acos serve                                       # unix-socket session daemon; stop it when done
acos frame-diff reference.png candidate.png --json
acos parity --recorded hello-views --acos-tree out/tree.json --json
acos native-inventory fixtures/build/apps/hello-jni.apk --abi x86_64 --json -
acos sandbox-probe --help
```

Notes that save a run:

* Top-level `observe/tap/key/swipe/logs/type` use a **running** daemon when one
  exists and otherwise fall back to a one-shot in-process session; `--apk`
  always launches a fresh one. `acos session …` always talks to the daemon
  (auto-starting it). `ACOS_NO_DAEMON=1` disables the daemon.
* `acos launch --engine art` uses real ART. Without `--engine art` the **CLI's**
  default route is the frozen Rust VM, which verifies far less. The **MCP**
  session tools are not like that: they run real ART by default (see Limits).
  Use `--engine art` on the CLI for anything you intend to trust.
* `reset` and `destroy` are daemon verbs: `acos session reset|destroy`, or the
  MCP tools `acos_reset` / `acos_destroy`. There is no top-level `acos reset`.
* `acos run-dex <path> --main Main` executes a DEX `main` on the Rust
  interpreter: console-tier checks only, not UI evidence.
* Every command takes `--json` and prints the same envelope the MCP tools
  return.

<!-- /topic:cli -->

<!-- topic:scenarios -->
## Writing a scenario (`acos verify`)

A scenario is YAML with `version: 1`. Minimal, runnable shape:

```yaml
version: 1
name: counter-increment
app: fixtures/build/apps/counter.apk
engine: native-preferred        # native-preferred | native-only | reference-only (see note)
fallback: report-and-escalate   # report-and-escalate | report-only | forbid
api_level: 34
isolation: process              # shared-task | process | sandbox | core | vm
display: {mode: offscreen, width: 1080, height: 2400, density_dpi: 420}
capabilities: [lifecycle, ui.text, input]
budgets: {memory_mib: 512, wall_ms: 60000}
expectation: pass               # pass | fail | non_pass
steps:
  - launch: {activity: dev.acos.fixtures.counter/.MainActivity, clear_state: true}
  - wait_for: {selector: {id: count}, timeout_ms: 5000}
  - assert_text: {selector: {id: count}, text: {exact: "0"}}
  - tap: {selector: {id: inc}, expect_change: true}
  - assert_text: {selector: {id: count}, text: {exact: "1"}}
  - assert_no_crash: {}
```

`engine:` and `fallback:` above are the legacy **native-vs-reference** preference
(`native-preferred` | `native-only` | `reference-only`) and are **not** the
execution engine. The execution engine is `acos verify --engine art|rust-vm`
(CLI) or the `engine` argument of the `acos_verify` tool (default: `art`, real
ART). ART verification does not read the YAML field.

A step is a **one-key map**. Valid keys:

| step key | payload |
|---|---|
| `launch` | `activity` / `intent_action` / `data` / `extras` / `clear_state` |
| `wait_for` | `selector`, `timeout_ms`, `require_visible` |
| `tap` | `selector` or `x`+`y`, optional `timeout_ms`, `settle`, `settle_timeout_ms`, `expect_change` |
| `type` | optional `selector`, `text`, `clear`, `settle`, `settle_timeout_ms`, `expect_change` |
| `key` | `key` (`back`, `home`, `enter`, `tab`, `delete`, `escape`, `menu`, `dpad_*`), `settle`, `settle_timeout_ms`, `expect_change` |
| `back` | `{}` or `{expect_change: true}` |
| `rotate` | `orientation: portrait\|landscape` — **unsupported on the ART session channel** (see Limits) |
| `assert_text` | `selector`, `text` matcher, optional `bounds`, `timeout_ms` |
| `assert_visible` / `assert_not_visible` | `selector`, optional `bounds`, `timeout_ms` |
| `assert_state` | `selector` + at least one of `visible/enabled/focused/clickable/scrollable/checked` |
| `assert_log` | `tag`, `patterns` (regex list), `ordered`, `since_previous_step`, `timeout_ms`, `min_matches` |
| `assert_no_crash` | optional `since_seq` |
| `assert_screenshot` | `baseline`, `tolerance` (0..1), `region_masks` (display pixels) |
| `assert_region` | `x`, `y`, `w`, `h` (**display pixels**) plus `equals_png` / `hash` / `min_distinct_colors` / `max_distinct_colors` |
| `capture` | `name`, `format: png\|raw`, `fresh` |
| `reset` | `scope: task\|app\|data\|all` |
| `set_network` | `offline`, `rules[]` (`method`,`url`,`status`,`headers`,`body`,`delay_ms`,`times`) |
| `set_permission` | `permission`, `state: grant\|deny\|revoke\|reset` |
| `set_locale` / `set_density` / `set_theme` / `set_time` | `locale` / `density_dpi` / `theme` / `timestamp` |
| `call_activity_method` | only for debuggable, source-owned builds with an injected hook |

There is no `sleep` step and no polling: `wait_for` and the assertions are
event-driven.

**Selectors.** All given fields are ANDed; matching is case-sensitive.

```yaml
selector:
  id: dev.acos.fixtures.counter:id/count   # exact; bare `count` matches the suffix after `/`
  text: {exact: "0"}                        # or {contains: "0"} or {regex: "^[0-9]+$"}
  content_desc: {contains: Submit}
  class: android.widget.Button
  bounds: [42, 368, 273, 494]               # display pixels [left, top, right, bottom]
  index: 0                                  # 0-based, pre-order document order
```

**Settle modes (the part that decides whether a pass is real).**

* `settle: "strict"` (the default) waits for a *settled* observation after the
  action — a frame/tree that satisfied the strict rule — and every assertion
  runs on it. Timeout is `TIMEOUT`, never a pass.
* `settle: "fast"` returns the first fresh sample immediately. It is for
  latency-sensitive probing only: it may be mid-animation, it is **never**
  assertion evidence, and `acos_assert` rejects it with `INVALID_ARGUMENT`.
  Scenarios cannot request fast for an assertion step at all.
* Strict settle does *not* require pixels to change. `expect_change: true` on a
  `tap`/`type`/`key`/`back` step is the opt-in that does: `pixels_changed:false`
  then fails with `ASSERTION_FAILED` / `details.reason: "expect_change"`.
  `expect_change: true` + `settle: fast` is rejected at parse time. Use it for
  any flow whose point is that a control did something.
* A step that provokes no draw at all cannot settle and ends as `TIMEOUT`
  (`reason: "no-draw-after-action"`). Both are non-passes.

**Assertions are always strict.** `assert_text`, `assert_visible`,
`assert_state`, `assert_log`, `assert_no_crash`, `assert_screenshot` and
`assert_region` never take a settle option; a scenario that cannot prove its
claim this way must fail.

**Two coordinate spaces.** Tree bounds, `tap` `x`/`y`, `assert_region` and
`region_masks` are **display** pixels (1080x2400 on the API-34 profile). A
captured PNG is the display's application area, so its own `(0,0)` is at
`frame.display_origin` (`[0,128]` there). `assert_region` converts internally; a
PNG pixel written where a display pixel belongs fails loudly with both
rectangles in the diff.

**Expectations.** `expectation: pass` requires a pass. `expectation: fail`
requires exactly `fail` — `unsupported`, `timeout` and `crash` do not satisfy
it. `expectation: non_pass` accepts any non-pass. If a negative scenario
"passes", the runner is broken; report a harness bug.

**Read the evidence.** `--artifacts out/<name>` writes `verdict.json`,
`steps/NN-*.json`, frames and events. On a non-pass, read the step file for the
first failing step, its `error.code`, the diff and the linked frame.

<!-- /topic:scenarios -->

<!-- topic:errors -->
## Error codes → what to do

| code(s) | what it means | what to do |
|---|---|---|
| `UNSUPPORTED_ANDROID_API`, `UNSUPPORTED_JAVA_API`, `UNSUPPORTED_BYTECODE`, `UNSUPPORTED_NATIVE_SYMBOL`, `UNSUPPORTED_RELOCATION`, `UNSUPPORTED_BACKEND`, `NEEDS_REAL_DEVICE`, `REQUIRES_GMS`, `WRONG_ABI`, `NO_DISPLAY`, `NO_TEXT` | the engine cannot do this | read `details.api`/`details.at`/`details.symbol`/`details.reason`, avoid the API, or report unsupported. Never rewrite the app to hide it |
| `ASSERTION_FAILED`, `ELEMENT_NOT_FOUND`, `ELEMENT_OFFSCREEN`, `TREE_TRUNCATED` | the app ran, a check did not hold | read the diff, re-observe the tree, fix the app or the expectation |
| `APP_CRASHED`, `APP_NOT_RESPONDING`, `JAVA_EXCEPTION` | the app crashed or hung | read `details.root_cause` (deepest `caused by:`) and `acos_logs`; fix the app |
| `APK_*`, `AXML_MALFORMED`, `ARSC_MALFORMED`, `DEX_*`, `ELF_MALFORMED`, `NATIVE_*` | the build artifact is broken | rebuild/re-sign/repackage, then `acos inspect` again. Not an app verdict |
| `UNSUPPORTED_ABI` | the APK ships native code but no `lib/x86_64/` | rebuild with the x86_64 ABI, or run the pure-Java path |
| `SESSION_NOT_FOUND`, `SESSION_BUSY`, `STALE_REF` | session misuse | create the session first, serialise mutations on one session, re-observe before using a node |
| `TIMEOUT`, `NO_FRESH_FRAME`, `RESOURCE_LIMIT`, `RAM_GUARD`, `RAM_ABORT` | a deadline or budget was hit | raise the explicit budget, wait for the next frame, reduce concurrency. A timeout is never a pass |
| `HOST_MEMORY_LOW` | **no** session was started: `MemAvailable` minus this session's budget would leave the host under 10% of `MemTotal` | free memory (destroy a session, clear scratch) and call again. Nothing is queued; `error.details` carries `mem_available_mib`, `mem_total_mib`, `requested_mib`, `min_free_mib` |
| `HOST_MEMORY_LIMIT` | a memory ceiling was hit **while** the guest ran | reduce concurrency or run on a host with more headroom; the guest was capped by its cgroup |
| `HOST_DISK_FULL` | the scratch filesystem is full | free space under `.cache/`, or point `--artifacts` elsewhere, then retry |
| `UNVERIFIED_FIDELITY`, `ARTIFACTS_INVALID`, `INSUFFICIENT_HOST_EVIDENCE` | the evidence does not support the claim | re-run at a qualifying fidelity with fresh artifacts; never accept a pass without evidence |
| `UNSAFE_PATH` | a path (or a symlink it names) escaped `ACOS_ALLOWED_ROOTS` | move the file under an allowed root, or add that root to `ACOS_ALLOWED_ROOTS` and restart; see Limits → Paths |
| `INVALID_ARGUMENT`, `INVALID_SCENARIO` | bad request or bad YAML | read `details`/the validation `path` and fix it |
| `INTERNAL`, `HOST_PANIC`, `GUEST_EXITED` | ACOS harness failure, needs triage | capture request/scenario/artifacts, report the harness bug; do not count it as a pass |

Two session-specific hints worth memorising:

* `UNSUPPORTED_BACKEND` + `details.reason = "not_ready"` — the app has no
  window/frame yet (`tap --text` resolves a node, so with no tree there is
  nothing to resolve). Launch, read the ladder, then act.
* `ELEMENT_NOT_FOUND` + `details.kind = NoWindowAtPoint` / `NoFocusedWindow` —
  there is no window at that point / no focused window, so a coordinate tap,
  key or swipe cannot be delivered.

<!-- /topic:errors -->

<!-- topic:limits -->
## Limits and what is not supported (as of this build)

* **API 34, x86_64.** The runtime is Google's ART from the API-34 ART APEX on
  an x86_64 host image. An APK whose native code has no `lib/x86_64/` entry is
  `UNSUPPORTED_ABI` on every route (that is a gate, not a silent fallback).
* **The MCP session tools run real ART by default.** On the first session tool
  call the server starts its **own** private `acos serve --engine art` on a
  private socket under `$XDG_RUNTIME_DIR/acos/mcp-<pid>-…/` and forwards
  `acos_launch/observe/tap/text/key/logs/status/reset/destroy` to it, so the
  loop is the same real-ART loop `acos verify --engine art` runs. That daemon is
  a child of the server: on stdin EOF/exit its sessions are destroyed and it is
  terminated and reaped, so sessions never outlive `acos-mcp`.
  `ACOS_SESSION_DAEMON=1` still forwards to an external `acos serve` when one is
  listening. `ACOS_MCP_ENGINE=legacy` selects the frozen in-process Rust VM /
  RN-bridge backend: it is **legacy, diagnostics only**, cannot run real APKs
  and is never chosen implicitly. Default session ids are unique per server
  (`mcp-<pid>`), so two agents on one daemon cannot collide.
* **Rotation is not supported** on the ART session channel: the guest line
  protocol has no `rotate`, so a `rotate` step returns `UNSUPPORTED_BACKEND`
  (`capability: input`). Do not build a scenario that depends on it.
* **Flutter and React Native are deferred.** Their APKs are recognised
  structurally, and the native-child session paths exist, but full driver
  coverage is not there; treat any result as experimental, never as the
  framework-View verdict. Framework (View-based) APKs are the supported path.
* **`reset` is host-side**: it stops the guest and starts the same APK again in
  a fresh process with an empty app-data directory. That is the honest meaning
  of "back to the initial UI" on this channel; the app's own Reset button is
  never tapped for you.
* **`type` needs focus.** Type goes to the focused editor through the real
  `InputConnection` (`text_path: "input-connection"`); if the focused view is
  not an editor it falls back to `KeyEvent`s (`text_path: "key-events"`,
  ASCII/US layout only) and characters it cannot represent are a structured
  `UNSUPPORTED_BACKEND` rather than a dropped character. Observe, tap the field,
  then type.
* **No session count limit.** As many sessions as the host has memory for; each
  guest runs in its own cgroup with a memory ceiling (`ACOS_ART_MEM_MAX`,
  default 2 GiB). The one admission rule is `HOST_MEMORY_LOW` above: a launch
  never queues and never starts without room. Free memory instead of waiting.
  `ACOS_MEMORY_GUARD=off` disables that admission check on a lab host with
  known-static memory; it does not disable the cgroup ceiling.
* **Taps before the first frame are delivered but do not land** (`accepted:
  true`, `state_changed: false`, non-empty `note`). Launch to `first_frame` and
  observe before asserting.
* **The pressed-state ripple is a declared flat-colour fallback**, not a
  pixel-identical device animation. Do not read interaction frames as a device
  screenshot.
* **Signing.** ACOS never installs APKs on a device; it reads them. A debug
  keystore is only needed to *build* fixtures (`tools/build-dogfood-apps.sh`
  generates one) and `acos doctor` reports it.
* **Paths.** Every path argument (`apk`, `scenario`, `artifacts`, `path`) must
  resolve under one of the roots in `ACOS_ALLOWED_ROOTS` (`:`-separated). The
  registered server defaults it to your home directory, plus the artifacts
  directory. The path is canonicalised, so a **symlink that resolves outside the
  roots is rejected** even when the link itself is inside one, and `..` is
  rejected before resolution; any violation is `UNSAFE_PATH` and nothing is read
  and no guest starts. Fix it by (a) copying/pointing at the APK or scenario
  inside an allowed root (its build output directory usually is one), or (b)
  adding that repository to `ACOS_ALLOWED_ROOTS` in the client's server config
  and restarting the server, or (c) writing evidence to the artifacts
  directory. Relative paths resolve against the server's working directory,
  never against the APK's directory.

<!-- /topic:limits -->
