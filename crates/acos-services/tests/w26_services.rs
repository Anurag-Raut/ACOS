//! W26 acceptance: the three host-side walls W25 named for the Gaten debug APK.
//!
//! 1. `sensorservice` absent → `SystemSensorManager.nativeCreate` waits ~11.9 s
//!    (two 5 s `ServiceManagerShim::getService` retry loops) inside the
//!    launcher's 12 s resume budget.
//! 2. `IPackageManager.notifyDexLoad` (code 106) → `Str16` underflow on a
//!    31-dex APK because the generated table reads `[I32, I32]` where the image
//!    writes a `Map<String,String>`.
//! 3. `IActivityManager.handleApplicationWtf` → no
//!    `ApplicationErrorReport$ParcelableCrashInfo` codec, so every WTF report
//!    was lost.
//!
//! Every wire shape here is read from the **image**, quoted next to the test.
//! Negative cases are first-class: a truncated or nonsense parcel must produce
//! a structured `SERVICE_BAD_REQUEST` (or `UNSUPPORTED_ANDROID_API`) — never a
//! panic and never a hang.

use std::time::Instant;

use acos_parcel::Parcel;
use acos_services::codec;
use acos_services::exception::NO_EXCEPTION;
use acos_services::misc::{
    SENSOR_CREATE_DIRECT_CONNECTION, SENSOR_CREATE_EVENT_CONNECTION,
    SENSOR_GET_DYNAMIC_SENSOR_LIST, SENSOR_GET_SENSOR_LIST, SENSOR_SERVER_DESCRIPTOR,
};
use acos_services::{ServiceError, ServiceHost};

const ISERVICE_MANAGER: &str = "android.os.IServiceManager";
const IPACKAGE_MANAGER: &str = "android.content.pm.IPackageManager";
const IACTIVITY_MANAGER: &str = "android.app.IActivityManager";

fn transact(
    host: &mut ServiceHost,
    binder: acos_parcel::BinderId,
    code: u32,
    flags: u32,
    data: &mut Parcel,
) -> std::result::Result<Parcel, ServiceError> {
    let mut reply = Parcel::new();
    host.transact(binder, code, data, &mut reply, flags)?;
    reply.set_data_position(0);
    Ok(reply)
}

fn no_exception(reply: &mut Parcel) {
    assert_eq!(reply.read_int32().expect("exception header"), NO_EXCEPTION);
}

// ---------------------------------------------------------------------------
// 1. sensorservice
// ---------------------------------------------------------------------------

/// `BnSensorServer::onTransact` (image `libsensor.so` `0xE927`): the request is
/// the interface token then `readString16(package)`.
fn sensor_list_request(descriptor: &str, package: &str) -> Parcel {
    let mut data = Parcel::new();
    data.write_interface_token(descriptor).expect("token");
    data.write_string16(package).expect("string");
    data.set_data_position(0);
    data
}

