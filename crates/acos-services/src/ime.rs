//! `svc.ime`: the `IInputMethodManager` / `IInputMethodClient` /
//! `IImeTracker` service side and the in-process IME's key path.
//!
//! # Two modes, selected by the transport
//!
//! * **Bound IME (ART transport).** The transport attaches an
//!   `IInputMethodSession` binder ([`ImeState::attach_session`]); from then on
//!   `startInputOrWindowGainedFocus` answers `InputBindResult.SUCCESS` with that
//!   session, a session id and a real `InputChannel` pair. The app's
//!   `InputMethodManager` keeps the client end in `mCurChannel` and its
//!   `dispatchInputEvent` writes each `KeyEvent` into it; this manager keeps
//!   the server end and reads those parcels ([`ImeState::service_pending_keys`]).
//!   On `KEYCODE_BACK` while the soft input is shown it hides it (the WMS
//!   insets change the app sees, plus the IME visibility state) and answers
//!   `FINISHED(handled=true)`; every other key is answered
//!   `FINISHED(handled=false)` and therefore follows the existing view-tree
//!   typing path unchanged. This is AOSP's `InputMethodService` key path,
//!   reached through the framework's own `InputMethodManager`.
//! * **Null IME (VM transport).** Without an attached session the manager is the
//!   declared V1 null IME: `startInput` answers `NO_IME`, `showSoftInput`
//!   answers `false`, and typed text reaches the app only as injected
//!   `KeyEvent`s.
//!
//! `IInputMethodClient.setActive/onBindMethod/reportFullscreenMode` and
//! `IInputMethodSession.viewClicked/updateSelection` are decoded and recorded so
//! the trace shows what the framework asked for. `IImeTracker` tokens/progress
//! are recorded.

use std::os::fd::OwnedFd;
use std::sync::{Arc, Mutex};

use acos_parcel::binder::BinderId;
use acos_parcel::BinderRegistry;

use crate::error::{Result, ServiceError};
use crate::host::{CallResult, ServiceObject};
use crate::input_channel::{open_channel_fds, InputMessage, LoopbackConsumer};
use crate::value::Value;

/// `InputBindResult.SUCCESS`.
pub const BIND_SUCCESS: i32 = 0;
/// `InputBindResult.NO_IME` — no IME bound for this field (V1 divergence).
pub const BIND_NO_IME: i32 = 6; // API 34 InputBindResult.NO_IME = error(6)
/// `InputBindResult.NO_EDITOR` — no editor on the client.
pub const BIND_NO_EDITOR: i32 = 2;
/// `InputBindResult.ALREADY_BOUND`.
pub const BIND_ALREADY_BOUND: i32 = -1;

/// `InputMethodManager.RESULT_UNSHOWN_IME_IS_NOT_CURRENT`.
pub const RESULT_UNSHOWN_IME_IS_NOT_CURRENT: i32 = 2;
/// `InputMethodManager.RESULT_SHOWN`.
pub const RESULT_SHOWN: i32 = 2;

/// Append one diagnostic line to `$ACOS_IME_TRACE` when the env seam is set.
///
/// The launcher's stderr is a bounded tail the engine only reports on failure,
/// so the IME's key path needs its own opt-in trace file (the same pattern as
/// `ACOS_IME_DUMP`). Not a result channel: nothing reads it back.
fn trace(line: &str) {
    if let Ok(path) = std::env::var("ACOS_IME_TRACE") {
        use std::io::Write;
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        {
            let _ = writeln!(file, "{line}");
        }
    }
}

/// The binder descriptor of the fresh token every `InputChannel` pair carries.
///
/// `InputChannel::openInputChannelPair` (`frameworks/base/libs/input/
/// InputTransport.cpp`) creates `sp<IBinder> token = new BBinder();` **per
/// pair**; the token is the channel's identity, not the connection's. Nothing
/// ever transacts on it: the framework only compares it
/// (`InputMethodManager.areSameInputChannel`).
pub const CHANNEL_TOKEN_DESCRIPTOR: &str = "android.os.IBinder";
/// `KeyEvent.KEYCODE_BACK`.
pub const KEYCODE_BACK: i32 = 4;

