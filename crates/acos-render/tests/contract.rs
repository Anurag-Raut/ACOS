//! End-to-end contract tests: frame freshness, damage tracking, unsupported-op
//! reporting, error codes and a deterministic fuzz sweep.

use acos_render::{BitmapConfig, ClipOp, PngSetting, RectF, RenderContext, RenderError};

fn scene_json() -> String {
    r##"{
      "width": 200, "height": 120, "density": 1.0, "background": "#FFFFFFFF",
      "root": { "name": "r", "bounds": [0,0,200,120],
        "ops": [ {"op":"rect","rect":[10,10,60,60],"paint":{"color":"#FFFF0000"}} ] }
    }"##
    .to_string()
}

#[test]
fn frame_freshness_contract() {
    let scene = acos_render::Scene::from_json(&scene_json()).unwrap();
    let mut ctx = RenderContext::new();
    let out = scene.render(&mut ctx, 7).unwrap();
    assert_eq!(out.seq, 1);
    // capture_fresh(after=0) returns seq 1.
    // We re-find the surface via a fresh render context rather than exposing
    // internals: use the public capture via a new context is not possible, so
    // assert through SceneRender metadata.
    assert_eq!(out.frame.cause_id, 7);
    assert_eq!(out.frame.seq, 1);
    assert!(!out.frame.hash_hex().is_empty());
    let class = out.frame_class;
    assert_eq!(class, acos_render::FrameClass::Content);
}

#[test]
fn damage_tracking_skips_raster_when_unchanged() {
    let scene = acos_render::Scene::from_json(&scene_json()).unwrap();
    let mut ctx = RenderContext::new();
    let surface = ctx
        .surface_create(scene.width, scene.height, scene.density)
        .unwrap();
    let (root, _) = scene.build_tree(&mut ctx, &scene.root).unwrap();
    let seq1 = ctx.surface_render_node(surface, root, 1).unwrap();
    let ops_after_first = ctx.surfaces.get(surface).unwrap().raster_ops;
    assert!(ops_after_first > 0, "first frame should rasterise");

    // Unchanged tree: no raster work, but a fresh frame sequence.
    let seq2 = ctx.surface_render_node(surface, root, 2).unwrap();
    assert_eq!(seq2, seq1 + 1);
    assert_eq!(
        ctx.surfaces.get(surface).unwrap().raster_ops,
        ops_after_first
    );

    // Touch the tree: raster work resumes.
    ctx.render_node_set_alpha(root, 0.999).unwrap();
    let seq3 = ctx.surface_render_node(surface, root, 3).unwrap();
    assert_eq!(seq3, seq2 + 1);
    assert!(ctx.surfaces.get(surface).unwrap().raster_ops > ops_after_first);
}

#[test]
fn unsupported_ops_are_recorded_not_skipped() {
    let mut ctx = RenderContext::new();
    let surface = ctx.surface_create(64, 64, 1.0).unwrap();
    ctx.canvas_for_surface(surface).unwrap();
    let val = ctx.matrix_create();
    let path = ctx.path_create();
    let other = ctx.path_create();
    ctx.path_add_rect(path, RectF::new(0.0, 0.0, 10.0, 10.0), false)
        .unwrap();
    ctx.path_add_rect(other, RectF::new(5.0, 5.0, 15.0, 15.0), false)
        .unwrap();
    // Path.op is explicitly unsupported.
    let ok = ctx.path_op(path, other, 2).unwrap();
    assert!(!ok);
    assert!(ctx.unsupported_ops().iter().any(|o| o.contains("Path.op")));
    let _ = val;
}

#[test]
fn strict_mode_escalates_unsupported() {
    let mut ctx = RenderContext::new();
    ctx.strict = true;
    let path = ctx.path_create();
    let other = ctx.path_create();
    let err = ctx.path_op(path, other, 2).unwrap_err();
    assert_eq!(err.code(), "RENDER_STRICT");
}

