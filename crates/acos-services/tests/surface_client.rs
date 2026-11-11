//! C1 acceptance: the compositor-client surface stock `libgui` talks to.
//!
//! Every request is hand-built in the **image's** order and every reply is
//! decoded in the image's read order with `data_avail() == 0`. The wire facts
//! and their addresses in `.cache/art/sysroot/system/lib64/libgui.so` are in the
//! module docs of `acos_services::surface_client`.

use std::os::fd::{AsFd, OwnedFd};

use acos_parcel::{BinderId, Parcel};
use acos_services::compositor_client::{self, DisplayDescriptor};
use acos_services::exception::NO_EXCEPTION;
use acos_services::surface_client::{
    self, CallbackId, SurfaceState,
};
use acos_services::{ServiceError, ServiceHost};

const ISURFACE_COMPOSER: &str = "android.gui.ISurfaceComposer";
const ISURFACE_COMPOSER_LEGACY: &str = "android.ui.ISurfaceComposer";
const ISURFACE_COMPOSER_CLIENT: &str = "android.gui.ISurfaceComposerClient";
const ITRANSACTION_LISTENER: &str = "android.gui.ITransactionComposerListener";

const CREATE_CONNECTION: u32 = 3;
const CREATE_SURFACE: u32 = 1;
const SET_TRANSACTION_STATE: u32 = 8;

fn transact(
    host: &mut ServiceHost,
    binder: BinderId,
    code: u32,
    flags: u32,
    data: &mut Parcel,
) -> std::result::Result<Parcel, ServiceError> {
    let mut reply = Parcel::new();
    host.transact(binder, code, data, &mut reply, flags)?;
    reply.set_data_position(0);
    Ok(reply)
}

fn no_exception(reply: &mut Parcel) {
    assert_eq!(reply.read_int32().expect("exception header"), NO_EXCEPTION);
}

fn composer(host: &mut ServiceHost) -> BinderId {
    host.check_service("SurfaceFlinger")
        .expect("lookup is infallible")
        .expect("SurfaceFlinger resolves to the compositor node")
}

/// `ISurfaceComposer.createConnection()` (AIDL code 3): token only.
fn create_connection(host: &mut ServiceHost, sf: BinderId) -> BinderId {
    let mut data = Parcel::new();
    data.write_interface_token(ISURFACE_COMPOSER).expect("token");
    data.set_data_position(0);
    let mut reply = transact(host, sf, CREATE_CONNECTION, 0, &mut data).expect("createConnection");
    no_exception(&mut reply);
    let client = reply
        .read_strong_binder()
        .expect("binder")
        .expect("non-null ISurfaceComposerClient");
    assert_eq!(reply.data_avail(), 0, "createConnection reply has trailing bytes");
    client
}

/// `ISurfaceComposerClient.createSurface(name, flags, parent, metadata)`.
fn create_surface(host: &mut ServiceHost, client: BinderId, name: &str) -> (BinderId, i32) {
    let mut data = Parcel::new();
    data.write_interface_token(ISURFACE_COMPOSER_CLIENT)
        .expect("token");
    data.write_string16(name).expect("name");
    data.write_int32(0); // flags
    data.write_strong_binder_opt(None); // parent
    write_layer_metadata(&mut data);
    data.set_data_position(0);
    let mut reply =
        transact(host, client, CREATE_SURFACE, 0, &mut data).expect("createSurface");
    no_exception(&mut reply);
    // `CreateSurfaceResult` AIDL parcelable: marker 1, inline size, then fields.
    assert_eq!(reply.read_int32().expect("parcelable marker"), 1);
    let size = reply.read_int32().expect("parcelable size");
    assert!(size > 0);
    let handle = reply
        .read_strong_binder()
        .expect("binder")
        .expect("layer handle");
    let layer_id = reply.read_int32().expect("layer id");
    let layer_name = reply.read_string16().expect("name").expect("non-null");
    let transform_hint = reply.read_int32().expect("transform hint");
    assert_eq!(layer_name, name);
    assert_eq!(transform_hint, 0);
    assert_eq!(reply.data_avail(), 0, "createSurface reply has trailing bytes");
    (handle, layer_id)
}

fn write_layer_metadata(data: &mut Parcel) {
    data.write_int32(1); // non-null Parcelable
    data.write_int32(0); // empty map
}

fn write_rect(data: &mut Parcel, r: [i32; 4]) {
    for v in r {
        data.write_int32(v);
    }
}

fn write_words(data: &mut Parcel, bytes: &[u8]) {
    for word in bytes.chunks(4) {
        let mut buf = [0u8; 4];
        buf[..word.len()].copy_from_slice(word);
        data.write_int32(i32::from_le_bytes(buf));
    }
}

/// The image's `gui::WindowInfo` (empty: just the presence word).
fn write_window_info(data: &mut Parcel, full: bool) {
    if !full {
        data.write_int32(0);
        return;
    }
    data.write_int32(1); // non-empty
    data.write_strong_binder_opt(None); // token
    data.write_int64(0); // dispatching timeout
    data.write_int32(1); // id
    data.write_string16("Window{test}").expect("name");
    data.write_int32(0); // layoutParamsFlags
    data.write_int32(1); // layoutParamsType
    write_rect(data, [0, 0, 64, 48]); // frame
    data.write_int32(0); // surfaceInset (the image has no contentSize fields)
    data.write_float(1.0); // globalScaleFactor
    data.write_float(1.0); // alpha
    for _ in 0..6 {
        data.write_float(0.0); // transform
    }
    data.write_int32(0); // touchOcclusionMode
    data.write_int32(1000); // ownerPid
    data.write_int32(10000); // ownerUid
    data.write_string16("com.acos.test").expect("package");
    data.write_int32(0); // inputConfig
    data.write_int32(0); // displayId
    // InputApplicationInfo is written directly (no Parcelable marker): an
    // inline size word then token/name/timeout.
    let mut app = Parcel::new();
    app.write_strong_binder_opt(None);
    app.write_string16("app").expect("app name");
    app.write_int64(0);
    let body = app.marshall();
    data.write_int32(body.len() as i32);
    write_words(data, &body);
    write_light_blob(data, &[0]); // touchableRegion
    data.write_bool(false); // replaceTouchableRegionWithCrop
    data.write_strong_binder_opt(None); // touchableRegionCropHandle
    data.write_strong_binder_opt(None); // windowToken
    data.write_strong_binder_opt(None); // focusTransferTarget
}

