//! W31: `ApplicationInfo.flags` derived from the parsed manifest.
//!
//! The ART guest used to hardcode `0x4 | 0x800000` (`FLAG_HAS_CODE |
//! FLAG_INSTALLED`), so `FLAG_USES_CLEARTEXT_TRAFFIC` was absent for an APK
//! whose manifest asks for cleartext (Gaten debug: RN's `http://localhost:8081`
//! probe), and `FLAG_DEBUGGABLE`/`FLAG_ALLOW_BACKUP`/`FLAG_SUPPORTS_RTL`/
//! `FLAG_EXTRACT_NATIVE_LIBS` were all missing too.
//!
//! The expectations below are transcribed from
//! `.cache/aosp/base/core/java/android/content/pm/PackageParser.java::parseBaseApplication`
//! (android-14.0.0_r75) and the API-34 image's `ApplicationInfo` bit values;
//! `FLAG_INSTALLED` is the package manager's post-install bit
//! ([`Manifest::application_flags_installed`]), not a parser flag.
//!
//! The real-APK cases run when the corpus is present (this checkout has both
//! `fixtures/build/apps/` and `.cache/gaten/`); otherwise they print a skip line
//! and the synthetic target-SDK cases still run.

use std::path::{Path, PathBuf};

use acos_apk::manifest::{
    Manifest, FLAG_ALLOW_BACKUP, FLAG_ALLOW_CLEAR_USER_DATA, FLAG_DEBUGGABLE,
    FLAG_EXTRACT_NATIVE_LIBS, FLAG_HARDWARE_ACCELERATED, FLAG_HAS_CODE, FLAG_INSTALLED,
    FLAG_IS_GAME, FLAG_LARGE_HEAP, FLAG_MULTIARCH, FLAG_RESIZEABLE_FOR_SCREENS,
    FLAG_SUPPORTS_LARGE_SCREENS, FLAG_SUPPORTS_NORMAL_SCREENS, FLAG_SUPPORTS_RTL,
    FLAG_SUPPORTS_SCREEN_DENSITIES, FLAG_SUPPORTS_SMALL_SCREENS, FLAG_SUPPORTS_XLARGE_SCREENS,
    FLAG_USES_CLEARTEXT_TRAFFIC, FLAG_VM_SAFE_MODE,
};
use acos_apk::Apk;

/// Cargo runs integration tests with the crate directory as CWD.
fn repo_path(relative: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(relative)
}

fn manifest_of(relative: &str) -> Option<Manifest> {
    let path = repo_path(relative);
    if !path.is_file() {
        eprintln!("skip: {relative} is not present in this checkout");
        return None;
    }
    let apk = Apk::open(&path).unwrap_or_else(|e| panic!("open {}: {e}", path.display()));
    let manifest = apk
        .manifest()
        .unwrap_or_else(|e| panic!("parse {}: {e}", path.display()));
    Some(manifest)
}

fn assert_bits(flags: i32, expected: &[(i32, &str)], absent: &[(i32, &str)]) {
    for (bit, name) in expected {
        assert_ne!(
            flags & bit,
            0,
            "{name} (0x{bit:08x}) must be set; flags=0x{flags:08x}"
        );
    }
    for (bit, name) in absent {
        assert_eq!(
            flags & bit,
            0,
            "{name} (0x{bit:08x}) must be clear; flags=0x{flags:08x}"
        );
    }
}

