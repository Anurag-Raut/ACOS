//! The native ACOS backend: `acos-apk` for static facts and a live
//! [`AgentSession`] for the real runtime.
//!
//! The session is the **same** product path `acos launch --keep` /
//! `acos observe` / `acos tap` / `acos session text` use: one worker thread
//! owning a booted `Runtime`, the live `View` hierarchy, the frame store and the
//! guest log buffer. `verify` therefore drives exactly the tools an agent drives
//! (W161-A tap/type/reset, W167-A post-tap repaint), not a parallel copy.
//!
//! What it does **not** do today (and reports as `unsupported` rather than
//! guessing): display rotation / environment knobs. Those are reported as
//! `unsupported` with the missing capability.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::SystemTime;

use acos_protocol::commands::{KeyName, ResetScope};
use acos_protocol::{
    Backend as ProtocolBackend, Bounds, DisplaySpec, ErrorCode, Fidelity, FrameMeta, UiNode, UiTree,
};
use acos_runtime::session::{
    AgentSession, FrameInfo, LaunchReport, LogLine, ObserveReport, SessionError, SessionOptions,
};
use acos_runtime::view_tree::{ViewNode, ViewTree};

use crate::backend::{
    Backend, BackendError, Capabilities, EnvironmentChange, InputAction, LaunchOutcome,
    LaunchRequest, LogRecord, ManifestInfo, SessionObservation,
};
use crate::manifest::load_manifest;

/// A backend that executes the real ACOS runtime through an [`AgentSession`].
#[derive(Debug)]
pub struct NativeBackend {
    session: Option<AgentSession>,
    manifest: Option<ManifestInfo>,
    session_seq: u64,
    /// Package of the launched app, once `launch` ran.
    package_name: Option<String>,
    manifest_calls: u64,
    /// Run the APK's native libraries (requires the CLI confinement gate).
    native: bool,
    /// Session limits (wall/heap) applied to the worker.
    limits: SessionOptions,
    /// Occurrence counts of already-emitted log lines.
    ///
    /// The session's log view is `live guest buffer ++ launch lifecycle events`
    /// and it re-appends the (constant) lifecycle block on **every** call. A raw
    /// index would therefore skip the live records that were pushed in front of
    /// it (the W161-A tap log). Counting identical lines keeps that view honest
    /// without changing the session contract.
    logs_seen: BTreeMap<String, usize>,
    logs_seq: u64,
    /// Display reported by the scenario, used for frame metadata.
    display: DisplaySpec,
}

impl Default for NativeBackend {
    fn default() -> Self {
        NativeBackend {
            session: None,
            manifest: None,
            session_seq: 0,
            package_name: None,
            manifest_calls: 0,
            native: false,
            limits: SessionOptions {
                // A verifier step may pump the looper longer than an interactive
                // command; the scenario's own `budgets.wall_ms` is the gate.
                wall_secs: 120,
                ..SessionOptions::default()
            },
            logs_seen: BTreeMap::new(),
            logs_seq: 0,
            display: DisplaySpec::offscreen(1080, 2400, 420),
        }
    }
}

impl NativeBackend {
    /// A fresh native backend with no session.
    pub fn new() -> Self {
        NativeBackend::default()
    }

    /// Enable native-library loading (`--native`). The caller must already have
    /// entered the `acos-sandbox` confinement (the loader fails closed
    /// otherwise), exactly like `acos launch --native`.
    #[must_use]
    pub fn with_native(mut self, native: bool) -> Self {
        self.native = native;
        self
    }

    /// Override the session limits (wall clock, heap ceiling).
    #[must_use]
    pub fn with_limits(mut self, limits: SessionOptions) -> Self {
        self.limits = limits;
        self
    }

    /// The display the scenario asked for (recorded in frame metadata).
    #[must_use]
    pub fn with_display(mut self, display: DisplaySpec) -> Self {
        self.display = display;
        self
    }

    fn session(&self) -> Result<&AgentSession, BackendError> {
        self.session.as_ref().ok_or_else(|| BackendError::Error {
            code: ErrorCode::InvalidArgument,
            detail: "no session: a `launch` step must run first".to_string(),
        })
    }

    fn session_id(&self) -> String {
        format!("s_native_{}", self.session_seq.max(1))
    }

