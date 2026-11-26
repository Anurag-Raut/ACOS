//! Wiring I1's real `InputChannel` producer to the live window session (I2).
//!
//! W2 made `addToDisplayAsUser`'s `out InputChannel` byte-exact, W4 registered
//! the image's real `InputEventReceiver` native, and I1 built the producer
//! (`InputPublisher` + `InputChannelRegistry`) — but the fd written into the
//! reply was `acos-wm`'s private channel, so nothing could publish into the
//! socket the guest had installed its receiver on.
//!
//! This module is the one authoritative hand-off:
//!
//! * on `addToDisplayAsUser`, the session registers **one** I1 pair keyed by the
//!   window token (`IWindow` binder), writes that pair's client fd into the
//!   reply, publishes the real `FOCUS(true)` / `TOUCH_MODE(true)` messages the
//!   InputDispatcher would send for a newly focused window, and then forgets
//!   our copy of the client end (so the guest leaving is observable);
//! * `relayout` signals the injector thread (the first traversal is the first
//!   point where the view tree has real bounds);
//! * `inject_tap(x,y)` / `inject_key(code)` publish image-exact `InputMessage`s
//!   on that fd and surface the `FINISHED`/`TIMELINE` replies as logged events;
//! * `remove` drops the channel, closing the guest's end.
//!
//! # Why focus is a channel message, not an `IWindow` callback
//!
//! API-34's `IWindow` has no `windowFocusChanged` transaction (the DEX-derived
//! bytecode has no such method; code 17 is the VM replay model's extension).
//! The real path is `InputMessage` type `FOCUS`, which
//! `InputEventReceiver.onFocusEvent` turns into
//! `ViewRootImpl.windowFocusChanged(hasFocus)`. Without it
//! `mAttachInfo.mHasWindowFocus` stays false and
//! `ViewRootImpl.shouldDropInputEvent` drops every non-pointer event with
//! `"no window focus"`. The image's `publishFocusEvent` (libinput.so
//! `0x79910`) writes `{type=3, seq}` then `eventId` at `0x08` and `hasFocus`
//! at `0x0c`, size `0x10` (see `docs/IMAGE_ABI_NOTEBOOK.md`).
//!
//! # What is still policy, not this module
//!
//! Which window an event goes to is the session's `acos-wm` state; this module
//! only publishes to the token it is told. Test/CI injection is driven by
//! environment variables (`ACOS_INPUT_TAP`, `ACOS_INPUT_KEY`, ...), parsed once
//! in [`InputHost::from_env`]; library users call [`InputDispatch::inject_tap`]
//! / [`InputDispatch::inject_key`] directly.

use std::collections::BTreeSet;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use acos_parcel::{BinderId, Parcel};

use crate::input_channel::{
    monotonic_nanos, Ack, ChannelError, ConsumerResponse, Gesture, InputChannelRegistry,
};

type Result<T> = std::result::Result<T, ChannelError>;

/// One published or acknowledged event, kept for logs and tests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputEvent {
    /// A sequence was published on `token`.
    Published {
        /// Window token.
        token: BinderId,
        /// Sequence number.
        seq: u32,
        /// `motion`, `motion`, `key`, `focus`, `touch_mode`.
        kind: &'static str,
        /// `DOWN`/`UP`/`MOVE`/`CANCEL` or key scan for motion/key.
        detail: String,
    },
    /// The guest acknowledged a sequence.
    Acked {
        /// Window token.
        token: BinderId,
        /// The `Ack` I1 decoded.
        ack: Ack,
    },
    /// A `TIMELINE` reply.
    Timeline {
        /// Window token.
        token: BinderId,
        /// Sequence the timeline belongs to.
        seq: u32,
    },
    /// A publish/receive failure, surfaced instead of swallowed.
    Error {
        /// Window token.
        token: BinderId,
        /// Structured reason.
        message: String,
    },
    /// The channel was removed (window gone).
    Closed {
        /// Window token.
        token: BinderId,
    },
}

/// What to inject once the first `relayout` arrives.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Injection {
    Tap { x: f32, y: f32 },
    Key { code: i32 },
}

/// The live producer state for every window on this host.
#[derive(Debug, Default)]
pub struct InputDispatch {
    registry: InputChannelRegistry,
    tokens: BTreeSet<u64>,
    /// The window the dispatcher last sent `FOCUS(true)` to. Only a change
    /// publishes, so a repeated `set_focus` is free.
    focused: Option<u64>,
    relayouts: usize,
    events: Vec<InputEvent>,
    plan: Option<Injection>,
    inject_delay: Duration,
    ack_wait: Duration,
    /// Monotonic count of windows whose input channel went away.
    ///
    /// A gesture records this before it publishes and asks afterwards whether
    /// **its own** token was among the removals: dismissing a dialog window
    /// destroys the receiver that owes the `FINISHED` ack, so the guest never
    /// answers and the only honest evidence left is the host's own fact that the
    /// window it delivered to is gone (REL3I3). A removal of a *different*
    /// window never counts, which is what the token check gives.
    removals: u64,
    /// `(removals value, token)` for every removal, oldest first.
    removed: Vec<(u64, BinderId)>,
}

impl InputDispatch {
    /// An empty dispatcher.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a pair for `token`, publish `TOUCH_MODE(true)`, and keep the
    /// server end. The client end is written into the add reply by
    /// [`InputDispatch::write_and_forget`].
    ///
    /// LAYER1: `FOCUS` is **not** published here any more. The window that gets
    /// focus is the WMS layer stack's `focus_target()` (topmost focusable
    /// window), so the session calls [`InputDispatch::set_focus`] after the add
    /// and after every remove/relayout. Publishing `FOCUS(true)` to whatever
    /// window happened to be registered made a `FLAG_NOT_FOCUSABLE` overlay
    /// steal the activity's focus.
    pub fn register_window(&mut self, token: BinderId, name: &str) -> Result<()> {
        self.registry.register(token, name)?;
        self.tokens.insert(token.0);
        let Some(publisher) = self.registry.publisher_mut(token) else {
            return Err(ChannelError::Malformed("unknown input token"));
        };
        let touch_mode = publisher.touch_mode(true)?;
        self.events.push(InputEvent::Published {
            token,
            seq: touch_mode,
            kind: "touch_mode",
            detail: "inTouchMode=true".into(),
        });
        Ok(())
    }

    /// Publish the policy focus state: `FOCUS(false)` to the window that loses
    /// focus, `FOCUS(true)` to the one that gains it. No message when the
    /// target did not change. `ACOS_INPUT_NO_FOCUS=1` suppresses the messages
    /// for diagnostics only (the recorded focus state still moves).
    pub fn set_focus(&mut self, focus: Option<BinderId>) -> Result<()> {
        let next = focus.map(|t| t.0);
        if self.focused == next {
            return Ok(());
        }
        let previous = self.focused;
        self.focused = next;
        if std::env::var_os("ACOS_INPUT_NO_FOCUS").is_some() {
            return Ok(());
        }
        if let Some(old) = previous {
            let token = BinderId(old);
            if self.tokens.contains(&old) {
                self.publish_focus(token, false)?;
            }
        }
        if let Some(new) = next {
            self.publish_focus(BinderId(new), true)?;
        }
        Ok(())
    }

    /// The window the dispatcher currently believes has focus.
    #[must_use]
    pub fn focused(&self) -> Option<BinderId> {
        self.focused.map(BinderId)
    }

    /// Publish the real `FOCUS(hasFocus)` message. This is what
    /// `ViewRootImpl`'s `WindowInputEventReceiver.onFocusEvent` consumes; key
    /// events are dropped without it (`"no window focus"`).
    pub fn publish_focus(&mut self, token: BinderId, has_focus: bool) -> Result<u32> {
        let Some(publisher) = self.registry.publisher_mut(token) else {
            return Err(ChannelError::Malformed("unknown input token"));
        };
        let seq = publisher.focus(has_focus)?;
        self.events.push(InputEvent::Published {
            token,
            seq,
            kind: "focus",
            detail: format!("hasFocus={has_focus}"),
        });
        Ok(seq)
    }

    /// Write the W2 `out InputChannel` body from the registry, then drop this
    /// process's client copy (I1 §6 step 2). The descriptor written into the
    /// parcel is the **only** client end the guest receives.
    pub fn write_and_forget(&mut self, token: BinderId, reply: &mut Parcel) -> Result<()> {
        self.registry.write_to_parcel(token, reply)?;
        self.registry.forget_client_end(token);
        Ok(())
    }

    /// A duplicate of the client end (tests and a future control surface). The
    /// session itself forgets its copy after writing the reply, so this only
    /// returns a descriptor while one is still held here.
    #[must_use]
    pub fn duplicate_client_fd(&self, token: BinderId) -> Option<std::os::fd::OwnedFd> {
        self.registry.duplicate_client_fd(token)
    }

    /// `true` when `token` has a live channel.
    #[must_use]
    pub fn is_registered(&self, token: BinderId) -> bool {
        self.tokens.contains(&token.0)
    }

    /// The number of `relayout` calls observed.
    #[must_use]
    pub fn relayouts(&self) -> usize {
        self.relayouts
    }

    /// Every published/acknowledged event so far.
    #[must_use]
    pub fn events(&self) -> &[InputEvent] {
        &self.events
    }

    /// Record a relayout (the injector thread uses this as "the view tree now
    /// has bounds"). Returns whether this was the first one.
    pub fn note_relayout(&mut self, token: BinderId) -> bool {
        if !self.is_registered(token) {
            return false;
        }
        self.relayouts += 1;
        self.relayouts == 1
    }

    /// Publish a tap on `token`'s channel.
    pub fn inject_tap(&mut self, token: BinderId, x: f32, y: f32) -> Result<Gesture> {
        self.publish_tap(token, x, y)
    }

    /// Publish a real swipe/scroll gesture on `token`'s channel: `DOWN` at
    /// `from`, one `MOVE` per display refresh interval, `UP` at `to`.
    ///
    /// The events are **paced** at the refresh cadence (a sleep until each
    /// sample's scheduled time), so the guest sees the same arrival pattern a
    /// touchscreen produces instead of a burst that arrives in one looper wake
    /// up. `eventTime` is `downTime` plus the sample's offset, so the
    /// timestamps a `VelocityTracker` reads are evenly spaced and end at the
    /// lift — the list's own fling physics then runs on real numbers.
    ///
    /// Coordinates are carried in the message transform, exactly like
    /// [`Self::inject_tap`] (the image's `MotionEvent` path reads them there).
    /// `duration` shorter than one refresh interval still publishes one
    /// interpolated `MOVE`; nothing about the gesture is invented beyond the
    /// interpolation between the two points the caller named.
    pub fn inject_swipe(
        &mut self,
        token: BinderId,
        from: (f32, f32),
        to: (f32, f32),
        duration: Duration,
    ) -> Result<Gesture> {
        self.publish_swipe(token, from, to, duration)
    }

