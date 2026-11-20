//! MODNOTIF4: the `notifications` module (item 3) and the `PendingIntent`
//! sender layer it taps through (item 2).
//!
//! Everything here drives the *wire*: the argument lists the framework's own
//! proxies write (`INotificationManager.enqueueNotificationWithTag`,
//! `IActivityManager.getIntentSender`) are transacted against the registered
//! services through `ServiceHost`, so the decode, the gate and the delivery run
//! the same paths the ART binder peer uses.

use acos_parcel::binder::BinderId;
use acos_parcel::Parcel;
use acos_services::module::{ModuleConfig, ModuleRegistry, Profile};
use acos_services::parcelable::write_payload;
use acos_services::value::{Parcelable, Value};
use acos_services::{Bundle, ServiceHost};

const NOTIFICATION_MANAGER: &str = "android.app.INotificationManager";
const ACTIVITY_MANAGER: &str = "android.app.IActivityManager";
/// `enqueueNotificationWithTag` (API-34 table, code 7).
const ENQUEUE_NOTIFICATION_WITH_TAG: u32 = 7;
/// `areNotificationsEnabled` (code 20).
const ARE_NOTIFICATIONS_ENABLED: u32 = 20;
/// `getIntentSender` (code 62).
const GET_INTENT_SENDER: u32 = 62;

const PACKAGE: &str = "dev.acos.fixtures.notifpermprobe";

/// An `android.app.Notification` with a title/text, a channel, an action and a
/// content `PendingIntent` over `sender`.
fn notification(sender: u64) -> Parcelable {
    let mut extras = Bundle::new();
    extras.put("android.title", Value::Str(Some("Probe".into())));
    extras.put("android.text", Value::Str(Some("posted".into())));
    let icon = Parcelable::ordered(
        "android.graphics.drawable.Icon",
        vec![
            Value::Int(2), // TYPE_RESOURCE
            Value::Str(Some(PACKAGE.into())),
            Value::Int(0x7f_01_0001),
            Value::Cond {
                present: false,
                values: vec![],
            },
            Value::Int(5), // BlendMode.SRC_IN
        ],
    );
    let pending = |binder: u64| Value::Cond {
        present: true,
        values: vec![Value::Parcelable(Parcelable::ordered(
            "android.app.PendingIntent",
            vec![Value::Binder(Some(BinderId(binder)))],
        ))],
    };
    let absent = || Value::Cond {
        present: false,
        values: vec![],
    };
    Parcelable::ordered(
        "android.app.Notification",
        vec![
            Value::Int(1),       // version
            Value::Binder(None), // mAllowlistToken
            Value::Long(1_700_000_000_000),
            Value::Long(1_700_000_000_000),
            Value::Cond {
                present: true,
                values: vec![Value::Parcelable(icon)],
            },
            Value::Int(1),    // number
            pending(sender),  // contentIntent
            absent(),         // deleteIntent
            absent(),         // tickerText
            absent(),         // tickerView
            absent(),         // contentView
            absent(),         // mLargeIcon
            Value::Int(0),    // defaults
            Value::Int(0x10), // flags
            absent(),         // sound
            Value::Int(0),    // audioStreamType
            absent(),         // audioAttributes
            Value::LongArray(None),
            Value::Int(0),
            Value::Int(0),
            Value::Int(0),
            Value::Int(0),
            absent(),      // fullScreenIntent
            Value::Int(0), // priority
            Value::Str8(None),
            Value::Str8(Some("fixture-group".into())),
            Value::Str8(None),
            Value::Bundle(Some(extras)),
            Value::List(Some(vec![Value::Parcelable(Parcelable::ordered(
                "android.app.Notification$Action",
                vec![
                    absent(),                         // icon
                    Value::Str(Some("Reply".into())), // title
                    pending(sender),                  // actionIntent
                    Value::Bundle(None),
                    Value::List(None),
                    Value::Int(1),
                    Value::Int(0),
                    Value::Int(0),
                    Value::Int(0),
                ],
            ))])),
            absent(),      // bigContentView
            absent(),      // headsUpContentView
            Value::Int(1), // visibility
            absent(),      // publicVersion
            Value::Int(0), // color
            Value::Cond {
                present: true,
                values: vec![Value::Str8(Some("fixture-channel".into()))],
            },
            Value::Long(0), // mTimeout
            absent(),       // mShortcutId
            absent(),       // mLocusId
            Value::Int(0),  // mBadgeIcon
            absent(),       // mSettingsText
            Value::Int(0),  // mGroupAlertBehavior
            absent(),       // mBubbleMetadata
            Value::Bool(false),
            Value::Int(0),
            Value::List(None), // allPendingIntents
        ],
    )
}

