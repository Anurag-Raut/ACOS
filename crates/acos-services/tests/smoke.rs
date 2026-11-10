use acos_parcel::BinderId;
use acos_services::value::Parcelable;
use acos_services::{ServiceHost, Value};

fn binder(host: &mut ServiceHost, name: &str) -> BinderId {
    host.check_service(name)
        .expect("check_service")
        .unwrap_or_else(|| panic!("unknown service {name}"))
}

#[test]
fn service_manager_checks_activity() {
    let mut host = ServiceHost::standard();
    let sm = host.aux("android.os.IServiceManager").expect("sm binder");
    let out = host
        .invoke(
            sm,
            "android.os.IServiceManager",
            "checkService",
            &[Value::Str(Some("activity".into()))],
        )
        .expect("checkService");
    let id = out[0].as_binder().expect("binder");
    assert_eq!(
        host.descriptor_of(id),
        Some("android.app.IActivityManager")
    );
}

#[test]
fn activity_check_permission() {
    let mut host = ServiceHost::standard();
    let activity = binder(&mut host, "activity");
    let out = host
        .invoke(
            activity,
            "android.app.IActivityManager",
            "checkPermission",
            &[
                Value::Str(Some("android.permission.INTERNET".into())),
                Value::Int(10192),
                Value::Int(4242),
            ],
        )
        .expect("checkPermission");
    assert_eq!(out[0].as_int(), Some(0));
}

#[test]
fn display_info_round_trip() {
    let mut host = ServiceHost::standard();
    let display = binder(&mut host, "display");
    let out = host
        .invoke(
            display,
            "android.hardware.display.IDisplayManager",
            "getDisplayInfo",
            &[Value::Int(0)],
        )
        .expect("getDisplayInfo");
    let parcelable = out[0].as_parcelable().expect("DisplayInfo");
    let info = acos_services::display::DisplayInfo::from_parcelable(parcelable)
        .expect("decode DisplayInfo");
    assert_eq!(info.logical_width, 1080);
    assert_eq!(info.logical_height, 2400);
    assert_eq!(info.density_dpi, 420);
    assert_eq!(info.name, "Built-in Screen");
    assert_eq!(info.refresh_rate, 60.000004);
}

#[test]
fn package_application_info_from_apk() {
    let mut host = ServiceHost::standard();
    let package = binder(&mut host, "package");
    let out = host
        .invoke(
            package,
            "android.content.pm.IPackageManager",
            "getApplicationInfo",
            &[
                Value::Str(Some("dev.acos.fixtures.helloviews".into())),
                Value::Long(0),
                Value::Int(0),
            ],
        )
        .expect("getApplicationInfo");
    let p = out[0].as_parcelable().expect("ApplicationInfo");
    let app = acos_services::pms::AppInfo::from_parcelable(p).expect("decode ApplicationInfo");
    assert_eq!(app.package_name, "dev.acos.fixtures.helloviews");
    assert!(app.source_dir.ends_with("hello-views.apk"), "sourceDir={}", app.source_dir);
    assert_eq!(app.uid, 10192);
    assert_eq!(app.target_sdk, 34);
    assert_eq!(app.data_dir, "/data/user/0/dev.acos.fixtures.helloviews");
}

#[test]
fn settings_read_and_generation() {
    let mut host = ServiceHost::standard();
    let provider = host.aux("android.content.IContentProvider").expect("provider");
    let args = |method: &str, arg: &str| {
        // `ContentProviderProxy.call(AttributionSource, authority, method, arg, extras)`.
        vec![
            Value::Parcelable(Parcelable::ordered(
                "android.content.AttributionSource",
                vec![
                    Value::Int(0),
                    Value::Int(4242),
                    Value::Int(10192),
                    Value::Str(Some("dev.acos.fixtures.helloviews".into())),
                    Value::Str(None),
                    Value::Binder(None),
                    Value::StringArray(None),
                    Value::List(None),
                ],
            )),
            Value::Str(Some("settings".into())),
            Value::Str(Some(method.into())),
            Value::Str(Some(arg.into())),
            Value::Bundle(None),
        ]
    };
    let out = host
        .invoke(
            provider,
            "android.content.IContentProvider",
            "call",
            &args("GET_system", "auto_caps"),
        )
        .expect("GET_system");
    let values = out[0].as_bundle().expect("bundle");
    assert_eq!(
        values.get("value").and_then(Value::as_str),
        Some("1")
    );
}
