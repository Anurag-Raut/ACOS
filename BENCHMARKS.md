# ACOS — Benchmarks

**Status:** methodology defined; **ACOS measurements exist**. The single
authoritative V1 table is **§9.8** (W176-A rerun at committed HEAD `fd1a887`,
2026-11-11): **9 PASS / 2 FAIL (M5, M9), all 11 legacy-gated metrics
measured**. §9.1 (W160-A) and §9.7 (W165-A) are the earlier tables and
measurement notes and are retained as history; their M8/M10/M11 rows are stale.
§8.x is the wave-by-wave history that led there. The legacy baseline below is copied from
the frozen ACVM repo and is a *reference*, not an ACOS result; nothing outside
§9 may be used as an ACOS speed claim. **The ART D2 table is §12** (W39 rows plus the
W52 M11 row); its latest M11 row is **NOT_COMPARABLE** to the legacy-derived 24.4 bar,
so §12 has **0 PASS, 1 NOISY, 5 NOT_COMPARABLE, 5 NOT_MEASURED** and no ART D2 PASS is
claimed. §9.8 remains the Rust-VM table.

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

Note on the two core rows (added W57, 2026-11-12): the legacy "workset" column is
defined by the cited source as **the core cgroup's anon + shmem**
(`docs/reference/ACVM_INVENTORY.md` §2, `cgroup_mem`), so it is the same
`CGROUP_ANON_SHMEM` type this document's ACOS rows measure; the anon-only and
`memory.current` components of the same W1/v9 runs are 203.4 / 285.7 MiB and
301.1 / 409.9 MiB. §12.1 relies on this when it rejects the mixed-type M11 bar for
its other two inputs, not for the 206.0 MiB core.

### 3.1 Legacy research targets (aspirations, never promises)

From `docs/handoff/04`: fixed idle core 100–150 MiB or lower; light task
20–40 MiB (30–60 MiB sandboxed); cold first correct screenshot ≤1 s
(aspirational); worker create→frame ≤0.2 s; fresh screenshot ≤0.067 s (no
regression), stretch ≤0.03 s; reset-to-ready ≤0.75 s, stretch ≤0.2 s; UI text
tree ≤20 ms; N=10 independently demonstrated, later N=20/50. **No target
overrides correctness, compatibility or isolation.**

---

## 4. ACOS results (superseded by §9)

The authoritative ACOS measurement table is **§9** (raw artifact
`results/bench/v1-2026-11-11-051824.json`). This section is kept as the original
blank template and must not be filled: a row may only be added with a
reproducible artifact path and method record, never a pre-filled estimate.

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
types. ACOS values: **measured in §9.1** (commit `2c682c7`, 2026-11-11, release;
see §9 for the current 4 PASS / 7 FAIL result). The table below is the frozen
gate definition; §9.1 is the result.

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


### 8.10 Framework-init snapshot (ACOSSNAP): launch latency, W45-A (2026-11-11)

Target: cut app-launch latency (time from `acos launch` to `app_created` /
first resume) by booting the framework once, snapshotting the quiescent
`ActivityThread`-attached `Vm` (ACOSSNAP), and restoring per launch instead of
re-running the framework boot. Ownership: `crates/acos-vm/src/snapshot.rs`,
`crates/acos-runtime/src/session_pool.rs`, `crates/acos-cli/src/bench.rs`,
`tools/bench-acos.sh`.

Run `results/bench/run-2026-11-11-001239.json` (commit `66e4232`, `dev`
profile, 20 samples/metric, host 32 threads / 60 GiB, `corelib real`,
fixture `fixtures/build/apps/hello-views.apk`). New reproducible row:
`tools/bench-acos.sh --profile dev launch` (the `launch` metric is in the
default `METRICS` list; `acos bench launch --runs 20 --warmup 2`). Fidelity
`native-runtime` — **not** ART-verified.

`bench launch` measures two paths in one process (so the process-global boot
DEX/decoded-code caches are equally warm):

* **before** — the `acos launch` path today: `Runtime::standard_with`
  (`Vm::new`) + `boot_app_process` (`open-apk`, `load-app-dex`,
  `prepare-main-looper`, `instantiate-activity-thread`, `attach-application`) +
  `bindApplication` + `Application.onCreate` + the launch `ClientTransaction` +
  `Activity.onCreate`/`onResume` + `quitSafely`;
* **after** — one `RuntimeTemplate::boot` (`Vm::new` + framework stages +
  `Vm::snapshot`, paid once) and then per launch: `Vm::restore` +
  `Runtime::with_vm` + APK/DEX load + `bindApplication` + `ClientTransaction`
  + `quitSafely`.

| phase | before p50 / p95 (ms) | after p50 / p95 (ms) | note |
|---|---:|---:|---|
| `Vm` ready (`Vm::new`) | 5.396 / 11.522 | — | part of `restore_ms` after |
| framework boot (`boot_app_process` stages) | 66.545 / 126.268 | — | replaced by `restore_ms` |
| `Vm::restore` from ACOSSNAP | — | 55.282 / 62.592 | re-links 332 classes from the boot DEX |
| app DEX load/short stage | — | 0.048 / 0.055 | |
| `attachApplication` → `bindApplication` | 9.445 / 17.847 | 11.134 / 12.389 | |
| `Application.onCreate` phase | 0.0 / 0.0 | 0.0 / 0.0 | blocked in both (see below) |
| `ClientTransaction` delivery + `Activity.onCreate` | 0.0 / 0.0 | 0.0 / 0.0 | blocked in both |
| **launch total** | **144.407 / 230.761** | **141.714 / 157.110** | after = `restore_ms` + `launch_ms`; p50 **1.019x** |
| launch phases only (excl. boot/restore) | 77.862 / 104.493 | 84.494 / 90.512 | p50 0.92x, p95 1.15x (flat within host-load noise) |

Template (paid once, amortized over launches): boot `92.413 ms`, snapshot
`0.956 ms`, snapshot size **221 324 B**, 332 classes.

ACOS-vs-legacy (BENCHMARKS.md §3 / `results/bench/baseline.json`):

| row | legacy ACVM | ACOS before | ACOS after | verdict |
|---|---|---|---|---|
| `M3` cold start → first correct frame | 3.620 s | 144.407 ms (`launch` before, no frame) | 141.714 ms (no frame) | `not-comparable` (no renderer; different metric) |
| `M4` worker create → correct frame | 1.251–1.343 s | — | 84.494 ms launch phases | `not-comparable` (no frame) |
| `boot-attach.boot_ms.p50` (`baseline.json`) | 95.115 ms (ACOS baseline) | 66.545 ms | — | framework boot only; the `launch` row is larger |
| `bind-app.attach_to_bind_ms.p50` (`baseline.json`) | 3.760 ms (ACOS baseline) | 9.445 ms | 11.134 ms | same order; host load differs |

**Correctness.** `tools/diff-dexprogs.sh --corelib real` on the same worktree:
**27 pass / 0 fail / 0 unsupported** (`results/vm/dexprogs-w45a-real.json`).
The snapshot change only relaxes the precondition for the host sentinel base
frame and makes the boot fingerprint the stable `BOOTCLASSPATH` definition
instead of the dynamically loaded lazy providers; neither touches the default
`Vm::run` path. The `launch` metric itself is `status: pass`.

**Verdict / what remains.**

* The snapshot is **post-framework-boot**, not post-app-load: the app launch
  chain is not yet stable. In both the before and after paths
  `bindApplication` throws `uncaught java.lang.ExceptionInInitializerError`, so
  `app_created = 0` and `transaction_delivered = 0`; the measured `launch` is
  the real framework work up to that blocker, not a fake pass.
* The framework bring-up (`Vm::new` + `boot_app_process` stages, 71.9 ms =
  5.396 + 66.545) is replaced by the `Vm::restore` (55.3 ms), a **16.7 ms
  phase saving**; the end-to-end p50 saving is 2.7 ms because the app-bound
  `bind` phase is slower in this run and the errored `Application.onCreate`
  run dominates both paths. The launch phases alone are flat
  (77.9 ms before vs 84.5 ms after, 0.92x p50), so the framework bring-up
  saving is not yet reflected end-to-end; the p95 improves **1.47x**
  (the before tail has a 324 ms outlier).
* The dominant remaining restore cost is the per-`Vm` **class re-link**
  (field layouts, vtables, lazy boot-jar pulls) for the 332 snapshot classes;
  sharing parsed class metadata / a lazy-jar provider across restored VMs (T-43)
  is what would turn the 1.02x total into a large win.
* Amortization: the template boot is paid once per process; at N=1 the
  template path is slower than the boot path, and it only pays off for many
  sequential launches until the restore cost is reduced.

Method notes: no `/usr/bin/time`, `/proc` based, one `tools/guarded.sh` scope
per metric, `CARGO_TARGET_DIR` respected, no fixed sleeps in the timed path.

### 8.11 Allocation/GC gap vs `java -Xint` (w48a, 2026-11-11)

Target: the `BenchGc` 4.4x / `BenchAlloc` 2.1x residual left by §8.9.1.
A clean worktree of HEAD `515eeff` (`.cache/wt-w48a`, `CARGO_TARGET_DIR`
`.cache/target-w48a`, every command under `tools/guarded.sh`) was profiled
**first**. No `perf`/`samply` on the host, so the profile is a gperftools
sampling profile (`LD_PRELOAD=/usr/lib/libprofiler.so`, 500 Hz,
`setarch -R` for stable symbols), symbolized with `addr2line` against the
release binary's debug info.

**Profile (BenchGc, 1546 samples).** Time is dominated by dispatch, not the
allocator: `acos_vm::dispatch::fast_op` + `advance`, `acos_vm::interp::execute`,
and `Arc<MethodCode>` clone/drop in the frame push/pop path. `Vm::alloc`
(`interp.rs:860` in the base binary) is ~1.4 % of samples, the mark/sweep ~9 %
(53 ms of a 0.6 s run is `gc_ms`), and the libc allocator frames are low single
digits once the profiler's own signal-handler sample (16 %) is excluded. The
same holds for `BenchAlloc` (`gc_ms` 101 ms of 2.7 s; 10 M `new-instance` are
outweighed by 20 M `return-void`/`invoke-direct`).

**Kept change (1).** `new-array` in `interp.rs`: the component descriptor was
unconditionally copied with `type_descriptor(..).to_string()` — a throwaway
`String` heap allocation per array — although `ArrayObj` already owns a copy.
The descriptor is now borrowed from the DEX type table and only rebuilt on the
cold bounds/`OOM` error path. This is a strict allocation removal (4.0 M fewer
`String`s on `BenchGc`).

**Measured, then rejected.** (2) Scaling the `Object::Instance` field-vector
recycle pool from 4096 to `max(4096, heap_max/4096)` and replacing
`resize(slots,0); fill(0)` with `clear(); resize(slots,0)` in `heap.rs`:
neutral on `BenchAlloc` and a consistent-but-noisy 0.3–1.5 % *slower* on
`BenchGc`, at the cost of caching more dead `Vec`s for the whole heap cap;
reverted. (3) A `new-instance`/`new-array` IR op was **not** attempted: it
would need a GC-capable allocation inside `run_ir`'s frame-borrowed loop, and
the tier deliberately never holds `&mut Vm` (see §8.9.1).

Method: **5 interleaved before/after rounds per program, `vmbench` best-of-3
per round, min reported**; `java -Xint` is OpenJDK 17, the same wall method.
`ACOS_CORELIB=real` except `BenchString`, which hangs under `real` at HEAD for
both binaries and is measured with the default bootstrap corelib (as in §8.8).
Host 32 threads, loadavg1 3–5 (other agents building concurrently).

| Program | before (ms) | after (ms) | after + IR (ms) | `java -Xint` (ms) | after / `-Xint` | IR / `-Xint` |
|---|---:|---:|---:|---:|---:|---:|
| `BenchGc` | 601.75 | 610.90 | 557.83 | 442.57 | 1.38x | 1.26x |
| `BenchAlloc` | 2738.15 | 2783.99 | 2317.26 | 1437.48 | 1.94x | 1.61x |
| `BenchCollections` | 140.28 | 141.29 | 149.75 | 398.24 | 0.35x | 0.38x |
| `BenchString` | 131.30 | 129.88 | 129.52 | 918.20 | 0.14x | 0.14x |

The before/after deltas are **within the run-to-run spread** (a second
8-round interleaved run of the same binaries put `BenchGc` after at 592.10 ms
vs before 598.17 ms — the sign flips). The change is therefore shipped as a
strict work reduction, **not** as a measured win: the allocation/GC gap is
dispatch-bound, and the only large lever left is running `new-array`/
`new-instance` inside the IR tier (rejected above) or a bump/size-class object
arena, both outside a semantics-preserving patch of this size.

Correctness for the wave (worktree = HEAD `515eeff` + the kept change):

* `tools/diff-dexprogs.sh --corelib real`: **27 pass / 0 fail / 0 unsupported**;
  `--gc-stress`: **27 / 0 / 0**; `ACOS_EXEC=ir`: **27 / 0 / 0**; `ACOS_EXEC=ir`
  `--gc-stress`: **27 / 0 / 0**.
* `cargo clippy --release -p acos-vm --lib --tests -- -D warnings`: clean.
* `#![forbid(unsafe_code)]` untouched; the change only moves the existing
  `array_len_fits` check onto the hot path and copies no `String` on success.

Verdict: no measurable closure of the `BenchGc`/`BenchAlloc` gap from an
allocation micro-optimisation. `bench-vm`/`vmbench` should next be run with an
allocation-tier design, not more heap bookkeeping.

### 8.13 Shared linked boot metadata: `Vm::restore` cost, W64-A / T-43 (2026-11-11)

Target: cut `Vm::restore` (the framework ACOSSNAP restore of §8.10) and the
per-session class re-link by sharing the **immutable linked class metadata**
across VMs. Ownership: `crates/acos-vm/src/snapshot.rs`, `crates/acos-vm/src/vm.rs`
(class link/cache construction), `crates/acos-runtime/src/session_pool.rs`.

**What changed.** `Vm::snapshot()` now records, once per
`(boot-DEX fingerprint, corelib, order-sensitive class-descriptor hash)`, an
in-process [`BootImage`]: the already-linked `classes` (field layouts, absolute
slot offsets, vtables, interface default tables, `direct_map`/`virtual_map`),
`methods` (signatures, param types, `ins_size`/`ins_ref`, shared `Arc<MethodCode>`
bodies) and `types`, plus the provider list (`Arc<DexFile>` order, boot/lazy jar
lists, `c_*` ids). `Vm::restore` looks the image up **before** constructing a
VM; on a hit it builds a bare `Vm` (`Vm::empty` + native registry + provider
contexts) and clones the linked tables instead of re-parsing `class_data`,
re-formatting signatures, re-interning method string constants, rebuilding
vtables and re-decoding 255 DEX class bodies. Everything mutable stays per-VM:
`statics`, `Class.init`, `Class.class_obj`, the heap, interner, `dispatch_cache`,
IR quickening and the per-DEX resolution caches are re-seeded from the snapshot
(or reset), so no mutable state is shared. The cache is bounded (4 images) and
`#![forbid(unsafe_code)]` is untouched. A snapshot whose class set/order differs
from the image falls back to the old relink path and records its own image; the
**first** restore of a class set in a fresh process still pays that relink.

Measurements (worktree of HEAD `145c6eb`, `dev` profile,
`CARGO_TARGET_DIR=.cache/target-w64a`, `tools/bench-acos.sh --profile dev launch`,
20 samples, host 32 threads / 60 GiB, loadavg ≈ 5–6 during the runs, corelib
`real`, fixture `fixtures/build/apps/hello-views.apk`). "before" = the same
tree with the T-43 diff stashed (relink path); "after" = the diff applied.
Both rows are from the same two-run A/B, interleaved (before → after):

| row | before p50 / p95 (ms) | after p50 / p95 (ms) | change |
|---|---:|---:|---|
| `Vm::restore` (+`Runtime::with_vm`) | 54.006 / 57.982 | **12.832 / 13.757** | **4.21x / 4.21x** |
| after = restore + launch phases (`bindApplication` … `quitSafely`) | 210.462 / 218.174 | 172.658 / 178.162 | 1.22x / 1.22x |
| `launch` before arm (Vm::new + framework boot, same run) | 223.747 / 237.742 | 236.279 / 376.856 | host noise, not a target |
| template boot (paid once) | 80.356 | 92.686 | incl. image capture |
| template snapshot capture | 0.771 | 6.820 | +one `BootImage::capture` |
| snapshot size / classes | 221 324 B / 332 | 221 324 B / 332 | unchanged |

