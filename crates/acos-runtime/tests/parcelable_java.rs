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
    };
    let act_bytes = encoded(&act.to_parcelable(&app));
    let pkg = PmsPackageInfo {
        package_name: app.package_name.clone(),
        version_code: 1,
        version_name: "1.0".into(),
        application: app.clone(),
        activities: vec![act.clone()],
        requested_permissions: Vec::new(),
        first_install_time: 0,
        last_update_time: 0,
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
        format!("service_info={}", hex(&service_bytes)),
        format!("instrumentation_info={}", hex(&instrumentation_bytes)),
        format!("permission_info={}", hex(&permission_bytes)),
        format!("resolve_info={}", hex(&resolve_bytes)),
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
        "service_info",
        "instrumentation_info",
        "permission_info",
        "resolve_info",
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
        ("service_info", &service_bytes),
        ("instrumentation_info", &instrumentation_bytes),
        ("permission_info", &permission_bytes),
        ("resolve_info", &resolve_bytes),
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
