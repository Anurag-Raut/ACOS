//! The `location` module at the wire: exactly what a framework client sends and
//! exactly what it reads back.
//!
//! Every request here is built the way the AIDL-generated proxy builds it
//! (`ILocationManager.aidl`, `android-14.0.0_r75`), and every reply is read the
//! way the generated stub reads it, so a field-order mistake fails here instead
//! of silently on ART. The end-to-end proof (a real app on ART) is
//! `results/handoff/MODLOC/`.

use acos_parcel::{BinderId, Parcel};
use acos_services::module::{ModuleConfig, ModuleRegistry, ModuleSpec, Profile};
use acos_services::{ServiceError, ServiceHost};
use serde_json::json;

const DESCRIPTOR: &str = "android.location.ILocationManager";
const LISTENER: &str = "android.location.ILocationListener";

fn spec(config: serde_json::Value) -> ModuleSpec {
    let mut spec = ModuleSpec::new("location", Profile::Scripted);
    spec.config = ModuleConfig::from_json("location", &config).expect("config");
    spec
}

/// A host with `core` + the location module attached, and its binder.
fn host_with(config: serde_json::Value) -> (ServiceHost, BinderId) {
    let mut host = ServiceHost::new();
    let mut registry = ModuleRegistry::standard();
    registry
        .attach(&mut host, "core", Profile::Real, &ModuleConfig::default())
        .expect("core attaches");
    registry
        .attach(
            &mut host,
            "location",
            Profile::Scripted,
            &spec(config).config,
        )
        .expect("location attaches");
    let binder = host
        .check_service("location")
        .expect("lookup is infallible")
        .expect("location must resolve");
    (host, binder)
}

fn transact(
    host: &mut ServiceHost,
    binder: BinderId,
    code: u32,
    fill: impl FnOnce(&mut Parcel),
) -> Result<Parcel, ServiceError> {
    let mut data = Parcel::new();
    data.write_interface_token(DESCRIPTOR).expect("token");
    fill(&mut data);
    data.set_data_position(0);
    let mut reply = Parcel::new();
    host.transact(binder, code, &mut data, &mut reply, 0)?;
    reply.set_data_position(0);
    Ok(reply)
}

fn write_last_location_request(data: &mut Parcel) {
    data.write_string16("gps").expect("provider");
    // AIDL parcelable argument: int32 presence word (writeTypedObject), then
    // LastLocationRequest's three booleans.
    data.write_int32(1);
    data.write_int32(0);
    data.write_int32(0);
    data.write_int32(0);
    data.write_string16("dev.acos.fixtures.locationprobe")
        .expect("package");
    data.write_string16_opt(None).expect("attribution tag");
}

/// `android.location.GeocoderParams` the way the image's `Geocoder` writes it:
/// presence word, uid, package String8, attribution String8, locale
/// language/country/variant String8.
fn write_geocoder_params(data: &mut Parcel) {
    data.write_int32(1); // parcelable presence
    data.write_int32(10000); // uid
    data.write_string8("dev.acos.fixtures.locationprobe")
        .expect("package");
    data.write_string8_opt(None).expect("tag");
    data.write_string8("en").expect("language");
    data.write_string8("US").expect("country");
    data.write_string8("").expect("variant");
}

/// One `LocationRequest` argument the way the generated proxy writes it:
/// presence word, then the parcelable body.
fn write_location_request(data: &mut Parcel, provider: &str) {
    data.write_int32(1);
    data.write_string16(provider).expect("request provider");
    data.write_int64(1000);
    data.write_int32(0);
    data.write_int64(0);
    data.write_int64(0);
    data.write_int32(i32::MAX);
    data.write_int64(0);
    data.write_float(0.0);
    data.write_int64(0);
    data.write_int32(0);
    data.write_int32(0);
    data.write_int32(0);
    data.write_int32(0);
    data.write_int32(0);
}

/// The `Location` fields in `Location.writeToParcel` order.
fn read_location(reply: &mut Parcel) -> (String, i32, f64, f64, f32) {
    let provider = reply.read_string8().expect("provider").expect("provider");
    let mask = reply.read_int32().expect("mask");
    let _time = reply.read_int64().expect("time");
    let _elapsed = reply.read_int64().expect("elapsed");
    let latitude = reply.read_double().expect("lat");
    let longitude = reply.read_double().expect("lng");
    if mask & 1 != 0 {
        let _altitude = reply.read_double().expect("altitude");
    }
    if mask & 2 != 0 {
        let _speed = reply.read_float().expect("speed");
    }
    if mask & 4 != 0 {
        let _bearing = reply.read_float().expect("bearing");
    }
    let accuracy = reply.read_float().expect("accuracy");
    assert_eq!(reply.read_int32().expect("extras"), -1, "null Bundle");
    (provider, mask, latitude, longitude, accuracy)
}