/// One key the app offered to the IME through its `InputChannel`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImeKeyEvent {
    /// The channel sequence (`ImeInputEventSender`'s `getSequenceNumber`).
    pub seq: u32,
    /// `KeyEvent.getKeyCode()`.
    pub key_code: i32,
    /// `KeyEvent.getAction()` (`0` down, `1` up).
    pub action: i32,
    /// The `FINISHED(handled)` this manager answered.
    pub handled: bool,
    /// `true` when this key hid the soft input (the layer consumed it).
    pub hid_ime: bool,
}

/// The server end of the IME `InputBindResult` channel plus the client end
/// until the reply parcel carries it.
///
/// Direction: the **app** produces (`InputMethodManager.dispatchInputEvent` ->
/// `ImeInputEventSender`), ACOS consumes. That is why this side is a
/// [`LoopbackConsumer`] and not the `InputPublisher` a window channel uses.
#[derive(Debug)]
pub struct ImeChannel {
    /// The decoder + `FINISHED` writer over the server end.
    pub consumer: LoopbackConsumer,
    /// The `InputChannel` token written into the parcel (the session binder).
    pub token: BinderId,
    /// The channel name the app sees (`"<name> (client)"`).
    pub name: String,
    /// The client end, until [`ImeState::take_bind_channel`] hands it to the
    /// reply writer. `None` once written (this process keeps no copy).
    pub client_fd: Option<OwnedFd>,
}

/// One registered `IInputMethodClient`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImeClient {
    /// The client binder (`InputMethodManager$1` in the app).
    pub binder: Option<BinderId>,
    /// Its `IRemoteInputConnection` (null for the null IME).
    pub remote_connection: Option<BinderId>,
    /// User id from `addClient`.
    pub user_id: i32,
    /// Whether the framework reported the client active.
    pub active: bool,
    /// Whether `startInput` produced a bound session.
    pub session_bound: bool,
}

/// One `ImeTracker` progress record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TrackerRecord {
    /// Token id (`ImeTracker.Token.mId`).
    pub token: i64,
    /// `onProgress` phase, or `-1` for `onRequestShow`/`onShown`/`onHidden`.
    pub phase: i32,
}

