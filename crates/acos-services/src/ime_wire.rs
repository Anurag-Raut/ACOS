//! Image-exact marshalling for the `IInputMethodManager` requests the real
//! `ViewRootImpl` focus path makes (W13).
//!
//! The generic `WireOp` codec can express the scalar/binder arguments of
//! `com.android.internal.view.IInputMethodManager`, but not the two framework
//! parcelables on this path: the request carries a real
//! `android.view.inputmethod.EditorInfo` (`writeTypedObject`, presence byte
//! then `writeToParcel`) and the reply is a real
//! `com.android.internal.inputmethod.InputBindResult`. `EditorInfo` has
//! conditional payloads (`AutofillId`, `SurroundingText`, `Bundle`,
//! `LocaleList`), which is why this module reads it field by field from the
//! **image's own** `writeToParcel` and refuses anything it has not verified.
//!
//! The layouts below were read out of the API-34 image's `framework.jar`
//! (`acos-dex dexdump`, `.cache/boot/android-34/dex/framework`, which is
//! byte-identical to `.cache/art/sysroot/system/framework/framework.jar`) and
//! pinned by `tests/ime_wire.rs` against a request captured from a live launch
//! plus a negative case for every truncation. The tag differs from the image in
//! at least one place: `ImeTracker$Token` is `writeStrongBinder` + `writeString8`
//! here, not the tag's `[int, string16]`.
//!
//! # Why a `raw_transact` service and not a `FieldOp` spec
//!
//! The IME manager is registered as a **dynamic** node (`registry.rs`) so the
//! host calls [`ServiceObject::raw_transact`], exactly like `IWindowSession`.
//! The other `IInputMethodManager` methods (`addClient`, `showSoftInput`, …)
//! keep the generic `Value` codec: this module delegates them to
//! [`crate::ime::ImeService`] after `codec::read_args` and writes the reply with
//! `codec::write_reply`. Only the three methods whose wire format the generic
//! codec cannot express are hand-decoded here.
//!
//! # The null IME answer
//!
//! ACOS runs no `InputMethodService`. `startInputOrWindowGainedFocus` therefore
//! returns the framework's own `InputBindResult.NO_IME` (`result = 6`, verified
//! in the image's `InputBindResult.<clinit>`), with no session, no channel and
//! the request's sequence echoed. That is what a device with no IME installed
//! returns; the framework then proceeds with the window focused, so key events
//! reach the focused view. It is a declared divergence, not a fake success.

use std::sync::{Arc, Mutex};

use acos_aidl::WireOp;
use acos_parcel::{BinderId, Parcel};

use crate::codec;
use crate::error::{ServiceError, Result};
use crate::exception::write_no_exception;
use crate::host::{CallResult, ServiceObject};
use crate::ime::{ImeService, ImeState};

/// The `IInputMethodManager` AIDL descriptor.
pub const IME_DESCRIPTOR: &str = "com.android.internal.view.IInputMethodManager";
/// `IInputMethodManager.startInputOrWindowGainedFocus`.
pub const METHOD_START_INPUT: &str = "startInputOrWindowGainedFocus";
/// `android.view.inputmethod.EditorInfo`.
pub const EDITOR_INFO_CLASS: &str = "android.view.inputmethod.EditorInfo";
/// `com.android.internal.inputmethod.InputBindResult`.
pub const INPUT_BIND_RESULT_CLASS: &str = "com.android.internal.inputmethod.InputBindResult";
/// `android.window.ImeOnBackInvokedDispatcher`.
pub const IME_ON_BACK_INVOKED_CLASS: &str = "android.window.ImeOnBackInvokedDispatcher";
/// `android.view.inputmethod.ImeTracker$Token`.
pub const IME_TRACKER_TOKEN_CLASS: &str = "android.view.inputmethod.ImeTracker$Token";
/// `android.os.ResultReceiver`.
pub const RESULT_RECEIVER_CLASS: &str = "android.os.ResultReceiver";

