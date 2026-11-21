//! `hw.camera` — the Camera2 service seam (`off`, `stub`).
//!
//! A phone app reaches a camera through `CameraManager`, and `CameraManager`
//! reaches the platform through **one** binder: `ServiceManager.getService(
//! "media.camera")` -> `android.hardware.ICameraService`
//! (`base/core/java/android/hardware/camera2/CameraManager.java`,
//! `CameraManagerGlobal.connectCameraServiceLocked()`). Everything else — the
//! device, the capture session, the buffers — hangs off the binders that
//! service hands back. This module owns that name, so "ACOS has no camera" and
//! "ACOS has one virtual camera" are one attach apart, as ADR-0029 asks.
//!
//! # What `stub` answers, and why each answer is not a lie
//!
//! | code | method | answer |
//! |---|---|---|
//! | 1 | `getNumberOfCameras(int)` | `1` — one back camera, so `CameraManager.getCameraIdList()` is `["0"]` |
//! | 2 | `getCameraInfo(int, boolean)` | `android.hardware.CameraInfo` = `{facing: 0 (back), orientation: 90}` |
//! | 5 | `addListener(ICameraServiceListener)` | one `CameraStatus` `{cameraId: "0", status: PRESENT (1)}` |
//! | 6 | `getConcurrentCameraIds()` | empty array — one camera, no concurrent set |
//! | 7 | `isConcurrentSessionConfigurationSupported(...)` | `false` |
//! | 8 | `removeListener` | `void` |
//! | 9 | `getCameraCharacteristics(String, int, boolean)` | a real `CameraMetadataNative`: the stub's `camera_metadata_t` ([`super::camera_metadata::characteristics`]) in `CameraMetadata::writeToParcel`'s framing; an unknown id is `ServiceSpecificException(ERROR_ILLEGAL_ARGUMENT = 3)` |
//! | 10/11 | `getCameraVendorTagDescriptor`, `getCameraVendorTagCache` | non-null empty parcelable (`present = 1, count = 0`) — no vendor tags published (`libcamera_client.so` rejects null with `UNEXPECTED_NULL`) |
//! | 12 | `getLegacyParameters(int)` | `null` — the API-1 parameters blob is not served |
//! | 13 | `supportsCameraApi(String, int)` | `true` for `CAMERA_API_LEVEL_2`, `false` for level 1 |
//! | 14 | `isHiddenPhysicalCamera(String)` | `false` (no physical camera behind id `0`) |
//! | 18 | `getTorchStrengthLevel(String)` | `0` (no flash) |
//! | 19/20/21 | `notifySystemEvent`, `notifyDisplayConfigurationChange`, `notifyDeviceStateChange` | oneway `void` notifications; recorded, nothing to do |
//! | 22 | `reportExtensionSessionStats(...)` | `null` |
//! | 4 | `connectDevice(...)` | checks `android.permission.CAMERA` (`ServiceSpecificException(ERROR_PERMISSION_DENIED = 1)` if denied), validates id (`ERROR_ILLEGAL_ARGUMENT = 3`), then structured `UNSUPPORTED_ANDROID_API` until Slice 2 |
//! | 3/15/16/17 | API-1 `connect`, `injectCamera`, `setTorchMode`, `turnOnTorchWithStrengthLevel` | structured `UNSUPPORTED_ANDROID_API` |
//!
//! `getCameraCharacteristics` is **not** a fabricated shortcut and the guest is
//! not bypassed: `CameraMetadataNative.readFromParcel` is JNI into the image's
//! own `libandroid_runtime.so` -> `libcamera_client.so` ->
//! `libcamera_metadata.so`, which parse *and validate* the blob. This module
//! writes the format that library reads (`camera_metadata.rs`), so
//! `CameraCharacteristics.get(key)`, `StreamConfigurationMap` and CameraX's
//! `Camera2CameraInfo` all run Google's code over ACOS's values. The values are
//! those of a fixed-focus, no-flash, `LIMITED`-level back camera; nothing claims
//! a capability the stub cannot honour.
//!
//! `connectDevice` returns `ICameraDeviceUser`; opening one leads to
//! `createStream`/`submitRequestList`, i.e. an `IGraphicBufferProducer` owned by
//! the app's `ImageReader` that the service must dequeue frames from. That is
//! Slice 2 of `docs/CAMERA_PLAN.md`; until then the call enforces the `CAMERA`
//! permission gate and then refuses by name instead of handing back a device
//! binder that would fail later, in the middle of a session.
//!
//! # Profiles
//!
//! * `off` — `media.camera` is tombstoned (`TOMBSTONES` row `AospReaction::Tombstone`),
//!   so calling `CameraManager` methods when `hw.camera` is detached throws a
//!   structured `UnsupportedOperationException` containing `UNSUPPORTED_ANDROID_API`
//!   and naming `hw.camera`, while `PackageManager.hasSystemFeature(FEATURE_CAMERA*)`
//!   answers `false` and app startup (`getSystemService(CAMERA_SERVICE)`) is unaffected.
//! * `stub` — one fixed back camera, `CameraCharacteristics` from `config.camera`
//!   (or defaults), answering the full discovery & characteristics surface above.
//! * `scripted` — same node, plus frame sequences via `set_frame`. **Not
//!   implemented yet** (Slices 2-3 in `docs/CAMERA_PLAN.md`).
//!
//! # Control ops
//!
//! | op | args | answer |
//! |---|---|---|
//! | `status` | none | the served camera table + every transaction the app made |
//! | `camera` | none | the single camera's fixed description (what `getCameraInfo` answers) |
//! | `characteristics` | none | the served `CameraCharacteristics` tag summary |
//! | `calls` | none | the transaction log (`{code, method, camera_id}`) |
//! | `reset` | none | clear the log |
//!
//! # Wire source
//!
//! All 22 transaction ids and reply shapes are verified directly against **this
//! image's** framework dex (`.cache/boot/android-34/dex/framework/classes.dex`,
//! `Landroid/hardware/ICameraService$Stub;` + `Proxy`, `CameraInfo`,
//! `CameraStatus`) and native libraries (`libcamera_client.so`,
//! `libcamera_metadata.so`).

