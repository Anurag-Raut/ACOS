//! Top-level runner errors.

use acos_protocol::scenario::ScenarioValidation;
use acos_protocol::ErrorCode;

use crate::backend::BackendError;

/// A failure that prevents producing a verdict at all.
#[derive(Debug, thiserror::Error)]
pub enum VerifyError {
    /// The scenario is syntactically or semantically invalid.
    #[error("invalid scenario")]
    InvalidScenario(ScenarioValidation),
    /// The scenario file could not be read.
    #[error("cannot read scenario: {0}")]
    ScenarioIo(String),
    /// An artifact could not be written.
    #[error("cannot write artifact `{path}`: {detail}")]
    ArtifactIo {
        /// Path that failed.
        path: String,
        /// Underlying detail.
        detail: String,
    },
    /// The host filesystem refused an artifact write (ENOSPC/EDQUOT) or is
    /// below the configured minimum. A host condition, not a verdict failure.
    #[error("host disk full while writing `{path}`: {detail}")]
    DiskFull {
        /// Path that failed.
        path: String,
        /// Underlying detail.
        detail: String,
    },
    /// The backend failed before the first step (for example loading the APK).
    #[error(transparent)]
    Backend(#[from] BackendError),
}

impl VerifyError {
    /// The protocol error code for this failure.
    pub fn code(&self) -> ErrorCode {
        match self {
            VerifyError::InvalidScenario(_) | VerifyError::ScenarioIo(_) => {
                ErrorCode::InvalidScenario
            }
            VerifyError::ArtifactIo { .. } => ErrorCode::ArtifactsInvalid,
            VerifyError::DiskFull { .. } => ErrorCode::HostDiskFull,
            VerifyError::Backend(error) => error.code(),
        }
    }
}
