//! GATENBOOT2 step 2: an accessibility service is bound when the **enable set**
//! changes, not only when the app's `AccessibilityManager` calls `addClient`.
//!
//! AOSP's `AccessibilityManagerService` reacts to
//! `Settings.Secure.ENABLED_ACCESSIBILITY_SERVICES` / `ACCESSIBILITY_ENABLED`
//! through its `SettingsObserver` → `onUserStateChangedLocked` →
//! `setEnabledServices` → `bindServices` (and `removeServiceLocked` →
//! `unbindService` for a service that left the set). ACOS's host re-reads the
//! same table at the session's delivery drain
//! (`a11y_bind::sync_enabled_services`), which is where a settings write —
//! from a guest `ContentResolver.put` or from the module's own control op —
//! becomes visible to the system.
//!
//! Every assertion runs against the real `ServiceHost`: the settings table is
//! the one `core` installs, the bind is `IApplicationThread
//! .scheduleCreateService`/`scheduleBindService`, the reply is
//! `IActivityManager.publishService`, and the unbind is
//! `scheduleUnbindService`/`scheduleStopService` — the transactions a device
//! runs for the same transitions.

use std::sync::{Arc, Mutex};

use acos_parcel::{BinderId, Parcel};
use acos_services::host::ServiceHost;
use acos_services::misc::{A11yService, A11yState};
use acos_services::module::{ModuleRegistry, ModuleSpec};
use acos_services::pms::ServiceInfo;
use acos_services::provider::{Namespace, SettingsState};
use acos_services::value::Value;

const MANAGER: &str = "android.view.accessibility.IAccessibilityManager";
const AM: &str = "android.app.IActivityManager";
const APP_THREAD: &str = "android.app.IApplicationThread";
const PACKAGE: &str = "dev.acos.fixtures.a11yprobe";
const SERVICE: &str = "dev.acos.fixtures.a11yprobe.A11yProbeService";
const COMPONENT: &str = "dev.acos.fixtures.a11yprobe/.A11yProbeService";

fn a11y_state(host: &mut ServiceHost) -> Arc<Mutex<A11yState>> {
    host.ensure(MANAGER)
        .expect("A11yService")
        .as_any_mut()
        .and_then(|any| any.downcast_mut::<A11yService>())
        .expect("A11yService")
        .state()
}

fn settings(host: &mut ServiceHost) -> Arc<Mutex<SettingsState>> {
    a11y_state(host)
        .lock()
        .unwrap()
        .settings
        .clone()
        .expect("the session has the settings provider")
}

/// Install the probe's accessibility service into the in-process package
/// manager ("the APK is installed" for the module).
fn install_probe(host: &mut ServiceHost) {
    let state = a11y_state(host);
    let pms = state
        .lock()
        .unwrap()
        .pms
        .clone()
        .expect("package manager is wired");
    let mut pms = pms.lock().unwrap();
    let primary = pms.primary_mut();
    primary.services.push(ServiceInfo {
        name: SERVICE.to_string(),
        package_name: PACKAGE.to_string(),
        process_name: PACKAGE.to_string(),
        permission: "android.permission.BIND_ACCESSIBILITY_SERVICE".to_string(),
        exported: true,
        flags: 0,
        foreground_service_type: 0,
        meta_data: Vec::new(),
    });
}

/// A session whose accessibility module is attached but has nothing enabled —
/// the state a device is in before the user (or the agent) switches a service
/// on.
fn attached_without_enabled_services() -> (ServiceHost, ModuleRegistry) {
    let mut host = ServiceHost::standard();
    let mut registry = ModuleRegistry::standard();
    registry
        .attach_session(
            &mut host,
            &[ModuleSpec::parse("accessibility:real").expect("spec")],
        )
        .expect("attach");
    install_probe(&mut host);
    a11y_state(&mut host)
        .lock()
        .unwrap()
        .set_app_thread(BinderId(1 << 32 | 0x77).0);
    let _ = host.take_deliveries();
    (host, registry)
}

fn transact(host: &mut ServiceHost, name: &str, interface: &str, code: u32, args: Vec<Value>) -> Parcel {
    let method = acos_aidl::lookup(interface, code).expect("table row");
    let mut data = acos_services::codec::build_request(interface, method, &args).expect("request");
    data.set_data_position(0);
    let binder = host.check_service(name).unwrap().expect("binder");
    let mut reply = Parcel::new();
    host.transact(binder, code, &mut data, &mut reply, 0)
        .expect("transact");
    reply
}

/// The `Settings.Secure` write a real `ContentResolver.putString` performs
/// (`Settings.NameValueCache.putStringForUser` → `ContentProvider.call("PUT_secure")`).
fn put_secure(host: &mut ServiceHost, key: &str, value: &str) {
    settings(host)
        .lock()
        .unwrap()
        .put(Namespace::Secure, key, value);
}