#[test]
fn counter_flags_follow_the_parser_defaults() {
    let Some(manifest) = manifest_of("fixtures/build/apps/counter.apk") else {
        return;
    };
    assert_eq!(manifest.target_sdk, Some(34));
    // counter declares debuggable=true, allowBackup=false, targetSdk 34, and
    // nothing else on <application> (aapt2 dump xmltree, build-tools 36.0.0).
    assert_eq!(manifest.application.debuggable, Some(true));
    assert_eq!(manifest.application.allow_backup, Some(false));
    // Absent attributes: hasCode/extractNativeLibs default true; supportsRtl
    // defaults false in this tag's parser; hardwareAccelerated defaults true
    // for targetSdk >= 14; usesCleartextTraffic defaults false for targetSdk
    // >= 28 (P). The absent `<supports-screens>` group defaults all six screen
    // flags on for screen SDK 34: SMALL(9) | NORMAL(10) | LARGE(11) |
    // RESIZEABLE(12) | DENSITIES(13) | XLARGE(19).
    assert_eq!(manifest.application_flags(), 0x3008_3e46);
    assert_eq!(
        manifest.application_flags_installed(),
        0x3088_3e46,
        "the installed package adds FLAG_INSTALLED (1 << 23)"
    );
    assert_bits(
        manifest.application_flags_installed(),
        &[
            (FLAG_DEBUGGABLE, "FLAG_DEBUGGABLE"),
            (FLAG_HAS_CODE, "FLAG_HAS_CODE"),
            (FLAG_ALLOW_CLEAR_USER_DATA, "FLAG_ALLOW_CLEAR_USER_DATA"),
            (FLAG_EXTRACT_NATIVE_LIBS, "FLAG_EXTRACT_NATIVE_LIBS"),
            (FLAG_HARDWARE_ACCELERATED, "FLAG_HARDWARE_ACCELERATED"),
            (
                FLAG_SUPPORTS_SMALL_SCREENS,
                "FLAG_SUPPORTS_SMALL_SCREENS (absent group)",
            ),
            (
                FLAG_SUPPORTS_NORMAL_SCREENS,
                "FLAG_SUPPORTS_NORMAL_SCREENS (absent group)",
            ),
            (
                FLAG_SUPPORTS_LARGE_SCREENS,
                "FLAG_SUPPORTS_LARGE_SCREENS (absent group)",
            ),
            (
                FLAG_RESIZEABLE_FOR_SCREENS,
                "FLAG_RESIZEABLE_FOR_SCREENS (absent group)",
            ),
            (
                FLAG_SUPPORTS_SCREEN_DENSITIES,
                "FLAG_SUPPORTS_SCREEN_DENSITIES (absent group)",
            ),
            (
                FLAG_SUPPORTS_XLARGE_SCREENS,
                "FLAG_SUPPORTS_XLARGE_SCREENS (absent group)",
            ),
            (FLAG_INSTALLED, "FLAG_INSTALLED"),
        ],
        &[
            (FLAG_ALLOW_BACKUP, "FLAG_ALLOW_BACKUP (declared false)"),
            (
                FLAG_USES_CLEARTEXT_TRAFFIC,
                "FLAG_USES_CLEARTEXT_TRAFFIC (target 34, absent)",
            ),
            (FLAG_LARGE_HEAP, "FLAG_LARGE_HEAP"),
            (FLAG_VM_SAFE_MODE, "FLAG_VM_SAFE_MODE"),
            (FLAG_IS_GAME, "FLAG_IS_GAME"),
            (FLAG_MULTIARCH, "FLAG_MULTIARCH"),
            (FLAG_SUPPORTS_RTL, "FLAG_SUPPORTS_RTL (absent)"),
        ],
    );
}

#[test]
fn gaten_debug_declares_cleartext_debuggable_rtl_and_backup() {
    let Some(manifest) = manifest_of(".cache/gaten/gaten-x86_64-debug.apk") else {
        return;
    };
    assert_eq!(manifest.target_sdk, Some(36));
    assert_eq!(manifest.application.uses_cleartext_traffic, Some(true));
    assert_eq!(manifest.application.debuggable, Some(true));
    assert_eq!(manifest.application.allow_backup, Some(true));
    assert_eq!(manifest.application.supports_rtl, Some(true));
    assert_eq!(manifest.application.extract_native_libs, Some(true));
    // Parser flags: HW(29) | EXTRACT(28) | CLEARTEXT(27) | RTL(22) |
    // BACKUP(15) | CLEAR_USER_DATA(6) | HAS_CODE(2) | DEBUGGABLE(1), plus the
    // absent `<supports-screens>` defaults SMALL(9) | NORMAL(10) | LARGE(11) |
    // RESIZEABLE(12) | DENSITIES(13) | XLARGE(19).
    assert_eq!(manifest.application_flags(), 0x3848_be46);
    assert_bits(
        manifest.application_flags_installed(),
        &[
            (FLAG_USES_CLEARTEXT_TRAFFIC, "FLAG_USES_CLEARTEXT_TRAFFIC"),
            (FLAG_DEBUGGABLE, "FLAG_DEBUGGABLE"),
            (FLAG_ALLOW_BACKUP, "FLAG_ALLOW_BACKUP"),
            (FLAG_SUPPORTS_RTL, "FLAG_SUPPORTS_RTL"),
            (FLAG_EXTRACT_NATIVE_LIBS, "FLAG_EXTRACT_NATIVE_LIBS"),
            (FLAG_HAS_CODE, "FLAG_HAS_CODE"),
            (FLAG_SUPPORTS_SMALL_SCREENS, "FLAG_SUPPORTS_SMALL_SCREENS"),
            (FLAG_SUPPORTS_NORMAL_SCREENS, "FLAG_SUPPORTS_NORMAL_SCREENS"),
            (FLAG_SUPPORTS_LARGE_SCREENS, "FLAG_SUPPORTS_LARGE_SCREENS"),
            (FLAG_RESIZEABLE_FOR_SCREENS, "FLAG_RESIZEABLE_FOR_SCREENS"),
            (
                FLAG_SUPPORTS_SCREEN_DENSITIES,
                "FLAG_SUPPORTS_SCREEN_DENSITIES",
            ),
            (FLAG_SUPPORTS_XLARGE_SCREENS, "FLAG_SUPPORTS_XLARGE_SCREENS"),
            (FLAG_INSTALLED, "FLAG_INSTALLED"),
        ],
        &[
            (FLAG_LARGE_HEAP, "FLAG_LARGE_HEAP"),
            (FLAG_VM_SAFE_MODE, "FLAG_VM_SAFE_MODE"),
        ],
    );
}

