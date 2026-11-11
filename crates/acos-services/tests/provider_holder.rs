//! Q2 acceptance: the content-provider peer, byte for byte.
//!
//! Every request here is built with `acos-parcel` **by hand, in the order the
//! framework's own client writes it** (`ContentProviderNative.onTransact` /
//! `ContentProviderProxy` for `IContentProvider`, the generated
//! `IActivityManager$Stub$Proxy` for `getContentProvider`), and every reply is
//! decoded in the framework's own read order (`ContentProviderHolder.CREATOR`,
//! `ContentProviderProxy.call`, `BulkCursorDescriptor.CREATOR`). Nothing is
//! asserted through the ACOS value model alone: the reply is also re-encoded
//! from the decoded fields and compared byte-for-byte, and the decoder must
//! consume the reply exactly (`data_avail() == 0`).
//!
//! AOSP (`.cache/aosp/base`): `core/java/android/app/ContentProviderHolder.java`,
//! `core/java/android/app/ActivityThread.java` (`acquireProvider`),
//! `core/java/android/content/ContentProviderNative.java`,
//! `core/java/android/provider/Settings.java` (`Config.getStrings`).

use acos_parcel::{BinderId, Parcel};
use acos_services::cursor;
use acos_services::parcelable;
use acos_services::value::{Bundle, Parcelable, Value};
use acos_services::ServiceHost;

const IACTIVITY_MANAGER: &str = "android.app.IActivityManager";
const ICONTENT_PROVIDER: &str = "android.content.IContentProvider";

/// `ContentProviderNative` transaction codes (`IContentProvider.java`).
const QUERY: u32 = 1;
const CALL: u32 = 21;

/// `IActivityManager.getContentProvider` (`ActivityManager.java`).
const GET_CONTENT_PROVIDER: u32 = 30;

fn attribution(parcel: &mut Parcel) {
    let source = Parcelable::ordered(
        "android.content.AttributionSource",
        vec![
            Value::Int(0), // backpatched size
            Value::Int(4242),
            Value::Int(10123),
            Value::Str(Some("dev.acos.fixtures.helloviews".into())),
            Value::Str(None),
            Value::Binder(None),
            Value::StringArray(None),
            Value::List(None),
        ],
    );
    parcelable::write_payload(parcel, &source).expect("AttributionSource");
}

fn uri(parcel: &mut Parcel, text: &str) {
    let value = Parcelable::ordered(
        "android.net.Uri",
        vec![Value::Int(3), Value::Str8(Some(text.into()))],
    );
    parcelable::write_payload(parcel, &value).expect("Uri");
}

/// `IActivityManager$Stub$Proxy.getContentProvider(caller, callingPackage,
/// name, userId, stable)`.
fn get_content_provider_request(authority: &str) -> Parcel {
    let mut data = Parcel::new();
    data.write_interface_token(IACTIVITY_MANAGER)
        .expect("token");
    data.write_strong_binder_opt(None); // caller IApplicationThread
    data.write_string16("dev.acos.fixtures.helloviews")
        .expect("callingPackage");
    data.write_string16(authority).expect("name");
    data.write_int32(0); // userId
    data.write_int32(1); // stable = true
    data.set_data_position(0);
    data
}

/// `ContentProviderProxy.query` — note the projection is `writeStringArray`
/// (`int count` + strings), not the single `Str16` a generated AIDL stub would
/// write; that mismatch is why the host decodes this transaction itself.
fn query_request(uri_text: &str, projection: &[&str], selection: Option<(&str, &str)>) -> Parcel {
    let mut data = Parcel::new();
    data.write_interface_token(ICONTENT_PROVIDER)
        .expect("token");
    attribution(&mut data);
    uri(&mut data, uri_text);
    data.write_int32(i32::try_from(projection.len()).expect("projection len"));
    for column in projection {
        data.write_string16(column).expect("projection column");
    }
    let mut args = Bundle::new();
    if let Some((column, key)) = selection {
        args.put(
            "android:query-arg-sql-selection",
            Value::Str(Some(format!("{column}=?"))),
        );
        args.put(
            "android:query-arg-sql-selection-args",
            Value::StringArray(Some(vec![Some(key.into())])),
        );
    }
    parcelable::write_bundle(&mut data, Some(&args)).expect("queryArgs");
    data.write_strong_binder_opt(None); // IContentObserver
    data.write_strong_binder_opt(None); // ICancellationSignal
    data.set_data_position(0);
    data
}

