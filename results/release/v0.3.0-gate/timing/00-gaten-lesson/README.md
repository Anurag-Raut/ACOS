# RELGATE2 item 1 -- the one RELGATE1 Gaten row that failed, re-run

`scenarios/gaten-lesson.yaml` at RC `169bfa5e` (the gaten-lesson fix: a second
`back: {}` after the `type:` step, because on v0.3.0 the first Back lands on the
real bound IME and only hides the keyboard).

Command (the same one RELGATE1 used, `results/release/v0.3.0-gate/correctness/sweep.sh`,
into a fresh artifacts dir):

```
ACOS_RUNTIME_DIR=$WT/.cache/rg2/rt/w0 ACOS_SESSION_SOCKET=$WT/.cache/rg2/rt/w0/sock \
  run-exclusive.sh --task RELGATE2 -- guarded.sh --mem 6G --time 600 -- \
  .cache/rg2-target/release/acos verify scenarios/gaten-lesson.yaml --engine art \
    --artifacts results/release/v0.3.0-gate/timing/00-gaten-lesson/artifacts --json
```

Result: **PASS** -- `status: pass`, `ok: true`, `fidelity: art-reference`,
29 steps, wall 20 s (`verify.json`, `artifacts/verdict.json`).

Warnings carried by the run (both expected, neither a failure):
`SCENARIO_ENGINE_MISMATCH` (the scenario declares `engine: rust-vm`; the request
ran `art`), `INPUT_UNHANDLED` on step 21 (a `type:` step whose 32 events the
window acked `handled=false`; it makes no change claim, so it is a pass with a
warning). The final `wait_for text*="Recently looked up"` -- the step that timed
out in RELGATE1 -- settles in well under its 30 s deadline.
