//! `net.wifi`: the wire formats against the image's own framework dex, and the
//! scripted radio end to end at the service-host level.
//!
//! The app half (a real `WifiManager`, `WifiInfo.CREATOR`, a `BroadcastReceiver`
//! and a `NetworkCallback`) is proven on ART by `scenarios/modfill-wifi.yaml`.

use acos_parcel::Parcel;
use acos_services::ams::IntentFacts;
use acos_services::module::{ModuleConfig, ModuleRegistry, ModuleSpec, Profile};
use acos_services::modules::wifi;
use acos_services::parcelable::{self, Scanner};
use acos_services::{ServiceError, ServiceHost};
use serde_json::json;

const MODULE: &str = "net.wifi";
const DESCRIPTOR: &str = "android.net.wifi.IWifiManager";

// ---------------------------------------------------------------------------
// Wire formats: the image dex is the authority (BRIEF_COMMON wire-format rule)
// ---------------------------------------------------------------------------

/// Every `writeToParcel` the module emits is a spec row, and every row accounts
/// for **every** `Parcel.write*` call of the image's method (`is_full`).
///
/// Fails before the rows exist (the classes have no codec) and fails on the
/// first draft of this module, which wrote `SupplicantState` as an ordinal int
/// and the three `ScanResult` ANQP/radio-chain loops as bare counts.
#[test]
fn the_wifi_parcelables_account_for_every_write_of_the_image_dex() {
    for class in [
        "android.net.wifi.WifiInfo",
        "android.net.wifi.ScanResult",
        "android.net.wifi.WifiSsid",
        "android.net.wifi.SupplicantState",
        "android.net.DhcpInfo",
    ] {
        let spec = parcelable::spec_for(class).unwrap_or_else(|| panic!("no codec row for {class}"));
        let scanned = Scanner::write_to_parcel_boot(class)
            .unwrap_or_else(|error| panic!("{class} is not in the image's boot jars: {error}"));
        let mut verdict = parcelable::verify(spec, &scanned);
        if class == "android.net.wifi.ScanResult" {
            // D8 duplicates the tail of `ScanResult.writeToParcel` into the
            // `else` arms of its `anqpLines`/`anqpElements`/`radioChainInfos`
            // null checks, so the linear scan lists the last four writes
            // (`ifaceName`, `mApMldMacAddress`, `mApMloLinkId`,
            // `mAffiliatedMloLinks`) twice, each preceded by the arm's own
            // `writeInt(0)`. The spec must account for the first pass in full
            // and the rest must be exactly that copy.
            assert!(verdict.all_fields, "ScanResult spec has an unconsumed field");
            let tail = &scanned.calls[verdict.matched_calls - 4..verdict.matched_calls];
            let rest = &scanned.calls[verdict.matched_calls..];
            assert_eq!(rest.len(), 8, "{:?}", scanned.calls);
            assert!(rest[..4].iter().all(|call| call == "writeInt"), "{rest:?}");
            assert_eq!(&rest[4..], tail, "the copy is the tail, not other fields");
            verdict.matched_calls = verdict.real_calls;
        }
        assert!(
            verdict.is_full(),
            "{class}: the spec consumes {}/{} of the image's calls {:?}",
            verdict.matched_calls,
            verdict.real_calls,
            scanned.calls
        );
    }
    // The image's WifiInfo does not end with the AOSP-main `writeList(vendorData)`.
    let wifi_info = Scanner::write_to_parcel_boot("android.net.wifi.WifiInfo").unwrap();
    assert_eq!(wifi_info.calls.last().map(String::as_str), Some("writeBoolean"));
    assert!(!wifi_info.calls.iter().any(|call| call == "writeList"));
}

/// An independent reader of `WifiInfo(Parcel)` in the image's field order: the
/// bytes the module writes must parse as the framework's reader parses them and
/// leave nothing behind (a desync is a wrong value or a leftover word).
struct ParsedInfo {
    network_id: i32,
    rssi: i32,
    link_speed: i32,
    frequency: i32,
    ip: Option<Vec<u8>>,
    ssid: Option<Vec<u8>>,
    bssid: Option<String>,
    mac: Option<String>,
    usable: bool,
    supplicant: String,
    wifi_standard: i32,
    is_primary: i32,
    security: i32,
    mlo_link: i32,
}

