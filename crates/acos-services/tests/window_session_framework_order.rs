//! Framework-order **hand transcription** of the `IWindowSession` replies.
//!
//! Unlike `window_session_api34.rs` (which decodes with the `acos-wm` codecs
//! that also produced the bytes), every reader here is transcribed by hand from
//! the **API-34 image** and consumes the reply with `acs-parcel` primitives
//! only. If a writer and its own reader disagree with the framework, this test
//! fails even though the codec round-trips.
//!
//! Sources, all from the image (`.cache/boot/android-34`, disassembled with
//! `cargo run -p acos-dex --example dexdump`):
//!
//! ```text
//! classes4.dex  Landroid/view/IWindowSession$Stub;->onTransact        (what the
//!               server writes: writeNoException, writeInt, writeTypedObject x4,
//!               writeFloatArray)
//! classes4.dex  Landroid/view/IWindowSession$Stub$Proxy;->addToDisplayAsUser
//!               (what the client reads: readInt presence + the parcelable
//!               body for each out parameter)
//! classes4.dex  Landroid/view/InputChannel;->readFromParcel -> nativeReadFromParcel
//! libandroid_runtime.so:0x144f40  android_view_InputChannel_nativeReadFromParcel
//!               (int32 isInitialized, then InputChannel::readFromParcel)
//! libinput.so:0x78dc0            InputChannel::readFromParcel
//!               (readStrongBinder token, readUtf8FromUtf16 name,
//!                readUniqueFileDescriptor fd) — and 0x78d40, the writer, in the
//!               same order
//! classes4.dex  InsetsState.readFromParcel, DisplayCutout$ParcelableWrapper.
//!               readCutoutFromParcel, InsetsSource(Parcel),
//!               InsetsSourceControl(Parcel), InsetsSourceControl$Array,
//!               RoundedCorners/RoundedCorner, PrivacyIndicatorBounds,
//!               DisplayShape, ClientWindowFrames.readFromParcel,
//!               SurfaceControl.readFromParcel
//! classes3.dex  MergedConfiguration.readFromParcel (3 x Configuration),
//! classes1.dex  Configuration.readFromParcel / WindowConfiguration.readFromParcel
//! ```

use acos_parcel::{BinderId, Parcel};
use acos_services::exception::NO_EXCEPTION;
use acos_services::ServiceHost;
use acos_wm::codec::{LayoutParams, ParcelCodec};
use acos_wm::wms::ADD_OKAY;

const IWINDOW_MANAGER: &str = "android.view.IWindowManager";
const IWINDOW_SESSION: &str = "android.view.IWindowSession";

// Image transaction codes.
const OPEN_SESSION: u32 = 4;
const ADD_TO_DISPLAY_AS_USER: u32 = 2;
const RELAYOUT: u32 = 5;

// ---------------------------------------------------------------------------
// harness (requests are built in the image's write order, as in W1's test)
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
    data.write_int32(1); // writeTypedObject(LayoutParams)
    app_params().write_to_parcel(&mut data).expect("LayoutParams");
    data.write_int32(0); // viewVisibility
    data.write_int32(-1); // layerStackId
    data.write_int32(0); // userId
    data.write_int32(0); // requestedVisibleTypes
    data.write_int32(1); // sizeCompatScale.length
    data.set_data_position(0);
    data
}

fn relayout_request(window: BinderId) -> Parcel {
    let mut data = Parcel::new();
    data.write_interface_token(IWINDOW_SESSION).expect("token");
    data.write_strong_binder_opt(Some(window));
    data.write_int32(1);
    app_params().write_to_parcel(&mut data).expect("LayoutParams");
    data.write_int32(-1); // requestedWidth
    data.write_int32(-1); // requestedHeight
    data.write_int32(0); // viewVisibility
    data.write_int32(0); // flags
    data.write_int32(0); // seq
    data.write_int32(-1); // lastSyncSeqId
    data.set_data_position(0);
    data
}

fn transact(host: &mut ServiceHost, binder: BinderId, code: u32, data: &mut Parcel) -> Parcel {
    let mut reply = Parcel::new();
    host.transact(binder, code, data, &mut reply, 0)
        .expect("transaction");
    reply.set_data_position(0);
    assert_eq!(
        reply.read_int32().expect("exception header"),
        NO_EXCEPTION,
        "the reply must start with writeNoException"
    );
    reply
}

