# RELGATE6 — ACOS v0.4.1 Release Gate Re-run (Steps 1 & 7 on `origin/fix041`)

> Raw logs/JSON, run scripts, `repro.sh` files and generated tables referenced below are not tracked (repo policy: one report per task); they are archived on the lead host at `~/acos-evidence/v041-relgate6-raw-20261123.tar.zst` and `~/acos-evidence/v041-results-scripts-generated-md-20261123.tar.zst`.

> **Lead re-run (normal shell, 2026-11-23):** `cargo test -p acos-art --test spike_net_boundary` with `ACOS_HOME=~/.local/share/acos/current` → **2 passed, 0 failed** (`guest_cannot_change_the_lan_drop_rules`, `boundary_failure_never_starts_the_guest`). Step 1 has no remaining open item.

- **Branch / Commit**: `relgate6` from `origin/fix041` (`a84f915bbef5caf6402dbf9b8fca00273a4c383c` — `origin/modules` `36859df75` + `FIX041` fixes `cb1f4e435`, `209284a9a`, `47ba64855`, `a84f915bb`)
- **Baseline**: `v0.4.0` tag (`8e020cee3b33e3663232b7341ec29ca99b2b2574`) in `/home/Qui/.t3/worktrees/acos/timing-base-8e020` (unmodified sources; release binaries built into its own `.cache/target/release/`)
- **Scope**: Re-run RELGATE5's two failing steps (Step 1: workspace release test suite; Step 7: interleaved A/B benchmark vs `v0.4.0`) with identical commands, baseline, thresholds, rep counts, and interleaving. Steps 0, 2–6, 8–9 were already green in RELGATE5 (`results/release/v0.4.1-gate/relgate5/README.md`) and were not re-run.
- **Host / Environment**:
  - Host: `Linux arch 7.2.11-arch1-1 x86_64`, AMD Ryzen 9 9950X (32 hardware threads), `61,908 MiB` (`60.46 GiB`) RAM, 0 swap
  - Host RAM during gate: ~15 GiB used / ~45–46 GiB available (~25% used, well under 90% limit)
  - Disk `/`: `83%` used (`739G / 945G`, `159G` avail) after runs, before pruning temporary `.cache/target` and `fixtures/build`
  - Sandbox condition: `NoNewPrivs: 1`, `Seccomp: 2` (`acos-art::spike_net_boundary` cannot set up `bwrap` uid map inside this sandbox; recorded as `sandbox-only, lead re-runs`)

## Gate Summary (Steps 1 & 7)

| Step | Check | Verdict | Key Finding | Evidence |
|---|---|---|---|---|
| **1** | Release workspace build + test suite (`cargo test --release --no-fail-fast --offline`) | **PASS (with 1 sandbox-only + 9 opt-in)** | **2971 passed, 0 genuine product/test failures**. `acos-services::mproc_pms` (RELGATE5 blocker) now **PASSES 9/9** (`773/773` in `acos-services`). Remaining 10 failures: 1 sandbox-only (`spike_net_boundary`, lead re-runs) + 9 documented opt-ins (all pass `7/7` and `59/59` when opted in). | `01-tests/` |
| **7** | Interleaved 3-rep A/B `v0.4.0` (`8e020cee3`) vs `v0.4.1 fix041` (`a84f915bb`) | **PASS (`ab-compare.py` `rc=0`)** | **0 regressions under either gate.** RELGATE5 idle CPU regression is eliminated and improved by **53.8%–59.3%** vs `v0.4.0` (`cpu_idle_n5_pct` `2.42% → 0.99%`, `cpu_idle_n10_pct` `4.25% → 1.96%`). All memory metrics (incl. `m2_per_session_pss_n1`: `+0.14%`) and all strict/lattice latency metrics are `within-noise`. | `07-ab/` |

---

## Step 1 — Release Build + Workspace Test Suite — **PASS** (0 genuine failures; `spike_net_boundary` sandbox-only for lead)

### Commands

```bash
source tools/agent-env.sh
export CARGO_TARGET_DIR="$PWD/.cache/target"
env -u TMPDIR tools/guarded.sh --mem 12G --time 900 -- cargo build --release --workspace --offline
bash results/release/v0.4.1-gate/relgate6/01-tests/run-tests.sh
```

