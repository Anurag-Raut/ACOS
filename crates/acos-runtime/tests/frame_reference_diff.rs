//! M3 frame pipeline test: synthetic DisplayList/Canvas → WMS compositor →
//! completed-frame [`FrameStore`], scored against the ART reference screenshot.
//!
//! This is the end-to-end portion of the M3 frame test that does **not** need
//! the VM lifecycle: the `hello-views` display list is rasterised by
//! `acos-render`, submitted through the same `WmSurfaceBackend` the
//! `HardwareRenderer` frame handoff installs, composited, and published into the
//! [`FrameStore`] a screenshot/observe consumer reads.
//!
//! The framework-bound path (`ViewRootImpl` → `HardwareRenderer.nSyncAndDrawFrame`
//! → this sink) is still blocked upstream in M2 (`Application.onCreate` is not
//! reached), so this test verifies the renderer + handoff + store boundary
//! independently and records the gap in `results/render/M3_LOG.md`.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use acos_render::{diff, image, RenderContext, Scene};
use acos_runtime::frame_handoff::{clear_frame_handoff, FrameHandoffSink, FrameStore};
use acos_runtime::wms_java::WmSurfaceBackend;
use acos_vm::natives::fwk_surface::SurfaceBackend;
use acos_vm::natives::gfx_hwui::FrameSink;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Rasterise the hello-views scene and return `(width, height, rgba)`.
fn render_hello_views() -> (u32, u32, Vec<u8>) {
    let scene_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../acos-render/tests/scenes/hello-views.json");
    let scene = Scene::load(&scene_path).expect("load hello-views scene");
    let mut ctx = RenderContext::new();
    let out = scene.render(&mut ctx, 7).expect("render scene");
    assert!(
        out.unsupported_ops.is_empty(),
        "unsupported ops on the synthetic frame path: {:?}",
        out.unsupported_ops
    );
    let rgba = out.frame.rgba8().to_vec();
    (out.frame.width, out.frame.height, rgba)
}

#[test]
fn synthetic_frame_reaches_frame_store_and_matches_reference() {
    clear_frame_handoff();
    let backend = Rc::new(RefCell::new(WmSurfaceBackend::new().expect("backend")));
    let layer = backend
        .borrow_mut()
        .create_layer(0x2000, "hello-views", 1080, 2400);
    let store = Arc::new(FrameStore::new());
    let mut sink = FrameHandoffSink::new(Rc::clone(&backend), Arc::clone(&store));

    let (w, h, rgba) = render_hello_views();
    assert_eq!((w, h), (1080, 2400));

    assert_eq!(store.published_count(), 0, "nothing before a real submit");
    let id = sink.submit_frame(layer, w, h, &rgba, 7);
    assert_eq!(id, 1, "first compositor frame id");
    let frame = store.latest().expect("completed frame");

    assert_eq!(frame.frame_id, 1);
    assert_eq!(frame.window, layer);
    assert_eq!(frame.cause_id, 7);
    assert_eq!((frame.width, frame.height), (1080, 2400));
    assert_ne!(frame.content_hash, 0, "content hash must be set");
    assert_eq!(store.published_count(), 1);
    // The compositor output is opaque; alpha must be fully set.
    assert!(frame.pixels.chunks_exact(4).all(|p| p[3] == 0xFF));

    let Some(reference) = load_reference() else {
        eprintln!("SKIP reference comparison: no ART reference screenshot");
        clear_frame_handoff();
        return;
    };
    let candidate = diff::decoded_from_rgba8(frame.width, frame.height, &frame.pixels)
        .expect("decode compositor frame");
    let report = diff::diff(&reference, &candidate, &diff::DiffOptions::default())
        .expect("diff compositor frame vs reference");
    eprintln!(
        "frame-store vs ART reference: mean={:.4} rmse={:.4} ssim mean={:.4} global={:.4} \
         differing={:.3}%",
        report.mean_abs_error,
        report.rmse,
        report.mean_ssim,
        report.global_ssim,
        report.differing_fraction * 100.0,
    );
    assert!(
        report.mean_abs_error < 12.0,
        "mean error {:.4} regressed past 12",
        report.mean_abs_error
    );
    assert!(
        report.mean_ssim > 0.90,
        "mean SSIM {:.4} regressed below 0.90",
        report.mean_ssim
    );

    clear_frame_handoff();
}

fn load_reference() -> Option<image::DecodedImage> {
    let p =
        repo_root().join("fixtures/reference/hello-views/art-api34-x86_64/screenshot_initial.png");
    let bytes = std::fs::read(&p).ok()?;
    image::decode(&bytes).ok()
}
