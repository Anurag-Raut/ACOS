//! `hw.camera`: the `media.camera` seam, its `off` tombstone, and byte-level
//! wire verification against **this image's** framework DEX.
//!
//! Every transaction id, status/error constant, and reply parcel layout here is
//! verified against `.cache/boot/android-34/dex/framework/*.dex` (`ICameraService$Stub`,
//! `ICameraService`, `ICameraServiceListener`, `CameraMetadata`, `Camera$CameraInfo`)
//! and `libcamera_client.so` / `libcamera_metadata.so`.

use std::path::PathBuf;

use acos_parcel::{BinderId, Parcel};
use acos_services::exception::EX_SERVICE_SPECIFIC;
use acos_services::module::{self, AospReaction, ModuleConfig, ModuleRegistry, Profile, TOMBSTONES};
use acos_services::modules::camera::{
    callback_code, code, device_code, sink_code, test_pattern_yuv420_crc32, CAMERA_API_LEVEL_1,
    CAMERA_API_LEVEL_2, CAMERA_TYPE_ALL, CAMERA_TYPE_BACKWARD_COMPATIBLE, DESCRIPTOR,
    DEVICE_CALLBACKS_DESCRIPTOR, DEVICE_USER_DESCRIPTOR, ERROR_ILLEGAL_ARGUMENT,
    ERROR_PERMISSION_DENIED, FACING_BACK, FACING_FRONT, FRAME_DURATION_NS, FRAME_SINK_DESCRIPTOR,
    FRAME_SINK_SERVICE, INITIAL_TIMESTAMP_NS, MODULE_ID, NO_IN_FLIGHT_REPEATING_FRAMES,
    SERVICE_NAME, STATUS_NOT_PRESENT, STATUS_PRESENT, SURFACE_PARCEL_MAGIC_BQUE,
};
use acos_services::modules::camera_metadata::{self, tags, HARDWARE_LEVEL_LIMITED};
use acos_services::{ServiceError, ServiceHost};

fn attach_with(profile: Profile, config: serde_json::Value) -> (ServiceHost, ModuleRegistry) {
    let mut host = ServiceHost::new();
    let mut registry = module::install_standard(&mut host);
    registry
        .attach(
            &mut host,
            MODULE_ID,
            profile,
            &ModuleConfig::from_json(MODULE_ID, &config).expect("config parses"),
        )
        .expect("module attaches");
    (host, registry)
}

fn attach() -> (ServiceHost, ModuleRegistry) {
    attach_with(Profile::Stub, serde_json::json!(null))
}

/// A request parcel with the interface token the client's `enforceInterface` writes.
fn request(host: &mut ServiceHost) -> Parcel {
    let binder = host
        .check_service(SERVICE_NAME)
        .expect("lookup is infallible")
        .unwrap_or_else(|| panic!("{SERVICE_NAME} did not resolve to a binder"));
    let descriptor = host
        .descriptor_of(binder)
        .unwrap_or_else(|| panic!("{SERVICE_NAME} has no descriptor"))
        .to_string();
    assert_eq!(descriptor, DESCRIPTOR);
    let mut data = Parcel::new();
    data.write_interface_token(&descriptor).expect("token");
    data
}

/// Send `data` as `code` and return the reply positioned **after** the AIDL
/// exception header (which must be `0`).
fn transact(host: &mut ServiceHost, code: u32, mut data: Parcel) -> Parcel {
    let binder = host.check_service(SERVICE_NAME).unwrap().unwrap();
    data.set_data_position(0);
    let mut reply = Parcel::new();
    host.transact(binder, code, &mut data, &mut reply, 0)
        .unwrap_or_else(|error| panic!("code {code} failed: {error}"));
    reply.set_data_position(0);
    assert_eq!(reply.read_int32().expect("exception header"), 0);
    reply
}

/// Send `data` as `code` and return the raw reply starting at position 0.
fn transact_raw(host: &mut ServiceHost, code: u32, mut data: Parcel) -> Parcel {
    let binder = host.check_service(SERVICE_NAME).unwrap().unwrap();
    data.set_data_position(0);
    let mut reply = Parcel::new();
    host.transact(binder, code, &mut data, &mut reply, 0)
        .unwrap_or_else(|error| panic!("code {code} failed: {error}"));
    reply.set_data_position(0);
    reply
}

fn error_of(host: &mut ServiceHost, code: u32, mut data: Parcel) -> ServiceError {
    let binder = host.check_service(SERVICE_NAME).unwrap().unwrap();
    data.set_data_position(0);
    let mut reply = Parcel::new();
    host.transact(binder, code, &mut data, &mut reply, 0)
        .expect_err("this transaction must be refused")
}

fn framework_dex_dir() -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest_dir
        .parent()
        .and_then(|p| p.parent())
        .expect("workspace root")
        .join(".cache/boot/android-34/dex/framework")
}

fn framework_static_ints_for(class_desc: &str) -> Vec<(String, i64)> {
    let dir = framework_dex_dir();
    assert!(
        dir.is_dir(),
        "framework dex directory missing at {}; run `source tools/dev/agent-env.sh`",
        dir.display()
    );
    let mut dex_found = None;
    for entry in std::fs::read_dir(&dir).expect("read framework dex dir") {
        let path = entry.expect("dir entry").path();
        if path.extension().and_then(|e| e.to_str()) == Some("dex") {
            let bytes = std::fs::read(&path).expect("read dex");
            if let Ok(dex) = acos_dex::DexFile::parse(bytes) {
                if dex.find_class(class_desc).is_some() {
                    dex_found = Some(dex);
                    break;
                }
            }
        }
    }
    let dex = dex_found.unwrap_or_else(|| panic!("{class_desc} not found in {}", dir.display()));
    let def = dex.find_class(class_desc).expect("class def");
    let cd = dex
        .class_data(def)
        .expect("class_data")
        .expect("class_data present");
    let values = dex
        .static_values(def)
        .expect("static_values")
        .unwrap_or_default();
    let mut out = Vec::new();
    for (i, field) in cd.static_fields.iter().enumerate() {
        if let Ok(fr) = dex.field_ref(field.field_idx) {
            if let Some(acos_dex::EncodedValue::Int(v)) = values.get(i) {
                out.push((fr.name.to_string(), i64::from(*v)));
            }
        }
    }
    out
}

