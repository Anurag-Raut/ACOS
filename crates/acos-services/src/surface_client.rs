//! `android.gui.ISurfaceComposerClient` + the transaction applier — the part of
//! the compositor a *client-side* SurfaceControl needs (W-R option A, C1).
//!
//! `SurfaceComposerClient` reaches this node through
//! `ISurfaceComposer.createConnection()` (code 3), then calls `createSurface`
//! (code 1) to turn a name/size/format into an opaque layer handle + layer id.
//! Stock `SurfaceControl`/`Transaction`/`BLASTBufferQueue` then talk to the
//! compositor with a **legacy** interface, `android.ui.ISurfaceComposer`
//! `SET_TRANSACTION_STATE` (code 8), whose parcel is
//! `FrameTimelineInfo, ComposerState[], DisplayState[], flags, applyToken,
//! InputWindowCommands, desiredPresentTime, isAutoTimestamp, uncacheBuffers,
//! listenerCallbacks, transactionId, mergedTransactionIds`.
//!
//! This module decodes that parcel field for field (the image's own
//! `layer_state_t::write`/`read`, `BufferData::writeToParcel`, `WindowInfo::
//! writeToParcel` and `BpSurfaceComposer::setTransactionState`), stores the
//! newest `GraphicBuffer` (memfd + metadata) per layer for the later frame
//! capture, and queues the two reverse callbacks `BLASTBufferQueue` needs:
//!
//! * `ITransactionCompletedListener.onTransactionCompleted` for every callback
//!   id the transaction registered, and
//! * `ITransactionCompletedListener.onReleaseBuffer` for the **previous**
//!   buffer, so the producer's `BufferQueue` gets its slot back.
//!
//! # What is real, what is not
//!
//! Real: the AIDL request/reply shapes, the legacy transaction parcel, the
//! `GraphicBuffer` flatten form (1 fd + 9 handle ints, the gralloc shim wire
//! form from G1), the release callback id (`bufferId`, `frameNumber`), and the
//! fact that the newest pixels are a live `memfd` this process can read.
//!
//! Not real: there is no compositor. Applying a transaction records state and
//! never composes or presents; `displayId`/`latchTime` are synthesized. There is
//! no sync group, so `eOneWay` vs synchronous is recorded but both reply
//! immediately (what W-R calls the "sink" half). Pixels are captured from the
//! latest-buffer store by a later task, not from a display.
//!
//! # Image facts this file depends on (and why they are not the r75 tag)
//!
//! * `ISurfaceComposerClient` codes: createSurface 1, clearLayerFrameStats 2,
//!   getLayerFrameStats 3, mirrorSurface 4, mirrorDisplay 5 (proxy immediates
//!   at `0x165d70`, `0x166071`, `0x1662c7`, `0x166537`, `0x1667a7`).
//! * The transaction path is the **legacy** interface
//!   `android.ui.ISurfaceComposer` with `SET_TRANSACTION_STATE = 8` and the
//!   r75 `ISurfaceComposer.cpp` write order (proxy at `0xd038f` writes the
//!   descriptor from `android::ISurfaceComposer::descriptor`, `0xd06ae` passes
//!   code 8).
//! * `layer_state_t` follows the r75 `LayerState.cpp` order with three measured
//!   image deltas (all from `libgui.so` disassembly and the captured parcels in
//!   `tests/fixtures/`): the frame-rate group is **three** bytes instead of
//!   four-plus-bool-plus-byte (−8 bytes), the `GraphicBuffer` magic is written
//!   in reverse byte order, and `TrustedPresentationListener` carries one extra
//!   int32 after its binder. An AIDL parcelable's size word counts itself, so
//!   `TrustedPresentationThresholds`/`FocusRequest` payloads are `size - 4`.
//!   These four facts are what let a real BLAST transaction decode at all (W9).
//! * The image's `gui::WindowInfo` is **older than r75**: it has no
//!   `contentSize`, no `canOccludePresentation`, and its
//!   `InputApplicationInfo`/`FrameTimelineInfo`/`FocusRequest` parcelables
//!   carry an inline size word. See [`read_window_info`].
//! * `FrameEventHistoryStats` in the image has the
//!   `frametimestamps_previousrelease` flag **off** (its writer has one
//!   `uint64` then the fence bool), so the callback stats match that shape.

use std::collections::HashMap;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd, RawFd};
use std::sync::{Arc, Mutex};

use acos_aidl::MethodInfo;
use acos_parcel::{BinderId, Parcel};
use acos_sfgeom::{Display, LayerIn, Snapshot};

use crate::compositor_client::DisplayDescriptor;
use crate::display_events::now_ns;
use crate::error::{Result as SResult, ServiceError};
use crate::exception;
use crate::host::{PendingDelivery, ServiceObject};

/// `ISurfaceComposerClient` transaction codes in the API-34 image.
pub mod codes {
    /// `createSurface(name, flags, parent, LayerMetadata)` -> `CreateSurfaceResult`.
    pub const CREATE_SURFACE: u32 = 1;
    /// `clearLayerFrameStats(handle)`.
    pub const CLEAR_LAYER_FRAME_STATS: u32 = 2;
    /// `getLayerFrameStats(handle)` -> `FrameStats`.
    pub const GET_LAYER_FRAME_STATS: u32 = 3;
    /// `mirrorSurface(mirrorFromHandle)` -> `CreateSurfaceResult`.
    pub const MIRROR_SURFACE: u32 = 4;
    /// `mirrorDisplay(displayId)` -> `CreateSurfaceResult`.
    pub const MIRROR_DISPLAY: u32 = 5;
    /// `getSchedulingPolicy()` -> `SchedulingPolicy`.
    pub const GET_SCHEDULING_POLICY: u32 = 6;
}

/// The AIDL descriptor of the client connection node.
pub const CLIENT_DESCRIPTOR: &str = "android.gui.ISurfaceComposerClient";
/// The **legacy** descriptor `Transaction::apply` writes its token with.
pub const LEGACY_COMPOSER_DESCRIPTOR: &str = "android.ui.ISurfaceComposer";
/// The image's legacy transaction code (`BnSurfaceComposer::SET_TRANSACTION_STATE`).
pub const SET_TRANSACTION_STATE: u32 = 8;
/// The descriptor a layer handle binder is registered under.
pub const LAYER_HANDLE_DESCRIPTOR: &str = "android.gui.LayerHandle";
/// The reverse-callback descriptor (`IMPLEMENT_META_INTERFACE` in the image).
pub const TRANSACTION_LISTENER_DESCRIPTOR: &str = "android.gui.ITransactionComposerListener";
/// `ITransactionCompletedListener` tags (legacy enum, `FIRST_CALL_TRANSACTION` = 1).
pub mod listener_codes {
    /// `onTransactionCompleted(ListenerStats)` (oneway).
    pub const ON_TRANSACTION_COMPLETED: u32 = 1;
    /// `onReleaseBuffer(ReleaseCallbackId, Fence, uint32_t)` (oneway).
    pub const ON_RELEASE_BUFFER: u32 = 2;
    /// `onTransactionQueueStalled(String8)` (oneway).
    pub const ON_TRANSACTION_QUEUE_STALLED: u32 = 3;
}

/// How many layer-handle binders are pre-allocated at install time.
pub const LAYER_HANDLE_POOL: usize = 128;

// ---------------------------------------------------------------------------
// Bounds on guest-controlled sizes (S7 sandbox certification)
//
// Everything below arrives in a trampoline parcel from a process ACOS does not
// trust. A compositor must never let a *declared* size become an allocation or
// a read loop that the host pays for, so each shape has a hard ceiling here and
// a rejected parcel is a structured error, never a guess. The ceilings are
// deliberately far above a real BLAST transaction (measured: 88-byte
// `GraphicBuffer` header, 1 fd, 1 composer, one callback per transaction,
// 1080x2209 stride 1088); they bound the host, not the app.

/// Largest accepted `GraphicBuffer` width or height, in pixels.
pub const MAX_BUFFER_DIMENSION: u32 = 8192;
/// Largest accepted `GraphicBuffer` stride, in pixels.
pub const MAX_BUFFER_STRIDE: u32 = 8192;
/// Largest accepted frame, in bytes (`stride * 4 * height`). 64 MiB covers 4K
/// RGBA_8888 (33 MiB) with headroom; the live session's frame is 9.6 MiB.
pub const MAX_BUFFER_BYTES: u64 = 64 * 1024 * 1024;
/// Largest accepted flattened `GraphicBuffer` header (the image writes 88).
pub const MAX_GRAPHIC_BUFFER_HEADER_BYTES: i32 = 4096;
/// Largest accepted fd count in one `GraphicBuffer` (the image writes 1).
pub const MAX_GRAPHIC_BUFFER_FDS: i32 = 8;
/// Largest accepted `CallbackId[]` in one listener entry.
pub const MAX_CALLBACK_IDS: i32 = 4096;
/// Largest accepted listener entries on one layer/top-level vector.
pub const MAX_LISTENERS: i32 = 64;
/// Largest accepted `ComposerState[]` (one per changed layer).
pub const MAX_COMPOSERS: u32 = 256;
/// Largest accepted `DisplayState[]` (ACOS has one display).
pub const MAX_DISPLAYS: u32 = 16;
/// Largest accepted top-level `uncacheBuffers` count.
pub const MAX_UNCACHE_BUFFERS: u32 = 1024;
/// Largest number of layers one C1 composite may draw. A live session has two
/// or three (Activity, dialog, popup); 16 leaves room for a menu with submenus
/// and still bounds the per-frame work a hostile guest can ask for.
pub const MAX_COMPOSITE_LAYERS: usize = 16;
/// Largest total of the layers' own buffers a composite may read, in bytes.
/// Four 64 MiB frames; a real session is 9.6 MiB per layer.
pub const MAX_COMPOSITE_INPUT_BYTES: u64 = 256 * 1024 * 1024;

/// `WindowManager.LayoutParams.FLAG_DIM_BEHIND`, as the image writes it in
/// `gui::WindowInfo.layoutParamsFlags`. The dim **layer** is the window
/// manager's (`mDimLayer` in the real `WindowManagerService`), so ACOS draws it
/// from the host's own `LayoutParams` (`flags` + `dimAmount`) at composite
/// time; see [`SurfaceState::capture_composite`].
pub const FLAG_DIM_BEHIND: i32 = 0x0000_0002;

/// `WindowManager.LayoutParams.FLAG_SECURE`: the window's pixels may not be
/// captured or shown on a non-secure display. ACOS's live view is a separate
/// stream, so it blanks any frame while a secure window is visible
/// ([`SurfaceState::has_secure_window`]).
pub const FLAG_SECURE: i32 = 0x0000_2000;

/// `layer_state_t` change flags used by this task.
pub mod what {
    /// `ePositionChanged`.
    pub const POSITION: u64 = 0x0000_0001;
    /// `eLayerChanged`.
    pub const LAYER: u64 = 0x0000_0002;
    /// `eAlphaChanged`.
    pub const ALPHA: u64 = 0x0000_0008;
    /// `eFlagsChanged` (carries `flags`/`mask`, incl. [`super::LAYER_HIDDEN`]).
    pub const FLAGS: u64 = 0x0000_0040;
    /// `eReparent` (`parentSurfaceControlForChild` on the wire).
    ///
    /// `Transaction::reparent(sc, nullptr)` is what `SurfaceControl::destroy()`
    /// in the image does first: the layer is detached from its parent and the
    /// client drops the handle. ACOS has no binder-death signal for a layer
    /// handle, so a reparent-to-null of a **window surface** layer (one the
    /// window manager created) is the observable half of that destroy.
    pub const REPARENT: u64 = 0x0000_8000;
    /// `eBufferChanged`.
    pub const BUFFER: u64 = 0x0020_0000;
    /// `eDataspaceChanged`.
    pub const DATASPACE: u64 = 0x0080_0000;
    /// `eMatrixChanged`.
    pub const MATRIX: u64 = 0x0000_0010;
    /// `eBufferCropChanged`.
    pub const BUFFER_CROP: u64 = 0x0000_2000;
    /// `eBufferTransformChanged`.
    pub const BUFFER_TRANSFORM: u64 = 0x0004_0000;
    /// `eTransformToDisplayInverseChanged`.
    pub const TRANSFORM_TO_DISPLAY_INVERSE: u64 = 0x0008_0000;
    /// `eCropChanged`.
    pub const CROP: u64 = 0x0010_0000;
    /// `eDestinationFrameChanged` (bit 32; `what` is a `uint64_t`).
    pub const DESTINATION_FRAME: u64 = 0x0000_0001_0000_0000;
}

/// `layer_state_t::eLayerHidden` (the bit in `flags` when `eFlagsChanged` is
/// set). `SurfaceControl.Transaction.hide()`/`setVisibility(..., false)` and a
/// stopped Activity's window surface both arrive this way.
pub const LAYER_HIDDEN: u32 = 0x0000_0001;

/// A rectangle as `ui::Rect` flattens (four `int32`s).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rect {
    /// Left edge.
    pub left: i32,
    /// Top edge.
    pub top: i32,
    /// Right edge.
    pub right: i32,
    /// Bottom edge.
    pub bottom: i32,
}

fn rect_is_empty(rect: Rect) -> bool {
    rect.right <= rect.left || rect.bottom <= rect.top
}

/// `Some(rect)` when the wire rect has area; an empty `ui::Rect` is "unset"
/// (`RequestedLayerState.cpp:363` tests `destinationFrame.isEmpty()`).
fn rect_option(rect: Rect) -> Option<Rect> {
    (!rect_is_empty(rect)).then_some(rect)
}

fn rect_array(rect: Rect) -> [i32; 4] {
    [rect.left, rect.top, rect.right, rect.bottom]
}

/// C1 frame identity with no pixel payload (see
/// [`SurfaceState::latest_frame_metadata`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrameMetadata {
    /// C1 layer id the buffer belongs to.
    pub layer_id: i32,
    /// C1 layer name when known.
    pub layer_name: String,
    /// Buffer width in pixels.
    pub width: u32,
    /// Buffer height in pixels.
    pub height: u32,
    /// `BufferData.frameNumber` (the producer's own frame number).
    pub frame_number: u64,
    /// `GraphicBuffer.id`.
    pub buffer_id: u64,
    /// `BufferData.producerId`.
    pub producer_id: u32,
    /// C1 store counter at the time this buffer became `latest`.
    pub generation: u64,
}

/// One host-side copy of the newest C1 buffer, read from the guest's memfd.
///
/// W10's live session uses this as its screenshot: it is labelled as an
/// **ACOS C1 buffer capture**, never as a physical-device capture.
#[derive(Debug, Clone)]
pub struct FrameCapture {
    /// C1 layer id the buffer belongs to.
    pub layer_id: i32,
    /// C1 layer name (the window's `SurfaceControl` name when known).
    pub layer_name: String,
    /// Buffer width in pixels.
    pub width: u32,
    /// Buffer height in pixels.
    pub height: u32,
    /// Row stride in pixels.
    pub stride: u32,
    /// `HAL_PIXEL_FORMAT_*`.
    pub format: i32,
    /// `BufferData.frameNumber` (the producer's own frame number).
    pub frame_number: u64,
    /// `GraphicBuffer.id` (`ReleaseCallbackId.bufferId`).
    pub buffer_id: u64,
    /// `BufferData.producerId`.
    pub producer_id: u32,
    /// C1 store counter at the time this buffer became `latest`.
    pub generation: u64,
    /// Bytes per row (`stride * 4`).
    pub row_bytes: usize,
    /// Raw pixels (`row_bytes * height`, RGBA_8888-class layout).
    pub rgba: Vec<u8>,
    /// FNV-1a over the raw pixels (a content hash independent of the engine).
    pub fnv1a: u64,
    /// Pixels with any non-zero byte (0 means a blank buffer).
    pub non_zero_pixels: usize,
    /// How many layers this frame is composed of (1 for a single-layer pass
    /// through, more once a dialog/popup is visible).
    pub layers: u32,
    /// `true` when `rgba` is the composite of the visible layers in z-order,
    /// `false` when it is one layer's buffer passed through unchanged.
    pub composite: bool,
}

/// The newest buffer a layer has posted.
#[derive(Debug)]
pub struct LatestBuffer {
    /// The buffer's memfd (`GraphicBuffer`'s `BINDER_TYPE_FD`), owned here.
    pub fd: OwnedFd,
    /// `GraphicBuffer.width`.
    pub width: u32,
    /// `GraphicBuffer.height`.
    pub height: u32,
    /// `GraphicBuffer.stride` in pixels.
    pub stride: u32,
    /// `GraphicBuffer.format` (`HAL_PIXEL_FORMAT_*`).
    pub format: i32,
    /// `BufferData.frameNumber`.
    pub frame_number: u64,
    /// `GraphicBuffer.id` (the release-callback `bufferId`).
    pub buffer_id: u64,
    /// `cachedBuffer.token` (the owning process's `BufferCache` binder).
    pub cached_token: BinderId,
    /// Monotonic `SurfaceState::buffer_cache` entry generation (`0` when uncached).
    pub cache_gen: u64,
    /// `BufferData.producerId`.
    pub producer_id: u32,
    /// `BufferData.crop` if the transaction carried one.
    pub crop: Option<Rect>,
}

impl LatestBuffer {
    /// Borrow the memfd for a capture.
    #[must_use]
    pub fn fd(&self) -> BorrowedFd<'_> {
        self.fd.as_fd()
    }

    /// Byte size of one row (`stride * 4`) for RGBA_8888-class formats.
    #[must_use]
    pub fn row_bytes(&self) -> usize {
        self.stride as usize * 4
    }

    /// The `ReleaseCallbackId` the client keyed its release callback with.
    #[must_use]
    pub fn release_id(&self) -> ReleaseCallbackId {
        ReleaseCallbackId {
            buffer_id: self.buffer_id,
            frame_number: self.frame_number,
        }
    }
}

/// The compositor-visible placement of one layer, in display coordinates.
///
/// The guest sends everything a *client* can know on the wire
/// (`layer_state_t.x/y/z`, `gui::WindowInfo.alpha`); the window's frame origin,
/// its z-order against other windows and its dim amount belong to the window
/// manager, so the host's `WmsState` pushes those into the registry on every
/// `relayout` ([`crate::wms::WmsState::publish_layer_placement`]). A placement
/// overrides the guest's own values.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LayerPlacement {
    /// Frame left edge on the display.
    pub x: i32,
    /// Frame top edge on the display.
    pub y: i32,
    /// Z-order (higher draws later; stock: `windows.len()+1` at add time).
    pub z: i32,
    /// Window alpha (`WindowManager.LayoutParams.alpha`).
    pub alpha: f32,
    /// Dim amount drawn *below* this layer (`0.0` = no dim layer).
    pub dim_amount: f32,
    /// The window carries `FLAG_SECURE`: its pixels must not be published.
    pub secure: bool,
}

