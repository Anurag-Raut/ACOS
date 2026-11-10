//! Native-APK launch gate (W59-A): ABI check + fail-closed confinement.
//!
//! Product native loading is opt-in (`--native` or `ACOS_NATIVE=1`) until the
//! security review in DECISIONS.md ADR-0018 passes. When it is requested and
//! the APK ships a `lib/<host-abi>/` directory, this module:
//!
//! 1. refuses an APK whose only native ABIs exclude the host (`UNSUPPORTED_ABI`,
//!    naming the ABIs the APK does provide);
//! 2. enters the `acos-sandbox` confinement **before** the session worker (and
//!    therefore before any guest code) starts, because seccomp is installed with
//!    `TSYNC` and must run while the process is single-threaded;
//! 3. fails closed with `CONFINEMENT_UNAVAILABLE` if any required layer cannot be
//!    established. It never proceeds to load native code unconfined.
//!
//! An APK with no native libraries is not gated: there is nothing to confine.

use std::path::{Path, PathBuf};

use acos_apk::{Apk, HOST_ABI};
use acos_runtime::session::SessionError;
use acos_sandbox::{confine, ConfineProfile, ConfinementReport};

/// Whether native loading was requested by flag or the `ACOS_NATIVE` env var.
///
/// Accepted truthy spellings: `1`, `true`, `yes`, `on` (case-insensitive).
pub fn native_requested(flag: bool) -> bool {
    if flag {
        return true;
    }
    matches!(
        std::env::var("ACOS_NATIVE")
            .unwrap_or_default()
            .to_ascii_lowercase()
            .as_str(),
        "1" | "true" | "yes" | "on"
    )
}

fn error(code: &'static str, message: impl Into<String>, details: Vec<(&str, String)>) -> SessionError {
    SessionError {
        code,
        message: message.into(),
        details: details
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect(),
    }
}

/// Check the APK's native ABI against the host.
///
/// Returns the parsed APK and whether it actually ships native code. An APK
/// with no native libs returns `(apk, false)`; the caller skips confinement.
pub fn check_apk(apk: &Path) -> Result<(Apk, bool), SessionError> {
    let parsed = Apk::open(apk).map_err(|e| {
        error(
            e.code(),
            format!("{}: {e}", apk.display()),
            vec![("apk", apk.display().to_string())],
        )
    })?;
    parsed.check_native_abi(HOST_ABI).map_err(|e| {
        let available = parsed.native_abis().join(", ");
        error(
            e.code(),
            e.message().to_string(),
            vec![
                ("requested_abi", HOST_ABI.to_string()),
                ("available_abis", available),
                ("apk", apk.display().to_string()),
            ],
        )
    })?;
    let has_native = parsed.has_native_libs();
    Ok((parsed, has_native))
}

/// Determine the VFS session root used by the runtime and make it exist now, so
/// the Landlock ruleset can name it before the runtime creates it.
///
/// If `ACOS_SESSION_ROOT` is set it is honoured (the runtime does the same);
/// otherwise a deterministic root under `.cache/sessions/w59a-<pid>` is chosen
/// and exported so the runtime uses exactly the confined root.
fn session_root() -> Result<PathBuf, SessionError> {
    let root = match std::env::var_os("ACOS_SESSION_ROOT") {
        Some(path) => PathBuf::from(path),
        None => {
            let root = PathBuf::from(format!(".cache/sessions/w59a-{}", std::process::id()));
            std::env::set_var("ACOS_SESSION_ROOT", &root);
            root
        }
    };
    std::fs::create_dir_all(root.join("data/user/0")).map_err(|e| {
        error(
            "CONFINEMENT_UNAVAILABLE",
            format!("cannot create session root {}: {e}", root.display()),
            vec![("root", root.display().to_string())],
        )
    })?;
    Ok(root)
}

/// The host boot cache the VM reads `/system` files from, if it exists.
fn boot_root() -> Option<PathBuf> {
    let candidates = [
        ".cache/boot/android-34",
        "target/tmp/boot/android-34",
    ];
    candidates
        .iter()
        .map(PathBuf::from)
        .find(|p| p.is_dir())
}

/// Enter confinement for a native launch.
///
/// `apk` is the APK whose directory hosts the extracted libs; only the data
/// root is writable. On success the process is confined and `confinement_active`
/// reports true, which is what `acos-native-loader` requires before mapping a
/// guest `.so`.
pub fn confine_native_launch(apk: &Path) -> Result<ConfinementReport, SessionError> {
    let root = session_root()?;
    let mut profile = ConfineProfile {
        read_only_paths: vec![PathBuf::from("/usr"), PathBuf::from("/lib"), PathBuf::from("/etc")],
        read_write_paths: vec![root.clone()],
        ..ConfineProfile::default()
    };
    if let Some(boot) = boot_root() {
        profile.read_only_paths.push(boot);
    }
    // Keep the explicit root visible to the runtime across `clearenv`.
    profile
        .env
        .push(("ACOS_SESSION_ROOT".to_string(), root.display().to_string()));
    if let Some(dir) = apk.parent() {
        profile.read_only_paths.push(dir.to_path_buf());
    }

    confine(&profile).map_err(|e| {
        error(
            e.code(),
            format!("native launch refused: {e}"),
            vec![
                ("apk", apk.display().to_string()),
                ("session_root", root.display().to_string()),
            ],
        )
    })
}
