//! `net.telephony`: the scripted answers, the SMS injection and the call-state
//! event, end to end at the service-host level.
//!
//! Everything here runs the **host** half of a session: the module's real
//! `ServiceManager` entries, the framework's `IIntentReceiver.performReceive`
//! transaction and the `IPhoneStateListener` callback. The app half (a real
//! `SmsMessage.createFromPdu` decode and a `PhoneStateListener` callback) is
//! proven by the ART fixture (`fixtures/apps/tail-probe`) under
//! `results/handoff/MODTAIL/`.

use acos_parcel::Parcel;
use acos_services::ams::IntentFacts;
use acos_services::module::{ModuleConfig, ModuleRegistry, Profile};
use acos_services::{ServiceError, ServiceHost};

const MODULE: &str = "net.telephony";

/// A Java-side binder id as the peer tags it (`JAVA_TAG | wire`).
fn java_binder(wire: u64) -> u64 {
    (1 << 32) | wire
}

fn open(host: &mut ServiceHost, name: &str, code: u32) -> (u32, Parcel, Parcel) {
    let binder = host
        .check_service(name)
        .expect("lookup is infallible")
        .unwrap_or_else(|| panic!("{name} did not resolve to a binder"));
    let descriptor = host.descriptor_of(binder).unwrap_or("<unknown>").to_string();
    let mut data = Parcel::new();
    data.write_interface_token(&descriptor).expect("token");
    (code, data, Parcel::new())
}

fn call(host: &mut ServiceHost, name: &str, code: u32, data: Parcel) -> Parcel {
    let binder = host.check_service(name).unwrap().unwrap();
    let mut data = data;
    data.set_data_position(0);
    let mut reply = Parcel::new();
    host.transact(binder, code, &mut data, &mut reply, 0)
        .unwrap_or_else(|error| panic!("{name} code {code} failed: {error}"));
    reply.set_data_position(0);
    // Skip the AIDL reply header (`writeNoException`).
    reply.read_int32().expect("exception header");
    reply
}

fn start() -> (ServiceHost, ModuleRegistry) {
    let mut host = ServiceHost::standard();
    let mut registry = ModuleRegistry::standard();
    registry
        .attach(&mut host, MODULE, Profile::Scripted, &ModuleConfig::default())
        .expect("net.telephony attaches");
    (host, registry)
}

#[test]
fn the_scripted_answers_are_the_ones_the_framework_asks_for() {
    let (mut host, _registry) = start();

    // 350 getSimStateForSlotIndex(int)
    let (code, mut data, _) = open(&mut host, "phone", 350);
    data.write_int32(0);
    let mut reply = call(&mut host, "phone", code, data);
    assert_eq!(reply.read_int32().unwrap(), 5, "SIM_STATE_READY");

    // 41 getActivePhoneTypeForSlot(int)
    let (code, mut data, _) = open(&mut host, "phone", 41);
    data.write_int32(0);
    let mut reply = call(&mut host, "phone", code, data);
    assert_eq!(reply.read_int32().unwrap(), 1, "PHONE_TYPE_GSM");

    // 64 getNetworkTypeForSubscriber(int, String, String)
    let (code, mut data, _) = open(&mut host, "phone", 64);
    data.write_int32(-1);
    data.write_string16("dev.acos.fixtures.tailprobe").unwrap();
    data.write_string16_opt(None).unwrap();
    let mut reply = call(&mut host, "phone", code, data);
    assert_eq!(reply.read_int32().unwrap(), 13, "NETWORK_TYPE_LTE");

    // 69 hasIccCardUsingSlotIndex(int)
    let (code, mut data, _) = open(&mut host, "phone", 69);
    data.write_int32(0);
    let mut reply = call(&mut host, "phone", code, data);
    assert_eq!(reply.read_int32().unwrap(), 1, "an ICC is present");

    // 125 getLine1NumberForDisplay(int, String, String)
    let (code, mut data, _) = open(&mut host, "phone", 125);
    data.write_int32(-1);
    data.write_string16("dev.acos.fixtures.tailprobe").unwrap();
    data.write_string16_opt(None).unwrap();
    let mut reply = call(&mut host, "phone", code, data);
    assert_eq!(
        reply.read_string16().unwrap().as_deref(),
        Some("+15551234567")
    );

    // 35 getCallStateForSubscription(int, String, String)
    let (code, mut data, _) = open(&mut host, "phone", 35);
    data.write_int32(-1);
    data.write_string16("dev.acos.fixtures.tailprobe").unwrap();
    data.write_string16_opt(None).unwrap();
    let mut reply = call(&mut host, "phone", code, data);
    assert_eq!(reply.read_int32().unwrap(), 0, "CALL_STATE_IDLE");

    // 16 IPhoneSubInfo.getLine1NumberForSubscriber(int, String, String)
    let (code, mut data, _) = open(&mut host, "iphonesubinfo", 16);
    data.write_int32(-1);
    data.write_string16("dev.acos.fixtures.tailprobe").unwrap();
    data.write_string16_opt(None).unwrap();
    let mut reply = call(&mut host, "iphonesubinfo", code, data);
    assert_eq!(
        reply.read_string16().unwrap().as_deref(),
        Some("+15551234567")
    );
}