/// One `EditorInfo`'s wire fields that this module decodes.
///
/// Only the fields the null IME needs are kept; `EditorInfo`'s own
/// `writeToParcel` order is still consumed in full so the request parcel ends
/// exactly where the framework left it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EditorInfo {
    /// `inputType`.
    pub input_type: i32,
    /// `imeOptions`.
    pub ime_options: i32,
    /// `privateImeOptions`.
    pub private_ime_options: Option<String>,
    /// `internalImeOptions`.
    pub internal_ime_options: i32,
    /// `actionLabel` (only the non-`Spanned` form is on the V1 path).
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
    /// `autofillId` presence and `mViewId` (the payload is consumed in full).
    pub autofill_view_id: Option<i32>,
    /// `fieldId`.
    pub field_id: i32,
    /// `fieldName`.
    pub field_name: Option<String>,
    /// Whether `extras` was a non-null `Bundle` (its payload is consumed).
    pub has_extras: bool,
    /// `mSupportedHandwritingGestureTypes`.
    pub supported_handwriting_gesture_types: i32,
    /// `mSupportedHandwritingGesturePreviewTypes`.
    pub supported_handwriting_gesture_preview_types: i32,
    /// `mInitialSurroundingText` presence (its payload is consumed).
    pub has_initial_surrounding_text: bool,
    /// `hintLocales` (`LocaleList.mStringRepresentation`, `None` for null).
    pub hint_locales: Option<String>,
    /// `contentMimeTypes` (`None` for a null array).
    pub content_mime_types: Option<Vec<Option<String>>>,
    /// `targetInputMethodUser` identifier (`None` for null / `-10000`).
    pub target_input_method_user: Option<i32>,
}

/// `ImeTracker$Token` payload (the image form).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ImeTrackerToken {
    /// `mBinder`.
    pub binder: Option<BinderId>,
    /// `mTag`.
    pub tag: Option<String>,
}

/// The `InputBindResult` this null IME returns.
#[derive(Debug, Clone, PartialEq)]
pub struct InputBindResult {
    /// `result` (`6` = `NO_IME` in the image).
    pub result: i32,
    /// `method` (`IInputMethodSession`, always null here).
    pub method: Option<BinderId>,
    /// `accessibilitySessions` (`-1` = null).
    pub accessibility_sessions: Option<Vec<(i32, BinderId)>>,
    /// `channel` (`None` = null).
    pub channel: Option<()>,
    /// `id`.
    pub id: Option<String>,
    /// `sequence` (the request's).
    pub sequence: i32,
    /// `mVirtualDisplayToScreenMatrixValues`.
    pub matrix_values: Option<Vec<f32>>,
    /// `isInputMethodSuppressingSpellChecker`.
    pub suppressing_spell_checker: bool,
}

impl InputBindResult {
    /// The null-IME result: `NO_IME`, no session, the request's sequence.
    #[must_use]
    pub const fn no_ime(sequence: i32) -> Self {
        Self {
            result: crate::ime::BIND_NO_IME,
            method: None,
            accessibility_sessions: None,
            channel: None,
            id: None,
            sequence,
            matrix_values: None,
            suppressing_spell_checker: false,
        }
    }

    /// Write the payload exactly as the image's `writeToParcel` does (no
    /// `writeTypedObject` presence marker; the caller writes that).
    pub fn write_payload(&self, p: &mut Parcel) -> Result<()> {
        p.write_int32(self.result);
        // `writeStrongInterface`.
        p.write_strong_binder_opt(self.method);
        match &self.accessibility_sessions {
            None => p.write_int32(-1),
            Some(v) => {
                p.write_int32(i32::try_from(v.len()).map_err(|_| bad("IME: too many sessions"))?);
                for (key, binder) in v {
                    p.write_int32(*key);
                    p.write_strong_binder_opt(Some(*binder));
                }
            }
        }
        // `channel == null ? writeInt(0) : writeInt(1) + InputChannel.writeToParcel`.
        match &self.channel {
            None => p.write_int32(0),
            Some(()) => {
                return Err(ServiceError::UnsupportedParcelable {
                    class: "android.view.InputChannel (bind result channel)".into(),
                })
            }
        }
        p.write_string16_opt(self.id.as_deref())?;
        p.write_int32(self.sequence);
        p.write_float_array(self.matrix_values.as_deref())?;
        p.write_bool(self.suppressing_spell_checker);
        Ok(())
    }
}

fn bad(detail: impl Into<String>) -> ServiceError {
    ServiceError::BadRequest {
        interface: IME_DESCRIPTOR.into(),
        method: METHOD_START_INPUT.into(),
        detail: detail.into(),
    }
}