fn read_wifi_info(parcel: &mut Parcel) -> ParsedInfo {
    let network_id = parcel.read_int32().unwrap();
    let rssi = parcel.read_int32().unwrap();
    let link_speed = parcel.read_int32().unwrap();
    parcel.read_int32().unwrap(); // tx
    parcel.read_int32().unwrap(); // rx
    let frequency = parcel.read_int32().unwrap();
    let ip = if parcel.read_byte().unwrap() == 1 {
        parcel.read_byte_array().unwrap()
    } else {
        None
    };
    let ssid = if parcel.read_int32().unwrap() == 1 {
        parcel.read_byte_array().unwrap()
    } else {
        None
    };
    let bssid = parcel.read_string16().unwrap();
    let mac = parcel.read_string16().unwrap();
    for _ in 0..7 {
        parcel.read_int32().unwrap(); // metered .. score
    }
    let usable = parcel.read_bool().unwrap();
    for _ in 0..4 {
        parcel.read_int64().unwrap();
        parcel.read_double().unwrap();
    }
    let supplicant = parcel.read_string16().unwrap().unwrap();
    parcel.read_int32().unwrap(); // osu ap
    for _ in 0..3 {
        parcel.read_string16().unwrap();
    }
    let wifi_standard = parcel.read_int32().unwrap();
    parcel.read_int32().unwrap(); // max tx
    parcel.read_int32().unwrap(); // max rx
    parcel.read_string16().unwrap(); // passpoint id
    parcel.read_int32().unwrap(); // subscription id
    assert_eq!(parcel.read_int32().unwrap(), -1, "information elements are a null list");
    let is_primary = parcel.read_int32().unwrap();
    let security = parcel.read_int32().unwrap();
    parcel.read_int32().unwrap(); // restricted
    parcel.read_string16().unwrap(); // network key
    assert_eq!(parcel.read_string16().unwrap(), None, "mApMldMacAddress is writeParcelable(null)");
    let mlo_link = parcel.read_int32().unwrap();
    assert_eq!(parcel.read_int32().unwrap(), 0, "no affiliated MLO links");
    parcel.read_bool().unwrap(); // tid-to-link mapping
    ParsedInfo {
        network_id,
        rssi,
        link_speed,
        frequency,
        ip,
        ssid,
        bssid,
        mac,
        usable,
        supplicant,
        wifi_standard,
        is_primary,
        security,
        mlo_link,
    }
}

fn payload_of(value: &acos_services::value::Parcelable) -> Parcel {
    let mut parcel = Parcel::new();
    parcelable::write_payload(&mut parcel, value).expect("payload writes");
    parcel.set_data_position(0);
    parcel
}

fn state_with(configure: impl FnOnce(&mut ModuleConfig)) -> (ServiceHost, ModuleRegistry) {
    let mut config = ModuleConfig::default();
    configure(&mut config);
    let mut host = ServiceHost::standard();
    let mut registry = ModuleRegistry::standard();
    registry
        .attach(&mut host, MODULE, Profile::Scripted, &config)
        .expect("net.wifi attaches");
    (host, registry)
}

fn config(value: serde_json::Value) -> ModuleConfig {
    ModuleConfig::from_json("net.wifi", &value).expect("config")
}

