# ACOS — Status

**As of 2026-11-10.** Honest, dated snapshot. `TASKS.md` is the backlog;
`EXPERIMENTS.md` is the history.

## One line

**Nothing executes yet.** ACOS is at the end of P0: the workspace exists, a
handful of crates are being scaffolded, and the specification, architecture,
decision log, compatibility ledger, benchmark method and reference inventory are
written. No APK has been parsed by ACOS, no DEX has been verified, no
instruction has been interpreted, and no frame has been rendered.

## What exists today

| Area | State |
|---|---|
| Git/Rust workspace | exists at `~/Projects/acos`; `Cargo.toml` (edition 2021, rust-version 1.90); no commits yet |
| Crates | `crates/acos-dex/` scaffolded (Cargo.toml only); `acos-apk`, `acos-cli` pending — other agents |
| Docs | `SPEC.md`, `ARCHITECTURE.md`, `DECISIONS.md`, `COMPATIBILITY.md`, `BENCHMARKS.md`, `TASKS.md`, `STATUS.md`, `EXPERIMENTS.md` (this set) |
| Reference | `docs/reference/ACVM_INVENTORY.md` (read-only audit of the frozen legacy engine) |
| Fixtures | none |
| Results | none |
| Executable capability | **none** |

## Capability level

| Level | Available? |
|---|---|
| H0 host tooling inside ACOS | no |
| H1 host renderer | no |
| H2 native Rust runtime | no |
| H3 isolated native runtime | no |
| H4 ACVM/device oracle | not wired; legacy engine frozen, use requires authorisation + RAM check |

Every cell in `COMPATIBILITY.md` is `none`. There is no verified Android
capability and no benchmark result.

## Decisions in force

- ADR-0001 clean-sheet Rust workspace; managed runtime in Rust, no `libart`.
- ADR-0002 first target Android API 34, x86_64; DEX 035–039 parse versions.
- ADR-0003 smallest cohesive crate set (`acos-dex`, `acos-apk`, `acos-vm`,
  `acos-cli`), split only on stable interfaces.
- ADR-0004 `javac`/`d8`/`aapt2` allowed as fixture build tools only, never in
  the execution path.
- ADR-0005 oracle tiers: `jvm-reference` (weak, host JDK 17) vs `art-reference`
  (authoritative, deferred); mandatory fidelity labels; a JVM match is never
  Android-verified.
- ADR-0006 core-library strategy open; hand-written Rust intrinsics first, real
  core-library DEX later (GPLv2+Classpath licence question open).
- Host safety: RAM guard 85 %, no privileged containers, no `sudo`, no >2 GB
  downloads without authorisation.

## Blocked / waiting

- **H4/art-reference oracle** (emulator or frozen ACVM core): needs host-RAM and
  explicit authorisation; legacy AOSP track is frozen at `aosp-track-final`.
- **Real AES/core-library sources**: licence review pending; no download.
- **Fixture corpus**: not yet authored; third-party legacy APKs are not in the
  repo and its helper APKs use a research platform key.

## Next checkpoint (P1 gate)

A real compiled Java/Kotlin test app's DEX is parsed, verified and **executed**
by the Rust engine and is reproducibly correct under a `jvm-reference`
differential — with a fidelity label and a negative case. Until that artifact
exists, `STATUS.md` stays "nothing executes yet".

## Change discipline

- Every accepted change improves a **verified** capability or a **measured**
  metric; no regression disguised as progress.
- No compatibility claim without an evidence artifact; `COMPATIBILITY.md`
  advances one row at a time.
- Failures and false passes are recorded in `EXPERIMENTS.md`.
