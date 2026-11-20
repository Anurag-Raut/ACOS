# ACOS engineering policy: reuse by default, replace only for massive gains (2026-11-11)

The founding ART direction (2026-11-11) is archived in
`legacy/docs/OWNER_DIRECTION_2026-11-11.md`. This file applies it alongside
`AGENTS.md` and `docs/internals/overview.md`.

## Primary objective (final direction, 2026-11-11)
ACOS is an agent-first Android application execution and verification environment, not a replacement mobile OS. Run real Android APKs with the highest practical compatibility, fastest verification loops, lowest memory and minimum engineering effort. Agents install, launch, inspect, interact with, reset and verify apps quickly and concurrently. Final compatibility testing happens on a real emulator or physical device; ACOS gives high-fidelity development verification without reproducing unnecessary phone infrastructure.

Metric: **correctly verified tasks per GiB-minute at N=10 concurrent agents** (total attributable PSS over wall time, with the verification result counted only when it passes).

Default to reuse; replace only with compelling evidence. Reuse ladder per layer: stock -> minimal shims -> replacement only if justified. Use lightweight independent processes with isolated app state. Rendering infrastructure only when rendering is needed; audio/camera/media and other optional services initialise on demand. system_server: lightweight Rust services where practical, but do not insist on replacing a service when reuse is much cheaper. Rust VM and its tests are preserved, feature development frozen.

## Preference ladder (AOSP first)
Use the first rung that works on the verification task:
1. Unchanged AOSP.
2. AOSP with configuration.
3. A small shim.
4. A subset of AOSP.
5. Custom code, only when justified.

Rules that go with the ladder:
- A few extra MiB of memory are acceptable to stay on AOSP.
- Eliminating `system_server`, SurfaceFlinger or kernel binder is not a goal.
- Do not spend time saving 5-10 MiB.
- Rust is not assumed to be faster than Google's code. Measure first.

## Rewrite gate (evaluate benefit, effort, compatibility before any significant rewrite)
A rewrite is a candidate if it meets one of these thresholds:
- at least 50% reduction in a major subsystem, with meaningful whole-system savings;
- at least 25% improvement in total memory or verified-task throughput;
- at least 2x on an important operation.

Also:
- A small gain from a simple, safe configuration change is accepted.
- A small gain (5-10%) from a complicated reimplementation is rejected.
- Correctness and compatibility fixes are exempt from the thresholds. They are not optional because they save nothing.
- Architecture matters more than implementation language.
- Android behaviour stays correct: verified with real APKs, not isolated benchmarks. Lower-fidelity shortcuts must be an explicitly labelled mode.
- If an experiment cannot clear the gate, stop it and reuse the existing implementation.

## Component defaults (owner, 2026-11-11)

| Component | Default | Notes |
|---|---|---|
| ART, AOT/JIT | Reuse Google | Real `dalvikvm64` from the API-34 image |
| Framework classes | Reuse Google | `ActivityThread`, `Instrumentation`, `Application`, `Activity` run as the framework builds them |
| Bionic / linker | Reuse Google | App namespaces come from `PathClassLoader` + libnativeloader |
| JNI | Reuse Google | |
| libandroid_runtime | Reuse Google | Stock library; registration shims only where a native is missing |
| libbinder / Parcel | Reuse Google | |
| HWUI / Skia | Reuse Google | CPU Skia path first; `acos-render` only where measured to win or needed for compatibility |
| ViewRootImpl | Reuse Google | |
| Choreographer | Reuse Google | ACOS supplies the vsync source |
| AssetManager / overlays | Reuse Google | Static overlays through stock `ApkAssets`/idmap2 |
| Binder transport | Keep the ACOS adapter | Userspace driver emulation to the Rust `ServiceHost`; optimise only if measured |
| System services | Evaluate stock vs lightweight, per service | Rust serves only the transactions a test hits; unknown ones return a structured `UNSUPPORTED_*` error |
| Window / surface | Investigate AOSP components before expanding custom code | |
| Sessions | ACOS-specific (Rust) | Independent processes with isolated app state |
| Observation / verification | ACOS-specific | Frame, tree, screenshot and verdict tooling |
| Sandbox | Linux isolation plus ACOS orchestration | bwrap namespaces, plus the ACOS launcher and watchdog |
| Rust DEX VM | Frozen | Reference and explicit fallback only; no feature work |

