//! W32 acceptance: a **dialog** window (`TYPE_APPLICATION` = 2, `token = null`,
//! `WRAP_CONTENT`) on the real `IWindowSession` wire, next to the Activity window.
//!
//! The measurement this pins (`results/art-runtime/W32.md` §1): W27 believed the
//! first non-Activity `addView` blocked the app main thread. It does not — the
//! host answers `addToDisplayAsUser` (code 2), `relayout` (code 5) and
//! `finishDrawing` (code 9) for the dialog, with its own compositor layer, its own
//! `InputChannel` (so the newest window's channel is the dialog's) and a
//! `WRAP_CONTENT` frame from the client-measured size. These tests keep that
//! behaviour honest from the host side, and pin the negative cases a second add
//! can produce.
//!
//! Wire shapes and codes are the API-34 image's `IWindowSession$Stub$Proxy`
//! (identical to `window_session_api34.rs`, whose byte-exact decode helpers this
//! file re-derives for the two windows it needs).

use acos_parcel::{BinderId, Parcel};
use acos_services::exception::NO_EXCEPTION;
use acos_services::ServiceHost;
use acos_wm::codec::{ParcelCodec, SurfaceControlInfo};
use acos_wm::geom::RectI;
use acos_wm::wms::{ADD_BAD_APP_TOKEN, ADD_DUPLICATE_ADD, ADD_OKAY};

const IWINDOW_MANAGER: &str = "android.view.IWindowManager";
const IWINDOW_SESSION: &str = "android.view.IWindowSession";

const OPEN_SESSION: u32 = 4;
const ADD_TO_DISPLAY_AS_USER: u32 = 2;
const RELAYOUT: u32 = 5;
const FINISH_DRAWING: u32 = 9;
const ONEWAY: u32 = 1;

fn transact(
    host: &mut ServiceHost,
    binder: BinderId,
    code: u32,
    flags: u32,
    data: &mut Parcel,
) -> Parcel {
    let mut reply = Parcel::new();
    host.transact(binder, code, data, &mut reply, flags)
        .expect("IWindowSession transaction");
    reply.set_data_position(0);
    reply
}

fn no_exception(reply: &mut Parcel) {
    assert_eq!(reply.read_int32().expect("exception header"), NO_EXCEPTION);
}

fn open_session(host: &mut ServiceHost) -> BinderId {
    let mut data = Parcel::new();
    data.write_interface_token(IWINDOW_MANAGER).expect("token");
    data.write_strong_binder_opt(None);
    data.set_data_position(0);
    let manager = host
        .binder_for_descriptor(IWINDOW_MANAGER)
        .expect("IWindowManager node");
    let mut reply = transact(host, manager, OPEN_SESSION, 0, &mut data);
    no_exception(&mut reply);
    reply
        .read_strong_binder()
        .expect("openSession reply")
        .expect("a routable IWindowSession binder")
}

/// The Activity window's params: `TYPE_BASE_APPLICATION`-style full-size window.
fn activity_params() -> Parcel {
    let mut p = Parcel::new();
    acos_wm::codec::LayoutParams::application("dev.acos.fixtures.helloviews", "MainActivity")
        .write_to_parcel(&mut p)
        .expect("activity LayoutParams");
    p
}

/// The dialog window's params as the image's `ViewRootImpl.setView` sends them:
/// `type = TYPE_APPLICATION (2)`, `token = null`, `WRAP_CONTENT` for both axes.
fn dialog_params() -> Parcel {
    let mut lp = acos_wm::codec::LayoutParams::application(
        "dev.acos.fixtures.helloviews",
        "MainActivity",
    );
    lp.type_ = 2;
    lp.token = None;
    lp.width = -2;
    lp.height = -2;
    let mut p = Parcel::new();
    lp.write_to_parcel(&mut p).expect("dialog LayoutParams");
    p
}

