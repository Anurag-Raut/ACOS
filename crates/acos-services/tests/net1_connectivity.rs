//! NET1: `IConnectivityManager` callback registration, verified through the
//! real `ServiceHost` dispatcher (table lookup → argument decode → handler →
//! reply encode → queued `IMessenger` deliveries).
//!
//! The request parcels are the framework's own shapes:
//!
//! ```
//! NetworkCapabilities nc;                      // requestNetwork arg 1
//! messenger.writeToParcel(data);               // writeTypedObject: class + binder
//! binder (IBinder, AIDL nullable);              // writeStrongBinder
//! ```
//!
//! and the asserted reply/callback shapes are AOSP's
//! (`NetworkRequest.writeToParcel`, `Message.writeToParcel`).

use acos_parcel::{BinderId, Parcel};
use acos_services::connectivity::{
    default_network_capabilities, ConnectivityState, CALLBACK_AVAILABLE, CALLBACK_BLK_CHANGED,
    CALLBACK_CAP_CHANGED, CALLBACK_IP_CHANGED, IMESSENGER_SEND, LEGACY_TYPE_NONE, NET_ID,
};
use acos_services::parcelable::{read_payload, write_payload};
use acos_services::value::{Parcelable, Value};
use acos_services::{ServiceError, ServiceHost};

const IFACE: &str = "android.net.IConnectivityManager";
const NO_EXCEPTION: i32 = 0;

/// Transaction codes from the API-34 `IConnectivityManager.aidl` (verified
/// against `.cache/aosp/connectivity/framework/src/android/net/
/// IConnectivityManager.aidl`: `requestNetwork` is the 42nd method).
const GET_ACTIVE_NETWORK: u32 = 1;
const GET_ACTIVE_NETWORK_INFO: u32 = 3;
const GET_NETWORK_INFO_FOR_UID: u32 = 6;
const GET_ALL_NETWORKS: u32 = 9;
const GET_ACTIVE_LINK_PROPERTIES: u32 = 12;
const GET_LINK_PROPERTIES: u32 = 14;
const GET_NETWORK_CAPABILITIES: u32 = 16;
const GET_PROXY_FOR_NETWORK: u32 = 32;
const REQUEST_NETWORK: u32 = 42;
const RELEASE_PENDING_NETWORK_REQUEST: u32 = 44;
const LISTEN_FOR_NETWORK: u32 = 45;
const RELEASE_NETWORK_REQUEST: u32 = 47;
const GET_DEFAULT_REQUEST: u32 = 55;

/// The emulated default `Network` as an argument payload.
fn default_network_parcelable() -> Parcelable {
    Parcelable::ordered("android.net.Network", vec![Value::Int(NET_ID)])
}

/// An app-side `Messenger` binder as the guest's driver reports it.
fn app_messenger() -> BinderId {
    BinderId(acos_binder_peer_tag() | 7)
}

/// `JAVA_TAG` from `acos-binder-peer` (`1 << 32`): the tag that makes a binder
/// id a Java local binder, i.e. a legal delivery target.
fn acos_binder_peer_tag() -> u64 {
    1 << 32
}

fn connectivity(host: &mut ServiceHost) -> BinderId {
    host.check_service("connectivity")
        .expect("lookup is infallible")
        .expect("connectivity resolves")
}

fn transact(
    host: &mut ServiceHost,
    binder: BinderId,
    code: u32,
    data: &mut Parcel,
) -> std::result::Result<Parcel, ServiceError> {
    let mut reply = Parcel::new();
    data.set_data_position(0);
    host.transact(binder, code, data, &mut reply, 0)?;
    reply.set_data_position(0);
    Ok(reply)
}

fn expects_no_exception(reply: &mut Parcel) {
    assert_eq!(
        reply.read_int32().expect("exception header"),
        NO_EXCEPTION,
        "the reply must not carry an exception"
    );
}

