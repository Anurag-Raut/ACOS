# GATEFIX — v0.3.0 release blocker, correctness side

Branch `gatefix` from `rel-030` (`5a912d4b`). Findings fixed: **F1** (content
settle published `PLACEHOLDER_CONTENT` on a quiet host), **F2** (the matrix
taxonomy had no class for `ELEMENT_OFFSCREEN`) and **F3** (the default matrix set
referenced `finish-start.apk`, which a stale `fixtures/build` could miss).

Environment: `ACOS_FONT_DIR=/home/Qui/Projects/acos/.cache/boot/android-34/fonts`;
worktree `.cache/{art,gaten,boot/android-34}` symlinks; `fixtures/build` =
`cp -al` of the main checkout; `ACOS_BOOT_DIR`/`XDG_RUNTIME_DIR`/`ACOS_HOME`
never set; target dir `.cache/gf-target` (deleted at the end). Every ART run went
through `tools/art-runtime/run-exclusive.sh --task GATEFIX` +
`tools/guarded.sh`.

## F1 — `launch` + `settle: content` declared a placeholder before the app ran

**What the rule should be (ADR-0032 amendment 1, `DECISIONS.md`).** A placeholder
is a claim about the app, so it needs the same evidence shape as every other
settle: a **window plus a guest signal**, never a count of samples.

1. The mount wait is a *change* wait for every settle rule: the launch hands the
   guest the tree it just published (`baseline_tree`) and asks with the strict
   change rule. A `fast` first traversal has no gate, and the old wait inherited
   that — eight samples of the guest's cached skeleton cost ~0 ms on a quiet
   host. The wait is now bounded by the `ACOS_ART_FIRST_FRAME_MS` window (not by
   `FIRST_FRAME_MAX_ATTEMPTS = 8`), and a guest that answers before the window
   ran out is paced (`FIRST_FRAME_PROBE_PACE_MS = 25`) and observed again.
2. `first_frame_placeholder_verdict` requires the skeleton to be stable **for the
   whole window** (`first_frame.window_ms`, `window_spent`) with no frame/pixel
   movement. Eight samples in a millisecond are not evidence of absence.
3. A wait that cannot observe the window (guest error) yields neither a
   placeholder nor a pass: the launch returns the guest's structured error
   (`TIMEOUT`/`first_frame_window_unproven` when there is none). Never a false
   placeholder, never a false pass.
4. `content_settle` itself is unchanged: content stability stays a tree rule and
   never becomes a "did it mount" rule (a text-free SurfaceView/GL screen is a
   legitimate content-stable frame). The launch's mount wait is where mounting is
   decided, because only it has the pixel evidence.

**Root cause.** `ensure_launch_tree("content")` binds the first frame with the
guest's `fast` rule and then proves content stability. In `bind_first_frame` the
wait's baseline was `if settle == "strict" { … } else { None }`, and the loop
exited after 8 traversals. On the release-gate host that was 8 cached-skeleton
replies in <1 ms → `placeholder: true`; the content settle afterwards then found
the *skeleton* content-stable (it is trivially stable) and the launch answered
`PLACEHOLDER_CONTENT` for an app that mounted ~300 ms later (2/32 matrix
negatives; they passed on a busy host where the same 8 traversals spanned the
mount).

**Fail-before / pass-after evidence.** Three new unit tests in
`crates/acos-cli/src/engine/art.rs` against the fake launcher (`fake_first_frame_art`):

| test | before the change | after |
|---|---|---|
| `gatefix_a_slow_mount_is_never_called_a_placeholder` (fake `slow-mount`: constant skeleton identity for 400 ms, then content) | 8 traversals in ~0 ms, bound the skeleton, `placeholder` → fails on "the app's content is the frame" and on the ≥300 ms wait | binds the content frame, `event_waits ≥ 1`, `!placeholder`, `!suspect` |
| `gatefix_a_real_placeholder_still_blocks_after_the_window` (fake `never`, 150 ms window) | `placeholder` with `waited_ms 0`, no window | `placeholder`, `window_spent`, `window_ms 150`, elapsed ≥100 ms |
| `gatefix_a_wait_that_cannot_observe_the_window_is_not_a_placeholder` (fake `wait-dies`) | 8 cached replies → `placeholder` | the guest's `INTERNAL` error, no report published |