/// The layer geometry a client can set on its own surface
/// (`SurfaceControl.Transaction.setPosition`/`setLayer`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LayerGeometry {
    /// `layer_state_t.x` (pixels).
    pub x: i32,
    /// `layer_state_t.y` (pixels).
    pub y: i32,
    /// `layer_state_t.z`.
    pub z: i32,
}

/// Why a C1 composite could not be produced. Structured, never a blank frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompositeError {
    /// No layer has posted a buffer yet.
    NoFrame,
    /// More visible layers than [`MAX_COMPOSITE_LAYERS`].
    TooManyLayers {
        /// Layers offered.
        layers: usize,
        /// The cap.
        max: usize,
    },
    /// The union of the layer frames is larger than
    /// [`MAX_BUFFER_DIMENSION`] on a side, or degenerate.
    CanvasTooLarge {
        /// Union width.
        width: i64,
        /// Union height.
        height: i64,
    },
    /// The composite (or the layers it reads) is larger than the byte caps.
    BytesTooLarge {
        /// Bytes required.
        bytes: u64,
        /// The cap that refused it.
        max: u64,
    },
    /// A layer buffer's format is not RGBA_8888-class (the only layout a C1
    /// capture can interpret).
    UnsupportedFormat {
        /// The image's `HAL_PixelFormat`.
        format: i32,
    },
    /// A layer's buffer could not be read in full.
    Unreadable {
        /// C1 layer id.
        layer_id: i32,
    },
}

impl CompositeError {
    /// The stable code for evidence lines.
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::NoFrame => "COMPOSITE_NO_FRAME",
            Self::TooManyLayers { .. } => "COMPOSITE_TOO_MANY_LAYERS",
            Self::CanvasTooLarge { .. } => "COMPOSITE_CANVAS_TOO_LARGE",
            Self::BytesTooLarge { .. } => "COMPOSITE_BYTES_TOO_LARGE",
            Self::UnsupportedFormat { .. } => "COMPOSITE_UNSUPPORTED_FORMAT",
            Self::Unreadable { .. } => "COMPOSITE_UNREADABLE",
        }
    }
}

impl std::fmt::Display for CompositeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoFrame => write!(f, "no layer has posted a buffer"),
            Self::TooManyLayers { layers, max } => {
                write!(f, "{layers} visible layers exceeds the cap of {max}")
            }
            Self::CanvasTooLarge { width, height } => {
                write!(f, "composite canvas {width}x{height} is out of bounds")
            }
            Self::BytesTooLarge { bytes, max } => {
                write!(f, "composite needs {bytes} bytes, cap is {max}")
            }
            Self::UnsupportedFormat { format } => {
                write!(f, "layer buffer format {format} is not RGBA_8888")
            }
            Self::Unreadable { layer_id } => {
                write!(f, "layer {layer_id}'s buffer could not be read in full")
            }
        }
    }
}

/// `BufferData::generateReleaseCallbackId()`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReleaseCallbackId {
    /// `GraphicBuffer.id`.
    pub buffer_id: u64,
    /// `BufferData.frameNumber`.
    pub frame_number: u64,
}

/// One `ListenerCallbacks` entry from `layer_state_t.listeners`.
#[derive(Clone, Debug)]
pub struct ListenerCallbacks {
    /// The `ITransactionCompletedListener` binder (null is recorded as 0).
    pub listener: BinderId,
    /// Callback ids this listener registered for this transaction.
    pub callback_ids: Vec<CallbackId>,
}

/// One `CallbackId` (`{int64 id; int32 type;}`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CallbackId {
    /// Client-assigned id.
    pub id: i64,
    /// 0 = `ON_COMPLETE`, 1 = `ON_COMMIT`.
    pub kind: i32,
}

/// One layer as the B1b geometry kernel sees it: the record, the buffer whose
/// size feeds `getBufferSize`, and the input row handed to `acos_sfgeom`.
struct KernelRow<'a> {
    record: &'a LayerRecord,
    buffer: Option<&'a LatestBuffer>,
    /// Window-manager dim for this layer (inherited from its window surface).
    dim_amount: f32,
    input: LayerIn,
}

/// The compositor's per-layer record.
#[derive(Debug)]
pub struct LayerRecord {
    /// The binder the client holds (`SurfaceControl.mHandle`).
    pub handle: BinderId,
    /// `CreateSurfaceResult.layerId`.
    pub layer_id: i32,
    /// `CreateSurfaceResult.layerName`.
    pub name: String,
    /// Requested width.
    pub width: u32,
    /// Requested height.
    pub height: u32,
    /// Requested format.
    pub format: i32,
    /// `createSurface` flags.
    pub flags: i32,
    /// The parent handle, when one was supplied.
    pub parent: Option<BinderId>,
    /// The newest posted buffer (what a live-frame capture reads).
    pub latest: Option<LatestBuffer>,
    /// The listener that owns `latest`'s release callback.
    pub latest_listener: BinderId,
    /// Transactions applied to this layer.
    pub transactions: u64,
    /// Frame numbers posted, in order (bounded to 32).
    pub posted_frames: Vec<u64>,
    /// C1's monotonic store counter for `latest`: one increment per stored
    /// `BufferData` buffer, across all layers. A capture is a new observation
    /// when this is greater than the previous capture's value.
    pub latest_generation: u64,
    /// The client's own position/z for this layer (`setPosition`/`setLayer`).
    pub geometry: LayerGeometry,
    /// Window alpha from `gui::WindowInfo` (1.0 when the transaction carried no
    /// window info, i.e. a BLAST child layer).
    pub alpha: f32,
    /// The window manager's placement, when this layer is a window surface.
    pub placement: Option<LayerPlacement>,
    /// `layer_state_t.destinationFrame` (BLAST's `Rect(mSize)`, the buffer's
    /// display size). Empty means "no destination frame", i.e. the
    /// position/matrix path (`RequestedLayerState.cpp:363`).
    pub destination_frame: Option<Rect>,
    /// `layer_state_t.crop`: the window crop, distinct from `bufferCrop`.
    pub crop: Option<Rect>,
    /// `layer_state_t.bufferCrop`: the crop inside the buffer, distinct from
    /// `crop`. A `BufferData.crop` on a newer buffer supersedes it for that
    /// buffer (kept in [`LatestBuffer::crop`]).
    pub buffer_crop: Option<Rect>,
    /// `layer_state_t.bufferTransform` (`ui::Transform::RotationFlags`).
    pub buffer_transform: u32,
    /// `layer_state_t.transformToDisplayInverse`.
    pub transform_to_display_inverse: bool,
    /// `layer_state_t.matrix` in wire order `(dsdx, dtdx, dtdy, dsdy)`.
    /// Identity unless the transaction carried `eMatrixChanged`.
    pub matrix: [f32; 4],
    /// `layer_state_t.flags & eLayerHidden`: the client hid the layer.
    ///
    /// A hidden layer is not drawn at all (stock: `Layer::setFlags` → not in
    /// the visible set), but its stored buffer stays until it is replaced or
    /// the layer is destroyed. `W42` made this a first-class state so a
    /// stopped Activity's surface stops appearing in the composite before its
    /// window is removed.
    pub hidden: bool,
}

/// A `GraphicBuffer` the client sent inline, kept so a later `client_cache_t`
/// reference to the same id can be resolved (`Transaction::cacheBuffers`).
#[derive(Debug)]
struct CachedBuffer {
    /// A duplicate of the guest's memfd, absent when the entry could not be kept.
    fd: Option<OwnedFd>,
    /// Width.
    width: u32,
    /// Height.
    height: u32,
    /// Row stride in bytes.
    stride: u32,
    /// `PixelFormat`.
    format: i32,
    /// `GraphicBuffer::getId()`.
    id: u64,
    /// Monotonic generation assigned when inserted into `SurfaceState::buffer_cache`.
    cache_gen: u64,
}

impl CachedBuffer {
    /// A private duplicate so the cache entry outlives one transaction.
    fn duplicate(&self) -> Option<Self> {
        Some(Self {
            fd: Some(self.fd.as_ref()?.try_clone().ok()?),
            width: self.width,
            height: self.height,
            stride: self.stride,
            format: self.format,
            id: self.id,
            cache_gen: self.cache_gen,
        })
    }
}

/// `android.gui.FrameStats` counters for `getLayerFrameStats`.
#[derive(Clone, Debug, Default)]
struct FrameStats {
    /// Posted buffer frame numbers.
    posted: Vec<i64>,
    /// No presentation exists, so this is always empty.
    actual: Vec<i64>,
    /// Nanoseconds.
    frame_ready: Vec<i64>,
}

/// Shared state of the compositor-client surface (one per `ServiceHost`).
#[derive(Debug)]
pub struct SurfaceState {
    /// The `ISurfaceComposerClient` binder `createConnection` answers.
    pub client_binder: BinderId,
    /// Layers by handle binder id.
    layers: HashMap<u64, LayerRecord>,
    /// Pre-allocated layer-handle binders (see [`LAYER_HANDLE_POOL`]).
    handle_pool: Vec<BinderId>,
    /// `binder id -> frame stats`.
    stats: HashMap<u64, FrameStats>,
    /// Reverse calls queued for the runtime to deliver (`PENDING` deliveries).
    deliveries: Vec<PendingDelivery>,
    next_layer_id: i32,
    /// `getMaxAcquiredBufferCount()` — the same value the compositor node
    /// reports (BLAST's buffer pool is sized from it; see W10).
    pub max_acquired_buffers: u32,
    /// Number of legacy transaction parcels decoded.
    pub transactions_decoded: u64,
    /// Monotonic counter: one increment per stored `BufferData` buffer. This is
    /// the compositor-client's own "a new frame landed" signal (the W10 live
    /// session binds its captures to it).
    buffer_generations: u64,
    /// `BufferCache` entries keyed by `(cachedBuffer.token, GraphicBuffer.id)`
    /// (matching AOSP `ClientCache::getBuffer` on `client_cache_t { token, id }`;
    /// see [`SurfaceState::cache_buffer`]). Each process runs in its own PID
    /// namespace, so `GraphicBuffer::getId()` (`pid << 32 | counter`) collides
    /// across processes (e.g. `0x2_0000_0000` in both Counter and Launcher);
    /// keying by `(token, id)` keeps each process's memfds isolated.
    buffer_cache: HashMap<(BinderId, u64), CachedBuffer>,
    /// Insertion order for [`SurfaceState::buffer_cache`] (drop-oldest).
    buffer_cache_order: Vec<(BinderId, u64)>,
    /// Monotonic entry generation for [`SurfaceState::buffer_cache`] so
    /// destroying an older layer cannot evict a newer replacement entry.
    next_cache_gen: u64,
    /// Layer-handle binder ids that were destroyed (W42). A transaction that
    /// still names one must not resurrect the record: the auto-insert in
    /// [`apply_layer_state`] is how a destroyed layer would otherwise come back
    /// with no placement and its old pixels. Entries are removed when the
    /// handle is handed out again ([`SurfaceState::create_layer`]).
    destroyed: std::collections::HashSet<u64>,
    /// Layers dropped by [`SurfaceState::destroy_layer_tree`] (evidence).
    destroyed_layers: u64,
    /// Stored buffers dropped with their layer (evidence: memfds released).
    released_buffers: u64,
    /// The display C1 composites onto (`DisplayState.layerStackSpaceRect`).
    ///
    /// SurfaceFlinger clips every layer to the display viewport; a window whose
    /// surface is larger than its frame (one with surface insets, W54) extends
    /// past the display edge on purpose, and the frame must stay display-sized.
    display: DisplayDescriptor,
}

impl SurfaceState {
    fn new(
        client_binder: BinderId,
        handle_pool: Vec<BinderId>,
        display: DisplayDescriptor,
    ) -> Self {
        Self {
            client_binder,
            layers: HashMap::new(),
            handle_pool,
            stats: HashMap::new(),
            deliveries: Vec::new(),
            next_layer_id: 1,
            max_acquired_buffers: std::env::var("ACOS_MAX_ACQUIRED_BUFFERS")
                .ok()
                .and_then(|v| v.trim().parse::<u32>().ok())
                .filter(|v| *v > 0 && *v <= 64)
                .unwrap_or(crate::compositor_client::DEFAULT_MAX_ACQUIRED_BUFFERS),
            transactions_decoded: 0,
            buffer_generations: 0,
            buffer_cache: HashMap::new(),
            buffer_cache_order: Vec::new(),
            next_cache_gen: 1,
            destroyed: std::collections::HashSet::new(),
            destroyed_layers: 0,
            released_buffers: 0,
            display,
        }
    }

    /// Layers dropped by [`SurfaceState::destroy_layer_tree`] so far.
    #[must_use]
    pub fn destroyed_layers(&self) -> u64 {
        self.destroyed_layers
    }

    /// Stored buffers whose memfd was released with their layer.
    #[must_use]
    pub fn released_buffers(&self) -> u64 {
        self.released_buffers
    }

    /// Layers that currently retain a buffer (each holds one memfd).
    #[must_use]
    pub fn retained_buffers(&self) -> usize {
        self.layers.values().filter(|l| l.latest.is_some()).count()
    }

    /// Every memfd the compositor-client registry keeps alive: one per stored
    /// layer buffer plus one per `BufferCache` entry. A destroy must lower
    /// this; a leak shows up as a count that only grows.
    #[must_use]
    pub fn open_buffer_fds(&self) -> usize {
        self.retained_buffers()
            + self
                .buffer_cache
                .values()
                .filter(|c| c.fd.is_some())
                .count()
    }

    /// Destroy a layer and every layer below it, dropping their retained
    /// buffers (memfds) and their frame stats.
    ///
    /// This is the stock `Layer` teardown ACOS can observe: the window manager
    /// destroys the window's `SurfaceControl` (`IWindowSession.remove`) or the
    /// client reparents the window surface to null, and SurfaceFlinger removes
    /// the layer **and its children** — including the BLAST child that holds the
    /// pixels. Returns the number of layers dropped; each was added to the
    /// tombstone set so a late transaction cannot resurrect it.
    pub fn destroy_layer_tree(&mut self, root: BinderId) -> usize {
        let mut doomed = vec![root.0];
        let mut index = 0;
        while index < doomed.len() {
            let current = doomed[index];
            let children: Vec<u64> = self
                .layers
                .iter()
                .filter(|(handle, layer)| {
                    layer.parent == Some(BinderId(current)) && !doomed.contains(handle)
                })
                .map(|(handle, _)| *handle)
                .collect();
            doomed.extend(children);
            index += 1;
        }
        let mut dropped = 0usize;
        let mut released = 0u64;
        for handle in doomed {
            let Some(layer) = self.layers.remove(&handle) else {
                continue;
            };
            dropped += 1;
            if let Some(buffer) = layer.latest.as_ref() {
                released += 1;
                // The client's `BufferCache` holds a second dup of the same
                // `(token, GraphicBuffer.id)` (see `cache_buffer`). Only evict
                // it when the cache still holds this exact entry generation
                // (not a newer replacement) and no other live layer retains it.
                let key = (buffer.cached_token, buffer.buffer_id);
                let still_shared = self.layers.values().any(|other| {
                    other.latest.as_ref().is_some_and(|b| {
                        b.cached_token == buffer.cached_token
                            && b.buffer_id == buffer.buffer_id
                            && b.cache_gen == buffer.cache_gen
                    })
                });
                let matches_current = buffer.cache_gen != 0
                    && self
                        .buffer_cache
                        .get(&key)
                        .is_some_and(|entry| entry.cache_gen == buffer.cache_gen);
                if matches_current && !still_shared && self.buffer_cache.remove(&key).is_some() {
                    self.buffer_cache_order.retain(|k| *k != key);
                }
            }
            // Dropping `layer.latest` closes this process's memfd dup.
            self.stats.remove(&handle);
            self.destroyed.insert(handle);
            // Give the handle binder back. The pool is pre-allocated (a
            // `createSurface` has no `&mut ServiceHost` to mint a node), so a
            // destroyed layer that kept its handle leaked one of the
            // `LAYER_HANDLE_POOL` entries for the life of the session: every
            // window/popup/dialog consumed two (the window surface and its
            // BLAST child) and the Nth one failed `createSurface` with
            // `layer-handle pool exhausted` -> `BLASTBufferQueue: mSurfaceControl
            // must not be NULL` -> SIGABRT (a React Native dev loop hit it on
            // the 4th reload: each reload shows and dismisses a loading popup).
            // Reuse is FIFO (`take_handle` pops the tail, this inserts at the
            // head) so a handle is the last one handed out again, and the
            // `destroyed` tombstone above stays until `create_layer` reissues
            // it, so a late transaction naming the dead layer is still refused.
            self.handle_pool.insert(0, BinderId(handle));
        }
        self.destroyed_layers += dropped as u64;
        self.released_buffers += released;
        if std::env::var_os("ACOSC1_TRACE").is_some() {
            eprintln!(
                "ACOSC1 destroy handle={:#x} layers={} buffers={} retained={} fds={}",
                root.0,
                dropped,
                released,
                self.retained_buffers(),
                self.open_buffer_fds()
            );
        }
        dropped
    }

    /// Set a layer's `eLayerHidden` state. `false` when the handle is not a
    /// live layer (destroyed or never created).
    pub fn set_layer_hidden(&mut self, handle: BinderId, hidden: bool) -> bool {
        match self.layers.get_mut(&handle.0) {
            Some(layer) => {
                layer.hidden = hidden;
                true
            }
            None => false,
        }
    }

