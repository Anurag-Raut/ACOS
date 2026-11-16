//! The scenario executor: run steps against a [`Backend`] and build a verdict.
//!
//! The executor is where "never pass what was not executed" is enforced:
//!
//! * a step whose capability is absent is `unsupported`, never silently skipped;
//! * execution stops at the first non-pass step, so later assertions are not
//!   evaluated against stale state;
//! * a negative scenario that somehow passes is converted into an `INTERNAL`
//!   harness failure instead of being reported as a successful test.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use acos_protocol::assertion::{
    AssertionDiff, AssertionResult, RegionAssertion, ScreenshotAssertion,
};
use acos_protocol::commands::{Point, ResetScope};
use acos_protocol::events::{Event, EventBody};
use acos_protocol::scenario::{
    Expectation, ExpectedError, Scenario, Step, StepRef, StepSettle, SwipeDirection,
};
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
use crate::settle::ObservationSettle;
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
    /// Deterministic `NAME=VALUE` flags for the reproducer.
    pub flags: Vec<String>,
    /// `acos` version string.
    pub acos_version: String,
    /// The execution engine this run was started with (`art` / `rust-vm`),
    /// from the request. Recorded in the verdict's provenance block so the
    /// verdict names the engine even when it is produced without the CLI.
    pub engine: String,
    /// Structured, non-fatal facts for `provenance.warnings[]` (for example
    /// `SCENARIO_APP_OVERRIDDEN` when `--apk` and the scenario's `app:` name
    /// different files). Warnings never change the verdict.
    pub warnings: Vec<serde_json::Value>,
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
    /// What artifact retention did (ARTGC); `None` for runs that never reached
    /// the artifacts stage.
    pub retention: Option<crate::retention::RetentionReport>,
}

/// Internal per-step execution state.
struct StepExec {
    status: Status,
    error: Option<ProtocolError>,
    assertions: Vec<AssertionResult>,
    observation: Option<SessionObservation>,
    produced_frame: Option<acos_protocol::FrameMeta>,
    session: Option<String>,
    /// SE2: the settle record the step bound its frame with, when the backend
    /// reported one (a `launch` carries it in its detail).
    settle: Option<serde_json::Value>,
    /// REL3E: the action's own delivery record (`handled`, acks, target), kept
    /// even when the step failed. A tap the app never consumed usually produces
    /// no new frame and times out its settle; the agent still needs to be told
    /// *that nothing consumed it*, so the delivery travels with the failure.
    delivery: Option<crate::backend::InputDelivery>,
    /// REL3S: the `scroll_until` search report (`selector`, `found`, `attempts`,
    /// the settled `frame_seq`). `None` for every other step; `found: false`
    /// with the miss's attempts when the search exhausted its budget.
    until: Option<serde_json::Value>,
    /// REL3H2: structured step-level warnings (copied into the step JSON and the
    /// verdict's `provenance.warnings[]`). Empty for a step with nothing to say.
    warnings: Vec<serde_json::Value>,
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
            settle: None,
            delivery: None,
            until: None,
            warnings: Vec::new(),
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
            settle: None,
            delivery: None,
            until: None,
            warnings: Vec::new(),
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
            settle: None,
            delivery: None,
            until: None,
            warnings: Vec::new(),
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
            settle: None,
            delivery: None,
            until: None,
            warnings: Vec::new(),
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
            settle: None,
            delivery: None,
            until: None,
            warnings: Vec::new(),
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
            settle: None,
            delivery: None,
            until: None,
            warnings: Vec::new(),
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
    // PROVENANCE: both inputs are identified **before** anything runs, so the
    // digest in the verdict is the digest of the bytes the run read. Hashing
    // them afterwards would describe whatever is on disk at the end, which is
    // how a verdict can name a file the run never saw. The capture also keeps
    // size/mtime for the post-run `INPUT_CHANGED_DURING_RUN` check below.
    let inputs = acos_protocol::provenance::Inputs::capture(&options.apk, Some(&options.scenario_path));
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
    // REL3H2: step-level warnings promoted to the verdict's
    // `provenance.warnings[]` (an unhandled input must not be a silent pass).
    let mut step_warnings: Vec<serde_json::Value> = Vec::new();
    // `capture` step names -> the PNG that step wrote. Written into the verdict
    // so the scenario's own label answers "the screenshot for X".
    let mut captures: BTreeMap<String, String> = BTreeMap::new();

    // The advisor frame-binding ledger: the completed frame every pure UI or
    // screenshot assertion must describe, plus the highest frame seen (for
    // `capture fresh`).
    let mut previous_observation: Option<SessionObservation> = None;
    let mut bound_frame_seq: Option<u64> = None;
    let mut last_frame_seq: Option<u64> = None;
    // GR3/G3a: whether the bound frame came from a `settle: fast` input. Fast is
    // navigation only, so an assertion may not describe that frame; a strict
    // `wait_for`/`capture`/input must bind a new one first.
    let mut bound_is_fast = false;
    // SE2: how the bound frame was settled, when the backend reported it. A
    // `content` bound frame may back UI/text assertions but never a pixel one.
    let mut bound_settle: Option<serde_json::Value> = None;

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
        let exec = if bound_is_fast && binds_to_bound_frame(step) {
            fast_bound_failure(step.kind())
        } else if let Some(failure) = content_settle_pixel_failure(step, bound_settle.as_ref()) {
            // SE2: content stability says nothing about pixels (they may still
            // be animating). A pixel/frame assertion bound to a content-settled
            // frame is a failed binding, never a comparison.
            failure
        } else if let Some(failure) = content_settle_geometry_failure(step, bound_settle.as_ref()) {
            // SE2b: nor does it say anything about geometry - a view translated
            // by an animation changes its reported rect every frame (measured on
            // the reference screen). A bounds claim on a content-settled frame is
            // a failed binding too.
            failure
        } else {
            execute_step(
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
        )
        };
        // ---- SE2: a requested content settle must be honoured ----------------
        // If a step asked for `settle: content` and the backend returned a
        // frame with no content record (or another rule's record), the step did
        // not get the rule it asked for. Fail closed instead of letting a
        // strict/fast observation stand in for a content-stable one.
        let exec = match content_settle_unhonored(step, &exec) {
            Some(failure) => failure,
            None => exec,
        };
        // ---- SE1: the opt-in `expect_change` rule ---------------------------
        // P12 keeps "a step whose action changed no pixels still settles and
        // passes"; `expect_change: true` is the per-step opt-in that turns the
        // measured fact into a failure. It runs after the strict settle, on the
        // same settle record the reply reports (`pixels_changed`).
        let exec = match expect_change_failure(step, &exec, previous_observation.as_ref()) {
            Some(failure) => failure,
            None => exec,
        };
        // ---- REL3H2: `expect_handled` is a claim, and an unhandled step says so
        // ---- A gesture nothing consumed must not be a silent plain pass: the
        // claim side (`expect_handled: true`) fails closed, the default side
        // carries `INPUT_UNHANDLED` in the step JSON and the verdict's
        // `provenance.warnings[]`, and `expect_handled: false` is the scenario's
        // "this no-op is intended".
        let mut exec = match expect_handled_failure(step, &exec) {
            Some(failure) => failure,
            None => exec,
        };
        if let Some(warning) = input_unhandled_warning(step, index, &exec) {
            exec.warnings.push(warning.clone());
            step_warnings.push(warning);
        }
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
        // REL3H2: the step's own structured warnings (also in the verdict's
        // provenance block, so either file tells the same story).
        step_result.warnings = exec.warnings.clone();
        // P12: a tap/type/key reply's settle block (including pixels_changed)
        // is step evidence. It is never the pass/fail rule by itself.
        // SE2b: prefer the per-action record. `observation.settle` is the
        // session's sticky bound record and can describe an earlier traversal or
        // an earlier action's rule; the input's own record is the one this step
        // actually applied.
        step_result.settle =
            step_settle(exec.observation.as_ref(), exec.settle.as_ref(), duration_ms);
        // REL3E: an input step says what the app did with it (`handled`) and what
        // the injected point resolved to, plus whether a change was required -
        // `"auto"` for a tap, whose own state-change rule runs on every step even
        // when the scenario never asked for it. REL3S2: a swipe/scroll step adds
        // how far the gesture got (`until`).
        step_result.input = step_input(step, exec.delivery.as_ref(), exec.until.as_ref());

