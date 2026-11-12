//! W37: the content-addressed build cache must (a) reuse an artefact without
//! running the builder again, (b) detect a corrupted entry and rebuild it, and
//! (c) serialise a cold cohort so N concurrent builders compile once.
//!
//! Everything lives under `.cache/w37-tests/` (never `/tmp`). `ACOS_ART_BUILD_CACHE`
//! is process-global, so every test (a) takes a process-wide lock, (b) points the
//! cache at its **own** throw-away root and (c) holds both for its lifetime. That
//! makes this binary safe under plain `cargo test` (W53: running in parallel used
//! to let one test observe another's root, or `off`, and fail on a cache HIT it
//! did not create).

use acos_art::build_cache;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

fn scratch(tag: &str) -> PathBuf {
    let base = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.cache/w37-tests");
    let dir = base.join(format!("{}-{tag}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// The process-global cache env, held for the whole test body.
static CACHE_ENV: Mutex<()> = Mutex::new(());

/// Take the env lock, create this test's own scratch root under `.cache` and
/// point `ACOS_ART_BUILD_CACHE` at it. The guard must live as long as the test.
fn isolate(tag: &str) -> (MutexGuard<'static, ()>, PathBuf, PathBuf) {
    let guard = CACHE_ENV.lock().unwrap_or_else(|e| e.into_inner());
    let dir = scratch(tag);
    let root = dir.join("cache");
    std::env::set_var("ACOS_ART_BUILD_CACHE", &root);
    (guard, dir, root)
}

#[test]
fn second_build_is_a_cache_hit() {
    let (_env, dir, root) = isolate("reuse");
    let runs = Arc::new(AtomicUsize::new(0));
    let key = build_cache::digest(&[b"w37-test/reuse"]);

    let build = |runs: Arc<AtomicUsize>| {
        move |stage: &Path| {
            runs.fetch_add(1, Ordering::SeqCst);
            fs::write(stage.join("artifact.bin"), b"first-build").unwrap();
            Ok(())
        }
    };

    let first = build_cache::ensure(&key, &["artifact.bin"], build(Arc::clone(&runs))).unwrap();
    assert!(!first.reused, "cold cache must build");
    assert_eq!(runs.load(Ordering::SeqCst), 1);
    // Staging is inside the cache root (never /tmp), as the directives require.
    let staged = first.staged_from.as_deref().expect("a build has a staging dir");
    assert!(staged.starts_with(root.join(".tmp")), "{}", staged.display());
    assert_eq!(build_cache::cache_root(), root, "the isolated root is what the cache uses");

    let dest = dir.join("dest1");
    build_cache::install(&first, &["artifact.bin"], &dest).unwrap();
    assert_eq!(fs::read(dest.join("artifact.bin")).unwrap(), b"first-build");

    // Second call: a DIFFERENT builder body that would produce different bytes.
    // It must not run at all.
    let second = build_cache::ensure(&key, &["artifact.bin"], |stage| {
        fs::write(stage.join("artifact.bin"), b"should-never-run").unwrap();
        Ok(())
    })
    .unwrap();
    assert!(second.reused, "second call must reuse the entry");
    assert_eq!(runs.load(Ordering::SeqCst), 1, "builder ran twice");
    assert_eq!(second.dir, first.dir, "same content-addressed entry");
    assert_eq!(fs::read(second.path("artifact.bin")).unwrap(), b"first-build");
}

#[test]
fn corrupted_entry_is_detected_and_rebuilt() {
    let (_env, dir, _root) = isolate("corrupt");
    let key = build_cache::digest(&[b"w37-test/corrupt"]);
    let runs = Arc::new(AtomicUsize::new(0));

    let built = build_cache::ensure(&key, &["guard.so"], {
        let runs = Arc::clone(&runs);
        move |stage: &Path| {
            runs.fetch_add(1, Ordering::SeqCst);
            fs::write(stage.join("guard.so"), b"good-bytes").unwrap();
            Ok(())
        }
    })
    .unwrap();
    assert!(!built.reused);
    assert_eq!(runs.load(Ordering::SeqCst), 1);

    // Teeth: flip one byte of the published artefact. Reuse would hand a
    // corrupted .so to every launch; verification must catch it.
    let victim = built.path("guard.so");
    fs::write(&victim, b"bad-bytes!").unwrap();

    let rebuilt = build_cache::ensure(&key, &["guard.so"], {
        let runs = Arc::clone(&runs);
        move |stage: &Path| {
            runs.fetch_add(1, Ordering::SeqCst);
            fs::write(stage.join("guard.so"), b"good-bytes").unwrap();
            Ok(())
        }
    })
    .unwrap();
    assert!(!rebuilt.reused, "a corrupt entry must not be reused");
    assert_eq!(runs.load(Ordering::SeqCst), 2, "the builder must run again");
    assert_eq!(fs::read(rebuilt.path("guard.so")).unwrap(), b"good-bytes");

    // A missing manifest is just as untrusted as a wrong hash.
    fs::remove_file(rebuilt.dir.join(build_cache::MANIFEST)).unwrap();
    let third = build_cache::ensure(&key, &["guard.so"], {
        let runs = Arc::clone(&runs);
        move |stage: &Path| {
            runs.fetch_add(1, Ordering::SeqCst);
            fs::write(stage.join("guard.so"), b"good-bytes").unwrap();
            Ok(())
        }
    })
    .unwrap();
    assert!(!third.reused);
    assert_eq!(runs.load(Ordering::SeqCst), 3);
    assert!(third.dir.join(build_cache::MANIFEST).is_file());
}

#[test]
fn cold_cohort_compiles_once() {
    let (_env, _dir, _root) = isolate("single-flight");
    let key = build_cache::digest(&[b"w37-test/single-flight"]);
    let runs = Arc::new(AtomicUsize::new(0));

    let barrier = Arc::new(std::sync::Barrier::new(8));
    let mut threads = Vec::new();
    for _ in 0..8 {
        let key = key.clone();
        let runs = Arc::clone(&runs);
        let barrier = Arc::clone(&barrier);
        threads.push(std::thread::spawn(move || {
            barrier.wait();
            build_cache::ensure(&key, &["dex.bin"], move |stage| {
                runs.fetch_add(1, Ordering::SeqCst);
                std::thread::sleep(std::time::Duration::from_millis(120));
                fs::write(stage.join("dex.bin"), b"one-compile").unwrap();
                Ok(())
            })
            .unwrap()
        }));
    }
    let mut dirs = Vec::new();
    for t in threads {
        let b = t.join().unwrap();
        assert_eq!(fs::read(b.path("dex.bin")).unwrap(), b"one-compile");
        dirs.push(b.dir);
    }
    assert_eq!(
        runs.load(Ordering::SeqCst),
        1,
        "a cold cohort must compile exactly once (flock single-flight)"
    );
    assert!(dirs.iter().all(|d| d == &dirs[0]));
}

#[test]
fn install_replaces_a_stale_copy_atomically() {
    let (_env, dir, _root) = isolate("install");
    let key = build_cache::digest(&[b"w37-test/install"]);
    let built = build_cache::ensure(&key, &["shim.so"], |stage| {
        fs::write(stage.join("shim.so"), b"fresh").unwrap();
        Ok(())
    })
    .unwrap();

    let dest = dir.join("data");
    fs::create_dir_all(&dest).unwrap();
    fs::write(dest.join("shim.so"), b"stale-partial").unwrap();
    let out = build_cache::install(&built, &["shim.so"], &dest).unwrap();
    assert_eq!(out[0], dest.join("shim.so"));
    assert_eq!(fs::read(&out[0]).unwrap(), b"fresh");
    // No temporary files left behind.
    let leftovers: Vec<_> = fs::read_dir(&dest)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| n != "shim.so")
        .collect();
    assert!(leftovers.is_empty(), "leftovers: {leftovers:?}");
}

#[test]
fn disabled_cache_builds_but_never_publishes() {
    // Same lock as `isolate`, but `off` instead of a directory: no other test in
    // this binary may switch the cache back on while this one runs.
    let _env = CACHE_ENV.lock().unwrap_or_else(|e| e.into_inner());
    let dir = scratch("disabled");
    let root = dir.join("cache");
    std::env::set_var("ACOS_ART_BUILD_CACHE", "off");
    let key = build_cache::digest(&[b"w37-test/disabled"]);
    let built = build_cache::ensure(&key, &["x.bin"], |stage| {
        fs::write(stage.join("x.bin"), b"uncached").unwrap();
        Ok(())
    })
    .unwrap();
    assert!(!built.reused);
    assert!(!root.join(&key).exists(), "disabled cache must not publish");
    std::env::remove_var("ACOS_ART_BUILD_CACHE");
}
