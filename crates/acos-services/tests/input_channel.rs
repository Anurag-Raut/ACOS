//! I1 acceptance: the producer side of the real `InputChannel`.
//!
//! Every byte assertion below is built **by hand at the image's offsets** (see
//! the module docs and `InputTransport.h` in `android-14.0.0_r75`), not by
//! calling the encoder under test. The image's own `libinput.so` confirms the
//! sizes this file pins: `InputMessage::size()` is `0xa8 + 0x90 * n` for
//! `MOTION`, `0x68` for `KEY`, `0x18` for `FINISHED`, `0x20` for `TIMELINE`,
//! and `sizeof(InputMessage)` is `0x9a8`.
//!
//! The socketpair tests are a real loopback: the registry's server end
//! publishes, a consumer on the duplicated client end decodes and answers
//! `FINISHED`/`TIMELINE`, and the publisher reports `handled` plus latency.

use std::time::Duration;

use acos_parcel::{BinderId, Parcel};
use acos_services::input_channel::{
    monotonic_nanos, open_input_channel_pair, ChannelError, ConsumerResponse, DispatchKind,
    InputChannelRegistry, InputMessage, KeyEvent, LoopbackConsumer, MotionEvent, Pointer,
    PointerCoords, PointerProperties, ACTION_DOWN, ACTION_MOVE, ACTION_UP, AXIS_PRESSURE,
    AXIS_SIZE, AXIS_TOUCH_MAJOR, AXIS_TOUCH_MINOR, AXIS_X, AXIS_Y, DEFAULT_TAP_HOLD_NANOS,
    FINISHED_MESSAGE_SIZE, HEADER_SIZE, KEYCODE_ENTER, KEY_ACTION_DOWN, KEY_ACTION_UP,
    KEY_MESSAGE_SIZE, MAX_MESSAGE_SIZE, MAX_POINTERS, MOTION_FIXED_SIZE, POINTER_COORDS_SIZE,
    POINTER_SIZE, SOURCE_KEYBOARD, SOURCE_TOUCHSCREEN, TIMELINE_MESSAGE_SIZE, TYPE_FINISHED,
    TYPE_KEY, TYPE_MOTION, TYPE_TIMELINE,
};

const TOKEN: BinderId = BinderId(0x41_0000);

// ---------------------------------------------------------------------------
// hand-built wire bytes (independent of the encoder)
// ---------------------------------------------------------------------------

fn w32(buf: &mut [u8], off: usize, v: u32) {
    buf[off..off + 4].copy_from_slice(&v.to_le_bytes());
}

fn wi32(buf: &mut [u8], off: usize, v: i32) {
    w32(buf, off, v as u32);
}

fn wi64(buf: &mut [u8], off: usize, v: i64) {
    buf[off..off + 8].copy_from_slice(&v.to_le_bytes());
}

fn wf32(buf: &mut [u8], off: usize, v: f32) {
    w32(buf, off, v.to_bits());
}

