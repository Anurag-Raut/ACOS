//! The producer side of the real `InputChannel` (I1): create the socketpair,
//! publish framework-layout `InputMessage`s, and read the consumer's
//! `FINISHED`/`TIMELINE` replies.
//!
//! `results/art-runtime/W2.md` made `IWindowSession.addToDisplayAsUser`'s
//! `out InputChannel` byte-exact: `[1][isInitialized][token][String16 name][fd]`.
//! The descriptor in that reply is a duplicate of a real
//! `AF_UNIX`/`SOCK_SEQPACKET` endpoint, so the guest's
//! `InputEventReceiver` can be installed on a real channel
//! (`ViewRootImpl.WindowInputEventReceiver`). What was missing is the **other**
//! end: something has to write `InputMessage`s into that socket in the layout
//! the image's `libinput.so` reads, and consume the replies.
//!
//! This module is that end. It is fd-only and framework-layout-only: nothing
//! here knows about the ACOS renderer or the VM.
//!
//! # Wire layout (image-exact, verified against the image's own library)
//!
//! Source of truth: `InputMessage`/`InputTransport.cpp` in
//! `android-14.0.0_r75` (`include/input/InputTransport.h`) **and** the image's
//! `libinput.so`, which agrees with it on every size and offset used here:
//!
//! * `InputMessage::size()` (`libinput.so:0x77410`) is a jump table over
//!   `Type`; `MOTION` returns `0xa8 + 0x90 * pointerCount` (i.e.
//!   `8` header + `0xa0` fixed body + `0x90` per pointer), `KEY` returns
//!   `0x68`, `FINISHED` `0x18`, `TIMELINE` `0x20`.
//! * `getSanitizedCopy` (`0x775d0`) and `InputPublisher::publishMotionEvent`
//!   (`0x79280`) store `eventTime` at message offset `0x10`, the 32-byte hmac at
//!   `0x24`, `action`/`actionButton`/`flags`/`metaState`/`buttonState` at
//!   `0x44`/`0x48`/`0x4c`/`0x50`/`0x54`, `classification` at `0x58`,
//!   `edgeFlags` at `0x5c`, `downTime` at `0x60`, the six window-transform
//!   floats at `0x68`, precision/cursor at `0x80`, the six raw-transform floats
//!   at `0x90`, and the pointer array at `0xa8` — which is exactly the r75
//!   header order below. (Struct sizes confirm it: the image's
//!   `sizeof(InputMessage)` is `0x9a8` = 8 + 0xa0 + 16 * 0x90, and its
//!   `PointerCoords` is 136 bytes = 8-byte `bits` + 30 floats + resample flag.)
//! * `Type` is `KEY=0, MOTION=1, FINISHED=2, FOCUS=3, CAPTURE=4, DRAG=5,
//!   TIMELINE=6, TOUCH_MODE=7`; the header is `{u32 type, u32 seq}`.
//! * Both directions use `send(..., MSG_DONTWAIT|MSG_NOSIGNAL)` and
//!   `recv(..., MSG_DONTWAIT)`, so the descriptors stay blocking for the guest
//!   (the guest's looper blocks on them) and this side never does.
//! * `InputChannel::openInputChannelPair` (`InputTransport.cpp`) uses
//!   `socketpair(AF_UNIX, SOCK_SEQPACKET, 0, ...)`, names the ends
//!   `"<name> (server)"` / `"<name> (client)"`, and shares one `BBinder` token.
//!   ACOS uses the window's `IWindow` binder id as the token (the same value
//!   `window_session.rs` already writes), and keeps the handle under it in
//!   [`InputChannelRegistry`].
//!
//! `MotionEvent` layout used here (message offsets, little-endian):
//!
//! ```text
//! 0x00 u32 type = 1             0x48 i32 actionButton
//! 0x04 u32 seq                  0x4c i32 flags
//! 0x08 i32 eventId              0x50 i32 metaState
//! 0x0c u32 pointerCount         0x54 i32 buttonState
//! 0x10 i64 eventTime            0x58 u8  classification (+3 pad)
//! 0x18 i32 deviceId             0x5c i32 edgeFlags
//! 0x1c i32 source               0x60 i64 downTime
//! 0x20 i32 displayId            0x68 6 x f32 (dsdx,dtdx,dtdy,dsdy,tx,ty)
//! 0x24 [u8;32] hmac             0x80 4 x f32 (x/yPrecision, x/yCursor)
//! 0x44 i32 action               0x90 6 x f32 raw transform
//!                               0xa8 pointers[pointerCount]
//! ```
//!
//! `Pointers` are `{ i32 id, i32 toolType }` then `PointerCoords`:
//! `{ u64 bits, f32 values[30], u8 isResampled, [u8;7] }` (144 bytes).
//! `values` holds only the axes whose bit is set in `bits`, packed in
//! ascending axis-id order (`PointerCoords::getAxisValue`).
//!
//! # What is real and what is not
//!
//! * Real: the socketpair, the message bytes, sequence allocation, the
//!   `FINISHED`/`TIMELINE` handshake, handled status and dispatch→ack latency.
//! * Not modelled: the `InputDispatcher` policy in front of this (which window
//!   gets the event, focus, ANR). [`InputChannelRegistry`] is keyed by window
//!   token so the `acos-wm`/session layer can pick the target; that wiring is
//!   the remaining step (see `results/art-runtime/I1.md` §6).
//! * `type(text)` is **not** implemented by mapping characters to key codes:
//!   that needs the device's key character map. Key events carry
//!   `unicodeChar` nowhere on the wire (the app resolves it through
//!   `KeyCharacterMap`), so a text-injection API is a separate task; this
//!   module exposes `key(code)` and `key_sequence(&[code])` only.

use std::collections::BTreeMap;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::time::{Duration, Instant};

use acos_parcel::{BinderId, Parcel};
use rustix::net::{RecvFlags, SendFlags, SocketFlags, SocketType};

/// `InputMessage::Type::KEY`.
pub const TYPE_KEY: u32 = 0;
/// `InputMessage::Type::MOTION`.
pub const TYPE_MOTION: u32 = 1;
/// `InputMessage::Type::FINISHED`.
pub const TYPE_FINISHED: u32 = 2;
/// `InputMessage::Type::FOCUS` (never published by this module).
pub const TYPE_FOCUS: u32 = 3;
/// `InputMessage::Type::CAPTURE` (never published by this module).
pub const TYPE_CAPTURE: u32 = 4;
/// `InputMessage::Type::DRAG` (never published by this module).
pub const TYPE_DRAG: u32 = 5;
/// `InputMessage::Type::TIMELINE`.
pub const TYPE_TIMELINE: u32 = 6;
/// `InputMessage::Type::TOUCH_MODE` (never published by this module).
pub const TYPE_TOUCH_MODE: u32 = 7;

/// `sizeof(InputMessage::Header)`.
pub const HEADER_SIZE: usize = 8;
/// Fixed part of a `MOTION` body (before the variable pointer array).
pub const MOTION_FIXED_SIZE: usize = 0xa0;
/// `sizeof(InputMessage::Body::Motion::Pointer)`.
pub const POINTER_SIZE: usize = 0x90;
/// `sizeof(PointerCoords)` on the image.
pub const POINTER_COORDS_SIZE: usize = 136;
/// `PointerCoords::MAX_AXES` on the image (30 so that the struct is 136 bytes).
pub const MAX_AXES: usize = 30;
/// `MAX_POINTERS`: the largest pointer array a real receiver accepts.
pub const MAX_POINTERS: usize = 16;
/// `sizeof(InputMessage)` with `MAX_POINTERS` (the receiver's read buffer).
pub const MAX_MESSAGE_SIZE: usize = HEADER_SIZE + MOTION_FIXED_SIZE + MAX_POINTERS * POINTER_SIZE;
/// `sizeof` of a published `KEY` message.
pub const KEY_MESSAGE_SIZE: usize = 0x68;
/// `sizeof` of a `FOCUS` message (server → consumer). I1 originally recorded
/// `8` from a hand-count of the header; the image's `InputMessage::size()`
/// jump table returns `0x10`, and `publishFocusEvent` writes `eventId` at
/// `0x08` and `hasFocus` at `0x0c` (see `docs/IMAGE_ABI_NOTEBOOK.md`, I2).
pub const FOCUS_MESSAGE_SIZE: usize = 0x10;
/// `sizeof` of a `TOUCH_MODE` message (server → consumer).
pub const TOUCH_MODE_MESSAGE_SIZE: usize = 0x10;
/// `sizeof` of a `FINISHED` message (the consumer's reply).
pub const FINISHED_MESSAGE_SIZE: usize = 0x18;
/// `sizeof` of a `TIMELINE` message (the consumer's reply).
pub const TIMELINE_MESSAGE_SIZE: usize = 0x20;
/// `sizeof(InputMessage::Body::Finished)`.
pub const FINISHED_BODY_SIZE: usize = 0x10;
/// `sizeof(InputMessage::Body::Timeline)`.
pub const TIMELINE_BODY_SIZE: usize = 0x18;