/// `IInputMethodManager`/`IInputMethodClient`/`IImeTracker` state.
#[derive(Debug)]
pub struct ImeState {
    /// Registered clients, in `addClient` order.
    pub clients: Vec<ImeClient>,
    /// `startInputOrWindowGainedFocus` calls: `(client, sequence, result)`.
    pub start_inputs: Vec<(BinderId, i32, i32)>,
    /// `showSoftInput` calls: `(client, flags, answered)`.
    pub show_inputs: Vec<(BinderId, i32, bool)>,
    /// `hideSoftInput` calls.
    pub hide_inputs: Vec<(BinderId, i32)>,
    /// Whether the (null) IME surface is considered visible: always false.
    pub ime_visible: bool,
    /// `ImeTracker` records in arrival order.
    pub tracker: Vec<TrackerRecord>,
    /// `onProgress` count (the trace measures 22 ± 3).
    pub progress_count: u32,
    /// `IInputMethodSession.updateSelection` calls.
    pub selection_updates: u32,
    /// `IInputMethodSession.viewClicked` calls.
    pub view_clicks: u32,
    /// Recorded `IInputMethodClient` deliveries (`setActive`, ...).
    pub client_deliveries: Vec<(&'static str, bool)>,
    /// The `IInputMethodSession` binder this transport can hand the app, once
    /// attached by the service registry. `None` keeps the null-IME mode.
    session_binder: Option<BinderId>,
    /// The live `InputBindResult` channel (server end held here).
    pub channel: Option<ImeChannel>,
    /// The session id last handed out (`InputBindResult.id`, non-null exactly
    /// when the bind succeeded).
    pub session_id: Option<String>,
    /// Every key the app offered to the IME, in arrival order.
    pub channel_keys: Vec<ImeKeyEvent>,
    /// Every `FINISHED(seq, handled)` written back to the app, in order.
    pub channel_replies: Vec<(u32, bool)>,
    /// `true` once a `startInput` bound a session (the soft input can be shown).
    session_bound: bool,
    /// `true` while a handled BACK's UP half is still owed an answer.
    back_tracking: bool,
    /// The `ACOSWMS` state whose IME insets follow this manager's visibility.
    wms: Option<Arc<Mutex<crate::wms::WmsState>>>,
    /// The next synthetic session binder (never handed out by the null IME).
    next_session: u64,
    /// The strong references the host holds on the registered clients: every
    /// `addClient` stores an `IInputMethodClient` (and its
    /// `IRemoteInputConnection`), which on Android is an `sp<IBinder>` each.
    /// AOSP drops them when the client process dies; ACOS has no
    /// `removeClient` in `IInputMethodManager`, so the peer's
    /// process-teardown `release_all_refs` is the release path (`crate::refs`).
    pub refs: crate::refs::GuestRefs,
    /// The registry the per-pair `InputChannel` token is allocated from. The
    /// ART transport attaches the session's own registry
    /// ([`ImeState::attach_binder_registry`]); a unit-test state allocates from
    /// a registry of its own. `BinderRegistry` is internally synchronised, so a
    /// shared `Arc` is all a token needs.
    tokens: Arc<BinderRegistry>,
}

impl ImeState {
    /// A null-IME manager: no client bound, no soft keyboard.
    #[must_use]
    pub fn new() -> Self {
        Self {
            clients: Vec::new(),
            start_inputs: Vec::new(),
            show_inputs: Vec::new(),
            hide_inputs: Vec::new(),
            ime_visible: false,
            tracker: Vec::new(),
            progress_count: 0,
            selection_updates: 0,
            view_clicks: 0,
            client_deliveries: Vec::new(),
            session_binder: None,
            channel: None,
            session_id: None,
            channel_keys: Vec::new(),
            channel_replies: Vec::new(),
            session_bound: false,
            back_tracking: false,
            wms: None,
            refs: crate::refs::GuestRefs::new(),
            next_session: 0x1_3e00,
            tokens: Arc::new(BinderRegistry::new()),
        }
    }

    /// Attach the `IInputMethodSession` binder the transport answers with.
    ///
    /// This is what turns the null IME into the bound IME: without it the
    /// manager keeps the declared `NO_IME` answer, with it every
    /// `startInputOrWindowGainedFocus` binds a real session and channel.
    pub fn attach_session(&mut self, session: BinderId) {
        self.session_binder = Some(session);
    }

    /// `true` when this manager can bind an IME session.
    #[must_use]
    pub fn has_session_binder(&self) -> bool {
        self.session_binder.is_some()
    }

    /// Follow the `ACOSWMS` window state: an IME show/hide flips the app's IME
    /// insets source on the focused window (the change the app sees).
    pub fn attach_wms(&mut self, wms: Arc<Mutex<crate::wms::WmsState>>) {
        self.wms = Some(wms);
    }

    /// Allocate the per-pair `InputChannel` tokens from `registry`.
    ///
    /// AOSP's `openInputChannelPair` gives **every** pair its own fresh
    /// `BBinder` token. The token is what the framework compares in
    /// `InputMethodManager.areSameInputChannel`: with a token shared between
    /// pairs, a second `startInput` looks like "the same connection" and the
    /// framework keeps the *old* `mCurChannel` while this manager keeps the
    /// *new* pair's server end — the app then writes keys into a socket whose
    /// peer was dropped (`sendInputEvent` returns false and the key silently
    /// falls through to the view tree). Allocating from the session's own
    /// registry keeps the written handle a real, releasable binder.
    pub fn attach_binder_registry(&mut self, registry: Arc<BinderRegistry>) {
        self.tokens = registry;
    }

    /// The token of one new channel pair: a fresh local binder handle, exactly
    /// like `openInputChannelPair`'s `new BBinder()`.
    fn next_channel_token(&mut self) -> BinderId {
        self.tokens.register_local(CHANNEL_TOKEN_DESCRIPTOR)
    }

