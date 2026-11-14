# ZPROD2 handoff — step 1 done (`PersistentZygote`), step 2 (engine wiring) NOT done

Branch `zygote-prod-1`, HEAD after `git merge main` (`0c0b322`) + the step-1 commit.
Design: `docs/decisions/2026-11-14-zygote-prod.md`. `EXPERIMENTS.md` §E2.18.

## Status: PARTIAL — step 1 landed and tested, step 2 not started

The task had two steps. **Step 1 is complete and green**; step 2 (wire the keeper into
`acos serve` under `ACOS_ART_ZYGOTE=on`, `ZYGOTE_DEAD`, reply fields) was **not written** in this
task: the hour ran out. The gate runs (flag off and flag on) were **not executed** — the run I
started sat in the host ART-lock queue behind another agent's `--exclusive` bench, `G3F1` and `K5C`
holders and my tool timeout killed the *wait* (no gate JSON was produced, nothing was changed).

Nothing is claimed that was not measured.

## What is done (step 1)

`crates/acos-art/src/zygote_prod.rs` — the host half of the production zygote, the Rust port of
`research/zygote-proto/z4/reset.py` steps 1–4:

* `PersistentZygote::start(root, key, binary, apk, env, zygote_build)` — plan dir
  `<art-context>/zygote/<key>/plan`, `mkfifo(plan/cmd, 0600)` opened `O_RDWR` (so a writer restart
  never produces a spurious `cmd eof`), the keeper launcher (`acos-art launch` with
  `ACOS_ZYGOTE_APP` / `ACOS_ZYGOTE_PLAN_DIR` / `ACOS_ZYGOTE_N=0` / `ACOS_ZYGOTE_PERSIST=1`,
  `ACOS_ZYGOTE_SESSION` removed), then `<plan>/ready` + `<plan>/zygote.pid` with **timeouts**
  (`ACOS_ART_ZYGOTE_ACK_MS`, default 30 s; ready 180 s).
* `spawn(tag)` → writes `spawn <tag>`, waits for the `<plan>/<tag>/spawned` ack the **zygote** wrote,
  returns `ZygoteChild { tag, child_pid, run_root }`.
