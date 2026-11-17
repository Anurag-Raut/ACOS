//! BINDERREF2: a registration is a host-held strong reference.
//!
//! Every module that stores a callback binder takes a strong reference on it —
//! on Android that is what `Parcel::readStrongBinder` + `sp<IBinder>` do — and
//! drops it when the registration goes away. In ACOS the host is a separate
//! process from the guest, so the reference is a message to the binder driver
//! (`F_REF` + `REF_RELEASE` → `BR_RELEASE` + `BR_DECREFS`): without it the
//! driver unroots the guest's node the moment the app's own reference goes
//! away, and a callback answers `DEAD_OBJECT` while the registration is still
//! live.
//!
//! These tests are the host half: they drive the **real** dispatch path (the
//! decoded `acos-aidl` codec or the module's raw wire form), then assert what
//! the host holds (`held_refs`) and what it told the peer to release
//! (`take_released_binders`). The guest half is the ART evidence in
//! `results/handoff/BINDERREF2/`.

use acos_parcel::{BinderId, Parcel};
use acos_services::host::ServiceHost;
use acos_services::module::{ModuleConfig, ModuleRegistry, Profile};
use acos_services::value::Value;

/// `acos-binder-peer`'s `JAVA_TAG`: every guest-owned binder arrives tagged, and
/// that tag is what makes a reference a reference the driver can act on.
const GUEST: u64 = 1 << 32;

fn guest(wire: u64) -> BinderId {
    BinderId(GUEST | wire)
}

fn call(host: &mut ServiceHost, name: &str, code: u32, data: &mut Parcel) -> Parcel {
    let binder = host
        .check_service(name)
        .expect("lookup is infallible")
        .unwrap_or_else(|| panic!("{name} did not resolve to a binder"));
    data.set_data_position(0);
    let mut reply = Parcel::new();
    host.transact(binder, code, data, &mut reply, 0)
        .unwrap_or_else(|error| panic!("{name} code {code}: {error}"));
    reply.set_data_position(0);
    reply
}

fn decoded(
    host: &mut ServiceHost,
    name: &str,
    interface: &str,
    code: u32,
    args: Vec<Value>,
) -> Parcel {
    let method = acos_aidl::lookup(interface, code).expect("table row");
    let mut data = acos_services::codec::build_request(interface, method, &args).expect("request");
    call(host, name, code, &mut data)
}

fn registry_with(host: &mut ServiceHost, module: &str, profile: Profile) -> ModuleRegistry {
    let mut registry = ModuleRegistry::standard();
    if module != "net.telephony" {
        registry
            .attach(host, "core", Profile::Real, &ModuleConfig::default())
            .expect("core attaches");
    }
    registry
        .attach(host, module, profile, &ModuleConfig::default())
        .unwrap_or_else(|error| panic!("{module} attaches: {error}"));
    registry
}

// ---------------------------------------------------------------------------
// net.telephony — ITelephonyRegistry
// ---------------------------------------------------------------------------

fn telephony_listen(host: &mut ServiceHost, listener: BinderId, events: &[i32]) {
    let mut data = Parcel::new();
    data.write_interface_token("com.android.internal.telephony.ITelephonyRegistry")
        .expect("token");
    data.write_int32(0); // renounceFineLocationAccess
    data.write_int32(0); // renounceCoarseLocationAccess
    data.write_int32(-1); // subId
    data.write_string16("dev.acos.fixtures.tailprobe")
        .expect("package");
    data.write_string16_opt(None).expect("feature");
    data.write_strong_binder(listener);
    data.write_int32_array(Some(events)).expect("events");
    data.write_int32(1); // notifyNow
    let _ = call(host, "telephony.registry", 4, &mut data);
}

#[test]
fn a_registered_phone_state_listener_is_referenced_until_listen_none() {
    let mut host = ServiceHost::standard();
    let _registry = registry_with(&mut host, "net.telephony", Profile::Scripted);

    let listener = guest(0x11);
    assert_eq!(host.held_refs(listener), 0, "nothing held yet");
    telephony_listen(&mut host, listener, &[36]);
    assert_eq!(
        host.held_refs(listener),
        1,
        "a registration is one strong reference on the guest's listener"
    );
    let _ = host.take_deliveries();
    assert!(
        host.take_released_binders().is_empty(),
        "taking a reference is not a release"
    );

    // Re-registering the same listener (what `PhoneStateListener.listen` does on
    // every call) is not a second reference, and does not drop the first.
    telephony_listen(&mut host, listener, &[36]);
    assert_eq!(host.held_refs(listener), 1);
    assert!(host.take_released_binders().is_empty());

    // `LISTEN_NONE`: the registration goes away with its reference.
    telephony_listen(&mut host, listener, &[]);
    assert_eq!(host.held_refs(listener), 0);
    assert_eq!(
        host.take_released_binders(),
        vec![listener],
        "the peer is told to release the node (`F_REF` release)"
    );
}