#[test]
fn bad_handle_reports_stable_code() {
    let mut ctx = RenderContext::new();
    let err = ctx.bitmap_erase(acos_render::Handle::NULL, 0).unwrap_err();
    assert_eq!(err.code(), "RENDER_BAD_HANDLE");
    assert_eq!(err, RenderError::BadHandle);
}

#[test]
fn capture_fresh_semantics() {
    let mut ctx = RenderContext::new();
    let surface = ctx.surface_create(32, 32, 1.0).unwrap();
    // Nothing presented yet.
    let err = ctx.surface_capture_fresh(surface, 0).unwrap_err();
    assert_eq!(err.code(), "NO_FRESH_FRAME");
    ctx.surface_present(surface, 1).unwrap();
    assert!(ctx.surface_capture_fresh(surface, 0).is_ok());
    let err = ctx.surface_capture_fresh(surface, 1).unwrap_err();
    assert_eq!(err.code(), "NO_FRESH_FRAME");
    ctx.surface_present(surface, 2).unwrap();
    assert!(ctx.surface_capture_fresh(surface, 1).is_ok());
}

#[test]
fn save_layer_alpha_composites() {
    let mut ctx = RenderContext::new();
    let surface = ctx.surface_create(20, 20, 1.0).unwrap();
    ctx.canvas_for_surface(surface).unwrap();
    // Red background.
    ctx.canvas_draw_color(surface, 0xffff0000).unwrap();
    // Blue 50% layer.
    ctx.canvas_save_layer_alpha(surface, None, 128).unwrap();
    ctx.canvas_draw_color(surface, 0xff0000ff).unwrap();
    ctx.canvas_restore(surface).unwrap();
    ctx.surface_present(surface, 1).unwrap();
    let f = ctx.surface_capture(surface).unwrap().unwrap();
    let p = f.pixels.pixel(10, 10).unwrap();
    // Half red + half blue.
    assert!((p.red() as i32 - 128).abs() < 12, "r={}", p.red());
    assert!((p.blue() as i32 - 128).abs() < 12, "b={}", p.blue());
}

#[test]
fn clip_rect_difference_clips_drawing() {
    let mut ctx = RenderContext::new();
    let surface = ctx.surface_create(40, 40, 1.0).unwrap();
    ctx.canvas_for_surface(surface).unwrap();
    ctx.canvas_clip_rect(
        surface,
        RectF::new(0.0, 0.0, 20.0, 40.0),
        ClipOp::Difference,
        false,
    )
    .unwrap();
    ctx.canvas_draw_color(surface, 0xff00ff00).unwrap();
    ctx.surface_present(surface, 1).unwrap();
    let f = ctx.surface_capture(surface).unwrap().unwrap();
    assert_eq!(f.pixels.pixel(5, 5).unwrap().alpha(), 0);
    assert!(f.pixels.pixel(30, 5).unwrap().alpha() > 0);
}

#[test]
fn bitmap_pixel_round_trip_through_context() {
    let mut ctx = RenderContext::new();
    let b = ctx.bitmap_create(4, 4, BitmapConfig::Rgba8888).unwrap();
    ctx.bitmap_erase(b, 0xff336699).unwrap();
    assert_eq!(ctx.bitmap_get_pixel(b, 0, 0).unwrap(), 0xff336699);
    ctx.bitmap_set_pixel(b, 1, 1, 0x80123456).unwrap();
    assert_eq!(ctx.bitmap_get_pixel(b, 1, 1).unwrap(), 0x80123456);
    assert_eq!(ctx.bitmap_get_pixel(b, 9, 9).unwrap(), 0);
}

