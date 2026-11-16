//! W35: the C1 capture is the **composite of every visible layer in z-order**.
//!
//! W10–W32 captured the newest buffer of one layer, so a dialog (its own
//! top-level `TYPE_APPLICATION` window with its own compositor layer above the
//! Activity — W32 §1) never showed up in the frame. The rows below build the
//! same `setTransactionState` parcels a real BLAST client sends (the S7
//! `sandbox_c1` wire builders) and assert **exact pixels**: two synthetic
//! buffers at different z and different positions, a dim layer, alpha blending,
//! and the caps that bound a hostile guest.
//!
//! Teeth: against the pre-W35 capture (`newest_frame_layer` only) every
//! composite row shows one layer's buffer, so `layers == 1` and the second
//! layer's pixels are absent.

use std::os::fd::{AsFd, OwnedFd};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use acos_parcel::{BinderId, Parcel};
use acos_services::compositor_client::{self, DisplayDescriptor};
use acos_services::surface_client::{
    self, CompositeError, FrameCapture, LayerPlacement, SurfaceState, MAX_COMPOSITE_LAYERS,
};
use acos_services::ServiceHost;

const ISURFACE_COMPOSER: &str = "android.gui.ISurfaceComposer";
const ISURFACE_COMPOSER_LEGACY: &str = "android.ui.ISurfaceComposer";
const ISURFACE_COMPOSER_CLIENT: &str = "android.gui.ISurfaceComposerClient";

const CREATE_CONNECTION: u32 = 3;
const CREATE_SURFACE: u32 = 1;
const SET_TRANSACTION_STATE: u32 = 8;

static NEXT: AtomicU64 = AtomicU64::new(1);

/// One installed compositor, ready to receive transactions.
struct Rig {
    host: ServiceHost,
    state: Arc<Mutex<SurfaceState>>,
    sf: BinderId,
    client: BinderId,
    next_layer: i32,
}

fn rig() -> Rig {
    let mut host = ServiceHost::new();
    let (_pool, state) = compositor_client::install_full(&mut host, DisplayDescriptor::reference());
    let sf = host
        .check_service("SurfaceFlinger")
        .expect("lookup infallible")
        .expect("SurfaceFlinger resolves");

    let mut data = Parcel::new();
    data.write_interface_token(ISURFACE_COMPOSER).expect("token");
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
        next_layer: 1,
    }
}

impl Rig {
    /// `ISurfaceComposerClient.createSurface` for a named layer.
    fn create_surface(&mut self, name: &str) -> (BinderId, i32) {
        let mut data = Parcel::new();
        data.write_interface_token(ISURFACE_COMPOSER_CLIENT)
            .expect("token");
        data.write_string16(name).expect("name");
        data.write_int32(0);
        data.write_strong_binder_opt(None);
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
        self.next_layer += 1;
        (handle, layer_id)
    }

    fn transact(&mut self, parcel: &mut Parcel) -> Result<(), acos_services::ServiceError> {
        let mut reply = Parcel::new();
        self.host
            .transact(self.sf, SET_TRANSACTION_STATE, parcel, &mut reply, 0)
    }

    fn capture(&self) -> Result<FrameCapture, CompositeError> {
        self.state.lock().expect("state").capture_composite()
    }
}

/// A guest-shaped `GraphicBuffer` holding `pixels` (stride × height RGBA).
struct Buf {
    fd: OwnedFd,
    width: u32,
    height: u32,
    stride: u32,
    id: u64,
}

impl Buf {
    /// A buffer filled with one RGBA colour.
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

/// One layer in a transaction: where the client puts it and what it draws.
struct Layer<'a> {
    handle: BinderId,
    layer_id: i32,
    x: f32,
    y: f32,
    z: i32,
    buffer: &'a Buf,
}

