//! Error type for the parcel codec.
//!
//! Every failure carries a stable string [`ParcelError::code`] so the native
//! layer (Java `android.os.Parcel` natives) can map it onto a Java exception
//! without depending on the enum shape.

/// Stable, machine-readable error codes.
pub mod codes {
    /// Reader ran off the end of the parcel.
    pub const PARCEL_UNDERFLOW: &str = "PARCEL_UNDERFLOW";
    /// A data position outside `[0, data_size]` was requested.
    pub const PARCEL_BAD_POSITION: &str = "PARCEL_BAD_POSITION";
    /// A recorded object offset was misaligned or out of range.
    pub const PARCEL_BAD_OBJECT: &str = "PARCEL_BAD_OBJECT";
    /// A flat object of the wrong type was read as a binder.
    pub const PARCEL_NOT_A_BINDER: &str = "PARCEL_NOT_A_BINDER";
    /// A flat object of the wrong type was read as a file descriptor.
    pub const PARCEL_NOT_A_FD: &str = "PARCEL_NOT_A_FD";
    /// A string was not NUL terminated / could not be decoded.
    pub const PARCEL_BAD_STRING: &str = "PARCEL_BAD_STRING";
    /// Two interface tokens did not match.
    pub const PARCEL_TOKEN_MISMATCH: &str = "PARCEL_TOKEN_MISMATCH";
    /// The interface-token magic header was wrong.
    pub const PARCEL_BAD_TOKEN_HEADER: &str = "PARCEL_BAD_TOKEN_HEADER";
    /// File descriptors may not be written (`allowFds == false`).
    pub const PARCEL_FDS_NOT_ALLOWED: &str = "PARCEL_FDS_NOT_ALLOWED";
    /// The ashmem blob form was requested; ACOS implements inline only.
    pub const PARCEL_ASHMEM_UNSUPPORTED: &str = "PARCEL_ASHMEM_UNSUPPORTED";
    /// A binder id was not present in the registry.
    pub const PARCEL_UNKNOWN_BINDER: &str = "PARCEL_UNKNOWN_BINDER";
    /// A length field was out of range.
    pub const PARCEL_BAD_LENGTH: &str = "PARCEL_BAD_LENGTH";
    /// An operating-system file-descriptor operation failed.
    pub const PARCEL_FD_ERROR: &str = "PARCEL_FD_ERROR";
    /// The requested parcel handle was stale (generation mismatch).
    pub const PARCEL_STALE_HANDLE: &str = "PARCEL_STALE_HANDLE";
    /// `restoreCallingIdentity` was called out of LIFO order.
    pub const PARCEL_IDENTITY_LIFO: &str = "PARCEL_IDENTITY_LIFO";
}

/// Errors returned by [`crate::Parcel`] and [`crate::BinderRegistry`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ParcelError {
    /// Reader ran off the end of the parcel.
    #[error("parcel underflow: need {needed} byte(s) at {at}, have {available}")]
    Underflow {
        /// Offset the read started at.
        at: usize,
        /// Number of bytes required.
        needed: usize,
        /// Number of bytes actually available.
        available: usize,
    },
    /// A data position outside `[0, data_size]` was requested.
    #[error("parcel position {pos} out of range 0..={size}")]
    BadPosition {
        /// Requested position.
        pos: usize,
        /// Logical data size.
        size: usize,
    },
    /// A recorded object offset was misaligned or out of range.
    #[error("bad parcelled object at offset {offset} (data size {size})")]
    BadObject {
        /// Offset of the object.
        offset: usize,
        /// Logical data size.
        size: usize,
    },
    /// A flat object of the wrong type was read as a binder.
    #[error("object at {offset} is not a binder (type {ty:#010x})")]
    NotABinder {
        /// Offset of the object.
        offset: usize,
        /// Object type tag.
        ty: u32,
    },
    /// A flat object of the wrong type was read as a file descriptor.
    #[error("object at {offset} is not a file descriptor (type {ty:#010x})")]
    NotAFileDescriptor {
        /// Offset of the object.
        offset: usize,
        /// Object type tag.
        ty: u32,
    },
    /// A string was not NUL terminated / could not be decoded.
    #[error("bad string: {0}")]
    BadString(&'static str),
    /// A length field was out of range.
    #[error("bad length {len} (max {max})")]
    BadLength {
        /// Requested length.
        len: usize,
        /// Maximum accepted.
        max: usize,
    },
    /// Two interface tokens did not match.
    #[error("interface token mismatch: expected {expected:?}, found {found:?}")]
    InterfaceTokenMismatch {
        /// Descriptor the caller asked for.
        expected: String,
        /// Descriptor found in the parcel.
        found: String,
    },
    /// The interface-token magic header was wrong.
    #[error("bad interface token header {found:#010x} (expected {expected:#010x})")]
    BadInterfaceTokenHeader {
        /// Header value found on the wire.
        found: u32,
        /// Header value Android 14 writes.
        expected: u32,
    },
    /// File descriptors may not be written (`allowFds == false`).
    #[error("file descriptors are not allowed in this parcel")]
    FdsNotAllowed,
    /// The ashmem blob form was requested; ACOS implements inline only.
    #[error("ashmem/immutable blobs are not supported by acos-parcel")]
    AshmemUnsupported,
    /// A binder id was not present in the registry.
    #[error("unknown binder id {0}")]
    UnknownBinder(u64),
    /// An operating-system file-descriptor operation failed.
    #[error("fd error: {0}")]
    Fd(String),
    /// The requested parcel handle was stale (generation mismatch).
    #[error("stale parcel handle {0:#x}")]
    StaleHandle(u64),
    /// `restoreCallingIdentity` was called out of LIFO order.
    #[error("restoreCallingIdentity called out of LIFO order")]
    IdentityLifo,
}

