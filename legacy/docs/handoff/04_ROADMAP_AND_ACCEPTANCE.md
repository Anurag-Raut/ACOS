# 4 — Research, implementation roadmap, acceptance and safety

## The rule for all phases

A phase is **complete** only when it demonstrates an observable, correctly classified capability on real executable input, passes its dedicated differential tests, and reports measured resources against the reference. Avoid spending weeks on enormous abstractions before the first real DEX execution and usable app screen.

A complete-from-scratch Rust runtime for broadly compatible Android APKs is a **large multi-stage systems-engineering undertaking**. Do not promise a date or imply that a small prototype proves full compatibility. Keep a live API/ABI, app-class and behavior coverage ledger.

## Milestones and gates

### P0 — Reference inventory, benchmark foundation, workspace
- Read old ACVM at `/home/Qui/Work/android-container` read-only; study `PLAN.md`, `TASKS.md`, `EXPERIMENTS.md`, `ITERATIONS.md`, `STATE.md`, `research/PLAN-task-first.md`, agent CLI, samples and benchmark harness, if present.
- Create the clean Rust workspace in `~/Projects/acos` with executable CLI, typed JSON contracts and tests.
- Build a licensable app-fixture corpus covering DEX, JNI, View, Compose, Flutter, RN, WebView, lifecycle and network, with expected reference results.
- Measure the existing ACVM and stock Android reference on equivalent tasks, under controlled resource limits, before reporting a 'speedup'.
- Gate: baseline is reproducible, constraints and user stories defined, no older repository changes.

### P1 — Real APK and DEX loading in Rust
- APK/split-APK parser; Android binary manifest/resources; DEX verification/bytecode decoding; ABI/native dependency analysis.
- Execute **actual DEX bytecode compiled from Java/Kotlin** (not a custom instruction format) with arithmetic, branches, methods, objects and exceptions.
- Differential comparison against actual Android reference; reject invalid DEX with structured errors.
- Gate: a real compiled test app's DEX runs through Rust and is reproducibly correct. Parsing APKs alone is not sufficient.

### P2 — Rust Android-compatible managed runtime
- Class loaders, class initialization, object model, GC, threads, monitors, exceptions, Java standard library behavioral subset, reflection as required.
- Expand DEX opcode coverage toward full spec for selected API, with property-based/fuzz tests, opcode coverage report and performance counters.
- Optional JIT/AOT experiment with Cranelift after interpreter correctness; no undocumented compilation shortcuts.
- Gate: complex JVM/DEX fixtures and multithreaded app logic pass with correct JNI-independent behavior and no engine memory leaks.

### P3 — Rust JNI, ELF and native compatibility
- Implement JNIEnv/JavaVM tables in Rust with ABI-correct behavior; local/global/weak handles, attached native threads, strings/arrays, native registration, exceptions and runtime transitions.
- Implement/load needed native ELF/Bionic compatibility, correct filesystem and linker behavior, plus opt-in ABI translation as required for ARM-only APKs on x86_64 host.
- Gate: a real NDK app loads a `.so` and calls both directions through JNI correctly. Fuzz symbols and boundary cases. No claim that native apps are supported before this.

### P4 — App lifecycle and minimal useful Android APIs
- Implement an app manager with Activity/Task basics, intent delivery, resource/configuration handling, storage, permissions baseline, structured errors.
- Start an actual APK's Activity, reach a meaningful UI / event state, and navigate back/forward.
- Gate: an authentic Java/Kotlin Views APK launches, responds to input, has correct lifecycle and produces native or explicit reference-verified output.

### P5 — Offscreen rendering and direct agent observation
- Offscreen surface on demand, font/layout/resources, text tree, input, frame sequence and request-causal freshness.
- Match reference screenshots/semantics across densities, dark/light themes, keyboard and animations. Distinguish meaningful frame from black placeholder.
- Gate: agent code-change -> built APK -> native run -> interaction -> verifiable screenshot + assertions -> cleanup, without full Android boot.

### P6 — Real parallel workloads and isolation
- Shared-task mode, separate processes, secure per-app data namespaces, file descriptors, permissions, native library state, event routing, app resets, budget enforcement.
- Bench N=1/2/5/10/20 with separate apps and same-app cases. Validate no input/state leakage; report real capacity rather than promise unlimited sessions.
- Gate: multiple real apps concurrently receive the right input, produce independent frames and traces, and do not compromise host security.

### P7 — Broader app ecosystem
- Prioritize Compose, Flutter, React Native, WebView, native graphics, background services, notifications, media, camera and sensors on demand.
- Expand Android API-level support, split APKs, 32/64-bit / ARM native ABI strategy and dependency graph. Test Android permission/security fidelity and app lifecycle under stress.
- Gate: reproducible class-by-class compatibility score and realistic agent development loop successful on representative apps.

### P8 — High-compatibility / performance engineering
- Broader Android SDK/NDK API surface; managed runtime JIT/AOT, profile-driven class sharing and app template processes, efficient offscreen graphics, cheap reset/park, sandbox hardening.
- Validate complex third-party apps, larger test suites and appropriate CTS subsets where possible. Full CTS certification is a separate question and must not be implied from passing a limited subset.
- Gate: measured end-to-end performance, explicit compatibility coverage, stable regressions and no silent false positives.

### P9 — Optional production-grade distribution
- CI artifacts, reproducible builds, signed releases, SDK/MCP packages, language-server/IDE integration, Linux host installation, capacity planner, observability and sustained-run safety.
- Only after live compatibility, threat model, incident recovery and license compliance are credible.

## Parallel research tracks (use bounded experiments)

