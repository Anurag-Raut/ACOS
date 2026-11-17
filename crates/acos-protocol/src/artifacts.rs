//! Artifacts layout for `acos verify --artifacts <dir>` and its validator.
//!
//! Layout (all paths relative to the artifacts directory):
//!
//! ```text
//! run.json
//! verdict.json
//! steps/00-launch.json
//! steps/01-assert_text.json
//! frames/19.png
//! frames/16-meanings-collapsed.png
//! tree/19.json
//! logs/logcat.txt
//! events.jsonl
//! resources.json
//! repro.sh
//! repro.json
//! ```
//!
//! The central rule: **a missing artifact for a claimed pass makes the verdict
//! invalid.** A pass is accepted only when every step has evidence, every
//! referenced frame/tree exists, and the reproducer is present and consistent
//! with the verdict's hashes.
//!
//! **A run owns its directory.** `run.json` is written before anything else in
//! the run and names the owning run (`request_id`). A non-empty directory that
//! already holds another run's artifacts is refused (`INVALID_ARGUMENT`, naming
//! the previous run id) unless the caller passes `--overwrite`; see
//! [`claim_run_dir`].

use std::path::{Component, Path, PathBuf};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::error::{ErrorCode, ProtocolError};
use crate::verdict::{Reproducer, Verdict, VERDICT_SCHEMA_VERSION};

#[cfg(test)]
use crate::envelope::Status;

/// `run.json`: the run-ownership marker, written first.
pub const RUN_JSON: &str = "run.json";

/// `verdict.json`.
pub const VERDICT_JSON: &str = "verdict.json";
/// `steps/` directory.
pub const STEPS_DIR: &str = "steps";
/// `frames/` directory.
pub const FRAMES_DIR: &str = "frames";
/// `tree/` directory.
pub const TREE_DIR: &str = "tree";
/// `logs/` directory.
pub const LOGS_DIR: &str = "logs";
/// `events.jsonl`.
pub const EVENTS_JSONL: &str = "events.jsonl";
/// `resources.json`.
pub const RESOURCES_JSON: &str = "resources.json";
/// `repro.sh`.
pub const REPRO_SH: &str = "repro.sh";
/// `repro.json`.
pub const REPRO_JSON: &str = "repro.json";

/// Step result artifact file name (`steps/03-assert_text.json`).
pub fn step_file_name(index: usize, kind: &str) -> String {
    format!("{STEPS_DIR}/{index:02}-{kind}.json")
}

/// Frame artifact path (`frames/19.png`).
pub fn frame_file_name(frame_seq: u64) -> String {
    format!("{FRAMES_DIR}/{frame_seq}.png")
}

/// Sanitise a `capture` step name for use as a file-name component.
///
/// ASCII letters, digits, `-`, `_` and `.` survive; every other character
/// (including `/`, spaces and CJK) becomes `-`. Runs of `-` collapse, leading
/// and trailing `-`/`.` are trimmed, and the result is capped at 64 bytes so a
/// pathological name cannot hit a file-system limit. An empty result becomes
/// `capture`. The mapping is deterministic: the same scenario always names the
/// same file.
pub fn sanitize_capture_name(name: &str) -> String {
    let mut out = String::with_capacity(name.len().min(64));
    for ch in name.chars() {
        if out.len() >= 64 {
            break;
        }
        if ch.is_ascii_alphanumeric() || ch == '_' || ch == '.' {
            out.push(ch);
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
    }
    let trimmed = out.trim_matches(|c| c == '-' || c == '.');
    if trimmed.is_empty() {
        "capture".to_string()
    } else {
        trimmed.to_string()
    }
}

/// Named copy of a capture frame (`frames/16-meanings-collapsed.png`).
///
/// The canonical frame artifact stays `frames/<seq>.png` (the file
/// `steps[].frame_seq` names); this is the same bytes under the step's own
/// label, so "the screenshot for X" is answerable without cross-referencing
/// step JSONs.
pub fn capture_frame_file_name(frame_seq: u64, name: &str) -> String {
    format!(
        "{FRAMES_DIR}/{frame_seq}-{}.png",
        sanitize_capture_name(name)
    )
}

/// Which run wrote an artifacts directory, as recorded by `run.json` (falling
/// back to `verdict.json` / `engine.json` for directories written before the
/// marker existed).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
pub struct RunMarker {
    /// The run that owns the directory (the envelope `request_id`).
    pub request_id: String,
    /// RFC 3339 UTC start time.
    #[serde(default)]
    pub started_at: String,
    /// Scenario path as used.
    #[serde(default)]
    pub scenario: String,
    /// APK path as used.
    #[serde(default)]
    pub apk: String,
    /// ACOS version.
    #[serde(default)]
    pub acos_version: String,
    /// The run id whose artifacts `--overwrite` replaced, when this marker
    /// itself was written after a clobber.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub overwrote_run_id: Option<String>,
}

impl RunMarker {
    /// A marker for one run.
    pub fn new(
        request_id: impl Into<String>,
        started_at: impl Into<String>,
        scenario: impl Into<String>,
        apk: impl Into<String>,
        acos_version: impl Into<String>,
    ) -> Self {
        RunMarker {
            request_id: request_id.into(),
            started_at: started_at.into(),
            scenario: scenario.into(),
            apk: apk.into(),
            acos_version: acos_version.into(),
            overwrote_run_id: None,
        }
    }
}

/// Read `run.json` (the ownership marker), if present and valid.
pub fn read_run_marker(dir: &Path) -> Option<RunMarker> {
    let text = std::fs::read_to_string(dir.join(RUN_JSON)).ok()?;
    serde_json::from_str(&text).ok()
}

/// The result of claiming an artifacts directory for one run.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RunDirClaim {
    /// The previous run's id, when this run replaced its artifacts.
    pub previous_run_id: Option<String>,
    /// True when existing artifacts were removed (`--overwrite`).
    pub overwrote: bool,
}

