//! Golden-image tests.
//!
//! A small deterministic scene is rendered and compared against a checked-in
//! PNG. Comparison is tolerance-based (per-channel and pixel-count) so
//! tiny-skia/font revisions do not cause false failures. Set `UPDATE_GOLDEN=1`
//! to regenerate the goldens.

use std::path::PathBuf;

use acos_render::{image, Scene};

const GOLDEN_SCENE: &str = r##"{
  "width": 96, "height": 64, "density": 1.0, "background": "#FFFFFFFF",
  "root": {
    "name": "root", "bounds": [0,0,96,64],
    "ops": [
      {"op":"rect","rect":[4,4,44,28],"paint":{"color":"#FF3366CC"}},
      {"op":"roundRect","rect":[50,4,92,28],"rx":6,"ry":6,"paint":{"color":"#FFCC3366","antiAlias":true}},
      {"op":"circle","cx":24,"cy":46,"r":14,"paint":{"color":"#FF22AA55","antiAlias":true}},
      {"op":"oval","rect":[48,34,92,58],"paint":{"color":"#FFAA8822","antiAlias":true}},
      {"op":"line","x0":4,"y0":60,"x1":92,"y1":60,"paint":{"color":"#FF000000","style":"stroke","strokeWidth":2}}
    ]
  }
}"##;

fn golden_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden/primitives.png")
}

#[test]
fn golden_primitives_match() {
    let scene = Scene::from_json(GOLDEN_SCENE).unwrap();
    let mut ctx = acos_render::RenderContext::new();
    let out = scene.render(&mut ctx, 1).unwrap();
    let actual = out.frame.png(acos_render::PngSetting::FastFiltered);

    let path = golden_path();
    if std::env::var("UPDATE_GOLDEN").is_ok() || !path.exists() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, &actual).unwrap();
        eprintln!("wrote golden {}", path.display());
        return;
    }
    let expected_bytes = std::fs::read(&path).unwrap();
    let expected = image::decode_png(&expected_bytes).unwrap();
    let got = image::decode_png(&actual).unwrap();
    assert_eq!(expected.width, got.width, "golden width");
    assert_eq!(expected.height, got.height, "golden height");

    let total = got.pixels.len();
    let mut differing = 0usize;
    let mut max_channel_diff = 0u8;
    for (e, g) in expected.pixels.iter().zip(got.pixels.iter()) {
        let (er, eg, eb, ea) = acos_render::color::unpack_unpremul(*e);
        let (gr, gg, gb, ga) = acos_render::color::unpack_unpremul(*g);
        let d = [
            (er as i32 - gr as i32).unsigned_abs() as u8,
            (eg as i32 - gg as i32).unsigned_abs() as u8,
            (eb as i32 - gb as i32).unsigned_abs() as u8,
            (ea as i32 - ga as i32).unsigned_abs() as u8,
        ];
        let m = *d.iter().max().unwrap();
        if m > 12 {
            differing += 1;
            max_channel_diff = max_channel_diff.max(m);
        }
    }
    let ratio = differing as f64 / total as f64;
    eprintln!(
        "golden diff: {differing}/{total} pixels ({:.3}%), max channel diff {max_channel_diff}",
        ratio * 100.0
    );
    assert!(
        ratio < 0.005,
        "golden mismatch: {differing}/{total} pixels differ ({:.3}%)",
        ratio * 100.0
    );
}

#[test]
fn hello_views_scene_renders() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/scenes/hello-views.json");
    let scene = Scene::load(&path).unwrap();
    let mut ctx = acos_render::RenderContext::new();
    let out = scene.render(&mut ctx, 1).unwrap();
    assert_eq!((out.frame.width, out.frame.height), (1080, 2400));
    assert!(out.node_count >= 8, "node count {}", out.node_count);
    // The scene rasterises real widget-ish content, so it must not be blank.
    assert_eq!(out.frame_class, acos_render::FrameClass::Content);
    // Shapes must be present even without fonts; with fonts we also get text.
    if !ctx.has_fonts() {
        eprintln!("SKIP text assertions: no fonts available (set ACOS_FONT_DIR)");
    }
}
