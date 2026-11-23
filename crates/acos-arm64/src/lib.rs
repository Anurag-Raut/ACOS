//! # `acos-arm64` — the aarch64 transport
//!
//! An APK whose only native ABI is `arm64-v8a` cannot be mapped by the
//! x86_64 in-process loader. This crate provides the **transport** half of the
//! arm64 plan in `docs/ARM64.md`: it runs an aarch64 ELF under
//! `qemu-aarch64` (user-mode) **inside** the `acos-sandbox` confinement, so a
//! later step can load real `lib/arm64-v8a/*.so` libraries.
//!
//! ## Why seccomp is deferred
//!
//! `acos_sandbox::confine` installs the seccomp filter inline, and that filter
//! denies `execve`. The transport must `execve(qemu-aarch64)`, so it cannot
//! install seccomp first. The arm64 profile therefore uses
//! `SeccompMode::DeferredPreload`:
//!
//! 1. the `arm64_child` launcher applies `PR_SET_NO_NEW_PRIVS`, fd/env/rlimit
//!    hygiene and a Landlock ruleset, then `execve`s `qemu-aarch64`;
//! 2. an `LD_PRELOAD` guard (`acos-seccomp-guard`, built from the **same**
//!    audited filter builder) installs seccomp inside the qemu process, after
//!    the dynamic linker has run and before `main`;
//! 3. the guard reports `OK`/`ERR` over a status pipe held across the exec, so
//!    [`launch`] never treats a child as confined on faith.
//!
//! ## Fail closed
//!
//! A missing `qemu-aarch64`, a missing bionic sysroot (`linker64` + bionic
//! libs), or a missing guard produces a structured [`Arm64Error`] with code
//! [`ARM64_UNAVAILABLE`] and an exact remedy — never a silent fallback and
//! never a fake pass. The x86_64 path is untouched: this crate is only entered
//! by a caller that has already selected the arm64 transport.
#![deny(unsafe_op_in_unsafe_fn)]

use std::ffi::OsString;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

/// `ARM64_UNAVAILABLE`: the transport cannot be assembled (missing qemu,
/// bionic sysroot, guard, or a failed deferred seccomp install). The
/// `remedy` field names the exact command or env var that fixes it.
pub const ARM64_UNAVAILABLE: &str = "ARM64_UNAVAILABLE";

/// `UNSUPPORTED_ABI`: the APK ships native code, but not for an ABI this
/// host can execute with the arm64 transport.
pub const UNSUPPORTED_ABI: &str = "UNSUPPORTED_ABI";

/// The guest instruction set a launch targets.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum GuestAbi {
    /// `lib/x86_64` — the existing in-process loader (not this crate).
    X86_64,
    /// `lib/arm64-v8a` — this qemu transport.
    Arm64V8a,
}

impl GuestAbi {
    /// The directory name under `lib/` in the APK.
    pub fn dir(self) -> &'static str {
        match self {
            GuestAbi::X86_64 => "x86_64",
            GuestAbi::Arm64V8a => "arm64-v8a",
        }
    }

    /// Parse an APK `lib/<abi>` directory name.
    pub fn from_dir(dir: &str) -> Option<Self> {
        match dir {
            "x86_64" => Some(GuestAbi::X86_64),
            "arm64-v8a" => Some(GuestAbi::Arm64V8a),
            _ => None,
        }
    }
}

/// Pick the guest ABI for a set of APK `lib/<abi>` directories.
///
/// Prefers the native `x86_64` loader; otherwise falls back to the arm64 qemu
/// transport. An APK with no native code returns [`GuestAbi::X86_64`] (there
/// is nothing to execute). Anything else is a structured
/// [`UNSUPPORTED_ABI`] error naming the available ABIs.
pub fn select_guest_abi(available: &[String]) -> Result<GuestAbi, Arm64Error> {
    if available.is_empty() {
        return Ok(GuestAbi::X86_64);
    }
    if available.iter().any(|a| a == "x86_64") {
        return Ok(GuestAbi::X86_64);
    }
    if available.iter().any(|a| a == "arm64-v8a") {
        return Ok(GuestAbi::Arm64V8a);
    }
    Err(Arm64Error::unsupported_abi(available))
}