    /// Remember a buffer the client sent inline so a later `client_cache_t`
    /// reference to `(owner, GraphicBuffer.id)` can be resolved.
    ///
    /// Keyed by `(owner, meta.id)` like AOSP `ClientCache::getBuffer` (`{token,
    /// id}`): separate guest processes in separate PID namespaces mint the same
    /// `GraphicBuffer::getId()` values (`0x2_0000_0000`, ...), so `owner`
    /// (`cachedBuffer.token`) keeps them separate. On a collision for the same
    /// `(owner, id)` key, the stale entry is replaced with the new buffer.
    /// Returns the assigned monotonic `cache_gen` (`0` when not cached).
    fn cache_buffer(state: &mut SurfaceState, owner: BinderId, meta: &GraphicBufferMeta) -> u64 {
        const MAX_CACHED_BUFFERS: usize = 16;
        const MAX_CACHED_BUFFERS_TOTAL: usize = 64;
        if meta.id == 0 {
            return 0;
        }
        // A descriptor limit (EMFILE) must not abort the host: an entry that
        // cannot be duplicated is simply not cached, and a later `client_cache_t`
        // reference to it is the same structured miss as an unknown id.
        let Some(fd) = meta.fd.try_clone().ok() else {
            return 0;
        };
        let key = (owner, meta.id);
        if state.buffer_cache.contains_key(&key) {
            state.buffer_cache_order.retain(|k| *k != key);
        } else {
            let owner_count = state
                .buffer_cache_order
                .iter()
                .filter(|(t, _)| *t == owner)
                .count();
            if owner_count >= MAX_CACHED_BUFFERS {
                if let Some(pos) = state.buffer_cache_order.iter().position(|(t, _)| *t == owner) {
                    let oldest = state.buffer_cache_order.remove(pos);
                    state.buffer_cache.remove(&oldest);
                }
            }
            while state.buffer_cache_order.len() >= MAX_CACHED_BUFFERS_TOTAL {
                let oldest = state.buffer_cache_order.remove(0);
                state.buffer_cache.remove(&oldest);
            }
        }
        let cache_gen = state.next_cache_gen;
        state.next_cache_gen = state.next_cache_gen.wrapping_add(1).max(1);
        state.buffer_cache_order.push(key);
        state.buffer_cache.insert(
            key,
            CachedBuffer {
                fd: Some(fd),
                width: meta.width,
                height: meta.height,
                stride: meta.stride,
                format: meta.format,
                id: meta.id,
                cache_gen,
            },
        );
        cache_gen
    }

    /// Evict a single `(token, id)` entry requested by `uncacheBuffers`.
    pub fn uncache_buffer(&mut self, owner: BinderId, id: u64) -> bool {
        let key = (owner, id);
        if self.buffer_cache.remove(&key).is_some() {
            self.buffer_cache_order.retain(|k| *k != key);
            true
        } else {
            false
        }
    }

    /// Drop every `BufferCache` entry owned by process `tag` or any of its
    /// recorded binder tokens (AOSP `ClientCache::removeProcess` on death).
    pub fn release_process_buffers(&mut self, tag: u32, owned_binders: &[BinderId]) -> usize {
        let mut released = 0usize;
        self.buffer_cache.retain(|(token, _), _| {
            let by_token = !token.is_null() && owned_binders.contains(token);
            let by_tag = token.0 & crate::host::GUEST_BINDER_TAG != 0
                && crate::atms::process_tag_of(token.0) == tag;
            if by_token || by_tag {
                released += 1;
                false
            } else {
                true
            }
        });
        if released > 0 {
            self.buffer_cache_order
                .retain(|key| self.buffer_cache.contains_key(key));
        }
        released
    }

    /// The nearest `z` and dim amount on this layer's ancestor chain.
    ///
    /// B1b feeds the kernel one flat layer list, while stock gets stacking from
    /// the layer hierarchy (`LayerHierarchy::traverseInZOrder`). A BLAST child
    /// has no `layer_state_t.z` of its own; its window surface does, and the
    /// window manager owns it. So the child takes the nearest ancestor's
    /// placement z (the old `placement_of` did the same, for x/y too). The dim
    /// layer is the window manager's, not a kernel input, so it stays here.
    #[must_use]
    fn inherited_stacking(&self, layer: &LayerRecord) -> (i32, f32) {
        const MAX_DEPTH: usize = 8;
        let mut current = layer;
        for _ in 0..MAX_DEPTH {
            if let Some(placement) = current.placement {
                return (placement.z, placement.dim_amount);
            }
            let Some(parent) = current.parent else {
                break;
            };
            let Some(next) = self.layers.get(&parent.0) else {
                break;
            };
            if next.handle == current.handle {
                break;
            }
            current = next;
        }
        (layer.geometry.z, 0.0)
    }

    /// Build the kernel's geometry input for every layer in the registry.
    ///
    /// Values come from what the transaction applier stored:
    /// * `x`/`y`/`z`/`alpha` — the window manager's [`LayerPlacement`] when the
    ///   layer has one, otherwise the guest's `layer_state_t`/`WindowInfo`;
    /// * `destinationFrame`, `crop`, `bufferCrop`, `bufferTransform`,
    ///   `transformToDisplayInverse`, `matrix` — the decoded transaction;
    /// * buffer `(w, h)` — the newest stored `LatestBuffer`;
    /// * `hidden` — `eLayerHidden` (`LayerRecord::hidden`).
    ///
    /// `eIgnoreDestinationFrame` is always false: C1 does not decode that flag
    /// (it is not on the wire for the app windows ACOS runs), so the kernel
    /// takes the destination-frame path whenever one is set, as stock does by
    /// default (`RequestedLayerState.cpp:363`).
    fn kernel_rows(&self) -> Vec<KernelRow<'_>> {
        let id_by_handle: HashMap<u64, i32> = self
            .layers
            .iter()
            .map(|(handle, record)| (*handle, record.layer_id))
            .collect();
        let mut rows = Vec::with_capacity(self.layers.len());
        for record in self.layers.values() {
            let parent = record
                .parent
                .and_then(|handle| id_by_handle.get(&handle.0).copied());
            let (z, dim_amount) = self.inherited_stacking(record);
            let (x, y, alpha) = match record.placement {
                Some(placement) => (placement.x as f32, placement.y as f32, placement.alpha),
                None => (
                    record.geometry.x as f32,
                    record.geometry.y as f32,
                    record.alpha,
                ),
            };
            // `BufferData.crop` is the newest buffer's own crop; otherwise the
            // layer's `bufferCrop` applies (stock: `getBufferCrop`, RLS:481).
            let buffer_crop = record
                .latest
                .as_ref()
                .and_then(|buffer| buffer.crop)
                .filter(|rect| !rect_is_empty(*rect))
                .or(record.buffer_crop);
            rows.push(KernelRow {
                record,
                buffer: record.latest.as_ref(),
                dim_amount,
                input: LayerIn {
                    id: record.layer_id,
                    parent,
                    z,
                    x,
                    y,
                    matrix: record.matrix,
                    destination_frame: record.destination_frame.map(rect_array),
                    crop: record.crop.map(rect_array),
                    buffer_crop: buffer_crop.map(rect_array),
                    buffer_size: record.latest.as_ref().map(|b| (b.width, b.height)),
                    buffer_transform: record.buffer_transform,
                    transform_to_display_inverse: record.transform_to_display_inverse,
                    alpha,
                    hidden: record.hidden,
                },
            });
        }
        rows
    }

    /// C1's monotonic buffer store counter (0 = no frame has landed yet).
    #[must_use]
    pub fn latest_generation(&self) -> u64 {
        self.buffer_generations
    }

    /// Identity of the newest stored buffer **without copying its pixels**.
    ///
    /// A tree-only observation needs to say which frame it is bound to
    /// (`generation`, producer `frame_number`, buffer/layer ids) but never
    /// returns pixels, so it must not clone the RGBA buffer or encode a PNG
    /// (P3: capture was 34.5 ms of a 186 ms tree request). This is a pure
    /// metadata read under the caller's compositor lock.
    #[must_use]
    pub fn latest_frame_metadata(&self) -> Option<FrameMetadata> {
        self.newest_frame_layer()
            .map(|(layer, buffer)| FrameMetadata {
                layer_id: layer.layer_id,
                layer_name: layer.name.clone(),
                width: buffer.width,
                height: buffer.height,
                frame_number: buffer.frame_number,
                buffer_id: buffer.buffer_id,
                producer_id: buffer.producer_id,
                generation: layer.latest_generation,
            })
    }

    /// The layer whose newest stored buffer is the newest across all layers.
    ///
    /// A live session has one app window layer, so this is deterministic: the
    /// most recent buffer any layer posted is the frame the app drew last.
    #[must_use]
    pub fn newest_frame_layer(&self) -> Option<(&LayerRecord, &LatestBuffer)> {
        self.layers
            .values()
            .filter_map(|layer| layer.latest.as_ref().map(|buffer| (layer, buffer)))
            .max_by_key(|(layer, _)| layer.latest_generation)
    }

    /// Copy the newest stored C1 buffer out of its memfd.
    ///
    /// This is the **ACOS C1 buffer capture** (`capture_source`): the pixels
    /// are read through this process's own duplicate of the `GraphicBuffer` fd
    /// the guest produced, not from a device framebuffer and not from the
    /// preview renderer. `None` means no buffer has landed yet.
    #[must_use]
    pub fn capture_latest(&self) -> Option<FrameCapture> {
        match self.capture_composite() {
            Ok(frame) => Some(frame),
            Err(err) => {
                // Fail closed with a named code. `None` means "no frame" to the
                // caller, so the *reason* must be visible in the host log
                // instead of the frame silently missing from the evidence.
                if !matches!(err, CompositeError::NoFrame) {
                    eprintln!("ACOSC1 composite refused: {} ({err})", err.code());
                }
                None
            }
        }
    }

    /// The frame the app shows: **every visible layer in z-order**, composited.
    ///
    /// W35 replaces the W10/W32 "newest buffer of one layer" rule. A dialog,
    /// popup or menu is its own top-level `TYPE_APPLICATION` window with its own
    /// compositor layer and a frame above the Activity (W32 §1), so capturing
    /// one layer loses it. The inputs are the state C1 already decoded:
    ///
    /// * pixels — each layer's newest stored `GraphicBuffer` (memfd);
    /// * crop — `BufferData.crop`/`layer_state_t.bufferCrop` (empty = whole
    ///   buffer);
    /// * position/z — the window manager's placement when the layer is a window
    ///   surface ([`LayerPlacement`], pushed on `relayout`), else the client's
    ///   own `layer_state_t.x/y/z`;
    /// * alpha — `WindowManager.LayoutParams.alpha` (window layers) or
    ///   `gui::WindowInfo.alpha` for a layer the guest positions itself;
    /// * dim — the dialog's `FLAG_DIM_BEHIND` + `dimAmount` from the host's own
    ///   `LayoutParams`. The image sends those two values (never a dim layer:
    ///   the dim layer is the *window manager's*, `mDimLayer` in the real
    ///   `WindowManagerService`, and the window manager is the host here), so
    ///   the dim is drawn per its flags: the canvas already drawn is darkened by
    ///   `dimAmount` before the window above it is drawn. Stock bounds the dim
    ///   layer to the display, so the whole canvas is dimmed.
    ///
    /// The canvas is the union of the visible layer frames. A single full-window
    /// layer at the origin passes through unchanged (its own stride included),
    /// so a one-layer session's frame stays byte-identical to W10–W32.
    ///
    /// Every guest-controlled shape is bounded ([`MAX_COMPOSITE_LAYERS`],
    /// [`MAX_COMPOSITE_INPUT_BYTES`], the S7 [`MAX_BUFFER_DIMENSION`]/
    /// [`MAX_BUFFER_BYTES`] ceilings); a refusal is a structured
    /// [`CompositeError`], never a blank or partial frame.
    pub fn capture_composite(&self) -> Result<FrameCapture, CompositeError> {
        // B1b: where pixels go is decided by the vendored Android 14 SurfaceFlinger
        // geometry (`acos_sfgeom`), not by `placement_of`/`drawn_region`/a local
        // canvas sort. C1 still decodes the wire, owns the buffers, and supplies
        // the window manager's placement (x/y/z/alpha/dim) as kernel *inputs*.
        //
        // One unrotated display: ACOS drives a single internal display and no
        // live surface sets a display rotation, so `rotation_flags` is 0
        // (`transformToDisplayInverse` therefore has no display swap), and the
        // display viewport is `self.display` (the app area, W58).
        let rows = self.kernel_rows();
        let inputs: Vec<LayerIn> = rows.iter().map(|row| row.input).collect();
        let snapshots = acos_sfgeom::compute(
            &inputs,
            Display {
                width: self.display.width.max(0).unsigned_abs(),
                height: self.display.height.max(0).unsigned_abs(),
                rotation_flags: 0,
            },
        );

        let mut placed: Vec<Placed<'_>> = Vec::new();
        for (row, snapshot) in rows.iter().zip(snapshots) {
            let Some(buffer) = row.buffer else {
                // No pixels (a window surface or a destroyed layer): its geometry
                // still matters to its children, which the kernel already used.
                continue;
            };
            // `hidden_by_policy` is the kernel's inherited hidden bit (stock
            // LayerSnapshotBuilder.cpp:717-719): own hidden, an ancestor's
            // hidden, or an ancestor's invalid transform.
            if snapshot.hidden_by_policy || snapshot.alpha <= 0.0 {
                continue;
            }
            if snapshot.invalid_transform {
                // No old-path fallback: a singular transform is not drawn and the
                // reason is a structured trace line (stock resets it and hides
                // the layer, LayerSnapshotBuilder.cpp:950-976).
                if std::env::var_os("ACOSC1_TRACE").is_some() {
                    eprintln!(
                        "ACOSC1 snapshot-skip layer={} invalidTransform=1 geom={:?} name={:?}",
                        row.record.layer_id, snapshot.geom_transform, row.record.name
                    );
                }
                continue;
            }
            let source = source_crop(&snapshot, buffer);
            let dest = placed_rect_from_kernel(&snapshot, source);
            placed.push(Placed {
                row,
                snapshot,
                source,
                dest,
            });
        }
        if placed.is_empty() {
            return Err(CompositeError::NoFrame);
        }
        if placed.len() > MAX_COMPOSITE_LAYERS {
            return Err(CompositeError::TooManyLayers {
                layers: placed.len(),
                max: MAX_COMPOSITE_LAYERS,
            });
        }
        // Bottom-to-top: the kernel's ascending (z, id) draw order. The id is the
        // C1 layer id, which is the same stable tie-break the old path used.
        placed.sort_by_key(|p| (p.snapshot.draw_order, p.row.record.layer_id));

        let mut min_x = i64::MAX;
        let mut min_y = i64::MAX;
        let mut max_x = i64::MIN;
        let mut max_y = i64::MIN;
        let mut input_bytes = 0u64;
        for p in &placed {
            if let Some(clipped) = clip_to_display(p.dest, self.display) {
                min_x = min_x.min(clipped.0);
                min_y = min_y.min(clipped.1);
                max_x = max_x.max(clipped.2);
                max_y = max_y.max(clipped.3);
            }
            let bytes = checked_buffer_bytes(
                p.row.buffer.expect("placed has a buffer").width,
                p.row.buffer.expect("placed has a buffer").height,
                p.row.buffer.expect("placed has a buffer").stride,
            )
            .ok_or(CompositeError::CanvasTooLarge {
                width: i64::from(p.row.buffer.expect("placed has a buffer").width),
                height: i64::from(p.row.buffer.expect("placed has a buffer").height),
            })?;
            input_bytes = input_bytes.saturating_add(bytes);
        }
        if min_x == i64::MAX {
            // Every visible layer lies entirely outside the display.
            return Err(CompositeError::NoFrame);
        }
        let canvas_width = max_x - min_x;
        let canvas_height = max_y - min_y;
        if canvas_width <= 0 || canvas_height <= 0 {
            return Err(CompositeError::NoFrame);
        }
        if canvas_width > i64::from(MAX_BUFFER_DIMENSION)
            || canvas_height > i64::from(MAX_BUFFER_DIMENSION)
        {
            return Err(CompositeError::CanvasTooLarge {
                width: canvas_width,
                height: canvas_height,
            });
        }
        let row_bytes = (canvas_width as u64).saturating_mul(4);
        let bytes = row_bytes.saturating_mul(canvas_height as u64);
        if bytes > MAX_BUFFER_BYTES {
            return Err(CompositeError::BytesTooLarge {
                bytes,
                max: MAX_BUFFER_BYTES,
            });
        }
        if input_bytes > MAX_COMPOSITE_INPUT_BYTES {
            return Err(CompositeError::BytesTooLarge {
                bytes: input_bytes,
                max: MAX_COMPOSITE_INPUT_BYTES,
            });
        }

        // One layer, at the origin, whole buffer, opaque: the W10-W32
        // pass-through (the buffer's own stride is preserved). The kernel's
        // dest rect is exactly the buffer rect and no crop/dim applies.
        if placed.len() == 1 {
            let p = &placed[0];
            let buffer = p.row.buffer.expect("placed has a buffer");
            let full_buffer = p.source == (0, 0, buffer.width, buffer.height);
            if p.dest == (0, 0, i64::from(buffer.width), i64::from(buffer.height))
                && full_buffer
                && p.row.dim_amount <= 0.0
                && p.snapshot.alpha >= 1.0
            {
                return Self::capture_layer(p.row.record, buffer).ok_or(
                    CompositeError::Unreadable {
                        layer_id: p.row.record.layer_id,
                    },
                );
            }
        }

        let width = canvas_width as u32;
        let height = canvas_height as u32;
        let row = width as usize * 4;
        // One line per composited frame under `ACOSC1_TRACE=1`: the kernel's
        // snapshot for every layer that draws (the B1b replacement for the old
        // placement/crop line).
        if std::env::var_os("ACOSC1_TRACE").is_some() {
            let list: Vec<String> = placed
                .iter()
                .map(|p| {
                    let buffer = p.row.buffer.expect("placed has a buffer");
                    format!(
                        "layer{}(buf={}x{} geom={:?} transformed={:?} cropped={:?} \
                         bufferCrop={:?} alpha={:.3} draw={} dest=[{},{}+{}x{}]{} parent={:?} \
                         name={:?})",
                        p.row.record.layer_id,
                        buffer.width,
                        buffer.height,
                        p.snapshot.geom_transform,
                        p.snapshot.transformed_bounds,
                        p.snapshot.cropped_buffer_size,
                        p.snapshot.buffer_crop,
                        p.snapshot.alpha,
                        p.snapshot.draw_order,
                        p.dest.0,
                        p.dest.1,
                        p.dest.2 - p.dest.0,
                        p.dest.3 - p.dest.1,
                        if p.row.dim_amount > 0.0 {
                            format!(" dim={}", p.row.dim_amount)
                        } else {
                            String::new()
                        },
                        p.row.record.parent.map(|h| h.0),
                        p.row.record.name
                    )
                })
                .collect();
            eprintln!(
                "ACOSC1 composite {}x{} origin=({},{}) of {}",
                width,
                height,
                min_x,
                min_y,
                list.join(" + ")
            );
        }
        let mut canvas = vec![0u8; row * height as usize];
        let (top_layer, top_buffer) = {
            let last = placed.last().expect("non-empty");
            (
                last.row.record.layer_id,
                last.row.buffer.expect("placed has a buffer"),
            )
        };
        let mut drawn = 0u32;
        for p in placed {
            let buffer = p.row.buffer.expect("placed has a buffer");
            if buffer.format != 1 {
                return Err(CompositeError::UnsupportedFormat {
                    format: buffer.format,
                });
            }
            if p.row.dim_amount > 0.0 && drawn > 0 {
                dim_canvas(&mut canvas, p.row.dim_amount.clamp(0.0, 1.0));
            }
            let pixels = read_buffer_pixels(buffer).ok_or(CompositeError::Unreadable {
                layer_id: p.row.record.layer_id,
            })?;
            if p.row.record.buffer_transform & 4 != 0 && std::env::var_os("ACOSC1_TRACE").is_some()
            {
                // Known B1b gap: a 90-degree buffer rotation is carried by the
                // kernel's size/bounds, but the blit below does not rotate pixel
                // data. No live ACOS surface sets it; logged, never silent.
                eprintln!(
                    "ACOSC1 snapshot-buffer-transform layer={} flags={} (pixel rotation not \
                     implemented; drawn unrotated)",
                    p.row.record.layer_id, p.row.record.buffer_transform
                );
            }
            let spec = DrawSpec {
                crop: p.source,
                dst_x: p.dest.0 - min_x,
                dst_y: p.dest.1 - min_y,
                dst_w: (p.dest.2 - p.dest.0).max(0) as u32,
                dst_h: (p.dest.3 - p.dest.1).max(0) as u32,
                alpha: (p.snapshot.alpha.clamp(0.0, 1.0) * 255.0).round() as u32,
            };
            blend_layer(&mut canvas, row, width, height, &pixels, buffer, &spec);
            drawn += 1;
        }
        let (fnv1a, non_zero_pixels) = hash_pixels(&canvas);
        Ok(FrameCapture {
            layer_id: top_layer,
            layer_name: format!("composite({drawn})"),
            width,
            height,
            // The composite's rows are packed: `width * 4` bytes, no stride
            // padding.
            stride: width,
            format: 1,
            frame_number: top_buffer.frame_number,
            buffer_id: top_buffer.buffer_id,
            producer_id: top_buffer.producer_id,
            generation: self.buffer_generations,
            row_bytes: row,
            rgba: canvas,
            fnv1a,
            non_zero_pixels,
            layers: drawn,
            composite: true,
        })
    }

    /// The pre-W35 capture: one layer's stored buffer, read in full.
    fn capture_layer(layer: &LayerRecord, buffer: &LatestBuffer) -> Option<FrameCapture> {
        let pixels = read_buffer_pixels(buffer)?;
        let row = buffer.row_bytes();
        let (fnv1a, non_zero_pixels) = hash_pixels(&pixels);
        Some(FrameCapture {
            layer_id: layer.layer_id,
            layer_name: layer.name.clone(),
            width: buffer.width,
            height: buffer.height,
            stride: buffer.stride,
            format: buffer.format,
            frame_number: buffer.frame_number,
            buffer_id: buffer.buffer_id,
            producer_id: buffer.producer_id,
            generation: layer.latest_generation,
            row_bytes: row,
            rgba: pixels,
            fnv1a,
            non_zero_pixels,
            layers: 1,
            composite: false,
        })
    }

    /// Record the window manager's placement for a layer (W35).
    ///
    /// Called on every `relayout` so a dialog's frame, a window's z-order and the
    /// dim amount follow the window, not the guest's own transaction. Returns
    /// `false` when the handle is not a layer of this registry.
    pub fn set_layer_placement(&mut self, handle: BinderId, placement: LayerPlacement) -> bool {
        match self.layers.get_mut(&handle.0) {
            Some(layer) => {
                layer.placement = Some(placement);
                true
            }
            None => false,
        }
    }

    /// A layer by its handle binder.
    #[must_use]
    pub fn layer(&self, handle: BinderId) -> Option<&LayerRecord> {
        self.layers.get(&handle.0)
    }

    /// `true` when a **visible** window with `FLAG_SECURE` is on screen.
    ///
    /// `FLAG_SECURE` is the window manager's (`LayoutParams.flags`), so the
    /// compositor learns it from [`Self::set_layer_placement`] on every
    /// `relayout`; a layer the guest positions itself has no placement and can
    /// never be marked secure (so it cannot hide a secure window either). The
    /// live view publishes a blank frame while this is true instead of the app's
    /// pixels ([`FLAG_SECURE`]).
    #[must_use]
    pub fn has_secure_window(&self) -> bool {
        self.layers
            .values()
            .any(|layer| layer.placement.is_some_and(|placement| placement.secure) && !layer.hidden)
    }

    /// Every layer (callers should key by handle; the map order is not a
    /// contract).
    pub fn layers(&self) -> impl Iterator<Item = &LayerRecord> + '_ {
        self.layers.values()
    }

    /// The newest buffer for `handle`, the thing the live-frame capture reads.
    #[must_use]
    pub fn latest_buffer(&self, handle: BinderId) -> Option<&LatestBuffer> {
        self.layers.get(&handle.0).and_then(|l| l.latest.as_ref())
    }

    /// Reverse calls produced so far. The host drains them; tests can read them
    /// directly to assert the release callback without a guest.
    pub fn take_deliveries(&mut self) -> Vec<PendingDelivery> {
        std::mem::take(&mut self.deliveries)
    }

    fn take_handle(&mut self) -> Option<BinderId> {
        self.handle_pool.pop()
    }

    /// Create a layer record and return `(layer-handle binder, layer id)` (W5).
    ///
    /// This is the **one** allocation path for layers. `createSurface` uses it
    /// for the client's own layers (BLAST's `bbq-wrapper` child), and
    /// `IWindowSession.relayout` uses it to bind the window's `SurfaceControl`
    /// to a real record, so the parent handle BLAST passes to `createSurface`
    /// and every transaction the client applies against a layer handle land in
    /// this registry.
    ///
    /// `None` means the bounded handle pool is exhausted; the caller reports
    /// that as a structured error rather than inventing an id.
    pub fn create_layer(
        &mut self,
        name: &str,
        width: u32,
        height: u32,
        format: i32,
        flags: i32,
        parent: Option<BinderId>,
    ) -> Option<(BinderId, i32)> {
        let handle = self.take_handle()?;
        // A recycled handle is a *new* layer: it must not stay tombstoned.
        self.destroyed.remove(&handle.0);
        let layer_id = self.next_layer_id;
        self.next_layer_id += 1;
        let record = LayerRecord {
            handle,
            layer_id,
            name: name.to_string(),
            width,
            height,
            format,
            flags,
            parent,
            latest: None,
            latest_listener: BinderId(0),
            transactions: 0,
            posted_frames: Vec::new(),
            latest_generation: 0,
            geometry: LayerGeometry::default(),
            alpha: 1.0,
            placement: None,
            destination_frame: None,
            crop: None,
            buffer_crop: None,
            buffer_transform: 0,
            transform_to_display_inverse: false,
            matrix: [1.0, 0.0, 0.0, 1.0],
            hidden: false,
        };
        self.layers.insert(handle.0, record);
        self.stats.entry(handle.0).or_default();
        Some((handle, layer_id))
    }
}

