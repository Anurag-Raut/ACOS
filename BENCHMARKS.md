# ACOS — Benchmarks

**Status:** methodology defined; **no ACOS measurements exist yet.** The legacy
baseline below is copied from the frozen ACVM repo and is a *reference*, not an
ACOS result. Nothing here may be used as an ACOS speed claim.

Rules borrowed from the legacy engine are cited in
`docs/reference/ACVM_INVENTORY.md` §5.

---

## 1. Metric definitions

All memory is in **MiB (2²⁰ B)** unless stated; all times in **seconds**. Every
reported value states its **metric type** and **cold/warm** state. Never mix
types (`docs/handoff/04`, legacy `EXPERIMENTS.md` §18/§24).

| Metric | Definition | Unit / type |
|---|---|---|
| **verified tasks / GiB-min** | ⬆ north-star: correctly completed predeclared tasks ÷ ∫(job memory GiB × dt minutes) | ratio |
| **job memory** | exclusive job cgroup `memory.current` over non-overlapping subtrees (incl. charged file/kernel pages), + attributable zram physical; peaks and integral; **never** PSS and cgroup summed | MiB |
| **fixed/core idle memory** | settled memory of the runtime core with zero tasks | MiB (state type: anon, anon+shmem, `memory.current`, PSS, host Δ) |
| **per-session incremental memory** | `(loaded − bare) / N` at N=1/2/5/10 | MiB / session |
| **marginal per-session memory** | `(total(N_high) − total(N_low)) / (N_high − N_low)` | MiB / session |
| **cold core-ready → first correct frame** | process/engine start → first **correct** fresh frame, no owned live core beforehand | seconds, cold |
| **worker/task create → correct frame** | create request → first correct fresh frame on a warm core | seconds, warm |
| **app launch → first frame** | explicit launch → first fresh frame | seconds, warm |
| **fresh screenshot** | retrieval latency of a request-sequenced, non-stale frame | seconds, warm |
| **reset → ready** | reset command latency and time to first fresh usable frame; scope stated (`app`/`task`/`user`) | seconds, warm |
| **UI tree (structure / text)** | latency of one tree dump on the stated route | seconds, warm |
| **action → state latency** | tap/type/scroll/navigation issue → observed state change | seconds, warm |
| **end-to-end verify** | build → install → launch → assert → evidence → reset → cleanup | seconds and per-step |
| **CPU** | idle/parked, active frame producer, interaction load, JIT overhead; wall CPU and % of one core, distinguished | % / core-seconds |
| **correctness** | per-app pass/fail, **false passes (must be 0)**, image/tree accuracy, crash classification | counts |
| **isolation** | cross-app secret/state leakage, sandbox escapes, privileged interfaces, shared-cache leaks | pass/fail counts |
| **repeatability** | first-run vs warmed cache; ≥20 reps for latency (p50/p95); paired/interleaved A/B; full raw logs; guard records | distribution |

Memory metric types must be named explicitly: **PSS**, **USS**, **RSS**,
**cgroup `anon+shmem` (workset)**, **cgroup `memory.current`**, **host
`MemAvailable` Δ**, **zram physical**, **swap logical** (reported separately, not
added to physical RAM).

---

## 2. Methodology rules

1. **Same inputs across contenders**: identical APK (sha256), screen, fixture,
   density, state, CPU architecture, API level, frame-freshness contract and test
   actions. Record image/toolchain/config versions.
2. **Paired/interleaved A/B** (`A/B/B/A`), **≥3 fresh-core pairs** for memory and
   **≥20 samples** for latency; report p50, p95 and spread. Noise floor is the
   larger of run-to-run spread and 5 %.
3. **No fixed sleeps** in a timed path. Completion is proven by a
   request-sequenced fresh frame / polling, never by waiting.
4. **Cold and warm separately.** Report first-run and warmed-cache conditions.
5. **Whole job is charged**: build, JVM/daemon, core(s), helpers, retained pools,
   install, observation, reset, fallback, cleanup. No bare-core subtraction as a
   denominator; report the integral and peaks.
6. **Never mix metric types** or cross-session host states.
7. **Zero false passes.** A false pass blocks promotion regardless of score.
8. **Correctness/regression gates are mandatory**: a must-have regression blocks
   promotion; a deliberate trade-off requires an explicit decision.
9. **Fidelity label on every result.** A preview or a JVM match is never an
   Android-verified result. Native success statistics exclude fallback successes.
10. **Host safety is part of the method**: 85 % RAM hard ceiling (abort below
  admission), spawn lock, no `/tmp`/`/dev/shm` scratch, bounded timeouts,
  `ulimit -v 3000000` for host-only test runs, per-job cgroup ceilings, emergency
  cleanup. No guard bypass; repair any harness that hardcodes one.
11. **Record failures, false passes and negative findings**, not only wins.
12. **Publish submit→completion latency separately from zero-RAM queue time** so
  throughput cannot conceal bad latency.

### 2.1 Fixed representative dev-task suite (target, from `docs/handoff/04` / legacy T01–T12)

12 frozen tasks with known-good **and** seeded-wrong variants and independent
oracles: T01 build/install; T02 pure state logic; T03 component preview; T04
long text/large font/RTL; T05 incremental install preserving data; T06
tap→screen→back; T07 type/scroll/submit; T08 HTTP/SQLite persistence; T09
crash detection + app reset; T10 two accounts concurrent; T11 two views shared
state; T12 permission dialog + provider/intent flow. Compose and WebView must
appear in the visual/live cases. Unsupported inputs must escalate/refuse.

---

## 3. Legacy ACVM baseline (reference only — not an ACOS result)

Copied from the frozen repo; full citations in
`docs/reference/ACVM_INVENTORY.md` §2. **Caveats:** these are one host, one
frozen image set, mostly `acvm-core:v9`/`v10`; bare-core reads vary 259–322 MiB
across host states; memory types differ per row (stated); the late ACVM Lite
direction means this engine is frozen.

| Metric | Legacy value | Metric type | Cold/warm | Source |
|---|---|---|---|---|
| Stock emulator cold boot | 25.2 s / ~3300 MiB | PSS | cold | `EXPERIMENTS.md` §E1.1 |
| Legacy bare core (P2c, stripped) | 278 MiB | anon | settled | `EXPERIMENTS.md` §5 |
| Legacy bare core (v9 base) | 303.9 MiB | anon+shmem | settled, 60 s | `iter/W1.md` |
| Legacy bare core (W1 diet) | 206.0 MiB | anon+shmem | settled, 60 s | `iter/W1.md` |
| Per-session N=1 / N=5 (v9) | 130.3 / 78.7 MiB | anon+shmem | warm, 60 s | ITERATIONS v9 |
| Per-session N=1 / N=5 (v6 KSM, opt-in) | 18.4 / 40.4 MiB | workset | warm | ITERATIONS v6 |
| Per-session N=1 / N=5 (W2 soft render, opt-in) | 67.9 / 33.2 MiB | workset | warm | `iter/W2.md` |
| Marginal N=5→10 (v5) | ~99.3 MiB ws / 112.1 `memory.current` | ws / current | warm | `EXPERIMENTS.md` §v6 |
| Shared-process extra view (W7) | ≈21–29 MiB | workset | warm, **no isolation** | `iter/W7.md` |
| Cold core→first shot (v4 → v12) | 18.94 → 11.29 → 4.198 → 3.620 s | wall | cold | ITERATIONS / `iter/V12.md` |
| Guest boot `boot_completed` (v9 → v10) | 4.221 → 1.517 s | wall | cold | ITERATIONS v10 |
| Worker create→frame | 1.251–1.343 s p50 | wall | warm | `EXPERIMENTS.md` §21/§23 |
| Fresh screenshot | 0.066–0.067 s p50 | wall | warm | ITERATIONS v1/v9 |
| Resume→shot | 0.207–0.215 s | wall | warm | ITERATIONS v1/v9 |
| `reset --app` cmd / → frame | 110.3 ms / 748.1 ms p50 | wall | warm | `iter/D2.md` |
| Whole-user reset + observe | ~48–50 s | wall | warm | `iter/U1.md` |
| UI tree structure / one-shot text / persistent text | 14–87 ms / 538–768 ms / 4.1 ms | wall | warm | `EXPERIMENTS.md` §12/§25 |
| Parked idle CPU | 3.86–5.94 % (unreliable) | CPU window | parked | ITERATIONS v0–v9 |
| Host renderer R1 | 6.27 s p50, 1.26 GiB RSS | wall/RSS | warm, host-only | ITERATIONS R1 |
| Chrome reference | core 340 MiB PSS; tab 18–24 MiB; shot 61 ms; AX 18 ms | PSS | warm | `EXPERIMENTS.md` §13 |

