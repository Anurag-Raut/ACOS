# ACOS agent guide

ACOS runs real Android apps on Google's real ART (API 34, x86_64) inside an
unprivileged `bwrap` sandbox and reports what actually happened: a launch ladder,
a live UI tree, frames, logs, and an evidence-backed verdict. Agents are the
only users. This file is the complete operating guide; the MCP tool
`acos_help` returns a short skeleton of it, `acos_help {"topic": ...}` one
section and `acos_help {"topic": "all"}` the whole thing.

<!-- topic:overview -->

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

## The loop, in ten lines

```
doctor -> inspect -> launch -> observe_tree -> tap/text/key/swipe -> logs -> verify -> reset -> destroy
```

1. `acos_doctor` — host preflight; fix every failing check's `fix:` first.
2. `acos_inspect {"apk": ...}` — static: package, launcher, SDK, DEX/native.
3. `acos_launch {"apk": ...}` — boot it (**absolute** paths only), read
   `reached_stage`; the first non-ok rung carries the blocker. Idle sessions
   expire after 600 s (`idle_ttl_s`; see `workflow`).
4. `acos_observe_tree` — nodes with `id`/`text`/`bounds`; act on those.
5. `acos_tap` / `acos_text` / `acos_key` / `acos_swipe` — then re-observe.
6. `acos_logs` — the app's own words; `launched`/`stage_reached` included.
7. `acos_verify {"apk","scenario"}` — the verdict and its artifacts are the
   product; `"engine": "art"` for anything with `animations: off`.
8. Read `verdict.json` + `steps/NN-*.json` on a non-pass; never guess.
9. `acos_reset {"scope": "app"}` — fresh guest, empty app data.
10. `acos_destroy` — idempotent; sessions also die with the server.

## Topics

`acos_help {"topic": "<name>"}` returns one section; `"all"` returns everything.

* `overview` — this page: the three rules, the loop in ten lines, the topic list.
* `mcp` — every MCP tool: arguments, an example call, what it returns.
* `workflow` — the loop step by step, plus screenshot / type / find / prove-a-
  flow recipes.
* `cli` — every `acos` command with a runnable example.
* `scenarios` — writing a YAML scenario: steps, selectors, settle rules.
* `errors` — error code → what to do next.
* `limits` — what this build cannot do (ABI, rotation, RN/Flutter, paths).
* `all` — the complete guide in one reply (largest; ask for a section when you
  can).

Call `acos_help {"topic": "workflow"}` next if you are about to run an app.

<!-- /topic:overview -->

<!-- topic:workflow -->
## Recipes (answer these first)

**Take a screenshot.** There is no capture tool on the MCP surface yet. Two
routes work today: (a) a `capture: {name: shot, format: png, fresh: true}` step
in an `acos_verify` scenario — the PNG lands as `frames/<seq>.png` under the
artifacts directory and the step JSON names it; (b) with the CLI on a live
daemon session, `acos observe --screenshot out/shot.png --json`. Coming:
`screenshot: true` on `acos_observe_tree`, which returns the PNG path in the
reply (and `NO_FRESH_FRAME` instead of a silent no-op when the frame carries no
pixels).

**Type into a field.** Observe the tree, tap the field (`acos_tap {"id": ...}`
or `{"text": ...}`), type with `acos_text {"text": "Ada"}`, then observe again to
read the value. Text goes to the *focused* editor through the real
`InputConnection`; `ELEMENT_NOT_FOUND` with `details.kind = NoFocusedEditor`
means the tap did not land on an editable field. If `acos_text` answers
`UNSUPPORTED_BACKEND` on a live session, the same interaction as a scenario
`type:` step runs through `acos_verify` and does work.

**Find a control by `content_desc`.** `acos_observe_tree` returns `text` **and**
`content_desc` per node; icon buttons usually carry their name only in
`content_desc` (for example `Search Japanese dictionary`). Select it in a
scenario with `selector: {content_desc: {contains: Search}}`, or hand the label
to `acos_tap {"text": ...}` (text or content-desc), or tap the centre of the
node's `bounds` with `x`/`y`. Never read a coordinate off a PNG: PNG space is
offset by `frame.display_origin`.