fn add_request(window: BinderId, params: &Parcel) -> Parcel {
    let mut data = Parcel::new();
    data.write_interface_token(IWINDOW_SESSION).expect("token");
    data.write_strong_binder_opt(Some(window));
    data.write_int32(1); // writeTypedObject(LayoutParams) presence
    let mut body = params.clone();
    body.set_data_position(0);
    let avail = body.data_avail();
    data.append_from(&body, 0, avail).expect("LayoutParams body");
    data.write_int32(0); // viewVisibility
    data.write_int32(-1); // layerStackId
    data.write_int32(0); // userId
    data.write_int32(0); // requestedVisibleTypes
    data.write_int32(2); // out float[] length
    data.set_data_position(0);
    data
}

struct AddReply {
    result: i32,
    channel_token: Option<BinderId>,
    channel_name: String,
    attached_frame: RectI,
}

fn read_add_reply(reply: &mut Parcel) -> AddReply {
    let result = reply.read_int32().expect("result");
    assert_eq!(reply.read_int32().expect("InputChannel present"), 1);
    assert_eq!(reply.read_int32().expect("isInitialized"), 1);
    let channel_token = reply.read_strong_binder().expect("channel token");
    let channel_name = reply.read_string16().expect("channel name").expect("channel name");
    let _fd = reply.read_file_descriptor().expect("channel fd");
    // InsetsState / InsetsSourceControl.Array / Rect are not this test's subject;
    // skip them with the framework's presence words so the reply is consumed.
    assert_eq!(reply.read_int32().expect("InsetsState present"), 1);
    let _ = acos_wm::codec::InsetsState::read_from_parcel(reply).expect("InsetsState");
    assert_eq!(reply.read_int32().expect("controls present"), 1);
    let _ = acos_wm::codec::InsetsSourceControlArray::read_from_parcel(reply)
        .expect("InsetsSourceControl.Array");
    assert_eq!(reply.read_int32().expect("attachedFrame present"), 1);
    let attached_frame = RectI::read_from_parcel(reply).expect("attachedFrame");
    let _ = reply.read_float_array().expect("sizeCompatScale");
    AddReply {
        result,
        channel_token,
        channel_name,
        attached_frame,
    }
}

struct RelayoutReply {
    result: i32,
    frame: RectI,
    surface: SurfaceControlInfo,
}

fn relayout_request(window: BinderId, params: Option<&Parcel>, width: i32, height: i32) -> Parcel {
    let mut data = Parcel::new();
    data.write_interface_token(IWINDOW_SESSION).expect("token");
    data.write_strong_binder_opt(Some(window));
    match params {
        Some(p) => {
            data.write_int32(1);
            let mut body = p.clone();
            body.set_data_position(0);
            let avail = body.data_avail();
            data.append_from(&body, 0, avail).expect("LayoutParams body");
        }
        None => data.write_int32(0),
    }
    data.write_int32(width); // requestedWidth
    data.write_int32(height); // requestedHeight
    data.write_int32(0); // viewVisibility
    data.write_int32(0); // flags
    data.write_int32(0); // seq
    data.write_int32(-1); // lastSyncSeqId
    data.set_data_position(0);
    data
}

fn read_relayout_reply(reply: &mut Parcel) -> RelayoutReply {
    let result = reply.read_int32().expect("result");
    assert_eq!(reply.read_int32().expect("frames present"), 1);
    let frames =
        acos_wm::codec::ClientWindowFrames::read_from_parcel(reply).expect("ClientWindowFrames");
    assert_eq!(reply.read_int32().expect("config present"), 1);
    let _ = acos_wm::codec::MergedConfiguration::read_from_parcel(reply)
        .expect("MergedConfiguration");
    assert_eq!(reply.read_int32().expect("surface present"), 1);
    let surface = SurfaceControlInfo::read_from_parcel(reply).expect("SurfaceControl");
    assert_eq!(reply.read_int32().expect("insets present"), 1);
    let _ = acos_wm::codec::InsetsState::read_from_parcel(reply).expect("InsetsState");
    assert_eq!(reply.read_int32().expect("controls present"), 1);
    let _ = acos_wm::codec::InsetsSourceControlArray::read_from_parcel(reply).expect("controls");
    let _bundle = reply.read_int32().expect("out Bundle");
    RelayoutReply {
        result,
        frame: frames.frame,
        surface,
    }
}

