# ACOS — Status

## Current direction (ART)

Direction set 2026-11-11 in `docs/OWNER_DIRECTION_2026-11-11.md` (the owner's
consolidated direction, which wins on conflict), `docs/DECISION_ART_RUNTIME.md`,
`docs/PLAN_V2.md` and `docs/ENGINEERING_POLICY.md`. ACOS is an agent-first
environment for running real Android APKs and verifying app changes, not a
replacement OS. The agent-day estimates and kill criteria in
`docs/ART_RUNTIME_PLAN.md` are void (its header says so). The Rust VM track is
frozen; its old status is kept below under "Rust VM track (frozen)".

### Engine and rules

- **Engine:** real Google ART from the API-34 image (`dalvikvm64`, `com.android.art`
  APEX, device boot classpath) inside a bwrap user+mount namespace. No init,
  zygote, system_server, servicemanager or kernel `/dev/binder`
  (`results/art-feasibility/E1A.md`, `results/art-runtime/P2a.md`). Product
  sysroot: `tools/art-sysroot.sh`; crate `crates/acos-art`.
- **Reuse ladder:** stock `libandroid_runtime`, stock `libbinder` with a
  userspace `/dev/binder` emulation (`LD_PRELOAD`), stock `libhwui` on the
  CPU Skia path, stock `AssetManager`/idmap2 overlays. Rust owns the services
  (`crates/acos-services`, `crates/acos-binder-peer`), window/frame and
  verify tooling (`docs/PLAN_V2.md` §2, `docs/ENGINEERING_POLICY.md`).
- **Frozen:** the Rust VM stays in the tree as the fallback and deterministic
  reference, with no new features (`docs/PLAN_V2.md` §1).
- **No silent fallback** between engines. No snapshot or virtual-time
  determinism claims on the ART path (`docs/DECISION_ART_RUNTIME.md`).
- **One-hour rule:** a task gets at most 60 min of autonomous work. At 60 min
  the agent checkpoints safely (never a kill), reports done / blocking / why
  it is hard / the smallest next action, and asks for approval before more
  effort. No time-based abandonment of ART (`docs/ENGINEERING_POLICY.md`,
  `docs/OWNER_DIRECTION_2026-11-11.md` §6).
- **Priority order:** P0 real APK running, P0 interactive loop, P0 host
  protection; then P1 sessions and benchmarks; P2 efficiency and compatibility
  after measurements (`docs/PLAN_V2.md` §6).

### Component status (ART, 2026-11-11)

Sources: `results/art-runtime/{P4b,S3,Q1,Q2b,D1,R1,R2,S2,Q3}.md` and the git
log at `3eabff1`. Only facts in those files are used. Commit state comes from
each file's own text: P4b, S3, Q2b, D1, R2, Q3 and S2 say nothing is committed,
R1's fix is in `3678c34`, and Q1 does not say. Most ART work is uncommitted.

Status values (`docs/OWNER_DIRECTION_2026-11-11.md` §12):

- **DONE:** exercised on the real ART path by a real APK run with a result
  file, and nothing in its required scope is missing. Library loading or an
  isolated test is not DONE.
- **PARTIAL:** exercised on the real ART path by a real APK run with a result
  file, and part of its required scope is missing.
- **BLOCKED:** a real APK run reaches it and fails at a named cause recorded in
  `results/`.
- **NOT STARTED:** no real APK run on the ART path touches it. Isolated tests
  and library loads are listed as evidence only.