**Prove a flow.** Put the interaction in a YAML scenario (`topic: "scenarios"`)
and run `acos_verify` (real ART by default). The pass counts only with its
evidence on disk: `verdict.json`, `steps/NN-*.json` and the referenced frames —
and `expect_change: true` on the taps whose point is to change the screen is
what makes the pass mean something.

**Launch arguments.** Paths must be **absolute**: the server's cwd is not your
shell's, and a relative path is `INVALID_ARGUMENT` with
`details.resolved_path`. Each launch rung carries `ms`. Pass `"label": "..."`
and, if you will park the session, `"idle_ttl_s": N`: the server destroys its
own sessions after the idle TTL (default 600 s, `ACOS_SESSION_IDLE_TTL_S`) and
the next call on an expired one returns `SESSION_NOT_FOUND` with
`details.reason=idle_ttl_expired` and the TTL.

**Assert, do not eyeball.** `assert_text` (optionally with `bounds`),
`assert_visible` / `assert_not_visible` (collapsed content stays in the tree with
`visible: false`, so assert visibility, never presence) and `assert_log` turn a
visual claim into a machine check with a diff.

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
5. **Act.** `acos_tap {"text": "Increment"}` (preferred),
   `acos_tap {"content_desc": "Search Japanese dictionary"}`,
   `acos_tap {"id": "inc"}`, `acos_tap {"x":.., "y":..}`,
   `acos_key {"code": "back"}`, `acos_text {"text": "hello"}` (types into the
   focused editor — the same guest `type_text` command a scenario `type` step
   sends), `acos_swipe {"from":[x1,y1], "to":[x2,y2]}`. `text` names a node the
   way the platform does — its text, else its content description — so a React
   Native pressable (`accessibilityLabel`, e.g. a bottom tab) is reachable by the
   label you read on screen. `content_desc` and `id` are the *explicit* fields:
   use them when the label lives in a content description while the node's text
   is something else (an RN `TextInput` whose text is its placeholder), or simply
   when you want the model to be exact. A selector that matches **several** nodes
   the user can see is refused (`AMBIGUOUS_SELECTOR`, `error.details.candidates`
   lists every match with id/text/content_desc/bounds) — narrow it with
   `content_desc`/`id`/`bounds`, or pass `index`; nothing is tapped on a guess.
   A node that is not visible to the user is never tapped: `ELEMENT_OFFSCREEN`
   names its bounds. Re-observe after every action;
   `state_changed: false` with `accepted: true` means the event was delivered
   but the screen did not change — read the in-band `note`.
6. **Read the app's own words.** `acos_logs` returns the guest log buffer and
   also `launched` / `stage_reached`, so an empty buffer is only clean if the
   app actually reached `onCreate`. The **default view is `min_priority: "W"`
   plus the app's own package** (records at warning or above, plus anything that
   names the app), which keeps SoLoader/framework noise out of the reply. The
   full buffer stays on disk (`logs/logcat.txt` in a verify run), `"all": true`
   or `min_priority: "V"` returns all of it, and `tag` narrows further.
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

### Recipe: type into a search field

A live session can type — the same path a scenario `type` step uses — so a
search flow no longer has to become a scenario. In four calls:

```
1. acos_observe_tree                      # read the field's name from the tree
2. acos_tap {"content_desc": "Search Japanese dictionary"}
3. acos_text {"text": "kayou"}
4. acos_observe_tree                      # the query is in the field, results below
```

* **Pick the field by `content_desc`, not by `text`.** A React Native
  `TextInput` reports its *placeholder* as `text` (AOSP's accessibility mapping
  takes the hint when the field is empty) and keeps the accessibility label in
  `content_desc`. So `acos_tap {"text": "Search Japanese dictionary"}` is
  `ELEMENT_NOT_FOUND` while `acos_tap {"content_desc": "..."}` hits the field —
  the same value works in a scenario selector.
* **Tap first, then type.** Typing goes to the focused editor. The tap's
  `accepted: true` + `state_changed: true` is the delivery evidence; the next
  tree's `focused: true` on the field node is the focus evidence.
