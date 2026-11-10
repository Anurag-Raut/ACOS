//! JSON Schema generation (checked in under `crates/acos-protocol/schema/`).
//!
//! The checked-in schemas are the public machine contract for the typed Rust
//! types. `cargo test -p acos-protocol` fails when they are stale; refresh them
//! with `UPDATE_SCHEMA=1 cargo test -p acos-protocol`.

use schemars::JsonSchema;
use serde_json::Value;

use crate::artifacts::ArtifactValidation;
use crate::commands::{
    ActionRequest, AssertRequest, CapabilityReportRequest, CaptureRequest, CommandRequest,
    DestroyRequest, EventsRequest, InspectRequest, ObserveRequest, ProfileRequest, ResetRequest,
    RunDexRequest, RunRequest, VerifyRequest,
};
use crate::envelope::Envelope;
use crate::error::ProtocolError;
use crate::events::Event;
use crate::scenario::Scenario;
use crate::ui::UiTree;
use crate::verdict::Verdict;

/// Generate the schema of any protocol type as a JSON value.
pub fn schema_value<T: JsonSchema>() -> Value {
    serde_json::to_value(schemars::schema_for!(T)).expect("schema is serializable")
}

/// The exact text written to `schema/<name>`.
pub fn schema_text<T: JsonSchema>() -> String {
    let mut text =
        serde_json::to_string_pretty(&schema_value::<T>()).expect("schema is serializable");
    text.push('\n');
    text
}

/// All checked-in schema documents, in stable name order.
pub fn schema_documents() -> Vec<(&'static str, Value)> {
    vec![
        ("action-request.json", schema_value::<ActionRequest>()),
        (
            "artifacts-validation.json",
            schema_value::<ArtifactValidation>(),
        ),
        ("assert-request.json", schema_value::<AssertRequest>()),
        (
            "capability-report-request.json",
            schema_value::<CapabilityReportRequest>(),
        ),
        ("capture-request.json", schema_value::<CaptureRequest>()),
        ("commands.json", schema_value::<CommandRequest>()),
        ("destroy-request.json", schema_value::<DestroyRequest>()),
        ("envelope.json", schema_value::<Envelope>()),
        ("event.json", schema_value::<Event>()),
        ("events-request.json", schema_value::<EventsRequest>()),
        ("inspect-request.json", schema_value::<InspectRequest>()),
        ("observe-request.json", schema_value::<ObserveRequest>()),
        ("profile-request.json", schema_value::<ProfileRequest>()),
        ("protocol-error.json", schema_value::<ProtocolError>()),
        ("reset-request.json", schema_value::<ResetRequest>()),
        ("run-dex-request.json", schema_value::<RunDexRequest>()),
        ("run-request.json", schema_value::<RunRequest>()),
        ("scenario.json", schema_value::<Scenario>()),
        ("ui-tree.json", schema_value::<UiTree>()),
        ("verdict.json", schema_value::<Verdict>()),
        ("verify-request.json", schema_value::<VerifyRequest>()),
    ]
}
