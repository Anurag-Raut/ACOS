//! Regression test for W3's leash rule: the `SurfaceControl` inside
//! `InsetsSourceControl` and `relayout`'s `outSurfaceControl` must carry
//! **non-null** strong binders in the `client` and layer-`handle` slots.
//!
//! Why (results/art-runtime/W3.md §1-§2): the image's
//! `SurfaceControl::readFromParcel` (libgui) reads both with
//!
//! ```text
//! SAFE_PARCEL(parcel.readStrongBinder, &client);
//! SAFE_PARCEL(parcel.readStrongBinder, &handle);
//! ```
//!
//! and `Parcel::readStrongBinder(sp<IBinder>*)` is **not** the tolerant
//! variant: after `readNullableStrongBinder` succeeds with a null binder it
//! logs `Expecting binder but got null!` and returns `UNEXPECTED_NULL`
//! (`-2147483640`). `SAFE_PARCEL` then returns from `readFromParcel` *in the
//! middle of the leash*, leaving the enclosing `InsetsSourceControl$Array` walk
//! 88 bytes out of step: the next leash's client read lands 88 bytes later, at
//! the byte offset the guest logged as
//! `Attempt to read object from Parcel ... at offset 1440 that is not in the
//! object list`, and the reply finally desynchronises into the trailing
//! `float[]` (`bad array lengths`).
//!
//! `Parcel::readObject(bool)`'s null fast path (`binder == 0 && cookie == 0`,
//! no object-table check) is real, but it only makes
//! `readNullableStrongBinder` return null *successfully*; the strict
//! `readStrongBinder` converts that into the error above.
//!
//! So this file pins two things with hand-transcribed framework readers (no
//! `acos-wm` codec in the decode path):
//!
//! 1. the reply the real `IWindowSession` node produces writes **non-null**
//!    client/handle binders and still consumes exactly the reply
//!    (`data_avail() == 0`);
//! 2. the rule has teeth: the previous null-binder shape fails the same reader
//!    at the client slot, with the framework's own `UNEXPECTED_NULL` semantics.

use acos_parcel::{BinderId, Parcel};
use acos_services::exception::NO_EXCEPTION;
use acos_services::ServiceHost;
use acos_wm::codec::{placeholder_binder, LayoutParams, ParcelCodec};

const IWINDOW_MANAGER: &str = "android.view.IWindowManager";
const IWINDOW_SESSION: &str = "android.view.IWindowSession";
const OPEN_SESSION: u32 = 4;
const ADD_TO_DISPLAY_AS_USER: u32 = 2;

/// `status_t UNEXPECTED_NULL` (`utils/Errors.h`): `UNKNOWN_ERROR + 7`, i.e.
/// `-2147483640`, the value libgui logs when the leash's `client` is null.
const UNEXPECTED_NULL: i32 = -2147483640;

// ---------------------------------------------------------------------------
// strict framework-order readers
// ---------------------------------------------------------------------------

/// The outcome of `Parcel::readStrongBinder(sp<IBinder>*)` for one object.
#[derive(Debug, PartialEq, Eq)]
enum StrictBinder {
    /// A readable, non-null binder object (what the client/handle slots need).
    Some(BinderId),
    /// Readable null binder: `readStrongBinder` logs `Expecting binder but got
    /// null!` and returns [`UNEXPECTED_NULL`], aborting `readFromParcel`.
    Null(usize, i32),
}

impl StrictBinder {
    fn is_null(&self) -> bool {
        matches!(self, StrictBinder::Null(..))
    }
}

fn note_pos(p: &Parcel) -> usize {
    p.data_position()
}

/// `Parcel::readStrongBinder(sp<IBinder>*)` as the image implements it:
/// `readNullableStrongBinder` + "a null value is `UNEXPECTED_NULL`".
fn fw_read_strong_binder(p: &mut Parcel) -> StrictBinder {
    let at = note_pos(p);
    match p.read_strong_binder().expect("strong binder") {
        Some(id) => StrictBinder::Some(id),
        None => StrictBinder::Null(at, UNEXPECTED_NULL),
    }
}

