//! Install an APK's x86_64 native libraries into the per-run app lib dir.
//!
//! Stock Android extracts an APK's `lib/<abi>/*.so` into
//! `ApplicationInfo.nativeLibraryDir` at install time (`installd`), and
//! `LoadedApk` adds that directory to the class loader's native library search
//! path. ACOS stages the APK read-only under `/data/app` and never ran that
//! install step, so `dlopen` could only report
//! `library "libhellojni.so" not found` (W15 blocker 2).
//!
//! This module is the host-side install step: it uses `acos-apk`'s
//! already-hardened extractor (single path components, zip-slip re-check,
//! declared-size verification, 256 MiB per-entry cap) and adds the aggregate
//! cap and the ABI decision. The caller binds the returned host directory at
//! [`GUEST_NATIVE_LIB_ROOT`] and sets `nativeLibraryDir` to
//! `<root>/x86_64`, exactly the shape `LoadedApk`/`SoLoader` expect.
//!
//! An APK that carries native code only for another ABI is reported as
//! [`NativeLibInstall::UnsupportedAbi`] with the available ABIs; it is never
//! silently installed or guessed.

use std::path::{Path, PathBuf};

use acos_apk::{AbiGate, Apk};

use crate::error::{Result, ServiceError};

/// The ABI this host can execute, re-exported from [`acos_apk::HOST_ABI`] so a
/// caller (the launcher) can name it in a refusal's `details` without taking a
/// direct `acos-apk` dependency.
pub const HOST_ABI: &str = acos_apk::HOST_ABI;

/// Guest directory the per-run app lib tree is bound at.
pub const GUEST_NATIVE_LIB_ROOT: &str = "/data/app-lib";

/// Guest `ApplicationInfo.nativeLibraryDir` for the per-run tree.
pub const GUEST_NATIVE_LIB_DIR: &str = "/data/app-lib/x86_64";

/// Aggregate ceiling for one APK's x86_64 libraries (zip-bomb backstop; the
/// per-entry ceiling is 256 MiB inside `acos-apk`).
pub const MAX_TOTAL_BYTES: u64 = 512 * 1024 * 1024;

/// Outcome of installing one APK's native libraries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NativeLibInstall {
    /// The APK has no native code: nothing to install (an empty directory is
    /// still mounted so the framework's `nativeLibraryDir` exists).
    None,
    /// Libraries were extracted into `dir` (a host path ending in `x86_64`).
    Installed {
        /// Host directory holding the extracted `.so` files.
        dir: PathBuf,
        /// File names, in the APK's deterministic order.
        libs: Vec<String>,
    },
    /// The APK has native code, but none for [`HOST_ABI`] (the AOSP
    /// `INSTALL_FAILED_NO_MATCHING_ABIS` case: the package cannot be installed).
    UnsupportedAbi {
        /// ABIs the APK does carry, sorted.
        available: Vec<String>,
        /// A bounded `lib/<abi>/<name>` sample of what the APK does ship.
        sample: Vec<String>,
        /// The `acos-apk` structured message (`UNSUPPORTED_ABI: ...`).
        message: String,
    },
}

impl NativeLibInstall {
    /// The guest `nativeLibraryDir` this install corresponds to.
    #[must_use]
    pub fn guest_dir(&self) -> &'static str {
        GUEST_NATIVE_LIB_DIR
    }
}

/// Extract the APK's x86_64 libraries under `dest_root/x86_64`.
///
/// Opens the APK itself. A launch that already holds an open of it calls
/// [`install_from`] instead, so the APK is read once.
///
/// `dest_root` must be a per-run directory the caller owns; it is created when
/// needed and never reused across runs by this function.
pub fn install(apk_path: &Path, dest_root: &Path) -> Result<NativeLibInstall> {
    let apk = Apk::open(apk_path).map_err(|e| {
        ServiceError::Internal(format!("native libs: open {}: {e}", apk_path.display()))
    })?;
    install_from(&apk, apk_path, dest_root)
}

/// [`install`] for an APK that is already open (`apk` is the open of `apk_path`).
/// PERF-REL3c: a launch opens its APK once and reuses that open here.
pub fn install_from(apk: &Apk, apk_path: &Path, dest_root: &Path) -> Result<NativeLibInstall> {
    // AOSP's per-package ABI choice (`Apk::abi_gate`): one host ABI directory is
    // enough for the package to install, and only *that* directory is extracted
    // — a library that exists solely under another ABI stays missing at
    // `dlopen`, exactly as on the platform.
    match apk.abi_gate() {
        AbiGate::NoNative => return Ok(NativeLibInstall::None),
        AbiGate::Supported { .. } => {}
        AbiGate::Unsupported {
            apk_abis,
            libs_sample,
        } => {
            // Deliberately not an error: the decision belongs to the launcher,
            // which refuses the launch before any guest exists (ABIGATE). This
            // step must not spawn, guess or silently install another ABI.
            //
            // The message names the host ABI and the APK's own inventory; the
            // code is the variant, so it is not repeated in the prose (the CLI
            // prefixes its own `acos-art: UNSUPPORTED_ABI: …`).
            let message = format!(
                "APK has no native libraries for ABI {HOST_ABI:?}; available: [{}]",
                apk_abis.join(", ")
            );
            return Ok(NativeLibInstall::UnsupportedAbi {
                available: apk_abis,
                sample: libs_sample,
                message,
            });
        }
    }
    let total: u64 = apk
        .native_libs
        .get(HOST_ABI)
        .map(|libs| libs.iter().map(|l| l.size).sum())
        .unwrap_or(0);
    if total > MAX_TOTAL_BYTES {
        return Err(ServiceError::Internal(format!(
            "native libs: APK declares {total} bytes for {HOST_ABI}, over the {MAX_TOTAL_BYTES} byte cap"
        )));
    }
    let dir = dest_root.join(HOST_ABI);
    let extracted = apk.extract_native_libs(HOST_ABI, &dir).map_err(|e| {
        ServiceError::Internal(format!("native libs: extract {}: {e}", apk_path.display()))
    })?;
    Ok(NativeLibInstall::Installed {
        dir,
        libs: extracted.into_iter().map(|(name, _)| name).collect(),
    })
}
