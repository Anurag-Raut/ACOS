//! B1b: the recorded dialog shape (window surface + BLAST child carrying
//! `destinationFrame`, buffer 776 then 1024 wide) through C1's real transaction
//! decoder and the AOSP geometry kernel.
//!
//! The window manager's window surface is a layer with **no buffer** at
//! `(57,640)`; the BLAST child under it owns the pixels and a destination frame
//! `(0,0,466,400)`. The kernel composes parent∘local, so the child lands at
//! `(457,840)` and its buffer is scaled by `466/buffer_width`
//! (`RequestedLayerState.cpp:388-396`, `LayerSnapshotBuilder.cpp:948,988`).
//! The composite therefore has exactly the destination rectangle's size, and
//! the dialog rectangle is invariant to the buffer width — the W54 fix, now
//! produced by the kernel rather than by `placement_of`.

use std::os::fd::{AsFd, OwnedFd};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use acos_parcel::{BinderId, Parcel};
use acos_services::compositor_client::{self, DisplayDescriptor};
use acos_services::surface_client::{self, FrameCapture, LayerPlacement, SurfaceState};
use acos_services::ServiceHost;

const ISURFACE_COMPOSER: &str = "android.gui.ISurfaceComposer";
const ISURFACE_COMPOSER_LEGACY: &str = "android.ui.ISurfaceComposer";
const CREATE_CONNECTION: u32 = 3;
const SET_TRANSACTION_STATE: u32 = 8;

static NEXT: AtomicU64 = AtomicU64::new(1);

const RED: [u8; 4] = [255, 0, 0, 255];
const BLUE: [u8; 4] = [0, 0, 255, 255];

struct Rig {
    host: ServiceHost,
    state: Arc<Mutex<SurfaceState>>,
    sf: BinderId,
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
    let _client = reply.read_strong_binder().expect("binder").expect("client");
    Rig { host, state, sf }
}

impl Rig {
    fn transact(&mut self, parcel: &mut Parcel) {
        let mut reply = Parcel::new();
        self.host
            .transact(self.sf, SET_TRANSACTION_STATE, parcel, &mut reply, 0)
            .expect("transaction decodes");
    }

    fn capture(&self) -> Result<FrameCapture, surface_client::CompositeError> {
        self.state.lock().expect("state").capture_composite()
    }
}

/// A buffer whose left half is red and right half is blue (one pixel column of
/// split at `width / 2`).
struct Buf {
    fd: OwnedFd,
    width: u32,
    height: u32,
    stride: u32,
    id: u64,
}

