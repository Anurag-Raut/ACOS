//! `hw.camera` — the Camera2 service seam (`off`, `stub`).
//!
//! A phone app reaches a camera through `CameraManager`, and `CameraManager`
//! reaches the platform through **one** binder: `ServiceManager.getService(
//! "media.camera")` -> `android.hardware.ICameraService`
//! (`base/core/java/android/hardware/camera2/CameraManager.java`,
//! `CameraManagerGlobal.connectCameraServiceLocked()`). Everything else — the
//! `ICameraDeviceUser`, the capture session, the streams, and the callbacks —
//! hangs off the binders that service hands back.
//!
//! # Profiles & Owner Decisions
//!
//! * `off` (Owner Default "Z") — [`CameraModule::off_node`] serves `media.camera`
//!   so `CameraManager.getCameraIdList()` returns `[]` like a camera-less device,
//!   while `openCamera` / `connectDevice` / `getCameraCharacteristics` answer
//!   structured `UNSUPPORTED_ANDROID_API` naming `hw.camera` (`profile: "stub"`).
//!   `PackageManager.hasSystemFeature(FEATURE_CAMERA*)` answers `false`.
//! * `stub` (Slice 2, ADR Camera Frames Option A) — one fixed back camera, real
//!   validated `CameraCharacteristics`, `ICameraService.connectDevice` -> live
//!   `ICameraDeviceUser` (`beginConfigure`, `createStream`, `endConfigure`,
//!   `createDefaultRequest`, `submitRequest`/`submitRequestList`, `cancelRequest`,
//!   `flush`, `waitUntilIdle`, `deleteStream`, `disconnect`), real
//!   `ICameraDeviceCallbacks` (`onCaptureStarted`, `onResultReceived` with real
//!   validated `TotalCaptureResult`, `onDeviceIdle`, `onRequestQueueEmpty`), and
//!   deterministic `ACOS_PATTERN_COLORBARS_V1` frames delivered via the app
//!   process's guest-side `acos.camera.frame_sink` helper into the app's own
//!   local `ANativeWindow` / `ImageReader` (`YUV_420_888` & `BLOB`).
//! * `scripted` — agent-supplied frames via `set_frame` (Slice 3 in
//!   `docs/CAMERA_PLAN.md`; not implemented yet).
//!
//! # Wire source
//!
//! All transaction ids and parcel layouts (`ICameraService$Stub`,
//! `ICameraDeviceUser$Stub`, `ICameraDeviceCallbacks$Stub`, `CameraInfo`,
//! `CameraStatus`, `OutputConfiguration`, `CaptureRequest`, `SubmitInfo`,
//! `CaptureResultExtras`, `Surface`) are verified directly against **this
//! image's** framework dex (`.cache/boot/android-34/dex/framework/classes.dex`)
//! and native libraries (`libgui.so`, `libcamera_client.so`,
//! `libcamera_metadata.so`).

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use acos_parcel::{BinderId, Parcel, ParcelFd};
use image::codecs::jpeg::JpegEncoder;
use image::imageops::FilterType;
use image::{ImageEncoder, RgbaImage};
use serde_json::{json, Value};

use crate::error::{Result, ServiceError};
use crate::exception::{write_no_exception, EX_SERVICE_SPECIFIC};
use crate::host::{CallResult, NodeRegistrar, PendingDelivery, ServiceHost, ServiceObject};
use crate::module::{
    attach_call, AttachMode, Budget, ControlOp, Module, ModuleConfig, ModuleManifest,
    ModuleReport, Profile, SecurityClass, ServiceProvide,
};
use crate::modules::camera_metadata;
use crate::permissions::{PermissionStore, PERMISSION_GRANTED};

/// The module id (`acos_launch {"services": [{"id": "hw.camera"}]}`).
pub const MODULE_ID: &str = "hw.camera";
/// The `ServiceManager` name (`CameraManagerGlobal.CAMERA_SERVICE`).
pub const SERVICE_NAME: &str = "media.camera";
/// The AIDL descriptor the client writes as its interface token.
pub const DESCRIPTOR: &str = "android.hardware.ICameraService";
/// The `ICameraDeviceUser` AIDL descriptor returned by `connectDevice`.
pub const DEVICE_USER_DESCRIPTOR: &str = "android.hardware.camera2.ICameraDeviceUser";
/// The `ICameraDeviceCallbacks` AIDL descriptor passed to `connectDevice`.
pub const DEVICE_CALLBACKS_DESCRIPTOR: &str = "android.hardware.camera2.ICameraDeviceCallbacks";
/// Guest-side Option A frame-sink service name registered by `LaunchMain`.
pub const FRAME_SINK_SERVICE: &str = "acos.camera.frame_sink";
/// Guest-side Option A frame-sink binder interface descriptor.
pub const FRAME_SINK_DESCRIPTOR: &str = "dev.acos.camera.ICameraFrameSink";

/// `IGraphicBufferProducer::exportToParcel` magic (`"bque"` = `USE_BUFFER_QUEUE`)
/// verified in this image's `libgui.so`.
pub const SURFACE_PARCEL_MAGIC_BQUE: u32 = 0x6271_7565;

/// `ICameraDeviceUser.NO_IN_FLIGHT_REPEATING_FRAMES`.
pub const NO_IN_FLIGHT_REPEATING_FRAMES: i64 = -1;

/// Default frame interval for repeating requests (30 fps = 33.333 ms).
pub const FRAME_DURATION_NS: i64 = 33_333_333;
/// Initial deterministic sensor timestamp (1 s).
pub const INITIAL_TIMESTAMP_NS: i64 = 1_000_000_000;

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
/// `ICameraService.ERROR_INVALID_OPERATION` (verified from `framework/classes.dex`).
pub const ERROR_INVALID_OPERATION: i32 = 10;

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

/// Transaction ids from `Landroid/hardware/camera2/ICameraDeviceUser$Stub;`
/// verified against `.cache/boot/android-34/dex/framework/classes.dex`.
pub mod device_code {
    /// `disconnect() -> void`.
    pub const DISCONNECT: u32 = 1;
    /// `submitRequest(CaptureRequest, boolean streaming) -> SubmitInfo`.
    pub const SUBMIT_REQUEST: u32 = 2;
    /// `submitRequestList(CaptureRequest[], boolean streaming) -> SubmitInfo`.
    pub const SUBMIT_REQUEST_LIST: u32 = 3;
    /// `cancelRequest(int requestId) -> long`.
    pub const CANCEL_REQUEST: u32 = 4;
    /// `beginConfigure() -> void`.
    pub const BEGIN_CONFIGURE: u32 = 5;
    /// `endConfigure(int operatingMode, CameraMetadataNative sessionParams, long startTimeMs) -> int[]`.
    pub const END_CONFIGURE: u32 = 6;
    /// `isSessionConfigurationSupported(SessionConfiguration) -> boolean`.
    pub const IS_SESSION_CONFIGURATION_SUPPORTED: u32 = 7;
    /// `deleteStream(int streamId) -> void`.
    pub const DELETE_STREAM: u32 = 8;
    /// `createStream(OutputConfiguration) -> int`.
    pub const CREATE_STREAM: u32 = 9;
    /// `createInputStream(int width, int height, int format, boolean isMultiResolution) -> int`.
    pub const CREATE_INPUT_STREAM: u32 = 10;
    /// `getInputSurface(Surface) -> void`.
    pub const GET_INPUT_SURFACE: u32 = 11;
    /// `createDefaultRequest(int templateId) -> CameraMetadataNative`.
    pub const CREATE_DEFAULT_REQUEST: u32 = 12;
    /// `getCameraInfo() -> CameraMetadataNative`.
    pub const GET_CAMERA_INFO: u32 = 13;
    /// `waitUntilIdle() -> void`.
    pub const WAIT_UNTIL_IDLE: u32 = 14;
    /// `flush() -> long`.
    pub const FLUSH: u32 = 15;
    /// `prepare(int streamId) -> void`.
    pub const PREPARE: u32 = 16;
    /// `tearDown(int streamId) -> void`.
    pub const TEAR_DOWN: u32 = 17;
    /// `prepare2(int maxCount, int streamId) -> void`.
    pub const PREPARE2: u32 = 18;
    /// `updateOutputConfiguration(int streamId, OutputConfiguration) -> void`.
    pub const UPDATE_OUTPUT_CONFIGURATION: u32 = 19;
    /// `finalizeOutputConfigurations(int streamId, OutputConfiguration) -> void`.
    pub const FINALIZE_OUTPUT_CONFIGURATIONS: u32 = 20;
    /// `setCameraAudioRestriction(int mode) -> void`.
    pub const SET_CAMERA_AUDIO_RESTRICTION: u32 = 21;
    /// `getGlobalAudioRestriction() -> int`.
    pub const GET_GLOBAL_AUDIO_RESTRICTION: u32 = 22;
    /// `switchToOffline(ICameraDeviceCallbacks, int[]) -> ICameraOfflineSession`.
    pub const SWITCH_TO_OFFLINE: u32 = 23;
}

/// Transaction ids from `Landroid/hardware/camera2/ICameraDeviceCallbacks$Stub;`
/// verified against `.cache/boot/android-34/dex/framework/classes.dex`.
pub mod callback_code {
    /// `onDeviceError(int errorCode, CaptureResultExtras)`.
    pub const ON_DEVICE_ERROR: u32 = 1;
    /// `onDeviceIdle()`.
    pub const ON_DEVICE_IDLE: u32 = 2;
    /// `onCaptureStarted(CaptureResultExtras, long timestamp)`.
    pub const ON_CAPTURE_STARTED: u32 = 3;
    /// `onResultReceived(CameraMetadataNative, CaptureResultExtras, PhysicalCaptureResultInfo[])`.
    pub const ON_RESULT_RECEIVED: u32 = 4;
    /// `onPrepared(int streamId)`.
    pub const ON_PREPARED: u32 = 5;
    /// `onRepeatingRequestError(long lastFrameNumber, int repeatingRequestId)`.
    pub const ON_REPEATING_REQUEST_ERROR: u32 = 6;
    /// `onRequestQueueEmpty()`.
    pub const ON_REQUEST_QUEUE_EMPTY: u32 = 7;
}

