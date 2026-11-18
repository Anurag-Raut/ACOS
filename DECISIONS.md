# ACOS — Decision log (ADRs)

Status: accepted decisions by the project lead, recorded here with rationale.
Dates use the project clock (2026-11-10). Each ADR lists context, decision,
rationale, consequences and any open question. New ADRs are appended; do not
rewrite accepted ones (supersede instead).

---

## ADR-0001 — Clean-sheet Rust workspace; managed runtime implemented in Rust

**Status:** accepted.

**Context.** The legacy engine (`/home/Qui/Work/android-container`, "ACVM") is a
privileged-redroid container orchestrator, frozen at `aosp-track-final`. The
handoff (`docs/handoff/02`, `AGENT_KICKOFF.md`) mandates a Rust-owned engine and
forbids embedding C++ ART. The latest legacy commit also records a user decision
to build an "own lightweight Android testing runtime" rather than keep stripping
AOSP.

**Decision.** ACOS is a **new, clean-sheet Rust workspace**. The managed runtime
— DEX parse/verify/interpret, object model, GC, class loader, threading, JNI
function tables/native bridge — is implemented in Rust. There is **no
`libart` embedding**. Native ELF/C/C++ guest libraries remain valid inputs and
call the Rust-owned standard JNI ABI. Linux kernel, host graphics drivers and
third-party native dependencies need not be Rust.

**Rationale.** The product requirement is ownership of the execution engine, not
a wrapper. AOSP is a specification/behaviour reference, not a mandatory guest
system. Legacy ACVM is a compatibility oracle, benchmark baseline and research
archive — not the runtime architecture.

**Consequences.**
- Very large multi-stage systems effort; compatibility is a live ledger, not a
  headline (`COMPATIBILITY.md`).
- Any non-Rust dependency needs an explicit portability/performance/licence
  rationale.
- Diagrams and synthetic DEX demos do not count as app support; real APKs must
  run.

**Open question.** None on the mandate. Bounded use of Rust code-generation
(Cranelift) is explicitly an optimisation layer *after* interpreter correctness
(see `docs/handoff/02`).

---

## ADR-0002 — First target: Android API 34, x86_64; DEX 035–039

**Status:** accepted.

**Context.** The host is x86_64 with the Android SDK installed. The SDK has an
`android-34` platform jar (`~/Android/Sdk/platforms/android-34/android.jar`) and
an `android-34/google_apis/x86_64` system image, so an ART-on-Android-34
reference oracle is possible later (deferred, see ADR-0005). The legacy corpus
targets API 34.

**Decision.**
- **First target API level = Android 34**; compile fixtures against the
  `android-34` platform jar; the SDK's `android-34` x86_64 system image is the
  future reference oracle.
- **First CPU ABI = x86_64** (host-native). ARM translation is **deferred**.
- The DEX parser accepts **DEX versions 035–039**.

**Rationale.** API 34 matches the existing fixture corpus and installed SDK;
x86_64 is host-native and removes translation from the first proof; accepting
035–039 covers the realistic DEX version range without premature scope.

**Consequences.**
- Apps whose only native libraries are ARM-only are `WRONG_ABI` until a
  translator exists (explicitly reported, never faked).
  *Superseded in part by ADR-0018 update (W59-A, 2026-11-11):* the APK-level
  ABI gate now reports `UNSUPPORTED_ABI` naming every ABI the APK provides (see
  also the E1 error-contract note in ADR-0018). `WRONG_ABI` remains only for a
  single ELF whose `e_machine` the in-process loader cannot map. The original
  text is kept for history.
- API-level support is declared per task; "supports Android" is not claimed.
- Compiling fixtures uses the SDK tools only as build tools (ADR-0004).

**Open question.** When to add arm64-v8a translation, and whether to add a
higher API level first — driven by the fixture corpus.

---

## ADR-0003 — Smallest cohesive initial crate set

**Status:** accepted.

**Context.** The handoff suggests a large target workspace but warns: "Do not
generate dozens of placeholder crates merely to fill a diagram."

**Decision.** Start with the smallest cohesive set and split only when
interfaces stabilise:

1. `acos-dex` — parse + verify + decode DEX.
2. `acos-apk` — ZIP/APK, manifest, resources.
3. `acos-vm` — interpreter + heap (added later, once dex/apk interfaces are
   stable).
4. `acos-cli` — the `acos` binary and typed JSON contract.

**Rationale.** Minimises parallel interface churn across concurrent agents and
keeps the first real execution slice small and testable.

**Consequences.** The target subsystem map in `ARCHITECTURE.md` §2 is a roadmap,
not an initial scaffold. Crate splits are justified by stable interfaces and
measured need.

**Open question.** None.

---

## ADR-0004 — External build tools allowed, never in the execution path

**Status:** accepted.

**Context.** The legacy fixtures are built with `javac`, `d8`, `aapt2`,
`zipalign`, `apksigner` directly from the local SDK (`helper/lib/common.sh`). The
handoff explicitly permits standard host tools as build tools.

**Decision.** `javac`/`d8`/`aapt2` (and the rest of the local Android build
tools) are allowed **as build tools for fixtures only**. They are **never** in
the ACOS execution path. The runtime that executes an APK remains Rust-driven.

**Rationale.** Build tooling is not the product; reimplementing a DEX compiler
now would delay real execution. It also keeps fixture provenance reproducible.

**Consequences.** `fixtures/build/` may depend on the local SDK (documented and
pinned). The runtime must not shell out to `d8`/`aapt2`/`javac` to execute or
"just-in-time compile" DEX. Build-tool versions must be recorded with fixtures.

**Open question.** Whether to vendor/pin exact build-tool versions in a
manifest. (Recommended: yes, record version + hash per fixture.)

---

## ADR-0005 — Oracle tiers and mandatory fidelity labels

**Status:** accepted; **updated 2026-11-10** — tier 2 (`art-reference`) is now
authorised on the installed API 34 x86_64 emulator image (see ADR-0015).

**Context.** Correctness must be judged against a reference. The legacy engine
and the API-34 system image are available but heavy; the host has OpenJDK 17.
The host is an actively used workstation with a 60 GiB RAM and an 85 % guard.

**Decision.** Two oracle tiers, each result carrying an explicit fidelity label:

1. **`jvm-reference` (weak oracle):** host OpenJDK 17 running the same Java
   source. Valid only for pure-Java semantics (arithmetic, data structures,
   standard-library behaviour). It does **not** model ART, Android framework,
   resources, JNI or rendering.
2. **`art-reference` (authoritative):** real ART on Android 34. **Authorised
   2026-11-10**: a dedicated headless oracle emulator built from the installed
   `android-34/google_apis/x86_64` image, started on demand under the RAM guard
   (≤2 GiB guest RAM, one instance, always shut down). The frozen legacy ACVM
   core remains a secondary reference backend. See ADR-0015.

**Rules.**
- Every result must carry a fidelity label.
- A JVM match is **never** reported as Android-verified.
- A `jvm-reference` pass can never satisfy an `art-reference` requirement.
- The AOSP track is frozen (`aosp-track-final`); using it as a live oracle is an
  explicit, authorised, RAM-guarded action (see
  `docs/reference/ACVM_INVENTORY.md` §6).

**Rationale.** Differential testing needs a reference, but the strongest
reference is expensive and currently unauthorised. Labelling prevents the
classic "it matched the JVM, therefore it is Android-correct" error.

**Consequences.** Early differential tests are intentionally weak and must say
so. Pure-Java semantic fixtures are the first differential target; framework/
resource/JNI/rendering behaviour stays unverified until tier 2.

**Open question.** Resolved by ADR-0015 (the emulator is the authoritative
oracle tier; the legacy ACVM core stays a secondary reference). ADR-0015 also
governs which JVM-vs-ART differences must be recorded.

---

## ADR-0006 — Core-library strategy is OPEN (hand-written intrinsics first)

**Status:** accepted as an ordering recommendation; **resolved 2026-11-10 by
ADR-0007 as option (b)** — execute the real core-library DEX on the Rust VM with
Rust natives. The licence question remains open (ADR-0016).

**Context.** Executing real DEX requires `java.lang`/`java.util`/`javax.*` (and
eventually `android.*`) semantics. Two options:

- **(a) Hand-written Rust intrinsics** for a minimal `java.lang` subset first.
- **(b) Later load real core-library DEX** (e.g. AOSP `core-oj`/libcore) and
  execute it with the Rust VM plus Rust natives.

**Decision (updated 2026-11-10).** ADR-0007 superseded the (a)-now ordering:
ACOS goes straight to **(b)** — real core-library DEX on the Rust VM with Rust
natives — because reimplementing the framework is infeasible and fidelity-
critical. The original recommendation below is kept for history.

**Recommend (a) now → (b) as the scalable path.** Start with
hand-written Rust intrinsics for the smallest `java.lang`/`java.util` subset
needed by the first DEX proof. Design the class-loader/intrinsic boundary so
that (b) can replace or supplement intrinsics later.

**Rationale.** (a) gives a small, license-clean, testable first slice and avoids
a large DEX-execution dependency before the interpreter is correct. (b) is the
only realistic path to broad core-library coverage, but requires executing real
core-library DEX and reviewing licences.

**Consequences / open question.**
- **Licence question is OPEN:** AOSP core libraries are typically GPLv2 with the
  Classpath exception (per-component review required). Do not copy or link
  core-library code until reviewed (per `docs/handoff/06` licensing rules).
- Native `java.lang` methods must eventually be implemented as Rust natives; the
  boundary between "intrinsic" and "DEX-implemented core class" must be explicit
  and testable.
- Do not fetch AOSP sources or multi-GB artefacts without authorisation.

---

## ADR-0007 — Do NOT reimplement the Android framework; execute the real boot-classpath bytecode

**Status:** accepted (2026-11-10). Resolves ADR-0006 as option (b).

**Decision.** ACOS executes the **REAL Android 14 (API 34) boot-classpath
bytecode** on the Rust VM and supplies Rust implementations of the `native`
methods. The boot classpath is:
`core-oj`, `core-libart`, `okhttp`, `bouncycastle`, `apache-xml`, `framework`,
`framework-graphics`, `ext`, `telephony-common`, `voip-common`, `ims-common`,
`core-icu4j` — in the order given by the image's bootclasspath config (see
`research/framework-dex/REPORT.md` §Q2 and
`.cache/boot/android-34/BOOTCLASSPATH`).

**Rationale.**
- **Fidelity:** apps, AndroidX and Compose depend on exact framework behaviour,
  including internals reached by reflection. A rewrite would differ.
- **Size:** the boot classpath is **41,829 classes / 328,352 methods / 5,607
  `native` methods**. Reimplementing it is years of work and never exact.
- **Maintenance:** one implementation per API level, not a parallel universe.
- **No false results for the agent:** the real framework means real semantics.

**Evidence pointer.** `research/framework-dex/REPORT.md`: static native work-list
— `println` **152 natives lower bound / 663 CHA upper bound**; minimal Activity
**612 lower / 2,270 upper**; ~10 system services on the launch path; real DEX
v039 obtainable unprivileged from the installed image.

**Consequences.**
- The hand-written Rust core-library built-ins in `acos-vm` are a **TEMPORARY
  bootstrap** for the 20-program fixture corpus only; they will be replaced by
  the real core-library DEX + true natives.
- Selective Rust replacement of individual framework methods is allowed **only
  as a measured optimisation verified equal to the original** (same observable
  behaviour, differential-tested).
- The VM must load, verify and execute large platform DEX (boot classpath),
  with hiddenapi flags parsed (already supported by `acos-dex`).

---

## ADR-0008 — Replace everything below and around the framework with thin Rust

**Status:** accepted (2026-11-10).

**Decision.** No `system_server`, no Binder kernel driver, no SurfaceFlinger, no
HWUI/Skia, no zygote. The system-service interfaces reached by the app process —
`IActivityManager`, `IActivityTaskManager`, `IPackageManager`,
`IWindowManager`/`IWindowSession`, `IDisplayManager`, `IInputMethodManager`,
`IAccessibilityManager`, … — are answered **in-process by a Rust service layer
that starts only what a task needs**.