    fn package(&self) -> String {
        self.package_name.clone().unwrap_or_else(|| {
            self.manifest
                .as_ref()
                .map(|manifest| manifest.package.clone())
                .unwrap_or_default()
        })
    }

    /// Read the live frame + view hierarchy into one bound observation.
    ///
    /// `fresh` forces a real repaint (`render_frame_now`) first; a capture must
    /// publish a frame newer than the one the previous step completed.
    fn observe_now(&mut self, fresh: bool) -> Result<SessionObservation, BackendError> {
        let session = self.session()?;
        let report = if fresh {
            session.capture(None).map_err(session_error)?
        } else {
            session.observe(None).map_err(session_error)?
        };
        let tree = session
            .observe_tree(acos_runtime::view_tree::DEFAULT_MAX_NODES)
            .map_err(session_error)?;
        observation_from(&report, &tree, &self.package(), self.display)
    }
}

/// The `ObserveReport` + `ViewTree` pair as one `SessionObservation`.
///
/// Both halves are read from the same session one after the other; because the
/// app is idle between the two calls (there is no render thread) they describe
/// the same completed frame. The frame hash/tree come from the frame store; the
/// tree's nodes are the live framework `View`s for that frame.
fn observation_from(
    report: &ObserveReport,
    tree: &ViewTree,
    package: &str,
    display: DisplaySpec,
) -> Result<SessionObservation, BackendError> {
    let Some(frame) = report.frame.as_ref() else {
        return Err(BackendError::Error {
            code: ErrorCode::UnsupportedBackend,
            detail: format!(
                "no completed frame has been published yet (stage reached: {}); refusing to \\
                 return a blank image or an empty tree",
                report.stage_reached
            ),
        });
    };
    let meta = frame_meta(frame, display);
    let mut nodes = Vec::with_capacity(tree.nodes.len());
    for (index, node) in tree.nodes.iter().enumerate() {
        nodes.push(ui_node(node, index + 1, package));
    }
    let mut ui_tree = UiTree::new(meta.clone(), package.to_string(), nodes);
    ui_tree.truncated = tree.truncated;
    Ok(SessionObservation {
        session_id: String::new(),
        package: package.to_string(),
        frame: meta,
        tree: ui_tree,
        screenshot_png: Some(encode_png(&frame.pixels, frame.width, frame.height)?),
        crashed: None,
        input: None,
        settle: None,
    })
}

/// Density from the scenario display (`none` falls back to 160 dpi).
fn display_density(display: DisplaySpec) -> u32 {
    match display {
        DisplaySpec::Offscreen { density_dpi, .. } => density_dpi,
        DisplaySpec::None => 160,
    }
}

/// Frame metadata for one published frame.
///
/// W58: the ART frame is the display's application area, so the metadata carries
/// where that area sits on the display. Without it, an agent reading a tree
/// bound and cropping the PNG is off by the status-bar inset (128 px on the
/// reference) with nothing in the evidence saying so.
fn frame_meta(frame: &FrameInfo, display: DisplaySpec) -> FrameMeta {
    let meta = FrameMeta::new(
        frame.frame_id,
        now_rfc3339(),
        frame.cause_id.to_string(),
        frame.width,
        frame.height,
        display_density(display),
        frame.content_hash.clone(),
        frame_is_meaningful(&frame.pixels),
    );
    match crate::display::frame_display_geometry(frame.width, frame.height) {
        Some(geometry) => geometry.apply(meta),
        None => meta,
    }
}

/// A frame is meaningful only when it carries rendered content: at least one
/// non-zero pixel and not a single uniform colour. A blank/placeholder buffer
/// is reported `meaningful: false` so the executor rejects it.
fn frame_is_meaningful(pixels: &[u8]) -> bool {
    let Some(first) = pixels.get(..4) else {
        return false;
    };
    if first == [0, 0, 0, 0] {
        return false;
    }
    pixels.chunks_exact(4).any(|px| px != first)
}

