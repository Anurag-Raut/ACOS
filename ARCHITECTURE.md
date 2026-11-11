# ACOS — Architecture

**Status:** clean-sheet; the first crates are committed, the rest are in
progress. This document is kept in line with the repo. It is not a claim that any
execution capability works — see `STATUS.md` for the dated truth and
`docs/V1.md` for the V1 definition.

Owner directives D1–D4 (`SPEC.md`) and ADR-0007…ADR-0017 govern this design. The
handoff architecture is **not** binding (D3); what is fixed is the outcome: APKs
behave as on Android and an agent can close the verification loop as fast as
possible.

---

## 1. Layering (authoritative)

```text
        Agent / CI / MCP client  (the only consumer — D1)
                    │  machine-first JSON / MCP, stable ids, structured errors
                    ▼
        acos-cli  +  acos-protocol           [acos-cli committed; protocol planned]
                    │
                    ▼
        Job / task runner: scenario, isolation, evidence, metrics
        acos-verify (planned), acos-bench (planned)
                    │
     ┌──────────────┴───────────────────────────────────────────────┐
     │                    REAL Android 14 framework                 │
     │   the boot-classpath bytecode (ADR-0007), executed by:       │
     │                                                              │
     │   acos-vm  ── interpreter │ handle/arena heap │ precise GC   │
     │             │ one class provider: DEX or Rust built-in       │
     │             │ native registry keyed by (class,name,sig)      │
     │             │ explicit VM frame stack (no host recursion)    │
     └──────────────┬───────────────────────────────────────────────┘
                    │  native methods                     native methods
        ┌───────────┼───────────────────────────┬────────────────────┐
        ▼           ▼                           ▼                    ▼
  acos-render   Rust in-process           acos-jni +          Rust core-library
  (ADR-0011)    service layer             acos-native-loader  natives
  software 2D   (ADR-0008): no            (ADR-0014):         (ADR-0007), later
  rasteriser,   system_server, no         JNIEnv/JavaVM       replaced by real
  text shaping, Binder kernel driver,     tables in Rust;     core DEX where
  RenderNode/   no SurfaceFlinger,        ELF/Bionic loader;  possible
  display list  no HWUI/Skia, no zygote   UNSUPPORTED_NATIVE_
                                          SYMBOL on gaps
                    │
                    ▼
        Evidence: fresh frame (seq/cause/hash), UI tree, events, crashes,
        metrics → results/                       [acos-observe planned]
                    │
                    ▼
        Verdict: status/backend/fidelity/isolation + artifacts + resource report
```

**Fixed reference tiers (oracle, not execution):** real ART on the installed API
34 x86_64 emulator (`art-reference`, ADR-0015) and host OpenJDK 17
(`jvm-reference`, weak, ADR-0005). These are launched on demand for comparison
only; they are never on the `acos verify` execution path.

### 1.1 What each layer owns

| Layer | Owns | Explicitly *not* here |
|---|---|---|
| `acos-cli` / protocol | typed JSON envelope, commands, stable ids | app semantics |
| verify/bench runner | scenario, isolation, artifacts, metrics, verdict | Android internals |
| **`acos-vm`** | real framework bytecode execution, object model, GC, frames, natives registry | system services, rendering, JNI host ABI |
| Rust service layer (planned) | the ~10 AIDL interfaces on the app path, in-process | real Binder, system_server |
| `acos-render` | software 2D raster, text shaping, display-list model, frame identity | GPU, HWUI/Skia |
| `acos-jni` + `acos-native-loader` | JNI ABI tables, ELF/Bionic loading, symbol binding | JIT, translation (deferred) |
| `acos-apk` | ZIP/AXML/`resources.arsc`/signing/ABI ingest | policy/trust decisions |
| `acos-dex` | DEX 035–039 parse/verify/decode | execution |

---

## 2. Crate map and current status

**Read from the repo on 2026-11-10.** Status: `committed+verified`,
`in progress`, `planned`. "Verified" means a committed test/artifact exists; it
is not a claim of V1 capability.