/// The run id recorded in `dir`, if any.
///
/// Order: `run.json` (the ownership marker), then `verdict.json` (a completed
/// run), then `engine.json` (the CLI's annotated report). A directory with files
/// but none of those is still owned by *something*; caller reports `None` as an
/// unknown previous run rather than pretending the directory is fresh.
pub fn previous_run_id(dir: &Path) -> Option<String> {
    let read = |name: &str, key: &str| -> Option<String> {
        let text = std::fs::read_to_string(dir.join(name)).ok()?;
        let value: serde_json::Value = serde_json::from_str(&text).ok()?;
        value
            .get(key)
            .and_then(serde_json::Value::as_str)
            .filter(|id| !id.trim().is_empty())
            .map(str::to_string)
    };
    read(RUN_JSON, "request_id")
        .or_else(|| read(VERDICT_JSON, "request_id"))
        .or_else(|| read("engine.json", "request_id"))
}

/// Does `dir` hold any file (at any depth)? Empty directories do not count: a
/// runner that pre-creates `--artifacts out/x/art-frames` for the engine has not
/// written evidence, and must not be mistaken for a previous run.
fn has_files(dir: &Path) -> Result<bool, ProtocolError> {
    let entries = std::fs::read_dir(dir).map_err(|e| artifact_io_error(dir, &e))?;
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        let file_type = entry
            .file_type()
            .map_err(|e| artifact_io_error(&path, &e))?;
        if file_type.is_file() || file_type.is_symlink() {
            return Ok(true);
        }
        if file_type.is_dir() && has_files(&path)? {
            return Ok(true);
        }
    }
    Ok(false)
}

fn artifact_io_error(path: &Path, error: &std::io::Error) -> ProtocolError {
    let path_text = path.display().to_string();
    if matches!(error.raw_os_error(), Some(28) | Some(122)) {
        ProtocolError::new(
            ErrorCode::HostDiskFull,
            format!("cannot write {path_text}: {error}"),
        )
        .with_details(serde_json::json!({"path": path_text, "os_error": error.raw_os_error()}))
    } else {
        ProtocolError::new(
            ErrorCode::ArtifactsInvalid,
            format!("cannot write {path_text}: {error}"),
        )
        .with_hint(
            "The artifacts path could not be written but the host disk is not full. Check \
             permissions and that --artifacts is inside the workspace.",
        )
        .with_details(serde_json::json!({"path": path_text, "os_error": error.raw_os_error()}))
    }
}

fn clear_directory(dir: &Path) -> Result<(), ProtocolError> {
    let entries = std::fs::read_dir(dir).map_err(|e| artifact_io_error(dir, &e))?;
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        let file_type = entry
            .file_type()
            .map_err(|e| artifact_io_error(&path, &e))?;
        let result = if file_type.is_dir() {
            std::fs::remove_dir_all(&path)
        } else {
            std::fs::remove_file(&path)
        };
        result.map_err(|e| artifact_io_error(&path, &e))?;
    }
    Ok(())
}

