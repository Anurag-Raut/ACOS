//! Artifacts layout for `acos verify --artifacts <dir>` and its validator.
//!
//! Layout (all paths relative to the artifacts directory):
//!
//! ```text
//! verdict.json
//! steps/00-launch.json
//! steps/01-assert_text.json
//! frames/19.png
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

use std::path::{Component, Path, PathBuf};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::error::ErrorCode;
use crate::verdict::{Reproducer, Verdict, VERDICT_SCHEMA_VERSION};

#[cfg(test)]
use crate::envelope::Status;

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
}
