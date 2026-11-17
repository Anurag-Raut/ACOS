//! A backend that replays the checked-in **ART reference** evidence.
//!
//! This is not an app execution backend. It serves the UI tree and log stream
//! that were captured from the real fixture apps on real ART (`art-reference`,
//! API 34, x86_64). It exists so the scenario runner and its assertion logic can
//! be exercised and, crucially, so the negative scenarios can be proven to
//! **fail** instead of being waved through as `unsupported`.
//!
//! It deliberately does **not** implement input, reset or environment: the
//! reference evidence has no way to react to a step, so those steps are
//! `unsupported`. That keeps `hello-views.yaml` an honest partial result.
//!
//! Fidelity is [`Fidelity::ArtReference`]: what is served is the oracle itself,
//! not a native ACOS result.

use std::path::{Path, PathBuf};

use acos_protocol::commands::ResetScope;
use acos_protocol::{
    Backend as ProtocolBackend, ErrorCode, Fidelity, FrameMeta, LogLevel, Selector, UiNode, UiTree,
};

use crate::backend::{
    Backend, BackendError, Capabilities, EnvironmentChange, InputAction, LaunchOutcome,
    LaunchRequest, LogRecord, ManifestInfo, SessionObservation,
};
use crate::manifest::load_manifest;

/// The reference state a replay backend serves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReferenceState {
    /// `ui_initial.json` / `logcat_initial.txt` / `screenshot_initial.png`.
    Initial,
    /// `ui_after_click1.json`.
    AfterClick1,
    /// `ui_after_click2.json`.
    AfterClick2,
    /// `ui_after_recreate.json`.
    AfterRecreate,
    /// `ui_after_rotate_back.json`.
    AfterRotateBack,
}

impl ReferenceState {
    fn ui_file(self) -> &'static str {
        match self {
            ReferenceState::Initial => "ui_initial.json",
            ReferenceState::AfterClick1 => "ui_after_click1.json",
            ReferenceState::AfterClick2 => "ui_after_click2.json",
            ReferenceState::AfterRecreate => "ui_after_recreate.json",
            ReferenceState::AfterRotateBack => "ui_after_rotate_back.json",
        }
    }

    fn log_file(self) -> &'static str {
        match self {
            ReferenceState::Initial => "logcat_initial.txt",
            ReferenceState::AfterClick1 => "logcat_initial.txt",
            ReferenceState::AfterClick2 => "logcat_initial.txt",
            ReferenceState::AfterRecreate => "logcat_recreate.txt",
            ReferenceState::AfterRotateBack => "logcat_final.txt",
        }
    }
}

/// A replay backend over one `fixtures/reference/<app>/art-api34-x86_64/` dir.
#[derive(Debug)]
pub struct ReferenceBackend {
    dir: PathBuf,
    state: ReferenceState,
    manifest: Option<ManifestInfo>,
    session_seq: u64,
    frame_seq: u64,
    logs: Vec<LogRecord>,
    tree: Option<UiTree>,
    screenshot_png: Option<Vec<u8>>,
    crashed: Option<crate::backend::CrashRecord>,
}

impl ReferenceBackend {
    /// Build a replay backend for a reference directory.
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        ReferenceBackend {
            dir: dir.into(),
            state: ReferenceState::Initial,
            manifest: None,
            session_seq: 0,
            frame_seq: 0,
            logs: Vec::new(),
            tree: None,
            screenshot_png: None,
            crashed: None,
        }
    }

    /// The reference directory.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// True when the directory contains the minimum expected ART evidence.
    pub fn is_usable(&self) -> bool {
        self.dir.join(ReferenceState::Initial.ui_file()).is_file()
            && self.dir.join(ReferenceState::Initial.log_file()).is_file()
    }

    fn load_state(&mut self) -> Result<(), BackendError> {
        let package = self
            .manifest
            .as_ref()
            .map(|m| m.package.clone())
            .unwrap_or_default();
        let ui_path = self.dir.join(self.state.ui_file());
        let raw = std::fs::read_to_string(&ui_path).map_err(|e| BackendError::Error {
            code: ErrorCode::Internal,
            detail: format!("cannot read reference tree {}: {e}", ui_path.display()),
        })?;
        let nodes =
            parse_reference_nodes(&raw, &package).map_err(|detail| BackendError::Error {
                code: ErrorCode::Internal,
                detail: format!("cannot parse {}: {detail}", ui_path.display()),
            })?;

        let log_path = self.dir.join(self.state.log_file());
        let logs = std::fs::read_to_string(&log_path)
            .map(|text| parse_logcat(&text))
            .unwrap_or_default();

        let screenshot_path = self.dir.join("screenshot_initial.png");
        self.screenshot_png = std::fs::read(&screenshot_path).ok();
        self.logs = logs;

        self.frame_seq += 1;
        let hash = crate::manifest::sha256_hex(&serde_json::to_vec(&nodes).unwrap_or_default());
        let frame = FrameMeta::new(
            self.frame_seq,
            "1970-01-01T00:00:00.000Z",
            "reference:launch",
            1080,
            2400,
            420,
            hash,
            true,
        );
        self.tree = Some(UiTree::new(frame, package, nodes));
        Ok(())
    }

    fn observation(&self) -> Result<SessionObservation, BackendError> {
        let tree = self.tree.clone().ok_or_else(|| BackendError::Error {
            code: ErrorCode::Internal,
            detail: "reference backend observed before launch".to_string(),
        })?;
        Ok(SessionObservation {
            session_id: format!("s_reference_{}", self.session_seq),
            package: tree.package.clone(),
            frame: tree.frame.clone(),
            tree,
            screenshot_png: self.screenshot_png.clone(),
            crashed: self.crashed.clone(),
            input: None,
            settle: None,
        })
    }
}