/// One hand-built motion message: exactly `0xa8 + 0x90*n` bytes, one finger at
/// `(x, y)` with the six axes `PointerCoords::touch` sets.
fn golden_motion(
    seq: u32,
    action: i32,
    down_time: i64,
    event_time: i64,
    x: f32,
    y: f32,
) -> Vec<u8> {
    let mut b = vec![0u8; MAX_MESSAGE_SIZE];
    w32(&mut b, 0x00, TYPE_MOTION);
    w32(&mut b, 0x04, seq);
    wi32(&mut b, 0x08, 1); // eventId
    w32(&mut b, 0x0c, 1); // pointerCount
    wi64(&mut b, 0x10, event_time);
    wi32(&mut b, 0x18, 1); // deviceId
    wi32(&mut b, 0x1c, SOURCE_TOUCHSCREEN);
    wi32(&mut b, 0x20, 0); // displayId
                           // 0x24..0x44 hmac zeros
    wi32(&mut b, 0x44, action);
    // actionButton 0x48 / flags 0x4c / metaState 0x50 / buttonState 0x54 = 0
    b[0x58] = 0; // classification, 3 pad bytes stay zero
    wi32(&mut b, 0x5c, 0); // edgeFlags
    wi64(&mut b, 0x60, down_time);
    for (i, v) in [1.0f32, 0.0, 0.0, 1.0, 0.0, 0.0].iter().enumerate() {
        wf32(&mut b, 0x68 + i * 4, *v); // window transform (identity)
    }
    wf32(&mut b, 0x80, 1.0); // xPrecision
    wf32(&mut b, 0x84, 1.0); // yPrecision
    wf32(&mut b, 0x88, 0.0); // xCursorPosition
    wf32(&mut b, 0x8c, 0.0); // yCursorPosition
    for (i, v) in [1.0f32, 0.0, 0.0, 1.0, 0.0, 0.0].iter().enumerate() {
        wf32(&mut b, 0x90 + i * 4, *v); // raw transform (identity)
    }
    // pointer[0] at 0xa8: properties then PointerCoords (136 bytes)
    wi32(&mut b, 0xa8, 0); // id
    wi32(&mut b, 0xac, 1); // toolType = FINGER
    wi64(&mut b, 0xb0, 0b111111); // axes X,Y,PRESSURE,SIZE,TOUCH_MAJOR,TOUCH_MINOR
    for (i, v) in [x, y, 1.0f32, 0.05, 0.1, 0.1].iter().enumerate() {
        wf32(&mut b, 0xb8 + i * 4, *v);
    }
    // isResampled at pointer-coords offset 128 stays zero
    b.truncate(HEADER_SIZE + MOTION_FIXED_SIZE + POINTER_SIZE);
    b
}

fn golden_key(seq: u32, action: i32, key_code: i32, down_time: i64, event_time: i64) -> Vec<u8> {
    let mut b = vec![0u8; KEY_MESSAGE_SIZE];
    w32(&mut b, 0x00, TYPE_KEY);
    w32(&mut b, 0x04, seq);
    wi32(&mut b, 0x08, 1); // eventId
    wi64(&mut b, 0x10, event_time);
    wi32(&mut b, 0x18, 1); // deviceId
    wi32(&mut b, 0x1c, SOURCE_KEYBOARD);
    wi32(&mut b, 0x20, 0); // displayId
    wi32(&mut b, 0x44, action);
    wi32(&mut b, 0x4c, key_code);
    wi64(&mut b, 0x60, down_time);
    b
}

fn golden_finished(seq: u32, handled: bool, consume_time: i64) -> Vec<u8> {
    let mut b = vec![0u8; FINISHED_MESSAGE_SIZE];
    w32(&mut b, 0x00, TYPE_FINISHED);
    w32(&mut b, 0x04, seq);
    b[0x08] = u8::from(handled);
    wi64(&mut b, 0x10, consume_time);
    b
}

fn golden_timeline(seq: u32, event_id: i32, present: i64, frame: i64) -> Vec<u8> {
    let mut b = vec![0u8; TIMELINE_MESSAGE_SIZE];
    w32(&mut b, 0x00, TYPE_TIMELINE);
    w32(&mut b, 0x04, seq);
    wi32(&mut b, 0x08, event_id);
    wi64(&mut b, 0x10, present);
    wi64(&mut b, 0x18, frame);
    b
}

// ---------------------------------------------------------------------------
// sizes and exact bytes
// ---------------------------------------------------------------------------

#[test]
fn message_sizes_are_the_image_sizes() {
    // `InputMessage::size()` in the image's libinput.so.
    assert_eq!(acos_services::input_channel::motion_message_size(1), 0x138);
    assert_eq!(acos_services::input_channel::motion_message_size(2), 0x1c8);
    assert_eq!(
        acos_services::input_channel::motion_message_size(MAX_POINTERS),
        0x9a8
    );
    assert_eq!(MAX_MESSAGE_SIZE, 0x9a8);
    assert_eq!(KEY_MESSAGE_SIZE, 0x68);
    assert_eq!(FINISHED_MESSAGE_SIZE, 0x18);
    assert_eq!(TIMELINE_MESSAGE_SIZE, 0x20);
    // The pointer half of the layout: 8 property bytes + 136 coordinate bytes.
    assert_eq!(POINTER_SIZE, 0x90);
    assert_eq!(POINTER_COORDS_SIZE, 136);
    assert_eq!(HEADER_SIZE + MOTION_FIXED_SIZE + POINTER_SIZE, 0x138);
}

