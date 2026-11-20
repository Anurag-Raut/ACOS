# 2 — Clean-sheet architecture and Rust implementation

## Intentional clean-sheet boundary

ACOS is **not** a fork of `/home/Qui/Work/android-container`, not merely redroid stripped further, and not a new version of the old Docker orchestration layer. Its design and implementation live solely in `~/Projects/acos`, with a new Rust workspace and new agent. The existing ACVM is the **compatibility oracle / benchmark reference / research archive**, not the runtime architecture. AOSP is a **specification and behavior reference**, not an assumed mandatory guest system. Where practical, use the official public Android compatibility, DEX, JNI, SDK and CDD specifications.

**Rust-first, including runtime internals.** Use existing Rust components if suitable, but the final ACOS native executor has a Rust-developed Android-compatible managed runtime, not C++ ART concealed behind a Rust CLI. Guest app libraries compiled in C/C++ are inputs and may be executed through a safe, audited ABI boundary.

## Architecture (conceptual)

```text
Agent / IDE / CI / Codex / MCP clients
        |
   ACOS typed control plane  (Rust CLI, library, JSON, MCP)
        |
   Task planner + capability resolver + fidelity policy
        |
   Job dispatcher & evidence recorder
        |
   Shared ACOS core (one per compatible runtime/API/ABI variant)
   |-- Rust DEX loader/verifier + Android-compatible managed VM
   |-- Rust GC, class loader, interpreter, JIT/AOT cache, native JNI ABI
   |-- immutable Android API libraries/resources, framework metadata
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

The diagram is a proposed implementation, not a claim that arbitrary Android third-party apps can run immediately in a standalone process. Android framework behaviors and native libraries will be major engineering efforts.

## Core subsystems (Rust-owned)

| Subsystem | Responsibility | Acceptance proof |
|---|---|---|
| APK ingest | APK/split-APK manifest, permissions, signatures, ABI, assets, resources, DEX | parse and classify legal corpus, no silent omission |
| DEX verifier | formats, methods, register layouts, type/prototype resolution, control-flow and bytecode validation | agreement with Android verifier on valid/invalid fixtures |
| Managed VM | DEX register machine; dispatch; methods, classes, fields, arrays, exceptions, monitors, threads, class init | real compiled DEX tests, differential Android results |
| Managed heap / GC | object layout, roots, weak/global refs, concurrent or stop-the-world strategies, finalizers as required | stress and JNI cross-boundary correctness |
| JIT/AOT | DEX IR, profile-guided compilation, code-cache lifecycle, W^X, shared read-only caches | measured improvement beyond interpreter on real workloads |
| JNI bridge | **Rust implementation of `JavaVM` and `JNIEnv` ABI**, local/global refs, calls, arrays, strings, thread attach, exceptions, native registration | real Android NDK `.so` tests against reference |
| Native loader | ELF loading/linking, relocations, symbol resolution, Bionic behaviors, JNI `dlopen`, ABI translation | real ELF JNI tests including ARM-only via supported translator |
| Android API layer | Java/Kotlin framework semantics, resources, system services, intents, app lifecycle, package and permission behavior | differential tests against existing ACVM/AOSP |
| IPC/broker | service discovery, message transport, caller identity, limits, lifecycle | correct calls and isolation across sessions |
| Renderer | no-display or offscreen surfaces, layouts, text, images, touch targets, frame lifecycle | pixel/semantic parity and direct capture |
| Input/control | key, touch, gesture, IME, accessibility semantics, focus per surface | 2+ concurrent interactive apps, no leakage |
| Isolation | cgroup v2, Linux user/mount/pid/network namespaces, UID/data policy, seccomp, limits | red-team tests, distinct state and signatures |
| Observability | typed tree including text/roles/ids, crash/ANR, frame times, heaps, JIT stats | deterministic evidence and no stale screenshots |
| Scheduler | shared-core reuse, capabilities, per-task limits, pooling, pause/resume/reset | N-scaling and correct budget enforcement |

### Rust managed runtime: scope cannot be hand-waved

Implementing an ART-compatible runtime requires more than parsing DEX or creating an interpreter loop. Include:
- Multi-DEX and ZIP/APK loading; DEX file version matrix; classpath and class-loader hierarchies, method/field resolution and verification.
- All standard DEX instruction families required by the selected API level: moves, constants, arithmetic, branches, switch, invokes, instance/static fields, arrays, checks/casts, new-instance, synchronization/monitors, exceptions.
- Java core library semantics needed by the target APK: `java.*`, `javax.*`, Android framework classes; Kotlin coroutines and reflection behaviors where used.
- Object model, method dispatch, garbage collection, roots, finalization/references, Java memory model, thread APIs, stack traces, class initialization ordering and thread-local state.
- JNI call-in/call-out using the **standard function table ABI** implemented in Rust; native libs should not know or care what language ACOS uses. Correct local/global/weak references, arrays, strings, exception propagation, `RegisterNatives`, JNI version negotiations, attached native threads.
- App native libraries must load through a compatible ELF/linker/Bionic layer. Native graphics, TLS, filesystem, pthreads and unwinding are substantial compatibility dependencies.
- Fast JIT/AOT is an **optimization layer after the interpreter is correct**. Rust `Cranelift` is worth evaluating as a code-generation backend, not a ready-made DEX VM. Record interpreter vs compiled code differences and security of generated executable memory.
- Standard host tools like `d8`/`aapt2` can remain external *build tools*; the runtime that executes the APK remains Rust-driven.
- Differential tests compare VM results, exceptions, native calls, rendering and lifecycle to stock AOSP/reference device behavior. The official DEX opcode specification and JNI ABI are critical references.

**Legal engineering choices:** An externally maintained C/C++ native library can be accepted as a guest library or audited host dependency. It must not become a hidden central C++ Android runtime. If a dependency is GPL/copyleft, perform licensing review before integrating or copying its source. Clean-room behavior reimplementation using public docs/tests is an option.

### Why not Rust-only everywhere?

Rust is a strong language choice for the kernel-facing engine, but the objective is compatibility and performance. Linux kernel/drivers, OS C ABI, LLVM/Cranelift internals, graphics drivers and guest NDK libraries need not be rewritten. The new managed runtime and JNI machinery should be owned by Rust because that is the product requirement. **Do not claim Rust alone lowers app heap memory or makes every native library efficient.** Only benchmarks establish that.

## Runtime capabilities and explicit dependencies

Possible module contract (example):

```toml
[capability.webview]
provider = "native-webview"
requires = ["graphics.offscreen", "network", "storage.app", "jni", "process.spawn"]
startup = "lazy"
unload = "when-no-live-users-and-no-global-state"

