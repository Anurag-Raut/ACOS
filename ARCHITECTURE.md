# ACOS — Architecture

**Status:** clean-sheet design; implementation has just begun. This document
describes the intended subsystem map, the *actual* current crate layout, and the
data flow from APK to evidence. It is not a claim that any of it works.
See `DECISIONS.md` for the rationale and `STATUS.md` for the dated truth.

---

## 1. Conceptual architecture

```text
Agent / IDE / CI / MCP clients
        |
   ACOS typed control plane  (acos CLI, library, JSON, MCP)
        |
   Task planner + capability resolver + fidelity policy
        |
   Job dispatcher & evidence recorder
        |
   Shared ACOS core (one per compatible runtime/API/ABI variant)
   |-- Rust DEX loader/verifier + Android-compatible managed VM
   |-- Rust GC, class loader, interpreter, JIT/AOT cache, native JNI ABI
   |-- immutable Android API libraries/resources and framework metadata
   |-- capability registry, lazy services, IPC broker
   |-- optional offscreen compositor & input/event router
   |-- app/session orchestrator and memory/budget manager
        |
    app/task workers (many)
    |-- shared-task worker (same process/app data, explicitly allowed)
    |-- separate app process, optional display
    |-- isolated filesystem/credentials/process/net namespaces
    |-- separate core/VM fallback when required
        |
    structured UI/events/frames/crashes/profiling/evidence
        |
  task verdict + hashes + reproducible traces + resource report
```

This is a proposed implementation. Android framework behaviour and native
libraries are major engineering efforts and are not implied by the diagram.

---

## 2. Subsystem map and status

Status legend: **planned** (no code), **in progress** (active scaffolding/code),
**implemented** (works and is tested). At this date **no subsystem is
implemented**; the three listed as in progress are being scaffolded by other
agents concurrently.

| Subsystem | Responsibility | Status | Evidence required to claim "implemented" |
|---|---|---|---|
| APK ingest (`acos-apk`) | APK/split-APK manifest, permissions, signatures, ABI, assets, resources, DEX location | **in progress** | parse a legal corpus and classify it with no silent omission |
| DEX verifier (`acos-dex`) | DEX 035–039 formats, methods, register layouts, type/prototype resolution, control-flow and bytecode validation | **in progress** | agreement with the Android verifier on valid/invalid fixtures |
| Managed VM (`acos-vm`) | DEX register machine; dispatch; methods, classes, fields, arrays, exceptions, monitors, threads, class init | planned | real compiled DEX tests + differential Android results |
| Managed heap / GC (`acos-gc`) | object layout, roots, weak/global refs, STW/concurrent strategies, finalizers as required | planned | stress + JNI cross-boundary correctness |
| JIT/AOT (`acos-jit`) | DEX IR, profile-guided compilation, code-cache lifecycle, W^X, shared read-only caches | planned | measured improvement beyond interpreter on real workloads |
| JNI bridge (`acos-jni`) | Rust implementation of `JavaVM`/`JNIEnv` ABI; local/global/weak refs, calls, arrays, strings, thread attach, exceptions, `RegisterNatives` | planned | real NDK `.so` tests against reference |
| Native loader (`acos-native-loader`) | ELF loading/linking, relocations, symbol resolution, Bionic behaviours, JNI `dlopen`, ABI translation | planned | real ELF JNI tests incl. ARM-only via a supported translator |
| Android API layer (`acos-framework`) | Java/Kotlin framework semantics, resources, system services, intents, lifecycle, package/permission behaviour | planned | differential tests against reference |
| IPC / broker | service discovery, transport, caller identity, limits, lifecycle | planned | correct calls and isolation across sessions |
| Renderer (`acos-render`) | no-display or offscreen surfaces, layouts, text, images, touch targets, frame lifecycle | planned | pixel/semantic parity and direct capture |
| Input / control (`acos-input`) | key/touch/gesture/IME/accessibility semantics, focus per surface | planned | ≥2 concurrent interactive apps with no leakage |
| Isolation (`acos-sandbox`) | cgroup v2, user/mount/pid/net namespaces, UID/data policy, seccomp, limits | planned | red-team tests; distinct state and signatures |
| Observability (`acos-observe`) | typed tree with text/roles/ids, crash/ANR, frame times, heaps, JIT stats | planned | deterministic evidence, no stale screenshots |
| Scheduler (`acos-engine` + `acos-protocol`) | shared-core reuse, capabilities, per-task limits, pooling, pause/resume/reset | planned | N-scaling and correct budget enforcement |
| CLI (`acos-cli`) | typed JSON contract; one object per command | **in progress** | `acos inspect`/`verify` end-to-end on a real APK |

Per ADR-0003 the workspace starts with the **smallest cohesive crate set**
(`acos-dex`, `acos-apk`, `acos-vm`, `acos-cli`) and splits further only when
interfaces stabilise. The full list above is the *target* map, not an initial
scaffold; do not create placeholder crates merely to fill the diagram.

---

## 3. Current crate layout vs. planned

**Actual, as of this writing** (`ls crates/`, read-only check):

```text
~/Projects/acos/
  Cargo.toml                  # workspace (resolver 2, edition 2021, rust 1.90)
  crates/
    acos-dex/                 # scaffolded: Cargo.toml only (in progress)
  docs/
    handoff/                  # product specification (00..06 + kickoff)
    reference/ACVM_INVENTORY.md
  fixtures/                   # empty
  research/                   # empty
  results/                    # empty
  tools/                      # empty
```

