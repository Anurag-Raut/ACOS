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

use crate::engine::Engine;
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

    /// Enable native code from the APK's `lib/<abi>/` under confinement.
    ///
    /// Off by default until the ADR-0018 security review passes. Also enabled
    /// by `ACOS_NATIVE=1`. Fails closed (`CONFINEMENT_UNAVAILABLE`) when the
    /// process cannot enter the sandbox, and `UNSUPPORTED_ABI` when the APK has
    /// native code only for a non-host ABI.
    #[arg(long)]
    pub native: bool,

    /// Force the Flutter product path. Auto-detected from the APK layout
    /// (`libflutter.so` / `flutter_assets`) when omitted.
    #[arg(long)]
    pub flutter: bool,

    /// Force the React Native product path. Auto-detected from the APK layout
    /// (`libreactnative*`/`libhermes.so` + `assets/index.android.bundle`) when
    /// omitted.
    #[arg(long)]
    pub rn: bool,

    /// Execution engine. `art` runs the APK's **own** launcher Activity on
    /// Google ART — the same path a plain Android app takes, including the
    /// APK's `lib/<abi>/` native libraries — so a React Native / Flutter APK
    /// boots its own `MainApplication`/`ReactHost`/Hermes instead of the frozen
    /// Rust VM's hand-written drivers. Omitted keeps today's routing: the
    /// daemon default, with RN/Flutter APKs on the native-child route.
    #[arg(long)]
    pub engine: Option<String>,

    /// Keep the session alive in the `acos serve` daemon after launching, so
    /// separate `acos observe/tap/logs` invocations see the same live session.
    #[arg(long)]
    pub keep: bool,

    /// Override the display density (dpi) the client sees. `--density 400`
    /// lays ACOS out at 1080x2400 @ 400 dpi (the moto g35 under test) instead
    /// of the API-34 emulator's 420 dpi, so geometry can be compared on the
    /// same display configuration.
    #[arg(long)]
    pub density: Option<u32>,

    /// Override the logical display size, `WIDTHxHEIGHT` (e.g. `1080x2400`).
    #[arg(long)]
    pub display: Option<String>,

    /// Reference mode: run with the test phone's exact display parameters
    /// (`1080x2400 @ 400 dpi`, 108 px status bar, gesture navigation).
    #[arg(long)]
    pub reference: bool,

    /// W7: app-window inset model. `carve` (default) is the pre-W7 shim whose
    /// app frame is the display application area and whose reported insets are
    /// therefore zero. `report` gives the app the full-display window frame and
    /// reports status/navigation/system-gesture insets, which DecorView (non-
    /// edge-to-edge apps) or the app's own padding (edge-to-edge apps) then
    /// consumes. `--engine art` forwards it to the ART guest and its WMS peer.
    #[arg(long, value_name = "carve|report")]
    pub insets: Option<String>,

    /// MPROC1: install this APK into the session beside the launched one
    /// (repeatable). Each extra package gets its own uid and its own guest
    /// paths, and is reachable through `PackageManager` /
    /// `createPackageContext` — the way a second app exists on the device.
    ///
    /// ART only: the frozen Rust VM loads exactly one APK and refuses rather
    /// than pretending a second package is installed.
    #[arg(long = "extra-apk", value_name = "APK")]
    pub extra_apk: Vec<PathBuf>,

    /// P12: framework animation scales for an ART session (`on`/`off`).
    ///
    /// `off` sets Android's three animation scales to 0 for functional checks
    /// (standard CI practice); it is not for animation/transition testing.
    /// Launch-time only, and ART-only: the in-process Rust VM has no framework
    /// animators, so it refuses `off` rather than pretending.
    #[arg(long, value_name = "on|off")]
    pub animations: Option<String>,

    /// MPROC7: how long a lazily spawned secondary ART process may sit idle
    /// (no started activity, no started/bound service, no visible window)
    /// before the launcher kills it; the next intent respawns it. `0` disables
    /// the idle kill. Launch-time only, ART sessions only.
    #[arg(long, value_name = "SECS")]
    pub idle_kill_seconds: Option<u64>,
}