    fn publish_swipe(
        &mut self,
        token: BinderId,
        from: (f32, f32),
        to: (f32, f32),
        duration: Duration,
    ) -> Result<Gesture> {
        let samples = crate::input_channel::swipe_samples(
            from,
            to,
            i64::try_from(duration.as_nanos()).unwrap_or(i64::MAX / 2),
            crate::input_channel::GESTURE_SAMPLE_INTERVAL_NANOS,
        );
        let Some(publisher) = self.registry.publisher_mut(token) else {
            let message = format!("inject_swipe: no input channel for token {:#x}", token.0);
            self.events.push(InputEvent::Error {
                token,
                message: message.clone(),
            });
            return Err(ChannelError::Io(message));
        };
        let down_time = monotonic_nanos();
        let started = Instant::now();
        let last = samples.len().saturating_sub(1);
        let mut dispatches = Vec::with_capacity(samples.len());
        for (index, sample) in samples.iter().enumerate() {
            // Pace the publication to the sample's own event time. The `DOWN`
            // has offset 0 and goes immediately.
            let scheduled = started + Duration::from_nanos(u64::try_from(sample.offset_nanos).unwrap_or(0));
            let now = Instant::now();
            if scheduled > now {
                thread::sleep(scheduled - now);
            }
            let action = if index == 0 {
                crate::input_channel::ACTION_DOWN
            } else if index == last {
                crate::input_channel::ACTION_UP
            } else {
                crate::input_channel::ACTION_MOVE
            };
            let event_id = publisher.allocate_event_id();
            let mut event = crate::input_channel::MotionEvent::touch(
                event_id,
                action,
                down_time,
                down_time + sample.offset_nanos,
                0.0,
                0.0,
            );
            event.transform.tx = sample.x;
            event.transform.ty = sample.y;
            event.raw_transform.tx = sample.x;
            event.raw_transform.ty = sample.y;
            match publisher.publish_motion(&event) {
                Ok(dispatch) => {
                    self.events.push(InputEvent::Published {
                        token,
                        seq: dispatch.seq,
                        kind: "motion",
                        detail: format!(
                            "{:?} x={} y={} t={}ms",
                            dispatch.kind,
                            sample.x.round() as i64,
                            sample.y.round() as i64,
                            sample.offset_nanos / 1_000_000
                        ),
                    });
                    dispatches.push(dispatch);
                }
                Err(e) => {
                    self.events.push(InputEvent::Error {
                        token,
                        message: format!("inject_swipe: {e}"),
                    });
                    return Err(e);
                }
            }
        }
        Ok(crate::input_channel::Gesture { dispatches })
    }

    fn publish_tap(&mut self, token: BinderId, x: f32, y: f32) -> Result<Gesture> {
        let Some(publisher) = self.registry.publisher_mut(token) else {
            let message = format!("inject_tap: no input channel for token {:#x}", token.0);
            self.events.push(InputEvent::Error {
                token,
                message: message.clone(),
            });
            return Err(ChannelError::Io(message));
        };
        // Coordinates are carried in the message's window transform, not the
        // pointer axes. Measured (I2): a `PointerCoords` with bits=0x3f and
        // x=176/y=303 reaches `View.dispatchTouchEvent` as (0,0) through this
        // image's `MotionEvent` path, while the same point in
        // `MotionEvent.transform` reaches it as raw (176,303) and the correct
        // view-local point. The transform is a real `InputMessage` field
        // (`InputPublisher::publishMotionEvent` writes dsdx/dtdx/dtdy/dsdy/tx/ty
        // at 0x68), so this is still a framework-layout event, not a shortcut.
        let now = monotonic_nanos();
        let down_event_id = publisher.allocate_event_id();
        let mut down = crate::input_channel::MotionEvent::touch(
            down_event_id,
            crate::input_channel::ACTION_DOWN,
            now,
            now,
            0.0,
            0.0,
        );
        down.transform.tx = x;
        down.transform.ty = y;
        down.raw_transform.tx = x;
        down.raw_transform.ty = y;
        let down_dispatch = match publisher.publish_motion(&down) {
            Ok(d) => d,
            Err(e) => {
                self.events.push(InputEvent::Error {
                    token,
                    message: format!("inject_tap: {e}"),
                });
                return Err(e);
            }
        };
        let mut up = down.clone();
        up.event_id = publisher.allocate_event_id();
        up.action = crate::input_channel::ACTION_UP;
        up.event_time = now + crate::input_channel::DEFAULT_TAP_HOLD_NANOS;
        let up_dispatch = match publisher.publish_motion(&up) {
            Ok(d) => d,
            Err(e) => {
                self.events.push(InputEvent::Error {
                    token,
                    message: format!("inject_tap: {e}"),
                });
                return Err(e);
            }
        };
        let gesture = crate::input_channel::Gesture {
            dispatches: vec![down_dispatch, up_dispatch],
        };
        for d in &gesture.dispatches {
            self.events.push(InputEvent::Published {
                token,
                seq: d.seq,
                kind: "motion",
                detail: format!("{:?} x={x} y={y}", d.kind),
            });
        }
        Ok(gesture)
    }

    /// Publish one `MotionEvent` series (a voice/accessibility gesture) on
    /// `token`'s channel, each event with its own time.
    ///
    /// This is the `dispatchGesture` path: the events are the guest's own
    /// (`GestureDescription$MotionEventGenerator` sampled them on the device
    /// clock), so the host publishes them verbatim instead of sleeping between
    /// strokes. Every event is published before the first one can be acked, so
    /// a failure half-way is still reported as a failure of the whole series.
    pub fn inject_motion_series(
        &mut self,
        token: BinderId,
        events: &[crate::input_channel::MotionEvent],
    ) -> Result<Gesture> {
        if events.is_empty() {
            return Err(ChannelError::Malformed("empty gesture"));
        }
        let Some(_) = self.registry.publisher_mut(token) else {
            let message = format!(
                "inject_motion_series: no input channel for token {:#x}",
                token.0
            );
            self.events.push(InputEvent::Error {
                token,
                message: message.clone(),
            });
            return Err(ChannelError::Io(message));
        };
        let mut dispatches = Vec::with_capacity(events.len());
        for event in events {
            let Some(publisher) = self.registry.publisher_mut(token) else {
                return Err(ChannelError::Io("input channel vanished".into()));
            };
            // A stroke is many events: each one carries its own `eventId`, like
            // the dispatcher's own allocation (`inject_tap` does the same), so
            // the guest's `InputEventReceiver` never sees two `FINISHED` acks
            // for one id.
            let mut event = event.clone();
            event.event_id = publisher.allocate_event_id();
            match publisher.publish_motion(&event) {
                Ok(dispatch) => {
                    self.events.push(InputEvent::Published {
                        token,
                        seq: dispatch.seq,
                        kind: "motion",
                        detail: format!(
                            "{:?} action={} t={}",
                            dispatch.kind, event.action, event.event_time
                        ),
                    });
                    dispatches.push(dispatch);
                }
                Err(error) => {
                    self.events.push(InputEvent::Error {
                        token,
                        message: format!("inject_motion_series: {error}"),
                    });
                    return Err(error);
                }
            }
        }
        Ok(Gesture { dispatches })
    }

    /// Publish a single motion frame (single- or multi-pointer) on `token`'s
    /// channel. `pointers` are `(id, window_local_x, window_local_y)`.
    pub fn inject_motion_frame(
        &mut self,
        token: BinderId,
        action: i32,
        down_time: i64,
        event_time: i64,
        pointers: &[(i32, f32, f32)],
    ) -> Result<Gesture> {
        if pointers.is_empty() || pointers.len() > crate::input_channel::MAX_POINTERS {
            return Err(ChannelError::BadPointerCount(pointers.len()));
        }
        let Some(publisher) = self.registry.publisher_mut(token) else {
            let message = format!(
                "inject_motion_frame: no input channel for token {:#x}",
                token.0
            );
            self.events.push(InputEvent::Error {
                token,
                message: message.clone(),
            });
            return Err(ChannelError::Io(message));
        };
        let event_id = publisher.allocate_event_id();
        let (lx0, ly0) = (pointers[0].1, pointers[0].2);
        let mut event = crate::input_channel::MotionEvent::touch(
            event_id, action, down_time, event_time, 0.0, 0.0,
        );
        event.transform.tx = lx0;
        event.transform.ty = ly0;
        event.raw_transform.tx = lx0;
        event.raw_transform.ty = ly0;
        event.pointers = pointers
            .iter()
            .enumerate()
            .map(|(idx, &(id, lx, ly))| crate::input_channel::Pointer {
                properties: crate::input_channel::PointerProperties { id, tool_type: 1 },
                coords: if idx == 0 {
                    crate::input_channel::PointerCoords::touch(0.0, 0.0)
                } else {
                    crate::input_channel::PointerCoords::touch(lx - lx0, ly - ly0)
                },
            })
            .collect();
        match publisher.publish_motion(&event) {
            Ok(dispatch) => {
                self.events.push(InputEvent::Published {
                    token,
                    seq: dispatch.seq,
                    kind: "motion",
                    detail: format!(
                        "{:?} action={action} pointers={} x={} y={}",
                        dispatch.kind,
                        pointers.len(),
                        lx0.round() as i64,
                        ly0.round() as i64
                    ),
                });
                Ok(Gesture {
                    dispatches: vec![dispatch],
                })
            }
            Err(e) => {
                self.events.push(InputEvent::Error {
                    token,
                    message: format!("inject_motion_frame: {e}"),
                });
                Err(e)
            }
        }
    }

    /// Publish a single `KeyEvent` (`KEY_ACTION_DOWN` or `KEY_ACTION_UP`) with
    /// explicit `meta_state` and `device_id`.
    pub fn inject_key_event(
        &mut self,
        token: BinderId,
        action: i32,
        key_code: i32,
        meta_state: i32,
        device_id: i32,
        down_time: i64,
        event_time: i64,
    ) -> Result<Gesture> {
        let Some(publisher) = self.registry.publisher_mut(token) else {
            let message = format!(
                "inject_key_event: no input channel for token {:#x}",
                token.0
            );
            self.events.push(InputEvent::Error {
                token,
                message: message.clone(),
            });
            return Err(ChannelError::Io(message));
        };
        let mut ev = crate::input_channel::KeyEvent::new(
            publisher.allocate_event_id(),
            action,
            key_code,
            down_time,
            event_time,
        );
        ev.meta_state = meta_state;
        ev.device_id = device_id;
        match publisher.publish_key(&ev) {
            Ok(dispatch) => {
                self.events.push(InputEvent::Published {
                    token,
                    seq: dispatch.seq,
                    kind: "key",
                    detail: format!("{:?} keyCode={key_code} meta={meta_state}", dispatch.kind),
                });
                Ok(Gesture {
                    dispatches: vec![dispatch],
                })
            }
            Err(e) => {
                self.events.push(InputEvent::Error {
                    token,
                    message: format!("inject_key_event token={:#x}: {e}", token.0),
                });
                Err(e)
            }
        }
    }

    /// Publish a key press on `token`'s channel.
    pub fn inject_key(&mut self, token: BinderId, key_code: i32) -> Result<Gesture> {
        self.publish_key(token, key_code)
    }

    /// Publish on the newest live window, skipping channels whose guest end is
    /// gone.
    ///
    /// The app re-adds a window (`IWindow` handle churn) and a dismissed
    /// dialog's channel entry can outlive its `InputEventReceiver`; the
    /// dispatcher must not deliver into a dead channel, and it must not treat
    /// one dead entry as "input is broken". AOSP's dispatcher holds the live
    /// focus connection, so asking the next token is the same policy with
    /// ACOS's token-set bookkeeping.
    pub fn publish_on_live(
        &mut self,
        mut publish: impl FnMut(&mut InputDispatch, BinderId) -> Result<Gesture>,
    ) -> Result<(BinderId, Gesture)> {
        let mut last: Option<ChannelError> = None;
        loop {
            let Some(token) = self.newest_token() else {
                return Err(last.unwrap_or(ChannelError::Malformed(
                    "no live window input channel yet",
                )));
            };
            match publish(self, token) {
                Ok(gesture) => return Ok((token, gesture)),
                Err(ChannelError::PeerClosed) => {
                    let message = format!(
                        "input channel {:#x} peer is closed; forgetting it",
                        token.0
                    );
                    self.events.push(InputEvent::Error { token, message });
                    self.remove(token);
                    last = Some(ChannelError::PeerClosed);
                }
                Err(e) => return Err(e),
            }
        }
    }

