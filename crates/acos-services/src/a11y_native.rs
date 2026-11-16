//! The two `AccessibilityService` payloads whose bodies are **wire** structures,
//! not Java field lists: `dispatchGesture`'s
//! `ParceledListSlice<GestureDescription.GestureStep>` and `takeScreenshot`'s
//! `HardwareBuffer`.
//!
//! # `dispatchGesture`: the slice carries gesture *steps*, not MotionEvents
//!
//! MODA11Y7 read the image's `AccessibilityService.dispatchGesture` as building
//! `MotionEvent`s. The image's own bytecode says otherwise (dumped from
//! `.cache/boot/android-34/dex/framework/classes.dex`):
//!
//! ```text
//! AccessibilityService->dispatchGesture:
//!   invoke-static MotionEventGenerator.getGestureStepsFromGestureDescription(...)Ljava/util/List;
//!   new-instance ParceledListSlice; <init>(List); dispatchGesture(mSequence, slice, displayId)
//! ```
//!
//! `getGestureStepsFromGestureDescription` returns `List<GestureStep>` (the
//! method's own generic signature), so the AIDL named `gestureSteps` really
//! does carry steps. A step is
//! `GestureDescription.GestureStep.writeToParcel` = `writeLong(timeSinceGestureStart)`
//! + `writeParcelableArray(touchPoints)`, and a touch point is
//! `TouchPoint.writeToParcel` = `mStrokeId`, `mContinuedStrokeId`,
//! `mIsStartOfPath | (mIsEndOfPath ? 2 : 0)`, `mX`, `mY`. The *system* turns
//! steps into `MotionEvent`s (`MotionEventInjector.injectEvents`), which is
//! exactly what this module does with [`GestureStep::channel_events`].
//!
//! Every field order above was verified twice: against the image's writer and
//! reader bytecode, and against a real captured parcel (`tests/a11ynative.rs`
//! decodes the bytes an ART run handed the module, and consumes them exactly).
//!
//! # `takeScreenshot`: a flattened `GraphicBuffer`
//!
//! `HardwareBuffer` is a flattened `GraphicBuffer`
//! (`GraphicBuffer::flatten` in `.cache/aosp/native/libs/ui/GraphicBuffer.cpp`,
//! confirmed against the image's own `libui` in `docs/IMAGE_ABI_NOTEBOOK.md`
//! §W8: 13 header words + the handle's transport ints, one fd): `'GB01'` magic,
//! width, height, stride, format, layerCount, usage low, id high, id low,
//! generation, transport fd count, transport int count, usage high, then the
//! handle ints; `Parcel::write(const Flattenable&)` wraps it as
//! `[size][fdCount][body]` with the fd written after the body.
//!
//! # Why the guest side needs no new native
//!
//! The ART guest runs the image's own `libandroid_runtime`/`libui` and the
//! image's `MotionEvent`/`HardwareBuffer` registrations, so both are the
//! guest's real implementations (not the frozen Rust VM's refusal). What was
//! missing on the guest side is only the `android.hardware.HardwareBuffer`
//! registration (see `SessionMain.stepRuntimeExtras`), and the memory behind
//! the buffer: ACOS's `libacoshw.so` gralloc shim already owns buffer memory (a
//! memfd) and imports a handle on unflatten, so the host produces a flattened
//! `GraphicBuffer` over a memfd copy of the composited frame — no real gralloc
//! HAL (there is none in the guest) and no GPU (there is none either). That
//! substitution is the one the shim already documents for the app's own window
//! surfaces; nothing about it is specific to a screenshot.

use acos_parcel::Parcel;

use crate::error::{Result, ServiceError};
use crate::misc::A11yState;

/// `HAL_PIXEL_FORMAT_RGBA_8888` (the format the composited frames carry).
pub const PIXEL_FORMAT_RGBA_8888: i32 = 1;

/// `GRALLOC_USAGE_SW_READ_OFTEN | GRALLOC_USAGE_HW_TEXTURE`: the usage AOSP's
/// screenshot buffer reports (`USAGE_CPU_READ_OFTEN` +
/// `USAGE_GPU_SAMPLED_IMAGE`; the two are bit-compatible,
/// `AHardwareBufferHelpers::convertFromGrallocUsageBits`).
pub const SCREENSHOT_USAGE: u64 = 0x1 | 0x100;

