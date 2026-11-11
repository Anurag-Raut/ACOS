# Q1 — baseline: verified Android dev tasks per GiB-minute at N=10

Measured 2026-11-11 (host 60 GiB, 32 cores). Harness: `tools/art-runtime/bench-n.sh`.
Raw per-cohort output: `.cache/art-bench/{art,vm}-N{1,10}/summary.json` (not committed).

## 1. Result

Two routes were measured with the same harness, same guard, same metric code. Each row is
a cohort of N agents launched together, repeated 3 times (cohorts strictly sequential).
Medians across the 3 reps; ranges in brackets.

| route | workload per agent | N | verified | peak PSS sum (MiB) | peak RSS sum (MiB) | cohort wall (s) | agent wall mean (s) | GiB-min | **verified tasks / GiB-min** (median, [range]) |
|---|---|---|---|---|---|---|---|---|---|
| ART (`acos-art render-app`) | counter.apk, 1 frame + tree, compared to ART oracle | 1 | 1/1 | 145.1 | 196.7 | 2.02 | 2.01 | 0.0026 | **381.2** [375.7 – 382.9] |
| ART (`acos-art render-app`) | same | **10** | **10/10** (all 3 reps) | **1386.1** | 1970.5 | 2.75 | 2.44 | 0.0306 | **326.4** [314.1 – 337.6] |
| Rust VM (`acos verify`) | counter.apk, 13-step scenario (taps, reset, verdict) | 1 | 1/1 | 624.7 | 714.7 | 2.48 | 2.48 | 0.0152 | **65.9** [65.5 – 66.5] |
| Rust VM (`acos verify`) | same | **10** | **10/10** (all 3 reps) | **5780.2** | 7043.3 | 3.42 | 3.29 | 0.1917 | **52.2** [51.7 – 52.3] |

Derived: ART ≈ 139 MiB PSS per agent at N=10; VM ≈ 578 MiB per agent at N=10 (≈4.2× ART).
Render-only wall (ART, excluding the guard and the post-run compare): 0.73 s mean at N=10,
0.62 s at N=1.

Verified means:
- ART: `render-app` outcome `ok` **and** `tools/art-render-app-compare.py` exit 0
  (full-frame SSIM ≥ 0.99 and all 8 structural nodes exact). Trial frame: SSIM 0.9935,
  structural 8/8, tree 12/12.
- VM: `acos verify --json` → `result.verdict.verdict == "pass"` with top-level `ok: true`.

No cohort had a failed or missing run. All 4 cohorts × 3 reps exited rc=0 on every agent.

## 2. Legacy Android-container (cited, not run)

Source: `/home/Qui/Work/android-container` docs, read-only. Not started: its documented
procedure is a multi-step Docker/redroid runbook (container lock, RAM gate, `docker run
--memory=4g`, `adb` polling), not a one-command guarded run under 4 GiB, so the start rule
was not met.

| metric | value | where recorded |
|---|---|---|
| boot_completed (idle core, reference image, n=3 fresh boots) | median **8.50 s** (8.43–8.55) | `image/verify/SUMMARY.md` |
| anon / working set (idle core, settled 60 s) | **297.2 MiB** / **300.1 MiB** | `image/verify/SUMMARY.md` |
| memory.current / memory.peak (idle core) | **407.8 MiB** / **430.4 MiB** | `image/verify/SUMMARY.md` |
| working set with the test app | 352 MiB | `image/verify/SUMMARY.md` |
| memory per session, N=1 / N=5 (baseline `v5`) | **137.2 / 105.7 MiB** | `STATE.md`, v6 A/B row (2026-11-10) |
| boot, same arm | 8.58 s; interleaved n=5 with the v9 image: 5.73 s | `STATE.md` (2026-11-10) |
| verified dev tasks per GiB-minute | **not recorded** | — |

The docs define the headline metric (`PLAN.md`: verified dev tasks/min/GiB, task list
T01–T12 in `research/PLAN-task-first.md`) but I found no recorded result for it. So there
is no legacy tasks-per-GiB-minute number to set against the two routes above.

## 3. Caveats — what is and is not comparable

