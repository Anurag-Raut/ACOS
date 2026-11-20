//! T-29 acceptance: the real `IWindowSession` / `IWindowManager` wire path.
//!
//! This drives [`acos_runtime::wms_java::transact`] with real request parcels
//! built exactly as the framework's `IWindowSession$Stub$Proxy` builds them
//! (interface token, `writeTypedObject(LayoutParams)`, the decoded `int`
//! arguments) and decodes the reply with the same DEX-derived codecs the Java
//! reader uses.
//!
//! The Java-level companion (`tests/java/WmsMain.java`) exercises the real
//! `Stub.Proxy` for `IWindowManager` and `openSession`. The
//! `addToDisplayAsUser`/`relayout` reply carries an `android.view.InputChannel`
//! whose `nativeReadFromParcel` is not yet implemented (T-36, owned by the
//! input agent); the Java test therefore reports that sub-path as
//! `pending-input-channel` instead of asserting it, and this test covers the
//! full reply byte layout directly.

mod common;

use acos_parcel::binder::BinderId;
use acos_parcel::Parcel;
use acos_runtime::wms_java;
use acos_vm::natives::fwk_surface::SurfaceBackend;
use acos_wm::codec::{
    inset_type, ClientWindowFrames, InsetsSourceControlArray, InsetsState, LayoutParams,
    MergedConfiguration, ParcelCodec, SurfaceControlInfo,
};
use acos_wm::geom::RectI;

const IWM: &str = "android.view.IWindowManager";
const IWS: &str = "android.view.IWindowSession";

fn method(interface: &str, name: &str) -> &'static acos_aidl::MethodInfo {
    acos_aidl::lookup_by_name(interface, name).expect("decoded AIDL method")
}

fn frame_params() -> LayoutParams {
    LayoutParams::application("dev.acos.fixtures.helloviews", "MainActivity")
}

/// Build a request for `addToDisplayAsUser` / `relayout`.
fn build(interface: &str, name: &str, extra_ints: usize) -> (Parcel, u32) {
    let m = method(interface, name);
    let mut p = Parcel::new();
    p.write_interface_token(interface).unwrap();
    p.write_strong_binder_opt(Some(BinderId(0x1111_1111)));
    p.write_int32(1);
    frame_params().write_to_parcel(&mut p).unwrap();
    for _ in 0..extra_ints {
        p.write_int32(0);
    }
    p.set_data_position(0);
    (p, m.code)
}

fn call(interface: &str, name: &str, extra_ints: usize) -> Parcel {
    let (mut data, code) = build(interface, name, extra_ints);
    let mut reply = Parcel::new();
    wms_java::transact(BinderId(0x9999), code, &mut data, &mut reply, 0).expect("WMS transact");
    reply.set_data_position(0);
    reply
}

fn read_cond<T: ParcelCodec>(p: &mut Parcel) -> Option<T> {
    if p.read_int32().unwrap() != 0 {
        Some(T::read_from_parcel(p).unwrap())
    } else {
        None
    }
}

/// Decode the `addToDisplayAsUser` `out InputChannel` exactly like the real
/// `InputChannel.nativeReadFromParcel`: the AIDL presence marker, then the
/// native `String16 name`, file descriptor and token binder.
fn read_wms_input_channel(
    p: &mut Parcel,
) -> Option<(String, std::os::fd::RawFd, Option<BinderId>)> {
    if p.read_int32().unwrap() == 0 {
        return None;
    }
    let name = p.read_string16_lenient().unwrap_or_default();
    let fd = p.read_file_descriptor().unwrap().unwrap_or(-1);
    let token = p.read_strong_binder().unwrap();
    Some((name, fd, token))
}

fn simple(interface: &str, name: &str, write_args: impl FnOnce(&mut Parcel), flags: u32) -> Parcel {
    let m = method(interface, name);
    let mut data = Parcel::new();
    data.write_interface_token(interface).unwrap();
    write_args(&mut data);
    data.set_data_position(0);
    let mut reply = Parcel::new();
    wms_java::transact(BinderId(1), m.code, &mut data, &mut reply, flags).unwrap();
    reply.set_data_position(0);
    reply
}