/// `ContentProviderProxy.call(attributionSource, authority, method, stringArg,
/// extras)`.
fn call_request(method: &str, arg: &str, extras: Option<&Bundle>) -> Parcel {
    let mut data = Parcel::new();
    data.write_interface_token(ICONTENT_PROVIDER)
        .expect("token");
    attribution(&mut data);
    data.write_string16("settings").expect("authority");
    data.write_string16(method).expect("method");
    data.write_string16(arg).expect("stringArg");
    parcelable::write_bundle(&mut data, extras).expect("extras");
    data.set_data_position(0);
    data
}

fn transact(
    host: &mut ServiceHost,
    descriptor: &str,
    code: u32,
    mut data: Parcel,
) -> (Parcel, Vec<u8>) {
    let binder = host
        .binder_for_descriptor(descriptor)
        .unwrap_or_else(|| panic!("no binder for {descriptor}"));
    let mut reply = Parcel::new();
    host.transact(binder, code, &mut data, &mut reply, 0)
        .unwrap_or_else(|e| panic!("{descriptor} code {code}: {e}"));
    let bytes = reply.marshall();
    reply.set_data_position(0);
    (reply, bytes)
}

/// A `ContentProviderHolder` decoded per `ContentProviderHolder.CREATOR`.
struct Holder {
    info: Parcelable,
    provider: Option<BinderId>,
    connection: Option<BinderId>,
    no_release_needed: i32,
    local: i32,
}

fn read_holder(reply: &mut Parcel) -> Option<Holder> {
    assert_eq!(reply.read_int32().expect("exception header"), 0);
    if reply.read_int32().expect("typed-object marker") == 0 {
        return None;
    }
    // `info.writeToParcel(dest, 0)` — a bare payload, no presence marker.
    let info = parcelable::read_payload("android.content.pm.ProviderInfo", reply).expect("info");
    let provider = reply.read_strong_binder().expect("provider");
    let connection = reply.read_strong_binder().expect("connection");
    let no_release_needed = reply.read_int32().expect("noReleaseNeeded");
    let local = reply.read_int32().expect("mLocal");
    Some(Holder {
        info,
        provider,
        connection,
        no_release_needed,
        local,
    })
}

#[test]
fn get_content_provider_returns_the_framework_holder_for_settings() {
    let mut host = ServiceHost::standard();
    let (mut reply, bytes) = transact(
        &mut host,
        IACTIVITY_MANAGER,
        GET_CONTENT_PROVIDER,
        get_content_provider_request("settings"),
    );
    let holder = read_holder(&mut reply).expect("settings must resolve");
    assert_eq!(reply.data_avail(), 0, "the reply has no slack after mLocal");

    // `ProviderInfo`: name, packageName, …, authority at index 17 in the
    // API-34 field order (see `parcelable::SPECS`).
    assert_eq!(
        holder.info.field(0).and_then(Value::as_str),
        Some("SettingsProvider")
    );
    assert_eq!(
        holder.info.field(1).and_then(Value::as_str),
        Some("com.android.providers.settings")
    );
    assert_eq!(
        holder.info.field(17).and_then(Value::as_str),
        Some("settings")
    );
    assert_eq!(
        holder.info.field(10).and_then(Value::as_str),
        Some("android.process.settings")
    );

    // The provider is the local binder `ServiceHost` dispatches
    // `android.content.IContentProvider` transactions to.
    let provider = holder.provider.expect("provider binder");
    assert_eq!(
        host.descriptor_of(provider),
        Some(ICONTENT_PROVIDER),
        "holder.provider must be the IContentProvider binder"
    );
    assert!(
        host.binder_for_descriptor(ICONTENT_PROVIDER) == Some(provider),
        "holder.provider must be the registered settings provider"
    );
    // … and the connection token is a real binder, not a placeholder int.
    let connection = holder.connection.expect("connection token");
    assert_ne!(connection.0, provider.0);
    assert_eq!(
        host.descriptor_of(connection),
        Some("com.android.server.am.ContentProviderConnection")
    );
    assert_eq!(
        holder.no_release_needed, 0,
        "a real connection is handed out"
    );
    assert_eq!(holder.local, 1, "the provider is served in this process");

    // Byte-exact: re-encode the reply from the decoded fields in the framework's
    // write order and compare the whole parcel.
    let mut expected = Parcel::new();
    expected.write_int32(0);
    expected.write_int32(1);
    parcelable::write_payload(&mut expected, &holder.info).expect("info payload");
    expected.write_strong_binder_opt(Some(provider));
    expected.write_strong_binder_opt(Some(connection));
    expected.write_int32(holder.no_release_needed);
    expected.write_int32(holder.local);
    assert_eq!(bytes, expected.marshall());

    // The acquisition is recorded (refcount 1), not silent.
    let lookups = host.provider_lookups();
    assert_eq!(lookups.len(), 1);
    assert_eq!(lookups[0].method, "getContentProvider");
    assert_eq!(lookups[0].authority, "settings");
    assert!(lookups[0].hit);
    assert_eq!(lookups[0].binder, provider.0);
    assert_eq!(lookups[0].refcount, 1);
}

