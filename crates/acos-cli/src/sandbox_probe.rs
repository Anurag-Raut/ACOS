//! `acos sandbox-probe` — report confinement capabilities and which layers
//! are actually active on this host.
//!
//! The static capabilities are read from `/proc`/`/sys`; the `live` object
//! comes from forking a child that applies the profile and reports the layer
//! outcomes. The command always emits the standard ACOS JSON envelope.

use std::process::ExitCode;

use acos_protocol::{Backend, Envelope, Fidelity, Metrics};
use serde_json::{json, Value};

use crate::new_request_id;

/// Run `acos sandbox-probe`.
pub fn run() -> ExitCode {
    let request_id = new_request_id();
    let caps = acos_sandbox::probe();
    let systemd_scope = acos_sandbox::systemd_user_scope_available();

    let scratch = std::env::temp_dir().join(format!("acos-sandbox-probe-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&scratch);
    let profile = acos_sandbox::ConfineProfile::probe(scratch.clone()).with_namespaces();
    let live = acos_sandbox::probe_live(&profile);
    let _ = std::fs::remove_dir_all(&scratch);

    let live: Value = match live {
        Ok(report) => json!(report),
        Err(e) => json!({"error": e.to_string(), "code": e.code()}),
    };

    let result = json!({
        "capabilities": caps,
        "systemd_user_scope": systemd_scope,
        "live": live,
    });

    let envelope = Envelope::pass(
        &request_id,
        Backend::AcosNative,
        Fidelity::NativeRuntime,
        Metrics::wall_ms(0),
    )
    .with_value_result(result)
    .with_capabilities(["sandbox.probe"]);
    crate::print_json(&envelope);
    ExitCode::SUCCESS
}
