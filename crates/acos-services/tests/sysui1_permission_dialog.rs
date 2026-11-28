//! SYSUI1: the permission dialog is the guest system app (ADR-0029 amendment).
//!
//! These tests pin the host half of the path — the parts a real ART run cannot
//! isolate:
//!
//! * the framework's request intent resolves to the installed controller and is
//!   recorded with its resolved **component** (`ActivityThread` reads
//!   `r.intent.getComponent()`),
//! * the AIDL `resultWho` the framework passes for `requestPermissions` reaches
//!   the `ResultInfo` the host builds (`Activity.dispatchActivityResult` routes
//!   `onRequestPermissionsResult` only when it is `@android:requestPermissions:`),
//! * `EXTRA_REQUEST_PERMISSIONS_RESULTS` survives the wire as a `VAL_INTARRAY`,
//! * the dialog's result Intent updates the one runtime-permission table,
//! * no controller / dialogs off keeps MODNOTIF2's structured refusal.

use acos_services::ams::{AmsState, Extra, IntentFacts};
use acos_services::atms::AtmsState;
use acos_services::error::ServiceError;
use acos_services::permissions::{
    PermissionStore, ACTION_REQUEST_PERMISSIONS, EXTRA_REQUEST_PERMISSIONS_NAMES,
    EXTRA_REQUEST_PERMISSIONS_NEVER_ASK_AGAIN, EXTRA_REQUEST_PERMISSIONS_RESULTS,
    PERMISSION_CONTROLLER_PACKAGE, PERMISSION_DENIED, PERMISSION_GRANTED,
};
use std::sync::{Arc, Mutex};

const CONTROLLER_ACTIVITY: &str =
    "com.android.permissioncontroller.permission.ui.GrantPermissionsActivity";
const CAMERA: &str = "android.permission.CAMERA";
const LOCATION: &str = "android.permission.ACCESS_FINE_LOCATION";

fn request_intent(permissions: &[&str]) -> IntentFacts {
    let mut facts = IntentFacts::action(ACTION_REQUEST_PERMISSIONS);
    facts.package = Some(PERMISSION_CONTROLLER_PACKAGE.to_string());
    facts.extras.insert(
        EXTRA_REQUEST_PERMISSIONS_NAMES.to_string(),
        Extra::Strings(permissions.iter().map(|p| (*p).to_string()).collect()),
    );
    facts
}

/// A host with the controller package declared the way the launcher installs it.
fn state_with_controller(store: Arc<Mutex<PermissionStore>>) -> AmsState {
    let mut state = AmsState::new();
    state.runtime_permissions = Some(Arc::clone(&store));
    state.manifests.packages.push(acos_services::ams::PackageRecord {
        package: PERMISSION_CONTROLLER_PACKAGE.to_string(),
        uid: 10_193,
        activities: vec![acos_services::ams::DeclaredComponent {
            package: PERMISSION_CONTROLLER_PACKAGE.to_string(),
            class: CONTROLLER_ACTIVITY.to_string(),
            exported: true,
            launch_mode: 0,
            filters: vec![acos_services::ams::Filter {
                actions: vec![ACTION_REQUEST_PERMISSIONS.to_string()],
                categories: vec!["android.intent.category.DEFAULT".to_string()],
                ..Default::default()
            }],
        }],
        ..acos_services::ams::PackageRecord::default()
    });
    state
}

fn store(dialogs: bool) -> Arc<Mutex<PermissionStore>> {
    let mut store = PermissionStore::for_package("dev.acos.fixtures.permprobe", &[]);
    store.set_dialogs(dialogs);
    Arc::new(Mutex::new(store))
}

#[test]
fn dialogs_off_keeps_the_structured_refusal() {
    let store = store(false);
    let mut state = state_with_controller(Arc::clone(&store));
    assert!(!state.permission_dialog_available(&request_intent(&[CAMERA])));
    let request = acos_services::permissions::permission_request(&request_intent(&[CAMERA]))
        .expect("the intent is a permission request");
    let error = state.permission_ui_unavailable(&request);
    match error {
        ServiceError::PermissionUiUnavailable { state: ref s, .. } => assert_eq!(s, "denied"),
        other => panic!("expected PERMISSION_UI_UNAVAILABLE, got {other:?}"),
    }
}

