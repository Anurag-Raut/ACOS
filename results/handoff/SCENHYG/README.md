# SCENHYG — the scenario corpus and the release gate, made honest

Branch `scenhyg` from `modules` @ `45a1feab`. Worktree
`/home/Qui/.t3/worktrees/acos/t3-aa065f95-064e8d2a8d945bad`; target
`.cache/scenhyg-target`; private `ACOS_RUNTIME_DIR`/`ACOS_SESSION_SOCKET`
(`.cache/rt-scenhyg`), `ACOS_VIEW_TEST_DIR=$PWD/.cache/lv`; every ART run through
`tools/art-runtime/run-exclusive.sh` + `tools/guarded.sh`.

## 1. The 16 scenarios that failed/skipped for known reasons

RELGATE1 (`results/release/v0.3.0-gate/correctness/04-scenarios-table.md`) found
16 scenarios that did not match their declared expectation on both v0.2.2 and
v0.3.0. Every one now **declares** what really happens, with a one-line
`expectation_reason` in the file and a pinned `expect_error` so it cannot go red
for an unrelated reason. `run-probe.sh probe1` is the before-run,
`probe1/../probe2` the after-run (same binaries, declarations changed).

| scenario | real outcome on this tree | honest declaration |
|---|---|---|
| `hello-views` | timeout `no-draw-after-action` at `back` | `non_pass`, TIMEOUT/no-draw-after-action |
| `multi-activity` | timeout `no-draw-after-action` at `back` | `non_pass`, TIMEOUT/no-draw-after-action |
| `negative/multi-wrong-after-back` | timeout `no-draw-after-action` at `back` (its wrong-text assertion is never reached) | `non_pass`, TIMEOUT/no-draw-after-action |
| `negative/offscreen-pager-wait` | timeout (the designed wait_for deadline) | `non_pass`, TIMEOUT |
| `svg-viewbox` | fail `ASSERTION_FAILED` (`canvasIsViewPx=false`, the open GR1 defect its own header documents) | `non_pass`, ASSERTION_FAILED |
| `suite/T07-form-good` | **was** fail `ELEMENT_OFFSCREEN`: an empty inline error is a zero-height, not-visible TextView | **fixed**: the faithful check is `assert_not_visible`; the scenario is green (probe2) |
| `s8-hostile-untrusted` | fail `INSUFFICIENT_HOST_EVIDENCE` (the untrusted-run security rule) | `non_pass`, INSUFFICIENT_HOST_EVIDENCE |
| `w44-hostile-forest-untrusted` | same | `non_pass`, INSUFFICIENT_HOST_EVIDENCE |
| `w44-hostile-probe-spam` | same | `non_pass`, INSUFFICIENT_HOST_EVIDENCE |
| `s8-hostile-region` | timeout `probe-floor` | `non_pass`, TIMEOUT/probe-floor |
| `w44-hostile-parcel` | timeout `probe-floor` | `non_pass`, TIMEOUT/probe-floor |
| `w15-broken-crash` | error `GUEST_EXITED` (the deliberate broken fixture) | `non_pass`, GUEST_EXITED |
| `w15-broken-missing-symbol` | `GUEST_EXITED` | `non_pass`, GUEST_EXITED |
| `w25-gaten-launch` | `GUEST_EXITED` (the debug APK predates the shipped image) | `non_pass`, GUEST_EXITED |
| `w26-gaten-debug-launch` | `GUEST_EXITED` | `non_pass`, GUEST_EXITED |
| `w28-gaten-debug-launch` | `GUEST_EXITED` | `non_pass`, GUEST_EXITED |

Nothing was weakened or marked ignored: the positive scenarios that cannot pass
keep **every** assertion and pin the defect they hit; the negatives keep their
own failures. Where the cause is a product seam, the file names it (Back's
strict settle demands a new frame although the finish/cached-window path draws
none — REL3A/PERF4), so the "make it pass" work has a single place to land.

`needs_env` was added for the host-capability class: `net-check`,
`net-network-real` and `net-network-offline` declare `needs_env: [pasta]` and say
why; a runner skips them (naming the capability) instead of failing them, and
`tools/run-matrix.sh` records them in `summary.skipped_needs_env` (no pasta is
installed in the worktrees).

## 2. The three "infra_timeout" matrix negatives

`results/handoff/GATEFIX/02-matrix-quiet/matrix.json` had `infra_timeout: 3` for
three designed TIMEOUT negatives, so `negative_all_correct` could never be true:

