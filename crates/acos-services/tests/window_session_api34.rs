//! W1 acceptance: the real `ViewRootImpl` window path reaches `ServiceHost`.
//!
//! Every request below is built **by hand, in the order the image's generated
//! client writes it**, and every reply is decoded in the image's own read order.
//! The evidence is the API-34 image's `IWindowSession$Stub$Proxy`
//! (`classes4.dex`, disassembled with `cargo run -p acos-dex --example
//! dexdump`); the transaction codes and argument orders quoted next to each
//! test are from that disassembly, not from the `android-14.0.0_r75` AIDL (which
//! is one code ahead for the tail of the interface).
//!
//! Nothing is asserted through the ACOS value model: every decoder must consume
//! the reply exactly (`data_avail() == 0`), and the parcelable payloads are
//! re-encoded from the decoded fields and compared byte-for-byte.
//!
//! ```text
//! addToDisplayAsUser (code 2) request:
//!   writeInterfaceToken("android.view.IWindowSession")
//!   writeStrongInterface(IWindow)         writeTypedObject(LayoutParams)
//!   writeInt(viewVisibility) writeInt(layerStackId) writeInt(userId)
//!   writeInt(requestedVisibleTypes) writeInt(sizeCompatScale.length)
//! reply:
//!   readInt()=result
//!   readInt()=1 -> InputChannel.writeToParcel (nativeReadFromParcel):
//!                  readInt()=isInitialized, readStrongBinder()=token,
//!                  readString16()=name, readFileDescriptor()=fd
//!   readInt()=1 -> InsetsState.readFromParcel
//!   readInt()=1 -> InsetsSourceControl$Array.readFromParcel
//!   readInt()=1 -> Rect.readFromParcel
//!   readFloatArray(sizeCompatScale)
//!
//! relayout (code 5) reply:
//!   readInt()=result
//!   readInt()=1 -> ClientWindowFrames  readInt()=1 -> MergedConfiguration
//!   readInt()=1 -> SurfaceControl      readInt()=1 -> InsetsState
//!   readInt()=1 -> InsetsSourceControl$Array  readInt()=bundle present (0)
//! ```

use acos_parcel::{BinderId, Parcel};
use acos_services::exception::NO_EXCEPTION;
use acos_services::wms::WmsService;
use acos_services::{ServiceError, ServiceHost};
use acos_wm::codec::{
    ClientWindowFrames, InsetsSourceControlArray, InsetsState, LayoutParams, MergedConfiguration,
    ParcelCodec, SurfaceControlInfo,
};
use acos_wm::geom::RectI;
use acos_wm::wms::ADD_OKAY;

const IWINDOW_MANAGER: &str = "android.view.IWindowManager";
const IWINDOW_SESSION: &str = "android.view.IWindowSession";

// Image (`classes4.dex`) transaction codes.
const OPEN_SESSION: u32 = 4;
const ADD_TO_DISPLAY_AS_USER: u32 = 2;
const REMOVE: u32 = 4;
const RELAYOUT: u32 = 5;
const OUT_OF_MEMORY: u32 = 7;
const SET_INSETS: u32 = 8;
const FINISH_DRAWING: u32 = 9;
const PERFORM_HAPTIC_FEEDBACK: u32 = 10;
const ON_RECTANGLE_ON_SCREEN_REQUESTED: u32 = 25;
const GET_WINDOW_ID: u32 = 26;
const POKE_DRAW_LOCK: u32 = 27;
const UPDATE_REQUESTED_VISIBLE_TYPES: u32 = 32;
const REPORT_SYSTEM_GESTURE_EXCLUSION_CHANGED: u32 = 33;
const CANCEL_DRAW: u32 = 41;

const ONEWAY: u32 = 1;

fn app_params() -> LayoutParams {
    LayoutParams::application("dev.acos.fixtures.helloviews", "MainActivity")
}

/// `IWindowManager$Stub$Proxy.openSession(IWindowSessionCallback)`.
fn open_session_request() -> Parcel {
    let mut data = Parcel::new();
    data.write_interface_token(IWINDOW_MANAGER).expect("token");
    data.write_strong_binder_opt(None); // the session callback binder
    data.set_data_position(0);
    data
}