#[test]
fn motion_encodes_to_hand_built_image_bytes() {
    let down = 1_700_000_000_000_000_000i64;
    let event = MotionEvent::touch(1, ACTION_DOWN, down, down, 120.5, 240.25);
    let encoded = InputMessage::Motion { seq: 7, event }
        .encode()
        .expect("encode");
    let expected = golden_motion(7, ACTION_DOWN, down, down, 120.5, 240.25);
    assert_eq!(encoded.len(), expected.len());
    assert_eq!(
        encoded, expected,
        "motion bytes differ from the image layout"
    );
    // Spot-check the offsets a receiver would use.
    assert_eq!(
        u32::from_le_bytes(encoded[0..4].try_into().unwrap()),
        TYPE_MOTION
    );
    assert_eq!(u32::from_le_bytes(encoded[4..8].try_into().unwrap()), 7);
    assert_eq!(
        u32::from_le_bytes(encoded[0x0c..0x10].try_into().unwrap()),
        1
    );
    assert_eq!(
        i64::from_le_bytes(encoded[0x10..0x18].try_into().unwrap()),
        down
    );
    assert_eq!(
        i64::from_le_bytes(encoded[0x60..0x68].try_into().unwrap()),
        down
    );
    assert_eq!(
        f32::from_bits(u32::from_le_bytes(encoded[0xb8..0xbc].try_into().unwrap())),
        120.5
    );
}

#[test]
fn key_encodes_to_hand_built_image_bytes() {
    let down = 42_000_000_000i64;
    let event = KeyEvent::new(1, KEY_ACTION_DOWN, KEYCODE_ENTER, down, down);
    let encoded = InputMessage::Key { seq: 3, event }
        .encode()
        .expect("encode");
    assert_eq!(
        encoded,
        golden_key(3, KEY_ACTION_DOWN, KEYCODE_ENTER, down, down)
    );
}

#[test]
fn two_pointer_motion_uses_the_variable_pointer_array() {
    let down = 5_000_000_000i64;
    let mut event = MotionEvent::touch(1, ACTION_MOVE, down, down + 10, 10.0, 20.0);
    event.pointers.push(Pointer {
        properties: PointerProperties {
            id: 1,
            tool_type: 1,
        },
        coords: PointerCoords::with_axes(&[(AXIS_X, 30.0), (AXIS_Y, 40.0)]),
    });
    let encoded = InputMessage::Motion {
        seq: 9,
        event: event.clone(),
    }
    .encode()
    .expect("encode");
    assert_eq!(encoded.len(), 0x1c8);
    assert_eq!(
        u32::from_le_bytes(encoded[0x0c..0x10].try_into().unwrap()),
        2
    );
    // Second pointer starts at 0xa8 + 0x90.
    assert_eq!(
        i32::from_le_bytes(encoded[0x138..0x13c].try_into().unwrap()),
        1
    );
    let decoded = InputMessage::decode(&encoded).expect("decode");
    match decoded {
        InputMessage::Motion { seq, event } => {
            assert_eq!(seq, 9);
            assert_eq!(event.pointers.len(), 2);
            assert_eq!(event.x(1), 30.0);
            assert_eq!(event.y(1), 40.0);
            assert_eq!(event.pointers[1].coords.bits, 0b11);
        }
        other => panic!("expected motion, got {other:?}"),
    }
}

