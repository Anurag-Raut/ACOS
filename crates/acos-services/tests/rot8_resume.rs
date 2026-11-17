//! ROT8: the server-side half of "Back from B resumes A".
//!
//! Stock ATMS, when the top Activity of a task finishes, makes the record below
//! it the task's top (and resumes it). The guest's lifecycle transactions
//! (Pause(finished) -> Resume(previous) -> Stop -> Destroy) follow that order;
//! the host's task stack must agree, and an unrelated record must not be
//! touched by the press or the finish.

use acos_services::ams::IntentFacts;
use acos_services::atms::AtmsState;

const PACKAGE: &str = "dev.acos.fixtures.multiactivity";
const MAIN: &str = "dev.acos.fixtures.multiactivity.MainActivity";
const SECOND: &str = "dev.acos.fixtures.multiactivity.SecondActivity";
const GUEST_TAG: u64 = 1 << 32;

fn attached() -> AtmsState {
    let mut atms = AtmsState::new();
    atms.attach_application(0x1000, 7, PACKAGE);
    atms.finish_attach_application(7).expect("finish attach");
    atms
}

#[test]
fn back_from_the_second_activity_leaves_the_first_as_the_task_top() {
    let mut atms = attached();
    atms.ams
        .manifests
        .add_apk(
            acos_services::workspace_path("fixtures/build/apps/multi-activity.apk")
                .expect("multi-activity fixture APK"),
            10_000,
        )
        .expect("seed the APK component table");

    let main = GUEST_TAG | 0xa1;
    atms.ams
        .start_activity(&IntentFacts::explicit(PACKAGE, MAIN), 0, 0, 0)
        .expect("startActivity resolves MainActivity");
    atms.activity_resumed(main, true).expect("adopt MainActivity");
    atms.ams
        .start_activity(&IntentFacts::explicit(PACKAGE, SECOND), 0, 0, 0)
        .expect("startActivity resolves SecondActivity");
    let second = GUEST_TAG | 0xa2;
    atms.activity_resumed(second, true).expect("adopt SecondActivity");

    let task = atms
        .ams
        .tasks
        .iter()
        .find(|t| t.activities.contains(&second))
        .expect("the second activity is in a task");
    assert_eq!(task.activities.last(), Some(&second), "B is the task top");

    // The framework's back path: press (callback), then the client's finish.
    atms.on_back_pressed(second, GUEST_TAG | 0xa3)
        .expect("back press on the top");
    atms.finish_activity(second, 0, None).expect("finish of the top");

    let top = atms
        .ams
        .tasks
        .iter()
        .flat_map(|t| t.activities.last().copied())
        .next();
    assert_eq!(
        top,
        Some(main),
        "the record below becomes the task top (what the guest resumes)"
    );
    let below = atms.activities.iter().find(|a| a.token == main).expect("A");
    assert!(!below.finished && !below.back_pressed, "A is untouched by B's back");
}
