# SE2b handoff — SE2's `settle: content` review fixes (branch `settle-se2-content-stable`)

Worktree `/home/Qui/.paseo/worktrees/3oxa6xao/s3ab`, track SE2b, 2026-11-14.
Base `7837894` (SE2). Commits: `4ecaea8`, `cd8b644`, `…`.

A read-only review of the SE2 landing returned **DO-NOT-MERGE** with nine
findings. All nine are fixed, each with a test that fails before the fix. One
new defect was found and fixed while proving finding 3. EXPERIMENTS.md E13.16
carries the same record (subsection *E13.16b*; the experiment number is kept).

## 1. Findings, fixes, tests

| # | Finding | Fix (files) | Test (all PASS) |
|---|---|---|---|
| 1 | **CRITICAL** content traversals not proven fresh/distinct: strict `tree` + 1 ms deadline returns `latest_probe()` with no floor, `frame_seq` advances per reply, so two "traversals" could be one stale sample | window traversals are fast/tree-only requests with a one-shot `min_probe_seq` floor (`acos-art/src/launch.rs`); `ContentProbeWindow` requires strictly increasing `probe_seq`, refuses a missing `probe_seq` (`content-probe-unproven`), counts only distinct fresh traversals, floor = newest sample bound before the window (`acos-verify/src/settle.rs`, `acos-cli/src/engine/art.rs`) | `settle::tests::a_stale_probe_sample_never_counts_toward_the_window`, `a_reply_without_probe_seq_fails_closed`, `a_repeated_probe_sample_does_not_count_as_a_distinct_traversal`, `the_next_floor_is_the_last_consumed_sample` |
| 2 | **HIGH** serve `acos_assert` served the cached tree in content mode | assert re-runs `content_settle()` on fresh traversals (`assert_needs_fresh_content_settle`); `begin_input`/`capture_frame` clear `bound_settle` (`serve.rs`, `art.rs`) | `engine::serve::tests::a_content_assert_reproves_the_settle_instead_of_reading_the_cache` |
| 3 | **HIGH** hang: deadline gated on `traversals >= 2`, which a changing signature resets forever | deadline checked on every traversal inside `ContentProbeWindow::attempt` | `settle::tests::a_signature_that_changes_every_traversal_still_times_out`; ART control `negative/gaten-content-window-impossible.yaml` → `TIMEOUT`/`content-not-stable` 3/3 |
| 4 | **MEDIUM** "honoured" matched only the mode string; recovery branch skipped the rule | require `settled: true` **and** `settle.frame_seq == observation frame` (`produced_frame` for launch); the `NoFreshFrame` recovery branch runs `content_settle` (`executor.rs`, `art.rs`) | `content_settle_tests::content_settle_requires_the_settled_frame_identity` |
| 5 | **MEDIUM** signature missed fields assertions read | hash `resource_id`, `clickable`, `scrollable`, `window`, node `package`, tree `package`/`truncated`/`text_available`; `SIGNATURE_FIELDS` is the audited list. **Geometry (`bounds`/`visible_bounds`) is refused instead of hashed** — measured 1 px drift per fresh traversal on the reference screen (`.cache/se2b/probe-trees.json`); `content_settle_geometry` fails closed | `settle::tests::signature_covers_every_field_an_assertion_reads`, `content_settle_tests::geometry_claims_are_refused_on_content_settle` |
| 6 | **LOW** reset silently mapped `content` → strict | structured `INVALID_ARGUMENT` (`reset_bind`) + `debug_assert` | `engine::serve::tests::reset_refuses_a_content_settle_instead_of_silently_binding_strict` |
| 7 | **LOW** `expect_change` + content failed closed incidentally | refused up front for both unmeasured rules | `engine::serve::tests::expect_change_is_refused_with_every_unmeasured_settle` |
| 8 | **LOW** step evidence preferred the sticky settle | `step_settle` prefers the per-action record; `bound_settle` cleared per action | `content_settle_tests::step_evidence_prefers_the_per_action_settle` |
| 9 | **LOW** weak negatives | new `expect_error: {code, reason}` in the scenario schema, enforced by `apply_expectation` (mismatch = harness `INTERNAL`, `negative_expectation_mismatch`); new controls + post-tap assertion in the positive | `content_settle_tests::a_negative_failure_must_match_its_pinned_error`; `gaten-content-assert-screenshot`, `gaten-content-window-impossible`, `gaten-content-welcome` |