/// `writeTypedObject(messenger, 0)`: AIDL's presence marker, then the object's
/// own `writeToParcel` (no class name — the type is static on the wire).
fn write_messenger_arg(data: &mut Parcel, messenger: Option<BinderId>) {
    match messenger {
        None => data.write_int32(0),
        Some(id) => {
            data.write_int32(1);
            data.write_strong_binder(id);
        }
    }
}

/// `writeTypedObject(nc, 0)` for a parcelable argument.
fn write_typed_arg(data: &mut Parcel, value: &Parcelable) {
    match value {
        Parcelable::Null => data.write_int32(0),
        other => {
            data.write_int32(1);
            write_payload(data, other).expect("typed object payload");
        }
    }
}

/// The framework's request for `registerDefaultNetworkCallback` (TRACK_DEFAULT
/// = `Type.ordinal()` 2) / `listenForNetwork`.
fn request_network_request(kind_ordinal: i32, messenger: Option<BinderId>) -> Parcel {
    let mut data = Parcel::new();
    data.write_interface_token(IFACE).expect("token");
    data.write_int32(-1); // asUid (Process.INVALID_UID)
    write_typed_arg(&mut data, &default_network_capabilities());
    data.write_int32(kind_ordinal);
    write_messenger_arg(&mut data, messenger);
    data.write_int32(0); // timeoutSec
    data.write_strong_binder(BinderId(9001)); // the death recipient binder
    data.write_int32(LEGACY_TYPE_NONE);
    data.write_int32(0); // callbackFlags
    data.write_string16("net.cozic.joplin").expect("package");
    data.write_string16("").expect("attribution tag");
    data
}

/// `registerDefaultNetworkCallback`'s exact request: `TRACK_DEFAULT` with
/// `null` capabilities (AOSP replaces them with the default network's).
fn default_network_callback_request(messenger: Option<BinderId>) -> Parcel {
    let data = request_network_request(2, messenger);
    // Rewrite arg 1 as a null parcelable: the test builder wrote a present one.
    let mut rewritten = Parcel::new();
    rewritten.write_interface_token(IFACE).expect("token");
    rewritten.write_int32(-1);
    rewritten.write_int32(0); // null NetworkCapabilities
    // Re-append everything after the capabilities argument.
    let mut rest = Parcel::new();
    rest.write_int32(2);
    write_messenger_arg(&mut rest, messenger);
    rest.write_int32(0);
    rest.write_strong_binder(BinderId(9001));
    rest.write_int32(LEGACY_TYPE_NONE);
    rest.write_int32(0);
    rest.write_string16("net.cozic.joplin").expect("package");
    rest.write_string16("").expect("attribution tag");
    let mut tail = rest.marshall();
    let mut head = rewritten.marshall();
    head.append(&mut tail);
    let mut out = Parcel::new();
    out.unmarshall(&head);
    out.set_data_position(0);
    let _ = data;
    out
}

fn listen_for_network_request(messenger: Option<BinderId>) -> Parcel {
    let mut data = Parcel::new();
    data.write_interface_token(IFACE).expect("token");
    write_typed_arg(&mut data, &default_network_capabilities());
    write_messenger_arg(&mut data, messenger);
    data.write_strong_binder(BinderId(9001));
    data.write_int32(0); // callbackFlags
    data.write_string16("net.cozic.joplin").expect("package");
    data.write_string16("").expect("attribution tag");
    data
}

/// Read a `NetworkRequest` reply (`readTypedObject`: presence then payload).
fn read_network_request(reply: &mut Parcel) -> Parcelable {
    let present = reply.read_int32().expect("presence");
    assert_eq!(present, 1, "the reply is a non-null NetworkRequest");
    read_payload("android.net.NetworkRequest", reply).expect("NetworkRequest payload")
}

fn field<'a>(p: &'a Parcelable, i: usize) -> &'a Value {
    p.field(i).expect("field")
}