    /// Pop the `IWindow` insets callbacks this manager's IME show/hide queued.
    ///
    /// The show rides the `showSoftInput` transaction, but the hide is driven by
    /// a key on the channel (no transaction of its own), so the binder peer's
    /// idle flush drains this queue too (`ServiceHost::drain_raw_deliveries_all`).
    pub fn take_ime_deliveries(&mut self) -> Vec<crate::host::PendingDelivery> {
        let Some(wms) = &self.wms else {
            return Vec::new();
        };
        let mut state = wms.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        crate::window_session::take_ime_insets_deliveries(&mut state)
    }

    /// Take the client end of the newly bound channel (the reply writer owns it
    /// from here, exactly like `addToDisplayAsUser`'s `out InputChannel`).
    pub fn take_bind_channel(&mut self) -> Option<(BinderId, String, OwnedFd)> {
        let channel = self.channel.as_mut()?;
        let fd = channel.client_fd.take()?;
        Some((channel.token, channel.name.clone(), fd))
    }

    /// `addClient(client, remoteConnection, userId)`; repeated calls update.
    pub fn add_client(
        &mut self,
        client: Option<BinderId>,
        remote_connection: Option<BinderId>,
        user_id: i32,
    ) {
        if let Some(existing) = self
            .clients
            .iter_mut()
            .find(|c| c.binder.is_some() && c.binder == client)
        {
            // Re-registering the same client updates it (AOSP's `ClientState`
            // is keyed by the client binder); it is not a second reference.
            if let Some(remote) = remote_connection {
                if existing.remote_connection != remote_connection {
                    if let Some(previous) = existing.remote_connection {
                        self.refs.release(previous);
                    }
                    self.refs.hold(remote);
                }
            }
            existing.remote_connection = remote_connection;
            existing.user_id = user_id;
            return;
        }
        // The manager stores the client and its remote connection: one strong
        // reference each, so the guest's nodes stay rooted while the client is
        // registered.
        if let Some(client) = client.filter(|id| !id.is_null()) {
            self.refs.hold(client);
        }
        if let Some(remote) = remote_connection.filter(|id| !id.is_null()) {
            self.refs.hold(remote);
        }
        self.clients.push(ImeClient {
            binder: client,
            remote_connection,
            user_id,
            active: false,
            session_bound: false,
        });
    }

    /// `startInputOrWindowGainedFocus`.
    ///
    /// Returns `(result_code, method, session_binder)`. With a session binder
    /// attached (the ART transport) this binds the real session and creates a
    /// fresh `InputChannel` pair, so the framework's `InputMethodManager` can
    /// offer keys to the IME (`mCurChannel != null`). Without one it stays the
    /// declared null-IME answer: `NO_IME`, no session, no channel.
    pub fn start_input(
        &mut self,
        client: Option<BinderId>,
        sequence: i32,
        remote_connection: Option<BinderId>,
    ) -> (i32, Option<String>, Option<BinderId>) {
        if let Some(c) = client {
            if let Some(entry) = self.clients.iter_mut().find(|e| e.binder == Some(c)) {
                if remote_connection.is_some() {
                    entry.remote_connection = remote_connection;
                }
            } else {
                self.add_client(Some(c), remote_connection, 0);
            }
        }
        let Some(session) = self.session_binder else {
            let client = client.unwrap_or(BinderId(0));
            self.start_inputs.push((client, sequence, BIND_NO_IME));
            // The null IME never has a session. Keeping `next_session` bumping
            // here would imply one; it stays untouched so the bound IME can
            // hand out this range.
            let _ = &mut self.next_session;
            self.ime_visible = false;
            return (BIND_NO_IME, None, None);
        };
        let id = format!("acos-ime-{sequence}");
        // `InputChannel::openInputChannelPair("<ime> (server)", ...)`: the app
        // gets the client end in the bind result, this manager keeps the
        // server end. A previous bind's channel is disposed (the framework
        // disposed `mCurChannel` for the old sequence before it asked again).
        match open_channel_fds() {
            Ok((server, client_fd)) => {
                // AOSP: a fresh token per pair (never the session binder, which
                // is shared by every bind from the same session).
                let token = self.next_channel_token();
                let name = format!("{id} (client)");
                self.channel = Some(ImeChannel {
                    consumer: LoopbackConsumer::new(server),
                    token,
                    name: name.clone(),
                    client_fd: Some(client_fd),
                });
                self.back_tracking = false;
                self.session_bound = true;
                let client = client.unwrap_or(BinderId(0));
                if let Some(entry) = self.clients.iter_mut().find(|e| e.binder == Some(client)) {
                    entry.session_bound = true;
                }
                self.start_inputs.push((client, sequence, BIND_SUCCESS));
                self.session_id = Some(id.clone());
                let line = format!(
                    "bind sequence={sequence} client={:#x} session={:#x} channel=\"{name}\"",
                    client.0, token.0
                );
                eprintln!("ACOSIME {line}");
                trace(&line);
                (BIND_SUCCESS, Some(id), Some(session))
            }
            Err(e) => {
                // No channel means the framework would silently fall back to
                // the view tree; that is a structured failure, never a success.
                let client = client.unwrap_or(BinderId(0));
                self.start_inputs.push((client, sequence, BIND_NO_IME));
                eprintln!("ACOSIME bind sequence={sequence} channel pair failed: {e}");
                (BIND_NO_IME, None, None)
            }
        }
    }

