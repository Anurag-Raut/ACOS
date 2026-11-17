//! Load-time benchmark. Run in release:
//!
//! ```text
//! cargo run --release -p acos-native-loader --example bench
//! ```
//!
//! The `host dlopen` row is a rough reference only: glibc's dynamic linker
//! does much more work than this loader, and `libhostref.so` is a glibc
//! library, so it is **not** like-for-like.

use acos_native_loader::{Loader, LoaderConfig, LogSink};
use serde::Serialize;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

#[derive(Serialize)]
struct Stats {
    library: String,
    engine: String,
    iterations: usize,
    p50_ns: u128,
    p95_ns: u128,
    mean_ns: u128,
}

struct NullSink;
impl LogSink for NullSink {
    fn log(&self, _priority: i32, _tag: &str, _message: &str) {}
}

fn summarize(mut times: Vec<u128>) -> (u128, u128, u128) {
    times.sort_unstable();
    let p50 = times[times.len() / 2];
    let p95 = times[(times.len() * 95 / 100).min(times.len() - 1)];
    let mean = times.iter().sum::<u128>() / times.len() as u128;
    (p50, p95, mean)
}

fn main() {
    let iters: usize = std::env::var("ACOS_BENCH_ITERS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(200);
    let data = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/data");
    let loader = Loader::new(
        LoaderConfig::for_tests_without_confinement()
            .with_search_path(data.clone())
            .with_log_sink(Arc::new(NullSink)),
    );

    let mut results = Vec::new();

    for name in ["libhellojni.so", "libtestcpp.so"] {
        let path = data.join(name);
        // Warm up.
        for _ in 0..10 {
            let h = loader.load_file(&path).unwrap();
            loader.unload(&h).unwrap();
        }
        let mut times = Vec::with_capacity(iters);
        for _ in 0..iters {
            let t = Instant::now();
            let h = loader.load_file(&path).unwrap();
            loader.unload(&h).unwrap();
            times.push(t.elapsed().as_nanos());
        }
        let (p50, p95, mean) = summarize(times);
        results.push(Stats {
            library: name.to_string(),
            engine: "acos-native-loader".to_string(),
            iterations: iters,
            p50_ns: p50,
            p95_ns: p95,
            mean_ns: mean,
        });
    }

    // Host dlopen reference.
    let host = data.join("libhostref.so");
    if let Ok(cpath) = std::ffi::CString::new(host.to_string_lossy().as_bytes()) {
        let mut times = Vec::with_capacity(iters);
        for _ in 0..iters {
            let t = Instant::now();
            // SAFETY: valid NUL-terminated path; dlclose the same handle.
            unsafe {
                let h = libc::dlopen(cpath.as_ptr(), libc::RTLD_NOW);
                assert!(!h.is_null(), "host dlopen failed");
                libc::dlclose(h);
            }
            times.push(t.elapsed().as_nanos());
        }
        let (p50, p95, mean) = summarize(times);
        results.push(Stats {
            library: "libhostref.so".to_string(),
            engine: "host dlopen (glibc reference, NOT like-for-like)".to_string(),
            iterations: iters,
            p50_ns: p50,
            p95_ns: p95,
            mean_ns: mean,
        });
    }

    let json = serde_json::to_string_pretty(&results).unwrap();
    let out = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../results/native-loader/load_times.json");
    if let Some(dir) = out.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    std::fs::write(&out, &json).unwrap();
    println!("{json}");
    println!("wrote {}", out.display());
}
