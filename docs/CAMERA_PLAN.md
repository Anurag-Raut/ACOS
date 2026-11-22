# Camera2 Plan (`hw.camera`, v0.4.1+)

> Status: **Slice 1 implemented** (`hw.camera` `off` tombstone + `stub` discovery & real `CameraCharacteristics`).
> **Slice 2 stops at an architectural decision** (Section 7, Options A / B / C) for the owner before modifying `acos-binder-peer`, `gralloc_shim.c`, or introducing a guest process.

---

## 1. How Camera2 Reaches `ICameraService` (`media.camera`)

Every Android Camera2 (and CameraX) application enters the platform through one Binder name:

1. **Lazy construction (`getSystemService(Context.CAMERA_SERVICE)`)**:
   `SystemServiceRegistry` constructs `android.hardware.camera2.CameraManager` without transacting on any Binder. App startup is therefore unaffected when `hw.camera` is `off`.
2. **First query (`getCameraIdList()`, `getCameraCharacteristics(id)`, `openCamera(...)`)**:
   `CameraManagerGlobal.connectCameraServiceLocked()` (`CameraManager.java:1951`) performs:
   - `ServiceManager.getService("media.camera")` -> `android.hardware.ICameraService`.
   - `CameraMetadataNative.setupGlobalVendorTagDescriptor()` (JNI in `libandroid_runtime.so` -> `android_hardware_camera2_CameraMetadata.cpp` -> native `BpCameraService::getCameraVendorTagDescriptor` [code `10`] and `getCameraVendorTagCache` [code `11`] in `libcamera_client.so`). Both C++ calls use `Parcel::readParcelable(Parcelable*)`, which requires a **non-null** empty parcelable (`present = 1`, `count = 0`); returning `present = 0` triggers `UNEXPECTED_NULL`.
   - `ICameraService.addListener(ICameraServiceListener)` [code `5`] -> `CameraStatus[]` (`Parcel.createTypedArray`: `int32 len`, then per element `int32 present=1`, `String cameraId`, `int32 status`, `String[] unavailablePhysicalCameras`, `String clientPackage`). `CameraManagerGlobal` filters for `STATUS_PRESENT` (`1`) — `0` is `STATUS_NOT_PRESENT` and causes `getCameraIdList()` to return `[]`.
3. **Characteristics query (`getCameraCharacteristics(cameraId)`)**:
   - `ICameraService.getCameraCharacteristics(String cameraId, int targetSdkVersion, boolean overrideToPortrait)` [code `9`] returns a `CameraMetadataNative` parcelable (`int32 present=1` + `CameraMetadata::writeToParcel` framing: `int32 blobSize`, `Parcel::writeBlob`, `int32 alignmentOffset`).
   - Inside the guest, `CameraMetadataNative.readFromParcel` calls JNI into `libandroid_runtime.so` -> `libcamera_client.so` -> `libcamera_metadata.so` (`validate_camera_metadata_structure`), which parses and validates the 48-byte `camera_metadata_t` header, 16-byte entry array, inline/out-of-line data alignment, and tag types before Java `CameraCharacteristics` or `StreamConfigurationMap` ever touches it.
   - Unknown camera IDs return `ServiceSpecificException(ERROR_ILLEGAL_ARGUMENT = 3)`, which `ExceptionUtils.throwAsPublicException` converts to `IllegalArgumentException`.
4. **Device open & capture (`openCamera` -> `createCaptureSession` -> `capture` / `setRepeatingRequest`)**:
   - `CameraManager.openCameraDeviceUserAsync` first reads `getCameraCharacteristics(cameraId)`, constructs `CameraDeviceImpl` (with its local `ICameraDeviceCallbacks` binder), and calls `ICameraService.connectDevice(callbacks, cameraId, opPackageName, featureId, clientUid, oomScoreOffset, targetSdkVersion, overrideToPortrait)` [code `4`] -> returns `ICameraDeviceUser`.
   - `CameraDeviceImpl.createCaptureSession` calls `ICameraDeviceUser.beginConfigure()` -> `createStream(OutputConfiguration)` (for each target `Surface`, which parcels its guest-owned `IGraphicBufferProducer` binder) -> `endConfigure(...)`.
   - `CameraCaptureSession.capture` / `setRepeatingRequest` calls `ICameraDeviceUser.submitRequestList(CaptureRequest[], boolean streaming)` -> returns `SubmitInfo`.
   - For each captured frame, the camera service writes pixel/JPEG data into the target `Surface`'s `IGraphicBufferProducer` and delivers `ICameraDeviceCallbacks.onCaptureStarted` and `onResultReceived` (containing a `CameraMetadataNative` capture result with matching `SENSOR_TIMESTAMP`) back to `CameraDeviceImpl`.

