//! W52-A: the runtime-side AMS bridge reads real `android.content.Intent`
//! objects and re-materialises them without ever hand-writing a parcelable's
//! bytes.
//!
//! The test builds a real `Intent` on the real Android 14 core library,
//! marshals it with the framework's own `Intent.writeToParcel`, reduces the
//! bytes with `Intent.CREATOR` + the real accessors (`getAction`,
//! `getComponent`, `getCategories`, `getExtras`), then re-materialises the
//! typed facts into a fresh `Intent` and proves the reduction round-trips.
//!
//! When the boot-classpath cache is absent the test skips loudly.

mod common;

use acos_parcel::Parcel;
use acos_runtime::ams_java;
use acos_services::ams::Extra;
use acos_vm::heap::{ArrayData, Handle, Object};
use acos_vm::javabridge::JavaValue;
use acos_vm::Vm;

fn as_obj(v: JavaValue) -> Handle {
    match v {
        JavaValue::Object(h) => h,
        other => panic!("expected object, got {}", other.kind()),
    }
}

fn byte_array(vm: &Vm, h: Handle) -> Vec<u8> {
    match vm.heap.get(h) {
        Some(Object::Array(a)) => match &a.data {
            ArrayData::Byte(v) => v.iter().map(|&b| b as u8).collect(),
            other => panic!("expected byte array, got {other:?}"),
        },
        _ => panic!("expected an array"),
    }
}

/// `intent.writeToParcel(new Parcel())`, marshalled to bytes.
fn intent_bytes(vm: &mut Vm, intent: Handle) -> Vec<u8> {
    let parcel = as_obj(
        vm.call_static("android.os.Parcel", "obtain", "()Landroid/os/Parcel;", &[])
            .expect("Parcel.obtain"),
    );
    vm.call_method(
        intent,
        "writeToParcel",
        "(Landroid/os/Parcel;I)V",
        &[JavaValue::Object(parcel), JavaValue::Int(0)],
    )
    .expect("writeToParcel");
    let bytes = as_obj(
        vm.call_method(parcel, "marshall", "()[B", &[])
            .expect("marshall"),
    );
    byte_array(vm, bytes)
}

/// Wrap marshalled `Intent` bytes in the AIDL `writeTypedObject` presence
/// marker and read them back through the real `CREATOR`.
fn facts_from_bytes(vm: &mut Vm, bytes: &[u8]) -> acos_services::ams::IntentFacts {
    let mut data = Parcel::new();
    data.write_int32(1);
    let mut tmp = Parcel::new();
    tmp.unmarshall(bytes);
    let len = tmp.data_size();
    data.append_from(&tmp, 0, len).expect("append intent payload");
    data.set_data_position(0);
    ams_java::read_intent_facts(vm, &mut data)
        .expect("read intent facts")
        .expect("present intent")
}

#[test]
fn intent_facts_round_trip_through_the_real_creators() {
    let Some(mut rt) = common::new_runtime() else {
        return;
    };
    let vm = rt.vm_mut();

    let intent = vm
        .new_instance_ctor("android.content.Intent", "()V", &[])
        .expect("new Intent");
    let package = vm
        .new_string_rooted("dev.acos.fixtures.twoactivity")
        .expect("string");
    let class = vm
        .new_string_rooted("dev.acos.fixtures.twoactivity.DetailActivity")
        .expect("string");
    vm.call_method(
        intent,
        "setClassName",
        "(Ljava/lang/String;Ljava/lang/String;)Landroid/content/Intent;",
        &[JavaValue::Object(package), JavaValue::Object(class)],
    )
    .expect("setClassName");
    let key = vm.new_string_rooted("name").expect("string");
    let value = vm.new_string_rooted("Ada").expect("string");
    vm.call_method(
        intent,
        "putExtra",
        "(Ljava/lang/String;Ljava/lang/String;)Landroid/content/Intent;",
        &[JavaValue::Object(key), JavaValue::Object(value)],
    )
    .expect("putExtra String");
    let attempt = vm.new_string_rooted("attempt").expect("string");
    vm.call_method(
        intent,
        "putExtra",
        "(Ljava/lang/String;I)Landroid/content/Intent;",
        &[JavaValue::Object(attempt), JavaValue::Int(1)],
    )
    .expect("putExtra int");
    let flag = vm.new_string_rooted("flag").expect("string");
    vm.call_method(
        intent,
        "putExtra",
        "(Ljava/lang/String;Z)Landroid/content/Intent;",
        &[JavaValue::Object(flag), JavaValue::Boolean(true)],
    )
    .expect("putExtra bool");
    let category = vm
        .new_string_rooted("android.intent.category.LAUNCHER")
        .expect("string");
    vm.call_method(
        intent,
        "addCategory",
        "(Ljava/lang/String;)Landroid/content/Intent;",
        &[JavaValue::Object(category)],
    )
    .expect("addCategory");

    let bytes = intent_bytes(vm, intent);
    let facts = facts_from_bytes(vm, &bytes);
    assert_eq!(
        facts.component.as_ref().map(|(p, c)| (p.as_str(), c.as_str())),
        Some((
            "dev.acos.fixtures.twoactivity",
            "dev.acos.fixtures.twoactivity.DetailActivity"
        ))
    );
    assert_eq!(facts.extras.get("name"), Some(&Extra::String("Ada".into())));
    assert_eq!(facts.extras.get("attempt"), Some(&Extra::Int(1)));
    assert_eq!(facts.extras.get("flag"), Some(&Extra::Bool(true)));
    assert!(facts
        .categories
        .contains(&"android.intent.category.LAUNCHER".to_string()));

    // Re-materialise and re-read: the typed facts are lossless for the
    // String/int/boolean subset.
    let rebuilt = ams_java::materialize_intent(vm, &facts).expect("materialize Intent");
    let rebuilt_bytes = intent_bytes(vm, rebuilt);
    let facts2 = facts_from_bytes(vm, &rebuilt_bytes);
    assert_eq!(facts, facts2);
}

