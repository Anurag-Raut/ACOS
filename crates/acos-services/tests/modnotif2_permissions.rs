//! MODNOTIF2: the `system.permissions` module, the one runtime-permission
//! table behind both `checkPermission` paths, and the framework rule that a name
//! a module takes over from `core` comes back when the module is detached.
//!
//! Every assertion here is a behaviour that did not exist before this change:
//!
//! * before: `registry.end_scope` deleted what a scope *added* and restored the
//!   name table, but a module that re-registered core's descriptor left the
//!   module's factory installed after detach (a detached module kept answering).
//! * before: `grantRuntimePermission` threw `UNSUPPORTED_ANDROID_API`, and
//!   `IActivityManager.checkPermission` / `IPackageManager.checkPermission`
//!   answered from two separate install-time lists.
//! * before: a `REQUEST_PERMISSIONS` intent fell through to activity
//!   resolution and answered `START_NOT_FOUND`.

use acos_parcel::{BinderId, Parcel};
use acos_services::host::ServiceHost;
use acos_services::module::{ModuleConfig, ModuleRegistry, ModuleSpec, Profile};
use acos_services::permissions::{PermissionStore, ACTION_REQUEST_PERMISSIONS};
use acos_services::value::Value;
use acos_services::ServiceError;

const PERMISSION_DESCRIPTOR: &str = "android.permission.IPermissionManager";
const CAMERA: &str = "android.permission.CAMERA";

fn method(interface: &str, name: &str) -> &'static acos_aidl::MethodInfo {
    acos_aidl::lookup_by_name(interface, name)
        .unwrap_or_else(|| panic!("{interface}::{name} is in the image's table"))
}