fn open_session(host: &mut ServiceHost) -> BinderId {
    let window = host
        .binder_for_descriptor(IWINDOW_MANAGER)
        .expect("window service");
    let mut reply = transact(host, window, OPEN_SESSION, &mut open_session_request());
    reply.read_strong_binder().expect("session").expect("session")
}

// ---------------------------------------------------------------------------
// hand transcriptions (framework order; no codec readers)
// ---------------------------------------------------------------------------

/// `Rect.readFromParcel`: four ints, no presence marker.
fn fw_rect(p: &mut Parcel) -> [i32; 4] {
    [
        p.read_int32().expect("rect.left"),
        p.read_int32().expect("rect.top"),
        p.read_int32().expect("rect.right"),
        p.read_int32().expect("rect.bottom"),
    ]
}

/// `Parcel.readTypedObject`: presence marker, then the bare body.
fn fw_typed<T>(p: &mut Parcel, body: impl FnOnce(&mut Parcel) -> T) -> Option<T> {
    if p.read_int32().expect("typed object presence") == 0 {
        None
    } else {
        Some(body(p))
    }
}

/// `Parcel.readTypedArray`: count, then one `readTypedObject` per element.
fn fw_typed_array<T>(p: &mut Parcel, mut body: impl FnMut(&mut Parcel) -> T) -> Vec<Option<T>> {
    let n = p.read_int32().expect("typed array count");
    assert!(n >= -1, "typed array count must be -1 or >= 0, was {n}");
    let mut out = Vec::new();
    for _ in 0..n.max(0) {
        out.push(fw_typed(p, &mut body));
    }
    out
}

/// `Parcel.readTypedArray(T[] val, Creator)` — the image's
/// `DisplayCutout.readCutoutFromParcel` reads into a **fixed `Rect[4]`** and
/// throws `RuntimeException("bad array lengths")` unless the wire count is
/// exactly 4 (`Parcel.java:3998`). This is the read that failed in
/// `results/art-runtime/L1.md` E2.
fn fw_typed_array_fixed(p: &mut Parcel, len: usize) -> Vec<Option<[i32; 4]>> {
    let n = p.read_int32().expect("typed array count");
    assert_eq!(
        n as usize, len,
        "readTypedArray(Rect[{len}]) requires an exact count; the framework throws 'bad array \
         lengths' for anything else (n = {n})"
    );
    (0..len).map(|_| fw_typed(p, fw_rect)).collect()
}

/// `android.graphics.Insets`: four ints.
fn fw_insets(p: &mut Parcel) -> [i32; 4] {
    fw_rect(p)
}

/// `InsetsSource(Parcel)`: id, type, bare frame, optional visibleFrame, visible,
/// flags, insetsRoundedCornerFrame.
fn fw_insets_source(p: &mut Parcel) -> (i32, i32, [i32; 4], Option<[i32; 4]>, bool, i32, bool) {
    let id = p.read_int32().expect("source.id");
    let type_ = p.read_int32().expect("source.type");
    let frame = fw_rect(p); // Rect.CREATOR.createFromParcel (no presence)
    let visible_frame = fw_typed(p, fw_rect); // readInt + Rect.createFromParcel
    let visible = p.read_bool().expect("source.visible");
    let flags = p.read_int32().expect("source.flags");
    let rounded = p.read_bool().expect("source.insetsRoundedCornerFrame");
    (id, type_, frame, visible_frame, visible, flags, rounded)
}

/// `InsetsState.readFromParcel` (image `:799`), including
/// `DisplayCutout$ParcelableWrapper.readCutoutFromParcel`.
struct FwInsetsState {
    display_frame: [i32; 4],
    cutout_bounds: Option<Vec<Option<[i32; 4]>>>,
    cutout_spec: Option<String>,
    rounded_corners: Option<usize>,
    privacy_bounds: Option<usize>,
    display_shape: Option<(String, i32, i32)>,
    sources: Vec<(i32, i32, [i32; 4], bool)>,
}

