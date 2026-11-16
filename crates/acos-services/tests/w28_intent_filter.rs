//! W28 acceptance: `android.content.IntentFilter` (the argument of
//! `IActivityManager.registerReceiver[WithFeature]`, codes 18/19) and the AMS
//! fake's dynamic-receiver handling.
//!
//! Every byte shape here is transcribed from the **image** (`.cache/art/sysroot`
//! `framework.jar` / `.cache/boot/android-34/dex/framework`) with
//! `acos-dex`'s `dexdump`:
//!
//! * `Landroid/content/IntentFilter;->writeToParcel(Landroid/os/Parcel;I)V`
//!   (`classes.dex`) — 16 fields: `writeStringArray(actions)`, then
//!   presence+`writeStringList` pairs for categories / data schemes / static
//!   data types / data types / mime groups, then presence+count+payload groups
//!   for scheme-specific parts (`PatternMatcher`), authorities
//!   (`IntentFilter$AuthorityEntry`) and paths, then
//!   priority / hasStaticPartialTypes / hasDynamicPartialTypes / autoVerify /
//!   instantAppVisibility / order, then optional `PersistableBundle` extras.
//!   `Landroid/content/IntentFilter;-><init>(Landroid/os/Parcel;)V` reads the
//!   same order back.
//! * `Landroid/os/PatternMatcher;->writeToParcel` = `writeString(mPattern)`,
//!   `writeInt(mType)`, `writeIntArray(mParsedPattern)` (`classes3.dex`).
//! * `Landroid/content/IntentFilter$AuthorityEntry;->writeToParcel` =
//!   `writeString(mOrigHost)`, `writeString(mHost)`, `writeInt(mWild)`,
//!   `writeInt(mPort)` (`classes.dex`).
//! * `IActivityManager$Stub$Proxy.registerReceiverWithFeature` writes
//!   `binder, Str16, Str16, Str16, binder, typedObject(IntentFilter), Str16,
//!   Int, Int` and transacts code 19; `registerReceiver` writes
//!   `binder, Str16, binder, typedObject(IntentFilter), Str16, Int, Int`
//!   (code 18). The reply is `readTypedObject(Intent.CREATOR)`.
//!
//! Negative cases are first-class: a truncated string, a count that cannot fit
//! in the remaining payload and a nested-message count must all produce a
//! structured `SERVICE_BAD_REQUEST` — never a panic, never an allocation driven
//! by a hostile guest count.

use std::time::Instant;

use acos_parcel::{BinderId, Parcel};
use acos_services::exception::NO_EXCEPTION;
use acos_services::parcelable::{self, FieldOp};
use acos_services::{ServiceError, ServiceHost, Value};

const IACTIVITY_MANAGER: &str = "android.app.IActivityManager";
const INTENT_FILTER: &str = "android.content.IntentFilter";
const CALLER: u64 = 0x7101;
const RECEIVER: u64 = 0x7201;
/// `android.content.Context.RECEIVER_NOT_EXPORTED` (API 34).
const RECEIVER_NOT_EXPORTED: i32 = 0x4;

fn transact(
    host: &mut ServiceHost,
    binder: BinderId,
    code: u32,
    data: &mut Parcel,
) -> std::result::Result<Parcel, ServiceError> {
    let mut reply = Parcel::new();
    host.transact(binder, code, data, &mut reply, 0)?;
    reply.set_data_position(0);
    Ok(reply)
}

fn no_exception(reply: &mut Parcel) {
    assert_eq!(reply.read_int32().expect("exception header"), NO_EXCEPTION);
}

/// `IntentFilter.writeToParcel` for the filter RN's debug build creates in
/// `DevSupportManagerBase.compatRegisterReceiver`: `new IntentFilter(RELOAD)`,
/// i.e. one action and every other list null.
fn write_dev_support_filter(p: &mut Parcel) {
    p.write_int32(1);
    p.write_string16("com.facebook.react.devsupport.RELOAD")
        .expect("action");
    for _ in 0..5 {
        // categories, dataSchemes, staticDataTypes, dataTypes, mMimeGroups:
        // null -> the presence int alone.
        p.write_int32(0);
    }
    for _ in 0..3 {
        // dataSchemeSpecificParts, dataAuthorities, dataPaths.
        p.write_int32(0);
    }
    for _ in 0..6 {
        // priority, hasStaticPartialTypes, hasDynamicPartialTypes,
        // autoVerify, instantAppVisibility, order.
        p.write_int32(0);
    }
    p.write_int32(0);
}