`acos-apk` and `acos-cli` are named as in-progress first-crate targets but were
not yet present on disk when this document was written; they are owned by
concurrent agents and this doc will be corrected as they land.

**Planned initial workspace** (ADR-0003):

```text
crates/
  acos-cli/        # binary `acos`, typed JSON CLI
  acos-apk/        # ZIP/APK, AndroidManifest.xml (AXML), resources.arsc, signature/ABI
  acos-dex/        # DEX parse + verify + decode (035–039)
  acos-vm/         # interpreter + heap + object model (added when dex/apk interfaces stabilise)
```

**Planned later splits** (only when justified by stable interfaces): `acos-gc`,
`acos-jit`, `acos-jni`, `acos-native-loader`, `acos-framework`,
`acos-capabilities`, `acos-lifecycle`, `acos-render`, `acos-observe`,
`acos-input`, `acos-sandbox`, `acos-bench`, `acos-oracle`.

Directory intent: `fixtures/` (licensable Java/Kotlin/NDK sample apps),
`scenarios/` (task definitions), `docs/` (ADRs + ledgers), `results/`
(reproducible artefacts), `research/` (bounded spikes, including negative
findings).

---

## 4. Data flow: APK → DEX → VM → evidence

```text
 APK / split APK / .apk + .apks
   │
   ├─ acos-apk: ZIP central directory, AndroidManifest.xml (binary AXML),
   │            resources.arsc + resource table, permissions, min/target SDK,
   │            signature blocks, native ABI inventory, asset inventory,
   │            classes*.dex discovery (multi-DEX order), split-APK merge plan
   │
   ├─ acos-dex: DEX magic/version (035–039), header/checksum/signature,
   │            string/type/proto/field/method/class tables, class_data,
   │            code_items, instruction decode, register/type verification,
   │            exception handlers, debug info, try/catch, annotations
   │
   ├─ acos-vm (planned): class loader hierarchy + method/field resolution,
   │            `java.*`/`javax.*`/`android.*` intrinsics, object model,
   │            interpreter dispatch, exceptions, monitors, threads,
   │            class initialization ordering, JIT/AOT later
   │
   ├─ acos-jni + acos-native-loader (planned): Rust `JNIEnv`/`JavaVM` tables,
   │            ELF/Bionic symbol resolution for guest `.so`, ABI translation
   │
   ├─ acos-framework + capabilities + lifecycle (planned):
   │            Activity/Task, intents, resources, storage, permissions,
   │            system-service semantics implemented or explicitly refused
   │
   ├─ acos-render + acos-input (planned): offscreen surface on demand,
   │            layout/text/images, frame lifecycle, input routing, IME
   │
   └─ acos-observe + engine (planned): typed UI tree (text/id/role/bounds),
                request-sequenced fresh frames, crash/ANR, metrics
                    │
                    ▼
     Result envelope: job/session/request IDs, status, backend, fidelity,
     isolation, capabilities_used, frame{sequence,fresh_after_request,sha256},
     evidence paths, metrics{wall,peak_rss,cpu}
                    │
                    ▼
     Verdict + hashes + reproducible traces + resource report
```

Invariants across the flow:

- **Real inputs only.** The VM executes compiled DEX from real Java/Kotlin; the
  native path executes real ELF `.so` through the Rust JNI ABI.
- **Verify before execute.** `acos-dex` must reject invalid DEX with structured
  errors before any instruction runs.
- **Freshness is end-to-end.** A rendered frame is linked to the request/action
  that caused it; a stale frame is impossible.
- **Fidelity is attached to the result, not assumed.**
- **Every stage can refuse.** Missing API, ABI, symbol, resource or isolation
  guarantee produces a structured `unsupported`/`escalated` result.

---

## 5. Memory and capability model (design)

- Track, separately: shared fixed runtime; per-process private managed heap;
  native heap; graphics buffers; app data/cache; kernel/page tables; code cache;
  isolate overhead.
- Share immutable executable/resource pages and precompiled code across workers
  via file-backed/CoW mechanisms **when safe**.
- Never share mutable statics, permissions, credentials, JNI local state,
  database handles, or app singleton state between independent sandboxes.
- Evaluate process-template strategies only after reliable independent
  instances and safe JNI/GC interactions exist.
- A parked state may compress/evict cold pages only if resume correctly
  restores timers, native threads and app lifecycle semantics.
- Measure both `anon+shmem` and cgroup `memory.current`, PSS/USS where
  permitted, host `MemAvailable`, GPU allocations and zram footprint. Never
  compare mixed metric types.

Capability modules are declared with explicit providers, dependencies, startup
policy and unload conditions; an off switch must not silently change behaviour.
Many Android APIs rely on process-global/service-global state, so modularity is
only real when the dependency graph, startup order, per-task state, cleanup and
failure semantics are implemented.

---

## 6. Security posture (design)

- Untrusted APKs run only behind an audited boundary (planned `acos-sandbox` /
  `vm`), never with host-root-equivalent privilege.
- All isolation boundaries are explicit and independently reviewed.
- Signing/trust checks precede execution at every level, including build scripts
  (they execute code).
- No silent API faking: unimplemented behaviour is refused or escalated.