/// Oneway transaction ids on the guest-side `acos.camera.frame_sink` helper
/// (`dev.acos.camera.ICameraFrameSink`, Option A).
pub mod sink_code {
    /// `registerStream(int streamId, Surface, int width, int height)`.
    pub const REGISTER_STREAM: u32 = 1;
    /// `deliverFrame(int streamId, int width, int height, long frameNumber, long timestampNs, int patternId)`.
    pub const DELIVER_FRAME: u32 = 2;
    /// `releaseStream(int streamId)` (`< 0` releases all streams).
    pub const RELEASE_STREAM: u32 = 3;
    /// `deliverFrameFd(int streamId, int width, int height, long frameNumber, long timestampNs, int patternId, ParcelFileDescriptor blobFd)`.
    pub const DELIVER_FRAME_FD: u32 = 4;
}

/// Magic header for `acos_cam_frame_blob_t` passed over `DELIVER_FRAME_FD` (`"ACMF"`).
pub const ACOS_FRAME_BLOB_MAGIC: u32 = 0x464D_4341;
/// Header version for `acos_cam_frame_blob_t`.
pub const ACOS_FRAME_BLOB_VERSION: u32 = 1;

/// 100% RGB color-bar colors matching [`TEST_PATTERN_BARS`] under BT.601.
pub const TEST_PATTERN_RGB: [(u8, u8, u8); 8] = [
    (255, 255, 255), // 0: White
    (255, 255, 0),   // 1: Yellow
    (0, 255, 255),   // 2: Cyan
    (0, 255, 0),     // 3: Green
    (255, 0, 255),   // 4: Magenta
    (255, 0, 0),     // 5: Red
    (0, 0, 255),     // 6: Blue
    (0, 0, 0),       // 7: Black
];

/// BT.601 color-bar `(Y, U/Cb, V/Cr)` values for `ACOS_PATTERN_COLORBARS_V1`
/// (exact mirror of `render_shim.c::fill_yuv420_colorbars`).
pub const TEST_PATTERN_BARS: [(u8, u8, u8); 8] = [
    (235, 128, 128), // 0: White
    (210, 16, 146),  // 1: Yellow
    (170, 166, 16),  // 2: Cyan
    (145, 54, 34),   // 3: Green
    (106, 202, 222), // 4: Magenta
    (81, 90, 240),   // 5: Red
    (41, 240, 110),  // 6: Blue
    (16, 128, 128),  // 7: Black
];

/// Reference IEEE CRC32 of `ACOS_PATTERN_COLORBARS_V1` over logical planar
/// `YUV_420_888` bytes (Y `W*H`, then U `(W/2)*(H/2)`, then V `(W/2)*(H/2)`).
///
/// Must match `render_shim.c::fill_yuv420_colorbars` and `camera-probe`'s
/// `java.util.zip.CRC32` readout bit-for-bit.
/// Width/height of the scripted-profile test frame (`ACOS_TEST_FRAME_QUADRANTS_V1`).
pub const TEST_FRAME_SIZE: (u32, u32) = (640, 480);

/// The scripted-profile test frame `ACOS_TEST_FRAME_QUADRANTS_V1`, generated instead of
/// committed: 640x480 RGB, quadrants red (top-left), green (top-right), blue
/// (bottom-left), yellow (bottom-right), and a white box over x 240..=399, y 180..=299.
/// `camera-scripted` scenarios and `camera_module` tests read it as PNG and JPEG.
#[must_use]
pub fn test_frame_rgb() -> image::RgbImage {
    let (w, h) = TEST_FRAME_SIZE;
    image::RgbImage::from_fn(w, h, |x, y| {
        if (240..=399).contains(&x) && (180..=299).contains(&y) {
            return image::Rgb([255, 255, 255]);
        }
        match (x >= w / 2, y >= h / 2) {
            (false, false) => image::Rgb([255, 0, 0]),
            (true, false) => image::Rgb([0, 255, 0]),
            (false, true) => image::Rgb([0, 0, 255]),
            (true, true) => image::Rgb([255, 255, 0]),
        }
    })
}

/// [`test_frame_rgb`] as PNG bytes (lossless).
pub fn test_frame_png_bytes() -> std::result::Result<Vec<u8>, image::ImageError> {
    let img = test_frame_rgb();
    let mut out = Vec::new();
    image::codecs::png::PngEncoder::new(&mut out).write_image(
        img.as_raw(),
        img.width(),
        img.height(),
        image::ExtendedColorType::Rgb8,
    )?;
    Ok(out)
}

/// [`test_frame_rgb`] as baseline JPEG bytes, quality 95, from the `image` crate pinned in
/// `Cargo.lock` (so the bytes, and the decoded `crc32_yuv`, are reproducible).
pub fn test_frame_jpeg_bytes() -> std::result::Result<Vec<u8>, image::ImageError> {
    let img = test_frame_rgb();
    let mut out = Vec::new();
    JpegEncoder::new_with_quality(&mut out, 95).write_image(
        img.as_raw(),
        img.width(),
        img.height(),
        image::ExtendedColorType::Rgb8,
    )?;
    Ok(out)
}

/// Write `test-frame.png` and `test-frame.jpg` into `dir` (created if missing). Used by
/// `tools/build/build-apps.sh` (into `fixtures/build/camera/`) via the
/// `gen_camera_frames` example.
pub fn write_test_frames(dir: &Path) -> std::io::Result<()> {
    let io = |e: image::ImageError| std::io::Error::other(e.to_string());
    std::fs::create_dir_all(dir)?;
    std::fs::write(dir.join("test-frame.png"), test_frame_png_bytes().map_err(io)?)?;
    std::fs::write(dir.join("test-frame.jpg"), test_frame_jpeg_bytes().map_err(io)?)?;
    Ok(())
}

/// Generate exact planar `I420` (`YUV_420_888`) bytes for `ACOS_PATTERN_COLORBARS_V1`
/// (exact mirror of `render_shim.c::fill_yuv420_colorbars`).
#[must_use]
pub fn test_pattern_yuv420_bytes(width: u32, height: u32, frame_number: u64) -> Vec<u8> {
    let cw = width.div_ceil(2);
    let ch = height.div_ceil(2);
    let y_len = (width as usize) * (height as usize);
    let c_len = (cw as usize) * (ch as usize);
    let mut out = vec![0u8; y_len + 2 * c_len];
    let ramp_y0 = (height * 3) / 4;
    for y in 0..height {
        for x in 0..width {
            let val = if y == 0 && x < 4 {
                match x {
                    0 => (0xA0u8) | ((frame_number & 0x0F) as u8),
                    1 => ((frame_number >> 4) & 0xFF) as u8,
                    2 => ((frame_number >> 12) & 0xFF) as u8,
                    _ => 0x5Au8,
                }
            } else if y >= ramp_y0 {
                if width > 1 {
                    ((x * 255) / (width - 1)) as u8
                } else {
                    128
                }
            } else {
                let bar = ((x * 8) / width).min(7) as usize;
                TEST_PATTERN_BARS[bar].0
            };
            out[(y * width + x) as usize] = val;
        }
    }
    let u_off = y_len;
    let v_off = y_len + c_len;
    let ramp_cy0 = ramp_y0 / 2;
    for cy in 0..ch {
        for cx in 0..cw {
            let (u, v) = if cy >= ramp_cy0 {
                (128, 128)
            } else {
                let bar = (((cx * 2) * 8) / width).min(7) as usize;
                (TEST_PATTERN_BARS[bar].1, TEST_PATTERN_BARS[bar].2)
            };
            let idx = (cy * cw + cx) as usize;
            out[u_off + idx] = u;
            out[v_off + idx] = v;
        }
    }
    out
}

#[must_use]
pub fn test_pattern_yuv420_crc32(width: u32, height: u32, frame_number: u64) -> u32 {
    ieee_crc32(&test_pattern_yuv420_bytes(width, height, frame_number))
}

/// IEEE CRC32 over an arbitrary byte slice.
#[must_use]
pub fn ieee_crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &byte in bytes {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            if (crc & 1) != 0 {
                crc = (crc >> 1) ^ 0xEDB8_8320;
            } else {
                crc >>= 1;
            }
        }
    }
    !crc
}

fn rgb_to_y(r: u8, g: u8, b: u8) -> u8 {
    let r = i32::from(r);
    let g = i32::from(g);
    let b = i32::from(b);
    (((66 * r + 129 * g + 25 * b + 128) >> 8) + 16).clamp(16, 235) as u8
}

fn rgb_to_cb(r: u8, g: u8, b: u8) -> u8 {
    let r = i32::from(r);
    let g = i32::from(g);
    let b = i32::from(b);
    (((-38 * r - 74 * g + 112 * b + 128) >> 8) + 128).clamp(16, 240) as u8
}

fn rgb_to_cr(r: u8, g: u8, b: u8) -> u8 {
    let r = i32::from(r);
    let g = i32::from(g);
    let b = i32::from(b);
    (((112 * r - 94 * g - 18 * b + 128) >> 8) + 128).clamp(16, 240) as u8
}

/// Convert an `RgbaImage` to tightly-packed planar `I420` (`Y`, then `U/Cb`,
/// then `V/Cr`) using BT.601 studio-swing coefficients.
#[must_use]
pub fn rgba_to_yuv420_i420(rgba: &RgbaImage) -> Vec<u8> {
    let (w, h) = rgba.dimensions();
    let cw = w.div_ceil(2);
    let ch = h.div_ceil(2);
    let y_len = (w as usize) * (h as usize);
    let c_len = (cw as usize) * (ch as usize);
    let mut out = vec![0u8; y_len + 2 * c_len];
    let raw = rgba.as_raw();

    for y in 0..h {
        for x in 0..w {
            let idx = ((y * w + x) * 4) as usize;
            out[(y * w + x) as usize] = rgb_to_y(raw[idx], raw[idx + 1], raw[idx + 2]);
        }
    }
    let u_off = y_len;
    let v_off = y_len + c_len;
    for cy in 0..ch {
        let y0 = cy * 2;
        let y1 = (y0 + 1).min(h.saturating_sub(1));
        for cx in 0..cw {
            let x0 = cx * 2;
            let x1 = (x0 + 1).min(w.saturating_sub(1));
            let coords = [(x0, y0), (x1, y0), (x0, y1), (x1, y1)];
            let (mut sr, mut sg, mut sb) = (0u32, 0u32, 0u32);
            for (px, py) in coords {
                let idx = ((py * w + px) * 4) as usize;
                sr += u32::from(raw[idx]);
                sg += u32::from(raw[idx + 1]);
                sb += u32::from(raw[idx + 2]);
            }
            let r = ((sr + 2) / 4) as u8;
            let g = ((sg + 2) / 4) as u8;
            let b = ((sb + 2) / 4) as u8;
            let cidx = (cy * cw + cx) as usize;
            out[u_off + cidx] = rgb_to_cb(r, g, b);
            out[v_off + cidx] = rgb_to_cr(r, g, b);
        }
    }
    out
}