/// `MotionEvent.ACTION_DOWN`.
pub const ACTION_DOWN: i32 = 0;
/// `MotionEvent.ACTION_UP`.
pub const ACTION_UP: i32 = 1;
/// `MotionEvent.ACTION_MOVE`.
pub const ACTION_MOVE: i32 = 2;
/// `MotionEvent.ACTION_CANCEL`.
pub const ACTION_CANCEL: i32 = 3;
/// `MotionEvent.ACTION_OUTSIDE`.
pub const ACTION_OUTSIDE: i32 = 4;
/// `MotionEvent.ACTION_POINTER_DOWN`.
pub const ACTION_POINTER_DOWN: i32 = 5;
/// `MotionEvent.ACTION_POINTER_UP`.
pub const ACTION_POINTER_UP: i32 = 6;
/// `MotionEvent.ACTION_POINTER_INDEX_SHIFT`.
pub const ACTION_POINTER_INDEX_SHIFT: i32 = 8;

/// `KeyEvent.ACTION_DOWN`.
pub const KEY_ACTION_DOWN: i32 = 0;
/// `KeyEvent.ACTION_UP`.
pub const KEY_ACTION_UP: i32 = 1;

/// `InputDevice.SOURCE_TOUCHSCREEN`.
pub const SOURCE_TOUCHSCREEN: i32 = 0x0000_1002;
/// `InputDevice.SOURCE_KEYBOARD`.
pub const SOURCE_KEYBOARD: i32 = 0x0000_0101;

/// `AMOTION_EVENT_AXIS_X`.
pub const AXIS_X: i32 = 0;
/// `AMOTION_EVENT_AXIS_Y`.
pub const AXIS_Y: i32 = 1;
/// `AMOTION_EVENT_AXIS_PRESSURE`.
pub const AXIS_PRESSURE: i32 = 2;
/// `AMOTION_EVENT_AXIS_SIZE`.
pub const AXIS_SIZE: i32 = 3;
/// `AMOTION_EVENT_AXIS_TOUCH_MAJOR`.
pub const AXIS_TOUCH_MAJOR: i32 = 4;
/// `AMOTION_EVENT_AXIS_TOUCH_MINOR`.
pub const AXIS_TOUCH_MINOR: i32 = 5;

/// `KeyEvent.KEYCODE_BACK` — what `performGlobalAction(GLOBAL_ACTION_BACK)`
/// and a scenario `back` publish on the focused window's channel.
pub const KEYCODE_BACK: i32 = 4;
/// `KeyEvent.FLAG_TRACKING` (`0x200`, the same value as the wire format's
/// `AKEY_EVENT_FLAG_TRACKING`).
///
/// The flag is **not** a host-side delivery detail: nothing here sets it. It is
/// decided inside the app process, by the framework's own
/// `KeyEvent.DispatcherState`:
///
/// * on the down, `KeyEvent.dispatch(down, state, target)` calls
///   `receiver.onKeyDown`; when that returns `true` and the handler set
///   `FLAG_START_TRACKING`, `state.startTracking(event, target)` records the
///   key code (`.cache/aosp/base/core/java/android/view/KeyEvent.java:2960-2975`);
/// * `Activity.onKeyDown(KEYCODE_BACK)` is that handler: it calls
///   `event.startTracking()` and returns `true`
///   (`.../android/app/Activity.java:4018-4028`);
/// * on the up, `KeyEvent.dispatch` runs `state.handleUpEvent(event)` *before*
///   `receiver.onKeyUp`, and that sets `FLAG_TRACKING` when the down was tracked
///   (`KeyEvent.java:3088-3100`).
///
/// So `Activity.onKeyUp`'s `event.isTracking()` test
/// (`Activity.java:4117-4136`) is satisfied by a plain up event with no flags,
/// as long as down and up reach the same `DispatcherState` (same window; note
/// `ViewRootImpl.handleWindowFocusChanged` resets it at
/// `ViewRootImpl.java:4359`). This constant documents the value for readers and
/// tests; nothing in this crate puts it on the wire.
pub const KEY_FLAG_TRACKING: i32 = 0x200;
/// `KeyEvent.KEYCODE_ENTER`.
pub const KEYCODE_ENTER: i32 = 66;
/// `KeyEvent.KEYCODE_DEL` (backspace).
pub const KEYCODE_DEL: i32 = 67;
/// `KeyEvent.KEYCODE_TAB`.
pub const KEYCODE_TAB: i32 = 61;
/// `KeyEvent.KEYCODE_SPACE`.
pub const KEYCODE_SPACE: i32 = 62;
/// `KeyEvent.KEYCODE_DPAD_UP`.
pub const KEYCODE_DPAD_UP: i32 = 19;
/// `KeyEvent.KEYCODE_DPAD_DOWN`.
pub const KEYCODE_DPAD_DOWN: i32 = 20;
/// `KeyEvent.KEYCODE_DPAD_LEFT`.
pub const KEYCODE_DPAD_LEFT: i32 = 21;
/// `KeyEvent.KEYCODE_DPAD_RIGHT`.
pub const KEYCODE_DPAD_RIGHT: i32 = 22;
/// `KeyEvent.KEYCODE_DPAD_CENTER`.
pub const KEYCODE_DPAD_CENTER: i32 = 23;

/// Default DOWN→UP hold for a synthesized tap/key (`50 ms`, as `acos-wm` uses).
pub const DEFAULT_TAP_HOLD_NANOS: i64 = 50_000_000;

/// The display refresh cadence an interpolated gesture is published at
/// (`16 ms` ≈ 60 Hz, the `Choreographer` vsync period a real touchscreen
/// samples at).
pub const GESTURE_SAMPLE_INTERVAL_NANOS: i64 = 16_000_000;

/// One sample of an interpolated touch gesture.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GestureSample {
    /// Sample position (window-local, carried in the message transform).
    pub x: f32,
    /// Sample position (window-local).
    pub y: f32,
    /// `eventTime` offset from the stream's `downTime`, in nanoseconds.
    pub offset_nanos: i64,
}

/// Interpolate a swipe/scroll into the samples a real touchscreen would have
/// produced: one sample at the down point, one `MOVE` per display refresh
/// interval (`interval_nanos`, 16 ms by default), and a final sample at the up
/// point whose offset is exactly `duration_nanos`.
///
/// This is the shape the framework's own gesture generators use (equidistant
/// motion events over the gesture duration), and it is what lets
/// `VelocityTracker`/`OverScroller` see a real velocity: the timestamps are
/// monotonic, evenly spaced and end at the lift, so the last few samples carry
/// the gesture's speed. A duration shorter than one refresh interval still
/// produces one interpolated `MOVE` — never a synthetic "up with no motion".
#[must_use]
pub fn swipe_samples(
    from: (f32, f32),
    to: (f32, f32),
    duration_nanos: i64,
    interval_nanos: i64,
) -> Vec<GestureSample> {
    let duration = duration_nanos.max(1);
    let interval = interval_nanos.max(1);
    // At least one MOVE, at most 1000 (a bounded stream the receiver can drain).
    let moves = usize::try_from((duration / interval).max(1)).unwrap_or(1).min(1000);
    let mut samples = Vec::with_capacity(moves + 1);
    samples.push(GestureSample {
        x: from.0,
        y: from.1,
        offset_nanos: 0,
    });
    for i in 1..=moves {
        let t = i as f64 / moves as f64;
        samples.push(GestureSample {
            x: from.0 + (to.0 - from.0) * t as f32,
            y: from.1 + (to.1 - from.1) * t as f32,
            offset_nanos: duration * i as i64 / moves as i64,
        });
    }
    samples
}

// ---------------------------------------------------------------------------
// errors
// ---------------------------------------------------------------------------

/// A structured input-channel failure. Never a silent drop.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ChannelError {
    /// The socketpair/dup/fcntl failed.
    #[error("input channel syscall: {0}")]
    Io(String),
    /// The channel is full (`EAGAIN`, `WOULD_BLOCK` in `libinput`).
    #[error("input channel is full")]
    WouldBlock,
    /// The peer end is gone (`EPIPE`/`ENOTCONN`/`ECONNRESET`).
    #[error("input channel peer is closed")]
    PeerClosed,
    /// The message body is not one this module can decode.
    #[error("malformed input message: {0}")]
    Malformed(&'static str),
    /// A message type this module does not produce/consume.
    #[error("unsupported input message type {0}")]
    UnsupportedType(u32),
    /// A touch at a display point with **no touchable window** under it.
    ///
    /// `InputDispatcher` drops such a gesture (there is nothing to deliver it
    /// to). ACOS names the point instead of publishing it to some other window,
    /// so "the tap vanished" can never look like a delivered tap.
    #[error("no touch target: {0}")]
    NoTouchTarget(String),
    /// `pointerCount` outside `1..=MAX_POINTERS`.
    #[error("bad pointer count {0}")]
    BadPointerCount(usize),
    /// A parcel write failed while writing the `InputChannel` body.
    #[error("input channel parcel: {0}")]
    Parcel(String),
}

type Result<T> = std::result::Result<T, ChannelError>;

fn io_err(what: &str, e: rustix::io::Errno) -> ChannelError {
    match e {
        rustix::io::Errno::AGAIN => ChannelError::WouldBlock,
        rustix::io::Errno::PIPE | rustix::io::Errno::NOTCONN | rustix::io::Errno::CONNRESET => {
            ChannelError::PeerClosed
        }
        other => ChannelError::Io(format!("{what}: {other}")),
    }
}

// ---------------------------------------------------------------------------
// little-endian field helpers
// ---------------------------------------------------------------------------

fn put_u32(buf: &mut [u8], off: usize, v: u32) {
    buf[off..off + 4].copy_from_slice(&v.to_le_bytes());
}

fn put_i32(buf: &mut [u8], off: usize, v: i32) {
    put_u32(buf, off, v as u32);
}