#[test]
fn a_telephony_module_detach_releases_the_listeners_it_rooted() {
    let mut host = ServiceHost::standard();
    let mut registry = registry_with(&mut host, "net.telephony", Profile::Scripted);

    let first = guest(0x21);
    let second = guest(0x22);
    telephony_listen(&mut host, first, &[36]);
    telephony_listen(&mut host, second, &[6]);
    let _ = host.take_deliveries();
    let _ = host.take_released_binders();
    assert_eq!(host.held_refs(first), 1);
    assert_eq!(host.held_refs(second), 1);

    // The module detaches while the guest is alive: the registrations are gone,
    // so the nodes must not stay rooted (the object is dropped with the scope,
    // and a `Drop` has no host to report to).
    registry.detach(&mut host, "net.telephony").expect("detach");
    assert_eq!(host.held_refs(first), 0);
    assert_eq!(host.held_refs(second), 0);
    let released = host.take_released_binders();
    assert!(released.contains(&first), "released: {released:?}");
    assert!(released.contains(&second), "released: {released:?}");
}

// ---------------------------------------------------------------------------
// location — ILocationManager
// ---------------------------------------------------------------------------

const LOCATION: &str = "android.location.ILocationManager";

fn write_location_request(data: &mut Parcel, provider: &str) {
    data.write_int32(1);
    data.write_string16(provider).expect("request provider");
    data.write_int64(1000);
    data.write_int32(0);
    data.write_int64(0);
    data.write_int64(0);
    data.write_int32(i32::MAX);
    data.write_int64(0);
    data.write_float(0.0);
    data.write_int64(0);
    data.write_int32(0);
    data.write_int32(0);
    data.write_int32(0);
    data.write_int32(0);
    data.write_int32(0);
}

fn location_register(host: &mut ServiceHost, listener: BinderId) {
    let mut data = Parcel::new();
    data.write_interface_token(LOCATION).expect("token");
    data.write_string16("gps").expect("provider");
    write_location_request(&mut data, "gps");
    data.write_strong_binder(listener);
    data.write_string16("dev.acos.fixtures.locationprobe")
        .expect("package");
    data.write_string16_opt(None).expect("tag");
    data.write_string16("listener-id").expect("listener id");
    let _ = call(host, "location", 3, &mut data);
}

fn location_unregister(host: &mut ServiceHost, listener: BinderId) {
    let mut data = Parcel::new();
    data.write_interface_token(LOCATION).expect("token");
    data.write_strong_binder(listener);
    let _ = call(host, "location", 4, &mut data);
}

#[test]
fn a_location_listener_is_referenced_and_unregister_releases_it() {
    let mut host = ServiceHost::standard();
    let _registry = registry_with(&mut host, "location", Profile::Scripted);

    let listener = guest(0x31);
    location_register(&mut host, listener);
    assert_eq!(host.held_refs(listener), 1, "the listener is rooted");
    let _ = host.take_deliveries();
    let _ = host.take_released_binders();

    // A second registration of the same listener replaces the first: still one
    // reference.
    location_register(&mut host, listener);
    assert_eq!(host.held_refs(listener), 1);
    assert!(host.take_released_binders().is_empty());

    location_unregister(&mut host, listener);
    assert_eq!(host.held_refs(listener), 0);
    assert_eq!(host.take_released_binders(), vec![listener]);

    // Unregistering what was never registered must not tell the driver to
    // unroot a node the guest still owns.
    location_unregister(&mut host, guest(0x32));
    assert!(
        host.take_released_binders().is_empty(),
        "a release for a reference that was never taken is not a message"
    );
}

#[test]
fn a_location_module_detach_releases_the_listeners_it_rooted() {
    let mut host = ServiceHost::standard();
    let mut registry = registry_with(&mut host, "location", Profile::Scripted);

    let listener = guest(0x41);
    location_register(&mut host, listener);
    let _ = host.take_deliveries();
    let _ = host.take_released_binders();
    assert_eq!(host.held_refs(listener), 1);

    registry.detach(&mut host, "location").expect("detach");
    assert_eq!(
        host.held_refs(listener),
        0,
        "the module's node empties itself; the reference goes with it"
    );
    assert!(host.take_released_binders().contains(&listener));
}