**Rationale.** The legacy fixed cost (206–322 MiB bare core) and per-session
cost (~88 MiB native heap/session, mostly the renderer) lived **there**, not in
the framework Java code. Executing the real framework on the Rust VM (ADR-0007)
plus a thin Rust service/rendering layer keeps the framework fidelity while
removing the legacy fixed overhead.

**Evidence pointer.** `research/framework-dex/REPORT.md` §Q4 (the reachable AIDL
interfaces and which are satisfiable by an in-process Rust stub);
`docs/reference/ACVM_INVENTORY.md` §2 (legacy memory split).

**Consequences.** ABI/Binder boundary semantics that an app can observe must be
modeled honestly; anything the Rust layer cannot answer is a structured
`unsupported`, never a silent stub that changes observable behaviour.

---

## ADR-0009 — VM design for the metrics

**Status:** accepted (2026-11-10).

**Decision.**
- Object references are **32-bit handles into a VM-owned arena heap** (enables
  compaction, JNI handle tables, and whole-heap snapshot/restore).
- Java calls use an **explicit VM-managed frame stack** — no host recursion;
  `StackOverflowError` is catchable; threads are VM-scheduled.
- **Precise GC** with a **GC-stress mode** that must pass the entire corpus.
- Classes come from **DEX or Rust built-ins behind one provider interface**.
- Natives are bound via a **registry keyed by `(class, name, signature)`**.

**Rationale.** These are the mechanical prerequisites for compaction, JNI
handles, snapshot/restore (ADR-0010), and accurate memory metrics. Handles +
arena make references relocatable; a VM frame stack makes overflow catchable and
decouples Java threads from host threads.

**Consequences.** The interpreter must never recurse into the host stack for a
Java call; JNI sees opaque handles (ADR-0014). GC-stress is a standing corpus
gate.

---

## ADR-0010 — Start-up and reset strategy = snapshot/restore

**Status:** **PLANNED, UNPROVEN** (2026-11-10).

**Decision.** Initialise the boot classpath (and later an app) once, snapshot the
heap + class state, and start/reset workers by restoring it (copy-on-write
sharing across workers). This is the planned answer to cold-start,
reset-to-ready and per-session memory targets.

**Gates (both must pass):**
1. framework initialisation time on the interpreter (measurable), and
2. correct restore of an initialised state (observable behaviour identical).

If either makes the cold-start or interaction targets unreachable, the
architecture is revisited — including the alternative in ADR-0012.

**Rationale.** Snapshot/restore is the only mechanism that plausibly reaches
reset-to-ready ≤0.75 s and per-session memory while starting from the real
framework. The handle/arena design (ADR-0009) is what makes it possible.

**Evidence pointer.** None yet (planned). Measurement will be recorded in
`results/` and `EXPERIMENTS.md`.

---

## ADR-0011 — Rendering: Rust software 2D backend

**Status:** accepted (2026-11-10).

**Decision.** A Rust software 2D backend (`crate acos-render`: tiny-skia
rasteriser + Rust text shaping, Android `fonts.xml`/typeface resolution,
`RenderNode`/display-list model, frame sequence + cause id + content hash,
damage tracking) is bound to the framework's `android.graphics` natives.
**No GPU, no display unless a task asks**; headless tasks allocate no surface.
Unsupported draw ops are **recorded per frame, never silently skipped**.

**Rationale.** The legacy renderer (SwiftShader/Vulkan ICD) was the dominant
per-session cost and forced a surface on every app. A software backend can
render offscreen on demand and produce a semantic tree plus pixels. Recording
unsupported ops keeps evidence honest (no black frames labelled success).

**Evidence pointer.** `research/framework-dex/REPORT.md` §Q3 (the `android.*`
graphics native work-list: `RenderNode` 54, `Paint` 23, `Canvas` 17,
`HardwareRenderer` 16 …); `docs/reference/ACVM_INVENTORY.md` §2 (renderer cost).

---

## ADR-0012 — Rejected for now: embedding Google's C++ ART (`libart`)

**Status:** rejected for now (2026-11-10); revisit trigger defined.

**Decision.** ACOS does **not** embed Google's C++ `libart` with a Rust system
layer.

**Rationale.** It would reach app compatibility sooner, but gives up control of
heap layout, sharing and snapshotting — which are exactly the levers for the
memory/start/reset metrics that are ACOS's reason to exist.

**Revisit trigger.** The gates in ADR-0010 fail, **or** interpreter interaction
latency cannot meet targets even with the JIT plan in ADR-0013.

---

## ADR-0013 — Performance plan for interpreted framework code

**Status:** accepted (2026-11-10).

**Decision.** In order: (1) pre-decoded instructions + resolved-reference
caches; (2) snapshot to avoid init cost (ADR-0010); (3) Rust intrinsics for hot
leaf methods **proven equal**; (4) JIT/AOT (Cranelift candidate) **only after
interpreter correctness**. Raw compute speed versus ART is a known gap to be
**measured and reported, not hidden**.

**Rationale.** The framework is large; correctness first, then measured
optimisation. The metrics are dominated by start/reset/memory, but interaction
latency still needs a plan.

---

## ADR-0014 — Native code: Rust loader/ABI, structured unsupported

**Status:** accepted (2026-11-10).

**Decision.** Guest NDK libraries (Bionic-targeted ELF, **x86_64 first**) are
loaded by a Rust loader (`crate acos-native-loader`) with an explicit,
auditable Bionic-compat symbol table. Missing symbols are a structured
`UNSUPPORTED_NATIVE_SYMBOL` error, **never a silent stub**. `JNIEnv`/`JavaVM`
tables are implemented in Rust against the VM (`crate acos-jni`, planned).
**ARM-only libraries are `WRONG_ABI` for now**; translation deferred.
*Superseded in part by the ADR-0018 update (W59-A, 2026-11-11):* when the APK
ships native code but no host-ABI `lib/<abi>/`, the launch gate and the daemon
RN/Flutter backends return `UNSUPPORTED_ABI`; `WRONG_ABI` is the loader-level
per-ELF `e_machine` mismatch only.

**Rationale.** ADR-0007 requires real `native` behaviour for the real framework;
ADR-0009's handle design makes the JNI reference tables tractable. Silent stubs
would create false results.

**Evidence pointer.** `fixtures/apps/hello-jni` (the V1 JNI fixture:
`NewStringUTF`, array access, callbacks, `RegisterNatives`, `ThrowNew`).

---

## ADR-0015 — Oracle: real ART on the installed API 34 x86_64 emulator

**Status:** accepted; lead-authorized 2026-11-10.

**Decision.** Authoritative expectations come from **real ART on the installed
API 34 x86_64 emulator image** (`fixtures/expected-art`, `fixtures/reference`),
started **headless on demand** under the RAM guard (lead-authorized: **≤2 GiB
guest RAM, one instance, always shut down**). Host-JVM outputs remain the weak
**`jvm-reference`** tier (ADR-0005). JVM-vs-ART differences are recorded in
`results/oracle/jvm-vs-art.md`.

**Evidence pointer.** `tools/oracle/emulator.sh`; `results/oracle/*`. The
emulator was booted once (22 s) during the spike and is shut down.

**Rationale.** The real framework may behave differently from the JVM in ways
apps depend on; only real ART can adjudicate. The emulator is available with no
download and fits the RAM guard at 2 GiB.

---

## ADR-0016 — Boot-classpath sourcing and licence posture

**Status:** accepted for development; **OPEN for human/legal review before any
distribution**.

**Decision.** ACOS **never bundles or redistributes** Android/OpenJDK bytecode.
It loads **unmodified jars extracted from the user's own installed SDK system
image at runtime** (`tools/extract-bootclasspath.sh` → `.cache/boot/android-34`,
gitignored).

**Licence facts (not legal advice).** `framework` is Apache-2.0; `core-oj`
contains GPLv2 **+ Classpath-exception** OpenJDK material; ICU/okhttp/
bouncycastle/conscrypt are per their licences.

**OPEN for human/legal review before any distribution:**
1. whether supplying **replacement `native` implementations** interacts with the
   Classpath exception (ACOS supplies natives for the very classes it loads), and
2. the fact that production (`user`) images may ship **dex-stripped** boot jars;
   only a `userdebug` image was tested here.

**Rationale.** A runtime-only, user-provided-image posture avoids ACOS
triggering redistribution obligations today, while flagging the two genuine
uncertainties. Does **not** block development.

**Evidence pointer.** `research/framework-dex/REPORT.md` §Q1/§Q5.

---

## ADR-0017 — DEX loading follows Android load semantics, not stricter

**Status:** accepted (2026-11-10); fix in progress at time of writing.

**Decision.** ACOS follows **Android load semantics** in `acos-dex`: a **SHA-1
signature mismatch is accepted by default** and reported as
`signature_valid=false`; **Adler-32 is enforced**; a **strict mode** exists
(`ParseOptions::STRICT`).

**Rationale / finding that motivated it.** Android does not verify the DEX SHA-1
signature at load time, and platform hiddenapi tooling rewrites boot dex without
updating it. `acos-dex` initially rejected every boot-classpath dex with
`DEX_BAD_SIGNATURE`; matching Android's actual behaviour is required to load the
real boot classpath at all.

**Evidence pointer.** `crates/acos-dex/src/lib.rs` (`ParseOptions`,
`signature_valid`) and `crates/acos-dex/tests/boot_jars.rs`
(`signature_mismatches_accepted`); finding recorded in
`docs/FINDINGS.md` and `EXPERIMENTS.md`.

---

## Cross-cutting host-safety decision (applies to all ADRs)

- **RAM guard 85 %.** Never exceed 85 % of physical RAM; admission checks and
  per-job limits are enforced. Host is shared with other workloads.
- **No privileged containers.** No `sudo`, no kernel/sysctl/global-host changes.
- **No downloads > 2 GB without explicit authorisation** (AOSP, system images,
  Cuttlefish, …).
- **No writes outside `~/Projects/acos`** (except explicit, authorised artefact
  paths). The legacy repo is read-only.
- **No fake success.** Unimplemented behaviour is `unsupported`/`escalated`,
  never pass.
- **Autonomy (owner directive D4, 2026-11-10).** The lead takes all decisions
  autonomously; implementation is done by subagents in parallel; decisions and
  findings are recorded in the repo; **verified work is committed locally (no
  push)**.
- **One consumer (owner directive D1, 2026-11-10).** ACOS has exactly one
  consumer: an AI coding agent. No human-facing UX. Machine-first JSON/MCP,
  structured errors, stable ids. See `SPEC.md` "Owner directives".

## ADR-0018 — Host resource policy: RAM ceiling 90%, internal target 85%, free promptly

Status: accepted (owner directive, 2026-11-10).