fn put_i64(buf: &mut [u8], off: usize, v: i64) {
    buf[off..off + 8].copy_from_slice(&v.to_le_bytes());
}

fn put_f32(buf: &mut [u8], off: usize, v: f32) {
    buf[off..off + 4].copy_from_slice(&v.to_le_bytes());
}

fn get_u32(buf: &[u8], off: usize) -> Result<u32> {
    let bytes = buf
        .get(off..off + 4)
        .ok_or(ChannelError::Malformed("truncated u32"))?;
    Ok(u32::from_le_bytes(bytes.try_into().unwrap()))
}

fn get_i32(buf: &[u8], off: usize) -> Result<i32> {
    Ok(get_u32(buf, off)? as i32)
}

fn get_i64(buf: &[u8], off: usize) -> Result<i64> {
    let bytes = buf
        .get(off..off + 8)
        .ok_or(ChannelError::Malformed("truncated i64"))?;
    Ok(i64::from_le_bytes(bytes.try_into().unwrap()))
}

fn get_f32(buf: &[u8], off: usize) -> Result<f32> {
    Ok(f32::from_bits(get_u32(buf, off)?))
}

// ---------------------------------------------------------------------------
// pointer / motion / key payloads
// ---------------------------------------------------------------------------

/// `PointerProperties`; `toolType` is `ToolType::FINGER` for a synthetic touch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PointerProperties {
    /// `PointerProperties.id` (0 for a single pointer).
    pub id: i32,
    /// `PointerProperties.toolType` (1 = `FINGER`).
    pub tool_type: i32,
}

impl Default for PointerProperties {
    fn default() -> Self {
        Self {
            id: 0,
            tool_type: 1,
        }
    }
}

/// `PointerCoords`: a 30-bit axis mask plus the packed axis values.
///
/// Only the axes whose bit is set are stored, packed in ascending axis order —
/// exactly what `PointerCoords::getAxisValue` (and therefore
/// `MotionEvent.getX/getY/getPressure`) reads.
#[derive(Debug, Clone, PartialEq)]
pub struct PointerCoords {
    /// Axis bits (`1 << axis`), 30 bits used.
    pub bits: u64,
    /// Packed values for the set bits (unused tail is zero).
    pub values: [f32; MAX_AXES],
    /// `PointerCoords.isResampled` (false for an injected event).
    pub is_resampled: bool,
}

impl PointerCoords {
    /// A finger at `(x, y)` with pressure 1 and a small touch size.
    ///
    /// Axes set: `X, Y, PRESSURE, SIZE, TOUCH_MAJOR, TOUCH_MINOR` (bits
    /// `0b111111`), packed in that order. That is the minimum set a real
    /// `View` reads (`getX`, `getY`, `getPressure`, `getSize`,
    /// `getTouchMajor/Minor`).
    #[must_use]
    pub fn touch(x: f32, y: f32) -> Self {
        Self::with_axes(&[
            (AXIS_X, x),
            (AXIS_Y, y),
            (AXIS_PRESSURE, 1.0),
            (AXIS_SIZE, 0.05),
            (AXIS_TOUCH_MAJOR, 0.1),
            (AXIS_TOUCH_MINOR, 0.1),
        ])
    }

    /// Build from `(axis, value)` pairs. Duplicate axes keep the last value.
    #[must_use]
    pub fn with_axes(axes: &[(i32, f32)]) -> Self {
        let mut bits = 0u64;
        for (axis, _) in axes {
            if let Ok(shift) = u32::try_from(*axis) {
                if (shift as usize) < MAX_AXES {
                    bits |= 1u64 << shift;
                }
            }
        }
        let mut values = [0.0f32; MAX_AXES];
        let mut index = 0;
        for axis in 0..MAX_AXES {
            if bits & (1u64 << axis) == 0 {
                continue;
            }
            if let Some((_, value)) = axes
                .iter()
                .rev()
                .find(|(candidate, _)| *candidate == axis as i32)
            {
                values[index] = *value;
            }
            index += 1;
        }
        Self {
            bits,
            values,
            is_resampled: false,
        }
    }

    /// `PointerCoords::getAxisValue` (0 when the axis is absent).
    #[must_use]
    pub fn get(&self, axis: i32) -> f32 {
        let Ok(axis) = u32::try_from(axis) else {
            return 0.0;
        };
        if axis as usize >= MAX_AXES || self.bits & (1u64 << axis) == 0 {
            return 0.0;
        }
        let index = (0..axis).filter(|i| self.bits & (1u64 << i) != 0).count();
        self.values[index]
    }

    fn encode(&self, out: &mut [u8], off: usize) {
        out[off..off + 8].copy_from_slice(&self.bits.to_le_bytes());
        for (i, value) in self.values.iter().enumerate() {
            put_f32(out, off + 8 + i * 4, *value);
        }
        out[off + POINTER_COORDS_SIZE - 8] = u8::from(self.is_resampled);
    }

    fn decode(buf: &[u8], off: usize) -> Result<Self> {
        let bits = get_i64(buf, off)? as u64;
        let mut values = [0.0f32; MAX_AXES];
        for (i, slot) in values.iter_mut().enumerate() {
            *slot = get_f32(buf, off + 8 + i * 4)?;
        }
        Ok(Self {
            bits,
            values,
            is_resampled: buf[off + POINTER_COORDS_SIZE - 8] != 0,
        })
    }
}

/// One pointer entry: properties then coordinates (144 bytes on the wire).
#[derive(Debug, Clone, PartialEq)]
pub struct Pointer {
    /// `PointerProperties`.
    pub properties: PointerProperties,
    /// `PointerCoords`.
    pub coords: PointerCoords,
}

/// A `ui::Transform` row-major 2x3 (`dsdx dtdy tx / dtdx dsdy ty` in the
/// message's field order).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Transform {
    /// `dsdx`.
    pub dsdx: f32,
    /// `dtdx`.
    pub dtdx: f32,
    /// `dtdy`.
    pub dtdy: f32,
    /// `dsdy`.
    pub dsdy: f32,
    /// `tx`.
    pub tx: f32,
    /// `ty`.
    pub ty: f32,
}

impl Transform {
    /// The identity transform (no scaling, no offset).
    #[must_use]
    pub fn identity() -> Self {
        Self {
            dsdx: 1.0,
            dtdx: 0.0,
            dtdy: 0.0,
            dsdy: 1.0,
            tx: 0.0,
            ty: 0.0,
        }
    }

    fn encode(&self, out: &mut [u8], off: usize) {
        for (i, v) in [self.dsdx, self.dtdx, self.dtdy, self.dsdy, self.tx, self.ty]
            .iter()
            .enumerate()
        {
            put_f32(out, off + i * 4, *v);
        }
    }

    fn decode(buf: &[u8], off: usize) -> Result<Self> {
        Ok(Self {
            dsdx: get_f32(buf, off)?,
            dtdx: get_f32(buf, off + 4)?,
            dtdy: get_f32(buf, off + 8)?,
            dsdy: get_f32(buf, off + 12)?,
            tx: get_f32(buf, off + 16)?,
            ty: get_f32(buf, off + 20)?,
        })
    }
}

/// A motion event as it goes on the wire (not the `acos-wm` policy type).
#[derive(Debug, Clone, PartialEq)]
pub struct MotionEvent {
    /// `eventId` (`InputDispatcher`'s event id; unique per motion stream).
    pub event_id: i32,
    /// `MotionEvent.getDeviceId()`.
    pub device_id: i32,
    /// `MotionEvent.getSource()`.
    pub source: i32,
    /// `MotionEvent.getDisplayId()`.
    pub display_id: i32,
    /// `MotionEvent.getAction()` (masked action + index bits).
    pub action: i32,
    /// `MotionEvent.getActionButton()`.
    pub action_button: i32,
    /// `MotionEvent.getFlags()`.
    pub flags: i32,
    /// `MotionEvent.getEdgeFlags()`.
    pub edge_flags: i32,
    /// `MotionEvent.getMetaState()`.
    pub meta_state: i32,
    /// `MotionEvent.getButtonState()`.
    pub button_state: i32,
    /// `MotionClassification` (0 = `NONE`).
    pub classification: u8,
    /// `MotionEvent.getDownTime()` (`CLOCK_MONOTONIC` ns).
    pub down_time: i64,
    /// `MotionEvent.getEventTime()` (`CLOCK_MONOTONIC` ns).
    pub event_time: i64,
    /// `MotionEvent.getXPrecision()`.
    pub x_precision: f32,
    /// `MotionEvent.getYPrecision()`.
    pub y_precision: f32,
    /// Window-transform cursor offset X.
    pub x_cursor_position: f32,
    /// Window-transform cursor offset Y.
    pub y_cursor_position: f32,
    /// Input-verifier hmac (zeros for an ACOS-injected event).
    pub hmac: [u8; 32],
    /// The window transform (identity for a synthesized event).
    pub transform: Transform,
    /// The raw transform (identity for a synthesized event).
    pub raw_transform: Transform,
    /// The pointer array.
    pub pointers: Vec<Pointer>,
}