* **No class check is needed.** RN inputs are obfuscated subclasses
  (`Z4.g`), not `android.widget.EditText`; `acos_text` does not care what the
  class is called — it sends the guest's `type_text` command and judges the
  settled result, exactly like `type:` in YAML.
* **The tree proves it.** After `acos_text {"text": "kayou"}` on Gaten, the next
  tree carries `kayou` in the field and the result list (`19 results`, `歌謡`,
  `かよう`, ...). Assert on those texts (or in a scenario with
  `assert_text`) — never on the call's `status` alone.
* **The reply names the field, and masks unless it is known to be plain.**
  `acos_text`'s `result.target.node` is the focused field the input went to, as
  the tree shows it. The typed text is echoed (`target.text`) only when that
  field is focused and is not a password. Otherwise the reply carries
  `target.text_masked: true`, `target.chars` (the honest count), `target.text:
  "*"` and a `reason`: a focused password field, `mask: true`, or an
  **unknown target** (the tree names no focused field; nothing is guessed, and no
  other field is ever reported in its place). Masking only hides the echo: the
  tree still shows what the app displays, so prove typing with the next
  `acos_observe_tree`, not with the reply.

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
acos tap --content-desc "Search Japanese dictionary" --json
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
acos verify scenarios/counter.yaml --artifacts out/counter --overwrite --json  # replace a prior run's evidence
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
  return. With `--json`, **stdout carries only the JSON document**; progress,
  warnings and the one-line failure summary go to **stderr**. So
  `acos verify … --json > out.json 2>&1` is *not* JSON — send stderr elsewhere
  (`2>err.txt`). Text mode prints the human summary on stdout.
* `acos verify --artifacts <dir>` gives one run ownership of that directory. A
  non-empty dir written by another run is refused (`INVALID_ARGUMENT` naming the
  previous run id); use a fresh dir, or `--overwrite` to replace that evidence
  deliberately. Without `--artifacts` the run uses `out/<scenario stem>`, and a
  rerun there simply replaces the previous run (the fix -> rerun loop needs no flag).

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

`launch.activity` takes the same component spellings Android does, expanded
before the launch gate runs: `com.example.app/.MainActivity` (a real
`adb shell am start` component, `ComponentName.unflattenFromString`),
`com.example.app/MainActivity`, `.MainActivity` and a bare `MainActivity`
(`PackageParser.buildClassName`). A component prefix may differ from the
manifest package (`com.gaten` + `com.gaten.app/.MainActivity`). A target the APK
does not declare is `INVALID_SCENARIO` (status `error`, the app never ran) with
`details.requested`, `details.resolved` and `details.declared_activities` — not
`ASSERTION_FAILED`.

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

**Visited vs. named: what a `tap` selector resolves to.** `visible` in a tree is
Android's own `View.isVisibleToUser()` answer — attached, the view and every
ancestor `VISIBLE`, alpha > 0, and an on-screen clipped rect — copied verbatim,
never recomputed by ACOS. `assert_visible`/`assert_text`/`wait_for` match only
the nodes that answer yes to it. A **tap** additionally resolves the selector
against the platform's *accessible name*, which is the node's text, else its
content description (`AccessibilityNodeInfo.getText()`, else
`getContentDescription()`), because that is the name an agent (and UiAutomator's
`By.desc`, and an `agent-device` `label`) targets. Concretely, `acos_tap
{"text": "Settings"}` finds a visible node whose own text is `Settings`, or — when
no visible node has that text — a visible node with no text of its own whose
`content_desc` is `Settings`. React Native renders every pressable as a
text-less `ViewGroup` carrying its `accessibilityLabel` in the content
description, so this is what makes tap-by-label work on RN apps (`text` is empty
on the tab pressable, and the tab's decorative label `TextView` is not visible
to the user at all). The tap lands at the centre of the target's *visible* rect
(or `x`/`y`), and a node that is not visible to the user is never a tap target:
if only such a node matches, the reply is `ELEMENT_OFFSCREEN` with its bounds,
never a tap. When several nodes match, the first in document order wins;
`index` picks another. (The **live** channel — `acos_tap`/`acos_assert` on a
`acos serve` session and the CLI `tap` that forwards to it — refuses several
user-visible matches instead: `AMBIGUOUS_SELECTOR` with the candidates listed.
A scenario step is a fixed document: it keeps first-match-plus-`index`.)

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
`steps/NN-*.json`, frames and events. `verdict.json` carries both `verdict` and
`status` (the envelope's field name), and every assertion echoes `expected` and
`actual` on a pass as well as a failure, so the artifacts say *what* was checked.
A `capture: {name: x}` step writes `frames/<seq>-x.png` and adds
`captures: {"x": "frames/<seq>-x.png"}` to the verdict. On a non-pass, read the
step file for the first failing step, its `error.code`, the diff and the linked
frame. Reusing another run's `--artifacts` directory is `INVALID_ARGUMENT`
(`--overwrite` replaces it).

**Screenshots are compressed, losslessly.** A capture is a PNG with real deflate
and adaptive row filtering: the same pixels as the compositor's buffer (region
and frame assertions decode it, so nothing about them changed) in roughly
**60-100 KB** instead of the old 10.4 MB stored-block file. `png_bytes` in the
reply says which. Read the file for exact pixels:
`acos observe --screenshot out/counter.png`.

