# FIX041b — Idle-CPU Regression Root Cause, Fix, and Verification

**Branch / commit:** `fix041` (`47ba64855` + this report)
**Baseline:** `v0.4.0` (`8e020cee3`, `/home/Qui/.t3/worktrees/acos/timing-base-8e020`)
**Regression source:** `209284a9a` (from `origin/fix041`, atop `v0.4.1` RC / `launcher2` `0f930c5af`)

---

## 1. Culprit & Root Cause

Per-thread idle profiling over a 20.02 s window (10 s post-launch, N=5 idle `counter.apk` sessions under debug builds) confirmed the regression was caused by `LaunchMain.startTopFinishWatcher` (`crates/acos-art/guest/java/dev/acos/art/LaunchMain.java`, thread `"acos-top-finish-watcher"`):

1. **Pre-v0.4.1 (`8e020cee3`):** `startTopFinishWatcher` polled at 10 ms intervals for a bounded **120 s deadline** (`deadline = System.currentTimeMillis() + 120_000L`), scanning only the single launch token. In `tools/bench/acos_bench.py --phase memory`, each N stage takes ~70 s (10 s settle + 60 s measurement), so earlier sessions' watchers expired during the N=5 (70–140 s) and N=10 (140–210 s) windows.
2. **v0.4.1 (`0f930c5af` `LAUNCHER2`):** Changed `startTopFinishWatcher` to an **infinite `while (true)` loop** polling every **10 ms** over `sThread.mActivities.entrySet()` with reflective `getMethod("isDestroyed").invoke(activity)` for every live activity. Each idle guest session pinned one polling thread forever (~100 wakeups/s per session, ~+0.10–0.30% of a core per session depending on debug vs release).
3. **Why `wakeups_idle_n*_per_s` looked flat in RELGATE5 (`3.11 -> 3.12`):** `acos_bench.py`'s `tree_snapshot()` reads `/proc/<pid>/status` `voluntary_ctxt_switches`, which in Linux only counts context switches of the **process thread-group leader** (not worker threads such as `acos-top-finish-watcher`), whereas `/proc/<pid>/stat` `utime + stime` sums CPU ticks across **all threads** in the process.

### Per-Thread Idle Profile (N=5, 20.0 s window, 10 s after launch, debug builds)

| Run | Process tree CPU % | Process-leader wakeups/s | `acos-top-finish` threads | `acos-top-finish` ticks / CPU % | `acos-top-finish` worker vctxt (/s) | Other active threads |
|---|---:|---:|---:|---:|---:|---|
| `v0.4.0` (`8e020cee3`, 120 s watcher) | 2.848% | 1.998 | 5 | 29 ticks / 1.449% | 9,919 (495.5/s) | `acos-mem-watchd` 0.949% |
| Pre-fix `origin/fix041` (`209284a9a`, infinite watcher) | 3.197% | 2.098 | 5 | 32 ticks / 1.599% | 9,913 (495.2/s) | `acos-mem-watchd` 1.049%, `ixtures.counter` 0.150% |
| **Post-fix `FIX041b` (`47ba64855`)** | **1.748%** | 2.248 | **0** | **0 ticks / 0.000%** | **0 (0.0/s)** | `acos-mem-watchd` 1.049%, `acos-result-wat` 0.250%, `ixtures.counter` 0.050% |

---

## 2. Root Fix

Replaced the 10 ms background polling thread with an **event-driven binder client proxy** in `crates/acos-art/guest/java/dev/acos/art/LaunchMain.java`:

- **`installTopFinishHook()`** (called at launch right after `installStartActivityGuard()`): hooks `android.app.ActivityClient.INTERFACE_SINGLETON` (`mInstance` and `mKnownInstance`) and wraps `IActivityTaskManager.getActivityClientController()` return values via `wrapActivityClientController()`.
- **`wrapActivityClientController()`**: creates an idempotent `IActivityClientController` dynamic proxy that delegates all calls unchanged and intercepts `finishActivity` and `finishActivityAffinity`.
- When `IActivityClientController.finishActivity(...)` or `finishActivityAffinity(...)` returns `Boolean.TRUE`, **`scheduleTopFinishCheck(...)`** posts a one-shot `Runnable` onto `Looper.getMainLooper()` (`sMainHandler`). Because `Activity.finish(int)` writes `mFinished = true` immediately after `finishActivity()` returns `true`, running on the next main-looper turn guarantees `mFinished` is set cleanly (with a bounded 10 × 5 ms retry only for that finishing token if invoked off-main-thread).
- **`dispatchTopFinishes(...)` -> `deliverTopFinishTransactions(...)`**: skips tokens already claimed by `sLocallyWatchedFinishes` or `sTopFinishHandled`, checks `mFinished && !isDestroyed()`, and dispatches `PauseActivityItem(true, false, false) -> StopActivityItem(false) -> DestroyActivityItem(true, false, false)` (emitting `AcosRot6: ACOSLAUNCH MARK activity-destroyed-after-finish ...`).
- **`startTopFinishWatcher(...)`**: returns immediately when `sTopFinishHookInstalled` is `true`, keeping the polling loop only as an unreached fallback if hook installation fails.
- Kept host-side `atms.rs` unchanged so `crates/acos-services/tests/rot7_back.rs` (`no lifecycle transition is invented by finishActivity`) remains strictly intact.

