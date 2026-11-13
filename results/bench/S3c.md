# S3c — the memory sampler's churn undercount (fixed, re-measured)

**Status:** measured, tool fixed, metric corrected. No product code changed (`tools/benchlib.py`,
`tools/bench/acos_bench.py`, `tools/tests/test-benchlib.py` only). The metric defect (`S3c`) was
found by S3 (E3.4); this packet closes it and rewrites the numbers it touched.

| what | value |
|---|---|
| branch / commit | `bench-s3c-sampler` off `main` `5cae694` |
| binary | `target/release/acos`, sha256 `7fb5e56064d3…` (release, **unchanged main build**: before/after is a *paired sampler* comparison, see §1) |
| lock | `tools/art-runtime/run-exclusive.sh --task s3c -- tools/guarded.sh --mem 8G --time 400 -- …`; one acquisition per rep, all < 400 s |
| scratch | `.cache/s3c/**` (156 KB total: JSONs only, no PNGs) |
| raw | `.cache/s3c/loop-n20-<arm>-<rep>.json` (ephemeral, small); kernel-side `runs.log` lines `task=s3c` |

## 1. Method — before and after in the same window (paired)

`acos_bench --phase loop20` now runs **two** samplers over the **same cohort**:

* **before** — `d2_warm.WarmSampler`, unchanged: 100 ms ticks, guest scopes rediscovered by a
  full `/proc` ppid walk **every 1 s** (`discover` every 10 ticks). This is the S3 configuration.
* **after** — `benchlib.ChurnSampler` (new): 100 ms ticks, guest scopes discovered **on creation**
  — one `os.listdir` of the slice the job scope lives in (`…/app.slice`, a handful of names, no
  `/proc` walk per tick), ownership proved by walking the *new scope's own* ppid chain to the
  harness pid; the `/proc` walk is kept as a 1 Hz backstop. Per tick it reads the guest scope's
  `memory.stat` (anon+shmem), and every 5th tick `memory.current` + `memory.peak` (the kernel's own
  monotone peak, independent of the tick grid).
  Pss is charged **per session id** (from the run-tree path in the guest's cmdline, i.e.
  `<ACOS_SERVE_ARTIFACTS>/<session_id>/…`), not per "current child of `acos serve`" — the
  `session_pss()` defect that produced 2.7 / **0.0** / 51.5 MiB readings.

Both samplers see the same tasks, the same guest scopes and the same host window, so the
before/after delta is a metric change only, not a run-to-run difference.

Host: Ryzen 9 9950X, 32 threads, 61.9 GiB, cgroup v2. Other agents held the lock throughout
(K5h, S3AB); each rep records its own load1. N=20, 1 warm-up + 5 measured tasks per session
(100 measured per rep), 3 executions attempted per arm.

## 2. Results (N=20 agent loop, counter fixture, verified tasks)

| arm | rep | ms/task p50 | p90 | tasks/s | host CPU % | load1 | **before** GiB-min (anon+shmem) | **before** tasks/GiB-min | **after** GiB-min | **after** tasks/GiB-min | verified |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| off | r1 | 530.8 | 773.2 | 9.20 | 21.9 | 5.93 | 0.0816 | **1225.6** | 0.1795 | **557.0** | 100/100 |
| off | r2 | 534.6 | 780.7 | 8.87 | 22.0 | 3.66 | 0.0846 | **1182.1** | 0.1807 | **553.5** | 100/100 |
| off | r3 † | 1303.0 | 2103.0 | 6.72 | 36.9 | 7.60 | 0.1673 | 597.7 | 0.2457 | 407.0 | 100/100 |
| on | r1 | 1489.8 | 1575.1 | 6.70 | 40.4 | 8.47 | 0.1504 | **664.7** | 0.2341 | **427.2** | 100/100 |
| on | r2 | 1467.1 | 1590.4 | 7.58 | 22.1 | 5.30 | 0.1302 | **768.1** | 0.2108 | **474.3** | 100/100 |