fn enqueue_request(sender: u64, id: i32) -> Parcel {
    let mut data = Parcel::new();
    data.write_interface_token(NOTIFICATION_MANAGER)
        .expect("token");
    data.write_string16_opt(Some(PACKAGE)).expect("pkg");
    data.write_string16_opt(None).expect("opPkg");
    data.write_string16_opt(Some("probe-tag")).expect("tag");
    data.write_int32(id);
    data.write_int32(1); // writeTypedObject presence
    write_payload(&mut data, &notification(sender)).expect("encode Notification");
    data.write_int32(0); // userId
    data.set_data_position(0);
    data
}

fn binder_of(host: &mut ServiceHost, name: &str) -> BinderId {
    host.check_service(name)
        .expect("lookup is infallible")
        .unwrap_or_else(|| panic!("{name} did not resolve"))
}

fn transact(host: &mut ServiceHost, name: &str, code: u32, mut data: Parcel) -> Parcel {
    // The framework's proxy leaves the position at the end; the dispatcher reads
    // from the start (`libbinder` rewinds before the call).
    data.set_data_position(0);
    let binder = binder_of(host, name);
    let mut reply = Parcel::new();
    host.transact(binder, code, &mut data, &mut reply, 0)
        .unwrap_or_else(|error| panic!("{name} code {code} failed: {error}"));
    reply.set_data_position(0);
    reply
}

/// A session host with core installed and the notifications module attached.
fn installed_session() -> (ServiceHost, ModuleRegistry) {
    let mut host = ServiceHost::new();
    acos_services::registry::install(&mut host);
    let mut registry = ModuleRegistry::standard();
    // The gate reads the permissions module's store, so a session that cares
    // about POST_NOTIFICATIONS attaches both.
    registry
        .attach(
            &mut host,
            "system.permissions",
            Profile::Real,
            &ModuleConfig::default(),
        )
        .expect("the permissions module attaches");
    let config = ModuleConfig::from_json(
        "system.notifications",
        &serde_json::json!({"package": PACKAGE, "target_sdk": 34}),
    )
    .expect("config");
    registry
        .attach(&mut host, "system.notifications", Profile::Real, &config)
        .expect("the notifications module attaches");
    (host, registry)
}