/// The `ISurfaceComposerClient` node.
pub struct SurfaceClientService {
    state: Arc<Mutex<SurfaceState>>,
    display: DisplayDescriptor,
}

impl SurfaceClientService {
    /// Serve `state` for `display`.
    #[must_use]
    pub fn new(state: Arc<Mutex<SurfaceState>>, display: DisplayDescriptor) -> Self {
        Self { state, display }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, SurfaceState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl ServiceObject for SurfaceClientService {
    fn call(
        &mut self,
        interface: &str,
        method: &MethodInfo,
        _args: &[crate::value::Value],
    ) -> SResult<crate::host::CallResult> {
        Err(ServiceError::UnsupportedAndroidApi {
            interface: interface.to_string(),
            code: method.code,
            method: method.name.clone(),
        })
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }

    fn drain_raw_deliveries(&mut self) -> Vec<PendingDelivery> {
        self.lock().take_deliveries()
    }

    fn raw_transact(
        &mut self,
        interface: &str,
        code: u32,
        data: &mut Parcel,
        reply: &mut Parcel,
    ) -> SResult<bool> {
        if interface != CLIENT_DESCRIPTOR {
            return Ok(false);
        }
        match code {
            codes::CREATE_SURFACE => {
                let name = data.read_string16().map_err(perr)?.unwrap_or_default();
                let flags = data.read_int32().map_err(perr)?;
                let parent = data.read_strong_binder().map_err(perr)?;
                // `Parcel::writeParcelable(LayerMetadata)`: non-null marker,
                // then `count`, then `(uint32 key, byte[] value)` pairs.
                read_layer_metadata(data)?;

                let mut state = self.lock();
                let (handle, layer_id) = state
                    .create_layer(&name, 0, 0, 0, flags, parent)
                    .ok_or_else(|| ServiceError::BadRequest {
                        interface: CLIENT_DESCRIPTOR.to_string(),
                        method: "createSurface".into(),
                        detail: format!(
                            "layer-handle pool exhausted ({} handles); a real compositor allocates one per layer",
                            LAYER_HANDLE_POOL
                        ),
                    })?;

                exception::write_no_exception(reply);
                // `CreateSurfaceResult` is an AIDL parcelable: marker 1, then
                // the inline size word, then
                // IBinder handle, int layerId, String layerName, int transformHint.
                // The binder must be written directly into the reply, not
                // copied out of a scratch parcel (see `write_aidl_parcelable`).
                write_aidl_parcelable(reply, |p| {
                    write_create_surface_result(p, handle, layer_id, &name, 0)
                })?;
                Ok(true)
            }
            codes::CLEAR_LAYER_FRAME_STATS => {
                let handle = data.read_strong_binder().map_err(perr)?;
                if let Some(handle) = handle {
                    self.lock().stats.remove(&handle.0);
                    if let Some(layer) = self.lock().layers.get_mut(&handle.0) {
                        layer.posted_frames.clear();
                    }
                }
                exception::write_no_exception(reply);
                Ok(true)
            }
            codes::GET_LAYER_FRAME_STATS => {
                let handle = data.read_strong_binder().map_err(perr)?;
                let stats = handle
                    .and_then(|h| self.lock().stats.get(&h.0).cloned())
                    .unwrap_or_default();
                exception::write_no_exception(reply);
                write_frame_stats(reply, &stats, self.display.period_ns());
                Ok(true)
            }
            codes::MIRROR_SURFACE | codes::MIRROR_DISPLAY | codes::GET_SCHEDULING_POLICY => {
                Err(ServiceError::UnsupportedAndroidApi {
                    interface: CLIENT_DESCRIPTOR.to_string(),
                    code,
                    method: method_name(code)
                        .unwrap_or("mirror/getSchedulingPolicy")
                        .into(),
                })
            }
            _ => Err(ServiceError::UnsupportedAndroidApi {
                interface: CLIENT_DESCRIPTOR.to_string(),
                code,
                method: method_name(code).map_or_else(|| format!("code {code}"), str::to_string),
            }),
        }
    }
}

impl DisplayDescriptor {
    /// The 60 Hz vblank period the frame clock uses (kept here so the client
    /// node can answer `getLayerFrameStats.refreshPeriodNano` without importing
    /// the clock module).
    #[must_use]
    pub fn period_ns(self) -> i64 {
        crate::display_events::PERIOD_NS_60HZ
    }
}

/// Best-effort method name for the structured-unsupported record.
#[must_use]
pub fn method_name(code: u32) -> Option<&'static str> {
    Some(match code {
        codes::CREATE_SURFACE => "createSurface",
        codes::CLEAR_LAYER_FRAME_STATS => "clearLayerFrameStats",
        codes::GET_LAYER_FRAME_STATS => "getLayerFrameStats",
        codes::MIRROR_SURFACE => "mirrorSurface",
        codes::MIRROR_DISPLAY => "mirrorDisplay",
        codes::GET_SCHEDULING_POLICY => "getSchedulingPolicy",
        _ => return None,
    })
}

fn perr(e: impl std::fmt::Display) -> ServiceError {
    ServiceError::Internal(format!("{CLIENT_DESCRIPTOR}: parcel: {e}"))
}

/// Write an AIDL `Parcelable` body into `reply` as `(marker, size, fields...)`.
///
/// The size word has to be known before the body, so the fields are measured in
/// a scratch parcel first and then **written again into the reply through the
/// normal `Parcel` writers**. Copying the marshalled scratch bytes instead
/// (what C1 originally did) loses the reply parcel's object-table entry for the
/// binder: the guest's libbinder then reads a `flat_binder_object` at an offset
/// that is not in the object list and reports
/// `Attempt to read object ... that is not in the object list`, which is what
/// made `SurfaceComposerClient::createSurface` fail with `UNKNOWN_ERROR` while
/// every in-process `acos-parcel` test still passed (W5).
fn write_aidl_parcelable(
    reply: &mut Parcel,
    write: impl Fn(&mut Parcel) -> std::result::Result<(), acos_parcel::ParcelError>,
) -> SResult<()> {
    let mut scratch = Parcel::new();
    write(&mut scratch).map_err(perr)?;
    let size = scratch.data_size();
    reply.write_int32(1); // AIDL parcelable / typed-object marker
    reply.write_int32(size as i32);
    write(reply).map_err(perr)
}

/// Build the `CreateSurfaceResult` body (the fields after the size word).
fn write_create_surface_result(
    p: &mut Parcel,
    handle: BinderId,
    layer_id: i32,
    name: &str,
    transform_hint: u32,
) -> std::result::Result<(), acos_parcel::ParcelError> {
    p.write_strong_binder(handle);
    p.write_int32(layer_id);
    p.write_string16(name)?;
    p.write_uint32(transform_hint);
    Ok(())
}

fn write_words(parcel: &mut Parcel, bytes: &[u8]) {
    for word in bytes.chunks(4) {
        let mut buf = [0u8; 4];
        buf[..word.len()].copy_from_slice(word);
        parcel.write_int32(i32::from_le_bytes(buf));
    }
}

/// Encode `gui::FrameStats` as the client reads it: AIDL marker + size word,
/// `long refreshPeriod`, then three `long[]` (`desired`, `actual`,
/// `frameReady`).
fn write_frame_stats(parcel: &mut Parcel, stats: &FrameStats, period_ns: i64) {
    let mut body = Parcel::new();
    body.write_int64(period_ns);
    body.write_int64_array(Some(&stats.posted)).ok();
    body.write_int64_array(Some(&stats.actual)).ok();
    body.write_int64_array(Some(&stats.frame_ready)).ok();
    let bytes = body.marshall();
    parcel.write_int32(1);
    parcel.write_int32(bytes.len() as i32);
    write_words(parcel, &bytes);
}

/// Install the `ISurfaceComposerClient` node and return the shared state.
///
/// `createConnection` (`android.gui.ISurfaceComposer`, code 3) answers
/// `state.client_binder`; the composer node is installed by
/// [`crate::compositor_client::install`].
pub fn install(
    host: &mut crate::host::ServiceHost,
    display: DisplayDescriptor,
) -> Arc<Mutex<SurfaceState>> {
    // Pre-allocate handles because `createSurface` cannot register a binder
    // (`raw_transact` has no `&mut ServiceHost`). A bounded pool is honest: the
    // error says the pool is exhausted rather than fabricating an id.
    let pool = (0..LAYER_HANDLE_POOL)
        .map(|_| host.register_aux(LAYER_HANDLE_DESCRIPTOR))
        .collect::<Vec<_>>();
    let state = Arc::new(Mutex::new(SurfaceState::new(BinderId(0), pool, display)));
    let binder = host.register_dynamic(
        CLIENT_DESCRIPTOR,
        Box::new(SurfaceClientService::new(Arc::clone(&state), display)),
    );
    state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .client_binder = binder;
    host.set_compositor(Arc::clone(&state));
    state
}

/// Decode the legacy `SET_TRANSACTION_STATE` parcel and apply it to `state`.
///
/// This is called from [`crate::compositor_client::SurfaceComposerService`]
/// because the same `SurfaceFlinger` binder serves both the AIDL interface and
/// the legacy transaction path.
pub fn apply_transaction_state(
    state: &Arc<Mutex<SurfaceState>>,
    data: &mut Parcel,
    reply: &mut Parcel,
) -> SResult<bool> {
    let transaction = match read_transaction_state(data) {
        Ok(t) => t,
        Err(e) => {
            // Diagnostic (opt-in): dump the request parcel that failed to
            // decode so a wire-shape mismatch can be compared with the
            // decoder. Off by default: a failed transaction already produces a
            // structured `UNSUPPORTED_ANDROID_API` line.
            if std::env::var_os("ACOS_W5_DUMP").is_some() {
                let mut probe = data.clone();
                probe.set_data_position(0);
                let bytes = probe.marshall();
                eprintln!(
                    "ACOS W5: setTransactionState decode failed size={} at pos {}: {e}; bytes={:02x?}",
                    bytes.len(),
                    data.data_position(),
                    bytes
                );
            }
            return Err(e);
        }
    };
    let mut s = state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    s.transactions_decoded += 1;
    let latch_time = now_ns();
    let dbg = std::env::var_os("ACOS_W5_BUF").is_some();
    for composer in &transaction.composers {
        apply_layer_state(
            &mut s,
            composer,
            transaction.desired_present_time,
            latch_time,
            &transaction.listener_callbacks,
        );
    }
    for &(token, id) in &transaction.uncached_buffers {
        if id != 0 {
            s.uncache_buffer(token, id);
        }
    }
    if dbg {
        eprintln!(
            "ACOSDBG TXN t={latch_time} composers={} displays={} has_buffer={} total_transactions={} deliveries={}",
            transaction.composers.len(),
            transaction.displays,
            transaction
                .composers
                .iter()
                .any(|c| c.buffer.as_ref().is_some_and(|b| b.buffer.is_some())),
            s.transactions_decoded,
            s.deliveries.len()
        );
    }
    // The legacy path has no AIDL status header; the proxy ignores the reply.
    reply.reset();
    Ok(true)
}

/// One decoded `ComposerState` (the image's `layer_state_t`).
#[derive(Debug, Default)]
pub struct LayerState {
    /// `SurfaceControl` handle (the layer binder).
    pub surface: Option<BinderId>,
    /// `layerId`.
    pub layer_id: i32,
    /// Change flags.
    pub what: u64,
    /// X position.
    pub x: f32,
    /// Y position.
    pub y: f32,
    /// Z order.
    pub z: i32,
    /// Layer stack id.
    pub layer_stack: u32,
    /// Layer flags.
    pub flags: u32,
    /// Flag mask.
    pub mask: u32,
    /// 2x2 matrix.
    pub matrix: [f32; 4],
    /// Crop.
    pub crop: Rect,
    /// `transparentRegion` size (the body is skipped; this struct does not
    /// interpret regions).
    pub transparent_region_size: i32,
    /// Buffer transform.
    pub buffer_transform: u32,
    /// Transform-to-display-inverse.
    pub transform_to_display_inverse: bool,
    /// Dataspace.
    pub dataspace: u32,
    /// API.
    pub api: i32,
    /// Corner radius.
    pub corner_radius: f32,
    /// Background blur radius.
    pub background_blur_radius: u32,
    /// `bufferCrop`.
    pub buffer_crop: Rect,
    /// `destinationFrame`.
    pub destination_frame: Rect,
    /// Drop-input mode.
    pub drop_input_mode: u32,
    /// The buffer state, when `eBufferChanged` carried one.
    pub buffer: Option<BufferData>,
    /// `layer_state_t.listeners`.
    pub listeners: Vec<ListenerCallbacks>,
    /// `gui::WindowInfo`, when the transaction carried one (`null` for a plain
    /// BLAST child layer).
    pub window_info: Option<WindowInfo>,
    /// `layer_state_t.parentSurfaceControlForChild` when `eReparent` was set:
    /// `Some(layer handle)` for a new parent, `None` for a reparent **to null**
    /// (what `SurfaceControl::destroy()` applies before dropping the handle).
    pub reparent: Option<Option<BinderId>>,
}

/// The fields of `gui::WindowInfo` this compositor uses.
///
/// The image's struct is **older than r75** (no `contentSize`, no
/// `canOccludePresentation`); [`read_window_info`] consumes the whole shape and
/// keeps only what a C1 composite needs. `layoutParamsFlags` is where the
/// dialog's `FLAG_DIM_BEHIND` arrives, and `alpha` is the only alpha the image
/// puts on the wire — `layer_state_t` itself has no alpha field in this tag.
#[derive(Clone, Debug, PartialEq)]
pub struct WindowInfo {
    /// `WindowInfo.id` (the window id).
    pub id: i32,
    /// `WindowInfo.name` (String16).
    pub name: String,
    /// `WindowInfo.layoutParamsFlags`.
    pub layout_params_flags: i32,
    /// `WindowInfo.layoutParamsType`.
    pub layout_params_type: i32,
    /// `WindowInfo.frame` (the window frame on the display).
    pub frame: Rect,
    /// `WindowInfo.alpha`.
    pub alpha: f32,
}

/// One decoded `BufferData`.
#[derive(Debug)]
pub struct BufferData {
    /// Change flags.
    pub flags: i32,
    /// The `GraphicBuffer`, if present.
    pub buffer: Option<GraphicBufferMeta>,
    /// Frame number.
    pub frame_number: u64,
    /// `releaseBufferListener` binder.
    pub release_buffer_listener: BinderId,
    /// `releaseBufferEndpoint` binder.
    pub release_buffer_endpoint: BinderId,
    /// `cachedBuffer.token` (per-process `BufferCache` binder identity).
    pub cached_token: BinderId,
    /// `cachedBuffer.id`.
    pub cached_buffer_id: u64,
    /// `hasBarrier`.
    pub has_barrier: bool,
    /// `barrierFrameNumber`.
    pub barrier_frame_number: u64,
    /// `producerId`.
    pub producer_id: u32,
    /// The buffer's crop (from the layer state, retained for capture).
    pub crop: Option<Rect>,
}

/// The fields of a flattened `GraphicBuffer` this task needs.
#[derive(Debug)]
pub struct GraphicBufferMeta {
    /// The memfd (owned by the `BufferData`).
    pub fd: OwnedFd,
    /// Width.
    pub width: u32,
    /// Height.
    pub height: u32,
    /// Stride in pixels.
    pub stride: u32,
    /// Pixel format.
    pub format: i32,
    /// `GraphicBuffer.id`.
    pub id: u64,
    /// `usage` (the 64-bit value the image flattens as two words).
    pub usage: u64,
}

/// The decoded legacy transaction parcel.
#[derive(Debug, Default)]
pub struct TransactionState {
    /// `FrameTimelineInfo` size word (body skipped).
    pub frame_timeline_size: i32,
    /// `ComposerState[]`.
    pub composers: Vec<LayerState>,
    /// `DisplayState[]` count (bodies skipped; ACOS has no display transaction).
    pub displays: usize,
    /// `stateFlags`.
    pub flags: u32,
    /// `applyToken` (a binder the client uses as a token).
    pub apply_token: Option<BinderId>,
    /// `desiredPresentTime`.
    pub desired_present_time: i64,
    /// `isAutoTimestamp`.
    pub is_auto_timestamp: bool,
    /// `uncacheBuffers` count.
    pub uncache_buffers: usize,
    /// `(token, id)` entries from `uncacheBuffers`.
    pub uncached_buffers: Vec<(BinderId, u64)>,
    /// `hasListenerCallbacks`.
    pub has_listener_callbacks: bool,
    /// Top-level `listenerCallbacks`.
    pub listener_callbacks: Vec<ListenerCallbacks>,
    /// `transactionId`.
    pub transaction_id: u64,
    /// `mergedTransactionIds`.
    pub merged_transaction_ids: Vec<u64>,
}

fn apply_layer_state(
    state: &mut SurfaceState,
    layer: &LayerState,
    _desired_present_time: i64,
    latch_time: i64,
    top_level_listeners: &[ListenerCallbacks],
) {
    let Some(handle) = layer.surface.filter(|h| !h.is_null()) else {
        return;
    };
    // A destroyed layer must stay destroyed: a late transaction that still
    // names its handle is dropped (never resurrected with the auto-insert
    // below, which would draw stale pixels at the origin).
    if state.destroyed.contains(&handle.0) {
        if std::env::var_os("ACOSC1_TRACE").is_some() {
            eprintln!(
                "ACOSC1 ignored-transaction handle={:#x} layer={} what={:#x}",
                handle.0, layer.layer_id, layer.what
            );
        }
        return;
    }
    if std::env::var_os("ACOSC1_TRACE").is_some() {
        eprintln!(
            "ACOSC1 txn-layer handle={:#x} layer={} what={:#x} flags={:#x} mask={:#x} \
             buffer={} reparent={:?}",
            handle.0,
            layer.layer_id,
            layer.what,
            layer.flags,
            layer.mask,
            if layer.buffer.is_some() {
                "present"
            } else {
                "none"
            },
            layer.reparent
        );
        // W54: the full geometry a compositor must honour. `crop` is the
        // legacy `setCrop` rect, `buffer_crop` the BLAST/BBQ buffer crop in
        // **buffer** coordinates, `destination_frame` BLAST's `Rect(mSize)`.
        eprintln!(
            "ACOSC1 layer-state handle={:#x} layer={} pos=({},{}) z={} matrix={:?} crop={:?} \
             buffer_crop={:?} destination_frame={:?} buffer_dim={:?} corner_radius={}",
            handle.0,
            layer.layer_id,
            layer.x,
            layer.y,
            layer.z,
            layer.matrix,
            layer.crop,
            layer.buffer_crop,
            layer.destination_frame,
            layer
                .buffer
                .as_ref()
                .and_then(|b| b.buffer.as_ref())
                .map(|b| (b.width, b.height)),
            layer.corner_radius
        );
        eprintln!(
            "ACOSC1 layer-graphic handle={:#x} layer={} buffer={:?} cached={:?} frame={:?} \
             dataspace={} transform={} drop_input={}",
            handle.0,
            layer.layer_id,
            layer
                .buffer
                .as_ref()
                .and_then(|b| b.buffer.as_ref())
                .map(|m| (m.width, m.height, m.stride, m.format, m.id)),
            layer.buffer.as_ref().map(|b| b.cached_buffer_id),
            layer.buffer.as_ref().map(|b| b.frame_number),
            layer.dataspace,
            layer.buffer_transform,
            layer.drop_input_mode
        );
    }
    state.layers.entry(handle.0).or_insert_with(|| LayerRecord {
        handle,
        layer_id: layer.layer_id,
        name: format!("layer-{}", layer.layer_id),
        width: 0,
        height: 0,
        format: 0,
        flags: 0,
        parent: None,
        latest: None,
        latest_listener: BinderId(0),
        transactions: 0,
        posted_frames: Vec::new(),
        latest_generation: 0,
        geometry: LayerGeometry::default(),
        alpha: 1.0,
        placement: None,
        destination_frame: None,
        crop: None,
        buffer_crop: None,
        buffer_transform: 0,
        transform_to_display_inverse: false,
        matrix: [1.0, 0.0, 0.0, 1.0],
        hidden: false,
    });

    // `eFlagsChanged`: the client's own visibility bit. A stopped Activity's
    // ViewRootImpl hides its surface (`setVisibility(false)`) before the window
    // is removed, and a dialog's dim/close toggles it too.
    if layer.what & what::FLAGS != 0 {
        if let Some(record) = state.layers.get_mut(&handle.0) {
            record.hidden = layer.flags & LAYER_HIDDEN != 0;
        }
    }

    // `eReparent`: a *window surface* layer (one the window manager created and
    // placed) reparented to null is being destroyed — that is exactly what the
    // image's `SurfaceControl::destroy()` applies (`reparent(sc, nullptr)`) when
    // `ViewRootImpl` releases a destroyed Activity's surface. ACOS cannot see the
    // layer-handle binder die, so this is the observable destroy signal; the
    // layer's pixels and memfd must go with it. A client layer with no placement
    // is just detached (stock moves it to the display's root layer).
    if let Some(parent) = layer.reparent {
        let is_window_surface = state
            .layers
            .get(&handle.0)
            .is_some_and(|record| record.placement.is_some());
        if parent.is_none() && is_window_surface {
            state.destroy_layer_tree(handle);
            return;
        }
        if let Some(record) = state.layers.get_mut(&handle.0) {
            record.parent = parent;
        }
    }

    // The client's own geometry (W35): `Transaction.setPosition`/`setLayer`
    // land in `layer_state_t.x/y/z`. They are the honest fallback when no window
    // manager placement exists (a layer the guest owns directly); a window
    // layer's placement is pushed by `WmsState::publish_layer_placement`.
    if layer.what & (what::POSITION | what::LAYER) != 0 {
        if let Some(record) = state.layers.get_mut(&handle.0) {
            record.geometry = LayerGeometry {
                x: layer.x.round() as i32,
                y: layer.y.round() as i32,
                z: layer.z,
            };
        }
    }
    // B1b: the geometry inputs the AOSP kernel needs that C1 used to decode and
    // drop. Each field is latched only on its own change flag, exactly like
    // `RequestedLayerState::merge` (RequestedLayerState.cpp:311-317, 2420-2450
    // analogues); an unchanged field keeps its previous value.
    if let Some(record) = state.layers.get_mut(&handle.0) {
        if layer.what & what::DESTINATION_FRAME != 0 {
            record.destination_frame = rect_option(layer.destination_frame);
        }
        if layer.what & what::CROP != 0 {
            record.crop = rect_option(layer.crop);
        }
        if layer.what & what::BUFFER_CROP != 0 {
            record.buffer_crop = rect_option(layer.buffer_crop);
        }
        if layer.what & what::BUFFER_TRANSFORM != 0 {
            record.buffer_transform = layer.buffer_transform;
        }
        if layer.what & what::TRANSFORM_TO_DISPLAY_INVERSE != 0 {
            record.transform_to_display_inverse = layer.transform_to_display_inverse;
        }
        if layer.what & what::MATRIX != 0 {
            record.matrix = layer.matrix;
        }
    }
    if let Some(info) = &layer.window_info {
        if let Some(record) = state.layers.get_mut(&handle.0) {
            record.alpha = info.alpha.clamp(0.0, 1.0);
        }
    }

    // A transaction can name the same listener/callback id both on the layer
    // (`layer_state_t.listeners`) and at the top level
    // (`listenerCallbacks`). The client's `TransactionCompletedListener` keys
    // by `(listener, id)`, so collapse duplicates before queueing; otherwise a
    // synchronous transaction would fire the same callback twice.
    let mut callbacks: Vec<ListenerCallbacks> = Vec::new();
    for group in layer.listeners.iter().chain(top_level_listeners.iter()) {
        if let Some(existing) = callbacks.iter_mut().find(|c| c.listener == group.listener) {
            for cb in &group.callback_ids {
                if !existing.callback_ids.contains(cb) {
                    existing.callback_ids.push(*cb);
                }
            }
        } else {
            callbacks.push(group.clone());
        }
    }

    // The previous buffer is released only once the replacement is applied;
    // this is what returns a slot to the producer's BufferQueue.
    if std::env::var_os("ACOS_W5_BUF").is_some() {
        if let Some(b) = &layer.buffer {
            eprintln!(
                "ACOSDBG BUFFERDATA flags={:#x} has_buffer={} frame={} cached_token={:#x} cached_id={} has_barrier={} barrier_frame={} producer={}",
                b.flags,
                b.buffer.is_some(),
                b.frame_number,
                b.cached_token.0,
                b.cached_buffer_id,
                b.has_barrier,
                b.barrier_frame_number,
                b.producer_id
            );
        }
    }
    let mut release: Option<(BinderId, ReleaseCallbackId)> = None;
    let mut latched_frame_number = 0u64;
    if let Some(buffer) = &layer.buffer {
        // The client strips a `GraphicBuffer` it has already sent from later
        // parcels: `Transaction::cacheBuffers()` replaces it with a
        // `client_cache_t` (`{token, id}`) whose `id` **is** the `GraphicBuffer`
        // id and whose `token` identifies the client process's `BufferCache`.
        let resolved: Option<CachedBuffer> = match &buffer.buffer {
            Some(meta) => {
                let cache_gen = SurfaceState::cache_buffer(state, buffer.cached_token, meta);
                Some(CachedBuffer {
                    fd: try_clone(meta),
                    width: meta.width,
                    height: meta.height,
                    stride: meta.stride,
                    format: meta.format,
                    id: meta.id,
                    cache_gen,
                })
            }
            None if buffer.cached_buffer_id != 0 => state
                .buffer_cache
                .get(&(buffer.cached_token, buffer.cached_buffer_id))
                .and_then(CachedBuffer::duplicate),
            None => None,
        };
        if let Some(CachedBuffer {
            fd: Some(fd),
            width: cached_width,
            height: cached_height,
            stride: cached_stride,
            format: cached_format,
            id: cached_id,
            cache_gen,
        }) = resolved
        {
            // The generation is reserved before the record is borrowed mutably:
            // a stored buffer always advances C1's monotonic store counter.
            state.buffer_generations = state.buffer_generations.saturating_add(1);
            let generation = state.buffer_generations;
            let record = state.layers.get_mut(&handle.0).expect("inserted above");
            let previous = record.latest.take();
            let old_listener = record.latest_listener;
            let listener = if !buffer.release_buffer_listener.is_null() {
                buffer.release_buffer_listener
            } else {
                old_listener
            };
            if let Some(previous) = &previous {
                let target = if !old_listener.is_null() {
                    old_listener
                } else {
                    listener
                };
                if !target.is_null() {
                    release = Some((target, previous.release_id()));
                }
            }
            latched_frame_number = buffer.frame_number;
            record.latest = Some(LatestBuffer {
                fd,
                width: cached_width,
                height: cached_height,
                stride: cached_stride,
                format: cached_format,
                frame_number: buffer.frame_number,
                buffer_id: cached_id,
                cached_token: buffer.cached_token,
                cache_gen,
                producer_id: buffer.producer_id,
                crop: buffer.crop.or(Some(layer.buffer_crop)),
            });
            record.latest_generation = generation;
            if std::env::var_os("ACOS_W6_DUMP").is_some() {
                if let (Some(dir), Some(stored)) =
                    (std::env::var_os("ACOS_W6_DUMP"), record.latest.as_ref())
                {
                    dump_latest_buffer(record.layer_id, stored, std::path::Path::new(&dir));
                }
            }
            if std::env::var_os("ACOS_W5_BUF").is_some() {
                eprintln!(
                    "ACOSDBG BUFFER layer={} frame={} {}x{} stride={} fmt={} id={}{}",
                    record.layer_id,
                    buffer.frame_number,
                    cached_width,
                    cached_height,
                    cached_stride,
                    cached_format,
                    cached_id,
                    if buffer.buffer.is_none() {
                        " (from client cache)"
                    } else {
                        ""
                    }
                );
            }
            record.latest_listener = listener;
            record.transactions += 1;
            record.posted_frames.push(buffer.frame_number);
            if record.posted_frames.len() > 32 {
                record.posted_frames.remove(0);
            }
            let stats = state.stats.entry(handle.0).or_default();
            stats.posted.push(buffer.frame_number as i64);
            stats.frame_ready.push(buffer.frame_number as i64);
            if stats.posted.len() > 32 {
                stats.posted.remove(0);
                stats.frame_ready.remove(0);
            }
        }
    } else if let Some(record) = state.layers.get_mut(&handle.0) {
        record.transactions += 1;
    }

    // The completion stats carry two different frame numbers and the client
    // (`BLASTBufferQueue`) uses both:
    //
    // * `FrameEventHistoryStats.frameNumber` is the frame **this transaction
    //   latched** (`SurfaceFlinger`'s `Layer::setTransactionCompletedListeners`
    //   copies `mDrawingState.frameNumber`, the *new* buffer's number).
    //   `BLASTBufferQueue::transactionCommittedCallback` removes exactly that
    //   number from `mSyncedFrameNumbers` and only flushes the shadow queue
    //   once the set is empty, and `transactionCallback` releases every
    //   submitted buffer whose frame is **older** than it
    //   (`currFrameNumber > key.framenumber`, `fakeRelease`). Reporting the
    //   replaced (one-behind) buffer's number here suppresses both: no slot of
    //   a merged/dropped frame is ever returned, the acquired set only grows,
    //   and the producer starves once `2 + maxAcquired` buffers are allocated
    //   (W11: with maxAcquired 2 the fourth allocation is followed by a dequeue
    //   that never returns).
    // * `previousReleaseCallbackId` is the buffer this transaction replaced
    //   (`Layer::mPreviousReleaseCallbackId`), i.e. the one the client may now
    //   recycle.
    let previous_id = release.map_or(
        ReleaseCallbackId {
            buffer_id: 0,
            frame_number: 0,
        },
        |(_, id)| id,
    );

    // Stock `SurfaceFlinger` reports the replaced buffer inside the completion
    // stats (`previousReleaseCallbackId`); it sends a standalone
    // `onReleaseBuffer` only for a buffer it releases without such a
    // transaction (`Layer::callReleaseBufferCallback`). Sending both would pop
    // the client's release callback twice (the second pop finds nothing and
    // logs "buffer not found"), so pick the same path stock does.
    let stats_carry_the_release = callbacks
        .iter()
        .any(|c| !c.listener.is_null() && !c.callback_ids.is_empty());
    if let Some((target, id)) = release {
        if std::env::var_os("ACOS_W5_BUF").is_some() {
            eprintln!(
                "ACOSDBG RELEASE target={:#x} buffer={} frame={} via={}",
                target.0,
                id.buffer_id,
                id.frame_number,
                if stats_carry_the_release {
                    "stats"
                } else {
                    "onReleaseBuffer"
                }
            );
        }
        if !stats_carry_the_release {
            state.deliveries.push(on_release_buffer_delivery(
                target,
                id,
                state.max_acquired_buffers,
            ));
        }
    }
    // One `onTransactionCompleted` per listener that registered callback ids.
    for listener in callbacks {
        if listener.listener.is_null() || listener.callback_ids.is_empty() {
            continue;
        }
        if std::env::var_os("ACOS_W5_BUF").is_some() {
            eprintln!(
                "ACOSDBG COMPLETED target={:#x} ids={:?} layer={:#x}",
                listener.listener.0,
                listener
                    .callback_ids
                    .iter()
                    .map(|c| c.id)
                    .collect::<Vec<_>>(),
                handle.0
            );
        }
        state.deliveries.push(on_transaction_completed_delivery(
            listener.listener,
            &listener.callback_ids,
            handle,
            latched_frame_number,
            &previous_id,
            latch_time,
            state.max_acquired_buffers,
        ));
    }
}

fn try_clone(meta: &GraphicBufferMeta) -> Option<OwnedFd> {
    // `dup` can fail on a descriptor limit (EMFILE); the caller treats that as
    // "no frame stored", never as a panic in the host.
    meta.fd.try_clone().ok()
}

/// The display-space rectangle a layer draws into (**W54**).
///
/// `crop` is the buffer region the layer shows, `p` its placement (a window
/// layer's placement is `frame − surfaceInsets`, pushed by the window
/// manager). The result is clipped to the display viewport, which is
/// what SurfaceFlinger does for every layer
/// (`Layer::setGeometry`/`composeSurfaces` intersect with the display clip); a
/// window surface is `frame + surfaceInsets` so it deliberately overhangs the
/// display edge. `None` when nothing of the layer is on the display.
#[must_use]
pub fn placed_rect(
    crop: (u32, u32, u32, u32),
    placement: &LayerPlacement,
    display: DisplayDescriptor,
) -> Option<(i64, i64, i64, i64)> {
    let (crop_left, crop_top, width, height) = crop;
    let x0 = i64::from(placement.x) + i64::from(crop_left);
    let y0 = i64::from(placement.y) + i64::from(crop_top);
    let clipped_x0 = x0.max(0);
    let clipped_y0 = y0.max(0);
    let clipped_x1 = (x0 + i64::from(width)).min(i64::from(display.width.max(0)));
    let clipped_y1 = (y0 + i64::from(height)).min(i64::from(display.height.max(0)));
    (clipped_x1 > clipped_x0 && clipped_y1 > clipped_y0)
        .then_some((clipped_x0, clipped_y0, clipped_x1, clipped_y1))
}

/// The buffer region a kernel snapshot draws: stock `getBufferCrop()`
/// (`RequestedLayerState.cpp:481-494`), which is already intersected with the
/// buffer bounds. `(left, top, width, height)`.
fn source_crop(snapshot: &Snapshot, buffer: &LatestBuffer) -> (u32, u32, u32, u32) {
    let [left, top, right, bottom] = snapshot.buffer_crop;
    let left = left.max(0).unsigned_abs().min(buffer.width);
    let top = top.max(0).unsigned_abs().min(buffer.height);
    let right = right.max(0).unsigned_abs().clamp(left, buffer.width);
    let bottom = bottom.max(0).unsigned_abs().clamp(top, buffer.height);
    if right <= left || bottom <= top {
        (0, 0, buffer.width, buffer.height)
    } else {
        (left, top, right - left, bottom - top)
    }
}

/// Transform a rectangle with the kernel's composed transform, min/max of the
/// four corners exactly like `ui::Transform::transform(const FloatRect&)`
/// (`Transform.cpp:281-303`).
fn transform_rect(transform: &[f32; 9], rect: (f32, f32, f32, f32)) -> (f32, f32, f32, f32) {
    let (left, top, right, bottom) = rect;
    let corners = [(left, top), (right, top), (left, bottom), (right, bottom)];
    let mut min_x = f32::INFINITY;
    let mut min_y = f32::INFINITY;
    let mut max_x = f32::NEG_INFINITY;
    let mut max_y = f32::NEG_INFINITY;
    for (x, y) in corners {
        // `Transform::transform(const vec2&)` (Transform.cpp:229-236); geom is
        // column-major, so geom[0]=dsdx, geom[3]=dtdx, geom[6]=tx.
        let tx = transform[0] * x + transform[3] * y + transform[6];
        let ty = transform[1] * x + transform[4] * y + transform[7];
        min_x = min_x.min(tx);
        min_y = min_y.min(ty);
        max_x = max_x.max(tx);
        max_y = max_y.max(ty);
    }
    (min_x, min_y, max_x, max_y)
}

/// The kernel's display-space rectangle for one layer, clipped to the layer's
/// `transformedBounds` but **not** to the display: the blit clips per pixel, so
/// `x0` may be negative (a surface-inset window overhangs the display, W54).
/// Keeping the unclipped rectangle is what makes the source:destination scale
/// correct; clipping first would squeeze the buffer into the visible part.
fn placed_rect_from_kernel(
    snapshot: &Snapshot,
    source: (u32, u32, u32, u32),
) -> (i64, i64, i64, i64) {
    let (left, top, width, height) = source;
    let dest = transform_rect(
        &snapshot.geom_transform,
        (
            left as f32,
            top as f32,
            (left + width) as f32,
            (top + height) as f32,
        ),
    );
    let bounds = snapshot.transformed_bounds;
    // `ui::Transform::transform(const Rect&)` rounds to nearest
    // (`Transform.cpp:258-276`); finding the same ints for 1:1 placement keeps
    // W10-W54 frames byte-identical.
    let round = |v: f32| (v + 0.5).floor() as i64;
    let x0 = round(dest.0.max(bounds[0]));
    let y0 = round(dest.1.max(bounds[1]));
    let x1 = round(dest.2.min(bounds[2]));
    let y1 = round(dest.3.min(bounds[3]));
    (x0, y0, x1, y1)
}

/// Clip a display-space rectangle to the display viewport (W54: the canvas is
/// never larger than the display). `None` means the layer draws nothing.
fn clip_to_display(
    rect: (i64, i64, i64, i64),
    display: DisplayDescriptor,
) -> Option<(i64, i64, i64, i64)> {
    let clipped = (
        rect.0.max(0),
        rect.1.max(0),
        rect.2.min(i64::from(display.width.max(0))),
        rect.3.min(i64::from(display.height.max(0))),
    );
    (clipped.2 > clipped.0 && clipped.3 > clipped.1).then_some(clipped)
}

/// Copy one layer's stored buffer out of its memfd.
///
/// The bytes are read through **this process's own dup** of the guest's
/// `GraphicBuffer` fd, never from a device framebuffer. A short read is not a
/// frame (`None`), and the geometry is bounded before it becomes an allocation.
fn read_buffer_pixels(buffer: &LatestBuffer) -> Option<Vec<u8>> {
    use std::os::unix::fs::FileExt;

    let bytes = checked_buffer_bytes(buffer.width, buffer.height, buffer.stride)?;
    let size = usize::try_from(bytes).ok()?;
    if size == 0 {
        return None;
    }
    let owned = rustix::io::dup(buffer.fd()).ok()?;
    let file = std::fs::File::from(owned);
    let mut pixels = vec![0u8; size];
    let mut read = 0usize;
    while read < size {
        match file.read_at(&mut pixels[read..], read as u64) {
            Ok(0) | Err(_) => break,
            Ok(n) => read += n,
        }
    }
    (read == size).then_some(pixels)
}

/// FNV-1a over the pixels plus the count of pixels with any non-zero byte.
fn hash_pixels(pixels: &[u8]) -> (u64, usize) {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    let mut non_zero = 0usize;
    for px in pixels.chunks_exact(4) {
        for b in px {
            hash ^= u64::from(*b);
            hash = hash.wrapping_mul(0x0100_0000_01b3);
        }
        if px.iter().any(|b| *b != 0) {
            non_zero += 1;
        }
    }
    (hash, non_zero)
}

/// Darken the pixels already drawn by `amount` (`0.0..=1.0`), the window
/// manager's dim layer for a window with `FLAG_DIM_BEHIND`.
fn dim_canvas(canvas: &mut [u8], amount: f32) {
    let keep = (1.0 - amount).clamp(0.0, 1.0);
    for px in canvas.chunks_exact_mut(4) {
        for channel in &mut px[..3] {
            *channel = (f32::from(*channel) * keep).round().clamp(0.0, 255.0) as u8;
        }
    }
}

/// One layer the composite will draw, with the kernel snapshot that decided it.
struct Placed<'a> {
    row: &'a KernelRow<'a>,
    snapshot: Snapshot,
    /// Source region in raw buffer pixels.
    source: (u32, u32, u32, u32),
    /// Display-space destination `(x0, y0, x1, y1)`, already clipped.
    dest: (i64, i64, i64, i64),
}

