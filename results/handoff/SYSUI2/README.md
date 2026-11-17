# SYSUI2 — finishing SYSUI1: the pre-granted launch, the notification shade, real permission labels

Branch `sysui2` (from `sysui1`, `f0f19b1c`), 2026-11-17. All ART runs are private
(`ACOS_RUNTIME_DIR=$PWD/.cache/rt-sysui2`, `ACOS_SESSION_SOCKET=$PWD/.cache/sysui2-session.sock`,
`ACOS_VIEW_TEST_DIR=$PWD/.cache/lv`) and every one went through
`tools/art-runtime/run-exclusive.sh --task SYSUI2` + `tools/guarded.sh`.

| Item | State |
|---|---|
| 1. Shade (`fixtures/apps/systemui` `ShadeActivity`, `open_shade` op, `system_apps`, host result sink, `scenarios/sysui1-shade.yaml` on ART) | **9 of 10 steps pass**; the row tap fires the notification's real content sender, but the target start is refused (below) and the tap's strict settle sees the window detach |
| 2. Pre-granted short-circuit (`sysui1-permission-pregranted`) | **Still failing**, but three real defects on the path are fixed and the arm now reaches the result delivery; the caller never runs `onRequestPermissionsResult` (below) |
| 3. Real permission labels (`IPermissionManager.getPermissionInfo` + groups) | **NOT STARTED** |
| `cargo test -p acos-services -p acos-wm` | **PASS** (160 + all wm suites) |
| `cargo test -p acos-art` | **163 pass, 6 `live_view` tests fail for a worktree path reason, not a code reason** (§5) |
| Regressions `sysui1-permission-allow` / `-deny`, `notif-perm-granted` / `-denied` | **PASS** (4/4) |
| Regressions `a11y-probe`, `mproc3-two-process` | run in `.cache/batch2.log` (APKs were missing on the first attempt) |

## 1. The pre-granted arm: what actually races (finding + three fixes)

SYSUI1 reported "the guest `wait-for-host-transaction` guard times out although the host encoded
and sent the `ClientTransaction`", and guessed an ordering race. It is **not** an ordering race:
the host's transaction ran, the controller answered, and the controller's own process simply never
told anyone, because of three separate defects.

### 1a. The launch milestone was `mResumed`, which AOSP never sets for a finishing Activity

`ActivityThread.handleResumeActivity` **skips** `performResumeActivity` when the Activity is already
`mFinished` ("Skipping resume of … activity is finishing", `ActivityThread.java:5290`). A
`GrantPermissionsActivity` that finds nothing to ask answers in `onCreate` and finishes, so
`mResumed` is never true. The guard polled `mResumed` and therefore timed out on a *successful*
launch, then `Runtime.halt(0)`ed the process — which is why the caller never got its result.

Fixed in the shared launch path (`LaunchMain.java`): the host-scheduled branch now waits for
`launchedActivity(thread)` — an `ActivityClientRecord` whose `Activity.mCalled` is set, i.e. the
record the host's `LaunchActivityItem` created and `onCreate`/`onStart` completed — and then:
resumed → the existing path; **created and finished during onCreate** → publish
`activity-resumed.marker` with `finish_during_create=true` (honest: `resumed=false`,
`resume_skipped=finishing-in-onCreate`) and keep the real main looper alive instead of halting.
Evidence: `pregranted/art-frames/process-1/stdout.log` (`ACOSLAUNCH MARK activity-resumed … finish_during_create=true`).

### 1b. The stop report's saved-instance-state Bundle had no codec

With the process alive, the framework's `PendingTransactionActions$StopInfo` reported
`activityStopped(token, Bundle, …)` and the host refused it (`no parcelable codec for
android.view.AbsSavedState$1`, then `…Toolbar$SavedState`, then `…PhoneWindow$PanelFeatureState$SavedState`).
The saved-instance-state bundle is a real `View` hierarchy state, so the codecs were added from the
image's own dex (`examples/scan_parcelables.rs`), all in `crates/acos-services/src/parcelable.rs`:

* `android.view.AbsSavedState` + `…$1` — one `writeParcelable` (the `EMPTY_STATE` singleton),
* `android.view.View$BaseSavedState` — that plus `mSavedData`, `mStartActivityRequestWho`,
  `mIsAutofilled`, `mHideHighlight`, `mAutofillViewId`,
* `android.widget.{Toolbar,ProgressBar,ScrollView,HorizontalScrollView,AdapterViewAnimator}$SavedState`,
  `AbsSpinner`, `CompoundButton`, `CheckedTextView`, `SearchView`, `TextView`,
* `com.android.internal.policy.PhoneWindow$PanelFeatureState$SavedState` (`PREFIX`: its bundle is
  written only when the panel is open, a branch on a field already written),