    /// `showSoftInput`: the soft input is shown when a session is bound.
    pub fn show_soft_input(&mut self, client: BinderId, flags: i32) -> bool {
        let bound = self.session_bound
            && (client.is_null()
                || self
                    .clients
                    .iter()
                    .any(|c| c.session_bound && c.binder == Some(client)));
        self.show_inputs.push((client, flags, bound));
        if bound {
            self.ime_visible = true;
            self.set_wms_ime_visible(true);
            eprintln!("ACOSIME showSoftInput client={:#x} shown=true", client.0);
            trace(&format!("showSoftInput client={:#x} shown=true", client.0));
        }
        bound
    }

    /// `hideSoftInput`: hides the soft input; the receiver is still answered.
    pub fn hide_soft_input(&mut self, client: BinderId, flags: i32) -> bool {
        self.hide_inputs.push((client, flags));
        let was = self.ime_visible;
        self.ime_visible = false;
        self.back_tracking = false;
        if was {
            self.set_wms_ime_visible(false);
        }
        was
    }

    fn set_wms_ime_visible(&mut self, visible: bool) {
        let Some(wms) = &self.wms else { return };
        let mut state = wms.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        match state.set_ime_visible(visible) {
            Ok(()) => trace(&format!("wms set_ime_visible({visible})")),
            Err(e) => {
                eprintln!("ACOSIME wms set_ime_visible({visible}) failed: {e}");
                trace(&format!("wms set_ime_visible({visible}) failed: {e}"));
            }
        }
    }