/// Where one layer's pixels go on the canvas.
struct DrawSpec {
    /// Source region in buffer pixels `(left, top, width, height)`.
    crop: (u32, u32, u32, u32),
    /// Canvas column of the source region's left edge.
    dst_x: i64,
    /// Canvas row of the source region's top edge.
    dst_y: i64,
    /// Destination width in canvas pixels (`== crop.2` on the 1:1 path).
    dst_w: u32,
    /// Destination height in canvas pixels (`== crop.3` on the 1:1 path).
    dst_h: u32,
    /// Layer alpha, `0..=255`.
    alpha: u32,
}

/// Blend one layer's pixels into the canvas, source-over, alpha-weighted.
///
/// A destination that is not the source size is sampled **nearest-neighbour**
/// (deterministic, no extra state; B1b's `destinationFrame` scale path). The
/// 1:1 path below is the pre-B1b loop byte for byte.
fn blend_layer(
    canvas: &mut [u8],
    row: usize,
    canvas_width: u32,
    canvas_height: u32,
    pixels: &[u8],
    buffer: &LatestBuffer,
    spec: &DrawSpec,
) {
    let (crop_left, crop_top, crop_width, crop_height) = spec.crop;
    let stride = buffer.stride as usize;
    let one_to_one = spec.dst_w == crop_width && spec.dst_h == crop_height;
    for dy in 0..spec.dst_h as usize {
        let y = spec.dst_y + dy as i64;
        if y < 0 || y >= i64::from(canvas_height) {
            continue;
        }
        let src_y = if one_to_one {
            crop_top as usize + dy
        } else {
            crop_top as usize
                + (dy as u64 * u64::from(crop_height) / u64::from(spec.dst_h.max(1))) as usize
        };
        let src_row = src_y * stride * 4;
        let dst_row = y as usize * row;
        for dx in 0..spec.dst_w as usize {
            let x = spec.dst_x + dx as i64;
            if x < 0 || x >= i64::from(canvas_width) {
                continue;
            }
            let src_x = if one_to_one {
                crop_left as usize + dx
            } else {
                crop_left as usize
                    + (dx as u64 * u64::from(crop_width) / u64::from(spec.dst_w.max(1))) as usize
            };
            let src = src_row + src_x * 4;
            let Some(src_px) = pixels.get(src..src + 4) else {
                return;
            };
            // The layer's own alpha times the pixel's alpha, per the image's
            // premultiplied-free RGBA_8888 layout.
            let a = spec.alpha * u32::from(src_px[3]) / 255;
            let dst = dst_row + x as usize * 4;
            let dst_px = &mut canvas[dst..dst + 4];
            if a >= 255 {
                dst_px.copy_from_slice(src_px);
                continue;
            }
            let inv = 255 - a;
            for channel in 0..3 {
                dst_px[channel] = ((u32::from(src_px[channel]) * a
                    + u32::from(dst_px[channel]) * inv)
                    / 255) as u8;
            }
            dst_px[3] = (a + u32::from(dst_px[3]) * inv / 255) as u8;
        }
    }
}

