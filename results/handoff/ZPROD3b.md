# ZPROD3b handoff — the flag-on probe path: the demand probe now reaches the zygote child

Branch `zygote-prod-1`, base `e709696` (ZPROD3). Design
`docs/decisions/2026-11-14-zygote-prod.md` §2–§6; `EXPERIMENTS.md` §E2.18.
Task: ZPROD3's open item — with `ACOS_ART_ZYGOTE=on` the post-tap capture never
settled (`capture: strict settle timed out (probe-floor)`, `probe_seq=41`,
`tree_age_ms=1639`), while flag OF F was green.

## Status: root cause found + fixed; flag-ON gates PASS; flag-OFF gates PASS

| acceptance | result |
|---|---|
| 1. root cause named with trace evidence; fix + a test for the demand-probe path under the zygote | **PASS** — `.cache/zprod3b/on.log`, `out/*/zygote/**/keeper.log`, 3 new unit tests (`cargo test -p acos-art --lib probe_channel`) |
| 2. flag ON: three gates pass, both negatives fail with their codes | **PASS** — `.cache/zprod3b/out/on.log` |
| 3. flag OFF unchanged + `-p acos-cli` / `-p acos-art` suites green | **PASS** — `.cache/zprod3b/out/off.log`, `.cache/zprod3b/cli.log` (`CLI_RC=0`), `.cache/zprod3b/art.log` (`ART_RC=0`) |
| 4. flag ON vs OF F latency (M4/M6 strict p50) | **not run** — the ART lock queue (two other tracks' 900–2400 s acquisitions) left no window inside the hour; no number is claimed |
| 5. E2.18 updated + this file | **PASS** |

## Root cause (proven, not hypothesised)

The guest's demand-probe channel is its **stdin**: `LaunchMain.startLiveProbe`
reads one `PROBE` line from `System.in` and wakes the probe thread
(`crates/acos-art/guest/java/dev/acos/art/LaunchMain.java`,
`acos-probe-control`). The launcher's strict settle asks for that wake
(`request_guest_probe` → `writeln!(stdin, "PROBE")`).

* **Exec path:** `child` is the ART process itself, spawned with
  `stdin(Stdio::piped())` (`launch.rs`, the `ACOSLAUNCH_INTERACTIVE` branch), so
  `child.stdin.take()` is the guest's own fd 0. The wake lands. (Flag OFF green.)
* **Zygote path:** the keeper is a *zygote host*, spawned by
  `zygote_prod::PersistentZygote::start` with **`.stdin(Stdio::null())`**, and the
  app is a `fork()` of that keeper (`ZygoteApp.z4ForkOne`) — so the app child's
  fd 0 is `/dev/null`. Meanwhile the session launcher does **not** run ART in
  this mode: its `child` is the `/system/bin/sleep` tombstone
  (`launch.rs`, `cmd = sb.command("/system/bin/sleep", …)` for
  `session_dir.is_some() && zygote_app.is_none()`). The `PROBE` wake was written
  to the **sleep stub's** stdin pipe and discarded.

That is exactly the observed symptom: after the tap the child's own dirty draws
keep emitting samples (the strict tap step passes, `pixels_changed=true`), but
once the UI is idle nothing new is emitted; the following `tree` request sets
`entry_floor = latest_probe().seq` (41), asks for a sample, and the wake never
arrives — `probe-floor`, `tree_age_ms` = the age of sample 41. Raising
`ACOS_ART_FRAME_WAIT_SECS` could not help (it is not the settle window).

Trace evidence, after the fix (same shape the old run lacked):

```
out/w22-counter-reset/zygote/zygote/9f08ca59-…/keeper.log
  "ZP1 ready epoch=33137279661 boot_ms=14 threads=6
   ZP1 child tag=0 probe_stdin=on          ← the child now owns the channel
   ZP1 forked tag=0 child=8 …
   ZP1 killed tag=0 pid=8 kill_rc=0 status=0x9
   ZP1 child tag=0 probe_stdin=on          ← reset: same keeper/epoch, new child
   ZP1 forked tag=0 child=35 …
   ZP1 killed tag=0 pid=35 kill_rc=0 status=0x9"
```

Before the fix the keeper log had **no** `probe_stdin` line at all (the guest
never opened a channel), and the engine's capture reported the probe-floor
timeout quoted above.

## Fix

| file | change |
|---|---|
| `crates/acos-art/src/launch.rs` | `GuestProbeChannel { Pipe, Fifo }` replaces `Option<ChildStdin>` in the three settle/tree waiters; `open_guest_probe_fifo` creates `<session>/probe.fifo` (`0600`, opened **read-write** like the keeper's `cmd` FIFO, so the child's blocking open never waits and a vanished reader cannot `EPIPE` the next wake) and `launch_session` opens it **before** `<session>/argv` is published, i.e. before the host can send `spawn <tag>`; the exec path keeps `child.stdin` unchanged (`probe_fifo` is `None` without `ACOS_ZYGOTE_SESSION`) |
| `research/zygote-proto/z3/java/dev/acos/art/ZygoteApp.java` | `z4ProbeStdin(sessions, tag)`: the forked child opens `<sessions>/<tag>/probe.fifo` and `System.setIn`s it **before** `spawnSession` (no namespace/seccomp/Landlock rule exists yet, and fd 0 is inherited by the pidns init and app processes); called in `z4ForkOne` (persistent) and `z4Main` (Z4 cohort). Logs `probe_stdin=on` |
| `crates/acos-art/src/probe_channel_tests.rs` | **new** 3 tests (see below) |

Strict settle is untouched: the rule "a probe sample newer than the request"
still holds, and a stale sample still cannot back a capture. Nothing in the
engine needed to change.

## Tests (green, no ART)

```bash
WT=/home/Qui/.paseo/worktrees/3oxa6xao/g3cd; cd $WT
export CARGO_TARGET_DIR=$WT/.cache/g3cd-target
tools/guarded.sh --mem 12G --time 600 -- cargo test --release --offline -p acos-art --lib probe_channel
# test result: ok. 3 passed; 0 failed
```

| test | what it proves |
|---|---|
| `every_probe_wake_reaches_the_zygote_child_side_of_the_fifo` | the session creates the FIFO before any reader exists; three `PROBE` wakes (settle entry, mid-wait) arrive on the child side in order, nothing dropped; mode `0600` |
| `a_vanished_reader_cannot_break_the_next_wake` | the host half is read-write: after the child's reader is gone the next wake still writes without `EPIPE` (a reset/dead child cannot poison the launcher command loop) |
| `the_forked_child_installs_the_probe_fifo_as_stdin_before_its_sandbox` | the guest source (the exact file `research/zygote-proto/z3/build.sh` compiles) opens `<sessions>/<tag>/probe.fifo`, `System.setIn`s it, and does so **before** `spawnSession`; the Z4 cohort path does the same; the name matches the host's `GUEST_PROBE_FIFO` |

## Gate runs

Runner: `.cache/zprod3b/run_flags.sh <off|on> <tag>` — the ZPROD3 runner with
`ACOS_ART_ZYGOTE_BUILD=$WT/.cache/zprod3b-build` (the guest rebuilt from this
tree: `ACOS_CACHE=$WT/.cache ZBUILD=zprod3b-build research/zygote-proto/z3/build.sh`).
Each flag is one lock acquisition through the main checkout:

```bash
/home/Qui/Projects/acos/tools/art-runtime/run-exclusive.sh --task ZPROD3b -- \
  tools/guarded.sh --mem 8G --time 900 -- .cache/zprod3b/run_flags.sh on on
/home/Qui/Projects/acos/tools/art-runtime/run-exclusive.sh --task ZPROD3b -- \
  tools/guarded.sh --mem 8G --time 900 -- .cache/zprod3b/run_flags.sh off off
```

### Flag ON — PASS, all five

```
rc=0 v2-art-counter              status=pass code=None
rc=0 w22-counter-reset           status=pass code=None
rc=0 gaten-home                  status=pass code=None
rc=1 suite/T03-render-wrong      status=fail code=ASSERTION_FAILED
rc=1 negative/gaten-home-wrong   status=fail code=ASSERTION_FAILED
```

Evidence `.cache/zprod3b/out/on.log` + `on-<scenario>.json`. The formerly
failing step `04-assert_text` (post-tap, strict capture) is `status=pass`
(`out/w22-counter-reset/steps/04-assert_text.json`); the tap step passed with
`settle.mode=strict bound=strict-event pixels_changed=true first_change_ms=75`.

Zygote facts (from the same run's artifacts):

* every session ran as a real zygote child — the keeper log carries
  `probe_stdin=on` + `ZP1 forked … child=<pid>`; the session launcher in this
  mode starts only the `/system/bin/sleep` tombstone, so no exec-path ART exists
  to fall back to;
* `w22-counter-reset` reset after the tap: same keeper and epoch
  (`ZP1 ready epoch=33137279661` once), child pid **8 → 35**, old child killed
  `status=0x9`; the post-reset `assert_text` reads counter `0`
  (`steps/06…08` pass).

### Flag OFF — PASS, all five (unchanged behaviour)

```
rc=0 v2-art-counter              status=pass code=None
rc=0 w22-counter-reset           status=pass code=None
rc=0 gaten-home                  status=pass code=None
rc=1 suite/T03-render-wrong      status=fail code=ASSERTION_FAILED
rc=1 negative/gaten-home-wrong   status=fail code=ASSERTION_FAILED
```

Evidence `.cache/zprod3b/out/off.log` + `off-<scenario>.json`. The host is disabled in this
run: every file under the artifact roots' `zygote/` predates the off run
(`find out -path '*/zygote/*' -newermt '<off start>'` is empty), i.e. flag OFF created no
keeper, plan or child.

## Test suites

```
.cache/zprod3b/cli.log   CLI_RC=0   # cargo test --release --offline -p acos-cli
.cache/zprod3b/art.log   ART_RC=0   # cargo test --release --offline -p acos-art -- --test-threads=1 \
                                    #   --skip refuses_to_run_when_no_cgroup_can_be_created
```

## What is NOT done

1. **Flag ON vs OFF latency** (acceptance 4) — not measured; no claim.
2. **F3 per-child cgroup memory** — deferred (ZPROD4/P3), unchanged.
3. **ZYGOTE_DEAD restart / new epoch / refork** — ZPROD4.
4. The FIFO is per session tag; a keeper that serves several tags keeps one
   channel per tag by construction, but only tag 0 was exercised by the gates
   (`acos verify` uses one session per run).

## Smallest next action

Measure flag ON vs OF F strict settle latency (M4/M6 p50) with the runner in
acceptance 4, interleaved ×2, now that the flag-on gates are green; then
ZPROD4 (dead-keeper restart/new epoch/refork) on top of this.