    /// Publish on the window that holds **input focus**, falling back to the
    /// newest live one only when no focus is known (a bare host with no layer
    /// stack).
    ///
    /// This is `InputDispatcher`'s own policy: a key goes to
    /// `getFocusedWindowHandle`, never to "the window that registered last".
    /// Measured before this existed: after a `TYPE_ACCESSIBILITY_OVERLAY`
    /// (`FLAG_NOT_FOCUSABLE|FLAG_NOT_TOUCHABLE`) was added, the next `back`
    /// landed on the overlay's `ViewRootImpl`, which logged `VRI[]: Dropping
    /// event (no window focus)` and the app never saw the key.
    pub fn publish_on_focused(
        &mut self,
        mut publish: impl FnMut(&mut InputDispatch, BinderId) -> Result<Gesture>,
    ) -> Result<(BinderId, Gesture)> {
        let focused = self.focused().filter(|token| self.tokens.contains(&token.0));
        if let Some(token) = focused {
            match publish(self, token) {
                Ok(gesture) => return Ok((token, gesture)),
                Err(ChannelError::PeerClosed) => {
                    let message =
                        format!("input channel {:#x} peer is closed; forgetting it", token.0);
                    self.events.push(InputEvent::Error { token, message });
                    self.remove(token);
                }
                Err(error) => return Err(error),
            }
        }
        self.publish_on_live(publish)
    }

    fn publish_key(&mut self, token: BinderId, key_code: i32) -> Result<Gesture> {
        let Some(publisher) = self.registry.publisher_mut(token) else {
            let message = format!(
                "inject_key: no input channel for token {:#x} (live: {:?})",
                token.0,
                self.tokens
                    .iter()
                    .map(|t| format!("{t:#x}"))
                    .collect::<Vec<_>>()
            );
            self.events.push(InputEvent::Error {
                token,
                message: message.clone(),
            });
            return Err(ChannelError::Io(message));
        };
        let gesture = match publisher.key(key_code) {
            Ok(g) => g,
            Err(e) => {
                self.events.push(InputEvent::Error {
                    token,
                    message: format!("inject_key token={:#x}: {e}", token.0),
                });
                return Err(e);
            }
        };
        for d in &gesture.dispatches {
            self.events.push(InputEvent::Published {
                token,
                seq: d.seq,
                kind: "key",
                detail: format!("{:?} keyCode={key_code}", d.kind),
            });
        }
        Ok(gesture)
    }

    /// Publish `text` as one key stroke per character on `token`'s channel.
    ///
    /// Every character is encoded **before** anything is published, so a text
    /// the US key map cannot express fails without a half-typed field.
    pub fn inject_text(&mut self, token: BinderId, text: &str) -> Result<Gesture> {
        let strokes = text_strokes(text)?;
        let Some(publisher) = self.registry.publisher_mut(token) else {
            let message = format!("inject_text: no input channel for token {:#x}", token.0);
            self.events.push(InputEvent::Error {
                token,
                message: message.clone(),
            });
            return Err(ChannelError::Io(message));
        };
        let mut dispatches = Vec::with_capacity(strokes.len() * 2);
        for (key_code, meta_state) in strokes {
            match publisher.key_stroke(key_code, meta_state, VIRTUAL_KEYBOARD_DEVICE_ID) {
                Ok(gesture) => dispatches.extend(gesture.dispatches),
                Err(e) => {
                    self.events.push(InputEvent::Error {
                        token,
                        message: format!("inject_text: {e}"),
                    });
                    return Err(e);
                }
            }
            self.events.push(InputEvent::Published {
                token,
                seq: dispatches[dispatches.len() - 2].seq,
                kind: "key",
                detail: format!("text keyCode={key_code} meta={meta_state}"),
            });
        }
        Ok(Gesture { dispatches })
    }

    /// Read every reply currently queued on `token` and record it. `EAGAIN`
    /// returns an empty vector; a closed peer is recorded as an event and
    /// returned as an error so a lost guest is never a silent success.
    pub fn drain_acks(&mut self, token: BinderId) -> Result<Vec<ConsumerResponse>> {
        let mut out = Vec::new();
        loop {
            let Some(publisher) = self.registry.publisher_mut(token) else {
                return Ok(out);
            };
            match publisher.receive_consumer_response() {
                Ok(Some(response)) => {
                    match &response {
                        ConsumerResponse::Finished(ack) => {
                            self.events.push(InputEvent::Acked { token, ack: *ack })
                        }
                        ConsumerResponse::Timeline(timeline) => {
                            self.events.push(InputEvent::Timeline {
                                token,
                                seq: timeline.seq,
                            });
                        }
                    }
                    out.push(response);
                }
                Ok(None) => break,
                Err(ChannelError::PeerClosed) => {
                    // The guest disposed its `InputEventReceiver` (a removed
                    // window, or an APP that closed the channel). Forget the
                    // token so it can never be picked as the newest target
                    // again; this call still reports the lost channel.
                    self.remove(token);
                    return Err(ChannelError::PeerClosed);
                }
                Err(e) => {
                    self.events.push(InputEvent::Error {
                        token,
                        message: e.to_string(),
                    });
                    return Err(e);
                }
            }
        }
        Ok(out)
    }

    /// Drop `token`'s channel. Dropping the server end makes the guest's next
    /// read see EOF.
    ///
    /// The removal is also recorded as a fact for the gesture that was in
    /// flight: a window that goes away before its receiver finished the event
    /// can never ack it (REL3I3).
    pub fn remove(&mut self, token: BinderId) -> bool {
        let was = self.registry.remove(token);
        self.tokens.remove(&token.0);
        if self.focused == Some(token.0) {
            self.focused = None;
        }
        if was {
            self.events.push(InputEvent::Closed { token });
            self.removals += 1;
            self.removed.push((self.removals, token));
        }
        was
    }

    /// The removals counter to pass to [`Self::was_removed_since`] later.
    #[must_use]
    pub fn removal_mark(&self) -> u64 {
        self.removals
    }

    /// `true` when `token` itself was removed after `mark`.
    #[must_use]
    pub fn was_removed_since(&self, token: BinderId, mark: u64) -> bool {
        self.removed
            .iter()
            .any(|(at, removed)| *at > mark && *removed == token)
    }

    /// The token of the most recently registered live window (env-driven
    /// injection targets the only window a single launch creates).
    fn newest_token(&self) -> Option<BinderId> {
        self.tokens.iter().next_back().map(|t| BinderId(*t))
    }
}

/// One acknowledged input sequence (the `FINISHED` reply the guest sent).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InjectedAck {
    /// The acknowledged sequence.
    pub seq: u32,
    /// The guest's `InputEventReceiver` result (`handled`).
    pub handled: bool,
    /// Publish-to-ack latency in whole microseconds, when tracked.
    pub latency_us: Option<u64>,
}

/// One gesture published from a live session, with the guest's acks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InjectedGesture {
    /// The window token the gesture went to.
    pub token: BinderId,
    /// The sequence numbers published (one per `InputMessage`).
    pub seqs: Vec<u32>,
    /// `FINISHED` replies received within the ack window (may be fewer than
    /// `seqs.len()` when the guest is slow; that is reported, not hidden).
    pub acks: Vec<InjectedAck>,
}

/// Where a touch at a display point lands: the window under the point
/// (`InputDispatcher::findTouchedWindowAt`) and the point in **that window's**
/// space (the `InputChannel` transform's space).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TouchTarget {
    /// The window's input token (its `IWindow` binder id).
    pub token: BinderId,
    /// The point in the window's own coordinates.
    pub x: f32,
    pub y: f32,
    /// The window frame's display origin (diagnostics: the conversion back).
    pub origin: (i32, i32),
}

/// The session-facing handle: shared producer state plus the injector thread.
#[derive(Debug, Clone)]
pub struct InputHost {
    state: Arc<Mutex<InputDispatch>>,
    relayout_tx: Option<Sender<BinderId>>,
    /// The window stack the touch rule reads. `None` on a bare host (unit
    /// tests, a session with no layer stack): the only target there is, is the
    /// newest live window, and [`Self::touch_target`] says so.
    stack: Option<Arc<Mutex<crate::wms::WmsState>>>,
}

impl InputHost {
    /// Construct from `ACOS_INPUT_TAP="x,y"` / `ACOS_INPUT_KEY="<code>"`.
    ///
    /// Optional knobs: `ACOS_INPUT_DELAY_MS` (wait after the first relayout
    /// before publishing, default 200) and `ACOS_INPUT_ACK_MS` (how long to
    /// collect `FINISHED` replies, default 2500). With neither tap nor key set,
    /// no injector thread is started; focus/touch-mode are still published.
    #[must_use]
    pub fn from_env() -> Self {
        let plan = parse_injection();
        let delay = Duration::from_millis(env_u64("ACOS_INPUT_DELAY_MS", 200));
        let ack_wait = Duration::from_millis(env_u64("ACOS_INPUT_ACK_MS", 2500));
        let mut dispatch = InputDispatch::new();
        dispatch.plan = plan;
        dispatch.inject_delay = delay;
        dispatch.ack_wait = ack_wait;
        let state = Arc::new(Mutex::new(dispatch));
        let relayout_tx = plan.map(|plan| {
            let (tx, rx) = mpsc::channel();
            let state = Arc::clone(&state);
            thread::Builder::new()
                .name("acos-input-injector".into())
                .spawn(move || injector_loop(state, rx, plan, delay, ack_wait))
                .ok();
            tx
        });
        Self {
            state,
            relayout_tx,
            stack: None,
        }
    }

    /// An inert host (no channel, no thread); the session keeps its W2 path.
    #[must_use]
    pub fn disabled() -> Self {
        Self {
            state: Arc::new(Mutex::new(InputDispatch::new())),
            relayout_tx: None,
            stack: None,
        }
    }

    /// Give this host the session's window stack: the **one** owner of "which
    /// window is under this point".
    ///
    /// `InputDispatcher` routes a touch by the layer stack
    /// (`findTouchedWindowAt`: topmost touchable window containing the point)
    /// and a key by focus (`getFocusedWindowHandle`). `acos-wm` already models
    /// both ([`acos_wm::layers::WindowLayerStack::hit_test_candidates`] /
    /// `focus_target`); attaching the stack here is what makes the same rule
    /// hold for every input path (scenario steps, the session API, a bound
    /// accessibility service) instead of per-call-site guesses.
    #[must_use]
    pub fn with_window_stack(mut self, stack: Arc<Mutex<crate::wms::WmsState>>) -> Self {
        self.stack = Some(stack);
        self
    }

    /// `true` when a layer stack is attached (the touch rule is by point).
    #[must_use]
    pub fn has_window_stack(&self) -> bool {
        self.stack.is_some()
    }