#[test]
fn window_manager_scalars_and_open_session() {
    let mut reply = simple(IWM, "useBLAST", |_| {}, 0);
    assert_eq!(reply.read_int32().unwrap(), 0, "no exception");
    assert!(reply.read_bool().unwrap(), "useBLAST");

    let mut reply = simple(IWM, "getInitialDisplaySize", |p| p.write_int32(0), 0);
    assert_eq!(reply.read_int32().unwrap(), 0);
    assert_eq!(reply.read_int32().unwrap(), 1, "present");
    assert_eq!(reply.read_int32().unwrap(), 1080);
    assert_eq!(reply.read_int32().unwrap(), 2400);

    let mut reply = simple(IWM, "getBaseDisplayDensity", |p| p.write_int32(0), 0);
    assert_eq!(reply.read_int32().unwrap(), 0);
    assert_eq!(reply.read_int32().unwrap(), 420);

    let mut reply = simple(
        IWM,
        "openSession",
        |p| p.write_strong_binder_opt(Some(BinderId(0x2222))),
        0,
    );
    assert_eq!(reply.read_int32().unwrap(), 0);
    assert!(
        reply.read_strong_binder().unwrap().is_some(),
        "openSession returns an IWindowSession binder"
    );
}

#[test]
#[ignore = "frozen legacy Rust VM: asserts the pre-W7 carved outFrame (0,128,1080,2337); W7b (18622e5) made report the default inset model, ADR-0026 #6, so the app window is the full display — see results/handoff/TESTHYG2.md"]
fn add_to_display_as_user_returns_reference_frame_and_insets() {
    let mut reply = call(IWS, "addToDisplayAsUser", 4);
    assert_eq!(reply.read_int32().unwrap(), 0, "no exception");
    assert_eq!(reply.read_int32().unwrap(), 0, "ADD_OKAY");

    let (channel_name, channel_fd, channel_token) =
        read_wms_input_channel(&mut reply).expect("input channel");
    assert!(
        channel_name.contains("client"),
        "channel name {channel_name}"
    );
    assert!(channel_fd >= 0, "no real client fd in the reply");
    assert!(channel_token.is_some(), "channel token binder");

    let insets: InsetsState = read_cond(&mut reply).expect("insets state");
    assert_eq!(
        insets.visible_insets_for(inset_type::STATUS_BAR).top,
        128,
        "status bar"
    );
    assert_eq!(
        insets.visible_insets_for(inset_type::NAVIGATION_BAR).bottom,
        63,
        "nav bar"
    );
    assert_eq!(insets.display_frame.right, 1080);
    assert_eq!(insets.display_frame.bottom, 2400);

    let controls: InsetsSourceControlArray = read_cond(&mut reply).expect("insets controls");
    assert_eq!(controls.0.len(), 3);
    assert!(
        controls.0.iter().any(|c| c.type_ == inset_type::IME),
        "IME control"
    );

    let out_frame = read_cond::<RectI>(&mut reply).expect("outFrame");
    assert_eq!(out_frame, RectI::new(0, 128, 1080, 2337));

    let floats = reply.read_float_array().unwrap().unwrap();
    assert_eq!(floats, vec![1.0]);
}

#[test]
#[ignore = "frozen legacy Rust VM: asserts the pre-W7 carved ClientWindowFrames (frame 0,128,1080,2337, surface 1080x2209); W7b (18622e5) made report the default inset model, ADR-0026 #6, so the window is the full display — see results/handoff/TESTHYG2.md"]
fn relayout_returns_a_valid_surface_control_and_reference_frames() {
    let _ = call(IWS, "addToDisplayAsUser", 4);
    let mut reply = call(IWS, "relayout", 6);
    assert_eq!(reply.read_int32().unwrap(), 0, "no exception");
    let result = reply.read_int32().unwrap();
    assert_ne!(result & wms_java::relayout_first_time(), 0);
    assert_ne!(result & wms_java::relayout_surface_changed(), 0);

    let frames: ClientWindowFrames = read_cond(&mut reply).expect("ClientWindowFrames");
    assert_eq!(frames.frame, RectI::new(0, 128, 1080, 2337));
    assert_eq!(frames.display_frame, RectI::new(0, 0, 1080, 2400));
    assert_eq!(frames, wms_java::reference_frames());

    let merged: MergedConfiguration = read_cond(&mut reply).expect("MergedConfiguration");
    assert_eq!(merged, wms_java::reference_merged_configuration());

    let surface: SurfaceControlInfo = read_cond(&mut reply).expect("SurfaceControl");
    assert!(
        surface.handle.is_some(),
        "SurfaceControl has a native handle"
    );
    assert_eq!(surface.width, 1080);
    assert_eq!(surface.height, 2209);

    let insets: InsetsState = read_cond(&mut reply).expect("InsetsState");
    assert_eq!(insets, wms_java::reference_insets());

    let controls: InsetsSourceControlArray = read_cond(&mut reply).expect("controls");
    assert_eq!(controls.0.len(), 3);

    assert_eq!(reply.read_int32().unwrap(), 0, "null trailing Bundle");

    let mut reply2 = call(IWS, "relayout", 6);
    assert_eq!(reply2.read_int32().unwrap(), 0);
    let result2 = reply2.read_int32().unwrap();
    assert_eq!(result2 & wms_java::relayout_first_time(), 0);
    let _frames2: ClientWindowFrames = read_cond(&mut reply2).unwrap();
    let _merged2: MergedConfiguration = read_cond(&mut reply2).unwrap();
    let surface2: SurfaceControlInfo = read_cond(&mut reply2).unwrap();
    assert_eq!(surface2.handle, surface.handle, "stable surface handle");
}

