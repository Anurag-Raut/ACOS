//! Micro-benchmarks for `acos-wm`, run with `cargo bench -p acos-wm`.
//!
//! Reports p50/p95 for the agent-visible operations (inject→deliver→ack,
//! relayout compute, a 1080×2400 compositor present) and measures idle CPU over
//! five seconds: the state machines must contribute **zero** wakeups and no
//! measurable `/proc/self/stat` utime/stime.
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
use acos_wm::display::DisplaySpec;
use acos_wm::facade::SessionWm;

const SAMPLES: usize = 201;

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

fn session() -> SessionWm {
    SessionWm::with_clock(
        DisplaySpec::reference_1080x2400(),
        Arc::new(RealClock::new()),
    )
    .unwrap()
}

fn add_window(s: &mut SessionWm, id: u64) {
    s.add_window(BinderId(id), LayoutParams::application("pkg", "Main"), 0)
        .unwrap();
}

/// Time a full inject → deliver → ack round trip.
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

/// Time `IWindowSession.relayout` argument decode + frame computation + reply.
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

/// Time a compositor present of a 3-layer 1080×2400 stack.
fn bench_present() -> Vec<f64> {
    let mut s = session();
    add_window(&mut s, 1);
    let layer = s.compositor.layer_for_window(1).unwrap();
    let make = || {
        let mut b = Bitmap::new(1080, 2400, BitmapConfig::Rgba8888).unwrap();
        b.erase(0xff10_2030);
        b
    };
    (0..SAMPLES)
        .map(|i| {
            let t = Instant::now();
            s.compositor
                .set_buffer(layer, make(), i as u64, i as i64)
                .unwrap();
            black_box(s.compositor.present(i as i64).unwrap());
            t.elapsed().as_nanos() as f64
        })
        .collect()
}

/// `/proc/self/stat` `(utime, stime)` in clock ticks.
fn proc_self_ticks() -> (u64, u64) {
    let stat = std::fs::read_to_string("/proc/self/stat").unwrap_or_default();
    // Fields after the comm field (which is in parentheses and may contain
    // spaces): state is field 3, utime field 14, stime field 15.
    let Some(close) = stat.rfind(')') else {
        return (0, 0);
    };
    let rest: Vec<&str> = stat[close + 1..].split_whitespace().collect();
    let utime = rest.get(11).and_then(|s| s.parse().ok()).unwrap_or(0);
    let stime = rest.get(12).and_then(|s| s.parse().ok()).unwrap_or(0);
    (utime, stime)
}

/// Measure idle CPU and wakeups over five seconds.
fn bench_idle() -> (u64, u64, u64, u64) {
    let clock = Arc::new(RealClock::new());
    let mut s = SessionWm::with_clock(DisplaySpec::reference_1080x2400(), clock.clone()).unwrap();
    add_window(&mut s, 1);
    // Let the window settle (relayout/resized are delivered on add).
    s.relayout(1, LayoutParams::application("pkg", "Main"), -1)
        .unwrap();
    let arms_before = clock.arm_count();
    let (u0, s0) = proc_self_ticks();
    let t = Instant::now();
    while t.elapsed() < Duration::from_secs(5) {
        // The state machine is never poked; a real runtime would be blocked in
        // epoll_wait(-1). We sleep here only to keep the bench alive.
        std::thread::sleep(Duration::from_millis(200));
    }
    let (u1, s1) = proc_self_ticks();
    let arms_after = clock.arm_count();
    (
        arms_after - arms_before,
        u1 - u0,
        s1 - s0,
        t.elapsed().as_millis() as u64,
    )
}

fn main() {
    let inject = row("inject → deliver → ack (ns)", bench_inject_latency());
    let relayout = row("relayout compute + reply (ns)", bench_relayout());
    let present = row("compositor present 1080×2400 (ns)", bench_present());
    let (arms, utime, stime, elapsed_ms) = bench_idle();

    let mut out = String::new();
    out.push_str("# acos-wm benchmark (release)\n\n");
    out.push_str("| operation | p50 (ns) | p95 (ns) | mean (ns) |\n|---|---:|---:|---:|\n");
    out.push_str(&inject);
    out.push('\n');
    out.push_str(&relayout);
    out.push('\n');
    out.push_str(&present);
    out.push('\n');
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

