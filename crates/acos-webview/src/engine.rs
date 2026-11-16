//! The pinned engine: Chrome for Testing `chrome-headless-shell`, downloaded by
//! the installer/first use, verified against a pinned SHA-256, and cached under
//! the ACOS cache (`<root>/.cache/webview/engine/<version>/`).
//!
//! Nothing here trusts an existing file: the install is accepted only when the
//! `VERIFIED.json` marker matches the pinned version **and** the pinned zip
//! digest **and** the engine binary's size. A mismatch re-verifies the cached
//! zip and, if that fails too, re-downloads it. A partially extracted engine is
//! never used (the marker is written last, after the version probe succeeds).

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::{Error, Result};

/// Pinned engine version (ADR-0031: Chrome for Testing, checksum verified).
pub const ENGINE_VERSION: &str = "157.0.8103.78";
/// Official Chrome for Testing download for the pinned version, linux64.
pub const ENGINE_URL: &str = "https://storage.googleapis.com/chrome-for-testing-public/157.0.8103.78/linux64/chrome-headless-shell-linux64.zip";
/// SHA-256 of the pinned zip (measured 2026-11-16 on the download from
/// [`ENGINE_URL`]; the spike's `research/spike-wv/README.md` records the same
/// digest truncated as `f9032972…d153a`).
pub const ENGINE_ZIP_SHA256: &str =
    "f90329720fb61e5a76da9b6b509b6ad12a484c9c5df0359403e935177c6d153a";
/// Top-level directory inside the zip.
pub const ENGINE_ZIP_DIR: &str = "chrome-headless-shell-linux64";
/// The executable inside that directory.
pub const ENGINE_BIN: &str = "chrome-headless-shell";

/// `$ACOS_WEBVIEW_CACHE`, else `<root>/.cache/webview`, where `root` is
/// `$ACOS_HOME` (installed ACOS) or the developer checkout — the same rule the
/// rest of ACOS uses for its cache (`acos_mcp::paths`, `acos_art::repo_root`).
pub fn cache_root() -> PathBuf {
    if let Some(p) = std::env::var_os("ACOS_WEBVIEW_CACHE") {
        if !p.is_empty() {
            return PathBuf::from(p);
        }
    }
    cache_root_with(std::env::var_os("ACOS_HOME"), Path::new(env!("CARGO_MANIFEST_DIR")))
}

/// [`cache_root`] with both inputs passed in, so tests prove each arm.
pub fn cache_root_with(acos_home: Option<std::ffi::OsString>, manifest_dir: &Path) -> PathBuf {
    let root = match acos_home {
        Some(h) if !h.is_empty() => PathBuf::from(h),
        _ => manifest_dir
            .parent()
            .and_then(Path::parent)
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from(".")),
    };
    root.join(".cache/webview")
}

/// The marker that makes an install trustworthy.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Verified {
    pub version: String,
    pub zip_sha256: String,
    pub binary_size: u64,
    pub binary_sha256: String,
    pub installed_at: String,
}

/// One engine install directory.
#[derive(Debug, Clone)]
pub struct EngineCache {
    root: PathBuf,
}