#[test]
fn an_unsupported_telephony_method_is_a_structured_error_naming_the_module() {
    let (mut host, _registry) = start();
    // 500 is not a method this image's ITelephony has.
    let (code, data, _) = open(&mut host, "phone", 9999);
    let binder = host.check_service("phone").unwrap().unwrap();
    let mut data = data;
    data.set_data_position(0);
    let mut reply = Parcel::new();
    let error = host
        .transact(binder, code, &mut data, &mut reply, 0)
        .expect_err("an unimplemented method must not succeed");
    assert!(
        matches!(error, ServiceError::UnsupportedAndroidApi { .. }),
        "expected UNSUPPORTED_ANDROID_API, got {error:?}"
    );
}

#[test]
fn send_text_for_subscriber_is_recorded_with_the_destination_and_body() {
    let mut host = ServiceHost::standard();
    let mut registry = ModuleRegistry::standard();
    registry
        .attach(&mut host, MODULE, Profile::Scripted, &ModuleConfig::default())
        .unwrap();
    let (code, mut data, _) = open(&mut host, "isms", 5);
    data.write_int32(-1);
    data.write_string16("dev.acos.fixtures.tailprobe").unwrap();
    data.write_string16_opt(None).unwrap();
    data.write_string16("+15557654321").unwrap();
    data.write_string16_opt(None).unwrap();
    data.write_string16("hello from the app").unwrap();
    data.write_int32(0); // no sentIntent
    data.write_int32(0); // no deliveryIntent
    let _ = call(&mut host, "isms", code, data);

    let state = registry
        .control(&mut host, MODULE, "state", &serde_json::Value::Null)
        .unwrap();
    let sent = state["sent"].as_array().expect("sent[]");
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0]["destination"], "+15557654321");
    assert_eq!(sent[0]["text"], "hello from the app");
    assert_eq!(sent[0]["sub_id"], -1);
    assert_eq!(sent[0]["has_sent_intent"], false);
}

