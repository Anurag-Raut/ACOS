//! `IServiceManager`'s notification surface (ADR-0029 item 1/3).
//!
//! `ServiceManager.waitForService(name)` on a device is: `getService`, and if it
//! is null, `registerForNotifications(name, callback)` + wait for
//! `IServiceCallback.onRegistration`
//! (`frameworks/native/libs/binder/IServiceManager.cpp:376-430`,
//! `frameworks/native/cmds/servicemanager/ServiceManager.cpp:527-570`).
//!
//! ACOS answered `getService`/`checkService` for a registered name but refused
//! `registerForNotifications` with a structured "unsupported", so the waiting
//! client could only fail (or, for a name nothing could ever serve, sleep
//! forever). These tests pin the three cases that matter:
//!
//! * a registered name resolves at once (the `waitForService` fast path);
//! * a name a shipped module owns but has not attached is *declared*: the
//!   callback is kept and fired by the live attach that registers it;
//! * a name nothing can ever provide is refused with the reason, so the client
//!   fails fast instead of waiting.

use acos_parcel::{BinderId, Parcel};
use acos_services::module::{ModuleRegistry, ModuleSpec, Profile, SessionModules};
use acos_services::ServiceError;

fn java_binder(wire: u64) -> u64 {
    (1 << 32) | wire
}

/// `IServiceManager.<method>(name, …)` on the host's own servicemanager binder.
fn service_manager(host: &mut acos_services::ServiceHost, code: u32, body: impl FnOnce(&mut Parcel)) -> Result<Parcel, ServiceError> {
    let binder = host
        .binder_for_descriptor("android.os.IServiceManager")
        .expect("the host answers IServiceManager");
    let mut data = Parcel::new();
    data.write_interface_token("android.os.IServiceManager")
        .expect("token");
    body(&mut data);
    data.set_data_position(0);
    let mut reply = Parcel::new();
    host.transact(binder, code, &mut data, &mut reply, 0)?;
    reply.set_data_position(0);
    reply.read_int32().expect("exception header");
    Ok(reply)
}

fn session() -> SessionModules {
    let mut modules = SessionModules::new();
    modules.attach(&[]).expect("core attaches");
    modules
}

#[test]
fn a_registered_module_name_resolves_without_a_notification() {
    let mut modules = SessionModules::new();
    modules
        .attach(&[ModuleSpec::new(
            "hw.bluetooth",
            Profile::Scripted,
        )])
        .expect("attach");
    let host = modules.host_mut();
    let mut reply = service_manager(host, 1, |data| {
        data.write_string16("bluetooth_manager").expect("name");
    })
    .expect("getService");
    let binder = reply.read_strong_binder().expect("binder word");
    assert!(
        binder.is_some(),
        "a registered module name must answer a binder (the waitForService fast path)"
    );
}

#[test]
fn a_declared_module_name_is_declared_but_not_resolved() {
    let mut modules = session();
    let host = modules.host_mut();
    // `hw.bluetooth` is off, so the name is not registered ...
    let mut reply = service_manager(host, 2, |data| {
        data.write_string16("bluetooth_manager").expect("name");
    })
    .expect("checkService");
    assert!(
        reply.read_strong_binder().expect("binder word").is_none(),
        "an unattached module's name is not running (AOSP: getService answers null)"
    );
    // ... but it is declared: a shipped module provides it.
    let mut reply = service_manager(host, 7, |data| {
        data.write_string16("bluetooth_manager").expect("name");
    })
    .expect("isDeclared");
    assert_eq!(reply.read_int32().expect("bool"), 1);
}

#[test]
fn a_registration_for_a_registered_name_answers_at_once() {
    let mut modules = SessionModules::new();
    modules
        .attach(&[ModuleSpec::new(
            "hw.bluetooth",
            Profile::Scripted,
        )])
        .expect("attach");
    let host = modules.host_mut();
    let callback = java_binder(61);
    service_manager(host, 5, |data| {
        data.write_string16("bluetooth_manager").expect("name");
        data.write_strong_binder(BinderId(callback));
    })
    .expect("registerForNotifications");

    // AOSP calls the callback immediately when the service is already there.
    let deliveries = host.take_deliveries();
    assert_eq!(deliveries.len(), 1, "one immediate onRegistration");
    let delivery = &deliveries[0];
    assert_eq!(delivery.target, BinderId(callback));
    assert_eq!(delivery.interface, "android.os.IServiceCallback");
    assert_eq!(delivery.code, 1, "onRegistration");
    let mut data = delivery.data.clone();
    data.set_data_position(0);
    data.enforce_interface("android.os.IServiceCallback")
        .expect("IServiceCallback token");
    // `@utf8InCpp String name`: AIDL's writeUtf8 layout = writeString8.
    assert_eq!(
        data.read_string8().expect("utf8 name").as_deref(),
        Some("bluetooth_manager")
    );
    let binder = data.read_strong_binder().expect("binder word");
    assert!(binder.is_some(), "onRegistration carries the service binder");
    assert_eq!(host.pending_notifications(), 0);
}

