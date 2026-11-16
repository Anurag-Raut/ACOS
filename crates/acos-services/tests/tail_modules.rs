//! `hw.bluetooth` and `hw.biometric`: the scripted binders and the two event
//! channels (broadcast + result receiver, and the authentication callback).

use acos_parcel::{BinderId, Parcel};
use acos_services::ams::IntentFacts;
use acos_services::module::{ModuleConfig, ModuleRegistry, Profile};
use acos_services::{ServiceError, ServiceHost};
use acos_services::value::DynamicValue;

fn java_binder(wire: u64) -> u64 {
    (1 << 32) | wire
}

fn attach(module: &str) -> (ServiceHost, ModuleRegistry) {
    let mut host = ServiceHost::standard();
    let mut registry = ModuleRegistry::standard();
    registry
        .attach(&mut host, module, Profile::Scripted, &ModuleConfig::default())
        .expect("module attaches");
    (host, registry)
}

fn open(host: &mut ServiceHost, name: &str, code: u32) -> (u32, Parcel) {
    let binder = host
        .check_service(name)
        .expect("lookup")
        .unwrap_or_else(|| panic!("{name} did not resolve"));
    let descriptor = host.descriptor_of(binder).unwrap_or("<unknown>").to_string();
    let mut data = Parcel::new();
    data.write_interface_token(&descriptor).expect("token");
    (code, data)
}

fn transact(host: &mut ServiceHost, name: &str, code: u32, data: Parcel) -> Result<Parcel, ServiceError> {
    let binder = host.check_service(name).unwrap().unwrap();
    let mut data = data;
    data.set_data_position(0);
    let mut reply = Parcel::new();
    host.transact(binder, code, &mut data, &mut reply, 0)?;
    reply.set_data_position(0);
    reply.read_int32().expect("exception header");
    Ok(reply)
}

/// A oneway transaction: the host writes no reply at all.
fn transact_oneway(host: &mut ServiceHost, name: &str, code: u32, data: Parcel) {
    let binder = host.check_service(name).unwrap().unwrap();
    let mut data = data;
    data.set_data_position(0);
    let mut reply = Parcel::new();
    host.transact(binder, code, &mut data, &mut reply, 1)
        .unwrap_or_else(|error| panic!("{name} code {code} (oneway) failed: {error}"));
}

/// Ask `IBluetooth.getState` the way the image's client does (oneway, with a
/// `SynchronousResultReceiver`) and return the answer the module delivered to
/// the receiver, or `None` when nothing was delivered.
fn adapter_state_via_receiver(host: &mut ServiceHost, receiver: u64) -> Option<i32> {
    let (code, mut data) = open(host, "bluetooth", 1);
    data.write_strong_binder(BinderId(receiver));
    transact_oneway(host, "bluetooth", code, data);
    let deliveries = host.take_deliveries();
    let delivery = deliveries
        .iter()
        .find(|delivery| delivery.target.0 == receiver)?;
    assert_eq!(
        delivery.interface,
        "com.android.bluetooth.x.com.android.modules.utils.ISynchronousResultReceiver",
        "the answer must travel through the client's receiver"
    );
    let mut payload = delivery.data.clone();
    payload.set_data_position(0);
    payload
        .enforce_interface("com.android.bluetooth.x.com.android.modules.utils.ISynchronousResultReceiver")
        .expect("ISynchronousResultReceiver token");
    assert_eq!(payload.read_int32().unwrap(), 1, "Result is a typed object");
    // Image order: `mObject` first, then `mException`.
    let value = acos_services::parcelable::read_dynamic(&mut payload).expect("mObject");
    let exception = acos_services::parcelable::read_dynamic(&mut payload).expect("mException");
    assert!(matches!(
        exception,
        acos_services::value::DynamicValue::Null
    ));
    match value {
        acos_services::value::DynamicValue::Int(answer) => Some(answer),
        other => panic!("expected VAL_INTEGER, got {other:?}"),
    }
}

fn register_receiver(host: &mut ServiceHost, receiver: u64, action: &str) {
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
        vec![action.to_string()],
        Vec::new(),
        2,
    );
}

