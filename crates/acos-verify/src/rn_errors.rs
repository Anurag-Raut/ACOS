//! METRO4: React Native dev-loop errors as data.
//!
//! React Native reports a JS error through a handful of exact shapes (all from
//! `react-native@0.81.4`, `ReactAndroid/src/main/java/com/facebook/react`):
//!
//! - `DevSupportManagerBase.logJSException`: tag `ReactNative`,
//!   `Exception in native call from JS` + a `JavascriptException` whose message
//!   is `JSStackTrace.format`: `<message>, stack:\n<fn>@<line>:<col>\n...`
//!   (`fn@N.js:line:col` for a RAM bundle).
//! - `ExceptionsManagerModule.reportException` (soft): the same format on tag
//!   `ReactNative` without the exception wrapper.
//! - `console.error` / LogBox: tag `ReactNativeJS`, an `Error.stack`-style text
//!   (`TypeError: msg\n    at fn (url:line:col)` / Hermes `fn@url:line:col`),
//!   with React's `This error is located at:` component stack.
//! - The bundle fetch (`DevSupportManagerBase.fetchSplitBundleAndCreateBundleLoader`
//!   / `BundleDownloader`): `Unable to download JS bundle` + a
//!   `DebugServerException`. Either Metro answered with a build error (its JSON
//!   names `filename:lineNumber:column`, so it is a **source-level**
//!   `SyntaxError`/`TransformError`) or it never answered (`Could not connect to
//!   development server.` / `catalyst_reload_error` + `Caused by:
//!   java.net.ConnectException ... ECONNREFUSED`), which is `bundle_unreachable`.
//! - A structured redbox payload (`{message, stack:[{file, lineNumber, column,
//!   methodName}], name, componentStack, id}`) printed as JSON.
//!
//! The parser is pure, runs only over log records an agent or verdict asks
//! about, and returns nothing for an app that never logged on a React Native
//! tag — a non-RN or release app pays nothing and is never labelled.

use std::sync::OnceLock;

use acos_protocol::rn::{BundleUnreachable, JsError, JsFrame, RnDevState};
use regex::Regex;

use crate::backend::LogRecord;
use acos_protocol::LogLevel;

/// The most log lines one record keeps as `log_tail`.
const LOG_TAIL_LINES: usize = 40;

/// The catalyst_reload_error string (`res/devsupport/values/strings.xml`).
const CATALYST_RELOAD_ERROR: &str = "Failed to load bundle. Try restarting the bundler";

/// Parse a run's (or a slice's) log records into the dev-loop state: the
/// **last** JS error and the **last** bundle-fetch failure.
#[must_use]
pub fn parse_logs(records: &[LogRecord]) -> RnDevState {
    parse_logs_ordered(records).0
}

/// [`parse_logs`] plus which of the two came **last** (`true` = the bundle
/// failure is the most recent dev-loop event).
#[must_use]
pub fn parse_logs_ordered(records: &[LogRecord]) -> (RnDevState, bool) {
    let mut state = RnDevState::default();
    let mut unreachable_last = false;
    for block in blocks(records) {
        match classify(&block) {
            Some(Event::Js(error)) => {
                state.js_error = Some(error);
                unreachable_last = false;
            }
            Some(Event::Unreachable(failure)) => {
                state.bundle_unreachable = Some(failure);
                unreachable_last = true;
            }
            None => {}
        }
    }
    (state, unreachable_last)
}

/// The dev-server port a debug app is using, read from the bundle URLs it
/// logged (`http://localhost:8081/index.bundle?...`). `None`: the log never
/// named one, so nothing is guessed.
#[must_use]
pub fn metro_port_hint(records: &[LogRecord]) -> Option<u16> {
    static PORT: OnceLock<Regex> = OnceLock::new();
    let pattern = re(
        &PORT,
        r"https?://(?:localhost|127\.0\.0\.1):(\d{2,5})/[^\s]*\.(?:bundle|map)",
    );
    records
        .iter()
        .rev()
        .filter(|record| is_rn_tag(&record.tag))
        .find_map(|record| {
            pattern
                .captures(&record.message)
                .and_then(|c| c[1].parse::<u16>().ok())
        })
}

/// Whether an `assert_js_error` wait can stop on this (un-symbolicated) slice.
///
/// It stops when the assertion already holds; on a bundle failure for a
/// JS-error assertion (a distinct terminal verdict); and when the latest JS
/// error matches every constraint that does not need symbolication
/// (`file`/`line`/`symbolicated` are judged after the step symbolicates).
/// `none: true` and `bundle_unreachable: true` wait until they hold.
#[must_use]
pub fn wait_settled(
    assertion: &acos_protocol::scenario::JsErrorAssertion,
    state: &RnDevState,
    unreachable_last: bool,
) -> bool {
    match check(assertion, state, unreachable_last) {
        JsCheck::Pass(_) => true,
        _ if assertion.none == Some(true) || assertion.bundle_unreachable == Some(true) => false,
        JsCheck::Unreachable(_) => true,
        JsCheck::Fail(_) => {
            let needs_mapping = assertion.file.is_some() || assertion.symbolicated == Some(true);
            let unmapped = state.js_error.as_ref().is_some_and(|error| !error.symbolicated);
            if !(needs_mapping && unmapped) {
                return false;
            }
            let relaxed = acos_protocol::scenario::JsErrorAssertion {
                file: None,
                line: None,
                symbolicated: None,
                ..assertion.clone()
            };
            let has_other = relaxed.name.is_some() || relaxed.message.is_some();
            // nothing else to match: any unmapped error may become the frame asked for
            !has_other || matches!(check(&relaxed, state, unreachable_last), JsCheck::Pass(_))
        }
    }
}

/// The outcome of an `assert_js_error` check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JsCheck {
    /// The assertion holds.
    Pass(String),
    /// A JS-error constraint did not hold (`ASSERTION_FAILED`).
    Fail(String),
    /// The bundle never loaded, so there is no JS error to judge (`BUNDLE_UNREACHABLE`).
    Unreachable(String),
}

