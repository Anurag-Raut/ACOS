# REL3S — swipe/scroll on the real-ART engine

Branch `rel3s` (base `rel-030` `cec9c358`). Track REL3S of
`docs/PLAN_2026-11-16.md` (v0.3.0). Feedback entry 9/10
(`~/.claude/acos-feedback/ACOS_AGENT_FEEDBACK.md`): an agent could not scroll a
list in a real session — `acos_swipe` / `acos swipe` / a scenario swipe step on
ART answered `UNSUPPORTED_BACKEND` (`input.swipe`) because the live input channel
had a tap publisher and nothing else.

## Status: PARTIAL (feature implemented, ART end-to-end test NOT run)

The code is complete for the core feature and unit-tested. The ART end-to-end
proof (`crates/acos-cli/tests/rel3s_swipe.rs`) is written but **was not executed**:
the host disk reached **88 %** (`df -h /`) while the run was queued, and
COMMON_RULES says stop and report at >= 85 %. No acceptance claim is made for
anything below marked UNVERIFIED.

## What was built

1. **Real gesture stream (`crates/acos-services`)**
   * `input_channel::swipe_samples(from, to, duration_ns, interval_ns)` — the pure
     interpolation: a sample at the down point, one `MOVE` per display refresh
     interval (`GESTURE_SAMPLE_INTERVAL_NANOS` = 16 ms), and a final sample at the
     up point whose offset is exactly `duration_ns`. Positions are linear in time,
     so the timestamps a `VelocityTracker` reads produce the gesture's real
     velocity.
   * `input_dispatch::InputDispatch::inject_swipe` — publishes `DOWN`, the
     `MOVE`s and `UP` **paced** at the refresh cadence (a sleep until each
     sample's scheduled time, `eventTime = downTime + offset`), on the same real
     `InputChannel` (socketpair, framework `InputMessage` layout) a tap uses;
     coordinates ride in the message transform exactly as `inject_tap` measured
     them (I2).
   * `InputHost::swipe(from, to, duration, ack_wait)` — collects every `FINISHED`
     reply, i.e. each event's `finishInputEvent(handled)` bit (REL3E's contract).
2. **Guest protocol (`crates/acos-art/src/launch.rs`)**: the live command loop
   accepts `{"cmd":"swipe","x1","y1","x2","y2","duration_ms"}` next to
   `tap`/`key`/`type_text`, publishes through `live.input.swipe(...)`, then runs
   the **existing** settle (`wait_for_settled_observation`) and reports the
   `gesture {seqs, acks, token}` block with per-ack `handled`. No settle rule was
   changed (REL3A owns those).
3. **Host session (`crates/acos-cli`)**
   * `SessionCapabilities::swipe` + `SessionBackend::swipe(from,to,duration_ms,fling)`
     (default = structured `UNSUPPORTED_BACKEND input.swipe`, so a future engine
     cannot silently no-op) + `ArtSession::swipe` which sends the guest command
     with window-local coordinates (the `tap` origin rule).
   * `SessionBackend::scroll_until(selector, from, to, distance, fling,
     max_attempts)` — one **single source** for the repeated search: swipe, take
     a fresh **strict** traversal (`self.tree()`, never a cached tree), require
     the node to be `visible`; a miss is `ELEMENT_NOT_FOUND` with
     `details {attempts, max_attempts, last_frame_seq, selector, last_tree_texts}`
     — never a found, never a pass on a cached tree.
   * `serve.rs`: `acos_swipe` is a real verb now (was a refusal). Arguments:
     `from`/`to` objects **or** `direction` (`up`/`down`/`left`/`right`) +
     `distance`; `duration_ms` (default 400, 120 with `fling`); `fling`;
     `until` + `max_attempts`. The reply is `input_result(...)`, so it carries
     REL3E's `handled`/`handled_evidence`/`target`/`handled_hint`, FB3's
     `changes`, the settle record and (for `until`) `until {selector, found,
     attempts, frame_seq}`. `acos_action {"kind":"swipe"|"scroll"}` reaches the
     same path; `acos_status.capabilities.swipe` is now `true` and
     `unsupported` is empty.
4. **MCP/CLI surface**: `acos_swipe` forwards `direction`/`distance`/`fling`/
   `until`/`max_attempts` to the daemon; the tool schema + descriptor + help
   result row were updated (strict-args would otherwise reject the new fields).
   The frozen Rust VM refuses `until` explicitly (only ART can prove a settled
   observation).
5. **Docs**: `docs/AGENT_GUIDE.md` workflow §5 (gesture shapes, `direction`
   semantics, fling vs drag, `until`/`scroll_until` contract and its honest
   failure, per-event `handled`) and `docs/MCP.md` §1c/§1d + a new
   §1d-bis (`acos_swipe` + `scroll_until`).