/// Generate an `RgbaImage` of `ACOS_PATTERN_COLORBARS_V1` for `(width, height)`
/// and `frame_number` (used for BLOB/JPEG capture in `TestPattern` mode).
#[must_use]
pub fn test_pattern_rgba(width: u32, height: u32, frame_number: u64) -> RgbaImage {
    let mut buf = vec![0u8; (width as usize) * (height as usize) * 4];
    let ramp_y0 = (height * 3) / 4;
    for y in 0..height {
        for x in 0..width {
            let (r, g, b) = if y == 0 && x < 4 {
                let tag = match x {
                    0 => (0xA0u8) | ((frame_number & 0x0F) as u8),
                    1 => ((frame_number >> 4) & 0xFF) as u8,
                    2 => ((frame_number >> 12) & 0xFF) as u8,
                    _ => 0x5Au8,
                };
                (tag, tag, tag)
            } else if y >= ramp_y0 {
                let l = if width > 1 {
                    ((x * 255) / (width - 1)) as u8
                } else {
                    128
                };
                (l, l, l)
            } else {
                let bar = ((x * 8) / width).min(7) as usize;
                TEST_PATTERN_RGB[bar]
            };
            let idx = ((y * width + x) * 4) as usize;
            buf[idx] = r;
            buf[idx + 1] = g;
            buf[idx + 2] = b;
            buf[idx + 3] = 255;
        }
    }
    RgbaImage::from_raw(width, height, buf).expect("test_pattern_rgba buffer size")
}

fn encode_jpeg_from_rgba(rgba: &RgbaImage, quality: u8) -> Result<Vec<u8>> {
    let (w, h) = rgba.dimensions();
    let raw = rgba.as_raw();
    let mut rgb = Vec::with_capacity((w as usize) * (h as usize) * 3);
    for px in raw.chunks_exact(4) {
        rgb.push(px[0]);
        rgb.push(px[1]);
        rgb.push(px[2]);
    }
    let mut jpeg = Vec::new();
    let encoder = JpegEncoder::new_with_quality(&mut jpeg, quality);
    encoder
        .write_image(&rgb, w, h, image::ExtendedColorType::Rgb8)
        .map_err(|e| ServiceError::ModuleRejected {
            module: MODULE_ID,
            detail: format!("failed to encode JPEG frame: {e}"),
        })?;
    Ok(jpeg)
}

fn build_frame_blob_memfd(
    rgba: &RgbaImage,
    yuv: &[u8],
    jpeg: &[u8],
    crc32_yuv: u32,
) -> Result<ParcelFd> {
    let (w, h) = rgba.dimensions();
    let rgba_bytes = rgba.as_raw();
    let header_len = 48u32;
    let rgba_off = header_len;
    let rgba_len = rgba_bytes.len() as u32;
    let yuv_off = rgba_off + rgba_len;
    let yuv_len = yuv.len() as u32;
    let jpeg_off = yuv_off + yuv_len;
    let jpeg_len = jpeg.len() as u32;
    let total = u64::from(jpeg_off) + u64::from(jpeg_len);

    let memfd = ParcelFd::memfd_sized("acos-cam-frame", total).map_err(|e| {
        ServiceError::ModuleRejected {
            module: MODULE_ID,
            detail: format!("memfd_sized failed: {e}"),
        }
    })?;
    let dup = memfd.try_clone().map_err(|e| ServiceError::ModuleRejected {
        module: MODULE_ID,
        detail: format!("memfd dup failed: {e}"),
    })?;
    let mut file = std::fs::File::from(dup.into_owned());
    let mut hdr = [0u8; 48];
    let fields: [u32; 12] = [
        ACOS_FRAME_BLOB_MAGIC,
        ACOS_FRAME_BLOB_VERSION,
        w,
        h,
        0,
        rgba_off,
        rgba_len,
        yuv_off,
        yuv_len,
        jpeg_off,
        jpeg_len,
        crc32_yuv,
    ];
    for (i, word) in fields.iter().enumerate() {
        hdr[i * 4..i * 4 + 4].copy_from_slice(&word.to_le_bytes());
    }
    file.seek(SeekFrom::Start(0))
        .and_then(|_| file.write_all(&hdr))
        .and_then(|_| file.write_all(rgba_bytes))
        .and_then(|_| file.write_all(yuv))
        .and_then(|_| file.write_all(jpeg))
        .and_then(|_| file.flush())
        .map_err(|e| ServiceError::ModuleRejected {
            module: MODULE_ID,
            detail: format!("writing frame memfd failed: {e}"),
        })?;
    Ok(memfd)
}

fn decode_base64(input: &str) -> std::result::Result<Vec<u8>, String> {
    let trimmed = input.trim();
    let payload = if let Some(rest) = trimmed.strip_prefix("data:") {
        if let Some((_, b64)) = rest.split_once(";base64,") {
            b64
        } else {
            return Err("data URI is missing `;base64,`".to_string());
        }
    } else {
        trimmed
    };
    let clean: Vec<u8> = payload
        .bytes()
        .filter(|b| !b.is_ascii_whitespace())
        .collect();
    if clean.is_empty() || clean.len() % 4 != 0 {
        return Err("base64 length must be a non-zero multiple of 4".to_string());
    }
    let mut out = Vec::with_capacity((clean.len() / 4) * 3);
    for (chunk_idx, chunk) in clean.chunks_exact(4).enumerate() {
        let mut vals = [0u8; 4];
        let mut pad = 0usize;
        for (i, &b) in chunk.iter().enumerate() {
            if b == b'=' {
                if chunk_idx * 4 + i < clean.len() - 2 || (i == 2 && chunk[3] != b'=') {
                    return Err("invalid base64 padding".to_string());
                }
                pad += 1;
                vals[i] = 0;
            } else {
                if pad > 0 {
                    return Err("invalid base64 data after padding".to_string());
                }
                vals[i] = match b {
                    b'A'..=b'Z' => b - b'A',
                    b'a'..=b'z' => b - b'a' + 26,
                    b'0'..=b'9' => b - b'0' + 52,
                    b'+' => 62,
                    b'/' => 63,
                    _ => return Err(format!("invalid base64 byte 0x{b:02x}")),
                };
            }
        }
        out.push((vals[0] << 2) | (vals[1] >> 4));
        if pad < 2 {
            out.push((vals[1] << 4) | (vals[2] >> 2));
        }
        if pad < 1 {
            out.push((vals[2] << 6) | vals[3]);
        }
    }
    Ok(out)
}

fn resolve_frame_path(raw: &str) -> std::result::Result<PathBuf, String> {
    let direct = PathBuf::from(raw);
    if direct.is_file() {
        return Ok(direct);
    }
    if direct.is_relative() {
        if let Some(root) = std::env::var_os("ACOS_ROOT") {
            let cand = PathBuf::from(root).join(&direct);
            if cand.is_file() {
                return Ok(cand);
            }
        }
        let manifest_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").join(&direct);
        if manifest_root.is_file() {
            return Ok(manifest_root);
        }
    }
    Err(format!("frame image path not found: {raw:?}"))
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

/// Active frame source served by `hw.camera`.
#[derive(Clone, Debug, Default)]
pub enum ActiveFrame {
    /// Deterministic `ACOS_PATTERN_COLORBARS_V1` test pattern.
    #[default]
    TestPattern,
    /// Decoded PNG/JPEG image injected via `set_frame` (`scripted` profile).
    Image {
        /// Human-readable label (`path:<...>` or `base64`).
        label: String,
        /// Decoded RGBA8888 pixels at the source image's dimensions.
        rgba: Arc<RgbaImage>,
        /// Monotonic generation bumped on every `set_frame`.
        generation: u64,
    },
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
    /// Total `connectDevice` sessions opened.
    pub devices_opened: u32,
    /// Currently open `ICameraDeviceUser` sessions (decremented on `disconnect`).
    pub open_devices: u32,
    /// Total `createStream` calls succeeded.
    pub streams_created: u32,
    /// Total frames delivered (single-shot + repeating).
    pub frames_delivered: u64,
    /// Active frame source (`TestPattern` by default, or `Image` via `set_frame`).
    pub active_frame: ActiveFrame,
    /// Monotonic generation bumped on every `set_frame`.
    pub frame_generation: u64,
    /// Cached `acos_cam_frame_blob_t` memfds keyed by `(generation, frame_tag, width, height)`.
    frame_memfds: BTreeMap<(u64, i64, u32, u32), Arc<ParcelFd>>,
}

impl CameraState {
    fn pattern_name(&self) -> &'static str {
        match self.active_frame {
            ActiveFrame::TestPattern => "ACOS_PATTERN_COLORBARS_V1",
            ActiveFrame::Image { .. } => "ACOS_SCRIPTED_IMAGE",
        }
    }

    fn active_frame_json(&self) -> Value {
        match &self.active_frame {
            ActiveFrame::TestPattern => json!({
                "source": "test_pattern",
                "pattern": "ACOS_PATTERN_COLORBARS_V1",
                "generation": self.frame_generation,
            }),
            ActiveFrame::Image {
                label,
                rgba,
                generation,
            } => json!({
                "source": "image",
                "label": label,
                "width": rgba.width(),
                "height": rgba.height(),
                "generation": generation,
            }),
        }
    }

    fn get_or_build_memfd(
        &mut self,
        width: u32,
        height: u32,
        frame_number: i64,
    ) -> Result<(Arc<ParcelFd>, i32)> {
        let (gen, tag_key, pattern_id) = match &self.active_frame {
            ActiveFrame::TestPattern => (0u64, frame_number, 1i32),
            ActiveFrame::Image { generation, .. } => (*generation, 0i64, 2i32),
        };
        let key = (gen, tag_key, width, height);
        if let Some(fd) = self.frame_memfds.get(&key) {
            return Ok((Arc::clone(fd), pattern_id));
        }
        let (owned_rgba, yuv): (Cow<'_, RgbaImage>, Vec<u8>) = match &self.active_frame {
            ActiveFrame::TestPattern => (
                Cow::Owned(test_pattern_rgba(width, height, frame_number as u64)),
                test_pattern_yuv420_bytes(width, height, frame_number as u64),
            ),
            ActiveFrame::Image { rgba, .. } => {
                let r = if rgba.dimensions() == (width, height) {
                    Cow::Borrowed(rgba.as_ref())
                } else {
                    Cow::Owned(image::imageops::resize(
                        rgba.as_ref(),
                        width,
                        height,
                        FilterType::Triangle,
                    ))
                };
                let y = rgba_to_yuv420_i420(&r);
                (r, y)
            }
        };
        let crc32_yuv = ieee_crc32(&yuv);
        let jpeg = encode_jpeg_from_rgba(&owned_rgba, 95)?;
        let memfd = Arc::new(build_frame_blob_memfd(&owned_rgba, &yuv, &jpeg, crc32_yuv)?);
        if self.frame_memfds.len() >= 8 {
            if let Some(first_key) = self.frame_memfds.keys().next().copied() {
                self.frame_memfds.remove(&first_key);
            }
        }
        self.frame_memfds.insert(key, Arc::clone(&memfd));
        Ok((memfd, pattern_id))
    }
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
            "devices_opened": self.devices_opened,
            "open_devices": self.open_devices,
            "streams_created": self.streams_created,
            "frames_delivered": self.frames_delivered,
        })
    }
}