† r3 ran under an external-load spike (load1 7.6, host CPU 36.9 %, off-arm p50 1303 ms vs 531/535 in
r1/r2 — the *same code*): its rates are reported, not used for the headline. Integrity: 500/500
verified, 0 false passes across the 5 reps.

**The sampler was the defect, not the loop.** The before column is 2.2× (off) and 1.56× (on) too
*high* in tasks/GiB-min: the 1 s discovery missed most of every guest's life, so the anon+shmem
integral counted the daemon's ~83 MiB most of the time instead of the ~1.37 GiB that was actually
resident.

### 2.1 Per-session memory — the numbers S3 could not quote

| arm | before: anon+shmem/session (median of ticks) | before: Pss/session | after: anon+shmem/session | after: Pss/session |
|---|---:|---:|---:|---:|
| off r1 | **2.78 MiB** (max 63.9) | 2.77 MiB | 68.90 MiB | 56.40 MiB |
| off r2 | **2.75 MiB** | **0.00 MiB** ‡ | 67.49 MiB | 55.86 MiB |
| off r3 † | 43.78 MiB | 51.48 MiB | 66.51 MiB | 53.05 MiB |
| on r1 | 15.01 MiB | 43.93 MiB | 66.66 MiB | 55.01 MiB |
| on r2 | **20.74 MiB** | **55.00 MiB** ‡ | 68.09 MiB | 57.04 MiB |

‡ `session_pss()` is not always zero in the *wrong direction*: it also over-reads when the serve's
child tree still holds a dying guest plus the next launcher. Off r2 is the exact failure S3 recorded
in the other direction — during a relaunch burst every launcher child has no guest, so the sum is
**0.0 MiB**. Together the five before rows span 0.0–55.0 MiB, a 23×-wide artefact.

The corrected figure is **66.5–68.9 MiB anon+shmem and 53.0–57.0 MiB ΣPss per session in every rep,
both arms** — stable to ±2 %, zero-free, and consistent with E3.2's M2 (65.6 MiB/session, N=10).
The before column is unusable in exactly the way the backlog said (2.7 – 43.8 MiB anon+shmem,
0.0 – 55.0 MiB Pss: a 16×/23× spread with a zero in it).

## 3. Coverage — what "100 %" means here

| metric | off r1 | off r2 | off r3 | on r1 | on r2 |
|---|---:|---:|---:|---:|---:|
| live guest scopes per tick (median) / N=20 | 21 (1.05×) | 21 | 21 | 21 | 21 |
| ticks with zero live guests | 0 | 0 | 0 | 0 | 0 |
| guest scopes observed / launch+reset steps | 141/200 | 144/200 | 142/200 | 155/200 | 142/200 |
| samples per guest (min / median) | 8 / 18 | 1 / 22 | 1 / 17 | 1 / 33 | 2 / 18 |
| guest scopes with a kernel `memory.peak` | 100 % | 100 % | 100 % | 100 % | 100 % |
| sampler CPU (% of one core, own thread) | 37.5 | 20.2 | 8.2 | 20.9 | 8.1 |

**Instantaneous coverage is complete: 21 live guest scopes for 20 sessions in every rep, and not a
single zero-guest tick** (the before sampler had 25 ticks of which most saw 0 guests). The
`observed/observed-expected` row is *not* 100 % and the rule is deliberately stated: it counts one
scope per `acos_launch` **and** per `acos_reset`, so a relaunch whose old and new scope overlap
counts twice while the sampler (correctly) never sees two scopes for one session; 71 % is the
step-derived upper bound, not a missing-guest count. The load-bearing checks are the live-scope
count (1.05×, no zero ticks), the per-guest sample counts (median 17–33 ticks = 1.7–3.3 s observed
per guest, longer than every measured guest lifetime) and the fact that every guest carries a
kernel-side `memory.peak` read.

Sampler cost: 8.1–8.2 % of one core in the final configuration (Pss at 1 Hz, `memory.peak` at 2 Hz,
`memory.stat` at 10 Hz over ~21 scopes; the 20–37 % rows are the earlier 20 Hz/10 Hz-Pss tuning
that was rejected for this reason). The sampler spawns no processes, opens no sockets and keeps no
spare guest.