#[test]
fn every_icameraservice_transaction_code_and_constant_matches_framework_dex() {
    // 1. All 22 TRANSACTION_* constants on ICameraService$Stub.
    let stub_ints = framework_static_ints_for("Landroid/hardware/ICameraService$Stub;");
    let expected_tx: &[(&str, u32)] = &[
        ("TRANSACTION_getNumberOfCameras", code::GET_NUMBER_OF_CAMERAS),
        ("TRANSACTION_getCameraInfo", code::GET_CAMERA_INFO),
        ("TRANSACTION_connect", code::CONNECT),
        ("TRANSACTION_connectDevice", code::CONNECT_DEVICE),
        ("TRANSACTION_addListener", code::ADD_LISTENER),
        ("TRANSACTION_getConcurrentCameraIds", code::GET_CONCURRENT_CAMERA_IDS),
        (
            "TRANSACTION_isConcurrentSessionConfigurationSupported",
            code::IS_CONCURRENT_SESSION_CONFIGURATION_SUPPORTED,
        ),
        ("TRANSACTION_removeListener", code::REMOVE_LISTENER),
        (
            "TRANSACTION_getCameraCharacteristics",
            code::GET_CAMERA_CHARACTERISTICS,
        ),
        (
            "TRANSACTION_getCameraVendorTagDescriptor",
            code::GET_CAMERA_VENDOR_TAG_DESCRIPTOR,
        ),
        (
            "TRANSACTION_getCameraVendorTagCache",
            code::GET_CAMERA_VENDOR_TAG_CACHE,
        ),
        ("TRANSACTION_getLegacyParameters", code::GET_LEGACY_PARAMETERS),
        ("TRANSACTION_supportsCameraApi", code::SUPPORTS_CAMERA_API),
        (
            "TRANSACTION_isHiddenPhysicalCamera",
            code::IS_HIDDEN_PHYSICAL_CAMERA,
        ),
        ("TRANSACTION_injectCamera", code::INJECT_CAMERA),
        ("TRANSACTION_setTorchMode", code::SET_TORCH_MODE),
        (
            "TRANSACTION_turnOnTorchWithStrengthLevel",
            code::TURN_ON_TORCH_WITH_STRENGTH_LEVEL,
        ),
        (
            "TRANSACTION_getTorchStrengthLevel",
            code::GET_TORCH_STRENGTH_LEVEL,
        ),
        ("TRANSACTION_notifySystemEvent", code::NOTIFY_SYSTEM_EVENT),
        (
            "TRANSACTION_notifyDisplayConfigurationChange",
            code::NOTIFY_DISPLAY_CONFIGURATION_CHANGE,
        ),
        (
            "TRANSACTION_notifyDeviceStateChange",
            code::NOTIFY_DEVICE_STATE_CHANGE,
        ),
        (
            "TRANSACTION_reportExtensionSessionStats",
            code::REPORT_EXTENSION_SESSION_STATS,
        ),
    ];
    for &(name, expected) in expected_tx {
        let actual = stub_ints
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| *v)
            .unwrap_or_else(|| panic!("{name} missing from ICameraService$Stub in framework dex"));
        assert_eq!(
            actual,
            i64::from(expected),
            "ICameraService$Stub.{name} drifted from framework dex"
        );
    }

    // 2. ICameraService error and type constants.
    let svc_ints = framework_static_ints_for("Landroid/hardware/ICameraService;");
    for &(name, expected) in &[
        ("CAMERA_TYPE_BACKWARD_COMPATIBLE", CAMERA_TYPE_BACKWARD_COMPATIBLE),
        ("CAMERA_TYPE_ALL", CAMERA_TYPE_ALL),
        ("API_VERSION_1", CAMERA_API_LEVEL_1),
        ("API_VERSION_2", CAMERA_API_LEVEL_2),
        ("ERROR_PERMISSION_DENIED", ERROR_PERMISSION_DENIED),
        ("ERROR_ILLEGAL_ARGUMENT", ERROR_ILLEGAL_ARGUMENT),
    ] {
        let actual = svc_ints
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| *v)
            .unwrap_or_else(|| panic!("{name} missing from ICameraService in framework dex"));
        assert_eq!(actual, i64::from(expected), "ICameraService.{name} drifted");
    }

    // 3. ICameraServiceListener status constants.
    let listener_ints = framework_static_ints_for("Landroid/hardware/ICameraServiceListener;");
    for &(name, expected) in &[
        ("STATUS_NOT_PRESENT", STATUS_NOT_PRESENT),
        ("STATUS_PRESENT", STATUS_PRESENT),
    ] {
        let actual = listener_ints
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| *v)
            .unwrap_or_else(|| {
                panic!("{name} missing from ICameraServiceListener in framework dex")
            });
        assert_eq!(
            actual,
            i64::from(expected),
            "ICameraServiceListener.{name} drifted"
        );
    }

    // 4. CameraMetadata hardware level & lens facing constants.
    let meta_ints = framework_static_ints_for("Landroid/hardware/camera2/CameraMetadata;");
    for &(name, expected) in &[
        ("INFO_SUPPORTED_HARDWARE_LEVEL_LIMITED", i32::from(HARDWARE_LEVEL_LIMITED)),
        ("LENS_FACING_FRONT", 0),
        ("LENS_FACING_BACK", 1),
    ] {
        let actual = meta_ints
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| *v)
            .unwrap_or_else(|| panic!("{name} missing from CameraMetadata in framework dex"));
        assert_eq!(actual, i64::from(expected), "CameraMetadata.{name} drifted");
    }

    // 5. Legacy Camera.CameraInfo facing constants (note back=0, front=1 vs Camera2's back=1, front=0).
    let info_ints = framework_static_ints_for("Landroid/hardware/Camera$CameraInfo;");
    for &(name, expected) in &[
        ("CAMERA_FACING_BACK", FACING_BACK),
        ("CAMERA_FACING_FRONT", FACING_FRONT),
    ] {
        let actual = info_ints
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| *v)
            .unwrap_or_else(|| panic!("{name} missing from Camera$CameraInfo in framework dex"));
        assert_eq!(actual, i64::from(expected), "Camera$CameraInfo.{name} drifted");
    }
}

#[test]
fn the_module_is_registered_with_the_manifest_the_kit_expects() {
    let registry = ModuleRegistry::standard();
    let ids: Vec<&str> = registry.ids();
    assert!(
        ids.contains(&MODULE_ID),
        "the camera module must be in the standard set, got {ids:?}"
    );
    let manifest = registry.manifest(MODULE_ID).expect("manifest");
    manifest.validate().expect("manifest validates");
    assert_eq!(manifest.services.len(), 1);
    assert_eq!(manifest.services[0].name, SERVICE_NAME);
    assert_eq!(manifest.services[0].descriptor, DESCRIPTOR);
    assert!(manifest.features.contains(&"android.hardware.camera"));
    assert!(manifest.features.contains(&"android.hardware.camera.any"));
    assert_eq!(
        manifest.profiles,
        &[Profile::Stub, Profile::Scripted],
        "there is no camera HAL: `real` would be a fake success; `stub` and `scripted` are supported"
    );
    assert!(manifest.budget.ram_mib > 0 && manifest.budget.startup_ms > 0);
}

#[test]
fn off_mode_returns_empty_list_and_refuses_open_with_stub_attach_hint() {
    let mut host = ServiceHost::new();
    let _registry = module::install_standard(&mut host);

    let row = TOMBSTONES
        .iter()
        .find(|row| row.name == SERVICE_NAME)
        .expect("media.camera has a decision row");
    assert_eq!(row.module, MODULE_ID);
    assert_eq!(
        row.reaction,
        AospReaction::Tombstone,
        "media.camera decision row remains documented in TOMBSTONES"
    );

    // Tombstone::error() itself must use each module's default_profile (never
    // hardcoded "real" for stub/scripted-only modules).
    let cam_err = row.error(DESCRIPTOR);
    match &cam_err {
        ServiceError::ModuleUnavailable { attach, .. } => {
            assert!(
                attach.contains("\"profile\": \"scripted\""),
                "hw.camera tombstone hint must name highest profile scripted, got {attach}"
            );
        }
        other => panic!("expected ModuleUnavailable, got {other:?}"),
    }
    let vib_row = TOMBSTONES
        .iter()
        .find(|r| r.name == "vibrator_manager")
        .expect("vibrator_manager tombstone");
    match vib_row.error(vib_row.descriptor) {
        ServiceError::ModuleUnavailable { attach, .. } => {
            assert!(
                attach.contains("\"profile\": \"scripted\""),
                "feedback.vibrator tombstone hint must name profile scripted, got {attach}"
            );
        }
        other => panic!("expected ModuleUnavailable, got {other:?}"),
    }

    let binder = host
        .check_service(SERVICE_NAME)
        .expect("lookup")
        .expect("off-node binder installed");

    // 1. Owner Default "Z": getCameraIdList() -> addListener returns CameraStatus[0] cleanly.
    let mut data = Parcel::new();
    data.write_interface_token(DESCRIPTOR).expect("token");
    data.write_strong_binder(BinderId::NULL);
    data.set_data_position(0);
    let mut reply = Parcel::new();
    host.transact(binder, code::ADD_LISTENER, &mut data, &mut reply, 0)
        .expect("addListener succeeds with empty list when hw.camera is off");
    reply.set_data_position(0);
    assert_eq!(reply.read_int32().expect("ex"), 0);
    assert_eq!(reply.read_int32().expect("CameraStatus[] len"), 0);

    // getNumberOfCameras returns 0 cleanly.
    let mut data = Parcel::new();
    data.write_interface_token(DESCRIPTOR).expect("token");
    data.write_int32(CAMERA_TYPE_ALL);
    data.set_data_position(0);
    let mut reply = Parcel::new();
    host.transact(binder, code::GET_NUMBER_OF_CAMERAS, &mut data, &mut reply, 0)
        .expect("getNumberOfCameras returns 0");
    reply.set_data_position(0);
    assert_eq!(reply.read_int32().expect("ex"), 0);
    assert_eq!(reply.read_int32().expect("count"), 0);

    // 2. Owner Default "Z": openCamera / connectDevice and getCameraCharacteristics
    // throw structured ModuleUnavailable naming hw.camera and profile "stub".
    for tx in [code::CONNECT_DEVICE, code::GET_CAMERA_CHARACTERISTICS] {
        let mut data = Parcel::new();
        data.write_interface_token(DESCRIPTOR).expect("token");
        data.set_data_position(0);
        let mut reply = Parcel::new();
        let error = host
            .transact(binder, tx, &mut data, &mut reply, 0)
            .expect_err("off-node must refuse open/characteristics");
        assert_eq!(error.code(), "UNSUPPORTED_ANDROID_API");
        match &error {
            ServiceError::ModuleUnavailable {
                service,
                module,
                attach,
                ..
            } => {
                assert_eq!(service, SERVICE_NAME);
                assert_eq!(*module, MODULE_ID);
                assert!(
                    attach.contains("acos_launch")
                        && attach.contains(MODULE_ID)
                        && attach.contains("\"profile\": \"stub\""),
                    "off-node attach hint must name hw.camera + stub profile, got {attach}"
                );
            }
            other => panic!("expected ModuleUnavailable, got {other:?}"),
        }
    }
}

