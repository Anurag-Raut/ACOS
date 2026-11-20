//! W70-A renderer-fidelity effects: bitmap shaders, two-lobe elevation
//! shadows, and Skia radius->sigma mask-filter blur.
//!
//! These are deterministic renderer tests (no ART capture needed to be
//! meaningful): they assert the *semantics* the framework natives expose
//! (`BitmapShader`, `RenderNode.setElevation`, `BlurMaskFilter`) rather than a
//! screenshot diff. The per-scene ART reference diffs live in `corpus.rs`.

use acos_render::{
    BitmapConfig, ColorFilterSpec, DisplayOp, Matrix3x3, PorterDuffMode, RectF, RenderContext,
    RenderEffectSpec, Scene, ShaderSpec, TileMode,
};

fn render(scene_json: &str) -> acos_render::SceneRender {
    let scene = Scene::from_json(scene_json).expect("parse scene");
    let mut ctx = RenderContext::new();
    scene.render(&mut ctx, 1).expect("render scene")
}

fn rgba(frame: &acos_render::Frame, x: u32, y: u32) -> (u8, u8, u8, u8) {
    let p = frame.pixels.pixel(x, y).expect("pixel in bounds");
    (p.red(), p.green(), p.blue(), p.alpha())
}

/// `BitmapShader` with `TileMode.REPEAT`: each bitmap texel is replicated and
/// the pattern restarts at the bitmap width/height.
#[test]
fn bitmap_shader_repeats_the_bitmap() {
    // 2x2 source: red, green / blue, yellow.
    let mut ctx = RenderContext::new();
    let b = ctx
        .bitmap_create(2, 2, BitmapConfig::Rgba8888)
        .expect("bitmap create");
    ctx.bitmap_set_pixel(b, 0, 0, 0xffff_0000).unwrap();
    ctx.bitmap_set_pixel(b, 1, 0, 0xff00_ff00).unwrap();
    ctx.bitmap_set_pixel(b, 0, 1, 0xff00_00ff).unwrap();
    ctx.bitmap_set_pixel(b, 1, 1, 0xffff_ff00).unwrap();
    let png = ctx.bitmap_compress_png(b).expect("png encode");
    let path = std::env::temp_dir().join(format!("acos-w70a-shader-{}.png", std::process::id()));
    std::fs::write(&path, &png).unwrap();

    let json = format!(
        r##"{{
          "width": 6, "height": 6, "density": 1.0,
          "background": "#FF000000",
          "root": {{
            "name": "root", "bounds": [0,0,6,6],
            "ops": [
              {{"op":"rect","rect":[0,0,4,4],"paint":{{
                 "shader":{{"type":"bitmap","path":"{}","tile":"repeat"}}
              }}}}
            ]
          }}
        }}"##,
        path.to_string_lossy()
    );
    let out = render(&json);
    let _ = std::fs::remove_file(&path);
    assert!(out.unsupported_ops.is_empty(), "{:?}", out.unsupported_ops);
    let rgb = |x, y| {
        let (r, g, b, _) = rgba(&out.frame, x, y);
        (r, g, b)
    };
    // Texel-for-texel, then repeated in x and y.
    assert_eq!(rgb(0, 0), (255, 0, 0), "0,0 red");
    assert_eq!(rgb(1, 0), (0, 255, 0), "1,0 green");
    assert_eq!(rgb(0, 1), (0, 0, 255), "0,1 blue");
    assert_eq!(rgb(1, 1), (255, 255, 0), "1,1 yellow");
    assert_eq!(rgb(2, 0), (255, 0, 0), "2,0 repeats column 0");
    assert_eq!(rgb(0, 2), (255, 0, 0), "0,2 repeats row 0");
    assert_eq!(rgb(1, 2), (0, 255, 0), "1,2 repeats row 0");
}

