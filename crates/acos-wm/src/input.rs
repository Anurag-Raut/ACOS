//! `svc.input`: the InputDispatcher / InputChannel model.
//!
//! Each window owns an [`crate::transport::Channel`]; the dispatcher writes a
//! framed `InputMessage` to its end and the application's
//! `InputEventReceiver` reads the other. The record layout below is the one the
//! ACOS native layer implements for `NativeInputEventReceiver.consumeEvents` /
//! `MotionEvent.nativeInitialize` / `KeyEvent` (the exact `MotionEvent` native
//! set is in `research/service-layer/natives_plan.tsv`, module `view.input`).
//!
//! Properties the tests pin down:
//!
//! * hit-testing walks windows in z-order within **one session**, so two
//!   concurrent sessions never observe each other's events;
//! * every injected event gets a monotonic `seq` and a `cause_id`, and the
//!   agent-visible latency is measured from inject → deliver → ack;
//! * an ANR deadline is armed **only while an event is unacked**, so the idle
//!   state has no timer.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use crate::clock::Clock;
use crate::error::{Result, WmError};
use crate::geom::RectI;
use crate::transport::ChannelEnd;
use crate::wms::{SessionId, WindowId};

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

/// Default ANR timeout: 5 s, as `ActivityManagerService` uses.
pub const DEFAULT_ANR_TIMEOUT_NANOS: i64 = 5_000_000_000;

/// Message kind discriminant.
pub mod message_kind {
    /// Motion event.
    pub const MOTION: i32 = 1;
    /// Key event.
    pub const KEY: i32 = 2;
    /// Focus event.
    pub const FOCUS: i32 = 3;
}

/// One pointer snapshot inside a [`MotionEvent`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PointerCoords {
    /// `PointerProperties.id`.
    pub id: i32,
    /// `PointerProperties.toolType` (1 = finger).
    pub tool_type: i32,
    /// X in display coordinates.
    pub x: f32,
    /// Y in display coordinates.
    pub y: f32,
    /// Pressure `0..=1`.
    pub pressure: f32,
    /// Touch size.
    pub size: f32,
}

impl PointerCoords {
    /// A finger at `(x,y)` with full pressure.
    #[must_use]
    pub fn finger(x: f32, y: f32) -> Self {
        Self {
            id: 0,
            tool_type: 1,
            x,
            y,
            pressure: 1.0,
            size: 0.05,
        }
    }
}

/// A motion event as the app's `MotionEvent.nativeInitialize` would receive it.
#[derive(Debug, Clone, PartialEq)]
pub struct MotionEvent {
    /// `getActionMasked()`.
    pub action: i32,
    /// Index for `ACTION_POINTER_*` (0 otherwise).
    pub action_index: i32,
    /// `getActionButton()`.
    pub action_button: i32,
    /// `getFlags()`.
    pub flags: i32,
    /// `getEdgeFlags()`.
    pub edge_flags: i32,
    /// `getSource()`.
    pub source: i32,
    /// `getDeviceId()`.
    pub device_id: i32,
    /// `getDisplayId()`.
    pub display_id: i32,
    /// `getDownTime()`.
    pub down_time: i64,
    /// `getEventTime()`.
    pub event_time: i64,
    /// `getXOffset()`.
    pub x_offset: f32,
    /// `getYOffset()`.
    pub y_offset: f32,
    /// `getXPrecision()` (`-1` for touchscreen).
    pub x_precision: f32,
    /// `getYPrecision()`.
    pub y_precision: f32,
    /// Pointer snapshots.
    pub pointers: Vec<PointerCoords>,
}

impl MotionEvent {
    /// The pointer at `action_index`.
    #[must_use]
    pub fn action_pointer(&self) -> Option<PointerCoords> {
        self.pointers
            .get(self.action_index.max(0) as usize)
            .copied()
    }
}