| Component | Google/Rust/Hybrid | Status | Working now | Missing | Priority |
|---|---|---|---|---|---|
| ART runtime (`dalvikvm64`) | Google | PARTIAL | Console corpus: 27 pass, 0 fail, 1 skipped (`28_sqlite_basic`, dex not built), 0 fatal logs, unchanged after sandbox hardening (`verify-27-f2.json`, S3 §2). The launch reaches `Application.onCreate` and `Activity.onCreate/onStart/onResume` (P4b §0, P4c). | Real window attach/draw not reached (`IWindowManager.openSession` unsupported, P4c). AOT vs JIT on the APK path is not measured in the listed files. | P0 |
| Framework classes (`ActivityThread`, `Instrumentation`, `Application`, `Activity`) | Google | PARTIAL | Real `ActivityThread`: `BIND_APPLICATION` (110) finishes on the main Looper. `Instrumentation` is created by the framework (`instrumentationName=null`). A `ClientTransaction` with `LaunchActivityItem` and `ResumeActivityItem` is built by framework code and delivered (code 54). `Activity.attach`, `PhoneWindow.<init>`, `Activity.onCreate`, `onStart` and `onResume` run (AcosCounter logs, commit `80db296`, P4c). | `ViewRootImpl` construction fails at `IWindowManager.openSession` (P4c; W1 in progress). | P0 |
| Settings content provider (`settings`) | Rust (`acos-services` `ProviderRegistry`) | PARTIAL | Rust route: `getContentProvider` is answered from the registry with a real `ContentProviderHolder`. `GET_config`, `LIST_config` and `query` return registry data. 7/7 `ams_java` tests pass (Q2b §2). | P4c fixed the hang (the binder peer closed the reply's fd before sending); `GET_config` through the ART peer now answers and the Activity resumes. `query` bulk-cursor path and other providers are not exercised on ART. | P0 |
| Bionic / linker (APK native libraries) | Google | PARTIAL | `hello-jni` natives return the V1 values, 12/12 (R2 §2), with an app-namespace `PathClassLoader`. The RN native closure (6 libraries, `BIND_NOW`) loads (R2 §4). | Only in the R2 harness, not through the `ActivityThread` launch path (R2 §4.3). | P2 |
| JNI | Google | PARTIAL | `hello-jni` natives run through `RegisterNatives` and `ThrowNew`. RN's merged `JNI_OnLoad` completes (R2 §2, §4). | `AndroidRuntime::mJavaVM` is null in the `dalvikvm64` guest, so a JNI upcall on an error path crashes at 0x0 (Q3 §1.1, §8 defect 4). App logging needs one framework registration, `register_android_util_Log`, through a shim (R2 §3.2). | P1 |
| libandroid_runtime (framework natives) | Google | PARTIAL | The stock library runs under `dalvikvm64`. Parcel, Looper and the other framework natives run on the launch path (P4b §0). `NativeDisplayEventReceiver` runs its native path (Q3 §1). | `AndroidRuntime::start()` never runs, so `mJavaVM` and `gCurRuntime` stay null (Q3 §1, §8). The display event receiver fails and then segfaults in its destructor (Q3 §1). | P0 |
| libbinder / Parcel | Google | PARTIAL | Stock `libbinder` talks to the Rust peer. `attachApplication`, `bindApplication` (1324 bytes, code 5) and `scheduleTransaction` (2108 bytes, code 54) reach the real `ApplicationThread` (P4b §0, §3). `servicemanager.ready=true` ends the libbinder wait loop, so the R1 log spin is gone (P4b §6). | `Parcel.marshall()` cannot carry binder objects, so the activity token and three system tokens are `null` (P4b §3, §7). The arg tables for `notifyDexLoad` (106) and `handleApplicationWtf` (101) are stale and return `SERVICE_BAD_REQUEST`, and their descriptor reads `<unknown>` (P4b §9). | P0 |
| Binder transport (ACOS adapter) | Hybrid (stock libbinder + Rust peer) | PARTIAL | Reverse delivery through the userspace driver: the same `F_TXN` path carries `bindApplication` and the `ClientTransaction` (P4b §3). No kernel binder is used (P4b §7). | Binder objects do not cross the transport; see the token stand-in above. No transport optimisation is measured, and none is claimed. | P0 |
| System services (`ServiceHost`, Rust) | Rust | PARTIAL | On the ART launch path, `attachApplication`, `getActivityClientController` (18) and `getDisplayId` (21) are answered (P4b §5 item 4). The `IDisplayManager.getDisplayInfo` codec matches the API-34 layout byte for byte: 260-byte reply, 6 tests (D1 §4). Unsupported transactions are recorded as structured records (P4b §9). | The D1 codec is not on the launch path; the guest still pushes its template (D1 §5). `getApplicationInfo` and `getInstrumentationInfoAsUser` are guest-marshalled templates (P4b §7). Stock vs lightweight is not yet evaluated per service in the listed files. | P0 |
| Window / surface (`IWindowSession`, `Surface`) | Hybrid (stock `PhoneWindow`/`DecorView`; the ACOS window service is to be decided) | PARTIAL | One-shot `render-app` frame through stock `PhoneWindow`/`DecorView`: SSIM 0.99351, 8/8 structural nodes, tree 12/12, md5 `9ec17f05…` (P4b §0, S3 §2). | No live window lifecycle. `addToDisplayAsUser`, `relayout` and a `Surface`/`SurfaceControl` are not reached (P4b §5). The rung is undecided: a null `SurfaceControl` first, or a shmem `ANativeWindow` (P4b §5.2). | P0 |
| ViewRootImpl | Google | NOT STARTED | Nothing on the live path. The frame comes from `render-app`, not from a `ViewRootImpl` traversal. | `setView`, `addToDisplay`, `relayout`, `performTraversals` and the input channel (P4b §5). | P0 |
| Choreographer and vsync | Google, with an ACOS vsync source | NOT STARTED | Isolated only, from the Q3 harness, not an APK run. The first unpressed draw of a ripple starts a `ValueAnimator`, which builds `Choreographer` and `DisplayEventReceiver`. Without SurfaceFlinger the process hangs (property absent) or segfaults (property served) (Q3 §2, §4). Chosen routes: a guest settle policy now, a vsync peer later (Q3 §5). | No vsync source on any path. The settle policy is not in the guest yet (Q3 §8). `Choreographer` is constructed before any traversal (P4b §5.3). | P0 |
| HWUI / Skia | Google | PARTIAL | Stock libhwui on the CPU Skia path draws the counter frame: SSIM 0.99351, 8/8 structural nodes, md5 stable (P4b §0, S3 §2, R1 §3.3). A pressed and focused `Button` frame draws in the harness with no crash, with the ripple downgraded to a content layer (Q3 §6). | The frame is one-shot, not from a live session. Settled animation frames (ripple, halo, shadow) need a clock (Q3 §6). `drawRenderNode` is unavailable on a software canvas (Q3 §4 row 6). | P0 |
| AssetManager / overlays | Google | PARTIAL | Static overlays through stock `ApkAssets`, with no hardcoding (commit `9c5947d`). The counter frame depends on them. | Not exercised through a live `Activity` launch. `hello-views` inflation is blocked upstream by the settings provider (P4b §4). | P0 |
| Sessions (independent processes) | ACOS (Rust) | PARTIAL | N=10 independent ART processes, each rendering `counter.apk`, verified 10/10 in all 3 reps. Peak PSS sum 1386.1 MiB. Median 326.4 verified tasks per GiB-minute (Q1 §1). | The Q1 run has no LLM agent in the loop (Q1 §3.1). The `session` route never reaches its ready line: it aborts in `StateListAnimator` → `Choreographer` (R1 §5). Every run writes the same `/data` tree (S3 §5). | P1 |
| Input and live interaction (tap, key, live tree, reset, relaunch) | ACOS (input) with the stock `ViewRootImpl` | NOT STARTED | No ART input result in the listed files. | Everything. P4b stops before `onResume`, so no input reaches a live view. | P0 |
| Observation and verification | ACOS | PARTIAL | `render-app` plus `tools/art-render-app-compare.py`: `ok` and exit 0 means SSIM at least 0.99 and 8/8 structural nodes (Q1 §1; S3 §2). Tree 12/12 (P4b §0). | Frame and tree from a single shot. No `acos verify --engine art` result in the listed files. No live tree. | P0 |
| Sandbox (Linux isolation + ACOS orchestration) | ACOS (bwrap, Rust launcher) | PARTIAL | Hardened argv: `--unshare-pid/net/ipc/uts/cgroup-try`, `--die-with-parent`, `--clearenv`, `--cap-drop ALL`. Corpus unchanged. Killing the launcher leaves no guest or bwrap (S3 §2, §2.1; S2). Memory: logd cap 50 000 entries, 8 MiB stream caps, 2 GiB RSS watchdog (R1 §2). | `/data` is writable by any guest, and `boot.art` can be overwritten (S2 item 5; S3 §5). No mandatory cgroup memory limit (S3 §3). No seccomp or Landlock on the ART route (S3 §3). | P0 |
| Rust DEX VM (`acos-vm`) | Rust | PARTIAL | Frozen as reference and fallback. `acos verify`: 10/10 at N=10 on the 13-step counter scenario, median 52.2 verified tasks per GiB-minute, about 578 MiB PSS per agent (Q1 §1). | No feature work (owner). Not comparable with the ART workload (Q1 §3.2). | frozen |
| Warm-fork / shared pages | ACOS on Google ART | NOT STARTED | Nothing measured in the listed files beyond per-agent PSS (Q1 §1). | Gated after M-C, and only if it clears the gate (`docs/PLAN_V2.md` §2). | P2 |
| Compatibility apps (RN, Flutter, Gaten) | Google (app code) | NOT STARTED | In the R2 harness, the RN native closure loads and `JNI_OnLoad` completes. RN's first blocker is `SoLoader.init()` (R2 §4.2). | No ART run of an RN, Flutter or Gaten app through the launch path. No Flutter evidence in the listed files. | P2 |

**Decisive test (owner §11, `counter.apk`): NOT PASSED (update after P4c, `80db296`: launch now reaches `Activity.onResume`; next blocker `IWindowManager.openSession`; no live frame, tap or `--engine art` yet).**

- Real `Activity` on ART: not reached. The launch stops inside `Activity.attach`, in the `PhoneWindow` constructor's settings read (P4b §4).
- UI and view tree: a one-shot tree exists (12/12), not from a live `Activity` (P4b §0).
- Tap Increment, 0 becomes 1: not reached. No ART input path is recorded.
- Fresh screenshot: not produced from a live session.
- Verify, reset, rerun: not reached.

**Smallest next actions, from the result files (in order):**

1. Serve the `settings` provider to the ART peer, or fail the call with a structured error. A hang is not allowed (P4b §4). Verify with `tools/guarded.sh --mem 4G --time 300 -- ./target/debug/acos-art launch --apk fixtures/build/apps/counter.apk --until activity-resumed` (P4b §10).
2. Serve `addToDisplayAsUser` and `relayout` with a null `SurfaceControl` (rung 0), so `performTraversals` gets past `relayout` (P4b §5.2).
3. Provide a vsync peer node for `createDisplayEventConnection`, or the guest settle policy, so no animator starts during draw (P4b §5.3; Q3 §5, §8).
4. Per-run `/data` root and a mandatory cgroup before any untrusted APK runs (S3 §3, §5).

Owner decisions pending: none is recorded in the listed files. The window and
surface rung (P4b §5.2) becomes an owner gate if it commits to a complex new
window or graphics implementation (`docs/OWNER_DIRECTION_2026-11-11.md` §10).

### Proven on ART

| Claim | Result | Evidence |
|---|---|---|
| Console corpus on real ART | **27/27** pass, normal run, 0 fatal ART log entries | `results/art-runtime/verify-27-f2.json`, `results/art-runtime/F2.md` |
| Fatal mainline CHECK removed | ART starts on the device-accurate `BOOTCLASSPATH`/`DEX2OATBOOTCLASSPATH`; the abort is no longer suppressed | `results/art-runtime/F2.md` |
| Stock framework natives on ART | `Bundle`, `Parcel`, `Looper`, `Resources`, `ServiceManager` run on ART, no Android boot | `results/art-feasibility/E2D.md` |
| Stock `libbinder` + userspace driver | `getService`, `transact`, fd passing and reverse calls to a Rust peer on real `ServiceHost` | `results/art-feasibility/E3G.md`, `results/art-runtime/P2d.md`, `results/art-runtime/P2f.md` (committed `9a8e70a`) |
| Binder reverse delivery | 9/9 checks, 5/5 consecutive runs, including 10 deliveries to a parcel-only binder | `results/art-runtime/P2f.md` |
| Stock `libhwui` draws on ART | Bitmap `Canvas` draws correct pixels, no GPU | `results/art-feasibility/E2E.md` |
| `counter.apk` first frame, software path | SSIM **0.99685** vs ART oracle, 4/4 content bounds exact, md5 stable over 5 runs, with image static RROs applied by stock path | `results/art-feasibility/E4H.md`, `results/art-runtime/P2c.md`, `results/art-runtime/P2e.md` |
| Real window chrome | `PhoneWindow`/`DecorView` built by stock `setContentView`; full frame SSIM **0.99290** (P3b), **0.99351** (P3c), tree 12/12, structural 8/8 | `results/art-runtime/P3b.md`, `results/art-runtime/P3c.md` |
| Fonts | `sSystemFontMap` built from `/system/etc/fonts.xml`; ActionBar title bounds exact (189 px) | `results/art-runtime/P3c.md` |
| Launch reaches `Application.onCreate` | `bindApplication` (1324 bytes, `instrumentationName=null`, framework-created `Instrumentation`) finishes on the main Looper; `Application.onCreate` runs; the `ClientTransaction` reaches `Activity.attach` | `results/art-runtime/P4b.md` (uncommitted) |
| Memory, measured | Cold path: 16–21 MiB private per session; AOT boot image: 64–66 MiB; shared file-backed part 100–113 MiB (cold) / 60–83 MiB (AOT), whole host | `results/art-runtime/BASELINE.md` |

Caveats: the `counter` frame comes from a one-shot `render-app` run, not a
live session. P3c's numbers are uncommitted at the time of writing.

### In progress (not verified)

- **Launch past `Application.onCreate`: resolved by P4b.** `Application.onCreate`
  runs with `instrumentationName=null`, and the framework creates
  `Instrumentation` (`results/art-runtime/P4b.md` §1–§2). The `ClientTransaction`
  reaches `Activity.attach`. `Activity.onCreate` is blocked by the `settings`
  provider (P4b §4). Code: `crates/acos-art/src/launch.rs`,
  `crates/acos-art/guest/java/dev/acos/art/LaunchMain.java` (uncommitted, P4b §8).
- **Content provider (Settings).** The Rust route is fixed: `Q2b` made
  `getContentProvider`, `call` and `query` registry-backed (7/7 `ams_java`
  tests). The ART path still blocks: `Settings.Global` →
  `IContentProvider.call` hangs the main thread, because the holder's binder has
  no server (`results/art-runtime/P4b.md` §4). `hello-views` still fails in
  `EditText` (`results/art-runtime/P2a.md` §4). Q2b has no ART evidence (Q2b §6).
- **Sessions.** Q1 ran N=10 independent `render-app` processes, 10/10 verified
  (`results/art-runtime/Q1-baselines.md`). The `session` route never reaches its
  ready line: it aborts in `StateListAnimator` → `Choreographer`
  (`results/art-runtime/R1.md` §5). Every run shares one writable `/data` tree
  (`results/art-runtime/S3.md` §5). `crates/acos-art/src/session.rs`,
  `tools/art-session-drive.py` and `tools/art-runtime/bench.sh` (ART engine row)
  are untracked. There is no ART session result file in `results/`.
- **`results/art-runtime/p3d/`** holds a frame, tree and `compare.json`
  (tree identical to `p3c`). There is no `P3d.md` report, so its status is unknown.
- **Working tree.** Most ART work is uncommitted (`git status`: `crates/acos-art`,
  `results/art-runtime/P3c.md`, `P4a.md`, `p3c/`, `p3d/`, `tools/art-runtime/`).

### Milestones (`docs/PLAN_V2.md` §3, `docs/ART_RUNTIME_PLAN.md` §5)

| Gate | State | Evidence |
|---|---|---|
| M-A framework classes run | **Not accepted.** Most pieces are proven (above); no runner exists: `tools/art-runtime/accept.sh` is absent (only `bench.sh`) | `docs/PLAN_V2.md` §3 |
| M-B `counter.apk` first frame | **Partial.** Frame via `render-app` (SSIM 0.99685 / 0.99351); `Application.onCreate` runs; the real Activity launch is blocked at `Activity.attach` by the `settings` provider | P2e, P3c, P4b |
| M-C tap + UI tree | **Not started on ART (unknown).** No ART tap or live-tree result file | — |
| M-D V1 parity + M1–M11 on ART | **Not started.** M1–M11 have not been measured on ART; `results/` has only memory numbers | `results/art-runtime/BASELINE.md` |

### Open gaps

1. `hello-views` cannot inflate `EditText` (Settings content provider, above).
2. `counter.apk` `Activity.onCreate` is not reached: the launch blocks at the `settings` provider (P4b §4). `Application.onCreate` runs (P4b §1).
3. Two framework arg tables are stale: `IPackageManager.notifyDexLoad` (code 106) and
   `IActivityManager.handleApplicationWtf` (code 101) return `SERVICE_BAD_REQUEST`.
   The peer also logs `descriptor "<unknown>"` (P4a §5).
4. No ART input path (tap, type, back) and no ART reset or recreate is recorded
   (unknown).
5. The `acos verify --engine art` selector is required by `docs/PLAN_V2.md`
   (phase 2, T2.5). Whether it exists is unknown; no ART verify result is recorded.
6. Fixed-core memory and N=10 measurements (M12, `docs/ENGINEERING_POLICY.md`)
   on the ART path exist only for the one-shot `render-app` workload: median
   326.4 verified tasks per GiB-minute at N=10, with no agent in the loop
   (`results/art-runtime/Q1-baselines.md`). The agent-loop benchmark is not measured.
7. Warm-fork pool: not started. Gated after M-C (`docs/ART_RUNTIME_PLAN.md` §4,
   `docs/PLAN_V2.md` §3 phase 4).
8. Flutter, React Native and Gaten: deferred until `counter.apk` runs on ART
   (`docs/PLAN_V2.md` §1).
9. Image ships no `/product/fonts` (`results/art-runtime/P3c.md`).

### Rust VM track: last V1 gate

- Last V1 gate: `results/v1/GATE-f0c9fdb.md`, HEAD `f0c9fdb`. **V1 not done.**
- Passing: hello-views 13/13, counter 13/13, recreate 17/17, reset 11/11,
  hello-jni 13/13 with `--native`, dexprogs 27/27 (normal and GC-stress),
  negative suite with zero false passes. M1–M5, M7, M8 and M10 pass; M11 passes
  by script rule on one task.
- Failing: 1.4 (hello-views title 1 px narrow), 1.9 (`--reference` path stale,
  `UNSUPPORTED_BACKEND`), 5.1 (no `--concurrency`; independence unproven), M6
  (1.5× legacy), M9. The launch RSS guard trips intermittently (2067 MiB vs 2048).
- Dropped follow-ups (`docs/PLAN_V2.md` §1): M9 fixes, 1 px title, list_scroll,
  `--concurrency`, VM speed, and JNI fidelity W188/W197/W199 (partial edits not
  committed).

## Rust VM track (frozen)

Recorded before 2026-11-11. Kept as written. Where it conflicts with "Current
direction (ART)" above, the section above wins. Everything from "## One line" to the end of this file describes the Rust VM track; it is historical, not the current ART status.

**As of 2026-11-11, HEAD `fd1a887`** (W165-A; `main` also carries W161-A through
W171-A). `git log` is authoritative, and `tools/status-check.sh` fails when this
line drifts more than 3 commits (`tools/verify-all.sh` now **fails** on it, not
warns). **`BENCHMARKS.md` §9 is the authoritative measured table**; **§9.8 (W176-A) is
the newest V1 run** (`results/bench/v1-w176a.json`) and reports **9 PASS /
2 FAIL** (M5, M9) with all 11 metrics measured. It was built from `fd1a887`
plus two then-untracked W172-A pieces (`private_root.rs` + one `lib.rs`
declaration) because `fd1a887` did not compile standalone; W172-A has since
committed them and `main` advanced to `af4ab94` (§9.8 records it). §9.1
(W160-A) and §9.7 (W165-A) are earlier tables/history and their M8/M10/M11
notes are stale: the tap now lands (`results/m2/M2_LOG.md` §W161-A,
`crates/acos-runtime/tests/counter_interaction.rs`) and W167-A added the post-tap
repaint invalidation. `TASKS.md` is the plan;
`docs/V1.md` is the V1 definition; `docs/FINDINGS.md` is the append-only
findings log; `EXPERIMENTS.md` is history; `docs/SECURITY.md` lists the
safety-relevant `ACOS_*` knobs and the private-root rule.

## One line

**`counter.apk` reaches `first_frame` through a real launched Activity
(`ThreadedRenderer.draw` → `FrameStore`), and a tap on it now lands: the app's
own `onClick` runs and the live tree shows `count=1` (W161-A,
`crates/acos-runtime/tests/counter_interaction.rs`, un-ignored). `hello-views`,
`form-validation` and `list-scroll` reach `first_frame` at W147-A; the W166-A
fixtures `dialogs-menus`, `list-adapter` and `fragments-nav` reach it too. The
default boot-cache **restore** path is fixed (W154-A carried native regions,
W160-A records their file source and re-reads it): a warm `acos launch`
reaches `first_frame` **5/5** at 0.806 s p50 with a **25.58 MB** cache entry. A
live `View` tree is observable (M7 49.8 ms p50). The pressed-state **ripple** is
a **declared flat-colour fallback** (W167-A implemented the AGSL/RippleShader
natives to draw the declared flat colour, not the agsl shader); it is a named
visual divergence, not a fake frame.** The
Rust VM runs the real Android 14 core-library bytecode, the real boot-classpath
DEX is extracted unprivileged, `acos inspect`/`run-dex`/`verify` exist, and the
Rust service layer (`acos-services`, `acos-wm`), the SQLite engine
(`acos-sqlite`), the confinement layer (`acos-sandbox`) and the renderer
(`acos-render`) have their own acceptance tests. The V1 benchmark at
`20663b3`+W160-A beats legacy on **5 of 11** metrics (**5 PASS / 6 not-pass** vs
owner directive D2; see `BENCHMARKS.md` §9); the frame, live UI tree and fresh
screenshot now land, while input, reset, independence and the V1 verdict remain
unverified.

## Milestone table (M0..M5 + V1 gate)

The named gates come from `research/service-layer/BACKLOG.md` §"Milestone
summary" (M1..M5); M0 is the P0/P1 static-analysis foundation in
`docs/handoff/04_ROADMAP_AND_ACCEPTANCE.md`.

| Gate | Requirement | State | Evidence |
|---|---|---|---|
| **M0** static APK/DEX analysis | parse/verify/decode APK + DEX; differential vs `dexdump`/`aapt2` | **done** (static-analysis tier) | `crates/acos-dex/tests/diff_dexdump.sh` → 3 files / 380 methods / 4,253 insns / 0 mismatches (`docs/FINDINGS.md` F-0004); `crates/acos-apk/tests/differential_aapt2.rs`; `docs/FINDINGS.md` F-0002 |
| **M1** println on the real core library | console programs execute on real `core-oj`/`core-libart`/`core-icu4j` DEX and match the ART oracle | **done** (art-reference) | `results/m1/README.md` (20/20 byte-identical, normal + GC-stress); `results/vm/OS_NATIVES_AUDIT.md` §3 (`--corelib real`: 25 pass / 0 fail / 0 unsupported of 25); `results/vm/dexprogs-real-normal.json` |
| **M2** `Application` + `Activity.onCreate` | `bindApplication` delivered; lifecycle `onCreate→onStart→onResume` matches ART | **done for counter** (gate enabled W147-A) | `bindApplication` + `Application.onCreate` run; the launch reaches `on_resume` and `first_frame` on `counter.apk` (`results/m2/M2_LOG.md`; `BENCHMARKS.md` §9.1 M3); `hello-views`/`form-validation`/`list-scroll` reach `first_frame` at W147-A (`f3120d5`); `crates/acos-runtime/tests/m2.rs` |
| **M3** first frame in `acos-render` | real non-black frame with `frame_seq`/cause/hash from a launched Activity | **done for counter** (W133-A) | `results/render/M3_LOG.md`, `results/render/m3-hello-views-vs-art.json`, `crates/acos-runtime/src/frame_handoff.rs`; framework-bound `acos launch counter.apk` → `first_frame` in **0.806 s p50** on the **default warm path** (5/5; cold control 1.073 s p50), `BENCHMARKS.md` §9.1 M3; the launched frame is not yet ART-scored |
| **M4** input + UI tree | `ui_initial.json` equality; tap → `clicks: 1` | **partial** — tree live, tap lands on `counter` | live `View` tree via `acos observe --tree` (M7 49.9 ms, `results/wm/README.md`); `acos tap --text Increment` on a kept session runs the app's `onClick` and the tree shows `count=1` (`results/m2/M2_LOG.md` §W161-A, `crates/acos-runtime/tests/counter_interaction.rs`); still missing: `ui_initial.json` equality against ART and the real AGSL shader (the ripple is a declared flat-colour fallback, W167-A). The §9.1 M8 row predates the tap fix (§9.5 item 3 is stale); §9.7 (W165-A) is the newer measurement |
| **M5** interaction trace | `lifecycle-summary.txt` + `ui_after_*` + native-diff | **not started** | depends on M2–M4 |
| **V1 gate** `acos verify` full verdict | `acos verify --apk … --scenario … --json --artifacts …` passes on the two fixtures with no Android boot | **partial (W180-A)** | `NativeBackend` now drives the product `AgentSession`: `counter.apk` core loop **passes** 13/13 (`scenarios/v1-counter.yaml`, 16.8 s, fresh capture frame 10, `results/v1/w180a/counter/verdict.json`); negative suite `fail`/`crash`/`unsupported` with **0 false passes** (`crates/acos-cli/tests/negative_verify.rs`); `hello-views` is blocked after typing by a >120 s traversal spin; `hello-jni` reports `UNSUPPORTED_NATIVE_SYMBOL` for an exported symbol (W93-A); rotation and ART bounds are open (W179-A). Full item-by-item status in `docs/V1.md` §W180-A and `results/v1/W180A.md` |

## V1 benchmark (2026-11-11, W160-A)

`BENCHMARKS.md` §9 is the authoritative measured table; raw per-round samples in
`results/bench/v1-2026-11-11-061354.json` (release, 3 rounds, loadavg 1.99,
35.8 GiB available, `.cache/target-w160a`, guarded). Built from `20663b3` + the
uncommitted W160-A patch; `main` moved to `b467193`/`65856f2` during the run.
Reproduce with `tools/bench-v1.sh --rounds 3`.

Against owner directive D2 (W176-A, §9.8): **9 PASS / 2 FAIL / 0 NOT_MEASURED**
(was 7 PASS / 4 FAIL at W165-A). Passes: M1 fixed idle core **133.90 MiB** vs
206.0 (W1); M2 N=5 per-session **39.40 MiB** vs 78.7 (v9); M3 framework-bound
`acos launch counter.apk` → `first_frame` **0.937 s p50** on the **default warm
path** (5/5) vs 3.620 s; M4 worker create→frame **0.719 s** vs 1.297 s; M6
reset→frame **714.7 ms** vs 748.1 ms; M7 live `View` tree **49.97 ms p50** vs
538 ms one-shot; M8 tap→state **249.2 ms**, `accepted=true`
`state_changed=true`; M10 end-to-end verify wall **1.664 s** vs ~19 s with the
assert passing (frame hash `b6759…`→`f79465…`, PNG diff 448 pixels); M11
**937.3 verified tasks/GiB-min** (`verified_tasks=1`). Fails: **M5** daemon
screenshot **99.35 ms** vs 66.5 ms (regressed from §9.1's 50 ms at HEAD;
`noisy`), **M9** per-session peak **PSS 528.2 MiB** (Pss_Anon 525.9 +
Pss_File 3.5; the ~114 MiB file-backed RSS is shared page cache) with input
independence still unproven. ACOS-native: process→`Vm` 16.37 ms, boot→attach
11.88 ms, `Vm::restore` 50.8 ms, SessionPool create 1.22 ms / reset 2.32 ms
(N=5), renderer cold frame 2.15 ms, PNG 7.37 ms, `frame-diff` 145 ms, live tree
50.0 ms. VM throughput is still behind `java -Xint` on 11/12 `Bench*` programs
(only `BenchString` 0.203×; worst `BenchGc` 2.55×). Noisy rows:
`observe_tree_ms`, M5 screenshot. Raw: `results/bench/v1-w176a.json` (built from
`fd1a887` + two then-untracked W172-A pieces; §9.8 records why).
`results/perf/BOOTIMAGE_CACHE.md` §W160-A).