impl Backend for ReferenceBackend {
    fn id(&self) -> &'static str {
        "art-reference-replay"
    }

    fn protocol_backend(&self) -> ProtocolBackend {
        ProtocolBackend::AcvmReference
    }

    fn fidelity(&self) -> Fidelity {
        Fidelity::ArtReference
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            static_manifest: true,
            launch: true,
            ui_tree: true,
            logs: true,
            screenshot: false,
            input: false,
            reset: false,
            environment: false,
            crash_reports: false,
            services: false,
        }
    }

    fn manifest(&mut self, apk: &Path) -> Result<ManifestInfo, BackendError> {
        let info = load_manifest(apk)?;
        self.manifest = Some(info.clone());
        Ok(info)
    }

    fn launch(&mut self, request: &LaunchRequest) -> Result<LaunchOutcome, BackendError> {
        if !self.is_usable() {
            return Err(BackendError::Error {
                code: ErrorCode::Internal,
                detail: format!(
                    "reference directory {} does not contain ART evidence ({} missing)",
                    self.dir.display(),
                    ReferenceState::Initial.ui_file()
                ),
            });
        }
        let _ = request;
        self.session_seq += 1;
        self.frame_seq = 0;
        self.load_state()?;
        let tree = self.tree.clone().expect("load_state sets the tree");
        Ok(LaunchOutcome {
            session_id: format!("s_reference_{}", self.session_seq),
            package: tree.package.clone(),
            frame: Some(tree.frame.clone()),
            screenshot_png: self.screenshot_png.clone(),
            crashed: None,
            detail: serde_json::json!({
                "source": self.dir.display().to_string(),
                "state": self.state.ui_file(),
                "fidelity_note": "replayed fixture ART capture, not a native ACOS execution",
            }),
        })
    }

    fn observe(&mut self) -> Result<SessionObservation, BackendError> {
        self.observation()
    }

    fn logs_since(&mut self, since: u64) -> Result<Vec<LogRecord>, BackendError> {
        Ok(self
            .logs
            .iter()
            .filter(|record| record.seq > since)
            .cloned()
            .collect())
    }

    fn input(&mut self, action: &InputAction) -> Result<SessionObservation, BackendError> {
        Err(BackendError::unsupported(
            "input",
            format!(
                "the ART reference replay cannot react to `{}` input",
                action.kind()
            ),
        ))
    }

    fn reset(&mut self, _scope: ResetScope) -> Result<(), BackendError> {
        Err(BackendError::unsupported(
            "reset",
            "the ART reference replay cannot reset state",
        ))
    }

    fn configure(&mut self, change: &EnvironmentChange) -> Result<(), BackendError> {
        Err(BackendError::unsupported(
            "environment",
            format!("the ART reference replay cannot apply `{}`", change.kind()),
        ))
    }

    fn capture(&mut self) -> Result<SessionObservation, BackendError> {
        Err(BackendError::unsupported(
            "screenshot",
            "the ART reference replay does not serve capture steps",
        ))
    }
}

/// One node as stored in `ui_*.json`.
#[derive(Debug, serde::Deserialize)]
struct RawNode {
    #[serde(default)]
    class: String,
    #[serde(default)]
    resource_id: String,
    #[serde(default)]
    text: String,
    #[serde(default)]
    content_desc: String,
    bounds: String,
    #[serde(default)]
    clickable: bool,
    #[serde(default)]
    focused: bool,
    #[serde(default = "default_true")]
    enabled: bool,
}

const fn default_true() -> bool {
    true
}