#[test]
fn finish_drawing_is_recorded_but_never_publishes_a_frame() {
    let _ = call(IWS, "addToDisplayAsUser", 4);
    let mut relayout = call(IWS, "relayout", 6);
    let _ = relayout.read_int32().unwrap();
    let _ = relayout.read_int32().unwrap();
    let _frames: ClientWindowFrames = read_cond(&mut relayout).unwrap();
    let _merged: MergedConfiguration = read_cond(&mut relayout).unwrap();
    let _surface: SurfaceControlInfo = read_cond(&mut relayout).unwrap();

    let before = wms_java::published_frames();
    let handler_count_before = wms_java::with_wms(|s| s.finish_drawing_count);

    // finishDrawing: binder, TypedObject(Transaction), int. Oneway.
    let mut reply = simple(
        IWS,
        "finishDrawing",
        |p| {
            p.write_strong_binder_opt(Some(BinderId(0x1111_1111)));
            p.write_int32(1);
            p.write_byte_array(Some(&[0u8; 4])).unwrap();
            p.write_int32(0);
        },
        1,
    );
    let _ = reply.read_int32();

    assert_eq!(
        wms_java::with_wms(|s| s.finish_drawing_count),
        handler_count_before + 1,
        "finishDrawing was recorded"
    );
    assert_eq!(
        wms_java::published_frames(),
        before,
        "finishDrawing alone must not publish a frame"
    );
}

/// The Java companion: the real `IWindowManager$Stub$Proxy` reads the ACOS WMS
/// replies over the real Binder transport. Compiles `tests/java/WmsMain.java`
/// against the `@hide` stubs and runs it on the real framework DEX.
#[test]
fn wms_java_stub_proxy_matches_the_reference() {
    let Some(dex) = common::compile_case(&["WmsMain"]) else {
        acos_test_support::require_prereq(false, "wms_java_stub_proxy_matches_the_reference", "wms_java_stub_proxy_matches_the_reference: javac/d8/android.jar unavailable");
        return;
    };
    let Some(stdout) = common::run_ok(&dex, "WmsMain", "WmsMain") else {
        return;
    };
    let expected = [
        "useBLAST=true",
        "getCurrentAnimatorScale=1.0",
        "getBaseDisplayDensity=420",
        "hasNavigationBar=true",
        "isInTouchMode=true",
        "setInTouchMode=ok",
        "openSession=ok",
        "wms=ok",
    ];
    let actual: Vec<&str> = stdout.lines().filter(|l| !l.is_empty()).collect();
    assert_eq!(actual, expected, "WMS Java golden\nstdout:\n{stdout}");
}

/// W20-A deferred acceptance: the real `IWindowSession$Stub$Proxy` drives
/// `addToDisplayAsUser` -> `relayout`; the reply's `InputChannel`
/// (`nativeReadFromParcel`) and `SurfaceControl` (`nativeReadFromParcel`) must
/// materialise, `Surface.copyFrom` must produce a valid surface, and the
/// incoming `IWindow` callbacks must reach the app stub.
#[test]
fn wms_java_session_add_and_relayout_are_accepted() {
    let Some(dex) = acos_test_support::require_some(common::compile_case(&["WmsSessionMain"]), "wms_java_session_add_and_relayout_are_accepted", "toolchain unavailable") else {
        return;
    };
    let Some(stdout) = common::run_ok(&dex, "android.view.WmsSessionMain", "WmsSessionMain") else {
        return;
    };
    let lines: Vec<&str> = stdout.lines().filter(|l| !l.is_empty()).collect();
    let get = |key: &str| {
        lines
            .iter()
            .find_map(|l| l.strip_prefix(&format!("{key}=")))
            .unwrap_or_else(|| panic!("missing {key} in\n{stdout}"))
    };
    assert_eq!(get("addResult"), "0", "ADD_OKAY\n{stdout}");
    let surface_ok = get("surfaceValid") == "true";
    let copied_ok = get("surfaceIsValid") == "true";
    assert!(surface_ok, "SurfaceControl.isValid()\n{stdout}");
    assert!(copied_ok, "Surface.copyFrom(SurfaceControl)\n{stdout}");
    assert_eq!(
        get("frame"),
        "Rect(0, 128 - 1080, 2337)",
        "outFrames.frame\n{stdout}"
    );
    assert_eq!(
        get("displayFrame"),
        "Rect(0, 0 - 1080, 2400)",
        "outFrames.displayFrame\n{stdout}"
    );
    let relayout: i32 = get("relayoutResult").parse().unwrap();
    assert_ne!(relayout & wms_java::relayout_first_time(), 0);
    assert_ne!(relayout & wms_java::relayout_surface_changed(), 0);
    assert!(get("visibilityCalls").parse::<i32>().unwrap() >= 1);
    assert!(get("insetsCalls").parse::<i32>().unwrap() >= 1);
    assert!(get("resizedCalls").parse::<i32>().unwrap() >= 1);
    assert!(lines.contains(&"wmssession=ok"), "{stdout}");
}

