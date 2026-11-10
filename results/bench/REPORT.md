# ACOS V1 benchmark report

- run: `run-2026-11-10-201637.json` (2026-11-10T20:20:21, commit `ced0d70`, 20 samples/metric, `release` profile)
- host: 32 threads, 60 GiB RAM, loadavg1 2.93, kernel 7.2.11-arch1-1, `rustc 1.90.0 (1159e78c4 2025-09-14)`
- method: BENCHMARKS.md §2: >=20 samples (p50/p95 nearest-rank), metric type labelled, cold/warm stated, /proc based (no /usr/bin/time), no fixed sleep in a timed path, one tools/guarded.sh scope per metric
- interpreter rows: w25a hot-path rerun `run-2026-11-10-205045.json` (commit `40f075c`, `release`, 20 samples, loadavg1 4.80). That metric's wall time is dominated by `Vm::new` startup rather than by interpretation, so it is host-load-sensitive; the honest per-instruction evidence for this wave is the `vmbench` table in BENCHMARKS.md §8.6 (commit 40f075c, own target dir, 5 processes × best-of-2).
- session pool rows: `run-2026-11-10-204202.json` (commit `40f075c`, `release`, 20 samples/metric; `sessions-pool` uses 5 fresh child processes per N). Full method/scope and isolation analysis: `results/bench/POOL_DESIGN.md`.
- fidelity: `native-runtime` (ACOS executes the code itself). No row here is ART-verified, and no legacy value is an ACOS result.

Metric types are never mixed: RSS, PSS, anon+shmem (`RssAnon+RssShmem`) and private/shared pages are reported in separate labelled columns.

## 1. Measured ACOS metrics