#[test]
fn component_name_reply_is_a_real_component_name() {
    let Some(mut rt) = common::new_runtime() else {
        return;
    };
    let vm = rt.vm_mut();
    let cn = ams_java::component_name(vm, "dev.acos.fixtures.svcprovider", "dev.acos.fixtures.svcprovider.EchoService")
        .expect("component name");
    let package = match vm
        .call_method(cn, "getPackageName", "()Ljava/lang/String;", &[])
        .expect("getPackageName")
    {
        JavaValue::Object(h) => vm.java_string_to_rust(h).expect("string"),
        other => panic!("getPackageName returned {}", other.kind()),
    };
    let class = match vm
        .call_method(cn, "getClassName", "()Ljava/lang/String;", &[])
        .expect("getClassName")
    {
        JavaValue::Object(h) => vm.java_string_to_rust(h).expect("string"),
        other => panic!("getClassName returned {}", other.kind()),
    };
    assert_eq!(package, "dev.acos.fixtures.svcprovider");
    assert_eq!(class, "dev.acos.fixtures.svcprovider.EchoService");
}

#[test]
fn unsupported_extra_type_is_a_structured_error() {
    let Some(mut rt) = common::new_runtime() else {
        return;
    };
    let vm = rt.vm_mut();
    let intent = vm
        .new_instance_ctor("android.content.Intent", "()V", &[])
        .expect("new Intent");
    // A `long` extra is outside the AMS typed subset: it must be refused, not
    // silently recorded as present.
    let key = vm.new_string_rooted("big").expect("string");
    vm.call_method(
        intent,
        "putExtra",
        "(Ljava/lang/String;J)Landroid/content/Intent;",
        &[JavaValue::Object(key), JavaValue::Long(7)],
    )
    .expect("putExtra long");
    let bytes = intent_bytes(vm, intent);
    let mut data = Parcel::new();
    data.write_int32(1);
    let mut tmp = Parcel::new();
    tmp.unmarshall(&bytes);
    let len = tmp.data_size();
    data.append_from(&tmp, 0, len).expect("append");
    data.set_data_position(0);
    let err = ams_java::read_intent_facts(vm, &mut data).expect_err("long extra must be refused");
    assert!(err.contains("unsupported type"), "{err}");
}

/// Build the AIDL request for `getContentProvider(IApplicationThread,
/// String callingPackage, String name, int userId, boolean stable)`.
fn content_provider_request(calling_package: &str, authority: &str) -> Parcel {
    let mut data = Parcel::new();
    data.write_interface_token("android.app.IActivityManager")
        .expect("token");
    data.write_strong_binder_opt(None);
    data.write_string16(calling_package).expect("callingPackage");
    data.write_string16(authority).expect("authority");
    data.write_int32(0);
    data.write_int32(0);
    data.set_data_position(0);
    data
}

