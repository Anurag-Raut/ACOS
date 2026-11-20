#![allow(clippy::field_reassign_with_default)]
//! Smoke test: load the real framework/app APKs and exercise the engine.

use std::path::PathBuf;

use acos_framework_natives::res::{ApkAssets, AssetManager, AssetSource, ResConfig};

pub fn workspace_root() -> PathBuf {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.pop();
    p.pop();
    p
}

pub fn framework_res() -> Option<PathBuf> {
    // Hard-fail under ACOS_REQUIRE_FIXTURES; otherwise a loud skip.
    let rel = ".cache/boot/android-34/framework-res.apk";
    acos_test_support::have_fixture(rel).then(|| workspace_root().join(rel))
}

pub fn hello_views() -> Option<PathBuf> {
    let rel = "fixtures/build/apps/hello-views.apk";
    acos_test_support::have_fixture(rel).then(|| workspace_root().join(rel))
}

#[test]
fn framework_res_loads_and_resolves() {
    let Some(path) = acos_test_support::require_some(
        framework_res(),
        "framework_res_loads_and_resolves",
        "framework-res.apk missing",
    ) else {
        return;
    };
    let apk = ApkAssets::open(&path).expect("open framework-res");
    assert!(apk.len() > 1000);
    let table = apk.resource_table().expect("resource table");
    assert!(!table.packages.is_empty());
    let pkg = &table.packages[0];
    assert_eq!(pkg.name, "android");

    // Build the device configuration (1080x2400, 420dpi, en-US, port).
    let mut config = ResConfig::default();
    config.language = *b"en";
    config.country = *b"US";
    config.density = 420;
    config.smallest_screen_width_dp = 411;
    config.screen_width_dp = 411;
    config.screen_height_dp = 842;
    config.orientation = 1; // port
    config.ui_mode = 0x11; // normal + notnight
    config.sdk_version = 34;
    config.screen_layout = 0x12; // ldltr + normal

    // android:attr/colorBackground -> should resolve to a color value.
    let entry = table
        .resolve(apk.resource_bytes().unwrap(), 0x0101_0031, &config)
        .expect("colorBackground");
    eprintln!(
        "colorBackground type={:#04x} data={:#010x} config={} key={}",
        entry.value.data_type, entry.value.data, entry.config_label, entry.key
    );
    assert_eq!(entry.type_name, "attr");

    // A string resource resolves to a global string.
    let strings = table.all_configs(apk.resource_bytes().unwrap(), 0x0104_0000);
    assert!(!strings.is_empty(), "string/cancel should exist");
    let table2 = apk.resource_table().unwrap();
    let s = table2
        .resolve(apk.resource_bytes().unwrap(), 0x0104_0000, &config)
        .unwrap();
    if s.value.data_type == 0x03 {
        let text = table2
            .global_string(apk.resource_bytes().unwrap(), s.value.data)
            .unwrap();
        eprintln!("string/cancel = {text:?} (config {})", s.config_label);
        assert!(!text.is_empty());
    }
}

#[test]
fn manager_reads_asset() {
    let Some(path) = framework_res() else {
        return;
    };
    let apk = ApkAssets::open(&path).unwrap();
    let mut mgr = AssetManager::new();
    mgr.add_apk_assets(apk);
    let data = mgr
        .open_asset("assets/images/android-logo-mask.png", 0)
        .expect("open logo");
    let handle = mgr.asset(data).unwrap();
    assert!(handle.length() > 100);
    assert_eq!(&handle.data[..8], b"\x89PNG\r\n\x1a\n");
}

#[test]
fn app_apk_parses() {
    let Some(path) =
        acos_test_support::require_some(hello_views(), "app_apk_parses", "hello-views.apk missing")
    else {
        return;
    };
    let apk = ApkAssets::open(&path).unwrap();
    let table = apk.resource_table().unwrap();
    let pkg = &table.packages[0];
    assert_eq!(pkg.name, "dev.acos.fixtures.helloviews");
    // string/app_name = "Hello Views"
    let entry = table.resolve(
        apk.resource_bytes().unwrap(),
        0x7f03_0000,
        &ResConfig::default(),
    );
    assert!(entry.is_some());
    let text = table.global_string(apk.resource_bytes().unwrap(), entry.unwrap().value.data);
    assert_eq!(text.as_deref(), Some("Hello Views"));
}
