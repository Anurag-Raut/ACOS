//! # acos-framework-natives
//!
//! The VM-independent Rust engine behind the Android framework's
//! `android.content.res` native methods: `AssetManager`, `ApkAssets`,
//! `StringBlock`, `XmlBlock`, themes and style resolution.
//!
//! The Android framework bytecode calls a small set of JNI natives; ACOS runs
//! that same bytecode on a Rust VM (ADR-0007). This crate implements the
//! *engine* those natives are a thin shim over — plain Rust structs, integer
//! handle tables, primitive/slice arguments — so it can be unit-tested without
//! a VM and bound to natives later.
//!
//! Design rules (mirroring `acos-apk`):
//! - no panic on malformed/fuzzed input — every failure is a structured
//!   [`ResError`] with a stable code;
//! - APKs are memory-mapped (or fd-backed), never copied wholesale; `resources.arsc`
//!   is parsed lazily (only chunk headers and offsets up front);
//! - `unsafe` is confined to memory mapping and documented in
//!   `src/res/NATIVES.md`.
//!
//! ```
//! # fn main() -> Result<(), acos_framework_natives::res::ResError> {
//! let apk = acos_framework_natives::res::ApkAssets::open("does-not-exist.apk");
//! assert!(apk.is_err());
//! # Ok(())
//! # }
//! ```

#![forbid(unsafe_op_in_unsafe_fn)]
#![warn(missing_debug_implementations)]

pub mod res;

pub use res::ResError;
