//! `verdict.json`: the reproducible result of `acos verify`.
//!
//! A verdict is only valid when the artifacts directory backs every claimed
//! pass. See [`crate::artifacts::validate_artifacts_dir`].

use std::collections::BTreeMap;

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
    /// `capture` step name (the scenario's own label for this screenshot).
    ///
    /// Written into the step JSON so "the screenshot for X" is answerable from
    /// the artifacts; the same name keys `verdict.json`'s `captures` map.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Assertion results (empty for non-assertion steps).
    #[serde(default)]
    pub assertions: Vec<AssertionResult>,
    /// Settle evidence for an input step (P12): `mode`, `settled`, `bound`,
    /// `pixels_changed`, `reason`, ... `None` for steps with no settle reply.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settle: Option<serde_json::Value>,
    /// REL3E: the input step's own evidence. `handled` is the app window's
    /// `finishInputEvent(handled)` answer (`null` when the guest did not ack),
    /// `target` is what the selector resolved to plus the hit-test at the
    /// injected point, and `expect_change` says whether a change was required -
    /// `"auto"` for a `tap` (the step's own state-change check), `true`/`false`
    /// for the scenario's explicit flag. `None` for steps that inject no input.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<serde_json::Value>,
    /// REL3H2: structured, non-fatal facts about this step (for example
    /// `INPUT_UNHANDLED`: the gesture was delivered and nothing consumed it).
    /// The step's status is exactly as measured; a warning never changes it, and
    /// the same records are copied into the verdict's `provenance.warnings[]` so
    /// a reader of either file sees them. Empty (and absent on the wire) when
    /// there is nothing to say.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<serde_json::Value>,
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
            name: None,
            assertions: Vec::new(),
            settle: None,
            input: None,
            warnings: Vec::new(),
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
    /// The same outcome as [`Verdict::verdict`], under the envelope's field name
    /// and vocabulary (`pass`/`fail`/`unsupported`/`blocked`/`error`/`timeout`/
    /// `crash`). `verdict` is kept for compatibility; a reader that checks
    /// `status` (as it does on every envelope) gets the same answer.
    #[serde(default = "legacy_status")]
    pub status: Status,
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
    /// `capture` step names mapped to the PNG that step wrote
    /// (`{"meanings-collapsed": "frames/16-meanings-collapsed.png"}`).
    /// Relative to the artifacts directory, like every other evidence path.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub captures: BTreeMap<String, String>,
    /// Failure classification, present for every non-pass verdict.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<FailureClassification>,
    /// Resource report.
    pub resources: ResourceReport,
    /// Deterministic reproducer.
    pub reproducer: Reproducer,
    /// PROVENANCE: the exact inputs this verdict judged — absolute scenario and
    /// APK paths, their SHA-256 (full and the 12-character display form), the
    /// engine that ran and the fidelity it reached, plus structured
    /// `warnings[]` (for example `SCENARIO_APP_OVERRIDDEN` when `--apk` and the
    /// scenario's own `app:` disagree). See [`crate::provenance`].
    ///
    /// Written by the verifier; the CLI re-states the same block in the
    /// envelope `result` and in the human summary line, so a reader never has to
    /// open a second file to find out which scenario and APK produced a `pass`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance: Option<serde_json::Value>,
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
            status: verdict,
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
            captures: BTreeMap::new(),
            failure: None,
            resources: ResourceReport::default(),
            reproducer,
            provenance: None,
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

/// A `verdict.json` written before the `status` field existed deserialises as
/// `fail`: never as a pass. The artifacts validator reports the mismatch, so an
/// old document can never be read as a green by accident.
fn legacy_status() -> Status {
    Status::Fail
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_mirrors_verdict_with_the_envelope_vocabulary() {
        use crate::envelope::{Backend, Fidelity};
        use crate::identity::{DisplaySpec, IsolationLevel};

        for status in [
            Status::Pass,
            Status::Fail,
            Status::Unsupported,
            Status::Blocked,
            Status::Error,
            Status::Timeout,
        ] {
            let repro = Reproducer::new(
                "0.2.0",
                "a.apk",
                "sha",
                "s.yaml",
                "sha2",
                IsolationLevel::Sandbox,
                DisplaySpec::offscreen(1080, 2400, 420),
                Fidelity::NativeRuntime,
                Backend::AcosNative,
            );
            let verdict = Verdict::new(
                status,
                Fidelity::NativeRuntime,
                Backend::AcosNative,
                IsolationLevel::Sandbox,
                "req_1",
                "2026-11-15T00:00:00Z",
                "2026-11-15T00:00:01Z",
                AppIdentity {
                    path: "a.apk".to_string(),
                    package: Some("pkg".to_string()),
                    sha256: "sha".to_string(),
                },
                ScenarioIdentity {
                    path: "s.yaml".to_string(),
                    name: "s".to_string(),
                    sha256: "sha2".to_string(),
                },
                repro,
            );
            let value = serde_json::to_value(&verdict).unwrap();
            assert_eq!(value["verdict"], status.as_str());
            assert_eq!(value["status"], status.as_str());
            assert_eq!(verdict.status, verdict.verdict);
        }
    }

    #[test]
    fn a_verdict_without_status_reads_as_fail_never_pass() {
        use crate::envelope::{Backend, Fidelity};
        use crate::identity::{DisplaySpec, IsolationLevel};
        let repro = Reproducer::new(
            "0.2.0",
            "a.apk",
            "sha",
            "s.yaml",
            "sha2",
            IsolationLevel::Sandbox,
            DisplaySpec::offscreen(1080, 2400, 420),
            Fidelity::NativeRuntime,
            Backend::AcosNative,
        );
        let verdict = Verdict::new(
            Status::Pass,
            Fidelity::NativeRuntime,
            Backend::AcosNative,
            IsolationLevel::Sandbox,
            "req_1",
            "2026-11-15T00:00:00Z",
            "2026-11-15T00:00:01Z",
            AppIdentity {
                path: "a.apk".to_string(),
                package: None,
                sha256: "sha".to_string(),
            },
            ScenarioIdentity {
                path: "s.yaml".to_string(),
                name: "s".to_string(),
                sha256: "sha2".to_string(),
            },
            repro,
        );
        let mut value = serde_json::to_value(&verdict).unwrap();
        value.as_object_mut().unwrap().remove("status");
        let legacy: Verdict = serde_json::from_value(value).unwrap();
        assert_eq!(legacy.verdict, Status::Pass);
        assert_eq!(
            legacy.status,
            Status::Fail,
            "a document without status must never be read as a pass"
        );
    }

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
