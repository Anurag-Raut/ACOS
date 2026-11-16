# REL3A — "settle is defined by pixels" (ACOS v0.3.0, root cause A)

Branch `rel3a` (base `rel-030`, HEAD `c3f36b11` + REL3C/REL3F). Evidence in this directory.

## What was wrong (v0.3.0 agent feedback)

| # | Symptom | Cause |
|---|---|---|
| 1 | `wait_for` timed out (31 s, `reason: queue` / `newer-store`) on an always-animating RN onboarding screen; `animations: off` did not help; `wait_for` had no `settle` option | the settle model existed only as an *input* option; every observation (`wait_for`, `assert_*`) ran the strict (event/pixel) rule with no way to ask for content stability |
| 2 | `assert_text` ignored `timeout_ms` | it read the frame the previous step bound, once, ~1 ms after the action, and the schema's promise was never implemented |
| 3 | `acos_tap` returned `status: pass` with `settled:false` | `serve::call_art` answered `Envelope::pass` for every delivered action and never looked at the settle timeout the engine had already recorded |
| 4 | MCP `acos_verify` had no `animations` (or `native`) although the hint recommended `animations:off` | `VerifyRequest` lacked both fields and the MCP CLI relay never passed them |
| 5 | an unapplied settle rule could silently run another rule | `EngineBackend::set_observation_settle` (new) was swallowed by `TrustGuard`, the trait default no-op (found by the real-ART run, fixed) |

## Semantic decisions (also in DECISIONS.md ADR-0032)

1. **`settle` is an observation rule, not only an input rule.** `wait_for` and
   `assert_text`/`assert_visible`/`assert_not_visible`/`assert_state` accept
   `settle: strict|content`. `fast` is rejected at validation time (`INVALID_SCENARIO`,
   path `steps[i].<step>.settle`) because it can never back a wait or an assertion.
2. **Default per step kind** (documented in `docs/AGENT_GUIDE.md` §"Settle modes",
   `docs/MCP.md`, `docs/AGENT_API.md`):
   * `wait_for` + tree/text/state assertions → **auto**: strict first, then
     content-stable after strict proves it cannot settle. The step JSON reports
     `mode: content`, `fallback_from: strict`.
   * `tap`/`type`/`key`/`back`/`launch`/`reset` → **strict** (unchanged; an action's
     frame is evidence and an endless animation there is an honest TIMEOUT).
   * `capture`/`assert_screenshot`/`assert_region` → **pixels** (they take their own
     fresh capture; content stability is never pixel evidence).
3. **Never weakened.** An assertion still requires its condition on a settled
   observation of the mode it reports. Content stability still refuses pixel claims
   (`details.reason: content_settle_pixel`) and geometry claims
   (`content_settle_geometry`); an explicit `settle: content` step whose backend
   returns no content record fails closed (`content_settle_unhonored`). An auto
   fallback does not require a record (a backend that reports none still returns a
   settled observation or an error – its contract), but it *reports* the mode it
   asked for with `source: "executor"`.
4. **Every step/reply states the settle outcome.** `settle: {mode, settled,
   waited_ms, reason}` plus the mode-specific evidence (`bound`, `window_ms`,
   `traversals`, `pixels_changed`, `probe_seq`, `fallback_from`, ...).
   `waited_ms` is the backend's own measurement when it separated the settle
   (`waited_ms`/`elapsed_ms`); otherwise the step's wall time, marked
   `waited_ms_source: "step"`.
5. **A settle that timed out is never a pass — chosen shape:**
   * scenario step → `status: timeout`, `error.code: TIMEOUT`, error class
     `infra_timeout` (the verdict model's existing non-pass status for "the
     harness/engine could not complete", not a new `SETTLE_TIMEOUT` warning code:
     an agent already branches on `status` and on `error.class`, and a warning next
     to `status: pass` is exactly the shape that produced the F-06 false green).
   * live channel (`acos_tap`/`acos_text`/`acos_key`/`acos_action`) →
     `status: timeout`, `ok: false`, `error.code: TIMEOUT`,
     `error.details.reason: "settle_timeout"`, the normalised record in
     `error.details.settle`, and the delivered facts (`accepted`, `state_changed`,
     `frame`, `changes`, `timing`) in `error.details.delivery` **and** in `result`,
     so the screen is not lost.