#[test]
fn a_declared_name_fires_the_callback_when_the_module_attaches() {
    let mut modules = session();
    let callback = java_binder(62);
    {
        let host = modules.host_mut();
        service_manager(host, 5, |data| {
            data.write_string16("bluetooth_manager").expect("name");
            data.write_strong_binder(BinderId(callback));
        })
        .expect("registerForNotifications for a declared name");
        assert!(host.take_deliveries().is_empty(), "nothing to deliver yet");
        assert_eq!(host.pending_notifications(), 1, "the callback is kept");
    }

    // The live attach is the event the client is waiting for.
    modules
        .attach(&[ModuleSpec::new(
            "hw.bluetooth",
            Profile::Scripted,
        )])
        .expect("live attach");
    let host = modules.host_mut();
    let deliveries = host.take_deliveries();
    assert_eq!(deliveries.len(), 1, "one onRegistration once the name exists");
    assert_eq!(deliveries[0].target, BinderId(callback));
    assert_eq!(deliveries[0].interface, "android.os.IServiceCallback");
    assert_eq!(host.pending_notifications(), 0, "fired exactly once");
}

#[test]
fn a_name_nothing_can_provide_is_refused_with_the_reason() {
    let mut modules = session();
    let host = modules.host_mut();
    let error = service_manager(host, 5, |data| {
        data.write_string16("no_such_service").expect("name");
        data.write_strong_binder(BinderId(java_binder(63)));
    })
    .expect_err("an unservable name cannot be waited for");
    let ServiceError::BadRequest { detail, .. } = &error else {
        panic!("expected a structured BadRequest, got {error:?}");
    };
    assert!(
        detail.contains("no_such_service") && detail.contains("never appear"),
        "{detail}"
    );
    assert_eq!(error.exception_code(), -3, "EX_ILLEGAL_ARGUMENT");
}

#[test]
fn unregistering_a_pending_callback_answers_ok_and_an_unknown_one_is_an_error() {
    let mut modules = session();
    let callback = java_binder(64);
    let host = modules.host_mut();
    service_manager(host, 5, |data| {
        data.write_string16("bluetooth_manager").expect("name");
        data.write_strong_binder(BinderId(callback));
    })
    .expect("register");
    assert_eq!(host.pending_notifications(), 1);

    service_manager(host, 6, |data| {
        data.write_string16("bluetooth_manager").expect("name");
        data.write_strong_binder(BinderId(callback));
    })
    .expect("unregister");
    assert_eq!(host.pending_notifications(), 0);

    // AOSP: EX_ILLEGAL_STATE, "Nothing to unregister."
    let error = service_manager(host, 6, |data| {
        data.write_string16("bluetooth_manager").expect("name");
        data.write_strong_binder(BinderId(callback));
    })
    .expect_err("nothing left to unregister");
    assert!(matches!(error, ServiceError::IllegalState { .. }), "{error:?}");
}

/// The registry half: the names a session declares are exactly the names its
/// modules provide, so `isDeclared` cannot claim a name no module owns.
#[test]
fn the_declared_set_is_the_module_set() {
    let registry = ModuleRegistry::standard();
    let provided = registry.provided_names();
    for name in ["phone", "isms", "bluetooth_manager", "bluetooth", "auth", "vibrator_manager"] {
        assert!(provided.contains(&name), "{name} must be declared: {provided:?}");
    }
    let modules = session();
    let host = modules.host();
    for name in &provided {
        assert!(host.is_declared(name), "{name} must be declared on the host");
    }
    assert!(
        !host.is_declared("no_such_service"),
        "a name no module provides is not declared"
    );

    // `core` is attached, so its names are registered, not merely declared.
    assert!(host.declared_names().contains(&"phone"));
    assert!(
        !host.declared_names().contains(&"package"),
        "a registered name is not a declaration gap; names={:?} gaps={:?}",
        host.service_names(),
        host.declared_names()
    );
}