| Metric | Value (p50, p95) | Type | Cold/warm |
|---|---|---|---|
| process spawn -> Vm ready | 16.929 / 18.229 ms | wall (parent-measured) | cold |
| Vm::new (boot classpath loaded) | 1.34 / 1.636 ms | wall | cold |
| boot -> attachApplication | 13.052 / 14.933 ms | wall | cold |
| boot counters | classes=332, boot-classpath-classes=40, clinits=82, instructions=149338, natives=76 | counts | cold |
| - stage `open-apk` | 0.053 / 0.061 ms | wall | cold |
| - stage `load-app-dex` | 0.017 / 0.021 ms | wall | cold |
| - stage `prepare-main-looper` | 0.784 / 1.22 ms | wall | cold |
| - stage `instantiate-activity-thread` | 6.684 / 7.629 ms | wall | cold |
| - stage `attach-application` | 5.508 / 6.167 ms | wall | cold |
| attach -> bindApplication delivered | 2.311 / 2.732 ms | wall | cold |
| bindApplication -> parked/blocked | 12.534 / 13.429 ms | wall | cold |
| first blocker after bind | `uncaught Ljava/util/MissingResourceException;: Could not find the bundle android/icu/impl/data/icudt72b/likelySubtags` | structured | - |
| fixed idle core (Vm only, zero tasks): PSS | 30794.0 / 30847.0 KiB | PSS | settled |
| fixed idle core (Vm only, zero tasks): RSS | 34604.0 / 34748.0 KiB | RSS | settled |
| fixed idle core (Vm only, zero tasks): anon+shmem | 26984.0 / 26988.0 KiB | anon+shmem | settled |
| fixed idle core (Vm only, zero tasks): private / shared | 29568.0 / 5068.0 KiB | private/shared pages | settled |
| fixed idle core (framework booted): PSS | 166739.0 / 166866.0 KiB | PSS | settled |
| fixed idle core (framework booted): RSS | 170628.0 / 170768.0 KiB | RSS | settled |
| fixed idle core (framework booted): anon+shmem | 161384.0 / 161388.0 KiB | anon+shmem | settled |
| fixed idle core (framework booted): private / shared | 165508.0 / 5164.0 KiB | private/shared pages | settled |
| N=1 sessions: PSS / anon+shmem / private / shared | 166757.0 / 161384.0 / 165488.0 / 5192.0 KiB (p50) | mixed, labelled | fresh process per N |
| N=2 sessions: PSS / anon+shmem / private / shared | 181672.0 / 176316.0 / 180364.0 / 5204.0 KiB (p50) | mixed, labelled | fresh process per N |
| N=5 sessions: PSS / anon+shmem / private / shared | 225744.0 / 220324.0 / 224492.0 / 5184.0 KiB (p50) | mixed, labelled | fresh process per N |
| N=10 sessions: PSS / anon+shmem / private / shared | 298970.0 / 293612.0 / 297672.0 / 5184.0 KiB (p50) | mixed, labelled | fresh process per N |
| incremental N=1 | 157.602 MiB/session | anon+shmem | fresh process |
| incremental N=2 | 86.092 MiB/session | anon+shmem | fresh process |
| incremental N=5 | 43.032 MiB/session | anon+shmem | fresh process |
| incremental N=10 | 28.673 MiB/session | anon+shmem | fresh process |
| marginal N=1->N=2 | 14.582 MiB/session | anon+shmem | fresh process |
| marginal N=2->N=5 | 14.326 MiB/session | anon+shmem | fresh process |
| marginal N=5->N=10 | 14.314 MiB/session | anon+shmem | fresh process |
| N=1 sessions (shared boot): PSS / anon+shmem / private / shared | 166829.0 / 161396.0 / 165616.0 / 5088.0 KiB (p50) | mixed, labelled | fresh process per N |
| N=2 sessions (shared boot): PSS / anon+shmem / private / shared | 181702.0 / 176336.0 / 180472.0 / 5092.0 KiB (p50) | mixed, labelled | fresh process per N |
| N=5 sessions (shared boot): PSS / anon+shmem / private / shared | 225652.0 / 220304.0 / 224444.0 / 5080.0 KiB (p50) | mixed, labelled | fresh process per N |
| N=10 sessions (shared boot): PSS / anon+shmem / private / shared | 299022.0 / 293640.0 / 297792.0 / 5084.0 KiB (p50) | mixed, labelled | fresh process per N |
| N=20 sessions (shared boot): PSS / anon+shmem / private / shared | 445730.0 / 440376.0 / 444500.0 / 5088.0 KiB (p50) | mixed, labelled | fresh process per N |
| incremental N=1 (shared boot) | 157.613 MiB/session | anon+shmem | fresh process |
| incremental N=2 (shared boot) | 86.102 MiB/session | anon+shmem | fresh process |
| incremental N=5 (shared boot) | 43.028 MiB/session | anon+shmem | fresh process |
| incremental N=10 (shared boot) | 28.676 MiB/session | anon+shmem | fresh process |
| incremental N=20 (shared boot) | 21.503 MiB/session | anon+shmem | fresh process |
| marginal N=1->N=2 (shared boot) | 14.59 MiB/session | anon+shmem | fresh process |
| marginal N=2->N=5 (shared boot) | 14.313 MiB/session | anon+shmem | fresh process |
| marginal N=5->N=10 (shared boot) | 14.323 MiB/session | anon+shmem | fresh process |
| marginal N=10->N=20 (shared boot) | 14.33 MiB/session | anon+shmem | fresh process |
| N=1 session pool: anon+shmem / PSS | 29496.0 / 29500.0 / 33134.0 / 33237.0 KiB (p50) | anon+shmem / PSS (separate) | warm restore |
| N=1 session pool: jobs/s / reset ms / session create/s | 134.525 / 2.411 / 504.273 | wall | warm |
| N=1 session pool: CPU cores used / efficiency | 1.173 / 1.173 cores/session | CPU | warm |
| N=2 session pool: anon+shmem / PSS | 32120.0 / 32128.0 / 35822.0 / 35869.0 KiB (p50) | anon+shmem / PSS (separate) | warm restore |
| N=2 session pool: jobs/s / reset ms / session create/s | 274.071 / 2.062 / 937.018 | wall | warm |
| N=2 session pool: CPU cores used / efficiency | 1.46 / 0.73 cores/session | CPU | warm |
| N=5 session pool: anon+shmem / PSS | 39992.0 / 40000.0 / 43749.0 / 43762.0 KiB (p50) | anon+shmem / PSS (separate) | warm restore |
| N=5 session pool: jobs/s / reset ms / session create/s | 508.059 / 2.827 / 2037.245 | wall | warm |
| N=5 session pool: CPU cores used / efficiency | 4.774 / 0.955 cores/session | CPU | warm |
| N=10 session pool: anon+shmem / PSS | 53128.0 / 53144.0 / 56892.0 / 56907.0 KiB (p50) | anon+shmem / PSS (separate) | warm restore |
| N=10 session pool: jobs/s / reset ms / session create/s | 733.341 / 3.656 / 3023.89 | wall | warm |
| N=10 session pool: CPU cores used / efficiency | 8.765 / 0.877 cores/session | CPU | warm |
| N=20 session pool: anon+shmem / PSS | 79404.0 / 79412.0 / 83117.0 / 83158.0 KiB (p50) | anon+shmem / PSS (separate) | warm restore |
| N=20 session pool: jobs/s / reset ms / session create/s | 840.396 / 5.168 / 3198.729 | wall | warm |
| N=20 session pool: CPU cores used / efficiency | 17.012 / 0.851 cores/session | CPU | warm |
| session pool marginal N=1->N=2 | 2.563 MiB/session | anon+shmem | warm |
| session pool marginal N=2->N=5 | 2.563 MiB/session | anon+shmem | warm |
| session pool marginal N=5->N=10 | 2.566 MiB/session | anon+shmem | warm |
| session pool marginal N=10->N=20 | 2.566 MiB/session | anon+shmem | warm |
| idle CPU, parked/blocked | 0.0 / 0.0 ticks over 5 s (0.0 / 0.0 % of one core), zero_ticks=True | CPU window | parked |
| interpreter `02_int_arith` | 16.048 / 18.218 ms, 62380 insns, 3860458.116 insn/s | wall + VM counters | cold |
| interpreter `05_branches_loops` | 15.054 / 16.372 ms, 49732 insns, 3289003.346 insn/s | wall + VM counters | cold |
| interpreter `09_strings` | 19.730 / 25.791 ms, 143608 insns, 7235423.952 insn/s | wall + VM counters | cold |

