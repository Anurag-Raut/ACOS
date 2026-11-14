# ZPROD4 handoff — main merge, dead-keeper restart, latency A/B status

Branch `zygote-prod-1`, merge of `main` `389429a` (`7c715ad`), then ZPROD4
commits. Design `docs/decisions/2026-11-14-zygote-prod.md`; `EXPERIMENTS.md`
E2.18 (same number, per the increment's convention).

## Status

| acceptance | result |
|---|---|
| 1. latency A/B (flag OFF vs ON, 3 reps, interleaved, zygote evidence, load/rep) | **NOT RUN — void window.** Only the OFF arm started before the window closed; no ON number exists, so no pair is claimed. See "Why" below. |
| 2. ZYGOTE_DEAD: current request structured, next request new keeper + new epoch + refork, old sessions lost (not re-pointed) | **PASS at unit level** (mock keeper, SIGKILL between two requests). **Live proof not run** (same window/disk stop). |
| 3. flag OFF: 15 gates + 5 Gaten + 5 negatives | **PASS, all 25 as specified** (`.cache/zprod4/out/off.log`). |
| 3. flag ON: 3 gates + 2 negatives | **first run FAILED (merge regression, found and fixed), re-run after the fix is in `.cache/zprod4/out/on2.log`** |
| 4. E2.18 + this file | **PASS** |

## The merge (both sides kept, exactly)

`git merge --no-edit main` → conflicts in three files; `EXPERIMENTS.md`
auto-merged (main's E2.17, E2.19–E2.21 + our E2.18 both present).

* `crates/acos-cli/src/engine/art.rs`
  * `ArtSession`: kept the ZPROD3 fields (`zygote`, `zygote_previous_child_pid`)
    **and** main's `relaunch_retry`.
  * `reset`: main's one-bounded-retry loop (S3b, `relaunch_transport_reason` +
    `preserve_failed_relaunch`) now wraps `ArtSession::start_inner(..,
    zygote_start.clone())` instead of `start_with_options`. With the flag off
    `zygote_start` is `None` so this **is** `start_with_options` (byte-identical
    semantics); with it on the retry re-spawns the same keeper tag.
    `ZygoteStart` got a manual `Clone` for that reason. On success the fresh
    session carries **both** `zygote_previous_child_pid` and `relaunch_retry`.
  * kept main's `boot_image()` accessor next to the ZPROD3 `zygote_facts()` /
    `zygote_previous_child_pid()` / `zygote_reply_fields()`.
* `crates/acos-cli/src/engine/serve.rs` (`reset` reply): kept
  `merge_zygote_fields(&mut reply, session)` **and** the `relaunch_retry` field.
* `crates/acos-art/src/launch.rs`: kept `probe_channel_tests` (ZPROD3b) and
  main's `skipped_traversal_tests` module.
* `crates/acos-art/src/zygote_prod.rs`, `crates/acos-cli/src/engine/zygote_host.rs`:
  no conflict, but see the boot-image fix below.

## Dead keeper (ZPROD4 acceptance 2)

* `ZygoteHost::prepare` — a keeper that is not `alive()` is removed (its exit
  status + log tail already travel in the `ZYGOTE_DEAD` that observed it) and the
  **next** request boots a fresh keeper: new zygote, **new `zygote_boot_epoch`**,
  fresh fork.
* `ZygoteHost::verb_error(key, tag, expected_epoch, error)` — a verb whose keeper
  is gone, or whose epoch is no longer the session's, is reported as
  `ZYGOTE_DEAD` with `details.zygote = {session:"lost", zygote_tag,
  session_boot_epoch, keeper_boot_epoch (null when no keeper), repointed:false}`.
  A lost session is **never** re-pointed at the new keeper.
* `ZygoteHost::live_boot_epoch` — only a live keeper's epoch answers, so a dead
  keeper's cached epoch string cannot make a lost session look current (the first
  version of the test failed exactly here).
* `engine/art.rs` reset — the zygote kill path carries the session's epoch into
  `verb_error` (the `zygote_link` tuple now holds it).
* **Bug found by the new test:** a restarted keeper inherited the dead keeper's
  `plan/ready` (which carries `epoch=`), so `PersistentZygote::start` returned
  immediately and the "new" zygote answered with the **old** epoch.
  `start()` now removes `plan/ready` and `plan/zygote.pid` before spawning (§4.2).
* `ZygoteRegistry::remove` for the restart path.

Test (green, no ART, 0.2 s):