/// Convert one live `View` node to a protocol `UiNode` (recursively).
fn ui_node(node: &ViewNode, index: usize, package: &str) -> UiNode {
    let mut ui = UiNode::new(format!("n{index}"), node.class.clone(), Bounds(node.bounds));
    ui.resource_id = node.resource_id.clone();
    ui.text = node.text.clone();
    ui.content_desc = node.content_desc.clone();
    ui.visible = node.visible;
    ui.visible_bounds = node.visible_bounds.map(Bounds);
    ui.enabled = node.enabled;
    ui.focused = node.focused;
    ui.clickable = node.clickable;
    ui.scrollable = node.scrollable;
    ui.checked = node.checked;
    ui.package = if node.package.is_empty() {
        package.to_string()
    } else {
        node.package.clone()
    };
    ui.children = node
        .children
        .iter()
        .enumerate()
        .map(|(child, node)| ui_node(node, child + 1, package))
        .collect();
    ui
}

fn now_rfc3339() -> String {
    let ms = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    crate::clock::rfc3339_from_unix_ms(ms)
}

fn encode_png(rgba: &[u8], width: u32, height: u32) -> Result<Vec<u8>, BackendError> {
    if width == 0 || height == 0 {
        return Err(BackendError::Error {
            code: ErrorCode::NoFreshFrame,
            detail: "published frame has zero size".to_string(),
        });
    }
    let mut out = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut out, width, height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.set_compression(png::Compression::Fast);
        let mut writer = encoder.write_header().map_err(|e| BackendError::Error {
            code: ErrorCode::Internal,
            detail: format!("png header: {e}"),
        })?;
        writer
            .write_image_data(rgba)
            .map_err(|e| BackendError::Error {
                code: ErrorCode::Internal,
                detail: format!("png encode: {e}"),
            })?;
    }
    Ok(out)
}

fn session_error(error: SessionError) -> BackendError {
    let code = ErrorCode::from_str_opt(error.code).unwrap_or(ErrorCode::Internal);
    BackendError::Error {
        code,
        detail: error.message,
    }
}