#[test]
fn sensorservice_resolves_the_first_time_and_serves_an_empty_sensor_list() {
    // The framework asks `ServiceManager.getService("sensorservice")` twice
    // inside `SystemSensorManager.nativeCreate` (libsensor
    // `SensorManager::waitForSensorService`), and each null answer costs
    // libbinder 5000 ms of `checkService` retries. The name must therefore
    // resolve to a binder, not to null.
    let mut host = ServiceHost::standard();
    let started = Instant::now();
    let server = host
        .check_service("sensorservice")
        .expect("name lookup is infallible")
        .expect("sensorservice must be published");
    let lookup = started.elapsed();
    assert!(
        lookup.as_millis() < 100,
        "a published service must not cost a retry loop (took {lookup:?})"
    );
    assert_eq!(host.descriptor_of(server), Some(SENSOR_SERVER_DESCRIPTOR));

    // getSensorList: `writeUint32(count)` + per sensor
    // `writeInt32(flattenedSize)` + flattened `Sensor`. Zero sensors is one
    // count word.
    //
    // MODSENS: `ISensorServer` is a **hand-written** binder interface
    // (`libsensor/ISensorServer.cpp`), so its proxy reads the reply with no
    // `binder::Status`/exception header. W26 wrote one (`no_exception` first),
    // which the client read as the count: harmless while the list was always
    // empty, and fatal for a populated one. The reply now starts at the count.
    for code in [SENSOR_GET_SENSOR_LIST, SENSOR_GET_DYNAMIC_SENSOR_LIST] {
        let mut data = sensor_list_request(SENSOR_SERVER_DESCRIPTOR, "com.gaten");
        let mut reply = transact(&mut host, server, code, 0, &mut data).expect("transact");
        assert_eq!(reply.read_int32().expect("sensor count"), 0);
        assert_eq!(reply.data_avail(), 0, "an empty Vector<Sensor> is 4 bytes");
    }

    // The same name through the IServiceManager node the guest really uses.
    let mut data = Parcel::new();
    data.write_interface_token(ISERVICE_MANAGER).expect("token");
    data.write_string16("sensorservice").expect("string");
    data.set_data_position(0);
    let sm = host
        .binder_for_descriptor(ISERVICE_MANAGER)
        .expect("IServiceManager node");
    let mut reply = transact(&mut host, sm, 1, 0, &mut data).expect("getService");
    no_exception(&mut reply);
    let answered = reply.read_strong_binder().expect("binder").expect("non-null");
    assert_eq!(answered, server);
    assert_eq!(reply.data_avail(), 0);

    // An unknown name still answers *null* (stock behaviour) and answers it
    // immediately: the host never sleeps.
    let mut data = Parcel::new();
    data.write_interface_token(ISERVICE_MANAGER).expect("token");
    data.write_string16("no_such_service_acme").expect("string");
    data.set_data_position(0);
    let started = Instant::now();
    let mut reply = transact(&mut host, sm, 1, 0, &mut data).expect("getService");
    assert!(
        started.elapsed().as_millis() < 100,
        "unknown names answer null without a retry loop"
    );
    no_exception(&mut reply);
    assert_eq!(reply.read_strong_binder().expect("binder"), None);
    assert_eq!(reply.data_avail(), 0);
}

#[test]
fn sensor_server_rejects_truncated_and_garbage_requests_without_hanging() {
    let mut host = ServiceHost::standard();
    let server = host
        .check_service("sensorservice")
        .expect("lookup")
        .expect("published");

    // Truncated: token only, no package string.
    let mut data = Parcel::new();
    data.write_interface_token(SENSOR_SERVER_DESCRIPTOR).expect("token");
    data.set_data_position(0);
    let err = transact(&mut host, server, SENSOR_GET_SENSOR_LIST, 0, &mut data)
        .expect_err("a missing package must be a bad request");
    assert_eq!(err.code(), "SERVICE_BAD_REQUEST", "{err}");

    // Garbage: a string length far past the end of the parcel.
    let mut data = Parcel::new();
    data.write_interface_token(SENSOR_SERVER_DESCRIPTOR).expect("token");
    data.write_int32(0x00ff_ffff);
    data.write_int32(0x1234);
    data.set_data_position(0);
    let err = transact(&mut host, server, SENSOR_GET_DYNAMIC_SENSOR_LIST, 0, &mut data)
        .expect_err("an impossible string length must be a bad request");
    assert_eq!(err.code(), "SERVICE_BAD_REQUEST", "{err}");

    // Garbage: a wrong interface token is not this node's business.
    let mut data = sensor_list_request("android.gui.ISurfaceComposer", "com.gaten");
    let err = transact(&mut host, server, SENSOR_GET_SENSOR_LIST, 0, &mut data)
        .expect_err("a foreign interface token must be unsupported");
    assert_eq!(err.code(), "UNSUPPORTED_ANDROID_API", "{err}");
}