```bash
CARGO_TARGET_DIR=$WT/.cache/g3cd-target \
  tools/guarded.sh --mem 6G --time 300 -- \
  cargo test --release --offline -p acos-cli --bin acos zygote_host::tests
# 5 passed; 0 failed   (a_dead_keeper_restarts_with_a_new_epoch_and_old_sessions_are_lost)
```

The test SIGKILLs the mock keeper between two requests and asserts: the current
request is `ZYGOTE_DEAD` + `session:"lost"` + `repointed:false`; the next request
gets a **new keeper pid**, a **new epoch**, a **new child pid**; the old session
still reports lost and now names the new epoch; a live session of the current
epoch is plain dead, not lost. The mock keeper's child pids now start at its own
pid, so a restarted keeper cannot hand out the dead keeper's pid.

**Live proof (kill the keeper between two `acos serve` requests) is scripted but
NOT run:** `.cache/zprod4/live_dead.sh` (launch `--keep` → `SIGKILL` the keeper →
`session reset` → `launch` again → compare epochs/child pids → destroy + stop
serve). Unrun for the reason below.

## Merge regression on the flag-on path (found, fixed)

First flag-on run after the merge: all five checks `ZYGOTE_DEAD`, keeper exit 1.
Keeper log (`out/v2-art-counter/zygote/zygote/*/keeper.log`, third block):

```
Fatal signal 6 (SIGABRT) ... libartshim.so (__android_log_call_aborter)
Check failed: in_memory || ... Registering a non /system oat file: /data/img/boot.oat
```

`main` made `shared-fixed` the default boot image (`boot_image.rs`), i.e. a
**file** image at `/data/img`; `-Xzygote` sets `only_use_system_oat_files_`, so
ART refuses an oat outside `/system`/APEX — exactly the documented limitation
`results/art-runtime/ZS-security-review.md` **F11** and `Z1.md` §5.2. The zygote
arm had only ever been measured against the pre-merge `lz4` default (in-memory
image). Fix: `ZygoteHost::prepare` pins `ACOS_ART_BOOT_IMAGE=lz4` for the keeper
unless the caller set the knob explicitly (an explicit value is passed through
and still fails closed). This restores the pre-merge flag-on behaviour; it also
means a future OFF/ON comparison must pin the **same** `ACOS_ART_BOOT_IMAGE` on
both arms (the `shared-fixed` P5 gate needs the image provisioned under
`/system`/APEX first — F11).

## Flag OFF — 25/25 as specified (`.cache/zprod4/out/off.log`)

```
pass: v2-art-counter w22-counter-reset w54-dialog-pixels w42-back-frame
      w15-dialog-menu form-validation suite/T03-render-good v1-offscreen-pager
      w7-insets-report se1-expect-change-tap
fail ASSERTION_FAILED: suite/T03-render-wrong w7-insets-carve-negative
      negative/se1-expect-change-noop
fail ELEMENT_OFFSCREEN: negative/offscreen-pager-visible
fail TIMEOUT: p11-progress-timeout
Gaten pass 5/5: gaten-home gaten-tabs gaten-settings gaten-search gaten-lesson
negative Gaten ASSERTION_FAILED 5/5
```

No zygote artifacts were created by the OFF arm (the host is disabled).

## Flag ON after the fix — PASS (`.cache/zprod4/out/on2.log`)

```
rc=0 on2-v2-art-counter              status=pass code=None
rc=0 on2-w22-counter-reset           status=pass code=None
rc=0 on2-gaten-home                  status=pass code=None
rc=1 on2-suite-T03-render-wrong      status=fail code=ASSERTION_FAILED
rc=1 on2-negative-gaten-home-wrong   status=fail code=ASSERTION_FAILED
```

Real zygote evidence (not inferred from `status=pass`) in
`out/<scenario>/zygote/zygote/<key>/keeper.log` of the same tree — the last block
is the merged guest build `.cache/zprod4-build`:

```
ZP1 ready epoch=17206167546 boot_ms=14 threads=6        (v2-art-counter, once)
ZP1 child tag=0 probe_stdin=on
ZP1 forked tag=0 child=8 threads=6 total_ms=38
ZP1 killed tag=0 pid=8 kill_rc=0 status=0x9
-- w22-counter-reset: epoch=17210197100
ZP1 child tag=0 probe_stdin=on / forked child=8
ZP1 killed tag=0 pid=8 kill_rc=0 status=0x9
ZP1 child tag=0 probe_stdin=on / forked child=35   (reset: same epoch, new child)
```

