//! T-22 acceptance: the real `IPackageManager$Stub$Proxy` reads the
//! `ApplicationInfo` / `PackageInfo` / `ActivityInfo` the ACOS PMS serves,
//! after `acos-runtime::pms_java` materialises them through the Java bridge.
//!
//! The golden file is the committed expectation derived from
//! `fixtures/reference/hello-views/art-api34-x86_64/manifest.json` (package,
//! versionCode/Name, minSdk 26, targetSdk 34) and `display.txt` (uid 10192).
//! `sourceDir`/`dataDir` come from the ACOS VFS layout.

mod common;

use common::{compile_case, repo_root, run_ok};

#[test]
fn pms_java_app_info_matches_the_reference_capture() {
    let Some(dex) = compile_case(&["PmsMain"]) else {
        eprintln!("SKIP pms_java_app_info_matches_the_reference_capture: javac/d8/android.jar unavailable");
        return;
    };
    let Some(stdout) = run_ok(&dex, "PmsMain", "PmsMain") else {
        return;
    };
    let expected = std::fs::read_to_string(
        repo_root().join("crates/acos-runtime/tests/data/pms_hello_views.golden"),
    )
    .expect("committed PMS golden file");

    let actual_lines: Vec<&str> = stdout.lines().filter(|l| !l.is_empty()).collect();
    let expected_lines: Vec<&str> = expected.lines().filter(|l| !l.is_empty()).collect();
    if actual_lines != expected_lines {
        panic!(
            "PMS golden mismatch\n--- expected ---\n{}\n--- actual ---\n{}\n",
            expected_lines.join("\n"),
            actual_lines.join("\n")
        );
    }
}