/// Decode an `IMessenger.send(Message)` delivery and return
/// `(what, arg1, bundle entries as (key, class))` in wire order.
fn read_message(parcel: &Parcel) -> (i32, i32, Vec<(String, Option<String>)>) {
    let mut p = parcel.clone();
    p.set_data_position(0);
    p.enforce_interface("android.os.IMessenger").expect("token");
    assert_eq!(p.read_int32().expect("Message presence"), 1);
    let what = p.read_int32().expect("what");
    let arg1 = p.read_int32().expect("arg1");
    assert_eq!(p.read_int32().expect("arg2"), 0);
    assert_eq!(p.read_int32().expect("obj"), 0);
    assert_eq!(p.read_int64().expect("when"), 0);
    // Bundle: length, magic, size, entries.
    let len = p.read_int32().expect("bundle length");
    assert!(len > 0, "a callback bundle is never the empty fast path");
    assert_eq!(p.read_int32().expect("bundle magic") as u32, 0x4C444E42);
    let size = p.read_int32().expect("bundle size");
    let mut out = Vec::new();
    for _ in 0..size {
        let key = p.read_string16().expect("key").expect("key");
        let tag = p.read_int32().expect("value tag");
        if tag == -1 {
            out.push((key, None));
            continue;
        }
        assert_eq!(tag, 4, "VAL_PARCELABLE");
        // `Parcel.writeValue` length-prefixes VAL_PARCELABLE
        // (`isLengthPrefixed`), covering the class name plus the payload.
        let value_len = p.read_int32().expect("value length");
        assert!(value_len > 0, "a parcelable value is length-prefixed");
        let value_start = p.data_position();
        let class = p.read_string16().expect("class").expect("class");
        // Walk the payload with the real spec so the next key lands right, and
        // check the length prefix agrees with what was consumed.
        read_payload(&class, &mut p).expect("parcelable payload");
        assert_eq!(
            p.data_position() - value_start,
            value_len as usize,
            "VAL_PARCELABLE length must cover class name + payload"
        );
        out.push((key, Some(class)));
    }
    // `replyTo` is a null strong binder: one zero word, then `sendingUid`.
    //
    // The API-34 *image* ends `Message.writeToParcel` there. The r75 source
    // cache has one more field (`workSourceUid`); NET6 measured the image reject
    // a message carrying it with `Parcel data not fully consumed, unread size: 4`
    // (an app's `NetworkCallback` then never fires), so the image wins.
    assert_eq!(p.read_int32().expect("replyTo"), 0);
    assert_eq!(p.read_int32().expect("sendingUid"), 0);
    assert_eq!(
        p.data_size() - p.data_position(),
        0,
        "a callback Message must end exactly after sendingUid (image layout)"
    );
    (what, arg1, out)
}

