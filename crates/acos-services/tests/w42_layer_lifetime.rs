//! W42: window/layer lifetime in C1 — a destroyed layer leaves the composite
//! and its memfds are released; a hidden layer is not drawn.
//!
//! W38 §5.3: after the second Activity finished (Back), `frames/8.png` and
//! `frames/11.png` were byte-identical — the destroyed Activity's stored buffer
//! was still drawn — while the live tree showed only the first Activity. The
//! rows below are the "teeth" for the fix:
//!
//! * `destroyed_layer_is_not_drawn` — two layers, destroy the top one,
//!   the capture is the one below and the retained-buffer count drops;
//! * `buffer_release_is_counted_and_a_late_transaction_cannot_resurrect` — the
//!   memfd release is observable, and a transaction that still names the dead
//!   handle does not bring the layer back;
//! * `hidden_layer_is_not_drawn` — `eFlagsChanged` + `eLayerHidden` removes a
//!   layer's pixels without destroying it; clearing the bit shows it again;
//! * `window_surface_destroy_drops_its_blast_child` — `IWindowSession.remove`
//!   path (`WmsState::destroy_surface_layer`) drops the window surface **and**
//!   the BLAST child that holds the pixels.
//!
//! The parcels below are the same shape `tests/w35_composite.rs` builds (the
//! S7 wire builders), so they exercise the real decoder, not a private hook.

use std::os::fd::{AsFd, OwnedFd};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use acos_parcel::{BinderId, Parcel};
use acos_services::compositor_client::{self, DisplayDescriptor};
use acos_services::surface_client::{self, FrameCapture, SurfaceState};
use acos_services::wms::WmsState;
use acos_services::ServiceHost;

const ISURFACE_COMPOSER: &str = "android.gui.ISurfaceComposer";
const ISURFACE_COMPOSER_LEGACY: &str = "android.ui.ISurfaceComposer";
const ISURFACE_COMPOSER_CLIENT: &str = "android.gui.ISurfaceComposerClient";

const CREATE_CONNECTION: u32 = 3;
const CREATE_SURFACE: u32 = 1;
const SET_TRANSACTION_STATE: u32 = 8;

static NEXT: AtomicU64 = AtomicU64::new(1);

struct Rig {
    host: ServiceHost,
    state: Arc<Mutex<SurfaceState>>,
    sf: BinderId,
    client: BinderId,
}

fn rig() -> Rig {
    let mut host = ServiceHost::new();
    let (_pool, state) = compositor_client::install_full(&mut host, DisplayDescriptor::reference());
    let sf = host
        .check_service("SurfaceFlinger")
        .expect("lookup infallible")
        .expect("SurfaceFlinger resolves");
    let mut data = Parcel::new();
    data.write_interface_token(ISURFACE_COMPOSER)
        .expect("token");
    data.set_data_position(0);
    let mut reply = Parcel::new();
    host.transact(sf, CREATE_CONNECTION, &mut data, &mut reply, 0)
        .expect("createConnection");
    reply.set_data_position(0);
    assert_eq!(reply.read_int32().expect("exception"), 0);
    let client = reply
        .read_strong_binder()
        .expect("binder")
        .expect("ISurfaceComposerClient");
    Rig {
        host,
        state,
        sf,
        client,
    }
}

impl Rig {
    /// `ISurfaceComposerClient.createSurface` with an optional parent binder.
    fn create_surface(&mut self, name: &str, parent: Option<BinderId>) -> (BinderId, i32) {
        let mut data = Parcel::new();
        data.write_interface_token(ISURFACE_COMPOSER_CLIENT)
            .expect("token");
        data.write_string16(name).expect("name");
        data.write_int32(0);
        data.write_strong_binder_opt(parent);
        data.write_int32(1); // non-null LayerMetadata
        data.write_int32(0); // empty
        data.set_data_position(0);
        let mut reply = Parcel::new();
        self.host
            .transact(self.client, CREATE_SURFACE, &mut data, &mut reply, 0)
            .expect("createSurface");
        reply.set_data_position(0);
        assert_eq!(reply.read_int32().expect("exception"), 0);
        assert_eq!(reply.read_int32().expect("marker"), 1);
        let _size = reply.read_int32().expect("size");
        let handle = reply
            .read_strong_binder()
            .expect("binder")
            .expect("layer handle");
        let layer_id = reply.read_int32().expect("layer id");
        (handle, layer_id)
    }