* a new `FieldOp::SizedBlob` for `ParcelableParcel` (`int size` + verbatim bytes, `TextView.SavedState.editorState`).

The declared-length check now names the class whose table is short (`bad reply for parcelable::field:
<class>: VAL_4 body consumed N bytes but declared M`), so the next gap is one log line away.

### 1c. `Stopped -> Resume` was an illegal lifecycle transition

The finished dialog's process reports `activityDestroyed`; the host then delivers the caller's
`ActivityResultItem` + `ResumeActivityItem`. The caller was paused **and stopped** while the dialog
was the top, so the guest's `activityResumed` was refused (`illegal transition Resume from Stopped`).
AOSP restarts a stopped Activity on resume (`performRestartActivity` → `onStart` → `onResume`), so
`(Stopped, Start)` and `(Stopped, Resume)` are now legal (`crates/acos-services/src/atms.rs`); test
`crates/acos-services/tests/lifecycle.rs::a_stopped_activity_can_be_started_and_resumed_again`
fails before and passes after.

### 1d. What still fails (the open defect, precisely)

After 1a–1c the chain runs to the end: `ACOSATMS finish-request token=0x400000077…` →
`ACOSATMS result token=0x400000077 result_to=0x100000003 request=7 delivered=false finished=true` →
`ACOSATMS deliver tag=0 method=scheduleTransaction app_thread=Some(0x100000001)` (both processes
attached). The primary process then executes one `H 159` transaction and does **not** run
`onRestart`/`onStart`/`onResume`/`onActivityResult` (`pregranted/logs/logcat.txt` has no such line;
the lifecycle logs added to the fixture are the probe). So the delivery reaches the caller and the
executor produces no effect.

Prime suspect, not yet confirmed: `android.app.servertransaction.ActivityResultItem.writeToParcel` in
this image writes **only** `writeTypedList(mResultInfoList)` (dex scan) — no `super` call, so no
`mActivityToken`. `TransactionExecutor.executeNonLifecycleItem` keys the record on
`item.getActivityToken()`, which is therefore null after a parcel round trip, and the item's
`getPostExecutionState() == ON_RESUME` makes it cycle a null record before executing. The same
encoding is what SYSUI1's dialog arms use, and those deliver results today, so the discriminating
experiment is the next step: capture the `ClientTransaction` bytes the host sends and the guest's
`TransactionExecutor` state (an offline round-trip through the `parcelable` codec, plus a
`ResultInfo`/`ActivityResultItem` golden captured from the image, answers it without an ART run).

## 2. The shade (item 2): built, 9 of 10 steps pass

* `fixtures/apps/systemui` — package `com.android.systemui`, `ShadeActivity` at the AOSP class path,
  one row per launch extra (`acos.shade.keys/titles/texts/actions`), a row tap answers
  `acos.shade.tapped` + `acos.shade.action` and finishes. Layout ids `shade_title`, `shade_empty`,
  `shade_row_0..2` (real resource ids, so the served tree and `selector: {id: shade_row_0}` work).
* `notifications` module — `system_apps: ["com.android.systemui"]` (`module::system_apks` resolves
  `fixtures/build/apps/systemui.apk`), a new `open_shade` control op, and a published
  `SystemUiResultSink` (`crate::host`) that the ATMS calls from `finish_activity`: the module finds
  the tapped record, takes it out of the list (not for a foreground-service notification) and answers
  the content `PendingIntent` sender, which the ATMS fires through the real sender table
  (`apply_system_ui_result`). `ServiceHost::set_atms`/`atms()` is the new seam (registry::install
  publishes the session's one `AtmsState`), documented on the setter.
* `scenarios/sysui1-shade.yaml` — post (notif-perm-probe) → `list` → `open_shade` (expects
  `rows: 1`, `keys: ["7:probe-notification"]`, `start_result: 0`) → the shade's window is the served
  tree (`assert_windows`, `shade_title`, `shade_row_0` = `Probe title — Probe text · Open`) → tap
  `shade_row_0` → the target activity's log line → `list` → `posted: []`.
  Evidence: `shade/verdict.json` (steps 0–8 `pass`).

### 2a. The two open defects on the tap

1. **The tap step's strict settle reports `detached`** (`shade/verdict.json` step 9,
   `settle.reason: detached`): the shade Activity finishes on the tap, so the surface the settle was
   watching goes away before a frame/tree change can be attributed to the gesture. That is a real
   question about the settle contract for "the tapped window closed", not an app defect.
