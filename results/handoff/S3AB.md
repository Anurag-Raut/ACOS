# S3AB handoff — S3a/S3b reset/relaunch investigation

**Date:** 2026-11-13 ~09:50 UTC (handoff written after the last measurement attempt)
**Worktree:** `/home/Qui/.paseo/worktrees/3oxa6xao/s3ab`
**Branch:** `fix-s3ab-reset`, based on `main` `6301c06` (`verify: an assertion bound to a settle: fast input frame fails closed`)
**State:** **no commit has been made**, **nothing pushed/merged**. All work is in the working tree.
**Reading order:** this file first, then `results/bench/S3ab.md` (checkpoint 1), then the diffs and evidence listed below. Checkpoint 1 is now partially stale: it describes an `ADD_FLAG_APP_VISIBLE` experiment that was later **reverted** (it broke T03 pixels); see §2.4/§3.

---

## 1. Exact current state of the investigation

### 1.1 What is proven, with evidence

#### S3b class A — host task-limit `EAGAIN` in the vsync thread

**Proven, reproduced at N=40, fixed in the harness.**

At N=40 in one `acos serve`, two guests exited 1 before their first frame. The first failed attempt was made **before** the `guarded.sh` change. Evidence:

* `results/bench/S3ab-evidence/repro-n40-thread-eagain/a9-art-frames/session-exit.json`
* `results/bench/S3ab-evidence/repro-n40-thread-eagain/a16-art-frames/session-exit.json`
* `results/bench/S3ab-evidence/repro-n40-thread-eagain/serve.log`

`session-exit.json` says (verbatim facts):

```
code=GUEST_EXIT_AFTER_RESUME guest_exit=1 guest_signal=null
guard: seccomp: OK guard=active insns=83 / OK landlock=active abi=10 rules=15 skipped=0
logd:
  E/DisplayEventReceiver: DisplayEventConnection creation failed:
    status=Status(-3, EX_ILLEGAL_ARGUMENT): 'internal service error:
    vsync thread: Resource temporarily unavailable (os error 11)'
  W/DisplayEventDispatcher: Failed to initialize display event receiver, status=-19
  I/: System.exit called, status: 1
guest:
  ACOSLAUNCH {"code":"LOOPER_THREW", ... "java.lang.RuntimeException:
   Failed to initialize display event receiver. status=-19" ...}
```

`serve.log` at the same moment:

```
acos: HOST_PANIC: panicked at ... failed to spawn thread:
  Os { code: 11, kind: WouldBlock, message: "Resource temporarily unavailable" }
```

Mechanism:

1. `tools/guarded.sh` used `systemd-run --scope -p TasksMax=512`.
2. A 20/40-session cohort (each session = launcher threads + an ART guest with
   main/GC/JIT/binder/probe threads + one hosted vsync thread) exhausted the
   scope's pids/task controller.
3. `ConnectionClock::new` in `crates/acos-services/src/display_events.rs`
   spawns the vsync writer thread from `createDisplayEventConnection`. Its
   `std::thread::Builder::spawn` returned `EAGAIN`.
4. The host answered the binder call with
   `EX_ILLEGAL_ARGUMENT: vsync thread: Resource temporarily unavailable`;
   the guest's `DisplayEventReceiver.nativeInit` returned `-ENODEV`; the
   framework's `Choreographer` constructor threw; `Looper.loop` ended with
   `LOOPER_THREW`; the guest called `System.exit(1)`.

**H17 is ruled out for this class:** the guard line is `OK guard=active` and
`OK landlock=active`, not `ERR landlock=plan-missing`, not exit 126; there is no
SIGBUS, no exit 135, no truncated-APK signal. This was a host task-count limit,
not a shared input rewrite.

**Harness fix:** `tools/guarded.sh` now uses
`TasksMax=${ACOS_GUARD_TASKS_MAX:-8192}`. After that change (and before the
later product changes), N=40 ran **600/600 verified, 0 false passes, 0
failures** (15 reps × 40 tasks): `.cache/s3ab/before-n40-on-8192.json`
(binary sha256 `0f18b9df390c3e83340573c940392a57c9db68ad3699723b9167bf85da74d024`).

#### S3b class B — malformed binder reply on `addToDisplayAsUser` (still open)

**Proven symptom, reproduced at N=20 after the TasksMax fix; root cause not fixed.**

A 600-task N=20 run (`30 reps × 20`, final code before the bounded-retry change,
binary sha256 `4bf7377436662837bdb7ed4b185fe7098abef1be84d5ba400b2d34469d8252a8`)
had one failure:

* `.cache/s3ab/final-n20-on.json` → rep 6, agent 13, index 1300001, reset failed
  after 215.106 ms with `GUEST_EXITED: the guest ended before its first live frame
  (status exit status: 1)`; **599/600 verified, 0 false passes**.