use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

use crate::error::{Result, ServiceError};
use crate::exception::{write_no_exception, EX_SERVICE_SPECIFIC};
use crate::host::{CallResult, ServiceHost, ServiceObject};
use crate::module::{
    AttachMode, Budget, ControlOp, Module, ModuleConfig, ModuleManifest, ModuleReport, Profile,
    SecurityClass, ServiceProvide,
};
use crate::modules::camera_metadata;
use crate::permissions::{PermissionStore, PERMISSION_GRANTED};

/// The module id (`acos_launch {"services": [{"id": "hw.camera"}]}`).
pub const MODULE_ID: &str = "hw.camera";
/// The `ServiceManager` name (`CameraManagerGlobal.CAMERA_SERVICE`).
pub const SERVICE_NAME: &str = "media.camera";
/// The AIDL descriptor the client writes as its interface token.
pub const DESCRIPTOR: &str = "android.hardware.ICameraService";

/// `ICameraService.CAMERA_TYPE_BACKWARD_COMPATIBLE`.
pub const CAMERA_TYPE_BACKWARD_COMPATIBLE: i32 = 0;
/// `ICameraService.CAMERA_TYPE_ALL`.
pub const CAMERA_TYPE_ALL: i32 = 1;
/// `ICameraService.CAMERA_API_LEVEL_1` (the deprecated API-1 `Camera` class).
pub const CAMERA_API_LEVEL_1: i32 = 1;
/// `ICameraService.CAMERA_API_LEVEL_2` (Camera2).
pub const CAMERA_API_LEVEL_2: i32 = 2;

/// `ICameraServiceListener.STATUS_NOT_PRESENT` (verified from `framework/classes.dex`).
pub const STATUS_NOT_PRESENT: i32 = 0;
/// `ICameraServiceListener.STATUS_PRESENT` (verified from `framework/classes.dex`).
pub const STATUS_PRESENT: i32 = 1;

/// `ICameraService.ERROR_PERMISSION_DENIED` (verified from `framework/classes.dex`).
pub const ERROR_PERMISSION_DENIED: i32 = 1;
/// `ICameraService.ERROR_ILLEGAL_ARGUMENT` (verified from `framework/classes.dex`).
pub const ERROR_ILLEGAL_ARGUMENT: i32 = 3;

/// `Camera.CameraInfo.CAMERA_FACING_BACK` (legacy `CameraInfo.facing` = 0;
/// note Camera2's `LENS_FACING_BACK` in metadata is 1).
pub const FACING_BACK: i32 = 0;
/// `Camera.CameraInfo.CAMERA_FACING_FRONT` (legacy `CameraInfo.facing` = 1;
/// note Camera2's `LENS_FACING_FRONT` in metadata is 0).
pub const FACING_FRONT: i32 = 1;