Run files: baseline `results/bench/run-2026-11-11-003022.json` (HEAD, relink),
after `results/bench/run-2026-11-11-003100.json` (HEAD + T-43). The restore
phase split moved from `vm_new 5.4 + relink 41.0 + heap 1.6 + tables 0.7 +
cache-clear 1.9` to `bare Vm + image clone ≈ 8.6 + heap 1.6 + tables 0.7`
(image-path provider caches start empty, so the clear pass is skipped).

**Targets.** `restore < 15 ms`: **met** (12.8 ms p50 / 13.8 ms p95). Launch
total p50 < 60 ms: **not met** (172.7 ms). The restore half is now ~7 % of the
total; the remaining 159.2 ms is the launch chain outside this change
(`bindApplication` → `Application.onCreate` → `ClientTransaction` →
`quitSafely`), which in this fixture still ends in the known pre-existing
`uncaught java.lang.ExceptionInInitializerError` blocker (§8.10;
`app_created = 0`, `transaction_delivered = 0` — evidence is the real
framework work, not a stubbed pass). That arm is owned by the runtime/launch
waves, not T-43.

**SessionPool memory** (`acos bench sessions-pool --sessions 5,16 --runs 3`,
fresh child per N, `anon+shmem` p50, real corelib, `02_int_arith`):

| metric | before | after |
|---|---:|---:|
| N=5 total anon+shmem (MiB) | 45.078 | **40.148** |
| N=16 total anon+shmem (MiB) | 82.121 | **65.023** |
| marginal MiB/session (N=5→16) | 3.368 | **2.261** |
| session create p50 (ms, N=5 / N=16) | 6.535 / 8.999 | **2.590 / 4.154** |
| per-session reset p50 (ms, N=5 / N=16) | 7.930 / 8.189 | **3.801 / 3.899** |

Marginal RSS is **better**, not worse: each session clones the shared linked
tables rather than re-deriving them, and the one process-global image is
bounded.

**Correctness.**
* `tools/diff-dexprogs.sh --corelib real`: **27 pass / 0 fail / 0 unsupported**
  (`results/vm/dexprogs-w64a-real-normal.json`); `--gc-stress`: **27 / 0 / 0**
  (`results/vm/dexprogs-w64a-real-gc-stress.json`).
* `cargo test -p acos-vm --test snapshot`: **16 passed** (determinism,
  idempotence, 100-cycle stability, mismatch refusal, real regex);
  `cargo test -p acos-runtime --test session_pool`: **4 passed** (reset
  isolation, panic isolation, deterministic across worker counts, wall guard).
* `cargo clippy -p acos-vm -p acos-dex --lib --no-deps -- -D warnings`: clean.
  (A workspace-wide `cargo clippy --all-targets -D warnings` still fails on a
  pre-existing `acos-framework-natives` lint at HEAD, unrelated to this diff.)
* `snapshot_probe --corelib real --dex 09_strings --iters 5`: outputs
  **5/5 identical** to the fresh path, `determinism_byte_equal` and
  `idempotence_byte_equal` true; 40-class restore p50 2.34 ms (from 5.8 ms).

**Verdict / what remains.** Sharing the linked metadata is a 4.2x restore win
and removes the per-session relink the §8.10 analysis named as dominant, with
lower marginal RSS. Still open: (a) a cold process (`Vm::restore` without a
prior in-process template/snapshot) pays the ~54 ms relink — a content-addressed
on-disk `.cache/` image (validated by the same fingerprint/class-list key,
atomically written) is the next step and is **not** implemented or claimed
here; (b) the launch-total target needs the launch chain, not restore.

### 8.14 On-disk boot-image cache (T-43b / W96-A): cold-process framework bring-up (2026-11-11)

Target: let a fresh process skip the framework boot by loading the
framework-init `ACOSSNAP` (W45-A) from a content-addressed on-disk cache
(T-43b). Ownership: `crates/acos-runtime/src/boot_image_cache.rs` (new),
`crates/acos-runtime/src/session_pool.rs`, `crates/acos-vm/src/snapshot.rs`
(additive `boot_dir_fingerprint`). The cache key is FNV-1a over the
boot-classpath definition plus the path/size/mtime of every
`dex/<jar>/classes*.dex`, the ACOS package version and the cache format
version. Entries are validated on load by magic, format version, ACOS version,
key, payload length and a 64-bit FNV-1a payload checksum; a
short/oversized/corrupt entry is a **miss** that falls back to the real boot.
Writes are `tmp + fsync + rename` into a private `0700` dir (`ACOS_CACHE_DIR`
or `~/.cache/acos`), guarded by an `O_EXCL` lock, with size-bounded LRU
eviction. The crate is `#![forbid(unsafe_code)]`, so an entry is read into a
`Vec` under a 64 MiB cap (no `mmap`).

Method: clean worktree of HEAD `7360183`, `CARGO_TARGET_DIR=.cache/target-w96a`,
`CARGO_PROFILE_DEV_DEBUG=0`, `dev` profile, host 32 threads / 60 GiB, corelib
`real`, fixture `fixtures/build/apps/counter.apk`. 5 interleaved before/after
rounds, each round a fresh process running `acos bench launch --runs 1`:
**before** = `ACOS_BOOT_CACHE=0` (full `Vm::new` + `prepareMainLooper` +
`new ActivityThread` + `attach` + ACOSSNAP capture); **after** = warm
`ACOS_CACHE_DIR` (disk load + checksum + `Vm::restore`, which relinks the 338
classes and captures the in-process `BootImage`). Raw:
`results/bench/w96a-cache.json`.

| row | before p50 / max (ms) | after p50 / max (ms) | change |
|---|---:|---:|---|
| framework bring-up (`RuntimeTemplate::boot`) | **94.221 / 95.912** | **65.676 / 69.192** | **1.44x / 1.39x** |
| ACOSSNAP capture (before) / cache load+restore (after) | 6.698 / 6.903 | 65.402 / 68.901 | cache load pays the relink |
| per-launch `Vm::restore` (in-process image, after) | — | 10.931 / 12.458 | W64-A image path |
| `from_cache` | false (5/5) | true (5/5) | — |

**Verdict / what remains.** A cold process restores the framework image from
disk in ~65 ms instead of booting in ~94 ms (1.44x) and never relinks twice:
the restore's own `BootImage` capture warms the W64-A image, so the per-launch
restore is ~11 ms. This is **not** the in-process 12.8 ms of §8.13: the cold
restore still pays the ~54 ms class relink because the linked `BootImage`
itself is not yet serialized — a content-addressed on-disk *linked-class* image
is the next lever.

**`acos launch` is intentionally not wired to this cache yet.** Restoring the
post-attach ACOSSNAP in a fresh process leaves the Java `ActivityThread`'s
`BinderProxy` handles bound to the boot process's parcel registry, so the
launch regresses to `IllegalArgumentException: unknown binder handle 0x24` at
`application_on_create` instead of reaching the current `client_transaction`
blocker. Re-seeding binder identity after restore is the remaining T-43 item;
until then the real launch path is unchanged. Control measurements (5 fresh
processes, `results/bench/w96a-launch.json`):

| row | value | legacy ACVM (§3) | verdict |
|---|---:|---|---|
| `acos launch counter.apk` wall | 5465 ms p50 | M3 cold → first frame 3.620 s | `not-comparable` (no renderer/frame) |
| `acos launch` peak RSS | 542 298 112 B (≈517 MiB) | — | control |
| `acos run-dex 02_int_arith --corelib real` | 451 ms off / 453 ms on | — | unchanged (cache unused) |

Correctness (worktree = HEAD `7360183` + this diff): `tools/diff-dexprogs.sh
--corelib real` **27 pass / 0 fail / 0 unsupported**
(`results/vm/dexprogs-real-normal.json`); `cargo test -p acos-vm --test
snapshot` **16 passed**; the new cache tests **10 unit + 3 integration passed**
(round trip, corrupt/truncated/oversized fall back, concurrent writers, key
invalidation on boot-dir content change, LRU eviction, private dir, warm
template hit, corrupt-entry repair); `cargo clippy -p acos-vm -p acos-runtime
-p acos-cli --lib --tests --no-deps -- -D warnings` clean; `cargo check
--workspace --all-targets` clean.

---

## 9. V1 measured benchmark (2026-11-11, W160-A)

This is the authoritative ACOS measurement table. Every number below was
produced by `tools/bench-v1.sh --rounds 3` with a release build at
`CARGO_TARGET_DIR=.cache/target-w160a`, under `tools/guarded.sh`; raw per-round
samples are in `results/bench/v1-2026-11-11-061354.json`. The binary was built
from **committed `20663b3` (W152-A) plus the uncommitted W160-A working-tree
patch** (source-recorded native regions), because W160-A is the change under
test and this wave does not commit. The script's `commit` field is `b467193`
(W158-A, HEAD when the run started); `main` advanced to `65856f2` (W159-A)
while the run was in flight. Rows without a measurement are `NOT_MEASURED`
with the reason; no estimate is present. **M3 now scores the default warm launch
path** (`launch_default`, boot cache on); W148-A had to fall back to
`ACOS_NO_BOOT_CACHE` because the restore path was broken, which W154-A fixed
and W160-A keeps correct.

> **Superseded for the V1 verdict by §9.8 (W176-A, committed HEAD `fd1a887`):
> 9 PASS / 2 FAIL (M5, M9), all 11 metrics measured.** The table below is the
> W160-A run and is history; the tap-fix rows (M8/M10/M11) are re-measured in
> §9.8.

**Method record**

```yaml
date: 2026-11-11T06:13+05:30
built_from: 20663b3 (W152-A) + W160-A working-tree patch (uncommitted)
commit_field: b46719379e92a7f60917915997be2f2e6ad6711f (W158-A, run start)
commit_at_run_head: 65856f2 (W159-A, advanced mid-run)
host: {kernel: 7.2.11-arch1-1, nproc: 32, mem_total: 61908 MiB,
       mem_available_at_start: 35830 MiB, loadavg1: 1.99, loadavg5: 2.25,
       uptime: other agents building/running concurrently (W150-A/W156-A/W159-A)}
profile: release
rounds: 3 (self-contained/new-capability rows: 5)
interleaving: every metric timed in one host window; Bench* vs `java -Xint`
              alternated per round in the same window
fidelity: native-runtime (ACOS executes the code); NOT ART-verified
memory_type: anon+shmem (RssAnon+RssShmem) unless the row says PSS;
             never summed with PSS or a cgroup value
report: min and median (p50); spread = (max-min)/median; p95 where n>=5
noise: rows with spread > 15 % are flagged `noisy` (host load, not code)
cache: ACOS_CACHE_DIR=results/tmp/w160a-bench-cache (private, fixed),
       ACOS_PROFILE_OUT fixed so the snapshot property_hash is stable
```

### 9.1 Owner directive D2 — every legacy-tracked metric

D2 (`SPEC.md`) makes every legacy metric an acceptance gate: ACOS must **beat**
the legacy ACVM value. Verdicts: `PASS` = same metric type/state and ACOS is
lower; `FAIL` = ACOS is worse, or the capability is missing; a missing
measurement is `NOT_MEASURED` and never a pass. Reproduce with
`tools/bench-v1.sh --rounds 3`.

| # | Metric | ACOS (measured) | Legacy ACVM (§3) | Ratio | Verdict (D2) | Plan / caveat |
|---|---|---|---|---|---|---|
| M1 | Fixed idle core memory (anon+shmem, booted framework) | **133.86 MiB** (Vm-only 21.72; PSS 142.45) | 206.0 MiB (W1); 303.9 MiB (v9) | **0.650** | **PASS** | 33.9 MiB above the 100–150 MiB research target; not a D2 failure |
| M2 | Incremental memory per session at N=5 (anon+shmem) | **39.47 MiB/session** (N=1 133.86, N=2 74.90, N=10 27.63) | 78.7 MiB (v9); 33.2 MiB (W2 opt-in) | **0.502** | **PASS** (vs v9; still loses to W2 opt-in) | Close the W2 gap with the zygote-like template (`results/bench/MEMORY_PROFILE.md` §5) |
| M3 | Cold start → first correct frame | **0.8062 s p50** framework-bound `acos launch counter.apk` → `first_frame`, **DEFAULT warm path** (boot cache on, 5/5; min 0.7978, max 0.8303). Cold control (cache off): 1.0731 s p50, 5/5 | 3.620 s (v12) | **0.223** | **PASS** (default path) | ⚠ The launched frame is **not** ART-scored. The old boot-image restore bug is fixed (W154-A/W160-A). |
| M4 | Worker create → correct frame | 1.19 ms SessionPool restore (N=5) — no frame | 1.251–1.343 s p50 | — | **FAIL** | Add a worker-create path that owns a frame; pool restore is a quiescent VM |
| M5 | Fresh screenshot | **50.08 ms p50** daemon-session completed-frame retrieval (min 49.23, max 51.75; renderer-only warm encode 8.25 ms) | 0.066–0.067 s p50 | **0.753** | **PASS** | Process stdout is wall-timed and includes PNG encode+write; the value is not a pure retrieval |
| M6 | Reset-to-ready | 2.26 ms SessionPool reset-to-clean (N=5) — no app, no frame | 110.3 ms cmd / 748.1 ms → frame p50 | — | **FAIL** | Implement app-scoped reset → first frame; pool reset drops the VM only |
| M7 | UI tree with text | **49.81 ms p50** live View tree from a kept daemon session (min 28.01, p95 50.4) | one-shot 538–768 ms; persistent 4.1 ms; structure 14–87 ms | **0.093** | **PASS** (vs one-shot) | Loses to the legacy *persistent* 4.1 ms; tree text is not yet diffed against `ui_initial.xml` |
| M8 | Action-to-state latency | `acos tap --text Increment` 49.87 ms p50, `accepted: true`, **`state_changed: false`** | not cleanly published | — | **FAIL** | Tap is acked but does not land; W144-A/W152-A leash work pending |
| M9 | Concurrent workloads | N=20 pool 57.61 MiB total (create 3.24 ms, reset 4.10 ms); independent state/input **not** proven | N=5 scorecard, N=10 measured | — | **FAIL** | Per-session Binder transports, then assert independent state/input at N |
| M10 | End-to-end verify wall | not measured (`acos verify` stops at the first UI step) | ~19 s install-bound dev loop | — | **NOT_MEASURED** | Land launch/assert/evidence/reset/cleanup |
| M11 | Verified tasks / GiB-minute | not measured | not published | — | **NOT_MEASURED** | Run the T01–T12 suite with job-cgroup accounting |

**Result against D2: 5 PASS / 4 FAIL / 2 NOT_MEASURED among the 11 legacy-gated
metrics** (M1, M2, M3, M5, M7 pass; M4, M6, M8, M9 fail; M10, M11 not
measured). Counting a non-measurement as a non-pass gives **5 PASS / 6
not-pass**. The M3 caveat from the previous run is closed: the **default**
`acos launch` with a warm cache now reaches `first_frame` 5/5, at 0.8062 s p50
(vs the 1.0731 s p50 cold control).

### 9.2 ACOS-native rows (no legacy baseline)

Not legacy-tracked, so D2 does not gate them; the honest current performance of
the paths ACOS has. min / p50 over the stated n.