/// Claim `dir` for the run described by `marker` and write the `run.json` marker.
///
/// The rule: **one run owns one artifacts directory.**
///
/// * absent directory, or one with no files in it (empty `frames/` and so on):
///   created and claimed;
/// * non-empty directory written by another run: `INVALID_ARGUMENT` naming the
///   previous run id (`details.previous_run_id`), unless `overwrite` is set;
/// * `overwrite`: the existing artifacts are removed, then the directory is
///   claimed; the previous run id is returned so the caller can record it.
///
/// The check runs before any step, frame or session is written, so two runs can
/// never interleave evidence in one directory.
pub fn claim_run_dir(
    dir: &Path,
    marker: &RunMarker,
    overwrite: bool,
) -> Result<RunDirClaim, ProtocolError> {
    if marker.request_id.trim().is_empty() {
        return Err(ProtocolError::new(
            ErrorCode::InvalidArgument,
            "run marker has an empty request_id",
        )
        .with_hint(
            "This is a runner bug: every run has a request id before it writes artifacts.",
        ));
    }
    let mut claim = RunDirClaim::default();
    if dir.exists() {
        if !dir.is_dir() {
            return Err(ProtocolError::new(
                ErrorCode::InvalidArgument,
                format!("artifacts path {} is not a directory", dir.display()),
            )
            .with_hint("Pass a directory path to --artifacts (it is created when missing)."));
        }
        let used = has_files(dir)?;
        if used {
            let previous = previous_run_id(dir);
            if !overwrite {
                let named = previous
                    .clone()
                    .unwrap_or_else(|| "an earlier run (no run.json/verdict.json)".to_string());
                return Err(ProtocolError::new(
                    ErrorCode::InvalidArgument,
                    format!(
                        "artifacts directory {} already holds {}, so this run may not write into it",
                        dir.display(),
                        match &previous {
                            Some(id) => format!("run {id}"),
                            None => "an incomplete earlier run".to_string(),
                        }
                    ),
                )
                .with_hint(format!(
                    "A run owns its artifacts directory ({named}). Pass a fresh --artifacts <dir>, \
                     or --overwrite to replace the existing evidence deliberately."
                ))
                .with_details(serde_json::json!({
                    "artifacts": dir.display().to_string(),
                    "previous_run_id": previous,
                    "run_id": marker.request_id,
                    "overwrite": false,
                    "fix": "--overwrite",
                })));
            }
            clear_directory(dir)?;
            claim.previous_run_id = previous;
            claim.overwrote = true;
        }
    } else {
        std::fs::create_dir_all(dir).map_err(|e| artifact_io_error(dir, &e))?;
    }
    let mut marker = marker.clone();
    if claim.overwrote {
        marker.overwrote_run_id = claim.previous_run_id.clone();
    }
    let text = serde_json::to_string_pretty(&marker).map_err(|e| {
        ProtocolError::new(
            ErrorCode::Internal,
            format!("cannot serialize run marker: {e}"),
        )
    })?;
    let path = dir.join(RUN_JSON);
    std::fs::write(&path, text.as_bytes()).map_err(|e| artifact_io_error(&path, &e))?;
    Ok(claim)
}

/// Tree artifact path (`tree/19.json`).
pub fn tree_file_name(frame_seq: u64) -> String {
    format!("{TREE_DIR}/{frame_seq}.json")
}

/// Log artifact path (`logs/logcat.txt`).
pub fn logcat_file_name() -> String {
    format!("{LOGS_DIR}/logcat.txt")
}

/// One artifact-validation problem.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ArtifactIssue {
    /// Stable code (`ARTIFACTS_INVALID` today).
    pub code: ErrorCode,
    /// Path of the offending artifact.
    pub path: String,
    /// What is wrong.
    pub message: String,
}

impl ArtifactIssue {
    fn new(path: impl Into<String>, message: impl Into<String>) -> Self {
        ArtifactIssue {
            code: ErrorCode::ArtifactsInvalid,
            path: path.into(),
            message: message.into(),
        }
    }
}

/// Result of validating an artifacts directory.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
pub struct ArtifactValidation {
    /// True when the directory is internally consistent and complete.
    pub ok: bool,
    /// Every problem found.
    pub issues: Vec<ArtifactIssue>,
}

impl ArtifactValidation {
    /// Empty, successful validation.
    pub fn ok() -> Self {
        ArtifactValidation {
            ok: true,
            issues: Vec::new(),
        }
    }

    fn push(&mut self, path: impl Into<String>, message: impl Into<String>) {
        self.ok = false;
        self.issues.push(ArtifactIssue::new(path, message));
    }
}

/// Resolve a relative artifact path inside `dir`, rejecting escapes.
fn resolve_inside(dir: &Path, relative: &str) -> Result<PathBuf, String> {
    if relative.trim().is_empty() {
        return Err("artifact path is empty".to_string());
    }
    let rel = Path::new(relative);
    if rel.is_absolute() {
        return Err("artifact path must be relative".to_string());
    }
    for component in rel.components() {
        match component {
            Component::Normal(_) | Component::CurDir => {}
            Component::ParentDir => return Err("artifact path must not contain `..`".to_string()),
            Component::RootDir | Component::Prefix(_) => {
                return Err("artifact path must be relative".to_string())
            }
        }
    }
    let joined = dir.join(rel);
    if joined.exists() {
        let canonical_dir = dir
            .canonicalize()
            .map_err(|e| format!("cannot canonicalize artifacts dir: {e}"))?;
        let canonical_file = joined
            .canonicalize()
            .map_err(|e| format!("cannot canonicalize artifact path: {e}"))?;
        if !canonical_file.starts_with(&canonical_dir) {
            return Err("artifact path escapes the artifacts directory (symlink?)".to_string());
        }
    }
    Ok(joined)
}

