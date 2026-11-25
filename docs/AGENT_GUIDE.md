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
   `reached_stage`; the first non-ok rung carries the blocker. The launch reply
   is compact (`tree_path` = full tree). Idle sessions expire after 600 s.
4. `acos_observe_tree` — visible text/desc nodes with `id`/`bounds`/`clickable`;
   long lists page with `cursor`.
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
A step settles on the screen its action produced; a change the app makes **later** (a delayed post, a worker thread, a timer) may land after the step settled. Check such an effect with `assert_text` + `timeout_ms` (a real wait), not with the action step's result.

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
   behaviour. It also cross-checks the **native libraries the app asks for**
   against the ones the APK ships: `result.native_lib_check.warnings` lists
   `{"code": "NATIVE_LIB_MISSING", "lib": "libworklets.so", "abi": "x86_64",
   "source": "elf-needed"|"dex-load-library", "referenced_by": [...]}` for every
   library named by a bundled library's `DT_NEEDED` or by a
   `System.loadLibrary`/`SoLoader.loadLibrary` call site in the DEX that has no
   `lib/<abi>/` entry. Platform libraries (`libc.so`, `libm.so`, …) are never
   warnings, and a healthy APK answers `warnings: []` — so a non-empty list is a
   **hint**, not the cause: it says "this library is referenced and absent", not
   "this is why the app misbehaves". Treat it as the first thing to check before
   debugging the UI, then confirm at runtime.
3. **Run it.** `acos_launch {"apk": "..."}` (MCP, session persists) or
   `acos launch <apk> --engine art --json`. **The MCP session tools always run
   real ART** (the server starts its own private `acos serve --engine art`
   daemon); the `acos` CLI's own default route is the frozen Rust VM, so pass
   `--engine art` there. Read the ladder:
   `apk_parsed -> process_booted -> bind_application -> application_on_create ->
   client_transaction -> activity_on_create -> on_resume -> first_frame`.
   `reached_stage` is the honest answer to "how far did it get"; the first
   rung that is not `ok` carries the blocker. **A launch that gets a window but
   never mounts the app's own content is `blocked` with
   `error.code = PLACEHOLDER_CONTENT`**, never `pass`/`first_frame`: the reply's
   `result.content` says `{"mounted": false, "reason": "placeholder-content"}`,
   `stage_reached` is `window`, and `error.details.log_hint` quotes the one guest
   log line that explains it (an `UnsatisfiedLinkError`, a `SoLoader` line, a
   `couldn't find` line, else the first `E` line; if the log says nothing the
   hint says so instead of guessing). The usual cause is a native library the
   APK references and does not ship — `acos_inspect`'s
   `native_lib_check.warnings` names it statically. Do not retry the same APK
   with a longer timeout; it is the packaging.
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
   sends), `acos_swipe {"from":{"x":x1,"y":y1}, "to":{"x":x2,"y":y2}}`. `text` names a node the
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
   names its bounds. **The reply already says what changed** (FB3): `changes`
   carries the settled tree diff of the action — `added`, `removed` and
   `changed` nodes (each with `id`, `text`, `content_desc`, `class`, `bounds`; a
   changed node also lists the `fields` that changed, old → new), plus the
   current `activity` and the `focused` node. A normal step is one call, not
   tap-then-observe; call `acos_observe_tree` only when you need the whole tree
   or bounds you have not seen (`diff: false` restores the old reply).
   `state_changed: false` with `accepted: true` means the event was delivered
   but the screen did not change — read the in-band `note` and `changes`.

   **Scrolling a list (REL3S).** The ART engine publishes a real gesture, not a
   jump: `DOWN`, one `MOVE` every ~16 ms (the display refresh cadence) with real
   event timestamps, then `UP`, on the same input channel a tap uses. The app's
   own `VelocityTracker`/`OverScroller` therefore decide the fling, so a
   `ListView`/`RecyclerView`/RN `FlatList` scrolls exactly as it does on a phone.
   Three ways to name the gesture:

   ```
   acos_swipe {"from": {"x": 300, "y": 1100}, "to": {"x": 300, "y": 400},
               "duration_ms": 300, "fling": true}
   acos_swipe {"direction": "up", "distance": 900, "duration_ms": 300}
   acos_swipe {"direction": "up", "fling": true, "until": {"text": "Row 80"}}
   ```

   `direction` (`up`/`down`/`left`/`right`) gestures across the display centre;
   `up` moves the finger up, which scrolls the content up and brings later rows
   into view. `duration_ms` defaults to 400 (120 with `fling: true`); a slow
   drag follows the finger, a fling coasts on the list's own physics.

   **`until` is `scroll_until`.** With a selector, the call repeats the swipe
   (bounded by `max_attempts`, default 6, 10 with `fling`) and after **every**
   swipe takes a fresh **settled** traversal and looks for the node. `until.found`
   is only ever reported from such a traversal (`until.attempts`,
   `until.frame_seq` name it). **Two consumed swipes that move nothing end the
   search** (REL3H2): when the settled tree before and after two *consumed*
   gestures in a row is identical, the list has reached its end and the call
   fails with `END_OF_LIST` (`error.details.end_of_list: true`, `attempts`,
   `max_attempts`, `unchanged_gestures`, `last_tree_texts`) instead of spending
   the remaining attempts. One unchanged gesture is deliberately not enough: a
   list that is still loading is unchanged for a gesture too. **A swipe the app
   did not consume** (`finishInputEvent(handled) = false`, or a settle with
   `reason: unhandled_input`) moved nothing because it missed the scroller — that
   is `ELEMENT_NOT_FOUND` with the hint "gesture not consumed (swipe missed the
   scroller?)", never `END_OF_LIST`. Otherwise a node that never appears fails
   with `ELEMENT_NOT_FOUND`, `error.details.attempts`,
   `error.details.max_attempts` and `error.details.last_tree_texts` (what the
   list actually showed at the end) — never a pass, and never a guess from a
   cached tree. `until` requires the
   strict settle; `settle: fast`/`content` is refused because neither can prove a
   settled observation.

   `handled` works per event: a scrolling gesture reports
   `handled_evidence.published` equal to `acked` (a 300 ms swipe is ~20 events,
   not 2) and `unhandled` lists any event the window dropped — e.g. a gesture
   that lands on a non-scrollable view.

   **The same two steps in a scenario (REL3S).** A scenario scrolls with the
   same gesture and the same search:

   ```yaml
   - swipe: {direction: up, distance: 900, duration_ms: 300, fling: true}
   - scroll_until: {selector: {text: {exact: "Row 80"}}, fling: true, max_attempts: 12}
   - tap: {selector: {text: {exact: "Row 80"}}}
   ```

   `swipe` takes `from`/`to` points or `direction` (+ `distance`), `duration_ms`,
   `fling`, the per-step `settle`/`settle_timeout_ms` rules and SE1's
   `expect_change`. `scroll_until` takes the same geometry plus the `selector`
   to find and `max_attempts` (1..50); it always runs strict. Both report REL3E's
   `input` block (`handled`, `handled_evidence`, `target`), and `scroll_until`
   also reports `input.until {selector, found, end_of_list, attempts, frame_seq}`;
   a miss fails the step with `ELEMENT_NOT_FOUND` (every attempt used, or the
   swipe was not consumed) or `END_OF_LIST` (two consumed swipes moved nothing:
   the list ended) plus `details.attempts` / `details.last_tree_texts`. The next
   `assert_*` binds to the swipe/search's own settled frame.
   Identity rule, compared fields and the 40-entry cap: `docs/MCP.md` §1c.
   **`handled` is the app's own answer, and it is the one you branch on.**
   Every `acos_tap`/`acos_key`/`acos_text`/`acos_swipe` reply (and every
   scenario `tap`/`type`/`key`/`back` step JSON, under `input`) carries
   `handled`: the bit the app window passed to Android's
   `InputEventReceiver.finishInputEvent(handled)`. It is the framework's
   verdict, not a guess from pixels:

   - `handled: false` — **nothing in the app consumed the input.** A tap on a
     label whose row is not pressable (the switch beside it is) is exactly this:
     `state_changed: false` is then the correct, expected outcome, not a broken
     tap. The same reply's `target` names what happened: `target.node` is the
     node the selector resolved to, `target.hit.node` is what the injected point
     actually hit (its `class`, `text`/`content_desc`, `bounds`, `clickable`),
     `target.hit.consumer` is the nearest clickable ancestor-or-self (`null`
     when there is none), and `target.hit.suggest` is the nearest clickable node
     off that branch — the switch, the row's pressable, the button. A
     `handled_hint` spells it out: *"nothing tappable there … tap the nearest
     clickable android.widget.Switch … instead"*.
   - `handled: true` + `state_changed: false` — the app **did** consume it and
     the settled frame still shows no change: the handler may be asynchronous,
     or the control already had that state. `handled_hint` says so. Assert on
     the state you expect rather than expecting a change.
   - `handled: true` + `state_changed: true` — the press was consumed and the
     screen changed; `changes` says what.
   - `handled: null` — the guest did not ack every event
     (`handled_evidence.acked` < `published`): **unknown**, never reported as
     "not handled". `handled_evidence` always travels with the boolean
     (`source`, `published`, `acked`, `unhandled` seqs).
   - `handled: true` + `handled_evidence.source: "host_window_removed"` — the
     window the input was delivered to was removed during the step (a dialog
     dismissed by `back`). Its `InputEventReceiver` is disposed before it can
     finish the event, so the host's removal is the proof and it outranks an
     all-unhandled ack set (`source` is then `host_window_removed`, never
     `guest`, and the step carries no `INPUT_UNHANDLED` warning). `source` is
     `guest` when an ack answered, `none` when nothing was delivered.

   Two consequences worth knowing. (a) `state_changed` is evidence about the
   screen; `handled` is evidence about the app. An agent that only reads
   `state_changed` cannot tell "nothing tappable there" from "handled, nothing
   changed" — read `handled` first. (b) The hit-test in `target.hit` is
   computed from the tree the tap resolved against (visible rects, deepest
   node first); it tells you *what is there*. `handled` tells you *whether the
   app took it*. When they disagree (`hit.clickable: true`, `handled: false`)
   the control was hit but refused the press — disabled, or a parent
   intercepted it.
