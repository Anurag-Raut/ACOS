//! MPROC5c step 2: the idle kill's **rule**, at the service layer.
//!
//! Stock `ActivityManagerService` keeps a lazily started process while it holds
//! something (a started activity, a started/bound service) and kills it once it
//! is a cached process past the timeout. The rule lives here because the
//! activity manager is the only component that knows those facts; the launcher
//! owns the process and does the killing (proved on real ART, not here).
//!
//! The invariant these tests pin: a process is never reported idle while it
//! holds a started activity or a service, and the **launched primary**
//! (`tag 0`) is never killed.

use acos_services::ams::ServiceRecord;
use acos_services::atms::{ActivityLifecycle, ActivityRecord, AtmsState};
use std::time::Duration;

/// A guest activity token: `acos-binder-peer` writes the process tag into bits
/// 34..40 of every binder id it imports from a guest (MPROC2 step 3a).
fn token_of(tag: u32, wire: u64) -> u64 {
    (wire & 0x0000_0000_ffff_ffff) | (u64::from(tag) << 34)
}

fn activity(tag: u32, token: u64, lifecycle: ActivityLifecycle) -> ActivityRecord {
    ActivityRecord {
        token,
        activity: format!("dev.acos.fixtures.procb.Activity{tag}"),
        task_id: 10,
        display_id: 0,
        lifecycle,
        resumed: matches!(lifecycle, ActivityLifecycle::Resumed),
        idle: false,
        finished: false,
        saved_clicks: 0,
        result_to: 0,
        request_code: 0,
        result_code: 0,
        result_data: None,
        result_delivered: false,
    }
}

#[test]
fn a_lazily_spawned_process_with_nothing_started_is_idle_after_the_timeout() {
    let mut atms = AtmsState::new();
    atms.process_started(0, "dev.acos.fixtures.proca", 10001);
    atms.process_started(1, "dev.acos.fixtures.procb", 10002);
    // Nothing started yet: not idle under the timeout, idle once it expires.
    assert!(
        atms.idle_processes(Duration::from_secs(3600)).is_empty(),
        "a process that just spawned is not idle yet"
    );
    let idle = atms.idle_processes(Duration::from_secs(0));
    assert_eq!(
        idle.iter().map(|(tag, ..)| *tag).collect::<Vec<_>>(),
        vec![1],
        "only the lazily spawned secondary is a candidate (tag 0 is the session's own process)"
    );
    assert_eq!(idle[0].1, "dev.acos.fixtures.procb");
}

#[test]
fn a_started_activity_keeps_its_process_alive() {
    let mut atms = AtmsState::new();
    atms.process_started(0, "dev.acos.fixtures.proca", 10001);
    atms.process_started(1, "dev.acos.fixtures.procb", 10002);
    atms.activities.push(activity(
        1,
        token_of(1, 0x11),
        ActivityLifecycle::Started,
    ));
    assert!(
        atms.idle_processes(Duration::from_secs(0)).is_empty(),
        "a started activity is work: the process must survive any timeout"
    );
    // A paused activity is a cached-process candidate, not work.
    atms.activities[0].lifecycle = ActivityLifecycle::Paused;
    assert_eq!(
        atms.idle_processes(Duration::from_secs(0))
            .iter()
            .map(|(tag, ..)| *tag)
            .collect::<Vec<_>>(),
        vec![1],
        "a paused (not started) activity no longer keeps the process alive"
    );
    // A finished activity is not work either.
    atms.activities[0].lifecycle = ActivityLifecycle::Resumed;
    atms.activities[0].finished = true;
    assert_eq!(atms.idle_processes(Duration::from_secs(0)).len(), 1);
}

#[test]
fn a_bound_service_keeps_its_process_alive() {
    let mut atms = AtmsState::new();
    atms.process_started(1, "dev.acos.fixtures.procb", 10002);
    atms.ams.services.insert(
        "dev.acos.fixtures.procb/dev.acos.fixtures.procb.EchoService".into(),
        ServiceRecord {
            package: "dev.acos.fixtures.procb".into(),
            class: "dev.acos.fixtures.procb.EchoService".into(),
            started: false,
            bind_count: 1,
        },
    );
    assert!(
        atms.idle_processes(Duration::from_secs(0)).is_empty(),
        "a live bindService connection is work: the process must survive"
    );
    atms.ams.services.clear();
    assert_eq!(
        atms.idle_processes(Duration::from_secs(0))
            .iter()
            .map(|(tag, ..)| *tag)
            .collect::<Vec<_>>(),
        vec![1],
        "once the last connection is gone the process is a candidate again"
    );
}

#[test]
fn the_foreground_transition_restarts_the_idle_clock() {
    let mut atms = AtmsState::new();
    atms.process_started(1, "dev.acos.fixtures.procb", 10002);
    atms.activities.push(activity(
        1,
        token_of(1, 0x21),
        ActivityLifecycle::Uncreated,
    ));
    // Resuming the activity (the cross-process top-of-task path) marks the
    // process active: it is not idle, and there is a deadline to wake at.
    atms.activity_resumed(token_of(1, 0x21), true)
        .expect("resumed report");
    assert!(
        atms.idle_processes(Duration::from_secs(0)).is_empty(),
        "the resumed process is not idle"
    );
    assert!(
        atms.next_idle_deadline(Duration::from_secs(30)).is_none(),
        "a process holding a started activity has no idle deadline at all"
    );
    // It stops (B finished / A came back): now it is a cached process and the
    // launcher gets a wake time instead of polling.
    atms.activities[0].lifecycle = ActivityLifecycle::Stopped;
    assert_eq!(
        atms.idle_processes(Duration::from_secs(0))
            .iter()
            .map(|(tag, ..)| *tag)
            .collect::<Vec<_>>(),
        vec![1]
    );
    assert!(
        atms.next_idle_deadline(Duration::from_secs(30)).is_some(),
        "a cached process has an idle deadline the launcher can wake at"
    );
}

#[test]
fn process_death_drops_the_record_and_its_activities() {
    let mut atms = AtmsState::new();
    atms.process_started(1, "dev.acos.fixtures.procb", 10002);
    atms.activities.push(activity(
        1,
        token_of(1, 0x31),
        ActivityLifecycle::Started,
    ));
    atms.process_died(1);
    assert!(
        atms.process_list().is_empty(),
        "the process record is gone (stock removeProcessLocked)"
    );
    assert!(
        atms.activities.is_empty(),
        "its activity records went with it, so a later startActivity is a spawn again"
    );
}
