# MPROC5c status (branch `mproc5c`, base `mproc5b` @ `f229ae3f`)

**Done: the window-session root cause of MPROC5b is fixed the AOSP way (one
`IWindowSession` node **per `openSession` call**), the step-1 gates are green on
real ART, the idle kill (step 2) is implemented and proved on real ART, and
`scenarios/mproc3-two-process.yaml` (`engine: art`) exists for steps 1+2.**

| step | state |
|---|---|
| 1 per-process top-of-task on ART | **green** — `proc-b-window-on-top`, `proc-a-paused-or-stopped-under-b`, `back-from-b-returns-to-a` PASS; `proc-a-onActivityResult-from-b` stays the MPROC7 hold |
| 2 idle kill | **done + proved on real ART** (kill record, `ProcessRecord` removed, session closed, respawn with a new pid) |
| 3 `scenarios/mproc3-two-process.yaml` (`engine: art`) | written; parses + validates (`cargo test -p acos-protocol`) |
| MPROC5b §5 decisions | both applied: AIDL `activityResumed` fix kept; window sessions done **without** caller identity |

## 1 — The window-session fix (AOSP `new Session(this, callback)`)

MPROC5b measured `unknown session 2` on proc-a's `relayout` after proc-b's
`openSession` overwrote the single session id. The fix is AOSP's shape, not a
workaround:

* `ServiceHost` gains `NodeRegistrar` (`host.rs`): the handle a service uses to
  create a **new host node mid-transaction** (stock does exactly this inside
  `IWindowManager.openSession`). The node is installed by
  `ServiceHost::install_pending_nodes()` before the reply reaches the guest (the
  peer calls it after `direct_dispatch`, which bypasses `transact`'s drain).
  `binder_owners` became `Arc<Mutex<..>>` so the registrar can answer "which
  app process handed this `IWindow` over" (stock `Session.mClientPid`).
* `WmsState` (`wms.rs`) keeps `sessions: HashMap<SessionId, SessionEntry>` —
  `{id, binder, process}` — **never a single "last session"**. Every operation
  that was implicitly `self.session` now has an explicit `*_for(session, …)`
  variant, and the host node always passes **its own** session:
  `relayout`, `finishDrawing`, `remove`, `addToDisplayAsUser`, `setInsets`,
  `updateRequestedVisibleTypes`, `reportSystemGestureExclusionChanged`,
  `pokeDrawLock`, `onRectangleOnScreenRequested`. A call for another session's
  window is `UnknownSession` (the image's `IllegalArgumentException`), because
  it arrived on the wrong node.
* `WindowSessionService` carries its `SessionId`; `SessionFactory` creates the
  session + node (registry installs the factory: `IWindowId`, compositor and the
  input producer are wired into it).
* Session death: `WmsState::close_session` / `close_process_sessions` remove the
  session's windows, destroy their compositor layers (W42) and drop the input
  windows; `Peer::disconnect_process` closes the dead process's sessions (its
  recorded tag, or a window binder it owned), unregisters the host nodes and
  drops the `ProcessRecord` (`AtmsState::process_died`). MPROC5b's death half
  then delivers `F_DEATH` to holders as before.
* No caller identity was added to `ServiceObject` (that stays the queued
  BINDERPID track). The process tag of a session comes from the host's own
  binder-owner record of the `IWindow` binder, never from a guessed pid.

### Evidence

`results/handoff/MPROC5c/evidence/run1/driver-stdout.log` (real ART, release
`acos-art`):

```
PASS proc-b-window-on-top: {"probe_seq": 6}
PASS proc-b-resumed-in-its-own-process: {"lines": ["ACOSPROBE b created …", "ACOSPROBE b resumed pkg=dev.acos.fixtures.procb"]}
PASS proc-a-paused-or-stopped-under-b: {"lines": ["ACOSPROBE a ready …", "ACOSPROBE a resumed", "ACOSPROBE a start-b …", "ACOSPROBE a paused", "ACOSPROBE a stopped"]}
PASS back-from-b-returns-to-a: {"probe_seq": 41}
FAILURES_3B []
OPEN_3C ["proc-a-onActivityResult-from-b", "aidl-round-trip-through-foreign-node", "transaction-ran-in-proc-b"]
```

`run0` (same driver, before the `previous_top` fix described below) shows the
session crash of MPROC5b is gone and only the return-to-A path is missing; the
`unknown session 2` error no longer appears in `unsupported.jsonl` (its 4
remaining records are the pre-existing `reportSizeConfigurations` /
`activityStopped` Bundle gaps).

**One more root cause was found on the way to `back-from-b-returns-to-a`:**
`AtmsState::resume_below_after_finish` looked for the activity below only in
`ams.tasks`, and the **launched primary** Activity is not in that table (it was
never created by a host-recorded `startActivity`), so a cross-process finish had
nothing to resume and the top process stayed the finished one. `AtmsState` now
records `previous_top` (the activity that just lost the top) and resumes it when
the task table has no answer — what stock `ActivityTaskSupervisor` does.

## 2 — Idle kill

`acos-art launch --idle-kill-seconds N` (default 30, `0` disables;
`ACOSLAUNCH_IDLE_KILL_SECS` is the env/plan spelling):

* `AtmsState::idle_processes(idle)` — the rule: a **lazily spawned** secondary
  (`tag != 0`) with no started/resumed Activity of its own and no started or
  bound service of its package, idle for `idle` since its last foreground
  transition (`last_active`, refreshed on spawn, on the top-of-task winner *and*
  the displaced top, and on a cross-process finish). `next_idle_deadline` gives
  the launcher an exact wake time (no polling timer). The "no visible window"
  half is `WmsState::process_has_visible_window`.
* `SecondaryPool::idle_kill` kills the process **group**, records
  `{tag, package, pid, reason: "idle", idle_ms, idle_kill_secs, window_sessions}`
  in the `processes` control module (`killed`), and forgets the process, so the
  next intent's spawn is a real spawn.
* The guest's connection close runs `Peer::disconnect_process` → binder records
  dropped, `F_DEATH`/`BR_DEAD_BINDER` to holders (MPROC5b death half),
  `IWindowSession` node unregistered + its windows/layers dropped, and the
  `ProcessRecord` removed.

### Evidence

`results/handoff/MPROC5c/evidence/idle0/` (`tools/mproc5c-idle-kill-drive.py`,
`--idle-kill-seconds 3`, real ART):

```
PASS idle-kill-recorded: {"idle_kill_secs": 3, "idle_ms": 3000, "package": "dev.acos.fixtures.procb", "pid": 2837110, "reason": "idle", "tag": 1, "window_sessions": 1}
PASS idle-kill-after-the-timeout: {"pid": 2837110, "first_pid": 2837110, "idle_ms": 3000}
PASS process-record-removed: {"processes": []}
PASS killed-process-owned-a-window-session: {"window_sessions": 1}
PASS session-window-closed-on-death: {"sessions": ["2"]}
PASS next-intent-respawns-the-process: {"pid": 2837742, "first_pid": 2837110}
PASS spawn-recorded-twice: {… two spawns with pid 2837110 then 2837742 …}
PASS respawned-window-on-top: {"probe_seq": 8}
PASS proc-b-logged-created-in-a-new-process: {"lines": ["ACOSPROBE b created …", "ACOSPROBE b created …"]}
FAILURES []
```

## 3 — Scenario

`scenarios/mproc3-two-process.yaml`: `app: proc-a.apk`, `extra_apks:
[proc-b.apk]`, `engine: art` (the protocol `Engine` enum gained `Art`; the JSON
schema was refreshed). Steps: Start B → "Finish B" is the served tree (proc-b's
window on top) → B resumed / A paused / A stopped in that order → Finish B →
"Start B" is served again (back to A) → Start B again after the idle kill →
"Finish B" again + B's `created` line twice (a new process instance). The
finished `onActivityResult` row (MPROC7) is deliberately **not** asserted. The
kill record itself has no scenario step kind (no `ctl` step), so the driver
above is the evidence for it — the same split MPROC1 used for its `ctl` facts.

## Tests

`results/handoff/MPROC5c/evidence-tests.txt` — see the header of that file for
the exact commands. New/updated tests:

* `crates/acos-services/src/wms.rs`: `a_second_session_cannot_relayout_the_first_ones_window`
  (MPROC5b's failure as a test, including `remove`/`finishDrawing`),
  `closing_a_session_drops_its_windows_and_keeps_the_others`,
  `open_session_returns_a_fresh_session_binder` (both sessions stay open).
* `crates/acos-services/tests/window_session_api34.rs`:
  `open_session_answers_a_routable_iwindow_session_node` now asserts **distinct**
  per-call nodes (two live host objects), and `a_relayout_on_the_wrong_session_node_is_refused`
  is the wire-level proof of the refusal.
* `crates/acos-services/tests/mproc5c_idle_kill.rs` (new): the idle rule
  (nothing-started → candidate after the timeout, tag 0 never a candidate, a
  started Activity / a bound service keeps the process, the foreground
  transition restarts the clock and yields a wake deadline, `process_died`
  drops the record and its activities).

## Remaining / next

* `proc-a-onActivityResult-from-b` is still red (MPROC7 hold, as decided).
* `activityStopped`'s saved-state Bundle carries `VAL_LIST` (tag 12), which the
  Bundle reader does not model (`unsupported.jsonl`), and B's stop report then
  arrives after its destroy (`illegal transition Stop from Destroyed`). Both are
  pre-existing gaps in the activity-lifecycle parcel path, not in the window
  session; they do not block steps 1–2 but they are the next fidelity item for
  the cross-process lifecycle.
* `acos-art`'s `tests/launch_hang.rs::ordinary_launch_to_application_created_stays_fast`
  is red in this worktree — **and equally red on the base commit `f229ae3f`**
  (guest exit 1 on a plain `application-created` launch; proof appended to
  `evidence-tests.txt`). Nothing in this branch runs on that path, so it is an
  environment/base failure, not a MPROC5c regression.
