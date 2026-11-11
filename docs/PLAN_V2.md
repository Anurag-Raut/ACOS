# ACOS plan v2 (2026-11-11): evaluated against docs/ENGINEERING_POLICY.md

Philosophy: ACOS is an agent-first Android APK execution and verification environment, not an OS. It is the cheapest correct way to run real Android apps for many agents at once. Reuse Google's code (ART, bionic, framework, and wherever it works libandroid_runtime/libhwui/libbinder); own only the overhead layers (sessions, services-on-demand, binder transport, window/frame/verify tooling). Owner direction: `docs/OWNER_DIRECTION_2026-11-11.md`.

Metric: correctly verified tasks per GiB-minute with N=10 concurrent agents (M12: total attributable PSS, N=10). Secondary: M1-M11 vs legacy ACVM.

## 1. Evaluation of every current track

| Track | Gate result | Decision |
|---|---|---|
| Rust DEX VM (acos-vm, IR tier, snapshots) | ART is 10-100x faster (JIT) and ~16-21 MB/proc vs ~525 MiB. Fails gate on speed and memory | FREEZE. Keep in tree as fallback + deterministic reference. No new feature work. |
| VM follow-ups: M9 fixes (fork template, boot DEX mmap), 1px title, list_scroll regression, --concurrency, VM speed | Only matter if VM stays primary | DROP (reopen only if ART kill criteria fire) |
| JNI fidelity in VM (W188 fbjni refs, W197 GetEnv, W199 arm64 launch) | ART provides real JNI | CANCEL (partial edits in tree: discard or keep unstaged, do not commit) |
| acos-framework-natives (resources), acos-render, acos-wm, acos-services, acos-parcel | Reusable engines; keep | KEEP. W200 render fixes (focus ripple, action-bar offset) stay valuable: renderer is shared by both engines |
| Native loader, sandbox (Landlock/seccomp), arm64/qemu transport | Loader route B rejected on effort (its estimate is withdrawn: no agent-day estimates). Sandbox + arm64 still useful: ART in bwrap, arm64 apps via qemu later | KEEP sandbox; PARK loader route; arm64 later |
| Verify/scenario tooling, UI tree protocol, parity (ART oracle), benchmarks | Engine-independent | KEEP; add `--engine art` |
| Flutter, RN, Gaten | Cheaper on ART (real JNI, native libs just run) | KEEP DEFERRED until counter.apk runs on ART |
| ART plan S1..S4 as written (replacement framework JNI + patched libbinder + custom graphics intercept) | Not reuse-first. Replaces libandroid_runtime/libhwui/libbinder wholesale before proving stock pieces cannot work | REVISE: reuse-first ladder below |

## 1b. Evidence since this plan was written (experiments D, E, 2026-11-11)
- D: stock libandroid_runtime + libbinder run Parcel, Looper, Resources on ART (needs property table, registration order, LD_PRELOAD into default namespace). Binder driver emulation via one ioctl (BINDER_WRITE_READ) is the clean hook; fake /dev/binder in bwrap is not viable.
- E: stock libhwui draws via exported register_android_graphics_classes; seam = Skia CPU canvas. acos-vm gfx natives remain the pixel-diff oracle.
- Consequence: natives rung 1 (stock) and graphics rung 1 (stock software) selected; binder rung 1 (stock libbinder + driver emulation) under test (G). The effort estimate for M-A+M-B is no longer recorded in days; progress is tracked by the component table in `STATUS.md`. Caps unchanged.
- Product scope: verification environment, final compatibility on real emulator/device (docs/ENGINEERING_POLICY.md).

## 2. Reuse-first ladder (replaces the advisor's S2/S3 assumptions; advisor's S1/S4, gates and kill criteria stay)

For each layer try in order; stop at the first rung that works on counter.apk:
- Framework natives: (1) stock libandroid_runtime.so under dalvikvm (experiment D answers) -> (2) stock + small Rust/C shims for the failing pieces -> (3) hybrid replacement JNI (advisor plan).
- Binder: (1) stock libbinder + userspace /dev/binder emulation (bounded BC_*/BR_* command set behind an interposed open/ioctl/mmap, backed by Rust ServiceHost over a socket; D evaluates) -> (2) patched libbinder socket transport (advisor plan).
- Graphics: (1) stock libhwui with software/CPU Skia pipeline to a buffer (experiment E answers) -> (2) intercept at RenderNode/Canvas JNI into acos-render (advisor plan). Pick by measured fidelity and per-process memory; acos-render stays the oracle-tuned fallback.
- Services: no system_server. Rust ServiceHost answers only the transactions a test hits; unknown -> UNSUPPORTED_*.
- Process model: independent ART processes first; ART-aware warm-fork pool only after M-C and only if it clears the gate (>=25% of N=10 PSS).

## 3. Phases, todos, gates (no time budgets; one-hour rule applies)

Phase 0 - probes: D (libandroid_runtime/Binder on ART), E (graphics seam), F (launcher crate, properties, logd, CHECK fix), W200 (render), G (binder driver), H (ART memory baseline).
  T0.1 read E2D/E2E/F reports; fix the rung choice for natives, binder, graphics. Owner: orchestrator.
  T0.2 re-baseline: memory of a bare ART process with framework classes loaded (M-A style) -> start the ART-path M1/M2/M12 numbers. Owner: Haiku.
