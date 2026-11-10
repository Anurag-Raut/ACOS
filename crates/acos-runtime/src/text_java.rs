//! Typed-text injection through the real framework `InputConnection` path
//! (T-41).
//!
//! # Why this is the honest IME path
//!
//! The agent-facing `acos session text` / `acos_text` tool must edit the
//! **focused editor the way a soft keyboard does**, so that `TextWatcher`
//! callbacks, the selection/cursor, and `TextView`'s own `Editable` all update
//! exactly as they do on a device. That path is:
//!
//! ```text
//! IME -> InputConnection.commitText(text, newCursorPosition) -> TextView -> Editable.replace -> TextWatcher
//! ```
//!
//! ACOS runs no `InputMethodService`, so no IME process calls `commitText`. This
//! module plays that side in-process: it locates the focused `View` in the live
//! hierarchy, asks the framework for the field's own connection with the real
//! [`View.onCreateInputConnection`] (for a `TextView` that is the real
//! `EditableInputConnection`), and calls its real `commitText`. Nothing is
//! synthesised: the object and the method are the framework's.
//!
//! When the focused field has no `InputConnection` (a custom `View` with a key
//! listener, for example), it falls back to a `KeyEvent` sequence through the
//! existing input dispatcher, using the same `KeyCharacterMap` mapping the
//! framework's own `KeyEvent.getUnicodeChar` uses. Characters the key map does
//! not model (uppercase, punctuation, non-ASCII) are reported as a structured
//! [`TextError::UnsupportedCharacter`] rather than guessed at.
//!
//! # What is deliberately not modelled
//!
//! * composing text / `setComposingText` — ACOS commits, it never leaves a
//!   composing region open;
//! * `performEditorAction` / IME actions — use `acos session key enter`;
//! * rich text, spans, clipboard pastes.
//!
//! These are named in the result (`composing: false`) or refused, never faked.

use acos_vm::heap::{Handle, NULL};
use acos_vm::javabridge::JavaValue;
use acos_vm::Vm;

use crate::input_java;

/// How the text reached the application.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextPath {
    /// `InputConnection.commitText` on the focused field (the real IME path).
    InputConnection,
    /// A `KeyEvent` sequence through the input dispatcher.
    KeyEvents,
}

impl TextPath {
    /// Stable wire name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            TextPath::InputConnection => "input-connection",
            TextPath::KeyEvents => "key-events",
        }
    }
}

/// A successful typed-text injection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextReport {
    /// Which path edited the field.
    pub path: TextPath,
    /// Number of Unicode scalar values committed.
    pub characters: usize,
    /// Dispatcher acknowledgement sequences (empty on the `InputConnection`
    /// path, which writes no input frames).
    pub ack_seqs: Vec<u32>,
    /// The focused view class, when the `InputConnection` path was used.
    pub view_class: Option<String>,
}

/// Why typed text could not be delivered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TextError {
    /// The hierarchy has no focused `View`.
    NoFocusedEditor,
    /// A character that neither path can represent.
    UnsupportedCharacter(char),
    /// The runtime has not booted the app yet.
    NotReady(String),
    /// No window holds input focus for the key-event fallback.
    NoFocusedWindow,
    /// A framework call failed.
    Framework(String),
}

impl TextError {
    /// Stable ACOS error code.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            TextError::NoFocusedEditor => "ELEMENT_NOT_FOUND",
            TextError::NoFocusedWindow => "ELEMENT_NOT_FOUND",
            TextError::UnsupportedCharacter(_) => "UNSUPPORTED_BACKEND",
            TextError::NotReady(_) => "UNSUPPORTED_BACKEND",
            TextError::Framework(_) => "INTERNAL",
        }
    }

    /// Machine-readable `kind`/`reason` detail.
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            TextError::NoFocusedEditor => "NoFocusedEditor",
            TextError::NoFocusedWindow => "NoFocusedWindow",
            TextError::UnsupportedCharacter(_) => "UnsupportedCharacter",
            TextError::NotReady(_) => "not_ready",
            TextError::Framework(_) => "FrameworkError",
        }
    }

    /// Human-readable message.
    #[must_use]
    pub fn message(&self) -> String {
        match self {
            TextError::NoFocusedEditor => {
                "no focused editor: tap the text field first (the IME edits the focused view)"
                    .to_string()
            }
            TextError::NoFocusedWindow => {
                "no window currently holds input focus; launch and observe before typing".to_string()
            }
            TextError::UnsupportedCharacter(ch) => format!(
                "the key-event fallback cannot type {ch:?}; it only maps a-z, 0-9, space and newline"
            ),
            TextError::NotReady(msg) => msg.clone(),
            TextError::Framework(msg) => msg.clone(),
        }
    }
}

