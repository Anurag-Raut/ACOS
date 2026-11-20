//! SYSUI3B: `IPermissionManager.getPermissionInfo` / `getPermissionGroupInfo`.
//!
//! The permission dialog (`com.android.permissioncontroller`,
//! `GrantPermissionsActivity`) asks the framework for the permissions it is
//! about to show: `PackageManager.getPermissionInfo(name, 0)` ->
//! `PermissionManager` -> `IPermissionManager.getPermissionInfo` (code 3), then
//! `PermissionInfo.loadLabel(pm)`, and `getPermissionGroupInfo` for the group
//! title. Before this change the service answered
//! `UNSUPPORTED_ANDROID_API`, so the dialog fell back to the permission name's
//! last segment and the screen said `CAMERA` instead of the platform's label.
//!
//! These are full service round trips: the request is built from the image's
//! AIDL table, transacted through [`ServiceHost`], and the reply is decoded with
//! the same codec the host wrote it with, so a wrong field order in the new
//! `PermissionGroupInfo` codec fails here instead of in the guest.

use acos_parcel::Parcel;
use acos_services::host::ServiceHost;
use acos_services::module::{ModuleRegistry, ModuleSpec};
use acos_services::permission_catalog;
use acos_services::permissions::{PermissionStore, FLAG_PERMISSION_GRANTED_BY_DEFAULT};
use acos_services::value::{Parcelable, Value};
use acos_services::ServiceError;

const PERMISSION_DESCRIPTOR: &str = "android.permission.IPermissionManager";
const CAMERA: &str = "android.permission.CAMERA";
const FINE_LOCATION: &str = "android.permission.ACCESS_FINE_LOCATION";
const CAMERA_GROUP: &str = "android.permission-group.CAMERA";
const LOCATION_GROUP: &str = "android.permission-group.LOCATION";

fn method(interface: &str, name: &str) -> &'static acos_aidl::MethodInfo {
    acos_aidl::lookup_by_name(interface, name)
        .unwrap_or_else(|| panic!("{interface}::{name} is in the image's table"))
}

fn call(
    host: &mut ServiceHost,
    interface: &str,
    method_name: &str,
    args: &[Value],
) -> Result<Vec<Value>, ServiceError> {
    let binder = host
        .check_service("permission")
        .expect("the name resolves")
        .expect("system.permissions registers the name");
    let m = method(interface, method_name);
    let mut data = acos_services::codec::build_request(interface, m, args).expect("request builds");
    data.set_data_position(0);
    let mut reply = Parcel::new();
    match host.transact(binder, m.code, &mut data, &mut reply, 0) {
        Ok(()) => {
            reply.set_data_position(0);
            let exception = reply.read_int32().unwrap_or(0);
            assert_eq!(exception, 0, "{interface}::{method_name} replied ok");
            let mut values = Vec::new();
            for op in &m.reply {
                values.push(acos_services::codec::read_op(&mut reply, op).expect("reply decodes"));
            }
            Ok(values)
        }
        Err(error) => Err(error),
    }
}

fn attached() -> ServiceHost {
    let mut registry = ModuleRegistry::standard();
    let mut host = ServiceHost::new();
    registry
        .attach_session(
            &mut host,
            &[ModuleSpec::parse("system.permissions").expect("spec")],
        )
        .expect("core + system.permissions attach");
    host
}

/// The field values of a `writeTypedObject` reply.
fn fields(value: &Value) -> Option<&Vec<Value>> {
    match value {
        Value::TypedObject(Parcelable::Ordered { values, .. }) => Some(values),
        _ => None,
    }
}

fn string(value: &Value) -> Option<&str> {
    match value {
        Value::Str(Some(text)) => Some(text),
        Value::Str8(Some(text)) => Some(text),
        _ => None,
    }
}