* Preserved evidence:
  `results/bench/S3ab-evidence/repro-n20-binder-parcel/`
  (`session-exit.json`, `live-session.json`, `serve.log`, `rep-006.json`).
* Full scratch, including traces: `.cache/s3ab/final-n20-on/evidence/rep-006/`.

The failing guest's own records:

```
code=GUEST_EXIT_AFTER_RESUME guest_exit=1 guest_signal=null
guard: seccomp: OK guard=active insns=83 / OK landlock=active abi=10 rules=15 skipped=0
logd:
  E/Parcel: ipcSetDataReference: unsupported type object (7209057) at offset 16
  I/: System.exit called, status: 1
guest:
  ACOSLAUNCH {"code":"LOOPER_THREW", "activity_resumed":true,
   "message":"java.lang.RuntimeException: Adding window failed",
   "causes":[{"class":"java.lang.RuntimeException",
     "message":"Unknown exception code: -1040187388 msg null",
     "stack":["android.os.Parcel.createException(Parcel.java:3044)",
              "android.view.IWindowSession$Stub$Proxy.addToDisplayAsUser(...)",
              "android.view.ViewRootImpl.setView(ViewRootImpl.java:1340)", ...]},
    {"class":"android.os.RemoteException",
     "message":"Remote stack trace:\nandroid.view.IWindow"}]}
```

Interpretation (**hypothesis, not proven**): the guest could not parse the
`addToDisplayAsUser` reply. `0x6e0061` (7209057) looks like UTF-16 characters
inside the `InputChannel` name (`"Surface(name=..."`) being read where a binder
object type should be; the object-table/data alignment of the reply is wrong.
The host did not record an `addToDisplayAsUser` error in
`unsupported.jsonl` (only the usual `IActivityClientController` complaint), so
the service believed it succeeded. This points at the peer/transport reply
encoding (`frame_from_parcel` / InputChannel binder-token object table) rather
than at the app. **H17 is again ruled out** (guard OK, no signal, no
native_crash).

A bounded, host-only retry was added for this class (see §3.5) but has **not
been exercised end-to-end yet**. The very last attempt to measure it produced
no result (§5.1).

#### S3a — no direct reproduction yet; reason-based fix only

No S3a occurrence was reproduced. Across the valid baseline/after runs before
the final retry change, **more than 3 000 tasks** were run without a
`NO_LIVE_FRAME` (see §4). S3's rates (1/100) would predict ~30 events, so the
rate in this host window is much lower. **S3a is therefore a code-level
root-cause hypothesis, not a reproduced incident.**

The hypothesis is still strong and is supported by the old failing record
`results/bench/S3-evidence/loop-n20-reset-nofreshframe.json`:
`activity_resumed=true probe=true c1_generation=0`.

The guest could publish `activity-resumed` **without a draw**:

* `LaunchMain.awaitFirstTraversal` posts a check to the main looper. If
  `ViewRootImpl.mFirst` is already `false`, it immediately reports ready
  ("a window path that already traversed synchronously").
* AOSP sets `mFirst = false` at the end of the **first traversal, before the
  draw decision** (`ViewRootImpl.performTraversals`, `mFirst=false` around line
  4015; `performDraw` around 4093). A first traversal that skips the draw
  (view not visible, screen off, cancel/redraw) therefore still certified
  readiness. The live tree probe keeps sampling (so `probe=true`), while C1
  stores no buffer (`c1_generation=0`), and the launcher waits out its 60 s
  no-frame budget.
* The likely reason a first traversal is not visible is that ACOS does **not**
  set `ADD_FLAG_APP_VISIBLE` in the `addToDisplayAsUser` reply; it relies on
  the later oneway `IWindow.dispatchAppVisibility(true)` callback. If that
  oneway delivery is late or lost (reverse delivery to a Java binder under
  load), the first traversal sees `mAppVisible=false`, skips the draw, and
  nothing later invalidates the window.

The fix in the working tree:

1. `LaunchMain.awaitFirstTraversal` now requires **actual draw evidence**
   (`mLastPerformTraversalsSkipDrawReason`, `mLastPerformDrawSkippedReason`,
   `mLastDrawScreenOff`, `mLastTraversalWasVisible`). If a completed traversal
   skipped the draw, it records the reason, requests one redraw
   (`decor.requestLayout(); decor.invalidate()`), and waits for an
   `OnDrawListener`. It no longer silently reports ready.
2. If the skip reason is `not_visible`, it replays the host's
   `ViewRootImpl.dispatchAppVisibility(true)` (public method, idempotent) and
   then redraws. This is the callback the host meant to deliver, replayed only
   when its absence is observable; it is done **after** the first traversal so
   normal focus/pixel order is preserved.
