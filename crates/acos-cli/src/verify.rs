//! `acos verify <scenario.yaml> [--apk ...]` — the scenario runner entry point.
//!
//! Default backend is the native ACOS runtime (static manifest + M2 attach
//! boot); every capability it does not yet have is reported as `unsupported`.
//! Passing `--reference <dir>` selects the ART reference replay backend, which
//! serves the checked-in `art-reference` evidence so the assertion logic and
//! false-pass prevention can be exercised on this host.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

use acos_protocol::commands::VerifyRequest;
use acos_protocol::{
    Backend as ProtocolBackend, Envelope, ErrorCode, Fidelity, Metrics, ProtocolError, RequestBase,
};
use acos_verify::native::NativeBackend;
use acos_verify::reference::ReferenceBackend;
use acos_verify::Backend;

use crate::engine::{Engine, EngineBackend, Mode};

/// Arguments for `acos verify`.
#[derive(Debug, clap::Args)]
pub struct VerifyArgs {
    /// Scenario YAML file to run (positional).
    pub scenario: Option<PathBuf>,

    /// Scenario YAML file to run (equivalent to the positional argument).
    #[arg(long = "scenario", value_name = "FILE")]
    pub scenario_flag: Option<PathBuf>,

    /// APK to run (overrides the scenario `app` field).
    #[arg(long)]
    pub apk: Option<PathBuf>,

    /// Artifacts output directory (default: `out/<scenario stem>`).
    #[arg(long)]
    pub artifacts: Option<PathBuf>,

    /// ART reference directory; selects the reference-replay backend.
    #[arg(long)]
    pub reference: Option<PathBuf>,

    /// Minimum fidelity a pass must reach (for example `native-runtime`).
    #[arg(long = "required-fidelity")]
    pub required_fidelity: Option<String>,

    /// Deterministic flags recorded in the reproducer (repeatable).
    #[arg(long = "flag")]
    pub flags: Vec<String>,

    /// Enable the APK's native libraries (`lib/<host-abi>/`) under confinement.
    ///
    /// Equivalent to `acos launch --native`: the confinement is entered before
    /// the session worker (and therefore before guest code) starts, and the
    /// loader fails closed when it is unavailable.
    #[arg(long)]
    pub native: bool,

    /// Execution engine. No fallback: `art` fails loudly when ART is
    /// unavailable, and never silently runs the Rust VM instead.
    #[arg(long, value_name = "ENGINE", default_value = "rust-vm")]
    pub engine: String,

    /// Emit the standard ACOS JSON envelope instead of a text summary.
    #[arg(long)]
    pub json: bool,
}

