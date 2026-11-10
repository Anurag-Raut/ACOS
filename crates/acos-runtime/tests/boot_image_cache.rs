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

fn real_options() -> Option<VmOptions> {
    let boot_dir = acos_vm::vm::default_boot_dir(None)?;
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
    let dex_path = repo_root().join("fixtures/build/dexprogs/01_hello/classes.dex");
    let Ok(bytes) = std::fs::read(&dex_path) else {
        eprintln!("SKIP cached_snapshot_restores_a_runnable_vm: fixture missing");
        return;
    };
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