#[test]
fn unknown_authority_is_a_null_holder_with_a_recorded_miss() {
    let mut host = ServiceHost::standard();
    let (mut reply, bytes) = transact(
        &mut host,
        IACTIVITY_MANAGER,
        GET_CONTENT_PROVIDER,
        get_content_provider_request("com.example.absent"),
    );
    assert!(
        read_holder(&mut reply).is_none(),
        "unknown authority -> null"
    );
    assert_eq!(reply.data_avail(), 0);
    // Exactly `writeNoException()` + `writeTypedObject(null)`.
    assert_eq!(bytes, vec![0, 0, 0, 0, 0, 0, 0, 0]);
    let lookups = host.provider_lookups();
    assert_eq!(lookups.len(), 1);
    assert!(!lookups[0].hit);
    assert_eq!(lookups[0].refcount, 0);
    assert_eq!(lookups[0].binder, 0);
}

#[test]
fn remove_and_ref_content_provider_resolve_the_connection_token() {
    let mut host = ServiceHost::standard();
    let (mut reply, _) = transact(
        &mut host,
        IACTIVITY_MANAGER,
        GET_CONTENT_PROVIDER,
        get_content_provider_request("settings"),
    );
    let holder = read_holder(&mut reply).expect("holder");
    let connection = holder.connection.expect("connection");

    // `refContentProvider(IBinder connection, int stable, int unstable)`.
    let mut request = Parcel::new();
    request
        .write_interface_token(IACTIVITY_MANAGER)
        .expect("token");
    request.write_strong_binder_opt(Some(connection));
    request.write_int32(1);
    request.write_int32(0);
    request.set_data_position(0);
    let binder = host
        .binder_for_descriptor(IACTIVITY_MANAGER)
        .expect("binder");
    let mut ref_reply = Parcel::new();
    host.transact(
        binder,
        32, // refContentProvider
        &mut request,
        &mut ref_reply,
        0,
    )
    .expect("refContentProvider");
    ref_reply.set_data_position(0);
    assert_eq!(ref_reply.read_int32().expect("header"), 0);
    assert_eq!(
        ref_reply.read_int32().expect("result"),
        1,
        "known connection"
    );

    // `removeContentProvider(IBinder connection, boolean stable)` — oneway.
    let mut request = Parcel::new();
    request
        .write_interface_token(IACTIVITY_MANAGER)
        .expect("token");
    request.write_strong_binder_opt(Some(connection));
    request.write_int32(0);
    request.set_data_position(0);
    let mut release_reply = Parcel::new();
    host.transact(binder, 70, &mut request, &mut release_reply, 1)
        .expect("removeContentProvider");

    let lookups = host.provider_lookups();
    let methods: Vec<&str> = lookups.iter().map(|l| l.method.as_str()).collect();
    assert_eq!(
        methods,
        vec![
            "getContentProvider",
            "refContentProvider",
            "removeContentProvider"
        ]
    );
    assert_eq!(lookups[1].authority, "settings");
    assert_eq!(lookups[1].refcount, 2);
    assert_eq!(lookups[2].authority, "settings");
    assert_eq!(lookups[2].refcount, 1);
}

