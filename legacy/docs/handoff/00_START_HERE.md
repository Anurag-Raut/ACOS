# ACOS — Agent-Native Android Execution OS

**Handoff package — 2026-11-10**

## Identity and locations

- **New project:** `~/Projects/acos` (expand home using the local user's actual home). This is the sole write target for the new work.
- **Existing project/reference:** `/home/Qui/Work/android-container` — inspect **read-only** initially. Do not delete, reset, or re-architect it and do not treat it as the scaffold for ACOS.
- **New lead agent:** establish an independent architect/orchestrator for ACOS. Existing ACVM agents may answer bounded research questions, but must not mix branches, worktrees, project state, benchmarks, or runtime resources with the new project.
- **Name:** ACOS; working description: *Rust-first, agent-native Android application execution and verification OS*. ACVM remains the existing reference engine.

## Non-negotiable product intent

An AI coding agent should be able to modify Android app code, produce an APK, run only the minimum environment necessary, inspect the live screen and state, interact, assert the intended behavior, receive reliable evidence, and release the resources, without manually operating an entire simulated phone. This is a **task-first execution system**, not a phone emulator with a new coat of paint.

**North-star metric:** verified Android development tasks / (GiB of RAM × minute), gated by correctness and compatibility. Also track task wall-clock time, memory, CPU, cold/warm latency, number of independent concurrent workloads, isolation, and false-pass rate.

**Long-term compatibility objective:** run ordinary third-party Android APKs from the supported Android API levels and ABIs, unchanged, including Java/Kotlin, Views/Compose, Flutter, React Native, WebView, JNI/native and multi-process apps. Aim to expand toward the complete Android app ecosystem. This is an extremely ambitious long-term objective, **not** a present capability or a guarantee that proprietary GMS, DRM, Play Integrity, device-specific hardware, privileged OEM APIs, or every Android version will work without their dependencies. Never fake a pass when a feature is unimplemented.

**Rust mandate:** the managed-runtime **implementation itself** must be Rust: DEX decoding, verification, interpreter, eventual JIT/AOT frontend, class loader, object model, GC, threading, JNI function tables / native bridge, and runtime observability. Do **not** simply embed Google's C++ `libart.so` and call that the native ACOS engine. Native ELF/JNI libraries written in C/C++ remain valid guest inputs and will call an ABI implemented by Rust. The Linux kernel, host graphics drivers, and third-party native dependencies need not be rewritten in Rust. Any non-Rust dependency needs an explicit portability/performance/license rationale.

**Do not automatically allocate:** Android users, devices, displays, GUI windows, system servers, WebViews, HALs, cameras, Bluetooth, location, notification UIs, per-session VMs, or full Android boot. Create only what the task's semantics demand.

**One shared core, many tasks:** share immutable runtime/framework code and caches across distinct app workers. A worker can be headless, display-backed, share process state by explicit consent, or have separate process/data isolation. More than five concurrent workloads is a design requirement; there is **no hard-coded five-session ceiling**. Real capacity is constrained by CPU, RAM, rendering, app compatibility and the actual isolation mode. Independently verify N=1,2,5,10,20 and beyond when safe.

**Compatibility before speed claims:** a fast mock is not a substitute for correct real-device execution. Report exact fidelity of evidence: static preview, simulated behavior, native runtime, AOSP-backed oracle, or physical device.

## Reading order

1. `01_PHILOSOPHY_AND_REQUIREMENTS.md` — product contract and capability/task matrix
2. `02_ARCHITECTURE_AND_RUST_RUNTIME.md` — clean-sheet modular Rust design
3. `03_AGENT_WORKFLOWS_AND_API.md` — extensive agent-centric examples, CLI, scenario format and verification loop
4. `04_ROADMAP_AND_ACCEPTANCE.md` — research gates, implementation phases, benchmark science and safety
5. `05_OLD_ACVM_REFERENCE.md` — measured legacy results, valuable decisions, pitfalls and the exact old directory
6. `06_SOURCES_AND_COMPATIBILITY.md` — research references, standards, licensing and compatibility boundaries
7. `AGENT_KICKOFF.md` — short prompt to paste to the new lead agent

## Immediate first actions for the new agent

1. Verify both paths exist and inspect the existing ACVM docs, design, tests and measured metrics **without mutating ACVM**.
2. Initialize a new Git repository and Rust workspace under `~/Projects/acos`; write `SPEC.md`, `ARCHITECTURE.md`, `DECISIONS.md`, `COMPATIBILITY.md`, `BENCHMARKS.md`, `STATUS.md` and `TASKS.md` derived from these documents.
3. Choose and explicitly document the first target API level and CPU ABI based on the existing app corpus; don't claim broad support without evidence.
4. Build a real DEX/APK loader + executable interpreter proof-of-concept in Rust. Exercise **compiled actual DEX code**, not an invented instruction language. Establish differential tests against a known Android runtime.
5. Define the stable `acos` CLI/JSON/MCP contract and run one real end-to-end agent loop as soon as feasible. An initial result may only be a limited APK subset; report the subset accurately.
6. Run bounded work in parallel when safe. Never exceed 85% physical RAM usage, ignore the original host's permission requirements, or start root-requiring / multi-GB operations without explicit authorization. If blocked, do other tasks.
7. Log experiments, regressions, false passes and reproducible data. Every implementation milestone must improve a verified capability or performance metric.

**Design constraint:** An ambitious from-scratch architecture is approved, but architecture diagrams and synthetic DEX demos do not count as an Android app being supported. The work must repeatedly demonstrate real APKs running correctly and provide auditable agent-generated evidence.