#[test]
fn get_last_known_location_writes_the_location_parcel_layout() {
    let (mut host, binder) = host_with(json!({
        "initial": {"lat": 37.4220, "lng": -122.0841, "accuracy": 4.0,
                    "altitude": 12.5, "provider": "gps"}
    }));
    let mut reply = transact(&mut host, binder, 1, write_last_location_request).expect("reply");
    assert_eq!(reply.read_int32().expect("exception header"), 0);
    assert_eq!(reply.read_int32().expect("present"), 1);
    let (provider, mask, latitude, longitude, accuracy) = read_location(&mut reply);
    assert_eq!(provider, "gps");
    assert_eq!(mask & 8, 8, "accuracy is always present");
    assert_eq!(mask & 1, 1, "altitude was scripted");
    assert_eq!(latitude, 37.4220);
    assert_eq!(longitude, -122.0841);
    assert_eq!(accuracy, 4.0);
}

#[test]
fn a_coarse_grant_gets_a_coarsened_fix_and_a_missing_grant_gets_a_security_exception() {
    let (mut host, binder) = host_with(json!({
        "permissions": "coarse",
        "initial": {"lat": 37.4220, "lng": -122.0841, "accuracy": 4.0, "bearing": 90.0}
    }));
    let mut reply = transact(&mut host, binder, 1, write_last_location_request).expect("reply");
    assert_eq!(reply.read_int32().expect("header"), 0);
    assert_eq!(reply.read_int32().expect("present"), 1);
    let (_provider, mask, latitude, longitude, accuracy) = read_location(&mut reply);
    assert_eq!(mask & 4, 0, "a coarse fix carries no bearing");
    assert!(accuracy >= 200.0, "coarse accuracy floor, got {accuracy}");
    assert!((latitude - 37.4220).abs() < 0.01);
    assert!((longitude + 122.0841).abs() < 0.02);

    let (mut host, binder) = host_with(json!({"permissions": "none"}));
    let error = transact(&mut host, binder, 1, write_last_location_request)
        .expect_err("no location grant must throw");
    match &error {
        ServiceError::Android { code, message } => {
            assert_eq!(
                *code, -1,
                "Parcel.EX_SECURITY is java.lang.SecurityException"
            );
            assert!(
                message.contains("ACCESS_FINE_LOCATION"),
                "the message must name the permission, got {message}"
            );
        }
        other => panic!("expected a SecurityException, got {other:?}"),
    }
    assert_eq!(error.exception_code(), -1);
}

#[test]
fn an_unknown_provider_is_a_null_location_and_provider_queries_follow_the_setting() {
    let (mut host, binder) = host_with(json!({}));
    let reply = transact(&mut host, binder, 1, |data| {
        data.write_string16("nonesuch").expect("provider");
        data.write_int32(1); // LastLocationRequest presence
        data.write_int32(0);
        data.write_int32(0);
        data.write_int32(0);
        data.write_string16("p").expect("package");
        data.write_string16_opt(None).expect("tag");
    })
    .expect_err("an unknown provider is an IllegalArgumentException");
    assert_eq!(reply.exception_code(), -3);

    // `hasProvider` = 36.
    for (provider, expected) in [("gps", 1), ("passive", 1), ("nonesuch", 0)] {
        let mut reply = transact(&mut host, binder, 36, |data| {
            data.write_string16(provider).expect("provider");
        })
        .expect("reply");
        assert_eq!(reply.read_int32().expect("header"), 0);
        assert_eq!(
            reply.read_int32().expect("hasProvider"),
            expected,
            "{provider}"
        );
    }
    // isLocationEnabledForUser = 48.
    let mut reply = transact(&mut host, binder, 48, |data| data.write_int32(0)).expect("reply");
    assert_eq!(reply.read_int32().expect("header"), 0);
    assert_eq!(reply.read_int32().expect("enabled"), 1);
    // isProviderEnabledForUser = 47.
    let mut reply = transact(&mut host, binder, 47, |data| {
        data.write_string16("gps").expect("provider");
        data.write_int32(0);
    })
    .expect("reply");
    assert_eq!(reply.read_int32().expect("header"), 0);
    assert_eq!(reply.read_int32().expect("enabled"), 1);
}