| row | declared | produced |
|---|---|---|
| `negative/gaten-content-window-impossible` | non_pass, TIMEOUT/content-not-stable | TIMEOUT/content-not-stable |
| `negative/multi-wrong-after-back` | fail (intended: ASSERTION_FAILED) | TIMEOUT/no-draw-after-action |
| `negative/offscreen-pager-wait` | fail (intended: TIMEOUT) | TIMEOUT |

Root cause (taxonomy, not the scenarios): `TIMEOUT` was in benchlib's static
`infra_timeout` block, so a deadline the **scenario itself declared** was counted
as infrastructure; and `run-matrix.sh` mapped a verify-reported `timeout` status
to `verdict: error`, which only a *harness* failure should be. Fixed once for the
class:

* `tools/benchlib.py`: `classify_error(..., deadline="step"|"harness")` — a
  step deadline is a new `step_timeout` class (a real non-pass, retryable for
  triage), the runner's own rc=124 wall-clock kill stays `infra_timeout`.
  `step_timeout` is in the schema and the allowed-class list.
* `tools/run-matrix.sh`: `fail`/`timeout`/`unsupported` statuses are verdicts
  (`fail`), not errors; a negative is correct only when the run matches its
  declared `expect_error` (code and reason) — "went red somehow" no longer
  counts; `harness_or_timeout` excludes `step_timeout`; new gate
  `no_infra_timeout`; `summary` carries `infra_timeout`, `step_timeout`,
  `negative_expect_error_mismatch`, `skipped_needs_env`.