/// `IWindowSession$Stub$Proxy.addToDisplayAsUser`, byte for byte.
fn add_to_display_as_user_request(window: BinderId) -> Parcel {
    let mut data = Parcel::new();
    data.write_interface_token(IWINDOW_SESSION).expect("token");
    data.write_strong_binder_opt(Some(window));
    data.write_int32(1); // writeTypedObject(LayoutParams) presence
    app_params().write_to_parcel(&mut data).expect("LayoutParams");
    data.write_int32(0); // viewVisibility
    data.write_int32(-1); // layerStackId
    data.write_int32(0); // userId
    data.write_int32(0); // requestedVisibleTypes
    // `out float[]`: the client preallocates the array and writes its length;
    // the reply count must equal it (`Parcel.readFloatArray` compares them).
    data.write_int32(2);
    data.set_data_position(0);
    data
}

/// `IWindowSession$Stub$Proxy.relayout(..)`, byte for byte.
fn relayout_request(window: BinderId, params: Option<&LayoutParams>) -> Parcel {
    let mut data = Parcel::new();
    data.write_interface_token(IWINDOW_SESSION).expect("token");
    data.write_strong_binder_opt(Some(window));
    match params {
        Some(p) => {
            data.write_int32(1);
            p.write_to_parcel(&mut data).expect("LayoutParams");
        }
        None => data.write_int32(0),
    }
    data.write_int32(-1); // requestedWidth
    data.write_int32(-1); // requestedHeight
    data.write_int32(0); // viewVisibility
    data.write_int32(0); // flags
    data.write_int32(0); // seq
    data.write_int32(-1); // lastSyncSeqId
    data.set_data_position(0);
    data
}

/// A request parcel with the interface token already written; the caller
/// appends the arguments in the framework's order and then rewinds.
fn session_data() -> Parcel {
    let mut data = Parcel::new();
    data.write_interface_token(IWINDOW_SESSION).expect("token");
    data
}

fn binder_only_request(window: BinderId) -> Parcel {
    let mut data = Parcel::new();
    data.write_interface_token(IWINDOW_SESSION).expect("token");
    data.write_strong_binder_opt(Some(window));
    data.set_data_position(0);
    data
}

fn ibinder_request(token: BinderId) -> Parcel {
    let mut data = Parcel::new();
    data.write_interface_token(IWINDOW_SESSION).expect("token");
    data.write_strong_binder_opt(Some(token));
    data.set_data_position(0);
    data
}

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

fn open_session(host: &mut ServiceHost) -> BinderId {
    let window = host
        .binder_for_descriptor(IWINDOW_MANAGER)
        .expect("window service");
    let reply = transact(
        host,
        window,
        OPEN_SESSION,
        0,
        &mut open_session_request(),
    )
    .expect("openSession");
    let mut reply = reply;
    no_exception(&mut reply);
    let session = reply
        .read_strong_binder()
        .expect("session binder")
        .expect("non-null IWindowSession");
    assert_eq!(
        reply.data_avail(),
        0,
        "openSession reply must be consumed exactly"
    );
    session
}

/// The decoded `addToDisplayAsUser` reply, in the image's field order.
struct AddReply {
    result: i32,
    channel_name: Option<String>,
    channel_fd: Option<i32>,
    channel_token: Option<BinderId>,
    insets: InsetsState,
    controls: InsetsSourceControlArray,
    attached_frame: RectI,
    size_compat_scale: Vec<f32>,
}

fn read_add_reply(reply: &mut Parcel) -> AddReply {
    let result = reply.read_int32().expect("result");
    assert_eq!(
        reply.read_int32().expect("InputChannel present"),
        1,
        "the app needs a real InputChannel to install its InputEventReceiver"
    );
    assert_eq!(
        reply.read_int32().expect("InputChannel isInitialized"),
        1,
        "nativeReadFromParcel reads the initialized flag before the channel body"
    );
    let channel_token = reply.read_strong_binder().expect("channel token");
    let channel_name = reply.read_string16().expect("channel name");
    let channel_fd = reply.read_file_descriptor().expect("channel fd");
    assert_eq!(reply.read_int32().expect("InsetsState present"), 1);
    let insets = InsetsState::read_from_parcel(reply).expect("InsetsState");
    assert_eq!(
        reply.read_int32().expect("InsetsSourceControl.Array present"),
        1
    );
    let controls = InsetsSourceControlArray::read_from_parcel(reply).expect("controls");
    assert_eq!(reply.read_int32().expect("attachedFrame present"), 1);
    let attached_frame = RectI::read_from_parcel(reply).expect("attachedFrame");
    let size_compat_scale = reply
        .read_float_array()
        .expect("sizeCompatScale")
        .unwrap_or_default();
    AddReply {
        result,
        channel_name,
        channel_fd,
        channel_token,
        insets,
        controls,
        attached_frame,
        size_compat_scale,
    }
}