#[test]
fn a_location_request_with_a_real_work_source_keeps_the_listener_aligned() {
    // `LocationRequest.Builder` builds a non-null empty WorkSource, so the real
    // proxy writes `writeTypedObject(workSource)` = presence 1, mNum 0, int[]
    // null, String[] null, chains null (16 bytes). The listener binder follows.
    let (mut host, binder) = host_with(json!({}));
    let listener = BinderId(0x1000_0010);
    transact(&mut host, binder, 3, |data| {
        data.write_string16("gps").expect("provider");
        data.write_int32(1); // LocationRequest presence
        data.write_string16("gps").expect("request provider");
        data.write_int64(1000);
        data.write_int32(0);
        data.write_int64(0);
        data.write_int64(0);
        data.write_int32(i32::MAX);
        data.write_int64(0);
        data.write_float(0.0);
        data.write_int64(0);
        data.write_int32(0);
        data.write_int32(0);
        data.write_int32(0);
        data.write_int32(0);
        data.write_int32(1); // WorkSource present
        data.write_int32(0); // mNum
        data.write_int32(-1); // int[] mUids null
        data.write_int32(-1); // String[] mNames null
        data.write_int32(-1); // chains null
        data.write_strong_binder(listener);
        data.write_string16("p").expect("package");
        data.write_string16_opt(None).expect("tag");
        data.write_string16("id").expect("listener id");
    })
    .expect("a non-null WorkSource must keep the listener aligned");
    let deliveries = host.take_deliveries();
    assert_eq!(
        deliveries.len(),
        1,
        "the registration emits the provider state"
    );
    assert_eq!(deliveries[0].target, listener);
}

#[test]
fn get_all_providers_lists_the_four_phone_providers() {
    let (mut host, binder) = host_with(json!({}));
    let mut reply = transact(&mut host, binder, 37, |_| {}).expect("reply");
    assert_eq!(reply.read_int32().expect("header"), 0);
    let names = reply
        .read_string16_array()
        .expect("list")
        .expect("non-null list");
    let names: Vec<String> = names.into_iter().flatten().collect();
    assert_eq!(names, vec!["fused", "gps", "network", "passive"]);
}

#[test]
fn the_geocoder_answers_the_scripted_addresses_and_the_no_geocoder_path_sends_on_error() {
    let (mut host, binder) = host_with(json!({
        "geocoder": {
            "present": true,
            "places": [{
                "lat": 37.4220, "lng": -122.0841, "max_distance_m": 500,
                "addresses": [{"lines": ["1600 Amphitheatre Parkway"],
                               "locality": "Mountain View", "country_code": "US"}]
            }]
        }
    }));
    // isGeocodeAvailable = 12.
    let mut reply = transact(&mut host, binder, 12, |_| {}).expect("reply");
    assert_eq!(reply.read_int32().expect("header"), 0);
    assert_eq!(reply.read_int32().expect("present"), 1);
    // reverseGeocode = 13; the callback binder is a node this module never
    // transacts in this test, so the delivery stays queued and can be read.
    let callback = BinderId(0x1000_0001);
    let mut reply = transact(&mut host, binder, 13, |data| {
        // The image's geocoder ABI: getFromLocation(double, double, int,
        // GeocoderParams, IGeocodeListener).
        data.write_double(37.4221);
        data.write_double(-122.0840);
        data.write_int32(3);
        write_geocoder_params(data);
        data.write_strong_binder(callback);
    })
    .expect("reply");
    assert_eq!(reply.read_int32().expect("header"), 0);
    let deliveries = host.take_deliveries();
    eprintln!(
        "geocode deliveries: {:?}",
        deliveries
            .iter()
            .map(|d| (d.interface.clone(), d.code, d.method.clone()))
            .collect::<Vec<_>>()
    );
    assert_eq!(deliveries.len(), 1, "one geocode callback");
    let delivery = &deliveries[0];
    assert_eq!(delivery.code, 1, "onResults");
    assert_eq!(delivery.target, callback);
    let mut body = delivery.data.clone();
    body.set_data_position(0);
    body.enforce_interface("android.location.IGeocodeListener")
        .expect("token");
    assert!(
        body.read_string16().expect("error").is_none(),
        "successful geocode has a null error string"
    );
    assert_eq!(body.read_int32().expect("list size"), 1);
    assert_eq!(body.read_int32().expect("present"), 1);
    assert_eq!(body.read_string16().expect("lang").expect("lang"), "en");
    assert_eq!(
        body.read_string16().expect("country").expect("country"),
        "US"
    );
    assert_eq!(body.read_int32().expect("line count"), 1);
    assert_eq!(body.read_int32().expect("line index"), 0);
    assert_eq!(
        body.read_string16().expect("line").expect("line"),
        "1600 Amphitheatre Parkway"
    );
    assert!(body.read_string16().expect("feature").is_none());
    assert!(body.read_string16().expect("admin").is_none());
    assert!(body.read_string16().expect("sub-admin").is_none());
    assert_eq!(
        body.read_string16().expect("locality").expect("locality"),
        "Mountain View"
    );

    // No geocoder: the framework answers no addresses (`onResults(null, null)`),
    // which `Geocoder.GeocodeImpl` turns into `GeocodeListener.onError(null)`.
    let (mut host, binder) = host_with(json!({}));
    let mut reply = transact(&mut host, binder, 12, |_| {}).expect("reply");
    assert_eq!(reply.read_int32().expect("header"), 0);
    assert_eq!(reply.read_int32().expect("present"), 0);
    let callback = BinderId(0x1000_0002);
    transact(&mut host, binder, 13, |data| {
        data.write_double(0.0);
        data.write_double(0.0);
        data.write_int32(1);
        write_geocoder_params(data);
        data.write_strong_binder(callback);
    })
    .expect("reply");
    let deliveries = host.take_deliveries();
    assert_eq!(deliveries.len(), 1);
    assert_eq!(deliveries[0].code, 1, "onResults");
    assert_eq!(deliveries[0].method, "onResults");
    let mut body = deliveries[0].data.clone();
    body.set_data_position(0);
    body.enforce_interface("android.location.IGeocodeListener")
        .expect("token");
    assert!(
        body.read_string16().expect("error").is_none(),
        "the no-geocoder path answers onResults(null, null)"
    );
    assert_eq!(body.read_int32().expect("list"), -1, "null address list");
}

