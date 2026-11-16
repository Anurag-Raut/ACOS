//! MPROC5: the top-of-task rule across app processes.
//!
//! On a device `ActivityTaskSupervisor` sends the previous top's
//! `PauseActivityItem` and then its `StopActivityItem` to **that activity's own**
//! `IApplicationThread` when another process's activity resumes, and the item
//! that resumes the activity below a finished one goes to *its* process. ACOS
//! runs one process per package, so without this rule two guests both believe
//! they are resumed and the window stack has no owner.
//!
//! Rows:
//! * a cross-process resume queues exactly `PauseActivityItem` then
//!   `StopActivityItem` for the process that lost the top, encoded as the
//!   image's own `ClientTransaction` shape (item classes read back from the
//!   parcelable), and moves `tops` + the published foreground tag;
//! * finishing the new top queues one `ResumeActivityItem` for the activity
//!   below it when that one lives in the other process;
//! * a same-process resume queues nothing (the app's own framework watchers
//!   schedule those items; the host must not double-deliver).

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use acos_services::ams::{AmsState, Task};
use acos_services::atms::{AtmsState, Transition};
use acos_services::value::{Parcelable, Value};

/// The peer's `JAVA_TAG`: a binder owned by a guest JVM.
const GUEST_TAG: u64 = 1 << 32;

/// A guest-minted token `wire` of process `tag` (the peer's binder-id layout).
fn token(tag: u32, wire: u64) -> u64 {
    GUEST_TAG | (u64::from(tag) << 34) | wire
}

fn state() -> AtmsState {
    let mut atms = AtmsState::new();
    atms.process_started(0, "dev.acos.fixtures.proca", 10_100);
    atms.process_started(1, "dev.acos.fixtures.procb", 10_193);
    atms.attach_application_for(0, token(0, 0x10), 1, "dev.acos.fixtures.proca");
    atms.attach_application_for(1, token(1, 0x10), 2, "dev.acos.fixtures.procb");
    atms
}

fn create_and_start(atms: &mut AtmsState, activity_token: u64, class: &str) {
    atms.ensure_activity(activity_token, class);
    atms.transition(activity_token, Transition::Create)
        .expect("create");
    atms.transition(activity_token, Transition::Start)
        .expect("start");
}

/// `(item class name, activity token)` of one queued lifecycle call, read back
/// from the encoded `ClientTransaction` parcelable.
fn encoded(call: &(u32, String, Vec<Value>)) -> (String, u64) {
    let (tag, method, args) = call;
    assert_eq!(method, "scheduleTransaction", "delivered as IApplicationThread.scheduleTransaction");
    let Value::TypedObject(Parcelable::Ordered { class, values }) = &args[0] else {
        panic!("expected an encoded ClientTransaction, got {:?}", args[0]);
    };
    assert_eq!(class, "android.app.servertransaction.ClientTransaction");
    assert_eq!(*tag, 0, "the pause/stop belongs to the process that lost the top");
    let token = values[1].as_binder().expect("mActivityToken binder").0;
    let Value::NamedParcelable(Parcelable::Ordered { class, .. }) = &values[2] else {
        panic!("expected a named lifecycle request, got {:?}", values[2]);
    };
    let item = class
        .rsplit('.')
        .next()
        .expect("class name has a segment")
        .to_string();
    (item, token)
}

#[test]
fn a_cross_process_resume_pauses_then_stops_the_previous_top() {
    let mut atms = state();
    let top = Arc::new(AtomicU32::new(0));
    atms.top_process = Some(Arc::clone(&top));
    let a = token(0, 0x101);
    let b = token(1, 0x201);
    create_and_start(&mut atms, a, "dev.acos.fixtures.proca/.ProcAActivity");
    atms.activity_resumed(a, true).expect("A resumes");
    assert_eq!(atms.tops.get(&0), Some(&a));
    assert_eq!(top.load(Ordering::Relaxed), 0);
    assert!(
        atms.pending_lifecycle_calls.is_empty(),
        "the first resume has no previous top to pause"
    );

    create_and_start(&mut atms, b, "dev.acos.fixtures.procb/.ProcBActivity");
    atms.activity_resumed(b, true).expect("B resumes");

    assert_eq!(
        atms.pending_lifecycle_calls.len(),
        2,
        "PauseActivityItem then StopActivityItem"
    );
    assert_eq!(
        encoded(&atms.pending_lifecycle_calls[0]),
        ("PauseActivityItem".to_string(), a)
    );
    assert_eq!(
        encoded(&atms.pending_lifecycle_calls[1]),
        ("StopActivityItem".to_string(), a)
    );
    assert_eq!(atms.tops.get(&1), Some(&b));
    assert_eq!(atms.tops.get(&0), None, "A is no longer the top of its process");
    assert_eq!(
        top.load(Ordering::Relaxed),
        1,
        "the foreground tag follows the top (the launcher serves that process's tree)"
    );
}

