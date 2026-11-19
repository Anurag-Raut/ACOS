//! `media.session`: the wire formats against the image's own framework dex, and
//! the session lifecycle end to end at the service-host level.
//!
//! The app half (a real `MediaSession`, `MediaController` and
//! `MediaSession.Callback`) is proven on ART by `scenarios/modfill-media.yaml`.

use acos_parcel::{BinderId, Parcel};
use acos_services::module::{ModuleConfig, ModuleRegistry, Profile};
use acos_services::parcelable::{self, Scanner};
use acos_services::value::{Bundle, Parcelable, Value as PV};
use acos_services::{ServiceError, ServiceHost};
use serde_json::json;

const MODULE: &str = "media.session";
const MANAGER: &str = "android.media.session.ISessionManager";
const SESSION: &str = "android.media.session.ISession";
const CONTROLLER: &str = "android.media.session.ISessionController";

#[test]
fn the_media_parcelables_account_for_every_write_of_the_image_dex() {
    for class in [
        "android.media.MediaMetadata",
        "android.media.session.PlaybackState",
        "android.media.session.PlaybackState$CustomAction",
    ] {
        let spec = parcelable::spec_for(class).unwrap_or_else(|| panic!("no codec row for {class}"));
        let scanned = Scanner::write_to_parcel_boot(class)
            .unwrap_or_else(|error| panic!("{class} is not in the image's boot jars: {error}"));
        let verdict = parcelable::verify(spec, &scanned);
        assert!(
            verdict.is_full(),
            "{class}: the spec consumes {}/{} of the image's calls {:?}",
            verdict.matched_calls,
            verdict.real_calls,
            scanned.calls
        );
    }
}

fn playback_state(state: i32, position: i64, actions: i64) -> Parcelable {
    Parcelable::ordered(
        "android.media.session.PlaybackState",
        vec![
            PV::Int(state),
            PV::Long(position),
            PV::Float(1.0),
            PV::Long(1_000),
            PV::Long(0),
            PV::Long(actions),
            PV::List(Some(Vec::new())),
            PV::Long(-1),
            PV::Str(None),
            PV::Bundle(None),
        ],
    )
}

fn metadata(title: &str, artist: &str, duration: i64) -> Parcelable {
    let mut bundle = Bundle::new();
    bundle.put("android.media.metadata.TITLE", PV::Str(Some(title.to_string())));
    bundle.put(
        "android.media.metadata.ARTIST",
        PV::Str(Some(artist.to_string())),
    );
    bundle.put("android.media.metadata.DURATION", PV::Long(duration));
    Parcelable::ordered(
        "android.media.MediaMetadata",
        vec![PV::Bundle(Some(bundle)), PV::Int(1_048_576)],
    )
}

fn token(descriptor: &str) -> Parcel {
    let mut parcel = Parcel::new();
    parcel.write_interface_token(descriptor).expect("token");
    parcel
}

fn transact(host: &mut ServiceHost, binder: BinderId, code: u32, mut data: Parcel) -> Parcel {
    data.set_data_position(0);
    let mut reply = Parcel::new();
    host.transact(binder, code, &mut data, &mut reply, 0)
        .unwrap_or_else(|error| panic!("code {code} failed: {error}"));
    reply.set_data_position(0);
    assert_eq!(reply.read_int32().expect("exception header"), 0, "no exception");
    reply
}

fn transact_err(host: &mut ServiceHost, binder: BinderId, code: u32, mut data: Parcel) -> ServiceError {
    data.set_data_position(0);
    let mut reply = Parcel::new();
    host.transact(binder, code, &mut data, &mut reply, 0)
        .expect_err("the call must fail")
}

fn start() -> (ServiceHost, ModuleRegistry) {
    let mut host = ServiceHost::standard();
    let mut registry = ModuleRegistry::standard();
    registry
        .attach(&mut host, MODULE, Profile::Scripted, &ModuleConfig::default())
        .expect("media.session attaches");
    (host, registry)
}

/// The app's `ISessionCallback` binder (a guest node).
fn guest(wire: u64) -> BinderId {
    BinderId((1 << 32) | wire)
}

struct Created {
    session: BinderId,
}

fn create_session(host: &mut ServiceHost, callback: BinderId, tag: &str) -> Created {
    let manager = host.check_service("media_session").unwrap().unwrap();
    let mut data = token(MANAGER);
    data.write_string16("dev.acos.fixtures.mediaprobe").unwrap();
    data.write_strong_binder(callback);
    data.write_string16(tag).unwrap();
    data.write_int32(0); // sessionInfo: null Bundle
    data.write_int32(0); // userId
    let mut reply = transact(host, manager, 1, data);
    let session = reply.read_strong_binder().unwrap().expect("an ISession binder");
    Created { session }
}

