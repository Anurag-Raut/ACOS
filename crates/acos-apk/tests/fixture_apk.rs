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
