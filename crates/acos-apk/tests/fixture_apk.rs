//! Parse the checked-in fixture APKs. Requires no Android SDK.

use std::path::PathBuf;

use acos_apk::{Apk, ComponentKind};

fn data(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/data")
        .join(name)
}

#[test]
fn hello_views_ingests() {
    let apk = Apk::open(data("hello-views.apk")).expect("open hello-views");
    assert_eq!(apk.dex_files.len(), 1);
    assert_eq!(apk.dex_files[0].name, "classes.dex");
    assert!(apk.dex_files[0].bytes.starts_with(b"dex\n"));

    let m = apk.manifest().expect("manifest");
    assert_eq!(m.package, "dev.acos.fixtures.helloviews");
    assert_eq!(m.version_code, Some(1));
    assert_eq!(m.version_name.as_deref(), Some("1.0"));
    assert_eq!(m.min_sdk, Some(26));
    assert_eq!(m.target_sdk, Some(34));
    assert_eq!(m.compile_sdk, Some(34));
    assert_eq!(m.application.debuggable, Some(true));

    assert_eq!(m.activities.len(), 1);
    let launcher = m.launcher_activity().expect("launcher activity");
    assert_eq!(launcher.name, "dev.acos.fixtures.helloviews.MainActivity");
    assert_eq!(launcher.exported, Some(true));
    assert!(launcher.intent_filters.iter().any(|f| f.is_launcher()));

    assert!(m
        .uses_permissions
        .iter()
        .any(|p| p.name == "android.permission.INTERNET"));

    let res = apk.resources().expect("resources").expect("arsc present");
    assert_eq!(res.packages.len(), 1);
    assert_eq!(res.packages[0].type_specs.len(), 3);
    assert_eq!(m.app_label(Some(&res)).as_deref(), Some("Hello Views"));
    assert_eq!(
        res.resolve_string(0x7f03_0000).as_deref(),
        Some("Hello Views")
    );
    assert_eq!(
        res.resolve_string(0x7f03_0004).as_deref(),
        Some("Hello ACOS")
    );
    // @string/initial_result
    assert_eq!(
        res.resolve_string(0x7f03_0003).as_deref(),
        Some("clicks: 0")
    );

    assert!(apk.signing.v1, "v1 signature expected");
    assert!(apk.signing.v2, "v2 signature expected");
    assert!(apk.signing.v3, "v3 signature expected");
    assert!(apk.signing.note.contains("not cryptographically verified"));
    assert!(apk.native_libs.is_empty());
}

#[test]
fn hello_jni_ingests() {
    let apk = Apk::open(data("hello-jni.apk")).expect("open hello-jni");
    assert_eq!(apk.dex_files.len(), 1);
    assert!(apk.dex_files[0].bytes.starts_with(b"dex\n"));

    let m = apk.manifest().expect("manifest");
    assert_eq!(m.package, "dev.acos.fixtures.hellojni");
    assert_eq!(m.version_code, Some(2));
    assert_eq!(m.version_name.as_deref(), Some("2.0"));
    assert_eq!(m.min_sdk, Some(26));
    assert_eq!(m.target_sdk, Some(34));
    assert_eq!(
        m.launcher_activity().map(|c| c.name.as_str()),
        Some("dev.acos.fixtures.hellojni.MainActivity")
    );
    assert_eq!(m.application.extract_native_libs, Some(true));

    let res = apk.resources().expect("resources").expect("arsc present");
    assert_eq!(m.app_label(Some(&res)).as_deref(), Some("Hello JNI"));

    let abis = apk.native_abis();
    assert_eq!(abis, vec!["arm64-v8a".to_string(), "x86_64".to_string()]);
    let libs = apk.native_libs.get("x86_64").expect("x86_64 libs");
    assert_eq!(libs.len(), 1);
    assert_eq!(libs[0].name, "libhellojni.so");
    assert!(libs[0].size > 0);
}

#[test]
fn missing_manifest_is_structured() {
    // A valid zip with a manifest-shaped entry list but no AndroidManifest.xml.
    let cursor = std::io::Cursor::new(Vec::new());
    let mut zip = zip::ZipWriter::new(cursor);
    zip.start_file("classes.dex", zip::write::SimpleFileOptions::default())
        .unwrap();
    use std::io::Write;
    zip.write_all(b"dex\n035\0").unwrap();
    let bytes = zip.finish().unwrap().into_inner();

    let apk = Apk::from_bytes(bytes).expect("zip should open");
    let err = apk.manifest().unwrap_err();
    assert_eq!(err.code(), "APK_NO_MANIFEST");
}

#[test]
fn non_zip_is_structured() {
    let err = Apk::from_bytes(b"not a zip at all".to_vec()).unwrap_err();
    assert_eq!(err.code(), "APK_NOT_ZIP");
    assert!(!err.message().is_empty());
}

#[test]
fn component_kinds_are_populated() {
    let apk = Apk::open(data("hello-views.apk")).unwrap();
    let m = apk.manifest().unwrap();
    let kinds: Vec<ComponentKind> = m.all_components().into_iter().map(|(k, _)| k).collect();
    assert_eq!(kinds, vec![ComponentKind::Activity]);
}

