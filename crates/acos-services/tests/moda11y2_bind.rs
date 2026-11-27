//! MODA11Y2: the system -> app `AccessibilityService` bind on the host side.
//!
//! Every assertion here runs against the real `ServiceHost`: the bind is planned
//! by `IAccessibilityManager.addClient`, the reply is `IActivityManager.publishService`,
//! and the service gets `IAccessibilityServiceClient.init` on the binder its own
//! `onBind` returned.

use std::sync::{Arc, Mutex};

use acos_parcel::{BinderId, Parcel};
use acos_services::host::ServiceHost;
use acos_services::misc::{A11yService, A11yState};
use acos_services::module::{ModuleRegistry, ModuleSpec};
use acos_services::pms::ServiceInfo;
use acos_services::value::{Parcelable, Value};

const MANAGER: &str = "android.view.accessibility.IAccessibilityManager";
const AM: &str = "android.app.IActivityManager";
const APP_THREAD: &str = "android.app.IApplicationThread";
const PACKAGE: &str = "dev.acos.fixtures.a11yprobe";
const SERVICE: &str = "dev.acos.fixtures.a11yprobe.A11yProbeService";

fn host() -> ServiceHost {
    ServiceHost::standard()
}

fn a11y_state(host: &mut ServiceHost) -> Arc<Mutex<A11yState>> {
    host.ensure(MANAGER)
        .expect("A11yService")
        .as_any_mut()
        .and_then(|any| any.downcast_mut::<A11yService>())
        .expect("A11yService")
        .state()
}

/// Install the probe's service into the in-process package manager, which is
/// what "the APK is installed" means for the module.
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
    primary.app.package_name = PACKAGE.to_string();
    primary.package.package_name = PACKAGE.to_string();
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

fn attach(host: &mut ServiceHost) -> ModuleRegistry {
    let mut registry = ModuleRegistry::standard();
    registry
        .attach_session(
            host,
            &[ModuleSpec::parse(&format!(
                "accessibility:real,{{\"enable\":[\"{PACKAGE}/.A11yProbeService\"]}}"
            ))
            .expect("spec")],
        )
        .expect("attach");
    registry
}

fn transact(host: &mut ServiceHost, interface: &str, code: u32, args: Vec<Value>) -> Parcel {
    let method = acos_aidl::lookup(interface, code).expect("table row");
    let mut data = acos_services::codec::build_request(interface, method, &args).expect("request");
    data.set_data_position(0);
    let binder = host
        .check_service(name_for(interface))
        .unwrap()
        .expect("binder");
    let mut reply = Parcel::new();
    host.transact(binder, code, &mut data, &mut reply, 0)
        .expect("transact");
    reply
}

fn name_for(interface: &str) -> &'static str {
    match interface {
        MANAGER => "accessibility",
        AM => "activity",
        _ => panic!("no service name for {interface}"),
    }
}

#[test]
fn the_installed_list_is_read_from_the_package_manager() {
    let mut host = host();
    let registry = attach(&mut host);
    // Nothing is installed yet: the enabled list names a service the package
    // manager does not have, so the platform's answer is "no installed service".
    let details = registry
        .reports()
        .into_iter()
        .find(|r| r.id == "accessibility")
        .and_then(|r| r.details)
        .expect("details");
    assert_eq!(details["enabled"], true);
    assert_eq!(
        details["installed_services"].as_array().map(Vec::len),
        Some(0)
    );

    install_probe(&mut host);
    let state = a11y_state(&mut host);
    let installed = state.lock().unwrap().installed_services();
    assert_eq!(installed, vec![format!("{PACKAGE}/.A11yProbeService")]);

    // ... and the real transaction answers exactly that component.
    let reply = transact(&mut host, MANAGER, 5, vec![Value::Int(0)]);
    let mut reply = reply;
    reply.set_data_position(0);
    let values = acos_services::codec::parse_reply(
        MANAGER,
        acos_aidl::lookup(MANAGER, 5).expect("row"),
        &mut reply,
    )
    .expect("reply");
    match &values[0] {
        Value::List(Some(items)) => assert_eq!(items.len(), 1),
        other => panic!("expected one installed service, got {other:?}"),
    }
}

