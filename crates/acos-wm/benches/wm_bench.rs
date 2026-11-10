//! Micro-benchmarks for `acos-wm`, run with `cargo bench -p acos-wm`.
//!
//! Reports p50/p95 for the agent-visible operations (inject→deliver→ack,
//! relayout compute, compositor compose/present) and measures idle CPU over five
//! seconds: the state machines must contribute **zero** wakeups and no
//! measurable `/proc/self/stat` utime/stime.
//!
//! The compositor rows separate the part `acos-wm` owns (`compose`: layer blits
//! into the persistent surface pixmap) from the `acos-render` commit
//! (`surface.present`, which hashes and clones the full frame). A `legacy`
//! reference reimplements the pre-damage per-pixel path so the before/after
//! comparison is measured in the same run.
//!
//! Dependency-free, so `results/wm/bench.txt` can be regenerated with a stock
//! release build.

use std::hint::black_box;
use std::sync::Arc;
use std::time::{Duration, Instant};

use acos_parcel::binder::BinderId;
use acos_render::{Bitmap, BitmapConfig};
use acos_wm::clock::{Clock, RealClock};
use acos_wm::codec::LayoutParams;
use acos_wm::compositor::Compositor;
use acos_wm::display::DisplaySpec;
use acos_wm::facade::SessionWm;
use acos_wm::geom::PointI;

const SAMPLES: usize = 101;
const W: u32 = 1080;
const H: u32 = 2400;
const IME_H: u32 = 126;

fn percentile(sorted: &[f64], pct: usize) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let idx = (sorted.len() * pct / 100).min(sorted.len() - 1);
    sorted[idx]
}

fn stats(mut samples: Vec<f64>) -> (f64, f64, f64) {
    samples.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let mean = samples.iter().sum::<f64>() / samples.len() as f64;
    (percentile(&samples, 50), percentile(&samples, 95), mean)
}

fn row(name: &str, samples: Vec<f64>) -> String {
    let (p50, p95, mean) = stats(samples);
    format!("| {name} | {p50:.1} | {p95:.1} | {mean:.1} |")
}

fn display() -> DisplaySpec {
    DisplaySpec::reference_1080x2400()
}

/// A full-display opaque content bitmap (a real app frame, not a flat colour,
/// so `acos-render`'s classifier exits early).
fn content_bitmap(seed: u32) -> Bitmap {
    let mut b = Bitmap::new(W, H, BitmapConfig::Rgba8888).unwrap();
    for (i, p) in b.pixels_mut().iter_mut().enumerate() {
        let i = i as u32 + seed;
        let r = (i.wrapping_mul(7) >> 3) & 0xff;
        let g = (i.wrapping_mul(13) >> 4) & 0xff;
        let bl = (i.wrapping_mul(29) >> 5) & 0xff;
        *p = r | (g << 8) | (bl << 16) | (255 << 24);
    }
    b.set_has_alpha(false);
    b
}

/// A solid 200×100 buffer, used to damage exactly that region.
/// Same content but with the alpha channel *declared*: `detect_opaque` must scan
/// the pixels to find that it is opaque.
fn content_bitmap_rgba(seed: u32) -> Bitmap {
    let mut b = content_bitmap(seed);
    b.set_has_alpha(true);
    b
}

fn small_bitmap() -> Bitmap {
    let mut b = Bitmap::new(200, 100, BitmapConfig::Rgba8888).unwrap();
    b.erase(0xff22_88ff);
    b
}

/// A translucent IME-sized buffer (`alpha ≈ 0.8`, premultiplied).
fn ime_bitmap() -> Bitmap {
    let mut b = Bitmap::new(W, IME_H, BitmapConfig::Rgba8888).unwrap();
    // 0xcc alpha; erase() premultiplies the channels.
    b.erase(0xcc33_4455);
    b
}

fn new_compositor() -> Compositor {
    Compositor::new(display()).unwrap()
}

/// Two-layer stack: opaque full-screen app + translucent 1080×126 IME.
fn two_layer_scene() -> Compositor {
    let mut c = new_compositor();
    let app = c.create_layer(None, "app", 0);
    c.set_buffer(app, content_bitmap(1), 1, 1).unwrap();
    let ime = c.create_layer(None, "ime", 10);
    c.set_buffer(ime, ime_bitmap(), 1, 1).unwrap();
    c.apply(&acos_wm::compositor::Transaction::SetPosition {
        layer: ime,
        position: PointI::new(0, (H - IME_H) as i32),
    })
    .unwrap();
    c
}

