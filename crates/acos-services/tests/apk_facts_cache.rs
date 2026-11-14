//! PERF-REL3b acceptance: the launch-facts cache returns the same answers as a
//! fresh read, does not open the APK again on a hit, and re-reads when the file
//! changes (by length, and by modification time alone).
//!
//! One `#[test]` on purpose: the cache and its open counter are process-wide, so
//! the steps must run in order.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use acos_services::apk_facts::{clear_cache, launch_facts, open_count};
use acos_services::manifest_resolve::{launch_budget, launcher_activity};
use acos_services::pms::manifest_labels;

const COUNTER: &str = "fixtures/build/apps/counter.apk";
const COUNTER_COMPONENT: &str = "dev.acos.fixtures.counter.MainActivity";

fn repo(rel: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(rel)
}

/// Scratch under the cargo target dir (never /tmp).
fn scratch(name: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("apk-facts-cache");
    fs::create_dir_all(&dir).unwrap();
    dir.join(name)
}

#[test]
fn cache_matches_fresh_reads_and_invalidates_on_change() {
    let counter = repo(COUNTER);
    if !counter.is_file() {
        eprintln!("SKIP apk_facts_cache: fixtures missing");
        return;
    }
    clear_cache();

    // 1. Cold read, then a hit: same answers, no second open.
    let app = scratch("app.apk");
    fs::copy(&counter, &app).unwrap();
    let before = open_count();
    let cold = launch_facts(&app).expect("cold");
    assert_eq!(open_count(), before + 1, "cold read opens the APK once");
    let warm = launch_facts(&app).expect("warm");
    assert_eq!(open_count(), before + 1, "a hit does not open the APK");
    assert!(
        std::sync::Arc::ptr_eq(&cold, &warm),
        "hit returns the cached facts"
    );
    assert_eq!(cold.launcher.as_ref().unwrap().component, COUNTER_COMPONENT);

    // The public functions agree with a fresh, uncached parse of the same bytes.
    clear_cache();
    let fresh_launcher = launcher_activity(&counter).unwrap();
    let fresh_budget = launch_budget(&counter).unwrap();
    let fresh_labels = manifest_labels(&counter).unwrap();
    assert_eq!(launcher_activity(&app).unwrap(), fresh_launcher);
    assert_eq!(launch_budget(&app).unwrap(), fresh_budget);
    assert_eq!(manifest_labels(&app).unwrap(), fresh_labels);

    // 2. Changed length (different APK bytes at the same path) -> re-read.
    // dialogs-menus.apk is 16852 bytes, counter.apk 12756: the length key differs.
    let dialogs = repo("fixtures/build/apps/dialogs-menus.apk");
    let other = scratch("other.apk");
    fs::copy(&dialogs, &other).unwrap();
    clear_cache();
    let expected = launcher_activity(&other).expect("the other APK, read cold");
    assert_ne!(expected.component, COUNTER_COMPONENT, "a different APK");
    fs::copy(&dialogs, &app).unwrap();
    let before = open_count();
    let changed = launcher_activity(&app).expect("after replacing the APK");
    assert_eq!(
        open_count(),
        before + 1,
        "a new length invalidates the entry"
    );
    assert_eq!(changed, expected, "the new APK is what is read");

    // 3. Same length, only the mtime differs -> re-read.
    // Copy counter, then move its mtime an hour forward: same bytes, same length,
    // different file key, so the entry must be re-read.
    let same_len = scratch("same-len.apk");
    fs::copy(&counter, &same_len).unwrap();
    let first = launcher_activity(&same_len).unwrap();
    assert_eq!(first.component, COUNTER_COMPONENT);
    let before = open_count();
    let hit = launcher_activity(&same_len).unwrap();
    assert_eq!(open_count(), before, "unchanged file is a hit");
    assert_eq!(hit, first);
    let shifted = fs::metadata(&same_len).unwrap().modified().unwrap() + Duration::from_secs(3600);
    fs::OpenOptions::new()
        .write(true)
        .open(&same_len)
        .unwrap()
        .set_modified(shifted)
        .unwrap();
    let before = open_count();
    let after_touch = launcher_activity(&same_len).unwrap();
    assert_eq!(
        open_count(),
        before + 1,
        "a new mtime invalidates the entry"
    );
    assert_eq!(after_touch, first, "same bytes, same answer");

    // 4. A failed open is not cached: a missing path errors every time.
    let missing = scratch("missing.apk");
    let _ = fs::remove_file(&missing);
    assert!(launch_budget(&missing).is_err());
    assert!(launch_budget(&missing).is_err());
}
