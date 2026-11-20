//! SYSUI4: the host's result delivery ordering, pinned.
//!
//! AOSP never pauses/stops an activity it is about to resume with a result:
//! `ActivityRecord.sendResult` keeps the result on the record while the caller
//! is not resumed, and `TaskFragment.resumeTopActivity` resumes the caller
//! **with** the stored results (`ActivityResultItem` + `ResumeActivityItem` in
//! one transaction) after calling `next.notifyAppResumed()`. A late stop report
//! for a record that is no longer stopping is ignored
//! (`ActivityRecord.activityStopped`: "Activity reported stop, but no longer
//! stopping").
//!
//! The pre-granted permission arm is exactly that window: the controller
//! activity is launched (which queues the caller's `PauseActivityItem` +
//! `StopActivityItem`), and it finishes inside `onCreate` before the caller ever
//! ran them. These cases pin the host's answer; they fail before the fix.

use acos_services::atms::{ActivityLifecycle, AtmsState, ClientTransactionItem, Transition};

const CALLER: u64 = 0x1000_0003;
/// Process tag 1 (`process_tag_of` reads bits 34..40: this is the
/// `0x400000077` token the pre-granted run traced).
const DIALOG: u64 = 0x4_0000_0077;

fn resumed(atms: &mut AtmsState, token: u64, class: &str) {
    atms.ensure_activity(token, class);
    atms.transition(token, Transition::Create).expect("create");
    atms.transition(token, Transition::Start).expect("start");
    // `activity_resumed` is the guest's report: it also publishes the process's
    // top-of-task (`top_of_task`), which is what queues the previous top's
    // pause/stop.
    atms.activity_resumed(token, true).expect("resume");
}

/// The caller runs in the primary process, the controller in a second one, with
/// both app threads attached (what `attachApplication` records).
fn state() -> AtmsState {
    let mut atms = AtmsState::new();
    atms.process_started(0, "dev.acos.fixtures.permprobe", 10_192, 0);
    atms.attach_application_for(0, 0x1000_0001, 1, "dev.acos.fixtures.permprobe");
    atms.finish_attach_application_for(0, 1)
        .expect("the primary app thread attached");
    atms.process_started(1, "com.android.permissioncontroller", 10_193, 0);
    atms.attach_application_for(1, 0x1000_0002, 2, "com.android.permissioncontroller");
    atms.finish_attach_application_for(1, 2)
        .expect("the controller app thread attached");
    atms
}

fn has_result_item(atms: &AtmsState) -> bool {
    atms.pending_lifecycle_calls.iter().any(|call| {
        call.token == CALLER
            && call.args.iter().any(|arg| {
                matches!(
                    arg,
                    acos_services::value::Value::TypedObject(parcelable)
                        if acos_services::client_transaction::encode(parcelable)
                            .map(|bytes| bytes.len() > 0)
                            .unwrap_or(false)
                )
            })
            && call.item == ClientTransactionItem::ResumeActivity
    })
}

#[test]
fn a_dialog_that_finishes_in_oncreate_leaves_the_caller_no_stale_pause_or_stop() {
    let mut atms = state();
    resumed(
        &mut atms,
        CALLER,
        "dev.acos.fixtures.permprobe/.MainActivity",
    );
    // The controller is launched by the caller's `requestPermissions`
    // (`startActivityForResult` with the AOSP who), which makes it the top and
    // queues the caller's pause + stop (MPROC5's `top_of_task`).
    resumed(
        &mut atms,
        DIALOG,
        "com.android.permissioncontroller/permission.ui.GrantPermissionsActivity",
    );
    let dialog = atms
        .activities
        .iter_mut()
        .find(|a| a.token == DIALOG)
        .expect("the controller record");
    dialog.result_to = CALLER;
    dialog.request_code = 7;
    dialog.result_who = Some("@android:requestPermissions:".into());
    dialog.result_code = -1; // Activity.RESULT_OK
    let queued: Vec<String> = atms
        .pending_lifecycle_calls
        .iter()
        .filter(|call| call.token == CALLER)
        .map(|call| format!("{:?}", call.item))
        .collect();
    assert_eq!(
        queued,
        vec![
            "PauseActivity { finished: false }".to_string(),
            "StopActivity { show_window: false }".to_string()
        ],
        "the controller becoming top pauses then stops the caller"
    );

    // It answers inside `onCreate` and finishes (the pre-granted short circuit).
    atms.finish_activity(DIALOG, -1, None).expect("finish");
    atms.activity_destroyed(DIALOG).expect("destroy");

    // The result delivery superseded the caller's pause/stop: nothing is queued
    // for the caller except the result transaction that restarts/resumes it.
    let caller_calls: Vec<&str> = atms
        .pending_lifecycle_calls
        .iter()
        .filter(|call| call.token == CALLER)
        .map(|call| match call.item {
            ClientTransactionItem::PauseActivity { .. } => "pause",
            ClientTransactionItem::StopActivity { .. } => "stop",
            ClientTransactionItem::ResumeActivity => "resume+result",
            _ => "other",
        })
        .collect();
    assert_eq!(
        caller_calls,
        vec!["resume+result"],
        "no stale pause/stop may follow the caller's ActivityResultItem \
         (calls: {caller_calls:?})"
    );
    assert!(has_result_item(&atms), "the caller's result is queued");
    assert_eq!(
        atms.lifecycle_of(CALLER),
        Some(ActivityLifecycle::Resumed),
        "the result transaction resumes the caller, so the host models it as resumed"
    );
}

