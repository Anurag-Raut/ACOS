//! ACOS render benchmark.
//!
//! Run in release for meaningful numbers:
//!
//!   cargo run -p acos-render --release --example bench_render -- results/render/bench.json
//!
//! Measures: hello-views-like scene cold + warm, unchanged-frame capture,
//! PNG encode at each setting, raw RGBA copy, text measurement throughput with
//! and without the shaping cache, a heavy scene, and process RSS/VmHWM.
//! Single-threaded. Prints JSON to stdout and (optionally) writes it to the
//! given path.

use std::time::Instant;

use acos_render::{
    BitmapConfig, DisplayOp, PngSetting, Rect, RectF, RenderContext, Scene, Surface,
    ACOS_RENDER_VERSION,
};

const ITER: usize = 60;

fn main() {
    let out_path = std::env::args().nth(1);
    let mut report = serde_json::Map::new();
    let host = host_info();
    report.insert("version".into(), ACOS_RENDER_VERSION.into());
    report.insert("iterations".into(), ITER.into());
    report.insert("host".into(), host);

    let (rss_before, hwm_before) = rss_kb().unwrap_or((0, 0));
    let mut ctx = RenderContext::new();
    let (rss_fonts, _hwm_fonts) = rss_kb().unwrap_or((0, 0));
    let has_fonts = ctx.has_fonts();
    report.insert("hasFonts".into(), has_fonts.into());
    report.insert("rssKbBefore".into(), rss_before.into());
    report.insert("rssKbAfterFonts".into(), rss_fonts.into());
    report.insert(
        "fontDeltaKb".into(),
        (rss_fonts.saturating_sub(rss_before)).into(),
    );
    report.insert("hwmKbBefore".into(), hwm_before.into());

    let scene_path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/scenes/hello-views.json");
    let scene = Scene::load(&scene_path).expect("load hello-views scene");

    // ---- (a) hello-views-like scene ------------------------------------
    let surface = ctx
        .surface_create(scene.width, scene.height, scene.density)
        .unwrap();
    let (root, node_count) = scene.build_tree(&mut ctx, &scene.root).unwrap();
    if let Some(node) = ctx.render_nodes.get_mut(root) {
        node.display_list
            .insert(0, DisplayOp::DrawColor(0xfff5f5f5));
    }
    let cold_start = Instant::now();
    ctx.surface_render_node(surface, root, 0).unwrap();
    let cold_ms = cold_start.elapsed().as_secs_f64() * 1000.0;
    let (rss_first, _) = rss_kb().unwrap_or((0, 0));
    report.insert("helloNodeCount".into(), node_count.into());
    report.insert("helloColdMs".into(), cold_ms.into());
    report.insert("rssKbAfterFirstFrame".into(), rss_first.into());

    // Warm re-render: bump a node version each iteration so the raster path
    // runs (otherwise damage tracking would cache it).
    let mut warm = Vec::new();
    for i in 0..ITER {
        ctx.render_node_set_alpha(root, 1.0).unwrap();
        let t = Instant::now();
        ctx.surface_render_node(surface, root, i as i64 + 1)
            .unwrap();
        warm.push(t.elapsed().as_secs_f64() * 1000.0);
    }
    report.insert("helloWarmMs".into(), percentile_json(&mut warm));

    // ---- (b) cached / unchanged frame ----------------------------------
    let mut cached = Vec::new();
    let mut capture = Vec::new();
    for _ in 0..ITER {
        let t = Instant::now();
        ctx.surface_render_node(surface, root, 999).unwrap(); // unchanged -> cached
        cached.push(t.elapsed().as_secs_f64() * 1000.0);
        let t = Instant::now();
        let _ = ctx.surface_capture(surface).unwrap();
        capture.push(t.elapsed().as_secs_f64() * 1000.0);
    }
    report.insert("helloCachedRenderMs".into(), percentile_json(&mut cached));
    report.insert("helloCaptureMs".into(), percentile_json(&mut capture));
    let frame = ctx.surface_capture(surface).unwrap().unwrap();
    report.insert("helloContentHash".into(), frame.hash_hex().into());
    report.insert("helloFrameSeq".into(), frame.seq.into());

    // ---- (c) PNG encode + raw copy -------------------------------------
    let mut png_report = serde_json::Map::new();
    for setting in PngSetting::all() {
        let mut times = Vec::new();
        let mut size = 0;
        for _ in 0..20 {
            let t = Instant::now();
            let bytes = frame.png(setting);
            times.push(t.elapsed().as_secs_f64() * 1000.0);
            size = bytes.len();
        }
        png_report.insert(
            format!("{setting:?}"),
            serde_json::json!({
                "ms": percentile_json(&mut times),
                "bytes": size,
            }),
        );
    }
    let mut raw_times = Vec::new();
    let mut raw_size = 0;
    for _ in 0..ITER {
        let t = Instant::now();
        let raw = frame.rgba8().to_vec();
        raw_times.push(t.elapsed().as_secs_f64() * 1000.0);
        raw_size = raw.len();
    }
    png_report.insert(
        "RawRgba8Copy".into(),
        serde_json::json!({ "ms": percentile_json(&mut raw_times), "bytes": raw_size }),
    );
    let mut qoi_times = Vec::new();
    let mut qoi_size = 0;
    for _ in 0..20 {
        let t = Instant::now();
        let q = frame.qoi();
        qoi_times.push(t.elapsed().as_secs_f64() * 1000.0);
        qoi_size = q.len();
    }
    png_report.insert(
        "Qoi".into(),
        serde_json::json!({ "ms": percentile_json(&mut qoi_times), "bytes": qoi_size }),
    );
    report.insert("encoding".into(), png_report.into());

    // ---- (d) text measurement throughput -------------------------------
    if has_fonts {
        let paint = ctx.paint_create();
        ctx.paint_set_text_size(paint, 42.0).unwrap();
        let strings: Vec<Vec<u16>> = (0..1000)
            .map(|i| {
                format!("Hello ACOS row {i} — valeur")
                    .encode_utf16()
                    .collect()
            })
            .collect();

        // First pass: distinct strings -> cache misses.
        ctx.reset_text_cache_stats();
        let t = Instant::now();
        for s in &strings {
            let _ = ctx.paint_measure_text(paint, s).unwrap();
        }
        let miss_ms = t.elapsed().as_secs_f64() * 1000.0;
        let stats_miss = ctx.shape_cache_stats();

        // Second pass: identical strings -> cache hits.
        let t = Instant::now();
        for s in &strings {
            let _ = ctx.paint_measure_text(paint, s).unwrap();
        }
        let hit_ms = t.elapsed().as_secs_f64() * 1000.0;
        let stats_hit = ctx.shape_cache_stats();
        let warm_hits = stats_hit.hits.saturating_sub(stats_miss.hits);
        let warm_misses = stats_hit.misses.saturating_sub(stats_miss.misses);
        let warm_hit_rate = if warm_hits + warm_misses == 0 {
            0.0
        } else {
            warm_hits as f64 / (warm_hits + warm_misses) as f64
        };

        report.insert(
            "textMeasure".into(),
            serde_json::json!({
                "strings": 1000,
                "coldMs": miss_ms,
                "coldStringsPerSec": 1000.0 / (miss_ms / 1000.0).max(1e-9),
                "warmMs": hit_ms,
                "warmStringsPerSec": 1000.0 / (hit_ms / 1000.0).max(1e-9),
                "coldMisses": stats_miss.misses,
                "warmHits": warm_hits,
                "warmMisses": warm_misses,
                "warmHitRate": warm_hit_rate,
            }),
        );
    } else {
        report.insert("textMeasure".into(), serde_json::Value::Null);
    }

    // ---- (e) heavy scene ------------------------------------------------
    let heavy_surface = ctx
        .surface_create(scene.width, scene.height, scene.density)
        .unwrap();
    let heavy_root = build_heavy_scene(&mut ctx, 200, 50, 20);
    let t = Instant::now();
    ctx.surface_render_node(heavy_surface, heavy_root, 1)
        .unwrap();
    let heavy_cold = t.elapsed().as_secs_f64() * 1000.0;
    let mut heavy_warm = Vec::new();
    for i in 0..ITER {
        ctx.render_node_set_alpha(heavy_root, 1.0).unwrap();
        let t = Instant::now();
        ctx.surface_render_node(heavy_surface, heavy_root, i as i64 + 2)
            .unwrap();
        heavy_warm.push(t.elapsed().as_secs_f64() * 1000.0);
    }
    report.insert(
        "heavyScene".into(),
        serde_json::json!({
            "textRows": 200,
            "cards": 50,
            "bitmaps": 20,
            "coldMs": heavy_cold,
            "warmMs": percentile_json(&mut heavy_warm),
        }),
    );

    // ---- (f) damage-aware present --------------------------------------
    // 1080x2400 surface: a repeat present that reports a 200x100 damage rect
    // must stay well under 0.1 ms, while the full-frame present reports the
    // honest full-buffer cost. Run in release; p50 over 60 runs.
    let mut surface = Surface::new(1080, 2400, 2.625).unwrap();
    {
        let data = surface.pixmap_mut().data_mut();
        for (i, b) in data.iter_mut().enumerate() {
            *b = (i as u32).wrapping_mul(2654435761) as u8;
        }
    }
    surface.present(0, Vec::new());
    let damage = Rect::new(100, 100, 300, 200);
    let mut damage_present = Vec::new();
    for i in 0..60 {
        // Touch the reported region, then present only it.
        {
            let data = surface.pixmap_mut().data_mut();
            let off = ((damage.top as u32 * 1080 + damage.left as u32) * 4) as usize;
            data[off] = i as u8;
        }
        let t = Instant::now();
        surface.present_damage(i as i64 + 1, Some(damage), Vec::new());
        damage_present.push(t.elapsed().as_secs_f64() * 1000.0);
    }
    let mut full_present = Vec::new();
    for i in 0..60 {
        let t = Instant::now();
        surface.present(i as i64 + 100, Vec::new());
        full_present.push(t.elapsed().as_secs_f64() * 1000.0);
    }
    report.insert(
        "presentDamage".into(),
        serde_json::json!({
            "width": 1080,
            "height": 2400,
            "damage": [100, 100, 300, 200],
            "damageMs": percentile_json(&mut damage_present),
            "fullMs": percentile_json(&mut full_present),
        }),
    );

    // ---- (g) memory -----------------------------------------------------
    let (rss_after_heavy, hwm_after_heavy) = rss_kb().unwrap_or((0, 0));
    report.insert("rssKbAfterHeavy".into(), rss_after_heavy.into());
    report.insert("hwmKbAfterHeavy".into(), hwm_after_heavy.into());

    // Per additional surface.
    let before_surface = rss_kb().map(|x| x.0).unwrap_or(0);
    let mut surfaces = Vec::new();
    for _ in 0..20 {
        surfaces.push(ctx.surface_create(1080, 2400, 2.625).unwrap());
    }
    let after_surfaces = rss_kb().map(|x| x.0).unwrap_or(0);
    report.insert(
        "concurrentSurfaces".into(),
        serde_json::json!({
            "count": 20,
            "width": 1080,
            "height": 2400,
            "rssKbBefore": before_surface,
            "rssKbAfter": after_surfaces,
            "rssGrowthKb": after_surfaces.saturating_sub(before_surface),
            "rssPerSurfaceKb": (after_surfaces.saturating_sub(before_surface)) as f64 / 20.0,
        }),
    );

    // RSS of a headless context (no surfaces drawn) for the "allocates
    // nothing" claim is captured by rssKbBefore in this run.

    let json = serde_json::to_string_pretty(&serde_json::Value::Object(report)).unwrap();
    println!("{json}");
    if let Some(path) = out_path {
        if let Some(parent) = std::path::Path::new(&path).parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        std::fs::write(&path, &json).expect("write bench json");
        eprintln!("wrote {path}");
    }
}

