//! T-22 acceptance: the real `IPackageManager$Stub$Proxy` reads the
//! `ApplicationInfo` / `PackageInfo` / `ActivityInfo` the ACOS PMS serves,
//! after `acos-runtime::pms_java` materialises them through the Java bridge.
//!
//! The golden file is the committed expectation derived from
//! `fixtures/reference/hello-views/art-api34-x86_64/manifest.json` (package,
//! versionCode/Name, minSdk 26, targetSdk 34) and `display.txt` (uid 10192).
//! `sourceDir`/`dataDir` come from the ACOS VFS layout.
//!
//! `app.flags=536870912` (0x2000_0000, `ApplicationInfo.FLAG_HARDWARE_ACCELERATED`)
//! is **not** something the reference capture recorded: the `art-api34-x86_64`
//! capture set has no `dumpsys package` / `ApplicationInfo` dump, so the field
//! has no measured source. It is instead the AOSP-parser truth for this APK,
//! verified from the real Android 14 `framework.jar` bytecode:
//! `PackageParser.parseBaseApplication` computes
//! `sa.getBoolean(AndroidManifestApplication_hardwareAccelerated,
//! targetSdkVersion >= 14)` and ORs in the flag. The fixture manifest declares
//! `targetSdkVersion=34` and no `android:hardwareAccelerated`, so a real device
//! sets the flag. The earlier golden value `0` was an ACOS-authored placeholder.
//! Full evidence and the disassembly offsets: `results/perf/PMS_FLAGS_W151A.md`.

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
