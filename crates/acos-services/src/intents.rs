//! MPROC1 step 2: the intent **capture target** (the built-in share/pick receiver).
//!
//! On a device, `ACTION_SEND` / `ACTION_VIEW` / `ACTION_PICK` /
//! `ACTION_GET_CONTENT` are resolved by the package manager to some installed
//! app — the share sheet, a browser, the photo picker. A session may have no
//! such app installed (it is one APK), and the old behaviour was an
//! `UNSUPPORTED_ANDROID_API` marker from the guest's `IActivityTaskManager`
//! guard: the app's `startActivity` silently did nothing and a
//! `startActivityForResult` waited forever.
//!
//! The capture target is what the plan calls for: a receiver in the broker
//! (the Rust service layer, which already plays system_server), with **no
//! process**. The unresolved intent is recorded verbatim — action, data, MIME
//! type, categories, extras, the synthetic component the caller was told, the
//! intent flags and the request code — and `startActivity` answers
//! `START_SUCCESS` because on a phone the share sheet *would* have accepted it.
//! Nothing pretends an app handled it: every reply says `captured`.
//!
//! The agent reads the record with the session control op
//! `acos_service_ctl {module: "intents", op: "last" || "list"}` and can script
//! the answer a picker would give
//! (`op: "result", args: {result_code, data, mime_type, extras}`), which is
//! delivered to the waiting activity as the framework's own
//! `ActivityResultItem` `ClientTransaction` (see `LaunchMain.deliverScriptedResult`).
//!
//! What is captured is deliberately small and documented: only the four
//! intent families above, only when **nothing installed matches**. Every other
//! unresolvable start stays `START_NOT_FOUND`, so a typo in an explicit
//! component never turns into a fake capture.

use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex};

use acos_aidl::MethodInfo;

use crate::ams::{Extra, IntentFacts};
use crate::error::{Result, ServiceError};
use crate::host::{CallResult, ServiceObject};
use crate::value::Value;

/// `ServiceManager` name of the broker (a session service, not an Android one).
pub const SERVICE_NAME: &str = "acos.intents";

/// The AIDL-ish descriptor the broker registers under. Nothing in the guest
/// calls it: it exists so the broker is a real, listable session service like
/// every other module (`ServiceManager.getService("acos.intents")`).
pub const DESCRIPTOR: &str = "dev.acos.intents.IIntentBroker";

/// Package of the synthetic capture component the caller is told about.
pub const CAPTURE_PACKAGE: &str = "com.acos.capture";

/// Class of the synthetic capture component.
pub const CAPTURE_CLASS: &str = "com.acos.capture.Capture";

/// The component name the caller sees (`com.acos.capture/.Capture`).
#[must_use]
pub fn capture_component() -> String {
    format!("{CAPTURE_PACKAGE}/.Capture")
}

/// How many captures one session keeps (oldest dropped first).
pub const MAX_CAPTURES: usize = 32;

/// One captured intent: what the app asked for, verbatim.
#[derive(Debug, Clone, PartialEq)]
pub struct CapturedIntent {
    /// Monotonic capture sequence (1-based, per session).
    pub seq: u64,
    /// Capture time (host wall clock, ms since the epoch).
    pub utc_ms: u64,
    /// `Intent.getAction()`.
    pub action: String,
    /// `Intent.getDataString()` (a URI string, when present).
    pub data: Option<String>,
    /// `Intent.getType()` (the MIME type).
    pub mime_type: Option<String>,
    /// `Intent.getCategories()`.
    pub categories: Vec<String>,
    /// `Intent.getExtras()`: string/int/bool values; other types are recorded
    /// as present-without-value (`Extra::Present`).
    pub extras: BTreeMap<String, Extra>,
    /// The synthetic component the caller was told accepts the intent.
    pub component: String,
    /// `Intent.getFlags()`.
    pub flags: i32,
    /// The request code of `startActivityForResult` (0 when the caller did not
    /// ask for a result).
    pub request_code: i32,
    /// True when the caller passed a result target (`startActivityForResult` /
    /// `startActivityIfNeeded`), i.e. it is waiting for a result.
    pub for_result: bool,
    /// The answer scripted for this capture, when one was scripted.
    pub scripted_result: Option<ScriptedResult>,
}

/// A scripted answer: what the capture target "returns" to a waiting activity.
#[derive(Debug, Clone, PartialEq)]
pub struct ScriptedResult {
    /// The capture this result answers.
    pub capture_seq: u64,
    /// `Activity.RESULT_OK` (-1) by default; any `Activity.RESULT_*` value.
    pub result_code: i32,
    /// The request code the result is delivered for.
    pub request_code: i32,
    /// `Intent.getAction()` of the result data (`null` = no data Intent).
    pub action: Option<String>,
    /// `Intent.getDataString()` of the result data (`content://…` for a pick).
    pub data: Option<String>,
    /// `Intent.getType()` of the result data.
    pub mime_type: Option<String>,
    /// String extras on the result data Intent.
    pub extras: BTreeMap<String, String>,
    /// When the launcher handed it to the guest (host wall clock, ms).
    pub delivered_utc_ms: Option<u64>,
}

