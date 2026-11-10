# ACOS — Tasks and phases

**Status:** P0 in progress. Phases P1–P9 are planned. This file is the work
backlog; `STATUS.md` is the dated truth and `EXPERIMENTS.md` is the history.

A phase completes only when it demonstrates an observable, correctly classified
capability on **real executable input**, passes its dedicated differential tests,
and reports measured resources against the reference. Architecture and synthetic
demos do not complete a phase.

---

## P0 — Reference inventory, benchmark foundation, workspace **(IN PROGRESS)**

| # | Task | Owner | Status |
|---|---|---|---|
| P0.1 | Read the full handoff package (`docs/handoff/00`–`06`, kickoff) | docs owner | done |
| P0.2 | Read-only inventory of legacy ACVM; reconcile claims vs. tree/commits | docs owner | done → `docs/reference/ACVM_INVENTORY.md` |
| P0.3 | Record ADRs 0001–0006 and host-safety rules | docs owner | done → `DECISIONS.md` |
| P0.4 | Write SPEC / ARCHITECTURE / COMPATIBILITY / BENCHMARKS / TASKS / STATUS / EXPERIMENTS | docs owner | done |
| P0.5 | Clean Rust workspace with an executable CLI + typed JSON contracts + tests | other agents | **in progress** (`crates/acos-dex` scaffolded; `acos-apk`, `acos-cli` pending) |
| P0.6 | Define the benchmark method record and the fixed T01–T12 task suite | other agents + docs | **next** (schema in `BENCHMARKS.md` §2.1/§4.1) |
| P0.7 | Build a licensable fixture corpus (Java/Kotlin/NDK; good + seeded-wrong variants) | other agents | **next** |
| P0.8 | Measure the legacy ACVM/emulator reference on equivalent tasks under resource limits, before any "speedup" | other agents | blocked on authorisation/RAM (H4) |

**P0 gate:** baseline reproducible; constraints and user stories defined; legacy
repo unchanged. Gate is **not yet met** (P0.5–P0.8 outstanding).

---

## P1 — Real APK and DEX loading in Rust

| # | Task | Gate/evidence |
|---|---|---|
| P1.1 | APK/split-APK ZIP + binary `AndroidManifest.xml` (AXML) + `resources.arsc` parsing | parse corpus, no silent omission |
| P1.2 | Signature/ABI/native-dependency inventory | classify supported vs `WRONG_ABI` |
| P1.3 | DEX 035–039 header/tables/class_data/code_item parse | valid/invalid fixtures |
| P1.4 | DEX verification (register/type/control-flow/bytecode) | agreement with Android verifier |
| P1.5 | Decode actual DEX bytecode **compiled from Java/Kotlin** (not a custom IR) | decode report |
| P1.6 | Execute that bytecode with arithmetic, branches, methods, objects, exceptions | real compiled app DEX runs and is reproducibly correct |
| P1.7 | Differential comparison vs. reference; structured reject of invalid DEX | `jvm-reference` first (ADR-0005) |

**P1 gate:** a real compiled test app's DEX runs through the Rust engine and is
reproducibly correct. Parsing APKs alone is insufficient.

---

## P2 — Rust Android-compatible managed runtime

| # | Task | Gate/evidence |
|---|---|---|
| P2.1 | class loaders, class initialization ordering, multi-DEX | fixture behaviour matches reference |
| P2.2 | object model, method dispatch, arrays, fields | property-based tests |
| P2.3 | GC + roots + weak/global refs + finalizers as required | stress, no leaks |
| P2.4 | threads, monitors, exceptions, Java memory model, stack traces | multithreaded fixtures |
| P2.5 | expand DEX opcode coverage toward the full spec for API 34 | opcode coverage report + perf counters |
| P2.6 | optional JIT/AOT spike (Cranelift) **after** interpreter correctness | measured improvement, W^X/security recorded |

**P2 gate:** complex JVM/DEX fixtures and multithreaded app logic pass with
correct JNI-independent behaviour and no engine leaks.

---

## P3 — Rust JNI, ELF and native compatibility

| # | Task | Gate/evidence |
|---|---|---|
| P3.1 | Rust `JavaVM`/`JNIEnv` tables; refs, strings, arrays, calls, `RegisterNatives`, version negotiation, attached threads, exceptions | ABI-correct behaviour |
| P3.2 | ELF loading/linking, relocations, symbol resolution, Bionic behaviours, JNI `dlopen` | real ELF JNI test |
| P3.3 | opt-in ABI translation for ARM-only guests on x86_64 (deferred) | `WRONG_ABI` until then |

