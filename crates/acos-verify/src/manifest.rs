//! Static APK/manifest resolution shared by every backend.
//!
//! This uses `acos-apk` only: it never executes code. The result feeds the
//! `AppIdentity`/`Reproducer` and the static `launch` assertion (the scenario's
//! activity must actually be declared).

use std::path::Path;

use acos_apk::Apk;
use acos_protocol::ErrorCode;
use sha2::{Digest, Sha256};

use crate::backend::{BackendError, ManifestInfo};

/// Lowercase hex encoding (no `hex` dependency).
pub fn to_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

/// SHA-256 of arbitrary bytes as lowercase hex.
pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    to_hex(&hasher.finalize())
}

/// SHA-256 of a file as lowercase hex.
pub fn sha256_file(path: &Path) -> Result<String, BackendError> {
    let bytes = std::fs::read(path).map_err(|e| BackendError::Error {
        code: ErrorCode::ApkIo,
        detail: format!("cannot read {}: {e}", path.display()),
    })?;
    Ok(sha256_hex(&bytes))
}

/// Resolve the static manifest facts of an APK.
pub fn load_manifest(apk_path: &Path) -> Result<ManifestInfo, BackendError> {
    let apk = Apk::open(apk_path).map_err(|e| BackendError::Error {
        code: ErrorCode::from_str_opt(e.code()).unwrap_or(ErrorCode::Internal),
        detail: e.message().to_string(),
    })?;
    let manifest = apk.manifest().map_err(|e| BackendError::Error {
        code: ErrorCode::from_str_opt(e.code()).unwrap_or(ErrorCode::Internal),
        detail: e.message().to_string(),
    })?;
    let resources = apk.resources().ok().flatten();
    let label = manifest.app_label(resources.as_ref());
    let launcher_activity = manifest.launcher_activity().map(|c| c.name.clone());
    let activities = manifest
        .activities
        .iter()
        .map(|c| c.name.clone())
        .collect::<Vec<_>>();
    let sha256 = sha256_hex(apk.raw_bytes());

    Ok(ManifestInfo {
        path: apk.source.clone(),
        sha256,
        package: manifest.package.clone(),
        version_code: manifest.version_code.unwrap_or(0).max(0) as u64,
        version_name: manifest.version_name.clone().unwrap_or_default(),
        min_sdk: manifest.min_sdk.unwrap_or(0),
        target_sdk: manifest.target_sdk.unwrap_or(0),
        label,
        launcher_activity,
        activities,
        native_abis: apk.native_abis(),
        debuggable: manifest.application.debuggable.unwrap_or(false),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn fixture(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/build/apps")
            .join(name)
    }

    #[test]
    fn hex_encoding_is_lowercase() {
        assert_eq!(to_hex(&[0x00, 0xab, 0xff]), "00abff");
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn hello_views_manifest_resolves() {
        let apk = fixture("hello-views.apk");
        if acos_test_support::require_prereq(
            apk.is_file(),
            "hello_views_manifest_resolves",
            &format!(
                "{} is missing — run tools/build-apps.sh (or tools/build-fixtures.sh)",
                apk.display()
            ),
        )
        .is_none()
        {
            return;
        }
        let manifest = load_manifest(&apk).unwrap();
        assert_eq!(manifest.package, "dev.acos.fixtures.helloviews");
        assert_eq!(manifest.target_sdk, 34);
        assert!(manifest.declares_activity("dev.acos.fixtures.helloviews/.MainActivity"));
        assert!(!manifest.declares_activity("dev.acos.fixtures.helloviews/.Nope"));
        assert_eq!(manifest.sha256.len(), 64);
    }
}