#[test]
fn sms_injects_a_real_sms_received_broadcast_that_reaches_a_registered_receiver() {
    let mut host = ServiceHost::standard();
    let mut registry = ModuleRegistry::standard();
    registry
        .attach(&mut host, MODULE, Profile::Scripted, &ModuleConfig::default())
        .unwrap();

    // Register a dynamic receiver for SMS_RECEIVED, as the app does.
    let receiver = java_binder(7);
    let shared = host
        .ensure("android.app.IActivityManager")
        .expect("activity manager")
        .as_any_mut()
        .and_then(|any| any.downcast_mut::<acos_services::atms::AtmsService>())
        .expect("AtmsService")
        .shared_state();
    shared.lock().unwrap().ams.register_receiver(
        receiver,
        "dev.acos.fixtures.tailprobe",
        vec!["android.provider.Telephony.SMS_RECEIVED".into()],
        Vec::new(),
        2,
    );

    let reply = registry
        .control(
            &mut host,
            MODULE,
            "sms",
            &serde_json::json!({"from": "+15550001111", "body": "ACOS SMS"}),
        )
        .expect("sms injection");
    assert_eq!(reply["matched"], 1, "the registered receiver matched");
    let pdu_hex = reply["pdu"].as_str().expect("pdu hex").to_string();
    assert!(!pdu_hex.is_empty());

    let deliveries = host.take_deliveries();
    assert_eq!(deliveries.len(), 1, "one performReceive delivery");
    let delivery = &deliveries[0];
    assert_eq!(delivery.target.0, receiver);
    assert_eq!(delivery.interface, "android.content.IIntentReceiver");
    assert_eq!(delivery.code, 1, "performReceive");
    let mut data = delivery.data.clone();
    data.set_data_position(0);
    data.enforce_interface("android.content.IIntentReceiver")
        .expect("performReceive token");
    assert_eq!(
        data.read_int32().unwrap(),
        1,
        "the Intent is a present typed object"
    );
    let intent = acos_services::parcelable::read_intent(&mut data).expect("the Intent parses");
    let facts = IntentFacts::from_parcelable(&intent);
    assert_eq!(
        facts.action.as_deref(),
        Some("android.provider.Telephony.SMS_RECEIVED")
    );
    match facts.extras.get("pdus") {
        Some(acos_services::ams::Extra::ByteArrays(pdus)) => {
            assert_eq!(pdus.len(), 1);
            assert_eq!(
                pdus[0],
                (0..pdu_hex.len())
                    .step_by(2)
                    .map(|i| u8::from_str_radix(&pdu_hex[i..i + 2], 16).unwrap())
                    .collect::<Vec<u8>>()
            );
            // GSM 03.40: SMSC length 0, then the SMS-DELIVER first octet's
            // TP-MTI (bits 0-1) is 0b00 (deliver; 0b01 would be a submit).
            assert_eq!(pdus[0][0], 0x00);
            assert_eq!(pdus[0][1] & 0x03, 0x00);
        }
        other => panic!("expected a `pdus` Object[] of byte[], got {other:?}"),
    }
    match facts.extras.get("format") {
        Some(acos_services::ams::Extra::String(format)) => assert_eq!(format, "3gpp"),
        other => panic!("expected `format=3gpp`, got {other:?}"),
    }
}

#[test]
fn a_call_state_change_reaches_a_phone_state_listener_and_a_telephony_callback() {
    let mut host = ServiceHost::standard();
    let mut registry = ModuleRegistry::standard();
    registry
        .attach(&mut host, MODULE, Profile::Scripted, &ModuleConfig::default())
        .unwrap();

    // `PhoneStateListener.listen(LISTEN_CALL_STATE)` registers
    // EVENT_LEGACY_CALL_STATE_CHANGED (36).
    let legacy = java_binder(11);
    let (code, mut data, _) = open(&mut host, "telephony.registry", 4);
    data.write_int32(0); // renounceFineLocationAccess
    data.write_int32(0); // renounceCoarseLocationAccess
    data.write_int32(-1); // subId
    data.write_string16("dev.acos.fixtures.tailprobe").unwrap();
    data.write_string16_opt(None).unwrap();
    data.write_strong_binder(acos_parcel::BinderId(legacy));
    data.write_int32_array(Some(&[36])).unwrap();
    data.write_int32(1); // notifyNow
    let _ = call(&mut host, "telephony.registry", code, data);

    // The fresh listener gets the current state immediately (Android's registry
    // does), through `onLegacyCallStateChanged`.
    let notified = host.take_deliveries();
    assert_eq!(notified.len(), 1, "notifyNow delivers the current state");
    assert_eq!(notified[0].target.0, legacy);
    assert_eq!(notified[0].code, 6, "onLegacyCallStateChanged");

    // A `TelephonyCallback.CallStateListener` registers
    // EVENT_CALL_STATE_CHANGED (6).
    let modern = java_binder(12);
    let (code, mut data, _) = open(&mut host, "telephony.registry", 4);
    data.write_int32(0);
    data.write_int32(0);
    data.write_int32(-1);
    data.write_string16("dev.acos.fixtures.tailprobe").unwrap();
    data.write_string16_opt(None).unwrap();
    data.write_strong_binder(acos_parcel::BinderId(modern));
    data.write_int32_array(Some(&[6])).unwrap();
    data.write_int32(1);
    let _ = call(&mut host, "telephony.registry", code, data);
    let _ = host.take_deliveries();

    let reply = registry
        .control(
            &mut host,
            MODULE,
            "call_state",
            &serde_json::json!({"state": "ringing", "number": "+15550002222"}),
        )
        .expect("call state");
    assert_eq!(reply["call_state"], 1, "CALL_STATE_RINGING");
    assert_eq!(reply["delivered"], 2, "both listeners got the event");

    let deliveries = host.take_deliveries();
    assert_eq!(deliveries.len(), 2);
    let mut legacy_delivery = deliveries
        .iter()
        .find(|d| d.target.0 == legacy)
        .expect("legacy listener")
        .data
        .clone();
    legacy_delivery.set_data_position(0);
    legacy_delivery
        .enforce_interface("com.android.internal.telephony.IPhoneStateListener")
        .expect("listener token");
    assert_eq!(
        legacy_delivery.read_int32().unwrap(),
        1,
        "CALL_STATE_RINGING"
    );
    assert_eq!(
        legacy_delivery.read_string16().unwrap().as_deref(),
        Some("+15550002222"),
        "the legacy callback carries the number"
    );
    let mut modern_delivery = deliveries
        .iter()
        .find(|d| d.target.0 == modern)
        .expect("modern listener")
        .data
        .clone();
    modern_delivery.set_data_position(0);
    modern_delivery
        .enforce_interface("com.android.internal.telephony.IPhoneStateListener")
        .expect("listener token");
    assert_eq!(
        modern_delivery.read_int32().unwrap(),
        1,
        "onCallStateChanged(int) carries CALL_STATE_RINGING"
    );
    assert!(
        modern_delivery.read_int32().is_err(),
        "onCallStateChanged(int) carries only the state, no number"
    );

    // The synchronous query agrees with the event.
    let (code, mut data, _) = open(&mut host, "phone", 35);
    data.write_int32(-1);
    data.write_string16("dev.acos.fixtures.tailprobe").unwrap();
    data.write_string16_opt(None).unwrap();
    let mut reply = call(&mut host, "phone", code, data);
    assert_eq!(reply.read_int32().unwrap(), 1);
}