#[test]
fn get_number_of_cameras_answers_one_and_records_the_type() {
    let (mut host, _registry) = attach();
    for kind in [CAMERA_TYPE_BACKWARD_COMPATIBLE, CAMERA_TYPE_ALL] {
        let mut data = request(&mut host);
        data.write_int32(kind);
        let mut reply = transact(&mut host, code::GET_NUMBER_OF_CAMERAS, data);
        assert_eq!(reply.read_int32().expect("count"), 1);
        assert_eq!(reply.data_size(), reply.data_position(), "no trailing words");
    }
}

#[test]
fn get_camera_info_writes_camera_info_s_parcel_field_order() {
    let (mut host, _registry) = attach();
    let mut data = request(&mut host);
    data.write_int32(0); // cameraId
    data.write_int32(0); // overrideToPortrait
    let mut reply = transact(&mut host, code::GET_CAMERA_INFO, data);
    // `Parcel.readTypedObject(CameraInfo.CREATOR)` -> presence int, then facing, orientation.
    assert_eq!(reply.read_int32().expect("presence"), 1);
    assert_eq!(
        reply.read_int32().expect("facing"),
        FACING_BACK,
        "Camera.CameraInfo.CAMERA_FACING_BACK is 0"
    );
    assert_eq!(reply.read_int32().expect("orientation"), 90);
    assert_eq!(reply.data_size(), reply.data_position(), "no trailing words");
}

#[test]
fn add_listener_answers_one_present_camera_status_with_typed_array_wire_format() {
    let (mut host, _registry) = attach();
    let listener = host.register_aux("android.hardware.ICameraServiceListener");
    let mut data = request(&mut host);
    data.write_strong_binder(listener);
    let mut reply = transact(&mut host, code::ADD_LISTENER, data);
    // `Parcel.createTypedArray(CameraStatus.CREATOR)`:
    //   int32 length, then for each element: int32 present(1), CameraStatus.readFromParcel
    assert_eq!(reply.read_int32().expect("array length"), 1);
    assert_eq!(reply.read_int32().expect("element presence"), 1);
    assert_eq!(
        reply.read_string16().expect("cameraId").as_deref(),
        Some("0")
    );
    assert_eq!(
        reply.read_int32().expect("status"),
        STATUS_PRESENT,
        "STATUS_PRESENT must be 1, not 0 (STATUS_NOT_PRESENT)"
    );
    assert_eq!(
        reply.read_int32().expect("unavailablePhysicalCameras length"),
        0
    );
    assert_eq!(
        reply.read_string16().expect("clientPackage").as_deref(),
        Some(""),
        "empty string when nobody has the camera open"
    );
    assert_eq!(reply.data_size(), reply.data_position(), "no trailing words");
}

fn find_entry(parsed: &camera_metadata::Parsed, tag: u32) -> Option<camera_metadata::Entry> {
    parsed.entries().into_iter().find(|e| e.tag == tag)
}

#[test]
fn get_camera_characteristics_writes_valid_native_metadata_blob_and_rejects_unknown_id() {
    let (mut host, _registry) = attach();

    // Valid id "0" -> typed object present + CameraMetadata::writeToParcel framing.
    let mut data = request(&mut host);
    data.write_string16("0").expect("cameraId");
    data.write_int32(34); // targetSdkVersion
    data.write_int32(0); // overrideToPortrait
    let mut reply = transact(&mut host, code::GET_CAMERA_CHARACTERISTICS, data);
    assert_eq!(reply.read_int32().expect("presence"), 1);
    let blob = camera_metadata::read_parcel(&mut reply)
        .expect("parcel framing")
        .expect("non-empty metadata");
    assert_eq!(reply.data_size(), reply.data_position(), "no trailing words");

    let parsed = camera_metadata::validate(&blob).expect("metadata validates");
    // Verify key characteristics values match the stub back camera.
    assert_eq!(
        find_entry(&parsed, tags::LENS_FACING.id).map(|e| e.as_i32()),
        Some(vec![1]),
        "Camera2 LENS_FACING_BACK is 1"
    );
    assert_eq!(
        find_entry(&parsed, tags::INFO_SUPPORTED_HARDWARE_LEVEL.id).map(|e| e.as_i32()),
        Some(vec![0]),
        "INFO_SUPPORTED_HARDWARE_LEVEL_LIMITED is 0"
    );
    assert_eq!(
        find_entry(&parsed, tags::SENSOR_ORIENTATION.id).map(|e| e.as_i32()),
        Some(vec![90])
    );
    assert_eq!(
        find_entry(&parsed, tags::SENSOR_INFO_PIXEL_ARRAY_SIZE.id).map(|e| e.as_i32()),
        Some(vec![4000, 3000])
    );
    assert!(
        find_entry(&parsed, tags::SCALER_AVAILABLE_STREAM_CONFIGURATIONS.id).is_some(),
        "StreamConfigurationMap table must be present"
    );
    assert!(
        find_entry(&parsed, tags::REQUEST_AVAILABLE_CHARACTERISTICS_KEYS.id).is_some(),
        "REQUEST_AVAILABLE_CHARACTERISTICS_KEYS must be present"
    );

    // Unknown id "99" -> ServiceSpecificException(ERROR_ILLEGAL_ARGUMENT = 3).
    let mut data = request(&mut host);
    data.write_string16("99").expect("unknown cameraId");
    data.write_int32(34);
    data.write_int32(0);
    let mut reply = transact_raw(&mut host, code::GET_CAMERA_CHARACTERISTICS, data);
    assert_eq!(reply.read_int32().expect("ex code"), EX_SERVICE_SPECIFIC);
    let msg = reply
        .read_string16()
        .expect("message")
        .unwrap_or_default();
    assert!(msg.contains("99"), "message names unknown id: {msg}");
    assert_eq!(reply.read_int32().expect("stack header"), 0);
    assert_eq!(
        reply.read_int32().expect("service specific code"),
        ERROR_ILLEGAL_ARGUMENT,
        "ERROR_ILLEGAL_ARGUMENT must be 3 (ExceptionUtils -> IllegalArgumentException)"
    );
}

#[test]
fn vendor_tag_descriptor_and_cache_write_non_null_empty_parcelables() {
    let (mut host, _registry) = attach();

    // libcamera_client.so BpCameraService calls Parcel::readParcelable(Parcelable*)
    // which returns UNEXPECTED_NULL if the presence int is 0.
    for tx in [
        code::GET_CAMERA_VENDOR_TAG_DESCRIPTOR,
        code::GET_CAMERA_VENDOR_TAG_CACHE,
    ] {
        let data = request(&mut host);
        let mut reply = transact(&mut host, tx, data);
        assert_eq!(reply.read_int32().expect("presence"), 1);
        assert_eq!(reply.read_int32().expect("empty count"), 0);
        assert_eq!(reply.data_size(), reply.data_position(), "no trailing words");
    }
}

#[test]
fn supports_camera_api_answers_camera2_and_refuses_api_1() {
    let (mut host, _registry) = attach();
    for (level, expected) in [(CAMERA_API_LEVEL_2, 1), (CAMERA_API_LEVEL_1, 0)] {
        let mut data = request(&mut host);
        data.write_string16("0").expect("cameraId");
        data.write_int32(level);
        let mut reply = transact(&mut host, code::SUPPORTS_CAMERA_API, data);
        assert_eq!(reply.read_int32().expect("supported"), expected);
    }
}

#[test]
fn auxiliary_listing_and_notification_transactions_match_proxy_wire_format() {
    let (mut host, _registry) = attach();

    // getConcurrentCameraIds(): empty typed array (len = 0).
    let data = request(&mut host);
    let mut reply = transact(&mut host, code::GET_CONCURRENT_CAMERA_IDS, data);
    assert_eq!(reply.read_int32().expect("array length"), 0);

    // isConcurrentSessionConfigurationSupported(): false (0).
    let mut data = request(&mut host);
    data.write_int32(0); // empty array
    data.write_int32(34);
    let mut reply = transact(
        &mut host,
        code::IS_CONCURRENT_SESSION_CONFIGURATION_SUPPORTED,
        data,
    );
    assert_eq!(reply.read_int32().expect("supported"), 0);

    // isHiddenPhysicalCamera("0"): false (0).
    let mut data = request(&mut host);
    data.write_string16("0").expect("cameraId");
    let mut reply = transact(&mut host, code::IS_HIDDEN_PHYSICAL_CAMERA, data);
    assert_eq!(reply.read_int32().expect("hidden"), 0);

    // getTorchStrengthLevel("0"): 0 (no flash).
    let mut data = request(&mut host);
    data.write_string16("0").expect("cameraId");
    let mut reply = transact(&mut host, code::GET_TORCH_STRENGTH_LEVEL, data);
    assert_eq!(reply.read_int32().expect("strength"), 0);

    // getLegacyParameters(0): null string.
    let mut data = request(&mut host);
    data.write_int32(0);
    let mut reply = transact(&mut host, code::GET_LEGACY_PARAMETERS, data);
    assert_eq!(reply.read_string16().expect("legacy params"), None);

    // removeListener: void.
    let listener = host.register_aux("android.hardware.ICameraServiceListener");
    let mut data = request(&mut host);
    data.write_strong_binder(listener);
    let reply = transact(&mut host, code::REMOVE_LISTENER, data);
    assert_eq!(reply.data_size(), reply.data_position());

    // oneway notifications: accepted without error.
    for tx in [
        code::NOTIFY_SYSTEM_EVENT,
        code::NOTIFY_DISPLAY_CONFIGURATION_CHANGE,
        code::NOTIFY_DEVICE_STATE_CHANGE,
    ] {
        let data = request(&mut host);
        let _ = transact(&mut host, tx, data);
    }
}