#[test]
fn wifi_info_bytes_parse_like_the_framework_reader_and_leave_nothing_behind() {
    // Associated (the stub defaults).
    let (mut host, mut registry) = state_with(|_| {});
    let reply = call(&mut host, "wifi", 41, strings(&["dev.acos.test", ""]));
    let mut reply = reply;
    assert_eq!(reply.read_int32().unwrap(), 1, "WifiInfo is a present typed object");
    let info = read_wifi_info(&mut reply);
    assert_eq!(reply.data_position(), reply.data_size(), "no bytes left behind");
    assert_eq!(info.network_id, 0);
    assert_eq!(info.rssi, -55);
    assert_eq!(info.link_speed, 72);
    assert_eq!(info.frequency, 2437);
    assert_eq!(info.ssid.as_deref(), Some(&b"AndroidWifi"[..]));
    assert_eq!(info.bssid.as_deref(), Some("00:13:10:85:fe:01"));
    assert_eq!(info.mac.as_deref(), Some("02:00:00:00:00:00"));
    assert!(info.usable);
    assert_eq!(info.supplicant, "COMPLETED", "SupplicantState is written by name");
    assert_eq!(info.wifi_standard, 4, "802.11n on 2.4 GHz");
    assert_eq!(info.is_primary, 1);
    assert_eq!(info.security, 2);
    assert_eq!(info.mlo_link, -1);
    assert!(
        info.ip.is_none(),
        "no `network` module -> no address (no fake connectivity)"
    );

    // Disconnected: `WifiInfo.reset()` values.
    registry
        .control(&mut host, MODULE, "disconnect", &json!({}))
        .expect("disconnect");
    let mut reply = call(&mut host, "wifi", 41, strings(&["dev.acos.test", ""]));
    assert_eq!(reply.read_int32().unwrap(), 1);
    let info = read_wifi_info(&mut reply);
    assert_eq!(reply.data_position(), reply.data_size());
    assert_eq!(info.network_id, -1);
    assert_eq!(info.rssi, wifi::INVALID_RSSI);
    assert_eq!(info.link_speed, -1);
    assert_eq!(info.frequency, -1);
    assert!(info.ssid.is_none(), "an absent SSID reads as <unknown ssid>");
    assert!(info.bssid.is_none());
    assert!(!info.usable);
    assert_eq!(info.supplicant, "DISCONNECTED");
    assert_eq!(info.is_primary, 0);
}

#[test]
fn redaction_without_scan_access_matches_wifi_info_make_copy() {
    let (mut host, _registry) = state_with(|c| *c = config(json!({"scan_access": false})));
    let mut reply = call(&mut host, "wifi", 41, strings(&["dev.acos.test", ""]));
    assert_eq!(reply.read_int32().unwrap(), 1);
    let info = read_wifi_info(&mut reply);
    assert!(info.ssid.is_none(), "SSID is location-sensitive");
    assert_eq!(info.bssid.as_deref(), Some(wifi::DEFAULT_MAC));
    assert_eq!(info.network_id, -1);
    assert_eq!(info.rssi, -55, "RSSI is not location-sensitive");

    // getScanResults is a SecurityException, not an empty list.
    let error = expect_error(&mut host, "wifi", 36, strings(&["dev.acos.test", ""]));
    assert!(
        matches!(error, ServiceError::Android { code, .. } if code == acos_services::exception::EX_SECURITY),
        "{error:?}"
    );
}

