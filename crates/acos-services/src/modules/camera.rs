//! `hw.camera` — the Camera2 service seam (`off`, `stub`).
//!
//! A phone app reaches a camera through `CameraManager`, and `CameraManager`
//! reaches the platform through **one** binder: `ServiceManager.getService(
//! "media.camera")` -> `android.hardware.ICameraService`
//! (`base/core/java/android/hardware/camera2/CameraManager.java`,
//! `CameraManagerGlobal.getCameraService()`). Everything else — the device, the
//! capture session, the buffers — hangs off the binders that service hands back.
//! This module owns that name, so "ACOS has no camera" and "ACOS has one
//! scripted camera" are one attach apart, exactly as ADR-0029 asks.
//!
//! # What `stub` answers, and why each answer is not a lie
//!
//! | code | method | answer |
//! |---|---|---|
//! | 6 | `getNumberOfCameras(int)` | `1` — one back camera, so `CameraManager.getCameraIdList()` is `["0"]` |
//! | 4 | `getCameraInfo(int, boolean)` | `android.hardware.CameraInfo` = `{facing: 0 (back), orientation: 90}` |
//! | 13 | `addListener(ICameraServiceListener)` | one `CameraStatus` `{cameraId: "0", status: PRESENT (0)}` |
//! | 20 | `removeListener` | `void` |
//! | 22 | `supportsCameraApi(String, int)` | `true` for `CAMERA_API_LEVEL_2`, `false` for level 1 |
//! | 15 | `isHiddenPhysicalCamera(String)` | `false` (no physical camera behind id `0`) |
//! | 7/14/21 | `notifyDeviceStateChange`, `notifyDisplayConfigurationChange`, `notifySystemEvent` | `void` (notifications the client sends; nothing to do) |
//! | 2/9 | `getCameraVendorTagDescriptor`, `getCameraVendorTagCache` | `null` — no vendor tags are published |
//! | 10 | `getLegacyParameters(int)` | `null` — the API-1 parameters blob is not served |
//! | 11 | `getConcurrentCameraIds()` | empty array — one camera, no concurrent set |
//! | 12 | `getTorchStrengthLevel(String)` | `0` (no flash) |
//! | 18 | `isConcurrentSessionConfigurationSupported(...)` | `false` |
//! | 19 | `reportExtensionSessionStats(...)` | `null` |
//! | 3 | `getCameraCharacteristics(...)` | structured `UNSUPPORTED_ANDROID_API` — see below |
//! | 5 | `connectDevice(...)` | structured `UNSUPPORTED_ANDROID_API` — see below |
//! | 1/8/16/17 | `injectCamera`, `setTorchMode`, `turnOnTorchWithStrengthLevel`, API-1 `connect` | structured `UNSUPPORTED_ANDROID_API` |
//!
//! `getCameraCharacteristics` is **not** a fabricated blob. The reply type is
//! `android.hardware.camera2.impl.CameraMetadataNative`, whose
//! `readFromParcel`/`writeToParcel` are JNI (`libcamera_client.so`'s
//! `CameraMetadata`), and this guest ships no `libcamera_client.so`: a metadata
//! blob written here would be parsed by *native* code ACOS does not have, so the
//! honest answer is the structured error that names the missing piece. Serving
//! it is slice 3 of `docs/CAMERA_PLAN.md` (the ACOS-side `CameraMetadata` codec
//! plus its JNI seam), not a stub's job.
//!
//! `connectDevice` returns `ICameraDeviceUser`; opening one leads to
//! `configureStreams` + `processCaptureRequest`, i.e. gralloc buffers, and that
//! is slice 4. Until then the call is refused by name instead of handing back a
//! device binder that would fail later, in the middle of a session, with a
//! shape no app can act on.
//!
//! # Profiles
//!
//! * `off` — the name answers `null`, AOSP's own "no camera on this device"
//!   path: `CameraManagerGlobal.getCameraService()` returns `null` and
//!   `getCameraIdList()` is `new String[0]`. This is the pre-module behaviour and
//!   the `TOMBSTONES` row keeps it.
//! * `stub` — one fixed back camera, fixed `CameraCharacteristics` values
//!   (`config.camera` / the defaults), the table above.
//! * `scripted` — same node, plus frames. **Not implemented yet** (slice 2:
//!   `set_frame` with a host-side PNG/JPEG or the built-in test pattern, which
//!   needs `connectDevice` and `ImageReader` surfaces from slice 3/4).
//!
//! # Control ops
//!
//! | op | args | answer |
//! |---|---|---|
//! | `status` | none | the served camera table + every transaction the app made |
//! | `camera` | none | the single camera's fixed description (what `getCameraInfo` answers) |
//! | `calls` | none | the transaction log (`{code, method, camera_id}`) |
//! | `reset` | none | clear the log |
//!
//! # Wire source
//!
//! Transaction ids and reply shapes are read from **this image's** framework
//! dex: `android.hardware.ICameraService$Stub`'s `TRANSACTION_*` static values in
//! `.cache/boot/android-34/dex/framework/classes.dex` (codes 1-12, 14-16, 18-22
//! are named there; `addListener` = 13 and the API-1 `connect` = 17 are the two
//! ids the interface declares without a `TRANSACTION_` constant, which is why
//! the AIDL version markers `API_VERSION_1 = 13` / `API_VERSION_2 = 17` are
//! those exact numbers). `CameraInfo`'s parcel is two ints
//! (`base/core/java/android/hardware/CameraInfo.java:40-45`: `facing`,
//! `orientation`) and `CameraStatus`'s is `(String, int, String[], String)`
//! (`base/core/java/android/hardware/CameraStatus.java:42-47`).

