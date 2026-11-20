# 5 — Existing ACVM: what to read, reuse and deliberately reject

## Exact legacy location and rules

**Existing engine:** `/home/Qui/Work/android-container`

The new agent should inspect this path **read-only** before finalizing ACOS architecture. Do not rename, delete, reset, switch branches, create commits, change Docker images, kill ACVM workers or alter its running host services as part of research. The user wants a **separate new project** at `~/Projects/acos`, not a continuation of the legacy source tree.

Expected reference documents (check existence; their status may be newer than this handoff):
- `PLAN.md` — central architecture and status.
- `TASKS.md` — tracked workstreams.
- `EXPERIMENTS.md` — measured experiments, rejected attempts and root causes.
- `ITERATIONS.md` — v0+ per-version scoreboard and benchmark rules.
- `STATE.md` — runtime/orchestrator state, if present.
- `research/PLAN-task-first.md` — earlier design for task-first verification and tiered execution.
- `bench/`, `agent/`, `tools/`, `research/task-first/`, `image/`, `spikes/` — actual harness, contracts, experimental code and evidence, if present.

Read the current source tree and actual Git history. Previous chat summaries are a starting point, not live verification of the project.

## Why the old engine is valuable

The old project is not wasted work. It proved a task-first idea could run real Android, established realistic resource measurements on this host and developed the control/verification features AI coding agents need. The new project should **reuse its knowledge, tests, benchmark scenarios, and potentially protocol schemas**, not inherit the redroid + per-user session architecture by default.

### Major verified discoveries (from user-provided history)

- Original stock Android emulator ~25.2 s cold boot, about **3.3 GiB** PSS and substantial idle CPU, unsuitable for many agents.
- redroid substrate around **753 MiB** starting footprint; extreme stripping brought bare core to ~278 MiB earlier, subsequently **W1 candidate ~206 MiB** (candidate image not default when last reported).
- Multi-user-per-core sessions were possible; virtual displays replaced costly overlay displays. **Old five-session count is a benchmark point, not an intrinsic system limit.** Ten extra virtual displays were created in earlier experiments, but this alone did not verify ten fully independent app sessions.
- v1 persistent frames reduced screenshot median from ~431 ms to **~67 ms** and resumed-session screenshot to ~215 ms. Frame-causality/freshness matters.
- v2 removed unnecessary secondary-user packages, reducing incremental session cost substantially. v9 baseline N=5 around **79 MiB** incremental working set (anon+shmem), N=1 around 130 MiB, depending on measurement setup.
- W7 experimental shared-process tasks added **11–22 MiB** per additional view (not independent), reinforcing that task != Android user. Apps with singleTask, singleInstance, document mode, noHistory and global navigation can invalidate shared-view semantics.
- v5–v12 reduced real core-to-first screenshot latency; after v12 about **3.62 s** median, while guest boot itself measured ~1.5 s in a prior stage. These are different metrics.
- D2 developed `acvm reset <id> --app` at ~0.11 s for the command and **~0.75 s to first fresh frame** (12 runs); the earlier 46–50 s delay was *first text tree after new-user observe dexopt*, not an actual app-reset operation.
- W5 opt-in park compaction reclaimed around **56 MiB PSS** while retaining state; frozen tier passed a 10-minute soak but had binder-freezer risks. No default assumption should be made without a reliable A/B.
- W2 software-rendering option reportedly reduced per-session memory dramatically but added around **120 MiB** fixed core cost and **~7.5 s** boot delay in initial builds. Trade-off and correctness require matched tests; render probe found `hardwareAccelerated=false` saved about **46.6 MiB** per N=5 session in its specific workload.
- KSM page dedup was powerful in opt-in measurements but created a timing side channel and cannot be a casual default for untrusted apps.
- Android-user creation, starting apps on non-default displays, framework permission services and software graphics were major sources of complexity and overhead.
- `system_server`/surfaceflinger/zygote/ART plus service startup underpin the fixed bare-core cost, but **~200 MiB was never proved a physical lower bound**.
- Agent-facing CLI/MCP, direct observation, structured errors, screenshot freshness, automated benchmark acceptance and a 14-step usability gate existed.
- **Security failure:** privileged redroid containers can be host-root equivalent; signing keys, auth sockets and secure adb reduced other risks but did not solve the privileged-container boundary.
- Benchmark correctness lessons: invalid boot rows, accidental fixed sleeps, wrong process/cgroup accounting, stale screenshots, noisy MemAvailable, swapped/zram state contamination, sequential rather than interleaved A/B and stale fixture APK builds all produced misleading results at various points.