#[test]
fn no_installed_controller_keeps_the_structured_refusal() {
    let store = store(true);
    let mut state = AmsState::new();
    state.runtime_permissions = Some(Arc::clone(&store));
    // No `com.android.permissioncontroller` in the manifest table.
    assert!(!state.permission_dialog_available(&request_intent(&[CAMERA])));
}

#[test]
fn the_request_launches_a_real_dialog_recording_the_resolved_component_and_result_who() {
    let store = store(true);
    let mut state = state_with_controller(Arc::clone(&store));
    let intent = request_intent(&[CAMERA, LOCATION]);
    assert!(state.permission_dialog_available(&intent));

    let request = acos_services::permissions::permission_request(&intent).expect("request");
    state.record_permission_request(&request);
    let launch = state.permission_dialog_intent(&request, &intent);
    // The controller gets the two facts it cannot query for itself.
    assert_eq!(
        launch.extras.get(acos_services::permissions::EXTRA_GRANTED),
        Some(&Extra::Strings(Vec::new()))
    );
    assert_eq!(
        launch.extras.get(acos_services::permissions::EXTRA_CALLING_PACKAGE),
        Some(&Extra::String("dev.acos.fixtures.permprobe".to_string()))
    );

    let code = state
        .start_activity_with_who(
            &launch,
            0x1000_0000,
            0x1234,
            7,
            Some("@android:requestPermissions:".to_string()),
        )
        .expect("the controller activity resolves");
    assert_eq!(code, 0);
    let pending = state
        .pending_launches
        .iter()
        .find(|launch| launch.package == PERMISSION_CONTROLLER_PACKAGE)
        .expect("a launch for the controller");
    // AOSP's `ActivityStarter` sets the component on the intent it launches.
    assert_eq!(
        pending.intent.component,
        Some((PERMISSION_CONTROLLER_PACKAGE.to_string(), CONTROLLER_ACTIVITY.to_string()))
    );
    assert_eq!(pending.result_to, 0x1234);
    assert_eq!(pending.request_code, 7);
    assert_eq!(
        pending.result_who.as_deref(),
        Some("@android:requestPermissions:")
    );
    assert_eq!(store.lock().unwrap().requests.len(), 1);
    assert!(store
        .lock()
        .unwrap()
        .entry(CAMERA)
        .is_some_and(|entry| entry.requested));
}

#[test]
fn the_grant_results_extra_is_a_val_intarray_on_the_wire() {
    let mut facts = IntentFacts::default();
    facts.extras.insert(
        EXTRA_REQUEST_PERMISSIONS_RESULTS.to_string(),
        Extra::Ints(vec![PERMISSION_GRANTED, PERMISSION_DENIED]),
    );
    let intent = acos_services::client_transaction::intent(&facts).expect("encodes");
    let extras = intent
        .fields()
        .and_then(|fields| fields.last().cloned())
        .and_then(|value| value.as_bundle().cloned());
    let extras = extras.or_else(|| {
        intent
            .fields()
            .and_then(|fields| {
                fields
                    .iter()
                    .find_map(|value| value.as_bundle().cloned())
            })
    });
    let extras = extras.expect("the intent carries a bundle");
    assert_eq!(
        extras.get(EXTRA_REQUEST_PERMISSIONS_RESULTS),
        Some(&acos_services::value::Value::IntArray(Some(vec![
            PERMISSION_GRANTED,
            PERMISSION_DENIED
        ])))
    );
}

