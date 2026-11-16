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
    relayouts: usize,
    events: Vec<InputEvent>,
    plan: Option<Injection>,
    inject_delay: Duration,
    ack_wait: Duration,
}

impl InputDispatch {
    /// An empty dispatcher.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a pair for `token`, publish `TOUCH_MODE(true)` and
    /// `FOCUS(true)` (the state the real dispatcher sends to a newly focused
    /// window), and keep the server end. The client end is written into the add
    /// reply by [`InputDispatch::write_and_forget`].
    ///
    /// W13: `FOCUS` is now delivered by default. The image's focus path runs
    /// `IInputMethodManager.startInputOrWindowGainedFocus`, whose `EditorInfo`
    /// request and `InputBindResult` reply are marshalled by
    /// [`crate::ime_wire`], so focus no longer kills the guest's main looper.
    /// Setting `ACOS_INPUT_NO_FOCUS=1` suppresses it for diagnostics only.
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
        if std::env::var_os("ACOS_INPUT_NO_FOCUS").is_none() {
            self.publish_focus(token, true)?;
        }
        Ok(())
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

    /// Publish a key press on `token`'s channel.
    pub fn inject_key(&mut self, token: BinderId, key_code: i32) -> Result<Gesture> {
        self.publish_key(token, key_code)
    }

    fn publish_key(&mut self, token: BinderId, key_code: i32) -> Result<Gesture> {
        let Some(publisher) = self.registry.publisher_mut(token) else {
            let message = format!("inject_key: no input channel for token {:#x}", token.0);
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
                    message: format!("inject_key: {e}"),
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
                        ConsumerResponse::Finished(ack) => self.events.push(InputEvent::Acked {
                            token,
                            ack: *ack,
                        }),
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
                    self.events.push(InputEvent::Closed { token });
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
    pub fn remove(&mut self, token: BinderId) -> bool {
        let was = self.registry.remove(token);
        self.tokens.remove(&token.0);
        if was {
            self.events.push(InputEvent::Closed { token });
        }
        was
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

/// The session-facing handle: shared producer state plus the injector thread.
#[derive(Debug, Clone)]
pub struct InputHost {
    state: Arc<Mutex<InputDispatch>>,
    relayout_tx: Option<Sender<BinderId>>,
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
        Self { state, relayout_tx }
    }

    /// An inert host (no channel, no thread); the session keeps its W2 path.
    #[must_use]
    pub fn disabled() -> Self {
        Self {
            state: Arc::new(Mutex::new(InputDispatch::new())),
            relayout_tx: None,
        }
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

    /// Publish one tap on the newest live window and collect the guest's
    /// `FINISHED` replies for a bounded window.
    ///
    /// This is the interactive-session API (W10). It is the same
    /// `InputDispatch::inject_tap` I2 already proved (window-transform-carried
    /// coordinates on the real `InputChannel`), with the wait for the guest's
    /// acks instead of a fire-and-forget log.
    pub fn tap(&self, x: f32, y: f32, ack_wait: Duration) -> Result<InjectedGesture> {
        let token = self
            .newest_token()
            .ok_or(ChannelError::Malformed("no live window input channel yet"))?;
        let gesture = lock(&self.state).inject_tap(token, x, y)?;
        self.collect(&token, &gesture, ack_wait)
    }

    /// Publish one swipe/scroll on the newest live window and collect the
    /// guest's `FINISHED` replies for a bounded window.
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
        let token = self
            .newest_token()
            .ok_or(ChannelError::Malformed("no live window input channel yet"))?;
        let gesture = lock(&self.state).inject_swipe(token, from, to, duration)?;
        self.collect(&token, &gesture, ack_wait)
    }

    /// Publish one key press on the newest live window (delivery only; window
    /// focus still needs the IME `EditorInfo` codec, so the app may drop it —
    /// the ack's `handled` field reports exactly that).
    pub fn key(&self, code: i32, ack_wait: Duration) -> Result<InjectedGesture> {
        let token = self
            .newest_token()
            .ok_or(ChannelError::Malformed("no live window input channel yet"))?;
        let gesture = lock(&self.state).inject_key(token, code)?;
        self.collect(&token, &gesture, ack_wait)
    }

    /// Publish `text` as a real key stroke per character on the newest live
    /// window and collect every `FINISHED` reply.
    ///
    /// This is the `type_text` session API. It is a **keyboard** path: the
    /// guest resolves each character through its `KeyCharacterMap`, exactly as
    /// for a physical keyboard. It is not an IME `commitText` (ACOS runs a null
    /// IME; see `crate::ime`).
    pub fn type_text(&self, text: &str, ack_wait: Duration) -> Result<InjectedGesture> {
        let token = self
            .newest_token()
            .ok_or(ChannelError::Malformed("no live window input channel yet"))?;
        let gesture = lock(&self.state).inject_text(token, text)?;
        self.collect(&token, &gesture, ack_wait)
    }

    fn collect(
        &self,
        token: &BinderId,
        gesture: &Gesture,
        ack_wait: Duration,
    ) -> Result<InjectedGesture> {
        let seqs: Vec<u32> = gesture.dispatches.iter().map(|d| d.seq).collect();
        let mut acks: Vec<InjectedAck> = Vec::new();
        let deadline = Instant::now() + ack_wait;
        while Instant::now() < deadline && acks.len() < seqs.len() {
            let responses = lock(&self.state).drain_acks(*token)?;
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
            if acks.len() < seqs.len() {
                thread::sleep(Duration::from_millis(5));
            }
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
    #[test]
    fn stale_focus_and_touch_mode_acks_are_never_counted_as_the_taps() {
        let mut dispatch = InputDispatch::new();
        dispatch
            .register_window(TOKEN, "Surface(name=acos-fixture/counter)/@0x10")
            .expect("register");
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

        let gesture = host
            .tap(10.0, 20.0, Duration::from_secs(2))
            .expect("tap");
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
        // Drain the registration messages (touch mode + focus).
        for _ in 0..2 {
            guest.recv().expect("recv").expect("registration message");
        }

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
        for _ in 0..2 {
            guest.recv().expect("recv").expect("registration message");
        }
        let state = Arc::new(Mutex::new(dispatch));
        let host = InputHost {
            state: Arc::clone(&state),
            relayout_tx: None,
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
}
