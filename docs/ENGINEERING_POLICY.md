# ACOS engineering policy: reuse by default, replace only for massive gains (2026-11-11)

## Primary objective (final direction, 2026-11-11)
ACOS is an agent-first Android application execution and verification environment, not a replacement mobile OS. Run real Android APKs with the highest practical compatibility, fastest verification loops, lowest memory and minimum engineering effort. Agents install, launch, inspect, interact with, reset and verify apps quickly and concurrently. Final compatibility testing happens on a real emulator or physical device; ACOS gives high-fidelity development verification without reproducing unnecessary phone infrastructure.
Metric: maximum correctly verified app changes per second per GiB of RAM, with minimum engineering effort.
Default to reuse; replace only with compelling evidence. Reuse ladder per layer: stock -> minimal shims -> replacement only if justified. Use lightweight independent processes with isolated app state. Rendering infrastructure only when rendering is needed; audio/camera/media and other optional services initialise on demand. system_server: lightweight Rust services where practical, but do not insist on replacing a service when reuse is much cheaper. Rust VM and its tests are preserved, feature development frozen.

## Rewrite gate (evaluate benefit, effort, compatibility before any significant rewrite)
Thresholds: >=50% reduction in a major subsystem with meaningful whole-system savings, or >=25% whole-system improvement, or >=2x on a critical operation = strong candidate. (Final message was cut off after '>=25% improvement'; these thresholds are carried over from the earlier policy.)
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
