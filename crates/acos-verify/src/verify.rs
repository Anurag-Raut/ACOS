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
    //
    // The source text is read once and kept: the scenario's *declared* `engine:`
    // spelling (`engine_mismatch_warning`) is only visible in the document, not
    // in the parsed value, and this is also the one read of the input a run
    // needs before it starts (PROVENANCE hashes those same bytes).
    let scenario_text = match std::fs::read_to_string(Path::new(&request.scenario)) {
        Ok(text) => Some(text),
        Err(_) => None,
    };
    let parsed = match scenario_text.as_deref() {
        Some(text) => Scenario::from_yaml_str(text),
        None => Scenario::load(Path::new(&request.scenario)),
    };
    let scenario = match parsed {
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
    //
    // PROVENANCE: both paths are made absolute before anything else. A relative
    // path means a different file to every reader (the caller's cwd, the MCP
    // server's cwd, `repro.sh`'s cwd), and it was a relative scenario path in
    // `repro.json` that made an older scenario run against a stale APK look
    // reproducible.
    // A relative `app:` resolves against the scenario path **as the caller
    // spelled it** (its parent directory), which is the documented behaviour; a
    // canonicalized path would silently re-anchor the app after a symlinked
    // scenario directory. Only the value written into provenance is canonical.
    let scenario_path = acos_protocol::provenance::absolute(Path::new(&request.scenario));
    let declared_app = resolve_relative(&request.scenario, &scenario.app);
    let apk = request
        .apk
        .as_deref()
        .map(|path| acos_protocol::provenance::absolute(Path::new(path)))
        .unwrap_or_else(|| acos_protocol::provenance::absolute(&declared_app));
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

    // PROVENANCE: a `pass` says which APK it judged. When `--apk` and the
    // scenario's own `app:` name different files the run is still valid (the
    // flag is documented to override), but it is never silent: the warning goes
    // into `provenance.warnings[]` in the envelope and in `verdict.json`.
    let mut warnings: Vec<serde_json::Value> = Vec::new();
    if request.apk.is_some() {
        let declared = acos_protocol::provenance::absolute(&declared_app);
        let used = acos_protocol::provenance::absolute(&apk);
        if declared != used {
            warnings.push(serde_json::json!({
                "code": acos_protocol::provenance::WARN_SCENARIO_APP_OVERRIDDEN,
                "message": format!(
                    "the scenario's own `app:` names {} but this run used --apk {}; the verdict \
                     is about the --apk file (provenance.apk.sha256_12 is the digest of the bytes \
                     that ran). Drop --apk to run the scenario's own app.",
                    declared.display(),
                    used.display()
                ),
                "scenario_app": declared.display().to_string(),
                "apk": used.display().to_string(),
            }));
        }
    }

    // PROVENANCE: a scenario that pins one engine while another runs is never
    // silent. The field does not select the engine (the request's `engine` does)
    // — the warning states both words and the verdict keeps the engine that ran.
    if let Some(declared) = scenario_text
        .as_deref()
        .and_then(acos_protocol::scenario::declared_engine)
    {
        let ran = request.engine.as_str();
        let canonical = match declared.as_str() {
            "native-preferred" | "native-only" => "rust-vm",
            "reference-only" => "art",
            other => other,
        };
        if let Some(warning) = acos_protocol::scenario::engine_mismatch_warning(canonical, ran)
            .filter(|_| {
                // A declared engine always produces the mismatch warning; the
                // legacy-spelling warning already explains the rename.
                canonical != ran
            })
        {
            warnings.push(serde_json::json!({
                "code": acos_protocol::provenance::WARN_SCENARIO_ENGINE_MISMATCH,
                "message": warning.message,
                "scenario_engine": canonical,
                "engine": ran,
            }));
        }
    }

    let isolation = request.base.isolation.unwrap_or(scenario.isolation);
    let mut scenario = scenario;
    scenario.isolation = isolation;
    if let Some(display) = request.base.display {
        scenario.display = display;
    }

    let options = RunOptions {
        apk,
        scenario_path: scenario_path.clone(),
        artifacts_dir: PathBuf::from(&request.artifacts),
        required_fidelity: request.required_fidelity,
        request_id: request_id.clone(),
        flags: request.flags.clone(),
        acos_version: env!("CARGO_PKG_VERSION").to_string(),
        // ADR-0029 item 4: the scenario's requested `services:` (a requested
        // `off` is attached-off, so a step that needs it is unsupported).
        services: scenario
            .services
            .iter()
            .map(|service| {
                (
                    service.id.clone(),
                    service
                        .profile
                        .clone()
                        .unwrap_or_else(|| "real".to_string()),
                )
            })
            .collect(),
        engine: request.engine.as_str().to_string(),
        warnings,
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
                    format!(
                        "verify produced status `{}` without a failure object",
                        verdict.verdict
                    ),
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
            ProtocolError::new(
                ErrorCode::Internal,
                format!("cannot serialize verdict: {error}"),
            )
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