#[test]
fn add_client_creates_and_binds_the_enabled_service() {
    let mut host = host();
    let registry = attach(&mut host);
    let _ = registry;
    install_probe(&mut host);
    let app_thread = BinderId(1 << 32 | 0x77);
    a11y_state(&mut host)
        .lock()
        .unwrap()
        .set_app_thread(app_thread.0);
    // The settings observer's drain binds the enabled service as soon as the
    // app thread exists (the bind is already in flight).
    let deliveries = host.take_deliveries();

    // The app's real `AccessibilityManager` calls `addClient` on start-up: a
    // bind to the same component is already pending, so no second bind (and no
    // second token that would orphan the first service instance) is started.
    let _ = transact(
        &mut host,
        MANAGER,
        3,
        vec![Value::Binder(Some(BinderId(1 << 32 | 0x21))), Value::Int(0)],
    );
    assert!(
        host.take_deliveries().is_empty(),
        "addClient must not start a second bind while one is pending"
    );
    assert_eq!(
        deliveries.len(),
        2,
        "a bind is `scheduleCreateService` then `scheduleBindService`"
    );
    let create = &deliveries[0];
    assert_eq!(create.interface, APP_THREAD);
    assert_eq!(create.method, "scheduleCreateService");
    assert_eq!(create.target, app_thread);
    let bind = &deliveries[1];
    assert_eq!(bind.method, "scheduleBindService");
    assert_eq!(bind.target, app_thread);

    // The bind intent is the framework's own `Intent.writeToParcel` shape with
    // the accessibility-service action and the enabled component.
    let mut data = bind.data.clone();
    data.set_data_position(0);
    data.enforce_interface(APP_THREAD).expect("token");
    let token = data.read_strong_binder().unwrap().expect("token binder");
    assert_eq!(data.read_int32().unwrap(), 1, "writeTypedObject presence");
    let intent = acos_services::parcelable::read_intent(&mut data).expect("intent");
    let fields = intent.fields().expect("intent fields");
    assert_eq!(
        fields[acos_services::parcelable::INTENT_ACTION].as_str(),
        Some("android.accessibilityservice.AccessibilityService")
    );
    match &fields[acos_services::parcelable::INTENT_COMPONENT] {
        Value::Parcelable(Parcelable::Ordered { values, .. }) => {
            assert_eq!(values[0].as_str(), Some(PACKAGE));
            assert_eq!(values[1].as_str(), Some(SERVICE));
        }
        other => panic!("expected a ComponentName, got {other:?}"),
    }
    assert!(!data.read_bool().unwrap(), "rebind = false runs onBind");
    assert_eq!(token.0, token_for(&mut host));
}

fn token_for(host: &mut ServiceHost) -> u64 {
    let state = a11y_state(host);
    let state = state.lock().unwrap();
    state.pending_bind.as_ref().expect("pending bind").token
}

