# Prompt for the new ACOS lead agent

Build **ACOS**, a **new clean-sheet Rust-first, agent-native Android execution and verification OS**, in **`~/Projects/acos`**. Read the complete handoff package (`00_START_HERE.md` through `06_SOURCES_AND_COMPATIBILITY.md`) before implementing.

**Old reference:** `/home/Qui/Work/android-container`. Inspect its docs, experiments, API/benchmarks and app test fixtures **read-only**. Preserve it unchanged as the Android behavior oracle and performance baseline. ACOS is a **new codebase and architecture**, not another redroid container iteration.

**Vision:** An AI coding agent changes code, builds an APK, executes only the necessary Android-compatible capabilities, controls optional offscreen UI, asserts behavior, captures fresh evidence and resets/cleans up **as fast and cheaply as possible**. One core should multiplex many apps/tasks without new Android users or screens by default. Preserve explicit data/isolation levels and correct Android semantics. No artificial five-task cap.

**Rust mandate:** Own the execution engine in Rust, **including DEX parsing/verification/execution, a Rust ART-compatible managed runtime with GC and eventual JIT/AOT, and the standard JNI ABI implemented in Rust**, not merely a Rust wrapper around C++ ART. Existing third-party native `.so` libraries remain guest inputs. Use external code-generation or graphics dependencies selectively with clear justification.

**Compatibility goal:** work toward running **all ordinary Android APKs across explicitly supported API levels/ABIs**, including Views, Compose, Flutter, React Native, WebView, native JNI, services and split APKs. This is a long-term goal, not a current promise. Never silently fake APIs or count fallback-to-ACVM as native support. Unsupported behaviors need explicit error/fidelity labels and, where appropriate, an opt-in Android reference fallback.

**Start immediately:** initialize the separate Git/Rust project, inventory the existing ACVM read-only, create technical specs/compatibility matrix and baseline harness, then build the smallest true Rust APK/DEX execution proof and progress toward an actual app Activity, correct input, live UI/frame, assertions and reliable evidence. Split bounded research across agents as resource limits allow, but implement and benchmark continuously. Track app coverage, correctness, memory, cold/warm start, screenshot, full dev-loop latency, CPU, reset, concurrency and verified tasks per GiB-minute.

Keep host RAM below **85%**, bound parallel workloads, avoid privileged/untrusted execution, never modify the old repo or unrelated host state, and defer sudo or multi-GB downloads until explicitly authorized. Continue safely on other tasks rather than blocking. Record every experiment, including failures; merge only tested, reproduced improvements.

**Do not stop after making diagrams or a toy demo.** Deliver a real, progressively compatible agent-first APK execution engine and prove each capability with genuine APK tests against the old ACVM/AOSP oracle.