3. If a draw still never happens it fails with `POST_RESUME_NOT_READY` and a
   `draw_skip` reason instead of hanging for 60 s.
4. The host `NO_LIVE_FRAME` record now carries a C1 snapshot
   (`c1_layers`, `c1_transactions`, `c1_retained`, `c1_destroyed`, per-layer
   `tx/gen/buf/hidden/placed`) and probe age/sequence/draw sequence, so the
   next occurrence can distinguish "no surface" from "surface, no buffer" from
   "layer destroyed".

**Important:** an earlier attempt set `ADD_FLAG_APP_VISIBLE` on the add reply
instead. That made the first traversal draw **before** focus/visibility
settled and regressed `suite/T03-render-good` by 945 compressed PNG bytes /
~198k differing pixels (missing caret and different EditText hint colour).
That change was **reverted**. Do not re-try it without solving the
focus/second-traversal ordering (see §2.4).

### 1.2 What is still hypothesis / open

* The exact reason the oneway visibility callback would be lost (if S3a is that
  class) is not proven. The guest-side replay is a recovery, not a proof of the
  loss.
* The malformed `addToDisplayAsUser` reply (S3b class B) is **not root-fixed**.
  The current mitigation is a single bounded relaunch retry for host transport
  strings, with the first failure preserved.
* The retry path has **not** been observed firing. It is unit-tested at the
  classifier level only.
* No S3a/S3b evidence was captured at the exact S3 rates in this worktree at
  N=10/N=20 before/after; the strongest reproduction is N=40 (class A) and
  N=20 1/600 (class B).

---

## 2. Everything tried that did not work (do not repeat blindly)

### 2.1 `acos-bench.sh` loop cells as the reproducer

`tools/bench/acos-bench.sh --groups "g:loop20:24G:650"` deletes the loop
scratch directory in `phase_loop` (`shutil.rmtree` in `finally`), even with
`--keep-scratch`, so the failing guest's `session-exit.json`, guard line and
guest log are destroyed. It is fine for throughput measurement, **not** for
capturing S3a/S3b evidence.

### 2.2 P5 loop at very high `--loops` (3000 tasks in one serve)

`p5_loop.py` writes a ~9.5 MiB caller PNG plus an engine frame mirror per task.
A 3 020-task run produced **27 GiB** of PNGs and hit the guarded wall clock;
the JSON was never written. The orchestration deleted the 27 GiB of screens.
Do not keep per-task PNGs. Use the new `tools/bench/s3ab_repro.py`, which
deletes PNGs after each rep and keeps ≤ small evidence.

### 2.3 N=20 with `TasksMax=512` as an S3a reproducer

1 900 tasks at N=10/N=20 with the old 512 limit produced **0 failures**. The
512 limit only made the class-A failure easy at N=40. It did not reproduce S3a.

### 2.4 `ADD_FLAG_APP_VISIBLE` in the add reply (reverted)

Setting `WindowManagerGlobal.ADD_FLAG_APP_VISIBLE` (0x2) in
`window_session::write_add_result` / `addToDisplayWithoutInputChannel` made
the guest's first traversal draw. It broke `suite/T03-render-good`:

```
ASSERTION_FAILED: region differs from scenarios/suite/baselines/T03-hello-views-top.png
(945 differing bytes; ~198k pixels; missing EditText caret and different hint colour)
```

Reason: the first traversal now draws before the visibility/focus-settled
second traversal that the old oneway callback triggered. The change was
`git checkout`-reverted, including its test changes. **Do not re-apply it** as
a quick S3a fix.

### 2.5 Host-side `dispatchAppVisibility` re-delivery plumbing

A host-side re-delivery via `WmsState`/`LiveHandles` was designed but not
implemented; the guest-side replay in `LaunchMain` was used instead. The design
is still a possible alternative if the guest-side method proves insufficient:
`WmsState.forced_visibility: Vec<PendingDelivery>`, drained by
`WindowSessionService::drain_raw_deliveries`, plus a `Peer` flush on idle
timeout. It was dropped for scope/time and because it needs a `LiveHandles`
field change.

### 2.6 N=40 after the TasksMax fix as a general S3a detector

600 tasks at N=40 with `TasksMax=8192` passed; the later N=20 1/600 binder
failure shows N=40 is not a substitute for the N=20 cohort.

### 2.7 Ignoring the 60 s `NO_LIVE_FRAME` as "just slow"

Do not shorten the startup budget or retry it silently. The old record proves
`activity_resumed=true probe=true c1_generation=0`; a longer budget would only
hide a real no-frame state. The fix must be evidence + draw recovery.

---

## 3. Precise uncommitted diff in this worktree

`git status --short` (current):