/// `Rect.readFromParcel`: four ints, no presence marker.
fn fw_rect(p: &mut Parcel) -> [i32; 4] {
    [
        p.read_int32().expect("rect.left"),
        p.read_int32().expect("rect.top"),
        p.read_int32().expect("rect.right"),
        p.read_int32().expect("rect.bottom"),
    ]
}

/// `Parcel.readTypedObject`: presence marker then the bare body.
fn fw_typed<T>(p: &mut Parcel, body: impl FnOnce(&mut Parcel) -> T) -> Option<T> {
    if p.read_int32().expect("typed presence") == 0 {
        None
    } else {
        Some(body(p))
    }
}

/// The two binders (and the rest of the native body) of a
/// `android.view.SurfaceControl`.
struct FwSurface {
    name: String,
    width: i32,
    height: i32,
    native: Option<FwSurfaceNative>,
}

struct FwSurfaceNative {
    client: StrictBinder,
    handle: StrictBinder,
    layer_id: i32,
    layer_name: String,
    transform_hint: i32,
    width: i32,
    height: i32,
    format: i32,
}

/// `SurfaceControl.readFromParcel` (image `0x1d26ac`) + the native body
/// (`libgui` `SurfaceControl::readFromParcel`).
fn fw_surface_control(p: &mut Parcel) -> FwSurface {
    let name = p.read_string8().expect("surface name").unwrap_or_default();
    let width = p.read_int32().expect("surface width");
    let height = p.read_int32().expect("surface height");
    let present = p.read_int32().expect("surface hasNative");
    let native = if present != 0 {
        let client = fw_read_strong_binder(p);
        let handle = fw_read_strong_binder(p);
        let layer_id = p.read_int32().expect("surface layerId");
        let layer_name = p
            .read_string16()
            .expect("surface layerName")
            .unwrap_or_default();
        let transform_hint = p.read_int32().expect("surface transformHint");
        let w = p.read_int32().expect("surface native width");
        let h = p.read_int32().expect("surface native height");
        let format = p.read_int32().expect("surface format");
        Some(FwSurfaceNative {
            client,
            handle,
            layer_id,
            layer_name,
            transform_hint,
            width: w,
            height: h,
            format,
        })
    } else {
        None
    };
    FwSurface {
        name,
        width,
        height,
        native,
    }
}

/// `InsetsSourceControl(Parcel)` (image `0x1b96f8`): id, type, leash,
/// initiallyVisible, Point, Insets, skipAnimationOnce.
fn fw_insets_source_control(p: &mut Parcel) -> (i32, i32, Option<FwSurface>) {
    let id = p.read_int32().expect("control.id");
    let type_ = p.read_int32().expect("control.type");
    let leash = fw_typed(p, fw_surface_control);
    let _initially_visible = p.read_bool().expect("control.initiallyVisible");
    let _position = fw_typed(p, |p| {
        (
            p.read_int32().expect("point.x"),
            p.read_int32().expect("point.y"),
        )
    });
    let _insets = fw_typed(p, fw_rect);
    let _skip = p.read_bool().expect("control.skipAnimationOnce");
    (id, type_, leash)
}

/// `InsetsSourceControl$Array.readFromParcel`: count + one typed object each.
fn fw_insets_control_array(p: &mut Parcel) -> Vec<Option<(i32, i32, Option<FwSurface>)>> {
    let n = p.read_int32().expect("controls count");
    (0..n.max(0))
        .map(|_| fw_typed(p, fw_insets_source_control))
        .collect()
}

// ---------------------------------------------------------------------------
// requests (the image's write order, as in W1/W2's tests)
// ---------------------------------------------------------------------------

fn app_params() -> LayoutParams {
    LayoutParams::application("dev.acos.fixtures.helloviews", "MainActivity")
}