    /// Where a touch at display point `(x, y)` lands.
    ///
    /// The topmost **touchable** window containing the point whose input
    /// channel is live (`InputDispatcher`'s search order), with the point
    /// converted into that window's space. `None` when no window is under the
    /// point — a structured refusal, never "the newest window".
    ///
    /// This is the same rule the VM transport's dispatcher already implements
    /// ([`crate::input::InputDispatcher::hit_test`] /
    /// [`crate::input::InputDispatcher::half_inject_tap`]-style point routing),
    /// and the same one `acos-wm` models for composition. There is exactly one
    /// rule: touch by point, keys by focus.
    #[must_use]
    pub fn touch_target(&self, x: f32, y: f32) -> Option<TouchTarget> {
        let stack = self.stack.as_ref()?;
        let point = acos_wm::geom::PointI::new(x as i32, y as i32);
        // The stack lock is released before the input state is taken: the two
        // never nest (the window session delivers input while holding its own
        // lock).
        let candidates = {
            let wms = stack
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            wms.wm
                .hit_test_candidates(point)
                .into_iter()
                .map(|window| {
                    let origin = wms
                        .wm
                        .windows
                        .get(&window)
                        .map(|record| (record.frame.left, record.frame.top))
                        .unwrap_or((0, 0));
                    (window, origin)
                })
                .collect::<Vec<_>>()
        };
        for (window, origin) in candidates {
            let token = BinderId(window);
            if !self.is_live(token) {
                continue;
            }
            return Some(TouchTarget {
                token,
                x: x - origin.0 as f32,
                y: y - origin.1 as f32,
                origin,
            });
        }
        None
    }

    /// The live window tokens (`{:#x}` strings), for diagnostics.
    #[must_use]
    pub fn live_tokens(&self) -> Vec<String> {
        let state = lock(&self.state);
        state.tokens.iter().map(|t| format!("{t:#x}")).collect()
    }

    /// The removals counter, taken before an action so
    /// [`Self::window_removed_since`] can answer about that action only.
    #[must_use]
    pub fn removal_mark(&self) -> u64 {
        lock(&self.state).removal_mark()
    }

    /// `true` when `token`'s own window was removed after `mark`.
    ///
    /// REL3I3: the host's fact that the window it delivered input to no longer
    /// exists. A dialog's Back destroys the dialog window before its
    /// `InputEventReceiver` can finish the event (`Attempted to finish an input
    /// event but the input event receiver has already been disposed.`), so no
    /// guest ack will ever exist; the removal is the evidence.
    #[must_use]
    pub fn window_removed_since(&self, token: BinderId, mark: u64) -> bool {
        lock(&self.state).was_removed_since(token, mark)
    }

    /// Shared producer state.
    #[must_use]
    pub fn state(&self) -> &Arc<Mutex<InputDispatch>> {
        &self.state
    }

    /// Signal that a window's first traversal called `relayout`. Only the
    /// first one is forwarded, so one launch injects exactly one gesture.
    pub fn note_relayout(&self, token: BinderId) {
        let first = {
            let mut d = lock(&self.state);
            d.note_relayout(token)
        };
        if first {
            if let Some(tx) = &self.relayout_tx {
                let _ = tx.send(token);
            }
        }
    }

    /// `true` when `token` still has a live input channel.
    #[must_use]
    pub fn is_live(&self, token: BinderId) -> bool {
        lock(&self.state).is_registered(token)
    }

    /// `true` when an environment-driven injection plan is configured.
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.relayout_tx.is_some()
    }

    /// The window token a live injection targets: the most recently registered
    /// window (a single-app session has exactly one).
    #[must_use]
    pub fn newest_token(&self) -> Option<BinderId> {
        default_token(&lock(&self.state))
    }

    /// The token of the window input focus is on.
    ///
    /// Keys go to the **focused** window (`InputDispatcher::dispatchKeyLocked`
    /// only dispatches a key to `getFocusedWindowHandle`), not to the most
    /// recently added one: a `FLAG_NOT_FOCUSABLE` accessibility overlay added
    /// after the activity must not receive Back. `None` when no window holds
    /// focus, which for a key is a refusal, not a guess.
    #[must_use]
    pub fn focused_token(&self) -> Option<BinderId> {
        lock(&self.state).focused()
    }

    /// Publish one tap at display point `(x, y)` and collect the guest's
    /// `FINISHED` replies for a bounded window.
    ///
    /// The target is the **window under the point** (`InputDispatcher`'s
    /// `findTouchedWindowAt`), and the published point is that window's own
    /// space — never "the newest registered window" and never "the focused
    /// one": an overlay above the activity takes a touch that lands on it, and
    /// an activity takes one that lands on it even while another window holds
    /// focus. A point with no touchable window under it is a structured
    /// refusal (`ChannelError::Malformed`), because falling back to another
    /// window would deliver a tap the user did not make.
    ///
    /// A host with no attached layer stack (unit tests, a bare host) has one
    /// window and uses it; [`Self::has_window_stack`] reports which rule
    /// applied.
    pub fn tap(&self, x: f32, y: f32, ack_wait: Duration) -> Result<InjectedGesture> {
        if self.stack.is_some() {
            let Some(target) = self.touch_target(x, y) else {
                return Err(no_touch_target(x, y));
            };
            return self.tap_window(target.token, target.x, target.y, ack_wait);
        }
        let (token, gesture) = lock(&self.state)
            .publish_on_live(|dispatch, token| dispatch.inject_tap(token, x, y))?;
        self.collect(&token, &gesture, ack_wait)
    }

    /// Publish one tap on an explicit window token. The point is **window
    /// local** (the `InputChannel` transform's space); the caller chooses the
    /// token from the WMS layer stack's hit test (LAYER1), not from registration
    /// order.
    pub fn tap_window(
        &self,
        token: BinderId,
        x: f32,
        y: f32,
        ack_wait: Duration,
    ) -> Result<InjectedGesture> {
        let gesture = lock(&self.state).inject_tap(token, x, y)?;
        self.collect(&token, &gesture, ack_wait)
    }

    /// Publish one swipe/scroll and collect the guest's `FINISHED` replies for
    /// a bounded window.
    ///
    /// A swipe is a **touch stream**, so its target is the window under its
    /// start point (`InputDispatcher` finds the touched window on `DOWN` and
    /// keeps the stream on it), both points converted into that window's
    /// space. Routing it by focus was the A11YPACE slip this rule replaces: a
    /// drag that starts on an overlay went to the activity that held focus.
    ///
    /// The stream is paced over `duration` (see
    /// [`InputDispatch::inject_swipe`]); the ack window starts after the `UP`
    /// is published, so a slow drag is never reported as "no acks" because the
    /// gesture itself consumed the budget.
    pub fn swipe(
        &self,
        from: (f32, f32),
        to: (f32, f32),
        duration: Duration,
        ack_wait: Duration,
    ) -> Result<InjectedGesture> {
        if self.stack.is_some() {
            let Some(target) = self.touch_target(from.0, from.1) else {
                return Err(no_touch_target(from.0, from.1));
            };
            let local_from = (target.x, target.y);
            let local_to = (
                to.0 - target.origin.0 as f32,
                to.1 - target.origin.1 as f32,
            );
            let gesture = lock(&self.state)
                .inject_swipe(target.token, local_from, local_to, duration)?;
            return self.collect(&target.token, &gesture, ack_wait);
        }
        let (token, gesture) = lock(&self.state).publish_on_live(|dispatch, token| {
            dispatch.inject_swipe(token, from, to, duration)
        })?;
        self.collect(&token, &gesture, ack_wait)
    }

    /// True while `token` still has an open, registered input channel.
    pub fn has_channel(&self, token: BinderId) -> bool {
        lock(&self.state).registry.publisher(token).is_some()
    }

    /// Publish one raw multi-pointer touch frame (for live interactive panel
    /// input). On `ACTION_DOWN` (`*pinned_target` is `None`), resolves the window
    /// under `pointers[0]` and writes `(token, origin)` into `pinned_target` as
    /// soon as the event is published — before waiting for acks — so even a
    /// post-publish error preserves the target pin for compensating releases.
    pub fn touch_frame(
        &self,
        action: i32,
        pinned_target: &mut Option<(BinderId, (i32, i32))>,
        down_time: i64,
        event_time: i64,
        pointers: &[(i32, f32, f32)],
        ack_wait: Duration,
    ) -> Result<(BinderId, (i32, i32), InjectedGesture)> {
        if pointers.is_empty() {
            return Err(ChannelError::BadPointerCount(0));
        }
        let ((token, origin), local_pointers, gesture) = if let Some((token, origin)) =
            *pinned_target
        {
            let local: Vec<(i32, f32, f32)> = pointers
                .iter()
                .map(|&(id, x, y)| (id, x - origin.0 as f32, y - origin.1 as f32))
                .collect();
            let gesture = lock(&self.state).inject_motion_frame(
                token,
                action,
                down_time,
                event_time,
                &local,
            )?;
            ((token, origin), local, gesture)
        } else if self.stack.is_some() {
            let Some(target) = self.touch_target(pointers[0].1, pointers[0].2) else {
                return Err(no_touch_target(pointers[0].1, pointers[0].2));
            };
            let local: Vec<(i32, f32, f32)> = pointers
                .iter()
                .map(|&(id, x, y)| (id, x - target.origin.0 as f32, y - target.origin.1 as f32))
                .collect();
            let gesture = lock(&self.state).inject_motion_frame(
                target.token,
                action,
                down_time,
                event_time,
                &local,
            )?;
            *pinned_target = Some((target.token, target.origin));
            ((target.token, target.origin), local, gesture)
        } else {
            let (token, gesture) = lock(&self.state).publish_on_live(|dispatch, token| {
                dispatch.inject_motion_frame(token, action, down_time, event_time, pointers)
            })?;
            *pinned_target = Some((token, (0, 0)));
            ((token, (0, 0)), pointers.to_vec(), gesture)
        };
        let _ = local_pointers;
        let injected = self.collect(&token, &gesture, ack_wait)?;
        Ok((token, origin, injected))
    }

    /// Publish one key press on the focused window (delivery only; window
    /// focus still needs the IME `EditorInfo` codec, so the app may drop it —
    /// the ack's `handled` field reports exactly that).
    pub fn key(&self, code: i32, ack_wait: Duration) -> Result<InjectedGesture> {
        let (token, gesture) = lock(&self.state)
            .publish_on_focused(|dispatch, token| dispatch.inject_key(token, code))?;
        self.collect(&token, &gesture, ack_wait)
    }

    /// Publish a single `KEY_ACTION_DOWN` or `KEY_ACTION_UP` on the focused
    /// window with explicit `meta_state` and `device_id`.
    pub fn key_event(
        &self,
        action: i32,
        code: i32,
        meta_state: i32,
        device_id: i32,
        down_time: i64,
        event_time: i64,
        ack_wait: Duration,
    ) -> Result<InjectedGesture> {
        let (token, gesture) = lock(&self.state).publish_on_focused(|dispatch, token| {
            dispatch.inject_key_event(
                token, action, code, meta_state, device_id, down_time, event_time,
            )
        })?;
        self.collect(&token, &gesture, ack_wait)
    }

    /// Publish `text` as a real key stroke per character on the focused window
    /// (falling back to the newest live one) and collect every `FINISHED`
    /// reply.
    ///
    /// This is the `type_text` session API. It is a **keyboard** path: the
    /// guest resolves each character through its `KeyCharacterMap`, exactly as
    /// for a physical keyboard. It is not an IME `commitText` (ACOS runs a null
    /// IME; see `crate::ime`).
    pub fn type_text(&self, text: &str, ack_wait: Duration) -> Result<InjectedGesture> {
        let (token, gesture) = lock(&self.state)
            .publish_on_focused(|dispatch, token| dispatch.inject_text(token, text))?;
        self.collect(&token, &gesture, ack_wait)
    }

    fn collect(
        &self,
        token: &BinderId,
        gesture: &Gesture,
        ack_wait: Duration,
    ) -> Result<InjectedGesture> {
        let seqs: Vec<u32> = gesture.dispatches.iter().map(|d| d.seq).collect();
        let traced = std::env::var_os("ACOS_TRACE_FILE").is_some()
            || std::env::var_os("ACOS_INPUT_TRACE").is_some();
        if traced {
            crate::trace::line(format!(
                "ACOSINPUT collect-wait token={:#x} seqs={seqs:?} budget_ms={}",
                token.0,
                ack_wait.as_millis()
            ));
        }
        let mut acks: Vec<InjectedAck> = Vec::new();
        let deadline = Instant::now() + ack_wait;
        loop {
            // REL3I: when the soft input is shown, the app's `ImeInputStage`
            // offers this key to its `InputMethodManager`, which writes it into
            // the IME channel and blocks on our `FINISHED`. Answer it here, on
            // the same wait that collects the window's own ack.
            let _ = crate::ime::service_pending_keys();
            let responses = match lock(&self.state).drain_acks(*token) {
                Ok(responses) => responses,
                Err(ChannelError::PeerClosed) => {
                    // The target's guest end went away while we waited. The
                    // publish went somewhere, but nothing can acknowledge it:
                    // report the acks we do have (possibly none) instead of
                    // failing the whole action. The dead token was already
                    // forgotten by `drain_acks`.
                    break;
                }
                Err(e) => return Err(e),
            };
            for response in responses {
                if let ConsumerResponse::Finished(ack) = response {
                    // P1/W59: the framework's `NativeInputEventReceiver`
                    // acknowledges FOCUS and TOUCH_MODE with
                    // `finishInputEvent(seq, true)`
                    // (`android_view_InputEventReceiver.cpp`), so their
                    // `FINISHED` replies sit queued from window attach until
                    // the first input drains them. They are *not* this
                    // gesture's acks: pushing them made the first tap return on
                    // two stale acks (latency measured from attach) and charged
                    // the tap's real acks to the next gesture. Only the
                    // sequences this gesture published count.
                    if !seqs.contains(&ack.seq) {
                        if std::env::var_os("ACOS_INPUT_TRACE").is_some() {
                            eprintln!(
                                "ACOSI2 stale-ack token={:#x} seq={} handled={} latency_ms={} discarded",
                                token.0,
                                ack.seq,
                                ack.handled,
                                ack.latency
                                    .map(|l| l.as_micros() as f64 / 1000.0)
                                    .map(|ms| format!("{ms:.2}"))
                                    .unwrap_or_else(|| "untracked".into())
                            );
                        }
                        continue;
                    }
                    acks.push(InjectedAck {
                        seq: ack.seq,
                        handled: ack.handled,
                        // `InputChannelRegistry` measured publish -> this
                        // matching `FINISHED`, so this is the real delivery
                        // latency, never a stale ack's attach time.
                        latency_us: ack.latency.map(|l| l.as_micros() as u64),
                    });
                }
            }
            if acks.len() >= seqs.len() || Instant::now() >= deadline {
                break;
            }
            thread::sleep(Duration::from_millis(5));
        }
        Ok(InjectedGesture {
            token: *token,
            seqs,
            acks,
        })
    }
}

