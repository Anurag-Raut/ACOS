//! ROT8b: the host ATMS proof behind `window-gone` / `task_finished`.
//!
//! A guest sample with an empty window forest is only a finished task when the
//! HOST ATMS agrees: no live record and no pending start. `finish()` +
//! `startActivity(Home)` leaves Home queued in `pending_launches` while the
//! guest already destroyed the finisher; that gap must not prove "finished".

use acos_services::ams::IntentFacts;
use acos_services::atms::AtmsState;

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

#[test]
fn live_record_is_not_finished() {
    let mut atms = seeded();
    atms.ams
        .start_activity(&IntentFacts::explicit(PACKAGE, MAIN), 0, 0, 0)
        .unwrap();
    atms.activity_resumed(GUEST_TAG | 1, true).unwrap();
    assert!(!atms.task_finished_proof(), "a resumed record is live");
}

#[test]
fn pending_start_blocks_the_proof_even_with_every_record_destroyed() {
    let mut atms = seeded();
    atms.ams
        .start_activity(&IntentFacts::explicit(PACKAGE, MAIN), 0, 0, 0)
        .unwrap();
    let main = GUEST_TAG | 1;
    atms.activity_resumed(main, true).unwrap();
    // finish() + startActivity(Second) in one tap: Second is pending, Main is
    // finished and the guest destroys it at once.
    atms.ams
        .start_activity(&IntentFacts::explicit(PACKAGE, SECOND), 0, 0, 0)
        .unwrap();
    atms.finish_activity(main).unwrap();
    atms.activity_paused(main).unwrap();
    atms.activity_stopped(main, None).unwrap();
    atms.activity_destroyed(main).unwrap();
    assert!(
        !atms.task_finished_proof(),
        "a pending start means the task is not finished"
    );
    // Second materialises: a live record, still not finished.
    atms.activity_resumed(GUEST_TAG | 2, true).unwrap();
    assert!(!atms.task_finished_proof());
}

#[test]
fn all_records_destroyed_and_nothing_pending_is_finished() {
    let mut atms = seeded();
    atms.ams
        .start_activity(&IntentFacts::explicit(PACKAGE, MAIN), 0, 0, 0)
        .unwrap();
    let main = GUEST_TAG | 1;
    atms.activity_resumed(main, true).unwrap();
    atms.finish_activity(main).unwrap();
    atms.activity_paused(main).unwrap();
    atms.activity_stopped(main, None).unwrap();
    atms.activity_destroyed(main).unwrap();
    assert!(atms.task_finished_proof());
}

#[test]
fn an_unnamed_record_fails_closed() {
    let mut atms = seeded();
    atms.ensure_activity(GUEST_TAG | 9, "");
    assert!(!atms.task_finished_proof(), "a record with no class is not judged finished");
}