#[test]
fn the_dialog_result_updates_the_one_table() {
    let store = store(true);
    {
        let mut store = store.lock().unwrap();
        store.record_request(&[CAMERA.to_string(), LOCATION.to_string()], "dev.acos.fixtures.permprobe");
        store.apply_dialog_result(
            &[CAMERA.to_string(), LOCATION.to_string()],
            &[PERMISSION_GRANTED, PERMISSION_DENIED],
            true,
        );
        assert_eq!(store.check(CAMERA), PERMISSION_GRANTED);
        assert_eq!(store.check(LOCATION), PERMISSION_DENIED);
        // Deny + "don't ask again" is `FLAG_PERMISSION_USER_FIXED`.
        assert!(store.never_ask_again(LOCATION));
        assert!(!store.never_ask_again(CAMERA));
        assert!(!store.should_show_rationale(LOCATION));
    }

    // The ATMS applies a finishing dialog's result Intent to the same table.
    let mut atms = AtmsState::default();
    atms.runtime_permissions = Some(Arc::clone(&store));
    atms.ensure_activity(0x7100, CONTROLLER_ACTIVITY);
    let mut result = IntentFacts::default();
    result.extras.insert(
        EXTRA_REQUEST_PERMISSIONS_NAMES.to_string(),
        Extra::Strings(vec![LOCATION.to_string()]),
    );
    result.extras.insert(
        EXTRA_REQUEST_PERMISSIONS_RESULTS.to_string(),
        Extra::Ints(vec![PERMISSION_GRANTED]),
    );
    atms.finish_activity(0x7100, -1, Some(result))
        .expect("the controller records its result");
    assert_eq!(store.lock().unwrap().check(LOCATION), PERMISSION_GRANTED);
}

#[test]
fn a_normal_result_intent_is_not_a_permission_answer() {
    let store = store(true);
    let mut atms = AtmsState::default();
    atms.runtime_permissions = Some(Arc::clone(&store));
    atms.ensure_activity(0x7200, "dev.acos.fixtures.procb.ProcBActivity");
    let mut result = IntentFacts::default();
    result
        .extras
        .insert("from".to_string(), Extra::String("b".to_string()));
    atms.finish_activity(0x7200, -1, Some(result))
        .expect("a normal activity result");
    assert_eq!(store.lock().unwrap().mutations, vec!["system_ui:dialogs".to_string()]);
}

#[test]
fn the_permissions_module_declares_the_controller_and_defaults_dialogs_on() {
    use acos_services::module::{ModuleConfig, ModuleRegistry, Profile};
    use acos_services::host::ServiceHost;
    let manifest = acos_services::modules::permissions::PermissionsModule::manifest_ref();
    manifest.validate().expect("valid manifest");
    assert_eq!(
        manifest.system_apps,
        &[PERMISSION_CONTROLLER_PACKAGE]
    );

    let mut registry = ModuleRegistry::standard();
    let mut host = ServiceHost::new();
    let spec = ModuleConfig::from_json("system.permissions", &serde_json::json!({}))
        .expect("empty config");
    registry
        .attach(&mut host, "system.permissions", Profile::Real, &spec)
        .expect("attach");
    let store = host.permission_store().expect("the table is published");
    assert!(store.lock().unwrap().dialogs_enabled());

    registry
        .detach(&mut host, "system.permissions")
        .expect("detach");
    assert!(
        !store.lock().unwrap().dialogs_enabled(),
        "detach turns the dialog off with the service layer"
    );

    let off = ModuleConfig::from_json("system.permissions", &serde_json::json!({"system_ui": "off"}))
        .expect("config");
    registry
        .attach(&mut host, "system.permissions", Profile::Real, &off)
        .expect("attach with dialogs off");
    assert!(!store.lock().unwrap().dialogs_enabled());

    let bogus =
        ModuleConfig::from_json("system.permissions", &serde_json::json!({"system_ui": "maybe"}))
            .expect("config");
    let err = registry
        .attach(&mut host, "system.permissions", Profile::Real, &bogus)
        .expect_err("an unknown system_ui is refused");
    assert!(err.to_string().contains("system_ui"));
}

#[test]
fn the_never_ask_again_result_extra_is_a_bool_extra() {
    // The dialog's own contract (documented on the constant): the flag travels as
    // a boolean extra, which the ATMS reads back as `Extra::Bool`.
    use acos_services::permissions::EXTRA_REQUEST_PERMISSIONS_NEVER_ASK_AGAIN as KEY;
    assert_eq!(KEY, "android.content.pm.extra.REQUEST_PERMISSIONS_NEVER_ASK_AGAIN");
    let mut facts = IntentFacts::default();
    facts.extras.insert(KEY.to_string(), Extra::Bool(true));
    assert_eq!(facts.extras.get(KEY), Some(&Extra::Bool(true)));
}
