# SYSUI4 — the caller's result delivery: AOSP ordering in the host, and what the guest does with it

Branch `sysui4` (from `sysui3a`, `c2e9c9e9`), 2026-11-17. All ART runs are private
(`ACOS_RUNTIME_DIR=$PWD/.cache/rt-sysui4`, `ACOS_SESSION_SOCKET=$PWD/.cache/sysui4-session.sock`,
`ACOS_VIEW_TEST_DIR=$PWD/.cache/lv`, `ACOS_ART_BIN=$PWD/.cache/sysui4-target/release/acos-art`) and
every one went through `tools/art-runtime/run-exclusive.sh --task SYSUI4` + `tools/guarded.sh`.
`run.sh` in this directory is the exact command set.

| Item | State |
|---|---|
| 1. Offline-first step: boot the real `ActivityThread`, schedule the host's `ActivityResultItem(+ResumeActivityItem)` for a caller in `Resumed`/`Paused`/`Stopped`, read `mActivities`/`Activity.mCalled` | **DONE** — `crates/acos-runtime/tests/activity_result_delivery.rs` |
| 2. The blocking call named | **PAUSED: the delivery works** (`onRequestPermissionsResult` runs on the real framework with the host's exact bytes, token included). `RESUMED`: AOSP's `handleSendResult` pauses first (`performPauseActivity`); `STOPPED`: `TransactionExecutor.executeCallbacks` cycles to the closest pre-execution state first (`handleStartActivity`/`performStart`, AOSP's `getClosestPreExecutionState`). Both named from the real call stack. |
| 3. Host ordering fixed per AOSP `ActivityTaskSupervisor`/`ActivityRecord` | **DONE** in `AtmsState` (withdrawn superseded pause/stop, `notifyAppResumed`, AOSP's "no longer stopping" stop rule) + `crates/acos-services/tests/sysui4_result_ordering.rs` (3 tests) |
| 4. `sysui1-permission-pregranted` | **STILL FAILS** — the host now delivers exactly one `ActivityResultItem + ResumeActivityItem` to the caller after its stop, and the guest still executes it with no callback (below) |
| `cargo test -p acos-services` | **PASS** (incl. the 3 new ordering tests; mproc5 updated for the typed queue) |
| `cargo test -p acos-wm -p acos-runtime` | **PASS** |
| `cargo test -p acos-art` | **162 pass, 7 `live_view` fail** — worktree socket-path length (pre-existing, environmental; see §5) |
| Regressions `sysui1-permission-allow` / `-deny` / `shade`, `notif-perm-granted` / `-denied`, `a11y-probe`, `mproc3-two-process`, `form-validation` | **PASS** (8/8) |
| `gaten-backstack` | **FAILS, pre-existing and unrelated** (`startActivity flags 0x10200000` refused with the shared `gaten-dev-f344272` APK; the flag check is `ams::start_activity`, untouched here) |
| `back-layers` | not a scenario in this branch (`scenarios/` has `gaten-backstack`, `w42-back-frame`, `layer0-overlay-stack`) |

## 1. Offline-first: the real `ActivityThread`, the host's own bytes, three caller states

`crates/acos-runtime/tests/activity_result_delivery.rs` boots the app process with the real
`ActivityThread` (`boot_app_process`, the launcher's own path), installs a real
`ActivityClientRecord` + a real `dev.acos.fixtures.permprobe.MainActivity` in
`ActivityThread.mActivities` (the fields `Activity.attach` would fill: `mBase`, `mMainThread`,
`mInstrumentation`, `mToken`, `mApplication`, `mComponent`, the record's `intent`/`activityInfo`),
then decodes **the host encoder's bytes** with the image's own
`ClientTransaction.CREATOR` and hands the result to the real
`ApplicationThread.scheduleTransaction`. The scan is `acos_services::client_transaction`
(`result_info("@android:requestPermissions:", 7, RESULT_OK, data-with-the-AOSP-extras)` +
`activity_result_item` + `resume_item`, `client_transaction(Some(BinderId(caller)), …)`), i.e. the
exact transaction `AtmsState::deliver_result` sends — **including the strong-binder token**, whose
absence was the SYSUI2/3A suspect. The decoded token is what the record is installed under, so a
dropped/rewritten token cannot be papered over.

The case's proof per state is the framework's own:
`Activity.mHasCurrentPermissionsRequest` is `true` before the delivery (what
`Activity.requestPermissions` sets) and `false` after only if
`dispatchRequestPermissionsResult` ran (i.e. `onRequestPermissionsResult`); the fixture's
`Log.i("AcosPermProbe", "result request=7 …")` line is kept as the secondary signal.

Measured (`.cache/activity-result-delivery.log`):

```
activity_result_delivery: state=resumed   permissions_request_open=true  blocker=…Activity.onSaveInstanceState(2364) / performPauseActivity
activity_result_delivery: state=paused    permissions_request_open=false lines=["result request=7 n=2 m=2 CAMERA=0 LOCATION=0"]
                                          blocker=…MainActivity.onRequestPermissionsResult(98) (the fixture's resultView is null in this harness)
activity_result_delivery: state=stopped   permissions_request_open=true  blocker=…FragmentManagerImpl.ensureExecReady(2008) / performStart
```

So:

* **Paused** (the pre-granted arm's real state at the moment the dialog is destroyed): the host's
  bytes decode to one `ActivityResultItem`, the executor calls `ActivityThread.handleSendResult` →
  `deliverResults` → `Activity.dispatchActivityResult` → `dispatchRequestPermissionsResult` →
  `onRequestPermissionsResult`. The blocking call was **not** in the item, the token or the list.
* **Resumed**: AOSP's `handleSendResult` first pauses the caller
  (`performPauseActivityIfNeeded` → `callActivityOnSaveInstanceState`); the offline harness has no
  real `Window` so that is where it stops — the named call is the answer, not a defect.
* **Stopped**: the executor's own `getClosestPreExecutionState`/`cycleToPath` restarts the caller
  *before* the item (`handleStartActivity` → `performStart`); the harness's manually installed
  `FragmentController` stops there. Also the named call.

## 2. The host ordering fix (AOSP-faithful)

`AtmsState` now models the one `ActivityRecord` rule the pre-granted arm needs:

* `pending_lifecycle_calls` is typed (`PendingLifecycleCall{tag, token, item, method, args}`) instead
  of an opaque tuple, so a queued item can be named.
* `deliver_result` calls `withdraw_pending_pause_stop(tag, caller)` first: AOSP's
  `ActivityRecord.sendResult` stores the result while the caller is not resumed and
  `TaskFragment.resumeTopActivity` resumes the caller **with** it, so a pause/stop the host queued
  (dialog became top) but never sent is superseded and must not pause the caller after its result.
  (`tests/sysui4_result_ordering.rs::a_dialog_that_finishes_in_oncreate_leaves_the_caller_no_stale_pause_or_stop`)
* `deliver_result` then calls `notify_caller_resumed(caller)` — `TaskFragment.resumeTopActivity`
  calls `notifyAppResumed()` before scheduling the result+resume transaction. The host model says
  `Resumed` before the guest's reports arrive.
* `activity_stopped` applies AOSP's `ActivityRecord.activityStopped` rule: a stop for a record that
  is no longer stopping is logged and ignored ("Activity reported stop, but no longer stopping"),
  never `illegal transition Stop from Resumed`. The guest produces that order itself because
  `handleStopActivity` defers its report into a `PendingTransactionActions.StopInfo` runnable, which
  can run after the result's resume
  (`tests/sysui4_result_ordering.rs::a_stop_that_lands_after_the_result_resume_is_ignored_like_aosp`;
  the already-sent case is the third test).
* The encoder is **unchanged** (no byte-level edit in `client_transaction`/`parcelable`).

## 3. What still fails, and the exact next step

`sysui1-permission-pregranted` after the fix (`pregranted-trace/`, `ACOS_ATMS_TRACE=1`):

```
ACOSATMS finish-request token=0x400000077 known=[0x100000003, 0x400000077] n=2
ACOSATMS deliver tag=0 method=scheduleTransaction app_thread=Some(0x100000001)   ← caller pause
ACOSATMS deliver tag=0 method=scheduleTransaction app_thread=Some(0x100000001)   ← caller stop
ACOSATMS result token=0x400000077 result_to=0x100000003 request=7 delivered=false finished=true
ACOSATMS result-route caller=0x100000003 tag=0 attached=[(0, dev.acos.fixtures.permprobe, true), …]
ACOSATMS deliver tag=0 method=scheduleTransaction app_thread=Some(0x100000001)   ← the result transaction
```

The caller's looper (`pregranted-trace/launcher.stderr.log`) runs `Pause` `159`, `Stop` `159`, the
deferred `StopInfo`, then the result `159` — and the last one finishes with **no lifecycle call, no
`onRequestPermissionsResult`, no exception** (`<<<<< Finished to Handler` is printed; a throw would
have killed the looper). `logs/logcat.txt` still ends at `AcosPermProbe: requesting code=7`.

That is *the same transaction shape* the offline case just proved works when it is read by the
image's own reader on the ACOS VM — with the same token, callbacks list and `ResultInfo`. So the
remaining difference is between the offline reader (ACOS VM `Parcel`) and the ART guest's real
`Parcel`/binder path, or the guest-side record state that `handleSendResult` reads. **Next step
(evidence-only, no architecture):** dump the delivered request bytes host-side
(`PendingDelivery` for `android.app.IApplicationThread` code 54) and add one guest mark around the
image's `TransactionExecutor.execute` (a `PendingTransactionActions`/`Looper`-level mark is already
there) reporting `getCallbacks().size()`, `getActivityToken()` identity and
`r.activity.mFinished`/`r.paused` — one ART run then names the exact divergence instead of inferring
it.

## 4. Evidence

| path | what |
|---|---|
| `pregranted/` | the arm after the fix: `verdict.json` (assert_text fail on `result: asking`), logcat, launcher stderr |
| `pregranted-trace/` | the same run with `ACOS_ATMS_TRACE=1`: the host's finish/result/deliver lines above |
| `sysui1-permission-allow/`, `-deny/`, `sysui1-shade/`, `notif-perm-granted/`, `notif-perm-denied/`, `a11y-probe/`, `mproc3-two-process/`, `form-validation/` | regressions, all `pass` |
| `gaten-backstack/` | the pre-existing unrelated failure (`startActivity flags 0x10200000` with the shared gaten APK) |
| `.cache/activity-result-delivery.log`, `.cache/cargo-test-*.log`, `.cache/run-*.log` | the offline case, the crate suites, the run logs (worktree `.cache`, not committed) |

## 5. `cargo test -p acos-art` in this worktree

162 pass; the 7 `live_view::tests::*` failures are all
`stream socket path …/.cache/live-view-test/…/stream.sock is too long for a unix socket` — the
worktree path is longer than a unix socket allows (108 bytes). Pre-existing and environmental (the
same failures on `sysui1`/`sysui2`), not a code failure.