All 22 `ICameraService$Stub` transaction codes and `ICameraService` / `ICameraServiceListener` / `CameraMetadata` / `Camera$CameraInfo` constants are verified by automated byte-level tests against `.cache/boot/android-34/dex/framework/*.dex` in [`crates/acos-services/tests/camera_module.rs`](../crates/acos-services/tests/camera_module.rs).

---

## 2. `CameraDevice` / `CaptureSession` / `ImageReader` Buffers via Gralloc (ACOS Memfd Shim)

When an app opens an `ImageReader` (e.g. `ImageReader.newInstance(640, 480, ImageFormat.YUV_420_888, 2)` or `ImageFormat.JPEG`), `SurfaceTexture`, or `SurfaceView`:

1. **Buffer ownership**: The **app process (guest)** creates a `BufferQueueCore` + `BufferQueueProducer` (`IGraphicBufferProducer`) + `BufferQueueConsumer` (`ImageReader` / `GLConsumer`) in `libgui.so`.
2. **Surface handoff**: `OutputConfiguration.writeToParcel` marshals the `Surface`'s `IGraphicBufferProducer` strong binder into `ICameraDeviceUser.createStream(...)`.
3. **Producer lifecycle**: To deliver a single frame to `ImageReader`, the camera producer must execute synchronous two-way Binder RPCs on that guest-owned `IGraphicBufferProducer`:
   - `connect(IProducerListener, NATIVE_WINDOW_API_CAMERA, false)`
   - `dequeueBuffer(width, height, format, usage)` -> returns `(slot, Fence)`
   - `requestBuffer(slot)` -> returns a `GraphicBuffer` parcelable containing `native_handle_t` with a **Binder FD (`BINDER_TYPE_FD`)** backed by ACOS's memfd allocator
   - `mmap` the memfd, write frame bytes (YUV_420_888 planes or JPEG bitstream + `camera3_jpeg_blob` trailer for `HAL_PIXEL_FORMAT_BLOB`)
   - `queueBuffer(slot, QueueBufferInput { timestamp, crop, scalingMode, transform, fence })` -> wakes `ImageReader.OnImageAvailableListener.onImageAvailable` in the guest!
4. **Two concrete ACOS gaps identified by code inspection**:
   - **Gap 1 — Gralloc formats & YCbCr lock (`crates/acos-art/shim/gralloc_shim.c:202-260`)**:
     ACOS's memfd gralloc shim currently allocates 4-bytes-per-pixel RGB buffers (`RGBA_8888`, `RGBX_8888`, `BGRA_8888`, etc.) and only implements `lock`/`lockAsync`. It does **not** yet support:
     - `HAL_PIXEL_FORMAT_YCbCr_420_888` (`0x23`) planar/semi-planar allocation (`Y` + `Cb` + `Cr` layout) and `GraphicBufferMapper::lockYCbCr` / `lockAsyncYCbCr` (called by `ImageReader_imageSetup` in `libandroid_runtime.so` for `YUV_420_888` images).
     - `HAL_PIXEL_FORMAT_BLOB` (`0x21`, 1D byte buffer sized to `width * 1` with `JPEG` payload + 8-byte `camera3_jpeg_blob` trailer `[0x00FF, jpeg_size]` at the end of the buffer, required by `ImageReader` for `ImageFormat.JPEG`).
   - **Gap 2 — Host-to-guest two-way Binder RPC (`crates/acos-binder-peer/src/peer.rs:1077-1140`)**:
     Host-to-guest delivery (`ServiceHost` outbox -> guest) is currently one-way / fire-and-forget: non-foreign `F_REP` frames discard the reply payload and treat any non-empty reply as `REVERSE_DELIVERY_EXCEPTION`. A Rust service in `acos-services` therefore cannot call synchronous two-way methods (`dequeueBuffer`, `requestBuffer`, `queueBuffer`) on a guest `IGraphicBufferProducer` or receive `SCM_RIGHTS` memfds in the reply. (By contrast, inter-process `forward_foreign` *already* supports synchronous two-way `F_REQ`/`F_REP` with `SCM_RIGHTS` FD passing between two guest processes.)