fn fw_insets_state(p: &mut Parcel) -> FwInsetsState {
    let display_frame = fw_rect(p); // mDisplayFrame.writeToParcel / readFromParcel
    // mDisplayCutout.readFromParcel -> readCutoutFromParcel
    let variant = p.read_int32().expect("cutout variant");
    let mut cutout_bounds = None;
    let mut cutout_spec = None;
    match variant {
        -1 | 0 => {
            assert!(
                variant == 0,
                "ACOS serves NO_CUTOUT (0) or a present cutout (1), not null"
            );
        }
        _ => {
            let safe_insets = fw_typed(p, fw_rect); // readTypedObject(Rect)
            assert!(safe_insets.is_some(), "a present cutout carries safe insets");
            // `new Rect[4]` + readTypedArray(val, Rect.CREATOR)
            let bounds = fw_typed_array_fixed(p, 4);
            cutout_bounds = Some(bounds);
            let waterfall = fw_typed(p, fw_insets); // readTypedObject(Insets)
            assert!(waterfall.is_some(), "a present cutout carries waterfall insets");
            let _display_width = p.read_int32().expect("cutout displayWidth");
            let _display_height = p.read_int32().expect("cutout displayHeight");
            let _physical_width = p.read_int32().expect("cutout physicalDisplayWidth");
            let _physical_height = p.read_int32().expect("cutout physicalDisplayHeight");
            let _density = p.read_float().expect("cutout density");
            cutout_spec = p.read_string16().expect("cutout cutoutSpec");
            let _rotation = p.read_int32().expect("cutout rotation");
            let _scale = p.read_float().expect("cutout scale");
            let _ratio = p.read_float().expect("cutout physicalPixelDisplaySizeRatio");
        }
    }
    // mRoundedCorners = in.readTypedObject(RoundedCorners.CREATOR)
    let rounded_corners = fw_typed(p, |p| {
        // RoundedCorners(Parcel): hasCorners int, then createTypedArray
        let has = p.read_int32().expect("roundedCorners marker");
        assert_ne!(has, 0, "a present RoundedCorners is not NO_ROUNDED_CORNERS");
        fw_typed_array(p, |p| {
            // RoundedCorner: position, radius, center.x, center.y
            let position = p.read_int32().expect("corner.position");
            let _radius = p.read_int32().expect("corner.radius");
            let _x = p.read_int32().expect("corner.center.x");
            let _y = p.read_int32().expect("corner.center.y");
            position
        })
        .len()
    });
    let _rounded_corner_frame = fw_rect(p); // mRoundedCornerFrame.readFromParcel
    // mPrivacyIndicatorBounds = in.readTypedObject(...)
    let privacy_bounds = fw_typed(p, |p| {
        // PrivacyIndicatorBounds(Parcel): createTypedArray(Rect.CREATOR), rotation
        let bounds = fw_typed_array(p, fw_rect);
        let _rotation = p.read_int32().expect("privacy rotation");
        bounds.len()
    });
    // mDisplayShape = in.readTypedObject(DisplayShape.CREATOR)
    let display_shape = fw_typed(p, |p| {
        // DisplayShape: writeString8, width, height, ratio, rotation, x, y, scale
        let spec = p.read_string8().expect("shape spec").unwrap_or_default();
        let width = p.read_int32().expect("shape width");
        let height = p.read_int32().expect("shape height");
        let _ratio = p.read_float().expect("shape ratio");
        let _rotation = p.read_int32().expect("shape rotation");
        let _x = p.read_int32().expect("shape offsetX");
        let _y = p.read_int32().expect("shape offsetY");
        let _scale = p.read_float().expect("shape scale");
        (spec, width, height)
    });
    // final int size = in.readInt(); for (i < size) readTypedObject(InsetsSource)
    let n = p.read_int32().expect("InsetsState source count");
    assert!(n >= 0, "source count must be >= 0, was {n}");
    let mut sources = Vec::new();
    for _ in 0..n {
        let (id, type_, frame, _visible_frame, visible, _flags, _rounded) =
            fw_typed(p, fw_insets_source).expect("non-null InsetsSource");
        sources.push((id, type_, frame, visible));
    }
    FwInsetsState {
        display_frame,
        cutout_bounds,
        cutout_spec,
        rounded_corners,
        privacy_bounds,
        display_shape,
        sources,
    }
}

