//! PERF-REL3b: what the manifest parses cost on the ART launch path.
//!
//! One ART launch (`acos-art` `session::launch` + `launch::launch`) reads the
//! APK's binary manifest and zip index several times. The call sites, default
//! options (`until = application-created`, no `--activity`):
//!
//! 1. `acos-art/src/session.rs` `apk_activity` -> `manifest_resolve::launcher_activity`
//! 2. `acos-art/src/launch.rs` `native_libs::install` -> `Apk::open` (zip index + dex copy)
//! 3. `acos-art/src/launch.rs` `pms::manifest_labels` -> `Apk::open` + `manifest()`
//! 4. `acos-art/src/launch.rs` `manifest_resolve::launch_budget` -> `Apk::open`
//! 5. `acos-art/src/launch.rs` `launcher_activity` (config changes) -> `Apk::open` + AXML DOM
//!
//! This file measures one call of each parse on real APKs and checks the
//! results, so the per-launch sum can be compared with 1 ms.
//!
//! PERF-REL3b: the five call sites now share one open per APK through
//! `acos_services::apk_facts` (a cache keyed by path, length and mtime). The
//! per-call rows below clear the cache before each sample, so they stay the
//! uncached cost of one call (the "before" numbers). The launch rows model one
//! launch in call order: the cold sequence (first call opens, the rest hit) and
//! a warm sequence (a second launch of the same APK).
//!
//! * Default (`cargo test`): one iteration per call. A correctness smoke only;
//!   it prints nothing and makes no timing claim.
//! * Measurement: `ACOS_MANIFEST_TIMING_ITERS=100 cargo test --release -p
//!   acos-services --test manifest_launch_timing -- --nocapture`, run through
//!   `tools/art-runtime/run-exclusive.sh --exclusive` so the numbers are not
//!   taken under bench/correctness contention.

use std::hint::black_box;
use std::path::{Path, PathBuf};
use std::time::Instant;

use acos_services::apk_facts::{clear_cache, open_count, open_shared, prime_from};
use acos_services::manifest_resolve::{
    launch_budget, launcher_activity, RESUME_MS_BASE, RESUME_MS_LARGE,
};
use acos_services::pms::manifest_labels;

struct Target {
    name: &'static str,
    /// Path relative to the repository root.
    rel: &'static str,
    /// Expected `LauncherActivity::component`.
    launcher: &'static str,
    /// Expected `LaunchBudget::resume_ms`.
    resume_ms: u64,
}

const TARGETS: &[Target] = &[
    Target {
        name: "counter",
        rel: "fixtures/build/apps/counter.apk",
        launcher: "dev.acos.fixtures.counter.MainActivity",
        resume_ms: RESUME_MS_BASE,
    },
    Target {
        name: "form-validation",
        rel: "fixtures/build/apps/form-validation.apk",
        launcher: "dev.acos.fixtures.formvalidation.MainActivity",
        resume_ms: RESUME_MS_BASE,
    },
    Target {
        name: "gaten",
        rel: ".cache/gaten/gaten-dev-f344272-x86_64-release-unsigned.apk",
        launcher: "com.gaten.app.LauncherIconLight",
        resume_ms: RESUME_MS_LARGE,
    },
];

fn repo_path(rel: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(rel)
}

fn iterations() -> usize {
    std::env::var("ACOS_MANIFEST_TIMING_ITERS")
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|n: &usize| *n > 0)
        .unwrap_or(1)
}

/// Run `f` `n` times; return per-call wall time in milliseconds.
fn time_ms<T>(n: usize, mut f: impl FnMut() -> T) -> Vec<f64> {
    (0..n)
        .map(|_| {
            let start = Instant::now();
            black_box(f());
            start.elapsed().as_secs_f64() * 1000.0
        })
        .collect()
}

fn percentile(sorted: &[f64], p: f64) -> f64 {
    // Nearest-rank on the sorted samples.
    let rank = ((p / 100.0) * sorted.len() as f64).ceil() as usize;
    sorted[rank.clamp(1, sorted.len()) - 1]
}

fn report(apk: &str, call: &str, mut samples: Vec<f64>) -> f64 {
    samples.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let mean = samples.iter().sum::<f64>() / samples.len() as f64;
    let p50 = percentile(&samples, 50.0);
    let p95 = percentile(&samples, 95.0);
    eprintln!(
        "TIMING apk={apk} call={call} n={} mean_ms={mean:.3} p50_ms={p50:.3} p95_ms={p95:.3}",
        samples.len()
    );
    p50
}

