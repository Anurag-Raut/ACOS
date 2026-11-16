# MPROC5 status (branch `mproc5`, base `mproc4` @ `5efe9869`)

**Step 1 is implemented and unit-tested, and is NOT yet proven on real ART (the
run did not happen before the one-hour checkpoint). Steps 2 (idle kill), 3
(driver death half) and 4 (the scenario) are not started.**

| step | state |
|---|---|
| 1 per-process top-of-task | **code + unit tests done**, ART proof **not run** (see "next action") |
| 2 idle kill | not started (`MPROC2/STATUS.md` §4 design unchanged; the spawn path it needs already exists) |
| 3 driver death half | not started (`F_DEATH` → `BR_DEAD_BINDER`, `BC_DEAD_BINDER_DONE`, `F_RSP` → `BR_DEAD_REPLY`) |
| 4 `scenarios/mproc3-two-process.yaml` | not written (a scenario for steps 1+2 that cannot pass would be a fake spec) |

Commit: `08602a28` (one commit, branch `mproc5`).

## Step 1 — what exists now

### The rule (ATMS, `crates/acos-services/src/atms.rs`)

- `AtmsState::tops: BTreeMap<u32 /*process tag*/, u64 /*activity token*/>` — the
  top-of-task activity of each process. There is one entry per process because
  ACOS schedules one task per process, which is exactly what the rule reads.
- `activity_resumed(token, true)` now calls `top_of_task(token)`: every **other**
  process whose top is still on screen (record `resumed`, or lifecycle
  `Resumed`/`Started`/`Paused`) is sent, in this order,
  1. one `ClientTransaction` whose `mLifecycleStateRequest` is
     `PauseActivityItem(finished=false, …)`,
  2. one `ClientTransaction` whose lifecycle request is
     `StopActivityItem(configChanges=0)`,
  each keyed on **that** process's activity token, and `tops` moves to the new
  token. No local state transition is applied: stock ATMS also waits for the
  app's `activityPaused`/`activityStopped` before the record moves, so the
  existing `activityPaused`/`activityStopped` handlers stay the only writers and
  an out-of-order callback still fails loudly.
- `AtmsState::finish_activity(token)` calls `resume_below_after_finish(token)`:
  the activity below the finished one in `AmsState`'s task stack resumes. When it
  belongs to the **same** process the host does nothing (the guest's own W38/ROT6
  watchers schedule those items); when it belongs to **another** process the host
  queues one `ResumeActivityItem` transaction to that process — the item stock
  ATMS sends, and the only one that can reach another process.
- `AtmsState::pending_lifecycle_calls: Vec<(u32 tag, String method, Vec<Value>)>`
  is drained by `AtmsService::call` and encoded into the **existing**
  `PendingDelivery` transport, addressed at `BinderId(that process's
  IApplicationThread)`. An unknown tag logs a `ActivityManagerLifecycle` warning
  and delivers nothing (never a silent drop, never a fabricated target).

### Wire shape (host-side, from the **image** dex)

`crates/acos-services/src/parcelable.rs` gained two `Verification::Full` specs,
scanned out of `.cache/boot/android-34/dex/framework` (not guessed from AOSP):

- `PauseActivityItem`: `writeBoolean, writeBoolean, writeInt, writeBoolean,
  writeBoolean` (`mFinished, mUserLeaving, mConfigChanges, mDontReport,
  mAutoEnteringPip`);
- `StopActivityItem`: `writeInt` (`mConfigChanges`) — this image's item has no
  `showWindow` on the wire, so the host writes none.

`ClientTransactionItem::to_parcelable` + `ClientTransaction::lifecycle_parcelable`
write the framework's own `ClientTransaction.writeToParcel`
(`mIsGlobal`, `mActivityToken` binder, named lifecycle request, `mIsReady`,
`mActivityCallbacks` = null list) so the guest decodes it with
`ClientTransaction.CREATOR` in its own `ApplicationThread.scheduleTransaction`.
`LaunchActivityItem` stays guest-marshalled (step 3b design, unchanged).

### Served tree/screenshot come from the top window's process

`ProbeState` now keeps `by_process: BTreeMap<u32, ProbeSample>` (one sample per
process tag) plus `top_process: Arc<AtomicU32>` — the atomic in
`LiveHandles::top_process`, published by ATMS (`publish_top`) whenever the top of
task moves. `latest_probe` serves the **top process's** sample, so a `tree` reply
(tap targeting, the window-on-top assertion, and the frame evidence a screenshot
reply names) can no longer come from whichever guest printed last. Before this,
`proc-b`'s probe lines and `proc-a`'s shared one `latest` slot, with unrelated
sequence counters. `latest` is kept as the pre-ATMS fallback (startup).

## Tests (green)

`cargo test -p acos-services -p acos-binder-peer`: **54 test binaries, 0
failures** (evidence: `evidence-step1-tests.txt`).

New: `crates/acos-services/tests/mproc5_top_of_task.rs` (4 rows):
cross-process resume queues Pause then Stop for the losing process (item classes
and `mActivityToken` read back out of the encoded parcelable) and moves `tops` +
the published tag; finishing the new top queues `ResumeActivityItem` for A's own
token; a same-process resume queues nothing; a bare state machine with no
published tag still records the truth.

`crates/acos-services/tests/parcelable.rs` re-verifies every spec against the
image dex, including the two new item specs (`4 passed`).

## Next action (smallest, in order)

1. Build the release/debug `acos-art` and run the existing driver, which now has
   `proc-b-window-on-top`, `proc-a-paused-or-stopped-under-b` and
   `back-from-b-returns-to-a` in its **fatal** set (`tools/mproc4-two-process-drive.py`):
   ```
   tools/art-runtime/run-exclusive.sh --task MPROC5 -- \
     tools/guarded.sh --mem 10G --time 500 -- \
     env ACOS_FONT_DIR=…/android-34/fonts ACOS_ART_DATA=$WT/.cache/mproc5-art-data \
     python3 tools/mproc4-two-process-drive.py <acos-art> \
       fixtures/build/apps/proc-a.apk fixtures/build/apps/proc-b.apk \
       results/handoff/MPROC5/evidence/run1
   ```
   The two risks to watch are (a) A's guest must decode the host-marshalled
   `ClientTransaction` — the item specs are dex-verified but this is the first
   host-marshalled *lifecycle* delivery — and (b) whether the guest's
   `TransactionExecutor` needs `mActivityCallbacks` as an empty list rather than
   the null list the framework's own `obtain` leaves.
2. Only then: step 2 (idle kill), step 3 (driver death half), step 4 (scenario).
