# SE1 handoff — `expect_change: true` (ADR-0026 decision 2), and GR4 Backlog G4c

Branch `settle-se1-expect-change` (worktree `/home/Qui/.paseo/worktrees/3oxa6xao/gr4`,
base `main` `3e11d9c`). Track SE1. Date 2026-11-14.

## 1. What shipped

Strict settle keeps the P12 rule: a step whose action drew but changed no pixel
still settles and passes, and the reply/step evidence keep reporting
`settle.pixels_changed: true|false` (default `false` = unchanged behaviour).

New opt-in per step: **`expect_change: true`** on the input steps `tap`, `type`,
`key` and `back`.

* after the strict settle, `pixels_changed:true` → pass, as before;
* `pixels_changed:false` → step **fails** with `ASSERTION_FAILED`,
  `details = {reason: "expect_change", pixels_changed: false, step_kind,
  before: {frame_seq, frame}, after: {frame_seq, frame}}`;
* no measured `pixels_changed` (backend without the evidence, or fast) →
  `UNSUPPORTED_ASSERTION`, never an assumed change;
* `expect_change: true` + `settle: fast` → `INVALID_SCENARIO` at
  `steps[i].<kind>.expect_change` (parse/validation time, before any action);
* non-bool → YAML/format parse error (`INVALID_SCENARIO` at `$`).

Code choice: **no new `ErrorCode`.** The codebase convention is one code per
outcome class plus `details` for the situation (`ELEMENT_OFFSCREEN`,
`INSUFFICIENT_HOST_EVIDENCE`, `assert_region.text_ocr` →
`UNSUPPORTED_ASSERTION`). An `expect_change` miss *is* an assertion that ran and
did not hold, so `ASSERTION_FAILED` + `details.reason` is the existing home;
`docs/ERROR_CODES.md` now says so explicitly.

Files:
* `crates/acos-protocol/src/scenario.rs` (+ `schema/scenario.json`): the flag on
  `TapStep`/`TypeStep`/`KeyStep`, `BackStep` (was `EmptyStep`, so `- back: {}`
  still parses), `validate_expect_change`, 3 new unit tests.
* `crates/acos-verify/src/executor.rs`: `expect_change_failure` (runs on the
  action's own settle record, keeps the observation as evidence, never
  overrides an earlier failure).
* `crates/acos-verify/tests/scenarios.rs`: 4 end-to-end tests on the
  vertical-slice backend.
* `crates/acos-cli/src/engine/serve.rs`: `expect_change` on `acos_tap`,
  `acos_key`, `acos_text` (and forwarded into `acos_action`), with
  `reject_fast_expect_change` before the input is injected and
  `check_expect_change` after it. `crates/acos-mcp/src/descriptors.rs`: the same
  flag documented in the three tool schemas (the generated `docs/MCP.md` table
  lists only required args, so it does not change).
* `docs/AGENT_API.md` §9 step table + new §9.5; `docs/ERROR_CODES.md`
  (`ASSERTION_FAILED`, `UNSUPPORTED_ASSERTION` rows).
* New gates `scenarios/se1-expect-change-tap.yaml` (pass) and
  `scenarios/negative/se1-expect-change-noop.yaml` (`ASSERTION_FAILED`).
* `EXPERIMENTS.md` E13.10.

## 2. Evidence (all under one ART lock per batch, `--engine art`, animations on)

`.cache/se1/suite/*.json` (verdict summariser `.cache/se1/verdict.py`,
runner `.cache/se1/run_suite.sh`, binaries `.cache/gr4-target/release/`):

| run | result |
|---|---|
| `se1-expect-change-tap` | **pass**, 5 steps (counter `inc` tap really moves 0→1) |
| `negative/se1-expect-change-noop` | **fail `ASSERTION_FAILED`** at step 2 (the `tap`) |
| `v2-art-counter` | pass 11/11 |
| `w22-counter-reset` | pass 9/9 |
| `w54-dialog-pixels` | pass 14/14 |
| `w42-back-frame` | pass 17/17 |
| `w15-dialog-menu` | pass 3/3 |
| `form-validation` | pass 28/28 |
| `suite/T03-render-good` | pass 5/5 |
| `v1-offscreen-pager` | pass 9/9 |
| `suite/T03-render-wrong` | fail `ASSERTION_FAILED` |
| `negative/offscreen-pager-visible` | fail `ELEMENT_OFFSCREEN` |
| `p11-progress-timeout` | non-pass `TIMEOUT` |
| Gaten `home/tabs/settings/search/lesson` (fixture APK f344272) | pass 14/22/33/22/28 |
| Gaten five wrong twins | fail `ASSERTION_FAILED` at 7/7/12/16/10 |

Byte-identical check vs main: a script compared status, verdict, failure code and
**every step's** (index, kind, status, code) between `.cache/se1/suite/*.json` and
the main-side run in `/home/Qui/Projects/acos/.cache/wt-int/.cache/int/suite/*.json`
(11 gates + 10 Gaten runs): **identical=True for all 21**. `w7-insets-report` was
not run (known main-side red, owned by W7B).

Crate tests (all green, guarded):
`cargo test --release --offline -p acos-protocol` (incl. 3 new),
`-p acos-verify` (incl. 4 new), `-p acos-cli` (87 lib + suites, 4 ignored as
before), `-p acos-services`, `-p acos-art -- --test-threads=1 --skip
refuses_to_run_when_no_cgroup_can_be_created` (then that test alone with
`--exact`: `refuses_to_run_when_no_cgroup_can_be_created ... ok`).

## 3. G4c (GR4 Backlog) — how it is addressed

G4c: "a scenario can pass 3/3 while proving nothing, and nothing in the harness
notices" — GR4's `gaten-kanji` v1 tapped RTK while the value already was RTK
(`KanjiReferencePreference.select()` returns early), so the post-tap assertions
held on unchanged state; only a hand byte-diff of the before/after captures
found it. GR4's candidate was "an explicit step (`expect_change` /
`assert_frame_differs`)". SE1 ships exactly that candidate, measured on the real
engine: put `expect_change: true` on the mutating tap and a vacuous flow fails
with `ASSERTION_FAILED` (before/after frame refs attached) instead of passing.
The legitimate no-op stays expressible: omit the flag (P12 behaviour, pass, with
`pixels_changed:false` in the step's `settle` evidence). GR4/GR3 scenario
authors should add the flag to the tap of every claim whose point is "this
action changes the screen"; nothing in the format changed for existing files.

## 4. Known limits / not done

* The negative gate twin uses the *drawing but pixel-identical* case (focus the
  empty `Name` field). A tap on a blank non-interactive area does not draw at
  all, so it is a strict-settle `TIMEOUT` (`reason: no-draw-after-action`), not
  an `expect_change` failure — probed on the counter fixture and recorded. Both
  are non-passes; there is no way to make "no interaction at all" settle.
* `expect_change` is not accepted on `rotate`/`launch`/`reset`/environment steps
  (they have no strict settle record of their own to measure).
* The serve/MCP path is compile-verified and string-verified in the release
  binary and covered by the shared rule function, but not exercised by a live
  serve session in this track (no serve round-trip was run); the scenario path is
  the measured one.
* Host incident during the run (not caused by this track): at 14:06–14:07 the
  worktree's ignored files were wiped by another process (`fixtures/build`,
  `fixtures/reference*` and all of `.cache/`, including the release binaries and
  the scratch runner). They were recreated as symlinks to the main checkout and
  everything above was re-run afterwards; nothing in the commit depends on them.
  Worth a host-wide check for whatever cleans worktrees while agents run.