[capability.camera.virtual]
provider = "synthetic-camera"
requires = ["sensor-clock", "media-pipeline"]
startup = "on-demand"
```

Potential modules:
- `core`: DEX VM, class loading, JNI, basic Java/Android APIs, resources, application lifecycle.
- `ui`: offscreen rendering, graphics, text shaping, input, font/density/theme; allow no-surface operation.
- `web`: WebView + JavaScript + networking (may be a substantial process tree).
- `storage`: app data, SQLite, prefs, files, content providers.
- `network`: DNS/TLS/socket APIs, proxies, recording and deterministic response mocking.
- `permissions`: Android permission state/queries/prompts where faithfully implemented.
- `events`: broadcast, alarm, notification, scheduling and background restrictions.
- `media`: virtual camera, mic, playback, codec services and synthetic sensor streams.
- `hardware`: location, motion, telephony, Bluetooth, NFC only when semantically necessary.
- `gms-compat`: opt-in, licensed/compatible implementations only; not falsely represented as Google certified.
- `native`: native loader, Bionic symbol compatibility and ABI translation where needed.
- `agent`: UI tree, screenshot, events, actions, reset, assertion and traces.

**Engineering note:** A capability registry only becomes genuinely modular if its dependency graph, startup order, per-task state, cleanup and failure semantics are implemented. Many Android APIs implicitly rely on process-global / service-global state. An off switch must not silently change behavior.

## Execution identities

Every task declares isolation explicitly:

1. `shared-task`: another activity/task in a shared app process/data state; cheapest, **not independent**.
2. `process`: separate process, potentially same package/storage identity; process-only separation, *not independent data* unless configured.
3. `sandbox`: separate OS process/UID/credentials/mount namespace and app data, with verified service identity mapping and JNI/native calls.
4. `core`: fully separate engine instance for ABI/API conflicts and greater process/service state independence.
5. `vm`: sandboxed full Android/backend for compatible untrusted execution or device-level fidelity, as available.

Use `shared-task` only when no cross-task independent state is claimed. Do not call a Linux sandbox an Android-user equivalent until permission, storage, Binder, provider, and process semantics are demonstrated.

## Rendering must be optional and correctly evidenced

- UI-less worker: no display or surface allocation, no compositor loop.
- Visual worker: create an offscreen surface on demand at task-specified density/size; no desktop window by default.
- Capture worker: return latest **fresh frame** with sequence ID, production timestamp, display ID, action causal ID, and stable content hash; avoid fake fresh frames.
- Text UI tree: native instrumentation should include readable text/content descriptions/IDs/roles/bounds/visibility without starting Android UiAutomation 0.5 seconds each call.
- Graphics: choose Rust-owned interface and evaluated renderers (`wgpu`/`tiny-skia` etc.) with parity verification; native OpenGL/Vulkan compatibility may require more complete engine integration.
- Apps using SurfaceView, WebView, Flutter, Compose, codecs or hardware acceleration require their real rendering behaviors, not black images labeled successful.

## Memory model

- Track shared fixed runtime + per-process private managed heap + native heap + graphics buffers + app data/cache + kernel/page tables + code cache + overhead for isolates.
- Share immutable executable/resources pages and precompiled code across workers with file-backed or CoW mechanisms when safe.
- Do not share mutable statics, permissions, credentials, JNI local state, database handles or application singleton state between independent sandboxes.
- Evaluate process-template strategies experimentally only after reliable independent app instances and safe JNI/GC interactions.
- A parked state policy may compress/evict cold pages but must correctly resume, including timers, native threads and app lifecycle semantics.
- Measure both `anon+shmem` and cgroup `memory.current`, PSS/USS where permitted, host MemAvailable, GPU allocations and zram footprint. No unsupported comparison with browser-tab PSS.

## Proposed Rust workspace (initial suggestion, revise with evidence)

```text
~/Projects/acos/
  Cargo.toml                  # workspace
  crates/
    acos-cli/                 # CLI contract
    acos-protocol/            # typed JSON, MCP schema
    acos-engine/              # orchestration, scheduling, profiles
    acos-apk/                 # APK, split, resources and manifest
    acos-dex/                 # parse + verify DEX
    acos-vm/                  # interpreter, class/object model, threading
    acos-gc/                  # managed heap
    acos-jit/                 # optional backend for DEX IR
    acos-jni/                 # JavaVM/JNIEnv ABI implemented in Rust
    acos-native-loader/       # ELF/Bionic compatibility
    acos-framework/           # Java/Android API semantics
    acos-capabilities/        # dependency registry and modules
    acos-lifecycle/           # app/activity/task behaviors
    acos-render/              # headless graphics
    acos-observe/             # UI semantics, traces, frames
    acos-input/               # injection and routing
    acos-sandbox/             # process + filesystem + security
    acos-bench/               # common benchmarks and golden diff
    acos-oracle/              # read-only adapter to existing ACVM
  fixtures/                   # licensable Java/Kotlin/NDK sample apps
  scenarios/                  # end-to-end task definitions
  docs/                       # ADRs and compatibility matrices
  results/                    # reproducible benchmark artifacts
  research/                   # bounded spikes, negative findings
