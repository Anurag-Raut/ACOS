# BACKSETTLE — a host-recorded window-forest change is settle evidence

Branch `backsettle` from `scenhyg` (`8b3afbdd`). Resumed after the host
power-off in `/home/Qui/.t3/worktrees/acos/t3-6219a35a-eb2ef407add7c3f9`
(the first session's worktree `t3-bcd8bf0a-…` no longer exists; its
pre-change diagnostics are the committed `diag/` tree). Target
`.cache/backsettle-target`, private `ACOS_RUNTIME_DIR`/`ACOS_SESSION_SOCKET`
(`.cache/rt-backsettle`), `ACOS_VIEW_TEST_DIR=$PWD/.cache/lv`; every ART run
through `tools/art-runtime/run-exclusive.sh` + `tools/guarded.sh`.

**Status: DONE.** The rule is one place in the settle code, the three
scenarios declare their headers' claim, and every acceptance run is in this
directory (§4, §5).

## 1. What was wrong, measured before any change

`hello-views`, `multi-activity` and `negative/multi-wrong-after-back` ended in
`TIMEOUT back: no-draw-after-action` (SCENHYG, and reproduced here in
`diag/hv1` and `diag/ma1`). Tracing the failing step (`ACOS_ART_INPUT_TRACE`)
splits that timeout into **two** facts, and only one of them is about Back:

1. **The first Back never reaches the Activity: the soft input takes it.**
   AOSP's Back order is dialog → soft input → Activity (`back-layers-ime`
   documents it; REL3K2). Both fixtures type into an `EditText`, so the IME is
   up when Back arrives; the framework's `ImeInputStage` consumes the press and
   hides the keyboard (`ACOSIME back-down hid the soft input seq=1`), the
   Activity stays resumed, **nothing draws** and the guest's tree changes only by
   the IME insets. `diag/ma2` step `06-back` is exactly that: `handled: true`,
   `pixels_changed: false`, no lifecycle line in `logs/logcat.txt`.
   The reference captures typed with `adb shell input text`, which bypasses the
   IME, so their single `input keyevent KEYCODE_BACK` did reach the Activity —
   the scenario headers inherited that precondition without it.
2. **The second Back is the finish, and its effect is a host fact, not a draw.**
   With the IME hidden, Back reaches the Activity, ATMS runs
   `Pause(finished) → Resume(previous) → Stop → Destroy`
   (`diag/ma4`), the delivered window is removed and the window below becomes
   the top window again. On an idle host it also redraws ~40 ms later, and the
   shipped strict rule binds `host-window-removal`; on a loaded host that draw is
   late and the step charges the full settle deadline (`no-draw-after-action`)
   although the screen is complete.

So the corpus fix is the IME press (a scenario change, §3) and the settle fix is
the second fact: when **the host itself recorded** that the top window changed to
an existing window whose composited frame is complete, that is the action's
observable effect.

## 2. The rule (one place in the settle code)

`crates/acos-art/src/launch.rs`, inside `wait_for_event_settled` (the one strict
rule; `wait_for_settled_observation` forwards to it and no second path was
added): `WindowForest` + `WindowReshownGate` + `reshown_bind`.

* **Evidence.** `settle.bound: "window-reshown"`, `settle.window_reshown` (the
  token, `"0x…"`) and `settle.window_reshown_frame` (the frame number the host
  composites). The bind also reports the measured `pixels_changed`, so
  `expect_change: true` still has to earn its pass.
* **The forest.** `window_forest()` reads the LAYER1 stack (`acos-wm`
  `layer_stack().ordered()`, the same order composition, hit-testing and
  `assert_windows` use) and, for the top window, the newest **complete** buffer in
  that window's compositor layer subtree. The window's own `SurfaceControl` layer
  carries the placement (`placement.z` = stack index) but the pixels arrive on its
  BLAST child layers (`diag` dump: root `h=0x15 z=0 latest=None`, children
  `h=0x13 latest=18`, `h=0xe latest=5`), so the frame is looked up through the
  parent links — the root alone never has one.