fn validate_json_file(
    validation: &mut ArtifactValidation,
    dir: &Path,
    relative: &str,
    label: &str,
) {
    match resolve_inside(dir, relative) {
        Err(message) => validation.push(relative, message),
        Ok(path) => {
            if !path.exists() {
                validation.push(relative, format!("missing {label}"));
                return;
            }
            if let Ok(bytes) = std::fs::read(&path) {
                if serde_json::from_slice::<serde_json::Value>(&bytes).is_err() {
                    validation.push(relative, format!("{label} is not valid JSON"));
                }
            } else {
                validation.push(relative, format!("cannot read {label}"));
            }
        }
    }
}

fn validate_events_file(validation: &mut ArtifactValidation, dir: &Path) {
    match resolve_inside(dir, EVENTS_JSONL) {
        Err(message) => validation.push(EVENTS_JSONL, message),
        Ok(path) => {
            if !path.exists() {
                validation.push(EVENTS_JSONL, "missing events stream");
                return;
            }
            match std::fs::read_to_string(&path) {
                Err(e) => validation.push(EVENTS_JSONL, format!("cannot read events: {e}")),
                Ok(text) => {
                    for (line_no, line) in text.lines().enumerate() {
                        if line.trim().is_empty() {
                            continue;
                        }
                        if serde_json::from_str::<serde_json::Value>(line).is_err() {
                            validation.push(
                                EVENTS_JSONL,
                                format!("line {} is not a JSON object", line_no + 1),
                            );
                        }
                    }
                }
            }
        }
    }
}