```
 M crates/acos-art/guest/java/dev/acos/art/LaunchMain.java
 M crates/acos-art/src/launch.rs
 M crates/acos-cli/src/engine/art.rs
 M crates/acos-cli/src/engine/serve.rs
 M crates/acos-services/src/display_events.rs
 M tools/guarded.sh
?? out/
?? results/art-runtime/evidence/P5/__pycache__/
?? results/bench/S3ab-evidence/
?? results/bench/S3ab.md
?? tools/__pycache__/
?? tools/art-runtime/__pycache__/
?? tools/bench/__pycache__/
?? tools/bench/s3ab_repro.py
```

`git diff --stat`:

```
 crates/acos-art/guest/java/dev/acos/art/LaunchMain.java | 107 +++++++++++++-
 crates/acos-art/src/launch.rs                           |  58 +++++++-
 crates/acos-cli/src/engine/art.rs                       | 179 +++++++++++++++++++--
 crates/acos-cli/src/engine/serve.rs                     |  10 +-
 crates/acos-services/src/display_events.rs              |  64 +++++++-
 tools/guarded.sh                                        |  11 +-
 6 files changed, 404 insertions(+), 25 deletions(-)
```

Nothing is committed because (a) the S3a fix is not end-to-end proven, (b) the
S3b class-B retry has not been exercised, (c) the final full gates/scenarios
have not been run against the final binary, and (d) the evidence/report files
are intentionally left uncommitted for the orchestrator to decide packaging.

### 3.1 `crates/acos-art/guest/java/dev/acos/art/LaunchMain.java` (+107/−...)

S3a guest readiness and recovery:

* new `static volatile String sTraversalSkipReason`;
* `awaitFirstTraversal` no longer accepts `mFirst == false` alone. It calls
  `drawSkipReason(decor)`; reports ready only when the last completed traversal
  actually drew;
* on a skipped draw it records the reason, calls
  `redeliverAppVisibility(decor)` when the reason is `not_visible`, then
  `decor.requestLayout(); decor.invalidate();` and waits for the OnDraw
  listener;
* new `redeliverAppVisibility(View)` invokes the public
  `ViewRootImpl.dispatchAppVisibility(true)` via reflection;
* new `drawSkipReason(View)` reads only framework fields:
  `mLastPerformTraversalsSkipDrawReason`, `mLastPerformDrawSkippedReason`,
  `mLastDrawScreenOff`, `mLastTraversalWasVisible`;
* `POST_RESUME_NOT_READY` now carries `draw_skip=<reason>` and prints it in
  `ACOSLAUNCH MARK activity-not-ready`.

Why not committed: the recovery path has not been hit by a real S3a
occurrence; the normal path passes `v2-art-counter`, `T03-render-good`,
`w15-dialog-menu`, `form-validation`, etc. (see §4.3).

### 3.2 `crates/acos-art/src/launch.rs` (+58/−...)

S3a evidence:

* new `c1_state_summary(live)` — bounded layer list and exact counters
  (`c1_layers`, `c1_transactions`, `c1_retained`, `c1_destroyed`,
  `name:id:tx:gen:buf:hidden:placed` for up to 8 layers);
* the `NO_LIVE_FRAME` ready error now appends the C1 summary plus
  `probe_seq`, `probe_age_ms`, `probe_gen`, `probe_draw_seq`.

Why not committed: diagnostic-only, but it is the evidence needed for the next
occurrence; review wording/size.

### 3.3 `crates/acos-cli/src/engine/art.rs` (+179/−...)

S3b host-transport retry and evidence:

* `ArtSession` gains `relaunch_retry: Option<serde_json::Value>` and getter
  `relaunch_retry()`;
* `reset()` now runs the relaunch start in a 2-attempt loop. On attempt 1 it
  calls `relaunch_transport_reason(error, artifacts)`; only named host strings
  match:
  * `vsync thread:`
  * `display event receiver`
  * `ipcsetdatareference`
  * `unsupported type object`
  * `adding window failed`
  * `host_panic`
  A generic `GUEST_EXITED` / app wall never matches;
* on a match, `preserve_failed_relaunch` copies the first attempt's
  `session-exit.json`, `live-session.json`, `session-timing.json`,
  `native-crash*`, `session.json`, `argv`, `unsupported.jsonl` into
  `<artifacts>/relaunch-retry-1/` and writes `relaunch-retry.json` both there
  and at `<artifacts>/relaunch-retry.json`;
* the retry is exactly **one** extra attempt; if it fails too, the second error
  is returned and the first evidence remains;
* unit test `only_host_transport_startup_failures_are_retryable` checks that a
  normal app exit is not retried and the host strings are.

Why not committed: **the retry has never fired end-to-end**. It is a mitigation
for S3b class B, not a root fix; the orchestrator must decide whether a bounded
retry is acceptable here (the task brief allows it only if the cause is outside
the app and the first failure is reported — the evidence path does that, but a
review is needed). Also the final full `acos-cli` gate has not been rerun after
this change.

