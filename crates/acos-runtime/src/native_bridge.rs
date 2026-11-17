//! Native APK bridge (W72-A): attach the real JNI runtime to a product launch.
//!
//! `acos-runtime` is the one crate that owns the `Vm` + framework wiring; this
//! module is the seam that lets an APK's `System.loadLibrary` /
//! `System.load` / `Runtime.nativeLoad` / `BaseDexClassLoader.findLibrary`
//! resolve and run the `.so` files shipped in its `lib/<abi>/` directory.
//!
//! # Confinement
//!
//! Native code is **only** executed when the process reports an active
//! `acos-sandbox` confinement (ADR-0022). The JNI runtime is created with
//! `JniConfig::require_confinement = true` (the default), and
//! `acos-native-loader` refuses to map a guest ELF unless
//! `acos_sandbox::confinement_active()` is true. The launch gate
//! (`acos-cli::native_launch`) establishes the confinement *before* the session
//! worker is spawned, so this module never has to enter a sandbox itself; if
//! confinement is missing the first `System.loadLibrary` fails closed with
//! `CONFINEMENT_UNAVAILABLE` and the session survives.
//!
//! # Default behaviour
//!
//! When the caller does not request native code (`--native` / `ACOS_NATIVE=1`)
//! the bridge is not created at all: no JNI host is installed and the VM
//! reports `UNSUPPORTED_JAVA_API` for a library load, with a hint to re-run with
//! `--native`.

use std::path::{Path, PathBuf};

use acos_apk::{Apk, HOST_ABI};
use acos_jni::{JniConfig, JniRuntime};
use acos_vm::Vm;

use crate::session::SessionError;

/// The JNI runtime attached to a native launch, plus the VFS lib directory its
/// APK's native code was extracted into.
///
/// Dropping the bridge detaches the JNI host and clears the process-global
/// runtime slot, so a subsequent session does not see a dangling environment.
pub struct NativeBridge {
    jni: JniRuntime,
    lib_dir: PathBuf,
    abi: String,
    libraries: Vec<String>,
    /// `(class descriptor, name, signature)` of every `native` method declared
    /// by the APK's DEX files. Registered with the JNI dispatcher at attach
    /// time, before the app's classes are loaded, so the VM's `bind_dex_native`
    /// binds them as soon as the class is parsed.
    natives: Vec<(String, String, String)>,
}

impl std::fmt::Debug for NativeBridge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NativeBridge")
            .field("lib_dir", &self.lib_dir)
            .field("abi", &self.abi)
            .field("libraries", &self.libraries)
            .finish_non_exhaustive()
    }
}

impl Drop for NativeBridge {
    fn drop(&mut self) {
        self.jni.detach();
    }
}

impl NativeBridge {
    /// The APK's DEX `native` method declarations, without extracting anything.
    ///
    /// Used to install the JNI dispatcher even when native execution is off, so
    /// the default path reports `UNSUPPORTED_JAVA_API` with the `--native` hint.
    pub fn declarations(apk_path: &Path) -> Result<Vec<(String, String, String)>, SessionError> {
        let apk = Apk::open(apk_path).map_err(|e| {
            error(
                e.code(),
                format!("{}: {e}", apk_path.display()),
                vec![("apk", apk_path.display().to_string())],
            )
        })?;
        Ok(collect_native_declarations(&apk))
    }

