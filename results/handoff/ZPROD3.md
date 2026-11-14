# ZPROD3 handoff — engine wiring for the production zygote (`ACOS_ART_ZYGOTE=on`)

Branch `zygote-prod-1`, base `e4cca7b` (ZPROD2). Design
`docs/decisions/2026-11-14-zygote-prod.md` §2–§6. `EXPERIMENTS.md` §E2.18.
Orchestrator decision: this track is **wiring + reply fields + the flag-off/flag-on counter gate
pair**; ZYGOTE_DEAD *restart + new epoch + refork* is ZPROD4, F3 (per-child cgroup memory) deferred.

## Status: PARTIAL — wiring + tests landed; real-ART gate evidence not yet produced

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

Results: see `.cache/zprod3/out/{flagoff,flagon}.log` (the log lines are the acceptance evidence;
they are copied into §"Gate evidence" of this file when the lock run completes).

## What is NOT done

1. **Real-ART gate evidence** for this tree (flag off and flag on): the runs were queued on the host
   ART lock (`--exclusive` bench + `G3F1` + `PHONE2` holders) and had not started at the hour limit.
   Run the two commands above; they are the acceptance pair from the task brief.
2. **ZYGOTE_DEAD restart / new epoch / refork** — next track (ZPROD4). A dead keeper already fails
   closed with `ZYGOTE_DEAD` here (no exec fallback, session closed), but there is no automatic
   restart, so a dead keeper stays dead for the life of the serve.
3. **F3 per-child cgroup memory** — deferred; hence `memory_scope:"shared-scope …"` in every reply.

## Smallest next action

Run the flag-off acquisition above; if `v2-art-counter`, `w22-counter-reset` and `gaten-home` pass
and `T03-render-wrong`/`gaten-home-wrong` fail with their codes, the flag-off acceptance is done;
then run the flag-on acquisition (same script, `on`) and compare `child_pid` changes +
`zygote_boot_epoch` stability in `.cache/zprod3/out/flagon-w22-counter-reset.json`.