**Test counts.** The full suite is **not** re-run every wave (the tree is shared
with other agents); each wave records its own gates. W160-A's gates were green
at `20663b3`+W160-A (`acos-vm` lib native_mem 6/6, vfs 18/18, snapshot 16/16,
`acos-runtime` boot_image_cache 6/6, `acos-cli` session 11/11 incl. the warm-cache
regression). W172-A adds: `acos-native-loader` lib 39/39 (sem membership + guest-stack
validation), `acos-jni` lib 30/30 (`NewDirectByteBuffer` range validation),
`acos-runtime` lib 35/35 (private-root + boot-cache rejection), boot-cache
6/6, `counter_interaction` 3/3, `m2` 3/3, `acos-cli` session 11/11,
`native_session` 1/1, `serve_daemon` 1/1, clippy `-D warnings --no-deps` clean
on the touched crates, `cargo check --workspace --all-targets` pass and
`tools/diff-dexprogs.sh --corelib real` **27 pass / 0 fail**, all re-run at
`fd1a887` in `.cache/target-w172a` through `tools/guarded.sh`. W172-A also
strengthened the `native_session`/`serve_daemon` frame assertions, gated the
boot-cache and RN-pixel skips on `have_fixture`, and made
`verify-all.sh`'s status-check step **fail** instead of warn.