fn write_pattern(p: &mut Parcel, pattern: &str, kind: i32, parsed: &[i32]) {
    p.write_string16(pattern).expect("pattern");
    p.write_int32(kind);
    p.write_int32_array(Some(parsed)).expect("parsed pattern");
}

/// A filter that exercises every field, in the image's order.
fn write_rich_filter(p: &mut Parcel) {
    // writeStringArray(actions)
    p.write_int32(2);
    p.write_string16("android.intent.action.VIEW").expect("action");
    p.write_string16("android.intent.action.SEND").expect("action");
    // categories (present)
    p.write_int32(1);
    p.write_int32(2);
    p.write_string16("android.intent.category.DEFAULT").expect("cat");
    p.write_string16("android.intent.category.BROWSABLE").expect("cat");
    // dataSchemes (present)
    p.write_int32(1);
    p.write_int32(1);
    p.write_string16("https").expect("scheme");
    // mStaticDataTypes (present)
    p.write_int32(1);
    p.write_int32(1);
    p.write_string16("text/plain").expect("type");
    // mDataTypes (present)
    p.write_int32(1);
    p.write_int32(2);
    p.write_string16("*/*").expect("type");
    p.write_string16("image/*").expect("type");
    // mMimeGroups (present)
    p.write_int32(1);
    p.write_int32(1);
    p.write_string16("images").expect("mime group");
    // dataSchemeSpecificParts: presence, count, PatternMatcher payloads
    p.write_int32(1);
    p.write_int32(1);
    write_pattern(p, "example.com/path", 1, &[1, 2, 3]);
    // dataAuthorities: presence, count, AuthorityEntry payloads
    p.write_int32(1);
    p.write_int32(2);
    for (host, orig, wild, port) in [
        ("example.com", "example.com", 0, -1),
        ("example.com", "*.example.com", 1, 8080),
    ] {
        p.write_string16(orig).expect("orig host");
        p.write_string16(host).expect("host");
        p.write_int32(wild);
        p.write_int32(port);
    }
    // dataPaths: presence, count, PatternMatcher payloads
    p.write_int32(1);
    p.write_int32(2);
    write_pattern(p, "/articles/.*", 2, &[]);
    write_pattern(p, "prefix:", 1, &[7]);
    // priority, partial types, autoVerify, instant-app visibility, order
    p.write_int32(10);
    p.write_int32(1);
    p.write_int32(0);
    p.write_int32(1);
    p.write_int32(2);
    p.write_int32(7);
    // mExtras: present, empty PersistableBundle (length + magic + 0 entries).
    p.write_int32(1);
    let empty = acos_services::value::Bundle::new();
    parcelable::write_bundle(p, Some(&empty)).expect("extras bundle");
}

/// A `registerReceiverWithFeature` request (`code 19`) in the image's order.
struct Request {
    data: Parcel,
    /// Offset of the first `IntentFilter` byte (for truncation tests).
    filter_start: usize,
}

impl Request {
    /// Cut the request inside the filter payload, mirroring a peer that writes
    /// a short body.
    fn truncate_to_filter_offset(mut self, offset: usize) -> Parcel {
        self.data.set_data_size(self.filter_start + offset);
        self.data.set_data_position(0);
        self.data
    }

    fn into_parcel(mut self) -> Parcel {
        self.data.set_data_position(0);
        self.data
    }
}

fn register_with_feature(
    receiver: BinderId,
    receiver_id: &str,
    filter: impl FnOnce(&mut Parcel),
) -> Request {
    let mut data = Parcel::new();
    data.write_interface_token(IACTIVITY_MANAGER)
        .expect("interface token");
    data.write_strong_binder_opt(Some(BinderId(CALLER)));
    data.write_string16("com.gaten").expect("caller package");
    data.write_string16("").expect("caller feature id");
    data.write_string16(receiver_id).expect("receiver id");
    data.write_strong_binder_opt(Some(receiver));
    data.write_int32(1);
    let filter_start = data.data_position();
    filter(&mut data);
    data.write_string16("").expect("permission");
    data.write_int32(-2);
    data.write_int32(RECEIVER_NOT_EXPORTED);
    Request { data, filter_start }
}

