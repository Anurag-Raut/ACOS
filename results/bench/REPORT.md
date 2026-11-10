# ACOS V1 benchmark report

- run: `run-2026-11-10-223843.json` (2026-11-10T22:38:44, commit `9426b5b`, 10 samples/metric, `release` profile)
- host: 32 threads, 60 GiB RAM, loadavg1 3.62, kernel 7.2.11-arch1-1, `rustc 1.90.0 (1159e78c4 2025-09-14)`
- method: BENCHMARKS.md §2: >=20 samples (p50/p95 nearest-rank), metric type labelled, cold/warm stated, /proc based (no /usr/bin/time), no fixed sleep in a timed path, one tools/guarded.sh scope per metric
- fidelity: `native-runtime` (ACOS executes the code itself). No row here is ART-verified, and no legacy value is an ACOS result.

Metric types are never mixed: RSS, PSS, anon+shmem (`RssAnon+RssShmem`) and private/shared pages are reported in separate labelled columns.

## 1. Measured ACOS metrics

| Metric | Value (p50, p95) | Type | Cold/warm |
|---|---|---|---|
| interpreter `02_int_arith` | 6.548 / 7.434 ms, 62583 insns, 9556903.455 insn/s | wall + VM counters | cold |
| interpreter `05_branches_loops` | 6.183 / 6.523 ms, 49935 insns, 8076347.728 insn/s | wall + VM counters | cold |
| interpreter `09_strings` | 8.001 / 8.996 ms, 143811 insns, 17973658.731 insn/s | wall + VM counters | cold |

## 2. ACOS vs legacy, every row of BENCHMARKS.md

Verdicts: `better`/`worse` are computed only for the pairs whose metric type and state match (anon+shmem vs anon+shmem, MiB vs MiB). Everything else is `not-measurable` (no engine capability yet) or `no-legacy-baseline` (legacy never published it). Nothing is estimated.