**Found while fixing 3 (new):** `settle_timeout_ms` was silently ignored on the
session engine path — `Backend::set_settle_timeout` reached
`Box<dyn SessionBackend>`, whose default is a no-op (`ArtSession` only had an
inherent setter). Fixed with a trait override, plus two follow-ons the fix
exposed: a content input resolves its selector on the **content-settled bound
tree** (`SessionBackend::bound_tree`) instead of a strict wait that can never
succeed, and the step budget applies to the content window, not to the guest's
navigation settle. Reference scenario wall **3.11 s → 1.33 s**.

## 2. Acceptance evidence

* Unit: `cargo test --release --offline -p acos-verify -p acos-cli` green
  (66 + 27 verify, 90 cli bin tests and every integration suite 0 failed;
  `acos-art` with `--test-threads=1 --skip
  refuses_to_run_when_no_cgroup_can_be_created`: 14 suites, 0 failed).
  `acos-protocol` green after `UPDATE_SCHEMA=1` (`schema/scenario.json` has
  `expect_error`).
* ART (final binary, `.cache/se2b/suite/*.json`):
  * 15 gates exactly as specified (10 pass; `T03-render-wrong` and
    `w7-insets-carve-negative` `ASSERTION_FAILED`; `offscreen-pager-visible`
    `ELEMENT_OFFSCREEN`; `p11-progress-timeout` `TIMEOUT`;
    `se1-expect-change-noop` `ASSERTION_FAILED`/`expect_change`).
  * `scenarios/gaten-content-welcome.yaml` **3/3 pass**, four content negatives
    **3/3 each** with pinned codes/reasons:
    `content_settle_pixel` (region and screenshot), `ASSERTION_FAILED` (wrong
    text), `TIMEOUT`/`content-not-stable` (deadline control, also 3/3 in
    `.cache/se2b/suite3..5/`).
  * Gaten flows **5/5 pass**; their negatives **`ASSERTION_FAILED` 5/5**; GR4
    (support/kanji/recognition/backstack) pass and their 4 negatives
    `ASSERTION_FAILED` (previous binary; later deltas are content-mode only).
* Content settle p50 **584 ms** on the Gaten welcome screen (launch 504, tap
  674; `window_ms:300`, 31–70 proven fresh traversals) — lower than the
  pre-review 740 ms because traversals are tree-only and the guest navigation
  settle no longer burns the content budget.
* A/B (5 interleaved rounds, main arm `…/.cache/wt-abmain2` `.cache/ab-target`,
  `--reps 1 --groups "a:latency:8G:300" --latency-reps 12 --no-build`):
  M4 −0.1 %, M5 −2.0 %, M6 −0.3 %, M7 0.0 %, M8 counter −0.2 %, M8 form
  +0.07 % — every strict metric inside +5 %/2 ms. Raw:
  `results/bench/baseline-20261114T14*.json`, `…T15*.json`.

## 3. Remains / decisions

* **Geometry is not hashed** — the deliberate deviation from the review's
  wording, forced by measurement (see finding 5). A bounds claim on a
  content-settled frame is `ASSERTION_FAILED`/`content_settle_geometry`.
  Selector bounds matching still reads the tree, but a miss fails closed.
* The positive scenario's post-tap assertion uses `"Skip"` (present on every
  onboarding page) because page numbers auto-advance.
* The *changing-signature* deadline path is unit-tested; no shipped fixture
  changes its tree content forever, so the ART control exercises the same
  deadline with a 1 ms budget on the counter fixture.
* Serve-side content assert now costs a full content settle (~0.5–0.7 s per
  assert) — correctness over latency; a per-session cache keyed by frame is a
  possible follow-up.
* Untested on this pass: the MCP/`acos serve` surface end-to-end (unit-level
  decisions are covered); `acos-bench` P11/scenario cohorts were not re-run.
