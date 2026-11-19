//! Where the pinned `pasta` binary is, in **one** place.
//!
//! ADR-0030's transport is a static `pasta` build of the pinned tag
//! [`TAG`] (`research/spike-net/artifacts/passt-build.txt` records the source
//! commit, the build command and the binary's sha256). Until NETREAL it was
//! only reachable through an agent-private path (`ACOS_NET_PASTA` pointing into
//! some `.cache/<track>/passt-src/pasta`), which is exactly why the real-network
//! scenario arms had never run in a gate: the capability was not part of the
//! product.
//!
//! This module is the product's answer to "which `pasta`?", and every consumer
//! reads it:
//!
//! * `acos doctor` reports `pasta` with the version it found (or `missing`);
//! * the relay supervisor (`acos-netd`) starts its NAT process from it;
//! * `acos-netd pasta [--json]` prints the resolution for scripts (the scenario
//!   runners use it to resolve a `needs_env: [pasta]` declaration);
//! * `tools/release/install.sh` installs the binary into the location this file
//!   documents ([`INSTALL_REL`] under the runtime root), so an install is
//!   self-contained.
//!
//! # The rule
//!
//! In order:
//!
//! 1. `ACOS_NET_PASTA` — an explicit path (the one override; a spike or a
//!    locally built binary names itself here).
//! 2. `<root>/libexec/pasta` — the install's own copy. `<root>` is the same
//!    runtime root every other asset uses: `$ACOS_HOME` when set, else the
//!    nearest ancestor of the executable or the cwd that carries it (which is
//!    the developer checkout during development). One location, so a checkout
//!    and an install differ only in which root it is.
//! 3. `$ACOS_PREFIX/share/acos/current/libexec/pasta` — the install's `current`
//!    link, for a caller that knows only the prefix.
//! 4. `pasta` on `PATH`.
//!
//! A candidate must be an existing executable file; a stale `ACOS_NET_PASTA`
//! that points at nothing ends the search with `missing` and is named in
//! [`searched`], never silently replaced by an install or PATH binary (a named path that does not work is a fact the operator has
//! to see).

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Serialize;

/// Environment variable naming the `pasta` binary explicitly (the override the
/// relay has always honoured; also what the spike scripts set).
pub const ENV: &str = "ACOS_NET_PASTA";

/// The spike's older spelling (`acos-art`'s standalone arm), still honoured by
/// the launcher. Documented here so there is one list of names.
pub const SPIKE_ENV: &str = "ACOS_SPIKE_NET_PASTA";

/// The pinned `pasta` tag. The binary reports it as `pasta <tag>`, and the
/// relay's `status` names both the found and the expected value.
pub const TAG: &str = "2026_10_02.cba3570";

/// Where the install carries the binary, relative to the runtime root
/// (`$ACOS_HOME`, else the checkout).
pub const INSTALL_REL: &str = "libexec/pasta";

/// How a resolution was reached; part of the JSON so a report can say *why*
/// this binary is the one (and so a wrong-root install is visible).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    /// `ACOS_NET_PASTA` named it.
    Env,
    /// `<root>/libexec/pasta` inside `$ACOS_HOME`.
    AcosHome,
    /// `<root>/libexec/pasta` beside/above the executable (an install, or the
    /// checkout a developer built in).
    RuntimeRoot,
    /// `$ACOS_PREFIX/share/acos/current/libexec/pasta`.
    Prefix,
    /// `pasta` on `PATH`.
    Path,
}

impl Source {
    /// The short name used in text output and in `doctor`'s detail line.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Source::Env => "env",
            Source::AcosHome => "acos_home",
            Source::RuntimeRoot => "runtime_root",
            Source::Prefix => "prefix",
            Source::Path => "path",
        }
    }
}

/// A resolved `pasta` binary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Resolution {
    /// The binary.
    pub path: PathBuf,
    /// Why this one.
    pub source: Source,
}

/// One place [`resolve`] looked, with the reason it was not used.
#[derive(Debug, Clone)]
pub struct Candidate {
    /// What was probed (a path, or `PATH`).
    pub what: String,
    /// The binary found there, or `None`.
    pub found: Option<Resolution>,
}

fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    match std::fs::metadata(path) {
        Ok(meta) => meta.is_file() && meta.permissions().mode() & 0o111 != 0,
        Err(_) => false,
    }
}

/// The runtime roots to probe, most specific first.
///
/// `acos_home` is passed in (rather than read here) so the rule is testable and
/// so a caller whose root comes from somewhere else (the installer, the
/// supervisor's own `ACOS_HOME`) uses the same function.
///
/// The discovered root is the same one every other runtime path is joined onto
/// (`acos_art::repo_root`): `$ACOS_HOME` when set, else the nearest ancestor of
/// the executable or the cwd that is a checkout (`Cargo.toml` + `crates/`). An
/// *install* needs no discovery: its wrapper exports `ACOS_HOME`.
#[must_use]
pub fn roots(
    acos_home: Option<&Path>,
    prefix: Option<&Path>,
    exe: Option<&Path>,
) -> Vec<(PathBuf, Source)> {
    let cwd = std::env::current_dir().ok();
    roots_from(acos_home, prefix, exe, cwd.as_deref())
}

/// [`roots`] with the cwd passed in: no process state is read through this one,
/// so the search order is provable in a test (and in which order).
#[must_use]
pub fn roots_from(
    acos_home: Option<&Path>,
    prefix: Option<&Path>,
    exe: Option<&Path>,
    cwd: Option<&Path>,
) -> Vec<(PathBuf, Source)> {
    let mut out: Vec<(PathBuf, Source)> = Vec::new();
    let mut push = |root: PathBuf, source: Source| {
        if !out.iter().any(|(seen, _)| *seen == root) {
            out.push((root, source));
        }
    };
    if let Some(home) = acos_home {
        push(home.to_path_buf(), Source::AcosHome);
    }
    if let Some(prefix) = prefix {
        push(
            prefix.join("share").join("acos").join("current"),
            Source::Prefix,
        );
    }
    let mut starts: Vec<&Path> = Vec::new();
    if let Some(dir) = exe.and_then(Path::parent) {
        starts.push(dir);
    }
    if let Some(cwd) = cwd {
        starts.push(cwd);
    }
    for start in starts {
        if let Some(root) = checkout_root(start) {
            push(root, Source::RuntimeRoot);
        }
    }
    out
}

/// The nearest ancestor of `start` that is a developer checkout.
///
/// The predicate is deliberately the one `acos_art::dev_root` uses; this crate
/// cannot call it (`acos-art` depends on this crate, never the reverse), and a
/// second *resolution* order would be the bug — so the marker is spelled out
/// here once and referenced from both places.
fn checkout_root(start: &Path) -> Option<PathBuf> {
    start
        .ancestors()
        .find(|dir| dir.join("Cargo.toml").is_file() && dir.join("crates").is_dir())
        .map(Path::to_path_buf)
}

/// The env overrides, in order (`ACOS_NET_PASTA`, then the spike's spelling).
fn env_candidates() -> Vec<(OsString, &'static str)> {
    [ENV, SPIKE_ENV]
        .iter()
        .filter_map(|name| std::env::var_os(name).map(|value| (value, *name)))
        .collect()
}

