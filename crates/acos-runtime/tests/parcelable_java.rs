//! W112-A real-Java parcel differential harness.
//!
//! `tests/java/ParcelableCompareMain.java` runs on the real Android 14
//! framework: it constructs `Configuration` (with a `LocaleList` and a
//! non-default `WindowConfiguration`), `PersistableBundle`, calls the real
//! `writeToParcel` into an ACOS-native `Parcel`, and prints the marshalled
//! bytes. This test encodes the *same* logical value with
//! `acos-services::parcelable`, asserts the two byte streams are identical, and
//! then feeds the Rust bytes back to the real `CREATOR.createFromParcel`,
//! checking the reconstructed fields.
//!
//! The Java case skips loudly when the `javac`/`d8`/`android.jar` toolchain is
//! absent; it never silently passes.

mod common;

use std::collections::HashMap;

use common::{compile_case, run_case_args};
use acos_services::parcelable;
use acos_services::value::{Parcelable, Value};
use acos_services::Bundle;

fn rect(left: i32, top: i32, right: i32, bottom: i32) -> Value {
    Value::Parcelable(Parcelable::ordered(
        "android.graphics.Rect",
        vec![
            Value::Int(left),
            Value::Int(top),
            Value::Int(right),
            Value::Int(bottom),
        ],
    ))
}

/// The logical value the Java `configuration()` case builds, in
/// `writeToParcel` field order.
fn configuration_value() -> Parcelable {
    let window = Parcelable::ordered(
        "android.app.WindowConfiguration",
        vec![
            rect(0, 0, 1080, 2400),                       // mBounds (Nested)
            Value::TypedObject(Parcelable::ordered(
                "android.graphics.Rect",
                vec![
                    Value::Int(0),
                    Value::Int(0),
                    Value::Int(1080),
                    Value::Int(2337),
                ],
            )), // mAppBounds
            rect(0, 0, 1080, 2400),                       // mMaxBounds (Nested)
            Value::Int(1), // mWindowingMode
            Value::Int(1), // mActivityType
            Value::Int(0), // mAlwaysOnTop
            Value::Int(0), // mRotation
            Value::Int(1), // mDisplayWindowingMode
            Value::Int(0), // mDisplayRotation
        ],
    );
    let locale = Value::TypedObject(Parcelable::ordered(
        "android.os.LocaleList",
        vec![Value::Str8(Some("en-US".into()))],
    ));
    Parcelable::ordered(
        "android.content.res.Configuration",
        vec![
            Value::Float(1.25),
            Value::Int(310), // mcc
            Value::Int(260), // mnc
            locale,
            Value::Int(1), // userSetLocale (setLocales sets it true)
            Value::Int(3), // touchscreen
            Value::Int(2), // keyboard
            Value::Int(1), // keyboardHidden
            Value::Int(1), // hardKeyboardHidden
            Value::Int(1), // navigation
            Value::Int(2), // navigationHidden
            Value::Int(2), // orientation
            Value::Int(0x20), // screenLayout
            Value::Int(0), // colorMode
            Value::Int(0x11), // uiMode
            Value::Int(411), // screenWidthDp
            Value::Int(914), // screenHeightDp
            Value::Int(411), // smallestScreenWidthDp
            Value::Int(420), // densityDpi
            Value::Int(411), // compatScreenWidthDp
            Value::Int(914), // compatScreenHeightDp
            Value::Int(411), // compatSmallestScreenWidthDp
            Value::Parcelable(window),
            Value::Int(7),   // assetsSeq
            Value::Int(3),   // seq
            Value::Int(100), // fontWeightAdjustment
            Value::Int(3),   // mGrammaticalGender
        ],
    )
}

fn persistable_bundle_value() -> Parcelable {
    let mut b = Bundle::new();
    b.put("name", Value::Str(Some("helloviews".into())));
    Parcelable::ordered(
        "android.os.PersistableBundle",
        vec![Value::Bundle(Some(b))],
    )
}