impl MotionEvent {
    /// A touchscreen event with the fields `acos-wm`/`View` read.
    #[must_use]
    pub fn touch(
        event_id: i32,
        action: i32,
        down_time: i64,
        event_time: i64,
        x: f32,
        y: f32,
    ) -> Self {
        Self {
            event_id,
            device_id: 1,
            source: SOURCE_TOUCHSCREEN,
            display_id: 0,
            action,
            action_button: 0,
            flags: 0,
            edge_flags: 0,
            meta_state: 0,
            button_state: 0,
            classification: 0,
            down_time,
            event_time,
            x_precision: 1.0,
            y_precision: 1.0,
            x_cursor_position: 0.0,
            y_cursor_position: 0.0,
            hmac: [0; 32],
            transform: Transform::identity(),
            raw_transform: Transform::identity(),
            pointers: vec![Pointer {
                properties: PointerProperties::default(),
                coords: PointerCoords::touch(x, y),
            }],
        }
    }

    /// The x of the pointer at `pointer_index` (0 when absent).
    #[must_use]
    pub fn x(&self, pointer_index: usize) -> f32 {
        self.pointers
            .get(pointer_index)
            .map_or(0.0, |p| p.coords.get(AXIS_X))
    }

    /// The y of the pointer at `pointer_index` (0 when absent).
    #[must_use]
    pub fn y(&self, pointer_index: usize) -> f32 {
        self.pointers
            .get(pointer_index)
            .map_or(0.0, |p| p.coords.get(AXIS_Y))
    }
}

/// A key event as it goes on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyEvent {
    /// `eventId`.
    pub event_id: i32,
    /// `KeyEvent.getDeviceId()`.
    pub device_id: i32,
    /// `KeyEvent.getSource()`.
    pub source: i32,
    /// `KeyEvent.getDisplayId()`.
    pub display_id: i32,
    /// `KeyEvent.getAction()` (`0` down, `1` up).
    pub action: i32,
    /// `KeyEvent.getFlags()`.
    pub flags: i32,
    /// `KeyEvent.getKeyCode()`.
    pub key_code: i32,
    /// `KeyEvent.getScanCode()`.
    pub scan_code: i32,
    /// `KeyEvent.getMetaState()`.
    pub meta_state: i32,
    /// `KeyEvent.getRepeatCount()`.
    pub repeat_count: i32,
    /// `KeyEvent.getDownTime()` (`CLOCK_MONOTONIC` ns).
    pub down_time: i64,
    /// `KeyEvent.getEventTime()` (`CLOCK_MONOTONIC` ns).
    pub event_time: i64,
    /// Input-verifier hmac (zeros for an ACOS-injected event).
    pub hmac: [u8; 32],
}

impl KeyEvent {
    /// A keyboard event for `key_code`.
    #[must_use]
    pub fn new(event_id: i32, action: i32, key_code: i32, down_time: i64, event_time: i64) -> Self {
        Self {
            event_id,
            device_id: 1,
            source: SOURCE_KEYBOARD,
            display_id: 0,
            action,
            flags: 0,
            key_code,
            scan_code: 0,
            meta_state: 0,
            repeat_count: 0,
            down_time,
            event_time,
            hmac: [0; 32],
        }
    }
}

// ---------------------------------------------------------------------------
// messages
// ---------------------------------------------------------------------------

/// One framed `InputMessage` in either direction.
#[derive(Debug, Clone, PartialEq)]
pub enum InputMessage {
    /// A published motion event.
    Motion {
        /// Channel sequence number (non-zero).
        seq: u32,
        /// The event body.
        event: MotionEvent,
    },
    /// A published key event.
    Key {
        /// Channel sequence number.
        seq: u32,
        /// The event body.
        event: KeyEvent,
    },
    /// A focus change published by the dispatcher (`InputPublisher::
    /// publishFocusEvent`). The receiver turns it into
    /// `InputEventReceiver.onFocusEvent` → `ViewRootImpl.windowFocusChanged`;
    /// it is **not** acknowledged with `FINISHED`.
    Focus {
        /// Channel sequence number.
        seq: u32,
        /// `FocusEvent.eventId`.
        event_id: i32,
        /// `FocusEvent.hasFocus`.
        has_focus: bool,
    },
    /// A touch-mode change (`publishTouchModeEvent`).
    TouchMode {
        /// Channel sequence number.
        seq: u32,
        /// `TouchModeEvent.eventId`.
        event_id: i32,
        /// `TouchModeEvent.inTouchMode`.
        in_touch_mode: bool,
    },
    /// The consumer's acknowledgement of one sequence.
    Finished {
        /// The sequence being acknowledged.
        seq: u32,
        /// `handled` (`InputEventReceiver`'s dispatch result).
        handled: bool,
        /// `consumeTime`, on the same `CLOCK_MONOTONIC` base as event times.
        consume_time: i64,
    },
    /// The consumer's graphics-timeline report.
    Timeline {
        /// The sequence the timeline belongs to.
        seq: u32,
        /// `eventId` of the input message the timeline belongs to.
        event_id: i32,
        /// `GraphicsTimeline` (`presentTime`, `frameTime`).
        graphics_timeline: [i64; 2],
    },
}

/// Total wire size of a `MOTION` message with `pointer_count` pointers.
#[must_use]
pub fn motion_message_size(pointer_count: usize) -> usize {
    HEADER_SIZE + MOTION_FIXED_SIZE + pointer_count * POINTER_SIZE
}

impl InputMessage {
    /// The sequence number carried in the header (`0` for a decoded unknown).
    #[must_use]
    pub fn seq(&self) -> u32 {
        match self {
            Self::Motion { seq, .. }
            | Self::Key { seq, .. }
            | Self::Focus { seq, .. }
            | Self::TouchMode { seq, .. }
            | Self::Finished { seq, .. }
            | Self::Timeline { seq, .. } => *seq,
        }
    }

    /// Encode into the image's layout. The returned length is exactly
    /// `InputMessage::size()` (`0xa8 + 0x90*n` for motion, `0x68` for key,
    /// `0x18` for finished, `0x20` for timeline).
    pub fn encode(&self) -> Result<Vec<u8>> {
        match self {
            Self::Motion { seq, event } => encode_motion(*seq, event),
            Self::Key { seq, event } => Ok(encode_key(*seq, event)),
            Self::Focus {
                seq,
                event_id,
                has_focus,
            } => Ok(encode_focus(*seq, *event_id, *has_focus)),
            Self::TouchMode {
                seq,
                event_id,
                in_touch_mode,
            } => Ok(encode_touch_mode(*seq, *event_id, *in_touch_mode)),
            Self::Finished {
                seq,
                handled,
                consume_time,
            } => Ok(encode_finished(*seq, *handled, *consume_time)),
            Self::Timeline {
                seq,
                event_id,
                graphics_timeline,
            } => Ok(encode_timeline(*seq, *event_id, *graphics_timeline)),
        }
    }

    /// Decode one message, rejecting a truncated or mis-sized body. Used by
    /// the tests, by [`loopback_consumer`], and by
    /// [`InputPublisher::receive_consumer_response`].
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        if bytes.len() < HEADER_SIZE {
            return Err(ChannelError::Malformed("shorter than the header"));
        }
        let kind = get_u32(bytes, 0)?;
        let seq = get_u32(bytes, 4)?;
        match kind {
            TYPE_MOTION => {
                let pointer_count = get_u32(bytes, 0xc)? as usize;
                if pointer_count == 0 || pointer_count > MAX_POINTERS {
                    return Err(ChannelError::BadPointerCount(pointer_count));
                }
                let expected = motion_message_size(pointer_count);
                if bytes.len() != expected {
                    return Err(ChannelError::Malformed("motion size mismatch"));
                }
                let mut hmac = [0u8; 32];
                hmac.copy_from_slice(&bytes[0x24..0x44]);
                let mut pointers = Vec::with_capacity(pointer_count);
                for i in 0..pointer_count {
                    let off = 0xa8 + i * POINTER_SIZE;
                    pointers.push(Pointer {
                        properties: PointerProperties {
                            id: get_i32(bytes, off)?,
                            tool_type: get_i32(bytes, off + 4)?,
                        },
                        coords: PointerCoords::decode(bytes, off + 8)?,
                    });
                }
                Ok(Self::Motion {
                    seq,
                    event: MotionEvent {
                        event_id: get_i32(bytes, 0x08)?,
                        device_id: get_i32(bytes, 0x18)?,
                        source: get_i32(bytes, 0x1c)?,
                        display_id: get_i32(bytes, 0x20)?,
                        action: get_i32(bytes, 0x44)?,
                        action_button: get_i32(bytes, 0x48)?,
                        flags: get_i32(bytes, 0x4c)?,
                        edge_flags: get_i32(bytes, 0x5c)?,
                        meta_state: get_i32(bytes, 0x50)?,
                        button_state: get_i32(bytes, 0x54)?,
                        classification: bytes[0x58],
                        down_time: get_i64(bytes, 0x60)?,
                        event_time: get_i64(bytes, 0x10)?,
                        x_precision: get_f32(bytes, 0x80)?,
                        y_precision: get_f32(bytes, 0x84)?,
                        x_cursor_position: get_f32(bytes, 0x88)?,
                        y_cursor_position: get_f32(bytes, 0x8c)?,
                        hmac,
                        transform: Transform::decode(bytes, 0x68)?,
                        raw_transform: Transform::decode(bytes, 0x90)?,
                        pointers,
                    },
                })
            }
            TYPE_KEY => {
                if bytes.len() != KEY_MESSAGE_SIZE {
                    return Err(ChannelError::Malformed("key size mismatch"));
                }
                let mut hmac = [0u8; 32];
                hmac.copy_from_slice(&bytes[0x24..0x44]);
                Ok(Self::Key {
                    seq,
                    event: KeyEvent {
                        event_id: get_i32(bytes, 0x08)?,
                        device_id: get_i32(bytes, 0x18)?,
                        source: get_i32(bytes, 0x1c)?,
                        display_id: get_i32(bytes, 0x20)?,
                        action: get_i32(bytes, 0x44)?,
                        flags: get_i32(bytes, 0x48)?,
                        key_code: get_i32(bytes, 0x4c)?,
                        scan_code: get_i32(bytes, 0x50)?,
                        meta_state: get_i32(bytes, 0x54)?,
                        repeat_count: get_i32(bytes, 0x58)?,
                        down_time: get_i64(bytes, 0x60)?,
                        event_time: get_i64(bytes, 0x10)?,
                        hmac,
                    },
                })
            }
            TYPE_FOCUS => {
                if bytes.len() != FOCUS_MESSAGE_SIZE {
                    return Err(ChannelError::Malformed("focus size mismatch"));
                }
                Ok(Self::Focus {
                    seq,
                    event_id: get_i32(bytes, 0x08)?,
                    has_focus: bytes[0x0c] != 0,
                })
            }
            TYPE_TOUCH_MODE => {
                if bytes.len() != TOUCH_MODE_MESSAGE_SIZE {
                    return Err(ChannelError::Malformed("touch-mode size mismatch"));
                }
                Ok(Self::TouchMode {
                    seq,
                    event_id: get_i32(bytes, 0x08)?,
                    in_touch_mode: bytes[0x0c] != 0,
                })
            }
            TYPE_FINISHED => {
                if bytes.len() != FINISHED_MESSAGE_SIZE {
                    return Err(ChannelError::Malformed("finished size mismatch"));
                }
                Ok(Self::Finished {
                    seq,
                    handled: bytes[0x08] != 0,
                    consume_time: get_i64(bytes, 0x10)?,
                })
            }
            TYPE_TIMELINE => {
                if bytes.len() != TIMELINE_MESSAGE_SIZE {
                    return Err(ChannelError::Malformed("timeline size mismatch"));
                }
                Ok(Self::Timeline {
                    seq,
                    event_id: get_i32(bytes, 0x08)?,
                    graphics_timeline: [get_i64(bytes, 0x10)?, get_i64(bytes, 0x18)?],
                })
            }
            other => Err(ChannelError::UnsupportedType(other)),
        }
    }
}