6. **Read the app's own words.** `acos_logs` returns the guest log buffer and
   also `launched` / `stage_reached`, so an empty buffer is only clean if the
   app actually reached `onCreate`. The **default view is `min_priority: "W"`
   plus the app's own package** (records at warning or above, plus anything that
   names the app), which keeps SoLoader/framework noise out of the reply. The
   full buffer stays in `logs/logcat.txt` (a live reply names `log_path`),
   `"all": true` or `min_priority: "V"` returns all of it, and `tag` narrows
   further.
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
acos guide                                    # the agent guide (same text as MCP acos_help)
acos guide workflow                           # one topic: workflow|cli|scenarios|errors|limits|mcp
acos guide all                                # the whole guide
acos doctor --json                            # includes `gles`: angle-swiftshader | none
acos inspect fixtures/build/apps/counter.apk --json
acos launch fixtures/build/apps/counter.apk --engine art --json
acos launch fixtures/build/apps/counter.apk --engine art --keep --json   # session survives in the daemon
acos launch fixtures/build/apps/counter.apk --engine art --service feedback.vibrator:stub --json
acos launch fixtures/build/apps/counter.apk --engine art --keep --label counter --device-name "Counter Lab" --json
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
acos view                                        # local interactive browser hub (127.0.0.1 + token)
acos view --once --json                          # list live view-registry sessions and exit
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
* `--service id[:profile][,config]` attaches a service module to the session
  (repeatable; `core` is always attached first at `real`). It needs a session
  host, so use `--engine art` (or `--keep`); the bare in-process Rust VM has no
  service table and refuses with `UNSUPPORTED_BACKEND` instead of ignoring the
  flag. The launch reply's `modules[]` reports what was actually attached and
  its `fidelity` (`real`/`stub`/`scripted`/`off`); check it before trusting a
  result that depends on a module. Live sessions change the set with
  `acos_services` (MCP).