### Known gaps (V1)

1. **[CLOSED] Boot-image restore for launch.** W154-A carried the native
   regions; W160-A records their file *source* and re-reads it, failing closed
   on a changed file. Warm `acos launch` reaches `first_frame` **5/5** at
   0.806 s p50 with a **25.58 MB** cache entry. Evidence: `BENCHMARKS.md` §9.1
   M3, `results/perf/BOOTIMAGE_CACHE.md` §W160-A. Regression guard:
   `crates/acos-cli/tests/session.rs::warm_boot_cache_reaches_first_frame_on_every_launch`
   (1 cold process + 5 warm processes).
2. **Launched frame not ART-scored.** `acos-render` produces a correct
   hello-views frame (cold 2.22 ms, SSIM 0.9938 vs the ART capture) and a
   launched Activity publishes through `frame_handoff` (M3), but no ART-diff
   has been recorded for the launched frame. Evidence:
   `results/render/README.md`, `results/render/M3_LOG.md`.
3. **[CLOSED for `counter`] Tap / input.** `acos tap --text Increment` now runs
   the app's `onClick` and the live tree shows `count=1` (W161-A; the W152-A
   blocker was a sync-barrier traversal, fixed by cancelling the pending
   traversal after input delivery and re-anchoring the vsync grid).
   `results/m2/M2_LOG.md` §W161-A, `crates/acos-runtime/tests/counter_interaction.rs`.
   The pressed-state **ripple** is a **declared flat-colour fallback**: W167-A
   implemented the AGSL/RippleShader natives well enough to draw the declared
   flat colour rather than the shader, which is a named visual divergence (not a
   fake frame). Pool reset is still core-VM-only (M6). Evidence:
   `BENCHMARKS.md` §9.8 M6/M8 (tap→state **249.2 ms**, frame hash and
   before/after PNGs change; measured after the tap fix), `results/m2/M2_LOG.md`
   §W161-A.