/// F6 regression on the real corpus APK: the manifest package is `com.gaten`,
/// the launcher entries are `<activity-alias>` elements (three themed icons,
/// only one enabled) that target `com.gaten.app.LauncherActivity`, and no
/// `<activity>` carries a MAIN/LAUNCHER filter. Before the alias support,
/// `acos inspect` reported `launcher_activity: null` and `intent_filters: []`
/// for every component.
///
/// The APK is not checked in (114 MB), so the test skips when it is absent —
/// asserted by the fixture-based unit tests in `src/manifest.rs`.
#[test]
fn gaten_apk_reports_its_alias_launcher() {
    use acos_apk::manifest::ComponentKind;

    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../.cache/gaten/gaten-dev-f344272-x86_64-release-unsigned.apk");
    if !path.is_file() {
        eprintln!(
            "SKIP gaten_apk_reports_its_alias_launcher: {} missing",
            path.display()
        );
        return;
    }
    let apk = Apk::open(&path).expect("open gaten");
    let manifest = apk.manifest().expect("manifest");
    assert_eq!(manifest.package, "com.gaten");

    let launcher = manifest.launcher_activity().expect("Gaten has a launcher");
    assert_eq!(launcher.kind, ComponentKind::ActivityAlias);
    // The enabled alias is the entry; the dark/system ones are disabled.
    assert_eq!(launcher.name, "com.gaten.app.LauncherIconLight");
    assert_eq!(
        launcher.target_activity.as_deref(),
        Some("com.gaten.app.LauncherActivity")
    );
    assert!(launcher.intent_filters.iter().any(|f| f.is_launcher()));

    // Every launcher alias declares its filter, enabled or not.
    for alias in [
        "com.gaten.app.LauncherIconSystem",
        "com.gaten.app.LauncherIconLight",
        "com.gaten.app.LauncherIconDark",
    ] {
        let component = manifest
            .activities
            .iter()
            .find(|c| c.name == alias)
            .unwrap_or_else(|| panic!("{alias} missing"));
        assert_eq!(component.kind, ComponentKind::ActivityAlias);
        assert!(
            component.intent_filters.iter().any(|f| f.is_launcher()),
            "{alias} lost its MAIN/LAUNCHER filter"
        );
        assert_eq!(
            component.target_activity.as_deref(),
            Some("com.gaten.app.LauncherActivity")
        );
    }
    // The declared activities are still there, with no fabricated filters.
    let main = manifest
        .activities
        .iter()
        .find(|c| c.name == "com.gaten.app.MainActivity")
        .expect("MainActivity");
    assert_eq!(main.kind, ComponentKind::Activity);
    assert!(main.intent_filters.is_empty());
    // Services/receivers keep their filters (the parser change is additive).
    assert!(manifest
        .services
        .iter()
        .any(|s| s.intent_filters.iter().any(|f| !f.actions.is_empty())));
}

/// REL3P: a real v2+v3 signed fixture APK must yield its signer certificate.
///
/// The signers sequence (one length-prefixed signer, itself a length-prefixed
/// signed data + signatures + public key) used to be parsed one nesting level
/// off, so the reader consumed the whole signer as the signed data and failed
/// with `truncated signatures`. Every APK `tools/build-apps.sh` produces was
/// affected, which made `PackageInfo.signatures` (a `GET_SIGNATURES` reply)
/// an empty array — a phone reports the signer certificate there.
#[test]
fn signed_fixture_reports_its_signer_certificate() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/hello-views.apk");
    let apk = acos_apk::Apk::open(&path).expect("open fixture APK");
    assert_eq!(
        apk.signing_certificates_error, None,
        "signing block must parse"
    );
    assert_eq!(apk.signing_certificates.len(), 1, "one signer certificate");
    let cert = &apk.signing_certificates[0];
    // A DER SEQUENCE, not a signature blob: `30 82 <len16>`.
    assert_eq!(cert.first(), Some(&0x30), "certificate is a DER SEQUENCE");
    assert_eq!(cert.get(1), Some(&0x82), "DER long-form length");
    // The certificate is the one `apksigner verify --print-certs
    // crates/acos-apk/tests/data/hello-views.apk` reports ("Signer #1
    // certificate SHA-256 digest: fc297f51...6a9f"): its DER length and the
    // issuer/validity prefix are pinned here so a wrong blob (a signature, a
    // chain member, a different scheme) fails loudly without a hash dependency.
    assert_eq!(cert.len(), 791, "certificate DER length");
    assert_eq!(
        &cert[..16],
        &[
            0x30, 0x82, 0x03, 0x13, 0x30, 0x82, 0x01, 0xfb, 0xa0, 0x03, 0x02, 0x01, 0x02, 0x02,
            0x08, 0x35,
        ],
        "certificate DER prefix (SEQUENCE, tbsCertificate, version v3, serial 0x34..)"
    );
}