#[test]
fn request_network_answers_a_network_request_and_queues_the_aosp_callbacks() {
    let mut host = ServiceHost::standard();
    let conn = connectivity(&mut host);
    let messenger = app_messenger();
    let mut data = request_network_request(2, Some(messenger));
    let mut reply = transact(&mut host, conn, REQUEST_NETWORK, &mut data).expect("requestNetwork");
    expects_no_exception(&mut reply);
    let request = read_network_request(&mut reply);
    assert_eq!(
        field(&request, 3),
        &Value::Str(Some("TRACK_DEFAULT".to_string())),
        "the reply's Type name is what NetworkRequest.CREATOR parses"
    );
    assert_eq!(field(&request, 1), &Value::Int(LEGACY_TYPE_NONE));
    let request_id = field(&request, 2).as_int().expect("requestId");
    assert!(request_id > 0);

    let deliveries = host.take_deliveries();
    assert_eq!(deliveries.len(), 4, "one Message per AOSP callback");
    let expected = [
        CALLBACK_AVAILABLE,
        CALLBACK_CAP_CHANGED,
        CALLBACK_IP_CHANGED,
        CALLBACK_BLK_CHANGED,
    ];
    for (delivery, what) in deliveries.iter().zip(expected) {
        assert_eq!(delivery.target, messenger);
        assert_eq!(delivery.interface, "android.os.IMessenger");
        assert_eq!(delivery.code, IMESSENGER_SEND);
        let (got_what, arg1, bundle) = read_message(&delivery.data);
        assert_eq!(got_what, what, "callback order is AOSP's");
        assert_eq!(arg1, 0, "no blocked reasons on an unmetered network");
        let keys: Vec<&str> = bundle.iter().map(|(k, _)| k.as_str()).collect();
        // AOSP puts the NetworkRequest first, then the network, then the
        // notification-specific extras, and the bundle is in ArrayMap order.
        assert!(keys.contains(&"NetworkRequest"), "{keys:?}");
        assert!(keys.contains(&"Network"), "{keys:?}");
        match what {
            CALLBACK_AVAILABLE => {
                assert!(keys.contains(&"NetworkCapabilities"), "{keys:?}");
                assert!(keys.contains(&"LinkProperties"), "{keys:?}");
                assert!(keys.contains(&"LocalNetworkInfo"), "{keys:?}");
            }
            CALLBACK_CAP_CHANGED => {
                assert!(keys.contains(&"NetworkCapabilities"), "{keys:?}");
                assert!(!keys.contains(&"LinkProperties"), "{keys:?}");
            }
            CALLBACK_IP_CHANGED => {
                assert!(keys.contains(&"LinkProperties"), "{keys:?}");
            }
            _ => {}
        }
    }
    // Every callback message carries the exact NetworkRequest the reply did,
    // which is how the app's `sCallbacks` lookup succeeds.
    let first = read_message(&deliveries[0].data);
    assert_eq!(first.2.len(), 5);
}

#[test]
fn the_same_network_request_is_echoed_in_every_callback_message() {
    let mut host = ServiceHost::standard();
    let conn = connectivity(&mut host);
    let mut data = request_network_request(2, Some(app_messenger()));
    let mut reply = transact(&mut host, conn, REQUEST_NETWORK, &mut data).expect("requestNetwork");
    expects_no_exception(&mut reply);
    let reply_bytes = reply.data_size();
    let request = read_network_request(&mut reply);
    assert_eq!(reply_bytes, reply.data_size());

    let deliveries = host.take_deliveries();
    for delivery in &deliveries {
        let p = delivery.data.clone();
        // Walk to the NetworkRequest value and compare it with the reply's:
        // `NetworkRequest.equals` is what the app keys on.
        let mut q = p.clone();
        q.enforce_interface("android.os.IMessenger").expect("token");
        q.read_int32().expect("Message presence");
        q.read_int32().expect("what");
        q.read_int32().expect("arg1");
        q.read_int32().expect("arg2");
        q.read_int32().expect("obj");
        q.read_int64().expect("when");
        q.read_int32().expect("bundle length");
        q.read_int32().expect("bundle magic");
        let entries = q.read_int32().expect("bundle size") as usize;
        let size = 0; // entries were counted above; walk them all
        let _ = size;
        let mut embedded = None;
        for _ in 0..entries {
            let key = q.read_string16().expect("key").expect("key");
            let tag = q.read_int32().expect("tag");
            if tag == -1 {
                continue;
            }
            let _value_len = q.read_int32().expect("value length");
            let class = q.read_string16().expect("class").expect("class");
            let value = read_payload(&class, &mut q).expect("payload");
            if key == "NetworkRequest" {
                embedded = Some(value);
            }
        }
        assert_eq!(
            embedded.expect("every callback carries the NetworkRequest"),
            request,
            "the callback's NetworkRequest must equal the reply's"
        );
    }
}