#[test]
fn render_error_codes_are_stable() {
    let errors = [
        (RenderError::BadHandle, "RENDER_BAD_HANDLE"),
        (
            RenderError::Unsupported { op: "x".into() },
            "RENDER_UNSUPPORTED_OP",
        ),
        (
            RenderError::FontNotFound { name: "x".into() },
            "RENDER_FONT_NOT_FOUND",
        ),
        (
            RenderError::NoFreshFrame {
                after: 1,
                latest: 0,
            },
            "NO_FRESH_FRAME",
        ),
        (
            RenderError::InvalidArgument { msg: "x".into() },
            "RENDER_INVALID_ARGUMENT",
        ),
        (RenderError::Size { msg: "x".into() }, "RENDER_SIZE"),
        (RenderError::Decode { msg: "x".into() }, "RENDER_DECODE"),
        (RenderError::Io { msg: "x".into() }, "RENDER_IO"),
        (RenderError::Strict { msg: "x".into() }, "RENDER_STRICT"),
    ];
    for (e, code) in errors {
        assert_eq!(e.code(), code);
    }
}

/// Deterministic LCG so the fuzz test is reproducible.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0 >> 33
    }
    fn f(&mut self, lo: f32, hi: f32) -> f32 {
        let t = (self.next() % 1000) as f32 / 1000.0;
        lo + (hi - lo) * t
    }
}

#[test]
fn fuzz_random_op_sequences_never_panic() {
    let mut rng = Rng(1);
    for seed in 0..12 {
        rng.0 = 0x9e37_79b9_7f4a_7c15u64.wrapping_mul(seed as u64 + 1);
        let mut ctx = RenderContext::new();
        let surface = ctx.surface_create(64, 64, 1.0).unwrap();
        ctx.canvas_for_surface(surface).unwrap();
        let paint = ctx.paint_create();
        ctx.paint_set_anti_alias(paint, true).unwrap();
        for _ in 0..200 {
            match rng.next() % 10 {
                0 => {
                    let r = RectF::new(
                        rng.f(-20.0, 60.0),
                        rng.f(-20.0, 60.0),
                        rng.f(20.0, 100.0),
                        rng.f(20.0, 100.0),
                    );
                    let _ = ctx.canvas_draw_rect(surface, r, paint);
                }
                1 => {
                    let _ = ctx.canvas_draw_circle(
                        surface,
                        rng.f(0.0, 64.0),
                        rng.f(0.0, 64.0),
                        rng.f(1.0, 40.0),
                        paint,
                    );
                }
                2 => {
                    let _ = ctx.canvas_translate(surface, rng.f(-10.0, 10.0), rng.f(-10.0, 10.0));
                }
                3 => {
                    let _ = ctx.canvas_save(surface);
                }
                4 => {
                    let _ = ctx.canvas_restore(surface);
                }
                5 => {
                    let _ = ctx.canvas_clip_rect(
                        surface,
                        RectF::new(0.0, 0.0, 32.0, 32.0),
                        ClipOp::Intersect,
                        true,
                    );
                }
                6 => {
                    let path = ctx.path_create();
                    let _ = ctx.path_move_to(path, rng.f(0.0, 64.0), rng.f(0.0, 64.0));
                    let _ = ctx.path_line_to(path, rng.f(0.0, 64.0), rng.f(0.0, 64.0));
                    let _ = ctx.path_cubic_to(
                        path,
                        rng.f(0.0, 64.0),
                        rng.f(0.0, 64.0),
                        rng.f(0.0, 64.0),
                        rng.f(0.0, 64.0),
                        rng.f(0.0, 64.0),
                        rng.f(0.0, 64.0),
                    );
                    let _ = ctx.canvas_draw_path(surface, path, paint);
                }
                7 => {
                    let _ = ctx.canvas_rotate(surface, rng.f(-180.0, 180.0));
                }
                8 => {
                    let _ = ctx.canvas_draw_color(surface, (rng.next() as u32) | 0xff000000);
                }
                _ => {
                    let _ = ctx.canvas_scale(surface, rng.f(0.2, 3.0), rng.f(0.2, 3.0));
                }
            }
        }
        let fuzz_root = ctx.render_node_create("fuzz");
        let _ = ctx.surface_render_node(surface, fuzz_root, 1);
        ctx.surface_present(surface, 1).unwrap();
        assert!(ctx.surface_capture(surface).unwrap().is_some());
    }
}

