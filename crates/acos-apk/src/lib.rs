//! # acos-apk
//!
//! APK ingest for ACOS: zip container indexing, DEX/native-lib/asset
//! discovery, binary `AndroidManifest.xml` (AXML) parsing, `resources.arsc`
//! parsing and signature-scheme detection.
//!
//! Design rules:
//! - no `unsafe`;
//! - no panics on malformed or fuzzed input — every failure is a structured
//!   [`ApkError`] with a stable [`ApkError::code`];
//! - signature detection is *detection only* and is explicitly labelled as not
//!   cryptographically verified.
//!
//! ```
//! # fn main() -> Result<(), acos_apk::ApkError> {
//! let apk = acos_apk::Apk::open("does-not-exist.apk");
//! assert!(apk.is_err());
//! # Ok(())
//! # }
//! ```

#![forbid(unsafe_code)]

mod binary;

pub mod apk;
pub mod arsc;
pub mod axml;
pub mod error;
pub mod manifest;
pub mod native;
pub mod signing;

pub use apk::{Apk, ApkEntry, DexFile, NativeLib, HOST_ABI};
pub use arsc::{Config, ResValue, ResourceEntry, ResourcePackage, ResourceTable, TypeSpec};
pub use axml::{TypedValue, XmlAttribute, XmlDocument, XmlNode};
pub use error::{ApkError, Result};
pub use manifest::{
    ApplicationInfo, Component, ComponentKind, IntentData, IntentFilter, Manifest, UsesFeature,
    UsesPermission,
};
pub use native::{missing_native_libs, NativeLibWarning, NATIVE_LIB_MISSING, SYSTEM_LIBS};
pub use signing::SigningInfo;