fn encode_motion(seq: u32, event: &MotionEvent) -> Result<Vec<u8>> {
    let pointer_count = event.pointers.len();
    if pointer_count == 0 || pointer_count > MAX_POINTERS {
        return Err(ChannelError::BadPointerCount(pointer_count));
    }
    let mut buf = vec![0u8; motion_message_size(pointer_count)];
    put_u32(&mut buf, 0x00, TYPE_MOTION);
    put_u32(&mut buf, 0x04, seq);
    put_i32(&mut buf, 0x08, event.event_id);
    put_u32(&mut buf, 0x0c, pointer_count as u32);
    put_i64(&mut buf, 0x10, event.event_time);
    put_i32(&mut buf, 0x18, event.device_id);
    put_i32(&mut buf, 0x1c, event.source);
    put_i32(&mut buf, 0x20, event.display_id);
    buf[0x24..0x44].copy_from_slice(&event.hmac);
    put_i32(&mut buf, 0x44, event.action);
    put_i32(&mut buf, 0x48, event.action_button);
    put_i32(&mut buf, 0x4c, event.flags);
    put_i32(&mut buf, 0x50, event.meta_state);
    put_i32(&mut buf, 0x54, event.button_state);
    buf[0x58] = event.classification;
    put_i32(&mut buf, 0x5c, event.edge_flags);
    put_i64(&mut buf, 0x60, event.down_time);
    event.transform.encode(&mut buf, 0x68);
    put_f32(&mut buf, 0x80, event.x_precision);
    put_f32(&mut buf, 0x84, event.y_precision);
    put_f32(&mut buf, 0x88, event.x_cursor_position);
    put_f32(&mut buf, 0x8c, event.y_cursor_position);
    event.raw_transform.encode(&mut buf, 0x90);
    for (i, pointer) in event.pointers.iter().enumerate() {
        let off = 0xa8 + i * POINTER_SIZE;
        put_i32(&mut buf, off, pointer.properties.id);
        put_i32(&mut buf, off + 4, pointer.properties.tool_type);
        pointer.coords.encode(&mut buf, off + 8);
    }
    Ok(buf)
}

fn encode_key(seq: u32, event: &KeyEvent) -> Vec<u8> {
    let mut buf = vec![0u8; KEY_MESSAGE_SIZE];
    put_u32(&mut buf, 0x00, TYPE_KEY);
    put_u32(&mut buf, 0x04, seq);
    put_i32(&mut buf, 0x08, event.event_id);
    put_i32(&mut buf, 0x0c, 0); // empty1
    put_i64(&mut buf, 0x10, event.event_time);
    put_i32(&mut buf, 0x18, event.device_id);
    put_i32(&mut buf, 0x1c, event.source);
    put_i32(&mut buf, 0x20, event.display_id);
    buf[0x24..0x44].copy_from_slice(&event.hmac);
    put_i32(&mut buf, 0x44, event.action);
    put_i32(&mut buf, 0x48, event.flags);
    put_i32(&mut buf, 0x4c, event.key_code);
    put_i32(&mut buf, 0x50, event.scan_code);
    put_i32(&mut buf, 0x54, event.meta_state);
    put_i32(&mut buf, 0x58, event.repeat_count);
    put_i32(&mut buf, 0x5c, 0); // empty2
    put_i64(&mut buf, 0x60, event.down_time);
    buf
}

fn encode_focus(seq: u32, event_id: i32, has_focus: bool) -> Vec<u8> {
    let mut buf = vec![0u8; FOCUS_MESSAGE_SIZE];
    put_u32(&mut buf, 0x00, TYPE_FOCUS);
    put_u32(&mut buf, 0x04, seq);
    put_i32(&mut buf, 0x08, event_id);
    buf[0x0c] = u8::from(has_focus);
    buf
}

fn encode_touch_mode(seq: u32, event_id: i32, in_touch_mode: bool) -> Vec<u8> {
    let mut buf = vec![0u8; TOUCH_MODE_MESSAGE_SIZE];
    put_u32(&mut buf, 0x00, TYPE_TOUCH_MODE);
    put_u32(&mut buf, 0x04, seq);
    put_i32(&mut buf, 0x08, event_id);
    buf[0x0c] = u8::from(in_touch_mode);
    buf
}

fn encode_finished(seq: u32, handled: bool, consume_time: i64) -> Vec<u8> {
    let mut buf = vec![0u8; FINISHED_MESSAGE_SIZE];
    put_u32(&mut buf, 0x00, TYPE_FINISHED);
    put_u32(&mut buf, 0x04, seq);
    buf[0x08] = u8::from(handled);
    put_i64(&mut buf, 0x10, consume_time);
    buf
}

fn encode_timeline(seq: u32, event_id: i32, timeline: [i64; 2]) -> Vec<u8> {
    let mut buf = vec![0u8; TIMELINE_MESSAGE_SIZE];
    put_u32(&mut buf, 0x00, TYPE_TIMELINE);
    put_u32(&mut buf, 0x04, seq);
    put_i32(&mut buf, 0x08, event_id);
    put_i32(&mut buf, 0x0c, 0); // empty
    put_i64(&mut buf, 0x10, timeline[0]);
    put_i64(&mut buf, 0x18, timeline[1]);
    buf
}

// ---------------------------------------------------------------------------
// clock
// ---------------------------------------------------------------------------

/// `CLOCK_MONOTONIC` in nanoseconds — the base of `MotionEvent.eventTime`,
/// `KeyEvent.eventTime` and `InputMessage::Finished.consumeTime`.
#[must_use]
pub fn monotonic_nanos() -> i64 {
    let ts = rustix::time::clock_gettime(rustix::time::ClockId::Monotonic);
    ts.tv_sec * 1_000_000_000 + ts.tv_nsec
}

// ---------------------------------------------------------------------------
// publisher
// ---------------------------------------------------------------------------

/// What kind of event one sequence carried.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DispatchKind {
    /// `ACTION_DOWN`.
    MotionDown,
    /// `ACTION_MOVE`.
    MotionMove,
    /// `ACTION_UP`.
    MotionUp,
    /// `ACTION_CANCEL`.
    MotionCancel,
    /// `KeyEvent.ACTION_DOWN`.
    KeyDown,
    /// `KeyEvent.ACTION_UP`.
    KeyUp,
}

/// One published sequence, as the caller can report it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Dispatch {
    /// Channel sequence.
    pub seq: u32,
    /// `eventId` of the event.
    pub event_id: i32,
    /// `downTime` (ns, `CLOCK_MONOTONIC`).
    pub down_time: i64,
    /// `eventTime` (ns, `CLOCK_MONOTONIC`).
    pub event_time: i64,
    /// What was published.
    pub kind: DispatchKind,
}

/// The consumer's acknowledgement of one sequence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ack {
    /// The acknowledged sequence.
    pub seq: u32,
    /// `handled` — the guest's `InputEventReceiver` result.
    pub handled: bool,
    /// `consumeTime` reported by the consumer (ns, `CLOCK_MONOTONIC`).
    pub consume_time: i64,
    /// Wall time from `publish` to receiving `FINISHED` (`None` when the
    /// sequence was not one this publisher tracked, e.g. a stale ack).
    pub latency: Option<Duration>,
}