/// `ColorSpace.Named.SRGB`'s id (`ColorSpace.getId()`), the color space of the
/// composited frames.
pub const COLOR_SPACE_SRGB: i32 = 1;

/// `GraphicBuffer::flatten`'s magic: the C multichar constant `'GB01'`, i.e.
/// `0x47423031` (the image writes the same word; `IMAGE_ABI_NOTEBOOK.md` §W8
/// observed its little-endian bytes `31 30 42 47`).
pub const GRAPHIC_BUFFER_MAGIC: i32 = 0x4742_3031;

/// Number of header words in `GraphicBuffer::flatten`'s body.
pub const GRAPHIC_BUFFER_HEADER_WORDS: usize = 13;

/// `handle->numFds` for the ACOS software gralloc handle.
pub const GRAPHIC_BUFFER_HANDLE_FDS: i32 = 1;

/// `handle->numInts` for the ACOS software gralloc handle
/// (`w, h, stride, format, usageLo, usageHi, layers, bpp, dataspace`).
pub const GRAPHIC_BUFFER_HANDLE_INTS: i32 = 9;

/// `GestureDescription$GestureStep` is the `ParceledListSlice` element class.
pub const GESTURE_STEP_CLASS: &str = "android.accessibilityservice.GestureDescription$GestureStep";

/// `GestureDescription$TouchPoint`'s class name (one per array element).
pub const TOUCH_POINT_CLASS: &str = "android.accessibilityservice.GestureDescription$TouchPoint";

/// `GestureDescription.TouchPoint.FLAG_IS_START_OF_PATH`.
pub const TOUCH_POINT_START: i32 = 1;
/// `GestureDescription.TouchPoint.FLAG_IS_END_OF_PATH`.
pub const TOUCH_POINT_END: i32 = 2;

/// `MotionEvent.ACTION_MASK`.
const ACTION_MASK: i32 = 0xff;
/// `MotionEvent.ACTION_POINTER_DOWN` (`5 | index << 8`).
const ACTION_POINTER_DOWN: i32 = 5;
/// `MotionEvent.ACTION_POINTER_UP` (`6 | index << 8`).
const ACTION_POINTER_UP: i32 = 6;
/// `MotionEvent.ACTION_MOVE`.
const ACTION_MOVE: i32 = 2;
/// `MotionEvent.ACTION_DOWN`.
const ACTION_DOWN: i32 = 0;
/// `MotionEvent.ACTION_UP`.
const ACTION_UP: i32 = 1;

/// `bad_reply` and `bad_request` name the same failure: a request this module
/// cannot decode. Two names because the screenshot writers read like replies.
fn bad_reply(method: &str, detail: impl Into<String>) -> ServiceError {
    bad_request(method, detail)
}

fn bad_request(method: &str, detail: impl Into<String>) -> ServiceError {
    ServiceError::BadRequest {
        interface: "android.accessibilityservice.IAccessibilityServiceConnection".into(),
        method: method.into(),
        detail: detail.into(),
    }
}

/// One `GestureDescription$TouchPoint`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GestureTouchPoint {
    /// `mStrokeId`.
    pub stroke_id: i32,
    /// `mContinuedStrokeId` (`-1` when the stroke starts here).
    pub continued_stroke_id: i32,
    /// `mIsStartOfPath | (mIsEndOfPath ? 2 : 0)`.
    pub start_end: i32,
    /// `mX` in display coordinates.
    pub x: f32,
    /// `mY` in display coordinates.
    pub y: f32,
}

impl GestureTouchPoint {
    /// `mIsStartOfPath`.
    #[must_use]
    pub const fn is_start(&self) -> bool {
        self.start_end & TOUCH_POINT_START != 0
    }

    /// `mIsEndOfPath`.
    #[must_use]
    pub const fn is_end(&self) -> bool {
        self.start_end & TOUCH_POINT_END != 0
    }
}