`g3f1_missing_content_stays_a_labelled_placeholder` was updated to the window
rule (one launch traversal + N paced, gated observations, `event_waits ==
traversals - 1`, `window_spent`) instead of the old fixed "exactly 2".
`the_placeholder_verdict_needs_frame_evidence_not_a_node_count` gained the
"unproven window is neither" case (`frame_evidence(…, window_spent=false)`).

**ART evidence on the real app** (`art-scenarios/`, release binaries,
`--engine art`, artifacts kept):

| scenario | before (release-gate README, `01-matrix-postreboot`) | now |
|---|---|---|
| `negative/gaten-content-assert-screenshot` | step 0 `PLACEHOLDER_CONTENT` (`harness`) | launch pass 445 ms, step 1 `ASSERTION_FAILED` / `content_settle_pixel` |
| `negative/gaten-content-selector-bounds` | step 0 `PLACEHOLDER_CONTENT` (`harness`) | launch pass 426 ms, step 1 `ASSERTION_FAILED` / `content_settle_geometry` |
| `scenarios/gaten-content-welcome` (positive) | pass | pass, all four steps `content-stable` (300 ms window) |

## F2 — `ELEMENT_OFFSCREEN` had no class in the matrix taxonomy

`tools/benchlib.py` listed `ASSERTION_FAILED`, `ELEMENT_NOT_FOUND`, `STALE_REF`,
`WRONG_TEXT`, `WRONG_BOUNDS` but not the protocol's other
`default_status() == Status::Fail` codes. `negative/form-wrong-error` and
`negative/offscreen-pager-visible` are *correct negatives* that fail with
`ELEMENT_OFFSCREEN`, and the missing class made them `unclassified` — the run was
INVALID at `false_pass = 0`.

Fixed at the class boundary: the assertion block in `_CLASS_BY_CODE` now mirrors
the protocol's `Status::Fail` set (`ELEMENT_OFFSCREEN`, `AMBIGUOUS_SELECTOR`,
`TREE_TRUNCATED`, `END_OF_LIST`, `INPUT_NOT_HANDLED` added). An expected negative
that fails with one of them is a `correct_negative`, never `unknown`.
`tools/tests/test-benchlib.py` gained `test_an_assertion_outcome_is_never_unclassified`
(including the matrix-row validation) and `test_every_protocol_assertion_code_is_classified`,
which parses `crates/acos-protocol/src/error.rs`'s `=> Status::Fail` arm and fails
if any of those codes is not `assertion` in benchlib — a future code cannot
silently become `unknown` again. **Teeth**: remove `"ELEMENT_OFFSCREEN"` from the
table and `test_every_protocol_assertion_code_is_classified` reports
`['ELEMENT_OFFSCREEN']`.

## F3 — the default matrix set needs `finish-start.apk`

`tools/build-apps.sh` already builds `finish-start` (its output list did not say
so; fixed), but `tools/run-matrix.sh` never built or checked fixtures, so a
`fixtures/build` copied from another worktree made
`negative/finish-start-wrong-home` an `APK_IO` row. `run-matrix.sh` now resolves
every selected scenario's `app:` under `fixtures/build/apps/`, builds the missing
ones with `ACOS_APP_ONLY=<names> tools/build-apps.sh` (guarded), re-checks them,
and refuses with an actionable message for a fixture that has no
`fixtures/apps/<name>` source (e.g. `hostile-*.apk`, built by
`fixtures/build-hostile-probe.sh`). Documented in `tools/run-matrix.sh`,
`tools/build-apps.sh` and `fixtures/README.md`.

Evidence in `02-matrix/run-matrix.log` (the fixture was deleted before the run):

```
run-matrix: building missing fixture APK(s): finish-start
build-apps: [finish-start] -> .../fixtures/build/apps/finish-start.apk (12823 bytes)
```

and the row: `03-negative-finish-start-wrong-home-r1  negative  fail  assertion
ASSERTION_FAILED` (was `error / unknown / APK_IO`).

## Harness: `--allow-contaminated`