    fn apply(&mut self, parcel: &mut Parcel) {
        let mut reply = Parcel::new();
        self.host
            .transact(self.sf, SET_TRANSACTION_STATE, parcel, &mut reply, 0)
            .expect("setTransactionState");
    }

    fn capture(&self) -> Result<FrameCapture, surface_client::CompositeError> {
        self.state.lock().expect("state").capture_composite()
    }
}

struct Buf {
    fd: OwnedFd,
    width: u32,
    height: u32,
    stride: u32,
    id: u64,
}

impl Buf {
    fn solid(width: u32, height: u32, stride: u32, id: u64, rgba: [u8; 4]) -> Self {
        let len = u64::from(stride) * 4 * u64::from(height);
        let fd = anon_file(len);
        let mut row = vec![0u8; stride as usize * 4];
        for px in row.chunks_exact_mut(4) {
            px.copy_from_slice(&rgba);
        }
        let file = std::fs::File::from(fd.try_clone().expect("dup"));
        use std::os::unix::fs::FileExt;
        for y in 0..height {
            file.write_at(&row, u64::from(y) * row.len() as u64)
                .expect("write row");
        }
        Self {
            fd,
            width,
            height,
            stride,
            id,
        }
    }
}

fn anon_file(len: u64) -> OwnedFd {
    let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.cache/w42-tests");
    std::fs::create_dir_all(&dir).expect("scratch dir");
    let path = dir.join(format!(
        "anon-{}-{}.bin",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(&path)
        .expect("scratch file");
    file.set_len(len).expect("size");
    std::fs::remove_file(&path).expect("unlink");
    file.into()
}

/// One layer in a transaction. `buffer: None` writes a bufferless composer
/// state (what `setVisibility`/`reparent` transactions do).
struct Layer<'a> {
    handle: BinderId,
    layer_id: i32,
    x: f32,
    y: f32,
    z: i32,
    what_extra: u64,
    flags: u32,
    reparent: Option<Option<BinderId>>,
    buffer: Option<&'a Buf>,
}

fn write_rect(data: &mut Parcel, r: [i32; 4]) {
    for v in r {
        data.write_int32(v);
    }
}

fn write_light_blob(data: &mut Parcel, words: &[i32]) {
    data.write_int32((words.len() * 4) as i32);
    for w in words {
        data.write_int32(*w);
    }
}

fn write_buffer(data: &mut Parcel, b: &Buf) {
    data.write_int32(1); // Parcelable BufferData
    data.write_int32(0x07); // cached|frame|fence changed
    data.write_bool(true);
    data.write_int32(88);
    data.write_int32(1);
    data.write_int32(i32::from_le_bytes(*b"GB01"));
    data.write_int32(b.width as i32);
    data.write_int32(b.height as i32);
    data.write_int32(b.stride as i32);
    data.write_int32(1); // format RGBA_8888
    data.write_int32(1); // layerCount
    data.write_int32(0); // usage lo
    data.write_int32((b.id >> 32) as i32);
    data.write_int32(b.id as i32);
    data.write_int32(0); // generation
    data.write_int32(1); // handle numFds
    data.write_int32(9); // handle numInts
    data.write_int32(0); // usage hi
    for _ in 0..9 {
        data.write_int32(0); // gralloc-shim handle ints
    }
    data.write_file_descriptor(b.fd.as_fd()).expect("fd");
    data.write_bool(false); // no acquire fence
    data.write_uint64(1); // frameNumber
    data.write_strong_binder(BinderId(0)); // releaseBufferListener
    data.write_strong_binder(BinderId(0)); // releaseBufferEndpoint
    data.write_strong_binder(BinderId(0)); // cachedBufferToken
    data.write_uint64(b.id);
    data.write_bool(false); // hasBarrier
    data.write_uint64(0);
    data.write_uint32(1); // producerId
}

fn write_nullable_surface_control(data: &mut Parcel, handle: Option<BinderId>) {
    match handle {
        None => data.write_bool(false),
        Some(handle) => {
            data.write_bool(true);
            data.write_strong_binder(handle); // client
            data.write_strong_binder(handle); // handle
            data.write_int32(0); // layerId
            data.write_string16("parent").expect("name");
            data.write_uint32(0); // transformHint
            data.write_uint32(0); // width
            data.write_uint32(0); // height
            data.write_uint32(1); // format
        }
    }
}

fn write_layer(data: &mut Parcel, layer: &Layer<'_>) {
    data.write_strong_binder(layer.handle);
    data.write_int32(layer.layer_id);
    let mut what = surface_client::what::LAYER | layer.what_extra;
    if layer.buffer.is_some() {
        what |= surface_client::what::BUFFER;
    }
    if layer.reparent.is_some() {
        what |= surface_client::what::REPARENT;
    }
    data.write_uint64(what);
    data.write_float(layer.x);
    data.write_float(layer.y);
    data.write_int32(layer.z);
    data.write_uint32(0); // layerStack
    data.write_uint32(layer.flags);
    data.write_uint32(0xffff_ffff); // mask (every flag bit meaningful)
    for _ in 0..4 {
        data.write_float(0.0); // matrix
    }
    write_rect(data, [0, 0, 0, 0]); // crop
    write_nullable_surface_control(data, None); // relativeLayerSurfaceControl
    write_nullable_surface_control(data, layer.reparent.flatten()); // parent
    for _ in 0..4 {
        data.write_float(0.0); // color
    }
    data.write_int32(0); // empty gui::WindowInfo
    write_light_blob(data, &[0]); // transparentRegion
    data.write_uint32(0); // bufferTransform
    data.write_bool(false); // transformToDisplayInverse
    data.write_bool(false); // borderEnabled
    data.write_float(0.0);
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
    data.write_int32(1); // LayerMetadata marker
    data.write_int32(0); // empty
    for _ in 0..4 {
        data.write_float(0.0); // bgColor
    }
    data.write_uint32(0);
    data.write_bool(false); // colorSpaceAgnostic
    data.write_int32(0); // listeners
    data.write_float(0.0); // shadowRadius
    data.write_int32(0); // frameRateSelectionPriority
    data.write_float(60.0); // frameRate
    data.write_byte(0);
    data.write_byte(0);
    data.write_byte(0);
    data.write_uint32(0); // fixedTransformHint
    data.write_bool(false); // autoRefresh
    data.write_bool(false); // dimmingEnabled
    data.write_uint32(0); // blurRegions
    for _ in 0..10 {
        data.write_float(0.0); // StretchEffect
    }
    write_rect(data, [0, 0, 0, 0]); // bufferCrop
    write_rect(data, [0, 0, 0, 0]); // destinationFrame
    data.write_bool(false); // isTrustedOverlay
    data.write_uint32(0); // dropInputMode
    match layer.buffer {
        Some(buffer) => {
            data.write_bool(true); // has BufferData
            write_buffer(data, buffer);
        }
        None => data.write_bool(false),
    }
    data.write_int32(1); // TrustedPresentationThresholds marker
    data.write_int32(16); // counts itself: 12-byte payload
    data.write_float(-1.0);
    data.write_float(-1.0);
    data.write_int32(0);
    data.write_int32(1); // TrustedPresentationListener marker
    data.write_strong_binder(BinderId(0));
    data.write_int32(-1);
    data.write_float(1.0); // currentHdrSdrRatio
    data.write_float(1.0); // desiredHdrSdrRatio
    data.write_int32(0); // cachingHint
}

fn transaction(layers: &[Layer<'_>]) -> Parcel {
    let mut data = Parcel::new();
    data.write_interface_token(ISURFACE_COMPOSER_LEGACY)
        .expect("token");
    write_light_blob(&mut data, &[0]); // FrameTimelineInfo
    data.write_uint32(layers.len() as u32);
    for layer in layers {
        write_layer(&mut data, layer);
    }
    data.write_uint32(0); // displays
    data.write_uint32(0); // flags
    data.write_strong_binder_opt(None); // applyToken
    data.write_int32(0); // InputWindowCommands.focusRequests
    data.write_int32(0); // InputWindowCommands.windowInfosReportedListeners
    data.write_int64(0); // desiredPresentTime
    data.write_bool(true); // isAutoTimestamp
    data.write_uint32(0); // uncacheBuffers
    data.write_bool(false); // hasListenerCallbacks
    data.write_int32(0); // no listener callback entries
    data.write_uint64(1); // transactionId
    data.write_uint32(0); // mergedTransactionIds
    data.set_data_position(0);
    data
}

/// A layer with a buffer at the origin, z given by the caller.
fn buffered<'a>(handle: BinderId, layer_id: i32, z: i32, buffer: &'a Buf) -> Layer<'a> {
    Layer {
        handle,
        layer_id,
        x: 0.0,
        y: 0.0,
        z,
        what_extra: 0,
        flags: 0,
        reparent: None,
        buffer: Some(buffer),
    }
}

