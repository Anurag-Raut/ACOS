//! Structured errors for the in-process service layer.
//!
//! Every failure has a stable machine-readable [`ServiceError::code`]. The two
//! codes that matter to the agent contract are
//! [`codes::UNSUPPORTED_ANDROID_API`] (an `(interface, code)` pair has no
//! decoded handler — never a silent success) and
//! [`codes::SERVICE_NOT_OWNED`] (the interface belongs to another ACOS
//! component, e.g. `acos-wm`).

use acos_parcel::ParcelError;

/// Stable error codes surfaced to the agent evidence stream.
pub mod codes {
    /// The `(descriptor, code)` pair is not implemented.
    pub const UNSUPPORTED_ANDROID_API: &str = "UNSUPPORTED_ANDROID_API";
    /// A binder handle is not registered.
    pub const SERVICE_UNKNOWN_BINDER: &str = "SERVICE_UNKNOWN_BINDER";
    /// A `ServiceManager` name is not registered (real Android returns null).
    pub const SERVICE_UNKNOWN_NAME: &str = "SERVICE_UNKNOWN_NAME";
    /// The request `Parcel` did not match the decoded `WireOp` layout.
    pub const SERVICE_BAD_REQUEST: &str = "SERVICE_BAD_REQUEST";
    /// The service state machine rejected a transition.
    pub const SERVICE_ILLEGAL_STATE: &str = "SERVICE_ILLEGAL_STATE";
    /// The interface is implemented by another component (`acos-wm`).
    pub const SERVICE_NOT_OWNED: &str = "SERVICE_NOT_OWNED";
    /// A service handler failed internally.
    pub const SERVICE_INTERNAL: &str = "SERVICE_INTERNAL";
    /// The `Parcel` codec failed.
    pub const SERVICE_PARCEL: &str = "SERVICE_PARCEL";
    /// A handler needed a type this crate does not encode yet.
    pub const SERVICE_UNSUPPORTED_PARCELABLE: &str = "SERVICE_UNSUPPORTED_PARCELABLE";
    /// A handler raised an explicit Android exception.
    pub const SERVICE_ANDROID_EXCEPTION: &str = "SERVICE_ANDROID_EXCEPTION";
}

/// Errors returned by [`crate::ServiceHost`] and the service state machines.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ServiceError {
    /// No decoded transaction for `(interface, code)`.
    #[error("unsupported Android API: {interface} code {code} ({method})")]
    UnsupportedAndroidApi {
        /// AIDL interface descriptor.
        interface: String,
        /// Transaction code.
        code: u32,
        /// Best-effort method name (`code N` when not decodable).
        method: String,
    },
    /// The interface exists but is deliberately implemented elsewhere.
    #[error("interface {interface}::{method} is owned by {owner}")]
    NotOwned {
        /// AIDL interface descriptor.
        interface: String,
        /// Method name.
        method: String,
        /// Owning component.
        owner: &'static str,
    },
    /// A binder handle was not registered.
    #[error("unknown binder handle {handle:#x}")]
    UnknownBinder {
        /// Handle that was passed to `transact`.
        handle: u64,
    },
    /// The service name is unknown (Android returns `null`).
    #[error("unknown service name {name:?}")]
    UnknownService {
        /// Requested `ServiceManager` name.
        name: String,
    },
    /// A request `Parcel` could not be decoded by the decoded layout.
    #[error("bad request for {interface}::{method}: {detail}")]
    BadRequest {
        /// AIDL interface descriptor.
        interface: String,
        /// Method name.
        method: String,
        /// Human detail (field index, expected op, parcel error).
        detail: String,
    },
    /// The service state machine rejected the transition.
    #[error("illegal state in {component}: {detail}")]
    IllegalState {
        /// Component name (`atms`, `provider`, ...).
        component: &'static str,
        /// Human detail.
        detail: String,
    },
    /// A reply value did not match the decoded reply layout.
    #[error("bad reply for {interface}::{method}: {detail}")]
    BadReply {
        /// AIDL interface descriptor.
        interface: String,
        /// Method name.
        method: String,
        /// Human detail.
        detail: String,
    },
    /// A parcelable codec is not implemented for `class`.
    #[error("no parcelable codec for {class}")]
    UnsupportedParcelable {
        /// Dotted class name.
        class: String,
    },
    /// The `Parcel` codec failed.
    #[error(transparent)]
    Parcel(#[from] ParcelError),
    /// A handler raised an explicit Android `Parcel.EX_*` exception.
    #[error("android exception {code}: {message}")]
    Android {
        /// `Parcel.EX_*` code.
        code: i32,
        /// Message for `EX_SERVICE_SPECIFIC`.
        message: String,
    },
    /// Internal failure.
    #[error("internal service error: {0}")]
    Internal(String),
}

impl ServiceError {
    /// Stable machine-readable code.
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::UnsupportedAndroidApi { .. } | Self::NotOwned { .. } => {
                codes::UNSUPPORTED_ANDROID_API
            }
            Self::UnknownBinder { .. } => codes::SERVICE_UNKNOWN_BINDER,
            Self::UnknownService { .. } => codes::SERVICE_UNKNOWN_NAME,
            Self::BadRequest { .. } | Self::BadReply { .. } => codes::SERVICE_BAD_REQUEST,
            Self::IllegalState { .. } => codes::SERVICE_ILLEGAL_STATE,
            Self::UnsupportedParcelable { .. } => codes::SERVICE_UNSUPPORTED_PARCELABLE,
            Self::Android { .. } => codes::SERVICE_ANDROID_EXCEPTION,
            Self::Parcel(e) => {
                let _ = e.code();
                codes::SERVICE_PARCEL
            }
            Self::Internal(_) => codes::SERVICE_INTERNAL,
        }
    }

    /// The Android `Parcel` exception code this error maps onto.
    ///
    /// `AIDL` stubs call `reply.writeException(e)`, whose first `int` is one of
    /// the `Parcel.EX_*` constants. ACOS mirrors that mapping so a real
    /// framework proxy's `readException` sees the right exception class.
    #[must_use]
    pub fn exception_code(&self) -> i32 {
        use crate::exception::*;
        match self {
            Self::UnsupportedAndroidApi { .. }
            | Self::NotOwned { .. }
            | Self::UnsupportedParcelable { .. } => EX_UNSUPPORTED_OPERATION,
            Self::UnknownBinder { .. } | Self::UnknownService { .. } => EX_ILLEGAL_ARGUMENT,
            Self::BadRequest { .. } | Self::BadReply { .. } => EX_ILLEGAL_ARGUMENT,
            Self::IllegalState { .. } => EX_ILLEGAL_STATE,
            Self::Android { code, .. } => *code,
            Self::Parcel(_) | Self::Internal(_) => EX_ILLEGAL_ARGUMENT,
        }
    }

    /// The message written after an `EX_SERVICE_SPECIFIC` header.
    #[must_use]
    pub fn exception_message(&self) -> String {
        match self {
            Self::Android { message, .. } => message.clone(),
            _ => self.to_string(),
        }
    }

    /// Build an explicit Android exception (`Parcel.EX_*`).
    #[must_use]
    pub fn android(code: i32, message: impl Into<String>) -> Self {
        Self::Android {
            code,
            message: message.into(),
        }
    }
}

/// Convenience result alias.
pub type Result<T> = core::result::Result<T, ServiceError>;