/// A `TIMELINE` reply from the guest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimelineReport {
    /// The sequence the timeline belongs to.
    pub seq: u32,
    /// `eventId` of the input message.
    pub event_id: i32,
    /// `GraphicsTimeline` (`presentTime`, `frameTime`).
    pub graphics_timeline: [i64; 2],
}

/// A consumer reply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConsumerResponse {
    /// `InputMessage::Type::FINISHED` with the caller's bookkeeping attached.
    Finished(Ack),
    /// `InputMessage::Type::TIMELINE`.
    Timeline(TimelineReport),
}

/// The server end of one window's input channel: publish events, read acks.
///
/// `seq` starts at 1 (`publish*` rejects 0, like `InputPublisher`), and
/// `eventId` is a separate monotonically increasing id shared by the motion and
/// key streams of this channel (the framework only requires it to be unique per
/// stream; one counter keeps the trace readable).
#[derive(Debug)]
pub struct InputPublisher {
    fd: OwnedFd,
    name: String,
    next_seq: u32,
    next_event_id: i32,
    pending: BTreeMap<u32, (DispatchKind, Instant)>,
    acks: Vec<Ack>,
    last_timeline: Option<TimelineReport>,
}

impl InputPublisher {
    /// Wrap the server end of an existing pair.
    #[must_use]
    pub fn new(fd: OwnedFd, name: impl Into<String>) -> Self {
        Self {
            fd,
            name: name.into(),
            next_seq: 1,
            next_event_id: 1,
            pending: BTreeMap::new(),
            acks: Vec::new(),
            last_timeline: None,
        }
    }

    /// The channel name (`"<window> (server)"`).
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The server descriptor (for `poll`/`epoll` registration).
    #[must_use]
    pub fn fd(&self) -> BorrowedFd<'_> {
        self.fd.as_fd()
    }

    /// The next sequence that will be published.
    #[must_use]
    pub fn next_seq(&self) -> u32 {
        self.next_seq
    }

    /// Sequences published but not yet acknowledged.
    pub fn pending_seqs(&self) -> impl Iterator<Item = u32> + '_ {
        self.pending.keys().copied()
    }

    /// Number of unacknowledged sequences.
    #[must_use]
    pub fn pending_len(&self) -> usize {
        self.pending.len()
    }

    /// Every acknowledgement received so far, in arrival order.
    #[must_use]
    pub fn acks(&self) -> &[Ack] {
        &self.acks
    }

    /// The most recent `TIMELINE` reply, if any.
    #[must_use]
    pub fn last_timeline(&self) -> Option<TimelineReport> {
        self.last_timeline
    }

    fn alloc_seq(&mut self) -> Result<u32> {
        let seq = self.next_seq;
        if seq == 0 {
            return Err(ChannelError::Io("sequence number overflowed".into()));
        }
        self.next_seq = seq.wrapping_add(1);
        Ok(seq)
    }

    fn alloc_event_id(&mut self) -> i32 {
        let id = self.next_event_id;
        self.next_event_id = self.next_event_id.wrapping_add(1).max(1);
        id
    }

    /// Allocate an `eventId` for a body this caller encodes itself (the
    /// session's transform-carried tap). Same counter the `tap`/`key` helpers
    /// use, so ids stay unique per channel.
    pub fn allocate_event_id(&mut self) -> i32 {
        self.alloc_event_id()
    }

    /// `InputChannel::sendMessage`: one `SOCK_SEQPACKET` datagram, non-blocking,
    /// `MSG_NOSIGNAL`, and the buffer the consumer will read (`0x9a8` max).
    pub fn publish(&mut self, message: &InputMessage) -> Result<()> {
        let bytes = message.encode()?;
        if bytes.len() > MAX_MESSAGE_SIZE {
            return Err(ChannelError::Malformed("message larger than InputMessage"));
        }
        let sent = rustix::net::send(&self.fd, &bytes, SendFlags::DONTWAIT | SendFlags::NOSIGNAL)
            .map_err(|e| io_err("send", e))?;
        if sent != bytes.len() {
            // `InputChannel::sendMessage` treats a short seqpacket write as a
            // dead object (it can only happen on a broken channel).
            return Err(ChannelError::PeerClosed);
        }
        Ok(())
    }

    fn track(&mut self, dispatch: Dispatch) {
        self.pending
            .insert(dispatch.seq, (dispatch.kind, Instant::now()));
    }

    /// Publish a motion event with an explicit wire body.
    pub fn publish_motion(&mut self, event: &MotionEvent) -> Result<Dispatch> {
        let seq = self.alloc_seq()?;
        let kind = match event.action & 0xff {
            ACTION_DOWN => DispatchKind::MotionDown,
            ACTION_UP => DispatchKind::MotionUp,
            ACTION_MOVE => DispatchKind::MotionMove,
            ACTION_CANCEL => DispatchKind::MotionCancel,
            _ => DispatchKind::MotionMove,
        };
        self.publish(&InputMessage::Motion {
            seq,
            event: event.clone(),
        })?;
        let dispatch = Dispatch {
            seq,
            event_id: event.event_id,
            down_time: event.down_time,
            event_time: event.event_time,
            kind,
        };
        self.track(dispatch);
        Ok(dispatch)
    }

    /// Publish a key event with an explicit wire body.
    pub fn publish_key(&mut self, event: &KeyEvent) -> Result<Dispatch> {
        let seq = self.alloc_seq()?;
        let kind = if event.action == KEY_ACTION_DOWN {
            DispatchKind::KeyDown
        } else {
            DispatchKind::KeyUp
        };
        self.publish(&InputMessage::Key { seq, event: *event })?;
        let dispatch = Dispatch {
            seq,
            event_id: event.event_id,
            down_time: event.down_time,
            event_time: event.event_time,
            kind,
        };
        self.track(dispatch);
        Ok(dispatch)
    }

    /// `ACTION_DOWN` at `(x, y)` with an explicit clock value. `down_time` is
    /// the stream's down time returned in [`Gesture`] for the matching `UP`.
    pub fn down_at(&mut self, x: f32, y: f32, down_time: i64, event_time: i64) -> Result<Dispatch> {
        let event_id = self.alloc_event_id();
        let event = MotionEvent::touch(event_id, ACTION_DOWN, down_time, event_time, x, y);
        self.publish_motion(&event)
    }

    /// `ACTION_MOVE` in an existing stream.
    pub fn move_at(&mut self, x: f32, y: f32, down_time: i64, event_time: i64) -> Result<Dispatch> {
        let event_id = self.alloc_event_id();
        let event = MotionEvent::touch(event_id, ACTION_MOVE, down_time, event_time, x, y);
        self.publish_motion(&event)
    }

    /// `ACTION_UP` in an existing stream.
    pub fn up_at(&mut self, x: f32, y: f32, down_time: i64, event_time: i64) -> Result<Dispatch> {
        let event_id = self.alloc_event_id();
        let event = MotionEvent::touch(event_id, ACTION_UP, down_time, event_time, x, y);
        self.publish_motion(&event)
    }

    /// A tap (`DOWN` + `UP`) starting at `down_time_ns`, holding for
    /// `hold_ns`. The two events share `downTime`; `UP.eventTime` is
    /// `down_time_ns + hold_ns`. Sequences are consecutive.
    pub fn tap_at(&mut self, x: f32, y: f32, down_time_ns: i64, hold_ns: i64) -> Result<Gesture> {
        let down = self.down_at(x, y, down_time_ns, down_time_ns)?;
        let up = self.up_at(x, y, down_time_ns, down_time_ns + hold_ns.max(0))?;
        Ok(Gesture {
            dispatches: vec![down, up],
        })
    }

    /// A tap on `CLOCK_MONOTONIC` now, with [`DEFAULT_TAP_HOLD_NANOS`].
    pub fn tap(&mut self, x: f32, y: f32) -> Result<Gesture> {
        self.tap_at(x, y, monotonic_nanos(), DEFAULT_TAP_HOLD_NANOS)
    }

    /// A key press (`DOWN` + `UP`) with an explicit clock value.
    pub fn key_at(&mut self, key_code: i32, down_time_ns: i64, hold_ns: i64) -> Result<Gesture> {
        let down_id = self.alloc_event_id();
        let down = KeyEvent::new(
            down_id,
            KEY_ACTION_DOWN,
            key_code,
            down_time_ns,
            down_time_ns,
        );
        let up_id = self.alloc_event_id();
        let up = KeyEvent::new(
            up_id,
            KEY_ACTION_UP,
            key_code,
            down_time_ns,
            down_time_ns + hold_ns.max(0),
        );
        // No `FLAG_TRACKING` on the wire (see [`KEY_FLAG_TRACKING`]): the app's
        // own `KeyEvent.DispatcherState` sets it on the up because
        // `Activity.onKeyDown` tracked the down, exactly as on a phone. The down
        // must therefore be a first press (`repeat_count == 0`, no flags) so
        // `KeyEvent.dispatch` starts tracking it.
        Ok(Gesture {
            dispatches: vec![self.publish_key(&down)?, self.publish_key(&up)?],
        })
    }

    /// A key press on `CLOCK_MONOTONIC` now.
    pub fn key(&mut self, key_code: i32) -> Result<Gesture> {
        self.key_at(key_code, monotonic_nanos(), DEFAULT_TAP_HOLD_NANOS)
    }

    /// `InputPublisher::publishFocusEvent`: announce a focus change.
    ///
    /// Focus is not acknowledged (`InputEventReceiver.onFocusEvent` has no
    /// `finishInputEvent`), so it is not added to the pending table.
    pub fn focus(&mut self, has_focus: bool) -> Result<u32> {
        let seq = self.alloc_seq()?;
        let event_id = self.alloc_event_id();
        self.publish(&InputMessage::Focus {
            seq,
            event_id,
            has_focus,
        })?;
        Ok(seq)
    }

    /// `InputPublisher::publishTouchModeEvent`: announce the touch-mode state.
    pub fn touch_mode(&mut self, in_touch_mode: bool) -> Result<u32> {
        let seq = self.alloc_seq()?;
        let event_id = self.alloc_event_id();
        self.publish(&InputMessage::TouchMode {
            seq,
            event_id,
            in_touch_mode,
        })?;
        Ok(seq)
    }

    /// A key press (`DOWN` + `UP`) carrying an explicit meta state and device
    /// id. Used by typed text: the guest's `KeyEvent.getUnicodeChar()` resolves
    /// the character through `KeyCharacterMap.load(deviceId)`, so a virtual
    /// keyboard (`deviceId = -1`) is the right source for injected text.
    pub fn key_stroke(
        &mut self,
        key_code: i32,
        meta_state: i32,
        device_id: i32,
    ) -> Result<Gesture> {
        let down_time = monotonic_nanos();
        let mut down = KeyEvent::new(
            self.alloc_event_id(),
            KEY_ACTION_DOWN,
            key_code,
            down_time,
            down_time,
        );
        down.meta_state = meta_state;
        down.device_id = device_id;
        let mut up = down;
        up.event_id = self.alloc_event_id();
        up.action = KEY_ACTION_UP;
        up.event_time = down_time + DEFAULT_TAP_HOLD_NANOS;
        // Same rule as `key_at`: no `FLAG_TRACKING` on the wire — the app's own
        // `KeyEvent.DispatcherState` sets it once it has tracked the down.
        Ok(Gesture {
            dispatches: vec![self.publish_key(&down)?, self.publish_key(&up)?],
        })
    }

    /// Press several key codes in order (each a `DOWN`+`UP` pair).
    pub fn key_sequence(&mut self, key_codes: &[i32]) -> Result<Vec<Dispatch>> {
        let mut out = Vec::with_capacity(key_codes.len() * 2);
        for code in key_codes {
            out.extend(self.key(*code)?.dispatches);
        }
        Ok(out)
    }

    /// A configurable gesture: `DOWN` at `points[0]`, one `MOVE` per further
    /// point, then `UP` at the last point. Event times advance by `step_ns`
    /// (>= 0); `downTime` stays the first event's time.
    pub fn gesture(&mut self, points: &[(f32, f32)], step_ns: i64) -> Result<Vec<Dispatch>> {
        let Some((&(first_x, first_y), rest)) = points.split_first() else {
            return Err(ChannelError::Malformed("gesture with no points"));
        };
        let down_time = monotonic_nanos();
        let mut t = down_time;
        let mut out = Vec::with_capacity(points.len() + 1);
        out.push(self.down_at(first_x, first_y, down_time, t)?);
        for &(x, y) in rest {
            t += step_ns.max(0);
            out.push(self.move_at(x, y, down_time, t)?);
        }
        if let Some(&(x, y)) = points.last() {
            t += step_ns.max(0);
            out.push(self.up_at(x, y, down_time, t)?);
        }
        Ok(out)
    }

    /// `InputPublisher::receiveConsumerResponse`: read one reply, or `None`
    /// when the channel has nothing pending. `FINISHED` is matched against the
    /// dispatch table so the caller gets `handled` **and** the publish→ack
    /// latency; unknown sequences are returned with `latency: None` instead of
    /// being dropped.
    pub fn receive_consumer_response(&mut self) -> Result<Option<ConsumerResponse>> {
        let mut buf = [0u8; MAX_MESSAGE_SIZE];
        let (n, _) = match rustix::net::recv(&self.fd, &mut buf, RecvFlags::DONTWAIT) {
            Ok(v) => v,
            Err(rustix::io::Errno::AGAIN) => return Ok(None),
            Err(e) => return Err(io_err("recv", e)),
        };
        if n == 0 {
            return Err(ChannelError::PeerClosed);
        }
        match InputMessage::decode(&buf[..n])? {
            InputMessage::Finished {
                seq,
                handled,
                consume_time,
            } => {
                let latency = self
                    .pending
                    .remove(&seq)
                    .map(|(_, at)| Instant::now().saturating_duration_since(at));
                let ack = Ack {
                    seq,
                    handled,
                    consume_time,
                    latency,
                };
                self.acks.push(ack);
                Ok(Some(ConsumerResponse::Finished(ack)))
            }
            InputMessage::Timeline {
                seq,
                event_id,
                graphics_timeline,
            } => {
                let report = TimelineReport {
                    seq,
                    event_id,
                    graphics_timeline,
                };
                self.last_timeline = Some(report);
                Ok(Some(ConsumerResponse::Timeline(report)))
            }
            other => Err(ChannelError::Malformed(match other {
                InputMessage::Motion { .. } => "consumer sent MOTION",
                InputMessage::Key { .. } => "consumer sent KEY",
                _ => "consumer sent an unexpected message",
            })),
        }
    }
}