* `reset` and `destroy` are daemon verbs: `acos session reset|destroy`, or the
  MCP tools `acos_reset` / `acos_destroy`. There is no top-level `acos reset`.
* `acos run-dex <path> --main Main` executes a DEX `main` on the Rust
  interpreter: console-tier checks only, not UI evidence.
* `acos guide [topic]` prints the agent guide from the same source as the MCP
  `acos_help` tool (`docs/AGENT_GUIDE.md`); `acos guide all` is the whole text.
  A CLI-only agent does not need the checkout to read the scenario schema.
* **Human output contract:** with `--json`, stdout is the JSON document and
  nothing else; the human summary (status line, warnings, the next action) goes
  to stderr. In text mode the human summary is stdout. Never parse text mode to
  get a verdict, and never mix the streams (`2>&1`) when the output is JSON.
* Every command takes `--json` and prints the same envelope the MCP tools
  return. So `acos verify … --json > out.json 2>&1` is *not* JSON — send stderr
  elsewhere (`2>err.txt`).
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
engine: rust-vm                 # art | rust-vm (the same names --engine takes)
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

`engine:` and `fallback:` are **metadata** about what the scenario was written
for; the execution engine is chosen by `acos verify --engine art|rust-vm` (CLI)
or the `engine` argument of `acos_verify` (MCP). Both use the **same two words**:
`art` (real ART) and `rust-vm` (the frozen Rust VM). The default is `art` in
both front doors, from one constant, so `--help` cannot disagree with behaviour.

The pre-v0.3 YAML spellings still parse and are reported as a warning that names
the current word: `native-preferred` and `native-only` mean `rust-vm`,
`reference-only` means `art`. `engine: art` is accepted, not
`INVALID_SCENARIO: unknown variant 'art'`.

A step is a **one-key map**. Valid keys:

| step key | payload |
|---|---|
| `launch` | `activity` / `intent_action` / `data` / `extras` / `clear_state`, `settle`, `settle_timeout_ms` |
| `wait_for` | `selector`, `timeout_ms`, `require_visible`, `settle`, `settle_timeout_ms` |
| `tap` | `selector` or `x`+`y`, optional `timeout_ms`, `settle`, `settle_timeout_ms`, `expect_change` (`true` = must change, `false` = transient effect / no change claimed, unset = the auto tree rule), `expect_handled` |
| `type` | optional `selector`, `text`, `clear`, `settle`, `settle_timeout_ms`, `expect_change`, `expect_handled` |
| `key` | `key` (`back`, `home`, `enter`, `tab`, `delete`, `escape`, `menu`, `dpad_*`), `settle`, `settle_timeout_ms`, `expect_change`, `expect_handled` |
| `back` | `{}`, `{expect_change: true}` or `{expect_handled: false}` |
| `rotate` | `orientation: portrait\|landscape` — **unsupported on the ART session channel** (see Limits) |
| `assert_text` | `selector`, `text` matcher, optional `bounds`, `timeout_ms`, `settle` |
| `assert_visible` / `assert_not_visible` | `selector`, optional `bounds`, `timeout_ms`, `settle` |
| `assert_state` | `selector` + at least one of `visible/enabled/focused/clickable/scrollable/checked`, `timeout_ms`, `settle` |
| `assert_log` | `tag`, `patterns` (regex list), `ordered`, `since_previous_step`, `timeout_ms`, `min_matches` |
| `assert_no_crash` | optional `since_seq` |
| `assert_screenshot` | `baseline`, `tolerance` (0..1), `region_masks` (display pixels) |
| `assert_region` | `x`, `y`, `w`, `h` (**display pixels**) plus `equals_png` / `hash` / `min_distinct_colors` / `max_distinct_colors` |
| `capture` | `name`, `format: png\|raw`, `fresh` |
| `reset` | `scope: task\|app\|data\|all` |
| `set_network` | `offline`, `rules[]` (`method`,`url`,`status`,`headers`,`body`,`delay_ms`,`times`) |
| `set_permission` | `permission`, `state: grant\|deny\|revoke\|reset` |
| `set_locale` / `set_density` / `set_theme` / `set_time` | `locale` / `density_dpi` / `theme` / `timestamp` |
| `service_ctl` | `module`, `op` (`attach`/`detach`/a module control op), optional `profile`+`config` (attach), `args` (control arguments), `expect` (a JSON subset the reply must contain — this is what makes the op an assertion) |
| `dev_server` | `op` (`start`\|`edit`\|`wait_serves`\|`fast_refresh`\|`reload`\|`stop`) — drives a run-owned host Metro server for a React Native dev loop (`needs_env: [pasta, metro]`); `start` copies `project`, opens the relay host-loopback opt-in (`expose_to_guest`, default `true`), and waits for `/status`; `edit` edits only the copy; `fast_refresh` (default) waits for Metro to serve the edit for `/hot` delivery; `reload` sends best-effort `POST /reload`; `stop` terminates Metro and deletes the copy (also enforced automatically on every exit path, failure, or timeout) |
| `assert_tree` | `record: <label>` (remember the UI tree node count) or `not_grown_since: <label>` (assert node count `<=` the recorded count so a stale/doubled root fails), optional `timeout_ms`, `settle` |
| `call_activity_method` | only for debuggable, source-owned builds with an injected hook |

There is no `sleep` step and no polling: `wait_for` and the assertions are
event-driven.

