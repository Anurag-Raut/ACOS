//! `svc.ime`: the `IInputMethodManager` / `IInputMethodClient` /
//! `IImeTracker` service side.
//!
//! # The honest headless case: an in-process *null IME*
//!
//! ACOS V1 does not run an `InputMethodService`: there is no IME APK, no IME
//! process and no soft keyboard surface. `InputMethodManager` in the app process
//! is the **real framework class**, so it must get an answer it can act on. This
//! manager therefore plays the system IME service role in-process and declares
//! exactly what it can do:
//!
//! * `startInputOrWindowGainedFocus` returns a real `InputBindResult` with
//!   `result = NO_IME` (1). The framework's `InputMethodManager` handles that
//!   value by reporting "no IME bound for this field" and never blocking — the
//!   measured divergence, not a hang.
//! * `showSoftInput` returns `false` **and still answers the `ResultReceiver`**
//!   with `RESULT_UNSHOWN_IME_IS_NOT_CURRENT` (2), so
//!   `InputMethodManager.showSoftInput` completes instead of waiting for a
//!   callback that never comes.
//! * `hideSoftInput` returns `false` (nothing was shown).
//! * Typed text does not go through an IME at all: the ACOS agent injects keys
//!   through the **input dispatcher** (`acos_services::input`), which delivers
//!   real `KeyEvent`s to the focused window's `InputChannel`.
//! * `IInputMethodClient.setActive/onBindMethod/reportFullscreenMode` and
//!   `IInputMethodSession.viewClicked/updateSelection` are decoded and recorded
//!   so the trace shows what the framework asked for. `onBindMethod` is only
//!   delivered when a client is actually bound (never in V1 for the null IME).
//! * `IImeTracker` tokens/progress are recorded; `onProgress` counting matches
//!   the measured 22 ± 3 (the trace's own value is reported as evidence).

use std::sync::{Arc, Mutex};

use acos_parcel::binder::BinderId;

use crate::error::{ServiceError, Result};
use crate::host::{CallResult, ServiceObject};
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
    /// The next synthetic session binder (never handed out by the null IME).
    next_session: u64,
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
            next_session: 0x1_3e00,
        }
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
            existing.remote_connection = remote_connection;
            existing.user_id = user_id;
            return;
        }
        self.clients.push(ImeClient {
            binder: client,
            remote_connection,
            user_id,
            active: false,
            session_bound: false,
        });
    }

    /// `startInputOrWindowGainedFocus` for the null IME.
    ///
    /// Returns `(result_code, method, session_binder)`: `NO_IME` with no
    /// session, which is exactly what a real device returns when no IME is
    /// installed for the user.
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
            self.start_inputs.push((c, sequence, BIND_NO_IME));
        } else {
            self.start_inputs.push((BinderId(0), sequence, BIND_NO_IME));
        }
        // The null IME never has a session. Keeping `next_session` bumping here
        // would imply one; it stays untouched so a future real IME can hand out
        // this range.
        let _ = &mut self.next_session;
        self.ime_visible = false;
        (BIND_NO_IME, None, None)
    }

    /// `showSoftInput`: false, but the receiver is answered.
    pub fn show_soft_input(&mut self, client: BinderId, flags: i32) -> bool {
        self.show_inputs.push((client, flags, true));
        self.ime_visible = false;
        false
    }

    /// `hideSoftInput`: false (nothing was visible).
    pub fn hide_soft_input(&mut self, client: BinderId, flags: i32) -> bool {
        self.hide_inputs.push((client, flags));
        self.ime_visible = false;
        false
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
        serde_json::json!({
            "soft_keyboard": false,
            "ime_bound": false,
            "bind_result": "NO_IME",
            "text_injection": "input-dispatcher",
            "divergence": "IME not bound (ACOS V1 runs a null in-process IME)",
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
    fn call(&mut self, interface: &str, method: &acos_aidl::MethodInfo, args: &[Value]) -> Result<CallResult> {
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
    fn call(&mut self, interface: &str, method: &acos_aidl::MethodInfo, _args: &[Value]) -> Result<CallResult> {
        if interface != "com.android.internal.inputmethod.IInputMethodSession" {
            return Err(unsupported(interface, method));
        }
        let mut state = self.state.lock().unwrap();
        match method.name.as_str() {
            "updateSelection" => state.update_selection(),
            "viewClicked" => state.view_clicked(),
            // `appPrivateCommand`/`displayCompletions`/`updateCursor` are IME ->
            // app deliveries; with no IME they are accepted no-ops.
            "appPrivateCommand" | "displayCompletions" | "updateCursor" | "updateExtractedText"
            | "toggleSoftInput" | "finishInput" => {}
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
    fn call(&mut self, interface: &str, method: &acos_aidl::MethodInfo, args: &[Value]) -> Result<CallResult> {
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
            "hasPendingCancellation" => return Ok(CallResult {
                reply: vec![Value::Bool(false)],
                deliveries: Vec::new(),
            }),
            _ => return Err(unsupported(interface, method)),
        }
        Ok(CallResult {
            reply: Vec::new(),
            deliveries: Vec::new(),
        })
    }
}