2. **The target Activity is refused a process — half fixed.** The notification's content sender
   fires (`process 2: dev.acos.fixtures.notifpermprobe needs a process for
   …/TargetActivity (startActivity)`), and the launcher no longer refuses it with `no such package was
   installed`: `SecondaryPool::spawn` now recognises the session's **primary** package (`tag 0`) and
   records "the session's primary process; the launch goes to it" (the package is passed into the
   pool from the launcher, and `AtmsState::process_started` now fills an empty record name from the
   peer handshake — `attachApplication` carries no package, so the attach path could create the
   record with an empty one, and an empty package cannot answer "does this package already run?").
   The launch then has to be encoded for tag 0 by `Peer::encode_host_launch`, which resolves the
   process package through `AtmsState::process_list()`/`process_name`; on ART this still produced no
   `delivering ClientTransaction` line for tag 0, so the remaining step is to make the primary's
   package name authoritative (seed it from the launcher's `seed_attached_process`, or set
   `ACOS_PROCESS_NAME` for the primary too) and rerun.

## 3. Item 3 (real permission labels) is not started

The dialog's fallback is still the last name segment; `pregranted`/`allow` logcat shows
`label fallback for android.permission.CAMERA: … unsupported Android API: android.permission.IPermissionManager code 3 (getPermissionInfo)`,
i.e. the guest's `PackageManager.getPermissionInfo` lands on the **`IPermissionManager`** face here
(`getPermissionInfo(String, String, int)`, code 3), not `IPackageManager`. The planned shape (from
the orchestrator's decision): one `PermissionStore` owns the labels/groups; the module answers
`IPermissionManager.getPermissionInfo`/`getPermissionGroupInfo` (+ `getAllPermissionGroups`) from it
and the PMS face answers from the same store; `PermissionInfo.nonLocalizedLabel` carries the real
label so the framework's own `PermissionInfo.loadLabel` returns it without resource resolution (a new
`android.content.pm.PermissionGroupInfo` codec is needed; `PermissionInfo` already exists).

## 4. Evidence (this directory)

| file | what |
|---|---|
| `pregranted/`, `pregranted-trace/` | the pre-granted arm (verdict, markers, launcher stderr with the fix's MARK lines, ATMS trace) |
| `shade/` | the shade scenario, 9/10 |
| `sysui1-permission-allow/`, `sysui1-permission-deny/`, `notif-perm-granted/`, `notif-perm-denied/` | regressions, all `pass` |
| `a11y-probe/`, `mproc3-two-process/` | regressions (rerun in `.cache/batch2.log`) |

Commands (from the worktree, after the test-env symlinks):

```
export ACOS_FONT_DIR=/home/Qui/Projects/acos/.cache/boot/android-34/fonts
export ACOS_RUNTIME_DIR=$PWD/.cache/rt-sysui2 ACOS_SESSION_SOCKET=$PWD/.cache/sysui2-session.sock
export ACOS_VIEW_TEST_DIR=$PWD/.cache/lv ACOS_ART_BIN=$PWD/.cache/sysui2-target/release/acos-art
tools/art-runtime/run-exclusive.sh --task SYSUI2 -- tools/guarded.sh --mem 10G --time 700 -- \
  env -u ACOS_HOME .cache/sysui2-target/release/acos verify scenarios/sysui1-shade.yaml \
  --engine art --json --artifacts results/handoff/SYSUI2/shade --overwrite
```

## 5. `cargo test -p acos-art` in this worktree

163 pass; the 6 failures are all `live_view::tests::*` and all fail with
`stream socket path …/.cache/live-view-test/…/stream.sock is too long for a unix socket` — the
worktree path is longer than a unix socket allows (108 bytes). Pre-existing and environmental: the
same tests fail on `sysui1` at the same path, and they pass where the checkout path is shorter.

## 6. Smallest next actions

1. **Shade tap, part 2**: make the primary's package name authoritative so `Peer::encode_host_launch(0)`
   finds the pending launch (`AtmsState::process_list()` still answers an empty package for tag 0 on
   ART; seed it from `ServiceHost::seed_attached_process`, which the launcher already calls with the
   session package, or set `ACOS_PROCESS_NAME` for the primary). Then rerun `sysui1-shade`.
2. **Shade tap, part 1**: decide the settle contract for a tap that closes the tapped window
   (accept "the tapped window detached and a different window is now the top" as a change), or have
   the scenario tap and then `wait_for` the target activity instead of relying on the tap's settle.
   The former is the honest fix (the harness must not report a *successful* interaction as a timeout).
3. **Pre-granted**: the `ActivityResultItem` token question in §1d — offline `parcelable` round-trip +
   `ResultInfo` golden first, then the fix (either set the item token the way the framework's own
   `obtain` would, or deliver through the shape this image's `TransactionExecutor` needs).
4. **Labels**: §3.
