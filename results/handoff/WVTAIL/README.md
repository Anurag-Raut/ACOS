# WVTAIL — the tap tail, and the `NO_VIEW` that was an engine restart

Branch `wvtail`, from `wvtap` (`0faabef0`). Private `ACOS_RUNTIME_DIR` +
`ACOS_SESSION_SOCKET`, `ACOS_VIEW_TEST_DIR=$PWD/.cache/lv`, every ART run through
`tools/art-runtime/run-exclusive.sh --task WVTAIL` + `tools/guarded.sh`, builds in
`.cache/wvtail-target` (`*_DEBUG=0`, incremental off), fixtures built in the
worktree (`tools/build-apps.sh`, `tools/build-dogfood-apps.sh`).

## Status

| item | state |
|---|---|
| 1. `webview-fair` tap → click **all under 1 s** | **PASS 20/20**, worst **529 ms** (§3.1). Root cause found and fixed: the input-ack wait, not the input frame (§1) |
| 1b. pending input preempts non-input attempts | **done** (§2), 4 gate tests pin it |
| 1c. an abandoned frame attempt leaves the engine consistent | **done** (§2.3): the abandoned `beginFrame` is drained, never overlapped; `Another frame is pending` is a transient, not a stale frame |
| 1d. the slow page still gets frames | **PASS**: the scenario now asserts a *later* frame of the slow view through the tap (`since_previous_step`), 20/20; 3.2 |
| 2. broker session view-map loss (`NO_VIEW: no view 2` + a 30 s unanswered render) | **cause identified and answered honestly** (§4): it is an **engine restart**, not a replaced `Session` and not a second `hello`; the broker now says so (`ENGINE_RESTARTED`), logs it with the exit status and stderr tail, and 60+ runs show no view-map loss. The remaining item is a process-model decision for the orchestrator (§4.4) |
| 3. `cargo test -p acos-art -p acos-webview -p acos-services` | **green** (§5) |
| 4. `webview-probe` 20×, `a11y-probe` | **PASS** (§3.2, §3.3) |

## 1. The tail was the *ack*, not the input frame

WVFAIR §7.1 and WVTAP §3.1 attributed the 2.0–2.6 s tap to "an input frame waits
behind a 1.9 s non-input engine frame slice". Instrumenting the gate and the
input path (temporary traces, the numbers below are from them) showed something
different, and it is the whole of the tail:

```
[gate]  view=9  prio=input  granted_after=0ms    attempt=1ms   budget=2s -> frame
[input] view=9  touch(touchStart): sent  at 0ms
[input] view=9  touch(touchStart): frame at 1ms      <- the gesture's own frame: 1 ms
[input] view=9  touch(touchStart): ack  Err(Timeout(2s, "request 720")) at 2001ms
[op]    wv-worker-1 id=g38 op=input.touch done in 2001ms
[input] view=9  touch(touchEnd):   frame at 1ms / ack Ok at 1ms
```

