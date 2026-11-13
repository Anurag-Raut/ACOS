//! T-22 acceptance: the real `IPackageManager$Stub$Proxy` reads the
//! `ApplicationInfo` / `PackageInfo` / `ActivityInfo` the ACOS PMS serves,
//! after `acos-runtime::pms_java` materialises them through the Java bridge.
//!
//! The golden file is the committed expectation derived from
//! `fixtures/reference/hello-views/art-api34-x86_64/manifest.json` (package,
//! versionCode/Name, minSdk 26, targetSdk 34) and `display.txt` (uid 10192).
//! `sourceDir`/`dataDir` come from the ACOS VFS layout.
//!
//! `app.flags` is the AOSP-parser truth for this APK, not a measured capture
//! (the `art-api34-x86_64` set has no `dumpsys package` dump). The current value
//! `805846598` = `0x3008_3E46` is:
//!
//! * `0x4` `FLAG_HAS_CODE` — declared/omitted `android:hasCode` defaults true
//!   (`PackageParser.java`, `AndroidManifestApplication_hasCode`);
//! * `0x2` `FLAG_DEBUGGABLE` — the fixture manifest sets
//!   `android:debuggable="true"` (`fixtures/apps/hello-views/AndroidManifest.xml`;
//!   `PackageParser.java` line 3533);
//! * `0x40` `FLAG_ALLOW_CLEAR_USER_DATA` — `android:allowClearUserData`
//!   defaults true (`PackageParser.java` line 3562);
//! * `0x1000_0000` `FLAG_EXTRACT_NATIVE_LIBS` + `0x2000_0000`
//!   `FLAG_HARDWARE_ACCELERATED` — the parser derives the latter from
//!   `targetSdkVersion >= 14` when `android:hardwareAccelerated` is absent;
//! * `0x8_3E00` screen flags — the manifest has no `<supports-screens>`, so
//!   `PackageParser.parseBaseApkCommon`'s "unknown" sentinel defaults
//!   smallScreens/normalScreens/largeScreens/resizeable/anyDensity and
//!   xlargeScreens on for the effective screen SDK (`targetSdkVersion` 34):
//!   `FLAG_SUPPORTS_SMALL_SCREENS` (9) | `FLAG_SUPPORTS_NORMAL_SCREENS` (10) |
//!   `FLAG_SUPPORTS_LARGE_SCREENS` (11) | `FLAG_RESIZEABLE_FOR_SCREENS` (12) |
//!   `FLAG_SUPPORTS_SCREEN_DENSITIES` (13) | `FLAG_SUPPORTS_XLARGE_SCREENS` (19).
//!
//! The previous golden value (`805306372` = `0x3000_0004`) missed the
//! debuggable/allowClearUserData bits that the freshly rebuilt fixture manifest
//! legitimately produces. See `results/perf/PMS_FLAGS_W151A.md`.

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