### 3.4 `crates/acos-cli/src/engine/serve.rs` (+10/−...)

The successful `acos_reset` reply now includes
`"relaunch_retry": {...}` when the engine retried a host-transport startup
failure. This makes the retry visible to a caller/evidence bundle. No reply
change on ordinary resets.

Why not committed: same as 3.3; needs an exercised path and gate.

### 3.5 `crates/acos-services/src/display_events.rs` (+64/−...)

S3b class-A defense-in-depth: `ConnectionClock::new` now acquires the vsync
writer thread with a bounded retry loop:

* 20 attempts, 10 ms backoff (max ~200 ms);
* retries only `WouldBlock`/`OutOfMemory` (EAGAIN/ENOMEM);
* logs at attempt 1 and every 5th;
* final error names the attempt count.

This is a thread-resource acquisition retry, logged; it does not relaunch a
guest. It did not fix class B and did not by itself fix S3b on a saturated
512-task scope (the serve process itself also hit `HOST_PANIC`), which is why
the `guarded.sh` limit fix is the primary class-A fix.

Why not committed: final review; it is product-side safety even if the harness
limit is corrected.

### 3.6 `tools/guarded.sh` (+11/−...)

`TasksMax` is now configurable:
`TASKS_MAX="${ACOS_GUARD_TASKS_MAX:-8192}"`, used in the `systemd-run` scope
(was hard-coded 512). Header documents it. This is the primary fix for S3b
class A. It is a harness/environment fix, **not** a product fix; see risks.

### 3.7 `tools/bench/s3ab_repro.py` (new, untracked, ~15 KB)

Targeted S3a/S3b reproducer. It imports the unmodified P5 loop, runs short reps
(default one measured task per session so a failing reset is the last action),
and:

* applies the `animations` arm to `acos_launch`;
* after each rep, copies `session-exit.json`, `live-session.json`,
  `session-timing.json`, `native-crash*`, and the engine's
  `relaunch-retry.json` / `relaunch-retry-1/` for failures/retries;
* deletes all PNGs after each rep (the disk lesson above), and deletes
  successful `sessions/` and big traces;
* writes a summary with `measured`, `verified`, `false_pass`,
  `relaunch_retries`, `failed_reps`, binary sha, host before/after;
* installs SIGTERM/SIGINT handlers and a `/proc` sweep so a guarded timeout
  does not leave an orphan `acos serve` (the earlier high-loop run left PID
  625897; it was killed manually; current `ps` shows none).

Why not committed: new harness; the cleanup was added after the last natural
failure and has not been re-validated under a forced timeout. It should be
reviewed and probably committed with the S3ab report.

### 3.8 Untracked/evidence files

* `results/bench/S3ab.md` — checkpoint 1 write-up (6 KB). **Partially stale:**
  it still describes `ADD_FLAG_APP_VISIBLE` as the planned S3a fix; that was
  reverted. Use this handoff for the current state.
* `results/bench/S3ab-evidence/repro-n40-thread-eagain/` — class-A evidence.
* `results/bench/S3ab-evidence/repro-n20-binder-parcel/` — class-B evidence
  (`session-exit.json`, `live-session.json`, `serve.log`, `rep-006.json`).
* `out/` — artifacts from the scenario runs (`out/T03-render-good`, etc.);
  should not be committed.
* `__pycache__/` under `tools/`, `tools/art-runtime/`,
  `results/art-runtime/evidence/P5/` — generated; should not be committed.

---

## 4. Measured results before handoff

### 4.1 Baselines (old binary `0f18b9df…`, `TasksMax=512` unless noted)

| run | arm | tasks | verified | false pass | failures | evidence/file |
|---|---|---:|---:|---:|---|---|
| before | N=20 on | 1 200 (60×20) | 1 200 | 0 | 0 | `.cache/s3ab/before-n20-on.json` |
| before | N=20 on (p5 direct) | 600 | 600 | 0 | 0 | `.cache/s3ab/p5/before-n20-on-600.json` |
| before | N=10 on (p5 direct) | 600 | 600 | 0 | 0 | `.cache/s3ab/p5/before-n10-on-600.json` |
| before | N=40 on, `TasksMax=8192` | 600 | 600 | 0 | 0 | `.cache/s3ab/before-n40-on-8192.json` |
| before | N=40 on, `TasksMax=512` | 40 attempted | — | 0 | 2 × class-A, then HOST_PANIC; run aborted | `results/bench/S3ab-evidence/repro-n40-thread-eagain/` |

Host load was recorded per run; ranges were roughly load1 0.7–13, MemAvailable
43–49 GiB, host RAM well under 90 %. The low-rate S3a/S3b sample at N=10/20 is
a limit: S3's 1/100 rates were not reproduced in 2 500+ baseline tasks.

