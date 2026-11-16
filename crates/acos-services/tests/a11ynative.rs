//! MODA11YNATIVE: the two `AccessibilityService` payloads that are wire
//! structures — `dispatchGesture`'s
//! `ParceledListSlice<GestureDescription.GestureStep>` and `takeScreenshot`'s
//! `HardwareBuffer` — decoded/encoded against the layout of the framework's own
//! implementation.
//!
//! Teeth: the pre-MODA11YNATIVE code answered `dispatchGesture`/`takeScreenshot`
//! with `UnsupportedAndroidApi` (the connection table ended at transaction 14),
//! so no gesture decoded and no screenshot `Bundle` existed. The captured-parcel
//! row below is the bytes a live ART run of the probe handed the module: it
//! fails on that tree and on any decoder that guesses a field order.

use acos_parcel::Parcel;
use acos_services::a11y_config::{
    A11yServiceConfig, CAPABILITY_CAN_PERFORM_GESTURES, CAPABILITY_CAN_TAKE_SCREENSHOT,
};
use acos_services::a11y_native::{
    self, GRAPHIC_BUFFER_MAGIC, PIXEL_FORMAT_RGBA_8888, SCREENSHOT_USAGE, TAKE_SCREENSHOT_SUCCESS,
};
use acos_services::misc::A11yState;

// ---------------------------------------------------------------------------
// dispatchGesture: a captured parcel from a live ART run
// ---------------------------------------------------------------------------

/// The exact bytes the probe's `dispatchGesture(tap)` put on the wire, captured
/// from an ART run (`ACOSA11Y_DUMP=1`, 460 bytes):
/// `int sequence`, the AIDL typed-object presence word, the
/// `ParceledListSlice` (2 `GestureStep`s: the press at t=0 and the release at
/// t=40 ms, both at (540, 1392) of a 1080x2400 display) and `int displayId`.
fn captured_tap_parcel() -> Parcel {
    let hex = concat!(
        "0100000001000000020000003b00000061006e00640072006f00690064002e0061006300630065007300730069006200",
        "69006c0069007400790073006500720076006900630065002e0047006500730074007500720065004400650073006300",
        "720069007000740069006f006e0024004700650073007400750072006500530074006500700000000100000000000000",
        "00000000010000003a00000061006e00640072006f00690064002e006100630063006500730073006900620069006c00",
        "69007400790073006500720076006900630065002e004700650073007400750072006500440065007300630072006900",
        "7000740069006f006e00240054006f0075006300680050006f0069006e0074000000000000000000ffffffff01000000",
        "000007440000ae44010000002800000000000000010000003a00000061006e00640072006f00690064002e0061006300",
        "63006500730073006900620069006c0069007400790073006500720076006900630065002e0047006500730074007500",
        "720065004400650073006300720069007000740069006f006e00240054006f0075006300680050006f0069006e007400",
        "0000000000000000ffffffff02000000000007440000ae4400000000"
    );
    let bytes: Vec<u8> = (0..hex.len() / 2)
        .map(|i| u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).expect("hex"))
        .collect();
    let mut parcel = Parcel::new();
    parcel.write_raw_bytes(&bytes);
    parcel.set_data_position(0);
    parcel
}