#[test]
fn play_track_delivers_at_real_timing_and_the_session_records_each_fix() {
    let mut host = ServiceHost::new();
    let mut registry = ModuleRegistry::standard();
    registry
        .attach(&mut host, "core", Profile::Real, &ModuleConfig::default())
        .expect("core attaches");
    registry
        .attach(
            &mut host,
            "location",
            Profile::Scripted,
            &spec(json!({})).config,
        )
        .expect("location attaches");
    let binder = host
        .check_service("location")
        .expect("lookup")
        .expect("binder");

    // Register a listener exactly as `LocationManager.requestLocationUpdates`
    // does: LocationRequest, ILocationListener, package, attribution tag, id.
    let listener = BinderId(0x1000_0003);
    transact(&mut host, binder, 3, |data| {
        data.write_string16("gps").expect("provider");
        write_location_request(data, "gps");
        data.write_strong_binder(listener);
        data.write_string16("dev.acos.fixtures.locationprobe")
            .expect("package");
        data.write_string16_opt(None).expect("tag");
        data.write_string16("listener-id").expect("listener id");
    })
    .expect("registerLocationListener");

    // The registration itself emits `onProviderEnabledChanged(gps, true)`.
    let initial = host.take_deliveries();
    assert_eq!(initial.len(), 1);
    assert_eq!(initial[0].code, 2);
    let mut body = initial[0].data.clone();
    body.set_data_position(0);
    body.enforce_interface(LISTENER).expect("token");
    assert_eq!(body.read_string16().expect("provider").expect("p"), "gps");
    assert_eq!(body.read_int32().expect("enabled"), 1);

    let started = registry
        .control(
            &mut host,
            "location",
            "play_track",
            &json!({
                "points": [{"lat": 37.4221, "lng": -122.0842},
                           {"lat": 37.4222, "lng": -122.0843},
                           {"lat": 37.4223, "lng": -122.0844}],
                "interval_ms": 40
            }),
        )
        .expect("play_track");
    assert_eq!(started["points"], 3);

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let mut fixes: Vec<(f64, f64)> = Vec::new();
    while std::time::Instant::now() < deadline && fixes.len() < 3 {
        // The module delivers from its own thread: `take_timed_deliveries` is the
        // drain the peer's idle tick uses (see `ServiceHost::take_timed_deliveries`).
        for delivery in host.take_timed_deliveries() {
            if delivery.code != 1 {
                continue;
            }
            let mut body = delivery.data.clone();
            body.set_data_position(0);
            body.enforce_interface(LISTENER).expect("token");
            assert_eq!(body.read_int32().expect("list size"), 1);
            assert_eq!(body.read_int32().expect("present"), 1);
            let provider = body.read_string8().expect("provider").expect("provider");
            assert_eq!(provider, "gps");
            let mask = body.read_int32().expect("mask");
            let _time = body.read_int64().expect("time");
            let elapsed = body.read_int64().expect("elapsed");
            let latitude = body.read_double().expect("lat");
            let longitude = body.read_double().expect("lng");
            if mask & 1 != 0 {
                let _altitude = body.read_double().expect("altitude");
            }
            if mask & 2 != 0 {
                let _speed = body.read_float().expect("speed");
            }
            if mask & 4 != 0 {
                let _bearing = body.read_float().expect("bearing");
            }
            let _accuracy = body.read_float().expect("accuracy");
            assert_eq!(body.read_int32().expect("extras"), -1);
            assert!(elapsed > 0);
            fixes.push((latitude, longitude));
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert_eq!(
        fixes.len(),
        3,
        "the three track points must arrive: {fixes:?}"
    );
    assert!((fixes[2].0 - 37.4223).abs() < 1e-9);
    assert!((fixes[2].1 + 122.0844).abs() < 1e-9);

    let record = registry
        .control(&mut host, "location", "delivered", &json!(null))
        .expect("delivered");
    assert_eq!(
        record["count"], 3,
        "every delivered fix is recorded: {record}"
    );
    assert_eq!(record["fixes"][2]["lat"], 37.4223);
    let report = registry
        .reports()
        .into_iter()
        .find(|report| report.id == "location")
        .expect("report");
    let reason = report.reason.expect("an attached module explains itself");
    assert!(
        reason.contains("3 fix(es) delivered"),
        "the report cites the deliveries: {reason}"
    );
    registry
        .control(&mut host, "location", "stop_track", &json!(null))
        .expect("stop_track");
}

#[test]
fn a_provider_toggle_notifies_its_listeners_and_a_detached_module_is_honest() {
    let mut host = ServiceHost::new();
    let mut registry = ModuleRegistry::standard();
    registry
        .attach(&mut host, "core", Profile::Real, &ModuleConfig::default())
        .expect("core attaches");
    registry.install_tombstones(&mut host);
    // Off: the name keeps the framework's disabled device answer (a phone with
    // location off still publishes `ILocationManager`): a structured error for
    // every provider call, and `isGeocodeAvailable` = false.
    let off_binder = host
        .check_service("location")
        .expect("lookup")
        .expect("the off node is registered");
    let error = transact(&mut host, off_binder, 1, write_last_location_request)
        .expect_err("an off module must not pretend a fix");
    assert_eq!(error.code(), "UNSUPPORTED_ANDROID_API");
    let mut reply =
        transact(&mut host, off_binder, 12, |_| {}).expect("geocoder presence is a fixed truth");
    assert_eq!(reply.read_int32().expect("header"), 0);
    assert_eq!(reply.read_int32().expect("present"), 0);

    registry
        .attach(
            &mut host,
            "location",
            Profile::Scripted,
            &spec(json!({})).config,
        )
        .expect("attach");
    let binder = host
        .check_service("location")
        .expect("lookup")
        .expect("binder");
    // ADR-0029 note (MODSENS): the attach fills `core`'s node, it does not replace
    // it (a guest's `LocationManager` caches the proxy for the process's life).
    assert_eq!(
        binder, off_binder,
        "an attach must reuse the binder core published, not register a new one"
    );
    let listener = BinderId(0x1000_0004);
    transact(&mut host, binder, 3, |data| {
        data.write_string16("gps").expect("provider");
        write_location_request(data, "gps");
        data.write_strong_binder(listener);
        data.write_string16("p").expect("package");
        data.write_string16_opt(None).expect("tag");
        data.write_string16("id").expect("listener id");
    })
    .expect("register");
    host.take_deliveries();

    let toggled = registry
        .control(
            &mut host,
            "location",
            "set_provider_enabled",
            &json!({"provider": "gps", "enabled": false}),
        )
        .expect("set_provider_enabled");
    assert_eq!(toggled["enabled"], false);
    assert_eq!(toggled["listeners_notified"], 1);
    let deliveries = host.take_timed_deliveries();
    assert_eq!(deliveries.len(), 1);
    assert_eq!(deliveries[0].code, 2);
    let mut body = deliveries[0].data.clone();
    body.set_data_position(0);
    body.enforce_interface(LISTENER).expect("token");
    assert_eq!(body.read_string16().expect("provider").expect("p"), "gps");
    assert_eq!(body.read_int32().expect("enabled"), 0);

    // Detach: the node is **emptied in place**, back to the honest disabled
    // device answer. A handle the guest still holds keeps working on the same
    // binder and now gets that answer — never a stale fix, never a dead handle.
    registry.detach(&mut host, "location").expect("detach");
    let emptied = host
        .check_service("location")
        .expect("lookup")
        .expect("the node core published stays registered");
    assert_eq!(
        emptied, binder,
        "a detach must empty the node in place, not free the binder"
    );
    let stale = transact(&mut host, binder, 1, write_last_location_request)
        .expect_err("the emptied node must not answer a fix");
    assert_eq!(stale.code(), "UNSUPPORTED_ANDROID_API");
    match &stale {
        ServiceError::UnsupportedAndroidApi { interface, method, .. } => {
            assert_eq!(interface, "android.location.ILocationManager");
            assert!(
                method.contains("not attached") && method.contains("id\": \"location"),
                "the disabled answer must name the module and the attach call: {method}"
            );
        }
        other => panic!("expected the disabled-device answer, got {other:?}"),
    }
    // The geocoder truth is still answered on the same binder (a phone with
    // location off has no geocoder provider either).
    let mut reply = transact(&mut host, binder, 12, |_| {}).expect("geocoder presence");
    assert_eq!(reply.read_int32().expect("header"), 0);
    assert_eq!(reply.read_int32().expect("present"), 0);
    // Control on a detached module refuses (no fake success).
    assert!(matches!(
        registry.control(
            &mut host,
            "location",
            "set_location",
            &json!({"lat": 1.0, "lng": 2.0})
        ),
        Err(ServiceError::ModuleRejected { .. })
    ));
}

/// MODLOC3: `requestLocationUpdates(PendingIntent)` (transaction 5) is refused,
/// and the refusal names the one thing it would need.
///
/// The image's own client is the evidence: `LocationPendingIntentTransport`
/// (`services/classes2.dex`, class
/// `Lcom/android/server/location/provider/LocationProviderManager$LocationPendingIntentTransport;`)
/// builds `new Intent()`, `putExtra("location", lastLocation)` and
/// `PendingIntentSender.send(pendingIntent, context, intent, …)` — the fix travels
/// as the **fill-in** intent of `IIntentSender.send`, under
/// `LocationManager.KEY_LOCATION_CHANGED`. MODNOTIF5 refuses a fill-in on a
/// mutable sender (`Intent.fillIn` is not modelled) and drops it on an immutable
/// one, so accepting this registration could only fire a sender with no fix or
/// with a stale one. The step is refused; the LocationListener form is the
/// supported path.
#[test]
fn the_pending_intent_form_is_refused_and_names_the_fill_in_it_needs() {
    let (mut host, binder) = host_with(json!({}));
    // A real `PendingIntent` parcel: the presence word, then the strong binder.
    let sender = BinderId(0x1000_0005);
    let error = transact(&mut host, binder, 5, |data| {
        data.write_string16("gps").expect("provider");
        write_location_request(data, "gps");
        data.write_int32(1);
        data.write_strong_binder(sender);
        data.write_string16("dev.acos.fixtures.locationprobe")
            .expect("package");
        data.write_string16_opt(None).expect("attribution tag");
    })
    .expect_err("the pending-intent form must be refused, never accepted silently");
    assert_eq!(error.code(), "UNSUPPORTED_ANDROID_API");
    match &error {
        ServiceError::UnsupportedAndroidApi {
            interface,
            code,
            method,
        } => {
            assert_eq!(interface, DESCRIPTOR);
            // The refusal reports the transaction the caller sent, not a made-up
            // interface code.
            assert_eq!(*code, 5);
            assert!(
                method.contains("KEY_LOCATION_CHANGED"),
                "the refusal must name the extra the framework sends: {method}"
            );
            assert!(
                method.contains("fill-in") && method.contains("PIFILLIN"),
                "the refusal must name the fill-in merge it needs: {method}"
            );
            assert!(
                method.contains("LocationListener"),
                "the refusal must name the supported path: {method}"
            );
        }
        other => panic!("expected UnsupportedAndroidApi, got {other:?}"),
    }
}