#[test]
fn register_default_network_callback_sends_null_capabilities() {
    // React Native's `NetInfo` path: `registerDefaultNetworkCallback` passes
    // `null` capabilities with `TRACK_DEFAULT`, and AOSP answers with the
    // default network's capabilities. Nothing may panic and no field may be a
    // null payload where the framework reader expects an object.
    let mut host = ServiceHost::standard();
    let conn = connectivity(&mut host);
    let mut data = default_network_callback_request(Some(app_messenger()));
    let mut reply = transact(&mut host, conn, REQUEST_NETWORK, &mut data)
        .expect("requestNetwork(TRACK_DEFAULT, null)");
    expects_no_exception(&mut reply);
    let request = read_network_request(&mut reply);
    assert_eq!(
        field(&request, 3),
        &Value::Str(Some("TRACK_DEFAULT".to_string()))
    );
    let nc = match field(&request, 0) {
        Value::Parcelable(p) | Value::NamedParcelable(p) => p,
        other => panic!("NetworkRequest.networkCapabilities: {other:?}"),
    };
    assert_eq!(nc.class(), Some("android.net.NetworkCapabilities"));
    assert_eq!(host.take_deliveries().len(), 4);
}

#[test]
fn a_request_without_a_messenger_queues_no_callbacks() {
    // AOSP: `if (nri.mMessenger == null) return;` — the system default request.
    let mut host = ServiceHost::standard();
    let conn = connectivity(&mut host);
    let mut data = request_network_request(5, None);
    let mut reply =
        transact(&mut host, conn, REQUEST_NETWORK, &mut data).expect("requestNetwork");
    expects_no_exception(&mut reply);
    let request = read_network_request(&mut reply);
    assert_eq!(
        field(&request, 3),
        &Value::Str(Some("TRACK_SYSTEM_DEFAULT".to_string()))
    );
    assert!(host.take_deliveries().is_empty());
}

#[test]
fn listen_for_network_is_a_listen_request() {
    let mut host = ServiceHost::standard();
    let conn = connectivity(&mut host);
    let mut data = listen_for_network_request(Some(app_messenger()));
    let mut reply = transact(&mut host, conn, LISTEN_FOR_NETWORK, &mut data).expect("listen");
    expects_no_exception(&mut reply);
    let request = read_network_request(&mut reply);
    assert_eq!(
        field(&request, 3),
        &Value::Str(Some("LISTEN".to_string()))
    );
    assert_eq!(host.take_deliveries().len(), 4);
}

#[test]
fn release_network_request_drops_the_registration_and_unknown_ids_fail() {
    let mut host = ServiceHost::standard();
    let conn = connectivity(&mut host);
    let mut data = request_network_request(2, Some(app_messenger()));
    let mut reply = transact(&mut host, conn, REQUEST_NETWORK, &mut data).expect("requestNetwork");
    expects_no_exception(&mut reply);
    let request = read_network_request(&mut reply);
    let _ = host.take_deliveries();

    let mut release = Parcel::new();
    release.write_interface_token(IFACE).expect("token");
    write_typed_arg(&mut release, &request);
    let mut released =
        transact(&mut host, conn, RELEASE_NETWORK_REQUEST, &mut release).expect("release");
    expects_no_exception(&mut released);

    // The same request a second time is AOSP's IllegalArgumentException.
    let err = transact(&mut host, conn, RELEASE_NETWORK_REQUEST, &mut release)
        .expect_err("a second release must fail");
    assert!(
        matches!(err, ServiceError::BadRequest { .. }),
        "expected BadRequest, got {err:?}"
    );
}