- Host RAM use must never exceed 90% (the host has 62 GB and no swap, so exceeding it can hard-crash the PC). The internal target is 85%; guards act when MemAvailable drops below ~9 GiB.
- Every VM, test, benchmark and emulator process runs under a memory and wall-clock bound (`tools/guarded.sh`, `tools/ramguard.sh`; VM-side `ACOS_HOST_RSS_MAX` and `ACOS_WALL_MAX_SECS` with structured errors `HOST_MEMORY_LIMIT` / `TIMEOUT`).
- Resources are released as soon as they are no longer needed: processes, fds, temp dirs, scratch artifacts, finished agents, merged worktrees.
- Orchestrator checks `free -m` before dispatching a parallel wave and gives every implementer the bounded-run rule.
- Incident that motivated this: a debug `acos run-dex` of 01_hello ran for about 2.8 h and reached 45 GB RSS (see docs/FINDINGS.md). It was killed by hand.
- **Implementation (W12, 2026-11-10).**
  - `acos-vm::limits::HostGuard` enforces `ACOS_HOST_RSS_MAX` (default 2 GiB -> structured `HOST_MEMORY_LIMIT`) and `ACOS_WALL_MAX_SECS` (default 600 -> structured `TIMEOUT`). Sampled on allocation/GC thresholds and once per scheduler turn (RSS read throttled to 100 ms) and on the blocking looper pump, so it holds independently of `ACOS_HEAP_MAX`. Documented in `docs/AGENT_API.md` §16.
  - **Root cause.** The vm-perf merge rewrote `interp.rs::run_batch` and omitted `ThreadStatus::Parked` from its yield condition; `interp::run` then reset the current thread to `Runnable` on its periodic pre-emption. A looper thread parked in `nativePollOnce` was therefore re-run forever at 100% host CPU once the VM held more than one thread, never reaching the blocking `pump`; the VM had no host-side ceiling, so the runaway was unbounded. Fixed by yielding on `Parked` and only pre-empting a thread that is still `Running`. Regression: `acos-vm` `tests/looper_cpu_mt.rs`, `tests/host_limits.rs`, and the `limits.rs` unit tests (the MT CPU test fails 308->808 ticks with the original code).
  - `tools/guarded.sh` is the mandatory bounded wrapper (`systemd-run` `MemoryMax`/swap-off, `ulimit -v` fallback, `timeout`, process-group kill, scratch cleanup); `tools/diff-dexprogs.sh`, `tools/bench-vm.sh` and `tools/build-*.sh` invoke it; `tools/ramguard.sh` is the host watchdog. See `tools/README.md`.
  - Resources are freed on every exit path: `guarded.sh` kills the whole process group and removes its scratch; `diff-dexprogs.sh` uses `target/tmp/` and traps its removal; `bench-vm.sh` removes `target/tmp/acos-bench`; `ramguard.sh` logs to `results/tmp/ramguard.log`.

### ADR-0018 note — native loader sandbox gap (W35-A, 2026-11-10)

Status: accepted as a **limitation**, not a solution.

- `acos-native-loader` maps guest ELF `PROT_EXEC` and runs it **in the host
  process**. There is no seccomp filter, landlock rule, user/mount namespace or
  separate sandbox process, so a loaded `.so` gets the host syscall surface.
- Path confinement is now enforced (`LoaderConfig::allow_host_paths`, default
  `false`; `resolve_library` canonicalises and requires the path to be under a
  configured `search_paths` root). That closes the "`dlopen` an arbitrary
  absolute host library" class, but it does **not** sandbox the mapped code.
- **Decision:** native `.so` execution stays **disabled for product paths**
  until the mapped code runs under a seccomp/landlock sandbox. Today the only
  entry point is `jni_glue::load_library_impl`, which raises
  `UNSUPPORTED_JAVA_API` when no JNI host is installed, and no workspace crate
  depends on `acos-jni`; only its own tests/examples exercise the loader. Do not
  wire native loading into `acos run`/`verify` without the sandbox.
- Enforcement point: the default `LoaderConfig` refuses out-of-root paths; a
  future product path must additionally refuse to load native code unless an
  explicit sandbox feature is enabled. See `HONESTY_AUDIT.md` #1/§2 rank 1.

### ADR-0018 update — native enable path (W59-A, 2026-11-11)

Status: the ADR-0018 note's product ban is **conditionally lifted** behind an
opt-in flag plus the ADR-0022 confinement layer; the default remains off.

- **Opt-in.** `ACOS_NATIVE=1` (or `acos launch --native`) enables APK native
  code for one launch process. Default is off until the security review passes,
  exactly as the W35-A note required.
- **ABI gate.** Before anything is extracted or mapped, the APK's `lib/<abi>/`
  directories are compared with the host ABI (`x86_64`). An APK with native
  code only for other ABIs fails with `UNSUPPORTED_ABI` naming every ABI it does
  provide (`acos-apk::Apk::check_native_abi`, `HOST_ABI`).
- **Never trust names.** `acos-apk::Apk::extract_native_lib(s)` copies a `.so`
  into the app's VFS lib directory only after validating the ABI and file name
  as single path components; `..`, `/`, `\` and NUL are `ZIP_SLIP`. The written
  path is re-checked to stay a direct child of the destination. Stored vs
  deflated entries and their in-APK `data_offset` are retained so the
  uncompressed case can be mapped `fd+offset` rather than copied (loader-side
  `android_dlopen_ext` mmap is the follow-up).
- **Confinement precedes guest code.** `acos-native-loader` already refuses to
  map a guest `.so` unless `acos_sandbox::confinement_active()` is true
  (`LoaderConfig::require_confinement`, ADR-0022). The launch gate calls
  `acos_sandbox::confine` in the launch process *before* the session worker
  thread is spawned, because seccomp is installed with `TSYNC` and must run while
  single-threaded. If a required layer is unavailable the launch fails closed
  with `CONFINEMENT_UNAVAILABLE`; no native code is loaded.
- **Resource caps.** The same profile installs `RLIMIT_AS`/`NOFILE`/`NPROC`/
  `CPU`/`CORE` (ADR-0022) and the VM's `HostGuard` keeps the host below the
  ADR-0018 RAM ceiling.
- **Codes.** New stable `ErrorCode`s: `UNSUPPORTED_ABI`, `CONFINEMENT_UNAVAILABLE`,
  `ZIP_SLIP`; the checked-in JSON Schemas were regenerated.
- **Not yet wired.** Extracting on-demand at the `System.loadLibrary` call site
  into the VFS lib dir, and MCP parity for `--native`, remain open; the loader
  already accepts the APK `lib/<abi>/` bytes and enforces confinement, so the
  product `acos`/MCP launch path still needs the JNI runtime attached. Until
  then `--native` proves the ABI/confinement gate and fails closed, and no
  runtime `System.loadLibrary` reaches untrusted ELF code.

### ADR-0018 update — one error contract for ABI misses and Landlock (E1, 2026-11-11)

Status: accepted, narrows ADR-0002/ADR-0014 without rewriting them.

- **One ABI-miss code.** `UNSUPPORTED_ABI` is the code for "the APK ships
  native code, but none for this host's ABI". The CLI gate (commit f221a77)
  already did this; the daemon RN backend (`acos-runtime::RnSessionError`) and
  the Flutter session (`acos-jni::FlutterSessionError`) now report it too
  instead of `WRONG_ABI`. The MCP `launch` path is therefore consistent with
  `acos launch` regardless of route.
- **`WRONG_ABI` kept, narrowly.** It remains the `acos-native-loader` code for
  one ELF whose `e_machine`/class is not what the in-process loader executes.
  That is a different condition (a mapped library, not an APK-level ABI
  inventory), and changing it would alter the loader policy contract and its
  `policy` test; the ADR-0014 note above is retained for history.
- **Landlock is its own family.** `acos-art::Error::LandlockUnavailable`
  (stable code `LANDLOCK_UNAVAILABLE`) replaces reusing
  `Error::SeccompUnavailable` for the S6 guest ruleset, and the code is in
  `acos-protocol::ErrorCode`. The seccomp filter and the Landlock ruleset still
  ship in one guard bundle, but a filesystem-confinement failure is now
  distinguishable from a syscall-filter failure in logs and JSON.
- **One table.** `docs/ERROR_CODES.md` lists code, meaning, component and exit
  status.

### ADR-0018 update — the ART path gates a wrong ABI before the guest (ABIGATE, 2026-11-14)

Status: accepted, implements the W59-A/E1 rule on the ART route without changing it.

- **Context.** The E1 note above made every *route* report one code, but on the
  ART path `acos-art` still logged `NativeLibInstall::UnsupportedAbi` and started
  the guest ("the guest must still fail with its own `UnsatisfiedLinkError`").
  The caller therefore saw `GUEST_EXITED` (or `INTERNAL` on the older path) for
  an APK the documentation says is `UNSUPPORTED_ABI` (REL3a §3, goal 4).
- **Decision.** The ABI decision is made from the APK index in `acos-apk`
  (`Apk::abi_gate()`) and enforced in `acos-art` at the `native-libs` stage —
  before the image, props, logd or any guest process — so `acos launch/verify
  --engine art`, `acos serve` sessions and MCP `acos_launch/acos_verify` all get
  `UNSUPPORTED_ABI`, `status: "unsupported"`, `retryable: false` and
  `details.apk_abis`/`host_abi`/`libs_sample`. Nothing is a pass.
- **The rule is AOSP's, per package.** `PackageManager` chooses one ABI for the
  whole package: `NativeLibraryHelper.findSupportedAbi` keeps the `lib/<abi>/`
  directory whose index in the device's `SUPPORTED_ABIS` list is lowest
  (`frameworks/base/core/jni/com_android_internal_content_NativeLibraryHelper.cpp`),
  `NO_NATIVE_LIBRARIES` for an APK with no `lib/`, and
  `INSTALL_FAILED_NO_MATCHING_ABIS` for `lib/` with no match;
  `iterateOverNativeFiles` then extracts only that ABI. Hence: no `lib/` always
  launches; `lib/x86_64` plus `lib/arm64-v8a` launches as x86_64; and
  `lib/x86_64/liba.so` with an arm64-only `libb.so` **launches** too — `liba.so`
  loads and the first `dlopen("libb.so")` is the app's own `UnsatisfiedLinkError`,
  which is a packaging bug the platform reproduces, not an install gate.
- **Consequences.** `UNSUPPORTED_ABI` now means "no directory for a host ABI" on
  every route and at every layer; the mixed case is covered by a unit test so a
  later "gate any missing library" change cannot silently start refusing apps a
  real device installs.

## ADR-0019 — Text rendering: FreeType + HarfBuzz glyph rasterisation; no Skia adoption (supersedes the Skia part of the advisor suggestion)

Status: accepted (2026-11-10), based on research/render-fidelity/REPORT.md.

- Measured per-glyph coverage error vs the real device screenshot (positions removed): tiny-skia no AA 0.0582 mean, tiny-skia + AA 0.0458, HarfBuzz + FreeType (no hinting, grayscale) 0.0163, Skia m157 CPU 0.0549 (and +12.5% ink). Android 14 uses Skia m112/m124; no skia-safe release maps to it, and building AOSP Skia/Minikin from source is a multi-day, >1 GB port.
- Decision: keep the Rust renderer (acos-render), replace glyph rasterisation with FreeType (FT_LOAD_NO_HINTING | FT_LOAD_RENDER, grayscale A8 masks) shaped by rustybuzz (HarfBuzz port), blit masks in acos-render. Fix Paint::default().anti_alias (Android's Paint() has ANTI_ALIAS_FLAG; current default renders text with 2 grey levels).
- Keep revisiting if shadows/layers/filters parity (non-text) becomes the dominant error; then re-evaluate a pinned Skia CPU path.
- Licence: FreeType under FTL, HarfBuzz MIT.

## ADR-0020 — SQLite engine boundary: vendored `rusqlite` in a new `acos-sqlite` crate

Status: accepted (W49-A, 2026-11-10), supersedes no previous decision.

- **Context.** The `android.database.sqlite` framework path
  (`SQLiteDatabase` → `SQLiteConnectionPool` → `SQLiteConnection.native*`,
  plus `CursorWindow.native*`) is pure Java over a C engine. G-03 in
  `results/dogfood/GAPS.md` records that ACOS had no SQLite code at all, so the
  `sqlite-crud` fixture and any DB-using app stopped at the first native.
- **Decision.** Link the SQLite C amalgamation through the audited
  `rusqlite` wrapper (feature `bundled`, crate `rusqlite = "0.37"`), isolated
  in the new `crates/acos-sqlite`. `acos-vm` keeps `#![forbid(unsafe_code)]`;
  the FFI unsafe lives only in the external `rusqlite` / `libsqlite3-sys`
  dependencies, and `acos-sqlite`'s own code is also
  `#![forbid(unsafe_code)]`. No ACOS crate other than `acos-sqlite` links
  SQLite. Licences: SQLite is public domain; `rusqlite` is MIT.
- **Why not directly in `acos-vm`.** `rusqlite::Statement` borrows its
  `Connection`, so statement lifetime would force self-referential or raw
  pointer handling inside the interpreter crate. The facade keeps the FFI and
  its lifetime rules in one place and exposes an owned, statement-oriented API.