#[test]
fn the_captured_gesture_parcel_decodes_exactly() {
    let mut parcel = captured_tap_parcel();
    assert_eq!(parcel.read_int32().expect("sequence"), 1);
    assert_eq!(parcel.read_int32().expect("typed-object presence"), 1);
    let steps = a11y_native::read_gesture_slice(&mut parcel).expect("decodes");
    assert_eq!(parcel.read_int32().expect("displayId"), 0);
    assert_eq!(
        parcel.remaining(),
        0,
        "the decoder consumes the whole slice: a guessed field order would leave bytes behind"
    );
    assert_eq!(steps.len(), 2, "the guest sampled a press and a release");
    assert_eq!(steps[0].time_since_gesture_start, 0);
    assert_eq!(steps[1].time_since_gesture_start, 40);
    for (index, step) in steps.iter().enumerate() {
        assert_eq!(step.touch_points.len(), 1);
        let point = step.touch_points[0];
        assert_eq!(point.stroke_id, 0);
        assert_eq!(point.continued_stroke_id, -1, "the stroke starts here");
        assert_eq!(point.x, 540.0);
        assert_eq!(point.y, 1392.0);
        if index == 0 {
            assert!(point.is_start() && !point.is_end(), "the press");
        } else {
            assert!(point.is_end() && !point.is_start(), "the release");
        }
    }

    // The step stream becomes the MotionEvent stream a `View` receives: DOWN
    // then UP, at the guest's own times.
    let events = a11y_native::GestureStep::gesture_events(&steps);
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].action, 0, "ACTION_DOWN");
    assert_eq!(events[1].action, 1, "ACTION_UP");
    assert_eq!(events[0].time_ms, 0);
    assert_eq!(events[1].time_ms, 40);

    let channel = a11y_native::channel_events(&events, 1_000_000, 0);
    assert_eq!(channel.len(), 2);
    assert_eq!(channel[0].event_time, 1_000_000);
    assert_eq!(
        channel[1].event_time, 41_000_000,
        "40 ms on the guest clock"
    );
    assert_eq!(channel[0].down_time, 1_000_000);
    // Coordinates ride in the window transform (the image's measured
    // `InputMessage` contract, I2).
    assert_eq!(channel[0].transform.tx, 540.0);
    assert_eq!(channel[0].transform.ty, 1392.0);
}

#[test]
fn a_swipe_step_stream_becomes_down_move_move_up() {
    let step = |time: i64, start_end: i32, x: f32| a11y_native::GestureStep {
        time_since_gesture_start: time,
        touch_points: vec![a11y_native::GestureTouchPoint {
            stroke_id: 0,
            continued_stroke_id: if start_end & a11y_native::TOUCH_POINT_START != 0 {
                -1
            } else {
                0
            },
            start_end,
            x,
            y: 800.0,
        }],
    };
    let steps = vec![
        step(0, a11y_native::TOUCH_POINT_START, 540.0),
        step(16, 0, 530.0),
        step(32, 0, 520.0),
        step(48, a11y_native::TOUCH_POINT_END, 510.0),
    ];
    let events = a11y_native::GestureStep::gesture_events(&steps);
    let actions: Vec<i32> = events.iter().map(|e| e.action).collect();
    assert_eq!(actions, vec![0, 2, 2, 1], "DOWN, MOVE, MOVE, UP");
    assert_eq!(events[3].time_ms, 48);
    assert_eq!(events[2].points[0].0, 520.0);
}

#[test]
fn a_slice_of_the_wrong_element_class_is_a_structured_refusal() {
    let mut parcel = Parcel::new();
    parcel.write_int32(1);
    parcel
        .write_string16("android.view.MotionEvent")
        .expect("class");
    parcel.write_int32(1);
    parcel.set_data_position(0);
    let error = a11y_native::read_gesture_slice(&mut parcel).expect_err("refused");
    assert!(
        error.to_string().contains("android.view.MotionEvent"),
        "the refusal names the element class: {error}"
    );
}

#[test]
fn an_empty_slice_is_an_empty_gesture_not_a_failure() {
    let mut parcel = Parcel::new();
    parcel.write_int32(0);
    parcel.set_data_position(0);
    assert!(a11y_native::read_gesture_slice(&mut parcel)
        .expect("empty")
        .is_empty());
}

#[test]
fn a_remote_retriever_slice_is_refused_rather_than_half_read() {
    let mut parcel = Parcel::new();
    parcel.write_int32(2);
    parcel
        .write_string16(a11y_native::GESTURE_STEP_CLASS)
        .expect("class");
    parcel.write_int32(0); // the retriever marker where an element belongs
    parcel.set_data_position(0);
    let error = a11y_native::read_gesture_slice(&mut parcel).expect_err("refused");
    assert!(error.to_string().contains("remote retriever"), "{error}");
}

// ---------------------------------------------------------------------------
// takeScreenshot payloads
// ---------------------------------------------------------------------------