/// The `DOWN`+`UP` pair a tap/key press publishes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Gesture {
    /// The published sequences, in order.
    pub dispatches: Vec<Dispatch>,
}

// ---------------------------------------------------------------------------
// socketpair + registry
// ---------------------------------------------------------------------------

/// A freshly created pair, before one end is handed to the guest.
#[derive(Debug)]
pub struct InputChannelPair {
    /// The server (dispatcher) end, wrapped as a publisher.
    pub publisher: InputPublisher,
    /// The client end. It is duplicated into the `out InputChannel` parcel and
    /// should then be dropped in this process (`InputChannelRegistry::
    /// forget_client_end`), so the server observes EOF when the guest exits.
    pub client_fd: Option<OwnedFd>,
}

/// Create `socketpair(AF_UNIX, SOCK_SEQPACKET, 0, ...)` like
/// `InputChannel::openInputChannelPair`, naming the ends `"<name> (server)"`
/// and `"<name> (client)"`.
///
/// The descriptors are left **blocking** (as in AOSP): the guest's looper
/// blocks on its end, while both sides pass `MSG_DONTWAIT` per call.
pub fn open_input_channel_pair(name: &str) -> Result<InputChannelPair> {
    let (server, client) = open_channel_fds()?;
    Ok(InputChannelPair {
        publisher: InputPublisher::new(server, format!("{name} (server)")),
        client_fd: Some(client),
    })
}

/// Create the raw `socketpair(AF_UNIX, SOCK_SEQPACKET, 0, ...)` pair, without
/// wrapping either end in a producer.
///
/// [`open_input_channel_pair`] is the window-shaped caller (server end = the
/// dispatcher's [`InputPublisher`]). The IME bind result needs the opposite
/// direction: the **app** is the producer (`ImeInputEventSender` sends the key
/// into its end, exactly like `InputMethodManager.mCurChannel`) and ACOS's IME
/// service is the consumer, so it keeps this pair's server end in a
/// [`LoopbackConsumer`] and hands the client end to the app.
///
/// Both descriptors are left blocking (AOSP's `InputChannel` contract); every
/// call passes `MSG_DONTWAIT` explicitly.
pub fn open_channel_fds() -> Result<(OwnedFd, OwnedFd)> {
    rustix::net::socketpair(
        rustix::net::AddressFamily::UNIX,
        SocketType::SEQPACKET,
        SocketFlags::CLOEXEC,
        None,
    )
    .map_err(|e| io_err("socketpair", e))
}

/// Write one `InputChannel` body exactly as the image's
/// `InputChannel.nativeWriteToParcel` does, **without** a `writeTypedObject`
/// presence word:
///
/// ```text
/// int32 isInitialized   1 with an fd, 0 for an unknown token
/// strong binder token
/// String16 name         "<window> (client)"
/// fd                    dup of the client end
/// ```
///
/// One encoder for both callers: `addToDisplayAsUser`'s `out InputChannel`
/// (which prefixes the AIDL presence word) and `InputBindResult.channel`
/// (whose `writeToParcel` writes the non-null `int32 1` itself).
pub fn write_channel_body(
    p: &mut Parcel,
    token: BinderId,
    name: &str,
    fd: BorrowedFd<'_>,
) -> Result<()> {
    p.write_int32(1);
    p.write_strong_binder_opt(Some(token));
    p.write_string16_opt(Some(name))
        .map_err(|e| ChannelError::Parcel(e.to_string()))?;
    p.write_file_descriptor(fd)
        .map_err(|e| ChannelError::Parcel(e.to_string()))?;
    Ok(())
}

/// One live input channel, keyed by the window token the session layer has.
#[derive(Debug)]
struct ChannelEntry {
    token: BinderId,
    name: String,
    publisher: InputPublisher,
    client_fd: Option<OwnedFd>,
}

/// The per-host set of live input channels, keyed by window token.
///
/// The token is the window's `IWindow` binder — the value W2 already writes in
/// the `out InputChannel` parcel (the real `openInputChannelPair` token is a
/// fresh `BBinder`; ACOS uses the window binder so the producer can be found
/// from the token the guest holds).
#[derive(Debug, Default)]
pub struct InputChannelRegistry {
    channels: BTreeMap<u64, ChannelEntry>,
}

