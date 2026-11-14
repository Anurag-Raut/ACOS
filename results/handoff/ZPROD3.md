# ZPROD3 handoff — engine wiring for the production zygote (`ACOS_ART_ZYGOTE=on`)

Branch `zygote-prod-1`, base `e4cca7b` (ZPROD2). Design
`docs/decisions/2026-11-14-zygote-prod.md` §2–§6. `EXPERIMENTS.md` §E2.18.
Orchestrator decision: this track is **wiring + reply fields + the flag-off/flag-on counter gate
pair**; ZYGOTE_DEAD *restart + new epoch + refork* is ZPROD4, F3 (per-child cgroup memory) deferred.

## Status: PARTIAL — wiring + tests + flag-off gates green; flag-on gates FAIL (post-tap capture never settles)

| acceptance | result |
|---|---|
| 1. new `-p acos-cli` tests + `-p acos-cli` suite + `-p acos-art` suite | **PASS** (see §Tests) |
| 2. flag OFF: `v2-art-counter`, `w22-counter-reset`, `gaten-home` pass; `suite/T03-render-wrong` and `negative/gaten-home-wrong` fail with `ASSERTION_FAILED` | **PASS** (`.cache/zprod3/out/flagoff.log`) |
| 3. flag ON: same three pass, the two negatives fail | **FAIL** — `TIMEOUT` (strict settle, `probe-floor`) on the post-tap `assert_text`; see §Flag-on |
| 4. EXPERIMENTS.md E2.18 updated + this file | **PASS** |

Two real defects were found and fixed **because** the flag-on runs were checked for zygote facts rather
than only for `status=pass`:

1. **The first flag-on batch silently ran the exec path.** `acos verify` (which the gates use) built
   its session with `start_with_options`, so `ACOS_ART_ZYGOTE=on` had no effect and the gates "passed"
   as ordinary exec-path runs — a false pass. Fixed: `crates/acos-cli/src/verify.rs` now creates one
   `ZygoteHost` per run (its `Drop` closes the keeper) and calls `start_with_zygote`. Evidence of the
   fix: `out/<scenario>/zygote/zygote/<key>/plan/{ready,0/argv,0/binds}` exist and the keeper log
   carries `ZP1 ready epoch=… boot_ms=14`, `ZP1 forked tag=0 child=8 …`, `ZP1 killed tag=0 pid=8
   kill_rc=0 status=0x9`.
2. **The keeper did not export the guest seams**, so the forked child published no tree probe and the
   first genuinely-zygote run died with `NO_LIVE_FRAME` (`activity_resumed=true probe=false`). Fixed
   in `ZygoteHost::prepare`: the keeper (and therefore every child) now gets
   `ACOSLAUNCH_INTERACTIVE`, `ACOSLAUNCH_LIVE_PROBE`, `ACOSLAUNCH_FRAME_WAIT_SECS`,
   `ACOS_INPUT_ACK_MS`, `ACOS_ZYGOTE_PROBE` — the same set Z4's `drive.launch_env` gave the keeper.

Nothing below is claimed beyond what was measured. Where the host ART lock blocked a run, this file
says so instead of guessing.

## What landed

| file | change |
|---|---|
| `crates/acos-protocol/src/error.rs` | `ErrorCode::ZygoteDead` → `"ZYGOTE_DEAD"`, class `host_guard`, retryable, hint, `ALL_ERROR_CODES` |
| `crates/acos-cli/src/engine/zygote_host.rs` | **new**: `ZygoteHost` (daemon-owned keepers, `Mutex`-interior), `ZygoteFacts`, `zygote_dead()`, `reply_fields()`, `ISOLATION_ZYGOTE_CHILD`, `MEMORY_SCOPE_ZYGOTE`, 4 tests with a mock keeper |
| `crates/acos-cli/src/engine/art.rs` | `start_with_zygote` + `start_inner` (hook: prepare → `ACOS_ZYGOTE_SESSION`/`ACOS_ZYGOTE_PROBE` → launcher spawn → `argv`+`binds` → `spawn <tag>` → ready line); `reset` zygote branch (launcher group down → `kill <tag>` → same-tag relaunch/spawn); `zygote_facts`/`zygote_previous_child_pid`/`zygote_reply_fields`; `close` kills its own child; `zygote_key_of` |
| `crates/acos-cli/src/engine/serve.rs` | `ServeEngine` owns `Arc<ZygoteHost>`; launch uses `start_with_zygote`; `merge_zygote_fields` on launch/reset/status replies; `shutdown()` calls `shutdown_all()` |
| `crates/acos-cli/Cargo.toml` | `acos-art` dependency (the engine now drives `PersistentZygote` directly) |
| `crates/acos-art/src/zygote_prod.rs` | `keeper_pid()`, `ZygoteRegistry::{keeper_pids, child_pid_of}` (leak assertions) |