Prerequisites prepared before tests (matching RELGATE5 Step 0):
- Shared cache symlinks under `.cache/` pointing to `/home/Qui/Projects/acos/.cache/` (`art`, `bionic-arm64`, `boot/android-34`, `corpus`, `flutter-1`, `gaten`, `qemu-user`, `webview`, `rn-0.81.4`, and `libexec/pasta`).
- Built workspace APKs (`tools/build-apps.sh`, `tools/build-dogfood-apps.sh`, `tools/build-negative-apps.sh`, `tools/build-webview-provider.sh`, `tools/fetch-camerax-deps.sh && tools/build-camerax-probe.sh`, `tools/build-rn-hello.sh debug`), dex/JNI fixtures (`tools/build-fixtures.sh`, `fixtures/jni/build.sh`), and hardlinked prebuilt legacy APKs (`flutter-*`, `rn-hello*`, `hostile-*`, `native-hostile*`, `widget-gallery*`, `acos-counter*`).

### Results (`01-tests/`)

- `cargo build --release --workspace --offline` → `CARGO_RC=0`, `62 s` (`01-tests/01-build-release.log`).
- `cargo test --release --no-fail-fast --offline` split identically to RELGATE5 (`01-tests/run-tests.sh`, `01-tests/driver.log`):

| log | crates | passed | failed | ignored | notes |
|---|---|---:|---:|---:|---|
| `02-test-groupA.log` | 19 non-ART crates | 805 | 0 | 1 | `rc=0` |
| `03-test-groupB.log` | `acos-vm`, `acos-runtime`, `acos-sandbox` | 353 | 0 | 10 | `rc=0` |
| `04-test-acos-verify.log` | `acos-verify` | 171 | 0 | 0 | `rc=0` |
| `05-test-acos-art.log` | `acos-art` | 263 | 1 | 1 | `spike_net_boundary` only (`NoNewPrivs: 1`) |
| `06-test-acos-cli.log` | `acos-cli` | 315 | 1 | 15 | `sandbox_s8` opt-in only |
| `07-test-acos-services.log` | `acos-services` | 773 | 0 | 2 | **`rc=0` (`mproc_pms` 9/9 PASS; fixed in `cb1f4e435`)** |
| `08-test-acos-mcp.log` | `acos-mcp` | 169 | 0 | 0 | `rc=0` |
| `09-test-acos-jni.log` | `acos-jni` | 71 | 0 | 0 | `rc=0` |
| `10-test-acos-webview.log` | `acos-webview` | 51 | 8 | 0 | 8 Chromium opt-ins only |
| **total (29 crates)** | | **2971** | **10** | **29** | **0 genuine failures** |

### Classification of the 10 non-passing tests in default sweep

| # | test(s) | count | alone rerun | classification | evidence |
|---|---|---:|---|---|---|
| 1 | `acos-art::spike_net_boundary::guest_cannot_change_the_lan_drop_rules` | 1 | `RC=101` (`bwrap: setting up uid map: Operation not permitted`) | **sandbox-only, lead re-runs** (`NoNewPrivs: 1`, `Seccomp: 2` in agent sandbox) | `01-tests/11-failure-spike-net-alone.log` |
| 2 | `acos-cli::sandbox_s8::hostile_live_run_is_gated_and_recorded_separately` | 1 | **PASS 7/7** (`RC=0`) with `ACOS_S8_LIVE_HOSTILE=1` | **documented opt-in** (`tools/test-prereq-optouts.txt`) | `01-tests/12-failure-cli-s8-optin-alone.log` |
| 3 | `acos-webview` heavy Chromium integration tests | 8 | **PASS 59/59** (`RC=0`) crate total with `ACOS_WEBVIEW_TEST=1` (restored tracked `results/handoff/{WVB,WVFAIR}/` JSONs via `git checkout`) | **documented opt-in** (`tools/test-prereq-optouts.txt`) | `01-tests/13-failure-webview-optin-alone.log` |

