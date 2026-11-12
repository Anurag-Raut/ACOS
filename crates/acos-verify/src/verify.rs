//! The top-level `verify` command: scenario in, evidence-backed envelope out.

use std::path::{Path, PathBuf};

use acos_protocol::artifacts::VERDICT_JSON;
use acos_protocol::commands::{VerifyRequest, VerifyResponse};
use acos_protocol::scenario::Scenario;
use acos_protocol::{
    Backend as ProtocolBackend, Envelope, ErrorCode, Evidence, Fidelity, Metrics, ProtocolError,
    Status,
};

use crate::backend::Backend;
use crate::clock::{Clock, SystemClock};
use crate::error::VerifyError;
use crate::executor::{run_scenario, RunOptions};

/// Run `verify` with the host clock.
pub fn verify(request: &VerifyRequest, backend: &mut dyn Backend) -> Envelope {
    verify_with_clock(request, backend, &SystemClock)
}

/// Run `verify` with an explicit clock (tests use a [`crate::clock::FixedClock`]).
pub fn verify_with_clock(
    request: &VerifyRequest,
    backend: &mut dyn Backend,
    clock: &dyn Clock,
) -> Envelope {
    let request_id = request.base.request_id.clone();

    // 1. Parse and validate the scenario. Every validation problem is returned.
    let scenario = match Scenario::load(Path::new(&request.scenario)) {
        Ok(scenario) => scenario,
        Err(validation) => {
            let protocol = ProtocolError::new(
                ErrorCode::InvalidScenario,
                format!(
                    "{} is not a valid scenario: {} error(s)",
                    request.scenario,
                    validation.errors.len()
                ),
            )
            .with_hint("Fix every path in `details.errors`; do not weaken runtime validation to accept it.")
            .with_details(serde_json::json!({
                "scenario": request.scenario,
                "errors": validation.errors,
                "warnings": validation.warnings,
            }));
            return Envelope::failure(
                request_id,
                backend.protocol_backend(),
                Fidelity::StaticAnalysis,
                protocol,
                Metrics::wall_ms(0),
            );
        }
    };

    // 2. Resolve the APK (flag overrides the scenario).
    let apk = request
        .apk
        .clone()
        .map(PathBuf::from)
        .unwrap_or_else(|| resolve_relative(&request.scenario, &scenario.app));
    if !apk.is_file() {
        let protocol = ProtocolError::new(
            ErrorCode::ApkIo,
            format!("APK not found: {}", apk.display()),
        )
        .with_hint("Pass `--apk <path>` or fix the scenario `app` field.")
        .with_details(serde_json::json!({"apk": apk.display().to_string()}));
        return Envelope::failure(
            request_id,
            backend.protocol_backend(),
            backend.fidelity(),
            protocol,
            Metrics::wall_ms(0),
        );
    }

    let isolation = request
        .base
        .isolation
        .unwrap_or(scenario.isolation);
    let mut scenario = scenario;
    scenario.isolation = isolation;
    if let Some(display) = request.base.display {
        scenario.display = display;
    }

    let options = RunOptions {
        apk,
        scenario_path: PathBuf::from(&request.scenario),
        artifacts_dir: PathBuf::from(&request.artifacts),
        required_fidelity: request.required_fidelity,
        request_id: request_id.clone(),
        flags: request.flags.clone(),
        acos_version: env!("CARGO_PKG_VERSION").to_string(),
    };

    // 3. Run.
    let outcome = match run_scenario(&scenario, &options, backend, clock) {
        Ok(outcome) => outcome,
        Err(error) => return error_envelope(&request_id, backend, error),
    };

    // 4. Wrap the verdict in the response envelope.
    let verdict = outcome.verdict;
    let verdict_path = format!(
        "{}/{}",
        request.artifacts.trim_end_matches('/'),
        VERDICT_JSON
    );
    let response = VerifyResponse {
        verdict: verdict.clone(),
        verdict_path,
        artifacts_dir: request.artifacts.clone(),
        artifact_validation: outcome.artifact_validation,
    };

    let metrics = Metrics::wall_ms(verdict.resources.wall_ms);
    let metrics = match verdict.resources.peak_rss_bytes {
        Some(peak) => metrics.with_peak_rss_bytes(peak),
        None => metrics,
    };

    let evidence = Evidence {
        screenshot: None,
        tree: None,
        events: Some("events.jsonl".to_string()),
        log: Some("logs/logcat.txt".to_string()),
        verdict: Some(VERDICT_JSON.to_string()),
    };

    let mut envelope = if verdict.verdict == Status::Pass {
        Envelope::pass(
            request_id,
            backend.protocol_backend(),
            verdict.fidelity,
            metrics,
        )
    } else {
        let error = verdict
            .failure
            .as_ref()
            .map(|failure| failure.error.clone())
            .unwrap_or_else(|| {
                ProtocolError::new(
                    ErrorCode::Internal,
                    format!("verify produced status `{}` without a failure object", verdict.verdict),
                )
                .with_hint(
                    "Runner invariant violated: a non-pass verdict must carry a failure object. \
                     Fix the executor; never synthesize a pass.",
                )
            });
        Envelope::failure(
            request_id,
            backend.protocol_backend(),
            verdict.fidelity,
            error,
            metrics,
        )
    };

    envelope = envelope
        .with_isolation(verdict.isolation)
        .with_capabilities(outcome.capabilities_used)
        .with_evidence(evidence);
    if let Some(session_id) = &verdict.session_id {
        envelope = envelope.with_session_id(session_id);
    }
    match envelope.with_result(&response) {
        Ok(envelope) => envelope,
        Err(error) => Envelope::failure(
            response.verdict.request_id.clone(),
            backend.protocol_backend(),
            response.verdict.fidelity,
            ProtocolError::new(ErrorCode::Internal, format!("cannot serialize verdict: {error}"))
                .with_hint(
                    "The verdict type serialized to invalid JSON (a runner type bug). Capture the \
                     scenario/artifacts and report it; the run is not a pass.",
                ),
            metrics,
        ),
    }
}