#[test]
fn sensor_event_connection_is_a_named_gap_not_a_fabricated_queue() {
    // `createSensorEventConnection(String8 opPackage, int mode, String16
    // package, String16 attributionTag) -> ISensorEventConnection` (image
    // `0xECDB`). With **no feed** (the modules `sensors:off` state) the honest
    // answer is a named structured gap: a real device's reply carries a `BitTube`
    // of event fds, and ACOS only serves one from a scripted feed
    // (`crates/acos-services/tests/sensors_module.rs`).
    let mut host = ServiceHost::standard();
    let server = host
        .check_service("sensorservice")
        .expect("lookup")
        .expect("published");
    let mut data = Parcel::new();
    data.write_interface_token(SENSOR_SERVER_DESCRIPTOR).expect("token");
    data.write_string8("com.gaten").expect("string8");
    data.write_int32(0);
    data.write_string16("com.gaten").expect("string");
    data.write_string16("").expect("string");
    data.set_data_position(0);
    let err = transact(&mut host, server, SENSOR_CREATE_EVENT_CONNECTION, 0, &mut data)
        .expect_err("no fabricated queue");
    match err {
        ServiceError::UnsupportedAndroidApi { code, .. } => {
            assert_eq!(code, SENSOR_CREATE_EVENT_CONNECTION)
        }
        other => panic!("expected a named gap, got {other}"),
    }

    // The direct-channel call (a `NativeHandle` argument, image `0xEB68`) is
    // likewise a structured gap rather than a guessed layout.
    let mut data = Parcel::new();
    data.write_interface_token(SENSOR_SERVER_DESCRIPTOR).expect("token");
    data.set_data_position(0);
    let err = transact(&mut host, server, SENSOR_CREATE_DIRECT_CONNECTION, 0, &mut data)
        .expect_err("no guessed direct channel");
    assert_eq!(err.code(), "UNSUPPORTED_ANDROID_API", "{err}");

    // And a code the image does not define at all.
    let mut data = Parcel::new();
    data.write_interface_token(SENSOR_SERVER_DESCRIPTOR).expect("token");
    data.set_data_position(0);
    let err = transact(&mut host, server, 99, 0, &mut data).expect_err("unknown code");
    assert_eq!(err.code(), "UNSUPPORTED_ANDROID_API", "{err}");
}

// ---------------------------------------------------------------------------
// 2. IPackageManager.notifyDexLoad (106)
// ---------------------------------------------------------------------------

/// The image's `IPackageManager$Stub$Proxy.notifyDexLoad` writer, byte for byte
/// (`classes.dex`: `writeString(pkg)`, `writeInt(size)`, `forEach { writeString(k);
/// writeString(v) }`, `writeString(loaderIsa)`, `transact(106, .., FLAG_ONEWAY)`).
fn notify_dex_load_body(package: &str, entries: &[(String, String)], loader: &str) -> Parcel {
    let mut data = Parcel::new();
    data.write_interface_token(IPACKAGE_MANAGER).expect("token");
    let args_start = data.data_position();
    data.write_string16(package).expect("string");
    data.write_int32(i32::try_from(entries.len()).expect("count"));
    for (k, v) in entries {
        data.write_string16(k).expect("string");
        data.write_string16(v).expect("string");
    }
    data.write_string16(loader).expect("string");
    data.set_data_position(args_start);
    data
}

fn dex_entries(n: usize) -> Vec<(String, String)> {
    (0..n)
        .map(|i| {
            (
                format!("/data/app/com.gaten-1/base.apk!/classes{i}.dex"),
                "PCL[]".to_string(),
            )
        })
        .collect()
}

