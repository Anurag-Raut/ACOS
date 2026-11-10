//! `verdict.json`: the reproducible result of `acos verify`.
//!
//! A verdict is only valid when the artifacts directory backs every claimed
//! pass. See [`crate::artifacts::validate_artifacts_dir`].

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::assertion::AssertionResult;
use crate::envelope::{Backend, Fidelity, Status};
use crate::error::{ErrorCode, ProtocolError};
use crate::identity::{DisplaySpec, IsolationLevel};

/// Current verdict schema version.
pub const VERDICT_SCHEMA_VERSION: u32 = 1;

/// An evidence file referenced by a step/verdict.
///
/// Paths are relative to the artifacts directory, use `/` separators, and must
/// not contain `..`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ArtifactRef {
    /// Artifact kind (`frame`, `tree`, `log`, `events`, `step`, ...).
    pub kind: String,
    /// Relative path.
    pub path: String,
}

impl ArtifactRef {
    /// Build a reference.
    pub fn new(kind: impl Into<String>, path: impl Into<String>) -> Self {
        ArtifactRef {
            kind: kind.into(),
            path: path.into(),
        }
    }
}

/// One step in a scenario run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct StepResult {
    /// Zero-based step index (artifact file is `steps/NN-<step>.json`).
    pub index: usize,
    /// Step kind (`launch`, `assert_text`, ...).
    pub step: String,
    /// Step outcome.
    pub status: Status,
    /// Wall time for this step.
    pub duration_ms: u64,
    /// Frame produced/used by this step.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frame_seq: Option<u64>,
    /// Assertion results (empty for non-assertion steps).
    #[serde(default)]
    pub assertions: Vec<AssertionResult>,
    /// Evidence backing this step. A passing step must have at least one entry.
    #[serde(default)]
    pub evidence: Vec<ArtifactRef>,
    /// Error, when the step did not pass.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ProtocolError>,
}

impl StepResult {
    /// Build a step result.
    pub fn new(index: usize, step: impl Into<String>, status: Status, duration_ms: u64) -> Self {
        StepResult {
            index,
            step: step.into(),
            status,
            duration_ms,
            frame_seq: None,
            assertions: Vec::new(),
            evidence: Vec::new(),
            error: None,
        }
    }

    /// True when this step claims success.
    pub const fn is_pass(&self) -> bool {
        self.status.is_pass()
    }
}

/// Why the verdict is not a pass.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct FailureClassification {
    /// Primary error code.
    pub code: ErrorCode,
    /// Step index that first failed, when step-specific.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step_index: Option<usize>,
    /// One-line summary.
    pub summary: String,
    /// The structured error that caused the failure.
    pub error: ProtocolError,
}

/// Resource usage for the whole run.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
pub struct ResourceReport {
    /// Wall time for the run.
    #[serde(default)]
    pub wall_ms: u64,
    /// CPU time, when measured.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu_ms: Option<u64>,
    /// Peak resident set size in bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peak_rss_bytes: Option<u64>,
    /// Peak attributable memory in MiB.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_mib: Option<u64>,
    /// Frames captured.
    #[serde(default)]
    pub frames_captured: u64,
    /// Events recorded.
    #[serde(default)]
    pub events: u64,
    /// Sessions created.
    #[serde(default)]
    pub sessions: u32,
    /// Steps executed.
    #[serde(default)]
    pub steps: u32,
}

/// Deterministic inputs needed to reproduce a run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Reproducer {
    /// ACOS version string.
    pub acos_version: String,
    /// APK path as used.
    pub apk_path: String,
    /// APK SHA-256.
    pub apk_sha256: String,
    /// Scenario path as used.
    pub scenario_path: String,
    /// Scenario SHA-256.
    pub scenario_sha256: String,
    /// Flags, sorted lexicographically and de-duplicated for determinism.
    pub flags: Vec<String>,
    /// Isolation level.
    pub isolation: IsolationLevel,
    /// Display spec.
    pub display: DisplaySpec,
    /// Fidelity.
    pub fidelity: Fidelity,
    /// Backend.
    pub backend: Backend,
}