/// Composite-only full-damage time (first present / invalidate path).
fn bench_compose_full(scene: Compositor) -> Vec<f64> {
    let mut c = scene;
    (0..SAMPLES)
        .map(|_| {
            c.invalidate();
            let t = Instant::now();
            black_box(c.compose_pending_for_bench().unwrap());
            t.elapsed().as_nanos() as f64
        })
        .collect()
}

/// End-to-end full-damage present (compose + `acos-render` commit).
fn bench_present_full(scene: Compositor) -> Vec<f64> {
    let mut c = scene;
    (0..SAMPLES)
        .map(|i| {
            c.invalidate();
            let t = Instant::now();
            black_box(c.present(i as i64).unwrap());
            t.elapsed().as_nanos() as f64
        })
        .collect()
}

/// Composite-only time for a 200×100 damage region.
fn bench_compose_damage(scene: Compositor) -> Vec<f64> {
    let mut c = scene;
    let small = c.create_layer(None, "toast", 100);
    let buf = small_bitmap();
    (0..SAMPLES)
        .map(|i| {
            // Commit outside the timed region (its opacity scan is measured
            // separately); the commit itself is what creates the damage.
            c.set_buffer(small, buf.clone(), i as u64, i as i64)
                .unwrap();
            let t = Instant::now();
            black_box(c.compose_pending_for_bench().unwrap());
            t.elapsed().as_nanos() as f64
        })
        .collect()
}

/// End-to-end time for a 200×100 damage region.
fn bench_present_damage(scene: Compositor) -> Vec<f64> {
    let mut c = scene;
    let small = c.create_layer(None, "toast", 100);
    let buf = small_bitmap();
    (0..SAMPLES)
        .map(|i| {
            c.set_buffer(small, buf.clone(), i as u64, i as i64)
                .unwrap();
            let t = Instant::now();
            black_box(c.present(i as i64).unwrap());
            t.elapsed().as_nanos() as f64
        })
        .collect()
}

/// A no-change present must be a pure check (no compose, no commit).
fn bench_present_noop(scene: Compositor) -> Vec<f64> {
    let mut c = scene;
    c.present(0).unwrap();
    (0..SAMPLES)
        .map(|i| {
            let t = Instant::now();
            let out = c.present(i as i64).unwrap();
            assert!(out.is_none());
            t.elapsed().as_nanos() as f64
        })
        .collect()
}

/// The cost of committing a fresh opaque 1080×2400 buffer (copy + opacity scan).
fn bench_commit_full(declared_alpha: bool) -> Vec<f64> {
    let mut c = new_compositor();
    let layer = c.create_layer(None, "app", 0);
    let buf = if declared_alpha {
        content_bitmap_rgba(1)
    } else {
        content_bitmap(1)
    };
    (0..SAMPLES)
        .map(|i| {
            let t = Instant::now();
            c.set_buffer(layer, buf.clone(), i as u64, i as i64)
                .unwrap();
            t.elapsed().as_nanos() as f64
        })
        .collect()
}

// --- legacy reference (pre-damage compositor path) -------------------------

fn legacy_blend_argb(src: u32, src_alpha: f32, dst: u32) -> u32 {
    let sr = ((src >> 16) & 0xff) as f32;
    let sg = ((src >> 8) & 0xff) as f32;
    let sb = (src & 0xff) as f32;
    let da = ((dst >> 24) & 0xff) as f32 / 255.0;
    let dr = ((dst >> 16) & 0xff) as f32;
    let dg = ((dst >> 8) & 0xff) as f32;
    let db = (dst & 0xff) as f32;
    let out_a = src_alpha + da * (1.0 - src_alpha);
    let mix = |s: f32, d: f32| -> u32 {
        if out_a <= 0.0 {
            0
        } else {
            (((s * src_alpha + d * da * (1.0 - src_alpha)) / out_a).clamp(0.0, 255.0)) as u32
        }
    };
    let a = (out_a * 255.0).clamp(0.0, 255.0) as u32;
    (a << 24) | (mix(sr, dr) << 16) | (mix(sg, dg) << 8) | mix(sb, db)
}