/// `stride * 4 * height`, checked, and `None` when the geometry is outside the
/// S7 bounds (dimension/stride ceilings, byte ceiling, or arithmetic overflow).
fn checked_buffer_bytes(width: u32, height: u32, stride: u32) -> Option<u64> {
    if width > MAX_BUFFER_DIMENSION
        || height > MAX_BUFFER_DIMENSION
        || stride > MAX_BUFFER_STRIDE
        || (width != 0 && stride < width)
    {
        return None;
    }
    let row = u64::from(stride).checked_mul(4)?;
    let bytes = row.checked_mul(u64::from(height))?;
    (bytes <= MAX_BUFFER_BYTES).then_some(bytes)
}

/// W6 diagnostic: copy the stored buffer's pixels out of the memfd that C1 kept.
///
/// The bytes are read through **this process's own dup** of the guest's
/// `GraphicBuffer` fd (`LatestBuffer::fd`), so a dump is evidence that the
/// buffer reached C1 in full, not a copy the producer made. Writes
/// `<dir>/frame-<n>.rgba` (stride × height × 4) plus a `.meta` line with the
/// dimensions, the FNV-1a of the pixels and the count of non-zero bytes (a
/// blank buffer has 0). Diagnostic only: nothing is dumped unless the env var
/// names a directory.
fn dump_latest_buffer(layer_id: i32, buf: &LatestBuffer, dir: &std::path::Path) {
    use std::io::Write;
    use std::os::unix::fs::FileExt;

    let Some(size) = checked_buffer_bytes(buf.width, buf.height, buf.stride)
        .and_then(|b| usize::try_from(b).ok())
    else {
        return;
    };
    if size == 0 {
        return;
    }
    let Ok(owned) = rustix::io::dup(buf.fd()) else {
        eprintln!("ACOSW6 dump: dup fd failed");
        return;
    };
    let file = std::fs::File::from(owned);
    let mut pixels = vec![0u8; size];
    let mut read = 0usize;
    while read < size {
        match file.read_at(&mut pixels[read..], read as u64) {
            Ok(0) | Err(_) => break,
            Ok(n) => read += n,
        }
    }
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    let mut non_zero = 0usize;
    let mut distinct = std::collections::HashSet::new();
    for px in pixels[..read].chunks_exact(4) {
        for b in px {
            hash ^= u64::from(*b);
            hash = hash.wrapping_mul(0x0100_0000_01b3);
        }
        if px.iter().any(|b| *b != 0) {
            non_zero += 1;
        }
        distinct.insert([px[0], px[1], px[2], px[3]]);
    }
    if let Err(e) = std::fs::create_dir_all(dir) {
        eprintln!("ACOSW6 dump: {e}");
        return;
    }
    let stem = format!("frame-{}", buf.frame_number);
    let rgba = dir.join(format!("{stem}.rgba"));
    if let Err(e) = std::fs::write(&rgba, &pixels[..read]) {
        eprintln!("ACOSW6 dump: {e}");
        return;
    }
    let meta = format!(
        "layer={layer_id} frame={} width={} height={} stride={} format={} bytes={} \
         fnv1a={hash:016x} non_zero_pixels={non_zero} distinct_pixels={}\n",
        buf.frame_number,
        buf.width,
        buf.height,
        buf.stride,
        buf.format,
        read,
        distinct.len()
    );
    let _ = std::fs::File::create(dir.join(format!("{stem}.meta")))
        .and_then(|mut f| f.write_all(meta.as_bytes()));
    eprintln!("ACOSW6 dump {}", meta.trim_end());
}

/// Build `onReleaseBuffer(ReleaseCallbackId, Fence::NO_FENCE, maxAcquired)`.
fn on_release_buffer_delivery(
    target: BinderId,
    id: ReleaseCallbackId,
    max_acquired: u32,
) -> PendingDelivery {
    let mut data = Parcel::new();
    data.write_interface_token(TRANSACTION_LISTENER_DESCRIPTOR)
        .ok();
    // Parcelable `ReleaseCallbackId`: marker then two uint64s.
    data.write_int32(1);
    data.write_uint64(id.buffer_id);
    data.write_uint64(id.frame_number);
    // `Fence::NO_FENCE`: Flattenable write = len 4, fd count 0, body {0}.
    data.write_int32(4);
    data.write_int32(0);
    data.write_int32(0);
    data.write_uint32(max_acquired);
    data.set_data_position(0);
    PendingDelivery {
        target,
        interface: TRANSACTION_LISTENER_DESCRIPTOR.into(),
        code: listener_codes::ON_RELEASE_BUFFER,
        method: "onReleaseBuffer".into(),
        data,
    }
}

/// Build `onTransactionCompleted(ListenerStats)` for one listener.
///
/// `latched_frame_number` is the frame this transaction latched (the new
/// buffer's `frameNumber`), `previous` is the buffer it replaced. See the
/// comment in [`apply_layer_state`] for why the two are not interchangeable.
fn on_transaction_completed_delivery(
    target: BinderId,
    callback_ids: &[CallbackId],
    handle: BinderId,
    latched_frame_number: u64,
    previous: &ReleaseCallbackId,
    latch_time: i64,
    max_acquired: u32,
) -> PendingDelivery {
    let mut data = Parcel::new();
    data.write_interface_token(TRANSACTION_LISTENER_DESCRIPTOR)
        .ok();
    // Parcelable `ListenerStats`: marker, int32 count of TransactionStats.
    data.write_int32(1);
    data.write_int32(1);
    // Parcelable TransactionStats.
    data.write_int32(1);
    data.write_int32(callback_ids.len() as i32);
    for cb in callback_ids {
        data.write_int32(1);
        data.write_int64(cb.id);
        data.write_int32(cb.kind);
    }
    data.write_int64(latch_time);
    data.write_bool(false); // no present fence
                            // ParcelableVector<SurfaceStats>: one stat for this layer.
    data.write_int32(1);
    data.write_int32(1);
    data.write_strong_binder(handle);
    data.write_bool(false); // acquire time, not a fence
    data.write_int64(latch_time);
    data.write_bool(false); // previous release fence
    data.write_bool(false); // no transform hint
    data.write_uint32(max_acquired);
    // Parcelable FrameEventHistoryStats (image: one uint64 + fence bool + 5 int64).
    // The image has `frametimestamps_previousrelease` off, so the number here is
    // the frame this transaction **latched**, not the one it replaced.
    data.write_int32(1);
    data.write_uint64(latched_frame_number);
    data.write_bool(false);
    for _ in 0..5 {
        data.write_int64(0);
    }
    data.write_int32(0); // no jank data
                         // Parcelable ReleaseCallbackId (previous buffer).
    data.write_int32(1);
    data.write_uint64(previous.buffer_id);
    data.write_uint64(previous.frame_number);
    data.set_data_position(0);
    PendingDelivery {
        target,
        interface: TRANSACTION_LISTENER_DESCRIPTOR.into(),
        code: listener_codes::ON_TRANSACTION_COMPLETED,
        method: "onTransactionCompleted".into(),
        data,
    }
}

