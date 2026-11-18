//! MODFILL: the `core` autofill service answers the `IResultReceiver` its
//! `oneway` methods carry, in the shape the framework's own
//! `SyncResultReceiver` reads.
//!
//! `IAutoFillManager.addClient`, `startSession`, `isServiceEnabled`,
//! `isServiceSupported`, `isFieldClassificationEnabled`, ... are all
//! `oneway` with a trailing `IResultReceiver` (see
//! `crates/acos-aidl/src/generated/tables.rs`): the answer is **not** a reply
//! int, it is `IResultReceiver.send(resultCode, Bundle)` delivered to the
//! caller. The image's `com.android.internal.util.SyncResultReceiver`
//! (`framework/classes3.dex`) reads:
//!
//! | method | reads |
//! |---|---|
//! | `getIntResult()` | the transaction's `resultCode` (`mResult`) |
//! | `getOptionalExtraIntResult(int)` | the `EXTRA` key out of the bundle — and NPEs on a `null` bundle |
//! | `getParcelableResult()`, `getStringResult()`, ... | the `EXTRA` key out of the bundle |
//!
//! and `SyncResultReceiver.bundleFor(int value)` builds exactly
//! `Bundle{"EXTRA": value}`. So an answer that is a stray reply int leaves
//! `AutofillManager` blocked on its 15000 ms / 5000 ms timeout, and an answer
//! that passes `null` for `startSession` is an NPE in the app once a service
//! exists.
//!
//! These tests pin the two facts that make that impossible.

use acos_parcel::{BinderId, Parcel};
use acos_services::host::ServiceHost;
use acos_services::module::{ModuleConfig, ModuleRegistry, Profile};
use acos_services::value::Value;

const AUTOFILL: &str = "android.view.autofill.IAutoFillManager";
/// `acos-binder-peer`'s `JAVA_TAG`: guest-owned binders arrive tagged.
const GUEST: u64 = 1 << 32;

fn guest(wire: u64) -> BinderId {
    BinderId(GUEST | wire)
}

fn call(host: &mut ServiceHost, name: &str, code: u32, data: &mut Parcel) -> Parcel {
    let binder = host
        .check_service(name)
        .expect("lookup is infallible")
        .expect("the autofill service resolves to a binder");
    data.set_data_position(0);
    let mut reply = Parcel::new();
    host.transact(binder, code, data, &mut reply, 0)
        .unwrap_or_else(|error| panic!("{name} code {code}: {error}"));
    reply.set_data_position(0);
    reply
}

fn decoded(host: &mut ServiceHost, name: &str, code: u32, args: Vec<Value>) -> Parcel {
    let method = acos_aidl::lookup(AUTOFILL, code).expect("table row");
    let mut data = acos_services::codec::build_request(AUTOFILL, method, &args).expect("request");
    call(host, name, code, &mut data)
}

fn core(host: &mut ServiceHost) -> ModuleRegistry {
    let mut registry = ModuleRegistry::standard();
    registry
        .attach(host, "core", Profile::Real, &ModuleConfig::default())
        .expect("core attaches");
    registry
}

/// The `(resultCode, EXTRA value)` an `IResultReceiver.send` delivery carries.
fn sync_reply(host: &mut ServiceHost, receiver: BinderId) -> (i32, Option<i32>) {
    let deliveries = host.take_deliveries();
    assert_eq!(deliveries.len(), 1, "exactly one answer is delivered");
    let delivery = &deliveries[0];
    assert_eq!(delivery.target, receiver, "the answer goes to the caller's receiver");
    assert_eq!(delivery.interface, "com.android.internal.os.IResultReceiver");
    assert_eq!(delivery.method, "send");
    let mut data = delivery.data.clone();
    data.set_data_position(0);
    let token = data
        .peek_interface_token()
        .expect("token read")
        .expect("a result receiver token");
    assert_eq!(token, "com.android.internal.os.IResultReceiver");
    data.enforce_interface("com.android.internal.os.IResultReceiver")
        .expect("interface");
    let method = acos_aidl::lookup_by_name("com.android.internal.os.IResultReceiver", "send")
        .expect("table row");
    let values = acos_services::codec::read_args(
        &mut data,
        &method.args,
        "com.android.internal.os.IResultReceiver",
        "send",
    )
    .expect("args");
    let result_code = values
        .first()
        .and_then(|value| match value {
            Value::Int(value) => Some(*value),
            _ => None,
        })
        .expect("result code");
    // The AIDL table writes the `Bundle` argument as a *typed object*
    // (`readTypedObject(Bundle.CREATOR)` in the framework stub), so it arrives
    // as `Parcelable::Ordered{class: "android.os.Bundle", values: [Bundle(..)]}`.
    let bundle = values
        .iter()
        .find_map(|value| match value {
            Value::TypedObject(acos_services::value::Parcelable::Ordered { class, values })
                if class == "android.os.Bundle" =>
            {
                match values.first() {
                    Some(Value::Bundle(bundle)) => bundle.clone(),
                    _ => None,
                }
            }
            _ => None,
        })
        .and_then(|bundle| bundle.get("EXTRA").cloned());
    let extra = bundle
        .and_then(|value| match value {
            Value::Int(value) => Some(value),
            _ => None,
        });
    (result_code, extra)
}