#[test]
fn scan_results_are_real_scan_result_parcels() {
    let (mut host, _registry) = state_with(|c| {
        *c = config(json!({
            "ssid": "Office",
            "bssid": "aa:bb:cc:dd:ee:01",
            "networks": [
                {"ssid": "Office", "bssid": "aa:bb:cc:dd:ee:01", "rssi": -48, "frequency": 2412},
                {"ssid": "Guest", "rssi": -81, "capabilities": "[ESS]"}
            ]
        }))
    });
    let mut reply = call(&mut host, "wifi", 36, strings(&["dev.acos.test", ""]));
    assert_eq!(reply.read_int32().unwrap(), 2, "two ScanResults");
    let mut seen = Vec::new();
    for _ in 0..2 {
        assert_eq!(reply.read_int32().unwrap(), 1, "typed-list element presence");
        // ScanResult(Parcel): the image order, field by field.
        assert_eq!(reply.read_int32().unwrap(), 1, "wifiSsid present");
        let ssid_bytes = reply.read_byte_array().unwrap().unwrap();
        let ssid = reply.read_string16().unwrap().unwrap();
        let bssid = reply.read_string16().unwrap().unwrap();
        assert_eq!(ssid_bytes, ssid.as_bytes());
        reply.read_int64().unwrap(); // hessid
        reply.read_int32().unwrap(); // anqp domain id
        let capabilities = reply.read_string16().unwrap().unwrap();
        let level = reply.read_int32().unwrap();
        let frequency = reply.read_int32().unwrap();
        reply.read_int64().unwrap(); // timestamp
        for _ in 0..6 {
            reply.read_int32().unwrap(); // distances, width, centres, standard
        }
        reply.read_int64().unwrap(); // seen
        reply.read_int32().unwrap(); // untrusted
        reply.read_int32().unwrap(); // numUsage
        reply.read_string16().unwrap(); // venue
        reply.read_string16().unwrap(); // operator
        reply.read_int64().unwrap(); // flags
        assert_eq!(reply.read_int32().unwrap(), 0, "no information elements");
        assert_eq!(reply.read_int32().unwrap(), 0, "no anqp lines");
        assert_eq!(reply.read_int32().unwrap(), 0, "no anqp elements");
        assert_eq!(reply.read_int32().unwrap(), 0, "no radio chain infos");
        assert_eq!(reply.read_string16().unwrap().as_deref(), Some("wlan0"));
        assert_eq!(reply.read_string16().unwrap(), None, "mApMldMacAddress null");
        reply.read_int32().unwrap(); // mlo link id
        assert_eq!(reply.read_int32().unwrap(), 0, "no affiliated links");
        seen.push((ssid, bssid, capabilities, level, frequency));
    }
    assert_eq!(reply.data_position(), reply.data_size(), "no bytes left behind");
    assert_eq!(
        seen[0],
        (
            "Office".to_string(),
            "aa:bb:cc:dd:ee:01".to_string(),
            "[WPA2-PSK-CCMP][ESS]".to_string(),
            -48,
            2412
        )
    );
    assert_eq!(seen[1].0, "Guest");
    assert_eq!(seen[1].1, "02:00:00:00:00:01", "a missing BSSID is derived, deterministically");
    assert_eq!(seen[1].3, -81);
}

#[test]
fn the_dhcp_info_is_zero_without_the_network_module() {
    // `session_has_network()` is the one gate; this test process never sets it.
    assert!(!acos_services::network::session_has_network());
    let (mut host, _registry) = state_with(|_| {});
    let mut reply = call(&mut host, "wifi", 57, strings(&["dev.acos.test"]));
    assert_eq!(reply.read_int32().unwrap(), 1, "DhcpInfo is present (zeros), not null");
    for field in 0..7 {
        assert_eq!(reply.read_int32().unwrap(), 0, "DhcpInfo field {field}");
    }
    assert_eq!(reply.data_position(), reply.data_size());
}

// ---------------------------------------------------------------------------
// The service
// ---------------------------------------------------------------------------

fn strings(items: &[&str]) -> Parcel {
    let mut parcel = Parcel::new();
    parcel.write_interface_token(DESCRIPTOR).expect("token");
    for item in items {
        if item.is_empty() {
            parcel.write_string16_opt(None).unwrap();
        } else {
            parcel.write_string16(item).unwrap();
        }
    }
    parcel
}

fn bare() -> Parcel {
    let mut parcel = Parcel::new();
    parcel.write_interface_token(DESCRIPTOR).expect("token");
    parcel
}

fn call(host: &mut ServiceHost, name: &str, code: u32, data: Parcel) -> Parcel {
    let binder = host.check_service(name).unwrap().unwrap();
    let mut data = data;
    data.set_data_position(0);
    let mut reply = Parcel::new();
    host.transact(binder, code, &mut data, &mut reply, 0)
        .unwrap_or_else(|error| panic!("{name} code {code} failed: {error}"));
    reply.set_data_position(0);
    assert_eq!(reply.read_int32().expect("exception header"), 0, "no exception");
    reply
}

fn expect_error(host: &mut ServiceHost, name: &str, code: u32, data: Parcel) -> ServiceError {
    let binder = host.check_service(name).unwrap().unwrap();
    let mut data = data;
    data.set_data_position(0);
    let mut reply = Parcel::new();
    host.transact(binder, code, &mut data, &mut reply, 0)
        .expect_err("the call must fail")
}

