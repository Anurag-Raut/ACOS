//! `acos launch <apk>` — boot an app and report the honest launch stage ladder.
//!
//! The command creates an in-process [`AgentSession`] (worker thread owns the
//! `Vm`), boots the real framework bytecode as far as it goes, and prints the
//! ladder: `apk_parsed → process_booted → bind_application →
//! application_on_create → client_transaction → activity_on_create → on_resume
//! → first_frame`. Every rung is `ok` only when the real framework completed it;
//! the first rung that did not completes with a structured blocker.
//!
//! There is no daemon: the session ends when the command exits. Use the MCP
//! server (`acos-mcp`) when a session must persist across tool calls.

use std::path::PathBuf;
use std::process::ExitCode;

use acos_protocol::{
    Backend as ProtocolBackend, Envelope, ErrorCode, Fidelity, Metrics, ProtocolError,
};
use acos_runtime::session::{
    AgentSession, LaunchReport, SessionError, SessionOptions, StageStatus,
};
use serde_json::{json, Value};

use crate::new_request_id;

/// Arguments for `acos launch`.
#[derive(Debug, clap::Args)]
pub struct LaunchArgs {
    /// APK to launch.
    pub apk: PathBuf,

    /// Wall-clock ceiling for the VM watchdog, in seconds.
    #[arg(long, default_value_t = 120)]
    pub timeout: u64,

    /// Emit the standard ACOS JSON envelope instead of a text summary.
    #[arg(long)]
    pub json: bool,
}

/// Run `acos launch`.
pub fn run(args: LaunchArgs) -> ExitCode {
    let request_id = new_request_id();
    let session = AgentSession::start(SessionOptions {
        wall_secs: args.timeout,
        ..SessionOptions::default()
    });
    let session_id = session.id().to_string();
    let result = session.launch(&args.apk);
    session.close();

    match result {
        Ok(report) => {
            let envelope = launch_envelope(&request_id, &session_id, &report);
            finish(&envelope, &args, &report);
            if envelope.status().is_pass() {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
        Err(err) => {
            let envelope = session_error_envelope(&request_id, &err);
            if args.json {
                crate::print_json(&envelope);
            } else {
                eprintln!("launch: {}: {}", err.code, err.message);
            }
            ExitCode::FAILURE
        }
    }
}

fn finish(envelope: &Envelope, args: &LaunchArgs, report: &LaunchReport) {
    if args.json {
        crate::print_json(envelope);
    } else {
        print_text(report);
    }
}

fn print_text(report: &LaunchReport) {
    println!(
        "launch: reached {} (classes={}, wall={}ms)",
        report.reached_stage, report.classes_loaded, report.wall_ms
    );
    if let Some(package) = &report.package {
        println!("  package: {package}");
    }
    for stage in &report.stages {
        let mark = match stage.status {
            StageStatus::Ok => "ok",
            StageStatus::Blocked => "blocked",
            StageStatus::NotReached => "not-reached",
        };
        println!("  [{mark:>11}] {}", stage.name);
        if let Some(blocker) = &stage.blocker {
            println!("      {}: {}", blocker.code, first_line(&blocker.message));
        }
    }
}

fn first_line(text: &str) -> &str {
    text.lines().next().unwrap_or("")
}

/// Build the `launch` envelope from a completed ladder.
pub fn launch_envelope(request_id: &str, session_id: &str, report: &LaunchReport) -> Envelope {
    let result = ladder_json(report);
    let mut metrics = Metrics::wall_ms(report.wall_ms);
    if report.peak_rss_bytes > 0 {
        metrics = metrics.with_peak_rss_bytes(report.peak_rss_bytes);
    }
    metrics.classes_loaded = Some(report.classes_loaded as u64);

    let envelope = match &report.root_blocker {
        None => Envelope::pass(
            request_id,
            ProtocolBackend::AcosNative,
            Fidelity::NativeRuntime,
            metrics,
        ),
        Some(blocker) => {
            let code = error_code(&blocker.code);
            let mut error = ProtocolError::new(code, blocker.message.clone())
                .with_details(blocker_details(blocker, report));
            if code == ErrorCode::UnsupportedBackend {
                error = error.with_hint(
                    "The app process ran but no completed frame exists yet. Do not treat this as a pass; \
                     re-run `acos observe` after the render/handoff path publishes a frame.",
                );
            }
            Envelope::failure(
                request_id,
                ProtocolBackend::AcosNative,
                Fidelity::NativeRuntime,
                error,
                metrics,
            )
        }
    };
    envelope
        .with_session_id(session_id)
        .with_value_result(result)
        .with_capabilities(["apk.inspect", "lifecycle"])
}

/// Map any [`SessionError`] onto a protocol envelope.
pub fn session_error_envelope(request_id: &str, err: &SessionError) -> Envelope {
    let code = error_code(err.code);
    let mut error = ProtocolError::new(code, err.message.clone());
    if !err.details.is_empty() {
        error = error.with_details(details_json(&err.details));
    }
    Envelope::failure(
        request_id,
        ProtocolBackend::AcosNative,
        Fidelity::NativeRuntime,
        error,
        Metrics::wall_ms(0),
    )
}

/// Resolve a session error-code string to a protocol [`ErrorCode`].
///
/// Unknown strings become `INTERNAL` (a harness defect) rather than being
/// silently reinterpreted.
pub fn error_code(code: &str) -> ErrorCode {
    ErrorCode::from_str_opt(code).unwrap_or(ErrorCode::Internal)
}

/// Build the `result` ladder JSON shared by the CLI and the MCP session tools.
pub fn ladder_json(report: &LaunchReport) -> Value {
    let stages: Vec<Value> = report
        .stages
        .iter()
        .map(|stage| {
            let status = match stage.status {
                StageStatus::Ok => "ok",
                StageStatus::Blocked => "blocked",
                StageStatus::NotReached => "not_reached",
            };
            json!({
                "stage": stage.name,
                "ok": stage.status == StageStatus::Ok,
                "status": status,
                "blocker": stage.blocker.as_ref().map(|b| json!({
                    "code": b.code,
                    "message": b.message,
                    "details": details_json(&b.details),
                })),
            })
        })
        .collect();
    json!({
        "stages": stages,
        "reached_stage": report.reached_stage,
        "package": report.package,
        "activity": report.activity,
        "classes_loaded": report.classes_loaded,
        "events": report.events,
        "wall_ms": report.wall_ms,
        "peak_rss_bytes": report.peak_rss_bytes,
    })
}

fn blocker_details(blocker: &acos_runtime::session::StageBlocker, report: &LaunchReport) -> Value {
    let mut map = details_json(&blocker.details);
    if let Value::Object(ref mut object) = map {
        object.insert("reached_stage".to_string(), json!(report.reached_stage));
        object.insert("classes_loaded".to_string(), json!(report.classes_loaded));
    }
    map
}

/// `Vec<(String, String)>` details to a JSON object.
pub fn details_json(details: &[(String, String)]) -> Value {
    let mut map = serde_json::Map::new();
    for (key, value) in details {
        map.insert(key.clone(), Value::String(value.clone()));
    }
    Value::Object(map)
}
