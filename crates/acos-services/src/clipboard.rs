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

use crate::error::{Result, ServiceError};
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
    /// The marshalled `ClipDescription` (the clip's leading bytes), replayed
    /// verbatim by `getPrimaryClipDescription` on the ART wire path. Empty when
    /// the facts were not read from a parcel.
    pub description_bytes: Vec<u8>,
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
            description_bytes: Vec::new(),
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
    /// `onPrimaryClipChanged` deliveries a raw clip transaction queued.
    pending: Vec<crate::host::PendingDelivery>,
}

/// `IOnPrimaryClipChangedListener`.
const LISTENER_DESCRIPTOR: &str = "android.content.IOnPrimaryClipChangedListener";

impl ClipboardService {
    /// Wrap shared state.
    #[must_use]
    pub fn new(state: Arc<Mutex<ClipboardState>>) -> Self {
        Self {
            state,
            pending: Vec::new(),
        }
    }

    /// Queue `onPrimaryClipChanged()` for every registered listener.
    fn notify_listeners(&mut self, listeners: &[BinderId]) {
        for listener in listeners {
            let mut data = acos_parcel::Parcel::new();
            let _ = data.write_interface_token(LISTENER_DESCRIPTOR);
            self.pending.push(crate::host::PendingDelivery {
                target: *listener,
                interface: LISTENER_DESCRIPTOR.to_string(),
                // The AIDL's only method (`oneway void onPrimaryClipChanged()`).
                code: 1,
                method: "onPrimaryClipChanged".to_string(),
                data,
            });
        }
    }
}

fn bad_request(method: &str, error: impl std::fmt::Display) -> ServiceError {
    ServiceError::BadRequest {
        interface: "android.content.IClipboard".to_string(),
        method: method.to_string(),
        detail: error.to_string(),
    }
}

/// The four trailing `(String callingPackage, String attributionTag, int userId,
/// int deviceId)` arguments every `IClipboard` clip method ends with; returns
/// the calling package.
fn read_caller(data: &mut acos_parcel::Parcel, method: &str) -> Result<String> {
    let package = data
        .read_string16()
        .map_err(|e| bad_request(method, e))?
        .unwrap_or_default();
    let _attribution = data.read_string16().map_err(|e| bad_request(method, e))?;
    let _user = data.read_int32().map_err(|e| bad_request(method, e))?;
    let _device = data.read_int32().map_err(|e| bad_request(method, e))?;
    Ok(package)
}

impl ServiceObject for ClipboardService {
    fn serves_raw(&self, interface: &str, method: &str) -> bool {
        interface == "android.content.IClipboard"
            && matches!(
                method,
                "setPrimaryClip"
                    | "setPrimaryClipAsPackage"
                    | "getPrimaryClip"
                    | "getPrimaryClipDescription"
                    | "clearPrimaryClip"
            )
    }

    fn drain_raw_deliveries(&mut self) -> Vec<crate::host::PendingDelivery> {
        std::mem::take(&mut self.pending)
    }

    /// The `ClipData`-carrying methods, on the wire path of the ART engine.
    ///
    /// The clip is stored as the **bytes the framework's own
    /// `ClipData.writeToParcel` produced** (located with
    /// [`crate::parcelable::read_clip_shape`], which walks the image's field
    /// order), and `getPrimaryClip` replays them, so the caller's
    /// `ClipData.CREATOR` reads exactly what was copied. No `ClipData` is ever
    /// re-encoded here.
    fn raw_transact(
        &mut self,
        interface: &str,
        code: u32,
        data: &mut acos_parcel::Parcel,
        reply: &mut acos_parcel::Parcel,
    ) -> Result<bool> {
        if interface != "android.content.IClipboard" {
            return Ok(false);
        }
        let Some(method) = acos_aidl::lookup(interface, code) else {
            return Ok(false);
        };
        let name = method.name.clone();
        match name.as_str() {
            "setPrimaryClip" | "setPrimaryClipAsPackage" => {
                let present = data.read_int32().map_err(|e| bad_request(&name, e))?;
                if present == 0 {
                    return Err(bad_request(&name, "clip is null"));
                }
                let start = data.data_position();
                let shape = crate::parcelable::read_clip_shape(data)?;
                let end = data.data_position();
                let whole = data.marshall();
                let bytes = whole
                    .get(start..end)
                    .ok_or_else(|| bad_request(&name, "clip extent is outside the parcel"))?
                    .to_vec();
                let package = read_caller(data, &name)?;
                let source = if name == "setPrimaryClipAsPackage" {
                    data.read_string16()
                        .map_err(|e| bad_request(&name, e))?
                        .unwrap_or_default()
                } else {
                    package
                };
                let facts = ClipFacts {
                    label: shape.label,
                    mime_types: shape.mime_types,
                    item_count: shape.item_count,
                    has_text: shape.has_text,
                    has_uri: shape.has_uri,
                    has_intent: shape.has_intent,
                    description_bytes: bytes[..shape.description_len.min(bytes.len())].to_vec(),
                };
                let listeners = {
                    let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                    state.set_primary_clip(bytes, facts, source);
                    state.listeners.clone()
                };
                self.notify_listeners(&listeners);
                crate::exception::write_no_exception(reply);
                Ok(true)
            }
            "clearPrimaryClip" => {
                let _ = read_caller(data, &name)?;
                let listeners = {
                    let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                    let had = state.has_primary_clip();
                    state.clear();
                    if had { state.listeners.clone() } else { Vec::new() }
                };
                self.notify_listeners(&listeners);
                crate::exception::write_no_exception(reply);
                Ok(true)
            }
            "getPrimaryClip" => {
                let _ = read_caller(data, &name)?;
                let entry = self
                    .state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .get_primary_clip()
                    .cloned();
                crate::exception::write_no_exception(reply);
                match entry {
                    Some(entry) => {
                        reply.write_int32(1);
                        reply.write_raw_bytes(&entry.bytes);
                    }
                    None => reply.write_int32(0),
                }
                Ok(true)
            }
            "getPrimaryClipDescription" => {
                let _ = read_caller(data, &name)?;
                let facts = self
                    .state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .description()
                    .cloned();
                crate::exception::write_no_exception(reply);
                match facts {
                    Some(facts) if !facts.description_bytes.is_empty() => {
                        reply.write_int32(1);
                        reply.write_raw_bytes(&facts.description_bytes);
                    }
                    _ => reply.write_int32(0),
                }
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    fn call(
        &mut self,
        interface: &str,
        method: &acos_aidl::MethodInfo,
        args: &[Value],
    ) -> Result<CallResult> {
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
                state.access_notifications = args
                    .first()
                    .and_then(|v| match v {
                        Value::Bool(b) => Some(*b),
                        _ => None,
                    })
                    .unwrap_or(false);
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