/// A `LightFlattenable` variable-size body: size word then body words.
fn write_light_blob(data: &mut Parcel, words: &[i32]) {
    data.write_int32((words.len() * 4) as i32);
    for w in words {
        data.write_int32(*w);
    }
}

/// `GraphicBuffer`: 13 header words + 9 shim handle ints + one memfd.
fn write_graphic_buffer(
    data: &mut Parcel,
    fd: &OwnedFd,
    width: u32,
    height: u32,
    stride: u32,
    format: i32,
    id: u64,
) {
    // header + handle, the exact 88 bytes G1's shim makes `getFlattenedSize()`
    // report (13 words + 9 ints).
    data.write_int32(88); // len
    data.write_int32(1); // fd count
    data.write_int32(i32::from_le_bytes(*b"GB01"));
    data.write_int32(width as i32);
    data.write_int32(height as i32);
    data.write_int32(stride as i32);
    data.write_int32(format);
    data.write_int32(1); // layerCount
    data.write_int32((id & 0xffff_ffff) as i32); // usage lo (unused here)
    data.write_int32((id >> 32) as i32); // GraphicBuffer.id hi
    data.write_int32((id & 0xffff_ffff) as i32); // GraphicBuffer.id lo
    data.write_int32(0); // generation
    data.write_int32(1); // handle numFds
    data.write_int32(9); // handle numInts
    data.write_int32(0); // usage hi
    // the nine gralloc-shim handle ints: w,h,stride,format,usage lo,usage hi,
    // layers,bpp,dataspace
    data.write_int32(width as i32);
    data.write_int32(height as i32);
    data.write_int32(stride as i32);
    data.write_int32(format);
    data.write_int32(0);
    data.write_int32(0);
    data.write_int32(1);
    data.write_int32(4);
    data.write_int32(0);
    data.write_file_descriptor(fd.as_fd()).expect("fd");
}

#[allow(clippy::too_many_arguments)]
fn write_buffer_data(
    data: &mut Parcel,
    fd: Option<&OwnedFd>,
    width: u32,
    height: u32,
    frame_number: u64,
    buffer_id: u64,
    listener: BinderId,
) {
    data.write_int32(1); // Parcelable BufferData non-null
    data.write_int32(0x04 | 0x02 | 0x01); // flags: cached/frame/fence changed
    data.write_bool(fd.is_some()); // has buffer
    if let Some(fd) = fd {
        write_graphic_buffer(data, fd, width, height, width, 1, buffer_id);
    }
    data.write_bool(false); // no acquire fence
    data.write_uint64(frame_number);
    data.write_strong_binder(listener); // releaseBufferListener
    data.write_strong_binder(BinderId(0)); // releaseBufferEndpoint
    data.write_strong_binder(BinderId(0)); // cached buffer token
    data.write_uint64(buffer_id);
    data.write_bool(false); // hasBarrier
    data.write_uint64(0);
    data.write_uint32(1); // producerId
}

/// How a transaction names its buffer.
///
/// `Inline` carries the flattened `GraphicBuffer`; `Cached` is the shape the
/// image's own client sends for a buffer it has **already** sent
/// (`Transaction::cacheBuffers` strips it and references the id instead).
#[derive(Clone, Copy)]
enum BufferSpec {
    /// `GraphicBuffer` in the parcel.
    Inline {
        /// `releaseBufferListener`.
        listener: BinderId,
        /// Frame number.
        frame: u64,
        /// `GraphicBuffer::getId()`.
        id: u64,
    },
    /// `client_cache_t` reference to a buffer sent earlier.
    Cached {
        /// `releaseBufferListener`.
        listener: BinderId,
        /// Frame number.
        frame: u64,
        /// `client_cache_t.id` (the `GraphicBuffer` id).
        id: u64,
    },
}