// ---------------------------------------------------------------------------
// 1. the codec itself
// ---------------------------------------------------------------------------

#[test]
fn the_intent_filter_codec_matches_the_image_spec() {
    let spec = parcelable::spec_for(INTENT_FILTER).expect("IntentFilter spec");
    assert_eq!(spec.fields.len(), 16, "the image's 16 write groups");
    assert_eq!(spec.fields[0], FieldOp::StringArray, "mActions");
    for (i, field) in spec.fields.iter().enumerate().take(6).skip(1) {
        assert!(
            matches!(field, FieldOp::Cond(&[FieldOp::StringList])),
            "field {i} is a presence+writeStringList pair"
        );
    }
    assert!(matches!(
        spec.fields[6],
        FieldOp::Cond(&[FieldOp::CountedNested("android.os.PatternMatcher")])
    ));
    assert!(matches!(
        spec.fields[7],
        FieldOp::Cond(&[FieldOp::CountedNested(
            "android.content.IntentFilter$AuthorityEntry"
        )])
    ));
    assert!(matches!(
        spec.fields[15],
        FieldOp::Cond(&[FieldOp::PersistableBundle])
    ));
    // The nested payloads are not `Parcelable`s on the wire (no presence marker,
    // no class name), so they need their own payload specs.
    assert!(parcelable::has_codec("android.os.PatternMatcher"));
    assert!(parcelable::has_codec("android.content.IntentFilter$AuthorityEntry"));
}

#[test]
fn the_image_dev_support_filter_decodes_to_exactly_its_bytes() {
    let mut p = Parcel::new();
    write_dev_support_filter(&mut p);
    let total = p.data_size();
    p.set_data_position(0);
    let filter = parcelable::read_payload(INTENT_FILTER, &mut p).expect("decode");
    assert_eq!(p.data_avail(), 0, "every byte of the {total}-byte payload");
    assert_eq!(
        filter.field(0),
        Some(&Value::StringArray(Some(vec![Some(
            "com.facebook.react.devsupport.RELOAD".to_string()
        )]))),
        "actions come from writeStringArray"
    );
    for i in 1..=5 {
        assert_eq!(
            filter.field(i),
            Some(&Value::Cond {
                present: false,
                values: Vec::new()
            }),
            "field {i} is the null list marker"
        );
    }
    assert_eq!(filter.field(9), Some(&Value::Int(0)), "priority");
    assert_eq!(filter.field(15).and_then(|v| match v {
        Value::Cond { present, .. } => Some(*present),
        _ => None,
    }), Some(false), "null extras");
}

#[test]
fn a_rich_intent_filter_round_trips_byte_for_byte() {
    let mut p = Parcel::new();
    write_rich_filter(&mut p);
    let bytes = p.marshall();
    p.set_data_position(0);
    let filter = parcelable::read_payload(INTENT_FILTER, &mut p).expect("decode");
    assert_eq!(p.data_avail(), 0, "the whole filter is consumed");

    let mut out = Parcel::new();
    parcelable::write_payload(&mut out, &filter).expect("re-encode");
    assert_eq!(
        out.marshall(),
        bytes,
        "writeToParcel(decoded) reproduces the image bytes"
    );
}

// ---------------------------------------------------------------------------
// 2. the AMS fake: record the receiver + filter, answer a null Intent
// ---------------------------------------------------------------------------

