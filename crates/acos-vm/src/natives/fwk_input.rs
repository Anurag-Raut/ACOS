//! `view.input`: the real-framework input natives (T-36).
//!
//! This module supplies the Rust implementation of every `android.view`
//! input native the Android 14 boot classpath declares:
//!
//! * `MotionEvent` — the whole native set over a Rust struct pool
//!   (`nativeInitialize`, the getters/setters, `nativeCopy`, `nativeAddBatch`,
//!   parcel/transform entry points);
//! * `KeyEvent.nativeNextId` and the `KeyCharacterMap` natives (one virtual
//!   QWERTY map);
//! * `InputChannel` — a real `AF_UNIX`/`SOCK_SEQPACKET` socketpair carrying
//!   framed [`InputFrame`] messages, plus the parcel/token/dup/dispose natives;
//! * `InputEventReceiver` — `nativeInit` registers the channel's read fd with
//!   the `MessageQueue` looper (via [`LooperHandle::add_fd`]); when the looper
//!   wakes for that fd, [`on_looper_wake`] drains the channel and invokes the
//!   real `InputEventReceiver.dispatchInputEvent` on the looper's green thread;
//! * `InputEventSender` — the app→system direction (`nativeSendMotionEvent` /
//!   `nativeSendKeyEvent` write an [`InputFrame`] to the channel fd).
//!
//! ## Why a Rust frame, not a Parcel
//!
//! An `InputChannel` is a kernel object, not a Parcel: the real framework
//! writes an `InputMessage` struct into the socketpair. ACOS does the same with
//! [`InputFrame`], a flat little-endian record. The Java-visible object graph
//! (`MotionEvent`, `KeyEvent`) is always built by the *real* framework
//! (`MotionEvent.obtain`, the `KeyEvent` constructor), so a Java test can
//! round-trip an event through a channel without any ACOS-specific Java class.
//!
//! ## Ack accounting
//!
//! Every dispatched frame gets a receiver-local sequence, passed to
//! `dispatchInputEvent(seq, event)`. The subclass's `finishInputEvent` calls
//! `nativeFinishInputEvent(ptr, seq, handled)`, which is forwarded to the
//! installed [`InputHost`] (or counted locally). The dispatcher side installs a
//! host so it can match ack→dispatch exactly once.

use std::cell::RefCell;
use std::collections::{BTreeMap, VecDeque};
use std::io;
use std::os::fd::{AsFd, AsRawFd, OwnedFd, RawFd};

use rustix::event::epoll::EventFlags;
use rustix::net::{RecvFlags, SendFlags};

use crate::class::NativeCtx;
use crate::error::{VmError, VmResult};
use crate::heap::{ArrayData, Handle, Object};
use crate::javabridge::JavaValue;
use crate::vm::Vm;

use super::kcm::{self, Kcm};
use super::looper::{handle_for_ptr, LooperHandle};
use super::parcel::with_store;

/// `MotionEvent.ACTION_DOWN`.
pub const ACTION_DOWN: i32 = 0;
/// `MotionEvent.ACTION_UP`.
pub const ACTION_UP: i32 = 1;
/// `MotionEvent.ACTION_MOVE`.
pub const ACTION_MOVE: i32 = 2;
/// `MotionEvent.ACTION_CANCEL`.
pub const ACTION_CANCEL: i32 = 3;

/// `KeyEvent.ACTION_DOWN`.
pub const KEY_ACTION_DOWN: i32 = 0;
/// `KeyEvent.ACTION_UP`.
pub const KEY_ACTION_UP: i32 = 1;

/// `InputDevice.SOURCE_TOUCHSCREEN`.
pub const SOURCE_TOUCHSCREEN: i32 = 0x0000_1002;
/// `InputDevice.SOURCE_KEYBOARD`.
pub const SOURCE_KEYBOARD: i32 = 0x0000_0101;

/// Frame kind: a `MotionEvent`.
pub const KIND_MOTION: i32 = 1;
/// Frame kind: a `KeyEvent`.
pub const KIND_KEY: i32 = 2;

// ---------------------------------------------------------------------------
// Frame codec
// ---------------------------------------------------------------------------

/// One pointer inside an [`InputFrame`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FramePointer {
    /// `PointerProperties.id`.
    pub id: i32,
    /// `PointerProperties.toolType`.
    pub tool_type: i32,
    /// X in window coordinates.
    pub x: f32,
    /// Y in window coordinates.
    pub y: f32,
    /// Pressure `0..=1`.
    pub pressure: f32,
    /// Touch size.
    pub size: f32,
}

/// A framed `InputMessage` written to an `InputChannel` socketpair.
#[derive(Debug, Clone, PartialEq)]
pub struct InputFrame {
    /// [`KIND_MOTION`] or [`KIND_KEY`].
    pub kind: i32,
    /// Sender sequence number (the ack key).
    pub seq: i32,
    /// Motion action / key action.
    pub action: i32,
    /// Motion flags / key flags.
    pub flags: i32,
    /// Motion edge flags.
    pub edge_flags: i32,
    /// Input source.
    pub source: i32,
    /// Device id.
    pub device_id: i32,
    /// Display id.
    pub display_id: i32,
    /// Meta state.
    pub meta_state: i32,
    /// Button state (motion).
    pub button_state: i32,
    /// Motion classification.
    pub classification: i32,
    /// Motion action button.
    pub action_button: i32,
    /// Key code (key).
    pub key_code: i32,
    /// Scan code (key).
    pub scan_code: i32,
    /// Repeat count (key).
    pub repeat: i32,
    /// Unicode char (key).
    pub unicode_char: i32,
    /// `downTime` in nanoseconds.
    pub down_time: i64,
    /// `eventTime` in nanoseconds.
    pub event_time: i64,
    /// X offset.
    pub x_offset: f32,
    /// Y offset.
    pub y_offset: f32,
    /// X precision.
    pub x_precision: f32,
    /// Y precision.
    pub y_precision: f32,
    /// Pointer snapshots.
    pub pointers: Vec<FramePointer>,
}

impl Default for InputFrame {
    fn default() -> Self {
        Self {
            kind: KIND_MOTION,
            seq: 0,
            action: 0,
            flags: 0,
            edge_flags: 0,
            source: 0,
            device_id: 0,
            display_id: 0,
            meta_state: 0,
            button_state: 0,
            classification: 0,
            action_button: 0,
            key_code: 0,
            scan_code: 0,
            repeat: 0,
            unicode_char: 0,
            down_time: 0,
            event_time: 0,
            x_offset: 0.0,
            y_offset: 0.0,
            x_precision: 0.0,
            y_precision: 0.0,
            pointers: Vec::new(),
        }
    }
}

impl InputFrame {
    /// Encode the frame as the channel's byte record.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(160);
        for v in [
            self.kind,
            self.seq,
            self.action,
            self.flags,
            self.edge_flags,
            self.source,
            self.device_id,
            self.display_id,
            self.meta_state,
            self.button_state,
            self.classification,
            self.action_button,
            self.key_code,
            self.scan_code,
            self.repeat,
            self.unicode_char,
        ] {
            out.extend_from_slice(&v.to_le_bytes());
        }
        out.extend_from_slice(&self.down_time.to_le_bytes());
        out.extend_from_slice(&self.event_time.to_le_bytes());
        for v in [
            self.x_offset,
            self.y_offset,
            self.x_precision,
            self.y_precision,
        ] {
            out.extend_from_slice(&v.to_le_bytes());
        }
        out.extend_from_slice(&(self.pointers.len() as i32).to_le_bytes());
        for p in &self.pointers {
            for v in [p.id, p.tool_type] {
                out.extend_from_slice(&v.to_le_bytes());
            }
            for v in [p.x, p.y, p.pressure, p.size] {
                out.extend_from_slice(&v.to_le_bytes());
            }
        }
        out
    }

    /// Decode a record produced by [`InputFrame::encode`].
    pub fn decode(bytes: &[u8]) -> io::Result<Self> {
        let mut r = FrameReader { b: bytes, o: 0 };
        let mut f = InputFrame {
            kind: r.i32()?,
            seq: r.i32()?,
            action: r.i32()?,
            flags: r.i32()?,
            edge_flags: r.i32()?,
            source: r.i32()?,
            device_id: r.i32()?,
            display_id: r.i32()?,
            meta_state: r.i32()?,
            button_state: r.i32()?,
            classification: r.i32()?,
            action_button: r.i32()?,
            key_code: r.i32()?,
            scan_code: r.i32()?,
            repeat: r.i32()?,
            unicode_char: r.i32()?,
            down_time: r.i64()?,
            event_time: r.i64()?,
            x_offset: r.f32()?,
            y_offset: r.f32()?,
            x_precision: r.f32()?,
            y_precision: r.f32()?,
            pointers: Vec::new(),
        };
        let n = r.i32()?;
        if !(0..=256).contains(&n) {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "bad pointer count"));
        }
        for _ in 0..n {
            f.pointers.push(FramePointer {
                id: r.i32()?,
                tool_type: r.i32()?,
                x: r.f32()?,
                y: r.f32()?,
                pressure: r.f32()?,
                size: r.f32()?,
            });
        }
        Ok(f)
    }
}

struct FrameReader<'a> {
    b: &'a [u8],
    o: usize,
}