fn enable_set(host: &mut ServiceHost, services: &[&str]) {
    put_secure(
        host,
        "enabled_accessibility_services",
        &services.join(":"),
    );
    put_secure(
        host,
        "accessibility_enabled",
        if services.is_empty() { "0" } else { "1" },
    );
    // AOSP's `SettingsObserver` fires for both keys; ACOS's pull reads the pair.
}

fn methods(deliveries: &[acos_services::host::PendingDelivery]) -> Vec<&str> {
    deliveries
        .iter()
        .map(|delivery| delivery.method.as_str())
        .collect()
}

/// The whole point of the change: writing the enable set binds the service,
/// which is what makes `onServiceConnected` run after the service is switched
/// on — without the app calling `addClient` again.
#[test]
fn a_settings_write_binds_the_enabled_service() {
    let (mut host, _registry) = attached_without_enabled_services();

    enable_set(&mut host, &[COMPONENT]);
    let deliveries = host.take_deliveries();
    assert_eq!(
        methods(&deliveries),
        vec!["scheduleCreateService", "scheduleBindService"],
        "the settings write binds through the framework's own two transactions"
    );
    assert!(deliveries.iter().all(|delivery| delivery.interface == APP_THREAD));
    let pending = a11y_state(&mut host)
        .lock()
        .unwrap()
        .pending_bind
        .clone()
        .expect("the bind is in flight");
    assert_eq!(pending.component, COMPONENT);

    // The app completes the handshake the way `ActivityThread` does.
    let intent = acos_services::a11y_bind::bind_intent(&pending.package, &pending.class);
    let service = BinderId(1 << 32 | 0x31);
    let _ = transact(
        &mut host,
        "activity",
        AM,
        40,
        vec![
            Value::Binder(Some(BinderId(pending.token))),
            Value::Parcelable(intent),
            Value::Binder(Some(service)),
        ],
    );
    let init = host.take_deliveries();
    assert!(
        init.iter().any(|delivery| delivery.method == "init"),
        "publishService answers init: {:?}",
        methods(&init)
    );
    assert!(a11y_state(&mut host).lock().unwrap().bound_service.is_some());
}

/// `setEnabledServices` removes a service that left the set:
/// `removeServiceLocked` → `unbindService` → `scheduleUnbindService` +
/// `scheduleStopService`, so the app's `onUnbind`/`onDestroy` run.
#[test]
fn removing_the_component_unbinds_the_bound_service() {
    let (mut host, _registry) = attached_without_enabled_services();
    enable_set(&mut host, &[COMPONENT]);
    let _ = host.take_deliveries();
    let pending = a11y_state(&mut host)
        .lock()
        .unwrap()
        .pending_bind
        .clone()
        .expect("pending bind");
    let intent = acos_services::a11y_bind::bind_intent(&pending.package, &pending.class);
    let _ = transact(
        &mut host,
        "activity",
        AM,
        40,
        vec![
            Value::Binder(Some(BinderId(pending.token))),
            Value::Parcelable(intent),
            Value::Binder(Some(BinderId(1 << 32 | 0x31))),
        ],
    );
    let _ = host.take_deliveries();
    assert!(a11y_state(&mut host).lock().unwrap().bound_service.is_some());

    // The user switches the service off: the enable set becomes empty.
    enable_set(&mut host, &[]);
    let deliveries = host.take_deliveries();
    assert_eq!(
        methods(&deliveries),
        vec!["scheduleUnbindService", "scheduleStopService"],
        "an emptied enable set unbinds and destroys the service"
    );
    assert!(deliveries.iter().all(|delivery| delivery.interface == APP_THREAD));
    let state = a11y_state(&mut host);
    let state = state.lock().unwrap();
    assert!(state.bound_service.is_none());
    assert!(state.pending_bind.is_none());
    assert!(!state.enabled && state.enabled_services.is_empty());
}