* **Guards, all required, each with a test that fails if it is dropped**
  (`launch::backsettle_tests`, 10 tests):
  1. *The change must be caused after the action's input ack.* The baseline is
     read where the gesture's acks have all arrived (the collect returned), so a
     top window that was already top at the ack never binds
     (`a_top_window_that_was_already_top_at_the_ack_never_binds`).
  2. *The re-shown window's frame must be the one the host composites.* The top
     window must exist **in the baseline** (a window the action created is not a
     re-show: `a_window_created_by_the_action_never_binds_as_reshown`) and its
     layer subtree must hold a complete stored buffer (`hidden` excluded,
     `frame_number/width/height > 0`); otherwise `window-frame-missing`
     (`a_re_shown_window_without_a_complete_frame_never_binds`). ADR-0032
     amendment 1 is untouched: the placeholder montage wait still owns "the app
     never mounted", and a re-show is by construction a window that already drew.
  3. *If the app draws afterwards within the settle window, the newer frame wins.*
     A C1 store after the baseline (`generation != baseline.generation`) makes
     the gate step aside for good; the shipped rule then binds that newer frame
     (`a_draw_after_the_action_supersedes_the_re_shown_frame`). It is visible on
     ART in `diag/ma9-trace.log:818` — `window-reshown-superseded top=0x100000009
     frame=Some(1) gen=23 base=22`, then the step binds `host-window-removal`.
  4. *Back with no window change keeps today's semantics.* The gate never binds
     on an unchanged forest (`an_unchanged_window_forest_keeps_the_shipped_semantics`),
     so `INPUT_UNHANDLED` / `unhandled_input` / `no-draw-after-action` are
     unchanged (`rel3h-*`, `rel3h2-*`, `touchdisp-*` in §4).
  5. *A scenario expecting a change that did not happen still fails.* The bind
     measures `pixels_changed` against the caller's pre-action capture and
     `expect_change: true` judges it, so an unchanged screen fails the claim.
  6. The tree half: the sample must be **settled** (no pending traversal — a
     screen in motion is never evidence that the action's effect is complete;
     the arm sits after the shipped `!sample.settled` wait, so a re-show cannot
     pre-empt it), newer than the change, and non-empty (the tree of the
     now-top window); a task-finished forest is `window-gone`, never a re-show
     (`an_empty_forest_is_not_a_re_show`,
     `an_empty_tree_is_not_a_bindable_observation`,
     `only_a_settled_post_change_sample_binds_the_re_shown_window`).
  7. One confirmation window (`ACOS_STRICT_SETTLE_CONFIRM_MS`, the strict rule's
     own 24 ms) and a restart on a second change
     (`a_second_window_change_restarts_the_confirmation_window`).
* **Cost.** The gate is built only for a **strict input action**
  (`mode == Strict`, `window_reshown: Some(baseline)`); tree/launch waits and
  `fast` pass `None`, so the fast bench path takes no lock and no extra work.