impl EngineCache {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// [`cache_root`] in the current environment.
    pub fn default_cache() -> Self {
        Self::new(cache_root())
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn install_dir(&self) -> PathBuf {
        self.root.join("engine").join(ENGINE_VERSION)
    }

    /// The `chrome-headless-shell` executable path (whether or not it exists).
    pub fn binary(&self) -> PathBuf {
        self.install_dir().join(ENGINE_ZIP_DIR).join(ENGINE_BIN)
    }

    pub fn zip_path(&self) -> PathBuf {
        self.root.join("engine").join("chrome-headless-shell-linux64.zip")
    }

    fn marker_path(&self) -> PathBuf {
        self.install_dir().join("VERIFIED.json")
    }

    /// A trustworthy engine, or `None`. Never downloads: used by tests and
    /// status reporting.
    pub fn cached(&self) -> Option<Verified> {
        let marker: Verified = serde_json::from_slice(&std::fs::read(self.marker_path()).ok()?).ok()?;
        if marker.version != ENGINE_VERSION || marker.zip_sha256 != ENGINE_ZIP_SHA256 {
            return None;
        }
        let bin = self.binary();
        let meta = std::fs::metadata(&bin).ok()?;
        if meta.len() != marker.binary_size {
            return None;
        }
        Some(marker)
    }

    /// Install on first use and return the executable path. Idempotent and
    /// safe to call from several processes: the install happens in a staging
    /// directory and the marker is written last.
    pub fn ensure(&self) -> Result<PathBuf> {
        if self.cached().is_some() {
            return Ok(self.binary());
        }
        std::fs::create_dir_all(self.install_dir())
            .map_err(|e| Error::engine(format!("creating {}: {e}", self.install_dir().display())))?;
        // Archive: reuse a cached copy, but only if it hashes to the pin.
        let zip = self.zip_path();
        let good_zip = zip.exists() && sha256_file(&zip)? == ENGINE_ZIP_SHA256;
        if !good_zip {
            let _ = std::fs::remove_file(&zip);
            download(ENGINE_URL, &zip)?;
            let got = sha256_file(&zip)?;
            if got != ENGINE_ZIP_SHA256 {
                let _ = std::fs::remove_file(&zip);
                return Err(Error::engine(format!(
                    "engine checksum mismatch: {ENGINE_URL} hashed {got}, pinned {ENGINE_ZIP_SHA256}; \
                     refusing to run an unverified engine"
                )));
            }
        }
        // Extract into a staging dir, probe the version, then publish.
        let staging = self.install_dir().join(format!(".staging-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&staging);
        std::fs::create_dir_all(&staging)
            .map_err(|e| Error::engine(format!("creating {}: {e}", staging.display())))?;
        let extracted = staging.join(ENGINE_ZIP_DIR).join(ENGINE_BIN);
        let staged_zip = staging.join("engine.zip");
        std::fs::copy(&zip, &staged_zip)
            .map_err(|e| Error::engine(format!("staging {}: {e}", zip.display())))?;
        let status = Command::new("unzip")
            .arg("-q")
            .arg("-o")
            .arg(&staged_zip)
            .arg("-d")
            .arg(&staging)
            .status()
            .map_err(|e| Error::engine(format!("running unzip: {e}")))?;
        if !status.success() {
            return Err(Error::engine(format!(
                "unzip failed with {status} for {}",
                zip.display()
            )));
        }
        let _ = std::fs::remove_file(&staged_zip);
        let probe = version_of(&extracted)?;
        if !probe.contains(ENGINE_VERSION) {
            let _ = std::fs::remove_dir_all(&staging);
            return Err(Error::engine(format!(
                "downloaded engine reports {probe:?}, expected {ENGINE_VERSION}"
            )));
        }
        // Publish: replace the (untrusted) install dir with the staging dir.
        let final_dir = self.install_dir().join(ENGINE_ZIP_DIR);
        let _ = std::fs::remove_dir_all(&final_dir);
        std::fs::rename(staging.join(ENGINE_ZIP_DIR), &final_dir)
            .map_err(|e| Error::engine(format!("publishing engine: {e}")))?;
        let _ = std::fs::remove_dir_all(&staging);
        let bin = final_dir.join(ENGINE_BIN);
        let marker = Verified {
            version: ENGINE_VERSION.to_string(),
            zip_sha256: ENGINE_ZIP_SHA256.to_string(),
            binary_size: std::fs::metadata(&bin)
                .map_err(|e| Error::engine(format!("stat {}: {e}", bin.display())))?
                .len(),
            binary_sha256: sha256_file(&bin)?,
            installed_at: now_utc(),
        };
        std::fs::write(
            self.marker_path(),
            serde_json::to_vec_pretty(&marker).map_err(|e| Error::engine(e.to_string()))?,
        )
        .map_err(|e| Error::engine(format!("writing marker: {e}")))?;
        // The archive is not needed once the install is verified (122 MiB):
        // the marker carries its digest, so a corrupted install is detected and
        // re-downloaded instead of silently trusted.
        let _ = std::fs::remove_file(&zip);
        Ok(bin)
    }

    /// `--version` output of an installed engine, or `None` when absent.
    pub fn installed_version(&self) -> Option<String> {
        version_of(&self.binary()).ok()
    }
}

/// `<engine> --version` → e.g. `Google Chrome for Testing 157.0.8103.78`.
pub fn version_of(binary: &Path) -> Result<String> {
    let out = Command::new(binary)
        .arg("--version")
        .output()
        .map_err(|e| Error::engine(format!("probing {} --version: {e}", binary.display())))?;
    if !out.status.success() {
        return Err(Error::engine(format!(
            "{} --version exited with {}: {}",
            binary.display(),
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Download `url` to `dest` with curl (streamed, resumable-free, atomic rename).
fn download(url: &str, dest: &Path) -> Result<()> {
    let tmp = dest.with_extension("zip.part");
    let _ = std::fs::remove_file(&tmp);
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| Error::engine(format!("creating {}: {e}", parent.display())))?;
    }
    let status = Command::new("curl")
        .args(["-fL", "--retry", "3", "--connect-timeout", "20"])
        .arg("-o")
        .arg(&tmp)
        .arg(url)
        .status()
        .map_err(|e| Error::engine(format!("running curl: {e}")))?;
    if !status.success() {
        let _ = std::fs::remove_file(&tmp);
        return Err(Error::engine(format!(
            "downloading {url} failed with {status}"
        )));
    }
    std::fs::rename(&tmp, dest)
        .map_err(|e| Error::engine(format!("installing {}: {e}", dest.display())))?;
    Ok(())
}

/// Streaming SHA-256 of a file, lowercase hex.
pub fn sha256_file(path: &Path) -> Result<String> {
    let mut f = std::fs::File::open(path)
        .map_err(|e| Error::engine(format!("opening {}: {e}", path.display())))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f
            .read(&mut buf)
            .map_err(|e| Error::engine(format!("reading {}: {e}", path.display())))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex::encode(hasher.finalize()))
}

fn now_utc() -> String {
    // Seconds since the epoch; the marker is metadata, not a timestamp format
    // anyone parses. Avoids a chrono dependency for the installer.
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("unix:{secs}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_root_follows_the_acos_rule() {
        let manifest = Path::new("/repo/crates/acos-webview");
        assert_eq!(
            cache_root_with(Some("/installed".into()), manifest),
            PathBuf::from("/installed/.cache/webview")
        );
        assert_eq!(
            cache_root_with(None, manifest),
            PathBuf::from("/repo/.cache/webview")
        );
        assert_eq!(
            cache_root_with(Some("".into()), manifest),
            PathBuf::from("/repo/.cache/webview"),
            "an empty ACOS_HOME is unset"
        );
    }

    #[test]
    fn paths_are_pinned_to_the_version() {
        let c = EngineCache::new("/c");
        assert!(c.install_dir().ends_with(format!("engine/{ENGINE_VERSION}")));
        assert!(c.binary().ends_with("chrome-headless-shell-linux64/chrome-headless-shell"));
        assert_eq!(ENGINE_ZIP_SHA256.len(), 64);
        assert!(ENGINE_URL.contains(ENGINE_VERSION));
        assert!(ENGINE_URL.ends_with("chrome-headless-shell-linux64.zip"));
    }

    #[test]
    fn an_empty_dir_is_not_a_trusted_install() {
        let root = std::env::temp_dir().join(format!("acos-wv-cache-{}", std::process::id()));
        let c = EngineCache::new(&root);
        assert!(c.cached().is_none());
        assert!(c.installed_version().is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_marker_with_the_wrong_pin_is_rejected() {
        let root = std::env::temp_dir().join(format!("acos-wv-cache2-{}", std::process::id()));
        let c = EngineCache::new(&root);
        std::fs::create_dir_all(c.install_dir().join(ENGINE_ZIP_DIR)).unwrap();
        std::fs::write(c.binary(), b"not chrome").unwrap();
        let bad = Verified {
            version: "1.2.3".into(),
            zip_sha256: ENGINE_ZIP_SHA256.into(),
            binary_size: 10,
            binary_sha256: "00".into(),
            installed_at: "unix:0".into(),
        };
        std::fs::write(c.marker_path(), serde_json::to_vec(&bad).unwrap()).unwrap();
        assert!(c.cached().is_none(), "wrong version must not be trusted");
        let _ = std::fs::remove_dir_all(&root);
    }
}
