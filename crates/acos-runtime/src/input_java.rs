//! The runtime bridge for the T-36 service-side input dispatcher.
//!
//! `acos-services::input` owns the routing/sequencing/ack state machine, but it
//! is deliberately VM-free: it hands each [`DispatchFrame`] to a
//! [`FrameWriter`]. This module is the only place that knows both the service
//! dispatch model and the VM's on-wire input format:
//!
//! * [`ChannelFrameWriter`] encodes a motion/key event as a
//!   [`fwk_input::InputFrame`] and sends it on the **dispatcher end** of the
//!   window's real `AF_UNIX`/`SOCK_SEQPACKET` channel. It is the exact record
//!   `InputChannel.nativeReadFromParcel` / `InputEventReceiver` reads, so the
//!   *real* framework `dispatchInputEvent` runs in the app.
//! * [`RuntimeInputHost`] implements the native ack hook
//!   (`InputEventReceiver.nativeFinishInputEvent` -> [`fwk_input::InputHost`])
//!   and routes it back to [`InputDispatcher::finish_event`], so every delivered
//!   event is acknowledged exactly once.
//!
//! WMS installs a writer for each window right after `addToDisplayAsUser`
//! succeeds ([`install_window_writer`]); the Java helper then only has to read
//! the `InputChannel` from the reply and install its `InputEventReceiver`.
//!
//! [`DispatchFrame`]: acos_services::input::DispatchFrame
//! [`FrameWriter`]: acos_services::input::FrameWriter
//! [`InputDispatcher::finish_event`]: acos_services::input::InputDispatcher::finish_event

use std::sync::Arc;

use acos_services::input::{DispatchFrame, FrameWriter, InputError};
use acos_vm::natives::fwk_input::{
    FramePointer, InputFrame, InputHost, KIND_KEY, KIND_MOTION,
};
use acos_wm::input::InputEvent;
use acos_wm::transport::ChannelEnd;

use crate::wms_java::with_wms;

/// Writes `InputFrame` records to the dispatcher end of a window channel.
pub struct ChannelFrameWriter {
    end: Arc<dyn ChannelEnd>,
}

impl std::fmt::Debug for ChannelFrameWriter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChannelFrameWriter").finish()
    }
}

impl ChannelFrameWriter {
    /// Wrap a `acos-wm` channel end.
    #[must_use]
    pub fn new(end: Arc<dyn ChannelEnd>) -> Self {
        Self { end }
    }
}

/// Translate one service dispatch frame into the VM's `InputFrame` record.
///
/// Returns `None` for an event kind the `InputEventReceiver` protocol has no
/// frame for (focus is delivered through `IWindow`, not the input channel).
#[must_use]
pub fn to_input_frame(frame: &DispatchFrame) -> Option<InputFrame> {
    match &frame.event {
        InputEvent::Motion(m) => Some(InputFrame {
            kind: KIND_MOTION,
            seq: frame.seq,
            action: m.action,
            flags: m.flags,
            edge_flags: m.edge_flags,
            source: m.source,
            device_id: m.device_id,
            display_id: m.display_id,
            down_time: m.down_time,
            event_time: m.event_time,
            x_offset: m.x_offset,
            y_offset: m.y_offset,
            x_precision: m.x_precision,
            y_precision: m.y_precision,
            pointers: m
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
        }),
        InputEvent::Key(k) => Some(InputFrame {
            kind: KIND_KEY,
            seq: frame.seq,
            action: k.action,
            flags: k.flags,
            source: k.source,
            device_id: k.device_id,
            display_id: k.display_id,
            meta_state: k.meta_state,
            key_code: k.key_code,
            scan_code: k.scan_code,
            repeat: 0,
            unicode_char: k.unicode_char,
            down_time: k.down_time,
            event_time: k.event_time,
            ..InputFrame::default()
        }),
        InputEvent::Focus(_) => None,
    }
}

impl FrameWriter for ChannelFrameWriter {
    fn write_frame(&self, frame: &DispatchFrame) -> Result<(), String> {
        let Some(wire) = to_input_frame(frame) else {
            return Err("focus events are not carried by an InputChannel".into());
        };
        self.end
            .send(&wire.encode())
            .map_err(|e| format!("channel send: {e}"))
    }
}

/// The native ack hook: every `nativeFinishInputEvent` becomes exactly one
/// `finish_event` on the service dispatcher.
pub struct RuntimeInputHost;

impl std::fmt::Debug for RuntimeInputHost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RuntimeInputHost")
    }
}

impl InputHost for RuntimeInputHost {
    fn finish_event(&self, seq: i32, handled: bool) {
        let _ = handled;
        let _ = with_wms(|s| s.input.finish_event(seq as u32));
    }

    fn send_frame(&self, _frame: &InputFrame) {
        // App -> system events (IME/back injection) are not on the T-36 path.
    }
}

/// Install the native ack host. Idempotent enough: it replaces any previous
/// host with the same behaviour.
pub fn install() {
    acos_vm::natives::fwk_input::install_host(Box::new(RuntimeInputHost));
}

/// Register the frame writer for `window` after the WMS created its channel.
///
/// Called by `wms_java` immediately after a successful `addToDisplayAsUser`.
pub fn install_window_writer(window: u64) -> Result<(), String> {
    with_wms(|s| {
        let record = s.wm.windows.get(&window).cloned()?;
        let end = s
            .wm
            .channels
            .get(record.input_channel_handle)
            .map(|c| c.dispatcher.clone())?;
        s.input
            .register_window(&record, Arc::new(ChannelFrameWriter::new(end)));
        Some(())
    })
    .ok_or_else(|| format!("input_java: no channel for window {window}"))
}

/// Remove the window from the service dispatcher on `remove`.
pub fn remove_window(window: u64) {
    with_wms(|s| s.input.remove_window(window));
}

/// Inject a tap through the service dispatcher.
pub fn inject_tap(x: f32, y: f32) -> Result<Vec<u32>, InputError> {
    with_wms(|s| s.input.inject_tap(x, y))
}

/// Inject the DOWN half of a tap (see `InputDispatcher::inject_tap_down`).
pub fn inject_tap_down(x: f32, y: f32) -> Result<Vec<u32>, InputError> {
    with_wms(|s| s.input.inject_tap_down(x, y))
}

/// Inject the UP half of a tap started by [`inject_tap_down`].
pub fn inject_tap_up(x: f32, y: f32) -> Result<Vec<u32>, InputError> {
    with_wms(|s| s.input.inject_tap_up(x, y))
}

/// Inject a swipe through the service dispatcher.
pub fn inject_swipe(
    from: (f32, f32),
    to: (f32, f32),
    duration_nanos: i64,
    steps: usize,
) -> Result<Vec<u32>, InputError> {
    with_wms(|s| s.input.inject_swipe(from, to, duration_nanos, steps))
}

/// Inject a key press/release through the service dispatcher.
pub fn inject_key(key_code: i32) -> Result<Vec<u32>, InputError> {
    with_wms(|s| s.input.inject_key(key_code))
}

/// Number of outstanding (unacknowledged) events (test helper).
#[must_use]
pub fn pending_count() -> usize {
    with_wms(|s| s.input.pending().len())
}