/// Build the display override for `acos launch` from its flags.
///
/// `--reference` selects the full phone configuration; `--density` /
/// `--display` change one axis while keeping the API-34 reference geometry.
fn display_override_from_args(args: &LaunchArgs) -> Option<acos_runtime::DisplayOverride> {
    if args.reference {
        return Some(acos_runtime::DisplayOverride::phone());
    }
    if args.density.is_none() && args.display.is_none() {
        return None;
    }
    let mut o = acos_runtime::DisplayOverride {
        width: 1080,
        height: 2400,
        density_dpi: 420,
        status_bar_top: 128,
        nav_bar_bottom: 63,
    };
    if let Some(d) = args.density {
        o.density_dpi = d as i32;
    }
    if let Some(spec) = &args.display {
        let (w, h) = spec.split_once('x').unwrap_or((spec.as_str(), ""));
        if let (Ok(w), Ok(h)) = (w.parse::<i32>(), h.parse::<i32>()) {
            o.width = w;
            o.height = h;
        }
    }
    Some(o)
}

/// Structurally detect a Flutter APK (engine `.so` and/or `flutter_assets`).
pub fn detect_flutter(apk: &std::path::Path) -> bool {
    matches!(acos_jni::flutter::detect(apk), Ok(Some(_)))
}

/// Structurally detect a React Native APK (RN/Hermes `.so` + JS bundle).
pub fn detect_react_native(apk: &std::path::Path) -> bool {
    matches!(acos_jni::react_native::detect(apk), Ok(Some(_)))
}

