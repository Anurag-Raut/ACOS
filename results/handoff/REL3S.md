# REL3S — swipe/scroll on the real-ART engine

Branch `rel3s` (base `rel-030` `cec9c358`); **REL3S2** (branch `rel3s2`) finished
and ran the ART proof and the scenario steps. Track REL3S of
`docs/PLAN_2026-11-16.md` (v0.3.0). Feedback entry 9/10
(`~/.claude/acos-feedback/ACOS_AGENT_FEEDBACK.md`): an agent could not scroll a
list in a real session — `acos_swipe` / `acos swipe` / a scenario swipe step on
ART answered `UNSUPPORTED_BACKEND` (`input.swipe`) because the live input channel
had a tap publisher and nothing else.

## Status: DONE (REL3S2)

Session 1 wrote the gesture stream, the guest command and the `until` search and
unit-tested them, but the ART run was refused by the disk limit and the scenario
steps were not written. REL3S2 ran the ART proof on the real guest, added the two
scenario steps, and ran one third-party (Gaten) scroll check. Evidence:
`results/handoff/REL3S/art-test.log`, `art-scenario.log`, `gaten-scroll.log`,
`gaten-scroll-compare.txt`, `scenario-*.verdict.json`, `gaten-scroll.verdict.json`.

## REL3S2 completion

* **ART e2e (`crates/acos-cli/tests/rel3s_swipe.rs`) — RAN, PASS.** On the real
  ART fixture: one 300 ms fling moved the 100-row list (`Row 0` gone, `Row 23`
  newly visible), `scroll_until {"text":"Row 80"}` found it (strict frame), and a
  `Row 9999` miss failed with `ELEMENT_NOT_FOUND`, `attempts: 2`,
  `max_attempts: 2` and the last tree's texts. Two test-side fixes from the first
  real run: the capability check reads `acos_status` (the launch reply carries
  `capabilities_used`, not the session capability map), and the delivery key is
  `accepted` (not `delivered`). One product fix the run found: the miss's
  `last_tree_texts` was empty because the search read only root nodes; it now
  uses `UiTree::all_nodes()` (same fix in the verifier's default search).
* **Scenario steps (`acos-protocol::scenario`)** — `swipe` and `scroll_until`,
  schema regenerated (`crates/acos-protocol/schema/scenario.json`). Geometry is
  explicit `from`/`to` points **or** `direction` (+ `distance`), validated with
  stable paths; `scroll_until` always runs strict (a found node must come from a
  settled observation). `SwipeDirection::endpoints` is the one rule the CLI/MCP
  `acos_swipe` and the executor share.
  `acos-verify`: `InputAction::Swipe`, `Backend::scroll_until` (default loop =
  gesture + fresh observation), `Backend::display_size`, executor arms with
  settle handling and REL3E's `input` block (plus `input.until` for the search).
  `EngineBackend` forwards to `SessionBackend::swipe` / `SessionBackend::scroll_until`
  (same source as `acos_swipe`), `NativeBackend` uses the frozen VM's
  `AgentSession::swipe`; `TrustGuard` forwards delivery/display/search (it was
  dropping `last_input_delivery`, so scenario `input` blocks never appeared).
* **Scenario e2e (`crates/acos-cli/tests/rel3s_scenario.rs`) — RAN, PASS.**
  `swipe` then `assert_text` of the row the launch tree did not render (the test
  reads the pre-gesture capture tree and asserts the exact fold+1 row is new),
  then `scroll_until Row 80` + `tap` + `Selected: Row 80`. The search uses a
  controlled 500 px drag because a fast fling can pass the target between two
  settled observations (the first attempt at a 14×120 ms fling overshot to
  `Row 85..99`; the honest miss showed it).
* **Gaten scroll smoke — RAN, PASS.** `scenarios/rel3s-gaten-scroll.yaml` on
  `gaten-dev-f344272-x86_64-release-unsigned.apk`: search `a` → result list →
  swipe (38 events, `handled: true`, `pixels_changed: true`, `expect_change`
  satisfied). Trees: `17 results` and the first rows scrolled off, new rows
  arrived, the first result's bounds moved `y=1489 → 36`.
* **Known, pre-existing (not this branch):** `cargo test -p acos-cli --bins`
  fails the two `view::tests` on this deep worktree path (`SUN_LEN`, documented
  by REL3E); `cargo test -p acos-mcp --test compact_reply` fails because that
  test still sends a `cursor` argument `acos_verify` no longer accepts (stale on
  `rel3s`; unchanged by REL3S2).

## Session 1 (original notes, kept for the record)

### What was built

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
| ART e2e: one swipe moves a 100-row list, `scroll_until` finds row 80, a miss fails honestly | **PASS (REL3S2)** | `results/handoff/REL3S/art-test.log` — `test result: ok. 1 passed` |
| scenario `swipe` step / `scroll_until` as a scenario step | **PASS (REL3S2)** | implemented (`scenario.json` regenerated) and run on the ART fixture: `results/handoff/REL3S/art-scenario.log`, `scenario-swipe.verdict.json`, `scenario-scroll.verdict.json` |
| Gaten scroll check | **PASS (REL3S2)** | `scenarios/rel3s-gaten-scroll.yaml` on `gaten-dev-f344272`: verdict pass, 38/38 events handled, list moved — `results/handoff/REL3S/gaten-scroll.log`, `gaten-scroll-compare.txt` |

Exact command for the ART test (needs a release `acos-art`; REL3S2 built it in
`.cache/rel3s2-target/release/`):

```
ACOS_APP_ONLY=scroll-list tools/build-apps.sh
tools/art-runtime/run-exclusive.sh --task REL3S -- \
  tools/guarded.sh --mem 10G --time 900 -- \
  env -u ACOS_HOME CARGO_TARGET_DIR=$PWD/.cache/rel3s2-target \
  cargo test -p acos-cli --release --offline --test rel3s_swipe -- --ignored --test-threads=1
```

The scenario test uses the same launcher/target:

```
  cargo test -p acos-cli --release --offline --test rel3s_scenario -- --ignored --test-threads=1
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