const CONTROL_OPS: [ControlOp; 6] = [
    ControlOp {
        name: "status",
        args: "none — the served camera table, active frame source, open devices/frames, and the transaction log",
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
        name: "set_frame",
        args: "{\"path\": \"...\"} | {\"base64\": \"...\"} | {\"source\": \"test_pattern\"} — \
               inject a PNG/JPEG frame or restore `ACOS_PATTERN_COLORBARS_V1` (`scripted` profile)",
    },
    ControlOp {
        name: "reset",
        args: "none — clear the transaction log and counters",
    },
];

fn manifest_ref() -> &'static ModuleManifest {
    static MANIFEST: ModuleManifest = ModuleManifest {
        id: MODULE_ID,
        version: "0.4.0",
        summary: "camera service (`media.camera`): one camera (id, facing, orientation, sensor \
                  size from the config) with real CameraCharacteristics, Camera2 device/session \
                  lifecycle (`ICameraDeviceUser` + `ICameraDeviceCallbacks`), deterministic \
                  `ACOS_PATTERN_COLORBARS_V1` frames (`stub`) and host-injected PNG/JPEG frames via \
                  `set_frame` (`scripted`) into `ImageReader`/`Surface` (`YUV_420_888`, `BLOB`, \
                  `RGBA_8888`); no `real` profile (ACOS has no camera HAL)",
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
        profiles: &[Profile::Stub, Profile::Scripted],
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
                     `facing`, `orientation`, `width` and `height` only (agent-supplied frames \
                     arrive with the `scripted` profile)"
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

/// `media.camera` when `hw.camera` is `off` (Owner Default "Z", 2026-11-21).
///
/// Behaves like a camera-less Android device for discovery (`getCameraIdList()`
/// returns `[]`, `getNumberOfCameras()` returns `0`) while refusing any attempt
/// to open or query a camera (`connectDevice`, `connect`,
/// `getCameraCharacteristics`, `getCameraInfo`, torch) with structured
/// `ModuleUnavailable` naming `hw.camera` and `profile: "stub"`.
struct CameraOffService;

impl CameraOffService {
    fn unavailable() -> ServiceError {
        ServiceError::ModuleUnavailable {
            service: SERVICE_NAME.to_string(),
            interface: DESCRIPTOR.to_string(),
            module: MODULE_ID,
            attach: attach_call(MODULE_ID, Profile::Stub),
        }
    }
}

impl ServiceObject for CameraOffService {
    fn call(
        &mut self,
        _interface: &str,
        _method: &acos_aidl::MethodInfo,
        _args: &[crate::value::Value],
    ) -> Result<CallResult> {
        Err(Self::unavailable())
    }

    fn raw_transact(
        &mut self,
        interface: &str,
        code: u32,
        data: &mut Parcel,
        reply: &mut Parcel,
    ) -> Result<bool> {
        if interface != DESCRIPTOR {
            return Ok(false);
        }
        match code {
            code::GET_NUMBER_OF_CAMERAS => {
                let _kind = data.read_int32().ok();
                write_no_exception(reply);
                reply.write_int32(0);
                Ok(true)
            }
            code::ADD_LISTENER => {
                let _listener = data.read_strong_binder().ok();
                write_no_exception(reply);
                // CameraStatus[0]: no cameras present.
                reply.write_int32(0);
                Ok(true)
            }
            code::REMOVE_LISTENER
            | code::NOTIFY_SYSTEM_EVENT
            | code::NOTIFY_DISPLAY_CONFIGURATION_CHANGE
            | code::NOTIFY_DEVICE_STATE_CHANGE => {
                write_no_exception(reply);
                Ok(true)
            }
            code::GET_CONCURRENT_CAMERA_IDS => {
                write_no_exception(reply);
                reply.write_int32(0);
                Ok(true)
            }
            code::IS_CONCURRENT_SESSION_CONFIGURATION_SUPPORTED
            | code::SUPPORTS_CAMERA_API
            | code::IS_HIDDEN_PHYSICAL_CAMERA => {
                write_no_exception(reply);
                reply.write_int32(0);
                Ok(true)
            }
            code::GET_CAMERA_VENDOR_TAG_DESCRIPTOR | code::GET_CAMERA_VENDOR_TAG_CACHE => {
                write_no_exception(reply);
                reply.write_int32(1);
                reply.write_int32(0);
                Ok(true)
            }
            code::REPORT_EXTENSION_SESSION_STATS => {
                write_no_exception(reply);
                reply.write_string16_opt(None)?;
                Ok(true)
            }
            // Opening, querying characteristics/info, or torch control while
            // `hw.camera` is off fails with the structured module-unavailable
            // error naming `hw.camera` and `profile: "stub"`.
            _ => Err(Self::unavailable()),
        }
    }
}

/// One `Surface` flattened by `android.view.Surface.writeToParcel`.
#[derive(Clone, Debug)]
struct ParsedSurface {
    name: String,
    is_single_buffered: i32,
    igbp: BinderId,
}

/// One stream registered through `ICameraDeviceUser.createStream`.
#[derive(Clone, Debug)]
struct ConfiguredStream {
    width: i32,
    height: i32,
    is_blob: bool,
    surfaces: Vec<ParsedSurface>,
}

fn parse_image_reader_dims(name: &str) -> Option<(i32, i32)> {
    let rest = name.strip_prefix("ImageReader-")?;
    let f_pos = rest.find('f')?;
    let (ws, hs) = rest[..f_pos].split_once('x')?;
    let w = ws.parse::<i32>().ok()?;
    let h = hs.parse::<i32>().ok()?;
    if w >= 16 && h >= 16 {
        Some((w, h))
    } else {
        None
    }
}

/// One `CaptureRequest` parsed from `submitRequest` / `submitRequestList`.
#[derive(Clone, Debug)]
struct ParsedCaptureRequest {
    /// Stream ids from `CaptureRequest.convertSurfaceToStreamId` (`mStreamIdxArray`).
    stream_ids: Vec<i32>,
    /// Fallback `IGraphicBufferProducer` binders if surfaces were not converted.
    surface_binders: Vec<BinderId>,
    /// `ANDROID_CONTROL_CAPTURE_INTENT` (default 1 = `PREVIEW`).
    capture_intent: u8,
}

/// Active repeating request driven by `ServiceObject::next_delivery_deadline` +
/// `advance_timed`.
struct RepeatingRequest {
    request_id: i32,
    requests: Vec<ParsedCaptureRequest>,
    last_frame_number: i64,
    next_tick: Instant,
}

/// Read one `android.view.Surface` written by `Surface.writeToParcel`.
fn read_surface_parcel(data: &mut Parcel) -> Result<ParsedSurface> {
    let name = data.read_string16_lenient().unwrap_or_default();
    let is_single_buffered = data.read_int32().unwrap_or(0);
    let magic = data.read_int32().unwrap_or(0) as u32;
    let igbp = if magic == SURFACE_PARCEL_MAGIC_BQUE {
        data.read_strong_binder()
            .ok()
            .flatten()
            .unwrap_or(BinderId::NULL)
    } else {
        BinderId::NULL
    };
    let _surface_control_handle = data.read_strong_binder().ok();
    Ok(ParsedSurface {
        name,
        is_single_buffered,
        igbp,
    })
}

/// Write one `android.view.Surface` in `Surface.CREATOR.createFromParcel` layout
/// (no leading presence int; exact match to `Surface.writeToParcel` / `Surface.cpp`).
fn write_surface_parcel(parcel: &mut Parcel, surface: &ParsedSurface) {
    let _ = parcel.write_string16(&surface.name);
    parcel.write_int32(surface.is_single_buffered);
    if !surface.igbp.is_null() {
        parcel.write_int32(SURFACE_PARCEL_MAGIC_BQUE as i32);
        parcel.write_strong_binder(surface.igbp);
    } else {
        parcel.write_int32(0);
    }
    parcel.write_strong_binder(BinderId::NULL);
}

/// Read `android.hardware.camera2.params.OutputConfiguration` written by
/// `OutputConfiguration.writeToParcel` (after the AIDL typed-object presence int).
fn read_output_configuration(data: &mut Parcel) -> Result<ConfiguredStream> {
    let _rotation = data.read_int32()?;
    let _surface_group_id = data.read_int32()?;
    let _surface_type = data.read_int32()?;
    let width = data.read_int32()?;
    let height = data.read_int32()?;
    let _is_deferred = data.read_int32()?;
    let _is_shared = data.read_int32()?;
    // `dest.writeTypedList(mSurfaces)`: int32 count, then per element
    // int32 present + Surface.writeToParcel.
    let surface_count = data.read_int32().unwrap_or(0);
    let mut surfaces = Vec::new();
    if surface_count > 0 {
        for _ in 0..(surface_count as usize).min(16) {
            let present = data.read_int32().unwrap_or(0);
            if present != 0 {
                surfaces.push(read_surface_parcel(data)?);
            }
        }
    }
    // Remaining trailing fields (`mPhysicalCameraId`, `mIsMultiResolution`,
    // `mSensorPixelModesUsed`, `mDynamicRangeProfile`, `mColorSpace`,
    // `mStreamUseCase`, `mTimestampBase`, `mMirrorMode`, `mReadoutTimestampEnabled`)
    // are optional to consume at the end of a single-arg `createStream` call.
    let is_blob = surfaces.iter().any(|s| {
        s.name.contains("f100m")
            || s.name.contains("f100-")
            || s.name.contains("f21m")
            || s.name.contains("f21-")
    });
    let (eff_w, eff_h) = if width > 8192 || height < 16 {
        surfaces
            .iter()
            .find_map(|s| parse_image_reader_dims(&s.name))
            .unwrap_or((width.max(16), height.max(16)))
    } else {
        (width.max(16), height.max(16))
    };
    Ok(ConfiguredStream {
        width: eff_w,
        height: eff_h,
        is_blob,
        surfaces,
    })
}

/// Extract `ANDROID_CONTROL_CAPTURE_INTENT` from a validated `camera_metadata_t` blob.
fn extract_capture_intent(blob: &[u8]) -> Option<u8> {
    let parsed = camera_metadata::validate(blob).ok()?;
    let entry = parsed
        .entries()
        .into_iter()
        .find(|e| e.tag == camera_metadata::tags::CONTROL_CAPTURE_INTENT.id)?;
    entry.value.first().copied()
}

/// Read one `android.hardware.camera2.CaptureRequest` written by
/// `CaptureRequest.writeToParcel` (after the AIDL typed-object presence int).
fn read_capture_request(data: &mut Parcel) -> Result<ParsedCaptureRequest> {
    let physical_count = data.read_int32()?.max(0) as usize;
    let mut capture_intent = 1u8; // TEMPLATE_PREVIEW
    for idx in 0..physical_count.min(16) {
        let _cam_id = data.read_string16_lenient();
        if let Ok(Some(blob)) = camera_metadata::read_parcel(data) {
            if idx == 0 {
                if let Some(intent) = extract_capture_intent(&blob) {
                    capture_intent = intent;
                }
            }
        }
    }
    let _is_reprocess = data.read_int32()?;
    // `dest.writeParcelableArray(surfaces, flags)`: int32 len, then per elem
    // `writeParcelable` -> `String16 className` (null => skip) + `Surface`.
    let surface_arr_len = data.read_int32().unwrap_or(-1);
    let mut surface_binders = Vec::new();
    if surface_arr_len > 0 {
        for _ in 0..(surface_arr_len as usize).min(16) {
            if data.read_string16_lenient().is_some() {
                if let Ok(surface) = read_surface_parcel(data) {
                    if !surface.igbp.is_null() {
                        surface_binders.push(surface.igbp);
                    }
                }
            }
        }
    }
    // `mStreamIdxArray` / `mSurfaceIdxArray` pairs (written when
    // `CameraDeviceImpl` converted surfaces to stream ids).
    let stream_pair_count = data.read_int32().unwrap_or(0);
    let mut stream_ids = Vec::new();
    if stream_pair_count > 0 {
        for _ in 0..(stream_pair_count as usize).min(32) {
            let stream_id = data.read_int32()?;
            let _surface_idx = data.read_int32()?;
            if !stream_ids.contains(&stream_id) {
                stream_ids.push(stream_id);
            }
        }
    }
    // User tag flag + optional String16.
    let has_user_tag = data.read_int32().unwrap_or(0);
    if has_user_tag != 0 {
        let _ = data.read_string16_lenient();
    }
    Ok(ParsedCaptureRequest {
        stream_ids,
        surface_binders,
        capture_intent,
    })
}

/// Write `android.hardware.camera2.impl.CaptureResultExtras` matching
/// `CaptureResultExtras.readFromParcel` in this image's framework DEX.
#[allow(clippy::too_many_arguments)]
fn write_capture_result_extras(
    parcel: &mut Parcel,
    request_id: i32,
    subsequence_id: i32,
    frame_number: i64,
    partial_result_count: i32,
    last_completed_regular: i64,
) {
    parcel.write_int32(request_id);
    parcel.write_int32(subsequence_id);
    parcel.write_int32(0); // afTriggerId
    parcel.write_int32(0); // precaptureTriggerId
    parcel.write_int64(frame_number);
    parcel.write_int32(partial_result_count);
    parcel.write_int32(-1); // errorStreamId = -1 (no error)
    parcel.write_int32(0); // errorPhysicalCameraIdPresent = false
    parcel.write_int64(last_completed_regular);
    parcel.write_int64(NO_IN_FLIGHT_REPEATING_FRAMES); // lastCompletedReprocessFrameNumber
    parcel.write_int64(NO_IN_FLIGHT_REPEATING_FRAMES); // lastCompletedZslFrameNumber
    parcel.write_int32(0); // hasReadoutTimestamp = false
}

/// Per-open-camera `android.hardware.camera2.ICameraDeviceUser` host node.
struct CameraDeviceUser {
    camera: StubCamera,
    state: Arc<Mutex<CameraState>>,
    nodes: NodeRegistrar,
    callbacks: BinderId,
    owner_tag: Option<u32>,
    next_stream_id: i32,
    streams: BTreeMap<i32, ConfiguredStream>,
    next_request_id: i32,
    next_frame_number: i64,
    last_completed_regular_frame: i64,
    repeating: Option<RepeatingRequest>,
    audio_restriction: i32,
    disconnected: bool,
    pending_deliveries: Vec<PendingDelivery>,
    pending_retained: Vec<BinderId>,
    pending_released: Vec<BinderId>,
}

impl CameraDeviceUser {
    fn new(
        camera: StubCamera,
        state: Arc<Mutex<CameraState>>,
        nodes: NodeRegistrar,
        callbacks: BinderId,
        owner_tag: Option<u32>,
    ) -> Self {
        let mut pending_retained = Vec::new();
        if !callbacks.is_null() {
            pending_retained.push(callbacks);
        }
        Self {
            camera,
            state,
            nodes,
            callbacks,
            owner_tag,
            next_stream_id: 0,
            streams: BTreeMap::new(),
            next_request_id: 1,
            next_frame_number: 0,
            last_completed_regular_frame: NO_IN_FLIGHT_REPEATING_FRAMES,
            repeating: None,
            audio_restriction: 0,
            disconnected: false,
            pending_deliveries: Vec::new(),
            pending_retained,
            pending_released: Vec::new(),
        }
    }

    fn state(&self) -> std::sync::MutexGuard<'_, CameraState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn record(&self, code: u32, method: &'static str) {
        self.state()
            .record(code, method, Some(self.camera.id.clone()));
    }

    fn frame_sink(&self) -> Option<BinderId> {
        self.nodes.guest_service(self.owner_tag, FRAME_SINK_SERVICE)
    }

    fn queue_sink_register(&mut self, stream_id: i32, stream: &ConfiguredStream) {
        let Some(sink) = self.frame_sink() else {
            return;
        };
        let Some(surface) = stream.surfaces.iter().find(|s| !s.igbp.is_null()) else {
            return;
        };
        let mut data = Parcel::new();
        let _ = data.write_interface_token(FRAME_SINK_DESCRIPTOR);
        data.write_int32(stream_id);
        write_surface_parcel(&mut data, surface);
        data.write_int32(stream.width);
        data.write_int32(stream.height);
        self.pending_deliveries.push(PendingDelivery {
            target: sink,
            interface: FRAME_SINK_DESCRIPTOR.to_string(),
            code: sink_code::REGISTER_STREAM,
            method: "registerStream".to_string(),
            data,
        });
    }

    fn queue_sink_deliver(
        &mut self,
        stream_id: i32,
        width: i32,
        height: i32,
        _is_blob: bool,
        frame_number: i64,
        timestamp_ns: i64,
    ) {
        let Some(sink) = self.frame_sink() else {
            return;
        };
        let maybe_fd = self
            .state()
            .get_or_build_memfd(width.max(16) as u32, height.max(16) as u32, frame_number)
            .ok();
        if let Some((memfd, pattern_id)) = maybe_fd {
            let mut data = Parcel::new();
            let _ = data.write_interface_token(FRAME_SINK_DESCRIPTOR);
            data.write_int32(stream_id);
            data.write_int32(width);
            data.write_int32(height);
            data.write_int64(frame_number);
            data.write_int64(timestamp_ns);
            data.write_int32(pattern_id);
            if data.write_file_descriptor(memfd.as_fd()).is_ok() {
                self.pending_deliveries.push(PendingDelivery {
                    target: sink,
                    interface: FRAME_SINK_DESCRIPTOR.to_string(),
                    code: sink_code::DELIVER_FRAME_FD,
                    method: "deliverFrameFd".to_string(),
                    data,
                });
                return;
            }
        }
        let mut data = Parcel::new();
        let _ = data.write_interface_token(FRAME_SINK_DESCRIPTOR);
        data.write_int32(stream_id);
        data.write_int32(width);
        data.write_int32(height);
        data.write_int64(frame_number);
        data.write_int64(timestamp_ns);
        data.write_int32(1); // ACOS_PATTERN_COLORBARS_V1
        self.pending_deliveries.push(PendingDelivery {
            target: sink,
            interface: FRAME_SINK_DESCRIPTOR.to_string(),
            code: sink_code::DELIVER_FRAME,
            method: "deliverFrame".to_string(),
            data,
        });
    }

    fn queue_sink_release(&mut self, stream_id: i32) {
        let Some(sink) = self.frame_sink() else {
            return;
        };
        let mut data = Parcel::new();
        let _ = data.write_interface_token(FRAME_SINK_DESCRIPTOR);
        data.write_int32(stream_id);
        self.pending_deliveries.push(PendingDelivery {
            target: sink,
            interface: FRAME_SINK_DESCRIPTOR.to_string(),
            code: sink_code::RELEASE_STREAM,
            method: "releaseStream".to_string(),
            data,
        });
    }

    fn queue_callback_simple(&mut self, code: u32, method: &str) {
        if self.callbacks.is_null() {
            return;
        }
        let mut data = Parcel::new();
        let _ = data.write_interface_token(DEVICE_CALLBACKS_DESCRIPTOR);
        self.pending_deliveries.push(PendingDelivery {
            target: self.callbacks,
            interface: DEVICE_CALLBACKS_DESCRIPTOR.to_string(),
            code,
            method: method.to_string(),
            data,
        });
    }

    fn queue_prepared(&mut self, stream_id: i32) {
        if self.callbacks.is_null() {
            return;
        }
        let mut data = Parcel::new();
        let _ = data.write_interface_token(DEVICE_CALLBACKS_DESCRIPTOR);
        data.write_int32(stream_id);
        self.pending_deliveries.push(PendingDelivery {
            target: self.callbacks,
            interface: DEVICE_CALLBACKS_DESCRIPTOR.to_string(),
            code: callback_code::ON_PREPARED,
            method: "onPrepared".to_string(),
            data,
        });
    }

    fn resolve_target_streams(&self, req: &ParsedCaptureRequest) -> Vec<(i32, i32, i32, bool)> {
        let mut targets = Vec::new();
        for &sid in &req.stream_ids {
            if let Some(s) = self.streams.get(&sid) {
                targets.push((sid, s.width, s.height, s.is_blob));
            }
        }
        if targets.is_empty() && !req.surface_binders.is_empty() {
            for (&sid, s) in &self.streams {
                if s.surfaces
                    .iter()
                    .any(|surf| req.surface_binders.contains(&surf.igbp))
                {
                    targets.push((sid, s.width, s.height, s.is_blob));
                }
            }
        }
        if targets.is_empty() && self.streams.len() == 1 {
            if let Some((&sid, s)) = self.streams.iter().next() {
                targets.push((sid, s.width, s.height, s.is_blob));
            }
        }
        targets
    }

    /// Deliver one frame: pixel buffer(s) via Option A frame-sink, followed by
    /// `onCaptureStarted` and `onResultReceived` with a validated
    /// `TotalCaptureResult` metadata blob.
    fn emit_frame(
        &mut self,
        request_id: i32,
        subsequence_id: i32,
        req: &ParsedCaptureRequest,
    ) -> i64 {
        let frame_number = self.next_frame_number;
        self.next_frame_number += 1;
        let timestamp_ns = INITIAL_TIMESTAMP_NS + frame_number * FRAME_DURATION_NS;
        let prev_completed = self.last_completed_regular_frame;

        let targets = self.resolve_target_streams(req);
        for (stream_id, width, height, is_blob) in targets {
            self.queue_sink_deliver(
                stream_id,
                width,
                height,
                is_blob,
                frame_number,
                timestamp_ns,
            );
        }

        if !self.callbacks.is_null() {
            // 1. onCaptureStarted(CaptureResultExtras, long timestamp)
            let mut started = Parcel::new();
            let _ = started.write_interface_token(DEVICE_CALLBACKS_DESCRIPTOR);
            started.write_int32(1); // typed object present
            write_capture_result_extras(
                &mut started,
                request_id,
                subsequence_id,
                frame_number,
                1, // REQUEST_PARTIAL_RESULT_COUNT = 1
                prev_completed,
            );
            started.write_int64(timestamp_ns);
            self.pending_deliveries.push(PendingDelivery {
                target: self.callbacks,
                interface: DEVICE_CALLBACKS_DESCRIPTOR.to_string(),
                code: callback_code::ON_CAPTURE_STARTED,
                method: "onCaptureStarted".to_string(),
                data: started,
            });

            // 2. onResultReceived(CameraMetadataNative, CaptureResultExtras, PhysicalCaptureResultInfo[])
            let result_blob = camera_metadata::capture_result(
                &self.camera.sensor(),
                timestamp_ns,
                frame_number,
                req.capture_intent,
            )
            .to_bytes();
            let mut result = Parcel::new();
            let _ = result.write_interface_token(DEVICE_CALLBACKS_DESCRIPTOR);
            result.write_int32(1); // CameraMetadataNative present
            camera_metadata::write_parcel(&mut result, &result_blob);
            result.write_int32(1); // CaptureResultExtras present
            write_capture_result_extras(
                &mut result,
                request_id,
                subsequence_id,
                frame_number,
                1,
                prev_completed,
            );
            result.write_int32(0); // PhysicalCaptureResultInfo[0]
            self.pending_deliveries.push(PendingDelivery {
                target: self.callbacks,
                interface: DEVICE_CALLBACKS_DESCRIPTOR.to_string(),
                code: callback_code::ON_RESULT_RECEIVED,
                method: "onResultReceived".to_string(),
                data: result,
            });
        }

        self.last_completed_regular_frame = frame_number;
        self.state().frames_delivered += 1;
        frame_number
    }

    fn handle_submit(
        &mut self,
        requests: Vec<ParsedCaptureRequest>,
        streaming: bool,
        reply: &mut Parcel,
    ) -> Result<bool> {
        let request_id = self.next_request_id;
        self.next_request_id += 1;

        if requests.is_empty() {
            reply.write_int32(EX_SERVICE_SPECIFIC);
            reply.write_string16("CaptureRequest list must not be empty")?;
            reply.write_int32(0);
            reply.write_int32(ERROR_ILLEGAL_ARGUMENT);
            return Ok(true);
        }

        let reply_last_frame = if streaming {
            let prev_last = self
                .repeating
                .take()
                .map(|r| r.last_frame_number)
                .unwrap_or(NO_IN_FLIGHT_REPEATING_FRAMES);
            let mut last = NO_IN_FLIGHT_REPEATING_FRAMES;
            for (sub, req) in requests.iter().enumerate() {
                last = self.emit_frame(request_id, sub as i32, req);
            }
            self.repeating = Some(RepeatingRequest {
                request_id,
                requests,
                last_frame_number: last,
                next_tick: Instant::now() + Duration::from_millis(33),
            });
            prev_last
        } else {
            let mut last = NO_IN_FLIGHT_REPEATING_FRAMES;
            for (sub, req) in requests.iter().enumerate() {
                last = self.emit_frame(request_id, sub as i32, req);
            }
            if self.repeating.is_none() {
                self.queue_callback_simple(callback_code::ON_REQUEST_QUEUE_EMPTY, "onRequestQueueEmpty");
                self.queue_callback_simple(callback_code::ON_DEVICE_IDLE, "onDeviceIdle");
            }
            last
        };

        // SubmitInfo: present = 1, mRequestId, mLastFrameNumber.
        write_no_exception(reply);
        reply.write_int32(1);
        reply.write_int32(request_id);
        reply.write_int64(reply_last_frame);
        Ok(true)
    }

    fn release_all_held(&mut self) -> Vec<BinderId> {
        let mut out = std::mem::take(&mut self.pending_released);
        if !self.callbacks.is_null() {
            out.push(self.callbacks);
            self.callbacks = BinderId::NULL;
        }
        for (_, stream) in std::mem::take(&mut self.streams) {
            for surface in stream.surfaces {
                if !surface.igbp.is_null() {
                    out.push(surface.igbp);
                }
            }
        }
        out
    }
}