#[test]
fn register_receiver_with_feature_records_the_receiver_and_answers_null() {
    let mut host = ServiceHost::standard();
    let am = host
        .check_service("activity")
        .expect("lookup")
        .expect("activity manager");

    let mut data = register_with_feature(BinderId(RECEIVER), "reload-receiver", |p| {
        write_dev_support_filter(p)
    })
    .into_parcel();
    let mut reply = transact(&mut host, am, 19, &mut data).expect("registerReceiverWithFeature");
    no_exception(&mut reply);
    assert_eq!(
        reply.read_int32().expect("typed object marker"),
        0,
        "non-sticky registration answers a null Intent"
    );
    assert_eq!(reply.data_avail(), 0, "reply is exactly the null Intent");

    let records = host.receiver_registrations();
    assert_eq!(records.len(), 1, "one registration recorded");
    let record = &records[0];
    assert_eq!(record.receiver, RECEIVER);
    assert_eq!(record.package, "com.gaten");
    assert_eq!(record.receiver_id, "reload-receiver");
    assert_eq!(record.flags, RECEIVER_NOT_EXPORTED);
    assert_eq!(
        record.actions,
        vec!["com.facebook.react.devsupport.RELOAD".to_string()]
    );
    assert!(record.categories.is_empty());
    assert!(record.data_types.is_empty());
    assert_eq!(record.priority, 0);
    assert!(!record.auto_verify);

    // The record is also visible in the guest log (audit trail, no secrets).
    let events = host.drain_events();
    let logged: Vec<&str> = events
        .iter()
        .filter_map(|e| match &e.body {
            acos_protocol::EventBody::Log { tag, message, .. }
                if tag == "ActivityManagerReceiver" =>
            {
                Some(message.as_str())
            }
            _ => None,
        })
        .collect();
    assert_eq!(logged.len(), 1, "one receiver log line: {events:?}");
    assert!(
        logged[0].contains("com.facebook.react.devsupport.RELOAD"),
        "{}",
        logged[0]
    );
    assert!(logged[0].contains("flags=0x4"), "{}", logged[0]);
}

#[test]
fn register_receiver_code_18_uses_the_same_codec_and_records_the_full_filter() {
    let mut host = ServiceHost::standard();
    let am = host
        .check_service("activity")
        .expect("lookup")
        .expect("activity manager");

    // `registerReceiver(caller, callerPackage, receiver, filter, permission,
    // userId, flags)` -- code 18.
    let mut data = Parcel::new();
    data.write_interface_token(IACTIVITY_MANAGER).expect("token");
    data.write_strong_binder_opt(Some(BinderId(CALLER)));
    data.write_string16("com.gaten").expect("package");
    data.write_strong_binder_opt(Some(BinderId(RECEIVER)));
    data.write_int32(1);
    write_rich_filter(&mut data);
    data.write_string16("").expect("permission");
    data.write_int32(0);
    data.write_int32(RECEIVER_NOT_EXPORTED);
    data.set_data_position(0);

    let mut reply = transact(&mut host, am, 18, &mut data).expect("registerReceiver");
    no_exception(&mut reply);
    assert_eq!(reply.read_int32().expect("null marker"), 0);
    assert_eq!(reply.data_avail(), 0);

    let records = host.receiver_registrations();
    assert_eq!(records.len(), 1);
    let r = &records[0];
    assert_eq!(r.receiver, RECEIVER);
    assert_eq!(r.receiver_id, "", "the legacy form has no receiverId");
    assert_eq!(r.actions.len(), 2);
    assert_eq!(
        r.categories,
        vec![
            "android.intent.category.DEFAULT".to_string(),
            "android.intent.category.BROWSABLE".to_string()
        ]
    );
    assert_eq!(r.schemes, vec!["https".to_string()]);
    assert_eq!(r.static_data_types, vec!["text/plain".to_string()]);
    assert_eq!(r.data_types, vec!["*/*".to_string(), "image/*".to_string()]);
    assert_eq!(r.mime_groups, vec!["images".to_string()]);
    assert_eq!(
        r.scheme_specific_parts,
        vec![acos_services::atms::PatternFacts {
            pattern: Some("example.com/path".to_string()),
            kind: 1,
        }]
    );
    assert_eq!(r.authorities.len(), 2);
    assert_eq!(r.authorities[0].host.as_deref(), Some("example.com"));
    assert_eq!(r.authorities[0].port, -1);
    assert!(!r.authorities[0].wild);
    assert!(r.authorities[1].wild, "*.example.com is a wildcard entry");
    assert_eq!(r.authorities[1].port, 8080);
    assert_eq!(r.paths.len(), 2);
    assert_eq!(r.paths[0].pattern.as_deref(), Some("/articles/.*"));
    assert_eq!(r.paths[0].kind, 2);
    assert_eq!(r.priority, 10);
    assert!(r.has_static_partial_types);
    assert!(!r.has_dynamic_partial_types);
    assert!(r.auto_verify);
    assert_eq!(r.instant_app_visibility, 2);
    assert_eq!(r.order, 7);
}