/// One `GestureDescription$GestureStep`.
#[derive(Debug, Clone, PartialEq)]
pub struct GestureStep {
    /// `timeSinceGestureStart` in milliseconds (the guest's own sampling).
    pub time_since_gesture_start: i64,
    /// The touch points active in this step.
    pub touch_points: Vec<GestureTouchPoint>,
}

/// Read the `ParceledListSlice<GestureStep>` of `dispatchGesture`.
///
/// `ParceledListSlice.writeToParcel` writes the item count, then (for a
/// non-empty list) the element creator's class name and one
/// `writeTypedObject`-framed element per item; each element is a
/// `GestureStep.writeToParcel` body. A remote retriever (a list too large to
/// inline) is a structured refusal, never a half-read list.
pub fn read_gesture_slice(parcel: &mut Parcel) -> Result<Vec<GestureStep>> {
    let count = parcel.read_int32()?;
    if count <= 0 {
        return Ok(Vec::new());
    }
    let class = parcel
        .read_string16()?
        .ok_or_else(|| bad_request("dispatchGesture", "a non-empty slice has no element class"))?;
    if class != GESTURE_STEP_CLASS {
        return Err(bad_request(
            "dispatchGesture",
            format!("ParceledListSlice element {class}, expected {GESTURE_STEP_CLASS}"),
        ));
    }
    let count = usize::try_from(count)
        .map_err(|_| bad_request("dispatchGesture", "negative element count"))?;
    let mut steps = Vec::with_capacity(count.min(4096));
    for _ in 0..count {
        if parcel.read_int32()? == 0 {
            return Err(bad_request(
                "dispatchGesture",
                "the gesture slice carries a remote retriever, which ACOS does not model",
            ));
        }
        steps.push(read_gesture_step(parcel)?);
    }
    Ok(steps)
}

/// Read one `GestureStep` (`long timeSinceGestureStart` + `TouchPoint[]`).
fn read_gesture_step(parcel: &mut Parcel) -> Result<GestureStep> {
    let time_since_gesture_start = parcel.read_int64()?;
    let point_count = parcel.read_int32()?;
    if point_count < 0 || point_count > 32 {
        return Err(bad_request(
            "dispatchGesture",
            format!("GestureStep touch point count {point_count}"),
        ));
    }
    let mut touch_points = Vec::with_capacity(point_count as usize);
    for _ in 0..point_count {
        let class = parcel
            .read_string16()?
            .ok_or_else(|| bad_request("dispatchGesture", "a null GestureStep touch point"))?;
        if class != TOUCH_POINT_CLASS {
            return Err(bad_request(
                "dispatchGesture",
                format!("touch point class {class}, expected {TOUCH_POINT_CLASS}"),
            ));
        }
        let stroke_id = parcel.read_int32()?;
        let continued_stroke_id = parcel.read_int32()?;
        let start_end = parcel.read_int32()?;
        let x = parcel.read_float()?;
        let y = parcel.read_float()?;
        touch_points.push(GestureTouchPoint {
            stroke_id,
            continued_stroke_id,
            start_end,
            x,
            y,
        });
    }
    Ok(GestureStep {
        time_since_gesture_start,
        touch_points,
    })
}

/// The `MotionEvent`s one decoded gesture produces, with times relative to the
/// stroke's own `timeSinceGestureStart`.
#[derive(Debug, Clone, PartialEq)]
pub struct GestureEvent {
    /// `MotionEvent.getAction()` (masked action + pointer index bits).
    pub action: i32,
    /// Milliseconds since the gesture started (the guest's sampling clock).
    pub time_ms: i64,
    /// `(x, y)` in display coordinates, one per active pointer.
    pub points: Vec<(f32, f32)>,
}

