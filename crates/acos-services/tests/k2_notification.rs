//! K2/R2: notification channel parcels from the API-34 image.
//!
//! The K1 corpus showed `IllegalArgumentException: bad request for
//! android.app.INotificationManager::createNotificationChannels` for four apps
//! and missing get-channel transactions for two more. The create argument is a
//! `ParceledListSlice<NotificationChannel>` whose element layout is the image's
//! `NotificationChannel(Parcel)` constructor (every nullable member is a
//! `writeByte` presence marker). These tests drive the full service-path
//! round-trip without the corpus: create -> store -> get -> encode -> decode.

use acos_services::declared::{NotificationService, NotificationState};
use acos_services::parcelable::{parceled_list_items, parceled_list_of, read_payload};
use acos_services::value::{Parcelable, Value};
use acos_services::ServiceObject;

fn method(interface: &str, name: &str) -> &'static acos_aidl::MethodInfo {
    acos_aidl::lookup_by_name(interface, name).expect("AIDL table entry")
}

fn channel(id: &str, group: Option<&str>) -> Parcelable {
    Parcelable::ordered(
        "android.app.NotificationChannel",
        vec![
            Value::Cond { present: true, values: vec![Value::Str(Some(id.into()))] },
            Value::Cond { present: true, values: vec![Value::Str(Some("Messages".into()))] },
            Value::Cond { present: false, values: Vec::new() },
            Value::Int(3),
            Value::Byte(0),
            Value::Int(-1000),
            Value::Cond { present: false, values: Vec::new() },
            Value::Byte(0),
            Value::LongArray(None),
            Value::Int(0),
            Value::Byte(0),
            Value::Byte(1),
            Value::Byte(1),
            Value::Byte(0),
            Value::Cond {
                present: group.is_some(),
                values: group.map(|g| vec![Value::Str(Some(g.into()))]).unwrap_or_default(),
            },
            Value::Cond { present: false, values: Vec::new() },
            Value::Int(0),
            Value::Bool(false),
            Value::Int(0),
            Value::Int(3),
            Value::Str(None),
            Value::Str(None),
            Value::Bool(false),
            Value::Bool(false),
            Value::Long(0),
            Value::Bool(false),
        ],
    )
}

fn service() -> NotificationService {
    NotificationService::new(std::sync::Arc::new(std::sync::Mutex::new(
        NotificationState::new(),
    )))
}

fn create(service: &mut NotificationService, package: &str, channels: Vec<Parcelable>) {
    let arg = Value::TypedObject(parceled_list_of(
        channels.into_iter().map(Value::Parcelable).collect(),
    ));
    service
        .call(
            "android.app.INotificationManager",
            method("android.app.INotificationManager", "createNotificationChannels"),
            &[Value::Str(Some(package.into())), arg],
        )
        .expect("createNotificationChannels decodes and succeeds");
}

/// A channel that sets an `AudioAttributes` exercises the image's
/// `AudioAttributes.writeToParcel` order (5 ints, StringArray, nullable
/// string, bundle marker, Bundle). Getting that order wrong desynchronised the
/// rest of the channel and was the K1 "bad string: String16 is not NUL
/// terminated" failure.
#[test]
fn a_channel_with_audio_attributes_round_trips() {
    let mut service = service();
    let mut payload = channel("alerts", Some("chat"));
    if let Parcelable::Ordered { values, .. } = &mut payload {
        values[15] = Value::Cond {
            present: true,
            values: vec![Value::Parcelable(Parcelable::ordered(
                "android.media.AudioAttributes",
                vec![
                    Value::Int(0),
                    Value::Int(0),
                    Value::Int(5),
                    Value::Int(0),
                    Value::Int(0),
                    Value::StringArray(None),
                    Value::Str(None),
                    Value::Int(0),
                    Value::Bundle(None),
                ],
            ))],
        };
    }
    create(&mut service, "pkg", vec![payload]);
    let reply = service
        .call(
            "android.app.INotificationManager",
            method("android.app.INotificationManager", "getNotificationChannel"),
            &[
                Value::Str(Some("pkg".into())),
                Value::Int(0),
                Value::Str(Some("alerts".into())),
                Value::Str(None),
            ],
        )
        .expect("get")
        .reply;
    assert_eq!(
        reply[0].as_parcelable().and_then(|p| p.field(0)).and_then(|v| match v {
            Value::Cond { values, .. } => values[0].as_str(),
            _ => None,
        }),
        Some("alerts")
    );
}

#[test]
fn create_then_get_returns_the_same_channel() {
    let mut service = service();
    create(&mut service, "pkg", vec![channel("messages", Some("chat"))]);
    let reply = service
        .call(
            "android.app.INotificationManager",
            method("android.app.INotificationManager", "getNotificationChannel"),
            &[
                Value::Str(Some("pkg".into())),
                Value::Int(0),
                Value::Str(Some("messages".into())),
                Value::Str(None),
            ],
        )
        .expect("getNotificationChannel")
        .reply;
    let got = reply[0].as_parcelable().expect("a channel");
    assert_eq!(got.field(0).and_then(|v| match v {
        Value::Cond { values, .. } => values[0].as_str(),
        _ => None,
    }), Some("messages"));
}

#[test]
fn created_channel_list_is_a_parceled_list_slice_with_the_image_layout() {
    let mut service = service();
    create(&mut service, "pkg", vec![channel("messages", Some("chat"))]);
    let reply = service
        .call(
            "android.app.INotificationManager",
            method("android.app.INotificationManager", "getNotificationChannels"),
            &[
                Value::Str(Some("pkg".into())),
                Value::Str(None),
                Value::Int(0),
            ],
        )
        .expect("getNotificationChannels")
        .reply;
    let slice = reply[0].as_parcelable().expect("a ParceledListSlice");
    let items = parceled_list_items(slice).expect("slice items");
    assert_eq!(items.len(), 1);

    // Encode the typed-object reply exactly as the host does and decode it with
    // the same reader the framework's `NotificationManager` uses.
    let mut parcel = acos_parcel::Parcel::new();
    acos_services::codec::write_reply(
        &mut parcel,
        &method("android.app.INotificationManager", "getNotificationChannels").reply,
        &reply,
        "android.app.INotificationManager",
        "getNotificationChannels",
    )
    .expect("reply encodes");
    parcel.set_data_position(0);
    assert_eq!(parcel.read_int32().expect("typed presence"), 1);
    let decoded = read_payload("android.content.pm.ParceledListSlice", &mut parcel)
        .expect("ParceledListSlice decodes");
    let decoded_items = parceled_list_items(&decoded).expect("decoded items");
    assert_eq!(decoded_items.len(), 1);
    assert_eq!(
        decoded_items[0]
            .as_parcelable()
            .and_then(|p| p.field(0))
            .and_then(|v| match v {
                Value::Cond { values, .. } => values[0].as_str(),
                _ => None,
            }),
        Some("messages")
    );
}

#[test]
fn an_unknown_channel_is_null_not_an_error() {
    let mut service = service();
    let reply = service
        .call(
            "android.app.INotificationManager",
            method("android.app.INotificationManager", "getNotificationChannel"),
            &[
                Value::Str(Some("pkg".into())),
                Value::Int(0),
                Value::Str(Some("missing".into())),
                Value::Str(None),
            ],
        )
        .expect("getNotificationChannel")
        .reply;
    assert!(matches!(
        &reply[0],
        Value::TypedObject(Parcelable::Null)
    ));
}