/// Regression (W51-A): `getContentProvider` must read the authority from the
/// *second* string argument (the first is `callingPackage`), and the reply
/// holder must carry a real `IContentProvider` so `Settings` can call it.
#[test]
fn get_content_provider_reads_the_authority_and_carries_the_provider() {
    let Some(mut rt) = common::new_runtime() else {
        return;
    };
    let vm = rt.vm_mut();
    let mut data = content_provider_request("dev.acos.fixtures.counter", "settings");
    let mut reply = Parcel::new();
    ams_java::transact(
        vm,
        acos_parcel::binder::BinderId(0),
        30,
        &mut data,
        &mut reply,
        0,
    )
    .expect("getContentProvider transact");
    reply.set_data_position(0);
    assert_eq!(reply.read_int32().expect("read status"), 0, "reply must not carry an exception");
    let holder = acos_runtime::parcel_java::read_typed_object(
        vm,
        &mut reply,
        "android.app.ContentProviderHolder",
    )
    .expect("read ContentProviderHolder")
    .expect("holder must be present for `settings`");
    let provider = vm.get_field(holder, "provider").expect("provider field");
    assert!(
        matches!(provider, JavaValue::Object(h) if h != acos_vm::heap::NULL),
        "holder.provider must be a real IContentProvider, got {}",
        provider.kind()
    );
}

/// Regression (W51-A): the authority is arg 2, so a calling package of
/// `settings` with a different authority is a genuine miss (null holder).
#[test]
fn get_content_provider_does_not_treat_the_calling_package_as_the_authority() {
    let Some(mut rt) = common::new_runtime() else {
        return;
    };
    let vm = rt.vm_mut();
    let mut data = content_provider_request("settings", "not.settings");
    let mut reply = Parcel::new();
    ams_java::transact(
        vm,
        acos_parcel::binder::BinderId(0),
        30,
        &mut data,
        &mut reply,
        0,
    )
    .expect("getContentProvider transact");
    reply.set_data_position(0);
    assert_eq!(reply.read_int32().expect("read status"), 0, "reply must not carry an exception");
    let holder = acos_runtime::parcel_java::read_typed_object(
        vm,
        &mut reply,
        "android.app.ContentProviderHolder",
    )
    .expect("read ContentProviderHolder");
    assert!(holder.is_none(), "unknown authority must yield a null holder");
}

// ---------------------------------------------------------------------------
// Q2b: the content-provider peer, answered through the `ams_java` path
// ---------------------------------------------------------------------------
//
// `ams_java` no longer owns a provider record: `getContentProvider` and
// `IContentProvider.call`/`query` are answered by the `acos-services`
// `ProviderRegistry` behind the runtime `ServiceHost` — the same implementation
// the ART binder-peer route dispatches to. The requests below are built in the
// order `ContentProviderProxy`/`ContentProviderNative` write them (the wire
// layouts Q2 §3 pins down), and the replies are decoded in the framework's own
// read order.

use acos_parcel::BinderId;
use acos_services::cursor;
use acos_services::parcelable;
use acos_services::value::{Bundle, Parcelable, Value};

const ICONTENT_PROVIDER: &str = "android.content.IContentProvider";
/// `ContentProviderNative` transaction codes (`IContentProvider.java`).
const QUERY: u32 = 1;
const CALL: u32 = 21;
/// `ContentProviderHolder.mLocal` / `noReleaseNeeded` as the registry sets them.
const CONNECTION_DESCRIPTOR: &str = "com.android.server.am.ContentProviderConnection";

/// `AttributionSource.writeToParcel` payload.
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

/// `Uri.writeToParcel` payload (`StringUri` = 3, then `writeString8`).
fn uri_payload(parcel: &mut Parcel, text: &str) {
    let value = Parcelable::ordered(
        "android.net.Uri",
        vec![Value::Int(3), Value::Str8(Some(text.into()))],
    );
    parcelable::write_payload(parcel, &value).expect("Uri");
}

/// `ContentProviderProxy.call(attributionSource, authority, method, arg, extras)`.
fn call_request(method: &str, arg: &str, extras: Option<&Bundle>) -> Parcel {
    let mut data = Parcel::new();
    data.write_interface_token(ICONTENT_PROVIDER).expect("token");
    attribution(&mut data);
    data.write_string16("settings").expect("authority");
    data.write_string16(method).expect("method");
    data.write_string16(arg).expect("stringArg");
    parcelable::write_bundle(&mut data, extras).expect("extras");
    data.set_data_position(0);
    data
}

