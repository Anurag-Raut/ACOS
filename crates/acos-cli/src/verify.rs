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

    let mut backend: Box<dyn Backend> = match &args.reference {
        Some(dir) => Box::new(ReferenceBackend::new(dir)),
        None => Box::new(NativeBackend::new().with_native(native)),
    };
    let envelope = acos_verify::verify(&request, backend.as_mut());
    finish(&envelope, &args, &artifacts)
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