fn env_u64(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(default)
}

fn parse_injection() -> Option<Injection> {
    if let Ok(tap) = std::env::var("ACOS_INPUT_TAP") {
        let mut parts = tap.split(',');
        if let (Some(x), Some(y), None) = (parts.next(), parts.next(), parts.next()) {
            if let (Ok(x), Ok(y)) = (x.trim().parse(), y.trim().parse()) {
                return Some(Injection::Tap { x, y });
            }
        }
        eprintln!("ACOSI2 ignoring malformed ACOS_INPUT_TAP={tap:?} (want \"x,y\")");
    }
    if let Ok(code) = std::env::var("ACOS_INPUT_KEY") {
        if let Ok(code) = code.trim().parse() {
            return Some(Injection::Key { code });
        }
        eprintln!("ACOSI2 ignoring malformed ACOS_INPUT_KEY={code:?}");
    }
    None
}

/// `KeyEvent.META_SHIFT_ON` (the framework value on the image).
pub const META_SHIFT_ON: i32 = 1;
/// `KeyCharacterMap.VIRTUAL_KEYBOARD`: the device a host-injected text key
/// names so the guest resolves the character through its virtual keyboard map.
pub const VIRTUAL_KEYBOARD_DEVICE_ID: i32 = -1;

/// Map printable ASCII text to Android key strokes (`keyCode`, `metaState`) in
/// the **US/QWERTY** layout of the image's virtual keyboard.
///
/// This is only the host-side encoding of a character into the key that would
/// produce it on a US keyboard. The guest still resolves the character through
/// its own `KeyCharacterMap` (`KeyEvent.getUnicodeChar`), so this is the same
/// path a physical keyboard uses, not an `InputConnection` shortcut.
///
/// A character with no US key is a structured error naming the character; no
/// event is published for a text that cannot be fully encoded.
pub fn text_strokes(text: &str) -> std::result::Result<Vec<(i32, i32)>, ChannelError> {
    let mut out = Vec::with_capacity(text.chars().count());
    for ch in text.chars() {
        let (code, shift) = match ch {
            'a'..='z' => (29 + (ch as i32 - 'a' as i32), false),
            'A'..='Z' => (29 + (ch as i32 - 'A' as i32), true),
            '0'..='9' => (7 + (ch as i32 - '0' as i32), false),
            ' ' => (62, false),
            '\n' => (66, false),
            '\t' => (61, false),
            '-' => (69, false),
            '_' => (69, true),
            '=' => (70, false),
            '+' => (70, true),
            '[' => (71, false),
            '{' => (71, true),
            ']' => (72, false),
            '}' => (72, true),
            '\\' => (73, false),
            '|' => (73, true),
            ';' => (74, false),
            ':' => (74, true),
            '\'' => (75, false),
            '"' => (75, true),
            ',' => (55, false),
            '<' => (55, true),
            '.' => (56, false),
            '>' => (56, true),
            '/' => (76, false),
            '?' => (76, true),
            '`' => (68, false),
            '~' => (68, true),
            '!' => (8, true),
            '@' => (9, true),
            '#' => (10, true),
            '$' => (11, true),
            '%' => (12, true),
            '^' => (13, true),
            '&' => (14, true),
            '*' => (15, true),
            '(' => (16, true),
            ')' => (7, true),
            other => {
                return Err(ChannelError::Io(format!(
                    "no US key produces character {other:?} (U+{:04X}); text input is ASCII-only",
                    other as u32
                )))
            }
        };
        out.push((code, if shift { META_SHIFT_ON } else { 0 }));
    }
    Ok(out)
}

