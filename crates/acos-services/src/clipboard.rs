//! `svc.clipboard`: a real in-memory `IClipboard` with a `ClipData` round trip.
//!
//! The clipboard stores the **marshalled `ClipData` bytes** the framework's own
//! `ClipData.writeToParcel` produced, exactly as the platform service stores the
//! `ClipData` object. `getPrimaryClip` hands those bytes back untouched, so the
//! caller's `ClipData.CREATOR.createFromParcel` reads the same clip that was
//! set — including `ClipDescription`, MIME types, text, HTML text, URI, Intent
//! and the per-item styles. A Rust-side re-encoding of `ClipData` would be a
//! second implementation of a 4-class parcel format; it is deliberately avoided
//! (`research/service-layer/REPORT.md` §3.6).
//!
//! `hasClipboardText`/`hasPrimaryClip` need *semantic* knowledge, which the
//! store gets from the decoded `ClipData` facts the runtime hands in with the
//! bytes ([`ClipFacts`]) — read from the real Java object, not guessed from the
//! bytes.
//!
//! There is one clipboard, for the single ACOS user (id 0); per-user and
//! per-device clipboards do not exist here and are reported as
//! [`ServiceError::UnsupportedAndroidApi`] when a caller asks for another user.

use std::sync::{Arc, Mutex};

use acos_parcel::binder::BinderId;

use crate::error::{ServiceError, Result};
use crate::host::{CallResult, ServiceObject};
use crate::value::Value;

/// The facts the runtime read from the real `ClipData` object.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ClipFacts {
    /// `ClipDescription.getLabel()`.
    pub label: String,
    /// `ClipDescription.getMimeTypeCount()` / each MIME type.
    pub mime_types: Vec<String>,
    /// `ClipData.getItemCount()`.
    pub item_count: i32,
    /// `ClipData.getItemAt(0).getText() != null`.
    pub has_text: bool,
    /// `ClipData.getItemAt(0).getUri() != null`.
    pub has_uri: bool,
    /// `ClipData.getItemAt(0).getIntent() != null`.
    pub has_intent: bool,
}

impl ClipFacts {
    /// A one-item text clip.
    #[must_use]
    pub fn text(text: &str) -> Self {
        Self {
            label: String::new(),
            mime_types: vec!["text/plain".to_string()],
            item_count: 1,
            has_text: !text.is_empty(),
            has_uri: false,
            has_intent: false,
        }
    }
}

/// One stored clip.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClipEntry {
    /// The marshalled `ClipData` (produced by the real `writeToParcel`).
    pub bytes: Vec<u8>,
    /// Facts read from the real object.
    pub facts: ClipFacts,
    /// The package that set it (`setPrimaryClip`'s calling package).
    pub source: String,
}

/// `IClipboard` state.
#[derive(Debug, Default)]
pub struct ClipboardState {
    /// The primary clip, if any.
    pub primary: Option<ClipEntry>,
    /// Registered `IOnPrimaryClipChangedListener` binders.
    pub listeners: Vec<BinderId>,
    /// `setPrimaryClip*` calls recorded.
    pub sets: Vec<String>,
    /// `getPrimaryClip` calls recorded.
    pub gets: u32,
    /// Whether clipboard-access notifications are enabled for user 0.
    pub access_notifications: bool,
}

impl ClipboardState {
    /// An empty clipboard.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// `setPrimaryClip(bytes, facts, source)`.
    pub fn set_primary_clip(&mut self, bytes: Vec<u8>, facts: ClipFacts, source: String) {
        self.sets.push(source.clone());
        self.primary = Some(ClipEntry {
            bytes,
            facts,
            source,
        });
    }

    /// `getPrimaryClip()`: the stored marshalled clip.
    #[must_use]
    pub fn get_primary_clip(&mut self) -> Option<&ClipEntry> {
        self.gets += 1;
        self.primary.as_ref()
    }

    /// `clearPrimaryClip()`.
    pub fn clear(&mut self) {
        self.primary = None;
    }

    /// `hasPrimaryClip()`.
    #[must_use]
    pub fn has_primary_clip(&self) -> bool {
        self.primary.is_some()
    }

    /// `hasClipboardText()`.
    #[must_use]
    pub fn has_clipboard_text(&self) -> bool {
        self.primary.as_ref().is_some_and(|e| e.facts.has_text)
    }

    /// `getPrimaryClipDescription()` facts.
    #[must_use]
    pub fn description(&self) -> Option<&ClipFacts> {
        self.primary.as_ref().map(|e| &e.facts)
    }

    /// `addPrimaryClipChangedListener`.
    pub fn add_listener(&mut self, listener: Option<BinderId>) {
        if let Some(l) = listener {
            if !self.listeners.contains(&l) {
                self.listeners.push(l);
            }
        }
    }

