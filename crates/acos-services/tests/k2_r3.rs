//! K2/R3: the read-only query replies added for the K1 corpus.
//!
//! Each reply is written with the same `WireOp` list the image's AIDL proxy
//! uses and read back with the matching parcelable spec, so a field-order
//! mistake fails here rather than in a 20-second corpus run.

use acos_services::codec::{read_args, write_reply};
use acos_services::parcelable::read_payload;
use acos_services::pms::PmsState;
use acos_services::value::{Parcelable, Value};
use acos_services::{ServiceObject, Wiring};

fn method(interface: &str, name: &str) -> &'static acos_aidl::MethodInfo {
    acos_aidl::lookup_by_name(interface, name).expect("AIDL table entry")
}

fn round_trip(interface: &str, name: &str, reply: Vec<Value>) -> Vec<Value> {
    let mut parcel = acos_parcel::Parcel::new();
    write_reply(
        &mut parcel,
        &method(interface, name).reply,
        &reply,
        interface,
        name,
    )
    .expect("reply encodes");
    parcel.set_data_position(0);
    read_args(&mut parcel, &method(interface, name).reply, interface, name).expect("reply decodes")
}

#[test]
fn memory_info_has_the_image_field_order() {
    let info = Parcelable::ordered(
        "android.app.ActivityManager$MemoryInfo",
        vec![
            Value::Long(1),
            Value::Long(2),
            Value::Long(3),
            Value::Long(4),
            Value::Int(0),
            Value::Long(5),
            Value::Long(6),
            Value::Long(7),
            Value::Long(8),
        ],
    );
    let reply = round_trip(
        "android.app.IActivityManager",
        "getMemoryInfo",
        vec![Value::Cond {
            present: true,
            values: vec![Value::Parcelable(info)],
        }],
    );
    let Value::Cond { present, values } = &reply[0] else {
        panic!("expected the out-parameter conditional");
    };
    assert!(*present);
    assert_eq!(
        values[0].as_parcelable().and_then(Parcelable::class),
        Some("android.app.ActivityManager$MemoryInfo")
    );
}

#[test]
fn install_source_info_has_the_image_field_order() {
    let info = Parcelable::ordered(
        "android.content.pm.InstallSourceInfo",
        vec![
            Value::Str(None),
            Value::NamedParcelable(Parcelable::Null),
            Value::Str(None),
            Value::Str(None),
            Value::Str8(None),
            Value::Int(0),
        ],
    );
    let mut parcel = acos_parcel::Parcel::new();
    write_reply(
        &mut parcel,
        &[acos_aidl::WireOp::TypedObject {
            class: "android.content.pm.InstallSourceInfo".into(),
            nullable: true,
        }],
        &[Value::TypedObject(info)],
        "android.content.pm.IPackageManager",
        "getInstallSourceInfo",
    )
    .expect("encodes");
    parcel.set_data_position(0);
    assert_eq!(parcel.read_int32().expect("typed presence"), 1);
    let decoded =
        read_payload("android.content.pm.InstallSourceInfo", &mut parcel).expect("decodes");
    assert_eq!(
        decoded.class(),
        Some("android.content.pm.InstallSourceInfo")
    );
}

#[test]
fn permission_controller_name_is_the_aosp_module_package() {
    let state = std::sync::Arc::new(std::sync::Mutex::new(PmsState::hello_views()));
    let mut pms = acos_services::pms::PmsService::new(state, Wiring::default());
    let reply = pms
        .call(
            "android.content.pm.IPackageManager",
            method(
                "android.content.pm.IPackageManager",
                "getPermissionControllerPackageName",
            ),
            &[],
        )
        .expect("supported")
        .reply;
    assert_eq!(reply[0].as_str(), Some("com.android.permissioncontroller"));
}