impl<'a> FrameReader<'a> {
    fn take(&mut self, n: usize) -> io::Result<&'a [u8]> {
        if self.o + n > self.b.len() {
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "truncated frame"));
        }
        let s = &self.b[self.o..self.o + n];
        self.o += n;
        Ok(s)
    }
    fn i32(&mut self) -> io::Result<i32> {
        Ok(i32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn i64(&mut self) -> io::Result<i64> {
        Ok(i64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }
    fn f32(&mut self) -> io::Result<f32> {
        Ok(f32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
}

// ---------------------------------------------------------------------------
// Rust state pools
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, Default)]
struct Pointer {
    id: i32,
    tool_type: i32,
    x: f32,
    y: f32,
    pressure: f32,
    size: f32,
    touch_major: f32,
    touch_minor: f32,
    tool_major: f32,
    tool_minor: f32,
    orientation: f32,
    tilt_x: f32,
    tilt_y: f32,
    distance: f32,
}

#[derive(Debug, Clone)]
struct MotionEventState {
    device_id: i32,
    source: i32,
    display_id: i32,
    id: i32,
    action: i32,
    action_button: i32,
    flags: i32,
    edge_flags: i32,
    meta_state: i32,
    button_state: i32,
    classification: i32,
    x_offset: f32,
    y_offset: f32,
    x_precision: f32,
    y_precision: f32,
    down_time: i64,
    event_time: i64,
    surface_rotation: i32,
    cursor_x: f32,
    cursor_y: f32,
    pointers: Vec<Pointer>,
    /// `(eventTime, pointers)` history samples.
    history: Vec<(i64, Vec<Pointer>)>,
}

impl Default for MotionEventState {
    fn default() -> Self {
        Self {
            device_id: 0,
            source: 0,
            display_id: 0,
            id: 0,
            action: 0,
            action_button: 0,
            flags: 0,
            edge_flags: 0,
            meta_state: 0,
            button_state: 0,
            classification: 0,
            x_offset: 0.0,
            y_offset: 0.0,
            x_precision: 0.0,
            y_precision: 0.0,
            down_time: 0,
            event_time: 0,
            surface_rotation: 0,
            cursor_x: 0.0,
            cursor_y: 0.0,
            pointers: Vec::new(),
            history: Vec::new(),
        }
    }
}

#[derive(Debug)]
struct KeyMapState {
    keyboard_type: i32,
    /// The layout name the services layer wrote into the `KeyCharacterMap`
    /// parcel (`Generic` for ACOS's virtual keyboard).
    layout: String,
    /// The parsed map.
    kcm: Kcm,
}

#[derive(Debug)]
struct ChannelState {
    name: String,
    fd: OwnedFd,
    /// Frames the sender could not write because the seqpacket queue was full.
    pending: VecDeque<Vec<u8>>,
    /// Cached `android.os.Binder` token returned by `nativeGetToken`.
    token: Option<Handle>,
}

#[derive(Debug)]
struct ReceiverState {
    channel: u64,
    looper: u64,
    weak_ref: Handle,
}

#[derive(Debug)]
struct SenderState {
    channel: u64,
}

#[derive(Default)]
struct Pools {
    next_id: u64,
    motions: BTreeMap<u64, MotionEventState>,
    key_maps: BTreeMap<u64, KeyMapState>,
    channels: BTreeMap<u64, ChannelState>,
    receivers: BTreeMap<u64, ReceiverState>,
    senders: BTreeMap<u64, SenderState>,
    /// Local ack counter used when no [`InputHost`] is installed.
    finished: u64,
    /// Frames actually dispatched through `dispatchInputEvent`.
    dispatched: u64,
    /// Times the looper poll hook observed a ready input fd.
    wake_polls: u64,
    /// Native `MotionEvent` structs allocated.
    motion_created: u64,
    /// Native `MotionEvent` structs disposed.
    motion_disposed: u64,
    /// Per-native invocation counters, for the T-36 acceptance evidence.
    hits: BTreeMap<&'static str, u64>,
}

impl Pools {
    fn alloc_id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }
    fn hit(&mut self, name: &'static str) {
        *self.hits.entry(name).or_insert(0) += 1;
    }
}

thread_local! {
    static POOLS: RefCell<Pools> = RefCell::new(Pools::default());
    static HOST: RefCell<Option<Box<dyn InputHost>>> = const { RefCell::new(None) };
}

/// A dispatcher-side hook installed by the runtime.
///
/// `nativeFinishInputEvent` forwards every ack here so the service layer can
/// match dispatch→ack exactly once and never leak an unacked event.
pub trait InputHost: std::fmt::Debug {
    /// One event was finished by the app.
    fn finish_event(&self, seq: i32, handled: bool);
    /// The app sent an event back to the system (IME / back injection).
    fn send_frame(&self, frame: &InputFrame);
}

/// Install (or replace) the process-wide input host. Called by `acos-runtime`.
pub fn install_host(host: Box<dyn InputHost>) {
    HOST.with(|h| *h.borrow_mut() = Some(host));
}

/// Remove the installed host.
pub fn clear_host() {
    HOST.with(|h| *h.borrow_mut() = None);
}

/// Reset the per-native hit counters (test helper).
pub fn reset_hits() {
    POOLS.with(|p| p.borrow_mut().hits.clear());
}

/// Snapshot of the per-native hit counters (test helper).
#[must_use]
pub fn hits() -> BTreeMap<&'static str, u64> {
    POOLS.with(|p| p.borrow().hits.clone())
}

/// Number of live native `MotionEvent` structs (test helper).
#[must_use]
pub fn live_motion_events() -> usize {
    POOLS.with(|p| p.borrow().motions.len())
}

/// Number of live native `InputChannel`s (test helper).
#[must_use]
pub fn live_channels() -> usize {
    POOLS.with(|p| p.borrow().channels.len())
}

/// Number of live `InputEventReceiver`s (test helper).
#[must_use]
pub fn live_receivers() -> usize {
    POOLS.with(|p| p.borrow().receivers.len())
}

/// Number of events finished through the native ack path (test helper).
#[must_use]
pub fn finished_events() -> u64 {
    POOLS.with(|p| p.borrow().finished)
}

/// Number of frames dispatched through the real `dispatchInputEvent` (test helper).
#[must_use]
pub fn dispatched_frames() -> u64 {
    POOLS.with(|p| p.borrow().dispatched)
}

/// Times the looper poll hook ran with a ready fd (test helper).
#[must_use]
pub fn wake_polls() -> u64 {
    POOLS.with(|p| p.borrow().wake_polls)
}

/// Native `MotionEvent` structs allocated so far (test helper).
#[must_use]
pub fn motion_created() -> u64 {
    POOLS.with(|p| p.borrow().motion_created)
}

/// Native `MotionEvent` structs disposed so far (test helper).
#[must_use]
pub fn motion_disposed() -> u64 {
    POOLS.with(|p| p.borrow().motion_disposed)
}

fn unsupported(api: &str) -> VmError {
    VmError::unsupported_api(api, "acos-vm::natives::fwk_input")
}

// ---------------------------------------------------------------------------
// Registration
// ---------------------------------------------------------------------------

/// Register every `view.input` native.
pub(crate) fn install(vm: &mut Vm) -> VmResult<()> {
    let me = "Landroid/view/MotionEvent;";
    vm.register_native(me, "nativeInitialize", "(JIIIIIIIIIFFFFJJI[Landroid/view/MotionEvent$PointerProperties;[Landroid/view/MotionEvent$PointerCoords;)J", me_initialize);
    vm.register_native(me, "nativeDispose", "(J)V", me_dispose);
    vm.register_native(me, "nativeCopy", "(JJZ)J", me_copy);
    vm.register_native(me, "nativeAddBatch", "(JJ[Landroid/view/MotionEvent$PointerCoords;I)V", me_add_batch);
    vm.register_native(me, "nativeGetAction", "(J)I", me_get_action);
    vm.register_native(me, "nativeSetAction", "(JI)V", me_set_action);
    vm.register_native(me, "nativeGetActionButton", "(J)I", me_get_action_button);
    vm.register_native(me, "nativeSetActionButton", "(JI)V", me_set_action_button);
    vm.register_native(me, "nativeGetFlags", "(J)I", me_get_flags);
    vm.register_native(me, "nativeSetFlags", "(JI)V", me_set_flags);
    vm.register_native(me, "nativeGetEdgeFlags", "(J)I", me_get_edge_flags);
    vm.register_native(me, "nativeSetEdgeFlags", "(JI)V", me_set_edge_flags);
    vm.register_native(me, "nativeGetMetaState", "(J)I", me_get_meta_state);
    vm.register_native(me, "nativeGetButtonState", "(J)I", me_get_button_state);
    vm.register_native(me, "nativeSetButtonState", "(JI)V", me_set_button_state);
    vm.register_native(me, "nativeGetSource", "(J)I", me_get_source);
    vm.register_native(me, "nativeSetSource", "(JI)V", me_set_source);
    vm.register_native(me, "nativeGetId", "(J)I", me_get_id);
    vm.register_native(me, "nativeGetDeviceId", "(J)I", me_get_device_id);
    vm.register_native(me, "nativeGetDisplayId", "(J)I", me_get_display_id);
    vm.register_native(me, "nativeSetDisplayId", "(JI)V", me_set_display_id);
    vm.register_native(me, "nativeGetClassification", "(J)I", me_get_classification);
    vm.register_native(me, "nativeGetDownTimeNanos", "(J)J", me_get_down_time);
    vm.register_native(me, "nativeSetDownTimeNanos", "(JJ)V", me_set_down_time);
    vm.register_native(me, "nativeGetEventTimeNanos", "(JI)J", me_get_event_time);
    vm.register_native(me, "nativeGetPointerCount", "(J)I", me_get_pointer_count);
    vm.register_native(me, "nativeGetPointerId", "(JI)I", me_get_pointer_id);
    vm.register_native(me, "nativeFindPointerIndex", "(JI)I", me_find_pointer_index);
    vm.register_native(me, "nativeGetToolType", "(JI)I", me_get_tool_type);
    vm.register_native(me, "nativeGetAxisValue", "(JIII)F", me_get_axis_value);
    vm.register_native(me, "nativeGetRawAxisValue", "(JIII)F", me_get_raw_axis_value);
    vm.register_native(me, "nativeGetXOffset", "(J)F", me_get_x_offset);
    vm.register_native(me, "nativeGetYOffset", "(J)F", me_get_y_offset);
    vm.register_native(me, "nativeGetXPrecision", "(J)F", me_get_x_precision);
    vm.register_native(me, "nativeGetYPrecision", "(J)F", me_get_y_precision);
    vm.register_native(me, "nativeGetXCursorPosition", "(J)F", me_get_x_cursor);
    vm.register_native(me, "nativeGetYCursorPosition", "(J)F", me_get_y_cursor);
    vm.register_native(me, "nativeSetCursorPosition", "(JFF)V", me_set_cursor);
    vm.register_native(me, "nativeGetSurfaceRotation", "(J)I", me_get_surface_rotation);
    vm.register_native(me, "nativeGetHistorySize", "(J)I", me_get_history_size);
    vm.register_native(me, "nativeGetPointerCoords", "(JIILandroid/view/MotionEvent$PointerCoords;)V", me_get_pointer_coords);
    vm.register_native(me, "nativeGetPointerProperties", "(JILandroid/view/MotionEvent$PointerProperties;)V", me_get_pointer_properties);
    vm.register_native(me, "nativeIsTouchEvent", "(J)Z", me_is_touch_event);
    vm.register_native(me, "nativeOffsetLocation", "(JFF)V", me_offset_location);
    vm.register_native(me, "nativeScale", "(JF)V", me_scale);
    vm.register_native(me, "nativeAxisFromString", "(Ljava/lang/String;)I", me_axis_from_string);
    vm.register_native(me, "nativeAxisToString", "(I)Ljava/lang/String;", me_axis_to_string);
    vm.register_native(me, "nativeReadFromParcel", "(JLandroid/os/Parcel;)J", me_read_from_parcel);
    vm.register_native(me, "nativeWriteToParcel", "(JLandroid/os/Parcel;)V", me_write_to_parcel);
    vm.register_native(me, "nativeApplyTransform", "(JLandroid/graphics/Matrix;)V", me_apply_transform);
    vm.register_native(me, "nativeTransform", "(JLandroid/graphics/Matrix;)V", me_apply_transform);

    let ke = "Landroid/view/KeyEvent;";
    vm.register_native(ke, "nativeNextId", "()I", key_next_id);
    vm.register_native(ke, "nativeKeyCodeFromString", "(Ljava/lang/String;)I", key_code_from_string);
    vm.register_native(ke, "nativeKeyCodeToString", "(I)Ljava/lang/String;", key_code_to_string);

    let kcm = "Landroid/view/KeyCharacterMap;";
    vm.register_native(kcm, "nativeDispose", "(J)V", kcm_dispose);
    vm.register_native(kcm, "nativeEquals", "(JJ)Z", kcm_equals);
    vm.register_native(kcm, "nativeGetKeyboardType", "(J)I", kcm_get_keyboard_type);
    vm.register_native(kcm, "nativeGetCharacter", "(JII)C", kcm_get_character);
    vm.register_native(kcm, "nativeGetDisplayLabel", "(JI)C", kcm_get_display_label);
    vm.register_native(kcm, "nativeGetNumber", "(JI)C", kcm_get_number);
    vm.register_native(kcm, "nativeGetMatch", "(JI[CI)C", kcm_get_match);
    vm.register_native(kcm, "nativeGetEvents", "(J[C)[Landroid/view/KeyEvent;", kcm_get_events);
    vm.register_native(kcm, "nativeGetFallbackAction", "(JIILandroid/view/KeyCharacterMap$FallbackAction;)Z", kcm_get_fallback);
    vm.register_native(kcm, "nativeObtainEmptyKeyCharacterMap", "(I)Landroid/view/KeyCharacterMap;", kcm_obtain_empty);
    vm.register_native(kcm, "nativeReadFromParcel", "(Landroid/os/Parcel;)J", kcm_read_from_parcel);
    vm.register_native(kcm, "nativeWriteToParcel", "(JLandroid/os/Parcel;)V", kcm_write_to_parcel);

    let ic = "Landroid/view/InputChannel;";
    vm.register_native(ic, "nativeOpenInputChannelPair", "(Ljava/lang/String;)[J", ic_open_pair);
    vm.register_native(ic, "nativeGetName", "(J)Ljava/lang/String;", ic_get_name);
    vm.register_native(ic, "nativeGetToken", "(J)Landroid/os/IBinder;", ic_get_token);
    vm.register_native(ic, "nativeDup", "(J)J", ic_dup);
    vm.register_native(ic, "nativeDispose", "(J)V", ic_dispose);
    vm.register_native(ic, "nativeGetFinalizer", "()J", ic_get_finalizer);
    vm.register_native(ic, "nativeReadFromParcel", "(Landroid/os/Parcel;)J", ic_read_from_parcel);
    vm.register_native(ic, "nativeWriteToParcel", "(Landroid/os/Parcel;J)V", ic_write_to_parcel);

    let ier = "Landroid/view/InputEventReceiver;";
    vm.register_native(ier, "nativeInit", "(Ljava/lang/ref/WeakReference;Landroid/view/InputChannel;Landroid/os/MessageQueue;)J", ier_init);
    vm.register_native(ier, "nativeDispose", "(J)V", ier_dispose);
    vm.register_native(ier, "nativeFinishInputEvent", "(JIZ)V", ier_finish);
    vm.register_native(ier, "nativeConsumeBatchedInputEvents", "(JJ)Z", ier_consume_batched);
    vm.register_native(ier, "nativeReportTimeline", "(JIJJ)V", ier_report_timeline);
    vm.register_native(ier, "nativeDump", "(JLjava/lang/String;)Ljava/lang/String;", ier_dump);

    let ies = "Landroid/view/InputEventSender;";
    vm.register_native(ies, "nativeInit", "(Ljava/lang/ref/WeakReference;Landroid/view/InputChannel;Landroid/os/MessageQueue;)J", ies_init);
    vm.register_native(ies, "nativeDispose", "(J)V", ies_dispose);
    vm.register_native(ies, "nativeSendMotionEvent", "(JILandroid/view/MotionEvent;)Z", ies_send_motion);
    vm.register_native(ies, "nativeSendKeyEvent", "(JILandroid/view/KeyEvent;)Z", ies_send_key);
    Ok(())
}