/// Validate an artifacts directory against its own `verdict.json`.
///
/// For a claimed pass this enforces the complete evidence chain. For a non-pass
/// verdict only `verdict.json` is required, but malformed hashes/reproducers are
/// still reported because they would break reproduction.
pub fn validate_artifacts_dir(dir: &Path) -> ArtifactValidation {
    let mut validation = ArtifactValidation::ok();
    if !dir.is_dir() {
        validation.push(
            dir.display().to_string(),
            "artifacts directory does not exist or is not a directory",
        );
        return validation;
    }

    let verdict_path = dir.join(VERDICT_JSON);
    let verdict_bytes = match std::fs::read(&verdict_path) {
        Ok(bytes) => bytes,
        Err(e) => {
            validation.push(
                VERDICT_JSON,
                format!("missing/unreadable verdict.json: {e}"),
            );
            return validation;
        }
    };
    let verdict: Verdict = match serde_json::from_slice(&verdict_bytes) {
        Ok(verdict) => verdict,
        Err(e) => {
            validation.push(VERDICT_JSON, format!("verdict.json is invalid: {e}"));
            return validation;
        }
    };

    if verdict.schema_version != VERDICT_SCHEMA_VERSION {
        validation.push(
            VERDICT_JSON,
            format!(
                "unsupported verdict schema_version {} (expected {})",
                verdict.schema_version, VERDICT_SCHEMA_VERSION
            ),
        );
    }

    // One outcome, two field names: `verdict` (historical) and `status` (the
    // envelope's). They must agree, so a reader that checks one never sees a
    // different answer than a reader that checks the other.
    if verdict.status != verdict.verdict {
        validation.push(
            VERDICT_JSON,
            format!(
                "verdict.status `{}` does not match verdict `{}`",
                verdict.status.as_str(),
                verdict.verdict.as_str()
            ),
        );
    }

    // `captures` names the PNG of every `capture` step by the scenario's own
    // label. A declared path must exist (for every status), and every step that
    // carries a name must be in the map (no silent half-mapping).
    for (name, path) in &verdict.captures {
        check_evidence_path(&mut validation, dir, path);
        if name.trim().is_empty() {
            validation.push(VERDICT_JSON, "captures contains an empty name");
        }
    }
    for step in &verdict.steps {
        if let Some(name) = &step.name {
            match verdict.captures.get(name) {
                Some(path) if !path.trim().is_empty() => {}
                Some(_) => validation.push(
                    VERDICT_JSON,
                    format!(
                        "captures[{name:?}] for step {} is an empty path",
                        step.index
                    ),
                ),
                None => validation.push(
                    VERDICT_JSON,
                    format!(
                        "step {} (capture {name:?}) is missing from verdict.captures",
                        step.index
                    ),
                ),
            }
        }
    }

    // Reproducer consistency is required for every verdict.
    if verdict.reproducer.apk_sha256.trim().is_empty() {
        validation.push(VERDICT_JSON, "reproducer.apk_sha256 is empty");
    }
    if verdict.reproducer.scenario_sha256.trim().is_empty() {
        validation.push(VERDICT_JSON, "reproducer.scenario_sha256 is empty");
    }
    if verdict.reproducer.apk_sha256 != verdict.app.sha256 {
        validation.push(
            VERDICT_JSON,
            "reproducer.apk_sha256 does not match app.sha256",
        );
    }
    if verdict.reproducer.scenario_sha256 != verdict.scenario.sha256 {
        validation.push(
            VERDICT_JSON,
            "reproducer.scenario_sha256 does not match scenario.sha256",
        );
    }
    let mut sorted_flags = verdict.reproducer.flags.clone();
    sorted_flags.sort();
    sorted_flags.dedup();
    if sorted_flags != verdict.reproducer.flags {
        validation.push(
            VERDICT_JSON,
            "reproducer.flags must be sorted and de-duplicated for determinism",
        );
    }
    validate_reproducer_shape(&mut validation, &verdict.reproducer);

    if !verdict.claims_pass() {
        // Non-pass verdicts are allowed to omit heavy evidence, but any declared
        // path must still be safe/existing.
        for step in &verdict.steps {
            for evidence in &step.evidence {
                check_evidence_path(&mut validation, dir, &evidence.path);
            }
        }
        return validation;
    }

    // ---- pass-only strict evidence checks ---------------------------------
    if verdict.failure.is_some() {
        validation.push(VERDICT_JSON, "a pass verdict must not carry a failure");
    }
    if verdict.steps.is_empty() {
        validation.push(
            VERDICT_JSON,
            "a pass verdict must contain at least one step",
        );
    }
    for step in &verdict.steps {
        let step_path = format!("{}[{}]", VERDICT_JSON, step.index);
        if !step.is_pass() {
            validation.push(
                &step_path,
                format!("overall pass but step {} is {}", step.index, step.status),
            );
        }
        if step.error.is_some() {
            validation.push(&step_path, "a passing step must not carry an error");
        }
        if step.evidence.is_empty() {
            validation.push(
                &step_path,
                "a passing step must declare at least one evidence artifact",
            );
        }
        for evidence in &step.evidence {
            check_evidence_path(&mut validation, dir, &evidence.path);
        }
        if let Some(frame_seq) = step.frame_seq {
            let frame = frame_file_name(frame_seq);
            check_evidence_path(&mut validation, dir, &frame);
            let tree = tree_file_name(frame_seq);
            if resolve_inside(dir, &tree)
                .map(|p| !p.exists())
                .unwrap_or(false)
            {
                validation.push(tree, "missing UI tree for a claimed frame");
            }
        }

        let step_artifact = step_file_name(step.index, &step.step);
        match resolve_inside(dir, &step_artifact) {
            Err(message) => validation.push(step_artifact, message),
            Ok(path) => {
                if !path.exists() {
                    validation.push(
                        step_artifact,
                        "missing per-step result artifact for a claimed pass",
                    );
                } else if let Ok(bytes) = std::fs::read(&path) {
                    if serde_json::from_slice::<serde_json::Value>(&bytes).is_err() {
                        validation.push(step_artifact, "step artifact is not valid JSON");
                    }
                }
            }
        }
    }

    // Required run-level evidence for a pass.
    let logs_dir = dir.join(LOGS_DIR);
    match std::fs::read_dir(&logs_dir) {
        Err(_) => validation.push(LOGS_DIR, "missing logs directory for a claimed pass"),
        Ok(entries) => {
            if entries.filter_map(Result::ok).next().is_none() {
                validation.push(LOGS_DIR, "logs directory is empty for a claimed pass");
            }
        }
    }
    validate_reproducer_files(&mut validation, dir);
    validate_events_file(&mut validation, dir);
    validate_json_file(&mut validation, dir, RESOURCES_JSON, "resource report");
    // `repro.json` must be present and parseable.
    validate_json_file(&mut validation, dir, REPRO_JSON, "reproducer JSON");

    validation
}

fn validate_reproducer_shape(validation: &mut ArtifactValidation, repro: &Reproducer) {
    if repro.acos_version.trim().is_empty() {
        validation.push(VERDICT_JSON, "reproducer.acos_version is empty");
    }
    if repro.apk_path.trim().is_empty() {
        validation.push(VERDICT_JSON, "reproducer.apk_path is empty");
    }
    if repro.scenario_path.trim().is_empty() {
        validation.push(VERDICT_JSON, "reproducer.scenario_path is empty");
    }
}

fn validate_reproducer_files(validation: &mut ArtifactValidation, dir: &Path) {
    for (relative, label) in [
        (REPRO_JSON, "reproducer JSON"),
        (REPRO_SH, "reproducer script"),
    ] {
        match resolve_inside(dir, relative) {
            Err(message) => validation.push(relative, message),
            Ok(path) => {
                if !path.exists() {
                    validation.push(relative, format!("missing {label} for a claimed pass"));
                }
            }
        }
    }
    validate_json_file(validation, dir, REPRO_JSON, "reproducer JSON");
}

fn check_evidence_path(validation: &mut ArtifactValidation, dir: &Path, relative: &str) {
    match resolve_inside(dir, relative) {
        Err(message) => validation.push(relative, message),
        Ok(path) => {
            if !path.exists() {
                validation.push(relative, "declared evidence artifact is missing");
            }
        }
    }
}