/// `InsetsSourceControl(Parcel)` + `InsetsSourceControl$Array.readFromParcel`
/// (`createTypedArray`), including the `SurfaceControl` leash body.
fn fw_surface_control(p: &mut Parcel) -> (String, i32, i32, Option<FwSurfaceNative>) {
    // `SurfaceControl.readFromParcel` (image `SurfaceControl.java:0x1d26ac`):
    // String8 name, int width, int height, int present; then, when present,
    // `SurfaceControl::readFromParcel` (libgui `:250`):
    // client binder, handle binder, layerId, String16 layerName,
    // transformHint, width, height, format.
    let name = p.read_string8().expect("surface name").unwrap_or_default();
    let width = p.read_int32().expect("surface width");
    let height = p.read_int32().expect("surface height");
    let present = p.read_int32().expect("surface handle present");
    let native = if present != 0 {
        let client = p.read_strong_binder().expect("surface client binder");
        let handle = p.read_strong_binder().expect("surface handle binder");
        let layer_id = p.read_int32().expect("surface layerId");
        let layer_name = p.read_string16().expect("surface layerName");
        let transform_hint = p.read_int32().expect("surface transformHint");
        let w = p.read_int32().expect("surface native width");
        let h = p.read_int32().expect("surface native height");
        let format = p.read_int32().expect("surface format");
        Some(FwSurfaceNative {
            client_is_null: client.is_none(),
            handle_is_null: handle.is_none(),
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
    (name, width, height, native)
}

struct FwSurfaceNative {
    client_is_null: bool,
    handle_is_null: bool,
    layer_id: i32,
    layer_name: Option<String>,
    transform_hint: i32,
    width: i32,
    height: i32,
    format: i32,
}

fn fw_insets_source_control(p: &mut Parcel) -> (i32, i32, Option<String>) {
    let id = p.read_int32().expect("control.id");
    let type_ = p.read_int32().expect("control.type");
    let leash = fw_typed(p, fw_surface_control).map(|t| t.0);
    let _initially_visible = p.read_bool().expect("control.initiallyVisible");
    let _position = fw_typed(p, |p| {
        (
            p.read_int32().expect("point.x"),
            p.read_int32().expect("point.y"),
        )
    });
    let _insets_hint = fw_typed(p, fw_insets);
    let _skip_animation_once = p.read_bool().expect("control.skipAnimationOnce");
    (id, type_, leash)
}

fn fw_insets_control_array(p: &mut Parcel) -> Vec<Option<(i32, i32, Option<String>)>> {
    fw_typed_array(p, fw_insets_source_control)
}

/// `Configuration.readFromParcel` + `WindowConfiguration.readFromParcel`.
fn fw_configuration(p: &mut Parcel) -> i32 {
    let _font_scale = p.read_float().expect("fontScale");
    let _mcc = p.read_int32().expect("mcc");
    let _mnc = p.read_int32().expect("mnc");
    // readTypedObject(LocaleList): LocaleList writes String8.
    let _locale = fw_typed(p, |p| p.read_string8().expect("locale list"));
    let _user_set_locale = p.read_int32().expect("userSetLocale");
    for name in [
        "touchscreen",
        "keyboard",
        "keyboardHidden",
        "hardKeyboardHidden",
        "navigation",
        "navigationHidden",
        "orientation",
        "screenLayout",
        "colorMode",
        "uiMode",
        "screenWidthDp",
        "screenHeightDp",
        "smallestScreenWidthDp",
        "densityDpi",
        "compatScreenWidthDp",
        "compatScreenHeightDp",
        "compatSmallestScreenWidthDp",
    ] {
        let _ = p.read_int32().unwrap_or_else(|e| panic!("{name}: {e}"));
    }
    // WindowConfiguration.readFromParcel
    let _bounds = fw_rect(p);
    let _app_bounds = fw_typed(p, fw_rect);
    let _max_bounds = fw_rect(p);
    let windowing_mode = p.read_int32().expect("windowingMode");
    for name in [
        "activityType",
        "alwaysOnTop",
        "rotation",
        "displayWindowingMode",
        "displayRotation",
    ] {
        let _ = p.read_int32().unwrap_or_else(|e| panic!("{name}: {e}"));
    }
    let _assets_seq = p.read_int32().expect("assetsSeq");
    let _seq = p.read_int32().expect("seq");
    let _font_weight_adjustment = p.read_int32().expect("fontWeightAdjustment");
    let _grammatical_gender = p.read_int32().expect("mGrammaticalGender");
    windowing_mode
}

/// `ClientWindowFrames.readFromParcel`: frame, displayFrame, parentFrame (bare
/// `Rect.readFromParcel`), attachedFrame (typed), clipped bool, compatScale.
fn fw_client_window_frames(p: &mut Parcel) -> ([i32; 4], [i32; 4], [i32; 4], Option<[i32; 4]>) {
    let frame = fw_rect(p);
    let display_frame = fw_rect(p);
    let parent_frame = fw_rect(p);
    let attached = fw_typed(p, fw_rect);
    let _clipped = p.read_bool().expect("isParentFrameClippedByDisplayCutout");
    let _compat_scale = p.read_float().expect("compatScale");
    (frame, display_frame, parent_frame, attached)
}

// ---------------------------------------------------------------------------
// tests
// ---------------------------------------------------------------------------

#[test]
fn add_to_display_as_user_reply_reads_exactly_in_the_framework_order() {
    let mut host = ServiceHost::standard();
    let session = open_session(&mut host);
    let window = BinderId(0x6101);
    let mut reply = transact(&mut host, session, ADD_TO_DISPLAY_AS_USER, &mut add_request(window));

    // int result
    let result = reply.read_int32().expect("result");
    assert_eq!(result, ADD_OKAY);

    // InputChannel: AIDL presence, then
    // nativeReadFromParcel: isInitialized, token, name, fd.
    assert_eq!(reply.read_int32().expect("InputChannel presence"), 1);
    assert_eq!(
        reply.read_int32().expect("InputChannel isInitialized"),
        1,
        "android_view_InputChannel_nativeReadFromParcel reads this int before the body"
    );
    let token = reply.read_strong_binder().expect("InputChannel token");
    assert!(token.is_some(), "the channel must carry a token binder");
    let name = reply.read_string16().expect("InputChannel name").expect("name");
    assert!(
        name.contains("dev.acos.fixtures.helloviews/MainActivity"),
        "channel name was {name}"
    );
    let fd = reply.read_file_descriptor().expect("InputChannel fd");
    assert!(fd.is_some(), "the channel must carry a real descriptor");

    // out InsetsState
    assert_eq!(reply.read_int32().expect("InsetsState presence"), 1);
    let insets = fw_insets_state(&mut reply);
    assert_eq!(insets.display_frame, [0, 0, 1080, 2400]);
    let bounds = insets.cutout_bounds.expect("the reference display has a cutout");
    assert_eq!(bounds.len(), 4, "DisplayCutout.Bounds is a fixed Rect[4]");
    assert!(
        bounds[1].expect("top cutout rect").eq(&[492, 0, 610, 128]),
        "the hole-punch rect comes from display.txt, was {:?}",
        bounds[1]
    );
    assert!(
        insets.cutout_spec.as_deref().unwrap_or_default().contains("a 33,33"),
        "the cutout path spec is carried inline"
    );
    assert_eq!(insets.rounded_corners, Some(4), "four 28px rounded corners");
    assert_eq!(insets.privacy_bounds, Some(1), "one privacy indicator rect");
    let shape = insets.display_shape.expect("mDisplayShape must not be null");
    assert_eq!((shape.1, shape.2), (0, 0), "DisplayShape.NONE is 0x0");
    assert_eq!(insets.sources.len(), 10, "the ten display.txt InsetsSources");
    let status = insets
        .sources
        .iter()
        .find(|s| s.1 == 1)
        .expect("status bar source");
    assert_eq!(status.2, [0, 0, 1080, 128]);
    assert!(status.3, "the status bar is visible");

    // out InsetsSourceControl.Array
    assert_eq!(reply.read_int32().expect("controls presence"), 1);
    let controls = fw_insets_control_array(&mut reply);
    assert!(!controls.is_empty(), "the app receives status/nav controls");
    assert!(
        controls[0].as_ref().is_some_and(|c| c.0 != 0 && c.1 != 0),
        "each control carries an id and a type"
    );

    // out Rect attachedFrame
    assert_eq!(reply.read_int32().expect("attachedFrame presence"), 1);
    let attached = fw_rect(&mut reply);
    assert_eq!(attached, [0, 128, 1080, 2337]);

    // out float[] sizeCompatScale
    let scale = reply
        .read_float_array()
        .expect("sizeCompatScale")
        .unwrap_or_default();
    assert_eq!(scale, vec![1.0]);

    assert_eq!(
        reply.data_avail(),
        0,
        "the hand transcription must consume the whole reply ({} bytes left)",
        reply.data_avail()
    );
}

#[test]
fn relayout_reply_reads_exactly_in_the_framework_order() {
    let mut host = ServiceHost::standard();
    let session = open_session(&mut host);
    let window = BinderId(0x6102);
    let _ = transact(&mut host, session, ADD_TO_DISPLAY_AS_USER, &mut add_request(window));
    let mut reply = transact(&mut host, session, RELAYOUT, &mut relayout_request(window));

    let result = reply.read_int32().expect("result");
    assert!(result > 0, "relayout returns a result bitmask");

    // out ClientWindowFrames
    assert_eq!(reply.read_int32().expect("ClientWindowFrames presence"), 1);
    let (frame, display_frame, parent_frame, attached) = fw_client_window_frames(&mut reply);
    assert_eq!(frame, [0, 128, 1080, 2337]);
    assert_eq!(display_frame, [0, 0, 1080, 2400]);
    assert!(parent_frame[2] >= parent_frame[0]);
    assert!(attached.is_some(), "attachedFrame is written for the first relayout");

    // out MergedConfiguration: three bare Configuration payloads
    assert_eq!(reply.read_int32().expect("MergedConfiguration presence"), 1);
    let global_windowing_mode = fw_configuration(&mut reply);
    let override_windowing_mode = fw_configuration(&mut reply);
    let merged_windowing_mode = fw_configuration(&mut reply);
    assert_eq!(
        global_windowing_mode, override_windowing_mode,
        "the override configuration is the merged application configuration"
    );
    assert_eq!(merged_windowing_mode, global_windowing_mode);

    // out SurfaceControl: the structural shape the image's reader expects.
    assert_eq!(reply.read_int32().expect("SurfaceControl presence"), 1);
    let (surface_name, width, height, handle) = fw_surface_control(&mut reply);
    assert!(
        surface_name.contains("dev.acos.fixtures.helloviews"),
        "surface name was {surface_name}"
    );
    assert_eq!((width, height), (1080, 2209), "the app window surface size");
    let native = handle.expect("the placeholder carries a native object");
    assert!(
        !native.client_is_null && !native.handle_is_null,
        "the leash client/handle must be non-null: SurfaceControl::readFromParcel reads both with \
         SAFE_PARCEL(parcel.readStrongBinder, ...), and Parcel::readStrongBinder turns a \
         *readable* null binder into UNEXPECTED_NULL, aborting the parse and desynchronising the \
         enclosing InsetsSourceControl$Array (W3 §2; `readObject`'s null fast path only helps \
         readNullableStrongBinder). The binders are the documented placeholder — live and \
         routable, but declaring no ISurfaceComposerClient interface."
    );
    assert_eq!(native.layer_name.as_deref(), Some(surface_name.as_str()));
    assert_eq!((native.width, native.height), (width, height));
    assert_eq!(native.transform_hint, 0);
    assert_eq!(native.format, 1, "PIXEL_FORMAT_RGBA_8888");
    assert_ne!(native.layer_id, 0, "the synthetic ACOS layer id");

    // out InsetsState
    assert_eq!(reply.read_int32().expect("InsetsState presence"), 1);
    let insets = fw_insets_state(&mut reply);
    assert_eq!(insets.sources.len(), 10);
    assert_eq!(insets.display_frame, [0, 0, 1080, 2400]);

    // out InsetsSourceControl.Array
    assert_eq!(reply.read_int32().expect("controls presence"), 1);
    let _controls = fw_insets_control_array(&mut reply);

    // trailing out Bundle (SyncSeqId): null
    assert_eq!(
        reply.read_int32().expect("out Bundle presence"),
        0,
        "ACOS has no surface sync group, so the Bundle is null"
    );

    assert_eq!(
        reply.data_avail(),
        0,
        "the hand transcription must consume the whole relayout reply ({} bytes left)",
        reply.data_avail()
    );
}
