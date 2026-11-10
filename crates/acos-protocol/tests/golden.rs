//! Golden envelopes: the existing `inspect` shape and a structured unsupported.

use acos_protocol::envelope::{Backend, Envelope, Fidelity, Metrics};
use acos_protocol::error::{ErrorCode, ProtocolError};

fn assert_golden(envelope: &Envelope, golden: &str) {
    let actual = serde_json::to_value(envelope).expect("serialize envelope");
    let expected: serde_json::Value = serde_json::from_str(golden).expect("golden is JSON");
    assert_eq!(actual, expected, "golden mismatch\nactual: {actual:#}");
}

#[test]
fn inspect_pass_envelope_is_golden() {
    let envelope = Envelope::pass(
        "req_1",
        Backend::HostTooling,
        Fidelity::StaticAnalysis,
        Metrics::wall_ms(3),
    )
    .with_capabilities(["apk.inspect"])
    .with_value_result(serde_json::json!({
        "package": "dev.acos.fixtures.helloviews",
        "launcher_activity": "dev.acos.fixtures.helloviews.MainActivity"
    }));
    assert_golden(&envelope, include_str!("golden/envelope-pass-inspect.json"));
}

#[test]
fn unsupported_envelope_is_golden() {
    let error = ProtocolError::new(
        ErrorCode::UnsupportedJavaApi,
        "java.lang.reflect.Proxy is not implemented (at Lx;->y)",
    )
    .with_details(serde_json::json!({
        "api": "java.lang.reflect.Proxy",
        "at": "Lx;->y"
    }));
    let envelope = Envelope::failure(
        "req_2",
        Backend::AcosNative,
        Fidelity::NativeRuntime,
        error,
        Metrics::wall_ms(4),
    )
    .with_capabilities(["dex.run"]);
    assert_golden(&envelope, include_str!("golden/envelope-unsupported.json"));
}

#[test]
fn a_pass_never_serializes_an_error_and_a_failure_never_serializes_ok() {
    let pass = Envelope::pass(
        "req_1",
        Backend::AcosNative,
        Fidelity::NativeRuntime,
        Metrics::wall_ms(1),
    );
    let value = serde_json::to_value(&pass).unwrap();
    assert_eq!(value["ok"], true);
    assert_eq!(value["status"], "pass");
    assert!(value.get("error").is_none());

    let fail = Envelope::failure(
        "req_2",
        Backend::AcosNative,
        Fidelity::NativeRuntime,
        ProtocolError::new(ErrorCode::AppCrashed, "uncaught"),
        Metrics::wall_ms(2),
    );
    let value = serde_json::to_value(&fail).unwrap();
    assert_eq!(value["ok"], false);
    assert_eq!(value["status"], "crash");
    assert_eq!(value["error"]["code"], "APP_CRASHED");
}
