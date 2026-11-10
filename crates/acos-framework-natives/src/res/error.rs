//! Structured, stable errors for the resource engine.
//!
//! Every failure carries a stable [`ResError::code`] string so the (later)
//! native shim and tests can branch without matching on messages. No parser in
//! this crate panics on malformed input; every short read becomes one of these.

use std::fmt;

/// A resource engine failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResError {
    code: &'static str,
    message: String,
}

/// The input is not a readable APK/zip container.
pub const RES_IO: &str = "RES_IO";
/// The container is a zip but the entry could not be read.
pub const RES_ZIP: &str = "RES_ZIP";
/// `resources.arsc` is malformed.
pub const RES_ARSC: &str = "RES_ARSC";
/// A binary XML document is malformed.
pub const RES_AXML: &str = "RES_AXML";
/// A resource string pool is malformed.
pub const RES_STRING_POOL: &str = "RES_STRING_POOL";
/// A resource configuration chunk is malformed.
pub const RES_CONFIG: &str = "RES_CONFIG";
/// The resource id does not exist in any loaded table.
pub const RES_NOT_FOUND: &str = "RES_NOT_FOUND";
/// A handle passed to the engine is unknown or has been freed.
pub const RES_BAD_HANDLE: &str = "RES_BAD_HANDLE";
/// An index argument is out of range.
pub const RES_INDEX: &str = "RES_INDEX";
/// The requested operation is not supported by this engine.
pub const RES_UNSUPPORTED: &str = "RES_UNSUPPORTED";
/// No package with the requested id is loaded.
pub const RES_NO_PACKAGE: &str = "RES_NO_PACKAGE";
/// An asset/XML entry is not present.
pub const RES_ASSET_NOT_FOUND: &str = "RES_ASSET_NOT_FOUND";
/// A reference chain exceeded the resolution depth.
pub const RES_REFERENCE_LOOP: &str = "RES_REFERENCE_LOOP";

impl ResError {
    /// Construct an error with a stable code and a human-readable message.
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        ResError {
            code,
            message: message.into(),
        }
    }

    /// The stable machine-readable error code.
    pub fn code(&self) -> &'static str {
        self.code
    }

    /// The human-readable message.
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for ResError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for ResError {}

impl From<std::io::Error> for ResError {
    fn from(e: std::io::Error) -> Self {
        ResError::new(RES_IO, e.to_string())
    }
}

/// Convenience result alias.
pub type Result<T> = std::result::Result<T, ResError>;
