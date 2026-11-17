//! Micro-benchmarks for the Parcel codec, run with `cargo bench -p acos-parcel`.
//!
//! The launch trace called `nativeReadInt` alone ~1,835 times, so per-op cost
//! matters. This harness reports **p50/p95 ns per operation** plus a 100-call
//! synthetic AIDL transaction replay. It is deliberately dependency-free so
//! `BENCH.md` can be regenerated with a stock release build.

use std::hint::black_box;
use std::time::Instant;

use acos_parcel::binder::{BinderId, BinderKind};
use acos_parcel::parcel::Parcel;

const SAMPLES: usize = 401;
const ITERS: usize = 2_000;

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

fn row(name: &str, samples: Vec<f64>) -> (String, (f64, f64, f64)) {
    let s = stats(samples);
    (
        format!("| {name} | {:.2} | {:.2} | {:.2} |", s.0, s.1, s.2),
        s,
    )
}

fn bench_write_int32() -> Vec<f64> {
    let mut p = Parcel::with_capacity(ITERS * 4);
    (0..SAMPLES)
        .map(|_| {
            let t = Instant::now();
            p.set_data_position(0);
            for i in 0..ITERS {
                p.write_int32(black_box(i as i32));
            }
            t.elapsed().as_nanos() as f64 / ITERS as f64
        })
        .collect()
}

fn bench_read_int32() -> Vec<f64> {
    let mut p = Parcel::with_capacity(ITERS * 4);
    for i in 0..ITERS {
        p.write_int32(i as i32);
    }
    (0..SAMPLES)
        .map(|_| {
            let t = Instant::now();
            p.set_data_position(0);
            for _ in 0..ITERS {
                black_box(p.read_int32().unwrap());
            }
            t.elapsed().as_nanos() as f64 / ITERS as f64
        })
        .collect()
}

const DESC: &str = "android.hardware.display.IDisplayManager";

fn bench_write_string16() -> Vec<f64> {
    // 88 bytes on the wire (4 + 80 + 2, padded); pre-grow so we measure the op.
    let mut p = Parcel::with_capacity(ITERS * 88);
    (0..SAMPLES)
        .map(|_| {
            let t = Instant::now();
            for _ in 0..ITERS {
                p.set_data_position(0);
                p.write_string16(black_box(DESC)).unwrap();
            }
            t.elapsed().as_nanos() as f64 / ITERS as f64
        })
        .collect()
}

fn bench_read_string16() -> Vec<f64> {
    let mut p = Parcel::with_capacity(ITERS * 88);
    for _ in 0..ITERS {
        p.write_string16(DESC).unwrap();
    }
    (0..SAMPLES)
        .map(|_| {
            let t = Instant::now();
            p.set_data_position(0);
            for _ in 0..ITERS {
                black_box(p.read_string16().unwrap());
            }
            t.elapsed().as_nanos() as f64 / ITERS as f64
        })
        .collect()
}

/// One synthetic AIDL transaction: interface token + strong binder + two ints +
/// a String16 + marshall, exactly the shape of the measured
/// `IWindowSession`/`IDisplayManager` proxies.
fn synthetic_call(p: &mut Parcel, id: BinderId) {
    p.set_data_position(0);
    p.write_interface_token(DESC).unwrap();
    p.write_strong_binder_kind(id, BinderKind::Proxy);
    p.write_int32(1080);
    p.write_int32(2337);
    p.write_string16("Built-in Screen").unwrap();
    black_box(p.marshall());
}

fn bench_call_replay() -> Vec<f64> {
    const CALLS: usize = 100;
    let mut p = Parcel::with_capacity(4096);
    let id = BinderId(0x2a);
    (0..SAMPLES)
        .map(|_| {
            let t = Instant::now();
            for _ in 0..CALLS {
                synthetic_call(&mut p, id);
            }
            t.elapsed().as_nanos() as f64 / CALLS as f64
        })
        .collect()
}

fn main() {
    let call_samples = bench_call_replay();
    let call_stats = stats(call_samples.clone());
    let rows = vec![
        row("writeInt32", bench_write_int32()),
        row("readInt32", bench_read_int32()),
        row("writeString16 (40 units)", bench_write_string16()),
        row("readString16 (40 units)", bench_read_string16()),
        row("100-call AIDL replay (per call)", call_samples),
    ];

    println!("# acos-parcel micro-benchmarks\n");
    println!(
        "Release build, {} samples x {} iterations per sample.\n",
        SAMPLES, ITERS
    );
    println!("| Benchmark | p50 (ns) | p95 (ns) | mean (ns) |");
    println!("|---|---:|---:|---:|");
    for (line, _) in &rows {
        println!("{line}");
    }
    println!();
    println!(
        "100-call replay total: p50 {:.0} ns, p95 {:.0} ns",
        call_stats.0 * 100.0,
        call_stats.1 * 100.0
    );
}