#[test]
fn a_real_notify_request_posts_through_the_module_and_the_gate_follows_the_permission() {
    let (mut host, mut registry) = installed_session();

    // The gate reads the session's one permission table. Grant
    // POST_NOTIFICATIONS through the permissions module (the same store
    // `IPermissionManager` and both `checkPermission` paths read).
    registry
        .control(
            &mut host,
            "system.permissions",
            "grant",
            &serde_json::json!({"permission": "android.permission.POST_NOTIFICATIONS"}),
        )
        .expect("grant POST_NOTIFICATIONS");

    let reply = transact(
        &mut host,
        "notification",
        ENQUEUE_NOTIFICATION_WITH_TAG,
        enqueue_request(900, 42),
    );
    let mut reply = reply;
    assert_eq!(
        reply.read_int32().expect("exception header"),
        acos_services::exception::NO_EXCEPTION,
        "the module answered the notify request"
    );

    let state = registry
        .control(
            &mut host,
            "system.notifications",
            "list",
            &serde_json::json!({}),
        )
        .expect("list");
    let posted = state["posted"].as_array().expect("posted[]");
    assert_eq!(posted.len(), 1, "one notification is posted: {state}");
    assert_eq!(posted[0]["id"], 42);
    assert_eq!(posted[0]["tag"], "probe-tag");
    assert_eq!(posted[0]["title"], "Probe");
    assert_eq!(posted[0]["text"], "posted");
    assert_eq!(posted[0]["channel"], "fixture-channel");
    assert_eq!(posted[0]["content_intent_sender"], 900);
    assert_eq!(posted[0]["actions"][0]["title"], "Reply");
    assert_eq!(posted[0]["actions"][0]["sender"], 900);
    assert_eq!(state["notifications_enabled"], true);

    // `areNotificationsEnabled` over the wire.
    let mut data = Parcel::new();
    data.write_interface_token(NOTIFICATION_MANAGER)
        .expect("token");
    data.write_string16_opt(Some(PACKAGE)).expect("pkg");
    let mut reply = transact(&mut host, "notification", ARE_NOTIFICATIONS_ENABLED, data);
    assert_eq!(reply.read_int32().expect("exception"), 0);
    assert_eq!(reply.read_int32().expect("enabled"), 1);

    // Deny it: the next notify is dropped, not posted, and the app sees the
    // gate closed (AOSP drops silently, which is what an app can observe).
    registry
        .control(
            &mut host,
            "system.permissions",
            "deny",
            &serde_json::json!({"permission": "android.permission.POST_NOTIFICATIONS"}),
        )
        .expect("deny POST_NOTIFICATIONS");
    let mut reply = transact(
        &mut host,
        "notification",
        ENQUEUE_NOTIFICATION_WITH_TAG,
        enqueue_request(900, 43),
    );
    assert_eq!(
        reply.read_int32().expect("exception header"),
        acos_services::exception::NO_EXCEPTION,
        "a dropped notification is not an exception to the app"
    );
    let state = registry
        .control(
            &mut host,
            "system.notifications",
            "list",
            &serde_json::json!({}),
        )
        .expect("list");
    assert_eq!(
        state["posted"].as_array().expect("posted").len(),
        1,
        "the denied notification is not posted: {state}"
    );
    assert_eq!(state["notifications_enabled"], false);
    let dropped = state["dropped"].as_array().expect("dropped");
    assert_eq!(dropped.len(), 1);
    assert_eq!(dropped[0]["id"], 43);
    assert!(
        dropped[0]["reason"]
            .as_str()
            .unwrap_or_default()
            .contains("POST_NOTIFICATIONS"),
        "the drop names the permission: {state}"
    );
}

#[test]
fn tap_and_dismiss_drive_the_posted_notification() {
    let (mut host, mut registry) = installed_session();
    registry
        .control(
            &mut host,
            "system.permissions",
            "grant",
            &serde_json::json!({"permission": "android.permission.POST_NOTIFICATIONS"}),
        )
        .expect("grant");
    let mut reply = transact(
        &mut host,
        "notification",
        ENQUEUE_NOTIFICATION_WITH_TAG,
        enqueue_request(901, 7),
    );
    let _ = reply.read_int32().expect("exception");

    // The content sender is fired through the published delivery path, and a
    // sender this session never created (the synthetic binder 901) is refused
    // with a structured error — never reported as fired.
    let error = registry
        .control(
            &mut host,
            "system.notifications",
            "tap",
            &serde_json::json!({"id": 7}),
        )
        .expect_err("no such sender record exists on this host");
    assert_eq!(error.code(), "SERVICE_BAD_REQUEST", "{error}");
    assert!(
        error
            .to_string()
            .contains("not a live pending-intent sender"),
        "{error}"
    );

    // `dismiss` removes the record like a user swipe.
    let dismissed = registry
        .control(
            &mut host,
            "system.notifications",
            "dismiss",
            &serde_json::json!({"id": 7}),
        )
        .expect("dismiss");
    assert_eq!(dismissed["remaining"], 0);
    let state = registry
        .control(
            &mut host,
            "system.notifications",
            "list",
            &serde_json::json!({}),
        )
        .expect("list");
    assert_eq!(state["posted"].as_array().expect("posted").len(), 0);
}

