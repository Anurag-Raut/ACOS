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
    code, CAMERA_API_LEVEL_1, CAMERA_API_LEVEL_2, CAMERA_TYPE_ALL, CAMERA_TYPE_BACKWARD_COMPATIBLE,
    DESCRIPTOR, ERROR_ILLEGAL_ARGUMENT, ERROR_PERMISSION_DENIED, FACING_BACK, FACING_FRONT,
    MODULE_ID, SERVICE_NAME, STATUS_NOT_PRESENT, STATUS_PRESENT,
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
        "framework dex directory missing at {}; run `source tools/agent-env.sh`",
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
        &[Profile::Stub],
        "there is no camera HAL: `real` would be a fake success, `scripted` (frames) is slice 3"
    );
    assert!(manifest.budget.ram_mib > 0 && manifest.budget.startup_ms > 0);
}

#[test]
fn off_installs_structured_tombstone_naming_hw_camera() {
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
        "off mode must install a structured UNSUPPORTED_ANDROID_API tombstone"
    );

    let binder = host
        .check_service(SERVICE_NAME)
        .expect("lookup")
        .expect("tombstone binder installed");
    let mut data = Parcel::new();
    data.write_interface_token(DESCRIPTOR).expect("token");
    data.set_data_position(0);
    let mut reply = Parcel::new();
    let error = host
        .transact(binder, code::ADD_LISTENER, &mut data, &mut reply, 0)
        .expect_err("tombstone must refuse transactions");
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
                attach.contains("acos_launch") && attach.contains(MODULE_ID),
                "tombstone attach hint must name hw.camera, got {attach}"
            );
        }
        other => panic!("expected ModuleUnavailable, got {other:?}"),
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
fn connect_device_enforces_camera_permission_then_refuses_until_slice_2() {
    let (mut host, _registry) = attach();
    let cb = host.register_aux("android.hardware.camera2.ICameraDeviceCallbacks");

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

    // 4. Valid camera id with CAMERA granted -> structured UNSUPPORTED_ANDROID_API citing Slice 2.
    let data = build_connect(&mut host, "0");
    let error = error_of(&mut host, code::CONNECT_DEVICE, data);
    assert_eq!(error.code(), "UNSUPPORTED_ANDROID_API");
    assert!(
        matches!(
            error,
            ServiceError::UnsupportedAndroidApi { ref method, .. }
                if method.contains("connectDevice") && method.contains("CAMERA_PLAN.md")
        ),
        "expected structured Slice 2 refusal, got {error:?}"
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
    for profile in [Profile::Scripted, Profile::Real] {
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
fn detach_restores_the_tombstone_and_reattach_serves_cleanly() {
    let (mut host, mut registry) = attach();
    assert!(host.check_service(SERVICE_NAME).unwrap().is_some());

    registry.detach(&mut host, MODULE_ID).expect("detach");

    // Detach must restore the structured tombstone, never leave a silent null.
    let binder = host
        .check_service(SERVICE_NAME)
        .unwrap()
        .expect("tombstone restored on detach");
    let mut data = Parcel::new();
    data.write_interface_token(DESCRIPTOR).expect("token");
    data.set_data_position(0);
    let mut reply = Parcel::new();
    let error = host
        .transact(binder, code::GET_NUMBER_OF_CAMERAS, &mut data, &mut reply, 0)
        .expect_err("restored tombstone must refuse");
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