fn unsupported(class: &str) -> ServiceError {
    ServiceError::UnsupportedParcelable {
        class: class.to_string(),
    }
}

/// `TextUtils.writeToParcel`: kind `1` is a plain `CharSequence`
/// (`writeString8`), kind `0` is a `Spanned` (text, span records, terminator).
///
/// A `Spanned` is on the real focus path: `EditorInfo.mInitialSurroundingText`
/// carries the editable, which is a `SpannableStringBuilder`. The span payloads
/// below are the image's own `writeToParcelInternal` layouts (ids from each
/// class's `getSpanTypeIdInternal`):
///
/// | id | class | payload |
/// |---:|---|---|
/// | 1 | `AlignmentSpan$Standard` | string |
/// | 2 | `ForegroundColorSpan` | int |
/// | 3 | `RelativeSizeSpan` | float |
/// | 4 | `ScaleXSpan` | float |
/// | 5, 6, 14, 15 | strike/underline/super/subscript | none |
/// | 7 | `StyleSpan` | int |
/// | 8 | `BulletSpan` | 4 ints |
/// | 9 | `QuoteSpan` | 3 ints |
/// | 10 | `LeadingMarginSpan$Standard` | 2 ints |
/// | 11 | `URLSpan` | string |
/// | 12 | `BackgroundColorSpan` | int |
/// | 13 | `TypefaceSpan` | string |
/// | 16 | `AbsoluteSizeSpan` | 2 ints |
/// | 18 | `Annotation` | 2 strings |
/// | 19 | `SuggestionSpan` | array, int, 2 strings, int, then 4 (int, float) |
/// | 20, 21 | spell-check / suggestion-range | int |
/// | 23 | `LocaleSpan` | `LocaleList` (string8) |
/// | 24 | `TtsSpan` | string + `PersistableBundle` |
/// | 25 | `AccessibilityClickableSpan` | int |
/// | 26 | `AccessibilityURLSpan` | URL string + int |
/// | 27, 28 | line background / height | int |
///
/// Ids 17 (`TextAppearanceSpan`: two `ColorStateList`s, a `Typeface` and two
/// booleans), 22 (`EasyEditSpan`: a `PendingIntent`) and 29
/// (`AccessibilityReplacementSpan`: a recursive `CharSequence`) are a
/// structured `UNSUPPORTED_*`, never a guess.
pub fn read_char_sequence(p: &mut Parcel) -> Result<Option<String>> {
    let kind = p.read_int32()?;
    match kind {
        1 => Ok(p.read_string8()?),
        0 => {
            let text = p.read_string8()?;
            loop {
                let span_type = p.read_int32()?;
                if span_type == 0 {
                    return Ok(text);
                }
                read_span_payload(p, span_type)?;
                // `TextUtils.writeWhere`: start, end, flags.
                let _start = p.read_int32()?;
                let _end = p.read_int32()?;
                let _flags = p.read_int32()?;
            }
        }
        other => Err(bad(format!(
            "EditorInfo CharSequence kind {other}, want 0 (Spanned) or 1 (plain)"
        ))),
    }
}

/// Read one `ParcelableSpan` payload by its image type id.
fn read_span_payload(p: &mut Parcel, span_type: i32) -> Result<()> {
    match span_type {
        // None-payload spans: strikethrough, underline, superscript, subscript.
        5 | 6 | 14 | 15 => {}
        1 | 11 | 13 => {
            let _string = p.read_string16()?;
        }
        2 | 7 | 12 | 20 | 21 | 25 | 27 | 28 => {
            let _int = p.read_int32()?;
        }
        3 | 4 => {
            let _float = p.read_float()?;
        }
        8 => {
            for _ in 0..4 {
                let _int = p.read_int32()?;
            }
        }
        9 => {
            for _ in 0..3 {
                let _int = p.read_int32()?;
            }
        }
        10 | 16 => {
            let _a = p.read_int32()?;
            let _b = p.read_int32()?;
        }
        18 => {
            let _key = p.read_string16()?;
            let _value = p.read_string16()?;
        }
        19 => {
            let _suggestions = p.read_string16_array()?;
            let _flags = p.read_int32()?;
            let _locale = p.read_string16()?;
            let _language_tag = p.read_string16()?;
            let _hash = p.read_int32()?;
            for _ in 0..4 {
                let _color = p.read_int32()?;
                let _thickness = p.read_float()?;
            }
        }
        23 => {
            let _locales = p.read_string8()?;
        }
        24 => {
            let _domain = p.read_string16()?;
            let _args = crate::parcelable::read_bundle(p)?;
        }
        26 => {
            let _url = p.read_string16()?;
            let _clickable = p.read_int32()?;
        }
        other => {
            return Err(ServiceError::UnsupportedParcelable {
                class: format!("android.text.ParcelableSpan type {other} in EditorInfo"),
            })
        }
    }
    Ok(())
}

