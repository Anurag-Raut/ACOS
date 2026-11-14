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

---

# SE2c — second (fresh) review pass: MERGE-WITH-FIXES, all three fixed

Worktree `/home/Qui/.t3/worktrees/acos/t3-d41b3b08-5ea0b20149a5a9a9`, branch
`settle-se2-content-stable`, base `04292df` (SE2b) merged with `main`
`c517a76`. Scratch `.cache/se2c`, target `.cache/se2c-target`. Same EXPERIMENTS
number: **E13.16c**.

## 1. Findings, fixes, tests

| # | Finding | Fix (files) | Test (PASS) |
|---|---|---|---|
| 1 | **HIGH** `content_settle_geometry_failure` checked only the assertion's own `bounds`; `Selector::matches_node` also compares `selector.bounds` exactly (un-hashed), so `assert_not_visible {selector: {text: "Error", bounds: [0,100,200,140]}}` on a 1 px-drifting banner → "node absent" → **pass** while the banner is visible; the positive side (`assert_visible`/`assert_text`/`assert_state` with `selector.bounds`) passed on a single drifting frame | every assertion kind that takes a selector is enumerated (`AssertText`, `AssertVisible`, `AssertNotVisible`, `AssertState`) and refuses `assertion.bounds` **or** `selector.bounds`; details name the claiming position (`bounds_claim.{assertion,selector}`); hint says so (`acos-verify/src/executor.rs`) | `executor::content_settle_tests::selector_bounds_is_a_geometry_claim_on_every_assertion_kind` (one case per kind, plus the no-rectangle cases stay allowed); ART `scenarios/negative/gaten-content-selector-bounds.yaml` → `ASSERTION_FAILED`/`content_settle_geometry` **3/3** |
| 2 | **MEDIUM** content-mode taps aimed at the previous step's cached tree (`bound_tree()`), whose `bounds` the signature does not hash → a layout shift between steps lands the tap on a neighbour | content inputs resolve on **ONE fresh `fast` traversal** with the one-shot `min_probe_seq` floor, no quiet window, no cached tree (`SessionBackend::fresh_input_tree` → `ArtSession::fresh_input_tree`, `engine::point_for`); a reply without `probe_seq` fails closed (`NO_FRESH_FRAME`/`content_resolution_unproven`). The `bound_tree` trait seam is removed so the cached route cannot creep back (`engine/mod.rs`, `engine/art.rs`) | `engine::engine_tests::a_content_input_resolves_its_target_on_a_fresh_traversal` — fake whose cached rect is 200 px from every fresh traversal: before the fix the recorded tap is `(157,431)` (the stale/neighbour rect), after `(157,631)`; a strict input still resolves on `tree()` |
| 3 | **LOW** a content launch reply carried the pre-settle skeleton tree built by `ensure_launch_tree` | the content path returns the **content-settled** tree and re-describes its `first_frame` evidence with it (`FirstFrameReport::describes_tree`), in `ensure_launch_tree` and in the session `launch` reply, so `nodes`/`placeholder`/`frame.meaningful` describe the published tree | `engine::art::tests::a_content_launch_report_describes_the_tree_the_reply_publishes` |

**Teeth, proven on ART for the HIGH finding:** with the guard reverted, the new
negative's `assert_not_visible` step **passes** on the content-settled Gaten
frame (`.cache/se2c/suite-prefix/prefix-selector-bounds.json`: `step launch pass`,
`step assert_not_visible pass`; only the `expect_error` pin turns the run into
`INTERNAL`, i.e. the harness catching a false pass). With the fix: 3/3
`ASSERTION_FAILED`/`content_settle_geometry`.

**Recorded as asked — strict timing change (from SE2b):**
`SessionBackend::set_settle_timeout` now forwards `settle_timeout_ms` on the
**strict** path too (the trait default used to swallow it, so every scenario's
declared settle budget was silently ignored). Strict steps now honour a declared
budget where they previously used the engine default. This is in E13.16b/E13.16c
as well.

**Test-infra fix (environmental):** in this worktree the W50 serve test's socket
path is exactly 108 chars, one over `sun_path`'s limit (`path must be shorter
than SUN_LEN`), so `an_unknown_session_is_not_routed_to_the_frozen_rust_vm`
could not start its daemon. `tests/w50_serve_art.rs` now falls back to a shorter
socket name in the same scratch dir. No assertion changed; the refusal was
reproduced directly (`acos serve --socket <108-char path>`).

## 2. Acceptance evidence (final binary)

* `cargo test --release --offline -p acos-verify -p acos-cli -p acos-protocol`
  green: 77 + 118 unit tests, every integration suite 0 failed (incl. the
  previously-red `w50_serve_art`, now 3 passed / 1 ignored). Schemas unchanged →
  no `UPDATE_SCHEMA`.
* ART (`.cache/se2c/suite/*.json`), one ART-lock acquisition per batch:
  * content positive `gaten-content-welcome` **3/3 pass** — launch content
    settle 446/456/505 ms, post-tap 703/666/670 ms, `window_ms:300`,
    20–25 / 61–65 proven fresh traversals;
  * content negatives **3/3 each**: `selector-bounds` `ASSERTION_FAILED`/
    `content_settle_geometry`, `pixel` + `assert-screenshot` `ASSERTION_FAILED`/
    `content_settle_pixel`, `wrong-text` `ASSERTION_FAILED`,
    `window-impossible` `TIMEOUT`/`content-not-stable`;
  * 15 gates exactly as expected (10 pass; `T03-render-wrong`,
    `w7-insets-carve-negative`, `negative/se1-expect-change-noop`
    `ASSERTION_FAILED`; `negative/offscreen-pager-visible` `ELEMENT_OFFSCREEN`;
    `p11-progress-timeout` `TIMEOUT`);
  * 5 Gaten flows **pass 5/5**; their 5 negatives **`ASSERTION_FAILED` 5/5**.
* Latency: the content tap's wall rose 674 ms → 703/666/670 ms (**+~30 ms**, the
  one extra resolution traversal) — the price of acting on a target that is
  provably not stale. No strict metric was re-benched: the only strict-path
  change in this pass is the removal of a dead trait default, and the 15 strict
  gates are unchanged.

## 3. Remains / decisions

* Geometry in **either** position (`assertion.bounds`, `selector.bounds`) is
  refused on a content-settled frame; there is no opt-out. A caller who needs a
  rectangle must settle strictly.
* `wait_for`/`tap`/`type` are not refused: they match against a traversal they
  take themselves (and a content input's traversal is now floored/proven fresh).
* Unchanged from SE2b: serve-side content assert costs a full content settle
  (~0.5–0.7 s), the changing-signature deadline is unit-tested only (no shipped
  fixture changes its content forever), and the MCP surface is covered at the
  unit level rather than end-to-end.