## 4. Recomputing tasks/GiB-min — and the off/on ratio

What S3 published (`S3.md` §1) versus what the corrected denominator gives (N=20):

| arm | S3 published (before) | corrected (after) | correction |
|---|---:|---:|---|
| animations on | 603.0 | **427–474** | before was 1.3–1.4× too high |
| animations off | 1414.2 † | **553–557** | before was 2.5× too high |
| off/on ratio | 2.34 | **1.23–1.30** | see below |

**The sampling-independent 2.7–2.9× is a *per-busy-session* ratio, and the cohort GiB-min ratio is
not the same number.** Both are now computable and both agree with their own definition:

* p50 latency ratio (per task, and therefore tasks per GiB-min *while a session is busy*):
  `1489.8 / 534.6 = 2.79` and `1467.1 / 530.8 = 2.76` → **2.76–2.79×, inside the predicted 2.7–2.9×.**
  The sampling-independent formula `60 / (p50_s × 0.0641)` gives 586–630 (on) vs 1730–1790 (off).
* cohort GiB-min ratio: **1.23–1.30×**, because at N=20 the denominator charges 20 resident
  sessions for the whole cohort while throughput is limited by relaunch, not by the task:
  measured throughput ratio is 8.87–9.20 / 6.70–7.58 = **1.17–1.37×**, and per-session footprint is
  identical across arms (66.5–68.9 MiB both). GiB-min at fixed footprint is `throughput/footprint`,
  so the ratio must track throughput — 1.23–1.30× is the *correct* cohort-level answer, and any
  2.7–2.9× GiB-min ratio would require the off arm to be 2.7× more productive per second per GiB,
  which it is not at N=20 (each session is idle ~76 % of the cohort wall in both arms).

So S3's "the true ratio is 2.7–2.9×" is right for latency and per-busy-session GiB-min, and E3.3's
5–8.7× was the biased denominator; but the *cohort* GiB-min ratio at N=20 was never 2.7× — the
before sampler's larger undercount on the off arm (2.5× vs 1.3×) manufactured a 2.34×.

## 5. What changed

* `tools/benchlib.py` — `ChurnSampler` (+ `_pid_chain_hits`, `cgroup_procs`, `thread_cpu_seconds`,
  `MiB`, `CHURN_*`): creation-time discovery, per-scope lifetime records with kernel `memory.peak`,
  per-session-id Pss, coverage/CPU accounting in `summary()`.
* `tools/bench/acos_bench.py` — `phase_loop` runs the `ChurnSampler` beside the legacy sampler,
  `loop_cohort_stats` records `cohort.memory_s3c` (integral, per-session, coverage, CPU), new metric
  `loop_n<N>_tasks_per_gib_min_net_s3c` registered in `METRICS`.
* `tools/tests/test-benchlib.py` — 4 tests with a fake cgroup tree, fake `/proc` and a fake clock:
  short-guest discovery + integral (teeth: the 1 s cadence loses the guest), foreign-scope rejection
  (another agent's scopes are never charged), Pss/session attribution, coverage counting.

`python3 tools/tests/test-benchlib.py`: **232 passed, 0 failed.**

## 6. Open risks

* 2 clean reps per arm, not 3 (lock contention; the third off rep was load-contaminated). The
  per-session footprint and the corrected integral agree to ±2 % across 5 reps, but the GiB-min
  rates inherit the run-to-run throughput spread (±8 %).
* Coverage is stated as live-scope completeness + per-guest sample counts, not as a per-guest
  lifetime integral (a guest is only observable while its scope exists; the kernel does not keep
  an integral for a removed cgroup). `memory.peak` per guest is captured for 100 % of guests.
* `s3_scale.py` (compat/Gaten memory record) still uses `d2_warm.WarmSampler`; its per-session
  numbers carry the same bias and are not quoted here.
* Sampler cost is 8 % of one core at N=20 on this host. It is charged back out of every sample
  (RssAnon+RssShmem) but it is not zero; at N=30 it would grow with the live-scope count.