#[test]
fn encode_decode_round_trips_every_field() {
    let down = 1_000_000_000i64;
    let mut event = MotionEvent::touch(17, ACTION_MOVE, down, down + 16_000_000, 3.5, 4.5);
    event.flags = 0x2;
    event.edge_flags = 0x1;
    event.meta_state = 0x1000;
    event.button_state = 0x2;
    event.action_button = 0x1;
    event.classification = 2;
    event.x_cursor_position = 1.5;
    event.y_cursor_position = 2.5;
    event.hmac = [7; 32];
    let encoded = InputMessage::Motion { seq: 11, event }
        .encode()
        .expect("encode");
    match InputMessage::decode(&encoded).expect("decode") {
        InputMessage::Motion { seq, event } => {
            assert_eq!(seq, 11);
            assert_eq!(event.event_id, 17);
            assert_eq!(event.action, ACTION_MOVE);
            assert_eq!(event.flags, 0x2);
            assert_eq!(event.edge_flags, 0x1);
            assert_eq!(event.meta_state, 0x1000);
            assert_eq!(event.button_state, 0x2);
            assert_eq!(event.action_button, 0x1);
            assert_eq!(event.classification, 2);
            assert_eq!(event.x_cursor_position, 1.5);
            assert_eq!(event.y_cursor_position, 2.5);
            assert_eq!(event.hmac, [7; 32]);
            assert_eq!(event.down_time, down);
            assert_eq!(event.event_time, down + 16_000_000);
            assert_eq!(event.x(0), 3.5);
            assert_eq!(event.y(0), 4.5);
            assert_eq!(event.pointers[0].coords.get(AXIS_PRESSURE), 1.0);
            assert_eq!(event.pointers[0].coords.get(AXIS_SIZE), 0.05);
            assert_eq!(event.pointers[0].coords.get(AXIS_TOUCH_MAJOR), 0.1);
            assert_eq!(event.pointers[0].coords.get(AXIS_TOUCH_MINOR), 0.1);
        }
        other => panic!("expected motion, got {other:?}"),
    }

    let key = KeyEvent::new(3, KEY_ACTION_UP, KEYCODE_ENTER, down, down + 50_000_000);
    let encoded = InputMessage::Key {
        seq: 12,
        event: key,
    }
    .encode()
    .expect("encode");
    assert_eq!(encoded.len(), KEY_MESSAGE_SIZE);
    match InputMessage::decode(&encoded).expect("decode") {
        InputMessage::Key { seq, event } => {
            assert_eq!(seq, 12);
            assert_eq!(event.action, KEY_ACTION_UP);
            assert_eq!(event.key_code, KEYCODE_ENTER);
            assert_eq!(event.down_time, down);
            assert_eq!(event.event_time, down + 50_000_000);
        }
        other => panic!("expected key, got {other:?}"),
    }
}

#[test]
fn consumer_replies_decode_from_hand_built_bytes() {
    let finished = InputMessage::decode(&golden_finished(5, true, 12345)).expect("finished");
    assert_eq!(
        finished,
        InputMessage::Finished {
            seq: 5,
            handled: true,
            consume_time: 12345
        }
    );
    let timeline = InputMessage::decode(&golden_timeline(6, 9, 100, 200)).expect("timeline");
    assert_eq!(
        timeline,
        InputMessage::Timeline {
            seq: 6,
            event_id: 9,
            graphics_timeline: [100, 200]
        }
    );
}

#[test]
fn decode_rejects_bad_bodies() {
    let good = golden_motion(1, ACTION_DOWN, 1, 1, 0.0, 0.0);
    assert!(matches!(
        InputMessage::decode(&good[..good.len() - 1]),
        Err(ChannelError::Malformed(_))
    ));
    let mut zero_pointers = good.clone();
    w32(&mut zero_pointers, 0x0c, 0);
    assert!(matches!(
        InputMessage::decode(&zero_pointers),
        Err(ChannelError::BadPointerCount(0))
    ));
    let mut too_many = good.clone();
    w32(&mut too_many, 0x0c, (MAX_POINTERS + 1) as u32);
    assert!(matches!(
        InputMessage::decode(&too_many),
        Err(ChannelError::Malformed(_)) | Err(ChannelError::BadPointerCount(_))
    ));
    let mut bad_type = good.clone();
    w32(&mut bad_type, 0x00, 99);
    assert!(matches!(
        InputMessage::decode(&bad_type),
        Err(ChannelError::UnsupportedType(99))
    ));
    assert!(matches!(
        InputMessage::decode(&[]),
        Err(ChannelError::Malformed(_))
    ));
}

// ---------------------------------------------------------------------------
// socketpair + publisher/consumer loopback
// ---------------------------------------------------------------------------

fn new_loopback() -> (InputChannelRegistry, LoopbackConsumer) {
    let mut registry = InputChannelRegistry::new();
    registry
        .register(TOKEN, "Surface(name=com.acos.counter/CounterActivity)")
        .expect("register");
    let consumer = LoopbackConsumer::new(registry.duplicate_client_fd(TOKEN).expect("client fd"));
    (registry, consumer)
}