**P3 gate:** a real NDK app loads a `.so` and calls both directions correctly;
fuzz symbols and boundary cases. No native-support claim before this.

---

## P4 — App lifecycle and minimal useful Android APIs

| # | Task | Gate/evidence |
|---|---|---|
| P4.1 | Activity/Task basics, intents, configuration handling, storage, permissions baseline | lifecycle oracle |
| P4.2 | start an actual APK's Activity; reach a meaningful UI/event state; back/forward | authentic Views APK launch + input + lifecycle |

**P4 gate:** an authentic Java/Kotlin Views APK launches, responds to input, has
correct lifecycle and produces native or explicitly reference-verified output.

---

## P5 — Offscreen rendering and direct agent observation

| # | Task | Gate/evidence |
|---|---|---|
| P5.1 | offscreen surface on demand; font/layout/resources | parity across density/theme |
| P5.2 | text/semantics tree (text/content-desc/ids/roles/bounds/visibility) | semantic oracle |
| P5.3 | input injection + routing | action→state oracle |
| P5.4 | request-sequenced frame freshness (sequence, causal request, hash) | no stale frame possible |
| P5.5 | distinguish meaningful frame from black placeholder | seeded black/defect detection |

**P5 gate:** agent code-change → built APK → native run → interaction →
verifiable screenshot + assertions → cleanup, without full Android boot.

---

## P6 — Real parallel workloads and isolation

| # | Task | Gate/evidence |
|---|---|---|
| P6.1 | `shared-task`, separate process, per-app data namespaces, resets | isolation oracles |
| P6.2 | independent input/event routing, focus per surface | no leakage |
| P6.3 | budget enforcement + capacity planning | correct admission |
| P6.4 | bench N=1/2/5/10/20 separate apps and same-app | real capacity reported |

**P6 gate:** multiple real apps concurrently receive the right input, produce
independent frames/traces, and do not compromise host security.

---

## P7 — Broader app ecosystem

Compose, Flutter, React Native, WebView, native graphics, background services,
notifications, media, camera, sensors on demand; split APKs; 32/64-bit and ARM
strategy; permission/security fidelity; lifecycle stress.

**P7 gate:** reproducible class-by-class compatibility score and a realistic
agent dev loop on representative apps.

---

## P8 — High-compatibility / performance engineering

Broader SDK/NDK surface; JIT/AOT; profile-driven class sharing and app templates;
efficient offscreen graphics; cheap reset/park; sandbox hardening; validated
complex apps and appropriate CTS subsets. Full CTS is a separate question.

**P8 gate:** measured end-to-end performance, explicit coverage, stable
regressions, no silent false positives.

---

## P9 — Optional production-grade distribution

CI artefacts, reproducible builds, signed releases, SDK/MCP packages,
IDE/language-server integration, Linux host install, capacity planner,
observability, sustained-run safety — only after live compatibility, threat
model, incident recovery and licence compliance are credible.

---

## Immediate next tasks (concrete)

Ordered; `[docs]` = docs-owner scope, `[impl]` = other agents.

1. `[impl]` Land `acos-apk` + `acos-cli` scaffolds and make `acos inspect
   <apk> --json` parse a real APK to a typed report (no execution).
2. `[impl]` Land `acos-dex` parse + decode for DEX 035–039 with a
   valid/invalid fixture and a coverage report skeleton.
3. `[impl]` Self-author the minimal fixture trio (Views Activity, arithmetic/
   state dependency, seeded-wrong variant) and build them with the SDK tools
   (ADR-0004); record tool versions + hashes.
4. `[docs]` Freeze the benchmark method record and the T01–T12 manifest
   template once impl picks the first task family.
5. `[docs]` Keep `COMPATIBILITY.md` at `none` until evidence exists; advance one
   row per verified artifact.
6. `[impl]` Produce the first `jvm-reference` differential harness for pure-Java
   semantics (ADR-0005 tier 1), explicitly labelled weak.
7. `[lead]` Decide the H4/art-reference authorisation + RAM budget before any
   emulator/ACVM oracle run.

No task may start an emulator, download >2 GB, use `sudo`, or touch the legacy
repo.