/// 200 text rows + 50 rounded cards + 20 bitmaps.
fn build_heavy_scene(
    ctx: &mut RenderContext,
    rows: usize,
    cards: usize,
    bitmaps: usize,
) -> acos_render::Handle {
    let root = ctx.render_node_create("heavy-root");
    ctx.render_node_set_bounds(root, 0, 0, 1080, 2400).unwrap();
    ctx.render_node_push_op(root, DisplayOp::DrawColor(0xffffffff))
        .unwrap();

    // Bitmaps.
    let mut bitmap_handles = Vec::new();
    for i in 0..bitmaps {
        let b = ctx.bitmap_create(128, 128, BitmapConfig::Rgba8888).unwrap();
        let color =
            0xff00_0000 | ((i as u32 * 13) & 0xff) << 16 | ((i as u32 * 29) & 0xff) << 8 | 0x80;
        ctx.bitmap_erase(b, color).unwrap();
        bitmap_handles.push(b);
    }

    // 50 rounded cards.
    for i in 0..cards {
        let y = 40.0 + i as f32 * 46.0;
        let paint = ctx.paint_create();
        ctx.paint_set_color(paint, 0xffe0e0e0).unwrap();
        ctx.paint_set_anti_alias(paint, true).unwrap();
        ctx.render_node_push_op(
            root,
            DisplayOp::DrawRoundRect(RectF::new(40.0, y, 1040.0, y + 40.0), 8.0, 8.0, paint),
        )
        .unwrap();
    }

    // 20 bitmaps.
    for (i, b) in bitmap_handles.iter().enumerate() {
        let y = 80.0 + i as f32 * 120.0;
        ctx.render_node_push_op(
            root,
            DisplayOp::DrawBitmap(*b, None, RectF::new(800.0, y, 928.0, y + 128.0), None, true),
        )
        .unwrap();
    }

    // 200 text rows.
    if ctx.has_fonts() {
        for i in 0..rows {
            let paint = ctx.paint_create();
            ctx.paint_set_text_size(paint, 36.0).unwrap();
            ctx.paint_set_color(paint, 0xff202124).unwrap();
            let text: Vec<u16> = format!("Row {i}: the quick brown fox jumps over the lazy dog")
                .encode_utf16()
                .collect();
            let y = 2400.0 - (rows as f32 - i as f32) * 10.0 - 20.0;
            ctx.render_node_push_op(root, DisplayOp::DrawText(text, 60.0, y, paint))
                .unwrap();
        }
    }
    root
}

