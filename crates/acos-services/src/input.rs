//! `svc.input`: the service-side input dispatcher (BACKLOG T-36).
//!
//! `acos-wm` owns the *policy-free* input state machine (its `input` module
//! frames an `InputMessage` and can be unit-tested with an in-memory channel).
//! This module is the **service-object** face of the dispatcher: it is driven by
//! the real `acos-wm` window state ([`WindowRecord`]) so hit-testing uses the
//! same frames and z-order the WMS reports, and it writes each event to the
//! window's server-end `InputChannel` through a [`FrameWriter`] the runtime
//! installs.
//!
//! The split matters because the receiver's wire format is the VM layer's
//! business (`acos-vm::natives::fwk_input::InputFrame`), not the service
//! layer's: `acos-services` does not depend on the VM. The runtime bridge
//! (`acos-runtime::input_java`) implements [`FrameWriter`] by encoding the
//! frame the way the real `InputEventReceiver` reads it, and implements the
//! native ack hook that feeds [`InputDispatcher::finish_event`].
//!
//! Everything here is deterministic and fd-free: injecting a tap at the same
//! point twice produces the same sequence of frames, which is what the replay
//! harness asserts.
//!
//! # Structured errors
//!
//! A tap outside every window, a key with no focused window or a missing
//! channel returns [`InputError`] — never an `Ok(())` with nothing delivered.

use std::collections::BTreeMap;
use std::sync::Arc;

use acos_wm::geom::RectI;
use acos_wm::input::{
    InputEvent, KeyEvent, MotionEvent, PointerCoords, KEY_ACTION_DOWN, KEY_ACTION_UP,
    SOURCE_TOUCHSCREEN,
};
use acos_wm::wms::{WindowId, WindowRecord};

/// Deterministic base time for injected events (`1 s` in nanoseconds).
const INJECT_BASE_NANOS: i64 = 1_000_000_000;
/// The nominal DOWN→UP delay (50 ms), matching the `acos-wm` dispatcher.
const TAP_HOLD_NANOS: i64 = 50_000_000;

/// A structured input-dispatch failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputError {
    /// No visible, touchable window contains the point.
    NoWindowAtPoint {
        /// X in display coordinates.
        x: i32,
        /// Y in display coordinates.
        y: i32,
    },
    /// No window currently holds focus (key/focus dispatch).
    NoFocusedWindow,
    /// The target window has no registered channel.
    NoChannel(WindowId),
    /// The receiver acknowledged a sequence that was never dispatched.
    UnknownSequence(u32),
    /// The channel write failed.
    Write {
        /// Target window.
        window: WindowId,
        /// Failure detail.
        detail: String,
    },
}

impl std::fmt::Display for InputError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            InputError::NoWindowAtPoint { x, y } => {
                write!(f, "no window at ({x},{y})")
            }
            InputError::NoFocusedWindow => write!(f, "no focused window"),
            InputError::NoChannel(w) => write!(f, "window {w} has no input channel"),
            InputError::UnknownSequence(s) => write!(f, "unknown input sequence {s}"),
            InputError::Write { window, detail } => {
                write!(f, "window {window} input write failed: {detail}")
            }
        }
    }
}

impl std::error::Error for InputError {}

/// One event queued for a window's receiver, in the receiver's format once the
/// runtime [`FrameWriter`] encodes it.
#[derive(Debug, Clone, PartialEq)]
pub struct DispatchFrame {
    /// Receiver-local sequence number (also the ack key).
    pub seq: i32,
    /// The event body.
    pub event: InputEvent,
}

/// The runtime-owned sink that writes frames to a window's server-end channel.
///
/// Implemented in `acos-runtime::input_java`, which encodes the event exactly as
/// `InputEventReceiver` reads it (the `fwk_input::InputFrame` record) and sends
/// it on the dispatcher end of the window's real socketpair.
pub trait FrameWriter: Send + Sync + std::fmt::Debug {
    /// Write one frame; the error is surfaced as [`InputError::Write`].
    fn write_frame(&self, frame: &DispatchFrame) -> Result<(), String>;
}