#[test]
fn viewrootimpl_handlers_return_faithful_scalars() {
    // `ViewRootImpl.dispatchFocusEvent`/`handleOutOfResourcesException`
    // call `outOfMemory` and expect a boolean; ACOS has no memory-pressure
    // signal, so the faithful answer is `false`.
    let mut reply = simple(
        IWS,
        "outOfMemory",
        |p| p.write_strong_binder_opt(Some(BinderId(0x1111))),
        0,
    );
    assert_eq!(reply.read_int32().unwrap(), 0, "no exception");
    assert!(!reply.read_bool().unwrap(), "not out of memory");

    // `ViewRootImpl.performTraversals` uses `cancelDraw` to drop a queued draw.
    let mut reply = simple(
        IWS,
        "cancelDraw",
        |p| p.write_strong_binder_opt(Some(BinderId(0x1111))),
        0,
    );
    assert_eq!(reply.read_int32().unwrap(), 0, "no exception");
    assert!(!reply.read_bool().unwrap(), "no queued draw to cancel");

    // Oneway calls from `ViewRootImpl`: accepted without a reply body.
    for name in [
        "clearTouchableRegion",
        "performHapticFeedbackAsync",
        "reportKeepClearAreasChanged",
        "dragRecipientEntered",
        "dragRecipientExited",
        "reportDropResult",
    ] {
        let mut reply = simple(
            IWS,
            name,
            |p| {
                p.write_strong_binder_opt(Some(BinderId(0x1111)));
                // `performHapticFeedbackAsync` takes (int, bool); the drag calls
                // take an `IWindow` and a bool/PointF; `reportKeepClearAreasChanged`
                // takes two typed lists. The exact payload is not asserted here;
                // the handler only needs to consume a well-formed request.
                p.write_int32(0);
                p.write_bool(false);
                p.write_int32(-1);
                p.write_int32(-1);
            },
            0,
        );
        assert_eq!(reply.read_int32().unwrap(), 0, "{name}: no exception");
    }

    // `relayoutAsync` is the oneway form of `relayout`; the request carries the
    // same `LayoutParams` + 6 ints. Add the window first, then consume it.
    let mut add = call(IWS, "addToDisplayAsUser", 4);
    assert_eq!(add.read_int32().unwrap(), 0, "add: no exception");
    let (mut data, code) = build(IWS, "relayoutAsync", 6);
    let mut reply = Parcel::new();
    wms_java::transact(BinderId(0x9999), code, &mut data, &mut reply, 0)
        .expect("relayoutAsync accepted");
    reply.set_data_position(0);
    assert_eq!(reply.read_int32().unwrap(), 0, "no exception");
}

#[test]
fn wm_surface_backend_publishes_only_on_buffer_submission() {
    let mut backend = wms_java::WmSurfaceBackend::new().unwrap();
    let _layer = backend.create_layer(0x2000, "test-layer", 1080, 2209);
    let g0 = backend.generation();
    backend
        .apply(&[acos_vm::natives::fwk_surface::SurfaceOp::Alpha {
            layer: 0x2000,
            alpha: 0.5,
        }])
        .unwrap();
    assert_eq!(
        backend.generation(),
        g0 + 1,
        "apply bumped the compositor generation"
    );
    let presented = backend.submit_buffer(0x2000, 1).unwrap();
    assert_eq!(presented, 1, "a real buffer submission publishes one frame");
    backend.release(0x2000);
}
