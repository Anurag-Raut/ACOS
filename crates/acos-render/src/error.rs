//! Structured errors for the ACOS offscreen renderer.
//!
//! Every error carries a stable `code()` string so the agent-facing control
//! plane can branch on it without matching on Rust variants. The codes are
//! deliberately aligned with the ACOS error vocabulary (`NO_FRESH_FRAME`,
//! `UNSUPPORTED_ANDROID_API`, ...).

use thiserror::Error;

/// Errors returned by [`crate::RenderContext`] and friends.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum RenderError {
    /// The requested draw/measure operation is not implemented. The `op`
    /// field names it (e.g. `"Path.op union"`). In strict mode this is fatal;
    /// otherwise it is recorded in the frame's `unsupported_ops` list.
    #[error("unsupported operation: {op}")]
    Unsupported { op: String },

    /// A handle did not resolve (wrong index, stale generation, or removed).
    #[error("bad handle")]
    BadHandle,

    /// No font or font family could be resolved.
    #[error("font not found: {name}")]
    FontNotFound { name: String },

    /// `capture_fresh` was asked for a frame newer than what exists.
    #[error("no fresh frame: requested after seq {after}, latest is {latest}")]
    NoFreshFrame { after: u64, latest: u64 },

    /// A caller-supplied argument was out of range or malformed.
    #[error("invalid argument: {msg}")]
    InvalidArgument { msg: String },

    /// A surface/bitmap allocation would be degenerate or absurdly large.
    #[error("size error: {msg}")]
    Size { msg: String },

    /// An image or font file could not be decoded.
    #[error("decode error: {msg}")]
    Decode { msg: String },

    /// Host I/O failure (font discovery, image files).
    #[error("io error: {msg}")]
    Io { msg: String },

    /// Strict mode escalated an otherwise-recordable fidelity loss.
    #[error("strict-mode failure: {msg}")]
    Strict { msg: String },
}

impl RenderError {
    /// Stable machine-readable code for this error.
    pub fn code(&self) -> &'static str {
        match self {
            RenderError::Unsupported { .. } => "RENDER_UNSUPPORTED_OP",
            RenderError::BadHandle => "RENDER_BAD_HANDLE",
            RenderError::FontNotFound { .. } => "RENDER_FONT_NOT_FOUND",
            RenderError::NoFreshFrame { .. } => "NO_FRESH_FRAME",
            RenderError::InvalidArgument { .. } => "RENDER_INVALID_ARGUMENT",
            RenderError::Size { .. } => "RENDER_SIZE",
            RenderError::Decode { .. } => "RENDER_DECODE",
            RenderError::Io { .. } => "RENDER_IO",
            RenderError::Strict { .. } => "RENDER_STRICT",
        }
    }

    pub(crate) fn unsupported(op: impl Into<String>) -> Self {
        RenderError::Unsupported { op: op.into() }
    }

    pub(crate) fn invalid(msg: impl Into<String>) -> Self {
        RenderError::InvalidArgument { msg: msg.into() }
    }

    pub(crate) fn size(msg: impl Into<String>) -> Self {
        RenderError::Size { msg: msg.into() }
    }
}

/// Convenience result alias.
pub type RenderResult<T> = Result<T, RenderError>;
