//! M3 frame test: render the hello-views DisplayList/Canvas path and score it
//! against the real `art-reference` screenshot with [`acos_render::diff`].
//!
//! The framework cannot yet reach drawing (M2 blocks in `bindApplication`
//! before `Application.onCreate`; see `results/m2/M2_LOG.md`), so this test
//! drives the **synthetic DisplayList/Canvas path** the renderer shares with the
//! framework: `Scene` → `DisplayOp` display list → `RenderContext` → surface →
//! RGBA. That verifies the renderer (geometry, text, blending, damage) in
//! isolation from the VM lifecycle.
//!
//! The scene is a reconstruction of the `hello-views` layout at the ART oracle's
//! 1080x2400 @ 420 dpi geometry. The numbers below are the *measured* diff, not
//! an aspirational target: the test asserts generous upper bounds so a
//! regression fails while a font/AA revision does not, and prints the exact
//! report so `results/render/M3_LOG.md` and `acos frame-diff` agree.

use std::path::PathBuf;

use acos_render::{diff, image, PngSetting, RenderContext, Scene};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn reference_png() -> Option<PathBuf> {
    let p =
        repo_root().join("fixtures/reference/hello-views/art-api34-x86_64/screenshot_initial.png");
    p.exists().then_some(p)
}

#[test]
fn hello_views_scene_diff_vs_art_reference() {
    let Some(ref_path) = reference_png() else {
        eprintln!(
            "SKIP hello_views_scene_diff_vs_art_reference: no reference at {}",
            ref_path_display()
        );
        return;
    };
    let scene_path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/scenes/hello-views.json");
    let scene = Scene::load(&scene_path).expect("load hello-views scene");

    let mut ctx = RenderContext::new();
    let out = scene.render(&mut ctx, 1).expect("render scene");

    // The synthetic path must draw every op; an unsupported op is a renderer
    // gap that must be fixed (or declared), never silently skipped.
    assert!(
        out.unsupported_ops.is_empty(),
        "scene rendered with unsupported ops: {:?}",
        out.unsupported_ops
    );
    assert_eq!(out.frame.width, 1080);
    assert_eq!(out.frame.height, 2400);

    let got_bytes = out.frame.png(PngSetting::FastFiltered);
    let got = image::decode_png(&got_bytes).expect("decode rendered PNG");
    let ref_bytes = std::fs::read(&ref_path).expect("read reference PNG");
    let reference = image::decode(&ref_bytes).expect("decode reference PNG");

    let report = diff::diff(&reference, &got, &diff::DiffOptions::default()).expect("diff");

    eprintln!(
        "hello-views synthetic vs ART reference: mean={:.4} rmse={:.4} max={} psnr={:.2} \
         differing={:.3}% bbox={:?} ssim mean={:.4} global={:.4} blocks_changed={:.2}%",
        report.mean_abs_error,
        report.rmse,
        report.max_abs_error,
        report.psnr_db.unwrap_or(f64::INFINITY),
        report.differing_fraction * 100.0,
        report.changed_bbox,
        report.mean_ssim,
        report.global_ssim,
        report.blocks.changed_fraction * 100.0,
    );
    eprintln!(
        "  full report: {}",
        serde_json::to_string(&report).expect("serialize report")
    );

    // W47-A target (results/render/M3_LOG.md §4 gap 6): the synthetic frame must
    // now match the ART capture to <5% differing pixels and SSIM > 0.99. The
    // scene reproduces the app content (bars, title, hint/underline, button,
    // gesture pill, action-bar shadow) and uses the Material surface colour.
    assert!(
        report.differing_fraction < 0.05,
        "differing pixels {:.3}% regressed past 5%",
        report.differing_fraction * 100.0
    );
    assert!(
        report.mean_abs_error < 6.0,
        "mean absolute error {:.4} regressed past 6",
        report.mean_abs_error
    );
    assert!(
        report.rmse < 20.0,
        "RMSE {:.4} regressed past 20",
        report.rmse
    );
    assert!(
        report.mean_ssim > 0.99,
        "mean SSIM {:.4} regressed below 0.99",
        report.mean_ssim
    );
    assert!(
        report.global_ssim > 0.99,
        "global SSIM {:.4} regressed below 0.99",
        report.global_ssim
    );
    // The renderer must not have produced an empty/placeholder frame.
    assert!(
        !report.identical,
        "synthetic frame must not be pixel-identical"
    );
    assert!(report.differing_pixels > 0);
}

fn ref_path_display() -> String {
    repo_root()
        .join("fixtures/reference/hello-views/art-api34-x86_64/screenshot_initial.png")
        .display()
        .to_string()
}

/// A pure DisplayList comparison with no framework/reference dependency: the
/// same node rendered twice must be bit-identical, and changing one op must
/// move every reported metric in the expected direction. This isolates the
/// rasteriser from scene parsing.
#[test]
fn displaylist_render_is_deterministic_and_sensitive() {
    use acos_render::geom::RectF;
    use acos_render::paint::Paint;
    use acos_render::render_node::{DisplayOp, RenderNode};

    fn render(with_overlay: bool) -> image::DecodedImage {
        let mut ctx = RenderContext::new();
        let base = ctx.paints.insert(Paint {
            color: 0xFF00_00FF,
            ..Paint::default()
        });
        let mut node = RenderNode::new("m3");
        node.left = 0;
        node.top = 0;
        node.right = 32;
        node.bottom = 16;
        node.display_list.push(DisplayOp::DrawColor(0xFFFF_FFFF));
        node.display_list
            .push(DisplayOp::DrawRect(RectF::new(0.0, 0.0, 16.0, 16.0), base));
        if with_overlay {
            let red = ctx.paints.insert(Paint {
                color: 0xFFFF_0000,
                ..Paint::default()
            });
            let mut overlay = RenderNode::new("overlay");
            overlay.left = 0;
            overlay.top = 0;
            overlay.right = 8;
            overlay.bottom = 8;
            overlay
                .display_list
                .push(DisplayOp::DrawRect(RectF::new(0.0, 0.0, 8.0, 8.0), red));
            let h2 = ctx.render_nodes.insert(overlay);
            node.children.push(h2);
        }
        let node = ctx.render_nodes.insert(node);
        let surface = ctx.surface_create(32, 16, 1.0).expect("surface");
        ctx.surface_render_node(surface, node, 1).expect("render");
        let rgba = ctx.surface_rgba8(surface).expect("rgba8");
        diff::decoded_from_rgba8(32, 16, &rgba).expect("decoded")
    }

    let a = render(false);
    let b = render(false);
    let same = diff::diff(&a, &b, &diff::DiffOptions::default()).expect("diff");
    assert!(same.identical, "deterministic render must be identical");

    let c = render(true);
    let changed = diff::diff(&a, &c, &diff::DiffOptions::default()).expect("diff");
    assert!(!changed.identical, "adding an overlay must change pixels");
    assert!(changed.differing_pixels > 0);
    assert!(changed.mean_abs_error > 0.0);
}
