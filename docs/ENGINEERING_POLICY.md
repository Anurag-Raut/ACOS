# ACOS engineering policy: reuse by default, replace only for massive gains (2026-11-11)

## Primary objective (final direction, 2026-11-11)
ACOS is an agent-first Android application execution and verification environment, not a replacement mobile OS. Run real Android APKs with the highest practical compatibility, fastest verification loops, lowest memory and minimum engineering effort. Agents install, launch, inspect, interact with, reset and verify apps quickly and concurrently. Final compatibility testing happens on a real emulator or physical device; ACOS gives high-fidelity development verification without reproducing unnecessary phone infrastructure.
Metric: maximum correctly verified app changes per second per GiB of RAM, with minimum engineering effort.
Default to reuse; replace only with compelling evidence. Reuse ladder per layer: stock -> minimal shims -> replacement only if justified. Use lightweight independent processes with isolated app state. Rendering infrastructure only when rendering is needed; audio/camera/media and other optional services initialise on demand. system_server: lightweight Rust services where practical, but do not insist on replacing a service when reuse is much cheaper. Rust VM and its tests are preserved, feature development frozen.

## Rewrite gate (evaluate benefit, effort, compatibility before any significant rewrite)
Thresholds: >=50% reduction in a major subsystem with meaningful whole-system savings, or >=25% whole-system improvement, or >=2x on a critical operation = strong candidate. Also: >=25% improvement in total memory or verified-task throughput; small gain from simple safe configuration acceptable; small gain from complicated reimplementation rejected; compatibility/correctness fixes allowed without a perf gain. Architecture matters more than implementation language.
1. Massive gain: >=25% of a whole-system metric (total core or multi-session memory), >=50% of a major subsystem, or >=2x on a critical operation. 5-10% from a complicated rewrite: reject. Small gain from a safe config change: accept.
2. Fast to get: prefer configuration, reuse and small architectural changes over months of reimplementation.
3. Android behaviour stays correct: verified with real APKs, not isolated benchmarks. Lower-fidelity shortcuts must be an explicitly labelled mode.
Exception: correctness/compatibility work is not optional because it saves nothing.
If an experiment cannot clear the gate, stop it and reuse the existing implementation.

## Reuse vs replace
| Component | Decision |
|---|---|
| ART, bionic/linker, framework classes | Reuse Google |
| Binder protocol | Reuse; replace transport only (socket to Rust ServiceHost), optimise only if measured |
| system_server, SurfaceFlinger, audio/camera/media | Not started; Rust services supply only the behaviour a test needs, on demand |
| Zygote | Adapt: real ART with a correctly initialised lightweight fork (warm pool, after M-C) |
| Graphics | Software path into acos-render; measure memory before optimising |
| Session management | Custom (Rust) |

## Order of work
1. Prove real framework + counter.apk on ART first (M-A..M-C in docs/ART_RUNTIME_PLAN.md).
2. Profile the working system; find where memory actually goes.
3. Only then pursue gated optimisations: fixed baseline, warm-fork/shared pages, fast reset, parallel sessions.

## Success metric
Benchmark N=10 simultaneous agents (total attributable PSS, verified tasks per GiB-minute), not just per-session memory.
Note: figures such as 78.1->33.2 MiB and the 280 MiB fixed baseline come from the legacy ACVM comparison; ART-path numbers must be re-measured.

## One-hour task rule (2026-11-11)
No task may run more than 60 minutes of actual execution without explicit justification and user approval. Split phases into small independently testable tasks; track elapsed time including retries; renaming/reassigning does not reset the clock; stop early if the approach is failing. At 60 min pause, preserve partial work/evidence, and report: what was done, exact blocker, evidence, why no existing implementation can be reused, smallest next step, why more time is worthwhile. Do not auto-grant another hour; continue other independent tasks. Long-running tests stay bounded and resource-safe. No agent-day estimates or cumulative agent-day caps anywhere.

## ART fallback policy
A missed deadline or failed framework integration does not abandon ART (standalone experiments already show major speed/memory wins). When blocked: identify the failing component; inspect Google's implementation; evaluate a minimal shim; compare with a hybrid; consider the legacy Android-container backend for capabilities not yet efficiently supported; present alternatives and evidence BEFORE changing the primary architecture. Never silently fall back between engines. Rust VM stays as reference and explicit fallback.

## Avoid rebuilding Android
Binder: do not commit to a large /dev/binder emulation without verifying that stock Binder/native object semantics are reusable. Graphics: stock libhwui/Skia CPU first; our Rust renderer only where it shows a compatibility or performance advantage. system_server: the goal is eliminating its resource cost, not eliminating it; reuse a slice of Google's services if cheaper, safer and light.

## Benchmarks
Use latest actual baselines from the Rust VM and the legacy container repo; never compare incompatible memory measurements. Record: fixed core memory, incremental per-app memory, total with N=10 agents, PSS/private/cgroup/host memory and graphics allocations, cold/warm start, first correct frame, screenshot and UI-tree latency, tap-to-state latency, reset/relaunch latency, verified tasks per GiB-minute, compatibility pass/fail. Headline: correctly verified Android dev tasks per GiB-minute at N=10. Real APKs only; no wins claimed from tiny Java programs or incomplete launches.
