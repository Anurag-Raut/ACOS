//! T-43b acceptance tests: the on-disk content-addressed boot-image cache.
//!
//! The unit tests in `crates/acos-runtime/src/boot_image_cache.rs` cover the
//! byte-level contract (round trip, corruption/truncation/oversize rejection,
//! concurrency, eviction, key invalidation). These integration tests cover the
//! two paths a caller actually takes:
//!
//! * a snapshot published and loaded back through the cache restores to a
//!   VM that runs the same program (no cache artefacts in the result);
//! * `RuntimeTemplate::boot_with_options` uses a warm cache entry instead of
//!   re-booting the framework, and a corrupt entry falls back to the real boot
//!   and is repaired.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use acos_dex::DexFile;
use acos_runtime::boot_image_cache::{BootImageCache, LoadResult};
use acos_runtime::session_pool::RuntimeTemplate;
use acos_vm::vm::{CoreLib, Vm, VmOptions};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> TempDir {
        static NONCE: AtomicU64 = AtomicU64::new(0);
        let mut path = std::env::temp_dir();
        path.push(format!(
            "acos-w96a-it-{tag}-{}-{}",
            std::process::id(),
            NONCE.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&path).expect("temp dir");
        TempDir(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The gitignored boot-classpath fixture every template test needs.
const BOOT_FIXTURE: &str = ".cache/boot/android-34/BOOTCLASSPATH";

/// Real-corelib options, or `None` when the boot classpath is absent.
///
/// The boot classpath is a gitignored artifact, so the skip is routed through
/// `have_fixture`: under `ACOS_REQUIRE_FIXTURES=1` a missing classpath is a
/// hard failure, not a green SKIP (W170-A H5).
fn real_options() -> Option<VmOptions> {
    let boot_dir = match acos_vm::vm::default_boot_dir(None) {
        Some(dir) => dir,
        None => {
            let _ = acos_test_support::have_fixture(BOOT_FIXTURE);
            return None;
        }
    };
    Some(VmOptions {
        corelib: CoreLib::Real,
        boot_dir: Some(boot_dir),
        ..VmOptions::default()
    })
}

/// A snapshot published through the cache must restore to a VM that produces
/// byte-identical output.
#[test]
fn cached_snapshot_restores_a_runnable_vm() {
    const DEX_FIXTURE: &str = "fixtures/build/dexprogs/01_hello/classes.dex";
    if !acos_test_support::have_fixture(DEX_FIXTURE) {
        eprintln!("SKIP cached_snapshot_restores_a_runnable_vm: fixture missing");
        return;
    }
    let dex_path = repo_root().join(DEX_FIXTURE);
    let bytes = std::fs::read(&dex_path).expect("have_fixture guaranteed the DEX");
    let dex = DexFile::parse(bytes).expect("parse fixture");

    let options = VmOptions {
        corelib: CoreLib::Bootstrap,
        ..VmOptions::default()
    };
    let dex = std::sync::Arc::new(dex);
    // The snapshot covers the core provider set only (the T-42 precondition),
    // so capture the bare VM, then add the app DEX after restore.
    let vm = Vm::new(options.clone()).expect("bootstrap vm");
    let snapshot = vm.snapshot().expect("snapshot");

    let mut reference = Vm::new(options).expect("reference vm");
    reference.add_shared_dex(std::sync::Arc::clone(&dex));
    let expected = reference.run("Main", &[]);

    let dir = TempDir::new("roundtrip");
    let cache = BootImageCache::with_dir(dir.path()).expect("cache");
    let key = cache.image_key(None, CoreLib::Bootstrap);
    cache.store(key, &snapshot).expect("store");
    let loaded = match cache.load(key) {
        LoadResult::Hit(bytes) => bytes,
        other => panic!("expected cache hit, got {other:?}"),
    };
    let mut restored = Vm::restore(&loaded).expect("restore");
    restored.add_shared_dex(dex);
    let got = restored.run("Main", &[]);
    assert_eq!(got.exit_code, expected.exit_code, "exit code diverged");
    assert_eq!(got.stdout, expected.stdout, "stdout diverged");
}

/// A warm template entry is used instead of re-booting; the recovered
/// `ActivityThread` handle and class count match the booted template.
#[test]
fn template_cache_hit_skips_the_framework_boot() {
    let Some(options) = real_options() else {
        eprintln!("SKIP template_cache_hit_skips_the_framework_boot: no boot classpath");
        return;
    };
    let dir = TempDir::new("template");
    let cache = BootImageCache::with_dir(dir.path()).expect("cache");

    let first = RuntimeTemplate::boot_with_options(options.clone(), Some(&cache))
        .expect("first template boot");
    assert!(
        !first.from_cache(),
        "the first boot must not be a cache hit"
    );
    let second = RuntimeTemplate::boot_with_options(options, Some(&cache))
        .expect("second template boot");
    assert!(second.from_cache(), "the second boot must hit the cache");
    assert_eq!(
        first.activity_thread(),
        second.activity_thread(),
        "the cached snapshot must preserve the ActivityThread handle"
    );
    assert_eq!(
        first.classes(),
        second.classes(),
        "the cached snapshot must preserve the class count"
    );
    // The restored framework VM must itself be consumable.
    let _rt = second.restore_runtime().expect("restore from cached template");
}

/// The deterministic-class priming (W111-A) survives the snapshot: every class
/// reported primed is `Init` in a fresh restore, so the launch does not re-run
/// its `<clinit>`. The primed set must contain at least `PackagePartitions`,
/// the largest deterministic class on the launch path.
#[test]
fn restored_template_carries_deterministic_clinit() {
    let Some(options) = real_options() else {
        eprintln!("SKIP restored_template_carries_deterministic_clinit: no boot classpath");
        return;
    };
    let dir = TempDir::new("primed");
    let cache = BootImageCache::with_dir(dir.path()).expect("cache");
    let first = RuntimeTemplate::boot_with_options(options, Some(&cache)).expect("template boot");
    assert!(
        first
            .primed_classes()
            .iter()
            .any(|c| c == "Landroid/content/pm/PackagePartitions;"),
        "PackagePartitions must be primed into the template, got {:?}",
        first.primed_classes()
    );
    for desc in first.primed_classes() {
        assert!(
            first
                .restored_class_initialised(desc)
                .expect("restore for init check"),
            "{desc} must be Init after a restore (priming must survive the snapshot)"
        );
    }
}

/// A corrupt entry is rejected, the real boot runs, and the entry is repaired.
#[test]
fn corrupt_template_entry_falls_back_and_reboots() {
    let Some(options) = real_options() else {
        eprintln!("SKIP corrupt_template_entry_falls_back_and_reboots: no boot classpath");
        return;
    };
    let dir = TempDir::new("corrupt");
    let cache = BootImageCache::with_dir(dir.path()).expect("cache");
    let boot_dir = options.boot_dir.clone();
    let key = cache.image_key(boot_dir.as_deref(), options.corelib);

    let first = RuntimeTemplate::boot_with_options(options.clone(), Some(&cache))
        .expect("first template boot");
    assert!(!first.from_cache());
    let path = cache.entry_path(key);
    assert!(path.is_file(), "first boot must publish an entry");

    // Corrupt the payload (last byte) and confirm the caller repairs it.
    let mut bytes = std::fs::read(&path).expect("read entry");
    let last = bytes.len() - 1;
    bytes[last] ^= 0xff;
    std::fs::write(&path, &bytes).expect("write corrupt entry");
    assert!(
        matches!(cache.load(key), LoadResult::Invalid(_)),
        "corrupt entry must be rejected on load"
    );

    let second = RuntimeTemplate::boot_with_options(options, Some(&cache))
        .expect("fallback boot");
    assert!(
        !second.from_cache(),
        "a corrupt entry must fall back to the real boot"
    );
    assert!(
        matches!(cache.load(key), LoadResult::Hit(_)),
        "the fallback boot must repair the entry"
    );
}

// ---------------------------------------------------------------------------
// T-43 binder-identity re-seed: cold-process restore must reach the same
// launch ladder rung as the uncached launch (W102-A).
// ---------------------------------------------------------------------------

/// Environment variable carrying the cache dir to the cold child process.
const COLD_ENV: &str = "W102A_COLD_CACHE";

/// Parse `W102A bind=.. app=.. tx=.. txd=..` from the child's stdout.
fn parse_ladder(line: &str) -> Option<(bool, bool, bool, bool)> {
    let field = |name: &str| -> Option<bool> {
        line.split_whitespace()
            .find_map(|part| part.strip_prefix(name))
            .and_then(|v| match v {
                "true" => Some(true),
                "false" => Some(false),
                _ => None,
            })
    };
    Some((
        field("bind=")?,
        field("app=")?,
        field("tx=")?,
        field("txd=")?,
    ))
}

/// Run the real launch ladder on a `Runtime` restored from the on-disk cache.
///
/// This is a normal test (it no-ops without `W102A_COLD_CACHE`) that is
/// re-executed as a child process by
/// [`cold_restore_matches_uncached_ladder`], so the restore happens in a
/// genuinely fresh process with an empty binder/looper/parcel table.
#[test]
fn cold_restore_helper() {
    let Ok(dir) = std::env::var(COLD_ENV) else {
        return;
    };
    let apk = repo_root().join("fixtures/build/apps/counter.apk");
    let Some(options) = real_options() else {
        println!("W102A skip=no-boot-classpath");
        return;
    };
    let cache = BootImageCache::with_dir(&dir).expect("cache");
    let template = RuntimeTemplate::boot_with_options(options, Some(&cache)).expect("template");
    assert!(
        template.from_cache(),
        "the cold child must be a cache hit, not a fresh boot"
    );
    let thread = template.activity_thread();
    let mut rt = template.restore_runtime().expect("restore from cached template");
    let report = rt.boot_and_run_lifecycle_from_boot(&apk, thread);
    println!(
        "W102A bind={} app={} tx={} txd={}",
        report.bind_delivered,
        report.app_created,
        report.transaction_scheduled,
        report.transaction_delivered
    );
}

/// A cold process restoring the post-attach framework snapshot from the disk
/// cache must reach the same lifecycle rung as the uncached launch.
#[test]
fn cold_restore_matches_uncached_ladder() {
    use std::process::Command;

    const APK_FIXTURE: &str = "fixtures/build/apps/counter.apk";
    if !acos_test_support::have_fixture(APK_FIXTURE) {
        eprintln!("SKIP cold_restore_matches_uncached_ladder: counter.apk missing");
        return;
    }
    let apk = repo_root().join(APK_FIXTURE);
    let Some(options) = real_options() else {
        eprintln!("SKIP cold_restore_matches_uncached_ladder: no boot classpath");
        return;
    };

    // Reference: the real `acos launch` path (fresh Vm + framework boot).
    let mut rt = acos_runtime::Runtime::standard_with(options.clone()).expect("runtime");
    let uncached = rt.boot_and_run_lifecycle(&apk);
    assert!(uncached.bind_delivered, "uncached launch did not bind");
    assert!(uncached.app_created, "uncached launch did not create the app");
    assert!(
        uncached.transaction_scheduled,
        "uncached launch did not reach client_transaction"
    );

    // Populate the on-disk cache from one framework boot + ACOSSNAP.
    let dir = TempDir::new("cold");
    let cache = BootImageCache::with_dir(dir.path()).expect("cache");
    let _ = RuntimeTemplate::boot_with_options(options, Some(&cache)).expect("populate cache");

    // Restore + launch in a genuinely fresh process.
    let exe = std::env::current_exe().expect("current test exe");
    let out = Command::new(&exe)
        .args(["--exact", "cold_restore_helper", "--nocapture"])
        .env(COLD_ENV, dir.path())
        .output()
        .expect("spawn cold helper");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    let line = stdout
        .lines()
        .find(|l| l.starts_with("W102A ") || l.starts_with("W102A bind="))
        .unwrap_or_else(|| {
            panic!(
                "cold helper produced no ladder; status={:?}\nstdout:\n{stdout}\nstderr:\n{stderr}",
                out.status
            )
        });
    if line.contains("skip=") {
        eprintln!("SKIP cold_restore_matches_uncached_ladder: child skipped ({line})");
        return;
    }
    let (bind, app, tx, txd) = parse_ladder(line).expect("parse child ladder");
    assert_eq!(
        (bind, app, tx),
        (
            uncached.bind_delivered,
            uncached.app_created,
            uncached.transaction_scheduled
        ),
        "cold cache restore ladder diverged from the uncached launch: {line}"
    );
    // `txd` is the same on both paths (the counter fixture blocks in
    // `Activity.onCreate` on an unimplemented VectorDrawable native).
    assert_eq!(txd, uncached.transaction_delivered, "activity-on-create rung diverged");
}
