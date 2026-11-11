//! Stage the app APK into the guest VFS and publish it as the app's
//! `sourceDir`, so the real framework can build a `PathClassLoader` and an
//! `AssetManager` over it.
//!
//! Real Android installs an APK under `/data/app/.../base.apk`; ACOS has no
//! package installer, so the trusted Rust side copies the fixture APK into the
//! session sandbox at `<dataDir>/base.apk` *before* `bindApplication`. From
//! then on the framework only ever sees the guest path:
//!
//! * `LoadedApk` uses `ApplicationInfo.sourceDir` for
//!   `ClassLoaderFactory.createClassLoader` -> `DexFile.openDexFileNative`,
//!   which resolves the guest path through the VFS (no host path leaks into
//!   the guest).
//! * `ResourcesManager` uses the same `sourceDir` for
//!   `AssetManager.addAssetPath` / `ApkAssets.nativeLoad`; the resource natives
//!   map `/data/**` onto the same VFS root.
//!
//! The copy is a byte-for-byte copy of the APK (the dex inside is exactly the
//! one the build produced). Nothing is rewritten or re-signed.

use std::path::Path;

use acos_services::pms::AppInfo;
use acos_vm::Vm;

/// Android's per-package data sub-directories.
///
/// Real `installd` creates these at install time; the framework's
/// `ContextImpl.ensurePrivateDirExists` only calls `mkdir` (not `mkdir -p`),
/// so it relies on the package directory already existing. ACOS has no package
/// installer, so the trusted side creates the tree before `bindApplication`.
const APP_DATA_SUBDIRS: &[&str] = &["files", "cache", "code_cache", "no_backup"];

/// Create `<root>/<guest dir>/{files,cache,code_cache,no_backup}` for every
/// guest data directory under `/data/**`.
///
/// `root` is the VFS session root. Directories outside `/data/**` are ignored:
/// the VFS only maps that subtree, and `stage` already rejects a `dataDir`
/// outside it. The operation is idempotent.
fn ensure_app_data_dirs(root: &Path, guest_dirs: &[&str]) -> Result<(), String> {
    for guest in guest_dirs {
        let guest = guest.trim_end_matches('/');
        if !guest.starts_with("/data/") {
            continue;
        }
        let base = root.join(guest.trim_start_matches('/'));
        std::fs::create_dir_all(&base)
            .map_err(|e| format!("apk_loader: create {}: {e}", base.display()))?;
        for sub in APP_DATA_SUBDIRS {
            let dir = base.join(sub);
            std::fs::create_dir_all(&dir)
                .map_err(|e| format!("apk_loader: create {}: {e}", dir.display()))?;
        }
    }
    Ok(())
}

/// Where the APK lives inside the guest sandbox.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StagedApk {
    /// Guest path (`/data/user/<n>/<pkg>/base.apk`).
    pub guest_path: String,
    /// Payload size in bytes.
    pub size: u64,
}

/// Copy `apk_path` into `app.data_dir` as `base.apk` and point
/// `app.source_dir` / `public_source_dir` at the guest path.
///
/// The directory tree is created under the VFS session root (the same root the
/// guest's `java.io`/`libcore.io` natives use). The APK is **not** registered
/// with the VM here: the framework's own `DexFile.openDexFileNative` does that
/// when it builds the class loader, which is what makes the class-loader test
/// meaningful.
pub fn stage(vm: &Vm, apk_path: &Path, app: &mut AppInfo) -> Result<StagedApk, String> {
    let bytes = std::fs::read(apk_path)
        .map_err(|e| format!("apk_loader: read {}: {e}", apk_path.display()))?;
    let guest_dir = app.data_dir.trim_end_matches('/').to_string();
    if !guest_dir.starts_with("/data/") {
        return Err(format!(
            "apk_loader: dataDir {guest_dir} is not under /data (the VFS only maps /data/**)"
        ));
    }
    let rel = guest_dir.trim_start_matches('/');
    let host_dir = vm.vfs.root().join(rel);
    std::fs::create_dir_all(&host_dir)
        .map_err(|e| format!("apk_loader: create {}: {e}", host_dir.display()))?;
    // Create the credential- and device-protected package trees the real
    // framework expects to exist before the app runs (see APP_DATA_SUBDIRS).
    ensure_app_data_dirs(
        vm.vfs.root(),
        &[
            app.data_dir.as_str(),
            app.credential_protected_data_dir.as_str(),
            app.device_protected_data_dir.as_str(),
        ],
    )?;
    let guest_path = format!("{guest_dir}/base.apk");
    let host_path = host_dir.join("base.apk");
    std::fs::write(&host_path, &bytes)
        .map_err(|e| format!("apk_loader: write {}: {e}", host_path.display()))?;

    app.source_dir = guest_path.clone();
    app.public_source_dir = guest_path.clone();
    Ok(StagedApk {
        guest_path,
        size: bytes.len() as u64,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("acos-apk-loader-{tag}-{}", std::process::id()))
    }

    #[test]
    fn app_data_dirs_are_created_for_both_storage_trees() {
        let root = scratch("both");
        let _ = std::fs::remove_dir_all(&root);
        ensure_app_data_dirs(
            &root,
            &["/data/user/0/com.example", "/data/user_de/0/com.example"],
        )
        .expect("create app data dirs");
        for tree in ["data/user/0/com.example", "data/user_de/0/com.example"] {
            for sub in APP_DATA_SUBDIRS {
                let dir = root.join(tree).join(sub);
                assert!(dir.is_dir(), "missing {}", dir.display());
            }
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn app_data_dirs_ignore_paths_outside_data() {
        let root = scratch("skip");
        let _ = std::fs::remove_dir_all(&root);
        ensure_app_data_dirs(&root, &["/etc/passwd"]).expect("no-op for non-/data path");
        assert!(!root.join("etc/passwd").exists());
        let _ = std::fs::remove_dir_all(&root);
    }
}