/// The decoded `relayout` reply, in the image's field order.
struct RelayoutReply {
    result: i32,
    frames: ClientWindowFrames,
    merged_configuration: MergedConfiguration,
    surface: SurfaceControlInfo,
    insets: InsetsState,
    controls: InsetsSourceControlArray,
    bundle_present: i32,
}

fn read_relayout_reply(reply: &mut Parcel) -> RelayoutReply {
    let result = reply.read_int32().expect("result");
    assert_eq!(reply.read_int32().expect("ClientWindowFrames present"), 1);
    let frames = ClientWindowFrames::read_from_parcel(reply).expect("ClientWindowFrames");
    assert_eq!(reply.read_int32().expect("MergedConfiguration present"), 1);
    let merged_configuration =
        MergedConfiguration::read_from_parcel(reply).expect("MergedConfiguration");
    assert_eq!(reply.read_int32().expect("SurfaceControl present"), 1);
    let surface = SurfaceControlInfo::read_from_parcel(reply).expect("SurfaceControl");
    assert_eq!(reply.read_int32().expect("InsetsState present"), 1);
    let insets = InsetsState::read_from_parcel(reply).expect("InsetsState");
    assert_eq!(
        reply.read_int32().expect("InsetsSourceControl.Array present"),
        1
    );
    let controls = InsetsSourceControlArray::read_from_parcel(reply).expect("controls");
    let bundle_present = reply.read_int32().expect("out Bundle");
    RelayoutReply {
        result,
        frames,
        merged_configuration,
        surface,
        insets,
        controls,
        bundle_present,
    }
}

#[test]
fn open_session_answers_a_routable_iwindow_session_node() {
    let mut host = ServiceHost::standard();
    let session = open_session(&mut host);
    let registered = host
        .binder_for_descriptor(IWINDOW_SESSION)
        .expect("IWindowSession node");
    assert_eq!(
        session, registered,
        "openSession must answer the host's registered IWindowSession node, not an id the \
         dispatcher does not know"
    );
    // Routine check: the descriptor really is the session's (the dispatcher
    // would otherwise serve it from another table).
    assert_eq!(
        host.descriptor_of(session),
        Some(IWINDOW_SESSION),
        "the session binder must carry the framework descriptor"
    );
    // A second openSession allocates a new session record but answers the same
    // single-process node.
    assert_eq!(open_session(&mut host), session);
}