/// A launch ladder that did not reach `first_frame` is not a pass.
fn launch_failure(report: &LaunchReport) -> Option<BackendError> {
    let blocker = report.root_blocker.as_ref()?;
    // The VM mirrors a native method with no exported/registered implementation
    // as `UnsatisfiedLinkError: No implementation found for ... (tried X)`.
    // That is exactly V1 §4.3's missing-symbol case and must be reported as the
    // structured `UNSUPPORTED_NATIVE_SYMBOL` (with the exact symbol), not as a
    // generic Java crash.
    if let Some(symbol) = missing_native_symbol(&blocker.message) {
        return Some(BackendError::Error {
            code: ErrorCode::UnsupportedNativeSymbol,
            detail: format!(
                "native symbol {symbol} is not provided by the loaded library: {}",
                blocker.message.lines().next().unwrap_or_default()
            ),
        });
    }
    let code = ErrorCode::from_str_opt(&blocker.code).unwrap_or(ErrorCode::Internal);
    let mut stack = blocker
        .details
        .iter()
        .find(|(key, _)| key == "stack")
        .map(|(_, value)| {
            value
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if stack.is_empty() {
        // The runtime renders the uncaught exception (class, message, frames)
        // into `blocker.message`; keep the frames as structured stack entries.
        stack = blocker
            .message
            .lines()
            .map(str::trim)
            .filter(|line| line.starts_with("at "))
            .map(str::to_string)
            .collect();
    }
    let first_line = blocker.message.lines().next().unwrap_or_default();
    let exception = blocker
        .details
        .iter()
        .find(|(key, _)| key == "exception")
        .map(|(_, value)| value.clone())
        .filter(|value| value != "JAVA_EXCEPTION")
        .or_else(|| {
            first_line
                .split(':')
                .next()
                .filter(|name| name.contains('.'))
                .map(str::to_string)
        })
        .unwrap_or_else(|| blocker.code.clone());
    let crash_like = code == ErrorCode::AppCrashed
        || blocker.code.contains("CRASH")
        || blocker.code.contains("JAVA_EXCEPTION");
    if crash_like {
        return Some(BackendError::Crash {
            exception,
            message: blocker.message.clone(),
            stack,
            fatal: true,
        });
    }
    Some(BackendError::Error {
        code,
        detail: format!(
            "launch stopped at `{}`: {} ({})",
            report.reached_stage, blocker.message, blocker.code
        ),
    })
}

/// `Some("name")` when the runtime reported an unresolved native method.
fn missing_native_symbol(message: &str) -> Option<String> {
    if !message.contains("UnsatisfiedLinkError") || !message.contains("No implementation found for")
    {
        return None;
    }
    let start = message.find("->")? + 2;
    let rest = &message[start..];
    let end = rest
        .find('(')
        .or_else(|| rest.find(';'))
        .unwrap_or(rest.len());
    let name = rest[..end].trim();
    if name.is_empty() {
        None
    } else {
        Some(name.to_string())
    }
}

fn input_error(error: SessionError) -> BackendError {
    let code = ErrorCode::from_str_opt(error.code).unwrap_or(ErrorCode::Internal);
    // `TREE_TRUNCATED` is the truncated-tree form of a missing selector: it
    // belongs on the `Failed` (assertion) path with `ELEMENT_NOT_FOUND`, and it
    // is deliberately *not* retryable like a stale ref.
    if code == ErrorCode::ElementNotFound || code == ErrorCode::TreeTruncated {
        return BackendError::Failed {
            code,
            detail: error.message,
        };
    }
    BackendError::Error {
        code,
        detail: error.message,
    }
}

fn key_code(key: KeyName) -> i32 {
    match key {
        KeyName::Back => 4,
        KeyName::Home => 3,
        KeyName::Enter => 66,
        KeyName::Tab => 61,
        KeyName::Delete => 67,
        KeyName::Escape => 111,
        KeyName::VolumeUp => 24,
        KeyName::VolumeDown => 25,
        KeyName::Power => 26,
        KeyName::Menu => 82,
        KeyName::DpadUp => 19,
        KeyName::DpadDown => 20,
        KeyName::DpadLeft => 21,
        KeyName::DpadRight => 22,
    }
}

impl Backend for NativeBackend {
    fn id(&self) -> &'static str {
        "acos-native"
    }

    fn protocol_backend(&self) -> ProtocolBackend {
        ProtocolBackend::AcosNative
    }

    fn fidelity(&self) -> Fidelity {
        Fidelity::NativeRuntime
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            static_manifest: true,
            launch: true,
            ui_tree: true,
            logs: true,
            screenshot: true,
            input: true,
            reset: true,
            environment: false,
            crash_reports: true,
            services: false,
        }
    }

    fn manifest(&mut self, apk: &Path) -> Result<ManifestInfo, BackendError> {
        self.manifest_calls += 1;
        let info = load_manifest(apk)?;
        self.manifest = Some(info.clone());
        Ok(info)
    }

    fn launch(&mut self, request: &LaunchRequest) -> Result<LaunchOutcome, BackendError> {
        // A second launch replaces the previous worker (the session owns one VM).
        if let Some(session) = self.session.take() {
            session.close();
        }
        self.logs_seen.clear();
        self.logs_seq = 0;
        self.display = request.display;

        let limits = SessionOptions {
            native: self.native,
            ..self.limits
        };
        let session = AgentSession::start(limits);
        let report = match session.launch(&request.apk) {
            Ok(report) => report,
            Err(error) => {
                let mapped = session_error(error);
                session.close();
                return Err(mapped);
            }
        };
        self.session_seq += 1;
        let stages: Vec<serde_json::Value> = report
            .stages
            .iter()
            .map(|stage| {
                serde_json::json!({
                    "stage": stage.name,
                    "status": match stage.status {
                        acos_runtime::session::StageStatus::Ok => "ok",
                        acos_runtime::session::StageStatus::Blocked => "blocked",
                        acos_runtime::session::StageStatus::NotReached => "not_reached",
                    },
                    "blocker": stage.blocker.as_ref().map(|blocker| serde_json::json!({
                        "code": blocker.code,
                        "message": blocker.message,
                        "details": blocker.details.iter().cloned().collect::<std::collections::BTreeMap<_, _>>(),
                    })),
                })
            })
            .collect();
        let detail = serde_json::json!({
            "stages": stages,
            "reached_stage": report.reached_stage,
            "classes_loaded": report.classes_loaded,
            "wall_ms": report.wall_ms,
            "native": self.native,
            "fidelity_note": "live AgentSession: real framework bytecode, live View tree, frame store",
        });

        if let Some(error) = launch_failure(&report) {
            session.close();
            return Err(error);
        }

        self.package_name = report.package.clone();
        self.session = Some(session);
        Ok(LaunchOutcome {
            session_id: self.session_id(),
            package: report.package.clone().unwrap_or_else(|| self.package()),
            frame: None,
            screenshot_png: None,
            crashed: None,
            detail,
        })
    }

    fn observe(&mut self) -> Result<SessionObservation, BackendError> {
        let mut observation = self.observe_now(false)?;
        observation.session_id = self.session_id();
        Ok(observation)
    }

    fn logs_since(&mut self, _since: u64) -> Result<Vec<LogRecord>, BackendError> {
        let session = self.session()?;
        let lines: Vec<LogLine> = session.logs().map_err(session_error)?;
        // Multiset difference against the previous snapshot, not "count beyond
        // the previous count". The guest log sink is a bounded ring (4096
        // records): once it evicts, a line's *count* in the snapshot goes
        // **down**, and count-based dedup then silently drops new records that
        // share a line with an evicted one. That is what hid the recreation
        // lifecycle (`onCreate` after `onDestroy`) from `assert_log` in
        // W191-A. A multiset difference is stable under eviction.
        let mut remaining = self.logs_seen.clone();
        let mut out = Vec::new();
        let mut seen_now: BTreeMap<String, usize> = BTreeMap::new();
        for line in &lines {
            let key = line.line.clone();
            *seen_now.entry(key.clone()).or_insert(0) += 1;
            match remaining.get_mut(&key) {
                Some(n) if *n > 0 => *n -= 1,
                _ => {
                    self.logs_seq += 1;
                    out.push(log_record(line, self.logs_seq));
                }
            }
        }
        self.logs_seen = seen_now;
        Ok(out)
    }

    fn display_size(&self) -> Option<(u32, u32)> {
        match self.display {
            acos_protocol::DisplaySpec::Offscreen { width, height, .. } => Some((width, height)),
            acos_protocol::DisplaySpec::None => None,
        }
    }

    fn input(&mut self, action: &InputAction) -> Result<SessionObservation, BackendError> {
        match action {
            InputAction::Tap { selector, x, y } => {
                let (tx, ty) = match (selector, x, y) {
                    (Some(selector), _, _) => self.selector_center(selector)?,
                    (None, Some(x), Some(y)) => (*x as f32, *y as f32),
                    _ => {
                        return Err(BackendError::Error {
                            code: ErrorCode::InvalidArgument,
                            detail: "tap needs a selector or explicit x and y".to_string(),
                        })
                    }
                };
                self.session()?.tap(tx, ty).map_err(input_error)?;
            }
            InputAction::Type {
                selector,
                text,
                clear,
            } => {
                let node = match selector {
                    Some(selector) => Some(self.find_node(selector)?),
                    None => None,
                };
                let (tx, ty) = match &node {
                    Some(node) => {
                        let (x, y) = node.bounds.center();
                        (x as f32, y as f32)
                    }
                    None => return Err(BackendError::unsupported(
                        "input",
                        "type without a selector needs a focused editor; the session text path \
                             requires focus to be established first",
                    )),
                };
                let session = self.session()?;
                session.tap(tx, ty).map_err(input_error)?;
                if *clear {
                    let count = node.as_ref().map(|n| n.text.chars().count()).unwrap_or(0);
                    // Move to the end of the field, then delete the existing text.
                    session.key(123).map_err(input_error)?;
                    for _ in 0..count {
                        session.key(67).map_err(input_error)?;
                    }
                }
                session.text(text.clone()).map_err(input_error)?;
            }
            InputAction::Swipe {
                from,
                to,
                duration_ms,
                fling: _,
            } => {
                // The frozen VM's own gesture path: the same interpolated
                // `DOWN`/`MOVE`/`UP` stream its `AgentSession` publishes. It has
                // no scroller physics, so `fling` only changes the timing on the
                // ART engine; the gesture itself is identical.
                self.session()?.swipe(
                    (from.0 as f32, from.1 as f32),
                    (to.0 as f32, to.1 as f32),
                    std::time::Duration::from_millis(*duration_ms),
                    8,
                ).map_err(input_error)?;
            }
            InputAction::Key(key) => {
                self.session()?.key(key_code(*key)).map_err(input_error)?;
            }
            InputAction::Back => {
                self.session()?.key(4).map_err(input_error)?;
            }
            InputAction::Rotate(orientation) => {
                // The real config-change path: the session flips the display
                // and drives `ActivityThread.handleRelaunchActivity`, so the
                // activity is paused, stopped (`onSaveInstanceState`),
                // destroyed, and recreated from the saved `Bundle`.
                let code = match orientation {
                    acos_protocol::commands::Orientation::Portrait => 1,
                    acos_protocol::commands::Orientation::Landscape => 2,
                };
                self.session()?.relaunch(code).map_err(input_error)?;
            }
        }
        // Force a completed frame for the post-action observation: a text commit
        // does not pump the looper, and a tap's paint may be coalesced. The
        // renderer draws the current tree, so the frame is real; the
        // change check still compares tree/screenshot content.
        let mut observation = self.observe_now(true)?;
        observation.session_id = self.session_id();
        Ok(observation)
    }

    fn reset(&mut self, _scope: ResetScope) -> Result<(), BackendError> {
        let session = self.session()?;
        session.reset().map_err(session_error)?;
        self.logs_seen.clear();
        self.logs_seq = 0;
        Ok(())
    }

    fn configure(&mut self, change: &EnvironmentChange) -> Result<(), BackendError> {
        Err(BackendError::unsupported(
            "environment",
            format!("the native runtime cannot apply `{}` yet", change.kind()),
        ))
    }

    fn capture(&mut self) -> Result<SessionObservation, BackendError> {
        let mut observation = self.observe_now(true)?;
        observation.session_id = self.session_id();
        Ok(observation)
    }
}