/// Judge `assertion` against the dev-loop state in scope.
///
/// `unreachable_last` says whether the bundle failure is more recent than the
/// JS error (a JS error from before a failed reload is not "on screen").
#[must_use]
pub fn check(
    assertion: &acos_protocol::scenario::JsErrorAssertion,
    state: &RnDevState,
    unreachable_last: bool,
) -> JsCheck {
    if assertion.none == Some(true) {
        return match (&state.js_error, &state.bundle_unreachable) {
            (None, None) => JsCheck::Pass("no JS error and no bundle failure in scope".to_string()),
            (_, Some(failure)) if unreachable_last || state.js_error.is_none() => JsCheck::Unreachable(format!(
                "the bundle could not be fetched ({}); the app shows the bundle error, not a cleared screen",
                failure.error
            )),
            (Some(error), _) => JsCheck::Fail(format!(
                "a JS error is still reported: {}: {}",
                error.name, error.message
            )),
            (None, Some(_)) => unreachable!("covered above"),
        };
    }
    if assertion.bundle_unreachable == Some(true) {
        return match &state.bundle_unreachable {
            Some(failure) if unreachable_last || state.js_error.is_none() => JsCheck::Pass(format!(
                "bundle unreachable: {} ({})",
                failure.url.as_deref().unwrap_or("<no url>"),
                failure.error
            )),
            Some(_) => JsCheck::Fail(
                "the bundle failure is older than the latest JS error (the bundle loaded and then threw)".to_string(),
            ),
            None => JsCheck::Fail("no bundle-fetch failure was reported".to_string()),
        };
    }
    if let Some(failure) = &state.bundle_unreachable {
        if unreachable_last || state.js_error.is_none() {
            return JsCheck::Unreachable(format!(
                "no JS ran: the bundle could not be fetched from {} ({})",
                failure.url.as_deref().unwrap_or("the dev server"),
                failure.error
            ));
        }
    }
    let Some(error) = &state.js_error else {
        return JsCheck::Fail("no JS error was reported in scope".to_string());
    };
    let mut problems = Vec::new();
    if let Some(name) = &assertion.name {
        if &error.name != name {
            problems.push(format!("name is `{}`, expected `{name}`", error.name));
        }
    }
    if let Some(message) = &assertion.message {
        if !error.message.contains(message.as_str()) {
            problems.push(format!(
                "message `{}` does not contain `{message}`",
                error.message
            ));
        }
    }
    if let Some(file) = &assertion.file {
        let hit = error.frames.iter().any(|frame| {
            frame
                .file
                .as_deref()
                .is_some_and(|f| f.ends_with(file.as_str()))
                && assertion.line.is_none_or(|line| frame.line == Some(line))
        });
        if !hit {
            let seen: Vec<String> = error
                .frames
                .iter()
                .map(|frame| {
                    format!(
                        "{}:{}",
                        frame.file.as_deref().unwrap_or("?"),
                        frame.line.map_or("?".to_string(), |l| l.to_string())
                    )
                })
                .collect();
            problems.push(format!(
                "no frame at `{file}{}` (frames: [{}]{})",
                assertion.line.map(|l| format!(":{l}")).unwrap_or_default(),
                seen.join(", "),
                if error.symbolicated {
                    String::new()
                } else {
                    format!(
                        "; not symbolicated: {}",
                        error.symbolication_reason.as_deref().unwrap_or("unknown")
                    )
                }
            ));
        }
    }
    if let Some(symbolicated) = assertion.symbolicated {
        if error.symbolicated != symbolicated {
            problems.push(format!(
                "symbolicated is {}, expected {symbolicated} ({})",
                error.symbolicated,
                error.symbolication_reason.as_deref().unwrap_or("")
            ));
        }
    }
    if problems.is_empty() {
        JsCheck::Pass(format!("{}: {}", error.name, error.message))
    } else {
        JsCheck::Fail(problems.join("; "))
    }
}

/// One UI node as the redbox parser needs it: `(resource_id, text)`, in tree order.
pub type UiText<'a> = (&'a str, &'a str);

/// The redbox's own view ids (`res/devsupport/layout/redbox_*.xml`).
fn has_id(resource_id: &str, id: &str) -> bool {
    resource_id == id || resource_id.ends_with(&format!(":id/{id}")) || resource_id.ends_with(&format!("/{id}"))
}