| Crate | State | What is actually there | Evidence |
|---|---|---|---|
| `acos-dex` | **committed (5945a57) + verified decoder; in progress** | DEX 035–039 parse, full Dalvik opcode decode, structural verifier, disassembler, `hiddenapi` parser (uncommitted), env-gated boot-classpath test (uncommitted) | `crates/acos-dex/`; `tests/diff_dexdump.sh` → 380 methods / 4,253 instructions / 0 mismatches (`docs/FINDINGS.md` F-0004) |
| `acos-apk` | **committed (5945a57) + verified** | ZIP, binary `AndroidManifest.xml` (AXML), `resources.arsc`, signing-scheme detection, ABI/native/asset inventory; `aapt2` differential test | `crates/acos-apk/`; `tests/differential_aapt2.rs` |
| `acos-cli` | **committed (5945a57) + verified** | `acos inspect <apk> --json` with the JSON envelope (`ok/status/backend/fidelity/request_id/…`) | `crates/acos-cli/`; `tests/cli.rs`; live run of the prebuilt binary produced a correct static report |
| `acos-vm` | **in progress** (untracked) | DEX interpreter, handle/arena heap, precise mark-sweep GC, class model, **temporary core-library built-ins** (bootstrap only, ADR-0007), cooperative threads. ~8.5k LOC; **0 `#[test]` at time of writing** | `crates/acos-vm/` |
| `acos-native-loader` | **in progress** (untracked) | ELF (`.so`) reader and dynamic-info scaffolding; incomplete (`lib.rs`/`build.rs` referenced by `Cargo.toml` not yet present) | `crates/acos-native-loader/` |
| `acos-render` | **in progress** (untracked) | Placeholder `lib.rs`; dependencies declared (`tiny-skia`, `rustybuzz`, `ttf-parser`, `unicode-bidi`, `png`, `jpeg-decoder`, `image-webp`, `roxmltree`, `xxhash-rust`, `sha2`) | `crates/acos-render/` |
| Oracle tooling | **in progress** (untracked) | `tools/oracle/emulator.sh` (headless ART oracle, ≤2 GiB, one instance); `tools/extract-bootclasspath.sh`; `results/oracle/` captured a single 22 s boot; `fixtures/expected-art/`, `fixtures/reference/` empty | `tools/oracle/`, `results/oracle/`, `.cache/boot/android-34/` |
| `acos-jni` | **planned** | JNIEnv/JavaVM tables in Rust against the VM (ADR-0014) | — |
| Rust service layer | **planned** | in-process stubs for the AIDL interfaces on the app path (ADR-0008) | — |
| verify / scenario runner | **planned** | `acos verify --apk … --scenario … --json --artifacts …` (V1) | — |
| snapshot / restore | **planned, UNPROVEN** | heap + class-state snapshot, CoW worker start/reset (ADR-0010) | — |
| `acos-observe`, isolation/`acos-sandbox`, scheduler | **planned** | evidence, isolation, concurrency (V1 items 5–6) | — |

> **Working-tree note (2026-11-10):** concurrent agents have uncommitted edits to
> `acos-dex` (`src/lib.rs`, `src/verify.rs`, `src/annotation.rs`,
> `examples/dexdump.rs`, tests) plus new `acos-vm`, `acos-native-loader`,
> `acos-render`, and oracle tools. At the moment of reading, the working tree
> **did not compile** (`acos-dex` lazy-table edits, 14 errors), so the committed
> state is the verified one. This is expected mid-flight and is reported, not
> hidden.

### 2.1 Workspace

`Cargo.toml` is a workspace with `members = ["crates/*"]`, edition 2021,
rust-version 1.90, Apache-2.0. There is no `acos-protocol`/`acos-verify` crate
yet; the V1 CLI surface is parsed in `acos-cli`.

---

## 3. Data flow: APK → DEX → VM → evidence

```text
 APK / split APK
   │ acos-apk: ZIP, AXML manifest, resources.arsc, signature schemes,
   │           ABI/native/asset inventory, classes*.dex discovery
   │ acos-dex: DEX 035–039 header/tables/class_data/code_item, hiddenapi,
   │           instruction decode, structural verification
   ▼
 class provider (ADR-0009): DEX or Rust built-in behind one interface
   │
   ▼
 acos-vm: load real boot-classpath bytecode (ADR-0007)
   │   handle/arena heap + precise GC + VM frame stack + native registry
   │   ── invokes native (class,name,sig) ──►
   │        ├─ acos-render (android.graphics: RenderNode/Canvas/Paint/…)
   │        ├─ Rust service layer (IActivityManager/IPackageManager/…)
   │        ├─ Rust core-library natives (java.lang/… in the bootstrap)
   │        └─ acos-jni + acos-native-loader (guest .so)
   ▼
 framework code drives lifecycle, layout, draw
   │
   ▼
 evidence: fresh frame (sequence, cause id, content hash), UI tree
 (text/id/role/bounds), events, crash/ANR, metrics
   │
   ▼
 verdict envelope: status/backend/fidelity/isolation + artifacts + resources
```

