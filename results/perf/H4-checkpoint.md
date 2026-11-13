# H4 checkpoint — idle CPU/wakeups (paused for owner rendering research)

- **Date / commit:** 2026-11-13, worktree `3oxa6xao/h4`, branch `perf-h4-idle-cpu` off `main 11b917c`. WIP commit: see `git log -1` (this checkpoint).
- **Status:** implementation done and builds; idle profile before/after measured. Latency A/B, gates, EXPERIMENTS/Leaderboard/Backlog and `results/bench/H4.md` are still to do after resume.

## Measured so far (60 s idle windows, real ART, same host)

Profiler: `.cache/h4/profile_idle.py` (scratch, not committed), per-thread `/proc/<pid>/task/<tid>/{stat,status,schedstat,wchan}`. Official leader metric = `/proc/<pid>/status` of each process leader (the bench's `tree_delta`).

| metric | base N=1 | base N=10 | fixed N=1 | fixed N=10 |
|---|---:|---:|---:|---:|
| official wakeups/s | 34.0 | 160.6 | 1.2 | 3.2 |
| all-thread wakeups/s | 210.4 | 1924.6 | 44.1 | 431.4 |
| all-thread CPU % of one core | 0.60 | 5.70 | 0.17 | 1.52 |
| host load1 before → after | 7.31→4.25 | 5.38→5.38 | 3.92→2.29 | 1.99→3.23 |

Base N=10 matches B2 (161.7 wakeups/s; CPU 5.28 % in the same range; this window 5.70 % at load1 5.4). Fixed N=10 official wakeups **−98 %**, all-thread **−77.6 %**, CPU **−73.4 %**.

Remaining fixed all-thread work at N=10: two `acos-art` threads per session at ~20 s⁻¹ (memory watchdog 50 ms safety sampler + one other 50 ms-class timer, attribution in progress), ART daemons (HeapTaskDaemon/Finalizer/ReferenceQueue ≈1.5 s⁻¹/session), and live-probe 0.4 s⁻¹/session. Official leaders are ~0 except the 1 s serve poll.

## Changes in the working tree (builds clean, release, `-j 4`, `.cache/h4-target`)
- `crates/acos-art/guest/java/dev/acos/art/LaunchMain.java`: probe fallback 250 ms → 5000 ms (host `PROBE` wake and dirty events still deliver samples; liveness bound kept).
- `crates/acos-art/src/launch.rs`: interactive idle loop is event-driven (`ServeEvent`: stdin line or `waitid(WNOWAIT)` child exit); no 100 ms poll. Only the 900 s session cap remains as a periodic bound. Peer receive timeout 100 ms → 1 s (guest exit closes the socket; bound kept).
- `crates/acos-art/src/logd.rs`: reader timeout 100 ms → 1 s; `finish_capped` already sends an explicit wake datagram.
- `crates/acos-binder-peer/native/binder_driver.c`: 20 ms `pthread_cond_timedwait` looper wait → untimed `pthread_cond_wait` (broadcast on `cv_txn`; non-loopers already return before this point). Removes ~100 s⁻¹/guest (two looper threads).
- `crates/acos-cli/src/serve.rs`: idle poll 50 ms → up to 1 s (connection wakes `poll` immediately; idle-exit check still uses the remaining idle time; while a session is held it waits 1 s).
- Not touched: strict-settle semantics, safety timeout values (watchdog 50 ms, session cap, settle bounds), ART/framework sources.

## Next step on resume
1. Attribute the remaining two ~20 s⁻¹ `acos-art` threads/session (watchdog + one unnamed 50 ms-class timer); decide whether the latter is removable/event-drivable.
2. A/B under the ART lock with ≥3 reps at N=1/5/10 plus M7, M8 (both animation modes), H6 loop N=1/N=5; record load; prove p50 within ±5 %, 0 false passes.
3. Gates: per-crate cargo tests; the 11 positive scenarios; T03-render-wrong `ASSERTION_FAILED`; offscreen-pager-visible `ELEMENT_OFFSCREEN`; offscreen-pager-wait and p11-progress-timeout `TIMEOUT`.
4. Write `EXPERIMENTS.md` E2.16 + Leaderboard idle CPU/wakeups + Backlog H4; `results/bench/H4.md`.

## Uncommitted file list at pause
```
M crates/acos-art/guest/java/dev/acos/art/LaunchMain.java
M crates/acos-art/src/launch.rs
M crates/acos-art/src/logd.rs
M crates/acos-binder-peer/native/binder_driver.c
M crates/acos-cli/src/serve.rs
```
(Scratch/binary evidence under `.cache/h4/`: base/fixed binaries, `results/profile-{base2,fixed}.json`, run logs. No warm pools or stray processes left by this agent; the aborted watchdog-disabled probe was killed.)