        // Write evidence and declare it.
        let step_artifact = acos_protocol::step_file_name(index, step.kind());
        let capture_name = match step.as_step() {
            Some(StepRef::Capture(capture)) => Some(capture.name.clone()),
            _ => None,
        };
        if let Some(observation) = &exec.observation {
            let seq = observation.frame.frame_seq;
            step_result.frame_seq = Some(seq);
            // REL3C: a failed step whose bound tree is a one-node skeleton (the
            // feedback case: `tree/<seq>.json` = [DecorView], truncated) cannot
            // be debugged. Refresh once for *evidence only*; the assertion still
            // judges the frame it actually bound, so settle semantics do not
            // change. The richer artifact also survives `write_tree`'s
            // no-smaller-overwrite rule.
            let mut evidence_tree = observation.tree.clone();
            if step_result.status != Status::Pass && tree_node_count(&evidence_tree) <= 1 {
                if let Ok(refreshed) = backend.refresh() {
                    if tree_node_count(&refreshed.tree) > tree_node_count(&evidence_tree) {
                        evidence_tree = refreshed.tree;
                    }
                }
            }
            artifacts::write_tree(&options.artifacts_dir, seq, &evidence_tree)?;
            step_result
                .evidence
                .push(ArtifactRef::new("tree", acos_protocol::tree_file_name(seq)));
            if let Some(png) = &observation.screenshot_png {
                artifacts::write_frame(&options.artifacts_dir, seq, png)?;
                step_result.evidence.push(ArtifactRef::new(
                    "frame",
                    acos_protocol::frame_file_name(seq),
                ));
                // A `capture` step's own name is evidence: the step JSON gets
                // `name`, the PNG gets a named copy, and the verdict gets the
                // name -> path map.
                if let Some(name) = &capture_name {
                    let named = acos_protocol::artifacts::capture_frame_file_name(seq, name);
                    artifacts::write_capture_frame(
                        &options.artifacts_dir,
                        &acos_protocol::frame_file_name(seq),
                        &named,
                    )?;
                    step_result.name = Some(name.clone());
                    step_result
                        .evidence
                        .push(ArtifactRef::new("capture", named.clone()));
                    captures.insert(name.clone(), named);
                }
            } else if let Some(name) = &capture_name {
                // No pixels: the capture cannot pass, and a name without a file
                // would be a dangling declaration. Record the gap honestly.
                notes.push(format!(
                    "capture step {index} ({name}) produced no frame bytes; no captures entry was written"
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
                if frame_advancing_step(step)
                    || rebinds_bound_frame(step, bound_frame_seq, observation.frame.frame_seq)
                {
                    bound_frame_seq = Some(observation.frame.frame_seq);
                    bound_is_fast = fast_input_step(step);
                    bound_settle = observation.settle.clone().or_else(|| {
                        observation.input.as_ref().and_then(|input| input.settle.clone())
                    });
                }
                previous_observation = Some(observation.clone());
            } else if let Some(frame) = &exec.produced_frame {
                last_frame_seq = Some(frame.frame_seq);
                if matches!(step.as_step(), Some(StepRef::Launch(_))) {
                    bound_frame_seq = Some(frame.frame_seq);
                    bound_is_fast = false;
                    bound_settle = exec.settle.clone();
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
    apply_expectation(
        scenario.expectation,
        scenario.expect_error.as_ref(),
        &mut status,
        &mut failure,
        &mut notes,
    );

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
    // The pre-run capture, not a second read: the reproducer must replay the
    // scenario the run used.
    let scenario_sha256 = match inputs.scenario.as_ref().and_then(|s| s.sha256.clone()) {
        Some(sha) => sha,
        None => sha256_file(&options.scenario_path)?,
    };
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
    verdict.captures = captures;
    verdict.failure = failure;
    verdict.notes = notes;

    // PROVENANCE: which inputs produced this verdict. The block is built here,
    // not in the CLI, so every front door (CLI, `acos serve`, MCP) and every
    // direct `acos_verify::verify` caller writes the same fields into
    // `verdict.json`: absolute scenario/APK paths with their digests, the
    // engine, the achieved fidelity and structured warnings. A relative path
    // or a missing digest is exactly how an older scenario ran against a stale
    // APK and returned `pass` without saying so (feedback 2026-11-14).
    let mut provenance = inputs.block(&options.engine, achieved);
    if let Some(object) = provenance.as_object_mut() {
        object.insert(
            "scenario_engine".to_string(),
            serde_json::json!(scenario.engine.as_str()),
        );
    }
    for warning in &options.warnings {
        acos_protocol::provenance::push_warning(&mut provenance, warning.clone());
    }
    // REL3H2: the same `INPUT_UNHANDLED` records the step JSONs carry, so a
    // reader of `verdict.json` alone sees that an input was not handled. They
    // are facts about the run, never a status: the step's own result decides.
    for warning in &step_warnings {
        acos_protocol::provenance::push_warning(&mut provenance, warning.clone());
    }
    // LOW review finding: an input rewritten while the run was in flight is
    // never silent. The status stays exactly as measured (the warning is a
    // fact, not a failure), but the envelope, `verdict.json` and the summary
    // line all say the file on disk is no longer the bytes that were judged.
    for warning in inputs.changed_warnings() {
        acos_protocol::provenance::push_warning(&mut provenance, warning);
    }
    verdict.provenance = Some(provenance);

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

    // ---- ARTGC: bounded artifacts ---------------------------------------
    // The verdict and every step are on disk, so retention can see exactly what
    // the run declares. Only a pass is ever pruned (the decision is a pure
    // function of the status, `crate::retention::decision`), and nothing a
    // retained report names is deleted. The note is part of the verdict so an
    // agent reading only `verdict.json` knows what an absent frame means.
    let (keep_mode, keep_warning) = crate::retention::KeepArtifacts::from_env();
    // `none-on-pass` removes frames, so the verdict must stop declaring them
    // before it is written. A report never names a file retention removes.
    if crate::retention::decision(status, keep_mode) == crate::retention::Decision::PruneAllFrames {
        let stripped = crate::retention::strip_frame_evidence(&mut verdict);
        if stripped > 0 {
            verdict.notes.push(format!(
                "artifacts: `{keep_mode}` dropped {stripped} frame declaration(s) and every frame;                  trees, logs and assertion results are the evidence for this pass"
            ));
        }
    }
    artifacts::write_verdict(&options.artifacts_dir, &verdict)?;
    let retention = crate::retention::apply_with_warning(
        &options.artifacts_dir,
        keep_mode,
        status,
        &verdict,
        keep_warning.clone(),
    )?;
    if let Some(warning) = keep_warning {
        verdict.notes.push(format!("artifacts: {warning}; using `{keep_mode}`"));
    }
    verdict.notes.push(format!(
        "artifacts: {} ({}): {} file(s) / {:.1} MiB kept, {} file(s) / {:.1} MiB pruned; see {}",
        retention.mode,
        retention.decision,
        retention.kept_files,
        retention.kept_bytes as f64 / (1024.0 * 1024.0),
        retention.pruned_files,
        retention.pruned_bytes as f64 / (1024.0 * 1024.0),
        crate::retention::RETENTION_JSON
    ));
    artifacts::write_verdict(&options.artifacts_dir, &verdict)?;

    // Validate *after* retention: a pruned directory that no longer backs its
    // own verdict would be an invalid pass, and this is where that shows up.
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
        retention: Some(retention),
    })
}

/// Apply the scenario's `expectation` to the observed status.
///
/// The hard rule is: a scenario that declares `fail`/`non_pass` must never be
/// reported as `pass`. If it is, the runner itself is broken and this returns a
/// harness failure instead.
fn apply_expectation(
    expectation: Expectation,
    expect_error: Option<&ExpectedError>,
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
    // SE2b: a negative that declares the failure it expects must produce
    // exactly that failure. "It went red somehow" is not proof: a control that
    // fails for another reason (an unapplied settle rule, a stale frame, a
    // harness timeout) would look green while proving nothing.
    let Some(expected) = expect_error else {
        return;
    };
    if *status == Status::Pass {
        // The `fail`/`non_pass` branches above already converted a pass into a
        // harness error; nothing to pin.
        return;
    }
    let actual_code = failure.as_ref().map(|failure| failure.code.as_str());
    let actual_reason = failure.as_ref().and_then(|failure| {
        failure
            .error
            .details
            .as_ref()
            .and_then(|details| details.get("reason"))
            .and_then(serde_json::Value::as_str)
    });
    let code_matches = actual_code == Some(expected.code.as_str());
    let reason_matches = expected
        .reason
        .as_deref()
        .map(|reason| actual_reason == Some(reason))
        .unwrap_or(true);
    if code_matches && reason_matches {
        notes.push(format!(
            "negative control failed exactly as declared ({} / {:?})",
            expected.code, expected.reason
        ));
        return;
    }
    let detail = format!(
        "the scenario declares `expect_error: {{code: {}, reason: {:?}}}` but the run failed with \
         code {:?} / reason {:?}; a negative that fails for another reason proves nothing",
        expected.code, expected.reason, actual_code, actual_reason
    );
    let error = ProtocolError::new(ErrorCode::Internal, detail.clone())
        .with_hint("Fix the scenario or the rule it is meant to exercise. Do not widen expect_error to accept this failure.")
        .with_details(serde_json::json!({
            "reason": "negative_expectation_mismatch",
            "expected": {"code": expected.code, "reason": expected.reason},
            "actual": {"code": actual_code, "reason": actual_reason},
        }));
    notes.push("negative_expectation_mismatch".to_string());
    *status = Status::Error;
    *failure = Some(FailureClassification {
        code: ErrorCode::Internal,
        step_index: failure.as_ref().and_then(|failure| failure.step_index),
        summary: detail,
        error,
    });
}

/// REL3A: one observation's settle rule, scoped and never sticky.
///
/// `Backend::set_observation_settle` is session state: if the executor left it
/// at `content`, every later observation (`capture`, a pixel assertion, the next
/// action's frame) would inherit a rule it never asked for. A content-settled
/// frame is not pixel evidence, so that leak is a false pass. The guard sets the
/// rule for one call and restores `strict` on **every** exit path, including an
/// early `return` or a panic in the observation.
struct ObservationScope<'a> {
    backend: &'a mut dyn Backend,
}

impl<'a> ObservationScope<'a> {
    fn new(backend: &'a mut dyn Backend, mode: ObservationSettle) -> Self {
        backend.set_observation_settle(mode);
        Self { backend }
    }

    fn backend(&mut self) -> &mut dyn Backend {
        self.backend
    }
}

impl Drop for ObservationScope<'_> {
    fn drop(&mut self) {
        self.backend.set_observation_settle(ObservationSettle::Strict);
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
    // REL3A: the observation settle rule is scoped to the observation loops
    // below. A step that does not observe (launch, capture, reset, a pixel
    // assertion) starts from the strict rule, so a previous `wait_for` content
    // fallback can never leak into its pixels.
    backend.set_observation_settle(ObservationSettle::Strict);
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
            // Hand the backend the expanded component (`pkg/.Cls` -> `pkg.Cls`),
            // not the shorthand: engines that resolve the class themselves must
            // not each re-implement Android's name expansion.
            let activity = match &launch.activity {
                Some(activity) => crate::backend::resolve_component(&manifest.package, activity)
                    .or_else(|| Some(activity.clone())),
                None => manifest.launcher_activity.clone(),
            };
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
                settle: launch.settle.unwrap_or_default(),
            };
            match backend.launch(&request) {
                Ok(outcome) => {
                    let mut exec = StepExec::pass();
                    exec.produced_frame = outcome.frame.clone();
                    exec.session = Some(outcome.session_id.clone());
                    // SE2: the launch's settle record travels with the frame
                    // binding so a pixel assertion on a content-launched frame
                    // fails closed.
                    exec.settle = outcome.detail.get("settle").cloned().filter(|settle| {
                        settle.get("mode").and_then(serde_json::Value::as_str) == Some("content")
                    });
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
            // REL3A: the settle rule this wait asks each poll for. `None` is the
            // documented default for a wait (auto): strict first, then the
            // content-stable rule once strict has proven it cannot settle.
            let requested = ObservationSettle::from_step(wait.settle);
            let mut fell_back_from: Option<&'static str> = None;
            let mut settle_wait_ms = 0u64;
            // Assigned on every successful poll before it can be read; the
            // initialiser only exists so the timeout branch can move it.
            #[allow(unused_assignments)]
            let mut last: Option<SessionObservation> = None;
            // A poll whose strict settle timed out (the screen was still
            // changing, e.g. an RN splash) is "not yet", not the step's
            // verdict: keep polling until the wait_for deadline. Nothing an
            // unsettled poll saw can satisfy the condition, so this is never
            // more permissive; if the deadline expires while the screen is
            // still unsettled, that settle TIMEOUT (with its evidence) is the
            // step's result.
            let mut unsettled: Option<BackendError> = None;
            // REL3A §2: a content verdict is a *stability* claim, so one sample
            // is never enough. The condition must hold on two consecutive
            // content-settled observations at least one content window apart
            // (explicit `settle: content` and the automatic fallback alike)
            // before the step may pass. A poll that is not satisfied (or never
            // settled) clears the pending confirmation.
            let content_window = Duration::from_millis(crate::settle::content_window_ms(None));
            let mut content_confirmation: Option<Instant> = None;
            loop {
                polls += 1;
                let mode = match requested {
                    Some(mode) => mode,
                    None if fell_back_from.is_some() => ObservationSettle::Content,
                    None => ObservationSettle::Strict,
                };
                // P12b: the step's own settle budget, applied (and reset) for
                // this observation. A previous step's budget must never leak.
                backend.set_settle_timeout(wait.settle_timeout_ms);
                // REL3A: the rule is scoped to this one observation; the guard
                // restores strict on every exit path.
                let mut scope = ObservationScope::new(backend, mode);
                let backend = scope.backend();
                let attempt_started = Instant::now();
                match backend.refresh() {
                    Err(error) if is_settle_timeout(&error) => {
                        settle_wait_ms += attempt_started.elapsed().as_millis() as u64;
                        content_confirmation = None;
                        if requested.is_none() && mode == ObservationSettle::Strict {
                            // REL3A: strict settle cannot settle this screen (an
                            // endless animation). The tree content may still be
                            // stable, which is exactly what the content rule
                            // measures; try it at once instead of burning the
                            // rest of the deadline on a rule that cannot win.
                            fell_back_from = Some(ObservationSettle::Strict.as_str());
                            continue;
                        }
                        unsettled = Some(error);
                    }
                    Ok(observation) => {
                        unsettled = None;
                        if let Some(failure) = observation_binding_error(backend.id(), &observation)
                        {
                            return failure;
                        }
                        // Only an *explicit* `settle: content` is checked against the
                        // record: an auto fallback reports whatever the backend
                        // proved (a backend that reports no records still returns a
                        // settled observation or an error, which is its contract),
                        // while a step that named the rule must see it applied.
                        if let Some(failure) = settled_observation_error(
                            "wait_for",
                            &observation,
                            mode,
                            requested.is_some(),
                        ) {
                            return failure;
                        }
                        if let Some(failure) = meaningful_error(&observation) {
                            return failure;
                        }
                        let matched = observation.tree.find_one(&wait.selector).cloned();
                        let satisfied = matched
                            .as_ref()
                            .is_some_and(|node| !wait.require_visible || node.visible);
                        let settle_record = settle_record_of(
                            &observation,
                            mode,
                            settle_wait_ms + attempt_started.elapsed().as_millis() as u64,
                            fell_back_from,
                        );
                        let observed_at = Instant::now();
                        last = Some(observation);
                        if satisfied {
                            let confirmed = mode != ObservationSettle::Content
                                || content_confirmation.is_some_and(|first| {
                                    observed_at.duration_since(first) >= content_window
                                });
                            if confirmed {
                                let mut exec = StepExec::pass();
                                exec.observation = last;
                                exec.settle = settle_record;
                                return exec;
                            }
                            // The first content-settled observation that holds:
                            // remember when it was seen and wait for the second.
                            content_confirmation.get_or_insert(observed_at);
                        } else {
                            content_confirmation = None;
                        }
                    }
                    Err(error) => {
                        return StepExec::backend_error(&error, backend.id(), "wait_for");
                    }
                }
                let elapsed = started.elapsed();
                if elapsed >= timeout {
                    if let Some(error) = unsettled.take() {
                        return StepExec::backend_error(&error, backend.id(), "wait_for");
                    }
                    // A selector that matches only a node the user cannot see is
                    // the GR2 W4 false-pass class: say so by name (bounds and
                    // visible rect), instead of a bare timeout over "no match".
                    let offscreen = last
                        .as_ref()
                        .and_then(|observation| observation.tree.find_invisible(&wait.selector))
                        .map(|node| {
                            serde_json::json!({
                                "node_id": node.id,
                                "text": node.text,
                                "content_desc": node.content_desc,
                                "class": node.class,
                                "bounds": node.bounds.to_string(),
                                "visible_bounds": node.visible_bounds.map(|b| b.to_string()),
                                "window": node.window,
                            })
                        });
                    let mut exec =
                        StepExec::failed(assertions::element_missing("wait_for", &wait.selector, last.as_ref()));
                    exec.status = Status::Timeout;
                    exec.settle = Some(crate::settle::observation_settle_record(
                        requested.unwrap_or(ObservationSettle::Content),
                        false,
                        settle_wait_ms,
                        crate::settle::SETTLE_REASON_UNSETTLED,
                    ));
                    exec.error = Some(
                        ProtocolError::new(
                            ErrorCode::Timeout,
                            format!(
                                "wait_for timed out after {} ms: selector {} did not satisfy the condition (require_visible={}){}{}",
                                wait.timeout_ms, wait.selector, wait.require_visible,
                                offscreen.as_ref().map_or_else(String::new, |node| format!(
                                    "; it DID match a node that is not visible to the user (bounds {}, visible_bounds {}) — scroll it into view first",
                                    node.get("bounds").and_then(serde_json::Value::as_str).unwrap_or("?"),
                                    node.get("visible_bounds").and_then(serde_json::Value::as_str).unwrap_or("<none>"),
                                )),
                                if content_confirmation.is_some() {
                                    "; it DID match on a content-settled observation, but the content verdict needs a second settled observation at least one window later before it may pass"
                                } else {
                                    ""
                                },
                            ),
                        )
                        .with_hint(
                            "The live tree kept being re-traversed until the deadline. Increase \
                             `timeout_ms` if the app is genuinely slow, or fix the selector/state; \
                             a timeout is a structured failure, never a pass. The step's `settle` \
                             record names the rule(s) it asked for and why each observation failed.",
                        )
                        .with_details(serde_json::json!({
                            "timeout_ms": wait.timeout_ms,
                            "elapsed_ms": elapsed.as_millis() as u64,
                            "polls": polls,
                            "require_visible": wait.require_visible,
                            "selector": wait.selector,
                            "offscreen_match": offscreen,
                            "content_confirmation_pending": content_confirmation.is_some(),
                            "settle": exec.settle,
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
            backend.set_input_settle(matches!(tap.settle, Some(StepSettle::Fast)));
            backend.set_input_content_settle(matches!(tap.settle, Some(StepSettle::Content)));
            let action = InputAction::Tap {
                selector: tap.selector.clone(),
                x: tap.x,
                y: tap.y,
            };
            input_step(backend, &action, caps, previous, last_frame_seq)
        }
        StepRef::Swipe(swipe) => {
            capabilities_used.insert(Capability::Input.as_str().to_string());
            if !caps.input {
                return StepExec::unsupported(
                    Capability::Input,
                    "the backend cannot deliver a swipe/scroll gesture",
                );
            }
            backend.set_settle_timeout(swipe.settle_timeout_ms);
            backend.set_input_settle(matches!(swipe.settle, Some(StepSettle::Fast)));
            backend.set_input_content_settle(matches!(swipe.settle, Some(StepSettle::Content)));
            let (from, to) = match swipe_endpoints(
                backend,
                swipe.from,
                swipe.to,
                swipe.direction,
                swipe.distance,
            ) {
                Ok(points) => points,
                Err(error) => return StepExec::backend_error(&error, backend.id(), "swipe"),
            };
            let action = InputAction::Swipe {
                from: (from.0.round() as i32, from.1.round() as i32),
                to: (to.0.round() as i32, to.1.round() as i32),
                duration_ms: swipe_duration(swipe.duration_ms, swipe.fling),
                fling: swipe.fling,
            };
            input_step(backend, &action, caps, previous, last_frame_seq)
        }
        StepRef::ScrollUntil(scroll) => {
            capabilities_used.insert(Capability::Input.as_str().to_string());
            if !caps.input {
                return StepExec::unsupported(
                    Capability::Input,
                    "the backend cannot deliver a scroll_until search",
                );
            }
            // A found node is only ever reported from a settled traversal, so
            // the search always runs under the strict rule.
            backend.set_settle_timeout(scroll.settle_timeout_ms);
            backend.set_input_settle(false);
            backend.set_input_content_settle(false);
            let (from, to) = match swipe_endpoints(
                backend,
                scroll.from,
                scroll.to,
                scroll.direction,
                scroll.distance,
            ) {
                Ok(points) => points,
                Err(error) => {
                    return StepExec::backend_error(&error, backend.id(), "scroll_until")
                }
            };
            let max_attempts = scroll
                .max_attempts
                .unwrap_or(if scroll.fling { 10 } else { 6 })
                .clamp(1, 50);
            scroll_until_step(
                backend,
                &scroll.selector,
                from,
                to,
                swipe_duration(scroll.duration_ms, scroll.fling),
                scroll.fling,
                max_attempts,
                last_frame_seq,
            )
        }
        StepRef::Type(ty) => {
            capabilities_used.insert(Capability::Input.as_str().to_string());
            if !caps.input {
                return StepExec::unsupported(Capability::Input, "the backend cannot deliver type");
            }
            backend.set_settle_timeout(ty.settle_timeout_ms);
            backend.set_input_settle(matches!(ty.settle, Some(StepSettle::Fast)));
            backend.set_input_content_settle(matches!(ty.settle, Some(StepSettle::Content)));
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
            backend.set_input_settle(matches!(key.settle, Some(StepSettle::Fast)));
            backend.set_input_content_settle(matches!(key.settle, Some(StepSettle::Content)));
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
            // A scenario `back` has no settle field: it is always strict, and a
            // previous `settle: fast` input must not leak into it.
            backend.set_input_settle(false);
            input_step(backend, &InputAction::Back, caps, previous, last_frame_seq)
        }
        StepRef::Rotate(rotate) => {
            capabilities_used.insert(Capability::Input.as_str().to_string());
            if !caps.input {
                return StepExec::unsupported(Capability::Input, "the backend cannot rotate");
            }
            backend.set_input_settle(false);
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
            // REL3A: `assert_text` honours `timeout_ms` and `settle` like every
            // other observation step (before this it read the bound frame once
            // and returned ~1 ms after an action whose effect was still loading).
            // An assertion has no separate settle budget: it resets the previous
            // step's `settle_timeout_ms` so a tap's budget can never leak in, and
            // its own `timeout_ms` bounds the whole wait.
            backend.set_settle_timeout(None);
            observation_assertion(
                backend,
                |observation| eval_text(assertion, observation),
                "assert_text",
                bound_frame_seq,
                assertion.timeout_ms,
                assertion.settle,
            )
        }
        StepRef::AssertVisible(assertion) => {
            capabilities_used.insert(Capability::UiTree.as_str().to_string());
            if !caps.ui_tree {
                return StepExec::unsupported(
                    Capability::UiTree,
                    "the backend cannot observe a UI tree",
                );
            }
            backend.set_settle_timeout(None);
            observation_assertion(
                backend,
                |observation| eval_visible(assertion, observation),
                "assert_visible",
                bound_frame_seq,
                assertion.timeout_ms,
                assertion.settle,
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
            backend.set_settle_timeout(None);
            observation_assertion(
                backend,
                |observation| eval_not_visible(assertion, observation),
                "assert_not_visible",
                bound_frame_seq,
                assertion.timeout_ms,
                assertion.settle,
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
            backend.set_settle_timeout(None);
            observation_assertion(
                backend,
                |observation| eval_state(assertion, observation),
                "assert_state",
                bound_frame_seq,
                assertion.timeout_ms,
                assertion.settle,
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
            exec.delivery = observation.input.clone();
            exec.observation = Some(observation);
            exec
        }
        Err(error) => {
            // REL3E: a failed/timed-out settle still has the delivery evidence.
            // A tap nothing consumed produces no new frame and *will* time out;
            // the step must still say `handled: false` and name what it hit, or
            // the agent is back to guessing from `state_changed`.
            let mut exec = StepExec::backend_error(&error, backend.id(), action.kind());
            exec.delivery = backend.last_input_delivery();
            exec
        }
    }
}

/// Resolve a scenario swipe's two points: explicit `from`/`to`, or
/// `direction` (+ optional `distance`) over the backend's live display.
///
/// Coordinates are display pixels, the same space as a tap and tree bounds. A
/// backend that cannot report its display size cannot resolve a direction, and
/// says so instead of guessing a screen.
fn swipe_endpoints(
    backend: &dyn Backend,
    from: Option<Point>,
    to: Option<Point>,
    direction: Option<SwipeDirection>,
    distance: Option<u32>,
) -> Result<((f32, f32), (f32, f32)), BackendError> {
    match (from, to) {
        (Some(from), Some(to)) => Ok((
            (from.x as f32, from.y as f32),
            (to.x as f32, to.y as f32),
        )),
        (None, None) => {
            let direction = direction.ok_or_else(|| BackendError::Error {
                code: ErrorCode::InvalidArgument,
                detail: "a swipe needs `from`/`to` points or a `direction`".to_string(),
            })?;
            let (width, height) = backend.display_size().ok_or_else(|| BackendError::Error {
                code: ErrorCode::UnsupportedAssertion,
                detail: format!(
                    "this backend does not report its display size, so a direction-only swipe \
                     ({direction:?}) cannot be resolved into screen points; name `from` and `to` \
                     explicitly"
                ),
            })?;
            Ok(direction.endpoints(distance, width, height))
        }
        _ => Err(BackendError::Error {
            code: ErrorCode::InvalidArgument,
            detail: "a swipe needs both `from` and `to` points".to_string(),
        }),
    }
}

/// The gesture duration a swipe step asks for (`fling` shortens the default).
fn swipe_duration(duration_ms: Option<u64>, fling: bool) -> u64 {
    duration_ms
        .unwrap_or(if fling { 120 } else { 400 })
        .clamp(1, 30_000)
}

/// A `scroll_until` step: a bounded search whose found node comes from a
/// settled observation (REL3S).
///
/// The backend owns the per-attempt gesture + fresh traversal (the ART engine
/// reuses the same `SessionBackend::scroll_until` the CLI/MCP path uses). This
/// only enforces the executor's own rules: the returned observation must bind a
/// frame, it must be newer than the previous step's frame, and the search report
/// travels in the step's `input.until` evidence.
fn scroll_until_step(
    backend: &mut dyn Backend,
    selector: &acos_protocol::Selector,
    from: (f32, f32),
    to: (f32, f32),
    duration_ms: u64,
    fling: bool,
    max_attempts: usize,
    last_frame_seq: Option<u64>,
) -> StepExec {
    let describe = serde_json::to_value(selector).unwrap_or(serde_json::Value::Null);
    match backend.scroll_until(selector, from, to, duration_ms, fling, max_attempts) {
        Ok((report, observation)) => {
            if let Some(failure) = observation_binding_error(backend.id(), &observation) {
                return failure;
            }
            if let Some(previous_seq) = last_frame_seq {
                if observation.frame.frame_seq <= previous_seq {
                    return no_fresh_frame_failure(previous_seq, &observation, "scroll_until");
                }
            }
            let mut exec = StepExec::pass();
            exec.delivery = observation
                .input
                .clone()
                .or_else(|| backend.last_input_delivery());
            exec.until = Some(serde_json::json!({
                "selector": describe,
                "found": true,
                "attempts": report.attempts,
                "frame_seq": report.frame_seq,
            }));
            exec.observation = Some(observation);
            exec
        }
        Err(error) => {
            let mut exec = StepExec::backend_error(&error, backend.id(), "scroll_until");
            exec.delivery = backend.last_input_delivery();
            // Keep the search report even on a miss: the error details already
            // carry the attempts and the last tree's texts, and this block is
            // the step-shaped view of the same facts. REL3H: a search that
            // reached the end of the list says so (`end_of_list: true`) instead
            // of looking like a node that was never rendered.
            if let BackendError::Protocol {
                code: ErrorCode::ElementNotFound | ErrorCode::EndOfList,
                details: Some(details),
                ..
            } = &error
            {
                exec.until = Some(serde_json::json!({
                    "selector": details.get("selector").cloned().unwrap_or(describe),
                    "found": false,
                    "end_of_list": details.get("end_of_list").cloned().unwrap_or(serde_json::Value::Bool(false)),
                    "attempts": details.get("attempts").cloned().unwrap_or(serde_json::Value::Null),
                    "max_attempts": details.get("max_attempts").cloned().unwrap_or(serde_json::Value::Null),
                    "frame_seq": details.get("last_frame_seq").cloned().unwrap_or(serde_json::Value::Null),
                    "last_tree_texts": details.get("last_tree_texts").cloned().unwrap_or(serde_json::Value::Null),
                }));
            }
            exec
        }
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
    evaluate: impl Fn(&SessionObservation) -> Evaluated,
    context: &str,
    bound_frame_seq: Option<u64>,
    timeout_ms: Option<u64>,
    settle: Option<StepSettle>,
) -> StepExec {
    // REL3A: `settle: content` (and the auto fallback) is a rule about a
    // *fresh* observation; an assertion that only reads the frame a previous
    // step bound cannot apply it retroactively. Remember which rule the
    // assertion asked for so the record it publishes is the rule actually used.
    let requested = ObservationSettle::from_step(settle);
    let mut waited_ms = 0u64;
    let mut fell_back_from: Option<&'static str> = None;

    // 1) The bound frame first: an assertion that already holds costs no
    //    traversal, exactly as before REL3A. Its mode is the mode that bound it.
    let first = match backend.observe() {
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
            // REL3A §2: a content verdict needs a second content-settled
            // observation at least one window later. When a `timeout_ms` gives
            // us the room to take it, do not short-circuit on the bound frame's
            // content record: go through the poll loop (which confirms it).
            // Without `timeout_ms` the old cheap single-observation path is
            // unchanged (see AGENT_GUIDE: `settle` takes effect with
            // `timeout_ms` on assertions).
            let bound_is_content = observation
                .settle
                .as_ref()
                .and_then(|settle| settle.get("mode"))
                .and_then(serde_json::Value::as_str)
                == Some("content");
            let needs_content_confirmation = timeout_ms.is_some()
                && (requested == Some(ObservationSettle::Content)
                    || (requested.is_none() && bound_is_content));
            if evaluated.result.passed && !needs_content_confirmation {
                let mut exec = StepExec::passed_assertion(evaluated);
                exec.settle = settle_record_of(&observation, requested.unwrap_or(ObservationSettle::Strict), 0, None);
                exec.observation = Some(observation);
                return exec;
            }
            Some((observation, evaluated))
        }
        Err(error) => return StepExec::backend_error(&error, backend.id(), context),
    };

    // 2) REL3A: `timeout_ms` is a real wait. Poll *fresh* settled observations
    //    until the condition holds or the deadline passes. A poll is a new
    //    traversal, so the assertion no longer describes the previous step's
    //    frame - it describes the frame it actually judged, and the executor's
    //    ledger rebinds to it (`rebinds_bound_frame`).
    let Some(timeout_ms) = timeout_ms else {
        let (observation, evaluated) = first.expect("a failed evaluation is present");
        let mut exec = StepExec::failed(evaluated);
        exec.settle = settle_record_of(&observation, requested.unwrap_or(ObservationSettle::Strict), 0, None);
        exec.observation = Some(observation);
        return exec;
    };
    let started = Instant::now();
    let deadline = Duration::from_millis(timeout_ms);
    let mut interval = Duration::from_millis(WAIT_FOR_POLL_INTERVAL_MS);
    let mut attempts = 0u64;
    let mut last_evaluated: Option<(SessionObservation, Evaluated)> = first;
    let mut unsettled: Option<BackendError> = None;
    // The same two-consecutive-content-observations rule as `wait_for`.
    let content_window = Duration::from_millis(crate::settle::content_window_ms(None));
    let mut content_confirmation: Option<Instant> = None;
    loop {
        attempts += 1;
        let mode = match requested {
            Some(mode) => mode,
            None if fell_back_from.is_some() => ObservationSettle::Content,
            None => ObservationSettle::Strict,
        };
        // REL3A: scoped to this observation; strict is restored on every exit.
        let mut scope = ObservationScope::new(backend, mode);
        let backend = scope.backend();
        let attempt_started = Instant::now();
        match backend.refresh() {
            Err(error) if is_settle_timeout(&error) => {
                waited_ms += attempt_started.elapsed().as_millis() as u64;
                content_confirmation = None;
                if requested.is_none() && mode == ObservationSettle::Strict {
                    // The same auto rule as `wait_for`: the event settle cannot
                    // settle this screen, so try the content-stable rule instead
                    // of spending the whole budget on a rule that cannot win.
                    fell_back_from = Some(ObservationSettle::Strict.as_str());
                    continue;
                }
                unsettled = Some(error);
            }
            Ok(observation) => {
                unsettled = None;
                if let Some(failure) = observation_binding_error(backend.id(), &observation) {
                    return failure;
                }
                if let Some(failure) = settled_observation_error(
                    context,
                    &observation,
                    mode,
                    requested.is_some(),
                ) {
                    return failure;
                }
                if let Some(failure) = meaningful_error(&observation) {
                    return failure;
                }
                let evaluated = evaluate(&observation);
                let settled = settle_record_of(
                    &observation,
                    mode,
                    waited_ms + attempt_started.elapsed().as_millis() as u64,
                    fell_back_from,
                );
                let observed_at = Instant::now();
                let passed = evaluated.result.passed;
                last_evaluated = Some((observation.clone(), evaluated));
                if passed {
                    let confirmed = mode != ObservationSettle::Content
                        || content_confirmation.is_some_and(|first| {
                            observed_at.duration_since(first) >= content_window
                        });
                    if confirmed {
                        let mut exec =
                            StepExec::passed_assertion(last_evaluated.take().expect("just set").1);
                        exec.settle = settled;
                        exec.observation = Some(observation);
                        return exec;
                    }
                    content_confirmation.get_or_insert(observed_at);
                } else {
                    content_confirmation = None;
                }
            }
            Err(error) => return StepExec::backend_error(&error, backend.id(), context),
        }
        let elapsed = started.elapsed();
        if elapsed >= deadline {
            // REL3A §3: a step that could not obtain a *settled* observation
            // fails as an infra timeout, never as an app assertion failure and
            // never as a pass. A settled observation that simply did not match
            // is an ordinary assertion failure, and says when it gave up.
            if let Some(error) = unsettled.take() {
                let mut failure = StepExec::backend_error(&error, backend.id(), context);
                failure.settle = Some(crate::settle::observation_settle_record(
                    requested.unwrap_or(ObservationSettle::Content),
                    false,
                    waited_ms,
                    crate::settle::SETTLE_REASON_UNSETTLED,
                ));
                return failure;
            }
            let Some((observation, evaluated)) = last_evaluated.take() else {
                return StepExec::failed_backend(&BackendError::Protocol {
                    code: ErrorCode::Timeout,
                    detail: format!(
                        "{context}: no settled observation within {timeout_ms} ms"
                    ),
                    hint: Some(
                        "An assertion needs a settled observation of the rule it reports. \
                         Nothing it judged was settled, so this is an infra timeout and never a \
                         pass."
                            .to_string(),
                    ),
                    details: Some(serde_json::json!({
                        "reason": "settle-timeout",
                        "timeout_ms": timeout_ms,
                        "attempts": attempts,
                    })),
                });
            };
            let mut exec = StepExec::failed(evaluated);
            exec.settle = settle_record_of(&observation, requested.unwrap_or(ObservationSettle::Strict), waited_ms, fell_back_from);
            exec.observation = Some(observation);
            if let Some(error) = exec.error.as_mut() {
                let mut details = error
                    .details
                    .take()
                    .unwrap_or_else(|| serde_json::json!({}));
                if let Some(object) = details.as_object_mut() {
                    object.insert("timeout_ms".into(), serde_json::json!(timeout_ms));
                    object.insert("waited_ms".into(), serde_json::json!(elapsed.as_millis() as u64));
                    object.insert("attempts".into(), serde_json::json!(attempts));
                }
                error.details = Some(details);
            }
            return exec;
        }
        let remaining = deadline.saturating_sub(elapsed);
        if remaining.is_zero() {
            continue;
        }
        std::thread::sleep(interval.min(remaining));
        interval = (interval * 2).min(Duration::from_millis(WAIT_FOR_MAX_POLL_INTERVAL_MS));
    }
}

/// REL3A §2: the settle record an observation publishes.
///
/// The backend's own record wins (it names what it measured: `bound`,
/// `window_ms`, `traversals`, `signature`, ...); it is normalised to always
/// carry `mode`, `settled`, `waited_ms` and `reason`. A backend that reported no
/// record gets an executor record that says so, never a made-up measurement.
fn settle_record_of(
    observation: &SessionObservation,
    mode: ObservationSettle,
    waited_ms: u64,
    fell_back_from: Option<&'static str>,
) -> Option<serde_json::Value> {
    let record = observation.settle.as_ref().map(|record| {
        let mut normalised = crate::settle::settle_record(record, waited_ms);
        if let Some(object) = normalised.as_object_mut() {
            if let Some(from) = fell_back_from {
                object.insert("fallback_from".into(), serde_json::json!(from));
                object.insert(
                    "fallback_reason".into(),
                    serde_json::json!(crate::settle::SETTLE_REASON_STRICT_FELL_BACK),
                );
            }
        }
        normalised
    });
    match record {
        Some(record) => Some(record),
        // No backend record: the backend did not confirm anything. REL3A §2: an
        // observation may never be labelled `mode: content` on the executor's
        // own say-so, so an unconfirmed auto fallback is reported as `strict`
        // with `fallback_unconfirmed: true` (and the mode that was asked for).
        None => Some({
            let unconfirmed = mode == ObservationSettle::Content;
            let reported_mode = if unconfirmed {
                ObservationSettle::Strict
            } else {
                mode
            };
            let reason = if unconfirmed {
                crate::settle::SETTLE_REASON_FALLBACK_UNCONFIRMED
            } else {
                crate::settle::SETTLE_REASON_SETTLED
            };
            let mut record = crate::settle::observation_settle_record(
                reported_mode,
                true,
                waited_ms,
                reason,
            );
            if let Some(object) = record.as_object_mut() {
                if unconfirmed {
                    object.insert("fallback_unconfirmed".into(), serde_json::json!(true));
                    object.insert("requested_mode".into(), serde_json::json!(mode.as_str()));
                }
                if let Some(from) = fell_back_from {
                    object.insert("fallback_from".into(), serde_json::json!(from));
                }
            }
            record
        }),
    }
}

/// REL3A §1: a step that explicitly asked for `settle: content` must receive a
/// content-settled observation of the frame it binds; otherwise the rule was not
/// applied and the step fails closed instead of letting a strict or fast
/// observation stand in for it.
fn settled_observation_error(
    context: &str,
    observation: &SessionObservation,
    mode: ObservationSettle,
    explicit: bool,
) -> Option<StepExec> {
    if mode != ObservationSettle::Content || !explicit {
        return None;
    }
    let settled = observation.settle.as_ref().is_some_and(|settle| {
        settle.get("mode").and_then(serde_json::Value::as_str) == Some("content")
            && settle.get("settled").and_then(serde_json::Value::as_bool) == Some(true)
            && settle.get("frame_seq").and_then(serde_json::Value::as_u64)
                == Some(observation.frame.frame_seq)
    });
    if settled {
        return None;
    }
    Some(StepExec::failed_backend(&BackendError::Protocol {
        code: ErrorCode::UnsupportedAssertion,
        detail: format!(
            "{context}: `settle: content` was requested but this observation carries no settled \
             content record for its own frame, so the content-stable rule was not applied",
        ),
        hint: Some(
            "An unapplied settle rule never passes. Run the step on the ART engine (which \
             implements `settle: content`), or drop the option and say what rule actually backs \
             the observation."
                .to_string(),
        ),
        details: Some(serde_json::json!({
            "reason": "content_settle_unhonored",
            "observation_frame_seq": observation.frame.frame_seq,
            "settle": observation.settle,
        })),
    }))
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
                | StepRef::Swipe(_)
                | StepRef::ScrollUntil(_)
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
        settle: None,
        delivery: None,
        until: None,
        warnings: Vec::new(),
    })
}

/// A blank/placeholder frame must never satisfy a UI or screenshot assertion.
/// A strict-settle deadline that elapsed inside one observation (P12b's
/// `settle_timeout_error`: code `TIMEOUT` with the guest's `settle` record in
/// the details). Any other timeout or error is not this.
fn is_settle_timeout(error: &BackendError) -> bool {
    matches!(
        error,
        BackendError::Protocol { code: ErrorCode::Timeout, details: Some(details), .. }
            if details.get("settle").is_some()
    )
}

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
        settle: None,
        delivery: None,
        until: None,
        warnings: Vec::new(),
    })
}

/// An input step that asked for the `settle: fast` navigation rule.
fn fast_input_step(step: &Step) -> bool {
    match step.as_step() {
        Some(StepRef::Tap(tap)) => matches!(tap.settle, Some(StepSettle::Fast)),
        Some(StepRef::Swipe(swipe)) => matches!(swipe.settle, Some(StepSettle::Fast)),
        Some(StepRef::Type(ty)) => matches!(ty.settle, Some(StepSettle::Fast)),
        Some(StepRef::Key(key)) => matches!(key.settle, Some(StepSettle::Fast)),
        _ => false,
    }
}

/// SE1: did this step opt into the per-step pixel-change requirement?
///
/// Only the input actions that carry a strict settle record may ask for it:
/// `tap`, `type`, `key` and `back`. Every other step kind has no after-action
/// frame of its own, so it cannot express the rule.
fn expect_change_requested(step: &Step) -> bool {
    match step.as_step() {
        Some(StepRef::Tap(tap)) => tap.expect_change,
        Some(StepRef::Swipe(swipe)) => swipe.expect_change,
        Some(StepRef::Type(ty)) => ty.expect_change,
        Some(StepRef::Key(key)) => key.expect_change,
        Some(StepRef::Back(back)) => back.expect_change,
        _ => false,
    }
}

/// REL3H2: the scenario's own `expect_handled` claim for an input step.
///
/// `Some(true)` means "the app must consume this gesture", `Some(false)` means
/// "this press is expected to be a no-op" (it silences the warning), and `None`
/// (the default) means the step passes as measured and carries
/// `INPUT_UNHANDLED`. Steps that inject no input have no claim.
fn expect_handled_requested(step: &Step) -> Option<bool> {
    match step.as_step() {
        Some(StepRef::Tap(tap)) => tap.expect_handled,
        Some(StepRef::Swipe(swipe)) => swipe.expect_handled,
        Some(StepRef::Type(ty)) => ty.expect_handled,
        Some(StepRef::Key(key)) => key.expect_handled,
        Some(StepRef::Back(back)) => back.expect_handled,
        _ => None,
    }
}

/// REL3H2: the app window's own `finishInputEvent(handled)` answer for a step.
///
/// `Some(false)` is the app's proof that nothing consumed the gesture;
/// `Some(true)` the proof that something did; `None` means the guest did not ack
/// every event (unknown, never a guess - REL3E's rule).
fn step_handled(exec: &StepExec) -> Option<bool> {
    exec.delivery.as_ref().and_then(|delivery| delivery.handled)
}

/// REL3H2: `expect_handled: true` on a gesture nothing consumed is a failed
/// claim, never a pass.
///
/// REL3H turned a delivered-but-unhandled input into a *settled* observation
/// (instead of a `TIMEOUT`), which is honest about the settle and silent about
/// the flow: a scenario whose tap missed the control passed. `expect_handled:
/// true` is the opt-in that says "this press must reach something", and an
/// unhandled ack then fails with `INPUT_NOT_HANDLED` (error class `assertion`),
/// naming what the point hit. `handled: null` (no complete ack) cannot verify
/// the claim either, so it is `UNSUPPORTED_ASSERTION` - fail closed, never a
/// silent pass.
fn expect_handled_failure(step: &Step, exec: &StepExec) -> Option<StepExec> {
    if expect_handled_requested(step) != Some(true) || exec.status != Status::Pass {
        return None;
    }
    let handled = step_handled(exec);
    if handled == Some(true) {
        return None;
    }
    let delivery = exec.delivery.as_ref();
    let target = delivery.and_then(|delivery| delivery.target.clone());
    let settle = delivery.and_then(|delivery| delivery.settle.clone());
    let (published, acked) = delivery
        .map(|delivery| (delivery.published, delivery.acks.len()))
        .unwrap_or((0, 0));
    let error = if handled == Some(false) {
        BackendError::Protocol {
            code: ErrorCode::InputNotHandled,
            detail: format!(
                "{}: `expect_handled: true` and the app window acknowledged the gesture \
                 `finishInputEvent(handled) = false` ({acked}/{published} event(s) acked, every \
                 one unhandled) - the input was delivered and nothing consumed it",
                step.kind()
            ),
            hint: Some(ErrorCode::InputNotHandled.hint().to_string()),
            details: Some(serde_json::json!({
                "reason": "expect_handled",
                "handled": false,
                "step_kind": step.kind(),
                "published": published,
                "acked": acked,
                "target": target,
                "settle": settle,
            })),
        }
    } else {
        BackendError::Protocol {
            code: ErrorCode::UnsupportedAssertion,
            detail: format!(
                "{}: `expect_handled: true` cannot be verified - the guest did not ack every \
                 published event, so the app's handling of this gesture is unknown",
                step.kind()
            ),
            hint: Some(
                "An unverifiable handling claim never passes. Run the step on the ART engine \
                 (which reports the window's own `finishInputEvent(handled)` acks), or drop \
                 `expect_handled`."
                    .to_string(),
            ),
            details: Some(serde_json::json!({
                "reason": "expect_handled_unverified",
                "handled": serde_json::Value::Null,
                "step_kind": step.kind(),
                "published": published,
                "acked": acked,
                "target": target,
                "settle": settle,
            })),
        }
    };
    let mut failure = StepExec::failed(Evaluated {
        result: AssertionResult {
            kind: "expect_handled".to_string(),
            passed: false,
            expected: Some(serde_json::json!({"handled": true})),
            actual: Some(serde_json::json!({"handled": handled})),
            diff: Some(AssertionDiff {
                expected: serde_json::json!({"handled": true}),
                actual: serde_json::json!({"handled": handled}),
                path: Some("input.handled".to_string()),
                detail: Some(
                    "the app window's own finishInputEvent(handled) answer for this gesture"
                        .to_string(),
                ),
            }),
            matched_node_ids: Vec::new(),
            message: None,
        },
        error: Some(error),
    });
    // The failing step's tree/frame and delivery are still evidence.
    failure.observation = exec.observation.clone();
    failure.delivery = exec.delivery.clone();
    failure.settle = exec.settle.clone();
    Some(failure)
}

/// REL3H2: an unhandled input is never a silent plain pass.
///
/// A gesture the app window acknowledged `finishInputEvent(handled) = false` did
/// nothing, and REL3H lets that step pass (a delivered no-op on an honest,
/// stable screen is a settled observation, not a timeout). Passing silently is
/// the false-green half: an agent that reads only `status` cannot tell "the tap
/// worked" from "nothing was there to tap". The step stays exactly as measured
/// and carries a structured `INPUT_UNHANDLED` warning - in the step JSON and in
/// the verdict's `provenance.warnings[]` - unless the scenario says the no-op
/// was intended (`expect_handled: false`).
fn input_unhandled_warning(step: &Step, index: usize, exec: &StepExec) -> Option<serde_json::Value> {
    if exec.status != Status::Pass || expect_handled_requested(step).is_some() {
        return None;
    }
    if step_handled(exec) != Some(false) {
        return None;
    }
    let delivery = exec.delivery.as_ref()?;
    let unhandled: Vec<u64> = delivery
        .acks
        .iter()
        .filter(|ack| !ack.handled)
        .map(|ack| ack.seq)
        .collect();
    let target = delivery.target.clone();
    let hint = acos_protocol::ui::handling_hint(
        target.as_ref(),
        Some(false),
        delivery.visual_change,
    );
    Some(serde_json::json!({
        "code": acos_protocol::provenance::WARN_INPUT_UNHANDLED,
        "step": index,
        "step_kind": step.kind(),
        "message": format!(
            "step {index} ({}): the gesture was delivered and the app window acknowledged every \
             event `finishInputEvent(handled) = false` - nothing consumed it. The step is `pass` \
             because it made no change claim; it is not evidence that the input reached a control.",
            step.kind()
        ),
        "hint": hint.unwrap_or_else(|| {
            "Set `expect_handled: true` on this step to make the app's handling a hard \
             requirement (it then fails with INPUT_NOT_HANDLED), or `expect_handled: false` when \
             the no-op press is intended and this warning should not be reported."
                .to_string()
        }),
        "handled": false,
        "published": delivery.published,
        "acked": delivery.acks.len(),
        "unhandled": unhandled,
        "target": target,
    }))
}

/// REL3E: the `input` block of a step JSON.
///
/// `handled` is the app window's own `finishInputEvent(handled)` answer (`null`
/// when the guest did not ack every event), `target` is the resolved node plus
/// the hit-test at the injected point, and `expect_change` is `"auto"` for a tap
/// (whose `state_change` assertion is the step's own, not the scenario's) or the
/// scenario's explicit flag for `type`/`key`/`back`.
fn step_input(
    step: &Step,
    delivery: Option<&crate::backend::InputDelivery>,
    until: Option<&serde_json::Value>,
) -> Option<serde_json::Value> {
    let expect_change = match step.as_step() {
        // A tap's state-change rule runs on every step, so an unflagged tap says
        // `"auto"` - never a silent `false` that hides the rule.
        Some(StepRef::Tap(tap)) => {
            if tap.expect_change {
                serde_json::json!(true)
            } else {
                serde_json::json!("auto")
            }
        }
        Some(StepRef::Swipe(swipe)) => serde_json::json!(swipe.expect_change),
        Some(StepRef::ScrollUntil(_)) => serde_json::json!(false),
        Some(StepRef::Type(ty)) => serde_json::json!(ty.expect_change),
        Some(StepRef::Key(key)) => serde_json::json!(key.expect_change),
        Some(StepRef::Back(back)) => serde_json::json!(back.expect_change),
        _ => return None,
    };
    let mut input = match delivery {
        None => serde_json::json!({
            "handled": serde_json::Value::Null,
            "handled_evidence": {"source": "none", "published": 0, "acked": 0},
            "target": serde_json::Value::Null,
            "expect_change": expect_change,
        }),
        Some(delivery) => {
            let unhandled: Vec<u64> = delivery
                .acks
                .iter()
                .filter(|ack| !ack.handled)
                .map(|ack| ack.seq)
                .collect();
            let target = delivery.target.clone();
            let mut input = serde_json::json!({
                "handled": delivery.handled,
                "handled_evidence": {
                    "source": if delivery.published > 0 { "finishInputEvent" } else { "none" },
                    "published": delivery.published,
                    "acked": delivery.acks.len(),
                    "unhandled": unhandled,
                },
                "target": target.clone().unwrap_or(serde_json::Value::Null),
                "expect_change": expect_change,
            });
            // The same one-line explanation an MCP reply carries, from the same facts.
            if let Some(hint) = acos_protocol::ui::handling_hint(
                target.as_ref(),
                delivery.handled,
                delivery.visual_change,
            ) {
                input["handled_hint"] = serde_json::json!(hint);
            }
            input
        }
    };
    if let Some(until) = until {
        input["until"] = until.clone();
    }
    Some(input)
}

/// The artifact reference for one observation's frame, when it was captured.
///
/// The executor writes `frames/<seq>.png` for every observation that carried
/// PNG bytes, so the ref is only named when those bytes exist.
fn frame_ref(observation: Option<&SessionObservation>) -> serde_json::Value {
    let Some(observation) = observation else {
        return serde_json::Value::Null;
    };
    let seq = observation.frame.frame_seq;
    serde_json::json!({
        "frame_seq": seq,
        "frame": if observation.screenshot_png.is_some() {
            serde_json::Value::String(acos_protocol::frame_file_name(seq))
        } else {
            serde_json::Value::Null
        },
    })
}

/// SE1: enforce an opt-in `expect_change: true` on an input step.
///
/// ADR-0026 decision 2 keeps the P12 rule: a step whose action drew but changed
/// no pixels still settles and passes, and its reply reports
/// `pixels_changed:false`. `expect_change: true` turns that measured fact into
/// a failure (`ASSERTION_FAILED`, `details.reason = "expect_change"`) with the
/// pre-action and post-action frame refs.
///
/// Three honesty rules:
/// * a step that already failed keeps its own error - it is the better evidence;
/// * "could not measure" never passes: a backend that returns no settle record
///   (or a fast label, which validation rejects) is `UNSUPPORTED_ASSERTION` -
///   the engine cannot supply the evidence, so the step is not a pass;
/// * the check runs on the settle record the reply reported, not on a second
///   pixel comparison invented here.
fn expect_change_failure(
    step: &Step,
    exec: &StepExec,
    previous: Option<&SessionObservation>,
) -> Option<StepExec> {
    if !expect_change_requested(step) || exec.status != Status::Pass {
        return None;
    }
    let observation = exec.observation.as_ref();
    let settle = observation
        .and_then(|observation| observation.input.as_ref())
        .and_then(|delivery| delivery.settle.as_ref());
    let pixels_changed = settle
        .and_then(|settle| settle.get("pixels_changed"))
        .and_then(serde_json::Value::as_bool);
    let fast = settle
        .and_then(|settle| settle.get("mode"))
        .and_then(serde_json::Value::as_str)
        == Some("fast");
    let before = frame_ref(previous);
    let after = frame_ref(observation);
    if pixels_changed == Some(true) {
        return None;
    }

    let (error, kind, diff_detail) = if !fast && pixels_changed == Some(false) {
        (
            BackendError::Protocol {
                code: ErrorCode::AssertionFailed,
                detail: format!(
                    "{}: the settled frame is pixel-identical to the pre-action frame \
                     (`settle.pixels_changed:false`); the step proved no visual change",
                    step.kind()
                ),
                hint: Some(
                    "The action was delivered and settled, but nothing on screen changed, so \
                     anything asserted afterwards would describe state this step never produced \
                     (GR4 G4c). Target the control that really writes the state, or drop \
                     `expect_change` if this action legitimately changes no pixels."
                        .to_string(),
                ),
                details: Some(serde_json::json!({
                    "reason": "expect_change",
                    "pixels_changed": false,
                    "step_kind": step.kind(),
                    "before": before,
                    "after": after,
                })),
            },
            "expect_change",
            "the settled frame did not change from the pre-action frame",
        )
    } else {
        (
            BackendError::Protocol {
                code: ErrorCode::UnsupportedAssertion,
                detail: format!(
                    "{}: `expect_change` cannot be verified on this backend/settle rule \
                     (no measured `settle.pixels_changed`)",
                    step.kind()
                ),
                hint: Some(
                    "A pixel-change claim needs a strict settle record that measures the pixels. \
                     Run the step on a backend that reports `pixels_changed` (the ART engine, \
                     strict settle), or drop `expect_change`; an unmeasured change never passes."
                        .to_string(),
                ),
                details: Some(serde_json::json!({
                    "reason": "expect_change",
                    "pixels_changed": serde_json::Value::Null,
                    "step_kind": step.kind(),
                    "settle": settle.cloned(),
                    "before": before,
                    "after": after,
                })),
            },
            "expect_change_unverified",
            "no measured `settle.pixels_changed` on the action's reply",
        )
    };
    let mut failure = StepExec::failed(Evaluated {
        result: AssertionResult {
            kind: kind.to_string(),
            passed: false,
            expected: Some(serde_json::json!({"pixels_changed": true})),
            actual: Some(serde_json::json!({"pixels_changed": pixels_changed})),
            diff: Some(AssertionDiff {
                expected: serde_json::json!({"pixels_changed": true}),
                actual: serde_json::json!({"pixels_changed": pixels_changed}),
                path: Some("settle.pixels_changed".to_string()),
                detail: Some(diff_detail.to_string()),
            }),
            matched_node_ids: Vec::new(),
            message: None,
        },
        error: Some(error),
    });
    // Keep the observation: the failing step's tree/frame are still evidence.
    failure.observation = observation.cloned();
    Some(failure)
}

/// Total nodes in a tree (roots + descendants).
fn tree_node_count(tree: &acos_protocol::UiTree) -> usize {
    fn count(node: &acos_protocol::UiNode) -> usize {
        1 + node.children.iter().map(count).sum::<usize>()
    }
    tree.nodes.iter().map(count).sum()
}

/// REL3A: a tree/text assertion that **waited** (`timeout_ms`) judged a frame it
/// took itself, not the previous step's. That frame becomes the bound frame, so
/// a later strict capture/pixel assertion describes the freshest observation
/// instead of failing on a stale binding.
///
/// An assertion that did not wait keeps the existing rule: it describes the
/// frame the previous frame-advancing step left behind (its `frame_seq` must
/// equal `bound`).
fn rebinds_bound_frame(step: &Step, bound: Option<u64>, observed: u64) -> bool {
    if bound == Some(observed) {
        return false;
    }
    match step.as_step() {
        Some(StepRef::AssertText(assertion)) => assertion.timeout_ms.is_some(),
        Some(StepRef::AssertVisible(assertion) | StepRef::AssertNotVisible(assertion)) => {
            assertion.timeout_ms.is_some()
        }
        Some(StepRef::AssertState(assertion)) => assertion.timeout_ms.is_some(),
        _ => false,
    }
}

/// Steps that evaluate the bound frame itself (UI-tree assertions, a non-fresh
/// capture). After a `settle: fast` input these must fail closed.
/// `assert_screenshot`/`assert_region` take their own fresh strict capture, so
/// they are not blocked (advisor review of 6301c06).
fn binds_to_bound_frame(step: &Step) -> bool {
    match step.as_step() {
        Some(
            StepRef::AssertText(_)
            | StepRef::AssertVisible(_)
            | StepRef::AssertNotVisible(_)
            | StepRef::AssertState(_),
        ) => true,
        Some(StepRef::Capture(capture)) => !capture.fresh,
        _ => false,
    }
}

/// An assertion whose bound frame came from a `settle: fast` input. Fast is the
/// navigation rule (never assertion evidence), so this is a failed binding, not
/// an evaluation of a possibly unsettled frame.
fn fast_bound_failure(kind: &str) -> StepExec {
    let detail = "the bound frame came from a `settle: fast` input, which is navigation only; \
                  add a strict wait_for or a capture (fresh: true) before asserting";
    StepExec::failed(Evaluated {
        result: AssertionResult {
            kind: "frame_binding".to_string(),
            passed: false,
            expected: Some(serde_json::json!({"settle": "strict"})),
            actual: Some(serde_json::json!({"settle": "fast"})),
            diff: Some(AssertionDiff {
                expected: serde_json::json!({"settle": "strict"}),
                actual: serde_json::json!({"settle": "fast"}),
                path: Some("frame.settle".to_string()),
                detail: Some(detail.to_string()),
            }),
            matched_node_ids: Vec::new(),
            message: None,
        },
        error: Some(BackendError::Failed {
            code: ErrorCode::AssertionFailed,
            detail: format!("{kind}: {detail}"),
        }),
    })
}

/// SE2b: the settle record a step's evidence publishes.
///
/// Order matters: the action's own record first (it names the rule this step
/// applied and the frame it bound), then the session's sticky bound record, then
/// a record the backend attached to the step itself.
fn step_settle(
    observation: Option<&SessionObservation>,
    exec_settle: Option<&serde_json::Value>,
    step_ms: u64,
) -> Option<serde_json::Value> {
    // REL3A §2: the record a step's evidence publishes is always normalised to
    // `{mode, settled, waited_ms, reason}` so a caller can branch on the outcome
    // without knowing which engine produced it. `step_ms` is the fallback
    // `waited_ms` when the backend did not separate the settle from the action.
    // Order: the record the step itself produced (a wait/assert that applied a
    // rule of its own, a launch's content record), then the action's own record,
    // then the session's sticky bound record. REL3A: the first position matters
    // for `wait_for`/assertions, whose record names the rule that actually backed
    // *this* observation (including an auto fallback from strict).
    exec_settle
        .cloned()
        .or_else(|| {
            observation.and_then(|observation| {
                observation
                    .input
                    .as_ref()
                    .and_then(|input| input.settle.clone())
                    .or_else(|| observation.settle.clone())
            })
        })
        .map(|record| crate::settle::settle_record(&record, step_ms))
}

/// SE2: did a step request `settle: content` without receiving a content
/// record?
///
/// A backend that cannot implement the content rule must not answer as if it
/// had: the scenario asked for content-stable evidence, and a strict/fast (or
/// absent) record is not that. This is the one place the mode is enforced from
/// the scenario side, so no future backend can silently downgrade it.
fn content_settle_unhonored(step: &Step, exec: &StepExec) -> Option<StepExec> {
    let requested = match step.as_step() {
        Some(StepRef::Tap(tap)) => matches!(tap.settle, Some(StepSettle::Content)),
        Some(StepRef::Swipe(swipe)) => matches!(swipe.settle, Some(StepSettle::Content)),
        Some(StepRef::Type(ty)) => matches!(ty.settle, Some(StepSettle::Content)),
        Some(StepRef::Key(key)) => matches!(key.settle, Some(StepSettle::Content)),
        Some(StepRef::Launch(launch)) => matches!(launch.settle, Some(StepSettle::Content)),
        _ => false,
    };
    if !requested || exec.status != Status::Pass {
        return None;
    }
    let settle = exec
        .observation
        .as_ref()
        .and_then(|observation| {
            // SE2b: the per-action record first. `observation.settle` is the
            // session-sticky bound record, which may describe an earlier
            // traversal; the input's own record describes *this* action.
            observation
                .input
                .as_ref()
                .and_then(|input| input.settle.clone())
                .or_else(|| observation.settle.clone())
        })
        .or_else(|| exec.settle.clone());
    // SE2b: a mode string alone is not proof. The record must say the content
    // window settled AND name the very frame the observation carries; a record
    // from another traversal (or one that timed out) never honours the step.
    // The frame identity is the observation's own frame; a `launch` step hands
    // back `produced_frame` instead of a full observation, so that frame is the
    // one the record must name.
    let observation_frame_seq = exec
        .observation
        .as_ref()
        .map(|observation| observation.frame.frame_seq)
        .or_else(|| exec.produced_frame.as_ref().map(|frame| frame.frame_seq));
    let honoured = settle.as_ref().is_some_and(|settle| {
        settle.get("mode").and_then(serde_json::Value::as_str) == Some("content")
            && settle.get("settled").and_then(serde_json::Value::as_bool) == Some(true)
            && settle.get("frame_seq").and_then(serde_json::Value::as_u64)
                == observation_frame_seq
    });
    if honoured {
        return None;
    }
    let mut failure = StepExec::failed(Evaluated {
        result: AssertionResult {
            kind: "settle".to_string(),
            passed: false,
            expected: Some(serde_json::json!({"settle": "content"})),
            actual: Some(settle.clone().unwrap_or(serde_json::Value::Null)),
            diff: Some(AssertionDiff {
                expected: serde_json::json!({"settle": "content"}),
                actual: settle.clone().unwrap_or(serde_json::Value::Null),
                path: Some("step.settle".to_string()),
                detail: Some(
                    "the step asked for `settle: content` but the backend returned no settled \
                     content record for the frame it bound, so the content-stable rule was not \
                     applied to this observation"
                        .to_string(),
                ),
            }),
            matched_node_ids: Vec::new(),
            message: None,
        },
        error: Some(BackendError::Protocol {
            code: ErrorCode::UnsupportedAssertion,
            detail: format!(
                "{}: `settle: content` was requested but this backend supplied no content \
                 settle record",
                step.kind()
            ),
            hint: Some(
                "An unapplied settle rule never passes. Run the step on the ART engine \
                 (which implements `settle: content`), or drop the option and say what rule \
                 actually backs the observation."
                    .to_string(),
            ),
            details: Some(serde_json::json!({
                "reason": "content_settle_unhonored",
                "step_kind": step.kind(),
                "observation_frame_seq": observation_frame_seq,
                "settle": settle,
            })),
        }),
    });
    failure.observation = exec.observation.clone();
    Some(failure)
}

/// SE2: a pixel/frame assertion whose bound frame was only content-settled.
///
/// `settle: content` proves that the UI tree content (text, nodes, bounds,
/// state) stopped changing for the window. It says nothing about the pixels,
/// which may still be animating, so a screenshot comparison, a region pixel
/// check or any other frame-pixel claim backed only by content stability is a
/// failed binding. The structured failure names the reason
/// (`content_settle_pixel`) and the content record that bound the frame, so the
/// caller cannot mistake it for a pixel mismatch.
fn content_settle_pixel_failure(
    step: &Step,
    bound_settle: Option<&serde_json::Value>,
) -> Option<StepExec> {
    let settle = bound_settle?;
    if settle.get("mode").and_then(serde_json::Value::as_str) != Some("content") {
        return None;
    }
    if !pixel_assertion_step(step) {
        return None;
    }
    let detail = format!(
        "{}: the bound frame was settled by `settle: content` (UI tree content stable), which \
         is not pixel evidence - the pixels may still be animating",
        step.kind()
    );
    let mut exec = StepExec::failed(Evaluated {
        result: AssertionResult {
            kind: "frame_binding".to_string(),
            passed: false,
            expected: Some(serde_json::json!({"settle": "strict", "pixel_evidence": true})),
            actual: Some(serde_json::json!({"settle": "content", "pixel_evidence": false})),
            diff: Some(AssertionDiff {
                expected: serde_json::json!({"settle": "strict", "pixel_evidence": true}),
                actual: serde_json::json!({"settle": "content", "pixel_evidence": false}),
                path: Some("frame.settle".to_string()),
                detail: Some(
                    "content settle proves the tree's text/nodes/bounds/state stopped changing; \
                     it never proves the pixels stopped changing"
                        .to_string(),
                ),
            }),
            matched_node_ids: Vec::new(),
            message: None,
        },
        error: Some(BackendError::Protocol {
            code: ErrorCode::AssertionFailed,
            detail,
            hint: Some(
                "A pixel/frame assertion needs a pixel-settled frame: use `settle: strict` \
                 (the default) and let the screen stop animating, or assert on the UI tree \
                 (assert_text/assert_visible/assert_state) which content settle may back."
                    .to_string(),
            ),
            details: Some(serde_json::json!({
                "reason": "content_settle_pixel",
                "step_kind": step.kind(),
                "settle": settle,
                "pixel_evidence": false,
            })),
        }),
    });
    exec.observation = None;
    Some(exec)
}

/// SE2b/SE2c: a **geometry** claim bound only to a content-settled frame.
///
/// The content signature cannot hash `bounds`/`visible_bounds`: on the very
/// screen ADR-0026 decision 3 names, a Reanimated timeline translates four views
/// and their reported rects differ by 1 px from one fresh traversal to the next
/// while every content field is identical
/// (`.cache/se2b/probe-trees.json`). Geometry therefore animates with the
/// pixels, so an assertion that reads it needs a strict (geometry-settled)
/// frame.
///
/// **Both** places a step can name a rectangle are geometry claims, and both are
/// refused (SE2c): an assertion's own `bounds` (`assert_text`/`assert_visible`/
/// `assert_not_visible`) **and** `selector.bounds`, which
/// [`acos_protocol::ui::Selector::matches_node`] compares exactly. The selector
/// position is the dangerous one: on a still-drifting node the reported rect is
/// `0,101,…` instead of `0,100,…`, so the selector does not match, and
/// `assert_not_visible {selector: {text: "Error", bounds: [0,100,200,140]}}`
/// would report "node absent" — a pass — while the banner is on screen. The
/// same drift makes a *matching* selector-backed pass a single-frame
/// coincidence. `assert_state` has no `bounds` of its own but takes a selector,
/// so it is enumerated too.
///
/// Steps that *resolve* a target (`wait_for`, `tap`, `type`) are deliberately
/// NOT refused here: they compare their selector against a fresh traversal they
/// take themselves, not against the bound frame this guard is about (a content
/// input's own resolution is proven fresh by SE2c's fresh-resolution rule in the
/// engine).
fn content_settle_geometry_failure(
    step: &Step,
    bound_settle: Option<&serde_json::Value>,
) -> Option<StepExec> {
    let settle = bound_settle?;
    if settle.get("mode").and_then(serde_json::Value::as_str) != Some("content") {
        return None;
    }
    // Every assertion that takes a selector is enumerated; a new one must be
    // added here or its bounds claim would ride an un-hashed frame.
    let (claims_assertion_bounds, claims_selector_bounds) = match step.as_step() {
        Some(StepRef::AssertText(assertion)) => {
            (assertion.bounds.is_some(), assertion.selector.bounds.is_some())
        }
        Some(StepRef::AssertVisible(assertion) | StepRef::AssertNotVisible(assertion)) => {
            (assertion.bounds.is_some(), assertion.selector.bounds.is_some())
        }
        Some(StepRef::AssertState(assertion)) => (false, assertion.selector.bounds.is_some()),
        _ => (false, false),
    };
    if !(claims_assertion_bounds || claims_selector_bounds) {
        return None;
    }
    let detail = format!(
        "{}: the bound frame was settled by `settle: content` (UI tree content stable), which \
         is not geometry evidence - an animating view's reported bounds change every frame",
        step.kind()
    );
    let mut exec = StepExec::failed(Evaluated {
        result: AssertionResult {
            kind: "frame_binding".to_string(),
            passed: false,
            expected: Some(serde_json::json!({"settle": "strict", "geometry_evidence": true})),
            actual: Some(serde_json::json!({"settle": "content", "geometry_hashed": false})),
            diff: Some(AssertionDiff {
                expected: serde_json::json!({"settle": "strict", "geometry_evidence": true}),
                actual: serde_json::json!({"settle": "content", "geometry_hashed": false}),
                path: Some("frame.settle".to_string()),
                detail: Some(
                    "content settle proves the tree's text/nodes/state stopped changing; a \
                     translated view's bounds keep changing with the pixels"
                        .to_string(),
                ),
            }),
            matched_node_ids: Vec::new(),
            message: None,
        },
        error: Some(BackendError::Protocol {
            code: ErrorCode::AssertionFailed,
            detail,
            hint: Some(
                "A bounds claim needs a geometry-settled frame: use `settle: strict` (the \
                 default) and wait for the animation to stop. Assert text/state instead, which \
                 content settle may back. A `bounds` inside the selector is a geometry claim \
                 too - drop it (the text/id/class match is enough) or settle strictly."
                    .to_string(),
            ),
            details: Some(serde_json::json!({
                "reason": "content_settle_geometry",
                "step_kind": step.kind(),
                "settle": settle,
                "signature_geometry_fields": crate::settle::SIGNATURE_GEOMETRY_FIELDS,
                "geometry_evidence": false,
                // Which of the two rectangle positions made the claim.
                "bounds_claim": {
                    "assertion": claims_assertion_bounds,
                    "selector": claims_selector_bounds,
                },
            })),
        }),
    });
    exec.observation = None;
    Some(exec)
}

/// Steps that assert about the frame's pixels (never satisfied by a
/// content-settled frame alone).
fn pixel_assertion_step(step: &Step) -> bool {
    matches!(
        step.as_step(),
        Some(StepRef::AssertScreenshot(_) | StepRef::AssertRegion(_))
    )
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
            expected: Some(serde_json::json!({"frame_seq": bound})),
            actual: Some(serde_json::json!({"frame_seq": observation.frame.frame_seq})),
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
        // REL3E: the app window's own answer comes first. `handled: false` is the
        // framework saying *nothing consumed the press* (a label with no handler,
        // a disabled control), which is a different situation from "the app
        // handled it and nothing changed" - and the agent must be able to tell
        // them apart from this step's own evidence.
        if delivery.handled == Some(false) {
            let acks = delivery.acks.len();
            return Evaluated::pass(
                "input_delivery",
                Vec::new(),
                Some(format!(
                    "the app's window did not consume this tap (`finishInputEvent(handled) = \
                     false`, {acks} guest ack(s)); nothing was handled, so no state change is \
                     expected or claimed - the target/hit-test in the input evidence names what \
                     the point hit and, when the tree shows one, the clickable node to tap"
                )),
                serde_json::json!({"delivered": true, "handled": false}),
                serde_json::json!({
                    "delivered": delivery.delivered,
                    "handled": false,
                    "acks": acks,
                    "visual_change": delivery.visual_change,
                }),
            );
        }
        if delivery.delivered && !delivery.visual_change {
            let acks = delivery.acks.len();
            let handled = delivery.handled;
            return Evaluated::pass(
                "input_delivery",
                Vec::new(),
                Some(format!(
                    "host-observed input delivery ({acks} guest ack(s), handled: {handled:?}); \
                     no visual change; no state change claimed"
                )),
                serde_json::json!({"delivered": true}),
                serde_json::json!({
                    "delivered": delivery.delivered,
                    "handled": handled,
                    "acks": acks,
                    "visual_change": delivery.visual_change,
                }),
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
            serde_json::json!({"compared": false}),
            serde_json::json!({"previous_observation": false}),
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
        return Evaluated::pass(
            "state_change",
            Vec::new(),
            Some(message),
            serde_json::json!({"tree_changed": true, "screenshot_changed": true}),
            serde_json::json!({
                "tree_changed": change.tree_changed,
                "screenshot_changed": change.screenshot_changed,
            }),
        );
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
        settle: None,
        delivery: None,
        until: None,
        warnings: Vec::new(),
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
                return Evaluated::pass(
                    "screenshot",
                    Vec::new(),
                    Some(detail),
                    serde_json::json!({
                        "baseline": assertion.baseline,
                        "tolerance": assertion.tolerance,
                    }),
                    actual_value,
                );
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
            serde_json::json!({
                "criteria": checks,
                "display_region": [assertion.x, assertion.y, assertion.w, assertion.h],
                "display_origin": frame_meta.display_origin,
            }),
            actual_value,
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
///
/// A launch target the APK does not declare is a **scenario/setup error**, not
/// an app assertion: the app never ran, so the run cannot be evidence about the
/// app. The code is `INVALID_SCENARIO` (status `error`), never
/// `ASSERTION_FAILED`.
fn static_launch_assertion(
    manifest: &crate::backend::ManifestInfo,
    launch: &acos_protocol::LaunchStep,
) -> Option<StepExec> {
    if let Some(activity) = &launch.activity {
        let resolved = crate::backend::resolve_component(&manifest.package, activity);
        if !manifest.declares_activity(activity) {
            let expanded = resolved.clone().unwrap_or_else(|| activity.clone());
            let error = ProtocolError::new(
                ErrorCode::InvalidScenario,
                format!(
                    "launch activity `{activity}` resolves to `{expanded}`, which {} does not declare",
                    manifest.path
                ),
            )
            .with_hint(
                "Fix the scenario's `launch.activity` (`pkg/.Activity`, `pkg/Activity`, `.Activity` \
                 and a bare `Activity` are all accepted) or rebuild the APK. This is a scenario \
                 error: the app was never launched, so it is not an app verdict.",
            )
            .with_details(serde_json::json!({
                "declared_activities": manifest.activities,
                "requested": activity,
                "resolved": expanded,
                "launcher_activity": manifest.launcher_activity,
                "error_class": "invalid_scenario",
            }));
            return Some(StepExec {
                status: Status::Error,
                error: Some(error),
                assertions: Vec::new(),
                observation: None,
                produced_frame: None,
                session: None,
                settle: None,
                delivery: None,
                until: None,
                warnings: Vec::new(),
            });
        }
    } else if launch.intent_action.is_none() && manifest.launcher_activity.is_none() {
        let error = ProtocolError::new(
            ErrorCode::InvalidScenario,
            "scenario launches without an activity and the APK has no launcher activity"
                .to_string(),
        )
        .with_hint(
            "Name the activity in the scenario's `launch.activity`, or launch the APK by its \
             launcher (declare MAIN/LAUNCHER on an <activity> or <activity-alias>).",
        );
        return Some(StepExec {
            status: Status::Error,
            error: Some(error),
            assertions: Vec::new(),
            observation: None,
            produced_frame: None,
            session: None,
            settle: None,
            delivery: None,
            until: None,
            warnings: Vec::new(),
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
            settle: None,
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
            settle: None,
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

    /// REL3E: a tap step's JSON says `expect_change: "auto"` explicitly (the
    /// step's own state-change rule runs whether or not the scenario asked), it
    /// carries the app window's `handled` answer and the target block, and a tap
    /// the app never handled passes with that fact instead of a claim.
    #[test]
    fn rel3e_tap_step_reports_handled_target_and_expect_change_auto() {
        const HEAD: &str = "version: 1\nname: rel3e\napp: dev.acos.fixtures.taptargets\n\
             isolation: process\ndisplay: {mode: offscreen, width: 100, height: 100, \
             density_dpi: 420}\n";
        let scenario = Scenario::from_yaml_str(&format!(
            "{HEAD}steps:\n  - tap:\n      selector: {{id: title}}\n"
        ))
        .expect("a one-tap scenario");
        let step = scenario.steps.first().expect("the tap step");
        assert!(matches!(step.as_step(), Some(StepRef::Tap(_))));

        let previous = observation(&[[0, 0, 0, 255]], 1, 1);
        let mut current = observation(&[[0, 0, 0, 255]], 1, 1);
        current.input = Some(InputDelivery {
            delivered: true,
            visual_change: false,
            handled: Some(false),
            published: 2,
            acks: vec![
                crate::backend::InputAck {
                    seq: 1,
                    handled: false,
                    latency_us: Some(900),
                },
                crate::backend::InputAck {
                    seq: 2,
                    handled: false,
                    latency_us: Some(950),
                },
            ],
            target: Some(serde_json::json!({
                "selector": {"id": "title"},
                "hit": {"node": {"class": "android.widget.TextView"}, "suggest": {"class": "android.widget.Switch"}},
            })),
            settle: None,
        });
        let input =
            step_input(step, current.input.as_ref(), None).expect("a tap step always has input evidence");
        assert_eq!(input["handled"], serde_json::json!(false));
        assert_eq!(input["expect_change"], serde_json::json!("auto"));
        assert_eq!(input["handled_evidence"]["source"], serde_json::json!("finishInputEvent"));
        assert_eq!(input["handled_evidence"]["published"], serde_json::json!(2));
        assert_eq!(input["handled_evidence"]["acked"], serde_json::json!(2));
        assert_eq!(
            input["handled_evidence"]["unhandled"],
            serde_json::json!([1, 2]),
            "the unhandled event seqs are named"
        );
        assert_eq!(input["target"]["selector"]["id"], serde_json::json!("title"));

        // The finding must not turn "nothing consumed it" into a failure, and it
        // must not claim a state change either.
        let evaluated = eval_tap_change(Some(&previous), &current, Capabilities::default());
        assert!(evaluated.result.passed, "{:?}", evaluated.result);
        assert_eq!(evaluated.result.kind, "input_delivery");
        let message = evaluated.result.message.unwrap_or_default();
        assert!(message.contains("did not consume"), "{message}");
        let actual = evaluated.result.actual.expect("the measured facts");
        assert_eq!(actual["delivered"], serde_json::json!(true));
        assert_eq!(actual["handled"], serde_json::json!(false));
        assert_eq!(actual["visual_change"], serde_json::json!(false));
        assert_eq!(actual["acks"], serde_json::json!(2));

        // A step that explicitly asked for a change says `true`, not "auto".
        let flagged = Scenario::from_yaml_str(&format!(
            "{HEAD}steps:\n  - tap:\n      selector: {{id: title}}\n      \
             expect_change: true\n"
        ))
        .expect("a flagged tap scenario");
        let input = step_input(flagged.steps.first().expect("the tap step"), current.input.as_ref(), None)
            .expect("input evidence");
        assert_eq!(input["expect_change"], serde_json::json!(true));

        // No delivery at all is "unknown", never "not handled".
        current.input = None;
        let input = step_input(step, current.input.as_ref(), None).expect("input evidence");
        assert_eq!(input["handled"], serde_json::Value::Null);
        assert_eq!(input["handled_evidence"]["source"], serde_json::json!("none"));
    }

    // ---- REL3H2: an unhandled input is never a silent plain pass -----------

    fn unhandled_delivery(handled: Option<bool>) -> InputDelivery {
        InputDelivery {
            delivered: true,
            visual_change: false,
            handled,
            published: 2,
            acks: vec![
                crate::backend::InputAck {
                    seq: 1,
                    handled: handled.unwrap_or(false),
                    latency_us: Some(900),
                },
                crate::backend::InputAck {
                    seq: 2,
                    handled: handled.unwrap_or(false),
                    latency_us: Some(950),
                },
            ],
            target: Some(serde_json::json!({
                "selector": {"id": "title"},
                "hit": {"node": {"class": "android.widget.TextView"},
                        "suggest": {"class": "android.widget.Switch"}},
            })),
            settle: Some(serde_json::json!({
                "mode": "strict",
                "settled": true,
                "reason": "unhandled_input",
                "bound": "unhandled-input",
                "pixels_changed": false,
            })),
        }
    }

    fn tap_exec(delivery: Option<InputDelivery>) -> StepExec {
        let mut exec = StepExec::pass();
        exec.delivery = delivery;
        exec
    }

    /// The default (no `expect_handled`): the step is still a pass, and it says
    /// so with a structured `INPUT_UNHANDLED` warning that names the step, the
    /// acks and the clickable neighbour - in the step JSON and, via the run
    /// loop, in `provenance.warnings[]`.
    #[test]
    fn rel3h2_an_unhandled_pass_carries_input_unhandled() {
        const HEAD: &str = "version: 1\nname: rel3h2\napp: dev.acos.fixtures.taptargets\n\
             isolation: process\ndisplay: {mode: offscreen, width: 100, height: 100, \
             density_dpi: 420}\n";
        let scenario = Scenario::from_yaml_str(&format!(
            "{HEAD}steps:\n  - tap:\n      selector: {{id: title}}\n"
        ))
        .expect("a one-tap scenario");
        let step = scenario.steps.first().expect("the tap step");
        let exec = tap_exec(Some(unhandled_delivery(Some(false))));

        let warning = input_unhandled_warning(step, 0, &exec).expect("an unhandled pass warns");
        assert_eq!(
            warning["code"],
            serde_json::json!(acos_protocol::provenance::WARN_INPUT_UNHANDLED)
        );
        assert_eq!(warning["step"], serde_json::json!(0));
        assert_eq!(warning["step_kind"], serde_json::json!("tap"));
        assert_eq!(warning["handled"], serde_json::json!(false));
        assert_eq!(warning["published"], serde_json::json!(2));
        assert_eq!(warning["acked"], serde_json::json!(2));
        assert_eq!(warning["unhandled"], serde_json::json!([1, 2]));
        let message = warning["message"].as_str().unwrap_or_default();
        assert!(message.contains("nothing consumed it"), "{message}");
        // REL3E's neighbour hint is reused, not re-invented.
        assert!(
            warning["hint"].as_str().unwrap_or_default().contains("Switch"),
            "the hint names the clickable sibling: {warning:#?}"
        );
        // The step's own status is untouched: a warning is a fact, not a verdict.
        assert_eq!(exec.status, Status::Pass);
    }

    /// A handled input, an unknown ack and a step with no delivery never warn -
    /// the warning is the app's own answer, never a guess.
    #[test]
    fn rel3h2_only_a_proven_unhandled_pass_warns() {
        const HEAD: &str = "version: 1\nname: rel3h2\napp: dev.acos.fixtures.taptargets\n\
             isolation: process\ndisplay: {mode: offscreen, width: 100, height: 100, \
             density_dpi: 420}\n";
        let scenario = Scenario::from_yaml_str(&format!(
            "{HEAD}steps:\n  - tap:\n      selector: {{id: inc}}\n"
        ))
        .expect("a one-tap scenario");
        let step = scenario.steps.first().expect("the tap step");
        assert!(input_unhandled_warning(step, 0, &tap_exec(Some(unhandled_delivery(Some(true))))).is_none());
        assert!(input_unhandled_warning(step, 0, &tap_exec(Some(unhandled_delivery(None)))).is_none());
        assert!(input_unhandled_warning(step, 0, &tap_exec(None)).is_none());

        // A step that failed for its own reason keeps that error; the warning
        // would only obscure it.
        let mut failed = tap_exec(Some(unhandled_delivery(Some(false))));
        failed.status = Status::Timeout;
        assert!(input_unhandled_warning(step, 0, &failed).is_none());
    }

    /// `expect_handled: false` is the scenario's "this no-op press is intended":
    /// it silences the warning and changes nothing else.
    #[test]
    fn rel3h2_expect_handled_false_silences_the_warning() {
        const HEAD: &str = "version: 1\nname: rel3h2\napp: dev.acos.fixtures.taptargets\n\
             isolation: process\ndisplay: {mode: offscreen, width: 100, height: 100, \
             density_dpi: 420}\n";
        let scenario = Scenario::from_yaml_str(&format!(
            "{HEAD}steps:\n  - tap:\n      selector: {{id: title}}\n      expect_handled: false\n"
        ))
        .expect("a one-tap scenario");
        let step = scenario.steps.first().expect("the tap step");
        let exec = tap_exec(Some(unhandled_delivery(Some(false))));
        assert!(
            input_unhandled_warning(step, 0, &exec).is_none(),
            "expect_handled: false says the no-op is intended"
        );
        assert!(expect_handled_failure(step, &exec).is_none(), "and it is not a claim");
    }

    /// `expect_handled: true` makes the app's handling a hard requirement: the
    /// same unhandled delivery is now a step failure with `INPUT_NOT_HANDLED`
    /// (error class `assertion`), naming the injected target and the settle
    /// record. A handled delivery passes the claim.
    #[test]
    fn rel3h2_expect_handled_true_fails_with_input_not_handled() {
        const HEAD: &str = "version: 1\nname: rel3h2\napp: dev.acos.fixtures.taptargets\n\
             isolation: process\ndisplay: {mode: offscreen, width: 100, height: 100, \
             density_dpi: 420}\n";
        let scenario = Scenario::from_yaml_str(&format!(
            "{HEAD}steps:\n  - tap:\n      selector: {{id: title}}\n      expect_handled: true\n"
        ))
        .expect("a one-tap scenario");
        let step = scenario.steps.first().expect("the tap step");

        let failed = expect_handled_failure(step, &tap_exec(Some(unhandled_delivery(Some(false)))))
            .expect("an unhandled gesture fails the claim");
        assert_eq!(failed.status, Status::Fail);
        let error = failed.error.expect("a structured error");
        assert_eq!(error.code, ErrorCode::InputNotHandled);
        assert_eq!(
            error.code.class(),
            acos_protocol::error::ErrorClass::Assertion
        );
        let details = error.details.expect("details");
        assert_eq!(details["reason"], serde_json::json!("expect_handled"));
        assert_eq!(details["handled"], serde_json::json!(false));
        assert_eq!(details["published"], serde_json::json!(2));
        assert_eq!(details["acked"], serde_json::json!(2));
        assert_eq!(details["target"]["selector"]["id"], serde_json::json!("title"));
        assert_eq!(
            details["settle"]["reason"],
            serde_json::json!("unhandled_input"),
            "the settle record travels with the failure"
        );
        // The failing step's frame is still evidence.
        assert!(failed.delivery.is_some());

        // Handled: the claim holds and nothing is reported.
        assert!(expect_handled_failure(step, &tap_exec(Some(unhandled_delivery(Some(true)))))
            .is_none());

        // Unknown (no complete ack): the claim cannot be verified - unsupported,
        // never a silent pass.
        let unverified = expect_handled_failure(step, &tap_exec(Some(unhandled_delivery(None))))
            .expect("an unverifiable claim never passes");
        assert_eq!(
            unverified.error.as_ref().map(|error| error.code),
            Some(ErrorCode::UnsupportedAssertion)
        );
    }

    #[test]
    fn delivered_but_visually_unchanged_input_is_not_a_state_change() {
        let previous = observation(&[[0, 0, 0, 255]], 1, 1);
        let mut current = observation(&[[0, 0, 0, 255]], 1, 1);
        current.input = Some(InputDelivery {
            delivered: true,
            visual_change: false,
            handled: None,
            published: 0,
            acks: Vec::new(),
            target: None,
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

/// F5: the launch gate must accept every Android component-name spelling and
/// must report a bad target as a scenario error, never as an app assertion.
#[cfg(test)]
mod launch_gate_tests {
    use super::*;
    use crate::backend::ManifestInfo;
    use acos_protocol::scenario::LaunchStep;

    fn gaten_manifest() -> ManifestInfo {
        // The real Gaten shape: the manifest package is `com.gaten` and the
        // activities live under `com.gaten.app`, which is why the old
        // "package part must equal the manifest package" rule rejected
        // `com.gaten.app/.MainActivity`.
        ManifestInfo {
            path: "gaten.apk".to_string(),
            sha256: "0".repeat(64),
            package: "com.gaten".to_string(),
            version_code: 10,
            version_name: "0.1.0".to_string(),
            min_sdk: 29,
            target_sdk: 36,
            label: Some("Gaten".to_string()),
            launcher_activity: Some("com.gaten.app.LauncherIconLight".to_string()),
            activities: vec![
                "com.gaten.app.MainActivity".to_string(),
                "com.gaten.app.LauncherActivity".to_string(),
                "com.gaten.app.LauncherIconLight".to_string(),
            ],
            native_abis: vec!["x86_64".to_string()],
            debuggable: false,
        }
    }

    fn launch(activity: &str) -> LaunchStep {
        LaunchStep {
            activity: Some(activity.to_string()),
            intent_action: None,
            data: None,
            extras: Default::default(),
            clear_state: true,
            settle_timeout_ms: None,
            settle: None,
        }
    }

    #[test]
    fn documented_component_shorthand_passes_the_gate() {
        let manifest = gaten_manifest();
        for activity in [
            "com.gaten.app/.MainActivity",
            "com.gaten.app/MainActivity",
            "com.gaten.app/com.gaten.app.MainActivity",
            "com.gaten.app.MainActivity",
        ] {
            assert!(
                static_launch_assertion(&manifest, &launch(activity)).is_none(),
                "{activity} must pass the launch gate"
            );
        }
    }

    #[test]
    fn undeclared_activity_is_an_invalid_scenario_not_an_assertion() {
        let manifest = gaten_manifest();
        let failure = static_launch_assertion(&manifest, &launch("com.gaten.app/.Nope"))
            .expect("an undeclared target must fail the gate");
        assert_eq!(failure.status, Status::Error, "not an app verdict");
        let error = failure.error.expect("error");
        assert_eq!(error.code, ErrorCode::InvalidScenario);
        assert_ne!(error.code, ErrorCode::AssertionFailed);
        assert_eq!(error.status(), Status::Error);
        let details = error.details.expect("details");
        assert_eq!(details["resolved"], "com.gaten.app.Nope");
        assert_eq!(details["requested"], "com.gaten.app/.Nope");
        assert_eq!(details["error_class"], "invalid_scenario");
        assert!(details["declared_activities"]
            .as_array()
            .expect("declared_activities")
            .iter()
            .any(|v| v == "com.gaten.app.MainActivity"));
        assert!(
            error.message.contains("resolves to `com.gaten.app.Nope`"),
            "the message must show the expansion: {}",
            error.message
        );
    }

    #[test]
    fn a_scenario_without_an_activity_and_no_launcher_is_an_invalid_scenario() {
        let mut manifest = gaten_manifest();
        manifest.launcher_activity = None;
        let step = LaunchStep {
            activity: None,
            intent_action: None,
            data: None,
            extras: Default::default(),
            clear_state: false,
            settle_timeout_ms: None,
            settle: None,
        };
        let failure = static_launch_assertion(&manifest, &step).expect("must fail");
        assert_eq!(failure.status, Status::Error);
        let error = failure.error.expect("error");
        assert_eq!(error.code, ErrorCode::InvalidScenario);
    }
}

#[cfg(test)]
mod content_settle_tests {
    use super::*;
    use acos_protocol::scenario::Scenario;

    fn scenario(yaml: &str) -> Scenario {
        Scenario::from_yaml_str(yaml).expect("scenario parses")
    }

    fn content_record() -> serde_json::Value {
        content_record_for(5)
    }

    /// SE2b: a content record always names the frame it settled.
    fn content_record_for(frame_seq: u64) -> serde_json::Value {
        serde_json::json!({
            "mode": "content",
            "settled": true,
            "window_ms": 300,
            "traversals": 3,
            "signature": "0f0f0f0f0f0f0f0f",
            "frame_seq": frame_seq,
            "pixel_evidence": false,
        })
    }

    fn content_observation(frame_seq: u64, settle: serde_json::Value) -> SessionObservation {
        use acos_protocol::ui::UiTree;
        let mut tree = UiTree::new(
            acos_protocol::FrameMeta::new(
                frame_seq,
                "2026-01-01T00:00:00Z",
                "test",
                100,
                100,
                420,
                "h",
                true,
            ),
            "dev.acos.fixtures.counter",
            Vec::new(),
        );
        tree.frame.frame_seq = frame_seq;
        SessionObservation {
            session_id: "s_test".to_string(),
            package: "dev.acos.fixtures.counter".to_string(),
            frame: tree.frame.clone(),
            tree,
            screenshot_png: None,
            crashed: None,
            input: None,
            settle: Some(settle),
        }
    }

    fn strict_record() -> serde_json::Value {
        serde_json::json!({"mode": "strict", "settled": true})
    }

    const HEADER: &str = "version: 1\nname: se2\napp: dev.acos.fixtures.counter\nisolation: process\ndisplay: {mode: offscreen, width: 100, height: 100, density_dpi: 420}\nsteps:\n  - launch: {activity: .MainActivity}\n";

    /// SE2: a pixel/frame assertion bound to a content-settled frame must fail
    /// closed with the structured reason, never compare pixels.
    #[test]
    fn pixel_assertion_backed_by_content_settle_fails_closed() {
        let parsed = scenario(&format!(
            "{HEADER}  - assert_region: {{x: 0, y: 0, w: 10, h: 10, min_distinct_colors: 1}}\n"
        ));
        let step = &parsed.steps[1];
        let failure = content_settle_pixel_failure(step, Some(&content_record()))
            .expect("a content-bound pixel assertion is a failed binding");
        assert_eq!(failure.status, Status::Fail);
        let error = failure.error.as_ref().expect("structured error");
        assert_eq!(error.code, ErrorCode::AssertionFailed);
        let details = error.details.as_ref().expect("details");
        assert_eq!(details["reason"], "content_settle_pixel");
        assert_eq!(details["pixel_evidence"], false);
        assert_eq!(failure.assertions[0].kind, "frame_binding");
        assert!(!failure.assertions[0].passed);

        // A screenshot comparison is the same pixel claim.
        let parsed = scenario(&format!(
            "{HEADER}  - assert_screenshot: {{baseline: baseline.png, tolerance: 0.0}}\n"
        ));
        let failure = content_settle_pixel_failure(&parsed.steps[1], Some(&content_record()))
            .expect("a content-bound screenshot comparison is a failed binding");
        assert_eq!(failure.error.as_ref().unwrap().code, ErrorCode::AssertionFailed);
    }

    /// SE2b: a bounds claim on a content-settled frame fails closed, exactly
    /// like a pixel claim. Before the fix the signature omitted every geometry
    /// field an assertion could read, so `assert_text {bounds: ...}` passed on a
    /// frame whose views were still being translated.
    #[test]
    fn geometry_claims_are_refused_on_content_settle() {
        let parsed = scenario(&format!(
            "{HEADER}  - assert_text: {{selector: {{id: title}}, text: {{contains: Welcome}}, bounds: [0, 0, 10, 10]}}\n"
        ));
        let step = &parsed.steps[1];
        let failure = content_settle_geometry_failure(step, Some(&content_record()))
            .expect("a content-bound bounds claim is a failed binding");
        assert_eq!(failure.status, Status::Fail);
        let error = failure.error.as_ref().expect("structured error");
        assert_eq!(error.code, ErrorCode::AssertionFailed);
        let details = error.details.as_ref().expect("details");
        assert_eq!(details["reason"], "content_settle_geometry");
        assert_eq!(details["geometry_evidence"], false);
        assert_eq!(failure.assertions[0].kind, "frame_binding");
        // Without a bounds claim the same assertion is fine, and a strict frame
        // may back the bounds claim.
        let parsed = scenario(&format!(
            "{HEADER}  - assert_text: {{selector: {{id: title}}, text: {{contains: Welcome}}}}\n"
        ));
        assert!(content_settle_geometry_failure(&parsed.steps[1], Some(&content_record())).is_none());
        let parsed = scenario(&format!(
            "{HEADER}  - assert_visible: {{selector: {{id: title}}, bounds: [0, 0, 10, 10]}}\n"
        ));
        assert!(
            content_settle_geometry_failure(&parsed.steps[1], Some(&strict_record())).is_none(),
            "a strict settle may back a bounds claim"
        );
        assert!(content_settle_geometry_failure(&parsed.steps[1], Some(&content_record())).is_some());
    }

    /// SE2c: `selector.bounds` is a geometry claim on every assertion kind.
    ///
    /// `Selector::matches_node` compares `selector.bounds` exactly, so a
    /// selector carrying a rectangle reads the same un-hashed geometry an
    /// assertion's own `bounds` does. The false-pass this closes: on a
    /// still-drifting node the reported rect is `0,101,...` instead of
    /// `0,100,...`, so
    /// `assert_not_visible {selector: {text: "Error", bounds: [...]}}` sees "no
    /// match" and passes while the banner is on screen.
    #[test]
    fn selector_bounds_is_a_geometry_claim_on_every_assertion_kind() {
        // One case per assertion kind that takes a selector.
        for yaml in [
            "  - assert_text: {selector: {text: {contains: Welcome}, bounds: [0, 100, 200, 140]}, \
             text: {contains: Welcome}}\n",
            "  - assert_visible: {selector: {text: {contains: Welcome}, bounds: [0, 100, 200, 140]}}\n",
            "  - assert_not_visible: {selector: {text: {contains: Error}, bounds: [0, 100, 200, 140]}}\n",
            "  - assert_state: {selector: {id: title, bounds: [0, 100, 200, 140]}, visible: true}\n",
        ] {
            let parsed = scenario(&format!("{HEADER}{yaml}"));
            let step = &parsed.steps[1];
            let failure = content_settle_geometry_failure(step, Some(&content_record()))
                .unwrap_or_else(|| {
                    panic!("selector.bounds must be refused on {}: {yaml}", step.kind())
                });
            assert_eq!(failure.status, Status::Fail);
            let error = failure.error.as_ref().expect("structured error");
            assert_eq!(error.code, ErrorCode::AssertionFailed);
            let details = error.details.as_ref().expect("details");
            assert_eq!(details["reason"], "content_settle_geometry");
            assert_eq!(details["geometry_evidence"], false);
            assert_eq!(details["bounds_claim"]["assertion"], false);
            assert_eq!(details["bounds_claim"]["selector"], true);
            // A strict-settled frame may back the same claim.
            assert!(
                content_settle_geometry_failure(step, Some(&strict_record())).is_none(),
                "a strict settle may back a selector bounds claim on {}",
                step.kind()
            );
            // No settle record at all changes nothing.
            assert!(content_settle_geometry_failure(step, None).is_none());
        }
        // The *absence* of a rectangle in either position is still allowed: the
        // guard must not refuse ordinary content-settled assertions.
        for yaml in [
            "  - assert_text: {selector: {text: {contains: Welcome}}, text: {contains: Welcome}}\n",
            "  - assert_visible: {selector: {text: {contains: Welcome}}}\n",
            "  - assert_not_visible: {selector: {text: {contains: Error}}}\n",
            "  - assert_state: {selector: {id: title}, visible: true}\n",
        ] {
            let parsed = scenario(&format!("{HEADER}{yaml}"));
            assert!(
                content_settle_geometry_failure(&parsed.steps[1], Some(&content_record())).is_none(),
                "an assertion without a rectangle must stay evaluable: {yaml}"
            );
        }
        // The assertion's own `bounds` still reports itself as the claim.
        let parsed = scenario(&format!(
            "{HEADER}  - assert_text: {{selector: {{id: title, bounds: [0, 0, 10, 10]}}, text: {{contains: Welcome}}, bounds: [0, 0, 10, 10]}}\n"
        ));
        let failure = content_settle_geometry_failure(&parsed.steps[1], Some(&content_record()))
            .expect("both rectangle positions are refused");
        let details = failure
            .error
            .as_ref()
            .and_then(|error| error.details.as_ref())
            .expect("details");
        assert_eq!(details["bounds_claim"]["assertion"], true);
        assert_eq!(details["bounds_claim"]["selector"], true);
    }

    /// SE2: a UI/text assertion may be backed by content settle.
    #[test]
    fn text_assertions_are_allowed_on_content_settle() {
        let parsed = scenario(&format!(
            "{HEADER}  - assert_text: {{selector: {{id: title}}, text: {{contains: Welcome}}}}\n"
        ));
        assert!(
            content_settle_pixel_failure(&parsed.steps[1], Some(&content_record())).is_none(),
            "a text assertion must be evaluable on a content-stable tree"
        );
        // A strict-settled bound frame is unaffected as well.
        assert!(content_settle_pixel_failure(&parsed.steps[1], Some(&strict_record())).is_none());
        // No settle record: unchanged behaviour (nothing new is refused).
        assert!(content_settle_pixel_failure(&parsed.steps[1], None).is_none());
    }

    /// SE2: `fast` still never backs a pass. The fast guard is unchanged and
    /// independent of the content guard.
    #[test]
    fn fast_still_never_backs_a_pass() {
        let failure = fast_bound_failure("assert_text");
        assert_eq!(failure.status, Status::Fail);
        assert_eq!(
            failure.error.as_ref().unwrap().code,
            ErrorCode::AssertionFailed
        );
        // And a pixel assertion on a fast-bound frame is still refused by the
        // existing fast guard (`fast` never backs a pass).
        let parsed = scenario(&format!(
            "{HEADER}  - assert_region: {{x: 0, y: 0, w: 10, h: 10, min_distinct_colors: 1}}\n"
        ));
        let fast = fast_bound_failure(parsed.steps[1].kind());
        assert_eq!(fast.error.as_ref().unwrap().code, ErrorCode::AssertionFailed);
    }

    /// SE2: a step that asks for content settle and gets no content record
    /// fails closed (no backend can silently downgrade the rule).
    #[test]
    fn a_content_step_without_a_content_record_fails_closed() {
        let parsed = scenario(&format!(
            "{HEADER}  - tap: {{x: 1, y: 2, settle: content}}\n"
        ));
        let step = &parsed.steps[1];
        let mut exec = StepExec::pass();
        exec.settle = Some(serde_json::json!({"mode": "strict", "settled": true}));
        let failure = content_settle_unhonored(step, &exec).expect("downgrade is refused");
        assert_eq!(failure.status, Status::Unsupported);
        assert_eq!(
            failure.error.as_ref().unwrap().code,
            ErrorCode::UnsupportedAssertion
        );
        // SE2b: a mode string is not enough - the record must be a *settled*
        // one that names the very frame the step observed.
        exec.settle = Some(content_record());
        assert!(
            content_settle_unhonored(step, &exec).is_some(),
            "a record with no observation frame identity never honours the step"
        );
        exec.settle = None;
        exec.observation = Some(content_observation(5, content_record_for(5)));
        assert!(content_settle_unhonored(step, &exec).is_none());
    }

    /// SE2b finding 4: the content record must be settled AND bound to the
    /// observation's own frame. A mode string from a stale record (or an
    /// unsettled one) is a downgrade, not a pass.
    #[test]
    fn content_settle_requires_the_settled_frame_identity() {
        let parsed = scenario(&format!("{HEADER}  - tap: {{x: 1, y: 2, settle: content}}\n"));
        let step = &parsed.steps[1];
        let mut exec = StepExec::pass();
        // Record for another traversal: refused.
        exec.observation = Some(content_observation(6, content_record_for(5)));
        let failure = content_settle_unhonored(step, &exec).expect("another frame is not evidence");
        assert_eq!(
            failure.error.as_ref().unwrap().code,
            ErrorCode::UnsupportedAssertion
        );
        let details = failure.error.as_ref().unwrap().details.clone().unwrap();
        assert_eq!(details["reason"], "content_settle_unhonored");
        assert_eq!(details["observation_frame_seq"], 6);
        // An unsettled content record: refused.
        let mut unsettled = content_record_for(6);
        unsettled["settled"] = serde_json::json!(false);
        exec.observation = Some(content_observation(6, unsettled));
        assert!(content_settle_unhonored(step, &exec).is_some());
        // No record at all: refused.
        exec.observation = Some(content_observation(6, strict_record()));
        assert!(content_settle_unhonored(step, &exec).is_some());
        // The settled record for exactly this frame: honoured.
        exec.observation = Some(content_observation(6, content_record_for(6)));
        assert!(content_settle_unhonored(step, &exec).is_none());
    }

    /// SE2b finding 8: step evidence publishes the *action's* settle record, not
    /// the session's sticky bound record from an earlier traversal/action.
    #[test]
    fn step_evidence_prefers_the_per_action_settle() {
        use crate::backend::InputDelivery;
        let mut observation = content_observation(6, strict_record());
        observation.input = Some(InputDelivery {
            delivered: true,
            visual_change: true,
            handled: None,
            published: 0,
            acks: Vec::new(),
            target: None,
            settle: Some(content_record_for(6)),
        });
        let settle = step_settle(Some(&observation), None, 1).expect("a record");
        assert_eq!(settle["mode"], "content");
        // Only the sticky record: it is used, but it is the sticky one.
        observation.input = None;
        let settle = step_settle(Some(&observation), None, 1).expect("a record");
        assert_eq!(settle["mode"], "strict");
        // No observation at all: the step's own record still travels.
        let settle = step_settle(None, Some(&content_record_for(6)), 1).expect("a record");
        assert_eq!(settle["mode"], "content");
    }

    /// SE2b finding 9: a pinned negative expectation is enforced. A negative
    /// that fails with another code/reason is a harness error, never green.
    #[test]
    fn a_negative_failure_must_match_its_pinned_error() {
        let expected = ExpectedError {
            code: "ASSERTION_FAILED".to_string(),
            reason: Some("content_settle_pixel".to_string()),
        };
        let failure = |code: ErrorCode, reason: Option<&str>| {
            let error = ProtocolError::new(code, "boom".to_string()).with_details(
                serde_json::json!({"reason": reason.unwrap_or("other")}),
            );
            FailureClassification {
                code,
                step_index: Some(2),
                summary: "boom".to_string(),
                error,
            }
        };
        // Matching code + reason: the control is accepted (status untouched).
        let mut status = Status::Fail;
        let mut actual = Some(failure(ErrorCode::AssertionFailed, Some("content_settle_pixel")));
        let mut notes = Vec::new();
        apply_expectation(Expectation::Fail, Some(&expected), &mut status, &mut actual, &mut notes);
        assert_eq!(status, Status::Fail);
        assert!(
            notes.iter().any(|note| note.contains("exactly as declared")),
            "{notes:?}"
        );
        // Right code, wrong reason: harness error.
        let mut status = Status::Fail;
        let mut actual = Some(failure(ErrorCode::AssertionFailed, Some("something_else")));
        let mut notes = Vec::new();
        apply_expectation(Expectation::Fail, Some(&expected), &mut status, &mut actual, &mut notes);
        assert_eq!(status, Status::Error);
        let actual = actual.expect("failure");
        assert_eq!(actual.code, ErrorCode::Internal);
        assert_eq!(
            actual.error.details.as_ref().unwrap()["reason"],
            "negative_expectation_mismatch"
        );
        // A TIMEOUT control pins the timeout code and its reason.
        let expected = ExpectedError {
            code: "TIMEOUT".to_string(),
            reason: Some("content-not-stable".to_string()),
        };
        let mut status = Status::Timeout;
        let mut actual = Some(failure(ErrorCode::Timeout, Some("content-not-stable")));
        let mut notes = Vec::new();
        apply_expectation(
            Expectation::NonPass,
            Some(&expected),
            &mut status,
            &mut actual,
            &mut notes,
        );
        assert_eq!(status, Status::Timeout);
        // A scenario that declares both `pass` and a pinned error is invalid.
        let parsed = Scenario::from_yaml_str_unvalidated(&format!(
            "{HEADER}expect_error: {{code: ASSERTION_FAILED}}\n"
        ))
        .expect("parses");
        let report = parsed.validate();
        assert!(
            report.errors.iter().any(|error| error.path.contains("expect_error")),
            "expect_error on a pass scenario must be INVALID_SCENARIO: {report:?}"
        );
    }

    /// SE2: `expect_change` (a pixel claim) cannot combine with content settle.
    #[test]
    fn expect_change_with_content_settle_is_invalid() {
        let yaml = format!(
            "{HEADER}  - tap: {{x: 1, y: 2, settle: content, expect_change: true}}\n"
        );
        let parsed = Scenario::from_yaml_str_unvalidated(&yaml).expect("parses");
        let report = parsed.validate();
        assert!(
            report.errors.iter().any(|error| error.path.contains("expect_change")),
            "content settle + expect_change must be INVALID_SCENARIO: {report:?}"
        );
    }

    #[test]
    fn content_settle_parses_on_inputs_and_launch() {
        let yaml = "version: 1\nname: se2\napp: dev.acos.fixtures.counter\nisolation: process\ndisplay: {mode: offscreen, width: 100, height: 100, density_dpi: 420}\nsteps:\n  - launch: {activity: .MainActivity, settle: content}\n  - tap: {x: 1, y: 2, settle: content}\n  - type: {text: hi, settle: content}\n  - key: {key: enter, settle: content}\n";
        let parsed = scenario(yaml);
        assert_eq!(parsed.validate().errors, Vec::new());
        assert!(matches!(
            parsed.steps[0].launch.as_ref().unwrap().settle,
            Some(StepSettle::Content)
        ));
        assert!(matches!(
            parsed.steps[1].tap.as_ref().unwrap().settle,
            Some(StepSettle::Content)
        ));
    }
}

/// REL3A: content stability as a first-class settle mode for **every** wait,
/// assertion and action, and the "a settle that timed out is never a pass" rule.
///
/// These tests drive `run_scenario` through a scripted backend that can be in the
/// one state the v0.3.0 feedback kept hitting: a screen whose pixels never settle
/// (an endless animation) while its tree content is stable. Before REL3A a
/// `wait_for`/`assert_text` on that screen could only time out (F-07) and an
/// `assert_text` with `timeout_ms` returned ~1 ms later (F-13).
#[cfg(test)]
mod rel3a_settle_tests {
    use super::*;
    use acos_protocol::scenario::Scenario;
    use acos_protocol::ui::{Bounds, UiNode, UiTree};
    use acos_protocol::{Backend as ProtocolBackend, FrameMeta};

    /// A screen that animates forever in pixels, with a stable tree.
    struct AnimatingBackend {
        /// `true` = the pixels never stop (strict settle can never succeed).
        animating: bool,
        /// Requested observation settle modes, in call order.
        modes: Vec<&'static str>,
        /// The mode requested for the *next* observation.
        next_mode: ObservationSettle,
        /// Refreshes served before the awaited text appears (F-13's race).
        text_missing_polls: usize,
        /// How many refreshes have run.
        polls: usize,
        /// How many content-settled refreshes have run.
        content_polls: usize,
        /// REL3A §2: when set, only the *first* content-settled observation
        /// satisfies the text condition; every later one is content-stable but
        /// no longer holds it. A single-sample content pass returns here.
        flip_after_first_content: bool,
        frame_seq: u64,
        cached: Option<SessionObservation>,
    }

    impl AnimatingBackend {
        fn new(animating: bool) -> Self {
            let mut backend = Self {
                animating,
                modes: Vec::new(),
                next_mode: ObservationSettle::Strict,
                text_missing_polls: 0,
                polls: 0,
                content_polls: 0,
                flip_after_first_content: false,
                frame_seq: 1,
                cached: None,
            };
            backend.cached = Some(backend.observation(false, None));
            backend
        }

        fn tree(&self, text_ready: bool) -> UiTree {
            let mut id = UiNode::new(
                "n1",
                "android.widget.TextView",
                Bounds::new(0, 0, 100, 40),
            )
            .with_text("Welcome to Gaten");
            id.resource_id = "dev.acos.fixtures:id/title".to_string();
            let mut children = vec![id];
            if text_ready {
                let mut results = UiNode::new(
                    "n2",
                    "android.widget.TextView",
                    Bounds::new(0, 40, 100, 80),
                )
                .with_text("19 results");
                results.resource_id = "dev.acos.fixtures:id/results".to_string();
                children.push(results);
            }
            let mut tree = UiTree::new(
                FrameMeta::new(
                    self.frame_seq,
                    "2026-01-01T00:00:00Z",
                    "test",
                    100,
                    100,
                    420,
                    "h",
                    true,
                ),
                "dev.acos.fixtures.counter",
                children,
            );
            tree.frame.frame_seq = self.frame_seq;
            tree
        }

        fn observation(&self, text_ready: bool, settle: Option<serde_json::Value>) -> SessionObservation {
            let tree = self.tree(text_ready);
            SessionObservation {
                session_id: "s_rel3a".to_string(),
                package: "dev.acos.fixtures.counter".to_string(),
                frame: tree.frame.clone(),
                tree,
                screenshot_png: None,
                crashed: None,
                input: None,
                settle,
            }
        }

        fn strict_timeout() -> BackendError {
            BackendError::Protocol {
                code: ErrorCode::Timeout,
                detail: "strict settle timed out (animating)".to_string(),
                hint: Some("use settle: content".to_string()),
                details: Some(serde_json::json!({
                    "settle": {
                        "mode": "strict",
                        "settled": false,
                        "bound": "timeout",
                        "reason": "animating",
                    }
                })),
            }
        }
    }

    impl Backend for AnimatingBackend {
        fn id(&self) -> &'static str {
            "rel3a-scripted"
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
                logs: false,
                screenshot: true,
                input: true,
                reset: false,
                environment: false,
                crash_reports: false,
            }
        }

        fn manifest(&mut self, apk: &Path) -> Result<crate::backend::ManifestInfo, BackendError> {
            Ok(crate::backend::ManifestInfo {
                path: apk.display().to_string(),
                sha256: "0".repeat(64),
                package: "dev.acos.fixtures.counter".to_string(),
                version_code: 1,
                version_name: "1.0".to_string(),
                min_sdk: 26,
                target_sdk: 34,
                label: None,
                launcher_activity: Some("dev.acos.fixtures.counter.MainActivity".to_string()),
                activities: vec!["dev.acos.fixtures.counter.MainActivity".to_string()],
                native_abis: Vec::new(),
                debuggable: true,
            })
        }

        fn launch(&mut self, _request: &LaunchRequest) -> Result<crate::backend::LaunchOutcome, BackendError> {
            self.frame_seq += 1;
            let observation = self.observation(false, None);
            Ok(crate::backend::LaunchOutcome {
                session_id: "s_rel3a".to_string(),
                package: "dev.acos.fixtures.counter".to_string(),
                frame: Some(observation.frame.clone()),
                screenshot_png: None,
                crashed: None,
                detail: serde_json::json!({}),
            })
        }

        fn observe(&mut self) -> Result<SessionObservation, BackendError> {
            self.cached
                .clone()
                .ok_or_else(|| BackendError::Timeout {
                    detail: "no observation yet".to_string(),
                })
        }

        fn refresh(&mut self) -> Result<SessionObservation, BackendError> {
            self.polls += 1;
            let mode = self.next_mode;
            self.modes.push(mode.as_str());
            self.frame_seq += 1;
            if mode == ObservationSettle::Content {
                self.content_polls += 1;
            }
            let text_ready = if self.flip_after_first_content
                && mode == ObservationSettle::Content
            {
                self.content_polls == 1
            } else {
                self.polls > self.text_missing_polls
            };
            let observation = match mode {
                ObservationSettle::Strict => {
                    if self.animating {
                        return Err(Self::strict_timeout());
                    }
                    self.observation(
                        text_ready,
                        Some(serde_json::json!({
                            "mode": "strict",
                            "settled": true,
                            "bound": "strict-event",
                            "tree_age_ms": 7,
                        })),
                    )
                }
                ObservationSettle::Content => self.observation(
                    text_ready,
                    Some(serde_json::json!({
                        "mode": "content",
                        "settled": true,
                        "bound": "content-stable",
                        "window_ms": 300,
                        "traversals": 3,
                        "elapsed_ms": 320,
                        "frame_seq": self.frame_seq,
                        "pixel_evidence": false,
                    })),
                ),
            };
            self.cached = Some(observation.clone());
            Ok(observation)
        }

        fn capture(&mut self) -> Result<SessionObservation, BackendError> {
            self.refresh()
        }

        fn set_observation_settle(&mut self, mode: ObservationSettle) {
            self.next_mode = mode;
        }
    }

    fn scenario(yaml: &str) -> Scenario {
        Scenario::from_yaml_str(yaml).expect("scenario parses")
    }

    fn run(scenario_yaml: &str, backend: AnimatingBackend) -> RunOutcome {
        run_recording(scenario_yaml, backend).0
    }

    /// Like [`run`], but also returns the observation settle modes the backend
    /// was asked for, in order (REL3A: the mode must not leak between steps).
    fn run_recording(
        scenario_yaml: &str,
        mut backend: AnimatingBackend,
    ) -> (RunOutcome, Vec<&'static str>) {
        let temp = std::env::temp_dir().join(format!(
            "acos-rel3a-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = std::fs::remove_dir_all(&temp);
        std::fs::create_dir_all(&temp).expect("scratch dir");
        // `run_scenario` hashes the scenario file for the reproducer, so the path
        // must name real bytes (the backend never reads it).
        let scenario_path = temp.join("rel3a.yaml");
        std::fs::write(&scenario_path, scenario_yaml).expect("scenario file");
        let options = RunOptions {
            apk: PathBuf::from("fixtures/build/apps/counter.apk"),
            scenario_path,
            artifacts_dir: temp.clone(),
            required_fidelity: None,
            request_id: "req_rel3a".to_string(),
            flags: Vec::new(),
            acos_version: "0.3.0".to_string(),
            // REL3B: the engine the run was started with and the structured
            // warnings it collected. These unit tests run a scripted backend
            // that emulates the ART content rule, and the scenario declares no
            // `engine:`, so the provenance fields never change a verdict here.
            engine: "art".to_string(),
            warnings: Vec::new(),
        };
        let parsed = scenario(scenario_yaml);
        let outcome = run_scenario(&parsed, &options, &mut backend, &crate::clock::FixedClock::new(1_794_318_674_000))
            .expect("run_scenario succeeds");
        let modes = backend.modes.clone();
        let _ = std::fs::remove_dir_all(&temp);
        (outcome, modes)
    }

    const HEADER: &str = "version: 1\nname: rel3a\napp: dev.acos.fixtures.counter\nisolation: process\ndisplay: {mode: offscreen, width: 100, height: 100, density_dpi: 420}\n";

    /// REL3A §1/§2: a `wait_for` on a forever-animating screen passes on the
    /// content-stable rule by default and says so in its step JSON.
    ///
    /// Before REL3A, `wait_for` had no settle option and every poll used the
    /// strict rule, so this step could only time out (the v0.3.0 P1 report: 31 s
    /// TIMEOUT on the RN onboarding screen, `animations: off` did not help).
    #[test]
    fn wait_for_falls_back_to_content_and_reports_it() {
        let outcome = run(
            &format!("{HEADER}steps:\n  - wait_for: {{selector: {{id: title}}, timeout_ms: 4000}}\n"),
            AnimatingBackend::new(true),
        );
        let step = &outcome.verdict.steps[0];
        assert_eq!(step.status, Status::Pass, "content stability backs a text wait");
        let settle = step.settle.as_ref().expect("a settle record");
        assert_eq!(settle["mode"], "content");
        assert_eq!(settle["settled"], true);
        assert_eq!(settle["fallback_from"], "strict");
        assert!(settle["waited_ms"].as_u64().is_some(), "waited_ms is present");
        assert!(settle["reason"].as_str().is_some(), "reason is present");
    }

    /// REL3A §2: one content-settled observation is not a verdict. A wait whose
    /// condition holds on the first stable sample and then stops holding must
    /// not pass (it did before REL3A: the first sample was returned at once).
    #[test]
    fn a_single_content_observation_does_not_pass_a_wait() {
        let mut backend = AnimatingBackend::new(true);
        backend.flip_after_first_content = true;
        let outcome = run(
            &format!(
                "{HEADER}steps:\n  - wait_for: {{selector: {{id: results}}, timeout_ms: 1500}}\n"
            ),
            backend,
        );
        let step = &outcome.verdict.steps[0];
        assert!(
            !step.status.is_pass(),
            "a condition that held on one content sample and then stopped must not pass"
        );
    }

    /// REL3A §1: the observation settle rule is scoped to the observation loop.
    /// A content fallback in a wait must be restored to strict before the next
    /// step's capture; otherwise the capture inherits `settle: content` and
    /// reports pixel evidence from a frame that never stopped animating.
    #[test]
    fn a_content_observation_does_not_leak_into_the_next_capture() {
        let (outcome, modes) = run_recording(
            &format!(
                "{HEADER}steps:\n  - wait_for: {{selector: {{id: title}}, timeout_ms: 4000}}\n  - capture: {{name: leak, fresh: true}}\n"
            ),
            AnimatingBackend::new(true),
        );
        assert_eq!(outcome.verdict.steps[0].status, Status::Pass);
        let capture = &outcome.verdict.steps[1];
        assert!(
            !capture.status.is_pass(),
            "a capture after a content wait must use the strict rule, not inherit content"
        );
        assert_eq!(
            modes.last().copied(),
            Some("strict"),
            "the capture must observe under strict, got modes {modes:?}"
        );
    }

    /// REL3A §1/§3: `wait_for {settle: strict}` never silently falls back, and a
    /// strict settle that never succeeds is a TIMEOUT, never a pass.
    #[test]
    fn wait_for_strict_on_an_animating_screen_is_a_timeout() {
        let outcome = run(
            &format!("{HEADER}steps:\n  - wait_for: {{selector: {{id: title}}, timeout_ms: 300, settle: strict}}\n"),
            AnimatingBackend::new(true),
        );
        let step = &outcome.verdict.steps[0];
        assert_eq!(step.status, Status::Timeout);
        assert!(!step.status.is_pass());
        let error = step.error.as_ref().expect("structured error");
        assert_eq!(error.code, ErrorCode::Timeout);
        assert_eq!(error.code.class().as_str(), "infra_timeout");
    }

    /// REL3A §4: `assert_text` honours `timeout_ms`. Before the fix the assertion
    /// read the bound frame once (1 ms after the action) and failed even though
    /// the text arrived moments later (F-13). With `timeout_ms` absent it still
    /// evaluates exactly one observation, so the old cheap path is unchanged.
    #[test]
    fn assert_text_honours_timeout_ms() {
        let mut backend = AnimatingBackend::new(false);
        backend.text_missing_polls = 2;
        let outcome = run(
            &format!(
                "{HEADER}steps:\n  - wait_for: {{selector: {{id: title}}, timeout_ms: 2000, settle: strict}}\n  - assert_text: {{selector: {{id: results}}, text: {{exact: 19 results}}, timeout_ms: 4000}}\n"
            ),
            backend,
        );
        assert_eq!(outcome.verdict.steps[0].status, Status::Pass);
        let step = &outcome.verdict.steps[1];
        assert_eq!(
            step.status,
            Status::Pass,
            "the assertion waited for the async text instead of racing it"
        );
        let settle = step.settle.as_ref().expect("a settle record");
        assert_eq!(settle["settled"], true);
        assert!(settle["waited_ms"].as_u64().is_some());

        // Without `timeout_ms` the assertion is the single-observation check it
        // always was: the same screen fails.
        let mut backend = AnimatingBackend::new(false);
        backend.text_missing_polls = 2;
        let outcome = run(
            &format!(
                "{HEADER}steps:\n  - wait_for: {{selector: {{id: title}}, timeout_ms: 2000, settle: strict}}\n  - assert_text: {{selector: {{id: results}}, text: {{exact: 19 results}}}}\n"
            ),
            backend,
        );
        assert_eq!(outcome.verdict.steps[1].status, Status::Fail);
    }

    /// REL3A §1/§4: assertions use the content-stable fallback too, so a text
    /// assertion on an animating screen does not need `settle: content` spelled
    /// out.
    #[test]
    fn assert_text_waits_under_content_settle_on_an_animating_screen() {
        let mut backend = AnimatingBackend::new(true);
        backend.text_missing_polls = 1;
        let outcome = run(
            &format!(
                "{HEADER}steps:\n  - assert_text: {{selector: {{id: results}}, text: {{exact: 19 results}}, timeout_ms: 4000}}\n"
            ),
            backend,
        );
        let step = &outcome.verdict.steps[0];
        assert_eq!(step.status, Status::Pass);
        let settle = step.settle.as_ref().expect("a settle record");
        assert_eq!(settle["mode"], "content");
        assert_eq!(settle["settled"], true);
        assert_eq!(settle["fallback_from"], "strict");
    }

    /// REL3A §3: an assertion that never obtains a settled observation fails as
    /// an infra timeout (error class `infra_timeout`), never as a pass and never
    /// as an app assertion failure.
    #[test]
    fn an_assertion_without_a_settled_observation_is_an_infra_timeout() {
        // Content is requested explicitly, and the backend reports a content
        // record for a different frame (so nothing it says can be trusted).
        struct NeverSettles(AnimatingBackend);
        impl Backend for NeverSettles {
            fn id(&self) -> &'static str {
                "rel3a-never"
            }
            fn protocol_backend(&self) -> ProtocolBackend {
                ProtocolBackend::AcosNative
            }
            fn fidelity(&self) -> Fidelity {
                Fidelity::NativeRuntime
            }
            fn capabilities(&self) -> Capabilities {
                self.0.capabilities()
            }
            fn manifest(
                &mut self,
                apk: &Path,
            ) -> Result<crate::backend::ManifestInfo, BackendError> {
                self.0.manifest(apk)
            }
            fn launch(
                &mut self,
                request: &LaunchRequest,
            ) -> Result<crate::backend::LaunchOutcome, BackendError> {
                self.0.launch(request)
            }
            fn observe(&mut self) -> Result<SessionObservation, BackendError> {
                self.0.observe()
            }
            fn refresh(&mut self) -> Result<SessionObservation, BackendError> {
                Err(AnimatingBackend::strict_timeout())
            }
            fn set_observation_settle(&mut self, mode: ObservationSettle) {
                self.0.set_observation_settle(mode);
            }
        }
        let temp = std::env::temp_dir().join(format!(
            "acos-rel3a-never-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&temp);
        std::fs::create_dir_all(&temp).expect("scratch dir");
        // `run_scenario` hashes the scenario file for the reproducer, so the path
        // must name real bytes (the backend never reads it).
        let yaml = format!(
            "{HEADER}steps:\n  - assert_text: {{selector: {{id: results}}, text: {{exact: 19 results}}, timeout_ms: 300}}\n"
        );
        let scenario_path = temp.join("rel3a.yaml");
        std::fs::write(&scenario_path, &yaml).expect("scenario file");
        let options = RunOptions {
            apk: PathBuf::from("fixtures/build/apps/counter.apk"),
            scenario_path,
            artifacts_dir: temp.clone(),
            required_fidelity: None,
            request_id: "req_rel3a_never".to_string(),
            flags: Vec::new(),
            acos_version: "0.3.0".to_string(),
            // REL3B: the engine the run was started with and the structured
            // warnings it collected. These unit tests run a scripted backend
            // that emulates the ART content rule, and the scenario declares no
            // `engine:`, so the provenance fields never change a verdict here.
            engine: "art".to_string(),
            warnings: Vec::new(),
        };
        let parsed = scenario(&yaml);
        let mut backend = NeverSettles(AnimatingBackend::new(true));
        let outcome = run_scenario(
            &parsed,
            &options,
            &mut backend,
            &crate::clock::FixedClock::new(1_794_318_674_000),
        )
        .expect("run_scenario succeeds");
        let _ = std::fs::remove_dir_all(&temp);
        let step = &outcome.verdict.steps[0];
        assert_eq!(step.status, Status::Timeout);
        let error = step.error.as_ref().expect("structured error");
        assert_eq!(error.code.class().as_str(), "infra_timeout");
        let settle = step.settle.as_ref().expect("a settle record");
        assert_eq!(settle["settled"], false);
    }

    /// REL3A §1: content stability still never backs a pixel claim. The pixel
    /// assertion that follows a content-settled wait fails closed.
    #[test]
    fn a_pixel_assertion_after_a_content_settled_wait_fails_closed() {
        let outcome = run(
            &format!(
                "{HEADER}steps:\n  - wait_for: {{selector: {{id: title}}, timeout_ms: 4000}}\n  - assert_region: {{x: 0, y: 0, w: 10, h: 10, min_distinct_colors: 1}}\n"
            ),
            AnimatingBackend::new(true),
        );
        assert_eq!(outcome.verdict.steps[0].status, Status::Pass);
        let step = &outcome.verdict.steps[1];
        assert!(!step.status.is_pass(), "a pixel claim needs pixel evidence");
        let error = step.error.as_ref().expect("structured error");
        let details = error.details.as_ref().expect("details");
        assert_eq!(details["reason"], "content_settle_pixel");
    }

    /// REL3A §2: an observation may only be labelled `mode: content` on a
    /// backend settle record. With no record the executor names the rule it
    /// asked for and marks it unconfirmed - never a content verdict it did not
    /// measure.
    #[test]
    fn an_unconfirmed_content_observation_is_labelled_strict() {
        let backend = AnimatingBackend::new(false);
        let observation = backend.observation(true, None);
        let record = settle_record_of(
            &observation,
            ObservationSettle::Content,
            12,
            Some("strict"),
        )
        .expect("a record");
        assert_eq!(record["mode"], "strict");
        assert_eq!(record["fallback_unconfirmed"], true);
        assert_eq!(record["requested_mode"], "content");
        assert_eq!(record["settled"], true);
        assert_eq!(
            record["reason"],
            crate::settle::SETTLE_REASON_FALLBACK_UNCONFIRMED
        );
    }

    /// REL3A §1: `settle: fast` is rejected on waits and assertions at
    /// validation time, with the options that do work named.
    #[test]
    fn fast_is_rejected_on_observation_steps() {
        for yaml in [
            "  - wait_for: {selector: {id: title}, timeout_ms: 1000, settle: fast}\n",
            "  - assert_text: {selector: {id: title}, text: {contains: Welcome}, settle: fast}\n",
            "  - assert_visible: {selector: {id: title}, settle: fast}\n",
            "  - assert_state: {selector: {id: title}, visible: true, settle: fast}\n",
        ] {
            let error = Scenario::from_yaml_str(&format!("{HEADER}steps:\n{yaml}"))
                .expect_err("settle: fast must be INVALID_SCENARIO");
            assert!(
                error.errors.iter().any(|error| error.path.ends_with(".settle")),
                "settle: fast must be rejected at the step's `settle` path for {yaml}: {error:?}"
            );
        }
        // And the modes that do work parse cleanly.
        let parsed = scenario(&format!(
            "{HEADER}steps:\n  - wait_for: {{selector: {{id: title}}, timeout_ms: 1000, settle: content}}\n  - assert_text: {{selector: {{id: title}}, text: {{contains: Welcome}}, settle: content, timeout_ms: 2000}}\n"
        ));
        assert_eq!(parsed.validate().errors, Vec::new());
    }
}