impl ServiceObject for CameraDeviceUser {
    fn call(
        &mut self,
        interface: &str,
        method: &acos_aidl::MethodInfo,
        _args: &[crate::value::Value],
    ) -> Result<CallResult> {
        Err(ServiceError::UnsupportedAndroidApi {
            interface: interface.to_string(),
            code: 0,
            method: format!("{MODULE_ID}: {interface}::{}", method.name),
        })
    }

    fn raw_transact(
        &mut self,
        interface: &str,
        code: u32,
        data: &mut Parcel,
        reply: &mut Parcel,
    ) -> Result<bool> {
        if interface != DEVICE_USER_DESCRIPTOR {
            return Ok(false);
        }
        match code {
            device_code::DISCONNECT => {
                self.record(device_code::DISCONNECT, "device.disconnect");
                self.repeating = None;
                self.queue_sink_release(-1);
                if !self.disconnected {
                    self.disconnected = true;
                    let mut st = self.state();
                    st.open_devices = st.open_devices.saturating_sub(1);
                }
                write_no_exception(reply);
                Ok(true)
            }
            device_code::BEGIN_CONFIGURE => {
                self.record(device_code::BEGIN_CONFIGURE, "device.beginConfigure");
                write_no_exception(reply);
                Ok(true)
            }
            device_code::CREATE_STREAM => {
                self.record(device_code::CREATE_STREAM, "device.createStream");
                let present = data.read_int32()?;
                if present == 0 {
                    reply.write_int32(EX_SERVICE_SPECIFIC);
                    reply.write_string16("OutputConfiguration must not be null")?;
                    reply.write_int32(0);
                    reply.write_int32(ERROR_ILLEGAL_ARGUMENT);
                    return Ok(true);
                }
                let stream = read_output_configuration(data)?;
                let stream_id = self.next_stream_id;
                self.next_stream_id += 1;
                for surf in &stream.surfaces {
                    if !surf.igbp.is_null() {
                        self.pending_retained.push(surf.igbp);
                    }
                }
                self.queue_sink_register(stream_id, &stream);
                self.streams.insert(stream_id, stream);
                self.state().streams_created += 1;
                write_no_exception(reply);
                reply.write_int32(stream_id);
                Ok(true)
            }
            device_code::DELETE_STREAM => {
                let stream_id = data.read_int32().unwrap_or(-1);
                self.record(device_code::DELETE_STREAM, "device.deleteStream");
                if let Some(removed) = self.streams.remove(&stream_id) {
                    for surf in removed.surfaces {
                        if !surf.igbp.is_null() {
                            self.pending_released.push(surf.igbp);
                        }
                    }
                    self.queue_sink_release(stream_id);
                }
                write_no_exception(reply);
                Ok(true)
            }
            device_code::END_CONFIGURE => {
                let _operating_mode = data.read_int32().ok();
                if data.read_int32().unwrap_or(0) != 0 {
                    let _ = camera_metadata::read_parcel(data);
                }
                let _start_time_ms = data.read_int64().ok();
                self.record(device_code::END_CONFIGURE, "device.endConfigure");
                write_no_exception(reply);
                // `int[] offlineStreamIds` = empty array (length 0).
                reply.write_int32(0);
                Ok(true)
            }
            device_code::IS_SESSION_CONFIGURATION_SUPPORTED => {
                self.record(
                    device_code::IS_SESSION_CONFIGURATION_SUPPORTED,
                    "device.isSessionConfigurationSupported",
                );
                write_no_exception(reply);
                reply.write_int32(1);
                Ok(true)
            }
            device_code::CREATE_DEFAULT_REQUEST => {
                let template_id = data.read_int32().unwrap_or(1);
                self.record(
                    device_code::CREATE_DEFAULT_REQUEST,
                    "device.createDefaultRequest",
                );
                let blob =
                    camera_metadata::default_request(&self.camera.sensor(), template_id).to_bytes();
                write_no_exception(reply);
                reply.write_int32(1);
                camera_metadata::write_parcel(reply, &blob);
                Ok(true)
            }
            device_code::GET_CAMERA_INFO => {
                self.record(device_code::GET_CAMERA_INFO, "device.getCameraInfo");
                let blob = camera_metadata::characteristics(&self.camera.sensor()).to_bytes();
                write_no_exception(reply);
                reply.write_int32(1);
                camera_metadata::write_parcel(reply, &blob);
                Ok(true)
            }
            device_code::SUBMIT_REQUEST => {
                self.record(device_code::SUBMIT_REQUEST, "device.submitRequest");
                let present = data.read_int32().unwrap_or(0);
                let mut requests = Vec::new();
                if present != 0 {
                    requests.push(read_capture_request(data)?);
                }
                let streaming = data.read_int32().unwrap_or(0) != 0;
                self.handle_submit(requests, streaming, reply)
            }
            device_code::SUBMIT_REQUEST_LIST => {
                self.record(device_code::SUBMIT_REQUEST_LIST, "device.submitRequestList");
                let count = data.read_int32().unwrap_or(0);
                let mut requests = Vec::new();
                if count > 0 {
                    for _ in 0..(count as usize).min(32) {
                        let present = data.read_int32().unwrap_or(0);
                        if present != 0 {
                            requests.push(read_capture_request(data)?);
                        }
                    }
                }
                let streaming = data.read_int32().unwrap_or(0) != 0;
                self.handle_submit(requests, streaming, reply)
            }
            device_code::CANCEL_REQUEST => {
                let request_id = data.read_int32().unwrap_or(-1);
                self.record(device_code::CANCEL_REQUEST, "device.cancelRequest");
                if self
                    .repeating
                    .as_ref()
                    .is_some_and(|r| r.request_id == request_id)
                {
                    let last = self
                        .repeating
                        .take()
                        .map(|r| r.last_frame_number)
                        .unwrap_or(NO_IN_FLIGHT_REPEATING_FRAMES);
                    self.queue_callback_simple(
                        callback_code::ON_REQUEST_QUEUE_EMPTY,
                        "onRequestQueueEmpty",
                    );
                    self.queue_callback_simple(callback_code::ON_DEVICE_IDLE, "onDeviceIdle");
                    write_no_exception(reply);
                    reply.write_int64(last);
                } else {
                    reply.write_int32(EX_SERVICE_SPECIFIC);
                    reply.write_string16(&format!(
                        "No active repeating request with id {request_id}"
                    ))?;
                    reply.write_int32(0);
                    reply.write_int32(ERROR_ILLEGAL_ARGUMENT);
                }
                Ok(true)
            }
            device_code::WAIT_UNTIL_IDLE => {
                self.record(device_code::WAIT_UNTIL_IDLE, "device.waitUntilIdle");
                write_no_exception(reply);
                Ok(true)
            }
            device_code::FLUSH => {
                self.record(device_code::FLUSH, "device.flush");
                let last = self
                    .repeating
                    .take()
                    .map(|r| r.last_frame_number)
                    .unwrap_or(self.last_completed_regular_frame);
                self.queue_callback_simple(
                    callback_code::ON_REQUEST_QUEUE_EMPTY,
                    "onRequestQueueEmpty",
                );
                self.queue_callback_simple(callback_code::ON_DEVICE_IDLE, "onDeviceIdle");
                write_no_exception(reply);
                reply.write_int64(last);
                Ok(true)
            }
            device_code::PREPARE => {
                let stream_id = data.read_int32().unwrap_or(0);
                self.record(device_code::PREPARE, "device.prepare");
                self.queue_prepared(stream_id);
                write_no_exception(reply);
                Ok(true)
            }
            device_code::PREPARE2 => {
                let _max_count = data.read_int32().unwrap_or(0);
                let stream_id = data.read_int32().unwrap_or(0);
                self.record(device_code::PREPARE2, "device.prepare2");
                self.queue_prepared(stream_id);
                write_no_exception(reply);
                Ok(true)
            }
            device_code::TEAR_DOWN
            | device_code::UPDATE_OUTPUT_CONFIGURATION
            | device_code::FINALIZE_OUTPUT_CONFIGURATIONS => {
                self.record(code, "device.streamOp");
                write_no_exception(reply);
                Ok(true)
            }
            device_code::SET_CAMERA_AUDIO_RESTRICTION => {
                self.audio_restriction = data.read_int32().unwrap_or(0);
                self.record(
                    device_code::SET_CAMERA_AUDIO_RESTRICTION,
                    "device.setCameraAudioRestriction",
                );
                write_no_exception(reply);
                Ok(true)
            }
            device_code::GET_GLOBAL_AUDIO_RESTRICTION => {
                self.record(
                    device_code::GET_GLOBAL_AUDIO_RESTRICTION,
                    "device.getGlobalAudioRestriction",
                );
                write_no_exception(reply);
                reply.write_int32(self.audio_restriction);
                Ok(true)
            }
            device_code::CREATE_INPUT_STREAM
            | device_code::GET_INPUT_SURFACE
            | device_code::SWITCH_TO_OFFLINE => {
                let method = match code {
                    device_code::CREATE_INPUT_STREAM => "createInputStream (reprocessing unsupported)",
                    device_code::GET_INPUT_SURFACE => "getInputSurface (reprocessing unsupported)",
                    _ => "switchToOffline (offline session unsupported)",
                };
                self.state().unsupported.push(format!("{code}: {method}"));
                Err(ServiceError::UnsupportedAndroidApi {
                    interface: DEVICE_USER_DESCRIPTOR.to_string(),
                    code,
                    method: format!("{MODULE_ID}: {method}"),
                })
            }
            _ => Ok(false),
        }
    }

