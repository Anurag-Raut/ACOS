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

/// One attempt at a step the engine may perform more than once.
///
/// A step is one entry in `verdict.json` and in the reply's `result.verdict`;
/// an attempt is never a second entry. `StepResult.status` is the *final*
/// outcome and `StepResult.attempts` is the history that produced it, so a
/// passing step can never look failing because an earlier attempt was retried.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct StepAttempt {
    /// 1-based attempt number (`1` is the first real gesture/traversal).
    pub attempt: usize,
    /// The attempt's own outcome.
    pub status: Status,
    /// Why it ended that way, in one line (the miss reason, the error).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// The settled traversal this attempt judged, when it took one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frame_seq: Option<u64>,
}

impl StepAttempt {
    /// Build one attempt record.
    #[must_use]
    pub fn new(attempt: usize, status: Status, reason: impl Into<String>) -> Self {
        StepAttempt {
            attempt,
            status,
            reason: Some(reason.into()),
            frame_seq: None,
        }
    }

    /// Attach the settled frame this attempt judged.
    #[must_use]
    pub fn with_frame(mut self, frame_seq: u64) -> Self {
        self.frame_seq = Some(frame_seq);
        self
    }
}

/// Where an input step's `handled` answer came from (REL3I3).
///
/// The field exists because "the app handled it" has two honest proofs that
/// are *not* interchangeable:
///
/// * [`HandledSource::Guest`] — the app window's own
///   `InputEventReceiver.finishInputEvent(handled)` bit, one per acknowledged
///   `InputMessage`;
/// * [`HandledSource::HostWindowRemoved`] — the host's own record that the
///   window this input was delivered to was removed during the step, so its
///   receiver was destroyed before it could finish the event. That is what
///   AOSP sees when a dialog's Back dismisses the dialog: the press *did* act
///   (the window it acted on is gone), but no ack can exist.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum HandledSource {
    /// A guest `finishInputEvent` ack.
    Guest,
    /// The delivered window was removed within the step.
    HostWindowRemoved,
    /// No input was delivered, so there is nothing to have handled.
    None,
}

/// The delivery evidence behind [`StepInputResult::handled`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct HandledEvidence {
    /// Which fact the `handled` verdict rests on.
    pub source: HandledSource,
    /// `InputMessage`s the host published for the gesture.
    pub published: usize,
    /// `FINISHED` acks the guest returned for them.
    pub acked: usize,
    /// Sequence numbers the guest answered `handled: false`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unhandled: Vec<u64>,
}

impl HandledEvidence {
    /// Build the evidence block.
    #[must_use]
    pub fn new(source: HandledSource, published: usize, acked: usize, unhandled: Vec<u64>) -> Self {
        Self {
            source,
            published,
            acked,
            unhandled,
        }
    }

    /// Nothing was delivered.
    #[must_use]
    pub fn none() -> Self {
        Self::new(HandledSource::None, 0, 0, Vec::new())
    }
}

/// REL3E: the `input` block of a step result.
///
/// `handled` is the app's own `finishInputEvent(handled)` answer, or the host's
/// window-removal fact when the app could not answer; `handled_evidence` says
/// which. `target` is the resolved node plus the hit-test at the injected point
/// and `expect_change` is the rule the step asked for (`"auto"` for a `tap`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct StepInputResult {
    /// The consumer's verdict (`null` when the guest did not ack).
    pub handled: Option<bool>,
    /// The facts the verdict rests on.
    pub handled_evidence: HandledEvidence,
    /// The resolved node plus the hit-test at the injected point.
    pub target: serde_json::Value,
    /// `true`/`false`, or `"auto"` for a tap (the step's own check).
    pub expect_change: serde_json::Value,
    /// The `scroll_until`/`wait_for` search outcome, when the action searched.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub until: Option<serde_json::Value>,
    /// One line explaining a `handled`/hit-test combination that is not
    /// self-evident (REL3E).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handled_hint: Option<String>,
}

/// One step in a scenario run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct StepResult {
    /// Zero-based step index (artifact file is `steps/NN-<step>.json`).
    pub index: usize,
    /// Step kind (`launch`, `assert_text`, ...).
    pub step: String,
    /// Step outcome. The **final** one: a retried attempt never changes this
    /// into a second entry.
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
    /// or the host's window-removal fact when the delivered window died before
    /// it could answer (`handled_evidence.source`). `target` is what the
    /// selector resolved to plus the hit-test at the injected point, and
    /// `expect_change` says whether a change was required - `"auto"` for a
    /// `tap` (the step's own state-change check), `true`/`false` for the
    /// scenario's explicit flag. `None` for steps that inject no input.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<StepInputResult>,
    /// REL3H2: structured, non-fatal facts about this step (for example
    /// `INPUT_UNHANDLED`: the gesture was delivered and nothing consumed it).
    /// The step's status is exactly as measured; a warning never changes it, and
    /// the same records are copied into the verdict's `provenance.warnings[]` so
    /// a reader of either file sees them. Empty (and absent on the wire) when
    /// there is nothing to say.
    ///
    /// A warning is a *guest* fact (`finishInputEvent(handled) = false`), so a
    /// gesture whose delivered window the host removed reports no warning: that
    /// input is handled (`handled_evidence.source: host_window_removed`), which
    /// is the MERGE3I3 precedence (see `acos_protocol::ui::handled_source`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<serde_json::Value>,
    /// Evidence backing this step. A passing step must have at least one entry.
    #[serde(default)]
    pub evidence: Vec<ArtifactRef>,
    /// Error, when the step did not pass.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ProtocolError>,
    /// Every attempt the step took, in order, when it took more than one (a
    /// `scroll_until` search performs one real gesture + fresh traversal per
    /// attempt). `status` above is the last attempt's outcome; this array says
    /// how it got there. Absent when the step succeeded on its first attempt.
    ///
    /// One entry per step is the rule that keeps `verdict.json` and the inline
    /// reply's `result.verdict` the same document: both are built from this
    /// struct, so neither can carry a "retried non-pass" step in a passing run.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attempts: Vec<StepAttempt>,
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
            attempts: Vec::new(),
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