#[test]
fn get_permission_info_answers_the_platform_label_group_and_description() {
    let mut host = attached();
    let reply = call(
        &mut host,
        PERMISSION_DESCRIPTOR,
        "getPermissionInfo",
        &[
            Value::Str(Some(CAMERA.into())),
            Value::Str(Some("com.android.permissioncontroller".into())),
            Value::Int(0),
        ],
    )
    .expect("getPermissionInfo replies");
    let values = fields(&reply[0]).expect("a non-null PermissionInfo");
    assert_eq!(values.len(), 17, "PermissionInfo.writeToParcel field count");
    assert_eq!(string(&values[0]), Some(CAMERA), "name");
    assert_eq!(string(&values[1]), Some("android"), "packageName");
    // `PackageItemInfo.nonLocalizedLabel` is the 4th field (after name,
    // packageName, banner) and is what `PermissionInfo.loadLabel` returns.
    assert_eq!(
        string(&values[3]),
        Some("take pictures and videos"),
        "nonLocalizedLabel is AOSP's permlab_camera"
    );
    assert_eq!(values[7], Value::Int(0), "showUser");
    assert_eq!(
        values[9],
        Value::Int(0x1 | 0x1000),
        "protectionLevel: dangerous|instant"
    );
    assert_eq!(string(&values[11]), Some(CAMERA_GROUP), "group");
    assert_eq!(
        string(&values[12]),
        Some("android.permission.BACKGROUND_CAMERA"),
        "backgroundPermission"
    );
    assert_eq!(
        string(&values[15]),
        Some(
            "This app can take pictures and record videos using the camera while the app is in use."
        ),
        "nonLocalizedDescription is AOSP's permdesc_camera"
    );
}

#[test]
fn an_unknown_permission_is_null_rather_than_a_guess() {
    let mut host = attached();
    let reply = call(
        &mut host,
        PERMISSION_DESCRIPTOR,
        "getPermissionInfo",
        &[
            Value::Str(Some("com.example.permission.NOT_A_PLATFORM_PERMISSION".into())),
            Value::Str(Some("com.example".into())),
            Value::Int(0),
        ],
    )
    .expect("getPermissionInfo replies");
    assert_eq!(
        reply[0],
        Value::TypedObject(Parcelable::Null),
        "the framework turns null into NameNotFoundException; the dialog's fallback is its own"
    );
}

#[test]
fn get_permission_group_info_answers_the_group_title() {
    let mut host = attached();
    let reply = call(
        &mut host,
        PERMISSION_DESCRIPTOR,
        "getPermissionGroupInfo",
        &[Value::Str(Some(LOCATION_GROUP.into())), Value::Int(0)],
    )
    .expect("getPermissionGroupInfo replies");
    let values = fields(&reply[0]).expect("a non-null PermissionGroupInfo");
    assert_eq!(values.len(), 17, "PermissionGroupInfo.writeToParcel field count");
    assert_eq!(string(&values[0]), Some(LOCATION_GROUP), "name");
    assert_eq!(string(&values[1]), Some("android"), "packageName");
    assert_eq!(string(&values[3]), Some("Location"), "nonLocalizedLabel");
    assert_eq!(
        string(&values[14]),
        Some("access this device's location"),
        "nonLocalizedDescription"
    );
    assert_eq!(values[16], Value::Int(400), "priority");

    // A group the store does not know is null, and so is `UNDEFINED`
    // (`<permission-group android:name="…UNDEFINED">` declares no label).
    for unknown in ["android.permission-group.NOT_A_GROUP", "android.permission-group.UNDEFINED"] {
        let reply = call(
            &mut host,
            PERMISSION_DESCRIPTOR,
            "getPermissionGroupInfo",
            &[Value::Str(Some(unknown.into())), Value::Int(0)],
        )
        .expect("getPermissionGroupInfo replies");
        match unknown {
            "android.permission-group.UNDEFINED" => assert!(
                fields(&reply[0]).is_some(),
                "the platform declares UNDEFINED, it just has no label"
            ),
            _ => assert_eq!(reply[0], Value::TypedObject(Parcelable::Null)),
        }
    }
}