Invariants:

- **Real inputs only.** Real DEX compiled from Java/Kotlin; real boot-classpath
  bytecode; real guest ELF `.so`.
- **Verify before execute.** `acos-dex` rejects invalid DEX with structured
  errors. DEX SHA-1 is lenient by default per Android semantics (ADR-0017).
- **One class provider.** A class is either loaded from DEX or supplied by a Rust
  built-in; the bootstrap built-ins are temporary (ADR-0007).
- **Natives are explicit.** Registry keyed by `(class, name, signature)`; an
  unbound native is a structured error, never a silent stub.
- **Frames are addressable.** A frame names the request/cause that produced it;
  a stale frame is impossible.
- **Fidelity is attached, not assumed.**

---

## 4. Boot classpath and the in-process service layer

- Boot classpath order (from the image config, `.cache/boot/android-34/BOOTCLASSPATH`):
  `core-oj, core-libart, okhttp, bouncycastle, apache-xml, framework,
  framework-graphics, ext, telephony-common, voip-common, ims-common,
  core-icu4j`.
- The boot classpath is **41,829 classes / 328,352 methods / 5,607 natives**
  (ADR-0007). Native work-list targets: `println` 152 natives; minimal Activity
  612 (lower bound) of which **419 are `android.*`** and 175 core (ADR-0007
  evidence).
- **No `system_server`, Binder kernel driver, SurfaceFlinger, HWUI/Skia or
  zygote** (ADR-0008). The app process's service calls are answered in-process by
  a Rust layer that starts only what a task needs.
- Boot jars are extracted at runtime from the user's installed SDK image into
  `.cache/boot/android-34/` (gitignored); ACOS never redistributes them
  (ADR-0016). ADR-0016 flags two open legal questions before distribution.

---

## 5. Memory, snapshot and capability model

- Object references are 32-bit handles into a VM arena; Java calls use a VM frame
  stack (ADR-0009). This enables compaction, JNI handle tables, and heap
  snapshot/restore.
- **Snapshot/restore (ADR-0010, PLANNED, UNPROVEN)** is the intended answer to
  cold start / reset-to-ready / per-session memory: initialise the boot classpath
  (and later an app) once, snapshot heap + class state, start/reset workers by CoW
  restore. Gate 1: framework init time on the interpreter. Gate 2: correct
  restore of an initialised state. If either fails the cold/interaction targets,
  the architecture is revisited (ADR-0012 alternative).
- **Rendering (ADR-0011):** Rust software 2D; no GPU; no surface unless asked;
  unsupported draw ops recorded per frame.
- **Native code (ADR-0014, ADR-0018 W59-A):** x86_64 first; missing symbol →
  structured `UNSUPPORTED_NATIVE_SYMBOL`; an APK with native code but no
  host-ABI `lib/<abi>/` → `UNSUPPORTED_ABI` (both the CLI gate and the daemon
  RN/Flutter backends); `WRONG_ABI` only for an individual ELF whose
  `e_machine` the loader cannot map. Full table: `docs/ERROR_CODES.md`.
- Measure both `anon+shmem` and cgroup `memory.current`, PSS where permitted,
  host `MemAvailable`, GPU allocations and zram; never mix metric types
  (`BENCHMARKS.md`).

---

## 6. Security posture

- Untrusted APKs never run with host-root-equivalent privilege; privileged
  containers are rejected as an execution boundary.
- All isolation boundaries explicit and reviewed; `sandbox` hardening beyond
  process separation is out of scope for V1.
- Signing/trust checks precede execution; no silent API faking.
- Host safety: 85 % RAM guard, no `sudo`, no global host changes, no >2 GB
  downloads without authorisation, legacy repo read-only.