fn recv_motion(consumer: &mut LoopbackConsumer) -> (u32, MotionEvent) {
    match consumer.recv().expect("recv").expect("a message") {
        InputMessage::Motion { seq, event } => (seq, event),
        other => panic!("expected motion, got {other:?}"),
    }
}

#[test]
fn open_pair_creates_a_real_seqpacket_channel() {
    let pair = open_input_channel_pair("w").expect("pair");
    assert_eq!(pair.publisher.name(), "w (server)");
    assert!(pair.client_fd.is_some());
    // The pair is usable: one message written by the server is readable on the
    // client end and keeps its boundary.
    let mut publisher = pair.publisher;
    let mut consumer = LoopbackConsumer::new(pair.client_fd.expect("client fd"));
    let down = monotonic_nanos();
    let gesture = publisher
        .tap_at(1.0, 2.0, down, DEFAULT_TAP_HOLD_NANOS)
        .expect("tap");
    assert_eq!(gesture.dispatches.len(), 2);
    let (seq, event) = recv_motion(&mut consumer);
    assert_eq!(seq, 1);
    assert_eq!(event.action, ACTION_DOWN);
    assert_eq!(event.down_time, down);
    assert_eq!(event.event_time, down);
}

#[test]
fn tap_publishes_down_then_up_with_shared_downtime_and_both_acks() {
    let (mut registry, mut consumer) = new_loopback();
    let down = 2_000_000_000i64;

    let gesture = registry
        .publisher_mut(TOKEN)
        .expect("publisher")
        .tap_at(100.0, 200.0, down, 50_000_000)
        .expect("tap");
    assert_eq!(gesture.dispatches.len(), 2);
    assert_eq!(gesture.dispatches[0].kind, DispatchKind::MotionDown);
    assert_eq!(gesture.dispatches[1].kind, DispatchKind::MotionUp);
    assert_eq!(gesture.dispatches[0].seq, 1);
    assert_eq!(gesture.dispatches[1].seq, 2);

    let (seq1, down_event) = recv_motion(&mut consumer);
    let (seq2, up_event) = recv_motion(&mut consumer);
    assert_eq!((seq1, seq2), (1, 2));
    assert_eq!(down_event.action, ACTION_DOWN);
    assert_eq!(up_event.action, ACTION_UP);
    assert_eq!(down_event.event_time, down);
    assert_eq!(up_event.event_time, down + 50_000_000);
    assert_eq!(down_event.down_time, down);
    assert_eq!(
        up_event.down_time, down,
        "UP must carry the stream downTime"
    );
    assert_eq!(down_event.x(0), 100.0);
    assert_eq!(up_event.y(0), 200.0);

    // The consumer acks in order; the publisher maps each ack back to its
    // dispatch and records the latency.
    let consumed = monotonic_nanos();
    consumer
        .send_finished_at(seq1, true, consumed)
        .expect("finished 1");
    consumer
        .send_finished_at(seq2, false, consumed)
        .expect("finished 2");
    let publisher = registry.publisher_mut(TOKEN).expect("publisher");
    let first = publisher
        .receive_consumer_response()
        .expect("response")
        .expect("finished");
    let second = publisher
        .receive_consumer_response()
        .expect("response")
        .expect("finished");
    match (first, second) {
        (ConsumerResponse::Finished(a), ConsumerResponse::Finished(b)) => {
            assert_eq!(a.seq, 1);
            assert!(a.handled);
            assert_eq!(a.consume_time, consumed);
            assert!(a.latency.is_some(), "tracked sequence must report latency");
            assert_eq!(b.seq, 2);
            assert!(!b.handled, "handled=false must be reported verbatim");
            assert!(b.latency.is_some());
        }
        other => panic!("expected two finished replies, got {other:?}"),
    }
    assert_eq!(publisher.pending_len(), 0);
    assert_eq!(publisher.acks().len(), 2);
    assert_eq!(
        publisher
            .receive_consumer_response()
            .expect("empty channel"),
        None
    );
}