/// A `ServiceInfo` in the exact API-34 `writeToParcel` order, with the nested
/// `ComponentInfo.applicationInfo` payload from the hello-views `AppInfo`.
fn service_info_value(app: &acos_services::pms::AppInfo) -> Parcelable {
    acos_services::pms::ServiceInfo {
        name: "dev.acos.fixtures.svcprovider.EchoService".into(),
        package_name: "dev.acos.fixtures.svcprovider".into(),
        process_name: "dev.acos.fixtures.svcprovider".into(),
        permission: "dev.acos.fixtures.ECHO".into(),
        exported: false,
        flags: 0,
        foreground_service_type: 0,
        meta_data: Vec::new(),
    }
    .to_parcelable(app)
}

/// An `InstrumentationInfo` in the exact API-34 `writeToParcel` order.
fn instrumentation_info_value() -> Parcelable {
    Parcelable::ordered(
        "android.content.pm.InstrumentationInfo",
        vec![
            // PackageItemInfo prefix
            Value::Str8(Some("dev.acos.fixtures.EchoInstrumentation".into())),
            Value::Str8(Some("dev.acos.fixtures".into())),
            Value::Int(0),    // banner
            Value::Str(None), // label
            Value::Int(0),    // icon
            Value::Int(0),    // labelRes
            Value::Bundle(None),
            Value::Int(0), // showUser
            Value::Int(0), // showBadge
            // InstrumentationInfo
            Value::Str8(Some("dev.acos.fixtures".into())), // targetPackage
            Value::Str8(None),                          // targetProcesses
            Value::Str8(Some("/data/app/base.apk".into())), // sourceDir
            Value::Str8(Some("/data/app/base.apk".into())), // publicSourceDir
            Value::StringArray(None),                   // splitNames
            Value::StringArray(None),                   // splitSourceDirs
            Value::StringArray(None),                   // splitPublicSourceDirs
            Value::List(None),                          // splitDependencies (SparseArray null)
            Value::Str8(Some("/data/user/0/dev.acos.fixtures".into())), // dataDir
            Value::Str8(Some("/data/user_de/0/dev.acos.fixtures".into())), // deviceProtectedDataDir
            Value::Str8(Some("/data/user/0/dev.acos.fixtures".into())), // credentialProtectedDataDir
            Value::Str8(None), // primaryCpuAbi
            Value::Str8(None), // secondaryCpuAbi
            Value::Str8(Some("/data/app/lib".into())), // nativeLibraryDir
            Value::Str8(None), // secondaryNativeLibraryDir
            Value::Int(0),     // handleProfiling
            Value::Int(1),     // functionalTest
        ],
    )
}

/// A `PermissionInfo` in the exact API-34 `writeToParcel` order with a null
/// `knownCerts` set.
fn permission_info_value() -> Parcelable {
    Parcelable::ordered(
        "android.content.pm.PermissionInfo",
        vec![
            // PackageItemInfo prefix
            Value::Str8(Some("android.permission.INTERNET".into())),
            Value::Str8(Some("android".into())),
            Value::Int(0),    // banner
            Value::Str(None), // label
            Value::Int(0),    // icon
            Value::Int(0),    // labelRes
            Value::Bundle(None),
            Value::Int(0), // showUser
            Value::Int(0), // showBadge
            // PermissionInfo
            Value::Int(0), // protectionLevel
            Value::Int(0), // flags
            Value::Str8(Some("android.permission-group.NETWORK".into())), // group
            Value::Str8(None), // backgroundPermission
            Value::Int(0),     // descriptionRes
            Value::Int(0),     // requestRes
            Value::Str(None),  // nonLocalizedDescription
            Value::Int(0),     // knownCerts (empty ForStringSet: `unparcel` maps -1 to empty)
        ],
    )
}