impl GestureStep {
    /// Convert a step list into the `MotionEvent` stream the framework's
    /// `MotionEventInjector` would inject.
    ///
    /// The mapping is the one AOSP's injector uses: a point whose
    /// `mIsStartOfPath` is set is a `DOWN` (or `POINTER_DOWN` when another
    /// pointer is already down), a point whose `mIsEndOfPath` is set is an `UP`
    /// (`POINTER_UP` with several pointers), and a point that is neither is a
    /// `MOVE`. A point that starts and ends in the same step (a tap) produces
    /// the down first and the up in the step after it, exactly as the guest's
    /// own samples describe.
    #[must_use]
    pub fn gesture_events(steps: &[GestureStep]) -> Vec<GestureEvent> {
        let mut active: Vec<i32> = Vec::new();
        let mut out: Vec<GestureEvent> = Vec::new();
        for step in steps {
            // One step can lift one pointer, press another and move a third, so
            // the events are ordered the way a touch stream must be: releases,
            // then presses, then the moves that carry every pointer's position.
            for point in step.touch_points.iter().filter(|p| p.is_end()) {
                let index = active
                    .iter()
                    .position(|id| *id == point.stroke_id)
                    .unwrap_or(0);
                let action = if active.len() <= 1 {
                    ACTION_UP
                } else {
                    ACTION_POINTER_UP | ((index as i32) << 8)
                };
                if let Some(position) = active.iter().position(|id| *id == point.stroke_id) {
                    active.remove(position);
                }
                out.push(GestureEvent {
                    action,
                    time_ms: step.time_since_gesture_start,
                    points: vec![(point.x, point.y)],
                });
            }
            for point in step
                .touch_points
                .iter()
                .filter(|p| p.is_start() && !p.is_end())
            {
                let first = active.is_empty();
                let index = active.len() as i32;
                active.push(point.stroke_id);
                out.push(GestureEvent {
                    action: if first {
                        ACTION_DOWN
                    } else {
                        ACTION_POINTER_DOWN | (index << 8)
                    },
                    time_ms: step.time_since_gesture_start,
                    points: vec![(point.x, point.y)],
                });
            }
            // A tap is one step whose point both starts and ends: the framework's
            // own injector sends the press and the release for it.
            for point in step
                .touch_points
                .iter()
                .filter(|p| p.is_start() && p.is_end())
            {
                let first = active.is_empty();
                if first {
                    active.push(point.stroke_id);
                    out.push(GestureEvent {
                        action: ACTION_DOWN,
                        time_ms: step.time_since_gesture_start,
                        points: vec![(point.x, point.y)],
                    });
                    active.clear();
                }
                out.push(GestureEvent {
                    action: ACTION_UP,
                    time_ms: step.time_since_gesture_start,
                    points: vec![(point.x, point.y)],
                });
            }
            let moved: Vec<&GestureTouchPoint> = step
                .touch_points
                .iter()
                .filter(|p| !p.is_start() && !p.is_end())
                .collect();
            if !moved.is_empty() {
                out.push(GestureEvent {
                    action: ACTION_MOVE,
                    time_ms: step.time_since_gesture_start,
                    points: moved.iter().map(|point| (point.x, point.y)).collect(),
                });
            }
        }
        // A stroke that never ends (a cancelled or truncated gesture) is closed
        // with an `UP` at the last step's time: a `View` never keeps a dangling
        // DOWN.
        if let (Some(last), Some(point)) = (
            steps.last(),
            steps.last().and_then(|s| s.touch_points.first()),
        ) {
            if !active.is_empty() {
                out.push(GestureEvent {
                    action: ACTION_UP,
                    time_ms: last.time_since_gesture_start,
                    points: vec![(point.x, point.y)],
                });
            }
        }
        out
    }
}

