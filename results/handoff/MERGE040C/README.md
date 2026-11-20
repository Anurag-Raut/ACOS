# MERGE040C — backsettle + wvtail into `modules`

Branch `merge040c` from `modules` @ `c1f08344` (MERGE040B + MERGEFIX2 + sysui4).
One commit per track. This file is the conflict table + intent, then the
evidence trim the brief asked for, then the verification.

```
c1f08344 modules (MERGE040B)                            <- base
9a5a7c65 MERGE040C(1/2) backsettle bc8fae6e  (squashed, one parent)
c4d213a0 MERGE040C(2/2) wvtail     75bfb6d4  (merge commit, base 0faabef0)  <- HEAD
```

`cargo check --workspace --all-targets`: green after each
(`01-cargo-check-after-backsettle.log`, `02-cargo-check-after-wvtail.log`).

## 1. Conflicts and resolutions

### 1.1 backsettle — no textual conflict, applied as ONE squashed commit

`git merge --squash backsettle` auto-merged all five files. The squash is the
point: backsettle's history adds ~157 MB of raw matrix frames and deletes them
again (`5f6553be`), so a merge commit would drag that history into `modules`.
The squashed commit has **one** parent (`c1f08344`), its tree has **no**
`results/handoff/BACKSETTLE/matrix/` directory, and nothing in it is over 5 MB
(§2).

The settle seam was re-checked after the merge, one implementation per seam:

| seam | merged state |
|---|---|
| `reshown_bind` | one definition (`crates/acos-art/src/launch.rs`), five guards: settled sample, sample newer than the change, not `window_gone`, non-empty tree, `ReshownOutcome::Bind` only |
| `WindowReshownGate` / `WindowForest` / `window_forest` / `top_window_frame` | one each; the gate is built only for a strict input action whose acks have all arrived (baseline at the ack) |
| the settle arm's place | one arm, **after** the shipped `!sample.settled` wait, so an unsettled screen still falls through to the shipped rule |
| MERGEFIX2 / SYSUI3A window-removed handled evidence | untouched: `window_removal`, `removal_binds`, `live.input.window_removed_since` still decide `window-removed` |
| REL3H2 guards | untouched: `unhandled_ack_floor` / `UnhandledSettleGate` / `UNHANDLED_ACK_FLOOR_REASON` unchanged |
| settle precedence | unchanged: host window removal > unhandled shortcut > `expect_change:false` (no side edits `eval_tap_change`'s branch order) |
| `SettleTiming.window_reshown` | one field; every other construction site sets `None` |

`interactive_serve` keeps both sides' arguments: `Some((injected.token,
removal_mark))` (window removal) **and** `window_reshown` (the forest at the
ack, strict inputs only).

### 1.2 wvtail — one textual conflict (DECISIONS.md)

**`DECISIONS.md`**: both sides appended a section at the same place — MERGEFIX2's
"ADR-0029 amendment: one launch, one delivering side" (ours) and WVTAIL's
"ADR-0031 settlement note: input latency is a frame rule, and a restart is a
restart" (theirs). Kept **both**, ours first (they are chronological and
independent; neither supersedes the other).

Everything else auto-merged. The one place that needed a decision was the tap
path, and the decision is **MERGE040B's `659e224d` stands**: one tap/ack
implementation.

`crates/acos-webview/src/view.rs` is the file MERGE040B had to cut back to one
implementation. WVTAIL's diff is on top of `wvtap` (already in `modules`), so
its `View::tap` hunk has the **same two pulls** MERGE040B kept —
`down → render_input → up → render_input`, `Input` priority — as unchanged
context lines; WVTAIL changes only what happens after them:

* the flat `reply.wait(Duration::from_secs(2))` per event is gone, replaced by
  `wait_input_ack` (a follow-up input-priority frame carries the ack, bounded by
  `ACK_FRAMES`, a miss reported as `acked: false` and logged, never a lost tap);
* `tap`/`touch` return `InputAcks { events, acked, frame_ms, ack_ms }` to the
  guest.

So the merged `tap` is `down → render_input → up → render_input → wait_input_ack`
— wvtap/WVFAIR's two pulls, WVTAIL's ack rule. **Not** a hybrid: no third
unconditional pull was added, and the extra frames only happen when an ack did
not arrive with the event's own frame (measured: 2 `NOT acked` lines per fair
run, the same count WVTAIL's own `evidence/fair20/` shows).

Checked for the MERGEFIX lesson (duplicate constructors / shadowed `let`s) after
each merge: one `render_input`, one `View::pending_frame`, one
`AttemptCtx::{wait_slice,preempted}`, one `Attempt` enum, one `FairGate::pull`,
one `InputAcks`, one `ENGINE_RESTARTED` code path; the only repeated `fn new`s
are on different types.

`scenarios/webview-fair.yaml` keeps the **1 s** bound (`[0-9]{1,3}` ms — WVTAIL
tightened it from 3 s; the merge did not relax it back) and WVTAIL's
`since_previous_step` slow-view frame proof. `scenarios/webview-probe.yaml` is
unchanged.

## 2. What was trimmed from backsettle's evidence (and what was not)

The brief: keep README, json/tsv/log summaries and small `runs/` evidence; drop
any file over 5 MB. What the import contains, from 46 MB on the source branch:

| kept | dropped (still on `backsettle`) | why |
|---|---|---|
| `README.md`, `matrix.json`, `matrix.tsv`, `matrix.log`, `suite{,2..6}.log`, `scenarios.log`, `bench.log`, `run-ab.sh`, `run-scenarios.sh` | — | the summaries |
| `bench/` (27 json summaries, 1.2 MB) | — | the A/B arms' numbers |
| `runs/<scenario>/` — `verdict.json`, `run.json`, `engine.json`, `steps/`, `logs/`, `repro.*`, `retention.json`, `resources.json`, `events.jsonl`, `session-*.json`, `unsupported.jsonl` | `runs/<scenario>/tree/*.json` (19.1 MB), `runs/<scenario>/frames/*.png` (12.2 MB) | raw tree dumps and per-frame pixels; the verdict + steps already carry the facts |
| `runs/<scenario>/art-frames/live-frames/` (36 PNGs, 3.0 MB) | — | **kept**: these are the settle's own before/after frames, the evidence for a settle change |
| `diag/*.yaml`, `diag/*-trace.log` | `diag/<run>/` (26 run dumps, a duplicate of the `runs/` shape) | the trace logs are the finding, the dumps are duplicates |

Result: 562 files, 13.2 MB (from 1656 files, 46 MB). **No file over 5 MB**
(largest imported file: `diag/hv-guest1.log` 1.05 MB; the largest dropped was
`runs/gaten-backstack/tree/18.json` at 1.79 MB).
`results/handoff/BACKSETTLE/matrix/` — the 157 MB of raw frames — is not in the
tree at all.

## 3. Verification

Environment: private `ACOS_RUNTIME_DIR=$WT/.cache/rt`,
`ACOS_SESSION_SOCKET=$WT/.cache/rt/session.sock`,
`ACOS_WEBVIEW_SOCKET_DIR=$WT/.cache/wv` (per-run dirs for the webview arms),
`ACOS_VIEW_TEST_DIR=$WT/.cache/lv`, `ACOS_WEBVIEW_CACHE=$WT/.cache/webview` (a
copy of the shared engine cache, so no run downloads it), `ACOS_FONT_DIR` from
the shared boot image, `CARGO_TARGET_DIR=$WT/.cache/mc-target`. Fixtures built
first: `tools/build-apps.sh`, `tools/build-dogfood-apps.sh`,
`tools/build-fixtures.sh` (`03`/`04`/`05`). Every ART run went through
`tools/art-runtime/run-exclusive.sh --task MERGE040C`.

| gate | result | evidence |
|---|---|---|
| `cargo check --workspace --all-targets` | green ×2 | `01-…`, `02-…` |
| `cargo test -p acos-protocol -p acos-wm -p acos-webview` | 276 passed, 27 binaries | `10-test-protocol-wm-webview.log` |
| `cargo test -p acos-art` | 254 passed, 16 binaries | `11-test-art.log` |
| `cargo test -p acos-verify -p acos-services -p acos-cli` | 1160 passed, 112 binaries | `12-test-verify-services-cli.log` |
| ART corpus, 19 scenarios (`run-corpus.sh`) | every verdict matches its declaration: 16 pass, 3 declared-fail | `art/<name>/verdict.json` |
| `webview-probe` ×5 | 5/5 pass | `art/webview-probe-probe/run{1..5}/verdict.json` |
| `webview-fair` ×5 | 5/5 pass, taps **367, 508, 509, 509, 515 ms** (all < 1 s) | `art/webview-fair-fair/run{1..5}/logs/logcat.txt` |
| `tools/run-matrix.sh --no-build` | 33 rows, **false_pass 0**, **infra_timeout 0**, unclassified 0, negative_correct 32/32, step_timeout 2, every gate true, `verdict_eligible: true` | `16-matrix-no-build.log` (summary line), `matrix.json` |
| `tools/check-tree-clean.sh` | `check-tree-clean: clean (…, 0 tracked change(s))` | `17-check-tree-clean.log` |

Corpus detail (`run-corpus.sh`, one ART acquisition per run):

| scenario | verdict | declared |
|---|---|---|
| hello-views | pass | pass |
| multi-activity | pass | pass |
| negative/multi-wrong-after-back | fail `ASSERTION_FAILED` (step 8 `assert_text`) | fail |
| back-layers | pass | pass |
| back-layers-ime | pass | pass |
| gaten-backstack | pass | pass |
| gaten-lesson | pass | pass |
| rel3h-unhandled-input | pass | pass |
| rel3h-unhandled-input-expect-change | fail `ASSERTION_FAILED` (`settle.pixels_changed`) | fail |
| rel3h2-expect-handled-true | fail `INPUT_NOT_HANDLED` (`expect_handled`) | fail, `expect_error: INPUT_NOT_HANDLED` |
| rel3h2-expect-handled-false | pass | pass |
| touchdisp-dispatch-probe | pass | pass |
| touchdisp-gesture | pass | pass |
| touchdisp-posted-side-effect | pass | pass |
| form-validation | pass | pass |
| mproc3-two-process | pass | pass |
| sysui1-shade | pass | pass |
| a11y-probe | pass | pass |
| counter | pass | pass |

`hello-views` and `multi-activity` are the two scenarios BACKSETTLE re-pointed
to `pass` (two Backs: the soft input takes the first, the Activity the second)
and `negative/multi-wrong-after-back` to `fail` (the wrong text is now reached
and fails strictly). All three behave as the merged tree declares.

## 4. Notes for the orchestrator

* The backsettle history was **not** merged (squash): `modules` never carries the
  157 MB of added-then-deleted matrix frames. If a future branch needs
  backsettle's own history it must come from `backsettle` itself.
* `View::tap`/`touch` return `InputAcks`; any branch that ignored the old `()` in
  a call site needs `let _ =` (none in this tree).
* The engine's teardown shows up as one `engine restarted … dropped 2 view(s)`
  line at the end of every webview run (the run's own kill). That is the
  `ENGINE_RESTARTED` path, not a session fault; the fair/probe runs are 5/5.
* `sysui1-shade` took 778 s wall in this corpus (queue + the shade's own
  settles); it passed. No timing number is claimed from any run here.
* The matrix ran with the host gate open (`loadavg1` 2.8, `MemAvailable` 43 GiB),
  so its rows are `verdict_eligible: true`; the two `step_timeout` rows are the
  new taxonomy's own-deadline class (`gaten-content-window-impossible`,
  `offscreen-pager-wait`), not infra, and both are negatives that still fail
  correctly.
* My own evidence is trimmed the same way as backsettle's: `tree/` and
  `frames/` dropped, except the settle's before/after frames for `hello-views`,
  `multi-activity` and `multi-wrong-after-back` (the claim BACKSETTLE changes).
  `matrix-artifacts/` (157 MB of per-run dumps) is not kept; `matrix.json` has
  every row.