fn controller_of(host: &mut ServiceHost, session: BinderId) -> BinderId {
    let mut reply = transact(host, session, 2, token(SESSION));
    reply.read_strong_binder().unwrap().expect("an ISessionController")
}

#[test]
fn a_session_publishes_state_and_a_controller_reads_the_frameworks_own_bytes_back() {
    let (mut host, mut registry) = start();
    let created = create_session(&mut host, guest(41), "acos-music");
    let controller = controller_of(&mut host, created.session);

    let mut data = token(SESSION);
    data.write_bool(true);
    transact(&mut host, created.session, 4, data); // setActive(true)

    // setMetadata(MediaMetadata, long duration, String description)
    let mut data = token(SESSION);
    data.write_int32(1);
    parcelable::write_payload(&mut data, &metadata("Blue in Green", "Miles Davis", 337_000)).unwrap();
    data.write_int64(337_000);
    data.write_string16("desc").unwrap();
    transact(&mut host, created.session, 9, data);

    // setPlaybackState(PlaybackState)
    let state = playback_state(3, 12_000, 0x1 | 0x2 | 0x20 | 0x10);
    let mut data = token(SESSION);
    data.write_int32(1);
    parcelable::write_payload(&mut data, &state).unwrap();
    transact(&mut host, created.session, 10, data);

    // The agent reads what the app published.
    let sessions = registry
        .control(&mut host, MODULE, "sessions", &json!({}))
        .unwrap();
    let session = &sessions["sessions"][0];
    assert_eq!(session["tag"], "acos-music");
    assert_eq!(session["package"], "dev.acos.fixtures.mediaprobe");
    assert_eq!(session["active"], true);
    assert_eq!(session["playback"]["state"], "PLAYING");
    assert_eq!(session["playback"]["position"], 12_000);
    assert_eq!(session["metadata"]["TITLE"], "Blue in Green");
    assert_eq!(session["metadata"]["ARTIST"], "Miles Davis");
    assert_eq!(session["metadata"]["DURATION"], 337_000);

    // A controller (any app) gets the app's bytes back unchanged.
    let mut reply = transact(&mut host, controller, 33, token(CONTROLLER));
    assert_eq!(reply.read_int32().unwrap(), 1, "PlaybackState present");
    let back = parcelable::read_payload("android.media.session.PlaybackState", &mut reply).unwrap();
    assert_eq!(back, state);
    assert_eq!(reply.data_position(), reply.data_size());
    let mut reply = transact(&mut host, controller, 32, token(CONTROLLER));
    assert_eq!(reply.read_int32().unwrap(), 1, "MediaMetadata present");
    let back = parcelable::read_payload("android.media.MediaMetadata", &mut reply).unwrap();
    // `BaseBundle` writes its `ArrayMap` in key order, so compare as a map.
    let sorted = |parcelable: &Parcelable| {
        let Parcelable::Ordered { values, .. } = parcelable else {
            panic!("not ordered")
        };
        let PV::Bundle(Some(bundle)) = &values[0] else {
            panic!("no bundle")
        };
        let mut entries = bundle.entries.clone();
        entries.sort_by(|a, b| a.0.cmp(&b.0));
        (entries, values[1].clone())
    };
    assert_eq!(
        sorted(&back),
        sorted(&metadata("Blue in Green", "Miles Davis", 337_000))
    );

    // getPackageName / getTag
    let mut reply = transact(&mut host, controller, 5, token(CONTROLLER));
    assert_eq!(
        reply.read_string16().unwrap().as_deref(),
        Some("dev.acos.fixtures.mediaprobe")
    );
    let mut reply = transact(&mut host, controller, 6, token(CONTROLLER));
    assert_eq!(reply.read_string16().unwrap().as_deref(), Some("acos-music"));
}

#[test]
fn a_session_with_no_state_answers_null_not_an_invented_state() {
    let (mut host, _registry) = start();
    let created = create_session(&mut host, guest(41), "empty");
    let controller = controller_of(&mut host, created.session);
    for code in [32, 33] {
        let mut reply = transact(&mut host, controller, code, token(CONTROLLER));
        assert_eq!(reply.read_int32().unwrap(), 0, "code {code}: null");
    }
}

