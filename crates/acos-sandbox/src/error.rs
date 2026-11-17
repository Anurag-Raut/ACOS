//! Structured confinement errors.

use crate::Layer;

/// Stable error code returned when a required confinement layer is missing.
pub const CONFINEMENT_UNAVAILABLE: &str = "CONFINEMENT_UNAVAILABLE";

/// Stable error code returned for any other confinement failure.
pub const CONFINEMENT_FAILED: &str = "CONFINEMENT_FAILED";

/// A structured confinement failure.
///
/// [`confine`](crate::confine) fails closed: if any layer in
/// [`ConfineProfile::required`](crate::ConfineProfile::required) is not active
/// after the attempt, it returns [`ConfineError::Unavailable`] with code
/// [`CONFINEMENT_UNAVAILABLE`] rather than running partially sandboxed.
#[derive(Debug, thiserror::Error)]
pub enum ConfineError {
    /// One or more required layers could not be established.
    #[error(
        "CONFINEMENT_UNAVAILABLE: required confinement layers not active: {missing:?} ({message})"
    )]
    Unavailable {
        /// Layers that were required but did not end up active.
        missing: Vec<Layer>,
        /// Human-readable explanation naming the kernel failures.
        message: String,
    },
}

impl ConfineError {
    /// Stable, machine-readable error code.
    pub fn code(&self) -> &'static str {
        match self {
            ConfineError::Unavailable { .. } => CONFINEMENT_UNAVAILABLE,
        }
    }
}