- `acos-services::mproc_pms::query_intent_activities_matches_every_installed_package` (the sole genuine failure in RELGATE5) **PASSED** in `07-test-acos-services.log` (`773 passed, 0 failed, 2 ignored`, `rc=0`).
- `acos-art::cleanup` **PASSED 9/9** in `05-test-acos-art.log`.
- Residual test-spawned `acos-webview` broker (PID `357107`, `--idle-shutdown-secs 60`) stopped cleanly after `13-failure-webview-optin-alone.log`.

---

## Step 7 — Interleaved A/B `v0.4.0` vs `v0.4.1 fix041` — **PASS** (`ab-compare.py` `rc=0`)

### Setup & Commands

| Arm | Source | Build command | Binary (`target/release/acos`) sha256 |
|---|---|---|---|
| **A (`v040`)** | tag `v0.4.0` (`8e020cee3`), `/home/Qui/.t3/worktrees/acos/timing-base-8e020` | `cargo build --release --offline -p acos-cli -p acos-art -p acos-webview -p acos-netd` (`V040_BUILD_RC=0`, `62 s`) | `2b78191f895035454bb6d7d59eb0f4a7bc457d429bd64c689368d83f74dbc78b` |
| **B (`v041`)** | `origin/fix041` (`a84f915bb`), worktree `t3-30ef7cb0-4a8f6f576a950a8e` | `cargo build --release --workspace --offline` (`CARGO_RC=0`, `62 s`) | `76be852263dc869895b99c16bb5bd80ff59476ea1531ee2191a3dc7036c0a6eb` |

Interleaved per rep identically to RELGATE5 (`reps 1..3`: A `latency,cold`, A `memory`, B `latency,cold`, B `memory`; `--reps 20 --fast-reps 40 --p11-reps 3 --loops 0 --animations on`; each group under `tools/art-runtime/run-exclusive.sh --exclusive -- tools/guarded.sh`). Because `run-ab.sh` hit the outer 600 s tool-command limit during `rep=1 v041 latency,cold` (after `rep=1 v040` completed `ok=True`), `rep=1 v041` through `rep=3 v041` were executed via `07-ab/run-ab-step.sh` (identical per-group invocation, ~3–5 min per group). Note: an aborted `rep=1 v041` invocation's PIDs were cleaned up and scratch removed before running `run-ab-step.sh 1 v041 latency,cold`.

```bash
python3 tools/bench/acos_bench.py --merge --run-dir results/release/v0.4.1-gate/relgate6/07-ab/run-v040 --acos /home/Qui/.t3/worktrees/acos/timing-base-8e020/.cache/target/release/acos --out results/release/v0.4.1-gate/relgate6/07-ab/v040.json --markdown results/release/v0.4.1-gate/relgate6/07-ab/v040.md --mode full
python3 tools/bench/acos_bench.py --merge --run-dir results/release/v0.4.1-gate/relgate6/07-ab/run-v041 --acos "$PWD/.cache/target/release/acos" --out results/release/v0.4.1-gate/relgate6/07-ab/v041.json --markdown results/release/v0.4.1-gate/relgate6/07-ab/v041.md --mode full
python3 tools/bench/ab-compare.py --a results/release/v0.4.1-gate/relgate6/07-ab/v040.json --b results/release/v0.4.1-gate/relgate6/07-ab/v041.json --label-a "v0.4.0 (8e020cee3)" --label-b "v0.4.1 fix041 (a84f915bb)" --json results/release/v0.4.1-gate/relgate6/07-ab/ab-compare.json > results/release/v0.4.1-gate/relgate6/07-ab/ab-compare.txt 2>&1
```

### Host Gate & Noisy-Phase Audit

All 18 phase JSONs (`run-{v040,v041}/rep-{1,2,3}/{latency,cold,memory}.json`) completed `ok=True`, `rc=0`, `MemAvailable` `44.49–45.62 GiB`:
- **Clean (`noisy: no`)**: all 6 `latency` phases (`loadavg1` `0.54–2.09`) and all 6 `memory` phases (`loadavg1` `0.42–1.19`).
- **Flagged noisy (`noisy: yes`)**: all 6 `cold` phases (`v040` `loadavg1` `7.91 / 8.36 / 8.35`; `v041` `loadavg1` `6.83 / 9.16 / 5.37`). As in RELGATE5 (`4.17–9.99`), this is the residual 1-minute load tail immediately after 20 back-to-back cold boots (`host_before`/`host_after` sampled at phase end) and affects both arms identically (`M3` delta `-0.21%`).