#[test]
fn agent_commands_reach_the_apps_session_callback_with_the_agent_as_caller() {
    let (mut host, mut registry) = start();
    let callback = guest(41);
    let created = create_session(&mut host, callback, "acos-music");
    let _ = created;

    for (op, code, method) in [
        ("play", 8u32, "onPlay"),
        ("pause", 13, "onPause"),
        ("next", 15, "onNext"),
        ("previous", 16, "onPrevious"),
        ("stop", 14, "onStop"),
        ("fast_forward", 17, "onFastForward"),
        ("rewind", 18, "onRewind"),
    ] {
        let reply = registry
            .control(&mut host, MODULE, op, &json!({}))
            .unwrap_or_else(|error| panic!("{op}: {error}"));
        assert_eq!(reply["callback"], method);
        let deliveries = host.take_deliveries();
        assert_eq!(deliveries.len(), 1, "{op}: one delivery");
        let delivery = &deliveries[0];
        assert_eq!(delivery.target, callback);
        assert_eq!(delivery.interface, "android.media.session.ISessionCallback");
        assert_eq!(delivery.code, code, "{op}: ISessionCallback code");
        let mut data = delivery.data.clone();
        data.set_data_position(0);
        data.enforce_interface("android.media.session.ISessionCallback")
            .unwrap();
        assert_eq!(
            data.read_string16().unwrap().as_deref(),
            Some("dev.acos.agent"),
            "the caller package MediaSession.Callback sees"
        );
        assert_eq!(data.read_int32().unwrap(), 0, "pid");
        assert_eq!(data.read_int32().unwrap(), 2000, "uid");
        assert_eq!(data.data_position(), data.data_size(), "{op}: nothing else on the wire");
    }

    // seek / skip_to_item carry a long; custom_action a string and a null Bundle.
    registry
        .control(&mut host, MODULE, "seek", &json!({"position": 90_000}))
        .unwrap();
    let delivery = host.take_deliveries().remove(0);
    assert_eq!(delivery.code, 19);
    let mut data = delivery.data.clone();
    data.set_data_position(0);
    data.enforce_interface("android.media.session.ISessionCallback")
        .unwrap();
    data.read_string16().unwrap();
    data.read_int32().unwrap();
    data.read_int32().unwrap();
    assert_eq!(data.read_int64().unwrap(), 90_000);
    assert_eq!(data.data_position(), data.data_size());

    registry
        .control(&mut host, MODULE, "custom_action", &json!({"action": "like"}))
        .unwrap();
    let delivery = host.take_deliveries().remove(0);
    assert_eq!(delivery.code, 22);
    let mut data = delivery.data.clone();
    data.set_data_position(0);
    data.enforce_interface("android.media.session.ISessionCallback")
        .unwrap();
    data.read_string16().unwrap();
    data.read_int32().unwrap();
    data.read_int32().unwrap();
    assert_eq!(data.read_string16().unwrap().as_deref(), Some("like"));
    assert_eq!(data.read_int32().unwrap(), 0, "null Bundle");
    assert_eq!(data.data_position(), data.data_size());

    let sessions = registry
        .control(&mut host, MODULE, "sessions", &json!({}))
        .unwrap();
    assert_eq!(
        sessions["sessions"][0]["commands"].as_array().unwrap().len(),
        9,
        "every delivered command (7 transport + seek + custom_action) is on the record"
    );
}

#[test]
fn a_media_controller_in_another_app_reaches_the_same_callback() {
    let (mut host, _registry) = start();
    let callback = guest(41);
    let created = create_session(&mut host, callback, "acos-music");
    let controller = controller_of(&mut host, created.session);
    let mut data = token(CONTROLLER);
    data.write_string16("com.other.app").unwrap();
    transact(&mut host, controller, 17, data); // play(packageName)
    let deliveries = host.take_deliveries();
    assert_eq!(deliveries.len(), 1);
    assert_eq!(deliveries[0].code, 8);
    assert_eq!(deliveries[0].target, callback);
    // The caller MediaSession.Callback sees is the controller's package, not the
    // agent; pid/uid are unknown (-1), not invented.
    let mut data = deliveries[0].data.clone();
    data.set_data_position(0);
    data.enforce_interface("android.media.session.ISessionCallback")
        .unwrap();
    assert_eq!(data.read_string16().unwrap().as_deref(), Some("com.other.app"));
    assert_eq!(data.read_int32().unwrap(), -1);
    assert_eq!(data.read_int32().unwrap(), -1);
}