- **Sandbox.** Database paths are accepted only through the guest VFS mapping
  (`/data/user/<n>/…`, `/data/user_de/…`, `/data/data/…` → `<Vfs::root>/…`);
  the parent is canonicalised and must stay under the sandbox root, so a
  symlink planted in app data cannot reach the host. `:memory:` is passed
  through. There is no host-path escape and no separate `-wal`/`-shm` policy
  beyond SQLite's own journal mode (WAL is not forced).
- **Fidelity.** Error messages reproduce Android 14's `throw_sqlite3_exception`
  format exactly, including the extended code and symbolic name
  (`UNIQUE constraint failed: notes.body (code 2067 SQLITE_CONSTRAINT_UNIQUE)`)
  and the `": , while compiling: <sql>"` suffix on prepare errors. The result
  code selects the same `SQLiteException` subclass (`SQLiteConstraintException`,
  `SQLiteReadOnlyDatabaseException`, `SQLiteFullException`, …). Unregistered
  natives (`nativeExecuteForBlobFileDescriptor`, `nativeRegisterCustom*`,
  parcel/`SQLiteDebug` paths) stay structured `UNSUPPORTED_JAVA_API` — never a
  faked result.
- **Evidence.** `crates/acos-vm/tests` + `fixtures/dexprogs/28_sqlite_basic`
  (create/insert/query/update/delete, commit/rollback, UNIQUE/NOT NULL,
  BLOB/type/NULL, BINARY/NOCASE). Captured from the real API-34 x86_64 ART
  (`app_process`, because the framework natives are registered by
  `AndroidRuntime` and not by `dalvikvm64`):
  `fixtures/expected-art/dexprogs/28_sqlite_basic.stdout` is byte-identical to
  the ACOS run, over three deterministic ART runs.
  `tools/diff-dexprogs.sh --corelib real` = 28 pass / 0 fail / 0 unsupported.

## ADR-0021 — JNI native OS-thread attach: process-global slots + owner-bound VM (W54-A)

Status: accepted (2026-11-11), workstream 3 of cross-platform apps.

- **Context.** Complex native engines (Hermes, the Flutter engine, fbjni) call
  `AttachCurrentThread` from OS threads created with `pthread_create`, then make
  JNI calls with a per-thread `JNIEnv`. ACOS runs one VM on one host thread;
  `acos-vm` is `#![forbid(unsafe_code)]` and `Rc`/`RefCell`-based, so its state
  is neither `Send` nor `Sync`. The previous `AttachCurrentThread` returned the
  owner's environment unconditionally and `GetEnv` always succeeded, which is
  wrong on a native thread.
- **Decision.** Publish the immutable `JNIEnv` slot address in a process-global
  atomic at `JniRuntime::new`. `AttachCurrentThread` on a thread with no
  thread-local runtime attaches it through a global `HashSet<ThreadId>` and
  returns the shared environment (the function table is stateless, so one
  `JNIEnv` address is valid for every attached thread). `GetEnv` never attaches
  and returns `JNI_EDETACHED` for an unattached thread; `DetachCurrentThread`
  clears the record. The owning thread keeps its thread-local attachment.
- **Boundary.** The VM stays single-threaded and owner-bound. An attached native
  thread can call the bookkeeping slots that need no VM, but a VM-touching slot
  reports `JNI_NO_VM` (structured) rather than racing the owner. Java callbacks
  from a native OS thread are **not** supported yet.
- **Why not a brokered re-entrant mutex now.** A sound broker needs the owner to
  release the `&mut Vm` borrow across the guest call and to pump queued requests
  at a safe point. That is a VM-wide refactor (safe points, a request channel,
  and a suspend protocol) that cannot be landed safely inside the W54-A window.
  The API surface and per-thread attach shipped now are the safe subset; the
  broker is the follow-up.
- **Evidence.** `fixtures/jni/acos_jni_slots.c` spawns a real `pthread` and
  asserts `GetEnv == JNI_EDETACHED` → `AttachCurrentThread == JNI_OK` → same-env
  `GetEnv == JNI_OK` → `DetachCurrentThread == JNI_OK` → `JNI_EDETACHED`
  (`jni_native_fixture`, bit 10). `jni_slots::javavm_attach_getenv_and_versions`
  keeps the owner-thread contract.

### ADR-0021 follow-up — brokered JNI request channel (W57-A), implemented

Status: implemented (2026-11-11). The follow-up promised under *Why not a
brokered re-entrant mutex now* has landed.

- **Decision.** An attached native OS thread that owns no `*mut Vm` no longer
  fails VM-touching slots with `JNI_NO_VM`. It enqueues a request in
  `acos-jni/src/broker.rs` and blocks on a `Condvar` for the reply. The owner
  drains the queue at two safe points: the interpreter/`run_inner` loop (via the
  safe Rust hook `acos-vm/src/natives/safe_point.rs`) and an explicit nested
  pump while the owner is itself blocked in a native call (`acos_jni_broker_pump`
  / `JniRuntime::pump_broker`). This makes synchronous
  owner→native→worker→Java→owner patterns terminate instead of deadlocking.
- **Invariant.** The `Vm` is still only dereferenced by its owner thread. A
  worker's forced thread id lets the owner run the slot against the worker's
  reference frames and pending-exception slot, which the owner owns and frees on
  `Detach`. The safe-point hook is re-entrancy-guarded. `acos-vm` remains
  `#![forbid(unsafe_code)]`; `unsafe` is confined to `acos-jni/src/broker.rs`
  with `SAFETY` comments.
- **Errors and limits.** Java exceptions are marshalled back through the
  worker's `ExceptionCheck`/`ExceptionOccurred`; broker faults become structured
  errors; an unserviced reply is `JNI_BROKER_TIMEOUT` (default 5 s,
  `ACOS_JNI_BROKER_TIMEOUT_MS`). A worker that never issues a VM request owns no
  frames and detaches without a round trip.
- **Evidence.** `crates/acos-jni/tests/jni_broker.rs` (7 tests, incl. 8 workers ×
  1000 calls, GC stress with held local/global refs, balanced reference
  accounting, and the Java-throw path) plus the NDK fixture
  `acos_jni_test_broker_worker` driven by
  `jni_native_fixture::native_fixture_worker_brokers_java_call`. Green 20× under
  `ACOS_GC_STRESS=1`; `clippy -D warnings` on `acos-jni`; workspace check and
  `diff-dexprogs --corelib real` all pass.

---

## ADR-0022 — Native confinement layer: `acos-sandbox`, no sudo, fail closed

Status: accepted (W53-A, 2026-11-11), supersedes the ADR-0018 note's
"native loader sandbox gap" as a *limitation* (the gap is now closed by a
layer; product wiring is still pending).

- **Context.** `acos-native-loader` maps guest ELF `PROT_EXEC` and runs it in
  the host process. The ADR-0018 note recorded that this is unsandboxed and
  therefore disabled for product paths. Cross-platform apps (workstream 1)
  need a confinement layer before an untrusted APK `.so` can run inside the
  ACOS session process, with **no sudo and no kernel/system changes**.
- **Decision.** Add `crates/acos-sandbox`, the *only* ACOS crate allowed to
  hold the `unsafe`/`libc` calls needed to enter a sandbox. It exposes a
  capability probe (`probe`, `Capabilities`), a profile (`ConfineProfile`), an
  orchestrator (`confine`) and a live fork probe (`probe_live`). Layers, in
  application order:
  1. `prctl(PR_SET_NO_NEW_PRIVS)`;
  2. close inherited fds except `keep_fds` (`close_range`, `ENOSYS` fallback);
  3. sanitised env (`clearenv` + a minimal Android-flavoured set);
  4. rlimits (`RLIMIT_AS=4 GiB`, `NOFILE=1024`, `NPROC=8192`, `CPU=60 s`,
     `CORE=0`, all configurable);
  5. optional unprivileged user/network/mount namespaces (`uid_map`/`gid_map`
     + private mount propagation);
  6. Landlock filesystem ruleset via the raw ABI (syscalls 444/445/446):
     `handled_access_fs` is every right the ABI supports, read-only roots grant
     `EXECUTE|READ_FILE|READ_DIR`, read-write roots grant the full mask, and
     any path outside the listed roots is denied;
  7. seccomp-bpf (`SECCOMP_SET_MODE_FILTER` + `TSYNC`; default `ALLOW`, denials
     return `EACCES`) denying `execve`/`execveat`, `ptrace`, `process_vm_*`,
     `mount`/`umount2`/`pivot_root`/`chroot`, `unshare`/`setns`, `kexec*`,
     `bpf`, `perf_event_open`, `keyctl`/`add_key`/`request_key`, module ops,
     `reboot`, and `socket`/`socketpair` for every family except `AF_UNIX`;
     `futex`, threads, `mmap`/`mprotect` (including `PROT_EXEC`), signals,
     clocks, `eventfd` and `epoll` stay available.
  Optional cgroup-v2 memory/PID limits via `systemd-run --user --scope` are
  **probed only** and never required.
- **Ordering deviation from the work item.** Namespaces are applied *before*
  Landlock/seccomp (not last): writing `uid_map` needs `/proc` and making
  mounts private needs `mount`, both of which the later layers deny. This is
  the only deviation; `PR_SET_NO_NEW_PRIVS` still precedes Landlock and seccomp.
- **Fail closed.** `confine` returns a structured `ConfineError` with code
  `CONFINEMENT_UNAVAILABLE` whenever a layer in `ConfineProfile::required` is
  not active. It never reports a layer active unless the kernel accepted it.
  `confinement_active()` is a process-global bit set only when
  `PR_SET_NO_NEW_PRIVS` + Landlock + seccomp are all active.
- **Native-loader integration.** `LoaderConfig` gains `require_confinement:
  bool` (default `true`); `Loader::load_bytes_named` refuses with
  `LoaderError::ConfinementRequired` (code `CONFINEMENT_UNAVAILABLE`) unless
  the process reports confinement. Trusted in-process tests use the
  clearly-named `LoaderConfig::for_tests_without_confinement()`; product paths
  must not. `acos sandbox-probe` reports capabilities and a live layer report
  as JSON.
- **Evidence.** `cargo test -p acos-sandbox` runs `sandbox_child` as a separate
  process and asserts, with confinement on, that `exec`, `ptrace`,
  `process_vm_readv`, `socket(AF_INET)`, and open/write outside the allowed
  roots fail while threads, `mmap(PROT_EXEC)`, `futex`, read of the read-only
  root and write to the data dir succeed; with confinement off the denied
  actions succeed, proving the sandbox is the cause. On the dev host (kernel
  `7.2.11-arch1-1`, Landlock ABI 10) all six mandatory layers are active.
- **W67-A hardening (2026-11-11).** The seccomp posture stays default-`ALLOW`
  (the allow-list was not validated against the full RN/Flutter native-lib
  inventory), but the deny list is extended to the guest-reachable host-escape
  set: io_uring `setup`/`enter`/`register`, `userfaultfd`,
  `open_by_handle_at`/`name_to_handle_at`, the new mount API (`fsopen`,
  `fsconfig`, `fsmount`, `fspick`, `move_mount`, `open_tree`, `mount_setattr`),
  the pidfd family, `kcmp`, `process_madvise`, `lookup_dcookie`, `quotactl`,
  `swapon`/`swapoff`, `syslog`, `acct`, `ioperm`/`iopl`, `modify_ldt`, the
  `setuid` family and `capset`. Arguments are filtered where classic BPF can:
  `clone` denies `CLONE_NEW*`, `clone3` returns `ENOSYS` (glibc/bionic fall
  back to the filtered `clone`), `kill`/`tkill` are denied while
  `tgkill`/`rt_*sigqueueinfo` allow only the own pid/tgid, and `personality`
  allows only `PER_LINUX` and the read-only query form. **Residual risk:** a
  future kernel attack surface reached by a syscall not on the list still
  defaults to allow; the deny list is the mitigation and must be revisited per
  kernel CVE. Landlock ABI >= 6 additionally sets `LANDLOCK_SCOPE_SIGNAL` and
  `LANDLOCK_SCOPE_ABSTRACT_UNIX_SOCKET`; `confinement_active()` now requires
  the full `Layer::TRUSTED` hygiene set, so a failed hygiene step or an
  under-specified probe profile can never make the loader trust a `.so`;
  rlimits add `RLIMIT_FSIZE`, `RLIMIT_STACK` and `RLIMIT_MEMLOCK`; and the
  native-launch read roots drop host `/usr`, `/lib` and `/etc`.