/// Convert a gesture's events into the channel events the session publishes.
///
/// Coordinates ride in the window transform, not the pointer axes, for the
/// measured reason `InputDispatch::inject_tap` does
/// (`docs/IMAGE_ABI_NOTEBOOK.md`, I2): this image's `InputMessage` consumer
/// reads the pointer position from `MotionEvent.transform`. `down_time_ns` is
/// the gesture's start on the host clock and every event keeps the guest's own
/// offset from it, so a stroke's timing is the guest's sampling, not a
/// host-side sleep.
#[must_use]
pub fn channel_events(
    events: &[GestureEvent],
    down_time_ns: i64,
    display_id: i32,
) -> Vec<crate::input_channel::MotionEvent> {
    use crate::input_channel::{MotionEvent, Pointer, PointerCoords, Transform};
    let mut out = Vec::with_capacity(events.len());
    for event in events {
        let (x, y) = event.points.first().copied().unwrap_or((0.0, 0.0));
        let mut motion = MotionEvent::touch(
            // Unique per published event (I1's `eventId`): the guest's own
            // event time is a stable, unique key for a gesture's stream.
            event.time_ms as i32,
            event.action,
            down_time_ns,
            down_time_ns + event.time_ms.saturating_mul(1_000_000),
            0.0,
            0.0,
        );
        motion.device_id = crate::window_session::input_dispatch::VIRTUAL_KEYBOARD_DEVICE_ID;
        motion.display_id = display_id;
        let mut transform = Transform::identity();
        transform.tx = x;
        transform.ty = y;
        motion.transform = transform;
        motion.raw_transform = transform;
        motion.pointers = event
            .points
            .iter()
            .enumerate()
            .map(|(index, (x, y))| Pointer {
                properties: crate::input_channel::PointerProperties {
                    id: index as i32,
                    tool_type: 1, // MotionEvent.TOOL_TYPE_FINGER
                },
                coords: PointerCoords::touch(*x, *y),
            })
            .collect();
        out.push(motion);
    }
    out
}

/// The composited frame plus the display and secure-window facts a screenshot
/// answers with.
#[derive(Debug, Clone)]
pub struct ScreenshotFrame {
    /// Buffer width in pixels.
    pub width: u32,
    /// Buffer height in pixels.
    pub height: u32,
    /// Row stride in pixels.
    pub stride: u32,
    /// Raw `RGBA_8888` pixels (`stride * 4 * height` bytes).
    pub rgba: Vec<u8>,
}

/// Capture the composited frame the screenshot is made of.
///
/// Failure is structured: no compositor (a bare host) or no stored buffer yet
/// (nothing drawn) is `ERROR_TAKE_SCREENSHOT_INVALID_DISPLAY`, which is what
/// AOSP answers when the display cannot be captured.
pub fn capture_frame(state: &A11yState) -> std::result::Result<ScreenshotFrame, i32> {
    /// `AccessibilityService.ERROR_TAKE_SCREENSHOT_INVALID_DISPLAY`.
    const ERROR_INVALID_DISPLAY: i32 = 4;
    let Some(compositor) = state.compositor.as_ref() else {
        return Err(ERROR_INVALID_DISPLAY);
    };
    let capture = compositor
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .capture_composite()
        .map_err(|_| ERROR_INVALID_DISPLAY)?;
    Ok(ScreenshotFrame {
        width: capture.width,
        height: capture.height,
        stride: capture.stride,
        rgba: capture.rgba,
    })
}

/// Is a `FLAG_SECURE` window on screen? (`AccessibilitySecurityPolicy`'s
/// secure-window answer; `ERROR_TAKE_SCREENSHOT_SECURE_WINDOW` when true.)
#[must_use]
pub fn secure_window_visible(state: &A11yState) -> bool {
    state.compositor.as_ref().is_some_and(|compositor| {
        compositor
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .has_secure_window()
    })
}