```

Do not generate dozens of placeholder crates merely to fill a diagram. Begin with the smallest cohesive workspace, and split crates when there are stable interfaces.

## Agent API requirements

One typed contract across CLI, JSON, MCP and SDKs. Commands are **proposed future API**, not implemented today:

```text
acos inspect app.apk --json
acos run --apk app.apk --activity com.example/.MainActivity --isolation sandbox --display offscreen:393x851
acos observe <session> --tree --text --json
acos capture <session> --fresh --format png
acos action <session> tap --x 140 --y 320
acos action <session> type --text 'hello'
acos assert <session> --text 'Welcome'
acos events <session> --since-seq 10
acos reset <session> --scope app
acos profile <session> --memory --cpu --frames
acos destroy <session>
acos verify --apk app.apk --scenario scenarios/login.yaml --json --artifacts out/
acos capability report app.apk --json
```

Stable request/response IDs, frame sequence IDs, session IDs, error types and capability negotiation are mandatory. Return errors like `UNSUPPORTED_ANDROID_API`, `NEEDS_REAL_DEVICE`, `REQUIRES_GMS`, `WRONG_ABI`, `UNSUPPORTED_NATIVE_SYMBOL`, `NO_FRESH_FRAME`, `UNVERIFIED_FIDELITY`, rather than passing tests or hanging.