    /// Read every key the app offered to the IME and answer it.
    ///
    /// AOSP's path: `InputMethodManager.dispatchInputEvent` writes the
    /// `KeyEvent` into `mCurChannel`; the IME service reads it, and
    /// `InputMethodService.onKeyDown/onKeyUp` hides the soft input on BACK and
    /// reports the key handled. Every other key is answered `handled=false`, so
    /// `ViewRootImpl.ImeInputStage` forwards it to the view tree exactly as
    /// before (the typing path is unchanged).
    ///
    /// Returns the number of keys answered.
    pub fn service_pending_keys(&mut self) -> usize {
        let mut answered = 0;
        loop {
            let Some(channel) = self.channel.as_mut() else {
                return answered;
            };
            let message = match channel.consumer.recv() {
                Ok(Some(message)) => message,
                Ok(None) => return answered,
                Err(crate::input_channel::ChannelError::PeerClosed) => {
                    // The app disposed `mCurChannel`; nothing can be answered on
                    // this end any more. The next `startInput` makes a new one.
                    eprintln!("ACOSIME channel closed by the app");
                    self.channel = None;
                    return answered;
                }
                Err(e) => {
                    eprintln!("ACOSIME channel read failed: {e}");
                    return answered;
                }
            };
            let InputMessage::Key { seq, event } = message else {
                // The IME channel carries keys only (`ImeInputEventSender`).
                continue;
            };
            let is_back = event.key_code == KEYCODE_BACK;
            let handled = match (is_back, event.action) {
                (true, 0) => {
                    let handled = self.ime_visible;
                    if handled {
                        // The soft input consumed the press: hide it (the
                        // app sees the IME insets change and the framework's
                        // visibility state flips) and finish the key handled.
                        self.ime_visible = false;
                        self.set_wms_ime_visible(false);
                        self.back_tracking = true;
                        eprintln!("ACOSIME back-down hid the soft input (seq={seq})");
                        trace(&format!("back-down hid the soft input seq={seq}"));
                    }
                    handled
                }
                (true, 1) => {
                    let handled = self.back_tracking;
                    self.back_tracking = false;
                    handled
                }
                _ => false,
            };
            let hid_ime = is_back && event.action == 0 && handled;
            if let Some(channel) = self.channel.as_mut() {
                match channel.consumer.send_finished(seq, handled) {
                    Ok(()) => self.channel_replies.push((seq, handled)),
                    Err(e) => eprintln!("ACOSIME FINISHED({seq}, {handled}) failed: {e}"),
                }
            }
            self.channel_keys.push(ImeKeyEvent {
                seq,
                key_code: event.key_code,
                action: event.action,
                handled,
                hid_ime,
            });
            answered += 1;
        }
    }

    /// Record a `IInputMethodClient.setActive` / `reportFullscreenMode`.
    pub fn record_client_delivery(&mut self, what: &'static str, value: bool) {
        if what == "setActive" {
            for c in &mut self.clients {
                c.active = value;
            }
        }
        self.client_deliveries.push((what, value));
    }

    /// `IImeTracker.onRequestShow` / `onShown` / `onRequestHide` / `onHidden`.
    pub fn tracker_event(&mut self, token: i64, phase: i32) {
        self.tracker.push(TrackerRecord { token, phase });
    }

    /// `IImeTracker.onProgress`.
    pub fn tracker_progress(&mut self, token: i64, phase: i32) {
        self.progress_count += 1;
        self.tracker.push(TrackerRecord { token, phase });
    }

    /// `IInputMethodSession.updateSelection`.
    pub fn update_selection(&mut self) {
        self.selection_updates += 1;
    }

    /// `IInputMethodSession.viewClicked`.
    pub fn view_clicked(&mut self) {
        self.view_clicks += 1;
    }