#[test]
fn a_location_pending_intent_registration_references_its_sender() {
    let mut host = ServiceHost::standard();
    let _registry = registry_with(&mut host, "location", Profile::Scripted);
    // The framework's `PendingIntent` is a host node (`IIntentSender`), which is
    // what the app hands back to `registerLocationPendingIntent`.
    let sender = host
        .binder_registry()
        .register_local("android.content.IIntentSender");

    let mut data = Parcel::new();
    data.write_interface_token(LOCATION).expect("token");
    data.write_string16("gps").expect("provider");
    write_location_request(&mut data, "gps");
    data.write_int32(1); // the PendingIntent is present
    data.write_strong_binder(sender);
    data.write_string16("dev.acos.fixtures.locationprobe")
        .expect("package");
    data.write_string16_opt(None).expect("tag");
    let _ = call(&mut host, "location", 5, &mut data);
    assert_eq!(
        host.held_refs(sender),
        1,
        "the PendingIntent form is a registration on its sender, like the listener form"
    );

    let mut data = Parcel::new();
    data.write_interface_token(LOCATION).expect("token");
    data.write_int32(1);
    data.write_strong_binder(sender);
    let _ = call(&mut host, "location", 6, &mut data);
    assert_eq!(host.held_refs(sender), 0, "unregister releases the sender");
}

// ---------------------------------------------------------------------------
// IIntentReceiver (broadcast receivers) — decoded IActivityManager
// ---------------------------------------------------------------------------

const AM: &str = "android.app.IActivityManager";

fn register_receiver(host: &mut ServiceHost, receiver: BinderId) {
    let mut data = Parcel::new();
    data.write_interface_token(AM).expect("token");
    data.write_strong_binder_opt(Some(BinderId(GUEST | 0x99))); // caller thread
    data.write_string16("dev.acos.fixtures.tailprobe")
        .expect("caller package");
    data.write_string16("").expect("caller feature id");
    data.write_string16("reload-receiver").expect("receiver id");
    data.write_strong_binder_opt(Some(receiver));
    data.write_int32(1); // filter present
    data.write_int32(1); // one action
    data.write_string16("com.facebook.react.devsupport.RELOAD")
        .expect("action");
    for _ in 0..5 {
        data.write_int32(0); // categories, schemes, static/dynamic data types, mime groups
    }
    for _ in 0..3 {
        data.write_int32(0); // scheme-specific parts, authorities, paths
    }
    for _ in 0..6 {
        data.write_int32(0); // priority, partial-type flags, autoVerify, visibility, order
    }
    data.write_int32(0);
    data.write_string16("").expect("permission");
    data.write_int32(-2); // userId
    data.write_int32(0x4); // RECEIVER_NOT_EXPORTED
    let _ = call(host, "activity", 19, &mut data);
}

fn unregister_receiver(host: &mut ServiceHost, receiver: BinderId) {
    let mut data = Parcel::new();
    data.write_interface_token(AM).expect("token");
    data.write_strong_binder_opt(Some(receiver));
    let _ = call(host, "activity", 20, &mut data);
}

#[test]
fn a_registered_broadcast_receiver_is_referenced_until_unregister() {
    let mut host = ServiceHost::standard();
    let receiver = guest(0x51);

    register_receiver(&mut host, receiver);
    assert_eq!(
        host.held_refs(receiver),
        1,
        "`registerReceiver` is a strong reference on the guest's IIntentReceiver"
    );
    let _ = host.take_released_binders();

    // The same receiver registered again is the same registration.
    register_receiver(&mut host, receiver);
    assert_eq!(host.held_refs(receiver), 1);
    assert!(host.take_released_binders().is_empty());

    unregister_receiver(&mut host, receiver);
    assert_eq!(host.held_refs(receiver), 0);
    assert_eq!(host.take_released_binders(), vec![receiver]);

    // Broadcasts still reach a *registered* receiver: the reference is not what
    // gates delivery, the registration record is.
    let kept = guest(0x52);
    register_receiver(&mut host, kept);
    assert_eq!(host.held_refs(kept), 1);
}

// ---------------------------------------------------------------------------
// IAccessibilityManager — clients and interaction connections
// ---------------------------------------------------------------------------

const A11Y: &str = "android.view.accessibility.IAccessibilityManager";
const A11Y_SERVICE: &str = "accessibility";