/// `Parcel.writeParcelable(AutofillId)` + the image's `AutofillId.writeToParcel`.
///
/// A null object is `writeString(null)` (one `-1` word). A non-null one is the
/// class name (`writeString`), then `mViewId`, `mFlags`, and the conditional
/// session/virtual-id words (`mFlags & 4` session int, `mFlags & 1` virtual int
/// else `mFlags & 2` virtual long).
pub fn read_autofill_id(p: &mut Parcel) -> Result<Option<i32>> {
    let class = p.read_string16()?;
    let Some(_class) = class else {
        return Ok(None);
    };
    let view_id = p.read_int32()?;
    let flags = p.read_int32()?;
    if flags & 4 != 0 {
        let _session = p.read_int32()?;
    }
    if flags & 1 != 0 {
        let _virtual = p.read_int32()?;
    } else if flags & 2 != 0 {
        let _virtual = p.read_int64()?;
    }
    Ok(Some(view_id))
}

/// `SurroundingText.writeToParcel`: `CharSequence`, selection start/end, offset.
pub fn read_surrounding_text(p: &mut Parcel) -> Result<()> {
    let _text = read_char_sequence(p)?;
    let _sel_start = p.read_int32()?;
    let _sel_end = p.read_int32()?;
    let _offset = p.read_int32()?;
    Ok(())
}

/// Read one image-exact `EditorInfo` payload (after the `writeTypedObject`
/// presence word).
///
/// The reads are sequential and two fields conditionally consume nested
/// payloads, so a field-by-field state machine is clearer than one literal.
#[allow(clippy::field_reassign_with_default)]
pub fn read_editor_info(p: &mut Parcel) -> Result<EditorInfo> {
    let mut info = EditorInfo::default();
    info.input_type = p.read_int32()?;
    info.ime_options = p.read_int32()?;
    info.private_ime_options = p.read_string16()?;
    info.internal_ime_options = p.read_int32()?;
    info.action_label = read_char_sequence(p)?;
    info.action_id = p.read_int32()?;
    info.initial_sel_start = p.read_int32()?;
    info.initial_sel_end = p.read_int32()?;
    info.initial_caps_mode = p.read_int32()?;
    info.initial_tool_type = p.read_int32()?;
    info.hint_text = read_char_sequence(p)?;
    info.label = read_char_sequence(p)?;
    info.package_name = p.read_string16()?;
    info.autofill_view_id = read_autofill_id(p)?;
    info.field_id = p.read_int32()?;
    info.field_name = p.read_string16()?;
    // `extras`: `writeBundle` (int length, `-1` for null) + BaseBundle payload.
    info.has_extras = crate::parcelable::read_bundle(p)?.is_some();
    info.supported_handwriting_gesture_types = p.read_int32()?;
    info.supported_handwriting_gesture_preview_types = p.read_int32()?;
    info.has_initial_surrounding_text = p.read_bool()?;
    if info.has_initial_surrounding_text {
        read_surrounding_text(p)?;
    }
    // `hintLocales == null ? LocaleList.getEmptyLocaleList() : hintLocales`,
    // then `LocaleList.writeToParcel` = one `writeString8` of the
    // `mStringRepresentation` (the empty list writes null/"").
    info.hint_locales = p.read_string8()?;
    info.content_mime_types = p.read_string16_array()?;
    // `UserHandle.writeToParcel(user, out)`: null writes the sentinel `-10000`.
    let user = p.read_int32()?;
    info.target_input_method_user = if user == -10000 { None } else { Some(user) };
    Ok(info)
}