impl NativeBackend {
    /// Resolve a selector against the live tree and return its centre point.
    fn selector_center(
        &mut self,
        selector: &acos_protocol::Selector,
    ) -> Result<(f32, f32), BackendError> {
        let node = self.find_node(selector)?;
        if !node.visible {
            return Err(BackendError::Failed {
                code: ErrorCode::ElementOffscreen,
                detail: format!(
                    "matched node {} is not visible to the user (bounds {}, visible_bounds {}); \
                     a tap must land inside the visible rect — scroll it into view first",
                    node.id,
                    node.bounds,
                    node.visible_bounds
                        .map_or_else(|| "<none>".to_string(), |b| b.to_string())
                ),
            });
        }
        let rect = node.visible_rect();
        if !rect.is_valid() || rect.width() <= 0 || rect.height() <= 0 {
            return Err(BackendError::Failed {
                code: ErrorCode::ElementNotFound,
                detail: format!(
                    "matched node {} has empty visible bounds {} (bounds {})",
                    node.id, rect, node.bounds
                ),
            });
        }
        // The centre of the *visible* rect: never a point outside what the user
        // can see (GR2 W3 tapped a text node whose full-bounds centre landed in
        // the neighbouring tab).
        let (x, y) = rect.center();
        Ok((x as f32, y as f32))
    }