### ADR-0028 Lattice Gate (Fast-Settle Metrics, `n=120/120`) — **PASS** (0 regressions)

| metric | `v0.4.0` trimmed | `v0.4.1 fix041` trimmed | delta | delta % | threshold | first slot A → B | verdict |
|---|---:|---:|---:|---:|---|---|---|
| `m4_launch_fast_ms` | 148.50 | 147.48 | -1.02 ms | -0.69% | > +5% & +2 ms | 32% → 33% | **within-noise** |
| `m6_reset_fast_ms` | 185.09 | 189.73 | +4.64 ms | +2.51% | > +5% & +2 ms | 16% → 2% | **within-noise (`SLOT-SHIFT -13 pp`)** |
| `m8_tap_counter_fast_ms` | 24.76 | 24.80 | +0.04 ms | +0.16% | > +5% & +2 ms | 50% → 50% | **within-noise** |
| `m8_tap_form_fast_ms` | 29.16 | 29.64 | +0.48 ms | +1.65% | > +5% & +2 ms | 100% → 100% | **within-noise** |

- **Flagged (`SLOT-SHIFT -13 pp` on `m6_reset_fast_ms`)**: `v0.4.0` rep 1 had `175.86 ms` while reps 2–3 had `195.39 / 189.67 ms` (16% in slot 0); `v0.4.1 fix041` had `189.00 / 189.60 / 190.03 ms` (2% in slot 0), matching RELGATE5 (`189.13 / 189.07 ms` p50). Trimmed delta is `+2.51%`, well inside the 5% gate.

### Full Old-Gate Metric Table (Medians of Per-Rep p50s, `ab-compare.py` `rc=0`) — **PASS**