#[test]
fn notify_dex_load_with_31_dex_files_decodes_instead_of_underflowing() {
    let entries = dex_entries(31);
    let mut data = notify_dex_load_body("com.gaten", &entries, "x86_64");

    // The generated table entry: `[Str16, I32, I32, Str16]`. It must fail (that
    // is the W25 wall, reproduced from the image's own writer), which is what
    // gives this test its teeth.
    let table = acos_aidl::lookup(IPACKAGE_MANAGER, 106).expect("table entry exists");
    assert_eq!(table.args.len(), 4, "generated table shape");
    let err = codec::read_args(&mut data, &table.args, IPACKAGE_MANAGER, "notifyDexLoad")
        .expect_err("the generated layout cannot read a Map<String,String>");
    assert_eq!(err.code(), "SERVICE_BAD_REQUEST", "{err}");

    // The image-verified layout decodes the same bytes exactly.
    let mut data = notify_dex_load_body("com.gaten", &entries, "x86_64");
    let args = codec::read_args_override(IPACKAGE_MANAGER, 106, &mut data)
        .expect("override decode")
        .expect("notifyDexLoad has an override");
    assert_eq!(data.data_avail(), 0, "every byte of the request is consumed");
    assert_eq!(args.len(), 3);
    assert_eq!(args[0].as_str(), Some("com.gaten"));
    match &args[1] {
        acos_services::Value::Map(Some(map)) => assert_eq!(map.len(), 31),
        other => panic!("expected a 31-entry map, got {other:?}"),
    }
    assert_eq!(args[2].as_str(), Some("x86_64"));
}

#[test]
fn notify_dex_load_null_map_records_zero_dex_files() {
    // The proxy writes `-1` for a null map.
    let mut data = Parcel::new();
    data.write_string16("com.gaten").expect("string");
    data.write_int32(-1);
    data.write_string16("x86_64").expect("string");
    data.set_data_position(0);
    let args = codec::read_args_override(IPACKAGE_MANAGER, 106, &mut data)
        .expect("override decode")
        .expect("override");
    assert_eq!(data.data_avail(), 0);
    assert!(matches!(args[1], acos_services::Value::Map(None)));
}

#[test]
fn notify_dex_load_rejects_impossible_counts_and_truncation() {
    // count < -1 is not a legal `Parcel.writeMap` shape.
    let mut data = Parcel::new();
    data.write_string16("com.gaten").expect("string");
    data.write_int32(-2);
    data.set_data_position(0);
    let err = codec::read_args_override(IPACKAGE_MANAGER, 106, &mut data)
        .expect_err("bad count");
    assert_eq!(err.code(), "SERVICE_BAD_REQUEST", "{err}");

    // A huge count with a short parcel must be refused *before* allocating.
    let mut data = Parcel::new();
    data.write_string16("com.gaten").expect("string");
    data.write_int32(1 << 24);
    data.set_data_position(0);
    let err = codec::read_args_override(IPACKAGE_MANAGER, 106, &mut data)
        .expect_err("impossible count");
    assert_eq!(err.code(), "SERVICE_BAD_REQUEST", "{err}");
    assert!(format!("{err}").contains("need at least"), "{err}");

    // A body that stops in the middle of an entry.
    let entries = dex_entries(3);
    let mut data = notify_dex_load_body("com.gaten", &entries, "x86_64");
    let len = data.data_size();
    data.set_data_size(len - 8);
    data.set_data_position(0);
    let err = codec::read_args_override(IPACKAGE_MANAGER, 106, &mut data)
        .expect_err("truncated map");
    assert_eq!(err.code(), "SERVICE_BAD_REQUEST", "{err}");

    // And the fallback for every other method is "no override".
    let mut data = Parcel::new();
    assert!(codec::read_args_override(IPACKAGE_MANAGER, 105, &mut data)
        .expect("no override")
        .is_none());
}

#[test]
fn the_package_manager_records_the_dex_files_it_was_told_about() {
    // End to end through the dispatcher: the oneway call must be accepted
    // (fire-and-forget: no reply body) and recorded.
    let mut host = ServiceHost::standard();
    let pms = host
        .check_service("package")
        .expect("lookup")
        .expect("package manager");
    let entries = dex_entries(31);
    let mut data = notify_dex_load_body("com.gaten", &entries, "x86_64");
    data.set_data_position(0); // the dispatcher reads the interface token first
    let started = Instant::now();
    let reply = transact(&mut host, pms, 106, 1, &mut data).expect("oneway notifyDexLoad");
    assert!(started.elapsed().as_millis() < 500, "no retry loop, no stall");
    assert_eq!(reply.data_size(), 0, "oneway: an empty reply body");
}

// ---------------------------------------------------------------------------
// 3. IActivityManager.handleApplicationWtf (101)
// ---------------------------------------------------------------------------