Because closing Gap 2 touches binder transport or introduces a guest helper/daemon, Slice 2 stops here and presents lettered options in Section 7.

---

## 3. The `hw.camera` Module (`off` / `stub` / `scripted`)

Module ID: `hw.camera` (`ServiceManager` name `media.camera`, descriptor `android.hardware.ICameraService`, features `android.hardware.camera` and `android.hardware.camera.any`, `AttachMode::Hot`, `SecurityClass::InProcess`).

| Profile | Status | Behaviour |
|---|---|---|
| `off` | **Implemented (Slice 1)** | `media.camera` is registered as an ADR-0029 tombstone (`AospReaction::Tombstone`). `hasSystemFeature(FEATURE_CAMERA*)` is `false`. `getSystemService(CAMERA_SERVICE)` succeeds lazily; any `CameraManager` query hits the tombstone and throws structured `UnsupportedOperationException("UNSUPPORTED_ANDROID_API: media.camera requires module hw.camera ...")`. |
| `stub` | **Slice 1 implemented** (discovery + characteristics); **frames in Slice 2** | One back camera (`id: "0"`, `facing: "back"`, `orientation: 90`, `width: 4000`, `height: 3000`, all configurable via `config.camera`). Publishes real validated `CameraCharacteristics` (`LIMITED`, `BACKWARD_COMPATIBLE`, fixed focus, no flash, `YUV_420_888` / `PRIVATE` / `JPEG`). In Slice 2+, `connectDevice` opens a virtual `ICameraDeviceUser` delivering deterministic SMPTE/checkerboard test-pattern frames (or an agent-supplied PNG/JPEG image via control op `set_frame`). |
| `scripted` | **Planned (Slice 3)** | Extends `stub` with a deterministic frame sequence (`set_sequence`: ordered list of test-pattern / PNG / JPEG frames with advance-on-capture or loop semantics, optional EXIF/timestamp control) and fault injection (`disconnect`, `camera_in_use`, `capture_failed`) for agent/CI testing. |
| `real` | **Deliberately absent** | ACOS runs headless without a physical camera HAL or V4L2 device; refusing `real` by name preserves honesty. |

### Control Operations

- **Slice 1 (implemented)**: `status`, `camera`, `characteristics`, `calls`, `reset`.
- **Slice 2/3 (planned)**:
  - `set_frame`: `{"source": "test_pattern"}` | `{"path": "/host/path/frame.png"}` | `{"base64": "...", "mime": "image/jpeg" | "image/png"}` — decodes PNG/JPEG on the host, scales/converts to the stream's configured `width × height` (`YUV_420_888`, `RGBA_8888`, or re-encoded `JPEG` + `camera3_jpeg_blob` trailer).
  - `set_sequence`: `{"frames": [...], "mode": "loop" | "hold_last"}` (for `scripted`).
  - `inject_error`: `{"error": "disconnected" | "in_use" | "capture_failed"}` (for `scripted`).

---

## 4. CameraX Compatibility

CameraX (`androidx.camera:camera-camera2` + `camera-core` + `camera-lifecycle`) sits directly on top of `CameraManager` and Camera2 without private framework hooks. Its startup and binding path checks:

1. **Enumeration (`Camera2CameraFactory`)**: Calls `CameraManager.getCameraIdList()`, then `getCameraCharacteristics(id)` for each camera, filtering for `REQUEST_AVAILABLE_CAPABILITIES` containing `BACKWARD_COMPATIBLE` (`0`).
2. **Required `CameraCharacteristics` keys** (all populated in [`camera_metadata.rs`](../crates/acos-services/src/modules/camera_metadata.rs)):
   - `INFO_SUPPORTED_HARDWARE_LEVEL` = `LIMITED` (`0`)
   - `LENS_FACING` (`BACK = 1` / `FRONT = 0`), `SENSOR_ORIENTATION` (`90`)
   - `SENSOR_INFO_PIXEL_ARRAY_SIZE`, `SENSOR_INFO_ACTIVE_ARRAY_SIZE`, `SENSOR_INFO_PHYSICAL_SIZE`, `SENSOR_INFO_TIMESTAMP_SOURCE` (`REALTIME = 1`)
   - `SCALER_AVAILABLE_STREAM_CONFIGURATIONS`, `SCALER_AVAILABLE_MIN_FRAME_DURATIONS`, `SCALER_AVAILABLE_STALL_DURATIONS` for `BLOB` (`0x21` / `JPEG`), `IMPLEMENTATION_DEFINED` (`0x22` / `PRIVATE`), and `YCbCr_420_888` (`0x23`) across standard 4:3 and 16:9 resolutions up to the sensor array size
   - `REQUEST_MAX_NUM_OUTPUT_STREAMS` = `[0 (raw), 3 (proc), 1 (stall)]`
   - `REQUEST_AVAILABLE_CAPABILITIES` = `[BACKWARD_COMPATIBLE]`
   - `REQUEST_AVAILABLE_REQUEST_KEYS`, `REQUEST_AVAILABLE_RESULT_KEYS`, `REQUEST_AVAILABLE_CHARACTERISTICS_KEYS`
   - `CONTROL_AE_*`, `CONTROL_AF_AVAILABLE_MODES = [OFF]`, `CONTROL_AWB_AVAILABLE_MODES = [AUTO]`, `FLASH_INFO_AVAILABLE = 0`, `LENS_INFO_AVAILABLE_FOCAL_LENGTHS`, `JPEG_AVAILABLE_THUMBNAIL_SIZES`
3. **Mandatory `LIMITED` stream combinations** (for Slice 2/3):
   CameraX's `SupportedSurfaceCombination` expects a `LIMITED` device to support simultaneously:
   - `Preview` (`PRIVATE` / `SurfaceTexture`) + `ImageCapture` (`JPEG` / `ImageReader`)
   - `Preview` (`PRIVATE`) + `ImageAnalysis` (`YUV_420_888` / `ImageReader`)
   - `Preview` (`PRIVATE`) + `ImageAnalysis` (`YUV_420_888`) + `ImageCapture` (`JPEG`) (requires 2 non-stalling + 1 stalling streams, which `REQUEST_MAX_NUM_OUTPUT_STREAMS = [0, 3, 1]` already declares).
4. **What works today (Slice 1)**: `ProcessCameraProvider.getInstance()` -> `Camera2CameraFactory` -> `Camera2CameraInfo` enumeration and characteristics queries work over the real AOSP `libcamera_client.so` / `libcamera_metadata.so` stack. Actual `bindToLifecycle(...)` frame capture waits for Slice 2's `connectDevice` + buffer delivery.

---

## 5. `CAMERA` Permission via `system.permissions` + Dialog

ACOS already has full platform metadata and runtime UI for `android.permission.CAMERA`:

- **Catalog (`crates/acos-services/src/permission_catalog.rs:195-204`)**:
  `android.permission.CAMERA` is registered with AOSP label (`"take pictures and videos"`), description, `protection_level = 0x1001` (`DANGEROUS | INSTANT`), and group `android.permission-group.CAMERA` (`"Camera"`).
- **Runtime grant & dialog (`system.permissions` + `com.android.permissioncontroller`)**:
  - Apps requesting `Activity.requestPermissions(new String[]{Manifest.permission.CAMERA}, reqCode)` show the existing PermissionController dialog (or use `system.permissions` pre-grant config `{"grant": ["android.permission.CAMERA"]}` / control op `grant`).
  - `CameraService` reads the **same shared `Arc<Mutex<PermissionStore>>`** (`host.permission_store()`) as `PackageManager.checkPermission`, `IPermissionManager`, and `system.notifications`.
- **Enforcement in `hw.camera`**:
  - `getCameraIdList()`, `getCameraInfo()`, and `getCameraCharacteristics()` do **not** require `CAMERA` permission (matching AOSP; camera discovery works before the user grants `CAMERA`).
  - `connectDevice(...)` checks `PermissionStore::check("android.permission.CAMERA") == PERMISSION_GRANTED` **before** opening the device. When denied, it returns `ServiceSpecificException(ERROR_PERMISSION_DENIED = 1)`, which `CameraManager` converts via `ExceptionUtils.throwAsPublicException` into `java.lang.SecurityException` — matching real Android devices.

