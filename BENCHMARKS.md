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
| per-session incremental N=1/2/5/10 | 155.0 / 154.3 / 153.8 / 153.7 MiB per session | anon+shmem | fresh process per N | `result.sessions.incremental` |
| marginal N=1→2 / 2→5 / 5→10 | 153.5 / 153.5 / 153.5 MiB per session | anon+shmem | fresh process per N | `result.sessions.marginal` |
| N=10 total | 1 573 576 KiB anon+shmem (1536.7 MiB), shared pages flat 5.0–5.1 MiB | anon+shmem + private/shared | fresh process | `result.sessions.sessions` |
| idle CPU, parked/blocked | 0 ticks over 5 s in all 20 windows (0.0 % of one core) | CPU window | parked | `result.idle-cpu` |
| interpreter `02_int_arith` | 22.823 / 22.988 ms, 62 380 insns, 2.73 M insn/s | wall + counters | cold | `result.interpreter` |
| interpreter `05_branches_loops` | 22.663 / 22.905 ms, 49 732 insns, 2.19 M insn/s | wall + counters | cold | `result.interpreter` |
| interpreter `09_strings` | 24.559 / 25.260 ms, 143 608 insns, 5.85 M insn/s | wall + counters | cold | `result.interpreter` |

### 8.2 ACOS vs legacy (§3/§5), every row

| Row | Legacy | ACOS | Verdict |
|---|---|---|---|
| M1 fixed idle core | 206.0 MiB (W1) / 303.9 MiB (v9) anon+shmem | 155.0 MiB anon+shmem booted; 26.2 MiB Vm-only | **better** (−24.8 % vs W1); states differ (see note) |
| M2 per-session at N=5 | 78.7 MiB (v9) anon+shmem | 153.8 MiB/session | **worse** (+95.5 %) |
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
| marginal N=5→10 (v5) | ~99.3 MiB workset | 153.5 MiB anon+shmem | **worse** (+54.6 %); not the same metric type |
| guest `boot_completed` (v10) | 1.517 s | 0.608 s attach handshake | not comparable (no guest boot) |
| parked idle CPU | 3.86–5.94 % (legacy: unreliable) | 0.0 % | **better** |
| process spawn → VM ready | no legacy metric | 20.841 ms | `no-legacy-baseline` |
| attach → bindApplication | no legacy metric | 3.760 ms | `no-legacy-baseline` |
| everything else in §3 (frames, shots, reset, UI tree, renderer, emulator, Chrome) | legacy values | — | `not-yet-measurable` / reference only |

### 8.3 Worse than legacy — explicit, with plan

1. **M2 / per-session memory (+95.5 % at N=5; +19.0 % at N=1; +54.6 % marginal
   N=5→10).** One in-process session costs ~154 MiB anon+shmem and the marginal
   cost is flat (153.5 MiB at every step), i.e. **nothing is shared**. Root
   cause, measured: the boot classpath and app DEX are read with
   `std::fs::read` and parsed into the VM heap — there is no `mmap`, so DEX
   pages are private anonymous pages. Shared pages stay at 5.0–5.1 MiB from
   N=1 to N=10 (that is the binary + libs only). Plan: (a) `mmap` boot-classpath
   and app DEX file-backed and share one parsed class table per process
   (the legacy engine's main lever), (b) copy-on-write session state instead of
   a full second VM, (c) re-measure with this harness; the gate rows
   `sessions.*` exist so a regression is caught, and the win will show up as a
   falling `sessions.marginal_n5_to_n10_mib`.
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