/// Transaction ids from `Landroid/hardware/ICameraService$Stub;` in this
/// image's `.cache/boot/android-34/dex/framework/classes.dex` (`TRANSACTION_*`).
pub mod code {
    /// `getNumberOfCameras(int type) -> int`.
    pub const GET_NUMBER_OF_CAMERAS: u32 = 1;
    /// `getCameraInfo(int cameraId, boolean overrideToPortrait) -> CameraInfo`.
    pub const GET_CAMERA_INFO: u32 = 2;
    /// API-1 `connect(...) -> ICamera`.
    pub const CONNECT: u32 = 3;
    /// `connectDevice(...) -> ICameraDeviceUser`.
    pub const CONNECT_DEVICE: u32 = 4;
    /// `addListener(ICameraServiceListener) -> CameraStatus[]`.
    pub const ADD_LISTENER: u32 = 5;
    /// `getConcurrentCameraIds() -> ConcurrentCameraIdCombination[]`.
    pub const GET_CONCURRENT_CAMERA_IDS: u32 = 6;
    /// `isConcurrentSessionConfigurationSupported(...) -> boolean`.
    pub const IS_CONCURRENT_SESSION_CONFIGURATION_SUPPORTED: u32 = 7;
    /// `removeListener(ICameraServiceListener) -> void`.
    pub const REMOVE_LISTENER: u32 = 8;
    /// `getCameraCharacteristics(String, int, boolean) -> CameraMetadataNative`.
    pub const GET_CAMERA_CHARACTERISTICS: u32 = 9;
    /// `getCameraVendorTagDescriptor() -> VendorTagDescriptor`.
    pub const GET_CAMERA_VENDOR_TAG_DESCRIPTOR: u32 = 10;
    /// `getCameraVendorTagCache() -> VendorTagDescriptorCache`.
    pub const GET_CAMERA_VENDOR_TAG_CACHE: u32 = 11;
    /// `getLegacyParameters(int cameraId) -> String`.
    pub const GET_LEGACY_PARAMETERS: u32 = 12;
    /// `supportsCameraApi(String cameraId, int apiLevel) -> boolean`.
    pub const SUPPORTS_CAMERA_API: u32 = 13;
    /// `isHiddenPhysicalCamera(String cameraId) -> boolean`.
    pub const IS_HIDDEN_PHYSICAL_CAMERA: u32 = 14;
    /// `injectCamera(...) -> ICameraInjectionSession`.
    pub const INJECT_CAMERA: u32 = 15;
    /// `setTorchMode(String, boolean, IBinder) -> void`.
    pub const SET_TORCH_MODE: u32 = 16;
    /// `turnOnTorchWithStrengthLevel(String, int, IBinder) -> void`.
    pub const TURN_ON_TORCH_WITH_STRENGTH_LEVEL: u32 = 17;
    /// `getTorchStrengthLevel(String) -> int`.
    pub const GET_TORCH_STRENGTH_LEVEL: u32 = 18;
    /// `notifySystemEvent(int, int[]) -> void` (oneway).
    pub const NOTIFY_SYSTEM_EVENT: u32 = 19;
    /// `notifyDisplayConfigurationChange() -> void` (oneway).
    pub const NOTIFY_DISPLAY_CONFIGURATION_CHANGE: u32 = 20;
    /// `notifyDeviceStateChange(long) -> void` (oneway).
    pub const NOTIFY_DEVICE_STATE_CHANGE: u32 = 21;
    /// `reportExtensionSessionStats(CameraExtensionSessionStats) -> String`.
    pub const REPORT_EXTENSION_SESSION_STATS: u32 = 22;
}

/// One camera the stub serves.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StubCamera {
    /// `CameraManager` id (`"0"`).
    pub id: String,
    /// `CameraInfo.facing`: [`FACING_BACK`] or [`FACING_FRONT`].
    pub facing: i32,
    /// `CameraInfo.orientation`, sensor degrees clockwise (`90` for a phone's
    /// back camera).
    pub orientation: i32,
    /// Pixel-array width (`SENSOR_INFO_PIXEL_ARRAY_SIZE`).
    pub width: i32,
    /// Pixel-array height.
    pub height: i32,
}

impl Default for StubCamera {
    fn default() -> Self {
        Self {
            id: "0".to_string(),
            facing: FACING_BACK,
            orientation: 90,
            width: 4000,
            height: 3000,
        }
    }
}

impl StubCamera {
    /// The JSON form (`status`, `camera`, the report).
    #[must_use]
    pub fn to_json(&self) -> Value {
        json!({
            "id": self.id,
            "facing": match self.facing {
                FACING_FRONT => "front",
                _ => "back",
            },
            "facing_value": self.facing,
            "orientation": self.orientation,
            "width": self.width,
            "height": self.height,
        })
    }

    /// The `CameraCharacteristics` sensor description for this camera.
    #[must_use]
    pub fn sensor(&self) -> camera_metadata::Sensor {
        camera_metadata::Sensor {
            back: self.facing == FACING_BACK,
            orientation: self.orientation,
            width: self.width,
            height: self.height,
        }
    }
}

/// One transaction the app made, as the verdict cites it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CallRecord {
    /// AIDL transaction id.
    pub code: u32,
    /// Method name when the id is one this module knows.
    pub method: &'static str,
    /// The `cameraId` argument when the method has one.
    pub camera_id: Option<String>,
}

impl CallRecord {
    fn to_json(&self) -> Value {
        json!({
            "code": self.code,
            "method": self.method,
            "camera_id": self.camera_id,
        })
    }
}

/// What the app asked the camera service for.
#[derive(Debug, Default)]
pub struct CameraState {
    /// Every transaction handled, in order (bounded to the last 128).
    pub calls: Vec<CallRecord>,
    /// Calls answered with a structured unsupported error.
    pub unsupported: Vec<String>,
    /// `addListener` count (a client registers once per process).
    pub listeners: u32,
}

impl CameraState {
    fn record(&mut self, code: u32, method: &'static str, camera_id: Option<String>) {
        if self.calls.len() >= 128 {
            self.calls.remove(0);
        }
        self.calls.push(CallRecord {
            code,
            method,
            camera_id,
        });
    }

    /// The JSON form (`calls` control op).
    #[must_use]
    pub fn to_json(&self) -> Value {
        json!({
            "calls": self.calls.iter().map(CallRecord::to_json).collect::<Vec<_>>(),
            "unsupported": self.unsupported,
            "listeners": self.listeners,
        })
    }
}

const CONTROL_OPS: [ControlOp; 5] = [
    ControlOp {
        name: "status",
        args: "none — the served camera table and the transaction log",
    },
    ControlOp {
        name: "camera",
        args: "none — the single camera's fixed description (what `getCameraInfo` answers)",
    },
    ControlOp {
        name: "characteristics",
        args: "none — the `CameraCharacteristics` the camera serves (hardware level, sensor, \
               every metadata tag id/type/count), exactly what `getCameraCharacteristics` writes",
    },
    ControlOp {
        name: "calls",
        args: "none — every transaction the app made (`{code, method, camera_id}`)",
    },
    ControlOp {
        name: "reset",
        args: "none — clear the transaction log",
    },
];

