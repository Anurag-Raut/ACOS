//! Vsync generator integration tests through `SessionWm`.

use std::sync::Arc;

use acos_wm::clock::{Clock, VirtualClock};
use acos_wm::display::{DisplaySpec, FRAME_INTERVAL_NANOS};
use acos_wm::facade::SessionWm;

fn session() -> (Arc<VirtualClock>, SessionWm) {
    let clock = Arc::new(VirtualClock::new());
    let s = SessionWm::with_clock(DisplaySpec::reference_1080x2400(), clock.clone()).unwrap();
    (clock, s)
}

#[test]
fn idle_vsync_has_zero_wakeups() {
    let (clock, mut s) = session();
    assert_eq!(clock.arm_count(), 0);
    clock.advance(5_000_000_000);
    assert!(s.pump_vsync().is_empty());
    assert_eq!(clock.arm_count(), 0, "idle => no timer, no wakeup");
    assert!(s.vsync.is_idle());
}

#[test]
fn deterministic_60hz_sequence() {
    let (clock, mut s) = session();
    s.vsync.schedule_vsync(1);
    assert_eq!(s.vsync.next_deadline(), Some(FRAME_INTERVAL_NANOS));
    assert!(clock.timer_armed());
    clock.advance(FRAME_INTERVAL_NANOS);
    let d = s.pump_vsync();
    assert_eq!(d.len(), 1);
    assert_eq!(d[0].data.vsync_id, 0);
    assert_eq!(d[0].timestamp_nanos, FRAME_INTERVAL_NANOS);
    assert!(!clock.timer_armed());

    s.vsync.schedule_vsync(1);
    s.vsync.schedule_vsync(2);
    clock.advance(FRAME_INTERVAL_NANOS);
    let d = s.pump_vsync();
    assert_eq!(d.len(), 2, "both clients get the shared deadline");
    assert_eq!(d[0].data.vsync_id, 1);
    assert_eq!(clock.arm_count(), 2, "one arm per scheduled deadline");
    assert!(s.vsync.is_idle());
}

#[test]
fn vrr_refresh_rate_is_queryable() {
    let (_clock, mut s) = session();
    assert!((s.vsync.refresh_rate() - 60.000004).abs() < 1e-3);
    s.vsync.set_refresh_rate(120.0);
    assert_eq!(s.vsync.refresh_rate(), 120.0);
    s.vsync.schedule_vsync(1);
    assert_eq!(s.vsync.next_deadline(), Some(8_333_333));
}

#[test]
fn pipe_record_has_the_dispatch_vsync_shape() {
    let (clock, mut s) = session();
    s.vsync.schedule_vsync(1);
    clock.advance(FRAME_INTERVAL_NANOS);
    let d = &s.pump_vsync()[0];
    let bytes = d.encode_pipe_record();
    // 3 scalars + frameInterval + vsyncId + preferredIndex + count = 44, plus
    // one 40-byte FrameTimeline.
    assert_eq!(bytes.len(), 84);
}
