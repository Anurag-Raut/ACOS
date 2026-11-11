# ART runtime implementation plan

Status: proposed implementation, 2026-11-11. Analysis only; no runtime changes made.

## Summary for the owner

| Decision / delivery | Recommendation |
|---|---|
| Execution | Route A: image `dalvikvm64`, bionic linker and API-34 boot classpath inside bwrap; independent processes first. |
| Framework JNI | **Hybrid:** a small replacement JNI registration library calling existing Rust engines through a C ABI; selectively reuse API-34 C++ Binder/Parcel/Looper glue. Do not load stock `libandroid_runtime.so` for the first-frame path. |
| Binder | Source-build a private libbinder variant with its driver-facing transport replaced by a Unix socket; preserve Java Binder/Parcel and native object semantics. No kernel Binder. |
| Graphics | Intercept at graphics JNI before HWUI creates native objects; record into `acos-render`, rasterise in the ART worker, present through Rust WMS. No SurfaceFlinger or GPU prerequisite. |
| First deliverable | Real counter Activity, real resources/layout/draw, then coordinate tap `0 → 1 → 2`, fresh frame and live View tree. |
| Cost | M-A + M-B first frame: **31–46 agent-days**; through M-C tap/tree: **40–58**; through M-D V1/benchmarks: **54–77**. Four concurrent workstreams maximum; approximately 17–25 working days overall, dependency/build delays excluded. |
| Immediate blocker | Remove E1A/E1C's suppressed fatal CHECK before counting any milestone as passed. |
| Stop rule | No Android boot, system_server, kernel Binder, SurfaceFlinger or route-B loader rescue; bounded milestone budgets below. Keep the Rust VM fallback intact. |

## Evidence and boundaries

Read: [decision](DECISION_ART_RUNTIME.md), [V1](V1.md),
[E1A](../results/art-feasibility/E1A.md), [E1B](../results/art-feasibility/E1B.md),
[E1C](../results/art-feasibility/E1C.md), and [E3-M9](../results/art-feasibility/E3-M9-peak-pss.md).

E1A proves 27/27 console programs, not Activity execution: independent ART processes
cost ~119 MB PSS at N=1, ~21 MB/process at N=20, ~16 MB marginal. E1C proves custom
JNI and a bytes-only parcel/socket exchange, not BinderProxy/AIDL lifecycle IPC.
Its raw-fork children lack restored ART runtime threads; 0.7–2.6 MB marginal is
an optimistic probe, not an app budget. E3 measures isolated Rust-VM sessions at
~527–565 MiB PSS at N=20; the ~39 MiB in-process M2 number is not comparable.

The inspected reusable engines are real: `acos-framework-natives` implements
resources, `acos-render` has `RenderContext`, `RenderNode` and `DisplayOp`,
`acos-wm` has `SessionWm`, compositor/input/vsync, and `acos-services` has
`ServiceHost::transact` plus `take_deliveries`. Their VM JNI adapters currently
live in `acos-vm/src/natives`; those take `Vm`/`NativeCtx`, not ART's `JNIEnv`.
They are references, not linkable ART JNI functions. Do not modify those adapters
or migrate/delete VM crates while building this path.