fn manifest_ref() -> &'static ModuleManifest {
    static MANIFEST: ModuleManifest = ModuleManifest {
        id: MODULE_ID,
        version: "0.2.0",
        summary: "camera service (`media.camera`): one camera (id, facing, orientation, sensor \
                  size from the config) with real CameraCharacteristics for CameraManager and \
                  CameraX; opening it and frames are not implemented yet; no `real` profile \
                  (ACOS has no camera HAL)",
        services: &[ServiceProvide {
            name: SERVICE_NAME,
            descriptor: DESCRIPTOR,
        }],
        replaces: &[],
        process_initializers: &[],
        features: &["android.hardware.camera", "android.hardware.camera.any"],
        providers: &[],
        broadcasts: &[],
        windows: &[],
        system_apps: &[],
        requires: &[],
        requires_host_tools: &[],
        // `real` is deliberately absent: there is no camera HAL and no sensor.
        profiles: &[Profile::Stub],
        budget: Budget {
            ram_mib: 1,
            startup_ms: 5,
        },
        attach: AttachMode::Hot,
        controls: &CONTROL_OPS,
        security: SecurityClass::InProcess,
    };
    &MANIFEST
}

/// The module.
pub struct CameraModule {
    profile: Profile,
    camera: StubCamera,
    state: Option<Arc<Mutex<CameraState>>>,
}

impl CameraModule {
    /// A detached module.
    #[must_use]
    pub fn new() -> Self {
        Self {
            profile: Profile::Off,
            camera: StubCamera::default(),
            state: None,
        }
    }

    /// The manifest (used by the tests and the registry).
    #[must_use]
    pub fn manifest_ref() -> &'static ModuleManifest {
        manifest_ref()
    }

    /// The camera this module serves, whatever the profile (`config.camera`).
    #[must_use]
    pub fn camera(&self) -> &StubCamera {
        &self.camera
    }
}

impl Default for CameraModule {
    fn default() -> Self {
        Self::new()
    }
}

/// `config.camera` -> [`StubCamera`].
///
/// Only the fields the stub actually answers are configurable: `id`, `facing`,
/// `orientation`, `width`, and `height`. Any other key is refused
/// (`ModuleRejected`, naming the accepted keys) rather than silently ignored.
fn camera_from_config(config: &ModuleConfig) -> Result<StubCamera> {
    let mut camera = StubCamera::default();
    let Some(value) = config.get("camera") else {
        return Ok(camera);
    };
    let Value::Object(map) = value else {
        return Err(ServiceError::ModuleRejected {
            module: MODULE_ID,
            detail: format!("config.camera must be a JSON object, got {value}"),
        });
    };
    for key in map.keys() {
        if !matches!(key.as_str(), "id" | "facing" | "orientation" | "width" | "height") {
            return Err(ServiceError::ModuleRejected {
                module: MODULE_ID,
                detail: format!(
                    "config.camera.{key} is not implemented: this stub configures `id`, \
                     `facing`, `orientation`, `width` and `height` only (frames arrive with \
                     the `scripted` profile)"
                ),
            });
        }
    }
    if let Some(id) = map.get("id") {
        let Some(id) = id.as_str() else {
            return Err(ServiceError::ModuleRejected {
                module: MODULE_ID,
                detail: format!("config.camera.id must be a string, got {id}"),
            });
        };
        if id.is_empty() {
            return Err(ServiceError::ModuleRejected {
                module: MODULE_ID,
                detail: "config.camera.id must not be empty".to_string(),
            });
        }
        camera.id = id.to_string();
    }
    if let Some(facing) = map.get("facing") {
        camera.facing = match facing.as_str() {
            Some("back") => FACING_BACK,
            Some("front") => FACING_FRONT,
            _ => {
                return Err(ServiceError::ModuleRejected {
                    module: MODULE_ID,
                    detail: format!("config.camera.facing must be \"back\" or \"front\", got {facing}"),
                })
            }
        };
    }
    if let Some(orientation) = map.get("orientation") {
        let Some(orientation) = orientation.as_i64() else {
            return Err(ServiceError::ModuleRejected {
                module: MODULE_ID,
                detail: format!("config.camera.orientation must be an integer, got {orientation}"),
            });
        };
        if !(0..=360).contains(&orientation) {
            return Err(ServiceError::ModuleRejected {
                module: MODULE_ID,
                detail: format!("config.camera.orientation must be 0..=360, got {orientation}"),
            });
        }
        camera.orientation = orientation as i32;
    }
    for (key, slot) in [("width", &mut camera.width), ("height", &mut camera.height)] {
        let Some(value) = map.get(key) else { continue };
        let Some(pixels) = value.as_i64().filter(|v| (16..=8192).contains(v)) else {
            return Err(ServiceError::ModuleRejected {
                module: MODULE_ID,
                detail: format!("config.camera.{key} must be an integer in 16..=8192, got {value}"),
            });
        };
        *slot = pixels as i32;
    }
    Ok(camera)
}

struct CameraService {
    camera: StubCamera,
    state: Arc<Mutex<CameraState>>,
    permissions: Option<Arc<Mutex<PermissionStore>>>,
}

