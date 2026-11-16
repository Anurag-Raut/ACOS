//! I2 acceptance: the session's one authoritative `InputChannel` is the I1
//! producer registry's pair, it carries the real FOCUS/TOUCH_MODE messages, and
//! injected motion/key bytes arrive at the client end with FINISHED acks.
//!
//! Teeth (each fails without the wiring):
//! * a wrong token never publishes and records a structured error;
//! * a closed channel surfaces `PeerClosed`, never a silent write;
//! * out-of-order FINISHED acks are all kept, with latency only for tracked
//!   sequences;
//! * `addToDisplayAsUser`'s written fd is the one carrying FOCUS — not
//!   `acos-wm`'s private channel.

use std::os::fd::OwnedFd;

use acos_parcel::{BinderId, Parcel};
use acos_services::exception::NO_EXCEPTION;
use acos_services::input_channel::{
    ChannelError, ConsumerResponse, InputMessage, LoopbackConsumer, ACTION_DOWN, ACTION_UP,
    KEY_ACTION_DOWN, KEY_ACTION_UP, KEYCODE_ENTER,
};
use acos_services::window_session::input_dispatch::{InputDispatch, InputEvent};
use acos_services::ServiceHost;
use acos_wm::codec::{LayoutParams, ParcelCodec};

const IWINDOW_MANAGER: &str = "android.view.IWindowManager";
const IWINDOW_SESSION: &str = "android.view.IWindowSession";

const OPEN_SESSION: u32 = 4;
const ADD_TO_DISPLAY_AS_USER: u32 = 2;

const TOKEN: BinderId = BinderId(0x7701);

fn registered_dispatch() -> (InputDispatch, LoopbackConsumer) {
    let mut d = InputDispatch::new();
    d.register_window(TOKEN, "Surface(name=acos-fixture/counter)/@0x10")
        .expect("register");
    // LAYER1: focus is published by the window session from the WMS layer
    // stack's `focus_target()` after the add (a FLAG_NOT_FOCUSABLE overlay must
    // not take focus); registration only publishes TOUCH_MODE.
    d.set_focus(Some(TOKEN)).expect("policy focus");
    let fd = d.duplicate_client_fd(TOKEN).expect("client fd");
    (d, LoopbackConsumer::new(fd))
}

fn next(consumer: &mut LoopbackConsumer) -> InputMessage {
    consumer.recv().expect("recv").expect("one message")
}

#[test]
fn registration_publishes_touch_mode_and_the_policy_focus_follows() {
    let (mut d, mut guest) = registered_dispatch();
    match next(&mut guest) {
        InputMessage::TouchMode {
            in_touch_mode: true,
            ..
        } => {}
        other => panic!("first message is TOUCH_MODE(true), got {other:?}"),
    }
    // W13: FOCUS is delivered for a newly focused window. The IME focus reply
    // (`startInputOrWindowGainedFocus`) is answered by `ime_wire`, so the
    // guest's main looper survives the framework's stock focus path. LAYER1:
    // the token is the WMS `focus_target()`, published after the add.
    match next(&mut guest) {
        InputMessage::Focus {
            has_focus: true, ..
        } => {}
        other => panic!("registration publishes FOCUS(true), got {other:?}"),
    }
    // The explicit API is still the same image body, e.g. for a lost focus.
    let seq = d.publish_focus(TOKEN, false).expect("focus");
    match next(&mut guest) {
        InputMessage::Focus {
            seq: got,
            has_focus: false,
            ..
        } => assert_eq!(got, seq),
        other => panic!("expected FOCUS(false), got {other:?}"),
    }
}

#[test]
fn inject_tap_publishes_down_then_up_and_records_the_acks_out_of_order() {
    let (mut d, mut guest) = registered_dispatch();
    // Drain the registration state messages: TOUCH_MODE then FOCUS(true).
    let _ = next(&mut guest);
    let _ = next(&mut guest);

    let gesture = d.inject_tap(TOKEN, 120.5, 340.25).expect("tap");
    assert_eq!(gesture.dispatches.len(), 2);
    assert_eq!(gesture.dispatches[0].kind, acos_services::input_channel::DispatchKind::MotionDown);
    assert_eq!(gesture.dispatches[1].kind, acos_services::input_channel::DispatchKind::MotionUp);

    match next(&mut guest) {
        InputMessage::Motion { seq, event } => {
            assert_eq!(seq, gesture.dispatches[0].seq);
            assert_eq!(event.action, ACTION_DOWN);
            // I2 measured that this image's MotionEvent path takes the point
            // from the window/raw transform (the pointer axes alone reach the
            // view as 0,0). Assert the real carrier, and that the axes are the
            // neutral origin so the transform is applied exactly once.
            assert_eq!(event.transform.tx, 120.5);
            assert_eq!(event.transform.ty, 340.25);
            assert_eq!(event.raw_transform.tx, 120.5);
            assert_eq!(event.raw_transform.ty, 340.25);
            assert_eq!(event.x(0), 0.0);
            assert_eq!(event.y(0), 0.0);
        }
        other => panic!("expected DOWN, got {other:?}"),
    }
    match next(&mut guest) {
        InputMessage::Motion { seq, event } => {
            assert_eq!(seq, gesture.dispatches[1].seq);
            assert_eq!(event.action, ACTION_UP);
        }
        other => panic!("expected UP, got {other:?}"),
    }

    // Acks out of order: UP first, then DOWN. Both must be kept.
    guest
        .send_finished(gesture.dispatches[1].seq, true)
        .expect("ack up");
    guest
        .send_finished(gesture.dispatches[0].seq, false)
        .expect("ack down");
    let responses = d.drain_acks(TOKEN).expect("acks");
    assert_eq!(responses.len(), 2);
    let acks: Vec<_> = responses
        .iter()
        .map(|r| match r {
            ConsumerResponse::Finished(a) => (a.seq, a.handled),
            other => panic!("not FINISHED: {other:?}"),
        })
        .collect();
    assert_eq!(acks, vec![(gesture.dispatches[1].seq, true), (gesture.dispatches[0].seq, false)]);
    assert!(d
        .events()
        .iter()
        .any(|e| matches!(e, InputEvent::Acked { ack, .. } if ack.seq == gesture.dispatches[0].seq)));
}