/// Parse the redbox dialog itself from the UI tree, in tree order.
///
/// Only React Native's `RedBoxContentView` counts: it is recognised by its own
/// view ids (`rn_redbox_stack`, `catalyst_redbox_title`,
/// `rn_redbox_dismiss_button` + `rn_redbox_reload_button`), never by labels —
/// an app screen with an "Error: ..." text and its own Dismiss/Reload buttons
/// is not a redbox. The title is `catalyst_redbox_title`; every frame is an
/// `rn_frame_method` row followed by its `rn_frame_file` row
/// (`StackTraceHelper.formatFrameSource`: `file[:line[:column]]`).
#[must_use]
pub fn parse_ui(nodes: &[UiText<'_>]) -> RnDevState {
    let mut state = RnDevState::default();
    let any = |id: &str| nodes.iter().any(|(rid, _)| has_id(rid, id));
    let redbox = any("rn_redbox_stack")
        || any("catalyst_redbox_title")
        || (any("rn_redbox_dismiss_button") && any("rn_redbox_reload_button"));
    if !redbox {
        return state;
    }
    let texts: Vec<String> = nodes
        .iter()
        .map(|(_, text)| text.trim().to_string())
        .filter(|text| !text.is_empty())
        .collect();
    if let Some(error) = unreachable_from_text(&texts.join("\n")) {
        state.bundle_unreachable = Some(error);
        return state;
    }
    let title = nodes
        .iter()
        .find(|(rid, _)| has_id(rid, "catalyst_redbox_title"))
        .map(|(_, text)| text.trim().to_string())
        .or_else(|| texts.iter().find(|text| split_name(text).is_some()).cloned());
    let Some(title) = title else {
        return state;
    };
    let first_line = title.lines().next().unwrap_or("").trim().to_string();
    let (name, message) = split_name(&first_line).unwrap_or(("Error".to_string(), first_line));
    let mut frames = Vec::new();
    let mut pending_fn: Option<String> = None;
    for (rid, text) in nodes {
        if has_id(rid, "rn_frame_method") {
            pending_fn = Some(text.trim().to_string()).filter(|t| !t.is_empty());
        } else if has_id(rid, "rn_frame_file") {
            let mut parts: Vec<&str> = text.trim().rsplitn(3, ':').collect();
            parts.reverse();
            let (file, line, column) = match parts.as_slice() {
                [file, line, col] if line.parse::<u32>().is_ok() => {
                    (file.to_string(), line.parse().ok(), col.parse().ok())
                }
                [file, line] if line.parse::<u32>().is_ok() => (file.to_string(), line.parse().ok(), None),
                _ => (text.trim().to_string(), None, None),
            };
            frames.push(JsFrame {
                file: Some(file).filter(|f| !f.is_empty()),
                line,
                column,
                function: pending_fn.take(),
            });
        }
    }
    let symbolicated = frames_are_source(&frames);
    state.js_error = Some(JsError {
        name,
        message,
        frames,
        component_stack: None,
        kind: "fatal".to_string(),
        source: "ui".to_string(),
        log_tail: texts.into_iter().take(LOG_TAIL_LINES).collect(),
        symbolicated,
        symbolication_reason: (!symbolicated).then(|| "not symbolicated yet".to_string()),
    });
    state
}

/// One message (or a run of continuation lines on the same tag).
struct Block {
    tag: String,
    level: LogLevel,
    text: String,
}

enum Event {
    Js(JsError),
    Unreachable(BundleUnreachable),
}

fn is_rn_tag(tag: &str) -> bool {
    tag.starts_with("ReactNative") || tag == "AndroidRuntime" || tag.starts_with("unknown:")
}

/// Group records into messages: a record that is a continuation of the
/// previous one (same tag, starts with whitespace / `at ` / `Caused by:` / a
/// `fn@line:col` frame) is joined to it, so a logcat that split a throwable
/// across records parses like one that did not.
fn blocks(records: &[LogRecord]) -> Vec<Block> {
    let mut out: Vec<Block> = Vec::new();
    for record in records {
        if !is_rn_tag(&record.tag) {
            continue;
        }
        // A new JS error header always starts a block, even indented: it must
        // never be glued onto the previous (say, Info) message and dropped
        // with it. Indented frames below it still attach to it.
        let header = split_name(record.message.lines().next().unwrap_or("").trim()).is_some();
        let continuation = !header && out.last().is_some_and(|last: &Block| {
            last.tag == record.tag
                && last.level == record.level
                && (record.message.starts_with(char::is_whitespace)
                    || record.message.starts_with("Caused by:")
                    || record.message.starts_with("at ")
                    || frame_line(&record.message).is_some()
                    || record.message.starts_with("This error is located at")
                    || (last.text.ends_with(", stack:")))
        });
        if continuation {
            if let Some(last) = out.last_mut() {
                last.text.push('\n');
                last.text.push_str(&record.message);
                continue;
            }
        }
        out.push(Block {
            tag: record.tag.clone(),
            level: record.level,
            text: record.message.clone(),
        });
    }
    out
}

fn classify(block: &Block) -> Option<Event> {
    if !matches!(block.level, LogLevel::Error) {
        return None;
    }
    let text = block.text.as_str();
    if block.tag == "AndroidRuntime" && !text.contains("JavascriptException") {
        return None;
    }
    // `bundle_unreachable` is the **native** loader's diagnosis (DevSupport,
    // BundleDownloader, CatalystInstance on `ReactNative*` native tags). A JS
    // exception whose message happens to say "Unable to load script" is a JS
    // error, not a missing bundle.
    let js_exception = block.tag.starts_with("ReactNativeJS")
        || text.contains("JavascriptException")
        || text.contains(", stack:");
    if !js_exception {
        if let Some(failure) = unreachable_from_text(text) {
            return Some(Event::Unreachable(failure));
        }
    }
    if let Some(error) = transform_error(text) {
        return Some(Event::Js(error));
    }
    if let Some(error) = json_payload(text) {
        return Some(Event::Js(error));
    }
    if let Some(error) = js_stack_trace_format(text) {
        return Some(Event::Js(error));
    }
    if block.tag.starts_with("ReactNativeJS") {
        if let Some(error) = error_stack_text(text) {
            return Some(Event::Js(error));
        }
    }
    None
}

fn tail(text: &str) -> Vec<String> {
    text.lines()
        .take(LOG_TAIL_LINES)
        .map(str::to_string)
        .collect()
}

fn re(cell: &'static OnceLock<Regex>, pattern: &str) -> &'static Regex {
    cell.get_or_init(|| Regex::new(pattern).expect("static regex"))
}

static URL: OnceLock<Regex> = OnceLock::new();
static CAUSE: OnceLock<Regex> = OnceLock::new();
static ERRNO: OnceLock<Regex> = OnceLock::new();
static FRAME_AT: OnceLock<Regex> = OnceLock::new();
static FRAME_AT_BARE: OnceLock<Regex> = OnceLock::new();
static FRAME_AT_SIGN: OnceLock<Regex> = OnceLock::new();
static NAME: OnceLock<Regex> = OnceLock::new();
static TRANSFORM_AT: OnceLock<Regex> = OnceLock::new();
static ENGINE_SUFFIX: OnceLock<Regex> = OnceLock::new();

/// `bundle_unreachable`: RN never got a bundle (no answer from Metro).
fn unreachable_from_text(text: &str) -> Option<BundleUnreachable> {
    let unreachable = text.contains("Could not connect to development server")
        || text.contains("Unable to load script")
        || text.contains(CATALYST_RELOAD_ERROR)
        || text.contains("catalyst_reload_error")
        || (text.contains("Unable to download JS bundle")
            && (text.contains("ConnectException")
                || text.contains("ECONNREFUSED")
                || text.contains("UnknownHostException")
                || text.contains("SocketTimeoutException")));
    if !unreachable {
        return None;
    }
    let url = re(&URL, r#"https?://[^\s"'<>]+"#)
        .find_iter(text)
        .map(|m| m.as_str().trim_end_matches(['.', ',', ')']).to_string())
        .find(|url| url.contains(".bundle") || url.contains("/status"))
        .or_else(|| {
            re(&URL, r#"https?://[^\s"'<>]+"#)
                .find(text)
                .map(|m| m.as_str().trim_end_matches(['.', ',', ')']).to_string())
        });
    let errno = re(&ERRNO, r"(E[A-Z]{3,}[A-Z]*) \(([^)]*)\)")
        .captures_iter(text)
        .last()
        .map(|c| format!("{} ({})", &c[1], &c[2]));
    let cause = re(&CAUSE, r"(?m)^\s*Caused by: [\w.$]+(?:: (.*))?$")
        .captures_iter(text)
        .last()
        .and_then(|c| c.get(1).map(|m| m.as_str().trim().to_string()))
        .filter(|cause| !cause.is_empty());
    let error = errno
        .or(cause)
        .or_else(|| {
            text.lines()
                .find(|line| {
                    line.contains("Could not connect")
                        || line.contains("Unable to load script")
                        || line.contains("Failed to load bundle")
                })
                .map(|line| line.trim().to_string())
        })
        .unwrap_or_else(|| "the bundle could not be fetched".to_string());
    Some(BundleUnreachable {
        url,
        error,
        log_tail: tail(text),
    })
}

/// A Metro build error the app received (`DebugServerException.parse`: the
/// message ends with `\n  at <file>:<line>:<column>`): already source-level.
fn transform_error(text: &str) -> Option<JsError> {
    if !text.contains("DebugServerException") && !text.contains("Unable to download JS bundle") {
        return None;
    }
    let start = text
        .find("DebugServerException: ")
        .map(|i| i + "DebugServerException: ".len())
        .unwrap_or(0);
    let body = &text[start..];
    let captures = re(&TRANSFORM_AT, r"(?m)^\s+at ([^\s:]+):(\d+):(\d+)\s*$").captures(body)?;
    let first = body.lines().next().unwrap_or("").trim();
    let (name, message) =
        split_name(first).unwrap_or(("SyntaxError".to_string(), first.to_string()));
    let frame = JsFrame {
        file: Some(captures[1].to_string()),
        line: captures[2].parse().ok(),
        column: captures[3].parse().ok(),
        function: None,
    };
    Some(JsError {
        name,
        message,
        frames: vec![frame],
        component_stack: None,
        kind: "transform".to_string(),
        source: "log".to_string(),
        log_tail: tail(text),
        symbolicated: true,
        symbolication_reason: Some("metro transform error".to_string()),
    })
}

/// A structured redbox payload printed as JSON.
fn json_payload(text: &str) -> Option<JsError> {
    let start = text.find('{')?;
    let end = text.rfind('}')?;
    if end <= start {
        return None;
    }
    let value: serde_json::Value = serde_json::from_str(&text[start..=end]).ok()?;
    let message = value.get("message")?.as_str()?;
    let stack = value.get("stack")?.as_array()?;
    let frames: Vec<JsFrame> = stack
        .iter()
        .map(|frame| JsFrame {
            file: frame
                .get("file")
                .and_then(|v| v.as_str())
                .map(str::to_string),
            line: frame
                .get("lineNumber")
                .and_then(serde_json::Value::as_u64)
                .and_then(|v| u32::try_from(v).ok()),
            column: frame
                .get("column")
                .and_then(serde_json::Value::as_u64)
                .and_then(|v| u32::try_from(v).ok()),
            function: frame
                .get("methodName")
                .and_then(|v| v.as_str())
                .map(str::to_string),
        })
        .collect();
    let (name, message) = match value.get("name").and_then(|v| v.as_str()) {
        Some(name) => (
            name.to_string(),
            strip_engine(
                message
                    .strip_prefix(&format!("{name}: "))
                    .unwrap_or(message),
            ),
        ),
        None => split_name(message).unwrap_or(("Error".to_string(), strip_engine(message))),
    };
    let fatal = value
        .get("isFatal")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(true);
    Some(unsymbolicated(JsError {
        name,
        message,
        frames,
        component_stack: value
            .get("componentStack")
            .and_then(|v| v.as_str())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty()),
        kind: if fatal { "fatal" } else { "soft" }.to_string(),
        source: "log".to_string(),
        log_tail: tail(text),
        symbolicated: false,
        symbolication_reason: None,
    }))
}

/// `JSStackTrace.format`: `<message>, stack:\n<fn>@[<N.js>:]<line>[:<col>]`.
fn js_stack_trace_format(text: &str) -> Option<JsError> {
    let marker = text.find(", stack:")?;
    let head = &text[..marker];
    let fatal = text.contains("JavascriptException");
    let message_start = head
        .rfind("JavascriptException: ")
        .map(|i| i + "JavascriptException: ".len())
        .or_else(|| head.rfind('\n').map(|i| i + 1))
        .unwrap_or(0);
    let raw = head[message_start..].trim();
    let mut frames = Vec::new();
    for line in text[marker + ", stack:".len()..].lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        match frame_line(line) {
            Some(frame) => frames.push(frame),
            None => break,
        }
    }
    let (name, message) = split_name(raw).unwrap_or(("Error".to_string(), strip_engine(raw)));
    Some(unsymbolicated(JsError {
        name,
        message,
        frames,
        component_stack: component_stack(text),
        kind: if fatal { "fatal" } else { "soft" }.to_string(),
        source: "log".to_string(),
        log_tail: tail(text),
        symbolicated: false,
        symbolication_reason: None,
    }))
}

/// An `Error.stack`-style text on `ReactNativeJS` (`console.error`, LogBox).
fn error_stack_text(text: &str) -> Option<JsError> {
    let mut lines = text.lines();
    let first = lines.next()?.trim();
    let (name, message) = split_name(first)?;
    let mut frames = Vec::new();
    for line in lines {
        let trimmed = line.trim();
        if trimmed.starts_with("This error is located at") {
            break;
        }
        if let Some(frame) = frame_line(trimmed) {
            frames.push(frame);
        }
    }
    Some(unsymbolicated(JsError {
        name,
        message,
        frames,
        component_stack: component_stack(text),
        kind: "soft".to_string(),
        source: "log".to_string(),
        log_tail: tail(text),
        symbolicated: false,
        symbolication_reason: None,
    }))
}

/// One frame line in any of the shapes RN / Hermes / V8 print.
fn frame_line(line: &str) -> Option<JsFrame> {
    let line = line.trim();
    let location = |loc: &str| -> Option<(Option<String>, u32, Option<u32>)> {
        // `address at <url>:<line>:<col>` is Hermes' bytecode form.
        let loc = loc.strip_prefix("address at ").unwrap_or(loc);
        let mut parts = loc.rsplitn(3, ':');
        let last = parts.next()?;
        let middle = parts.next();
        let rest = parts.next();
        match (middle, rest) {
            (Some(line), Some(file)) => Some((
                Some(file.to_string()),
                line.parse().ok()?,
                Some(last.parse().ok()?),
            )),
            (Some(line), None) => Some((None, line.parse().ok()?, last.parse().ok())),
            _ => None,
        }
    };
    if let Some(c) = re(&FRAME_AT, r"^at (?P<fn>.+?) \((?P<loc>.+)\)$").captures(line) {
        let (file, l, col) = location(&c["loc"])?;
        return Some(JsFrame {
            file,
            line: Some(l),
            column: col,
            function: Some(c["fn"].to_string()),
        });
    }
    if let Some(c) = re(&FRAME_AT_BARE, r"^at (?P<loc>\S+:\d+:\d+)$").captures(line) {
        let (file, l, col) = location(&c["loc"])?;
        return Some(JsFrame {
            file,
            line: Some(l),
            column: col,
            function: None,
        });
    }
    if let Some(c) = re(
        &FRAME_AT_SIGN,
        r"^(?P<fn>[^@\s]*)@(?P<loc>\S*?-?\d+(?::\d+)?)$",
    )
    .captures(line)
    {
        let loc = &c["loc"];
        let function = Some(c["fn"].to_string()).filter(|f| !f.is_empty());
        // `fn@-1` / `fn@12:34` (JSStackTrace.format: no file) / `fn@1.js:12:34` / `fn@url:12:34`
        let mut parts: Vec<&str> = loc.rsplitn(3, ':').collect();
        parts.reverse();
        let (file, line_no, column) = match parts.as_slice() {
            [line] => (None, line.parse::<i64>().ok()?, None),
            [a, b] if a.ends_with(".js") => (Some(a.to_string()), b.parse::<i64>().ok()?, None),
            [line, col] => (None, line.parse::<i64>().ok()?, col.parse::<u32>().ok()),
            [file, line, col] => (
                Some(file.to_string()),
                line.parse::<i64>().ok()?,
                col.parse::<u32>().ok(),
            ),
            _ => return None,
        };
        return Some(JsFrame {
            file,
            line: u32::try_from(line_no).ok(),
            column,
            function,
        });
    }
    None
}

fn component_stack(text: &str) -> Option<String> {
    let start = text.find("This error is located at:")?;
    let body: Vec<&str> = text[start + "This error is located at:".len()..]
        .lines()
        .map(str::trim)
        .filter(|line| line.starts_with("in ") || line.starts_with("at "))
        .collect();
    (!body.is_empty()).then(|| body.join("\n"))
}

/// `TypeError: msg` -> (`TypeError`, `msg`), with RN's engine suffix stripped.
fn split_name(text: &str) -> Option<(String, String)> {
    let c = re(&NAME, r"^(?P<name>[A-Za-z_$][\w$]*(?:Error|Exception)|Error|Invariant Violation)(?:: (?P<msg>[\s\S]*))?$")
        .captures(text)?;
    let name = c["name"].to_string();
    let message = c.name("msg").map(|m| m.as_str()).unwrap_or("");
    Some((
        name,
        strip_engine(message.lines().next().unwrap_or("").trim()),
    ))
}

fn strip_engine(message: &str) -> String {
    re(&ENGINE_SUFFIX, r", js engine: \w+$")
        .replace(message.trim(), "")
        .to_string()
}

/// `JSStackTrace.kt`'s generated frame ids — a RAM-bundle module (`1.js`), a
/// segment (`seg-1.js`) or a RAM segment module (`seg-1_2.js`). They name a
/// position in *generated* code, never a source file an agent can edit.
#[must_use]
pub fn is_generated_frame_id(file: &str) -> bool {
    static GENERATED: OnceLock<Regex> = OnceLock::new();
    re(&GENERATED, r"^(?:seg-\d+(?:_\d+)?|\d+)\.js$").is_match(file)
}

/// A frame is a **source** position when it has a line and its file is a real
/// source path: not a bundle URL, not a generated frame id.
#[must_use]
pub fn is_source_frame(frame: &JsFrame) -> bool {
    frame.line.is_some()
        && frame.file.as_deref().is_some_and(|file| {
            !file.is_empty()
                && !file.contains(".bundle")
                && !file.starts_with("http")
                && !is_generated_frame_id(file)
        })
}

fn frames_are_source(frames: &[JsFrame]) -> bool {
    !frames.is_empty() && frames.iter().all(is_source_frame)
}

fn unsymbolicated(mut error: JsError) -> JsError {
    error.symbolicated = frames_are_source(&error.frames);
    error.symbolication_reason = Some(if error.symbolicated {
        "already source positions".to_string()
    } else if error.frames.is_empty() {
        "no frames".to_string()
    } else {
        "not symbolicated: no Metro host or source map configured".to_string()
    });
    error
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(seq: u64, level: LogLevel, tag: &str, message: &str) -> LogRecord {
        LogRecord {
            seq,
            timestamp_ms: seq,
            level,
            tag: tag.to_string(),
            message: message.to_string(),
        }
    }

    #[test]
    fn fatal_type_error_from_javascript_exception() {
        let logs = vec![
            rec(1, LogLevel::Info, "ActivityManager", "Start proc"),
            rec(
                2,
                LogLevel::Error,
                "ReactNative",
                "Exception in native call from JS\ncom.facebook.react.common.JavascriptException: TypeError: undefined is not a function, js engine: hermes, stack:\nonPress@1:483920\nanonymous@1:120\n_performTransitionSideEffects@-1\n\tat com.facebook.react.modules.core.ExceptionsManagerModule.reportException(ExceptionsManagerModule.kt:52)",
            ),
        ];
        let state = parse_logs(&logs);
        let error = state.js_error.expect("js error");
        assert_eq!(error.name, "TypeError");
        assert_eq!(error.message, "undefined is not a function");
        assert_eq!(error.kind, "fatal");
        assert_eq!(error.frames.len(), 3);
        assert_eq!(error.frames[0].function.as_deref(), Some("onPress"));
        assert_eq!(error.frames[0].line, Some(1));
        assert_eq!(error.frames[0].column, Some(483920));
        assert_eq!(error.frames[2].line, None, "-1 is RN's 'no line'");
        assert!(!error.symbolicated);
        assert!(state.bundle_unreachable.is_none());
    }

    #[test]
    fn split_records_parse_like_one_throwable() {
        let logs = vec![
            rec(
                1,
                LogLevel::Error,
                "ReactNative",
                "TypeError: Cannot read property 'x' of undefined, stack:",
            ),
            rec(
                2,
                LogLevel::Error,
                "ReactNative",
                "render@http://localhost:8081/index.bundle?platform=android&dev=true:1234:56",
            ),
            rec(
                3,
                LogLevel::Error,
                "ReactNative",
                "App@http://localhost:8081/index.bundle?platform=android&dev=true:1300:7",
            ),
        ];
        let error = parse_logs(&logs).js_error.expect("js error");
        assert_eq!(error.kind, "soft");
        assert_eq!(error.frames.len(), 2);
        assert_eq!(
            error.frames[0].file.as_deref(),
            Some("http://localhost:8081/index.bundle?platform=android&dev=true")
        );
        assert_eq!(error.frames[0].line, Some(1234));
        assert_eq!(error.frames[0].column, Some(56));
    }

    #[test]
    fn console_error_with_component_stack() {
        let logs = vec![rec(
            1,
            LogLevel::Error,
            "ReactNativeJS",
            "ReferenceError: Property 'missing' doesn't exist\n    at Counter (http://localhost:8081/index.bundle?platform=android:2000:12)\n    at anonymous (address at http://localhost:8081/index.bundle?platform=android:1:900)\n\nThis error is located at:\n    in Counter\n    in RCTView (created by View)\n    in App",
        )];
        let error = parse_logs(&logs).js_error.expect("js error");
        assert_eq!(error.name, "ReferenceError");
        assert_eq!(error.message, "Property 'missing' doesn't exist");
        assert_eq!(error.frames.len(), 2);
        assert_eq!(error.frames[0].function.as_deref(), Some("Counter"));
        assert_eq!(error.frames[1].line, Some(1));
        assert_eq!(error.frames[1].column, Some(900));
        assert_eq!(
            error.component_stack.as_deref(),
            Some("in Counter\nin RCTView (created by View)\nin App")
        );
    }

    #[test]
    fn metro_syntax_error_is_a_source_level_js_error_not_unreachable() {
        let logs = vec![rec(
            1,
            LogLevel::Error,
            "ReactNative",
            "Unable to download JS bundle\ncom.facebook.react.common.DebugServerException: SyntaxError: /tmp/project/App.js: Unexpected token (26:4)\n\n  24 |   return (\n> 26 |     <<View>\n  at App.js:26:4\n\tat com.facebook.react.devsupport.BundleDownloader.processBundleResult(BundleDownloader.kt:250)",
        )];
        let state = parse_logs(&logs);
        assert!(state.bundle_unreachable.is_none());
        let error = state.js_error.expect("transform error");
        assert_eq!(error.name, "SyntaxError");
        assert!(
            error.message.contains("Unexpected token"),
            "{}",
            error.message
        );
        assert_eq!(error.kind, "transform");
        assert_eq!(error.frames[0].file.as_deref(), Some("App.js"));
        assert_eq!(error.frames[0].line, Some(26));
        assert_eq!(error.frames[0].column, Some(4));
        assert!(error.symbolicated);
    }

    #[test]
    fn connection_refused_is_bundle_unreachable_with_url_and_errno() {
        let logs = vec![rec(
            1,
            LogLevel::Error,
            "ReactNative",
            "Unable to download JS bundle\ncom.facebook.react.common.DebugServerException: Could not connect to development server.\n\nTry the following to fix the issue:\n\u{2022} Ensure that Metro is running\nURL: http://localhost:8081/index.bundle?platform=android&dev=true&minify=false&app=rnhello\n\tat com.facebook.react.common.DebugServerException.makeGeneric(DebugServerException.kt:70)\nCaused by: java.net.ConnectException: Failed to connect to localhost/127.0.0.1:8081\nCaused by: java.net.ConnectException: failed to connect to localhost/127.0.0.1 (port 8081) after 5000ms: isConnected failed: ECONNREFUSED (Connection refused)",
        )];
        let state = parse_logs(&logs);
        assert!(
            state.js_error.is_none(),
            "an unreachable bundle is not a JS error"
        );
        let failure = state.bundle_unreachable.expect("unreachable");
        assert_eq!(
            failure.url.as_deref(),
            Some("http://localhost:8081/index.bundle?platform=android&dev=true&minify=false&app=rnhello")
        );
        assert_eq!(failure.error, "ECONNREFUSED (Connection refused)");
    }

    #[test]
    fn catalyst_reload_error_and_unable_to_load_script_are_unreachable() {
        for text in [
            "Exception in native call\njava.lang.RuntimeException: Failed to load bundle. Try restarting the bundler or reconnecting your device.\nCaused by: java.net.SocketTimeoutException: timeout",
            "Exception in native call\njava.lang.RuntimeException: Unable to load script. Make sure you're either running Metro (run 'npx react-native start') or that your bundle 'index.android.bundle' is packaged correctly for release.",
        ] {
            let state = parse_logs(&[rec(1, LogLevel::Error, "ReactNative", text)]);
            assert!(state.bundle_unreachable.is_some(), "{text}");
            assert!(state.js_error.is_none());
        }
    }

    #[test]
    fn json_redbox_payload() {
        let logs = vec![rec(
            1,
            LogLevel::Error,
            "ReactNativeJS",
            r#"redbox {"message":"TypeError: x is not a function","name":"TypeError","componentStack":"\n    in App","id":3,"isFatal":true,"stack":[{"file":"http://localhost:8081/index.bundle?platform=android","lineNumber":10,"column":4,"methodName":"onPress"}]}"#,
        )];
        let error = parse_logs(&logs).js_error.expect("json");
        assert_eq!(error.name, "TypeError");
        assert_eq!(error.message, "x is not a function");
        assert_eq!(error.component_stack.as_deref(), Some("in App"));
        assert_eq!(error.frames[0].function.as_deref(), Some("onPress"));
        assert_eq!(error.frames[0].line, Some(10));
    }

    #[test]
    fn a_non_rn_app_and_warnings_yield_nothing() {
        let logs = vec![
            rec(
                1,
                LogLevel::Error,
                "MyApp",
                "TypeError: nope, stack:\nf@1:2",
            ),
            rec(2, LogLevel::Warn, "ReactNativeJS", "Error: just a warning"),
            rec(
                3,
                LogLevel::Error,
                "AndroidRuntime",
                "FATAL EXCEPTION: main\njava.lang.IllegalStateException: boom",
            ),
        ];
        assert!(parse_logs(&logs).is_empty());
    }

    #[test]
    fn the_metro_port_comes_only_from_a_logged_bundle_url() {
        let logs = vec![rec(1, LogLevel::Error, "ReactNative", RN_FRAMES)];
        assert_eq!(metro_port_hint(&logs), Some(19000));
        assert_eq!(
            metro_port_hint(&[rec(1, LogLevel::Error, "ReactNative", "f@1:2")]),
            None
        );
        assert_eq!(
            metro_port_hint(&[rec(
                1,
                LogLevel::Info,
                "OkHttp",
                "http://localhost:3000/index.bundle"
            )]),
            None,
            "only React Native's own lines"
        );
    }

    const RN_FRAMES: &str =
        "Error: x, stack:\nf@http://localhost:19000/index.bundle?platform=android:1:2";

    #[test]
    fn the_last_error_wins() {
        let logs = vec![
            rec(
                1,
                LogLevel::Error,
                "ReactNative",
                "Error: first, stack:\na@1:1",
            ),
            rec(2, LogLevel::Info, "ReactNativeJS", "Running \"rnhello\""),
            rec(
                3,
                LogLevel::Error,
                "ReactNative",
                "RangeError: second, stack:\nb@1:2",
            ),
        ];
        let error = parse_logs(&logs).js_error.unwrap();
        assert_eq!(error.name, "RangeError");
        assert_eq!(error.message, "second");
    }

    #[test]
    fn redbox_ui_requires_the_redbox_views() {
        let p = "dev.acos.fixtures.rnhello:id/";
        let ids: Vec<(String, &str)> = vec![
            (format!("{p}catalyst_redbox_title"), "TypeError: undefined is not a function"),
            (format!("{p}rn_frame_method"), "onPress"),
            (format!("{p}rn_frame_file"), "App.js:26:4"),
            (format!("{p}rn_frame_method"), "dispatch"),
            (format!("{p}rn_frame_file"), "index.bundle:1:900"),
            (format!("{p}rn_redbox_dismiss_button"), "Dismiss"),
            (format!("{p}rn_redbox_reload_button"), "Reload"),
        ];
        let nodes: Vec<UiText<'_>> = ids.iter().map(|(rid, text)| (rid.as_str(), *text)).collect();
        let error = parse_ui(&nodes).js_error.expect("ui error");
        assert_eq!(error.source, "ui");
        assert_eq!(error.name, "TypeError");
        assert_eq!(error.frames.len(), 2);
        assert_eq!(error.frames[0].function.as_deref(), Some("onPress"));
        assert_eq!(error.frames[0].file.as_deref(), Some("App.js"));
        assert_eq!(error.frames[0].line, Some(26));
        assert!(!error.symbolicated, "a bundle frame is not a source position");

        let unreachable = parse_ui(&[
            ("android:id/rn_redbox_stack", ""),
            ("", "Could not connect to development server."),
        ]);
        assert!(unreachable.bundle_unreachable.is_some());
    }

    #[test]
    fn an_app_screen_with_error_text_and_dismiss_reload_buttons_is_not_a_redbox() {
        let nodes: Vec<UiText<'_>> = vec![
            ("com.example:id/title", "TypeError: could not save your draft"),
            ("com.example:id/dismiss", "Dismiss"),
            ("com.example:id/reload", "Reload"),
            ("com.example:id/msg", "Could not connect to development server"),
        ];
        assert!(parse_ui(&nodes).is_empty());
        assert!(parse_ui(&[("", "Hello ACOS")]).is_empty());
    }

    #[test]
    fn an_indented_error_after_an_info_line_is_its_own_block() {
        let logs = vec![
            rec(1, LogLevel::Info, "ReactNativeJS", "Running \"rnhello\""),
            rec(2, LogLevel::Error, "ReactNativeJS", "  TypeError: boom"),
            rec(3, LogLevel::Error, "ReactNativeJS", "    at onPress (http://localhost:8081/index.bundle?platform=android:10:2)"),
        ];
        let error = parse_logs(&logs).js_error.expect("the error is not swallowed by the Info block");
        assert_eq!(error.name, "TypeError");
        assert_eq!(error.frames.len(), 1, "the indented frame still attaches to its error");
    }

    #[test]
    fn a_js_exception_saying_unable_to_load_script_is_a_js_error() {
        let js = parse_logs(&[rec(
            1,
            LogLevel::Error,
            "ReactNativeJS",
            "TypeError: Unable to load script from cache\n    at load (http://localhost:8081/index.bundle?platform=android:5:1)",
        )]);
        assert!(js.bundle_unreachable.is_none());
        assert_eq!(js.js_error.unwrap().name, "TypeError");
        let wrapped = parse_logs(&[rec(
            1,
            LogLevel::Error,
            "ReactNative",
            "Exception in native call from JS\ncom.facebook.react.common.JavascriptException: Error: Unable to load script, stack:\nf@1:2",
        )]);
        assert!(wrapped.bundle_unreachable.is_none());
        assert!(wrapped.js_error.is_some());
        // the native loader's own diagnosis stays bundle_unreachable
        let native = parse_logs(&[rec(
            1,
            LogLevel::Error,
            "ReactNative",
            "java.lang.RuntimeException: Unable to load script. Make sure you're either running Metro or that your bundle is packaged correctly.",
        )]);
        assert!(native.bundle_unreachable.is_some());
        assert!(native.js_error.is_none());
    }

    #[test]
    fn generated_frame_ids_are_not_source_files() {
        for id in ["1.js", "123.js", "seg-1.js", "seg-1_2.js"] {
            assert!(is_generated_frame_id(id), "{id}");
        }
        for file in ["App.js", "src/1.js", "seg-a.js", "index.js"] {
            assert!(!is_generated_frame_id(file), "{file}");
        }
        let error = parse_logs(&[rec(1, LogLevel::Error, "ReactNative", "Error: x, stack:\nf@1.js:10:2\ng@seg-1_2.js:3:4")])
            .js_error
            .unwrap();
        assert_eq!(error.frames[0].file.as_deref(), Some("1.js"));
        assert!(!error.symbolicated, "generated ids are not source positions");
    }

    /// `{key: value, ...}` (flat flow mapping) -> the assertion.
    fn assertion(flow: &str) -> acos_protocol::scenario::JsErrorAssertion {
        let mut map = serde_json::Map::new();
        for pair in flow.trim_matches(|c| c == '{' || c == '}').split(", ") {
            let (key, value) = pair.split_once(": ").expect("key: value");
            let value = match value {
                "true" => serde_json::Value::Bool(true),
                "false" => serde_json::Value::Bool(false),
                v => v
                    .parse::<u64>()
                    .map_or_else(|_| serde_json::json!(v), |n| serde_json::json!(n)),
            };
            map.insert(key.to_string(), value);
        }
        serde_json::from_value(serde_json::Value::Object(map)).expect("assertion")
    }

    fn state_with_error() -> RnDevState {
        parse_logs(&[rec(
            1,
            LogLevel::Error,
            "ReactNative",
            "Unable to download JS bundle\ncom.facebook.react.common.DebugServerException: SyntaxError: /p/App.js: Unexpected token (26:4)\n  at App.js:26:4",
        )])
    }

    #[test]
    fn assert_js_error_positive_and_negative() {
        let state = state_with_error();
        let ok = assertion("{name: SyntaxError, message: Unexpected token, file: App.js, line: 26, symbolicated: true}");
        assert!(matches!(check(&ok, &state, false), JsCheck::Pass(_)));
        for (yaml, needle) in [
            ("{name: TypeError}", "name is `SyntaxError`"),
            ("{message: missing}", "does not contain"),
            ("{file: App.js, line: 27}", "no frame at `App.js:27`"),
            ("{file: Other.js}", "no frame at `Other.js`"),
            ("{none: true}", "still reported"),
            ("{bundle_unreachable: true}", "no bundle-fetch failure"),
        ] {
            match check(&assertion(yaml), &state, false) {
                JsCheck::Fail(message) => assert!(message.contains(needle), "{yaml}: {message}"),
                other => panic!("{yaml}: {other:?}"),
            }
        }
        assert!(matches!(
            check(&assertion("{name: TypeError}"), &RnDevState::default(), false),
            JsCheck::Fail(m) if m.contains("no JS error")
        ));
    }

    #[test]
    fn the_wait_follows_the_requested_constraints() {
        let type_error = parse_logs(&[rec(1, LogLevel::Error, "ReactNative", "TypeError: early, stack:\nf@1:2")]);
        // an earlier TypeError does not end a wait for a SyntaxError
        assert!(!wait_settled(&assertion("{name: SyntaxError}"), &type_error, false));
        assert!(wait_settled(&assertion("{name: TypeError}"), &type_error, false));
        // file constraints are judged after symbolication: a matching name stops the wait
        assert!(wait_settled(&assertion("{name: TypeError, file: App.js}"), &type_error, false));
        assert!(!wait_settled(&assertion("{name: SyntaxError, file: App.js}"), &type_error, false));
        assert!(!wait_settled(&assertion("{none: true}"), &type_error, false));
        assert!(wait_settled(&assertion("{none: true}"), &RnDevState::default(), false));
        assert!(!wait_settled(&assertion("{bundle_unreachable: true}"), &type_error, false));
        // already source-level and wrong: keep waiting
        assert!(!wait_settled(&assertion("{file: Other.js}"), &state_with_error(), false));
    }

    #[test]
    fn assert_js_error_none_and_unreachable() {
        assert!(matches!(
            check(&assertion("{none: true}"), &RnDevState::default(), false),
            JsCheck::Pass(_)
        ));
        let unreachable = parse_logs(&[rec(
            1,
            LogLevel::Error,
            "ReactNative",
            "Could not connect to development server.\nURL: http://localhost:8081/index.bundle?platform=android",
        )]);
        // a JS-error assertion on an unreachable bundle is BUNDLE_UNREACHABLE, not a fail
        assert!(matches!(
            check(&assertion("{name: TypeError}"), &unreachable, true),
            JsCheck::Unreachable(_)
        ));
        assert!(matches!(
            check(&assertion("{none: true}"), &unreachable, true),
            JsCheck::Unreachable(_)
        ));
        assert!(matches!(
            check(&assertion("{bundle_unreachable: true}"), &unreachable, true),
            JsCheck::Pass(m) if m.contains("http://localhost:8081/index.bundle")
        ));
        // a JS error *after* a bundle failure (Metro came back, the code threw)
        let mut both = unreachable.clone();
        both.js_error = state_with_error().js_error;
        assert!(matches!(
            check(&assertion("{name: SyntaxError}"), &both, false),
            JsCheck::Pass(_)
        ));
    }
}