fn frame() -> a11y_native::ScreenshotFrame {
    // A 2x2 RGBA frame: one `#FF3366CC` pixel and three black ones, stride 2.
    let mut rgba = Vec::new();
    for pixel in [
        [0x33u8, 0x66, 0xCC, 0xFF],
        [0, 0, 0, 0],
        [0, 0, 0, 0],
        [0, 0, 0, 0],
    ] {
        rgba.extend_from_slice(&pixel);
    }
    a11y_native::ScreenshotFrame {
        width: 2,
        height: 2,
        stride: 2,
        rgba,
    }
}

/// Read the `[length][magic][count]` header of a `Bundle` body.
fn bundle_header(parcel: &mut Parcel) -> i32 {
    let _length = parcel.read_int32().expect("length");
    let magic = parcel.read_int32().expect("magic");
    assert_eq!(magic as u32, 0x4e44_4c42, "BUNDLE_MAGIC");
    parcel.read_int32().expect("count")
}

#[test]
fn the_failure_bundle_carries_the_error_code_the_callback_reads() {
    let mut parcel = Parcel::new();
    a11y_native::write_failure_bundle(&mut parcel, 6).expect("writes");
    parcel.set_data_position(0);
    assert_eq!(bundle_header(&mut parcel), 1);
    assert_eq!(
        parcel.read_string16().expect("key").as_deref(),
        Some("screenshot_status")
    );
    assert_eq!(parcel.read_int32().expect("tag"), 0, "VAL_INTEGER");
    assert_eq!(parcel.read_int32().expect("status"), 6);
}

#[test]
fn the_success_bundle_carries_a_flattened_graphic_buffer_over_a_memfd() {
    let frame = frame();
    let mut parcel = Parcel::new();
    a11y_native::write_success_bundle(&mut parcel, &frame, 12_345).expect("writes");
    assert_eq!(
        parcel.owned_file_descriptors().len(),
        1,
        "the HardwareBuffer travels as one fd"
    );
    parcel.set_data_position(0);
    assert_eq!(bundle_header(&mut parcel), 4);

    let mut seen = Vec::new();
    for _ in 0..4 {
        let key = parcel.read_string16().expect("key").expect("non-null key");
        let tag = parcel.read_int32().expect("tag");
        match key.as_str() {
            "screenshot_status" => {
                assert_eq!(tag, 0, "VAL_INTEGER");
                assert_eq!(
                    parcel.read_int32().expect("status"),
                    TAKE_SCREENSHOT_SUCCESS
                );
            }
            "screenshot_timestamp" => {
                assert_eq!(tag, 1, "VAL_LONG");
                assert_eq!(parcel.read_int64().expect("timestamp"), 12_345);
            }
            "screenshot_colorSpace" => {
                assert_eq!(tag, 2, "VAL_PARCELABLE");
                assert_eq!(
                    parcel.read_string16().expect("class").as_deref(),
                    Some("android.graphics.ParcelableColorSpace")
                );
                assert_eq!(parcel.read_int32().expect("ColorSpace id"), 1, "SRGB");
            }
            "screenshot_hardwareBuffer" => {
                assert_eq!(tag, 2, "VAL_PARCELABLE");
                assert_eq!(
                    parcel.read_string16().expect("class").as_deref(),
                    Some("android.hardware.HardwareBuffer")
                );
                // `Parcel::write(const Flattenable&)`: size, fd count, body.
                assert_eq!(parcel.read_int32().expect("size"), 88, "13+9 words");
                assert_eq!(parcel.read_int32().expect("fd count"), 1);
                assert_eq!(
                    parcel.read_int32().expect("magic"),
                    GRAPHIC_BUFFER_MAGIC,
                    "GraphicBuffer::flatten's 'GB01'"
                );
                assert_eq!(parcel.read_int32().expect("width"), 2);
                assert_eq!(parcel.read_int32().expect("height"), 2);
                assert_eq!(parcel.read_int32().expect("stride"), 2);
                assert_eq!(parcel.read_int32().expect("format"), PIXEL_FORMAT_RGBA_8888);
                assert_eq!(parcel.read_int32().expect("layers"), 1);
                assert_eq!(
                    parcel.read_int32().expect("usage lo") as u32 as u64,
                    SCREENSHOT_USAGE & 0xffff_ffff,
                    "USAGE_CPU_READ_OFTEN|USAGE_GPU_SAMPLED_IMAGE: wrapHardwareBuffer's gate"
                );
                let _id_hi = parcel.read_int32().expect("id hi");
                let _id_lo = parcel.read_int32().expect("id lo");
                let _generation = parcel.read_int32().expect("generation");
                assert_eq!(parcel.read_int32().expect("handle fds"), 1);
                assert_eq!(parcel.read_int32().expect("handle ints"), 9);
                let _usage_hi = parcel.read_int32().expect("usage hi");
                assert_eq!(parcel.read_int32().expect("handle width"), 2);
                assert_eq!(parcel.read_int32().expect("handle height"), 2);
                assert_eq!(parcel.read_int32().expect("handle stride"), 2);
                assert_eq!(parcel.read_int32().expect("handle format"), 1);
                for _ in 0..5 {
                    let _ = parcel.read_int32().expect("handle tail");
                }
                let _fd = parcel.read_file_descriptor().expect("fd").expect("present");
            }
            other => panic!("unexpected bundle key {other}"),
        }
        seen.push(key);
    }
    let mut sorted = seen.clone();
    sorted.sort_by_key(|key| {
        key.encode_utf16()
            .fold(0i32, |h, c| h.wrapping_mul(31).wrapping_add(i32::from(c)))
    });
    assert_eq!(seen, sorted, "Bundle entries are in ArrayMap order");
}