// ---------------------------------------------------------------------------
// transaction decoding
// ---------------------------------------------------------------------------

/// Decode one legacy `SET_TRANSACTION_STATE` parcel (positioned after the
/// interface token). Public for the byte-exact tests; the host goes through
/// [`apply_transaction_state`].
pub fn decode_transaction_state(data: &mut Parcel) -> SResult<TransactionState> {
    read_transaction_state(data)
}

fn read_transaction_state(data: &mut Parcel) -> SResult<TransactionState> {
    let start = data.data_position();
    let mut probe = data.clone();
    let size_word = probe.read_int32().map_err(perr)?;
    let body = usize::try_from(size_word.max(0)).unwrap_or(0);
    // `FrameTimelineInfo` is an AIDL parcelable written directly (no presence
    // marker): an `int32` size word then the fields. The image's writer counts
    // the size word **itself** (measured from a live `Transaction::apply`:
    // word 28, fields `int64 vsyncId, int32 inputEventId, int64
    // startTimeNanos, bool` = 24; with 24 the composer count and the layer's
    // `flat_binder_object` line up, with 28 the binder read starts on the
    // object's flags word). Hand-built parcels (C1's tests, and a future
    // writer that flattens body-only) use the body length, so both
    // interpretations are tried and the one that consumes the parcel exactly
    // wins. Before W5 the decoder always skipped the raw word, which put every
    // `setTransactionState` four bytes late and made the buffer transaction
    // undecodable.
    let mut candidates = vec![body.saturating_sub(4), body];
    candidates.dedup();
    let mut last_error: Option<ServiceError> = None;
    let mut inexact: Option<TransactionState> = None;
    for skip in candidates {
        data.set_data_position(start);
        let frame_timeline_size = data.read_int32().map_err(perr)?;
        match read_transaction_state_body(data, frame_timeline_size, skip) {
            Ok(t) if data.data_avail() == 0 => return Ok(t),
            Ok(t) => {
                if std::env::var_os("ACOS_W5_DUMP").is_some() {
                    eprintln!(
                        "ACOSDBG timeline skip={skip} size_word={frame_timeline_size} parses but leaves {} bytes",
                        data.data_avail()
                    );
                }
                inexact = inexact.or(Some(t));
            }
            Err(e) => {
                if std::env::var_os("ACOS_W5_DUMP").is_some() {
                    eprintln!(
                        "ACOSDBG timeline skip={skip} size_word={frame_timeline_size} failed at {}: {e}",
                        data.data_position()
                    );
                }
                last_error = Some(e);
            }
        }
    }
    if let Some(t) = inexact {
        return Ok(t);
    }
    Err(last_error.unwrap_or_else(|| {
        ServiceError::Internal(format!("{CLIENT_DESCRIPTOR}: empty transaction parcel"))
    }))
}

/// The body of a transaction after `FrameTimelineInfo` (see the caller for the
/// size-word rule).
fn read_transaction_state_body(
    data: &mut Parcel,
    frame_timeline_size: i32,
    timeline_body: usize,
) -> SResult<TransactionState> {
    skip_bytes(data, i32::try_from(timeline_body).unwrap_or(0))?;

    let composer_count = data.read_uint32().map_err(perr)?;
    if composer_count > MAX_COMPOSERS {
        return Err(bad_transaction(format!(
            "composer count {composer_count} exceeds {MAX_COMPOSERS}"
        )));
    }
    let mut composers = Vec::with_capacity(composer_count.min(64) as usize);
    for _ in 0..composer_count {
        composers.push(read_layer_state(data)?);
    }

    let display_count = data.read_uint32().map_err(perr)?;
    if display_count > MAX_DISPLAYS {
        return Err(bad_transaction(format!(
            "display count {display_count} exceeds {MAX_DISPLAYS}"
        )));
    }
    for _ in 0..display_count {
        read_display_state(data)?;
    }

    let dbg = std::env::var_os("ACOS_W5_TAIL").is_some();
    if dbg {
        eprintln!("ACOSDBG tail displays at {}", data.data_position());
    }
    let flags = data.read_uint32().map_err(perr)?;
    let apply_token = data.read_strong_binder().map_err(perr)?;
    if dbg {
        eprintln!("ACOSDBG tail iwc at {}", data.data_position());
    }
    read_input_window_commands(data)?;
    if dbg {
        eprintln!("ACOSDBG tail desired at {}", data.data_position());
    }
    let desired_present_time = data.read_int64().map_err(perr)?;
    let is_auto_timestamp = data.read_bool().map_err(perr)?;
    if dbg {
        eprintln!("ACOSDBG tail uncache at {}", data.data_position());
    }

    let uncache_count = data.read_uint32().map_err(perr)?;
    if uncache_count > MAX_UNCACHE_BUFFERS {
        return Err(bad_transaction(format!(
            "uncacheBuffers count {uncache_count} exceeds {MAX_UNCACHE_BUFFERS}"
        )));
    }
    let uncache_count = uncache_count as usize;
    let mut uncached_buffers = Vec::with_capacity(uncache_count.min(64));
    for _ in 0..uncache_count {
        let token = data
            .read_strong_binder()
            .map_err(perr)?
            .unwrap_or(BinderId(0));
        let id = data.read_uint64().map_err(perr)?;
        uncached_buffers.push((token, id));
    }

    if dbg {
        eprintln!("ACOSDBG tail listeners at {}", data.data_position());
    }
    let has_listener_callbacks = data.read_bool().map_err(perr)?;
    let listener_count = data.read_int32().map_err(perr)?;
    if listener_count > MAX_LISTENERS {
        return Err(bad_transaction(format!(
            "listener count {listener_count} exceeds {MAX_LISTENERS}"
        )));
    }
    let mut listener_callbacks = Vec::new();
    for _ in 0..listener_count.max(0) {
        let listener = data
            .read_strong_binder()
            .map_err(perr)?
            .unwrap_or(BinderId(0));
        let callback_ids = read_callback_id_vector(data)?;
        listener_callbacks.push(ListenerCallbacks {
            listener,
            callback_ids,
        });
    }

    if dbg {
        eprintln!("ACOSDBG tail txnid at {}", data.data_position());
    }
    let transaction_id = data.read_uint64().map_err(perr)?;
    let merged_count = data.read_uint32().map_err(perr)? as usize;
    let mut merged_transaction_ids = Vec::with_capacity(merged_count.min(64));
    for _ in 0..merged_count {
        merged_transaction_ids.push(data.read_uint64().map_err(perr)?);
    }

    Ok(TransactionState {
        frame_timeline_size,
        composers,
        displays: display_count as usize,
        flags,
        apply_token,
        desired_present_time,
        is_auto_timestamp,
        uncache_buffers: uncache_count,
        uncached_buffers,
        has_listener_callbacks,
        listener_callbacks,
        transaction_id,
        merged_transaction_ids,
    })
}

fn read_layer_state(data: &mut Parcel) -> SResult<LayerState> {
    if std::env::var_os("ACOS_W5_DUMP").is_some() {
        let start = data.data_position();
        let mut probe = data.clone();
        let mut bytes = Vec::new();
        for _ in 0..64 {
            match probe.read_byte() {
                Ok(b) => bytes.push(b),
                Err(_) => break,
            }
        }
        eprintln!(
            "ACOSDBG layer_state size={} at {start}: {bytes:02x?}",
            data.data_size()
        );
    }
    let surface = data.read_strong_binder().map_err(perr)?;
    let layer_id = data.read_int32().map_err(perr)?;
    let what = data.read_uint64().map_err(perr)?;
    let x = data.read_float().map_err(perr)?;
    let y = data.read_float().map_err(perr)?;
    let z = data.read_int32().map_err(perr)?;
    let layer_stack = data.read_uint32().map_err(perr)?;
    let flags = data.read_uint32().map_err(perr)?;
    let mask = data.read_uint32().map_err(perr)?;
    let matrix = [
        data.read_float().map_err(perr)?,
        data.read_float().map_err(perr)?,
        data.read_float().map_err(perr)?,
        data.read_float().map_err(perr)?,
    ];
    let crop = read_rect(data)?;

    // `SurfaceControl::readNullableFromParcel` twice: first
    // `relativeLayerSurfaceControl`, then `parentSurfaceControlForChild` (the
    // eReparent target). Both carry (client binder, handle binder, int32
    // layerId, String name, uint32 transformHint, uint32 width, uint32 height,
    // uint32 format).
    let _relative_parent = read_nullable_surface_control(data)?;
    let parent_handle = read_nullable_surface_control(data)?;
    let reparent = (what & what::REPARENT != 0).then_some(parent_handle);
    let dbg = std::env::var_os("ACOS_W5_LS").is_some();
    if dbg {
        eprintln!("ACOSDBG ls nsc at {}", data.data_position());
    }

    for _ in 0..4 {
        let _ = data.read_float().map_err(perr)?;
    }
    if dbg {
        eprintln!("ACOSDBG ls color at {}", data.data_position());
    }
    let window_info = read_window_info(data)?;
    if dbg {
        eprintln!("ACOSDBG ls windowinfo at {}", data.data_position());
    }

    let transparent_region_size = data.read_int32().map_err(perr)?;
    skip_bytes(data, transparent_region_size)?;
    if dbg {
        eprintln!("ACOSDBG ls region at {}", data.data_position());
    }
    let buffer_transform = data.read_uint32().map_err(perr)?;
    let transform_to_display_inverse = data.read_bool().map_err(perr)?;
    let _border_enabled = data.read_bool().map_err(perr)?;
    let _border_width = data.read_float().map_err(perr)?;
    for _ in 0..4 {
        let _ = data.read_float().map_err(perr)?;
    }
    let dataspace = data.read_uint32().map_err(perr)?;

    // HdrMetadata is a variable-size `LightFlattenable`: int32 size then body.
    let hdr_size = data.read_int32().map_err(perr)?;
    skip_bytes(data, hdr_size)?;
    // `surfaceDamageRegion`: same shape.
    let damage_size = data.read_int32().map_err(perr)?;
    skip_bytes(data, damage_size)?;

    let api = data.read_int32().map_err(perr)?;
    if data.read_bool().map_err(perr)? {
        // `NativeHandle::flatten` writes `int32 numFds, int32 numInts`, then
        // inline ints and (outside the body) the descriptors. No path in the
        // launch set uses a sideband stream; consume the shape so the rest of
        // the parcel stays aligned.
        let num_fds = data.read_int32().map_err(perr)?;
        let num_ints = data.read_int32().map_err(perr)?;
        for _ in 0..num_ints.max(0) {
            let _ = data.read_int32().map_err(perr)?;
        }
        for _ in 0..num_fds.max(0) {
            let _ = data.read_file_descriptor().map_err(perr)?;
        }
    }

    // `mat4` (16 floats) is a fixed-size LightFlattenable.
    for _ in 0..16 {
        let _ = data.read_float().map_err(perr)?;
    }
    let corner_radius = data.read_float().map_err(perr)?;
    let background_blur_radius = data.read_uint32().map_err(perr)?;

    read_layer_metadata(data)?;
    if dbg {
        eprintln!("ACOSDBG ls metadata at {}", data.data_position());
    }

    for _ in 0..4 {
        let _ = data.read_float().map_err(perr)?;
    }
    let _ = data.read_uint32().map_err(perr)?; // bgColorDataspace
    let _ = data.read_bool().map_err(perr)?; // colorSpaceAgnostic

    let listener_count = data.read_int32().map_err(perr)?;
    if listener_count > MAX_LISTENERS {
        return Err(bad_transaction(format!(
            "layer listener count {listener_count} exceeds {MAX_LISTENERS}"
        )));
    }
    let mut listeners = Vec::new();
    for _ in 0..listener_count.max(0) {
        let listener = data
            .read_strong_binder()
            .map_err(perr)?
            .unwrap_or(BinderId(0));
        let callback_ids = read_callback_id_vector(data)?;
        listeners.push(ListenerCallbacks {
            listener,
            callback_ids,
        });
    }

    if dbg {
        eprintln!("ACOSDBG ls listeners at {}", data.data_position());
    }
    let _shadow_radius = data.read_float().map_err(perr)?;
    let _frame_rate_selection_priority = data.read_int32().map_err(perr)?;
    let _frame_rate = data.read_float().map_err(perr)?;
    // The image's frame-rate group is **three** bytes, not r75's four bytes plus
    // `frameRateCategorySmoothSwitchOnly` (bool) and `frameRateSelectionStrategy`
    // (byte). Measured from `layer_state_t::{write,read}` in
    // `libgui.so` (`writeByte` @0xd1c56/0xd1c6e/0xd1c86) and from the captured
    // parcels (fixtures README, body 448..464). r75 would read 8 bytes too many
    // here, which is why every real BLAST transaction failed to decode.
    let _ = data.read_byte().map_err(perr)?;
    let _ = data.read_byte().map_err(perr)?;
    let _ = data.read_byte().map_err(perr)?;
    let _ = data.read_uint32().map_err(perr)?; // fixedTransformHint
    let _ = data.read_bool().map_err(perr)?; // autoRefresh
    let _ = data.read_bool().map_err(perr)?; // dimmingEnabled

    let blur_region_count = data.read_uint32().map_err(perr)?;
    for _ in 0..blur_region_count.min(1024) {
        let _ = data.read_uint32().map_err(perr)?;
        for _ in 0..5 {
            let _ = data.read_float().map_err(perr)?;
        }
        for _ in 0..4 {
            let _ = data.read_int32().map_err(perr)?;
        }
    }

    if dbg {
        eprintln!("ACOSDBG ls blur at {}", data.data_position());
    }
    // `StretchEffect` is fixed-size (6 floats + 4-float bounds = 40 bytes).
    for _ in 0..10 {
        let _ = data.read_float().map_err(perr)?;
    }
    let buffer_crop = read_rect(data)?;
    let destination_frame = read_rect(data)?;
    let _is_trusted_overlay = data.read_bool().map_err(perr)?;
    let drop_input_mode = data.read_uint32().map_err(perr)?;
    if dbg {
        eprintln!("ACOSDBG ls trusted at {}", data.data_position());
    }

    let has_buffer_data = data.read_bool().map_err(perr)?;
    if dbg {
        eprintln!(
            "ACOSDBG ls hasbuf={has_buffer_data} at {}",
            data.data_position()
        );
    }
    let buffer = if has_buffer_data {
        Some(read_buffer_data(data, buffer_crop)?)
    } else {
        None
    };
    if dbg {
        eprintln!("ACOSDBG ls buffer at {}", data.data_position());
    }

    // Parcelable TrustedPresentationThresholds (AIDL: marker + size + payload).
    read_aidl_parcelable(data)?;
    if dbg {
        eprintln!("ACOSDBG ls tpt at {}", data.data_position());
    }
    // Parcelable TrustedPresentationListener (C++: marker + binder + int32).
    //
    // The binder is **null** in the real BLAST transactions, and libbinder
    // appends a 4-byte `Stability::Level` record after a null binder. A null
    // binder is never in the parcel's object table, so the peer's import cannot
    // strip that record (`crates/acos-binder-peer` strips four bytes only after
    // table-listed binder objects) and instead marks the parcel
    // `wire_null_stability`; `read_strong_binder` consumes the record. W8 read
    // that word as an image-only field; it is the stability record (fixed by W9:
    // reading it twice put `displayCount`/`applyToken` four bytes late, which is
    // why the captured 700-byte transactions failed at the tail).
    let _present = data.read_int32().map_err(perr)?;
    let _listener = data.read_strong_binder().map_err(perr)?;
    let _id = data.read_int32().map_err(perr)?;
    if dbg {
        eprintln!("ACOSDBG ls tpl2 at {}", data.data_position());
    }

    if dbg {
        eprintln!("ACOSDBG ls tpl at {}", data.data_position());
    }
    let _current_hdr_sdr_ratio = data.read_float().map_err(perr)?;
    let _desired_hdr_sdr_ratio = data.read_float().map_err(perr)?;
    let _caching_hint = data.read_int32().map_err(perr)?;

    Ok(LayerState {
        surface,
        layer_id,
        what,
        x,
        y,
        z,
        layer_stack,
        flags,
        mask,
        matrix,
        crop,
        transparent_region_size,
        buffer_transform,
        transform_to_display_inverse,
        dataspace,
        api,
        corner_radius,
        background_blur_radius,
        buffer_crop,
        destination_frame,
        drop_input_mode,
        buffer,
        listeners,
        window_info,
        reparent,
    })
}

fn read_display_state(data: &mut Parcel) -> SResult<()> {
    let _token = data.read_strong_binder().map_err(perr)?;
    let _surface = data.read_strong_binder().map_err(perr)?;
    for _ in 0..4 {
        let _ = data.read_uint32().map_err(perr)?;
    }
    read_rect(data)?;
    read_rect(data)?;
    let _width = data.read_uint32().map_err(perr)?;
    let _height = data.read_uint32().map_err(perr)?;
    Ok(())
}

fn read_input_window_commands(data: &mut Parcel) -> SResult<()> {
    // `writeParcelableVector(focusRequests)`: int32 count then marker+size+body.
    let focus_count = data.read_int32().map_err(perr)?;
    for _ in 0..focus_count.max(0) {
        read_aidl_parcelable(data)?;
    }
    // `int32 listenerCount` then binders.
    let listener_count = data.read_int32().map_err(perr)?;
    for _ in 0..listener_count.max(0) {
        let _ = data.read_strong_binder().map_err(perr)?;
    }
    Ok(())
}