1. **No LLM agent in the loop.** "Agents" here are independent, scripted processes. Model
   thinking time, tokens, retries and cost are all excluded. These figures bound machine
   efficiency, not agent productivity. The task-level headline needs an agent workload.
2. **The two routes run different workloads.** ART = one frame render and tree dump.
   VM = a 13-step scenario with taps, reset and a verdict. The 381 vs 52 gap mostly
   reflects workload size. The meaningful reads are N-scaling and per-agent memory within a
   route, not the cross-route ratio.
3. **The correctness oracles differ.** ART is checked against the ART oracle frame and tree
   (SSIM and structural nodes). VM is checked against the scenario's own assertions. Neither
   is a shared oracle, so "verified" is not like-for-like across routes.
4. **Guest build cache.** ART runs used a seeded copy of `.cache/art/data` (guest `classes.dex`
   and `libacosart.so` already built). A cold build is not included. VM ran the existing
   `target/release/acos` (built 15:49) without a rebuild or a HEAD check.
5. **Memory measurement.** PSS and RSS summed over each agent's descendant process tree,
   sampled from `/proc/<pid>/smaps_rollup` every ~30 ms by a `python3 -I` sampler.
   - Peak PSS is the more reliable number.
   - The GiB-min integral is a trapezoid over 60–100 samples per cohort. The ART runs last
     about 0.7 s, so the ART GiB-min is coarse (±tens of %). The VM runs are better sampled.
   - Shared pages (the 856 MB sysroot) are split by PSS, not double-counted.
   - Host page cache (`buff/cache` ≈ 20 GB) is not counted.
6. **Wall includes overhead.** The agent wall includes the `systemd-run` scope creation
   (≈1.4 s on ART at N=10 against 0.73 s render-only) and, for ART, the post-run compare
   script. Both are in the process tree, so both are in GiB-min. Render-only wall is shown
   separately.
7. **Concurrency and RAM.** Each agent runs under `tools/guarded.sh --mem 4G --time 300`,
   so a cohort is bounded at 40 GiB (< 50 GiB ceiling). No stagger was needed: measured
   peaks were 1.4 GiB (ART) and 5.8 GiB (VM). Host used RAM stayed at 37–41% (limit 80%
   refusal in the harness).
8. **Sample size.** 3 reps per configuration, so the ranges are narrow but unreliable as
   variance estimates. The N=1 rows are per-agent baselines, not separate experiments.
9. **Scheduling reconciliation.** The brief asked both for "one heavy process at a time" and
   for N=10 concurrent. I ran cohorts strictly one after another, never overlapping. Inside
   a cohort the N guarded agents ran concurrently, because that is the measured condition.

## 4. Reproduce

```
tools/art-runtime/bench-n.sh art 1  --reps 3 --out .cache/art-bench/art-N1
tools/art-runtime/bench-n.sh art 10 --reps 3 --out .cache/art-bench/art-N10
tools/art-runtime/bench-n.sh vm  1  --reps 3 --out .cache/art-bench/vm-N1
tools/art-runtime/bench-n.sh vm  10 --reps 3 --out .cache/art-bench/vm-N10
```

Each run prints a JSON median block and writes `summary.json`. The harness refuses to start
a cohort when host used RAM is ≥ 80%.

## 5. Host notes (not caused by this benchmark)

- Eight orphaned `dalvikvm64 … dev.acos.art.SessionMain … /data/session/repro/…` processes
  (ppid 1, started 16:14–16:39, ~175 MiB RSS each, ≈1.4 GiB total) are still running. They
  are from a `session --session-id repro` run, not from this benchmark. My runs left no
  processes behind. Left running; someone should decide whether to kill them.
- A `bwrap` / `dalvikvm64 -Dq3.mode=pressed` pair (q3-ripple) was started at 16:48 by another
  workstream and was left alone.
- Discord sandbox processes are also running; not touched.
- `tools/art-runtime/bench.sh` (822 lines, untracked, the fuller session-based harness that
  STATUS.md cites) was **not modified**. The new harness is `tools/art-runtime/bench-n.sh`.
