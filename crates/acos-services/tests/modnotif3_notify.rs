//! MODNOTIF3: a real `NotificationManager.notify()` request decodes.
//!
//! `INotificationManager.enqueueNotificationWithTag(pkg, opPkg, tag, id,
//! in Notification, userId)` (`INotificationManager.aidl:58`) carries a whole
//! `android.app.Notification`. Before the MODNOTIF3 codec, the
//! `acos-aidl` table entry (`tables.rs:6324`, a `typed_object`) decoded the
//! presence marker and then failed with `SERVICE_UNSUPPORTED_PARCELABLE`
//! (`android.app.Notification` had no spec) — the exact MODNOTIF2 §4 blocker.
//!
//! This test drives the *wire*: it writes the argument list the framework's
//! proxy writes (interface token, three string16s, int, presence marker +
//! payload, int) and transacts it against the registered
//! `android.app.INotificationManager`, so the decode runs through the same
//! `ServiceHost` path the ART binder peer uses.

use acos_parcel::binder::BinderId;
use acos_parcel::Parcel;
use acos_services::declared::{NotificationService, NotificationState};
use acos_services::parcelable::write_payload;
use acos_services::value::{Parcelable, Value};
use acos_services::{Bundle, ServiceHost};
use std::sync::{Arc, Mutex};

const NOTIFICATION_MANAGER: &str = "android.app.INotificationManager";
/// `enqueueNotificationWithTag` in the API-34 table.
const ENQUEUE_NOTIFICATION_WITH_TAG: u32 = 7;

fn notification() -> Parcelable {
    let mut extras = Bundle::new();
    extras.put("android.title", Value::Str(Some("Probe".into())));
    extras.put("android.text", Value::Str(Some("posted".into())));
    let icon = Parcelable::ordered(
        "android.graphics.drawable.Icon",
        vec![
            Value::Int(2), // TYPE_RESOURCE
            Value::Str(Some("dev.acos.fixtures.notifpermprobe".into())),
            Value::Int(0x7f_01_0001),
            Value::Bool(false),
            Value::Float(1.0),
            Value::Cond { present: false, values: vec![] },
            Value::Int(0),
        ],
    );
    Parcelable::ordered(
        "android.app.Notification",
        vec![
            Value::Int(1),       // version
            Value::Binder(None), // mAllowlistToken
            Value::Long(1_700_000_000_000),
            Value::Long(1_700_000_000_000),
            Value::Cond { present: true, values: vec![Value::Parcelable(icon)] },
            Value::Int(1), // number
            Value::Cond {
                present: true,
                values: vec![Value::Parcelable(Parcelable::ordered(
                    "android.app.PendingIntent",
                    vec![Value::Binder(Some(BinderId(11)))],
                ))],
            },
            Value::Cond { present: false, values: vec![] }, // deleteIntent
            Value::Cond { present: false, values: vec![] }, // tickerText
            Value::Cond { present: false, values: vec![] }, // tickerView
            Value::Cond { present: false, values: vec![] }, // contentView
            Value::Cond { present: false, values: vec![] }, // mLargeIcon
            Value::Int(0),
            Value::Int(0x10),
            Value::Cond { present: false, values: vec![] }, // sound
            Value::Int(0),
            Value::Cond { present: false, values: vec![] }, // audioAttributes
            Value::LongArray(None),
            Value::Int(0),
            Value::Int(0),
            Value::Int(0),
            Value::Int(0),
            Value::Cond { present: false, values: vec![] }, // fullScreenIntent
            Value::Int(0),
            Value::Str8(None), // category
            Value::Str8(None), // mGroupKey
            Value::Str8(None), // mSortKey
            Value::Bundle(Some(extras)),
            Value::List(Some(vec![Value::Parcelable(Parcelable::ordered(
                "android.app.Notification$Action",
                vec![
                    Value::Cond { present: false, values: vec![] },
                    Value::Str(Some("Reply".into())),
                    Value::Cond { present: false, values: vec![] },
                    Value::Bundle(None),
                    Value::List(None),
                    Value::Int(1),
                    Value::Int(0),
                    Value::Int(0),
                    Value::Int(0),
                ],
            ))])),
            Value::Cond { present: false, values: vec![] }, // bigContentView
            Value::Cond { present: false, values: vec![] }, // headsUpContentView
            Value::Int(1),
            Value::Cond { present: false, values: vec![] }, // publicVersion
            Value::Int(0),
            Value::Cond {
                present: true,
                values: vec![Value::Str8(Some("probe-channel".into()))],
            },
            Value::Long(0),
            Value::Cond { present: false, values: vec![] }, // mShortcutId
            Value::Cond { present: false, values: vec![] }, // mLocusId
            Value::Int(0),
            Value::Cond { present: false, values: vec![] }, // mSettingsText
            Value::Int(0),
            Value::Cond { present: false, values: vec![] }, // mBubbleMetadata
            Value::Bool(false),
            Value::Int(0),
            Value::Int(-1), // allPendingIntents
        ],
    )
}

/// The proxy's request bytes for `enqueueNotificationWithTag`.
fn enqueue_request() -> Parcel {
    let mut data = Parcel::new();
    data.write_interface_token(NOTIFICATION_MANAGER).expect("token");
    data.write_string16_opt(Some("dev.acos.fixtures.notifpermprobe"))
        .expect("pkg");
    data.write_string16_opt(None).expect("opPkg");
    data.write_string16_opt(Some("probe-tag")).expect("tag");
    data.write_int32(42);
    // `writeTypedObject(notification, 0)`: the presence marker, then the payload.
    data.write_int32(1);
    write_payload(&mut data, &notification()).expect("encode Notification");
    data.write_int32(0); // userId
    data.set_data_position(0);
    data
}

#[test]
fn a_real_notify_request_decodes_into_the_service() {
    let state = Arc::new(Mutex::new(NotificationState::new()));
    let mut host = ServiceHost::new();
    host.set_notification_state(Arc::clone(&state));
    let service_state = Arc::clone(&state);
    let binder = host.register("notification", NOTIFICATION_MANAGER, move |_w| {
        Box::new(NotificationService::new(Arc::clone(&service_state))) as Box<dyn acos_services::ServiceObject>
    });

    let mut data = enqueue_request();
    let mut reply = Parcel::new();
    host.transact(
        binder,
        ENQUEUE_NOTIFICATION_WITH_TAG,
        &mut data,
        &mut reply,
        0,
    )
    .expect("a real Notification decodes (no UnsupportedParcelable)");
    reply.set_data_position(0);
    assert_eq!(
        reply.read_int32().expect("exception header"),
        acos_services::exception::NO_EXCEPTION,
        "the service answered without an exception"
    );

    let posted = state.lock().unwrap().posted.clone();
    assert_eq!(
        posted,
        vec![("dev.acos.fixtures.notifpermprobe".to_string(), 42)],
        "the decoded (package, id) reached the INotificationManager state"
    );
}

/// The other half of the same codec: the *reply* direction. A notification
/// re-encoded from the decoded value is byte-identical, so a module can hand a
/// posted notification back through `getActiveNotifications`.
#[test]
fn the_decoded_notification_re_encodes_byte_for_byte() {
    let value = notification();
    let mut first = Parcel::new();
    write_payload(&mut first, &value).expect("encode");
    let bytes = first.marshall();
    first.set_data_position(0);
    let decoded =
        acos_services::parcelable::read_payload("android.app.Notification", &mut first).expect("decode");
    assert_eq!(decoded, value);
    let mut second = Parcel::new();
    write_payload(&mut second, &decoded).expect("re-encode");
    assert_eq!(second.marshall(), bytes);
}