    fn finished(&self) -> bool {
        self.disconnected
    }

    fn drain_raw_deliveries(&mut self) -> Vec<PendingDelivery> {
        std::mem::take(&mut self.pending_deliveries)
    }

    fn next_delivery_deadline(&self) -> Option<Instant> {
        if self.disconnected {
            return None;
        }
        self.repeating.as_ref().map(|r| r.next_tick)
    }

    fn advance_timed(&mut self, now: Instant) {
        if self.disconnected {
            return;
        }
        let Some(due) = self.repeating.as_ref().filter(|r| now >= r.next_tick) else {
            return;
        };
        let request_id = due.request_id;
        let requests = due.requests.clone();
        let mut last = NO_IN_FLIGHT_REPEATING_FRAMES;
        for (sub, req) in requests.iter().enumerate() {
            last = self.emit_frame(request_id, sub as i32, req);
        }
        if let Some(rep) = self.repeating.as_mut() {
            rep.last_frame_number = last;
            rep.next_tick = now + Duration::from_millis(33);
        }
    }

    fn drain_retained(&mut self) -> Vec<BinderId> {
        std::mem::take(&mut self.pending_retained)
    }

    fn drain_released(&mut self) -> Vec<BinderId> {
        std::mem::take(&mut self.pending_released)
    }