impl InputChannelRegistry {
    /// An empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a pair for `token` and keep the server end. `name` is the window
    /// name (the same `"Surface(name=...)"` string W2 uses); the registry
    /// appends `" (server)"`/`" (client)"` like `openInputChannelPair`.
    pub fn register(&mut self, token: BinderId, name: &str) -> Result<()> {
        let pair = open_input_channel_pair(name)?;
        // The session's W2 name already ends in " client"; keep the framework's
        // `... client` spelling without adding a second suffix (the API-34
        // framework only requires a non-empty name, but the existing wire test
        // and `InputChannel.mName` convention both use this form).
        let client_name = if name.ends_with(" client") {
            name.to_string()
        } else {
            format!("{name} client")
        };
        self.channels.insert(
            token.0,
            ChannelEntry {
                token,
                name: client_name,
                publisher: pair.publisher,
                client_fd: pair.client_fd,
            },
        );
        Ok(())
    }

    /// The publisher for `token`.
    pub fn publisher_mut(&mut self, token: BinderId) -> Option<&mut InputPublisher> {
        self.channels.get_mut(&token.0).map(|e| &mut e.publisher)
    }

    /// The publisher for `token` (read-only).
    #[must_use]
    pub fn publisher(&self, token: BinderId) -> Option<&InputPublisher> {
        self.channels.get(&token.0).map(|e| &e.publisher)
    }

    /// The client-end name (`"<window> client"`).
    #[must_use]
    pub fn name(&self, token: BinderId) -> Option<&str> {
        self.channels.get(&token.0).map(|e| e.name.as_str())
    }

    /// The client descriptor while this process still holds it.
    #[must_use]
    pub fn client_fd(&self, token: BinderId) -> Option<BorrowedFd<'_>> {
        self.channels
            .get(&token.0)
            .and_then(|e| e.client_fd.as_ref())
            .map(AsFd::as_fd)
    }

    /// A duplicate of the client descriptor (caller owns it).
    pub fn duplicate_client_fd(&self, token: BinderId) -> Option<OwnedFd> {
        let fd = self.client_fd(token)?;
        rustix::io::dup(fd).ok()
    }

    /// Drop this process's client end after it has been written into the reply
    /// parcel. From then on the publisher observes the guest going away. The
    /// server end (and the publisher) stay live.
    pub fn forget_client_end(&mut self, token: BinderId) -> bool {
        self.channels
            .get_mut(&token.0)
            .and_then(|e| e.client_fd.take())
            .is_some()
    }

    /// Write the `out InputChannel` body exactly as W2 established it:
    ///
    /// ```text
    /// int32 1            AIDL writeTypedObject(InputChannel) presence
    /// int32 isInitialized  1 with an fd, 0 for an unknown token
    /// strong binder token
    /// String16 name      "<window> (client)"
    /// fd                 dup of the client end
    /// ```
    ///
    /// This is the same body `window_session.rs` writes from `acos-wm`'s
    /// `Channel`; it is here so the session layer can switch that call to the
    /// registry without a second encoder.
    pub fn write_to_parcel(&self, token: BinderId, reply: &mut Parcel) -> Result<()> {
        reply.write_int32(1);
        let Some(entry) = self.channels.get(&token.0) else {
            reply.write_int32(0); // native isInitialized = 0
            return Ok(());
        };
        let Some(client_fd) = entry.client_fd.as_ref() else {
            reply.write_int32(0);
            return Ok(());
        };
        write_channel_body(reply, entry.token, &entry.name, client_fd.as_fd())
    }

    /// Release a channel (window removed). Dropping the server end makes the
    /// guest's next read see EOF.
    pub fn remove(&mut self, token: BinderId) -> bool {
        self.channels.remove(&token.0).is_some()
    }

    /// Number of live channels.
    #[must_use]
    pub fn len(&self) -> usize {
        self.channels.len()
    }

    /// `true` when no channel is live.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.channels.is_empty()
    }

    /// The live tokens.
    pub fn tokens(&self) -> impl Iterator<Item = BinderId> + '_ {
        self.channels.values().map(|e| e.token)
    }
}

// ---------------------------------------------------------------------------
// test consumer (loopback proof)
// ---------------------------------------------------------------------------

/// A minimal `InputConsumer`: decode messages and answer `FINISHED`.
///
/// The direction inverts depending on which end it wraps:
///
/// * over a window channel's client end it is the test-side proof that the
///   bytes are readable and that the ack path works;
/// * over the **server** end of the IME `InputBindResult` channel it is the
///   real consumer: the app's `ImeInputEventSender` is the producer there, and
///   ACOS's IME service reads the `KEY` messages and answers
///   `FINISHED(handled)` (see `crate::ime`).
///
/// It is deliberately not a second dispatcher: it decodes the same bytes the
/// image's `InputChannel::receiveMessage` would, using only this module's
/// decoder.
#[derive(Debug)]
pub struct LoopbackConsumer {
    fd: OwnedFd,
}

impl LoopbackConsumer {
    /// Wrap the client end.
    #[must_use]
    pub fn new(fd: OwnedFd) -> Self {
        Self { fd }
    }

    /// Receive one message, or `None` when empty.
    pub fn recv(&mut self) -> Result<Option<InputMessage>> {
        let mut buf = [0u8; MAX_MESSAGE_SIZE];
        let (n, _) = match rustix::net::recv(&self.fd, &mut buf, RecvFlags::DONTWAIT) {
            Ok(v) => v,
            Err(rustix::io::Errno::AGAIN) => return Ok(None),
            Err(e) => return Err(io_err("recv", e)),
        };
        if n == 0 {
            return Err(ChannelError::PeerClosed);
        }
        InputMessage::decode(&buf[..n]).map(Some)
    }

    /// Send `FINISHED(seq, handled)` with `consume_time = now`.
    pub fn send_finished(&mut self, seq: u32, handled: bool) -> Result<()> {
        self.send_finished_at(seq, handled, monotonic_nanos())
    }

    /// Send `FINISHED(seq, handled)` with an explicit `consumeTime`.
    pub fn send_finished_at(&mut self, seq: u32, handled: bool, consume_time: i64) -> Result<()> {
        let bytes = encode_finished(seq, handled, consume_time);
        let sent = rustix::net::send(&self.fd, &bytes, SendFlags::DONTWAIT | SendFlags::NOSIGNAL)
            .map_err(|e| io_err("send", e))?;
        if sent != bytes.len() {
            return Err(ChannelError::PeerClosed);
        }
        Ok(())
    }

    /// Send a `TIMELINE` reply for `seq`.
    pub fn send_timeline(&mut self, seq: u32, event_id: i32, timeline: [i64; 2]) -> Result<()> {
        let bytes = encode_timeline(seq, event_id, timeline);
        let sent = rustix::net::send(&self.fd, &bytes, SendFlags::DONTWAIT | SendFlags::NOSIGNAL)
            .map_err(|e| io_err("send", e))?;
        if sent != bytes.len() {
            return Err(ChannelError::PeerClosed);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_match_the_image() {
        assert_eq!(motion_message_size(1), 0x138);
        assert_eq!(motion_message_size(2), 0x1c8);
        assert_eq!(motion_message_size(MAX_POINTERS), 0x9a8);
        assert_eq!(MAX_MESSAGE_SIZE, 0x9a8);
        assert_eq!(KEY_MESSAGE_SIZE, 0x68);
        assert_eq!(FINISHED_MESSAGE_SIZE, 0x18);
        assert_eq!(TIMELINE_MESSAGE_SIZE, 0x20);
        assert_eq!(FOCUS_MESSAGE_SIZE, 0x10);
        assert_eq!(TOUCH_MODE_MESSAGE_SIZE, 0x10);
    }

    #[test]
    fn focus_and_touch_mode_match_the_image_publisher_body() {
        // libinput.so publishFocusEvent (0x79910) writes type@0, seq@4,
        // eventId@8, hasFocus@0xc; size() returns 0x10 for FOCUS. I1 originally
        // recorded 8, which this test now pins against the image.
        let f = InputMessage::Focus {
            seq: 7,
            event_id: 42,
            has_focus: true,
        }
        .encode()
        .unwrap();
        assert_eq!(f.len(), 0x10);
        assert_eq!(u32::from_le_bytes(f[0..4].try_into().unwrap()), TYPE_FOCUS);
        assert_eq!(u32::from_le_bytes(f[4..8].try_into().unwrap()), 7);
        assert_eq!(i32::from_le_bytes(f[8..12].try_into().unwrap()), 42);
        assert_eq!(f[0x0c], 1);
        assert_eq!(
            InputMessage::decode(&f).unwrap(),
            InputMessage::Focus {
                seq: 7,
                event_id: 42,
                has_focus: true
            }
        );

        let t = InputMessage::TouchMode {
            seq: 8,
            event_id: 43,
            in_touch_mode: false,
        }
        .encode()
        .unwrap();
        assert_eq!(t.len(), 0x10);
        assert_eq!(
            u32::from_le_bytes(t[0..4].try_into().unwrap()),
            TYPE_TOUCH_MODE
        );
        assert_eq!(t[0x0c], 0);
    }

    #[test]
    fn pointer_coords_pack_in_axis_order() {
        let c = PointerCoords::with_axes(&[(AXIS_Y, 7.0), (AXIS_X, 3.0)]);
        assert_eq!(c.bits, 0b11);
        assert_eq!(c.values[0], 3.0); // X packs first despite input order
        assert_eq!(c.values[1], 7.0);
        assert_eq!(c.get(AXIS_X), 3.0);
        assert_eq!(c.get(AXIS_PRESSURE), 0.0);
    }
}