**`wait` is only for time-based behaviour, never for synchronisation.** It
exists so a scenario can prove something that depends on the *clock* — the
launcher's `idle_kill_seconds` firing, a timeout expiring — because those cannot
be observed by waiting for a UI change. It asserts nothing and it is not a way to
let the app catch up: waiting for a fixed number of milliseconds before an
assertion is a race (fast host: too early; loaded host: the test is slow and
flaky). To synchronise, use `wait_for` or an assertion's own `timeout_ms`, which
poll until the app really reached the state. Use `wait` only when the thing under
test *is* the passage of time.

**Service modules (`services:`).** A scenario attaches modules at launch with the
same shape as the launch request. A pass that relied on `stub`/`scripted` says so
in the verdict's `notes`, and `verdict.json` carries `modules[]` with each
module's profile and fidelity:

```yaml
services:
  - id: feedback.vibrator     # see acos_capabilities.modules
    profile: stub             # off | stub | scripted | real (omit for the default)
    config: {}                # optional module config
steps:
  - launch: {activity: dev.acos.fixtures.counter/.MainActivity}
  - assert_text: {selector: {id: count}, text: {exact: "0"}}
    requires_services: [feedback.vibrator]   # off -> the step is `unsupported`, never pass
```

A step that declares `requires_services` is `unsupported` (with
`details.module`) when the module is `off`; ACOS never runs an assertion against
behaviour the session does not have. The scenario `services:` block needs
`--engine art`; the Rust VM refuses it.

**Driving a scripted module mid-run (`service_ctl`).** `service_ctl` is the
scenario spelling of `acos_services {op: attach|detach}` and
`acos_service_ctl {module, op, args}` (MCP): it attaches/detaches a module or
applies one of its control ops **between** steps, so a scripted feed can be
changed while the app runs. The reply is the step's evidence (what the module
actually applied), never a pass claim; `expect` (a JSON subset of that reply)
is what turns the control op into an assertion.

```yaml
services:
  - id: sensors
    profile: scripted
steps:
  - launch: {activity: dev.acos.fixtures.sensorprobe/.MainActivity}
  - wait_for: {selector: {id: sensor_count}, timeout_ms: 20000}
  - assert_text: {selector: {id: sensor_count}, text: {exact: "sensors: 7"}}
  - service_ctl:            # a sensor value, delivered as a real sensor event
      module: sensors
      op: set_value
      args: {sensor: accelerometer, values: [1.5, -2.0, 9.81]}
  - wait_for: {selector: {id: accel_seen}, timeout_ms: 15000}   # event-driven wait
  - assert_text: {selector: {id: accel}, text: {exact: "accel: 1.5 -2.0 9.8"}}
  - service_ctl: {module: sensors, op: shake, args: {count: 12, amplitude: 12}}
  - service_ctl: {module: sensors, op: detach}
```

`assert_text` reads **one** tree (it does not poll), so after a `service_ctl`
that only changes app state, wait for something the app makes appear
(`wait_for`) before asserting; `timeout_ms` on an assertion is not a retry.
A `reset` re-applies the scenario's `services:`, by design — to see a module's
`off` state in the app, run a scenario that never attaches it.

### Sensors (`sensors`)

`sensors` serves Android's **native** sensor stack (`android.gui.ISensorServer`
+ `ISensorEventConnection` + a `BitTube` of `ASensorEvent` records), so
`SensorManager`, `SensorEventListener`, `flush()` and `onFlushCompleted` work
exactly as on a phone. Profiles: `off` (the app sees the stock
device-without-sensors list: empty), `stub` (the seven-sensor table with fixed
values) and `scripted` (the agent drives the values). There is **no `real`
profile** — ACOS has no sensor hardware, and a measured accelerometer cannot
exist; every verdict row for it says `fidelity: scripted`.

| control op | arguments | effect |
|---|---|---|
| `status` | — | the sensor table, current values, live connections, counters |
| `set_value` | `{sensor, values: [..]}` | set a sensor's value and deliver it to every enabled listener now |
| `play` | `{sensor, samples: [{t_ms, values}], loop}` | schedule a timeline of events at their own offsets |
| `shake` | `{amplitude?, count?, period_ms?}` | an alternating accelerometer timeline (a shake) |
| `tilt` | `{angle_deg?, axis?, duration_ms?}` | ramp the gravity vector about x or y |
| `enable` / `disable` | `{sensor, period_ns?}` | enable/disable a sensor on every open event connection |
| `events` | `{limit?}` | the delivered events the verdict can cite |
| `reset` | — | clear values, timelines and the event log |

```
acos launch fixtures/build/apps/sensor-probe.apk --engine art --service sensors:scripted --json
acos verify scenarios/sensors-scripted.yaml --engine art --json      # the module's real-app proof
acos verify scenarios/sensors-off.yaml --engine art --json           # the honest off state
```

Honest limits (also in every report): values are agent-chosen, never measured;
delivery is not permission-gated (the table still carries AOSP's
`requiredPermission`, e.g. `ACTIVITY_RECOGNITION` on the step counter); there is
no batching/FIFO (`maxBatchReportLatencyNs` is accepted and ignored); and a
**hot** attach cannot change the sensor list an app already read
(`SystemSensorManager` caches it per process), so attach at launch when the app
must see the sensors.

### Location (`location`)

**Feeding a scripted module (`service_ctl`).** A `scripted` module is driven by
control operations, live, from a scenario step or from `acos_service_ctl`. The
module's `controls` in `acos_capabilities` lists every op and its arguments:

```yaml
services:
  - id: location                    # no GPS in a sandbox: `scripted` is the top profile
    profile: scripted
    config: {permissions: fine}      # the launch config grants the permission
steps:
  - launch: {activity: dev.acos.fixtures.locationprobe/.MainActivity}
  - service_ctl:                     # one module control op, applied to the live session
      module: location
      op: set_location
      args: {lat: 37.4220, lng: -122.0841, accuracy: 4.0, provider: gps}
  - wait_for: {selector: {id: last, text: {contains: "37.4220"}}, timeout_ms: 10000}
  - service_ctl:
      module: location
      op: play_track
      args: {interval_ms: 250, points: [{lat: 37.423, lng: -122.085},
                                        {lat: 37.424, lng: -122.086}]}
  - assert_text: {selector: {id: fix}, text: {contains: "provider=gps"}}
```