impl CameraService {
    fn state(&self) -> std::sync::MutexGuard<'_, CameraState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn record(&self, code: u32, method: &'static str, camera_id: Option<String>) {
        self.state().record(code, method, camera_id);
    }

    fn unsupported(&self, code: u32, method: &str) -> ServiceError {
        {
            let mut state = self.state();
            state.unsupported.push(format!("{code}: {method}"));
        }
        ServiceError::UnsupportedAndroidApi {
            interface: DESCRIPTOR.to_string(),
            code,
            method: format!("{MODULE_ID}: {method}"),
        }
    }

    /// `getNumberOfCameras(int type)`.
    ///
    /// Both camera *types* (`BACKWARD_COMPATIBLE`, `ALL`) include this one back
    /// camera: the type selects API-1-compatible cameras vs every camera, and a
    /// back camera is both.
    fn number_of_cameras(&self, data: &mut acos_parcel::Parcel, reply: &mut acos_parcel::Parcel) -> Result<bool> {
        let kind = data.read_int32().unwrap_or(CAMERA_TYPE_BACKWARD_COMPATIBLE);
        let method = match kind {
            CAMERA_TYPE_BACKWARD_COMPATIBLE => "getNumberOfCameras(BACKWARD_COMPATIBLE)",
            _ => "getNumberOfCameras(ALL)",
        };
        self.record(code::GET_NUMBER_OF_CAMERAS, method, None);
        write_no_exception(reply);
        reply.write_int32(1);
        Ok(true)
    }

    /// `getCameraInfo(int cameraId, boolean overrideToPortrait)`.
    ///
    /// Reply: the `android.hardware.CameraInfo` parcelable written the way
    /// `CameraInfo.writeToParcel` writes it — presence int, `facing`,
    /// `orientation` (`base/core/java/android/hardware/CameraInfo.java:40-45`).
    fn camera_info(&self, data: &mut acos_parcel::Parcel, reply: &mut acos_parcel::Parcel) -> Result<bool> {
        let camera_id = data.read_int32().unwrap_or(0);
        let _override_to_portrait = data.read_int32().unwrap_or(0);
        let Some(_) = self.camera_for_index(camera_id) else {
            return Err(self.unknown_camera(code::GET_CAMERA_INFO, "getCameraInfo", camera_id));
        };
        self.record(
            code::GET_CAMERA_INFO,
            "getCameraInfo",
            Some(self.camera.id.clone()),
        );
        write_no_exception(reply);
        reply.write_int32(1); // the parcelable is present
        reply.write_int32(self.camera.facing);
        reply.write_int32(self.camera.orientation);
        Ok(true)
    }

    /// `addListener(ICameraServiceListener)` -> `CameraStatus[]`.
    ///
    /// `CameraManagerGlobal.getCameraService()` calls this before anything else
    /// and drops the service when it throws, so this reply decides whether
    /// `CameraManager.getCameraIdList()` works at all.
    fn add_listener(&self, data: &mut acos_parcel::Parcel, reply: &mut acos_parcel::Parcel) -> Result<bool> {
        // The listener binder: `read_binder` keeps the reference accounting right.
        let _listener = data.read_strong_binder().ok();
        self.state().listeners += 1;
        self.record(code::ADD_LISTENER, "addListener", None);
        write_no_exception(reply);
        // `writeTypedArray(CameraStatus[], 1)`: the length, then one
        // `writeTypedObject` per element — a presence `int32 1`, then
        // `CameraStatus.writeToParcel` (this image's dex: `writeString(cameraId)`,
        // `writeInt(status)`, `writeStringArray(unavailablePhysicalCameras)`,
        // `writeString(clientPackage)`). The service writes an empty
        // `clientPackage` for a camera nobody has open.
        reply.write_int32(1);
        reply.write_int32(1);
        reply.write_string16(&self.camera.id)?;
        reply.write_int32(STATUS_PRESENT);
        reply.write_int32(0); // String[0]: no unavailable physical cameras
        reply.write_string16("")?;
        Ok(true)
    }

    /// `getCameraCharacteristics(String cameraId, int targetSdkVersion, boolean
    /// overrideToPortrait)` -> `CameraMetadataNative` (`writeTypedObject`).
    ///
    /// The body is a real `camera_metadata_t` ([`camera_metadata::characteristics`])
    /// in `CameraMetadata::writeToParcel`'s framing; the guest's
    /// `libcamera_client.so` parses and validates it, so a blob it rejects fails
    /// the app visibly (`CameraAccessException` / `IllegalArgumentException`).
    /// An id the stub does not serve is the service's own
    /// `ServiceSpecificException(ERROR_ILLEGAL_ARGUMENT)` (`CameraManager` turns it
    /// into `IllegalArgumentException("Camera id ... does not exist")`... via
    /// `ExceptionUtils.throwAsPublicException`).
    fn camera_characteristics(
        &self,
        data: &mut acos_parcel::Parcel,
        reply: &mut acos_parcel::Parcel,
    ) -> Result<bool> {
        let camera_id = data.read_string16_lenient();
        let _target_sdk = data.read_int32().unwrap_or(0);
        let _override_to_portrait = data.read_int32().unwrap_or(0);
        self.record(
            code::GET_CAMERA_CHARACTERISTICS,
            "getCameraCharacteristics",
            camera_id.clone(),
        );
        if camera_id.as_deref() != Some(self.camera.id.as_str()) {
            return Ok(self.illegal_argument(
                reply,
                &format!(
                    "Camera id {:?} does not exist (this stub serves {:?})",
                    camera_id.unwrap_or_default(),
                    self.camera.id
                ),
            ));
        }
        let blob = camera_metadata::characteristics(&self.camera.sensor()).to_bytes();
        write_no_exception(reply);
        reply.write_int32(1); // the typed object is present
        camera_metadata::write_parcel(reply, &blob);
        Ok(true)
    }

    /// A `ServiceSpecificException` reply (`EX_SERVICE_SPECIFIC = -8`),
    /// message, remote stack-trace header size `0`, then the service-specific
    /// `ICameraService.ERROR_*` code.
    fn service_specific_error(
        &self,
        reply: &mut acos_parcel::Parcel,
        error_code: i32,
        message: &str,
    ) -> bool {
        reply.write_int32(EX_SERVICE_SPECIFIC);
        reply
            .write_string16(message)
            .expect("an in-memory string is writable");
        reply.write_int32(0); // remote stack-trace header size
        reply.write_int32(error_code);
        true
    }

    /// A `ServiceSpecificException(ERROR_ILLEGAL_ARGUMENT = 3)` reply
    /// (`ExceptionUtils.throwAsPublicException` turns it into `IllegalArgumentException`).
    fn illegal_argument(&self, reply: &mut acos_parcel::Parcel, message: &str) -> bool {
        self.service_specific_error(reply, ERROR_ILLEGAL_ARGUMENT, message)
    }

    /// `connectDevice(ICameraDeviceCallbacks, String cameraId, String opPackageName,
    /// String featureId, int clientUid, int oomScoreOffset, int targetSdkVersion,
    /// boolean overrideToPortrait) -> ICameraDeviceUser`.
    ///
    /// Enforces `android.permission.CAMERA` against the shared `PermissionStore`
    /// (`ServiceSpecificException(ERROR_PERMISSION_DENIED = 1)` -> `SecurityException`),
    /// checks the camera id (`ERROR_ILLEGAL_ARGUMENT = 3`), and then refuses with
    /// structured `UNSUPPORTED_ANDROID_API` until Slice 2 (`ICameraDeviceUser` +
    /// gralloc buffer delivery).
    fn connect_device(
        &self,
        data: &mut acos_parcel::Parcel,
        reply: &mut acos_parcel::Parcel,
    ) -> Result<bool> {
        let _callbacks = data.read_strong_binder().ok();
        let camera_id = data.read_string16_lenient();
        let _op_package = data.read_string16_lenient();
        let _feature_id = data.read_string16_lenient();
        let _client_uid = data.read_int32().unwrap_or(-1);
        let _oom_score_offset = data.read_int32().unwrap_or(0);
        let _target_sdk = data.read_int32().unwrap_or(34);
        let _override_to_portrait = data.read_int32().unwrap_or(0);
        self.record(code::CONNECT_DEVICE, "connectDevice", camera_id.clone());

        if let Some(store) = self.permissions.as_ref() {
            let granted = store
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .check("android.permission.CAMERA")
                == PERMISSION_GRANTED;
            if !granted {
                return Ok(self.service_specific_error(
                    reply,
                    ERROR_PERMISSION_DENIED,
                    "Permission denial: android.permission.CAMERA is not granted",
                ));
            }
        }

        if camera_id.as_deref() != Some(self.camera.id.as_str()) {
            return Ok(self.illegal_argument(
                reply,
                &format!(
                    "Camera id {:?} does not exist (this stub serves {:?})",
                    camera_id.unwrap_or_default(),
                    self.camera.id
                ),
            ));
        }

        Err(self.unsupported(
            code::CONNECT_DEVICE,
            "connectDevice — opening a camera device and delivering frames to ImageReader/Surface \
             requires Slice 2 (ICameraDeviceUser + gralloc YUV/BLOB buffer transport); see \
             docs/CAMERA_PLAN.md",
        ))
    }

    fn camera_for_index(&self, index: i32) -> Option<&StubCamera> {
        (index == 0).then_some(&self.camera)
    }

    fn unknown_camera(&self, code: u32, method: &str, index: i32) -> ServiceError {
        {
            let mut state = self.state();
            state.unsupported.push(format!("{code}: {method}({index})"));
        }
        ServiceError::UnsupportedAndroidApi {
            interface: DESCRIPTOR.to_string(),
            code,
            method: format!(
                "{MODULE_ID}: {method}({index}) — this stub serves camera index 0 only \
                 (id {:?}); CameraManager enumerates by index",
                self.camera.id
            ),
        }
    }
}