/// A structured, serialisable transport error.
///
/// `code` is the machine-readable protocol code ([`ARM64_UNAVAILABLE`] or
/// [`UNSUPPORTED_ABI`]); `message` explains what was missing; `remedy` is the
/// exact fix. `path` is the path that was probed, when one applies.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct Arm64Error {
    /// Machine-readable code.
    pub code: &'static str,
    /// Human-readable explanation.
    pub message: String,
    /// The exact remediation (a command or environment variable).
    pub remedy: String,
    /// The path that was probed, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

impl Arm64Error {
    /// The transport is not assemblable. `remedy` must be actionable.
    pub fn unavailable(message: impl Into<String>, remedy: impl Into<String>) -> Self {
        Arm64Error {
            code: ARM64_UNAVAILABLE,
            message: message.into(),
            remedy: remedy.into(),
            path: None,
        }
    }

    /// Same as [`Arm64Error::unavailable`] but records the probed path.
    pub fn unavailable_at(
        message: impl Into<String>,
        remedy: impl Into<String>,
        path: impl AsRef<Path>,
    ) -> Self {
        Arm64Error {
            path: Some(path.as_ref().display().to_string()),
            ..Arm64Error::unavailable(message, remedy)
        }
    }

    /// The APK ships no ABI this host can run.
    pub fn unsupported_abi(available: &[String]) -> Self {
        Arm64Error {
            code: UNSUPPORTED_ABI,
            message: format!(
                "APK has no native libraries for a supported ABI; available: [{}]",
                available.join(", ")
            ),
            remedy: "rebuild the APK for x86_64 or arm64-v8a (add an ABI split)".to_string(),
            path: None,
        }
    }
}

impl std::fmt::Display for Arm64Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}: {} — remedy: {}",
            self.code, self.message, self.remedy
        )
    }
}

impl std::error::Error for Arm64Error {}

impl From<io::Error> for Arm64Error {
    fn from(e: io::Error) -> Self {
        Arm64Error::unavailable(format!("io error: {e}"), "inspect the path and retry")
    }
}

/// The exact remedy string used whenever the bionic sysroot is missing.
pub const SYSROOT_REMEDY: &str =
    "run `tools/fetch/fetch-bionic-sysroot.sh` (read-only `adb pull` from the attached arm64 \
     device) and/or set ACOS_BIONIC_ROOT=/path/to/bionic-arm64";

/// The exact remedy string used whenever qemu-aarch64 is missing.
pub const QEMU_REMEDY: &str =
    "extract qemu-aarch64 user-locally with `tools/fetch/fetch-qemu-aarch64.sh` \
     (or set ACOS_QEMU_AARCH64=/path/to/qemu-aarch64)";

/// The exact remedy string used whenever the seccomp guard is missing.
pub const GUARD_REMEDY: &str = "build it with `tools/build/build-arm64-guard.sh` (or set \
     ACOS_SECCOMP_GUARD=/path/to/libacos_seccomp_guard.so)";

/// The workspace root (`crates/acos-arm64/../..`).
pub fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."))
}

