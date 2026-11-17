use acos_parcel::Parcel;
use acos_services::registry;
use acos_services::trace::MemoryTrace;
use acos_services::{ServiceError, ServiceHost, Value};
use proptest::prelude::*;

#[test]
fn services_are_constructed_lazily() {
    let mut host = ServiceHost::standard();
    assert_eq!(
        host.constructions(),
        0,
        "no service object built at install"
    );
    assert!(host.descriptors().len() >= 16);

    let display = host.check_service("display").unwrap().unwrap();
    assert_eq!(host.constructions(), 1);
    assert_eq!(
        host.descriptor_of(display),
        Some("android.hardware.display.IDisplayManager")
    );
    // A second lookup must not rebuild.
    let _ = host.check_service("display").unwrap().unwrap();
    assert_eq!(host.constructions(), 1);
}

#[test]
fn unknown_service_name_returns_none() {
    let mut host = ServiceHost::standard();
    assert!(host
        .check_service("definitely_not_a_service")
        .unwrap()
        .is_none());
}

/// Regression (W40-A): the PMS is seeded with `hello-views`; the launch path
/// must re-point it at the APK being launched or `getApplicationInfo` for the
/// real package raises `unknown service name <package>` during
/// `handleBindApplication`.
#[test]
fn installing_a_package_from_apk_repoints_the_package_manager() {
    let apk = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/build/apps/counter.apk");
    if !apk.is_file() {
        eprintln!(
            "SKIP installing_a_package_from_apk_repoints_the_package_manager: {}",
            apk.display()
        );
        return;
    }
    let mut host = ServiceHost::standard();
    host.install_package_from_apk(&apk)
        .expect("install counter.apk");
    let package = host.check_service("package").unwrap().unwrap();
    let reply = host
        .invoke(
            package,
            "android.content.pm.IPackageManager",
            "getApplicationInfo",
            &[
                Value::Str(Some("dev.acos.fixtures.counter".into())),
                Value::Long(0),
                Value::Int(0),
            ],
        )
        .expect("getApplicationInfo for the launched package");
    assert!(!reply.is_empty());
}

#[test]
fn unknown_transaction_is_structured_error() {
    let mut host = ServiceHost::standard();
    let activity = host.check_service("activity").unwrap().unwrap();
    // Code 4_000_000 is far outside the decoded table.
    let mut data = Parcel::new();
    data.write_interface_token("android.app.IActivityManager")
        .unwrap();
    data.write_int32(0);
    data.set_data_position(0);
    let mut reply = Parcel::new();
    let err = host
        .transact(activity, 4_000_000, &mut data, &mut reply, 0)
        .unwrap_err();
    assert_eq!(err.code(), acos_services::codes::UNSUPPORTED_ANDROID_API);
    match &err {
        ServiceError::UnsupportedAndroidApi {
            interface, code, ..
        } => {
            assert_eq!(interface, "android.app.IActivityManager");
            assert_eq!(*code, 4_000_000);
        }
        other => panic!("expected UnsupportedAndroidApi, got {other:?}"),
    }
}

#[test]
fn interface_token_mismatch_is_rejected() {
    let mut host = ServiceHost::standard();
    let activity = host.check_service("activity").unwrap().unwrap();
    let mut data = Parcel::new();
    // Wrong interface token for this binder.
    data.write_interface_token("android.content.pm.IPackageManager")
        .unwrap();
    data.set_data_position(0);
    let mut reply = Parcel::new();
    let err = host
        .transact(activity, 9, &mut data, &mut reply, 0)
        .unwrap_err();
    assert_eq!(err.code(), acos_services::codes::SERVICE_BAD_REQUEST);
}

#[test]
fn not_owned_interfaces_name_the_owner() {
    for descriptor in ["android.view.IWindowManager", "android.view.IWindow"] {
        assert_eq!(registry::owner_of(descriptor), Some("acos-wm"));
    }
    // W1: `IWindowSession` is no longer only an ownership marker — the host
    // registers a real node for it (`window_session.rs`), so `openSession` can
    // answer a binder the dispatcher routes.
    assert_eq!(registry::owner_of("android.view.IWindowSession"), None);
    // T-38: the IME is served by `acos-services::ime`; only the app-side
    // delivery targets stay with the VM.
    assert_eq!(
        registry::owner_of("com.android.internal.view.IInputMethodManager"),
        None
    );
    assert_eq!(
        registry::owner_of("com.android.internal.inputmethod.IInputMethodClient"),
        Some("acos-vm (delivery)")
    );
    assert_eq!(registry::owner_of("android.app.IActivityManager"), None);
}

#[test]
fn trace_records_calls_when_enabled() {
    let mut host = ServiceHost::standard();
    let trace = MemoryTrace::new();
    let handle = trace.clone();
    host.enable_trace(Box::new(trace));
    let activity = host.check_service("activity").unwrap().unwrap();
    let _ = host.invoke(
        activity,
        "android.app.IActivityManager",
        "checkPermission",
        &[
            Value::Str(Some("android.permission.INTERNET".into())),
            Value::Int(10192),
            Value::Int(1),
        ],
    );
    assert_eq!(handle.len(), 1);
    let call = &handle.calls()[0];
    assert_eq!(call.interface, "android.app.IActivityManager");
    assert_eq!(call.method, "checkPermission");
    assert!(call.wall_ns < 10_000_000_000);
    assert!(call.error.is_none());
}

#[test]
fn trace_off_by_default() {
    let host = ServiceHost::standard();
    assert!(format!("{host:?}").contains("trace: false"));
}

proptest! {
    /// Random bytes must never panic: the dispatcher either decodes a
    /// transaction or returns a structured error.
    #[test]
    fn random_bytes_never_panic(bytes in prop::collection::vec(any::<u8>(), 0..256)) {
        let mut host = ServiceHost::standard();
        let activity = host.check_service("activity").unwrap().unwrap();
        let mut data = Parcel::new();
        data.unmarshall(&bytes);
        let mut reply = Parcel::new();
        let _ = host.transact(activity, 9, &mut data, &mut reply, 0);
    }

    /// Random bytes *after* a valid interface token likewise never panic.
    #[test]
    fn random_args_never_panic(bytes in prop::collection::vec(any::<u8>(), 0..256)) {
        let mut host = ServiceHost::standard();
        let activity = host.check_service("activity").unwrap().unwrap();
        let mut data = Parcel::new();
        data.write_interface_token("android.app.IActivityManager").unwrap();
        data.write_blob(&bytes).unwrap();
        data.set_data_position(0);
        let mut reply = Parcel::new();
        let _ = host.transact(activity, 9, &mut data, &mut reply, 0);
    }
}