#[test]
fn a_null_filter_is_recorded_as_an_empty_filter_not_an_error() {
    let mut host = ServiceHost::standard();
    let am = host
        .check_service("activity")
        .expect("lookup")
        .expect("activity manager");

    let mut data = Parcel::new();
    data.write_interface_token(IACTIVITY_MANAGER).expect("token");
    data.write_strong_binder_opt(Some(BinderId(CALLER)));
    data.write_string16("com.gaten").expect("package");
    data.write_string16("").expect("feature id");
    data.write_string16("").expect("receiver id");
    data.write_strong_binder_opt(Some(BinderId(RECEIVER)));
    data.write_int32(0);
    data.write_string16("").expect("permission");
    data.write_int32(0);
    data.write_int32(0);
    data.set_data_position(0);

    let mut reply = transact(&mut host, am, 19, &mut data).expect("null filter accepted");
    no_exception(&mut reply);
    assert_eq!(reply.read_int32().expect("null Intent marker"), 0);
    assert_eq!(reply.data_avail(), 0);

    let records = host.receiver_registrations();
    assert_eq!(records.len(), 1);
    assert!(records[0].actions.is_empty());
}

#[test]
fn unregister_receiver_drops_the_registration() {
    let mut host = ServiceHost::standard();
    let am = host
        .check_service("activity")
        .expect("lookup")
        .expect("activity manager");

    let mut data = register_with_feature(BinderId(RECEIVER), "reload-receiver", |p| {
        write_dev_support_filter(p)
    })
    .into_parcel();
    transact(&mut host, am, 19, &mut data).expect("register");
    assert_eq!(host.receiver_registrations().len(), 1);

    // `unregisterReceiver(IIntentReceiver)` -- code 20, void.
    let mut data = Parcel::new();
    data.write_interface_token(IACTIVITY_MANAGER).expect("token");
    data.write_strong_binder_opt(Some(BinderId(RECEIVER)));
    data.set_data_position(0);
    let reply = transact(&mut host, am, 20, &mut data).expect("unregisterReceiver");
    let mut reply = reply;
    no_exception(&mut reply);
    assert_eq!(reply.data_avail(), 0, "void reply: only the exception header");
    assert!(
        host.receiver_registrations().is_empty(),
        "the registration is gone"
    );

    // Unregistering an unknown receiver is swallowed (the guest's own bookkeeping
    // is authoritative); it must not be an error.
    let mut data = Parcel::new();
    data.write_interface_token(IACTIVITY_MANAGER).expect("token");
    data.write_strong_binder_opt(Some(BinderId(0x9999)));
    data.set_data_position(0);
    assert!(transact(&mut host, am, 20, &mut data).is_ok());
}