#[test]
fn a_dialog_window_is_added_relayouted_and_drawn_as_its_own_top_level_window() {
    let mut host = ServiceHost::standard_with_insets(acos_services::wms::InsetsMode::Carve);
    let session = open_session(&mut host);

    let activity = BinderId(0x5701);
    let dialog = BinderId(0x5702);

    // 1. The Activity window (the W1 path), unchanged.
    let mut data = add_request(activity, &activity_params());
    let mut reply = transact(&mut host, session, ADD_TO_DISPLAY_AS_USER, 0, &mut data);
    no_exception(&mut reply);
    let a = read_add_reply(&mut reply);
    assert_eq!(reply.data_avail(), 0, "add reply consumed exactly");
    assert_eq!(a.result, ADD_OKAY);
    assert_eq!(a.attached_frame, RectI::new(0, 128, 1080, 2337));

    // 2. The dialog: type 2, no token, WRAP_CONTENT. Stock answers ADD_OKAY.
    let mut data = add_request(dialog, &dialog_params());
    let mut reply = transact(&mut host, session, ADD_TO_DISPLAY_AS_USER, 0, &mut data);
    no_exception(&mut reply);
    let d = read_add_reply(&mut reply);
    assert_eq!(reply.data_avail(), 0, "add reply consumed exactly");
    assert_eq!(
        d.result, ADD_OKAY,
        "a TYPE_APPLICATION dialog is a top-level app window, not a sub-window"
    );
    assert_eq!(d.attached_frame, RectI::new(0, 128, 1080, 2337));

    // 3. Two distinct real InputChannels, and the dialog is the newest (the
    //    `InputDispatch::tap` contract the dialog tap depends on).
    assert_ne!(a.channel_token, d.channel_token);
    assert_ne!(a.channel_name, d.channel_name);
    assert!(
        d.channel_name.contains("MainActivity"),
        "the dialog's channel names its window: {}",
        d.channel_name
    );

    // 4. The Activity relayout: full app frame, one compositor layer.
    let mut data = relayout_request(activity, Some(&activity_params()), 1080, 2209);
    let mut reply = transact(&mut host, session, RELAYOUT, 0, &mut data);
    no_exception(&mut reply);
    let ra = read_relayout_reply(&mut reply);
    assert_eq!(reply.data_avail(), 0, "relayout reply consumed exactly");
    assert_eq!(ra.frame, RectI::new(0, 128, 1080, 2337));
    let activity_handle = ra.surface.handle.expect("activity SurfaceControl handle");

    // 5. The dialog relayout: the client-measured WRAP_CONTENT size, centred in
    //    the app bounds, on a **different** layer (its own SurfaceControl).
    let mut data = relayout_request(dialog, Some(&dialog_params()), 776, 513);
    let mut reply = transact(&mut host, session, RELAYOUT, 0, &mut data);
    no_exception(&mut reply);
    let rd = read_relayout_reply(&mut reply);
    assert_eq!(reply.data_avail(), 0, "relayout reply consumed exactly");
    assert_eq!(
        rd.frame,
        RectI::new(152, 976, 928, 1489),
        "x=(1080-776)/2=152, y=128+(2209-513)/2=976"
    );
    assert_eq!(rd.surface.width, 776);
    assert_eq!(rd.surface.height, 513);
    let dialog_handle = rd.surface.handle.expect("dialog SurfaceControl handle");
    assert_ne!(
        activity_handle, dialog_handle,
        "the dialog must not share the Activity's layer: a released leash or a \
         later resize would invalidate both"
    );

    // 6. The dialog draws: `finishDrawing` (code 9) is accepted for it.
    let mut data = Parcel::new();
    data.write_interface_token(IWINDOW_SESSION).expect("token");
    data.write_strong_binder_opt(Some(dialog));
    data.write_int32(1); // SurfaceControl$Transaction presence (typed object)
    data.write_int32(0); // empty transaction body: the host records, never applies
    data.write_int32(0); // seqId
    data.set_data_position(0);
    // `finishDrawing` is oneway: a complete reply is an *empty* one.
    let reply = transact(&mut host, session, FINISH_DRAWING, ONEWAY, &mut data);
    assert_eq!(reply.data_avail(), 0, "a oneway reply carries no body");

    // 7. A second relayout of the dialog is stable (no resize, same frame and
    //    layer) — the shape the guest's second traversal produced live.
    let mut data = relayout_request(dialog, None, 776, 513);
    let mut reply = transact(&mut host, session, RELAYOUT, 0, &mut data);
    no_exception(&mut reply);
    let rd2 = read_relayout_reply(&mut reply);
    assert_eq!(reply.data_avail(), 0);
    assert_eq!(rd2.frame, rd.frame);
    assert_eq!(rd2.surface.handle, rd.surface.handle);
    assert_eq!(
        rd2.result & acos_wm::wms::RELAYOUT_RES_FIRST_TIME,
        0,
        "only the first relayout reports FIRST_TIME"
    );
}

