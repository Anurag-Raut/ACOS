//! W32 acceptance: `IPackageManager` `getComponentEnabledSetting` (85),
//! `setComponentEnabledSetting` (83) and `queryProperty` (203).
//!
//! Wire shapes, from the **image**'s generated stub (`classes*.dex`,
//! `Landroid/content/pm/IPackageManager$Stub$Proxy;` / `$Stub;`, transcribed in
//! `crates/acos-aidl/src/generated/tables.rs`):
//!
//! * code 83 `setComponentEnabledSetting(ComponentName, int newState, int flags,
//!   int userId, String callingPackage)` → void
//! * code 85 `getComponentEnabledSetting(ComponentName, int userId)` → int
//! * code 203 `queryProperty(String, int userId)` → `ParceledListSlice`
//!
//! `ComponentName.writeToParcel` is `writeString16(mPackage)`,
//! `writeString16(mClass)`; a typed object is the presence word then the body.
//! Stock `PackageManagerService` answers `COMPONENT_ENABLED_STATE_DEFAULT` (0)
//! for every component nothing has set, and `PackageApplication.getProperty`
//! turns the empty/`null` slice into an empty list.
//!
//! Negative cases are first-class: a truncated `ComponentName`, a missing
//! `userId`, an out-of-range state and a trailing bytes request must each produce
//! a structured error (never a panic, never a fabricated value).

use std::time::Instant;

use acos_parcel::{BinderId, Parcel};
use acos_services::exception::NO_EXCEPTION;
use acos_services::{ServiceError, ServiceHost};

const IPACKAGE_MANAGER: &str = "android.content.pm.IPackageManager";
const PACKAGE: &str = "dev.acos.fixtures.hello";
const ACTIVITY: &str = "dev.acos.fixtures.hello.MainActivity";

/// `COMPONENT_ENABLED_STATE_DEFAULT` / `_ENABLED` / `_DISABLED` (image
/// `android.content.PM.COMPONENT_ENABLED_STATE_*`).
const DEFAULT: i32 = 0;
const ENABLED: i32 = 1;
const DISABLED: i32 = 2;

fn pms(host: &mut ServiceHost) -> BinderId {
    host.binder_for_descriptor(IPACKAGE_MANAGER)
        .expect("IPackageManager node")
}

fn transact(
    host: &mut ServiceHost,
    binder: BinderId,
    code: u32,
    data: &mut Parcel,
) -> std::result::Result<Parcel, ServiceError> {
    let mut reply = Parcel::new();
    host.transact(binder, code, data, &mut reply, 0)?;
    reply.set_data_position(0);
    Ok(reply)
}

fn no_exception(reply: &mut Parcel) {
    assert_eq!(reply.read_int32().expect("exception header"), NO_EXCEPTION);
}

/// The image's `ComponentName` typed-object body (presence 1 + two string16s).
fn write_component(data: &mut Parcel, package: &str, class: &str) {
    data.write_int32(1);
    data.write_string16(package).expect("package");
    data.write_string16(class).expect("class");
}

fn request(build: impl FnOnce(&mut Parcel)) -> Parcel {
    let mut data = Parcel::new();
    data.write_interface_token(IPACKAGE_MANAGER).expect("token");
    build(&mut data);
    data.set_data_position(0);
    data
}

fn get_setting(host: &mut ServiceHost, package: &str, class: &str, user: i32) -> i32 {
    let binder = pms(host);
    let mut data = request(|d| {
        write_component(d, package, class);
        d.write_int32(user);
    });
    let mut reply = transact(host, binder, 85, &mut data).expect("getComponentEnabledSetting");
    no_exception(&mut reply);
    let value = reply.read_int32().expect("enabled setting");
    assert_eq!(reply.data_avail(), 0, "the reply is exactly one int");
    value
}

fn set_setting(
    host: &mut ServiceHost,
    package: &str,
    class: &str,
    state: i32,
) -> std::result::Result<(), ServiceError> {
    let binder = pms(host);
    let mut data = request(|d| {
        write_component(d, package, class);
        d.write_int32(state);
        d.write_int32(0); // PackageManager.SYNCHRONOUS
        d.write_int32(0);
        d.write_string16("dev.acos.fixtures.hello").expect("caller");
    });
    let mut reply = transact(host, binder, 83, &mut data)?;
    no_exception(&mut reply);
    assert_eq!(reply.data_avail(), 0, "a void reply has no body");
    Ok(())
}

#[test]
fn get_component_enabled_setting_answers_the_stock_default() {
    let mut host = ServiceHost::standard();
    // Nothing has been set: stock answers COMPONENT_ENABLED_STATE_DEFAULT.
    assert_eq!(get_setting(&mut host, PACKAGE, ACTIVITY, 0), DEFAULT);
    // A component the package never declared answers DEFAULT too (stock
    // `PackageManagerService` looks the setting up, not the component).
    assert_eq!(
        get_setting(&mut host, PACKAGE, "no.such.Class", 0),
        DEFAULT
    );
    // An unrelated package: DEFAULT, never an error and never another
    // component's setting.
    assert_eq!(get_setting(&mut host, "com.example.other", ACTIVITY, 10), DEFAULT);
}