6. **`timeout_ms` is a real wait on assertions.** With `timeout_ms` a tree/text/state
   assertion polls fresh settled observations until the condition holds or the
   deadline passes; the frame it judged becomes the bound frame (so a following
   strict capture/pixel assertion describes the freshest observation). Without it
   the old single-observation, no-extra-traversal path is unchanged (perf).
7. **MCP parity:** `VerifyRequest` gains `animations: on|off` and `native: bool`,
   relayed to `acos verify --animations/--native`. Hints that mention an option now
   name real ones; `settle_timeout_error` no longer recommends `animations:off` for
   app-level animations and names `settle: content` instead.
8. Bench-harness helpers `_environ_has`/`_record_daemon` were referenced but never
   defined in `tools/art-runtime/d2_warm.py` (NameError aborted the whole latency
   phase); both are implemented (ownership semantics unchanged).

## Acceptance criteria

| # | Criterion | Result | Evidence |
|---|---|---|---|
| 1 | content stability usable by every wait/assertion/action; default per step kind documented | PASS | `crates/acos-verify/src/executor.rs` (`rel3a_settle_tests`), `docs/AGENT_GUIDE.md`, ADR-0032; ART: steps 2–5 of `artifacts/content-settle/verdict.json` |
| 2 | every step/reply states mode + outcome | PASS | `verdict.json` step records (`settle` object) for both ART scenarios; `crates/acos-verify/src/settle.rs::settle_record` + tests |
| 3 | a timed-out settle is never a pass | PASS | verify: `rel3a_settle_tests::wait_for_strict_on_an_animating_screen_is_a_timeout`, `an_assertion_without_a_settled_observation_is_an_infra_timeout`; live: `crates/acos-cli/tests/rel3a_settle_status.rs` |
| 4 | `assert_text`/`assert_*`/`wait_for` honour `timeout_ms` | PASS | `rel3a_settle_tests::assert_text_honours_timeout_ms` (fails without `timeout_ms`, passes with) |
| 5 | MCP `acos_verify` gets `animations` + `native`; hints name real options | PASS | `crates/acos-protocol/src/commands.rs` (`VerifyRequest`), `crates/acos-mcp/src/backend.rs` relay, `crates/acos-protocol/schema/verify-request.json`, `docs/MCP.md` |
| 6 | tests fail-before/pass-after: unit per step kind, ART always-animating fixture, settle-timeout-not-pass | PASS | `cargo test -p acos-verify` (97 lib tests incl. 7 new), `cargo test -p acos-cli --test rel3a_settle_status` (3), real ART: `scenarios/rel3a-content-settle.yaml` (pass) + `scenarios/rel3a-content-pixel.yaml` (fail, `content_settle_pixel`) |
| 7 | A/B: no regression on M4/M6/M8 fast/strict vs `rel-030` | **NOT MEASURED (blocked)** | both arms abort identically before any metric: `RuntimeError: two consecutive settled captures differ: [None, None]` (`bench/rel030-arm-latency.json`, `bench/rel3a-arm-latency.json`) |

### Why criterion 7 is blocked (and why it is not a REL3A regression)

`tools/bench/acos_bench.py::make_reference` requires `frame.md5` in the
`acos_observe {png:true, screenshot:<path>}` reply. On `rel-030` that reply's
`frame` object no longer carries `md5` (`serve::label_frame` only fills `frame`
when the reply has none, and the observe/screenshot path already set one without
`md5`). Both the base arm and the REL3A arm stop at the same line, on the same
message, with no samples taken — the arm binary differs
(`1c322ba8…` base vs `2a80f9dd…` rel3a) and the failure is byte-identical.
That is a **pre-existing `rel-030` defect in the live observe reply / M5 path**,
independent of this change; it needs its own fix and re-blessed M5 numbers.

Structural argument that M4/M6/M8 cannot regress: the diff touches only
(a) the `wait_for`/assertion observation loops, (b) `ArtSession::frame()` (one
string compare; the `content` branch is only taken when the caller asked for it),
(c) reply/step JSON normalisation (pure in-memory `serde_json` work), and (d) the
`TrustGuard` forward. The M4 launch, M6 reset and M8 tap strict/fast paths are
byte-for-byte unchanged. That is a statement about the code, **not** a
measurement; criterion 7 remains open.