/// Run `acos launch`.
pub fn run(args: LaunchArgs) -> ExitCode {
    let request_id = new_request_id();
    let display = display_override_from_args(&args);
    acos_runtime::set_display_override(display);
    match display {
        Some(o) => std::env::set_var("ACOS_DISPLAY_OVERRIDE", o.spec()),
        None => std::env::remove_var("ACOS_DISPLAY_OVERRIDE"),
    }
    // W7: the inset selector is an environment contract for the WMS shim and
    // the ART guest; `--insets` is the CLI spelling. Validate here so a typo
    // never silently falls back to `carve`.
    if let Some(value) = args.insets.as_deref() {
        if acos_services::wms::parse_insets_mode(value).is_none() {
            eprintln!("launch: --insets must be \"carve\" or \"report\" (got {value:?})");
            return ExitCode::FAILURE;
        }
        std::env::set_var("ACOS_WM_INSETS", value);
    }
    // The engine is explicit and never inferred from the engine the last
    // command used. `art` means: run the APK's own launcher Activity on Google
    // ART, exactly like a plain Android app (the APK's `lib/<abi>/` native
    // closure included). That is a *different* launch from the frozen Rust VM's
    // RN/Flutter drivers, so the RN/Flutter product-path flags do not apply.
    let engine = match args.engine.as_deref() {
        None => None,
        Some(value) => match Engine::parse(value) {
            Some(engine) => Some(engine),
            None => return reject_engine(&request_id, &args, value),
        },
    };
    let art = engine == Some(Engine::Art);
    if art && crate::native_launch::native_requested(args.native) {
        return reject_engine_conflict(&request_id, &args);
    }
    // MPROC1: `--extra-apk` installs packages into an ART session. The Rust VM
    // runs one APK and has no package manager, so it refuses instead of
    // accepting the flag and installing nothing.
    if !args.extra_apk.is_empty() && !art {
        eprintln!(
            "launch: --extra-apk installs packages into an ART session; add \n\
             `--engine art` (the Rust VM runs one APK and would silently install nothing)."
        );
        return ExitCode::FAILURE;
    }
    // A Flutter APK can only be driven by the confined child, so it always
    // routes through the daemon even without `--keep`: the session must outlive
    // this process for `acos observe`/`tap` to address it. `--engine art` also
    // needs the daemon: the ART guest is its own process, not this one.
    let flutter = !art && (args.flutter || detect_flutter(&args.apk));
    let rn = !art && (args.rn || detect_react_native(&args.apk));
    if let Some(value) = args.animations.as_deref() {
        if !value.eq_ignore_ascii_case("on") && !value.eq_ignore_ascii_case("off") {
            eprintln!("launch: animations must be \"on\" or \"off\" (got {value:?})");
            return ExitCode::FAILURE;
        }
        if value.eq_ignore_ascii_case("off") && !(args.keep || flutter || rn || art) {
            eprintln!(
                "launch: animations:off is an ART-session setting; `acos launch` without \
                 --keep/--engine art/--flutter/--rn runs the in-process Rust VM, which has \
                 no framework animators. Use `acos launch --keep --animations off` or \
                 `acos verify --engine art`."
            );
            return ExitCode::FAILURE;
        }
    }
    if args.keep || flutter || rn || art {
        // The ABI gate (UNSUPPORTED_ABI, ADR-0018 W59-A) must run here too: the
        // daemon route's own backends report an ABI miss as WRONG_ABI. Only the
        // parse-and-check runs in this process; confinement is never entered
        // here, so the daemon spawn is still the first execution.
        if !art && crate::native_launch::native_requested(args.native) {
            if let Err(err) = crate::native_launch::check_apk(&args.apk) {
                return reject_native_gate(&request_id, &args, &err);
            }
        }
        return run_keep(&args, &request_id, flutter, rn, engine);
    }
    if crate::native_launch::native_requested(args.native) {
        if let Err(err) = gate_native(&args.apk) {
            return reject_native_gate(&request_id, &args, &err);
        }
    }
    let native = crate::native_launch::native_requested(args.native);
    let session = AgentSession::start(SessionOptions {
        wall_secs: args.timeout,
        native,
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

/// Launch through the `acos serve` daemon and leave the session running.
fn run_keep(
    args: &LaunchArgs,
    request_id: &str,
    flutter: bool,
    rn: bool,
    engine: Option<Engine>,
) -> ExitCode {
    let display = display_override_from_args(args);
    let native = crate::native_launch::native_requested(args.native) || flutter || rn;
    let mut client = match crate::serve::ensure_daemon() {
        Ok(client) => client,
        Err(e) => {
            let envelope = daemon_failure(request_id, e);
            return finish_daemon(&envelope, args.json);
        }
    };
    // The daemon's path check rejects `..`, so send a canonical absolute path.
    let apk = std::fs::canonicalize(&args.apk).unwrap_or_else(|_| args.apk.clone());
    let mut daemon_args = json!({
        "base": {"request_id": request_id, "session_id": "s_cli"},
        "apk": apk.display().to_string(),
        "native": native,
        "flutter": flutter,
        "rn": rn,
        "display": display.map(|o| o.spec()),
        "insets": args.insets.clone(),
        "animations": args.animations.clone(),
        "extra_apks": args
            .extra_apk
            .iter()
            .map(|path| json!(path.display().to_string()))
            .collect::<Vec<_>>(),
        "idle_kill_seconds": args.idle_kill_seconds,
        "timeout_ms": args.timeout.saturating_mul(1000),
    });
    // The engine is forwarded verbatim: the daemon must answer with the engine
    // that actually ran and must never fall back to another one.
    if let Some(engine) = engine {
        daemon_args["engine"] = json!(engine.as_str());
    }
    let envelope = match client.call("acos_launch", &daemon_args) {
        Ok(envelope) => envelope,
        Err(e) => daemon_failure(request_id, format!("session daemon: {e}")),
    };
    finish_daemon(&envelope, args.json)
}

/// Reject an `--engine` value this build does not have (no aliases, no guesses).
fn reject_engine(request_id: &str, args: &LaunchArgs, value: &str) -> ExitCode {
    let envelope = Envelope::failure(
        request_id,
        ProtocolBackend::AcosNative,
        Fidelity::NativeRuntime,
        ProtocolError::new(
            ErrorCode::InvalidArgument,
            format!("unknown --engine value {value:?}"),
        )
        .with_hint(
            "Use `--engine art` (the APK's own Activity on Google ART) or `--engine rust-vm` \
             (the frozen Rust VM). There is no silent fallback between engines.",
        )
        .with_details(json!({"field": "engine", "value": value})),
        Metrics::wall_ms(0),
    );
    finish_daemon(&envelope, args.json)
}

/// `--engine art` runs the APK's own process; it cannot be combined with the
/// Rust loader's `--native` selection.
fn reject_engine_conflict(request_id: &str, args: &LaunchArgs) -> ExitCode {
    let envelope = Envelope::failure(
        request_id,
        ProtocolBackend::AcosNative,
        Fidelity::NativeRuntime,
        ProtocolError::new(
            ErrorCode::InvalidArgument,
            "--engine art cannot be combined with --native",
        )
        .with_hint(
            "`--native` selects the Rust loader inside the ACOS runtime; ART has its own process \
             and loads the APK's libraries itself. Run them separately.",
        )
        .with_details(json!({"field": "engine", "value": "art"})),
        Metrics::wall_ms(0),
    );
    finish_daemon(&envelope, args.json)
}

fn daemon_failure(request_id: &str, message: String) -> Envelope {
    Envelope::failure(
        request_id,
        ProtocolBackend::AcosNative,
        Fidelity::NativeRuntime,
        ProtocolError::new(ErrorCode::Internal, message).with_hint(
            "The session daemon could not be started or answered with a broken response. \
             Check ACOS_NO_DAEMON / the daemon log; this is a harness failure, not app behaviour.",
        ),
        Metrics::wall_ms(0),
    )
}

fn finish_daemon(envelope: &Envelope, json: bool) -> ExitCode {
    if json {
        crate::print_json(envelope);
    } else if let Some(result) = envelope.result() {
        print_daemon_launch(result);
    } else if let Some(error) = envelope.error() {
        eprintln!("launch: {}: {}", error.code, error.message);
    }
    if envelope.status().is_pass() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

fn print_daemon_launch(result: &Value) {
    let reached = result
        .get("reached_stage")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let classes = result
        .get("classes_loaded")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let wall = result.get("wall_ms").and_then(Value::as_u64).unwrap_or(0);
    println!("launch: reached {reached} (classes={classes}, wall={wall}ms)");
    if let Some(package) = result.get("package").and_then(Value::as_str) {
        println!("  package: {package}");
    }
    if let Some(stages) = result.get("stages").and_then(Value::as_array) {
        for stage in stages {
            let name = stage.get("stage").and_then(Value::as_str).unwrap_or("");
            let status = stage.get("status").and_then(Value::as_str).unwrap_or("");
            println!("  [{status:>11}] {name}");
            if let Some(blocker) = stage.get("blocker") {
                let code = blocker.get("code").and_then(Value::as_str).unwrap_or("");
                let message = blocker.get("message").and_then(Value::as_str).unwrap_or("");
                println!("      {code}: {}", first_line(message));
            }
        }
    }
}

/// Report a native-gate rejection (`UNSUPPORTED_ABI`, `CONFINEMENT_UNAVAILABLE`,
/// ...) as the launch envelope and fail.
fn reject_native_gate(request_id: &str, args: &LaunchArgs, err: &SessionError) -> ExitCode {
    let envelope = session_error_envelope(request_id, err);
    if args.json {
        crate::print_json(&envelope);
    } else {
        eprintln!("launch: {}: {}", err.code, err.message);
    }
    ExitCode::FAILURE
}

/// ABI-check the APK and, when it ships host-ABI native code, confine the
/// process before the session worker is spawned.
fn gate_native(apk: &std::path::Path) -> Result<(), SessionError> {
    let (_apk, has_native) = crate::native_launch::check_apk(apk)?;
    if has_native {
        crate::native_launch::confine_native_launch(apk)?;
    }
    Ok(())
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
    if let Some(hint) = acos_runtime::session::session_error_hint(err.code, &err.details) {
        error = error.with_hint(hint);
    } else if ErrorCode::from_str_opt(&err.code).is_none() {
        error = error.with_hint(format!(
            "The runtime reported `{}`, which is not in the registered taxonomy. Register it \
             in acos_protocol::ErrorCode; until then this is a named harness gap, not app behaviour.",
            err.code
        ));
    }
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
                // FB5: `ms` is present on every rung. This ladder evaluates its
                // rungs from one lifecycle report at the end of the launch, so
                // `ok` rungs carry the launch wall time as their observation
                // time and unproven rungs carry `null` — `ms_basis` says so.
                "ms": if stage.status == StageStatus::Ok { Some(report.wall_ms) } else { None },
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
        "total_ms": report.wall_ms,
        "ms_basis": "observed_at_end",
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