/// A window routed by the dispatcher.
#[derive(Debug)]
struct WindowTarget {
    frame: RectI,
    z: i64,
    visible: bool,
    focusable: bool,
    touchable: bool,
    writer: Arc<dyn FrameWriter>,
}

/// A delivered-but-unacknowledged event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingEvent {
    /// Sequence number.
    pub seq: u32,
    /// Target window.
    pub window: WindowId,
    /// `true` for motion events.
    pub touch: bool,
}

/// An acknowledged event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AckRecord {
    /// Sequence number.
    pub seq: u32,
    /// Target window.
    pub window: WindowId,
}

/// The service-side input dispatcher.
#[derive(Debug, Default)]
pub struct InputDispatcher {
    windows: BTreeMap<WindowId, WindowTarget>,
    focus: Option<WindowId>,
    next_seq: u32,
    pending: BTreeMap<u32, PendingEvent>,
    acks: Vec<AckRecord>,
    /// A DOWN delivered by [`InputDispatcher::inject_tap_down`] awaiting its
    /// matching UP. The target is pinned so a relayout/z-order change between
    /// the two halves cannot reroute the UP to a different window.
    active_tap: Option<WindowId>,
}

impl InputDispatcher {
    /// An empty dispatcher.
    #[must_use]
    pub fn new() -> Self {
        Self {
            next_seq: 1,
            ..Self::default()
        }
    }

    /// Register (or update) a window from its WMS record and channel writer.
    ///
    /// `touchable` defaults to `focusable || visible`; an application window is
    /// a touch target. The first focusable window becomes the focus, matching
    /// `WindowManager`'s launch order.
    pub fn register_window(&mut self, record: &WindowRecord, writer: Arc<dyn FrameWriter>) {
        let target = WindowTarget {
            frame: record.frame,
            z: record.z,
            visible: record.visible,
            focusable: record.focusable,
            touchable: record.visible,
            writer,
        };
        if target.focusable && self.focus.is_none() {
            self.focus = Some(record.id);
        }
        self.windows.insert(record.id, target);
    }

    /// Remove a window and drop its pending routes.
    pub fn remove_window(&mut self, window: WindowId) {
        self.windows.remove(&window);
        if self.active_tap == Some(window) {
            self.active_tap = None;
        }
        if self.focus == Some(window) {
            self.focus = self
                .windows
                .iter()
                .filter(|(_, w)| w.focusable)
                .max_by_key(|(id, w)| (w.z, *id))
                .map(|(id, _)| *id);
        }
    }

    /// Update the routing frame after a `relayout`.
    pub fn update_frame(&mut self, window: WindowId, frame: RectI) {
        if let Some(w) = self.windows.get_mut(&window) {
            w.frame = frame;
        }
    }

    /// The currently focused window.
    #[must_use]
    pub fn focus(&self) -> Option<WindowId> {
        self.focus
    }

    /// Set focus to `window` if it is registered and focusable.
    pub fn set_focus(&mut self, window: WindowId) -> bool {
        if self.windows.get(&window).is_some_and(|w| w.focusable) {
            self.focus = Some(window);
            true
        } else {
            false
        }
    }

    /// The topmost visible, touchable window containing `(x,y)`.
    #[must_use]
    pub fn hit_test(&self, x: f32, y: f32) -> Option<WindowId> {
        let xi = x.floor() as i32;
        let yi = y.floor() as i32;
        self.windows
            .iter()
            .filter(|(_, w)| w.visible && w.touchable && w.frame.contains(xi, yi))
            .max_by_key(|(id, w)| (w.z, **id))
            .map(|(id, _)| *id)
    }

    fn writer_for(&self, window: WindowId) -> Result<Arc<dyn FrameWriter>, InputError> {
        self.windows
            .get(&window)
            .map(|w| Arc::clone(&w.writer))
            .ok_or(InputError::NoChannel(window))
    }

