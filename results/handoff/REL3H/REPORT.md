# REL3H — unhandled input settles, `scroll_until` knows the end of the list

Branch `rel3h` (base `rel-030` at `064f568d`: REL3A + fixes, REL3E, REL3S, REL3K merged).
Last semantic item of v0.3.0. Evidence in this directory.

**Merge note.** `rel-030` advanced while this branch was being written (now
`e4fb2424`, +PERF3/REL3D/REL3K2, 115 files). The only overlap with this branch is
`crates/acos-protocol/src/error.rs` (REL3K2 added `DAEMON_VERSION_MISMATCH` in
the *harness* section; `END_OF_LIST` is added in the *assertions* section) and
therefore the four generated `crates/acos-protocol/schema/*.json` files, which
both branches regenerated. Merge those by hand and re-run
`UPDATE_SCHEMA=1 cargo test -p acos-protocol --test schemas`. `launch.rs`,
`acos-verify/src/backend.rs`, `acos-cli/src/engine/mod.rs`, the tests, the
scenarios and the docs are untouched by the new `rel-030` commits. The
fail-before citations below name `064f568d` (this branch's base), not the moving
branch name.

## What was wrong

| # | Symptom | Cause |
|---|---|---|
| 1 | A tap/key/text nothing consumed was a `TIMEOUT` (`reason: no-draw-after-action`) | the strict settle waits for a draw, and "the app did not handle this" (REL3E's own `finishInputEvent(handled)=false` ack, already on the wire) had no representation. An agent got a red status for a delivered no-op on an honest, stable screen, and paid the whole settle deadline for it |
| 2 | `scroll_until` burned every remaining attempt at the end of a list | the search only knew "found" and "not found if the attempts run out". A gesture that moved nothing (the list is at its end) was indistinguishable from a slow search, so the remaining gestures were repeated and the answer was `ELEMENT_NOT_FOUND` — which an agent cannot tell from "this selector never renders" |
| 3 | the three REL3A scenarios emitted `SCENARIO_ENGINE_MISMATCH` | they still declared the legacy `engine: native-preferred`, and v0.3.0's provenance warning correctly says a `/rust-vm/`-pinned document ran on ART |

## The fix (one source each)

### 1. Unhandled input is a settled observation (guest + both front doors, one rule)

`crates/acos-art/src/launch.rs`:

* `every_event_acked_unhandled(seqs, acks)` — true only when **every** published
  `InputMessage` came back `FINISHED` (`acks.len() == seqs.len()`, each ack names
  a sequence this gesture published) **and** every one of them is
  `handled == false`. A short/missing ack is *unknown* (REL3E's rule), never
  "not handled"; any handled event means the app consumed the gesture.
* The flag is computed once at publish (`mode == strict && every_event_acked_unhandled`)
  and handed to `wait_for_settled_observation` → `wait_for_event_settled`.
* In the strict loop, the *existing* no-draw branch
  (`sample.draw_ms < publish_epoch_ms`, previously `reason: no-draw-after-action`)
  now returns `SettleEnd::Settled` when the flag holds:
  `bound: "unhandled-input"`, `strict_rule: "event"`,
  `reason: SETTLE_UNHANDLED_INPUT` (`"unhandled_input"`), `settled: true`.
  Nothing else changes: any draw, or any `handled=true`, takes the normal rule.
* `pixels_changed` is **measured**, not assumed: newest C1 capture against the
  caller's pre-action capture, or — when the caller has none (a `pixels:false`
  launch writes no capture) — against the newest C1 capture hash taken at
  publish (`entry_fnv`). That is what `expect_change: true` judges, so an
  unhandled tap fails that claim on a measured `false` instead of passing.
* The reply/step records are the REL3A ones (the guest's `settle` block flows
  into `ArtSession::last_input_settle` → `settle_record`), so the live reply and
  the scenario step JSON both carry
  `settle: {mode: "strict", settled: true, reason: "unhandled_input", bound:
  "unhandled-input", pixels_changed: false}`. `input_settle_timeout()` sees
  `settled: true`, so neither `serve::settle_timeout_envelope` nor the
  executor's `action_result` turns it into a timeout any more.
* REL3E's `handled_hint` (naming the clickable neighbour) is untouched: it is
  built from `target` + `handled` + `state_changed`, which are unchanged.

`expect_change` / state assertions still evaluate and fail honestly — the screen
did not change. Live: `ASSERTION_FAILED` / `details.reason: "expect_change"`
(`check_expect_change`). Verify: `ASSERTION_FAILED` / `reason: "expect_change"`
(`expect_change_failure`, judged on the same record).

### 2. `scroll_until` stops at the end of the list

* `acos-protocol`: new `ErrorCode::EndOfList` (`END_OF_LIST`, error class
  `assertion`, status `fail`) + hint + `ALL_ERROR_CODES` + schema regeneration
  (`envelope.json`, `protocol-error.json`, `verdict.json`,
  `artifacts-validation.json`) + one `docs/ERROR_CODES.md` row.
* `acos-verify::backend`: three shared helpers so both search implementations
  report the same facts — `scroll_until_seen_texts`, `scroll_until_end_of_list`
  (`details: {end_of_list: true, attempts, max_attempts, last_frame_seq,
  selector, last_tree_texts}`), `scroll_until_not_found` (same fields,
  `end_of_list: false`, code `ELEMENT_NOT_FOUND`), and
  `scroll_until_not_settled` (a traversal the backend says did not settle may
  never back a "found": `TIMEOUT`, `reason: settle_timeout`).
* Both search loops (the ART `SessionBackend` default and the verifier's
  `Backend` default) take a **settled baseline tree before the first gesture**
  (so the first swipe, which may already be at the end, is measurable), compare
  the settled tree's nodes (geometry included — a scroll changes bounds) after
  each swipe, and stop with `END_OF_LIST` when the gesture moved nothing. A
  found node still only ever comes from a settled traversal; a timed-out
  traversal is a `TIMEOUT`, never a hit.
* The executor's `scroll_until` step now carries
  `input.until {selector, found: false, end_of_list, attempts, max_attempts,
  frame_seq, last_tree_texts}` for **both** miss codes, so a step JSON says which
  one it was.
* Live `acos_swipe until` propagates the same structured error (its
  `until.found: true` block is only written on success).

### 3. REL3A scenarios declare the engine they run on

`scenarios/rel3a-content-{settle,pixel,sticky}.yaml`: `engine: native-preferred`
→ `engine: art`. The verdicts' `provenance.warnings` are now `[]` (they carried
`SCENARIO_ENGINE_MISMATCH`).

New scenarios: `scenarios/rel3h-unhandled-input.yaml` (pass) and
`scenarios/rel3h-unhandled-input-expect-change.yaml` (negative twin, `fail`).

## Acceptance criteria

| # | Criterion | Result | Evidence |
|---|---|---|---|
| 1 | an unhandled tap/key/text settles immediately as `settle: {mode: strict, settled: true, reason: "unhandled_input"}`, live **and** verify | PASS | `verify-unhandled/verdict.json` step 1: `status: pass`, `settle.mode: strict`, `settled: true`, `reason: unhandled_input`, `bound: unhandled-input`, `pixels_changed: false`, `waited_ms: 81`, `input.handled: false`; live: `rel3e-handled.log` (`settle.reason: unhandled_input`, `settled: true`, `status: pass`) |
| 2 | only when ALL events were delivered+acked unhandled; unknown/short acks keep today's behaviour | PASS | `crates/acos-art/src/launch.rs::rel3h_unhandled_input_tests::every_published_event_must_be_acked_unhandled` (short ack, empty, foreign seq, one handled ⇒ not unhandled); the normal path is byte-for-byte unchanged (`strict-event` in `verify-unhandled` step 2) |
| 3 | any draw or any `handled=true` goes through the normal settle | PASS | `verify-unhandled/verdict.json` step 2 (`inc`): `handled: true`, `settle: strict-event`, `pixels_changed: true`; the branch is only reachable with `draw_ms < publish && settled` |
| 4 | `expect_change` / state assertions still fail honestly | PASS | `verify-expect-change/verdict.json` step 1 `status: fail`, `ASSERTION_FAILED`, `details.reason: expect_change`; live variant in `rel3e-handled.log` |
| 5 | the REL3E tap-targets scenario passes without `settle: content` on the label tap | PASS | `scenarios/rel3h-unhandled-input.yaml` → `verify-unhandled.log` (`verify: pass`), 4/4 steps |
| 6 | `scroll_until` at the end stops with `END_OF_LIST` (attempts used, last tree texts), fewer attempts than max, live | PASS | `rel3s-swipe.log` (1 passed, 17.94 s): `error.code: END_OF_LIST`, `end_of_list: true`, `1 <= attempts < 12`, `last_tree_texts` contains `Row 99` |
| 7 | the same for a scenario `scroll_until` step (`input.until`) | PASS | `rel3s-scenario.log` (2 passed, 60.86 s) — `scroll_until_stops_at_the_end_of_the_list`: step `status: fail`, code `END_OF_LIST`, `input.until.end_of_list: true`, `found: false`, `attempts < 12` |
| 8 | never report found unless the node is in a settled observation | PASS | both loops refuse a `settled:false` traversal (`scroll_until_not_settled` / `settle_timeout_error`), before the hit test; unit test `backend::tests::a_visible_node_is_found_in_the_first_settled_observation` |
| 9 | the three REL3A scenarios declare `engine: art` and stop emitting `SCENARIO_ENGINE_MISMATCH` | PASS | `rel3a-settle.log` / `rel3a-pixel.log` / `rel3a-sticky.log`; each verdict's `provenance.warnings` is `[]` (checked in `rel3a-*/verdict.json`) |
| 10 | tests fail before / pass after | PASS (see “Fail-before”) | unit tests + both ART tests + the two verify scenarios |

### Fail-before (why each test could not pass on `rel-030`)

* `git show 064f568d:crates/acos-cli/tests/rel3e_handled.rs` asserts the label tap
  is `status: timeout` / `error.code: TIMEOUT` (line 228-229 there). The same tap
  on this branch is `status: pass` with `settle.reason: unhandled_input`; the
  test now asserts the new contract and adds the `expect_change: true` twin.
* `git show 064f568d:crates/acos-cli/tests/rel3s_swipe.rs` asserts a miss is
  `ELEMENT_NOT_FOUND` with `attempts == max_attempts` (lines 375-380 there).
  `END_OF_LIST` did not exist as a code, and no loop compared trees, so
  `scroll_until_stops_at_the_end_of_the_list` (and the live block) could not
  pass.
* `git show 064f568d:scenarios/rel3a-content-settle.yaml | grep '^engine:'`
  → `engine: native-preferred`; running it emitted
  `SCENARIO_ENGINE_MISMATCH` (REL3A's own verdict, warning list non-empty).
* The new unit tests reference `every_event_acked_unhandled` /
  `ErrorCode::EndOfList` / the shared miss builders, i.e. behaviour that did not
  exist before (they cannot compile against `064f568d`).

## Test commands actually run

Host env: `ACOS_FONT_DIR=/home/Qui/Projects/acos/.cache/boot/android-34/fonts`,
`CARGO_TARGET_DIR=$PWD/.cache/rel3h-target`,
`CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_INCREMENTAL=0`,
`ACOS_ART_BIN=$PWD/.cache/rel3h-target/release/acos-art`.

```
cargo check  --offline --workspace --all-targets                       # green
cargo test   --offline -p acos-protocol -p acos-verify                 # green: 85 + 3 + 7 + 8 + 9 + 2
                                                                        #   protocol, 104 verify lib, 34 verify scenarios
cargo test   --offline -p acos-art --lib                               # 150 pass, 8 fail (worktree path-length
                                                                        #   unix sockets in live_view::tests; env, not code)
cargo test   --offline -p acos-mcp --lib                               # 86 pass
ACOS_BIN=<release acos> cargo test -p acos-mcp --test guide --test docs # 1 + 12 pass
cargo test   --offline -p acos-cli --bins                              # 188 pass, 2 fail (view::tests SUN_LEN,
                                                                        #   identical on rel-030)
```

Real ART (release build, inside `run-exclusive.sh --task REL3H` +
`guarded.sh --mem 10G`):

```
acos verify scenarios/rel3h-unhandled-input.yaml              --engine art   → pass   (verify-unhandled/)
acos verify scenarios/rel3h-unhandled-input-expect-change.yaml --engine art  → fail, ASSERTION_FAILED/expect_change
acos verify scenarios/rel3a-content-settle.yaml               --engine art   → pass   (warnings [])
acos verify scenarios/rel3a-content-pixel.yaml                --engine art   → fail   content_settle_pixel (warnings [])
acos verify scenarios/rel3a-content-sticky.yaml               --engine art   → timeout, the scenario's own
                                                                                non-pass expectation (warnings [])
cargo test -p acos-cli --release --test rel3e_handled -- --ignored    → 1 passed (11.86 s)
cargo test -p acos-cli --release --test rel3s_swipe   -- --ignored    → 1 passed (17.94 s)
cargo test -p acos-cli --release --test rel3s_scenario -- --ignored   → 2 passed (60.86 s)
```

`rel3a-content-sticky` is `expectation: non_pass` and ends in the intended
non-pass (its last step is a pixel claim on a forever-animating screen: here the
strict capture times out, which is the same non-pass the REL3A fix produced);
`rel3a-content-pixel` ends `fail` with `details.reason: content_settle_pixel`.

## Files

* `crates/acos-art/src/launch.rs` — `every_event_acked_unhandled`,
  `SETTLE_UNHANDLED_INPUT`, the no-draw branch, `entry_fnv`, unit tests
* `crates/acos-protocol/src/error.rs` (+ 4 regenerated schemas) — `END_OF_LIST`
* `crates/acos-verify/src/backend.rs` — shared `scroll_until` helpers + both
  default loops' end-of-list / settled-traversal rules, unit tests
* `crates/acos-verify/src/executor.rs` — `input.until` for both miss codes
* `crates/acos-cli/src/engine/mod.rs` — ART `scroll_until` end-of-list rule
* `crates/acos-cli/tests/rel3e_handled.rs`, `rel3s_swipe.rs`, `rel3s_scenario.rs`
* `scenarios/rel3h-unhandled-input{,-expect-change}.yaml`,
  `scenarios/rel3a-content-*.yaml` (`engine: art`)
* `docs/AGENT_GUIDE.md`, `docs/MCP.md`, `docs/AGENT_API.md`, `docs/ERROR_CODES.md`

## Left open / for the orchestrator

1. `acos-cli --bins`: 2 pre-existing `view::tests` failures (`path must be
   shorter than SUN_LEN`) and `acos-art --lib`: 8 pre-existing
   `live_view::tests` failures — both the deep-worktree socket path, identical on
   `rel-030`, files untouched here.
2. The unhandled-input rule is implemented on the ART engine only (the frozen
   Rust VM has no `finishInputEvent` ack path to base it on; it keeps the old
   behaviour). MCP session tools default to ART, so the agent-facing surface has
   it.
3. `scroll_until`'s end-of-list stop relies on the settled tree being *identical*
   after a gesture that moved nothing. A list that keeps redrawing without moving
   (a counter in the header) would not stop early — the search then ends with
   `ELEMENT_NOT_FOUND` after its attempts, which is still honest.
4. **Merge work for the orchestrator:** `rel-030` is now `e4fb2424`; resolve the
   additive `error.rs` conflict and regenerate the four protocol schemas
   (`UPDATE_SCHEMA=1 cargo test -p acos-protocol --test schemas`), otherwise the
   schema test fails on the merged tree.

Peak RAM observed: ~9 GiB during the release build (guarded 12G), ~1.2 GiB for
one ART guest run. Disk: `/` 82% before, 84% during the release build; this
worktree's `.cache/rel3h-target` was ~2.4 GiB and is deleted after the merge
step.