/// The cache root: `ACOS_CACHE_DIR`, else `<workspace>/.cache`.
pub fn cache_dir() -> PathBuf {
    std::env::var_os("ACOS_CACHE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| workspace_root().join(".cache"))
}

fn first_file(candidates: impl IntoIterator<Item = PathBuf>) -> Option<PathBuf> {
    candidates.into_iter().find(|p| p.is_file())
}

/// Locate `qemu-aarch64`: `ACOS_QEMU_AARCH64`, then the extracted cache, then
/// `PATH`.
pub fn default_qemu() -> Result<PathBuf, Arm64Error> {
    if let Some(p) = std::env::var_os("ACOS_QEMU_AARCH64").map(PathBuf::from) {
        return validate_qemu(&p);
    }
    let mut candidates = vec![cache_dir().join("qemu-user/usr/bin/qemu-aarch64")];
    if let Some(path) = std::env::var_os("PATH") {
        candidates.extend(std::env::split_paths(&path).map(|dir| dir.join("qemu-aarch64")));
    }
    first_file(candidates).ok_or_else(|| {
        Arm64Error::unavailable_at(
            "qemu-aarch64 (user-mode) was not found; the arm64 transport needs it to \
             execute aarch64 ELFs",
            QEMU_REMEDY,
            cache_dir().join("qemu-user/usr/bin/qemu-aarch64"),
        )
    })
}

/// Validate an explicit qemu path (the `ACOS_QEMU_AARCH64` form).
pub fn validate_qemu(path: &Path) -> Result<PathBuf, Arm64Error> {
    if path.is_file() {
        Ok(path.to_path_buf())
    } else {
        Err(Arm64Error::unavailable_at(
            "qemu-aarch64 path is not a regular file",
            QEMU_REMEDY,
            path,
        ))
    }
}

/// Locate a bionic sysroot: `ACOS_BIONIC_ROOT`, else `<cache>/bionic-arm64`.
///
/// A directory only qualifies when it contains `system/bin/linker64` and
/// `system/lib64/libc.so`, i.e. the layout qemu's `-L` expects after the guest
/// paths are resolved.
pub fn default_sysroot() -> Result<PathBuf, Arm64Error> {
    let candidates = match std::env::var_os("ACOS_BIONIC_ROOT") {
        Some(root) => vec![PathBuf::from(root)],
        None => vec![cache_dir().join("bionic-arm64")],
    };
    let root = &candidates[0];
    validate_sysroot(root)
}

/// Validate a candidate sysroot directory: `system/bin/linker64` and
/// `system/lib64/libc.so` must both be regular files.
pub fn validate_sysroot(root: &Path) -> Result<PathBuf, Arm64Error> {
    if root.join("system/bin/linker64").is_file() && root.join("system/lib64/libc.so").is_file() {
        Ok(root.to_path_buf())
    } else {
        Err(Arm64Error::unavailable_at(
            "bionic sysroot is missing or incomplete (need system/bin/linker64 and \
             system/lib64/libc.so)",
            SYSROOT_REMEDY,
            root,
        ))
    }
}

/// Locate the `acos-seccomp-guard` shared object.
///
/// Order: `ACOS_SECCOMP_GUARD`, `<cache>/arm64-guard/libacos_seccomp_guard.so`,
/// then next to the running executable (cargo puts the cdylib one directory
/// above the `deps/` test binaries).
pub fn default_guard() -> Result<PathBuf, Arm64Error> {
    let mut candidates = Vec::new();
    if let Some(p) = std::env::var_os("ACOS_SECCOMP_GUARD").map(PathBuf::from) {
        if p.is_file() {
            return Ok(p);
        }
        return Err(Arm64Error::unavailable_at(
            "ACOS_SECCOMP_GUARD does not point at a regular file",
            GUARD_REMEDY,
            &p,
        ));
    }
    candidates.push(cache_dir().join("arm64-guard/libacos_seccomp_guard.so"));
    if let Ok(exe) = std::env::current_exe() {
        for dir in [exe.parent(), exe.parent().and_then(Path::parent)]
            .into_iter()
            .flatten()
        {
            candidates.push(dir.join("libacos_seccomp_guard.so"));
        }
    }
    first_file(candidates.clone()).ok_or_else(|| {
        Arm64Error::unavailable_at(
            "the deferred seccomp LD_PRELOAD guard was not found",
            GUARD_REMEDY,
            candidates
                .first()
                .cloned()
                .unwrap_or_else(|| cache_dir().join("arm64-guard/libacos_seccomp_guard.so")),
        )
    })
}

/// Locate the `arm64_child` launcher next to the running executable.
pub fn default_launcher() -> Result<PathBuf, Arm64Error> {
    let exe = std::env::current_exe()?;
    for dir in [exe.parent(), exe.parent().and_then(Path::parent)]
        .into_iter()
        .flatten()
    {
        let candidate = dir.join("arm64_child");
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    Err(Arm64Error::unavailable(
        "arm64_child launcher was not found next to the current executable",
        "run through `cargo test`/`cargo run`, or set ACOS_ARM64_LAUNCHER",
    ))
}

/// One confirmed deferred-seccomp install, as reported by the guard.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct SeccompReceipt {
    /// The detail string printed by the guard after `execve`.
    pub detail: String,
}

/// A running, confined qemu-aarch64 child.
#[derive(Debug)]
pub struct ConfinedChild {
    /// The launcher/qemu process.
    pub child: Child,
    /// Proof that the post-`execve` seccomp filter was installed.
    pub seccomp: SeccompReceipt,
    /// The qemu binary that was executed.
    pub qemu: PathBuf,
    /// The bionic sysroot passed as `-L`.
    pub sysroot: PathBuf,
}

impl ConfinedChild {
    /// Wait for the guest to finish.
    pub fn wait(&mut self) -> io::Result<ExitStatus> {
        self.child.wait()
    }

    /// The guest process id.
    pub fn id(&self) -> u32 {
        self.child.id()
    }
}

/// Everything [`launch`] needs. Use [`LaunchSpec::new`] and override the
/// optional fields.
#[derive(Debug)]
pub struct LaunchSpec {
    /// The aarch64 guest program.
    pub guest_program: PathBuf,
    /// Arguments passed to the guest.
    pub guest_args: Vec<OsString>,
    /// Extra arguments placed before the guest program (e.g. `-cpu max`).
    pub qemu_args: Vec<OsString>,
    /// Extra read-only roots beyond the sysroot and `/usr`, `/lib`, `/lib64`,
    /// `/proc`, `/dev`.
    pub read_only_paths: Vec<PathBuf>,
    /// The only writable root (the app data / session root).
    pub read_write_path: PathBuf,
    /// Override the qemu binary (defaults to [`default_qemu`]).
    pub qemu: Option<PathBuf>,
    /// Override the sysroot (defaults to [`default_sysroot`]).
    pub sysroot: Option<PathBuf>,
    /// Override the seccomp guard (defaults to [`default_guard`]).
    pub guard: Option<PathBuf>,
    /// Override the launcher (defaults to [`default_launcher`]).
    pub launcher: Option<PathBuf>,
    /// How long to wait for the guard's post-exec status line.
    pub status_timeout: Duration,
    /// Where the guest's stdout goes.
    pub stdout: Stdio,
    /// Where the guest's stderr goes.
    pub stderr: Stdio,
}

impl LaunchSpec {
    /// A spec for `guest_program` with `read_write_path` as the only writable
    /// root and inherited stdio.
    pub fn new(guest_program: impl Into<PathBuf>, read_write_path: impl Into<PathBuf>) -> Self {
        LaunchSpec {
            guest_program: guest_program.into(),
            guest_args: Vec::new(),
            qemu_args: Vec::new(),
            read_only_paths: Vec::new(),
            read_write_path: read_write_path.into(),
            qemu: None,
            sysroot: None,
            guard: None,
            launcher: None,
            status_timeout: Duration::from_secs(20),
            stdout: Stdio::inherit(),
            stderr: Stdio::inherit(),
        }
    }
}

/// Spawn `guest_program` under confined `qemu-aarch64`.
///
/// Applies `PR_SET_NO_NEW_PRIVS`, fd/env/rlimit hygiene and a Landlock ruleset
/// in the `arm64_child` launcher, `execve`s `qemu-aarch64 -L <sysroot>`, and
/// waits for the preloaded guard to confirm the seccomp filter. Returns only
/// once the filter is proven installed; a failed or silent guard kills the
/// child and returns [`ARM64_UNAVAILABLE`].
pub fn launch(spec: LaunchSpec) -> Result<ConfinedChild, Arm64Error> {
    let qemu = match &spec.qemu {
        Some(p) => p.clone(),
        None => default_qemu()?,
    };
    if !qemu.is_file() {
        return Err(Arm64Error::unavailable_at(
            "qemu binary is not a regular file",
            QEMU_REMEDY,
            &qemu,
        ));
    }
    let sysroot = match &spec.sysroot {
        Some(p) => p.clone(),
        None => default_sysroot()?,
    };
    if !sysroot.join("system/bin/linker64").is_file() {
        return Err(Arm64Error::unavailable_at(
            "bionic sysroot has no system/bin/linker64",
            SYSROOT_REMEDY,
            &sysroot,
        ));
    }
    let guard = match &spec.guard {
        Some(p) => p.clone(),
        None => default_guard()?,
    };
    if !guard.is_file() {
        return Err(Arm64Error::unavailable_at(
            "seccomp guard is not a regular file",
            GUARD_REMEDY,
            &guard,
        ));
    }
    let launcher = match &spec.launcher {
        Some(p) => p.clone(),
        None => default_launcher()?,
    };
    if !launcher.is_file() {
        return Err(Arm64Error::unavailable(
            "arm64_child launcher is not a regular file",
            "rebuild the workspace, or set ACOS_ARM64_LAUNCHER",
        ));
    }
    if !spec.guest_program.is_file() {
        return Err(Arm64Error::unavailable_at(
            "guest program is not a regular file",
            "extract or build the aarch64 guest first",
            &spec.guest_program,
        ));
    }

    // qemu argument vector: unset the host-only preload/status variables from
    // the *guest* environment, then `-L sysroot`, transport extras, guest and
    // guest args. Without `-U LD_PRELOAD` bionic's linker would try to load
    // the x86_64 guard `.so` as an aarch64 dependency.
    let mut qemu_argv: Vec<OsString> = vec![
        OsString::from("-U"),
        OsString::from("LD_PRELOAD"),
        OsString::from("-U"),
        OsString::from("ACOS_GUARD_STATUS_FD"),
        OsString::from("-L"),
        sysroot.as_os_str().to_os_string(),
    ];
    qemu_argv.extend(spec.qemu_args.iter().cloned());
    qemu_argv.push(spec.guest_program.as_os_str().to_os_string());
    qemu_argv.extend(spec.guest_args.iter().cloned());

    // Read-only roots the qemu *host* process needs after `execve`, per
    // docs/ARM64.md §4.2. Missing paths are skipped by Landlock.
    let mut read_only: Vec<PathBuf> = vec![
        sysroot.clone(),
        PathBuf::from("/usr"),
        PathBuf::from("/lib"),
        PathBuf::from("/lib64"),
        PathBuf::from("/proc"),
        PathBuf::from("/dev"),
    ];
    if let Some(parent) = guard.parent() {
        read_only.push(parent.to_path_buf());
    }
    // The qemu-aarch64 binary itself must be searchable/executable after the
    // Landlock ruleset is applied (it is `execve`d by the launcher).
    if let Some(parent) = qemu.parent() {
        read_only.push(parent.to_path_buf());
    }
    if let Some(parent) = spec.guest_program.parent() {
        read_only.push(parent.to_path_buf());
    }
    read_only.extend(spec.read_only_paths.iter().cloned());
    std::fs::create_dir_all(&spec.read_write_path)?;

    // Status pipe: the guard writes `OK ...`/`ERR ...` after `execve`. The
    // write end is dup2'd to fd 3 in the child so the launcher can keep it
    // across its own exec (CLOEXEC would otherwise drop it).
    let (read_fd, write_fd) = pipe_cloexec()?;

    let mut command = Command::new(&launcher);
    command
        .arg("--guard")
        .arg(&guard)
        .arg("--sysroot")
        .arg(&sysroot)
        .arg("--rw")
        .arg(&spec.read_write_path);
    for root in &read_only {
        command.arg("--ro").arg(root);
    }
    command.arg("--status-fd").arg("3");
    command.arg("--").arg(&qemu).args(&qemu_argv);
    command.stdout(spec.stdout);
    command.stderr(spec.stderr);
    // SAFETY: the closure runs in the forked child before exec and only uses
    // async-signal-safe `dup2`/`close` on `write_fd`, which the parent keeps
    // owned until after `spawn` returns.
    unsafe {
        command.pre_exec(move || {
            if libc::dup2(write_fd, 3) < 0 {
                return Err(io::Error::last_os_error());
            }
            if write_fd != 3 {
                libc::close(write_fd);
            }
            Ok(())
        });
    }

    let mut child = command.spawn()?;
    // Close the parent's copy of the write end so the pipe reports EOF when the
    // guard finishes.
    // SAFETY: `write_fd` is a live descriptor owned by this function and is
    // not used again.
    unsafe {
        libc::close(write_fd);
    }

    let status = match read_status(read_fd, spec.status_timeout) {
        Ok(status) => status,
        Err(e) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(Arm64Error::unavailable(
                format!("arm64 child failed before seccomp was confirmed: {e}"),
                "check `qemu-aarch64 --version` and the sysroot with `tools/fetch/fetch-bionic-sysroot.sh`",
            ));
        }
    };
    // SAFETY: `read_fd` is a live descriptor owned by this function and is no
    // longer needed.
    unsafe {
        libc::close(read_fd);
    }

    let receipt = match status {
        GuardStatus::Ok(detail) => SeccompReceipt { detail },
        GuardStatus::Err(message) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(Arm64Error::unavailable(
                format!("deferred seccomp guard failed: {message}"),
                GUARD_REMEDY,
            ));
        }
        GuardStatus::Silent => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(Arm64Error::unavailable(
                "arm64 child never reported the seccomp filter (LD_PRELOAD guard did not run)",
                format!("{GUARD_REMEDY}; also confirm the guest can execute"),
            ));
        }
    };

    Ok(ConfinedChild {
        child,
        seccomp: receipt,
        qemu,
        sysroot,
    })
}