#[test]
fn connect_device_enforces_camera_permission_then_opens_device_user() {
    let (mut host, _registry) = attach();
    let cb = host.register_aux(DEVICE_CALLBACKS_DESCRIPTOR);

    let build_connect = |host: &mut ServiceHost, camera_id: &str| {
        let mut data = request(host);
        data.write_strong_binder(cb);
        data.write_string16(camera_id).expect("cameraId");
        data.write_string16("dev.acos.fixtures.cameraprobe").expect("opPkg");
        data.write_string16_opt(None).expect("featureId");
        data.write_int32(-1); // USE_CALLING_UID
        data.write_int32(0); // oomScoreOffset
        data.write_int32(34); // targetSdkVersion
        data.write_int32(0); // overrideToPortrait
        data
    };

    // 1. Before CAMERA permission is granted -> ServiceSpecificException(ERROR_PERMISSION_DENIED = 1).
    let data = build_connect(&mut host, "0");
    let mut reply = transact_raw(&mut host, code::CONNECT_DEVICE, data);
    assert_eq!(reply.read_int32().expect("ex code"), EX_SERVICE_SPECIFIC);
    let msg = reply.read_string16().expect("msg").unwrap_or_default();
    assert!(msg.contains("CAMERA"), "denial names CAMERA: {msg}");
    assert_eq!(reply.read_int32().expect("stack header"), 0);
    assert_eq!(
        reply.read_int32().expect("service specific code"),
        ERROR_PERMISSION_DENIED
    );

    // 2. Grant android.permission.CAMERA in the shared PermissionStore.
    host.permission_store()
        .expect("core installs permission store")
        .lock()
        .unwrap()
        .grant("android.permission.CAMERA");

    // 3. Unknown camera id -> ServiceSpecificException(ERROR_ILLEGAL_ARGUMENT = 3).
    let data = build_connect(&mut host, "99");
    let mut reply = transact_raw(&mut host, code::CONNECT_DEVICE, data);
    assert_eq!(reply.read_int32().expect("ex code"), EX_SERVICE_SPECIFIC);
    let _ = reply.read_string16().expect("msg");
    assert_eq!(reply.read_int32().expect("stack header"), 0);
    assert_eq!(
        reply.read_int32().expect("service specific code"),
        ERROR_ILLEGAL_ARGUMENT
    );

    // 4. Valid camera id with CAMERA granted -> live ICameraDeviceUser binder.
    let data = build_connect(&mut host, "0");
    let mut reply = transact(&mut host, code::CONNECT_DEVICE, data);
    let dev_binder = reply
        .read_strong_binder()
        .expect("read binder")
        .expect("non-null ICameraDeviceUser");
    assert_eq!(
        host.descriptor_of(dev_binder),
        Some(DEVICE_USER_DESCRIPTOR),
        "connectDevice must register an ICameraDeviceUser node"
    );
}

#[test]
fn an_unknown_camera_index_is_a_structured_refusal_not_a_second_camera() {
    let (mut host, _registry) = attach();
    let mut data = request(&mut host);
    data.write_int32(1); // cameraId 1 does not exist
    data.write_int32(0);
    let error = error_of(&mut host, code::GET_CAMERA_INFO, data);
    assert!(
        matches!(
            error,
            ServiceError::UnsupportedAndroidApi { ref method, .. }
                if method.contains("getCameraInfo(1)")
        ),
        "expected a structured unsupported error naming the index, got {error:?}"
    );
}

#[test]
fn the_config_describes_the_one_camera_and_unknown_keys_are_refused() {
    let (mut host, _registry) = attach_with(
        Profile::Stub,
        serde_json::json!({
            "camera": {
                "id": "7",
                "facing": "front",
                "orientation": 270,
                "width": 1280,
                "height": 720
            }
        }),
    );

    // getCameraInfo reflects configured facing & orientation.
    let mut data = request(&mut host);
    data.write_int32(0);
    data.write_int32(0);
    let mut reply = transact(&mut host, code::GET_CAMERA_INFO, data);
    assert_eq!(reply.read_int32().expect("presence"), 1);
    assert_eq!(reply.read_int32().expect("facing"), FACING_FRONT);
    assert_eq!(reply.read_int32().expect("orientation"), 270);

    // addListener reports configured id "7" with STATUS_PRESENT = 1.
    let listener = host.register_aux("android.hardware.ICameraServiceListener");
    let mut data = request(&mut host);
    data.write_strong_binder(listener);
    let mut reply = transact(&mut host, code::ADD_LISTENER, data);
    assert_eq!(reply.read_int32().expect("array length"), 1);
    assert_eq!(reply.read_int32().expect("presence"), 1);
    assert_eq!(
        reply.read_string16().expect("cameraId").as_deref(),
        Some("7")
    );
    assert_eq!(reply.read_int32().expect("status"), STATUS_PRESENT);

    // getCameraCharacteristics reflects configured width/height/facing/orientation.
    let mut data = request(&mut host);
    data.write_string16("7").expect("cameraId");
    data.write_int32(34);
    data.write_int32(0);
    let mut reply = transact(&mut host, code::GET_CAMERA_CHARACTERISTICS, data);
    assert_eq!(reply.read_int32().expect("presence"), 1);
    let blob = camera_metadata::read_parcel(&mut reply)
        .expect("framing")
        .expect("blob");
    let parsed = camera_metadata::validate(&blob).expect("valid");
    assert_eq!(
        find_entry(&parsed, tags::LENS_FACING.id).map(|e| e.as_i32()),
        Some(vec![0]),
        "LENS_FACING_FRONT is 0"
    );
    assert_eq!(
        find_entry(&parsed, tags::SENSOR_INFO_PIXEL_ARRAY_SIZE.id).map(|e| e.as_i32()),
        Some(vec![1280, 720])
    );

    // An unimplemented key is refused, never silently ignored.
    let mut host = ServiceHost::new();
    let mut registry = module::install_standard(&mut host);
    let error = registry
        .attach(
            &mut host,
            MODULE_ID,
            Profile::Stub,
            &ModuleConfig::from_json(MODULE_ID, &serde_json::json!({"camera": {"fps": 60}}))
                .expect("config parses"),
        )
        .expect_err("an unimplemented key is refused");
    assert!(
        error.to_string().contains("fps"),
        "the refusal names the key: {error}"
    );
}

#[test]
fn the_profiles_it_does_not_implement_are_refused_by_name() {
    for profile in [Profile::Real] {
        let mut host = ServiceHost::new();
        let mut registry = module::install_standard(&mut host);
        let error = registry
            .attach(&mut host, MODULE_ID, profile, &ModuleConfig::default())
            .expect_err("unimplemented profile is refused");
        assert!(
            error.to_string().contains("not implemented"),
            "{profile} must be refused with the reason, got {error}"
        );
    }
}

#[test]
fn the_control_ops_report_the_served_camera_characteristics_and_transactions() {
    let (mut host, mut registry) = attach();

    let mut data = request(&mut host);
    data.write_int32(0);
    data.write_int32(0);
    transact(&mut host, code::GET_CAMERA_INFO, data);

    let camera = registry
        .control(&mut host, MODULE_ID, "camera", &serde_json::json!(null))
        .expect("camera op");
    assert_eq!(camera["id"], "0");
    assert_eq!(camera["facing"], "back");
    assert_eq!(camera["orientation"], 90);
    assert_eq!(camera["width"], 4000);
    assert_eq!(camera["height"], 3000);

    let chars = registry
        .control(
            &mut host,
            MODULE_ID,
            "characteristics",
            &serde_json::json!(null),
        )
        .expect("characteristics op");
    assert_eq!(chars["hardware_level"], "LIMITED");
    assert!(chars["bytes"].as_u64().unwrap_or(0) > 48);
    assert!(!chars["entries"].as_array().unwrap().is_empty());

    let calls = registry
        .control(&mut host, MODULE_ID, "calls", &serde_json::json!(null))
        .expect("calls op");
    let calls = calls["calls"].as_array().expect("calls array");
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0]["code"], code::GET_CAMERA_INFO);
    assert_eq!(calls[0]["method"], "getCameraInfo");
    assert_eq!(calls[0]["camera_id"], "0");

    let status = registry
        .control(&mut host, MODULE_ID, "status", &serde_json::json!(null))
        .expect("status op");
    assert_eq!(status["cameras"][0]["id"], "0");

    registry
        .control(&mut host, MODULE_ID, "reset", &serde_json::json!(null))
        .expect("reset op");
    let calls = registry
        .control(&mut host, MODULE_ID, "calls", &serde_json::json!(null))
        .expect("calls op");
    assert_eq!(calls["calls"].as_array().expect("calls array").len(), 0);
}