/// Write a flattened `GraphicBuffer` over an fd-backed copy of `frame`.
///
/// The bytes are `GraphicBuffer::flatten`'s body for a software-gralloc handle
/// (see the module docs). The fd is the guest's side of an anonymous memfd
/// holding exactly the pixels; the guest's gralloc shim imports it on
/// `GraphicBuffer::unflatten`, so the app's `HardwareBuffer` really does point
/// at the composited frame.
fn write_flattened_buffer(
    parcel: &mut Parcel,
    frame: &ScreenshotFrame,
    fd: std::os::fd::BorrowedFd<'_>,
) -> Result<()> {
    let body_words = GRAPHIC_BUFFER_HEADER_WORDS + GRAPHIC_BUFFER_HANDLE_INTS as usize;
    let body_bytes = body_words * 4;
    // `Parcel::write(const Flattenable&)`: length, fd count, body, then the fds.
    parcel.write_int32(i32::try_from(body_bytes).map_err(|_| bad_reply("takeScreenshot", "size"))?);
    parcel.write_int32(GRAPHIC_BUFFER_HANDLE_FDS);
    parcel.write_int32(GRAPHIC_BUFFER_MAGIC);
    parcel.write_int32(i32::try_from(frame.width).unwrap_or(i32::MAX));
    parcel.write_int32(i32::try_from(frame.height).unwrap_or(i32::MAX));
    parcel.write_int32(i32::try_from(frame.stride).unwrap_or(i32::MAX));
    parcel.write_int32(PIXEL_FORMAT_RGBA_8888);
    parcel.write_int32(1); // layerCount
    parcel.write_int32((SCREENSHOT_USAGE & 0xffff_ffff) as i32); // usage low
    parcel.write_int32(0); // mId high
    parcel.write_int32(1); // mId low (a stable, non-zero buffer id)
    parcel.write_int32(0); // mGenerationNumber
    parcel.write_int32(GRAPHIC_BUFFER_HANDLE_FDS);
    parcel.write_int32(GRAPHIC_BUFFER_HANDLE_INTS);
    parcel.write_int32((SCREENSHOT_USAGE >> 32) as i32); // usage high
                                                         // The handle's transport ints (`/data` after the fd in the gralloc shim's
                                                         // `native_handle_t`): w, h, stride, format, usageLo, usageHi, layers, bpp,
                                                         // dataspace.
    parcel.write_int32(i32::try_from(frame.width).unwrap_or(i32::MAX));
    parcel.write_int32(i32::try_from(frame.height).unwrap_or(i32::MAX));
    parcel.write_int32(i32::try_from(frame.stride).unwrap_or(i32::MAX));
    parcel.write_int32(PIXEL_FORMAT_RGBA_8888);
    parcel.write_int32((SCREENSHOT_USAGE & 0xffff_ffff) as i32);
    parcel.write_int32((SCREENSHOT_USAGE >> 32) as i32);
    parcel.write_int32(1); // layers
    parcel.write_int32(4); // bytes per pixel
    parcel.write_int32(0); // dataspace (unknown)
    parcel
        .write_file_descriptor(fd)
        .map_err(|error| bad_reply("takeScreenshot", format!("buffer fd: {error}")))?;
    Ok(())
}

/// Copy `frame` into an anonymous memfd and write the flattened buffer into
/// `parcel`.
fn write_screenshot_buffer(parcel: &mut Parcel, frame: &ScreenshotFrame) -> Result<()> {
    use std::io::Write as _;
    let file = acos_parcel::ParcelFd::memfd("acos-a11y-screenshot")
        .map_err(|error| bad_reply("takeScreenshot", format!("memfd: {error}")))?;
    let row_bytes = frame.stride as usize * 4;
    let expected = row_bytes.saturating_mul(frame.height as usize);
    if frame.rgba.len() != expected {
        return Err(bad_reply(
            "takeScreenshot",
            format!(
                "frame is {} bytes, {}x{}x4 needs {expected}",
                frame.rgba.len(),
                frame.width,
                frame.height
            ),
        ));
    }
    {
        // `ParcelFd` owns an `OwnedFd`; a second handle to the same memfd is
        // what lets us write through `File` while the parcel keeps the original.
        let dup = rustix::io::dup(file.as_fd())
            .map_err(|error| bad_reply("takeScreenshot", format!("dup memfd: {error}")))?;
        let mut writer = std::fs::File::from(dup);
        writer
            .write_all(&frame.rgba)
            .and_then(|()| writer.flush())
            .map_err(|error| bad_reply("takeScreenshot", format!("memfd write: {error}")))?;
    }
    write_flattened_buffer(parcel, frame, file.as_fd())
}

/// `android.os.Bundle` keys of a screenshot result
/// (`AccessibilityService.KEY_ACCESSIBILITY_SCREENSHOT_*`).
pub const KEY_SCREENSHOT_STATUS: &str = "screenshot_status";
/// The `HardwareBuffer` payload key.
pub const KEY_SCREENSHOT_HARDWAREBUFFER: &str = "screenshot_hardwareBuffer";
/// The `ParcelableColorSpace` payload key.
pub const KEY_SCREENSHOT_COLORSPACE: &str = "screenshot_colorSpace";
/// The capture timestamp key.
pub const KEY_SCREENSHOT_TIMESTAMP: &str = "screenshot_timestamp";