    fn release_all_refs(&mut self) -> Vec<BinderId> {
        self.release_all_held()
    }
}

struct CameraService {
    camera: StubCamera,
    state: Arc<Mutex<CameraState>>,
    permissions: Option<Arc<Mutex<PermissionStore>>>,
    nodes: NodeRegistrar,
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
    fn number_of_cameras(&self, data: &mut Parcel, reply: &mut Parcel) -> Result<bool> {
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
    fn camera_info(&self, data: &mut Parcel, reply: &mut Parcel) -> Result<bool> {
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
        reply.write_int32(1);
        reply.write_int32(self.camera.facing);
        reply.write_int32(self.camera.orientation);
        Ok(true)
    }

    /// `addListener(ICameraServiceListener)` -> `CameraStatus[]`.
    fn add_listener(&self, data: &mut Parcel, reply: &mut Parcel) -> Result<bool> {
        let _listener = data.read_strong_binder().ok();
        self.state().listeners += 1;
        self.record(code::ADD_LISTENER, "addListener", None);
        write_no_exception(reply);
        reply.write_int32(1);
        reply.write_int32(1);
        reply.write_string16(&self.camera.id)?;
        reply.write_int32(STATUS_PRESENT);
        reply.write_int32(0);
        reply.write_string16("")?;
        Ok(true)
    }

    /// `getCameraCharacteristics(String cameraId, int targetSdkVersion, boolean
    /// overrideToPortrait)` -> `CameraMetadataNative`.
    fn camera_characteristics(&self, data: &mut Parcel, reply: &mut Parcel) -> Result<bool> {
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
        reply.write_int32(1);
        camera_metadata::write_parcel(reply, &blob);
        Ok(true)
    }

    fn service_specific_error(&self, reply: &mut Parcel, error_code: i32, message: &str) -> bool {
        reply.write_int32(EX_SERVICE_SPECIFIC);
        reply
            .write_string16(message)
            .expect("an in-memory string is writable");
        reply.write_int32(0);
        reply.write_int32(error_code);
        true
    }

    fn illegal_argument(&self, reply: &mut Parcel, message: &str) -> bool {
        self.service_specific_error(reply, ERROR_ILLEGAL_ARGUMENT, message)
    }

    /// `connectDevice(ICameraDeviceCallbacks, String cameraId, String opPackageName,
    /// String featureId, int clientUid, int oomScoreOffset, int targetSdkVersion,
    /// boolean overrideToPortrait) -> ICameraDeviceUser`.
    ///
    /// Enforces `android.permission.CAMERA` against the shared `PermissionStore`
    /// (`ServiceSpecificException(ERROR_PERMISSION_DENIED = 1)` -> `SecurityException`),
    /// checks the camera id (`ERROR_ILLEGAL_ARGUMENT = 3`), and registers a live
    /// [`CameraDeviceUser`] node whose frames flow through the app's Option A
    /// `acos.camera.frame_sink` helper.
    fn connect_device(&self, data: &mut Parcel, reply: &mut Parcel) -> Result<bool> {
        let callbacks = data
            .read_strong_binder()
            .ok()
            .flatten()
            .unwrap_or(BinderId::NULL);
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

        let owner_tag = self.nodes.binder_owner(callbacks);
        {
            let mut st = self.state();
            st.devices_opened += 1;
            st.open_devices += 1;
        }
        let device = CameraDeviceUser::new(
            self.camera.clone(),
            Arc::clone(&self.state),
            self.nodes.clone(),
            callbacks,
            owner_tag,
        );
        let device_binder = self.nodes.register(DEVICE_USER_DESCRIPTOR, Box::new(device));
        write_no_exception(reply);
        reply.write_strong_binder(device_binder);
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
        data: &mut Parcel,
        reply: &mut Parcel,
    ) -> Result<bool> {
        if interface != DESCRIPTOR {
            return Ok(false);
        }
        match code {
            code::GET_NUMBER_OF_CAMERAS => self.number_of_cameras(data, reply),
            code::GET_CAMERA_INFO => self.camera_info(data, reply),
            code::ADD_LISTENER => self.add_listener(data, reply),
            code::REMOVE_LISTENER => {
                let _listener = data.read_strong_binder().ok();
                self.record(code::REMOVE_LISTENER, "removeListener", None);
                write_no_exception(reply);
                Ok(true)
            }
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
            code::GET_CAMERA_VENDOR_TAG_DESCRIPTOR => {
                self.record(
                    code::GET_CAMERA_VENDOR_TAG_DESCRIPTOR,
                    "getCameraVendorTagDescriptor",
                    None,
                );
                write_no_exception(reply);
                reply.write_int32(1);
                reply.write_int32(0);
                Ok(true)
            }
            code::GET_CAMERA_VENDOR_TAG_CACHE => {
                self.record(
                    code::GET_CAMERA_VENDOR_TAG_CACHE,
                    "getCameraVendorTagCache",
                    None,
                );
                write_no_exception(reply);
                reply.write_int32(1);
                reply.write_int32(0);
                Ok(true)
            }
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
            code::GET_CONCURRENT_CAMERA_IDS => {
                self.record(code::GET_CONCURRENT_CAMERA_IDS, "getConcurrentCameraIds", None);
                write_no_exception(reply);
                reply.write_int32(0);
                Ok(true)
            }
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
            code::GET_CAMERA_CHARACTERISTICS => self.camera_characteristics(data, reply),
            code::CONNECT_DEVICE => self.connect_device(data, reply),
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
            _ => Ok(false),
        }
    }
}

impl Module for CameraModule {
    fn manifest(&self) -> &'static ModuleManifest {
        manifest_ref()
    }