**In an MCP reply the image is a bounded preview.** `acos_observe` (and any tool
that returns a screenshot) inlines at most 1280 px on the long side and 512 KiB
(`ACOS_MCP_PREVIEW_MAX_SIDE`, `ACOS_MCP_MAX_IMAGE_BYTES`); a 1080x2400 capture
arrives as a 576x1280 PNG plus the path of the lossless file. Use the file when
you need to compare pixels, and the inline preview for "is it the right screen".

<!-- /topic:scenarios -->

<!-- topic:errors -->
## Error codes → what to do

| code(s) | what it means | what to do |
|---|---|---|
| `UNSUPPORTED_ANDROID_API`, `UNSUPPORTED_JAVA_API`, `UNSUPPORTED_BYTECODE`, `UNSUPPORTED_NATIVE_SYMBOL`, `UNSUPPORTED_RELOCATION`, `UNSUPPORTED_BACKEND`, `NEEDS_REAL_DEVICE`, `REQUIRES_GMS`, `WRONG_ABI`, `NO_DISPLAY`, `NO_TEXT` | the engine cannot do this | read `details.api`/`details.at`/`details.symbol`/`details.reason`, avoid the API, or report unsupported. Never rewrite the app to hide it |
| `ASSERTION_FAILED`, `ELEMENT_NOT_FOUND`, `ELEMENT_OFFSCREEN`, `AMBIGUOUS_SELECTOR`, `TREE_TRUNCATED` | the app ran, a check did not hold | read the diff, re-observe the tree, fix the app or the expectation. `AMBIGUOUS_SELECTOR` means several visible nodes match: pick one with `id`/`content_desc`/`bounds` or `index` (`error.details.candidates` lists them) |
| `APP_CRASHED`, `APP_NOT_RESPONDING`, `JAVA_EXCEPTION` | the app crashed or hung | read `details.root_cause` (deepest `caused by:`) and `acos_logs`; fix the app |
| `APK_*`, `AXML_MALFORMED`, `ARSC_MALFORMED`, `DEX_*`, `ELF_MALFORMED`, `NATIVE_*` | the build artifact is broken | rebuild/re-sign/repackage, then `acos inspect` again. Not an app verdict |
| `UNSUPPORTED_ABI` | the APK ships native code but no `lib/x86_64/` | rebuild with the x86_64 ABI, or run the pure-Java path |
| `SESSION_NOT_FOUND`, `SESSION_BUSY`, `STALE_REF` | session misuse — or the session **expired**: `details.reason=idle_ttl_expired` means the server destroyed it after `details.idle_ttl_s` seconds of idleness | create/relaunch the session (`acos_launch`), serialise mutations on one session, re-observe before using a node; pass a longer `idle_ttl_s` if you intend to park it |
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
  `UNSUPPORTED_ABI` on every route (that is a gate, not a silent fallback). On
  the ART path it is decided **before any guest starts**, so you get
  `UNSUPPORTED_ABI` (never `GUEST_EXITED`, never `INTERNAL`) with
  `status: "unsupported"` and `details.apk_abis` / `details.host_abi` /
  `details.libs_sample` naming exactly what the APK does ship. The decision is
  AOSP's, per **package**: one `lib/x86_64/` directory is enough to launch, only
  that directory is installed, and a library that exists solely under another
  ABI fails later at its own `dlopen` (`UnsatisfiedLinkError`) — an app
  packaging bug, not an ABI gate. A pure-Java APK (no `lib/` at all) launches.
