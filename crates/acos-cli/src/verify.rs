//! `acos verify <scenario.yaml> [--apk ...]` — the scenario runner entry point.
//!
//! Default backend is the native ACOS runtime (static manifest + M2 attach
//! boot); every capability it does not yet have is reported as `unsupported`.
//! Passing `--reference <dir>` selects the ART reference replay backend, which
//! serves the checked-in `art-reference` evidence so the assertion logic and
//! false-pass prevention can be exercised on this host.

use std::path::{Path, PathBuf};
use std::sync::Arc;
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

    /// P12: framework animation scales for the ART session (`on`/`off`).
    ///
    /// `off` sets Android's three animation scales to 0 for functional checks
    /// (standard CI practice); it is never for animation/transition testing.
    /// Overrides a scenario's top-level `animations`. Launch-time only; the
    /// Rust VM has no framework animators and refuses `off`.
    #[arg(long, value_name = "on|off")]
    pub animations: Option<String>,

    /// W7b: app-window inset model for the ART session (`carve`/`report`).
    ///
    /// Overrides a scenario's top-level `insets`, which overrides
    /// `ACOS_WM_INSETS`. The build default is `report` (AOSP edge-to-edge,
    /// ADR-0026 decision 6); `carve` selects the pre-W7 shim for baselines
    /// captured against the carved frame. Launch-time only; the Rust VM has
    /// no window insets.
    #[arg(long, value_name = "carve|report")]
    pub insets: Option<String>,

    /// Treat this run as an untrusted APK.
    ///
    /// Untrusted is an explicit opt-in and is allowed by product policy
    /// (`docs/SANDBOX_CERTIFICATION.md` §8.8), because the sandbox is
    /// CERTIFIED-WITH-LIMITS (§8.6, limits in §8.7). In this mode tree-only and
    /// log-only assertions fail closed with `INSUFFICIENT_HOST_EVIDENCE`;
    /// host-observed region/screenshot assertions and host-injected input still
    /// run. `ACOS_ALLOW_UNTRUSTED_FOR_TESTS=1` is accepted but redundant.
    /// Equivalent to a scenario `trust: untrusted`.
    #[arg(long)]
    pub untrusted: bool,

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

    let trust = match crate::trust::resolve(args.untrusted, &scenario) {
        Ok(trust) => trust,
        Err(detail) => {
            let envelope = Envelope::failure(
                request_id,
                ProtocolBackend::AcosNative,
                Fidelity::StaticAnalysis,
                ProtocolError::new(ErrorCode::InvalidScenario, detail),
                Metrics::wall_ms(0),
            );
            // The run was never classified; the envelope still carries the
            // certification record (`trust: unresolved`).
            return finish_run(&envelope, &args, &artifacts, None);
        }
    };
    // Product policy (docs/SANDBOX_CERTIFICATION.md §8.8): untrusted is opt-in
    // and allowed while `CERTIFICATION` says the sandbox is certified with
    // limits. The historical test override is accepted and redundant. Only a
    // future downgrade (`Certification::NotCertified`) produces
    // `UNTRUSTED_APK_NOT_CERTIFIED` again.
    if let Some(refusal) = crate::trust::untrusted_gate(trust, crate::trust::CERTIFICATION) {
        let envelope = Envelope::failure(
            request_id,
            ProtocolBackend::AcosNative,
            Fidelity::StaticAnalysis,
            refusal,
            Metrics::wall_ms(0),
        );
        return finish_run(&envelope, &args, &artifacts, Some(trust));
    }

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
        return finish_run(&envelope, &args, &artifacts, Some(trust));
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
        return finish_run(&envelope, &args, &artifacts, Some(trust));
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
        return finish_run(&envelope, &args, &artifacts, Some(trust));
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
                return finish_run(&envelope, &args, &artifacts, Some(trust));
            }
        },
    };

    // P12: the session animation choice (CLI overrides the scenario). The
    // scenario is parsed here too because the CLI's `scenario` is still a path;
    // the verifier parses it again for its own validation.
    let scenario_animations = acos_protocol::scenario::Scenario::load(&scenario)
        .ok()
        .and_then(|scenario| scenario.animations);
    let animations: &'static str = match args
        .animations
        .as_deref()
        .or(scenario_animations.as_deref())
    {
        None => "on",
        Some(value) => match value.trim().to_ascii_lowercase().as_str() {
            "on" => "on",
            "off" => "off",
            other => {
                let envelope = Envelope::failure(
                    request_id,
                    ProtocolBackend::AcosNative,
                    Fidelity::StaticAnalysis,
                    ProtocolError::new(
                        ErrorCode::InvalidArgument,
                        format!("animations must be \"on\" or \"off\" (got {other:?})"),
                    )
                    .with_hint("`animations: off` sets Android's three animation scales to 0 for functional checks; it is not for animation testing."),
                    Metrics::wall_ms(0),
                );
                return finish_run(&envelope, &args, &artifacts, Some(trust));
            }
        },
    };
    if engine == Engine::RustVm && animations == "off" {
        let envelope = Envelope::failure(
            request_id,
            ProtocolBackend::AcosNative,
            Fidelity::StaticAnalysis,
            ProtocolError::new(
                ErrorCode::UnsupportedBackend,
                "animations:off is an ART-engine setting",
            )
            .with_hint("The Rust VM has no Android framework animators to scale. Use `--engine art` for a real framework session."),
            Metrics::wall_ms(0),
        );
        return finish_run(&envelope, &args, &artifacts, Some(trust));
    }

    // W7b: the inset model is a self-contained per-scenario choice. CLI >
    // scenario > environment > build default, and the resolved value is
    // installed in the process environment before the ART session starts so
    // the guest, the WMS shim and the verifier's frame metadata all agree.
    // Without this, a `carve` scenario would inherit the `report` default and
    // its negative assertion could pass for the wrong model (a false pass).
    let scenario_insets = acos_protocol::scenario::Scenario::load(&scenario)
        .ok()
        .and_then(|scenario| scenario.insets);
    let requested_insets = args.insets.clone().or(scenario_insets);
    let insets_owned: String = match requested_insets {
        Some(value) => match value.trim().to_ascii_lowercase().as_str() {
            "carve" => "carve".to_string(),
            "report" => "report".to_string(),
            other => {
                let envelope = Envelope::failure(
                    request_id,
                    ProtocolBackend::AcosNative,
                    Fidelity::StaticAnalysis,
                    ProtocolError::new(
                        ErrorCode::InvalidArgument,
                        format!("insets must be \"carve\" or \"report\" (got {other:?})"),
                    )
                    .with_hint("`insets: report` is AOSP's full-display window with reported system-bar insets; `insets: carve` is the pre-W7 application-area frame."),
                    Metrics::wall_ms(0),
                );
                return finish_run(&envelope, &args, &artifacts, Some(trust));
            }
        },
        None => std::env::var(acos_services::wms::INSETS_ENV)
            .unwrap_or_else(|_| acos_services::wms::DEFAULT_INSETS_MODE.as_str().to_string()),
    };
    std::env::set_var(acos_services::wms::INSETS_ENV, &insets_owned);

    let request = VerifyRequest {
        base: RequestBase::new(request_id.clone()),
        apk: args.apk.as_ref().map(|path| path.display().to_string()),
        scenario: scenario.display().to_string(),
        artifacts: artifacts.display().to_string(),
        reference: args
            .reference
            .as_ref()
            .map(|path| path.display().to_string()),
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
                return finish_run(&envelope, &args, &artifacts, Some(trust));
            }
        };
        // The verifier must still be able to write its evidence directory, so
        // that path is added to the confinement's writable root explicitly.
        if let Err(error) = crate::native_launch::confine_native_launch_with_writable(
            &apk,
            Some(&artifacts),
            Some(&scenario),
        ) {
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
            return finish_run(&envelope, &args, &artifacts, Some(trust));
        }
    }

    // Guest boot to its ready line happens before the executor's clock starts,
    // so it is recorded here and charged separately from the `launch` step.
    let mut session_boot_ms: Option<u64> = None;
    let mut session_animations: Option<&'static str> = None;
    let mut session_insets: Option<&'static str> = None;
    // H13b: the boot image the guest booted and whether its cache hit.
    let mut session_boot_image: Option<serde_json::Value> = None;
    let backend: EngineBackend = match engine {
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
                    return finish_run(&envelope, &args, &artifacts, Some(trust));
                }
            };
            // ZPROD3: a `verify` run uses the same engine path as `acos serve`.
            // With `ACOS_ART_ZYGOTE=on` that means a persistent-zygote child
            // (`start_with_zygote`); with the flag off the host is disabled and
            // this is exactly `start_with_options`. One host per run: its `Drop`
            // closes the keeper, so a verify run never leaves a zygote behind.
            let zygote = Arc::new(crate::engine::zygote_host::ZygoteHost::for_artifacts(
                &artifacts,
            ));
            match crate::engine::art::ArtSession::start_with_zygote(
                &apk,
                &artifacts,
                animations,
                &insets_owned,
                Some(zygote),
            ) {
                Ok(session) => {
                    session_boot_ms = Some(session.session_start_ms());
                    session_animations = Some(session.animations());
                    session_insets = Some(session.insets());
                    session_boot_image = session.boot_image().cloned();
                    EngineBackend::session(Box::new(session))
                }
                Err(error) => {
                    let mut protocol = error.to_protocol_error();
                    // W46: a session that refused to start still leaves its own
                    // record (full disk, seccomp, panic). Put those facts in the
                    // envelope instead of only a detail string.
                    crate::failure::attach_session_exit(&mut protocol, &artifacts);
                    let envelope = Envelope::failure(
                        request_id,
                        ProtocolBackend::AcosNative,
                        Fidelity::StaticAnalysis,
                        protocol,
                        Metrics::wall_ms(0),
                    );
                    return finish_run(&envelope, &args, &artifacts, Some(trust));
                }
            }
        }
    };
    let engine_label = backend.engine();
    let mode = backend.mode().unwrap_or(Mode::Live);
    let session_caps = backend.session_capabilities();
    let mut guarded = crate::trust::TrustGuard::new(Box::new(backend), trust);
    let mut envelope = acos_verify::verify(&request, &mut guarded);
    // W46: tear the session down *before* annotating, so the launcher's own
    // `session-exit.json` exists when the verdict artifacts are written. The
    // record is evidence for a pass as well as for a failure (a clean run ends
    // with guest_exit=0), and a caller that keeps only the artifacts keeps it.
    drop(guarded);
    annotate(
        &mut envelope,
        &args,
        engine_label,
        mode,
        session_caps.as_ref(),
        trust,
        &artifacts,
        session_boot_ms,
        session_animations,
        session_insets,
        session_boot_image,
    );
    finish_run(&envelope, &args, &artifacts, Some(trust))
}

