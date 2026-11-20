use acos_protocol::LifecycleState;
use acos_services::atms::{
    ActivityLifecycle, AtmsState, ClientTransaction, ClientTransactionItem, Transition,
};
use proptest::prelude::*;

fn fixture_states() -> Vec<LifecycleState> {
    let path = acos_services::workspace_path(
        "fixtures/reference/hello-views/art-api34-x86_64/lifecycle-summary.txt",
    )
    .expect("lifecycle-summary.txt");
    let text = std::fs::read_to_string(path).expect("read lifecycle-summary.txt");
    text.lines()
        .filter_map(|line| {
            let event = line.strip_prefix("AcosFixture: ")?;
            Some(match event {
                "onCreate" => LifecycleState::Created,
                "onStart" => LifecycleState::Started,
                "onResume" => LifecycleState::Resumed,
                "onPause" => LifecycleState::Paused,
                "onStop" => LifecycleState::Stopped,
                "onSaveInstanceState clicks=2" => LifecycleState::SaveInstanceState,
                "onDestroy" => LifecycleState::Destroyed,
                _ => return None,
            })
        })
        .collect()
}

fn tx(items: Vec<ClientTransactionItem>) -> ClientTransaction {
    let mut t = ClientTransaction::default();
    for item in items {
        t.push(item);
    }
    t
}

#[test]
fn replay_lifecycle_summary_order() {
    let expected = fixture_states();
    assert!(!expected.is_empty(), "fixture has lifecycle lines");

    let mut atms = AtmsState::new();
    atms.attach_application(0x1000, 7, "dev.acos.fixtures.helloviews");
    // `finishAttachApplication` schedules the launch transaction
    // (LaunchActivityItem + ResumeActivityItem), so it emits the first
    // onCreate/onStart/onResume cycle.
    atms.finish_attach_application(7).expect("finish attach");

    let launch = || {
        tx(vec![
            ClientTransactionItem::LaunchActivity {
                activity: "dev.acos.fixtures.helloviews.MainActivity".into(),
            },
            ClientTransactionItem::ResumeActivity,
        ])
    };
    let teardown = || {
        tx(vec![
            ClientTransactionItem::PauseActivity { finished: false },
            ClientTransactionItem::StopActivity { show_window: false },
            ClientTransactionItem::DestroyActivity { finished: true },
        ])
    };

    atms.schedule(0xabc, teardown()).expect("teardown 1");
    atms.schedule(0xabc, launch()).expect("relaunch");
    atms.schedule(0xabc, teardown()).expect("teardown 2");
    atms.schedule(0xabc, launch()).expect("back");

    let actual: Vec<LifecycleState> = atms
        .events
        .iter()
        .map(|e| e.state)
        .filter(|s| {
            !matches!(
                s,
                LifecycleState::AppCreated | LifecycleState::ProcessStarted
            )
        })
        .collect();
    assert_eq!(
        actual, expected,
        "lifecycle order must match the ART fixture"
    );
    // `finishAttachApplication` counts as the first (synthesised) transaction.
    assert_eq!(atms.transactions.len(), 5);
}

#[test]
fn illegal_transitions_are_rejected() {
    let mut atms = AtmsState::new();
    atms.ensure_activity(1, "A");
    // Resume before create/start is illegal.
    assert!(atms.transition(1, Transition::Resume).is_err());
    // Destroy before create is illegal.
    assert!(atms.transition(1, Transition::Destroy).is_err());
    // Pause before resume is illegal.
    atms.transition(1, Transition::Create).unwrap();
    assert!(atms.transition(1, Transition::Pause).is_err());
    atms.transition(1, Transition::Start).unwrap();
    atms.transition(1, Transition::Resume).unwrap();
    atms.transition(1, Transition::Pause).unwrap();
    atms.transition(1, Transition::Stop).unwrap();
    atms.transition(1, Transition::Destroy).unwrap();
    assert_eq!(atms.lifecycle_of(1), Some(ActivityLifecycle::Destroyed));
}

