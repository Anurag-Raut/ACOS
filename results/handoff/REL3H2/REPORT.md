# REL3H2 — review fixes for REL3H (false-pass class)

Branch `rel3h2` (base `rel3h` @ `087f760b`), merged with `rel-030` @ `7168439c`.
Evidence in this directory.

## What the review found, and what changed

| # | Finding | Fix |
|---|---|---|
| 1 (medium) | the unhandled-input shortcut could bind a probe sample taken after the publish but **before** the UI thread ran the handler (or before its posted side effect ran and drew) — a false "nothing happened" that `expect_change: true` then failed, and the next assertion described a state the app had already left | `UnhandledSettleGate` (`crates/acos-art/src/launch.rs`): the bound sample must be **strictly newer than the gesture's last ack** (the caller records the newest probe sequence once every ack is in) **and** its `draw_seq` must stay newest for one confirmation window (`ACOS_STRICT_SETTLE_CONFIRM_MS`, the strict rule's own window). A side effect that draws inside the window restarts the wait, so the gesture falls through to the normal rule and captures the change |
| 2 (medium) | an unhandled step was a **silent plain pass** | step-level **and** verdict-level `INPUT_UNHANDLED` (`StepResult.warnings[]` + `provenance.warnings[]`, one record), `expect_handled: false` suppresses it, `expect_handled: true` turns an unhandled ack into `INPUT_NOT_HANDLED` (class `assertion`); an unverifiable claim (no complete ack) is `UNSUPPORTED_ASSERTION`, never a pass. Documented in `acos guide scenarios` (`docs/AGENT_GUIDE.md`), `docs/ERROR_CODES.md`, `docs/AGENT_API.md` |
| 3 (medium) | `END_OF_LIST` was inferred from **one** unchanged tree, even when the swipe was never consumed | `acos_verify::backend`: `scroll_until_unconsumed_reason`, `scroll_until_gesture_not_consumed` (`ELEMENT_NOT_FOUND`, hint "gesture not consumed (swipe missed the scroller?)"), `ScrollUntilMovement` (two **consumed** unchanged gestures), softened END_OF_LIST hint + `unchanged_gestures`/`gesture_consumed` in `details` |
| 4 (low) | the ART engine treated a traversal with **no** settle record as settled | `SessionBackend::tree_settle_record` + `scroll_until_no_settle_record` (`TIMEOUT`, `reason: no_settle_record`) — fail closed |

Integration with `rel-030`'s one-step-one-entry rule: the `scroll_until` stop
reason travels in **`attempts[]`** (the final attempt's `reason` starts with
`end_of_list:` or `gesture not consumed …`), not in a second structure;
`input.until` stays the summary it was (`selector`, `found`, `end_of_list`,
`attempts`, `max_attempts`, `frame_seq`, `last_tree_texts`). Machine facts
(`end_of_list`, `gesture_consumed`, `unchanged_gestures`) live in
`error.details`.

Also fixed while there (same false-evidence class, found by writing the tests):
`expect_change_failure` dropped the step's `InputDelivery` and settle record, so
a *failing* `expect_change` input step reported `input.handled: null`,
`published: 0` even though the acks existed. REL3E's rule ("a failing input step
still says what the app did with the gesture") now holds for that path too.

New scenarios: `rel3h2-expect-handled-false.yaml` (warning-free pass with
`expect_handled: false` + a positive `expect_handled: true`),
`rel3h2-expect-handled-true.yaml` (negative twin, `expect_error: {code:
INPUT_NOT_HANDLED, reason: expect_handled}`).

## Acceptance criteria