enum GuardStatus {
    Ok(String),
    Err(String),
    Silent,
}

fn read_status(fd: libc::c_int, timeout: Duration) -> io::Result<GuardStatus> {
    let deadline = Instant::now() + timeout;
    let mut out: Vec<u8> = Vec::new();
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Ok(GuardStatus::Silent);
        }
        let mut pfd = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        let ms = remaining.as_millis().min(i32::MAX as u128) as i32;
        // SAFETY: `pfd` is a valid single pollfd for the duration of the call.
        let rc = unsafe { libc::poll(&mut pfd, 1, ms) };
        if rc < 0 {
            let err = io::Error::last_os_error();
            if err.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(err);
        }
        if rc == 0 {
            return Ok(GuardStatus::Silent);
        }
        let mut buf = [0u8; 512];
        // SAFETY: `buf` is a live writable buffer and `fd` is a valid
        // descriptor.
        let n = unsafe { libc::read(fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
        if n < 0 {
            let err = io::Error::last_os_error();
            if err.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(err);
        }
        if n == 0 {
            // EOF without a newline: the guard closed without reporting.
            break;
        }
        out.extend_from_slice(&buf[..n as usize]);
        if out.contains(&b'\n') {
            break;
        }
    }
    if out.is_empty() {
        return Ok(GuardStatus::Silent);
    }
    let line = String::from_utf8_lossy(&out);
    let line = line.trim_end();
    if let Some(detail) = line.strip_prefix("OK ") {
        Ok(GuardStatus::Ok(detail.to_string()))
    } else if let Some(message) = line.strip_prefix("ERR ") {
        Ok(GuardStatus::Err(message.to_string()))
    } else {
        Ok(GuardStatus::Err(line.to_string()))
    }
}