    /// Find one live node by selector (the same rules as the protocol).
    fn find_node(&mut self, selector: &acos_protocol::Selector) -> Result<UiNode, BackendError> {
        let session = self.session()?;
        let tree = session
            .observe_tree(acos_runtime::view_tree::DEFAULT_MAX_NODES)
            .map_err(session_error)?;
        let package = self.package();
        let mut nodes = Vec::with_capacity(tree.nodes.len());
        for (index, node) in tree.nodes.iter().enumerate() {
            nodes.push(ui_node(node, index + 1, &package));
        }
        let frame = FrameMeta::new(
            0,
            now_rfc3339(),
            "selector".to_string(),
            0,
            0,
            display_density(self.display),
            String::new(),
            true,
        );
        let ui_tree = UiTree::new(frame, package, nodes);
        if let Some(node) = ui_tree.find_one(selector) {
            return Ok(node.clone());
        }
        if let Some(node) = ui_tree.find_invisible(selector) {
            return Err(BackendError::Failed {
                code: ErrorCode::ElementOffscreen,
                detail: format!(
                    "selector {selector} matched only a node that is not visible to the user \
                     (id {}, text {:?}, bounds {}, visible_bounds {})",
                    node.id,
                    node.text,
                    node.bounds,
                    node.visible_bounds
                        .map_or_else(|| "<none>".to_string(), |b| b.to_string())
                ),
            });
        }
        ui_tree
            .find_one(selector)
            .cloned()
            .ok_or_else(|| BackendError::Failed {
                code: ErrorCode::ElementNotFound,
                detail: format!("selector {selector} matched no node in the live view tree"),
            })
    }
}