    /// Prepare native execution for `apk_path`.
    ///
    /// * an APK with no `lib/` entries returns `Ok(None)` (nothing to do);
    /// * an APK with native code only for a non-host ABI fails with
    ///   `UNSUPPORTED_ABI` (naming every ABI it provides);
    /// * a hostile `lib/<abi>/<name>` entry fails with `ZIP_SLIP` before
    ///   anything is written;
    /// * on success every host-ABI library is extracted into
    ///   `<vfs_root>/data/user/0/<pkg>/lib/<abi>/` and a JNI runtime is created
    ///   with that directory as its search path.
    pub fn prepare(apk_path: &Path, vfs_root: &Path) -> Result<Option<NativeBridge>, SessionError> {
        let apk = Apk::open(apk_path).map_err(|e| {
            error(
                e.code(),
                format!("{}: {e}", apk_path.display()),
                vec![("apk", apk_path.display().to_string())],
            )
        })?;
        apk.check_native_abi(HOST_ABI).map_err(|e| {
            error(
                e.code(),
                e.message().to_string(),
                vec![
                    ("requested_abi", HOST_ABI.to_string()),
                    ("available_abis", apk.native_abis().join(", ")),
                    ("apk", apk_path.display().to_string()),
                ],
            )
        })?;
        if !apk.has_native_libs() {
            return Ok(None);
        }

        let package = apk
            .manifest()
            .map(|m| m.package.clone())
            .unwrap_or_else(|_| "app".to_string());
        let lib_dir = vfs_root
            .join("data")
            .join("user")
            .join("0")
            .join(sanitise_package(&package))
            .join("lib")
            .join(HOST_ABI);
        let extracted = apk.extract_native_libs(HOST_ABI, &lib_dir).map_err(|e| {
            error(
                e.code(),
                format!("extracting native libraries: {e}"),
                vec![
                    ("apk", apk_path.display().to_string()),
                    ("dest", lib_dir.display().to_string()),
                    ("abi", HOST_ABI.to_string()),
                ],
            )
        })?;
        let libraries = extracted.into_iter().map(|(name, _)| name).collect();
        let natives = collect_native_declarations(&apk);

        let config = JniConfig {
            search_paths: vec![lib_dir.clone()],
            apk: Some(apk_path.to_path_buf()),
            abi: HOST_ABI.to_string(),
            ..JniConfig::default()
        };
        let jni = JniRuntime::new(config);
        Ok(Some(NativeBridge {
            jni,
            lib_dir,
            abi: HOST_ABI.to_string(),
            libraries,
            natives,
        }))
    }

    /// Install the JNI host on `vm` (register the loader natives and bind the
    /// DEX `native` methods to the JNI dispatcher).
    pub fn attach(&self, vm: &mut Vm) -> Result<(), SessionError> {
        self.jni.attach(vm).map_err(|e| {
            error(
                e.code(),
                e.to_string(),
                vec![("stage", "native_bridge".to_string())],
            )
        })?;
        // The app's DEX is loaded later (during `bindApplication`), so register
        // its `native` methods now: `Vm::bind_dex_native` applies the JNI
        // dispatcher when the declaring class is parsed.
        for (class, name, sig) in &self.natives {
            vm.register_native(class, name, sig, acos_vm::natives::jni_glue::bridge());
        }
        Ok(())
    }

    /// The VFS directory the APK's native libraries were extracted into.
    #[must_use]
    pub fn lib_dir(&self) -> &Path {
        &self.lib_dir
    }

    /// The ABI the libraries were extracted for.
    #[must_use]
    pub fn abi(&self) -> &str {
        &self.abi
    }

    /// File names of the extracted libraries.
    #[must_use]
    pub fn libraries(&self) -> &[String] {
        &self.libraries
    }
}

/// Every DEX `native` method declared by the APK's DEX files.
fn collect_native_declarations(apk: &Apk) -> Vec<(String, String, String)> {
    let mut out = Vec::new();
    for dex in &apk.dex_files {
        let Ok(file) = acos_dex::DexFile::parse(dex.bytes.clone()) else {
            continue;
        };
        for def in file.class_defs() {
            let Ok(desc) = file.class_descriptor(def).map(str::to_string) else {
                continue;
            };
            let Ok(Some(cd)) = file.class_data(def) else {
                continue;
            };
            for m in cd.direct_methods.iter().chain(cd.virtual_methods.iter()) {
                if m.access_flags & acos_dex::access::NATIVE == 0 {
                    continue;
                }
                let Ok(mr) = file.method_ref(m.method_idx) else {
                    continue;
                };
                let params: Vec<String> = mr.parameters.iter().map(|p| p.to_string()).collect();
                let sig = format!("({}){}", params.concat(), mr.return_type);
                out.push((desc.clone(), mr.name.to_string(), sig));
            }
        }
    }
    out
}

/// Keep a package name usable as a single directory component.
fn sanitise_package(package: &str) -> String {
    let cleaned: String = package
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if cleaned.is_empty() || cleaned == "." || cleaned == ".." {
        "app".to_string()
    } else {
        cleaned
    }
}

fn error(
    code: &'static str,
    message: impl Into<String>,
    details: Vec<(&str, String)>,
) -> SessionError {
    SessionError {
        code,
        message: message.into(),
        details: details
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect(),
    }
}