- **Open question.** Running the confinement inside the already-multithreaded
  session process: seccomp is installed with `TSYNC`, so `confine` must be
  called before the runtime spawns coordinator threads, or the call fails
  closed. The session-lifecycle workstream (W51-A) owns that call site.
- **W93-A note (Flutter engine, 2026-11-11).** The Dart AOT runtime reserves a
  large, lightly committed compressed-pointer heap region at VM startup
  (`dart_vm`'s `virtual_memory_compressed.cc` mmaps it
  `MAP_NORESERVE|MAP_ANONYMOUS` and fails with `ENOMEM` if `RLIMIT_AS` is too
  small). The default `RLIMIT_AS = 4 GiB` is exactly exhausted by the 166 MiB
  `libflutter.so` mapping plus the loader's relocation tables, so the VM died
  before `main()`. The W93-A harness runs with a bounded 12 GiB
  `address_space_bytes`; this stays a DoS guard, not a capability, so the
  residual risk is only that a guest can reserve more address space before
  `ENOMEM`. Recommendation for the product profile: keep a finite bound but
  size it for the native-engine reservation (8-16 GiB) rather than 4 GiB, or
  give `acos-jni` an engine-aware `ConfineProfile`. W93-A did **not** change
  the product default (other workstreams depend on it); the per-run override is
  recorded here.

## ADR-0023 — Typed-IR slow-op trampoline; IR tier on by default (W73-A)

**Status:** accepted (2026-11-11).

**Context.** The typed-IR tier (`crates/acos-vm/src/exec.rs`, ADR-0013 / §8.9)
decoded loop bodies into a compact `Op` stream and ran them in a tight `match`
loop. `new-instance`, `new-array`, `const-string`, and every `invoke-*` lowered
to `Op::Fallback`: the loop stopped before them and the general interpreter ran
one instruction. On the allocation/GC programs this left `BenchAlloc` ~2x and
`BenchGc` ~4.4x behind `java -Xint`, and the tier stayed opt-in
(`ACOS_EXEC=ir`).

**Decision.** Run the VM-touching slow ops inside the tier through a trampoline
and make the tier the default execution path, with `ACOS_EXEC=interp` (also
`off`/`0`) as the escape hatch.

* `Op::NewInstance`, `Op::NewArray`, `Op::ConstString` and `Op::Invoke` are
  lowered normally. `run_ir` runs a tight loop (`run_ir_loop`) that holds the
  frame borrow for a whole run of register/field ops; on a slow op it returns,
  releases the borrow, runs the op with `&mut Vm` through shared helpers
  (`interp::exec_new_instance` / `exec_new_array` / `exec_const_string` /
  `exec_invoke_at`), and re-enters the loop.
* GC safety is preserved by construction: the frame never leaves
  `vm.threads[t].frames`, so its `regs`/`refs` are visible to the mark phase at
  every allocation, and the destination is written before any further
  allocation. `ACOS_GC_STRESS=1` passes 27/27.
* `invoke` positions the frame at the invoke, pushes the callee, and returns to
  the scheduler so the callee body runs; the caller's pc is advanced by
  `pop_frame` as before. A generic `Op::Fallback` (monitor, switch, check-cast,
  ...) still stops before the instruction and is run by the general path, so
  catch-table and resolution-error semantics are unchanged.

**Evidence** (release, 5 interleaved rounds, `vmbench` min, host 32 threads,
`ACOS_CORELIB` default; `results/bench/w73a-ir-before.json` /
`w73a-ir-after.json`):

| Program | IR off (ms) | IR on (ms) | speedup |
|---|---:|---:|---:|
| `BenchArith` | 2081.50 | 960.16 | **2.17x** |
| `BenchLoop` | 1832.66 | 842.67 | **2.17x** |
| `BenchArray` | 1353.23 | 710.83 | **1.90x** |
| `BenchFields` | 445.95 | 116.53 | **3.83x** |
| `BenchFib` | 485.82 | 499.97 | 0.97x¹ |
| `BenchStaticCall` | 3243.94 | 3205.41 | 1.01x |
| `BenchVirtualCall` | 3257.23 | 3225.01 | 1.01x |
| `BenchVirtual` | 1980.60 | 1860.92 | **1.06x** |
| `BenchAlloc` | 2465.98 | 1937.78 | **1.27x** |
| `BenchGc` | 620.35 | 533.78 | **1.16x** |
| `BenchString` | 122.65 | 123.50 | 0.99x¹ |
| `BenchCollections` | 157.43 | 133.71 | **1.18x** |

¹ Within the 5-15 % run-to-run spread; `BenchFib` has no backward branch, so
the tier never applies to it (the row is the `enabled()` check alone).

* `tools/diff-dexprogs.sh --corelib real`: 27/27 normal and `--gc-stress`, IR on
  and off.
* `crates/acos-vm/tests/ir_equivalence.rs`, `cargo test -p acos-vm`,
  `cargo clippy -p acos-vm --lib --tests -- -D warnings`, and
  `cargo check --workspace --all-targets`: green.

**Consequences.** The tier is the default; the general interpreter remains
available for debugging and as the reference path. `BenchAlloc` improves 1.27x
and `BenchGc` 1.16x over IR-off, and the arithmetic/field loops keep their
1.9-3.8x wins. The remaining `BenchGc`/`BenchAlloc` gap to `java -Xint` is
allocator/GC-bound, not dispatch-bound; a bump/size-class arena is the next
lever and is out of scope here.

---

## ADR-0024 — Const+if fusion, 4-way virtual cache, slimmer `Object` (W89-A)

**Status:** accepted (2026-11-11).

**Context.** After ADR-0023 the remaining `java -Xint` gaps were dominated by
dispatch on the arithmetic/array/field loops (`BenchArith` ~1.9x,
`BenchLoop`/`BenchArray` ~2.1x) and by virtual/interface dispatch on
`BenchVirtual` (~1.6x). The typed-IR loop already fused `<arith>/lit` + `if-*`
(the counter idiom), but javac/d8 emits the *loop-bound constant* immediately
before the loop test, so the test executes two dispatches (`const*`, `if-*`).
The per-call-site virtual cache held one entry, so a 3-shape polymorphic cycle
missed every probe and fell through to the tuple-keyed `dispatch_cache`
`HashMap`.

**Decision.**
* Fuse a 32-bit `const*` + conditional branch into `Op::ConstIf` at IR-lowering
  time, under the same guard as `ArithLitIf` (the branch must not be a re-entry
  point). The `const*` write is pure, so re-entering the fused op is safe.
* Replace the one-entry `DexCtx::virtual_cache` slot with a fixed 4-way
  `VirtualCache` (linear probe, round-robin eviction). It remains a pure
  function of (declared signature, receiver class), so no invalidation.
* Box the widest `BuiltinData::StackTraceElement` variant so `Object` (and
  therefore every `Option<Object>` slot moved per allocation/free) shrinks.
* `take_field_vec` uses `clear` + `resize` instead of `resize` + `fill`.

**Evidence** (`results/bench/w89a-after.json`; 5 interleaved `acos run-dex`
rounds, min of each config, host JVM 17 `-Xint`; supplementary in-process
`vmbench` best-of-8 is more stable):

| Program | before ms | after ms | before x | after x |
|---|---:|---:|---:|---:|
| BenchVirtual | 1844.7 | 1558.0 | 1.61 | **1.36** |
| BenchArray | 712.4 | 685.5 | 2.15 | **2.06** |
| BenchFields | 115.9 | 112.4 | 1.55 | **1.51** |
| BenchLoop | 834.1 | 826.2 | 2.06 | 2.04 |
| BenchArith (vmbench) | ~985 | ~915 | ~2.0 | **1.86** |
| BenchAlloc | 1620.6 | 1620.1 | 1.39 | 1.39 |
| BenchGc | 347.2 | 345.2 | 2.44 | 2.43 |
| BenchFib | 490.8 | 491.3 | 1.48 | 1.48 |
| BenchString | 123.2 | 128.3 | 0.20 | 0.21 |

`BenchCollections` 0.99x, `BenchStaticCall` 1.00x, `BenchVirtualCall` 1.01x are
within the process-time noise floor; no row is a net regression beyond noise.
Allocator-bound rows (`BenchAlloc`, `BenchGc`) are unchanged: their gap is the
per-object host allocation and the two invoke trampolines per iteration, not
dispatch or mark/sweep (measured `gc_ms` is ~7% of `BenchGc`), so a size-class
arena remains the next lever.

**Consequences.** Fused dispatch and the 4-way cache are on by default; no new
`unsafe` (crate still `#![forbid(unsafe_code)]`). `cargo test -p acos-vm`,
clippy `-D warnings --no-deps`, `cargo check --workspace --all-targets`, and
`tools/diff-dexprogs.sh --corelib real` 27/27 normal + `--gc-stress` are green.

## ADR-0025 — Software GLES2/EGL substrate inside the guest (`acos-gles`)

**Context.** Third-party native libraries (Flutter's legacy GL backend, React
Native GL modules, any NDK app that links `libGLESv2.so`) call the real EGL/GLES
entry points. Running them against the host driver would leave the host GPU and
the guest's memory model in the same address space, and every pixel would come
from a driver we cannot audit.

**Decision.** Provide a Rust implementation of the GLES2 subset those engines
actually use, installed into the confined guest process by the native loader
(`acos-jni::ndk_host`, `gles_host`) and linked as `libEGL.so`/`libGLESv2.so`.
It is also usable in-process by the renderer tests. Rules:

* A GLSL ES 1.00 interpreter parses and evaluates shader source per vertex /
  per fragment; there is no JIT and no raw guest memory access.
