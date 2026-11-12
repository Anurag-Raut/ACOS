//! The scenario executor: run steps against a [`Backend`] and build a verdict.
//!
//! The executor is where "never pass what was not executed" is enforced:
//!
//! * a step whose capability is absent is `unsupported`, never silently skipped;
//! * execution stops at the first non-pass step, so later assertions are not
//!   evaluated against stale state;
//! * a negative scenario that somehow passes is converted into an `INTERNAL`
//!   harness failure instead of being reported as a successful test.

use std::collections::{BTreeSet, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use acos_protocol::assertion::{
    AssertionDiff, AssertionResult, RegionAssertion, ScreenshotAssertion,
};
use acos_protocol::commands::ResetScope;
use acos_protocol::events::{Event, EventBody};
use acos_protocol::scenario::{Expectation, Scenario, Step, StepRef};
use acos_protocol::ui::Bounds;
use acos_protocol::verdict::{
    AppIdentity, ArtifactRef, FailureClassification, Reproducer, ResourceReport, ScenarioIdentity,
    StepResult, Verdict,
};
use acos_protocol::{ErrorCode, Fidelity, ProtocolError, Status};

use crate::artifacts;
use crate::assertions::{
    self, eval_log, eval_no_crash, eval_not_visible, eval_state, eval_text, eval_visible, Evaluated,
};
use crate::backend::{
    Backend, BackendError, Capabilities, Capability, CrashRecord, EnvironmentChange, InputAction,
    LaunchRequest, LogRecord, SessionObservation,
};
use crate::clock::Clock;
use crate::error::VerifyError;
use crate::manifest::sha256_file;
use crate::screenshot;
use sha2::{Digest, Sha256};

/// Pacing between `wait_for` live-tree polls. Each poll is a fresh traversal,
/// so this only bounds how often the engine asks; the first sample is immediate.
const WAIT_FOR_POLL_INTERVAL_MS: u64 = 25;
/// Upper bound for the `wait_for` poll backoff (one observation per second
/// once the early fast polls have missed).
const WAIT_FOR_MAX_POLL_INTERVAL_MS: u64 = 1000;

/// Everything the executor needs about one run.
#[derive(Debug, Clone)]
pub struct RunOptions {
    /// APK to run (overrides the scenario `app`).
    pub apk: PathBuf,
    /// Scenario file path (for identity/reproducer).
    pub scenario_path: PathBuf,
    /// Artifacts output directory.
    pub artifacts_dir: PathBuf,
    /// Minimum fidelity the caller requires.
    pub required_fidelity: Option<Fidelity>,
    /// Request id.
    pub request_id: String,
    /// Deterministic flags for the reproducer.
    pub flags: Vec<String>,
    /// `acos` version string.
    pub acos_version: String,
}

/// The result of a run.
#[derive(Debug, Clone)]
pub struct RunOutcome {
    /// The verdict document.
    pub verdict: Verdict,
    /// Validation of the artifacts directory against the verdict.
    pub artifact_validation: acos_protocol::ArtifactValidation,
    /// Capabilities actually exercised.
    pub capabilities_used: Vec<String>,
}

/// Internal per-step execution state.
struct StepExec {
    status: Status,
    error: Option<ProtocolError>,
    assertions: Vec<AssertionResult>,
    observation: Option<SessionObservation>,
    produced_frame: Option<acos_protocol::FrameMeta>,
    session: Option<String>,
}

impl StepExec {
    fn pass() -> Self {
        StepExec {
            status: Status::Pass,
            error: None,
            assertions: Vec::new(),
            observation: None,
            produced_frame: None,
            session: None,
        }
    }

    fn unsupported(capability: Capability, detail: impl Into<String>) -> Self {
        let detail = detail.into();
        let error = ProtocolError::new(
            ErrorCode::UnsupportedBackend,
            format!("{detail} (missing capability `{capability}`)"),
        )
        .with_hint("This engine cannot execute this step yet; report it as unsupported rather than faking a pass.")
        .with_details(serde_json::json!({
            "capability": capability.as_str(),
            "detail": detail,
        }));
        StepExec {
            status: Status::Unsupported,
            error: Some(error),
            assertions: Vec::new(),
            observation: None,
            produced_frame: None,
            session: None,
        }
    }

    fn backend_error(error: &BackendError, backend: &str, context: &str) -> Self {
        let mut protocol = error.to_protocol_error();
        if let Some(details) = protocol.details.as_mut() {
            if let Some(object) = details.as_object_mut() {
                object.insert("backend".to_string(), serde_json::json!(backend));
                object.insert("context".to_string(), serde_json::json!(context));
            }
        }
        StepExec {
            status: error.status(),
            error: Some(protocol),
            assertions: Vec::new(),
            observation: None,
            produced_frame: None,
            session: None,
        }
    }

    fn failed(evaluated: Evaluated) -> Self {
        let error = evaluated.error.as_ref().map(|error| {
            let mut protocol = error.to_protocol_error();
            if let Some(diff) = evaluated.result.diff.as_ref() {
                if let Some(details) = protocol.details.as_mut() {
                    if let Some(object) = details.as_object_mut() {
                        object.insert(
                            "diff".to_string(),
                            serde_json::to_value(diff).unwrap_or(serde_json::Value::Null),
                        );
                    }
                }
            }
            protocol
        });
        let status = error
            .as_ref()
            .map(|error| error.code.default_status())
            .unwrap_or(Status::Fail);
        StepExec {
            status,
            error,
            assertions: vec![evaluated.result],
            observation: None,
            produced_frame: None,
            session: None,
        }
    }

    /// A step failed because the backend refused the evidence channel (for
    /// example an untrusted run refusing guest-asserted logs). The backend's
    /// own code (`INSUFFICIENT_HOST_EVIDENCE`, ...) is preserved.
    fn failed_backend(error: &BackendError) -> Self {
        StepExec {
            status: Status::Fail,
            error: Some(error.to_protocol_error()),
            assertions: Vec::new(),
            observation: None,
            produced_frame: None,
            session: None,
        }
    }

    fn passed_assertion(evaluated: Evaluated) -> Self {
        StepExec {
            status: Status::Pass,
            error: None,
            assertions: vec![evaluated.result],
            observation: None,
            produced_frame: None,
            session: None,
        }
    }
}

/// Run one scenario and write its artifacts.
pub fn run_scenario(
    scenario: &Scenario,
    options: &RunOptions,
    backend: &mut dyn Backend,
    clock: &dyn Clock,
) -> Result<RunOutcome, VerifyError> {
    let caps = backend.capabilities();
    let manifest = backend.manifest(&options.apk)?;
    artifacts::prepare(&options.artifacts_dir)?;

    let started_at = clock.now_rfc3339();
    let started_ms = clock.now_unix_ms();

    let mut steps: Vec<StepResult> = Vec::new();
    let mut all_logs: Vec<LogRecord> = Vec::new();
    let mut last_log_seq = 0u64;
    let mut step_log_cursors: Vec<usize> = Vec::new();
    let mut produced_frames: Vec<acos_protocol::FrameMeta> = Vec::new();
    let mut capabilities_used: BTreeSet<String> = BTreeSet::new();
    capabilities_used.insert(Capability::StaticManifest.as_str().to_string());

    let mut session_id: Option<String> = None;
    let mut crashed: Option<CrashRecord> = None;
    let mut status = Status::Pass;
    let mut failure: Option<FailureClassification> = None;
    let mut notes: Vec<String> = Vec::new();

    // The advisor frame-binding ledger: the completed frame every pure UI or
    // screenshot assertion must describe, plus the highest frame seen (for
    // `capture fresh`).
    let mut previous_observation: Option<SessionObservation> = None;
    let mut bound_frame_seq: Option<u64> = None;
    let mut last_frame_seq: Option<u64> = None;

    // A log fetch that failed (for example the untrusted-run refusal from
    // `TrustGuard`) is remembered so an `assert_log` step reports that exact
    // structured error instead of a misleading "no matching line".
    let mut log_failure: Option<BackendError> = None;

    for (index, step) in scenario.steps.iter().enumerate() {
        // Flush any logs the previous step produced.
        if caps.logs {
            match backend.logs_since(last_log_seq) {
                Ok(new) => {
                    for record in new {
                        last_log_seq = last_log_seq.max(record.seq);
                        all_logs.push(record);
                    }
                    log_failure = None;
                }
                Err(error) => log_failure = Some(error),
            }
        }
        step_log_cursors.push(all_logs.len());

        // The step's clock starts **before** an `assert_log` wait: the wait is
        // part of the step, so a wrong twin whose line never appears must show
        // its full `timeout_ms` on the step, not in `unattributed_ms` (W34 §3).
        let step_started = clock.now_unix_ms();

        // `assert_log.timeout_ms` is the wait-for-log semantic: the line the
        // triggering action produced may still be in flight when the step
        // starts (buffered liblog write, logd reader thread). Poll the log
        // stream until the assertion holds or the deadline passes; the
        // assertion below still decides pass/fail, so a missing line is a
        // normal assertion failure, never a fake pass.
        if caps.logs {
            if let Some(StepRef::AssertLog(assertion)) = step.as_step() {
                if let Some(timeout_ms) = assertion.timeout_ms {
                    let start = if index > 0 {
                        step_log_cursors[index - 1]
                    } else {
                        0
                    };
                    // Real elapsed time, not the injected evidence clock: a
                    // `FixedClock` never advances, and a wait must not depend
                    // on the clock a test uses to stamp evidence.
                    let deadline = Instant::now() + Duration::from_millis(timeout_ms);
                    let mut backoff_ms = 10u64;
                    loop {
                        if eval_log(assertion, &all_logs[start..]).result.passed {
                            break;
                        }
                        if Instant::now() >= deadline {
                            break;
                        }
                        std::thread::sleep(std::time::Duration::from_millis(backoff_ms));
                        backoff_ms = (backoff_ms * 2).min(100);
                        match backend.logs_since(last_log_seq) {
                            Ok(new) => {
                                for record in new {
                                    last_log_seq = last_log_seq.max(record.seq);
                                    all_logs.push(record);
                                }
                                log_failure = None;
                            }
                            Err(error) => log_failure = Some(error),
                        }
                    }
                }
            }
        }

        let since_logs: &[LogRecord] = if index > 0 {
            &all_logs[step_log_cursors[index - 1]..]
        } else {
            &all_logs[..]
        };

        let cause_id = format!("{}:{}:{}", options.request_id, step.kind(), index);
        let exec = execute_step(
            backend,
            caps,
            &manifest,
            step,
            &options.apk,
            scenario,
            &options.scenario_path,
            &cause_id,
            since_logs,
            &all_logs,
            log_failure.as_ref(),
            crashed.as_ref(),
            previous_observation.as_ref(),
            bound_frame_seq,
            last_frame_seq,
            &mut capabilities_used,
        );
        let duration_ms = clock.now_unix_ms().saturating_sub(step_started);

        // ---- W58: complete the frame evidence at the verifier's boundary ----
        // The frame is the display's application area; say where it sits on the
        // display even when the backend did not. Everything downstream (the
        // tree artifact, step results, the coordinate note) then carries the
        // same geometry an agent needs to map a tree bound onto a PNG pixel.
        let mut exec = exec;
        if let Some(observation) = &mut exec.observation {
            let signed = crate::display::signed_frame(&observation.frame);
            observation.frame = signed.clone();
            observation.tree.frame = signed;
        }

        if session_id.is_none() {
            session_id = exec
                .session
                .clone()
                .or_else(|| exec.observation.as_ref().map(|o| o.session_id.clone()));
        }
        if let Some(observation) = &exec.observation {
            if let Some(record) = &observation.crashed {
                crashed = Some(record.clone());
            }
        }

        let mut step_result = StepResult::new(index, step.kind(), exec.status, duration_ms);
        step_result.assertions = exec.assertions.clone();
        step_result.error = exec.error.clone();
        // P12: a tap/type/key reply's settle block (including pixels_changed)
        // is step evidence. It is never the pass/fail rule by itself.
        step_result.settle = exec
            .observation
            .as_ref()
            .and_then(|observation| observation.input.as_ref())
            .and_then(|delivery| delivery.settle.clone());

        // Write evidence and declare it.
        let step_artifact = acos_protocol::step_file_name(index, step.kind());
        if let Some(observation) = &exec.observation {
            let seq = observation.frame.frame_seq;
            step_result.frame_seq = Some(seq);
            artifacts::write_tree(&options.artifacts_dir, seq, &observation.tree)?;
            step_result
                .evidence
                .push(ArtifactRef::new("tree", acos_protocol::tree_file_name(seq)));
            if let Some(png) = &observation.screenshot_png {
                artifacts::write_frame(&options.artifacts_dir, seq, png)?;
                step_result.evidence.push(ArtifactRef::new(
                    "frame",
                    acos_protocol::frame_file_name(seq),
                ));
            }
            produced_frames.push(observation.frame.clone());
        } else if let Some(frame) = &exec.produced_frame {
            produced_frames.push(frame.clone());
        }
        if matches!(
            step.as_step(),
            Some(StepRef::AssertLog(_) | StepRef::AssertNoCrash(_))
        ) && caps.logs
        {
            step_result.evidence.push(ArtifactRef::new(
                "log",
                acos_protocol::artifacts::logcat_file_name(),
            ));
        }
        step_result
            .evidence
            .push(ArtifactRef::new("step", step_artifact.clone()));
        artifacts::write_step(&options.artifacts_dir, &step_result)?;

        // Advance the frame-binding ledger. Only a step that completed can
        // establish the completed frame later assertions must describe.
        if exec.status == Status::Pass {
            if let Some(observation) = &exec.observation {
                last_frame_seq = Some(observation.frame.frame_seq);
                if frame_advancing_step(step) {
                    bound_frame_seq = Some(observation.frame.frame_seq);
                }
                previous_observation = Some(observation.clone());
            } else if let Some(frame) = &exec.produced_frame {
                last_frame_seq = Some(frame.frame_seq);
                if matches!(step.as_step(), Some(StepRef::Launch(_))) {
                    bound_frame_seq = Some(frame.frame_seq);
                }
            }
        }

        if exec.status != Status::Pass {
            status = exec.status;
            let code = exec
                .error
                .as_ref()
                .map(|error| error.code)
                .unwrap_or(ErrorCode::Internal);
            failure = Some(FailureClassification {
                code,
                step_index: Some(index),
                summary: format!("step {index} ({}) {}", step.kind(), exec.status),
                error: exec.error.clone().unwrap_or_else(|| {
                    ProtocolError::new(ErrorCode::Internal, format!("step {} failed", step.kind()))
                        .with_hint(
                            "A failing step carried no structured error. Fix the executor; this \
                             is a runner bug, never app behaviour.",
                        )
                }),
            });
        }

        steps.push(step_result);
        if status != Status::Pass {
            notes.push(format!(
                "stopped at step {index} ({}): {}; later steps were not executed",
                step.kind(),
                status
            ));
            break;
        }
    }

    // ---- expectation handling --------------------------------------------
    apply_expectation(scenario.expectation, &mut status, &mut failure, &mut notes);

    // ---- fidelity handling (only relevant to a claim of pass) -------------
    let achieved = backend.fidelity();
    if status == Status::Pass {
        if let Some(required) = options.required_fidelity {
            if !achieved.satisfies(required) {
                status = Status::Unsupported;
                let error = ProtocolError::new(
                    ErrorCode::UnsupportedBackend,
                    format!(
                        "achieved fidelity `{}` does not satisfy required `{}`",
                        achieved.as_str(),
                        required.as_str()
                    ),
                )
                .with_hint("Run on a backend that reaches the required fidelity, or lower `required_fidelity` explicitly.")
                .with_details(serde_json::json!({
                    "required_fidelity": required.as_str(),
                    "achieved_fidelity": achieved.as_str(),
                    "backend": backend.id(),
                }));
                notes.push(format!(
                    "required fidelity `{}` not satisfied by `{}`",
                    required.as_str(),
                    achieved.as_str()
                ));
                failure = Some(FailureClassification {
                    code: ErrorCode::UnsupportedBackend,
                    step_index: None,
                    summary: format!("required fidelity `{}` not reached", required.as_str()),
                    error,
                });
            }
        }
    }

    // ---- coordinate contract note (W58) -----------------------------------
    // Say, in the verdict itself, which space every coordinate in this run is
    // in and where the PNG starts. Without this an agent that reads a tree bound
    // and crops the PNG is off by the status-bar inset (128 px) silently.
    if let Some(frame) = produced_frames.iter().find(|frame| frame.display_origin.is_some()) {
        let origin = frame.display_origin_px();
        let display = frame
            .display_size
            .map(|[w, h]| format!("{w}x{h}"))
            .unwrap_or_else(|| "unknown".to_string());
        notes.push(format!(
            "coordinates: UI-tree bounds, tap x/y, assert_region and assert_screenshot.region_masks are display \
             pixels (display {display}, origin [0,0]); frames/<seq>.png is the display area starting at \
             display_origin [{},{}], so png_y = display_y - {} (frame {} is display {}). Do not compare a tree \
             bound with a PNG pixel without that offset.",
            origin[0],
            origin[1],
            origin[1],
            frame.frame_seq,
            frame.display_rect()
        ));
    }

    // ---- assemble verdict -------------------------------------------------
    let finished_at = clock.now_rfc3339();
    let finished_ms = clock.now_unix_ms();
    let apk_sha256 = manifest.sha256.clone();
    let scenario_sha256 = sha256_file(&options.scenario_path)?;
    let isolation = scenario.isolation;
    let display = scenario.display;

    let reproducer = Reproducer::new(
        options.acos_version.clone(),
        options.apk.display().to_string(),
        apk_sha256.clone(),
        options.scenario_path.display().to_string(),
        scenario_sha256.clone(),
        isolation,
        display,
        achieved,
        backend.protocol_backend(),
    )
    .with_flags(options.flags.clone());

    let mut verdict = Verdict::new(
        status,
        achieved,
        backend.protocol_backend(),
        isolation,
        options.request_id.clone(),
        started_at,
        finished_at,
        AppIdentity {
            path: options.apk.display().to_string(),
            package: Some(manifest.package.clone()),
            sha256: apk_sha256,
        },
        ScenarioIdentity {
            path: options.scenario_path.display().to_string(),
            name: scenario.name.clone(),
            sha256: scenario_sha256,
        },
        reproducer,
    );
    verdict.required_fidelity = options.required_fidelity;
    verdict.session_id = session_id.clone();
    verdict.steps = steps.clone();
    verdict.failure = failure;
    verdict.notes = notes;

    // ---- events / logs / resources ---------------------------------------
    let events = build_events(&produced_frames, &all_logs, session_id.as_deref());
    let mut resources = ResourceReport {
        wall_ms: finished_ms.saturating_sub(started_ms),
        cpu_ms: None,
        peak_rss_bytes: peak_rss_bytes(),
        memory_mib: None,
        frames_captured: produced_frames.len() as u64,
        events: events.len() as u64,
        sessions: u32::from(session_id.is_some()),
        steps: steps.len() as u32,
    };
    if let Some(peak) = resources.peak_rss_bytes {
        resources.memory_mib = Some(peak / (1024 * 1024));
    }
    verdict.resources = resources.clone();

    artifacts::write_logcat(&options.artifacts_dir, &all_logs)?;
    artifacts::write_events(&options.artifacts_dir, &events)?;
    artifacts::write_resources(&options.artifacts_dir, &resources)?;
    artifacts::write_reproducer(&options.artifacts_dir, &verdict.reproducer)?;
    artifacts::write_verdict(&options.artifacts_dir, &verdict)?;

    let artifact_validation = acos_protocol::validate_artifacts_dir(&options.artifacts_dir);
    // Report only capabilities the backend actually has. A step that was
    // reported `unsupported` must not inflate `capabilities_used`.
    let available = caps.names();
    let capabilities_used: Vec<String> = capabilities_used
        .into_iter()
        .filter(|name| available.contains(&name.as_str()))
        .collect();

    Ok(RunOutcome {
        verdict,
        artifact_validation,
        capabilities_used,
    })
}

/// Apply the scenario's `expectation` to the observed status.
///
/// The hard rule is: a scenario that declares `fail`/`non_pass` must never be
/// reported as `pass`. If it is, the runner itself is broken and this returns a
/// harness failure instead.
fn apply_expectation(
    expectation: Expectation,
    status: &mut Status,
    failure: &mut Option<FailureClassification>,
    notes: &mut Vec<String>,
) {
    match expectation {
        Expectation::Pass => {
            if *status != Status::Pass {
                notes.push(format!(
                    "scenario expected `pass`, engine produced `{}` — result is a truthful partial",
                    status
                ));
            }
        }
        Expectation::NonPass => {
            if *status == Status::Pass {
                let error = ProtocolError::new(
                    ErrorCode::Internal,
                    "a non_pass scenario produced status=pass; the runner is broken".to_string(),
                )
                .with_hint("This is a harness failure, not a successful test. Inspect the scenario and the backend.");
                notes.push("non_pass expectation was violated by a pass".to_string());
                *status = Status::Error;
                *failure = Some(FailureClassification {
                    code: ErrorCode::Internal,
                    step_index: None,
                    summary: "non_pass scenario produced a pass".to_string(),
                    error,
                });
            }
        }
        Expectation::Fail => {
            if *status == Status::Pass {
                let error = ProtocolError::new(
                    ErrorCode::Internal,
                    "a fail-expected negative scenario produced status=pass; false pass prevented"
                        .to_string(),
                )
                .with_hint("This is a harness failure, never a successful test. The assertions or the backend are wrong.");
                notes.push("fail expectation was violated by a pass".to_string());
                *status = Status::Error;
                *failure = Some(FailureClassification {
                    code: ErrorCode::Internal,
                    step_index: None,
                    summary: "fail-expected scenario produced a pass".to_string(),
                    error,
                });
            } else if *status != Status::Fail {
                notes.push(format!(
                    "scenario expected `fail` but engine produced `{}` (assertions were not reached or not executable)",
                    status
                ));
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn execute_step(
    backend: &mut dyn Backend,
    caps: Capabilities,
    manifest: &crate::backend::ManifestInfo,
    step: &Step,
    apk: &std::path::Path,
    scenario: &Scenario,
    scenario_path: &Path,
    cause_id: &str,
    since_logs: &[LogRecord],
    all_logs: &[LogRecord],
    log_failure: Option<&BackendError>,
    crashed: Option<&CrashRecord>,
    previous: Option<&SessionObservation>,
    bound_frame_seq: Option<u64>,
    last_frame_seq: Option<u64>,
    capabilities_used: &mut BTreeSet<String>,
) -> StepExec {
    let Some(step_ref) = step.as_step() else {
        return StepExec::unsupported(Capability::StaticManifest, "step has no recognised kind");
    };
    match step_ref {
        StepRef::Launch(launch) => {
            capabilities_used.insert(Capability::Launch.as_str().to_string());
            if !caps.launch {
                return StepExec::unsupported(
                    Capability::Launch,
                    "the backend cannot boot an application process",
                );
            }
            if let Some(failure) = static_launch_assertion(manifest, launch) {
                return failure;
            }
            let activity = launch
                .activity
                .clone()
                .or_else(|| manifest.launcher_activity.clone());
            backend.set_settle_timeout(launch.settle_timeout_ms);
            let request = LaunchRequest {
                apk: apk.to_path_buf(),
                activity,
                intent_action: launch.intent_action.clone(),
                data: launch.data.clone(),
                extras: launch.extras.clone(),
                clear_state: launch.clear_state,
                isolation: scenario.isolation,
                display: scenario.display,
                cause_id: cause_id.to_string(),
            };
            match backend.launch(&request) {
                Ok(outcome) => {
                    let mut exec = StepExec::pass();
                    exec.produced_frame = outcome.frame.clone();
                    exec.session = Some(outcome.session_id.clone());
                    if let Some(record) = outcome.crashed {
                        let error = BackendError::Crash {
                            exception: record.exception.clone(),
                            message: record.message.clone(),
                            stack: record.stack.clone(),
                            fatal: record.fatal,
                        };
                        exec.status = error.status();
                        exec.error = Some(error.to_protocol_error());
                    }
                    exec.observation = None;
                    exec
                }
                Err(error) => StepExec::backend_error(&error, backend.id(), "launch"),
            }
        }
        StepRef::WaitFor(wait) => {
            capabilities_used.insert(Capability::UiTree.as_str().to_string());
            if !caps.ui_tree {
                return StepExec::unsupported(
                    Capability::UiTree,
                    "the backend cannot observe a UI tree",
                );
            }
            // W17 measured the old behaviour: one `observe()` and a return in
            // ~1 ms even though the scenario asked for a 15 s wait. Poll a
            // *fresh* traversal until the condition holds or the deadline
            // passes. The first attempt is immediate; later attempts are
            // paced, and the failure is a structured TIMEOUT.
            let timeout = Duration::from_millis(wait.timeout_ms);
            let started = Instant::now();
            // Each poll is a fresh traversal and, on ART, a stored frame. Back
            // off so a long timeout cannot exhaust the guest's buffer pool: a
            // 15 s wait makes ~19 observations, not 600.
            let mut interval = Duration::from_millis(WAIT_FOR_POLL_INTERVAL_MS);
            let mut polls = 0u64;
            // Assigned on every successful poll before it can be read; the
            // initialiser only exists so the timeout branch can move it.
            #[allow(unused_assignments)]
            let mut last: Option<SessionObservation> = None;
            loop {
                polls += 1;
                match backend.refresh() {
                    Ok(observation) => {
                        if let Some(failure) = observation_binding_error(backend.id(), &observation)
                        {
                            return failure;
                        }
                        if let Some(failure) = meaningful_error(&observation) {
                            return failure;
                        }
                        let matched = observation.tree.find_one(&wait.selector).cloned();
                        let satisfied = matched
                            .as_ref()
                            .is_some_and(|node| !wait.require_visible || node.visible);
                        last = Some(observation);
                        if satisfied {
                            let mut exec = StepExec::pass();
                            exec.observation = last;
                            return exec;
                        }
                    }
                    Err(error) => {
                        return StepExec::backend_error(&error, backend.id(), "wait_for");
                    }
                }
                let elapsed = started.elapsed();
                if elapsed >= timeout {
                    let mut exec =
                        StepExec::failed(assertions::element_missing("wait_for", &wait.selector, last.as_ref()));
                    exec.status = Status::Timeout;
                    exec.error = Some(
                        ProtocolError::new(
                            ErrorCode::Timeout,
                            format!(
                                "wait_for timed out after {} ms: selector {:?} did not satisfy the condition (require_visible={})",
                                wait.timeout_ms, wait.selector, wait.require_visible
                            ),
                        )
                        .with_hint(
                            "The live tree kept being re-traversed until the deadline. Increase                              `timeout_ms` if the app is genuinely slow, or fix the selector/state;                              a timeout is a structured failure, never a pass.",
                        )
                        .with_details(serde_json::json!({
                            "timeout_ms": wait.timeout_ms,
                            "elapsed_ms": elapsed.as_millis() as u64,
                            "polls": polls,
                            "require_visible": wait.require_visible,
                            "selector": wait.selector,
                            "last_frame_seq": last.as_ref().map(|observation| observation.frame.frame_seq),
                        })),
                    );
                    exec.observation = last;
                    return exec;
                }
                let remaining = timeout.saturating_sub(elapsed);
                if remaining.is_zero() {
                    continue;
                }
                std::thread::sleep(interval.min(remaining));
                interval = (interval * 2).min(Duration::from_millis(WAIT_FOR_MAX_POLL_INTERVAL_MS));
            }
        }
        StepRef::Tap(tap) => {
            capabilities_used.insert(Capability::Input.as_str().to_string());
            if !caps.input {
                return StepExec::unsupported(Capability::Input, "the backend cannot deliver tap");
            }
            backend.set_settle_timeout(tap.settle_timeout_ms);
            let action = InputAction::Tap {
                selector: tap.selector.clone(),
                x: tap.x,
                y: tap.y,
            };
            input_step(backend, &action, caps, previous, last_frame_seq)
        }
        StepRef::Type(ty) => {
            capabilities_used.insert(Capability::Input.as_str().to_string());
            if !caps.input {
                return StepExec::unsupported(Capability::Input, "the backend cannot deliver type");
            }
            backend.set_settle_timeout(ty.settle_timeout_ms);
            let action = InputAction::Type {
                selector: ty.selector.clone(),
                text: ty.text.clone(),
                clear: ty.clear,
            };
            input_step(backend, &action, caps, previous, last_frame_seq)
        }
        StepRef::Key(key) => {
            capabilities_used.insert(Capability::Input.as_str().to_string());
            if !caps.input {
                return StepExec::unsupported(Capability::Input, "the backend cannot deliver key");
            }
            backend.set_settle_timeout(key.settle_timeout_ms);
            input_step(
                backend,
                &InputAction::Key(key.key),
                caps,
                previous,
                last_frame_seq,
            )
        }
        StepRef::Back(_) => {
            capabilities_used.insert(Capability::Input.as_str().to_string());
            if !caps.input {
                return StepExec::unsupported(Capability::Input, "the backend cannot deliver back");
            }
            input_step(backend, &InputAction::Back, caps, previous, last_frame_seq)
        }
        StepRef::Rotate(rotate) => {
            capabilities_used.insert(Capability::Input.as_str().to_string());
            if !caps.input {
                return StepExec::unsupported(Capability::Input, "the backend cannot rotate");
            }
            input_step(
                backend,
                &InputAction::Rotate(rotate.orientation),
                caps,
                previous,
                last_frame_seq,
            )
        }
        StepRef::AssertText(assertion) => {
            capabilities_used.insert(Capability::UiTree.as_str().to_string());
            if !caps.ui_tree {
                return StepExec::unsupported(
                    Capability::UiTree,
                    "the backend cannot observe a UI tree",
                );
            }
            match backend.observe() {
                Ok(observation) => {
                    if let Some(failure) = observation_binding_error(backend.id(), &observation) {
                        return failure;
                    }
                    if let Some(bound) = bound_frame_seq {
                        if observation.frame.frame_seq != bound {
                            return frame_binding_failure(bound, &observation);
                        }
                    }
                    if let Some(failure) = meaningful_error(&observation) {
                        return failure;
                    }
                    let evaluated = eval_text(assertion, &observation);
                    let mut exec = if evaluated.result.passed {
                        StepExec::passed_assertion(evaluated)
                    } else {
                        StepExec::failed(evaluated)
                    };
                    exec.observation = Some(observation);
                    exec
                }
                Err(error) => StepExec::backend_error(&error, backend.id(), "assert_text"),
            }
        }
        StepRef::AssertVisible(assertion) => {
            capabilities_used.insert(Capability::UiTree.as_str().to_string());
            if !caps.ui_tree {
                return StepExec::unsupported(
                    Capability::UiTree,
                    "the backend cannot observe a UI tree",
                );
            }
            observation_assertion(
                backend,
                |observation| eval_visible(assertion, observation),
                "assert_visible",
                bound_frame_seq,
            )
        }
        StepRef::AssertNotVisible(assertion) => {
            capabilities_used.insert(Capability::UiTree.as_str().to_string());
            if !caps.ui_tree {
                return StepExec::unsupported(
                    Capability::UiTree,
                    "the backend cannot observe a UI tree",
                );
            }
            observation_assertion(
                backend,
                |observation| eval_not_visible(assertion, observation),
                "assert_not_visible",
                bound_frame_seq,
            )
        }
        StepRef::AssertState(assertion) => {
            capabilities_used.insert(Capability::UiTree.as_str().to_string());
            if !caps.ui_tree {
                return StepExec::unsupported(
                    Capability::UiTree,
                    "the backend cannot observe a UI tree",
                );
            }
            observation_assertion(
                backend,
                |observation| eval_state(assertion, observation),
                "assert_state",
                bound_frame_seq,
            )
        }
        StepRef::AssertLog(assertion) => {
            capabilities_used.insert(Capability::Logs.as_str().to_string());
            if !caps.logs {
                return StepExec::unsupported(Capability::Logs, "the backend cannot expose logs");
            }
            // A refused log stream (untrusted run: guest-asserted logs cannot
            // justify a pass) is the failure, not "no line matched".
            if let Some(error) = log_failure {
                return StepExec::failed_backend(error);
            }
            let scoped: &[LogRecord] = if assertion.since_previous_step {
                since_logs
            } else {
                all_logs
            };
            let evaluated = eval_log(assertion, scoped);
            if evaluated.result.passed {
                StepExec::passed_assertion(evaluated)
            } else {
                StepExec::failed(evaluated)
            }
        }
        StepRef::AssertNoCrash(_) => {
            capabilities_used.insert(Capability::CrashReports.as_str().to_string());
            if !caps.crash_reports {
                return StepExec::unsupported(
                    Capability::CrashReports,
                    "the backend cannot report crash/ANR state",
                );
            }
            let evaluated = eval_no_crash(crashed);
            if evaluated.result.passed {
                StepExec::passed_assertion(evaluated)
            } else {
                StepExec::failed(evaluated)
            }
        }
        StepRef::AssertRegion(assertion) => {
            capabilities_used.insert(Capability::Screenshot.as_str().to_string());
            if !caps.screenshot {
                return StepExec::unsupported(
                    Capability::Screenshot,
                    "the backend cannot capture a frame for a region assertion",
                );
            }
            // A region assertion always needs new pixels: the executor's
            // certainty rule. It is host-observed and therefore allowed on an
            // untrusted APK run.
            match backend.capture() {
                Ok(observation) => {
                    if let Some(failure) = observation_binding_error(backend.id(), &observation) {
                        return failure;
                    }
                    if let Some(failure) = meaningful_error(&observation) {
                        return failure;
                    }
                    let evaluated = eval_region(assertion, &observation, scenario_path);
                    let mut exec = if evaluated.result.passed {
                        StepExec::passed_assertion(evaluated)
                    } else {
                        StepExec::failed(evaluated)
                    };
                    exec.observation = Some(observation);
                    exec
                }
                Err(error) => StepExec::backend_error(&error, backend.id(), "assert_region"),
            }
        }
        StepRef::AssertScreenshot(assertion) => {
            capabilities_used.insert(Capability::Screenshot.as_str().to_string());
            if !caps.screenshot {
                return StepExec::unsupported(
                    Capability::Screenshot,
                    "the backend cannot capture a frame for screenshot comparison",
                );
            }
            match backend.capture() {
                Ok(observation) => {
                    if let Some(failure) = observation_binding_error(backend.id(), &observation) {
                        return failure;
                    }
                    if let Some(failure) = meaningful_error(&observation) {
                        return failure;
                    }
                    let evaluated = eval_screenshot(assertion, &observation, scenario_path);
                    let mut exec = if evaluated.result.passed {
                        StepExec::passed_assertion(evaluated)
                    } else {
                        StepExec::failed(evaluated)
                    };
                    exec.observation = Some(observation);
                    exec
                }
                Err(error) => StepExec::backend_error(&error, backend.id(), "assert_screenshot"),
            }
        }
        StepRef::Capture(capture) => {
            capabilities_used.insert(Capability::Screenshot.as_str().to_string());
            if !caps.screenshot {
                return StepExec::unsupported(
                    Capability::Screenshot,
                    "the backend cannot capture frames",
                );
            }
            match backend.capture() {
                Ok(observation) => {
                    if let Some(failure) = observation_binding_error(backend.id(), &observation) {
                        return failure;
                    }
                    if capture.fresh {
                        if let Some(previous_seq) = last_frame_seq {
                            if observation.frame.frame_seq <= previous_seq {
                                return no_fresh_frame_failure(
                                    previous_seq,
                                    &observation,
                                    "capture",
                                );
                            }
                        }
                    } else if let Some(bound) = bound_frame_seq {
                        if observation.frame.frame_seq != bound {
                            return frame_binding_failure(bound, &observation);
                        }
                    }
                    let mut exec = StepExec::pass();
                    exec.observation = Some(observation);
                    exec
                }
                Err(error) => StepExec::backend_error(&error, backend.id(), "capture"),
            }
        }
        StepRef::Reset(reset_step) => {
            capabilities_used.insert(Capability::Reset.as_str().to_string());
            if !caps.reset {
                return StepExec::unsupported(Capability::Reset, "the backend cannot reset state");
            }
            backend.set_settle_timeout(reset_step.settle_timeout_ms);
            match backend.reset(ResetScope::App) {
                Ok(()) => {
                    // A reset leaves the app in its initial state, i.e. a new
                    // completed frame. Bind that frame as this step's
                    // observation, so a following pure assertion describes the
                    // post-reset state instead of the pre-reset frame (and a
                    // frame-advancing ledger entry). If the backend cannot
                    // observe after the reset (an untrusted run refuses the
                    // guest-asserted tree), the reset itself still passed; only
                    // the ledger stays where it was, so a later tree assertion
                    // fails closed as usual.
                    let mut exec = StepExec::pass();
                    if let Ok(observation) = backend.observe() {
                        if let Some(failure) =
                            observation_binding_error(backend.id(), &observation)
                        {
                            return failure;
                        }
                        if meaningful_error(&observation).is_none() {
                            exec.observation = Some(observation);
                        }
                    }
                    exec
                }
                Err(error) => StepExec::backend_error(&error, backend.id(), "reset"),
            }
        }
        StepRef::SetNetwork(setup) => {
            environment_step(backend, caps, EnvironmentChange::Network(setup.clone()))
        }
        StepRef::SetPermission(setup) => {
            environment_step(backend, caps, EnvironmentChange::Permission(setup.clone()))
        }
        StepRef::SetLocale(setup) => {
            environment_step(backend, caps, EnvironmentChange::Locale(setup.clone()))
        }
        StepRef::SetDensity(setup) => {
            environment_step(backend, caps, EnvironmentChange::Density(*setup))
        }
        StepRef::SetTheme(setup) => {
            environment_step(backend, caps, EnvironmentChange::Theme(*setup))
        }
        StepRef::SetTime(setup) => {
            environment_step(backend, caps, EnvironmentChange::Time(setup.clone()))
        }
        StepRef::CallActivityMethod(_) => StepExec::unsupported(
            Capability::Environment,
            "call_activity_method requires an injected debug hook that this build does not provide",
        ),
    }
}

fn input_step(
    backend: &mut dyn Backend,
    action: &InputAction,
    caps: Capabilities,
    previous: Option<&SessionObservation>,
    last_frame_seq: Option<u64>,
) -> StepExec {
    match backend.input(action) {
        Ok(observation) => {
            if let Some(failure) = observation_binding_error(backend.id(), &observation) {
                return failure;
            }
            // The action must complete a new frame; assertions about its effect
            // bind to it instead of an older traversal.
            if let Some(previous_seq) = last_frame_seq {
                if observation.frame.frame_seq <= previous_seq {
                    return no_fresh_frame_failure(previous_seq, &observation, action.kind());
                }
            }
            let mut exec = if matches!(action, InputAction::Tap { .. }) {
                let evaluated = eval_tap_change(previous, &observation, caps);
                if evaluated.result.passed {
                    StepExec::passed_assertion(evaluated)
                } else {
                    StepExec::failed(evaluated)
                }
            } else {
                StepExec::pass()
            };
            exec.observation = Some(observation);
            exec
        }
        Err(error) => StepExec::backend_error(&error, backend.id(), action.kind()),
    }
}

fn environment_step(
    backend: &mut dyn Backend,
    caps: Capabilities,
    change: EnvironmentChange,
) -> StepExec {
    if !caps.environment {
        return StepExec::unsupported(
            Capability::Environment,
            format!("the backend cannot apply `{}`", change.kind()),
        );
    }
    match backend.configure(&change) {
        Ok(()) => StepExec::pass(),
        Err(error) => StepExec::backend_error(&error, backend.id(), change.kind()),
    }
}

fn observation_assertion(
    backend: &mut dyn Backend,
    evaluate: impl FnOnce(&SessionObservation) -> Evaluated,
    context: &str,
    bound_frame_seq: Option<u64>,
) -> StepExec {
    match backend.observe() {
        Ok(observation) => {
            if let Some(failure) = observation_binding_error(backend.id(), &observation) {
                return failure;
            }
            if let Some(bound) = bound_frame_seq {
                if observation.frame.frame_seq != bound {
                    return frame_binding_failure(bound, &observation);
                }
            }
            if let Some(failure) = meaningful_error(&observation) {
                return failure;
            }
            let evaluated = evaluate(&observation);
            let mut exec = if evaluated.result.passed {
                StepExec::passed_assertion(evaluated)
            } else {
                StepExec::failed(evaluated)
            };
            exec.observation = Some(observation);
            exec
        }
        Err(error) => StepExec::backend_error(&error, backend.id(), context),
    }
}

/// Steps that establish (or deliberately wait for) a new completed frame. A pure
/// assertion step must describe the frame one of these left behind.
fn frame_advancing_step(step: &Step) -> bool {
    matches!(
        step.as_step(),
        Some(
            StepRef::Launch(_)
                | StepRef::WaitFor(_)
                | StepRef::Tap(_)
                | StepRef::Type(_)
                | StepRef::Key(_)
                | StepRef::Back(_)
                | StepRef::Rotate(_)
                | StepRef::Reset(_)
                | StepRef::Capture(_)
                | StepRef::AssertScreenshot(_)
                | StepRef::AssertRegion(_)
        )
    )
}

/// The advisor's structural binding rule: a screenshot and the UI tree it is
/// asserted against must be one `SessionObservation`, so a tree or screenshot
/// from a different frame sequence can never back a pass.
fn observation_binding_error(backend: &str, observation: &SessionObservation) -> Option<StepExec> {
    let detail = observation.binding_violation()?;
    let error = ProtocolError::new(
        ErrorCode::Internal,
        format!("backend returned an unbound observation: {detail}"),
    )
    .with_hint(
        "A screenshot and its UI tree must describe the same completed frame/traversal. \
         Fix the backend; do not weaken the assertion.",
    )
    .with_details(serde_json::json!({
        "backend": backend,
        "frame_seq": observation.frame.frame_seq,
        "tree_frame_seq": observation.tree.frame.frame_seq,
    }));
    Some(StepExec {
        status: error.code.default_status(),
        error: Some(error),
        assertions: Vec::new(),
        observation: None,
        produced_frame: None,
        session: None,
    })
}

/// A blank/placeholder frame must never satisfy a UI or screenshot assertion.
fn meaningful_error(observation: &SessionObservation) -> Option<StepExec> {
    if observation.frame.meaningful {
        return None;
    }
    let error = ProtocolError::new(
        ErrorCode::NoFreshFrame,
        format!(
            "frame {} is not meaningful (blank/placeholder)",
            observation.frame.frame_seq
        ),
    )
    .with_hint(
        "A non-meaningful frame must never satisfy a UI or screenshot assertion; wait for or \
         capture a real rendered frame first.",
    )
    .with_details(serde_json::json!({
        "frame_seq": observation.frame.frame_seq,
        "cause_id": observation.frame.cause_id,
    }));
    Some(StepExec {
        status: error.code.default_status(),
        error: Some(error),
        assertions: Vec::new(),
        observation: None,
        produced_frame: None,
        session: None,
    })
}

/// A UI-tree observation that must be bound to `bound` (the frame a preceding
/// `launch`/`wait_for`/`input`/`capture` completed). A newer frame without one
/// of those steps means the screenshot and the tree describe different
/// traversals.
fn frame_binding_failure(bound: u64, observation: &SessionObservation) -> StepExec {
    let evaluated = Evaluated {
        result: AssertionResult {
            kind: "frame_binding".to_string(),
            passed: false,
            diff: Some(AssertionDiff {
                expected: serde_json::json!({"frame_seq": bound}),
                actual: serde_json::json!({"frame_seq": observation.frame.frame_seq}),
                path: Some("frame.frame_seq".to_string()),
                detail: Some(
                    "the observation advanced the frame without a wait_for/capture; the UI tree \
                     is not bound to the completed frame the screenshot came from"
                        .to_string(),
                ),
            }),
            matched_node_ids: Vec::new(),
            message: None,
        },
        error: Some(BackendError::Failed {
            code: ErrorCode::AssertionFailed,
            detail: format!(
                "observation is frame {} but the bound completed frame is {bound}",
                observation.frame.frame_seq
            ),
        }),
    };
    let mut exec = StepExec::failed(evaluated);
    exec.observation = Some(observation.clone());
    exec
}

/// The advisor's tap rule: a tap may only be claimed to have changed state when
/// the same completed frame shows the change in **both** the UI tree and the
/// screenshot (when the backend provides both).
fn eval_tap_change(
    previous: Option<&SessionObservation>,
    current: &SessionObservation,
    caps: Capabilities,
) -> Evaluated {
    // A delivered input that produced no visible change is not stale: the host
    // observed the injection, the guest accepted it, and the action's frame is
    // fresh. It is also not a state change, so nothing visual is claimed.
    if let Some(delivery) = &current.input {
        if delivery.delivered && !delivery.visual_change {
            let acks = delivery.acks.len();
            return Evaluated::pass(
                "input_delivery",
                Vec::new(),
                Some(format!(
                    "host-observed input delivery ({acks} guest ack(s)); no visual change; \
                     no state change claimed"
                )),
            );
        }
        if !delivery.delivered {
            return Evaluated::fail(
                "input_delivery",
                Vec::new(),
                AssertionDiff {
                    expected: serde_json::json!({"delivered": true}),
                    actual: serde_json::json!({"delivered": false}),
                    path: None,
                    detail: Some("the host did not observe input delivery".to_string()),
                },
                BackendError::Failed {
                    code: ErrorCode::NoFreshFrame,
                    detail: "the host did not observe input delivery".to_string(),
                },
            );
        }
    }
    let Some(previous) = previous else {
        return Evaluated::pass(
            "state_change",
            Vec::new(),
            Some("no previous observation to compare; change not claimed".to_string()),
        );
    };
    let change = current.changed_from(previous);
    let tree_required =
        caps.ui_tree && (!current.tree.nodes.is_empty() || !previous.tree.nodes.is_empty());
    let screenshot_required =
        caps.screenshot && current.screenshot_png.is_some() && previous.screenshot_png.is_some();
    let tree_ok = !tree_required || change.tree_changed;
    let screenshot_ok = !screenshot_required || change.screenshot_changed;
    if tree_ok && screenshot_ok {
        let message = match (tree_required, screenshot_required) {
            (true, true) => "UI tree and screenshot both changed".to_string(),
            (true, false) => "UI tree changed (no screenshot evidence available)".to_string(),
            (false, true) => "screenshot changed (no UI tree evidence available)".to_string(),
            (false, false) => "no evidence channel available to compare".to_string(),
        };
        return Evaluated::pass("state_change", Vec::new(), Some(message));
    }
    let detail = if !tree_ok {
        "the UI tree did not change".to_string()
    } else {
        "the screenshot did not change".to_string()
    };
    Evaluated::fail(
        "state_change",
        Vec::new(),
        AssertionDiff {
            expected: serde_json::json!({"tree_changed": true, "screenshot_changed": true}),
            actual: serde_json::json!({
                "tree_changed": change.tree_changed,
                "screenshot_changed": change.screenshot_changed,
            }),
            path: None,
            detail: Some(format!(
                "{detail}; a tap assertion needs the changed state in both"
            )),
        },
        BackendError::Failed {
            code: ErrorCode::AssertionFailed,
            detail: format!("tap did not change state in both ({detail})"),
        },
    )
}

/// An action (`tap`/`type`/`key`/`back`/`rotate`) or a fresh `capture` did not
/// advance the completed frame, so a following assertion would not be bound to
/// the action it claims to verify.
fn no_fresh_frame_failure(
    previous_seq: u64,
    observation: &SessionObservation,
    context: &str,
) -> StepExec {
    let error = ProtocolError::new(
        ErrorCode::NoFreshFrame,
        format!(
            "{context} returned frame {} but the previous completed frame was {previous_seq}; \
             an action must produce a new completed frame",
            observation.frame.frame_seq
        ),
    )
    .with_hint(
        "A post-action assertion binds to the frame the action produced; a stale frame would let \
         a screenshot and tree from different traversals pass.",
    )
    .with_details(serde_json::json!({
        "previous_frame_seq": previous_seq,
        "frame_seq": observation.frame.frame_seq,
        "context": context,
    }));
    StepExec {
        status: error.code.default_status(),
        error: Some(error),
        assertions: Vec::new(),
        observation: Some(observation.clone()),
        produced_frame: None,
        session: None,
    }
}

/// Compare the captured frame with the scenario's baseline PNG.
fn eval_screenshot(
    assertion: &ScreenshotAssertion,
    observation: &SessionObservation,
    scenario_path: &Path,
) -> Evaluated {
    let Some(actual) = observation.screenshot_png.as_deref() else {
        return Evaluated::fail(
            "screenshot",
            Vec::new(),
            AssertionDiff {
                expected: serde_json::json!({"baseline": assertion.baseline}),
                actual: serde_json::Value::Null,
                path: None,
                detail: Some("capture returned no screenshot bytes".to_string()),
            },
            BackendError::Failed {
                code: ErrorCode::NoFreshFrame,
                detail: "capture returned no screenshot bytes".to_string(),
            },
        );
    };
    let baseline_path =
        crate::verify::resolve_relative(&scenario_path.display().to_string(), &assertion.baseline);
    let baseline = match std::fs::read(&baseline_path) {
        Ok(bytes) => bytes,
        Err(e) => {
            let detail = format!(
                "cannot read screenshot baseline {}: {e}",
                baseline_path.display()
            );
            return Evaluated::fail(
                "screenshot",
                Vec::new(),
                AssertionDiff {
                    expected: serde_json::json!({"baseline": baseline_path.display().to_string()}),
                    actual: serde_json::Value::Null,
                    path: None,
                    detail: Some(detail.clone()),
                },
                BackendError::Error {
                    code: ErrorCode::ArtifactsInvalid,
                    detail,
                },
            );
        }
    };
    // ---- coordinate boundary: display -> frame (W58) ----------------------
    // Scenario masks are display pixels; the image comparison is frame pixels.
    let signed_frame = crate::display::signed_frame(&observation.frame);
    let masks = screenshot::masks_in_frame_space(&signed_frame, &assertion.region_masks);
    match screenshot::compare_png(&baseline, actual, assertion.tolerance, &masks) {
        Ok(diff) => {
            let detail = format!(
                "{:.4}% of {} compared pixels changed (tolerance {:.4}%)",
                diff.changed_fraction * 100.0,
                diff.compared_pixels,
                diff.tolerance * 100.0
            );
            let actual_value = serde_json::json!({
                "frame_seq": observation.frame.frame_seq,
                "changed_fraction": diff.changed_fraction,
                "changed_pixels": diff.changed_pixels,
                "compared_pixels": diff.compared_pixels,
                "width": diff.width,
                "height": diff.height,
                "masked_regions": diff.masked_regions,
            });
            if diff.passed {
                return Evaluated::pass("screenshot", Vec::new(), Some(detail));
            }
            Evaluated::fail(
                "screenshot",
                Vec::new(),
                AssertionDiff {
                    expected: serde_json::json!({
                        "baseline": assertion.baseline,
                        "tolerance": assertion.tolerance,
                    }),
                    actual: actual_value,
                    path: None,
                    detail: Some(detail),
                },
                BackendError::Failed {
                    code: ErrorCode::AssertionFailed,
                    detail: "screenshot differs from baseline beyond tolerance".to_string(),
                },
            )
        }
        Err(detail) => Evaluated::fail(
            "screenshot",
            Vec::new(),
            AssertionDiff {
                expected: serde_json::json!({"baseline": assertion.baseline}),
                actual: serde_json::Value::Null,
                path: None,
                detail: Some(detail.clone()),
            },
            BackendError::Error {
                code: ErrorCode::RenderDecode,
                detail,
            },
        ),
    }
}

/// Evaluate a host-side pixel/region assertion against one fresh frame.
///
/// The region is given in **display pixels** (the same space as UI-tree bounds
/// and tap points) and converted to PNG pixels with the frame's
/// `display_origin` — never with a hand-written offset. A region the frame does
/// not cover (the status bar band above the ART app-area capture) fails with the
/// geometry in the detail instead of cropping 128 px away from the request.
///
/// The crop is taken from the **host-observed** PNG. A match is exact where the
/// scenario asks for exactness; colour statistics are reported alongside so a
/// failure says what was actually drawn. OCR is not implemented: asking for it
/// returns `UNSUPPORTED_ASSERTION`, never a pass.
fn eval_region(
    assertion: &RegionAssertion,
    observation: &SessionObservation,
    scenario_path: &Path,
) -> Evaluated {
    if assertion.text_ocr.is_some() {
        return Evaluated::fail(
            "region",
            Vec::new(),
            AssertionDiff {
                expected: serde_json::json!({"text_ocr": assertion.text_ocr}),
                actual: serde_json::Value::Null,
                path: None,
                detail: Some("no OCR implementation is available in this build".to_string()),
            },
            BackendError::Failed {
                code: ErrorCode::UnsupportedAssertion,
                detail: "assert_region.text_ocr needs an OCR engine; this build has none"
                    .to_string(),
            },
        );
    }
    let Some(actual_png) = observation.screenshot_png.as_deref() else {
        return Evaluated::fail(
            "region",
            Vec::new(),
            AssertionDiff {
                expected: serde_json::json!({"region": [assertion.x, assertion.y, assertion.w, assertion.h]}),
                actual: serde_json::Value::Null,
                path: None,
                detail: Some("capture returned no screenshot bytes".to_string()),
            },
            BackendError::Failed {
                code: ErrorCode::NoFreshFrame,
                detail: "capture returned no screenshot bytes".to_string(),
            },
        );
    };
    let (frame_w, frame_h, rgba) = match screenshot::decode_rgba8(actual_png) {
        Ok(decoded) => decoded,
        Err(detail) => {
            return Evaluated::fail(
                "region",
                Vec::new(),
                AssertionDiff {
                    expected: serde_json::json!({"region": [assertion.x, assertion.y, assertion.w, assertion.h]}),
                    actual: serde_json::Value::Null,
                    path: None,
                    detail: Some(detail.clone()),
                },
                BackendError::Error {
                    code: ErrorCode::RenderDecode,
                    detail,
                },
            );
        }
    };
    let w = assertion.w as u64;
    let h = assertion.h as u64;
    // ---- coordinate boundary: display -> frame (W58) ----------------------
    let frame_meta = crate::display::signed_frame(&observation.frame);
    let display_region = Bounds::new(
        assertion.x,
        assertion.y,
        assertion.x.saturating_add(assertion.w as i32),
        assertion.y.saturating_add(assertion.h as i32),
    );
    let frame_region = match frame_meta.frame_rect_for_display(display_region) {
        Ok(region) => region,
        Err(detail) => {
            return Evaluated::fail(
                "region",
                Vec::new(),
                AssertionDiff {
                    expected: serde_json::json!({
                        "display_region": [assertion.x, assertion.y, assertion.w, assertion.h],
                        "display_origin": frame_meta.display_origin,
                        "display_size": frame_meta.display_size,
                    }),
                    actual: serde_json::json!({
                        "frame": [frame_meta.width, frame_meta.height],
                    }),
                    path: None,
                    detail: Some(detail.clone()),
                },
                BackendError::Failed {
                    code: ErrorCode::AssertionFailed,
                    detail,
                },
            );
        }
    };
    let x = frame_region.left().max(0) as u64;
    let y = frame_region.top().max(0) as u64;
    if x + w > u64::from(frame_w) || y + h > u64::from(frame_h) {
        let detail = format!(
            "display region [{},{},{},{}] maps to frame [{},{},{},{}], outside the {}x{} frame",
            assertion.x,
            assertion.y,
            assertion.w,
            assertion.h,
            frame_region.left(),
            frame_region.top(),
            frame_region.right(),
            frame_region.bottom(),
            frame_w,
            frame_h
        );
        return Evaluated::fail(
            "region",
            Vec::new(),
            AssertionDiff {
                expected: serde_json::json!({"display_region": [assertion.x, assertion.y, assertion.w, assertion.h]}),
                actual: serde_json::json!({"width": frame_w, "height": frame_h}),
                path: None,
                detail: Some(detail.clone()),
            },
            BackendError::Failed {
                code: ErrorCode::AssertionFailed,
                detail,
            },
        );
    }
    let crop_rgba = |source: &[u8], source_w: u32, _source_h: u32| -> Vec<u8> {
        let mut out = Vec::with_capacity((w * h * 4) as usize);
        for row in 0..h {
            let offset = (((y + row) * u64::from(source_w) + x) * 4) as usize;
            out.extend_from_slice(&source[offset..offset + (w * 4) as usize]);
        }
        out
    };
    let region = crop_rgba(&rgba, frame_w, frame_h);
    let mut distinct = HashSet::new();
    for pixel in region.chunks_exact(4) {
        distinct.insert([pixel[0], pixel[1], pixel[2], pixel[3]]);
    }
    let actual_hash = hex_sha256(&region);
    let mut failures: Vec<String> = Vec::new();
    let mut checks: Vec<String> = Vec::new();
    if let Some(expected_hash) = &assertion.hash {
        checks.push(format!("hash={expected_hash}"));
        if !actual_hash.eq_ignore_ascii_case(expected_hash) {
            failures.push(format!("hash {actual_hash} != {expected_hash}"));
        }
    }
    if let Some(baseline) = &assertion.equals_png {
        let baseline_path =
            crate::verify::resolve_relative(&scenario_path.display().to_string(), baseline);
        match std::fs::read(&baseline_path) {
            Ok(bytes) => match screenshot::decode_rgba8(&bytes) {
                Ok((bw, bh, brgba)) => {
                    let baseline_region = if (bw, bh) == (frame_w, frame_h) {
                        crop_rgba(&brgba, bw, bh)
                    } else if (bw, bh) == (assertion.w, assertion.h) {
                        brgba.clone()
                    } else {
                        failures.push(format!(
                            "reference {} is {bw}x{bh}; expected the frame ({frame_w}x{frame_h}) or the region ({}x{})",
                            baseline_path.display(), assertion.w, assertion.h
                        ));
                        Vec::new()
                    };
                    if !baseline_region.is_empty() {
                        checks.push(format!("equals_png={}", baseline_path.display()));
                        if baseline_region != region {
                            let differing = baseline_region
                                .iter()
                                .zip(region.iter())
                                .filter(|(a, b)| a != b)
                                .count();
                            failures.push(format!(
                                "region differs from {} ({differing} differing bytes)",
                                baseline_path.display()
                            ));
                        }
                    }
                }
                Err(detail) => failures.push(format!(
                    "cannot decode reference {}: {detail}",
                    baseline_path.display()
                )),
            },
            Err(error) => failures.push(format!(
                "cannot read reference {}: {error}",
                baseline_path.display()
            )),
        }
    }
    let distinct_count = distinct.len() as u32;
    if let Some(min) = assertion.min_distinct_colors {
        checks.push(format!("min_distinct_colors={min}"));
        if distinct_count < min {
            failures.push(format!(
                "only {distinct_count} distinct colours, need >= {min}"
            ));
        }
    }
    if let Some(max) = assertion.max_distinct_colors {
        checks.push(format!("max_distinct_colors={max}"));
        if distinct_count > max {
            failures.push(format!("{distinct_count} distinct colours, need <= {max}"));
        }
    }
    if checks.is_empty() {
        return Evaluated::fail(
            "region",
            Vec::new(),
            AssertionDiff {
                expected: serde_json::Value::Null,
                actual: serde_json::json!({"distinct_colors": distinct_count, "hash": actual_hash}),
                path: None,
                detail: Some("region assertion has no criterion".to_string()),
            },
            BackendError::Failed {
                code: ErrorCode::UnsupportedAssertion,
                detail: "region assertion has no criterion".to_string(),
            },
        );
    }
    let actual_value = serde_json::json!({
        "frame_seq": observation.frame.frame_seq,
        "display_region": [assertion.x, assertion.y, assertion.w, assertion.h],
        "display_origin": frame_meta.display_origin,
        "frame_region": [frame_region.left(), frame_region.top(), frame_region.left() + assertion.w as i32, frame_region.top() + assertion.h as i32],
        "hash": actual_hash,
        "distinct_colors": distinct_count,
    });
    if failures.is_empty() {
        return Evaluated::pass(
            "region",
            Vec::new(),
            Some(format!(
                "display region [{},{},{},{}] (frame [{},{}]) matched ({})",
                assertion.x,
                assertion.y,
                assertion.w,
                assertion.h,
                frame_region.left(),
                frame_region.top(),
                checks.join(", ")
            )),
        );
    }
    let detail = failures.join("; ");
    Evaluated::fail(
        "region",
        Vec::new(),
        AssertionDiff {
            expected: serde_json::json!({"criteria": checks}),
            actual: actual_value,
            path: None,
            detail: Some(detail.clone()),
        },
        BackendError::Failed {
            code: ErrorCode::AssertionFailed,
            detail,
        },
    )
}

fn hex_sha256(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// Static assertion that the scenario's launch target exists in the manifest.
fn static_launch_assertion(
    manifest: &crate::backend::ManifestInfo,
    launch: &acos_protocol::LaunchStep,
) -> Option<StepExec> {
    if let Some(activity) = &launch.activity {
        if !manifest.declares_activity(activity) {
            let resolved = crate::backend::resolve_component(&manifest.package, activity)
                .unwrap_or_else(|| activity.clone());
            let error = ProtocolError::new(
                ErrorCode::AssertionFailed,
                format!("activity `{resolved}` is not declared in {}", manifest.path),
            )
            .with_hint("Fix the scenario launch target or rebuild the APK; this is a static manifest assertion.")
            .with_details(serde_json::json!({
                "declared_activities": manifest.activities,
                "requested": activity,
                "resolved": resolved,
            }));
            return Some(StepExec {
                status: Status::Fail,
                error: Some(error),
                assertions: Vec::new(),
                observation: None,
                produced_frame: None,
                session: None,
            });
        }
    } else if launch.intent_action.is_none() && manifest.launcher_activity.is_none() {
        let error = ProtocolError::new(
            ErrorCode::AssertionFailed,
            "scenario launches without an activity and the APK has no launcher activity"
                .to_string(),
        );
        return Some(StepExec {
            status: Status::Fail,
            error: Some(error),
            assertions: Vec::new(),
            observation: None,
            produced_frame: None,
            session: None,
        });
    }
    None
}

/// Build the `events.jsonl` stream from produced frames and fetched logs.
fn build_events(
    frames: &[acos_protocol::FrameMeta],
    logs: &[LogRecord],
    session: Option<&str>,
) -> Vec<Event> {
    let mut events = Vec::new();
    let mut seq = 0u64;
    for frame in frames {
        seq += 1;
        events.push(
            Event::new(
                seq,
                logs.last().map(|l| l.timestamp_ms).unwrap_or(seq * 1000),
                EventBody::Frame {
                    frame_seq: frame.frame_seq,
                    cause_id: frame.cause_id.clone(),
                    meaningful: frame.meaningful,
                    content_hash: frame.content_hash.clone(),
                },
            )
            .with_session_id(session.unwrap_or("s_unknown")),
        );
    }
    for record in logs {
        seq += 1;
        events.push(
            Event::new(
                seq,
                record.timestamp_ms,
                EventBody::Log {
                    tag: record.tag.clone(),
                    level: record.level,
                    message: record.message.clone(),
                },
            )
            .with_session_id(session.unwrap_or("s_unknown")),
        );
    }
    events
}

#[cfg(test)]
mod region_tests {
    use super::*;
    use crate::backend::InputDelivery;
    use acos_protocol::assertion::RegionAssertion;
    use acos_protocol::ui::UiTree;
    use acos_protocol::FrameMeta;

    fn png(width: u32, height: u32, pixels: &[[u8; 4]]) -> Vec<u8> {
        let mut bytes = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut bytes, width, height);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().unwrap();
            let flat: Vec<u8> = pixels
                .iter()
                .flat_map(|pixel| pixel.iter().copied())
                .collect();
            writer.write_image_data(&flat).unwrap();
        }
        bytes
    }

    fn observation(pixels: &[[u8; 4]], width: u32, height: u32) -> SessionObservation {
        let frame = FrameMeta::new(
            7,
            "2026-11-11T21:00:00.000Z",
            "req:test",
            width,
            height,
            420,
            "hash",
            true,
        );
        let tree = UiTree::new(frame.clone(), "test.pkg", Vec::new());
        SessionObservation {
            session_id: "s_test".to_string(),
            package: "test.pkg".to_string(),
            frame,
            tree,
            screenshot_png: Some(png(width, height, pixels)),
            crashed: None,
            input: None,
        }
    }

    fn region(criteria: RegionAssertion) -> RegionAssertion {
        criteria
    }

    #[test]
    fn region_hash_matches_exact_rgba() {
        let observed = observation(&[[10, 20, 30, 255], [40, 50, 60, 255]], 2, 1);
        let expected = hex_sha256(&[10, 20, 30, 255, 40, 50, 60, 255]);
        let assertion = region(RegionAssertion {
            x: 0,
            y: 0,
            w: 2,
            h: 1,
            equals_png: None,
            hash: Some(expected),
            min_distinct_colors: None,
            max_distinct_colors: None,
            text_ocr: None,
        });
        let evaluated = eval_region(&assertion, &observed, Path::new("unused.yaml"));
        assert!(evaluated.result.passed, "{:?}", evaluated.error);
        assert_eq!(evaluated.result.kind, "region");
    }

    #[test]
    fn region_hash_mismatch_fails() {
        let observed = observation(&[[255, 0, 0, 255]], 1, 1);
        let assertion = region(RegionAssertion {
            x: 0,
            y: 0,
            w: 1,
            h: 1,
            equals_png: None,
            hash: Some(hex_sha256(&[0, 255, 0, 255])),
            min_distinct_colors: None,
            max_distinct_colors: None,
            text_ocr: None,
        });
        let evaluated = eval_region(&assertion, &observed, Path::new("unused.yaml"));
        assert!(!evaluated.result.passed);
        assert_eq!(evaluated.error.unwrap().code(), ErrorCode::AssertionFailed);
    }

    #[test]
    fn region_equals_png_is_exact_and_reads_a_region_of_the_frame() {
        let pixels = [
            [0, 0, 0, 255],
            [255, 255, 255, 255],
            [9, 9, 9, 255],
            [1, 2, 3, 255],
        ];
        let observed = observation(&pixels, 2, 2);
        let dir = std::env::temp_dir().join(format!("acos-region-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let baseline = dir.join("frame.png");
        std::fs::write(&baseline, png(2, 2, &pixels)).unwrap();
        let assertion = region(RegionAssertion {
            x: 1,
            y: 0,
            w: 1,
            h: 2,
            equals_png: Some(baseline.display().to_string()),
            hash: None,
            min_distinct_colors: None,
            max_distinct_colors: None,
            text_ocr: None,
        });
        let evaluated = eval_region(&assertion, &observed, Path::new("unused.yaml"));
        assert!(evaluated.result.passed, "{:?}", evaluated.error);
        let wrong_baseline = dir.join("other.png");
        std::fs::write(
            &wrong_baseline,
            png(1, 2, &[[7, 7, 7, 255], [8, 8, 8, 255]]),
        )
        .unwrap();
        let wrong = region(RegionAssertion {
            x: 1,
            y: 0,
            w: 1,
            h: 2,
            equals_png: Some(wrong_baseline.display().to_string()),
            hash: None,
            min_distinct_colors: None,
            max_distinct_colors: None,
            text_ocr: None,
        });
        let evaluated = eval_region(&wrong, &observed, Path::new("unused.yaml"));
        assert!(
            !evaluated.result.passed,
            "a different reference must not match"
        );
    }

    #[test]
    fn region_colour_statistics_and_ocr_unsupported() {
        let observed = observation(&[[255, 0, 0, 255], [255, 0, 0, 255]], 2, 1);
        let one_colour = region(RegionAssertion {
            x: 0,
            y: 0,
            w: 2,
            h: 1,
            equals_png: None,
            hash: None,
            min_distinct_colors: Some(2),
            max_distinct_colors: None,
            text_ocr: None,
        });
        let evaluated = eval_region(&one_colour, &observed, Path::new("unused.yaml"));
        assert!(!evaluated.result.passed, "solid red has one colour");
        let ocr = region(RegionAssertion {
            x: 0,
            y: 0,
            w: 2,
            h: 1,
            equals_png: None,
            hash: None,
            min_distinct_colors: None,
            max_distinct_colors: None,
            text_ocr: Some("HOSTILE PASS".to_string()),
        });
        let evaluated = eval_region(&ocr, &observed, Path::new("unused.yaml"));
        assert!(!evaluated.result.passed);
        assert_eq!(
            evaluated.error.unwrap().code(),
            ErrorCode::UnsupportedAssertion
        );
    }

    /// W58: a frame of the ART application area: the PNG is 8x16 but the
    /// display is 8x20 and the frame starts at display y=4 (a status bar).
    fn app_area_observation(pixels: &[[u8; 4]], width: u32, frame_height: u32) -> SessionObservation {
        let frame = FrameMeta::new(
            7,
            "2026-11-11T21:00:00.000Z",
            "req:test",
            width,
            frame_height,
            420,
            "hash",
            true,
        )
        .with_display_geometry([0, 4], [width, 20]);
        let tree = UiTree::new(frame.clone(), "test.pkg", Vec::new());
        SessionObservation {
            session_id: "s_test".to_string(),
            package: "test.pkg".to_string(),
            frame,
            tree,
            screenshot_png: Some(png(width, frame_height, pixels)),
            crashed: None,
            input: None,
        }
    }

    /// A frame whose display crop is a solid block at frame (2,3)..(4,5), i.e.
    /// display (2,7)..(4,9). Everything else is black.
    fn block_frame() -> SessionObservation {
        let mut pixels = vec![[0u8, 0, 0, 255]; 8 * 16];
        for row in 3..5 {
            for col in 2..4 {
                pixels[row * 8 + col] = [255, 0, 0, 255];
            }
        }
        app_area_observation(&pixels, 8, 16)
    }

    /// The contract: `assert_region` takes **display** pixels and the executor
    /// converts them with `display_origin`. The tree bound of a 2x2 node at
    /// display (2,7) must crop the same pixels as frame (2,3).
    #[test]
    fn assert_region_takes_display_coordinates_and_finds_the_tree_crop() {
        let tree_bound = acos_protocol::ui::Bounds::new(2, 7, 4, 9);
        let expected = hex_sha256(&[
            255, 0, 0, 255, // 2,3
            255, 0, 0, 255, // 3,3
            255, 0, 0, 255, // 2,4
            255, 0, 0, 255, // 3,4
        ]);
        let assertion = RegionAssertion {
            x: tree_bound.left(),
            y: tree_bound.top(),
            w: 2,
            h: 2,
            equals_png: None,
            hash: Some(expected),
            min_distinct_colors: None,
            max_distinct_colors: None,
            text_ocr: None,
        };
        let evaluated = eval_region(&assertion, &block_frame(), Path::new("unused.yaml"));
        assert!(evaluated.result.passed, "{:?}", evaluated.error);
        let message = evaluated.result.message.clone().unwrap_or_default();
        assert!(message.contains("display region [2,7,2,2]"), "{message}");
        assert!(message.contains("frame [2,3]"), "{message}");

        // Negative test: the OLD interpretation (y is already a PNG pixel) reads
        // display rows 3..5, i.e. other pixels, so the same hash must fail. This
        // is the bug the contract closes — and it fails loudly, never silently.
        let old_interpretation = RegionAssertion {
            x: 2,
            y: 3,
            w: 2,
            h: 2,
            equals_png: None,
            hash: Some(hex_sha256(&[
                255, 0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255,
            ])),
            min_distinct_colors: None,
            max_distinct_colors: None,
            text_ocr: None,
        };
        let evaluated = eval_region(&old_interpretation, &block_frame(), Path::new("unused.yaml"));
        assert!(
            !evaluated.result.passed,
            "frame-space y must not be accepted as display-space y"
        );
        assert_eq!(evaluated.error.unwrap().code(), ErrorCode::AssertionFailed);
    }

    /// The display area the frame does not contain has no pixels: asking for it
    /// fails with the geometry spelled out, instead of cropping 4 px away.
    #[test]
    fn a_status_bar_region_fails_and_names_the_geometry() {
        let assertion = RegionAssertion {
            x: 0,
            y: 0,
            w: 4,
            h: 4,
            equals_png: None,
            hash: Some(hex_sha256(&[0, 0, 0, 255])),
            min_distinct_colors: None,
            max_distinct_colors: None,
            text_ocr: None,
        };
        let evaluated = eval_region(&assertion, &block_frame(), Path::new("unused.yaml"));
        assert!(!evaluated.result.passed);
        let detail = evaluated
            .result
            .diff
            .and_then(|d| d.detail)
            .unwrap_or_default();
        assert!(detail.contains("display region [0,0][4,4]"), "{detail}");
        assert!(detail.contains("display origin [0,4]"), "{detail}");
        assert_eq!(evaluated.error.unwrap().code(), ErrorCode::AssertionFailed);
    }

    #[test]
    fn delivered_but_visually_unchanged_input_is_not_a_state_change() {
        let previous = observation(&[[0, 0, 0, 255]], 1, 1);
        let mut current = observation(&[[0, 0, 0, 255]], 1, 1);
        current.input = Some(InputDelivery {
            delivered: true,
            visual_change: false,
            acks: Vec::new(),
            settle: None,
        });
        let evaluated = eval_tap_change(Some(&previous), &current, Capabilities::default());
        assert!(evaluated.result.passed);
        assert_eq!(evaluated.result.kind, "input_delivery");
        let message = evaluated.result.message.unwrap_or_default();
        assert!(message.contains("host-observed input delivery"));
        assert!(message.contains("no visual change"));
    }
}

/// Peak resident set size in bytes (`/proc/self/status` `VmHWM`).
fn peak_rss_bytes() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("VmHWM:") {
            let kb: u64 = rest.split_whitespace().next()?.parse().ok()?;
            return Some(kb.saturating_mul(1024));
        }
    }
    None
}