/// A fresh, unique test directory under the system temp dir (no external deps).
#[cfg(test)]
fn test_dir(name: &str) -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "acos-protocol-{}-{}-{n}-{name}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[cfg(test)]
fn write_file(dir: &Path, relative: &str, contents: &[u8]) {
    let path = dir.join(relative);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, contents).unwrap();
}

#[cfg(test)]
fn sample_verdict(step_evidence: Vec<crate::verdict::ArtifactRef>) -> Verdict {
    use crate::envelope::{Backend, Fidelity};
    use crate::identity::{DisplaySpec, IsolationLevel};
    use crate::verdict::{AppIdentity, ScenarioIdentity, StepResult};

    let repro = Reproducer::new(
        "0.1.0",
        "fixtures/build/apps/hello-views.apk",
        "apk-sha",
        "scenarios/hello-views.yaml",
        "scenario-sha",
        IsolationLevel::Sandbox,
        DisplaySpec::offscreen(1080, 2400, 420),
        Fidelity::NativeRuntime,
        Backend::AcosNative,
    )
    .with_flags(["--fidelity", "native-preferred"]);
    let mut verdict = Verdict::new(
        Status::Pass,
        Fidelity::NativeRuntime,
        Backend::AcosNative,
        IsolationLevel::Sandbox,
        "req_verify",
        "2026-11-10T13:00:00Z",
        "2026-11-10T13:00:01Z",
        AppIdentity {
            path: "fixtures/build/apps/hello-views.apk".to_string(),
            package: Some("dev.acos.fixtures.helloviews".to_string()),
            sha256: "apk-sha".to_string(),
        },
        ScenarioIdentity {
            path: "scenarios/hello-views.yaml".to_string(),
            name: "hello-views".to_string(),
            sha256: "scenario-sha".to_string(),
        },
        repro,
    );
    let mut step = StepResult::new(0, "launch", Status::Pass, 12);
    step.frame_seq = Some(7);
    step.evidence = step_evidence;
    verdict.steps.push(step);
    verdict
}

