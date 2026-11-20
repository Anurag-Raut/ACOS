//! # acos-verify
//!
//! The scenario runner behind `acos verify` — the agent's verification loop
//! entry point. It parses a scenario with `acos-protocol`, executes its steps
//! against a [`Backend`], writes the evidence directory described in
//! `docs/AGENT_API.md` §10, and returns an evidence-backed [`Envelope`].
//!
//! ## The honesty rules
//!
//! * `ok: true` (and `status: pass`) requires **every** step to pass — a step the
//!   backend cannot perform is `unsupported`, never skipped.
//! * A non-pass verdict always carries a structured failure and the artifacts
//!   that support it.
//! * A scenario that expects `fail`/`non_pass` can never be reported as a pass;
//!   if it is, the runner emits an `INTERNAL` harness failure.
//! * Fidelity is the achieved level. If a caller requires a stronger fidelity
//!   than the backend reaches, the result is `unsupported`, not an optimistic
//!   pass.
//!
//! ## Backends today
//!
//! * [`NativeBackend`] — `acos-apk` static facts plus `acos-runtime`
//!   `boot_app_process` to the M2 attach point. UI tree, screenshots, input and
//!   environment are not implemented yet and report `unsupported`.
//! * [`ReferenceBackend`] — replays the checked-in `art-reference` evidence so
//!   the assertion logic and false-pass prevention can be exercised today.
//!
//! The [`Backend`] trait is the seam: M2/M3 lifecycle, screenshot, UI-tree and
//! input capabilities plug in by implementing a method and switching on a
//! capability, with no scenario-schema change.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

pub mod artifacts;
pub mod assertions;
pub mod backend;
pub mod clock;
pub mod display;
pub mod error;
pub mod executor;
pub mod manifest;
pub mod native;
pub mod reference;
pub mod retention;
pub mod screenshot;
pub mod settle;
pub mod verify;

pub use backend::{
    Backend, BackendError, Capabilities, Capability, CrashRecord, EnvironmentChange, InputAction,
    LaunchOutcome, LaunchRequest, LogRecord, ManifestInfo, SessionObservation, StateChange,
};
pub use clock::{Clock, FixedClock, SystemClock};
pub use display::{frame_display_geometry, FrameDisplayGeometry};
pub use error::VerifyError;
pub use executor::{run_scenario, RunOptions, RunOutcome};
pub use native::NativeBackend;
pub use reference::{ReferenceBackend, ReferenceState};
pub use retention::{Decision, KeepArtifacts, RetentionReport, KEEP_ARTIFACTS_ENV};
pub use settle::{content_signature, content_window_ms, ContentWindow};
pub use verify::{verify, verify_with_clock};

// Re-export the protocol types a caller needs to drive `verify`, so consumers do
// not have to add `acos-protocol` themselves.
pub use acos_protocol::commands::{VerifyRequest, VerifyResponse};
pub use acos_protocol::{Envelope, Fidelity, Status};