#[test]
fn detach_restores_the_off_node_and_reattach_serves_cleanly() {
    let (mut host, mut registry) = attach();
    assert!(host.check_service(SERVICE_NAME).unwrap().is_some());

    registry.detach(&mut host, MODULE_ID).expect("detach");

    // Detach must restore the Owner Default "Z" off-node: empty list on
    // addListener, structured ModuleUnavailable on connectDevice.
    let binder = host
        .check_service(SERVICE_NAME)
        .unwrap()
        .expect("off-node restored on detach");
    let mut list_data = Parcel::new();
    list_data.write_interface_token(DESCRIPTOR).expect("token");
    list_data.write_strong_binder(BinderId::NULL);
    list_data.set_data_position(0);
    let mut list_reply = Parcel::new();
    host.transact(binder, code::ADD_LISTENER, &mut list_data, &mut list_reply, 0)
        .expect("restored off-node returns empty list");
    list_reply.set_data_position(0);
    assert_eq!(list_reply.read_int32().expect("ex"), 0);
    assert_eq!(list_reply.read_int32().expect("empty CameraStatus[]"), 0);

    let mut open_data = Parcel::new();
    open_data.write_interface_token(DESCRIPTOR).expect("token");
    open_data.set_data_position(0);
    let mut open_reply = Parcel::new();
    let error = host
        .transact(binder, code::CONNECT_DEVICE, &mut open_data, &mut open_reply, 0)
        .expect_err("restored off-node must refuse connectDevice");
    assert_eq!(error.code(), "UNSUPPORTED_ANDROID_API");

    // Re-attaching works and serves cleanly with no stale state.
    registry
        .attach(
            &mut host,
            MODULE_ID,
            Profile::Stub,
            &ModuleConfig::default(),
        )
        .expect("re-attach");
    let mut data = request(&mut host);
    data.write_int32(0);
    data.write_int32(0);
    let mut reply = transact(&mut host, code::GET_CAMERA_INFO, data);
    assert_eq!(reply.read_int32().expect("presence"), 1);
    assert_eq!(reply.read_int32().expect("facing"), FACING_BACK);
}

#[test]
fn an_unknown_transaction_is_a_structured_refusal() {
    let (mut host, _registry) = attach();
    let data = request(&mut host);
    let error = error_of(&mut host, 9999, data);
    assert!(
        matches!(error, ServiceError::UnsupportedAndroidApi { .. }),
        "an unknown transaction is a structured refusal, got {error:?}"
    );
    let _ = BinderId(0);
}

#[test]
fn every_device_user_and_callback_code_matches_framework_dex() {
    // 1. All 23 TRANSACTION_* constants on ICameraDeviceUser$Stub.
    let user_ints =
        framework_static_ints_for("Landroid/hardware/camera2/ICameraDeviceUser$Stub;");
    let expected_user_tx: &[(&str, u32)] = &[
        ("TRANSACTION_disconnect", device_code::DISCONNECT),
        ("TRANSACTION_submitRequest", device_code::SUBMIT_REQUEST),
        ("TRANSACTION_submitRequestList", device_code::SUBMIT_REQUEST_LIST),
        ("TRANSACTION_cancelRequest", device_code::CANCEL_REQUEST),
        ("TRANSACTION_beginConfigure", device_code::BEGIN_CONFIGURE),
        ("TRANSACTION_endConfigure", device_code::END_CONFIGURE),
        (
            "TRANSACTION_isSessionConfigurationSupported",
            device_code::IS_SESSION_CONFIGURATION_SUPPORTED,
        ),
        ("TRANSACTION_deleteStream", device_code::DELETE_STREAM),
        ("TRANSACTION_createStream", device_code::CREATE_STREAM),
        ("TRANSACTION_createInputStream", device_code::CREATE_INPUT_STREAM),
        ("TRANSACTION_getInputSurface", device_code::GET_INPUT_SURFACE),
        (
            "TRANSACTION_createDefaultRequest",
            device_code::CREATE_DEFAULT_REQUEST,
        ),
        ("TRANSACTION_getCameraInfo", device_code::GET_CAMERA_INFO),
        ("TRANSACTION_waitUntilIdle", device_code::WAIT_UNTIL_IDLE),
        ("TRANSACTION_flush", device_code::FLUSH),
        ("TRANSACTION_prepare", device_code::PREPARE),
        ("TRANSACTION_tearDown", device_code::TEAR_DOWN),
        ("TRANSACTION_prepare2", device_code::PREPARE2),
        (
            "TRANSACTION_updateOutputConfiguration",
            device_code::UPDATE_OUTPUT_CONFIGURATION,
        ),
        (
            "TRANSACTION_finalizeOutputConfigurations",
            device_code::FINALIZE_OUTPUT_CONFIGURATIONS,
        ),
        (
            "TRANSACTION_setCameraAudioRestriction",
            device_code::SET_CAMERA_AUDIO_RESTRICTION,
        ),
        (
            "TRANSACTION_getGlobalAudioRestriction",
            device_code::GET_GLOBAL_AUDIO_RESTRICTION,
        ),
        ("TRANSACTION_switchToOffline", device_code::SWITCH_TO_OFFLINE),
    ];
    for &(name, expected) in expected_user_tx {
        let actual = user_ints
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| *v)
            .unwrap_or_else(|| {
                panic!("{name} missing from ICameraDeviceUser$Stub in framework dex")
            });
        assert_eq!(
            actual,
            i64::from(expected),
            "ICameraDeviceUser$Stub.{name} drifted from framework dex"
        );
    }

    // 2. All 7 TRANSACTION_* constants on ICameraDeviceCallbacks$Stub.
    let cb_ints =
        framework_static_ints_for("Landroid/hardware/camera2/ICameraDeviceCallbacks$Stub;");
    let expected_cb_tx: &[(&str, u32)] = &[
        ("TRANSACTION_onDeviceError", callback_code::ON_DEVICE_ERROR),
        ("TRANSACTION_onDeviceIdle", callback_code::ON_DEVICE_IDLE),
        ("TRANSACTION_onCaptureStarted", callback_code::ON_CAPTURE_STARTED),
        ("TRANSACTION_onResultReceived", callback_code::ON_RESULT_RECEIVED),
        ("TRANSACTION_onPrepared", callback_code::ON_PREPARED),
        (
            "TRANSACTION_onRepeatingRequestError",
            callback_code::ON_REPEATING_REQUEST_ERROR,
        ),
        (
            "TRANSACTION_onRequestQueueEmpty",
            callback_code::ON_REQUEST_QUEUE_EMPTY,
        ),
    ];
    for &(name, expected) in expected_cb_tx {
        let actual = cb_ints
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| *v)
            .unwrap_or_else(|| {
                panic!("{name} missing from ICameraDeviceCallbacks$Stub in framework dex")
            });
        assert_eq!(
            actual,
            i64::from(expected),
            "ICameraDeviceCallbacks$Stub.{name} drifted from framework dex"
        );
    }

    // 3. ICameraDeviceUser.NO_IN_FLIGHT_REPEATING_FRAMES == -1.
    let iface_ints = framework_static_ints_for("Landroid/hardware/camera2/ICameraDeviceUser;");
    let no_in_flight = iface_ints
        .iter()
        .find(|(n, _)| n == "NO_IN_FLIGHT_REPEATING_FRAMES")
        .map(|(_, v)| *v)
        .expect("NO_IN_FLIGHT_REPEATING_FRAMES missing");
    assert_eq!(no_in_flight, NO_IN_FLIGHT_REPEATING_FRAMES);
}

fn dev_request() -> Parcel {
    let mut p = Parcel::new();
    p.write_interface_token(DEVICE_USER_DESCRIPTOR)
        .expect("token");
    p
}

fn dev_transact(host: &mut ServiceHost, dev: BinderId, tx: u32, mut data: Parcel) -> Parcel {
    data.set_data_position(0);
    let mut reply = Parcel::new();
    host.transact(dev, tx, &mut data, &mut reply, 0)
        .unwrap_or_else(|e| panic!("ICameraDeviceUser code {tx} failed: {e}"));
    reply.set_data_position(0);
    assert_eq!(reply.read_int32().expect("ex header"), 0);
    reply
}