Use the image's exact build fingerprints, ELF build IDs, jar hashes and APEX
versions as the compatibility key. API level alone is insufficient. The API-34
[AndroidRuntime registration source](https://raw.githubusercontent.com/aosp-mirror/platform_frameworks_base/android-14.0.0_r75/core/jni/AndroidRuntime.cpp)
shows why loading framework.jar is insufficient: Android registers Binder,
Parcel, resources, input and many other native groups separately. Treat that
tag as a reference; verify the source revision against our image before reuse.

## 1. Framework natives: choose hybrid registration

| Option | Effort / fidelity assessment |
|---|---|
| Stock image `libandroid_runtime.so` | Highest fidelity for resources/Looper/input, but brings a large native dependency closure, native pointer contracts and stock graphics/server assumptions. Redirecting its real HWUI objects into Rust becomes a C++ ABI replacement project. A quick load is not a short route to a Rust-rendered frame. |
| Entirely new Rust framework-native implementation | Avoids native C++ compatibility but reimplements Binder/Parcel identity/refcounts/JNI glue and hundreds of framework entry points. Broadest correctness burden; reject. |
| **Hybrid, recommended** | Own the reachable JNI tables; use real ART/core natives, reuse the Rust resource/render engines, and source-adapt the Binder/Parcel/Looper glue whose native object semantics matter. Less scope than either full replacement; fidelity comes from real Java and explicit native contracts. |

Build one guest `x86_64-linux-android` cdylib, `libacos_framework_jni.so`.
An ACOS bootstrap DEX loads it before any Android class requiring registration
is initialised. Its registration entry point installs exact class/name/signature
tables from the pinned framework. It is a **functional replacement of the
needed framework JNI**, not an ELF-ABI drop-in named `libandroid_runtime.so`.
Use `dalvikvm64` precisely so stock app_process does not force that ABI on us.

Native ownership:

- Retain the image's ART, libjavacore/openjdk/ICU/conscrypt and bionic implementations.
- Resource JNI (`AssetManager`, `ApkAssets`, `StringBlock`, `XmlBlock`) calls
  `acos-framework-natives::res`; preserve handles, TypedArray's seven-int layout,
  real `framework-res.apk`, themes, configuration and app resources.
- Graphics JNI calls `acos-render`; all Paint/Typeface/Bitmap/Canvas/RenderNode
  objects in this connected graph belong to ACOS. Never pass a Rust handle to
  a stock Skia/HWUI native or mix resource pointers from two implementations.
- Adapt API-34 Parcel/Binder JNI together against our private libbinder;
  reuse the MessageQueue/Looper glue with bionic/libutils where its closure is
  self-contained. Any AndroidRuntime utility calls must be explicitly ported.
- Implement log/properties/clock/DisplayEventReceiver and surface/input glue
  needed by the measured fixture path. An unavailable API produces a named
  unsupported result, not a generic success/zero return.

C ABI uses opaque generation-checked handles, primitive values, UTF-16 buffers,
explicit lengths, status codes and release functions. Rust owns its allocations;
no Rust Vec/String layout or exceptions cross C. Use ART JNI local/global refs,
exception checks and attach/detach rules; never use the Rust VM's JNI table.
Audit `@FastNative`/`@CriticalNative` calling conventions separately, including
critical methods without JNIEnv arguments and actual NativeAllocationRegistry
finaliser function pointers. GC must reclaim real handles without double frees.
Use E1C's export/retention recipe (`-u`, version script); verify `.dynsym`.

Registration is a bounded list driven by fixture entry traces and first missing
native reports, not a replacement of every Android native. M-A must exercise
class initialisation, reflection, resources, Looper and Binder JNI under GC.
M-D adds hello-jni using ART's own native loader/JNI; keep Rust-loader evidence
separate rather than relabelling it as an ART result.

## 2. Binder: interpose below BpBinder, above the kernel command protocol

```text
Java AIDL Proxy → BinderProxy.transactNative → adapted Binder JNI
  → native Parcel / BpBinder → private IPCThreadState transport
  → session Unix socket → Rust object translator → ServiceHost::transact
  ← reply bytes + object table + transferred fds
ServiceHost::take_deliveries → socket → local BBinder/JavaBBinder
  → Java Stub.onTransact → IApplicationThread / IWindow → main Handler
```

**Implement a source patch to libbinder's ProcessState/IPCThreadState transport.**
Keep Parcel, BBinder/BpBinder and reference machinery where possible. Replace
driver discovery/open/mmap/ioctl, context-object lookup, transact/wait, incoming
dispatch and death/reference operations coherently. Handle zero is the shim's
service manager; it resolves names through `ServiceHost::check_service`.
No `/dev/binder` device or real servicemanager process is required.

Do not LD_PRELOAD a single mangled `transact` symbol: internal symbol binding,
driver setup and callbacks bypass such a hook. Do not emulate the complete
BC_*/BR_* kernel protocol via ioctl interception. Confirm the private library
and selected glue build against the pinned image early; neither the NDK alone
nor stock library interposition guarantees a compatible platform build.

Use a versioned framed socket protocol containing request/session/generation IDs,
direction, target, code, flags, calling identity, parcel bytes, object offsets
and descriptor attachments. A session gets its own host worker/ServiceHost and
connection; process-global service/display caches cannot leak between sessions.
The socket is a justified process boundary: E1C measured ~4 microseconds/call,
not a reason to put ServiceHost inside the app process.

Mandatory translation, absent from E1C:

1. Export local Java Binder objects as session object IDs with retained native
   ownership; map remote handles bijectively. Rewrite flat objects both ways,
   including backreferences to the original local object. Never send raw pointers.
2. Transfer fds with SCM_RIGHTS; validate object offsets, type, length, count,
   attachment indices and ownership. Receiver gets a real duplicated fd.
3. Import/export an entire Rust Parcel envelope. Current `Parcel::marshall`
   returns bytes only and `unmarshall` clears object/fd tables; add explicit
   envelope import/export APIs rather than feeding that path silently.
4. Preserve ordered oneway delivery, sync errors, nested transactions, identity
   save/restore, handle release and death-on-worker-exit. Serve reverse requests
   while waiting for replies. Never hold the ServiceHost lock across a callback.
5. Route reverse Binder dispatch to a small attached native worker; let the real
   Stub post lifecycle/window work to the main Looper. Do not run UI callbacks
   directly on the socket thread. Unknown interface/code stays
   `UNSUPPORTED_ANDROID_API` with descriptor/code/method evidence.

Seed ServiceHost from the real APK manifest, decode AIDL tables from exactly
these jars, then implement attachApplication → bindApplication →
ClientTransaction launch/start/resume. Existing service state machines and
PendingDelivery encoding are reusable but full real-framework parcelables and
out parameters must be checked. WMS add/relayout/insets/window callbacks are
part of M-B, not postponed until input. A fake Java service object that bypasses
BinderProxy does not pass the Binder gate.

## 3. Graphics, input and observation

Keep Java ViewRootImpl measure/layout/traversal and ThreadedRenderer orchestration.
Intercept **before** native HWUI display-list creation:

```text
Choreographer → ViewRootImpl traversal → View.draw / RenderNode recording
  → RecordingCanvas/Canvas/Paint/Typeface JNI → acos-render DisplayOp graph
  → HardwareRenderer sync/draw JNI adapter → RenderContext rasterisation
  → RGBA memfd + completed-frame notification → Rust WMS compositor/FrameStore
```

The API-34 [RenderNode JNI source](https://raw.githubusercontent.com/aosp-mirror/platform_frameworks_base/android-14.0.0_r75/libs/hwui/jni/android_graphics_RenderNode.cpp)
uses native RenderNode pointers and native finalisers. Its opaque display lists
are not a stable serialisation format that `acos-render` can consume. Replace
the JNI object graph; do not decode stock HWUI lists or copy raw pointers.

Implement recording create/finish, node properties, save/restore/clip/transforms,
text shaping/metrics, paths/roundrects, bitmap/nine-patch and button background
operations reached by counter. Keep strict render errors: an unimplemented
visible operation blocks the verdict. Match image fonts, density, font padding,
status/navigation insets and theme; do not hardcode counter's measured bounds.

Provide Rust-owned Surface/SurfaceControl/SurfaceSession/BLAST handles and the
needed transaction, resize, buffer and draw-completion semantics. These are
substitutes for native SurfaceFlinger clients, not no-op success stubs. Stub
only nonsemantic tracing/profiling hints with a recorded capability declaration;
reject GPU-only effects/features outside scope. No EGL, gralloc, RenderThread
or SurfaceFlinger service is necessary for software presentation. ThreadedRenderer
completion callbacks and sync flags must reflect actual accepted/presented work.

Use real monotonic time and requested-only timerfd/epoll vsync notifications.
DisplayEventReceiver schedules the real Choreographer traversal; no virtual-time
claim. Render in the guest with Rust `acos-render`; host Rust WMS owns window
geometry, composition, freshness and evidence. Bound buffers and use an ownership
ack before reuse. Publish frame_seq/cause_id/hash only after pixels are complete.

For tap, Rust WMS selects the visible window and sends DOWN/UP through its input
channel. Adapt InputChannel/InputEventReceiver/MotionEvent JNI to that channel,
then call the real Java receiver so ViewRootImpl dispatches the event normally.
`acos-wm::InputMessage` is an ACOS framing with cause_id, not established as the
image's libinput ABI; use our matching receiver, not an unchanged stock consumer.
Wait for input ack, traversal and a completed causally linked frame; never call
`performClick` as the acceptance path.

A small platform bootstrap helper walks the real DecorView/ViewGroup tree on
the main thread after traversal: class, resource entry ID, text/contentDescription,
screen bounds, visibility/enabled/clickable state. Transform/clip consistently,
version observations against the last completed traversal and publish through
the existing UiTree protocol. Never reconstruct the tree from layout XML or
the renderer's draw list. M-D adds IME typing/back and genuine recreation.

## 4. Substrate and worker lifecycle

- Productise E1A extraction into a read-only versioned sysroot; mount APEXes by
  manifest module name, sharedlibs at its expected path, generate linkerconfig
  once per image, sanitise inherited LD_*/Java environment, and give each worker
  private `/data`, cache, app-native-library paths, logs and control sockets.
  Build/cache a checksum-keyed boot image outside app execution if it helps.
- Fix mainline framework metadata/property inputs until ART starts with fatal
  abort behavior restored. Keep structured liblog collection, but fatal logging
  must terminate. A liblog sink or correctly framed `/dev/socket/logdw` receiver
  supplies logs; a real logd daemon is unnecessary. Retain stderr, wait status,
  Java stack and native abort context without requiring tombstoned/debuggerd.
- Seed read-only properties from the image plus explicit per-session display/
  runtime overrides. Supply a bionic-compatible property area and the needed
  `__system_property_*` read/callback/wait/update behavior; a Rust map behind
  Java SystemProperties alone misses native readers. A privileged init/property
  daemon is unnecessary; reject undeclared writes and notify supported updates.
- Start `dalvikvm64 -Xint` with the full device BOOTCLASSPATH and an appended
  trusted bootstrap DEX. Register JNI, set up runtime exception handling and
  classloader namespace, then enter real ActivityThread/main Looper and the
  Binder-driven launch. Keep APK bytecode unchanged. Resolve hidden-API access
  explicitly for the trusted helper; never replace Android Java classes.
  Stock app_process is not the initial launcher: it runs AndroidRuntime's broad
  registration. A later custom app_process-style launcher remains route A.
- V1 excludes JIT: use `-Xint` for acceptance and comparisons; optional cached
  AOT boot images must be labelled. E1A's 13–53x compute improvements used JIT
  and are not promised for this configuration. No snapshots/deterministic time.
- Apply bwrap namespace setup before final seccomp/Landlock restrictions; ART
  needs mmap/signals/futex/threads and substantial reserved VA, which is not a
  resident-memory budget. Bound resident usage/wall time and kill/reap the entire
  worker process group. Fail closed if required user namespaces are unavailable.
- **Warm-fork is optional after M-C.** First use independent processes and shared
  read-only boot pages. A raw JNI `fork()` plus a Java-only quiescence handshake
  is insufficient. A pool must use the image's ART-aware Zygote pre/post-fork
  hooks and supported fork machinery, with fresh service/input fds, identity,
  RNG state and child thread/GC restoration; no system_server startup. Prove GC,
  threads, exceptions, native callbacks and teardown under repeated forks.
  If those hooks require forbidden Android infrastructure, retain cold workers;
  this kills the pool optimisation, not ART. Budget a separate 4–6 agent-days.

Map uncaught app Java exceptions/native app signals to `APP_CRASHED` with stacks/
signal/build IDs; assertion mismatch to fail; timeouts to an explicit timeout;
unknown JNI to `UNSUPPORTED_JAVA_API` with class/method/signature; native symbol
resolution failures to `UNSUPPORTED_NATIVE_SYMBOL`; service gaps to
`UNSUPPORTED_ANDROID_API`; unsupported APK ABIs to `UNSUPPORTED_ABI`.
Infrastructure CHECKs and corrupt transport are runtime errors, not app failures.
Proposed new codes must be added to the protocol before use. Record unsupported
hits independently of Java exception catches so a swallowed shim exception cannot
produce a false pass. Never silently fall back to the Rust VM on an ART run.

## 5. Execution ownership and gates

The following are **proposed files and commands**, not existing runnable APIs.
No implementation or acceptance run is authorised by this document alone.
Start after plan approval; at most these four workstreams run concurrently.
Each owner changes only its listed files; request owner integration for shared
changes. All leave existing VM/JNI source and unrelated working changes intact.

| Owner | Exclusive file ownership / responsibility |
|---|---|
| S1 — substrate/session process | New `crates/acos-art/{Cargo.toml,src/lib.rs,src/process.rs,src/substrate.rs}`; workspace Cargo.toml/Cargo.lock integration; `research/art-runtime/substrate/**`; packaging/build/launch scripts under `tools/art-runtime/` except S4's acceptance/benchmark scripts. |
| S2 — Binder/services | `research/art-runtime/binder/**` including pinned libbinder patches and wire specification; new `crates/acos-art/src/{wire,services}.rs`; guest `crates/acos-art-jni/src/binder/**`; `acos-parcel/src/parcel.rs` envelope APIs; required `acos-services/src/**` service/parcelable changes. |
| S3 — resource/render JNI | New `crates/acos-art-jni/{Cargo.toml,build.rs,src/lib.rs,src/abi.h,src/resources.rs,src/graphics/**}` and registrar integration; `acos-render/src/**`; `acos-wm/src/compositor.rs`. Owns JNI ABI headers and native registration assembly; S2/S4 supply their registration functions. Link existing `acos-framework-natives` unchanged initially. |
| S4 — launch/input/product/verify | `research/art-runtime/java/**`; new guest `crates/acos-art-jni/src/{looper,input,display,system}.rs`; host `crates/acos-art/src/control.rs`; `acos-wm/src/{input,vsync,transport,facade}.rs`; `acos-runtime/src/art.rs` and minimal lib/session routing; `acos-cli/src/**`, `acos-verify/src/**`, ART scenarios/results; `tools/art-runtime/{accept,bench}.sh`. |

First integration contract: S1 owns process/fd startup, S2 owns socket envelope,
S3 owns guest handles/C ABI and completed RGBA buffers, S4 owns command/event
mapping and Java main-thread scheduling. Agree those shapes before adapters,
then implement vertical slices; no competing registrars or session frameworks.

Each milestone command also accepts `--stream S1|S2|S3|S4` for the owner's
bounded acceptance evidence: S1 startup/fatal/containment/cleanup, S2 real
Binder object/fd/callback/ordering, S3 resources/GC/recording/pixels, and S4
lifecycle/input/tree/product verdict. For example,
`tools/art-runtime/accept.sh M-A --stream S2` runs the Binder gate; the command
without `--stream` requires all four streams and the integrated end-to-end gate.
S4 owns that script, with probe programs supplied by their respective owners.

| Gate | S1 days | S2 days | S3 days | S4 days | Acceptance / dependency |
|---|---:|---:|---:|---:|---|
| **M-A: framework classes run** | 3–5 | 4–6 | 3–4 | 2–3 | `tools/art-runtime/accept.sh M-A`; 27/27 dexprogs, fatal-check-free startup, JNI resource+Looper probes, real Parcel/BinderProxy → ServiceHost → reverse Java Binder callback, fd and object identity round trips, GC/finaliser probe. |
| **M-B: counter first frame** | 2–3 | 5–7 | 8–12 | 4–6 | After M-A, `tools/art-runtime/accept.sh M-B`; real APK probe=4242, Binder attach/bind/launch/start/resume, expected logs, live count=0, nonblack completed PNG and tree/layout reference comparison at 1080×2400@420. No static scene or direct lifecycle fixture bootstrap. |
| **M-C: tap + UI tree** | 1 | 2–3 | 2–3 | 4–5 | After M-B, `tools/art-runtime/accept.sh M-C`; launch/capture, coordinate taps inc twice, reset button, tree `0→1→2→0`, logged handlers, ack/cause-linked fresh frames; two sessions cannot change each other's state. |
| **M-D: V1 parity + M1–M11** | 2 | 2–3 | 4–6 | 6–8 | After M-C, `tools/art-runtime/accept.sh M-D` and `tools/art-runtime/bench.sh --rounds 20`; fixture/JNI/recreate/reset/negative/concurrency gates and complete side-by-side benchmark artifacts. |

M-A can develop four independent probes; M-B's critical chain is resource JNI
→ Activity launch → WMS relayout → render completion. S2/S3 develop against saved
requests and synthetic native probes while S4 integrates real framework calls.
M-C input/tree and M-D parity/bench tooling can develop in parallel, but later
milestones cannot be reported passed before earlier gates pass. Estimates cover
fixture/V1 reachability, not arbitrary Android apps; full platform builds may
increase elapsed time. Critical boundary regressions warrant tests; no coverage padding.

S4 adds an explicit `--engine art` selector (or equivalent single backend field)
to the existing product session and verify path. Acceptance must include:

```sh
acos verify --engine art --apk fixtures/build/apps/counter.apk \
  --scenario scenarios/v1-counter.yaml --json --artifacts results/art-runtime/M-C
acos verify --engine art --apk fixtures/build/apps/hello-views.apk \
  --scenario scenarios/v1-hello-views.yaml --json --artifacts results/art-runtime/M-D/hello
acos verify --engine art --apk fixtures/build/apps/hello-jni.apk \
  --scenario scenarios/hello-jni.yaml --native --json --artifacts results/art-runtime/M-D/jni
```

These commands require implementing the selector; existing `native-preferred`
scenario routing must not override it. Add strict ART-only routing, metadata
`backend=art`, image/native-adapter hashes and explicit Rust-renderer fidelity.
V1's Rust VM/loader/JNIEnv mechanism assertions need an owner-approved ART
equivalence checklist; keep their original results, distinguish execution engine
from renderer fidelity, and never claim those exact mechanisms ran on ART.

M-D also runs reference-bound scenarios, both rotation directions/save-restore,
typing/back, reset/destroy/no residual worker, all negative variants (zero false
passes), all console programs, GC-pressure behavior and N=1/2/5/10/20 isolation.
Translate VM-specific GC-stress requirements to a documented ART stress method;
an unimplemented stress method remains explicitly unmet. Exact texts/classes/IDs/
bounds must match existing references; report pixel differences with the existing
declared tolerance, never widen it to pass the new engine.

Benchmark M1 idle core, M2 N=5 incremental memory, M3 cold first correct frame,
M4 worker creation, M5 fresh screenshot, M6 reset, M7 UI tree, M8 action/state,
M9 concurrency, M10 full verify and M11 verified tasks/GiB-minute against both
the Rust VM and D2 legacy. Interleave ≥20 repetitions, p50/p95, identical display/
fixture/reference/host limits and explicit cold/warm labels. Sum PSS over every
host/guest/pool process once; report private dirty, peak sampling cadence, shared
baseline and marginal slope separately. Compute M11 from verified completions and
time-integrated total attributable PSS, not the old in-process M2 denominator.
List every worse-than-baseline row with a next action; no score for false passes.

## 6. Top risks and recommended stop budgets

| Risk | Evidence / mitigation | Kill criterion |
|---|---|---|
| Platform-native build/ABI and class initialisation expand scope | E1A never registered the app framework. Pin image/source, prove the narrow Binder/Parcel/Looper build and resource class initialisation first. | Stop ART implementation if M-A cannot pass within **18 cumulative agent-days**, or needs broad AndroidRuntime/HWUI ABI emulation rather than bounded JNI adapters. |
| Kernel Binder assumptions / callback deadlock | E1C tested primitive bytes only. Gate local-object roundtrip, fds, death, nested sync/oneway and lifecycle callbacks before Activity work. | Stop if correctness requires kernel Binder or real system_server; no ioctl-emulation rescue project. |
| Display-list/text/surface incompatibility | Existing Rust engine is reusable; VM wrappers are not. Keep all graphics handles under one owner and compare real layout early. | Stop if M-B misses a real correct first frame by **46 cumulative agent-days**, requires SurfaceFlinger/GPU boot, or needs Java framework/fixture behavior replaced to pass. |
| Input/observation passes without execution fidelity | Require ViewRootImpl input, live tree, causal frame and deliberate wrong/crashing fixtures. | Stop if M-C cannot pass by **58 cumulative agent-days**, or any false pass persists at M-D. |
| Fork corruption or no economic advantage | Raw-fork probe omitted GC/runtime thread restoration; cold ART already shares file pages. | Disable pool after 6 agent-days without GC/thread safety. Stop recommending ART if, after M-D plus at most 8 tuning agent-days, matched isolated runs show neither ≥2× memory improvement nor higher verified tasks/GiB-minute than Rust VM. |
| Broken diagnostics / containment | E1A aborter suppression and inherited host library paths are known defects. | No milestone passes with a suppressed fatal CHECK, silent fallback, leaked host paths or an unconfined worker. If these cannot be fixed within M-A budget, stop. |

Treat budgets as recommended funding caps, not measured implementation facts.
M-D cap is **77 agent-days** excluding optional pool/tuning. At a kill point,
retain evidence and the last unsupported/native/service hit, return the product
default to the existing Rust VM, and request a new architecture decision before
more ART work. Do not delete either engine or silently switch to route B.