#[test]
fn add_to_display_as_user_reply_is_api_34_exact() {
    let mut host = ServiceHost::standard();
    let session = open_session(&mut host);
    let window = BinderId(0x5701);
    let mut data = add_to_display_as_user_request(window);
    let mut reply = transact(&mut host, session, ADD_TO_DISPLAY_AS_USER, 0, &mut data)
        .expect("addToDisplayAsUser");
    no_exception(&mut reply);

    // The parcelable tail is compared byte-for-byte: remember where it starts
    // (right after the InputChannel's fd object) and re-encode it.
    let tail_start = {
        let mut probe = reply.clone();
        probe.read_int32().expect("result");
        assert_eq!(probe.read_int32().expect("channel present"), 1);
        assert_eq!(probe.read_int32().expect("channel initialized"), 1);
        probe.read_strong_binder().expect("token");
        probe.read_string16().expect("name");
        probe.read_file_descriptor().expect("fd");
        probe.data_position()
    };
    let add = read_add_reply(&mut reply);
    assert_eq!(
        reply.data_avail(),
        0,
        "the reply must be consumed exactly by the framework's readers"
    );
    assert_eq!(add.result, ADD_OKAY);
    let channel_name = add.channel_name.clone().expect("channel name");
    assert!(
        channel_name.contains("dev.acos.fixtures.helloviews/MainActivity"),
        "the channel name carries the framework's `package/window` form, was {channel_name}"
    );
    assert!(
        channel_name.ends_with("client"),
        "the app's end of the channel is the `client` side, was {channel_name}"
    );
    assert!(
        add.channel_fd.is_some(),
        "the InputChannel must carry a real descriptor"
    );
    assert!(add.channel_token.is_some(), "the channel carries a binder token");
    // The reference device: 1080x2400, density 420, status bar 128, nav 63.
    assert_eq!(add.attached_frame, RectI::new(0, 128, 1080, 2337));
    assert_eq!(
        add.insets
            .visible_insets_for(acos_wm::codec::inset_type::STATUS_BAR)
            .top,
        128
    );
    assert_eq!(
        add.insets
            .visible_insets_for(acos_wm::codec::inset_type::NAVIGATION_BAR)
            .bottom,
        63
    );
    assert!(
        !add.controls.0.is_empty(),
        "the app must receive the status/nav InsetsSourceControls"
    );
    assert_eq!(
        add.size_compat_scale,
        vec![1.0, 0.0],
        "the reply carries exactly the client's array length (2 here), holding the policy's \
         [1.0] and zero padding — `Parcel.readFloatArray` throws on any other count"
    );

    // Byte-exact re-encode of the parcelable tail.
    let mut again = Parcel::new();
    again.write_int32(1);
    add.insets.write_to_parcel(&mut again).expect("insets");
    again.write_int32(1);
    add.controls.write_to_parcel(&mut again).expect("controls");
    again.write_int32(1);
    add.attached_frame.write_to_parcel(&mut again).expect("rect");
    again
        .write_float_array(Some(&add.size_compat_scale))
        .expect("scale");
    assert_eq!(
        again.marshall(),
        reply.marshall()[tail_start..],
        "the typed out-parameters must be exactly what the framework writes"
    );
}

#[test]
fn relayout_reply_is_api_34_exact_and_routable() {
    let mut host = ServiceHost::standard();
    let session = open_session(&mut host);
    let window = BinderId(0x5702);
    let mut data = add_to_display_as_user_request(window);
    let mut add = transact(&mut host, session, ADD_TO_DISPLAY_AS_USER, 0, &mut data)
        .expect("addToDisplayAsUser");
    no_exception(&mut add);
    let added = read_add_reply(&mut add);

    let mut data = relayout_request(window, Some(&app_params()));
    let mut reply = transact(&mut host, session, RELAYOUT, 0, &mut data).expect("relayout");
    no_exception(&mut reply);
    let r = read_relayout_reply(&mut reply);
    assert_eq!(
        reply.data_avail(),
        0,
        "relayout's reply must be consumed exactly (frames, configuration, surface, insets, \
         controls, bundle)"
    );
    assert_ne!(r.result & acos_wm::wms::RELAYOUT_RES_FIRST_TIME, 0);
    assert_eq!(r.frames.frame, added.attached_frame);
    assert_eq!(r.frames.frame, RectI::new(0, 128, 1080, 2337));
    assert_eq!(r.insets.visible_insets_for(acos_wm::codec::inset_type::STATUS_BAR).top, 128);
    assert_eq!(
        r.insets
            .visible_insets_for(acos_wm::codec::inset_type::NAVIGATION_BAR)
            .bottom,
        63
    );
    assert_eq!(
        r.bundle_present, 0,
        "the out Bundle (SyncSeqId) is null: ACOS has no surface sync group"
    );
    // The SurfaceControl is a *structural* placeholder: the real shape, with a
    // layer handle that is not bound to a SurfaceFlinger layer (W1.md §3).
    assert_eq!(r.surface.width, 1080);
    assert_eq!(r.surface.height, 2209);

    // Whole-reply byte-exact re-encode (this reply carries no descriptor). The
    // exception header is part of the reply the framework reads first.
    let mut again = Parcel::new();
    again.write_int32(NO_EXCEPTION);
    again.write_int32(r.result);
    again.write_int32(1);
    r.frames.write_to_parcel(&mut again).expect("frames");
    again.write_int32(1);
    r.merged_configuration
        .write_to_parcel(&mut again)
        .expect("configuration");
    again.write_int32(1);
    r.surface.write_to_parcel(&mut again).expect("surface");
    again.write_int32(1);
    r.insets.write_to_parcel(&mut again).expect("insets");
    again.write_int32(1);
    r.controls.write_to_parcel(&mut again).expect("controls");
    again.write_int32(r.bundle_present);
    assert_eq!(
        again.marshall(),
        reply.marshall(),
        "the relayout reply must be exactly what the framework's proxy reads"
    );

    // A second relayout without attrs (`ViewRootImpl` sends null when nothing
    // changed) still answers: the WMS keeps the stored params.
    let mut data = relayout_request(window, None);
    let mut reply = transact(&mut host, session, RELAYOUT, 0, &mut data).expect("relayout(null)");
    no_exception(&mut reply);
    let second = read_relayout_reply(&mut reply);
    assert_eq!(reply.data_avail(), 0);
    assert_eq!(second.frames.frame, r.frames.frame);
    assert_eq!(second.result & acos_wm::wms::RELAYOUT_RES_FIRST_TIME, 0);
}

