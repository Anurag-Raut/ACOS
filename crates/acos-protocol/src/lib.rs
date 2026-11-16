//! # acos-protocol
//!
//! The agent-facing contract for ACOS. This crate is deliberately runtime-free:
//! it defines the typed request/response surface, the error taxonomy, the
//! scenario format, the artifacts/verdict layout and the MCP tool descriptors,
//! so runtime crates can implement against it and dogfood agents can use it.
//!
//! Start with [`docs/AGENT_API.md`](../../../docs/AGENT_API.md) in the repo, or
//! with [`Envelope`] + [`ErrorCode`] for the wire contract.
//!
//! Scenario YAML is parsed with `serde_yaml_ng` (the maintained fork of the
//! archived `serde_yaml`; `serde_yml` is deprecated by its own release notes).
//! JSON remains the only wire format for commands and envelopes.
//!
//! Key invariants:
//!
//! * `ok: true` is only representable with `status: pass`; a non-pass status
//!   always carries an error whose code maps to that status.
//! * `unsupported`, `crash`, `timeout` and `blocked` are never passes.
//! * frames are request/cause sequenced; a stale frame cannot satisfy a capture.
//! * a verdict claiming pass is invalid unless its artifacts exist.
//! * scenario validation reports every error with a path, not just the first.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

pub mod artifacts;
pub mod assertion;
pub mod commands;
pub mod diff;
pub mod envelope;
pub mod error;
pub mod events;
pub mod frame;
pub mod identity;
pub mod mcp;
pub mod provenance;
pub mod reply;
pub mod scenario;
pub mod schema;
pub mod ui;
pub mod verdict;

pub use artifacts::{
    frame_file_name, step_file_name, tree_file_name, validate_artifacts_dir, ArtifactIssue,
    ArtifactValidation, EVENTS_JSONL, FRAMES_DIR, REPRO_JSON, REPRO_SH, RESOURCES_JSON, STEPS_DIR,
    TREE_DIR, VERDICT_JSON,
};
pub use assertion::{
    Assertion, AssertionDiff, AssertionResult, LogAssertion, NoCrashAssertion, RegionMask,
    ScreenshotAssertion, StateAssertion, TextAssertion, VisibilityAssertion,
};
pub use commands::{
    Action, ActionRequest, ActionResponse, AssertRequest, AssertResponse, CapabilityCheck,
    CapabilityReport, CapabilityReportRequest, CapabilityStatus, CaptureFormat, CaptureRequest,
    CaptureResponse, CommandName, CommandRequest, CommandResponse, DataPolicy, DestroyRequest,
    DestroyResponse, EventsRequest, EventsResponse, InspectRequest, InspectResult, KeyName,
    ObserveRequest, ObserveResponse, Orientation, Point, ProfileRequest, ProfileResponse,
    RequestBase, ResetRequest, ResetResponse, ResetScope, RunDexRequest, RunDexResult, RunRequest,
    RunResponse, VerifyEngine, VerifyRequest, VerifyResponse,
};
pub use diff::{
    ChangeCounts, ChangedNode, DiffFrames, FieldChange, NodeRef, TreeChanges, MAX_DIFF_NODES,
};
pub use envelope::{Backend, Envelope, EnvelopeError, Evidence, Fidelity, Metrics, Status};
pub use error::{
    ErrorCode, ProtocolError, UnknownErrorCode, ALL_ERROR_CODES, EXISTING_CRATE_CODES,
};
pub use events::{
    seq_is_strictly_increasing, Event, EventBody, EventKind, LifecycleState, LogLevel,
};
pub use frame::FrameMeta;
pub use identity::{DisplaySpec, IsolationLevel, ResourceBudget};
pub use mcp::{mcp_tools, McpToolDescriptor, McpToolsList};
pub use reply::{
    compact_nodes, compact_text, enforce_cap, enforce_cap_pretty, shape_result_tree, shape_tree,
    write_full_tree, ShapedTree, TreeShapeOptions, DEFAULT_PAGE_SIZE, MAX_REPLY_BYTES,
};
pub use scenario::{
    validate_scenario, CaptureStep, DensitySetup, Engine, Expectation, FallbackPolicy, LaunchStep,
    LocaleSetup, NetworkRule, NetworkSetup, OutputOptions, PermissionSetup, PermissionState,
    ResetStep, RotateStep, Scenario, ScenarioError, ScenarioValidation, ScenarioWarning, Step,
    StepRef, TapStep, Theme, ThemeSetup, TimeSetup, TypeStep, WaitForStep, SCENARIO_VERSION,
};
pub use ui::{Bounds, Selector, TextMatch, UiNode, UiTree};
pub use verdict::{
    AppIdentity, ArtifactRef, FailureClassification, Reproducer, ResourceReport, ScenarioIdentity,
    StepAttempt, StepResult, Verdict, VERDICT_SCHEMA_VERSION,
};

/// The settle `reason` of an input whose every published event the app window
/// acknowledged `finishInputEvent(handled) = false`, with no draw (REL3H).
///
/// Nothing consumed the gesture, so there is no new state to settle into: the
/// unchanged, quiet screen the input produced **is** the settled observation.
/// The string lives here, not in the engine, because two crates must agree on
/// it: the ART guest writes it into the reply's `settle` block and the verifier
/// reads it to refuse an end-of-list inference from a gesture nothing consumed
/// (REL3H2).
pub const SETTLE_UNHANDLED_INPUT_REASON: &str = "unhandled_input";