| metric | `v0.4.0` median (per-rep) | `v0.4.1 fix041` median (per-rep) | delta | delta % | threshold (rel / abs) | verdict |
|---|---:|---:|---:|---:|---|---|
| `m3_cold_start_ms` | 4087.76 (`[4087.76, 4107.80, 4067.61]`) | 4079.06 (`[4094.51, 4079.06, 4063.20]`) | -8.69 ms | -0.21% | ±5.00% / 2.00 | within-noise (noisy cold phase) |
| **`m4_launch_strict_ms`** | 287.33 (`[287.33, 285.97, 300.60]`) | 300.61 (`[300.80, 297.06, 300.61]`) | +13.27 ms | **+4.62%** | ±5.00% / 2.00 | **within-noise (flagged: near 5% floor, ~1 frame)** |
| `m4_launch_fast_ms` | 150.45 (`[151.47, 150.35, 150.45]`) | 149.96 (`[149.96, 149.75, 150.87]`) | -0.49 ms | -0.33% | ±5.00% / 2.00 | within-noise |
| `m5_screenshot_ms` | 11.99 (`[11.99, 11.53, 12.15]`) | 12.13 (`[11.50, 12.13, 12.18]`) | +0.14 ms | +1.17% | ±11.00% / 2.00 | within-noise |
| `m6_reset_strict_ms` | 332.94 (`[318.03, 332.94, 333.22]`) | 337.76 (`[337.76, 337.97, 335.43]`) | +4.82 ms | +1.45% | ±5.00% / 2.00 | within-noise |
| `m6_reset_fast_ms` | 189.67 (`[175.86, 195.39, 189.67]`) | 189.60 (`[189.00, 189.60, 190.03]`) | -0.07 ms | -0.04% | ±5.00% / 2.00 | within-noise |
| `m7_tree_strict_ms` | 41.64 (`[41.35, 41.64, 41.73]`) | 41.65 (`[41.59, 41.65, 41.71]`) | +0.02 ms | +0.04% | ±5.00% / 2.00 | within-noise |
| `m7_tree_fast_ms` | 0.61 (`[0.44, 0.61, 0.64]`) | 0.63 (`[0.62, 0.63, 0.73]`) | +0.01 ms | +2.04% | ±33.00% / 2.00 | within-noise |
| `m8_tap_counter_strict_ms` | 953.78 (`[950.90, 955.70, 953.78]`) | 947.50 (`[947.21, 947.50, 949.12]`) | -6.28 ms | -0.66% | ±5.00% / 2.00 | within-noise |
| `m8_tap_counter_fast_ms` | 24.82 (`[24.46, 25.06, 24.82]`) | 24.65 (`[24.65, 24.71, 24.58]`) | -0.17 ms | -0.67% | ±5.00% / 2.00 | within-noise |
| `m8_tap_form_strict_ms` | 924.52 (`[930.66, 923.99, 924.52]`) | 923.91 (`[933.28, 923.91, 923.80]`) | -0.61 ms | -0.07% | ±5.00% / 2.00 | within-noise |
| `m8_tap_form_fast_ms` | 29.99 (`[27.85, 29.99, 30.23]`) | 30.13 (`[29.61, 30.13, 30.41]`) | +0.14 ms | +0.48% | ±5.00% / 2.00 | within-noise |
| `tap_ack_ms` | 5.00 (`[5.0, 5.0, 5.0]`) | 5.00 (`[5.0, 5.0, 5.0]`) | 0.00 ms | +0.00% | ±5.00% / 2.00 | within-noise |
| `teardown_ms` (GL1) | 21.00 (`[21.0, 22.5, 21.0, 21.5, 21.0, 20.0]`) | 21.75 (`[21.5, 22.0, 21.0, 22.0, 21.0, 24.0]`) | +0.75 ms | +3.57% | ±21.00% / 2.00 | within-noise |
| `teardown_wall_ms` | 22.24 (`[21.70, 23.85, 22.37, 22.55, 22.12, 21.03]`) | 22.65 (`[22.54, 22.76, 22.14, 22.79, 21.81, 24.68]`) | +0.41 ms | +1.83% | ±18.00% / 2.00 | within-noise |
| `cpu_active_tap_strict_pct` | 45.40 (`[41.28, 46.33, 45.40]`) | 45.32 (`[43.89, 45.32, 45.34]`) | -0.08 | -0.18% | ±10.00% / 0.20 | within-noise |
| **`cpu_idle_n1_pct`** | 0.531 (`[0.548, 0.498, 0.531]`) | **0.216** (`[0.249, 0.199, 0.216]`) | **-0.315** | **-59.32%** | ±31.00% / 0.20 | **improved** |
| **`cpu_idle_n5_pct`** | 2.424 (`[2.358, 2.424, 2.506]`) | **0.989** (`[0.989, 1.105, 0.940]`) | **-1.435** | **-59.20%** | ±24.00% / 0.20 | **improved** |
| **`cpu_idle_n10_pct`** | 4.249 (`[4.249, 4.133, 4.251]`) | **1.962** (`[2.043, 1.962, 1.799]`) | **-2.287** | **-53.82%** | ±17.00% / 0.20 | **improved** |
| `cpu_idle_n1_per_session_pct` | 0.531 (`[0.548, 0.498, 0.531]`) | **0.216** (`[0.249, 0.199, 0.216]`) | -0.315 | -59.32% | ±31.00% / 0.20 | **improved** |
| `cpu_idle_n5_per_session_pct` | 0.485 (`[0.472, 0.485, 0.501]`) | **0.198** (`[0.198, 0.221, 0.188]`) | -0.287 | -59.18% | ±24.00% / 0.20 | **improved** |
| `cpu_idle_n10_per_session_pct` | 0.425 (`[0.425, 0.413, 0.425]`) | **0.196** (`[0.204, 0.196, 0.180]`) | -0.229 | -53.88% | ±17.00% / 0.20 | **improved** |
| `wakeups_idle_n1_per_s` | 1.195 (`[1.195, 1.195, 1.212]`) | 1.211 (`[1.211, 1.212, 1.195]`) | +0.016 | +1.34% | ±10.00% / 0.20 | within-noise |
| `wakeups_idle_n5_per_s` | 2.045 (`[2.045, 2.028, 2.045]`) | 2.127 (`[2.094, 2.127, 2.128]`) | +0.082 | +4.01% | ±10.00% / 0.20 | within-noise |
| `wakeups_idle_n10_per_s` | 3.138 (`[3.138, 3.039, 3.155]`) | 3.155 (`[3.155, 3.155, 3.206]`) | +0.017 | +0.54% | ±10.00% / 0.20 | within-noise |
| `m1_idle_mem_mib` | 59.36 (`[58.61, 59.36, 59.48]`) | 59.78 (`[59.79, 60.22, 57.82]`) | +0.42 MiB | +0.71% | ±7.00% / 0.50 | within-noise |
| `m1_idle_pss_mib` | 239.99 (`[239.04, 239.99, 240.73]`) | 241.14 (`[241.14, 241.38, 239.71]`) | +1.15 MiB | +0.48% | ±3.00% / 0.50 | within-noise |
| `m2_per_session_anon_shmem_n1` | 57.40 (`[56.68, 57.40, 57.51]`) | 57.77 (`[57.77, 58.20, 55.80]`) | +0.37 MiB | +0.65% | ±7.00% / 0.50 | within-noise |
| `m2_per_session_anon_shmem_n5` | 56.13 (`[55.83, 57.16, 56.13]`) | 56.35 (`[56.37, 56.35, 55.76]`) | +0.22 MiB | +0.39% | ±3.00% / 0.50 | within-noise |
| `m2_per_session_anon_shmem_n10` | 56.12 (`[56.12, 56.77, 55.90]`) | 56.41 (`[56.41, 56.48, 56.17]`) | +0.29 MiB | +0.52% | ±3.00% / 0.50 | within-noise |
| `m2_per_session_pss_n1` | 220.88 (`[220.23, 220.88, 221.55]`) | 221.19 (`[221.92, 221.19, 219.76]`) | +0.31 MiB | +0.14% | ±3.00% / 0.50 | within-noise |
| `m2_per_session_pss_n5` | 74.03 (`[73.67, 75.00, 74.03]`) | 74.15 (`[74.23, 74.15, 73.70]`) | +0.12 MiB | +0.16% | ±3.00% / 0.50 | within-noise |
| `m2_per_session_pss_n10` | 55.28 (`[55.29, 56.03, 55.04]`) | 55.60 (`[55.60, 55.64, 55.34]`) | +0.32 MiB | +0.57% | ±3.00% / 0.50 | within-noise |

