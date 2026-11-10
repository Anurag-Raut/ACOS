//! Structured VM errors.
//!
//! Following the ACOS "never a silent fake" rule, every unimplemented API,
//! opcode or internal invariant failure is surfaced as a typed error with the
//! exact call site, never a default return value.

use std::fmt;

pub type VmResult<T> = Result<T, VmError>;

/// A VM error.
///
/// [`VmError::JavaException`] is an *internal* control-flow signal carrying a
/// heap handle to a live `java.lang.Throwable`; it is consumed by the
/// interpreter's unwinder and never shown to the user directly.
#[derive(Debug, Clone)]
pub enum VmError {
    /// A live Java exception is propagating (handle; 0 = null).
    JavaException(u32),
    /// A Java class/method/native is not implemented.
    UnsupportedApi { api: String, at: String },
    /// A Dalvik opcode or bytecode feature is not implemented.
    UnsupportedBytecode { opcode: String, at: String },
    /// A VM invariant/implementation bug (should never happen).
    Internal(String),
    /// DEX/class loading failure.
    Load(String),
    /// The real boot classpath cache is missing or incomplete.
    BootClasspathMissing(String),
    /// The host process RSS crossed `ACOS_HOST_RSS_MAX`.
    HostMemoryLimit {
        rss_bytes: usize,
        limit_bytes: usize,
    },
    /// The run crossed `ACOS_WALL_MAX_SECS`.
    Timeout { elapsed_secs: u64, limit_secs: u64 },
}

impl VmError {
    /// Stable machine-readable error code.
    pub fn code(&self) -> &'static str {
        match self {
            VmError::JavaException(_) => "JAVA_EXCEPTION",
            VmError::UnsupportedApi { .. } => "UNSUPPORTED_JAVA_API",
            VmError::UnsupportedBytecode { .. } => "UNSUPPORTED_BYTECODE",
            VmError::Internal(_) => "VM_INTERNAL",
            VmError::Load(_) => "DEX_LOAD_ERROR",
            VmError::BootClasspathMissing(_) => "BOOT_CLASSPATH_MISSING",
            VmError::HostMemoryLimit { .. } => "HOST_MEMORY_LIMIT",
            VmError::Timeout { .. } => "TIMEOUT",
        }
    }

    pub fn unsupported_api(api: impl Into<String>, at: impl Into<String>) -> VmError {
        VmError::UnsupportedApi {
            api: api.into(),
            at: at.into(),
        }
    }

    pub fn unsupported_bytecode(opcode: impl Into<String>, at: impl Into<String>) -> VmError {
        VmError::UnsupportedBytecode {
            opcode: opcode.into(),
            at: at.into(),
        }
    }
}

impl fmt::Display for VmError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            VmError::JavaException(h) => write!(f, "java exception (handle {h})"),
            VmError::UnsupportedApi { api, at } => {
                write!(f, "{api} is not implemented (at {at})")
            }
            VmError::UnsupportedBytecode { opcode, at } => {
                write!(f, "opcode {opcode} is not implemented (at {at})")
            }
            VmError::Internal(m) => write!(f, "internal VM error: {m}"),
            VmError::Load(m) => write!(f, "class loading error: {m}"),
            VmError::BootClasspathMissing(m) => write!(f, "{m}"),
            VmError::HostMemoryLimit {
                rss_bytes,
                limit_bytes,
            } => write!(
                f,
                "host RSS limit exceeded: {} MiB > {} MiB (ACOS_HOST_RSS_MAX)",
                rss_bytes / (1024 * 1024),
                limit_bytes / (1024 * 1024)
            ),
            VmError::Timeout {
                elapsed_secs,
                limit_secs,
            } => write!(
                f,
                "wall-clock limit exceeded: {}s > {}s (ACOS_WALL_MAX_SECS)",
                elapsed_secs, limit_secs
            ),
        }
    }
}

impl std::error::Error for VmError {}

impl From<acos_dex::DexError> for VmError {
    fn from(e: acos_dex::DexError) -> Self {
        VmError::Load(format!("{e}"))
    }
}