fn open_session_request() -> Parcel {
    let mut data = Parcel::new();
    data.write_interface_token(IWINDOW_MANAGER).expect("token");
    data.write_strong_binder_opt(None);
    data.set_data_position(0);
    data
}

fn add_request(window: BinderId) -> Parcel {
    let mut data = Parcel::new();
    data.write_interface_token(IWINDOW_SESSION).expect("token");
    data.write_strong_binder_opt(Some(window));
    data.write_int32(1);
    app_params()
        .write_to_parcel(&mut data)
        .expect("LayoutParams");
    data.write_int32(0); // viewVisibility
    data.write_int32(-1); // layerStackId
    data.write_int32(0); // userId
    data.write_int32(0); // requestedVisibleTypes
    data.write_int32(1); // sizeCompatScale.length
    data.set_data_position(0);
    data
}

fn open_session(host: &mut ServiceHost) -> BinderId {
    let window = host
        .binder_for_descriptor(IWINDOW_MANAGER)
        .expect("window service");
    let mut reply = Parcel::new();
    let mut data = open_session_request();
    host.transact(window, OPEN_SESSION, &mut data, &mut reply, 0)
        .expect("openSession");
    reply.set_data_position(0);
    assert_eq!(
        reply.read_int32().expect("exception header"),
        NO_EXCEPTION,
        "the reply must start with writeNoException"
    );
    reply
        .read_strong_binder()
        .expect("session")
        .expect("session")
}

// ---------------------------------------------------------------------------
// the tests
// ---------------------------------------------------------------------------

/// Skips the `InsetsState` body of the add reply with hand-written readers
/// (identical field order to `window_session_framework_order.rs`, trimmed to
/// what is needed to reach the controls).
fn skip_insets_state(p: &mut Parcel) {
    let _display_frame = fw_rect(p);
    let cutout = p.read_int32().expect("cutout variant");
    if cutout > 0 {
        let _safe = fw_typed(p, fw_rect);
        let n = p.read_int32().expect("cutout bounds");
        assert_eq!(n, 4, "the image reads a fixed Rect[4]");
        for _ in 0..n {
            let _ = fw_typed(p, fw_rect);
        }
        let _waterfall = fw_typed(p, fw_rect);
        let _ints = [
            p.read_int32().expect("displayWidth"),
            p.read_int32().expect("displayHeight"),
            p.read_int32().expect("physicalWidth"),
            p.read_int32().expect("physicalHeight"),
        ];
        let _density = p.read_int32().expect("density bits");
        let _spec = p.read_string16().expect("cutoutSpec");
        let _rotation = p.read_int32().expect("rotation");
        let _scale = p.read_int32().expect("scale bits");
        let _ratio = p.read_int32().expect("ratio bits");
    }
    let _rounded = fw_typed(p, |p| {
        assert_eq!(p.read_int32().expect("rounded marker"), 1);
        let n = p.read_int32().expect("corner count");
        for _ in 0..n {
            let _presence = p.read_int32().expect("corner presence");
            let _values = fw_rect(p);
        }
    });
    let _rounded_frame = fw_rect(p);
    let _privacy = fw_typed(p, |p| {
        let n = p.read_int32().expect("privacy count");
        for _ in 0..n {
            let _presence = p.read_int32().expect("privacy presence");
            let _rect = fw_rect(p);
        }
        let _rotation = p.read_int32().expect("privacy rotation");
    });
    let _shape = fw_typed(p, |p| {
        let _spec = p.read_string8().expect("displayShape spec");
        let _w = p.read_int32().expect("shape width");
        let _h = p.read_int32().expect("shape height");
        let _ratio = p.read_int32().expect("shape ratio bits");
        let _rotation = p.read_int32().expect("shape rotation");
        let _x = p.read_int32().expect("shape offsetX");
        let _y = p.read_int32().expect("shape offsetY");
        let _scale = p.read_int32().expect("shape scale bits");
    });
    let sources = p.read_int32().expect("sources count");
    for _ in 0..sources.max(0) {
        let _presence = p.read_int32().expect("source presence");
        let _id = p.read_int32().expect("source.id");
        let _type = p.read_int32().expect("source.type");
        let _frame = fw_rect(p);
        let _visible_frame = fw_typed(p, fw_rect);
        let _visible = p.read_int32().expect("source.visible");
        let _flags = p.read_int32().expect("source.flags");
        let _rounded = p.read_int32().expect("source rounded");
    }
}