| # | Criterion | Result | Evidence |
|---|---|---|---|
| 1 | the REL3H rows still pass: unhandled input settles, `expect_change` still fails honestly, `scroll_until` end-of-list, REL3A `engine: art` with no warning | PASS | `rel3h-unhandled-input/verdict.json` `pass` (+ one `INPUT_UNHANDLED`); `rel3h-unhandled-input-expect-change/verdict.json` `fail` `ASSERTION_FAILED`/`expect_change`; `rel3a-content-settle/verdict.json` `pass`, warnings `[]`; `rel3a-content-pixel` `fail` `content_settle_pixel`; `rel3a-content-sticky` `timeout` (its own `expectation: non_pass`), warnings `[]` |
| 2 | the live/verify ART tests: REL3E handled, REL3S swipe/scroll, REL3S scenario end-of-list | PASS | `rel3-art-tests.log`: `rel3e_handled` 1 passed; `rel3s_scenario` 3 passed (incl. the new unconsumed-swipe test); `rel3s_swipe` 1 passed |
| 3 | finding 1: sample must be after the last ack, then one confirmation window; a sample that precedes the handler's side effect must not settle via the shortcut | PASS (rule level) | `crates/acos-art/src/launch.rs::rel3h_unhandled_input_tests::gate`: `a_sample_at_or_before_the_last_ack_never_binds`, `a_sample_after_the_ack_binds_only_after_the_window`, `a_draw_inside_the_window_restarts_it_instead_of_binding`, `a_reset_forgets_the_window`. The end-to-end fixture is documented as blocked in `EXPERIMENTS.md` (ART guest does not dispatch to a non-clickable `OnTouchListener`), so no ART test is claimed for it |
| 4 | finding 2: `INPUT_UNHANDLED` in step + provenance, CLI and MCP identical, `expect_handled` both ways, documented in the guide | PASS | `rel3h-unhandled-input/verdict.json`: step 1 `warnings: [INPUT_UNHANDLED]` **and** `provenance.warnings: [INPUT_UNHANDLED]`; `rel3h2-expect-handled-false`: `pass`, warnings `[]` on the `expect_handled: false` tap and on the `expect_handled: true` button; `rel3h2-expect-handled-true`: `fail` `INPUT_NOT_HANDLED` / `details.reason: expect_handled` / `handled: false` / `target` + `settle`; unit tests `executor::region_tests::rel3h2_*` (4); MCP serves the same executor, and `cargo test -p acos-mcp` is green (86 lib + 12 guide/docs + args/bounded/e2e) |
| 5 | finding 3: unhandled swipe → `ELEMENT_NOT_FOUND` with that hint, never END_OF_LIST; two consumed unchanged gestures for END_OF_LIST; async-loading list not mistaken for an end; real end | PASS | scripted: `backend::tests::an_unconsumed_swipe_is_never_an_end_of_list`, `a_not_handled_delivery_alone_refuses_the_end_of_list`, `a_list_that_loads_after_the_first_unchanged_observation_is_not_an_end`, `a_real_end_of_list_is_reported_after_two_consumed_gestures` (+ `a_swipe_that_moves_nothing_ends_the_search` now expects attempt 2); ART: `scroll_until_whose_swipe_was_not_consumed_is_a_miss` and `scroll_until_stops_at_the_end_of_the_list` (`attempts[]` reason `end_of_list:`), `rel3s_swipe` end block (`unchanged_gestures: 2`, `gesture_consumed: true`, `2 <= attempts < 12`) |
| 6 | finding 4: ART traversal with no settle record fails closed | PASS | `tree_settle_record` on `SessionBackend` + `scroll_until_no_settle_record` (unit-pinned shape); `cargo test -p acos-cli --bins` and the ART scroll tests exercise the record present case |
| 7 | merge `rel-030` (`7168439c`), conflicts, regenerated schemas | PASS | merge commits `3716ff99` / `ab5ce434`; `UPDATE_SCHEMA=1 cargo test -p acos-protocol --test schemas` green; `END_OF_LIST`, `INPUT_NOT_HANDLED` and rel-030's `DAEMON_VERSION_MISMATCH` all present in the regenerated `envelope.json` |
| 8 | `cargo check --workspace --all-targets` + protocol/verify/mcp/cli/art tests | PASS | `cargo check --offline --workspace --all-targets` → `Finished` (0 errors); `-p acos-protocol` 93+3+7+8+9+2; `-p acos-verify` 112 lib + 37 scenarios; `-p acos-mcp` 86 lib + 12 guide/docs; `-p acos-cli --bins` 192; `-p acos-art --lib` 162 |

### Fail-before

* `git show rel3h:crates/acos-art/src/launch.rs` has no `UnhandledSettleGate` (and
  no `ack_floor`/confirmation rule): the gate tests cannot compile there, and on
  `rel3h` the shortcut binds the newest sample regardless of the ack floor.
* `git show rel3h:crates/acos-protocol/src/error.rs | grep INPUT_NOT_HANDLED` →
  nothing; `StepResult` has no `warnings` field; `TapStep` has no
  `expect_handled`. The `executor::region_tests::rel3h2_*` tests cannot exist.
* `git show rel3h:crates/acos-verify/src/backend.rs` returns `END_OF_LIST` after
  **one** unchanged tree with no consumption test; the four new
  `backend::tests` (`an_unconsumed_swipe_is_never_an_end_of_list`,
  `a_not_handled_delivery_alone_refuses_the_end_of_list`,
  `a_list_that_loads_after_the_first_unchanged_observation_is_not_an_end`,
  `a_real_end_of_list_is_reported_after_two_consumed_gestures`) fail on it: the
  first two get `END_OF_LIST` where `ELEMENT_NOT_FOUND` is required, and the
  async case reports the end after attempt 1 (the search never reaches the row
  that arrives).
* `git show rel3h:crates/acos-cli/src/engine/mod.rs | grep tree_settle_record` →
  nothing; the ART loop could not refuse a silent traversal.

## Test commands actually run

Host env: `ACOS_FONT_DIR=/home/Qui/Projects/acos/.cache/boot/android-34/fonts`,
`ACOS_VIEW_TEST_DIR=/home/Qui/.cache/lv` (short, inside `sun_path` and the
`test_scratch_root` budget), `CARGO_TARGET_DIR=$PWD/.cache/rel3h2-target`,
`CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_INCREMENTAL=0`.