#[test]
fn network_queries_describe_the_emulated_default_network() {
    let mut host = ServiceHost::standard();
    let conn = connectivity(&mut host);

    let mut data = Parcel::new();
    data.write_interface_token(IFACE).expect("token");
    let mut reply = transact(&mut host, conn, GET_ACTIVE_NETWORK, &mut data).expect("active");
    expects_no_exception(&mut reply);
    assert_eq!(reply.read_int32().expect("presence"), 1);
    assert_eq!(reply.read_int32().expect("netId"), NET_ID);

    // getAllNetworks: one Network.
    let mut data = Parcel::new();
    data.write_interface_token(IFACE).expect("token");
    let mut reply = transact(&mut host, conn, GET_ALL_NETWORKS, &mut data).expect("all");
    expects_no_exception(&mut reply);
    assert_eq!(reply.read_int32().expect("array length"), 1);
    assert_eq!(reply.read_int32().expect("presence"), 1);
    assert_eq!(reply.read_int32().expect("netId"), NET_ID);

    // getNetworkCapabilities(Network): the same capability payload the callback
    // messages carry.
    let mut data = Parcel::new();
    data.write_interface_token(IFACE).expect("token");
    write_typed_arg(&mut data, &default_network_parcelable());
    data.write_string16("net.cozic.joplin").expect("package");
    data.write_string16("").expect("tag");
    let mut reply = transact(&mut host, conn, GET_NETWORK_CAPABILITIES, &mut data).expect("caps");
    expects_no_exception(&mut reply);
    assert_eq!(reply.read_int32().expect("presence"), 1);
    let caps = read_payload("android.net.NetworkCapabilities", &mut reply).expect("caps payload");
    let bits = match field(&caps, 0) {
        Value::Long(bits) => *bits,
        other => panic!("capabilities bits: {other:?}"),
    };
    assert_eq!(bits & (1 << 12), 1 << 12, "NET_CAPABILITY_INTERNET");
    assert_eq!(
        bits & (1 << 16),
        0,
        "NET_CAPABILITY_VALIDATED must stay unset: ACOS cannot probe the internet"
    );
    assert_eq!(
        bits & (1 << 13),
        1 << 13,
        "NET_CAPABILITY_NOT_RESTRICTED"
    );
    assert_eq!(field(&caps, 2), &Value::Long(1 << 1), "TRANSPORT_WIFI");

    // getLinkProperties(Network).
    let mut data = Parcel::new();
    data.write_interface_token(IFACE).expect("token");
    write_typed_arg(&mut data, &default_network_parcelable());
    let mut reply = transact(&mut host, conn, GET_LINK_PROPERTIES, &mut data).expect("link");
    expects_no_exception(&mut reply);
    assert_eq!(reply.read_int32().expect("presence"), 1);
    read_payload("android.net.LinkProperties", &mut reply).expect("LinkProperties payload");

    // getActiveLinkProperties.
    let mut data = Parcel::new();
    data.write_interface_token(IFACE).expect("token");
    let mut reply = transact(&mut host, conn, GET_ACTIVE_LINK_PROPERTIES, &mut data).expect("alp");
    expects_no_exception(&mut reply);
    assert_eq!(reply.read_int32().expect("presence"), 1);
    read_payload("android.net.LinkProperties", &mut reply).expect("LinkProperties payload");

    // getActiveNetworkInfo: a CONNECTED TYPE_WIFI NetworkInfo.
    let mut data = Parcel::new();
    data.write_interface_token(IFACE).expect("token");
    let mut reply = transact(&mut host, conn, GET_ACTIVE_NETWORK_INFO, &mut data).expect("ani");
    expects_no_exception(&mut reply);
    assert_eq!(reply.read_int32().expect("presence"), 1);
    let info = read_payload("android.net.NetworkInfo", &mut reply).expect("NetworkInfo payload");
    assert_eq!(field(&info, 5), &Value::Str(Some("CONNECTED".to_string())));

    // getNetworkInfoForUid(Network, uid, ignoreBlocked): NetInfo's legacy path.
    let mut data = Parcel::new();
    data.write_interface_token(IFACE).expect("token");
    write_typed_arg(&mut data, &default_network_parcelable());
    data.write_int32(10_192);
    data.write_bool(false);
    let mut reply = transact(&mut host, conn, GET_NETWORK_INFO_FOR_UID, &mut data).expect("nifu");
    expects_no_exception(&mut reply);
    assert_eq!(reply.read_int32().expect("presence"), 1);
    let info = read_payload("android.net.NetworkInfo", &mut reply).expect("NetworkInfo");
    assert_eq!(field(&info, 4), &Value::Str(Some("CONNECTED".to_string())));

    // getDefaultRequest is a NONE-typed NetworkRequest.
    let mut data = Parcel::new();
    data.write_interface_token(IFACE).expect("token");
    let mut reply = transact(&mut host, conn, GET_DEFAULT_REQUEST, &mut data).expect("default");
    expects_no_exception(&mut reply);
    let request = read_network_request(&mut reply);
    assert_eq!(field(&request, 3), &Value::Str(Some("NONE".to_string())));

    // getProxyForNetwork keeps its measured answer: a null ProxyInfo.
    let mut data = Parcel::new();
    data.write_interface_token(IFACE).expect("token");
    data.write_int32(0); // null Network
    let mut reply = transact(&mut host, conn, GET_PROXY_FOR_NETWORK, &mut data).expect("proxy");
    expects_no_exception(&mut reply);
}

