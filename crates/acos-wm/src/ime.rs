//! `svc.ime`: the IInputMethodManager / IInputMethodClient / IRemoteInputConnection
//! state machine, and the text-field side of typing.
//!
//! ACOS V1 does not run an external `InputMethodService`; instead this Rust
//! manager plays the IME service role in-process. It decodes the real
//! `startInputOrWindowGainedFocus` arguments (including `EditorInfo`) and
//! produces a real `InputBindResult`; the actual text edits are delivered
//! through [`InputConnection`], the trait the native layer connects to the
//! application's `InputConnection`/`RemoteInputConnectionImpl` (MEASURED:
//! `IRemoteInputConnection` ×15 in the interaction trace).

use std::collections::HashMap;
use std::sync::Arc;

use acos_parcel::binder::BinderId;
use acos_parcel::Parcel;

use crate::codec::{InputChannelParcel, ParcelCodec};
use crate::error::{Result, WmError};

/// `InputBindResult.SUCCESS`.
pub const BIND_SUCCESS: i32 = 0;
/// `InputBindResult.NO_IME` (the declared V1 divergence when nothing is bound).
pub const BIND_NO_IME: i32 = 1;
/// `InputBindResult.ALREADY_BOUND` (for a redundant `startInput`).
pub const BIND_ALREADY_BOUND: i32 = -1;

/// One `ImeTracker.Token`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImeToken {
    /// Monotonic token id.
    pub id: i64,
    /// The request reason string hash.
    pub reason: i64,
}

/// A `ImeTracker` progress entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImeProgress {
    /// The token.
    pub token: ImeToken,
    /// Progress code (`onProgress`).
    pub phase: i32,
}

/// The text payload of `getSurroundingText`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SurroundingText {
    /// The text.
    pub text: String,
    /// Selection start.
    pub selection_start: i32,
    /// Selection end.
    pub selection_end: i32,
    /// Offset of `text` from the start of the field.
    pub offset: i32,
}

/// The text-field side of the input method, implemented by the native layer on
/// top of the application's real `InputConnection`.
pub trait InputConnection: Send + Sync + std::fmt::Debug {
    /// `getTextBeforeCursor`.
    fn get_text_before_cursor(&self, length: i32, flags: i32) -> String;
    /// `getTextAfterCursor`.
    fn get_text_after_cursor(&self, length: i32, flags: i32) -> String;
    /// `getSurroundingText`.
    fn get_surrounding_text(&self, before: i32, after: i32, flags: i32) -> SurroundingText;
    /// `commitText`.
    fn commit_text(&self, text: &str, new_cursor: i32);
    /// `setComposingText`.
    fn set_composing_text(&self, text: &str, new_cursor: i32);
    /// `finishComposingText`.
    fn finish_composing_text(&self);
    /// `deleteSurroundingText`.
    fn delete_surrounding_text(&self, before: i32, after: i32);
    /// `setComposingRegion`.
    fn set_composing_region(&self, start: i32, end: i32);
    /// `clearMetaKeyStates`.
    fn clear_meta_key_states(&self, states: i32);
    /// `performEditorAction`.
    fn perform_editor_action(&self, action: i32);
}

/// A minimal in-memory text field, used by the tests and the agent's `type_text`.
#[derive(Debug, Default)]
pub struct StringInputConnection {
    text: parking_lot_like::Mutex<String>,
    selection: parking_lot_like::Mutex<(i32, i32)>,
}

/// A tiny `Mutex` newtype so the text field needs no external lock crate.
mod parking_lot_like {
    /// A `Mutex` alias documenting that we only need poison-tolerant access.
    pub type Mutex<T> = std::sync::Mutex<T>;
}

impl StringInputConnection {
    /// An empty field with the cursor at 0.
    #[must_use]
    pub fn new() -> Self {
        Self {
            text: std::sync::Mutex::new(String::new()),
            selection: std::sync::Mutex::new((0, 0)),
        }
    }