* `kill(tag)` → `kill <tag>`, waits `<plan>/<tag>/reaped` (`status=0x9`), returns the pid it killed.
  The zygote ends that child's **process group**; the keeper never uses `cgroup.kill`
  (orchestrator decision 1: F3/per-child cgroup deferred to P3, children share the keeper's scope).
* `boot_epoch()` (from `ready`), `zygote_pid()`, `boot_ms()`, `alive()`, `dead_reason()` (exit status
  + last 20 keeper-log lines), `prepare_session(tag)` (fresh `run`+`sock`, every ack/contract file
  removed, the seven bind mount points recreated), `publish_binds(tag)` (the Z4 `binds` contract),
  `wait_contract(tag)` (waits for the launcher's `argv`, then writes `binds`), `reset(tag)` =
  `kill` + `prepare_session` + `spawn`, `shutdown()`/`Drop` (`exit` on the FIFO, then a 3 s grace,
  then kill+wait — the keeper can never be leaked past serve exit).
* `ZygoteRegistry` — one keeper per `ZygoteKey` (`apk, boot_image, width, height, density, insets,
  animations`, `dir_name()` is stable/path-safe), monotonic tags (never reused, so a stale contract
  can never answer a new spawn), `shutdown_all()` + `Drop`.
* `ZygoteError` → `acos_art::Error::Zygote`: **every** failure starts with `ZYGOTE_DEAD` and carries
  the exit status and log tail. There is no code path that falls back to the exec path.
* `PersistentZygote::idle_exit_s()` reads `ACOS_ART_ZYGOTE_IDLE_S` (orchestrator decision 2: default
  `0` = live until `acos serve` ends). The *guest* side of that idle exit is not implemented yet
  (it needs the zygote to time itself out), so the knob is parsed and reported only.

## Verified in this task (exact commands)

```bash
WT=/home/Qui/.paseo/worktrees/3oxa6xao/g3cd; cd $WT
export CARGO_TARGET_DIR=$WT/.cache/g3cd-target
tools/guarded.sh --mem 12G --time 900 -- cargo test --release --offline -p acos-art --lib \
  -- --test-threads=1 --skip refuses_to_run_when_no_cgroup_can_be_created
#   → 107 passed; 0 failed (7 of them are the new zygote_prod tests)
tools/guarded.sh --mem 12G --time 900 -- cargo test --release --offline -p acos-art
#   → every target ok (8+4+6+1+2+7+5+3+5+5+4 passed …), 0 failed
tools/guarded.sh --mem 12G --time 600 -- cargo test --release --offline -p acos-art \
  --test sandbox_s5 -- --test-threads=1 --exact refuses_to_run_when_no_cgroup_can_be_created
#   → 1 passed
tools/guarded.sh --mem 12G --time 900 -- cargo test --release --offline -p acos-cli
#   → 147 passed; 0 failed (flag off; acos-cli was not modified in this task)
```

New tests (`crates/acos-art/src/zygote_prod.rs::tests`):

| test | what it proves |
|---|---|
| `key_dir_names_are_stable_distinct_and_path_safe` | one keeper per `(apk, image, geometry, insets, animations)`; no `/` in the dir |
| `kv_and_pid_contracts_parse_z4_shapes` | Z4's `k=v` and bare-pid contract files; a missing file is `None`, never a fake pid |
| `prepare_session_wipes_the_previous_run_root_and_contract` | `run/app-data/state.txt` gone, the 7 mount points back, stale `spawned`/`pid`/`argv` removed |
| `publish_binds_writes_the_childs_run_root_and_socket_contract` | the 9-row `binds` contract, per-tag paths |
| `a_dead_keeper_is_a_structured_error_never_a_write` | `ZYGOTE_DEAD` + the log tail as evidence; no write is attempted |
| `the_keeper_protocol_spawn_kill_reset_and_shutdown` | a **mock keeper** (real protocol, no ART): spawn pid 101, reset → previous 101 / fresh 102, run root wiped, **epoch and zygote pid unchanged**, `shutdown()` leaves no process (`/proc/<keeper>` gone) |
| `one_session_bad_tag_is_a_structured_error_not_a_hang` | a missing ack is a named `ZYGOTE_DEAD` timeout, not a hang |

## What is NOT done — the exact next tasks

1. **Step 2, engine wiring** (`crates/acos-cli/src/engine/{serve,art}.rs`):
   * a `ZygoteHost` (in the engine, interior-mutable since `ServeEngine` methods take `&self`):
     `enabled()` = `ACOS_ART_ZYGOTE=on`; keeper per `ZygoteKey`; `allocate_tag()`;
     `restart(key)` for the `ZYGOTE_DEAD` path (new plan dir → new epoch).
   * `ArtSession::start_with_options` grows an `Option<(&mut PersistentZygote, u32)>`: when present
     it sets `ACOS_ZYGOTE_SESSION=<plan>/<tag>`, calls `prepare_session` before the launcher starts,
     then `wait_contract` + `spawn` **after** the launcher is spawned and **before** the ready line
     is read (single-threaded; the launcher publishes `argv` during launch). When absent the argv and
     the call order stay byte-identical — that is the flag-off guarantee.
   * `ArtSession` carries `zygote: Option<(Arc<Mutex<PersistentZygote>>, tag, child_pid, run_root,
     epoch)>`; `reset()` gets a zygote branch: `keeper.kill(tag)` → `self.shutdown()` (old launcher's
     group) → `start_with_options(zygote…)`; reply gains `isolation:"fresh-zygote-child"`,
     `child_pid`, `previous_child_pid`, `run_root`, `zygote_boot_epoch`; `status_value` too.
   * `ZYGOTE_DEAD`: add `ErrorCode::ZygoteDead` (`acos-protocol`, `HostGuard` class) and map
     `acos_art::Error::Zygote` to it; on the dead path restart the keeper (new epoch), refork on the
     next request, and **never** answer from the exec path while reporting zygote.
   * `ServeEngine::shutdown` must call `ZygoteHost::shutdown_all()` (keeper not leaked after serve
     exit) and a test must assert the keeper pid is gone.
   * property to state in replies: memory is **not** per-child under the zygote (F3 deferred).
2. **Flag-off gate list** (11 pass / 3 fail) + 5 Gaten flows + 5 negatives,
   `ACOS_ART_ZYGOTE` unset.
3. **Flag-on**: `gate-v2-art-counter`, `gate-w22-counter-reset`, `scenarios/gaten-home.yaml` 3/3,
   negatives `suite/T03-render-wrong` + `scenarios/negative/gaten-home-wrong.yaml` failing with their
   codes; `child_pid` changes across reset, `zygote_boot_epoch` stable, counter `0` after a tap+reset
   **proven by the tree**.
4. Flag-on vs flag-off quick number:
   `ACOS_BENCH_TASK=ZPROD2 tools/bench/acos-bench.sh --reps 1 --groups "a:latency:8G:300"
   --latency-reps 12 --no-build --target-dir <dir>` (the bench takes the lock itself; never wrap it),
   interleaved ×2 if time allows; it reports M4/M6 strict p50.

## Reproducing this task's evidence

```bash
WT=/home/Qui/.paseo/worktrees/3oxa6xao/g3cd; cd $WT
export ACOS_FONT_DIR=/home/Qui/Projects/acos/.cache/boot/android-34/fonts
export CARGO_TARGET_DIR=$WT/.cache/g3cd-target
CARGO_TARGET_DIR=$WT/.cache/g3cd-target tools/guarded.sh --mem 12G --time 900 -- \
  cargo test --release --offline -p acos-art --lib zygote_prod      # 7/7, ~0.1 s, no ART
```

Gate runner copied for this task: `.cache/zprod2/run_suite.sh` (target `$WT/.cache/g3cd-target`,
output `$WT/.cache/zprod2/suite`, artdata `$WT/.cache/zprod2/artdata`) and the single-gate helper
`.cache/zprod2/one_gate.sh <scenario> <tag>`. **Neither has been run to completion yet** — the host
was holding both ART slots (`int` bench `--exclusive`, `G3F1`, `K5C-T2`) at the end of the hour.