## The A/B, as far as it got (1 OFF rep measured; the ON arm is a NEW finding)

Same binary on both arms (`sha256 0583df84…e077980`, both JSONs record it), 12
latency samples per rep, `--groups "a:latency:8G:300" --latency-reps 12`.

OFF rep 1 (`.cache/zprod4/ab/off-1.json`, complete, load avg 1.5, mem available
31.5 GiB):

| metric | p50 |
|---|---:|
| M4 launch strict | 307.4 ms |
| M4 launch fast | 156.2 ms |
| M6 reset strict | 331.8 ms |
| M6 reset fast | 199.0 ms |
| M8 tap counter strict / fast | 961.1 / 44.6 ms |
| M8 tap form strict / fast | 939.4 / 45.5 ms |
| M5 screenshot | 24.1 ms |
| teardown | 12.0 ms (wall 18.0 ms), n=28 |

**ON rep 1 produced NO number and is void**: every reference launch failed with
`ZYGOTE_DEAD: `argv 0` was not acked within 180002 ms (keeper not responding)`
(`.cache/zprod4/ab/on-1.json`, `run-on-1/rep-1/latency.json`, `wall_s=180.25`).
The keeper itself boots fine (`boot_image: mode=lz4 … ACOS_ART_BOOT_IMAGE=lz4`,
`Z3 zygote guest …/zprod4-build`) — the stall is in the **session-launcher
contract** under the benchmark's daemon path (the launcher never published
`<plan>/0/argv` inside 180 s), i.e. a defect this increment did not exist before
only because the flag-on path had never been driven through `acos-bench`'s
`serve` cohort. It is a real, reproducible-by-invocation finding, not noise: the
same binaries pass `acos verify` on the same APK, so the difference is the daemon
cohort, not the guest. The chain was stopped there (the next thing it would have
measured is the same stall).

## Why the rest of the A/B is not measured

Two hard stops hit inside the one-hour window:

1. **ART lock queue.** The suite queued 18 min; other tracks (SE2, K5D, G3F1,
   int) held 1–2 slots continuously. After the OFF gates passed, the chain
   started OFF rep 1 and ON rep 1 — the ON rep was **void by construction** (the
   merge regression above), so the chain was stopped rather than record a fake
   ON arm.
2. **The ON arm's `argv 0` contract stall** (above).
3. **Disk at the 85 % stop line** (`df -h /`: 139 G free, 85 % used, having
   fallen 10 G in 40 min from other tracks' writes). The standing rule is
   "stop + report at ≥ 85 %", so no six-acquisition A/B and no 16 G N=10 loop
   were started after the fix. Deleting this task's own finished scratch
   (`.cache/zprod3*`, 442 MB) did not move the line.

Nothing about the zygote's latency is claimed. The bench invocation to run
(unchanged from the task) is in `.cache/zprod4/chain.sh`:

```bash
# per arm per rep, OFF interleaved with ON, 3 reps
ACOS_BENCH_TASK=ZPROD4 tools/bench/acos-bench.sh --reps 1 \
  --groups "a:latency:8G:300" --latency-reps 12 --no-build \
  --target-dir $WT/.cache/g3cd-target --out … --run-dir … --scratch-root …
# ON arm additionally: ACOS_ART_ZYGOTE=on ACOS_ART_ZYGOTE_BUILD=$WT/.cache/zprod4-build
#   (+ pin the SAME ACOS_ART_BOOT_IMAGE on both arms; see F11 above)
```

An ON arm is only valid with zygote evidence: the bench artifacts' `zygote/`
keeper log (epoch, `probe_stdin=on`, `forked`) or the reply fields
(`isolation`, `child_pid`, `zygote_boot_epoch`).

## Remaining

1. **Fix the ON-arm `argv 0` stall in the daemon cohort** (first: get the bench
   ON scratch's keeper log — the bench deletes its scratch unless `--keep 1`; re-run
   one ON group with `--keep 1` and read `<scratch>/…/keeper.log` +
   `<scratch>/…/plan/0/`), then run the A/B below. (3 interleaved reps, same boot image on both arms, load per
   rep) and the N=10 loop row.
2. Run `.cache/zprod4/live_dead.sh` for the live dead-keeper proof.
3. `cargo test -p acos-cli` and `-p acos-art` (with the cgroup test `--exact`
   separately) under one lock acquisition each — not re-run post-merge here.
4. F3 per-child cgroup memory (deferred).