### 3.1 Legacy research targets (aspirations, never promises)

From `docs/handoff/04`: fixed idle core 100–150 MiB or lower; light task
20–40 MiB (30–60 MiB sandboxed); cold first correct screenshot ≤1 s
(aspirational); worker create→frame ≤0.2 s; fresh screenshot ≤0.067 s (no
regression), stretch ≤0.03 s; reset-to-ready ≤0.75 s, stretch ≤0.2 s; UI text
tree ≤20 ms; N=10 independently demonstrated, later N=20/50. **No target
overrides correctness, compatibility or isolation.**

---

## 4. ACOS results (empty)

No ACOS measurement exists. This table is intentionally blank, and a row may
only be added with a reproducible artifact path and method record. Do not
pre-fill estimates.

| Date | Commit | Metric | Value | Type | Cold/warm | Fixture | Evidence |
|---|---|---|---|---|---|---|---|
| — | — | *(nothing measured)* | — | — | — | — | — |

### 4.1 Method record template (required with every future row)

```yaml
date: 2026-...
commit: <acos git sha>
host: {cpu, threads, ram_total, ram_available_at_start, loadavg1, zram/swap}
fixture: {path, sha256, package, activity, api_level, abi}
image/toolchain: {verified-built-by, rustc, sdk build-tools}
metric: {name, definition, unit, type, cold_or_warm}
method: {samples, interleaving, settle_s, guard, locks}
result: {p50, p95, min, max, raw_path}
correctness: {oracle, fidelity, negative_case, false_passes}
notes/regressions: ...
```

---

## 5. V1 metric acceptance table (owner directive D2)

Every tracked metric is an **acceptance gate for every capability**
(`SPEC.md` D2). Each metric below must be reported against the legacy ACVM
baseline with **≥20 repetitions, p50/p95**, the metric type stated and the
cold/warm state stated. Every metric where ACOS is **worse than legacy must be
listed explicitly with a plan** — regressions and gaps are reported, never
hidden. Correctness and **zero false passes** gate every speed claim.

Legacy values are reference only (`docs/reference/ACVM_INVENTORY.md` §2); they
are one frozen host/image set and are not directly comparable across metric
types. ACOS column: **no row is measured yet.**

| # | Metric | Definition (type) | Legacy baseline (ACVM) | ACOS (empty) | Gate |
|---|---|---|---|---|---|
| M1 | Fixed idle core memory | settled core, anon+shmem (also `memory.current`, PSS) | W1 206.0 / v9 303.9 MiB; reads span 259–322 MiB across host states | — | V1 §6.1 |
| M2 | Incremental memory per session at N=5 | `(loaded−bare)/5`, anon+shmem | 78.7 MiB (v9); 33.2 MiB (W2 opt-in) | — | V1 §6.1 |
| M3 | Cold start → first correct frame | engine start → correct fresh frame, wall, cold | 3.620 s (v12); 4.198 s (v11) | — | V1 §1.1, §6.1 |
| M4 | Worker create → correct frame | create request → correct fresh frame, wall, warm | 1.251–1.343 s p50 | — | V1 §1.1, §6.1 |
| M5 | Fresh screenshot | request-sequenced frame retrieval, wall, warm | 0.066–0.067 s p50 | — | V1 §1.3 |
| M6 | Reset-to-ready | `reset --app` cmd and → first frame, wall, warm | 110.3 ms cmd / 748.1 ms → frame p50 | — | V1 §1.8 |
| M7 | UI tree with text | one tree dump with text, wall, warm | one-shot 538–768 ms; persistent 4.1 ms; structure 14–87 ms | — | V1 §1.4 |
| M8 | Action-to-state latency | tap/type/scroll issue → observed state, wall, warm | not cleanly published (U1 step-5 747 ms incl. tree) | — | V1 §1.5 |
| M9 | Concurrent workloads | N with independent state/input, count | N=5 scorecard, N=10 measured; 10 extra displays | — | V1 §5.1 |
| M10 | End-to-end verify wall time | build→install→launch→assert→evidence→reset→cleanup | U2 dev loop install-bound (~19 s `adb install -r` = 74–81 % of loop); no direct equivalent | — | V1 §6.1 |
| M11 | Verified tasks / GiB-minute | correct tasks ÷ ∫(job GiB × min) | not published as such (task-first plan proposed it) | — | V1 §6.3 |

Additional required reporting: false-pass count (must be 0), fallback/escalation
count, peak and integrated job memory, CPU, and the exact fixture/image/toolchain
versions. See `docs/V1.md` §6 and §4.1 method record.

---

## 6. Measurement tooling constraints (verified 2026-11-10)

- **`/usr/bin/time` is not installed** on this host; use `/proc`
  (`/proc/<pid>/status`, `/proc/<pid>/stat`, cgroup v2 files) or shell builtins.
  Any tool that shells out to `/usr/bin/time` will fail (`docs/FINDINGS.md`
  F-0006).
- **`/tmp` is a 31 GiB tmpfs** (RAM-backed); large/generated artifacts must go
  to `.cache/` or `fixtures/build/` (both gitignored), not `/tmp`
  (`docs/FINDINGS.md` F-0007).
- **RAM guard 85 %**; unit/host-only runs use `ulimit -v 3000000`; the ART oracle
  emulator is ≤2 GiB guest RAM, one instance, always shut down (ADR-0015).
- No fixed sleeps in a timed path; completion is proven by a request-sequenced
  fresh frame (V1 §1.3).

> **Note:** the existing legacy benchmark harness (`bench/compare.py`) encodes
> the legacy must-have metric set; ACOS must implement its own runner for M1–M11.
> Until then, **no ACOS benchmark row or speed claim exists.**

---

## 7. Baseline comparisons still to obtain

1. An **ACOS-native** H0 run (APK/DEX parse) on the fixture corpus.
2. The first **H2** execution of a real compiled DEX fixture vs. `jvm-reference`,
   with an explicit fidelity label.
