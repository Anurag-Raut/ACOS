//! PROCUID README §7 item 2: a `PendingIntent` answers the uid of the app that
//! **created** it, not the fixture uid.
//!
//! `PendingIntent.getCreatorUid()`/`getCreatorPackage()` both go through
//! `IActivityManager.getInfoForIntentSender` (`PendingIntent.java:1600`
//! `getCachedInfo`), and AOSP's `ActivityManagerService.getInfoForIntentSender`
//! answers `PendingIntentRecord.key.packageName` + `PendingIntentRecord.uid`,
//! where `uid` is the `Binder.getCallingUid()` of the `getIntentSender` call.
//!
//! ACOS answered `pms::FIXTURE_UID` for every record, so in a session with two
//! apps the second app's pending intent reported the primary package's uid —
//! and `getCreatorPackage()` disagreed with `getCreatorUid()` for the same
//! record. The tests here transact the real wire form of `getIntentSender`
//! (code 62) under two calling identities and read the record back through
//! `getInfoForIntentSender` (code 65) as the *other* app, which is exactly the
//! order the framework uses.

use acos_parcel::binder::BinderId;
use acos_parcel::{push_calling_identity, Parcel};
use acos_services::parcelable::write_payload;
use acos_services::value::Value;
use acos_services::ServiceHost;

const ACTIVITY_MANAGER: &str = "android.app.IActivityManager";
/// `getIntentSender` (API-34 table, code 62).
const GET_INTENT_SENDER: u32 = 62;
/// `getIntentSenderWithFeature` (code 63) — the form the framework's
/// `PendingIntent.getBroadcast`/`getActivity` actually call.
const GET_INTENT_SENDER_WITH_FEATURE: u32 = 63;
/// `getInfoForIntentSender` (code 65).
const GET_INFO_FOR_INTENT_SENDER: u32 = 65;

/// The two packages of a two-process session (MPROC4/BINDERPID fixtures).
const PACKAGE_A: &str = "dev.acos.fixtures.proca";
const PACKAGE_B: &str = "dev.acos.fixtures.procb";
/// The uids the package manager allocates for them in one session.
const UID_A: i32 = 10192;
const UID_B: i32 = 10193;

fn installed_host() -> ServiceHost {
    let mut host = ServiceHost::new();
    acos_services::registry::install(&mut host);
    host
}

fn binder_of(host: &mut ServiceHost, name: &str) -> BinderId {
    host.check_service(name)
        .unwrap_or_else(|e| panic!("check_service({name}): {e}"))
        .unwrap_or_else(|| panic!("{name} is not served"))
}

fn transact(host: &mut ServiceHost, code: u32, mut data: Parcel) -> Parcel {
    data.set_data_position(0);
    let binder = binder_of(host, "activity");
    let mut reply = Parcel::new();
    host.transact(binder, code, &mut data, &mut reply, 0)
        .unwrap_or_else(|error| panic!("activity code {code} failed: {error}"));
    reply.set_data_position(0);
    reply
}

/// `IActivityManager.getIntentSender`'s wire form
/// `(type, package, activity, who, requestCode, Intent[], String[],
/// flags, Bundle, userId)`, as the framework's `PendingIntent.getBroadcast`
/// writes it, executed **as `uid`/`pid`** (the dispatcher's calling identity).
fn get_intent_sender_as(host: &mut ServiceHost, uid: i32, pid: i32, package: &str, request: i32) -> u64 {
    push_calling_identity(uid as u32, pid);
    let mut data = Parcel::new();
    data.write_interface_token(ACTIVITY_MANAGER).expect("token");
    data.write_int32(acos_services::pending_intent::TYPE_BROADCAST);
    data.write_string16_opt(Some(package)).expect("package");
    data.write_strong_binder_opt(None); // activity token: null
    data.write_string16_opt(None).expect("who");
    data.write_int32(request);
    // writeTypedArray(Intent[]): count, then each element's presence + payload.
    data.write_int32(1);
    data.write_int32(1);
    let intent = acos_services::ams::IntentFacts::action(&format!("{package}.PROBE"));
    write_payload(&mut data, &intent.to_parcelable()).expect("intent");
    data.write_int32(-1); // resolvedTypes: null String[]
    data.write_int32(acos_services::pending_intent::FLAG_IMMUTABLE);
    data.write_int32(0); // options: null Bundle
    data.write_int32(0); // userId
    let mut reply = transact(host, GET_INTENT_SENDER, data);
    acos_parcel::registry::pop_calling_identity();
    assert_eq!(reply.read_int32().expect("exception header"), 0);
    // `writeStrongBinder`: a flat object, read back as the `IIntentSender` id.
    let sender = reply
        .read_strong_binder()
        .expect("the reply parses")
        .expect("getIntentSender answers a non-null sender");
    sender.0
}