Flag-off guarantee: `start_with_options` delegates to `start_inner(..., None)`; `ZygoteHost` with the
flag off never boots a keeper and creates no directory; `merge_zygote_fields` returns early when
`reply_fields` is `None`, so the exec-path replies are unchanged.

## Tests (green, no ART needed)

```bash
WT=/home/Qui/.paseo/worktrees/3oxa6xao/g3cd; cd $WT
export CARGO_TARGET_DIR=$WT/.cache/g3cd-target
tools/guarded.sh --mem 12G --time 600 -- cargo test --release --offline -p acos-cli --bins \
  engine::zygote_host
# 4 passed; 0 failed — the_flag_off_default_touches_no_zygote,
# the_reply_fields_exist_only_for_a_real_zygote_child,
# a_dead_keeper_is_a_zygote_dead_error_and_never_a_session,
# shutdown_all_leaves_no_keeper_and_no_child_behind
```

| test | what it proves |
|---|---|
| `the_flag_off_default_touches_no_zygote` | `ACOS_ART_ZYGOTE=off` ⇒ `enabled()==false`, 0 keepers, no `zygote/` directory, no pid |
| `the_reply_fields_exist_only_for_a_real_zygote_child` | `reply_fields(None,..) == None`; with facts: `isolation:"fresh-zygote-child"`, `child_pid`, `previous_child_pid` (null on a first launch), `run_root`, `zygote_boot_epoch`, `memory_scope` says shared scope + deferred |
| `a_dead_keeper_is_a_zygote_dead_error_and_never_a_session` | a keeper that exits before `ready` ⇒ `ZYGOTE_DEAD` error, `ErrorCode::ZygoteDead` (`host_guard`), `details.zygote.fallback:"none …"`, registry still empty (nothing runnable was returned) |
| `shutdown_all_leaves_no_keeper_and_no_child_behind` | mock keeper: 2 tags spawn distinct children, reset path (`kill` + `prepare(Some(tag))` + `spawn`) reuses the tag, keeps the epoch, forks a new pid; after `shutdown_all()` the registry is empty and `/proc/<keeper>` is gone |

## Gate runs (flag off + flag on)

Runner: `.cache/zprod3/run_flags.sh <off|on> <tag>` — runs
`v2-art-counter`, `w22-counter-reset`, `gaten-home` (must pass) and
`suite/T03-render-wrong`, `negative/gaten-home-wrong` (must fail with their own codes), JSON under
`.cache/zprod3/out/`, log lines in `.cache/zprod3/out/<tag>.log`. Launched **through the main
checkout's lock**, one acquisition per flag:

```bash
/home/Qui/Projects/acos/tools/art-runtime/run-exclusive.sh --task ZPROD3 -- \
  tools/guarded.sh --mem 8G --time 900 -- .cache/zprod3/run_flags.sh off flagoff
/home/Qui/Projects/acos/tools/art-runtime/run-exclusive.sh --task ZPROD3 -- \
  tools/guarded.sh --mem 8G --time 900 -- .cache/zprod3/run_flags.sh on flagon
# flag on also needs the zygote guest build:
export ACOS_ART_ZYGOTE_BUILD=/home/Qui/Projects/acos/.cache/zprod1-build   # set by run_flags.sh
```

### Flag OFF — PASS (all five, measured on this tree)

```
rc=0 v2-art-counter          v2-art-counter flagoff:        status=pass code=None
rc=0 w22-counter-reset       w22-counter-reset flagoff:     status=pass code=None
rc=0 gaten-home              gaten-home flagoff:            status=pass code=None
rc=1 suite/T03-render-wrong  suite/T03-render-wrong flagoff: status=fail code=ASSERTION_FAILED
rc=1 negative/gaten-home-wrong  negative/gaten-home-wrong flagoff: status=fail code=ASSERTION_FAILED
```

Evidence: `.cache/zprod3/out/flagoff.log` + `flagoff-<scenario>.json` (ephemeral under `.cache/`).

