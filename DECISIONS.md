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
