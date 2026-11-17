//! ROT8b: the host ATMS proof behind `window-gone` / `task_finished`.
//!
//! A guest sample with an empty window forest is only a finished task when the
//! HOST ATMS agrees: no live record and no pending start. `finish()` +
//! `startActivity(Home)` leaves Home queued in `pending_launches` while the
//! guest already destroyed the finisher; that gap must not prove "finished".
//!
//! The guest emits `window-gone` from its probe thread, so the host's destroy
//! callback for the finishing record can still be in flight when the sample
//! lands: a record the client has already `finish()`ed is ended (AOSP keeps it
//! only until `onDestroy`), while any other non-destroyed record is live.

use acos_services::ams::IntentFacts;
use acos_services::atms::{
    AtmsState, ClientTransaction, ClientTransactionItem, FIXTURE_ACTIVITY_TOKEN,
};

const PACKAGE: &str = "dev.acos.fixtures.multiactivity";
const MAIN: &str = "dev.acos.fixtures.multiactivity.MainActivity";
const SECOND: &str = "dev.acos.fixtures.multiactivity.SecondActivity";
const GUEST_TAG: u64 = 1 << 32;

fn seeded() -> AtmsState {
    let mut atms = AtmsState::new();
    atms.attach_application(0x1000, 7, PACKAGE);
    atms.finish_attach_application(7).expect("finish attach");
    atms.ams
        .manifests
        .add_apk(
            acos_services::workspace_path("fixtures/build/apps/multi-activity.apk")
                .expect("multi-activity fixture APK"),
            10_000,
        )
        .expect("seed the APK component table");
    atms
}

/// Bring `token` to a live (`Resumed`) record the way the framework does:
/// `LaunchActivity` -> `Start` -> `Resume`. The synthetic fixture record
/// `finishAttachApplication` planted is dropped first, exactly as adopting the
/// guest's real token does (`FIXTURE_ACTIVITY_TOKEN`).
fn resumed(atms: &mut AtmsState, token: u64, name: &str) {
    // The first callback for the guest's token adopts it: the planted fixture
    // record is dropped and the pending `startActivity` launch is bound to it.
    atms.activity_resumed(token, true).expect("adopt");
    let record = atms
        .activities
        .iter()
        .find(|a| a.token == token)
        .expect("adopted record");
    assert_eq!(record.activity, name, "the launch resolved this Activity");
    let mut transaction = ClientTransaction { items: Vec::new() };
    transaction.push(ClientTransactionItem::LaunchActivity {
        activity: name.to_string(),
    });
    transaction.push(ClientTransactionItem::ResumeActivity);
    atms.schedule(token, transaction).expect("launch+resume");
}

#[test]
fn live_record_is_not_finished() {
    let mut atms = seeded();
    atms.ams
        .start_activity(&IntentFacts::explicit(PACKAGE, MAIN), 0, 0, 0)
        .unwrap();
    resumed(&mut atms, GUEST_TAG | 1, MAIN);
    assert!(
        atms.lifecycle_of(GUEST_TAG | 1) == Some(acos_services::atms::ActivityLifecycle::Resumed),
        "the record really is live"
    );
    assert!(!atms.task_finished_proof(), "a resumed record is live");
}

#[test]
fn a_paused_or_stopped_record_is_still_live() {
    // Not every non-resumed record is finished: a paused/stopped Activity is
    // still part of the task (a dialog on top, a translucent Activity).
    for state in ["paused", "stopped"] {
        let mut atms = seeded();
        atms.ams
            .start_activity(&IntentFacts::explicit(PACKAGE, MAIN), 0, 0, 0)
            .unwrap();
        let main = GUEST_TAG | 1;
        resumed(&mut atms, main, MAIN);
        atms.activity_paused(main).unwrap();
        if state == "stopped" {
            atms.activity_stopped(main, None).unwrap();
        }
        assert!(
            !atms.task_finished_proof(),
            "a {state} record is not finished"
        );
    }
}

#[test]
fn pending_start_blocks_the_proof_even_with_every_record_destroyed() {
    let mut atms = seeded();
    atms.ams
        .start_activity(&IntentFacts::explicit(PACKAGE, MAIN), 0, 0, 0)
        .unwrap();
    let main = GUEST_TAG | 1;
    resumed(&mut atms, main, MAIN);
    // finish() + startActivity(Second) in one tap: Second is pending, Main is
    // finished and the guest destroys it at once.
    atms.ams
        .start_activity(&IntentFacts::explicit(PACKAGE, SECOND), 0, 0, 0)
        .unwrap();
    atms.finish_activity(main, 0, None).unwrap();
    atms.activity_paused(main).unwrap();
    atms.activity_stopped(main, None).unwrap();
    atms.activity_destroyed(main).unwrap();
    assert!(
        !atms.task_finished_proof(),
        "a pending start means the task is not finished"
    );
    // Second materialises: a live record, still not finished.
    resumed(&mut atms, GUEST_TAG | 2, SECOND);
    assert!(!atms.task_finished_proof(), "Second is live");
}

#[test]
fn a_finished_record_ends_before_its_destroy_callback_arrives() {
    // The guest's `window-gone` sample and the host's `activityDestroyed`
    // report travel on different channels. A record the client has finished
    // is no longer live even while the host is still `Resumed` on it — the
    // guest can only emit `window-gone` after `onDestroy` ran.
    let mut atms = seeded();
    atms.ams
        .start_activity(&IntentFacts::explicit(PACKAGE, MAIN), 0, 0, 0)
        .unwrap();
    let main = GUEST_TAG | 1;
    resumed(&mut atms, main, MAIN);
    atms.finish_activity(main, 0, None).unwrap();
    assert!(
        atms.task_finished_proof(),
        "finish() ends the record; the destroy callback is bookkeeping"
    );
}

#[test]
fn all_records_destroyed_and_nothing_pending_is_finished() {
    let mut atms = seeded();
    atms.ams
        .start_activity(&IntentFacts::explicit(PACKAGE, MAIN), 0, 0, 0)
        .unwrap();
    let main = GUEST_TAG | 1;
    resumed(&mut atms, main, MAIN);
    atms.finish_activity(main, 0, None).unwrap();
    atms.activity_paused(main).unwrap();
    atms.activity_stopped(main, None).unwrap();
    atms.activity_destroyed(main).unwrap();
    assert!(atms.task_finished_proof());
}

#[test]
fn an_unstarted_record_fails_closed() {
    // A record that exists but never ran is not `alive` in the lifecycle enum;
    // proof is positive now (`finished` or `Destroyed`), so it fails closed.
    let mut atms = seeded();
    atms.activities
        .retain(|a| a.token != FIXTURE_ACTIVITY_TOKEN);
    atms.ensure_activity(GUEST_TAG | 8, MAIN);
    assert!(
        !atms.task_finished_proof(),
        "a record that never started is not finished"
    );
}

#[test]
fn an_unnamed_record_fails_closed() {
    let mut atms = seeded();
    atms.activities
        .retain(|a| a.token != FIXTURE_ACTIVITY_TOKEN);
    // `ensure_activity` is what an unidentified token becomes; it cannot be
    // judged finished even when it carries the flag.
    atms.ensure_activity(GUEST_TAG | 9, "").finished = true;
    assert!(
        !atms.task_finished_proof(),
        "a record with no class is not judged finished"
    );
}