/// Map `text` to Android key codes for the dispatcher fallback.
///
/// The mapping matches [`KeyCharacterMap`] as ACOS models it: `a-z` down, `0-9`
/// down, space and enter. Anything else is refused (never silently dropped).
///
/// [`KeyCharacterMap`]: acos_vm::natives::fwk_input
pub fn text_to_key_codes(text: &str) -> Result<Vec<i32>, TextError> {
    let mut codes = Vec::with_capacity(text.chars().count());
    for ch in text.chars() {
        let code = match ch {
            'a'..='z' => 29 + (ch as i32 - 'a' as i32),
            '0'..='9' => 7 + (ch as i32 - '0' as i32),
            ' ' => 62,
            '\n' => 66,
            other => return Err(TextError::UnsupportedCharacter(other)),
        };
        codes.push(code);
    }
    Ok(codes)
}

/// Type `text` into the focused editor.
///
/// Prefers the real `InputConnection.commitText` path; falls back to a
/// `KeyEvent` sequence only when the focused view has no editable connection.
pub fn inject_text(vm: &mut Vm, text: &str) -> Result<TextReport, TextError> {
    if text.is_empty() {
        return Ok(TextReport {
            path: TextPath::InputConnection,
            characters: 0,
            ack_seqs: Vec::new(),
            view_class: None,
        });
    }

    let view = match focused_view(vm) {
        Ok(view) => view,
        Err(TextError::NoFocusedEditor) => return inject_key_events(text),
        Err(e) => return Err(e),
    };
    let view_class = vm
        .class_of_object(view)
        .map(|cid| vm.class_desc_of_id(cid))
        .map(|desc| desc.replace('/', ".").trim_matches(';').to_string());

    match editable_connection(vm, view)? {
        Some(connection) => {
            let mut report = commit_text_to(vm, connection, text)?;
            report.view_class = view_class;
            Ok(report)
        }
        None => inject_key_events(text),
    }
}

/// Commit `text` through an already-resolved `InputConnection`.
///
/// This is the exact `IRemoteInputConnection.commitText` an IME performs; it is
/// public so a Java harness can drive the bridge with a real (or proxy)
/// connection without needing a launched `Activity`.
///
/// [`InputConnection`]: android.view.inputmethod.InputConnection
pub fn commit_text_to(
    vm: &mut Vm,
    connection: Handle,
    text: &str,
) -> Result<TextReport, TextError> {
    commit_text(vm, connection, text)?;
    Ok(TextReport {
        path: TextPath::InputConnection,
        characters: text.chars().count(),
        ack_seqs: Vec::new(),
        view_class: None,
    })
}

/// Deliver `text` as key down/up pairs through the service input dispatcher.
fn inject_key_events(text: &str) -> Result<TextReport, TextError> {
    let codes = text_to_key_codes(text)?;
    let mut ack_seqs = Vec::new();
    for code in &codes {
        let seqs = input_java::inject_key(*code).map_err(|e| match e {
            acos_services::input::InputError::NoFocusedWindow => TextError::NoFocusedWindow,
            other => TextError::Framework(format!("input dispatcher: {other}")),
        })?;
        ack_seqs.extend(seqs);
    }
    Ok(TextReport {
        path: TextPath::KeyEvents,
        characters: codes.len(),
        ack_seqs,
        view_class: None,
    })
}

/// Ask the focused field for its real `InputConnection`.
///
/// Returns `Ok(None)` when the focused view is not editable (the framework's
/// own `View.onCreateInputConnection` returns `null`).
fn editable_connection(vm: &mut Vm, view: Handle) -> Result<Option<Handle>, TextError> {
    let editor = vm
        .new_instance_ctor("android.view.inputmethod.EditorInfo", "()V", &[])
        .map_err(|e| {
            vm.pending_exception = NULL;
            TextError::Framework(format!("new EditorInfo: {e}"))
        })?;
    match vm.call_method(
        view,
        "onCreateInputConnection",
        "(Landroid/view/inputmethod/EditorInfo;)Landroid/view/inputmethod/InputConnection;",
        &[JavaValue::Object(editor)],
    ) {
        Ok(JavaValue::Object(h)) if h != NULL => Ok(Some(h)),
        Ok(_) => Ok(None),
        Err(e) => {
            vm.pending_exception = NULL;
            Err(TextError::Framework(format!(
                "onCreateInputConnection failed: {e}"
            )))
        }
    }
}

/// `InputConnection.commitText(text, 1)`: insert at the selection and move the
/// cursor to the end of the inserted text, exactly as an IME does.
fn commit_text(vm: &mut Vm, connection: Handle, text: &str) -> Result<(), TextError> {
    let sequence = vm.new_string_rooted(text).map_err(|e| {
        vm.pending_exception = NULL;
        TextError::Framework(format!("new String: {e}"))
    })?;
    match vm.call_method(
        connection,
        "commitText",
        "(Ljava/lang/CharSequence;I)Z",
        &[JavaValue::Object(sequence), JavaValue::Int(1)],
    ) {
        Ok(_) => Ok(()),
        Err(e) => {
            vm.pending_exception = NULL;
            Err(TextError::Framework(format!("commitText failed: {e}")))
        }
    }
}