use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

use crate::error::{Result, ServiceError};
use crate::exception::write_no_exception;
use crate::host::{CallResult, ServiceHost, ServiceObject};
use crate::module::{
    AttachMode, Budget, ControlOp, Module, ModuleConfig, ModuleManifest, ModuleReport, Profile,
    SecurityClass, ServiceProvide,
};

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
/// `ICameraServiceListener.STATUS_PRESENT`.
pub const STATUS_PRESENT: i32 = 0;

/// `LENS_FACING_BACK` / `Camera.CameraInfo.CAMERA_FACING_BACK`: the legacy
/// `android.hardware.Camera.CameraInfo.facing` value a back camera reports
/// (`base/core/java/android/hardware/Camera.java`, `CAMERA_FACING_BACK = 0`).
pub const FACING_BACK: i32 = 0;
/// `CAMERA_FACING_FRONT`.
pub const FACING_FRONT: i32 = 1;

/// Transaction ids, from `ICameraService$Stub`'s `TRANSACTION_*` values in this
/// image's `framework/classes.dex` (see the module docs for how 13 and 17 are
/// resolved).
pub mod code {
    /// `injectCamera`.
    pub const INJECT_CAMERA: u32 = 1;
    /// `getCameraVendorTagDescriptor`.
    pub const GET_CAMERA_VENDOR_TAG_DESCRIPTOR: u32 = 2;
    /// `getCameraCharacteristics`.
    pub const GET_CAMERA_CHARACTERISTICS: u32 = 3;
    /// `getCameraInfo`.
    pub const GET_CAMERA_INFO: u32 = 4;
    /// `connectDevice`.
    pub const CONNECT_DEVICE: u32 = 5;
    /// `getNumberOfCameras`.
    pub const GET_NUMBER_OF_CAMERAS: u32 = 6;
    /// `notifyDeviceStateChange`.
    pub const NOTIFY_DEVICE_STATE_CHANGE: u32 = 7;
    /// `setTorchMode`.
    pub const SET_TORCH_MODE: u32 = 8;
    /// `getCameraVendorTagCache`.
    pub const GET_CAMERA_VENDOR_TAG_CACHE: u32 = 9;
    /// `getLegacyParameters`.
    pub const GET_LEGACY_PARAMETERS: u32 = 10;
    /// `getConcurrentCameraIds`.
    pub const GET_CONCURRENT_CAMERA_IDS: u32 = 11;
    /// `getTorchStrengthLevel`.
    pub const GET_TORCH_STRENGTH_LEVEL: u32 = 12;
    /// `addListener` (`API_VERSION_1`).
    pub const ADD_LISTENER: u32 = 13;
    /// `notifyDisplayConfigurationChange`.
    pub const NOTIFY_DISPLAY_CONFIGURATION_CHANGE: u32 = 14;
    /// `isHiddenPhysicalCamera`.
    pub const IS_HIDDEN_PHYSICAL_CAMERA: u32 = 15;
    /// `turnOnTorchWithStrengthLevel`.
    pub const TURN_ON_TORCH_WITH_STRENGTH_LEVEL: u32 = 16;
    /// API-1 `connect` (`API_VERSION_2`).
    pub const CONNECT: u32 = 17;
    /// `isConcurrentSessionConfigurationSupported`.
    pub const IS_CONCURRENT_SESSION_CONFIGURATION_SUPPORTED: u32 = 18;
    /// `reportExtensionSessionStats`.
    pub const REPORT_EXTENSION_SESSION_STATS: u32 = 19;
    /// `removeListener`.
    pub const REMOVE_LISTENER: u32 = 20;
    /// `notifySystemEvent`.
    pub const NOTIFY_SYSTEM_EVENT: u32 = 21;
    /// `supportsCameraApi`.
    pub const SUPPORTS_CAMERA_API: u32 = 22;
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
}