    fn off_node(&self) -> Option<Box<dyn ServiceObject>> {
        Some(Box::new(CameraOffService))
    }

    fn attach(
        &mut self,
        host: &mut ServiceHost,
        profile: Profile,
        config: &ModuleConfig,
    ) -> Result<()> {
        if !matches!(profile, Profile::Stub | Profile::Scripted) {
            return Err(ServiceError::ModuleRejected {
                module: MODULE_ID,
                detail: format!(
                    "{profile} is not implemented: ACOS has no camera HAL or physical sensor; \
                     use `stub` or `scripted`"
                ),
            });
        }
        self.camera = camera_from_config(config)?;
        let state = Arc::new(Mutex::new(CameraState::default()));
        let permissions = host.permission_store();
        let nodes = host.node_registrar();
        host.register_dynamic_named(
            SERVICE_NAME,
            DESCRIPTOR,
            Box::new(CameraService {
                camera: self.camera.clone(),
                state: Arc::clone(&state),
                permissions,
                nodes,
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

    fn control(&mut self, _host: &mut ServiceHost, op: &str, args: &Value) -> Result<Value> {
        let Some(state) = self.state.as_ref() else {
            return Err(ServiceError::ModuleRejected {
                module: MODULE_ID,
                detail: format!("module is off, so {op:?} was not applied"),
            });
        };
        match op {
            "status" => {
                let st = state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                Ok(json!({
                    "cameras": [self.camera.to_json()],
                    "pattern": st.pattern_name(),
                    "active_frame": st.active_frame_json(),
                    "calls": st.to_json(),
                }))
            }
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
            "set_frame" => {
                if self.profile != Profile::Scripted {
                    return Err(ServiceError::ModuleRejected {
                        module: MODULE_ID,
                        detail: format!(
                            "set_frame requires the `scripted` profile (current profile is `{}`); \
                             attach with `scripted`",
                            self.profile
                        ),
                    });
                }
                let Value::Object(map) = args else {
                    return Err(ServiceError::ModuleRejected {
                        module: MODULE_ID,
                        detail: "set_frame args must be a JSON object with `path`, `base64`, or `source: \"test_pattern\"`".to_string(),
                    });
                };
                if map.get("source").and_then(Value::as_str) == Some("test_pattern") {
                    let mut st = state
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    st.frame_generation += 1;
                    st.active_frame = ActiveFrame::TestPattern;
                    st.frame_memfds.clear();
                    return Ok(json!({
                        "source": "test_pattern",
                        "pattern": "ACOS_PATTERN_COLORBARS_V1",
                        "generation": st.frame_generation,
                    }));
                }
                let (label, bytes) = if let Some(path_val) = map.get("path") {
                    let Some(raw_path) = path_val.as_str() else {
                        return Err(ServiceError::ModuleRejected {
                            module: MODULE_ID,
                            detail: "set_frame.path must be a string".to_string(),
                        });
                    };
                    let resolved = resolve_frame_path(raw_path).map_err(|detail| {
                        ServiceError::ModuleRejected {
                            module: MODULE_ID,
                            detail,
                        }
                    })?;
                    let bytes = std::fs::read(&resolved).map_err(|e| {
                        ServiceError::ModuleRejected {
                            module: MODULE_ID,
                            detail: format!("failed to read frame file {}: {e}", resolved.display()),
                        }
                    })?;
                    (format!("path:{raw_path}"), bytes)
                } else if let Some(b64_val) = map.get("base64") {
                    let Some(raw_b64) = b64_val.as_str() else {
                        return Err(ServiceError::ModuleRejected {
                            module: MODULE_ID,
                            detail: "set_frame.base64 must be a string".to_string(),
                        });
                    };
                    let bytes = decode_base64(raw_b64).map_err(|detail| {
                        ServiceError::ModuleRejected {
                            module: MODULE_ID,
                            detail,
                        }
                    })?;
                    ("base64".to_string(), bytes)
                } else {
                    return Err(ServiceError::ModuleRejected {
                        module: MODULE_ID,
                        detail: "set_frame requires `path`, `base64`, or `source: \"test_pattern\"`".to_string(),
                    });
                };
                let decoded = image::load_from_memory(&bytes).map_err(|e| {
                    ServiceError::ModuleRejected {
                        module: MODULE_ID,
                        detail: format!("failed to decode PNG/JPEG image ({label}): {e}"),
                    }
                })?;
                let rgba = Arc::new(decoded.to_rgba8());
                let (w, h) = rgba.dimensions();
                if w < 16 || h < 16 {
                    return Err(ServiceError::ModuleRejected {
                        module: MODULE_ID,
                        detail: format!("frame image {w}x{h} is smaller than minimum 16x16"),
                    });
                }
                let yuv = rgba_to_yuv420_i420(&rgba);
                let crc32_yuv = ieee_crc32(&yuv);
                let mut st = state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                st.frame_generation += 1;
                let generation = st.frame_generation;
                st.active_frame = ActiveFrame::Image {
                    label: label.clone(),
                    rgba,
                    generation,
                };
                st.frame_memfds.clear();
                Ok(json!({
                    "source": "image",
                    "label": label,
                    "width": w,
                    "height": h,
                    "crc32_yuv": format!("{crc32_yuv:#010x}"),
                    "generation": generation,
                }))
            }
            "reset" => {
                let mut st = state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let active_frame = st.active_frame.clone();
                let frame_generation = st.frame_generation;
                let frame_memfds = std::mem::take(&mut st.frame_memfds);
                *st = CameraState {
                    active_frame,
                    frame_generation,
                    frame_memfds,
                    ..CameraState::default()
                };
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
            let (calls, unsupported, listeners, opened, frames, pattern, active_frame) = self
                .state
                .as_ref()
                .map(|state| {
                    let state = state
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    (
                        state.calls.len(),
                        state.unsupported.len(),
                        state.listeners,
                        state.devices_opened,
                        state.frames_delivered,
                        state.pattern_name(),
                        state.active_frame_json(),
                    )
                })
                .unwrap_or((0, 0, 0, 0, 0, "ACOS_PATTERN_COLORBARS_V1", json!(null)));
            report.reason = Some(format!(
                "{} profile (no camera HAL): one fixed {} camera (id {:?}, orientation {}), \
                 {pattern} frames (Option A frame-sink); \
                 {listeners} listener(s), {opened} open(s), {frames} frame(s) delivered, \
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
                "frames": true,
                "pattern": pattern,
                "active_frame": active_frame,
                "frames_delivered": frames,
                "devices_opened": opened,
                "controls": ["status", "camera", "characteristics", "calls", "set_frame", "reset"],
            }));
        }
        report
    }
}