* **Watching a session is view-only and costs nothing when unused.** A live
  session publishes a registry entry (`$XDG_RUNTIME_DIR/acos/live/`) and a
  `stream_socket` for local viewers (half-resolution frames + an action feed,
  ≤ 15 fps). Agents never need it; `docs/LIVE_VIEW.md` has the protocol and the
  measured cost. Nothing an agent reads or asserts comes from the stream.
  Agent screenshots (`observe` screenshot / `capture_c1`) do **not** honour
  `FLAG_SECURE`; only the live stream blanks secure windows.
  To actually **watch** one (yours, or an agent's, from a browser or T3 Code),
  run `acos view` in a terminal and open the one line it prints:

  ```
  $ acos view
  acos view: http://127.0.0.1:54321/?t=<token>
  ```

  The page lists every live session (agent/client, project, APK, activity, age)
  and streams the one you click with tap ripples and the action feed. It is
  127.0.0.1 only and every request needs the token, so treat the URL as a
  secret: no other route exists, no input reaches the guest, and the frames stop
  the moment the last tab closes. `acos view --once --json` lists the sessions
  and exits (for scripts and tests). In **T3 Code**, open that URL in the
  built-in preview pane (paste it in the pane's address bar); no patch or
  plugin is needed, per ADR-0027.
* **The MCP session tools run real ART by default.** On the first session tool
  call the server starts its **own** private `acos serve --engine art` on a
  private socket under `$XDG_RUNTIME_DIR/acos/mcp-<pid>-…/` and forwards
  `acos_launch/observe/tap/text/key/logs/status/reset/destroy` to it, so the
  loop is the same real-ART loop `acos verify --engine art` runs. That daemon is
  a child of the server: on stdin EOF/exit its sessions are destroyed and it is
  terminated and reaped, so sessions never outlive `acos-mcp`.
  `ACOS_SESSION_DAEMON=1` still forwards to an external `acos serve` when one is
  listening. The server finds the CLI it spawns as `ACOS_BIN` when set, else the
  `acos` next to its own executable (how a normal install ships it), else the
  first `acos` on `PATH`. `ACOS_MCP_ENGINE=legacy` selects the frozen in-process Rust VM /
  RN-bridge backend: it is **legacy, diagnostics only**, cannot run real APKs
  and is never chosen implicitly. Default session ids are unique per server
  (`mcp-<pid>`), so two agents on one daemon cannot collide.
* **Rotation is not supported** on the ART session channel: the guest line
  protocol has no `rotate`, so a `rotate` step returns `UNSUPPORTED_BACKEND`
  (`capability: input`). Do not build a scenario that depends on it.
* **App types, one row per route.** Same APK, same engine, same verdict rules:

  | app type | MCP session tools (`acos_launch` -> `acos_verify`) | CLI `acos launch/observe/verify --engine art` | notes |
  |---|---|---|---|
  | Framework Views (Java/Kotlin native) | supported | supported | The reference path: real ART, real `View` tree, real frames. |
  | React Native | **supported** (real ART via the MCP session tools, since v0.2.0) | **supported** (real ART, since v0.2.0) | Verified end-to-end on a React Native release APK: launch ladder, tree, taps and `acos_verify` pass with `fidelity: art-reference`. The tree is the framework `View` hierarchy the RN host creates, not a JS component tree. |
  | Flutter | deferred | deferred | Recognised structurally; no driver coverage, so any result is a structural observation, never a Flutter framework verdict. |

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
