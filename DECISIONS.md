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

**Status:** accepted (tier 2 deferred pending RAM/authorisation check).

**Context.** Correctness must be judged against a reference. The legacy engine
and the API-34 system image are available but heavy; the host has OpenJDK 17.
The host is an actively used workstation with a 60 GiB RAM and an 85 % guard.

**Decision.** Two oracle tiers, each result carrying an explicit fidelity label:

1. **`jvm-reference` (weak oracle):** host OpenJDK 17 running the same Java
   source. Valid only for pure-Java semantics (arithmetic, data structures,
   standard-library behaviour). It does **not** model ART, Android framework,
   resources, JNI or rendering.
2. **`art-reference` (authoritative):** real ART on Android 34 (emulator or the
   frozen legacy ACVM core). Deferred until host RAM and explicit authorisation
   are checked.

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

**Open question.** Whether tier 2 will be a booted emulator, the frozen ACVM
core, or a hosted device — pending RAM and authorisation.

---

## ADR-0006 — Core-library strategy is OPEN (hand-written intrinsics first)

**Status:** accepted as an ordering recommendation; the licence question is open.

**Context.** Executing real DEX requires `java.lang`/`java.util`/`javax.*` (and
eventually `android.*`) semantics. Two options:

- **(a) Hand-written Rust intrinsics** for a minimal `java.lang` subset first.
- **(b) Later load real core-library DEX** (e.g. AOSP `core-oj`/libcore) and
  execute it with the Rust VM plus Rust natives.

**Decision.** **Recommend (a) now → (b) as the scalable path.** Start with
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