    /// The declared capability set, for evidence output.
    #[must_use]
    pub fn capabilities(&self) -> serde_json::Value {
        let bound = self.session_binder.is_some();
        serde_json::json!({
            "soft_keyboard": bound,
            "ime_bound": bound,
            "bind_result": if bound { "SUCCESS" } else { "NO_IME" },
            "input_channel": bound,
            "text_injection": "input-dispatcher",
            "divergence": if bound {
                "the soft input is an in-process IME role: no IME APK, no keyboard surface; keys reach it on a real InputChannel"
            } else {
                "IME not bound (this transport runs the null in-process IME)"
            },
        })
    }
}

impl Default for ImeState {
    fn default() -> Self {
        Self::new()
    }
}

/// The process-wide IME manager state (one null in-process IME per ACOS user).
///
/// The state machines that serve `IInputMethodManager` (this crate) and the
/// Java marshaller that materialises its replies (`acos-runtime::ime_java`)
/// must observe the *same* state; a private `Arc` inside the host service would
/// let `ime_java` answer a `startInput` the manager never recorded.
static IME: Mutex<Option<Arc<Mutex<ImeState>>>> = Mutex::new(None);

/// The shared IME state, installing a fresh one on first use.
#[must_use]
pub fn shared() -> Arc<Mutex<ImeState>> {
    let mut slot = IME.lock().unwrap();
    if let Some(state) = slot.as_ref() {
        return Arc::clone(state);
    }
    let state = Arc::new(Mutex::new(ImeState::new()));
    *slot = Some(Arc::clone(&state));
    state
}

/// Replace the shared IME state (test isolation / session reset).
pub fn install(state: Arc<Mutex<ImeState>>) {
    *IME.lock().unwrap() = Some(state);
}

/// Service the bound IME's key channel, if one exists.
///
/// Called by the input dispatcher while it waits for the window's `FINISHED`
/// replies: the app's `ViewRootImpl.ImeInputStage` offers the key to its
/// `InputMethodManager`, which writes it into the IME channel and blocks on our
/// answer. Reading and answering it here is what keeps that wait bounded (the
/// framework's own `INPUT_METHOD_NOT_RESPONDING_TIMEOUT` otherwise fires).
///
/// Returns the number of keys answered; installing nothing when no session is
/// bound.
pub fn service_pending_keys() -> usize {
    let state = {
        let slot = IME.lock().unwrap();
        let Some(state) = slot.as_ref() else {
            return 0;
        };
        Arc::clone(state)
    };
    let mut state = state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    state.service_pending_keys()
}

/// `true` when a client is bound and the soft input is shown.
#[must_use]
pub fn soft_input_shown() -> bool {
    let slot = IME.lock().unwrap();
    slot.as_ref()
        .map(|state| {
            state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .ime_visible
        })
        .unwrap_or(false)
}

fn unsupported(interface: &str, method: &acos_aidl::MethodInfo) -> ServiceError {
    ServiceError::UnsupportedAndroidApi {
        interface: interface.to_string(),
        code: method.code,
        method: method.name.clone(),
    }
}

fn binder_arg(args: &[Value], i: usize) -> Option<BinderId> {
    args.get(i).and_then(Value::as_binder)
}

fn int_arg(args: &[Value], i: usize) -> i32 {
    args.get(i).and_then(Value::as_int).unwrap_or(0)
}

/// `IInputMethodManager` handler.
pub struct ImeService {
    state: Arc<Mutex<ImeState>>,
}

impl ImeService {
    /// Wrap shared state.
    #[must_use]
    pub fn new(state: Arc<Mutex<ImeState>>) -> Self {
        Self { state }
    }
}

impl ServiceObject for ImeService {
    fn call(
        &mut self,
        interface: &str,
        method: &acos_aidl::MethodInfo,
        args: &[Value],
    ) -> Result<CallResult> {
        if interface != "com.android.internal.view.IInputMethodManager" {
            return Err(unsupported(interface, method));
        }
        let mut state = self.state.lock().unwrap();
        let reply = match method.name.as_str() {
            "addClient" => {
                state.add_client(binder_arg(args, 0), binder_arg(args, 1), int_arg(args, 2));
                Vec::new()
            }
            "startInputOrWindowGainedFocus" => {
                let client = binder_arg(args, 1);
                let sequence = int_arg(args, 0);
                let remote = binder_arg(args, 6);
                let (result, _method, _session) = state.start_input(client, sequence, remote);
                // `InputBindResult` is marshalled by `acos-runtime::ime_java`
                // from a real Java object; the typed description below is the
                // fallback the generic codec would write.
                vec![Value::TypedObject(crate::value::Parcelable::ordered(
                    "com.android.internal.inputmethod.InputBindResult",
                    vec![
                        Value::Int(result),
                        Value::Str8(None),
                        Value::Binder(None),
                        Value::Int(sequence),
                    ],
                ))]
            }
            "isImeTraceEnabled" => vec![Value::Bool(false)],
            "showSoftInput" => {
                let client = binder_arg(args, 0).unwrap_or(BinderId(0));
                let flags = int_arg(args, 3);
                vec![Value::Bool(state.show_soft_input(client, flags))]
            }
            "hideSoftInput" => {
                let client = binder_arg(args, 0).unwrap_or(BinderId(0));
                let flags = int_arg(args, 3);
                vec![Value::Bool(state.hide_soft_input(client, flags))]
            }
            "reportPerceptibleAsync" => Vec::new(),
            // `input method manager: remove the IME surface` reports whether a
            // surface was removed; the null IME has none.
            "removeImeSurfaceFromWindowAsync" => vec![Value::Bool(false)],
            "getImeTrackerService" => vec![Value::Binder(None)],
            "getInputMethodList" | "getEnabledInputMethodList" => {
                vec![Value::List(Some(Vec::new()))]
            }
            "getCurMethod" | "getLastInputMethod" => vec![Value::Str8(None)],
            // The null IME has no subtype; `InputMethodSubtype` is a real
            // framework parcelable the generic codec can express as `null`.
            "getCurrentInputMethodSubtype" | "getLastInputMethodSubtype" => {
                vec![Value::TypedObject(crate::value::Parcelable::Null)]
            }
            "isInputMethodShown" => vec![Value::Bool(false)],
            _ => return Err(unsupported(interface, method)),
        };
        Ok(CallResult {
            reply,
            deliveries: Vec::new(),
        })
    }

