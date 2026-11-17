//! Multi-session isolation: two concurrent `SessionWm`s must never observe each
//! other's input, windows or frames.

use std::sync::Arc;

use acos_parcel::binder::BinderId;
use acos_wm::clock::VirtualClock;
use acos_wm::codec::LayoutParams;
use acos_wm::display::DisplaySpec;
use acos_wm::facade::SessionWm;
use acos_wm::input::InputMessage;

fn session(clock: Arc<VirtualClock>) -> SessionWm {
    SessionWm::with_clock(DisplaySpec::reference_1080x2400(), clock).unwrap()
}

#[test]
fn concurrent_sessions_do_not_share_events() {
    // One process-wide virtual clock, two independent workers.
    let clock = Arc::new(VirtualClock::new());
    let mut a = session(clock.clone());
    let mut b = session(clock.clone());

    a.add_window(BinderId(1), LayoutParams::application("pkg", "A"), 0)
        .unwrap();
    b.add_window(BinderId(2), LayoutParams::application("pkg", "B"), 0)
        .unwrap();

    // Both stacks route a tap at the same coordinates to their own window.
    let oa = a.tap(100.0, 400.0, 1).unwrap();
    assert_eq!(oa.delivered_to, Some(1));
    assert_eq!(a.input.focus_for(a.id), Some(1));
    assert_eq!(b.input.focus_for(b.id), Some(2));

    // Only A's channel has messages.
    let ha = a.wm.windows.get(&1).unwrap().input_channel_handle;
    let hb = b.wm.windows.get(&2).unwrap().input_channel_handle;
    let app_a = a.wm.channels.get(ha).unwrap().app.clone();
    let app_b = b.wm.channels.get(hb).unwrap().app.clone();
    assert!(InputMessage::decode(&app_a.recv().unwrap().unwrap()).is_ok());
    assert!(
        app_b.recv().unwrap().is_none(),
        "session B must not observe session A's tap"
    );

    // The two dispatchers keep independent pending/ack accounting.
    assert!(!a.input.is_idle());
    assert!(b.input.is_idle());
    for seq in oa.seqs {
        a.ack(seq).unwrap();
    }
    assert!(a.input.is_idle());
}

#[test]
fn concurrent_sessions_keep_independent_frames() {
    let clock = Arc::new(VirtualClock::new());
    let mut a = session(clock.clone());
    let mut b = session(clock.clone());
    a.add_window(BinderId(1), LayoutParams::application("pkg", "A"), 0)
        .unwrap();
    b.add_window(BinderId(2), LayoutParams::application("pkg", "B"), 0)
        .unwrap();

    let mut red =
        acos_render::Bitmap::new(1080, 2400, acos_render::BitmapConfig::Rgba8888).unwrap();
    red.erase(0xffff_0000);
    let mut green =
        acos_render::Bitmap::new(1080, 2400, acos_render::BitmapConfig::Rgba8888).unwrap();
    green.erase(0xff00_ff00);

    a.finish_drawing(1, red, 1, 11).unwrap();
    b.finish_drawing(2, green, 1, 22).unwrap();

    let fa = a.capture_fresh(0).unwrap();
    let fb = b.capture_fresh(0).unwrap();
    assert_eq!(fa.cause_id, 11);
    assert_eq!(fb.cause_id, 22);
    assert_ne!(fa.content_hash(), fb.content_hash());
}