```
cargo check --offline --workspace --all-targets                      # green
cargo test  --offline -p acos-protocol -p acos-verify                # green (see table)
cargo test  --offline -p acos-mcp                                    # 86 + args/bounded/docs/e2e green
cargo test  --offline -p acos-cli --bins                             # 192 pass
cargo test  --offline -p acos-art --lib                              # 162 pass, 1 ignored
ACOS_BIN=<release acos> cargo test -p acos-mcp --test guide --test docs --test e2e   # green
UPDATE_SCHEMA=1 cargo test --offline -p acos-protocol --test schemas # green
```

Real ART (release build, `tools/art-runtime/run-exclusive.sh --task REL3H2` +
`tools/guarded.sh --mem 12G`):

```
acos verify scenarios/rel3h2-expect-handled-false.yaml       --engine art  → pass (warnings [])
acos verify scenarios/rel3h2-expect-handled-true.yaml        --engine art  → fail INPUT_NOT_HANDLED
acos verify scenarios/rel3h-unhandled-input.yaml             --engine art  → pass, INPUT_UNHANDLED
acos verify scenarios/rel3h-unhandled-input-expect-change.yaml --engine art → fail expect_change
acos verify scenarios/rel3a-content-{settle,pixel,sticky}.yaml --engine art → pass / fail / timeout, warnings []
cargo test -p acos-cli --release --test rel3e_handled --test rel3s_swipe --test rel3s_scenario \
  -- --ignored --test-threads=1                                       # 1 + 1 + 3 passed
```

`rel3-art-tests-run1.log` keeps one failed run: the `tap-targets` guest died on
launch with `LOOPER_THREW: java.lang.RuntimeException: Adding window failed`
(`GUEST_EXITED` / `GUEST_EXIT_AFTER_RESUME`) — a launch-time WMS flake, not this
change (the same test passed before and after; `rel3-art-tests.log` is the clean
run). Worth a look by whoever owns the WMS shim.

## Files

* `crates/acos-art/src/launch.rs` — `UnhandledSettleGate`, ack floor, gate tests
* `crates/acos-protocol/src/{error,provenance,scenario,verdict}.rs` +
  `lib.rs` (`SETTLE_UNHANDLED_INPUT_REASON`) + regenerated `schema/*.json` —
  `INPUT_NOT_HANDLED`, `WARN_INPUT_UNHANDLED`, `expect_handled`,
  `StepResult.warnings`
* `crates/acos-verify/src/backend.rs` — consumption rule, two-gesture end,
  `ScrollUntilMovement`, softened hints, 4 new + 3 updated tests
* `crates/acos-verify/src/executor.rs` — `expect_handled` claim, `INPUT_UNHANDLED`
  step+verdict warning, `attempts[]` stop reasons, delivery kept on a failed
  `expect_change`
* `crates/acos-cli/src/engine/{mod,art}.rs` — ART `tree_settle_record` + the
  shared search rules, `SETTLE_UNHANDLED_INPUT` pinned to the protocol string
* `crates/acos-cli/tests/{rel3s_scenario,rel3s_swipe}.rs` — the unconsumed-swipe
  scenario and the two-gesture end assertions
* `scenarios/rel3h2-expect-handled-{false,true}.yaml`,
  `docs/{AGENT_GUIDE,ERROR_CODES,AGENT_API}.md`, `EXPERIMENTS.md`

## Left open / for the orchestrator

1. **The posted-side-effect fixture is blocked by an emulation gap.** An ART
   guest acks a tap on a non-clickable area `handled = false` but never calls
   that view's `OnTouchListener` (measured: right node, right point, 2/2 acks
   false in 5 ms, no listener log, no posted `setText`). REL3H2 finding 1 is
   therefore pinned at the gate level (unit tests) and the fixture/test were
   removed rather than committed as a failing test — see `EXPERIMENTS.md` for
   the measurement. A follow-up track ("dispatch touches through the real view
   chain") would let the end-to-end case be tested.
2. The `Adding window failed` guest crash above (WMS shim, launch-time flake).
3. `scroll_until` end-of-list still relies on the settled tree being unchanged;
   a list that redraws without moving (a header counter) will not stop early and
   ends as `ELEMENT_NOT_FOUND` after its attempts — unchanged from REL3H, and now
   no longer confused with an unconsumed gesture.
4. The unhandled-input rule and `expect_handled` are ART-only (the frozen Rust VM
   has no `finishInputEvent` ack path); on that engine `handled: null` makes
   `expect_handled: true` an honest `UNSUPPORTED_ASSERTION`.

Peak RAM observed: ~13 GiB during the release build (guarded 14G), ~1.2 GiB per
ART guest. Disk: `/` 74-78% throughout; `.cache/rel3h2-target` deleted at the end.
