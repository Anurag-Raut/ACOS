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
/// otherwise a deterministic root under `<root>/.cache/sessions/w59a-<pid>` is
/// chosen and exported so the runtime uses exactly the confined root. `<root>`
/// is `$ACOS_HOME` in an installed layout, else the checkout, so an installed
/// `acos` never writes into a directory it was merely started from (REL1a).
fn session_root() -> Result<PathBuf, SessionError> {
    let root = match std::env::var_os("ACOS_SESSION_ROOT") {
        Some(path) => PathBuf::from(path),
        None => {
            let base = crate::root::runtime_root()
                .unwrap_or_else(|| PathBuf::from("."));
            let root = base.join(format!(".cache/sessions/w59a-{}", std::process::id()));
            std::env::set_var("ACOS_SESSION_ROOT", &root);
            root
        }
    };
    // `ACOS_SESSION_ROOT` names the only writable path a confined child gets.
    // A root owned by another user, or group/world-writable, would let that
    // user swap the app data; refuse it before creating anything under it
    // (W170-A E1). The code is the protocol's `UNSAFE_PATH`.
    if let Err((code, msg)) = acos_runtime::private_root::ensure_private_root(
        "ACOS_SESSION_ROOT",
        &root,
    ) {
        return Err(error(
            code.protocol_code(),
            msg,
            vec![
                ("root", root.display().to_string()),
                ("root_error", code.code().to_string()),
            ],
        ));
    }
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
    let env = std::env::var_os("ACOS_BOOT_DIR").map(PathBuf::from);
    if let Some(path) = env.filter(|path| path.is_dir()) {
        return Some(path);
    }
    // REL1a: an installed ACOS carries its own copy under ACOS_HOME.
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(root) = crate::root::runtime_root() {
        candidates.push(root.join(".cache/boot/android-34"));
    }
    candidates.push(PathBuf::from("target/tmp/boot/android-34"));
    candidates
        .into_iter()
        .find(|p| p.is_dir())
}

/// Enter confinement for a native launch.
///
/// `apk` is the APK whose directory hosts the extracted libs; only the data
/// root is writable. On success the process is confined and `confinement_active`
/// reports true, which is what `acos-native-loader` requires before mapping a
/// guest `.so`.
pub fn confine_native_launch(apk: &Path) -> Result<ConfinementReport, SessionError> {
    confine_native_launch_with_writable(apk, None, None)
}

/// Enter confinement with one extra writable path (used by `acos verify
/// --native`, which must write its evidence directory after the run).
///
/// The extra path is created when missing and is the only addition to the
/// writable surface; everything else is identical to
/// [`confine_native_launch`].
pub fn confine_native_launch_with_writable(
    apk: &Path,
    extra_writable: Option<&Path>,
    extra_read: Option<&Path>,
) -> Result<ConfinementReport, SessionError> {
    let root = session_root()?;
    // Read surface is exactly what native loading needs (W67A #13): the VFS
    // session root (writable), the boot-cache `/system` mapping, and the APK's
    // extracted-library directory. Host `/usr`, `/lib` and `/etc` are **not**
    // granted: the custom ELF loader resolves `DT_NEEDED` from the VFS/APK
    // search paths, and granting `/lib` was what made a traversing `DT_NEEDED`
    // (finding #4) useful. Read-only roots are added only when they exist.
    if let Some(extra) = extra_writable {
        std::fs::create_dir_all(extra).map_err(|e| {
            error(
                "CONFINEMENT_UNAVAILABLE",
                format!("cannot create write path {}: {e}", extra.display()),
                vec![("path", extra.display().to_string())],
            )
        })?;
    }
    let mut read_write_paths = vec![root.clone()];
    if let Some(extra) = extra_writable {
        read_write_paths.push(extra.to_path_buf());
    }
    let mut read_only_paths = Vec::new();
    if let Some(extra) = extra_read {
        if let Some(parent) = extra.parent() {
            read_only_paths.push(if parent.as_os_str().is_empty() {
                PathBuf::from(".")
            } else {
                parent.to_path_buf()
            });
        }
    }
    let mut profile = ConfineProfile {
        read_only_paths,
        read_write_paths,
        ..ConfineProfile::default()
    };
    if let Some(boot) = boot_root() {
        // The VM resolves `/system/fonts` through `ACOS_FONT_DIR`. `confine`
        // runs `clearenv`, so the caller's value is gone by the time the app
        // first shapes text. Repoint it at the boot tree we just granted
        // read-only; without it every text measurement fails with
        // `acos-render font collection missing` (hello-jni sets an ActionBar
        // title during `onCreate`, before any later font load).
        let fonts = boot.join("fonts");
        if fonts.is_dir() {
            profile
                .env
                .push(("ACOS_FONT_DIR".to_string(), fonts.display().to_string()));
        }
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