#[test]
fn draw_lines_points_and_paint() {
    let mut ctx = RenderContext::new();
    let surface = ctx.surface_create(40, 40, 1.0).unwrap();
    ctx.canvas_for_surface(surface).unwrap();
    let paint = ctx.paint_create();
    ctx.paint_set_color(paint, 0xff000000).unwrap();
    ctx.paint_set_style(paint, 1).unwrap();
    ctx.paint_set_stroke_width(paint, 4.0).unwrap();
    // Two horizontal lines.
    ctx.canvas_draw_lines(
        surface,
        &[0.0, 5.0, 40.0, 5.0, 0.0, 20.0, 40.0, 20.0],
        paint,
    )
    .unwrap();
    // A point at (30, 30).
    ctx.canvas_draw_points(surface, &[30.0, 30.0], paint)
        .unwrap();
    let fill = ctx.paint_create();
    ctx.paint_set_color(fill, 0xffff0000).unwrap();
    ctx.canvas_draw_paint(surface, fill).unwrap();
    ctx.surface_present(surface, 1).unwrap();
    let f = ctx.surface_capture(surface).unwrap().unwrap();
    // drawPaint filled the whole clip with red.
    let p = f.pixels.pixel(1, 1).unwrap();
    assert_eq!(p.red(), 255);
}

#[test]
fn nine_patch_scales_source_into_destination() {
    let mut ctx = RenderContext::new();
    let bmp = ctx.bitmap_create(8, 8, BitmapConfig::Rgba8888).unwrap();
    ctx.bitmap_erase(bmp, 0xff00ff00).unwrap();
    let surface = ctx.surface_create(64, 64, 1.0).unwrap();
    ctx.canvas_for_surface(surface).unwrap();
    ctx.canvas_draw_nine_patch(
        surface,
        bmp,
        acos_render::NinePatchChunk::default(),
        acos_render::Rect::new(10, 10, 54, 54),
        None,
        false,
    )
    .unwrap();
    ctx.surface_present(surface, 1).unwrap();
    let f = ctx.surface_capture(surface).unwrap().unwrap();
    assert_eq!(f.pixels.pixel(32, 32).unwrap().green(), 255);
    assert_eq!(f.pixels.pixel(2, 2).unwrap().alpha(), 0);
    // With no divs the stretch degrades to a uniform scale, with no
    // unsupported op recorded.
    assert!(f.unsupported_ops.is_empty(), "{:?}", f.unsupported_ops);
}

#[test]
fn scene_paint_defaults_antialias_like_android() {
    // A shape without an explicit `antiAlias` flag must be anti-aliased:
    // android.graphics.Paint() enables ANTI_ALIAS_FLAG (ADR-0019). The scene
    // schema previously forced `anti_alias: false`, which is what made the
    // fixture text render with only two grey levels.
    let json = r##"{
      "width": 64, "height": 64, "density": 1.0, "background": "#FFFFFFFF",
      "root": { "name": "r", "bounds": [0,0,64,64],
        "ops": [ {"op":"circle","cx":32,"cy":32,"r":20,"paint":{"color":"#FF000000"}} ] }
    }"##;
    let scene = acos_render::Scene::from_json(json).unwrap();
    let mut ctx = RenderContext::new();
    let out = scene.render(&mut ctx, 1).unwrap();
    let rgba = out.frame.rgba8();
    let mut unique = std::collections::HashSet::new();
    for px in rgba.chunks_exact(4) {
        unique.insert((px[0], px[1], px[2], px[3]));
    }
    assert!(
        unique.len() > 2,
        "AA off would leave only background and fill; got {} colours",
        unique.len()
    );
}