#[test]
fn gaten_release_does_not_claim_cleartext_or_debuggable() {
    let Some(manifest) = manifest_of(".cache/gaten/gaten-x86_64-release.apk") else {
        return;
    };
    assert_eq!(manifest.target_sdk, Some(36));
    assert_eq!(manifest.application.debuggable, None);
    assert_eq!(manifest.application.uses_cleartext_traffic, None);
    // targetSdk 36 keeps the cleartext default false: the release APK must not
    // be given a permission it did not ask for.
    assert_eq!(manifest.application_flags(), 0x3048_be44);
    assert_bits(
        manifest.application_flags(),
        &[
            (FLAG_ALLOW_BACKUP, "FLAG_ALLOW_BACKUP (declared true)"),
            (FLAG_SUPPORTS_RTL, "FLAG_SUPPORTS_RTL (declared true)"),
            (FLAG_EXTRACT_NATIVE_LIBS, "FLAG_EXTRACT_NATIVE_LIBS"),
            (FLAG_HAS_CODE, "FLAG_HAS_CODE"),
            (FLAG_SUPPORTS_SMALL_SCREENS, "FLAG_SUPPORTS_SMALL_SCREENS"),
            (FLAG_SUPPORTS_NORMAL_SCREENS, "FLAG_SUPPORTS_NORMAL_SCREENS"),
            (FLAG_SUPPORTS_LARGE_SCREENS, "FLAG_SUPPORTS_LARGE_SCREENS"),
            (FLAG_RESIZEABLE_FOR_SCREENS, "FLAG_RESIZEABLE_FOR_SCREENS"),
            // The Gaten runtime marker lacked this bit (0x10c08044, the
            // density-compat symptom); the parser must supply it for an
            // absent `<supports-screens>` group on a modern targetSdk.
            (
                FLAG_SUPPORTS_SCREEN_DENSITIES,
                "FLAG_SUPPORTS_SCREEN_DENSITIES",
            ),
            (FLAG_SUPPORTS_XLARGE_SCREENS, "FLAG_SUPPORTS_XLARGE_SCREENS"),
        ],
        &[
            (FLAG_DEBUGGABLE, "FLAG_DEBUGGABLE"),
            (FLAG_USES_CLEARTEXT_TRAFFIC, "FLAG_USES_CLEARTEXT_TRAFFIC"),
        ],
    );
}

#[test]
fn cleartext_default_follows_target_sdk_28() {
    let mut manifest = Manifest {
        package: "dev.acos.fixtures.target".to_string(),
        version_code: None,
        version_code_major: None,
        version_name: None,
        min_sdk: Some(26),
        target_sdk: Some(27),
        max_sdk: None,
        compile_sdk: Some(34),
        uses_permissions: Vec::new(),
        uses_features: Vec::new(),
        permissions: Vec::new(),
        application: Default::default(),
        activities: Vec::new(),
        services: Vec::new(),
        receivers: Vec::new(),
        providers: Vec::new(),
    };
    // `sa.getBoolean(usesCleartextTraffic, targetSdkVersion < P)`.
    assert_ne!(
        manifest.application_flags() & FLAG_USES_CLEARTEXT_TRAFFIC,
        0,
        "targetSdk 27 with the attribute absent defaults to cleartext allowed"
    );
    manifest.target_sdk = Some(28);
    assert_eq!(
        manifest.application_flags() & FLAG_USES_CLEARTEXT_TRAFFIC,
        0,
        "targetSdk 28 with the attribute absent defaults to cleartext denied"
    );
    manifest.application.uses_cleartext_traffic = Some(true);
    assert_ne!(
        manifest.application_flags() & FLAG_USES_CLEARTEXT_TRAFFIC,
        0,
        "an explicit usesCleartextTraffic=true wins over the targetSdk default"
    );
    manifest.application.uses_cleartext_traffic = Some(false);
    manifest.target_sdk = Some(27);
    assert_eq!(
        manifest.application_flags() & FLAG_USES_CLEARTEXT_TRAFFIC,
        0,
        "an explicit usesCleartextTraffic=false wins over the targetSdk default"
    );
}