/// `AccessibilityService.TAKE_SCREENSHOT_SUCCESS`.
pub const TAKE_SCREENSHOT_SUCCESS: i32 = 0;

/// The `Parcel.writeValue` tags, from [`crate::parcelable::val`] — the image's
/// own `Parcel.getValueType`/`isLengthPrefixed` values.
///
/// These were once hand-copied as `0/1/2`, which is `VAL_STRING`/`VAL_INTEGER`/
/// `VAL_MAP`: the framework then read an integer payload as a string, a long as
/// an int, and a parcelable as a length-prefixed map. Nothing reads a `Bundle`
/// with invented tags.
use crate::parcelable::val::{
    INTEGER as VAL_INTEGER, LONG as VAL_LONG, PARCELABLE as VAL_PARCELABLE,
};
/// Android 14 `Bundle` magic (`"BNDL"`).
///
/// The one definition lives in [`crate::parcelable`] and is shared here on
/// purpose: a local copy of this constant once carried the two 16-bit halves
/// swapped (`0x4e44_4c42`), so `BaseBundle.readFromParcelInner` read the magic
/// word **as** the byte length (`1313098818`, not a multiple of 4) and threw
/// `IllegalStateException("Bundle length is not aligned by 4")` inside the
/// guest before any callback ran. One constant, one wire fact.
const BUNDLE_MAGIC: i32 = crate::parcelable::BUNDLE_MAGIC as i32;

/// Java's `String.hashCode` (the `ArrayMap` order `Bundle` writes entries in).
fn java_string_hash(s: &str) -> i32 {
    s.encode_utf16()
        .fold(0i32, |h, c| h.wrapping_mul(31).wrapping_add(i32::from(c)))
}

/// Write one `VAL_PARCELABLE` value: the tag, the length of the value's body,
/// then `writeParcelable(v, 0)`'s class name followed by the payload.
///
/// `Parcel.isLengthPrefixed` — read out of the image's own bytecode
/// (`framework/classes3.dex`, `Landroid/os/Parcel;->isLengthPrefixed(I)Z`, its
/// sparse-switch keys are `2, 4, 11, 12, 16, 17, 21`) — lists `VAL_PARCELABLE`
/// (4) as length-prefixed, and `Parcel.readValue` consumes that word before
/// `readParcelableInternal`. Without it every following entry is four bytes out
/// of step, which is why the framework-side codec is written here as tags plus
/// lengths rather than as a bare tag/value stream.
fn write_parcelable_value(
    parcel: &mut Parcel,
    class: &str,
    body: impl FnOnce(&mut Parcel) -> Result<()>,
) -> Result<()> {
    parcel.write_int32(VAL_PARCELABLE);
    let length_at = parcel.data_size();
    parcel.write_int32(0);
    let start = parcel.data_size();
    parcel.write_string16(class)?;
    body(parcel)?;
    let end = parcel.data_size();
    let length = i32::try_from(end - start).map_err(|_| {
        bad_reply(
            "takeScreenshot",
            format!(
                "a {class} payload of {} bytes does not fit an int",
                end - start
            ),
        )
    })?;
    parcel.set_data_position(length_at);
    parcel.write_int32(length);
    parcel.set_data_position(end);
    Ok(())
}

/// Start an `IRemoteCallback.sendResult(in Bundle)` payload: the interface
/// token, then `Parcel.writeTypedObject`'s presence word.
///
/// The image's `IRemoteCallback$Stub.onTransact` (code 1) reads the argument
/// with `Parcel.readTypedObject(Bundle.CREATOR)` — an `int` presence word and
/// then `Bundle.writeToParcel` — and finishes with `enforceNoDataAvail()`. A
/// payload that starts straight at the `Bundle` makes the framework read the
/// Bundle's length word as the presence flag and the magic as the length, which
/// is exactly the `-Bundle length is not aligned by 4` the guest threw out of
/// the reverse delivery.
fn begin_send_result(parcel: &mut Parcel) -> Result<()> {
    parcel.write_interface_token(crate::a11y_bind::REMOTE_CALLBACK)?;
    parcel.write_int32(1);
    Ok(())
}