### Notes on Flagged / Previously Observed Metrics

- **`m2_per_session_pss_n1`**: In `FIX041b`'s debug-build A/B (`results/handoff/FIX041b/REPORT.md`), `m2_per_session_pss_n1` showed `+5.1%`; in this full 3-rep release A/B, it is `220.88 → 221.19 MiB` (**`+0.14%`**, `within-noise`, with rep 3 of `v0.4.1` at `219.76 MiB` below all `v0.4.0` reps).
- **`m4_launch_strict_ms`**: `+4.62%` (`287.33 → 300.61 ms`, just below the `±5.00%` floor, `within-noise`). `v0.4.0` rep 3 hit `300.60 ms` (`[287.33, 285.97, 300.60]`), matching `v0.4.1`'s `[300.80, 297.06, 300.61]` (a single ~14 ms frame quantum difference; `m4_launch_fast_ms` is `-0.33%` / `-0.69%` trimmed).

---

## Housekeeping

- Removed temporary build, runtime, and scratch artifacts after recording all results: `.cache/target`, `.cache/rt-s1`, `.cache/rg6b`, `.cache/rg6-ab.pids`, `.cache/lv`, `.cache/maven-camerax`, `fixtures/build`, `__pycache__` directories, and `/home/Qui/.t3/worktrees/acos/timing-base-8e020/.cache/target/release` (leaving `timing-base-8e020` sources and pre-existing `debug` target untouched).
- Verified zero lingering `acos-art`, `acos-webview`, or `acos-netd` processes.