### Flag ON — FAIL (mechanism works, one step does not settle)

The zygote really runs on real ART: keeper `ready epoch=30988051884 boot_ms=14`, `ZP1 forked tag=0
child=8`, `ZP1 killed tag=0 pid=8 kill_rc=0 status=0x9`, and the session captured real frames
(`frame_seq` 5 and 7, distinct content hashes). Step 4 (`tap inc`) **passes** with
`settle.pixels_changed=true`, `first_change_ms=66`, `frame_ms=929`.

Step 5 (`assert_text count == "1"`) needs a capture and fails:

```
status=timeout code=TIMEOUT
"capture: strict settle timed out (probe-floor)"
details.settle = {"mode":"strict","bound":"timeout","probe_seq":41,"tree_age_ms":1639,
                  "reason":"probe-floor","settled":false}
```

i.e. after the tap the guest's probe sample stays ~1.6 s old and no sample newer than the request
arrives inside the engine's strict budget, so the capture (and therefore the assertion) can never be
evidence. `ACOS_ART_FRAME_WAIT_SECS=30` (Z4's guest window) does not change it, so it is not the
settle window. Not diagnosed further in this task: the cost/benefit crossed the hour rule; the facts
above are enough to start. Evidence: `.cache/zprod3/out/on1.log`, `on1.json`,
`out/v2-art-counter/{steps/04-tap.json,steps/00..04,tree/5.json,tree/7.json,zygote/**}`.

**No flag-on claim is made.** The reply fields (`child_pid`, `previous_child_pid`, `run_root`,
`zygote_boot_epoch`, `isolation`, `memory_scope`) are proven by unit tests and exercised by the
running keeper, but no gate run in which they appear in a *passing* reset reply exists yet.

## Test suites (acceptance 1)

```
.cache/zprod3/cli-final.log   (rerun on the final tree, after the verify/keeper-env fix)
  cli_final_rc=0    # cargo test --release --offline -p acos-cli → 32 targets, every "test result: ok"
.cache/zprod3/tests.log       (earlier batch on the wiring commit)
  cli_rc=0            # cargo test --release --offline -p acos-cli  → all targets ok, 0 failed
  art_skip_rc=0       # cargo test -p acos-art -- --test-threads=1 --skip refuses_to_run_when_no_cgroup_can_be_created
  art_exact_rc=101    # the shipped `--exact` sandbox_s5 test FAILED in that batch …
```

`sandbox_s5::refuses_to_run_when_no_cgroup_can_be_created` failed once inside the batch run
(`a host without a cgroup must refuse the run: RunCgroup { cg: "", … }`) and **passed when rerun
alone**, twice — the documented order/`PATH`-probe sensitivity of that test, not this diff (the
`acos-art` changes are additive: `keeper_pid`, `ZygoteRegistry::{keeper_pids, child_pid_of}`).
Rerun alone (green):

```bash
tools/guarded.sh --mem 8G --time 200 -- cargo test --release --offline -p acos-art   --test sandbox_s5 -- --test-threads=1 --exact refuses_to_run_when_no_cgroup_can_be_created
# test result: ok. 1 passed; 0 failed
```

## What is NOT done

1. **Flag-on gate evidence** — blocked on the post-tap probe-floor settle (§Flag-on). This is the
   only thing between this track and acceptance 3.
2. **ZYGOTE_DEAD restart / new epoch / refork** — ZPROD4. A dead keeper already fails closed with
   `ZYGOTE_DEAD` (no exec fallback, session closed), but nothing restarts it.
3. **F3 per-child cgroup memory** — deferred; hence `memory_scope:"shared-scope …"` in every reply.

## Smallest next action

Take the flag-on `v2-art-counter` run and instrument the post-tap traversal: is the app's probe
still sampling after the gesture (`ACOS_ZYGOTE_PROBE`, `probe_seq` growth per request), or is the
launcher answering `tree` from a cached sample? Z4's `drive.Session.tree()` with
`ACOSLAUNCH_FRAME_WAIT_SECS=30` never hit this; the engine's strict settle additionally requires a
sample **newer than the request**, so compare one hand `{"cmd":"tree"}` sent twice through the
launcher with `probe_seq` printed for each reply. If `probe_seq` is identical for two consecutive
requests, the child's probe is demand-cached and the engine's probe-floor rule needs the demand
refresh plumbed through the zygote path.