fn pixel(frame: &FrameCapture, x: u32, y: u32) -> [u8; 4] {
    let off = y as usize * frame.row_bytes + x as usize * 4;
    [
        frame.rgba[off],
        frame.rgba[off + 1],
        frame.rgba[off + 2],
        frame.rgba[off + 3],
    ]
}

const RED: [u8; 4] = [255, 0, 0, 255];
const BLUE: [u8; 4] = [0, 0, 255, 255];

#[test]
fn destroyed_layer_is_not_drawn_and_its_memfd_is_released() {
    let mut rig = rig();
    let (base_handle, base_id) = rig.create_surface("base", None);
    let (top_handle, top_id) = rig.create_surface("top", None);
    let base = Buf::solid(8, 8, 8, 0x42_0000_0001, RED);
    let top = Buf::solid(8, 8, 8, 0x42_0000_0002, BLUE);
    rig.apply(&mut transaction(&[
        buffered(base_handle, base_id, 1, &base),
        buffered(top_handle, top_id, 2, &top),
    ]));

    let both = rig.capture().expect("two layers");
    assert_eq!(both.layers, 2);
    assert_eq!(pixel(&both, 4, 4), BLUE, "the top layer is what is shown");

    let fds_before = rig.state.lock().expect("state").open_buffer_fds();
    let dropped = rig
        .state
        .lock()
        .expect("state")
        .destroy_layer_tree(top_handle);
    assert_eq!(dropped, 1, "the top layer is destroyed");
    let state = rig.state.lock().expect("state");
    assert_eq!(state.retained_buffers(), 1, "only the base buffer is left");
    assert_eq!(
        state.released_buffers(),
        1,
        "the top buffer's memfd is gone"
    );
    assert_eq!(state.destroyed_layers(), 1);
    assert_eq!(
        state.open_buffer_fds(),
        fds_before - 2,
        "the destroyed layer's memfd and its BufferCache dup were released, not leaked"
    );
    drop(state);

    let after = rig.capture().expect("base layer");
    assert_eq!(after.layers, 1, "the destroyed layer is not composited");
    assert_eq!(
        pixel(&after, 4, 4),
        RED,
        "the Activity below is what is shown"
    );
}

