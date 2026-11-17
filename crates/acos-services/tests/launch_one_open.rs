//! PERF-REL3c acceptance: a launch opens its source APK once.
//!
//! The ART launch reads one APK for its native-library install and for the
//! manifest facts (launcher, labels, budget, config changes). This test runs that
//! sequence against the built `hello-jni` fixture (x86_64 native code) and checks
//! the open counter after each step.
//!
//! One `#[test]` on purpose: the open counter and the caches are process-wide.

use std::fs;
use std::path::{Path, PathBuf};

use acos_services::apk_facts::{clear_cache, launch_facts, open_count, open_shared, prime_from};
use acos_services::manifest_resolve::{launch_budget, launcher_activity};
use acos_services::native_libs::{install_from, NativeLibInstall};
use acos_services::pms::manifest_labels;

const HELLO_JNI: &str = "fixtures/build/apps/hello-jni.apk";

fn repo(rel: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(rel)
}

/// Scratch under the cargo target dir (never /tmp).
fn scratch(name: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join("launch-one-open")
        .join(name);
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn a_launch_opens_its_source_apk_once() {
    // Required fixture: a missing file fails loudly, it is never a skip.
    let fixture = repo(HELLO_JNI);
    assert!(
        fixture.is_file(),
        "required fixture missing: {} (fixtures/build is not in git; build the fixtures)",
        fixture.display()
    );
    let dir = scratch("hello-jni");
    let apk = dir.join("app.apk");
    fs::copy(&fixture, &apk).unwrap();
    clear_cache();

    let before = open_count();

    // Step 1: the launch's one open of the source APK.
    let source = open_shared(&apk).expect("open");
    assert_eq!(open_count(), before + 1, "the open itself");

    // Step 2: native-library install reads that open, it does not open again.
    let dest = dir.join("app-lib");
    let installed = install_from(&source, &apk, &dest).expect("install");
    match &installed {
        NativeLibInstall::Installed { libs, .. } => {
            assert!(
                libs.iter().any(|l| l == "libhellojni.so"),
                "x86_64 lib: {libs:?}"
            );
        }
        other => panic!("hello-jni must install its x86_64 lib, got {other:?}"),
    }
    assert_eq!(
        open_count(),
        before + 1,
        "install_from does not re-open the APK"
    );

    // Step 3: the facts are derived from the same open and cached.
    prime_from(&apk, &source);
    let launcher = launcher_activity(&apk).expect("launcher");
    assert!(!launcher.component.is_empty());
    launch_budget(&apk).expect("budget");
    manifest_labels(&apk).expect("labels");
    launch_facts(&apk).expect("facts");
    assert_eq!(
        open_count(),
        before + 1,
        "the whole launch opened the APK once"
    );

    // Step 4: the cache keeps facts, not bytes. Once the launch drops its open,
    // facts still answer without a read...
    drop(source);
    assert_eq!(launcher_activity(&apk).unwrap(), launcher);
    assert_eq!(open_count(), before + 1, "facts outlive the open");

    // ...and a new shared open after that reads the file again.
    let again = open_shared(&apk).expect("reopen");
    assert_eq!(
        open_count(),
        before + 2,
        "no bytes were retained by the cache"
    );
    drop(again);
}