/// Replacing the enabled component unbinds the old service and binds the new
/// one in the same reconciliation, and an unchanged set does nothing at all
/// (the drain runs after every transaction, so a rebind loop would be fatal).
#[test]
fn only_a_real_change_rebinds() {
    let (mut host, _registry) = attached_without_enabled_services();
    enable_set(&mut host, &[COMPONENT]);
    let first = host.take_deliveries();
    assert_eq!(methods(&first).len(), 2);

    // Nothing changed: the next drains produce no accessibility transaction.
    assert!(
        host.take_deliveries().is_empty(),
        "an unchanged enable set never rebinds"
    );
    assert!(host.take_deliveries().is_empty());

    // A different component: unbind the first, then bind the second.
    let other = format!("{PACKAGE}/.OtherService");
    let state = a11y_state(&mut host);
    let pms = state.lock().unwrap().pms.clone().expect("pms");
    pms.lock().unwrap().primary_mut().services.push(ServiceInfo {
        name: format!("{PACKAGE}.OtherService"),
        package_name: PACKAGE.to_string(),
        process_name: PACKAGE.to_string(),
        permission: "android.permission.BIND_ACCESSIBILITY_SERVICE".to_string(),
        exported: true,
        flags: 0,
        foreground_service_type: 0,
        meta_data: Vec::new(),
    });
    // The in-flight bind is completed first, so the unbind has a service to
    // tear down (a pending bind that never completed is dropped, not unbound).
    let pending = a11y_state(&mut host)
        .lock()
        .unwrap()
        .pending_bind
        .clone()
        .expect("pending bind");
    let intent = acos_services::a11y_bind::bind_intent(&pending.package, &pending.class);
    let _ = transact(
        &mut host,
        "activity",
        AM,
        40,
        vec![
            Value::Binder(Some(BinderId(pending.token))),
            Value::Parcelable(intent),
            Value::Binder(Some(BinderId(1 << 32 | 0x31))),
        ],
    );
    let _ = host.take_deliveries();

    enable_set(&mut host, &[&other]);
    let deliveries = host.take_deliveries();
    assert_eq!(
        methods(&deliveries),
        vec![
            "scheduleUnbindService",
            "scheduleStopService",
            "scheduleCreateService",
            "scheduleBindService"
        ],
        "the service that left the set is unbound before the new one is bound"
    );
    assert_eq!(
        a11y_state(&mut host)
            .lock()
            .unwrap()
            .pending_bind
            .as_ref()
            .map(|bind| bind.component.clone()),
        Some(other)
    );
}

/// The module's own Settings-UI equivalent (`service_ctl {op: disable}`) goes
/// through the very same reconciliation, even though `enable_services` also
/// keeps the module's own record in step: the write moves the
/// `Settings.Secure` generation, which is the observer's trigger.
#[test]
fn the_modules_disable_op_unbinds_through_the_same_reconciliation() {
    let (mut host, mut registry) = attached_without_enabled_services();
    let _ = registry.control(&mut host, "accessibility", "disable", &serde_json::json!({}));
    let _ = host.take_deliveries();

    // A control-op `enable` writes the settings; the bind follows from it.
    registry
        .control(
            &mut host,
            "accessibility",
            "enable",
            &serde_json::json!({"services": [COMPONENT]}),
        )
        .expect("enable op");
    let deliveries = host.take_deliveries();
    assert_eq!(
        methods(&deliveries),
        vec!["scheduleCreateService", "scheduleBindService"]
    );
    let pending = a11y_state(&mut host)
        .lock()
        .unwrap()
        .pending_bind
        .clone()
        .expect("pending bind");
    let intent = acos_services::a11y_bind::bind_intent(&pending.package, &pending.class);
    let _ = transact(
        &mut host,
        "activity",
        AM,
        40,
        vec![
            Value::Binder(Some(BinderId(pending.token))),
            Value::Parcelable(intent),
            Value::Binder(Some(BinderId(1 << 32 | 0x31))),
        ],
    );
    let _ = host.take_deliveries();

    // ... and the control-op `disable` unbinds it.
    let _ = registry.control(&mut host, "accessibility", "disable", &serde_json::json!({}));
    let deliveries = host.take_deliveries();
    assert_eq!(
        methods(&deliveries),
        vec!["scheduleUnbindService", "scheduleStopService"]
    );
    assert!(a11y_state(&mut host).lock().unwrap().bound_service.is_none());
}

/// Multi-process regression: hot-attaching `com.android.settings` causes a
/// second process to call `attachApplication`. Enabling the primary app's
/// accessibility service from Settings must still target the primary app's
/// `IApplicationThread`, never `com.android.settings`'s.
#[test]
fn secondary_process_attach_does_not_redirect_primary_a11y_bind() {
    let (mut host, _registry) = attached_without_enabled_services();
    let primary_thread = BinderId(1 << 32 | 0x77);
    let settings_thread = BinderId(5 << 32 | 0x11);
    {
        let state = a11y_state(&mut host);
        let mut state = state.lock().unwrap();
        state.app_thread = None;
        state.set_app_thread_for_package(PACKAGE, primary_thread.0);
        state.set_app_thread_for_package("com.android.settings", settings_thread.0);
    }

    enable_set(&mut host, &[COMPONENT]);
    let deliveries = host.take_deliveries();
    assert_eq!(
        methods(&deliveries),
        vec!["scheduleCreateService", "scheduleBindService"]
    );
    assert!(
        deliveries.iter().all(|d| d.target == primary_thread),
        "bind must target primary package thread {primary_thread:?}, got {:?}",
        deliveries.iter().map(|d| d.target).collect::<Vec<_>>()
    );
}