impl ServiceObject for CameraService {
    fn call(
        &mut self,
        interface: &str,
        method: &acos_aidl::MethodInfo,
        _args: &[crate::value::Value],
    ) -> Result<CallResult> {
        Err(self.unsupported(0, &format!("{interface}::{}", method.name)))
    }

    fn raw_transact(
        &mut self,
        interface: &str,
        code: u32,
        data: &mut acos_parcel::Parcel,
        reply: &mut acos_parcel::Parcel,
    ) -> Result<bool> {
        if interface != DESCRIPTOR {
            return Ok(false);
        }
        match code {
            code::GET_NUMBER_OF_CAMERAS => self.number_of_cameras(data, reply),
            code::GET_CAMERA_INFO => self.camera_info(data, reply),
            code::ADD_LISTENER => self.add_listener(data, reply),
            // removeListener(ICameraServiceListener): void
            code::REMOVE_LISTENER => {
                let _listener = data.read_strong_binder().ok();
                self.record(code::REMOVE_LISTENER, "removeListener", None);
                write_no_exception(reply);
                Ok(true)
            }
            // supportsCameraApi(String cameraId, int apiLevel): boolean
            code::SUPPORTS_CAMERA_API => {
                let camera_id = data.read_string16_lenient();
                let api_level = data.read_int32().unwrap_or(0);
                self.record(
                    code::SUPPORTS_CAMERA_API,
                    "supportsCameraApi",
                    camera_id.clone(),
                );
                write_no_exception(reply);
                reply.write_int32(i32::from(api_level == CAMERA_API_LEVEL_2));
                Ok(true)
            }
            // isHiddenPhysicalCamera(String cameraId): boolean
            code::IS_HIDDEN_PHYSICAL_CAMERA => {
                let camera_id = data.read_string16_lenient();
                self.record(
                    code::IS_HIDDEN_PHYSICAL_CAMERA,
                    "isHiddenPhysicalCamera",
                    camera_id,
                );
                write_no_exception(reply);
                reply.write_int32(0);
                Ok(true)
            }
            // getCameraVendorTagDescriptor(): non-null empty VendorTagDescriptor
            // (present = 1, tagCount = 0). Native `BpCameraService::getCameraVendorTagDescriptor`
            // uses `Parcel::readParcelable(Parcelable*)`, which returns UNEXPECTED_NULL on 0.
            code::GET_CAMERA_VENDOR_TAG_DESCRIPTOR => {
                self.record(
                    code::GET_CAMERA_VENDOR_TAG_DESCRIPTOR,
                    "getCameraVendorTagDescriptor",
                    None,
                );
                write_no_exception(reply);
                reply.write_int32(1); // parcelable present
                reply.write_int32(0); // tagCount = 0
                Ok(true)
            }
            // getCameraVendorTagCache(): non-null empty VendorTagDescriptorCache
            // (present = 1, numDescriptors = 0).
            code::GET_CAMERA_VENDOR_TAG_CACHE => {
                self.record(
                    code::GET_CAMERA_VENDOR_TAG_CACHE,
                    "getCameraVendorTagCache",
                    None,
                );
                write_no_exception(reply);
                reply.write_int32(1); // parcelable present
                reply.write_int32(0); // numDescriptors = 0
                Ok(true)
            }
            // getLegacyParameters(int cameraId): null (no API-1 parameters blob)
            code::GET_LEGACY_PARAMETERS => {
                let camera_id = data.read_int32().unwrap_or(0);
                self.record(
                    code::GET_LEGACY_PARAMETERS,
                    "getLegacyParameters",
                    self.camera_for_index(camera_id).map(|c| c.id.clone()),
                );
                write_no_exception(reply);
                reply.write_string16_opt(None)?;
                Ok(true)
            }
            // getConcurrentCameraIds(): empty array, one camera has no concurrent set
            code::GET_CONCURRENT_CAMERA_IDS => {
                self.record(code::GET_CONCURRENT_CAMERA_IDS, "getConcurrentCameraIds", None);
                write_no_exception(reply);
                reply.write_int32(0);
                Ok(true)
            }
            // getTorchStrengthLevel(String cameraId): 0 (no flash)
            code::GET_TORCH_STRENGTH_LEVEL => {
                let camera_id = data.read_string16_lenient();
                self.record(
                    code::GET_TORCH_STRENGTH_LEVEL,
                    "getTorchStrengthLevel",
                    camera_id,
                );
                write_no_exception(reply);
                reply.write_int32(0);
                Ok(true)
            }
            // isConcurrentSessionConfigurationSupported(...): false
            code::IS_CONCURRENT_SESSION_CONFIGURATION_SUPPORTED => {
                self.record(
                    code::IS_CONCURRENT_SESSION_CONFIGURATION_SUPPORTED,
                    "isConcurrentSessionConfigurationSupported",
                    None,
                );
                write_no_exception(reply);
                reply.write_int32(0);
                Ok(true)
            }
            // reportExtensionSessionStats(CameraExtensionSessionStats): null
            code::REPORT_EXTENSION_SESSION_STATS => {
                self.record(
                    code::REPORT_EXTENSION_SESSION_STATS,
                    "reportExtensionSessionStats",
                    None,
                );
                write_no_exception(reply);
                reply.write_string16_opt(None)?;
                Ok(true)
            }
            // notifyDeviceStateChange(long), notifyDisplayConfigurationChange(),
            // notifySystemEvent(int, int[]): oneway notifications, nothing to do
            code::NOTIFY_DEVICE_STATE_CHANGE
            | code::NOTIFY_DISPLAY_CONFIGURATION_CHANGE
            | code::NOTIFY_SYSTEM_EVENT => {
                let method = match code {
                    code::NOTIFY_DEVICE_STATE_CHANGE => "notifyDeviceStateChange",
                    code::NOTIFY_DISPLAY_CONFIGURATION_CHANGE => "notifyDisplayConfigurationChange",
                    _ => "notifySystemEvent",
                };
                self.record(code, method, None);
                write_no_exception(reply);
                Ok(true)
            }
            // getCameraCharacteristics(String, int, boolean): CameraMetadataNative
            code::GET_CAMERA_CHARACTERISTICS => self.camera_characteristics(data, reply),
            // connectDevice(...): CAMERA permission check + Slice 2 refusal.
            code::CONNECT_DEVICE => self.connect_device(data, reply),
            // API-1 connect, setTorchMode, turnOnTorchWithStrengthLevel, injectCamera
            code::CONNECT
            | code::SET_TORCH_MODE
            | code::TURN_ON_TORCH_WITH_STRENGTH_LEVEL
            | code::INJECT_CAMERA => {
                let method = match code {
                    code::CONNECT => "connect (API-1 camera)",
                    code::SET_TORCH_MODE => "setTorchMode (no flash)",
                    code::TURN_ON_TORCH_WITH_STRENGTH_LEVEL => {
                        "turnOnTorchWithStrengthLevel (no flash)"
                    }
                    _ => "injectCamera (no injected camera source)",
                };
                Err(self.unsupported(code, method))
            }
            other => Ok({
                let _ = other;
                false
            }),
        }
    }
}