- **R-DEX:** survey public DEX instructions/spec and Rust interpreter libraries; build small correct DEX VM candidate.
- **R-JNI:** minimal `JavaVM`/`JNIEnv` ABI proof against compiled NDK fixture; investigate Bionic/ELF host/native ABI restrictions.
- **R-RENDER:** compare pure Rust rendering and Rust-owned graphics frontend to reference outputs; assess Compose/Flutter/RN dependencies.
- **R-API:** build Android API dependency map, choose first target API level, list must-implement `android.*` method flows for minimal Activity.
- **R-SANDBOX:** investigate unprivileged Linux namespace/security model and per-app process isolation.
- **R-ORACLE:** capture equivalent app interactions/evidence from existing ACVM without mutating its implementation.

Research must conclude with code or a falsifiable result, not just a speculative recommendation. Use time-boxes and explicit stop conditions for nonviable paths.

## Benchmarks: actual experimental design

Baseline **exact same APK, screen, fixture, density, state, CPU architecture, API level (when possible), image/frame freshness contract and test actions** across contenders. Compare:

1. Time host `acos verify` starts to first **correct** frame; warm and cold separately.
2. Time session/worker creation to first correct frame; app launch to first frame; screenshot retrieval latency with causal frame IDs.
3. `build -> install -> launch -> assert -> evidence -> reset -> cleanup` end-to-end wall time.
4. Memory: runtime fixed/core idle, marginal N=1/2/5/10/20, per-process PSS/USS and RSS, cgroup memory.current, anon, shmem, page cache, GPU buffers, swap/zram, host physical memory.
5. CPU: idle/parked, active frame producer, agent-interaction load, compiler JIT overhead; distinguish wall CPU and percent of one core.
6. UI tree with text; tap, type, scroll and navigation action-to-state latency.
7. Correctness: supported/unsupported/escalated coverage; per-app task pass, **false passes (must be zero)**, image and tree accuracy, crash classification.
8. Security/isolation: cross-app secret/state leakage, sandbox escapes, privileged host interfaces, mutable shared cache leaks.
9. Repeatability: first-run and warmed cache, at least 20 repetitions for latency (p50/p95), paired/interleaved A/B, full raw logs, resource guard records.
10. Composite metric **correctly verified tasks / GiB-minute**, paired with absolute task time and failure rate so high throughput doesn't conceal catastrophic latency.

**Do not mix browser-tab PSS with ACVM anonymous+shmem as if identical metrics**, or compare a simulated screenshot with an app-rendered one. All plots/results must label units, methodology and cold/warm state.

## Research targets (not promises)

| Metric | Reference from legacy ACVM | Initial native research target | Long-term stretch |
|---|---|---|---|
| Fixed idle core | W1 ~206 MiB candidate, still gated | measure actual Rust core (do not assume 0) | 100–150 MiB or lower if correctly compatible |
| Incremental active light session N=5 | ~79 MiB legacy isolated user; W7 11–22 MiB extra shared task (not isolated) | beat matched legacy on a real app task | 20–40 MiB light task, 30–60 MiB independently sandboxed if feasible |
| Cold first correct screenshot | ~3.62 s after v12 | improve matched workload | <=1 s aspirational |
| Worker create -> correct frame | ~0.52 s legacy | improve matched workload | <=0.2 s aspirational |
| Fresh screenshot | ~0.067 s legacy | <=0.067 s no regression | <=0.03 s aspirational |
| Reset-to-ready | ~0.75 s old D2 `--app` | <=0.75 s | <=0.2 s aspirational |
| UI tree full text | legacy expensive text mode | native semantic tree with no slow instrumentation subprocess | <=20 ms aspirational |
| Concurrent workloads | old N=5 scorecard, 10 additional displays created | independently demonstrate N=10 | N=20/50+ bounded by resources |
| End-to-end verification | legacy U2 baseline was in progress | set verified-task baseline | continual statistically meaningful improvement |

No target should override correctness, app compatibility, or untrusted code isolation. Browser-tab memory is a **reference for aspiration**, not a fair expectation for arbitrary native/Java apps.

## Security / host constraints

- Host is an actively used Arch Linux workstation with 60 GiB RAM and many other workloads; the old project experienced a severe OOM incident. Do not exceed **85% RAM usage**, respect MemAvailable and pressure, set per-job cgroup ceilings, a spawn lock, emergency cleanup, and bounded concurrency. If close to the guard, run CPU-light analysis instead of allocating more containers.
- Do not run arbitrary APKs in privileged Docker or as host root. Use unprivileged namespaces/VM-backed isolation for untrusted workloads, independently audited. The old ACVM's `--privileged` dependency was a security gate failure.
- No `sudo`, kernel changes, `swapoff`, device configuration, hard resets, deletion of old worktrees, or multi-GB dependencies without explicit user authorization. If a step is blocked, document and continue with safe work.
- Do not modify unrelated user files, desktop services, hosts' swap settings or Linux kernel state. New source writes restricted to `~/Projects/acos` except explicit approved artifact paths.
- Confirm downloads and licenses; inspect file sizes. For major (>2 GB) AOSP/SDK/system-image downloads ask permission or use already installed resources.
- Preserve provenance: test source signatures, native ABI library inventory and app user data confidentiality; never upload user APKs/private projects without approval.

## Agent working cadence

- New lead agent can delegate bounded Rust VM, JNI, rendering, sandbox and benchmark tasks to independent workers.
- Maintain `STATUS.md` and `TASKS.md`, brief decision records, and an experiment log containing failures too.
- Every accepted change is gated by unit tests, differential tests, compatibility corpus, security checks, and paired benchmarks. No regression disguised as progress.
- Progress autonomously on safe, authorized tasks. Tasks requiring permission (sudo, large download, external publication, destructive operations) are deferred while other tasks proceed.
- Avoid spending all time on AI/planning loops. Implement the smallest real executable slice early, then extend coverage systematically.