#[test]
fn the_memfd_behind_the_hardware_buffer_holds_the_composited_pixels() {
    use std::io::Read as _;
    let frame = frame();
    let mut parcel = Parcel::new();
    a11y_native::write_success_bundle(&mut parcel, &frame, 0).expect("writes");
    let raw = parcel.owned_file_descriptors()[0];
    let borrowed = parcel
        .borrowed_file_descriptor(raw)
        .expect("the parcel still owns it");
    let dup = rustix::io::dup(borrowed).expect("dup");
    let mut file = std::fs::File::from(dup);
    // The descriptor shares the writer's file offset (the memfd was just
    // written through a duplicate), so read from the start explicitly.
    {
        use std::io::Seek as _;
        file.seek(std::io::SeekFrom::Start(0)).expect("seek");
    }
    let mut pixels = Vec::new();
    file.read_to_end(&mut pixels).expect("read memfd");
    assert_eq!(pixels, frame.rgba, "the memfd is a copy of the frame");
    assert_eq!(&pixels[0..4], &[0x33, 0x66, 0xCC, 0xFF], "the swatch pixel");
}

#[test]
fn a_frame_whose_pixels_disagree_with_its_size_is_refused() {
    let mut frame = frame();
    frame.rgba.truncate(4);
    let mut parcel = Parcel::new();
    let error = a11y_native::write_success_bundle(&mut parcel, &frame, 0).expect_err("refused");
    assert!(error.to_string().contains("needs"), "{error}");
}

// ---------------------------------------------------------------------------
// The capability gates the two requests read
// ---------------------------------------------------------------------------

#[test]
fn the_screenshot_and_gesture_gates_read_the_services_own_declaration() {
    let mut state = A11yState::new();
    assert!(
        !state.can_take_screenshot(),
        "no bound service, no capability"
    );
    assert!(!state.can_perform_gestures());
    state.service_config = Some(A11yServiceConfig {
        capabilities: CAPABILITY_CAN_TAKE_SCREENSHOT | CAPABILITY_CAN_PERFORM_GESTURES,
        ..A11yServiceConfig::default()
    });
    assert!(state.can_take_screenshot());
    assert!(state.can_perform_gestures());
    state.service_config = Some(A11yServiceConfig {
        capabilities: CAPABILITY_CAN_TAKE_SCREENSHOT,
        ..A11yServiceConfig::default()
    });
    assert!(state.can_take_screenshot());
    assert!(
        !state.can_perform_gestures(),
        "one capability is not the other"
    );
}

#[test]
fn an_unbound_host_has_no_gesture_target_and_no_screenshot_frame() {
    let state = A11yState::new();
    assert!(state.gesture_token().is_err(), "no input channels");
    // `ERROR_TAKE_SCREENSHOT_INVALID_DISPLAY` — never pixels that do not exist.
    assert_eq!(a11y_native::capture_frame(&state).expect_err("none"), 4);
    assert!(!a11y_native::secure_window_visible(&state));
}