fn legacy_blit(out: &mut Bitmap, src: &Bitmap, position: PointI, alpha: f32) {
    let alpha = alpha.clamp(0.0, 1.0);
    for sy in 0..src.height() as i32 {
        for sx in 0..src.width() as i32 {
            let dx = position.x + sx;
            let dy = position.y + sy;
            if dx < 0 || dy < 0 || dx >= out.width() as i32 || dy >= out.height() as i32 {
                continue;
            }
            let s = src.get_pixel(sx, sy);
            let sa = ((s >> 24) & 0xff) as f32 / 255.0 * alpha;
            if sa <= 0.0 {
                continue;
            }
            let d = out.get_pixel(dx, dy);
            out.set_pixel(dx, dy, legacy_blend_argb(s, sa, d));
        }
    }
}

/// Old `Compositor::compose`: fresh full-frame bitmap + per-pixel blits +
/// `to_pixmap` copy, for a single opaque full-screen layer.
fn bench_legacy_single() -> Vec<f64> {
    let app = content_bitmap(1);
    (0..SAMPLES)
        .map(|_| {
            let t = Instant::now();
            let mut out = Bitmap::new(W, H, BitmapConfig::Rgba8888).unwrap();
            out.erase(0xff00_0000);
            legacy_blit(&mut out, &app, PointI::new(0, 0), 1.0);
            let _ = out.to_pixmap().unwrap();
            t.elapsed().as_nanos() as f64
        })
        .collect()
}

/// Old path for the two-layer scene.
fn bench_legacy_two_layer() -> Vec<f64> {
    let app = content_bitmap(1);
    let ime = ime_bitmap();
    (0..SAMPLES)
        .map(|_| {
            let t = Instant::now();
            let mut out = Bitmap::new(W, H, BitmapConfig::Rgba8888).unwrap();
            out.erase(0xff00_0000);
            legacy_blit(&mut out, &app, PointI::new(0, 0), 1.0);
            legacy_blit(&mut out, &ime, PointI::new(0, (H - IME_H) as i32), 1.0);
            let _ = out.to_pixmap().unwrap();
            t.elapsed().as_nanos() as f64
        })
        .collect()
}

// --- input / relayout / idle ------------------------------------------------

fn session() -> SessionWm {
    SessionWm::with_clock(display(), Arc::new(RealClock::new())).unwrap()
}

fn add_window(s: &mut SessionWm, id: u64) {
    s.add_window(BinderId(id), LayoutParams::application("pkg", "Main"), 0)
        .unwrap();
}

fn bench_inject_latency() -> Vec<f64> {
    let mut s = session();
    add_window(&mut s, 1);
    (0..SAMPLES)
        .map(|i| {
            let t = Instant::now();
            let seqs = s.tap(540.0, 427.0, i as i64).unwrap().seqs;
            for seq in seqs {
                black_box(s.ack(seq).unwrap());
            }
            t.elapsed().as_nanos() as f64
        })
        .collect()
}

fn bench_relayout() -> Vec<f64> {
    let mut s = session();
    add_window(&mut s, 1);
    let params = LayoutParams::application("pkg", "Main");
    (0..SAMPLES)
        .map(|_| {
            let t = Instant::now();
            black_box(s.relayout(1, params.clone(), -1).unwrap());
            t.elapsed().as_nanos() as f64
        })
        .collect()
}

fn proc_self_ticks() -> (u64, u64) {
    let stat = std::fs::read_to_string("/proc/self/stat").unwrap_or_default();
    let Some(close) = stat.rfind(')') else {
        return (0, 0);
    };
    let rest: Vec<&str> = stat[close + 1..].split_whitespace().collect();
    let utime = rest.get(11).and_then(|s| s.parse().ok()).unwrap_or(0);
    let stime = rest.get(12).and_then(|s| s.parse().ok()).unwrap_or(0);
    (utime, stime)
}

