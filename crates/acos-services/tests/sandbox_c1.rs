//! S7 sandbox certification: the **C1 compositor surface** against a hostile
//! guest.
//!
//! Every input below is a `setTransactionState` parcel a guest can hand the
//! host: declared sizes, fd counts, ids and descriptors. The property under
//! test is the same for all of them — **a guest-declared number never becomes a
//! host allocation, a host read loop, a host read of a named file, or a panic**;
//! a rejected parcel is a structured `BadRequest`.
//!
//! The tests are unit-level (no ART, no guest process): the same `ServiceHost`
//! and the same decoder a live BLAST transaction reaches. `tools/guard/guarded.sh`
//! still wraps the run; the memory-bomb cases use a *declared* size that the
//! old code would have allocated, but the assertion is made at decode time, so
//! the post-fix path allocates nothing.
//!
//! Teeth: each row was run against the pre-S7 decoder and failed (see
//! `results/art-runtime/S7.md` §3, "before/after").

use std::os::fd::{AsFd, OwnedFd};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use acos_parcel::{BinderId, Parcel};
use acos_services::compositor_client::{self, DisplayDescriptor};
use acos_services::surface_client::{self, SurfaceState};
use acos_services::ServiceHost;

const ISURFACE_COMPOSER: &str = "android.gui.ISurfaceComposer";
const ISURFACE_COMPOSER_LEGACY: &str = "android.ui.ISurfaceComposer";
const ISURFACE_COMPOSER_CLIENT: &str = "android.gui.ISurfaceComposerClient";

const CREATE_CONNECTION: u32 = 3;
const CREATE_SURFACE: u32 = 1;
const SET_TRANSACTION_STATE: u32 = 8;

/// One installed compositor, ready to receive transactions.
struct Rig {
    host: ServiceHost,
    state: Arc<Mutex<SurfaceState>>,
    sf: BinderId,
    handle: BinderId,
    layer_id: i32,
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

    let mut data = Parcel::new();
    data.write_interface_token(ISURFACE_COMPOSER_CLIENT)
        .expect("token");
    data.write_string16("hostile").expect("name");
    data.write_int32(0);
    data.write_strong_binder_opt(None);
    data.write_int32(1); // non-null LayerMetadata
    data.write_int32(0); // empty
    data.set_data_position(0);
    let mut reply = Parcel::new();
    host.transact(client, CREATE_SURFACE, &mut data, &mut reply, 0)
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

    Rig {
        host,
        state,
        sf,
        handle,
        layer_id,
    }
}

impl Rig {
    fn transact(&mut self, parcel: &mut Parcel) -> Result<(), acos_services::ServiceError> {
        let mut reply = Parcel::new();
        self.host
            .transact(self.sf, SET_TRANSACTION_STATE, parcel, &mut reply, 0)
    }

    fn generation(&self) -> u64 {
        self.state.lock().expect("state").latest_generation()
    }
}

/// A guest-shaped `GraphicBuffer` body. `header_len` and `fd_count` are the
/// **guest-declared** words, independent of how many bytes/fds the test writes.
struct Buf {
    raw: Option<OwnedFd>,
    width: u32,
    height: u32,
    stride: u32,
    header_len: i32,
    fd_count: i32,
    fds_written: i32,
    id: u64,
}

impl Buf {
    fn valid(fd: OwnedFd, width: u32, height: u32, stride: u32) -> Self {
        Self {
            raw: Some(fd),
            width,
            height,
            stride,
            header_len: 88,
            fd_count: 1,
            fds_written: 1,
            id: 0x2_0000_0000,
        }
    }
}