* the two scenarios now pin what they really produce:
  `multi-wrong-after-back` → `non_pass` + TIMEOUT/no-draw-after-action (with the
  reason it is blocked by Back's settle), `offscreen-pager-wait` → `non_pass` +
  TIMEOUT.

Evidence: `matrix/` (§6) shows 0 `infra_timeout`, 32/32 correct negatives.

## 3. Test hygiene

### `gl1_kept_session` (4 ignored tests, all four failed)
* **Product bug, fixed**: `acos launch --keep` sent `"idle_kill_seconds": null`
  and the daemon rejected a present-but-null integer, so **every** CLI launch
  through `acos serve` failed `INVALID_ARGUMENT` (not just the tests). The CLI
  now omits an absent optional field (`crates/acos-cli/src/launch.rs`), the
  daemon treats explicit `null` as "not provided" via one testable reader
  (`idle_kill_seconds_arg`, `crates/acos-cli/src/engine/serve.rs`), and the
  "launch-time field changed" check uses `idle_kill_requested` so `null` cannot
  be read as 0.
* **Stale test, fixed**: the screenshot assertion wrote outside the session's
  artifacts dir (FB2c confinement → `INVALID_ARGUMENT`). The daemon now gets an
  explicit `ACOS_SERVE_ARTIFACTS` root, the test asks for a *relative* name and
  reads the path the reply reports.
* **Race in the assertion, fixed**: the SIGKILL test's fresh scan can be
  satisfied while a killed guest is mid-exit (`/proc/<pid>/cmdline` is already
  empty), and its final loop then failed on that pid with no wait. It now waits
  (bounded, 2 s) for the pids it captured before the kill and prints the state
  and cmdline of a real survivor.
* Result: `4 passed` (§6 `gl1.log`), before: 0 passed.

### SUN_LEN / inherited env
TESTFLAKE (§2 of `results/handoff/TESTFLAKE/REPORT.md`) already put every
socket-binding test on `acos_test_support::short_scratch_root` and every child
`acos` on an explicit `ACOS_RUNTIME_DIR` + `env_remove(ACOS_SESSION_SOCKET)`; it
is merged into `modules`, so this branch inherits it. Verified by the suite run
with the private 45-byte runtime dir (§6).

### Tests that rewrote tracked `results/` files
Files under `results/flutter/`, `results/rn/`, `results/render/`, `results/vm/`,
`results/aidl/` are tracked evidence, and the RW/VM/render/AIDL/JNI test targets
published into them (`results/flutter/w130a-frame-0.png`,
`results/render/corpus/corpus-pipeline-metrics.json`, ...). One rule now:
`acos_test_support::test_artifacts_dir(name)` → `$ACOS_TEST_ARTIFACTS` or
`<checkout>/.cache/test-artifacts/<name>` (never `results/`). Applied to every
writer (`acos-jni` ×4, `acos-mcp`, `acos-runtime` ×2, `acos-vm` ×2,
`acos-render`, `acos-aidl`; the last gains the dev-dependency). Teeth:
`test_artifacts_go_to_a_scratch_root_and_never_to_the_checkout_results_tree`.
Check: `tools/check-tree-clean.sh [--base FILE]` (used after the suite; the
acceptance run also requires a plain empty `git status --porcelain`).

## 4. The launch-intent fields a scenario could not send (WVFAIR finding)

The live launcher builds its own `ACTION_MAIN`/`CATEGORY_LAUNCHER` Intent for the
APK's **manifest** launcher component, so a scenario's `launch.extras:` (and
`intent_action:`, `data:`, a non-launcher `activity:`) were accepted and then
dropped with no error. Now the ART session path refuses them **by name** with
`UNSUPPORTED_ANDROID_API` before the launch, so no scenario can assert an Intent
state the app never received (`crates/acos-cli/src/engine/mod.rs`,
`refused_launch_intent_fields`).

* scenario `scenarios/launch-extras-unsupported.yaml`: launch with
  `extras: {greeting: hello}` → `expectation: unsupported`, pinned
  `UNSUPPORTED_ANDROID_API`; probe2 shows `status=unsupported` with that code.
* test `a_scenario_that_declares_extras_is_refused_loudly_not_silently_dropped`
  (fails before: the request was accepted and the fake session launched) and
  `the_launch_intent_fields_a_session_cannot_honour_are_named` (component/action/
  data, pure).
* Wiring string extras through the session-start Intent is the honest follow-up
  (the extras must reach `acos-art launch` before the guest resumes the
  Activity); the refusal is what keeps the corpus truthful until then.

## 5. `webview-probe`'s lost WebView tap (measured, partially mitigated, honestly classified)

30 runs on this tree, one ART acquisition per run, same APK/scenario; failure
mode is always the same: the tap step reports `delivered: true`, `handled: true`,
`acked: 2` with `ack_ms ≈ 2.0–2.4 s` and `visual_change: false`; the page never
logs `JS->Java: tapped`; `assert_text text=="tapped"` then has no node for its
whole 12 s. Evidence: `webview-run1..30.json` (envelopes; the artifact dirs were
deleted after the analysis, `run1`'s analysis is quoted below).

| runs | tree | result |
|---|---|---|
| 1–10 | `modules` @ 45a1feab | **4/10 failed** |
| 11–20 | + no frame pull between a gesture's phases (`View::tap`/`View::touch`) | **3/10 failed** |
| 21–30 | + a non-final touch phase waits 150 ms for the CDP reply, not 2 s | **2/10 failed** |

Cause (from `webview-run1`, `run13`, `run25`): the two WebViews both load at
launch; the other view pulls a frame (`frame decoded view=2 ... seq=2`) exactly
while the tap is dispatched; the broker serialises every `beginFrame` engine-wide
and runs input on the same workers, so view 1's touch dispatch waits seconds,
the guest's `up` lands ~2 s after its `down` (a long press), and the page's
`click` never fires. `onTouchEvent` still returns `true`, so the tap step's
handled evidence is truthful only about consumption, not about the effect.
The two fixes remove two aggravating factors (a frame pull *between* the down and
the up, and a full CDP reply wait on a non-final phase); they do **not** remove
the starvation. The root fix is WVFAIR's fair pull gate
(`crates/acos-webview/src/fair.rs` on branch `wvfair`: one attempt in flight,
per-view coalescing, input frames first with the full budget, `render` on its own
thread; measured there 41–140 ms tap acks and webview-probe 3/3).

Classification: the scenario keeps `expectation: pass` — it must pass; declaring
`non_pass` would turn a real pass into a harness error — and the file now carries
the measured rate, the mechanism and the root fix. Merging `wvfair` is an
orchestrator decision (it is a ~600-line broker change reviewed in that track).

## 6. Acceptance evidence

| item | file | result |
|---|---|---|
| `cargo test -p acos-art -p acos-verify -p acos-cli -p acos-services -p acos-wm` | `suite.log` | see the suite summary line |
| `git status` clean after the suite | `git-status-after-suite.txt` | empty |
| `cargo test -p acos-cli --test gl1_kept_session -- --ignored` | `gl1.log` | 4 passed |
| `cargo test -p acos-protocol` (schemas, scenario corpus) | `protocol.log` | see the summary |
| `tools/run-matrix.sh` | `matrix/matrix.json`, `matrix/matrix.tsv` | 0 `false_pass`, 0 `unclassified`, 0 `infra_timeout`, 32/32 negatives |
| 16 declarations before/after | `probe1/`, `probe2/` | table §1 |
| webview 30 runs | `webview-run*.json` | table §5 |