#[test]
fn publish_service_answers_init_with_a_connection_node() {
    let mut host = host();
    let registry = attach(&mut host);
    let _ = registry;
    install_probe(&mut host);
    a11y_state(&mut host)
        .lock()
        .unwrap()
        .set_app_thread(BinderId(1 << 32 | 0x77).0);
    let _ = transact(
        &mut host,
        MANAGER,
        3,
        vec![Value::Binder(Some(BinderId(1 << 32 | 0x21))), Value::Int(0)],
    );
    let _ = host.take_deliveries();
    let token = token_for(&mut host);
    let package = a11y_state(&mut host)
        .lock()
        .unwrap()
        .pending_bind
        .clone()
        .expect("pending bind");
    let intent = acos_services::a11y_bind::bind_intent(&package.package, &package.class);
    let service = BinderId(1 << 32 | 0x31);

    // The app reports its `onBind` binder the way `ActivityThread` does.
    let _ = transact(
        &mut host,
        AM,
        40,
        vec![
            Value::Binder(Some(BinderId(token))),
            Value::Parcelable(intent),
            Value::Binder(Some(service)),
        ],
    );
    let deliveries = host.take_deliveries();
    assert_eq!(deliveries.len(), 1, "one init delivery");
    let init = &deliveries[0];
    assert_eq!(
        init.interface,
        "android.accessibilityservice.IAccessibilityServiceClient"
    );
    assert_eq!(init.method, "init");
    assert_eq!(init.target, service);

    // `init(IAccessibilityServiceConnection, int connectionId, IBinder
    // windowToken)`.
    let mut data = init.data.clone();
    data.set_data_position(0);
    data.enforce_interface(init.interface.as_str())
        .expect("token");
    let connection = data.read_strong_binder().unwrap().expect("connection");
    assert_eq!(data.read_int32().unwrap(), 1, "connection id");
    let window_token = data.read_strong_binder().unwrap().expect("window token");
    assert_ne!(window_token.0, 0);

    let bound = a11y_state(&mut host)
        .lock()
        .unwrap()
        .bound_service
        .clone()
        .expect("service is bound");
    assert_eq!(bound.component, format!("{PACKAGE}/.A11yProbeService"));
    assert_eq!(bound.binder, service.0);
    assert_eq!(bound.connection, connection.0);

    // The connection node answers `getServiceInfo` (code 11) with the info the
    // bind was made with, through the real raw-transaction path.
    let mut request = Parcel::new();
    request
        .write_interface_token("android.accessibilityservice.IAccessibilityServiceConnection")
        .unwrap();
    request.set_data_position(0);
    let mut reply = Parcel::new();
    host.transact(connection, 11, &mut request, &mut reply, 0)
        .expect("getServiceInfo");
    reply.set_data_position(0);
    assert_eq!(reply.read_int32().unwrap(), 0, "no exception");
    assert_eq!(reply.read_int32().unwrap(), 1, "writeTypedObject presence");
    let info = acos_services::parcelable::read_payload(
        "android.accessibilityservice.AccessibilityServiceInfo",
        &mut reply,
    )
    .expect("service info payload");
    let fields = info.fields().expect("fields");
    assert_eq!(
        fields[8]
            .as_parcelable()
            .and_then(|c| c.fields())
            .and_then(|f| f.first().cloned())
            .and_then(|v| v.as_str().map(str::to_string)),
        Some(PACKAGE.to_string()),
        "the connection's ServiceInfo names the bound component"
    );
}

#[test]
fn a_foreign_publish_service_is_refused() {
    let mut host = host();
    let _registry = attach(&mut host);
    install_probe(&mut host);
    a11y_state(&mut host)
        .lock()
        .unwrap()
        .set_app_thread(BinderId(1 << 32 | 0x77).0);
    let _ = transact(
        &mut host,
        MANAGER,
        3,
        vec![Value::Binder(Some(BinderId(1 << 32 | 0x21))), Value::Int(0)],
    );
    let _ = host.take_deliveries();
    let package = a11y_state(&mut host)
        .lock()
        .unwrap()
        .pending_bind
        .clone()
        .expect("pending bind");
    let intent = acos_services::a11y_bind::bind_intent(&package.package, &package.class);
    let method = acos_aidl::lookup(AM, 40).expect("row");
    let mut data = acos_services::codec::build_request(
        AM,
        method,
        &[
            Value::Binder(Some(BinderId(0xdead))),
            Value::Parcelable(intent),
            Value::Binder(Some(BinderId(1 << 32 | 0x31))),
        ],
    )
    .expect("request");
    data.set_data_position(0);
    let binder = host.check_service("activity").unwrap().expect("binder");
    let mut reply = Parcel::new();
    let err = host
        .transact(binder, 40, &mut data, &mut reply, 0)
        .expect_err("a token that is not the pending bind must be refused");
    assert!(format!("{err}").contains("does not match the pending bind"));
    assert!(host.take_deliveries().is_empty());
}