---

## 3. Build, Unit Tests, and Scenario Verification

### Build
```bash
source tools/agent-env.sh
export CARGO_TARGET_DIR=$PWD/.cache/target
env -u TMPDIR tools/guarded.sh --mem 12G --time 900 -- cargo build --offline -p acos-art -p acos-cli
```
Result: `acos-art` and `acos-cli` built cleanly into `.cache/target/debug/{acos-art,acos}` (`rc=0`).

### Rust Tests
- `env -u TMPDIR tools/guarded.sh --mem 12G --time 600 -- cargo test --offline -p acos-services` (with `service-provider` + `dialogs-menus` dogfood APKs built): **773 passed, 0 failed, 2 ignored** (including `mproc_pms` 9/9 and `rot7_back`).
- `env -u TMPDIR tools/guarded.sh --mem 8G --time 180 -- cargo test --offline -p acos-art --lib`: **206 passed, 0 failed, 1 ignored**.

### Scenarios (`--engine art`)
Command shape:
```bash
env -u TMPDIR tools/guarded.sh --mem 8G --time 240 -- .cache/target/debug/acos verify scenarios/<name>.yaml --engine art --keep-artifacts none-on-pass --artifacts <dir> --overwrite --json
```
Run serialized under `tools/art-runtime/run-exclusive.sh --task FIX041b`:

| Scenario | Expected | Actual |
|---|---|---|
| `scenarios/launcher-home.yaml` | PASS | **PASS** (`rc=0`) |
| `scenarios/negative/launcher-home-wrong.yaml` | FAIL (negative) | **FAIL as expected** (`rc=1`) |
| `scenarios/settings-intents.yaml` | PASS | **PASS** (`rc=0`) |
| `scenarios/negative/settings-intents-wrong.yaml` | FAIL (negative) | **FAIL as expected** (`rc=1`) |
| `scenarios/multiwindow-split.yaml` | PASS | **PASS** (`rc=0`) |
| `scenarios/camera-preview.yaml` | PASS | **PASS** (`rc=0`) |
| `scenarios/mproc3-two-process.yaml` | PASS | **PASS** (`rc=0`) |
| `scenarios/mproc1-intents.yaml` | PASS | **PASS** (`rc=0`) |
| `scenarios/mproc1-multi-package.yaml` | PASS | **PASS** (`rc=0`) |
| `scenarios/mproc8-bind-only.yaml` | PASS | **PASS** (`rc=0`) |
| `scenarios/w38-multi-activity.yaml` | PASS | **PASS** (`rc=0`) |
| `scenarios/multi-activity-finish.yaml` | PASS | **PASS** (`rc=0`) |
| `scenarios/v1-hello-views.yaml` | PASS | **PASS** (`rc=0`) |
| `scenarios/w42-back-frame.yaml` | PASS | **PASS** (`rc=0`) |
| `scenarios/back-layers.yaml` | PASS | **PASS** (`rc=0`) |
| `scenarios/rot1-rotate.yaml` | PASS | **PASS** (`rc=0`) |
| `scenarios/gaten-backstack.yaml` | PASS | **PASS** (`rc=0`) |

*(Note: `hello-views.yaml` and `multi-activity.yaml`, which declare `engine: rust-vm` / `native-preferred` rather than ART-specific scenarios, failed on pre-existing soft-IME / `INPUT_UNHANDLED` coordinate mismatches when forced under `--engine art`; in `multi-activity.yaml` logcat, the hook executed as intended and emitted `ACOSLAUNCH MARK activity-destroyed-after-finish ...` followed by `Main.onPause -> onStop -> onDestroy`.)*

---

## 4. Interleaved Idle-CPU A/B vs `v0.4.0` (3 reps)

Executed with `tools/bench/acos_bench.py --phase memory --animations on` under `tools/art-runtime/run-exclusive.sh --task FIX041b-ab --exclusive -- tools/guarded.sh --mem 16G --time 500`, interleaved per rep (`rep 1..3: v040 then fix041b`), using debug binaries:
- **Arm A (`v0.4.0` `8e020cee3`):** `/home/Qui/.t3/worktrees/acos/timing-base-8e020/.cache/target/debug/acos` (`sha256: 1d18556ee6f80d8f...`, `ACOS_ROOT=/home/Qui/.t3/worktrees/acos/timing-base-8e020`)
- **Arm B (`FIX041b` `47ba64855`):** `.cache/target/debug/acos` (`sha256: 56011828387e9d47...`, `ACOS_ROOT=<worktree>`)