impl ScriptedResult {
    /// The result as reply JSON (never a claim that a file exists).
    #[must_use]
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "capture_seq": self.capture_seq,
            "result_code": self.result_code,
            "request_code": self.request_code,
            "action": self.action,
            "data": self.data,
            "mime_type": self.mime_type,
            "extras": self.extras,
            "delivered": self.delivered_utc_ms.is_some(),
            "delivered_utc_ms": self.delivered_utc_ms,
        })
    }
}

impl CapturedIntent {
    /// The capture as reply JSON: exactly the payload the agent asserts on.
    #[must_use]
    pub fn to_json(&self) -> serde_json::Value {
        let extras: serde_json::Map<String, serde_json::Value> = self
            .extras
            .iter()
            .map(|(key, value)| {
                let value = match value {
                    Extra::String(text) => serde_json::json!(text),
                    Extra::Int(number) => serde_json::json!(number),
                    Extra::Long(number) => serde_json::json!(number),
                    Extra::Bool(flag) => serde_json::json!(flag),
                    // MODNOTIF3/MODBLUETOOTH: the extra kinds the modules added
                    // are rendered in the shape the Android API returns them in,
                    // never dropped from the capture.
                    Extra::Short(short) => serde_json::json!(short),
                    Extra::Strings(items) => serde_json::json!(items),
                    Extra::Ints(items) => serde_json::json!(items),
                    Extra::Bytes(bytes) => serde_json::json!(bytes),
                    Extra::ByteArrays(items) => serde_json::json!(items),
                    Extra::Bundle(nested) => serde_json::json!({
                        "bundle": nested
                            .entries
                            .iter()
                            .map(|(k, v)| (k.clone(), serde_json::json!(format!("{v:?}"))))
                            .collect::<serde_json::Map<String, serde_json::Value>>()
                    }),
                    Extra::Parcelable(parcelable) => {
                        serde_json::json!({"parcelable": format!("{parcelable:?}")})
                    }
                    Extra::Present => serde_json::json!({"present": true}),
                };
                (key.clone(), value)
            })
            .collect();
        serde_json::json!({
            "seq": self.seq,
            "utc_ms": self.utc_ms,
            "action": self.action,
            "data": self.data,
            "mime_type": self.mime_type,
            "categories": self.categories,
            "extras": extras,
            "component": self.component,
            "flags": self.flags,
            "request_code": self.request_code,
            "for_result": self.for_result,
            "handled_by": "captured",
            "scripted_result": self.scripted_result.as_ref().map(ScriptedResult::to_json),
        })
    }
}

/// Is this intent one the capture target stands in for?
///
/// Only the documented families, and only an implicit intent (an explicit
/// component that failed to resolve is the app's own bug and stays
/// `START_NOT_FOUND`):
///
/// | action | condition |
/// |---|---|
/// | `SEND` / `SEND_MULTIPLE` | a `text/*`, `image/*` or `*/*` type |
/// | `VIEW` | an `http`/`https` data URI |
/// | `PICK` / `GET_CONTENT` | a type (an image picker) |
///
/// Everything else is not captured. Returning `false` leaves the caller's
/// `START_NOT_FOUND`, so nothing is ever invented for an unknown action.
#[must_use]
pub fn capture_eligible(facts: &IntentFacts) -> bool {
    if facts.component.is_some() {
        return false;
    }
    let action = facts.action.as_deref().unwrap_or_default();
    let mime = facts.mime_type.as_deref().unwrap_or_default();
    match action {
        "android.intent.action.SEND" | "android.intent.action.SEND_MULTIPLE" => {
            mime.is_empty()
                || mime.starts_with("text/")
                || mime.starts_with("image/")
                || mime == "*/*"
        }
        "android.intent.action.VIEW" => match facts.data.as_deref() {
            Some(uri) => uri.starts_with("http://") || uri.starts_with("https://"),
            None => false,
        },
        "android.intent.action.PICK" | "android.intent.action.GET_CONTENT" => !mime.is_empty(),
        _ => false,
    }
}

/// The session's capture state (one per `ServiceHost`, shared by ATMS and the
/// broker service so `startActivity` can capture without a second table).
#[derive(Debug, Default)]
pub struct IntentState {
    captures: VecDeque<CapturedIntent>,
    next_seq: u64,
}

