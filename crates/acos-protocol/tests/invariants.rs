//! Contract invariants: no false passes, honest fidelity, no stale frames.

use acos_protocol::envelope::{Backend, Envelope, Fidelity, Metrics, Status};
use acos_protocol::error::{ErrorCode, ProtocolError};
use acos_protocol::frame::FrameMeta;
use acos_protocol::identity::DisplaySpec;

fn failure(code: ErrorCode) -> Envelope {
    Envelope::failure(
        "req_1",
        Backend::AcosNative,
        Fidelity::NativeRuntime,
        ProtocolError::new(code, "detail"),
        Metrics::wall_ms(1),
    )
}

#[test]
fn ok_is_equivalent_to_pass_for_every_status() {
    for status in [
        Status::Pass,
        Status::Fail,
        Status::Error,
        Status::Unsupported,
        Status::Crash,
        Status::Timeout,
        Status::Blocked,
    ] {
        assert_eq!(status.is_pass(), status == Status::Pass);
        assert_eq!(status.is_failure(), status != Status::Pass);
    }
}

#[test]
fn failure_constructors_map_codes_to_honest_statuses() {
    let cases = [
        (ErrorCode::AssertionFailed, Status::Fail),
        (ErrorCode::ElementNotFound, Status::Fail),
        (ErrorCode::AppCrashed, Status::Crash),
        (ErrorCode::AppNotResponding, Status::Timeout),
        (ErrorCode::Timeout, Status::Timeout),
        (ErrorCode::SessionBusy, Status::Blocked),
        (ErrorCode::ResourceLimit, Status::Blocked),
        (ErrorCode::IsolationUnavailable, Status::Blocked),
        (ErrorCode::UnsupportedJavaApi, Status::Unsupported),
        (ErrorCode::UnsupportedNativeSymbol, Status::Unsupported),
        (ErrorCode::NeedsRealDevice, Status::Unsupported),
        (ErrorCode::RequiresGms, Status::Unsupported),
        (ErrorCode::WrongAbi, Status::Unsupported),
        (ErrorCode::NoFreshFrame, Status::Error),
        (ErrorCode::UnverifiedFidelity, Status::Error),
        (ErrorCode::InvalidArgument, Status::Error),
        (ErrorCode::ArtifactsInvalid, Status::Error),
    ];
    for (code, status) in cases {
        let envelope = failure(code);
        assert_eq!(envelope.status(), status, "{code}");
        assert!(!envelope.ok(), "{code}");
        assert_eq!(envelope.error().unwrap().code, code);
        assert!(envelope.validate().is_ok());
    }
}

#[test]
fn unsupported_is_never_a_pass() {
    for code in [
        ErrorCode::UnsupportedAndroidApi,
        ErrorCode::UnsupportedJavaApi,
        ErrorCode::UnsupportedBytecode,
        ErrorCode::UnsupportedNativeSymbol,
        ErrorCode::NeedsRealDevice,
        ErrorCode::RequiresGms,
        ErrorCode::WrongAbi,
        ErrorCode::NoDisplay,
        ErrorCode::NoText,
        ErrorCode::UnsupportedBackend,
    ] {
        let envelope = failure(code);
        assert!(!envelope.ok());
        assert_eq!(envelope.status(), Status::Unsupported);
        assert_ne!(envelope.status(), Status::Pass);
    }
}