    fn deliver(&mut self, window: WindowId, event: InputEvent) -> Result<u32, InputError> {
        let writer = self.writer_for(window)?;
        // `inject_*` takes display (screen) coordinates, matching the real
        // input system. A window receives events in its own coordinate space:
        // the dispatcher records the window origin as a negative
        // `mXOffset`/`mYOffset`, and the VM applies it to the event before
        // dispatch, so `MotionEvent.getX()` is window-local while `getRawX()`
        // (which is `getX() - mXOffset`) stays in display coordinates.
        let origin = self.windows.get(&window).map(|w| w.frame);
        let event = match (event, origin) {
            (InputEvent::Motion(mut m), Some(f)) => {
                m.x_offset = -(f.left as f32);
                m.y_offset = -(f.top as f32);
                InputEvent::Motion(m)
            }
            (e, _) => e,
        };
        let seq = self.next_seq;
        let frame = DispatchFrame {
            seq: seq as i32,
            event,
        };
        writer
            .write_frame(&frame)
            .map_err(|detail| InputError::Write { window, detail })?;
        // Only a delivered frame consumes a sequence.
        self.next_seq += 1;
        let touch = matches!(frame.event, InputEvent::Motion(_));
        self.pending.insert(
            seq,
            PendingEvent {
                seq,
                window,
                touch,
            },
        );
        Ok(seq)
    }

    /// Inject a DOWN/UP pair at `(x,y)`.
    ///
    /// Returns [`InputError::NoWindowAtPoint`] when no window contains the
    /// point; nothing is written in that case.
    pub fn inject_tap(&mut self, x: f32, y: f32) -> Result<Vec<u32>, InputError> {
        let target = self.hit_test(x, y).ok_or(InputError::NoWindowAtPoint {
            x: x.floor() as i32,
            y: y.floor() as i32,
        })?;
        let down = self.deliver(target, InputEvent::Motion(motion(0, x, y, INJECT_BASE_NANOS)))?;
        let up = self.deliver(
            target,
            InputEvent::Motion(motion(1, x, y, INJECT_BASE_NANOS + TAP_HOLD_NANOS)),
        )?;
        Ok(vec![down, up])
    }

    /// Inject the DOWN half of a tap and remember its target.
    ///
    /// A real tap delivers DOWN and UP in **separate** looper passes: the
    /// widget's posted tap/long-press scheduling runs between them. Batching
    /// both events in one channel drain skips that window, so a `ListView`
    /// never posts its `PerformClick`. The runtime calls this, pumps the
    /// looper, then calls [`InputDispatcher::inject_tap_up`].
    pub fn inject_tap_down(&mut self, x: f32, y: f32) -> Result<Vec<u32>, InputError> {
        let target = self.hit_test(x, y).ok_or(InputError::NoWindowAtPoint {
            x: x.floor() as i32,
            y: y.floor() as i32,
        })?;
        let seq = self.deliver(target, InputEvent::Motion(motion(0, x, y, INJECT_BASE_NANOS)))?;
        self.active_tap = Some(target);
        Ok(vec![seq])
    }

    /// Inject the UP half of a tap started by [`InputDispatcher::inject_tap_down`].
    pub fn inject_tap_up(&mut self, x: f32, y: f32) -> Result<Vec<u32>, InputError> {
        let target = self.active_tap.take().or_else(|| self.hit_test(x, y)).ok_or(
            InputError::NoWindowAtPoint {
                x: x.floor() as i32,
                y: y.floor() as i32,
            },
        )?;
        let seq = self.deliver(
            target,
            InputEvent::Motion(motion(1, x, y, INJECT_BASE_NANOS + TAP_HOLD_NANOS)),
        )?;
        Ok(vec![seq])
    }

    /// Inject an interpolated swipe: DOWN, `steps` MOVEs, UP.
    pub fn inject_swipe(
        &mut self,
        from: (f32, f32),
        to: (f32, f32),
        duration_nanos: i64,
        steps: usize,
    ) -> Result<Vec<u32>, InputError> {
        let target = self.hit_test(from.0, from.1).ok_or(InputError::NoWindowAtPoint {
            x: from.0.floor() as i32,
            y: from.1.floor() as i32,
        })?;
        let steps = steps.max(1);
        let mut seqs = Vec::with_capacity(steps + 2);
        seqs.push(self.deliver(
            target,
            InputEvent::Motion(motion(0, from.0, from.1, INJECT_BASE_NANOS)),
        )?);
        for i in 1..=steps {
            let f = i as f32 / (steps + 1) as f32;
            let x = from.0 + (to.0 - from.0) * f;
            let y = from.1 + (to.1 - from.1) * f;
            let t = INJECT_BASE_NANOS + (duration_nanos as f32 * f) as i64;
            seqs.push(self.deliver(target, InputEvent::Motion(motion(2, x, y, t)))?);
        }
        seqs.push(self.deliver(
            target,
            InputEvent::Motion(motion(1, to.0, to.1, INJECT_BASE_NANOS + duration_nanos)),
        )?);
        Ok(seqs)
    }