Notes on specific rows:
- **Zygote:** adapt. Real ART with a correctly initialised lightweight fork (warm pool), only after the M-C milestone and only if it clears the gate.
- **Graphics:** the earlier plan used a software path into `acos-render`. The current choice is stock libhwui with the CPU Skia path first. `acos-render` stays as the oracle-tuned fallback.
- **Binder protocol:** reuse. Replace only the transport (socket to the Rust `ServiceHost`). Do not build a larger `/dev/binder` emulation than the `BC_*`/`BR_*` commands the apps hit. A fake `/dev/binder` inside bwrap is not viable.

## Real Android behaviour
- The real `ActivityThread`, `ViewRootImpl`, `Choreographer` and stock HWUI are essential. Verification on the real framework path is the product.
- The P3c-style chrome renderer is kept only as an explicitly labelled **PREVIEW** mode. A PREVIEW result never counts as real end-to-end verification.
- Unsupported features give a clear error or a capability report. No fake success and no silent fallback.

## Order of work
1. Prove real framework + counter.apk on ART first (M-A..M-C in docs/ART_RUNTIME_PLAN.md).
2. Profile the working system; find where memory actually goes.
3. Only then pursue gated optimisations: fixed baseline, warm-fork/shared pages, fast reset, parallel sessions.

Speculative optimisation is postponed. An optimisation starts only with a measured target. The priority order is in `docs/PLAN_V2.md` §6.

## Success metric
Benchmark N=10 simultaneous agents (total attributable PSS, verified tasks per GiB-minute), not just per-session memory.
Note: figures such as 78.1->33.2 MiB and the 280 MiB fixed baseline come from the legacy ACVM comparison; ART-path numbers must be re-measured.

## One-hour task rule (2026-11-11)
No task may run more than 60 minutes of autonomous execution without an explicit owner decision. Split phases into small, independently testable tasks; track elapsed time including retries; renaming or reassigning does not reset the clock; stop early if the approach is failing.

At 60 minutes, checkpoint safely and report: done, blocking, why it is hard, the smallest next action, and the evidence. Then ask for approval before more effort on that task. Do not auto-grant another hour. Continue other independent tasks meanwhile. Long-running tests stay bounded and resource-safe. No agent-day estimates or cumulative agent-day caps anywhere.

### Clarification (2026-11-11): at 60 minutes do not kill
An agent passing 60 minutes is treated as a problem, not a termination trigger. Do not cancel it. Notify the user immediately (PushNotification plus chat) with: why it is taking long (what it is doing, evidence from its activity), what exactly blocks it, and what the user or orchestrator can do to help (decision, approval, access, resources, split the task). The agent keeps running (or is steered to a safe checkpoint that preserves partial work) while the user decides. Never auto-grant or auto-kill.

## ART fallback policy (six-step procedure)
A missed deadline or failed framework integration does not abandon ART (standalone experiments already show major speed/memory wins). On a major blocker, in this order:
1. Identify the failing component.
2. Inspect Google's implementation (`docs/AOSP_REFERENCE.md`, `.cache/aosp/`).
3. Try a minimal adapter.
4. Compare strategies, including a hybrid.
5. Consider the legacy Android-container backend for capabilities not yet supported efficiently.
6. Present the alternatives and the evidence, with a recommendation, before changing the primary architecture.

Never silently fall back between engines. The Rust VM stays as reference and explicit fallback. Changing the primary architecture, or abandoning ART, needs the owner's approval (`docs/OWNER_DIRECTION_2026-11-11.md` §10).

## Avoid rebuilding Android
- Binder: keep the ACOS transport adapter. Do not commit to a larger `/dev/binder` emulation without verifying that the stock Binder/native object semantics are reusable. One ioctl (`BINDER_WRITE_READ`) is the clean hook.
- Graphics: stock libhwui/Skia CPU first; our Rust renderer only where it shows a compatibility or performance advantage.
- system_server: the goal is eliminating its resource cost, not eliminating it. Reuse a slice of Google's services if cheaper, safer and light.
- SurfaceFlinger and kernel binder: not goals (owner, 2026-11-11).

## Benchmarks
Use latest actual baselines from the Rust VM and the legacy container repo; never compare incompatible memory measurements. Record: fixed core memory, incremental per-app memory, total with N=10 agents, PSS/private/cgroup/host memory and graphics allocations, cold/warm start, first correct frame, screenshot and UI-tree latency, tap-to-state latency, reset/relaunch latency, verified tasks per GiB-minute, compatibility pass/fail. Headline: correctly verified Android dev tasks per GiB-minute at N=10. Real APKs only; no wins claimed from tiny Java programs or incomplete launches.

## AOSP reference
Agents read Google's source (docs/AOSP_REFERENCE.md, `.cache/aosp/`) before guessing or disassembling. Source is for understanding; we run the image's compiled binaries and do not copy AOSP source into the repo.
