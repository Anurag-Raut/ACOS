//! W39: the derived **tree** entries (linkerconfig, `derive_classpath`, static
//! RRO idmaps) must reuse a verified entry without running the bionic/`idmap2`
//! builder again, and must refuse (and rebuild) an entry that is corrupted —
//! byte flip, missing file, or an unlisted extra file. The image digest that
//! keys them must follow the extracted sysroot, not just its path.
//!
//! Everything lives under `.cache/w39-tests/` (never `/tmp`). `ACOS_ART_BUILD_CACHE`
//! is process-global, so each test takes a process-wide lock and points the cache
//! at its own throw-away root: safe under plain `cargo test` (W53).

use acos_art::build_cache;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

fn scratch(tag: &str) -> PathBuf {
    let base = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.cache/w39-tests");
    let dir = base.join(format!("{}-{tag}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// The process-global cache env, held for the whole test body.
static CACHE_ENV: Mutex<()> = Mutex::new(());

/// Take the env lock, create this test's own scratch root and point
/// `ACOS_ART_BUILD_CACHE` at it. Hold the guard for the whole test.
fn isolate(tag: &str) -> (MutexGuard<'static, ()>, PathBuf) {
    let guard = CACHE_ENV.lock().unwrap_or_else(|e| e.into_inner());
    let dir = scratch(tag);
    std::env::set_var("ACOS_ART_BUILD_CACHE", dir.join("cache"));
    (guard, dir)
}

/// A linkerconfig-shaped source tree: top-level configs plus per-APEX files.
fn write_tree(dir: &Path, marker: &str) {
    fs::create_dir_all(dir.join("com.android.art")).unwrap();
    fs::create_dir_all(dir.join("com.android.runtime")).unwrap();
    fs::write(dir.join("ld.config.txt"), format!("ld {marker}\n")).unwrap();
    fs::write(dir.join("apex.libraries.config.txt"), format!("apex {marker}\n")).unwrap();
    fs::write(dir.join("com.android.art/ld.config.txt"), format!("art {marker}\n")).unwrap();
    fs::write(
        dir.join("com.android.runtime/ld.config.txt"),
        format!("runtime {marker}\n"),
    )
    .unwrap();
}

#[test]
fn tree_entry_reuses_every_nested_file() {
    let (_env, dir) = isolate("tree-reuse");
    let key = build_cache::digest(&[b"w39-test/tree-reuse"]);
    let src = dir.join("src");
    write_tree(&src, "v1");

    let _lock = build_cache::lock(&key).unwrap();
    let built =
        build_cache::publish_tree_locked(&key, &src, &|rel| rel == ".mounts").unwrap();
    assert!(!built.reused, "cold cache must publish");
    assert!(built.dir.join("com.android.art/ld.config.txt").is_file());

    // Reuse installs the whole tree, nested directories included.
    let dest = dir.join("dest");
    let files = build_cache::reuse_tree_to(&key, &dest)
        .unwrap()
        .expect("a published tree must be reusable");
    assert_eq!(files.len(), 4, "every manifest file is installed: {files:?}");
    assert_eq!(
        fs::read_to_string(dest.join("com.android.art/ld.config.txt")).unwrap(),
        "art v1\n"
    );
    assert_eq!(
        fs::read_to_string(dest.join("ld.config.txt")).unwrap(),
        "ld v1\n"
    );
}

#[test]
fn corrupted_nested_file_is_detected_and_reports_miss() {
    let (_env, dir) = isolate("tree-corrupt");
    let key = build_cache::digest(&[b"w39-test/tree-corrupt"]);
    let src = dir.join("src");
    write_tree(&src, "v1");
    let _lock = build_cache::lock(&key).unwrap();
    build_cache::publish_tree_locked(&key, &src, &|_| false).unwrap();

    // Flip one byte of a *nested* file: reuse must refuse the entry and remove
    // it, so the caller's builder runs again instead of serving bad bytes.
    let victim = build_cache::cache_root().join(&key).join("com.android.art/ld.config.txt");
    fs::write(&victim, b"tampered\n").unwrap();
    let dest = dir.join("dest");
    assert!(
        build_cache::reuse_tree_to(&key, &dest).unwrap().is_none(),
        "a corrupted tree entry must be a miss, never a hit"
    );
    assert!(
        !build_cache::cache_root().join(&key).is_dir(),
        "the corrupt entry is removed, not left to poison later launches"
    );
    assert!(!dest.exists(), "nothing is installed from a corrupt entry");
}

#[test]
fn unlisted_extra_file_is_a_corrupt_entry() {
    let (_env, dir) = isolate("tree-extra");
    let key = build_cache::digest(&[b"w39-test/tree-extra"]);
    let src = dir.join("src");
    write_tree(&src, "v1");
    let _lock = build_cache::lock(&key).unwrap();
    build_cache::publish_tree_locked(&key, &src, &|_| false).unwrap();

    // An extra file that no manifest line covers means the entry is a different
    // build (e.g. a previous sysroot's per-APEX namespace): refuse it.
    let stale = build_cache::cache_root().join(&key).join("com.android.media");
    fs::create_dir_all(&stale).unwrap();
    fs::write(stale.join("ld.config.txt"), b"stale\n").unwrap();
    assert!(build_cache::reuse_tree_to(&key, &dir.join("dest")).unwrap().is_none());
}

#[test]
fn publish_tree_skips_stamps_and_logs() {
    let (_env, dir) = isolate("tree-skip");
    let key = build_cache::digest(&[b"w39-test/tree-skip"]);
    let src = dir.join("src");
    write_tree(&src, "v1");
    fs::write(src.join(".mounts"), b"stamp\n").unwrap();
    fs::write(src.join("linkerconfig.log"), b"log\n").unwrap();
    fs::write(src.join("PixelConfigOverlayCommon.apk"), b"apk\n").unwrap();
    let _lock = build_cache::lock(&key).unwrap();
    build_cache::publish_tree_locked(&key, &src, &|rel| {
        rel == ".mounts" || rel.ends_with(".log") || rel.ends_with(".apk")
    })
    .unwrap();

    let entry = build_cache::cache_root().join(&key);
    assert!(entry.join("ld.config.txt").is_file());
    assert!(!entry.join(".mounts").exists(), "the data-dir stamp is not an artefact");
    assert!(!entry.join("linkerconfig.log").exists(), "the log is not an artefact");
    assert!(!entry.join("PixelConfigOverlayCommon.apk").exists(), "staged APKs are not artefacts");

    let dest = dir.join("dest");
    let files = build_cache::reuse_tree_to(&key, &dest).unwrap().unwrap();
    assert_eq!(files.len(), 4);
    assert!(!dest.join(".mounts").exists());
}

#[test]
fn image_digest_follows_the_extracted_image() {
    let dir = scratch("image-digest");
    let a = dir.join("sysroot-a");
    let b = dir.join("sysroot-b");
    for (s, manifest) in [
        (&a, &b"sha256  1  a/system/apex\n"[..]),
        (&b, &b"sha256  9  b/system/apex\n"[..]),
    ] {
        fs::create_dir_all(s).unwrap();
        fs::write(s.join("READY"), b"android_api=34\nabi=x86_64\n").unwrap();
        fs::write(s.join("MANIFEST.txt"), manifest).unwrap();
    }
    let first = build_cache::image_digest(&a);
    assert_eq!(first, build_cache::image_digest(&a), "same image, same digest");
    assert_ne!(first, build_cache::image_digest(&b), "different sysroot, different digest");

    // A repaired/re-extracted image changes MANIFEST.txt: the memoised digest
    // must follow the new bytes, not the path.
    std::thread::sleep(std::time::Duration::from_millis(20));
    fs::write(b.join("MANIFEST.txt"), b"sha256  2  changed/system/apex\n").unwrap();
    let third = build_cache::image_digest(&b);
    assert_ne!(build_cache::image_digest(&a), third);
    assert_ne!(third, build_cache::image_digest(&a));
}
