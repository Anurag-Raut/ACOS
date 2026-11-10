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