4. **M9 not independent.** One process-global Binder transport: N sessions are a
   memory/throughput experiment, not N isolated workers. Per-session peak is
   **PSS 528.2 MiB** at N=20 (private anon 525.9, shared file 3.5); the
   ~114 MiB file-backed RSS is shared page cache, not private cost. Evidence:
   `BENCHMARKS.md` §9.8.2.
5. **RN bridge.** Hermes/JNI confined-stack probes exist (`results/rn/`,
   `tools/rn-*`), but no React Native hello publishes a frame.
6. **Flutter first frame.** The Dart `main` executes in the confined engine
   (`results/flutter/FIRST_RUN.md`) but no Java `Surface`/`ANativeWindow` is
   delivered, so no first frame.
7. **IR tier default-off.** `ACOS_EXEC=ir` wins 2–2.7× on dispatch loops but is
   not default; `BenchGc`/`BenchAlloc` stay allocation-bound.
   Evidence: `BENCHMARKS.md` §8.9–§8.11.
8. **Committed artifacts lag.** Several `results/` files predate `2c682c7`;
   `results/vm/dexprogs-real-normal.json` (24/25) and
   `results/bench/baseline.json` (commit `fad5534`) are stale.

## Landed since the audit baseline (`9426b5b`, W67-A)

| Area | What landed | Evidence |
|---|---|---|
| **M2 Activity.onCreate** | real provider binder, `Path.approximate` triples, conditional parcelable codec and `@null` theme refs; launch reaches `MainActivity.onCreate` (`ad9c978`, `f84bd13`) | `results/m2/M2_LOG.md`, `crates/acos-runtime/tests/m2.rs` |
| **SQLite** | safe `acos-sqlite` engine (rusqlite + vendored C) behind the real `android.database.sqlite` natives; fixture 28 matches ART (`9114c58`) | `crates/acos-sqlite/tests/engine.rs`, `results/vm/W49A_SQLITE.md` |
| **Sandbox** | seccomp/Landlock/rlimits/namespaces confinement, fail-closed, loader requires confinement; hostile-process harness (`83d814d`) + W67-A hardening (extended seccomp posture, ABI-6 signal/abstract-socket scopes, hygiene-gated `confinement_active`) | `crates/acos-sandbox/tests/confine.rs`, `crates/acos-sandbox/src/linux/*` |
| **NDK** | `libandroid` surface: assets, bitmap, `ANativeWindow`, `ALooper`, `AChoreographer`, `AConfiguration`; GL traps (`a62c009`) | `crates/acos-jni/src/ndk_host.rs` (working tree), `fixtures/jni` |
| **JNI broker** | 227/233 `JNINativeInterface` slots with documented reasons; `CallNonvirtual*`, native OS-thread attach/detach (`b4d8a7b`) | `results/jni/JNI_COVERAGE.md`, `crates/acos-jni/tests/` |
| **Render corpus** | 12 hand-authored DisplayList/Canvas scene reconstructions vs ART API-34 captures (SSIM 0.948-0.995 on the widget corpus); geometry/colours were **measured off the reference** and per-scene thresholds are **calibrated post-hoc**, so this validates the rasteriser, not a VM-drawn frame (W94-A #12) | `results/render/W66A_CORPUS.md`, `results/render/W82A_CORPUS.md`, `results/render/corpus/` |
| **dexprogs** | 27/27 vs real ART (`--corelib real`, normal) | `results/vm/dexprogs-w45a-real.json`, `results/vm/dexprogs-real-normal.json` |

## What is verified vs claimed (tier labels)

Fidelity tiers are defined in `DECISIONS.md` ADR-0005 (updated by ADR-0015).

| Tier | Meaning | Evidence in repo |
|---|---|---|
| `jvm-reference` (weak) | host OpenJDK 17 output for the same source | `fixtures/expected/dexprogs/` (51 files) |
| `art-reference` (authoritative) | real ART on the API-34 `google_apis` x86_64 emulator | `fixtures/expected-art/dexprogs/` (75 files, 25 programs × stdout/exit/stderr); `fixtures/reference/{hello-views,hello-jni,counter,todo-list,multi-activity,form-validation}/art-api34-x86_64/`; `results/oracle/jvm-vs-art.md`; `results/oracle/dexprogs-runs.json` |
| `physical-device` | real phone capture | **captured, not interchangeable** — `fixtures/reference-device/` (moto g35 5G, Android 15/API 35; `DEVICE.md`/`COMPARISON.md`, commit `50f2f5f`). Kept separate from the API-34 emulator `art-reference`; density/API differ. |

**Verified (reproducible artifact exists):** DEX decode (`dexdump` differential),
APK manifest/AXML (`aapt2` differential), M1 console corpus on real core libs
(`results/m1/README.md`, `results/vm/OS_NATIVES_AUDIT.md`), ART-reference
captures for 6 apps, `acos-render` text metrics vs the ART UI dump
(`results/render/README.md`), `acos-wm`/`acos-services` acceptance suites
(`results/wm/README.md`, `results/services/REPORT.md`), snapshot/restore PoC
16/16 tests and 23/23 real dexprogs (`results/snapshot/REPORT.md`), the ELF/JNI
native loader (`results/native-loader/README.md`), `hello-jni` output matching
the ART reference (`crates/acos-jni/tests/hello_jni.rs`), and the measured
benchmark rows (`results/bench/REPORT.md`).

**Claimed / not yet verified:** Activity lifecycle order; any rendered frame from
a real Activity; UI tree; input; reset; concurrency with independent state;
`verified tasks / GiB-minute`; any end-to-end `acos verify` pass. The
`physical-device` tier is captured but only as a reference, not yet run as a
differential fixture.

## What runs

- **Console corpus:** 25 `fixtures/dexprogs` execute on the real Android 14 core
  library (`--corelib real`). The original 20-program M1 gate is byte-identical
  to `fixtures/expected-art` normal and GC-stress (`results/m1/README.md`);
  W25-B records **25 pass / 0 fail / 0 unsupported** (`results/vm/OS_NATIVES_AUDIT.md` §3).
  The committed `results/vm/dexprogs-real-normal.json` (from the `40f075c` run)
  still records **24/25** (`25_security_digest` fails) and
  `results/vm/dexprogs-real-gc-stress.json` records 23/23; the committed artifacts
  lag the working tree.
- **Opcode coverage:** 123 DEX opcodes executed, 95 implemented but unexercised,
  6 unsupported (`invoke-polymorphic`, `invoke-custom`, `const-method-handle`,
  `const-method-type`) — `results/vm/opcode-coverage.json`.
- **Framework boot:** real `ActivityThread` comes up, `attachApplication` is
  received by the Rust ATMS, `bindApplication` is delivered, and a real
  `ClientTransaction` is scheduled (`results/m2/M2_LOG.md`). `Application.onCreate`
  runs, the launch reaches `on_resume` and `first_frame` on `counter.apk`, and
  `VectorDrawable.nCreateGroup` is implemented (`BENCHMARKS.md` §9.1 M3).
- **Bench (native-runtime fidelity, not ART-verified):** `BENCHMARKS.md` §9 is
  the authoritative table (measured at `2c682c7`, release, 3 rounds,
  `results/bench/v1-2026-11-11-051824.json`). Headline: process spawn → `Vm`
  13.95 ms p50, framework boot → attachApplication 12.44 ms, idle core
  133.85 MiB booted / 21.71 MiB Vm-only, N=5 per-session 39.45 MiB,
  SessionPool create 1.24 ms / reset 2.22 ms, renderer cold frame 2.98 ms,
  `frame-diff` 171 ms, live View tree 49.9 ms, framework launch → `first_frame`
  0.903 s (boot cache off). D2 result **4 PASS / 7 FAIL**. The earlier
  `results/bench/REPORT.md` numbers (run `201637`, commit `ced0d70`) are
  superseded for V1 purposes and kept only as wave history.
- **On-disk boot-image cache (T-43b, W96-A, `BENCHMARKS.md` §8.14):** a fresh
  process restores the framework-init `ACOSSNAP` from a content-addressed cache
  under `ACOS_CACHE_DIR`/`~/.cache/acos` in **65.676 ms p50** vs a full
  framework boot of **94.221 ms p50** (1.44x; `results/bench/w96a-cache.json`).
  **`acos launch` is wired** (the old "not wired yet" claim is false at this
  HEAD): `acos launch counter.apk --json` reaches `first_frame` in **0.903 s
  p50** with `ACOS_NO_BOOT_CACHE=1`, 617 MiB peak RSS (`BENCHMARKS.md` §9.2).
  The cache **restore** path is **fixed** (W154-A carried the native regions,
  W160-A records their file source): a warm launch reaches `first_frame` 5/5 at
  0.806 s p50 (`BENCHMARKS.md` §9.1 M3; regression guard
  `crates/acos-cli/tests/session.rs::warm_boot_cache_reaches_first_frame_on_every_launch`).
  Forcing the cold path (`ACOS_NO_BOOT_CACHE=1`) is no longer required.

## What is unsupported (`UNSUPPORTED_JAVA_API` sites, by area)

Unregistered natives and explicit `unsupported_api` sites raise a structured
`UNSUPPORTED_JAVA_API` naming `Lclass;->name(sig)` — never a fake result.

| Area | Unsupported surface | Source |
|---|---|---|
| Graphics `android.graphics` | unregistered Canvas/Paint/Path/Matrix/Bitmap/Region/Text/HWUI entry points | `crates/acos-vm/src/natives/gfx_{canvas,paint,path,matrix,bitmap,text,hwui}.rs` |
| Fonts | `Font.nNewByteBuffer`, `Typeface.nativeReadTypefaces/nativeWriteTypefaces`; variable-font axes shape but the A8 mask renders the default instance | `crates/acos-vm/src/natives/fwk_fonts.rs`; ADR-0019 |
| ICU regex | constructs that cannot be matched exactly raise instead of silently mis-matching | `crates/acos-vm/src/natives/icu_regex.rs` |
| Parcel | ashmem blob (`nativeReadBlob`); unknown codec fields | `crates/acos-vm/src/natives/parcel.rs` |
| Native memory | `MAP_RW`/`MAP_PV` are private copies (no write-back) | `crates/acos-vm/src/natives/native_mem.rs`, `fwk_m2_filechannel.rs` |
| JNI / OAT | OAT introspection natives unregistered; JNI library loader raises | `crates/acos-vm/src/natives/fwk_dexfile.rs`, `jni_glue.rs` |
| `libcore.io.Linux` | 134 declared: 64 implemented, 9 pending, 61 unsupported-by-design (process control, networking, `mmap`, xattrs) | `results/vm/OS_NATIVES_AUDIT.md` §2 |
| Reflection | non-empty `Constructor.newInstance(Object[])` | `crates/acos-vm/src/natives/fwk_reflect.rs` |
| Vsync | `DisplayEventReceiver.nativeInit` on a dead `MessageQueue` | `crates/acos-vm/src/natives/fwk_vsync.rs` |
| Conscrypt | `SSL_*`/TLS, `EVP_PKEY`/`RSA_*`/`EC_*`/`X509_*`, AES-GCM | `crates/acos-vm/src/natives/fwk_conscrypt.rs` |
| Framework services | one process-global Binder transport → N sessions are a memory experiment, not N isolated workers | `results/bench/MEMORY_PROFILE.md` §5 |

## Known risks

- **Interpreter slower than `java -Xint`.** Final `tools/bench-vm.sh`
  comparison (`crates/acos-vm/PERF.md` "Final comparison"): BenchFib 1.55×
  slower, BenchLoop 4.13× slower, BenchAlloc 1.73× slower; BenchString 3.30×
  faster. vs JIT it is 4–42× slower (no JIT, by design).
- **Alloc/Loop speed targets missed.** Target was ≥4× Fib (met, 4.17×), ≥4×
  Alloc (3.58×) and ≥5× Loop (3.48×) — not met (`crates/acos-vm/PERF.md`).
- **ICU regex approximations.** Approximated constructs are a fidelity risk for
  Android-semantics regex (`crates/acos-vm/src/natives/icu_regex.rs`).
- **Variable fonts.** Axes are stored/applied to shaping, but the FreeType A8
  mask path renders the default instance (`results/m2/M2_LOG.md` W23-A;
  ADR-0019).
- **`OsConstants` in progress.** W25-B reports 538/538 `OsConstants` fields and
  a 64/9/61 `libcore.io.Linux` split in the working tree
  (`results/vm/OS_NATIVES_AUDIT.md`); not yet committed at HEAD.
- **M2 display/frame work uncommitted.** The real `DisplayInfo` parcel codec and
  APEX boot-classpath merge exist in the working tree (`display_java.rs`,
  `tests/display.rs`, `tests/boot_classpath.rs`) with no recorded full-lifecycle
  run.
- **Committed artifacts lag.** `results/vm/dexprogs-real-normal.json` (24/25)
  and `results/bench/baseline.json` (commit `fad5534`) predate the current tree.

## Fixture-dependent tests in CI

`fixtures/build/` and `.cache/boot/` are git-ignored, so differential tests
silently `SKIP` on a fresh clone. Setting `ACOS_REQUIRE_FIXTURES=1` turns every
skip into a **failure** (`crates/acos-vm/tests/fixtures_required.rs`);
`tools/diff-dexprogs.sh` and `tools/bench-acos.sh` set it by default. CI must run
`tools/build-fixtures.sh` / `tools/build-apps.sh` and the boot-cache extractor
first (`HONESTY_AUDIT.md` #11).

## Per-metric vs legacy

The authoritative ACOS-vs-legacy table is **`BENCHMARKS.md` §9.1** (measured at
`2c682c7`, 2026-11-11, release, 3 rounds;
`results/bench/v1-2026-11-11-051824.json`). Result: **4 PASS / 7 FAIL** against
owner directive D2 — M1/M2 memory, M3 framework-bound first frame and M7 live UI
tree beat the legacy ACVM; M4/M5/M6/M8/M9/M10/M11 are missing or fail. Note the
table predates W161-A, so its "tap does not land" rows (M8, §9.5 item 3) are
stale at HEAD: the tap lands on `counter.apk` (`results/m2/M2_LOG.md`
§W161-A). The next `tools/bench-v1.sh` run must refresh them. The older
`results/bench/REPORT.md` table (commit `ced0d70`) is retained as wave history
but is no longer the V1 record.

## Next 10 tasks (Rust VM track, frozen: historical)

Closed since the last refresh (do not re-open): **boot-image warm restore**
(W154-A/W160-A) and the **`counter` tap** (W161-A). Current list:

1. Re-run `tools/bench-v1.sh` on the committed post-W161-A HEAD so the V1 table
   reflects the tap fix, M8 and the ripple gap; record the exact committed SHA in
   the artefact (W170-A C1/C2). Owner: bench wave, not this doc.
2. Score the launched `counter` frame against the ART reference and route
   `observe --screenshot` evidence into a scenario (M5/M3 correctness).
3. Close the M2 gate with an ART-order lifecycle diff now that `on_resume` and
   `first_frame` are reached (`crates/acos-runtime/tests/m2.rs`), and decide the
   post-`onResume` error tolerance explicitly.
4. Finish the framework file/`/system` I/O substrate (FileChannel, `/system`
   fonts/resources) so `handleBindApplication` runs without a structured block.
5. Replace the W167-A flat-colour ripple fallback with real AGSL/RippleShader
   output, and keep the divergence declared in every interaction frame until then
   (`results/m2/M2_LOG.md` §W161-A/W167-A).
6. Commit the W25-B `OsConstants` + `libcore.io.Linux` work and clear the 9
   pending Linux natives (`results/vm/OS_NATIVES_AUDIT.md` §2).
7. Cover ICU regex constructs that currently raise, or record each as a declared
   divergence when it cannot change fixture output.
8. Give each session its own Binder transport so N workers are independently
   usable (M9/V1 §5.1; `results/bench/MEMORY_PROFILE.md` §5).
9. Keep the fixture-gate coverage honest: every fixture-dependent test calls
   `have_fixture` and is listed in `tools/verify-all.sh`'s `FIXTURE_TESTS`
   (W172-A extended it to the boot cache, M2, interaction and the RN/Flutter
   product targets).
10. Run the `scenarios/negative/*` suite (and a differential against the now
    captured `fixtures/reference-device/`) to prove zero false passes before any
    V1 verdict claim.

## Working tree / concurrency

HEAD `fd1a887` (W165-A) is the committed checkpoint (W172-A below it). The tree is shared with
concurrent waves (bench/pool, RN `fs` natives, ripple/tap, resources/input,
ICU), so the working tree carries their in-flight edits; each agent builds into
its own `CARGO_TARGET_DIR` (`.cache/target-w<N>`) through `tools/guarded.sh`.
Shared files (`heap.rs`, `reflect.rs`, `interp.rs`, `vm.rs`, `snapshot.rs`,
`vfs.rs`, `natives/*`) are edited in place; re-read before editing and never
`git stash`. Validate on a scratch worktree if the shared tree does not build.

## Decisions in force

Owner directives D1–D5 (`SPEC.md`, `docs/FINDINGS.md`) and ADR-0001…ADR-0019
(`DECISIONS.md`), in particular ADR-0007 (execute the real boot-classpath
bytecode), ADR-0008 (thin Rust around the framework), ADR-0009 (handle/arena
heap, precise GC), ADR-0010 (snapshot/restore), ADR-0014 (Rust JNI/ELF),
ADR-0016 (boot-jar sourcing; licence review open), ADR-0017 (Android DEX
loading semantics), ADR-0018 (host RAM ceiling), ADR-0019 (FreeType/HarfBuzz
text).

## Blocked / open

- **ADR-0016 licence review** (human/legal) before any distribution; does not
  block development.
- **Production-image dex stripping** untested (only `userdebug` available).
- **`android-34` framework sources** not installed (only `android-36`), so
  source-line references are indicative.
- **`physical-device` differential** not run: the capture exists
  (`fixtures/reference-device/`) but no scenario compares against it yet.