| Metric | min | p50 | p95 | Type / state | Evidence |
|---|---:|---:|---:|---|---|
| Cold process → `Vm` ready | 19.87 ms | **21.83 ms** | 23.56 | wall, cold | `process-start` |
| Framework boot → `attachApplication` (warm boot cache) | 12.25 ms | **14.35 ms** | 18.00 | wall, warm | `boot-attach` |
| Launch snapshot restore (`Vm::restore`) | 43.50 ms | **45.36 ms** | 46.90 | wall, warm restore | `launch_restore` |
| Launch before snapshot (`Vm::new` + boot) | 607.3 ms | **612.2 ms** | 617.4 | wall, cold | `launch_before` |
| Launch after snapshot (restore + launch) | 382.4 ms | **405.0 ms** | 565.3 | wall, warm | `launch_after_total` |
| SessionPool create/session (N=1/5/20) | — | **1.01 / 1.19 / 3.24 ms** | — | wall, warm restore | `sessions-pool` |
| SessionPool create rate (N=5 / N=20) | — | 3 483 / 5 541 sessions/s | — | wall | `sessions-pool` |
| SessionPool reset-to-clean (N=5 / N=20) | — | **2.26 / 4.10 ms** | — | wall, warm | `sessions-pool` |
| SessionPool total anon+shmem (N=1/5/10/20) | — | 23.77 / 30.90 / 39.82 / **57.61 MiB** | — | anon+shmem | `sessions-pool` |
| Framework launch → `first_frame` (cache off) | 1 042.7 ms | **1 073.1 ms** | 1 080.1 | wall (process), cold | `fw.launch_nocache` |
| Framework launch → `first_frame` (cache on, default) | 797.8 ms | **806.2 ms** | 830.3 | wall, warm restore, 5/5 | `fw.launch_default` |
| Peak RSS, cold framework launch → `first_frame` | 647.1 | **648.0 MiB** | 648.7 | RSS (peak) | `fw.launch_nocache` |
| Peak RSS, default (cache-on) launch | 658.7 | **659.1 MiB** | 660.1 | RSS (peak) | `fw.launch_default` |
| Live View tree, kept daemon session | 28.0 ms | **49.8 ms** | 50.4 | wall, warm session | `fw.daemon.tree` |
| `acos tap --text` (kept daemon session) | 44.2 | **49.9 ms** | 55.0 | wall, warm; `state_changed=false` | `fw.daemon.tap` |
| `acos observe --apk --tree` (launch + tree) | 1 048.2 | **1 059.9 ms** | 1 075.8 | wall, cold | `fw.observe_tree_apk` |
| `acos observe --apk --screenshot` (launch + frame) | 1 134.1 | **1 140.3 ms** | 1 149.4 | wall, cold + PNG write | `fw.observe_screenshot_apk` |
| Renderer cold first frame (hello-views, standalone) | 2.22 ms | **2.22 ms** | — | wall, cold | `bench_render.helloColdMs` |
| Renderer warm re-render | 0.78 ms | **0.81 ms** | 1.14 | wall, warm | `bench_render.helloWarmMs` |
| PNG `FastFiltered` 1080×2400 | 8.25 ms | **8.25 ms** | — | wall, warm | `bench_render.encoding` |
| `acos observe --tree` (no session, not_ready path) | 1.16 ms | **1.50 ms** | 1.56 | wall, no tree `noisy` | `observe_tree_ms` |
| `acos frame-diff` 1080×2400 | 145.2 ms | **146.9 ms** | 160.7 | wall | `frame_diff_ms` |
| `acos inspect` hello-views APK | 1.23 ms | **1.61 ms** | 1.78 | wall, static `noisy` | `apk_inspect_ms` |
| MCP `tools/call acos_inspect` / bare CLI | — | **3 ms / 1 ms** p50 | 4 / 2 | wall, warm server | `bench-overhead.sh` |
| Daemon ping, reused connection | 0.0040 ms | **0.0049 ms** | 0.0112 | wall, warm | `daemon.reuse_ms` |
| `acos session list` process spawn through daemon | 49.94 ms | **50.13 ms** | 50.34 | wall, process spawn | `daemon.cli_process_ms` |
| Interpreter `02_int_arith` / `05_branches_loops` | 6.24 / 5.90 | **6.64 / 5.95 ms** | — | wall, cold | `interpreter` |

Noisy rows (spread > 15 %): `observe_tree_ms`, `apk_inspect_ms`. The live-tree
row is also spread-heavy (28.0–50.4 ms) because the first sample warms the tree
path.

### 9.3 VM throughput: `Bench*` vs `java -Xint` (3 interleaved rounds)

Both binaries are timed the same way (process start → exit, wall clock) in the
same window; `java` is OpenJDK 17 `-Xint`. `x` = ACOS median ÷ `java -Xint`
median (lower is better).

| Program | ACOS p50 | `-Xint` p50 | x | Program | ACOS p50 | `-Xint` p50 | x |
|---|---:|---:|---:|---|---:|---:|---:|
| `BenchArith` | 843.9 | 528.7 | 1.596 | `BenchStaticCall` | 3 100.6 | 2 936.1 | 1.056 |
| `BenchLoop` | 655.1 | 418.3 | 1.566 | `BenchVirtualCall` | 3 069.4 | 2 938.6 | 1.044 |
| `BenchArray` | 480.6 | 320.2 | 1.501 | `BenchVirtual` | 1 572.1 | 1 159.9 | 1.355 |
| `BenchFields` | 110.3 | 76.9 | 1.434 | `BenchAlloc` | 1 546.1 | 1 124.9 | 1.374 |
| `BenchFib` | 522.7 | 333.0 | 1.570 | `BenchGc` | 337.1 | 131.2 | **2.570** |
| `BenchString` | 128.0 | 624.3 | **0.205** | `BenchCollections` | 132.1 | 81.4 | 1.622 |

ACOS beats `java -Xint` only on `BenchString`; `BenchStaticCall` and
`BenchVirtualCall` are near parity. The largest gaps are allocation/GC-bound
(`BenchGc` 2.57×) and dispatch/loop (`BenchArith` 1.60×, `BenchLoop` 1.57×,
`BenchFib` 1.57×). The IR tier (`ACOS_EXEC=ir`) is still not default-on. These
`Bench*` medians are much lower than the §8/W148-A run because the host load at
the start of this window was lower (loadavg1 1.99 vs 4.07); the *ratios* are the
stable comparison.

### 9.4 Honest render provenance and SSIM