(The `[input]` lines came from a temporary trace of the input path in the broker;
`evidence/before/host-test-gate-trace.log` keeps the gate half of the same
measurement, with the old gate's 333 ms input wait and 936 ms `maxWaitMs`.)

On ART the guest's `input.touch` blocked **2001 ms** while the engine answered in
1 ms; a tap is two of those, hence 2004 ms, 2519 ms, 2604 ms, and the WIP's
5405 ms (both phases, 2 s each). The pre-fix ART numbers on this host and
scenario are the committed ones from WVTAP — `results/handoff/WVTAP/evidence/fair10b`:
**2619, 2525, 2519, 2004 ms** in 4 of 10 runs (the same scenario, before this
change; the trace itself was a temporary instrument and is not kept).

**The rule** (measured, `evidence/…/run6.gate.log` of the fix rounds):

> Under `--enable-begin-frame-control` the renderer routes a dispatched input
> event **in** a compositor frame and acks it at the end of the frame that
> **follows** that one.

So the ack the broker waited for could only be produced by the guest's *next*
frame — and the guest cannot produce it: it sits inside `dispatchTouchEvent`
waiting for the ack. A self-deadlock with a 2 s timeout at the end of it. When
the provider happened to have pulled a frame in between, the ack arrived inline
and the tap was 2–8 ms; that is the whole bimodality of the distribution.

### 1.1 The fix

`crates/acos-webview/src/view.rs` — `View::wait_input_ack`: wait 150 ms for the
ack (the normal case answers inline), and if it has not arrived, **produce the
following frame** (input priority, so it is granted ahead of every guest pull)
and ask again, up to `ACK_FRAMES = 2`. A missing ack is now a fact reported to
the guest (`"acked": false`, plus `frameMs`/`ackMs`) and a line in the broker's
log (`input touch touchStart on view N NOT acked after Nms`), never a silently
lost tap. `tap()` collects both acks at the end (its `up` frame *is* the `down`
event's following frame), so the `input.tap` path costs no extra frame at all.

Measured after the change: the tap's two input frames are 1–4 ms, and every tap
in the 20-run batch is acked inline except two (both from the scenario's
synthetic touch on the *slow* view, §3.2).

## 2. The gate: pending input preempts a non-input attempt

Even with the ack fixed, an input frame could still wait one whole fairness
budget (750 ms) for the attempt that was already running. `crates/acos-webview/src/fair.rs`:

* **`AttemptCtx`** carries the attempt's budget, whether it is the last one, the
  wait slice, and a `preempted` flag. `View::begin_frame` awaits the
  `beginFrame` reply **in slices** (`wait_slice`, 20 ms) and checks the flag.
* **`Attempt::Yield`** — when a queued **input** pull needs the engine, the
  running non-input attempt abandons at its next slice and re-enters the queue
  at the back. A yield spends **no attempt** of that pull's budget (the frame was
  never finished), so a slow page whose frames keep being preempted still gets
  its three attempts and still produces a frame.
* **input priority survives the join** — an input pull that joins an
  outstanding guest pull (`Slot::promoted`) promotes it, so the frame the gesture
  needs keeps input priority when it re-queues instead of losing its place to the
  guest pulls queued after it.
* **measured**: `inputMaxWaitMs` (worst input wait) and `preemptions` in
  `engineStatus.frames`; the host-level test reports `inputMaxWaitMs 37–79 ms`
  (`evidence/host-fair/fair-pulls.json`) where the pre-change gate measured
  333–750 ms of dead wait (`evidence/before/…`).

### 2.1 Tests (no engine, the gate's own scheduling)

`crates/acos-webview/src/fair.rs`, `mod tests`:

| test | pins |
|---|---|
| `a_queued_input_frame_preempts_the_running_non_input_attempt` | the input is granted in ~one slice (`<= 100 ms`), not after the 750 ms budget; `preemptions >= 1`; the preempted pull still gets its frame |
| `a_preempted_attempt_does_not_spend_the_pulls_attempt_budget` | 4 preemptions + 1 frame ≠ 4 spent attempts; no timeout, no slot left behind |
| `an_input_join_promotes_the_outstanding_pull_to_input_priority` | one engine attempt (`coalesced == 1`), both callers get the same frame, the promoted frame is granted as input |
| `a_pull_that_never_gets_a_frame_fails_and_leaves_no_slot` | `FRAME_FAILED` after 3 attempts, never invented pixels |

### 2.3 An abandoned attempt must leave the engine consistent

`beginFrame` has no cancel: when the gate abandons an attempt at its budget, the
engine still finishes that frame. Issuing the next `beginFrame` for the same
page while the abandoned one is outstanding is what the pinned engine reports as

```
ERROR:content/gpu/gpu_child_thread.cc:70] Mojo error in GPU process:
    Received bad user message: Got overlapping IssueExternalBeginFrame
ERROR:content/browser/gpu/gpu_process_host.cc:1042] GPU process exited unexpectedly: exit_code=133
```

and then the browser dies (measured before this change, 3/6 runs of
`webview-fair`; the log is preserved in the crash evidence below). Two fixes:

* `View::pending_frame` — the reply of an abandoned attempt is kept, and the
  **next** attempt for that view drains it before issuing anything: a guest pull
  keeps those pixels (they are real and newer than the surface's last one), a
  gesture does not (its frame has to be the one produced after its event was
  dispatched, or the engine routes nothing — `View::begin_frame` takes the pull's
  `prio` for exactly this). A second `beginFrame` for one page is now impossible
  by construction.
* `Another frame is pending` (and `Not attached to an active page`) are
  **transient**: `View::engine_error` re-queues them instead of answering with
  the previous frame as if it were this pull's.

After the change the *browser* is never killed mid-run: in the 40 acceptance
runs the only engine death is the harness teardown (§4.3). The GPU-process fault
itself is **not gone**: 7 of the 40 runs' engine stderr still shows

```
Mojo error in GPU process: … Got overlapping IssueExternalBeginFrame
GPU process exited unexpectedly: exit_code=133   (then: reinitialized)
```

while every run passed, no guest op failed, and no `NO_VIEW` appears in any guest
log — the engine restarts its GPU process and the retry/`pending_frame` handling
absorbs the failed readbacks. See §7.2: the remaining candidate is an attempt
whose *own* reply said `no screenshotData (hasDamage=true)` (its readback may
still be in flight, which `pending_frame` does not cover, because we did get an
answer).

## 3. Acceptance runs

`run-runs.sh <scenario> <count> <tag>`: one ART acquisition per run, **one broker
per run** (endpoint and log under `.cache/rt-wvtail/<tag>-<n>/`), so a broker
line in the evidence belongs to that run and a crash cannot be inherited.
`ACOS_WEBVIEW_SOCKET_DIR` is what makes the broker *this branch's* binary: the
host-wide `/run/user/1000/acos-wv/broker.sock` was held by another agent's
leftover daemon built from the `wvtap` tree, which would have measured the old
code.

### 3.1 `webview-fair` × 20 — **20/20 pass, worst tap 529 ms**

`evidence/fair20/run1..20` (verdicts, steps, logcat, broker log per run):

```
tap -> page click: 442 4 367 512 2 508 359 510 358 365 512 2 3 2 3 529 363 3 366 2 ms
                    (max 529 ms, 9 of 20 under 10 ms, none over the 1 s bound)
slow view frames per run: 3 4 4 4 3 2 4 3 4 2 3 3 3 4 3 3 3 3 3 4
```

The scenario's own bound was tightened from `< 3000 ms` to `< 1000 ms`
(`[0-9]{1,3}`), and the two numbers a `webview-fair` run reports are now
enforced where they were only measured before.

### 3.2 The slow page still gets frames — asserted, not assumed

The probe touches the slow view once after measuring the tap
(`fair: slow view touched for a frame`). That dispatches guest input to view 2,
the provider invalidates and pulls view 2's next frame, and the scenario asserts
it **with `since_previous_step: true`**:

```yaml
- assert_log:
    tag: ACOSWvProvider
    patterns: ['frame decoded view=2 [0-9]+x[0-9]+ seq=[2-9][0-9]* bytes=[0-9]+']
    since_previous_step: true      # a frame from B's load cannot satisfy it
    timeout_ms: 30000
```

20/20 pass. The two `NOT acked` broker lines per some runs belong to that
synthetic touch on the *slow* page (a 1.9 s-sliced main thread never acks inside
3 × 150 ms), not to the measured tap — the measured tap is fully acked in every
run.

### 3.3 `webview-probe` × 20 — **20/20 pass**, `a11y-probe` 2/2 pass

`evidence/probe20/run1..20`, `evidence/regress/run1..2`. No `NO_VIEW`, no
`render failed`, no `ENGINE_RESTARTED` in any guest log of the 42 runs
(`grep -c` = 0 across `evidence/*20`).

### 3.4 Host-level fairness test (real engine)

`evidence/host-fair/fair-pulls.json` — `cargo test -p acos-webview --test
fair_pulls`: all four arms pass (`two_sessions`, `one_session`,
`coalesced_burst`, `worker_pool`), `inputMaxWaitMs` 37–79 ms, engine restarts 0.

## 4. Task 2: the session's view map

### 4.1 What it is **not**

* **Not a replaced `Session`.** `Broker::open_session` returns the *same*
  `Arc<Session>` for a repeated `hello` while `session.generation ==
  browser.generation`; only a generation change replaces it (and by then every
  target of that session is already dead).
* **Not a second `hello` resetting the map.** The provider hellos once per
  socket (`AcosSessionSocket`), the per-session socket accepts one connection,
  and 42 instrumented runs show a single `hello` per session.
* **Not reproducible on this tree at all**: 42 runs, 0 `NO_VIEW`.

### 4.2 What it is

`Conn::on_engine_restart` (the watcher saw the engine dead and respawned it)
**clears the session's view map** — correctly, because every target died with
the engine — and then answered every later op with `NO_VIEW: no view <id>`,
which says "that id was never yours". In a guest log that reads as a broker
bookkeeping bug, which is exactly how MERGE040A §5.2 filed it. The new log line
names the fact, with the cause:

```
acos-webview: engine pid=552214 generation=1 died (watcher restart):
              exit status signal: 9 (SIGKILL); stderr tail (last 10 line(s)): …
acos-webview: session wv: engine restarted (restart 1, generation 1),
              dropped 2 view(s): every target died with the engine
```

and the miss answer is now the honest one:

| situation | before | now |
|---|---|---|
| id never existed | `NO_VIEW` | `NO_VIEW` (unchanged) |
| the engine restarted under the session | `NO_VIEW: no view 2` | `ENGINE_RESTARTED: view 2 is gone: the engine restarted (generation 1) and every target died with it; re-create the view (the broker sent engine.restarted)` |

Pinned by `crates/acos-webview/src/broker.rs`, `mod tests` (4 tests:
`a_miss_before_any_restart_is_an_unknown_view`,
`a_miss_after_a_restart_names_the_restart_not_the_wrong_id`,
`a_restart_empties_the_view_map_and_closes_the_views`,
`a_session_is_only_replaced_when_the_engine_generation_changed`).

### 4.3 Where the restarts in my runs come from (and why it matters)

Every restart in the 42 runs is a **teardown** kill: the last broker lines of a
run are the engine's `SIGKILL` with nothing but startup noise in its stderr
(`evidence/probe20/run4.broker.log`). `guarded.sh` runs the session in a
systemd scope; the WebView daemon is `setsid`, which changes the session but not
the **cgroup**, so when the run's scope ends the kernel/systemd kills the broker's
engine too.

That is harmless for a run that is finishing — and it is a mechanism nobody could
see before `report_dead_engine`. It is *not* harmless for the production shape
(one broker per **host**, `/run/user/1000/acos-wv/broker.sock`): one session's
scope ending kills the engine for every other session of the host, which is
exactly `NO_VIEW: no view <id>` plus one in-flight op that never answers (its
target died mid-frame) in another agent's session. **This is my best
explanation for MERGE040A §5.2**, and it fits every symptom: `render` unanswered
for its full timeout, a second view's frame still decoded, and `NO_VIEW` for the
rest of the run.

### 4.4 What the orchestrator must decide

Moving the broker out of its spawner's cgroup (`systemd-run --scope` of its own,
or `Delegate=yes` on the session scope) is a **process-model change** — an
architectural wall for me. Two options:

1. spawn the shared daemon in its own transient scope (`systemd-run --user
   --scope --unit acos-webview-<...>` from `gate.rs`), so a session's teardown
   cannot take the host's engine down; keel: the daemon then needs its own
   lifetime rule (it has one: `--idle-shutdown-secs`);
2. keep it as is and require every *test* harness to use a private endpoint
   (`ACOS_WEBVIEW_SOCKET_DIR`, what this track's runs do), accepting that a
   shared broker is a shared fate.

Nothing else about the session/gen bookkeeping needs to change: with the restart
reported as a restart, a guest that follows the protocol (`engine.restarted` →
re-create its views) recovers.

## 5. `cargo test -p acos-art -p acos-webview -p acos-services`

`.cache/logs/test-art-services.log`, `-p acos-webview` run separately:

```
acos-webview: lib 25 passed (incl. 4 gate + 4 session tests); every integration
              target green; the engine-dependent ones skip without ACOS_WEBVIEW_TEST=1
acos-art:     all targets green
acos-services: all targets green; `--test apk_facts_cache` needs the dogfood APKs
              (`tools/build-dogfood-apps.sh`) and passes after building them —
              measured, and the same caveat WVTAP recorded
fair_pulls:   4 arms pass against the real engine (with ACOS_WEBVIEW_TEST=1)
```

No test was weakened, ignored or removed. The one assertion that changed,
`scenarios/webview-fair.yaml`'s tap bound, was made **stricter** (3 s → 1 s).

## 6. Files

* `crates/acos-webview/src/view.rs` — `InputAcks`, `wait_input_ack`, `ACK_WAIT`
  /`ACK_FRAMES`/`ACK_SLOW`, `begin_frame` await-in-slices + `pending_frame`
  drain, `frame_from_begin_frame`, `await_reply`/`Reply`, `engine_error`/
  `is_transient_engine_error`.
* `crates/acos-webview/src/fair.rs` — `AttemptCtx`, `Attempt::Yield`, the
  preempt flag, `Slot::promoted`, `take_engine_from_running`, `preempt_slice`,
  counters (`preemptions`, `inputMaxWaitMs`, `inputWaiters`), `mod tests`.
* `crates/acos-webview/src/cdp.rs` — `PendingReply::wait_shared`,
  `CdpClient::exit_status`.
* `crates/acos-webview/src/broker.rs` — `ViewMiss`, `Session::miss`/`drop_views`,
  `restarted_generation`, `report_dead_engine`, `mod tests`.
* `crates/acos-webview/src/server.rs` — the restart line and `drop_views`, the
  honest miss in `Conn::view`, `SLOW_OP_MS` slow-op line, `acked`/`frameMs`/
  `ackMs` in the `input.*` replies.
* `crates/acos-webview/src/wire.rs` — `code::ENGINE_RESTARTED`.
* `scenarios/webview-fair.yaml` — 1 s tap bound, the slow-view frame assertion.
* `fixtures/apps/webview-probe/.../MainActivity.java` — the post-measurement
  touch on the slow view.
* `results/handoff/WVTAIL/{README.md,run-runs.sh,loop-fair.sh,evidence/}`.

## 7. What remains

1. **The broker's cgroup (§4.4)** — one decision, then a ~20-line change in
   `gate.rs`'s `spawn_broker`.
2. **The pinned engine still faults once in a while** under two busy pages
   (`Got overlapping IssueExternalBeginFrame`, GPU `exit_code=133`, 7 of 40
   runs; the GPU process restarts itself and no run failed). It also happened
   with attempts never abandoned (1 in 5, measured), so abandonment alone is not
   the trigger. The remaining candidate is the one path that still puts a frame
   into the compositor after we got an answer: `beginFrame` replying
   `no screenshotData (hasDamage=true)` — its readback may still be in flight.
   Covering it means treating "damage, no screenshot" as a *pending* frame
   (wait for the next reply instead of retrying); that changes the retry policy
   WVFAIR measured, so it needs its own A/B. Not needed for any acceptance
   criterion here; `report_dead_engine` now leaves the evidence to ask it.
3. `input.key`/`input.scroll` do not wait for acks at all (their `call` answers
   inline); if a future engine acks keys on a frame, they will need the same
   `wait_input_ack` treatment.

## 8. Evidence map and cleanup

```
results/handoff/WVTAIL/
  README.md                  this file
  run-runs.sh                N runs of one scenario, own broker per run
  loop-fair.sh               webview-fair × N against one broker with a slice of
                             its log per run (the first measurement loop)
  evidence/before/           the host-level gate trace of the pre-change gate
                             (333 ms input wait, 936 ms maxWaitMs)
  evidence/fair20/           webview-fair 20/20 (+ the tap numbers above)
  evidence/probe20/          webview-probe 20/20
  evidence/regress/          a11y-probe 2/2
  evidence/host-fair/        fair_pulls.json (4 arms, inputMaxWaitMs 37-79 ms)
```

Frame PNGs and the per-run `frames/` trees were pruned after the fact (as
MERGE040A and WVTAP did); verdicts, steps, logcat, launcher stderr and the
per-run broker log are kept — they are what every claim above is read from.
Peak RAM ~14 GiB of 60 (one ART run + one release build); disk 79 → 81 % of `/`.
`.cache/wvtail-target`, `.cache/rt-wvtail`, `.cache/lv` and the `fixtures/build`
copy are deleted at the end, and the brokers this track started are stopped.