#[test]
fn the_leash_client_and_handle_are_non_null_and_the_reply_ends_clean() {
    let mut host = ServiceHost::standard();
    let session = open_session(&mut host);
    let window = BinderId(0x6101);

    let mut data = add_request(window);
    let mut reply = Parcel::new();
    host.transact(session, ADD_TO_DISPLAY_AS_USER, &mut data, &mut reply, 0)
        .expect("addToDisplayAsUser");
    reply.set_data_position(0);

    // `InsetsSourceControl$Stub.onTransact` (code 2): writeNoException,
    // writeInt(result), writeTypedObject(InputChannel), writeTypedObject(
    // InsetsState), writeTypedObject(InsetsSourceControl.Array),
    // writeTypedObject(Rect attachedFrame), writeFloatArray(sizeCompatScale).
    assert_eq!(reply.read_int32().expect("exception header"), NO_EXCEPTION);
    let _result = reply.read_int32().expect("result");
    // InputChannel (image order, W2 §1.1): presence, isInitialized, token,
    // String16 name, fd object.
    assert_eq!(reply.read_int32().expect("channel presence"), 1);
    assert_eq!(reply.read_int32().expect("channel isInitialized"), 1);
    let token = fw_read_strong_binder(&mut reply);
    assert!(!token.is_null(), "the channel token is a real binder");
    let _name = reply.read_string16().expect("channel name");
    // The fd slot is a `BINDER_TYPE_FD` flat object: `readUniqueFileDescriptor`
    // consumes 24 bytes of object data, no stability record.
    for _ in 0..6 {
        let _ = reply.read_int32().expect("InputChannel fd object");
    }

    assert_eq!(reply.read_int32().expect("InsetsState presence"), 1);
    skip_insets_state(&mut reply);

    assert_eq!(reply.read_int32().expect("controls presence"), 1);
    let controls = fw_insets_control_array(&mut reply);
    assert!(!controls.is_empty(), "the policy serves leash controls");

    let mut seen = 0;
    for control in controls.into_iter().flatten() {
        let (id, type_, leash) = control;
        let leash = leash.expect("every control carries a leash");
        let native = leash.native.expect("the leash has a native body");
        let expected = placeholder_binder(native.layer_id as u32 as u64);
        match &native.client {
            StrictBinder::Some(client) => {
                assert_eq!(
                    *client, expected,
                    "control {id:#x} (type {type_}): the client is the documented placeholder \
                     binder (live, routable, no ISurfaceComposerClient interface)"
                );
            }
            StrictBinder::Null(at, status) => panic!(
                "control {id:#x} (type {type_}): leash client at reply offset {at} is a null \
                 binder; Parcel::readStrongBinder returns {status} (UNEXPECTED_NULL) and aborts \
                 SurfaceControl::readFromParcel, which desynchronises this array (W3 §2)"
            ),
        }
        match &native.handle {
            StrictBinder::Some(handle) => assert_eq!(
                *handle, expected,
                "control {id:#x}: the layer handle is the same placeholder binder"
            ),
            StrictBinder::Null(at, status) => panic!(
                "control {id:#x}: leash layer handle at reply offset {at} is a null binder \
                 ({status})"
            ),
        }
        assert_eq!(native.layer_name, leash.name);
        assert_eq!((native.width, native.height), (leash.width, leash.height));
        assert_eq!((native.width, native.height), (1080, 2400));
        seen += 1;
    }
    assert!(seen >= 1, "at least one leash was decoded");

    assert_eq!(reply.read_int32().expect("attachedFrame presence"), 1);
    let _attached = fw_rect(&mut reply);
    let scale_len = reply.read_int32().expect("float[] count");
    assert_eq!(scale_len, 1, "the reply carries the caller's array length");
    let _scale = reply.read_int32().expect("float[] value");
    assert_eq!(
        reply.data_avail(),
        0,
        "a strict framework-order walk must consume the whole reply"
    );
}