6. **Fixture**: `fixtures/apps/scroll-list` (a `ListView` of 100 rows,
   `Row 0`..`Row 99`, plain framework APIs) built by `tools/build-apps.sh`
   (`ACOS_APP_ONLY=scroll-list`, built and signed here:
   `fixtures/build/apps/scroll-list.apk`).

## Acceptance

| Item | Status | Evidence |
|---|---|---|
| unit tests: interpolation/timestamps/pacing/per-event acks | **PASS** | `results/handoff/REL3S/unit-tests.log` — 4 tests `... ok` (`swipe_samples_interpolate_the_path_at_the_refresh_cadence`, `a_short_swipe_still_publishes_one_move_and_ends_at_the_lift_time`, `publish_swipe_sends_down_moves_and_up_with_real_event_times`, `input_host_swipe_reports_every_events_handled_bit`) |
| unit tests: crates green (`acos-services`, `acos-protocol`, `acos-cli` bins, MCP guide) | **PASS** | same log: 82 + 90 + 120 + 12 passed, 0 failed |
| workspace compiles (`cargo check --workspace --all-targets`) | **PASS** | no `error` lines |
| ART e2e: one swipe moves a 100-row list, `scroll_until` finds row 80, a miss fails honestly | **UNVERIFIED — NOT RUN** | test written at `crates/acos-cli/tests/rel3s_swipe.rs`; the run was refused by the disk limit (88 %). Re-run with the exact command below. |
| scenario `swipe` step / `scroll_until` as a scenario step | **NOT DONE** | the trait + serve.rs path exists (`InputAction`-independent); the scenario schema step (`StepRef::Swipe` in `acos-protocol::scenario`, `executor.rs`, `InputAction::Swipe` in `acos-verify::backend`, `EngineBackend::input`, `NativeBackend::input`) was not written |
| Gaten scroll check | **NOT RUN** | `ls /home/Qui/Projects/acos/.cache/gaten` has a dev APK; no ART run attempted because of the disk limit |

Exact command for the ART test (needs a release `acos-art`, already in
`.cache/rel3s-target/release/` if the target is kept):

```
ACOS_APP_ONLY=scroll-list tools/build-apps.sh
tools/art-runtime/run-exclusive.sh --task REL3S -- \
  tools/guarded.sh --mem 10G --time 900 -- \
  env -u ACOS_HOME CARGO_TARGET_DIR=$PWD/.cache/rel3s-target \
  cargo test -p acos-cli --release --offline --test rel3s_swipe -- --ignored --test-threads=1
```

## Also changed (integration fixes, unrelated to swipe)

* `crates/acos-mcp/src/server.rs`: `rel-030` did not compile — `tool_result(...)`
  was called with 4 of its 5 arguments on the strict-args refusal path. Passed
  `&args`. (Found by `cargo check`; blocked every test in this branch.)
* `crates/acos-cli/tests/rel3e_handled.rs` and `w50_serve_art.rs`: their
  assertions "ART swipe is `UNSUPPORTED_BACKEND` / `capabilities.swipe == false`"
  are the old contract; they now assert a delivered gesture and `swipe: true`.

## Known gaps / next actions (smallest first)

1. **Run the ART test** (command above) and paste the log into
   `results/handoff/REL3S/art-test.log`. Expect tuning: the scroll size 900 px on
   the fixture's display, and `Row 80` may need `max_attempts` 12-20 depending on
   the row height the guest reports.
2. **Scenario step**: `SwipeStep { from/to | direction/distance, duration_ms,
   fling, until, max_attempts, settle, settle_timeout_ms }` in
   `acos-protocol::scenario` (+ `Step`, `StepRef`, `variant_count`, `as_step`,
   `kind`, validation), `InputAction::Swipe` in `acos-verify::backend`,
   `executor.rs` arm (`backend.set_input_settle*`, `input_step`,
   `frame_advancing_step`, `fast_input_step`, `content_settle_unhonored`,
   `step_input` for the `handled` block), `EngineBackend::input` →
   `session.swipe` / `session.scroll_until`, `NativeBackend::input` →
   `session.swipe(...)` (the frozen VM has `swipe` on its `AgentSession`).
3. Gaten scroll smoke on the RN APK.
4. `results/handoff/REL3S/` needs the ART evidence; the guide already documents
   the contract.

## Notes for the orchestrator

* `rel-030` was **not buildable** as merged (see above). Anyone merging this
  branch should keep that one-line fix.
* One `cargo test -p acos-cli --bins engine::` run under load reported 1 failure
  whose name was not captured; the immediate clean re-runs were 120/120 green.
  Not reproducible, and the suite's own view tests (`view::tests`) fail on this
  deep worktree path for `SUN_LEN` reasons (pre-existing, documented by REL3E).
* Peak RAM observed ~9.5 GiB during the release build; disk `/` went 82 % → 88 %
  during the track (not from this worktree's target: 3.8 GiB).