---

## 6. Slices & Size Estimates

| Slice | Scope | Size | Status |
|---|---|---|---|
| **Slice 1** | `hw.camera` `off` tombstone (`UNSUPPORTED_ANDROID_API`) + `stub` discovery (`getNumberOfCameras`, `getCameraInfo`, `addListener`/`removeListener`, `getCameraVendorTagDescriptor`/`Cache`, `getCameraCharacteristics` real metadata blob, `supportsCameraApi`, `isHiddenPhysicalCamera`, `getConcurrentCameraIds`, `connectDevice` permission check + structured refusal), `module_kit`, `docs/MODULES.md`, byte-level DEX wire tests, `camera-probe` fixture + positive/negative ART scenarios. | **S–M** (done) | **Done on `camera2`** |
| **Slice 2** | `connectDevice` -> `ICameraDeviceUser` (`beginConfigure`, `createStream`, `endConfigure`, `createDefaultRequest`, `submitRequestList`, `cancelRequest`, `disconnect`) + `ICameraDeviceCallbacks` (`onCaptureStarted`, `onResultReceived`) + gralloc `YUV_420_888` & `BLOB` (`lockYCbCr`) + delivering test-pattern frames to an `ImageReader` (`YUV_420_888` 640x480 single-shot + repeating). | **L** (Option A implemented) | **Done on `camera3`** |
| **Slice 3** | `scripted` profile + `set_frame` control op (host PNG/JPEG path/base64 decode + scale + YUV420/JPEG encode + `test_pattern` reset), `ImageFormat.JPEG` (`BLOB`) capture + `BitmapFactory` decode check, `SurfaceView` repeating preview + `BLASTBufferQueue` screenshot verification (`assert_region`). | **M** (builds cleanly on Slice 2) | **Done on `camera3`** |
| **CAMERAX1** | Real AndroidX CameraX (`1.3.4`: `camera-core`, `camera-camera2`, `camera-lifecycle`, `camera-view`) `camerax-probe` fixture built offline via `tools/build-camerax-probe.sh`: binds `ProcessCameraProvider` with `PreviewView` (`PERFORMANCE` / `SurfaceView`), `ImageAnalysis` (`YUV_420_888` 640x480), and `ImageCapture` (JPEG 640x480 -> `BitmapFactory` decode tol<=4) against `hw.camera` on ART. | **S** (no `hw.camera` fixes needed) | **Done on `camera3`** (`scenarios/camerax-probe.yaml`) |

---

## 7. Architectural Decision Needed Before Slice 2 (Owner Options)

Delivering frames to an app's `ImageReader` / `SurfaceTexture` requires both:
1. Supporting `HAL_PIXEL_FORMAT_YCbCr_420_888` (`lockYCbCr`) and `HAL_PIXEL_FORMAT_BLOB` in `crates/acos-art/shim/gralloc_shim.c` (needed under **all** options), and
2. Calling synchronous two-way `IGraphicBufferProducer::{connect, dequeueBuffer, requestBuffer, queueBuffer}` on a **guest-owned** binder and receiving a `BINDER_TYPE_FD` memfd reply.

Three architectural options are available:

### Option A — Extend `acos-binder-peer` with Host-to-Guest Synchronous Two-Way RPC + FD Receipt
Add a synchronous request/reply channel (`F_REQ` / `F_REP` with `SCM_RIGHTS` FD passing) from `ServiceHost` to a guest `BinderPeer`, allowing Rust services (`hw.camera`) to act directly as an `IGraphicBufferProducer` client and write pixel/JPEG bytes into the returned memfd.

- **Pros**:
  - Zero extra guest processes; stays `SecurityClass::InProcess` within ACOS's modular Rust host architecture.
  - Low RAM/startup overhead (fits `hw.camera`'s tight budget) and simple host-side frame injection (`set_frame` writes straight into the `mmap`'d memfd).
  - Reuses the two-way host->guest RPC mechanism for any future host producer (e.g., `MediaCodec` surface injection, virtual display capture).