#[test]
fn attaching_the_module_takes_the_name_over_and_detach_restores_core() {
    let mut host = ServiceHost::new();
    acos_services::registry::install(&mut host);
    let core_binder = binder_of(&mut host, "notification");
    let mut registry = ModuleRegistry::standard();
    registry
        .attach(
            &mut host,
            "system.permissions",
            Profile::Real,
            &ModuleConfig::default(),
        )
        .expect("the permissions module attaches");
    registry
        .control(
            &mut host,
            "system.permissions",
            "grant",
            &serde_json::json!({"permission": "android.permission.POST_NOTIFICATIONS"}),
        )
        .expect("grant POST_NOTIFICATIONS");
    registry
        .attach(
            &mut host,
            "system.notifications",
            Profile::Real,
            &ModuleConfig::default(),
        )
        .expect("attach");
    let module_binder = binder_of(&mut host, "notification");
    assert_ne!(
        core_binder, module_binder,
        "the module serves the name, core does not"
    );
    // The module answers `areNotificationsEnabled` from the gate (true with no
    // runtime table), which is not core's hard-coded `false`.
    let mut data = Parcel::new();
    data.write_interface_token(NOTIFICATION_MANAGER)
        .expect("token");
    data.write_string16_opt(Some(PACKAGE)).expect("pkg");
    let mut reply = transact(&mut host, "notification", ARE_NOTIFICATIONS_ENABLED, data);
    assert_eq!(reply.read_int32().expect("exception"), 0);
    assert_eq!(reply.read_int32().expect("enabled"), 1);

    registry
        .detach(&mut host, "system.notifications")
        .expect("detach");
    assert_eq!(
        binder_of(&mut host, "notification"),
        core_binder,
        "detach restores core's binder and factory"
    );
    let mut data = Parcel::new();
    data.write_interface_token(NOTIFICATION_MANAGER)
        .expect("token");
    data.write_string16_opt(Some(PACKAGE)).expect("pkg");
    let mut reply = transact(&mut host, "notification", ARE_NOTIFICATIONS_ENABLED, data);
    assert_eq!(reply.read_int32().expect("exception"), 0);
    assert_eq!(
        reply.read_int32().expect("enabled"),
        0,
        "core's answer is back"
    );
}

