# MERGE3A — `rel3a-fixes` (settle semantics) merged into the v0.3.0 line

**Status: DONE.** Branch `rel-030-3a`, merge commit `ca5a232e`, base `rel-030`
at `c295190c`. Both intents are preserved: REL3A's scoped content settle and
REL3E's input `handled`/`target`/`input` block live in the same step record, and
the steps REL3BIT/REL3S2 added go through REL3A's rule.

At commit time `rel-030` was still at `c295190c`, so it can be **fast-forwarded**
to `rel-030-3a`. The brief named `2022d89c` as the line's head; it had already
moved to `8cc9d8c0` (rel3k) before this work started and to `c295190c` (rel3s2,
ART swipe/scroll + scenario steps) while it ran. The merge was re-cut twice so
the branch always sits on the line's current head; the earlier attempt
(`a617c2b9`, base `8cc9d8c0`, fully verified) is kept on branch
`merge3a-a617c2b9` in case the orchestrator prefers that base.

## Conflict resolution (4 textual, 2 semantic)

No two intents contradicted each other; every conflict was placement.

| File | Resolution |
|---|---|
| `crates/acos-verify/src/executor.rs` | `step_settle` is **REL3A's** 3-argument form (normalised `{mode, settled, waited_ms, reason}`, per-step record first, `waited_ms` falls back to the step's wall time). **REL3E's** `step_result.input = step_input(...)` stays next to it, with REL3S2's `exec.until` argument. Every step's JSON therefore carries *both* blocks. |
| `docs/AGENT_GUIDE.md` | "Settle modes" keeps REL3A's per-step-kind defaults, the two-observation rule, the residual hazard, the scoped/no-leak rule and the content-vs-geometry/pixel refusal, then REL3E's `tap` `expect_change: auto` / `input` block paragraph. The `fast` bullet merges both sides ("never assertion evidence"; rejected by `wait_for` and every `assert_*` with `INVALID_SCENARIO`/`INVALID_ARGUMENT`). REL3S2's action steps are named in the strict-default list (`swipe`) and `scroll_until`'s missing `settle` option is stated. |
| `crates/acos-protocol/src/scenario.rs` | Both sides appended a validator at the same anchor: REL3S2's `validate_swipe_geometry` and REL3A's `validate_observation_settle` are both kept, unchanged. |
| `tools/art-runtime/d2_warm.py` | Both sides defined the same two missing helpers independently (`_record_daemon`, `_environ_has`). Kept SERVELEAK2/F2's implementation (start-time field 22 read after the last `)`, exact `KEY=VALUE` match) and recorded REL3A's `NameError` finding in the docstrings. |

Semantic (auto-merged, but the result did not compile / did not pass):

1. `crates/acos-verify/src/executor.rs`, `crates/acos-verify/tests/scenarios.rs`
   — REL3A's new tests built `RunOptions` without REL3B's `engine`/`warnings` and
   REL3B's six `VerifyRequest` literals without REL3A's `animations`/`native`.
   Both filled in (`engine: "art"`, `warnings: Vec::new()`, `animations: None`,
   `native: false`); no assertion changed.
2. `crates/acos-cli/tests/rel3e_handled.rs` — the REL3E live test expected
   `ok:true` for a tap on the non-clickable label. On the merged tree that tap
   draws no frame, so REL3A's rule applies: the strict settle times out and "a
   settle that timed out is never a pass". The reply is now `status: timeout`,
   `ok:false`, `error.code: TIMEOUT`, `details.reason: settle_timeout` **with the
   delivered facts kept** (`result.handled:false`, target/hit/switch suggestion,
   hint, `error.details.delivery`). The test asserts the non-pass *and* every
   REL3E fact, so it is stronger, not weaker. REL3S2's real-ART swipe assertions
   in the same file are untouched and pass.

Nothing was weakened, no test was ignored to make it pass, and no behaviour was
dropped from either side.

## Verification on `ca5a232e` (base `c295190c`)

All commands with `CARGO_TARGET_DIR=$WT/.cache/merge3a-target`
(`CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_INCREMENTAL=0`),
`ACOS_FONT_DIR=/home/…/android-34/fonts`, ART runs through
`tools/art-runtime/run-exclusive.sh --task MERGE3A` + `tools/guarded.sh`.

