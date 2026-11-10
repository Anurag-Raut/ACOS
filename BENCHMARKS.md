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

## 5. Baseline comparisons still to obtain

1. An **ACOS-native** H0 run (APK/DEX parse) on the fixture corpus.
2. The first **H2** execution of a real compiled DEX fixture vs. `jvm-reference`,
   with an explicit fidelity label.
3. A whole-job **GiB-minute** baseline, even a poor one, before any optimisation
   claim.
4. An **art-reference** differential run, only once the RAM/authorisation check
   for H4 passes.
