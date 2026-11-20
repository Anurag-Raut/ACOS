# ACOS — Tasks and plan

**Status:** P0–P1 partially complete. Planning is now driven by the **V1
definition** (`docs/V1.md`) and owner directives D1–D4 (`SPEC.md`). `STATUS.md`
is the dated truth; `EXPERIMENTS.md` is history; `docs/FINDINGS.md` is the
append-only findings log.

Owners are named **`subagent`** (implementation is done by parallel subagents
per D4). The lead takes decisions and commits verified work locally (no push).

A stage/gate is passed only with a reproducible artifact; a claim without one is
not progress. The honest progress metric on the framework path is the **first
`unsupported` native hit per fixture** (`research/framework-dex/REPORT.md` §e).

---

## V1 critical path (ordered, each arrow is a gate)

| # | Stage | Gate (must pass before the next) | Owner | Status |
|---|---|---|---|---|
| C1 | VM interpreter executes the 20 `dexprogs` on DEX with the **temporary** Rust bootstrap built-ins | 20/20 stdout+exit match `fixtures/expected/dexprogs`; GC-stress passes | subagent | in progress |
| C2 | Boot the **real core library** and run the `println` path (ADR-0007) | `System.out.println` matches `jvm-reference`; first `unsupported` native reported | subagent | not started |
| C3 | Boot the **real framework** + in-process Rust service layer (ADR-0008) | framework `<clinit>` completes; only structured `unsupported` for unbound services | subagent | not started |
| C4 | Activity launch (hello-views) | lifecycle order matches ART-reference; first meaningful frame | subagent | not started |
| C5 | Bind `acos-render` to `android.graphics` natives (ADR-0011) | real rendered frame + UI tree; unsupported ops recorded | subagent | not started |
| C6 | Input + UI tree | tap/type/back drive state; tree has text/ids/bounds/classes | subagent | not started |
| C7 | `acos-jni` + `acos-native-loader` (ADR-0014) | hello-jni: load `.so`, both directions, `RegisterNatives`, `ThrowNew` | subagent | not started |
| C8 | `acos verify` / scenario runner | `acos verify --apk … --scenario … --json --artifacts …` produces the V1 verdict | subagent | not started |
| C9 | Snapshot/restore (ADR-0010) | both gates pass (init time; correct restore) or the architecture is revisited | subagent | not started |
| C10 | Concurrency N=1/2/5/10/20 | independent state/input, no leakage, memory/worker reported | subagent | not started |
| C11 | Benchmarks vs legacy baseline (D2) | every tracked metric reported ≥20 reps p50/p95; every worse metric listed with a plan | subagent | not started |

---

## Parallel workstreams (can run alongside the critical path)

| ID | Workstream | Output | Owner | Status |
|---|---|---|---|---|
| W-DEX | DEX parse/verify/decode hardening + boot-classpath coverage | boot-classpath env-gated test green; coverage report | subagent | in progress |
| W-APK | APK ingest hardening + `aapt2` differential | zero `aapt2` mismatches on fixtures; fuzz/mutation clean | subagent | in progress |
| W-VM | `acos-vm` interpreter, heap, GC, bootstrap built-ins | C1 gate; unit tests exist (currently 0) | subagent | in progress |
| W-NAT | `acos-native-loader` ELF/Bionic loader (x86_64) | loads a real NDK `.so`; structured missing-symbol errors | subagent | in progress (scaffold) |
| W-REND | `acos-render` software 2D (tiny-skia + shaping + fonts.xml) | renders a scene fixture with frame id/hash; op-support report | subagent | in progress (scaffold) |
| W-ORA | ART oracle tooling (ADR-0015) | `fixtures/expected-art/` populated for hello-views/hello-jni/dexprogs; `results/oracle/jvm-vs-art.md` | subagent | in progress |
| W-FIX | Fixtures (hello-views, hello-jni, 20 dexprogs, seeded-wrong variants) | V1 fixtures + negative variants built and hashed | subagent | partial (fixtures built; negatives not) |
| W-V1 | V1 test harness (C1–C11 gates) | one command per gate, artifacts under `results/` | subagent | not started |
| W-BOOT | Boot-classpath extraction + reproducibility | `.cache/boot/android-34` reproducible from the image; manifest recorded | subagent | in progress |