/// A launcher `ResolveInfo` in the exact API-34 `writeToParcel` order: the
/// activity branch and a null `IntentFilter`.
fn resolve_info_value(app: &acos_services::pms::AppInfo) -> Parcelable {
    let act = acos_services::pms::ActivityInfo {
        name: "dev.acos.fixtures.helloviews.MainActivity".into(),
        package_name: app.package_name.clone(),
        launch_mode: 0,
        flags: 0,
        theme: 0,
        exported: true,
        parent_activity_name: String::new(),
        meta_data: Vec::new(),
    };
    Parcelable::ordered(
        "android.content.pm.ResolveInfo",
        vec![
            Value::Cond {
                present: true,
                values: vec![Value::Parcelable(act.to_parcelable(app))],
            },
            Value::Cond {
                present: false,
                values: Vec::new(),
            },
            Value::Int(0),        // priority
            Value::Int(0),        // preferredOrder
            Value::Int(0x10_0000), // match
            Value::Int(-1),       // specificIndex
            Value::Int(0),        // labelRes
            Value::Str(None),     // nonLocalizedLabel
            Value::Int(0),        // icon
            Value::Str8(None),    // resolvePackageName
            Value::Int(0),        // targetUserId
            Value::Int(0),        // system
            Value::Int(0),        // noResourceId
            Value::Int(0),        // iconResourceId
            Value::Int(0),        // handleAllWebDataURI
            Value::Int(0),        // mAutoResolutionAllowed
            Value::Int(0),        // isInstantAppAvailable
            Value::Int(-2),       // userHandle == USER_CURRENT
        ],
    )
}


/// REL3P: the `PackageInfo` shape a `getPackageInfo(pkg, 0x100024c0)` reply
/// carries — non-empty `splitNames` / `requestedPermissions` (both
/// `writeString8Array` in the image's `PackageInfo.writeToParcel`) and a
/// one-element `Signature[]`. `applicationInfo` is null so the value is a pure
/// function of these fields and `ParcelableCompareMain.packageInfoArrays()`
/// builds the identical object for a byte-for-byte comparison.
fn package_info_arrays_value() -> Parcelable {
    Parcelable::ordered(
        "android.content.pm.PackageInfo",
        vec![
            Value::Str8(Some("dev.acos.fixtures.pmsprobe".into())),
            Value::StringArray(Some(vec![Some("config.xxhdpi".into())])), // splitNames (String8[])
            Value::Int(7),   // versionCode
            Value::Int(0),   // versionCodeMajor
            Value::Str8(Some("1.0".into())), // versionName
            Value::Int(0),   // baseRevisionCode
            Value::IntArray(None), // splitRevisionCodes
            Value::Str8(None), // sharedUserId
            Value::Int(0),   // sharedUserLabel
            Value::Cond { present: false, values: vec![] }, // applicationInfo
            Value::Long(1),  // firstInstallTime
            Value::Long(2),  // lastUpdateTime
            Value::IntArray(None), // gids
            Value::List(None), // activities
            Value::List(None), // receivers
            Value::List(None), // services
            Value::List(None), // providers
            Value::List(None), // instrumentation
            Value::List(None), // permissions
            Value::StringArray(Some(vec![
                Some("android.permission.INTERNET".into()),
                Some("android.permission.VIBRATE".into()),
            ])), // requestedPermissions (String8[])
            Value::IntArray(Some(vec![1, 1])), // requestedPermissionsFlags
            Value::List(Some(vec![Value::Parcelable(Parcelable::ordered(
                "android.content.pm.Signature",
                vec![Value::ByteArray(Some(vec![
                    0x30, 0x82, 0x01, 0x0a, 0x0d, 0x0e,
                ]))],
            ))])), // signatures
            Value::List(None), // configPreferences
            Value::List(None), // reqFeatures
            Value::List(None), // featureGroups
            Value::List(None), // attributions
            Value::Int(0),   // installLocation
            Value::Int(0),   // isStub
            Value::Int(0),   // coreApp
            Value::Int(0),   // requiredForAllUsers
            Value::Str8(None), // restrictedAccountType
            Value::Str8(None), // requiredAccountType
            Value::Str8(None), // overlayTarget
            Value::Str8(None), // overlayCategory
            Value::Int(0),   // overlayPriority
            Value::Bool(false), // mOverlayIsStatic
            Value::Int(0),   // compileSdkVersion
            Value::Str8(None), // compileSdkVersionCodename
            Value::Cond { present: false, values: vec![] }, // signingInfo
            Value::Bool(false), // isApex
            Value::Bool(false), // isActiveApex
        ],
    )
}