fn anon_file(len: u64) -> OwnedFd {
    let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.cache/w35-tests");
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

fn write_layer(data: &mut Parcel, layer: &Layer<'_>) {
    data.write_strong_binder(layer.handle);
    data.write_int32(layer.layer_id);
    data.write_uint64(surface_client::what::BUFFER | surface_client::what::LAYER);
    data.write_float(layer.x);
    data.write_float(layer.y);
    data.write_int32(layer.z);
    data.write_uint32(0); // layerStack
    data.write_uint32(0); // flags
    data.write_uint32(0); // mask
    for _ in 0..4 {
        data.write_float(0.0); // matrix
    }
    write_rect(data, [0, 0, 0, 0]); // crop
    data.write_bool(false); // relativeLayerSurfaceControl
    data.write_bool(false); // parentSurfaceControlForChild
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
    data.write_bool(true); // has BufferData
    write_buffer(data, layer.buffer);
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

fn pixel(frame: &FrameCapture, x: u32, y: u32) -> [u8; 4] {
    let off = y as usize * frame.row_bytes + x as usize * 4;
    let mut px = [0u8; 4];
    px.copy_from_slice(&frame.rgba[off..off + 4]);
    px
}

const RED: [u8; 4] = [255, 0, 0, 255];
const BLUE: [u8; 4] = [0, 0, 255, 255];

#[test]
fn two_layers_at_different_z_and_positions_composite_to_exact_pixels() {
    let mut r = rig();
    let (handle_a, id_a) = r.create_surface("activity");
    let (handle_b, id_b) = r.create_surface("dialog");
    let a = Buf::solid(4, 4, 4, 0x2_0000_0001, RED);
    let b = Buf::solid(4, 4, 4, 0x2_0000_0002, BLUE);
    // The Activity at the origin (z 0), the dialog offset by (2,2) and above it.
    let mut parcel = transaction(&[
        Layer {
            handle: handle_a,
            layer_id: id_a,
            x: 0.0,
            y: 0.0,
            z: 0,
            buffer: &a,
        },
        Layer {
            handle: handle_b,
            layer_id: id_b,
            x: 2.0,
            y: 2.0,
            z: 1,
            buffer: &b,
        },
    ]);
    r.transact(&mut parcel).expect("transaction decodes");

    let frame = r.capture().expect("a composite");
    assert_eq!((frame.width, frame.height), (6, 6), "union of the frames");
    assert_eq!(frame.layers, 2, "both layers are in the frame");
    assert!(frame.composite);
    assert_eq!(frame.row_bytes, 6 * 4, "the composite packs its rows");
    // Exact pixels over the whole 6x6 canvas: the dialog (z=1, at (2,2)) wins
    // where it overlaps, the Activity covers its own 4x4 at the origin, and
    // nothing is invented where no layer draws.
    for y in 0..6 {
        for x in 0..6 {
            let expected = if x >= 2 && y >= 2 {
                BLUE
            } else if x < 4 && y < 4 {
                RED
            } else {
                [0, 0, 0, 0]
            };
            assert_eq!(pixel(&frame, x, y), expected, "row {y} col {x}");
        }
    }
    assert_eq!(
        frame.non_zero_pixels,
        4 * 4 + 4 * 4 - 2 * 2,
        "exactly the union of the two layers' pixels is non-zero (the dialog's \
         2x2 overlap is drawn once, not summed)"
    );
}

#[test]
fn the_window_managers_dim_layer_darkens_what_is_below_it() {
    let mut r = rig();
    let (handle_a, id_a) = r.create_surface("activity");
    let (handle_b, id_b) = r.create_surface("dialog");
    let a = Buf::solid(4, 4, 4, 0x2_0000_0011, [200, 100, 50, 255]);
    let b = Buf::solid(2, 2, 2, 0x2_0000_0012, [10, 20, 30, 255]);
    let mut parcel = transaction(&[
        Layer {
            handle: handle_a,
            layer_id: id_a,
            x: 0.0,
            y: 0.0,
            z: 0,
            buffer: &a,
        },
        Layer {
            handle: handle_b,
            layer_id: id_b,
            x: 0.0,
            y: 0.0,
            z: 1,
            buffer: &b,
        },
    ]);
    r.transact(&mut parcel).expect("transaction decodes");

    // The host's WMS pushes the dialog's frame/z and its `FLAG_DIM_BEHIND`
    // `dimAmount` (0.5) — the values the image sends in `LayoutParams`.
    {
        let mut state = r.state.lock().expect("state");
        assert!(state.set_layer_placement(
            handle_b,
            LayerPlacement {
                x: 0,
                y: 0,
                z: 1,
                alpha: 1.0,
                dim_amount: 0.5,
                secure: false,
            },
        ));
    }
    let frame = r.capture().expect("a composite");
    assert_eq!((frame.width, frame.height), (4, 4));
    // Inside the dialog: its own pixels, undimmed.
    assert_eq!(pixel(&frame, 0, 0), [10, 20, 30, 255]);
    assert_eq!(pixel(&frame, 1, 1), [10, 20, 30, 255]);
    // Behind it: the Activity at half brightness (200,100,50) * 0.5.
    assert_eq!(pixel(&frame, 3, 3), [100, 50, 25, 255]);
    assert_eq!(pixel(&frame, 2, 0), [100, 50, 25, 255]);
}

/// VIEW1c L1: the compositor knows `FLAG_SECURE` from the window manager's
/// placement, so the live view can blank its frame while a secure window shows.
#[test]
fn a_visible_flag_secure_window_is_reported_so_the_live_view_can_blank_frames() {
    let mut r = rig();
    let (handle, id) = r.create_surface("login");
    let buffer = Buf::solid(2, 2, 2, 0x2_0000_0013, [1, 2, 3, 255]);
    let mut parcel = transaction(&[Layer {
        handle,
        layer_id: id,
        x: 0.0,
        y: 0.0,
        z: 0,
        buffer: &buffer,
    }]);
    r.transact(&mut parcel).expect("transaction decodes");
    let placement = |secure| LayerPlacement {
        x: 0,
        y: 0,
        z: 0,
        alpha: 1.0,
        dim_amount: 0.0,
        secure,
    };
    let mut state = r.state.lock().expect("state");
    assert!(!state.has_secure_window(), "no placement: not secure");
    assert!(state.set_layer_placement(handle, placement(false)));
    assert!(!state.has_secure_window());
    assert!(state.set_layer_placement(handle, placement(true)));
    assert!(state.has_secure_window(), "a visible FLAG_SECURE window");
    assert!(state.set_layer_hidden(handle, true));
    assert!(!state.has_secure_window(), "a hidden secure window shows nothing");
    assert!(state.set_layer_hidden(handle, false));
    assert!(state.has_secure_window());
    assert!(state.set_layer_placement(handle, placement(false)));
    assert!(!state.has_secure_window(), "FLAG_SECURE cleared again");
}

#[test]
fn a_translucent_window_blends_over_the_layer_below() {
    let mut r = rig();
    let (handle_a, id_a) = r.create_surface("activity");
    let (handle_b, id_b) = r.create_surface("dialog");
    let a = Buf::solid(2, 2, 2, 0x2_0000_0021, [0, 0, 0, 255]);
    let b = Buf::solid(2, 2, 2, 0x2_0000_0022, [255, 255, 255, 255]);
    let mut parcel = transaction(&[
        Layer {
            handle: handle_a,
            layer_id: id_a,
            x: 0.0,
            y: 0.0,
            z: 0,
            buffer: &a,
        },
        Layer {
            handle: handle_b,
            layer_id: id_b,
            x: 0.0,
            y: 0.0,
            z: 1,
            buffer: &b,
        },
    ]);
    r.transact(&mut parcel).expect("transaction decodes");
    {
        let mut state = r.state.lock().expect("state");
        assert!(state.set_layer_placement(
            handle_b,
            LayerPlacement {
                x: 0,
                y: 0,
                z: 1,
                alpha: 0.5,
                dim_amount: 0.0,
                secure: false,
            },
        ));
    }
    let frame = r.capture().expect("a composite");
    // 255 * 128/255 over black = 128, exactly (alpha 0.5 rounds to 128/255).
    assert_eq!(pixel(&frame, 0, 0), [128, 128, 128, 255]);
}

#[test]
fn a_single_full_window_layer_passes_through_with_its_own_stride() {
    let mut r = rig();
    let (handle, id) = r.create_surface("activity");
    let a = Buf::solid(8, 4, 12, 0x2_0000_0031, [7, 8, 9, 255]);
    let mut parcel = transaction(&[Layer {
        handle,
        layer_id: id,
        x: 0.0,
        y: 0.0,
        z: 0,
        buffer: &a,
    }]);
    r.transact(&mut parcel).expect("transaction decodes");
    let frame = r.capture().expect("a capture");
    assert_eq!((frame.width, frame.height, frame.stride), (8, 4, 12));
    assert_eq!(frame.rgba.len(), 12 * 4 * 4, "the buffer's own stride is kept");
    assert_eq!(frame.layers, 1);
    assert!(!frame.composite, "a one-layer capture is a pass-through");
    // The row padding is part of the pass-through, not the app's pixels.
    assert_eq!(pixel(&frame, 0, 0), [7, 8, 9, 255]);
}

#[test]
fn more_visible_layers_than_the_cap_are_refused() {
    let mut r = rig();
    let mut buffers = Vec::new();
    let mut layers = Vec::new();
    for i in 0..=MAX_COMPOSITE_LAYERS {
        buffers.push(Buf::solid(2, 2, 2, 0x2_0000_0100 + i as u64, RED));
    }
    for (i, buffer) in buffers.iter().enumerate() {
        layers.push(Layer {
            // Synthetic handles: the decoder records what the transaction names,
            // so a guest can offer more layers than the handle pool has.
            handle: BinderId(0x9000 + i as u64),
            layer_id: 1000 + i as i32,
            x: 0.0,
            y: 0.0,
            z: i as i32,
            buffer,
        });
    }
    let mut parcel = transaction(&layers);
    r.transact(&mut parcel).expect("transaction decodes");
    let err = r.capture().expect_err("17 layers must be refused");
    assert_eq!(
        err,
        CompositeError::TooManyLayers {
            layers: MAX_COMPOSITE_LAYERS + 1,
            max: MAX_COMPOSITE_LAYERS
        }
    );
    assert_eq!(err.code(), "COMPOSITE_TOO_MANY_LAYERS");
    // The cap is a refusal, not a partial frame: `capture_latest` says no frame.
    assert!(r.state.lock().expect("state").capture_latest().is_none());
}

#[test]
fn layers_far_apart_are_clipped_to_the_display_not_a_huge_canvas() {
    // W54: the composite is the display viewport (SurfaceFlinger clipping), so a
    // layer two displays away contributes nothing. Before W54 the canvas was the
    // raw union of layer frames and this transaction was refused with
    // CanvasTooLarge; the memory bound is now the display itself, which is
    // tighter, and the visible layer still composites at its own pixels.
    let mut r = rig();
    let a = Buf::solid(8, 8, 8, 0x2_0000_0201, RED);
    let b = Buf::solid(8, 8, 8, 0x2_0000_0202, BLUE);
    let mut parcel = transaction(&[
        Layer {
            handle: BinderId(0x9100),
            layer_id: 1,
            x: 0.0,
            y: 0.0,
            z: 0,
            buffer: &a,
        },
        Layer {
            handle: BinderId(0x9101),
            layer_id: 2,
            // Two windows this far apart cannot be one display: the far layer
            // is off the display viewport.
            x: 8185.0,
            y: 0.0,
            z: 1,
            buffer: &b,
        },
    ]);
    r.transact(&mut parcel).expect("transaction decodes");
    let frame = r.capture().expect("the on-display layer still composites");
    assert_eq!(
        (frame.width, frame.height),
        (8, 8),
        "the canvas is the on-display layer, not the union with an off-display one"
    );
    assert_eq!(frame.layers, 2, "both layers are considered, one draws nothing");
    assert_eq!(&frame.rgba[..4], &RED);
}

#[test]
fn an_empty_registry_has_no_frame() {
    let r = rig();
    assert_eq!(r.capture().err(), Some(CompositeError::NoFrame));
    assert!(r.state.lock().expect("state").capture_latest().is_none());
}