/// Run the `verify` command.
pub fn run(args: VerifyArgs) -> ExitCode {
    let request_id = new_request_id();
    let Some(scenario) = args.scenario.clone().or_else(|| args.scenario_flag.clone()) else {
        eprintln!("error: a scenario file is required (positional or `--scenario <file>`)");
        return ExitCode::FAILURE;
    };
    let artifacts = args
        .artifacts
        .clone()
        .unwrap_or_else(|| default_artifacts(&scenario));

    let Some(engine) = Engine::parse(&args.engine) else {
        let envelope = Envelope::failure(
            request_id,
            ProtocolBackend::AcosNative,
            Fidelity::StaticAnalysis,
            ProtocolError::new(
                ErrorCode::InvalidArgument,
                format!("unknown --engine value {:?}", args.engine),
            )
            .with_hint("Use `--engine rust-vm` (default) or `--engine art`. There is no silent fallback between engines.")
            .with_details(serde_json::json!({"field": "engine", "value": args.engine})),
            Metrics::wall_ms(0),
        );
        return finish(&envelope, &args, &artifacts);
    };
    if engine == Engine::Art && args.reference.is_some() {
        let envelope = Envelope::failure(
            request_id,
            ProtocolBackend::AcosNative,
            Fidelity::StaticAnalysis,
            ProtocolError::new(
                ErrorCode::InvalidArgument,
                "--engine art cannot be combined with --reference",
            )
            .with_hint("`--reference` replays recorded ART evidence; it is not live ART execution. Run them separately.")
            .with_details(serde_json::json!({"field": "engine", "value": "art"})),
            Metrics::wall_ms(0),
        );
        return finish(&envelope, &args, &artifacts);
    }
    if engine == Engine::Art && crate::native_launch::native_requested(args.native) {
        let envelope = Envelope::failure(
            request_id,
            ProtocolBackend::AcosNative,
            Fidelity::StaticAnalysis,
            ProtocolError::new(
                ErrorCode::InvalidArgument,
                "--engine art cannot be combined with --native",
            )
            .with_hint("`--native` selects the Rust loader inside the ACOS runtime; ART has its own process. Run them separately.")
            .with_details(serde_json::json!({"field": "engine", "value": "art"})),
            Metrics::wall_ms(0),
        );
        return finish(&envelope, &args, &artifacts);
    }

    let required_fidelity = match args.required_fidelity.as_deref() {
        None => None,
        Some(value) => match parse_fidelity(value) {
            Some(fidelity) => Some(fidelity),
            None => {
                let envelope = Envelope::failure(
                    request_id,
                    ProtocolBackend::AcosNative,
                    Fidelity::StaticAnalysis,
                    ProtocolError::new(
                        ErrorCode::InvalidArgument,
                        format!("unknown --required-fidelity value {value:?}"),
                    )
                    .with_hint("Use one of: static-analysis, jvm-reference, native-runtime, art-reference, physical-device, simulated.")
                    .with_details(serde_json::json!({"field": "required_fidelity", "value": value})),
                    Metrics::wall_ms(0),
                );
                return finish(&envelope, &args, &artifacts);
            }
        },
    };

    let request = VerifyRequest {
        base: RequestBase::new(request_id.clone()),
        apk: args.apk.as_ref().map(|path| path.display().to_string()),
        scenario: scenario.display().to_string(),
        artifacts: artifacts.display().to_string(),
        reference: args.reference.as_ref().map(|path| path.display().to_string()),
        required_fidelity,
        flags: args.flags.clone(),
    };

    let native = args.reference.is_none() && crate::native_launch::native_requested(args.native);
    if native {
        let path = args.apk.clone().unwrap_or_else(|| scenario.clone());
        let apk = match crate::verify::resolve_apk(&path, &scenario) {
            Ok(apk) => apk,
            Err(code) => {
                let envelope = Envelope::failure(
                    request_id,
                    ProtocolBackend::AcosNative,
                    Fidelity::StaticAnalysis,
                    code,
                    Metrics::wall_ms(0),
                );
                return finish(&envelope, &args, &artifacts);
            }
        };
        // The verifier must still be able to write its evidence directory, so
        // that path is added to the confinement's writable root explicitly.
        if let Err(error) =
            crate::native_launch::confine_native_launch_with_writable(
            &apk,
            Some(&artifacts),
            Some(&scenario),
        )
        {
            let envelope = Envelope::failure(
                request_id,
                ProtocolBackend::AcosNative,
                Fidelity::StaticAnalysis,
                ProtocolError::new(
                    ErrorCode::from_str_opt(error.code).unwrap_or(ErrorCode::UnsupportedBackend),
                    error.message,
                )
                .with_details(serde_json::json!({"capability": "native-loader"})),
                Metrics::wall_ms(0),
            );
            return finish(&envelope, &args, &artifacts);
        }
    }

    let mut backend: EngineBackend = match engine {
        // The default route is forwarded verbatim: engine selection does not
        // change Rust VM behaviour.
        Engine::RustVm => {
            let native_backend: Box<dyn Backend> = match &args.reference {
                Some(dir) => Box::new(ReferenceBackend::new(dir)),
                None => Box::new(NativeBackend::new().with_native(native)),
            };
            EngineBackend::native(native_backend)
        }
        Engine::Art => {
            let path = args.apk.clone().unwrap_or_else(|| scenario.clone());
            let apk = match resolve_apk(&path, &scenario) {
                Ok(apk) => apk,
                Err(code) => {
                    let envelope = Envelope::failure(
                        request_id,
                        ProtocolBackend::AcosNative,
                        Fidelity::StaticAnalysis,
                        code,
                        Metrics::wall_ms(0),
                    );
                    return finish(&envelope, &args, &artifacts);
                }
            };
            match crate::engine::art::ArtSession::start(&apk, &artifacts) {
                Ok(session) => EngineBackend::session(Box::new(session)),
                Err(error) => {
                    let envelope = Envelope::failure(
                        request_id,
                        ProtocolBackend::AcosNative,
                        Fidelity::StaticAnalysis,
                        error.to_protocol_error(),
                        Metrics::wall_ms(0),
                    );
                    return finish(&envelope, &args, &artifacts);
                }
            }
        }
    };
    let engine_label = backend.engine();
    let mode = backend.mode().unwrap_or(Mode::Live);
    let session_caps = backend.session_capabilities();
    let mut envelope = acos_verify::verify(&request, &mut backend);
    annotate(&mut envelope, &args, engine_label, mode, session_caps.as_ref(), &artifacts);
    finish(&envelope, &args, &artifacts)
}