fn encoded(value: &Parcelable) -> Vec<u8> {
    let mut parcel = acos_parcel::Parcel::new();
    parcelable::write_payload(&mut parcel, value).expect("encode payload");
    parcel.marshall()
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        write!(out, "{b:02x}").unwrap();
    }
    out
}

fn parse_writes(stdout: &str) -> HashMap<String, String> {
    stdout
        .lines()
        .filter_map(|l| {
            let rest = l.strip_prefix("write.")?;
            let (name, hex) = rest.split_once('=')?;
            Some((name.to_string(), hex.to_string()))
        })
        .collect()
}

#[test]
fn real_java_write_to_parcel_matches_the_rust_codec() {
    let Some(dex) = compile_case(&["ParcelableCompareMain"]) else {
        eprintln!("SKIP real_java_write_to_parcel_matches_the_rust_codec: toolchain unavailable");
        return;
    };

    // Forward: the real framework's bytes vs. the Rust encoder's bytes.
    let (_rt, outcome) = run_case_args(&dex, "ParcelableCompareMain", &[]);
    assert!(
        outcome.vm_error.is_none() && outcome.exit_code == 0,
        "ParcelableCompareMain failed: vm_error={:?}\nstdout:\n{}\nstderr:\n{}",
        outcome.vm_error,
        outcome.stdout,
        outcome.stderr
    );
    let writes = parse_writes(&outcome.stdout);
    let config = configuration_value();
    let config_bytes = encoded(&config);
    assert_eq!(
        writes.get("configuration").map(String::as_str),
        Some(hex(&config_bytes).as_str()),
        "Configuration real-Java bytes differ from the Rust codec\njava={:?}\nrust={}",
        writes.get("configuration"),
        hex(&config_bytes)
    );
    let pkg_arrays = package_info_arrays_value();
    let pkg_arrays_bytes = encoded(&pkg_arrays);
    assert_eq!(
        writes.get("package_info_arrays").map(String::as_str),
        Some(hex(&pkg_arrays_bytes).as_str()),
        "PackageInfo real-Java bytes differ from the Rust codec (String8[] fields)\njava={:?}\nrust={}",
        writes.get("package_info_arrays"),
        hex(&pkg_arrays_bytes)
    );
    // REL3P2: the two `android.net` replies ACOS serves today
    // (`getNetworkCapabilities` / `getLinkProperties`) against the real
    // framework writers. The assertions below compare the *production*
    // builders, so a change in `connectivity.rs` that stops being
    // framework-exact fails here.
    let caps = acos_services::connectivity::default_network_capabilities();
    let caps_bytes = encoded(&caps);
    assert_eq!(
        writes.get("network_capabilities").map(String::as_str),
        Some(hex(&caps_bytes).as_str()),
        "NetworkCapabilities real-Java bytes differ from the Rust codec\njava={:?}\nrust={}",
        writes.get("network_capabilities"),
        hex(&caps_bytes)
    );
    let link = acos_services::connectivity::default_link_properties();
    let link_bytes = encoded(&link);
    assert_eq!(
        writes.get("link_properties").map(String::as_str),
        Some(hex(&link_bytes).as_str()),
        "LinkProperties real-Java bytes differ from the Rust codec\njava={:?}\nrust={}",
        writes.get("link_properties"),
        hex(&link_bytes)
    );
    let bundle = persistable_bundle_value();
    let bundle_bytes = encoded(&bundle);
    assert_eq!(
        writes.get("persistable_bundle").map(String::as_str),
        Some(hex(&bundle_bytes).as_str()),
        "PersistableBundle real-Java bytes differ from the Rust codec\njava={:?}\nrust={}",
        writes.get("persistable_bundle"),
        hex(&bundle_bytes)
    );

    // Differential parcelables built from the parsed manifest/APK values.
    use acos_services::pms::{
        ActivityInfo as PmsActivityInfo, AppInfo, PackageInfo as PmsPackageInfo,
    };
    let app = AppInfo::hello_views_fallback(std::path::Path::new("hello-views.apk"));
    let app_bytes = encoded(&app.to_parcelable());
    let act = PmsActivityInfo {
        name: "dev.acos.fixtures.helloviews.MainActivity".into(),
        package_name: "dev.acos.fixtures.helloviews".into(),
        launch_mode: 0,
        flags: 0,
        theme: 0,
        exported: true,
        parent_activity_name: String::new(),
        meta_data: Vec::new(),
    };
    let act_bytes = encoded(&act.to_parcelable(&app));
    // K5: RedReader's real F-Droid signer certificate, the exact DER the host
    // PMS reply carries. If the Rust `PackageInfo`/`Signature` framing is
    // wrong, the real `PackageInfo.CREATOR.createFromParcel` rejects it here
    // (the app saw `OPENSSL_internal:DECODE_ERROR` on that reply).
    const REDREADER_CERT_HEX: &str = "3082033c30820224a00302010202045161c884300d06092a864886f70d01010505003060310b300906035504061302554b310c300a060355040813034f5247310c300a060355040713034f524731133011060355040a130a6664726f69642e6f7267310f300d060355040b13064644726f6964310f300d060355040313064644726f6964301e170d3133303430373139323730305a170d3430303832333139323730305a3060310b300906035504061302554b310c300a060355040813034f5247310c300a060355040713034f524731133011060355040a130a6664726f69642e6f7267310f300d060355040b13064644726f6964310f300d060355040313064644726f696430820122300d06092a864886f70d01010105000382010f003082010a028201010099ea49a2ce74d437f6dd23f8605d6534e32b7db7e6ea289335c67165d1c5e0dbb86e13fcad3da0135712bf01cef5ad56d307ecae86c3892dcb76f4ae340021c7869c1ec119b53a50b5a360a49a676ac3a5305f1cd0e091d9efcecd7233fb18c374f60221029d19da5f95b889adf7936dce7710beb87fd02d38a5ee31628f30718a0dc5bb97da27c590366a67f4d900b08061b2cb9bc7550765b3bc7cf3831a4dd882e7bb1303ba83b7c939f76071d37ed302646b525a98c55532734e90ec4302e7dd18965e5ce9e2d24d856a96471d48af148eea7b65e509e1b0fbad873297f5c16079d4ed21cb4cfaf7d87061c3b012e512710ef3f2ccc772b122611fee42d90203010001300d06092a864886f70d010105050003820101001998f10073528a50f6638902fed803b860489219baef5eac36accf37d428e105fc4031917c7987c861455d43bd6416feebe44e82090dea6141c992ccf4f1bf23598ff24063d417330bc7f961149a70365989ce126fa6292642e14d8a9d8dd893e91105da53be7ba86ec913c214f4e28b0abb2e465548a4ab819ae866e6ab113fb1085ad9a17d4994a35a869892e318c7e2c7bbb3d218b4bf5df9e20d8d7f0d6d0a0424395f9a065a252e77fc687466ba4d8fad9b9c71264ebde530736f890acffb0db6daa1ff54abbed7f35bd85ff0b0fdac6e32696a027ac236bfbe715f211cc22ea6cd4b356d33ae43b9d9f3fe6274604d090f9cab83119fbb4d8f88ba51e7";
    let cert = hex_bytes(REDREADER_CERT_HEX);
    let pkg = PmsPackageInfo {
        package_name: app.package_name.clone(),
        version_code: 1,
        version_name: "1.0".into(),
        application: app.clone(),
        activities: vec![act.clone()],
        requested_permissions: Vec::new(),
        first_install_time: 0,
        last_update_time: 0,
        signatures: vec![cert],
    };
    let pkg_bytes = encoded(&pkg.to_parcelable());
    // W155-A: the PackageInfo member arrays + intent resolution specs.
    let service_bytes = encoded(&service_info_value(&app));
    let instrumentation_bytes = encoded(&instrumentation_info_value());
    let permission_bytes = encoded(&permission_info_value());
    let resolve_bytes = encoded(&resolve_info_value(&app));

    // Reverse: feed the Rust bytes to the real createFromParcel, then compare
    // the real `writeToParcel` re-emission byte-for-byte.
    let args = vec![
        format!("configuration={}", hex(&config_bytes)),
        format!("persistable_bundle={}", hex(&bundle_bytes)),
        format!("application_info={}", hex(&app_bytes)),
        format!("activity_info={}", hex(&act_bytes)),
        format!("package_info={}", hex(&pkg_bytes)),
        format!("package_info_arrays={}", hex(&pkg_arrays_bytes)),
        format!("service_info={}", hex(&service_bytes)),
        format!("instrumentation_info={}", hex(&instrumentation_bytes)),
        format!("permission_info={}", hex(&permission_bytes)),
        format!("resolve_info={}", hex(&resolve_bytes)),
        format!("network_capabilities={}", hex(&caps_bytes)),
        format!("link_properties={}", hex(&link_bytes)),
    ];
    let (_rt, outcome) = run_case_args(&dex, "ParcelableCompareMain", &args);
    assert!(
        outcome.vm_error.is_none() && outcome.exit_code == 0,
        "reverse createFromParcel failed: vm_error={:?}\nstdout:\n{}\nstderr:\n{}",
        outcome.vm_error,
        outcome.stdout,
        outcome.stderr
    );
    for name in [
        "configuration",
        "persistable_bundle",
        "application_info",
        "activity_info",
        "package_info",
        "package_info_arrays",
        "service_info",
        "instrumentation_info",
        "permission_info",
        "resolve_info",
        "network_capabilities",
        "link_properties",
    ] {
        assert!(
            outcome.stdout.contains(&format!("read.{name}=ok")),
            "real {name}.createFromParcel rejected the Rust bytes:\n{}",
            outcome.stdout
        );
    }
    let roundtrips = parse_roundtrips(&outcome.stdout);
    for (name, bytes) in [
        ("configuration", &config_bytes),
        ("persistable_bundle", &bundle_bytes),
        ("application_info", &app_bytes),
        ("activity_info", &act_bytes),
        ("package_info", &pkg_bytes),
        ("package_info_arrays", &pkg_arrays_bytes),
        ("service_info", &service_bytes),
        ("instrumentation_info", &instrumentation_bytes),
        ("permission_info", &permission_bytes),
        ("resolve_info", &resolve_bytes),
        ("network_capabilities", &caps_bytes),
        ("link_properties", &link_bytes),
    ] {
        assert_eq!(
            roundtrips.get(name).map(String::as_str),
            Some(hex(bytes).as_str()),
            "{name}: real writeToParcel differs from the Rust codec\njava={:?}\nrust={}",
            roundtrips.get(name),
            hex(bytes)
        );
    }
}