/// A refused add still carries the full reply shape, but with an
/// **uninitialised** channel (`nativeWriteToParcel`: presence 1 then
/// `isInitialized = 0`), never a fabricated descriptor.
struct RefusedAdd {
    result: i32,
    channel_initialized: bool,
}

fn read_refused_add_reply(reply: &mut Parcel) -> RefusedAdd {
    let result = reply.read_int32().expect("result");
    assert_eq!(reply.read_int32().expect("InputChannel presence"), 1);
    let channel_initialized = reply.read_int32().expect("isInitialized") == 1;
    assert_eq!(reply.read_int32().expect("InsetsState present"), 1);
    let _ = acos_wm::codec::InsetsState::read_from_parcel(reply).expect("InsetsState");
    assert_eq!(reply.read_int32().expect("controls present"), 1);
    let _ = acos_wm::codec::InsetsSourceControlArray::read_from_parcel(reply).expect("controls");
    assert_eq!(reply.read_int32().expect("attachedFrame present"), 1);
    let _ = RectI::read_from_parcel(reply).expect("attachedFrame");
    let _ = reply.read_float_array().expect("sizeCompatScale");
    RefusedAdd {
        result,
        channel_initialized,
    }
}

#[test]
fn a_duplicate_or_null_dialog_add_is_a_typed_refusal() {
    let mut host = ServiceHost::standard_with_insets(acos_services::wms::InsetsMode::Carve);
    let session = open_session(&mut host);
    let dialog = BinderId(0x5801);

    let mut data = add_request(dialog, &dialog_params());
    let mut reply = transact(&mut host, session, ADD_TO_DISPLAY_AS_USER, 0, &mut data);
    no_exception(&mut reply);
    assert_eq!(read_add_reply(&mut reply).result, ADD_OKAY);

    // The same IWindow twice is ADD_DUPLICATE_ADD, and the first record is kept.
    let mut data = add_request(dialog, &dialog_params());
    let mut reply = transact(&mut host, session, ADD_TO_DISPLAY_AS_USER, 0, &mut data);
    no_exception(&mut reply);
    let dup = read_refused_add_reply(&mut reply);
    assert_eq!(dup.result, ADD_DUPLICATE_ADD);
    assert!(
        !dup.channel_initialized,
        "the duplicate must not get a second channel to install a receiver on"
    );
    assert_eq!(reply.data_avail(), 0, "the refusal reply is consumed exactly");

    // A null IWindow cannot be a window: ADD_BAD_APP_TOKEN, never a fabricated
    // channel (the reply still carries the uninitialised-channel body).
    let mut data = add_request(BinderId(0), &dialog_params());
    let mut reply = transact(&mut host, session, ADD_TO_DISPLAY_AS_USER, 0, &mut data);
    no_exception(&mut reply);
    let null = read_refused_add_reply(&mut reply);
    assert_eq!(null.result, ADD_BAD_APP_TOKEN);
    assert!(!null.channel_initialized);
    assert_eq!(reply.data_avail(), 0);
}