#[test]
fn detach_leaves_no_registration_and_control_refuses_a_detached_module() {
    let mut host = ServiceHost::standard();
    let mut registry = ModuleRegistry::standard();
    registry
        .attach(&mut host, MODULE, Profile::Scripted, &ModuleConfig::default())
        .unwrap();
    assert!(host.check_service("phone").unwrap().is_some());
    registry.detach(&mut host, MODULE).unwrap();
    for name in ["phone", "iphonesubinfo", "telephony.registry", "isms"] {
        assert!(
            host.check_service(name).unwrap().is_none(),
            "{name} must be null after detach (AOSP's honest missing path)"
        );
    }
    let error = registry
        .control(&mut host, MODULE, "sms", &serde_json::json!({}))
        .expect_err("a detached module must refuse");
    assert!(matches!(error, ServiceError::ModuleRejected { .. }));
}

#[test]
fn the_scripted_config_sets_the_initial_state() {
    let mut host = ServiceHost::standard();
    let mut registry = ModuleRegistry::standard();
    let config = ModuleConfig::from_json(
        MODULE,
        &serde_json::json!({
            "sim_state": "absent",
            "icc_present": false,
            "phone_type": "cdma",
            "network_type": "nr",
            "line1": null,
        }),
    )
    .unwrap();
    registry
        .attach(&mut host, MODULE, Profile::Scripted, &config)
        .unwrap();
    let state = registry
        .control(&mut host, MODULE, "state", &serde_json::Value::Null)
        .unwrap();
    assert_eq!(state["sim_state"], 1);
    assert_eq!(state["icc_present"], false);
    assert_eq!(state["phone_type"], 2);
    assert_eq!(state["network_type"], 20);
    assert_eq!(state["line1"], serde_json::Value::Null);

    // A phone with no SIM cannot send an SMS; the module refuses with a
    // structured error instead of recording a send that cannot happen.
    let (code, mut data, _) = open(&mut host, "isms", 5);
    data.write_int32(-1);
    data.write_string16("dev.acos.fixtures.tailprobe").unwrap();
    data.write_string16_opt(None).unwrap();
    data.write_string16("+15557654321").unwrap();
    data.write_string16_opt(None).unwrap();
    data.write_string16("nope").unwrap();
    data.write_int32(0);
    data.write_int32(0);
    let binder = host.check_service("isms").unwrap().unwrap();
    let mut data2 = data;
    data2.set_data_position(0);
    let mut reply = Parcel::new();
    let error = host
        .transact(binder, code, &mut data2, &mut reply, 0)
        .expect_err("no SIM, no send");
    assert!(
        error.to_string().contains("ABSENT"),
        "the error must name the absent SIM: {error}"
    );
}
