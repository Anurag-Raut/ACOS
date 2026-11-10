//! Render a JSON scene to a PNG and print frame metadata.
//!
//! Usage:
//!   cargo run -p acos-render --example render_scene -- scene.json out.png --json
//!
//! Prints (with `--json`) frame sequence, dimensions, class, content hash,
//! timings and the unsupported-op list.

use std::time::Instant;

use acos_render::{RenderContext, Scene, ACOS_RENDER_VERSION};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let json = args.iter().any(|a| a == "--json");
    let positional: Vec<&String> = args.iter().filter(|a| !a.starts_with("--")).collect();
    if positional.len() < 2 {
        eprintln!("usage: render_scene <scene.json> <out.png> [--json]");
        std::process::exit(2);
    }
    let scene_path = positional[0];
    let out_path = positional[1];

    let mut ctx = RenderContext::new();
    let scene = match Scene::load(std::path::Path::new(scene_path)) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error loading scene: {} ({})", e, e.code());
            std::process::exit(1);
        }
    };
    let start = Instant::now();
    let out = match scene.render(&mut ctx, 1) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("error rendering: {} ({})", e, e.code());
            std::process::exit(1);
        }
    };
    let elapsed = start.elapsed();
    let png = out.frame.png(acos_render::PngSetting::FastFiltered);
    if let Err(e) = std::fs::write(out_path, &png) {
        eprintln!("error writing {out_path}: {e}");
        std::process::exit(1);
    }
    if json {
        let meta = serde_json::json!({
            "version": ACOS_RENDER_VERSION,
            "scene": scene_path,
            "output": out_path,
            "seq": out.seq,
            "width": out.frame.width,
            "height": out.frame.height,
            "density": out.frame.density,
            "frameClass": format!("{:?}", out.frame_class),
            "contentHash": out.frame.hash_hex(),
            "sha256": out.frame.sha256(),
            "nodeCount": out.node_count,
            "unsupportedOps": out.unsupported_ops,
            "pngBytes": png.len(),
            "renderMs": elapsed.as_secs_f64() * 1000.0,
            "causeId": out.frame.cause_id,
        });
        println!("{}", serde_json::to_string_pretty(&meta).unwrap());
    } else {
        println!(
            "rendered {}x{} seq={} class={:?} hash={} -> {} ({} bytes) in {:.2} ms",
            out.frame.width,
            out.frame.height,
            out.seq,
            out.frame_class,
            out.frame.hash_hex(),
            out_path,
            png.len(),
            elapsed.as_secs_f64() * 1000.0,
        );
        if !out.unsupported_ops.is_empty() {
            eprintln!("unsupported ops: {:?}", out.unsupported_ops);
        }
    }
}
