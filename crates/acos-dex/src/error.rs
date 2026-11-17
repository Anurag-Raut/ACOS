//! Structured error type for all DEX parsing, decoding, and verification failures.
//!
//! Every fallible operation in this crate returns [`DexError`]. The crate never
//! panics on malformed input: out-of-bounds reads, invalid indices, truncated
//! sections and malformed encodings are all reported as values.

use thiserror::Error;

/// The result type used throughout the crate.
pub type DexResult<T> = Result<T, DexError>;

/// A structured DEX failure.
///
/// `offset` fields are absolute byte offsets into the DEX file where the failure
/// was detected. `code()` returns the stable machine-readable identifier used by
/// the ACOS JSON error contract.
#[derive(Debug, Clone, Error)]
pub enum DexError {
    #[error("bad magic at offset {offset}: expected \"dex\\n\", found {found:02x?}")]
    BadMagic { offset: u64, found: [u8; 8] },

    #[error(
        "unsupported dex version {version:?} at offset {offset} (supported: 035, 037, 038, 039)"
    )]
    UnsupportedVersion { offset: u64, version: String },

    #[error("bad adler32 checksum: stored {stored:#010x}, computed {computed:#010x}")]
    BadChecksum { stored: u32, computed: u32 },

    #[error("bad sha-1 signature: stored {stored}, computed {computed}")]
    BadSignature { stored: String, computed: String },

    #[error("truncated dex: need {needed} bytes at offset {offset}, but file is {len} bytes")]
    Truncated {
        offset: u64,
        needed: usize,
        len: usize,
    },

    #[error("unsupported endian tag {tag:#010x} at offset {offset} (only little-endian 0x12345678 is supported)")]
    BadEndian { offset: u64, tag: u32 },

    #[error("{kind} index {index} out of range (size {size}) at offset {offset}")]
    IndexOutOfRange {
        kind: &'static str,
        index: u32,
        size: u32,
        offset: u64,
    },

    #[error("bad instruction at pc {pc}: {reason} (opcode {opcode:#04x})")]
    BadInstruction {
        pc: u32,
        opcode: u8,
        reason: &'static str,
    },

    #[error("unused opcode {opcode:#04x} at pc {pc}")]
    UnusedOpcode { pc: u32, opcode: u8 },

    #[error("malformed uleb128/sleb128 at offset {offset}: {reason}")]
    BadLeb128 { offset: u64, reason: &'static str },

    #[error("malformed MUTF-8 at offset {offset}: {reason}")]
    BadMutf8 { offset: u64, reason: &'static str },

    #[error("malformed encoded value at offset {offset}: {reason}")]
    BadEncodedValue { offset: u64, reason: &'static str },

    #[error("malformed class_data_item at offset {offset}: {reason}")]
    BadClassData { offset: u64, reason: &'static str },

    #[error("malformed code_item at offset {offset}: {reason}")]
    BadCodeItem { offset: u64, reason: &'static str },

    #[error("malformed debug_info_item at offset {offset}: {reason}")]
    BadDebugInfo { offset: u64, reason: &'static str },

    #[error("malformed annotation at offset {offset}: {reason}")]
    BadAnnotation { offset: u64, reason: &'static str },

    #[error("bad map_list at offset {offset}: {reason}")]
    BadMap { offset: u64, reason: &'static str },

    #[error("bad header field {field}: {reason}")]
    BadHeader {
        field: &'static str,
        reason: &'static str,
    },

    #[error("structural verification failed in {method}: {reason}")]
    Verify { method: String, reason: String },

    #[error("integer overflow while computing {what}")]
    Overflow { what: &'static str },
}

impl DexError {
    /// Stable machine-readable error code for the JSON error contract.
    pub fn code(&self) -> &'static str {
        match self {
            DexError::BadMagic { .. } => "DEX_BAD_MAGIC",
            DexError::UnsupportedVersion { .. } => "DEX_UNSUPPORTED_VERSION",
            DexError::BadChecksum { .. } => "DEX_BAD_CHECKSUM",
            DexError::BadSignature { .. } => "DEX_BAD_SIGNATURE",
            DexError::Truncated { .. } => "DEX_TRUNCATED",
            DexError::BadEndian { .. } => "DEX_BAD_ENDIAN",
            DexError::IndexOutOfRange { .. } => "DEX_INDEX_OUT_OF_RANGE",
            DexError::BadInstruction { .. } => "DEX_BAD_INSTRUCTION",
            DexError::UnusedOpcode { .. } => "DEX_UNUSED_OPCODE",
            DexError::BadLeb128 { .. } => "DEX_BAD_LEB128",
            DexError::BadMutf8 { .. } => "DEX_BAD_MUTF8",
            DexError::BadEncodedValue { .. } => "DEX_BAD_ENCODED_VALUE",
            DexError::BadClassData { .. } => "DEX_BAD_CLASS_DATA",
            DexError::BadCodeItem { .. } => "DEX_BAD_CODE_ITEM",
            DexError::BadDebugInfo { .. } => "DEX_BAD_DEBUG_INFO",
            DexError::BadAnnotation { .. } => "DEX_BAD_ANNOTATION",
            DexError::BadMap { .. } => "DEX_BAD_MAP",
            DexError::BadHeader { .. } => "DEX_BAD_HEADER",
            DexError::Verify { .. } => "DEX_VERIFY_FAILED",
            DexError::Overflow { .. } => "DEX_OVERFLOW",
        }
    }

    /// Absolute byte offset associated with the failure, when known.
    pub fn offset(&self) -> Option<u64> {
        match self {
            DexError::BadMagic { offset, .. }
            | DexError::UnsupportedVersion { offset, .. }
            | DexError::BadEndian { offset, .. }
            | DexError::IndexOutOfRange { offset, .. }
            | DexError::BadLeb128 { offset, .. }
            | DexError::BadMutf8 { offset, .. }
            | DexError::BadEncodedValue { offset, .. }
            | DexError::BadClassData { offset, .. }
            | DexError::BadCodeItem { offset, .. }
            | DexError::BadDebugInfo { offset, .. }
            | DexError::BadAnnotation { offset, .. }
            | DexError::BadMap { offset, .. } => Some(*offset),
            DexError::Truncated { offset, .. } => Some(*offset),
            _ => None,
        }
    }

    /// Convenience constructor for out-of-range indices.
    pub(crate) fn index(kind: &'static str, index: u32, size: u32, offset: u64) -> DexError {
        DexError::IndexOutOfRange {
            kind,
            index,
            size,
            offset,
        }
    }
}