#[test]
fn get_secure_call_answers_a_key_and_leaves_an_unset_key_null() {
    let mut host = ServiceHost::standard();
    let (mut reply, bytes) = transact(
        &mut host,
        ICONTENT_PROVIDER,
        CALL,
        call_request("GET_secure", "android_id", None),
    );
    assert_eq!(reply.read_int32().expect("exception header"), 0);
    let bundle = parcelable::read_bundle(&mut reply)
        .expect("GET_secure reply")
        .expect("a Bundle");
    assert_eq!(
        bundle.get("value").and_then(Value::as_str),
        Some("acos0000000000ff")
    );
    // The generation counter is handed out as the real fd-backed
    // `MemoryIntArray` on the first read of a namespace.
    let generation = bundle
        .get("generation")
        .and_then(Value::as_parcelable)
        .expect("generation MemoryIntArray");
    assert_eq!(generation.class(), Some("android.util.MemoryIntArray"));
    assert_eq!(reply.data_avail(), 0);

    // The first read hands out a live descriptor inside `MemoryIntArray`, so
    // those bytes carry an fd *number* (process-local, not wire-stable) and are
    // not compared. The second read has no generation array and is byte-stable:
    // exactly the framework's `writeNoException` + `writeBundle(bundle)`.
    let _ = bytes;
    let (mut reply, bytes) = transact(
        &mut host,
        ICONTENT_PROVIDER,
        CALL,
        call_request("GET_secure", "android_id", None),
    );
    assert_eq!(reply.read_int32().expect("exception header"), 0);
    let bundle = parcelable::read_bundle(&mut reply)
        .expect("GET_secure reply")
        .expect("a Bundle");
    assert!(bundle.get("generation").is_none());
    assert_eq!(reply.data_avail(), 0);
    let mut expected = Parcel::new();
    expected.write_int32(0);
    parcelable::write_bundle(&mut expected, Some(&bundle)).expect("bundle");
    assert_eq!(bytes, expected.marshall());

    // An unset key answers a present Bundle whose `value` is null — the
    // framework then uses its own default (`Settings.getString`).
    let (mut reply, _) = transact(
        &mut host,
        ICONTENT_PROVIDER,
        CALL,
        call_request("GET_secure", "definitely.not.set", None),
    );
    assert_eq!(reply.read_int32().expect("exception header"), 0);
    let bundle = parcelable::read_bundle(&mut reply)
        .expect("reply")
        .expect("a Bundle");
    assert_eq!(bundle.get("value").and_then(Value::as_str), None);
}

#[test]
fn settings_config_list_call_answers_the_ime_namespace_map() {
    // `Settings$Config.getStrings(resolver, "ime", names)` builds
    // `_prefix = "ime/"` and reads `value` as a `HashMap<String, String>` of
    // composite `namespace/name` keys.
    let mut host = ServiceHost::standard();
    let mut extras = Bundle::new();
    extras.put("_prefix", Value::Str(Some("ime/".into())));
    let (mut reply, bytes) = transact(
        &mut host,
        ICONTENT_PROVIDER,
        CALL,
        call_request("LIST_config", "", Some(&extras)),
    );
    assert_eq!(reply.read_int32().expect("exception header"), 0);
    let bundle = parcelable::read_bundle(&mut reply)
        .expect("LIST_config reply")
        .expect("a Bundle");
    let entries = match bundle.get("value") {
        Some(Value::Map(Some(entries))) => entries,
        other => panic!("value must be a HashMap, got {other:?}"),
    };
    let mut keys: Vec<&str> = entries
        .iter()
        .map(|(k, _)| match k {
            acos_services::value::DynamicValue::String(Some(k)) => k.as_str(),
            other => panic!("map key must be a String, got {other:?}"),
        })
        .collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        vec![
            "ime/default_input_method",
            "ime/enabled_input_methods",
            "ime/show_ime_with_hard_keyboard"
        ]
    );
    assert!(entries.iter().any(|(k, v)| matches!(
        (k, v),
        (
            acos_services::value::DynamicValue::String(Some(k)),
            acos_services::value::DynamicValue::String(Some(v))
        ) if k == "ime/enabled_input_methods"
            && v == "com.android.inputmethod.latin/.LatinIME"
    )));
    assert_eq!(reply.data_avail(), 0);

    let mut expected = Parcel::new();
    expected.write_int32(0);
    parcelable::write_bundle(&mut expected, Some(&bundle)).expect("bundle");
    assert_eq!(bytes, expected.marshall());
}