fn bench_idle() -> (u64, u64, u64, u64) {
    let clock = Arc::new(RealClock::new());
    let mut s = SessionWm::with_clock(display(), clock.clone()).unwrap();
    add_window(&mut s, 1);
    s.relayout(1, LayoutParams::application("pkg", "Main"), -1)
        .unwrap();
    let arms_before = clock.arm_count();
    let (u0, s0) = proc_self_ticks();
    let t = Instant::now();
    while t.elapsed() < Duration::from_secs(5) {
        std::thread::sleep(Duration::from_millis(200));
    }
    let (u1, s1) = proc_self_ticks();
    (
        clock.arm_count() - arms_before,
        u1 - u0,
        s1 - s0,
        t.elapsed().as_millis() as u64,
    )
}

fn main() {
    let mut out = String::new();
    out.push_str("# acos-wm benchmark (release)\n\n");

    out.push_str("## Compositor (1080x2400, p50/p95/mean nanoseconds)\n\n");
    out.push_str("| operation | p50 (ns) | p95 (ns) | mean (ns) |\n|---|---:|---:|---:|\n");
    out.push_str(&row(
        "legacy compose+to_pixmap, 1 opaque layer",
        bench_legacy_single(),
    ));
    out.push('\n');
    out.push_str(&row(
        "legacy compose+to_pixmap, app+IME",
        bench_legacy_two_layer(),
    ));
    out.push('\n');
    out.push_str(&row("compose, 1 opaque layer (full damage)", bench_compose_full({
        let mut c = new_compositor();
        let l = c.create_layer(None, "app", 0);
        c.set_buffer(l, content_bitmap(1), 1, 1).unwrap();
        c
    })));
    out.push('\n');
    out.push_str(&row("present, 1 opaque layer (full damage)", bench_present_full({
        let mut c = new_compositor();
        let l = c.create_layer(None, "app", 0);
        c.set_buffer(l, content_bitmap(1), 1, 1).unwrap();
        c
    })));
    out.push('\n');
    out.push_str(&row("compose, 1 opaque layer (200x100 damage)", bench_compose_damage({
        let mut c = new_compositor();
        let l = c.create_layer(None, "app", 0);
        c.set_buffer(l, content_bitmap(1), 1, 1).unwrap();
        c
    })));
    out.push('\n');
    out.push_str(&row("present, 1 opaque layer (200x100 damage)", bench_present_damage({
        let mut c = new_compositor();
        let l = c.create_layer(None, "app", 0);
        c.set_buffer(l, content_bitmap(1), 1, 1).unwrap();
        c
    })));
    out.push('\n');
    out.push_str(&row("compose, app + translucent IME (full)", bench_compose_full(two_layer_scene())));
    out.push('\n');
    out.push_str(&row("present, app + translucent IME (full)", bench_present_full(two_layer_scene())));
    out.push('\n');
    out.push_str(&row("compose, app + IME (200x100 damage)", bench_compose_damage(two_layer_scene())));
    out.push('\n');
    out.push_str(&row("present, app + IME (200x100 damage)", bench_present_damage(two_layer_scene())));
    out.push('\n');
    out.push_str(&row("present, no change (skipped)", bench_present_noop(two_layer_scene())));
    out.push('\n');
    out.push_str(&row(
        "commit 1080x2400 buffer, no-alpha declared",
        bench_commit_full(false),
    ));
    out.push('\n');
    out.push_str(&row(
        "commit 1080x2400 buffer, Rgba8888 (opacity scan)",
        bench_commit_full(true),
    ));
    out.push('\n');

    out.push_str("\n## Input / relayout (nanoseconds)\n\n");
    out.push_str("| operation | p50 (ns) | p95 (ns) | mean (ns) |\n|---|---:|---:|---:|\n");
    out.push_str(&row("inject → deliver → ack", bench_inject_latency()));
    out.push('\n');
    out.push_str(&row("relayout compute + reply", bench_relayout()));
    out.push('\n');

    let (arms, utime, stime, elapsed_ms) = bench_idle();
    out.push_str("\n## Idle (5 s, no events)\n\n");
    out.push_str(&format!(
        "- timer arms while idle: **{arms}** (must be 0)\n- /proc/self/stat utime delta: **{utime}** ticks\n- /proc/self/stat stime delta: **{stime}** ticks\n- wall time: {elapsed_ms} ms\n"
    ));
    println!("{out}");

    let path = acos_wm::replay::workspace_root().join("results/wm/bench.txt");
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(&path, &out);
}