A trace line records the gate's own decision (`settle forest reason=… top=…
frame=… gen=…`), so a bind and the draw that superseded it are both readable.

## 3. Scenarios re-pointed to what their headers claim

| scenario | before | now | why |
|---|---|---|---|
| `hello-views` | `non_pass` TIMEOUT/no-draw-after-action | **`pass`** | IME press (`key: back, settle: content`) + the finish Back binding `window-gone` |
| `multi-activity` | `non_pass` TIMEOUT/no-draw-after-action | **`pass`** | IME press + the finish Back binding (`host-window-removal`, or `window-reshown` when the below window's draw is late) |
| `negative/multi-wrong-after-back` | `non_pass` TIMEOUT/no-draw-after-action | **`fail` ASSERTION_FAILED** | the wrong-text assertion is now reached and fails strictly on `Goodbye` vs `Hello` |

The added step is the AOSP layer the scenarios were missing, and the same idiom
`scenarios/gaten-lesson.yaml` already uses ("the first Back hides the soft input,
the second Back leaves the screen"). No assertion was dropped: `hello-views`
still checks its 7 lifecycle patterns, bounds and the no-crash/no-crash-after-
destroy claims; `multi-activity` still checks the AOSP finish order from
`fixtures/reference/multi-activity/art-api34-x86_64/lifecycle-summary.txt`; the
negative still asserts a wrong text after a real Back.

## 4. Acceptance evidence

| item | file | result |
|---|---|---|
| `cargo test -p acos-verify -p acos-cli -p acos-art -p acos-wm -p acos-protocol` | `suite6.log` (final tree) | **947 passed, 0 failed, 16 ignored** (green); `suite3.log` was the same command without the warm-data re-export and *skipped* the 6 ART cleanup tests (`suite4.log` is the pre-`settled`-guard run, `suite5.log` a load flake in `cleanup::sigkill…`, green in suite4/suite6) — see §6 |
| `backsettle_tests` (the rule + every guard) | `suite6.log` (`acos-art` lib) | **10 passed** |
| ART: the three re-pointed scenarios (`run-scenarios.sh`, log `scenarios.log`) | `runs/hello-views`, `runs/multi-activity`, `runs/multi-wrong-after-back` | `pass` (28/28), `pass` (23/23), `fail` on step 8 `ASSERTION_FAILED` (`"Goodbye"` vs `"Hello"`) |
| ART: `back-layers`, `back-layers-ime`, `gaten-backstack`, `form-validation`, `mproc3-two-process` | `runs/…` | `pass` 22/22, 22/22, 25/25, 28/28, 16/16 — declared outcome each |
| ART: `rel3h-unhandled-input` (pass), `rel3h-unhandled-input-expect-change` (fail), `rel3h2-expect-handled-false` (pass), `rel3h2-expect-handled-true` (fail) | `runs/…` | pass 4/4, fail `ASSERTION_FAILED`, pass 4/4, fail `INPUT_NOT_HANDLED` — declared outcome each |
| ART: `touchdisp-dispatch-probe`, `touchdisp-gesture`, `touchdisp-posted-side-effect` | `runs/…` | `pass` 4/4, 3/3, 5/5 |
| `tools/run-matrix.sh` (33 rows: counter + every `scenarios/negative/*.yaml`) | `matrix.json`, `matrix.tsv`, `matrix.log` | **false_pass 0, infra_timeout 0, all 5 gates true**, 32/32 negatives correct, host gate **ok** (`verdict_eligible: true`) |
| M8 counter fast indicative 5-rep A/B (base `8b3afbdd` vs backsettle) | `bench/` (`run-ab.sh`, `bench.log`) | see §5 |

## 5. Results

### 5.1 Crate tests — `suite6.log`

```
tools/art-runtime/run-exclusive.sh --task BACKSETTLE -- \
  tools/guarded.sh --mem 26G --time 2400 -- bash -c \
  'export ACOS_ART_DATA=$PWD/.cache/art/data; cargo test --offline -p acos-verify \
     -p acos-cli -p acos-art -p acos-wm -p acos-protocol'
```

947 passed / 0 failed / 16 ignored, rc=0. `ACOS_ART_DATA` is re-exported to
this checkout's warm data **inside** the lock because `run-exclusive.sh` sets a
fresh empty one, which makes the six ART-booting `tests/cleanup.rs` cases skip
(`SKIP cleanup test: no cached guest dex`) instead of running — `suite3.log` is
that false-green, kept here on purpose. `suite4.log` has no `SKIP cleanup` line
and `tests/cleanup.rs` runs 9/9 in 14.8 s.

### 5.2 ART scenarios — `scenarios.log`, `runs/`

`run-scenarios.sh` runs each scenario with the release binaries through
`run-exclusive.sh` (shared slot) and prints declared vs produced from the
scenario's own `expectation:` field. All 15 match; the `settle.bound` of every
Back in these runs is `host-window-removal` or `window-gone` (§5.4).

### 5.3 Matrix — `matrix.json`

```
false_pass 0 · false_fail 0 · errors 0 · infra_timeout 0 · step_timeout 2 (declared)
negative_runs 32 · negative_correct 32 · negative_expect_error_mismatch []
gates: false_pass_zero true · unclassified_zero true · harness_timeout_within_2pct true
       negative_all_correct true · no_infra_timeout true
```

`negative/multi-wrong-after-back` is now reached and fails where its header says
(`fail`/`assertion`), and it is counted correct because the declaration now
matches the code it produces.

The run behind these files is the one whose host gate **passed**
(`run-matrix: host gate ok (loadavg1 3.4, MemAvailable 48.32 GiB)`), so
`verdict_eligible: true`. An earlier run of the same command on a loaded host
(loadavg1 11.96, gate limit 4) was reproduced with `--allow-contaminated` and
produced the same summary (`false_pass 0`, `negative_correct 32/32`); it is not
kept, because the classification gates do not need the timing eligibility and
one gate-ok run is stronger evidence.

Raw per-run artifacts (157 MB of frames/trees) are **not** committed; the
runner's JSON/TSV/log are, and `tools/run-matrix.sh --no-build
--target-dir $PWD/.cache/backsettle-target --out results/handoff/BACKSETTLE/matrix.json`
reproduces them.

### 5.4 What `window-reshown` did on ART

No corpus run bound it, and none could have passed without the host facts:
every Back here ends with the delivered window **removed** (the framework's
finish path), which the shipped REL3I3 rule already binds as
`host-window-removal`; `hello-views`' task finishes and binds `window-gone`.
When the below window's draw landed after the removal, the gate stepped aside
exactly as designed — `diag/ma9-trace.log:818`:

```
settle forest reason=window-reshown-superseded top=0x100000009 frame=Some(1) gen=23 base=22
```

So the rule is the *fallback* for the case the shipped rules cannot bind (a
top-window change with no removal of the input's window and no new frame); its
bind, its frame and every guard are proven by the 9 unit tests in
`launch::backsettle_tests`, and it can never widen a pass: it requires a
host-recorded forest change **after** the ack, a complete composited frame of an
already-existing window, no newer C1 store, a non-empty post-change tree and a
measured `pixels_changed` for `expect_change`.

### 5.5 Bench — `bench/`, `bench.log`

`run-ab.sh`: 5 reps, arms **interleaved** (base `8b3afbdd` then backsettle per
rep), `--bundle latency --reps 20 --fast-reps 40` (ADR-0028 counts), one shared
slot per acquisition — **indicative**, the release-gate A/B is the bench
owner's. 200 fast samples per arm. `bench/ab-compare.txt`:

| metric | base trimmed | backsettle trimmed | delta | verdict |
|---|---:|---:|---:|---|
| **M8 tap → settled, counter (fast)** | **26.48** | **27.38** | **+0.90 ms (+3.4 %)** | **within-noise** |
| M8 tap → settled, form (fast) | 28.61 | 28.58 | −0.03 ms (−0.1 %) | within-noise |
| M4 warm launch (fast) | 161.04 | 154.22 | −6.82 ms (−4.2 %) | within-noise |
| M6 reset (fast) | 180.34 | 180.76 | +0.41 ms (+0.2 %) | within-noise |
| M8 tap → settled, counter (**strict**, old gate) | 946.79 | 947.28 | +0.50 ms (+0.1 %) | within-noise |
| cpu active during the strict tap cohort | 42.91 % | 43.11 % | +0.20 pp (+0.5 %) | within-noise |

The +0.90 ms on M8 counter fast is inside the lattice gate (+5 % and +2 ms) and
inside the base arm's own rep-to-rep spread; the first-slot share moved 45 % →
40 %, so the sample distribution is the same shape. Mechanically the fast path
cannot move: `interactive_serve` builds the forest gate only for
`mode == SettleMode::Strict` (`window_reshown` is `None` for `fast`), so a fast
tap takes no compositor lock and no extra work. The strict loop *does* read the
LAYER1 stack (and the top window's newest stored frame) on every observation,
and the strict metrics show no cost either: M8 counter strict +0.50 ms (+0.1 %),
M8 form strict +0.45 ms (+0.0 %), `cpu_active_tap_strict_pct` +0.20 pp.

## 6. The one test-side fix this needed (`crates/acos-art/tests/cleanup.rs`)

`cargo test -p acos-art` was red in every agent worktree, for a reason that has
nothing to do with Back: `tests/cleanup.rs` built its scratch root as
`<checkout>/.cache/h1-tests`, and a run below it binds
`<root>/<tag>-<pid>/data/runs/<pid>-<seq>/socket/…`. At this worktree's 53-byte
path the deepest socket was 113 bytes, over `sun_path`'s 108, so six of the nine
cases died with `acos-art: io: path must be shorter than SUN_LEN`
(`results/handoff/BACKSETTLE/suite.log`, and suite2 §5.1's false green).

That is the SUN_LEN class INTEG030 fixed for `acos-cli`/`live_view` with
`acos_test_support::short_scratch_root`; `cleanup.rs` was the one suite that had
not been ported. It now takes the same rule (the checkout's `.cache/h1-tests`
when it fits, `/dev/shm/acos-cleanup-tests` otherwise) plus
`the_scratch_root_leaves_room_for_the_run_sockets`, which computes the deepest
socket this suite makes the launcher bind and asserts it stays under
`SUN_PATH_MAX`.

Effect: 8 passed / 1 failed (the six SUN_LEN cases) before → 9 passed after, on
the same tree and host; the run is 15 s instead of 182 s because nothing waits
out a 60 s "guest never appeared" timeout any more.

The suite must be run with the warm data re-exported (see §5.1):
`run-exclusive.sh` sets a fresh empty `ACOS_ART_DATA`, which makes these cases
*skip* rather than run — a false green this handoff keeps (`suite3.log`) so the
next reader cannot repeat it.