// ---------------------------------------------------------------------------
// MotionEvent natives
// ---------------------------------------------------------------------------

fn me_hit(ctx: NativeCtx, name: &'static str) {
    let _ = ctx;
    POOLS.with(|p| p.borrow_mut().hit(name));
}

fn with_motion<R>(ptr: u64, f: impl FnOnce(&mut MotionEventState) -> R) -> Option<R> {
    POOLS.with(|p| p.borrow_mut().motions.get_mut(&ptr).map(f))
}

fn read_motion<R>(ptr: u64, f: impl FnOnce(&MotionEventState) -> R) -> Option<R> {
    POOLS.with(|p| p.borrow().motions.get(&ptr).map(f))
}

fn me_initialize(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    me_hit(ctx, "nativeInitialize");
    let ptr = vm.arg_i64(ctx, 0) as u64;
    let device_id = vm.arg_i32(ctx, 1);
    let source = vm.arg_i32(ctx, 2);
    let display_id = vm.arg_i32(ctx, 3);
    let action = vm.arg_i32(ctx, 4);
    let flags = vm.arg_i32(ctx, 5);
    let edge_flags = vm.arg_i32(ctx, 6);
    let meta_state = vm.arg_i32(ctx, 7);
    let button_state = vm.arg_i32(ctx, 8);
    let classification = vm.arg_i32(ctx, 9);
    let x_offset = vm.arg_f32(ctx, 10);
    let y_offset = vm.arg_f32(ctx, 11);
    let x_precision = vm.arg_f32(ctx, 12);
    let y_precision = vm.arg_f32(ctx, 13);
    let down_time = vm.arg_i64(ctx, 14);
    let event_time = vm.arg_i64(ctx, 15);
    let pointer_count = vm.arg_i32(ctx, 16);
    let props = vm.arg_ref(ctx, 17);
    let coords = vm.arg_ref(ctx, 18);

    if pointer_count < 0 {
        return Err(vm.throw_new(
            "Ljava/lang/IllegalArgumentException;",
            Some("pointerCount is negative"),
        ));
    }
    // Android validates `pointerCount` against the coords array. Reject a count
    // that cannot be satisfied instead of allocating/looping to a guest `int`
    // (HONESTY_AUDIT.md #10).
    let coord_len = match vm.heap.get(coords) {
        Some(crate::heap::Object::Array(a)) => a.len(),
        _ => 0,
    };
    if pointer_count as usize > coord_len {
        return Err(vm.throw_new(
            "Ljava/lang/IllegalArgumentException;",
            Some("pointerCount exceeds the coords array length"),
        ));
    }
    let pointer_count = pointer_count as usize;
    let mut pointers = Vec::with_capacity(pointer_count);
    for i in 0..pointer_count {
        let mut p = Pointer::default();
        if props != 0 {
            if let Ok(JavaValue::Object(o)) = vm.get_array_element(props, i) {
                p.id = read_int(vm, o, "id");
                p.tool_type = read_int(vm, o, "toolType");
                vm.release_bridge_root(o);
            }
        }
        if coords != 0 {
            if let Ok(JavaValue::Object(o)) = vm.get_array_element(coords, i) {
                p.x = read_f32(vm, o, "x");
                p.y = read_f32(vm, o, "y");
                p.pressure = read_f32(vm, o, "pressure");
                p.size = read_f32(vm, o, "size");
                p.touch_major = read_f32(vm, o, "touchMajor");
                p.touch_minor = read_f32(vm, o, "touchMinor");
                p.tool_major = read_f32(vm, o, "toolMajor");
                p.tool_minor = read_f32(vm, o, "toolMinor");
                p.orientation = read_f32(vm, o, "orientation");
                p.tilt_x = read_f32(vm, o, "tiltX");
                p.tilt_y = read_f32(vm, o, "tiltY");
                p.distance = read_f32(vm, o, "distance");
                vm.release_bridge_root(o);
            }
        }
        pointers.push(p);
    }

    let state = MotionEventState {
        device_id,
        source,
        display_id,
        action,
        flags,
        edge_flags,
        meta_state,
        button_state,
        classification,
        x_offset,
        y_offset,
        x_precision,
        y_precision,
        down_time,
        event_time,
        pointers,
        ..MotionEventState::default()
    };
    let new_ptr = POOLS.with(|p| {
        let mut p = p.borrow_mut();
        let id = if ptr != 0 { ptr } else { p.alloc_id() };
        p.motions.insert(id, state);
        p.motion_created += 1;
        id
    });
    vm.ret_i64(ctx, new_ptr as i64);
    Ok(())
}

fn read_int(vm: &mut Vm, obj: Handle, name: &str) -> i32 {
    match vm.get_field(obj, name) {
        Ok(JavaValue::Int(v)) => v,
        _ => 0,
    }
}

fn read_f32(vm: &mut Vm, obj: Handle, name: &str) -> f32 {
    match vm.get_field(obj, name) {
        Ok(JavaValue::Float(v)) => v,
        _ => 0.0,
    }
}

fn me_dispose(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    me_hit(ctx, "nativeDispose");
    let ptr = vm.arg_i64(ctx, 0) as u64;
    POOLS.with(|p| {
        let mut p = p.borrow_mut();
        if p.motions.remove(&ptr).is_some() {
            p.motion_disposed += 1;
        }
    });
    Ok(())
}

fn me_copy(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    me_hit(ctx, "nativeCopy");
    let dest = vm.arg_i64(ctx, 0) as u64;
    let src = vm.arg_i64(ctx, 1) as u64;
    let keep_history = vm.arg_bool(ctx, 2);
    let state = read_motion(src, Clone::clone);
    let out = POOLS.with(|p| {
        let mut p = p.borrow_mut();
        match state {
            Some(mut s) => {
                if !keep_history {
                    s.history.clear();
                }
                let id = if dest != 0 { dest } else { p.alloc_id() };
                p.motions.insert(id, s);
                id
            }
            None => 0,
        }
    });
    vm.ret_i64(ctx, out as i64);
    Ok(())
}

fn me_add_batch(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    me_hit(ctx, "nativeAddBatch");
    let ptr = vm.arg_i64(ctx, 0) as u64;
    let event_time = vm.arg_i64(ctx, 1);
    let coords = vm.arg_ref(ctx, 2);
    let _meta_state = vm.arg_i32(ctx, 3);
    let n: usize = read_motion(ptr, |s| s.pointers.len()).unwrap_or_default();
    let mut sample = Vec::with_capacity(n);
    for i in 0..n {
        let mut p = Pointer::default();
        if coords != 0 {
            if let Ok(JavaValue::Object(o)) = vm.get_array_element(coords, i) {
                p = Pointer {
                    x: read_f32(vm, o, "x"),
                    y: read_f32(vm, o, "y"),
                    pressure: read_f32(vm, o, "pressure"),
                    size: read_f32(vm, o, "size"),
                    ..Pointer::default()
                };
                vm.release_bridge_root(o);
            }
        }
        sample.push(p);
    }
    with_motion(ptr, |s| {
        s.history.push((event_time, sample));
        s.event_time = event_time;
    });
    Ok(())
}