/// Resolve `pasta` from an explicit list of sources. The testable core of
/// [`resolve`]: no process state is read except through the arguments.
#[must_use]
pub fn resolve_with(
    envs: &[(OsString, &'static str)],
    roots: &[(PathBuf, Source)],
    path_lookup: impl Fn(&str) -> Option<PathBuf>,
) -> (Option<Resolution>, Vec<Candidate>) {
    let mut looked = Vec::new();
    for (value, name) in envs {
        let path = PathBuf::from(value);
        if is_executable(&path) {
            let found = Resolution {
                path,
                source: Source::Env,
            };
            looked.push(Candidate {
                what: format!("{name}={}", value.to_string_lossy()),
                found: Some(found.clone()),
            });
            return (Some(found), looked);
        }
        // A named path that does not work ends the search: the supervisor runs
        // an explicit override verbatim, so `doctor` falling through to PATH
        // here would report a healthy `pasta` the relay would then fail to
        // start. The operator named it; the answer is `missing`, with the
        // stale value in `searched`.
        looked.push(Candidate {
            what: format!("{name}={} (not an executable file)", value.to_string_lossy()),
            found: None,
        });
        return (None, looked);
    }
    for (root, source) in roots {
        let path = root.join(INSTALL_REL);
        let found = is_executable(&path).then(|| Resolution {
            path: path.clone(),
            source: *source,
        });
        looked.push(Candidate {
            what: path.display().to_string(),
            found: found.clone(),
        });
        if let Some(found) = found {
            return (Some(found), looked);
        }
    }
    let found = path_lookup("pasta").map(|path| Resolution {
        path,
        source: Source::Path,
    });
    looked.push(Candidate {
        what: "PATH".to_string(),
        found: found.clone(),
    });
    (found, looked)
}

/// Resolve the `pasta` binary using this process's environment (see the module
/// docs for the order).
#[must_use]
pub fn resolve() -> (Option<Resolution>, Vec<Candidate>) {
    let home = std::env::var_os("ACOS_HOME")
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty());
    let prefix = std::env::var_os("ACOS_PREFIX")
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty());
    let exe = std::env::current_exe().ok();
    resolve_with(
        &env_candidates(),
        &roots(home.as_deref(), prefix.as_deref(), exe.as_deref()),
        which,
    )
}

/// The resolved binary alone (the common case).
#[must_use]
pub fn resolved_path() -> Option<PathBuf> {
    resolve().0.map(|r| r.path)
}

/// `pasta --version`'s tag: the last word of the first line (`2026_10_02.cba3570`).
///
/// `None` when the file is not runnable; a binary that runs but answers
/// something else is returned as-is, so the caller can name the mismatch instead
/// of hiding it.
#[must_use]
pub fn version(path: &Path) -> Option<String> {
    let out = Command::new(path).arg("--version").output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let line = text.lines().next().unwrap_or("").trim();
    line.split_whitespace().last().map(str::to_string)
}

/// Whether `version` is the pinned tag.
#[must_use]
pub fn is_pinned(version: Option<&str>) -> bool {
    version == Some(TAG)
}

/// The human-readable list of everywhere [`resolve`] looked, for a `missing`
/// verdict (a doctor fix line, or the CLI's error).
#[must_use]
pub fn searched(candidates: &[Candidate]) -> String {
    candidates
        .iter()
        .map(|c| c.what.clone())
        .collect::<Vec<_>>()
        .join(", ")
}