`service_ctl` needs a backend with a module registry (`art`); elsewhere the step
is `unsupported`, never a pass. A module that is `off` refuses the op with
`SERVICE_MODULE_REJECTED` naming the attach call. On a live session the same
call is `acos_service_ctl {"module": "location", "op": "set_location", "args":
{...}}` (a single control op) and `acos_services {"op": "list"}` /
`{"op": "attach"|"detach", "id": "location", "profile": "scripted"}` for the
lifecycle. What a module delivered is recorded in the session: `verdict.json` →
`modules[].reason` names the delivered fixes, and `ctl_op: "delivered"` returns
the list itself.

**A live attach/detach is visible through the app's own `LocationManager`.** The
`location` name is published by `core` (the disabled-device answer) and the
module fills that same binder (ADR-0029 note MODSENS), so an app that resolved
`LocationManager` while the module was off still talks to the live module after
`acos_services {"op": "attach", "id": "location", "profile": "scripted"}`,
and a detach answers the honest disabled state again on the same handle. Before
that fix the attach replaced the binder and the cached proxy kept answering the
old node. Full scenario: `results/handoff/MODLOC3/location-live-attach.yaml`.

**`requestLocationUpdates(PendingIntent)`** is supported (PIFILLIN). The
registration keeps the app's `IIntentSender`; every fix fires it with a
*fill-in* intent carrying the `Location` under
`LocationManager.KEY_LOCATION_CHANGED` (`"location"`), and a provider toggle
fires `"providerEnabled"`, exactly like the image's
`LocationPendingIntentTransport`. The system side merges the fill-in into the
stored intent with `Intent.fillIn` (the image's
`PendingIntentRecord.sendInner`: mutable senders merge, `FLAG_IMMUTABLE`
senders ignore it) and delivers the merged activity/broadcast/service intent.
`service_ctl set_location` reports `pending_intent_fires`. Full scenario:
`scenarios/location-pending-intent.yaml`.

A `PendingIntent` a module fires (a notification `tap`, a location fix) is a
real `IIntentSender.send`: a **mutable** sender's fill-in is merged and
delivered, an `FLAG_IMMUTABLE` sender's is ignored and named on the evidence
(`fill_in_ignored: true`).

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

There are four settle rules. Every step's JSON states the rule it used and how it
ended: `settle: {mode, settled, waited_ms, reason}` (`bound`, `window_ms`,
`traversals`, `pixels_changed` and `fallback_from` join it when they apply).

* `settle: "strict"` waits for a *settled* observation — a frame/tree where the
guest reports no pending framework UI work. Timeout is `TIMEOUT`, never a pass.
* `settle: "content"` is the **content-stable** rule: settled when the UI tree's
content signature (text, classes, ids, content-desc, state flags, window and
node structure/order — every field a selector or assertion reads, except
geometry) is unchanged across at least two *fresh, distinct* traversals spanning
at least 300 ms, **whether or not the pixels keep animating**. That is the rule
for a screen that animates forever: a React Native `Animated` timeline, a
spinner, an indefinite `ProgressBar`, a splash that never stops drawing.
* `settle: "fast"` returns the first fresh sample immediately. It is for
  latency-sensitive probing only: it may be mid-animation, it is **never**
  assertion evidence, and `acos_assert`, `wait_for` and every `assert_*` reject it
  with `INVALID_SCENARIO` (scenario) / `INVALID_ARGUMENT` (live channel).
* **No `settle:` at all** is the documented default per step kind:
  * `wait_for` and `assert_text`/`assert_visible`/`assert_not_visible`/`assert_state`
    are **auto**: strict first, then content-stable when strict proves it cannot
    settle. So a text wait/assertion on an animating screen passes on the stable
    tree instead of spending its whole timeout on a rule that can never win, and
    the step's `settle` record says `mode: content` with `fallback_from: strict`,
    `pixels_evidence: false`.
  * `tap`/`type`/`key`/`back`/`swipe`/`launch`/`reset` are **strict** (an action's frame
    is evidence; an endless animation there is a real `TIMEOUT`). Pass
    `settle: content` on the action when you only need the tree to be stable.
    `scroll_until` takes no `settle` option: every attempt is one real gesture
    followed by its own fresh settled traversal.
  * `capture`, `assert_screenshot` and `assert_region` are **pixels**: they take
    their own fresh capture and need pixel settle. They never accept content
    stability as evidence.

Never weakened: an assertion still requires its condition on a *settled*
observation, of the mode it reports.

* **A content pass needs two observations.** Under `settle: content` — explicit
  or the automatic fallback — the condition must hold on **two consecutive
  content-settled observations at least one content window apart** (>= 300 ms)
  before the step passes. One stable-looking sample is not a verdict. A poll
  that is not satisfied (or never settles) resets the confirmation, so an
  intermittent condition cannot slip through on unrelated stable samples.
* **The residual hazard.** Content stability is *not* proof that the app
  finished loading. A stalled JS thread, a paused renderer or a loading state
  that simply has not updated the tree yet can look perfectly stable for a whole
  window. `settle: content` answers "the readable content is not changing", not
  "the app is ready". For a screen whose text arrives in a later phase, wait for
  a **post-transition marker** (the destination screen's unique id/text, or a
  ready flag the app sets) instead of the start button or the spinner, and only
  then assert. Two observations reduce this window; they cannot eliminate it.
* **The rule never leaks.** A content observation is scoped to the step that
  asked for it. `capture`, `assert_screenshot` and `assert_region` always take
  their own strict pixel capture; a capture whose frame is somehow bound by a
  content record is refused with `details.reason: "content_settle_pixel"` rather
  than reported as pixel evidence.