### 4.2 After the **add-flag** binary `870a26f1…` (later reverted for T03)

These runs are **not** the final code, but they show the harness and the arms.
All were 600/600, 0 false passes:

* `.cache/s3ab/after-n20-on.json` (30×20)
* `.cache/s3ab/after-n10-on.json` (60×10)
* `.cache/s3ab/after-n20-off.json` (30×20)
* `.cache/s3ab/after-n10-off.json` (60×10)

The add-flag binary was rejected because `suite/T03-render-good` regressed
(see §2.4).

### 4.3 After reverting add-flag and adding guest recovery (binary `4bf73774…`)

* N=20 on, 600 tasks: **599/600**, 0 false passes, 1 × S3b class B
  (`final-n20-on.json`, rep 6). This is the run that produced the binder-parcel
  evidence.
* All 15 gate scenarios were run with this binary and gave the expected result:

| scenario | expected | result |
|---|---|---|
| v2-art-counter | pass | pass |
| w22-counter-reset | pass | pass |
| w54-dialog-pixels | pass | pass |
| w42-back-frame | pass | pass |
| w15-dialog-menu | pass | pass |
| form-validation | pass | pass |
| suite/T03-render-good | pass | **pass** (after revert; it had failed with the add flag) |
| v1-offscreen-pager | pass | pass |
| p11-background | pass | pass |
| p11-delay1000 | pass | pass |
| p12-progress-anim-off | pass | pass |
| suite/T03-render-wrong | fail / ASSERTION_FAILED | fail / ASSERTION_FAILED |
| negative/offscreen-pager-visible | fail / ELEMENT_OFFSCREEN | fail / ELEMENT_OFFSCREEN |
| negative/offscreen-pager-wait | fail / TIMEOUT | fail / TIMEOUT |
| p11-progress-timeout | non-pass / TIMEOUT | non-pass / TIMEOUT |

The JSONs are in `.cache/s3ab/gates-scenarios/`. These were run under
`run-exclusive.sh --task S3AB` + `guarded.sh --mem 16G --time 590`.

### 4.4 Cargo gates (binary era before the final retry change)

Run per crate under `guarded.sh --mem 16G --time 900`:

* `acos-art`: rc 0, logs `98/8/4/6/1/2/7/5/3/5/5/4` tests, 0 failed —
  `.cache/s3ab/gates-acos-art.log`
* `acos-cli`: rc 0, all suites 0 failed —
  `.cache/s3ab/gates-acos-cli.log`
* `acos-verify`: rc 0, 43 + 23 tests, 0 failed —
  `.cache/s3ab/gates-acos-verify.log`

**These are stale for the final code**: after them, `window_session.rs` was
reverted, `LaunchMain.java`/`launch.rs` changed, and `art.rs`/`serve.rs` gained
the retry. Only the targeted `acos-cli` retry-classifier test has been run
since (`only_host_transport_startup_failures_are_retryable`, passes). Full
gates must be rerun.

### 4.5 Final state of the last measurement attempt

After adding the bounded retry, the binary is:

```
sha256 .cache/s3ab-target/release/acos = 7c7553940fccdabb4021486e03dca31add6fa4ebdd74d94cbc930f948244416a
```

The intended N=20 on 600 run **did not produce a result**:
`.cache/s3ab/final2-n20-on.json` does not exist, no `acos`/`s3ab_repro`
process is left, and the tool call returned no output. Treat this as **not
measured**, not as a pass. The previous 599/600 run is the last valid N=20
number.

---

## 5. Exact next action, in order

### 5.1 First: clean and confirm no leftovers (do not skip)

```bash
cd /home/Qui/.paseo/worktrees/3oxa6xao/s3ab
ps -eo pid,ppid,pgid,etime,cmd | grep -E 's3ab_repro|\.cache/s3ab/' | grep -v grep
# Must be empty. If an `acos serve` is left, kill its process group:
#   kill -TERM -- -<pgid>; sleep 2; kill -KILL -- -<pgid>
rm -rf out tools/__pycache__ tools/art-runtime/__pycache__ \
       tools/bench/__pycache__ results/art-runtime/evidence/P5/__pycache__
df -h .   # currently 81 % used (172 GiB free); clean .cache/s3ab screens/traces if needed
```

Do not delete `results/bench/S3ab-evidence/`, `.cache/s3ab/*.json` summaries,
or `.cache/s3ab/final-n20-on/evidence/rep-006/` (the class-B evidence).

### 5.2 Rebuild the final binary and rerun the full gates