    fn drain_retained(&mut self) -> Vec<BinderId> {
        self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).refs.drain_retained()
    }

    fn drain_released(&mut self) -> Vec<BinderId> {
        self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).refs.drain_released()
    }

    fn release_all_refs(&mut self) -> Vec<BinderId> {
        self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).refs.release_all()
    }
}

/// `IInputMethodSession` handler (the null IME's session role).
pub struct ImeSessionService {
    state: Arc<Mutex<ImeState>>,
}

impl ImeSessionService {
    /// Wrap shared state.
    #[must_use]
    pub fn new(state: Arc<Mutex<ImeState>>) -> Self {
        Self { state }
    }
}

impl ServiceObject for ImeSessionService {
    fn call(
        &mut self,
        interface: &str,
        method: &acos_aidl::MethodInfo,
        _args: &[Value],
    ) -> Result<CallResult> {
        if interface != "com.android.internal.inputmethod.IInputMethodSession" {
            return Err(unsupported(interface, method));
        }
        let mut state = self.state.lock().unwrap();
        match method.name.as_str() {
            "updateSelection" => state.update_selection(),
            "viewClicked" => state.view_clicked(),
            // `appPrivateCommand`/`displayCompletions`/`updateCursor` are IME ->
            // app deliveries; with no IME they are accepted no-ops.
            "appPrivateCommand"
            | "displayCompletions"
            | "updateCursor"
            | "updateExtractedText"
            | "toggleSoftInput"
            | "finishInput" => {}
            _ => return Err(unsupported(interface, method)),
        }
        Ok(CallResult {
            reply: Vec::new(),
            deliveries: Vec::new(),
        })
    }
}

/// `IImeTracker` handler.
pub struct ImeTrackerService {
    state: Arc<Mutex<ImeState>>,
}

impl ImeTrackerService {
    /// Wrap shared state.
    #[must_use]
    pub fn new(state: Arc<Mutex<ImeState>>) -> Self {
        Self { state }
    }
}

impl ServiceObject for ImeTrackerService {
    fn call(
        &mut self,
        interface: &str,
        method: &acos_aidl::MethodInfo,
        args: &[Value],
    ) -> Result<CallResult> {
        if interface != "com.android.internal.inputmethod.IImeTracker" {
            return Err(unsupported(interface, method));
        }
        let token = args
            .first()
            .and_then(Value::as_parcelable)
            .and_then(|p| p.field(0))
            .and_then(Value::as_int)
            .map(i64::from)
            .unwrap_or(0);
        let mut state = self.state.lock().unwrap();
        match method.name.as_str() {
            "onRequestShow" => state.tracker_event(token, -1),
            "onRequestHide" => state.tracker_event(token, -2),
            "onShown" => state.tracker_event(token, -3),
            "onHidden" => state.tracker_event(token, -4),
            "onProgress" => {
                let phase = int_arg(args, 1);
                state.tracker_progress(token, phase);
            }
            "hasPendingCancellation" => {
                return Ok(CallResult {
                    reply: vec![Value::Bool(false)],
                    deliveries: Vec::new(),
                })
            }
            _ => return Err(unsupported(interface, method)),
        }
        Ok(CallResult {
            reply: Vec::new(),
            deliveries: Vec::new(),
        })
    }
}