fn injector_loop(
    state: Arc<Mutex<InputDispatch>>,
    rx: Receiver<BinderId>,
    plan: Injection,
    delay: Duration,
    ack_wait: Duration,
) {
    loop {
        let token = match rx.recv_timeout(Duration::from_secs(60)) {
            Ok(token) => token,
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => return,
        };
        thread::sleep(delay);
        let injected = {
            let mut d = lock(&state);
            match plan {
                Injection::Tap { x, y } => d.publish_tap(token, x, y),
                Injection::Key { code } => d.publish_key(token, code),
            }
        };
        match &injected {
            Ok(gesture) => eprintln!(
                "ACOSI2 inject {} token={:#x} seqs={:?}",
                match plan {
                    Injection::Tap { x, y } => format!("tap x={x} y={y}"),
                    Injection::Key { code } => format!("key code={code}"),
                },
                token.0,
                gesture.dispatches.iter().map(|d| d.seq).collect::<Vec<_>>()
            ),
            Err(e) => {
                eprintln!("ACOSI2 inject failed token={:#x}: {e}", token.0);
                continue;
            }
        }
        // Read the guest's FINISHED/TIMELINE replies for a bounded window.
        let deadline = Instant::now() + ack_wait;
        let mut seen = 0usize;
        while Instant::now() < deadline {
            let result = {
                let mut d = lock(&state);
                d.drain_acks(token)
            };
            match result {
                Ok(responses) => {
                    for response in responses {
                        match response {
                            ConsumerResponse::Finished(ack) => {
                                seen += 1;
                                eprintln!(
                                    "ACOSI2 ack token={:#x} seq={} handled={} latency_ms={}",
                                    token.0,
                                    ack.seq,
                                    ack.handled,
                                    ack.latency
                                        .map(|l| l.as_micros() as f64 / 1000.0)
                                        .map(|ms| format!("{ms:.2}"))
                                        .unwrap_or_else(|| "untracked".into())
                                );
                            }
                            ConsumerResponse::Timeline(timeline) => {
                                eprintln!(
                                    "ACOSI2 timeline token={:#x} seq={}",
                                    token.0, timeline.seq
                                );
                            }
                        }
                    }
                    // A tap produces two acks (DOWN and UP).
                    if seen >= 2 {
                        break;
                    }
                }
                Err(ChannelError::PeerClosed) => {
                    eprintln!(
                        "ACOSI2 channel closed by guest token={:#x} during ack wait",
                        token.0
                    );
                    break;
                }
                Err(e) => {
                    eprintln!("ACOSI2 ack error token={:#x}: {e}", token.0);
                    break;
                }
            }
            thread::sleep(Duration::from_millis(5));
        }
        let now = monotonic_nanos();
        eprintln!("ACOSI2 ack-window done token={:#x} now_ns={now}", token.0);
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The token an environment-driven injection will target when exactly one
/// window is live.
#[must_use]
pub fn default_token(d: &InputDispatch) -> Option<BinderId> {
    d.newest_token()
}

/// The structured refusal for a touch with no touchable window under it.
///
/// `InputDispatcher` drops such a gesture (there is nothing to deliver it to);
/// ACOS names the point instead of publishing it to some other window.
fn no_touch_target(x: f32, y: f32) -> ChannelError {
    ChannelError::NoTouchTarget(format!(
        "no touchable window at ({x},{y}) (the layer-stack hit test found none)"
    ))
}

#[cfg(test)]
mod focus_target_tests {
    use super::*;

    const FOCUSED: BinderId = BinderId(0x9a01);
    const NEWEST: BinderId = BinderId(0x9a02);

    /// A11YPACE: a key/gesture goes to the window that holds **input focus**,
    /// not to the newest one.
    ///
    /// Measured before this existed: a `TYPE_ACCESSIBILITY_OVERLAY`
    /// (`FLAG_NOT_FOCUSABLE|FLAG_NOT_TOUCHABLE`) was the newest window, so
    /// `back` was published to the overlay's channel, whose `ViewRootImpl`
    /// logged `VRI[]: Dropping event (no window focus)` — the app never saw the
    /// key. Focus is the layer stack's answer, so the overlay can be above the
    /// activity and still be skipped.
    #[test]
    fn keys_go_to_the_focused_window_not_the_newest() {
        let mut dispatch = InputDispatch::new();
        dispatch
            .register_window(FOCUSED, "activity")
            .expect("register activity");
        dispatch
            .register_window(NEWEST, "accessibility overlay")
            .expect("register overlay");
        dispatch.set_focus(Some(FOCUSED)).expect("publish focus");
        assert_eq!(dispatch.newest_token(), Some(NEWEST));

        let (token, gesture) = dispatch
            .publish_on_focused(|d, token| d.inject_key(token, crate::input_channel::KEYCODE_BACK))
            .expect("the focused channel accepts the key");
        assert_eq!(token, FOCUSED);
        assert_eq!(gesture.dispatches.len(), 2, "DOWN and UP");

        // With no focus known the fallback is the newest live window, so a bare
        // host still has a defined target.
        dispatch.set_focus(None).expect("clear focus");
        let (token, _) = dispatch
            .publish_on_focused(|d, token| d.inject_key(token, crate::input_channel::KEYCODE_BACK))
            .expect("the newest channel accepts the key");
        assert_eq!(token, NEWEST);
    }
}

#[cfg(test)]
mod touch_target_tests {
    use super::*;
    use crate::input_channel::{InputMessage, LoopbackConsumer};
    use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
    use acos_wm::display::DEFAULT_INSETS_MODE;
    use acos_wm::layers::{FLAG_NOT_FOCUSABLE, FLAG_NOT_TOUCHABLE, TYPE_APPLICATION};

    const ACTIVITY: u64 = 0x5a01;
    const DIALOG: u64 = 0x5a02;
    const OVERLAY: u64 = 0x5a03;

    /// The three windows the rule has to tell apart: an activity on the whole
    /// display, a dialog above it that holds focus, and a non-focusable,
    /// non-touchable accessibility overlay that was registered **last** (so it
    /// is "the newest window" for any rule that guesses by registration order).
    fn host_with_stack() -> (InputHost, Arc<Mutex<crate::wms::WmsState>>) {
        let state = crate::wms::WmsState::new_with_insets(DEFAULT_INSETS_MODE);
        let stack = Arc::new(Mutex::new(state));
        let session = {
            let mut wms = stack.lock().expect("wms");
            let session = wms.allocate_session();
            let mut activity = acos_wm::codec::LayoutParams::application("t", "activity");
            activity.type_ = TYPE_APPLICATION;
            activity.width = -1;
            activity.height = -1;
            wms.wm
                .add_to_display_as_user(session, BinderId(ACTIVITY), activity, 0)
                .expect("add activity");
            // A dialog in the top half: it takes focus, not the whole display.
            let mut dialog = acos_wm::codec::LayoutParams::application("t", "dialog");
            dialog.type_ = TYPE_APPLICATION;
            dialog.width = 600;
            dialog.height = 400;
            wms.wm
                .add_to_display_as_user(session, BinderId(DIALOG), dialog, 0)
                .expect("add dialog");
            // The overlay is added last (newest) and is neither focusable nor
            // touchable: it must not take focus and must not take a touch.
            let mut overlay = acos_wm::codec::LayoutParams::application("t", "overlay");
            overlay.type_ = TYPE_APPLICATION;
            overlay.width = 200;
            overlay.height = 200;
            overlay.flags = FLAG_NOT_TOUCHABLE | FLAG_NOT_FOCUSABLE;
            wms.wm
                .add_to_display_as_user(session, BinderId(OVERLAY), overlay, 0)
                .expect("add overlay");
            session
        };
        let _ = session;
        let mut dispatch = InputDispatch::new();
        for (token, name) in [
            (BinderId(ACTIVITY), "activity"),
            (BinderId(DIALOG), "dialog"),
            (BinderId(OVERLAY), "accessibility overlay"),
        ] {
            dispatch.register_window(token, name).expect("register");
        }
        // Focus is on the dialog; the overlay is the newest registration.
        dispatch
            .set_focus(Some(BinderId(DIALOG)))
            .expect("publish focus");
        let host = InputHost {
            state: Arc::new(Mutex::new(dispatch)),
            relayout_tx: None,
            stack: Some(Arc::clone(&stack)),
        };
        (host, stack)
    }

    /// WVTAP: a **touch** goes to the window under the point
    /// (`InputDispatcher::findTouchedWindowAt`), not to the focused window and
    /// not to the newest registration.
    ///
    /// Measured before this existed (A11YPACE): the scenario `tap` used
    /// `publish_on_live` ("the newest live window") and `swipe` used
    /// `publish_on_focused`, so with a dialog focused and an overlay registered
    /// last, a tap on the activity's own content either went to the overlay's
    /// channel or to the dialog — and the activity never saw the touch
    /// (`webview-probe` lost its page click; `ELEMENT_NOT_FOUND`).
    #[test]
    fn touch_goes_to_the_window_under_the_point_not_the_focused_or_newest() {
        let (host, stack) = host_with_stack();
        let (dialog_frame, activity_frame, overlay_frame) = {
            let wms = stack.lock().expect("wms");
            (
                wms.wm.windows[&DIALOG].frame,
                wms.wm.windows[&ACTIVITY].frame,
                wms.wm.windows[&OVERLAY].frame,
            )
        };
        // Inside the dialog: the dialog is under the point (and has focus).
        let target = host
            .touch_target(
                (dialog_frame.left + 10) as f32,
                (dialog_frame.top + 10) as f32,
            )
            .expect("the dialog is under the point");
        assert_eq!(target.token, BinderId(DIALOG));
        assert_eq!(target.origin, (dialog_frame.left, dialog_frame.top));
        assert_eq!(target.x, 10.0, "the point is the window's own space");
        assert_eq!(target.y, 10.0);
        // Outside the dialog and the overlay, inside the activity: the touch
        // goes to the activity even though the dialog holds focus.
        let bottom = (activity_frame.left + 40, activity_frame.bottom - 100);
        let target = host
            .touch_target(bottom.0 as f32, bottom.1 as f32)
            .expect("the activity is under the point");
        assert_eq!(target.token, BinderId(ACTIVITY));
        // Both rules this replaces are demonstrably wrong here: the tap step
        // used to publish on the newest live window, and `swipe` on the focused
        // one. Either would have sent this touch somewhere else.
        assert_eq!(
            host.newest_token(),
            Some(BinderId(OVERLAY)),
            "the newest window is the overlay, not the window under the point"
        );
        assert_eq!(
            host.focused_token(),
            Some(BinderId(DIALOG)),
            "the focused window is the dialog, not the window under the point"
        );
        assert_eq!(target.origin, (activity_frame.left, activity_frame.top));
        // Under the overlay: the overlay is NOT touchable, so the touch passes
        // through to whatever is below it (`LAYER1`'s pass-through rule). The
        // expected window is the stack's own answer; the assertion that matters
        // is that the routing *used* the stack and the overlay never took it.
        let under = (overlay_frame.left + 5, overlay_frame.top + 5);
        let expected = {
            let wms = stack.lock().expect("wms");
            wms.wm
                .hit_test(acos_wm::geom::PointI::new(under.0, under.1))
                .expect("something is under the overlay point")
        };
        let target = host
            .touch_target(under.0 as f32, under.1 as f32)
            .expect("a touchable window is still under the overlay");
        assert_eq!(
            target.token,
            BinderId(expected),
            "the touch follows the layer stack's own hit test"
        );
        assert_ne!(
            target.token,
            BinderId(OVERLAY),
            "a FLAG_NOT_TOUCHABLE overlay never takes the touch"
        );
    }

    /// The same host routes a **key** by focus: one rule per event class.
    #[test]
    fn keys_still_go_to_the_focused_window_on_the_same_host() {
        let (host, _stack) = host_with_stack();
        let mut state = lock(&host.state);
        let (token, gesture) = state
            .publish_on_focused(|d, token| d.inject_key(token, crate::input_channel::KEYCODE_BACK))
            .expect("the focused channel accepts the key");
        assert_eq!(token, BinderId(DIALOG), "the dialog holds focus");
        assert_eq!(gesture.dispatches.len(), 2);
        assert_eq!(
            state.newest_token(),
            Some(BinderId(OVERLAY)),
            "the overlay is the newest registration: a key must not go there"
        );
    }

    /// A point with no touchable window under it is a **structured refusal**,
    /// never a delivery to some other window (`InputDispatcher` drops it).
    #[test]
    fn a_touch_with_no_window_under_the_point_is_refused() {
        let (host, _stack) = host_with_stack();
        let err = host
            .tap(100_000.0, 100_000.0, Duration::from_millis(1))
            .expect_err("nothing is under that point");
        let message = err.to_string();
        assert!(
            message.contains("no touchable window at (100000,100000)"),
            "the refusal names the point, got {message:?}"
        );
        assert!(
            message.contains("layer-stack hit test"),
            "the refusal says which rule found nothing, got {message:?}"
        );
    }

    /// The **end-to-end** pin for the routing rule: the scenario `tap` (the
    /// public API every input path uses) delivers through the *channel of the
    /// window under the point*, and the other two windows' channels see
    /// nothing.
    ///
    /// Before this rule the same tap published on the newest live window (the
    /// overlay), so the activity's own channel saw no `Motion` at all — that is
    /// the shape of the measured A11YPACE regression (`webview-probe` step 11,
    /// `ELEMENT_NOT_FOUND`: the page's click never fired). The guests are real
    /// `InputChannel` loopbacks, so "delivered" here means the event was written
    /// to that window's channel and acknowledged, not that a target was picked.
    #[test]
    fn a_scenario_tap_delivers_through_the_channel_of_the_window_under_the_point() {
        let (host, stack) = host_with_stack();
        let activity_frame = {
            let wms = stack.lock().expect("wms");
            wms.wm.windows[&ACTIVITY].frame
        };
        // Three real client channels, one per window.
        let mut guests = Vec::new();
        for token in [ACTIVITY, DIALOG, OVERLAY] {
            let fd = lock(&host.state)
                .duplicate_client_fd(BinderId(token))
                .expect("client fd");
            guests.push(Arc::new(Mutex::new(LoopbackConsumer::new(fd))));
        }
        let counts = [
            Arc::new(AtomicUsize::new(0)),
            Arc::new(AtomicUsize::new(0)),
            Arc::new(AtomicUsize::new(0)),
        ];
        let stop = Arc::new(AtomicUsize::new(0));
        let mut ackers = Vec::new();
        for (guest, count) in guests.iter().cloned().zip(counts.iter().cloned()) {
            let stop = Arc::clone(&stop);
            ackers.push(thread::spawn(move || {
                loop {
                    {
                        let mut guest = guest.lock().expect("guest");
                        // Registration publishes TOUCH_MODE/FOCUS on this
                        // channel too; only Motion carries the gesture.
                        while let Some(message) = guest.recv().expect("recv") {
                            if let InputMessage::Motion { seq, .. } = message {
                                count.fetch_add(1, AtomicOrdering::SeqCst);
                                guest.send_finished(seq, true).expect("ack");
                            }
                        }
                    }
                    if stop.load(AtomicOrdering::SeqCst) == 1 {
                        return;
                    }
                    thread::sleep(Duration::from_millis(2));
                }
            }));
        }
        // A point inside the activity and outside the dialog and the overlay.
        let x = (activity_frame.left + 40) as f32;
        let y = (activity_frame.bottom - 100) as f32;
        let gesture = host.tap(x, y, Duration::from_secs(2)).expect("tap");
        stop.store(1, AtomicOrdering::SeqCst);
        for acker in ackers {
            acker.join().expect("acker");
        }
        assert_eq!(gesture.token, BinderId(ACTIVITY));
        assert_eq!(gesture.seqs.len(), 2, "DOWN and UP");
        assert_eq!(
            counts[0].load(AtomicOrdering::SeqCst),
            2,
            "the activity's channel carries the gesture (DOWN, UP)"
        );
        assert_eq!(
            counts[1].load(AtomicOrdering::SeqCst),
            0,
            "the focused dialog never sees a tap that was not on it"
        );
        assert_eq!(
            counts[2].load(AtomicOrdering::SeqCst),
            0,
            "the newest window (the overlay) never sees it either"
        );
    }

    /// A swipe is a touch stream: its target is the window under its **start**
    /// point, and both ends are published in that window's space.
    #[test]
    fn a_swipe_goes_to_the_window_under_its_start_point() {
        let (host, stack) = host_with_stack();
        let activity_frame = {
            let wms = stack.lock().expect("wms");
            wms.wm.windows[&ACTIVITY].frame
        };
        let from = (activity_frame.left as f32 + 40.0, activity_frame.bottom as f32 - 300.0);
        let to = (from.0, from.1 - 200.0);
        let injected = host
            .swipe(from, to, Duration::from_millis(60), Duration::from_millis(1))
            .expect("the activity takes the swipe");
        assert_eq!(
            injected.token,
            BinderId(ACTIVITY),
            "the swipe follows the point, not the dialog's focus"
        );
        assert!(injected.seqs.len() >= 3, "DOWN, MOVEs, UP");
    }

    #[test]
    fn touch_frame_preserves_pinned_target_after_post_publish_error() {
        use crate::input_channel::{InputMessage, LoopbackConsumer};
        let (host, stack) = host_with_stack();
        let (dialog_frame, activity_frame) = {
            let wms = stack.lock().expect("wms");
            (wms.wm.windows[&DIALOG].frame, wms.wm.windows[&ACTIVITY].frame)
        };
        let dialog_fd = host
            .state
            .lock()
            .expect("state")
            .duplicate_client_fd(BinderId(DIALOG))
            .expect("dialog fd");
        let activity_fd = host
            .state
            .lock()
            .expect("state")
            .duplicate_client_fd(BinderId(ACTIVITY))
            .expect("activity fd");
        let mut dialog_guest = LoopbackConsumer::new(dialog_fd);
        let mut activity_guest = LoopbackConsumer::new(activity_fd);
        // Drain initial focus/registration messages.
        while dialog_guest.recv().expect("dialog drain").is_some() {}
        while activity_guest.recv().expect("activity drain").is_some() {}

        let dialog_pt = ((dialog_frame.left + 20) as f32, (dialog_frame.top + 20) as f32);
        let activity_pt = (
            (activity_frame.left + 40) as f32,
            (activity_frame.bottom - 100) as f32,
        );
        // Confirm activity_pt really hit-tests to ACTIVITY, not DIALOG.
        assert_eq!(
            host.touch_target(activity_pt.0, activity_pt.1)
                .expect("activity target")
                .token,
            BinderId(ACTIVITY)
        );

        let down_time = crate::input_channel::monotonic_nanos();
        let mut pinned = None;
        let (token, origin, _) = host
            .touch_frame(
                crate::input_channel::ACTION_DOWN,
                &mut pinned,
                down_time,
                down_time,
                &[(0, dialog_pt.0, dialog_pt.1)],
                Duration::ZERO,
            )
            .expect("pointer A down on dialog");
        assert_eq!(token, BinderId(DIALOG));
        assert_eq!(pinned, Some((BinderId(DIALOG), origin)));

        // Inject a malformed reply byte on the dialog channel so `collect` fails
        // AFTER `ACTION_POINTER_DOWN` for pointer B is published to DIALOG.
        let malformed_fd = host
            .state
            .lock()
            .expect("state")
            .duplicate_client_fd(BinderId(DIALOG))
            .expect("dialog raw fd");
        rustix::io::write(&malformed_fd, &[0xff, 0xff, 0xff, 0xff]).expect("write malformed");

        // Pointer A has moved over ACTIVITY while pointer B goes down.
        let action_p2_down = crate::input_channel::ACTION_POINTER_DOWN
            | (1 << crate::input_channel::ACTION_POINTER_INDEX_SHIFT);
        let err = host
            .touch_frame(
                action_p2_down,
                &mut pinned,
                down_time,
                down_time + 10_000_000,
                &[
                    (0, activity_pt.0, activity_pt.1),
                    (1, dialog_pt.0 + 10.0, dialog_pt.1 + 10.0),
                ],
                Duration::from_millis(50),
            )
            .expect_err("malformed ack should surface as collect error");
        assert!(matches!(err, ChannelError::Malformed(_)), "got {err:?}");
        // Pinned target MUST remain DIALOG despite the post-publish error.
        assert_eq!(pinned, Some((BinderId(DIALOG), origin)));

        // Compensating ACTION_POINTER_UP for B with A still over ACTIVITY MUST
        // reach DIALOG (never ACTIVITY).
        let action_p2_up = crate::input_channel::ACTION_POINTER_UP
            | (1 << crate::input_channel::ACTION_POINTER_INDEX_SHIFT);
        let (up_token, _, _) = host
            .touch_frame(
                action_p2_up,
                &mut pinned,
                down_time,
                down_time + 20_000_000,
                &[
                    (0, activity_pt.0, activity_pt.1),
                    (1, dialog_pt.0 + 10.0, dialog_pt.1 + 10.0),
                ],
                Duration::ZERO,
            )
            .expect("compensating pointer up succeeds on pinned dialog");
        assert_eq!(up_token, BinderId(DIALOG));

        let mut dialog_actions = Vec::new();
        while let Some(msg) = dialog_guest.recv().expect("dialog recv") {
            if let InputMessage::Motion { event, .. } = msg {
                dialog_actions.push(event.action);
            }
        }
        assert_eq!(
            dialog_actions,
            vec![
                crate::input_channel::ACTION_DOWN,
                action_p2_down,
                action_p2_up,
            ],
            "DIALOG must receive A down, B pointer-down, and compensating B pointer-up"
        );

        let mut activity_motions = Vec::new();
        while let Some(msg) = activity_guest.recv().expect("activity recv") {
            if let InputMessage::Motion { event, .. } = msg {
                activity_motions.push(event.action);
            }
        }
        assert!(
            activity_motions.is_empty(),
            "ACTIVITY must never receive DIALOG's compensating POINTER_UP: {activity_motions:?}"
        );
    }
}

#[cfg(test)]
mod window_removal_tests {
    use super::*;

    const DELIVERED: BinderId = BinderId(0x7a01);
    const OTHER: BinderId = BinderId(0x7a02);

    fn dispatch_with_two_windows() -> InputDispatch {
        let mut dispatch = InputDispatch::new();
        dispatch
            .register_window(DELIVERED, "Surface(name=acos-fixture/dialog)/@0x20")
            .expect("register dialog");
        dispatch
            .register_window(OTHER, "Surface(name=acos-fixture/activity)/@0x21")
            .expect("register activity");
        dispatch
    }

    /// REL3I3: a dialog's Back removes the window the key was delivered to, and
    /// that is the evidence for `handled: true` (no ack can exist: the receiver
    /// was disposed). It must be scoped to the **delivered** token, so removing
    /// a different window is never evidence about this input.
    #[test]
    fn only_the_delivered_windows_removal_counts() {
        let mut dispatch = dispatch_with_two_windows();
        let mark = dispatch.removal_mark();
        // A different window goes away first: no evidence about the delivery.
        assert!(dispatch.remove(OTHER));
        assert!(
            !dispatch.was_removed_since(DELIVERED, mark),
            "a different window's removal must not count for the delivered one"
        );
        assert!(dispatch.was_removed_since(OTHER, mark));

        // Then the window the input went to is removed.
        assert!(dispatch.remove(DELIVERED));
        assert!(
            dispatch.was_removed_since(DELIVERED, mark),
            "the delivered window's own removal is the evidence"
        );
    }

    /// A removal before the gesture does not blur into it: the mark is taken
    /// when the action is published.
    #[test]
    fn a_removal_before_the_mark_is_not_evidence() {
        let mut dispatch = dispatch_with_two_windows();
        assert!(dispatch.remove(DELIVERED));
        let mark = dispatch.removal_mark();
        dispatch.remove(OTHER);
        dispatch
            .register_window(DELIVERED, "Surface(name=acos-fixture/dialog)/@0x22")
            .expect("re-register");
        assert!(
            !dispatch.was_removed_since(DELIVERED, mark),
            "an earlier removal of the same window id is not this gesture's evidence"
        );
    }

    /// Without a removal the answer is unchanged: no evidence at all.
    #[test]
    fn no_removal_means_no_evidence() {
        let dispatch = dispatch_with_two_windows();
        let mark = dispatch.removal_mark();
        assert!(!dispatch.was_removed_since(DELIVERED, mark));
    }
}

#[cfg(test)]
mod ack_accounting_tests {
    use super::*;
    use crate::input_channel::{InputMessage, LoopbackConsumer};

    const TOKEN: BinderId = BinderId(0x7701);

    /// P1/W59: focus and touch-mode are acknowledged when the window attaches,
    /// before any gesture exists. Those `FINISHED` replies must be drained and
    /// discarded, and the tap must report its *own* publish -> `FINISHED`
    /// latency; counting the stale pair made the first tap look instantaneous
    /// and the next gesture pay for it.
    /// LAYER1: focus is published by policy, not by registration. A newly
    /// added window only gets `FOCUS(true)` when it becomes the target; the
    /// window it took focus from gets `FOCUS(false)` first.
    #[test]
    fn focus_messages_follow_the_policy_target_and_only_on_change() {
        const OTHER: BinderId = BinderId(0x7702);
        let mut dispatch = InputDispatch::new();
        dispatch
            .register_window(TOKEN, "activity")
            .expect("register");
        dispatch
            .register_window(OTHER, "overlay")
            .expect("register");
        dispatch.set_focus(Some(TOKEN)).expect("focus activity");
        assert_eq!(dispatch.focused(), Some(TOKEN));

        let fd = dispatch.duplicate_client_fd(TOKEN).expect("client fd");
        let mut guest = LoopbackConsumer::new(fd);
        // Registration publishes TOUCH_MODE; focus is the next message.
        match guest.recv().expect("recv").expect("message") {
            InputMessage::TouchMode { .. } => {}
            other => panic!("expected TOUCH_MODE, got {other:?}"),
        }
        match guest.recv().expect("recv").expect("message") {
            InputMessage::Focus { has_focus, .. } => assert!(has_focus),
            other => panic!("expected FOCUS(true), got {other:?}"),
        }
        // Same target again: nothing is published.
        dispatch.set_focus(Some(TOKEN)).expect("same focus");
        assert_eq!(
            dispatch
                .events()
                .iter()
                .filter(|e| matches!(e, InputEvent::Published { kind: "focus", .. }))
                .count(),
            1
        );

        // A not-focusable overlay never becomes the target; `set_focus` is only
        // ever called with the WMS `focus_target()`, so the activity stays.
        dispatch.remove(OTHER);
        assert_eq!(dispatch.focused(), Some(TOKEN));
    }

    #[test]
    fn stale_focus_and_touch_mode_acks_are_never_counted_as_the_taps() {
        let mut dispatch = InputDispatch::new();
        dispatch
            .register_window(TOKEN, "Surface(name=acos-fixture/counter)/@0x10")
            .expect("register");
        // LAYER1: the window session publishes policy focus after the add.
        dispatch.set_focus(Some(TOKEN)).expect("focus");
        let fd = dispatch.duplicate_client_fd(TOKEN).expect("client fd");
        let mut guest = LoopbackConsumer::new(fd);

        let mut stale = Vec::new();
        for _ in 0..2 {
            match guest.recv().expect("recv").expect("registration message") {
                InputMessage::TouchMode { seq, .. } | InputMessage::Focus { seq, .. } => {
                    stale.push(seq);
                }
                other => panic!("unexpected registration message {other:?}"),
            }
        }
        for seq in &stale {
            guest.send_finished(*seq, true).expect("stale ack");
        }

        let state = Arc::new(Mutex::new(dispatch));
        let host = InputHost {
            state: Arc::clone(&state),
            relayout_tx: None,
            stack: None,
        };
        let acker = thread::spawn(move || {
            let mut acked = 0;
            while acked < 2 {
                while let Some(message) = guest.recv().expect("recv") {
                    if let InputMessage::Motion { seq, .. } = message {
                        guest.send_finished(seq, true).expect("tap ack");
                        acked += 1;
                    }
                }
                thread::sleep(Duration::from_millis(2));
            }
        });

        let gesture = host.tap(10.0, 20.0, Duration::from_secs(2)).expect("tap");
        acker.join().expect("acker");

        assert_eq!(gesture.seqs.len(), 2);
        assert_eq!(gesture.acks.len(), 2, "only the tap's own two acks");
        let mut got: Vec<u32> = gesture.acks.iter().map(|ack| ack.seq).collect();
        got.sort_unstable();
        let mut want = gesture.seqs.clone();
        want.sort_unstable();
        assert_eq!(got, want, "the tap's own sequences, never the stale pair");
        assert!(
            gesture.acks.iter().all(|ack| !stale.contains(&ack.seq)),
            "stale focus/touch-mode acks were counted as the tap's"
        );
        assert!(
            gesture.acks.iter().all(|ack| ack.latency_us.is_some()),
            "a matching ack carries its real publish -> FINISHED latency"
        );
    }
}

/// REL3S: the swipe event stream.
///
/// A swipe is not "a tap at two places": the framework's scroll/fling physics
/// read the *timestamps* and the *positions over time*. These tests pin the
/// properties a real touchscreen stream has, so a regression to "DOWN, UP" (or
/// to a burst with identical event times) fails here before it reaches an app.
#[cfg(test)]
mod swipe_stream_tests {
    use super::*;
    use crate::input_channel::{
        swipe_samples, DispatchKind, InputMessage, LoopbackConsumer, GESTURE_SAMPLE_INTERVAL_NANOS,
    };

    const TOKEN: BinderId = BinderId(0x8801);

    #[test]
    fn swipe_samples_interpolate_the_path_at_the_refresh_cadence() {
        let duration_ns = 320_000_000; // 320 ms
        let samples = swipe_samples((10.0, 600.0), (10.0, 100.0), duration_ns, GESTURE_SAMPLE_INTERVAL_NANOS);
        // DOWN + one MOVE per 16 ms (20) with the last sample serving as the UP.
        assert_eq!(samples.len(), 21, "down + 20 refresh samples");
        assert_eq!(samples.first().unwrap().offset_nanos, 0);
        assert_eq!(samples.first().unwrap().x, 10.0);
        assert_eq!(samples.first().unwrap().y, 600.0);
        let last = samples.last().unwrap();
        assert_eq!(last.offset_nanos, duration_ns, "the lift is the last sample");
        assert_eq!((last.x, last.y), (10.0, 100.0), "the lift is the requested end point");
        // Timestamps are strictly increasing and evenly spaced (the velocity a
        // VelocityTracker derives is the gesture's real speed: 500 px / 320 ms).
        for pair in samples.windows(2) {
            let step = pair[1].offset_nanos - pair[0].offset_nanos;
            assert!(step > 0, "event times must increase");
            assert!(
                (step - GESTURE_SAMPLE_INTERVAL_NANOS).abs() <= 1,
                "each sample is one refresh interval after the previous: {step}"
            );
            assert!(pair[1].y < pair[0].y, "an upward swipe moves monotonically up");
        }
        let travelled = samples.first().unwrap().y - samples.last().unwrap().y;
        assert!((travelled - 500.0).abs() < 0.01);
    }

    #[test]
    fn a_short_swipe_still_publishes_one_move_and_ends_at_the_lift_time() {
        let samples = swipe_samples((0.0, 0.0), (100.0, 0.0), 5_000_000, GESTURE_SAMPLE_INTERVAL_NANOS);
        assert_eq!(samples.len(), 2, "DOWN + one interpolated MOVE==UP");
        assert_eq!(samples[1].offset_nanos, 5_000_000);
        assert_eq!((samples[1].x, samples[1].y), (100.0, 0.0));
    }

    #[test]
    fn publish_swipe_sends_down_moves_and_up_with_real_event_times() {
        let mut dispatch = InputDispatch::new();
        dispatch
            .register_window(TOKEN, "Surface(name=acos-fixture/scroll-list)/@0x20")
            .expect("register");
        let fd = dispatch.duplicate_client_fd(TOKEN).expect("client fd");
        let mut guest = LoopbackConsumer::new(fd);
        // Drain the registration message (touch mode). Focus is the policy's
        // (`set_focus`, LAYER1), never registration order.
        guest.recv().expect("recv").expect("registration message");

        let started = Instant::now();
        let gesture = dispatch
            .inject_swipe(
                TOKEN,
                (540.0, 1800.0),
                (540.0, 600.0),
                Duration::from_millis(160),
            )
            .expect("swipe");
        let elapsed = started.elapsed();
        assert!(
            elapsed >= Duration::from_millis(120),
            "the stream is paced at the refresh cadence, not published in a burst: {elapsed:?}"
        );

        // 160 ms / 16 ms = 10 samples; adjacent samples share `downTime`.
        assert_eq!(gesture.dispatches.len(), 11, "down + 10 refresh samples");
        assert_eq!(gesture.dispatches[0].kind, DispatchKind::MotionDown);
        assert_eq!(gesture.dispatches[10].kind, DispatchKind::MotionUp);
        assert!(
            gesture.dispatches[1..10]
                .iter()
                .all(|d| d.kind == DispatchKind::MotionMove)
        );
        let down_time = gesture.dispatches[0].down_time;
        assert!(gesture.dispatches.iter().all(|d| d.down_time == down_time));
        for pair in gesture.dispatches.windows(2) {
            assert!(pair[1].event_time > pair[0].event_time);
        }
        assert_eq!(
            gesture.dispatches[10].event_time - down_time,
            160_000_000,
            "the lift carries the requested duration"
        );

        // The guest sees the interpolated positions and the increasing times.
        let mut events = Vec::new();
        while let Some(message) = guest.recv().expect("recv") {
            if let InputMessage::Motion { event, .. } = message {
                events.push(event);
            }
        }
        assert_eq!(events.len(), 11);
        assert_eq!(events[0].action, crate::input_channel::ACTION_DOWN);
        assert_eq!(events[0].transform.ty, 1800.0);
        assert_eq!(events[10].action, crate::input_channel::ACTION_UP);
        assert_eq!(events[10].transform.ty, 600.0);
        assert!(
            events.windows(2).all(|pair| pair[1].transform.ty < pair[0].transform.ty),
            "the finger travels up monotonically"
        );
        assert!(
            events.windows(2).all(|pair| pair[1].event_time > pair[0].event_time),
            "the wire carries real, increasing event times"
        );
    }

    #[test]
    fn input_host_swipe_reports_every_events_handled_bit() {
        let mut dispatch = InputDispatch::new();
        dispatch
            .register_window(TOKEN, "Surface(name=acos-fixture/scroll-list)/@0x30")
            .expect("register");
        let fd = dispatch.duplicate_client_fd(TOKEN).expect("client fd");
        let mut guest = LoopbackConsumer::new(fd);
        // Registration publishes TOUCH_MODE; focus follows the layer stack.
        guest.recv().expect("recv").expect("registration message");
        let state = Arc::new(Mutex::new(dispatch));
        let host = InputHost {
            state: Arc::clone(&state),
            relayout_tx: None,
            stack: None,
        };
        let acker = thread::spawn(move || {
            let mut acked = 0;
            while acked < 6 {
                while let Some(message) = guest.recv().expect("recv") {
                    if let InputMessage::Motion { seq, .. } = message {
                        // REL3E: the app window's own finishInputEvent bit.
                        guest.send_finished(seq, seq % 2 == 0).expect("ack");
                        acked += 1;
                    }
                }
                thread::sleep(Duration::from_millis(2));
            }
        });
        let gesture = host
            .swipe(
                (540.0, 1200.0),
                (540.0, 900.0),
                Duration::from_millis(80), // 5 samples + down = 6 events
                Duration::from_secs(2),
            )
            .expect("swipe");
        acker.join().expect("acker");
        assert_eq!(gesture.seqs.len(), 6);
        assert_eq!(gesture.acks.len(), 6, "every published event is acknowledged");
        let mut want = gesture.seqs.clone();
        want.sort_unstable();
        let mut got: Vec<u32> = gesture.acks.iter().map(|ack| ack.seq).collect();
        got.sort_unstable();
        assert_eq!(got, want);
        assert!(
            gesture.acks.iter().any(|ack| !ack.handled) && gesture.acks.iter().any(|ack| ack.handled),
            "the mixed handled/unhandled acks are reported per event, not folded away"
        );
    }

    #[test]
    fn multi_pointer_touch_frame_and_key_event() {
        let mut dispatch = InputDispatch::new();
        dispatch
            .register_window(TOKEN, "Surface(name=acos-fixture/panel)/@0x31")
            .expect("register");
        let fd = dispatch.duplicate_client_fd(TOKEN).expect("client fd");
        let mut guest = LoopbackConsumer::new(fd);
        guest.recv().expect("recv").expect("registration");
        let state = Arc::new(Mutex::new(dispatch));
        let host = InputHost {
            state: Arc::clone(&state),
            relayout_tx: None,
            stack: None,
        };

        let down_time = crate::input_channel::monotonic_nanos();
        let mut pinned = None;
        let (token, origin, _) = host
            .touch_frame(
                crate::input_channel::ACTION_DOWN,
                &mut pinned,
                down_time,
                down_time,
                &[(0, 100.0, 200.0)],
                Duration::ZERO,
            )
            .expect("down");
        assert_eq!(token, TOKEN);
        assert_eq!(pinned, Some((token, origin)));
        let action_p2 = crate::input_channel::ACTION_POINTER_DOWN
            | (1 << crate::input_channel::ACTION_POINTER_INDEX_SHIFT);
        host.touch_frame(
            action_p2,
            &mut pinned,
            down_time,
            down_time + 10_000_000,
            &[(0, 100.0, 200.0), (1, 150.0, 260.0)],
            Duration::ZERO,
        )
        .expect("pointer down");
        host.key_event(
            crate::input_channel::KEY_ACTION_DOWN,
            crate::input_channel::KEYCODE_ENTER,
            super::META_SHIFT_ON,
            super::VIRTUAL_KEYBOARD_DEVICE_ID,
            down_time,
            down_time + 20_000_000,
            Duration::ZERO,
        )
        .expect("key down");

        let mut msgs = Vec::new();
        while let Some(msg) = guest.recv().expect("recv") {
            msgs.push(msg);
        }
        assert_eq!(msgs.len(), 3);
        match &msgs[1] {
            InputMessage::Motion { event, .. } => {
                assert_eq!(event.action, action_p2);
                assert_eq!(event.pointers.len(), 2);
                assert_eq!(event.transform.tx, 100.0);
                assert_eq!(event.transform.ty, 200.0);
                assert_eq!(event.pointers[0].coords.get(crate::input_channel::AXIS_X), 0.0);
                assert_eq!(event.pointers[1].coords.get(crate::input_channel::AXIS_X), 50.0);
                assert_eq!(event.pointers[1].coords.get(crate::input_channel::AXIS_Y), 60.0);
            }
            other => panic!("expected motion, got {other:?}"),
        }
        match &msgs[2] {
            InputMessage::Key { event, .. } => {
                assert_eq!(event.action, crate::input_channel::KEY_ACTION_DOWN);
                assert_eq!(event.key_code, crate::input_channel::KEYCODE_ENTER);
                assert_eq!(event.meta_state, super::META_SHIFT_ON);
                assert_eq!(event.device_id, super::VIRTUAL_KEYBOARD_DEVICE_ID);
            }
            other => panic!("expected key, got {other:?}"),
        }
    }
}