#[test]
fn a_stop_that_lands_after_the_result_resume_is_ignored_like_aosp() {
    let mut atms = state();
    resumed(
        &mut atms,
        CALLER,
        "dev.acos.fixtures.permprobe/.MainActivity",
    );
    // The measured ART order (`sysui1-permission-pregranted`): the caller had
    // already run pause and stop before the result arrived, so the delivery is
    // the result+resume from `Stopped` — and the guest's deferred
    // `PendingTransactionActions.StopInfo` reports `activityStopped` *after* its
    // resume transaction ran.
    atms.ensure_activity(
        DIALOG,
        "com.android.permissioncontroller/.GrantPermissionsActivity",
    );
    atms.transition(DIALOG, Transition::Create).expect("create");
    atms.transition(DIALOG, Transition::Start).expect("start");
    atms.transition(DIALOG, Transition::Resume).expect("resume");
    atms.pending_lifecycle_calls.clear();
    atms.transition(CALLER, Transition::Pause).expect("pause");
    atms.transition(CALLER, Transition::Stop).expect("stop");
    let dialog = atms
        .activities
        .iter_mut()
        .find(|a| a.token == DIALOG)
        .expect("the controller record");
    dialog.result_to = CALLER;
    dialog.request_code = 7;
    dialog.result_who = Some("@android:requestPermissions:".into());
    dialog.result_code = -1;

    atms.finish_activity(DIALOG, -1, None).expect("finish");
    atms.activity_destroyed(DIALOG).expect("destroy");
    assert_eq!(atms.lifecycle_of(CALLER), Some(ActivityLifecycle::Resumed));

    // AOSP: "Activity reported stop, but no longer stopping" — not an error.
    atms.activity_stopped(CALLER, None)
        .expect("a late stop for a resumed record is ignored, never an illegal transition");
    assert_eq!(
        atms.lifecycle_of(CALLER),
        Some(ActivityLifecycle::Resumed),
        "the ignored stop did not move the record"
    );
}

#[test]
fn a_pause_stop_that_was_already_sent_is_not_withdrawn() {
    let mut atms = state();
    resumed(
        &mut atms,
        CALLER,
        "dev.acos.fixtures.permprobe/.MainActivity",
    );
    resumed(
        &mut atms,
        DIALOG,
        "com.android.permissioncontroller/permission.ui.GrantPermissionsActivity",
    );
    let dialog = atms
        .activities
        .iter_mut()
        .find(|a| a.token == DIALOG)
        .expect("the controller record");
    dialog.result_to = CALLER;
    dialog.request_code = 7;
    dialog.result_who = Some("@android:requestPermissions:".into());
    dialog.result_code = -1;
    // The guest processed both items before the controller finished, so the
    // reports moved the record and the queue no longer holds them.
    atms.pending_lifecycle_calls.clear();
    atms.transition(CALLER, Transition::Pause).expect("pause");
    atms.transition(CALLER, Transition::Stop).expect("stop");

    atms.finish_activity(DIALOG, -1, None).expect("finish");
    atms.activity_destroyed(DIALOG).expect("destroy");

    let caller_calls: Vec<&str> = atms
        .pending_lifecycle_calls
        .iter()
        .filter(|call| call.token == CALLER)
        .map(|call| match call.item {
            ClientTransactionItem::PauseActivity { .. } => "pause",
            ClientTransactionItem::StopActivity { .. } => "stop",
            ClientTransactionItem::ResumeActivity => "resume+result",
            _ => "other",
        })
        .collect();
    assert_eq!(
        caller_calls,
        vec!["resume+result"],
        "an already-reported pause/stop is state, not a queue entry"
    );
    assert_eq!(
        atms.lifecycle_of(CALLER),
        Some(ActivityLifecycle::Resumed),
        "the result resumes a stopped caller"
    );
}