/// Parse the flat `ui_*.json` array into a [`UiTree`]-shaped node list.
fn parse_reference_nodes(raw: &str, package: &str) -> Result<Vec<UiNode>, String> {
    let raw_nodes: Vec<RawNode> = serde_json::from_str(raw).map_err(|e| e.to_string())?;
    let mut nodes = Vec::with_capacity(raw_nodes.len());
    for (index, raw) in raw_nodes.into_iter().enumerate() {
        let bounds: acos_protocol::Bounds = raw
            .bounds
            .parse()
            .map_err(|e| format!("node {index} bounds `{}`: {e}", raw.bounds))?;
        nodes.push(
            UiNode::new(format!("n{}", index + 1), raw.class, bounds)
                .with_resource_id(raw.resource_id)
                .with_text(raw.text)
                .clickable(raw.clickable)
                .focused(raw.focused)
                .enabled(raw.enabled)
                .content_desc(raw.content_desc)
                .package(package),
        );
    }
    Ok(nodes)
}

/// Parse an `adb logcat` text file into [`LogRecord`]s.
fn parse_logcat(text: &str) -> Vec<LogRecord> {
    let pattern = regex::Regex::new(
        r"^\d{2}-\d{2}\s+\d{2}:\d{2}:\d{2}\.\d{3}\s+\d+\s+\d+\s+([VDIWEF])\s+([^:]+):\s?(.*)$",
    )
    .expect("static logcat regex");
    let mut records = Vec::new();
    for line in text.lines() {
        let Some(captures) = pattern.captures(line) else {
            continue;
        };
        let level = match captures.get(1).map(|m| m.as_str()).unwrap_or("I") {
            "V" => LogLevel::Verbose,
            "D" => LogLevel::Debug,
            "I" => LogLevel::Info,
            "W" => LogLevel::Warn,
            "E" => LogLevel::Error,
            _ => LogLevel::Info,
        };
        let tag = captures
            .get(2)
            .map(|m| m.as_str().trim().to_string())
            .unwrap_or_default();
        let message = captures
            .get(3)
            .map(|m| m.as_str().to_string())
            .unwrap_or_default();
        let seq = records.len() as u64 + 1;
        records.push(LogRecord {
            seq,
            timestamp_ms: seq * 1000,
            level,
            tag,
            message,
        });
    }
    records
}

/// Helper trait extension so nodes can be built fluently.
trait UiNodeExt {
    fn focused(self, focused: bool) -> Self;
    fn enabled(self, enabled: bool) -> Self;
    fn content_desc(self, content_desc: impl Into<String>) -> Self;
    fn package(self, package: impl Into<String>) -> Self;
}

impl UiNodeExt for UiNode {
    fn focused(mut self, focused: bool) -> Self {
        self.focused = focused;
        self
    }

    fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    fn content_desc(mut self, content_desc: impl Into<String>) -> Self {
        self.content_desc = content_desc.into();
        self
    }

    fn package(mut self, package: impl Into<String>) -> Self {
        self.package = package.into();
        self
    }
}

/// Find a node in a reference-built tree (used by tests and diagnostics).
pub fn find_node<'a>(tree: &'a UiTree, selector: &Selector) -> Option<&'a UiNode> {
    tree.find_one(selector)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn reference_dir(app: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/reference")
            .join(app)
            .join("art-api34-x86_64")
    }

    #[test]
    fn parses_reference_tree_and_logs() {
        let dir = reference_dir("hello-views");
        if !dir.is_dir() {
            eprintln!("SKIP parses_reference_tree_and_logs: reference missing");
            return;
        }
        let raw = std::fs::read_to_string(dir.join("ui_initial.json")).unwrap();
        let nodes = parse_reference_nodes(&raw, "dev.acos.fixtures.helloviews").unwrap();
        let result = nodes
            .iter()
            .find(|n| n.resource_id.ends_with(":id/result"))
            .expect("result node");
        assert_eq!(result.text, "clicks: 0");

        let log = std::fs::read_to_string(dir.join("logcat_initial.txt")).unwrap();
        let records = parse_logcat(&log);
        let messages: Vec<&str> = records.iter().map(|r| r.message.as_str()).collect();
        assert_eq!(messages, vec!["onCreate", "onStart", "onResume"]);
    }

    #[test]
    fn backend_replays_initial_state() {
        let dir = reference_dir("hello-views");
        if !dir.is_dir() {
            eprintln!("SKIP backend_replays_initial_state: reference missing");
            return;
        }
        let apk =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/build/apps/hello-views.apk");
        let mut backend = ReferenceBackend::new(&dir);
        if apk.is_file() {
            backend.manifest(&apk).unwrap();
        }
        assert_eq!(backend.fidelity(), Fidelity::ArtReference);
        assert!(backend.is_usable());
    }
}