3. A whole-job **GiB-minute** baseline, even a poor one, before any optimisation
   claim.
4. An **art-reference** differential run, only once the RAM/authorisation check
   for H4 passes.

---

## 8. ACOS V1 metrics harness + first ACOS results (2026-11-10)

The harness now exists: `tools/bench-acos.sh` drives `acos bench <metric>`
(`crates/acos-cli/src/bench.rs`). Every command emits a standard
`acos-protocol` envelope (re-parsed and invariant-checked before printing), and
every result carries its metric type, unit and cold/warm state.

```sh
tools/bench-acos.sh                     # measure everything that exists today (>=20 samples)
tools/bench-acos.sh --quick             # 3 samples: smoke test, NOT a claim
tools/bench-acos.sh --check             # regression gate vs results/bench/baseline.json
tools/bench-acos.sh --check --from-run results/bench/run-<stamp>.json
tools/bench-acos.sh --profile dev       # unoptimised build (default: release)
```

Artifacts: `results/bench/run-<stamp>.json` (envelopes + host/commit/method
record), `results/bench/REPORT.md` (ACOS-vs-legacy table for every row above),
`results/bench/baseline.json` (committed gate: 14 rows, tolerance 25 % or the
measured p95/p50 spread, whichever is larger; exits non-zero on a regression).

### 8.1 Measured (rows may now be filled for these metrics only)

Run `results/bench/run-2026-11-10-191849.json`, commit `fad5534`, `release`
profile, 20 samples/metric (memory: 5 fresh processes per N), host 32 threads /
60 GiB, kernel 7.2.11-arch1-1, rustc 1.90.0, loadavg1 3.01. Fidelity
`native-runtime` — **not** ART-verified.

| Metric | ACOS | Type | Cold/warm | Evidence |
|---|---|---|---|---|
| process spawn → `Vm` ready | 20.841 ms p50 / 21.873 ms p95 | wall (parent-measured) | cold | `result.process-start` |
| `Vm::new` (boot classpath) | 16.415 / 17.125 ms | wall | cold | `result.boot-attach.vm_ready_ms` |
| boot → `attachApplication` | 95.115 / 97.656 ms | wall | cold | `result.boot-attach.boot_ms` |
| – stage `prepare-main-looper` | 73.495 / 76.127 ms | wall | cold | per-stage rows |
| – stage `instantiate-activity-thread` | 11.303 / 11.683 ms | wall | cold | per-stage rows |
| – stage `attach-application` | 10.213 / 10.986 ms | wall | cold | per-stage rows |
| boot counters | 332 classes (40 boot classpath), 82 `<clinit>`, 149 338 instructions, 76 natives | counts | cold | traced probe (counters only) |
| attach → `bindApplication` delivered | 3.760 / 3.900 ms | wall | cold | `result.bind-app` |
| `bindApplication` → parked/blocked | 27.703 / 28.642 ms | wall | cold | `result.bind-app` |
| first blocker after bind | `Ljava/util/MissingResourceException;: Could not find the bundle android/icu/impl/data/icudt72b/likelySubtags` | structured | — | `result.bind-app.first_blocker` |
| fixed idle core (Vm only, zero tasks) | 26 780 KiB anon+shmem / 30 637 KiB PSS / 34 472 KiB RSS (p50) | anon+shmem, PSS, RSS (separate) | settled | `result.idle-rss.variants.vm` |
| fixed idle core (framework booted) | 158 744 KiB anon+shmem (155.0 MiB) / 163 983 KiB PSS (160.1 MiB) / 167 864 KiB RSS | anon+shmem, PSS, RSS (separate) | settled | `result.idle-rss.variants.booted` |
| per-session incremental N=1/2/5/10 | 157.6 / 86.1 / 43.0 / 28.7 MiB per session | anon+shmem | fresh process per N | `result.sessions.incremental` |
| marginal N=1→2 / 2→5 / 5→10 | 14.6 / 14.3 / 14.3 MiB per session | anon+shmem | fresh process per N | `result.sessions.marginal` |
| per-session N=1/2/5/10/20 (`sessions-shared`) | 157.6 / 86.1 / 43.0 / 28.7 / 21.5 MiB per session | anon+shmem | fresh process per N | `result.sessions-shared.incremental` |
| marginal N=10→20 (`sessions-shared`) | 14.33 MiB per session | anon+shmem | fresh process | `result.sessions-shared.marginal` |
| N=10 total | 293 640 KiB anon+shmem (286.7 MiB), shared pages flat 5.0 MiB | anon+shmem + private/shared | fresh process | `result.sessions.sessions` |
| N=20 total (`sessions-shared`) | 440 376 KiB anon+shmem (430.0 MiB), 20 separate heaps, shared pages flat 5.0 MiB | anon+shmem + private/shared | fresh process | `result.sessions-shared.sessions` |
| boot-classpath sharing | 50.2 MiB raw DEX + 25.4 MiB parsed + 9.2 MiB decoded code held **once** per process; per-session marginal ≈ 14.3 MiB | structural bytes + process | warm in-process | `results/bench/MEMORY_PROFILE.md` |
| idle CPU, parked/blocked | 0 ticks over 5 s in all 20 windows (0.0 % of one core) | CPU window | parked | `result.idle-cpu` |
| interpreter `02_int_arith` | 22.823 / 22.988 ms, 62 380 insns, 2.73 M insn/s | wall + counters | cold | `result.interpreter` |
| interpreter `05_branches_loops` | 22.663 / 22.905 ms, 49 732 insns, 2.19 M insn/s | wall + counters | cold | `result.interpreter` |
| interpreter `09_strings` | 24.559 / 25.260 ms, 143 608 insns, 5.85 M insn/s | wall + counters | cold | `result.interpreter` |

### 8.2 ACOS vs legacy (§3/§5), every row

| Row | Legacy | ACOS | Verdict |
|---|---|---|---|
| M1 fixed idle core | 206.0 MiB (W1) / 303.9 MiB (v9) anon+shmem | 155.0 MiB anon+shmem booted; 26.2 MiB Vm-only | **better** (−24.8 % vs W1); states differ (see note) |
| M2 per-session at N=5 | 78.7 MiB (v9) anon+shmem | 43.0 MiB/session | **better** (−45.3 %) |
| M3 cold → first correct frame | 3.620 s | not measured | `not-yet-measurable` (no renderer) |
| M4 worker create → frame | 1.251–1.343 s | not measured | `not-yet-measurable` (no worker path) |
| M5 fresh screenshot | 0.066–0.067 s | not measured | `not-yet-measurable` |
| M6 reset-to-ready | 110.3 ms / 748.1 ms | not measured | `not-yet-measurable` |
| M7 UI tree with text | 538–768 ms / 4.1 ms / 14–87 ms | not measured | `not-yet-measurable` |
| M8 action-to-state | not cleanly published | not measured | `not-yet-measurable` |
| M9 concurrent workloads | N=5 scorecard, N=10 | N=10 memory measured (1536.7 MiB); independent state/input **not** | partial |
| M10 end-to-end verify | ~19 s install-bound | not measured | `not-yet-measurable` |
| M11 tasks / GiB-minute | not published | not measured | `not-yet-measurable` |
| bare core (P2c) | 278 MiB anon | 26.2 MiB Vm-only anon+shmem | **better** (−90.4 %), different state (lazy classpath) |
| per-session N=1 (v9) | 130.3 MiB | 155.0 MiB | **worse** (+19.0 %) |
| marginal N=5→10 (v5) | ~99.3 MiB workset | 14.3 MiB anon+shmem | **better** (−85.6 %); not the same metric type |
| guest `boot_completed` (v10) | 1.517 s | 0.608 s attach handshake | not comparable (no guest boot) |
| parked idle CPU | 3.86–5.94 % (legacy: unreliable) | 0.0 % | **better** |
| process spawn → VM ready | no legacy metric | 20.841 ms | `no-legacy-baseline` |
| attach → bindApplication | no legacy metric | 3.760 ms | `no-legacy-baseline` |
| everything else in §3 (frames, shots, reset, UI tree, renderer, emulator, Chrome) | legacy values | — | `not-yet-measurable` / reference only |