fn hex_bytes(hex: &str) -> Vec<u8> {
    assert!(hex.len() % 2 == 0, "hex length");
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).expect("hex digit"))
        .collect()
}

fn parse_roundtrips(stdout: &str) -> HashMap<String, String> {
    stdout
        .lines()
        .filter_map(|l| {
            let rest = l.strip_prefix("roundtrip.")?;
            let (name, hex) = rest.split_once('=')?;
            Some((name.to_string(), hex.to_string()))
        })
        .collect()
}

/// WV2 wall 4: the `webviewupdate` peer's `waitForAndGetProvider` reply must
/// decode with the **framework's** `PackageInfo.<init>`, not with ACOS's own
/// reader.
///
/// This reply is the one parcel in the provider-load path that has no Java
/// materialiser: `WebViewUpdateService` writes the installed provider record
/// with the Rust codec (`acos_services::parcelable`), so a field-order or
/// encoding difference between the codec and the image's
/// `PackageInfo.writeToParcel` desynchronises `PackageInfo.<init>` — the image
/// reported `OutOfMemoryError: Failed to allocate a 7676737936 byte allocation
/// at Parcel.createString8Array` (`PackageInfo.<init>`), which is a garbage
/// array count read out of the wrong offset, not a memory problem.
///
/// The value is built exactly as the peer builds it (the installed provider
/// record, flags 0) and handed to the real `PackageInfo.CREATOR`, which checks
/// `packageName`, `versionCode`, the nested `ApplicationInfo` (`packageName`,
/// `sourceDir`, `uid`), `requestedPermissions` and `splitNames`, and then
/// `enforceNoDataAvail()` — so a missing or extra field anywhere in the payload
/// fails here, with or without an OOM.
#[test]
fn webview_provider_package_info_reads_back_in_the_framework_order() {
    use acos_services::pms::{AppInfo, PackageInfo as PmsPackageInfo};

    let Some(dex) = compile_case(&["ParcelableCompareMain"]) else {
        eprintln!("SKIP webview_provider_package_info_reads_back_in_the_framework_order: toolchain unavailable");
        return;
    };

    // The provider record: parsed from the real provider APK when the fixture
    // is built, otherwise the same values the install path produces (the codec
    // is what is under test, and both paths go through
    // `PackageInfo::to_parcelable_with_flags`).
    let apk = common::repo_root().join("fixtures/build/apps/webview-provider.apk");
    let (app, package) = match acos_services::pms::PmsState::from_apk(&apk) {
        Ok(state) => {
            let record = state
                .lookup(Some("com.android.webview"))
                .expect("the provider APK declares com.android.webview");
            (record.app.clone(), record.package.clone())
        }
        Err(_) => {
            eprintln!(
                "note: {} is not built; using the install path's values",
                apk.display()
            );
            let mut app =
                AppInfo::hello_views_fallback(std::path::Path::new("/data/app/com.android.webview.apk"));
            app.package_name = "com.android.webview".into();
            app.name = "com.android.webview".into();
            app.process_name = "com.android.webview".into();
            app.class_name = String::new();
            app.source_dir = "/data/app/com.android.webview.apk".into();
            app.public_source_dir = "/data/app/com.android.webview.apk".into();
            app.data_dir = "/data/data/com.android.webview".into();
            app.uid = 10193;
            let package = PmsPackageInfo {
                package_name: "com.android.webview".into(),
                version_code: 1,
                version_name: "1.0".into(),
                application: app.clone(),
                activities: Vec::new(),
                requested_permissions: vec!["android.permission.INTERNET".into()],
                first_install_time: 0,
                last_update_time: 0,
                signatures: Vec::new(),
            };
            (app, package)
        }
    };
    let _ = &app;
    let bytes = encoded(&package.to_parcelable_with_flags(0));

    let args = vec![format!("package_info_provider={}", hex(&bytes))];
    let (_rt, outcome) = run_case_args(&dex, "ParcelableCompareMain", &args);
    assert!(
        outcome.vm_error.is_none() && outcome.exit_code == 0,
        "the real framework could not read the webviewupdate reply ({} bytes): \
         vm_error={:?}\nstdout:\n{}\nstderr:\n{}",
        bytes.len(),
        outcome.vm_error,
        outcome.stdout,
        outcome.stderr
    );
    assert!(
        outcome.stdout.contains("read.package_info_provider=ok"),
        "the framework reader did not accept the reply:\n{}",
        outcome.stdout
    );
}