#[test]
fn the_manager_hands_out_the_adapter_and_the_scripted_state_answers() {
    let (mut host, mut registry) = attach("hw.bluetooth");

    let adapter_binder = host.check_service("bluetooth").unwrap().unwrap().0;
    let (code, mut data) = open(&mut host, "bluetooth_manager", 1);
    data.write_strong_binder(BinderId(java_binder(21))); // IBluetoothManagerCallback
    let mut reply = transact(&mut host, "bluetooth_manager", code, data).expect("registerAdapter");
    let returned = reply.read_strong_binder().unwrap().expect("adapter binder").0;
    assert_eq!(
        returned & 0xffff_ffff,
        adapter_binder & 0xffff_ffff,
        "registerAdapter returns the IBluetooth `bluetooth` service"
    );

    // `IBluetooth.getState` is oneway with an `ISynchronousResultReceiver`:
    // a blocking decoded reply would be dropped and the client would burn its
    // whole sync timeout (that was the ART failure this test now pins).
    assert_eq!(
        adapter_state_via_receiver(&mut host, java_binder(22)),
        Some(10),
        "STATE_OFF by default"
    );

    // `set {on: true}` broadcasts ACTION_STATE_CHANGED and calls the manager
    // callback.
    register_receiver(&mut host, java_binder(31), "android.bluetooth.adapter.action.STATE_CHANGED");
    let value = registry
        .control(&mut host, "hw.bluetooth", "set", &serde_json::json!({"on": true}))
        .expect("set on");
    assert_eq!(value["broadcast_matched"], 1);
    assert_eq!(value["callbacks_delivered"], 1);
    let deliveries = host.take_deliveries();
    let state = deliveries
        .iter()
        .find(|d| d.interface == "android.content.IIntentReceiver")
        .expect("ACTION_STATE_CHANGED broadcast");
    let mut intent_bytes = state.data.clone();
    intent_bytes.set_data_position(0);
    intent_bytes
        .enforce_interface("android.content.IIntentReceiver")
        .unwrap();
    assert_eq!(intent_bytes.read_int32().unwrap(), 1);
    let intent = acos_services::parcelable::read_intent(&mut intent_bytes).expect("Intent");
    let facts = IntentFacts::from_parcelable(&intent);
    assert_eq!(
        facts.action.as_deref(),
        Some("android.bluetooth.adapter.action.STATE_CHANGED")
    );
    assert!(matches!(
        facts.extras.get("android.bluetooth.adapter.extra.STATE"),
        Some(acos_services::ams::Extra::Int(12))
    ));
    let callback = deliveries
        .iter()
        .find(|d| d.interface == "android.bluetooth.IBluetoothManagerCallback")
        .expect("onBluetoothServiceUp");
    assert_eq!(callback.code, 1);

    assert_eq!(
        adapter_state_via_receiver(&mut host, java_binder(23)),
        Some(12),
        "STATE_ON after set"
    );
}

#[test]
fn get_remote_name_answers_the_scripted_device_name() {
    let (mut host, _registry) = attach("hw.bluetooth");
    // The adapter must be on: `BluetoothDevice.getName()` returns null when the
    // client sees a disabled adapter (`isBluetoothEnabled()`).
    let (code, mut data) = open(&mut host, "bluetooth_manager", 5);
    data.write_int32(0);
    let _ = transact(&mut host, "bluetooth_manager", code, data).expect("enable");

    /// `IBluetooth.getRemoteName(in BluetoothDevice, in AttributionSource,
    /// receiver)` exactly as AIDL writes it: the device is a typed object
    /// (presence word + `BluetoothDevice.writeToParcel` body, no class name).
    fn remote_name(host: &mut ServiceHost, receiver: u64, address: &str) -> Option<DynamicValue> {
        let (code, mut data) = open(host, "bluetooth", 32);
        data.write_int32(1); // a present typed object
        data.write_string16(address).unwrap(); // mAddress
        data.write_int32(0); // mAddressType == ADDRESS_TYPE_PUBLIC
        data.write_int32(0); // AttributionSource placeholder (data only)
        data.write_string16("dev.acos.fixtures.tailprobe").unwrap();
        data.write_strong_binder(BinderId(receiver));
        transact_oneway(host, "bluetooth", code, data);
        let deliveries = host.take_deliveries();
        let delivery = deliveries.iter().find(|d| d.target.0 == receiver)?;
        assert_eq!(
            delivery.interface,
            "com.android.bluetooth.x.com.android.modules.utils.ISynchronousResultReceiver",
            "the name travels through the client's receiver"
        );
        let mut payload = delivery.data.clone();
        payload.set_data_position(0);
        payload
            .enforce_interface(
                "com.android.bluetooth.x.com.android.modules.utils.ISynchronousResultReceiver",
            )
            .expect("ISynchronousResultReceiver token");
        assert_eq!(payload.read_int32().unwrap(), 1, "Result is a typed object");
        let value = acos_services::parcelable::read_dynamic(&mut payload).expect("mObject");
        let exception =
            acos_services::parcelable::read_dynamic(&mut payload).expect("mException");
        assert!(matches!(exception, DynamicValue::Null));
        Some(value)
    }

    assert_eq!(
        remote_name(&mut host, java_binder(71), "11:22:33:44:55:66"),
        Some(DynamicValue::String(Some("ACOS Headset".into()))),
        "the bonded device's scripted name"
    );
    assert_eq!(
        remote_name(&mut host, java_binder(72), "AA:BB:CC:DD:EE:01"),
        Some(DynamicValue::String(Some("ACOS Beacon".into()))),
        "a scan result's scripted name"
    );
    assert_eq!(
        remote_name(&mut host, java_binder(73), "00:00:00:00:00:00"),
        Some(DynamicValue::String(None)),
        "an unknown device has no name (null), not a fabricated one"
    );
}