#[cfg(test)]
fn write_valid_dir(dir: &Path, verdict: &Verdict) {
    write_file(
        dir,
        VERDICT_JSON,
        serde_json::to_string_pretty(verdict).unwrap().as_bytes(),
    );
    write_file(dir, "steps/00-launch.json", br#"{"status":"pass"}"#);
    write_file(dir, "frames/7.png", b"png");
    write_file(dir, "tree/7.json", br#"{"nodes":[]}"#);
    write_file(dir, EVENTS_JSONL, b"{\"seq\":1,\"kind\":\"lifecycle\"}\n");
    write_file(dir, &logcat_file_name(), b"I AcosFixture: onCreate\n");
    write_file(dir, RESOURCES_JSON, br#"{"wall_ms":12}"#);
    write_file(
        dir,
        REPRO_JSON,
        serde_json::to_string_pretty(&verdict.reproducer)
            .unwrap()
            .as_bytes(),
    );
    write_file(
        dir,
        REPRO_SH,
        b"#!/bin/sh\nacos verify --apk a.apk --scenario s.yaml\n",
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::verdict::ArtifactRef;

    fn evidence() -> Vec<ArtifactRef> {
        vec![
            ArtifactRef::new("frame", frame_file_name(7)),
            ArtifactRef::new("tree", tree_file_name(7)),
        ]
    }

    #[test]
    fn valid_directory_is_accepted() {
        let dir = test_dir("valid");
        let verdict = sample_verdict(evidence());
        write_valid_dir(&dir, &verdict);
        let validation = validate_artifacts_dir(&dir);
        assert!(validation.ok, "{:?}", validation.issues);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn missing_frame_makes_claimed_pass_invalid() {
        let dir = test_dir("missing-frame");
        let verdict = sample_verdict(evidence());
        write_valid_dir(&dir, &verdict);
        std::fs::remove_file(dir.join(frame_file_name(7))).unwrap();
        let validation = validate_artifacts_dir(&dir);
        assert!(!validation.ok);
        assert!(
            validation
                .issues
                .iter()
                .any(|i| i.path == frame_file_name(7)),
            "{:?}",
            validation.issues
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn claimed_pass_without_evidence_is_invalid() {
        let dir = test_dir("no-evidence");
        let verdict = sample_verdict(Vec::new());
        write_valid_dir(&dir, &verdict);
        let validation = validate_artifacts_dir(&dir);
        assert!(!validation.ok);
        assert!(
            validation
                .issues
                .iter()
                .any(|i| i.message.contains("at least one evidence")),
            "{:?}",
            validation.issues
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn missing_step_artifact_is_invalid() {
        let dir = test_dir("missing-step");
        let verdict = sample_verdict(evidence());
        write_valid_dir(&dir, &verdict);
        std::fs::remove_file(dir.join("steps/00-launch.json")).unwrap();
        let validation = validate_artifacts_dir(&dir);
        assert!(!validation.ok);
        assert!(
            validation
                .issues
                .iter()
                .any(|i| i.path == "steps/00-launch.json"),
            "{:?}",
            validation.issues
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn path_escape_is_rejected() {
        let dir = test_dir("escape");
        let verdict = sample_verdict(vec![ArtifactRef::new("frame", "../../etc/passwd")]);
        write_valid_dir(&dir, &verdict);
        let validation = validate_artifacts_dir(&dir);
        assert!(!validation.ok);
        assert!(
            validation.issues.iter().any(|i| i.message.contains("`..`")),
            "{:?}",
            validation.issues
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn non_pass_verdict_does_not_require_evidence() {
        let dir = test_dir("non-pass");
        let mut verdict = sample_verdict(Vec::new());
        verdict.verdict = Status::Fail;
        verdict.status = Status::Fail;
        verdict.steps[0].status = Status::Fail;
        verdict.steps[0].frame_seq = None;
        write_file(
            &dir,
            VERDICT_JSON,
            serde_json::to_string_pretty(&verdict).unwrap().as_bytes(),
        );
        let validation = validate_artifacts_dir(&dir);
        assert!(validation.ok, "{:?}", validation.issues);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn capture_names_are_sanitised_and_deterministic() {
        // Plain labels pass through.
        assert_eq!(
            sanitize_capture_name("meanings-collapsed"),
            "meanings-collapsed"
        );
        // Sloppy labels become one safe component: no separators, no spaces.
        assert_eq!(
            sanitize_capture_name("meanings collapsed"),
            "meanings-collapsed"
        );
        assert_eq!(sanitize_capture_name("a/b\\c:d"), "a-b-c-d");
        assert_eq!(sanitize_capture_name("  leading"), "leading");
        // Non-ASCII has no shell-safe representation: a placeholder, never a
        // path that a caller cannot type.
        assert_eq!(sanitize_capture_name("猫 expanded"), "expanded");
        assert_eq!(sanitize_capture_name("猫"), "capture");
        assert_eq!(sanitize_capture_name(""), "capture");
        assert_eq!(sanitize_capture_name(".."), "capture");
        // Long names are capped; the mapping is stable across calls.
        let long = "x".repeat(200);
        assert_eq!(sanitize_capture_name(&long).len(), 64);
        assert_eq!(sanitize_capture_name("cat"), sanitize_capture_name("cat"));
        // The file name keeps the frame sequence (unique frame identity) and the
        // label.
        assert_eq!(
            capture_frame_file_name(16, "meanings collapsed"),
            "frames/16-meanings-collapsed.png"
        );
    }

    #[test]
    fn claim_refuses_a_directory_another_run_owns() {
        let dir = test_dir("claim-owned");
        let first = RunMarker::new(
            "req_first",
            "2026-11-15T00:00:00Z",
            "s.yaml",
            "a.apk",
            "0.2.0",
        );
        claim_run_dir(&dir, &first, false).expect("an empty directory is claimable");
        write_file(&dir, "frames/16.png", b"png");

        let second = RunMarker::new(
            "req_second",
            "2026-11-15T00:01:00Z",
            "s.yaml",
            "a.apk",
            "0.2.0",
        );
        let error = claim_run_dir(&dir, &second, false)
            .expect_err("a second run must not interleave with the first");
        assert_eq!(error.code, ErrorCode::InvalidArgument);
        let details = error.details.expect("details name the previous run");
        assert_eq!(details["previous_run_id"], "req_first");
        assert_eq!(details["run_id"], "req_second");
        assert_eq!(details["overwrite"], false);
        // The refused run wrote nothing: the first run's evidence is untouched.
        assert_eq!(previous_run_id(&dir).as_deref(), Some("req_first"));
        assert!(dir.join("frames/16.png").is_file());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn claim_overwrite_replaces_the_previous_run_and_records_it() {
        let dir = test_dir("claim-overwrite");
        let first = RunMarker::new(
            "req_first",
            "2026-11-15T00:00:00Z",
            "s.yaml",
            "a.apk",
            "0.2.0",
        );
        claim_run_dir(&dir, &first, false).unwrap();
        write_file(&dir, "frames/16.png", b"png");
        write_file(&dir, VERDICT_JSON, br#"{"request_id":"req_first"}"#);

        let second = RunMarker::new(
            "req_second",
            "2026-11-15T00:01:00Z",
            "s.yaml",
            "a.apk",
            "0.2.0",
        );
        let claim = claim_run_dir(&dir, &second, true).expect("overwrite is the explicit opt-in");
        assert!(claim.overwrote);
        assert_eq!(claim.previous_run_id.as_deref(), Some("req_first"));
        // Old evidence is gone; the marker names the run that replaced it.
        assert!(!dir.join("frames/16.png").exists());
        assert!(!dir.join(VERDICT_JSON).exists());
        assert_eq!(previous_run_id(&dir).as_deref(), Some("req_second"));
        let marker: RunMarker =
            serde_json::from_slice(&std::fs::read(dir.join(RUN_JSON)).unwrap()).unwrap();
        assert_eq!(marker.overwrote_run_id.as_deref(), Some("req_first"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn claim_accepts_an_absent_or_empty_directory() {
        let dir = test_dir("claim-empty").join("nested");
        let marker = RunMarker::new("req_1", "2026-11-15T00:00:00Z", "s.yaml", "a.apk", "0.2.0");
        let claim = claim_run_dir(&dir, &marker, false).expect("an absent directory is created");
        assert!(!claim.overwrote);
        assert!(claim.previous_run_id.is_none());
        assert_eq!(previous_run_id(&dir).as_deref(), Some("req_1"));
        std::fs::remove_dir_all(dir.parent().unwrap()).unwrap();

        let empty = test_dir("claim-empty-dir");
        let claim = claim_run_dir(&empty, &marker, false).expect("an empty directory is claimable");
        assert!(!claim.overwrote);
        assert_eq!(previous_run_id(&empty).as_deref(), Some("req_1"));
        std::fs::remove_dir_all(&empty).unwrap();
    }

    #[test]
    fn claim_accepts_pre_created_empty_subdirectories() {
        // A runner may pre-create the engine's frame mirror; empty directories
        // hold no evidence, so the directory is still fresh.
        let dir = test_dir("claim-empty-subdirs");
        std::fs::create_dir_all(dir.join("art-frames/live-frames")).unwrap();
        std::fs::create_dir_all(dir.join("frames")).unwrap();
        let marker = RunMarker::new("req_9", "2026-11-15T00:00:00Z", "s.yaml", "a.apk", "0.2.0");
        let claim = claim_run_dir(&dir, &marker, false).expect("empty dirs are not evidence");
        assert!(!claim.overwrote);
        assert_eq!(previous_run_id(&dir).as_deref(), Some("req_9"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn claim_refuses_a_partial_run_without_a_marker() {
        // A crashed run leaves files but no verdict; it still owns the dir.
        let dir = test_dir("claim-partial");
        write_file(&dir, "art-frames/live-session.json", b"{}");
        let marker = RunMarker::new("req_2", "2026-11-15T00:00:00Z", "s.yaml", "a.apk", "0.2.0");
        let error = claim_run_dir(&dir, &marker, false).expect_err("not fresh");
        assert_eq!(error.code, ErrorCode::InvalidArgument);
        assert!(
            error.message.contains("incomplete earlier run"),
            "{}",
            error.message
        );
        assert!(error.details.unwrap()["previous_run_id"].is_null());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn validator_rejects_a_capture_map_that_dangles_or_is_incomplete() {
        let dir = test_dir("captures");
        let mut verdict = sample_verdict(evidence());
        verdict.steps[0].name = Some("meanings-collapsed".to_string());
        verdict.captures.insert(
            "meanings-collapsed".to_string(),
            capture_frame_file_name(7, "meanings-collapsed"),
        );
        write_valid_dir(&dir, &verdict);
        write_file(&dir, "frames/7-meanings-collapsed.png", b"png");
        let validation = validate_artifacts_dir(&dir);
        assert!(validation.ok, "{:?}", validation.issues);

        // A name with no file is a dangling declaration.
        std::fs::remove_file(dir.join(capture_frame_file_name(7, "meanings-collapsed"))).unwrap();
        let validation = validate_artifacts_dir(&dir);
        assert!(!validation.ok);
        assert!(
            validation
                .issues
                .iter()
                .any(|i| i.path == capture_frame_file_name(7, "meanings-collapsed")),
            "{:?}",
            validation.issues
        );

        // A named step missing from the map is a half-mapping.
        write_file(&dir, "frames/7-meanings-collapsed.png", b"png");
        verdict.captures.clear();
        write_file(
            &dir,
            VERDICT_JSON,
            serde_json::to_string_pretty(&verdict).unwrap().as_bytes(),
        );
        let validation = validate_artifacts_dir(&dir);
        assert!(!validation.ok);
        assert!(
            validation
                .issues
                .iter()
                .any(|i| i.message.contains("missing from verdict.captures")),
            "{:?}",
            validation.issues
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn validator_rejects_status_that_disagrees_with_verdict() {
        let dir = test_dir("status-mismatch");
        let mut verdict = sample_verdict(evidence());
        verdict.status = Status::Fail;
        write_valid_dir(&dir, &verdict);
        let validation = validate_artifacts_dir(&dir);
        assert!(!validation.ok);
        assert!(
            validation
                .issues
                .iter()
                .any(|i| i.message.contains("verdict.status")),
            "{:?}",
            validation.issues
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
