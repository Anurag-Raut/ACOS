//! Text-draw microbenchmark for the ADR-0019 FreeType mask path.
//!
//! Draws the same 14sp string (~36.75px) repeatedly through
//! `RenderContext::canvas_draw_text` on a small surface. After the first
//! (cold) draw the shaping cache and the FreeType glyph-mask cache are warm, so
//! the steady-state number is the mask blit + cached shape lookup.
//!
//! Run the mask path and the outline fallback in the same build by setting
//! `ACOS_RENDER_FORCE_TEXT_PATH=1`:
//!
//!   cargo run -p acos-render --release --example bench_text_draw -- out.json
//!   ACOS_RENDER_FORCE_TEXT_PATH=1 cargo run -p acos-render --release \
//!       --example bench_text_draw -- out-path.json
//!
//! p50/p95 are over `ITER` (>= 20) single-threaded draws.

use std::time::Instant;

use acos_render::RenderContext;

const ITER: usize = 60;
const TEXT: &str = "Hello ACOS";
const TEXT_SIZE: f32 = 36.75;

fn main() {
    let out_path = std::env::args().nth(1);
    let mode = if std::env::var_os("ACOS_RENDER_FORCE_TEXT_PATH").is_some() {
        "path"
    } else {
        "freetype_mask"
    };

    let (rss_before, hwm_before) = rss_kb().unwrap_or((0, 0));
    let mut ctx = RenderContext::new();
    let (rss_fonts, _) = rss_kb().unwrap_or((0, 0));
    if !ctx.has_fonts() {
        eprintln!("no fonts available; set ACOS_FONT_DIR");
        std::process::exit(2);
    }

    let surface = ctx.surface_create(512, 128, 1.0).unwrap();
    ctx.canvas_for_surface(surface).unwrap();
    let paint = ctx.paint_create();
    ctx.paint_set_text_size(paint, TEXT_SIZE).unwrap();
    ctx.paint_set_color(paint, 0xff75_7575).unwrap();
    let text: Vec<u16> = TEXT.encode_utf16().collect();

    // Cold: first shaping + first FreeType rasterisation.
    let cold_start = Instant::now();
    ctx.canvas_draw_text(surface, &text, 8.0, 80.0, paint).unwrap();
    let cold_ms = cold_start.elapsed().as_secs_f64() * 1000.0;
    let (rss_after_cold, _) = rss_kb().unwrap_or((0, 0));

    let mut times = Vec::with_capacity(ITER);
    for i in 0..ITER {
        let x = 8.0 + (i % 4) as f32 * 0.25;
        let t = Instant::now();
        ctx.canvas_draw_text(surface, &text, x, 80.0, paint).unwrap();
        times.push(t.elapsed().as_secs_f64() * 1000.0);
    }
    let (rss_after, hwm_after) = rss_kb().unwrap_or((0, 0));

    times.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let p = |q: f64| times[((times.len() - 1) as f64 * q).round() as usize];
    let json = serde_json::json!({
        "mode": mode,
        "text": TEXT,
        "textSizePx": TEXT_SIZE,
        "iterations": ITER,
        "coldMs": cold_ms,
        "drawNs": {
            "p50": p(0.50) * 1e6,
            "p95": p(0.95) * 1e6,
            "min": times[0] * 1e6,
            "max": times[times.len() - 1] * 1e6,
            "mean": times.iter().sum::<f64>() / times.len() as f64 * 1e6,
        },
        "rssKbBefore": rss_before,
        "rssKbAfterFonts": rss_fonts,
        "rssKbAfterColdDraw": rss_after_cold,
        "rssKbAfterWarm": rss_after,
        "hwmKbBefore": hwm_before,
        "hwmKbAfter": hwm_after,
    });
    let text = serde_json::to_string_pretty(&json).unwrap();
    println!("{text}");
    if let Some(path) = out_path {
        if let Some(parent) = std::path::Path::new(&path).parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        std::fs::write(&path, text.as_bytes()).expect("write bench json");
        eprintln!("wrote {path}");
    }
}

fn rss_kb() -> Option<(u64, u64)> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let mut rss = 0u64;
    let mut hwm = 0u64;
    for line in status.lines() {
        if let Some(v) = line.strip_prefix("VmRSS:") {
            rss = v.split_whitespace().next()?.parse().ok()?;
        } else if let Some(v) = line.strip_prefix("VmHWM:") {
            hwm = v.split_whitespace().next()?.parse().ok()?;
        }
    }
    Some((rss, hwm))
}