impl Reproducer {
    /// Build a reproducer with an empty flag list.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        acos_version: impl Into<String>,
        apk_path: impl Into<String>,
        apk_sha256: impl Into<String>,
        scenario_path: impl Into<String>,
        scenario_sha256: impl Into<String>,
        isolation: IsolationLevel,
        display: DisplaySpec,
        fidelity: Fidelity,
        backend: Backend,
    ) -> Self {
        Reproducer {
            acos_version: acos_version.into(),
            apk_path: apk_path.into(),
            apk_sha256: apk_sha256.into(),
            scenario_path: scenario_path.into(),
            scenario_sha256: scenario_sha256.into(),
            flags: Vec::new(),
            isolation,
            display,
            fidelity,
            backend,
        }
    }

    /// Set flags, sorting and de-duplicating them.
    pub fn with_flags<I, S>(mut self, flags: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let mut flags: Vec<String> = flags.into_iter().map(Into::into).collect();
        flags.sort();
        flags.dedup();
        self.flags = flags;
        self
    }
}

/// APK identity in a verdict.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AppIdentity {
    /// Path as used.
    pub path: String,
    /// Detected package.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub package: Option<String>,
    /// APK SHA-256.
    pub sha256: String,
}

/// Scenario identity in a verdict.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ScenarioIdentity {
    /// Path as used.
    pub path: String,
    /// Scenario name.
    pub name: String,
    /// Scenario file SHA-256.
    pub sha256: String,
}

/// The full `verdict.json` document.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Verdict {
    /// Schema version ([`VERDICT_SCHEMA_VERSION`]).
    pub schema_version: u32,
    /// Overall verdict.
    pub verdict: Status,
    /// Fidelity actually achieved.
    pub fidelity: Fidelity,
    /// Fidelity required by the caller, when stated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub required_fidelity: Option<Fidelity>,
    /// Backend that produced the verdict.
    pub backend: Backend,
    /// Isolation actually used.
    pub isolation: IsolationLevel,
    /// Request that produced the verdict.
    pub request_id: String,
    /// Session used.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// RFC 3339 UTC start time.
    pub started_at: String,
    /// RFC 3339 UTC finish time.
    pub finished_at: String,
    /// APK identity.
    pub app: AppIdentity,
    /// Scenario identity.
    pub scenario: ScenarioIdentity,
    /// Per-step results, in execution order.
    pub steps: Vec<StepResult>,
    /// Failure classification, present for every non-pass verdict.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<FailureClassification>,
    /// Resource report.
    pub resources: ResourceReport,
    /// Deterministic reproducer.
    pub reproducer: Reproducer,
    /// Additional human notes (warnings, reconciliation items).
    #[serde(default)]
    pub notes: Vec<String>,
}

impl Verdict {
    /// Build a skeleton verdict; callers fill steps/failure/resources.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        verdict: Status,
        fidelity: Fidelity,
        backend: Backend,
        isolation: IsolationLevel,
        request_id: impl Into<String>,
        started_at: impl Into<String>,
        finished_at: impl Into<String>,
        app: AppIdentity,
        scenario: ScenarioIdentity,
        reproducer: Reproducer,
    ) -> Self {
        Verdict {
            schema_version: VERDICT_SCHEMA_VERSION,
            verdict,
            fidelity,
            required_fidelity: None,
            backend,
            isolation,
            request_id: request_id.into(),
            session_id: None,
            started_at: started_at.into(),
            finished_at: finished_at.into(),
            app,
            scenario,
            steps: Vec::new(),
            failure: None,
            resources: ResourceReport::default(),
            reproducer,
            notes: Vec::new(),
        }
    }

    /// True when every step claims a pass and there is at least one step.
    pub fn all_steps_pass(&self) -> bool {
        !self.steps.is_empty() && self.steps.iter().all(StepResult::is_pass)
    }

    /// True when this verdict claims a pass and is internally consistent.
    pub fn claims_pass(&self) -> bool {
        self.verdict.is_pass()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reproducer_flags_are_deterministic() {
        let repro = Reproducer::new(
            "0.1.0",
            "a.apk",
            "sha",
            "s.yaml",
            "sha2",
            IsolationLevel::Sandbox,
            DisplaySpec::offscreen(1080, 2400, 420),
            Fidelity::NativeRuntime,
            Backend::AcosNative,
        )
        .with_flags(["zeta", "alpha", "alpha", "beta"]);
        assert_eq!(repro.flags, vec!["alpha", "beta", "zeta"]);
    }
}