/// Read one image-exact `ImeTracker$Token` payload (after the presence word):
/// `writeStrongBinder` then `writeString8` (the image form; the tag's
/// `[int, string16]` does not match this build).
pub fn read_ime_tracker_token(p: &mut Parcel) -> Result<ImeTrackerToken> {
    Ok(ImeTrackerToken {
        binder: p.read_strong_binder()?,
        tag: p.read_string8()?,
    })
}

/// Write one `ImeTracker$Token` payload in the image form.
pub fn write_ime_tracker_token(p: &mut Parcel, token: &ImeTrackerToken) -> Result<()> {
    p.write_strong_binder_opt(token.binder);
    p.write_string8_opt(token.tag.as_deref())?;
    Ok(())
}

fn read_typed(
    p: &mut Parcel,
    class: &str,
    editor: &mut Option<EditorInfo>,
    token: &mut Option<ImeTrackerToken>,
    saw_receiver: &mut bool,
) -> Result<()> {
    let present = p.read_int32()?;
    if present == 0 {
        return Ok(());
    }
    if class == EDITOR_INFO_CLASS {
        *editor = Some(read_editor_info(p)?);
    } else if class == IME_ON_BACK_INVOKED_CLASS {
        // `ImeOnBackInvokedDispatcher.writeToParcel` = `writeTypedObject(
        // mResultReceiver)`; `ResultReceiver.writeToParcel` = `writeStrongBinder`.
        let inner = p.read_int32()?;
        if inner != 0 {
            let _binder = p.read_strong_binder()?;
        }
    } else if class == IME_TRACKER_TOKEN_CLASS {
        *token = Some(read_ime_tracker_token(p)?);
    } else if class == RESULT_RECEIVER_CLASS {
        let _binder = p.read_strong_binder()?;
        *saw_receiver = true;
    } else {
        return Err(unsupported(class));
    }
    Ok(())
}

/// Test seam: decode one typed-object slot exactly as `startInputOrWindowGainedFocus`
/// does. Used by `tests/ime_wire.rs` to pin the unknown-class rejection.
#[doc(hidden)]
pub fn read_typed_for_test(
    p: &mut Parcel,
    class: &str,
    editor: &mut Option<EditorInfo>,
    token: &mut Option<ImeTrackerToken>,
    saw_receiver: &mut bool,
) -> Result<()> {
    read_typed(p, class, editor, token, saw_receiver)
}

/// The `IInputMethodManager` service whose reply the framework's focus path
/// needs. Registered as a dynamic node so the host routes it to
/// [`ServiceObject::raw_transact`].
pub struct ImeWireService {
    state: Arc<Mutex<ImeState>>,
    generic: ImeService,
}

impl std::fmt::Debug for ImeWireService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ImeWireService")
            .field("state", &Arc::as_ptr(&self.state))
            .finish()
    }
}

impl ImeWireService {
    /// Wrap the shared null-IME state.
    #[must_use]
    pub fn new(state: Arc<Mutex<ImeState>>) -> Self {
        Self {
            generic: ImeService::new(Arc::clone(&state)),
            state,
        }
    }