impl IntentState {
    /// Empty state.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Record one captured intent and return it. `None` when the intent is not
    /// one the capture target stands in for (see [`capture_eligible`]).
    pub fn capture(
        &mut self,
        facts: &IntentFacts,
        flags: i32,
        request_code: i32,
        for_result: bool,
        utc_ms: u64,
    ) -> Option<CapturedIntent> {
        if !capture_eligible(facts) {
            return None;
        }
        self.next_seq += 1;
        let record = CapturedIntent {
            seq: self.next_seq,
            utc_ms,
            action: facts.action.clone().unwrap_or_default(),
            data: facts.data.clone(),
            mime_type: facts.mime_type.clone(),
            categories: facts.categories.clone(),
            extras: facts.extras.clone(),
            component: capture_component(),
            flags,
            request_code,
            for_result,
            scripted_result: None,
        };
        if self.captures.len() >= MAX_CAPTURES {
            self.captures.pop_front();
        }
        self.captures.push_back(record.clone());
        Some(record)
    }

    /// The newest capture.
    #[must_use]
    pub fn last(&self) -> Option<&CapturedIntent> {
        self.captures.back()
    }

    /// Every capture, oldest first.
    #[must_use]
    pub fn list(&self) -> Vec<&CapturedIntent> {
        self.captures.iter().collect()
    }

    /// Drop every capture. Returns how many were dropped.
    pub fn clear(&mut self) -> usize {
        let n = self.captures.len();
        self.captures.clear();
        n
    }

    /// Script the answer for one capture (the newest by default).
    ///
    /// `result_code` defaults to `Activity.RESULT_OK` (-1) and `request_code` to
    /// the capture's own request code; `data` defaults to `None` (a result
    /// without data, which is what a cancel-like answer looks like).
    pub fn script_result(
        &mut self,
        capture_seq: Option<u64>,
        result_code: Option<i32>,
        data: Option<String>,
        mime_type: Option<String>,
        action: Option<String>,
        extras: BTreeMap<String, String>,
    ) -> Result<ScriptedResult> {
        let seq = capture_seq.or_else(|| self.captures.back().map(|c| c.seq));
        let Some(seq) = seq else {
            return Err(ServiceError::BadRequest {
                interface: DESCRIPTOR.into(),
                method: "result".into(),
                detail: "no captured intent to script a result for".into(),
            });
        };
        let Some(capture) = self.captures.iter_mut().find(|c| c.seq == seq) else {
            return Err(ServiceError::BadRequest {
                interface: DESCRIPTOR.into(),
                method: "result".into(),
                detail: format!("no captured intent with seq {seq}"),
            });
        };
        let result = ScriptedResult {
            capture_seq: seq,
            result_code: result_code.unwrap_or(-1),
            request_code: capture.request_code,
            action,
            data,
            mime_type,
            extras,
            delivered_utc_ms: None,
        };
        capture.scripted_result = Some(result.clone());
        Ok(result)
    }

    /// The scripted result for `seq` that has not been delivered to the guest.
    #[must_use]
    pub fn undelivered_result(&self, seq: u64) -> Option<&ScriptedResult> {
        self.captures
            .iter()
            .find(|c| c.seq == seq)
            .and_then(|c| c.scripted_result.as_ref())
            .filter(|r| r.delivered_utc_ms.is_none())
    }

    /// Mark `seq`'s scripted result delivered at `utc_ms`.
    pub fn mark_delivered(&mut self, seq: u64, utc_ms: u64) -> bool {
        if let Some(capture) = self.captures.iter_mut().find(|c| c.seq == seq) {
            if let Some(result) = capture.scripted_result.as_mut() {
                result.delivered_utc_ms = Some(utc_ms);
                return true;
            }
        }
        false
    }

    /// The newest capture that is still waiting for a scripted result.
    #[must_use]
    pub fn last_pending_result(&self) -> Option<&ScriptedResult> {
        self.captures
            .iter()
            .rev()
            .filter_map(|c| c.scripted_result.as_ref())
            .find(|r| r.delivered_utc_ms.is_none())
    }
}

impl ScriptedResult {
    /// Mark this result delivered (used by the launcher once the guest acked).
    pub fn mark_delivered(&mut self, utc_ms: u64) {
        self.delivered_utc_ms = Some(utc_ms);
    }
}

/// The broker service object.
///
/// It owns the shared state and answers nothing over binder: an app cannot
/// talk to the capture target (it has no process and no API surface). The
/// agent reads and scripts it through the session control op, which calls the
/// Rust methods below — that is what keeps "captured" an observation, never an
/// app-visible fake app.
pub struct IntentBrokerService {
    state: Arc<Mutex<IntentState>>,
}

impl IntentBrokerService {
    /// New broker over `state`.
    #[must_use]
    pub fn new(state: Arc<Mutex<IntentState>>) -> Self {
        Self { state }
    }

    /// The shared capture state.
    #[must_use]
    pub fn state(&self) -> &Arc<Mutex<IntentState>> {
        &self.state
    }
}