- **Cons**:
  - Modifies core binder transport (`crates/acos-binder-peer/src/peer.rs` + `acos-art` event loop) and introduces re-entrancy / deadlock risk if a guest blocks waiting for the host while the host blocks on a guest `BufferQueueProducer` (requires running host->guest RPCs on a dedicated worker thread off the main binder dispatch loop).
  - Must maintain Rust wire codecs for `ICameraDeviceUser`, `ICameraDeviceCallbacks`, `OutputConfiguration`, `CaptureRequest`, `CaptureResultExtras`, and `IGraphicBufferProducer` (`QueueBufferInput`, `GraphicBuffer`) across Android versions.

### Option B — Tiny Guest Camera Helper Process on Existing Inter-Process `forward_foreign`
Run a small guest native binary (e.g., `acos-camera-producer` linked against the image's own `libgui.so` / `libui.so` / `libbinder.so`) started only when `hw.camera` is attached. It receives `IGraphicBufferProducer` handles over ACOS's **already-working** inter-process `forward_foreign` binder path, uses AOSP's own `ANativeWindow` / `IGraphicBufferProducer` client code (zero hand-rolled `GraphicBuffer` parceling), and pulls raw frame pixels from the host `hw.camera` module over a simple Unix socket / memfd.

- **Pros**:
  - **Zero changes to `acos-binder-peer` transport** — reuses the already-proven mproc `forward_foreign` two-way + `SCM_RIGHTS` path.
  - Reuses AOSP's own `libgui.so` (`Surface` / `ANativeWindow_lock` / `IGraphicBufferProducer`) for buffer dequeue/request/queue and fence handling — eliminates an entire class of native parcel drift bugs.
  - Clean separation: host `hw.camera` owns policy, permissions, metadata, and `set_frame` PNG/JPEG decoding; guest helper only pumps pixels into `ANativeWindow`.
- **Cons**:
  - Introduces a second guest process when `hw.camera` is attached (~10–20 MiB RSS, process lifecycle/cleanup on detach).
  - Still requires Rust `ICameraDeviceUser` / `ICameraDeviceCallbacks` state machine in `acos-services` (though much simpler than `IGraphicBufferProducer` + `GraphicBuffer`).

### Option C — Reuse AOSP `cameraserver` (`libcameraservice.so`) + Virtual Emulated Camera Provider in Guest
Boot the Android 14 image's own `cameraserver` (or a minimal host-bridged guest process loading `libcameraservice.so` + an AOSP `EmulatedCameraProvider` HAL shim) so 100% of Google's Camera2 state machine (`ICameraService`, `ICameraDeviceUser`, `CaptureSession`, request templates, CameraX quirk handling, `CameraMetadataNative` results, JPEG encoding via `libjpeg`) runs unmodified from the image, while host `hw.camera` feeds control ops / frames into the virtual provider.

- **Pros**:
  - Maximum Android & CameraX compatibility (~100% of `ICameraDeviceUser`, repeating requests, AF/AE state machines, stream reconfiguration, multi-stream `Preview` + `ImageAnalysis` + `ImageCapture`, `createDefaultRequest` templates) with minimal hand-coded protocol logic (directly aligns with `docs/OWNER_DIRECTION_2026-11-18.md` "reuse first").
  - Zero changes to `acos-binder-peer` (uses existing mproc `forward_foreign`).
- **Cons**:
  - Highest initial bring-up risk: `libcameraservice.so` pulls in `libfmq`, HIDL/AIDL HAL `IServiceManager` (`android.hardware.camera.provider.ICameraProvider`), `ISensorPrivacyManager`, `IActivityManager`, `IAppOpsService`, and `libmedia` dependencies that must be stubbed or satisfied in the guest sysroot.
  - Larger RAM/startup footprint (~30–60 MiB) when `hw.camera` is attached.

### Recommendation
- **Option B** is the lowest-risk, highest-leverage path for Slice 2 (zero binder-transport changes, reuses AOSP `libgui.so` for `IGraphicBufferProducer`, small footprint, fast to verify with `ImageReader` + CameraX).
- **Option A** is cleaner if the owner already wants host-to-guest synchronous Binder RPC in `acos-binder-peer` for other subsystems.
- **Option C** is worth a short feasibility spike only if full AOSP `CaptureRequest`/`CaptureResult` state-machine fidelity (e.g., complex CameraX extensions / multi-camera / video recording) is a near-term priority.