/// One full service round trip: build the request from the AIDL table, transact,
/// and read the reply through the same codec the host used to write it.
fn call(
    host: &mut ServiceHost,
    name: &str,
    interface: &str,
    method_name: &str,
    args: &[Value],
) -> Result<Vec<Value>, ServiceError> {
    let binder = host
        .check_service(name)
        .expect("the name resolves")
        .unwrap_or_else(|| panic!("{name} is registered"));
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

fn store_of(host: &ServiceHost) -> std::sync::Arc<std::sync::Mutex<PermissionStore>> {
    host.permission_store().expect("core publishes the table")
}

#[test]
fn a_module_that_takes_over_a_core_name_restores_core_on_detach() {
    let mut registry = ModuleRegistry::standard();
    let mut host = ServiceHost::new();
    registry
        .attach_session(
            &mut host,
            &[ModuleSpec::parse("system.permissions").expect("spec")],
        )
        .expect("core + system.permissions attach");

    // `grantRuntimePermission(packageName, permissionName, userId)`.
    let grant = [
        Value::Str(Some("com.acos.probe".into())),
        Value::Str(Some(CAMERA.into())),
        Value::Int(0),
    ];
    call(
        &mut host,
        "permission",
        PERMISSION_DESCRIPTOR,
        "grantRuntimePermission",
        &grant,
    )
    .expect("the module grants through the real table");
    assert_eq!(
        store_of(&host).lock().unwrap().check(CAMERA),
        acos_services::permissions::PERMISSION_GRANTED
    );

    // Detach: core's `IPermissionManager` answers again (it refuses
    // `grantRuntimePermission`), and that is the proof the factory was restored.
    registry
        .detach(&mut host, "system.permissions")
        .expect("detach");
    let after = call(
        &mut host,
        "permission",
        PERMISSION_DESCRIPTOR,
        "grantRuntimePermission",
        &grant,
    )
    .expect_err("core does not grant runtime permissions");
    assert!(
        matches!(after, ServiceError::UnsupportedAndroidApi { .. }),
        "detach restored core's answer, got {after:?}"
    );
    // The store is session state, not the module's: `core` still reads it, so a
    // grant survives the detach and `checkPermission` keeps answering it.
    assert_eq!(
        store_of(&host).lock().unwrap().check(CAMERA),
        acos_services::permissions::PERMISSION_GRANTED
    );
    // The alias core published for the same descriptor follows too.
    assert!(host.is_declared("permissionmgr"));
}

#[test]
fn both_check_permission_paths_read_the_one_store() {
    let mut registry = ModuleRegistry::standard();
    let mut host = ServiceHost::new();
    registry
        .attach_session(
            &mut host,
            &[ModuleSpec::parse("system.permissions").expect("spec")],
        )
        .expect("attach");

    let denied = |host: &mut ServiceHost| {
        let activity = call(
            host,
            "activity",
            "android.app.IActivityManager",
            "checkPermission",
            &[
                Value::Str(Some(CAMERA.into())),
                Value::Int(0),
                Value::Int(0),
            ],
        )
        .expect("IActivityManager.checkPermission answers");
        let package = call(
            host,
            "package",
            "android.content.pm.IPackageManager",
            "checkPermission",
            &[
                Value::Str(Some(CAMERA.into())),
                Value::Str(Some("com.acos.probe".into())),
                Value::Int(0),
            ],
        )
        .expect("IPackageManager.checkPermission answers");
        (activity, package)
    };

    assert_eq!(
        denied(&mut host),
        (vec![Value::Int(-1)], vec![Value::Int(-1)],),
        "an ungranted runtime permission is denied by both paths"
    );

    registry
        .control(
            &mut host,
            "system.permissions",
            "grant",
            &serde_json::json!({"permission": CAMERA}),
        )
        .expect("grant control op");

    assert_eq!(
        denied(&mut host),
        (vec![Value::Int(0)], vec![Value::Int(0)]),
        "one grant changes both paths at once (one table)"
    );

    // `shouldShowRequestPermissionRationale` follows the documented contract.
    let rationale = |host: &mut ServiceHost| {
        call(
            host,
            "permission",
            PERMISSION_DESCRIPTOR,
            "shouldShowRequestPermissionRationale",
            &[
                Value::Str(Some("com.acos.probe".into())),
                Value::Str(Some(CAMERA.into())),
                Value::Int(0),
            ],
        )
        .expect("rationale answers")
    };
    assert_eq!(
        rationale(&mut host),
        vec![Value::Bool(false)],
        "granted: no rationale"
    );
    registry
        .control(
            &mut host,
            "system.permissions",
            "deny",
            &serde_json::json!({"permission": CAMERA}),
        )
        .expect("deny control op");
    assert_eq!(
        rationale(&mut host),
        vec![Value::Bool(true)],
        "denied once: show rationale"
    );
    registry
        .control(
            &mut host,
            "system.permissions",
            "deny",
            &serde_json::json!({"permission": CAMERA, "never_ask_again": true}),
        )
        .expect("deny never-ask-again");
    assert_eq!(
        rationale(&mut host),
        vec![Value::Bool(false)],
        "don't ask again: no rationale"
    );
}

/// The framework's own `requestPermissions` intent, addressed to the permission
/// controller package exactly as `PackageManager.buildRequestPermissionsIntent`
/// writes it (`PackageManager.java:7049`).
fn request_permissions_intent(permissions: &[&str]) -> acos_services::value::Parcelable {
    let mut facts = acos_services::ams::IntentFacts::action(ACTION_REQUEST_PERMISSIONS);
    facts.package = Some("com.android.permissioncontroller".into());
    facts.extras.insert(
        acos_services::permissions::EXTRA_REQUEST_PERMISSIONS_NAMES.to_string(),
        acos_services::ams::Extra::Strings(permissions.iter().map(|p| (*p).to_string()).collect()),
    );
    facts.to_parcelable()
}

/// `IActivityTaskManager.startActivity`'s argument vector, in the image's order
/// (`ams::start_activity_args`), with the Intent in the decoded position.
fn start_activity_args(intent: acos_services::value::Parcelable, request_code: i32) -> Vec<Value> {
    let interface = "android.app.IActivityTaskManager";
    let m = method(interface, "startActivity");
    // Build the real request bytes, then decode them exactly as the host does:
    // that pins this test to the image's argument order instead of guessing it.
    let mut parcel = Parcel::new();
    parcel
        .write_interface_token(interface)
        .expect("interface token");
    parcel.write_strong_binder_opt(Some(BinderId(0x4000)));
    parcel
        .write_string16("com.acos.probe")
        .expect("callingPackage");
    parcel.write_string16("feature").expect("callingFeatureId");
    parcel.write_int32(1); // writeTypedObject presence for the Intent
    acos_services::parcelable::write_intent(&mut parcel, &intent).expect("Intent writes");
    parcel.write_string16_opt(None).expect("resolvedType");
    parcel.write_strong_binder_opt(None);
    parcel.write_string16("who").expect("resultWho");
    parcel.write_int32(request_code);
    parcel.write_int32(0); // startFlags
    parcel.write_int32(0); // no ProfilerInfo
    parcel.write_int32(0); // no bOptions
    parcel.set_data_position(0);
    for _ in 0..3 {
        parcel.read_int32().expect("token prefix word");
    }
    parcel.read_string16().expect("token");
    acos_services::codec::read_args(&mut parcel, &m.args, interface, "startActivity")
        .expect("the start argument list decodes")
}

#[test]
fn request_permissions_resolves_to_a_structured_permission_ui_unavailable() {
    let mut registry = ModuleRegistry::standard();
    let mut host = ServiceHost::new();
    registry
        .attach_session(
            &mut host,
            &[ModuleSpec::parse("system.permissions").expect("spec")],
        )
        .expect("attach");

    let interface = "android.app.IActivityTaskManager";
    let m = method(interface, "startActivity");
    let binder = host
        .check_service("activity_task")
        .expect("resolves")
        .unwrap();
    let args = start_activity_args(request_permissions_intent(&[CAMERA]), 7);
    let mut data = acos_services::codec::build_request(interface, m, &args).expect("request");
    data.set_data_position(0);
    let mut reply = Parcel::new();
    let error = host
        .transact(binder, m.code, &mut data, &mut reply, 0)
        .expect_err("a request with no controller app is refused");
    let ServiceError::PermissionUiUnavailable {
        permissions,
        state,
        needs,
        ..
    } = &error
    else {
        panic!("expected PERMISSION_UI_UNAVAILABLE, got {error:?}");
    };
    assert_eq!(permissions, &[CAMERA.to_string()]);
    assert_eq!(state, "denied", "not pre-granted, so nothing to auto-grant");
    assert!(
        needs.contains("com.android.permissioncontroller"),
        "the error names the system app: {needs}"
    );
    assert_eq!(error.code(), "PERMISSION_UI_UNAVAILABLE");
    // Never a silent grant, and the refusal is evidence in the store.
    let store = store_of(&host);
    let store = store.lock().unwrap();
    assert_eq!(
        store.check(CAMERA),
        acos_services::permissions::PERMISSION_DENIED
    );
    assert_eq!(store.refused.len(), 1);
    drop(store);

    // Pre-granted: the configured state's answer is `granted` (what
    // `GrantPermissionsActivity` returns without drawing anything), still not a
    // grant made by the request path.
    registry
        .control(
            &mut host,
            "system.permissions",
            "grant",
            &serde_json::json!({"permission": CAMERA}),
        )
        .expect("grant");
    let args = start_activity_args(request_permissions_intent(&[CAMERA]), 8);
    let mut data = acos_services::codec::build_request(interface, m, &args).expect("request");
    data.set_data_position(0);
    let mut reply = Parcel::new();
    let error = host
        .transact(binder, m.code, &mut data, &mut reply, 0)
        .expect_err("the controller app still does not exist");
    match error {
        ServiceError::PermissionUiUnavailable { state, .. } => {
            assert_eq!(state, "granted", "pre-granted state is reported as granted");
        }
        other => panic!("expected PERMISSION_UI_UNAVAILABLE, got {other:?}"),
    }
}

#[test]
fn the_launch_config_pre_grants_and_pre_denies() {
    let mut registry = ModuleRegistry::standard();
    let mut host = ServiceHost::new();
    let spec = ModuleSpec {
        id: "system.permissions".to_string(),
        profile: Profile::Real,
        config: ModuleConfig::from_json(
            "system.permissions",
            &serde_json::json!({
                "grant": [CAMERA],
                "deny_never_ask_again": ["android.permission.POST_NOTIFICATIONS"],
            }),
        )
        .expect("config"),
    };
    registry
        .attach_session(&mut host, &[spec])
        .expect("attach with launch config");
    let store = store_of(&host);
    let store = store.lock().unwrap();
    assert_eq!(
        store.check(CAMERA),
        acos_services::permissions::PERMISSION_GRANTED
    );
    assert_eq!(
        store.check("android.permission.POST_NOTIFICATIONS"),
        acos_services::permissions::PERMISSION_DENIED
    );
    assert!(!store.should_show_rationale("android.permission.POST_NOTIFICATIONS"));
}