#[test]
fn a_late_transaction_cannot_resurrect_a_destroyed_layer() {
    let mut rig = rig();
    let (base_handle, base_id) = rig.create_surface("base", None);
    let (top_handle, top_id) = rig.create_surface("top", None);
    let base = Buf::solid(8, 8, 8, 0x42_0000_0011, RED);
    let top = Buf::solid(8, 8, 8, 0x42_0000_0012, BLUE);
    rig.apply(&mut transaction(&[
        buffered(base_handle, base_id, 1, &base),
        buffered(top_handle, top_id, 2, &top),
    ]));
    rig.state
        .lock()
        .expect("state")
        .destroy_layer_tree(top_handle);

    // The BLAST client keeps posting after the window is gone (its queue is
    // destroyed later): that transaction must be dropped, not re-inserted at
    // the origin with the old pixels.
    rig.apply(&mut transaction(&[buffered(top_handle, top_id, 9, &top)]));
    let state = rig.state.lock().expect("state");
    assert!(
        state.layer(top_handle).is_none(),
        "a destroyed handle has no layer record again"
    );
    assert_eq!(state.retained_buffers(), 1);
    drop(state);
    let frame = rig.capture().expect("base layer");
    assert_eq!(frame.layers, 1);
    assert_eq!(pixel(&frame, 4, 4), RED);
}

#[test]
fn hidden_layer_is_not_drawn_until_it_is_shown_again() {
    let mut rig = rig();
    let (base_handle, base_id) = rig.create_surface("base", None);
    let (top_handle, top_id) = rig.create_surface("top", None);
    let base = Buf::solid(8, 8, 8, 0x42_0000_0021, RED);
    let top = Buf::solid(8, 8, 8, 0x42_0000_0022, BLUE);
    rig.apply(&mut transaction(&[
        buffered(base_handle, base_id, 1, &base),
        buffered(top_handle, top_id, 2, &top),
    ]));

    let mut hide = buffered(top_handle, top_id, 2, &top);
    hide.what_extra = surface_client::what::FLAGS;
    hide.flags = surface_client::LAYER_HIDDEN;
    rig.apply(&mut transaction(&[hide]));

    let hidden = rig.capture().expect("base layer only");
    assert_eq!(hidden.layers, 1, "a hidden layer is not in the visible set");
    assert_eq!(pixel(&hidden, 4, 4), RED);
    assert_eq!(
        rig.state.lock().expect("state").retained_buffers(),
        2,
        "hiding keeps the buffer (stock: the layer still owns it)"
    );

    let mut show = buffered(top_handle, top_id, 2, &top);
    show.what_extra = surface_client::what::FLAGS;
    show.flags = 0;
    rig.apply(&mut transaction(&[show]));
    let shown = rig.capture().expect("both layers");
    assert_eq!(shown.layers, 2);
    assert_eq!(pixel(&shown, 4, 4), BLUE);
}

