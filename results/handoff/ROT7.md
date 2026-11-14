# ROT7 — `back` reaches the framework's real back path (server half), and what is left

**Branch:** `art-back-1` (off `int-021` `52beccf` = v0.2.1 candidate incl. ROT6)
**Worktree:** `/home/Qui/Projects/acos/.cache/wt-rot7`, target `.cache/rot7-target`
**Date:** 2026-11-15
**Status:** **PARTIAL — the Activity really finishes (measured, full destroy
sequence), the `back` *step* still cannot bind a frame, because the app's only
window is gone and ACOS has nothing to draw in its place.**

Every ART run went through `tools/art-runtime/run-exclusive.sh --task ROT7` +
`tools/guarded.sh`; builds used `tools/guarded.sh --mem 14G --time 1500`.
Note on slots: two foreign `--exclusive` benchmark loops (`FB3`, `PERF-REL3`)
held the lock continuously and a shared run starves behind a stream of pending
exclusives (`run-exclusive.sh` gives exclusives priority), so ROT7's own
correctness runs were also submitted `--exclusive` to get a turn. No timing or
memory number is reported from them.

## 1. ROT6's diagnosis was wrong: the ATMS *does* adopt the guest token

ROT6's evidence for "the ATMS never learns the token" was invisible by
construction: `ACOS_ATMS_TRACE` printed to the launcher's **stderr**, and the CLI
engine keeps only an 8 KiB ring buffer of that stderr and surfaces it on errors
only. Worse, `.cache/rot6/**` is gone, and the `acos-art` binary used by
`acos verify` is the one named by `ACOS_ART_BIN` — a rebuilt `acos` does **not**
rebuild it (measured twice this pass: a stale `acos-art` shows zero ATMS calls).

So the trace sink is now a file: `ACOS_ATMS_TRACE=<path>` appends
(`…=1`/empty keeps stderr). With it, the launch path is:

```
call android.app.IActivityTaskManager.getActivityClientController ...
call android.app.IActivityClientController.getDisplayId      token=0x100000003
call android.app.IActivityClientController.activityResumed   token=0x100000003 known=false
adopted token=0x100000003 activity=activity placeholder=None guest_tagged=true
call android.app.IActivityClientController.activityIdle      token=0x100000003 known=true
```

i.e. `ResumeActivityItem.postExecute` → `ActivityClient.activityResumed` →
`IActivityClientController.activityResumed` **does** cross the peer, and
`adopt_activity` takes it through the `JAVA_TAG` route (`GUEST_JAVA_BINDER_TAG`).
`hello-views`/`multi-activity`'s launch never needed `AmsState::adopt_launch_token`.

## 2. GAP 2: the missing piece is `IActivityClientController.onBackPressed`

ROT6's raw evidence — `back` acked in 5 ms with no finish — was real, but the
cause was mis-attributed. The peer's unsupported log had the answer all along:

```
IActivityClientController reportSizeConfigurations  SERVICE_BAD_REQUEST
IActivityClientController onBackPressed code 62     UNSUPPORTED_ANDROID_API
```

API 34's `Activity.onBackPressed()` does not finish anything:

* `Activity.onBackPressed()` → `onBackInvoked()` →
  `ActivityClient.onBackPressed(mToken, new RequestFinishCallback(this))`
  (`IActivityClientController.onBackPressed`, **code 62, one-way** — hence the
  immediate 5 ms ack);
* `ActivityClientController.onBackPressed` decides, and asks the client to finish
  through that callback;
* `RequestFinishCallback.requestFinish()` posts `Activity.finishAfterTransition()`
  on the Activity's handler → `Activity.finish()` →
  `IActivityClientController.finishActivity`, whose `true` is what flips
  `Activity.mFinished`;
* `onPause` → `onSaveInstanceState`/`onStop` → `onDestroy` are `ClientTransaction`s
  the server sends afterwards (ROT6's guest-side `startTopFinishWatcher` already
  delivers them once `mFinished` flips).

Landed:

* `crates/acos-aidl/src/generate.rs` + `generated/tables.rs`: decode
  `android.app.IRequestFinishCallback` from the image (`requestFinish`, code 1,
  **one-way**) — regenerated with `cargo run -p acos-aidl --example gen`, a
  16-line diff, never hand-edited.
* `crates/acos-services/src/atms.rs`:
  * `AtmsState::on_back_pressed(token, callback)` — resolves/adopts the record,
    records `back_pressed`, and queues the reverse call; it never fabricates a
    finish;
  * `pending_reverse_calls: Vec<(target, interface, method, args)>`, encoded into
    `CallResult::deliveries` exactly like the `IApplicationThread` deliveries, so
    the peer sends one `F_TXN` to the **guest's** callback binder;
  * the `(ClientController, "onBackPressed")` arm; a null callback is a
    `BadRequest`, an unknown/zero token still errors
    (`an_unknown_or_zero_token_still_errors` untouched and green);
  * `atms_trace()` with the file sink above, plus `adopt-rejected` /
    `back-pressed` / `finish-request` lines.
* `crates/acos-art/guest/.../LaunchMain.java`: `backDispatcherFacts()` recorded in
  every `activity-resumed.marker` (`back_dispatcher=ahead_of_time=… 
  mDefaultBackCallback=… top_callback=…`) — the step-3 answer below.
* `crates/acos-services/tests/rot7_back.rs` (5 tests, all green).

Measured on `counter.yaml` (`--engine art`, `.cache/rot7/out/counter`):