/// A key event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyEvent {
    /// `getAction()`.
    pub action: i32,
    /// `getFlags()`.
    pub flags: i32,
    /// `getKeyCode()`.
    pub key_code: i32,
    /// `getScanCode()`.
    pub scan_code: i32,
    /// `getMetaState()`.
    pub meta_state: i32,
    /// `getDeviceId()`.
    pub device_id: i32,
    /// `getSource()`.
    pub source: i32,
    /// `getDisplayId()`.
    pub display_id: i32,
    /// `getDownTime()`.
    pub down_time: i64,
    /// `getEventTime()`.
    pub event_time: i64,
    /// `getUnicodeChar()` (0 when none).
    pub unicode_char: i32,
}

/// A focus event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FocusEvent {
    /// `hasFocus`.
    pub has_focus: bool,
    /// `inTouchMode`.
    pub in_touch_mode: bool,
}

/// The union carried by an [`InputMessage`].
#[derive(Debug, Clone, PartialEq)]
pub enum InputEvent {
    /// A motion event.
    Motion(MotionEvent),
    /// A key event.
    Key(KeyEvent),
    /// A focus event.
    Focus(FocusEvent),
}

/// A framed message on an input channel.
#[derive(Debug, Clone, PartialEq)]
pub struct InputMessage {
    /// Message kind (`message_kind::*`).
    pub kind: i32,
    /// Monotonic per-dispatcher sequence number.
    pub seq: u32,
    /// The action that caused the event.
    pub cause_id: i64,
    /// The event body.
    pub event: InputEvent,
}

impl InputMessage {
    /// Encode the message into the channel's byte framing.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(96);
        out.extend_from_slice(&self.kind.to_le_bytes());
        out.extend_from_slice(&self.seq.to_le_bytes());
        out.extend_from_slice(&self.cause_id.to_le_bytes());
        match &self.event {
            InputEvent::Motion(m) => {
                out.extend_from_slice(&m.action.to_le_bytes());
                out.extend_from_slice(&m.action_index.to_le_bytes());
                out.extend_from_slice(&m.action_button.to_le_bytes());
                out.extend_from_slice(&m.flags.to_le_bytes());
                out.extend_from_slice(&m.edge_flags.to_le_bytes());
                out.extend_from_slice(&m.source.to_le_bytes());
                out.extend_from_slice(&m.device_id.to_le_bytes());
                out.extend_from_slice(&m.display_id.to_le_bytes());
                out.extend_from_slice(&m.down_time.to_le_bytes());
                out.extend_from_slice(&m.event_time.to_le_bytes());
                out.extend_from_slice(&m.x_offset.to_le_bytes());
                out.extend_from_slice(&m.y_offset.to_le_bytes());
                out.extend_from_slice(&m.x_precision.to_le_bytes());
                out.extend_from_slice(&m.y_precision.to_le_bytes());
                out.extend_from_slice(&(m.pointers.len() as i32).to_le_bytes());
                for p in &m.pointers {
                    out.extend_from_slice(&p.id.to_le_bytes());
                    out.extend_from_slice(&p.tool_type.to_le_bytes());
                    out.extend_from_slice(&p.x.to_le_bytes());
                    out.extend_from_slice(&p.y.to_le_bytes());
                    out.extend_from_slice(&p.pressure.to_le_bytes());
                    out.extend_from_slice(&p.size.to_le_bytes());
                }
            }
            InputEvent::Key(k) => {
                for v in [
                    k.action,
                    k.flags,
                    k.key_code,
                    k.scan_code,
                    k.meta_state,
                    k.device_id,
                    k.source,
                    k.display_id,
                ] {
                    out.extend_from_slice(&v.to_le_bytes());
                }
                out.extend_from_slice(&k.down_time.to_le_bytes());
                out.extend_from_slice(&k.event_time.to_le_bytes());
                out.extend_from_slice(&k.unicode_char.to_le_bytes());
            }
            InputEvent::Focus(f) => {
                out.extend_from_slice(&i32::from(f.has_focus).to_le_bytes());
                out.extend_from_slice(&i32::from(f.in_touch_mode).to_le_bytes());
            }
        }
        out
    }

    /// Decode a message previously produced by [`InputMessage::encode`].
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let mut r = Reader::new(bytes);
        let kind = r.i32()?;
        let seq = r.i32()? as u32;
        let cause_id = r.i64()?;
        let event = match kind {
            message_kind::MOTION => {
                let action = r.i32()?;
                let action_index = r.i32()?;
                let action_button = r.i32()?;
                let flags = r.i32()?;
                let edge_flags = r.i32()?;
                let source = r.i32()?;
                let device_id = r.i32()?;
                let display_id = r.i32()?;
                let down_time = r.i64()?;
                let event_time = r.i64()?;
                let x_offset = r.f32()?;
                let y_offset = r.f32()?;
                let x_precision = r.f32()?;
                let y_precision = r.f32()?;
                let n = r.i32()?;
                let mut pointers = Vec::with_capacity(n.max(0) as usize);
                for _ in 0..n {
                    pointers.push(PointerCoords {
                        id: r.i32()?,
                        tool_type: r.i32()?,
                        x: r.f32()?,
                        y: r.f32()?,
                        pressure: r.f32()?,
                        size: r.f32()?,
                    });
                }
                InputEvent::Motion(MotionEvent {
                    action,
                    action_index,
                    action_button,
                    flags,
                    edge_flags,
                    source,
                    device_id,
                    display_id,
                    down_time,
                    event_time,
                    x_offset,
                    y_offset,
                    x_precision,
                    y_precision,
                    pointers,
                })
            }
            message_kind::KEY => InputEvent::Key(KeyEvent {
                action: r.i32()?,
                flags: r.i32()?,
                key_code: r.i32()?,
                scan_code: r.i32()?,
                meta_state: r.i32()?,
                device_id: r.i32()?,
                source: r.i32()?,
                display_id: r.i32()?,
                down_time: r.i64()?,
                event_time: r.i64()?,
                unicode_char: r.i32()?,
            }),
            message_kind::FOCUS => InputEvent::Focus(FocusEvent {
                has_focus: r.i32()? != 0,
                in_touch_mode: r.i32()? != 0,
            }),
            other => {
                return Err(WmError::Unsupported(format!(
                    "unknown InputMessage kind {other}"
                )))
            }
        };
        Ok(InputMessage {
            kind,
            seq,
            cause_id,
            event,
        })
    }
}

struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, pos: 0 }
    }
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        if self.pos + n > self.bytes.len() {
            return Err(WmError::Unsupported("truncated InputMessage".into()));
        }
        let s = &self.bytes[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }
    fn i32(&mut self) -> Result<i32> {
        Ok(i32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn i64(&mut self) -> Result<i64> {
        Ok(i64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }
    fn f32(&mut self) -> Result<f32> {
        Ok(f32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
}

/// What the dispatcher knows about a window for routing.
#[derive(Debug, Clone, PartialEq)]
pub struct WindowInfo {
    /// Window id.
    pub window: WindowId,
    /// Owning session.
    pub session: SessionId,
    /// Frame in display coordinates.
    pub frame: RectI,
    /// Z-order.
    pub z: i64,
    /// Visible.
    pub visible: bool,
    /// Can hold focus.
    pub focusable: bool,
    /// `true` when the window consumes touches.
    pub touchable: bool,
    /// Display id.
    pub display_id: i32,
}

/// A delivered-but-unacked event (for ANR accounting).
#[derive(Debug, Clone, PartialEq)]
pub struct PendingEvent {
    /// Sequence number.
    pub seq: u32,
    /// Target window.
    pub window: WindowId,
    /// Session of the target.
    pub session: SessionId,
    /// Cause id.
    pub cause_id: i64,
    /// Injection time.
    pub inject_nanos: i64,
    /// Delivery time.
    pub deliver_nanos: i64,
    /// ANR deadline.
    pub anr_deadline_nanos: i64,
    /// Whether this was a touch event.
    pub touch: bool,
}

/// An acknowledged event and its latency.
#[derive(Debug, Clone, PartialEq)]
pub struct AckRecord {
    /// Sequence number.
    pub seq: u32,
    /// Target window.
    pub window: WindowId,
    /// Cause id.
    pub cause_id: i64,
    /// Injection time.
    pub inject_nanos: i64,
    /// Delivery time.
    pub deliver_nanos: i64,
    /// Ack time.
    pub ack_nanos: i64,
    /// `ack - inject`.
    pub latency_nanos: i64,
    /// The frame sequence present when the ack arrived (set by the facade).
    pub frame_at_ack: u64,
}

/// An ANR-style timeout.
#[derive(Debug, Clone, PartialEq)]
pub struct AnrEvent {
    /// Target window.
    pub window: WindowId,
    /// The unacked sequence.
    pub seq: u32,
    /// How long it has been outstanding.
    pub waited_nanos: i64,
}

/// The input dispatcher.
pub struct InputDispatcher {
    /// Clock seam.
    pub clock: Arc<dyn Clock>,
    /// Per-window routing info.
    pub windows: HashMap<WindowId, WindowInfo>,
    /// Per-window dispatcher channel ends.
    channels: HashMap<WindowId, Arc<dyn ChannelEnd>>,
    /// Currently focused window (global; sessions are isolated by routing).
    pub focus: Option<WindowId>,
    /// Sequence allocator.
    next_seq: u32,
    /// Outstanding events by sequence.
    pending: BTreeMap<u32, PendingEvent>,
    /// Ack history.
    acks: Vec<AckRecord>,
    /// ANR events generated.
    anrs: Vec<AnrEvent>,
    /// The single armed ANR deadline.
    armed_anr: Option<i64>,
    /// ANR timeout.
    pub anr_timeout_nanos: i64,
    /// Frames presented at last ack (mirrors the compositor).
    pub current_frame_seq: u64,
}

impl std::fmt::Debug for InputDispatcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InputDispatcher")
            .field("windows", &self.windows.len())
            .field("focus", &self.focus)
            .field("pending", &self.pending.len())
            .field("acks", &self.acks.len())
            .field("armed_anr", &self.armed_anr)
            .finish()
    }
}

impl InputDispatcher {
    /// A dispatcher with no windows.
    #[must_use]
    pub fn new(clock: Arc<dyn Clock>) -> Self {
        Self {
            clock,
            windows: HashMap::new(),
            channels: HashMap::new(),
            focus: None,
            next_seq: 1,
            pending: BTreeMap::new(),
            acks: Vec::new(),
            anrs: Vec::new(),
            armed_anr: None,
            anr_timeout_nanos: DEFAULT_ANR_TIMEOUT_NANOS,
            current_frame_seq: 0,
        }
    }

    /// Register (or update) a window and its dispatcher channel end.
    pub fn register_window(&mut self, info: WindowInfo, channel: Arc<dyn ChannelEnd>) {
        if info.focusable && self.focus.is_none() {
            self.focus = Some(info.window);
        }
        self.channels.insert(info.window, channel);
        self.windows.insert(info.window, info);
    }

    /// Remove a window.
    pub fn remove_window(&mut self, window: WindowId) {
        self.windows.remove(&window);
        self.channels.remove(&window);
        if self.focus == Some(window) {
            self.focus = None;
        }
    }

    /// Set focus explicitly (within `session`).
    pub fn set_focus(&mut self, session: SessionId, window: WindowId) {
        if let Some(w) = self.windows.get(&window) {
            if w.session == session {
                self.focus = Some(window);
            }
        }
    }

    /// The focused window in `session`.
    #[must_use]
    pub fn focus_for(&self, session: SessionId) -> Option<WindowId> {
        self.focus.filter(|w| {
            self.windows
                .get(w)
                .is_some_and(|info| info.session == session)
        })
    }

    /// The topmost touchable window in `session` containing `(x,y)`.
    #[must_use]
    pub fn hit_test(&self, session: SessionId, x: f32, y: f32) -> Option<WindowId> {
        let xi = x.floor() as i32;
        let yi = y.floor() as i32;
        self.windows
            .values()
            .filter(|w| {
                w.session == session && w.visible && w.touchable && w.frame.contains(xi, yi)
            })
            .max_by_key(|w| (w.z, w.window))
            .map(|w| w.window)
    }

    fn target_for(&self, session: SessionId, event: &InputEvent) -> Option<WindowId> {
        match event {
            InputEvent::Motion(m) => {
                let p = m.action_pointer()?;
                self.hit_test(session, p.x, p.y)
            }
            InputEvent::Key(_) | InputEvent::Focus(_) => self.focus_for(session),
        }
    }

    fn deliver(&mut self, session: SessionId, event: InputEvent, cause_id: i64) -> Result<u32> {
        let target = self
            .target_for(session, &event)
            .ok_or_else(|| WmError::Unsupported("no input target".into()))?;
        let channel = self
            .channels
            .get(&target)
            .cloned()
            .ok_or(WmError::NoCallback(target))?;
        let now = self.clock.now_nanos();
        let seq = self.next_seq;
        self.next_seq += 1;
        let touch = matches!(event, InputEvent::Motion(_));
        let msg = InputMessage {
            kind: match &event {
                InputEvent::Motion(_) => message_kind::MOTION,
                InputEvent::Key(_) => message_kind::KEY,
                InputEvent::Focus(_) => message_kind::FOCUS,
            },
            seq,
            cause_id,
            event,
        };
        channel.send(&msg.encode())?;
        let anr_deadline = now + self.anr_timeout_nanos;
        self.pending.insert(
            seq,
            PendingEvent {
                seq,
                window: target,
                session,
                cause_id,
                inject_nanos: now,
                deliver_nanos: self.clock.now_nanos(),
                anr_deadline_nanos: anr_deadline,
                touch,
            },
        );
        if self.armed_anr.is_none() {
            self.armed_anr = Some(anr_deadline);
            self.clock.arm_timer(anr_deadline);
        }
        Ok(seq)
    }

    /// `finishInputEvent`: acknowledge one event.
    pub fn finish_input_event(&mut self, seq: u32) -> Result<AckRecord> {
        let p = self
            .pending
            .remove(&seq)
            .ok_or(WmError::UnknownInputSequence(u64::from(seq)))?;
        let now = self.clock.now_nanos();
        let rec = AckRecord {
            seq,
            window: p.window,
            cause_id: p.cause_id,
            inject_nanos: p.inject_nanos,
            deliver_nanos: p.deliver_nanos,
            ack_nanos: now,
            latency_nanos: now - p.inject_nanos,
            frame_at_ack: self.current_frame_seq,
        };
        self.acks.push(rec.clone());
        if self.pending.is_empty() {
            self.clock.disarm_timer();
            self.armed_anr = None;
        }
        Ok(rec)
    }

    /// Deliver a focus event to the focused window.
    pub fn inject_focus(&mut self, session: SessionId, has_focus: bool) -> Result<u32> {
        let in_touch_mode = true;
        self.deliver(
            session,
            InputEvent::Focus(FocusEvent {
                has_focus,
                in_touch_mode,
            }),
            -1,
        )
    }

    /// Inject a key press/release pair.
    pub fn inject_key(
        &mut self,
        session: SessionId,
        key_code: i32,
        cause_id: i64,
    ) -> Result<Vec<u32>> {
        let clock = self.clock.clone();
        let now = clock.now_nanos();
        let mk = |action: i32| KeyEvent {
            action,
            flags: 0,
            key_code,
            scan_code: 0,
            meta_state: 0,
            device_id: 0,
            source: SOURCE_KEYBOARD,
            display_id: 0,
            down_time: now,
            event_time: clock.now_nanos(),
            unicode_char: 0,
        };
        let down = self.deliver(session, InputEvent::Key(mk(KEY_ACTION_DOWN)), cause_id)?;
        let up = self.deliver(session, InputEvent::Key(mk(KEY_ACTION_UP)), cause_id)?;
        Ok(vec![down, up])
    }

    /// Inject a tap (DOWN then UP at the same point).
    pub fn inject_tap(
        &mut self,
        session: SessionId,
        x: f32,
        y: f32,
        cause_id: i64,
    ) -> Result<Vec<u32>> {
        let clock = self.clock.clone();
        let now = clock.now_nanos();
        let mk = |action: i32, t: i64| MotionEvent {
            action,
            action_index: 0,
            action_button: 0,
            flags: 0,
            edge_flags: 0,
            source: SOURCE_TOUCHSCREEN,
            device_id: 0,
            display_id: 0,
            down_time: now,
            event_time: t,
            x_offset: 0.0,
            y_offset: 0.0,
            x_precision: -1.0,
            y_precision: -1.0,
            pointers: vec![PointerCoords::finger(x, y)],
        };
        let down = self.deliver(session, InputEvent::Motion(mk(ACTION_DOWN, now)), cause_id)?;
        // The UP is timestamped a nominal 50 ms later.
        let up = self.deliver(
            session,
            InputEvent::Motion(mk(ACTION_UP, now + 50_000_000)),
            cause_id,
        )?;
        Ok(vec![down, up])
    }

    /// Inject an interpolated swipe: DOWN, N MOVEs, UP.
    pub fn inject_swipe(
        &mut self,
        session: SessionId,
        from: (f32, f32),
        to: (f32, f32),
        duration_nanos: i64,
        steps: usize,
        cause_id: i64,
    ) -> Result<Vec<u32>> {
        let steps = steps.max(1);
        let clock = self.clock.clone();
        let now = clock.now_nanos();
        let mk = |action: i32, x: f32, y: f32, t: i64| MotionEvent {
            action,
            action_index: 0,
            action_button: 0,
            flags: 0,
            edge_flags: 0,
            source: SOURCE_TOUCHSCREEN,
            device_id: 0,
            display_id: 0,
            down_time: now,
            event_time: t,
            x_offset: 0.0,
            y_offset: 0.0,
            x_precision: -1.0,
            y_precision: -1.0,
            pointers: vec![PointerCoords::finger(x, y)],
        };
        let mut seqs = Vec::with_capacity(steps + 2);
        let start = clock.now_nanos();
        seqs.push(self.deliver(
            session,
            InputEvent::Motion(mk(ACTION_DOWN, from.0, from.1, start)),
            cause_id,
        )?);
        for i in 1..=steps {
            let f = i as f32 / (steps + 1) as f32;
            let x = from.0 + (to.0 - from.0) * f;
            let y = from.1 + (to.1 - from.1) * f;
            let t = start + (duration_nanos as f32 * f) as i64;
            seqs.push(self.deliver(
                session,
                InputEvent::Motion(mk(ACTION_MOVE, x, y, t)),
                cause_id,
            )?);
        }
        let end = start + duration_nanos;
        seqs.push(self.deliver(
            session,
            InputEvent::Motion(mk(ACTION_UP, to.0, to.1, end)),
            cause_id,
        )?);
        Ok(seqs)
    }

    /// The armed ANR deadline, if any.
    #[must_use]
    pub fn next_deadline(&self) -> Option<i64> {
        self.armed_anr
    }

    /// `true` when no event is outstanding and no timer is armed.
    #[must_use]
    pub fn is_idle(&self) -> bool {
        self.pending.is_empty() && self.armed_anr.is_none()
    }

    /// Outstanding events.
    #[must_use]
    pub fn pending(&self) -> &BTreeMap<u32, PendingEvent> {
        &self.pending
    }

    /// Ack history.
    #[must_use]
    pub fn acks(&self) -> &[AckRecord] {
        &self.acks
    }

    /// ANR events produced so far.
    #[must_use]
    pub fn anrs(&self) -> &[AnrEvent] {
        &self.anrs
    }

    /// Detect ANR timeouts at `now`. Returns the events produced.
    pub fn poll_anr(&mut self, now: i64) -> Vec<AnrEvent> {
        let Some(deadline) = self.armed_anr else {
            return Vec::new();
        };
        if now < deadline {
            return Vec::new();
        }
        let due: Vec<AnrEvent> = self
            .pending
            .values()
            .filter(|p| p.anr_deadline_nanos <= now)
            .map(|p| AnrEvent {
                window: p.window,
                seq: p.seq,
                waited_nanos: now - p.deliver_nanos,
            })
            .collect();
        if due.is_empty() {
            self.clock.disarm_timer();
            self.armed_anr = None;
            return Vec::new();
        }
        let next = self
            .pending
            .values()
            .map(|p| p.anr_deadline_nanos)
            .filter(|d| *d > now)
            .min();
        for e in &due {
            self.anrs.push(e.clone());
        }
        match next {
            Some(next) if next > now => {
                self.armed_anr = Some(next);
                self.clock.arm_timer(next);
            }
            _ => {
                self.clock.disarm_timer();
                self.armed_anr = None;
            }
        }
        due
    }

    /// Read one pending message from a window's application end.
    pub fn read_from_app(&self, window: WindowId) -> Result<Option<InputMessage>> {
        // The dispatcher does not own the app end; the transport registry does.
        // This helper exists for the facade to decode bytes it reads elsewhere.
        let _ = window;
        Err(WmError::Unsupported(
            "use the registered app ChannelEnd to read".into(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::VirtualClock;
    use crate::transport::Channel;

    fn info(window: WindowId, session: SessionId, frame: RectI, z: i64) -> WindowInfo {
        WindowInfo {
            window,
            session,
            frame,
            z,
            visible: true,
            focusable: true,
            touchable: true,
            display_id: 0,
        }
    }

    fn setup() -> (Arc<VirtualClock>, InputDispatcher, Channel, Channel) {
        let clock = Arc::new(VirtualClock::new());
        let mut d = InputDispatcher::new(clock.clone());
        let c1 = Channel::in_memory("w1", 0);
        let c2 = Channel::in_memory("w2", 1);
        d.register_window(
            info(1, 100, RectI::new(0, 0, 1080, 2400), 1),
            c1.dispatcher.clone(),
        );
        d.register_window(
            info(2, 100, RectI::new(0, 0, 540, 600), 2),
            c2.dispatcher.clone(),
        );
        // Return the app ends for reading.
        (clock, d, c1, c2)
    }

    #[test]
    fn hit_test_prefers_topmost_z() {
        let (_c, d, _c1, _c2) = setup();
        // (100,100) is inside both; the top layer (z=2) wins.
        assert_eq!(d.hit_test(100, 100.0, 100.0), Some(2));
        // (700,2000) is only inside the full-screen layer (z=1).
        assert_eq!(d.hit_test(100, 700.0, 2000.0), Some(1));
        // Outside every window.
        assert_eq!(d.hit_test(100, -1.0, -1.0), None);
    }

    #[test]
    fn sessions_never_see_each_others_events() {
        let (_c, mut d, _c1, c2) = setup();
        // Session 200 has one window over the same area.
        let c3 = Channel::in_memory("w3", 2);
        d.register_window(
            info(3, 200, RectI::new(0, 0, 1080, 2400), 5),
            c3.dispatcher.clone(),
        );
        let seqs = d.inject_tap(200, 100.0, 100.0, 7).unwrap();
        assert_eq!(seqs.len(), 2);
        // The event landed on window 3's channel, not window 2's.
        assert!(c3.app.recv().unwrap().is_some());
        assert!(c2.app.recv().unwrap().is_none());
        for s in seqs {
            d.finish_input_event(s).unwrap();
        }
    }

    #[test]
    fn tap_delivers_down_and_up_with_ack_latency() {
        let (_c, mut d, _c1, c2) = setup();
        let seqs = d.inject_tap(100, 300.0, 400.0, 42).unwrap();
        let down = InputMessage::decode(&c2.app.recv().unwrap().unwrap()).unwrap();
        let up = InputMessage::decode(&c2.app.recv().unwrap().unwrap()).unwrap();
        assert!(matches!(down.event, InputEvent::Motion(_)));
        assert_eq!(down.cause_id, 42);
        if let InputEvent::Motion(m) = &down.event {
            assert_eq!(m.action, ACTION_DOWN);
            assert_eq!(m.pointers[0].x, 300.0);
        }
        if let InputEvent::Motion(m) = &up.event {
            assert_eq!(m.action, ACTION_UP);
        }
        assert!(d.next_deadline().is_some(), "unacked arms the ANR timer");
        let ack = d.finish_input_event(seqs[0]).unwrap();
        assert_eq!(ack.cause_id, 42);
        assert!(ack.latency_nanos >= 0);
        d.finish_input_event(seqs[1]).unwrap();
        assert!(d.is_idle(), "all acked => timer disarmed");
        assert!(d.next_deadline().is_none());
    }

    #[test]
    fn swipe_interpolation_action_sequence() {
        let (clock, mut d, _c1, c2) = setup();
        let seqs = d
            .inject_swipe(100, (100.0, 100.0), (400.0, 500.0), 100_000_000, 3, 9)
            .unwrap();
        assert_eq!(seqs.len(), 5); // DOWN + 3 MOVE + UP
        let mut actions = Vec::new();
        let mut xs = Vec::new();
        for _ in 0..5 {
            let m = InputMessage::decode(&c2.app.recv().unwrap().unwrap()).unwrap();
            if let InputEvent::Motion(me) = m.event {
                actions.push(me.action);
                xs.push(me.pointers[0].x);
            }
        }
        assert_eq!(
            actions,
            vec![
                ACTION_DOWN,
                ACTION_MOVE,
                ACTION_MOVE,
                ACTION_MOVE,
                ACTION_UP
            ]
        );
        assert!(xs[0] == 100.0 && xs[4] == 400.0);
        assert!(xs[1] < xs[2] && xs[2] < xs[3]);
        let _ = clock;
    }

    #[test]
    fn anr_arms_only_while_unacked() {
        let (clock, mut d, _c1, _c2) = setup();
        assert_eq!(clock.arm_count(), 0, "idle input has no timer");
        let seqs = d.inject_tap(100, 100.0, 100.0, 1).unwrap();
        assert_eq!(clock.arm_count(), 1);
        clock.advance(DEFAULT_ANR_TIMEOUT_NANOS + 1);
        let anrs = d.poll_anr(clock.now_nanos());
        assert!(!anrs.is_empty());
        for s in seqs {
            let _ = d.finish_input_event(s);
        }
        assert!(d.is_idle());
    }

    #[test]
    fn unknown_ack_is_structured() {
        let (_c, mut d, _c1, _c2) = setup();
        assert!(matches!(
            d.finish_input_event(999),
            Err(WmError::UnknownInputSequence(999))
        ));
    }

    #[test]
    fn message_round_trip_is_exact() {
        let m = InputMessage {
            kind: message_kind::MOTION,
            seq: 3,
            cause_id: -5,
            event: InputEvent::Motion(MotionEvent {
                action: ACTION_MOVE,
                action_index: 0,
                action_button: 0,
                flags: 1,
                edge_flags: 2,
                source: SOURCE_TOUCHSCREEN,
                device_id: 0,
                display_id: 0,
                down_time: 10,
                event_time: 20,
                x_offset: 0.5,
                y_offset: 1.5,
                x_precision: -1.0,
                y_precision: -1.0,
                pointers: vec![PointerCoords::finger(1.0, 2.0)],
            }),
        };
        let bytes = m.encode();
        assert_eq!(InputMessage::decode(&bytes).unwrap(), m);
    }
}