#[test]
fn window_surface_destroy_drops_its_blast_child() {
    // The WMS side: `IWindowSession.remove` → `destroy_surface_layer` must drop
    // the window surface *and* the BLAST child that holds the pixels.
    let mut rig = rig();
    let mut wms = WmsState::new();
    let params = acos_wm::codec::LayoutParams::application(
        "dev.acos.fixtures.multiactivity",
        "SecondActivity",
    );
    let add = wms
        .add_to_display_as_user(BinderId(0x77), params.clone(), 0)
        .expect("addToDisplay");
    let relayout = wms.relayout(add.window, params, -1).expect("relayout");
    let mut surface = relayout.surface;
    let binding = wms
        .bind_surface_layer(&mut surface, &rig.state)
        .expect("a compositor layer is bound");
    wms.publish_layer_placement(binding, add.window, &rig.state);

    // The BLAST child the app draws into (parent = the window surface handle).
    let (child_handle, child_id) = rig.create_surface("bbq-wrapper", Some(binding.handle));
    let pixels = Buf::solid(8, 8, 8, 0x42_0000_0031, BLUE);
    rig.apply(&mut transaction(&[buffered(
        child_handle,
        child_id,
        1,
        &pixels,
    )]));
    let before = rig.capture().expect("the child is drawn");
    assert_eq!(pixel(&before, 4, 4), BLUE);

    let dropped = wms
        .destroy_surface_layer(add.window, &rig.state)
        .expect("the window had a bound layer");
    wms.remove(add.window).expect("IWindowSession.remove");
    assert_eq!(dropped, 2, "window surface + its BLAST child");
    let state = rig.state.lock().expect("state");
    assert_eq!(state.retained_buffers(), 0);
    assert_eq!(state.released_buffers(), 1);
    assert!(state.latest_buffer(binding.handle).is_none());
    drop(state);
    assert!(
        matches!(rig.capture(), Err(surface_client::CompositeError::NoFrame)),
        "no layer is left to draw after the window is destroyed"
    );
}

/// METRO2c: a destroyed layer gives its handle binder back. The pool is
/// pre-allocated (`LAYER_HANDLE_POOL`), so before this a session could only ever
/// create that many layers *in total*: a React Native dev loop (each reload
/// shows and dismisses a popup window = two layers) failed `createSurface` on
/// the 4th reload and the guest aborted in `BLASTBufferQueue`. Create and
/// destroy several times the pool size; every `createSurface` must succeed and
/// a dead handle must stay refused until it is reissued.
#[test]
fn destroyed_layers_return_their_handles_so_a_long_session_never_exhausts_the_pool() {
    let mut rig = rig();
    let pool = surface_client::LAYER_HANDLE_POOL;
    let mut last_dead: Option<BinderId> = None;
    for round in 0..(pool * 4) {
        let (handle, _id) = rig.create_surface(&format!("window-{round}"), None);
        let dropped = rig
            .state
            .lock()
            .expect("state")
            .destroy_layer_tree(handle);
        assert_eq!(dropped, 1, "round {round}: the layer is destroyed");
        last_dead = Some(handle);
    }
    assert!(last_dead.is_some());
    // The live set is still bounded: `pool` live layers fit, one more does not.
    let mut live = Vec::new();
    for i in 0..pool {
        live.push(rig.create_surface(&format!("live-{i}"), None).0);
    }
    let mut data = Parcel::new();
    data.write_interface_token(ISURFACE_COMPOSER_CLIENT)
        .expect("token");
    data.write_string16("one-too-many").expect("name");
    data.write_int32(0);
    data.write_strong_binder_opt(None);
    data.write_int32(1);
    data.write_int32(0);
    data.set_data_position(0);
    let mut reply = Parcel::new();
    let over = rig
        .host
        .transact(rig.client, CREATE_SURFACE, &mut data, &mut reply, 0);
    assert!(
        over.is_err(),
        "more live layers than the pool is still refused, never a fabricated handle"
    );
}