### 8.3 Worse than legacy — explicit, with plan

1. **RESOLVED for N≥2: per-session memory.** The immutable boot classpath is
   now shared through `acos_dex::cache::BootCache` (raw bytes + parsed
   `DexFile` + compact class index, keyed by file fingerprint) and
   `acos_vm::codecache` (decoded `MethodCode`, keyed by `DexFile::id` +
   `code_off`), both process-global `Arc` caches. Measured: N=5 incremental
   **43.0 MiB/session vs 78.7 MiB legacy (−45.3 %, better)**, marginal N=5→10
   **14.3 vs ~99.3 MiB (−85.6 %, better)**, N=20 total **430.0 MiB** for 20
   independent heaps (21.5 MiB/session). The first session still costs
   **157.6 MiB (+21 % vs legacy N=1)** because it pays the whole boot classpath;
   the remaining ~14.3 MiB marginal is per-VM DEX handle caches (~9.6 MiB) plus
   class/method/type tables. Full profile and the isolation trade-off (processes
   remain the isolation unit; in-process sharing is for trusted agent-owned
   sessions; zygote-like template proposed for untrusted N):
   `results/bench/MEMORY_PROFILE.md`.
2. **Vm-only core is not comparable to the legacy bare core as a like-for-like
   claim.** ACOS parses boot-classpath DEX lazily (40 classes at `Vm` ready), so
   its 26.2 MiB is a leaner state than a legacy core that had its image loaded.
   The honest ACOS number to quote against a booted legacy core is the 155.0 MiB
   booted state.
3. **M9 is only half-measured.** `Runtime::with_host` installs one
   process-global Binder transport (`install_transport`), so N live runtimes are
   a memory experiment, not a working multi-session configuration. N=20 and any
   per-session correctness/leak claim stay `not-yet-measurable` until sessions
   get their own transports.

### 8.4 not_yet_measurable (dependency named, never filled with a guess)

| Metric | Dependency |
|---|---|
| cold start → first correct frame (M3) | `acos-render` is not bound to the graphics natives; no frame exists to be correct |
| worker create → correct frame (M4) | no worker-create path + renderer |
| fresh screenshot (M5) | `acos-render` capture + request/cause frame sequencing |
| reset → ready (M6) | no reset implementation (`ResetRequest` is protocol-only) |
| UI tree with text (M7) | UI-tree bridge not wired (`UiNode`/`UiTree` are protocol-only) |
| action → state (M8) | input injection + UI tree |
| end-to-end verify (M10) | launch (`ClientTransaction`, T-23) + assert + evidence + reset |
| verified tasks / GiB-minute (M11) | T01–T12 suite + job-cgroup accounting |
| N=20 workers (V1 §5.1) | per-session Binder transports (see 8.3.3) |

`acos bench <metric>` for any of these returns `status: unsupported` with the
dependency in the message — it never returns a number.

### 8.5 Method notes / limits of this run

- The `bindApplication` phase is expected to stop at the first unwired
  dependency; the structured blocker is reported (today: the ICU
  `likelySubtags` resource bundle) and the phase wall time is reported as-is,
  not as a success.
- Boot-stage timings are produced by a probe that mirrors
  `acos_runtime::boot::boot_app_process` call-for-call (same public API, same
  order) and fails loudly if the attach handshake is not recorded. Per-stage
  class/`<clinit>`/instruction/native counts come from one extra **traced**
  probe so tracing overhead never inflates a timed sample.
- Memory is `/proc/self/smaps_rollup` + `/proc/self/status` (PSS, RSS,
  `RssAnon+RssShmem`, private vs shared) — never PSS summed with a cgroup value.
- Idle-CPU windows are 5 s each in a fresh process after boot+bind; 20 windows.
- No `/usr/bin/time`, no `/tmp` scratch, one `tools/guarded.sh` scope per
  metric, `CARGO_TARGET_DIR` respected.
- This section is the only place ACOS results are recorded; §4's table stays
  empty on purpose (single authoritative table, §8.1).

### 8.6 Interpreter hot-path wave w25a (2026-11-10, commit 40f075c)