#[test]
fn inject_key_publishes_the_key_layout() {
    let (mut d, mut guest) = registered_dispatch();
    let _ = next(&mut guest);
    let _ = next(&mut guest);
    let g = d.inject_key(TOKEN, KEYCODE_ENTER).expect("key");
    match next(&mut guest) {
        InputMessage::Key { seq, event } => {
            assert_eq!(seq, g.dispatches[0].seq);
            assert_eq!(event.action, KEY_ACTION_DOWN);
            assert_eq!(event.key_code, KEYCODE_ENTER);
        }
        other => panic!("expected KEY DOWN, got {other:?}"),
    }
    match next(&mut guest) {
        InputMessage::Key { event, .. } => assert_eq!(event.action, KEY_ACTION_UP),
        other => panic!("expected KEY UP, got {other:?}"),
    }
}

#[test]
fn a_wrong_token_is_a_structured_error_and_publishes_nothing() {
    let (mut d, mut guest) = registered_dispatch();
    let _ = next(&mut guest);
    let _ = next(&mut guest);
    let err = d.inject_tap(BinderId(0xdead_beef), 1.0, 2.0).unwrap_err();
    assert!(matches!(err, ChannelError::Io(_)), "got {err:?}");
    assert!(d.events().iter().any(|e| matches!(
        e,
        InputEvent::Error { token, message }
            if token.0 == 0xdead_beef && message.contains("no input channel")
    )));
    // The live token's socket is untouched: nothing to read.
    assert!(guest.recv().expect("recv").is_none());
}

#[test]
fn a_closed_channel_is_peer_closed_not_a_silent_success() {
    let mut d = InputDispatch::new();
    d.register_window(TOKEN, "closed").expect("register");
    let guest = d.duplicate_client_fd(TOKEN).expect("client fd");
    let mut parcel = Parcel::new();
    d.write_and_forget(TOKEN, &mut parcel).expect("write");
    drop(parcel); // the reply's own dup must be gone too
    drop(guest); // the guest process is gone
    let err = d.inject_tap(TOKEN, 10.0, 20.0).expect_err("must fail");
    assert!(matches!(err, ChannelError::PeerClosed), "got {err:?}");
    assert!(d
        .events()
        .iter()
        .any(|e| matches!(e, InputEvent::Error { message, .. } if message.contains("peer is closed"))));
}

#[test]
fn remove_closes_the_guest_end() {
    let mut d = InputDispatch::new();
    d.register_window(TOKEN, "removed").expect("register");
    d.set_focus(Some(TOKEN)).expect("policy focus");
    let fd = d.duplicate_client_fd(TOKEN).expect("client fd");
    let mut guest = LoopbackConsumer::new(fd);
    let _ = next(&mut guest);
    let _ = next(&mut guest);
    assert!(d.remove(TOKEN));
    assert!(!d.is_registered(TOKEN));
    assert_eq!(
        guest.recv().expect_err("EOF after removal"),
        ChannelError::PeerClosed
    );
}

fn transact(host: &mut ServiceHost, binder: BinderId, code: u32, data: &mut Parcel) -> Parcel {
    let mut reply = Parcel::new();
    host.transact(binder, code, data, &mut reply, 0)
        .expect("transaction");
    reply.set_data_position(0);
    reply
}

