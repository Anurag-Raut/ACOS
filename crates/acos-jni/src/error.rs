//! Structured JNI errors.
//!
//! JNI is a C ABI with no error channel: a bad reference is undefined behaviour
//! on Android. ACOS never guesses. Every contract violation records a typed
//! error in the calling thread, returns the slot's default value, and makes the
//! enclosing native call fail with that error instead of continuing.

/// Stable machine-readable JNI error.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum JniError {
    /// A non-exception-safe JNI function was called while a Java exception was
    /// pending.
    #[error("JNI_PENDING_EXCEPTION: {function} called with a pending exception")]
    PendingException { function: String },
    /// The slot is present in the table but not implemented.
    #[error("JNI_UNSUPPORTED: {slot} is not implemented")]
    Unsupported { slot: String },
    /// No attached VM on this host thread.
    #[error("JNI_NO_VM: {function} called without an attached VM")]
    NoVm { function: String },
    /// A null `jclass`/`jobject`/`jmethodID` where a live reference is required.
    #[error("JNI_NULL_REFERENCE: {function} received a null {what}")]
    NullReference { function: String, what: String },
    /// A reference that does not designate a live object of the expected kind.
    #[error("JNI_BAD_REFERENCE: {function} received an invalid {what} (handle {handle})")]
    BadReference {
        function: String,
        what: String,
        handle: u32,
    },
    #[error("JNI_CLASS_NOT_FOUND: {name}")]
    ClassNotFound { name: String },
    #[error("JNI_METHOD_NOT_FOUND: {class}->{name}{sig}")]
    MethodNotFound {
        class: String,
        name: String,
        sig: String,
    },
    #[error("JNI_FIELD_NOT_FOUND: {class}.{name}:{desc}")]
    FieldNotFound {
        class: String,
        name: String,
        desc: String,
    },
    /// The Java-side type does not match the JNI slot's declared type.
    #[error("JNI_TYPE_MISMATCH: {function} expected {expected}, got {got}")]
    TypeMismatch {
        function: String,
        expected: String,
        got: String,
    },
    #[error("JNI_BAD_ARGUMENT: {function}: {message}")]
    BadArgument { function: String, message: String },
    /// A native symbol could not be resolved for a DEX `native` method.
    #[error("JNI_UNSATISFIED_LINK: {class}->{name}{sig} (tried {symbol})")]
    UnsatisfiedLink {
        class: String,
        name: String,
        sig: String,
        symbol: String,
    },
    #[error("JNI_NATIVE_FAULT: {message}")]
    NativeFault { message: String },
    #[error("JNI_LOAD_FAILED: {message}")]
    LoadFailed { message: String },
}

impl JniError {
    /// Stable, machine-readable code.
    pub fn code(&self) -> &'static str {
        match self {
            JniError::PendingException { .. } => "JNI_PENDING_EXCEPTION",
            JniError::Unsupported { .. } => "JNI_UNSUPPORTED",
            JniError::NoVm { .. } => "JNI_NO_VM",
            JniError::NullReference { .. } => "JNI_NULL_REFERENCE",
            JniError::BadReference { .. } => "JNI_BAD_REFERENCE",
            JniError::ClassNotFound { .. } => "JNI_CLASS_NOT_FOUND",
            JniError::MethodNotFound { .. } => "JNI_METHOD_NOT_FOUND",
            JniError::FieldNotFound { .. } => "JNI_FIELD_NOT_FOUND",
            JniError::TypeMismatch { .. } => "JNI_TYPE_MISMATCH",
            JniError::BadArgument { .. } => "JNI_BAD_ARGUMENT",
            JniError::UnsatisfiedLink { .. } => "JNI_UNSATISFIED_LINK",
            JniError::NativeFault { .. } => "JNI_NATIVE_FAULT",
            JniError::LoadFailed { .. } => "JNI_LOAD_FAILED",
        }
    }
}
