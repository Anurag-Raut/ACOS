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
pub const LAYER_HANDLE_POOL: usize = 16;

/// `layer_state_t` change flags used by this task.
pub mod what {
    /// `ePositionChanged`.
    pub const POSITION: u64 = 0x0000_0001;
    /// `eLayerChanged`.
    pub const LAYER: u64 = 0x0000_0002;
    /// `eAlphaChanged`.
    pub const ALPHA: u64 = 0x0000_0008;
    /// `eBufferChanged`.
    pub const BUFFER: u64 = 0x0020_0000;
    /// `eDataspaceChanged`.
    pub const DATASPACE: u64 = 0x0080_0000;
}

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
    /// `getMaxAcquiredBufferCount()` — the same 2 the compositor node reports.
    pub max_acquired_buffers: u32,
    /// Number of legacy transaction parcels decoded.
    pub transactions_decoded: u64,
}

impl SurfaceState {
    fn new(client_binder: BinderId, handle_pool: Vec<BinderId>) -> Self {
        Self {
            client_binder,
            layers: HashMap::new(),
            handle_pool,
            stats: HashMap::new(),
            deliveries: Vec::new(),
            next_layer_id: 1,
            max_acquired_buffers: 2,
            transactions_decoded: 0,
        }
    }

    /// A layer by its handle binder.
    #[must_use]
    pub fn layer(&self, handle: BinderId) -> Option<&LayerRecord> {
        self.layers.get(&handle.0)
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
                    method: method_name(code).unwrap_or("mirror/getSchedulingPolicy").into(),
                })
            }
            _ => Err(ServiceError::UnsupportedAndroidApi {
                interface: CLIENT_DESCRIPTOR.to_string(),
                code,
                method: method_name(code)
                    .map_or_else(|| format!("code {code}"), str::to_string),
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
    let state = Arc::new(Mutex::new(SurfaceState::new(BinderId(0), pool)));
    let binder = host.register_dynamic(
        CLIENT_DESCRIPTOR,
        Box::new(SurfaceClientService::new(Arc::clone(&state), display)),
    );
    state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .client_binder = binder;
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
    });

    // A transaction can name the same listener/callback id both on the layer
    // (`layer_state_t.listeners`) and at the top level
    // (`listenerCallbacks`). The client's `TransactionCompletedListener` keys
    // by `(listener, id)`, so collapse duplicates before queueing; otherwise a
    // synchronous transaction would fire the same callback twice.
    let mut callbacks: Vec<ListenerCallbacks> = Vec::new();
    for group in layer.listeners.iter().chain(top_level_listeners.iter()) {
        if let Some(existing) = callbacks
            .iter_mut()
            .find(|c| c.listener == group.listener)
        {
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
    let mut release: Option<(BinderId, ReleaseCallbackId)> = None;
    if let Some(buffer) = &layer.buffer {
        if let Some(meta) = &buffer.buffer {
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
            record.latest = Some(LatestBuffer {
                fd: try_clone(meta),
                width: meta.width,
                height: meta.height,
                stride: meta.stride,
                format: meta.format,
                frame_number: buffer.frame_number,
                buffer_id: meta.id,
                producer_id: buffer.producer_id,
                crop: buffer.crop.or(Some(layer.buffer_crop)),
            });
            if std::env::var_os("ACOS_W6_DUMP").is_some() {
                if let (Some(dir), Some(stored)) =
                    (std::env::var_os("ACOS_W6_DUMP"), record.latest.as_ref())
                {
                    dump_latest_buffer(record.layer_id, stored, std::path::Path::new(&dir));
                }
            }
            if std::env::var_os("ACOS_W5_BUF").is_some() {
                eprintln!(
                    "ACOSDBG BUFFER layer={} frame={} {}x{} stride={} fmt={} id={}",
                    record.layer_id,
                    buffer.frame_number,
                    meta.width,
                    meta.height,
                    meta.stride,
                    meta.format,
                    meta.id
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
            stats
                .frame_ready
                .push(buffer.frame_number as i64);
            if stats.posted.len() > 32 {
                stats.posted.remove(0);
                stats.frame_ready.remove(0);
            }
        }
    } else if let Some(record) = state.layers.get_mut(&handle.0) {
        record.transactions += 1;
    }

    // `previousReleaseCallbackId` in the completion stats must be the buffer
    // this transaction replaced (not the new one), so take it from the release
    // decision made above.
    let previous_id = release.map_or(
        ReleaseCallbackId {
            buffer_id: 0,
            frame_number: 0,
        },
        |(_, id)| id,
    );

    if let Some((target, id)) = release {
        if std::env::var_os("ACOS_W5_BUF").is_some() {
            eprintln!(
                "ACOSDBG RELEASE target={:#x} buffer={} frame={}",
                target.0, id.buffer_id, id.frame_number
            );
        }
        state
            .deliveries
            .push(on_release_buffer_delivery(target, id, state.max_acquired_buffers));
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
                listener.callback_ids.iter().map(|c| c.id).collect::<Vec<_>>(),
                handle.0
            );
        }
        state.deliveries.push(on_transaction_completed_delivery(
            listener.listener,
            &listener.callback_ids,
            handle,
            &previous_id,
            latch_time,
            state.max_acquired_buffers,
        ));
    }
}

fn try_clone(meta: &GraphicBufferMeta) -> OwnedFd {
    meta.fd
        .try_clone()
        .unwrap_or_else(|_| panic!("duplicating a GraphicBuffer memfd cannot fail"))
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

    let row = buf.row_bytes();
    let size = row.saturating_mul(buf.height as usize);
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
    data.write_interface_token(TRANSACTION_LISTENER_DESCRIPTOR).ok();
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
fn on_transaction_completed_delivery(
    target: BinderId,
    callback_ids: &[CallbackId],
    handle: BinderId,
    previous: &ReleaseCallbackId,
    latch_time: i64,
    max_acquired: u32,
) -> PendingDelivery {
    let mut data = Parcel::new();
    data.write_interface_token(TRANSACTION_LISTENER_DESCRIPTOR).ok();
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
    data.write_int32(1);
    data.write_uint64(previous.frame_number);
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

    let composer_count = data.read_uint32().map_err(perr)? as usize;
    let mut composers = Vec::with_capacity(composer_count.min(64));
    for _ in 0..composer_count {
        composers.push(read_layer_state(data)?);
    }

    let display_count = data.read_uint32().map_err(perr)? as usize;
    for _ in 0..display_count {
        read_display_state(data)?;
    }

    let dbg = std::env::var_os("ACOS_W5_TAIL").is_some();
    if dbg { eprintln!("ACOSDBG tail displays at {}", data.data_position()); }
    let flags = data.read_uint32().map_err(perr)?;
    let apply_token = data.read_strong_binder().map_err(perr)?;
    if dbg { eprintln!("ACOSDBG tail iwc at {}", data.data_position()); }
    read_input_window_commands(data)?;
    if dbg { eprintln!("ACOSDBG tail desired at {}", data.data_position()); }
    let desired_present_time = data.read_int64().map_err(perr)?;
    let is_auto_timestamp = data.read_bool().map_err(perr)?;
    if dbg { eprintln!("ACOSDBG tail uncache at {}", data.data_position()); }

    let uncache_count = data.read_uint32().map_err(perr)? as usize;
    for _ in 0..uncache_count {
        let _token = data.read_strong_binder().map_err(perr)?;
        let _id = data.read_uint64().map_err(perr)?;
    }

    if dbg { eprintln!("ACOSDBG tail listeners at {}", data.data_position()); }
    let has_listener_callbacks = data.read_bool().map_err(perr)?;
    let listener_count = data.read_int32().map_err(perr)?;
    let mut listener_callbacks = Vec::new();
    if listener_count > 0 {
        for _ in 0..listener_count {
            let listener = data.read_strong_binder().map_err(perr)?.unwrap_or(BinderId(0));
            let callback_ids = read_callback_id_vector(data)?;
            listener_callbacks.push(ListenerCallbacks {
                listener,
                callback_ids,
            });
        }
    }

    if dbg { eprintln!("ACOSDBG tail txnid at {}", data.data_position()); }
    let transaction_id = data.read_uint64().map_err(perr)?;
    let merged_count = data.read_uint32().map_err(perr)? as usize;
    let mut merged_transaction_ids = Vec::with_capacity(merged_count.min(64));
    for _ in 0..merged_count {
        merged_transaction_ids.push(data.read_uint64().map_err(perr)?);
    }

    Ok(TransactionState {
        frame_timeline_size,
        composers,
        displays: display_count,
        flags,
        apply_token,
        desired_present_time,
        is_auto_timestamp,
        uncache_buffers: uncache_count,
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
        eprintln!("ACOSDBG layer_state size={} at {start}: {bytes:02x?}", data.data_size());
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

    // `SurfaceControl::readNullableFromParcel` twice: bool then
    // (client binder, handle binder, int32 layerId, String name, uint32
    // transformHint, uint32 width, uint32 height, uint32 format).
    read_nullable_surface_control(data)?;
    read_nullable_surface_control(data)?;
    let dbg = std::env::var_os("ACOS_W5_LS").is_some();
    if dbg { eprintln!("ACOSDBG ls nsc at {}", data.data_position()); }

    for _ in 0..4 {
        let _ = data.read_float().map_err(perr)?;
    }
    if dbg { eprintln!("ACOSDBG ls color at {}", data.data_position()); }
    read_window_info(data)?;
    if dbg { eprintln!("ACOSDBG ls windowinfo at {}", data.data_position()); }

    let transparent_region_size = data.read_int32().map_err(perr)?;
    skip_bytes(data, transparent_region_size)?;
    if dbg { eprintln!("ACOSDBG ls region at {}", data.data_position()); }
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
    if dbg { eprintln!("ACOSDBG ls metadata at {}", data.data_position()); }

    for _ in 0..4 {
        let _ = data.read_float().map_err(perr)?;
    }
    let _ = data.read_uint32().map_err(perr)?; // bgColorDataspace
    let _ = data.read_bool().map_err(perr)?; // colorSpaceAgnostic

    let listener_count = data.read_int32().map_err(perr)?;
    let mut listeners = Vec::new();
    for _ in 0..listener_count.max(0) {
        let listener = data.read_strong_binder().map_err(perr)?.unwrap_or(BinderId(0));
        let callback_ids = read_callback_id_vector(data)?;
        listeners.push(ListenerCallbacks {
            listener,
            callback_ids,
        });
    }

    if dbg { eprintln!("ACOSDBG ls listeners at {}", data.data_position()); }
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

    if dbg { eprintln!("ACOSDBG ls blur at {}", data.data_position()); }
    // `StretchEffect` is fixed-size (6 floats + 4-float bounds = 40 bytes).
    for _ in 0..10 {
        let _ = data.read_float().map_err(perr)?;
    }
    let buffer_crop = read_rect(data)?;
    let destination_frame = read_rect(data)?;
    let _is_trusted_overlay = data.read_bool().map_err(perr)?;
    let drop_input_mode = data.read_uint32().map_err(perr)?;
    if dbg { eprintln!("ACOSDBG ls trusted at {}", data.data_position()); }

    let has_buffer_data = data.read_bool().map_err(perr)?;
    if dbg { eprintln!("ACOSDBG ls hasbuf={has_buffer_data} at {}", data.data_position()); }
    let buffer = if has_buffer_data {
        Some(read_buffer_data(data, buffer_crop)?)
    } else {
        None
    };
    if dbg { eprintln!("ACOSDBG ls buffer at {}", data.data_position()); }

    // Parcelable TrustedPresentationThresholds (AIDL: marker + size + payload).
    read_aidl_parcelable(data)?;
    if dbg { eprintln!("ACOSDBG ls tpt at {}", data.data_position()); }
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
    if dbg { eprintln!("ACOSDBG ls tpl2 at {}", data.data_position()); }

    if dbg { eprintln!("ACOSDBG ls tpl at {}", data.data_position()); }
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
    if dbg { eprintln!("ACOSDBG bd flags at {} hasbuf={has_buf}", data.data_position()); }
    let buffer = if has_buf {
        Some(read_graphic_buffer(data)?)
    } else {
        None
    };
    if dbg { eprintln!("ACOSDBG bd gb at {}", data.data_position()); }
    let has_fence = data.read_bool().map_err(perr)?;
    if dbg { eprintln!("ACOSDBG bd fenceflag={has_fence} at {}", data.data_position()); }
    if has_fence {
        // An acquire fence: `Parcel::write(Fence)`.
        read_fence(data)?;
        if dbg { eprintln!("ACOSDBG bd fence at {}", data.data_position()); }
    }
    let frame_number = data.read_uint64().map_err(perr)?;
    if dbg { eprintln!("ACOSDBG bd frame={frame_number} at {}", data.data_position()); }
    let release_buffer_listener = data.read_strong_binder().map_err(perr)?.unwrap_or(BinderId(0));
    if dbg { eprintln!("ACOSDBG bd listener at {}", data.data_position()); }
    let release_buffer_endpoint = data.read_strong_binder().map_err(perr)?.unwrap_or(BinderId(0));
    if dbg { eprintln!("ACOSDBG bd endpoint at {}", data.data_position()); }
    let _cached_token = data.read_strong_binder().map_err(perr)?;
    if dbg { eprintln!("ACOSDBG bd cachedtok at {}", data.data_position()); }
    let cached_buffer_id = data.read_uint64().map_err(perr)?;
    let has_barrier = data.read_bool().map_err(perr)?;
    let barrier_frame_number = data.read_uint64().map_err(perr)?;
    let producer_id = data.read_uint32().map_err(perr)?;
    if dbg { eprintln!("ACOSDBG bd producer at {}", data.data_position()); }
    Ok(BufferData {
        flags,
        buffer,
        frame_number,
        release_buffer_listener,
        release_buffer_endpoint,
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
        eprintln!("ACOSDBG gb header len={len} fdc={fd_count} at {}", data.data_position());
    }
    if len < 13 * 4 || fd_count < 0 {
        return Err(ServiceError::BadRequest {
            interface: LEGACY_COMPOSER_DESCRIPTOR.to_string(),
            method: "setTransactionState".into(),
            detail: format!("bad GraphicBuffer flatten header len={len} fds={fd_count}"),
        });
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
            fd = Some(cloned.map_err(|e| {
                ServiceError::Internal(format!("dup(GraphicBuffer fd {raw}): {e}"))
            })?);
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

fn read_nullable_surface_control(data: &mut Parcel) -> SResult<()> {
    if !data.read_bool().map_err(perr)? {
        return Ok(());
    }
    let _client = data.read_strong_binder().map_err(perr)?;
    let _handle = data.read_strong_binder().map_err(perr)?;
    let _layer_id = data.read_int32().map_err(perr)?;
    let _name = data.read_string16().map_err(perr)?;
    let _transform_hint = data.read_uint32().map_err(perr)?;
    let _width = data.read_uint32().map_err(perr)?;
    let _height = data.read_uint32().map_err(perr)?;
    let _format = data.read_uint32().map_err(perr)?;
    Ok(())
}

/// The image's `gui::WindowInfo` (older than r75: no `contentSize`, no
/// trailing `canOccludePresentation`).
fn read_window_info(data: &mut Parcel) -> SResult<()> {
    if data.read_int32().map_err(perr)? == 0 {
        return Ok(());
    }
    let _token = data.read_strong_binder().map_err(perr)?;
    let _dispatching_timeout = data.read_int64().map_err(perr)?;
    let _id = data.read_int32().map_err(perr)?;
    let _name = data.read_string16().map_err(perr)?;
    let _layout_params_flags = data.read_int32().map_err(perr)?;
    let _layout_params_type = data.read_int32().map_err(perr)?;
    read_rect(data)?; // frame
    let _surface_inset = data.read_int32().map_err(perr)?;
    let _global_scale = data.read_float().map_err(perr)?;
    let _alpha = data.read_float().map_err(perr)?;
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
    Ok(())
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