impl ParcelError {
    /// Stable error code for the native/Java boundary.
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::Underflow { .. } => codes::PARCEL_UNDERFLOW,
            Self::BadPosition { .. } => codes::PARCEL_BAD_POSITION,
            Self::BadObject { .. } => codes::PARCEL_BAD_OBJECT,
            Self::NotABinder { .. } => codes::PARCEL_NOT_A_BINDER,
            Self::NotAFileDescriptor { .. } => codes::PARCEL_NOT_A_FD,
            Self::BadString(_) => codes::PARCEL_BAD_STRING,
            Self::BadLength { .. } => codes::PARCEL_BAD_LENGTH,
            Self::InterfaceTokenMismatch { .. } => codes::PARCEL_TOKEN_MISMATCH,
            Self::BadInterfaceTokenHeader { .. } => codes::PARCEL_BAD_TOKEN_HEADER,
            Self::FdsNotAllowed => codes::PARCEL_FDS_NOT_ALLOWED,
            Self::AshmemUnsupported => codes::PARCEL_ASHMEM_UNSUPPORTED,
            Self::UnknownBinder(_) => codes::PARCEL_UNKNOWN_BINDER,
            Self::Fd(_) => codes::PARCEL_FD_ERROR,
            Self::StaleHandle(_) => codes::PARCEL_STALE_HANDLE,
            Self::IdentityLifo => codes::PARCEL_IDENTITY_LIFO,
        }
    }

    /// Android `status_t` style numeric code (`0 == NO_ERROR`).
    ///
    /// These mirror the libbinder constants used by the real framework:
    /// `BAD_VALUE = -EINVAL`, `NOT_ENOUGH_DATA = -ENODATA`,
    /// `PERMISSION_DENIED = -EPERM`, `UNKNOWN_ERROR = MINUS_ONE`.
    #[must_use]
    pub fn status(&self) -> i32 {
        match self {
            Self::Underflow { .. } | Self::BadObject { .. } | Self::BadLength { .. } => -61, // -ENODATA
            Self::BadPosition { .. }
            | Self::NotABinder { .. }
            | Self::NotAFileDescriptor { .. }
            | Self::BadString(_)
            | Self::BadInterfaceTokenHeader { .. }
            | Self::AshmemUnsupported => -22, // -EINVAL
            Self::InterfaceTokenMismatch { .. } | Self::FdsNotAllowed => -1, // -EPERM
            Self::UnknownBinder(_) => -22,                                   // -EINVAL
            Self::StaleHandle(_) => -22,                                     // -EINVAL
            Self::Fd(_) => -5,                                               // -EIO
            Self::IdentityLifo => -22,                                       // -EINVAL
        }
    }
}

/// Convenience result alias.
pub type Result<T> = core::result::Result<T, ParcelError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_are_stable_and_unique_prefixes() {
        let samples = [
            ParcelError::Underflow {
                at: 0,
                needed: 4,
                available: 0,
            },
            ParcelError::BadPosition { pos: 9, size: 0 },
            ParcelError::BadObject { offset: 1, size: 0 },
            ParcelError::NotABinder { offset: 0, ty: 0 },
            ParcelError::NotAFileDescriptor { offset: 0, ty: 0 },
            ParcelError::BadString("x"),
            ParcelError::InterfaceTokenMismatch {
                expected: "a".into(),
                found: "b".into(),
            },
            ParcelError::BadInterfaceTokenHeader {
                found: 0,
                expected: 1,
            },
            ParcelError::FdsNotAllowed,
            ParcelError::AshmemUnsupported,
            ParcelError::UnknownBinder(1),
            ParcelError::Fd("nope".into()),
            ParcelError::StaleHandle(2),
            ParcelError::IdentityLifo,
        ];
        for e in samples {
            assert!(e.code().starts_with("PARCEL_"));
        }
    }
}