impl Default for StubCamera {
    fn default() -> Self {
        Self {
            id: "0".to_string(),
            facing: FACING_BACK,
            orientation: 90,
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
        })
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

const CONTROL_OPS: [ControlOp; 4] = [
    ControlOp {
        name: "status",
        args: "none — the served camera table and the transaction log",
    },
    ControlOp {
        name: "camera",
        args: "none — the single camera's fixed description (what `getCameraInfo` answers)",
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
        version: "0.1.0",
        summary: "camera service (`media.camera`): one fixed back camera whose id list and \
                  CameraInfo answer from the config; no `real` profile (ACOS has no camera HAL), \
                  no frames yet (slice 2)",
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
/// Only the fields the stub actually answers are configurable: the id, the
/// facing and the orientation. A key it does not implement is refused
/// (`ModuleRejected`, naming the accepted keys) rather than silently ignored,
/// because an agent that set `width: 640` and got a camera that never produces
/// a frame would have been lied to.
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
        if !matches!(key.as_str(), "id" | "facing" | "orientation") {
            return Err(ServiceError::ModuleRejected {
                module: MODULE_ID,
                detail: format!(
                    "config.camera.{key} is not implemented: this stub configures `id`, \
                     `facing` and `orientation` only (frames arrive with the `scripted` profile)"
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
    Ok(camera)
}

struct CameraService {
    camera: StubCamera,
    state: Arc<Mutex<CameraState>>,
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
        // CameraStatus[1]: {cameraId, status, unavailablePhysicalCameras, clientPackage}
        reply.write_int32(1);
        reply.write_string16(&self.camera.id)?;
        reply.write_int32(STATUS_PRESENT);
        reply.write_int32(0); // String[0]: no unavailable physical cameras
        reply.write_string16_opt(None)?; // clientPackage: null (nobody opened it)
        Ok(true)
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
            // getCameraVendorTagDescriptor(): null, no vendor tags published
            code::GET_CAMERA_VENDOR_TAG_DESCRIPTOR => {
                self.record(
                    code::GET_CAMERA_VENDOR_TAG_DESCRIPTOR,
                    "getCameraVendorTagDescriptor",
                    None,
                );
                write_no_exception(reply);
                reply.write_int32(0);
                Ok(true)
            }
            // getCameraVendorTagCache(): null
            code::GET_CAMERA_VENDOR_TAG_CACHE => {
                self.record(
                    code::GET_CAMERA_VENDOR_TAG_CACHE,
                    "getCameraVendorTagCache",
                    None,
                );
                write_no_exception(reply);
                reply.write_int32(0);
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
            // getCameraCharacteristics(String, int, boolean): slice 3.
            code::GET_CAMERA_CHARACTERISTICS => {
                let camera_id = data.read_string16_lenient();
                Err(self.unsupported(
                    code::GET_CAMERA_CHARACTERISTICS,
                    &format!(
                        "getCameraCharacteristics({}) — the reply is \
                         `CameraMetadataNative`, whose readFromParcel is native \
                         (libcamera_client.so) and this guest ships no camera client library; \
                         see docs/CAMERA_PLAN.md slice 3",
                        camera_id.unwrap_or_else(|| "<null>".to_string())
                    ),
                ))
            }
            // connectDevice(...): slice 4 (gralloc buffers + ImageReader).
            code::CONNECT_DEVICE => Err(self.unsupported(
                code::CONNECT_DEVICE,
                "connectDevice — a device binder would have to configure streams and deliver \
                 frames, which needs a gralloc-backed ImageReader surface; see \
                 docs/CAMERA_PLAN.md slice 4",
            )),
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
                     docs/CAMERA_PLAN.md slice 2; use `stub`"
                ),
            });
        }
        self.camera = camera_from_config(config)?;
        let state = Arc::new(Mutex::new(CameraState::default()));
        host.register_dynamic_named(
            SERVICE_NAME,
            DESCRIPTOR,
            Box::new(CameraService {
                camera: self.camera.clone(),
                state: Arc::clone(&state),
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
                 getCameraCharacteristics/connectDevice answer the structured \
                 unsupported error until slices 3-4; {listeners} listener(s), \
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
                "controls": ["status", "camera", "calls", "reset"],
            }));
        }
        report
    }
}