/// The same rule at the codec level: every `SurfaceControlInfo` with a native
/// body (the leash and `relayout`'s `outSurfaceControl`) writes the placeholder
/// in both binder slots, and the body still ends exactly at its end.
#[test]
fn every_surface_control_body_writes_the_placeholder_binders() {
    use acos_wm::codec::SurfaceControlInfo;

    let sc = SurfaceControlInfo {
        name: "Surface(name=dev.acos.fixtures.helloviews/MainActivity)/@0x1002".into(),
        width: 1080,
        height: 2209,
        handle: Some(0x1002),
        ..Default::default()
    };
    let mut p = Parcel::new();
    sc.write_to_parcel(&mut p).expect("SurfaceControlInfo");
    let len = p.data_size();
    p.set_data_position(0);
    let surface = fw_surface_control(&mut p);
    let native = surface.native.expect("native body");
    let expected = placeholder_binder(0x1002);
    assert_eq!(native.client, StrictBinder::Some(expected));
    assert_eq!(native.handle, StrictBinder::Some(expected));
    assert_ne!(expected.0, 0);
    assert_eq!((native.width, native.height), (sc.width, sc.height));
    assert_eq!(native.transform_hint, 0);
    assert_eq!(native.format, 1, "PIXEL_FORMAT_RGBA_8888");
    assert_ne!(native.layer_id, 0, "the synthetic ACOS layer id");
    assert_eq!(
        p.data_position(),
        len,
        "the surface body ends exactly at the parcel end"
    );
}

/// The rule's teeth: the same body with **null** binders (the shape W2 shipped)
/// makes the strict reader abort at the client slot exactly as the image did.
#[test]
fn a_null_client_binder_is_unexpected_null_and_aborts_the_parse() {
    let mut p = Parcel::new();
    p.write_int32(0x1_0008); // control.id
    p.write_int32(8); // control.type = IME
    p.write_int32(1); // typed leash presence
    p.write_string8("Surface(name=InputMethod animation-leash of insets_animation)")
        .expect("name8");
    p.write_int32(1080);
    p.write_int32(2400);
    p.write_int32(1); // hasNative
    p.write_strong_binder_opt(None); // client  <- W2's shape
    p.write_strong_binder_opt(None); // handle
    p.write_int32(0x1003); // layerId
    p.write_string16("Surface(name=InputMethod animation-leash of insets_animation)")
        .expect("layerName");
    p.write_int32(0); // transformHint
    p.write_int32(1080);
    p.write_int32(2400);
    p.write_int32(1); // format RGBA_8888
    p.set_data_position(0);

    let id = p.read_int32().expect("id");
    let _type = p.read_int32().expect("type");
    assert_eq!(p.read_int32().expect("leash presence"), 1);
    let _name = p.read_string8().expect("name8");
    let _w = p.read_int32();
    let _h = p.read_int32();
    assert_eq!(p.read_int32().expect("hasNative"), 1);

    let at = p.data_position();
    match fw_read_strong_binder(&mut p) {
        StrictBinder::Null(pos, status) => {
            assert_eq!(pos, at, "the abort happens at the client slot");
            assert_eq!(
                status, UNEXPECTED_NULL,
                "Parcel::readStrongBinder returns UNEXPECTED_NULL for a readable null binder"
            );
        }
        StrictBinder::Some(_) => {
            panic!("control {id:#x}: a null client must not read as a binder (this is the W2 bug)")
        }
    }
}
