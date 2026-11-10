//! Structured, stable errors for APK ingest.
//!
//! Every failure carries a stable [`ApkError::code`] string so agents and the
//! CLI can branch on it without string matching on messages.

use std::fmt;

/// The input is not a readable zip/APK container.
pub const APK_NOT_ZIP: &str = "APK_NOT_ZIP";
/// The container is a zip but has no `AndroidManifest.xml`.
pub const APK_NO_MANIFEST: &str = "APK_NO_MANIFEST";
/// I/O failure while reading the APK from disk.
pub const APK_IO: &str = "APK_IO";
/// The binary AndroidManifest.xml (AXML) is malformed.
pub const AXML_MALFORMED: &str = "AXML_MALFORMED";
/// The `resources.arsc` resource table is malformed.
pub const ARSC_MALFORMED: &str = "ARSC_MALFORMED";
/// A zip entry could not be read/decompressed.
pub const APK_ENTRY: &str = "APK_ENTRY";
/// The APK Signing Block is present but malformed.
pub const APK_SIGNING_MALFORMED: &str = "APK_SIGNING_MALFORMED";

/// An error produced by APK ingest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApkError {
    code: &'static str,
    message: String,
}

impl ApkError {
    /// Construct an error with a stable code and a human-readable message.
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        ApkError {
            code,
            message: message.into(),
        }
    }

    /// The stable machine-readable error code.
    pub fn code(&self) -> &'static str {
        self.code
    }

    /// The human-readable error message.
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for ApkError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for ApkError {}

impl From<std::io::Error> for ApkError {
    fn from(e: std::io::Error) -> Self {
        ApkError::new(APK_IO, e.to_string())
    }
}

/// Convenience result alias used throughout the crate.
pub type Result<T> = std::result::Result<T, ApkError>;
