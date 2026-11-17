# REL3A fixes — review findings (branch `rel3a-fixes`, base `rel3a` @ `976cd47c`)

Review verdict was MERGE-AFTER-FIXES. This file records each finding, the fix, the
test that fails before it, and the evidence on this branch.

## 1. HIGH — `observation_settle` was sticky (false pass into pixel evidence)

**Was:** `ArtSession::set_observation_settle` set session state that nothing reset.
`wait_for` could fall back to `settle: content`, and the next `tap` (fast/strict)
left the session in content mode. `ArtSession::frame()` then content-settled the
following `capture`/`assert_region`, so pixels that were still animating were
reported as a pixel assertion.

**Fix (scoped, never sticky):**

* `crates/acos-verify/src/executor.rs`: a new `ObservationScope` RAII guard sets the
  rule for exactly one observation and restores `Strict` in `Drop`, on every exit
  path (early `return` included). Both observation loops (`wait_for` and
  `assertions`) use it. `execute_step` also sets `Strict` before every step, so
  `launch`/`capture`/`assert_screenshot`/`assert_region`/`reset` can never inherit
  a previous step's content rule.
* `crates/acos-cli/src/engine/art.rs`: `frame()` is now one-shot — it consumes
  `content` and restores `strict` before running the command. `ArtSession::capture`
  refuses a frame whose bound settle record is `mode: content`
  (`details.reason: content_settle_pixel`, no frame handed back).
* `crates/acos-cli/src/engine/serve.rs`: every serve verb starts by resetting the
  observation rule to `strict`; the `art.rs` comment now states the real
  behaviour (serve never asks for a content observation).

**Tests (fail-before shown by temporarily restoring the sticky code):**

* `crates/acos-verify/src/executor.rs::rel3a_settle_tests::a_content_observation_does_not_leak_into_the_next_capture`
  — `wait_for` (content fallback) → `capture`: the capture must observe under
  `strict` and must not pass on the animating fixture. With the sticky code
  restored it fails: `a capture after a content wait must use the strict rule,
  not inherit content`.
* `crates/acos-cli/src/engine/art.rs::tests::a_content_settled_frame_is_refused_as_capture_evidence`
  — `content` settle record → refusal with `content_settle_pixel`;
  `strict`/absent records are not refused.
* Real ART scenario `scenarios/rel3a-content-sticky.yaml` (`expectation: non_pass`):
  `tap progress_start settle:fast` → `wait_for` (auto content fallback) →
  `tap rapid_inc settle:fast` → `assert_region`. The bound-settle ledger is
  `fast`, so the only way the final region claim could pass is the sticky
  observation rule. It now ends `timeout` at the step 4 strict capture:
  `TIMEOUT: capture: strict settle timed out (traversal); the step is a non-pass
  because the screen was still changing when the deadline elapsed`.

## 2. MEDIUM — a content pass needs two observations

`wait_for` and assertion polling now require the condition to hold on **two
consecutive content-settled observations at least one content window apart**
(`acos_verify::settle::content_window_ms(None)`, 300 ms). This applies to the
automatic strict→content fallback and to an explicit `settle: content`; a poll
that is unsatisfied or unsettled clears the pending confirmation. The bound-frame
shortcut of an assertion no longer bypasses this when `timeout_ms` is present.

Residual hazard documented in `docs/AGENT_GUIDE.md` (Settle modes): a stalled JS
thread / paused renderer / not-yet-updated loading state can look stable for a
whole window; wait for a post-transition marker (destination screen id/text or a
ready flag), not the start button/spinner. Two observations reduce the window,
they cannot eliminate it.

**Test (fails before):**
`rel3a_settle_tests::a_single_content_observation_does_not_pass_a_wait` — a
condition that holds only on the first content sample and then stops must not
pass (pre-fix executor returned that first sample as a pass; shown by temporarily
restoring `confirmed = true`).

Updated `crates/acos-verify/tests/scenarios.rs::wait_for_keeps_polling_through_unsettled_polls`:
the condition appears on poll 3, but the content verdict is confirmed on a later
poll (`polls >= 4`) instead of passing on the first sample. This strengthens the
old exact-count assertion rather than weakening the wait semantics.

## 3. MEDIUM — never label an observation `mode: content` without a backend record