* Constructs outside the implemented subset are a **structured rejection**
  (`UNSUPPORTED_GLES` + `GL_FALSE` from `glGetShaderiv(GL_COMPILE_STATUS)`), and
  no entry point is a silent no-op. Resource limits (GPU bytes, mip level,
  per-invocation shader instruction budget) are rejected with a structured
  `GLES_RESOURCE_LIMIT` + `GL_OUT_OF_MEMORY`, never an abort (W94-A #2–#4).
* Guest-controlled sizes are bounded by `MAX_DIM` before any allocation.
* The substrate requires confinement to be installed (`require_confinement`);
  the fail-closed rules of ADR-0022/ADR-0018 apply.

**Consequences.** Earlier documents (W86-A, W92-A) cited "ADR-0023" for this
substrate; ADR-0023 is the typed-IR trampoline and ADR-0024 is the IR fusion
work, so those references mean ADR-0025. The renderer is validated against
ART reference captures (`results/render/`), not against a device GPU.

## ADR-0026 — Owner decisions 2026-11-14 (zygote, settle semantics, boot image, insets, compositor)

**Context.** Seven open owner decisions from `docs/MORNING-2026-11-13.md` §3 and
`docs/TRACKS-2026-11-12.md`, re-explained to the owner on 2026-11-14.

**Decisions.**

1. **Zygote shared ART: build the production version (opt-in first).** Persistent
   zygote; reset forks one fresh child (not the whole cohort); per-session run
   root and per-session memory limit (ZS F1–F3, needed for verification
   correctness, not only security). It becomes the default when it wins on the
   real agent loop at N=10 (Z7 gate: ≥25 % whole-system vs the optimised current
   path, incl. the shared boot image) with 0 false passes. Owner view: ACOS is a
   test/verification environment with no real users, so hostile-APK hardening is
   not a blocker beyond protecting the host. Host-protecting kernel layers
   (bwrap, seccomp, Landlock) stay on every child.
2. **Strict settle needs no pixel change (P12 rule kept), plus an opt-in per-step
   `expect_change: true`** that fails the step if the settled frame's pixels did
   not change. Replies keep reporting `pixels_changed`.
3. **Forever-animating screens: add a content-stable settle.** Settled when the UI
   tree content signature (text, nodes, bounds) is unchanged for a bounded window
   (~300 ms) even if pixels keep animating. Pixel assertions on such screens stay
   strict (honest TIMEOUT). Existing navigation-only `settle: fast` stays but is
   no longer the documented answer for these screens.
4. **Boot image default = `shared-fixed`, built once and cached on disk** (reused
   across serves; rebuilt only when the system image changes). Image ASLR is
   given up (owner accepted). `lz4` stays selectable.
5. **ACOS `settle: fast` stays an opt-in agent navigation mode; it never backs a
   pass and benchmark headline rows are strict.** The bench loop's reset step
   stays strict (M6 341 ms vs the 300 ms target is accepted; the zygote is the
   route to a faster reset). Separate rule: orchestration never uses the Claude
   or DeepSeek provider "fast mode" (it drains the owner's limits).
6. **Window insets default = `report` (phone-like edge-to-edge).** Re-bless the
   `w42-back-frame` expected frame for the one anti-aliased row that changes;
   no pixel tolerance is added.
7. **Compositor Spike A (stock SurfaceFlinger): dropped for now.** C1 with the
   AOSP geometry code stays; revisit only if compositor placement bugs return.

**Consequences.** New work items: Z-prod (1), SE1 `expect_change` (2), SE2
content-stable settle (3), H13b cached `shared-fixed` default (4), W7b `report`
default + w42 re-bless (6). Each is gated as usual (tests + gates green, 0 false
passes) before merge.

## ADR-0027 — Owner decisions 2026-11-14 night (live view, Metro, T3 Code, screenshots)

**Context.** The owner asked for (1) an optional live view of a session's screen, (2) React Native
Metro / Fast Refresh in a session, (3) a way to watch what agents are doing from T3 Code, and
(4) smaller screenshots (each capture is a ~10 MB stored-zlib PNG today). Plan and options were
explained in chat; the owner chose the recommended option for every decision. All of it must keep
the tracked benchmark metrics (M4–M8) at parity with the viewer off.

**Decisions.**

1. **Viewer surface = a local browser page** (`acos view`, 127.0.0.1 only, random token in the URL).
   It works in T3 Code's built-in preview pane, any browser, and needs no native window.
2. **Frames stream only while a viewer is connected.** Every live session registers cheap metadata
   (`$XDG_RUNTIME_DIR/acos/live/<id>.json`: agent/client, project dir, APK, activity, stream
   socket); the launcher's frame tap does nothing (one atomic check) with no subscriber. With a
   subscriber: half-resolution (540×1200) lossy frames (JPEG/WebP) from the C1 compositor's latest
   frame, encoded on a separate thread, ≤ 15 fps, drop-latest, never blocking the compositor or an
   agent command, never forcing a present or changing frame_seq/generation (settle semantics).
   Each session also emits an action/event stream (tool, coordinates, masked text, result status,
   activity, agent + project) for overlays (tap ripple, current screen, recent actions).
3. **T3 Code: use its preview pane, no patch.** T3 Code is a binary install (/opt/t3code-bin); a
   patched build would break on every update. A docked panel is revisited only if the preview pane
   is not enough.
4. **Metro: a single-port forwarder, opt-in per session.** The sandbox keeps `--unshare-net`; one
   forwarder maps guest 127.0.0.1:8081 to the host Metro port (like `adb reverse`), nothing else.
   Requires the app's debug build (owner's agents build it; ACOS consumes). Dev-mode RN launch
   latency is tracked as its own metric (edit→changed-frame target ≤ ~1.5 s), never mixed into the
   release-build metrics. A one-hour spike answers the risks first (dev-server host detection,
   Hermes dev mode / Fast Refresh / LogBox inside the sandbox, Gradle debug build offline).
5. **The viewer is view-only.** Agents are the users; human input in the viewer would race the
   agent driving the same session.
6. **Screenshots: compress.** Default capture becomes a compressed lossless PNG (fast deflate +
   filter), still exact for pixel assertions, with M5 at parity or better (smaller write + hash
   should offset the encode). Images returned to agents over MCP are additionally size-bounded
   (downscaled lossy preview + path to the lossless file), since a full-resolution PNG in a tool
   reply is wasted context.

**Order.** Finish v0.2.0 → screenshot compression and live view (phase 1) in parallel → T3 preview
pane check → Metro spike → Metro bridge.

## ADR-0028 — v0.2.1 release, lattice-aware fast-settle gate (orchestrator, 2026-11-15)

**Context.** Fast-settle latencies (M4/M6/M8 `fast`) sit on a ~16.4 ms frame-period lattice; a rep's
p50 is whichever frame slot holds the middle sample, so the old gate (medians of per-rep p50s, +5 %
and +2 ms) flips by a whole frame between reps (REL3-AB, INT-021-AB). PERF-REL3 measured the v0.2.1
launch-fast "shift" with 40 samples/rep: pooled trimmed mean +1.0 % (+1.7 ms), first-slot share 44 %
vs 43 % (n = 120/120) — noise.

**Decision.**
1. Fast-settle metrics are gated on the **pooled 5 %-trimmed mean** (+5 % and +2 ms) from raw
   samples (`tools/bench/ab-compare.py`), with a ≥ 10 pp drop in first-slot share flagged for a
   look. The old median-of-p50s gate is still printed beside it and still gates strict metrics.
2. The bench takes 40 fast samples per rep by default (+~57 s/rep; `--fast-reps` / `ACOS_BENCH_FAST_REPS`).
3. Ship v0.2.1 (main = int-021): no metric regresses under either gate; M5 −57 %, M8 form fast −28 %.
4. The FB2 placeholder verdict and the VIEW1 live view are NOT in v0.2.1: both reviews returned
   "merge after fixes" (FB2c, VIEW1c in flight).

## ADR-0029 — Service modules: attachable Android building blocks (owner + orchestrator, 2026-11-16)

**Context.** The Gaten floating-button run showed ACOS covered ~15 % of a real change: no
accessibility service, overlay windows, second app, intents, WebView, network, notifications,
permissions, rotation or sensors. Today ~50 services are registered lazily in
`crates/acos-services/src/registry.rs`; unknown names return null. Owner: every missing service must
be buildable and attachable/detachable per session like building blocks. Full plan:
`docs/PLAN_2026-11-16.md`.

**Decision.**
1. A *module* owns a set of Android system pieces and has a manifest (provides, requires, profiles,
   RAM/startup budget, security class, control ops, `hot` or `at-launch`). Today's services are the
   always-attached `core` module.
2. Profiles: `off`, `stub`, `scripted`, `real`. Interface in `acos-services`: `attach`, `detach`,
   `control`, `report`, on the existing `ServiceHost::register` seam. Detach leaves no registration,
   thread, fd or socket.
3. Unattached known names answer with a tombstone: `UNSUPPORTED_ANDROID_API` naming the module and the
   attach call. Never a silent null presented as a phone, never a fake success.
4. API: `acos_launch {services:[...]}`, scenario `services:` block, `acos_services`
   (attach/detach/list/status, `hot` modules only on live sessions), `acos_service_ctl`.
5. Verdicts list attached modules and profiles; a pass relying on `stub`/`scripted` says so; an
   assertion needing an `off` module is `unsupported`.
6. `acos_capabilities` and help limits are generated from the module registry.
7. Conformance kit per module: manifest valid; attach/detach twice returns RAM, threads, fds to
   baseline; honest-report test; negative test; N=20 density within budget.
8. Foundations: module framework, window layer stack in C1 (absorbs ROT9), multi-process sessions
   (second app forked lazily from the session zygote, binder routed by the Rust service layer, idle
   kill after ~30 s with saved state; un-installed share/pick targets land in a broker capture
   target, no process), host broker.
9. All modules ship together in v0.4.0; each needs its real-app proof (plan §3.4).

## ADR-0030 — Real network through pasta (owner + orchestrator, 2026-11-16)

**Decision.**
1. The session keeps its network namespace; `pasta` (passt, rootless) provides a NATed interface,
   bundled static in the install (GPL-2 licence shipped), checked by `acos doctor`.
2. seccomp allows `AF_INET`/`AF_INET6` only when the network module is attached.
3. No route to host loopback or the LAN gateway by default (`--no-map-gw`); host ports only when named
   (Metro 8081 in v0.5.0).
4. ACOS serves `/dev/socket/dnsproxyd` (netd replacement); the image ships the AOSP CA store.
5. Connectivity reports a validated internet network; hot controls: offline/online, latency/loss,
   optional HAR recording proxy.
6. Default profile `real` only if the spike A/B shows no regression in cold launch, warm launch,
   per-session memory or N=20 density beyond noise (owner); otherwise default `off`.
7. Fresh Opus security review before merge.

## ADR-0031 — WebView on the host's Chromium (owner + orchestrator, 2026-11-16)

**Context.** The guest renders on the CPU with no GPU stack; the Android System WebView needs a
separate renderer process and a hwui GPU functor and would cost ~150–250 MiB per session.

**Decision.**
1. ACOS ships its own WebView provider package, loaded by the framework's `WebViewFactory`. It
   implements `android.webkit` by driving a pinned `chrome-headless-shell` (Chrome for Testing,
   installer download with checksum) on the host over the DevTools protocol, through the host broker.
   The guest never sees a raw DevTools socket. Chromium runs in its own bwrap sandbox and follows the
   session's network profile.
2. Frames are painted into the View at its size and density; touch, scroll and IME go through
   DevTools input; `evaluateJavascript` and `addJavascriptInterface` (bindings + shim) work both
   ways; request interception backs `shouldOverrideUrlLoading`/`shouldInterceptRequest` and serves
   `file:///android_asset` and `content://`; the page accessibility tree is merged into the ACOS UI
   tree as virtual nodes.
3. One Chromium per host, one browser context per session. Verdicts name the engine version and
   `fidelity` states it is host Blink, not the phone's WebView build.
4. Spike gate (2 days): real HTTPS page, JS bridge round trip both ways, tap on web text, frame
   < 50 ms, ≤ 60 MiB per extra session. If it fails, fall back to the Android WebView APK and accept
   the per-session cost.

**ADR-0030 amendment (2026-11-16, after SPIKE-NET).** pasta's defaults leak host loopback (measured:
the guest reached a host 127.0.0.1 listener). pasta runs with `-T none -U none --no-splice
--map-host-loopback none` and no port forwards except ones named explicitly. pasta has no destination
ACL, so RFC1918, link-local and CGNAT ranges are dropped by netfilter rules installed in the session
netns before the guest starts. That must be a real boundary: the guest has no capability over that
netns (no CAP_NET_ADMIN, AF_NETLINK blocked by seccomp), and a test proves the guest cannot change
the rules. Default stays `off` until the A/B gate (item 6) passes.

**ADR-0031 amendment (2026-11-16, after SPIKE-WV2).** The host-Chromium engine passed every gate on
the host (page load, JS bridge both ways, AX tap, 3–8 ms frames, +32 MiB PSS per extra session, ~202
MiB once per host). The in-session gate could not run: the framework cannot load a provider package
until the package manager knows a second package, and a pre-existing `PackageInfo` parcel defect
(String16 arrays written where AOSP writes `writeString8Array`) desyncs `getPackageInfo` for any
app. Decision: keep the host-Chromium design; the in-session gate is re-run in v0.4.0 right after the
multi-package PMS foundation lands; the parcel defect is fixed in v0.3.0 as its own root-cause item
(audit every ACOS parcelable writer against AOSP `writeToParcel`). The Android WebView APK fallback
stays rejected.