#[test]
fn acks_may_arrive_out_of_order() {
    let (mut registry, mut consumer) = new_loopback();
    let down = monotonic_nanos();
    let gesture = registry
        .publisher_mut(TOKEN)
        .expect("publisher")
        .tap_at(10.0, 20.0, down, DEFAULT_TAP_HOLD_NANOS)
        .expect("tap");
    assert_eq!(gesture.dispatches[1].seq, 2);
    recv_motion(&mut consumer);
    recv_motion(&mut consumer);

    consumer.send_finished(2, true).expect("finished 2 first");
    let publisher = registry.publisher_mut(TOKEN).expect("publisher");
    let first_ack = publisher
        .receive_consumer_response()
        .expect("response")
        .expect("finished");
    match first_ack {
        ConsumerResponse::Finished(ack) => {
            assert_eq!(ack.seq, 2);
            assert!(ack.latency.is_some());
        }
        other => panic!("expected finished, got {other:?}"),
    }
    // Seq 1 is still outstanding: an out-of-order ack must not clear it.
    assert_eq!(publisher.pending_seqs().collect::<Vec<_>>(), vec![1]);
    consumer.send_finished(1, true).expect("finished 1");
    assert!(matches!(
        publisher
            .receive_consumer_response()
            .expect("response")
            .expect("finished"),
        ConsumerResponse::Finished(acos_services::input_channel::Ack { seq: 1, .. })
    ));
    assert_eq!(publisher.pending_len(), 0);
}

#[test]
fn untracked_finished_is_returned_without_latency() {
    let (mut registry, mut consumer) = new_loopback();
    consumer.send_finished(77, true).expect("finished");
    let publisher = registry.publisher_mut(TOKEN).expect("publisher");
    match publisher
        .receive_consumer_response()
        .expect("response")
        .expect("finished")
    {
        ConsumerResponse::Finished(ack) => {
            assert_eq!(ack.seq, 77);
            assert!(ack.handled);
            assert_eq!(ack.latency, None);
        }
        other => panic!("expected finished, got {other:?}"),
    }
}

#[test]
fn key_press_publishes_down_and_up_in_the_key_layout() {
    let (mut registry, mut consumer) = new_loopback();
    let down = 7_000_000_000i64;
    let gesture = registry
        .publisher_mut(TOKEN)
        .expect("publisher")
        .key_at(KEYCODE_ENTER, down, 30_000_000)
        .expect("key");

    let first = consumer.recv().expect("recv").expect("key down");
    let second = consumer.recv().expect("recv").expect("key up");
    for (message, action, event_time) in [
        (first, KEY_ACTION_DOWN, down),
        (second, KEY_ACTION_UP, down + 30_000_000),
    ] {
        match message {
            InputMessage::Key { seq, event } => {
                assert_eq!(seq, gesture.dispatches[action as usize].seq);
                assert_eq!(event.action, action);
                assert_eq!(event.key_code, KEYCODE_ENTER);
                assert_eq!(event.down_time, down);
                assert_eq!(event.event_time, event_time);
                assert_eq!(event.source, SOURCE_KEYBOARD);
            }
            other => panic!("expected key, got {other:?}"),
        }
    }
    // The bytes are the image's key size and offsets.
    let encoded = InputMessage::Key {
        seq: 1,
        event: KeyEvent::new(1, KEY_ACTION_DOWN, KEYCODE_ENTER, down, down),
    }
    .encode()
    .expect("encode");
    assert_eq!(encoded.len(), KEY_MESSAGE_SIZE);
    assert_eq!(
        encoded,
        golden_key(1, KEY_ACTION_DOWN, KEYCODE_ENTER, down, down)
    );
}

#[test]
fn sequences_and_event_ids_are_monotonic_across_gestures() {
    let (mut registry, mut consumer) = new_loopback();
    let publisher = registry.publisher_mut(TOKEN).expect("publisher");
    let t = 1_000_000_000;
    let _ = publisher.tap_at(1.0, 1.0, t, 20_000_000).expect("tap 1");
    let _ = publisher
        .tap_at(2.0, 2.0, t + 40_000_000, 20_000_000)
        .expect("tap 2");
    let _ = publisher
        .key_at(KEYCODE_ENTER, t + 80_000_000, 20_000_000)
        .expect("key");
    let _ = publisher
        .gesture(&[(0.0, 0.0), (5.0, 5.0), (10.0, 10.0)], 10_000_000)
        .expect("gesture");
    // tap(2) + tap(2) + key(2) + gesture(DOWN, MOVE, MOVE, UP = 4) = 10
    let expected_seqs: Vec<u32> = (1..=10).collect();
    let seen_seqs: Vec<u32> = (0..10)
        .map(|_| consumer.recv().expect("recv").expect("message").seq())
        .collect();
    assert_eq!(seen_seqs, expected_seqs);
    assert_eq!(publisher.pending_len(), 10);
    assert_eq!(publisher.next_seq(), 11);
}