#[test]
fn finishing_the_new_top_resumes_the_activity_below_in_the_other_process() {
    let mut atms = state();
    let a = token(0, 0x102);
    let b = token(1, 0x202);
    create_and_start(&mut atms, a, "dev.acos.fixtures.proca/.ProcAActivity");
    atms.activity_resumed(a, true).expect("A resumes");
    create_and_start(&mut atms, b, "dev.acos.fixtures.procb/.ProcBActivity");
    atms.activity_resumed(b, true).expect("B resumes");
    // One task, A below B (what `startActivity` recorded: the task stack and
    // the activity entries it keeps beside it).
    atms.ams.tasks.push(Task {
        id: 10,
        activities: vec![a, b],
    });
    for (token, class) in [
        (a, "dev.acos.fixtures.proca.ProcAActivity"),
        (b, "dev.acos.fixtures.procb.ProcBActivity"),
    ] {
        atms.ams.activities.push(acos_services::ams::ActivityEntry {
            token,
            package: "fixture".into(),
            class: class.into(),
            task_id: 10,
            finished: false,
            reused: false,
        });
    }
    atms.pending_lifecycle_calls.clear();

    atms.finish_activity(b).expect("B finishes");

    assert_eq!(atms.pending_lifecycle_calls.len(), 1);
    let (tag, method, args) = &atms.pending_lifecycle_calls[0];
    assert_eq!((*tag, method.as_str()), (0, "scheduleTransaction"));
    let Value::TypedObject(Parcelable::Ordered { values, .. }) = &args[0] else {
        panic!("expected an encoded ClientTransaction");
    };
    let token_of = values[1].as_binder().expect("mActivityToken").0;
    assert_eq!(token_of, a, "the resume is keyed on A's own token");
    let Value::NamedParcelable(Parcelable::Ordered { class, .. }) = &values[2] else {
        panic!("expected a named lifecycle request");
    };
    assert!(
        class.ends_with("ResumeActivityItem"),
        "expected ResumeActivityItem, got {class}"
    );
    assert_eq!(atms.tops.get(&0), Some(&a));
}

#[test]
fn a_same_process_resume_queues_nothing() {
    let mut atms = state();
    let first = token(0, 0x103);
    let second = token(0, 0x104);
    create_and_start(&mut atms, first, "dev.acos.fixtures.proca/.ProcAActivity");
    atms.activity_resumed(first, true).expect("first resumes");
    create_and_start(&mut atms, second, "dev.acos.fixtures.proca/.SecondActivity");
    atms.activity_resumed(second, true).expect("second resumes");
    assert!(
        atms.pending_lifecycle_calls.is_empty(),
        "the guest's own watchers schedule pause/stop inside one process"
    );
    assert_eq!(atms.tops.get(&0), Some(&second));
}

#[test]
fn the_status_is_data_not_a_guess() {
    // A bare state machine has no published tag: nothing panics, nothing is
    // invented, and `tops` still records the truth.
    let mut atms = AtmsState::new();
    atms.process_started(0, "pkg", 1);
    atms.attach_application_for(0, token(0, 1), 1, "pkg");
    let a = token(0, 0x105);
    create_and_start(&mut atms, a, "pkg/.A");
    atms.activity_resumed(a, true).expect("resume");
    assert_eq!(atms.tops.get(&0), Some(&a));
    let _ = AmsState::new();
}