macro_rules! motion_getter {
    ($fn_name:ident, $hit:literal, $ret:ident, $field:ident, $ty:ty) => {
        fn $fn_name(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
            me_hit(ctx, $hit);
            let ptr = vm.arg_i64(ctx, 0) as u64;
            let v = read_motion(ptr, |s| s.$field).unwrap_or_default();
            vm.$ret(ctx, v as $ty);
            Ok(())
        }
    };
}

motion_getter!(me_get_action, "nativeGetAction", ret_i32, action, i32);
motion_getter!(me_get_action_button, "nativeGetActionButton", ret_i32, action_button, i32);
motion_getter!(me_get_flags, "nativeGetFlags", ret_i32, flags, i32);
motion_getter!(me_get_edge_flags, "nativeGetEdgeFlags", ret_i32, edge_flags, i32);
motion_getter!(me_get_meta_state, "nativeGetMetaState", ret_i32, meta_state, i32);
motion_getter!(me_get_button_state, "nativeGetButtonState", ret_i32, button_state, i32);
motion_getter!(me_get_source, "nativeGetSource", ret_i32, source, i32);
motion_getter!(me_get_device_id, "nativeGetDeviceId", ret_i32, device_id, i32);
motion_getter!(me_get_id, "nativeGetId", ret_i32, id, i32);
motion_getter!(me_get_display_id, "nativeGetDisplayId", ret_i32, display_id, i32);
motion_getter!(me_get_classification, "nativeGetClassification", ret_i32, classification, i32);
motion_getter!(me_get_down_time, "nativeGetDownTimeNanos", ret_i64, down_time, i64);
motion_getter!(me_get_x_offset, "nativeGetXOffset", ret_f32, x_offset, f32);
motion_getter!(me_get_y_offset, "nativeGetYOffset", ret_f32, y_offset, f32);
motion_getter!(me_get_x_precision, "nativeGetXPrecision", ret_f32, x_precision, f32);
motion_getter!(me_get_y_precision, "nativeGetYPrecision", ret_f32, y_precision, f32);
motion_getter!(me_get_x_cursor, "nativeGetXCursorPosition", ret_f32, cursor_x, f32);
motion_getter!(me_get_y_cursor, "nativeGetYCursorPosition", ret_f32, cursor_y, f32);
motion_getter!(me_get_surface_rotation, "nativeGetSurfaceRotation", ret_i32, surface_rotation, i32);

fn me_set_action(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    me_hit(ctx, "nativeSetAction");
    let ptr = vm.arg_i64(ctx, 0) as u64;
    let v = vm.arg_i32(ctx, 1);
    with_motion(ptr, |s| s.action = v);
    Ok(())
}

fn me_set_action_button(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    me_hit(ctx, "nativeSetActionButton");
    let ptr = vm.arg_i64(ctx, 0) as u64;
    let v = vm.arg_i32(ctx, 1);
    with_motion(ptr, |s| s.action_button = v);
    Ok(())
}

fn me_set_flags(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    me_hit(ctx, "nativeSetFlags");
    let ptr = vm.arg_i64(ctx, 0) as u64;
    let v = vm.arg_i32(ctx, 1);
    with_motion(ptr, |s| s.flags = v);
    Ok(())
}

fn me_set_edge_flags(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    me_hit(ctx, "nativeSetEdgeFlags");
    let ptr = vm.arg_i64(ctx, 0) as u64;
    let v = vm.arg_i32(ctx, 1);
    with_motion(ptr, |s| s.edge_flags = v);
    Ok(())
}

fn me_set_button_state(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    me_hit(ctx, "nativeSetButtonState");
    let ptr = vm.arg_i64(ctx, 0) as u64;
    let v = vm.arg_i32(ctx, 1);
    with_motion(ptr, |s| s.button_state = v);
    Ok(())
}

fn me_set_source(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    me_hit(ctx, "nativeSetSource");
    let ptr = vm.arg_i64(ctx, 0) as u64;
    let v = vm.arg_i32(ctx, 1);
    with_motion(ptr, |s| s.source = v);
    Ok(())
}

fn me_set_display_id(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    me_hit(ctx, "nativeSetDisplayId");
    let ptr = vm.arg_i64(ctx, 0) as u64;
    let v = vm.arg_i32(ctx, 1);
    with_motion(ptr, |s| s.display_id = v);
    Ok(())
}

fn me_set_down_time(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    me_hit(ctx, "nativeSetDownTimeNanos");
    let ptr = vm.arg_i64(ctx, 0) as u64;
    let v = vm.arg_i64(ctx, 1);
    with_motion(ptr, |s| s.down_time = v);
    Ok(())
}

fn me_set_cursor(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    me_hit(ctx, "nativeSetCursorPosition");
    let ptr = vm.arg_i64(ctx, 0) as u64;
    let x = vm.arg_f32(ctx, 1);
    let y = vm.arg_f32(ctx, 2);
    with_motion(ptr, |s| {
        s.cursor_x = x;
        s.cursor_y = y;
    });
    Ok(())
}

fn me_get_event_time(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    me_hit(ctx, "nativeGetEventTimeNanos");
    let ptr = vm.arg_i64(ctx, 0) as u64;
    let history_index = vm.arg_i32(ctx, 1);
    let v = read_motion(ptr, |s| {
        if history_index <= 0 {
            s.event_time
        } else {
            s.history
                .get(history_index as usize - 1)
                .map(|(t, _)| *t)
                .unwrap_or(s.event_time)
        }
    })
    .unwrap_or_default();
    vm.ret_i64(ctx, v);
    Ok(())
}

fn me_get_pointer_count(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    me_hit(ctx, "nativeGetPointerCount");
    let ptr = vm.arg_i64(ctx, 0) as u64;
    let n = read_motion(ptr, |s| s.pointers.len()).unwrap_or(0) as i32;
    vm.ret_i32(ctx, n);
    Ok(())
}

fn me_get_pointer_id(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    me_hit(ctx, "nativeGetPointerId");
    let ptr = vm.arg_i64(ctx, 0) as u64;
    let index = vm.arg_i32(ctx, 1);
    let id = read_motion(ptr, |s| {
        s.pointers.get(index.max(0) as usize).map(|p| p.id).unwrap_or(-1)
    })
    .unwrap_or(-1);
    vm.ret_i32(ctx, id);
    Ok(())
}

fn me_find_pointer_index(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    me_hit(ctx, "nativeFindPointerIndex");
    let ptr = vm.arg_i64(ctx, 0) as u64;
    let id = vm.arg_i32(ctx, 1);
    let index = read_motion(ptr, |s| {
        s.pointers.iter().position(|p| p.id == id).map(|i| i as i32).unwrap_or(-1)
    })
    .unwrap_or(-1);
    vm.ret_i32(ctx, index);
    Ok(())
}

fn me_get_tool_type(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    me_hit(ctx, "nativeGetToolType");
    let ptr = vm.arg_i64(ctx, 0) as u64;
    let index = vm.arg_i32(ctx, 1);
    let t = read_motion(ptr, |s| {
        s.pointers
            .get(index.max(0) as usize)
            .map(|p| p.tool_type)
            .unwrap_or(0)
    })
    .unwrap_or(0);
    vm.ret_i32(ctx, t);
    Ok(())
}

fn axis_value(p: &Pointer, axis: i32) -> f32 {
    match axis {
        0 => p.x,
        1 => p.y,
        2 => p.pressure,
        3 => p.size,
        4 => p.touch_major,
        5 => p.touch_minor,
        6 => p.tool_major,
        7 => p.tool_minor,
        8 => p.orientation,
        24 => p.distance,
        25 => p.tilt_x,
        _ => 0.0,
    }
}

fn me_get_axis_value(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    me_hit(ctx, "nativeGetAxisValue");
    let ptr = vm.arg_i64(ctx, 0) as u64;
    let axis = vm.arg_i32(ctx, 1);
    let index = vm.arg_i32(ctx, 2);
    let v = read_motion(ptr, |s| {
        s.pointers
            .get(index.max(0) as usize)
            .map(|p| axis_value(p, axis))
            .unwrap_or(0.0)
    })
    .unwrap_or(0.0);
    vm.ret_f32(ctx, v);
    Ok(())
}

fn me_get_raw_axis_value(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    me_hit(ctx, "nativeGetRawAxisValue");
    let ptr = vm.arg_i64(ctx, 0) as u64;
    let axis = vm.arg_i32(ctx, 1);
    let index = vm.arg_i32(ctx, 2);
    let v = read_motion(ptr, |s| {
        s.pointers
            .get(index.max(0) as usize)
            .map(|p| match axis {
                0 => p.x - s.x_offset,
                1 => p.y - s.y_offset,
                a => axis_value(p, a),
            })
            .unwrap_or(0.0)
    })
    .unwrap_or(0.0);
    vm.ret_f32(ctx, v);
    Ok(())
}

fn me_get_history_size(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    me_hit(ctx, "nativeGetHistorySize");
    let ptr = vm.arg_i64(ctx, 0) as u64;
    let n = read_motion(ptr, |s| s.history.len()).unwrap_or(0) as i32;
    vm.ret_i32(ctx, n);
    Ok(())
}

fn me_get_pointer_coords(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    me_hit(ctx, "nativeGetPointerCoords");
    let ptr = vm.arg_i64(ctx, 0) as u64;
    let index = vm.arg_i32(ctx, 1);
    let out = vm.arg_ref(ctx, 2);
    let p = read_motion(ptr, |s| s.pointers.get(index.max(0) as usize).copied())
        .flatten()
        .unwrap_or_default();
    if out != 0 {
        let _ = vm.set_field(out, "x", JavaValue::Float(p.x));
        let _ = vm.set_field(out, "y", JavaValue::Float(p.y));
        let _ = vm.set_field(out, "pressure", JavaValue::Float(p.pressure));
        let _ = vm.set_field(out, "size", JavaValue::Float(p.size));
        let _ = vm.set_field(out, "touchMajor", JavaValue::Float(p.touch_major));
        let _ = vm.set_field(out, "touchMinor", JavaValue::Float(p.touch_minor));
        let _ = vm.set_field(out, "toolMajor", JavaValue::Float(p.tool_major));
        let _ = vm.set_field(out, "toolMinor", JavaValue::Float(p.tool_minor));
        let _ = vm.set_field(out, "orientation", JavaValue::Float(p.orientation));
        let _ = vm.set_field(out, "tiltX", JavaValue::Float(p.tilt_x));
        let _ = vm.set_field(out, "tiltY", JavaValue::Float(p.tilt_y));
        let _ = vm.set_field(out, "distance", JavaValue::Float(p.distance));
    }
    Ok(())
}

