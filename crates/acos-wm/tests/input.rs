//! Input dispatch integration tests through `SessionWm`.

use std::sync::Arc;

use acos_parcel::binder::BinderId;
use acos_wm::clock::{Clock, VirtualClock};
use acos_wm::codec::LayoutParams;
use acos_wm::display::DisplaySpec;
use acos_wm::facade::SessionWm;
use acos_wm::ime::EditorInfo;
use acos_wm::input::{InputMessage, ACTION_DOWN, ACTION_MOVE, ACTION_UP};

fn session(clock: Arc<VirtualClock>) -> SessionWm {
    SessionWm::with_clock(DisplaySpec::reference_1080x2400(), clock).unwrap()
}

fn add_full_window(s: &mut SessionWm, id: u64) {
    let w = BinderId(id);
    s.add_window(w, LayoutParams::application("pkg", "Main"), 0)
        .unwrap();
}

#[test]
fn tap_is_hit_tested_delivered_and_acked() {
    let clock = Arc::new(VirtualClock::new());
    let mut s = session(clock.clone());
    add_full_window(&mut s, 0x4001);

    assert_eq!(s.tap(540.0, 427.0, 42).unwrap().delivered_to, Some(0x4001));
    // The app end reads the DOWN then the UP.
    let handle = s.wm.windows.get(&0x4001).unwrap().input_channel_handle;
    let app = s.wm.channels.get(handle).unwrap().app.clone();
    let down = InputMessage::decode(&app.recv().unwrap().unwrap()).unwrap();
    let up = InputMessage::decode(&app.recv().unwrap().unwrap()).unwrap();
    assert_eq!(down.cause_id, 42);
    if let acos_wm::input::InputEvent::Motion(m) = &down.event {
        assert_eq!(m.action, ACTION_DOWN);
        assert_eq!(m.pointers[0].x, 540.0);
        assert_eq!(m.pointers[0].y, 427.0);
    }
    if let acos_wm::input::InputEvent::Motion(m) = &up.event {
        assert_eq!(m.action, ACTION_UP);
    }

    // Ack both; the ANR timer disarms once nothing is outstanding.
    assert!(s.input.next_deadline().is_some());
    s.ack(down.seq).unwrap();
    s.ack(up.seq).unwrap();
    assert!(s.input.is_idle());
    assert_eq!(s.input.acks().len(), 2);
    assert!(s.input.acks()[0].latency_nanos >= 0);
}

#[test]
fn focus_change_reports_to_the_new_window() {
    let clock = Arc::new(VirtualClock::new());
    let mut s = session(clock);
    add_full_window(&mut s, 0x4001);
    assert_eq!(s.input.focus_for(s.id), Some(0x4001));
    s.input.set_focus(s.id, 0x4001);
    let seq = s.input.inject_focus(s.id, true).unwrap();
    s.ack(seq).unwrap();
    assert!(s.input.is_idle());
}

#[test]
fn swipe_produces_down_moves_up_in_order() {
    let clock = Arc::new(VirtualClock::new());
    let mut s = session(clock);
    add_full_window(&mut s, 0x4001);
    let outcome = s
        .swipe((100.0, 200.0), (500.0, 900.0), 100_000_000, 3, 7)
        .unwrap();
    assert_eq!(outcome.seqs.len(), 5);
    let handle = s.wm.windows.get(&0x4001).unwrap().input_channel_handle;
    let app = s.wm.channels.get(handle).unwrap().app.clone();
    let mut actions = Vec::new();
    for _ in 0..5 {
        let m = InputMessage::decode(&app.recv().unwrap().unwrap()).unwrap();
        if let acos_wm::input::InputEvent::Motion(me) = m.event {
            actions.push(me.action);
        }
    }
    assert_eq!(
        actions,
        vec![
            ACTION_DOWN,
            ACTION_MOVE,
            ACTION_MOVE,
            ACTION_MOVE,
            ACTION_UP
        ]
    );
    for seq in outcome.seqs {
        s.ack(seq).unwrap();
    }
}

#[test]
fn type_text_reaches_the_registered_input_connection() {
    let clock = Arc::new(VirtualClock::new());
    let mut s = session(clock);
    add_full_window(&mut s, 0x4001);
    let client = BinderId(0x5001);
    let connection = BinderId(0x6001);
    let field = Arc::new(acos_wm::ime::StringInputConnection::new());
    s.register_input_connection(connection, field.clone());
    s.bind_text_field(
        client,
        connection,
        EditorInfo {
            input_type: 1,
            package_name: Some("pkg".into()),
            ..EditorInfo::default()
        },
    )
    .unwrap();
    let outcome = s.type_text(client, "World", 9).unwrap();
    assert_eq!(outcome.cause_id, 9);
    assert_eq!(field.text(), "World");
}

#[test]
fn idle_input_has_no_wakeups() {
    let clock = Arc::new(VirtualClock::new());
    let s = session(clock.clone());
    let _ = &s;
    clock.advance(5_000_000_000);
    assert_eq!(clock.arm_count(), 0, "no timer while idle");
    assert!(!clock.timer_armed());
}

#[test]
fn anr_is_detected_only_while_unacked() {
    let clock = Arc::new(VirtualClock::new());
    let mut s = session(clock.clone());
    add_full_window(&mut s, 0x4001);
    let seqs = s.tap(100.0, 400.0, 1).unwrap().seqs;
    clock.advance(acos_wm::input::DEFAULT_ANR_TIMEOUT_NANOS + 1);
    let anrs = s.pump_anr();
    assert!(!anrs.is_empty());
    for seq in seqs {
        s.ack(seq).unwrap();
    }
    assert!(s.input.is_idle());
}