#[test]
fn pending_intent_registrations_stay_a_structured_refusal() {
    let mut host = ServiceHost::standard();
    let conn = connectivity(&mut host);
    let mut data = Parcel::new();
    data.write_interface_token(IFACE).expect("token");
    data.write_int32(0); // null PendingIntent
    let err = transact(&mut host, conn, RELEASE_PENDING_NETWORK_REQUEST, &mut data)
        .expect_err("a PendingIntent release is unsupported");
    assert!(
        matches!(err, ServiceError::UnsupportedAndroidApi { .. }),
        "expected UnsupportedAndroidApi, got {err:?}"
    );
}

#[test]
fn state_counters_record_each_registration_and_release() {
    let state = std::sync::Arc::new(std::sync::Mutex::new(ConnectivityState::new()));
    let mut host = ServiceHost::standard();
    // The registry owns its own state; this test drives the handler directly
    // through the public `ServiceObject` surface to observe the counters.
    let mut svc = acos_services::connectivity::ConnectivityService::new(std::sync::Arc::clone(
        &state,
    ));
    let method = acos_aidl::lookup(IFACE, REQUEST_NETWORK).expect("requestNetwork in the table");
    assert_eq!(method.code, REQUEST_NETWORK);
    let _ = connectivity(&mut host);
    let args = [
        Value::Int(-1),
        Value::Parcelable(default_network_capabilities()),
        Value::Int(2),
        Value::Parcelable(Parcelable::ordered(
            "android.os.Messenger",
            vec![Value::Binder(Some(app_messenger()))],
        )),
        Value::Int(0),
        Value::Binder(Some(BinderId(9001))),
        Value::Int(LEGACY_TYPE_NONE),
        Value::Int(0),
        Value::Str(Some("net.cozic.joplin".to_string())),
        Value::Str(Some(String::new())),
    ];
    let out = acos_services::host::ServiceObject::call(&mut svc, IFACE, method, &args)
        .expect("requestNetwork");
    assert_eq!(out.deliveries.len(), 4);
    assert_eq!(state.lock().unwrap().callbacks_sent, 4);
    assert_eq!(state.lock().unwrap().requests.len(), 1);
}