/// `CrashInfo.writeToParcel`, read from the image's `classes.dex`:
/// six strings, one int, then the stack trace and the API-34 `crashTag`.
fn write_crash_info(p: &mut Parcel) {
    for s in [
        "com.android.internal.os.RuntimeInit$KillApplicationHandler",
        "java.lang.IllegalStateException",
        "boom",
        "ReactHostImpl.kt",
        "com.facebook.react.runtime.ReactHostImpl",
        "start",
    ] {
        p.write_string16(s).expect("string");
    }
    p.write_int32(412);
    p.write_string16("java.lang.IllegalStateException: boom\n\tat a.b(C.kt:1)").expect("string");
    p.write_string16("GatenWtf").expect("string");
}

/// `IActivityManager.handleApplicationWtf` (code 101) request:
/// `IBinder app, String tag, boolean system, ParcelableCrashInfo crashInfo, int pid`.
fn wtf_request(truncate: usize) -> Parcel {
    let mut data = Parcel::new();
    data.write_interface_token(IACTIVITY_MANAGER).expect("token");
    data.write_strong_binder_opt(None);
    data.write_string16("GatenWtf").expect("string");
    data.write_int32(0); // boolean false
    data.write_int32(1); // writeTypedObject presence marker
    write_crash_info(&mut data);
    data.write_int32(4242);
    if truncate > 0 {
        let len = data.data_size();
        data.set_data_size(len - truncate);
    }
    data.set_data_position(0);
    data
}

#[test]
fn handle_application_wtf_is_decoded_logged_and_does_not_kill_the_guest() {
    let mut host = ServiceHost::standard();
    let am = host
        .check_service("activity")
        .expect("lookup")
        .expect("activity manager");

    let mut data = wtf_request(0);
    let mut reply = transact(&mut host, am, 101, 0, &mut data).expect("handleApplicationWtf");
    no_exception(&mut reply);
    assert_eq!(
        reply.read_int32().expect("return"),
        0,
        "the guest must not be killed"
    );
    assert_eq!(reply.data_avail(), 0);

    // The WTF text reaches the guest log through the event stream.
    let events = host.drain_events();
    let logged: Vec<_> = events
        .iter()
        .filter_map(|e| match &e.body {
            acos_protocol::EventBody::Log { tag, message, .. } if tag == "ActivityManagerWtf" => {
                Some(message.clone())
            }
            _ => None,
        })
        .collect();
    assert_eq!(logged.len(), 1, "one WTF log line: {events:?}");
    let line = &logged[0];
    assert!(line.contains("java.lang.IllegalStateException: boom"), "{line}");
    assert!(line.contains("(line 412)"), "{line}");
    assert!(line.contains("at a.b(C.kt:1)"), "{line}");
}

#[test]
fn handle_application_wtf_rejects_a_truncated_crash_info() {
    let mut host = ServiceHost::standard();
    let am = host
        .check_service("activity")
        .expect("lookup")
        .expect("activity manager");
    // Cut inside the stack-trace string: the codec must refuse, naming the
    // class it could not read, instead of reading past the end.
    let mut data = wtf_request(24);
    let err = transact(&mut host, am, 101, 0, &mut data).expect_err("truncated crash info");
    assert_eq!(err.code(), "SERVICE_BAD_REQUEST", "{err}");
    assert!(
        format!("{err}").contains("ApplicationErrorReport$ParcelableCrashInfo"),
        "{err}"
    );
}

/// The crash-info codec is registered under the exact class name the image's
/// AIDL uses; a wrong-dotted name must not silently resolve.
#[test]
fn the_crash_info_codec_is_registered_under_the_image_class_name() {
    assert!(acos_services::parcelable::has_codec(
        "android.app.ApplicationErrorReport$ParcelableCrashInfo"
    ));
    let spec = acos_services::parcelable::spec_for(
        "android.app.ApplicationErrorReport$ParcelableCrashInfo",
    )
    .expect("spec");
    assert_eq!(spec.fields.len(), 9, "six strings, int, stack, crashTag");
}
