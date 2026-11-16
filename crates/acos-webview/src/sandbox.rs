//! The bwrap sandbox the engine runs in (ADR-0031 item 1: "Chromium runs in its
//! own bwrap sandbox").
//!
//! What the sandbox guarantees, and what it deliberately does not:
//!
//! * A fresh mount namespace whose root is the host root **read-only**, with the
//!   user's home (`/home`), `/root` and `/run/user` replaced by empty tmpfs, a
//!   private `/tmp` and `/dev/shm`, the engine's install directory (read-only)
//!   and the throwaway profile (read-write) bound back at their own paths.
//!   **No user home is reachable**, so `~/.config/google-chrome`,
//!   `~/.config/chromium`, `~/.ssh` and the checkout's secrets are not visible
//!   to the page's engine, and the only writable paths are the profile and
//!   tmpfs.
//! * A private pid/ipc/uts/cgroup namespace, `--die-with-parent`, `--new-session`
//!   (no controlling terminal), all capabilities dropped, and a cleared
//!   environment that is rebuilt explicitly.
//! * Network is **not** unshared: the page fetch happens on the host and follows
//!   the host's network (ADR-0031 item 1). The guest's own network profile is
//!   enforced by request interception (`shouldInterceptRequest`), not by this
//!   sandbox.
//!
//! The argv is a pure function so a test can pin its shape without running it.

use std::path::{Path, PathBuf};

/// Directories masked by an empty tmpfs: the user's home (so no
/// `~/.config/google-chrome`, no `~/.ssh`, no checkout secrets), root's home and
/// the per-user runtime dir (session bus, Wayland/X sockets).
const MASKED: &[&str] = &["/home", "/root", "/run/user"];

/// Build the bwrap argv for `engine` with a profile directory at `profile`.
///
/// Measured constraints (2026-11-16, this crate's evidence in
/// `results/handoff/WVB/`):
/// * an empty-root layout (`--tmpfs /` + individual `--ro-bind`s) makes the
///   headless shell die with SIGTRAP and no output, so the root is bound
///   read-only in one step and individual paths are masked instead;
/// * `/home` must be masked *before* the profile is bound back, otherwise the
///   profile (which lives under the ACOS cache in the checkout) disappears —
///   bwrap applies mounts in order, later ones win;
/// * Chromium's own namespace sandbox works in this layout, so it stays on.
pub fn bwrap_argv(engine: &Path, profile: &Path, engine_args: &[String]) -> Vec<String> {
    let engine_dir = engine.parent().unwrap_or(engine);
    let mut a: Vec<String> = [
        "--unshare-pid",
        "--unshare-ipc",
        "--unshare-uts",
        "--unshare-cgroup-try",
        "--die-with-parent",
        "--new-session",
        "--cap-drop",
        "ALL",
        "--clearenv",
        // The host root, read-only, in one mount.
        "--ro-bind",
        "/",
        "/",
    ]
    .iter()
    .map(|s| (*s).to_string())
    .collect();
    for dir in MASKED {
        a.push("--tmpfs".into());
        a.push((*dir).into());
    }
    a.push("--proc".into());
    a.push("/proc".into());
    a.push("--dev-bind".into());
    a.push("/dev".into());
    a.push("/dev".into());
    a.push("--tmpfs".into());
    a.push("/tmp".into());
    a.push("--tmpfs".into());
    a.push("/dev/shm".into());
    // The engine install directory (read-only) and the throwaway profile (rw).
    a.push("--ro-bind".into());
    a.push(engine_dir.to_string_lossy().into_owned());
    a.push(engine_dir.to_string_lossy().into_owned());
    a.push("--bind".into());
    a.push(profile.to_string_lossy().into_owned());
    a.push(profile.to_string_lossy().into_owned());
    for (k, v) in [
        ("HOME", profile.to_string_lossy().into_owned()),
        ("TMPDIR", "/tmp".into()),
        ("XDG_CACHE_HOME", format!("{}/cache", profile.display())),
        ("XDG_CONFIG_HOME", format!("{}/config", profile.display())),
        ("PATH", "/usr/bin:/bin".into()),
    ] {
        a.push("--setenv".into());
        a.push(k.into());
        a.push(v);
    }
    a.push("--chdir".into());
    a.push(engine_dir.to_string_lossy().into_owned());
    a.push("--".into());
    a.push(engine.to_string_lossy().into_owned());
    a.extend(engine_args.iter().cloned());
    a
}