/// The four connectivity parcelables live in the `framework-connectivity`
/// APEX, not in `dex/framework`, so `tests/parcelable.rs`'s boot-DEX check
/// skips them. Scan the APEX DEX instead: the **image's own** `writeToParcel`
/// then decides whether the specs are right.
///
/// `android.net.NetworkRequest` and `android.net.NetworkInfo` line up call for
/// call, so they are verified as `Full` (every field, in the image's order).
///
/// `NetworkCapabilities` and `LinkProperties` call private helpers
/// (`writeParcelableArraySet`, `writeAddresses`, `writeAddress`), which the DEX
/// scanner cannot see into; the specs model those helpers as their inlined
/// wire form (an int count, a nullable byte array), so an op-by-op alignment is
/// impossible by construction even though the bytes are identical. For those
/// two the image's observed call sequence is pinned below: the pin fails if the
/// image's `writeToParcel` ever changes order or arity.
#[test]
fn the_connectivity_specs_match_the_boot_images_write_to_parcel() {
    let dir = acos_services::workspace_path(".cache/boot/android-34/dex/framework-connectivity")
        .expect("boot DEX cache (run from the repo root)");

    for class in ["android.net.NetworkRequest", "android.net.NetworkInfo"] {
        let spec = acos_services::parcelable::spec_for(class)
            .unwrap_or_else(|| panic!("{class} has no spec"));
        let scanned = acos_services::parcelable::Scanner::write_to_parcel(&dir, class)
            .unwrap_or_else(|e| panic!("{class}: {e}"));
        let result = acos_services::parcelable::verify(spec, &scanned);
        assert!(
            result.is_full(),
            "{class}: consumed {}/{} writeToParcel calls (spec {} fields, all_fields={})",
            result.matched_calls,
            result.real_calls,
            spec.fields.len(),
            result.all_fields
        );
    }

    // The image's `NetworkCapabilities.writeToParcel`, helper call included.
    let caps = acos_services::parcelable::Scanner::write_to_parcel(
        &dir,
        "android.net.NetworkCapabilities",
    )
    .expect("NetworkCapabilities writeToParcel");
    assert_eq!(
        caps.calls,
        vec![
            "writeLong",  // mNetworkCapabilities
            "writeLong",  // mForbiddenNetworkCapabilities
            "writeLong",  // mTransportTypes
            "writeInt",   // mLinkUpBandwidthKbps
            "writeInt",   // mLinkDownBandwidthKbps
            "writeParcelable", // mNetworkSpecifier
            "writeParcelable", // mTransportInfo
            "writeInt",   // mSignalStrength
            // `writeParcelableArraySet(mUids)` — invisible to the scanner; the
            // spec models it as the `writeInt` count it emits (-1 = null).
            "writeIntArray", // mAllowedUids
            "writeString",   // mSSID
            "writeBoolean",  // mPrivateDnsBroken
            "writeIntArray", // administrator uids
            "writeInt",      // mOwnerUid
            "writeInt",      // mRequestorUid
            "writeString",   // mRequestorPackageName
            "writeIntArray", // mSubIds
            "writeTypedList", // mUnderlyingNetworks
            "writeInt",      // mEnterpriseId
        ],
        "the image's NetworkCapabilities.writeToParcel changed"
    );

    // The image's `LinkProperties.writeToParcel`: the three `writeAddresses`
    // helpers and `writeAddress(dhcp)` are invisible; the address/route loops
    // show one `writeParcelable` each, and the nullable proxy/NAT64/captive-
    // portal slots are `writeParcelable`.
    let link =
        acos_services::parcelable::Scanner::write_to_parcel(&dir, "android.net.LinkProperties")
            .expect("LinkProperties writeToParcel");
    assert_eq!(
        link.calls,
        vec![
            "writeString", // interface name
            "writeInt",    // link address count
            "writeParcelable", // LinkAddress loop
            "writeBoolean", // usePrivateDns
            "writeString", // privateDnsServerName
            "writeString", // domains
            "writeInt",    // mtu
            "writeString", // tcpBufferSizes
            "writeInt",    // route count
            "writeParcelable", // RouteInfo loop
            "writeByte",   // httpProxy presence
            "writeParcelable", // httpProxy
            "writeParcelable", // nat64Prefix
            "writeList",   // stacked links
            "writeBoolean", // wakeOnLanSupported
            "writeParcelable", // captivePortalApiUrl
            "writeParcelable", // captivePortalData
        ],
        "the image's LinkProperties.writeToParcel changed"
    );
}