Profile: no `perf`/`samply` on the host, so the profile used the interpreter's
own counters (`vmbench --ops`, which routes every instruction through the
general path and prints the opcode histogram) plus per-opcode before/after wall
time on the `crates/acos-vm/benches/java` programs. Builds: `release`, own
`CARGO_TARGET_DIR=.cache/target-w25a`, `CARGO_BUILD_JOBS=6`, every command under
`tools/guarded.sh`. Each timed row is **p50 over 5 fresh processes, each
`best-of-2` internal repetitions** (10 samples per program); spread is
`(max-min)/p50` and is 1-8 % under host loadavg 4-25 (other agents were
building/running concurrently, so before/after pairs are read within one
optimisation's window, never across distant windows).

| Program | p50 before (ms) | p50 after (ms) | Speedup | Dominant opcodes (histogram) |
|---|---:|---:|---:|---|
| `BenchFields` | 642.09 | 429.11 | **1.50x** | iget/iput/iget-object/aget-object |
| `BenchCollections` | 1860.84 | 130.85 | **14.2x** | ArrayList.get + HashMap.get + boxing |
| `BenchVirtualCall` | 4566.87 | 3890.12 | **1.17x** | invoke-virtual + return |
| `BenchGc` | 693.58 | 624.19 | **1.11x** | new-array + GC |
| `BenchAlloc` | 3064.05 | 2837.86 | **1.08x** | new-instance + invoke-direct + iget/iput |
| `BenchFib` | 621.81 | 580.26 | 1.07x | invoke-static recursion |
| `BenchString` | 198.88 | 189.95 | 1.05x | StringBuilder.append(int/char) |
| `BenchStaticCall` | 3952.70 | 3892.01 | 1.02x | invoke-static + return |
| `BenchVirtual` | 2220.54 | 2283.65 | 0.97x (noise) | 3-class invoke-interface |
| `BenchLoop` | 1643.76 | (unchanged) | — | and-int/lit16, aget/aput, add-int/lit8 |
| `BenchArith` | 1833.40 | (unchanged) | — | int-to-long, add-long/2addr, add-int/2addr |
| `BenchArray` | 1186.81 | (unchanged) | — | aget, aput, and-int/lit16 |

Kept changes (each measured separately; dropped if it did not win):

1. **`iget`/`iput` bridge gating** (`interp.rs`). `bridge::internal_field_get`
   allocated two descriptor `String`s and probed them on **every** instance
   field read, even for ordinary user classes. The interpreter now calls the
   hook only when the resolved declaring class is `java/lang/Object`,
   `java/lang/Class` or `java/lang/String`. `BenchFields` 642→431 ms.
2. **Virtual/interface inline cache** (`vm.rs` `DexCtx::virtual_cache` +
   `interp.rs`). One-entry `(receiver class → method)` cache per call site ahead
   of the tuple-keyed `dispatch_cache` `HashMap`; monomorphic sites never touch
   the hash map after the first call. `BenchVirtualCall` 4567→3890 ms.
3. **Boxed-`Integer` equality fast path** (`corelib/mod.rs` `value_equals`).
   `HashMap`/`ArrayList` linear scans call `equals` O(n) per probe; the generic
   path paid a full Java dispatch + `call_method`. Two Integers now compare
   their `value` slots directly. `BenchCollections` 1861→142 ms.
4. **Array-class probe cache** (`vm.rs` `loaded_array_elems`). `Vm::alloc` used
   `format!("[{elem}")` + `class_by_desc` probe on every array allocation; the
   per-VM `elem_desc` set removes that allocation. `BenchGc` 694→624 ms.
5. **`ArrayList` read path** (`corelib/collections.rs`). `get`/`size`/`isEmpty`
   cloned the whole backing `Vec<Handle>` per call; now length/bounds are read
   in place. Neutral in wall time on `BenchCollections` (map equality dominated)
   but removes an O(n) copy per access.
6. **StringBuilder int/long append** (`corelib/lang.rs`). Decimal digits are
   written straight into the UTF-16 buffer instead of `v.to_string()` +
   `encode_utf16()`. Neutral in wall time on `BenchString` (native-call overhead
   dominates) but removes a per-append heap allocation.

Not done / rejected: superinstructions and a full invoke/return fast path were
not attempted — the general path still pays a frame push/pop per call, and the
measurement budget was spent on the wins above. No change regressed a benchmark
beyond its measured spread.

Correctness for the wave (all on commit 40f075c + these changes):

* `tests/fast_path_equivalence.rs`: 2/2 pass (fast path == general path, normal
  and GC stress).
* `tools/diff-dexprogs.sh --corelib real`: **25 pass / 0 fail**; `--gc-stress`:
  **25 pass / 0 fail**.
* `cargo test --release -p acos-vm -- --skip thousand_events`: every target
  green except the two `res_natives` cases, which **already fail at HEAD without
  these changes** (verified by reverting the wave patch and re-running);
  `clippy --release -p acos-vm` reports no warning in any touched file (the one
  warning is the pre-existing `let_unit_value` in `natives/fwk_surface.rs`).

Interpreter-metric rerun (`tools/bench-acos.sh interpreter`,
`run-2026-11-10-205045.json`, loadavg1 4.80): `02_int_arith` 16.048 ms p50 /
18.218 p95, `05_branches_loops` 15.054 / 16.372, `09_strings` 19.730 / 25.791.
That metric's wall covers `Vm::new` (boot/native bring-up) as well as
interpretation, so it is a startup + host-load measurement, not a per-instruction
one; it stays better than the committed `baseline.json` p50 (22.823 / 22.663 /
24.559 ms) but worse than the low-load `ced0d70` run. The `vmbench` table above
is the per-instruction evidence.

### 8.7 SessionPool: fast session creation, reset and concurrent throughput (2026-11-10, W25-C / T-44)

Run `results/bench/run-2026-11-10-204202.json`, commit `40f075c`, `release`,
20 samples/metric, host 32 threads / 60 GiB. `sessions-pool`: `corelib real`,
5 fresh child processes per `N`, 10 throughput rounds per child, workload
`fixtures/build/dexprogs/02_int_arith` (`Main`). Fidelity `native-runtime` —
**not** ART-verified. This is a metric-only row set; it does not change any
row above.

The pool boots **one** core `Vm` (`Vm::new` + `Vm::snapshot`, T-42/W19-E) and
restores every session from those few-KiB bytes with the process-global
`BootCache`/`codecache` (W21-B) warm. The template is a quiescent **core VM**,
not a booted application process (the T-42 snapshot preconditions refuse
application DEX), so its numbers are a different scope from `sessions-shared`
(which runs `boot_app_process` per session) and the two are never mixed.

| Metric | N=1 | N=2 | N=5 | N=10 | N=20 | Type | Cold/warm |
|---|---:|---:|---:|---:|---:|---|---|
| total `anon+shmem` p50 (MiB) | 28.80 | 31.37 | 39.06 | 51.88 | 77.54 | anon+shmem | warm restore |
| total PSS p50 (MiB) | 32.36 | 34.98 | 42.72 | 55.56 | 81.17 | PSS | warm restore |
| marginal per session (MiB) | — | 2.56 | 2.56 | 2.57 | 2.57 | anon+shmem | warm restore |
| session create rate p50 (sessions/s) | 504.3 | 937.0 | 2037.2 | 3023.9 | 3198.7 | wall, concurrent restore | warm |
| per-session restore p50 (ms) | 1.94 | 1.77 | 2.25 | 2.80 | 5.41 | wall | warm |
| jobs/s p50 (run + reset loop) | 134.5 | 274.1 | 508.1 | 733.3 | 840.4 | wall | warm |
| reset-to-clean p50 (ms) | 2.41 | 2.06 | 2.83 | 3.66 | 5.17 | wall | warm |
| CPU cores used p50 | 1.17* | 1.46 | 4.77 | 8.77 | 17.01 | core-seconds/wall | warm |
| CPU cores/session | 1.17* | 0.73 | 0.96 | 0.88 | 0.85 | ratio | warm |

\* N=1 CPU is a tick-granularity artefact (a round is ~7 ms, one host tick is
10 ms). The efficiency column is meaningful for N≥2 only.

Determinism / isolation in this run: all five `N` produced exactly **one**
distinct `(exit_code, stdout)` (`distinct_outputs_max = 1`), `failed = 0`,
`panics = 0`, `timeouts = 0`, `restore_failures = 0`. The acceptance tests
(`crates/acos-runtime/tests/session_pool.rs`) additionally prove determinism
across `N`, `catch_unwind` panic isolation with lazy replacement, reset
equivalence on a stateful program, and wall-watchdog killing of an infinite
loop.

Comparison (different scope — stated, never a like-for-like speed claim):

| Row | Legacy | ACOS SessionPool | Verdict |
|---|---|---|---|
| `reset --app` cmd / → frame | 110.3 ms / 748.1 ms p50 | 2.41 ms p50 (N=1) restore-to-clean core | `not-comparable` (legacy is app-scoped and frame-timed) |
| per-session memory | 78.7 MiB N=5 (v9) anon+shmem | 39.06 MiB total at N=5 (7.81 MiB/session) | `better`, different scope (core VM, not booted app) |
| marginal N=10→20 | ~99.3 MiB workset (v5) | 2.57 MiB anon+shmem | `better`, different metric type/scope |
| worker create → frame | 1.251–1.343 s p50 | 1.10 ms restore (no frame) | `not-comparable` (no renderer) |
| concurrent N | N=5 scorecard, N=10 | N=1/2/5/10/20 executed concurrently, deterministic | **M9 correctness half now measured** (for the agent's trusted, in-process core-VM workload) |

Worse-than-legacy rows: none in the measurable set. This is **not** a win claim:
legacy's frame/reset-to-ready numbers still have no ACOS counterpart.

Isolation (honest, full analysis in `results/bench/POOL_DESIGN.md`): the shared
boot DEX and decoded code are immutable, but this pool is **in-process**. A
memory-safety or native escape could reach another session's heap, the
`HostGuard` RSS ceiling is process-wide, and `catch_unwind` cannot contain an
abort. **Processes remain the isolation boundary for untrusted APKs**;
in-process pooling is for the agent's own trusted workloads. Defaults chosen:
60 s wall, template heap quota, RSS disabled.

### 8.8 Interpreter hot-path wave w28a: invoke/return fast path + GC generation mark (2026-11-10, W28-A)

Target: the remaining weak spots left by §8.6 — `BenchStaticCall`/`BenchVirtualCall`
(~3.9 s), `BenchFib` (580 ms), `BenchAlloc` (2.8 s) and `BenchGc` (624 ms) — without
touching correctness. Base = HEAD `40f075c` + the §8.6 w25a changes (the committed
baseline binary `.cache/vmbench-base-w28a`); "after" = base + the three kept changes
below. Builds: `release`, own `CARGO_TARGET_DIR=.cache/target-w28a`,
`CARGO_BUILD_JOBS=4`, every command under `tools/guarded.sh` (ADR-0018).
Method: **10 interleaved base/new rounds per program, each `best-of-1` internal
repetition, p50 over the 10 samples**; spread is `(max-min)/p50`. Host 32 threads /
60 GiB, loadavg1 2.56 (quiet window), so the base and after rows are read inside one
window per program. The same table is the evidence for every "kept" decision; each
change was measured alone before being kept.

| Program | base p50 (ms) | after p50 (ms) | speedup | base spread | after spread | `java -Xint` p50 (ms) |
|---|---:|---:|---:|---:|---:|---:|
| `BenchStaticCall` | 3909.61 | 3396.68 | **1.15x** | 13.4 % | 6.6 % | 2927 |
| `BenchVirtualCall` | 3882.49 | 3511.81 | **1.11x** | 5.2 % | 5.9 % | 2842 |
| `BenchVirtual` | 2230.49 | 2013.69 | **1.11x** | 5.0 % | 8.0 % | 1133 |
| `BenchFib` | 578.59 | 501.76 | **1.15x** | 5.9 % | 2.6 % | 332 |
| `BenchAlloc` | 2843.77 | 2586.87 | **1.10x** | 6.8 % | 1.6 % | 1125 |
| `BenchGc` | 659.15 | 598.31 | **1.10x** | 2.6 % | 6.0 % | 138 |
| `BenchString` | 145.92 | 134.82 | **1.08x** | 6.1 % | 3.8 % | 582 |
| `BenchCollections` | 135.79 | 129.73 | **1.05x** | 1.6 % | 2.1 % | 81 |
| `BenchLoop` | 1674.59 | 1604.80 | **1.04x** | 2.5 % | 1.8 % | 362 |
| `BenchArith` | 1889.64 | 1814.99 | **1.04x** | 1.7 % | 0.7 % | 468 |
| `BenchArray` | 1219.80 | 1196.35 | **1.02x** | 2.7 % | 6.3 % | 328 |
| `BenchFields` | 427.87 | 420.37 | **1.02x** | 4.4 % | 4.7 % | 75 |

The `java -Xint` column is OpenJDK 17 `-Xint` (confirmed `interpreted mode`,
`TieredCompilation=false`) measured with the same wall-clock method on the same host,
so it is a like-for-like **interpreted** comparison (it includes ~50-80 ms of JVM
startup, so the steady-state gap is slightly larger than the raw ratio). The ACOS
interpreter is still 3.6-5.6x behind HotSpot's template interpreter on the
pure-dispatch loops (`Array` 3.6x, `Arith` 3.9x, `Loop` 4.4x, `Fields` 5.6x),
1.16-1.51x behind on call-heavy programs (`StaticCall` 1.16x, `VirtualCall` 1.24x,
`Fib` 1.51x), and 2.3-4.3x behind on `Alloc`/`Gc`; it is **ahead** on `BenchString`
(135 vs 582 ms) because the native `StringBuilder` append path does not go through
the generic interpreter. No row regressed; `BenchFields` and `BenchArray` moved only
within their spread.

Kept changes (each measured alone; a change that did not win was dropped):

1. **`return` fast path in `run_batch`** (`interp.rs`). Every `return*` used to pay
   `exec_one`'s prologue — a `MethodImpl` match, a method lookup and an instruction
   fetch — for the opcode that ends every call. The new `exec_return` helper does the
   same register/result transfer and `pop_frame` the general path does, without the
   prologue. `BenchFib` 579→502 ms, `BenchStaticCall` 3910→3397 ms.
2. **`invoke-*` fast path in `run_batch`** (`interp.rs`). `invoke` already had an
   inline cache; the remaining cost was the `exec_one` prologue in front of it. The
   fast path calls `invoke` directly and routes a `JavaException` through the same
   `handle_exception` as the general path. `BenchVirtualCall` 3882→3512 ms,
   `BenchVirtual` 2230→2014 ms.
3. **GC generation bitmap + reused worklist** (`heap.rs`, `vm.rs`). `Vm::gc` allocated
   a fresh `vec![false; n]` mark bitmap and a fresh mark stack per collection. The
   bitmap is now a per-heap generation array (`gc_mark[slot] == gc_gen` means live,
   reset only on wrap) and the worklist is reused across collections
   (`std::mem::take`/restore). The sweep also probes `string_hashes` only for dead
   `Object::String` slots instead of every dead slot. `BenchGc` 659→598 ms with
   `gc_ms` 139→53 on the same run; `BenchAlloc` 2844→2587 ms.
4. **`new-array` redundant `to_string()`** (`interp.rs`). `strip_prefix` already
   returns a `&str`; the extra `to_string()` allocated a second `String` per
   `new-array`. Small, kept because it is a strict allocation removal and measured
   inside the `BenchGc`/`BenchAlloc` win above.

Rejected / not kept (measured, then reverted):

* Two `dispatch.rs` `fast_run` restructurings (split `Frame` borrow + `Regs` window
  struct, and `&mut Vec` variants). Mixed: `BenchLoop` 0.945-0.968x, `BenchArith`
  0.949-0.968x, with only `BenchStaticCall` +3-4 %. Reverted.
* A `new-instance`/`new-array` fast path in `run_batch` (shared helper with the
  general path). No win on `BenchAlloc` (1.08x either way) and it grew `run_batch`
  enough to regress the pure-dispatch benches to 0.94-0.97x. Reverted.
* `#[inline(never)]` on `dispatch::fast_run` to keep it out of `run_batch`: 0.90x on
  `BenchLoop`/`BenchArith`/`BenchArray` (the inlined copy is faster). Reverted.
* Superinstructions for hot pairs (`const+if`, `add-int/lit+if-lt`, `iget+add`) were
  not attempted: the measurement budget went to the wins above and the
  `fast_op`/`fast_arith` match already covers the hot opcode families.

Correctness for the wave (worktree at `40f075c` + these changes):

* `tests/fast_path_equivalence.rs`: 2/2 pass (fast path == general path, normal and
  GC stress) after the final `&elem`→`elem` clippy fix.
* `tools/diff-dexprogs.sh --corelib real`: **25 pass / 0 fail**; `--gc-stress`:
  **25 pass / 0 fail** on the `40f075c` worktree. (`26_regex_matrix` is a fixture
  added *after* `40f075c` in a later commit; the `40f075c` binary reports it
  unsupported, so it is excluded from this wave's 25/25 — `diff-dexprogs` classifies
  it as "unsupported", not "fail".)
* `cargo clippy --release -p acos-vm --all-targets -- -D warnings`: clean in the
  worktree; on the current main tree the only two errors are pre-existing
  `unnecessary_cast` in `natives/fwk_input.rs` (a file this wave does not touch).

The changes were applied to the current main tree as localized patches (the main tree
has since moved past `40f075c` with W33-A/W35-A work in `interp.rs`/`vm.rs`/`heap.rs`,
so the patches were re-read and applied against the current text, never by
overwriting whole files). Re-run on that tree:

* `cargo check --release -p acos-vm --lib`: passes.
* `cargo test --release -p acos-vm --test fast_path_equivalence`: 2/2 pass.
* `cargo test --release -p acos-vm --test dexprogs -- --skip thousand_events`: 2/2
  pass (the main tree's newer corelib supports `26_regex_matrix`, so the full
  26/26 corpus is green).
* `tools/diff-dexprogs.sh --corelib real`: **26 pass / 0 fail / 0 unsupported**;
  `--gc-stress`: **26 pass / 0 fail / 0 unsupported**.

Interpreter-metric rerun (`tools/bench-acos.sh interpreter --runs 10`,
`run-2026-11-10-223843.json`, loadavg1 2.56, release): `02_int_arith`
6.548 ms p50 / 7.434 p95, `05_branches_loops` 6.183 / 6.523, `09_strings`
8.001 / 8.996. The metric's wall covers `Vm::new` (boot/native bring-up) as well as
interpretation, so it is a startup + host-load measurement, not a per-instruction
one. These rows are much lower than the §8.6 run (16.048 / 15.054 / 19.730 ms p50)
because that run was on a loadavg1 4.80 host and this one on 2.56, and because the
main tree now carries later W33-A/W35-A work; they are **not** a like-for-like
speed claim against §8.6. The `vmbench` table above is the per-instruction evidence.

### 8.9 Typed-IR execution tier, w36a (2026-11-10, W36-A)

Target: the §8.8 residuals on the pure-dispatch loops (`BenchLoop` 4.4x,
`BenchArith` 3.9x, `BenchArray` 3.6x, `BenchFields` 5.6x behind `java -Xint`).
This wave adds an **opt-in** typed-IR tier: each DEX method body is decoded once
into a compact `&[Op]` program (operands pre-resolved, branch targets as IR
indices, hot pairs fused into superinstructions) executed by a tight `match`
loop over the same register file. It is enabled only by `ACOS_EXEC=ir`;
**default behaviour is unchanged**.

Design (all in new file `crates/acos-vm/src/exec.rs`; hooks are localized):

* `Op` is a small enum. The fused op this wave adds is `ArithLitIf` — any
  `<arith>/lit` (0xd0-0xe2) immediately followed by a conditional branch where
  the branch is not a branch/handler re-entry point. That is the loop-increment
  idiom (`add-int/lit8` + `if-*`) in every microbench.
* Anything else (invoke, return, field access/resolution, allocation, monitors,
  strings, switches, every opcode that can throw) lowers to `Op::Fallback`: the
  loop stops *before* that instruction and `run_batch` runs it through the
  existing general path. Catch-table/GC semantics are therefore untouched —
  every throwing instruction is still executed by the general interpreter,
  which unwinds with the original DEX pc.
* Lowered programs are immutable and cached process-wide keyed by the shared
  `Arc<MethodCode>` address (the cache holds the `Arc`, so the key cannot be
  reused); per-frame caching is `Arc<MethodCode>`-pinned. No per-VM quickening is
  needed because the tier does not resolve fields/methods itself.
* Only bodies with a backward branch use the tier: a straight-line body gets no
  fused-dispatch win and would pay the entry cost on a call-heavy path. This
  gate moved `BenchFib`/`BenchVirtual`/`BenchVirtualCall` from a 1.2-1.4x
  regression to par/slight wins.

Method: **10 interleaved off/on rounds per program, `vmbench` best-of-1 per
round, p50 over the 10 samples**; `java -Xint` p50 over 5 runs, same host/window.
Base = current main tree with `ACOS_EXEC` unset; tier = the same binary with
`ACOS_EXEC=ir`. Builds `release`, own `CARGO_TARGET_DIR=.cache/target-w36a`,
every command under `tools/guarded.sh` (ADR-0018).

| Program | off p50 (ms) | ir p50 (ms) | speedup | off spread | ir spread | `java -Xint` p50 (ms) | ir / `-Xint` |
|---|---:|---:|---:|---:|---:|---:|---:|
| `BenchArith` | 2226.01 | 841.41 | **2.65x** | 2.9 % | 6.2 % | 516.27 | 1.63x |
| `BenchLoop` | 1840.55 | 821.32 | **2.24x** | 1.0 % | 6.6 % | 395.92 | 2.07x |
| `BenchArray` | 1359.35 | 697.04 | **1.95x** | 1.5 % | 9.9 % | 317.45 | 2.20x |
| `BenchFields` | 456.27 | 405.38 | **1.13x** | 1.6 % | 6.0 % | 72.96 | 5.56x |
| `BenchFib` | 517.10 | 526.79 | 0.98x | 1.6 % | 2.4 % | 321.50 | 1.64x |
| `BenchStaticCall` | 3518.41 | 3310.96 | **1.06x** | 2.0 % | 12.8 % | 2807.74 | 1.18x |
| `BenchVirtualCall` | 3631.37 | 3352.39 | **1.08x** | 7.4 % | 2.8 % | 2857.92 | 1.17x |
| `BenchVirtual` | 2095.21 | 1851.29 | **1.13x** | 2.1 % | 2.4 % | 1111.48 | 1.67x |
| `BenchAlloc` | 2723.61 | 2563.25 | **1.06x** | 3.8 % | 7.7 % | 1141.89 | 2.24x |
| `BenchGc` | 643.88 | 574.18 | **1.12x** | 2.1 % | 3.3 % | 122.41 | 4.69x |
| `BenchString` | 134.18 | 126.94 | **1.06x** | 7.8 % | 2.6 % | 593.04 | 0.21x |
| `BenchCollections` | 132.46 | 132.05 | 1.00x | 2.6 % | 7.5 % | 77.61 | 1.70x |

Verdict: the tier is a clear win on the dispatch-bound loops (2.0-2.7x, closing
roughly half the gap to `java -Xint`: Loop 4.4x → 2.1x, Arith 3.9x → 1.6x,
Array 3.6x → 2.2x) and a small win or wash elsewhere. It is **kept, but default
off** (`ACOS_EXEC=ir`), because `BenchFields`/`BenchGc` are still 4.7-5.6x behind:
the tier deliberately does not execute field ops, so the graph walk stays on the
general path. Next step is per-VM quickening (resolved field offsets in a side
vector indexed by IR instruction) so `iget`/`iput`/`sget`/`sput` become IR ops,
then invoke inline-caching inside the tier; §8.8's register-window idea was not
needed because the compact `Op` stream alone already won on dispatch.

Rejected during the wave (measured, then reverted):

* Using the tier for **all** methods: 1.2-1.4x regressions on call-heavy
  programs (`BenchVirtual`, `BenchStaticCall`, `BenchVirtualCall`) because each
  tiny callee paid the per-segment entry cost. Replaced by the backward-branch
  gate.
* Calling the global lowering cache from `run_batch` per segment: the `Mutex`
  probe was paid per frame creation (frames are pooled, and a pooled frame's
  body changes between A/B/C callees). Replaced by per-frame `Arc` caching plus
  a cheap `is_loop_body` scan, and by not clearing the cached program when a
  pooled frame is reused for the same body.

Correctness for the wave (worktree = current main tree + `exec.rs`/frame/`interp`
hooks):

* `crates/acos-vm/tests/ir_equivalence.rs`: **2/2 pass** — every DEX corpus
  program, byte-identical stdout/exit for IR vs the general interpreter, normal
  and GC-stress.
* `tools/diff-dexprogs.sh --corelib real` with `ACOS_EXEC=ir`: **26 pass / 0 fail
  / 0 unsupported**; `--gc-stress`: **26 pass / 0 fail / 0 unsupported**.
* `cargo clippy --release -p acos-vm --lib --tests -- -D warnings`: clean.
* With `ACOS_EXEC` unset the only differences in the interpreter loop are a
  `Frame` field and a hoisted `exec::enabled()` check, so the default path is
  byte-for-byte the previous one (`ir_equivalence` also covers this via
  `count_opcodes`).

#### 8.9.1 Field quickening, w39a (2026-11-10, W39-A)

W36-A left `iget`/`iput`/`sget`/`sput` on the general path, so the
field-heavy graph walk never entered the tier. This wave lowers **all field
widths** (`0x52`-`0x6d`) into the IR and gives the tier its own quickening:

* Lowering assigns every field instruction a process-unique *quick slot*; each
  `Vm` keeps `ir_fields`, a **side vector indexed by IR instruction**
  (`FieldQuick::{Unresolved, Fallback, Ready(QuickField)}`). `exec::quicken`
  runs once per IR segment (off the per-instruction path) and fills any
  unresolved slot through the existing `Vm::resolve_field`.
* A slot is `Fallback` when (a) resolution fails (the general path must report
  the original error at the original pc), (b) the field is a VM-internal shadow
  field of `Object`/`Class`/`String`, or (c) the field is static and its class
  is not yet `InitState::Init` (so `<clinit>` still runs on the general path).
  A null receiver also stops the loop before the instruction, so the NPE is
  raised by the general path at the same pc. Every throwing path is therefore
  still general-path, exactly as before.
* The side vector is **per-VM**: a snapshot restore builds a fresh `Vm`, so it
  starts empty and re-resolves against the restored layout;
  `Vm::invalidate_ir_quickening` clears it for any class (re)definition. The
  lowered program only stores the DEX instruction and its slot, never an
  offset, so it survives both.

Method: identical to §8.9 — **10 interleaved off/on rounds per program,
`vmbench` best-of-1 per round, p50 over the 10 samples**; `java -Xint` p50 over
5 runs, same host/window. Base = this wave's tree with `ACOS_EXEC` unset; tier
= the same binary with `ACOS_EXEC=ir`. Builds `release`, own
`CARGO_TARGET_DIR=.cache/target-w39a`, every command under `tools/guarded.sh`
(ADR-0018). `ACOS_CORELIB=real`.

| Program | off p50 (ms) | ir p50 (ms) | speedup | off spread | ir spread | `java -Xint` p50 (ms) | ir / `-Xint` |
|---|---:|---:|---:|---:|---:|---:|---:|
| `BenchFields` | 450.06 | 120.23 | **3.74x** | 2.3 % | 3.3 % | 74.22 | **1.62x** |
| `BenchGc` | 628.99 | 592.82 | **1.06x** | 5.9 % | 7.5 % | 135.76 | 4.37x |
| `BenchVirtual` | 2036.80 | 1875.75 | **1.09x** | 2.4 % | 4.6 % | 1163.61 | **1.61x** |
| `BenchAlloc` | 2801.84 | 2374.82 | **1.18x** | 7.8 % | 3.7 % | 1111.33 | **2.14x** |
| `BenchFib` | 519.68 | 524.18 | 0.99x | 4.0 % | 6.4 % | 335.47 | **1.56x** |
| `BenchStaticCall` | 3486.36 | 3382.25 | **1.03x** | 4.8 % | 5.3 % | 2930.08 | **1.15x** |

Verdict: field quickening takes `BenchFields` from 405 ms (w36a) to **120 ms**,
3.74x over off and **1.62x of `java -Xint`** — the original 5.6x gap is closed.
The call-heavy programs stay level or slightly ahead (`BenchFib` 0.99x is inside
the 4-6 % spread; `BenchStaticCall`/`BenchVirtual`/`BenchAlloc` improve), so
there is no call regression.

`BenchGc` is the one miss: 4.37x of `java -Xint`, not the 3x target. It is
allocation/GC-bound, not dispatch-bound (9.0 % of its 44.6 M instructions are
`new-array`, every one of which must run a GC-capable general allocation, and
54 ms of the run is actual GC). Adding allocation to the tier would require a
GC-capable path inside the frame-borrowed loop, so it is left for a separate
wave; the tier’s contribution here is the surrounding fused arithmetic
(1.06x).

Decisions taken from measurement:

* **Backward-branch gate kept.** Removing it (lower every body) was measured:
  `BenchVirtual` regressed from 1856 ms to 2368 ms (+28 %) while other programs
  barely moved, so straight-line bodies still use `fast_run`.
* **Invokes remain the tier boundary.** Pushing a callee frame invalidates the
  tier’s `&mut Frame` borrow, so an invoke cannot be executed inside the same
  tight loop without re-indexing the frame through `&mut Vm` on every
  instruction (which would regress the arithmetic loops). `run_batch` executes
  the call immediately through the existing per-call inline caches
  (`dexs[..].virtual_cache`, `dexs[..].method_cache`), which is the inline
  caching for the boundary; measured call benches do not regress.

Correctness for w39a (worktree = main tree + `exec.rs`/`interp.rs`/`vm.rs`):

* `crates/acos-vm/tests/ir_equivalence.rs`: **2/2 pass** — every DEX corpus
  program, byte-identical stdout/exit for IR vs the general interpreter, normal
  and GC-stress.
* `tools/diff-dexprogs.sh --corelib real` with `ACOS_EXEC=ir`: **27 pass / 0 fail
  / 0 unsupported**; `--gc-stress`: **27 pass / 0 fail / 0 unsupported** (the
  27th fixture, `27_annotations_reflection`, landed concurrently with W38-A).
* `cargo clippy --release -p acos-vm --lib --tests -- -D warnings`: clean.
* Full `cargo test -p acos-vm --tests`: green except the two pre-existing
  `res_natives` `resolve=true` failures, reproduced with this wave’s three
  source files stashed (i.e. present on the base tree, unrelated to the tier).

Default-on recommendation: **keep default-off.** With `ACOS_EXEC=ir` the tier is
now correct and a large win where it applies, but it is still a net wash on the
allocation/GC-bound program and no faster than `fast_run` on some straight-line
bodies (hence the loop gate). `ir_equivalence` plus the `--corelib real` 27/27
run give the correctness evidence, but the remaining `BenchGc` 4.37x gap and the
extra per-VM quickening state argue for an explicit opt-in until allocation is
also tiered.

