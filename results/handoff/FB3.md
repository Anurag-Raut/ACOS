# FB3: input actions return the settled tree diff (`changes`)

**Status: DONE**, with one design decision recorded below (ART has no swipe).
**Branch:** `fb3-act-diff` (base `05ba66b`). **Commits:** `0ed96bf` (previous agent's feature,
reviewed here), `57c3c91` (review fixes, tests), plus the commit that adds this file, its
evidence and the descriptor wording change.
**Source:** `ACOS_AGENT_FEEDBACK.md` entry 1, item 5.
**Scratch and evidence:** `results/handoff/FB3-evidence/`. Scratch lives in `.cache/fb3/`.
Every ART run went through `tools/art-runtime/run-exclusive.sh --task FB3 -- tools/guarded.sh`.
Every MCP smoke was pinned to the scratch install with `ACOS_BIN=<prefix>/bin/acos` and
`env -u ACOS_HOME`.

## 1. What the feature does

`acos_tap`, `acos_text` and `acos_key` (MCP), `acos_action {kind: tap|text|key}`, and the same
verbs through `acos serve`/the CLI daemon return `result.changes`: the settled tree diff of the
action against the tree the session held before it.

* **Default on.** `diff: false` returns the pre-FB3 reply with no `changes` key.
* **Identity** is the stable id path (`resource_id`, or `class#N` for unnamed siblings), never
  list position. A node that really moves is reported as removed + added.
* **Compared fields:** text, content_desc, hint, class, resource_id, bounds, visible_bounds,
  visible, enabled, focused, clickable, scrollable, checked, selected. A changed node lists
  `fields: {name: {old, new}}`.
* **Bounded:** each of `added`/`removed`/`changed` is capped at 40 entries. `counts` holds the
  true totals and `truncated: true` says a list was cut.
* **No cost beyond the comparison.** The "before" tree is the frame the session already holds,
  snapshotted in `begin_input`. No traversal, no capture, no guest round trip. With no
  pre-action tree the reply is `{"baseline": "none"}` with empty lists.
* `focused` appears only when a node has focus, so it is absent on the counter fixture.

Contract: `docs/MCP.md` §1c. Descriptors: `crates/acos-mcp/src/descriptors.rs`.

## 2. Decision: `acos_swipe` on ART

`acos_swipe` answers `UNSUPPORTED_BACKEND` (`input.swipe`) on the ART engine, because ART has no
swipe command. It therefore returns no `changes` there. The `diff` argument is documented as
"applies when the engine supports the action". No live check was run for swipe, because it
cannot be exercised on ART.

## 3. Changes made in this pass (on top of `0ed96bf`)

Reviewed `0ed96bf` and fixed what was wrong:

1. **Mangled source.** The FB3 edit had damaged two lines (`impl CachedFrame {` and a test
   header in `serve.rs`) and left a stray blank line. Restored.
2. **Identity matching was O(n·m).** Each node scanned the other tree. It now uses a hash index
   (`diff.rs`). Output is unchanged.
3. **Missing unit tests.** Added pure-addition, pure-removal, and removed/changed caps with true
   counts. The earlier suite covered add+remove together and capped only the added list.
4. **Descriptor and docs wording** for `acos_swipe` (decision in §2).

Not changed, and recorded for the orchestrator: `0ed96bf` also repairs `acos-verify`, which did
not compile at `05ba66b` (three `AssertionResult` initializers missing `expected`/`actual`). The
base-arm build used only that repair.

## 4. Unit tests

`cargo test --release --offline -p acos-cli -p acos-art -p acos-mcp -p acos-verify -p acos-protocol`
(log: `FB3-evidence/tests-final.log`, run at `57c3c91`):

| result | count |
|---|---|
| passed | **698** |
| failed | **0** |
| ignored | 6 (present at `05ba66b`; this branch adds no `#[ignore]`) |

The earlier count on `0ed96bf` was 695 passed (`tests-baseline.log`). The 3 new tests account for the difference.
The `acos-mcp` tests were re-run after the descriptor wording change: **102 passed, 0 failed**
(`tests-mcp-swipe.log`).

Coverage of each required behaviour:

| behaviour | test |
|---|---|
| change | `diff::tests::one_text_change_is_the_only_change` (counter `0 → 1`, exactly one change); `flag_and_bounds_changes_carry_old_and_new` |
| add | `diff::tests::pure_addition_reports_only_added` (new) |
| remove | `diff::tests::pure_removal_reports_only_removed` (new); `added_and_removed_nodes_are_matched_by_id_path_not_position` |
| truncate | `diff::tests::lists_are_capped_and_say_so`; `removed_and_changed_lists_are_capped_with_true_counts` (new) |
| no baseline | `diff::tests::no_baseline_invents_nothing` |
| identical trees | `diff::tests::identical_trees_report_no_change` |
| `diff:false` byte-identical | `serve::tests::fb3_diff_false_reply_is_byte_identical_and_changes_are_appended` |
| default on, non-bool rejected | `serve::tests::fb3_diff_defaults_on_and_rejects_non_booleans` |
| duplicate identities | `diff::tests::duplicate_identities_stay_distinct` |

Note: the byte-identical test checks the reply composition (`input_reply()`) directly, because
building a live `ArtSession` is not possible in a unit test. The live MCP smoke below checks the
`diff:false` path end to end: the reply has no `changes` key.

## 5. Live MCP smoke (pinned to the scratch install)

Install: `tools/release/install.sh --from-target .cache/g3f1-target --prefix .cache/fb3/prefix`.
Binaries verified to match the target (`cmp`). Smoke run from an empty directory under
`.cache/fb3/run`, with `ACOS_BIN=<prefix>/bin/acos`.

**Pinning incident.** The first counter run used the owner's `~/.local/bin/acos`. `ACOS_BIN` was
unset, and `acos-mcp` falls back to `acos` on PATH, which is the owner's pre-FB3 install. That
run was discarded. It is not evidence, and the owner's install was only read, never modified.
Every smoke after that was pinned.

### Counter (`mcp_smoke.py --mode counter`): **18/18 checks, `{"smoke": "ok"}`**

`acos_launch` → `acos_tap {"text":"Increment"}`. The reply's `changes` (2,513-byte reply):

```json
"changes": {"baseline": "session", "frames": {"before": 2, "after": 3},
 "activity": "dev.acos.fixtures.counter.MainActivity",
 "added": [], "removed": [],
 "changed": [{"id": "n4_dev.acos.fixtures.counter:id/count", "text": "1", "content_desc": "",
              "class": "android.widget.TextView", "bounds": [42,317,63,368],
              "fields": {"text": {"old": "0", "new": "1"}}}],
 "counts": {"added": 0, "removed": 0, "changed": 1}, "truncated": false}
```

The same tap with `diff: false` returned a 2,127-byte reply with no `changes` key
(`acos_tap_diff_false` check).

### Gaten (`mcp_smoke.py --mode gaten`): **23/23 checks, `{"smoke": "ok"}`**

The Gaten session walks the three onboarding pages by text, then taps the bottom tab
`Analytics` (`RNTAP` rule: tap by the label the tree reports). Each tap carries its own
`changes`. The tab tap's reply is 9.5 KB of `changes`:

```json
"changes": {"baseline": "session", "frames": {"before": 11, "after": 12},
 "activity": "com.gaten.app.LauncherIconLight",
 "counts": {"added": 198, "removed": 362, "changed": 3}, "truncated": true,
 "added":   [{"id": "n11_", "text": "", "content_desc": "", "class": "u7.l", "bounds": [0,0,1080,2400]}, "…40 entries"],
 "removed": [{"id": "n12_", "text": "", "content_desc": "", "class": "u7.l", "bounds": [0,0,1080,2400]}, "…40 entries"],
 "changed": [{"id": "n11_", "text": "", "content_desc": "", "class": "b5.g", "bounds": [0,2151,1080,2400],
              "fields": {"bounds": {"old": [0,0,1080,2400], "new": [0,2151,1080,2400]},
                         "visible_bounds": {"old": [0,0,1080,2400], "new": [0,2151,1080,2400]}}}, "…"]}
```

The reply is about 10 KB. Earlier onboarding taps in the same run were 10–16 KB each.
The Gaten `changed` entries are class-based (`b5.g`); they have no resource id, so identity is
`class#ordinal`.

The smoke runs and the `diff:false` check were repeated after the final install (`cmp` against the
target, and after the descriptor wording change). Final logs: `FB3-evidence/smoke-counter-final.jsonl`
and `smoke-gaten-final.jsonl`.

## 6. Gates and Gaten flows (final binary, `acos verify --engine art`)

One run-exclusive acquisition per gate. Results in `FB3-evidence/gates/` and `FB3-evidence/gaten/`.

**15 standard gates, all as expected:**