#[test]
fn the_fixed_answers_are_what_an_unprivileged_app_gets() {
    let (mut host, _registry) = state_with(|_| {});
    // 43 getWifiEnabledState
    assert_eq!(
        call(&mut host, "wifi", 43, bare()).read_int32().unwrap(),
        wifi::WIFI_STATE_ENABLED
    );
    // 42 setWifiEnabled(pkg, false) -> false, state unchanged (target >= Q)
    let mut data = strings(&["dev.acos.test"]);
    data.write_int32(0);
    assert_eq!(call(&mut host, "wifi", 42, data).read_int32().unwrap(), 0);
    assert_eq!(
        call(&mut host, "wifi", 43, bare()).read_int32().unwrap(),
        wifi::WIFI_STATE_ENABLED
    );
    // 38 disconnect / 39 reconnect: false for an app
    assert_eq!(
        call(&mut host, "wifi", 38, strings(&["dev.acos.test"]))
            .read_int32()
            .unwrap(),
        0
    );
    // 1 getSupportedFeatures: INFRA only
    assert_eq!(call(&mut host, "wifi", 1, bare()).read_int64().unwrap(), 1);
    // 52..55 bands: 2.4 GHz only
    for (code, supported) in [(52, 1), (53, 0), (54, 0), (55, 0)] {
        assert_eq!(
            call(&mut host, "wifi", code, bare()).read_int32().unwrap(),
            supported,
            "band code {code}"
        );
    }
    // 137 calculateSignalLevel: the platform thresholds
    for (rssi, level) in [(-100, 0), (-88, 1), (-77, 2), (-66, 3), (-55, 4), (-30, 4)] {
        let mut data = bare();
        data.write_int32(rssi);
        assert_eq!(
            call(&mut host, "wifi", 137, data).read_int32().unwrap(),
            level,
            "rssi {rssi}"
        );
        assert_eq!(wifi::signal_level(rssi), level);
    }
    // 101 getCurrentNetwork: the default network while up
    let mut reply = call(&mut host, "wifi", 101, bare());
    assert_eq!(reply.read_int32().unwrap(), 1);
    assert_eq!(
        reply.read_int32().unwrap(),
        acos_services::connectivity::NET_ID
    );
}

#[test]
fn an_unmodelled_method_is_a_structured_unsupported_never_a_canned_success() {
    let (mut host, mut registry) = state_with(|_| {});
    // 9 getConfiguredNetworks, 76 startLocalOnlyHotspot
    for code in [9, 76, 128] {
        let error = expect_error(&mut host, "wifi", code, bare());
        assert!(
            matches!(error, ServiceError::UnsupportedAndroidApi { .. }),
            "code {code}: {error:?}"
        );
    }
    let state = registry
        .control(&mut host, MODULE, "state", &json!({}))
        .unwrap();
    assert_eq!(state["unsupported"].as_array().unwrap().len(), 3);
}

#[test]
fn a_stub_is_fixed_and_a_scripted_radio_is_driven() {
    let mut host = ServiceHost::standard();
    let mut registry = ModuleRegistry::standard();
    registry
        .attach(&mut host, MODULE, Profile::Stub, &ModuleConfig::default())
        .expect("stub attaches");
    // Reading is allowed on a stub; driving it is not.
    registry
        .control(&mut host, MODULE, "state", &json!({}))
        .expect("state on a stub");
    for op in ["enable", "connect", "disconnect", "rssi", "scan"] {
        let error = registry
            .control(&mut host, MODULE, op, &json!({"on": true, "rssi": -60}))
            .expect_err("a stub is fixed");
        assert!(
            error.to_string().contains("scripted"),
            "{op}: the refusal names the profile to attach: {error}"
        );
    }
    // Unknown ops and unknown config keys are refused, not ignored.
    assert!(registry
        .control(&mut host, MODULE, "fly", &json!({}))
        .is_err());
    let mut host2 = ServiceHost::standard();
    let mut registry2 = ModuleRegistry::standard();
    let error = registry2
        .attach(
            &mut host2,
            MODULE,
            Profile::Scripted,
            &config(json!({"connected": false})),
        )
        .expect_err("`connected` is not an attach-time key");
    assert!(error.to_string().contains("disconnect"), "{error}");
    // `real` is refused: there is no radio.
    let error = registry2
        .attach(&mut host2, MODULE, Profile::Real, &ModuleConfig::default())
        .expect_err("no real profile");
    assert!(error.to_string().contains("not implemented"), "{error}");
}