#[test]
fn get_bonded_devices_answers_a_real_list_of_bluetooth_device_parcels() {
    let (mut host, _registry) = attach("hw.bluetooth");
    // The adapter must be on (AOSP's client returns an empty set otherwise).
    let (code, mut data) = open(&mut host, "bluetooth_manager", 5);
    data.write_int32(0);
    let _ = transact(&mut host, "bluetooth_manager", code, data).expect("enable");


    // `getBondedDevices(AttributionSource, SynchronousResultReceiver)` is oneway:
    // the request carries the attribution source (data only) and the client's
    // result receiver binder.
    let receiver = java_binder(41);
    let (code, mut data) = open(&mut host, "bluetooth", 23);
    data.write_int32(0); // AttributionSource placeholder (data only)
    data.write_string16("dev.acos.fixtures.tailprobe").unwrap();
    data.write_strong_binder(BinderId(receiver));
    transact_oneway(&mut host, "bluetooth", code, data);

    let deliveries = host.take_deliveries();
    assert_eq!(deliveries.len(), 1);
    let delivery = &deliveries[0];
    assert_eq!(delivery.target.0, receiver);
    assert_eq!(
        delivery.interface,
        "com.android.bluetooth.x.com.android.modules.utils.ISynchronousResultReceiver"
    );
    let mut data = delivery.data.clone();
    data.set_data_position(0);
    data.enforce_interface("com.android.bluetooth.x.com.android.modules.utils.ISynchronousResultReceiver")
        .unwrap();
    assert_eq!(data.read_int32().unwrap(), 1, "Result is a typed object");
    // `Result.writeToParcel` writes `mObject` then `mException` (image dex:
    // `SynchronousResultReceiver$Result.writeToParcel`), and the client's
    // constructor reads them in that order, casting the second to
    // `RuntimeException` — the reverse order is a `ClassCastException` in the
    // client and a lost result.
    let value = acos_services::parcelable::read_dynamic(&mut data).expect("Result.mObject");
    let exception =
        acos_services::parcelable::read_dynamic(&mut data).expect("Result.mException");
    assert!(matches!(
        exception,
        acos_services::value::DynamicValue::Null
    ));
    let acos_services::value::DynamicValue::List(Some(items)) = value else {
        panic!("expected VAL_LIST, got {value:?}");
    };
    assert_eq!(items.len(), 1, "one bonded device");
    let acos_services::value::DynamicValue::Parcelable(parcelable) = &items[0] else {
        panic!("expected a BLUETOOTH_DEVICE parcelable, got {:?}", items[0]);
    };
    assert_eq!(parcelable.class(), Some("android.bluetooth.BluetoothDevice"));
    assert_eq!(
        parcelable.field(0).and_then(|v| v.as_str()),
        Some("11:22:33:44:55:66")
    );
}