/// Transaction knobs a hostile guest can set; `Default` is a plain one-layer
/// buffer transaction.
#[derive(Default)]
struct Knobs {
    listener_count_word: Option<i32>,
    callback_count_word: Option<i32>,
    composer_count_word: Option<u32>,
    uncache_count_word: Option<u32>,
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
    data.write_bool(b.raw.is_some());
    if let Some(fd) = &b.raw {
        data.write_int32(b.header_len);
        data.write_int32(b.fd_count);
        data.write_int32(i32::from_le_bytes(*b"GB01"));
        data.write_int32(b.width as i32);
        data.write_int32(b.height as i32);
        data.write_int32(b.stride as i32);
        data.write_int32(1); // format
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
        for _ in 0..b.fds_written.max(0) {
            data.write_file_descriptor(fd.as_fd()).expect("fd");
        }
    }
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

fn write_layer(data: &mut Parcel, r: &Rig, buffer: Option<&Buf>, k: &Knobs) {
    data.write_strong_binder(r.handle);
    data.write_int32(r.layer_id);
    data.write_uint64(surface_client::what::BUFFER | surface_client::what::LAYER);
    data.write_float(0.0);
    data.write_float(0.0);
    data.write_int32(0);
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

    let listeners = k.listener_count_word.unwrap_or(0);
    data.write_int32(listeners);
    if let Some(callbacks) = k.callback_count_word {
        data.write_strong_binder(BinderId(0x100)); // listener
        data.write_int32(callbacks); // guest-declared CallbackId[] count
                                     // No elements follow: the count is the attack.
    }

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

    data.write_bool(buffer.is_some());
    if let Some(buffer) = buffer {
        write_buffer(data, buffer);
    }

    data.write_int32(1); // TrustedPresentationThresholds marker
    data.write_int32(16); // size counts itself: 12-byte payload
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

fn transaction(r: &Rig, buffer: Option<&Buf>, k: &Knobs) -> Parcel {
    let mut data = Parcel::new();
    data.write_interface_token(ISURFACE_COMPOSER_LEGACY)
        .expect("token");
    write_light_blob(&mut data, &[0]); // FrameTimelineInfo
    let composers = k.composer_count_word.unwrap_or(1);
    data.write_uint32(composers);
    if k.composer_count_word.is_none() {
        write_layer(&mut data, r, buffer, k);
    }
    data.write_uint32(0); // displays
    data.write_uint32(0); // flags
    data.write_strong_binder_opt(None); // applyToken
    data.write_int32(0); // focusRequests
    data.write_int32(0); // windowInfosReportedListeners
    data.write_int64(0); // desiredPresentTime
    data.write_bool(true); // isAutoTimestamp
    data.write_uint32(k.uncache_count_word.unwrap_or(0));
    data.write_bool(false); // no top-level listener callbacks
    data.write_int32(0);
    data.write_uint64(7); // transactionId
    data.write_uint32(0); // mergedTransactionIds
    data.set_data_position(0);
    data
}

static NEXT: AtomicU64 = AtomicU64::new(0);

fn scratch_dir() -> std::path::PathBuf {
    let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.cache/s7-tests");
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

/// An **anonymous** regular file of `len` bytes: the shape a `memfd`/gralloc
/// buffer has (`/proc/self/fd/N` -> "/… (deleted)", `S_ISREG`).
fn anon_file(len: u64) -> OwnedFd {
    let path = scratch_dir().join(format!(
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

/// A **named** (still-linked) regular file: what a hostile fd must not smuggle
/// past the type check.
fn linked_file(len: u64) -> (OwnedFd, std::path::PathBuf) {
    let path = scratch_dir().join(format!(
        "linked-{}-{}.bin",
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
    (file.into(), path)
}

fn detail(err: &acos_services::ServiceError) -> String {
    format!("{err:?}")
}

#[test]
fn hostile_buffer_geometry_is_rejected_before_it_is_an_allocation() {
    let cases: &[(&str, u32, u32, u32)] = &[
        ("all-words-max", u32::MAX, u32::MAX, u32::MAX),
        ("height-max", 1080, u32::MAX, 1088),
        (
            "overflow-row-times-height",
            u32::MAX / 4,
            u32::MAX / 4,
            u32::MAX / 4,
        ),
        ("dimension-over-ceiling", 8192 + 1, 16, 16),
        ("stride-over-ceiling", 1080, 100, 8192 + 1),
        ("stride-short-of-width", 1080, 100, 512),
        ("byte-ceiling", 5000, 5000, 5000),
    ];
    for (name, width, height, stride) in cases {
        let mut r = rig();
        let fd = anon_file(64);
        let buf = Buf::valid(fd, *width, *height, *stride);
        let mut parcel = transaction(&r, Some(&buf), &Knobs::default());
        let err = r
            .transact(&mut parcel)
            .expect_err("hostile geometry must be refused");
        let text = detail(&err);
        assert!(
            text.contains("geometry") || text.contains("bounds"),
            "{name}: expected a geometry bounds error, got {text}"
        );
        assert_eq!(
            r.generation(),
            0,
            "{name}: a refused transaction must not store a frame"
        );
        assert!(
            r.state.lock().expect("state").capture_latest().is_none(),
            "{name}: no frame may be capturable after a refusal"
        );
    }
}

#[test]
fn a_valid_anonymous_frame_is_still_accepted_and_capturable() {
    // Control: the bounds must not reject a real BLAST frame shape
    // (1080x2209, stride 1088 = 9.6 MiB, one anonymous memfd).
    let mut r = rig();
    let (w, h, s) = (1080u32, 2209u32, 1088u32);
    let fd = anon_file(u64::from(s) * 4 * u64::from(h));
    let buf = Buf::valid(fd, w, h, s);
    let mut parcel = transaction(&r, Some(&buf), &Knobs::default());
    r.transact(&mut parcel).expect("a real frame decodes");
    assert_eq!(r.generation(), 1);
    let capture = r
        .state
        .lock()
        .expect("state")
        .capture_latest()
        .expect("frame");
    assert_eq!((capture.width, capture.height, capture.stride), (w, h, s));
    assert_eq!(capture.rgba.len(), 1088 * 2209 * 4);
}

#[test]
fn an_oversized_graphic_buffer_header_is_refused_not_allocated() {
    let mut r = rig();
    let fd = anon_file(64);
    let mut buf = Buf::valid(fd, 64, 48, 64);
    buf.header_len = 16 * 1024 * 1024; // old path: vec![0i32; 4M] = 16 MiB
    let mut parcel = transaction(&r, Some(&buf), &Knobs::default());
    let err = r.transact(&mut parcel).expect_err("header bomb refused");
    assert!(
        detail(&err).contains("header len"),
        "expected an explicit header bound, got {}",
        detail(&err)
    );
    assert_eq!(r.generation(), 0);
}

#[test]
fn a_graphic_buffer_fd_count_bomb_is_refused() {
    let mut r = rig();
    let fd = anon_file(64 * 48 * 4);
    let mut buf = Buf::valid(fd, 64, 48, 64);
    buf.fd_count = 1000; // guest-declared; only two fds are actually in the parcel
    buf.fds_written = 2;
    let mut parcel = transaction(&r, Some(&buf), &Knobs::default());
    let err = r.transact(&mut parcel).expect_err("fd bomb refused");
    assert!(
        detail(&err).contains("fd count"),
        "expected an fd-count bound, got {}",
        detail(&err)
    );
    assert_eq!(r.generation(), 0);
}

#[test]
fn a_linked_file_fd_is_not_a_frame() {
    // A descriptor to a named file (the app's own APK, a framework file) would
    // make the compositor read that file's bytes as pixels. It must be refused.
    let mut r = rig();
    let (fd, path) = linked_file(64 * 48 * 4);
    let buf = Buf::valid(fd, 64, 48, 64);
    let mut parcel = transaction(&r, Some(&buf), &Knobs::default());
    let err = r
        .transact(&mut parcel)
        .expect_err("a linked path is not a memfd");
    assert!(
        detail(&err).contains("live path") || detail(&err).contains("anonymous"),
        "expected a type error naming the live path, got {}",
        detail(&err)
    );
    assert_eq!(r.generation(), 0);
    let _ = std::fs::remove_file(path);
}

#[test]
fn a_socket_fd_is_not_a_frame() {
    let mut r = rig();
    let (a, _b) = std::os::unix::net::UnixStream::pair().expect("socketpair");
    let fd: OwnedFd = a.into();
    let buf = Buf::valid(fd, 64, 48, 64);
    let mut parcel = transaction(&r, Some(&buf), &Knobs::default());
    let err = r
        .transact(&mut parcel)
        .expect_err("a socket is not a frame");
    assert!(
        detail(&err).contains("not a regular file") || detail(&err).contains("live path"),
        "expected a non-file type error, got {}",
        detail(&err)
    );
    assert_eq!(r.generation(), 0);
}

#[test]
fn a_short_anonymous_fd_never_becomes_a_partial_frame() {
    // The fd is the right *type* but smaller than the declared geometry: the
    // transaction decodes (the type is honest) and the capture refuses a short
    // read, so no partial image is ever reported as the app's pixels.
    let mut r = rig();
    let fd = anon_file(1024);
    let buf = Buf::valid(fd, 1080, 2209, 1088);
    let mut parcel = transaction(&r, Some(&buf), &Knobs::default());
    r.transact(&mut parcel).expect("type is valid");
    assert_eq!(r.generation(), 1, "the store counter advanced");
    assert!(
        r.state.lock().expect("state").capture_latest().is_none(),
        "a short memfd must not yield a partial frame"
    );
}

#[test]
fn a_callback_id_count_bomb_is_refused_before_with_capacity() {
    let mut r = rig();
    let fd = anon_file(64 * 48 * 4);
    let buf = Buf::valid(fd, 64, 48, 64);
    let k = Knobs {
        listener_count_word: Some(1),
        callback_count_word: Some(1_000_000), // old path: 16 MiB with_capacity, then EOF
        ..Knobs::default()
    };
    let mut parcel = transaction(&r, Some(&buf), &k);
    let err = r.transact(&mut parcel).expect_err("callback bomb refused");
    assert!(
        detail(&err).contains("callback id count"),
        "expected an explicit CallbackId[] bound, got {}",
        detail(&err)
    );
    assert_eq!(r.generation(), 0);
}

#[test]
fn composer_display_uncache_and_listener_counts_are_bounded() {
    let k = Knobs {
        composer_count_word: Some(1_000_000),
        ..Knobs::default()
    };
    let mut r = rig();
    let mut parcel = transaction(&r, None, &k);
    let err = r.transact(&mut parcel).expect_err("composer bomb refused");
    assert!(detail(&err).contains("composer count"), "{}", detail(&err));

    let mut r = rig();
    let k = Knobs {
        uncache_count_word: Some(1_000_000),
        ..Knobs::default()
    };
    let mut parcel = transaction(&r, None, &k);
    let err = r.transact(&mut parcel).expect_err("uncache bomb refused");
    assert!(detail(&err).contains("uncacheBuffers"), "{}", detail(&err));

    let mut r = rig();
    let k = Knobs {
        listener_count_word: Some(1000),
        ..Knobs::default()
    };
    let mut parcel = transaction(&r, None, &k);
    let err = r.transact(&mut parcel).expect_err("listener bomb refused");
    assert!(detail(&err).contains("listener count"), "{}", detail(&err));
}