/// `ContentProviderProxy.query(attributionSource, uri, projection, queryArgs,
/// cancellationSignal)`.
fn query_request(uri_text: &str, projection: &[&str], selection: Option<(&str, &str)>) -> Parcel {
    let mut data = Parcel::new();
    data.write_interface_token(ICONTENT_PROVIDER).expect("token");
    attribution(&mut data);
    uri_payload(&mut data, uri_text);
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

/// The `settings` `IContentProvider` binder the host registered.
fn settings_binder(rt: &acos_runtime::Runtime) -> BinderId {
    rt.host()
        .borrow()
        .binder_for_descriptor(ICONTENT_PROVIDER)
        .expect("the host serves android.content.IContentProvider")
}

/// Dispatch one `IContentProvider` transaction through `ams_java`.
fn provider_transact_via_ams_java(
    rt: &mut acos_runtime::Runtime,
    binder: BinderId,
    code: u32,
    mut data: Parcel,
) -> (Parcel, Vec<u8>) {
    let mut reply = Parcel::new();
    ams_java::transact(rt.vm_mut(), binder, code, &mut data, &mut reply, 0)
        .unwrap_or_else(|e| panic!("IContentProvider code {code} through ams_java: {e}"));
    let bytes = reply.marshall();
    reply.set_data_position(0);
    (reply, bytes)
}

/// The same transaction through the `ServiceHost` — what `acos-binder-peer`
/// (the ART route) and `ServiceTransport` dispatch to.
fn provider_transact_via_host(
    rt: &acos_runtime::Runtime,
    binder: BinderId,
    code: u32,
    mut data: Parcel,
) -> (Parcel, Vec<u8>) {
    let mut reply = Parcel::new();
    rt.host()
        .borrow_mut()
        .transact(binder, code, &mut data, &mut reply, 0)
        .unwrap_or_else(|e| panic!("IContentProvider code {code} through ServiceHost: {e}"));
    let bytes = reply.marshall();
    reply.set_data_position(0);
    (reply, bytes)
}

/// Q2b: the `getContentProvider` holder is the registry's, not a hardcoded
/// record — same `IContentProvider` binder the transport dispatches to, a real
/// `ContentProviderConnection` token, `noReleaseNeeded=0`, `mLocal=1`, and the
/// registry's `ProviderInfo`.
#[test]
fn content_provider_holder_comes_from_the_services_registry() {
    let Some(mut rt) = common::new_runtime() else {
        return;
    };
    let registry_binder = settings_binder(&rt);
    let mut data = content_provider_request("dev.acos.fixtures.counter", "settings");
    let mut reply = Parcel::new();
    ams_java::transact(rt.vm_mut(), BinderId(0), 30, &mut data, &mut reply, 0)
        .expect("getContentProvider transact");
    reply.set_data_position(0);
    assert_eq!(reply.read_int32().expect("read status"), 0, "no exception");
    let vm = rt.vm_mut();
    let holder = acos_runtime::parcel_java::read_typed_object(
        vm,
        &mut reply,
        "android.app.ContentProviderHolder",
    )
    .expect("read ContentProviderHolder")
    .expect("the registry serves `settings`");

    // `ProviderInfo` from the registry entry.
    let info = as_obj(vm.get_field(holder, "info").expect("info field"));
    for (field, expected) in [
        ("authority", "settings"),
        ("packageName", "com.android.providers.settings"),
        ("name", "SettingsProvider"),
        ("processName", "android.process.settings"),
        ("readPermission", "android.permission.READ_SETTINGS"),
    ] {
        assert_eq!(field_string(vm, info, field).as_deref(), Some(expected), "{field}");
    }

    // `provider` is the binder the host dispatches `IContentProvider` to.
    let provider = as_obj(vm.get_field(holder, "provider").expect("provider field"));
    let remote = as_obj(
        vm.call_method(provider, "asBinder", "()Landroid/os/IBinder;", &[])
            .expect("asBinder"),
    );
    assert_eq!(
        field_long(vm, remote, "mNativeData"),
        registry_binder.0,
        "holder.provider must be the host's IContentProvider binder"
    );

    // The connection token is a registered binder (the framework writes it back
    // to removeContentProvider/refContentProvider), and the holder is local.
    let connection = as_obj(vm.get_field(holder, "connection").expect("connection field"));
    let connection_id = BinderId(field_long(vm, connection, "mNativeData"));
    assert_ne!(connection_id.0, 0, "a real ContentProviderConnection token");
    assert_eq!(
        vm.get_field(holder, "noReleaseNeeded").expect("noReleaseNeeded"),
        JavaValue::Boolean(false),
        "the framework's own refcounting runs"
    );
    assert_eq!(
        vm.get_field(holder, "mLocal").expect("mLocal"),
        JavaValue::Boolean(true),
        "the provider is served in this process"
    );
    assert_eq!(reply.data_avail(), 0, "the holder is exactly sized");
    assert_eq!(
        rt.host().borrow().descriptor_of(connection_id),
        Some(CONNECTION_DESCRIPTOR),
        "the connection is a registered binder id"
    );

    // The registry recorded the lookup (the authority list has one owner).
    let lookups = rt.host().borrow_mut().provider_lookups();
    assert_eq!(lookups.len(), 1);
    assert_eq!(lookups[0].method, "getContentProvider");
    assert_eq!(lookups[0].authority, "settings");
    assert!(lookups[0].hit);
    assert_eq!(lookups[0].binder, registry_binder.0);
}

/// Q2b: `IContentProvider.call` (`GET_config`/`LIST_config`, the `Settings$Config`
/// and `Settings$NameValueCache` requests) and `IContentProvider.query` (the
/// `getStringForUser` fallback) return the registry's data through the
/// `ams_java` path, byte-identical to the path `acos-binder-peer` dispatches.
#[test]
fn content_provider_call_and_query_return_registry_data_through_ams_java() {
    let Some(mut rt) = common::new_runtime() else {
        return;
    };
    let binder = settings_binder(&rt);

    // --- GET_config ------------------------------------------------------
    let (mut reply, bytes) = provider_transact_via_ams_java(
        &mut rt,
        binder,
        CALL,
        call_request("GET_config", "ime/enabled_input_methods", None),
    );
    assert_eq!(reply.read_int32().expect("exception header"), 0);
    let bundle = parcelable::read_bundle(&mut reply)
        .expect("GET_config reply")
        .expect("a Bundle");
    assert_eq!(
        bundle.get("value").and_then(Value::as_str),
        Some("com.android.inputmethod.latin/.LatinIME"),
        "the DeviceConfig value the registry seeds"
    );
    assert_eq!(reply.data_avail(), 0, "the reply is exactly sized");
    let (_, peer_bytes) = provider_transact_via_host(
        &rt,
        binder,
        CALL,
        call_request("GET_config", "ime/enabled_input_methods", None),
    );
    assert_eq!(bytes, peer_bytes, "one implementation: identical reply bytes");

    // --- LIST_config (the P2a §4 blocker) --------------------------------
    let mut extras = Bundle::new();
    extras.put("_prefix", Value::Str(Some("ime/".into())));
    let (mut reply, bytes) = provider_transact_via_ams_java(
        &mut rt,
        binder,
        CALL,
        call_request("LIST_config", "", Some(&extras)),
    );
    assert_eq!(reply.read_int32().expect("exception header"), 0);
    let bundle = parcelable::read_bundle(&mut reply)
        .expect("LIST_config reply")
        .expect("a Bundle");
    let entries = match bundle.get("value") {
        Some(Value::Map(Some(entries))) => entries,
        other => panic!("LIST_config value must be a HashMap, got {other:?}"),
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
        ],
        "`Settings$Config.getStrings` reads `namespace/name` keys"
    );
    assert_eq!(reply.data_avail(), 0, "the reply is exactly sized");
    let (_, peer_bytes) = provider_transact_via_host(
        &rt,
        binder,
        CALL,
        call_request("LIST_config", "", Some(&extras)),
    );
    assert_eq!(bytes, peer_bytes, "one implementation: identical reply bytes");

    // --- query (the `ContentResolver` fallback) --------------------------
    let (mut reply, _) = provider_transact_via_ams_java(
        &mut rt,
        binder,
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
    let window = descriptor.window.expect("a CursorWindow");
    assert_eq!(
        window.string(0, 0).as_deref(),
        Some("acos0000000000ff"),
        "the `secure` row the registry seeds"
    );
    assert_eq!(reply.data_avail(), 0, "the window frame is exactly sized");
}

fn field_string(vm: &mut Vm, obj: Handle, field: &str) -> Option<String> {
    match vm.get_field(obj, field).expect("field") {
        JavaValue::Object(h) if h != acos_vm::heap::NULL => {
            Some(vm.java_string_to_rust(h).expect("string"))
        }
        _ => None,
    }
}

fn field_long(vm: &mut Vm, obj: Handle, field: &str) -> u64 {
    match vm.get_field(obj, field).expect("field") {
        JavaValue::Long(v) => u64::try_from(v).expect("binder id"),
        other => panic!("{field} must be a long, got {}", other.kind()),
    }
}