    /// `removePrimaryClipChangedListener`.
    pub fn remove_listener(&mut self, listener: Option<BinderId>) {
        if let Some(l) = listener {
            self.listeners.retain(|x| *x != l);
        }
    }
}

fn unsupported(interface: &str, method: &acos_aidl::MethodInfo) -> ServiceError {
    ServiceError::UnsupportedAndroidApi {
        interface: interface.to_string(),
        code: method.code,
        method: method.name.clone(),
    }
}

/// `IClipboard` handler for the transactions whose layout is fully decoded.
///
/// `setPrimaryClip`/`setPrimaryClipAsPackage` carry a `ClipData` request that
/// only the real `ClipData.CREATOR` can read, so they are handled on the wire
/// side (`acos-runtime::clipboard_java`) which stores into this same state and
/// answers here. If one reaches this handler (no VM available) it is a
/// structured error, never a silent drop.
pub struct ClipboardService {
    state: Arc<Mutex<ClipboardState>>,
}

impl ClipboardService {
    /// Wrap shared state.
    #[must_use]
    pub fn new(state: Arc<Mutex<ClipboardState>>) -> Self {
        Self { state }
    }
}

impl ServiceObject for ClipboardService {
    fn call(&mut self, interface: &str, method: &acos_aidl::MethodInfo, args: &[Value]) -> Result<CallResult> {
        if interface != "android.content.IClipboard" {
            return Err(unsupported(interface, method));
        }
        let mut state = self.state.lock().unwrap();
        let reply = match method.name.as_str() {
            // The reply is the stored marshalled clip; only the runtime
            // wire path (`clipboard_java`) can emit it verbatim. Answering
            // with a generic codec here would hand the client bytes that
            // `ClipData.CREATOR` misreads, so it is a structured error.
            "getPrimaryClip" => return Err(unsupported(interface, method)),
            "getPrimaryClipDescription" => {
                let facts = state.description().cloned();
                match facts {
                    Some(f) => vec![Value::TypedObject(crate::value::Parcelable::ordered(
                        "android.content.ClipDescription",
                        vec![
                            Value::Str8(Some(f.label.clone())),
                            Value::StringArray(Some(
                                f.mime_types.iter().cloned().map(Some).collect(),
                            )),
                        ],
                    ))],
                    None => vec![Value::TypedObject(crate::value::Parcelable::Null)],
                }
            }
            "hasPrimaryClip" => vec![Value::Bool(state.has_primary_clip())],
            "hasClipboardText" => vec![Value::Bool(state.has_clipboard_text())],
            "getPrimaryClipSource" => vec![Value::Str8(
                state.primary.as_ref().map(|e| e.source.clone()),
            )],
            "clearPrimaryClip" => {
                state.clear();
                Vec::new()
            }
            "addPrimaryClipChangedListener" => {
                let listener = args.first().and_then(Value::as_binder);
                state.add_listener(listener);
                Vec::new()
            }
            "removePrimaryClipChangedListener" => {
                let listener = args.first().and_then(Value::as_binder);
                state.remove_listener(listener);
                Vec::new()
            }
            "areClipboardAccessNotificationsEnabledForUser" => {
                vec![Value::Bool(state.access_notifications)]
            }
            "setClipboardAccessNotificationsEnabledForUser" => {
                state.access_notifications = args.first().and_then(|v| match v {
                    Value::Bool(b) => Some(*b),
                    _ => None,
                }).unwrap_or(false);
                Vec::new()
            }
            // Needs the real `ClipData` reader; handled on the wire path.
            "setPrimaryClip" | "setPrimaryClipAsPackage" => {
                return Err(unsupported(interface, method))
            }
            _ => return Err(unsupported(interface, method)),
        };
        Ok(CallResult {
            reply,
            deliveries: Vec::new(),
        })
    }
}

/// The process-wide clipboard state (one clipboard per ACOS user).
static CLIPBOARD: Mutex<Option<Arc<Mutex<ClipboardState>>>> = Mutex::new(None);

/// The shared clipboard state, installing a fresh one on first use.
#[must_use]
pub fn shared() -> Arc<Mutex<ClipboardState>> {
    let mut slot = CLIPBOARD.lock().unwrap();
    if let Some(state) = slot.as_ref() {
        return Arc::clone(state);
    }
    let state = Arc::new(Mutex::new(ClipboardState::new()));
    *slot = Some(Arc::clone(&state));
    state
}

/// Replace the shared clipboard state (test isolation / session reset).
pub fn install(state: Arc<Mutex<ClipboardState>>) {
    *CLIPBOARD.lock().unwrap() = Some(state);
}