* **Content settle backs UI/text/state claims only.** The content signature does
not hash geometry (a view translated by an animation changes its `bounds` every
frame), so a `bounds` claim — an assertion's own `bounds` **or**
`selector.bounds` — on a content-settled frame fails closed with
`details.reason: "content_settle_geometry"`, and a pixel claim
(`assert_region`/`assert_screenshot`/`capture`) fails with
`"content_settle_pixel"`. Assert the text/state instead, or settle strictly.
* **A `tap` step always runs its own state-change check**
  (`expect_change: "auto"`). The step JSON's `input` block says
  `expect_change: "auto"` for every tap, so this is no longer hidden: when the
  tap's settled frame differs from the previous observation, the step
  additionally requires the change to be visible in **both** the UI tree and the
  screenshot (`state_change` assertion), and fails with `ASSERTION_FAILED` if
  only one moved. A tap that the app did
  not handle (`input.handled: false`) or that changed nothing
  (`state_changed: false`) passes with an `input_delivery` assertion and an
  explanatory message — the auto rule never asks a no-op tap to repaint. Set
  `expect_change: true` when you want "this tap must change something" to be a
  hard requirement; `input.expect_change` then reads `true`.
* **A tap whose effect is transient says so: `expect_change: false`.** Some
  taps really do change the screen, but not by the time the settle ends: a
  control that hides and re-shows itself (Gaten's bubble while its lookup
  runs), or a handler that posts its work to another thread (an accessibility
  `performGlobalAction` that lands as a Back key later). The auto tree rule then
  samples the tree on either side of the transient and can see it unchanged, so
  the step is a sampling race. An explicit `expect_change: false` makes the
  assertion an `input_delivery` claim instead — "the input was delivered, no
  change is claimed" — and the step JSON carries
  `input.expect_change: false` plus `change_claimed: false` in the assertion's
  actual. It is never a silent pass: `true` *and* the default still require the
  change, and the event that really matters is then asserted on its own
  (`assert_log` with `timeout_ms`, or a `wait_for`). Combine it with
  `settle: fast` when the wait for a settle would only burn the budget, as
  `scenarios/a11y-probe.yaml`'s async Back tap does.
* Strict settle does *not* require pixels to change. `expect_change: true` on a
  `tap`/`type`/`key`/`back` step is the opt-in that does: `pixels_changed:false`
  then fails with `ASSERTION_FAILED` / `details.reason: "expect_change"`.
  `expect_change: true` + `settle: fast`/`content` is rejected at parse time. Use
  it for any flow whose point is that a control did something.
* **Unhandled input is a settled observation, not a timeout (REL3H).** When
  *every* event of an action was delivered and acknowledged
  `finishInputEvent(handled) = false` and the app drew nothing, the strict settle
  ends immediately with `settle: {mode: "strict", settled: true, reason:
  "unhandled_input", bound: "unhandled-input", pixels_changed: false}` (the live
  reply and every step JSON carry it; REL3E's `handled: false` hint still names
  the clickable neighbour). Nothing consumed the input, so the unchanged screen
  *is* the outcome - a pass for the delivery rule only: an assertion after it
  still sees the unchanged screen, and `expect_change: true` fails on
  `pixels_changed: false`. If any event was handled, or the ack list is short
  (unknown, never "not handled"), the normal settle applies. The shortcut binds
  only a sample taken *after* the gesture's last ack (so the handler had
  finished) that stays on the same draw for one confirmation window, so a
  handler's posted side effect (`post()` + `setText`) is caught by the normal
  rule instead of settling on the pre-change screen (REL3H2).
* **An unhandled input is never a *silent* pass (REL3H2).** A step that
  passes with `input.handled: false` also carries a structured warning -
  `INPUT_UNHANDLED` in the step JSON's `warnings[]` **and** in the verdict's
  `provenance.warnings[]` - naming the step, the acked events and (when there is
  one) the clickable neighbour. The status stays `pass`: the warning says the
  step made no change claim, not that it failed. Two scenario fields decide what
  you get instead:
  * `expect_handled: true` - a claim: the app must consume this gesture. An
    unhandled ack fails the step with `INPUT_NOT_HANDLED` (error class
    `assertion`, `details.reason: "expect_handled"`, plus the target and the
    settle record). Use it on every tap whose point is that a control reacts; a
    green `pass` then means the control really took the press. If the guest acks
    no complete set of events the claim cannot be verified and the step is
    `unsupported` (`UNSUPPORTED_ASSERTION`), never a silent pass.
  * `expect_handled: false` - the annotation "this press is a no-op on purpose":
    it silences `INPUT_UNHANDLED` and changes nothing else (a handled event is
    still fine).
* A step that provokes no draw at all *and was handled* (or whose acks are
  unknown) cannot settle and ends as `TIMEOUT`
  (`reason: "no-draw-after-action"`). A content settle whose signature keeps
  changing ends as `TIMEOUT` (`reason: "content-not-stable"`). Both are
  non-passes.
* **A settle that timed out is never a pass.** In a scenario the step is
  `status: timeout` with `error.code: TIMEOUT` (error class `infra_timeout`);
  on the live channel (`acos_tap` & co.) the reply is `status: timeout` with
  `details.reason: "settle_timeout"`, `details.settle` and the delivered facts in
  `details.delivery`. `settle_timeout_ms` moves the deadline; it never turns a
  still-changing screen into a pass.
* `animations: off` is *not* the answer for an app that animates in its own code
  (RN `Animated`, a custom `ValueAnimator`, a spinner). It only zeroes Android's
  three framework animation scales. Use `settle: content` for those screens.

**Assertions and `timeout_ms`.** `assert_text`, `assert_visible`,
`assert_not_visible` and `assert_state` honour `timeout_ms`: with it they poll
fresh settled observations until the condition holds or the deadline passes;
without it they evaluate the frame the previous step bound (one observation, no
extra traversal). A scenario that cannot prove its claim must fail.

`settle` on an assertion therefore takes effect **only together with
`timeout_ms`**: without a timeout there is no second observation to make, so the
assertion reads the already-bound frame exactly as before (its `settle` record
reports the rule that frame was bound with, not the one the step named). We keep
that shape because it adds no default latency to the common case; spell out
`timeout_ms` whenever you need `settle: content` (or want the assertion to wait
at all).

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