/// Put the engine choice and mode into the JSON verdict, enforce that preview is
/// never a pass, and write the annotated verdict next to the verifier's one.
pub(crate) fn annotate(
    envelope: &mut Envelope,
    args: &VerifyArgs,
    engine: Option<Engine>,
    mode: Mode,
    caps: Option<&crate::engine::SessionCapabilities>,
    trust: crate::trust::Trust,
    artifacts: &Path,
    session_boot_ms: Option<u64>,
    session_animations: Option<&'static str>,
    session_insets: Option<&'static str>,
    session_boot_image: Option<serde_json::Value>,
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
                .filter(|(_, step)| {
                    step.get("status").and_then(|s| s.as_str()) == Some("unsupported")
                })
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
    let provenance = crate::provenance::run_evidence(trust);
    // W46: the session's own exit record belongs in the verdict artifacts, not
    // only in the engine's frames directory. A future death is then classified
    // from the verdict a caller already keeps.
    let session_exit = crate::failure::read_session_exit(artifacts);
    let session_exit_summary = session_exit
        .as_ref()
        .map(|doc| crate::failure::session_exit_summary(doc, artifacts));
    let mut timing = step_timing(&verdict, session_boot_ms);
    timing["boot_image"] = session_boot_image.unwrap_or(serde_json::Value::Null);
    let engine_block = serde_json::json!({
        "engine": engine.as_str(),
        "mode": mode.as_str(),
        "trust": trust.as_str(),
        "evidence_provenance": provenance["evidence_provenance"].clone(),
        "unsupported_steps": unsupported_steps,
        "evidence": evidence,
        "timing": timing,
        "session_exit": session_exit_summary,
        "commands": caps.map(|caps| serde_json::json!({
            "supported": caps.names(),
            "missing": caps.missing(),
        })),
        "explicit": args.engine != "rust-vm" || mode == Mode::Preview,
        "animations": session_animations.unwrap_or("on"),
        "insets": session_insets
            .unwrap_or(acos_services::wms::DEFAULT_INSETS_MODE.as_str()),
    });
    if let Some(object) = verdict.as_object_mut() {
        object.insert("engine".to_string(), engine_block["engine"].clone());
        object.insert("mode".to_string(), engine_block["mode"].clone());
        object.insert(
            "unsupported_steps".to_string(),
            engine_block["unsupported_steps"].clone(),
        );
        object.insert("evidence".to_string(), engine_block["evidence"].clone());
        object.insert(
            "animations".to_string(),
            engine_block["animations"].clone(),
        );
        object.insert("insets".to_string(), engine_block["insets"].clone());
        if let Some(summary) = &session_exit_summary {
            object.insert("session_exit".to_string(), summary.clone());
        }
    }
    crate::provenance::annotate_verdict(&mut verdict, trust);

    let preview_downgrade = mode == Mode::Preview && crate::engine::preview_downgrade(&mut verdict);
    // Evidence files: the annotated verdict (engine/mode fields are additive) and
    // a compact engine report. A write that fails for lack of host space is a
    // structured, retryable HOST_DISK_FULL — never a silent pass and never a
    // panic with an empty stderr (W43: nine such runs in the ENOSPC window).
    let mut write_error: Option<(std::io::Error, PathBuf)> = None;
    if let Err(error) = std::fs::create_dir_all(artifacts) {
        write_error = Some((error, artifacts.to_path_buf()));
    }
    if write_error.is_none() {
        let verdict_path = artifacts.join(acos_protocol::artifacts::VERDICT_JSON);
        match serde_json::to_string_pretty(&verdict) {
            Ok(text) => {
                if let Err(error) = crate::failure::write_artifact(&verdict_path, text.as_bytes()) {
                    write_error = Some((error, verdict_path));
                }
            }
            Err(error) => {
                write_error = Some((
                    std::io::Error::other(format!("serialize verdict: {error}")),
                    verdict_path,
                ));
            }
        }
    }
    if write_error.is_none() {
        let engine_path = artifacts.join("engine.json");
        let body = serde_json::json!({
            "request_id": envelope.request_id(),
            "engine": engine_block,
            "verdict_status": verdict.get("verdict"),
            "provenance_rule": provenance["rule"].clone(),
        });
        match serde_json::to_string_pretty(&body) {
            Ok(text) => {
                if let Err(error) = crate::failure::write_artifact(&engine_path, text.as_bytes()) {
                    write_error = Some((error, engine_path));
                }
            }
            Err(error) => {
                write_error = Some((
                    std::io::Error::other(format!("serialize engine report: {error}")),
                    engine_path,
                ));
            }
        }
    }
    if write_error.is_none() {
        crate::provenance::annotate_step_artifacts(&verdict, artifacts, trust);
    }
    let mut annotated_result = result;
    annotated_result["verdict"] = verdict;
    if let Some((error, path)) = write_error {
        let protocol = crate::failure::write_failure(&error, &path);
        *envelope = Envelope::failure(
            envelope.request_id().to_string(),
            envelope.backend(),
            envelope.fidelity(),
            protocol,
            envelope.metrics(),
        )
        .with_value_result(annotated_result);
    } else if preview_downgrade {
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

/// Per-step cost accounting for the engine report.
///
/// The verdict already carries `steps[].duration_ms` for every executed step.
/// What it cannot carry is the cost that happens outside a step: the guest boot
/// before the executor's clock starts (`session_start_ms`) and the part of the
/// verdict's own wall time that no step was charged with (the `assert_log`
/// wait-for-log poll, which runs before the step timer starts, and evidence
/// writes). Both are named here so a cost table has no unexplained residual.
fn step_timing(verdict: &serde_json::Value, session_boot_ms: Option<u64>) -> serde_json::Value {
    let steps: Vec<serde_json::Value> = verdict
        .get("steps")
        .and_then(|steps| steps.as_array())
        .map(|steps| {
            steps
                .iter()
                .map(|step| {
                    serde_json::json!({
                        "index": step.get("index"),
                        "step": step.get("step"),
                        "status": step.get("status"),
                        "wall_ms": step.get("duration_ms").and_then(|v| v.as_u64()).unwrap_or(0),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    let attributed: u64 = steps
        .iter()
        .map(|step| step.get("wall_ms").and_then(|v| v.as_u64()).unwrap_or(0))
        .sum();
    let wall_ms = verdict
        .pointer("/resources/wall_ms")
        .and_then(|v| v.as_u64());
    serde_json::json!({
        "unit": "ms",
        "steps": steps,
        "attributed_ms": attributed,
        "verdict_wall_ms": wall_ms,
        "unattributed_ms": wall_ms.map(|wall| wall.saturating_sub(attributed)),
        "session_start_ms": session_boot_ms,
        "note": "session_start_ms is the guest boot before the executor clock; \
                 unattributed_ms is verdict wall that no step timer covered \
                 (wait-for-log polling before a step starts, evidence writes)",
    })
}

fn finish_run(
    envelope: &Envelope,
    args: &VerifyArgs,
    artifacts: &Path,
    trust: Option<crate::trust::Trust>,
) -> ExitCode {
    finish(
        &crate::trust::stamp_envelope(envelope, trust),
        args,
        artifacts,
    )
}

fn finish(envelope: &Envelope, args: &VerifyArgs, artifacts: &Path) -> ExitCode {
    if args.json {
        print_json(envelope);
    } else {
        print_summary(envelope, artifacts);
    }
    if !envelope.status().is_pass() {
        // Always name the failure on stderr too: a caller that captures only
        // stderr must still see the code (W43: rc 101 with empty stderr).
        if let Some(error) = envelope.error() {
            eprintln!(
                "verify: {} ({}): {} [class={}, retryable={}]",
                error.code, error.class, error.message, error.class, error.retryable
            );
        }
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
        let engine = verdict
            .get("engine")
            .and_then(|v| v.as_str())
            .unwrap_or("-");
        let mode = verdict.get("mode").and_then(|v| v.as_str()).unwrap_or("-");
        println!("  engine: {engine} (mode: {mode})");
        if let Some(steps) = verdict.get("unsupported_steps").and_then(|v| v.as_array()) {
            for step in steps {
                println!(
                    "  unsupported: {} [{}]",
                    step.get("capability")
                        .and_then(|v| v.as_str())
                        .unwrap_or("unknown"),
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
        .map(|value| {
            value
                .trim()
                .trim_matches('"')
                .trim_matches('\'')
                .to_string()
        });
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