fn error_envelope(request_id: &str, backend: &dyn Backend, error: VerifyError) -> Envelope {
    let protocol = match &error {
        VerifyError::InvalidScenario(validation) => ProtocolError::new(
            ErrorCode::InvalidScenario,
            format!("scenario is invalid: {} error(s)", validation.errors.len()),
        )
        .with_details(serde_json::json!({
            "errors": validation.errors,
            "warnings": validation.warnings,
        })),
        VerifyError::ScenarioIo(detail) => {
            ProtocolError::new(ErrorCode::InvalidScenario, detail.clone())
        }
        VerifyError::ArtifactIo { path, detail } => {
            ProtocolError::new(ErrorCode::ArtifactsInvalid, detail.clone())
                .with_details(serde_json::json!({"path": path}))
        }
        VerifyError::DiskFull { path, detail } => {
            ProtocolError::new(ErrorCode::HostDiskFull, detail.clone())
                .with_details(serde_json::json!({"path": path}))
        }
        VerifyError::Backend(backend_error) => backend_error.to_protocol_error(),
    };
    Envelope::failure(
        request_id,
        backend.protocol_backend(),
        backend.fidelity(),
        protocol,
        Metrics::wall_ms(0),
    )
}

/// Resolve a scenario-relative path against the scenario file's directory.
pub(crate) fn resolve_relative(scenario_path: &str, app: &str) -> PathBuf {
    let app_path = Path::new(app);
    if app_path.is_absolute() {
        return app_path.to_path_buf();
    }
    let scenario_dir = Path::new(scenario_path)
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    // Prefer the scenario-relative path, then the process cwd.
    let candidate = scenario_dir.join(app_path);
    if candidate.exists() {
        candidate
    } else {
        app_path.to_path_buf()
    }
}

/// The protocol backend of a backend, for callers that need to build envelopes.
pub fn protocol_backend(backend: &dyn Backend) -> ProtocolBackend {
    backend.protocol_backend()
}