**Ask the observe verb for the frame you just read.**
`acos_observe_tree {"screenshot": true}` (or `screenshot: "<path>"`) writes a PNG
of the tree's own frame and returns its absolute path, `display_size` and
`display_origin` in the same reply — `png_x = display_x - display_origin.x`, the
same offset the input channel applies. `screenshot: true` lands under the
session's artifacts (`<artifacts>/screenshots/`), so it is reachable from any cwd.
On a placeholder frame (window exists, app content never mounted) the answer is
`NO_FRESH_FRAME`, never a splash PNG presented as the app.

### React Native Metro dev loop (`dev_server`, `assert_tree`)

A scenario can drive a **run-owned** host Metro server alongside a debug React
Native APK (`services: [{id: network, profile: real}]`, `needs_env: [pasta, metro]`):

```yaml
services:
  - id: network
    profile: real
needs_env: [pasta, metro]
steps:
  - dev_server: {op: start, project: fixtures/apps/rn-hello-dev, port: 8081, timeout_ms: 180000}
  - launch: {activity: dev.acos.fixtures.rnhello/.MainActivity, clear_state: true, settle_timeout_ms: 120000}
  - assert_text: {selector: {text: {exact: "Hello ACOS"}}, text: {exact: "Hello ACOS"}, timeout_ms: 120000}
  - assert_tree: {record: before, timeout_ms: 10000}
  - dev_server: {op: edit, file: App.js, find: "'Hello ACOS'", replace: "'Hello Metro'"}
  - dev_server: {op: wait_serves, contains: "Hello Metro", absent: "Hello ACOS", timeout_ms: 120000}
  - dev_server: {op: fast_refresh, timeout_ms: 120000}
  - assert_text: {selector: {text: {exact: "Hello Metro"}}, text: {exact: "Hello Metro"}, timeout_ms: 60000}
  - assert_not_visible: {selector: {text: {exact: "Hello ACOS"}}, timeout_ms: 10000}
  - assert_tree: {not_grown_since: before, timeout_ms: 10000}
  - dev_server: {op: stop}
  - assert_no_crash: {}
```

Rules and lifecycle guarantees:

* **Owned copy, never the source tree.** `op: start` copies `project` into run
  scratch, hard-links `node_modules` (from `node_modules:`, `$ACOS_METRO_NODE_MODULES`,
  or `.cache/rn-0.81.4/jsproject/node_modules`), refuses an already-busy `port`,
  opens the relay's host-loopback opt-in (`expose_to_guest`, default `true` — the
  `adb reverse` equivalent) before the ART guest starts, and waits until
  `GET /status` answers `packager-status:running`. `op: edit` edits only the copy
  via atomic rename and fails if `find` is absent (never a silent no-op) or if
  `file` escapes the project / touches `node_modules`.
* **Fast Refresh is the default dev loop (`op: fast_refresh`).** The update
  reaches the running app over Metro's `/hot` websocket in the same JS context
  without restarting the React root; `assert_tree: {not_grown_since: before}`
  proves the single-root invariant.
* **Full `/reload` (`op: reload`) is best-effort on RN 0.81's legacy architecture.**
  `POST /reload` only reports broadcast delivery; on the legacy architecture,
  fast context recreation can race the old root's unmount and leave a doubled
  tree (`scenarios/metro-reload-full.yaml` keeps strict assertions and is
  excluded from gate matrices).
* **No orphan `node` processes.** Metro runs in its own process group
  (`process_group(0)`). `op: stop`, scenario completion, step failure, timeout,
  and `Drop` all signal the entire process group (`SIGTERM` then `SIGKILL`),
  save the log tail to `dev-server/metro.log`, and delete the scratch copy.
* **Host preflight.** `acos doctor` includes a `metro` check using the same
  rule as `dev_server: start` (Node.js on `PATH` plus a `node_modules` containing
  `react-native/cli.js` and `metro/`). Missing Node or Metro is reported clearly
  as a warning with a `fix:` hint, and `tools/verify/run-matrix.sh` skips
  `needs_env: [metro]` scenarios cleanly rather than failing mysteriously.

<!-- /topic:scenarios -->

<!-- topic:errors -->
## Error codes → what to do

