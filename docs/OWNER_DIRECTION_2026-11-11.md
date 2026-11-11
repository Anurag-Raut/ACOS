# Owner direction, 2026-11-11 (consolidated)

This file records the owner's consolidated direction of 2026-11-11. The other
docs (`docs/ENGINEERING_POLICY.md`, `docs/PLAN_V2.md`, `STATUS.md`,
`docs/OPERATING_DIRECTIVES.md`) apply it. If one of them conflicts with this
file, this file wins until the owner changes it.

## 1. What ACOS is

- An **agent-first Android APK execution and verification environment**. It is
  not an operating system.
- Primary metric: **correctly verified tasks per GiB-minute at N=10 concurrent
  agents**.

## 2. Preference ladder (AOSP first)

Use the first rung that works:

1. Unchanged AOSP.
2. AOSP with configuration.
3. A small shim.
4. A subset of AOSP.
5. Custom code, only when justified.

Rules that go with the ladder:

- A few extra MiB of memory are acceptable to stay on AOSP.
- Eliminating `system_server`, SurfaceFlinger or kernel binder is **not a goal**.
- Do not spend time saving 5-10 MiB.
- Rust is **not assumed** to be faster than Google's code. Measure first.

## 3. Component defaults

| Component | Default |
|---|---|
| ART, AOT/JIT | Reuse Google |
| Framework classes | Reuse Google |
| Bionic / linker | Reuse Google |
| JNI | Reuse Google |
| libandroid_runtime | Reuse Google |
| libbinder / Parcel | Reuse Google |
| HWUI / Skia | Reuse Google |
| ViewRootImpl | Reuse Google |
| Choreographer | Reuse Google |
| AssetManager / overlays | Reuse Google |
| Binder transport | Keep the ACOS adapter |
| System services | Evaluate stock vs lightweight, per service |
| Window / surface | Investigate AOSP components before expanding custom code |
| Sessions | ACOS-specific |
| Observation / verification | ACOS-specific |
| Sandbox | Linux isolation plus ACOS orchestration |
| Rust DEX VM | Frozen (reference and explicit fallback only) |

## 4. Rewrite thresholds

A rewrite is a candidate only if it meets one of these:

- at least 50% of a subsystem, with a meaningful overall benefit;
- at least 25% of total memory or throughput;
- at least 2x on an important operation.

Correctness fixes are exempt from these thresholds.

## 5. Real Android behaviour

- The real `ActivityThread`, `ViewRootImpl`, `Choreographer` and stock HWUI are
  essential. Verification must run on the real framework path.
- The P3c-style chrome renderer remains only as an **explicitly labelled PREVIEW
  mode**. A PREVIEW result never counts as real end-to-end verification.
- Unsupported features give a clear error or a capability report. No fake
  success.

## 6. One-hour rule

- No agent-day estimates. No agent-day caps.
- A task gets at most 60 minutes of autonomous work.
- At 60 minutes: checkpoint safely, then report **done / blocking / why it is
  hard / the smallest next action**, and **ask for approval** before more effort
  on that task.
- Never kill an agent abruptly. Preserve partial work first.

## 7. Priority order

- **P0, real APK running:** provider binder; `Application` and `Activity`
  `onCreate`/`onResume`; real window lifecycle; `ViewRootImpl` + surface +
  `Choreographer`; a live frame.
- **P0, interactive loop:** tap and key input; counter 0 -> 1; fresh screenshot;
  live view tree; reset, close and relaunch; `acos verify --engine art`.
- **P0, host protection:** per-run data; process-tree cleanup and orphan
  detection; enforced memory limits; confinement. **No untrusted APK runs before
  the safety tests pass.**
- **P1, independent sessions.**
- **P1, real benchmarks:** ART vs Rust VM vs legacy at N = 1, 5, 10 and 20.
- **P2, efficiency** (on-demand services, graphics allocations, shared memory,
  warm-fork, AOT/JIT) **and compatibility** (Views/Compose, React Native, Gaten,
  Flutter). P2 work starts only after measurements exist.
- Postpone speculative optimisation.
- Never silently abandon ART or change engines.

## 8. Procedure for a major blocker

1. Identify the failing component.
2. Check Google's AOSP implementation (`docs/AOSP_REFERENCE.md`).
3. Try a minimal adapter.
4. Compare strategies, including a hybrid.
5. Consider the legacy Android-container backend for capabilities ACOS does not
   yet support.
6. Present the evidence and a recommendation to the owner.

## 9. Agent coordination

- File ownership and worktrees: one worktree per independent workstream. Each
  agent names the files it owns and the files it must not touch.
- Preserve unknown edits. Never overwrite or revert changes an agent did not
  make.
- Keep the docs consistent with the real status.

## 10. Ask the owner before

- replacing a substantial working Google component;
- committing to a complex new binder, window or graphics implementation;
- an architecture with large memory overhead;
- a major fidelity compromise;
- a significant isolation-model change;
- abandoning ART;
- deleting valuable work;
- spending more than one hour on a blocked task.

Escalation format:

- **Problem:**
- **Existing AOSP option:**
- **ACOS option:**
- **Tradeoff:**
- **Recommendation:**

## 11. Decisive test

`counter.apk` -> real `Activity` on ART -> UI -> view tree -> tap Increment ->
the counter goes from 0 to 1 -> fresh screenshot -> verify -> reset -> rerun.

It runs on real ART. There is **no Rust-VM fallback** and **no offline-render
shortcut**.

## 12. Progress reports

### Component table

Every major progress report uses this table:

| Component | Google/Rust/Hybrid | Status | Working now | Missing | Priority |
|---|---|---|---|---|---|

Status values, strictly:

- **DONE:** exercised on the real ART path by a real APK run, with a result file
  in `results/`, and nothing in its required scope is missing.
- **PARTIAL:** exercised on the real ART path by a real APK run, with a result
  file, and some of its required scope is missing.
- **BLOCKED:** a real APK run reaches it and fails at a named cause recorded in
  `results/`.
- **NOT STARTED:** no real APK run on the ART path touches it.

Library loading and isolated tests are **not DONE**. They are listed under
"Working now" with their evidence, and the status stays at the level the real
path supports.

Progress reports give no percentages.

### Seven standard questions per major update

The direction text says these questions exist but does not list them. The list
below is the working definition. The owner should confirm or replace it.

1. What runs on a real APK on the ART path now, and which result file shows it?
2. What is the exact blocker: component, cause, and file and line if known?
3. What changed since the last report: commits and result files?
4. Which tasks are past 60 minutes, and what is each one's checkpoint state?
5. Which owner gate in section 10, if any, is needed before the next step?
6. What is the smallest next action, and which command verifies it?
7. What are the host safety numbers: peak RSS, host RAM use, orphaned processes?

## 13. Where each part lives

| Item | File |
|---|---|
| Objective, reuse rules, rewrite gate, component defaults, benchmarks | `docs/ENGINEERING_POLICY.md` |
| Phases, priority order, agent policy | `docs/PLAN_V2.md` |
| Current component table, evidence, open gaps | `STATUS.md` |
| Operating rules, owner gates, escalation | `docs/OPERATING_DIRECTIVES.md` |