impl Module for CameraModule {
    fn manifest(&self) -> &'static ModuleManifest {
        manifest_ref()
    }

    fn attach(
        &mut self,
        host: &mut ServiceHost,
        profile: Profile,
        config: &ModuleConfig,
    ) -> Result<()> {
        if profile != Profile::Stub {
            return Err(ServiceError::ModuleRejected {
                module: MODULE_ID,
                detail: format!(
                    "{profile} is not implemented: there is no camera HAL or sensor to serve \
                     `real`, and `scripted` (agent-supplied frames) arrives with \
                     docs/CAMERA_PLAN.md slice 3; use `stub`"
                ),
            });
        }
        self.camera = camera_from_config(config)?;
        let state = Arc::new(Mutex::new(CameraState::default()));
        let permissions = host.permission_store();
        host.register_dynamic_named(
            SERVICE_NAME,
            DESCRIPTOR,
            Box::new(CameraService {
                camera: self.camera.clone(),
                state: Arc::clone(&state),
                permissions,
            }),
        );
        self.state = Some(state);
        self.profile = profile;
        Ok(())
    }

    fn detach(&mut self, _host: &mut ServiceHost) -> Result<()> {
        self.state = None;
        self.profile = Profile::Off;
        Ok(())
    }

    fn control(&mut self, _host: &mut ServiceHost, op: &str, _args: &Value) -> Result<Value> {
        let Some(state) = self.state.as_ref() else {
            return Err(ServiceError::ModuleRejected {
                module: MODULE_ID,
                detail: format!("module is off, so {op:?} was not applied"),
            });
        };
        match op {
            "status" => Ok(json!({
                "cameras": [self.camera.to_json()],
                "calls": state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .to_json(),
            })),
            "camera" => Ok(self.camera.to_json()),
            "characteristics" => {
                let metadata = camera_metadata::characteristics(&self.camera.sensor());
                let blob = metadata.to_bytes();
                let parsed = camera_metadata::validate(&blob).map_err(|detail| {
                    ServiceError::ModuleRejected {
                        module: MODULE_ID,
                        detail: format!("the served characteristics are malformed: {detail}"),
                    }
                })?;
                Ok(json!({
                    "camera": self.camera.to_json(),
                    "hardware_level": "LIMITED",
                    "bytes": blob.len(),
                    "entries": parsed
                        .entries()
                        .iter()
                        .map(|e| json!({
                            "tag": format!("{:#010x}", e.tag),
                            "type": format!("{:?}", e.ty),
                            "count": e.count,
                        }))
                        .collect::<Vec<_>>(),
                }))
            }
            "calls" => Ok(state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .to_json()),
            "reset" => {
                let mut state = state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                *state = CameraState::default();
                Ok(json!({"reset": true}))
            }
            other => Err(ServiceError::ModuleRejected {
                module: MODULE_ID,
                detail: format!(
                    "unknown control op {other:?}; this module implements {}",
                    CONTROL_OPS
                        .iter()
                        .map(|op| op.name)
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            }),
        }
    }

    fn report(&self) -> ModuleReport {
        let mut report = ModuleReport::of(manifest_ref(), self.profile);
        if self.profile.is_attached() {
            let (calls, unsupported, listeners) = self
                .state
                .as_ref()
                .map(|state| {
                    let state = state
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    (state.calls.len(), state.unsupported.len(), state.listeners)
                })
                .unwrap_or((0, 0, 0));
            report.reason = Some(format!(
                "{} profile: one fixed {} camera (id {:?}, orientation {}), no frames; \
                 getCameraIdList/getCameraInfo answer from the config, \
                 getCameraCharacteristics answers the camera's real metadata; \
                 connectDevice answers the structured unsupported error until slice 2; \
                 {listeners} listener(s), \
                 {calls} transaction(s) recorded, {unsupported} refused",
                self.profile,
                match self.camera.facing {
                    FACING_FRONT => "front",
                    _ => "back",
                },
                self.camera.id,
                self.camera.orientation
            ));
            report.details = Some(json!({
                "cameras": [self.camera.to_json()],
                "frames": false,
                "controls": ["status", "camera", "characteristics", "calls", "reset"],
            }));
        }
        report
    }
}