## 2. ACOS vs legacy, every row of BENCHMARKS.md

Verdicts: `better`/`worse` are computed only for the pairs whose metric type and state match (anon+shmem vs anon+shmem, MiB vs MiB). Everything else is `not-measurable` (no engine capability yet) or `no-legacy-baseline` (legacy never published it). Nothing is estimated.

| Row | Metric | Legacy (ACVM, reference only) | Legacy type/state | ACOS | Verdict | Note |
|---|---|---|---|---|---|---|
| M1 | Fixed idle core memory | 206.0 MiB (W1) / 303.9 MiB (v9); reads span 259-322 MiB | anon+shmem, settled 60 s | 157.6 MiB (booted app process), 26.4 MiB (Vm only); PSS 162.8 MiB | `better (-23.5%)` | ACOS's settled state is a booted framework app process (attach handshake done); the Vm-only column is an even leaner state (boot-classpath DEX is parsed lazily, 40 classes at Vm-ready) |
| M2 | Incremental memory per session at N=5 | 78.7 MiB (v9) / 33.2 MiB (W2 opt-in) | anon+shmem, warm | 43.032 MiB/session at N=5 | `better (-45.3%)` | in-process sequential sessions; immutable boot DEX, parsed DexFiles and decoded method bodies are Arc-shared through the process-global BootCache (no mmap; see results/bench/MEMORY_PROFILE.md) |
| M3 | Cold start -> first correct frame | 3.620 s (v12) | wall, cold | **not measured** | `not-measurable` | needs acos-render bound to the graphics natives |
| M4 | Worker create -> correct frame | 1.251-1.343 s p50 | wall, warm | **not measured** | `not-measurable` | needs a worker-create path + renderer |
| M5 | Fresh screenshot | 0.066-0.067 s p50 | wall, warm | **not measured** | `not-measurable` | needs acos-render capture + frame sequencing |
| M6 | Reset-to-ready | 110.3 ms cmd / 748.1 ms -> frame | wall, warm | **not measured** | `not-measurable` | no reset path (ResetRequest is protocol-only) |
| M7 | UI tree with text | 538-768 ms one-shot / 4.1 ms persistent / 14-87 ms structure | wall, warm | **not measured** | `not-measurable` | UI-tree bridge not wired |
| M8 | Action-to-state latency | not cleanly published (U1 step-5 747 ms incl. tree) | wall, warm | **not measured** | `not-measurable` | needs input injection + UI tree |
| M9 | Concurrent workloads (N) | N=5 scorecard, N=10 measured; 10 extra displays | count + memory | N=10 measured for memory: 286.7 MiB anon+shmem at N=10, marginal 14.314 MiB/session (N=5->10) | `measured` | memory is measured; independent state/input at N is NOT (process-global Binder transport) |
| M10 | End-to-end verify wall time | install-bound dev loop ~19 s, 74-81 % adb install (no direct equivalent) | wall | **not measured** | `not-measurable` | needs launch/assert/evidence/reset/cleanup |
| M11 | Verified tasks / GiB-minute | not published as such | ratio | **not measured** | `not-measurable` | needs the T01-T12 suite + job-cgroup accounting |
| L-idle-core | Legacy bare core (P2c, stripped) | 278 MiB | anon, settled | 26.4 MiB (Vm only, anon+shmem) | `better (-90.5%)` | closest state = Vm ready with zero tasks; ACOS also holds the boot classpath in the heap |
| L-session-n1 | Per-session N=1 (v9) | 130.3 MiB | anon+shmem, warm | 157.602 MiB/session at N=1 | `worse (+21.0%)` | in-process session |
| L-marginal | Marginal N=5->10 (v5) | ~99.3 MiB workset | workset, warm | 14.314 MiB/session (N=5->10) | `better (-85.6%)` | anon+shmem, not cgroup workset |
| L-cold-first-shot | Cold core->first shot (v12) | 3.620 s | wall, cold | **not measured** | `not-measurable` | no renderer |
| L-guest-boot | Guest boot `boot_completed` (v10) | 1.517 s | wall, cold | 0.013 s attach handshake incl. Vm init (no guest kernel/zygote to boot) | `measured` | not the same thing: ACOS has no guest boot; the attach handshake is the analogous gate |
| L-worker-frame | Worker create -> frame | 1.251-1.343 s p50 | wall, warm | **not measured** | `not-measurable` | no worker path |
| L-screenshot | Fresh screenshot | 0.066-0.067 s p50 | wall, warm | **not measured** | `not-measurable` | no renderer |
| L-resume-shot | Resume->shot | 0.207-0.215 s | wall, warm | **not measured** | `not-measurable` | no renderer |
| L-reset | `reset --app` cmd / -> frame | 110.3 ms / 748.1 ms p50 | wall, warm | **not measured** | `not-measurable` | no reset path |
| L-user-reset | Whole-user reset + observe | 48-50 s | wall, warm | **not measured** | `not-measurable` | no reset path |
| L-ui-tree | UI tree structure/text | 14-87 ms / 538-768 ms / 4.1 ms | wall, warm | **not measured** | `not-measurable` | UI-tree bridge not wired |
| L-idle-cpu | Parked idle CPU | 3.86-5.94 % (legacy called it unreliable) | CPU window | 0.0 % of one core, 0.0 ticks over 5 s | `measured` | ACOS: the interpreter thread is parked/stopped, so the process sleeps |
| L-emulator-boot | Stock emulator cold boot | 25.2 s / ~3300 MiB | PSS, cold | **not measured** | `not-measurable` | no emulator/guest in the ACOS path (out of scope) |
| L-renderer | Host renderer R1 | 6.27 s p50, 1.26 GiB RSS | wall/RSS, host-only | **not measured** | `not-measurable` | no renderer |
| L-chrome | Chrome reference | core 340 MiB PSS; shot 61 ms; AX 18 ms | PSS, warm | **not measured** | `not-measurable` | reference only |
| A-process-start | Process spawn -> Vm ready (ACOS-native) | no legacy metric | - | 16.929 ms p50 / 16.929 ms (p95 18.229) | `no-legacy-baseline` | legacy published only whole-job cold start |
| A-attach-bind | attach -> bindApplication delivered (ACOS-native) | no legacy metric | - | 2.311 ms | `no-legacy-baseline` | the legacy engine never published this stage split |
| A-pool-reset | SessionPool reset-to-clean (restore) vs legacy reset cmd | 110.3 ms cmd / 748.1 ms -> frame | wall, warm | 2.411 ms p50 at N=1; 2.827 ms at N=5 | `not-comparable` | ACOS restores a quiescent core VM (no app process/frame); legacy reset is app-scoped and frame-timed |
| A-pool-throughput | SessionPool throughput (jobs/s, N=1..20) (ACOS-native) | no legacy metric | - | 134.525 jobs/s at N=1; 840.396 at N=20 | `no-legacy-baseline` | in-process run+reset loop of a small DEX program; legacy published no comparable throughput |
| A-pool-memory | SessionPool marginal memory per session (N=10..20) | marginal N=5->10 ~99.3 MiB workset (v5) | workset, warm | 2.566 MiB/session (anon+shmem) | `measured` | template snapshot is a quiescent core VM (not a booted app process); anon+shmem vs legacy workset type differs |

## 3. Worse than legacy (explicit, with plan)

| Row | Metric | Legacy | ACOS | Delta | Plan |
|---|---|---|---|---|---|
| L-session-n1 | Per-session N=1 (v9) | 130.3 MiB | 157.602 MiB/session at N=1 | worse (+21.0%) | in-process session |

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

- run document (envelopes + host/method record): `run-2026-11-10-201637.json`
- per-metric envelopes are embedded in `envelopes`; every one is a validated `acos-protocol` envelope (`ok: true` only with `status: pass`).
- regression gate: `tools/bench-acos.sh --check` (baseline `results/bench/baseline.json`).