#[test]
fn the_a11y_manager_references_its_client_and_its_interaction_connections() {
    let mut host = ServiceHost::standard();
    let client = guest(0x61);
    let window = guest(0x62);
    let connection = guest(0x63);

    let _ = decoded(
        &mut host,
        A11Y_SERVICE,
        A11Y,
        3,
        vec![Value::Binder(Some(client)), Value::Int(0)],
    );
    assert_eq!(
        host.held_refs(client),
        1,
        "`addClient` roots the app's IAccessibilityManagerClient"
    );

    let _ = decoded(
        &mut host,
        A11Y_SERVICE,
        A11Y,
        7,
        vec![
            Value::Binder(Some(window)),
            Value::Binder(None),
            Value::Binder(Some(connection)),
            Value::string("dev.acos.fixtures.tailprobe"),
            Value::Int(0),
        ],
    );
    assert_eq!(
        host.held_refs(connection),
        1,
        "the window's interaction connection is reachable only through this binder"
    );
    let _ = host.take_deliveries();
    let _ = host.take_released_binders();

    let _ = decoded(
        &mut host,
        A11Y_SERVICE,
        A11Y,
        8,
        vec![Value::Binder(Some(window))],
    );
    assert_eq!(host.held_refs(connection), 0);
    assert!(host.take_released_binders().contains(&connection));

    let _ = decoded(
        &mut host,
        A11Y_SERVICE,
        A11Y,
        4,
        vec![Value::Binder(Some(client)), Value::Int(0)],
    );
    assert_eq!(host.held_refs(client), 0);
    assert!(host.take_released_binders().contains(&client));
}

// ---------------------------------------------------------------------------
// IConnectivityManager — network callbacks
// ---------------------------------------------------------------------------

const CONNECTIVITY: &str = "android.net.IConnectivityManager";

fn write_typed_network_capabilities(data: &mut Parcel) {
    data.write_int32(1);
    acos_services::parcelable::write_payload(
        data,
        &acos_services::connectivity::default_network_capabilities(),
    )
    .expect("network capabilities");
}

#[test]
fn a_network_callback_messenger_is_referenced_until_the_request_is_released() {
    let mut host = ServiceHost::standard();
    let messenger = guest(0x71);

    // `listenForNetwork(NetworkCapabilities, Messenger, IBinder, int, String,
    // String)` (code 45).
    let mut data = Parcel::new();
    data.write_interface_token(CONNECTIVITY).expect("token");
    write_typed_network_capabilities(&mut data);
    data.write_int32(1); // messenger present
    data.write_strong_binder(messenger);
    data.write_strong_binder(guest(0x72)); // binder
    data.write_int32(0); // callbackFlags
    data.write_string16("dev.acos.fixtures.tailprobe")
        .expect("package");
    data.write_string16("").expect("attribution tag");
    let mut reply = call(&mut host, "connectivity", 45, &mut data);
    assert_eq!(
        host.held_refs(messenger),
        1,
        "a registered network callback is a reference on its IMessenger"
    );
    let _ = host.take_released_binders();

    // The reply is the `NetworkRequest`; its requestId is what
    // `releaseNetworkRequest` (code 47) names.
    assert_eq!(
        reply.read_int32().expect("exception header"),
        0,
        "no exception"
    );
    let present = reply.read_int32().expect("NetworkRequest presence");
    assert_eq!(present, 1);
    let request = acos_services::parcelable::read_payload("android.net.NetworkRequest", &mut reply)
        .expect("NetworkRequest payload");

    let mut data = Parcel::new();
    data.write_interface_token(CONNECTIVITY).expect("token");
    data.write_int32(1);
    acos_services::parcelable::write_payload(&mut data, &request).expect("request payload");
    let _ = call(&mut host, "connectivity", 47, &mut data);
    assert_eq!(host.held_refs(messenger), 0);
    assert!(host.take_released_binders().contains(&messenger));
}

// ---------------------------------------------------------------------------
// session teardown
// ---------------------------------------------------------------------------

#[test]
fn releasing_everything_leaves_no_reference_behind_for_a_dead_guest() {
    let mut host = ServiceHost::standard();
    let _registry = registry_with(&mut host, "net.telephony", Profile::Scripted);
    let listener = guest(0x81);
    telephony_listen(&mut host, listener, &[36]);
    let a11y_client = guest(0x82);
    let _ = decoded(
        &mut host,
        A11Y_SERVICE,
        A11Y,
        3,
        vec![Value::Binder(Some(a11y_client)), Value::Int(0)],
    );
    let _ = host.take_deliveries();
    let _ = host.take_released_binders();
    assert_eq!(host.held_refs(listener), 1);
    assert_eq!(host.held_refs(a11y_client), 1);

    let released = host.release_all_refs();
    assert!(released.contains(&listener), "released: {released:?}");
    assert!(released.contains(&a11y_client), "released: {released:?}");
    assert_eq!(host.held_refs(listener), 0);
    assert_eq!(host.held_refs(a11y_client), 0);
    assert!(host.release_all_refs().is_empty(), "a second pass is empty");
}