fn write_fake_output_config(p: &mut Parcel, width: i32, height: i32, igbp: BinderId) {
    p.write_int32(1); // typed object present
    p.write_int32(0); // rotation
    p.write_int32(-1); // surfaceGroupId
    p.write_int32(-1); // surfaceType
    p.write_int32(width);
    p.write_int32(height);
    p.write_int32(0); // isDeferredConfig
    p.write_int32(0); // isShared
    // writeTypedList(mSurfaces): count=1, present=1, Surface.writeToParcel
    p.write_int32(1);
    p.write_int32(1);
    p.write_string16("ImageReader-640x480f23m4-1-0").expect("name");
    p.write_int32(0); // isSingleBuffered
    p.write_int32(SURFACE_PARCEL_MAGIC_BQUE as i32);
    p.write_strong_binder(igbp);
    p.write_strong_binder(BinderId::NULL); // surfaceControlHandle
    // Trailing OutputConfiguration fields
    p.write_string16_opt(None).expect("physicalCameraId");
    p.write_int32(0); // isMultiResolution
    p.write_int32(0); // sensorPixelModesUsed[]
    p.write_int64(1); // dynamicRangeProfile = STANDARD
    p.write_int32(-1); // colorSpace
    p.write_int64(0); // streamUseCase
    p.write_int32(0); // timestampBase
    p.write_int32(0); // mirrorMode
    p.write_int32(0); // readoutTimestampEnabled
}

fn write_fake_capture_request(p: &mut Parcel, stream_id: i32, template_id: i32) {
    let sensor = camera_metadata::Sensor {
        back: true,
        orientation: 90,
        width: 4000,
        height: 3000,
    };
    let meta = camera_metadata::default_request(&sensor, template_id).to_bytes();
    p.write_int32(1); // present
    p.write_int32(1); // physicalCameraCount = 1 (logical only)
    p.write_string16("0").expect("logicalCameraId");
    camera_metadata::write_parcel(p, &meta);
    p.write_int32(0); // isReprocess = false
    p.write_int32(0); // surfaces[] empty (converted to stream ids)
    p.write_int32(1); // mStreamIdxArray.length = 1
    p.write_int32(stream_id);
    p.write_int32(0); // surfaceIdx = 0
    p.write_int32(0); // no user tag
}