**ADR-0031 amendment (2026-11-17, WVFAIR).** Frame production stays serialised per engine — two
targets calling `beginFrame` at once crashed the pinned engine — but the serialisation is now a
*fair* queue (`crates/acos-webview/src/fair.rs`), not one mutex: one attempt in flight, bounded by
750 ms while another view is waiting (2 s when the engine serves a single view, i.e. the budget it
always had), one outstanding pull per view (coalesced), priority for the frames the input path needs,
and structured timeouts instead of unbounded blocking. Frame pulls run on their own threads, so they
never occupy the connection's worker pool behind which input and JavaScript-interface calls queue.
Measured: a two-view host test (one deliberately slow page) puts the fast view's pulls at 1 ms median
/ 927 ms worst case and a tap ack at 65 ms, where the pre-gate broker starved them (1943 ms pulls,
1085 ms tap acks, 8 concurrent pulls of one view serialised at 17 s); the ART two-WebView scenario
holds a 3 ms tap→page-click. The guest side follows the same rule: `evaluateJavascript` is async
(Android's contract), and a post-input `ax` read is bounded at 250 ms, so one busy page cannot stall
the guest's UI thread. The WebView provider APK is now **shipped by the image**
(`tools/extract-bootclasspath.sh` builds it with `tools/build-webview-provider.sh` and stages
`.cache/boot/android-34/system-apps/com.android.webview.apk`), so a fresh install needs no separate
step; the installer carries it and the module resolves it from the image first.

## ADR-0032 — Settle is not a pixel rule: content stability for every wait and assertion (REL3A, 2026-11-17)

**Context.** v0.3.0 agent feedback (two independent agents) showed the settle model still leaked into
the product in three ways: `wait_for` had no settle option and always ran the strict (pixel/event) rule,
so a screen whose pixels never stop animating could only time out (31 s `TIMEOUT`, `reason: queue` /
`newer-store`, `animations: off` did not help); `assert_text` advertised `timeout_ms` and ignored it
(it read the bound frame ~1 ms after the action and failed while the result list was still loading);
and `acos_tap` answered `status: pass` with `settle.settled:false` — a false green for an agent that
reads only `status`. MCP `acos_verify` also lacked the `animations` and `native` arguments the CLI has,
while the engine's own hint told agents to use `animations:off`.

**Decision.**

1. **Content stability is a first-class settle rule for every wait, assertion and action.** `wait_for`
   and `assert_text`/`assert_visible`/`assert_not_visible`/`assert_state` take `settle`
   (`strict`|`content`), like `tap`/`type`/`key`/`launch`/`reset` already did. `fast` is rejected there
   at validation time.
2. **Default per step kind.** `wait_for` and the tree/text/state assertions default to *auto*: strict
   first, then content-stable once strict has proven it cannot settle. Actions (`tap`/`type`/`key`/
   `back`/`launch`/`reset`) stay strict by default (an action's frame is evidence; an endless animation
   there is an honest `TIMEOUT`); pixel steps (`capture`, `assert_screenshot`, `assert_region`) always
   need pixel settle. This never weakens a claim: an assertion still requires its condition on a
   *settled* observation of the mode it reports, content stability still refuses pixel and geometry
   claims, and an explicit `settle: content` step fails closed when the backend cannot prove it.
3. **Every step and reply states the rule and its outcome**: `settle: {mode, settled, waited_ms,
   reason}` plus the mode-specific evidence (`bound`, `window_ms`, `traversals`, `pixels_changed`,
   `fallback_from`, ...).
4. **A settle that timed out is never a pass.** A scenario step is `status: timeout` with
   `error.code: TIMEOUT` (error class `infra_timeout`); a live-channel input reply is
   `status: timeout` with `details.reason: "settle_timeout"`, the settle record and the delivered facts
   in `details.delivery`. `settle_timeout_ms` moves the deadline only.
5. **`timeout_ms` is a real wait on assertions too.** With it, a tree/text/state assertion polls fresh
   settled observations until the condition holds or the deadline passes, and the frame it judged
   becomes the bound frame; without it the old single-observation, no-extra-traversal path is unchanged
   (benchmark metrics unaffected).
6. **MCP parity:** `acos_verify` gains `animations` (`on`/`off`) and `native`, matching `acos verify`
   (`REL3F`'s one-front-door rule). Every hint that names an option names a real one.

**Consequences.** `wait_for`/assertion settle records now name `content` where they previously could
not, so a verdict may show `mode: content, fallback_from: strict, pixel_evidence: false`. A scenario
that asserted a text on an animating screen used to fail (and was being worked around with a
`settle: content` tap); it now passes with a content record. Pixel assertions on the same screens still
fail (`content_settle_pixel`), as they must.

### ADR-0032 amendment 1 — a placeholder verdict needs a window, not a sample burst (GATEFIX, 2026-11-17)

**Context.** The v0.3.0 release gate found `launch` + `settle: content` answering
`PLACEHOLDER_CONTENT` on a quiet host (2/32 matrix negatives; the same rows passed on a busy host).
A content launch binds its first frame with the guest's `fast` rule and then proves the tree content
stable on the host. The pre-content mount wait was the G3c/G3f1 loop: repeat a re-traversal while the
tree is a window skeleton, capped at `FIRST_FRAME_MAX_ATTEMPTS = 8`. A `fast` traversal has no change
gate, so on an idle host the eight samples cost ~0 ms: the loop read the guest's cached skeleton eight
times and declared `placeholder` before the app could draw. ADR-0032 already says content settle is a
tree rule, not a pixel rule; it did not say what "the app never mounted" needs as evidence.

**Decision.** A placeholder is a claim about the app, so it gets the same shape as every other settle:
a **window plus a guest signal**, never a count of samples.

1. The mount wait is a *change* wait for every settle rule. It hands the guest the tree it just
   published (`baseline_tree`) and asks with the strict change rule, whatever the caller's rule is
   (the caller's rule is proven on the mounted tree afterwards). It is bounded by the
   `ACOS_ART_FIRST_FRAME_MS` window, not by a traversal count; a guest that answers before the window
   has run out is paced (`FIRST_FRAME_PROBE_PACE_MS`) and observed again.
2. `placeholder` requires the skeleton to be stable **for the whole window** (report:
   `first_frame.window_ms`, `window_spent`) with no frame/pixel movement. Eight samples in a
   millisecond are not evidence of an unmounted app.
3. A wait that cannot observe the window (guest error) yields neither a placeholder nor a pass: the
   launch returns the guest's own structured error (`first_frame_window_unproven` when there is none).
   Never a false placeholder, never a false pass.
4. The same rule for `reset`. Nothing else changes: a real placeholder still blocks with the log hint,
   and a text-free mounted screen is still `placeholder_suspect` (pixels moved), not blocked.

**Consequences.** A launch that really never mounts now spends the content window (default 5 s) before
`PLACEHOLDER_CONTENT` — it already did on the strict path, and the content path now matches it. The
reply's `first_frame` block names the window and whether it was spent, so a gate can check the
evidence instead of trusting the verdict.

`tools/benchlib.py`'s failure taxonomy gained the protocol's assertion outcomes (`ELEMENT_OFFSCREEN`,
`AMBIGUOUS_SELECTOR`, `TREE_TRUNCATED`, `END_OF_LIST`, `INPUT_NOT_HANDLED`): a correct negative that
fails with one of them is a `correct_negative`, never an `unclassified` row (v0.3.0 gate F2). The
matrix's default fixture set is built on demand (`tools/run-matrix.sh` → `tools/build-apps.sh
ACOS_APP_ONLY=<missing>`) so a stale `fixtures/build` cannot produce an `APK_IO` row (F3).
ADR-0030 note (SPIKE-NET2): the guest has no path to netfilter at all: `AF_NETLINK`, raw `AF_INET`
and `SO_MARK` are denied by seccomp, CapEff is 0, and the guest userns is not the netns owner; a
session whose drop rules cannot be installed fails closed. Session 1's AF_INET seccomp branch was dead
code; the fixed program has an interpreter test.

**ADR-0030 amendment 2 (owner, 2026-11-16, after SPIKE-NET3).** Real internet is proven in-app
(HTTP 200 via dnsproxyd, validated network, LinkProperties parity; host loopback and LAN blocked).
A per-session pasta costs ~28 MiB RSS, so the default is `off` (owner rule: on only if metrics do not
worsen). Owner decision: use **one shared network relay** for all sessions instead of one pasta per
session. The shared design must keep per-session isolation (no session can see or reach another's
traffic, per-session online/offline, latency and HAR controls), keep the host-loopback/LAN boundary and
fail-closed behaviour, and cost near zero per session; it needs its own design note and a security
review before implementation. Per-session pasta remains the proven fallback.

**ADR-0029/0030/0031 amendment (owner, 2026-11-16): security takes a back seat.** ACOS is a tool
agents use while developing apps, not an OS end users run. The mandatory security reviews in
ADR-0030 and ADR-0031 are dropped. The shared network relay needs no inter-session traffic isolation
beyond correctness (sessions must not disturb each other's results or controls). Cheap safeguards
stay: no default guest route to host localhost services (it also protects the MCP daemon and
`acos view`), explicit host-port opt-in (Metro), and fail-closed where it costs nothing. Any hardening
that costs RAM or latency is out. Verdict honesty (0 false passes) is unaffected.

**ADR-0029 note (MODNOTIF2, 2026-11-16): service-layer option C, built first.** The orchestrator chose
option B (system UI from guest system apps); MODNOTIF2 built the option-C layer every option needs,
without any window/dialog code. Three decisions were taken while building it:

1. **A module may replace a `core` name.** `ModuleManifest::replaces` names the `ServiceManager` names
   a module takes over (`permission` today; `notification`/`statusbar` when their modules land). The
   module registers the same descriptor, so attaching *overwrites* core's factory and detaching must
   put it back. `ServiceHost::end_scope` only deleted what a scope added, so a detached module kept
   answering; `RegistrationScope` now records the overwritten descriptor's factory/binder and restores
   them (and the host's notification/permission state slots). Core answers a replaced name only while
   the module is `off`, so a session without the module behaves exactly as before.
2. **One runtime-permission table, published on the host.** `crate::permissions::PermissionStore` is
   created by `registry::install` and shared by `PmsState`, `AtmsState` and the `system.permissions`
   module: `PackageManager.checkPermission` and `ActivityManager.checkPermission` cannot disagree, and
   a grant changes both. A permission with no runtime entry still answers from the manifest, so
   install-time grants keep working; a runtime (dangerous) permission now starts **not** granted, as on
   a phone.
3. **`requestPermissions` never resolves to a grant ACOS cannot complete.** The framework starts an
   `Intent` at `com.android.permissioncontroller` and expects `onRequestPermissionsResult`; ACOS
   installs no such app and has no activity-result delivery from the system side
   (`IApplicationThread` + `ActivityResultItem`), so the ATMS answers the structured
   `PERMISSION_UI_UNAVAILABLE` with the configured state (`granted` when everything is already granted
   — what `GrantPermissionsActivity` would return without drawing — else `denied`), naming the
   system-app work. A pre-granted request therefore *still* fails, because the result cannot be
   delivered; that delivery path is the first thing the permission-controller system app needs, and it
   belongs to the multi-process/`mproc` work, not to the service layer.

**ADR-0029/0031 note (WV8, 2026-11-16): `webview` is a module, and the two decisions it carries.**
The WebView track is finished as an ADR-0029 module (`crates/acos-services/src/modules/webview.rs`),
so the last two open questions get a recorded answer instead of an implementer's choice:

1. **Viewport contract (decision).** `WebSettings.setUseWideViewPort(false)` — Android's default and
   what this provider always did — means the layout viewport is the `View`'s width in CSS px
   (`device px / density`) and `<meta name="viewport">` is **ignored**; that is Chromium's
   `mobile: false`. `setUseWideViewPort(true)` honours the page's meta viewport and, without one,
   uses Chromium's Android default of 980 CSS px (`mobile: true`), which is what a phone does.
   `setLoadWithOverviewMode(true)` is a *visual* scale on top (`Emulation.setPageScaleFactor`,
   `cssWidth / layoutWidth`), so it never changes the layout width. Both arms are measured against the
   pinned engine (`crates/acos-webview/tests/wv8_viewport.rs`) and through the guest provider in
   `scenarios/webview-probe.yaml` (pages with and without a meta tag, layout width read from the page).
2. **Frames stay at the engine's CSS resolution (decision, follows WV7's measurement).** The wire
   reports the frame's real pixel size and the guest scales it into the `View`. This pinned
   `chrome-headless-shell` cannot capture at `deviceScaleFactor` under `--enable-begin-frame-control`
   (`results/handoff/WV7/evidence/engine-capture-facts.txt`), so a device-resolution PNG is not
   something a setting can turn on — it needs an engine whose capture path honours
   `deviceScaleFactor`. A frame that arrives while nothing else draws is still painted only on the
   next draw; that defect belongs to the LOOPERWAKE track (guest invalidation → traversal → vsync)
   and is *not* worked around here: the scenario tolerates it with a generous `timeout_ms` instead of
   hiding it.
3. **`off` is Android's own no-provider path, not a tombstone.** With the module unattached there is
   no `com.android.webview` package, so `hasSystemFeature("android.software.webview")` is false and
   `WebViewFactory` throws its own `UnsupportedOperationException`; `webviewupdate` stays `core`'s and
   answers from the package table (a module that re-registered it would be a second source of truth).
   `real` installs the provider APK as a system app (`system_apps`, resolved by
   `module::system_apks`) **at launch** and binds the session's gate socket; the one host Chromium is
   still started by the first guest connect. There is no `stub`/`scripted` profile: a scripted page
   would be a fake success.
**ADR-0030 amendment 3 (owner, 2026-11-16, after NETSHARE).** Owner chose design **B2**: one relay
netns per host served by one pasta (26 MiB once), each session keeps its own netns joined to it by a
veth pair (~0 MiB per session, 2 ms attach floor); per-session online/offline and latency/loss are
rules/netem on that session's veth. Measured: BASE 24 MiB per session (494 MiB at N=20, 887 ms
attach), B2 flat 26 MiB from N=1 to 20; aggregate ~15–18 MB/s through one pasta (accepted for a dev
tool). Orchestrator decisions on the open questions: the LAN/host-loopback drop rules live in the relay
netns (guest has no capability there); a dead relay is fail-closed (every session reports the network
unavailable, never a silent offline) and the supervisor restarts it; host ports (Metro) are opened per
session on request by a REDIRECT on that session's veth; the relay supervisor and `acos-netd`
(dnsproxyd) live in the existing host broker; per-session pasta (BASE) stays as a fallback arm for
one release. Option A (own userspace stack) remains the upgrade path if the throughput ceiling bites.
ADR-0030 note (NET1, orchestrator): the relay supervisor is its own lazily started process
(`acos-netd`), not code inside the MCP broker, because it must own the user namespace that holds the
relay netns and every session netns, and plain CLI use has no broker. Host-port opt-in is pasta
`-t <port>` plus a per-session DROP for sessions that did not opt in (simpler than an fd-passing
REDIRECT proxy). Attach cost 15–49 ms (exec-based) is accepted for v0.4.0; rtnetlink (~2 ms) moves to
the v0.5.0 density work.
ADR-0030 correction (NET3, measured on the pinned pasta): the `-t <port>` + DROP shape above is
superseded. `pasta -T <port>` serves only the target netns, never a relay-joined session, and pasta
rejects address-qualified `-T` (`Listening address not allowed`). Host-port opt-in is therefore the
gated `--map-host-loopback` address plus a per-session `ACCEPT` for an opted-in port, with a small
`acos-netd forward` binding the session's own `127.0.0.1:<port>`; sessions that did not opt in get a
tcp-reset (that is what "refused" means), not a `DROP` (a DROP stalled guest boot ~4.5 s). Evidence:
`results/handoff/NET3.md`, branch `net3`.

ADR-0031 note (WVB, orchestrator): the broker is one shared chrome-headless-shell 157.0.8103.78 per
host (sha256-verified into `.cache/webview/engine`), DevTools over `--remote-debugging-pipe` only, one
browser context per session. Engine sandbox: bwrap with the host root read-only and `/home`, `/root`,
`/run/user` masked by tmpfs (profile + engine re-bound); an empty-root layout makes the engine die
with SIGTRAP before logging, and full root minimisation is not pursued (security takes a back seat for
this tool). Chromium's own namespace sandbox stays on. Measured: fixed 130 MiB PSS, +3.9 MiB per extra
session context, frame p95 15.6 ms at 1080x2400. Activation is lazy (no Chromium for sessions that
never create a WebView); the per-session socket lives in a short per-session dir mounted at `/run/acos`.

ADR-0029 amendment (owner, 2026-11-16, after MPROC4): **the host builds launch transactions.** A
launch into an already-running process (and every later launch flavour: extras, results, flags,
singleTop/onNewIntent) is a `ClientTransaction` with `LaunchActivityItem` (+ lifecycle items) encoded by
the ACOS activity manager from the image dex shape and delivered on the existing host transport, the
way system_server does; no ACOS helper inside the app builds launches (option B rejected). Codecs are
checked byte-for-byte against real Java (the MODNOTIF4 differential). MPROC4's guest-marshalled first
launch of a freshly spawned process is migrated onto the same encoder.

**ADR-0029 amendment (owner, 2026-11-16, after MODNOTIF): system UI comes from system apps in the
guest.** The permission dialog and the notification shade are provided by small ACOS system apps
installed in the session image and run as their own ART process (multi-process foundation): a
`com.android.permissioncontroller` package with `GrantPermissionsActivity` answering the framework's own
`buildRequestPermissionsIntent`, and an ACOS shade/system-UI app. The same mechanism later provides the
launcher, Settings targets and the share chooser. ACOS does not paint host-side windows (option A
rejected). The `notifications` and `permissions` service layers (state, control ops) are built now;
names a module replaces (`notification`, `statusbar`, `permission`) move out of `core` into their
modules so detach restores the core answer (orchestrator).
ADR-0029 note (MODSENS, orchestrator): for a service whose framework client caches the proxy
(sensorservice, location), `core` publishes the binder node and the module fills it
(`core_module::FILLED_BY_MODULE` + `ServiceHost::dynamic_object_mut`); detach empties the node back to
the honest off answer. This is the framework pattern for such names.

**ADR-0029 amendment note (SYSUI1, 2026-11-17): the permission dialog is the guest system app, and
the host routes it there.** The `permissions` module now declares
`system_apps: ["com.android.permissioncontroller"]`; the launcher stages that APK through the one
`modules::system_app_apk` resolver (the mechanism WV8 introduced for the WebView provider), installs
it as a second package, and the ATMS resolves the framework's own
`PackageManager.buildRequestPermissionsIntent` (`ACTION_REQUEST_PERMISSIONS` + package) to its
`GrantPermissionsActivity`. The controller runs in its own ART process, spawned lazily the way a
second app is (`request_process` → launcher); the host launches it with a host-built
`ClientTransaction`, and the activity result comes back on the same MPROC7 path — with the AIDL
`resultWho` the framework passed (`"@android:requestPermissions:"`), which is what makes
`Activity.dispatchActivityResult` call `onRequestPermissionsResult` with the AOSP shape
(`EXTRA_REQUEST_PERMISSIONS_NAMES` + `EXTRA_REQUEST_PERMISSIONS_RESULTS`). The host applies the
answer to the one runtime-permission table the two `checkPermission` paths read.

Three contract details, each documented where it lives:

1. **The resolved component is written onto the launch intent.** AOSP's `ActivityStarter` does
   `intent.setComponent(…)` before it schedules the launch; `ActivityThread.performLaunchActivity`
   reads `r.intent.getComponent()` and crashes on null. An implicit start therefore records the
   resolved component (SYSUI1 found this on the first ART run of the dialog).
2. **The two facts the controller cannot query are passed in the launch.** A real controller calls
   `IPermissionManager.getPermissionFlags`; the ACOS guest app cannot reach the hidden interface, so
   the host puts `acos.permission.GRANTED` / `acos.permission.FIXED` (and the requesting package) in
   the launch extras, and the app does what `GrantPermissionsActivity` does with them — skip
   already-granted permissions, answer "don't ask again" ones denied, and return without drawing when
   nothing is left to ask. The result Intent carries the AOSP extras plus
   `…REQUEST_PERMISSIONS_NEVER_ASK_AGAIN` for the "Don't ask again" answer (AOSP's controller calls
   `updatePermissionFlags(FLAG_PERMISSION_USER_FIXED)` itself; this is the same flag, one hop later).
3. **`system_ui: dialogs|off`** (default `dialogs` for the `real` profile). `off`, an uninstalled
   controller, or a detached module keeps MODNOTIF2's structured `PERMISSION_UI_UNAVAILABLE` — never
   a silent grant.

Open, not decided here: the controller's process launch is subject to the same lazy-spawn launch race
`mproc3-two-process` exercises; the pre-granted arm of the SYSUI1 scenarios hits it
(`host-transaction-timeout`, `results/handoff/SYSUI1/pregranted-finding/`), while the two dialog arms
pass. The shade/system-UI app (item 2 of the track) is not built; the notification layer's control
path is where `open_shade` belongs.

**ADR-0029 settlement note (SYSUI3A, 2026-11-17): a tap that closes the tapped window is a pass,
and the primary process is named by the launcher.**

Two rules this track had to settle, both applied in one place each:

1. **The settle contract for a dismissed window.** The guest's strict settle reports `detached`
   (a root it still holds has no `ViewRootImpl`) or `no-draw-after-action` when the window the tap
   landed on has gone away. That is a *completed interaction*, and reporting it as a `TIMEOUT` was a
   harness bug (the notification shade's row tap). The backend now proves the removal against its own
   window record — a tap, a host-observed delivery with a handled guest ack, the top-most touchable
   pre-action window containing the point, that window absent afterwards, something still on screen —
   and publishes `settle.bound: "window-removed", handled: "window-removed"`; `acos-verify`'s
   `eval_tap_change` is the one place the tap contract accepts it. A window that vanishes with no
   delivered input keeps its timeout (the guard, with tests).
2. **The primary process's package is the launcher's fact.** `attachApplication` carries no package
   and the primary never sends the peer handshake, so `ServiceHost::seed_primary_package` records it
   where stock AMS's `startProcessLocked` would, and `Peer::package_of` never answers an empty name.
   Without it a `startActivity` into the already-running primary (a notification's content sender) was
   recorded and never encoded.

## ADR-0031 settlement note (WVTAIL, 2026-11-18): input latency is a *frame* rule, and a restart is a restart

Three rules the WebView track had to settle, each measured first
(`results/handoff/WVTAIL/`):

1. **An input ack is not a deadline.** Under `--enable-begin-frame-control` the engine
   acknowledges a dispatched touch event at the end of the compositor frame that
   **follows** the event's own. The broker produces that frame itself (input
   priority, so it is granted ahead of guest pulls) and then reads the ack;
   waiting a fixed timeout instead was 2 s of blocked guest input per event
   (2004-2604 ms per tap), because the guest cannot draw the frame it is waiting
   for while it is inside its own touch dispatch. `ack: false` is reported and
   logged, never turned into a lost tap or a fabricated `true`.
2. **A queued input frame preempts a non-input attempt.** An attempt never holds
   the engine for its whole fairness budget when an input pull is waiting: it
   waits in slices and abandons (`Yield`, which spends no attempt of its pull's
   budget). Input frames keep their own budget in full — cutting *that* is what
   drops gestures (WVFAIR §7.1). An abandoned `beginFrame` is not overlapped by
   the next one for the same view; its reply is drained and kept, because two
   outstanding frames for one page is an engine-side fault
   (`Got overlapping IssueExternalBeginFrame`).
3. **A restart is a restart.** When the engine is replaced every target of every
   session dies with it; the broker keeps saying so (`engine.restarted`) and
   answers the old view ids `ENGINE_RESTARTED`, never `NO_VIEW` — a `NO_VIEW`
   means the id was never the session's, and conflating the two made an engine
   restart read as a broker bookkeeping bug (MERGE040A §5.2). The restart is
   logged with the engine's exit status, its stderr tail and the number of views
   dropped. The known cause of a *cross-session* restart is the WebView daemon
   living in its spawner's cgroup (a session's scope ending kills the host's
   engine); moving it out is a process-model decision for the owner.
