//! The error type for the host broker. Every failure is one of these; the wire
//! layer maps it to a stable code (`wire::code`), never to a silent success.

use crate::cdp::CdpError;
use crate::wire::{code, WireError};

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("engine: {0}")]
    Engine(String),
    #[error("devtools: {0}")]
    Cdp(#[from] CdpError),
    #[error("wire: {0}")]
    Wire(#[from] WireError),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("{code}: {message}")]
    Proto { code: String, message: String },
}

impl Error {
    pub fn engine(msg: impl Into<String>) -> Self {
        Error::Engine(msg.into())
    }

    pub fn proto(code: impl Into<String>, message: impl Into<String>) -> Self {
        Error::Proto {
            code: code.into(),
            message: message.into(),
        }
    }

    /// The wire error code this failure maps to.
    pub fn code(&self) -> &str {
        match self {
            Error::Proto { code, .. } => code,
            Error::Engine(_) => code::ENGINE_UNAVAILABLE,
            Error::Cdp(CdpError::PipeClosed) => code::ENGINE_CRASHED,
            Error::Cdp(CdpError::Timeout(..)) => code::TIMEOUT,
            Error::Cdp(_) => code::INTERNAL,
            Error::Wire(WireError::TooLarge(_)) => code::FRAME_FAILED,
            Error::Wire(_) => code::BAD_FRAME,
            Error::Io(_) => code::INTERNAL,
        }
    }
}

impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Error::proto(code::BAD_REQUEST, format!("json: {e}"))
    }
}
