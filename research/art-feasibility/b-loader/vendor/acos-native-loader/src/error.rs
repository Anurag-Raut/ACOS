//! Structured error types for the native loader.
//!
//! Every variant maps to a stable, machine-readable [`LoaderError::code`]
//! string so that agent-facing callers can branch on failures without
//! string matching.

use std::path::PathBuf;

/// Structured loader failure.
#[derive(Debug, thiserror::Error)]
pub enum LoaderError {
    /// The input bytes are not a valid ELF64 image (parse stage; never panics).
    #[error("ELF is malformed: {0}")]
    ElfMalformed(String),

    /// The ELF is well formed but not for the host instruction set (e.g. an
    /// arm64 library loaded by an x86_64 runtime, or vice versa).
    #[error(
        "unsupported ABI {found}: this loader only executes {} ELF; {detail}",
        crate::elf::HOST_ABI_NAME
    )]
    WrongAbi { found: String, detail: String },

    /// A `DT_NEEDED` dependency could not be resolved.
    #[error("native dependency {needed:?} required by {requester:?} was not found (searched {searched:?})")]
    DepNotFound {
        needed: String,
        requester: String,
        searched: Vec<PathBuf>,
    },

    /// A `DT_NEEDED` entry named an absolute path or a path-bearing/`..` name,
    /// or resolved outside every `search_paths` root. Guest-controlled ELF
    /// content must not be able to name a host library (W67A #4).
    #[error("unsafe native dependency {needed:?} required by {requester:?}: only SONAME-style names inside the search paths are allowed")]
    UnsafeDependency { needed: String, requester: String },

    /// Relocation, init, or dependency initialization failed.
    #[error("library initialization failed: {0}")]
    InitFailed(String),

    /// One or more guest imports are not provided by the bionic table.
    #[error("unsupported native symbols: {0:?}")]
    UnsupportedSymbols(Vec<LibraryMissing>),

    /// A relocation kind the loader cannot apply.
    #[error("unsupported relocation type {kind} (0x{kind:x}) in {library:?} at 0x{offset:x}")]
    UnsupportedRelocation {
        kind: u32,
        library: String,
        offset: u64,
    },

    /// Filesystem access failed.
    #[error("io error: {0}")]
    Io(String),

    /// A symbol requested by the caller does not exist.
    #[error("symbol not found: {0}")]
    SymbolNotFound(String),

    /// Caller supplied an invalid argument.
    #[error("invalid argument: {0}")]
    InvalidArgument(String),

    /// Native loading was refused because the process is not confined.
    ///
    /// The loader maps `PROT_EXEC` guest code in-process, so it refuses to
    /// proceed unless `PR_SET_NO_NEW_PRIVS`, Landlock and seccomp are active
    /// (see DECISIONS.md ADR-0022).
    #[error(
        "refusing to load native code: process confinement is not active (CONFINEMENT_UNAVAILABLE)"
    )]
    ConfinementRequired,
}

impl LoaderError {
    /// Stable, machine-readable error code.
    pub fn code(&self) -> &'static str {
        match self {
            LoaderError::ElfMalformed(_) => "ELF_MALFORMED",
            LoaderError::WrongAbi { .. } => "WRONG_ABI",
            LoaderError::DepNotFound { .. } => "NATIVE_DEP_NOT_FOUND",
            LoaderError::UnsafeDependency { .. } => "NATIVE_DEP_UNSAFE",
            LoaderError::InitFailed(_) => "NATIVE_INIT_FAILED",
            LoaderError::UnsupportedSymbols(_) => "UNSUPPORTED_NATIVE_SYMBOL",
            LoaderError::UnsupportedRelocation { .. } => "UNSUPPORTED_RELOCATION",
            LoaderError::Io(_) => "NATIVE_IO_ERROR",
            LoaderError::SymbolNotFound(_) => "NATIVE_SYMBOL_NOT_FOUND",
            LoaderError::InvalidArgument(_) => "NATIVE_INVALID_ARGUMENT",
            LoaderError::ConfinementRequired => "CONFINEMENT_UNAVAILABLE",
        }
    }
}

impl From<std::io::Error> for LoaderError {
    fn from(e: std::io::Error) -> Self {
        LoaderError::Io(e.to_string())
    }
}

/// Missing imports for one library.
#[derive(Debug, Clone)]
pub struct LibraryMissing {
    pub library: String,
    pub symbols: Vec<String>,
}

/// Convenience result alias.
pub type Result<T> = std::result::Result<T, LoaderError>;