/// Item 2: `getIntentSender` answers a sender, and `send` replays the stored
/// intent through the real start path.
#[test]
fn get_intent_sender_returns_a_sender_whose_send_starts_the_activity() {
    let apk = std::path::Path::new("../../fixtures/build/apps/hello-views.apk");
    assert!(
        apk.exists(),
        "the hello-views fixture is built: {}",
        apk.display()
    );
    // The sender pool is registered by `registry::install`; the request carries
    // an explicit component intent for hello-views' launcher activity.
    // The same component table the launcher seeds for a live session.
    let mut records = acos_services::ams::AmsState::new();
    records
        .manifests
        .add_apk(apk, acos_services::pms::FIXTURE_UID)
        .expect("manifest");
    let mut table = acos_services::pending_intent::SenderTable::default();
    table.bind_nodes(vec![500]);
    let intent = acos_services::ams::IntentFacts::explicit(
        "dev.acos.fixtures.helloviews",
        "dev.acos.fixtures.helloviews.MainActivity",
    );
    let binder = table
        .get_intent_sender(
            acos_services::pending_intent::TYPE_ACTIVITY,
            PACKAGE,
            0,
            acos_services::pending_intent::FLAG_IMMUTABLE,
            vec![intent],
            vec![None],
            None,
        )
        .expect("getIntentSender")
        .expect("a sender binder");
    assert_eq!(binder, 500);
    records.intent_senders = table;

    let evidence = records.send_pending_intent(binder, 0).expect("send");
    assert!(
        evidence.outcome.is_ok(),
        "the tap delivery reports the start result: {evidence:?}"
    );
    assert!(
        !records.pending_launches.is_empty(),
        "the sender's send queued the target activity's launch"
    );
    assert_eq!(
        records.new_intents.len() + records.pending_launches.len(),
        1
    );
    // The evidence is on the record, so `tap` can report what it fired.
    let record = records.intent_senders.record(binder).expect("record");
    assert_eq!(record.sends.len(), 1);
    assert_eq!(record.sends[0].code, 0);
    // A cancel refuses the next send instead of starting it again.
    assert!(records.intent_senders.cancel(binder));
    let evidence = records.send_pending_intent(binder, 0).expect("send");
    assert!(
        evidence.refused_cancelled,
        "a cancelled sender refuses: {evidence:?}"
    );
    assert_eq!(records.pending_launches.len(), 1);
}

#[test]
fn get_intent_sender_decodes_the_framework_wire_form() {
    let mut host = ServiceHost::new();
    acos_services::registry::install(&mut host);
    let mut data = Parcel::new();
    data.write_interface_token(ACTIVITY_MANAGER).expect("token");
    data.write_int32(1); // INTENT_SENDER_ACTIVITY
    data.write_string16_opt(Some(PACKAGE)).expect("package");
    data.write_strong_binder_opt(None); // activity token: null
    data.write_string16_opt(None).expect("who");
    data.write_int32(11); // requestCode
                          // writeTypedArray(Intent[]): count, then each element's presence + payload.
    data.write_int32(1);
    data.write_int32(1);
    let intent = acos_services::ams::IntentFacts::explicit(
        "dev.acos.fixtures.helloviews",
        "dev.acos.fixtures.helloviews.MainActivity",
    );
    write_payload(&mut data, &intent.to_parcelable()).expect("intent");
    data.write_int32(-1); // resolvedTypes: null String[]
    data.write_int32(acos_services::pending_intent::FLAG_IMMUTABLE);
    data.write_int32(0); // options: null Bundle
    data.write_int32(0); // userId
    data.set_data_position(0);

    let mut reply = transact(&mut host, "activity", GET_INTENT_SENDER, data);
    assert_eq!(reply.read_int32().expect("exception header"), 0);
    let sender = reply.read_int32().expect("binder");
    assert!(
        sender > 0,
        "getIntentSender answers a non-null IIntentSender binder, got {sender}"
    );
    // The same key returns the same sender (`PendingIntent` identity).
    let mut data = Parcel::new();
    data.write_interface_token(ACTIVITY_MANAGER).expect("token");
    data.write_int32(1);
    data.write_string16_opt(Some(PACKAGE)).expect("package");
    data.write_strong_binder_opt(None);
    data.write_string16_opt(None).expect("who");
    data.write_int32(11);
    data.write_int32(1);
    data.write_int32(1);
    write_payload(&mut data, &intent.to_parcelable()).expect("intent");
    data.write_int32(-1);
    data.write_int32(acos_services::pending_intent::FLAG_IMMUTABLE);
    data.write_int32(0);
    data.write_int32(0);
    data.set_data_position(0);
    let mut reply = transact(&mut host, "activity", GET_INTENT_SENDER, data);
    assert_eq!(reply.read_int32().expect("exception header"), 0);
    assert_eq!(
        reply.read_int32().expect("binder"),
        sender,
        "an equal key returns the same sender"
    );
}