#[test]
fn a_controller_callback_is_told_about_published_state() {
    let (mut host, _registry) = start();
    let created = create_session(&mut host, guest(41), "acos-music");
    let controller = controller_of(&mut host, created.session);
    let listener = guest(77);
    let mut data = token(CONTROLLER);
    data.write_string16("com.other.app").unwrap();
    data.write_strong_binder(listener);
    transact(&mut host, controller, 3, data); // registerCallback

    let state = playback_state(2, 5_000, 0x4);
    let mut data = token(SESSION);
    data.write_int32(1);
    parcelable::write_payload(&mut data, &state).unwrap();
    transact(&mut host, created.session, 10, data);

    let deliveries = host.take_deliveries();
    let update = deliveries
        .iter()
        .find(|d| d.target == listener)
        .expect("onPlaybackStateChanged reaches the controller's callback");
    assert_eq!(update.interface, "android.media.session.ISessionControllerCallback");
    assert_eq!(update.code, 3);
    let mut data = update.data.clone();
    data.set_data_position(0);
    data.enforce_interface("android.media.session.ISessionControllerCallback")
        .unwrap();
    assert_eq!(data.read_int32().unwrap(), 1);
    let back = parcelable::read_payload("android.media.session.PlaybackState", &mut data).unwrap();
    assert_eq!(back, state);
    assert_eq!(data.data_position(), data.data_size());

    // destroySession tells it too.
    transact(&mut host, created.session, 8, token(SESSION));
    let deliveries = host.take_deliveries();
    assert!(deliveries
        .iter()
        .any(|d| d.target == listener && d.code == 2 && d.method == "onSessionDestroyed"));
}

#[test]
fn a_destroyed_session_has_no_callback_to_reach() {
    let (mut host, mut registry) = start();
    let created = create_session(&mut host, guest(41), "acos-music");
    transact(&mut host, created.session, 8, token(SESSION));
    let error = registry
        .control(&mut host, MODULE, "play", &json!({}))
        .expect_err("no live session");
    assert!(error.to_string().contains("no such live session"), "{error}");
}

#[test]
fn a_stub_records_but_does_not_drive_and_unknown_things_are_refused() {
    let mut host = ServiceHost::standard();
    let mut registry = ModuleRegistry::standard();
    registry
        .attach(&mut host, MODULE, Profile::Stub, &ModuleConfig::default())
        .unwrap();
    create_session(&mut host, guest(41), "acos-music");
    registry
        .control(&mut host, MODULE, "sessions", &json!({}))
        .expect("a stub reads");
    let error = registry
        .control(&mut host, MODULE, "play", &json!({}))
        .expect_err("a stub does not drive");
    assert!(error.to_string().contains("scripted"), "{error}");
    assert!(registry
        .control(&mut host, MODULE, "volume", &json!({}))
        .is_err());
    // `seek` needs its argument.
    let (mut host2, mut registry2) = start();
    create_session(&mut host2, guest(41), "x");
    assert!(registry2
        .control(&mut host2, MODULE, "seek", &json!({}))
        .is_err());
    // Config is refused rather than ignored.
    let mut host3 = ServiceHost::standard();
    let mut registry3 = ModuleRegistry::standard();
    let config = ModuleConfig::from_json(MODULE, &json!({"ssid": "x"})).unwrap();
    assert!(registry3
        .attach(&mut host3, MODULE, Profile::Scripted, &config)
        .is_err());
}

#[test]
fn unmodelled_calls_and_the_router_are_structured_unsupported() {
    let (mut host, mut registry) = start();
    let manager = host.check_service("media_session").unwrap().unwrap();
    // getSessions (2), dispatchMediaKeyEvent (5), addSessionsListener (10)
    for code in [2u32, 5, 10] {
        let error = transact_err(&mut host, manager, code, token(MANAGER));
        assert!(
            matches!(error, ServiceError::UnsupportedAndroidApi { .. }),
            "code {code}: {error:?}"
        );
    }
    let router = host.check_service("media_router").unwrap().unwrap();
    let error = transact_err(
        &mut host,
        router,
        1,
        token("android.media.IMediaRouterService"),
    );
    assert!(matches!(error, ServiceError::UnsupportedAndroidApi { .. }), "{error:?}");
    let state = registry
        .control(&mut host, MODULE, "sessions", &json!({}))
        .unwrap();
    assert_eq!(state["unsupported"].as_array().unwrap().len(), 3);
}

#[test]
fn detach_returns_the_tombstone_and_a_held_session_answers_module_off() {
    let (mut host, mut registry) = start();
    let created = create_session(&mut host, guest(41), "acos-music");
    registry.detach(&mut host, MODULE).expect("detach");
    // `media_session` is a tombstone again (structured error, not null).
    let manager = host.check_service("media_session").unwrap().unwrap();
    let error = transact_err(&mut host, manager, 16, token(MANAGER));
    assert!(
        matches!(error, ServiceError::ModuleUnavailable { module: MODULE, .. }),
        "{error:?}"
    );
    // The app still holds its ISession: it answers the same structured error.
    let error = transact_err(&mut host, created.session, 4, {
        let mut data = token(SESSION);
        data.write_bool(true);
        data
    });
    assert!(
        matches!(
            error,
            ServiceError::ModuleUnavailable { .. } | ServiceError::UnknownBinder { .. }
        ),
        "{error:?}"
    );
}