fn me_get_pointer_properties(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    me_hit(ctx, "nativeGetPointerProperties");
    let ptr = vm.arg_i64(ctx, 0) as u64;
    let index = vm.arg_i32(ctx, 1);
    let out = vm.arg_ref(ctx, 2);
    let p = read_motion(ptr, |s| s.pointers.get(index.max(0) as usize).copied())
        .flatten()
        .unwrap_or_default();
    if out != 0 {
        let _ = vm.set_field(out, "id", JavaValue::Int(p.id));
        let _ = vm.set_field(out, "toolType", JavaValue::Int(p.tool_type));
    }
    Ok(())
}

fn me_is_touch_event(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    me_hit(ctx, "nativeIsTouchEvent");
    let ptr = vm.arg_i64(ctx, 0) as u64;
    // `InputDevice.SOURCE_CLASS_POINTER == 2`; a touchscreen source is a pointer.
    let is_touch = read_motion(ptr, |s| (s.source & 0x0000_0002) != 0).unwrap_or(false);
    vm.ret_bool(ctx, is_touch);
    Ok(())
}

fn me_offset_location(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    me_hit(ctx, "nativeOffsetLocation");
    let ptr = vm.arg_i64(ctx, 0) as u64;
    let dx = vm.arg_f32(ctx, 1);
    let dy = vm.arg_f32(ctx, 2);
    with_motion(ptr, |s| {
        s.x_offset += dx;
        s.y_offset += dy;
        for p in &mut s.pointers {
            p.x -= dx;
            p.y -= dy;
        }
    });
    Ok(())
}

fn me_scale(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    me_hit(ctx, "nativeScale");
    let ptr = vm.arg_i64(ctx, 0) as u64;
    let scale = vm.arg_f32(ctx, 1);
    with_motion(ptr, |s| {
        for p in &mut s.pointers {
            p.x *= scale;
            p.y *= scale;
        }
    });
    Ok(())
}

fn me_axis_from_string(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    me_hit(ctx, "nativeAxisFromString");
    let h = vm.arg_ref(ctx, 0);
    let name = vm.java_string_to_rust(h).unwrap_or_default();
    let axis = match name.as_str() {
        "AXIS_X" => 0,
        "AXIS_Y" => 1,
        "AXIS_PRESSURE" => 2,
        "AXIS_SIZE" => 3,
        "AXIS_TOUCH_MAJOR" => 4,
        "AXIS_TOUCH_MINOR" => 5,
        "AXIS_TOOL_MAJOR" => 6,
        "AXIS_TOOL_MINOR" => 7,
        "AXIS_ORIENTATION" => 8,
        _ => -1,
    };
    vm.ret_i32(ctx, axis);
    Ok(())
}

fn me_axis_to_string(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    me_hit(ctx, "nativeAxisToString");
    let axis = vm.arg_i32(ctx, 0);
    let name = match axis {
        0 => "AXIS_X",
        1 => "AXIS_Y",
        2 => "AXIS_PRESSURE",
        3 => "AXIS_SIZE",
        4 => "AXIS_TOUCH_MAJOR",
        5 => "AXIS_TOUCH_MINOR",
        6 => "AXIS_TOOL_MAJOR",
        7 => "AXIS_TOOL_MINOR",
        8 => "AXIS_ORIENTATION",
        _ => "",
    };
    if name.is_empty() {
        vm.ret_ref(ctx, crate::heap::NULL);
    } else {
        let h = vm.new_string(name)?;
        vm.ret_ref(ctx, h);
    }
    Ok(())
}

fn me_read_from_parcel(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    // The MotionEvent parcel path is not exercised by the input stack; the
    // channel path carries `InputFrame`s instead.
    let _ = vm.arg_i64(ctx, 0);
    let _ = vm.arg_ref(ctx, 1);
    Err(unsupported("MotionEvent.nativeReadFromParcel"))
}

fn me_write_to_parcel(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let _ = vm.arg_i64(ctx, 0);
    let _ = vm.arg_ref(ctx, 1);
    Err(unsupported("MotionEvent.nativeWriteToParcel"))
}

fn me_apply_transform(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let _ = vm.arg_i64(ctx, 0);
    let _ = vm.arg_ref(ctx, 1);
    Err(unsupported("MotionEvent.nativeApplyTransform"))
}

// ---------------------------------------------------------------------------
// KeyEvent natives
// ---------------------------------------------------------------------------

thread_local! {
    static NEXT_KEY_ID: std::cell::Cell<i32> = const { std::cell::Cell::new(1) };
}

fn key_next_id(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    me_hit(ctx, "nativeNextId");
    let v = NEXT_KEY_ID.with(|c| {
        let v = c.get();
        c.set(v.wrapping_add(1));
        v
    });
    vm.ret_i32(ctx, v);
    Ok(())
}

fn key_code_from_string(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    me_hit(ctx, "nativeKeyCodeFromString");
    let h = vm.arg_ref(ctx, 0);
    let name = vm.java_string_to_rust(h).unwrap_or_default();
    let code = match name.as_str() {
        "KEYCODE_UNKNOWN" => 0,
        "KEYCODE_HOME" => 3,
        "KEYCODE_BACK" => 4,
        "KEYCODE_DPAD_UP" => 19,
        "KEYCODE_DPAD_DOWN" => 20,
        "KEYCODE_DPAD_LEFT" => 21,
        "KEYCODE_DPAD_RIGHT" => 22,
        "KEYCODE_ENTER" => 66,
        "KEYCODE_DEL" => 67,
        "KEYCODE_TAB" => 61,
        "KEYCODE_SPACE" => 62,
        "KEYCODE_ESCAPE" => 111,
        _ => -1,
    };
    vm.ret_i32(ctx, code);
    Ok(())
}