fn which(tool: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(tool))
        .find(|p| is_executable(p))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "acos-pasta-{name}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("scratch");
        dir
    }

    fn fake_binary(path: &Path, body: &str) {
        use std::os::unix::fs::PermissionsExt;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("parent");
        }
        fs::write(path, body).expect("write");
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).expect("chmod");
    }

    /// The version is the binary's own answer, not a string this crate keeps: a
    /// stub that prints the pinned banner must read back as the pinned tag.
    #[test]
    fn version_reads_the_binarys_own_banner() {
        let dir = scratch("version");
        let bin = dir.join("pasta");
        fake_binary(
            &bin,
            &format!("#!/bin/sh\nprintf 'pasta {TAG}\\nCopyright Red Hat\\n'\n"),
        );
        let found = version(&bin).expect("version");
        assert_eq!(found, TAG);
        assert!(is_pinned(Some(&found)));
        assert!(!is_pinned(Some("0.0.0")));
        assert_eq!(version(&dir.join("absent")), None);
    }

    /// The install location wins over PATH: an install must run its own pinned
    /// transport, never whatever the host happens to have.
    #[test]
    fn install_root_wins_over_path() {
        let dir = scratch("root");
        let root = dir.join("install");
        let installed = root.join(INSTALL_REL);
        fake_binary(&installed, "#!/bin/sh\nexit 0\n");
        let on_path = dir.join("path-bin/pasta");
        fake_binary(&on_path, "#!/bin/sh\nexit 0\n");
        let (found, looked) = resolve_with(
            &[],
            &[(root.clone(), Source::AcosHome)],
            |_| Some(on_path.clone()),
        );
        let found = found.expect("install root resolves");
        assert_eq!(found.path, installed);
        assert_eq!(found.source, Source::AcosHome);
        // ...and it is the first candidate that answered.
        assert_eq!(looked[0].found.as_ref().map(|r| r.source), Some(Source::AcosHome));
    }

    /// An explicit path that does not work is reported, not papered over by a
    /// PATH or install binary: the operator named that path, and the relay runs
    /// an override verbatim (so `doctor` must agree with what the relay does).
    #[test]
    fn a_broken_override_is_missing_and_named() {
        let dir = scratch("broken");
        let on_path = dir.join("pasta");
        fake_binary(&on_path, "#!/bin/sh\nexit 0\n");
        let (found, looked) = resolve_with(
            &[(OsString::from("/nonexistent/pasta"), ENV)],
            &[],
            |_| Some(on_path.clone()),
        );
        assert_eq!(found, None, "no silent fallback to PATH");
        assert!(
            looked[0].what.starts_with("ACOS_NET_PASTA=/nonexistent/pasta"),
            "the broken override is in the record: {:?}",
            looked[0].what
        );
        assert!(searched(&looked).contains("/nonexistent/pasta"));
    }

    /// A good override is used verbatim, before the install and PATH.
    #[test]
    fn the_override_wins() {
        let dir = scratch("override");
        let named = dir.join("mine/pasta");
        fake_binary(&named, "#!/bin/sh\nexit 0\n");
        let root = dir.join("install");
        fake_binary(&root.join(INSTALL_REL), "#!/bin/sh\nexit 0\n");
        let (found, _) = resolve_with(
            &[(named.clone().into_os_string(), ENV)],
            &[(root, Source::AcosHome)],
            |_| None,
        );
        let found = found.expect("override resolves");
        assert_eq!(found.path, named);
        assert_eq!(found.source, Source::Env);
    }

    /// Nothing anywhere: `None`, and every probed location is named.
    #[test]
    fn missing_is_none_and_every_location_is_named() {
        let dir = scratch("missing");
        let root = dir.join("install");
        let (found, looked) = resolve_with(
            &[],
            &[(root.clone(), Source::AcosHome)],
            |_| None,
        );
        assert!(found.is_none());
        let text = searched(&looked);
        assert!(text.contains(&root.join(INSTALL_REL).display().to_string()), "{text}");
        assert!(text.contains("PATH"), "{text}");
    }

    /// The runtime root is discovered the way `repo_root` discovers it, so a
    /// checkout's `<root>/libexec/pasta` is found from any directory inside it
    /// (the binary lives in `<root>/.cache/<track>-target/release/`).
    #[test]
    fn roots_find_the_checkout_root_from_inside_it() {
        let dir = scratch("walk");
        let root = dir.join("checkout");
        std::fs::create_dir_all(root.join("crates")).expect("crates");
        std::fs::write(root.join("Cargo.toml"), "[workspace]\n").expect("manifest");
        let exe = root.join(".cache/target/release/acos");
        fake_binary(&exe, "#!/bin/sh\nexit 0\n");
        let found = roots_from(None, None, Some(&exe), None)
            .into_iter()
            .find(|(_, source)| *source == Source::RuntimeRoot);
        assert_eq!(found.map(|(p, _)| p), Some(root));
    }

    /// `$ACOS_HOME` wins and is probed even when the binary is not there (the
    /// `searched:` line must name the location the operator has to populate).
    #[test]
    fn acos_home_is_probed_even_when_empty() {
        let dir = scratch("home");
        let home = dir.join("install");
        let roots = roots_from(Some(&home), None, None, Some(&dir.join("elsewhere")));
        assert_eq!(roots[0].0, home);
        assert_eq!(roots[0].1, Source::AcosHome);
        let (found, looked) = resolve_with(&[], &roots, |_| None);
        assert!(found.is_none());
        assert!(searched(&looked).contains(&home.join(INSTALL_REL).display().to_string()));
    }
}