    /// Inject a key press/release pair to the focused window.
    pub fn inject_key(&mut self, key_code: i32) -> Result<Vec<u32>, InputError> {
        let target = self.focus.ok_or(InputError::NoFocusedWindow)?;
        let down = self.deliver(target, InputEvent::Key(key_event(KEY_ACTION_DOWN, key_code)))?;
        let up = self.deliver(target, InputEvent::Key(key_event(KEY_ACTION_UP, key_code)))?;
        Ok(vec![down, up])
    }

    /// `finishInputEvent`: acknowledge exactly one dispatched sequence.
    pub fn finish_event(&mut self, seq: u32) -> Result<AckRecord, InputError> {
        let p = self
            .pending
            .remove(&seq)
            .ok_or(InputError::UnknownSequence(seq))?;
        let rec = AckRecord {
            seq,
            window: p.window,
        };
        self.acks.push(rec.clone());
        Ok(rec)
    }

    /// Outstanding (unacknowledged) events.
    #[must_use]
    pub fn pending(&self) -> &BTreeMap<u32, PendingEvent> {
        &self.pending
    }

    /// Acknowledged events.
    #[must_use]
    pub fn acks(&self) -> &[AckRecord] {
        &self.acks
    }

    /// `true` when nothing is outstanding.
    #[must_use]
    pub fn is_idle(&self) -> bool {
        self.pending.is_empty()
    }

    /// Number of routed windows.
    #[must_use]
    pub fn window_count(&self) -> usize {
        self.windows.len()
    }
}

fn motion(action: i32, x: f32, y: f32, event_time: i64) -> MotionEvent {
    MotionEvent {
        action,
        action_index: 0,
        action_button: 0,
        flags: 0,
        edge_flags: 0,
        source: SOURCE_TOUCHSCREEN,
        device_id: 0,
        display_id: 0,
        down_time: INJECT_BASE_NANOS,
        event_time,
        x_offset: 0.0,
        y_offset: 0.0,
        x_precision: -1.0,
        y_precision: -1.0,
        pointers: vec![PointerCoords::finger(x, y)],
    }
}