/// SYSUI2: a caller that was paused **and stopped** while a system-app dialog
/// was the top resumes when the host delivers its `ActivityResultItem`
/// (`AtmsState::deliver_result` sends `ActivityResultItem` + `ResumeActivityItem`,
/// so the guest reports `activityResumed` on a stopped record). AOSP's
/// `ActivityThread.handleResumeActivity` restarts such an Activity
/// (`performRestartActivity` + `onStart` + `onResume`); before this the host
/// refused `Stopped -> Resume`/`Stopped -> Start` and the caller's restart was
/// dropped with `illegal transition Resume from Stopped` (measured on ART in the
/// pre-granted permission arm).
#[test]
fn a_stopped_activity_can_be_started_and_resumed_again() {
    let mut atms = AtmsState::new();
    atms.ensure_activity(1, "A");
    atms.transition(1, Transition::Create).unwrap();
    atms.transition(1, Transition::Start).unwrap();
    atms.transition(1, Transition::Resume).unwrap();
    atms.transition(1, Transition::Pause).unwrap();
    atms.transition(1, Transition::Stop).unwrap();
    assert_eq!(atms.lifecycle_of(1), Some(ActivityLifecycle::Stopped));
    atms.transition(1, Transition::Start)
        .expect("a stopped Activity can be started again (onRestart -> onStart)");
    assert_eq!(atms.lifecycle_of(1), Some(ActivityLifecycle::Started));
    atms.transition(1, Transition::Resume)
        .expect("a stopped Activity can be resumed (the result delivery path)");
    assert_eq!(atms.lifecycle_of(1), Some(ActivityLifecycle::Resumed));

    // The exact shape the host delivers: resume straight from `Stopped`, which
    // is what `ActivityResultItem` + `ResumeActivityItem` makes the guest report.
    let mut direct = AtmsState::new();
    direct.ensure_activity(2, "B");
    direct.transition(2, Transition::Create).unwrap();
    direct.transition(2, Transition::Start).unwrap();
    direct.transition(2, Transition::Resume).unwrap();
    direct.transition(2, Transition::Pause).unwrap();
    direct.transition(2, Transition::Stop).unwrap();
    direct
        .transition(2, Transition::Resume)
        .expect("Stopped -> Resume is the caller's result-delivery resume");
    assert_eq!(direct.lifecycle_of(2), Some(ActivityLifecycle::Resumed));
}

#[test]
fn stop_emits_save_state_after_stop() {
    let mut atms = AtmsState::new();
    atms.ensure_activity(1, "A");
    atms.transition(1, Transition::Create).unwrap();
    atms.transition(1, Transition::Start).unwrap();
    atms.transition(1, Transition::Resume).unwrap();
    atms.transition(1, Transition::Pause).unwrap();
    atms.transition(1, Transition::Stop).unwrap();
    let states: Vec<_> = atms.events.iter().map(|e| e.state).collect();
    assert_eq!(
        states,
        vec![
            LifecycleState::Created,
            LifecycleState::Started,
            LifecycleState::Resumed,
            LifecycleState::Paused,
            LifecycleState::Stopped,
            LifecycleState::SaveInstanceState,
        ]
    );
}

fn arb_state() -> impl Strategy<Value = ActivityLifecycle> {
    prop_oneof![
        Just(ActivityLifecycle::Uncreated),
        Just(ActivityLifecycle::Created),
        Just(ActivityLifecycle::Started),
        Just(ActivityLifecycle::Resumed),
        Just(ActivityLifecycle::Paused),
        Just(ActivityLifecycle::Stopped),
        Just(ActivityLifecycle::Destroyed),
    ]
}

fn arb_transition() -> impl Strategy<Value = Transition> {
    prop_oneof![
        Just(Transition::Create),
        Just(Transition::Start),
        Just(Transition::Resume),
        Just(Transition::Pause),
        Just(Transition::Stop),
        Just(Transition::Destroy),
        Just(Transition::Relaunch),
    ]
}

proptest! {
    /// The transition table is a pure function and never accepts a move that
    /// leaves the activity in `Uncreated`.
    #[test]
    fn transition_table_never_returns_uncreated(state in arb_state(), t in arb_transition()) {
        if let Some(next) = state.apply(t) {
            prop_assert_ne!(next, ActivityLifecycle::Uncreated);
        }
    }

    /// A random walk only ever moves through legal states and a persisted
    /// activity never panics.
    #[test]
    fn random_walk_is_total(transitions in prop::collection::vec(arb_transition(), 1..60)) {
        let mut atms = AtmsState::new();
        atms.ensure_activity(7, "A");
        for t in transitions {
            let _ = atms.transition(7, t);
            let _ = atms.lifecycle_of(7);
        }
    }
}

#[test]
fn launch_mode_decisions_match_android() {
    use acos_services::atms::{decide_launch, LaunchDecision, TaskStack};
    assert_eq!(decide_launch(0, true), LaunchDecision::NewInstanceInTask);
    assert_eq!(decide_launch(1, true), LaunchDecision::ReuseTop);
    assert_eq!(decide_launch(1, false), LaunchDecision::NewInstanceInTask);
    assert_eq!(decide_launch(2, false), LaunchDecision::FinishAboveAndReuse);
    assert_eq!(decide_launch(3, false), LaunchDecision::NewTask);

    let mut task = TaskStack::new(10);
    task.push(1);
    task.push(2);
    task.push(3);
    assert_eq!(task.top(), Some(3));
    assert_eq!(task.clear_above(2), vec![3]);
    assert_eq!(task.pop(), Some(2));
    assert_eq!(task.pop(), Some(1));
    assert_eq!(task.pop(), None);
    assert!(task.is_empty());
}