#[test]
fn add_client_answers_the_sync_receiver_with_disabled_flags() {
    let mut host = ServiceHost::standard();
    let _registry = core(&mut host);
    let client = guest(0x71);
    let receiver = guest(0x72);
    // `addClient(IAutoFillManagerClient, ComponentName, int userId,
    //            IResultReceiver)`
    let _ = decoded(
        &mut host,
        "autofill",
        1,
        vec![
            Value::Binder(Some(client)),
            Value::TypedObject(acos_services::value::Parcelable::Null),
            Value::Int(0),
            Value::Binder(Some(receiver)),
        ],
    );
    let (result_code, extra) = sync_reply(&mut host, receiver);
    assert_eq!(
        result_code, 0,
        "flags word: FLAG_ADD_CLIENT_ENABLED clear -> AutofillManager.isEnabled() false"
    );
    assert_eq!(
        extra,
        Some(0),
        "the bundle carries `EXTRA` = the same flags (SyncResultReceiver.bundleFor)"
    );
    assert_eq!(host.held_refs(client), 1, "the registration is a reference");
}

#[test]
fn start_session_answers_no_session_in_the_bundle_not_null() {
    let mut host = ServiceHost::standard();
    let _registry = core(&mut host);
    let receiver = guest(0x73);
    let activity = guest(0x74);
    let client = guest(0x75);
    // `startSession(IBinder activityToken, IBinder client, AutofillId, Rect,
    //  AutofillValue, int flags, int userId, boolean, ComponentName,
    //  boolean, IResultReceiver)` — code 3, oneway. The handler reads only the
    // trailing receiver, so the parcelables are the null typed object the codec
    // writes for `Parcelable::Null`.
    let null = || Value::TypedObject(acos_services::value::Parcelable::Null);
    let _ = decoded(
        &mut host,
        "autofill",
        3,
        vec![
            Value::Binder(Some(activity)),
            Value::Binder(Some(client)),
            null(),
            null(),
            null(),
            Value::Int(0),
            Value::Bool(false),
            Value::Int(0),
            null(),
            Value::Bool(false),
            Value::Binder(Some(receiver)),
        ],
    );
    let (result_code, extra) = sync_reply(&mut host, receiver);
    assert_eq!(result_code, 0);
    assert_eq!(
        extra,
        Some(i32::MAX),
        "`AutofillManager.NO_SESSION` in the EXTRA slot; a null bundle would NPE the app"
    );
}

#[test]
fn is_service_enabled_answers_the_receiver_instead_of_a_reply_int() {
    let mut host = ServiceHost::standard();
    let _registry = core(&mut host);
    let receiver = guest(0x76);
    // `isServiceEnabled(int userId, String packageName, IResultReceiver)` — 14.
    let reply = decoded(
        &mut host,
        "autofill",
        14,
        vec![
            Value::Int(0),
            Value::Str8(Some("dev.acos.fixtures.clipprobe".into())),
            Value::Binder(Some(receiver)),
        ],
    );
    assert_eq!(
        reply.data_size(),
        4,
        "a void oneway method's reply is the exception header only: a stray reply int \
         would leave the client waiting on its receiver"
    );
    let (result_code, _) = sync_reply(&mut host, receiver);
    assert_eq!(result_code, 0, "0 = no autofill service is enabled");
}