#[test]
fn set_then_get_round_trips_and_is_per_component() {
    let mut host = ServiceHost::standard();
    set_setting(&mut host, PACKAGE, ACTIVITY, DISABLED).expect("set disabled");
    assert_eq!(get_setting(&mut host, PACKAGE, ACTIVITY, 0), DISABLED);
    // The same package, a different component: still default.
    assert_eq!(
        get_setting(&mut host, PACKAGE, "dev.acos.fixtures.hello.Other", 0),
        DEFAULT
    );
    // Re-setting replaces (the getter reports the newest value, not a list).
    set_setting(&mut host, PACKAGE, ACTIVITY, ENABLED).expect("set enabled");
    assert_eq!(get_setting(&mut host, PACKAGE, ACTIVITY, 0), ENABLED);
    // Back to default is a legal state and removes the value.
    set_setting(&mut host, PACKAGE, ACTIVITY, DEFAULT).expect("set default");
    assert_eq!(get_setting(&mut host, PACKAGE, ACTIVITY, 0), DEFAULT);
}

#[test]
fn an_out_of_range_state_is_rejected_and_changes_nothing() {
    let mut host = ServiceHost::standard();
    set_setting(&mut host, PACKAGE, ACTIVITY, ENABLED).expect("set enabled");
    for bad in [-1, 5, 1000, i32::MAX, i32::MIN] {
        let err = set_setting(&mut host, PACKAGE, ACTIVITY, bad).expect_err("invalid state");
        match err {
            ServiceError::BadRequest { interface, method, detail } => {
                assert_eq!(interface, IPACKAGE_MANAGER);
                assert_eq!(method, "setComponentEnabledSetting");
                assert!(
                    detail.contains("COMPONENT_ENABLED_STATE"),
                    "the error names the accepted states: {detail}"
                );
            }
            other => panic!("expected a structured BadRequest, got {other:?}"),
        }
        // The rejected call must not have overwritten the stored value.
        assert_eq!(
            get_setting(&mut host, PACKAGE, ACTIVITY, 0),
            ENABLED,
            "state {bad} must not be stored"
        );
    }
}

#[test]
fn query_property_answers_the_empty_stock_result() {
    let mut host = ServiceHost::standard();
    let binder = pms(&mut host);
    // `queryProperty(String name, int userId)`: no `<property>` element is
    // parsed from the manifest, so the list is the typed `null` slice.
    let mut data = request(|d| {
        d.write_string16("android.app.PROPERTY_SPECIAL_USE_FGS_SUBTYPE")
            .expect("name");
        d.write_int32(0);
    });
    let mut reply = transact(&mut host, binder, 203, &mut data).expect("queryProperty");
    no_exception(&mut reply);
    assert_eq!(
        reply.read_int32().expect("ParceledListSlice presence"),
        0,
        "a null typed object is the presence word 0"
    );
    assert_eq!(reply.data_avail(), 0, "nothing follows the null slice");
}

#[test]
fn hostile_or_truncated_requests_are_structured_errors() {
    let mut host = ServiceHost::standard();
    let binder = pms(&mut host);
    let started = Instant::now();

    // `ComponentName` truncated inside the class name (the image would throw
    // `Parcel` underflow).
    for cut in [1usize, 8, 20] {
        let mut full = Parcel::new();
        full.write_interface_token(IPACKAGE_MANAGER).expect("token");
        write_component(&mut full, PACKAGE, ACTIVITY);
        full.write_int32(0);
        full.set_data_position(0);
        let len = full.data_avail();
        let mut data = Parcel::new();
        data.append_from(&full, 0, len - cut).expect("prefix");
        data.set_data_position(0);
        match transact(&mut host, binder, 85, &mut data) {
            Ok(_) => panic!("a truncated request must not be answered"),
            Err(e) => assert!(
                matches!(
                    e,
                    ServiceError::BadRequest { .. } | ServiceError::UnsupportedAndroidApi { .. }
                ),
                "cut {cut}: expected a structured error, got {e:?}"
            ),
        }
    }

    // A hostile string length (`i32::MAX` chars for the class name).
    let mut data = Parcel::new();
    data.write_interface_token(IPACKAGE_MANAGER).expect("token");
    data.write_int32(1);
    data.write_string16(PACKAGE).expect("package");
    data.write_int32(i32::MAX);
    data.set_data_position(0);
    assert!(
        transact(&mut host, binder, 85, &mut data).is_err(),
        "a hostile length must be a structured error"
    );

    assert!(
        started.elapsed().as_millis() < 500,
        "hostile requests fail fast (took {:?})",
        started.elapsed()
    );
}
