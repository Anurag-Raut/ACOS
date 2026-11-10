//! `acos-parcel` — a standalone Android 14 `libbinder` `Parcel` codec and
//! binder handle registry for the ACOS Rust service layer.
//!
//! ACOS runs the real Android 14 framework bytecode, so the real AIDL proxies
//! and stubs write and read Android's binary `Parcel` format. This crate is the
//! Rust end of those bytes: VM-independent, no `unsafe` (`#![forbid(unsafe_code)]`),
//! and testable in isolation.
//!
//! # Layout
//!
//! ```text
//! Parcel        growable buffer + read/write position + object table
//! ParcelTable   slab of Parcels addressed by stable u64 jlong handles
//! token         writeInterfaceToken / enforceInterface / peekInterfaceToken
//! binder        BinderId, BinderKind and the 24-byte flat object record
//! fd            owned memfd/dup wrappers (via rustix, no unsafe)
//! registry      BinderRegistry + per-thread calling identity
//! ```
//!
//! # Semantics
//!
//! The byte layout is the one disassembled from Android 34
//! `libbinder.so`; see [`parcel`] and [`token`] for the exact evidence. Key
//! points:
//!
//! * four-byte alignment for every primitive;
//! * `String8`/`String16` are `int32 len` + data + NUL, padded to four;
//! * a null string is `int32 -1`;
//! * binders and fds are 24-byte flat objects whose offsets live in the object
//!   table;
//! * the interface-token header is build-dependent: the **system** libbinder the
//!   framework links against writes `0x53595354` (`"SYST"`), the VNDK/vendor
//!   build writes `0x564e4452` (`"VNDR"`). ACOS defaults to the system value;
//!   see [`token`] for the disassembly evidence.
//!
//! # Lenient reads
//!
//! ART's Java `Parcel.readInt()` returns `0` on a short buffer. The native layer
//! must call the `*_lenient` readers (e.g. [`Parcel::read_int32_lenient`]) to
//! reproduce that; the strict readers return [`ParcelError::Underflow`].

#![forbid(unsafe_code)]

pub mod binder;
pub mod error;
pub mod fd;
pub mod parcel;
pub mod registry;
pub mod token;

pub use binder::{BinderId, BinderKind, FlatObject};
pub use error::{ParcelError, Result};
pub use fd::ParcelFd;
pub use parcel::{align4, Parcel, ParcelTable};
pub use registry::{
    calling_identity, calling_pid, calling_uid, clear_calling_identity, process_identity,
    push_calling_identity, restore_calling_identity, set_process_identity, BinderRegistry,
    CallingIdentity, IdentityToken,
};
pub use token::{
    INTERFACE_TOKEN_HEADER, STRICT_MODE_PENALTY_GATHER, SYSTEM_INTERFACE_TOKEN_HEADER,
    VNDK_INTERFACE_TOKEN_HEADER,
};

/// Marker type documenting which reader variant the native layer should use.
///
/// This exists purely so the public API reads as a decision, not an accident:
/// Java `nativeReadInt` must call [`Parcel::read_int32_lenient`], while the
/// service-side dispatcher that wants a structured failure calls
/// [`Parcel::read_int32`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadMode {
    /// Match ART: short reads yield the type's zero value.
    Lenient,
    /// Return [`ParcelError::Underflow`] on a short read.
    Strict,
}