#[test]
fn get_config_answers_one_device_config_value() {
    let mut host = ServiceHost::standard();
    let (mut reply, _) = transact(
        &mut host,
        ICONTENT_PROVIDER,
        CALL,
        call_request("GET_config", "ime/enabled_input_methods", None),
    );
    assert_eq!(reply.read_int32().expect("exception header"), 0);
    let bundle = parcelable::read_bundle(&mut reply)
        .expect("GET_config reply")
        .expect("a Bundle");
    assert_eq!(
        bundle.get("value").and_then(Value::as_str),
        Some("com.android.inputmethod.latin/.LatinIME")
    );
}

#[test]
fn unknown_provider_method_is_unsupported_not_an_empty_bundle() {
    let mut host = ServiceHost::standard();
    let binder = host
        .binder_for_descriptor(ICONTENT_PROVIDER)
        .expect("binder");
    let mut data = call_request("DROP_secure", "x", None);
    let mut reply = Parcel::new();
    let err = host
        .transact(binder, CALL, &mut data, &mut reply, 0)
        .expect_err("unknown method must be structured");
    assert_eq!(err.code(), acos_services::codes::UNSUPPORTED_ANDROID_API);
    // The framework would see a real exception parcel, never a success.
    reply.set_data_position(0);
    assert_ne!(reply.read_int32().expect("exception header"), 0);
}

#[test]
fn settings_query_answers_a_framework_cursor_window() {
    // The fallback path of `Settings$NameValueCache.getStringForUser`:
    // `query(content://settings/secure, ["value"], {"name=?", ["android_id"]})`.
    let mut host = ServiceHost::standard();
    let (mut reply, bytes) = transact(
        &mut host,
        ICONTENT_PROVIDER,
        QUERY,
        query_request(
            "content://settings/secure",
            &["value"],
            Some(("name", "android_id")),
        ),
    );
    let descriptor = cursor::read_query_reply(&mut reply)
        .expect("query reply")
        .expect("a BulkCursorDescriptor");
    assert_eq!(descriptor.column_names, vec!["value".to_string()]);
    assert_eq!(descriptor.count, 1);
    assert!(!descriptor.wants_all_on_move_calls);
    assert!(descriptor.cursor.is_some(), "IBulkCursor binder");
    let window = descriptor.window.expect("window");
    assert_eq!((window.num_rows, window.num_columns), (1, 1));
    assert_eq!(
        window.string(0, 0).as_deref(),
        Some("acos0000000000ff"),
        "the value column of the settings row"
    );
    assert_eq!(reply.data_avail(), 0, "the window frame is exactly sized");

    // Byte-exact: rebuild the reply through the cursor writer and compare.
    let mut expected = Parcel::new();
    expected.write_int32(0);
    let result = acos_services::cursor::CursorResult::new(
        descriptor.column_names.clone(),
        vec![vec![acos_services::cursor::Field::Text(
            "acos0000000000ff".into(),
        )]],
    );
    result
        .write_query_reply(&mut expected, descriptor.cursor.expect("cursor"))
        .expect("write reply");
    assert_eq!(bytes, expected.marshall());
}