/// The engine's own command line. Everything here is either required by the
/// frame contract (`--enable-begin-frame-control` +
/// `--run-all-compositor-stages-before-draw` make `HeadlessExperimental.beginFrame`
/// legal and deterministic) or suppresses host-visible side effects (no
/// first-run traffic, no metrics, no crash uploads, no extensions).
pub fn engine_args(profile: &Path) -> Vec<String> {
    let mut v: Vec<String> = [
        "--headless",
        "--enable-begin-frame-control",
        "--run-all-compositor-stages-before-draw",
        "--disable-new-content-rendering-timeout",
        "--disable-gpu",
        "--no-first-run",
        "--no-default-browser-check",
        "--disable-background-networking",
        "--disable-background-timer-throttling",
        "--disable-breakpad",
        "--disable-component-update",
        "--disable-domain-reliability",
        "--disable-extensions",
        "--disable-sync",
        "--metrics-recording-only",
        "--mute-audio",
        "--touch-events=enabled",
        "--no-pings",
        "--font-render-hinting=none",
        // No TCP listener for DevTools: fd 3/4 only.
        "--remote-debugging-pipe",
    ]
    .iter()
    .map(|s| (*s).to_string())
    .collect();
    v.push(format!("--user-data-dir={}", profile.display()));
    v
}

/// A profile directory under the ACOS cache: throwaway, never a browser profile
/// the user owns. Removed by [`ProfileDir::drop`] unless `ACOS_WEBVIEW_KEEP_PROFILE`
/// is set (debugging only).
pub struct ProfileDir {
    path: PathBuf,
    keep: bool,
}

impl ProfileDir {
    pub fn create(root: &Path) -> std::io::Result<Self> {
        if let Err(e) = std::fs::create_dir_all(root) {
            return Err(e);
        }
        let unique = format!(
            "profile-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        );
        let path = root.join(unique);
        std::fs::create_dir_all(&path)?;
        // 0700: the profile is ours alone even if the parent is shared.
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700));
        Ok(Self {
            path,
            keep: std::env::var_os("ACOS_WEBVIEW_KEEP_PROFILE").is_some(),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for ProfileDir {
    fn drop(&mut self) {
        if !self.keep {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn argv_isolates_home_and_keeps_the_pipe() {
        let engine = Path::new(
            "/cache/webview/engine/1/chrome-headless-shell-linux64/chrome-headless-shell",
        );
        let profile = Path::new("/cache/webview/run/profile-1");
        let args = engine_args(profile);
        let argv = bwrap_argv(engine, profile, &args);
        assert_eq!(argv.first().map(String::as_str), Some("--unshare-pid"));
        for flag in [
            "--unshare-pid",
            "--unshare-ipc",
            "--unshare-uts",
            "--die-with-parent",
            "--new-session",
            "--cap-drop",
            "--clearenv",
            "--ro-bind",
        ] {
            assert!(argv.iter().any(|a| a == flag), "missing {flag}: {argv:?}");
        }
        // The host root is bound read-only, and every user home is masked.
        let root_bind = argv
            .iter()
            .position(|a| a == "--ro-bind")
            .expect("--ro-bind");
        assert_eq!(&argv[root_bind + 1..root_bind + 3], ["/", "/"]);
        for masked in MASKED {
            let at = argv
                .iter()
                .position(|a| a == masked)
                .unwrap_or_else(|| panic!("{masked} not masked"));
            assert_eq!(argv[at - 1], "--tmpfs");
        }
        // The mask comes before the profile bind, which brings the profile back.
        let mask_home = argv.iter().position(|a| a == "/home").expect("/home");
        let bind = argv.iter().position(|a| a == "--bind").expect("--bind");
        assert!(mask_home < bind, "the profile bind must follow the mask");
        assert_eq!(argv[bind + 1], profile.to_string_lossy());
        let home = argv.iter().position(|a| a == "HOME").expect("HOME");
        assert_eq!(argv[home + 1], profile.to_string_lossy());
        // Engine flags come after `--`, so bwrap cannot eat one as its own.
        let sep = argv.iter().position(|a| a == "--").expect("--");
        assert_eq!(argv[sep + 1], engine.to_string_lossy());
        assert!(argv[sep..].iter().any(|a| a == "--remote-debugging-pipe"));
        assert!(argv[sep..]
            .iter()
            .any(|a| a == "--enable-begin-frame-control"));
    }

    #[test]
    fn profile_is_throwaway_and_unique() {
        let root = std::env::temp_dir().join(format!("acos-wv-prof-{}", std::process::id()));
        let a = ProfileDir::create(&root).expect("profile a");
        let b = ProfileDir::create(&root).expect("profile b");
        assert_ne!(a.path(), b.path());
        let pa = a.path().to_path_buf();
        assert!(pa.is_dir());
        drop(a);
        assert!(!pa.exists(), "profile must be removed on drop");
        let _ = std::fs::remove_dir_all(&root);
    }
}