/// An elevated node paints a shadow outside its outline; an elevation-0 node
/// paints none. The two-lobe model must paint both a soft ambient spread and a
/// stronger spot lobe.
#[test]
fn elevation_paints_ambient_and_spot_shadow_lobes() {
    let card = |elev: f32| {
        format!(
            r##"{{
              "width": 220, "height": 220, "density": 1.0,
              "background": "#FFFFFFFF",
              "root": {{
                "name": "root", "bounds": [0,0,220,220],
                "children": [
                  {{
                    "name": "card",
                    "bounds": [60,60,160,160],
                    "elevation": {elev},
                    "outline": {{"type":"roundRect","bounds":[0,0,100,100],"radius":12}},
                    "ops": [
                      {{"op":"roundRect","rect":[0,0,100,100],"rx":12,"ry":12,
                        "paint":{{"color":"#FFFFFFFF"}}}}
                    ]
                  }}
                ]
              }}
            }}"##
        )
    };
    let flat = render(&card(0.0));
    let elevated = render(&card(8.0));

    // Directly below the card must be shaded; scan a small band because the
    // exact falloff depends on the box/Gaussian approximation.
    let min_below = (160u32..=182)
        .map(|y| rgba(&elevated.frame, 110, y).0)
        .min()
        .expect("band");
    assert!(
        min_below < 240,
        "expected a shadow below the card, min channel was {min_below}"
    );
    let just_below = rgba(&elevated.frame, 110, 166).0;
    let farther = rgba(&elevated.frame, 110, 178).0;
    assert!(
        just_below <= farther,
        "shadow should fade with distance: {just_below} vs {farther}"
    );
    // With elevation 0 there is no shadow at all.
    let min_flat = (160u32..=182)
        .map(|y| rgba(&flat.frame, 110, y).0)
        .min()
        .expect("band");
    assert_eq!(min_flat, 255, "flat card must not cast a shadow");
}

/// A `BlurMaskFilter` radius is converted to a Gaussian sigma by
/// `sigma = radius * 0.57735 + 0.5`; a larger radius must spread more
/// intermediate-coverage pixels along the edge.
#[test]
fn mask_filter_blur_radius_spreads_more() {
    fn edge_pixels(radius: f32) -> usize {
        let json = format!(
            r##"{{
              "width": 120, "height": 60, "density": 1.0,
              "background": "#FFFFFFFF",
              "root": {{
                "name": "root", "bounds": [0,0,120,60],
                "ops": [
                  {{"op":"rect","rect":[40,15,80,45],
                    "paint":{{"color":"#FF000000","blur":{radius}}}}}
                ]
              }}
            }}"##
        );
        let out = render(&json);
        let mut n = 0;
        for y in 0..out.frame.height {
            for x in 0..out.frame.width {
                let (r, _, _, _) = rgba(&out.frame, x, y);
                if r > 10 && r < 245 {
                    n += 1;
                }
            }
        }
        n
    }
    let sharp = edge_pixels(0.0);
    let blur2 = edge_pixels(2.0);
    let blur8 = edge_pixels(8.0);
    assert!(
        blur2 > sharp,
        "radius 2 should add intermediate pixels: {blur2} vs {sharp}"
    );
    assert!(
        blur8 > blur2,
        "radius 8 should spread more than radius 2: {blur8} vs {blur2}"
    );
}