impl Buf {
    fn split(width: u32, height: u32, id: u64) -> Self {
        let stride = width;
        let len = u64::from(stride) * 4 * u64::from(height);
        let fd = anon_file(len);
        let mut row = vec![0u8; stride as usize * 4];
        for (x, px) in row.chunks_exact_mut(4).enumerate() {
            px.copy_from_slice(if (x as u32) < width / 2 { &RED } else { &BLUE });
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
    let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.cache/b1b-tests");
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
    data.write_int32(1); // RGBA_8888
    data.write_int32(1); // layerCount
    data.write_int32(0); // usage lo
    data.write_int32((b.id >> 32) as i32);
    data.write_int32(b.id as i32);
    data.write_int32(0); // generation
    data.write_int32(1); // handle numFds
    data.write_int32(9); // handle numInts
    data.write_int32(0); // usage hi
    for _ in 0..9 {
        data.write_int32(0);
    }
    data.write_file_descriptor(b.fd.as_fd()).expect("fd");
    data.write_bool(false); // no acquire fence
    data.write_uint64(1); // frameNumber
    data.write_strong_binder(BinderId(0));
    data.write_strong_binder(BinderId(0));
    data.write_strong_binder(BinderId(0));
    data.write_uint64(b.id);
    data.write_bool(false); // hasBarrier
    data.write_uint64(0);
    data.write_uint32(1); // producerId
}

/// A child layer transaction: the pixels, its destination frame, and position 0.
fn write_child(data: &mut Parcel, handle: BinderId, layer_id: i32, b: &Buf, dest: [i32; 4]) {
    data.write_strong_binder(handle);
    data.write_int32(layer_id);
    data.write_uint64(
        surface_client::what::BUFFER
            | surface_client::what::LAYER
            | surface_client::what::DESTINATION_FRAME,
    );
    data.write_float(0.0);
    data.write_float(0.0);
    data.write_int32(0); // z (the window surface's placement supplies stacking)
    data.write_uint32(0);
    data.write_uint32(0);
    data.write_uint32(0);
    for _ in 0..4 {
        data.write_float(0.0); // matrix (not latched without eMatrixChanged)
    }
    write_rect(data, [0, 0, -1, -1]); // crop empty
    data.write_bool(false); // relativeLayerSurfaceControl
    data.write_bool(false); // parentSurfaceControlForChild
    for _ in 0..4 {
        data.write_float(0.0); // color
    }
    data.write_int32(0); // empty WindowInfo
    write_light_blob(data, &[0]); // transparentRegion
    data.write_uint32(0); // bufferTransform
    data.write_bool(false); // transformToDisplayInverse
    data.write_bool(false);
    data.write_float(0.0);
    for _ in 0..4 {
        data.write_float(0.0);
    }
    data.write_uint32(0); // dataspace
    write_light_blob(data, &[0]); // hdrMetadata
    write_light_blob(data, &[0]); // surfaceDamageRegion
    data.write_int32(0); // api
    data.write_bool(false); // sideband
    for _ in 0..16 {
        data.write_float(0.0); // colorTransform
    }
    data.write_float(0.0); // cornerRadius
    data.write_uint32(0); // backgroundBlurRadius
    data.write_int32(1); // LayerMetadata marker
    data.write_int32(0); // empty
    for _ in 0..4 {
        data.write_float(0.0);
    }
    data.write_uint32(0);
    data.write_bool(false);
    data.write_int32(0); // listeners
    data.write_float(0.0); // shadowRadius
    data.write_int32(0);
    data.write_float(60.0);
    data.write_byte(0);
    data.write_byte(0);
    data.write_byte(0);
    data.write_uint32(0);
    data.write_bool(false);
    data.write_bool(false);
    data.write_uint32(0); // blurRegions
    for _ in 0..10 {
        data.write_float(0.0);
    }
    write_rect(data, [0, 0, -1, -1]); // bufferCrop empty
    write_rect(data, dest); // destinationFrame
    data.write_bool(false);
    data.write_uint32(0);
    data.write_bool(true); // has BufferData
    write_buffer(data, b);
    data.write_int32(1);
    data.write_int32(16);
    data.write_float(-1.0);
    data.write_float(-1.0);
    data.write_int32(0);
    data.write_int32(1);
    data.write_strong_binder(BinderId(0));
    data.write_int32(-1);
    data.write_float(1.0);
    data.write_float(1.0);
    data.write_int32(0);
}

fn begin_transaction(data: &mut Parcel) {
    data.write_interface_token(ISURFACE_COMPOSER_LEGACY)
        .expect("token");
    write_light_blob(data, &[0]); // FrameTimelineInfo
    data.write_uint32(1); // one composer
}

fn end_transaction(data: &mut Parcel) {
    data.write_uint32(0); // displays
    data.write_uint32(0); // flags
    data.write_strong_binder_opt(None); // applyToken
    data.write_int32(0); // focusRequests
    data.write_int32(0); // windowInfosReportedListeners
    data.write_int64(0); // desiredPresentTime
    data.write_bool(true); // isAutoTimestamp
    data.write_uint32(0); // uncacheBuffers
    data.write_bool(false); // hasListenerCallbacks
    data.write_int32(0);
    data.write_uint64(1); // transactionId
    data.write_uint32(0); // mergedTransactionIds
    data.set_data_position(0);
}

fn pixel(frame: &FrameCapture, x: u32, y: u32) -> [u8; 4] {
    let off = y as usize * frame.row_bytes + x as usize * 4;
    let mut px = [0u8; 4];
    px.copy_from_slice(&frame.rgba[off..off + 4]);
    px
}

/// Builds the window-surface (no buffer) + BLAST child pair once; returns the
/// child handle/layer id and the buffer id counter.
fn setup() -> (Rig, BinderId, i32) {
    let r = rig();
    let (win_handle, child_handle, child_id) = {
        let mut state = r.state.lock().expect("state");
        let (win_handle, _win_id) = state
            .create_layer("window-surface", 1080, 2400, 1, 0, None)
            .expect("window layer");
        let (child_handle, child_id) = state
            .create_layer("bbq-wrapper", 0, 0, 1, 0, Some(win_handle))
            .expect("blast child");
        state.set_layer_placement(
            win_handle,
            LayerPlacement {
                x: 57,
                y: 640,
                z: 1,
                alpha: 1.0,
                dim_amount: 0.0,
            },
        );
        (win_handle, child_handle, child_id)
    };
    let _ = win_handle;
    (r, child_handle, child_id)
}

fn composite_dialog(buffer_width: u32) -> FrameCapture {
    let (mut r, child_handle, child_id) = setup();
    let b = Buf::split(buffer_width, 400, 0x3_0000_0000 + u64::from(buffer_width));
    let mut data = Parcel::new();
    begin_transaction(&mut data);
    write_child(&mut data, child_handle, child_id, &b, [0, 0, 466, 400]);
    end_transaction(&mut data);
    r.transact(&mut data);
    r.capture().expect("the dialog composites")
}

#[test]
fn a_recorded_dialog_transaction_lands_in_one_466x400_rect_for_776_and_1024() {
    for buffer_width in [776u32, 1024] {
        let frame = composite_dialog(buffer_width);
        // The kernel maps the destination frame to display pixels
        // (RequestedLayerState.cpp:388-396 => sx = 466/bufW; LSB.cpp:988), and
        // the composite is the destination rect clipped to the display, so its
        // size is the window's 466x400 regardless of the buffer width.
        assert_eq!(
            (frame.width, frame.height),
            (466, 400),
            "buffer {buffer_width}: the composite rect is the destination frame"
        );
        assert_eq!(frame.layers, 1);
        // Nearest-neighbour scaling: the source split at bufW/2 maps to the same
        // half of the destination for both buffer widths.
        assert_eq!(pixel(&frame, 0, 0), RED, "buffer {buffer_width}: left edge");
        assert_eq!(
            pixel(&frame, 465, 0),
            BLUE,
            "buffer {buffer_width}: right edge"
        );
        assert_eq!(
            pixel(&frame, 0, 399),
            RED,
            "buffer {buffer_width}: bottom-left"
        );
        assert_eq!(
            pixel(&frame, 465, 399),
            BLUE,
            "buffer {buffer_width}: bottom-right"
        );
    }
}

#[test]
fn the_dialog_rect_is_identical_for_both_buffer_widths() {
    let narrow = composite_dialog(776);
    let wide = composite_dialog(1024);
    assert_eq!((narrow.width, narrow.height), (wide.width, wide.height));
    // The destination rect only differs in which source columns are sampled,
    // not where it lands: the frame is the same size and the split stays at the
    // same display column (466/2 = 233).
    for y in [0, 100, 399] {
        assert_eq!(pixel(&narrow, 232, y), RED, "narrow left of the split");
        assert_eq!(pixel(&narrow, 233, y), BLUE, "narrow right of the split");
        assert_eq!(pixel(&wide, 232, y), RED, "wide left of the split");
        assert_eq!(pixel(&wide, 233, y), BLUE, "wide right of the split");
    }
}