impl ServiceObject for IntentBrokerService {
    fn call(
        &mut self,
        interface: &str,
        method: &MethodInfo,
        _args: &[Value],
    ) -> Result<CallResult> {
        // No AIDL surface: `descriptor` has no generated table, and no guest
        // code calls it. A transaction that reaches here is a structured
        // unsupported answer, never a fabricated success.
        Err(ServiceError::UnsupportedAndroidApi {
            interface: interface.to_string(),
            code: method.code,
            method: method.name.clone(),
        })
    }
}

/// The host wall clock in ms since the epoch (the capture/result timestamps).
#[must_use]
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

/// A `Parcel`-free helper for tests: the reply value of `op: "last"`.
#[must_use]
pub fn last_reply(state: &IntentState) -> serde_json::Value {
    match state.last() {
        Some(capture) => capture.to_json(),
        None => serde_json::Value::Null,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn send_text() -> IntentFacts {
        let mut extras = BTreeMap::new();
        extras.insert(
            "android.intent.extra.TEXT".to_string(),
            Extra::String("hello".into()),
        );
        IntentFacts {
            action: Some("android.intent.action.SEND".into()),
            mime_type: Some("text/plain".into()),
            extras,
            ..IntentFacts::default()
        }
    }

    #[test]
    fn a_share_is_captured_with_its_payload_and_reported_as_captured() {
        let mut state = IntentState::new();
        let captured = state
            .capture(&send_text(), 0x1000_0000, 0, false, 42)
            .expect("a SEND text/plain is capture-eligible");
        assert_eq!(captured.action, "android.intent.action.SEND");
        assert_eq!(captured.mime_type.as_deref(), Some("text/plain"));
        assert_eq!(captured.component, "com.acos.capture/.Capture");
        assert_eq!(captured.flags, 0x1000_0000);
        assert_eq!(
            captured.extras.get("android.intent.extra.TEXT"),
            Some(&Extra::String("hello".into()))
        );
        let json = captured.to_json();
        assert_eq!(json["handled_by"], "captured");
        assert_eq!(state.last().map(|c| c.seq), Some(1));
    }

    #[test]
    fn a_view_https_is_captured_but_a_typo_component_is_not() {
        let mut state = IntentState::new();
        let view = IntentFacts {
            action: Some("android.intent.action.VIEW".into()),
            data: Some("https://example.com/x".into()),
            ..IntentFacts::default()
        };
        assert!(state.capture(&view, 0, 0, false, 1).is_some());
        // An explicit component that failed to resolve is the app's bug: never
        // captured (the caller must see START_NOT_FOUND).
        let explicit = IntentFacts::explicit("dev.acos.nope", "dev.acos.nope.Main");
        assert!(state.capture(&explicit, 0, 0, false, 2).is_none());
        // An unknown action is not a share/pick/view: never captured.
        let unknown = IntentFacts {
            action: Some("dev.acos.FROBNICATE".into()),
            ..IntentFacts::default()
        };
        assert!(state.capture(&unknown, 0, 0, false, 3).is_none());
    }

    #[test]
    fn a_scripted_pick_result_round_trips_and_is_delivered_once() {
        let mut state = IntentState::new();
        let captured = state
            .capture(
                &IntentFacts {
                    action: Some("android.intent.action.GET_CONTENT".into()),
                    mime_type: Some("image/*".into()),
                    ..IntentFacts::default()
                },
                0,
                7,
                true,
                10,
            )
            .expect("a picker intent is capture-eligible");
        assert!(captured.for_result);
        let result = state
            .script_result(
                None,
                None,
                Some("content://com.acos.capture.images/pick.png".into()),
                Some("image/png".into()),
                Some("android.intent.action.SEND".into()),
                BTreeMap::new(),
            )
            .expect("scripting a result for the capture succeeds");
        assert_eq!(result.result_code, -1, "RESULT_OK by default");
        assert_eq!(result.request_code, 7, "the capture's own request code");
        assert!(state.undelivered_result(captured.seq).is_some());
        assert!(state.mark_delivered(captured.seq, 99));
        assert!(state.undelivered_result(captured.seq).is_none());
        assert!(state.last_pending_result().is_none());
        let json = state.last().expect("capture").to_json();
        assert_eq!(json["scripted_result"]["delivered"], true);
        assert_eq!(
            json["scripted_result"]["data"],
            "content://com.acos.capture.images/pick.png"
        );
    }

    #[test]
    fn scripting_without_a_capture_is_a_structured_error() {
        let mut state = IntentState::new();
        let err = state
            .script_result(None, None, None, None, None, BTreeMap::new())
            .expect_err("nothing captured yet");
        assert!(matches!(err, ServiceError::BadRequest { .. }));
    }
}
