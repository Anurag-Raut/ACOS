
use std::os::fd::{AsFd, OwnedFd};

use acos_parcel::{BinderId, Parcel};
use acos_services::compositor_client::{self, DisplayDescriptor};
use acos_services::exception::NO_EXCEPTION;
use acos_services::surface_client::{
    self, CallbackId, ReleaseCallbackId, SurfaceState,
};
use acos_services::{ServiceError, ServiceHost};

pub const ISURFACE_COMPOSER: &str = "android.gui.ISurfaceComposer";
pub const ISURFACE_COMPOSER_LEGACY: &str = "android.ui.ISurfaceComposer";
pub const ISURFACE_COMPOSER_CLIENT: &str = "android.gui.ISurfaceComposerClient";
pub const ITRANSACTION_LISTENER: &str = "android.gui.ITransactionComposerListener";

pub const CREATE_CONNECTION: u32 = 3;
pub const CREATE_SURFACE: u32 = 1;
pub const SET_TRANSACTION_STATE: u32 = 8;

pub fn transact(
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

pub fn no_exception(reply: &mut Parcel) {
    assert_eq!(reply.read_int32().expect("exception header"), NO_EXCEPTION);
}

pub fn composer(host: &mut ServiceHost) -> BinderId {
    host.check_service("SurfaceFlinger")
        .expect("lookup is infallible")
        .expect("SurfaceFlinger resolves to the compositor node")
}

/// `ISurfaceComposer.createConnection()` (AIDL code 3): token only.
pub fn create_connection(host: &mut ServiceHost, sf: BinderId) -> BinderId {
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
pub fn create_surface(host: &mut ServiceHost, client: BinderId, name: &str) -> (BinderId, i32) {
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

pub fn write_layer_metadata(data: &mut Parcel) {
    data.write_int32(1); // non-null Parcelable
    data.write_int32(0); // empty map
}

pub fn write_rect(data: &mut Parcel, r: [i32; 4]) {
    for v in r {
        data.write_int32(v);
    }
}

pub fn write_words(data: &mut Parcel, bytes: &[u8]) {
    for word in bytes.chunks(4) {
        let mut buf = [0u8; 4];
        buf[..word.len()].copy_from_slice(word);
        data.write_int32(i32::from_le_bytes(buf));
    }
}

/// The image's `gui::WindowInfo` (empty: just the presence word).
pub fn write_window_info(data: &mut Parcel, full: bool) {
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
pub fn write_light_blob(data: &mut Parcel, words: &[i32]) {
    data.write_int32((words.len() * 4) as i32);
    for w in words {
        data.write_int32(*w);
    }
}

/// `GraphicBuffer`: 13 header words + 9 shim handle ints + one memfd.
pub fn write_graphic_buffer(
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

/// The frame the probe sent: the shim's 88-byte wire form and the frame's fd.
pub struct RealBuf {
    pub flat: Vec<u8>,
    pub fd: OwnedFd,
    pub w: u32,
    pub h: u32,
    pub stride: u32,
    pub format: i32,
    pub checksum: u64,
}

pub static REAL_BUF: std::sync::Mutex<Option<RealBuf>> = std::sync::Mutex::new(None);

pub fn write_buffer_data_flat(
    data: &mut Parcel,
    rb: &RealBuf,
    frame_number: u64,
    buffer_id: u64,
    listener: BinderId,
) {
    data.write_int32(1); // Parcelable BufferData non-null
    data.write_int32(0x04 | 0x02 | 0x01); // flags: cached/frame/fence changed
    data.write_bool(true); // has buffer
    // the producer's own flatten bytes (88 = 13 words + 9 handle ints), verbatim
    data.write_int32(88); // flattened size
    data.write_int32(1); // fd count
    write_words(data, &rb.flat);
    data.write_file_descriptor(rb.fd.as_fd()).expect("fd");
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

#[allow(clippy::too_many_arguments)]
pub fn write_buffer_data(
    data: &mut Parcel,
    fd: &OwnedFd,
    width: u32,
    height: u32,
    frame_number: u64,
    buffer_id: u64,
    listener: BinderId,
) {
    data.write_int32(1); // Parcelable BufferData non-null
    data.write_int32(0x04 | 0x02 | 0x01); // flags: cached/frame/fence changed
    data.write_bool(true); // has buffer
    write_graphic_buffer(data, fd, width, height, width, 1, buffer_id);
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

#[allow(clippy::too_many_arguments)]
pub fn write_layer_state(
    data: &mut Parcel,
    handle: BinderId,
    layer_id: i32,
    what: u64,
    buffer: Option<(BinderId, u64, u64)>,
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
    data.write_byte(0); // frameRateCompatibility
    data.write_byte(0); // changeFrameRateStrategy
    data.write_byte(0); // defaultFrameRateCompatibility
    data.write_byte(0); // frameRateCategory
    data.write_bool(false); // frameRateCategorySmoothSwitchOnly
    data.write_byte(0); // frameRateSelectionStrategy
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

    if let Some((listener, frame_number, buffer_id)) = buffer {
        data.write_bool(true);
        let g = REAL_BUF.lock().unwrap();
        write_buffer_data_flat(data, g.as_ref().expect("real buffer"), frame_number, buffer_id, listener);
    } else {
        data.write_bool(false);
    }
    data.write_int32(1); // TrustedPresentationThresholds marker
    data.write_int32(12); // AIDL size = 3 words
    data.write_float(-1.0);
    data.write_float(-1.0);
    data.write_int32(0);
    data.write_int32(1); // TrustedPresentationListener marker
    data.write_strong_binder(BinderId(0));
    data.write_int32(-1);
    data.write_float(1.0); // currentHdrSdrRatio
    data.write_float(1.0); // desiredHdrSdrRatio
    data.write_int32(0); // cachingHint Enabled
}

pub fn test_memfd() -> OwnedFd {
    use std::sync::atomic::{AtomicU32, Ordering};
    static N: AtomicU32 = AtomicU32::new(0);
    let path = std::env::temp_dir().join(format!(
        "acos-c1-{}-{}.bin",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let file = std::fs::File::create(&path).expect("temp buffer");
    file.set_len(64 * 48 * 4).expect("size");
    let _ = std::fs::remove_file(&path);
    file.into()
}

pub type ComposerSpec = (
    BinderId,
    i32,
    Option<(BinderId, u64, u64)>,
    Vec<(BinderId, Vec<CallbackId>)>,
);

/// One concrete legacy transaction parcel with `count` composers.
pub fn transaction_parcel(
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

pub fn find_release(
    deliveries: &[acos_services::PendingDelivery],
    listener: BinderId,
) -> &acos_services::PendingDelivery {
    deliveries
        .iter()
        .find(|d| d.target == listener && d.code == 2)
        .expect("onReleaseBuffer delivery")
}

pub fn find_completed(
    deliveries: &[acos_services::PendingDelivery],
    listener: BinderId,
) -> &acos_services::PendingDelivery {
    deliveries
        .iter()
        .find(|d| d.target == listener && d.code == 1)
        .expect("onTransactionCompleted delivery")
}

pub fn score_state(state: &std::sync::Arc<std::sync::Mutex<SurfaceState>>) -> std::sync::MutexGuard<'_, SurfaceState> {
    state.lock().unwrap()
}

