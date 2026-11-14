# FB4 — a run owns its artifacts; capture names, assertion echo, human selectors, verdict `status`

**Branch:** `fb4-artifacts` (off `main` 74e4ee9 = v0.2.0)
**Worktree:** `/home/Qui/Projects/acos/.cache/wt-fb4`, target `.cache/fb4-target`
**Source:** `ACOS_AGENT_FEEDBACK.md` (second subagent "New items" 1–5, first subagent
items 7 and 11, Triage FB4)
**Date:** 2026-11-15

Every ART run went through `tools/art-runtime/run-exclusive.sh --task FB4` +
`tools/guarded.sh`; builds used `tools/guarded.sh --mem 14G --time 1500`.

## 1. What shipped

### (1) One run owns its `--artifacts` directory

Chosen rule: **refuse, with an explicit `--overwrite`**. Rejected alternative: nest
per run (`<dir>/<run-id>/` + `latest`). Reasons:

* the layout is a published contract (`verdict.json` at the directory root, `repro.sh`
  replays with `--artifacts .`, `check-artifact-references`, `acos artifacts gc`,
  gate runners). Nesting silently breaks every reader that looks at
  `<dir>/verdict.json` — a reader failure is the same class of stale-evidence bug
  this item fixes;
* nesting changes the evidence path on every replay, while `repro.sh` promises the
  run that produced the verdict;
* the refusal is loud and names the previous run id, so an agent that habitually
  reuses a directory is corrected on the first attempt instead of shipping a stale
  frame. `--overwrite` is one flag when discarding evidence is intended.

Implementation:

* `crates/acos-protocol/src/artifacts.rs` — `run.json` (`RunMarker`: `request_id`,
  `started_at`, `scenario`, `apk`, `acos_version`, `overwrote_run_id`),
  `previous_run_id()`, `read_run_marker()`, `claim_run_dir(dir, marker, overwrite)`.
  A directory counts as used when it holds **any file** at any depth (a
  pre-created empty `art-frames/` is not evidence). Refusal is
  `INVALID_ARGUMENT` with `details.previous_run_id` / `details.run_id` /
  `details.artifacts` / `details.fix`.
* `crates/acos-cli/src/verify.rs` — claims the directory **before** the engine
  session starts (a refused run writes nothing and leaves the previous evidence
  byte-identical); `--overwrite` clears the contents first; the new `run.json`
  records `overwrote_run_id`, the verdict carries `artifacts_overwrote_run` and a
  note.
* `crates/acos-protocol/src/commands.rs` — `VerifyRequest.overwrite` (also the MCP
  tool schema; `crates/acos-mcp/src/backend.rs` relays `overwrite: true` →
  `--overwrite`). MCP `acos_verify` runs the CLI, so CLI, serve delegation and MCP
  share one funnel. `acos serve` has no separate verify route.
* Docs: `docs/AGENT_API.md` §10.4, §17; `docs/AGENT_GUIDE.md` CLI bullets;
  `docs/MCP.md` verify section.

Demo (real ART, same scenario twice into one dir;
`.cache/fb4-gates/run/demo-*.json`):

```text
first run   rc=1 status=fail  run=req_2f10b6_18e7fe7e6ae62bcd
second run  rc=1 status=error code=INVALID_ARGUMENT previous_run_id=req_2f10b6_18e7fe7e6ae62bcd
--overwrite rc=1 status=fail  artifacts_overwrote_run=req_2f10b6_18e7fe7e6ae62bcd
```

### (2) A `capture` step's name is persisted

* `StepResult.name` (step JSON: `"name": "art-after-inc1"`).
* PNG copy `frames/<frame_seq>-<sanitised name>.png`; `sanitize_capture_name` keeps
  `[A-Za-z0-9._-]`, collapses other characters to `-`, caps at 64 bytes, empty →
  `capture`. The named copy is the same bytes as the canonical
  `frames/<frame_seq>.png` (hardlink when the filesystem allows, copy otherwise) —
  the canonical file stays because `steps[].frame_seq` names it.