Two different numbers exist and must not be mixed (W94-A #12, W101-A):

| Source | Provenance | SSIM vs ART | What it measures |
|---|---|---|---|
| `results/render/corpus/*` (canvas-shapes, counter, …) | **hand-authored** reconstruction of the ART UI dump | canvas-shapes **0.9953**, counter **0.9944** | how well the scene was authored — **not** a VM-drawn frame |
| `results/render/corpus/corpus-pipeline-metrics.json` | **pipeline-generated** (real `View.measure/layout/draw` → `RecordingCanvas` → `HardwareRenderer` → `acos-render`) | counter global **0.0143** / masked **0.9407**; canvas-shapes global **0.2612** / masked **0.8907** | the real draw pipeline, fair but lower |
| `results/render/hello-views.png` vs ART (`m3-hello-views-vs-art.json`) | standalone renderer frame | global **0.9941**, mean **0.9938** | renderer rasterisation on the hello-views scene |
| `acos launch counter.apk` → `first_frame` | real launched Activity, `ThreadedRenderer.draw` → `FrameStore` (W133-A) | **not yet scored** | a real frame exists; no ART-diff has been recorded for it |

Flutter: `FlutterSession` reaches a first frame and a successful tap (masked
SSIM **0.9987 / 0.9968**, W130-A) through the test harness; the confined-child
product routing is partial (W142-A).

### 9.5 Worse-than-legacy / gap list (explicit, with plan)

1. **[CLOSED] Boot-image restore for launch.** W154-A (embed regions) and
   W160-A (source-recorded regions) fixed the ICU `DatPackageReader` warm-restore
   crash. `acos launch counter.apk` now reaches `first_frame` **5/5** on the
   default warm path at 0.8062 s p50; the cache entry is back to 25.58 MB.
   Remaining caveat: the launched frame is still not ART-scored.
2. **[CLOSED] M5 screenshot routing.** The kept-session `observe --screenshot`
   path now returns a `CompletedFrame`; the daemon-session retrieval is
   50.08 ms p50 (`status=pass`, stage `first_frame`), which beats the 66.5 ms
   legacy value. The number includes PNG encode+write.
3. **M8 tap does not land.** Input is accepted (`accepted=true`) but
   `state_changed=false` and the counter stays `0`. Plan: land the W144-A/W152-A
   leash fix (`crates/acos-vm/src/natives/fwk_input.rs`).
4. **M9 is a memory experiment.** The process-global Binder transport means N
   pool sessions are not independent workers.
5. **VM throughput is behind `java -Xint` on 11/12 programs.** Plan: make the IR
   tier default-on for loop bodies, add an allocation cap for `BenchGc`/`BenchAlloc`.

### 9.6 What this run changed in the record

- The V1 gate moved from **4 PASS / 7 not-pass** (W148-A) to **5 PASS / 6
  not-pass**: M3 still passes and now measures the **default warm path** (0.8062 s
  p50, 5/5) instead of the cold fallback, and M5 (daemon-session screenshot)
  is now measured and passes (50.08 ms p50). M1/M2 hold at 133.86 MiB /
  39.47 MiB per session.
- The launch snapshot restore metrics are now measured (`Vm::restore` 45.36 ms
  p50; after-snapshot launch 405.0 ms p50) — W148-A could not measure them.
- The W154-A/W160-A fix closes the top runtime bug (warm restore) and removes
  the 28.4 MB embedded ICU region from the boot cache. See
  `results/perf/BOOTIMAGE_CACHE.md` §W160-A.
- `tools/bench-v1.sh` now takes the M3 verdict from `launch_default`; the cold
  control is reported in the row note.
- `STATUS.md` and `tools/status-check.sh` are refreshed for this run.

**Reproduce:**

```sh
# release build from 20663b3 (W152-A) + the uncommitted W160-A patch
# (main advanced to b467193/65856f2 while the run was in flight)
CARGO_TARGET_DIR=$PWD/.cache/target-w160a CARGO_PROFILE_DEV_DEBUG=0 \
  CARGO_BUILD_JOBS=2 cargo build --release -p acos-cli -p acos-mcp \
  -p acos-vm --example vmbench -p acos-render --example bench_render
ACOS_BIN=$PWD/.cache/target-w160a/release/acos \
ACOS_MCP_BIN=$PWD/.cache/target-w160a/release/acos-mcp \
VMBENCH_BIN=$PWD/.cache/target-w160a/release/examples/vmbench \
RENDER_BIN=$PWD/.cache/target-w160a/release/examples/bench_render \
ACOS_CACHE_DIR=$PWD/results/tmp/w160a-bench-cache \
ACOS_PROFILE_OUT=$PWD/results/tmp/w160a-profile.json \
  tools/bench-v1.sh --rounds 3
```

The run writes `results/bench/v1-2026-11-11-061354.json`; the printed table is
the one in §9.1.

---

### 9.7 M4/M6/M9/M10/M11: legacy methodology and the ACOS measurement (W165-A)

The V1 gate has to compare like with like, so this section states exactly how
the frozen ACVM measured each remaining non-pass and how ACOS measures it now.
Every ACOS number here comes from the same `tools/bench-v1.sh` run as §9.1.

#### Legacy method (frozen ACVM; reference only)

| # | Legacy metric | How legacy measured it | Source |
|---|---|---|---|
| M4 | Worker create → correct frame | Warm core. Issue a *create worker* request (a new session on the already-booted core, not a core boot) and time to the first **fresh, non-black** frame, p50 over n=20 (P5b) / n=30 (corrected P1.8); the reported value is create → frame, not the create command alone. | `EXPERIMENTS.md` §21/§23; `docs/reference/ACVM_INVENTORY.md` |
| M6 | Reset-to-ready | Warm core with a live app. `reset --app` command latency (110.3 ms p50, n=12) **and** the time to the first fresh non-black frame after it (748.1 ms p50, n=12); the D2 row compares the `→ frame` figure. Scope is `--app` (the app's user state), not the whole user. | `iter/D2.md`; `ACVM_INVENTORY.md` |
| M9 | Concurrent workloads | N=5 scorecard plus 10 additional public virtual displays created with 0 failures; the target was to "independently demonstrate N=10". Memory is cgroup workset; each session is an independent guest with its own display and input. | `EXPERIMENTS.md` §E3.2/§P1.5; `docs/handoff/04` |
| M10 | End-to-end verify wall | The U2 dev loop: build → install → launch → assert → evidence → reset → cleanup, wall-timed; the ~19 s figure is install-bound. | `docs/handoff/04`; §3 |
| M11 | Verified tasks / GiB-minute | Not published. Defined as correctly completed predeclared tasks ÷ ∫(job memory GiB × minutes), with 0 false passes; requires the T01–T12 suite and job-cgroup accounting. | BENCHMARKS.md §1/§2 |

#### ACOS measurement (this run)

* **M4** — `acos bench worker-create-frame` (implemented in
  `crates/acos-cli/src/bench.rs`). One `RuntimeTemplate` is booted (or loaded
  from the warm on-disk boot cache) **outside** the timed region; each sample is
  `Vm::restore` → counter.apk `bindApplication` → `Activity.onCreate` →
  `ThreadedRenderer.draw` into the process `FrameStore`. A sample counts only
  when `FrameStore::published_count` advanced, so a quiescent restore is never a
  frame. The reported p50 is create → fresh frame.
* **M6** — the `reset` half of the same metric: after a live app, drop the
  `Runtime`, restore the framework template and relaunch to a fresh frame. This
  is a **template reset**, not an in-place app-state reset; the process-global
  Binder transport is replaced. The comparison value is the legacy
  `reset --app → frame` 748.1 ms.
* **M9** — `tools/bench-v1.sh` runs N=5/10/20 **separate OS processes**, each
  `acos observe --apk counter --tree` (launch + live tree), concurrently. Wall
  is parent-measured; per-session memory is each child's own `/proc/<pid>/status`
  `VmHWM` sampled while it is alive, reported as RSS (not PSS, not summed with a
  cgroup value). Independence at the *input* level (tap in one session not
  altering another) is **not** claimed while M8 tap does not land, and the
  in-process `SessionPool` remains a memory experiment because of the shared
  Binder transport; the row therefore stays FAIL even when every process reaches
  `first_frame`.
* **M10** — a scripted CLI loop on one kept daemon session:
  `launch --keep` → `observe --tree` → `observe --screenshot A` →
  `tap --text Increment` → `observe --tree` → `observe --screenshot B` →
  `frame-diff A B`. The wall is the whole loop; the frame diff is the assert
  evidence. The wall is under the legacy install-bound value, but the assert
  fails (the tap is acked and does not change state), so the row is not a pass.
* **M11** — derived in the harness as `60 / (M10 wall seconds × M2 GiB per
  session)`; this is a *bound*, not a verified rate. `verified_tasks` is 0 until
  the M10 assert passes, so no verified rate is claimed.

The M4/M6 verdicts are taken from a run only when `frames_published == samples`;
otherwise the row is FAIL/NOT_MEASURED with the blocker recorded. No row is
promoted on a measurement type, engine state or scope the data does not support.

> **W176-A update (§9.8):** the M8 tap now lands (`accepted=true` **and**
> `state_changed=true`), M10's assert passes (frame `content_hash` changes and
> the same-session before/after PNGs differ: `differing_pixels=448`,
> `changed_bbox=[44,202,301,364]`), and M11 reports one verified task at a
> measured **937.3 tasks/GiB-min**. M9 is re-measured with PSS in §9.8.

---

### 9.8 V1 rerun at committed HEAD `fd1a887` (2026-11-11, W176-A)

At committed HEAD the tap fix landed (W161-A) and the post-tap repaint
invalidation was added (W167-A), so the M8/M10/M11 rows were re-measured. The
build is a clean worktree of committed HEAD, with one recorded exception below.

**Build-record exception (honesty):** committed HEAD `fd1a887` does **not**
build standalone: `crates/acos-mcp/src/session_backend.rs` (committed) calls
`acos_runtime::private_root::ensure_private_root`, but
`crates/acos-runtime/src/private_root.rs` is an **untracked W172-A in-flight
file** and the `pub mod private_root;` declaration in
`crates/acos-runtime/src/lib.rs` was **uncommitted**. The release build below
was made from HEAD plus exactly those two W172-A pieces (private_root.rs
sha256 `12f4a0c0841563d6888725144b5c23bf5f9d3778b2a4c8179039b057b9f94ba4`, and
one `pub mod private_root;` line). No other uncommitted patch is in the binary.
HEAD stayed `fd1a887` for the whole run; no other agent's work was committed
during it.

**Method record**

```yaml
date: 2026-11-11T07:15+05:30
built_from: fd1a887 (committed HEAD, W165-A) + W172-A minimal uncommitted pieces
           (private_root.rs + one lib.rs module declaration) — see exception
host: {kernel: 7.2.11-arch1-1, nproc: 32, mem_total: 61908 MiB,
       mem_available_at_start: 36294 MiB, loadavg1: 0.86, loadavg5: 2.14,
       uptime: other agents active (builds/clippy) but load lower than §9}
profile: release
rounds: 3 (self-contained/new-capability rows: 5)
interleaving: every metric timed in one host window; Bench* vs `java -Xint`
              alternated per round in the same window
fidelity: native-runtime (ACOS executes the code); NOT ART-verified
memory_type: anon+shmem (RssAnon+RssShmem) unless the row says PSS;
             PSS divides shared pages; never summed with PSS or a cgroup value
report: min and median (p50); spread = (max-min)/median; p95 where n>=5
noise: rows with spread > 15 % are flagged `noisy`
cache: ACOS_CACHE_DIR=results/tmp/w176a-bench-cache (private, fixed),
       ACOS_PROFILE_OUT fixed so the snapshot property_hash is stable
raw: results/bench/v1-2026-11-11-071510.json (copy: v1-w176a.json)
```

#### 9.8.1 Legacy-gated table (D2)

| # | Metric | ACOS (measured) | Legacy ACVM | Ratio | Verdict | Plan / caveat |
|---|---|---|---|---|---|---|
| M1 | Fixed idle core memory (anon+shmem, booted framework) | **133.90 MiB** (Vm-only 21.74; PSS 141.81) | 206.0 MiB (W1) | **0.650** | **PASS** | unchanged vs §9.1 |
| M2 | Incremental memory per session at N=5 (anon+shmem) | **39.40 MiB/session** (N=1 133.89, N=2 74.86, N=10 27.57; marginal ~15.8) | 78.7 MiB (v9) | **0.501** | **PASS** | still loses to W2 opt-in 33.2 |
| M3 | Cold start → first correct frame | **0.9368 s p50** `acos launch counter.apk` → `first_frame`, default warm path, 5/5 | 3.620 s (v12) | **0.259** | **PASS** | slower than §9.1's 0.8062 s under host load; frame still not ART-scored |
| M4 | Worker create → correct frame | **0.7188 s p50** (3/3 fresh frames) | 1.297 s p50 | **0.554** | **PASS** | frame not ART-scored |
| M5 | Fresh screenshot | **99.35 ms p50** daemon-session completed-frame retrieval (raw 98.9–120.7, spread 22 % `noisy`) | 0.0665 s p50 | **1.494** | **FAIL** | **regressed from §9.1's 50.08 ms at HEAD**; the daemon path now writes the 10.37 MB PNG (verified) and is ~2× the legacy value. The renderer-only warm encode is 8.19 ms |
| M6 | Reset-to-ready | **714.7 ms p50** (3/3 fresh frames) | 748.1 ms → frame p50 | **0.955** | **PASS** | template reset, not in-place app state |
| M7 | UI tree with text | **49.97 ms p50** live View tree from a kept daemon session | 538–768 ms one-shot | **0.093** | **PASS** | loses to legacy persistent 4.1 ms |
| M8 | Action-to-state latency | **249.2 ms p50** `tap --text Increment`, `accepted=true` **`state_changed=true`**; post-tap tree 6.9 ms; frame `content_hash` changes | not cleanly published | — | **PASS** | capability pass, not a D2 ratio; the tap now waits for the repaint |
| M9 | Concurrent workloads | **528.2 MiB peak PSS/session** at N=20 (Pss_Anon 525.9 + Pss_File 3.5); RSS 640.0 (RssAnon 525.9 + RssFile 113.9); N=20 20/20 frames+trees, wall 2.01 s | N=5 scorecard, N=10 measured | — | **FAIL** | input-level independence still not proven; memory is now PSS-honest (see §9.8.2) |
| M10 | End-to-end verify wall | **1.664 s** launch→tree→shot→tap→tree→shot→frame-diff on one kept session; assert passes (hash `b6759…`→`f79465…`; PNG diff `differing_pixels=448`) | ~19 s install-bound | **0.088** | **PASS** | single-task assert, not the T01–T12 suite |
| M11 | Verified tasks / GiB-minute | **937.3 tasks/GiB-min**, `verified_tasks=1` (60 / (M10 wall s × M2 GiB/session)) | not published | — | **PASS** | one verified task; uses the in-process M2 memory, not the M9 PSS |

**Result against D2: 9 PASS / 2 FAIL / 0 NOT_MEASURED among the 11 legacy-gated
metrics** (M5 and M9 fail; were 7 PASS / 4 FAIL at W165-A). M5 is a measured
regression at HEAD and is not hidden.

#### 9.8.2 M9 memory: VmHWM is not mostly shared page cache

Each M9 child is a **separate OS process** running
`acos observe --apk counter --tree` (launch + live tree). At N=20, sampled while
alive from `/proc/<pid>/smaps_rollup` + `/proc/<pid>/status`:

| Per session (N=20, p50) | MiB | What it is |
|---|---:|---|
| `RssAnon` | 525.93 | private anonymous (touched heap/arenas) |
| `RssFile` | 113.90 | file-backed pages resident in this process |
| `Rss` (≈ `VmHWM`) | 640.04 | peak resident set |
| `Pss_Anon` | 525.93 | private anon share (≈ `RssAnon`; not shared) |
| `Pss_File` | 3.48 | **shared** file-backed share after dividing by sharers |
| `Pss` | **528.20** | honest per-session memory |

Findings:

* The ~114 MiB of file-backed RSS is **shared page cache** (chiefly
  `framework-res.apk`, mapped several times per process, plus the `acos`
  binary and libc). PSS divides it to **3.5 MiB/session at N=20** (6.3 MiB at
  N=5). Reporting `VmHWM`/RSS as per-session memory therefore overstates it by
  ~112 MiB at N=20.
* But the peak is **not** mostly shared: 525.9 of 640.0 MiB (82 %) is private
  anon. The `VmHWM` number is not an artifact of shared mappings; the process
  really touches ~526 MiB of private heap/arenas at its peak.
* The peak is a **transient**: the M1 booted-framework idle footprint is
  133.90 MiB anon+shmem / **141.81 MiB PSS** at the same HEAD. The per-session
  in-process marginal is M2 **15.8 MiB** (N=5→6), i.e. a separate process pays
  the whole framework boot anew. That, not double-counted sharing, is the gap
  to M2's 39.4 MiB N=5 incremental.
* Cheap-reduction candidates checked against the measurement: the boot cache is
  **25.6 MB** read into anon (`fs::read`/`read_to_end`; an mmap read-only would
  move it to shared file-backed, ≤25.6 MB), DEX raw bytes are ~50 MB retained
  (`results/bench/MEMORY_PROFILE.md` §1), and the launch snapshot is only
  **3.64 MB**. All are ≤5 % of the 526 MiB peak. The dominant peak is a handful
  of large transient allocator arenas (a SIGSTOP `smaps` at ~400 MiB showed
  120.7 / 63.8 / 63.6 / 62.1 / 27.1 MiB `[anon]` regions) reclaimed before
  idle; no cheap per-allocation reduction was found. The structural fix remains
  the zygote-like template (`MEMORY_PROFILE.md` §5).

#### 9.8.3 ACOS-native rows (from the same run)

| Metric | p50 | Type / state |
|---|---:|---|
| Cold process → `Vm` ready | 16.37 ms | wall, cold |
| Framework boot → `attachApplication` (warm cache) | 11.88 ms | wall, warm |
| Launch snapshot restore (`Vm::restore`) | 50.77 ms | wall, warm restore |
| Launch before snapshot (`Vm::new` + boot) | 629.3 ms | wall, cold |
| Launch after snapshot (restore + launch) | 528.9 ms | wall, warm (spread high) |
| SessionPool create / reset (N=5) | 1.22 / 2.32 ms | wall, warm restore |
| SessionPool total anon+shmem (N=20) | 58.03 MiB | anon+shmem |
| Renderer cold first frame (hello-views) | 2.147 ms | wall, cold |
| Renderer warm re-render | 0.820 ms | wall, warm |
| PNG `FastFiltered` 1080×2400 | 7.37 ms | wall, warm |
| `frame-diff` 1080×2400 | 145.2 ms | wall |
| `acos inspect` hello-views APK | 1.47 ms | wall, static |
| `observe --tree` (no session, not_ready) | 1.72 ms | wall, `noisy` (14.7 %) |

All ACOS-native rows are `native-runtime`; none is ART-verified.

**Reproduce** (from a clean worktree of `fd1a887`, after adding the W172-A
`private_root.rs` + `pub mod private_root;` needed to compile):

```sh
CARGO_TARGET_DIR=$PWD/.cache/target-w176a CARGO_PROFILE_DEV_DEBUG=0 \
  CARGO_BUILD_JOBS=2 tools/guarded.sh --mem 12G --time 3600 -- \
  cargo build --release -p acos-cli -p acos-mcp --bins
CARGO_TARGET_DIR=$PWD/.cache/target-w176a CARGO_PROFILE_DEV_DEBUG=0 \
  CARGO_BUILD_JOBS=2 tools/guarded.sh --mem 12G --time 3600 -- \
  cargo build --release -p acos-vm --example vmbench -p acos-render --example bench_render
ACOS_BIN=$PWD/.cache/target-w176a/release/acos \
ACOS_MCP_BIN=$PWD/.cache/target-w176a/release/acos-mcp \
VMBENCH_BIN=$PWD/.cache/target-w176a/release/examples/vmbench \
RENDER_BIN=$PWD/.cache/target-w176a/release/examples/bench_render \
ACOS_CACHE_DIR=$PWD/results/tmp/w176a-bench-cache \
ACOS_PROFILE_OUT=$PWD/results/tmp/w176a-profile.json \
  tools/guarded.sh --mem 40G --time 3600 -- tools/bench-v1.sh --rounds 3
```

This writes `results/bench/v1-2026-11-11-071510.json`; the table above is
§9.8.1.

---

### 9.9 Screenshot daemon latency + concurrent-launch memory (W183-A)

The two §9.8 FAILs are re-examined with a **clean worktree of committed HEAD
`4f5b3fd` plus only the W183-A patch** (no other wave's in-flight file). M5 is
fixed; M9 is measured and its allocator levers are quantified and rejected.

**Method.** `CARGO_TARGET_DIR=.cache/target-w183a`,
`CARGO_PROFILE_DEV_DEBUG=0`, `CARGO_BUILD_JOBS=2`, release build under
`tools/guarded.sh`. M5 uses the exact `dm["screenshot"]` method of
`tools/bench-v1.sh` §13: one daemon (`ACOS_NO_BOOT_CACHE=1`, private socket),
`launch counter --keep`, then each sample is a **separate**
`acos observe --screenshot <path> --session s_cli --json` process timed wall;
n=25. The A/B ran in one host window: old = `.cache/target-w176a/release/acos`
(the §9.8 binary, `fd1a887` + W172-A), new = W183-A. M9 uses the
`concurrent_observe` method: N separate `acos observe --apk counter --tree`
processes, per-PID peak PSS from `/proc/<pid>/smaps_rollup` (peak of 20 ms
samples), p50.

#### 9.9.1 M5 — fresh screenshot: 100.11 → 25.61 ms p50 (PASS)

| M5, same method | p50 | min | max | n |
|---|---:|---:|---:|---:|
| §9.8 at HEAD (old binary, same window) | 100.11 ms | 98.17 | 123.58 | 25 |
| **W183-A** | **25.61 ms** | 23.69 | 29.71 | 25 |
| Legacy ACVM (§3) | 66.5 ms | — | — | — |
| Ratio vs legacy | **0.385** | | | |
| Verdict (D2) | **PASS** | | | |

A bare daemon `observe` (no `--screenshot`) is p50 **1.81 ms** under W183-A vs
**50.03 ms** under the old binary; this isolates the two independent costs:

1. **~50 ms accept-poll floor.** `acos serve` used
   `listener.set_nonblocking(true)` and `thread::sleep(50 ms)` on `WouldBlock`,
   so every new CLI connection waited up to a full poll interval before it was
   accepted. Replaced with `libc::poll(POLLIN, 50 ms)`, which wakes on the
   connection; the idle-check cadence is unchanged. This floor is `319d51e`
   and is *the whole* of §9.1's `50.08 ms` row.
2. **~50 ms PNG encode.** The daemon screenshot path was only made real by
   W159-A (`65856f2`, "daemon screenshot honoured"); §9.1 predates it and never
   wrote the PNG. It encoded the 10.4 MB IDAT with a bit-by-bit `Crc32`
   (8 branchy iterations/byte) and a per-byte-modulo `Adler32`. Both are now the
   standard byte-at-a-time CRC-32 table and zlib's 5552-byte-batched Adler-32.
   The emitted PNG is **byte-for-byte identical** (10 371 258 bytes, sha256
   `4aa99c2b4320750fcd03eeb43641b62c202878bc2903aed430c8ecb4adf3926f`), so this
   is a pure compute fix.

The regression origin is therefore W159-A's daemon encode on top of the
pre-existing `319d51e` accept sleep — **not** W167-A's repaint and not noise.
The target (≤60 ms p50, ≥20 samples) is met: 25.61 ms p50, n=25.

#### 9.9.2 M9 — concurrent per-session peak PSS (no change; levers quantified)

No M9 code changed. Per-session peak PSS, separate processes:

| N | old binary p50 | W183-A p50 | Pss_Anon / Pss_File (W183-A) | frames+trees |
|---:|---:|---:|---|---|
| 5 | 530.38 MiB | 529.90 MiB | 525.37 / 4.57 | 5/5 |
| 20 | 527.16 MiB | 527.54 MiB | 525.37 / 2.24 | 20/20 |

Allocator A/B at N=5 (W183-A), against the launch wall of the same path:

| setting | peak PSS p50 | `observe --apk counter --tree` wall p50 |
|---|---:|---:|
| default | 529.90 MiB | 1035 ms |
| `MALLOC_ARENA_MAX=1` | 522.59 MiB (−7.3) | 994 ms |
| `MALLOC_ARENA_MAX=1 MALLOC_TRIM_THRESHOLD_=0` | **493.30 MiB (−36.6)** | **2324 ms (2.25×)** |

`M_ARENA_MAX` saves only ~7 MiB (1.4 %); `M_TRIM_THRESHOLD=0` saves ~37 MiB
but costs 2.25× launch wall, so **neither is landed**. An `ACOS_PROFILE=1`
census of one launch (`observe --apk counter --tree`) reproduces W146-A: the
largest anonymous mappings are 133.8 / 79.9 / 67.1 / 66.8 / 65.1 / 56.5 / 42.5
/ 38.8 MiB and hold the named live buckets (`dex_raw` 66.2, `dex_parsed_tables`
24.0, `vm_class_meta` 17.6, `vm_dex_handle_caches` 16.3, `dex_decoded_strings`
16.2, `vm_heap_slots` 16.2 MiB). The peak is live bytes held in glibc arenas,
not fragmentation or dead buffers: `malloc_trim` after boot/first frame cannot
lower a recorded high-water mark; mmapping the 25.6 MB boot-cache file is
blocked by `#![forbid(unsafe_code)]` and the crate's documented trust model;
and the 66 MiB raw DEX is live shared data still needed for lazy method decode.
No safe, meaningful reduction was found in this window; the structural fix
remains the zygote-like template (`results/bench/MEMORY_PROFILE.md` §5).
M9 stays **FAIL** (input-level independence is still not proven), unchanged.

#### 9.9.3 Acceptance gates (clean worktree of `4f5b3fd` + W183-A only)

| gate | result |
|---|---|
| `cargo test -p acos-runtime` (boot cache, snapshot, session encode + new CRC/Adler vectors) | **36 unit + all integration suites pass, 0 failed** |
| `cargo test -p acos-cli` | all pass (serving/verify 2/2) **except** the pre-existing, fixture-gated `native_launch::wrong_abi_apk_is_rejected_before_any_execution`: the source emits `WRONG_ABI` while the test expects `UNSUPPORTED_ABI` for `rn-hello-arm64.apk`. This predates W183-A and touches no daemon/PNG path; it only runs when the gitignored `fixtures/build/apps/rn-hello-arm64.apk` exists (a fresh `git worktree add` does not copy it). **Corrected (E1, 2026-11-11):** the mismatch was the daemon RN backend's `WRONG_ABI`; the CLI gate and the daemon RN/Flutter backends now all report `UNSUPPORTED_ABI` (ADR-0018 E1 note). The W183-A row is kept as the historical result at that commit. |
| `cargo test -p acos-mcp` (incl. `e2e`) | **pass** after the same fixture placement + `target/release/acos` link |
| `cargo clippy -p acos-runtime -p acos-cli -p acos-mcp --all-targets --no-deps -- -D warnings` | **clean (exit 0)** |
| `cargo check --workspace --all-targets` | **pass (exit 0)** |
| `tools/diff-dexprogs.sh --corelib real` | **27/27 pass** |
| `tools/diff-dexprogs.sh --corelib real --gc-stress` | **27/27 pass** |

**Files:** `crates/acos-cli/src/serve.rs` (event-driven accept poll),
`crates/acos-runtime/src/session.rs` (CRC-32 table, Adler-32 batching, a
known-answer unit test). No other tracked file. The emitted PNG is unchanged.

---

## 10. W12 — per-session cost of the real interactive ART session (2026-11-11)

First measurement of **Option A's actual per-session cost** (stock `ViewRootImpl` /
`Choreographer` / BLAST / HWUI on the ACOS C1 + `IWindowSession`), on commit `da9e9aa`
with the W10 BLAST-pool workaround (`DEFAULT_MAX_ACQUIRED_BUFFERS = 24`, no env override).
Harness and full report: `results/art-runtime/W12.md`; raw run:
`results/bench/W12-session-memory.json`. One rep per (mode, N); host 61.9 GiB, 32 threads,
debug binaries, `date -u` 20:01:52–20:02:29.

Re-run (one command, ~40 s, holds the host ART lock, `guarded.sh --mem 14G`):

```bash
tools/art-runtime/bench-session.sh --n "1 10 20" --modes "full launch render" \
    --sample-ms 100 --out .cache/bench-session/w12-postw11
# re-derive tables without running ART: add --analyze .cache/bench-session/w12-…
```

Modes: `full` = `acos verify --engine art` on counter.apk (interactive: launch → live tree →
tap Increment → capture → reset → tap; the product path); `launch` = `acos-art launch
--until activity-resumed` (same window stack, no interaction); `render` = `acos-art
render-app` (real ART + framework `Activity` + stock libhwui, but the **non-windowed** path:
zeroed `ViewRootImpl` stand-in, no BLAST/C1/window session).

| cohort | N | verified | per-session total MiB (cgroup peak + launcher + bwrap) | per-session PSS MiB | Σ PSS MiB | Σ cgroup MiB | gralloc buffers/session | launch→frame ms | tap→fresh frame ms | host peak % |
|---|---:|---:|---:|---:|---:|---:|---|---:|---:|---:|
| full | 1 | 1/1 | **363.1** | 262.2 | 262.2 | 303.3 | 24 (218.4 MiB) | 535 | 1628 | 23.6 |
| full | 10 | **10/10** | 352.4 | 145.3 | 1452.0 | 2979.8 | 23–24 | 556 | 1714 | 31.5 |
| full | 20 | **20/20** | 348.1 | 141.3 | 2841.2 | 5899.2 | 23–24 | 759 | 1906 | 35.5 |
| launch | 1 | 1/1 | 109.4 | 206.0 | 206.0 | 97.8 | 2 (18.2 MiB) | – | – | 24.4 |
| launch | 10 | 10/10 | 101.5 | 89.3 | 891.1 | 945.6 | 2 | – | – | 25.2 |
| launch | 20 | 20/20 | 100.6 | 84.5 | 1693.4 | 1904.5 | 2 | – | – | 26.4 |
| render | 1 | 1/1 | 57.5 | 158.0 | 158.0 | 50.8 | 0 | – | – | 23.6 |
| render | 10 | 10/10 | 62.9 | 71.2 | 842.2 | 555.3 | 0 | – | – | 24.2 |
| render | 20 | 20/20 | 62.2 | 66.8 | 1434.2 | 1170.7 | 0 | – | – | 24.6 |

Headline readings:

* **Option A's window/surface stack costs +48.0 MiB PSS / +47.0 MiB guest cgroup per
  session** with a 2-buffer pool (`launch` − `render`, N=1); 18.2 MiB of that is the buffer
  bytes themselves. The `WR-window-surface-options.md` §3 estimate was **+27 MiB**, so the
  estimate is **low by ~21 MiB (1.8×)**. It stays far below the *estimated* +120–300 MiB of a
  stock SurfaceFlinger process (WR option C) — and no `libsurfaceflinger.so` /
  `librenderengine.so` exists in the sysroot we hold, so no SF size is claimed.
* **Today's product path costs 348–363 MiB per session**, 6.4× the small-pool window-stack
  cost, almost entirely because the W10 pool workaround allocates 24 × 1080×2209×4 B ≈
  218 MiB of memfd buffers whose touched pages are charged to the guest cgroup. W11's release
  fix should bring this to ≈150–165 MiB/session; re-measure, do not assume.
* **Metric-type rules matter here**: gralloc memfd pages are `mmap`ed only during
  `lock`/`unlock`, so they are **absent from every process' PSS** and only visible in the
  guest cgroup (`memory.stat shmem`) and host used. Per-session cost therefore uses the
  cgroup `memory.peak`; the PSS sums split shared sysroot pages instead. cgroup sums
  over-count sharing across sessions (Σ cgroup 2979.8 MiB vs Σ PSS 1452.0 MiB at N=10).
  Never add the two.
* Marginal per extra session (`Σ PSS` N=1→10 / N=10→20): **132.2 / 138.9 MiB** (share-aware,
  buffers excluded); (`Σ cgroup`) **297.4 / 291.9 MiB** (per-session bound, sharing counted
  per cgroup).
* Legacy ACVM reference (different metric type, not summed): per-session N=1 130.3 MiB
  anon+shmem, so ACOS-on-ART is ~2× today and ~0.85× expected after the pool fix, while
  running the real framework path.
* Correctness: 1/1, 10/10, 20/20 `verify` passes, three taps each with fresh frames,
  0 cgroup OOM kills, MemAvailable never below 44 GiB.

Caveats: one rep; host deltas contaminated by a concurrent agent (W11) and by ~9.5 MB/frame
PNG page cache; debug binaries; one-shot `launch`/`render` rows are 100 ms-sampled peaks;
`render-app` is a memory comparator, never verification evidence. `acos-art session` (the
older headless route used by `bench-n.sh`) exits rc=1 after `activity-resumed` at this commit
and could not be used as the comparator — recorded in `W12.md` §8.

---

## 11. Benchmark methodology (decided 2026-11-12)

**Status:** this section is the method that binds every future ACOS benchmark row. It is
implemented in `tools/benchlib.py` (with unit tests in `tools/tests/test-benchlib.py`),
`tools/art-runtime/bench-release.sh`, `tools/art-runtime/bench-session.sh`,
`tools/art-runtime/bench-n.sh`, `tools/run-matrix.sh` and
`results/bench/schema/matrix.schema.json`. (`tools/art-runtime/bench.sh` already builds and
runs release but is not yet stamped; it is outside the W29 file ownership.) It supersedes nothing numerically: **the
ART-vs-legacy comparison is not yet measured. §9.8 is the Rust VM reference** (`fidelity:
native-runtime ... NOT ART-verified`), and the numbers in §9.x are retained as Rust-VM
history only. Until an ART D2 table exists, no row in this document is an ART verdict.

### 11.1 Release-only builds and provenance

* Benchmarks that can feed a verdict are built **release only**:
  `cargo build --release -p acos-cli -p acos-art` via
  `tools/art-runtime/bench-release.sh` (it builds, stamps, and then runs a harness).
* Every result JSON written by the W29 harnesses stamps: `profile`, **full SHA-256 of each binary**, the
  `git status --porcelain` SHA-256 (dirty-tree fingerprint), `git rev-parse HEAD`, `rustc -V`,
  and the host gate block. `bench-session.sh` and `run-matrix.sh` refuse a debug build unless
  `--allow-debug` is given; an allowed debug run is written with
  `verdict_eligible: false` and the reason `debug build: NOT_VERDICT_ELIGIBLE`. A debug row is
  **never** a pass, a win, or a baseline: it is labelled non-eligible at row and file level.
* Profile is taken from the cargo target directory (`target/release` vs `target/debug`, also
  for a custom `CARGO_TARGET_DIR`). ELF debug sections are **not** a profile signal: this
  workspace sets `[profile.release] debug = 1`, so release binaries legitimately contain
  `.debug_info`.

### 11.2 Statistics, noise, spread gate and the pass rule

`stats(values)` reports `n, min, max, mean, p50 (median), p5, p95, spread, stdev, cv,
ci95_low, ci95_high` (percentile bootstrap of the median, 2000 resamples, fixed seed
20261112, so a re-analysis reproduces the same interval).

```
spread = (p95 - p5) / p50                      # fraction; > 0.15 => noisy
noise  = max(spread(acos), spread(legacy), 0.05)
PASS   iff same metric type AND same state
          AND acos n >= 3 and legacy n >= 3
          AND neither row noisy (spread <= 0.15)
          AND acos_p50 <= legacy_p50 * (1 - noise)      # lower is better
TIE    iff legacy_p50*(1-noise) < acos_p50 <= legacy_p50*(1+noise)   # NOT a pass
FAIL   iff acos_p50 > legacy_p50*(1+noise)
```

* A row with `spread > 15 %` is `NOISY` and **cannot pass**, whatever the p50 says (the M5
  PASS→FAIL flip at a 22 % spread is the motivating case). A single rep has no measured
  spread and is therefore `NOISY`/unmeasurable for verdict purposes.
* A **TIE is not a pass**; missing data is `NOT_MEASURED` (not a pass); insufficient samples
  are `INSUFFICIENT` (not a pass). Higher-is-better metrics (throughput) use the mirrored
  rule: `acos >= legacy * (1 + noise)`.
* Latency rows still require **≥20 samples** for p50/p95 per §2 rule 2; memory rows require
  **≥5 fresh-process reps**. The verdict function refuses fewer than 3 samples outright.

### 11.3 Interleaved A/B (ABBA)

`benchlib.abba(a, b, pairs=3)` runs `A B B A` per pair in one host window and returns both
arms' `stats()`. Fewer than 3 pairs is flagged `pairs_ok: false` and cannot be cited as a
verdict. Each callable may return its own sample (ms) or `None` to be wall-timed.

### 11.4 Host gate and contamination

Before a run and at each result stamp, `host_gate()` requires:

* `loadavg1 <= 4.0`,
* `MemAvailable >= 30 GiB`,
* no other holder of the host-wide `art.lock` (probed with a non-blocking `flock`).

A gate failure refuses the run (exit 4) or, if it happens mid-run, marks the row/result
`contaminated` and excludes it from verdicts. ART work stays serialized through
`tools/art-runtime/run-exclusive.sh --task <name> -- tools/guarded.sh --mem ... --`.
Host limits from `docs/OPERATING_DIRECTIVES.md` §5 still bind.

### 11.5 One result schema for the scenario matrix

The matrix runner is `tools/run-matrix.sh`; it writes
`results/bench/matrix-<UTC>.json` (validated before writing) plus a `.tsv` view. The schema is
`results/bench/schema/matrix.schema.json` and its validator is
`benchlib.validate_matrix()`. Each **row is one scenario run**, so a correctness run is also
a cost sample:

* identity: `run_id, utc, git, dirty_hash, profile, verdict_eligible, engine`;
* scenario: `scenario, scenario_sha256, apk_sha256, variant (good|wrong|negative), expected
  (pass|fail), verdict (pass|fail|error), false_pass, false_fail, negative`;
* failure: `error {code, class, retryable, hint, message, step_index}` with
  `class ∈ {assertion, app_crash, unsupported, harness, infra_timeout, host_guard}`;
  `unclassified` must be 0;
* cost: `wall_ms, guest_wall_ms, first_frame_ms, steps[{kind,ms,status}]`;
* memory: `mem {cgroup_peak_mib, cgroup_gib_min, anon_shmem_settled_mib, pss_mib, settle_s,
  metric_type, peak_type, pss_type, sample_count}`;
* host: `host {loadavg1, loadavg5, mem_available_mib, mem_available_end_mib, contaminated}`.

All `scenarios/negative/*.yaml` are included with `expected: fail`; `false_pass` is printed
first and a non-zero count exits 1. The file is invalid (written as `*.invalid.json`, exit 1)
if `false_pass > 0`, `unclassified > 0`, or a required field is missing.

### 11.6 Memory columns and the same-type rule

Every memory row states its type, state and settle time. The columns are reported side by
side and **never summed across types**:

1. **cgroup `memory.current`**, sampled every 100 ms inside the run's own systemd scope:
   **peak MiB** and **integral GiB·min** (trapezoid over the job window, `Σ bytes/2^30 ×
   Δt/60`).
2. **PSS** summed over the run's cgroup processes (secondary, share-aware account).
3. **cgroup `memory.stat` anon+shmem**, settled **60 s after first frame** — the **only**
   column comparable with the legacy ACVM §3 per-session value (130.3 MiB, warm, N=1).

`benchlib.compare_memory()` returns `NOT_COMPARABLE` (not-pass) when `metric_type`, `state`
or (for anon+shmem) `settle_s` differ. This is why MILESTONE-1's “171.4 / 155 vs 130.3” line
is not a comparison: it put cgroup peak + launcher + bwrap against legacy anon+shmem. Rows
that cannot reach the 60 s settle are reported with `anon_shmem_settled_mib: null` and render
`NOT_COMPARABLE`, never a ratio.

### 11.7 Cold/warm states and what is measured today

Three states are named in the schema: `first-run` (empty `ACOS_CACHE_DIR`), `cold-process`
(no ACOS process alive, caches warm) and `warm` (session/pool already up). Today's `acos
verify --engine art` path is **cold-process per task**; it exits before a 60 s settle, so the
legacy-comparable anon+shmem column is NOT_MEASURED for it and the legacy comparison is
NOT_COMPARABLE by construction. This is a limitation of the current session lifetime, not a
missing measurement to be guessed at.

### 11.8 Evidence of the method itself (smoke, not a verdict)

On 2026-11-12T01:07:23Z the pipeline ran end to end on a release build of the working tree
(profile `release`, `verdict_eligible: true`, binary sha256 recorded in the JSON):
`results/bench/matrix-20261112T010723Z.json` — two N=1 runs, `v2-art-counter` pass and
`scenarios/negative/missing-element.yaml` fail with `ELEMENT_NOT_FOUND` classified
`assertion`, `false_pass: 0`, `unclassified: 0`, schema-valid. It is a **pipeline smoke test,
not a performance result**; one run per row, no repetitions, no legacy comparison. Detail:
`results/art-runtime/W29.md`.

### 11.9 Run configuration and host disk (W48)

**ART heap configuration is a recorded variable, not a constant.** The six
`dalvik.vm.heap*` values ART's `AndroidRuntime` turns into `-Xms`/`-Xmx`/
`-XX:HeapGrowthLimit=`/`-XX:HeapMinFree=`/`-XX:HeapMaxFree=`/
`-XX:HeapTargetUtilization=` can be overridden at launch with
`ACOS_ART_HEAPSTARTSIZE`, `ACOS_ART_HEAPGROWTHLIMIT`, `ACOS_ART_HEAPSIZE`,
`ACOS_ART_HEAPMINFREE`, `ACOS_ART_HEAPMAXFREE`, `ACOS_ART_HEAPTARGETUTILIZATION`
(`<int>[kmg]`; the utilization a decimal in `(0,1)`), validated for format and for
`start ≤ growthlimit ≤ heapsize`. An invalid value is a structured
`INVALID_HEAP_CONFIG` before any guest starts — never a silent default — and the
resolved values are written into the session's records
(`<artifacts>/art-frames/session-exit.json` and `live-session.json` under `heap`,
plus `launch --json`/`session --json`), so every row can name the heap
configuration that produced it. Unset variables reproduce the stock table
exactly. A one-rep comparison of two configurations is **not** a memory result:
`resources.json`'s peak is the harness process's RSS (W41 §1.1: the guest is in a
sibling cgroup), so only an N=10 suite with per-job `memory.current` integrals can
attribute a memory effect to the heap. Detail: `results/art-runtime/W48.md` §1.

**The harnesses cannot fill the host disk.** On 2026-11-11/12 `bench-d2` kept
36 GB of per-rep artifacts and the filesystem reached 100 %. Both
`tools/art-runtime/bench-d2.sh` and `tools/art-runtime/bench-n.sh` now (a) refuse
to start when the output filesystem has less than `ACOS_BENCH_MIN_FREE_GB`
(default 40) free, exiting non-zero with a `HOST_DISK_FULL:` message, and (b) after
each rep's row is recorded, delete that rep's bulky artifacts (frames, live
frames, tree dumps, logs, PNGs, the seeded per-agent `/data`) while keeping the
row's evidence — `verdict.json`, `resources.json`, `steps/`, `repro.*`,
`summary.json`, the host record, and the two session records in `art-frames/`
(`session-exit.json`, which carries the heap configuration of §11.9, and
`live-session.json`). `ACOS_BENCH_KEEP_ARTIFACTS=1` disables the
pruning for diagnosis. Measured on a tiny run: `bench-d2 --quick` 950 MB kept vs
**1.4 MB pruned**; `bench-n art 1 --reps 1` 1.1 MB vs **208 KB**. Detail:
`results/art-runtime/W48.md` §2.


---

## 12. ART vs legacy ACVM (D2, measured 2026-11-12)

**Status:** the second full D2 table measured on the **real ART engine with release
binaries** (W39, 2026-11-12T03:00:16Z -> 03:05:03Z). It replaces the W33 table below
(kept as the previous run) and supersedes §9.8 for the ART verdict; §9.x stays Rust-VM
history and §11 remains the binding method. Raw record:
`results/bench/d2-20261112T030016Z-w39.json` (+ `.md`, every sample, per-rep host load and
cohort membership); harness `tools/art-runtime/bench-d2.sh` (uses `tools/benchlib.py`;
its M9 reason text was corrected in W39 because the W33 compiler-storm sentence no longer
applies).

**W52 update (M11 measured at N=10).** The M11 row is filled from
`results/bench/suite-W52-r4.json` (release, N=10, 3 rounds, 66 fresh scopes):
**M11 62.84 tasks/GiB·min, CI95 45.45–98.60, round spread 13.459 %, 11/11 good pass,
11/11 wrong fail with declared code, false_pass 0**. Its primary denominator is
anon+shmem job+guest; an independent host-level union integral agrees within noise
(4.495 %). The 24.4 bar is **not derivable from anon+shmem/warm legacy inputs**
(§12.1), so the row is **NOT_COMPARABLE**, not a D2 PASS. W45's N=1 row is 67.81.

**W57 update (M1, M2, M5, M6, M7 measured WARM on a held `acos serve` session).**
Those five rows were NOT_MEASURED/NOT_COMPARABLE in the table below only because
ACOS measured a cold-process one-shot `acos verify` while legacy measured a warm
held session. `acos serve` now holds a real ART session (W50), so they are
measured in the same state and type: **M1 PASS, M2 FAIL, M5 FAIL, M6 TIE, M7 PASS**
(§12.2, `results/bench/d2-warm-20261112T061838Z.json`). The cold-process cells for
M1, M2, M5, M6 and M7 in the table below are kept as history and are
**superseded** by §12.2.

**Method record**

```yaml
date: 2026-11-12T03:00:16Z -> 2026-11-12T03:05:03Z (one full run: 20 latency samples,
      5 memory reps for each of N=1/5/10/20, suite 3 expressible tasks)
engine: art                       # every row is `acos verify --engine art`
profile: release                  # debug/unknown refused before any guest starts (exit 5)
git_head: c183d34fd666f57743ed821a7a5cfb97c461cf33   # HEAD advanced after the run (other agents); the binary sha256 identifies the build
git_dirty: true, 119 files, git status --porcelain sha256 5fa7b3b0bc1cebf3
binaries: {acos: ab69923f0b800f32455107f1022e01bb82f4426c4f0d77b2c71e8f0e94eb0991,
           acos-art: c08214f8abe72a76c41f90fbc37da898fcbcbc6bc369408478d4337c383acd25}
apk: fixtures/build/apps/counter.apk sha256 e037aee210471283479f04339235e783c1d344aa560f82a22ae1b53ee6610366
scenario: scenarios/v2-art-counter.yaml sha256 cc270776f42796f8a64d9acd4bf507f86b478f34c59dacfba203f2c1e515e2b0
rustc: rustc 1.90.0 (1159e78c4 2025-09-14)
reps: 20 latency samples (16 usable; lat-010..013 failed, see below), 5 memory reps per N
lock: one ART run per run-exclusive.sh acquisition, never held across reps; other
      agents' W16/W38/W41 ART runs interleaved (queue waits 5-15 s), and four cohort
      reps plus all three suite samples crossed the host gate (loadavg1 > 4)
host: loadavg1 0.86 -> 3.76 during the run (other agents), MemAvailable 46.9-48.1 GiB,
      host CPU used ~21-22%; nothing was swapped out for the run
rules: spread=(p95-p5)/p50; >15% => NOISY, never PASS; noise=max(spread,5%); TIE is not
      a PASS; type/state mismatch => NOT_COMPARABLE; missing => NOT_MEASURED. Nothing
      estimated. A contaminated sample/rep is recorded and excluded from the verdict.
```

| # | Metric | ACOS-ART value | n | 95% CI (p50) | spread | state / type | Legacy ACVM | Legacy type / state | Same type+state | Verdict | Reason |
|---|---|---|---|---|---|---|---|---|---|---|---|
| M1 | Fixed idle core memory (booted framework, no interaction, 60 s settle) | not measured | 0 | n/a | n/a | warm / CGROUP_ANON_SHMEM | 206.00 MiB | CGROUP_ANON_SHMEM / warm | yes | **NOT_MEASURED** | ACOS row missing (NOT_MEASURED = not pass) NOT MEASURED: needs a held ART session that stays alive 60 s after first frame to settle cgroup memory.stat anon+shmem; `acos verify --engine art` exits after its assertion and this build has no held-session ART path (BENCHMARKS.md 11.7). Not estimated. |
| M2 | Per-session memory at N=5 (incremental and marginal) | 93.45 MiB/session | 5 | 92.23..95.19 | 2.9% | cold-process / CGROUP_ANON_SHMEM | 78.70 MiB | CGROUP_ANON_SHMEM / warm | no | **NOT_COMPARABLE** | state mismatch: cold-process vs warm ACOS value is cgroup memory.stat anon+shmem at process exit, NOT settled 60 s after first frame; legacy 78.7 MiB is warm/60 s. Same type name, different state/settle => NOT_COMPARABLE (benchlib.compare_memory rule). |
| M3 | Cold start -> first correct frame | 265.5 ms | 16 | 260.00..293.20 | 75.7% | cold / WALL_MS | 3620.00 ms | WALL_MS / cold | yes | **NOISY** | ACOS row noisy: spread 75.7% > 15% gate ACOS state is cold-process (no ACOS process alive, page cache warm), the same cold category as legacy. First frame = cumulative step ms through the first step with frame_seq>=1 plus the measured engine-timer overhead (resources.wall_ms - sum(step durations)); host exec/lock/guard startup is excluded. The scenario's count=0 assertion binds correctness at that frame. measured spread 75.7% > 15%: this row is noisy and cannot PASS |
| M4 | Worker create -> correct frame (warm core) | not measured | 0 | n/a | n/a | warm / WALL_MS | 1297.00 ms | WALL_MS / warm | yes | **NOT_MEASURED** | ACOS row missing (NOT_MEASURED = not pass) NOT MEASURED: requires a pre-booted warm ART guest pool (ADVISOR item 7); `acos verify --engine art` boots a fresh guest per task (ArtSession::start), so no warm create exists to time. Not estimated. |
| M5 | Fresh screenshot retrieval | 240.0 ms | 16 | 237.00..253.00 | 8.9% | cold-process / WALL_MS | 66.50 ms | WALL_MS / warm | no | **NOT_COMPARABLE** | state mismatch: cold-process vs warm state/scope: capture step inside a cold-process verify session and the CLI writes the PNG; legacy 66.5 ms is a warm-session retrieval without a PNG write. Gate would need the no-write row, which this CLI does not expose. |
| M6 | Reset -> ready | not measured | 0 | n/a | n/a | warm / WALL_MS | 748.10 ms | WALL_MS / warm | yes | **NOT_MEASURED** | ACOS row missing (NOT_MEASURED = not pass) NOT MEASURED: the ART launch engine has no session reset command (scenarios/v2-art-counter.yaml: Reset is the app's own button); tapping the app's Reset and observing state is not a reset->ready metric. The app-reset tap->assert value is recorded in detail, never substituted for M6. |
| M7 | UI tree with text | 2.0 ms | 16 | 1.50..2.00 | 150.0% | cold-process / WALL_MS | 538.00 ms | WALL_MS / warm | no | **NOT_COMPARABLE** | state mismatch: cold-process vs warm ACOS value is the first assert_text observation on the live tree inside a cold-process verify session. Legacy one-shot text tree 538-768 ms is warm; the legacy persistent route is 4.1 ms. A state mismatch is NOT_COMPARABLE; ACOS has no warm held-session tree row yet. measured spread 150.0% > 15%: this row is noisy and cannot PASS |
| M8 | Action -> state latency | 1358.5 ms | 16 | 1353.00..1375.00 | 4.0% | cold-process / WALL_MS | not published | WALL_MS / warm | no | **NOT_MEASURED** | legacy row missing (NOT_MEASURED = not pass) capability verified on ART (16/20 runs pass all steps, tap Increment 0->1 bound to a fresh frame), but the legacy ACVM never published a scalar action->state value, so the D2 comparison is NOT_MEASURED (BENCHMARKS.md 9.8 called it a capability pass, not a ratio). |
| M9 | Concurrent workloads | 103.52 MiB/session (cgroup peak) | 50 | 102.98..103.81 | 5.2% | cold-process / CGROUP_MEMORY_CURRENT | not published | CGROUP_MEMORY_CURRENT / warm | no | **NOT_MEASURED** | legacy row missing (NOT_MEASURED = not pass) NOT MEASURED as a D2 row: legacy M9 is a scorecard (N=5 plus 10 virtual displays), not a scalar of this type; ACOS correctness here is N=10 50/50 and N=20 84/100 passes, but independent state with a distinct count k per session is not implemented, and the legacy-comparable workset column does not exist. 16 of 150 sessions failed; the structured error of each failure is in the run JSON, and a failure is never re-labelled as app behaviour. cgroup peak per session is reported, never compared to legacy anon+shmem. 4 sample(s) recorded a failed host gate (loadavg1 > 4 or MemAvailable < 30 GiB) and are excluded from the verdict |
| M10 | End-to-end verify wall | 2253.0 ms | 3 | 389.00..4537.00 | 165.7% | cold-process / WALL_MS | 19000.00 ms | WALL_MS / warm | no | **NOT_COMPARABLE** | state mismatch: cold-process vs warm NOT COMPARABLE on scope/state: ACOS value is `acos verify --engine art` on prebuilt fixture APKs (no build/install stage) inside a cold process; legacy ~19 s is the U2 dev loop build->install->launch->assert->evidence->reset->cleanup (install-bound, warm). Suite: 3/3 good tasks pass; a wall on a failing task is still a cost sample, never a pass. 3 sample(s) recorded a failed host gate (loadavg1 > 4 or MemAvailable < 30 GiB) and are excluded from the verdict measured spread 165.7% > 15%: this row is noisy and cannot PASS |
| M11 | Verified tasks / GiB-minute (north star) | **62.84** (N=10; N=1 67.81) | 66 runs / 3 rounds | 45.45..98.60 | 13.46 % | cold-process / CGROUP_ANON_SHMEM per-task integral | **24.4** | mixed: workset core + `memory.current` marginal + warm install wall | **no** | **NOT_COMPARABLE** | ACOS row measured: `results/bench/suite-W52-r4.json`, release, N=10 x 3 rounds, 66 distinct fresh scopes, false_pass 0; primary = anon+shmem of job+every guest cgroup, 100 ms + closing sample, sampler's same-type charge subtracted; host-level union integral agrees 4.495 %. CI low 45.45 > 25.62, so the numeric rule would pass, but the legacy bar's own inputs are NOT anon+shmem warm: 206.0 MiB W1 bare core is **workset** (203.4 anon / 285.7 current), 112.1 MiB marginal is **`memory.current`** (paired ws 99.3), and 19 s is the **install-bound portion** (74–81 %) of a warm dev loop. A mixed-type/state bar cannot gate this row => NOT_COMPARABLE, never PASS. Detail and flake list: `results/art-runtime/W52.md`; N=1 (W45) 67.81 CI 47.91–110.28. |

**Verdict counts (this cold-process run, kept as history): 0 PASS, 1 NOISY (M3), 5
NOT_COMPARABLE (M2, M5, M7, M10, M11), 5
NOT_MEASURED (M1, M4, M6, M8, M9), 0 TIE/FAIL, false passes 0.** The JSON validates
(`bench-d2: validation ok`). No NOT_COMPARABLE or NOT_MEASURED row is a win. **This run
has no PASS at all**: M3 (cold start -> first correct frame) measured 265.5 ms
(first-frame component 235-260 ms in every sample) but its composite spread is 75.7 %,
because `timer_overhead_ms` (engine-timer wall minus step durations, i.e. session
close/teardown) spiked up to 4534 ms on two samples while four other agents' ART runs and
suite tasks shared the host. Per §11.2 a noisy row can never PASS, so it is reported
NOISY rather than repeating W33's PASS at a luckier moment.

### 12.1 M11 — verified tasks per GiB·minute, N=1 and N=10 (W45, W52)

Both rows use the same denominator contract: cgroup `memory.stat` **anon+shmem** of the
job **plus every guest** cgroup, 100 ms samples and a closing sample, sampler's
`RssAnon+RssShmem` × window subtracted, page cache excluded; `false_pass > 0` zeroes M11.
The N=10 runner (`tools/run-suite.sh --concurrency 10`) also reports the alternative
"host-level integral over the same window, all job + guest cgroups", discovered
independently of the per-run samplers, and rejects the run when the two disagree beyond
the 5 % noise floor (`SCOPE_MISMATCH`).

| row | N | rounds | runs/scopes | M11 | 95 % CI | round M11 / spread | good / wrong | false_pass | verdict |
|---|---:|---:|---|---:|---|---|---|---:|---|
| W45 `suite-W45-r2e.json` | 1 | 2 | 44 / 44 | **67.81** | 47.91–110.28 | 67.43 / 68.20, 1.14 % | 11/11, 11/11 | 0 | vs 24.4 numeric PASS, bar NOT_COMPARABLE |
| W52 `suite-W52-r4.json` | 10 | 3 | 66 / 66 | **62.84** | 45.45–98.60 | 58.00 / 64.72 / 66.46, 13.459 % | 11/11, 11/11 | 0 | **NOT_COMPARABLE** (numeric CI low 45.45 > 25.62) |

W52 method record: release binaries built at git HEAD
`5b722ac7badc3adb65e0fbc29bf7269bb37c2ea9` (05:04:07Z), `acos` sha256
`2fb5813bc123655958a1795f5b21f17488a3d6117a6c59cca9799d77fd5f537f`, `acos-art` sha256
`75d7b448ae431e855d5fd945926e4ffd57852cfc3be3871d7b4986d3b38acffb`; host gate at start
loadavg1 2.46, MemAvailable 41.87 GiB; 05:30:13Z–05:30:53Z; per-round denominator mean
0.175044 GiB·min; host-level vs per-run 4.495 % (rounds 4.964 / 4.367 / 4.089 %);
peer denominators (not gates): resident 60.41, `memory.current` 39.84, job-only 72.41.

**Bar comparability (checked, not assumed).** The 24.4 bar in
`results/art-runtime/ADVISOR-benchmarks-top10.md` item 3 is
`((206/10) + 112.1)/1024 × (19/60) = 0.0410 GiB·min/task`. The named legacy rows
(BENCHMARKS §3, `docs/reference/ACVM_INVENTORY.md` §2) are **not** anon+shmem warm:
206.0 MiB is the W1 diet **workset** (203.4 anon / 285.7 `memory.current`), 112.1 MiB is
**`memory.current`** (paired ws 99.3), and 19 s is the **`adb install -r` portion
(74–81 %)** of the warm U2 dev loop, not a whole-task wall. The bar is a mixed
type/state construction, so the M11 row is **NOT_COMPARABLE**; the numeric ratio
62.84 / 24.4 = 2.58× (CI low 1.77×) is reported for information only, never as a D2
PASS. No pure anon+shmem/warm legacy M11 bound exists in the record.

**Flakes seen during W52 (rejected attempts, kept as evidence).** r1: T09-wrong r3
`GUEST_EXITED` (status 126) at `reset`, task_ok 10/11, round 3 inflated → spread 25.5 %
NOISY. r2: T01–T06 good+wrong r3 all `GUEST_KILLED_BY_SIGNAL` (signal 9, ~150 ms,
peak 7 MiB, `oom_kills: 0`) with `utc_ms` within 3 ms of each other, plus
`INTERNAL: cannot write to acos-art: Broken pipe` at `assert_log` (T07-wrong, T12-wrong)
and `INTERNAL: acos-art session exited` at `tap` (T10-good) — another agent's debug ART
run was live in that window (user journal 05:23:19–22). r3: correctness clean but round 2
was 1.4× slower under other agents' CPU load → spread 35.5 % NOISY. r4 is the accepted
record with no flakes. Full detail: `results/art-runtime/W52.md`.

**Honest notes on this run (do not read them as a W39 regression)**

* Four latency samples (lat-010..013, 03:01:54-03:02:20Z) failed with a structured
  `INTERNAL: acos-art session exited` at a tap step; one N=1 rep (4-5) and two N=5 reps
  (1-2) failed the same way in the same window, while the N=10 cohorts are 50/50 and the
  later N=5 reps are 15/15 and the untagged N=20 reps 80/80. The window is 26 s wide, the
  fault hits N=1 as well as N=20, and the host gate flagged four *other* reps in the same
  run.
* It is not the W39 staging cache: 20 fresh `acos verify` sessions run back to back
  (03:09:38-03:11:26Z) pass **20/20 with `ACOS_ART_BUILD_CACHE` on and 20/20 with it
  off**. The failure hypothesis (host contention during a tap settle killing the
  session) is recorded as **untested** in `results/art-runtime/W39.md` §7; the smallest
  next step is to capture the session's own exit path (session.rs, W38-owned) when it
  recurs.
* M2's N=5 pass fraction (15/25) and M9's N=20 fraction (84/100) are dominated by the
  same window; the per-rep table in the `.md` shows which reps were host-gate
  contaminated.

**What this changes**

* M9's stale W33 reason ("All observed session failures ... clang bus error / pthread
  EAGAIN" while compiling the binder shim and D8 guest) is gone: the W37 build-once cache
  removed that compiler storm, N=10 is **50/50**, and the harness now states the measured
  pass fraction and says a failure is never re-labelled as app behaviour.
* Launch-wall evidence behind these rows is in `results/art-runtime/W39.md`: exec ->
  `activity-resumed` p50 **2156.0 ms** with the cache off vs **503.0 ms** with it on
  (20 cold launches each, same release binary, `ACOS_ART_STAGE_TIMING` markers), of which
  all APK-independent staging is now 0-10 ms.
* M1/M2/M4/M6/M7 still need a warm or held session; M9/M11 still need a scalar legacy
  workset and the N=10 suite runner; M3 needs the timer moved to `exec` (W33 residual 1)
  and a quiet host.

**Previous run (kept for history):** W33, `results/bench/d2-20261112T012430Z.json`
(2026-11-12T01:24:30Z -> 01:44:46Z, git HEAD `ae42670`, binaries `e8d01639…`/`9861d313…`):
**1 PASS (M3 258.0 ms), 4 NOT_COMPARABLE (M2, M5, M7, M10), 6 NOT_MEASURED (M1, M4, M6,
M8, M9, M11), 0 false passes**, N=10 35/50 and N=20 44/100 (the pre-W37 compiler storm).
W37's interim re-runs (`d2-20261112T021829Z-w37`, `d2-20261112T022353Z-w37`) measured only
M3/M9 (M3 NOISY, M9 N=10 10/10 twice).

**Reproduce** (`.cache/w39-target` used here):

```sh
CARGO_TARGET_DIR=.cache/w39-target cargo build --release -p acos-cli -p acos-art
tools/art-runtime/bench-d2.sh --target-dir .cache/w39-target --no-build
# stage table / launch wall (W39): results/art-runtime/evidence/W39-measure.sh before|after 20
#   with ACOS_ART_STAGE_TIMING=1; parser results/art-runtime/evidence/W39-stages.py
```

**Residuals:** (1) M3's composite is noisy whenever the host is shared (this run) — report
the first-frame component, not the composite; (2) the 26 s session-exit window above is
undiagnosed and needs a follow-up that captures the session's exit path; (3) no warm
pool/held session, so M1/M2/M4/M6/M7 remain cross-state; (4) M9/M11 need the scalar
legacy workset and the N=10 suite runner (W30-owned).

---

### 12.2 Warm D2 rows on a held `acos serve` ART session (W57, 2026-11-12)

**Status:** the five D2 rows that were only NOT_MEASURED or NOT_COMPARABLE because of
**state** (legacy measured a warm, held session; ACOS measured a cold-process one-shot
`verify`) are now measured in the legacy state: a held `acos serve` ART session
(W50), cgroup anon+shmem 60 s after the first frame, and warm screenshot / tree /
reset requests over the daemon's real Unix socket. Raw record:
`results/bench/d2-warm-20261112T061838Z.json` (+ `.md`, every sample and rep, per-rep
host load and contamination); harness `tools/art-runtime/d2_warm.py`, driven by
`tools/art-runtime/bench-d2.sh --mode warm`; tests `tools/tests/test-d2-warm.sh`
(15/15, no ART). Every verdict below is `benchlib.verdict()` (§11.2) — spread gate,
tie is not a pass, same type AND state or NOT_COMPARABLE.

**Method record**

```yaml
date: 2026-11-12T05:58:10Z -> 2026-11-12T06:18:34Z (M1 5 reps, M2 9 attempts / 7 clean-ish,
      M5 n=20, M6 n=10, M7 n=20, each rep its own run-exclusive.sh acquisition)
engine: art                       # held `acos serve --engine art` session, one ART guest per session
profile: release                  # a non-release binary path is refused (exit 2) before any guest
git_head: bab06c6 (detached worktree .cache/w57-wt at build time;
          the shared tree's HEAD advanced to fc47593 during the run)
binaries: {acos: c0ea1e7536515388e6d2efd381318341a646a75429afdda6d560f617d69bd5cb,
           acos-art: 74ac8711a6ba59d7e560059c2fefd11d5e01f98b40daeb48b80174903e35f8aa}
build: CARGO_TARGET_DIR=.cache/w57-target, release, from a clean detached worktree of
       the commit above (the shared tree was dirty with W54/W55/W56 work; the dirty
       fingerprint in the JSON therefore does NOT describe the measured binary)
apk: fixtures/build/apps/counter.apk sha256 e037aee210471283479f04339235e783c1d344aa560f82a22ae1b53ee6610366
rustc: rustc 1.90.0 (1159e78c4 2025-09-14)
settle: 60 s after the LAST first frame of the rep, 100 ms cgroup samples, value =
        last sample; sampler's own RssAnon+RssShmem subtracted (W45/W52 rule)
lock: one ART run per run-exclusive.sh acquisition (11 acquisitions + 2 acquired for
      the two failed N=5 attempts, see below); the lock was never held across reps
host: loadavg1 0.96-4.87, MemAvailable 40.6-41.8 GiB during the run; two N=5 reps
      (rep2, rep3) touched loadavg1 4.87 and are recorded contaminated + excluded
rules: spread=(p95-p5)/p50; >15% => NOISY, never PASS; noise=max(spread,5%); TIE is not a
       PASS; a type/state/settle mismatch is NOT_COMPARABLE; nothing is estimated
```

| # | Metric | ACOS-ART warm value | n | 95% CI (p50) | spread | state / type | Legacy ACVM | Same type+state | Verdict | Reason |
|---|---|---|---|---|---|---|---|---|---|---|
| M1 | Fixed idle core memory (held ART session, no interaction, 60 s settle) | **117.50 MiB** | 5 | 114.08..118.66 | 3.4% | warm / CGROUP_ANON_SHMEM | 206.00 MiB | yes (settle 60 s both) | **PASS** | ACOS row is `acos serve` + one launched, untouched counter session (the ACOS analogue of an idle core: the framework only exists inside a session), 60 s after its first frame, reads only. Legacy 206.0 MiB is a bare core with **no** app session, so this row charges ACOS for a session legacy's number does not carry (daemon alone p50 **1.36 MiB**; guest process tree p50 **87.82 MiB**). Type checked, not assumed: the inventory defines legacy "workset" as the core cgroup's anon+shmem (`docs/reference/ACVM_INVENTORY.md` §2), the same column measured here. p50 117.50 <= 206.0 x 0.95 = 195.7. |
| M2 | Per-session memory at N=5 (incremental and marginal) | **107.50 MiB/session** | 5 | 105.72..113.61 | 7.0% | warm / CGROUP_ANON_SHMEM | 78.70 MiB | yes (settle 60 s both) | **FAIL** | 5 held sessions, settled 60 s after the last first frame; incremental = (total − daemon-only)/5 per rep (the legacy `(loaded−bare)/5` formula; the "bare" baseline here is the `acos serve` daemon alone because ACOS has no booted framework without a session — stated, not hidden). Marginal (N=5 − N=1)/4 = **107.67 MiB/session** agrees, so the result is not an artefact of the baseline. p50 107.50 > 78.7 x 1.070 = 84.18: ACOS costs 1.37x the legacy v9 per-session anon+shmem. Two of nine N=5 attempts were excluded: rep2/rep3 failed the host gate (loadavg1 4.87) and are recorded, not dropped silently. |
| M5 | Fresh screenshot retrieval (warm, held session) | **80.08 ms** | 20 | 79.74..80.71 | 6.6% | warm / WALL_MS | 66.50 ms | yes | **FAIL** | One `acos_observe {png:true, screenshot:path}` on the already-held session, request write -> reply: host capture + PNG write of a 9,545,882-byte 1080x2400 frame. Freshness is part of every sample (frame_seq > previous_seq, PNG magic, non-zero bytes): 20/20 ok. p50 80.08 > 66.5 x 1.0657 = 70.87. The legacy row is retrieval latency of a request-sequenced fresh frame; the PNG write is inside the shipped verb, so this is not a like-for-like removal candidate — it is a real 1.20x regression on a warm retrieval. |
| M6 | Reset -> ready (warm, held session) | **681.79 ms** | 10 | 658.44..714.38 | 14.1% | warm / WALL_MS | 748.10 ms | yes | **TIE** | serve's `acos_reset` (stop the guest, relaunch the same APK with empty app-data, bind a fresh frame) timed request-write to reply; the reply is only sent once the fresh frame is bound, so the value IS reset -> ready. Readiness and correctness bound in every sample: frame_seq > previous_frame_seq, the app was tapped to a non-zero count first, and the post-reset tree reads 0 — 10/10 ok. p50 681.79 sits inside the noise band (642.40..785.50): a TIE is **not** a pass. Legacy's command-only value (110.3 ms) is never substituted for the ->frame value. Spread 14.1% is inside the 15% gate but close to it. |
| M7 | UI tree with text (warm, held session) | **251.93 ms** | 20 | 251.14..252.41 | 8.8% | warm / WALL_MS | 538.00 ms | yes | **PASS** | One `acos_observe_tree` on the already-held session (guest traversal + serialization, text in every node, 12 nodes / 4 text nodes / 4,101 B, never truncated): 20/20 ok. The legacy comparable row is the **shipped** one-shot `ui --text` tree (538 ms p50, warm); the 4.1 ms persistent-observe number is research, not a shipped route, so it is not the bar. p50 251.93 <= 538 x 0.912 = 490.57 (2.14x faster). |

**Verdict counts (warm set): 2 PASS (M1, M7), 2 FAIL (M2, M5), 1 TIE (M6), 0 NOISY,
0 NOT_COMPARABLE, 0 NOT_MEASURED, 0 false passes.** Both memory rows also carry a
`benchlib.compare_memory()` record (`COMPARABLE`, ratios 0.570 for M1 and 1.366 for M2),
so type + state + 60 s settle are machine-checked, not asserted. Combined with §12's cold-process
table: the D2 rows measured on real ART are now M1 PASS, M2 FAIL, M3 NOISY, M4
NOT_MEASURED, M5 FAIL, M6 TIE, M7 PASS, M8 NOT_MEASURED (W55 owns the tap-latency
change; not re-measured here), M9 NOT_MEASURED, M10 NOT_COMPARABLE, M11 NOT_COMPARABLE.
A TIE, NOT_COMPARABLE or NOT_MEASURED row is not a win.

**What changed vs the cold-process rows (and what did not)**

* The state objection is gone: M1/M2/M5/M6/M7 are now warm, held-session rows with the
  legacy settle (60 s) and the legacy type (cgroup anon+shmem / wall ms). M5 and M7 moved
  from NOT_COMPARABLE to a real comparison, and the comparison says one improvement
  (tree 2.14x faster) and one regression (screenshot 1.20x slower).
* M2 is a real **regression**, not a state artefact: both the incremental
  (107.50 MiB/session) and the marginal (107.67) are ~1.37x the legacy 78.7 MiB, whose
  workset type is also the core cgroup's anon+shmem. The legacy per-session number is a
  whole Android container session; ACOS's is one `acos-art` + `dalvikvm64` guest tree
  (~88 MiB of it is the guest processes), so the gap is real and named.
* M1's scope caveat is recorded in the row: legacy's 206.0 MiB core has no app session,
  ACOS's 117.5 MiB includes one. The derived legacy core+one-session figure (206.0 +
  130.3 = 336.3 MiB, never a published row) is information only and is **not** a
  verdict input.
* **Two N=5 reps died on `GUEST_EXITED`** during launch (rep1: 2/5 sessions, rep6: 4/5;
  each ~14 s in, one guest at a time, no exit record, loadavg1 1.4-3.6, no host-gate
  violation, no OOM kill recorded by our scope). Both attempts are kept in the raw JSON
  as failed reps and are excluded from the rows — a failure is never re-labelled as app
  behaviour. This is the same undiagnosed guest death W50 §7.3 and W39 §7 recorded;
  §12's residual (2) still stands.

**Reproduce**

```sh
CARGO_TARGET_DIR=.cache/w57-target cargo build --release -p acos-cli -p acos-art   # clean worktree of the commit above
tools/art-runtime/bench-d2.sh --mode warm --target-dir .cache/w57-target --no-build \
    --warm-reps 5 --warm-settle 60 --warm-samples 20 --warm-resets 10 --out .cache/bench-d2-warm/run1
tools/art-runtime/bench-d2.sh --mode warm --analyze-warm .cache/bench-d2-warm/run1/warm \
    --out .cache/bench-d2-warm/run1          # re-derive the table, no ART, no build
tools/tests/test-d2-warm.sh                  # the harness's own tests (no ART)
```

**Residuals (W57).** (1) M4 (worker create -> frame) still has no warm pool to time: a
held serve session returns a bound live frame (0.2 ms) instead of creating a worker, so
the row stays NOT_MEASURED — a warm pool is a capability, not a measurement gap.
(2) The `GUEST_EXITED` launch death above is undiagnosed; smallest next action is to
capture the launcher's exit path/session-exit record when it recurs (an engine-owner
change, `acos-art`/`serve.rs` are not W57's to edit). (3) M2's bare baseline is the
daemon (1.36 MiB), not a booted framework, because ACOS has no session-less booted
framework; the marginal column exists so the reader can see the row without that
baseline. (4) M6's spread (14.1%) is close to the 15% gate under a shared host; a
quieter window would tighten the CI but cannot turn a TIE into a PASS.

---

## 13. Stock emulator baseline (EMUBENCH, 2026-11-18)

**Status: the stock Android emulator is a tracked ACOS baseline. ACOS is better
on **12 of 12** comparable metrics** — MERGE040D (2026-11-18) closed the two rows
that could not be paired: warm launch now compares at one observation boundary
(a readable settled UI tree on both sides, §13.3) and install has an ACOS
counterpart ("APK ready", §13.3 + §13.4). Owner direction 2026-11-18 §2: *"the
stock emulator is a tracked baseline (boot, launch, tap-to-result latency,
UI-tree read, screenshot, install, memory per instance, CPU, and verified tasks
per GiB-minute at N concurrent instances). Every release gate compares against it
and flags any metric where ACOS is worse."* This section is that comparison: the
measured emulator column, the ACOS column from the last release gate, the
better/worse verdict per metric, and the gate rule a release must satisfy.

Raw record: `results/bench/emubench-2026-11-18.json` (every sample, every
`free -m` host step, per-rep task detail, failures). Harness:
`tools/art-runtime/emubench/emu_bench.py`, driver `run-emubench.sh`, gate
`emu_gate.py`. Evidence bundle: `results/handoff/EMUBENCH/`.

### 13.1 What is relaxed, and the limits that are not

ADR-0015 allows **one** emulator at ≤2048 MiB. The owner relaxed it *for this
benchmark only*: N=1, 2 and 4 concurrent instances, each ≤2048 MiB guest RAM,
only while the host stays under the 90 % RAM ceiling, always shut down. Every
limit is enforced in code, not by convention:

| Limit | How it is enforced | Evidence |
|---|---|---|
| ≤ 2048 MiB guest RAM | `hw.ramSize=2048` written into every AVD config and `-memory 2048` on the command line | `phase-*.json` `config.guest_mb`, `cmd-*.txt` |
| N ≤ 4 | the phase list is fixed (`n1-cold`, `n1-warm`, `n2`, `n4`); one AVD and one port per instance | `config.ports` |
| Host usage < 85 % (abort) / 90 % (hard ceiling) | `MemAvailable` is read **before every instance start**; the start is refused if `MemAvailable − 4.3 GiB` would fall below 15 % of `MemTotal`. `free -m` is logged at every step | `host_log[]` (`used_pct`, `free_m`) |
| Always shut down | every phase stops all instances in `finally`, plus SIGINT/SIGTERM handlers; the driver verifies no `qemu-system-x86_64` survives and force-kills leftovers **by PID** (never `pkill -f`) | driver log `PASS no qemu-system process is running` |

### 13.2 Configuration

| Item | Value |
|---|---|
| AVD | `emb-1…4` under `$WT/.cache/avd` (`ANDROID_AVD_HOME`), Pixel 6 profile |
| Emulator | `/home/Qui/Android/Sdk/emulator/emulator` 35.6.11.0 (build 13610412), KVM |
| System image | `system-images;android-34;google_apis;x86_64` (API 34, x86_64) — the same API level as ACOS's image |
| Guest RAM / vCPU | 2048 MiB / 4 |
| GPU | `swiftshader_indirect` (software; no GPU passthrough, as the brief allows) |
| Flags | `-no-window -no-audio -no-boot-anim -gpu swiftshader_indirect -memory 2048 -cores 4 -no-metrics`; **cold rows add `-no-snapshot`**, warm-boot rows load the quickboot snapshot |
| Display | 1080×2400 @ 420 dpi — the same screen and density as the ACOS reference frames |
| Apps | `fixtures/build/apps/counter.apk` (`dev.acos.fixtures.counter`), `form-validation.apk` (`dev.acos.fixtures.formvalidation`) — byte-identical APKs to the ACOS rows |
| Device route | `adb` 1.0.41, always `-s emulator-55xx` (the owner's phone was never addressed) |

### 13.3 Method — the same definitions as §1

The emulator exposes no ACOS API, so each metric is measured with the fastest
*standard* Android route, which is the route an agent using the emulator would
pay. Nothing is estimated; every row is a wall-clock sample.

| Metric | Route | Correctness oracle | Resolution |
|---|---|---|---|
| cold boot → first app frame | process start → `sys.boot_completed` → launch → first frame whose app region differs from the idle **launcher** frame | live `uiautomator` tree reads `count == 0` | one screencap (~200 ms) |
| warm launch → **readable settled UI tree** | `am force-stop` → `am start` (async) → first frame whose app region differs from the idle launcher → `uiautomator dump` until two **consecutive** dumps agree and read `count == 0` | that settled tree | one screencap + ≥2 tree dumps |
| tap → settled UI result | `adb shell input tap` on the node centre, then `uiautomator dump` until the expected state appears | counter: `count` text `0→1`; form: the `Name is required` error text | one tree dump (~2 s) |
| UI tree read | `adb exec-out uiautomator dump /dev/tty` (one fresh `app_process` per call) | non-empty tree with text | — |
| screenshot | `adb exec-out screencap -p` (PNG validated) | valid 1080×2400 PNG, not black | — |
| install | `adb install -r -t <apk>` | `Success` | — |
| install → APK ready (ACOS) | the launcher's own stage ladder (`ACOS_ART_STAGE_TIMING=1`): `launch-entry` → `guest-exec`, i.e. the APK path → the moment the guest can start it (ingest of the package facts/manifest/dex list, staging into `<data>/app`, native-lib extraction, DEX/classpath preparation) | the launch that follows is a real `acos_launch` pass (`count == 0`) | 16 stage markers (10 ms granularity) |
| reset | tap to a non-zero count, `am force-stop` + `pm clear` + launch → launcher-frame change | tree reads `count == 0` | one screencap |
| memory per instance | `/proc/<qemu>/smaps_rollup` Pss / Rss, `/proc/<qemu>/status` RssAnon+RssShmem, guest `dumpsys meminfo` | — | 2 s sampling |
| CPU | `/proc/<qemu>/stat` utime+stime delta over a 10 s idle window | — | — |
| verified tasks / GiB-minute | N concurrent instances, each running the counter scenario equivalent: launch → verify 0 → tap increment → verify 1 → reset. Tasks are counted only when **both** tree checks pass | tree, 0 false passes | 1 s memory sampler |

**The observation boundary is one rule for both sides (MERGE040D(b)).** A launch
is finished when the UI **tree** can be read and has stopped changing — not when
the pixels first moved. ACOS's publishable warm-launch number *is* that boundary
(`m4_launch_strict_ms`: its strict settle, which pays a tree read). The emulator
row used to stop at the launcher-frame change and paid no tree read, so the two
numbers answered different questions and the row was a "tie" that was really an
artefact of the definitions. `n1-warm-tree` measures the emulator at the same
boundary: `settled_tree()` in `emu_bench.py` requires two consecutive identical
`uiautomator` dumps that report the expected content, and the timer starts before
`am start`. The frame-change-only number is still recorded
(`warm_launch_ms`, 286.1 ms) as a **reference-only** row the gate does not
score.

Two deliberate differences from the P6 emulator run (`results/art-runtime/P6.md`):

1. **First frame = "app content visible", not "pixel-identical to a stored
   reference".** A frame captured in one emulator boot differs from the same
   screen in another boot by 1–3 units per channel on *every* pixel (measured:
   `#191c1e` → `#1a1b21`, `#f0f0f3` → `#f1f0f7`; 98 % of pixels "differ", 0.0 %
   of pixels differ by more than 8). A cross-session pixel-identity gate is
   therefore not a valid criterion here; the launcher→app change plus the tree
   read is. P6's within-session pixel gate is unaffected and still stands.
2. **The warm-boot row uses a quickboot snapshot** (owner: "a snapshot for warm
   rows"), reported separately from the cold boot.

### 13.4 Side-by-side

ACOS column: the last release gate (`results/release/v0.3.0-gate/timing/02-ab/`,
RELGATE2, v0.3.0 RC `169bfa5e`, 2026-11-16, n=60 strict samples per latency row;
M11 from §12 W52 at N=10). Emulator column: this run, median of ≥5 reps (n as
stated). "Better" names the side that wins the metric.

| Metric | ACOS | Emulator | Unit | Better | Delta | Verdict |
|---|---:|---:|---|---|---:|---|
| Cold boot -> first app frame | 4200 | 13632 | ms | ACOS | -69.2 % | better |
| Warm launch -> readable settled UI tree | 288.4 | 5004.9 | ms | ACOS | -94.2 % | better |
| Install -> APK ready to launch | 10.0 | 1358.8 | ms | ACOS | -99.3 % | better |
| Tap -> settled UI result (counter) | 949.2 | 1931 | ms | ACOS | -50.8 % | better |
| Tap -> settled UI result (form) | 920.0 | 1946 | ms | ACOS | -52.7 % | better |
| UI tree read with text | 41.4 | 1928 | ms | ACOS | -97.9 % | better |
| Fresh PNG screenshot | 11.7 | 527.1 | ms | ACOS | -97.8 % | better |
| Reset -> first correct screen | 308.8 | 530.8 | ms | ACOS | -41.8 % | better |
| Instance memory, PSS | 234.9 | 3578.0 | MiB | ACOS | -93.4 % | better |
| Instance memory, anon+shmem | 57.1 | 3531.6 | MiB | ACOS | -98.4 % | better |
| Idle CPU | 0.4 | 13.1 | % of one core | ACOS | -96.7 % | better |
| Verified tasks / GiB-minute | 62.84 | 2.54 | tasks/GiB-min | ACOS | -96.0 % | better |

Delta is `(ACOS − emulator) / emulator` for the latency and memory rows and
`(emulator − ACOS) / ACOS` for throughput; **negative always means ACOS is
better**. The verdict column is the gate's: `better`, `worse` (worse but inside
the 5 % noise floor) and `WORSE` (worse beyond it). Generated by
`tools/art-runtime/emubench/emu_gate.py`; the raw gate output is
`results/handoff/EMUBENCH/gate/gate.txt` + `gate.json`.

**The two rows MERGE040D fixed, and what each side really measures.**

* **Install — ACOS counterpart added (MERGE040D(c)).** ACOS does not "install"
  into a system image; it makes an APK launchable. That cost is now measured, not
  asserted: `m9_apk_ready_ms` = `launch-entry` → `guest-exec` from the launcher's
  own stage ladder, **10.0 ms** p50 (n=5, samples 50/10/10/10/0, 10 ms
  granularity) — a staged copy of the same 12 KB `counter.apk`, the package facts
  and DEX list read, no native libs to extract, and the guest about to exec.
  Emulator: **1358.8 ms** p50 first install (n=5, `adb install -r -t
  counter.apk`), 276 ms reinstall (n=20, app present). The pairing uses the
  emulator's *first* install (its more expensive number), so it is conservative
  for ACOS. Two honest caveats: ACOS runs no dexopt at all (ART verifies/JITs in
  the guest, lazily — that is the architectural difference the row captures), and
  the **first** launch in an empty scratch pays the launcher's one-time shared
  cache build — `m9_apk_ready_cold_ms` = **20730 ms** p50 (n=1) for the
  `derive_classpath`/linkerconfig/guest-DEX/shim build — which every later session
  and every other APK reuses, and which the emulator's own boot pays in its
  13.6 s cold boot. The 20730 ms is recorded, never mixed into the 10 ms.
* **Warm boot from a snapshot — 1071 ms** p50, p90 1194 ms (n=5). ACOS has no
  snapshot concept; for scale, ACOS's *cold* core→first-frame is 4200 ms (3.9×
  the emulator's snapshot boot) and the emulator's own cold boot is 12.7 s
  (3.0× ACOS).
* **Warm launch — the tie was a definition artefact (MERGE040D(b)).** The old
  row compared ACOS's strict settle (288.4 ms, which pays a tree read) with the
  emulator's frame-change-only number (286.1 ms, which pays none): two different
  questions, and the "+0.8 %, inside noise" verdict was the definitions talking.
  At the shared boundary the emulator needs **5004.9 ms** p50 (n=5, p90 5788 ms,
  spread 19.5 %): ~0.3–0.6 s to the frame change plus two ~1.9 s `uiautomator`
  dumps that must agree. ACOS is 94.2 % faster. The frame-only row is kept as a
  `REFERENCE` line in `emu_gate.py` (never scored); ACOS's fast-settle row for the
  same launch is **143.4 ms** for the record.
* **Throughput, all N.** The emulator is one instance per session, so the
  per-GiB rate is flat in N and N=4 is its most favourable point:

| N | verified tasks | wall | GiB (mean, PSS) | tasks/GiB-min p50 | p90 | CPU % / instance | MiB / instance |
|---:|---|---:|---:|---:|---:|---:|---:|
| 2 | 6/6 per rep (30/30) | 20.9 s | 6.78 | **2.53** | 2.59 | 158 % | 3479 |
| 4 | 12/12 per rep (60/60) | 21.0 s | 13.45 | **2.54** | 2.57 | 116 % | 3454 |

  ACOS's 62.84 is at **N=10** (BENCHMARKS.md §12, W52) with a whole-job
  anon+shmem integral, so the comparison is ACOS's hardest point against the
  emulator's easiest one; 0 false passes on both sides.

### 13.5 Release-gate rule

The emulator rows are part of the release gate, not a report:

1. A candidate release must run `tools/art-runtime/emubench/emu_gate.py --acos
   <gate timing JSON> --emu results/bench/emubench-2026-11-18.json` (or a
   re-measured baseline when the host changed) and show **0 metrics worse for
   ACOS**. The ACOS document must also carry `m9_apk_ready_ms` (the release
   gate's own `v0.3.0.json` predates it): the gate takes it from the file the
   bench wrote, with the provenance recorded next to it
   (`results/bench/merge040d/acos-gate-doc.json` is that merge for v0.3.0).
2. Any metric where ACOS is worse than the emulator **blocks promotion** or needs
   an explicit owner decision recorded in `DECISIONS.md`. A metric only one side
   measured is `NOT_MEASURED`, never a pass.
3. Re-measure the baseline when the host, kernel, emulator binary or system image
   changes; the emulator is not a fixed reference. The baseline JSON carries the
   host, image fingerprint and per-step `free -m`.
4. The gate is a *correctness-and-latency* gate: `false_pass == 0` from the
   scenario matrix still outranks it (§2 rule 7).
5. The gate may run without the ART lock only for the emulator side (it is not an
   ACOS ART run); the ACOS side must come from an exclusive, guarded bench run.

### 13.6 Findings and caveats

1. **The emulator's cost is structural, not a tuning artefact.** One session
   costs a whole instance: **3.45–3.58 GiB** of host PSS, **13 %** of a core
   idle, **1.9 s** for one UI-tree read and **0.5 s** for one screenshot. ACOS's
   core is 57 MiB anon+shmem / 235 MiB PSS with 0.4 % idle CPU and 41 ms strict
   tree reads. Nothing in the emulator's numbers is a bug; the architecture is
   the number.
2. **Warm launch was measuring two different things — fixed (MERGE040D(b)).**
   The +0.8 % "tie" compared ACOS's strict settle (which pays a tree read) with
   the emulator's frame change (which pays none). At the shared boundary the
   emulator row is **5004.9 ms** and ACOS is 94.2 % faster. Two consequences are
   recorded rather than smoothed over: (a) the emulator's settled-tree number is
   dominated by `uiautomator dump` (~1.9 s per read, and the boundary needs two),
   which is the *standard* Android route an agent pays — it is not an artefact of
   the harness; (b) the frame-change-only row stays in the baseline as
   `REFERENCE` so the old number can still be cited, but it is never scored.
3. **Install now pairs instead of `NOT_MEASURED` (MERGE040D(c))**: ACOS 10.0 ms
   per APK against the emulator's 1358.8 ms first install, with the 20730 ms
   one-time cache build recorded separately (§13.4). The gate scores the per-APK
   number; a release that regresses the *cold* build is caught by §3/§4, not here.
4. **A cross-boot frame is not pixel-identical** — every pixel differs by 1–3
   units per channel between two boots of the same AVD on the same screen
   (`#191c1e` → `#1a1b21`, `#f0f0f3` → `#f1f0f7`; 98 % of pixels "differ",
   0.0 % by more than 8). This is why the first-frame criterion here is the
   launcher→app change plus a tree read, and why P6's pixel-identity criterion
   is only valid *within* one emulator session. Any future ACOS-vs-emulator
   pixel-parity work must capture both sides in the same session or use a
   per-channel tolerance.
5. **Noisy rows are labelled, not hidden** (BENCHMARKS.md §2 rule: spread
   (p95−p5)/p50 > 15 % is noisy): cold launch after boot 148 %, warm launch
   (frame change) 77.7 %, reset 27.8 %, idle CPU 18.5 %, cold `am start -W`
   26.4 %, install 264 % (it depends on whether the app was already present).
   The rows the gate uses (tap, tree, screenshot, memory, throughput) are 1–8 %.
   MERGE040D's two new rows: the settled-tree warm launch spread is 19.5 %
   (labelled noisy — it inherits the dump timing) and APK ready is 5 samples at
   0–50 ms, i.e. at the 10 ms instrumentation granularity, which is reported as
   "≤ 1 tick" rather than as a precise number.
6. **Host usage never approached the ceiling**: peak **49.9 %** at N=4
   (`host_log` in the merged JSON, 110 `free -m` samples), limit 85 %/90 %; the
   guard was never tripped. N=4 costs ≈13.5 GiB of host memory.
7. **Not claimed**: N>4 (owner limit), GPU passthrough, any non-standard emulator
   route (no persistent tree service, no scrcpy), or any timing number taken
   outside an exclusive lock acquisition. The four phases were four separate
   `run-exclusive.sh --exclusive` acquisitions under `guarded.sh --mem 24G`, so
   no other agent's ART run overlapped them; the host still carried other
   agents' non-ART work (load1 2–10, recorded per step).

### 13.7 Reproduce

```sh
# unit checks (no emulator)
python3 -I tools/art-runtime/emubench/emu_bench.py --self-test      # 14 checks, 0 failed

# full baseline: 4 exclusive lock acquisitions, ~20 min, always shuts down
tools/art-runtime/emubench/run-emubench.sh
tools/art-runtime/emubench/run-emubench.sh --smoke                  # wiring only, never a claim

# one row only: the warm launch at the settled-tree boundary (MERGE040D(b))
tools/art-runtime/emubench/run-emubench.sh n1-warm-tree

# the ACOS install row (MERGE040D(c)): exclusive, guarded, ~1 min
tools/art-runtime/run-exclusive.sh --task EMUBENCH --exclusive -- \
  tools/guarded.sh --mem 12G --time 600 -- \
  python3 -I tools/bench/acos_bench.py --phase install \
    --acos target/release/acos --scratch .cache/bench-install-scratch \
    --out results/bench/merge040d/acos-apk-ready.json --reps 5

# gate: compare a gate's ACOS numbers against the committed baseline
tools/art-runtime/emubench/emu_gate.py \
    --acos results/release/v0.3.0-gate/timing/02-ab/v030.json \
    --emu results/bench/emubench-2026-11-18.json --noise 5
```
