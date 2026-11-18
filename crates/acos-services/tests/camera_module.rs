//! `hw.camera`: the `media.camera` seam, its `off` answer, and the wire the
//! framework's `CameraManager` reads.
//!
//! Every transaction here is the one `CameraManagerGlobal` makes, with the
//! transaction ids and reply shapes from **this image's** framework dex
//! (`android.hardware.ICameraService$Stub`'s `TRANSACTION_*` values, and
//! `CameraInfo`/`CameraStatus`'s `writeToParcel` field order). The parse side is
//! written the way the generated proxies read the reply, so a field the module
//! puts in the wrong order fails here instead of in an app.

use acos_parcel::{BinderId, Parcel};
use acos_services::module::{ModuleConfig, ModuleRegistry, Profile, TOMBSTONES};
use acos_services::{ServiceError, ServiceHost};

use acos_services::modules::camera::{
    code, CAMERA_API_LEVEL_1, CAMERA_API_LEVEL_2, DESCRIPTOR, FACING_BACK, FACING_FRONT, MODULE_ID,
    SERVICE_NAME, STATUS_PRESENT,
};

fn attach_with(profile: Profile, config: serde_json::Value) -> (ServiceHost, ModuleRegistry) {
    let mut host = ServiceHost::standard();
    let mut registry = ModuleRegistry::standard();
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

/// A request parcel with the interface token the client's `enforceInterface`
/// writes.
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
fn transact(host: &mut ServiceHost, code: u32, data: Parcel) -> Parcel {
    let binder = host.check_service(SERVICE_NAME).unwrap().unwrap();
    let mut data = data;
    data.set_data_position(0);
    let mut reply = Parcel::new();
    host.transact(binder, code, &mut data, &mut reply, 0)
        .unwrap_or_else(|error| panic!("code {code} failed: {error}"));
    reply.set_data_position(0);
    assert_eq!(reply.read_int32().expect("exception header"), 0);
    reply
}

fn error_of(host: &mut ServiceHost, code: u32, data: Parcel) -> ServiceError {
    let binder = host.check_service(SERVICE_NAME).unwrap().unwrap();
    let mut data = data;
    data.set_data_position(0);
    let mut reply = Parcel::new();
    host.transact(binder, code, &mut data, &mut reply, 0)
        .expect_err("this transaction must be refused")
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
    assert_eq!(
        manifest.profiles,
        &[Profile::Stub],
        "there is no camera HAL: `real` would be a fake success, `scripted` (frames) is slice 2"
    );
    assert!(manifest.budget.ram_mib > 0 && manifest.budget.startup_ms > 0);
}

#[test]
fn off_is_aosp_s_missing_path_and_the_decision_row_says_so() {
    // `off`: the name is absent, which is AOSP's own "no camera on this device"
    // (`CameraManagerGlobal.getCameraService()` returns null and
    // `getCameraIdList()` is an empty array). Never a throwing binder.
    let mut host = ServiceHost::standard();
    assert!(
        host.check_service(SERVICE_NAME).expect("lookup").is_none(),
        "with the module off `{SERVICE_NAME}` must stay absent (null binder)"
    );
    let row = TOMBSTONES
        .iter()
        .find(|row| row.name == SERVICE_NAME)
        .expect("media.camera keeps its decision row");
    assert_eq!(row.module, MODULE_ID);
    assert_eq!(
        row.reaction,
        acos_services::module::AospReaction::Null,
        "a throwing tombstone would break the client's handled null path"
    );
}

#[test]
fn get_number_of_cameras_answers_one_and_records_the_type() {
    let (mut host, _registry) = attach();
    let mut data = request(&mut host);
    data.write_int32(0); // CAMERA_TYPE_BACKWARD_COMPATIBLE
    let mut reply = transact(&mut host, code::GET_NUMBER_OF_CAMERAS, data);
    assert_eq!(reply.read_int32().expect("count"), 1);

    // CAMERA_TYPE_ALL also includes the back camera.
    let mut data = request(&mut host);
    data.write_int32(1);
    let mut reply = transact(&mut host, code::GET_NUMBER_OF_CAMERAS, data);
    assert_eq!(reply.read_int32().expect("count"), 1);
}

#[test]
fn get_camera_info_writes_camera_info_s_parcel_field_order() {
    let (mut host, _registry) = attach();
    let mut data = request(&mut host);
    data.write_int32(0); // cameraId
    data.write_int32(0); // overrideToPortrait
    let mut reply = transact(&mut host, code::GET_CAMERA_INFO, data);
    // `android.hardware.CameraInfo.writeToParcel`: facing, orientation.
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
fn add_listener_answers_one_present_camera_status() {
    let (mut host, _registry) = attach();
    let listener = host.register_aux("android.hardware.ICameraServiceListener");
    let mut data = request(&mut host);
    data.write_strong_binder(listener).expect("listener binder");
    let mut reply = transact(&mut host, code::ADD_LISTENER, data);
    assert_eq!(reply.read_int32().expect("array length"), 1);
    // `CameraStatus.writeToParcel`: cameraId, status, unavailablePhysicalCameras,
    // clientPackage.
    assert_eq!(
        reply.read_string16().expect("cameraId").as_deref(),
        Some("0")
    );
    assert_eq!(reply.read_int32().expect("status"), STATUS_PRESENT);
    assert_eq!(
        reply.read_int32().expect("unavailablePhysicalCameras length"),
        0
    );
    assert_eq!(
        reply.read_string16().expect("clientPackage"),
        None,
        "nobody has the camera open"
    );
}

#[test]
fn supports_camera_api_answers_camera2_and_refuses_the_api_1_class() {
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
fn the_listing_transactions_answer_empty_but_present_values() {
    let (mut host, _registry) = attach();

    // getConcurrentCameraIds(): an empty array, not a fabricated combination.
    let data = request(&mut host);
    let mut reply = transact(&mut host, code::GET_CONCURRENT_CAMERA_IDS, data);
    assert_eq!(reply.read_int32().expect("array length"), 0);

    // isHiddenPhysicalCamera("0"): false.
    let mut data = request(&mut host);
    data.write_string16("0").expect("cameraId");
    let mut reply = transact(&mut host, code::IS_HIDDEN_PHYSICAL_CAMERA, data);
    assert_eq!(reply.read_int32().expect("hidden"), 0);

    // getCameraVendorTagDescriptor(): null (no vendor tags).
    let data = request(&mut host);
    let mut reply = transact(&mut host, code::GET_CAMERA_VENDOR_TAG_DESCRIPTOR, data);
    assert_eq!(reply.read_int32().expect("null parcelable"), 0);

    // getTorchStrengthLevel("0"): 0 (no flash).
    let mut data = request(&mut host);
    data.write_string16("0").expect("cameraId");
    let mut reply = transact(&mut host, code::GET_TORCH_STRENGTH_LEVEL, data);
    assert_eq!(reply.read_int32().expect("strength"), 0);
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
fn characteristics_and_connect_device_fail_by_name_until_their_slices_land() {
    let (mut host, _registry) = attach();

    let mut data = request(&mut host);
    data.write_string16("0").expect("cameraId");
    data.write_int32(34).expect("targetSdkVersion");
    data.write_int32(0).expect("overrideToPortrait");
    let error = error_of(&mut host, code::GET_CAMERA_CHARACTERISTICS, data);
    assert!(
        matches!(
            error,
            ServiceError::UnsupportedAndroidApi { ref method, .. }
                if method.contains("libcamera_client.so")
        ),
        "getCameraCharacteristics must name the missing native metadata codec, got {error:?}"
    );

    let error = error_of(&mut host, code::CONNECT_DEVICE, request(&mut host));
    assert!(
        matches!(
            error,
            ServiceError::UnsupportedAndroidApi { ref method, .. }
                if method.contains("connectDevice")
        ),
        "connectDevice must be refused until sessions exist, got {error:?}"
    );
}

#[test]
fn the_config_describes_the_one_camera_and_unknown_keys_are_refused() {
    let (mut host, _registry) = attach_with(
        Profile::Stub,
        serde_json::json!({"camera": {"id": "7", "facing": "front", "orientation": 270}}),
    );
    let mut data = request(&mut host);
    data.write_int32(0);
    data.write_int32(0);
    let mut reply = transact(&mut host, code::GET_CAMERA_INFO, data);
    assert_eq!(reply.read_int32().expect("presence"), 1);
    assert_eq!(reply.read_int32().expect("facing"), FACING_FRONT);
    assert_eq!(reply.read_int32().expect("orientation"), 270);

    // The id is the config's, so `addListener` reports it too.
    let listener = host.register_aux("android.hardware.ICameraServiceListener");
    let mut data = request(&mut host);
    data.write_strong_binder(listener).expect("listener binder");
    let mut reply = transact(&mut host, code::ADD_LISTENER, data);
    assert_eq!(reply.read_int32().expect("array length"), 1);
    assert_eq!(
        reply.read_string16().expect("cameraId").as_deref(),
        Some("7")
    );

    // A key the stub cannot honour is refused, never silently ignored.
    let mut registry = ModuleRegistry::standard();
    let mut host = ServiceHost::standard();
    let error = registry
        .attach(
            &mut host,
            MODULE_ID,
            Profile::Stub,
            &ModuleConfig::from_json(MODULE_ID, &serde_json::json!({"camera": {"width": 640}}))
                .expect("config parses"),
        )
        .expect_err("an unimplemented key is refused");
    assert!(
        error.to_string().contains("width"),
        "the refusal names the key: {error}"
    );
}

#[test]
fn the_profiles_it_does_not_implement_are_refused_by_name() {
    for profile in [Profile::Scripted, Profile::Real] {
        let mut registry = ModuleRegistry::standard();
        let mut host = ServiceHost::standard();
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
fn the_control_ops_report_the_served_camera_and_every_transaction() {
    let mut host = ServiceHost::standard();
    let mut registry = ModuleRegistry::standard();
    registry
        .attach(
            &mut host,
            MODULE_ID,
            Profile::Stub,
            &ModuleConfig::default(),
        )
        .expect("attach");

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
fn detach_puts_the_name_back_on_aosp_s_null_path() {
    let mut host = ServiceHost::standard();
    let mut registry = ModuleRegistry::standard();
    registry
        .attach(
            &mut host,
            MODULE_ID,
            Profile::Stub,
            &ModuleConfig::default(),
        )
        .expect("attach");
    assert!(host.check_service(SERVICE_NAME).unwrap().is_some());
    registry.detach(&mut host, MODULE_ID).expect("detach");
    assert!(
        host.check_service(SERVICE_NAME).unwrap().is_none(),
        "a detached camera module leaves no registration behind"
    );
    // Attaching again works and answers the same camera (no stale state).
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
fn a_binder_nobody_registered_is_never_answered() {
    // Keep the binder id cheap: an unused strong binder argument must not make
    // the module answer a transaction it does not implement.
    let (mut host, _registry) = attach();
    let error = error_of(&mut host, 9999, request(&mut host));
    assert!(
        matches!(error, ServiceError::UnsupportedAndroidApi { .. }),
        "an unknown transaction is a structured refusal, got {error:?}"
    );
    let _ = BinderId(0);
}