```bash
export ACOS_FONT_DIR=/home/Qui/Projects/acos/.cache/boot/android-34/fonts
unset ACOS_BOOT_DIR
export CARGO_TARGET_DIR=$PWD/.cache/s3ab-target
export ACOS_ART_BUILD_CACHE=$PWD/.cache/s3ab/build-cache

tools/guarded.sh --mem 16G --time 900 -- \
  cargo build --release --offline -j 4 -p acos-cli -p acos-art

# per crate, exactly the task brief's gates:
tools/guarded.sh --mem 16G --time 900 -- \
  cargo test --release --offline -j 4 -p acos-art > .cache/s3ab/gates-acos-art.final.log 2>&1
tools/guarded.sh --mem 16G --time 900 -- \
  cargo test --release --offline -j 4 -p acos-cli > .cache/s3ab/gates-acos-cli.final.log 2>&1
tools/guarded.sh --mem 16G --time 900 -- \
  cargo test --release --offline -j 4 -p acos-verify > .cache/s3ab/gates-acos-verify.final.log 2>&1
```

All three must be rc 0 with `0 failed`. Do not run
`cargo test --workspace` in one call.

### 5.3 Rerun the 15 scenarios against the final binary

Use the exact loop that produced §4.3, extracting `app:` from each YAML and
running under `run-exclusive.sh --task S3AB -- tools/guarded.sh --mem 16G
--time 590 -- ...`. Required expectations:

* pass: `v2-art-counter`, `w22-counter-reset`, `w54-dialog-pixels`,
  `w42-back-frame`, `w15-dialog-menu`, `form-validation`,
  `suite/T03-render-good`, `v1-offscreen-pager`, `p11-background`,
  `p11-delay1000`, `p12-progress-anim-off`;
* fail as designed: `suite/T03-render-wrong` (`ASSERTION_FAILED`),
  `negative/offscreen-pager-visible` (`ELEMENT_OFFSCREEN`),
  `negative/offscreen-pager-wait` (`TIMEOUT`),
  `p11-progress-timeout` (`TIMEOUT`, non-pass).

Pay special attention to `T03-render-good`: it is the regression that rejected
`ADD_FLAG_APP_VISIBLE`. If it differs, the guest recovery/redraw is still
changing the first frame; stop and investigate before any measurement.

### 5.4 Rerun the A/B reliability matrix (the task brief's numbers)

Use `tools/bench/s3ab_repro.py` (not `acos-bench.sh` for evidence). Each run:

```bash
tools/art-runtime/run-exclusive.sh --task S3AB -- \
  tools/guarded.sh --mem 24G --time 590 -- \
  python3 tools/bench/s3ab_repro.py \
    --acos $PWD/.cache/s3ab-target/release/acos \
    --out .cache/s3ab/final-n20-on.json \
    --scratch .cache/s3ab/final-n20-on \
    --n 20 --tasks 1 --reps 30 --animations on
```

Repeat for:

* N=20 on, 30 reps = 600 tasks;
* N=10 on, 60 reps = 600 tasks;
* N=20 off, 30 reps = 600 tasks;
* N=10 off, 60 reps = 600 tasks;
* N=40 on, 15 reps = 600 tasks (class-A stress; `TasksMax` default 8192).

Acceptance per run: `measured == verified`, `false_pass == 0`,
`failed_reps == []`. **Also inspect `relaunch_retries`**:

* it must be 0 on the normal matrix;
* if it is non-zero, the run is only a pass if the first failure is preserved
  under that rep's evidence and the summary names the retry. This is the
  briefing's bounded-retry rule.

If a new 1/600 class-B failure occurs before the retry, confirm:
`<scratch>/rep-XXX/evidence/.../relaunch-retry-1/relaunch-retry.json` exists and
`<scratch>/sessions/<id>/relaunch-retry.json` exists. If the retry itself
fails, stop and report; do not add more attempts.

### 5.5 Rerun the S3b class-B reproduction test deliberately

The retry has not fired. The clean way to test it without waiting for 1/600 is
to extend the existing `acos-cli` fake-launcher test in
`crates/acos-cli/src/engine/art.rs`:

* write a fake `acos-art` script whose first reset attempt writes a
  `session-exit.json` containing
  `{"code":"GUEST_EXIT_AFTER_RESUME","guest_exit":1,"stdout_tail":["Adding window failed","ipcSetDataReference: unsupported type object (7209057) at offset 16"]}`
  and exits 1; its second attempt starts normally;
* assert `reset()` succeeds, `relaunch_retry()` is `Some`, and
  `<artifacts>/relaunch-retry-1/session-exit.json` is byte-identical to the
  first attempt.
* assert a fake app failure (`Application onCreate threw`) is **not** retried.

Do this before committing the retry. It makes the mitigation testable, which is
what the task brief's "first failure reported" rule needs.

### 5.6 Then write the final deliverables and commit

* `EXPERIMENTS.md` E2.15 (date/commit, hypothesis, method, host load, results,
  false passes, decision; S3b class A is an environment defect, class B is
  host transport; S3a is reason-based unless a real occurrence appears).