#[test]
fn broadcast_intent_delivers_to_a_matching_dynamic_receiver() {
    use acos_services::ams::{Extra, IntentFacts};
    use std::collections::BTreeMap;

    let mut host = ServiceHost::standard();
    let am = host
        .check_service("activity")
        .expect("lookup")
        .expect("activity manager");

    let mut data = register_with_feature(BinderId(RECEIVER), "", |p| {
        write_dev_support_filter(p)
    })
    .into_parcel();
    transact(&mut host, am, 19, &mut data).expect("register");
    let _ = host.drain_events();

    let intent = IntentFacts {
        action: Some("com.facebook.react.devsupport.RELOAD".to_string()),
        data: None,
        package: None,
        component: None,
        categories: Vec::new(),
        extras: BTreeMap::<String, Extra>::new(),
    }
    .to_parcelable();
    // `broadcastIntent(caller, intent, resolvedType, resultTo, resultCode,
    // resultData, map, requiredPermissions, appOp, bOptions, serialized,
    // sticky, userId)` -- code 21. The call goes to the service object
    // directly: the generated argument/reply encoder would need a full
    // `android.content.Intent` writeToParcel spec, which is not this wall (the
    // AMS fake consumes the reduced `IntentFacts` form, exactly like the VM's
    // `ams_java`).
    let args = [
        Value::Binder(Some(BinderId(CALLER))),
        Value::TypedObject(intent),
        Value::Str(Some(String::new())),
        Value::Binder(None),
        Value::Int(0),
        Value::Str(None),
        Value::Bundle(Some(acos_services::value::Bundle::new())),
        Value::StringArray(None),
        Value::Int(-1),
        Value::Bundle(None),
        Value::Bool(false),
        Value::Bool(false),
        Value::Int(0),
    ];
    let info = acos_aidl::lookup_by_name(IACTIVITY_MANAGER, "broadcastIntent")
        .expect("broadcastIntent table entry");
    let result = host
        .ensure("android.app.IActivityManager")
        .expect("activity service")
        .call(IACTIVITY_MANAGER, info, &args)
        .expect("broadcastIntent accepted");
    assert!(
        matches!(result.reply.as_slice(), [Value::Int(1)]),
        "the recorded filter matches the broadcast exactly once, got {:?}",
        result.reply
    );
    // A matching **dynamic** receiver is delivered to: the ART host
    // materialises `IIntentReceiver.performReceive` from the queued match (the
    // old "no receiver scheduler" rule dropped it, which is why a service module
    // could not raise an event). A one-way delivery carries a well-formed
    // request parcel and targets the registered binder.
    assert_eq!(
        result.deliveries.len(),
        1,
        "the matching receiver gets one performReceive delivery: {:?}",
        result.deliveries
    );
    let delivery = &result.deliveries[0];
    assert_eq!(
        delivery.interface, "android.content.IIntentReceiver",
        "the delivery is performReceive"
    );
    assert_eq!(delivery.code, 1, "performReceive is the first transaction");
    assert_eq!(
        delivery.target.0, RECEIVER,
        "the delivery targets the registered IIntentReceiver"
    );
    let mut data = delivery.data.clone();
    data.set_data_position(0);
    assert_eq!(
        data.peek_interface_token().expect("token").as_deref(),
        Some("android.content.IIntentReceiver")
    );

    // A different action does not match the recorded filter: the count is the
    // live proof that the registration (not a blanket "accept") drives matching.
    let other = IntentFacts {
        action: Some("com.gaten.SOMETHING_ELSE".to_string()),
        data: None,
        package: None,
        component: None,
        categories: Vec::new(),
        extras: BTreeMap::<String, Extra>::new(),
    }
    .to_parcelable();
    let mut other_args = args.to_vec();
    other_args[1] = Value::TypedObject(other);
    let result = host
        .ensure("android.app.IActivityManager")
        .expect("activity service")
        .call(IACTIVITY_MANAGER, info, &other_args)
        .expect("non-matching broadcast accepted");
    assert!(
        matches!(result.reply.as_slice(), [Value::Int(0)]),
        "no receiver matches an unrelated action, got {:?}",
        result.reply
    );

    // A broadcast nobody matched delivers nothing and fabricates no event.
    assert!(
        result.deliveries.is_empty(),
        "an unmatched broadcast has no delivery: {:?}",
        result.deliveries
    );
    let events = host.drain_events();
    let notes: Vec<&str> = events
        .iter()
        .filter_map(|e| match &e.body {
            acos_protocol::EventBody::Log { tag, message, .. }
                if tag == "ActivityManagerBroadcast" =>
            {
                Some(message.as_str())
            }
            _ => None,
        })
        .collect();
    assert!(
        notes.is_empty(),
        "an unmatched broadcast is Android-normal, not a log line: {notes:?}"
    );
    assert_eq!(
        host.receiver_registrations().len(),
        1,
        "the broadcast does not disturb the registration"
    );
}

// ---------------------------------------------------------------------------
// 3. negative cases: truncated and hostile bodies
// ---------------------------------------------------------------------------

#[test]
fn a_request_truncated_inside_the_action_string_is_a_structured_error() {
    let mut host = ServiceHost::standard();
    let am = host
        .check_service("activity")
        .expect("lookup")
        .expect("activity manager");
    // count (4) + the action's length word (4): the string body is missing.
    let mut data = register_with_feature(BinderId(RECEIVER), "", |p| write_dev_support_filter(p))
        .truncate_to_filter_offset(8);
    let err = transact(&mut host, am, 19, &mut data).expect_err("truncated action");
    assert_eq!(err.code(), "SERVICE_BAD_REQUEST", "{err}");
    assert!(
        format!("{err}").contains("android.content.IntentFilter"),
        "the error names the class it could not read: {err}"
    );
    assert!(host.receiver_registrations().is_empty());
}