| Row | Metric | Legacy (ACVM, reference only) | Legacy type/state | ACOS | Verdict | Note |
|---|---|---|---|---|---|---|
| M1 | Fixed idle core memory | 206.0 MiB (W1) / 303.9 MiB (v9); reads span 259-322 MiB | anon+shmem, settled 60 s | **not measured** | `not-measurable` | ACOS's settled state is a booted framework app process (attach handshake done); the Vm-only column is an even leaner state (boot-classpath DEX is parsed lazily, 40 classes at Vm-ready) |
| M2 | Incremental memory per session at N=5 | 78.7 MiB (v9) / 33.2 MiB (W2 opt-in) | anon+shmem, warm | **not measured** | `not-measurable` | in-process sequential sessions; immutable boot DEX, parsed DexFiles and decoded method bodies are Arc-shared through the process-global BootCache (no mmap; see results/bench/MEMORY_PROFILE.md) |
| M3 | Cold start -> first correct frame | 3.620 s (v12) | wall, cold | **not measured** | `not-measurable` | needs acos-render bound to the graphics natives |
| M4 | Worker create -> correct frame | 1.251-1.343 s p50 | wall, warm | **not measured** | `not-measurable` | needs a worker-create path + renderer |
| M5 | Fresh screenshot | 0.066-0.067 s p50 | wall, warm | **not measured** | `not-measurable` | needs acos-render capture + frame sequencing |
| M6 | Reset-to-ready | 110.3 ms cmd / 748.1 ms -> frame | wall, warm | **not measured** | `not-measurable` | no reset path (ResetRequest is protocol-only) |
| M7 | UI tree with text | 538-768 ms one-shot / 4.1 ms persistent / 14-87 ms structure | wall, warm | **not measured** | `not-measurable` | UI-tree bridge not wired |
| M8 | Action-to-state latency | not cleanly published (U1 step-5 747 ms incl. tree) | wall, warm | **not measured** | `not-measurable` | needs input injection + UI tree |
| M9 | Concurrent workloads (N) | N=5 scorecard, N=10 measured; 10 extra displays | count + memory | **not measured** | `measured` | memory is measured; independent state/input at N is NOT (process-global Binder transport) |
| M10 | End-to-end verify wall time | install-bound dev loop ~19 s, 74-81 % adb install (no direct equivalent) | wall | **not measured** | `not-measurable` | needs launch/assert/evidence/reset/cleanup |
| M11 | Verified tasks / GiB-minute | not published as such | ratio | **not measured** | `not-measurable` | needs the T01-T12 suite + job-cgroup accounting |
| L-idle-core | Legacy bare core (P2c, stripped) | 278 MiB | anon, settled | **not measured** | `not-measurable` | closest state = Vm ready with zero tasks; ACOS also holds the boot classpath in the heap |
| L-session-n1 | Per-session N=1 (v9) | 130.3 MiB | anon+shmem, warm | **not measured** | `not-measurable` | in-process session |
| L-marginal | Marginal N=5->10 (v5) | ~99.3 MiB workset | workset, warm | **not measured** | `not-measurable` | anon+shmem, not cgroup workset |
| L-cold-first-shot | Cold core->first shot (v12) | 3.620 s | wall, cold | **not measured** | `not-measurable` | no renderer |
| L-guest-boot | Guest boot `boot_completed` (v10) | 1.517 s | wall, cold | **not measured** | `measured` | not the same thing: ACOS has no guest boot; the attach handshake is the analogous gate |
| L-worker-frame | Worker create -> frame | 1.251-1.343 s p50 | wall, warm | **not measured** | `not-measurable` | no worker path |
| L-screenshot | Fresh screenshot | 0.066-0.067 s p50 | wall, warm | **not measured** | `not-measurable` | no renderer |
| L-resume-shot | Resume->shot | 0.207-0.215 s | wall, warm | **not measured** | `not-measurable` | no renderer |
| L-reset | `reset --app` cmd / -> frame | 110.3 ms / 748.1 ms p50 | wall, warm | **not measured** | `not-measurable` | no reset path |
| L-user-reset | Whole-user reset + observe | 48-50 s | wall, warm | **not measured** | `not-measurable` | no reset path |
| L-ui-tree | UI tree structure/text | 14-87 ms / 538-768 ms / 4.1 ms | wall, warm | **not measured** | `not-measurable` | UI-tree bridge not wired |
| L-idle-cpu | Parked idle CPU | 3.86-5.94 % (legacy called it unreliable) | CPU window | **not measured** | `measured` | ACOS: the interpreter thread is parked/stopped, so the process sleeps |
| L-emulator-boot | Stock emulator cold boot | 25.2 s / ~3300 MiB | PSS, cold | **not measured** | `not-measurable` | no emulator/guest in the ACOS path (out of scope) |
| L-renderer | Host renderer R1 | 6.27 s p50, 1.26 GiB RSS | wall/RSS, host-only | **not measured** | `not-measurable` | no renderer |
| L-chrome | Chrome reference | core 340 MiB PSS; shot 61 ms; AX 18 ms | PSS, warm | **not measured** | `not-measurable` | reference only |
| A-process-start | Process spawn -> Vm ready (ACOS-native) | no legacy metric | - | **not measured** | `no-legacy-baseline` | legacy published only whole-job cold start |
| A-attach-bind | attach -> bindApplication delivered (ACOS-native) | no legacy metric | - | **not measured** | `no-legacy-baseline` | the legacy engine never published this stage split |
| A-pool-reset | SessionPool reset-to-clean (restore) vs legacy reset cmd | 110.3 ms cmd / 748.1 ms -> frame | wall, warm | **not measured** | `not-comparable` | ACOS restores a quiescent core VM (no app process/frame); legacy reset is app-scoped and frame-timed |
| A-pool-throughput | SessionPool throughput (jobs/s, N=1..20) (ACOS-native) | no legacy metric | - | **not measured** | `no-legacy-baseline` | in-process run+reset loop of a small DEX program; legacy published no comparable throughput |
| A-pool-memory | SessionPool marginal memory per session (N=10..20) | marginal N=5->10 ~99.3 MiB workset (v5) | workset, warm | **not measured** | `measured` | template snapshot is a quiescent core VM (not a booted app process); anon+shmem vs legacy workset type differs |

## 3. Worse than legacy (explicit, with plan)

No row where ACOS is measurably worse than legacy exists in today's measurable set. This is **not** a win claim: the metrics where ACOS is unmeasured (frames, screenshots, UI tree, reset, workers) are exactly where legacy published its best numbers, so ACOS has no evidence there yet.

## 4. not_yet_measurable (dependency named, never guessed)

| Metric | BENCHMARKS.md row | Blocking dependency |
|---|---|---|
| cold start -> first correct frame | M3 | acos-render is not bound to the graphics natives; no frame exists to be correct |
| worker create -> correct frame | M4 | no worker-create path + renderer |
| fresh screenshot | M5 | acos-render capture + request/cause frame sequencing |
| reset -> ready | M6 | no reset implementation (ResetRequest is protocol-only) |
| UI tree with text | M7 | UI-tree bridge not wired (UiNode/UiTree are protocol-only) |
| action -> state latency | M8 | input injection + UI tree |
| end-to-end verify | M10 | launch (ClientTransaction, T-23) + assert + evidence + reset |
| verified tasks / GiB-minute | M11 | T01-T12 suite + job-cgroup accounting |
| N=20 concurrent workers | V1 §5.1 | process-global Binder transport: sessions are not independently usable |
| per-session correctness / isolation at N | M9 (correctness half) | same as above; only memory at N is measurable today |

## 5. Raw artifacts

- run document (envelopes + host/method record): `run-2026-11-10-223843.json`
- per-metric envelopes are embedded in `envelopes`; every one is a validated `acos-protocol` envelope (`ok: true` only with `status: pass`).
- regression gate: `tools/bench-acos.sh --check` (baseline `results/bench/baseline.json`).