* Leaderboard: update “Agent loop N=10 reliability” and add N=20 reliability.
* Backlog S3a/S3b status.
* `results/bench/S3ab.md` final (replace/annotate checkpoint 1).
* Commit only after all gates + final matrix + retry test are green. Do not
  merge/push (orchestrator decision).

---

## 6. Blockers, risks, open questions

1. **S3a is not reproduced.** The fix is a reason-based hardening. A reviewer
   may reject it without an end-to-end S3a occurrence. The strongest options:
   (a) add an off-by-default guest test seam that makes the first traversal
   skip the draw and proves the recovery; (b) keep the C1 timeout evidence and
   wait for a natural occurrence; (c) build a host-side lost-visibility test.
   Do not simply re-run until the rate looks good.
2. **S3b class B is only retried, not root-fixed.** The malformed
   `addToDisplayAsUser` reply is a real transport defect. The retry is allowed
   by the brief only with the first failure reported; it must be tested and
   reviewed. If class B recurs and the retry also fails, the loop still fails.
   The likely area is `frame_from_parcel` / the InputChannel binder-token
   object table in `crates/acos-binder-peer/src/peer.rs`.
3. **`tools/guarded.sh` TasksMax is an environment fix.** It fixes the
   benchmark under the guarded scope, not a product running under a 512-task
   cgroup. If the product must survive such a cgroup, it needs a graceful
   `HOST_THREAD_LIMIT`/retry story, not a bigger benchmark limit.
4. **The retry classification is string-based.** A real app that prints one of
   the host strings in its stderr could be retried once. The unit test covers a
   normal app failure, but the strings are broad (`display event receiver`,
   `adding window failed`). Review whether to restrict further (e.g. require
   `guest_exit==1`, `guest_signal==None`, and the string in the guest logd only).
5. **Final gates/scenarios are not run against the final binary** (§4.4/4.5).
   The last measurement attempt produced no JSON; treat it as missing.
6. **Disk/leaks.** Disk is at 81 % (172 GiB free). `.cache/s3ab` is 7.2 GiB;
   the 27 GiB screens were already deleted. The reproducer now deletes PNGs
   and has SIGTERM cleanup, but that cleanup was added after the last natural
   failure and was not re-validated under a forced timeout. Check `ps` after
   every long run.
7. **Checkpoint 1 is stale** in `results/bench/S3ab.md`: it says the S3a fix
   includes `ADD_FLAG_APP_VISIBLE`; that is no longer true. Do not copy that
   section into the final report without correcting it.
8. **The `out/` and `__pycache__` untracked files** should be removed before
   any commit. `results/bench/S3ab-evidence/` and `tools/bench/s3ab_repro.py`
   are intended to be committed; `out/` is not.
9. **No commits were made.** Everything is reviewable in the worktree. The
   branch is still exactly `6301c06` + working-tree changes.

---

## 7. Context/reasoning not already in `results/bench/S3ab.md`

* The add-flag regression is the single most important “do not repeat” item.
  The T03 baseline differences were:
  * baseline has an EditText caret and grey hint (`focused` look);
  * add-flag actual lacks the caret and uses the darker hint colour.
  Setting `ADD_FLAG_APP_VISIBLE` causes the first traversal to draw before the
  visibility-driven second traversal. The guest-side redelivery was designed
  specifically to happen **after** the first traversal, so it preserves that
  second traversal.
* The class-B failure is **not** the same as H17. H17 was a shared-input race
  (Landlock plan/APK) with guard `ERR`/exit 126 or SIGBUS/exit 135. Here the
  guard is OK, the guest exits 1, and the guest log names an
  `addToDisplayAsUser` reply parse error. It is also not the vsync-thread
  class-A failure: the guest got past `Choreographer` and failed in
  `ViewRootImpl.setView` `addToDisplayAsUser`.
* `0x6e0061` (7209057) is likely two UTF-16 characters from the InputChannel
  name (`"Surface(name=..."`), suggesting the object table points into the name
  string. That is a concrete lead for the peer/object-table bug, not just a
  generic “malformed reply”.
* The `repro-n40-thread-eagain` evidence is from a run with `TasksMax=512`
  **before** the later product changes. The class-A mechanism is independent of
  the S3a/S3b product changes.
* The `results/bench/S3ab.md` checkpoint says N=20/N=10 before/after numbers;
  the valid data files are in `.cache/s3ab` and the p5 direct files, not all in
  one place. The final report should not quote the add-flag `after-*` numbers
  as final reliability evidence.
* The last command attempted after the retry was an N=20 600 run; it returned
  no result. Do not invent a final 600/600. The last valid N=20 is 599/600
  (class B) with the pre-retry binary; the last valid N=10/N=20 off numbers are
  600/600 with the add-flag binary (T03 regressed, so not the final code).