---

## Phase history (P0–P9)

The phase framing from `docs/handoff/04` is retained as a coarse map; V1 is a
slice of P1–P5.

| Phase | Meaning | Status |
|---|---|---|
| P0 | reference inventory, workspace, docs | **mostly done** (docs + inventory written; workspace committed) |
| P1 | real APK/DEX loading in Rust | **partial** — parse/verify/decode + static APK inspect verified; execution not |
| P2 | managed runtime | in progress (`acos-vm` scaffold) |
| P3 | JNI/ELF/native | scaffolding (`acos-native-loader`); `acos-jni` planned |
| P4 | app lifecycle + minimal APIs | not started |
| P5 | offscreen rendering + observation | rendering scaffold only |
| P6–P9 | concurrency/isolation, ecosystem, performance, distribution | not started |

---

## Immediate next tasks (concrete, ordered)

1. `[subagent]` Make the working tree compile again (`acos-dex` lazy-table
   work-in-progress); commit when green.
2. `[subagent]` Add `acos-vm` unit tests and pass C1 (20 dexprogs on DEX) with a
   recorded artifact.
3. `[subagent]` Populate `fixtures/expected-art/` from the ART oracle (boot the
   emulator headless, capture, shut down) and record `results/oracle/jvm-vs-art.md`.
4. `[subagent]` Author the V1 scenario file(s) and reconcile the V1 assertion
   strings with the fixture source (`Hello, World!`/`clicks: 1` vs the fixture's
   `Hello ACOS`/`clicks: 0` — see `docs/V1.md` §1.9 note).
5. `[subagent]` Finish `acos-native-loader` (`lib.rs`, `build.rs`) and load a real
   x86_64 `.so` with structured missing-symbol errors.
6. `[subagent]` Make `acos-render` render a scene with frame id/cause/hash and an
   op-support report.
7. `[docs]` Keep `COMPATIBILITY.md`/`BENCHMARKS.md`/`STATUS.md`/`FINDINGS.md`
   current as evidence lands; advance one ledger row per verified artifact.

---

## Standing gates (apply to every task)

- **D2 acceptance gates:** every tracked benchmark metric is a gate; regressions
  and gaps are reported, never hidden; correctness and **zero false passes** gate
  every speed claim.
- **No fake success:** unimplemented behaviour is `unsupported`/`escalated`.
- **Fidelity label** on every result; JVM match ≠ Android-verified.
- **Host safety (ADR-0018):** host RAM must never exceed 90% (internal target
  85%). **Every** `cargo test`, `acos run-dex`, benchmark, build script and
  ad-hoc command runs through `tools/guarded.sh`:
  `tools/guarded.sh --mem 4G --time 900 -- cargo test -p acos-vm`.
  `tools/build-*.sh`, `tools/diff-dexprogs.sh` and `tools/bench-vm.sh` already
  re-invoke it. `tools/ramguard.sh` runs during parallel waves and kills ACOS
  processes once `MemAvailable < 9 GiB` or a single process exceeds 8 GiB RSS.
  The VM enforces `ACOS_HOST_RSS_MAX` (default 2 GiB, `HOST_MEMORY_LIMIT`) and
  `ACOS_WALL_MAX_SECS` (default 600, `TIMEOUT`) independently of the guest heap.
  Free processes, fds, temp dirs and scratch as soon as they are done; the ART
  oracle emulator is ≤2 GiB, one instance, always shut down (ADR-0015); no
  privileged containers; no `sudo`; no >2 GB downloads without authorisation;
  legacy repo read-only.
- **Measurement:** no fixed sleeps; state the metric type; ≥20 reps p50/p95;
  interleaved A/B; artifacts under `results/` (`BENCHMARKS.md`).