| code(s) | what it means | what to do |
|---|---|---|
| `UNSUPPORTED_ANDROID_API`, `UNSUPPORTED_JAVA_API`, `UNSUPPORTED_BYTECODE`, `UNSUPPORTED_NATIVE_SYMBOL`, `UNSUPPORTED_RELOCATION`, `UNSUPPORTED_BACKEND`, `NEEDS_REAL_DEVICE`, `REQUIRES_GMS`, `WRONG_ABI`, `NO_DISPLAY`, `NO_TEXT` | the engine cannot do this | read `details.api`/`details.at`/`details.symbol`/`details.reason`, avoid the API, or report unsupported. Never rewrite the app to hide it |
| `ASSERTION_FAILED`, `ELEMENT_NOT_FOUND`, `ELEMENT_OFFSCREEN`, `AMBIGUOUS_SELECTOR`, `TREE_TRUNCATED` | the app ran, a check did not hold | read the diff, re-observe the tree, fix the app or the expectation. `AMBIGUOUS_SELECTOR` means several visible nodes match: pick one with `id`/`content_desc`/`bounds` or `index` (`error.details.candidates` lists them) |
| `PLACEHOLDER_CONTENT` | the window exists but the app's own content never mounted (a bare skeleton tree **and** no frame movement: `frame.placeholder: true`, unchanged `frame_number`/`frame_seq`, identical pixels, after the whole `ACOS_ART_FIRST_FRAME_MS` budget) | read `error.details.log_hint` (one quoted log line) and `error.details` (`nodes`, `traversals`, `waited_ms`, `budget_ms`, `frames_advanced`, `pixels_changed`); then run `acos_inspect` and look at `native_lib_check.warnings` (`NATIVE_LIB_MISSING`) — a missing `lib/<abi>/*.so` is a common **hint**, not a proven cause. A longer timeout cannot help (the budget was already spent). Not a pass, and not an app verdict until the library or the JS bundle is fixed |
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
| `DAEMON_VERSION_MISMATCH` | the `acos serve` daemon (or the MCP server's private daemon) is **another build** — an older daemon left over from before an upgrade, or a second checkout. Its answer would be that build's behaviour, not yours | nothing to fix in the app: the CLI never uses that daemon. `acos session …` replaces an idle auto-started daemon and otherwise runs a build-private one (stderr carries `{"code":"DAEMON_VERSION_MISMATCH","client":…,"daemon":…,"action":…}` with both identities, and the tool call that follows runs on *your* build). An operator-started `acos serve` is never signalled, so after an upgrade restart it (`acos doctor` prints its pid and build id next to the CLI's) |

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
* **Software GL (GL ES 3.1) is on, and a HARDWARE bitmap in a `View` is still
  not supported.** The guest ships **ANGLE on SwiftShader's CPU Vulkan ICD**,
  ON by default (`gles: angle-swiftshader` in `acos doctor` and in
  `acos_capabilities`; `gles: none` when the image did not install it, in which
  case every app still launches exactly as before). What that buys you: a real
  EGL context at `OpenGL ES 3.1` inside the guest, so
  `Bitmap.wrapHardwareBuffer` + `Bitmap.copy(ARGB_8888)`,
  `Bitmap.Config.HARDWARE` decodes and GPU readbacks run on the CPU rasteriser
  (SwiftShader), including the 1080×2400 `AccessibilityService.takeScreenshot`
  buffer. What it does **not** buy you: drawing a HARDWARE bitmap into an
  ordinary `View`. ACOS paints windows on a **software** `Surface.lockCanvas`,
  and the framework refuses that draw exactly as a device with software
  rendering does —
  `IllegalArgumentException: Software rendering doesn't support hardware
  bitmaps` (`BaseCanvas.throwIfHwBitmapInSwMode`) inside the looper →
  `LOOPER_THREW`, the process dies. That is the window renderer, not the GL
  provider, and it is unchanged by this build. Workaround that works today:
  read the bitmap back to `ARGB_8888` before handing it to a `View` (and
  `disallowHardwareConfig()` on Glide/Coil, whose default is a HARDWARE
  bitmap). `ACOS_GUEST_GLES=off` disables software GL; `ACOS_GUEST_GLES=<tag>`
  selects another staged provider (`mesa-host` is reserved for the GPU track's
  host-Mesa provider — it is not part of this image, and asking for it when it
  is not staged is a structured error, never a silent fallback). Software GL
  costs a process that never touches EGL **nothing**: the provider is a
  read-only bind and two properties, the driver pages are mapped only when
  libhwui asks for a context, and its file digests are cached per host next to
  the provider (`<sysroot>/gles/<tag>/.verified`), re-hashed only when a file's
  size/mtime/inode moves.
* **Live view (`acos view`) is interactive for normal sessions, watch-only for verify runs, and zero-cost when unused (ADR-0027 + ADR-0033).** A live
  session publishes a registry entry (`$XDG_RUNTIME_DIR/acos/live/`, using
  `device_name` when supplied to `acos_launch` / `acos launch --device-name`)
  and a `stream_socket` for local viewers (half-resolution 0.5× PNG ≤ 15 fps
  while watching only, automatically upgraded to full-resolution 1.0× PNG @ 30 fps
  while a human is interacting). With nobody watching, the frame path does a
  single atomic load and zero encoding. Nothing an agent reads or asserts comes
  from the stream; agent screenshots (`observe` screenshot / `capture_c1`) do
  **not** honour `FLAG_SECURE`, whereas the live stream blanks secure windows.
  To **watch or interact** with a session from a browser or **T3 Code**, run
  `acos view` in a terminal and open the URL it prints:

  ```
  $ acos view
  acos view: http://127.0.0.1:54321/?t=<token>
  ```

  The page lists every live session (display name, agent/client, project, APK,
  activity, age). Clicking a session opens its live screen, Back/Home/Recents/Rotate
  toolbar, tap ripples, and the mixed agent/human action feed. Pointer taps,
  drags, mouse-wheel scrolls, and keyboard typing are sent over
  `/api/input/<session_id>?t=<token>` (opened lazily on first interaction) and
  tagged `source: "panel"` in the event feed alongside `source: "agent"` tool
  calls. Sessions started by `acos verify` advertise `watch_only: true`, show a
  `watch-only: verify run` banner, and refuse all input (`VERIFY_WATCH_ONLY`).
  The server binds `127.0.0.1` only and checks the token and `Host` header on
  every request (treat the URL as a secret); frames stop the moment the last tab
  closes. `acos view --once --json` lists the sessions and exits. In **T3 Code**,
  open that URL in the built-in preview pane (paste it in the pane's address
  bar); no patch or plugin is needed.
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
  keystore is only needed to *build* fixtures (`tools/build/build-dogfood-apps.sh`
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

* **Service modules are opt-in and honest.** A session attaches `core` (the
  service table every app needs) plus whatever `services:`/`--service` names.
  Any name a module does not serve is either a tombstone that answers
  `UNSUPPORTED_ANDROID_API` naming the module and the attach call, or Android's
  real missing path (`null`) where AOSP builds working behaviour out of it — see
  the generated table below. A module at `stub`/`scripted` is a documented
  stand-in, never a real implementation: the launch reply, `acos_services` and
  `verdict.json` all state its profile and fidelity. Only `hot` modules can be
  attached/detached on a live session; an `at-launch` module (e.g. `core`) is
  refused with the launch call in the message.
  `acos_capabilities {session_id}` shows one session's attached set.

<!-- modules:generated -->

<!-- /topic:limits -->