fn key_code_to_string(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    me_hit(ctx, "nativeKeyCodeToString");
    let code = vm.arg_i32(ctx, 0);
    let name = match code {
        0 => "KEYCODE_UNKNOWN",
        3 => "KEYCODE_HOME",
        4 => "KEYCODE_BACK",
        19 => "KEYCODE_DPAD_UP",
        20 => "KEYCODE_DPAD_DOWN",
        21 => "KEYCODE_DPAD_LEFT",
        22 => "KEYCODE_DPAD_RIGHT",
        66 => "KEYCODE_ENTER",
        67 => "KEYCODE_DEL",
        61 => "KEYCODE_TAB",
        62 => "KEYCODE_SPACE",
        111 => "KEYCODE_ESCAPE",
        _ => "",
    };
    if name.is_empty() {
        vm.ret_ref(ctx, crate::heap::NULL);
    } else {
        let h = vm.new_string(name)?;
        vm.ret_ref(ctx, h);
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// KeyCharacterMap natives
// ---------------------------------------------------------------------------

/// Layout name the services layer writes for ACOS's virtual keyboard.
const GENERIC_LAYOUT: &str = "Generic";

/// Resolve a layout name to a parsed map.
///
/// ACOS ships the AOSP generic layout; an unknown name is answered with that
/// same map rather than invented per-layout data, and the requested name is
/// kept in [`KeyMapState::layout`] so the mismatch is observable.
fn kcm_for_layout(layout: &str) -> Kcm {
    match layout {
        "Generic" | "Virtual" | "generic" | "virtual" | "" => kcm::generic(),
        _ => kcm::generic(),
    }
}

/// Run `f` over the map behind native pointer `ptr`.
fn kcm_lookup<R>(ptr: u64, f: impl FnOnce(&Kcm) -> R) -> Option<R> {
    POOLS.with(|p| p.borrow().key_maps.get(&ptr).map(|k| f(&k.kcm)))
}

/// Read a Java `char[]` into a Rust `Vec<u16>`.
fn java_char_array(vm: &Vm, h: Handle) -> Vec<u16> {
    match vm.heap.get(h) {
        Some(Object::Array(a)) => match &a.data {
            ArrayData::Char(v) => v.clone(),
            _ => Vec::new(),
        },
        _ => Vec::new(),
    }
}

fn kcm_dispose(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    me_hit(ctx, "nativeDispose");
    let ptr = vm.arg_i64(ctx, 0) as u64;
    POOLS.with(|p| {
        p.borrow_mut().key_maps.remove(&ptr);
    });
    Ok(())
}

fn kcm_equals(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    me_hit(ctx, "nativeEquals");
    let a = vm.arg_i64(ctx, 0) as u64;
    let b = vm.arg_i64(ctx, 1) as u64;
    vm.ret_bool(ctx, a == b);
    Ok(())
}

fn kcm_get_keyboard_type(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    me_hit(ctx, "nativeGetKeyboardType");
    let ptr = vm.arg_i64(ctx, 0) as u64;
    let t = POOLS.with(|p| {
        p.borrow()
            .key_maps
            .get(&ptr)
            .map(|k| k.keyboard_type)
            .unwrap_or(kcm::KEYBOARD_FULL)
    });
    vm.ret_i32(ctx, t);
    Ok(())
}

fn kcm_get_character(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    me_hit(ctx, "nativeGetCharacter");
    let ptr = vm.arg_i64(ctx, 0) as u64;
    let key_code = vm.arg_i32(ctx, 1);
    let meta_state = vm.arg_i32(ctx, 2);
    let ch = kcm_lookup(ptr, |k| k.character(key_code, meta_state)).unwrap_or(0);
    vm.ret_u16(ctx, ch);
    Ok(())
}

fn kcm_get_display_label(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    me_hit(ctx, "nativeGetDisplayLabel");
    let ptr = vm.arg_i64(ctx, 0) as u64;
    let key_code = vm.arg_i32(ctx, 1);
    let ch = kcm_lookup(ptr, |k| k.display_label(key_code)).unwrap_or(0);
    vm.ret_u16(ctx, ch);
    Ok(())
}

fn kcm_get_number(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    me_hit(ctx, "nativeGetNumber");
    let ptr = vm.arg_i64(ctx, 0) as u64;
    let key_code = vm.arg_i32(ctx, 1);
    let ch = kcm_lookup(ptr, |k| k.number(key_code)).unwrap_or(0);
    vm.ret_u16(ctx, ch);
    Ok(())
}

fn kcm_get_match(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    me_hit(ctx, "nativeGetMatch");
    let ptr = vm.arg_i64(ctx, 0) as u64;
    let key_code = vm.arg_i32(ctx, 1);
    let chars_obj = vm.arg_ref(ctx, 2);
    let meta_state = vm.arg_i32(ctx, 3);
    let chars = java_char_array(vm, chars_obj);
    let ch = kcm_lookup(ptr, |k| k.match_char(key_code, &chars, meta_state)).unwrap_or(0);
    vm.ret_u16(ctx, ch);
    Ok(())
}

fn kcm_get_events(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    me_hit(ctx, "nativeGetEvents");
    let ptr = vm.arg_i64(ctx, 0) as u64;
    let chars_obj = vm.arg_ref(ctx, 1);
    let chars = java_char_array(vm, chars_obj);
    // Resolve each character to its key-code/meta while the map is borrowed,
    // then build the real `KeyEvent`s once the borrow is released.
    let mut combos = Vec::with_capacity(chars.len());
    POOLS.with(|p| {
        if let Some(state) = p.borrow().key_maps.get(&ptr) {
            for &c in &chars {
                let Some(ch) = char::from_u32(c as u32) else {
                    continue;
                };
                if let Some((code, meta)) = state.kcm.events_for(ch) {
                    combos.push((code, meta));
                }
            }
        }
    });
    let mut events = Vec::with_capacity(combos.len() * 2);
    for (code, meta) in combos {
        for action in [KEY_ACTION_DOWN, KEY_ACTION_UP] {
            let ev = vm
                .new_instance_ctor(
                    "android/view/KeyEvent",
                    "(JJIIII)V",
                    &[
                        JavaValue::Long(0),
                        JavaValue::Long(0),
                        JavaValue::Int(action),
                        JavaValue::Int(code),
                        JavaValue::Int(0),
                        JavaValue::Int(meta),
                    ],
                )
                .map_err(bridge_err)?;
            events.push(JavaValue::Object(ev));
        }
    }
    let arr = vm
        .new_array("Landroid/view/KeyEvent;", &events)
        .map_err(bridge_err)?;
    vm.ret_ref(ctx, arr);
    Ok(())
}

fn kcm_get_fallback(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    me_hit(ctx, "nativeGetFallbackAction");
    let ptr = vm.arg_i64(ctx, 0) as u64;
    let key_code = vm.arg_i32(ctx, 1);
    let meta_state = vm.arg_i32(ctx, 2);
    let out = vm.arg_ref(ctx, 3);
    match kcm_lookup(ptr, |k| k.fallback(key_code, meta_state)).flatten() {
        Some((code, meta)) => {
            let _ = vm.set_field(out, "keyCode", JavaValue::Int(code));
            let _ = vm.set_field(out, "metaState", JavaValue::Int(meta));
            vm.ret_bool(ctx, true);
        }
        None => vm.ret_bool(ctx, false),
    }
    Ok(())
}

fn kcm_obtain_empty(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    me_hit(ctx, "nativeObtainEmptyKeyCharacterMap");
    let keyboard_type = vm.arg_i32(ctx, 0);
    let id = POOLS.with(|p| {
        let mut p = p.borrow_mut();
        let id = p.alloc_id();
        p.key_maps.insert(
            id,
            KeyMapState {
                keyboard_type,
                layout: GENERIC_LAYOUT.to_string(),
                kcm: kcm::generic(),
            },
        );
        id
    });
    let obj = vm
        .new_instance_ctor(
            "android/view/KeyCharacterMap",
            "(J)V",
            &[JavaValue::Long(id as i64)],
        )
        .map_err(bridge_err)?;
    vm.ret_ref(ctx, obj);
    Ok(())
}

/// `nativeReadFromParcel(Parcel)J`: read the `[keyboardType, layout]` payload
/// written by [`kcm_write_to_parcel`]/the services codec, parse the layout and
/// return the native pointer the Java constructor stores in `mPtr`.
fn kcm_read_from_parcel(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    me_hit(ctx, "nativeReadFromParcel");
    let parcel_obj = vm.arg_ref(ctx, 0);
    let handle = parcel_ptr(vm, parcel_obj)?;
    let keyboard_type = parcel_flat(with_store(|s| {
        s.table
            .with_parcel(handle, acos_parcel::Parcel::read_int32)
    }))?;
    let layout = parcel_flat(with_store(|s| {
        s.table
            .with_parcel(handle, acos_parcel::Parcel::read_string8)
    }))?
    .unwrap_or_default();
    let kcm = kcm_for_layout(&layout);
    let id = POOLS.with(|p| {
        let mut p = p.borrow_mut();
        let id = p.alloc_id();
        p.key_maps.insert(
            id,
            KeyMapState {
                keyboard_type,
                layout,
                kcm,
            },
        );
        id
    });
    vm.ret_i64(ctx, id as i64);
    Ok(())
}

/// `nativeWriteToParcel(J, Parcel)V`: write the same payload back.
fn kcm_write_to_parcel(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    me_hit(ctx, "nativeWriteToParcel");
    let ptr = vm.arg_i64(ctx, 0) as u64;
    let parcel_obj = vm.arg_ref(ctx, 1);
    let handle = parcel_ptr(vm, parcel_obj)?;
    let (keyboard_type, layout) = POOLS.with(|p| {
        p.borrow()
            .key_maps
            .get(&ptr)
            .map(|k| (k.keyboard_type, k.layout.clone()))
    })
    .unwrap_or((kcm::KEYBOARD_FULL, GENERIC_LAYOUT.to_string()));
    with_store(|s| s.table.with_parcel(handle, |p| p.write_int32(keyboard_type)))
        .map_err(parcel_err)?;
    parcel_flat(with_store(|s| {
        s.table
            .with_parcel(handle, |p| p.write_string8_opt(Some(&layout)))
    }))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// InputChannel natives
// ---------------------------------------------------------------------------

fn make_socketpair() -> io::Result<(OwnedFd, OwnedFd)> {
    let (a, b) = rustix::net::socketpair(
        rustix::net::AddressFamily::UNIX,
        rustix::net::SocketType::SEQPACKET,
        rustix::net::SocketFlags::CLOEXEC,
        None,
    )?;
    for fd in [&a, &b] {
        let flags = rustix::fs::fcntl_getfl(fd)?;
        rustix::fs::fcntl_setfl(fd, flags | rustix::fs::OFlags::NONBLOCK)?;
    }
    Ok((a, b))
}

fn register_channel(name: String, fd: OwnedFd) -> u64 {
    POOLS.with(|p| {
        let mut p = p.borrow_mut();
        let id = p.alloc_id();
        p.channels.insert(
            id,
            ChannelState {
                name,
                fd,
                pending: VecDeque::new(),
                token: None,
            },
        );
        id
    })
}

fn ic_open_pair(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    me_hit(ctx, "nativeOpenInputChannelPair");
    let name_h = vm.arg_ref(ctx, 0);
    let name = vm.java_string_to_rust(name_h).unwrap_or_default();
    let (a, b) = make_socketpair()
        .map_err(|e| vm.throw_new("Ljava/lang/RuntimeException;", Some(&format!("socketpair: {e}"))))?;
    let id0 = register_channel(format!("{name} (server)"), a);
    let id1 = register_channel(format!("{name} (client)"), b);
    let arr = vm
        .new_array(
            "J",
            &[JavaValue::Long(id0 as i64), JavaValue::Long(id1 as i64)],
        )
        .map_err(|e| match e {
            crate::javabridge::BridgeError::Vm(e) => e,
            other => VmError::Internal(other.to_string()),
        })?;
    vm.ret_ref(ctx, arr);
    Ok(())
}

fn ic_get_name(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    me_hit(ctx, "nativeGetName");
    let ptr = vm.arg_i64(ctx, 0) as u64;
    let name = POOLS.with(|p| p.borrow().channels.get(&ptr).map(|c| c.name.clone()));
    match name {
        Some(n) => {
            let h = vm.new_string(&n)?;
            vm.ret_ref(ctx, h);
        }
        None => vm.ret_ref(ctx, crate::heap::NULL),
    }
    Ok(())
}

fn ic_get_token(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    me_hit(ctx, "nativeGetToken");
    let ptr = vm.arg_i64(ctx, 0) as u64;
    if !POOLS.with(|p| p.borrow().channels.contains_key(&ptr)) {
        vm.ret_ref(ctx, crate::heap::NULL);
        return Ok(());
    }
    if let Some(h) = POOLS.with(|p| p.borrow().channels.get(&ptr).and_then(|c| c.token)) {
        vm.ret_ref(ctx, h);
        return Ok(());
    }
    // A real `android.os.Binder` object; the token identity is the channel ptr.
    let h = vm.new_instance("android/os/Binder").map_err(bridge_err)?;
    vm.root_handle(h);
    POOLS.with(|p| {
        if let Some(c) = p.borrow_mut().channels.get_mut(&ptr) {
            c.token = Some(h);
        }
    });
    vm.ret_ref(ctx, h);
    Ok(())
}

fn ic_dup(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    me_hit(ctx, "nativeDup");
    let ptr = vm.arg_i64(ctx, 0) as u64;
    let dup = POOLS.with(|p| {
        let p = p.borrow();
        match p.channels.get(&ptr) {
            Some(c) => rustix::io::dup(&c.fd).ok().map(|fd| (c.name.clone(), fd)),
            None => None,
        }
    });
    match dup {
        Some((name, fd)) => {
            let id = register_channel(name, fd);
            vm.ret_i64(ctx, id as i64);
        }
        None => vm.ret_i64(ctx, 0),
    }
    Ok(())
}

fn ic_dispose(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    me_hit(ctx, "nativeDispose");
    let ptr = vm.arg_i64(ctx, 0) as u64;
    let removed = POOLS.with(|p| p.borrow_mut().channels.remove(&ptr));
    if let Some(c) = removed {
        if let Some(t) = c.token {
            vm.release_bridge_root(t);
        }
    }
    Ok(())
}

fn ic_get_finalizer(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    me_hit(ctx, "nativeGetFinalizer");
    vm.ret_i64(ctx, 0);
    Ok(())
}

/// The `Parcel.mNativePtr` handle of a Java `Parcel` object.
fn parcel_ptr(vm: &mut Vm, obj: Handle) -> VmResult<u64> {
    // `ParcelTable` handle 0 is the first live slot, so 0 is a valid handle.
    match vm.get_field(obj, "mNativePtr") {
        Ok(JavaValue::Long(v)) => Ok(v as u64),
        _ => Err(unsupported("InputChannel: null or dead Parcel")),
    }
}

fn parcel_err(e: impl std::fmt::Display) -> VmError {
    VmError::Internal(format!("InputChannel parcel: {e}"))
}

fn parcel_flat<T>(
    r: Result<Result<T, acos_parcel::ParcelError>, acos_parcel::ParcelError>,
) -> VmResult<T> {
    match r {
        Ok(Ok(v)) => Ok(v),
        Ok(Err(e)) | Err(e) => Err(parcel_err(e)),
    }
}

/// `nativeReadFromParcel(Parcel)J`: rebuild a channel from the name + fd +
/// token written by [`ic_write_to_parcel`].
fn ic_read_from_parcel(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    me_hit(ctx, "nativeReadFromParcel");
    let parcel_obj = vm.arg_ref(ctx, 0);
    let handle = parcel_ptr(vm, parcel_obj)?;
    let name = with_store(|s| {
        s.table
            .with_parcel(handle, acos_parcel::Parcel::read_string16_lenient)
    })
    .unwrap_or_default();
    let raw = match with_store(|s| {
        s.table
            .with_parcel(handle, acos_parcel::Parcel::read_file_descriptor)
    }) {
        Ok(Ok(Some(raw))) => raw,
        Ok(Ok(None)) => 0,
        Ok(Err(e)) | Err(e) => return Err(parcel_err(e)),
    };
    let token_id = match with_store(|s| {
        s.table
            .with_parcel(handle, acos_parcel::Parcel::read_strong_binder)
    }) {
        Ok(Ok(v)) => v,
        Ok(Err(e)) | Err(e) => return Err(parcel_err(e)),
    };
    let token = token_id.and_then(|id| with_store(|s| s.binder_object_of(id)));
    if let Some(t) = token {
        vm.root_handle(t);
    }
    if raw == 0 {
        return Err(unsupported("InputChannel.nativeReadFromParcel: no fd"));
    }
    // Duplicate the parcel-owned descriptor into a channel ACOS owns.
    let fd = with_store(|s| {
        s.table.with_parcel(handle, |p| {
            p.borrowed_file_descriptor(raw)
                .and_then(|b| b.try_clone_to_owned().ok())
        })
    })
    .ok()
    .flatten()
    .ok_or_else(|| unsupported("InputChannel.nativeReadFromParcel: dead fd"))?;
    let id = POOLS.with(|p| {
        let mut p = p.borrow_mut();
        let id = p.alloc_id();
        p.channels.insert(
            id,
            ChannelState {
                name: name.unwrap_or_default(),
                fd,
                pending: VecDeque::new(),
                token,
            },
        );
        id
    });
    vm.ret_i64(ctx, id as i64);
    Ok(())
}

/// `nativeWriteToParcel(Parcel, J)V`: write name + duplicated fd + token.
fn ic_write_to_parcel(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    me_hit(ctx, "nativeWriteToParcel");
    let parcel_obj = vm.arg_ref(ctx, 0);
    let ptr = vm.arg_i64(ctx, 1) as u64;
    let handle = parcel_ptr(vm, parcel_obj)?;
    let (name, dup, token) = POOLS.with(|p| {
        let p = p.borrow();
        let c = p.channels.get(&ptr)?;
        Some((
            c.name.clone(),
            rustix::io::dup(&c.fd).ok()?,
            c.token,
        ))
    })
    .ok_or_else(|| unsupported("InputChannel.nativeWriteToParcel: dead channel"))?;
    parcel_flat(with_store(|s| {
        s.table
            .with_parcel(handle, |p| p.write_string16_opt(Some(&name)))
    }))?;
    parcel_flat(with_store(|s| {
        s.table
            .with_parcel(handle, |p| p.write_file_descriptor(dup.as_fd()))
    }))?;
    let token_id = token.and_then(|h| with_store(|s| s.binder_id_of(h)));
    let _ = with_store(|s| {
        s.table
            .with_parcel(handle, |p| p.write_strong_binder_opt(token_id))
    });
    Ok(())
}

// ---------------------------------------------------------------------------
// InputEventReceiver / InputEventSender
// ---------------------------------------------------------------------------

fn channel_fd(ptr: u64) -> Option<RawFd> {
    POOLS.with(|p| p.borrow().channels.get(&ptr).map(|c| c.fd.as_raw_fd()))
}

fn register_receiver(channel: u64, looper: u64, weak_ref: Handle) -> VmResult<u64> {
    if channel_fd(channel).is_none() {
        return Err(unsupported("InputEventReceiver: dead channel"));
    }
    let handle: LooperHandle =
        handle_for_ptr(looper).ok_or_else(|| unsupported("InputEventReceiver: dead MessageQueue"))?;
    // Register the read fd on the looper's epoll set. `add_fd` takes `AsFd`, so
    // borrow the channel's descriptor; epoll holds its own reference after this.
    POOLS.with(|p| {
        let p = p.borrow();
        let c = p.channels.get(&channel).ok_or_else(|| unsupported("dead channel"))?;
        handle
            .add_fd(&c.fd, EventFlags::IN, channel)
            .map_err(|e| VmError::Internal(format!("add_fd: {e}")))
    })?;
    let id = POOLS.with(|p| {
        let mut p = p.borrow_mut();
        let id = p.alloc_id();
        p.receivers.insert(
            id,
            ReceiverState {
                channel,
                looper,
                weak_ref,
            },
        );
        id
    });
    Ok(id)
}

fn ier_init(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    me_hit(ctx, "nativeInit");
    let weak = vm.arg_ref(ctx, 0);
    let channel_obj = vm.arg_ref(ctx, 1);
    let mq_obj = vm.arg_ref(ctx, 2);
    let channel = match vm.get_field(channel_obj, "mPtr") {
        Ok(JavaValue::Long(v)) => v as u64,
        _ => 0,
    };
    let looper = match vm.get_field(mq_obj, "mPtr") {
        Ok(JavaValue::Long(v)) => v as u64,
        _ => 0,
    };
    if channel == 0 || looper == 0 {
        return Err(unsupported("InputEventReceiver.nativeInit: null channel/queue"));
    }
    // Keep the WeakReference object alive for the receiver's lifetime.
    vm.root_handle(weak);
    let id = register_receiver(channel, looper, weak)?;
    vm.ret_i64(ctx, id as i64);
    Ok(())
}

fn ier_dispose(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    me_hit(ctx, "nativeDispose");
    let ptr = vm.arg_i64(ctx, 0) as u64;
    let removed = POOLS.with(|p| p.borrow_mut().receivers.remove(&ptr));
    if let Some(r) = removed {
        if let Some(handle) = handle_for_ptr(r.looper) {
            POOLS.with(|p| {
                let p = p.borrow();
                if let Some(c) = p.channels.get(&r.channel) {
                    let _ = handle.remove_fd(&c.fd);
                }
            });
        }
        vm.release_bridge_root(r.weak_ref);
    }
    Ok(())
}

fn ier_finish(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    me_hit(ctx, "nativeFinishInputEvent");
    let _receiver = vm.arg_i64(ctx, 0) as u64;
    let seq = vm.arg_i32(ctx, 1);
    let handled = vm.arg_bool(ctx, 2);
    let forwarded = HOST.with(|h| {
        let b = h.borrow();
        if let Some(host) = b.as_ref() {
            host.finish_event(seq, handled);
            true
        } else {
            false
        }
    });
    if !forwarded {
        POOLS.with(|p| p.borrow_mut().finished += 1);
    }
    Ok(())
}

fn ier_consume_batched(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    me_hit(ctx, "nativeConsumeBatchedInputEvents");
    let receiver = vm.arg_i64(ctx, 0) as u64;
    let _frame_time = vm.arg_i64(ctx, 1);
    let n = drain_receiver(vm, receiver)?;
    vm.ret_bool(ctx, n > 0);
    Ok(())
}

fn ier_report_timeline(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    me_hit(ctx, "nativeReportTimeline");
    let _ = vm.arg_i64(ctx, 0);
    Ok(())
}

fn ier_dump(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    me_hit(ctx, "nativeDump");
    let ptr = vm.arg_i64(ctx, 0) as u64;
    let s = format!("InputEventReceiver{{nativePtr={ptr}}}");
    let h = vm.new_string(&s)?;
    vm.ret_ref(ctx, h);
    Ok(())
}

fn ies_init(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    me_hit(ctx, "nativeInit");
    let _weak = vm.arg_ref(ctx, 0);
    let channel_obj = vm.arg_ref(ctx, 1);
    let _mq_obj = vm.arg_ref(ctx, 2);
    let channel = match vm.get_field(channel_obj, "mPtr") {
        Ok(JavaValue::Long(v)) => v as u64,
        _ => 0,
    };
    if channel == 0 {
        return Err(unsupported("InputEventSender.nativeInit: null channel"));
    }
    let id = POOLS.with(|p| {
        let mut p = p.borrow_mut();
        let id = p.alloc_id();
        p.senders.insert(id, SenderState { channel });
        id
    });
    vm.ret_i64(ctx, id as i64);
    Ok(())
}

fn ies_dispose(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    me_hit(ctx, "nativeDispose");
    let ptr = vm.arg_i64(ctx, 0) as u64;
    POOLS.with(|p| {
        p.borrow_mut().senders.remove(&ptr);
    });
    Ok(())
}

fn send_to_channel(channel: u64, frame: &InputFrame) -> io::Result<()> {
    let bytes = frame.encode();
    POOLS.with(|p| {
        let mut p = p.borrow_mut();
        let c = p
            .channels
            .get_mut(&channel)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "dead channel"))?;
        match rustix::net::send(&c.fd, &bytes, SendFlags::NOSIGNAL) {
            Ok(n) if n == bytes.len() => Ok(()),
            Ok(_) => Err(io::Error::new(io::ErrorKind::WriteZero, "short channel send")),
            // A full seqpacket queue is not an error: queue the frame and flush
            // it once the receiver has drained, exactly like the real sender.
            Err(rustix::io::Errno::AGAIN) => {
                c.pending.push_back(bytes);
                Ok(())
            }
            Err(e) => Err(io::Error::other(e.to_string())),
        }
    })
}

/// Flush any queued frames whose channel now has queue space.
fn flush_pending() {
    POOLS.with(|p| {
        let mut p = p.borrow_mut();
        for c in p.channels.values_mut() {
            while let Some(front) = c.pending.front() {
                match rustix::net::send(&c.fd, front, SendFlags::NOSIGNAL) {
                    Ok(n) if n == front.len() => {
                        c.pending.pop_front();
                    }
                    _ => break,
                }
            }
        }
    });
}

fn motion_to_frame(ptr: u64, seq: i32) -> Option<InputFrame> {
    read_motion(ptr, |s| InputFrame {
        kind: KIND_MOTION,
        seq,
        action: s.action,
        flags: s.flags,
        edge_flags: s.edge_flags,
        source: s.source,
        device_id: s.device_id,
        display_id: s.display_id,
        meta_state: s.meta_state,
        button_state: s.button_state,
        classification: s.classification,
        action_button: s.action_button,
        down_time: s.down_time,
        event_time: s.event_time,
        x_offset: s.x_offset,
        y_offset: s.y_offset,
        x_precision: s.x_precision,
        y_precision: s.y_precision,
        pointers: s
            .pointers
            .iter()
            .map(|p| FramePointer {
                id: p.id,
                tool_type: p.tool_type,
                x: p.x,
                y: p.y,
                pressure: p.pressure,
                size: p.size,
            })
            .collect(),
        ..InputFrame::default()
    })
}

fn ies_send_motion(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    me_hit(ctx, "nativeSendMotionEvent");
    let sender = vm.arg_i64(ctx, 0) as u64;
    let seq = vm.arg_i32(ctx, 1);
    let event = vm.arg_ref(ctx, 2);
    let ptr = match vm.get_field(event, "mNativePtr") {
        Ok(JavaValue::Long(v)) => v as u64,
        _ => 0,
    };
    let channel = POOLS.with(|p| p.borrow().senders.get(&sender).map(|s| s.channel));
    let Some(channel) = channel else {
        return Err(unsupported("InputEventSender: dead sender"));
    };
    let frame = motion_to_frame(ptr, seq)
        .ok_or_else(|| unsupported("InputEventSender: dead MotionEvent"))?;
    let ok = send_to_channel(channel, &frame).is_ok();
    if !ok {
        return Err(vm.throw_new(
            "Ljava/lang/RuntimeException;",
            Some("InputEventSender: channel send failed"),
        ));
    }
    vm.ret_bool(ctx, true);
    Ok(())
}

fn read_key_int(vm: &mut Vm, obj: Handle, name: &str) -> i32 {
    match vm.get_field(obj, name) {
        Ok(JavaValue::Int(v)) => v,
        _ => 0,
    }
}

fn read_key_long(vm: &mut Vm, obj: Handle, name: &str) -> i64 {
    match vm.get_field(obj, name) {
        Ok(JavaValue::Long(v)) => v,
        _ => 0,
    }
}

fn ies_send_key(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    me_hit(ctx, "nativeSendKeyEvent");
    let sender = vm.arg_i64(ctx, 0) as u64;
    let seq = vm.arg_i32(ctx, 1);
    let event = vm.arg_ref(ctx, 2);
    let channel = POOLS.with(|p| p.borrow().senders.get(&sender).map(|s| s.channel));
    let Some(channel) = channel else {
        return Err(unsupported("InputEventSender: dead sender"));
    };
    let frame = InputFrame {
        kind: KIND_KEY,
        seq,
        action: read_key_int(vm, event, "mAction"),
        flags: read_key_int(vm, event, "mFlags"),
        source: read_key_int(vm, event, "mSource"),
        device_id: read_key_int(vm, event, "mDeviceId"),
        display_id: read_key_int(vm, event, "mDisplayId"),
        meta_state: read_key_int(vm, event, "mMetaState"),
        key_code: read_key_int(vm, event, "mKeyCode"),
        scan_code: read_key_int(vm, event, "mScanCode"),
        repeat: read_key_int(vm, event, "mRepeatCount"),
        down_time: read_key_long(vm, event, "mDownTime"),
        event_time: read_key_long(vm, event, "mEventTime"),
        ..InputFrame::default()
    };
    if send_to_channel(channel, &frame).is_err() {
        return Err(vm.throw_new(
            "Ljava/lang/RuntimeException;",
            Some("InputEventSender: channel send failed"),
        ));
    }
    vm.ret_bool(ctx, true);
    Ok(())
}

// ---------------------------------------------------------------------------
// Dispatch path (called from the looper's `nativePollOnce`)
// ---------------------------------------------------------------------------

/// Drain every receiver registered on `looper` and dispatch its pending frames
/// on the current (looper) green thread.
///
/// Returns `true` when at least one frame was dispatched.
pub(crate) fn on_looper_wake(vm: &mut Vm, looper: u64) -> VmResult<bool> {
    POOLS.with(|p| p.borrow_mut().wake_polls += 1);
    flush_pending();
    let receivers: Vec<u64> = POOLS.with(|p| {
        p.borrow()
            .receivers
            .iter()
            .filter(|(_, r)| r.looper == looper)
            .map(|(id, _)| *id)
            .collect()
    });
    let mut any = false;
    for r in receivers {
        if drain_receiver(vm, r)? > 0 {
            any = true;
        }
    }
    Ok(any)
}

fn recv_one(fd: &OwnedFd) -> Option<Vec<u8>> {
    let mut buf = vec![0u8; 4096];
    match rustix::net::recv(fd, &mut buf[..], RecvFlags::DONTWAIT) {
        Ok((_, 0)) => None,
        Ok((_, n)) => Some(buf[..n].to_vec()),
        Err(rustix::io::Errno::AGAIN) => None,
        Err(_) => None,
    }
}

fn drain_receiver(vm: &mut Vm, receiver: u64) -> VmResult<usize> {
    // Read every pending record while holding the pools borrow, then drop the
    // borrow before any Java re-entry (`MotionEvent.obtain` touches the pools).
    let (bytes_list, weak) = POOLS.with(|p| {
        let p = p.borrow();
        let Some(r) = p.receivers.get(&receiver) else {
            return (Vec::new(), None);
        };
        let weak = r.weak_ref;
        let Some(c) = p.channels.get(&r.channel) else {
            return (Vec::new(), Some(weak));
        };
        let mut out = Vec::new();
        while let Some(b) = recv_one(&c.fd) {
            out.push(b);
        }
        (out, Some(weak))
    });
    let Some(weak) = weak else {
        return Ok(0);
    };
    // The receiver has now drained the socket; push any queued frames back in
    // so the fd stays readable and the next looper poll keeps dispatching.
    flush_pending();
    let mut frames = Vec::new();
    for bytes in bytes_list {
        if let Ok(f) = InputFrame::decode(&bytes) {
            frames.push(f);
        }
    }
    if frames.is_empty() {
        return Ok(0);
    }
    let mut dispatched = 0;
    for frame in frames {
        dispatch_frame(vm, weak, &frame)?;
        dispatched += 1;
    }
    if dispatched > 0 {
        POOLS.with(|p| p.borrow_mut().dispatched += dispatched as u64);
    }
    Ok(dispatched)
}

fn dispatch_frame(vm: &mut Vm, weak: Handle, frame: &InputFrame) -> VmResult<()> {
    // Resolve the live receiver through the WeakReference.
    let referent = vm
        .call_method(weak, "get", "()Ljava/lang/Object;", &[])
        .map_err(|e| match e {
            crate::javabridge::BridgeError::Vm(e) => e,
            other => VmError::Internal(other.to_string()),
        })?;
    let receiver = match referent {
        JavaValue::Object(h) if h != crate::heap::NULL => h,
        _ => return Ok(()),
    };
    let event = match frame.kind {
        KIND_KEY => build_key_event(vm, frame)?,
        _ => build_motion_event(vm, frame)?,
    };
    let event = match event {
        Some(h) => h,
        None => {
            vm.release_bridge_root(receiver);
            return Ok(());
        }
    };
    let seq = frame.seq;
    let res = vm.call_method_direct(
        receiver,
        "dispatchInputEvent",
        "(ILandroid/view/InputEvent;)V",
        &[JavaValue::Int(seq), JavaValue::Object(event)],
    );
    vm.release_bridge_root(event);
    vm.release_bridge_root(receiver);
    res.map_err(|e| match e {
        crate::javabridge::BridgeError::Vm(e) => e,
        other => VmError::Internal(other.to_string()),
    })?;
    Ok(())
}

fn build_motion_event(vm: &mut Vm, frame: &InputFrame) -> VmResult<Option<Handle>> {
    let n = frame.pointers.len().max(1);
    let props = vm
        .new_array_empty("Landroid/view/MotionEvent$PointerProperties;", n)
        .map_err(bridge_err)?;
    let coords = vm
        .new_array_empty("Landroid/view/MotionEvent$PointerCoords;", n)
        .map_err(bridge_err)?;
    for i in 0..n {
        let p = frame.pointers.get(i).copied().unwrap_or(FramePointer {
            id: 0,
            tool_type: 1,
            x: 0.0,
            y: 0.0,
            pressure: 1.0,
            size: 0.0,
        });
        let pp = vm.new_instance("android/view/MotionEvent$PointerProperties").map_err(bridge_err)?;
        vm.set_field(pp, "id", JavaValue::Int(p.id)).map_err(bridge_err)?;
        vm.set_field(pp, "toolType", JavaValue::Int(p.tool_type)).map_err(bridge_err)?;
        vm.set_array_element(props, i, JavaValue::Object(pp)).map_err(bridge_err)?;
        vm.release_bridge_root(pp);

        let pc = vm.new_instance("android/view/MotionEvent$PointerCoords").map_err(bridge_err)?;
        vm.set_field(pc, "x", JavaValue::Float(p.x)).map_err(bridge_err)?;
        vm.set_field(pc, "y", JavaValue::Float(p.y)).map_err(bridge_err)?;
        vm.set_field(pc, "pressure", JavaValue::Float(p.pressure)).map_err(bridge_err)?;
        vm.set_field(pc, "size", JavaValue::Float(p.size)).map_err(bridge_err)?;
        vm.set_array_element(coords, i, JavaValue::Object(pc)).map_err(bridge_err)?;
        vm.release_bridge_root(pc);
    }
    let sig = "(JJII[Landroid/view/MotionEvent$PointerProperties;[Landroid/view/MotionEvent$PointerCoords;IIFFIIIIII)Landroid/view/MotionEvent;";
    let ev = vm.call_static(
        "android.view.MotionEvent",
        "obtain",
        sig,
        &[
            JavaValue::Long(frame.down_time),
            JavaValue::Long(frame.event_time),
            JavaValue::Int(frame.action),
            JavaValue::Int(n as i32),
            JavaValue::Object(props),
            JavaValue::Object(coords),
            JavaValue::Int(frame.meta_state),
            JavaValue::Int(frame.button_state),
            JavaValue::Float(frame.x_precision),
            JavaValue::Float(frame.y_precision),
            JavaValue::Int(frame.device_id),
            JavaValue::Int(frame.edge_flags),
            JavaValue::Int(frame.source),
            JavaValue::Int(frame.display_id),
            JavaValue::Int(frame.flags),
            JavaValue::Int(frame.classification),
        ],
    )
    .map_err(bridge_err)?;
    vm.release_bridge_root(props);
    vm.release_bridge_root(coords);
    match ev {
        JavaValue::Object(h) if h != crate::heap::NULL => Ok(Some(h)),
        _ => Ok(None),
    }
}

fn build_key_event(vm: &mut Vm, frame: &InputFrame) -> VmResult<Option<Handle>> {
    let ev = vm
        .new_instance_ctor(
            "android/view/KeyEvent",
            "(JJII)V",
            &[
                JavaValue::Long(frame.down_time),
                JavaValue::Long(frame.event_time),
                JavaValue::Int(frame.action),
                JavaValue::Int(frame.key_code),
            ],
        )
        .map_err(bridge_err)?;
    Ok(Some(ev))
}

fn bridge_err(e: crate::javabridge::BridgeError) -> VmError {
    match e {
        crate::javabridge::BridgeError::Vm(e) => e,
        other => VmError::Internal(other.to_string()),
    }
}