/// Put the engine choice and mode into the JSON verdict, enforce that preview is
/// never a pass, and write the annotated verdict next to the verifier's one.
pub(crate) fn annotate(
    envelope: &mut Envelope,
    args: &VerifyArgs,
    engine: Option<Engine>,
    mode: Mode,
    caps: Option<&crate::engine::SessionCapabilities>,
    artifacts: &Path,
) {
    let engine = engine.unwrap_or(Engine::RustVm);
    let Some(result) = envelope.result().cloned() else {
        return;
    };
    let Some(mut verdict) = result.get("verdict").cloned() else {
        return;
    };
    let unsupported_steps: Vec<serde_json::Value> = verdict
        .get("steps")
        .and_then(|steps| steps.as_array())
        .map(|steps| {
            steps
                .iter()
                .enumerate()
                .filter(|(_, step)| step.get("status").and_then(|s| s.as_str()) == Some("unsupported"))
                .map(|(index, step)| {
                    let capability = step
                        .pointer("/error/details/capability")
                        .and_then(|c| c.as_str())
                        .unwrap_or("unknown");
                    serde_json::json!({
                        "index": index,
                        "step": step.get("step").and_then(|k| k.as_str()).unwrap_or(""),
                        "capability": capability,
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    let evidence = serde_json::json!({
        "artifacts": artifacts.display().to_string(),
        "verdict": artifacts.join(acos_protocol::artifacts::VERDICT_JSON).display().to_string(),
        "engine_report": artifacts.join("engine.json").display().to_string(),
    });
    let engine_block = serde_json::json!({
        "engine": engine.as_str(),
        "mode": mode.as_str(),
        "unsupported_steps": unsupported_steps,
        "evidence": evidence,
        "commands": caps.map(|caps| serde_json::json!({
            "supported": caps.names(),
            "missing": caps.missing(),
        })),
        "explicit": args.engine != "rust-vm" || mode == Mode::Preview,
    });
    if let Some(object) = verdict.as_object_mut() {
        object.insert("engine".to_string(), engine_block["engine"].clone());
        object.insert("mode".to_string(), engine_block["mode"].clone());
        object.insert(
            "unsupported_steps".to_string(),
            engine_block["unsupported_steps"].clone(),
        );
        object.insert("evidence".to_string(), engine_block["evidence"].clone());
    }

    let preview_downgrade = mode == Mode::Preview && crate::engine::preview_downgrade(&mut verdict);
    // Evidence files: the annotated verdict (engine/mode fields are additive) and
    // a compact engine report. Never fail the verdict over a write error.
    if std::fs::create_dir_all(artifacts).is_ok() {
        let _ = std::fs::write(
            artifacts.join(acos_protocol::artifacts::VERDICT_JSON),
            serde_json::to_string_pretty(&verdict).unwrap_or_default(),
        );
        let _ = std::fs::write(
            artifacts.join("engine.json"),
            serde_json::to_string_pretty(&serde_json::json!({
                "request_id": envelope.request_id(),
                "engine": engine_block,
                "verdict_status": verdict.get("verdict"),
            }))
            .unwrap_or_default(),
        );

    }
    let mut annotated_result = result;
    annotated_result["verdict"] = verdict;
    if preview_downgrade {
        let failure = Envelope::failure(
            new_request_id(),
            envelope.backend(),
            envelope.fidelity(),
            ProtocolError::new(
                ErrorCode::UnsupportedBackend,
                "preview output (mode: preview) is not a real verification",
            )
            .with_details(serde_json::json!({"capability": "mode.live", "mode": "preview"})),
            envelope.metrics(),
        );
        *envelope = failure.with_value_result(annotated_result);
    } else {
        *envelope = envelope.clone().with_value_result(annotated_result);
    }
}

fn finish(envelope: &Envelope, args: &VerifyArgs, artifacts: &Path) -> ExitCode {
    if args.json {
        print_json(envelope);
    } else {
        print_summary(envelope, artifacts);
    }
    if envelope.status().is_pass() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

fn print_summary(envelope: &Envelope, artifacts: &Path) {
    println!(
        "verify: {} (ok={}, backend={}, fidelity={})",
        envelope.status(),
        envelope.ok(),
        envelope.backend(),
        envelope.fidelity()
    );
    if let Some(verdict) = envelope.result().and_then(|result| result.get("verdict")) {
        let engine = verdict.get("engine").and_then(|v| v.as_str()).unwrap_or("-");
        let mode = verdict.get("mode").and_then(|v| v.as_str()).unwrap_or("-");
        println!("  engine: {engine} (mode: {mode})");
        if let Some(steps) = verdict.get("unsupported_steps").and_then(|v| v.as_array()) {
            for step in steps {
                println!(
                    "  unsupported: {} [{}]",
                    step.get("capability").and_then(|v| v.as_str()).unwrap_or("unknown"),
                    step.get("step").and_then(|v| v.as_str()).unwrap_or("")
                );
            }
        }
    }
    if let Some(error) = envelope.error() {
        println!("  error: {}: {}", error.code, error.message);
    }
    println!("  artifacts: {}", artifacts.display());
    if let Some(result) = envelope.result() {
        if let Some(steps) = result
            .get("verdict")
            .and_then(|verdict| verdict.get("steps"))
            .and_then(|steps| steps.as_array())
        {
            println!("  steps: {}", steps.len());
        }
    }
}

fn default_artifacts(scenario: &Path) -> PathBuf {
    let stem = scenario
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("scenario");
    PathBuf::from("out").join(stem)
}

fn parse_fidelity(value: &str) -> Option<Fidelity> {
    serde_json::from_value(serde_json::Value::String(value.to_string())).ok()
}

fn new_request_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("req_{:x}_{:x}", std::process::id(), nanos)
}

fn print_json<T: serde::Serialize>(value: &T) {
    match serde_json::to_string_pretty(value) {
        Ok(text) => println!("{text}"),
        Err(error) => eprintln!("error: failed to serialize JSON envelope: {error}"),
    }
}

/// Resolve the APK the same way `verify` does: `--apk` wins, otherwise the
/// scenario's `app` field relative to the scenario file (or the cwd).
pub fn resolve_apk(path: &Path, scenario: &Path) -> Result<PathBuf, ProtocolError> {
    let scenario_text = std::fs::read_to_string(scenario).map_err(|error| {
        ProtocolError::new(
            ErrorCode::InvalidScenario,
            format!("cannot read scenario {}: {error}", scenario.display()),
        )
    })?;
    let app = scenario_text
        .lines()
        .find_map(|line| line.trim().strip_prefix("app:"))
        .map(|value| value.trim().trim_matches('"').trim_matches('\'').to_string());
    if path != scenario {
        return Ok(path.to_path_buf());
    }
    let Some(app) = app else {
        return Err(ProtocolError::new(
            ErrorCode::InvalidScenario,
            format!("scenario {} has no `app:` field", scenario.display()),
        ));
    };
    let app = PathBuf::from(app);
    if app.is_absolute() {
        return Ok(app);
    }
    let candidate = scenario
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .map(|parent| parent.join(&app))
        .unwrap_or_else(|| app.clone());
    Ok(if candidate.exists() { candidate } else { app })
}