#[test]
fn a_scripted_scan_broadcasts_action_found_with_the_device_parcel() {
    let (mut host, mut registry) = attach("hw.bluetooth");
    register_receiver(
        &mut host,
        java_binder(51),
        "android.bluetooth.adapter.action.DISCOVERY_STARTED",
    );
    register_receiver(
        &mut host,
        java_binder(52),
        "android.bluetooth.device.action.FOUND",
    );
    register_receiver(
        &mut host,
        java_binder(53),
        "android.bluetooth.adapter.action.DISCOVERY_FINISHED",
    );
    let _ = registry
        .control(&mut host, "hw.bluetooth", "set", &serde_json::json!({"on": true}))
        .expect("on");
    let _ = host.take_deliveries();

    let value = registry
        .control(&mut host, "hw.bluetooth", "scan", &serde_json::json!({}))
        .expect("scan");
    assert_eq!(value["broadcast_matched"], 3, "started + found + finished");
    let deliveries = host.take_deliveries();
    assert_eq!(deliveries.len(), 3);
    let found = deliveries
        .iter()
        .find(|d| d.target.0 == java_binder(52))
        .expect("ACTION_FOUND");
    let mut data = found.data.clone();
    data.set_data_position(0);
    data.enforce_interface("android.content.IIntentReceiver").unwrap();
    assert_eq!(data.read_int32().unwrap(), 1);
    let intent = acos_services::parcelable::read_intent(&mut data).expect("Intent");
    let facts = IntentFacts::from_parcelable(&intent);
    assert_eq!(
        facts.action.as_deref(),
        Some("android.bluetooth.device.action.FOUND")
    );
    match facts.extras.get("android.bluetooth.device.extra.DEVICE") {
        Some(acos_services::ams::Extra::Parcelable(parcelable)) => {
            assert_eq!(
                parcelable.class(),
                Some("android.bluetooth.BluetoothDevice")
            )
        }
        other => panic!("expected a BluetoothDevice extra, got {other:?}"),
    }
    match facts.extras.get("android.bluetooth.device.extra.NAME") {
        Some(acos_services::ams::Extra::String(name)) => assert_eq!(name, "ACOS Beacon"),
        other => panic!("expected the device name, got {other:?}"),
    }
    // `EXTRA_RSSI` is a **short** extra (`Intent.getShortExtra` casts the bundle
    // value to `java.lang.Short`): an `Integer` there made the probe print
    // `rssi=0` after the client's `ClassCastException` warning.
    match facts.extras.get("android.bluetooth.device.extra.RSSI") {
        Some(acos_services::ams::Extra::Short(rssi)) => assert_eq!(*rssi, -40),
        other => panic!("expected a VAL_SHORT rssi extra, got {other:?}"),
    }
}

#[test]
fn can_authenticate_answers_the_script_and_auth_delivers_the_scripted_result() {
    let (mut host, mut registry) = attach("hw.biometric");

    let (code, mut data) = open(&mut host, "auth", 6);
    data.write_string16("dev.acos.fixtures.tailprobe").unwrap();
    data.write_int32(0); // userId
    data.write_int32(15); // Authenticators.BIOMETRIC_STRONG
    let mut reply = transact(&mut host, "auth", code, data).expect("canAuthenticate");
    assert_eq!(reply.read_int32().unwrap(), 0, "BIOMETRIC_SUCCESS");

    let receiver = java_binder(61);
    let (code, mut data) = open(&mut host, "auth", 4);
    data.write_strong_binder(BinderId(java_binder(62))); // token
    data.write_int64(0); // operationId
    data.write_int32(0); // userId
    data.write_strong_binder(BinderId(receiver));
    data.write_string16("dev.acos.fixtures.tailprobe").unwrap();
    // The PromptInfo parcelable is not needed by the module; leave it out.
    let mut reply = transact(&mut host, "auth", code, data).expect("authenticate");
    let auth_id = reply.read_int64().unwrap();
    assert_eq!(auth_id, 1);

    let value = registry
        .control(
            &mut host,
            "hw.biometric",
            "auth",
            &serde_json::json!({"result": "success"}),
        )
        .expect("auth success");
    assert_eq!(value["auth_id"], 1);
    let deliveries = host.take_deliveries();
    assert_eq!(deliveries.len(), 1);
    assert_eq!(deliveries[0].target.0, receiver);
    assert_eq!(
        deliveries[0].interface,
        "android.hardware.biometrics.IBiometricServiceReceiver"
    );
    assert_eq!(deliveries[0].code, 1, "onAuthenticationSucceeded");
    let mut data = deliveries[0].data.clone();
    data.set_data_position(0);
    data.enforce_interface("android.hardware.biometrics.IBiometricServiceReceiver")
        .unwrap();
    assert_eq!(data.read_int32().unwrap(), 2, "TYPE_FINGERPRINT");

    // A failed attempt and a lockout use the other callback shapes.
    let (code, mut data) = open(&mut host, "auth", 4);
    data.write_strong_binder(BinderId(java_binder(62)));
    data.write_int64(7);
    data.write_int32(0);
    data.write_strong_binder(BinderId(receiver));
    data.write_string16("dev.acos.fixtures.tailprobe").unwrap();
    let _ = transact(&mut host, "auth", code, data).expect("authenticate");
    let _ = registry
        .control(
            &mut host,
            "hw.biometric",
            "auth",
            &serde_json::json!({"result": "lockout"}),
        )
        .expect("lockout");
    let deliveries = host.take_deliveries();
    assert_eq!(deliveries[0].code, 3, "onError");
    let mut data = deliveries[0].data.clone();
    data.set_data_position(0);
    data.enforce_interface("android.hardware.biometrics.IBiometricServiceReceiver")
        .unwrap();
    assert_eq!(data.read_int32().unwrap(), 2, "modality fingerprint");
    assert_eq!(data.read_int32().unwrap(), 7, "BIOMETRIC_ERROR_LOCKOUT");
}