/// `RenderNode.setRenderEffect(createBlurEffect)` renders the node into an
/// offscreen layer and blurs that layer before compositing: the halo reaches
/// outside the node's own bounds.
#[test]
fn render_effect_blur_spreads_the_node_layer() {
    let mut ctx = RenderContext::new();
    let surface = ctx.surface_create(64, 64, 1.0).unwrap();
    ctx.canvas_for_surface(surface).unwrap();
    let paint = ctx.paint_create();
    ctx.paint_set_color(paint, 0xff00_0000).unwrap();
    let node = ctx.render_node_create("blurred");
    ctx.render_nodes
        .get_mut(node)
        .unwrap()
        .display_list
        .push(DisplayOp::DrawRect(
            RectF::new(24.0, 24.0, 40.0, 40.0),
            paint,
        ));
    ctx.render_nodes.get_mut(node).unwrap().render_effect = Some(RenderEffectSpec::Blur {
        sigma_x: 5.0,
        sigma_y: 5.0,
        edge: TileMode::Clamp,
    });
    ctx.surface_render_node(surface, node, 1).unwrap();
    let f = ctx.surface_capture(surface).unwrap().unwrap();
    assert!(f.unsupported_ops.is_empty(), "{:?}", f.unsupported_ops);
    assert_eq!(
        f.pixels.pixel(32, 32).unwrap().red(),
        0,
        "centre stays black"
    );
    let halo = f.pixels.pixel(32, 18).unwrap().red();
    assert!(
        halo < 250,
        "expected a blur halo outside the rect, got {halo}"
    );
}

/// `RenderEffect.createColorFilterEffect` applies the filter to the node layer.
#[test]
fn render_effect_color_filter_transforms_the_node_layer() {
    let mut ctx = RenderContext::new();
    let surface = ctx.surface_create(32, 32, 1.0).unwrap();
    ctx.canvas_for_surface(surface).unwrap();
    let paint = ctx.paint_create();
    ctx.paint_set_color(paint, 0xffff_0000).unwrap();
    let node = ctx.render_node_create("filtered");
    ctx.render_nodes
        .get_mut(node)
        .unwrap()
        .display_list
        .push(DisplayOp::DrawRect(RectF::new(4.0, 4.0, 28.0, 28.0), paint));
    // Zero the RGB rows, keep alpha => red becomes black.
    ctx.render_nodes.get_mut(node).unwrap().render_effect = Some(RenderEffectSpec::ColorFilter(
        ColorFilterSpec::ColorMatrix {
            matrix: [
                0.0, 0.0, 0.0, 0.0, 0.0, // r
                0.0, 0.0, 0.0, 0.0, 0.0, // g
                0.0, 0.0, 0.0, 0.0, 0.0, // b
                0.0, 0.0, 0.0, 1.0, 0.0, // a
            ],
        },
    ));
    ctx.surface_render_node(surface, node, 1).unwrap();
    let f = ctx.surface_capture(surface).unwrap().unwrap();
    let p = f.pixels.pixel(16, 16).unwrap();
    assert_eq!((p.red(), p.green(), p.blue(), p.alpha()), (0, 0, 0, 255));
}

/// `ComposeShader` rasterises both shaders into offscreen layers and combines
/// them with the PorterDuff operator instead of reporting unsupported.
#[test]
fn compose_shader_combines_two_shaders() {
    let solid = |c: u32| ShaderSpec::Linear {
        start: (0.0, 0.0),
        end: (32.0, 0.0),
        colors: vec![c, c],
        positions: None,
        tile: TileMode::Clamp,
        local: Matrix3x3::identity(),
    };
    let mut ctx = RenderContext::new();
    let surface = ctx.surface_create(32, 32, 1.0).unwrap();
    ctx.canvas_for_surface(surface).unwrap();
    let paint = ctx.paint_create();
    ctx.paints.get_mut(paint).unwrap().shader = Some(ShaderSpec::Compose {
        a: Box::new(solid(0xffff_0000)),
        b: Box::new(solid(0xff00_00ff)),
        mode: PorterDuffMode::Multiply,
    });
    ctx.canvas_draw_rect(surface, RectF::new(0.0, 0.0, 32.0, 32.0), paint)
        .unwrap();
    ctx.surface_present(surface, 1).unwrap();
    let f = ctx.surface_capture(surface).unwrap().unwrap();
    assert!(f.unsupported_ops.is_empty(), "{:?}", f.unsupported_ops);
    let p = f.pixels.pixel(16, 16).unwrap();
    assert!(
        p.red() < 16 && p.green() < 16 && p.blue() < 16,
        "multiply red*blue must be black, got {:?}",
        (p.red(), p.green(), p.blue())
    );
}