#[test]
fn slice2_full_capture_and_repeating_lifecycle_delivers_sink_frames_and_validated_results() {
    let (mut host, mut registry) = attach();
    host.permission_store()
        .expect("perm store")
        .lock()
        .unwrap()
        .grant("android.permission.CAMERA");

    // Simulate app process tag 7 registering guest `acos.camera.frame_sink`
    // and passing its own `ICameraDeviceCallbacks` and `IGraphicBufferProducer`.
    let app_tag = 7u32;
    let sink_binder = BinderId((1u64 << 32) | 501);
    let cb_binder = BinderId((1u64 << 32) | 502);
    let igbp_binder = BinderId((1u64 << 32) | 503);
    host.register_guest_service(app_tag, FRAME_SINK_SERVICE.to_string(), sink_binder);
    host.set_binder_owner(cb_binder, app_tag);
    host.set_binder_owner(igbp_binder, app_tag);

    // 1. connectDevice -> ICameraDeviceUser
    let mut data = request(&mut host);
    data.write_strong_binder(cb_binder);
    data.write_string16("0").expect("id");
    data.write_string16("dev.acos.fixtures.cameraprobe").expect("pkg");
    data.write_string16_opt(None).expect("feat");
    data.write_int32(-1);
    data.write_int32(0);
    data.write_int32(34);
    data.write_int32(0);
    let mut reply = transact(&mut host, code::CONNECT_DEVICE, data);
    let dev = reply
        .read_strong_binder()
        .expect("binder")
        .expect("non-null device user");

    // 2. beginConfigure -> createStream (640x480) -> endConfigure
    dev_transact(&mut host, dev, device_code::BEGIN_CONFIGURE, dev_request());

    let mut cs = dev_request();
    write_fake_output_config(&mut cs, 640, 480, igbp_binder);
    let mut cs_reply = dev_transact(&mut host, dev, device_code::CREATE_STREAM, cs);
    let stream_id = cs_reply.read_int32().expect("streamId");
    assert_eq!(stream_id, 0);

    // Verify OP_REGISTER delivery queued for `acos.camera.frame_sink` with exact Surface layout.
    let deliveries = host.take_deliveries();
    assert_eq!(deliveries.len(), 1);
    assert_eq!(deliveries[0].target, sink_binder);
    assert_eq!(deliveries[0].interface, FRAME_SINK_DESCRIPTOR);
    assert_eq!(deliveries[0].code, sink_code::REGISTER_STREAM);
    let mut reg_p = deliveries[0].data.clone();
    reg_p.set_data_position(0);
    reg_p.enforce_interface(FRAME_SINK_DESCRIPTOR).expect("iface");
    assert_eq!(reg_p.read_int32().expect("sid"), 0);
    assert_eq!(
        reg_p.read_string16().expect("name").as_deref(),
        Some("ImageReader-640x480f23m4-1-0")
    );
    assert_eq!(reg_p.read_int32().expect("singleBuf"), 0);
    assert_eq!(
        reg_p.read_int32().expect("magic") as u32,
        SURFACE_PARCEL_MAGIC_BQUE
    );
    assert_eq!(reg_p.read_strong_binder().expect("igbp"), Some(igbp_binder));
    assert_eq!(reg_p.read_strong_binder().expect("scHandle"), None);
    assert_eq!(reg_p.read_int32().expect("w"), 640);
    assert_eq!(reg_p.read_int32().expect("h"), 480);

    let mut ec = dev_request();
    ec.write_int32(0); // operatingMode = NORMAL
    ec.write_int32(0); // sessionParams = null
    ec.write_int64(12345); // startTimeMs
    let mut ec_reply = dev_transact(&mut host, dev, device_code::END_CONFIGURE, ec);
    assert_eq!(ec_reply.read_int32().expect("offlineStreamIds len"), 0);

    // 3. createDefaultRequest(TEMPLATE_STILL_CAPTURE = 2) -> validated metadata blob.
    let mut dr = dev_request();
    dr.write_int32(2);
    let mut dr_reply = dev_transact(&mut host, dev, device_code::CREATE_DEFAULT_REQUEST, dr);
    assert_eq!(dr_reply.read_int32().expect("present"), 1);
    let dr_blob = camera_metadata::read_parcel(&mut dr_reply)
        .expect("framing")
        .expect("non-empty default request");
    let dr_parsed = camera_metadata::validate(&dr_blob).expect("default request validates");
    assert_eq!(
        find_entry(&dr_parsed, tags::CONTROL_CAPTURE_INTENT.id).map(|e| e.as_i32()),
        Some(vec![2]),
        "TEMPLATE_STILL_CAPTURE sets CONTROL_CAPTURE_INTENT = 2"
    );

    // 4. submitRequestList (single-shot, streaming=false)
    let mut sub = dev_request();
    sub.write_int32(1); // 1 request
    write_fake_capture_request(&mut sub, stream_id, 2);
    sub.write_int32(0); // streaming = false
    let mut sub_reply = dev_transact(&mut host, dev, device_code::SUBMIT_REQUEST_LIST, sub);
    assert_eq!(sub_reply.read_int32().expect("SubmitInfo present"), 1);
    let req_id = sub_reply.read_int32().expect("requestId");
    let last_frame = sub_reply.read_int64().expect("lastFrameNumber");
    assert_eq!(req_id, 1);
    assert_eq!(last_frame, 0);

    // Verify exact deliveries: OP_DELIVER -> onCaptureStarted -> onResultReceived -> onRequestQueueEmpty -> onDeviceIdle
    let d = host.take_deliveries();
    assert_eq!(d.len(), 5, "single capture emits 5 oneway deliveries");
    assert_eq!(d[0].target, sink_binder);
    assert_eq!(d[0].code, sink_code::DELIVER_FRAME_FD);
    let mut dp = d[0].data.clone();
    dp.set_data_position(0);
    dp.enforce_interface(FRAME_SINK_DESCRIPTOR).expect("iface");
    assert_eq!(dp.read_int32().unwrap(), 0); // streamId
    assert_eq!(dp.read_int32().unwrap(), 640);
    assert_eq!(dp.read_int32().unwrap(), 480);
    assert_eq!(dp.read_int64().unwrap(), 0); // frame 0
    assert_eq!(dp.read_int64().unwrap(), INITIAL_TIMESTAMP_NS);
    assert_eq!(dp.read_int32().unwrap(), 1); // ACOS_PATTERN_COLORBARS_V1
    let raw_fd = dp.read_file_descriptor().unwrap().expect("memfd for frame 0");
    {
        use std::io::{Read, Seek, SeekFrom};
        use std::os::fd::FromRawFd;
        use acos_services::modules::camera::{ACOS_FRAME_BLOB_MAGIC, ACOS_FRAME_BLOB_VERSION};
        let mut f = unsafe { std::fs::File::from_raw_fd(raw_fd) };
        f.seek(SeekFrom::Start(0)).unwrap();
        let mut hdr = [0u8; 48];
        f.read_exact(&mut hdr).expect("read header");
        assert_eq!(u32::from_le_bytes(hdr[0..4].try_into().unwrap()), ACOS_FRAME_BLOB_MAGIC);
        assert_eq!(u32::from_le_bytes(hdr[4..8].try_into().unwrap()), ACOS_FRAME_BLOB_VERSION);
        let jpeg_off = u32::from_le_bytes(hdr[36..40].try_into().unwrap());
        let jpeg_size = u32::from_le_bytes(hdr[40..44].try_into().unwrap());
        let crc = u32::from_le_bytes(hdr[44..48].try_into().unwrap());
        assert_eq!(crc, test_pattern_yuv420_crc32(640, 480, 0));
        assert!(jpeg_size > 100);
        f.seek(SeekFrom::Start(u64::from(jpeg_off))).unwrap();
        let mut soi = [0u8; 2];
        f.read_exact(&mut soi).unwrap();
        assert_eq!(soi, [0xFF, 0xD8], "frame memfd JPEG SOI");
        std::mem::forget(f);
    }

    // onCaptureStarted
    assert_eq!(d[1].target, cb_binder);
    assert_eq!(d[1].code, callback_code::ON_CAPTURE_STARTED);
    let mut sp = d[1].data.clone();
    sp.set_data_position(0);
    sp.enforce_interface(DEVICE_CALLBACKS_DESCRIPTOR).expect("iface");
    assert_eq!(sp.read_int32().unwrap(), 1); // extras present
    assert_eq!(sp.read_int32().unwrap(), 1); // requestId
    assert_eq!(sp.read_int32().unwrap(), 0); // subsequenceId
    assert_eq!(sp.read_int32().unwrap(), 0); // afTriggerId
    assert_eq!(sp.read_int32().unwrap(), 0); // precaptureTriggerId
    assert_eq!(sp.read_int64().unwrap(), 0); // frameNumber
    assert_eq!(sp.read_int32().unwrap(), 1); // partialResultCount == REQUEST_PARTIAL_RESULT_COUNT
    assert_eq!(sp.read_int32().unwrap(), -1); // errorStreamId
    assert_eq!(sp.read_int32().unwrap(), 0); // errorPhysicalCameraIdPresent
    assert_eq!(sp.read_int64().unwrap(), NO_IN_FLIGHT_REPEATING_FRAMES);
    assert_eq!(sp.read_int64().unwrap(), NO_IN_FLIGHT_REPEATING_FRAMES);
    assert_eq!(sp.read_int64().unwrap(), NO_IN_FLIGHT_REPEATING_FRAMES);
    assert_eq!(sp.read_int32().unwrap(), 0); // hasReadoutTimestamp
    assert_eq!(sp.read_int64().unwrap(), INITIAL_TIMESTAMP_NS);
    assert_eq!(sp.data_size(), sp.data_position(), "enforceNoDataAvail");

    // onResultReceived -> validated TotalCaptureResult metadata
    assert_eq!(d[2].target, cb_binder);
    assert_eq!(d[2].code, callback_code::ON_RESULT_RECEIVED);
    let mut rp = d[2].data.clone();
    rp.set_data_position(0);
    rp.enforce_interface(DEVICE_CALLBACKS_DESCRIPTOR).expect("iface");
    assert_eq!(rp.read_int32().unwrap(), 1); // metadata present
    let res_blob = camera_metadata::read_parcel(&mut rp)
        .expect("framing")
        .expect("result metadata");
    let res_parsed = camera_metadata::validate(&res_blob).expect("TotalCaptureResult validates");
    assert_eq!(
        find_entry(&res_parsed, tags::SENSOR_TIMESTAMP.id).map(|e| e.as_i64()),
        Some(vec![INITIAL_TIMESTAMP_NS])
    );
    assert_eq!(
        find_entry(&res_parsed, tags::CONTROL_CAPTURE_INTENT.id).map(|e| e.as_i32()),
        Some(vec![2])
    );
    assert_eq!(rp.read_int32().unwrap(), 1); // extras present
    assert_eq!(rp.read_int32().unwrap(), 1); // requestId
    assert_eq!(rp.read_int32().unwrap(), 0); // subseq
    assert_eq!(rp.read_int32().unwrap(), 0); // af
    assert_eq!(rp.read_int32().unwrap(), 0); // precap
    assert_eq!(rp.read_int64().unwrap(), 0); // frameNumber
    assert_eq!(rp.read_int32().unwrap(), 1); // partialResultCount
    assert_eq!(rp.read_int32().unwrap(), -1); // errorStreamId
    assert_eq!(rp.read_int32().unwrap(), 0); // errPhys
    assert_eq!(rp.read_int64().unwrap(), NO_IN_FLIGHT_REPEATING_FRAMES);
    assert_eq!(rp.read_int64().unwrap(), NO_IN_FLIGHT_REPEATING_FRAMES);
    assert_eq!(rp.read_int64().unwrap(), NO_IN_FLIGHT_REPEATING_FRAMES);
    assert_eq!(rp.read_int32().unwrap(), 0); // hasReadoutTs
    assert_eq!(rp.read_int32().unwrap(), 0); // PhysicalCaptureResultInfo[0]
    assert_eq!(rp.data_size(), rp.data_position(), "enforceNoDataAvail");

    assert_eq!(d[3].code, callback_code::ON_REQUEST_QUEUE_EMPTY);
    assert_eq!(d[4].code, callback_code::ON_DEVICE_IDLE);

    // 5. Repeating request (streaming = true) -> frame 1 immediately + frames 2, 3 via timed tick.
    let mut rep = dev_request();
    rep.write_int32(1);
    write_fake_capture_request(&mut rep, stream_id, 1);
    rep.write_int32(1); // streaming = true
    let mut rep_reply = dev_transact(&mut host, dev, device_code::SUBMIT_REQUEST_LIST, rep);
    assert_eq!(rep_reply.read_int32().unwrap(), 1);
    let rep_req_id = rep_reply.read_int32().unwrap();
    let prev_last = rep_reply.read_int64().unwrap();
    assert_eq!(rep_req_id, 2);
    assert_eq!(prev_last, NO_IN_FLIGHT_REPEATING_FRAMES);

    let d1 = host.take_deliveries();
    assert_eq!(d1.len(), 3, "frame 1: sink + started + result");
    assert!(
        host.next_timed_deadline().is_some(),
        "repeating request must schedule next_timed_deadline"
    );

    // Wait just past 33ms twice to drive frames 2 and 3 via ServiceHost::take_timed_deliveries.
    std::thread::sleep(std::time::Duration::from_millis(40));
    let d2 = host.take_timed_deliveries();
    assert_eq!(d2.len(), 3, "timed tick delivers frame 2");
    let mut dp2 = d2[0].data.clone();
    dp2.set_data_position(0);
    dp2.enforce_interface(FRAME_SINK_DESCRIPTOR).unwrap();
    let _ = (dp2.read_int32(), dp2.read_int32(), dp2.read_int32());
    assert_eq!(dp2.read_int64().unwrap(), 2);
    assert_eq!(
        dp2.read_int64().unwrap(),
        INITIAL_TIMESTAMP_NS + 2 * FRAME_DURATION_NS
    );

    std::thread::sleep(std::time::Duration::from_millis(40));
    let d3 = host.take_timed_deliveries();
    assert_eq!(d3.len(), 3, "timed tick delivers frame 3");

    // 6. cancelRequest -> returns last frame number (3) and clears timed deadline.
    let mut cancel = dev_request();
    cancel.write_int32(rep_req_id);
    let mut cancel_reply = dev_transact(&mut host, dev, device_code::CANCEL_REQUEST, cancel);
    assert_eq!(cancel_reply.read_int64().unwrap(), 3);
    assert!(host.next_timed_deadline().is_none());
    let _ = host.take_deliveries();

    // 7. waitUntilIdle + disconnect -> releases sink + marks node finished.
    dev_transact(&mut host, dev, device_code::WAIT_UNTIL_IDLE, dev_request());
    dev_transact(&mut host, dev, device_code::DISCONNECT, dev_request());
    let rel = host.take_deliveries();
    assert_eq!(rel.len(), 1);
    assert_eq!(rel[0].target, sink_binder);
    assert_eq!(rel[0].code, sink_code::RELEASE_STREAM);

    // Control op `status` confirms 1 device opened, 0 still open, 4 frames delivered.
    let status = registry
        .control(&mut host, MODULE_ID, "status", &serde_json::json!(null))
        .expect("status");
    assert_eq!(status["calls"]["devices_opened"], 1);
    assert_eq!(status["calls"]["open_devices"], 0);
    assert_eq!(status["calls"]["streams_created"], 1);
    assert_eq!(status["calls"]["frames_delivered"], 4);
}

#[test]
fn test_pattern_colorbars_v1_crc32_is_deterministic_and_frame_tagged() {
    let f0 = test_pattern_yuv420_crc32(640, 480, 0);
    let f0_again = test_pattern_yuv420_crc32(640, 480, 0);
    let f1 = test_pattern_yuv420_crc32(640, 480, 1);
    let f2 = test_pattern_yuv420_crc32(640, 480, 2);
    assert_eq!(f0, f0_again);
    assert_ne!(f0, f1, "frame tag must make consecutive frames distinguishable");
    assert_ne!(f1, f2);
    assert_ne!(f0, 0);
    eprintln!(
        "ACOS_PATTERN_COLORBARS_V1 640x480 CRC32: f0={f0:#010x} f1={f1:#010x} f2={f2:#010x}"
    );
}



