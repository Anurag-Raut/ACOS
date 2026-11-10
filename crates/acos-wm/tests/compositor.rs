//! Compositor + frame-contract integration tests through `SessionWm`.

use std::sync::Arc;

use acos_parcel::binder::BinderId;
use acos_render::{Bitmap, BitmapConfig};
use acos_wm::clock::VirtualClock;
use acos_wm::codec::LayoutParams;
use acos_wm::display::DisplaySpec;
use acos_wm::facade::SessionWm;
use acos_wm::geom::RectI;

fn solid(w: u32, h: u32, argb: u32) -> Bitmap {
    let mut b = Bitmap::new(w, h, BitmapConfig::Rgba8888).unwrap();
    b.erase(argb);
    b
}

fn session() -> SessionWm {
    SessionWm::with_clock(
        DisplaySpec::reference_1080x2400(),
        Arc::new(VirtualClock::new()),
    )
    .unwrap()
}

#[test]
fn finish_drawing_produces_a_fresh_frame_with_cause() {
    let mut s = session();
    let w = BinderId(0x4001);
    s.add_window(w, LayoutParams::application("pkg", "Main"), 0)
        .unwrap();
    s.relayout(w.0, LayoutParams::application("pkg", "Main"), -1)
        .unwrap();
    assert!(s.capture().is_none(), "no frame before a draw");

    let seq = s
        .finish_drawing(w.0, solid(1080, 2400, 0xff10_2030), 1, 55)
        .unwrap();
    assert_eq!(seq, Some(1));
    let f = s.capture_fresh(0).unwrap();
    assert_eq!(f.frame_seq(), 1);
    assert_eq!(f.cause_id, 55);
    assert_eq!(f.frame.width, 1080);
    assert_eq!(f.frame.height, 2400);
    assert!(!f.content_hash().is_empty());
    assert!(!f.sha256().is_empty());
    assert!(f.label().contains("cause 55"));

    // A capture after seq 1 must refuse until a new draw.
    assert!(s.capture_fresh(1).is_err());
    let seq2 = s
        .finish_drawing(w.0, solid(1080, 2400, 0xffee_0000), 2, 56)
        .unwrap();
    assert_eq!(seq2, Some(2), "frame_seq is monotonic");
    let f2 = s.capture_fresh(1).unwrap();
    assert_eq!(f2.cause_id, 56);
    assert_ne!(f.content_hash(), f2.content_hash());
}

#[test]
fn two_windows_z_order_and_opaque_overlap() {
    let mut s = session();
    let a = BinderId(0x4001);
    let b = BinderId(0x4002);
    s.add_window(a, LayoutParams::application("pkg", "A"), 0)
        .unwrap();
    s.add_window(b, LayoutParams::application("pkg", "B"), 0)
        .unwrap();
    s.relayout(a.0, LayoutParams::application("pkg", "A"), -1)
        .unwrap();
    s.relayout(b.0, LayoutParams::application("pkg", "B"), -1)
        .unwrap();

    // Full-screen red for A, a blue square for B on top.
    s.finish_drawing(a.0, solid(1080, 2400, 0xffff_0000), 1, 1)
        .unwrap();
    // Resize B's layer through the compositor transaction model.
    let layer_b = s.compositor.layer_for_window(b.0).unwrap();
    s.compositor
        .set_buffer(layer_b, solid(200, 200, 0xff00_00ff), 2, 2)
        .unwrap();
    s.compositor
        .apply(&acos_wm::compositor::Transaction::SetPosition {
            layer: layer_b,
            position: acos_wm::geom::PointI::new(100, 100),
        })
        .unwrap();
    let seq = s.compositor.present(2).unwrap().unwrap();
    let frame = s.compositor.capture().unwrap();
    assert_eq!(seq, frame.seq);
    let px = |x: u32, y: u32| {
        let p = frame.pixels.pixel(x, y).unwrap();
        (p.red(), p.green(), p.blue(), p.alpha())
    };
    assert_eq!(px(50, 50), (255, 0, 0, 255), "red outside the blue square");
    assert_eq!(px(150, 150), (0, 0, 255, 255), "blue on top");
}

#[test]
fn remove_window_drops_layer_and_input_route() {
    let mut s = session();
    let w = BinderId(0x4001);
    s.add_window(w, LayoutParams::application("pkg", "Main"), 0)
        .unwrap();
    assert!(s.compositor.layer_for_window(w.0).is_some());
    s.remove_window(w.0).unwrap();
    assert!(s.compositor.layer_for_window(w.0).is_none());
    assert!(!s.wm.windows.contains_key(&w.0));
}

#[test]
fn window_frame_is_the_measured_app_bounds() {
    let mut s = session();
    let w = BinderId(0x4001);
    s.add_window(w, LayoutParams::application("pkg", "Main"), 0)
        .unwrap();
    let r = s
        .relayout(w.0, LayoutParams::application("pkg", "Main"), -1)
        .unwrap();
    assert_eq!(r.frames.frame, RectI::new(0, 128, 1080, 2337));
    assert_eq!(r.frames.display_frame, RectI::new(0, 0, 1080, 2400));
}