fn key_event(action: i32, key_code: i32) -> KeyEvent {
    KeyEvent {
        action,
        flags: 0,
        key_code,
        scan_code: 0,
        meta_state: 0,
        device_id: 0,
        source: acos_wm::input::SOURCE_KEYBOARD,
        display_id: 0,
        down_time: INJECT_BASE_NANOS,
        event_time: INJECT_BASE_NANOS,
        unicode_char: 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use acos_wm::codec::LayoutParams;
    use acos_wm::display::DisplaySpec;
    use acos_wm::wms::ADD_OKAY;
    use std::sync::Mutex;

    #[derive(Debug, Default)]
    struct RecordingWriter {
        frames: Mutex<Vec<DispatchFrame>>,
    }

    impl FrameWriter for RecordingWriter {
        fn write_frame(&self, frame: &DispatchFrame) -> Result<(), String> {
            self.frames.lock().unwrap().push(frame.clone());
            Ok(())
        }
    }

    struct FailingWriter;

    impl std::fmt::Debug for FailingWriter {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("FailingWriter")
        }
    }

    impl FrameWriter for FailingWriter {
        fn write_frame(&self, _frame: &DispatchFrame) -> Result<(), String> {
            Err("closed".into())
        }
    }

    fn add_window(wm: &mut acos_wm::wms::WindowManager) -> WindowRecord {
        let (session, _) = wm.open_session(None);
        let add = wm
            .add_to_display_as_user(
                session,
                acos_parcel::binder::BinderId(0x11),
                LayoutParams::application("dev.acos.fixtures.helloviews", "MainActivity"),
                0,
            )
            .unwrap();
        assert_eq!(add.result, ADD_OKAY);
        wm.windows.get(&add.window).unwrap().clone()
    }

    #[test]
    fn tap_hits_the_window_and_writes_down_up() {
        let mut wm = acos_wm::wms::WindowManager::new(DisplaySpec::reference_1080x2400());
        let record = add_window(&mut wm);
        let writer = Arc::new(RecordingWriter::default());
        let mut d = InputDispatcher::new();
        d.register_window(&record, writer.clone());

        let seqs = d.inject_tap(540.0, 427.0).unwrap();
        assert_eq!(seqs, vec![1, 2]);
        let frames = writer.frames.lock().unwrap().clone();
        assert_eq!(frames.len(), 2);
        match (&frames[0].event, &frames[1].event) {
            (InputEvent::Motion(down), InputEvent::Motion(up)) => {
                assert_eq!(down.action, 0, "ACTION_DOWN");
                assert_eq!(up.action, 1, "ACTION_UP");
                assert_eq!(down.pointers[0].x, 540.0);
                assert_eq!(down.pointers[0].y, 427.0);
                assert_eq!(up.pointers[0].x, 540.0);
            }
            other => panic!("unexpected frames {other:?}"),
        }
        assert_eq!(d.pending().len(), 2);
        d.finish_event(1).unwrap();
        d.finish_event(2).unwrap();
        assert!(d.is_idle());
        assert_eq!(d.acks().len(), 2);
    }

    #[test]
    fn tap_outside_every_window_is_structured_and_writes_nothing() {
        let mut wm = acos_wm::wms::WindowManager::new(DisplaySpec::reference_1080x2400());
        let record = add_window(&mut wm);
        let writer = Arc::new(RecordingWriter::default());
        let mut d = InputDispatcher::new();
        d.register_window(&record, writer.clone());

        let err = d.inject_tap(5.0, 5.0).unwrap_err();
        assert_eq!(err, InputError::NoWindowAtPoint { x: 5, y: 5 });
        assert!(writer.frames.lock().unwrap().is_empty());
        assert!(d.is_idle());
    }

    #[test]
    fn key_requires_focus_and_ack_is_exactly_once() {
        let mut wm = acos_wm::wms::WindowManager::new(DisplaySpec::reference_1080x2400());
        let record = add_window(&mut wm);
        let writer = Arc::new(RecordingWriter::default());
        let mut d = InputDispatcher::new();
        d.register_window(&record, writer.clone());
        let seqs = d.inject_key(4).unwrap();
        assert_eq!(seqs.len(), 2);
        assert!(matches!(
            d.finish_event(999),
            Err(InputError::UnknownSequence(999))
        ));
        d.finish_event(seqs[0]).unwrap();
        assert!(matches!(
            d.finish_event(seqs[0]),
            Err(InputError::UnknownSequence(_))
        ));
    }

    #[test]
    fn a_failing_writer_is_a_structured_error_and_leaves_no_pending() {
        let mut wm = acos_wm::wms::WindowManager::new(DisplaySpec::reference_1080x2400());
        let record = add_window(&mut wm);
        let mut d = InputDispatcher::new();
        d.register_window(&record, Arc::new(FailingWriter));
        let err = d.inject_tap(540.0, 427.0).unwrap_err();
        assert!(matches!(err, InputError::Write { .. }));
        assert!(d.is_idle());
        assert_eq!(d.next_seq, 1, "a failed write must not consume a sequence");
    }

    #[test]
    fn injection_is_deterministic() {
        let mut wm = acos_wm::wms::WindowManager::new(DisplaySpec::reference_1080x2400());
        let record = add_window(&mut wm);
        let run = || {
            let writer = Arc::new(RecordingWriter::default());
            let mut d = InputDispatcher::new();
            d.register_window(&record, writer.clone());
            d.inject_swipe((100.0, 200.0), (300.0, 400.0), 100_000_000, 3)
                .unwrap();
            let frames = writer.frames.lock().unwrap().clone();
            frames
        };
        assert_eq!(run(), run());
    }
}