#[test]
fn a_cancelled_prompt_answers_the_receiver_with_biometric_error_canceled() {
    let (mut host, _registry) = attach("hw.biometric");
    let receiver = java_binder(71);
    let (code, mut data) = open(&mut host, "auth", 4);
    data.write_strong_binder(BinderId(java_binder(72)));
    data.write_int64(0);
    data.write_int32(0);
    data.write_strong_binder(BinderId(receiver));
    data.write_string16("dev.acos.fixtures.tailprobe").unwrap();
    let mut reply = transact(&mut host, "auth", code, data).expect("authenticate");
    let auth_id = reply.read_int64().unwrap();

    let (code, mut data) = open(&mut host, "auth", 5);
    data.write_strong_binder(BinderId(java_binder(72)));
    data.write_string16("dev.acos.fixtures.tailprobe").unwrap();
    data.write_int64(auth_id);
    let _ = transact(&mut host, "auth", code, data).expect("cancelAuthentication");

    let deliveries = host.take_deliveries();
    assert_eq!(deliveries.len(), 1, "the cancel answers the receiver");
    assert_eq!(deliveries[0].code, 3, "onError");
    let mut data = deliveries[0].data.clone();
    data.set_data_position(0);
    data.enforce_interface("android.hardware.biometrics.IBiometricServiceReceiver")
        .unwrap();
    assert_eq!(data.read_int32().unwrap(), 2, "modality");
    assert_eq!(data.read_int32().unwrap(), 5, "BIOMETRIC_ERROR_CANCELED");
}

#[test]
fn both_modules_report_their_gaps_and_refuse_an_unsupported_profile() {
    let mut host = ServiceHost::standard();
    let mut registry = ModuleRegistry::standard();
    for module in ["hw.bluetooth", "hw.biometric"] {
        registry
            .attach(&mut host, module, Profile::Scripted, &ModuleConfig::default())
            .unwrap();
        let report = registry
            .reports()
            .into_iter()
            .find(|report| report.id == module)
            .expect("report");
        let reason = report.reason.expect("an attached module always says how");
        assert!(reason.contains("no radio") || reason.contains("no biometric hardware"));
        let error = registry
            .attach(&mut host, module, Profile::Real, &ModuleConfig::default())
            .expect_err("no `real` profile");
        assert!(matches!(error, ServiceError::ModuleRejected { .. }));
    }
}

#[test]
fn the_vibrator_scripted_profile_records_pattern_identity() {
    let (mut host, mut registry) = attach("feedback.vibrator");
    for (index, amplitude) in [128i32, 255].into_iter().enumerate() {
        let (code, mut data) = open(&mut host, "vibrator_manager", 7);
        data.write_int32(10_123); // uid
        data.write_int32(0); // deviceId
        data.write_string16("dev.acos.fixtures.tailprobe").unwrap();
        data.write_int32(1); // the CombinedVibration typed object is present
        data.write_int32(1); // PARCEL_TOKEN_MONO
        data.write_int64(250); // a OneShot/VibrationAttributes-shaped tail
        data.write_int32(amplitude);
        let mut reply = transact(&mut host, "vibrator_manager", code, data).expect("vibrate");
        assert!(reply.read_int32().is_err(), "vibrate has a void reply");
        assert_eq!(index < 2, true);
    }
    let value = registry
        .control(&mut host, "feedback.vibrator", "patterns", &serde_json::Value::Null)
        .expect("patterns");
    let patterns = value["patterns"].as_array().expect("patterns[]");
    assert_eq!(patterns.len(), 2);
    assert_eq!(patterns[0]["token"], 1, "mono CombinedVibration");
    assert_eq!(patterns[0]["uid"], 10_123);
    assert_eq!(
        patterns[0]["op_pkg"], "dev.acos.fixtures.tailprobe",
        "the calling package is recorded"
    );
    assert_ne!(
        patterns[0]["hash"], patterns[1]["hash"],
        "two different patterns have different hashes"
    );
    let calls = registry
        .control(&mut host, "feedback.vibrator", "calls", &serde_json::Value::Null)
        .expect("calls");
    assert_eq!(calls["vibrate_calls"], 2);
}