#[test]
fn add_to_display_writes_the_producer_registry_fd_which_carries_focus() {
    let mut host = ServiceHost::standard();
    let window_manager = host
        .binder_for_descriptor(IWINDOW_MANAGER)
        .expect("window service");
    let mut open = Parcel::new();
    open.write_interface_token(IWINDOW_MANAGER).expect("token");
    open.write_strong_binder_opt(None);
    open.set_data_position(0);
    let mut reply = transact(&mut host, window_manager, OPEN_SESSION, &mut open);
    assert_eq!(reply.read_int32().expect("exception"), NO_EXCEPTION);
    let session = reply
        .read_strong_binder()
        .expect("session")
        .expect("non-null");

    let window = BinderId(0x7a01);
    let mut add = Parcel::new();
    add.write_interface_token(IWINDOW_SESSION).expect("token");
    add.write_strong_binder_opt(Some(window));
    add.write_int32(1);
    LayoutParams::application("dev.acos.fixtures.counter", "MainActivity")
        .write_to_parcel(&mut add)
        .expect("LayoutParams");
    add.write_int32(0); // viewVisibility
    add.write_int32(-1); // layerStackId
    add.write_int32(0); // userId
    add.write_int32(0); // requestedVisibleTypes
    add.write_int32(0); // out float[] length
    add.set_data_position(0);

    let mut reply = transact(&mut host, session, ADD_TO_DISPLAY_AS_USER, &mut add);
    assert_eq!(reply.read_int32().expect("exception"), NO_EXCEPTION);
    assert_eq!(reply.read_int32().expect("result"), 0, "ADD_OKAY");
    assert_eq!(reply.read_int32().expect("channel presence"), 1);
    assert_eq!(reply.read_int32().expect("isInitialized"), 1);
    let token = reply.read_strong_binder().expect("token").expect("non-null");
    assert_eq!(token, window, "the registry is keyed by the IWindow token");
    let name = reply.read_string16().expect("name").expect("name");
    assert!(name.contains("counter"), "name was {name}");
    let raw = reply
        .read_file_descriptor()
        .expect("fd")
        .expect("one authoritative fd");
    let borrowed = reply
        .borrowed_file_descriptor(raw)
        .expect("parcel owns the fd");
    let fd: OwnedFd = rustix::io::dup(borrowed).expect("dup reply fd");

    // The fd the guest installs on is the producer registry's client end and
    // already carries the real FOCUS(true)/TOUCH_MODE(true) state.
    let mut guest = LoopbackConsumer::new(fd);
    match next(&mut guest) {
        InputMessage::TouchMode {
            in_touch_mode: true,
            ..
        } => {}
        other => panic!("add reply fd did not carry TOUCH_MODE(true): {other:?}"),
    }
}

#[test]
fn text_is_encoded_as_the_us_keys_the_guest_will_resolve() {
    use acos_services::window_session::input_dispatch::{text_strokes, META_SHIFT_ON};
    assert_eq!(
        text_strokes("aA0) z").expect("ascii"),
        vec![
            (29, 0),
            (29, META_SHIFT_ON),
            (7, 0),
            (7, META_SHIFT_ON),
            (62, 0),
            (54, 0),
        ]
    );
    assert_eq!(text_strokes("\n\t\\\"?/").expect("ascii"), vec![
        (66, 0),
        (61, 0),
        (73, 0),
        (75, META_SHIFT_ON),
        (76, META_SHIFT_ON),
        (76, 0),
    ]);
    let err = text_strokes("é").expect_err("non-ASCII has no US key");
    assert!(format!("{err}").contains("ASCII-only"), "got {err}");
}

#[test]
fn inject_text_publishes_one_key_stroke_per_character_on_the_virtual_keyboard() {
    let (mut d, mut guest) = registered_dispatch();
    let _ = next(&mut guest); // TOUCH_MODE
    let _ = next(&mut guest); // FOCUS(true)
    let gesture = d.inject_text(TOKEN, "Hi").expect("text");
    assert_eq!(gesture.dispatches.len(), 4, "two keys, down+up each");
    for (index, (code, meta)) in [(36, 1), (37, 0)].into_iter().enumerate() {
        for action in [KEY_ACTION_DOWN, KEY_ACTION_UP] {
            match next(&mut guest) {
                InputMessage::Key { seq, event } => {
                    assert_eq!(seq, gesture.dispatches[index * 2 + usize::from(action == KEY_ACTION_UP)].seq);
                    assert_eq!(event.action, action);
                    assert_eq!(event.key_code, code);
                    assert_eq!(event.meta_state, meta);
                    assert_eq!(
                        event.device_id,
                        acos_services::window_session::input_dispatch::VIRTUAL_KEYBOARD_DEVICE_ID,
                        "text resolves through KeyCharacterMap.load(VIRTUAL_KEYBOARD)"
                    );
                }
                other => panic!("expected KEY, got {other:?}"),
            }
        }
    }
    for dispatch in &gesture.dispatches {
        guest.send_finished(dispatch.seq, true).expect("ack");
    }
    let responses = d.drain_acks(TOKEN).expect("acks");
    assert_eq!(responses.len(), 4);
    assert!(responses.iter().all(|r| matches!(
        r,
        ConsumerResponse::Finished(ack) if ack.handled
    )));
}

#[test]
fn a_character_with_no_key_fails_before_anything_is_published() {
    let (mut d, mut guest) = registered_dispatch();
    let _ = next(&mut guest); // TOUCH_MODE
    let _ = next(&mut guest); // FOCUS(true)
    let err = d.inject_text(TOKEN, "ok\u{1F600}").expect_err("emoji");
    assert!(format!("{err}").contains("ASCII-only"), "got {err}");
    assert!(
        guest.recv().expect("recv").is_none(),
        "a partially-encodable text must publish nothing at all"
    );
}