#[test]
fn query_permissions_by_group_and_get_all_groups_answer_from_one_catalog() {
    let mut host = attached();
    let reply = call(
        &mut host,
        PERMISSION_DESCRIPTOR,
        "queryPermissionsByGroup",
        &[Value::Str(Some(LOCATION_GROUP.into())), Value::Int(0)],
    )
    .expect("queryPermissionsByGroup replies");
    let items = acos_services::parcelable::parceled_list_items(match &reply[0] {
        Value::TypedObject(p) => p,
        other => panic!("expected a ParceledListSlice, got {other:?}"),
    })
    .expect("a ParceledListSlice");
    let names: Vec<&str> = items
        .iter()
        .filter_map(|item| match item {
            Value::Parcelable(p) => match p {
                Parcelable::Ordered { values, .. } => string(&values[0]),
                _ => None,
            },
            _ => None,
        })
        .collect();
    assert_eq!(
        names,
        vec![
            "android.permission.ACCESS_BACKGROUND_LOCATION",
            "android.permission.ACCESS_COARSE_LOCATION",
            FINE_LOCATION,
        ],
        "the location group is AOSP's three platform permissions, in name order"
    );

    let reply = call(
        &mut host,
        PERMISSION_DESCRIPTOR,
        "getAllPermissionGroups",
        &[Value::Int(0)],
    )
    .expect("getAllPermissionGroups replies");
    let groups = acos_services::parcelable::parceled_list_items(match &reply[0] {
        Value::TypedObject(p) => p,
        other => panic!("expected a ParceledListSlice, got {other:?}"),
    })
    .expect("a ParceledListSlice");
    assert_eq!(
        groups.len(),
        permission_catalog::GROUPS.len(),
        "every catalog group is answered"
    );
    assert!(
        groups.len() >= 16,
        "the platform declares 16 permission groups"
    );
}

/// The store's description half is separate from the runtime state: an
/// app-declared custom permission answers from the manifest, and a platform
/// permission's *label* never changes what `checkPermission` says about it.
#[test]
fn a_declared_custom_permission_is_described_from_the_manifest() {
    let mut store = PermissionStore::for_package("dev.acos.fixtures.permprobe", &[]);
    assert_eq!(
        store.permission_description("dev.acos.fixtures.permprobe.permission.PROBE"),
        None,
        "nothing is declared yet"
    );
    store.set_declared_permissions(
        "dev.acos.fixtures.permprobe",
        &[acos_apk::manifest::DeclaredPermission {
            name: "dev.acos.fixtures.permprobe.permission.PROBE".into(),
            label: Some("probe permission".into()),
            description: Some("a declared permission".into()),
            group: Some("dev.acos.fixtures.permprobe.permission-group.PROBE".into()),
            protection_level: Some("dangerous|instant".into()),
            background_permission: None,
        }],
    );
    let info = store
        .permission_description("dev.acos.fixtures.permprobe.permission.PROBE")
        .expect("the manifest declaration answers");
    assert_eq!(info.package_name, "dev.acos.fixtures.permprobe");
    assert_eq!(info.label.as_deref(), Some("probe permission"));
    assert_eq!(
        info.description.as_deref(),
        Some("a declared permission")
    );
    assert_eq!(
        info.group,
        "dev.acos.fixtures.permprobe.permission-group.PROBE"
    );
    assert_eq!(info.protection_level, 0x1 | 0x1000);
    // The description is not the grant: the runtime table still says denied.
    assert_eq!(
        store.check("dev.acos.fixtures.permprobe.permission.PROBE"),
        acos_services::permissions::PERMISSION_DENIED
    );
    assert_eq!(
        store.flags("dev.acos.fixtures.permprobe.permission.PROBE"),
        0
    );
    // ... and an install-time grant still reports GRANTED_BY_DEFAULT.
    store.set_install_time("dev.acos.fixtures.permprobe", &["android.permission.INTERNET".into()]);
    assert_eq!(
        store.flags("android.permission.INTERNET"),
        FLAG_PERMISSION_GRANTED_BY_DEFAULT
    );
}