impl BufferSpec {
    fn parts(self) -> (BinderId, u64, u64, bool) {
        match self {
            Self::Inline { listener, frame, id } => (listener, frame, id, true),
            Self::Cached { listener, frame, id } => (listener, frame, id, false),
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn write_layer_state(
    data: &mut Parcel,
    handle: BinderId,
    layer_id: i32,
    what: u64,
    buffer: Option<BufferSpec>,
    listeners: &[(BinderId, Vec<CallbackId>)],
    window_info_full: bool,
) {
    data.write_strong_binder(handle);
    data.write_int32(layer_id);
    data.write_uint64(what);
    data.write_float(0.0);
    data.write_float(0.0);
    data.write_int32(0);
    data.write_uint32(0);
    data.write_uint32(0);
    data.write_uint32(0);
    for _ in 0..4 {
        data.write_float(0.0);
    }
    write_rect(data, [0, 0, 0, 0]);
    data.write_bool(false); // relativeLayerSurfaceControl
    data.write_bool(false); // parentSurfaceControlForChild
    for _ in 0..4 {
        data.write_float(0.0);
    }
    write_window_info(data, window_info_full);
    write_light_blob(data, &[0]); // transparentRegion
    data.write_uint32(0); // bufferTransform
    data.write_bool(false); // transformToDisplayInverse
    data.write_bool(false); // borderEnabled
    data.write_float(0.0); // borderWidth
    for _ in 0..4 {
        data.write_float(0.0); // borderColor
    }
    data.write_uint32(0); // dataspace
    write_light_blob(data, &[0]); // hdrMetadata
    write_light_blob(data, &[0]); // surfaceDamageRegion
    data.write_int32(0); // api
    data.write_bool(false); // no sideband stream
    for _ in 0..16 {
        data.write_float(0.0); // colorTransform
    }
    data.write_float(0.0); // cornerRadius
    data.write_uint32(0); // backgroundBlurRadius
    write_layer_metadata(data);
    for _ in 0..4 {
        data.write_float(0.0); // bgColor
    }
    data.write_uint32(0); // bgColorDataspace
    data.write_bool(false); // colorSpaceAgnostic

    data.write_int32(listeners.len() as i32);
    for (listener, callbacks) in listeners {
        data.write_strong_binder(*listener);
        data.write_int32(callbacks.len() as i32);
        for cb in callbacks {
            data.write_int32(1); // Parcelable CallbackId
            data.write_int64(cb.id);
            data.write_int32(cb.kind);
        }
    }

    data.write_float(0.0); // shadowRadius
    data.write_int32(0); // frameRateSelectionPriority
    data.write_float(60.0); // frameRate
    // The image's frame-rate group is three bytes (no frameRateCategory,
    // frameRateCategorySmoothSwitchOnly or frameRateSelectionStrategy).
    data.write_byte(0); // frameRateCompatibility
    data.write_byte(0); // changeFrameRateStrategy
    data.write_byte(0); // defaultFrameRateCompatibility
    data.write_uint32(0); // fixedTransformHint
    data.write_bool(false); // autoRefresh
    data.write_bool(false); // dimmingEnabled

    data.write_uint32(0); // blurRegions
    for _ in 0..10 {
        data.write_float(0.0); // StretchEffect (fixed 40 bytes)
    }
    write_rect(data, [0, 0, 0, 0]); // bufferCrop
    write_rect(data, [0, 0, 0, 0]); // destinationFrame
    data.write_bool(false); // isTrustedOverlay
    data.write_uint32(0); // dropInputMode

    if let Some(spec) = buffer {
        data.write_bool(true);
        let (listener, frame_number, buffer_id, inline) = spec.parts();
        // A File kept alive by the caller; the parcel dups the descriptor.
        let fd = test_memfd();
        let fd = if inline { Some(&fd) } else { None };
        write_buffer_data(data, fd, 64, 48, frame_number, buffer_id, listener);
    } else {
        data.write_bool(false);
    }
    data.write_int32(1); // TrustedPresentationThresholds marker
    data.write_int32(16); // AIDL size counts itself: 4 + 12 payload bytes
    data.write_float(-1.0);
    data.write_float(-1.0);
    data.write_int32(0);
    data.write_int32(1); // TrustedPresentationListener marker
    data.write_strong_binder(BinderId(0));
    // A hand-built (non-wire-form) parcel carries no stability record after the
    // null binder; see the fixture helper for the imported wire form.
    data.write_int32(-1);
    data.write_float(1.0); // currentHdrSdrRatio
    data.write_float(1.0); // desiredHdrSdrRatio
    data.write_int32(0); // cachingHint Enabled
}

fn test_memfd() -> OwnedFd {
    use std::sync::atomic::{AtomicU32, Ordering};
    static N: AtomicU32 = AtomicU32::new(0);
    let path = std::env::temp_dir().join(format!(
        "acos-c1-{}-{}.bin",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    // A GraphicBuffer fd is read-write; open it that way so a capture can read
    // the pixels back (W10's capture path reads through its own dup).
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(&path)
        .expect("temp buffer");
    file.set_len(64 * 48 * 4).expect("size");
    let _ = std::fs::remove_file(&path);
    file.into()
}

type ComposerSpec = (
    BinderId,
    i32,
    Option<BufferSpec>,
    Vec<(BinderId, Vec<CallbackId>)>,
);

/// One concrete legacy transaction parcel with `count` composers.
fn transaction_parcel(
    composers: &[ComposerSpec],
    frame_timeline: &[i32],
    listener_callbacks: &[(BinderId, Vec<CallbackId>)],
    transaction_id: u64,
) -> Parcel {
    let mut data = Parcel::new();
    data.write_interface_token(ISURFACE_COMPOSER_LEGACY)
        .expect("token");
    write_light_blob(&mut data, frame_timeline); // FrameTimelineInfo
    data.write_uint32(composers.len() as u32);
    for (handle, layer_id, buffer, listeners) in composers {
        write_layer_state(
            &mut data,
            *handle,
            *layer_id,
            surface_client::what::BUFFER | surface_client::what::LAYER,
            *buffer,
            listeners,
            false,
        );
    }
    data.write_uint32(0); // displays
    data.write_uint32(0); // flags
    data.write_strong_binder_opt(None); // applyToken
    data.write_int32(0); // InputWindowCommands: focusRequests
    data.write_int32(0); // InputWindowCommands: windowInfosReportedListeners
    data.write_int64(0); // desiredPresentTime
    data.write_bool(true); // isAutoTimestamp
    data.write_uint32(0); // uncacheBuffers
    data.write_bool(!listener_callbacks.is_empty());
    data.write_int32(listener_callbacks.len() as i32);
    for (listener, callbacks) in listener_callbacks {
        data.write_strong_binder(*listener);
        data.write_int32(callbacks.len() as i32);
        for cb in callbacks {
            data.write_int32(1);
            data.write_int64(cb.id);
            data.write_int32(cb.kind);
        }
    }
    data.write_uint64(transaction_id);
    data.write_uint32(0); // mergedTransactionIds
    data.set_data_position(0);
    data
}

fn find_release(
    deliveries: &[acos_services::PendingDelivery],
    listener: BinderId,
) -> &acos_services::PendingDelivery {
    deliveries
        .iter()
        .find(|d| d.target == listener && d.code == 2)
        .expect("onReleaseBuffer delivery")
}

fn find_completed(
    deliveries: &[acos_services::PendingDelivery],
    listener: BinderId,
) -> &acos_services::PendingDelivery {
    deliveries
        .iter()
        .find(|d| d.target == listener && d.code == 1)
        .expect("onTransactionCompleted delivery")
}

fn score_state(state: &std::sync::Arc<std::sync::Mutex<SurfaceState>>) -> std::sync::MutexGuard<'_, SurfaceState> {
    state.lock().unwrap()
}

#[test]
fn create_connection_and_surface_produce_a_routable_layer_handle() {
    let mut host = ServiceHost::new();
    let (_pool, state) = compositor_client::install_full(&mut host, DisplayDescriptor::reference());
    let sf = composer(&mut host);
    let client = create_connection(&mut host, sf);
    assert_eq!(
        state.lock().unwrap().client_binder,
        client,
        "createConnection must answer the installed client node"
    );

    let (handle, layer_id) = create_surface(&mut host, client, "Surface(name=test)");
    assert!(handle.0 != 0);
    assert_eq!(layer_id, 1);
    let s = score_state(&state);
    let layer = s.layer(handle).expect("layer record");
    assert_eq!(layer.name, "Surface(name=test)");
    assert_eq!(layer.layer_id, 1);
}

/// W5: the image's own `Transaction::apply` parcel writes
/// `FrameTimelineInfo`'s size word **including the word itself** (a live
/// BLAST transaction measured 28 for the 24 byte field set). The old decoder
/// skipped the raw word, so every real `setTransactionState` read four bytes
/// late and the layer's `flat_binder_object` was decoded as its own flags.
#[test]
fn the_image_timeline_size_word_that_counts_itself_decodes() {
    let mut host = ServiceHost::new();
    let (_pool, _state) = compositor_client::install_full(&mut host, DisplayDescriptor::reference());
    let sf = composer(&mut host);
    let client = create_connection(&mut host, sf);
    let (handle, layer_id) = create_surface(&mut host, client, "image-timeline");

    // `transaction_parcel` writes the body-only size (24), which is what the
    // hand-built tests and older writers emit.
    let mut parcel = transaction_parcel(
        &[(handle, layer_id, None, Vec::new())],
        &[0, 0, 0, 0, 0, 12],
        &[],
        7,
    );
    let mut probe = Parcel::new();
    probe
        .write_interface_token(ISURFACE_COMPOSER_LEGACY)
        .expect("token");
    let size_pos = probe.data_position();
    let mut bytes = parcel.marshall();
    bytes[size_pos..size_pos + 4].copy_from_slice(&28i32.to_le_bytes());
    parcel.unmarshall(&bytes);
    parcel
        .enforce_interface(ISURFACE_COMPOSER_LEGACY)
        .expect("token");

    let decoded =
        surface_client::decode_transaction_state(&mut parcel).expect("image shape must decode");
    assert_eq!(decoded.composers.len(), 1, "the layer state must be found");
    assert_eq!(decoded.composers[0].layer_id, layer_id);
    assert_eq!(
        decoded.transaction_id, 7,
        "the trailer is reached exactly (a four byte shift loses it)"
    );
    assert_eq!(parcel.data_avail(), 0, "the body ends at the parcel end");
}

#[test]
fn a_hand_built_transaction_decodes_to_the_last_byte() {
    let mut host = ServiceHost::new();
    let (_pool, state) = compositor_client::install_full(&mut host, DisplayDescriptor::reference());
    let sf = composer(&mut host);
    let client = create_connection(&mut host, sf);
    let (handle, layer_id) = create_surface(&mut host, client, "decode");

    let mut parcel = transaction_parcel(
        &[(handle, layer_id, None, Vec::new())],
        &[0, 0, 0, 0, 0, 12],
        &[],
        7,
    );
    parcel
        .enforce_interface(ISURFACE_COMPOSER_LEGACY)
        .expect("token");
    let decoded = surface_client::decode_transaction_state(&mut parcel).expect("decode");
    assert_eq!(
        parcel.data_avail(),
        0,
        "the decoder did not consume the transaction parcel exactly"
    );
    assert_eq!(decoded.transaction_id, 7);
    assert_eq!(decoded.composers.len(), 1);
    assert_eq!(decoded.composers[0].surface, Some(handle));
    assert_eq!(decoded.composers[0].layer_id, layer_id);
    assert_eq!(decoded.desired_present_time, 0);
    assert!(decoded.is_auto_timestamp);
    let _ = state;
}

/// W10: the live session's screenshot is a capture of C1's stored buffer, and
/// C1's store counter is the "a new frame landed" signal the launcher waits on.
/// Two transactions must advance the generation twice, and a capture must not
/// change it (a capture is a read, not a frame).
#[test]
fn the_live_capture_reads_the_newest_c1_buffer_and_reports_its_generation() {
    let mut host = ServiceHost::new();
    let (_pool, state) = compositor_client::install_full(&mut host, DisplayDescriptor::reference());
    let sf = composer(&mut host);
    let client = create_connection(&mut host, sf);
    let (handle, layer_id) = create_surface(&mut host, client, "live-capture");
    let listener = host.register_aux(ITRANSACTION_LISTENER);

    for (frame, id) in [(1u64, 100u64), (2, 200)] {
        let mut parcel =
            transaction_parcel(&[(handle, layer_id, Some(BufferSpec::Inline { listener, frame, id }), Vec::new())], &[0, 0, 0, 0, 0, 3], &[], frame);
        transact(&mut host, sf, SET_TRANSACTION_STATE, 0, &mut parcel).expect("transaction");
    }
    {
        let s = score_state(&state);
        assert_eq!(s.latest_generation(), 2, "one increment per stored buffer");
        let capture = s.capture_latest().expect("a stored buffer is capturable");
        assert_eq!((capture.width, capture.height), (64, 48));
        assert_eq!((capture.stride, capture.row_bytes), (64, 64 * 4));
        assert_eq!(capture.generation, 2);
        assert_eq!(capture.frame_number, 2);
        assert_eq!(capture.buffer_id, 200);
        assert_eq!(capture.layer_id, layer_id);
        assert_eq!(capture.rgba.len(), capture.row_bytes * capture.height as usize);
        assert_eq!(s.latest_generation(), 2, "a capture must not advance the store counter");
    }
}

#[test]
fn two_buffer_transactions_release_the_first_and_keep_the_latest() {
    let mut host = ServiceHost::new();
    let (_pool, state) = compositor_client::install_full(&mut host, DisplayDescriptor::reference());
    let sf = composer(&mut host);
    let client = create_connection(&mut host, sf);
    let (handle, layer_id) = create_surface(&mut host, client, "blast");

    let listener = host.register_aux(ITRANSACTION_LISTENER);
    let callback = CallbackId { id: 41, kind: 0 };

    // Frame 1: no previous buffer, so only the completion callback is expected.
    let mut parcel = transaction_parcel(
        &[(
            handle,
            layer_id,
            Some(BufferSpec::Inline { listener, frame: 1, id: 100 }),
            vec![(listener, vec![callback])],
        )],
        &[0, 0, 0, 0, 0, 1],
        &[(listener, vec![callback])],
        1,
    );
    transact(&mut host, sf, SET_TRANSACTION_STATE, 0, &mut parcel)
        .expect("first transaction");
    {
        let s = score_state(&state);
        let latest = s.latest_buffer(handle).expect("latest buffer after frame 1");
        assert_eq!(latest.frame_number, 1);
        assert_eq!(latest.buffer_id, 100);
        assert_eq!((latest.width, latest.height), (64, 48));
    }
    let first = host.take_deliveries();
    assert_eq!(first.len(), 1, "frame 1 must only complete, not release");
    let completed = find_completed(&first, listener);
    assert_eq!(completed.interface, ITRANSACTION_LISTENER);

    // Frame 2: frame 1 must now be released, and the store must hold frame 2.
    let mut parcel = transaction_parcel(
        &[(
            handle,
            layer_id,
            Some(BufferSpec::Inline { listener, frame: 2, id: 200 }),
            vec![(listener, vec![callback])],
        )],
        &[0, 0, 0, 0, 0, 2],
        &[(listener, vec![callback])],
        2,
    );
    transact(&mut host, sf, SET_TRANSACTION_STATE, 0, &mut parcel)
        .expect("second transaction");
    {
        let s = score_state(&state);
        let latest = s.latest_buffer(handle).expect("latest buffer after frame 2");
        assert_eq!(latest.frame_number, 2);
        assert_eq!(latest.buffer_id, 200);
        assert_eq!(s.transactions_decoded, 2);
    }

    let second = host.take_deliveries();
    // Stock `SurfaceFlinger` reports the replaced buffer inside the completion
    // stats, not in a second `onReleaseBuffer`; a duplicate would pop the
    // client's release callback twice (W11).
    assert_eq!(
        second.len(),
        1,
        "frame 2 must answer with one completion callback and no duplicate release"
    );
    let completed = find_completed(&second, listener);
    let mut stats = completed.data.clone();
    stats.enforce_interface(ITRANSACTION_LISTENER).expect("token");
    assert_eq!(stats.read_int32().expect("ListenerStats marker"), 1);
    assert_eq!(stats.read_int32().expect("TransactionStats count"), 1);
    assert_eq!(stats.read_int32().expect("TransactionStats marker"), 1);
    assert_eq!(stats.read_int32().expect("CallbackId count"), 1);
    assert_eq!(stats.read_int32().expect("CallbackId marker"), 1);
    assert_eq!(stats.read_int64().expect("callback id"), 41);
    assert_eq!(stats.read_int32().expect("callback type"), 0);
    let _latch = stats.read_int64().expect("latch time");
    assert!(!stats.read_bool().expect("present fence"));
    assert_eq!(stats.read_int32().expect("SurfaceStats count"), 1);
    assert_eq!(stats.read_int32().expect("SurfaceStats marker"), 1);
    assert_eq!(
        stats.read_strong_binder().expect("surface control"),
        Some(handle)
    );
    assert!(!stats.read_bool().expect("acquire fence"));
    let _acquire_time = stats.read_int64().expect("acquire time");
    assert!(!stats.read_bool().expect("previous release fence"));
    assert!(!stats.read_bool().expect("transform hint"));
    assert_eq!(
        stats.read_uint32().expect("max acquired"),
        acos_services::compositor_client::DEFAULT_MAX_ACQUIRED_BUFFERS
    );
    assert_eq!(stats.read_int32().expect("event stats marker"), 1);
    // The stats name the frame this transaction *latched* (the new buffer), and
    // `previousReleaseCallbackId` names the one it replaced. `BLASTBufferQueue`
    // releases with the second and unblocks its shadow queue with the first
    // (`mSyncedFrameNumbers.erase(frameEventStats.frameNumber)` +
    // `currFrameNumber > key.framenumber`); with the old (one-behind) value no
    // merged frame's slot was ever returned (W11).
    assert_eq!(
        stats.read_uint64().expect("event frame"),
        2,
        "eventStats.frameNumber must be the latched frame, not the replaced one"
    );
    assert!(!stats.read_bool().expect("gpu fence"));
    for _ in 0..5 {
        let _ = stats.read_int64().expect("compositor timing");
    }
    assert_eq!(stats.read_int32().expect("jank count"), 0);
    assert_eq!(stats.read_int32().expect("previous release marker"), 1);
    assert_eq!(stats.read_uint64().expect("previous buffer id"), 100);
    assert_eq!(stats.read_uint64().expect("previous frame"), 1);
    assert_eq!(stats.data_avail(), 0, "completion stats have trailing bytes");
}

#[test]
fn a_client_cached_buffer_reference_stores_the_frame_it_names() {
    // The image's own client caches every `GraphicBuffer` it sends
    // (`Transaction::cacheBuffers`) and from then on references it by id: the
    // parcel carries a `client_cache_t` instead of the flattened buffer, with
    // `hasBuffer == false`. A producer recycles slots, so after the first few
    // frames *most* frames arrive this way (W11 measured 213 of 215). A
    // compositor that ignores the reference stores only the first two frames and
    // the app's pixels never advance.
    let mut host = ServiceHost::new();
    let (_pool, state) = compositor_client::install_full(&mut host, DisplayDescriptor::reference());
    let sf = composer(&mut host);
    let client = create_connection(&mut host, sf);
    let (handle, layer_id) = create_surface(&mut host, client, "blast");

    let listener = host.register_aux(ITRANSACTION_LISTENER);
    let callback = CallbackId { id: 7, kind: 0 };

    // Frames 1 and 2 are sent inline; frame 3 reuses frame 1's buffer id and is
    // sent as a cache reference only (the client never drops the fd, so the
    // pixels are still the ones it wrote).
    for (frame, id, inline) in [(1u64, 100u64, true), (2, 101, true), (3, 100, false)] {
        let spec = if inline {
            BufferSpec::Inline {
                listener,
                frame,
                id,
            }
        } else {
            BufferSpec::Cached {
                listener,
                frame,
                id,
            }
        };
        let mut parcel = transaction_parcel(
            &[(handle, layer_id, Some(spec), vec![(listener, vec![callback])])],
            &[0, 0, 0, 0, 0, 1],
            &[(listener, vec![callback])],
            frame,
        );
        transact(&mut host, sf, SET_TRANSACTION_STATE, 0, &mut parcel)
            .unwrap_or_else(|e| panic!("frame {frame} transaction: {e}"));
        let s = score_state(&state);
        assert_eq!(
            s.latest_generation(),
            frame,
            "every frame must advance the store, frame {frame} included"
        );
        let latest = s.latest_buffer(handle).expect("latest buffer");
        assert_eq!(latest.frame_number, frame);
        assert_eq!(latest.buffer_id, id);
        assert_eq!((latest.width, latest.height), (64, 48));
        let _ = host.take_deliveries();
    }

    // A reference to an id the client never sent is a structured miss, not a
    // blank frame: the newest stored frame stays the previous one.
    let mut parcel = transaction_parcel(
        &[(
            handle,
            layer_id,
            Some(BufferSpec::Cached {
                listener,
                frame: 4,
                id: 999,
            }),
            vec![(listener, vec![callback])],
        )],
        &[0, 0, 0, 0, 0, 1],
        &[(listener, vec![callback])],
        4,
    );
    transact(&mut host, sf, SET_TRANSACTION_STATE, 0, &mut parcel)
        .expect("unknown cache id transaction");
    {
        let s = score_state(&state);
        assert_eq!(s.latest_generation(), 3);
        assert_eq!(
            s.latest_buffer(handle).expect("latest buffer").frame_number,
            3
        );
    }
}

#[test]
fn a_buffer_transaction_without_callbacks_releases_via_on_release_buffer() {
    // `SurfaceFlinger` falls back to a standalone `onReleaseBuffer` when the
    // transaction that replaced the buffer registered no completion callback
    // (`Layer::callReleaseBufferCallback`). The replaced buffer must still be
    // released, or the producer's slot is lost.
    let mut host = ServiceHost::new();
    let (_pool, state) = compositor_client::install_full(&mut host, DisplayDescriptor::reference());
    let sf = composer(&mut host);
    let client = create_connection(&mut host, sf);
    let (handle, layer_id) = create_surface(&mut host, client, "blast");

    let listener = host.register_aux(ITRANSACTION_LISTENER);
    for frame in 1..=2u64 {
        let mut parcel = transaction_parcel(
            &[(
                handle,
                layer_id,
                Some(BufferSpec::Inline {
                    listener,
                    frame,
                    id: 100 + frame,
                }),
                Vec::new(),
            )],
            &[0, 0, 0, 0, 0, 1],
            &[],
            frame,
        );
        transact(&mut host, sf, SET_TRANSACTION_STATE, 0, &mut parcel).expect("transaction");
    }
    let deliveries = host.take_deliveries();
    let release = find_release(&deliveries, listener);
    let mut data = release.data.clone();
    data.enforce_interface(ITRANSACTION_LISTENER).expect("token");
    assert_eq!(data.read_int32().expect("marker"), 1);
    assert_eq!(data.read_uint64().expect("buffer id"), 101);
    assert_eq!(data.read_uint64().expect("frame"), 1);
    assert_eq!(data.read_int32().expect("fence len"), 4);
    assert_eq!(data.read_int32().expect("fence fds"), 0);
    assert_eq!(data.read_int32().expect("fence body"), 0);
    assert_eq!(
        data.read_uint32().expect("maxAcquired"),
        acos_services::compositor_client::DEFAULT_MAX_ACQUIRED_BUFFERS
    );
    assert_eq!(data.data_avail(), 0);
    assert_eq!(score_state(&state).latest_generation(), 2);
}

#[test]
fn a_legacy_transaction_on_the_aidl_node_dispatches_by_interface_token() {
    let mut host = ServiceHost::new();
    let (_pool, _state) = compositor_client::install_full(&mut host, DisplayDescriptor::reference());
    let sf = composer(&mut host);
    let mut data = Parcel::new();
    data.write_interface_token(ISURFACE_COMPOSER_LEGACY).expect("token");
    // Only the interface/code dispatch is under test here: a truncated body is
    // a BadRequest, not a silent success on the AIDL code 8.
    data.set_data_position(0);
    let err = transact(&mut host, sf, 8, 0, &mut data).expect_err("truncated body");
    assert!(matches!(
        err,
        ServiceError::Internal(_) | ServiceError::BadRequest { .. }
    ));
}


#[test]
fn an_image_shaped_window_info_decodes_to_the_last_byte() {
    // The image's `gui::WindowInfo` is older than r75: no `contentSize`, no
    // trailing `canOccludePresentation`, and `InputApplicationInfo` carries an
    // inline size word with no Parcelable marker. This test builds exactly
    // that shape; a decoder transcribed from the tag would leave 24 bytes over.
    let mut host = ServiceHost::new();
    let (_pool, _state) =
        compositor_client::install_full(&mut host, DisplayDescriptor::reference());
    let sf = composer(&mut host);
    let client = create_connection(&mut host, sf);
    let (handle, layer_id) = create_surface(&mut host, client, "window-info");

    let mut data = Parcel::new();
    data.write_interface_token(ISURFACE_COMPOSER_LEGACY).expect("token");
    write_light_blob(&mut data, &[0, 0, 0, 0, 0, 0]); // FrameTimelineInfo
    data.write_uint32(1); // composers
    write_layer_state(
        &mut data,
        handle,
        layer_id,
        surface_client::what::LAYER,
        None,
        &[],
        true,
    );
    data.write_uint32(0); // displays
    data.write_uint32(0); // flags
    data.write_strong_binder_opt(None); // applyToken
    data.write_int32(0); // focusRequests
    data.write_int32(0); // windowInfosReportedListeners
    data.write_int64(0); // desiredPresentTime
    data.write_bool(true); // isAutoTimestamp
    data.write_uint32(0); // uncacheBuffers
    data.write_bool(false); // hasListenerCallbacks
    data.write_int32(0); // listenerCallbacks
    data.write_uint64(9);
    data.write_uint32(0); // mergedTransactionIds
    data.set_data_position(0);
    data.enforce_interface(ISURFACE_COMPOSER_LEGACY).expect("token");
    let decoded = surface_client::decode_transaction_state(&mut data).expect("decode");
    assert_eq!(data.data_avail(), 0, "WindowInfo body was not consumed exactly");
    assert_eq!(decoded.transaction_id, 9);
    assert_eq!(decoded.composers[0].surface, Some(handle));
}

// ---------------------------------------------------------------------------
// W9: the captured real image parcels (tests/fixtures)
// ---------------------------------------------------------------------------

/// The captured files include the 72-byte `writeInterfaceToken` prelude the peer
/// consumes before the handler runs (`fixtures/README.md`).
const IMAGE_TOKEN_PRELUDE: usize = 72;
/// Body offset of the `BINDER_TYPE_FD` object in `blast_txn_1028.bin`.
const IMAGE_FD_OBJECT_BODY: usize = 668;
/// Body offset of `frameRate`'s field group in `blast_txn_1028.bin` (the three
/// frame-rate bytes are at 452/456/460, `fixedTransformHint` at 464).
const IMAGE_FRAME_RATE_GROUP_END: usize = 460;
const FLAT_OBJECT_BYTES: usize = 24;

/// Build a parcel from a captured image parcel, replacing the one
/// `BINDER_TYPE_FD` object with a live memfd. A parcel's fd table is
/// process-local (the capture's raw descriptor number cannot be re-opened), so
/// that object's 24 bytes are the only ones allowed to differ; the helper
/// asserts the rest of the data is byte-identical to the fixture.
fn image_parcel(bytes: &[u8], fd_object_body_offset: Option<usize>, fd: &OwnedFd) -> Parcel {
    let mut parcel = Parcel::new();
    let Some(fd_object_body_offset) = fd_object_body_offset else {
        // No fd in this capture: the whole file must be byte-identical.
        parcel.unmarshall(bytes);
        parcel.set_wire_null_stability(true);
        return parcel;
    };
    let fd_at = IMAGE_TOKEN_PRELUDE + fd_object_body_offset;
    let mut i = 0;
    while i < bytes.len() {
        if i == fd_at {
            parcel.write_file_descriptor(fd.as_fd()).expect("fd");
            i += FLAT_OBJECT_BYTES;
            continue;
        }
        let mut word = [0u8; 4];
        word.copy_from_slice(&bytes[i..i + 4]);
        parcel.write_int32(i32::from_le_bytes(word));
        i += 4;
    }
    // Mirror `acos-binder-peer::import_parcel_raw`: the captured bytes are the
    // guest driver's wire form, where a null binder is followed by a 4-byte
    // stability record that the import cannot strip (null binders are not in the
    // object table).
    parcel.set_wire_null_stability(true);
    parcel.set_data_position(0);
    let built = parcel.marshall();
    assert_eq!(built.len(), bytes.len(), "fixture length changed");
    assert_eq!(
        &built[..fd_at],
        &bytes[..fd_at],
        "the prefix must stay byte-identical to the capture"
    );
    assert_eq!(
        &built[fd_at + FLAT_OBJECT_BYTES..],
        &bytes[fd_at + FLAT_OBJECT_BYTES..],
        "the tail after the fd object must stay byte-identical to the capture"
    );
    parcel
}

fn fixture_memfd() -> OwnedFd {
    use std::sync::atomic::{AtomicU32, Ordering};
    static N: AtomicU32 = AtomicU32::new(0);
    let path = std::env::temp_dir().join(format!(
        "acos-c1-fixture-{}-{}.rgba",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let file = std::fs::File::create(&path).expect("temp buffer");
    file.set_len(1080 * 2209 * 4).expect("size");
    let _ = std::fs::remove_file(&path);
    file.into()
}

/// W9: one real BLAST `setTransactionState` carrying a `GraphicBuffer` decodes
/// to the last byte. This is the test that failed for W7/W8 (the r75-shaped
/// reader was 8 bytes long at the frame-rate group and the GraphicBuffer magic
/// check read the image's reversed word as garbage).
#[test]
fn an_image_blast_transaction_with_a_buffer_decodes_to_the_last_byte() {
    let bytes = include_bytes!("fixtures/blast_txn_1028.bin");
    let fd = fixture_memfd();
    let mut parcel = image_parcel(bytes, Some(IMAGE_FD_OBJECT_BODY), &fd);
    parcel.set_data_position(IMAGE_TOKEN_PRELUDE);

    let decoded = surface_client::decode_transaction_state(&mut parcel)
        .expect("the captured real BLAST buffer transaction must decode");
    assert_eq!(
        parcel.data_avail(),
        0,
        "the real parcel was not consumed exactly"
    );
    assert_eq!(decoded.composers.len(), 1);
    assert_eq!(decoded.displays, 0, "the capture has no display state");
    assert!(decoded.apply_token.is_some(), "applyToken is a binder");
    let layer = &decoded.composers[0];
    assert_eq!(layer.layer_id, 3);
    // The captured change mask (byte-exact; it includes eBufferChanged
    // `0x0020_0000`, which is why the buffer body follows).
    assert_eq!(layer.what, 0x0000_1009_23ac_2000);
    assert_ne!(layer.what & surface_client::what::BUFFER, 0);
    assert!(layer.surface.is_some(), "surface is a strong binder");
    assert_eq!(
        layer.listeners.len(),
        1,
        "the layer registered one listener group"
    );
    assert_eq!(layer.listeners[0].callback_ids.len(), 2);

    let buffer = layer.buffer.as_ref().expect("the transaction carries a buffer");
    let meta = buffer.buffer.as_ref().expect("the buffer is a GraphicBuffer");
    assert_eq!((meta.width, meta.height, meta.stride), (1080, 2209, 1080));
    assert_eq!(meta.format, 1, "RGBA_8888");
    assert_eq!(meta.id, 0x2_0000_0000, "GraphicBuffer.id == cachedBufferId");
    assert_eq!(meta.usage & 0xffff_ffff, 2355, "usage low word");
    assert_eq!(buffer.frame_number, 1);
    assert_eq!(buffer.producer_id, 1);
    assert_eq!(buffer.cached_buffer_id, meta.id);
    assert!(!buffer.has_barrier);
    // The stored fd is a live, readable 1080x2209x4 memfd (what the capture
    // dump converts to a PNG).
    let readable = std::fs::File::from(meta.fd.try_clone().expect("dup"));
    let len = readable.metadata().expect("stat the captured memfd").len() as usize;
    assert_eq!(
        len,
        1080 * 2209 * 4,
        "the GraphicBuffer fd must be the captured frame buffer"
    );
}

/// The sibling capture has no buffer body: the same prefix and the trailing
/// parcelables must still consume exactly.
#[test]
fn an_image_blast_transaction_without_a_buffer_decodes_to_the_last_byte() {
    let bytes = include_bytes!("fixtures/blast_txn_700.bin");
    let fd = fixture_memfd();
    let mut parcel = image_parcel(bytes, None, &fd);
    parcel.set_data_position(IMAGE_TOKEN_PRELUDE);

    let decoded = surface_client::decode_transaction_state(&mut parcel)
        .expect("the captured real BLAST transaction must decode");
    assert_eq!(parcel.data_avail(), 0, "the real parcel was not consumed exactly");
    assert_eq!(decoded.composers.len(), 1);
    let layer = &decoded.composers[0];
    assert_eq!(layer.layer_id, 4098);
    assert!(
        layer.buffer.is_none(),
        "a transaction without a buffer body must not invent one"
    );
}

/// Teeth for the image's frame-rate group: put the r75 group back (a fourth
/// `byte`, `frameRateCategorySmoothSwitchOnly` and `frameRateSelectionStrategy`
/// — 12 added bytes) and the decoder must refuse the parcel. A reader
/// transcribed from the tag would decode this patched parcel exactly, so this
/// test fails on the old implementation instead of silently accepting both wire
/// shapes.
#[test]
fn a_tag_shaped_frame_rate_group_is_rejected() {
    let bytes = include_bytes!("fixtures/blast_txn_1028.bin");
    let insert_at = IMAGE_TOKEN_PRELUDE + IMAGE_FRAME_RATE_GROUP_END;
    let mut patched = Vec::with_capacity(bytes.len() + 12);
    patched.extend_from_slice(&bytes[..insert_at]);
    patched.extend_from_slice(&[0u8; 12]); // 4th byte + bool + byte, all zero
    patched.extend_from_slice(&bytes[insert_at..]);
    assert_eq!(patched.len(), bytes.len() + 12);

    let fd = fixture_memfd();
    let mut parcel = image_parcel(&patched, Some(IMAGE_FD_OBJECT_BODY + 12), &fd);
    parcel.set_data_position(IMAGE_TOKEN_PRELUDE);
    let err = surface_client::decode_transaction_state(&mut parcel)
        .expect_err("a tag-shaped frame-rate group must not decode as an image parcel");
    assert!(
        matches!(err, ServiceError::BadRequest { .. } | ServiceError::Internal(_)),
        "the rejection must be a structured error, got {err:?}"
    );
}