Phase 1 - M-A framework classes run : T1.1 S1 substrate productised (fatal CHECK gone, props, logs, crash map); T1.2 natives rung implementation; T1.3 binder rung implementation (Parcel/BinderProxy round trip to ServiceHost, fds, callbacks); T1.4 `tools/art-runtime/accept.sh M-A`.
Phase 2 - M-B counter first frame (P0): T2.1 resources via chosen rung; T2.2 ActivityThread attach/launch via ServiceHost (reuse acos-services state machines), through `Application.onCreate` and `Activity.onCreate`/`onResume`; T2.3 WMS relayout/surface and a live `ViewRootImpl` traversal; T2.4 vsync source for `Choreographer`; T2.5 graphics rung -> live frame in FrameStore; T2.6 `acos verify --engine art` selector, backend=art metadata.
Phase 3 - M-C tap + UI tree (P0 interactive loop, P1 sessions): T3.1 input channel -> real ViewRootImpl; T3.2 live View-tree walker helper; T3.3 two-session isolation test; T3.4 reset, close and relaunch on ART.
Phase 4 - profile + gated optimisation (only after M-C; headline metric: verified tasks per GiB-minute at N=10): T4.1 where does PSS go (baseline vs per-session slope); T4.2 candidate list judged by the gate: warm-fork pool, shared boot image, JIT vs -Xint tuning, on-demand services, fast reset; T4.3 M12 benchmark vs Rust VM and legacy (N=1/5/10/20).
Phase 5 - V1 parity on ART (M-D): hello-views, hello-jni (real JNI), recreate, reset, negatives, concurrency N=1..20, M1-M12 table, V1 gate rerun with `--engine art`.
Phase 6 - breadth (P2, after measurements): RN hello, Gaten (arm64 via qemu or x86 build), Flutter (deferred), Views/Compose, dogfood waves, parity vs phone.

Host protection track (P0, runs alongside phases 1-3; no untrusted APK before it passes):
  T-H1 per-run data root: `/data` is a per-run tree, with shared inputs mounted read-only inside it (design in `results/art-runtime/S3.md` §5).
  T-H2 adversarial test: a guest cannot overwrite `boot.art` or the classpath file (`results/art-runtime/S2.md` item 5).
  T-H3 mandatory memory limit: a cgroup created by acos-art, not only the RSS watchdog (`results/art-runtime/S3.md` §3).
  T-H4 decision on seccomp and Landlock for the ART route (`results/art-runtime/S3.md` §3). Namespaces do not cover syscall filtering.
  T-H5 keep the orphan test green: SIGKILL of the launcher leaves no guest or bwrap (`results/art-runtime/S3.md` §2.1).

## 4. Fallback rules (no time-based kill criteria)
- ART is not abandoned on a missed deadline or failed integration step. On a blocker, follow the six-step procedure in `docs/ENGINEERING_POLICY.md` (name the component, inspect Google's implementation, try a minimal shim, compare with a hybrid, consider the legacy Android-container backend, present the alternatives with evidence before changing the primary architecture).
- Never silently fall back between engines; Rust VM stays as reference and explicit fallback; no determinism/snapshot claims on ART.
- One-hour rule: no task runs more than 60 minutes without an owner decision (see ENGINEERING_POLICY.md). Phases are split into small tasks; each task carries a start time and is reported at 60 min.
- Each new component records the three-question gate in its report.

## 5. Agent policy for this plan
Max 5 DeepSeek (pi/opencode-go, thinking high) concurrently; Haiku for gates/profiling/doc work; advisor codex gpt-6.1-sol analysis-only. One worktree per independent workstream, with declared file ownership; preserve unknown edits. Commit verified work by pathspec, push to origin/main.

## 6. Priority order (owner, 2026-11-11)

Source: `docs/OWNER_DIRECTION_2026-11-11.md` §7. The phases above carry these priorities.

- **P0, real APK running** (phases 1-2): provider binder; `Application` and `Activity` `onCreate`/`onResume`; real window lifecycle; `ViewRootImpl` + surface + `Choreographer`; a live frame.
- **P0, interactive loop** (phase 3): tap and key input; counter 0 -> 1; fresh screenshot; live tree; reset, close and relaunch; `acos verify --engine art`.
- **P0, host protection** (host protection track). No untrusted APK before the safety tests pass.
- **P1, independent sessions** (phase 3, T3.3; phase 4).
- **P1, real benchmarks** ART vs Rust VM vs legacy at N=1/5/10/20 (phase 4, T4.3).
- **P2, efficiency** (on-demand services, graphics allocations, shared memory, warm-fork, AOT/JIT) and **compatibility** (Views/Compose, RN, Gaten, Flutter): only after measurements (phases 4 and 6).
- Postpone speculative optimisation.

Decisive test (owner): `counter.apk` -> real Activity on ART -> UI -> view tree -> tap Increment -> 0 becomes 1 -> fresh screenshot -> verify -> reset -> rerun. Real ART, no Rust-VM fallback, no offline-render shortcut. Current status is recorded in `STATUS.md`.
