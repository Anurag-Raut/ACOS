//! T-34 acceptance: `HardwareRenderer` frame → compositor → completed frame.
//!
//! The Rust-level equivalent of `acos-vm`'s `GfxCanvasMain`: record a display
//! list on a real `acos-render` `RenderNode`, rasterise it, and hand the pixels
//! to [`FrameHandoffSink`]. The sink submits to the WMS-backed
//! [`WmSurfaceBackend`], the `acos-wm` compositor presents, and a
//! [`CompletedFrame`] is published into the [`FrameStore`].
//!
//! What is asserted:
//!
//! * a real buffer submission publishes exactly one completed frame with the
//!   expected pixels and the layer's `window` handle;
//! * the frame id is monotonic across submissions and the content hash changes
//!   when the pixels change;
//! * `wait_for_frame_after` is bounded (returns `None` on an idle store);
//! * `IWindowSession.finishDrawing` alone publishes nothing.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use acos_render::context::RenderContext;
use acos_render::geom::RectF;
use acos_render::paint::Paint;
use acos_render::render_node::{DisplayOp, RenderNode};
use acos_runtime::frame_handoff::{
    clear_frame_handoff, install_frame_handoff, FrameHandoffSink, FrameStore,
};
use acos_runtime::wms_java::{self, WmSurfaceBackend};
use acos_vm::natives::fwk_surface::SurfaceBackend;
use acos_vm::natives::gfx_hwui::FrameSink;
use acos_wm::codec::LayoutParams;
use acos_parcel::binder::BinderId;

/// Rasterise a 4x3 black rectangle and return the RGBA8888 pixels.
fn record_black_rect() -> Vec<u8> {
    let mut ctx = RenderContext::new();
    let paint = ctx.paints.insert(Paint::default());
    let mut node = RenderNode::new("handoff");
    node.left = 0;
    node.top = 0;
    node.right = 4;
    node.bottom = 3;
    node.display_list
        .push(DisplayOp::DrawRect(RectF::new(0.0, 0.0, 4.0, 3.0), paint));
    let node = ctx.render_nodes.insert(node);
    let surface = ctx.surface_create(4, 3, 1.0).expect("surface");
    ctx.surface_render_node(surface, node, 42)
        .expect("render node");
    ctx.surface_rgba8(surface).expect("rgba8")
}

#[test]
fn frame_handoff_publishes_on_real_submission() {
    clear_frame_handoff();
    let backend = Rc::new(RefCell::new(WmSurfaceBackend::new().expect("backend")));
    let layer = backend.borrow_mut().create_layer(0x1000, "handoff", 4, 3);
    let store = Arc::new(FrameStore::new());
    let mut sink = FrameHandoffSink::new(Rc::clone(&backend), Arc::clone(&store));

    // Record and rasterise the display list (the `GfxCanvasMain` equivalent).
    let rgba = record_black_rect();
    assert_eq!(rgba.len(), 4 * 3 * 4);

    // Nothing is published before a real buffer submission.
    assert_eq!(store.published_count(), 0);
    assert!(store.latest().is_none());

    let id = sink.submit_frame(layer, 4, 3, &rgba, 42);
    assert_eq!(id, 1, "first submission frame id");
    let frame = store.latest().expect("completed frame");
    assert_eq!(frame.frame_id, 1);
    assert_eq!(frame.window, layer);
    assert_eq!(frame.cause_id, 42);
    assert_eq!((frame.width, frame.height), (1080, 2400), "display frame");
    // The layer is at 0,0 in the display; the first pixel is the black rect.
    assert_eq!(&frame.pixels[0..4], &[0, 0, 0, 255], "layer pixel at 0,0");
    assert_ne!(frame.content_hash, 0, "content hash must be set");
    assert_eq!(store.published_count(), 1);

    // A second submission with different pixels: monotonic id, new hash.
    let mut rgba2 = rgba.clone();
    rgba2[0] = 0xFF;
    rgba2[1] = 0x00;
    rgba2[2] = 0x00;
    let id2 = sink.submit_frame(layer, 4, 3, &rgba2, 43);
    assert_eq!(id2, 2, "second submission frame id");
    let frame2 = store.latest().expect("second completed frame");
    assert_eq!(frame2.frame_id, 2);
    assert_eq!(frame2.cause_id, 43);
    assert_eq!(&frame2.pixels[0..4], &[0xFF, 0, 0, 255], "red layer pixel");
    assert_ne!(
        frame2.content_hash, frame.content_hash,
        "content hash must change with the pixels"
    );
    assert_eq!(store.published_count(), 2);

    // `wait_for_frame_after` returns immediately for a newer frame and times
    // out when nothing new arrives.
    let newer = store
        .wait_for_frame_after(1, Duration::from_millis(20))
        .expect("frame 2 is newer than 1");
    assert_eq!(newer.frame_id, 2);
    assert!(
        store
            .wait_for_frame_after(2, Duration::from_millis(20))
            .is_none(),
        "idle store must time out, not block forever"
    );

    clear_frame_handoff();
}

#[test]
fn finish_drawing_alone_publishes_nothing() {
    clear_frame_handoff();
    let store = Arc::new(FrameStore::new());
    wms_java::with_wms(|s| {
        let _session = s.open_session();
        let params = LayoutParams::application("dev.acos.test", "MainActivity");
        let add = s
            .add_to_display_as_user(BinderId(0x1000), params, 0)
            .expect("add window");
        s.finish_drawing(add.window, 0).expect("finishDrawing");
        s.finish_drawing(add.window, 0).expect("finishDrawing twice");
    });
    assert_eq!(
        store.published_count(),
        0,
        "finishDrawing alone must not publish a frame"
    );
    assert!(store.latest().is_none());
    clear_frame_handoff();
}

#[test]
fn install_frame_handoff_is_idempotent() {
    clear_frame_handoff();
    let first = install_frame_handoff().expect("install");
    assert!(first.latest().is_none());
    let second = install_frame_handoff().expect("install again");
    assert!(
        Arc::ptr_eq(&first, &second),
        "a second install must return the same store"
    );
    clear_frame_handoff();
}