#[test]
fn the_traversal_calls_after_relayout_are_served_in_order() {
    let mut host = ServiceHost::standard();
    let session = open_session(&mut host);
    let window = BinderId(0x5703);
    let mut data = add_to_display_as_user_request(window);
    let mut reply = transact(&mut host, session, ADD_TO_DISPLAY_AS_USER, 0, &mut data)
        .expect("addToDisplayAsUser");
    no_exception(&mut reply);
    let _ = read_add_reply(&mut reply);

    // `getWindowId` (26): a routable `IWindowId` binder, not a fabricated id.
    let mut reply = transact(&mut host, session, GET_WINDOW_ID, 0, &mut ibinder_request(window))
        .expect("getWindowId");
    no_exception(&mut reply);
    let window_id = reply
        .read_strong_binder()
        .expect("IWindowId")
        .expect("non-null IWindowId");
    assert_eq!(reply.data_avail(), 0);
    assert_eq!(
        host.descriptor_of(window_id),
        Some("android.view.IWindowId"),
        "the IWindowId must be a registered node so a later transaction on it is routed"
    );

    // `finishDrawing` (9): typed SurfaceControl.Transaction + seq; oneway.
    let mut data = Parcel::new();
    data.write_interface_token(IWINDOW_SESSION).expect("token");
    data.write_strong_binder_opt(Some(window));
    data.write_int32(0); // null postDrawTransaction
    data.write_int32(7); // seqId
    data.set_data_position(0);
    let reply = transact(&mut host, session, FINISH_DRAWING, ONEWAY, &mut data)
        .expect("finishDrawing");
    assert_eq!(reply.data_avail(), 0, "a oneway reply carries no body");

    // `pokeDrawLock` (27) is synchronous and answers an exception header only.
    let mut reply =
        transact(&mut host, session, POKE_DRAW_LOCK, 0, &mut ibinder_request(window))
            .expect("pokeDrawLock");
    no_exception(&mut reply);
    assert_eq!(reply.data_avail(), 0);

    // `updateRequestedVisibleTypes` (32), oneway.
    let mut data = session_data();
    data.write_strong_binder_opt(Some(window));
    data.write_int32(0x7);
    data.set_data_position(0);
    transact(&mut host, session, UPDATE_REQUESTED_VISIBLE_TYPES, ONEWAY, &mut data)
        .expect("updateRequestedVisibleTypes");

    // `reportSystemGestureExclusionChanged` (33): typed list of Rects, oneway.
    let mut data = session_data();
    data.write_strong_binder_opt(Some(window));
    data.write_int32(1); // one Rect
    data.write_int32(1);
    RectI::new(0, 2000, 1080, 2337)
        .write_to_parcel(&mut data)
        .expect("rect");
    data.set_data_position(0);
    transact(
        &mut host,
        session,
        REPORT_SYSTEM_GESTURE_EXCLUSION_CHANGED,
        ONEWAY,
        &mut data,
    )
    .expect("reportSystemGestureExclusionChanged");

    // `setInsets` (8): IWindow, touchableInsets, two typed Rects, Region.
    let mut data = session_data();
    data.write_strong_binder_opt(Some(window));
    data.write_int32(0); // touchableInsets
    data.write_int32(1);
    RectI::new(0, 0, 1080, 2400)
        .write_to_parcel(&mut data)
        .expect("content insets");
    data.write_int32(1);
    RectI::new(0, 0, 1080, 2400)
        .write_to_parcel(&mut data)
        .expect("visible insets");
    data.write_int32(0); // null Region
    data.set_data_position(0);
    transact(&mut host, session, SET_INSETS, ONEWAY, &mut data).expect("setInsets");

    // `onRectangleOnScreenRequested` (25), oneway: IBinder + typed Rect.
    let mut data = session_data();
    data.write_strong_binder_opt(Some(window));
    data.write_int32(1);
    RectI::new(0, 0, 1080, 100)
        .write_to_parcel(&mut data)
        .expect("rect");
    data.set_data_position(0);
    transact(
        &mut host,
        session,
        ON_RECTANGLE_ON_SCREEN_REQUESTED,
        ONEWAY,
        &mut data,
    )
    .expect("onRectangleOnScreenRequested");

    // Boolean answers the framework reads.
    let mut reply = transact(&mut host, session, CANCEL_DRAW, 0, &mut ibinder_request(window))
        .expect("cancelDraw");
    no_exception(&mut reply);
    assert!(!reply.read_bool().expect("cancelDraw result"));
    assert_eq!(reply.data_avail(), 0);

    let mut reply =
        transact(&mut host, session, OUT_OF_MEMORY, 0, &mut binder_only_request(window))
            .expect("outOfMemory");
    no_exception(&mut reply);
    assert!(!reply.read_bool().expect("outOfMemory result"));
    assert_eq!(reply.data_avail(), 0);

    let mut data = Parcel::new();
    data.write_interface_token(IWINDOW_SESSION).expect("token");
    data.write_int32(0); // effectId
    data.write_bool(false); // always
    data.set_data_position(0);
    let mut reply = transact(
        &mut host,
        session,
        PERFORM_HAPTIC_FEEDBACK,
        0,
        &mut data,
    )
    .expect("performHapticFeedback");
    no_exception(&mut reply);
    assert!(!reply.read_bool().expect("haptic result"));
    assert_eq!(reply.data_avail(), 0);

    // State: one finished draw, no frame published by it (the ACOS rule).
    let state = host
        .ensure("android.view.IWindowManager")
        .expect("window service")
        .as_any_mut()
        .and_then(|any| any.downcast_mut::<WmsService>())
        .expect("WmsService")
        .shared_state();
    let state = state.lock().expect("wms state");
    assert_eq!(state.finish_drawing_count, 1);
    assert_eq!(
        state.frames_published, 0,
        "finishDrawing must never publish a frame by itself"
    );

    // `remove` (4) drops the window; a later relayout for it is a structured
    // error rather than a silent success.
    drop(state);
    let mut reply = transact(&mut host, session, REMOVE, 0, &mut binder_only_request(window))
        .expect("remove");
    no_exception(&mut reply);
    assert_eq!(reply.data_avail(), 0);
    let mut data = relayout_request(window, Some(&app_params()));
    let err = transact(&mut host, session, RELAYOUT, 0, &mut data)
        .expect_err("relayout of a removed window must fail");
    assert!(
        matches!(err, ServiceError::Internal(ref m) if m.contains("unknown window")),
        "unexpected error: {err:?}"
    );
}