| Check | Result | Evidence |
|---|---|---|
| `cargo check --workspace --all-targets --offline` | **green** (no errors; pre-existing warnings only) | `cargo-check.log` |
| `cargo test --offline -p acos-verify -p acos-protocol -p acos-mcp` | **403 passed, 0 failed**, 23 suites (mcp lib 86 + e2e/guide/protocol/documentation_args…, protocol lib 82 + 3/7/8/43/2, verify lib 101 + 34 scenarios) | `crate-tests.log` |
| `cargo test --offline -p acos-cli --test verify --test rel3a_settle_status --test rel3f_front_doors --test rel3e_handled` | **24 passed, 0 failed** (3 + 2 + 9 + 10) | `cli-tests.log` |
| `cargo test --offline -p acos-cli --test rel3e_handled -- --ignored --test-threads=1` (real ART guest) | **1 passed** (15.0 s) | `cli-rel3e-handled-art.log` |
| `cargo test --offline -p acos-cli --bins` | **188 passed, 2 failed** — both pre-existing deep-worktree failures: `view::tests::{frames_and_events_stream_and_close_the_upstream_with_the_last_browser, the_listing_hides_stale_sessions_and_reports_metadata}` (`path must be shorter than SUN_LEN`, `crates/acos-cli/src/view/mod.rs:991/913`). Identical on the previous base and on `rel-030` (REL3A report + REL3E handoff). | `cli-bins.log`, `cli-bins-8cc9d8c0.log` |

### ART scenarios (real ART, engine `art`, `--required-fidelity art-reference`)

| Scenario | Required | Observed | Evidence |
|---|---|---|---|
| `scenarios/rel3a-content-settle.yaml` | pass | **pass**, 6 steps | `artifacts/content-settle/verdict.json`, `art-content-settle.log` |
| `scenarios/rel3a-content-pixel.yaml` | non-pass | **fail** — `ASSERTION_FAILED`, `assert_region`: "the bound frame was settled by `settle: content` … not pixel evidence" | `artifacts/content-pixel/verdict.json`, `art-content-pixel.log` |
| `scenarios/rel3a-content-sticky.yaml` | non-pass | **timeout** — `capture: strict settle timed out (traversal)` (the sticky content rule does not leak into the pixel capture) | `artifacts/content-sticky/verdict.json`, `art-content-sticky.log` |

Both blocks in one step record (from `artifacts/content-settle/steps/02-tap.json`
and `…/03-wait_for.json`):

| step | `settle` | `input` |
|---|---|---|
| 02 `tap` `settle: content` | `mode: content, settled: true, waited_ms: 650, reason: content-stable, pixel_evidence: false` | `handled: true, expect_change: "auto"`, `handled_evidence{source: finishInputEvent, published: 2, acked: 2}`, `target{selector, node, point, hit{suggest}}` |
| 03 `wait_for` (no `settle`) | `mode: content, settled: true, waited_ms: 710, reason: content-stable, fallback_from: strict` | — |
| 04 `assert_text timeout_ms` | `mode: content, settled: true, waited_ms: 572, reason: content-stable, fallback_from: strict` | — |
| 05 `assert_visible timeout_ms` | `mode: content, settled: true, waited_ms: 558, reason: content-stable, fallback_from: strict` | — |

(`content-sticky` steps 01/03 report `mode: fast, reason: state-commit` together
with `input.handled: true, expect_change: "auto"`; step 04, the pixel capture, is
the timeout.)

## For the orchestrator

1. **Fast-forward `rel-030` to `rel-030-3a` (`ca5a232e`)** if it is still at
   `c295190c`; otherwise merge `rel-030-3a` (do not re-cut: the branch already
   holds rel3k and rel3s2).
2. **Expected, not a regression:** the three `rel3a-content-*.yaml` scenarios
   declare the legacy `engine: native-preferred`, so REL3B's new
   `provenance.warnings` reports `SCENARIO_ENGINE_MISMATCH` (1 warning) on each
   run. The verdict is unaffected. Rewriting those scenarios to `engine: art`
   would silence it; I did not edit REL3A's committed scenarios.
3. **Still open from REL3A:** acceptance criterion 7 (bench A/B) is unmeasured —
   both arms still abort in `tools/bench/acos_bench.py::make_reference` because
   the live `acos_observe {png:true}` reply's `frame` has no `md5` (pre-existing
   `rel-030` defect, not touched here).
4. The `acos-cli --bins` `view::tests` failures and `tests/flutter_session.rs`
   (fixture-gated, skipped) are pre-existing environment/path issues, unrelated
   to this merge.
5. Cleanup: `target/` and `.cache/merge3a-target` deleted (with the rest of the
   scratch) when the work closed; `/` went 82 % → 83 % peak → 80 % after cleanup.
   Host `free` was 12–14 GiB used of 60 GiB during the runs; every build/test/ART
   run was inside `guarded.sh --mem 10–12G`, one exclusive ART acquisition per
   scenario. Nothing of the owner's (`~/.local`, MCP registrations,
   `~/Projects/acos`, `~/Projects/gaten`) was touched.