All 6 phases passed (`ok=True`, 0 noisy phases; host `loadavg1` `0.22–0.53`, `MemAvailable` `50.3–51.3 GiB`).

### Idle CPU & Wakeup Comparison (`tools/bench/ab-compare.py`)

| Metric | `v0.4.0` p50 (per-rep) | `FIX041b` p50 (per-rep) | Delta | Delta % | Threshold | Verdict |
|---|---:|---:|---:|---:|---:|---|
| `cpu_idle_n1_pct` | 0.631% (`[0.780, 0.631, 0.531]`) | **0.282%** (`[0.382, 0.266, 0.282]`) | **-0.349 pp** | **-55.3%** | ±40.0% / 0.20 | **improved** |
| **`cpu_idle_n5_pct`** | **2.558%** (`[2.673, 2.558, 2.540]`) | **1.336%** (`[1.370, 1.221, 1.336]`) | **-1.222 pp** | **-47.8%** | ±18.0% / 0.20 | **improved** |
| **`cpu_idle_n10_pct`** | **4.404%** (`[4.404, 4.290, 4.537]`) | **2.340%** (`[2.423, 2.340, 2.309]`) | **-2.064 pp** | **-46.9%** | ±10.0% / 0.20 | **improved** |
| `cpu_idle_n5_per_session_pct` | 0.512% | **0.267%** | -0.245 pp | -47.9% | ±18.0% / 0.20 | **improved** |
| `cpu_idle_n10_per_session_pct` | 0.440% | **0.234%** | -0.206 pp | -46.8% | ±10.0% / 0.20 | **improved** |
| `wakeups_idle_n1_per_s` | 1.196 | 1.212 | +0.016 | +1.3% | ±10.0% / 0.20 | within-noise |
| `wakeups_idle_n5_per_s` | 2.063 | 2.079 | +0.016 | +0.8% | ±10.0% / 0.20 | within-noise |
| `wakeups_idle_n10_per_s` | 3.110 | 3.110 | 0.000 | +0.0% | ±10.0% / 0.20 | within-noise |

Why `FIX041b` beats `v0.4.0` by ~47% on debug builds: in `v0.4.0`, each new session spawned a 10 ms `acos-top-finish-watcher` thread with a 120 s timeout, so during the 70 s-per-tier `memory` benchmark (10 s settle + 60 s sample at N=1, N=5, N=10), the newly launched sessions' watchers were still actively polling during measurement. With the event-driven hook, zero polling threads exist at any point after launch.

### Memory & Teardown Metrics (same A/B run)

| Metric | `v0.4.0` p50 | `FIX041b` p50 | Delta | Delta % | Verdict |
|---|---:|---:|---:|---:|---|
| `m1_idle_mem_mib` (cgroup anon+shmem, N=1) | 62.49 MiB | 61.94 MiB | -0.55 MiB | -0.9% | within-noise |
| `m1_idle_pss_mib` (total PSS, N=1) | 276.88 MiB | 269.60 MiB | -7.28 MiB | -2.6% | within-noise |
| `m2_per_session_anon_shmem_n1` | 58.80 MiB | 58.11 MiB | -0.69 MiB | -1.2% | within-noise |
| `m2_per_session_anon_shmem_n5` | 58.17 MiB | 58.03 MiB | -0.14 MiB | -0.2% | within-noise |
| `m2_per_session_anon_shmem_n10` | 58.38 MiB | 58.17 MiB | -0.21 MiB | -0.4% | within-noise |
| `m2_per_session_pss_n1` | 204.37 MiB | 214.69 MiB | +10.33 MiB | +5.1% | flagged (`REGRESSION` on tight ±3.0% / 0.50 floor; see note) |
| `m2_per_session_pss_n5` | 72.39 MiB | 74.31 MiB | +1.92 MiB | +2.7% | within-noise |
| `m2_per_session_pss_n10` | 55.65 MiB | 56.50 MiB | +0.86 MiB | +1.5% | within-noise |
| `teardown_ms` (GL1) | 23.50 ms | 23.50 ms | 0.00 ms | +0.0% | within-noise |

*Honest note on `m2_per_session_pss_n1`:* `ab-compare.py` flagged `m2_per_session_pss_n1` (`204.37 -> 214.69 MiB`, +5.1%) because `m2_per_session_pss_n1` subtracts the bare debug daemon PSS from total N=1 PSS (`276.88 - 204.37 = 72.5 MiB` bare PSS in `v0.4.0` vs `269.60 - 214.69 = 54.9 MiB` bare PSS in `FIX041b`). Total N=1 PSS (`m1_idle_pss_mib`) actually **decreased** by `7.28 MiB` (`276.88 -> 269.60 MiB`), and `m2_per_session_pss_n5`, `m2_per_session_pss_n10`, and all `anon+shmem` metrics are within noise or lower.