#[test]
fn hostile_counts_are_refused_before_allocating() {
    let mut host = ServiceHost::standard();
    let am = host
        .check_service("activity")
        .expect("lookup")
        .expect("activity manager");

    // (a) actions count = i32::MAX with no strings behind it.
    let mut data = register_with_feature(BinderId(RECEIVER), "", |p| {
        p.write_int32(i32::MAX);
    })
    .into_parcel();
    let started = Instant::now();
    let err = transact(&mut host, am, 19, &mut data).expect_err("hostile action count");
    assert_eq!(err.code(), "SERVICE_BAD_REQUEST", "{err}");
    assert!(
        started.elapsed().as_millis() < 500,
        "no unbounded allocation/loop for i32::MAX strings ({} ms)",
        started.elapsed().as_millis()
    );

    // (b) categories present with a count of 1<<28.
    let mut data = register_with_feature(BinderId(RECEIVER), "", |p| {
        p.write_int32(0);
        p.write_int32(1);
        p.write_int32(1 << 28);
    })
    .into_parcel();
    let started = Instant::now();
    let err = transact(&mut host, am, 19, &mut data).expect_err("hostile list count");
    assert_eq!(err.code(), "SERVICE_BAD_REQUEST", "{err}");
    assert!(
        started.elapsed().as_millis() < 500,
        "bounded string list ({} ms)",
        started.elapsed().as_millis()
    );

    // (c) scheme-specific parts present with a count of i32::MAX.
    let mut data = register_with_feature(BinderId(RECEIVER), "", |p| {
        p.write_int32(0);
        for _ in 0..5 {
            p.write_int32(0);
        }
        p.write_int32(1);
        p.write_int32(i32::MAX);
    })
    .into_parcel();
    let started = Instant::now();
    let err = transact(&mut host, am, 19, &mut data).expect_err("hostile nested count");
    assert_eq!(err.code(), "SERVICE_BAD_REQUEST", "{err}");
    assert!(
        started.elapsed().as_millis() < 500,
        "bounded nested list ({} ms)",
        started.elapsed().as_millis()
    );
    assert!(host.receiver_registrations().is_empty());
}

#[test]
fn a_present_marker_with_a_null_list_is_legal_and_records_no_entries() {
    // `writeStringList(null)` writes `-1`; the image's reader accepts it, so the
    // codec must too (a presence marker of 1 does not guarantee a count >= 0).
    let mut p = Parcel::new();
    p.write_int32(0);
    p.write_int32(1);
    p.write_int32(-1);
    for _ in 0..4 {
        p.write_int32(0);
    }
    for _ in 0..3 {
        p.write_int32(0);
    }
    for _ in 0..6 {
        p.write_int32(0);
    }
    p.write_int32(0);
    p.set_data_position(0);
    let filter = parcelable::read_payload(INTENT_FILTER, &mut p).expect("present-but-null list");
    assert_eq!(p.data_avail(), 0);
    assert_eq!(
        filter.field(1),
        Some(&Value::Cond {
            present: true,
            values: vec![Value::StringList(None)],
        })
    );
}

#[test]
fn a_dropped_presence_word_does_not_silently_decode() {
    // Layout sensitivity: the image writes a presence int before every optional
    // list. Dropping the one before a non-empty categories list shifts the rest
    // by four bytes; a codec that fuzzes over the mismatch would accept it.
    let mut p = Parcel::new();
    p.write_int32(1);
    p.write_string16("android.intent.action.VIEW").expect("action");
    // NOTE: no categories presence word -- this writes the count directly.
    p.write_int32(1);
    p.write_string16("android.intent.category.DEFAULT").expect("category");
    for _ in 0..4 {
        p.write_int32(0);
    }
    for _ in 0..3 {
        p.write_int32(0);
    }
    for _ in 0..6 {
        p.write_int32(0);
    }
    p.write_int32(0);
    let total = p.data_size();
    p.set_data_position(0);
    match parcelable::read_payload(INTENT_FILTER, &mut p) {
        Err(_) => {} // the common case: the shifted tail underflows
        Ok(_) => assert_ne!(
            p.data_avail(),
            0,
            "a body with the presence word missing must not consume exactly {total} bytes"
        ),
    }
}