## Test commands actually run (all on this branch)

```
cargo check --offline --workspace --all-targets                      # green
cargo test  --offline -p acos-verify -p acos-protocol                # green (97 + 71 + integration)
cargo test  --offline -p acos-mcp --lib --test guide --test docs     # green (82 + 12 + 1)
cargo test  --offline -p acos-cli --test rel3a_settle_status         # green (3)
cargo test  --offline -p acos-cli --bins                             # 182 pass, 2 fail:
    view::tests::frames_and_events_stream_and_close_the_upstream_...  # pre-existing:
    view::tests::the_listing_hides_stale_sessions_and_reports_metadata# both fail identically on rel-030
cargo test  --offline -p acos-mcp --test e2e                         # needs a release `acos` in the
                                                                     # checkout root; environment, not code
UPDATE_SCHEMA=1 cargo test -p acos-protocol                          # schemas refreshed (4 files)
```

Real ART (env: `ACOS_FONT_DIR`, `ACOS_ART_BIN=<worktree>/.cache/rel3a-target/release/acos-art`,
inside `run-exclusive.sh --task REL3A` + `guarded.sh --mem 8G`):

```
acos verify scenarios/rel3a-content-settle.yaml --engine art   → verdict pass
acos verify scenarios/rel3a-content-pixel.yaml  --engine art   → verdict fail (content_settle_pixel)
```

Step records from the passing run (`artifacts/content-settle/verdict.json`):

| step | status | settle |
|---|---|---|
| 1 `wait_for` (strict) | pass | `mode: strict, settled: true, waited_ms: 55, reason: strict-event` |
| 2 `tap settle: content` | pass | `mode: content, settled: true, waited_ms: 331, reason: content-stable` |
| 3 `wait_for` (default) | pass | `mode: content, settled: true, waited_ms: 333, fallback_from: strict` |
| 4 `assert_text timeout_ms` | pass | `mode: content, settled: true, waited_ms: 333` |
| 5 `assert_visible timeout_ms` | pass | `mode: content, settled: true, waited_ms: 333` |

and from the negative twin, step 5 `assert_region` →
`status: fail`, `ASSERTION_FAILED`, `details.reason: content_settle_pixel`.

## Files

* `crates/acos-protocol/src/scenario.rs`, `assertion.rs` — schema + validation
* `crates/acos-verify/src/settle.rs` — `ObservationSettle`, `settle_record`,
  `observation_settle_record`
* `crates/acos-verify/src/executor.rs` — wait/assert observation loops, records,
  ledger rebind, `rel3a_settle_tests`
* `crates/acos-verify/src/backend.rs` — `set_observation_settle`
* `crates/acos-cli/src/engine/{art,mod}.rs` — observation settle in `frame()`
* `crates/acos-cli/src/engine/serve.rs` — `settle_timeout_envelope`
* `crates/acos-cli/src/trust.rs` — forward the rule through the guard
* `crates/acos-cli/src/verify.rs`, `crates/acos-protocol/src/commands.rs`,
  `crates/acos-mcp/src/backend.rs`, `crates/acos-protocol/src/mcp.rs` — MCP parity
* `scenarios/rel3a-content-settle.yaml`, `scenarios/rel3a-content-pixel.yaml`
* `crates/acos-cli/tests/rel3a_settle_status.rs`
* `docs/AGENT_GUIDE.md`, `docs/MCP.md`, `docs/AGENT_API.md`, `DECISIONS.md` (ADR-0032)
* `tools/art-runtime/d2_warm.py` — two missing helpers

## Left open

1. **Criterion 7** — re-run the A/B after the `rel-030` observe/`md5` defect is
   fixed (smallest action: make the observe reply's `frame` carry `md5`, re-bless
   M5, then `acos-bench.sh --reps 3 --phases latency` per arm + `ab-compare.py`).
2. `crates/acos-cli --bins`: 2 pre-existing `view::tests` failures (identical on
   `rel-030`).
3. `crates/acos-mcp/tests/e2e` needs a release `acos` beside the manifest dir.
4. Optional: a `settle_timeout_ms` on assertions (today they reset the previous
   step's budget and use the engine default per attempt).