#[test]
fn png_encoding_round_trips() {
    let scene = acos_render::Scene::from_json(&scene_json()).unwrap();
    let mut ctx = RenderContext::new();
    let out = scene.render(&mut ctx, 1).unwrap();
    for setting in PngSetting::all() {
        let bytes = out.frame.png(setting);
        let decoded = acos_render::image::decode_png(&bytes).unwrap();
        assert_eq!(decoded.width, 200);
        assert_eq!(decoded.height, 120);
    }
}

// ---------------------------------------------------------------------------
// W47-A: raster canvas backing store, ReverseDifference clip, stroke-to-fill.
// ---------------------------------------------------------------------------

#[test]
fn bitmap_render_node_over_composites_onto_existing_bitmap() {
    use acos_render::render_node::{DisplayOp, RenderNode};

    let mut ctx = RenderContext::new();
    let bmp = ctx.bitmap_create(16, 16, BitmapConfig::Rgba8888).unwrap();
    ctx.bitmap_erase(bmp, 0xff10_2030).unwrap();
    let base = ctx.bitmap_pixels(bmp).unwrap();

    let paint = ctx.paint_create();
    ctx.paint_set_color(paint, 0xffff_0000).unwrap();
    let mut node = RenderNode::new("raster-canvas");
    node.left = 0;
    node.top = 0;
    node.right = 16;
    node.bottom = 16;
    node.display_list
        .push(DisplayOp::DrawRect(RectF::new(0.0, 0.0, 8.0, 8.0), paint));
    let node = ctx.render_nodes.insert(node);

    ctx.bitmap_render_node_over(bmp, node, &base).unwrap();
    // The drawn rect wins inside its bounds...
    assert_eq!(ctx.bitmap_get_pixel(bmp, 2, 2).unwrap(), 0xffff_0000);
    // ...and the bitmap's pre-existing pixels survive outside it.
    assert_eq!(ctx.bitmap_get_pixel(bmp, 12, 12).unwrap(), 0xff10_2030);
}

#[test]
fn clip_rect_reverse_difference_is_new_minus_existing() {
    let mut ctx = RenderContext::new();
    let surface = ctx.surface_create(40, 40, 1.0).unwrap();
    ctx.canvas_for_surface(surface).unwrap();
    // Existing clip A = the left half; reverse-difference with B = [10,30)^2
    // yields B - A = the part of B with x >= 20.
    ctx.canvas_clip_rect(
        surface,
        RectF::new(0.0, 0.0, 20.0, 40.0),
        ClipOp::Intersect,
        false,
    )
    .unwrap();
    ctx.canvas_clip_rect(
        surface,
        RectF::new(10.0, 10.0, 30.0, 30.0),
        ClipOp::ReverseDifference,
        false,
    )
    .unwrap();
    ctx.canvas_draw_color(surface, 0xffff_0000).unwrap();
    ctx.surface_present(surface, 1).unwrap();
    let f = ctx.surface_capture(surface).unwrap().unwrap();
    // Inside B but right of A is filled red...
    assert_eq!(f.pixels.pixel(25, 20).unwrap().red(), 255);
    // ...the left half (A) and the region outside B stay transparent.
    assert_eq!(f.pixels.pixel(15, 20).unwrap().alpha(), 0);
    assert_eq!(f.pixels.pixel(35, 20).unwrap().alpha(), 0);
}

#[test]
fn stroke_to_fill_produces_a_filled_outline() {
    use acos_render::paint::{StrokeCap, StrokeJoin};
    use acos_render::path::Path;

    let mut p = Path::new();
    p.move_to(0.0, 0.0);
    p.line_to(10.0, 0.0);
    let filled = p
        .stroke_to_fill(4.0, StrokeCap::Butt, StrokeJoin::Miter, 4.0)
        .expect("line strokes to a fill outline");
    let b = filled.compute_bounds().expect("bounds");
    // A 4px-wide line centred on y=0 spans y in [-2, 2].
    assert!(b.top <= -1.9 && b.bottom >= 1.9, "{b:?}");
    assert!(b.right >= 9.9, "{b:?}");
}
