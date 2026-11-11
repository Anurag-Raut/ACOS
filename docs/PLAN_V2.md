# ACOS plan v2 (2026-11-11): evaluated against docs/ENGINEERING_POLICY.md

Philosophy change: ACOS is no longer "Android in Rust". It is the cheapest correct way to run real Android apps for many agents at once. Reuse Google's code (ART, bionic, framework, and wherever it works libandroid_runtime/libhwui/libbinder); own only the overhead layers (sessions, services-on-demand, binder transport, window/frame/verify tooling).
Metric: verified tasks per GiB-minute with N=10 concurrent agents (M12: total attributable PSS, N=10). Secondary: M1-M11 vs legacy ACVM.

## 1. Evaluation of every current track

| Track | Gate result | Decision |
|---|---|---|
| Rust DEX VM (acos-vm, IR tier, snapshots) | ART is 10-100x faster (JIT) and ~16-21 MB/proc vs ~525 MiB. Fails gate on speed and memory | FREEZE. Keep in tree as fallback + deterministic reference. No new feature work. |
| VM follow-ups: M9 fixes (fork template, boot DEX mmap), 1px title, list_scroll regression, --concurrency, VM speed | Only matter if VM stays primary | DROP (reopen only if ART kill criteria fire) |
| JNI fidelity in VM (W188 fbjni refs, W197 GetEnv, W199 arm64 launch) | ART provides real JNI | CANCEL (partial edits in tree: discard or keep unstaged, do not commit) |
| acos-framework-natives (resources), acos-render, acos-wm, acos-services, acos-parcel | Reusable engines; keep | KEEP. W200 render fixes (focus ripple, action-bar offset) stay valuable: renderer is shared by both engines |
| Native loader, sandbox (Landlock/seccomp), arm64/qemu transport | Loader route B rejected (34-63 d). Sandbox + arm64 still useful: ART in bwrap, arm64 apps via qemu later | KEEP sandbox; PARK loader route; arm64 later |
| Verify/scenario tooling, UI tree protocol, parity (ART oracle), benchmarks | Engine-independent | KEEP; add `--engine art` |
| Flutter, RN, Gaten | Cheaper on ART (real JNI, native libs just run) | KEEP DEFERRED until counter.apk runs on ART |
| ART plan S1..S4 as written (54-77 agent-days, replacement framework JNI + patched libbinder + custom graphics intercept) | Not reuse-first. Replaces libandroid_runtime/libhwui/libbinder wholesale before proving stock pieces cannot work | REVISE: reuse-first ladder below |

## 2. Reuse-first ladder (replaces the advisor's S2/S3 assumptions; advisor's S1/S4, gates and kill criteria stay)

For each layer try in order; stop at the first rung that works on counter.apk:
- Framework natives: (1) stock libandroid_runtime.so under dalvikvm (experiment D answers) -> (2) stock + small Rust/C shims for the failing pieces -> (3) hybrid replacement JNI (advisor plan).
- Binder: (1) stock libbinder + userspace /dev/binder emulation (bounded BC_*/BR_* command set behind an interposed open/ioctl/mmap, backed by Rust ServiceHost over a socket; D evaluates) -> (2) patched libbinder socket transport (advisor plan).
- Graphics: (1) stock libhwui with software/CPU Skia pipeline to a buffer (experiment E answers) -> (2) intercept at RenderNode/Canvas JNI into acos-render (advisor plan). Pick by measured fidelity and per-process memory; acos-render stays the oracle-tuned fallback.
- Services: no system_server. Rust ServiceHost answers only the transactions a test hits; unknown -> UNSUPPORTED_*.
- Process model: independent ART processes first; ART-aware warm-fork pool only after M-C and only if it clears the gate (>=25% of N=10 PSS).

## 3. Phases, todos, gates (agent-days are caps; kill criteria from ART_RUNTIME_PLAN.md apply)

Phase 0 - probes (running now, ~1 day): D (libandroid_runtime/Binder on ART), E (graphics seam), F (launcher crate, properties, logd, CHECK fix), W200 (render).
  T0.1 read E2D/E2E/F reports; fix the rung choice for natives, binder, graphics. Owner: orchestrator.
  T0.2 re-baseline: memory of a bare ART process with framework classes loaded (M-A style) -> start the ART-path M1/M2/M12 numbers. Owner: Haiku.
Phase 1 - M-A framework classes run (cap 18 d): T1.1 S1 substrate productised (fatal CHECK gone, props, logs, crash map); T1.2 natives rung implementation; T1.3 binder rung implementation (Parcel/BinderProxy round trip to ServiceHost, fds, callbacks); T1.4 `tools/art-runtime/accept.sh M-A`.
Phase 2 - M-B counter first frame (cap 46 d cumulative): T2.1 resources via chosen rung; T2.2 ActivityThread attach/launch via ServiceHost (reuse acos-services state machines); T2.3 WMS relayout/surface; T2.4 graphics rung -> frame in FrameStore; T2.5 `acos verify --engine art` selector, backend=art metadata.
Phase 3 - M-C tap + UI tree (cap 58 d): T3.1 input channel -> real ViewRootImpl; T3.2 live View-tree walker helper; T3.3 two-session isolation test.
Phase 4 - profile + gated optimisation (only after M-C): T4.1 where does PSS go (baseline vs per-session slope); T4.2 candidate list judged by the gate: warm-fork pool, shared boot image, JIT vs -Xint tuning, on-demand services, fast reset; T4.3 M12 benchmark vs Rust VM and legacy.
Phase 5 - V1 parity on ART (M-D, cap 77 d): hello-views, hello-jni (real JNI, free), recreate, reset, negatives, concurrency N=1..20, M1-M12 table, V1 gate rerun with `--engine art`.
Phase 6 - breadth: RN hello, Gaten (arm64 via qemu or x86 build), Flutter (deferred), dogfood waves, parity vs phone.

## 4. Kill / fallback rules
- M-A not passing by 18 agent-days, M-B by 46, M-C by 58, or no >=2x memory or throughput win after M-D + 8 tuning days -> stop ART, return default to Rust VM, request a new decision.
- Never silently fall back to the Rust VM on an ART run. Never claim determinism/snapshots on ART.
- Any new component needs the three-question gate recorded in its report.

## 5. Agent policy for this plan
Max 5 DeepSeek (pi/opencode-go, thinking high) concurrently; Haiku for gates/profiling/doc work; advisor codex gpt-6.1-sol analysis-only. Commit verified work by pathspec, push to origin/main.