#[test]
fn gesture_is_configurable_and_shares_one_downtime() {
    let (mut registry, mut consumer) = new_loopback();
    let points = [(10.0, 10.0), (20.0, 20.0), (30.0, 30.0)];
    let dispatches = registry
        .publisher_mut(TOKEN)
        .expect("publisher")
        .gesture(&points, 8_000_000)
        .expect("gesture");
    assert_eq!(dispatches.len(), 4, "DOWN, MOVE, MOVE, UP");
    let down_time = dispatches[0].down_time;

    let mut events = Vec::new();
    for _ in 0..4 {
        events.push(recv_motion(&mut consumer).1);
    }
    let actions: Vec<i32> = events.iter().map(|e| e.action).collect();
    assert_eq!(
        actions,
        vec![ACTION_DOWN, ACTION_MOVE, ACTION_MOVE, ACTION_UP]
    );
    for event in &events {
        assert_eq!(event.down_time, down_time);
    }
    assert_eq!(events[1].event_time, events[0].event_time + 8_000_000);
    assert_eq!(events[3].event_time, events[2].event_time + 8_000_000);
    assert_eq!(events[3].x(0), 30.0);
    assert_eq!(events[3].y(0), 30.0);
}

#[test]
fn timeline_reply_is_consumed_and_exposed() {
    let (mut registry, mut consumer) = new_loopback();
    let down = monotonic_nanos();
    let _ = registry
        .publisher_mut(TOKEN)
        .expect("publisher")
        .tap_at(4.0, 4.0, down, DEFAULT_TAP_HOLD_NANOS)
        .expect("tap");
    recv_motion(&mut consumer);
    recv_motion(&mut consumer);

    consumer.send_timeline(1, 42, [111, 222]).expect("timeline");
    let publisher = registry.publisher_mut(TOKEN).expect("publisher");
    let response = publisher
        .receive_consumer_response()
        .expect("response")
        .expect("timeline");
    match response {
        ConsumerResponse::Timeline(report) => {
            assert_eq!(report.seq, 1);
            assert_eq!(report.event_id, 42);
            assert_eq!(report.graphics_timeline, [111, 222]);
            assert_eq!(publisher.last_timeline(), Some(report));
        }
        other => panic!("expected timeline, got {other:?}"),
    }
    assert_eq!(
        publisher.pending_len(),
        2,
        "a timeline is not a finished ack"
    );
}

#[test]
fn closing_the_peer_is_a_structured_error_not_a_silent_write() {
    let (mut registry, consumer) = new_loopback();
    assert!(registry.forget_client_end(TOKEN));
    drop(consumer);
    let result = registry
        .publisher_mut(TOKEN)
        .expect("publisher")
        .tap(1.0, 1.0);
    assert!(
        matches!(result, Err(ChannelError::PeerClosed)),
        "expected PeerClosed, got {result:?}"
    );
}

#[test]
fn bad_pointer_count_is_rejected_before_the_write() {
    let (mut registry, _consumer) = new_loopback();
    let mut event = MotionEvent::touch(1, ACTION_DOWN, 1, 1, 0.0, 0.0);
    event.pointers.clear();
    let result = registry
        .publisher_mut(TOKEN)
        .expect("publisher")
        .publish_motion(&event);
    assert!(matches!(result, Err(ChannelError::BadPointerCount(0))));
}

// ---------------------------------------------------------------------------
// registry + the `out InputChannel` parcel body
// ---------------------------------------------------------------------------