#[test]
fn manifest_parses_are_correct_and_timed_per_call() {
    let n = iterations();
    for t in TARGETS {
        let path = repo_path(t.rel);
        if !path.is_file() {
            // HYG2: a missing APK is a missing prerequisite, not a quiet
            // `continue` — the loop used to report `ok` having timed nothing.
            acos_test_support::require_prereq(
                false,
                &format!("manifest_parses_are_correct_and_timed_per_call apk={}", t.name),
                &format!(
                    "{} is missing — run tools/build-apps.sh (and link .cache/gaten), or \
                     set ACOS_ALLOW_ART_SKIP=1 for a counted skip",
                    path.display()
                ),
            );
            continue;
        }

        // Correctness first: these are the values the launch path consumes.
        clear_cache();
        let got = launcher_activity(&path).expect("launcher_activity");
        assert_eq!(got.component, t.launcher, "{}: launcher", t.name);
        let budget = launch_budget(&path).expect("launch_budget");
        assert_eq!(budget.resume_ms, t.resume_ms, "{}: resume budget", t.name);
        let labels = manifest_labels(&path).expect("manifest_labels");
        assert!(!labels.activities.is_empty(), "{}: activities", t.name);

        // Uncached cost of one call (cache cleared before each sample). The zip
        // index is the dominant cost: Apk::open reads the whole file and copies
        // every dex.
        let open = time_ms(n, || acos_apk::Apk::open(&path).expect("open"));
        let open_dom = time_ms(n, || {
            let apk = acos_apk::Apk::open(&path).expect("open");
            apk.manifest_dom().expect("dom")
        });
        let launcher = time_ms(n, || {
            clear_cache();
            launcher_activity(&path).expect("launcher")
        });
        let budget_t = time_ms(n, || {
            clear_cache();
            launch_budget(&path).expect("budget")
        });
        let labels_t = time_ms(n, || {
            clear_cache();
            manifest_labels(&path).expect("labels")
        });

        let apk = t.name;
        let p_open = report(apk, "Apk::open", open);
        let p_dom = report(apk, "Apk::open+manifest_dom", open_dom);
        let p_launcher = report(apk, "launcher_activity(uncached)", launcher);
        let p_budget = report(apk, "launch_budget(uncached)", budget_t);
        let p_labels = report(apk, "manifest_labels(uncached)", labels_t);

        // Before (PERF-REL3b base, five independent opens on the default ART
        // path): launcher (apk_activity) + native_libs open + labels + budget +
        // launcher (config changes). Sum of p50s.
        let before = p_launcher + p_open + p_labels + p_budget + p_launcher;
        eprintln!(
            "TIMING apk={apk} call=per-launch-model-before opens=5 sum_p50_ms={before:.3} \
             (open_dom_p50_ms={p_dom:.3})"
        );

        // After (PERF-REL3c): the launch sequence the ART launch now runs. Its one
        // source open is `open_shared` (the native-library install reads it through
        // `install_from`; its extraction is not in this model, it is the same before
        // and after), `prime_from` caches the facts from that open, and the manifest
        // calls are cache hits. The open is dropped at the end of the sequence.
        let sequence = || {
            let src = open_shared(&path).expect("launch open");
            prime_from(&path, &src);
            let a = launcher_activity(&path).expect("launcher");
            let b = manifest_labels(&path).expect("labels");
            let c = launch_budget(&path).expect("budget");
            let d = launcher_activity(&path).expect("launcher, config changes");
            (a, b, c, d)
        };
        // Correctness of the claim: one cold launch opens the APK exactly once.
        clear_cache();
        let opens0 = open_count();
        let _ = sequence();
        assert_eq!(
            open_count() - opens0,
            1,
            "{}: one cold launch must open its APK once",
            t.name
        );
        let cold = time_ms(n, || {
            clear_cache();
            sequence()
        });
        // Warm: the facts of a previous launch are cached; the launch still opens
        // once, for its native-library install.
        let _ = sequence();
        let warm = time_ms(n, sequence);
        let p_cold = report(apk, "launch-sequence-after(cold)", cold);
        let p_warm = report(apk, "launch-sequence-after(warm)", warm);
        eprintln!(
            "TIMING apk={apk} call=per-launch-model-after cold_p50_ms={p_cold:.3} \
             warm_p50_ms={p_warm:.3} before_p50_ms={before:.3}"
        );
    }
}