fn percentile_json(times: &mut [f64]) -> serde_json::Value {
    if times.is_empty() {
        return serde_json::Value::Null;
    }
    times.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let p = |q: f64| -> f64 {
        let idx = ((times.len() - 1) as f64 * q).round() as usize;
        times[idx]
    };
    serde_json::json!({
        "p50": p(0.50),
        "p95": p(0.95),
        "min": times[0],
        "max": times[times.len() - 1],
        "mean": times.iter().sum::<f64>() / times.len() as f64,
    })
}

fn rss_kb() -> Option<(u64, u64)> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let mut rss = 0u64;
    let mut hwm = 0u64;
    for line in status.lines() {
        if let Some(v) = line.strip_prefix("VmRSS:") {
            rss = v
                .split_whitespace()
                .next()
                .and_then(|x| x.parse().ok())
                .unwrap_or(0);
        } else if let Some(v) = line.strip_prefix("VmHWM:") {
            hwm = v
                .split_whitespace()
                .next()
                .and_then(|x| x.parse().ok())
                .unwrap_or(0);
        }
    }
    Some((rss, hwm))
}

fn host_info() -> serde_json::Value {
    let cpu = std::fs::read_to_string("/proc/cpuinfo")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("model name"))
                .and_then(|l| l.split(':').nth(1))
                .map(|s| s.trim().to_string())
        })
        .unwrap_or_else(|| "unknown".to_string());
    let cpus = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(0);
    let mem_kb = std::fs::read_to_string("/proc/meminfo")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("MemTotal:"))
                .and_then(|l| l.split_whitespace().nth(1))
                .and_then(|x| x.parse::<u64>().ok())
        })
        .unwrap_or(0);
    serde_json::json!({
        "cpu": cpu,
        "logicalCpus": cpus,
        "memTotalKb": mem_kb,
        "threadsUsed": 1,
    })
}