#[test]
fn envelope_deserialization_cannot_create_a_false_pass() {
    let cases = [
        // ok=true + fail status
        serde_json::json!({
            "request_id": "req", "ok": true, "status": "fail",
            "backend": "acos-native", "fidelity": "native-runtime",
            "capabilities_used": [], "metrics": {"wall_ms": 1},
            "error": {"code": "ASSERTION_FAILED", "message": "x", "hint": "y", "retryable": false}
        }),
        // ok=true + pass status + error
        serde_json::json!({
            "request_id": "req", "ok": true, "status": "pass",
            "backend": "acos-native", "fidelity": "native-runtime",
            "capabilities_used": [], "metrics": {"wall_ms": 1},
            "error": {"code": "TIMEOUT", "message": "x", "hint": "y", "retryable": true}
        }),
        // ok=false + no error
        serde_json::json!({
            "request_id": "req", "ok": false, "status": "fail",
            "backend": "acos-native", "fidelity": "native-runtime",
            "capabilities_used": [], "metrics": {"wall_ms": 1}
        }),
        // unsupported code reported as ordinary error
        serde_json::json!({
            "request_id": "req", "ok": false, "status": "error",
            "backend": "acos-native", "fidelity": "native-runtime",
            "capabilities_used": [], "metrics": {"wall_ms": 1},
            "error": {"code": "UNSUPPORTED_BYTECODE", "message": "x", "hint": "y", "retryable": false}
        }),
    ];
    for case in cases {
        assert!(
            serde_json::from_value::<Envelope>(case.clone()).is_err(),
            "accepted invalid envelope {case}"
        );
    }
}

#[test]
fn fidelity_semantics_are_explicit() {
    assert!(!Fidelity::NativeRuntime.is_android_verified());
    assert!(Fidelity::ArtReference.is_android_verified());
    assert!(Fidelity::PhysicalDevice.is_android_verified());
    assert!(!Fidelity::JvmReference.is_android_verified());
    assert!(!Fidelity::Simulated.is_android_verified());

    // A JVM pass can never satisfy an ART requirement (ADR-0005).
    assert!(!Fidelity::JvmReference.satisfies(Fidelity::ArtReference));
    assert!(!Fidelity::NativeRuntime.satisfies(Fidelity::ArtReference));
    assert!(Fidelity::ArtReference.satisfies(Fidelity::ArtReference));
    assert!(Fidelity::NativeRuntime.satisfies(Fidelity::NativeRuntime));
    assert!(Fidelity::ArtReference.satisfies(Fidelity::JvmReference));
    assert!(Fidelity::StaticAnalysis.satisfies(Fidelity::StaticAnalysis));
    assert!(!Fidelity::StaticAnalysis.satisfies(Fidelity::NativeRuntime));
    assert!(!Fidelity::Simulated.satisfies(Fidelity::NativeRuntime));
    assert!(Fidelity::NativeRuntime
        .description()
        .contains("not validated against ART"));
}

#[test]
fn frames_cannot_be_reused_after_the_requested_sequence() {
    let frame = FrameMeta::new(
        19,
        "2026-11-10T13:51:14Z",
        "req_19",
        1080,
        2400,
        420,
        "hash",
        true,
    );
    assert!(frame.is_fresh_after(18));
    assert!(!frame.is_fresh_after(19));
    assert!(!frame.is_fresh_after(20));
}

#[test]
fn envelope_helpers_preserve_identity_and_evidence() {
    let envelope = Envelope::pass(
        "req_27",
        Backend::AcosNative,
        Fidelity::NativeRuntime,
        Metrics::wall_ms(42),
    )
    .with_session_id("s_1")
    .with_isolation(acos_protocol::IsolationLevel::Sandbox)
    .with_frame(FrameMeta::new(
        19,
        "2026-11-10T13:51:14Z",
        "req_27",
        1080,
        2400,
        420,
        "hash",
        true,
    ))
    .with_capabilities(["lifecycle", "ui.offscreen"])
    .with_result(serde_json::json!({"ok": true}))
    .unwrap();
    assert_eq!(envelope.request_id(), "req_27");
    assert_eq!(envelope.session_id(), Some("s_1"));
    assert_eq!(envelope.frame().unwrap().frame_seq, 19);
    assert_eq!(envelope.capabilities_used(), ["lifecycle", "ui.offscreen"]);
    assert_eq!(envelope.metrics().wall_ms, 42);
    assert!(envelope.validate().is_ok());
    assert_eq!(DisplaySpec::None.label(), "none");
}