    fn start_input(&self, data: &mut Parcel, reply: &mut Parcel) -> Result<()> {
        if let Ok(path) = std::env::var("ACOS_IME_DUMP") {
            // Diagnostic only: the captured request is the replay fixture the
            // tests pin the codec to.
            let _ = std::fs::write(path, data.marshall());
        }
        let method = acos_aidl::lookup(IME_DESCRIPTOR, 9)
            .ok_or_else(|| bad("no decoded table entry for startInputOrWindowGainedFocus"))?;
        let mut sequence = 0i32;
        let mut client: Option<BinderId> = None;
        let mut remote: Option<BinderId> = None;
        let mut editor: Option<EditorInfo> = None;
        for (index, op) in method.args.iter().enumerate() {
            match op {
                WireOp::I32 => {
                    let value = data.read_int32()?;
                    if index == 0 {
                        sequence = value;
                    }
                }
                WireOp::Binder { .. } => {
                    let value = data.read_strong_binder()?;
                    match index {
                        1 => client = value,
                        7 => remote = value,
                        _ => {}
                    }
                }
                WireOp::TypedObject { class, .. } => {
                    let mut token = None;
                    let mut saw_receiver = false;
                    read_typed(data, class, &mut editor, &mut token, &mut saw_receiver)?;
                }
                other => {
                    return Err(bad(format!(
                        "startInputOrWindowGainedFocus: undecoded argument {index} ({})",
                        other.render()
                    )))
                }
            }
        }
        let (recorded, _method, _session) = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .start_input(client, sequence, remote);
        eprintln!(
            "ACOSW13 ime startInput sequence={sequence} client={:?} recorded={recorded} \
             editor={{inputType={} imeOptions={} package={:?} fieldId={} hint={:?} autofill={:?} \
             extras={} surrounding={} mime={:?}}} -> NO_IME",
            client.map(|b| b.0),
            editor.as_ref().map(|e| e.input_type).unwrap_or(-1),
            editor.as_ref().map(|e| e.ime_options).unwrap_or(-1),
            editor.as_ref().and_then(|e| e.package_name.clone()),
            editor.as_ref().map(|e| e.field_id).unwrap_or(-1),
            editor.as_ref().and_then(|e| e.hint_text.clone()),
            editor.as_ref().and_then(|e| e.autofill_view_id),
            editor.as_ref().map(|e| e.has_extras).unwrap_or(false),
            editor
                .as_ref()
                .map(|e| e.has_initial_surrounding_text)
                .unwrap_or(false),
            editor.as_ref().and_then(|e| e.content_mime_types.clone()),
        );
        reply.reset();
        write_no_exception(reply);
        reply.write_int32(1); // writeTypedObject presence
        InputBindResult::no_ime(sequence).write_payload(reply)?;
        Ok(())
    }

    fn soft_input(&self, data: &mut Parcel, reply: &mut Parcel, show: bool) -> Result<()> {
        let code = if show { 7 } else { 8 };
        let method = acos_aidl::lookup(IME_DESCRIPTOR, code)
            .ok_or_else(|| bad("no decoded table entry for showSoftInput/hideSoftInput"))?;
        let mut client = BinderId(0);
        let mut flags = 0i32;
        for (index, op) in method.args.iter().enumerate() {
            match op {
                WireOp::I32 => {
                    let value = data.read_int32()?;
                    if index == 3 {
                        flags = value;
                    }
                }
                WireOp::Binder { .. } => {
                    let value = data.read_strong_binder()?;
                    if index == 0 {
                        client = value.unwrap_or(BinderId(0));
                    }
                }
                WireOp::TypedObject { class, .. } => {
                    let mut editor = None;
                    let mut token = None;
                    let mut saw_receiver = false;
                    read_typed(data, class, &mut editor, &mut token, &mut saw_receiver)?;
                }
                other => {
                    return Err(bad(format!(
                        "{}: undecoded argument {index} ({})",
                        method.name,
                        other.render()
                    )))
                }
            }
        }
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let shown = if show {
            state.show_soft_input(client, flags)
        } else {
            state.hide_soft_input(client, flags)
        };
        drop(state);
        reply.reset();
        write_no_exception(reply);
        reply.write_bool(shown);
        Ok(())
    }
}

impl ServiceObject for ImeWireService {
    fn call(
        &mut self,
        interface: &str,
        method: &acos_aidl::MethodInfo,
        args: &[crate::value::Value],
    ) -> Result<CallResult> {
        self.generic.call(interface, method, args)
    }

    fn raw_transact(
        &mut self,
        interface: &str,
        code: u32,
        data: &mut Parcel,
        reply: &mut Parcel,
    ) -> Result<bool> {
        if interface != IME_DESCRIPTOR {
            return Ok(false);
        }
        let method = acos_aidl::lookup(interface, code).ok_or_else(|| {
            ServiceError::UnsupportedAndroidApi {
                interface: interface.to_string(),
                code,
                method: format!("code {code}"),
            }
        })?;
        match method.name.as_str() {
            METHOD_START_INPUT => self.start_input(data, reply)?,
            "showSoftInput" => self.soft_input(data, reply, true)?,
            "hideSoftInput" => self.soft_input(data, reply, false)?,
            _ => {
                // Everything else keeps the generic `Value` layout. The request
                // has already had its interface token enforced by the host.
                let args = codec::read_args(data, &method.args, interface, &method.name)?;
                let call = self.generic.call(interface, method, &args)?;
                reply.reset();
                write_no_exception(reply);
                codec::write_reply(reply, &method.reply, &call.reply, interface, &method.name)?;
            }
        }
        Ok(true)
    }
}