`executor.rs::settle_record_of` with no backend settle record now labels an
asked-for content observation `mode: strict`, `settled: true`,
`fallback_unconfirmed: true`, `requested_mode: content`,
`reason: fallback-unconfirmed`. A content label always comes from a backend
record that measured it.

**Test (fails before):**
`rel3a_settle_tests::an_unconfirmed_content_observation_is_labelled_strict`.

## 4. LOW — `settle` on an assertion takes effect only with `timeout_ms`

Chosen documentation option (no default latency added): without `timeout_ms` an
assertion still evaluates the frame the previous step bound, one observation, and
its settle record reports the rule that frame was bound with. `docs/AGENT_GUIDE.md`
("Assertions and `timeout_ms`") now says this explicitly and tells the caller to
spell out `timeout_ms` to use `settle: content` or to wait.

## 5. LOW — `docs/ERROR_CODES.md`

Added the live-channel `TIMEOUT` / `details.reason: settle_timeout` row: delivered
facts stay in `error.details.delivery` and `result`; the reply is never a plain
pass; the scenario shape is `status: timeout` with error class `infra_timeout`.

## Acceptance evidence (branch `rel3a-fixes`)

Test environment as in the common brief (`ACOS_FONT_DIR`, `.cache/boot/android-34`,
`.cache/art`, `.cache/gaten`, hardlinked `fixtures/build`).

| Check | Result | Evidence |
|---|---|---|
| `cargo test --offline -p acos-verify -p acos-protocol -p acos-mcp` | **PASS** | acos-verify lib 100, scenarios 28; acos-protocol 71 + 3 + 7 + 8 + 4 + 2; acos-mcp 82 + 12 (guide) + 1 (docs) + e2e + 4 + 3 + 1 + 1 + 8 + 1 + 1 + 20 + 1 + 1 + 3; 0 failures. `acos` debug binary was built in the same `CARGO_TARGET_DIR` for the e2e test. |
| `cargo test --offline -p acos-cli --test rel3a_settle_status` | **PASS** | 3 passed |
| `cargo test --offline -p acos-cli --bins` | **2 pre-existing failures** | 183 passed; `view::tests::{frames_and_events_stream_and_close_the_upstream_with_the_last_browser, the_listing_hides_stale_sessions_and_reports_metadata}` — identical failures on `rel-030` per `results/handoff/REL3A/REPORT.md`; untouched by this change. New `a_content_settled_frame_is_refused_as_capture_evidence` passes. |
| `scenarios/rel3a-content-settle.yaml` | **PASS** | `results/handoff/REL3A/FIXES-artifacts/content-settle/verdict.json`; `verify: pass`, 6 steps; steps 3–5 report `settle.mode: content`, `settled: true`. |
| `scenarios/rel3a-content-pixel.yaml` | **non-pass (expected)** | `.../content-pixel/verdict.json`; step 4 `content_settle_pixel` — content stability still never backs pixels. |
| `scenarios/rel3a-content-sticky.yaml` | **non-pass (expected; bug was a pass)** | `.../content-sticky/verdict.json`; step 4 `TIMEOUT capture: strict settle timed out (traversal)`; steps 1/3 are `mode: fast`, step 2 `mode: content`. |
| `crates/acos-mcp/tests/guide.rs` | **PASS** | 12 passed |

ART command shape (all correctness runs; no timing numbers claimed):

```
ACOS_FONT_DIR=/home/Qui/Projects/acos/.cache/boot/android-34/fonts \
env -u ACOS_HOME tools/art-runtime/run-exclusive.sh --task REL3A-FIXES --exclusive -- \
  tools/guarded.sh --mem 12G --time 600 -- \
  .cache/rel3a-fixes-target/debug/acos verify --engine art \
    --scenario scenarios/<name>.yaml \
    --artifacts results/handoff/REL3A/FIXES-artifacts/<name> \
    --required-fidelity art-reference --overwrite
```

`--exclusive` was used for the two later scenarios because a shared
`run-exclusive.sh` acquisition waited >10 min twice (benchmark slots), which the
common rules allow for correctness runs; no timing was measured or claimed.

## Notes / follow-ups

* `cargo build` used `CARGO_TARGET_DIR=$WT/.cache/rel3a-fixes-target`; it must be
  deleted before the task is closed (not committed).
* The pre-existing `acos-cli --bins` view test failures and the M5 observe
  `frame.md5` defect (REL3A criterion 7) are unchanged and out of scope here.
