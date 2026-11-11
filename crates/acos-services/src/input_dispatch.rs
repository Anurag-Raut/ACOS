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

    /// Create a pair for `token`, publish `TOUCH_MODE(true)` (the state the
    /// real dispatcher sends before the first touch), and keep the server end.
    /// The client end is written into the add reply by
    /// [`InputDispatch::write_and_forget`].
    ///
    /// `FOCUS` is deliberately **not** published by default: the framework's
    /// `handleWindowFocusChanged` calls
    /// `IInputMethodManager.startInputOrWindowGainedFocus` with an `EditorInfo`
    /// argument that this host's generic parcel codec cannot decode yet
    /// (`no parcelable codec for android.view.inputmethod.EditorInfo`), which
    /// throws on the guest's main looper. That is an adjacent IME wall, not an
    /// input-channel one; [`InputDispatch::publish_focus`] is the real API and
    /// `ACOS_INPUT_FOCUS=1` opts in (for the day the IME codec lands).
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
        if std::env::var_os("ACOS_INPUT_FOCUS").is_some() {
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

    /// `true` when an injection plan is configured.
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.relayout_tx.is_some()
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
