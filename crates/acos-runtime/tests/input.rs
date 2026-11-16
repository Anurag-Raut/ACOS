//! T-36 acceptance: the service input dispatcher over the real WMS channel.
//!
//! The service dispatcher ([`acos_runtime::input_java`]) is driven through the
//! real `IWindowSession.addToDisplayAsUser` wire path, so it is routed to the
//! exact window/`InputChannel` pair the framework would get. Injecting a tap
//! must write `fwk_input::InputFrame` records to the dispatcher end; the reader
//! in this test is the *client* end of the same socketpair, and the frame codec
//! is the one the real `InputEventReceiver` uses (proved separately by
//! `acos-vm/tests/input_natives.rs`).
//!
//! A tap outside every window must be a structured [`InputError`], never a
//! silent success.

use std::os::fd::FromRawFd;

use acos_parcel::binder::BinderId;
use acos_parcel::Parcel;
use acos_runtime::{input_java, wms_java};
use acos_services::input::InputError;
use acos_vm::natives::fwk_input::InputFrame;
use acos_wm::codec::{LayoutParams, ParcelCodec};

const IWS: &str = "android.view.IWindowSession";

fn method(name: &str) -> &'static acos_aidl::MethodInfo {
    acos_aidl::lookup_by_name(IWS, name).expect("decoded AIDL method")
}

/// Add a reference hello-views window over the real wire path and return the
/// reply `Parcel` (which owns the client descriptor) plus that descriptor.
fn add_window_and_get_client_fd() -> (Parcel, i32) {
    let m = method("addToDisplayAsUser");
    let mut p = Parcel::new();
    p.write_interface_token(IWS).unwrap();
    p.write_strong_binder_opt(Some(BinderId(0x1111_1111)));
    // `writeTypedObject(LayoutParams)` = presence marker + body.
    p.write_int32(1);
    LayoutParams::application("dev.acos.fixtures.helloviews", "MainActivity")
        .write_to_parcel(&mut p)
        .unwrap();
    for _ in 0..4 {
        p.write_int32(0);
    }
    p.set_data_position(0);
    let mut reply = Parcel::new();
    wms_java::transact(BinderId(0x9999), m.code, &mut p, &mut reply, 0).expect("WMS transact");
    reply.set_data_position(0);
    assert_eq!(reply.read_int32().unwrap(), 0, "no exception");
    assert_eq!(reply.read_int32().unwrap(), 0, "ADD_OKAY");
    assert_eq!(reply.read_int32().unwrap(), 1, "InputChannel present");
    let _name = reply.read_string16_lenient();
    let fd = reply.read_file_descriptor().unwrap().expect("client fd");
    (reply, fd)
}

/// Read one frame from the client end without taking ownership of the parcel's
/// descriptor.
fn read_frame(fd: i32) -> InputFrame {
    use std::io::Read;
    let mut file = unsafe { std::fs::File::from_raw_fd(fd) };
    let mut buf = [0u8; 4096];
    let n = file.read(&mut buf).expect("channel read");
    // The descriptor belongs to the reply `Parcel`; do not close it here.
    let _ = std::mem::ManuallyDrop::new(file);
    InputFrame::decode(&buf[..n]).expect("decode InputFrame")
}

#[test]
fn injected_tap_reaches_the_real_window_channel() {
    let (_reply, fd) = add_window_and_get_client_fd();
    let seqs = input_java::inject_tap(540.0, 427.0).expect("tap inside the window");
    assert_eq!(seqs, vec![1, 2]);

    let down = read_frame(fd);
    assert_eq!(down.action, 0, "ACTION_DOWN");
    assert_eq!(down.pointers[0].x, 540.0);
    assert_eq!(down.pointers[0].y, 427.0);

    let up = read_frame(fd);
    assert_eq!(up.action, 1, "ACTION_UP");
    assert_eq!(up.pointers[0].x, 540.0);
    assert_eq!(up.pointers[0].y, 427.0);

    // Ack accounting: one unacked event remains until both are finished.
    assert_eq!(input_java::pending_count(), 2);
    // REL3E: the app window's `finishInputEvent(handled)` bit is recorded, not
    // thrown away - an unhandled press must stay visible to the reply.
    let ack = wms_java::with_wms(|s| s.input.finish_event(1, false)).unwrap();
    assert_eq!((ack.seq, ack.handled), (1, false));
    let ack = wms_java::with_wms(|s| s.input.finish_event(2, true)).unwrap();
    assert_eq!((ack.seq, ack.handled), (2, true));
    assert_eq!(input_java::pending_count(), 0);
    let handling = session_handling(&[1, 2]);
    assert_eq!(handling, Some(false), "one unhandled event proves non-consumption");
}

/// The app window's answers folded the way the dispatcher reads them.
fn session_handling(seqs: &[u32]) -> Option<bool> {
    let acks = wms_java::with_wms(|s| {
        s.input
            .acks()
            .iter()
            .filter(|ack| seqs.contains(&ack.seq))
            .map(|ack| ack.handled)
            .collect::<Vec<_>>()
    });
    if acks.iter().any(|handled| !*handled) {
        Some(false)
    } else if !acks.is_empty() && acks.len() == seqs.len() {
        Some(true)
    } else {
        None
    }
}

#[test]
#[ignore = "frozen legacy Rust VM: a tap at (5,5) is inside the app window since W7b (18622e5) made report the default inset model (ADR-0026 #6): the window is the full display and the bars are only reported as insets — see results/handoff/TESTHYG2.md"]
fn tap_outside_every_window_is_a_structured_error() {
    let (_reply, _fd) = add_window_and_get_client_fd();
    let err = input_java::inject_tap(5.0, 5.0).unwrap_err();
    assert_eq!(err, InputError::NoWindowAtPoint { x: 5, y: 5 });
    assert_eq!(input_java::pending_count(), 0, "nothing may be queued");
}

#[test]
fn dispatcher_errors_when_no_window_is_registered() {
    // Reset this thread's WMS to a fresh state with no windows.
    wms_java::with_wms(|s| *s = acos_services::wms::WmsState::new());
    let err = input_java::inject_tap(540.0, 427.0).unwrap_err();
    assert_eq!(err, InputError::NoWindowAtPoint { x: 540, y: 427 });
}