/// Create a close-on-exec pipe, returning `(read, write)`.
fn pipe_cloexec() -> io::Result<(libc::c_int, libc::c_int)> {
    let mut fds = [0 as libc::c_int; 2];
    // SAFETY: `fds` is a live two-element buffer.
    let rc = unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok((fds[0], fds[1]))
}

/// A path that is a valid Landlock root: it exists and has no interior NUL.
pub fn validate_root(path: &Path) -> Result<(), Arm64Error> {
    if path.as_os_str().as_bytes().contains(&0) {
        return Err(Arm64Error::unavailable(
            "path contains a NUL byte",
            "pass a normal filesystem path",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selects_x86_64_when_present() {
        let abis = vec!["arm64-v8a".to_string(), "x86_64".to_string()];
        assert_eq!(select_guest_abi(&abis).unwrap(), GuestAbi::X86_64);
    }

    #[test]
    fn selects_arm64_when_x86_64_absent() {
        let abis = vec!["arm64-v8a".to_string()];
        assert_eq!(select_guest_abi(&abis).unwrap(), GuestAbi::Arm64V8a);
    }

    #[test]
    fn no_native_dir_is_not_an_abi_error() {
        assert_eq!(select_guest_abi(&[]).unwrap(), GuestAbi::X86_64);
    }

    #[test]
    fn unsupported_abi_is_structured() {
        let abis = vec!["armeabi-v7a".to_string(), "x86".to_string()];
        let err = select_guest_abi(&abis).unwrap_err();
        assert_eq!(err.code, UNSUPPORTED_ABI);
        assert!(err.message.contains("armeabi-v7a"));
        assert!(!err.remedy.is_empty());
    }

    #[test]
    fn guest_abi_dir_round_trips() {
        for abi in [GuestAbi::X86_64, GuestAbi::Arm64V8a] {
            assert_eq!(GuestAbi::from_dir(abi.dir()), Some(abi));
        }
        assert_eq!(GuestAbi::from_dir("mips"), None);
    }

    #[test]
    fn missing_qemu_is_arm64_unavailable_with_remedy() {
        let err = validate_qemu(Path::new("/nonexistent-qemu-for-test/qemu-aarch64"))
            .expect_err("no qemu should be found");
        assert_eq!(err.code, ARM64_UNAVAILABLE);
        assert!(
            err.remedy.contains("fetch-qemu-aarch64.sh"),
            "{}",
            err.remedy
        );
        assert_eq!(
            err.path.as_deref(),
            Some("/nonexistent-qemu-for-test/qemu-aarch64")
        );
    }

    #[test]
    fn missing_sysroot_is_arm64_unavailable_with_remedy() {
        let err =
            validate_sysroot(Path::new("/nonexistent-bionic-for-test")).expect_err("no sysroot");
        assert_eq!(err.code, ARM64_UNAVAILABLE);
        assert!(
            err.remedy.contains("fetch-bionic-sysroot.sh"),
            "{}",
            err.remedy
        );
        assert_eq!(err.path.as_deref(), Some("/nonexistent-bionic-for-test"));
    }

    #[test]
    fn error_display_carries_code_and_remedy() {
        let err = Arm64Error::unavailable("nope", "do the thing");
        let text = err.to_string();
        assert!(text.contains(ARM64_UNAVAILABLE));
        assert!(text.contains("do the thing"));
    }

    #[test]
    fn error_serialises_as_json() {
        let err = Arm64Error::unavailable_at("gone", "fix it", "/tmp/gone");
        let json = serde_json::to_value(&err).unwrap();
        assert_eq!(json["code"], ARM64_UNAVAILABLE);
        assert_eq!(json["path"], "/tmp/gone");
        assert_eq!(json["remedy"], "fix it");
    }
}
