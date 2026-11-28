//! METRO4/METRO6: the React Native dev-loop error records an agent reads.
//!
//! A redbox is data, not a picture: [`JsError`] is the JS error React Native
//! reported (name, message, frames, component stack and the log lines it came
//! from), and [`BundleUnreachable`] is the *other* dev-loop failure — the app
//! never got a bundle to run, so there is no JS error at all. The two are never
//! merged: "Metro is down" and "your code threw" have different fixes.
//!
//! `symbolicated` is honest: `true` only when every frame position was mapped to
//! a source position (by Metro's `POST /symbolicate`, a source map, or because
//! the error is already source-level, a Metro transform error). Otherwise it is
//! `false` with a `symbolication_reason`.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// One JS stack frame.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct JsFrame {
    /// Source file (after symbolication) or the bundle URL / file id (before).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    /// 1-based line.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    /// Column as React Native reported it (the same convention Metro's
    /// `/symbolicate` consumes).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub column: Option<u32>,
    /// Function / method name (`fn`), when known.
    #[serde(rename = "fn", default, skip_serializing_if = "Option::is_none")]
    pub function: Option<String>,
}

/// A JS error React Native reported (redbox / LogBox / `ExceptionsManager`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct JsError {
    /// Error class (`TypeError`, `SyntaxError`, `ReferenceError`, `Error`, ...).
    pub name: String,
    /// The message without the `Name:` prefix and RN's `, js engine: ...` suffix.
    pub message: String,
    /// Stack frames, innermost first.
    #[serde(default)]
    pub frames: Vec<JsFrame>,
    /// React's component stack (`in App`, `at Counter (App.js:12)`), when reported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub component_stack: Option<String>,
    /// `fatal` (redbox, `reportFatalException`, LogBox "Uncaught Error" /
    /// "Render Error"), `soft` (`reportSoftException` / `console.error`, or a
    /// logged line that does not say which) or `transform` (Metro could not
    /// build the bundle).
    pub kind: String,
    /// Where it was read: `log` (guest logcat; the dialog on screen may add
    /// frames and kind) or `ui` (only the redbox / LogBox on screen).
    pub source: String,
    /// The log lines the error was parsed from (bounded).
    #[serde(default)]
    pub log_tail: Vec<String>,
    /// Every frame carries a source position an agent can edit.
    pub symbolicated: bool,
    /// Why `symbolicated` is false (`no metro configured`, `symbolicate_unreachable`,
    /// `no map`, ...), or how it became true (`metro /symbolicate`, `source map`,
    /// `source map (cached)`, `metro transform error`, `react native logbox`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub symbolication_reason: Option<String>,
}

/// The app never got a JS bundle: Metro was unreachable or refused it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct BundleUnreachable {
    /// The bundle URL the app asked for, when the log names it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// The low-level reason (`ECONNREFUSED (Connection refused)`,
    /// `Failed to connect to localhost/127.0.0.1:8081`, ...).
    pub error: String,
    /// The log lines it was read from (bounded).
    #[serde(default)]
    pub log_tail: Vec<String>,
}

/// What the dev loop looks like right now (`verdict.json` `rn` block and the
/// live `acos_logs` reply).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RnDevState {
    /// The most recent JS error, when one was reported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub js_error: Option<JsError>,
    /// The most recent bundle-fetch failure, when one was reported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bundle_unreachable: Option<BundleUnreachable>,
}

impl RnDevState {
    /// Nothing was reported.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.js_error.is_none() && self.bundle_unreachable.is_none()
    }
}