fn log_record(line: &LogLine, seq: u64) -> LogRecord {
    use acos_protocol::LogLevel;
    let level = match line.priority {
        2 => LogLevel::Verbose,
        3 => LogLevel::Debug,
        4 => LogLevel::Info,
        5 => LogLevel::Warn,
        6 => LogLevel::Error,
        _ => LogLevel::Error,
    };
    LogRecord {
        seq,
        timestamp_ms: 0,
        level,
        tag: line.tag.clone(),
        message: line.message.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_capabilities_are_honest() {
        let backend = NativeBackend::new();
        let caps = backend.capabilities();
        assert!(caps.static_manifest);
        assert!(caps.launch);
        assert!(caps.ui_tree);
        assert!(caps.input);
        assert!(caps.screenshot);
        assert!(caps.logs);
        assert!(
            !caps.environment,
            "rotation/locale knobs are not implemented"
        );
        assert_eq!(backend.fidelity(), Fidelity::NativeRuntime);
    }

    #[test]
    fn blank_frames_are_not_meaningful() {
        assert!(!frame_is_meaningful(&[]));
        assert!(!frame_is_meaningful(&[0, 0, 0, 0]));
        assert!(!frame_is_meaningful(&[0, 0, 0, 255, 0, 0, 0, 255]));
        assert!(!frame_is_meaningful(&[1, 2, 3, 255, 1, 2, 3, 255]));
        assert!(frame_is_meaningful(&[1, 2, 3, 255, 1, 2, 4, 255]));
    }

    #[test]
    fn view_tree_maps_to_protocol_ui_tree() {
        let node = ViewNode {
            id: "v1".to_string(),
            class: "android.widget.TextView".to_string(),
            resource_id: "dev.acos.fixtures.helloviews:id/title".to_string(),
            text: "Hello ACOS".to_string(),
            content_desc: String::new(),
            bounds: [42, 317, 231, 368],
            visible_bounds: Some([42, 317, 231, 368]),
            visible: true,
            enabled: true,
            focused: false,
            clickable: false,
            scrollable: false,
            checked: false,
            package: "dev.acos.fixtures.helloviews".to_string(),
            children: vec![ViewNode {
                id: "v2".to_string(),
                class: "android.widget.TextView".to_string(),
                resource_id: String::new(),
                text: "child".to_string(),
                content_desc: String::new(),
                bounds: [0, 0, 10, 10],
                visible_bounds: Some([0, 0, 10, 10]),
                visible: true,
                enabled: true,
                focused: false,
                clickable: false,
                scrollable: false,
                checked: false,
                package: String::new(),
                children: Vec::new(),
            }],
        };
        let ui = ui_node(&node, 1, "pkg");
        assert_eq!(ui.resource_id, "dev.acos.fixtures.helloviews:id/title");
        assert_eq!(ui.bounds, Bounds::new(42, 317, 231, 368));
        assert_eq!(ui.children.len(), 1);
        assert_eq!(ui.children[0].package, "pkg");
    }

    #[test]
    fn every_key_name_has_a_code() {
        // Compile-time: adding a KeyName without updating the mapping fails here.
        for key in [
            KeyName::Back,
            KeyName::Home,
            KeyName::Enter,
            KeyName::Tab,
            KeyName::Delete,
            KeyName::Escape,
            KeyName::VolumeUp,
            KeyName::VolumeDown,
            KeyName::Power,
            KeyName::Menu,
            KeyName::DpadUp,
            KeyName::DpadDown,
            KeyName::DpadLeft,
            KeyName::DpadRight,
        ] {
            assert!(key_code(key) > 0);
        }
    }
}