#[test]
fn registry_keys_channels_by_window_token() {
    let other = BinderId(0x41_0001);
    let mut registry = InputChannelRegistry::new();
    assert!(registry.is_empty());
    registry.register(TOKEN, "w0").expect("register 0");
    registry.register(other, "w1").expect("register 1");
    assert_eq!(registry.len(), 2);
    assert_eq!(registry.name(TOKEN), Some("w0 client"));
    assert_eq!(registry.name(other), Some("w1 client"));
    assert_eq!(
        registry.publisher(TOKEN).map(|p| p.name()),
        Some("w0 (server)")
    );
    assert_ne!(
        registry.client_fd(TOKEN).map(|f| format!("{f:?}")),
        registry.client_fd(other).map(|f| format!("{f:?}"))
    );
    let tokens: Vec<BinderId> = registry.tokens().collect();
    assert_eq!(tokens, vec![TOKEN, other]);
    assert!(registry.remove(TOKEN));
    assert!(!registry.remove(TOKEN));
    assert_eq!(registry.len(), 1);
    assert!(registry.publisher(TOKEN).is_none());
}

#[test]
fn parcel_body_is_the_w2_shape_and_round_trips() {
    let mut registry = InputChannelRegistry::new();
    registry
        .register(TOKEN, "Surface(name=w)")
        .expect("register");

    let mut reply = Parcel::new();
    registry.write_to_parcel(TOKEN, &mut reply).expect("write");
    reply.set_data_position(0);
    // Read in the image's native order: AIDL presence, `isInitialized`,
    // token binder, name, fd — and nothing left over.
    assert_eq!(reply.read_int32().expect("presence"), 1);
    assert_eq!(reply.read_int32().expect("isInitialized"), 1);
    assert_eq!(reply.read_strong_binder().expect("token"), Some(TOKEN));
    assert_eq!(
        reply.read_string16().expect("name").as_deref(),
        Some("Surface(name=w) client")
    );
    let fd = reply.read_file_descriptor().expect("fd");
    assert!(fd.is_some_and(|fd| fd >= 0), "a real descriptor");
    assert_eq!(reply.data_avail(), 0, "exactly the W2 body");

    // An unknown token is a present-but-uninitialised channel, not a short body.
    let mut missing = Parcel::new();
    registry
        .write_to_parcel(BinderId(0xdead), &mut missing)
        .expect("write");
    missing.set_data_position(0);
    assert_eq!(missing.read_int32().expect("presence"), 1);
    assert_eq!(missing.read_int32().expect("isInitialized"), 0);
    assert_eq!(missing.data_avail(), 0);
}

#[test]
fn forget_client_end_keeps_the_publisher_usable_until_the_guest_leaves() {
    let mut registry = InputChannelRegistry::new();
    registry.register(TOKEN, "w").expect("register");
    let consumer = LoopbackConsumer::new(registry.duplicate_client_fd(TOKEN).expect("fd"));
    assert!(registry.forget_client_end(TOKEN));
    assert!(registry.client_fd(TOKEN).is_none());
    // The guest's end is still open, so publishing still works.
    let gesture = registry
        .publisher_mut(TOKEN)
        .expect("publisher")
        .tap_at(1.0, 1.0, monotonic_nanos(), 10)
        .expect("tap");
    assert_eq!(gesture.dispatches.len(), 2);
    drop(consumer);
    assert!(matches!(
        registry
            .publisher_mut(TOKEN)
            .expect("publisher")
            .tap(2.0, 2.0),
        Err(ChannelError::PeerClosed)
    ));
}

#[test]
fn latency_is_measured_on_the_host_clock() {
    let (mut registry, mut consumer) = new_loopback();
    let _ = registry
        .publisher_mut(TOKEN)
        .expect("publisher")
        .tap_at(1.0, 1.0, monotonic_nanos(), 1_000_000)
        .expect("tap");
    recv_motion(&mut consumer);
    recv_motion(&mut consumer);
    std::thread::sleep(Duration::from_millis(2));
    consumer.send_finished(1, true).expect("finished");
    let publisher = registry.publisher_mut(TOKEN).expect("publisher");
    match publisher
        .receive_consumer_response()
        .expect("response")
        .expect("finished")
    {
        ConsumerResponse::Finished(ack) => {
            let latency = ack.latency.expect("tracked latency");
            assert!(
                latency >= Duration::from_millis(2),
                "latency {latency:?} must cover the hold"
            );
        }
        other => panic!("expected finished, got {other:?}"),
    }
}