| gate | expected | observed |
|---|---|---|
| v2-art-counter, w22-counter-reset, w54-dialog-pixels, w42-back-frame, w15-dialog-menu, form-validation, suite/T03-render-good, v1-offscreen-pager, w7-insets-report, se1-expect-change-tap | pass | pass (10/10) |
| suite/T03-render-wrong, w7-insets-carve-negative, negative/se1-expect-change-noop | ASSERTION_FAILED | fail ASSERTION_FAILED |
| negative/offscreen-pager-visible | ELEMENT_OFFSCREEN | fail ELEMENT_OFFSCREEN |
| p11-progress-timeout | TIMEOUT | timeout TIMEOUT |

**Gaten, 5 + 5 (APK `gaten-dev-f344272-x86_64-release-unsigned.apk`):**

| scenario | expected | observed |
|---|---|---|
| gaten-home, gaten-tabs, gaten-settings, gaten-search, gaten-lesson | pass | pass (5/5) |
| negative/gaten-{home,tabs,settings,search,lesson}-wrong | ASSERTION_FAILED | ASSERTION_FAILED (5/5) |

## 7. Bench (M4–M8, interleaved A/B, 3 reps per arm)

Full method and tables: `results/handoff/FB3-ab-compare.md`.

* **A** = final `57c3c91` (binary sha256 `15bc0fe7…`). **B** = `05ba66b` base in a linked worktree
  (binary sha256 `d316068c…`, no FB3 code).
* Order B A B A B A (`FB3-evidence/bench/bench-run.log`). Each run reported `Noisy phases: 0/1`.
* Scored with the old median gate and `tools/bench/ab-compare.py` (pooled 5 %-trimmed mean,
  frame-slot shares), with the base as `--a`.

**M8 tap, fast (the lattice-sensitive metrics), per the pooled trimmed mean:**

| metric | base trimmed | FB3 trimmed | Δ (FB3 − base) | verdict |
|---|---:|---:|---:|---|
| M8 tap counter, fast | 49.98 ms | 43.76 ms | −6.2 ms (−12 %) | **parity**: first-slot share moved 15 pp, per-rep spread 32–53 ms on both arms; not claimed as a speedup |
| M8 tap form, fast | 32.40 ms | 32.78 ms | +0.4 ms (+1.2 %) | **parity** |
| M8 tap counter, strict (recomputed from the raw samples) | 946.3 ms | 952.1 ms | +5.7 ms (+0.6 %) | **parity**: old median gate +3.0 ms (+0.3 %); a regression needs more than 5 % and more than 2 ms |

**Everything else:** M4 warm launch (fast −2.6 ms, −1.6 %; strict +0.3 ms), M5, M6 (fast +0.2 ms,
strict +0.1 ms), M7 (strict +0.1 ms, fast +0.01 ms), tap acknowledgement, teardown: all within
the gate, and **unchanged** within noise. The old per-rep median gate reports no regression on
any metric.

**Method notes.** The bench JSON records the worktree HEAD as its commit, so it reads
`57c3c91` for both arms. Arm B's binary is `05ba66b`. The bench baseline JSON also has no raw
samples, so `FB3-evidence/bench/merge-samples.py` merged each rep's raw values in before scoring. The
first comparison passed FB3 as `--a`, which inverted every verdict (`ab-compare.py` computes
`delta = b − a`), so that run is discarded and the final one uses the base as `--a`.

## 8. Corrections to the earlier draft of this file

* The earlier `FB3-ab-compare.md` compared `0ed96bf` against itself, and its tables were wrong.
  Replaced.
* The earlier bench table and the earlier Gaten reply size (11.7 KB) came from a different run.
  Replaced with the numbers above.
* The earlier smoke numbers were not pinned to the scratch install (see §5, pinning incident).
  Re-run pinned.
* The earlier test count (695) was for `0ed96bf`. The final count is 698.

## 9. Resources

* RAM: `MemAvailable` stayed around 30 GiB throughout (`free -g`: 28 GiB used of 60 at the last check).
* Disk: 83 % used, 158 GiB free at the last `df -h /`. Below the 85 % line.
* Scratch kept: `.cache/fb3/prefix` (1.6 GB scratch install) and `.cache/g3f1-target` (target). The
  base worktree `.cache/benchbase` is deleted after this write-up, along with its base target.

## 10. Remaining / for the orchestrator

1. **Default-on size cost.** Every input reply grows: the counter tap reply is 2,513 bytes
   against 2,127 with `diff:false` (+386 bytes). A Gaten tab tap is about 10 KB, capped at 40
   entries per list. Callers that care about size should send `diff: false`.
2. **`focused` is omitted, not `null`,** when no node has focus.
3. **`acos_swipe` has no live check** on ART (decision in §2).
4. **Merge.** Not done here, by instruction. Main is not merged into `fb3-act-diff`.