```
ACOSATMS back-pressed token=0x100000003 callback=0x10000001a activity=activity
ACOSATMS call android.app.IActivityClientController.finishActivity token=0x100000003 known=true
ACOSATMS finish-request token=0x100000003 known=[4294967299] n=1
```
```
AcosCounter: onPause
AcosCounter: onStop
AcosCounter: onSaveInstanceState count=2
AcosCounter: onDestroy
```

So the framework's **real** path now runs end to end: key → ahead-of-time back →
`onBackPressed(callback)` → reverse `requestFinish` → `Activity.finish()` →
`finishActivity` → `mFinished` → Pause(finished)/Stop/Destroy → `onDestroy`.

## 3. Step 3 answered: which stage claims the UP

Not ACOS, and not the legacy `onKeyUp` branch. `Activity.onCreate` registers
`mDefaultBackCallback = this::onBackInvoked` on the window's
`OnBackInvokedDispatcher` whenever
`WindowOnBackInvokedDispatcher.isOnBackInvokedCallbackEnabled(activity)` (targetSdk
34 ⇒ true), so:

* `ViewRootImpl$NativePreImeInputStage.doOnBackKeyEvent` claims `KEYCODE_BACK_UP`
  (`topCallback.onBackInvoked()` → `FINISH_HANDLED`) — that is the stage that
  reported "handled", and it is the framework's own ahead-of-time back dispatch;
* `Activity.onKeyUp`'s legacy branch (which needs `mDefaultBackCallback == null`)
  is therefore unreachable for these apps. ROT6's premise came from the
  *bytecode*, not a measurement: `mDefaultBackCallback` is non-null.

Both branches end in the same `ActivityClient.onBackPressed`, which is why the
distinction never showed up in the ATMS trace; the new
`back_dispatcher=` marker makes it observable per Activity.

## 4. What still blocks the four `back` steps: the strict settle, not the path

`counter.yaml` now fails at `back` with a **different** reason than before:

```
verify: TIMEOUT (infra_timeout): back: strict settle timed out (detached)
  settle: { bound: timeout, mode: strict, reason: detached, pixels_changed: false }
```

`detached` is the guest probe's own pending reason
(`LaunchMain.checkSettle`: the decor's `getViewRootImpl()` is null after
`WindowManagerGlobal.removeView`). Two independent facts:

1. after the last Activity is destroyed the app has **no live window**, so the
   probe can never publish a settled sample;
2. the strict settle binds a step to a **guest draw at/after the gesture**
   (`sample.draw_ms >= publish_epoch_ms`), and a window removal draws nothing.

On a phone the screen after back is the launcher (a different window); ACOS has no
launcher and no second layer, so "the app's task finished" is currently
unrepresentable: the last committed C1 frame stays newest forever.

That is why `hello-views` 23/24, `multi-activity` 19/20, `v1-recreate` 14/15 and
`counter`'s tail are unchanged **as step verdicts** even though their `back` step
now really destroys the Activity, and why the negative
("crafted wrong expectation") cannot be produced yet either — it has to clear the
same `back` step first.

## 5. Measured this pass

* `cargo test --release --offline -p acos-services`: **all green** (lib 86 tests,
  every integration target ok, incl. the new `rot7_back` 5/5 and
  `w38_activity_token` 3/3 — `.cache/rot7/test-services.log`,
  `.cache/rot7/test-back.log`).
* `cargo test --release --offline -p acos-art -p acos-cli -p acos-verify`:
  **all green** (34 targets, 0 failures, 1 pre-existing `#[ignore]`;
  `rc=0` in `.cache/art-locks/runs.log`, log `.cache/rot7/test-rest.log`). The
  known worktree-path `w50_serve_art` socket failure did not reproduce here
  (this worktree's path is short).
* `counter.yaml --engine art`: 10/11 steps pass; `back` destroys the Activity and
  ends `TIMEOUT (detached)` (`.cache/rot7/out/counter`, ATMS trace
  `.cache/rot7/atms-counter.log`).
* `hello-views`, `multi-activity`, `v1-recreate`, the 15 gates, the 5 Gaten flows
  and the 5 negatives were **not re-run**: they all sit behind the same `back`
  wall (the three scenarios) or are unaffected by a change that only *adds* a
  previously-unsupported one-way method (gates/Gaten), and the lock was held by
  two foreign benchmark loops for most of the window. Stated as a gap, not
  claimed.

## 6. Next smallest actions, in order

1. **Decide the "task finished" screen, then let the strict settle bind it.** The
   honest options are (a) the display keeps the last committed frame and a step
   whose effect is "the app's only window is gone" binds on the *state*
   (`tree empty`, `pixels_changed: false`, a new bound value such as
   `window-gone`), or (b) the compositor publishes a new empty frame when the last
   layer is destroyed. (a) needs ~15 lines in `launch.rs`'s settle plus the
   guest probe reporting `settled` with an empty window forest instead of the
   permanent `detached` pending; (b) is a compositor decision (ADR). Either way the
   assertion stays strict: the scenario's own `onPause → onStop →
   onSaveInstanceState → onDestroy` patterns must still match.
2. Then `hello-views`, `multi-activity`, `v1-recreate`, `counter` run 3/3, and the
   negative ("assert the app's text after back" → `ELEMENT_NOT_FOUND`) proves the
   finish from the outside.
3. Then the 15 gates + 5 Gaten flows + 5 negatives, and
   `cargo test -p acos-art -p acos-cli` (only the known worktree-path
   `w50_serve_art` socket test is expected to fail).
4. Optional, measured but not fixed: `reportSizeConfigurations` still dies in the
   codec (`SizeConfigurationBuckets`: parcel underflow at 124, 3 records in
   `counter`). It is a pre-existing ROT6-era record, unrelated to back, and worth
   its own row.