The host was busy for most of this track (`loadavg1` 5–11 from other agents), and
there was no documented override for `run-matrix`'s host gate. Added
`--allow-contaminated` (or `ACOS_MATRIX_ALLOW_CONTAMINATED=1`): when the gate
refuses it continues for **correctness classification only**, writing
`verdict_eligible: false` on every row, in the summary and in the provenance, so
no timing number can be quoted from it. While wiring that, a latent benchlib bug
was fixed: `benchlib.provenance` recorded `host_gate.ok: false` but ignored it
when stamping `verdict_eligible` (the run's summary said `false`, its provenance
said `true`). A refused gate now makes the provenance ineligible, with the
refusal reason recorded
(`test_a_refused_host_gate_makes_the_provenance_ineligible`).

## Matrix re-run

Release binaries, `tools/run-matrix.sh --no-build --profile release
--target-dir .cache/gf-target`, default 33-row scenario set.

**Authoritative run — `02-matrix-quiet/`** (host gate passed: `loadavg1 2.61`,
MemAvailable 45.6 GiB):

```
run-matrix: host gate ok (loadavg1 2.61, MemAvailable 45.6 GiB)
summary {"classes": {"assertion": 29, "infra_timeout": 3, "pass": 1},
         "false_pass": 0, "false_fail": 0, "negative_runs": 32,
         "negative_correct": 29, "unclassified": 0, "harness_or_timeout": 3,
         "verdict_eligible": true, ...}
run-matrix: verdict_eligible=True profile=release engine=art
```

`matrix.json` (not `.invalid.json`) exists → **VALID**: `false_pass = 0`,
`unclassified = 0`. `harness` rows are now 0 (were 2). The two gate findings are
fixed rows: `04-negative-form-wrong-error-r1 fail assertion ELEMENT_OFFSCREEN`,
`25-negative-offscreen-pager-visible-r1 fail assertion ELEMENT_OFFSCREEN`,
`03-negative-finish-start-wrong-home-r1 fail assertion ASSERTION_FAILED`,
`06/08-gaten-content-* fail assertion ASSERTION_FAILED` (were `harness`
`PLACEHOLDER_CONTENT`).

The three remaining `infra_timeout` rows are the **pre-existing** ones
(`negative/gaten-content-window-impossible`, `negative/multi-wrong-after-back`,
`negative/offscreen-pager-wait`): the release-gate README records identical
timeouts on the **v0.2.2** binary (`04-scenarios-baseline022.log`), so they are
not part of this track. They keep `harness_or_timeout_within_2pct` and
`negative_all_correct` false in the summary; neither is a `validate_matrix`
gate (the gate is `false_pass == 0` and `unclassified == 0`).

`02-matrix/` is the same run taken earlier under `--allow-contaminated` while
other agents held the host (`loadavg1 10.95`): identical classes and gate
results, `summary.verdict_eligible: false`, provenance eligibility `true`
because the benchlib provenance bug above was only fixed afterwards (its
`provenance.host_gate` block records `ok: false`). `03-fixed-rows/` is a
5-row verdict-eligible run of exactly the five rows F1/F2/F3 touched (host gate
passed): all five `fail`/`assertion`, `false_pass 0`, `unclassified 0`.

## Acceptance

* `cargo test -p acos-art -p acos-verify -p acos-protocol -p acos-cli -p acos-mcp`
  — green: `01-cargo-test.log` (69 `test result: ok` suites, 0 failed). The 15
  ignored tests are the pre-existing `#[ignore]`s that boot a real ART guest
  (`run under tools/guarded.sh [+ run-exclusive.sh]`) plus one measurement test;
  none is a skipped assertion.
* `python3 tools/tests/test-benchlib.py` — `258 passed, 0 failed`.
* Matrix VALID: `02-matrix-quiet/matrix.json`, `false_pass: 0`,
  `unclassified: 0`, `verdict_eligible: true`.
* Targets deleted: `.cache/gf-target` (2.8 GiB), `.cache/gf-smoke`, the matrix
  per-run artifacts (frames/trees pruned; `matrix.json`, `matrix.tsv` and the
  logs are kept), `fixtures/build` (the `cp -al` copy).

## What is left

The three pre-existing `infra_timeout` negatives and the timing regressions in
the release-gate README (M8 tap→settled, N=10 per-session memory) are outside
this track. Nothing else was changed.