#[test]
fn unknown_methods_are_structured_unsupported_naming_the_descriptor() {
    let mut host = ServiceHost::standard();
    let session = open_session(&mut host);

    // A code above the interface's table.
    let mut data = ibinder_request(BinderId(0x42));
    let err = transact(&mut host, session, 999, 0, &mut data).expect_err("code 999");
    match err {
        ServiceError::UnsupportedAndroidApi {
            interface,
            code,
            method,
        } => {
            assert_eq!(interface, "android.view.IWindowSession");
            assert_eq!(code, 999);
            assert!(method.contains("999"), "method was {method}");
        }
        other => panic!("expected UNSUPPORTED_ANDROID_API, got {other:?}"),
    }

    // A method that exists on the interface but is not implemented here: the
    // error names the method, and the reply is a real exception parcel (the
    // dispatcher never leaves the caller waiting for a reply).
    let mut data = ibinder_request(BinderId(0x42));
    let err = transact(&mut host, session, 20, 0, &mut data).expect_err("setShouldZoomOutWallpaper");
    match &err {
        ServiceError::UnsupportedAndroidApi {
            interface, method, ..
        } => {
            assert_eq!(interface, "android.view.IWindowSession");
            assert!(
                method == "setShouldZoomOutWallpaper" || method == "code 20",
                "method was {method}"
            );
        }
        other => panic!("expected UNSUPPORTED_ANDROID_API, got {other:?}"),
    }
    assert_eq!(err.code(), acos_services::codes::UNSUPPORTED_ANDROID_API);
}