### Latest reported legacy work state (not guaranteed live)

- v12, D2 and task-first research plan were merged/pushed; 281 agent and 138 bench tests passed in the report; app reset gate and U1 acceptance passed.
- W5 opt-in compaction was merged; W4 heap tweak was neutral and not promoted.
- R1 host-only renderer (Paparazzi/layoutlib) was merged as research; it caught seeded UI defects, but **warm render was ~6.3 s and 1.26 GiB RSS peak**, proving 'no Android' doesn't automatically mean faster/smaller.
- R2 shared-view compatibility matrix and live probes, D3 new-user text-tree delay, D4 toolkit gaps, U2 full dev-loop benchmark, W2b software-render overhead reduction were still in progress in the last supplied notes.
- W1 ~98 MiB bare-core saving was accepted in a candidate and rebased to v11; promotion to default was still pending the usability gate / host RAM slot when last reported.

The new agent must **reconcile against actual files and commits** if those status items changed after the user supplied the report.

## What to reuse

- Exact APK/screen/input test fixtures (where licensed) and correctness assertions.
- Benchmark harness concepts: cgroup accounting, p50/p95, interleaved A/B, stable frame sequence, no fixed sleeps, cold vs warm, 85% RAM policy.
- Control-plane contract ideas: CLI/MCP, task IDs, structured output, async frame/input and explicit errors.
- Agent usability needs: screenshots with timestamps, full text UI tree, input/tap/swipe, assert/diff, app reset, install, crash/ANR logs, network and permission testing, development profile.
- Capability-tier design from `research/PLAN-task-first.md`.
- Existing ACVM oracle results as a **behavioral comparator**: use reference APKs and rerun under actual reference environment when safe, without copying architecture.
- History of low-return and high-risk experiments (KSM, blind package stripping, host GPU, overlay displays, ART compilation image, privileged host security).

## What NOT to inherit automatically

- No Android `UserHandle` per session as a design primitive.
- No mandatory always-on `system_server`, Android telephony/launcher/SystemUI or generic device emulation.
- No redroid Docker `--privileged` assumption.
- No mandatory displays or software render loop for service/data tests.
- No global five-session limit.
- No default host-wide KSM or swap changes.
- No benchmark comparisons mixing PSS with cgroup anonymous+shmem or different app workloads.
- No assumptions that APK launch without crashing equals functional compatibility.
- No hidden delegation of the whole native engine to C++ ART (user explicitly wants Rust runtime/DEX/JNI).

## Proposed read-only audit commands for the local agent

```bash
cd /home/Qui/Work/android-container
pwd && git status --short && git log -5 --oneline
find . -maxdepth 3 -type f \( -name 'PLAN.md' -o -name 'TASKS.md' -o -name 'EXPERIMENTS.md' -o -name 'ITERATIONS.md' -o -name 'PLAN-task-first.md' \) -print
# Read only what exists; do not automatically run benchmarks, modify git, or start containers.

mkdir -p ~/Projects/acos
cd ~/Projects/acos
# Initialize the NEW repo here; do not git init inside android-container.
```

**Critical:** Existing ACVM remains a useful tool to validate whether ACOS's Rust runtime produces the same result on a real Android app. Its established design decisions are **research inputs, not new-project constraints**.