#[test]
fn the_launch_spelling_attaches_a_configured_ssid() {
    let spec = ModuleSpec::parse(r#"net.wifi:scripted,{"ssid":"Cafe","rssi":-70}"#).unwrap();
    let mut host = ServiceHost::standard();
    let mut registry = ModuleRegistry::standard();
    registry
        .attach(&mut host, &spec.id, spec.profile, &spec.config)
        .expect("attaches");
    let state = registry
        .control(&mut host, MODULE, "state", &json!({}))
        .unwrap();
    assert_eq!(state["ssid"], "Cafe");
    assert_eq!(state["rssi"], -70);
    assert_eq!(state["connected"], true);
    // The configured network is what a scan finds.
    assert_eq!(state["scan_results"][0]["ssid"], "Cafe");
}

// ---------------------------------------------------------------------------
// Broadcasts and the default network
// ---------------------------------------------------------------------------

fn register_receiver(host: &mut ServiceHost, wire: u64, actions: &[&str]) {
    let shared = host
        .ensure("android.app.IActivityManager")
        .expect("activity manager")
        .as_any_mut()
        .and_then(|any| any.downcast_mut::<acos_services::atms::AtmsService>())
        .expect("AtmsService")
        .shared_state();
    shared.lock().unwrap().ams.register_receiver(
        (1 << 32) | wire,
        "dev.acos.test",
        actions.iter().map(|a| (*a).to_string()).collect(),
        Vec::new(),
        2,
    );
}

fn delivered_intents(host: &mut ServiceHost) -> Vec<IntentFacts> {
    host.take_deliveries()
        .into_iter()
        .filter(|delivery| delivery.interface == "android.content.IIntentReceiver")
        .map(|delivery| {
            let mut data = delivery.data.clone();
            data.set_data_position(0);
            data.enforce_interface("android.content.IIntentReceiver")
                .expect("token");
            assert_eq!(data.read_int32().unwrap(), 1);
            let intent = parcelable::read_intent(&mut data).expect("Intent parses");
            IntentFacts::from_parcelable(&intent)
        })
        .collect()
}

#[test]
fn enable_broadcasts_the_state_transitions_in_order() {
    let (mut host, mut registry) = state_with(|_| {});
    register_receiver(&mut host, 7, &[wifi::ACTION_WIFI_STATE_CHANGED]);
    registry
        .control(&mut host, MODULE, "enable", &json!({"on": false}))
        .expect("off");
    let intents = delivered_intents(&mut host);
    let states: Vec<(i32, i32)> = intents
        .iter()
        .map(|intent| {
            let int = |key: &str| match intent.extras.get(key) {
                Some(acos_services::ams::Extra::Int(value)) => *value,
                other => panic!("{key}: {other:?}"),
            };
            (int("wifi_state"), int("previous_wifi_state"))
        })
        .collect();
    assert_eq!(
        states,
        vec![
            (wifi::WIFI_STATE_DISABLING, wifi::WIFI_STATE_ENABLED),
            (wifi::WIFI_STATE_DISABLED, wifi::WIFI_STATE_DISABLING),
        ]
    );
    assert_eq!(
        call(&mut host, "wifi", 43, bare()).read_int32().unwrap(),
        wifi::WIFI_STATE_DISABLED
    );
    // A radio that is off scans nothing and associates with nothing.
    let error = registry
        .control(&mut host, MODULE, "connect", &json!({}))
        .expect_err("connect while off");
    assert!(error.to_string().contains("off"), "{error}");
    assert!(registry
        .control(&mut host, MODULE, "scan", &json!({}))
        .is_err());
    // Getting it back on reconnects (autoconnect) and says so.
    registry
        .control(&mut host, MODULE, "enable", &json!({"on": true}))
        .expect("on");
    let intents = delivered_intents(&mut host);
    assert_eq!(
        intents
            .iter()
            .map(|intent| intent.action.clone().unwrap())
            .collect::<Vec<_>>(),
        vec![
            wifi::ACTION_WIFI_STATE_CHANGED.to_string(),
            wifi::ACTION_WIFI_STATE_CHANGED.to_string()
        ],
        "only the registered action is delivered"
    );
}

#[test]
fn connect_and_disconnect_move_the_default_network_and_say_so_in_broadcasts() {
    let (mut host, mut registry) = state_with(|_| {});
    register_receiver(&mut host, 7, &[wifi::ACTION_NETWORK_STATE_CHANGED]);
    // A NetworkCallback registered the way the framework does (a Messenger).
    let connectivity = host.connectivity_state().expect("a connectivity state");
    let messenger = (1 << 32) | 41;
    {
        let mut state = connectivity.lock().unwrap();
        state.requests.insert(
            1,
            acos_services::connectivity::RequestRecord {
                request_id: 1,
                kind: acos_services::connectivity::RequestType::Listen,
                legacy_type: -1,
                callback_flags: 0,
                messenger: Some(acos_parcel::BinderId(messenger)),
                request: acos_services::connectivity::network_request(
                    acos_services::connectivity::default_network_capabilities(),
                    -1,
                    1,
                    acos_services::connectivity::RequestType::Listen,
                ),
                uid: -1,
                package: "dev.acos.test".to_string(),
            },
        );
    }

    let lost = registry
        .control(&mut host, MODULE, "disconnect", &json!({}))
        .expect("disconnect");
    assert_eq!(lost["callbacks_queued"], 1, "one onLost for the one callback");
    assert_eq!(lost["default_network_up"], false);
    let deliveries = host.take_deliveries();
    assert!(
        deliveries
            .iter()
            .any(|d| d.interface == "android.os.IMessenger" && d.method == "CALLBACK_LOST"),
        "onLost reaches the messenger: {:?}",
        deliveries.iter().map(|d| &d.method).collect::<Vec<_>>()
    );
    assert!(!connectivity.lock().unwrap().default_up);
    // getCurrentNetwork and the wifi info agree.
    let mut reply = call(&mut host, "wifi", 101, bare());
    assert_eq!(reply.read_int32().unwrap(), 0, "no current network");
    // The NETWORK_STATE_CHANGED broadcast was queued with DISCONNECTED.
    // (`take_deliveries` above consumed the receiver delivery; script again.)
    registry
        .control(&mut host, MODULE, "connect", &json!({"ssid": "Cafe", "rssi": -62}))
        .expect("connect");
    let deliveries = host.take_deliveries();
    let methods: Vec<&str> = deliveries.iter().map(|d| d.method.as_str()).collect();
    assert!(
        methods.contains(&"CALLBACK_AVAILABLE"),
        "onAvailable after connect: {methods:?}"
    );
    assert!(connectivity.lock().unwrap().default_up);
    let state = registry
        .control(&mut host, MODULE, "state", &json!({}))
        .unwrap();
    assert_eq!(state["ssid"], "Cafe");
    assert_eq!(state["default_network_up"], true);
    // The receiver saw a CONNECTED broadcast with the WifiInfo extra.
    let intents: Vec<IntentFacts> = deliveries
        .into_iter()
        .filter(|d| d.interface == "android.content.IIntentReceiver")
        .map(|delivery| {
            let mut data = delivery.data.clone();
            data.set_data_position(0);
            data.enforce_interface("android.content.IIntentReceiver").unwrap();
            data.read_int32().unwrap();
            IntentFacts::from_parcelable(&parcelable::read_intent(&mut data).unwrap())
        })
        .collect();
    assert_eq!(intents.len(), 1);
    assert!(intents[0].extras.contains_key("wifiInfo"));
    assert!(intents[0].extras.contains_key("networkInfo"));
    assert_eq!(
        intents[0].extras.get("bssid"),
        Some(&acos_services::ams::Extra::String("00:13:10:85:fe:01".to_string()))
    );
}

#[test]
fn rssi_and_scan_are_broadcast_and_a_scan_callback_is_told() {
    let (mut host, mut registry) = state_with(|_| {});
    register_receiver(
        &mut host,
        7,
        &[wifi::ACTION_RSSI_CHANGED, wifi::ACTION_SCAN_RESULTS],
    );
    // The app registers an IScanResultsCallback (a guest binder).
    let callback = (1 << 32) | 55;
    let mut data = bare();
    data.write_strong_binder(acos_parcel::BinderId(callback));
    call(&mut host, "wifi", 131, data);

    registry
        .control(&mut host, MODULE, "rssi", &json!({"rssi": -73}))
        .expect("rssi");
    let intents = delivered_intents(&mut host);
    assert_eq!(intents.len(), 1);
    assert_eq!(
        intents[0].extras.get("newRssi"),
        Some(&acos_services::ams::Extra::Int(-73))
    );
    let mut reply = call(&mut host, "wifi", 41, strings(&["dev.acos.test", ""]));
    reply.read_int32().unwrap();
    assert_eq!(read_wifi_info(&mut reply).rssi, -73);

    // The control op completes a scan: broadcast + callback.
    registry
        .control(
            &mut host,
            MODULE,
            "scan",
            &json!({"results": [{"ssid": "New", "rssi": -50}]}),
        )
        .expect("scan");
    let deliveries = host.take_deliveries();
    assert!(deliveries.iter().any(|d| d.target.0 == callback
        && d.interface == "android.net.wifi.IScanResultsCallback"
        && d.method == "onScanResultsAvailable"));
    assert!(deliveries
        .iter()
        .any(|d| d.interface == "android.content.IIntentReceiver"));

    // An app-initiated startScan ends the same way — through the host's
    // broadcast drain, not a control op.
    let reply = call(&mut host, "wifi", 35, strings(&["dev.acos.test", ""]));
    let mut reply = reply;
    assert_eq!(reply.read_int32().unwrap(), 1, "startScan accepted");
    let deliveries = host.take_deliveries();
    assert!(deliveries.iter().any(|d| d.target.0 == callback
        && d.method == "onScanResultsAvailable"));
    assert!(
        deliveries
            .iter()
            .any(|d| d.interface == "android.content.IIntentReceiver"),
        "SCAN_RESULTS_AVAILABLE reaches the registered receiver"
    );
}

#[test]
fn detach_removes_the_service_and_android_s_missing_path_returns() {
    let (mut host, mut registry) = state_with(|_| {});
    assert!(host.check_service("wifi").unwrap().is_some());
    registry.detach(&mut host, MODULE).expect("detach");
    assert!(
        host.check_service("wifi").unwrap().is_none(),
        "`wifi` is null again: WifiFrameworkInitializer returns no manager"
    );
}

/// MODFILL root cause: `PmsService::set_package_from_apk_as` rebuilt the package
/// table with a fresh `FeatureTable`, so the features a module turned on at
/// attach were withdrawn when the session installed the launched APK (modules
/// attach *first*). `WifiManager`'s factory is gated on FEATURE_WIFI, so the app
/// got a null manager although the module was attached. Fails before the fix:
/// the feature reads `false` after the install.
#[test]
fn a_module_feature_survives_the_launched_apk_install() {
    let apk = acos_services::workspace_path("fixtures/build/apps/wifi-probe.apk")
        .expect("repo root");
    if !apk.exists() {
        panic!("build fixtures first: tools/build/build-apps.sh ({} missing)", apk.display());
    }
    let (mut host, _registry) = state_with(|_| {});
    let has_feature = |host: &mut ServiceHost| -> bool {
        let pm = host.check_service("package").unwrap().unwrap();
        let method = acos_aidl::lookup("android.content.pm.IPackageManager", 0);
        let _ = method;
        let code = (1..400u32)
            .find(|code| {
                acos_aidl::lookup("android.content.pm.IPackageManager", *code)
                    .is_some_and(|method| method.name == "hasSystemFeature")
            })
            .expect("hasSystemFeature is in the decoded table");
        let mut data = Parcel::new();
        data.write_interface_token("android.content.pm.IPackageManager")
            .unwrap();
        data.write_string16("android.hardware.wifi").unwrap();
        data.write_int32(0);
        data.set_data_position(0);
        let mut reply = Parcel::new();
        host.transact(pm, code, &mut data, &mut reply, 0).unwrap();
        reply.set_data_position(0);
        assert_eq!(reply.read_int32().unwrap(), 0);
        reply.read_int32().unwrap() != 0
    };
    assert!(has_feature(&mut host), "attach turns FEATURE_WIFI on");
    host.install_package_from_apk(&apk).expect("install");
    assert!(
        has_feature(&mut host),
        "the APK install must not withdraw a module's feature"
    );
}