/// The focused `View` in the live hierarchy.
///
/// Uses the framework's own `View.findFocus()` on every root window, so the
/// answer is whatever the app actually focused.
fn focused_view(vm: &mut Vm) -> Result<Handle, TextError> {
    let roots = root_views(vm)?;
    if roots.is_empty() {
        return Err(TextError::NotReady(
            "the app has not added a window yet".to_string(),
        ));
    }
    for root in roots {
        match vm.call_method(root, "findFocus", "()Landroid/view/View;", &[]) {
            Ok(JavaValue::Object(h)) if h != NULL => return Ok(h),
            Ok(_) => {}
            Err(_) => {
                vm.pending_exception = NULL;
            }
        }
    }
    Err(TextError::NoFocusedEditor)
}

/// Resolve the root `View` of every live `ViewRootImpl`.
///
/// Mirrors [`crate::view_tree`]'s root discovery so the tree walker and the
/// text path agree on what a "window" is.
fn root_views(vm: &mut Vm) -> Result<Vec<Handle>, TextError> {
    let global = match vm.call_static(
        "android.view.WindowManagerGlobal",
        "getInstance",
        "()Landroid/view/WindowManagerGlobal;",
        &[],
    ) {
        Ok(JavaValue::Object(h)) if h != NULL => h,
        Ok(_) => {
            return Err(TextError::NotReady(
                "WindowManagerGlobal.getInstance() returned null".to_string(),
            ))
        }
        Err(e) => {
            vm.pending_exception = NULL;
            return Err(TextError::Framework(format!(
                "WindowManagerGlobal.getInstance(): {e}"
            )));
        }
    };

    let list = match vm.call_method(global, "getRootViews", "()Ljava/util/ArrayList;", &[]) {
        Ok(JavaValue::Object(h)) if h != NULL => h,
        Ok(_) => match vm.get_field(global, "mViews") {
            Ok(JavaValue::Object(h)) if h != NULL => h,
            _ => {
                vm.pending_exception = NULL;
                return Err(TextError::NotReady(
                    "WindowManagerGlobal exposes neither getRootViews() nor mViews".to_string(),
                ));
            }
        },
        Err(_) => {
            vm.pending_exception = NULL;
            match vm.get_field(global, "mViews") {
                Ok(JavaValue::Object(h)) if h != NULL => h,
                _ => {
                    vm.pending_exception = NULL;
                    return Err(TextError::NotReady(
                        "WindowManagerGlobal exposes neither getRootViews() nor mViews".to_string(),
                    ));
                }
            }
        }
    };

    let count = match vm.call_method(list, "size", "()I", &[]) {
        Ok(JavaValue::Int(n)) if n > 0 => n as usize,
        _ => {
            vm.pending_exception = NULL;
            return Ok(Vec::new());
        }
    };

    let mut roots = Vec::new();
    for i in 0..count {
        let entry = match vm.call_method(
            list,
            "get",
            "(I)Ljava/lang/Object;",
            &[JavaValue::Int(i as i32)],
        ) {
            Ok(JavaValue::Object(h)) if h != NULL => h,
            _ => {
                vm.pending_exception = NULL;
                continue;
            }
        };
        // A `ViewRootImpl` entry exposes `mView`; a plain root `View` is used
        // directly (the `mViews` fallback list).
        let view = match vm.get_field(entry, "mView") {
            Ok(JavaValue::Object(h)) if h != NULL => h,
            _ => {
                vm.pending_exception = NULL;
                entry
            }
        };
        roots.push(view);
    }
    Ok(roots)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_map_covers_lowercase_digits_space_newline() {
        assert_eq!(text_to_key_codes("abc").unwrap(), vec![29, 30, 31]);
        assert_eq!(text_to_key_codes("09").unwrap(), vec![7, 16]);
        assert_eq!(text_to_key_codes(" ").unwrap(), vec![62]);
        assert_eq!(text_to_key_codes("\n").unwrap(), vec![66]);
    }

    #[test]
    fn key_map_refuses_what_it_cannot_type() {
        assert_eq!(
            text_to_key_codes("Hi"),
            Err(TextError::UnsupportedCharacter('H'))
        );
        assert_eq!(
            text_to_key_codes("café"),
            Err(TextError::UnsupportedCharacter('é'))
        );
        // Refuses before injecting anything: the whole string is checked first.
        assert!(text_to_key_codes("ok!").is_err());
    }

    #[test]
    fn errors_are_coded_not_passes() {
        assert_eq!(TextError::NoFocusedEditor.code(), "ELEMENT_NOT_FOUND");
        assert_eq!(
            TextError::UnsupportedCharacter('!').code(),
            "UNSUPPORTED_BACKEND"
        );
        assert_eq!(TextError::NotReady(String::new()).code(), "UNSUPPORTED_BACKEND");
        assert_eq!(TextError::Framework(String::new()).code(), "INTERNAL");
    }
}