    /// The current text.
    #[must_use]
    pub fn text(&self) -> String {
        self.text
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

fn lock<T>(m: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl InputConnection for StringInputConnection {
    fn get_text_before_cursor(&self, length: i32, _flags: i32) -> String {
        let text = lock(&self.text);
        let (start, _) = *lock(&self.selection);
        let start = start.max(0).min(text.len() as i32) as usize;
        let from = if length < 0 {
            0
        } else {
            start.saturating_sub(length as usize)
        };
        text[from..start].to_string()
    }

    fn get_text_after_cursor(&self, length: i32, _flags: i32) -> String {
        let text = lock(&self.text);
        let (_, end) = *lock(&self.selection);
        let end = end.max(0).min(text.len() as i32) as usize;
        let to = if length < 0 {
            text.len()
        } else {
            (end + length as usize).min(text.len())
        };
        text[end..to].to_string()
    }

    fn get_surrounding_text(&self, before: i32, after: i32, _flags: i32) -> SurroundingText {
        let b = self.get_text_before_cursor(before, 0);
        let a = self.get_text_after_cursor(after, 0);
        let (start, end) = *lock(&self.selection);
        SurroundingText {
            offset: b.len() as i32,
            text: format!("{b}{a}"),
            selection_start: start,
            selection_end: end,
        }
    }

    fn commit_text(&self, text: &str, new_cursor: i32) {
        {
            let mut buf = lock(&self.text);
            let (start, end) = *lock(&self.selection);
            let (s, e) = (
                start.max(0).min(buf.len() as i32) as usize,
                end.max(0).min(buf.len() as i32) as usize,
            );
            buf.replace_range(s..e, text);
        }
        let mut sel = lock(&self.selection);
        let pos = sel.0 + text.len() as i32 + new_cursor;
        *sel = (pos, pos);
    }

    fn set_composing_text(&self, text: &str, _new_cursor: i32) {
        // Composing text replaces the selection like commitText but keeps the
        // composing region open; the simplified field just records the text.
        self.commit_text(text, 0);
    }

    fn finish_composing_text(&self) {}

    fn delete_surrounding_text(&self, before: i32, after: i32) {
        let mut buf = lock(&self.text);
        let (start, end) = *lock(&self.selection);
        let s = start.max(0).min(buf.len() as i32) as usize;
        let e = end.max(0).min(buf.len() as i32) as usize;
        let del_start = s.saturating_sub(before.max(0) as usize);
        let del_end = (e + after.max(0) as usize).min(buf.len());
        buf.replace_range(del_start..del_end, "");
    }

    fn set_composing_region(&self, _start: i32, _end: i32) {}

    fn clear_meta_key_states(&self, _states: i32) {}

    fn perform_editor_action(&self, _action: i32) {}
}

/// `EditorInfo` (the fields the IME manager needs; order from the DEX).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct EditorInfo {
    /// `inputType`.
    pub input_type: i32,
    /// `imeOptions`.
    pub ime_options: i32,
    /// `privateImeOptions`.
    pub private_ime_options: Option<String>,
    /// `internalImeOptions`.
    pub internal_ime_options: i32,
    /// `actionLabel`.
    pub action_label: Option<String>,
    /// `actionId`.
    pub action_id: i32,
    /// `initialSelStart`.
    pub initial_sel_start: i32,
    /// `initialSelEnd`.
    pub initial_sel_end: i32,
    /// `initialCapsMode`.
    pub initial_caps_mode: i32,
    /// `mInitialToolType`.
    pub initial_tool_type: i32,
    /// `hintText`.
    pub hint_text: Option<String>,
    /// `label`.
    pub label: Option<String>,
    /// `packageName`.
    pub package_name: Option<String>,
    /// `fieldId`.
    pub field_id: i32,
    /// `fieldName`.
    pub field_name: Option<String>,
    /// `mSupportedHandwritingGestureTypes`.
    pub supported_handwriting_gesture_types: i32,
    /// `mSupportedHandwritingGesturePreviewTypes`.
    pub supported_handwriting_gesture_preview_types: i32,
    /// `contentMimeTypes`.
    pub content_mime_types: Option<Vec<String>>,
}

impl ParcelCodec for EditorInfo {
    const NAME: &'static str = "android.view.inputmethod.EditorInfo";

    fn write_to_parcel(&self, p: &mut Parcel) -> Result<()> {
        p.write_int32(self.input_type);
        p.write_int32(self.ime_options);
        p.write_string16_opt(self.private_ime_options.as_deref())?;
        p.write_int32(self.internal_ime_options);
        write_char_sequence(p, self.action_label.as_deref())?;
        p.write_int32(self.action_id);
        p.write_int32(self.initial_sel_start);
        p.write_int32(self.initial_sel_end);
        p.write_int32(self.initial_caps_mode);
        p.write_int32(self.initial_tool_type);
        write_char_sequence(p, self.hint_text.as_deref())?;
        write_char_sequence(p, self.label.as_deref())?;
        p.write_string16_opt(self.package_name.as_deref())?;
        // autofillId: writeParcelable(null) => writeInt(0).
        p.write_int32(0);
        p.write_int32(self.field_id);
        p.write_string16_opt(self.field_name.as_deref())?;
        // extras: writeBundle(null) => writeInt(-1).
        p.write_int32(-1);
        p.write_int32(self.supported_handwriting_gesture_types);
        p.write_int32(self.supported_handwriting_gesture_preview_types);
        // mInitialSurroundingText present flag.
        p.write_bool(false);
        // hintLocales: LocaleList.getEmptyLocaleList().writeToParcel => String8("").
        p.write_string8("")?;
        // contentMimeTypes: null String[] => -1.
        match &self.content_mime_types {
            None => p.write_int32(-1),
            Some(v) => {
                p.write_int32(v.len() as i32);
                for s in v {
                    p.write_string16(s)?;
                }
            }
        }
        // targetInputMethodUser = null UserHandle => writeInt(-10000).
        p.write_int32(-10000);
        Ok(())
    }

    fn read_from_parcel(p: &mut Parcel) -> Result<Self> {
        let input_type = p.read_int32()?;
        let ime_options = p.read_int32()?;
        let private_ime_options = p.read_string16()?;
        let internal_ime_options = p.read_int32()?;
        let action_label = read_char_sequence(p)?;
        let action_id = p.read_int32()?;
        let initial_sel_start = p.read_int32()?;
        let initial_sel_end = p.read_int32()?;
        let initial_caps_mode = p.read_int32()?;
        let initial_tool_type = p.read_int32()?;
        let hint_text = read_char_sequence(p)?;
        let label = read_char_sequence(p)?;
        let package_name = p.read_string16()?;
        let _autofill_marker = p.read_int32()?;
        let field_id = p.read_int32()?;
        let field_name = p.read_string16()?;
        let _extras = p.read_int32()?;
        let supported_handwriting_gesture_types = p.read_int32()?;
        let supported_handwriting_gesture_preview_types = p.read_int32()?;
        let _surrounding_present = p.read_bool()?;
        let _hint_locales = p.read_string8()?;
        let mime_n = p.read_int32()?;
        let content_mime_types = if mime_n < 0 {
            None
        } else {
            let mut v = Vec::with_capacity(mime_n as usize);
            for _ in 0..mime_n {
                v.push(p.read_string16()?.unwrap_or_default());
            }
            Some(v)
        };
        let _user = p.read_int32()?;
        Ok(EditorInfo {
            input_type,
            ime_options,
            private_ime_options,
            internal_ime_options,
            action_label,
            action_id,
            initial_sel_start,
            initial_sel_end,
            initial_caps_mode,
            initial_tool_type,
            hint_text,
            label,
            package_name,
            field_id,
            field_name,
            supported_handwriting_gesture_types,
            supported_handwriting_gesture_preview_types,
            content_mime_types,
        })
    }
}

/// `TextUtils.writeToParcel` for a non-`Spanned` sequence.
fn write_char_sequence(p: &mut Parcel, v: Option<&str>) -> Result<()> {
    p.write_int32(1);
    p.write_string8_opt(v)?;
    Ok(())
}

fn read_char_sequence(p: &mut Parcel) -> Result<Option<String>> {
    if p.read_int32()? == 0 {
        return Err(WmError::Unsupported(
            "Spanned CharSequence in EditorInfo".into(),
        ));
    }
    p.read_string8()
        .map_err(|e| WmError::Unsupported(format!("EditorInfo CharSequence: {e}")))
}

/// `com.android.internal.inputmethod.InputBindResult`.
#[derive(Debug, Clone, PartialEq)]
pub struct InputBindResult {
    /// `result` code.
    pub result: i32,
    /// `method` (`IInputMethodSession` binder).
    pub method: Option<BinderId>,
    /// `accessibilitySessions` (`SparseArray<IInputMethodSession>`).
    pub accessibility_sessions: Option<Vec<(i32, BinderId)>>,
    /// `channel`.
    pub channel: Option<InputChannelParcel>,
    /// `id` (the session id string).
    pub id: Option<String>,
    /// `sequence`.
    pub sequence: i32,
    /// `mVirtualDisplayToScreenMatrixValues`.
    pub matrix_values: Option<Vec<f32>>,
    /// `isInputMethodSuppressingSpellChecker`.
    pub suppressing_spell_checker: bool,
}

impl InputBindResult {
    /// The V1 "no IME bound" result.
    #[must_use]
    pub const fn no_ime(sequence: i32) -> Self {
        Self {
            result: BIND_NO_IME,
            method: None,
            accessibility_sessions: None,
            channel: None,
            id: None,
            sequence,
            matrix_values: None,
            suppressing_spell_checker: false,
        }
    }

    /// A successful bind to `session`.
    #[must_use]
    pub fn success(session: BinderId, id: impl Into<String>, sequence: i32) -> Self {
        Self {
            result: BIND_SUCCESS,
            method: Some(session),
            accessibility_sessions: Some(Vec::new()),
            channel: None,
            id: Some(id.into()),
            sequence,
            matrix_values: None,
            suppressing_spell_checker: false,
        }
    }
}

impl ParcelCodec for InputBindResult {
    const NAME: &'static str = "com.android.internal.inputmethod.InputBindResult";

    fn write_to_parcel(&self, p: &mut Parcel) -> Result<()> {
        p.write_int32(self.result);
        // writeStrongInterface(session) is a strong binder in the Parcel.
        p.write_strong_binder_opt(self.method);
        match &self.accessibility_sessions {
            None => p.write_int32(-1),
            Some(v) => {
                p.write_int32(v.len() as i32);
                for (key, binder) in v {
                    p.write_int32(*key);
                    p.write_strong_binder_opt(Some(*binder));
                }
            }
        }
        match &self.channel {
            None => p.write_int32(0),
            Some(c) => {
                p.write_int32(1);
                c.write_to_parcel(p)?;
            }
        }
        p.write_string16_opt(self.id.as_deref())?;
        p.write_int32(self.sequence);
        p.write_float_array(self.matrix_values.as_deref())?;
        p.write_bool(self.suppressing_spell_checker);
        Ok(())
    }

    fn read_from_parcel(p: &mut Parcel) -> Result<Self> {
        let result = p.read_int32()?;
        let method = p.read_strong_binder()?;
        let n = p.read_int32()?;
        let accessibility_sessions = if n < 0 {
            None
        } else {
            let mut v = Vec::with_capacity(n as usize);
            for _ in 0..n {
                let key = p.read_int32()?;
                let binder = p
                    .read_strong_binder()?
                    .ok_or_else(|| WmError::Codec {
                        what: Self::NAME,
                        reason: "null accessibility session binder".into(),
                    })?;
                v.push((key, binder));
            }
            Some(v)
        };
        let channel = if p.read_int32()? != 0 {
            Some(InputChannelParcel::read_from_parcel(p)?)
        } else {
            None
        };
        let id = p.read_string16()?;
        let sequence = p.read_int32()?;
        let matrix_values = p.read_float_array()?;
        let suppressing_spell_checker = p.read_bool()?;
        Ok(InputBindResult {
            result,
            method,
            accessibility_sessions,
            channel,
            id,
            sequence,
            matrix_values,
            suppressing_spell_checker,
        })
    }
}

/// A registered IME client (`IInputMethodClient`).
#[derive(Debug, Clone, PartialEq)]
pub struct ImeClient {
    /// The client binder.
    pub id: BinderId,
    /// The app's `IRemoteInputConnection`.
    pub remote_connection: Option<BinderId>,
    /// User id.
    pub user_id: i32,
    /// Whether the client is active.
    pub active: bool,
    /// The bound session binder, once `startInput` succeeds.
    pub session: Option<BinderId>,
}

/// An outgoing delivery to an IME client.
#[derive(Debug, Clone, PartialEq)]
pub enum ImeDelivery {
    /// `IInputMethodClient.setActive(boolean, boolean)`.
    SetActive {
        /// Active.
        active: bool,
        /// `needFocus`.
        need_focus: bool,
    },
    /// `IInputMethodClient.onBindMethod(InputBindResult)`.
    OnBindMethod(Box<InputBindResult>),
    /// `IInputMethodClient.onUnbindMethod(int, int)`.
    OnUnbindMethod {
        /// `sequence`.
        sequence: i32,
        /// `reason`.
        reason: i32,
    },
    /// `IInputMethodClient.reportFullscreenMode(boolean)`.
    ReportFullscreenMode(bool),
}

/// The IME service state machine.
#[derive(Debug)]
pub struct ImeManager {
    clients: HashMap<BinderId, ImeClient>,
    connections: HashMap<BinderId, Arc<dyn InputConnection>>,
    /// Outgoing client deliveries in order.
    deliveries: Vec<(BinderId, ImeDelivery)>,
    /// Next synthetic session binder.
    next_session: u64,
    /// `ImeTracker` state.
    next_token: i64,
    /// `onProgress` records.
    pub progress: Vec<ImeProgress>,
    /// `onRequestShow` / `onRequestHide` records.
    pub show_requests: Vec<ImeToken>,
    /// `onRequestHide` records.
    pub hide_requests: Vec<ImeToken>,
    /// `onShown` / `onHidden`.
    pub shown: Vec<ImeToken>,
    /// `onHidden`.
    pub hidden: Vec<ImeToken>,
    /// `IInputMethodSession.updateSelection` calls.
    pub selection_updates: u32,
    /// `IInputMethodSession.viewClicked` calls.
    pub view_clicks: u32,
    /// Whether the IME surface is currently requested visible.
    pub ime_visible: bool,
}

impl Default for ImeManager {
    fn default() -> Self {
        Self::new()
    }
}

impl ImeManager {
    /// An empty IME manager.
    #[must_use]
    pub fn new() -> Self {
        Self {
            clients: HashMap::new(),
            connections: HashMap::new(),
            deliveries: Vec::new(),
            next_session: 0x1_3e00,
            next_token: 1,
            progress: Vec::new(),
            show_requests: Vec::new(),
            hide_requests: Vec::new(),
            shown: Vec::new(),
            hidden: Vec::new(),
            selection_updates: 0,
            view_clicks: 0,
            ime_visible: false,
        }
    }

    /// Register the native layer's implementation of a client's
    /// `IRemoteInputConnection`.
    pub fn register_connection(&mut self, connection: BinderId, cb: Arc<dyn InputConnection>) {
        self.connections.insert(connection, cb);
    }

    /// The number of registered connections.
    #[must_use]
    pub fn connection_count(&self) -> usize {
        self.connections.len()
    }

    /// `IInputMethodManager.addClient`.
    pub fn add_client(
        &mut self,
        client: BinderId,
        remote_connection: Option<BinderId>,
        user_id: i32,
    ) -> Result<()> {
        self.clients.insert(
            client,
            ImeClient {
                id: client,
                remote_connection,
                user_id,
                active: false,
                session: None,
            },
        );
        Ok(())
    }

    /// `IInputMethodManager.startInputOrWindowGainedFocus`.
    pub fn start_input_or_window_gained_focus(
        &mut self,
        client: BinderId,
        editor_info: &EditorInfo,
        remote_connection: Option<BinderId>,
        sequence: i32,
    ) -> Result<InputBindResult> {
        let entry = self
            .clients
            .entry(client)
            .or_insert_with(|| ImeClient {
                id: client,
                remote_connection,
                user_id: 0,
                active: false,
                session: None,
            });
        if let Some(rc) = remote_connection {
            entry.remote_connection = Some(rc);
        }
        let connection_id = entry.remote_connection;
        let has_connection = connection_id
            .map(|c| self.connections.contains_key(&c))
            .unwrap_or(false);
        if !has_connection {
            // Declared V1 divergence: no bound IME for this field.
            let result = InputBindResult::no_ime(sequence);
            self.deliveries
                .push((client, ImeDelivery::OnBindMethod(Box::new(result.clone()))));
            return Ok(result);
        }
        let session = BinderId(self.next_session);
        self.next_session += 1;
        entry.active = true;
        entry.session = Some(session);
        let result = InputBindResult::success(
            session,
            format!("acos-ime-{}", sequence),
            sequence,
        );
        self.deliveries.push((
            client,
            ImeDelivery::SetActive {
                active: true,
                need_focus: false,
            },
        ));
        self.deliveries
            .push((client, ImeDelivery::ReportFullscreenMode(false)));
        self.deliveries
            .push((client, ImeDelivery::OnBindMethod(Box::new(result.clone()))));
        let _ = editor_info;
        Ok(result)
    }

    /// `IInputMethodManager.showSoftInput`.
    pub fn show_soft_input(&mut self, client: BinderId, _token: Option<ImeToken>) -> Result<bool> {
        let shown = self
            .clients
            .get(&client)
            .and_then(|c| c.remote_connection)
            .is_some_and(|c| self.connections.contains_key(&c));
        self.ime_visible = shown;
        Ok(shown)
    }

    /// `IInputMethodManager.hideSoftInput`.
    pub fn hide_soft_input(&mut self, _client: BinderId, _token: Option<ImeToken>) -> Result<bool> {
        let was = self.ime_visible;
        self.ime_visible = false;
        Ok(was)
    }

    /// `IInputMethodManager.isImeTraceEnabled`.
    #[must_use]
    pub const fn is_ime_trace_enabled(&self) -> bool {
        false
    }

    /// `IInputMethodManager.reportPerceptibleAsync` (oneway, recorded).
    pub fn report_perceptible_async(&mut self) {}

    /// `IInputMethodManager.removeImeSurfaceFromWindowAsync` (oneway).
    pub fn remove_ime_surface_from_window_async(&mut self) {
        self.ime_visible = false;
    }

    /// `IImeTracker.onRequestShow`.
    pub fn tracker_on_request_show(
        &mut self,
        reason: &str,
        _types: i32,
        _flags: i32,
    ) -> ImeToken {
        let token = ImeToken {
            id: self.next_token,
            reason: reason.len() as i64,
        };
        self.next_token += 1;
        self.show_requests.push(token);
        self.progress.push(ImeProgress {
            token,
            phase: 0,
        });
        token
    }

    /// `IImeTracker.onRequestHide`.
    pub fn tracker_on_request_hide(
        &mut self,
        reason: &str,
        _types: i32,
        _flags: i32,
    ) -> ImeToken {
        let token = ImeToken {
            id: self.next_token,
            reason: reason.len() as i64,
        };
        self.next_token += 1;
        self.hide_requests.push(token);
        token
    }

    /// `IImeTracker.onProgress`.
    pub fn tracker_on_progress(&mut self, token: ImeToken, phase: i32) {
        self.progress.push(ImeProgress { token, phase });
    }

    /// `IImeTracker.onShown`.
    pub fn tracker_on_shown(&mut self, token: ImeToken) {
        self.shown.push(token);
    }

    /// `IImeTracker.onHidden`.
    pub fn tracker_on_hidden(&mut self, token: ImeToken) {
        self.hidden.push(token);
    }

    /// `IInputMethodSession.updateSelection`.
    pub fn update_selection(
        &mut self,
        _old_start: i32,
        _old_end: i32,
        _new_start: i32,
        _new_end: i32,
        _candidates_start: i32,
        _candidates_end: i32,
    ) {
        self.selection_updates += 1;
    }

    /// `IInputMethodSession.viewClicked`.
    pub fn view_clicked(&mut self, _focus_changed: bool) {
        self.view_clicks += 1;
    }

    /// Drain outgoing client deliveries.
    pub fn take_deliveries(&mut self) -> Vec<(BinderId, ImeDelivery)> {
        std::mem::take(&mut self.deliveries)
    }

    fn connection(&self, client: BinderId) -> Result<Arc<dyn InputConnection>> {
        let c = self
            .clients
            .get(&client)
            .ok_or_else(|| WmError::ImeNotBound(format!("client {}", client.0)))?;
        let rc = c
            .remote_connection
            .ok_or_else(|| WmError::ImeNotBound(format!("client {} has no connection", client.0)))?;
        self.connections
            .get(&rc)
            .cloned()
            .ok_or_else(|| WmError::ImeNotBound(format!("connection {}", rc.0)))
    }

    /// `IRemoteInputConnection.commitText`.
    pub fn commit_text(&mut self, client: BinderId, text: &str, new_cursor: i32) -> Result<()> {
        self.connection(client)?.commit_text(text, new_cursor);
        Ok(())
    }

    /// `IRemoteInputConnection.setComposingText`.
    pub fn set_composing_text(
        &mut self,
        client: BinderId,
        text: &str,
        new_cursor: i32,
    ) -> Result<()> {
        self.connection(client)?.set_composing_text(text, new_cursor);
        Ok(())
    }

    /// `IRemoteInputConnection.finishComposingText`.
    pub fn finish_composing_text(&mut self, client: BinderId) -> Result<()> {
        self.connection(client)?.finish_composing_text();
        Ok(())
    }

    /// `IRemoteInputConnection.deleteSurroundingText`.
    pub fn delete_surrounding_text(&mut self, client: BinderId, before: i32, after: i32) -> Result<()> {
        self.connection(client)?
            .delete_surrounding_text(before, after);
        Ok(())
    }

    /// `IRemoteInputConnection.getTextBeforeCursor`.
    pub fn get_text_before_cursor(&self, client: BinderId, length: i32, flags: i32) -> Result<String> {
        Ok(self.connection(client)?.get_text_before_cursor(length, flags))
    }

    /// `IRemoteInputConnection.getSurroundingText`.
    pub fn get_surrounding_text(
        &self,
        client: BinderId,
        before: i32,
        after: i32,
        flags: i32,
    ) -> Result<SurroundingText> {
        Ok(self
            .connection(client)?
            .get_surrounding_text(before, after, flags))
    }

    /// `IRemoteInputConnection.setComposingRegion`.
    pub fn set_composing_region(&mut self, client: BinderId, start: i32, end: i32) -> Result<()> {
        self.connection(client)?.set_composing_region(start, end);
        Ok(())
    }

    /// `IRemoteInputConnection.clearMetaKeyStates`.
    pub fn clear_meta_key_states(&mut self, client: BinderId, states: i32) -> Result<()> {
        self.connection(client)?.clear_meta_key_states(states);
        Ok(())
    }

    /// `IRemoteInputConnection.performEditorAction`.
    pub fn perform_editor_action(&mut self, client: BinderId, action: i32) -> Result<()> {
        self.connection(client)?.perform_editor_action(action);
        Ok(())
    }

    /// Decode a `startInputOrWindowGainedFocus` request parcel body (after the
    /// interface token) into the arguments the replay needs.
    pub fn decode_start_input(p: &mut Parcel) -> Result<(i32, BinderId, EditorInfo, Option<BinderId>)> {
        let _kind = p.read_int32()?;
        let client = p
            .read_strong_binder()?
            .ok_or_else(|| WmError::ImeNotBound("startInput without a client".into()))?;
        let _window = p.read_strong_binder()?;
        let _missing_method = p.read_int32()?;
        let _user_id = p.read_int32()?;
        let _display_id = p.read_int32()?;
        let editor = EditorInfo::read_from_parcel(p)?;
        let connection = p.read_strong_binder()?;
        let _accessibility_connection = p.read_strong_binder()?;
        let _flags = p.read_int32()?;
        let _unused = p.read_int32()?;
        // ImeOnBackInvokedDispatcher: typed object; null marker.
        let _dispatcher = p.read_int32()?;
        Ok((_kind, client, editor, connection))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn client() -> BinderId {
        BinderId(0x1234)
    }

    #[test]
    fn typing_round_trip_through_input_connection() {
        let mut ime = ImeManager::new();
        let conn = BinderId(0xabcd);
        let field = Arc::new(StringInputConnection::new());
        ime.register_connection(conn, field.clone());
        ime.add_client(client(), Some(conn), 0).unwrap();
        let info = EditorInfo {
            input_type: 1,
            package_name: Some("dev.acos.fixtures.helloviews".into()),
            ..EditorInfo::default()
        };
        let bind = ime
            .start_input_or_window_gained_focus(client(), &info, Some(conn), 1)
            .unwrap();
        assert_eq!(bind.result, BIND_SUCCESS);
        assert!(bind.method.is_some());
        ime.commit_text(client(), "World", 1).unwrap();
        assert_eq!(field.text(), "World");
        ime.commit_text(client(), "!", 1).unwrap();
        assert_eq!(field.text(), "World!");
        assert_eq!(ime.get_text_before_cursor(client(), 3, 0).unwrap(), "ld!");
        assert_eq!(ime.selection_updates, 0);
        ime.update_selection(0, 0, 5, 5, 0, 0);
        assert_eq!(ime.selection_updates, 1);
    }

    #[test]
    fn start_input_without_connection_is_no_ime() {
        let mut ime = ImeManager::new();
        ime.add_client(client(), None, 0).unwrap();
        let bind = ime
            .start_input_or_window_gained_focus(client(), &EditorInfo::default(), None, 7)
            .unwrap();
        assert_eq!(bind.result, BIND_NO_IME);
        assert!(bind.method.is_none());
        assert!(matches!(
            ime.commit_text(client(), "x", 0),
            Err(WmError::ImeNotBound(_))
        ));
    }

    #[test]
    fn editor_info_round_trips() {
        let info = EditorInfo {
            input_type: 0x0000_0001,
            ime_options: 0x0000_0006,
            package_name: Some("dev.acos.fixtures.helloviews".into()),
            hint_text: Some("Enter your name".into()),
            initial_sel_start: 0,
            initial_sel_end: 0,
            field_id: 42,
            content_mime_types: Some(vec!["text/plain".into()]),
            ..EditorInfo::default()
        };
        let mut p = Parcel::new();
        info.write_to_parcel(&mut p).unwrap();
        p.set_data_position(0);
        assert_eq!(EditorInfo::read_from_parcel(&mut p).unwrap(), info);
        p.enforce_no_data_avail().unwrap();
    }

    #[test]
    fn input_bind_result_round_trips() {
        let r = InputBindResult::success(BinderId(0x99), "acos-ime-1", 3);
        let mut p = Parcel::new();
        r.write_to_parcel(&mut p).unwrap();
        p.set_data_position(0);
        assert_eq!(InputBindResult::read_from_parcel(&mut p).unwrap(), r);
        p.enforce_no_data_avail().unwrap();
    }

    #[test]
    fn tracker_tokens_are_monotonic() {
        let mut ime = ImeManager::new();
        let t1 = ime.tracker_on_request_show("tap", 0, 0);
        let t2 = ime.tracker_on_request_hide("back", 0, 0);
        assert!(t2.id > t1.id);
        for _ in 0..22 {
            ime.tracker_on_progress(t1, 1);
        }
        assert_eq!(ime.progress.len(), 23);
    }

    #[test]
    fn show_hide_are_honest() {
        let mut ime = ImeManager::new();
        let conn = BinderId(5);
        ime.register_connection(conn, Arc::new(StringInputConnection::new()));
        ime.add_client(client(), Some(conn), 0).unwrap();
        assert!(ime.show_soft_input(client(), None).unwrap());
        assert!(ime.hide_soft_input(client(), None).unwrap());
        assert!(!ime.ime_visible);
    }
}