#[test]
fn scripted_profile_set_frame_delivers_memfd_yuv_and_jpeg() {
    use std::io::{Read, Seek, SeekFrom};
    use std::os::fd::FromRawFd;
    use acos_services::modules::camera::{
        ieee_crc32, ACOS_FRAME_BLOB_MAGIC, ACOS_FRAME_BLOB_VERSION,
    };

    // 1. `stub` profile refuses `set_frame`.
    {
        let (mut host, mut registry) = attach();
        let err = registry
            .control(
                &mut host,
                MODULE_ID,
                "set_frame",
                &serde_json::json!({"path": "fixtures/build/camera/test-frame.png"}),
            )
            .expect_err("stub must refuse set_frame");
        assert!(err.to_string().contains("scripted"), "got {err}");
    }

    // 2. Attach `scripted`.
    let (mut host, mut registry) = attach_with(Profile::Scripted, serde_json::json!(null));
    host.permission_store()
        .expect("perm store")
        .lock()
        .unwrap()
        .grant("android.permission.CAMERA");

    let err_missing = registry
        .control(
            &mut host,
            MODULE_ID,
            "set_frame",
            &serde_json::json!({"path": "fixtures/camera/does-not-exist.png"}),
        )
        .expect_err("missing file must be rejected");
    assert!(err_missing.to_string().contains("not found"), "got {err_missing}");

    // 3. Inject the generated PNG test frame via `path` (frames are generated, not
    //    committed: `acos_services::modules::camera::write_test_frames`).
    let frames_dir = std::env::temp_dir().join(format!("acos-camera-frames-{}", std::process::id()));
    acos_services::modules::camera::write_test_frames(&frames_dir).expect("write test frames");
    let png_path = frames_dir.join("test-frame.png");
    let res_png = registry
        .control(
            &mut host,
            MODULE_ID,
            "set_frame",
            &serde_json::json!({"path": png_path.to_str().expect("utf-8 temp path")}),
        )
        .expect("set_frame png");
    assert_eq!(res_png["source"], "image");
    assert_eq!(res_png["width"], 640);
    assert_eq!(res_png["height"], 480);
    let png_crc_str = res_png["crc32_yuv"].as_str().expect("crc32_yuv");
    eprintln!("test-frame.png YUV CRC32 = {png_crc_str}");

    let app_tag = 7u32;
    let sink_binder = BinderId((1u64 << 32) | 501);
    let cb_binder = BinderId((1u64 << 32) | 502);
    let igbp_yuv = BinderId((1u64 << 32) | 503);
    host.register_guest_service(app_tag, FRAME_SINK_SERVICE.to_string(), sink_binder);
    host.set_binder_owner(cb_binder, app_tag);
    host.set_binder_owner(igbp_yuv, app_tag);

    let mut conn = request(&mut host);
    conn.write_strong_binder(cb_binder);
    conn.write_string16("0").expect("id");
    conn.write_string16("dev.acos.fixtures.cameraprobe").expect("pkg");
    conn.write_string16_opt(None).expect("feat");
    conn.write_int32(-1);
    conn.write_int32(0);
    conn.write_int32(34);
    conn.write_int32(0);
    let mut conn_reply = transact(&mut host, code::CONNECT_DEVICE, conn);
    let dev = conn_reply.read_strong_binder().unwrap().unwrap();

    dev_transact(&mut host, dev, device_code::BEGIN_CONFIGURE, dev_request());
    let mut cs = dev_request();
    write_fake_output_config(&mut cs, 640, 480, igbp_yuv);
    let mut cs_reply = dev_transact(&mut host, dev, device_code::CREATE_STREAM, cs);
    let stream_id = cs_reply.read_int32().unwrap();
    dev_transact(&mut host, dev, device_code::END_CONFIGURE, dev_request());
    let _ = host.take_deliveries();

    let mut sub = dev_request();
    sub.write_int32(1);
    write_fake_capture_request(&mut sub, stream_id, 2);
    sub.write_int32(0);
    dev_transact(&mut host, dev, device_code::SUBMIT_REQUEST_LIST, sub);

    let d = host.take_deliveries();
    assert_eq!(d[0].target, sink_binder);
    assert_eq!(d[0].code, sink_code::DELIVER_FRAME_FD);
    let mut dp = d[0].data.clone();
    dp.set_data_position(0);
    dp.enforce_interface(FRAME_SINK_DESCRIPTOR).unwrap();
    assert_eq!(dp.read_int32().unwrap(), stream_id);
    assert_eq!(dp.read_int32().unwrap(), 640);
    assert_eq!(dp.read_int32().unwrap(), 480);
    assert_eq!(dp.read_int64().unwrap(), 0);
    assert_eq!(dp.read_int64().unwrap(), INITIAL_TIMESTAMP_NS);
    assert_eq!(dp.read_int32().unwrap(), 2); // scripted image patternId
    let raw_fd = dp.read_file_descriptor().unwrap().expect("blob fd");
    let mut file = unsafe { std::fs::File::from_raw_fd(raw_fd) };
    file.seek(SeekFrom::Start(0)).unwrap();
    let mut blob = Vec::new();
    file.read_to_end(&mut blob).unwrap();
    std::mem::forget(file); // Parcel `dp` still owns `raw_fd`

    assert!(blob.len() > 48);
    let magic = u32::from_le_bytes(blob[0..4].try_into().unwrap());
    let ver = u32::from_le_bytes(blob[4..8].try_into().unwrap());
    let w = u32::from_le_bytes(blob[8..12].try_into().unwrap());
    let h = u32::from_le_bytes(blob[12..16].try_into().unwrap());
    let yuv_off = u32::from_le_bytes(blob[28..32].try_into().unwrap()) as usize;
    let yuv_size = u32::from_le_bytes(blob[32..36].try_into().unwrap()) as usize;
    let jpeg_off = u32::from_le_bytes(blob[36..40].try_into().unwrap()) as usize;
    let jpeg_size = u32::from_le_bytes(blob[40..44].try_into().unwrap()) as usize;
    let crc = u32::from_le_bytes(blob[44..48].try_into().unwrap());
    assert_eq!(magic, ACOS_FRAME_BLOB_MAGIC);
    assert_eq!(ver, ACOS_FRAME_BLOB_VERSION);
    assert_eq!((w, h), (640, 480));
    assert_eq!(yuv_size, 640 * 480 * 3 / 2);
    assert_eq!(format!("{crc:#010x}"), png_crc_str);
    assert_eq!(ieee_crc32(&blob[yuv_off..yuv_off + yuv_size]), crc);
    assert!(jpeg_size > 100);
    assert_eq!(&blob[jpeg_off..jpeg_off + 2], &[0xFF, 0xD8]);
    assert_eq!(&blob[jpeg_off + jpeg_size - 2..jpeg_off + jpeg_size], &[0xFF, 0xD9]);

    // 4. Switch to JPEG fixture via base64, then back to test_pattern.
    let jpg_bytes = std::fs::read(frames_dir.join("test-frame.jpg")).unwrap();
    let _ = std::fs::remove_dir_all(&frames_dir);
    let b64 = encode_test_base64(&jpg_bytes);
    let res_jpg = registry
        .control(
            &mut host,
            MODULE_ID,
            "set_frame",
            &serde_json::json!({"base64": b64}),
        )
        .expect("set_frame base64");
    assert_eq!(res_jpg["width"], 640);
    assert_eq!(res_jpg["height"], 480);
    eprintln!("test-frame.jpg YUV CRC32 = {}", res_jpg["crc32_yuv"]);

    let res_tp = registry
        .control(
            &mut host,
            MODULE_ID,
            "set_frame",
            &serde_json::json!({"source": "test_pattern"}),
        )
        .expect("restore test_pattern");
    assert_eq!(res_tp["pattern"], "ACOS_PATTERN_COLORBARS_V1");

    dev_transact(&mut host, dev, device_code::DISCONNECT, dev_request());
}

fn encode_test_base64(input: &[u8]) -> String {
    const TBL: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in input.chunks(3) {
        let b0 = u32::from(chunk[0]);
        let b1 = u32::from(*chunk.get(1).unwrap_or(&0));
        let b2 = u32::from(*chunk.get(2).unwrap_or(&0));
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(TBL[((n >> 18) & 63) as usize] as char);
        out.push(TBL[((n >> 12) & 63) as usize] as char);
        out.push(if chunk.len() > 1 { TBL[((n >> 6) & 63) as usize] as char } else { '=' });
        out.push(if chunk.len() > 2 { TBL[(n & 63) as usize] as char } else { '=' });
    }
    out
}