/// The `PendingIntentInfo` the system answers, read **as `uid`/`pid`**.
fn pending_intent_info_as(host: &mut ServiceHost, uid: i32, pid: i32, sender: u64) -> (String, i32) {
    push_calling_identity(uid as u32, pid);
    let mut data = Parcel::new();
    data.write_interface_token(ACTIVITY_MANAGER).expect("token");
    data.write_strong_binder_opt(Some(BinderId(sender)));
    let mut reply = transact(host, GET_INFO_FOR_INTENT_SENDER, data);
    acos_parcel::registry::pop_calling_identity();
    assert_eq!(reply.read_int32().expect("exception header"), 0);
    // `writeTypedObject`: an explicit presence marker before the payload.
    assert_eq!(
        reply.read_int32().expect("PendingIntentInfo presence"),
        1,
        "the reply is a non-null typed object"
    );
    let info = acos_services::parcelable::read_payload(
        "android.app.ActivityManager$PendingIntentInfo",
        &mut reply,
    )
    .expect("PendingIntentInfo body");
    let fields = info.fields().expect("ordered fields");
    assert_eq!(
        info.class(),
        Some("android.app.ActivityManager$PendingIntentInfo"),
        "the reply is the framework's own parcelable"
    );
    let creator = match fields.first() {
        Some(Value::Str(Some(package))) => package.clone(),
        other => panic!("creator package field: {other:?}"),
    };
    let creator_uid = fields
        .get(1)
        .and_then(Value::as_int)
        .unwrap_or_else(|| panic!("creator uid field: {:?}", fields.get(1)));
    (creator, creator_uid)
}

/// The bug: every record answered `FIXTURE_UID`, so the second app's pending
/// intent reported the primary package's uid. Each record now carries the uid of
/// the process that called `getIntentSender`, and the *reader* may be a
/// different app (`getInfoForIntentSender` is a system query).
#[test]
fn a_pending_intent_answers_the_uid_of_the_app_that_created_it() {
    let mut host = installed_host();
    // proc-a (the session's primary package) creates its sender.
    let sender_a = get_intent_sender_as(&mut host, UID_A, 4101, PACKAGE_A, 1);
    // proc-b, a second app in the same session, creates its own.
    let sender_b = get_intent_sender_as(&mut host, UID_B, 4102, PACKAGE_B, 1);
    assert_ne!(sender_a, sender_b, "two packages are two records");

    // proc-b reads proc-a's creator identity: the system answers proc-a.
    assert_eq!(
        pending_intent_info_as(&mut host, UID_B, 4102, sender_a),
        (PACKAGE_A.to_string(), UID_A),
        "a pending intent created by proc-a reports proc-a's package and uid, \
         even when another app asks"
    );
    // ... and proc-a reads proc-b's.
    assert_eq!(
        pending_intent_info_as(&mut host, UID_A, 4101, sender_b),
        (PACKAGE_B.to_string(), UID_B),
        "a pending intent created by proc-b reports proc-b's own uid, not the \
         fixture uid (PROCUID README §7 item 2)"
    );
    // The creator identity is not the caller's: the same record read by
    // proc-b still answers proc-a.
    assert_eq!(
        pending_intent_info_as(&mut host, UID_B, 4102, sender_b).1,
        UID_B
    );
}

/// The framework calls `getIntentSenderWithFeature` (code 63), whose argument
/// list inserts the `featureId` **after** the package name. The host read
/// argument `1 + base` as the package, i.e. the feature id — null for every
/// `PendingIntent.getBroadcast`, so the record's package was empty and
/// `PendingIntent.getCreatorPackage()` answered `""` in the guest (found by the
/// `mproc3-two-process` ART probe: `creator=10193/ mine=10193/…`).
#[test]
fn the_with_feature_form_keeps_the_package_name_out_of_the_feature_id_slot() {
    let mut host = installed_host();
    push_calling_identity(UID_B as u32, 4102);
    let mut data = Parcel::new();
    data.write_interface_token(ACTIVITY_MANAGER).expect("token");
    data.write_int32(acos_services::pending_intent::TYPE_BROADCAST);
    data.write_string16_opt(Some(PACKAGE_B)).expect("package");
    // The feature id: null, exactly what `PendingIntent.getBroadcast` passes.
    data.write_string16_opt(None).expect("featureId");
    data.write_strong_binder_opt(None); // activity token: null
    data.write_string16_opt(None).expect("who");
    data.write_int32(3); // requestCode
    data.write_int32(1); // one intent
    data.write_int32(1); // present
    let intent = acos_services::ams::IntentFacts::action("dev.acos.fixtures.procb.PI");
    write_payload(&mut data, &intent.to_parcelable()).expect("intent");
    data.write_int32(-1); // resolvedTypes: null
    data.write_int32(acos_services::pending_intent::FLAG_IMMUTABLE);
    data.write_int32(0); // options
    data.write_int32(0); // userId
    let mut reply = transact(&mut host, GET_INTENT_SENDER_WITH_FEATURE, data);
    acos_parcel::registry::pop_calling_identity();
    assert_eq!(reply.read_int32().expect("exception header"), 0);
    let sender = reply
        .read_strong_binder()
        .expect("the reply parses")
        .expect("a sender");
    assert_eq!(
        pending_intent_info_as(&mut host, UID_A, 4101, sender.0),
        (PACKAGE_B.to_string(), UID_B),
        "the creator package is the package argument, not the feature id"
    );
}

/// A non-sender binder answers the framework's "no information" shape
/// (`PendingIntentInfo(null, 0, false, 0)`) rather than a guessed creator: a
/// reply that invented `FIXTURE_UID` here would be the same false claim in
/// another place.
#[test]
fn a_binder_that_is_not_a_sender_has_no_creator() {
    let mut host = installed_host();
    let (package, uid) = pending_intent_info_as(&mut host, UID_A, 4101, 999_999);
    assert_eq!(package, String::new());
    assert_eq!(uid, 0);
}