* `Verdict.captures: {name → path}`, declared as step evidence (`kind: "capture"`).
* `validate_artifacts_dir` enforces: every `captures` path exists, every named step
  appears in the map (no half-mapping), and `none-on-pass` strips names, the map
  and the capture evidence together (a report never names a removed file).

### (3) Assertions echo `expected`/`actual` on PASS

`AssertionResult` gained `expected` and `actual` (JSON, always present). Text /
visible / not_visible / state carry the selector, matcher, expected bounds and
requested flags, plus the observed node id, text/hint/bounds/flags;
log carries patterns + matched sample; screenshot/region carry baseline,
tolerance, changed fraction / region, hash, colour count; `expect_change` carries
the pixels-changed comparison. Failure still keeps `diff` (path + human detail),
and now also fills `expected`/`actual`.

Real-ART evidence (gate `v2-art-counter` pass, `steps/02-assert_text.json` in
`.cache/fb4-gates/run/v2-art-counter/`):

```json
{"kind":"text","passed":true,
 "expected":{"selector":{"id":"count"},"text":{"exact":"0"},"bounds":null},
 "actual":{"node":"n4_dev.acos.fixtures.counter:id/count","text":"0","hint":"","bounds":[42,317,63,368],"visible":true}}
```

### (4) Human selector form in messages

`impl Display for Selector` (`Selector::human()`): `text=="Nonexistent"`,
`content_desc=="X"`, `id=="pkg:id/btn"`, `class==…`, `bounds=[l,t][r,b]`,
`index=N`, `include_invisible=true`; contains/regex keep their semantics
(`text*="…"`, `text~/…/`). Every `{selector:?}` in an error/message path now uses
`{}` (assertions, offscreen/truncated/missing errors, wait_for, native backend,
engine seam). `AssertionDiff.expected` still embeds the machine selector JSON.

Observed on a real miss (`v2-art-counter` capture demo run):
`selector id=="result" matched no node`.

### (5) `verdict.json` carries `status`

`Verdict.status` is written next to `verdict`, same `Status` vocabulary as every
envelope (`pass`/`fail`/`unsupported`/`blocked`/`error`/`timeout`/`crash`). The
validator rejects a mismatch. A legacy document without `status` deserialises as
`fail` (never as a pass), and then fails the mismatch check — an old file can
never be read green by accident.

### (6) Docs: `--json` stdout only

`docs/AGENT_API.md` (intro + §17) and `docs/AGENT_GUIDE.md` (CLI bullets) now say:
with `--json`, stdout is exactly one JSON envelope; progress/warnings and the
on-failure `verify: CODE (class): message` line go to stderr, so
`--json > out.json 2>&1` is invalid JSON. A CLI test asserts both halves.

## 2. Tests (`cargo test --release --offline -p acos-protocol -p acos-verify -p acos-cli`, UPDATE_SCHEMA=1)

Green: protocol (113 lib + schemas refreshed: `commands.json`, `verdict.json`,
`verify-request.json`), verify (62 lib + 28 integration), cli (113 lib + all
integration suites, including `w46_error_taxonomy` which pre-creates an
`artifacts/art-frames` dir — covered by the "empty directories are not evidence"
rule). New tests:

| item | test |
|---|---|
| 1 | `artifacts::tests::claim_{refuses_a_directory_another_run_owns, overwrite_replaces_the_previous_run_and_records_it, accepts_an_absent_or_empty_directory, accepts_pre_created_empty_subdirectories, refuses_a_partial_run_without_a_marker}`; CLI `a_second_run_into_the_same_artifacts_dir_is_refused_until_overwrite`; MCP `verify_relays_the_explicit_overwrite_choice` |
| 2 | `artifacts::tests::capture_names_are_sanitised_and_deterministic`; `artifacts::tests::validator_rejects_a_capture_map_that_dangles_or_is_incomplete`; verify `capture_name_is_persisted_in_the_step_file_name_and_verdict_map` |
| 3 | `assertions::tests::a_passing_{text,state,not_visible}_assertion_echoes_*` |
| 4 | `ui::tests::selector_human_form_is_not_a_debug_dump`; `assertions::tests::selector_miss_messages_use_the_human_selector_form`; CLI `json_stdout_is_one_document_and_human_diagnostics_go_to_stderr` |
| 5 | `verdict::tests::status_mirrors_verdict_with_the_envelope_vocabulary`, `a_verdict_without_status_reads_as_fail_never_pass`; `validator_rejects_status_that_disagrees_with_verdict` |
| 6 | CLI `json_stdout_is_one_document_and_human_diagnostics_go_to_stderr` + docs |

## 3. Gates (15, real ART, one lock acquisition; `.cache/fb4-gates/run/results.tsv`)

| scenario | expected | observed |
|---|---|---|
| `v2-art-counter`, `w22-counter-reset`, `w54-dialog-pixels`, `w42-back-frame`, `w15-dialog-menu`, `form-validation`, `suite/T03-render-good`, `v1-offscreen-pager`, `w7-insets-report`, `se1-expect-change-tap` | pass | **pass** (10/10) |
| `suite/T03-render-wrong`, `w7-insets-carve-negative`, `negative/se1-expect-change-noop` | fail `ASSERTION_FAILED` | **fail `ASSERTION_FAILED`** |
| `negative/offscreen-pager-visible` | fail `ELEMENT_OFFSCREEN` | **fail `ELEMENT_OFFSCREEN`** |
| `p11-progress-timeout` | timeout `TIMEOUT` | **timeout `TIMEOUT`** |

## 4. Gaten flows (APK `.cache/gaten/gaten-dev-f344272-x86_64-release-unsigned.apk`)

| scenario | expected | observed |
|---|---|---|
| `scenarios/gaten-{home,tabs,settings,search,lesson}.yaml` | pass | **pass** (5/5) |
| `scenarios/negative/gaten-{home,tabs,settings,search,lesson}-wrong.yaml` | fail `ASSERTION_FAILED` | **fail `ASSERTION_FAILED`** (5/5) |

## 5. Artifact integrity

`python3 -I tools/check-artifact-references.py .cache/fb4-gates/run` →
**`26 run(s) checked, 0 dangling`** (15 gates + 5 flows + 5 negatives + the
ownership demo's final run). Each run's `retention.json` is present; passing runs
keep only declared evidence, non-pass runs keep everything (by design).

## 6. Decisions

* Ownership = refuse + `--overwrite`, not per-run nesting (justification in §1.1).
* "Used" means *holds a file*: pre-created empty directories (the `w46` harness
  creates `artifacts/art-frames`) are not another run's evidence.
* The named capture PNG is an additional evidence file, not a rename: the
  canonical `frames/<seq>.png` remains what `frame_seq` names. Hardlink where
  possible, so a capture costs no extra bytes on this filesystem.
* `status` is a required field going forward; a document without it reads as
  `fail` and fails validation rather than being silently upgraded.
* `repro.sh` keeps `--artifacts .` without `--overwrite`: replaying into the
  directory a run already filled is refused (copy the dir or pass the flag).
  Recorded in `docs/AGENT_API.md` §10.4.

## 7. Not done

* `acos serve` exposes no verify route; MCP `acos_verify` delegates to the CLI, so
  the rule is enforced at the one funnel. No separate serve-side code exists to
  change (recorded instead of inventing a route).
* The 7-scenario GR4 Gaten set (support/kanji/recognition/backstack) was not
  re-run; the required 5 flows + 5 negatives were. Smallest next action: run
  `scenarios/gaten-{support,kanji,recognition,backstack}.yaml` + their negatives
  into a fresh artifacts root if the orchestrator wants the wider set.