fn read_buffer_data(data: &mut Parcel, crop: Rect) -> SResult<BufferData> {
    // `Parcel::writeParcelable(BufferData)`: non-null marker first.
    let present = data.read_int32().map_err(perr)?;
    if present == 0 {
        return Err(ServiceError::BadRequest {
            interface: LEGACY_COMPOSER_DESCRIPTOR.to_string(),
            method: "setTransactionState".into(),
            detail: "null BufferData in a present-buffer transaction".into(),
        });
    }
    let dbg = std::env::var_os("ACOS_W5_BD").is_some();
    let flags = data.read_int32().map_err(perr)?;
    let has_buf = data.read_bool().map_err(perr)?;
    if dbg {
        eprintln!(
            "ACOSDBG bd flags at {} hasbuf={has_buf}",
            data.data_position()
        );
    }
    let buffer = if has_buf {
        Some(read_graphic_buffer(data)?)
    } else {
        None
    };
    if dbg {
        eprintln!("ACOSDBG bd gb at {}", data.data_position());
    }
    let has_fence = data.read_bool().map_err(perr)?;
    if dbg {
        eprintln!(
            "ACOSDBG bd fenceflag={has_fence} at {}",
            data.data_position()
        );
    }
    if has_fence {
        // An acquire fence: `Parcel::write(Fence)`.
        read_fence(data)?;
        if dbg {
            eprintln!("ACOSDBG bd fence at {}", data.data_position());
        }
    }
    let frame_number = data.read_uint64().map_err(perr)?;
    if dbg {
        eprintln!(
            "ACOSDBG bd frame={frame_number} at {}",
            data.data_position()
        );
    }
    let release_buffer_listener = data
        .read_strong_binder()
        .map_err(perr)?
        .unwrap_or(BinderId(0));
    if dbg {
        eprintln!("ACOSDBG bd listener at {}", data.data_position());
    }
    let release_buffer_endpoint = data
        .read_strong_binder()
        .map_err(perr)?
        .unwrap_or(BinderId(0));
    if dbg {
        eprintln!("ACOSDBG bd endpoint at {}", data.data_position());
    }
    let cached_token = data
        .read_strong_binder()
        .map_err(perr)?
        .unwrap_or(BinderId(0));
    if dbg {
        eprintln!("ACOSDBG bd cachedtok at {}", data.data_position());
    }
    let cached_buffer_id = data.read_uint64().map_err(perr)?;
    let has_barrier = data.read_bool().map_err(perr)?;
    let barrier_frame_number = data.read_uint64().map_err(perr)?;
    let producer_id = data.read_uint32().map_err(perr)?;
    if dbg {
        eprintln!("ACOSDBG bd producer at {}", data.data_position());
    }
    Ok(BufferData {
        flags,
        buffer,
        frame_number,
        release_buffer_listener,
        release_buffer_endpoint,
        cached_token,
        cached_buffer_id,
        has_barrier,
        barrier_frame_number,
        producer_id,
        crop: Some(crop),
    })
}

/// Obtain an owned descriptor for a number that arrived in a peer frame
/// (`SCM_RIGHTS`) but that the imported `Parcel` does not own.
///
/// `acos-binder-peer::import_parcel_raw` rewrites a `BINDER_TYPE_FD` object's id
/// to the received descriptor and then imports the data with
/// `Parcel::unmarshall`, which does not build an fd table, so
/// [`Parcel::borrowed_file_descriptor`] misses. The number is live in this
/// process while the frame is being handled (the peer closes a received frame's
/// descriptors only after it has handled the frame). Re-opening
/// `/proc/self/fd/<n>` is a safe way to duplicate it from this crate, which is
/// `#![forbid(unsafe_code)]`; a `GraphicBuffer` is a memfd, which supports that
/// reopen. Anything else (closed number, non-reopenable descriptor) is an error
/// the caller turns into a structured failure — never a blank buffer.
fn received_file_descriptor(raw: RawFd) -> std::io::Result<OwnedFd> {
    let file = std::fs::File::open(format!("/proc/self/fd/{raw}"))?;
    Ok(file.into())
}

fn read_graphic_buffer(data: &mut Parcel) -> SResult<GraphicBufferMeta> {
    let len = data.read_int32().map_err(perr)?;
    let fd_count = data.read_int32().map_err(perr)?;
    if std::env::var_os("ACOS_W5_BD").is_some() {
        eprintln!(
            "ACOSDBG gb header len={len} fdc={fd_count} at {}",
            data.data_position()
        );
    }
    if len < 13 * 4 || fd_count < 0 {
        return Err(ServiceError::BadRequest {
            interface: LEGACY_COMPOSER_DESCRIPTOR.to_string(),
            method: "setTransactionState".into(),
            detail: format!("bad GraphicBuffer flatten header len={len} fds={fd_count}"),
        });
    }
    // The header length and fd count are guest-declared sizes: bound them before
    // they become an allocation (`vec![0i32; len/4]`) or a read loop.
    if len % 4 != 0 || len > MAX_GRAPHIC_BUFFER_HEADER_BYTES {
        return Err(bad_transaction(format!(
            "GraphicBuffer header len {len} is outside 52..={MAX_GRAPHIC_BUFFER_HEADER_BYTES} bytes"
        )));
    }
    if fd_count > MAX_GRAPHIC_BUFFER_FDS {
        return Err(bad_transaction(format!(
            "GraphicBuffer fd count {fd_count} exceeds {MAX_GRAPHIC_BUFFER_FDS}"
        )));
    }
    let words = len as usize / 4;
    let mut header = vec![0i32; words];
    for w in &mut header {
        *w = data.read_int32().map_err(perr)?;
    }
    if std::env::var_os("ACOS_W5_BD").is_some() {
        eprintln!("ACOSDBG gb len={len} fdc={fd_count} words={header:?}");
    }
    let magic = header[0];
    // The image's `libui` writes the magic word in the reverse byte order of the
    // r75 tag: the bytes `31 30 42 47` (uint32 `0x47423031`), where a tag-built
    // `'GB01'` literal is `47 42 30 31` (`0x31304247`). Both are accepted; any
    // other value is still rejected (never guess past a bad header).
    let tag_gb01 = i32::from_le_bytes(*b"GB01");
    let image_gb01 = i32::from_be_bytes(*b"GB01");
    let tag_gbfr = i32::from_le_bytes(*b"GBFR");
    if magic != tag_gb01 && magic != image_gb01 && magic != tag_gbfr {
        return Err(ServiceError::BadRequest {
            interface: LEGACY_COMPOSER_DESCRIPTOR.to_string(),
            method: "setTransactionState".into(),
            detail: format!("unknown GraphicBuffer magic 0x{magic:08x}"),
        });
    }
    let width = header[1] as u32;
    let height = header[2] as u32;
    let stride = header[3] as u32;
    let format = header[4];
    if checked_buffer_bytes(width, height, stride).is_none() {
        return Err(bad_transaction(format!(
            "GraphicBuffer geometry {width}x{height} stride {stride} is outside the accepted \
             {MAX_BUFFER_DIMENSION}px / {MAX_BUFFER_STRIDE}px / {MAX_BUFFER_BYTES}-byte bounds \
             (or overflows)"
        )));
    }
    let usage_lo = header[6] as u32 as u64;
    let id = ((header[7] as u32 as u64) << 32) | (header[8] as u32 as u64);
    let usage_hi = if magic == tag_gbfr {
        0
    } else {
        header[12] as u32 as u64
    };
    let mut fd: Option<OwnedFd> = None;
    for _ in 0..fd_count {
        if let Some(raw) = data.read_file_descriptor().map_err(perr)? {
            // Normally the parcel owns the descriptor (`borrowed_file_descriptor`
            // finds it). A parcel imported by `acos-binder-peer` has its
            // `BINDER_TYPE_FD` object id rewritten to the descriptor that arrived
            // in `SCM_RIGHTS`, but the import goes through `Parcel::unmarshall`,
            // which does not build an owned fd table — so fall back to dup'ing the
            // number directly. The peer keeps a received frame's descriptors open
            // until it has handled the frame, so the number is live for exactly
            // this call; `dup` reads only the number. If it is not a live
            // descriptor the dup fails and this stays a structured error (never a
            // silently zero-filled buffer).
            let cloned = match data.borrowed_file_descriptor(raw) {
                Some(borrowed) => borrowed.try_clone_to_owned().map_err(|e| e.to_string()),
                None => received_file_descriptor(raw).map_err(|e| e.to_string()),
            };
            let cloned = cloned
                .map_err(|e| ServiceError::Internal(format!("dup(GraphicBuffer fd {raw}): {e}")))?;
            // Type check: a frame must be an anonymous (memfd/unlinked) regular
            // file the producer owns, never a live path or a non-file descriptor.
            // Otherwise a guest could make the compositor read a named file (its
            // own APK, a framework file) or a pipe/socket/device as "pixels".
            validate_guest_buffer_fd(raw)?;
            fd = Some(cloned);
        }
    }
    let fd = fd.ok_or_else(|| ServiceError::BadRequest {
        interface: LEGACY_COMPOSER_DESCRIPTOR.to_string(),
        method: "setTransactionState".into(),
        detail: "GraphicBuffer had no fd (gralloc shim should send one memfd)".into(),
    })?;
    Ok(GraphicBufferMeta {
        fd,
        width,
        height,
        stride,
        format,
        id,
        usage: usage_lo | (usage_hi << 32),
    })
}

/// Reject a descriptor that is not an anonymous regular file.
///
/// The guest sends `BINDER_TYPE_FD` numbers and the compositor duplicates them
/// by number (`/proc/self/fd/<n>`). Without a type check that reopen is a
/// confused deputy: it would happily read a directory, a pipe, a socket (logd,
/// binder) or a **linked** file inside the guest's view (the app's own APK, a
/// framework resource) and store its bytes as a "frame". A `GraphicBuffer` is
/// always a `memfd_create()` object (the ACOS gralloc shim) or an unlinked
/// scratch file, so the accepted set is: regular file **and** an anonymous path
/// (`/memfd:...` or `... (deleted)`).
fn validate_guest_buffer_fd(raw: RawFd) -> SResult<()> {
    if raw < 0 {
        return Err(bad_transaction(format!(
            "GraphicBuffer fd number {raw} is negative"
        )));
    }
    let proc_path = format!("/proc/self/fd/{raw}");
    let meta = std::fs::metadata(&proc_path).map_err(|e| {
        ServiceError::Internal(format!("stat({proc_path}) for a GraphicBuffer fd: {e}"))
    })?;
    if !meta.is_file() {
        return Err(bad_transaction(format!(
            "GraphicBuffer fd {raw} is not a regular file (mode {:o})",
            std::os::unix::fs::MetadataExt::mode(&meta)
        )));
    }
    let target = std::fs::read_link(&proc_path).map_err(|e| {
        ServiceError::Internal(format!("readlink({proc_path}) for a GraphicBuffer fd: {e}"))
    })?;
    let shown = target.to_string_lossy();
    if !(shown.starts_with("/memfd:") || shown.ends_with(" (deleted)")) {
        return Err(bad_transaction(format!(
            "GraphicBuffer fd {raw} names a live path ({shown}); a frame must be an anonymous \
             memfd/unlinked regular file"
        )));
    }
    // Keep the borrow honest: the descriptor we validated is the descriptor the
    // caller stores.
    Ok(())
}

/// A malformed guest `setTransactionState` (bounds, type or shape).
fn bad_transaction(detail: String) -> ServiceError {
    ServiceError::BadRequest {
        interface: LEGACY_COMPOSER_DESCRIPTOR.to_string(),
        method: "setTransactionState".into(),
        detail,
    }
}

fn read_fence(data: &mut Parcel) -> SResult<()> {
    let len = data.read_int32().map_err(perr)?;
    let fd_count = data.read_int32().map_err(perr)?;
    if len < 4 {
        return Err(ServiceError::Internal(format!("bad Fence len {len}")));
    }
    let _num_fds = data.read_int32().map_err(perr)?;
    skip_bytes(data, len - 4)?;
    for _ in 0..fd_count.max(0) {
        let _ = data.read_file_descriptor().map_err(perr)?;
    }
    Ok(())
}

fn read_nullable_surface_control(data: &mut Parcel) -> SResult<Option<BinderId>> {
    if !data.read_bool().map_err(perr)? {
        return Ok(None);
    }
    let _client = data.read_strong_binder().map_err(perr)?;
    let handle = data.read_strong_binder().map_err(perr)?;
    let _layer_id = data.read_int32().map_err(perr)?;
    let _name = data.read_string16().map_err(perr)?;
    let _transform_hint = data.read_uint32().map_err(perr)?;
    let _width = data.read_uint32().map_err(perr)?;
    let _height = data.read_uint32().map_err(perr)?;
    let _format = data.read_uint32().map_err(perr)?;
    Ok(handle)
}

/// The image's `gui::WindowInfo` (older than r75: no `contentSize`, no
/// trailing `canOccludePresentation`). Returns `None` for the `int32 0` "no
/// window info" marker a BLAST child layer's transaction carries.
fn read_window_info(data: &mut Parcel) -> SResult<Option<WindowInfo>> {
    if data.read_int32().map_err(perr)? == 0 {
        return Ok(None);
    }
    let _token = data.read_strong_binder().map_err(perr)?;
    let _dispatching_timeout = data.read_int64().map_err(perr)?;
    let id = data.read_int32().map_err(perr)?;
    let name = data.read_string16().map_err(perr)?.unwrap_or_default();
    let layout_params_flags = data.read_int32().map_err(perr)?;
    let layout_params_type = data.read_int32().map_err(perr)?;
    let frame = read_rect(data)?;
    let _surface_inset = data.read_int32().map_err(perr)?;
    let _global_scale = data.read_float().map_err(perr)?;
    let alpha = data.read_float().map_err(perr)?;
    for _ in 0..6 {
        let _ = data.read_float().map_err(perr)?; // transform
    }
    let _touch_occlusion_mode = data.read_int32().map_err(perr)?;
    let _owner_pid = data.read_int32().map_err(perr)?;
    let _owner_uid = data.read_int32().map_err(perr)?;
    let _package_name = data.read_string16().map_err(perr)?;
    let _input_config = data.read_int32().map_err(perr)?;
    let _display_id = data.read_int32().map_err(perr)?;
    // `InputApplicationInfo` is written by `writeToParcel` directly (not via
    // `writeParcelable`), so its AIDL size word is the first thing here — there
    // is **no** non-null marker in front of it.
    read_sized_blob(data)?;
    let region_size = data.read_int32().map_err(perr)?;
    skip_bytes(data, region_size)?;
    let _replace = data.read_bool().map_err(perr)?;
    let _crop_handle = data.read_strong_binder().map_err(perr)?;
    let _window_token = data.read_strong_binder().map_err(perr)?;
    let _focus_transfer_target = data.read_strong_binder().map_err(perr)?;
    Ok(Some(WindowInfo {
        id,
        name,
        layout_params_flags,
        layout_params_type,
        frame,
        alpha,
    }))
}

fn read_layer_metadata(data: &mut Parcel) -> SResult<()> {
    let present = data.read_int32().map_err(perr)?;
    if present == 0 {
        return Ok(());
    }
    let count = data.read_int32().map_err(perr)?;
    for _ in 0..count.max(0) {
        let _key = data.read_uint32().map_err(perr)?;
        let len = data.read_int32().map_err(perr)?;
        skip_padded(data, len)?;
    }
    Ok(())
}

fn read_callback_id_vector(data: &mut Parcel) -> SResult<Vec<CallbackId>> {
    let count = data.read_int32().map_err(perr)?;
    // A guest-declared count must not size the allocation before any element
    // has been read (`with_capacity(i32::MAX)` is a 34 GiB reservation).
    if count > MAX_CALLBACK_IDS {
        return Err(bad_transaction(format!(
            "callback id count {count} exceeds {MAX_CALLBACK_IDS}"
        )));
    }
    let mut out = Vec::with_capacity(count.max(0) as usize);
    for _ in 0..count.max(0) {
        let present = data.read_int32().map_err(perr)?;
        if present == 0 {
            continue;
        }
        let id = data.read_int64().map_err(perr)?;
        let kind = data.read_int32().map_err(perr)?;
        out.push(CallbackId { id, kind });
    }
    Ok(out)
}

/// Read an AIDL parcelable as `marker, int32 size, payload`.
///
/// A generated AIDL `writeToParcel` size word **counts itself** (`size >= 4`, the
/// reader rejects anything smaller and then seeks to `start + size`), so the
/// payload is `size - 4` bytes. Measured on the image's
/// `TrustedPresentationThresholds` (`blast_txn_1028.bin` body 816: size 16 with a
/// 12-byte payload) and on the same shape in `FocusRequest`.
fn read_aidl_parcelable(data: &mut Parcel) -> SResult<()> {
    let present = data.read_int32().map_err(perr)?;
    if present == 0 {
        return Ok(());
    }
    let size = data.read_int32().map_err(perr)?;
    if size < 4 {
        return Err(ServiceError::BadRequest {
            interface: LEGACY_COMPOSER_DESCRIPTOR.to_string(),
            method: "setTransactionState".into(),
            detail: format!("AIDL parcelable size {size} cannot include its own size word"),
        });
    }
    skip_bytes(data, size - 4)
}

/// Read `int32 size` then skip that many bytes (a direct AIDL `writeToParcel`
/// call, with no outer `Parcelable` marker).
fn read_sized_blob(data: &mut Parcel) -> SResult<()> {
    let size = data.read_int32().map_err(perr)?;
    skip_bytes(data, size)
}

fn read_rect(data: &mut Parcel) -> SResult<Rect> {
    Ok(Rect {
        left: data.read_int32().map_err(perr)?,
        top: data.read_int32().map_err(perr)?,
        right: data.read_int32().map_err(perr)?,
        bottom: data.read_int32().map_err(perr)?,
    })
}

/// Skip `bytes` of already-aligned parcel data. Every skipped blob in this
/// format is 4-byte aligned (AIDL payloads, `LightFlattenable` bodies and
/// `Region` rects); a non-multiple is a malformed parcel, not a fudge.
fn skip_bytes(data: &mut Parcel, bytes: i32) -> SResult<()> {
    if bytes < 0 {
        return Err(ServiceError::BadRequest {
            interface: LEGACY_COMPOSER_DESCRIPTOR.to_string(),
            method: "setTransactionState".into(),
            detail: format!("negative inline blob length {bytes}"),
        });
    }
    let words = (bytes as usize).div_ceil(4);
    for _ in 0..words {
        let _ = data.read_int32().map_err(perr)?;
    }
    Ok(())
}

/// Skip a byte vector payload (`int32 len` already read) with 4-byte padding.
fn skip_padded(data: &mut Parcel, bytes: i32) -> SResult<()> {
    skip_bytes(data, bytes)
}