/// Write the failure `Bundle` of `sendScreenshotFailure`.
///
/// The whole `sendResult` payload is written: interface token, the typed-object
/// presence word, then the `Bundle` body.
pub fn write_failure_bundle(parcel: &mut Parcel, status: i32) -> Result<()> {
    begin_send_result(parcel)?;
    let start = parcel.data_size();
    // `BaseBundle.writeToParcelInner` starts with its `-1` placeholder and
    // back-patches the payload length after the entries.
    parcel.write_int32(-1);
    parcel.write_int32(BUNDLE_MAGIC);
    let payload_start = parcel.data_size();
    parcel.write_int32(1);
    parcel.write_string16(KEY_SCREENSHOT_STATUS)?;
    parcel.write_int32(VAL_INTEGER);
    parcel.write_int32(status);
    backpatch_bundle_length(parcel, start, payload_start)
}

/// Back-patch `BaseBundle.writeToParcelInner`'s length word: the payload only
/// (the `ArrayMap` entries after the magic), exactly the count
/// `readFromParcelInner` advances by.
fn backpatch_bundle_length(parcel: &mut Parcel, start: usize, payload_start: usize) -> Result<()> {
    let end = parcel.data_size();
    let length = i32::try_from(end - payload_start).map_err(|_| {
        bad_reply(
            "takeScreenshot",
            format!(
                "a bundle payload of {} bytes does not fit an int",
                end - payload_start
            ),
        )
    })?;
    parcel.set_data_position(start);
    parcel.write_int32(length);
    parcel.set_data_position(end);
    Ok(())
}

/// Write the success `sendResult` payload of `sendScreenshotSuccess`: status,
/// the `HardwareBuffer`, the `ParcelableColorSpace` and `capture_time_ms`.
///
/// The whole payload is written: interface token, the typed-object presence
/// word, then the `Bundle` body (see [`begin_send_result`]).
pub fn write_success_bundle(
    parcel: &mut Parcel,
    frame: &ScreenshotFrame,
    capture_time_ms: i64,
) -> Result<()> {
    begin_send_result(parcel)?;
    let keys = [
        KEY_SCREENSHOT_STATUS,
        KEY_SCREENSHOT_HARDWAREBUFFER,
        KEY_SCREENSHOT_COLORSPACE,
        KEY_SCREENSHOT_TIMESTAMP,
    ];
    let mut ordered = keys.to_vec();
    ordered.sort_by_key(|key| java_string_hash(key));
    let start = parcel.data_size();
    parcel.write_int32(-1);
    parcel.write_int32(BUNDLE_MAGIC);
    let payload_start = parcel.data_size();
    parcel.write_int32(i32::try_from(ordered.len()).map_err(|_| {
        bad_reply(
            "takeScreenshot",
            "the screenshot bundle has more entries than an int can carry",
        )
    })?);
    for key in ordered {
        parcel.write_string16(key)?;
        match key {
            KEY_SCREENSHOT_STATUS => {
                parcel.write_int32(VAL_INTEGER);
                parcel.write_int32(TAKE_SCREENSHOT_SUCCESS);
            }
            KEY_SCREENSHOT_HARDWAREBUFFER => {
                write_parcelable_value(parcel, "android.hardware.HardwareBuffer", |parcel| {
                    write_screenshot_buffer(parcel, frame)
                })?;
            }
            KEY_SCREENSHOT_COLORSPACE => {
                write_parcelable_value(
                    parcel,
                    "android.graphics.ParcelableColorSpace",
                    |parcel| {
                        // `ParcelableColorSpace.writeToParcel`: the `ColorSpace
                        // .getId()` for a named space, and nothing else.
                        parcel.write_int32(COLOR_SPACE_SRGB);
                        Ok(())
                    },
                )?;
            }
            KEY_SCREENSHOT_TIMESTAMP => {
                parcel.write_int32(VAL_LONG);
                parcel.write_int64(capture_time_ms);
            }
            _ => {
                return Err(bad_reply(
                    "takeScreenshot",
                    format!("unexpected bundle key {key}"),
                ));
            }
        }
    }
    backpatch_bundle_length(parcel, start, payload_start)
}
