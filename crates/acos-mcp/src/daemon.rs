//! Local session daemon wire protocol (F-08).
//!
//! `acos serve` exposes the [`crate::session_backend::SessionBackend`] session
//! tool surface on a Unix domain socket under a private runtime directory. The
//! wire format is deliberately the MCP session tool surface, not a new protocol:
//!
//! * request: newline-delimited JSON
//!   `{"id": <u64>, "tool": "acos_launch", "args": { ... }}
//!   `args` is the ACOS request object (`base.request_id`, `base.session_id`);
//! * response: `{"id": <u64>, "envelope": <ACOS envelope>}`, or
//!   `{"id": <u64>, "error": {"code": "...", "message": "..."}}` for a transport
//!   level failure.
//!
//! [`DaemonClient`] is shared by the CLI (`acos session …`) and by the MCP
//! server when it is pointed at a running daemon. The socket path is
//! [`socket_path`]; the runtime directory is created 0700 by the server.

use std::io::{BufRead, BufReader, ErrorKind, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

use acos_protocol::{
    Backend as ProtocolBackend, Envelope, ErrorCode, Fidelity, Metrics, ProtocolError,
};
use serde_json::{json, Value};

/// Environment variable pinning the socket path (both server and client).
pub const SOCKET_ENV: &str = "ACOS_SESSION_SOCKET";
/// Environment variable pinning the daemon runtime directory.
pub const RUNTIME_DIR_ENV: &str = "ACOS_RUNTIME_DIR";
/// When set to a truthy value, the MCP server forwards session tools to a
/// running daemon instead of holding sessions in-process.
pub const MCP_DAEMON_ENV: &str = "ACOS_SESSION_DAEMON";

/// The default socket file name inside the runtime directory.
pub const SOCKET_FILE: &str = "session.sock";

/// The control tool that reports the daemon is alive.
pub const TOOL_PING: &str = "ping";
/// The control tool that lists held sessions.
pub const TOOL_LIST: &str = "session_list";

/// Resolve the daemon runtime directory (not necessarily existing).
///
/// `ACOS_RUNTIME_DIR` wins; otherwise `$XDG_RUNTIME_DIR/acos`, then
/// `$TMPDIR/acos`, then the **per-uid** fallback [`fallback_runtime_dir`].
pub fn runtime_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os(RUNTIME_DIR_ENV) {
        return PathBuf::from(dir);
    }
    match std::env::var_os("XDG_RUNTIME_DIR").or_else(|| std::env::var_os("TMPDIR")) {
        Some(base) => PathBuf::from(base).join("acos"),
        // FB5 (SERVELEAK3 review finding 6): never a shared `/tmp/acos`. Two
        // users on one host would share sockets and session directories, and a
        // hostile user could pre-create the path. The per-uid name plus
        // [`ensure_private_dir`] makes that impossible.
        None => fallback_runtime_dir(),
    }
}

/// The no-`XDG_RUNTIME_DIR`/no-`TMPDIR` fallback: `/tmp/acos-<uid>`.
#[must_use]
pub fn fallback_runtime_dir() -> PathBuf {
    // SAFETY: `geteuid` has no preconditions and cannot fail.
    let uid = unsafe { libc::geteuid() };
    PathBuf::from(format!("/tmp/acos-{uid}"))
}

/// Create/chmod a runtime **base** the caller chose (an explicit
/// `ACOS_RUNTIME_DIR`, an `XDG_RUNTIME_DIR`): it must be ours, and it is repaired
/// to 0700 rather than refused, because the caller owns the choice.
///
/// The `/tmp` fallback is different: see [`ensure_private_dir`], which refuses a
/// mode it did not create.
pub fn ensure_runtime_base(dir: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    std::fs::create_dir_all(dir)?;
    // FB5b/M3: never chmod *through* a symlink (the chmod would land on whatever
    // the link names). A link is accepted only when its target is ours and
    // already private; it is verified, not repaired.
    let link = std::fs::symlink_metadata(dir)?;
    let meta = std::fs::metadata(dir)?;
    // SAFETY: `geteuid` has no preconditions and cannot fail.
    let uid = unsafe { libc::geteuid() };
    if meta.uid() != uid || !meta.is_dir() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            format!(
                "{} is not a directory owned by uid {uid} (owner {}); refusing to use it as a runtime directory",
                dir.display(),
                meta.uid()
            ),
        ));
    }
    if link.file_type().is_symlink() {
        let mode = meta.permissions().mode() & 0o777;
        if mode & 0o077 != 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                format!(
                    "{} is a symlink to a directory with mode 0{mode:o}; refusing to chmod through a link",
                    dir.display()
                ),
            ));
        }
        return Ok(());
    }
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
}

/// Create (or verify) a runtime directory that only this user can use.
///
/// Existing directories are **not** repaired blindly: one owned by another uid,
/// or one that is group/world accessible, is refused with an error naming both
/// facts. That is the FB5 rule for the `/tmp` fallback (SERVELEAK3 review
/// finding 6) and the same check the private daemon's own directory gets.
///
/// FB5b/M3: the final component is inspected with `symlink_metadata` and a
/// symlink (or a non-directory) is **refused** — a hostile `/tmp/acos-<uid>`
/// link to a directory the attacker controls must never become our runtime dir,
/// and no `chmod` ever follows a link.
pub fn ensure_private_dir(dir: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
    match std::fs::symlink_metadata(dir) {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            if let Some(parent) = dir.parent() {
                std::fs::create_dir_all(parent)?;
            }
            // Created 0700 atomically; an existing entry (a racing symlink) makes
            // this fail with AlreadyExists and falls through to the check below.
            match std::fs::DirBuilder::new().mode(0o700).create(dir) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(e) => return Err(e),
            }
        }
        Err(e) => return Err(e),
    }
    let meta = std::fs::symlink_metadata(dir)?;
    if meta.file_type().is_symlink() || !meta.is_dir() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            format!(
                "{} is a symlink or not a directory; refusing to use it as a private runtime directory",
                dir.display()
            ),
        ));
    }
    // SAFETY: `geteuid` has no preconditions and cannot fail.
    let uid = unsafe { libc::geteuid() };
    if meta.uid() != uid {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            format!(
                "{} is owned by uid {}, not {uid}; refusing to use another user's runtime directory",
                dir.display(),
                meta.uid()
            ),
        ));
    }
    let mode = meta.permissions().mode() & 0o777;
    if mode & 0o077 != 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            format!(
                "{} has mode 0{mode:o}, expected 0700 (group/other access refused)",
                dir.display()
            ),
        ));
    }
    Ok(())
}

/// The daemon socket path (`ACOS_SESSION_SOCKET` wins).
pub fn socket_path() -> PathBuf {
    if let Some(path) = std::env::var_os(SOCKET_ENV) {
        return PathBuf::from(path);
    }
    runtime_dir().join(SOCKET_FILE)
}

/// Whether the MCP server should use a running daemon.
pub fn mcp_daemon_requested() -> bool {
    matches!(
        std::env::var(MCP_DAEMON_ENV)
            .unwrap_or_default()
            .to_ascii_lowercase()
            .as_str(),
        "1" | "true" | "yes" | "on"
    )
}

/// A blocking client for one daemon connection.
pub struct DaemonClient {
    reader: BufReader<UnixStream>,
    next_id: u64,
}

impl std::fmt::Debug for DaemonClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DaemonClient").finish_non_exhaustive()
    }
}

impl DaemonClient {
    /// Connect to the daemon at `path`.
    pub fn connect(path: &Path) -> std::io::Result<Self> {
        let stream = UnixStream::connect(path)?;
        stream.set_read_timeout(Some(Duration::from_secs(600)))?;
        stream.set_write_timeout(Some(Duration::from_secs(600)))?;
        Ok(DaemonClient {
            reader: BufReader::new(stream),
            next_id: 0,
        })
    }

    /// Connect to the default socket.
    pub fn connect_default() -> std::io::Result<Self> {
        Self::connect(&socket_path())
    }

    /// The next request id this client will send (1-based).
    #[must_use]
    pub fn next_id(&self) -> u64 {
        self.next_id + 1
    }

    /// Send one session tool call and read its envelope.
    pub fn call(&mut self, tool: &str, args: &Value) -> std::io::Result<Envelope> {
        self.next_id += 1;
        let id = self.next_id;
        let request = json!({"id": id, "tool": tool, "args": args});
        let mut line = serde_json::to_string(&request)
            .map_err(|e| std::io::Error::new(ErrorKind::InvalidData, e))?;
        line.push('\n');
        {
            let stream = self.reader.get_mut();
            stream.write_all(line.as_bytes())?;
            stream.flush()?;
        }
        loop {
            let mut response = String::new();
            let n = self.reader.read_line(&mut response)?;
            if n == 0 {
                return Err(std::io::Error::new(
                    ErrorKind::UnexpectedEof,
                    "session daemon closed the connection",
                ));
            }
            let value: Value = match serde_json::from_str(&response) {
                Ok(value) => value,
                Err(_) => continue,
            };
            if value.get("id").and_then(Value::as_u64) != Some(id) {
                continue;
            }
            if let Some(envelope) = value.get("envelope") {
                return serde_json::from_value(envelope.clone())
                    .map_err(|e| std::io::Error::new(ErrorKind::InvalidData, e));
            }
            let code = value
                .pointer("/error/code")
                .and_then(Value::as_str)
                .unwrap_or("INTERNAL");
            let message = value
                .pointer("/error/message")
                .and_then(Value::as_str)
                .unwrap_or("session daemon error");
            return Ok(Envelope::failure(
                "req_daemon",
                ProtocolBackend::AcosNative,
                Fidelity::NativeRuntime,
                ProtocolError::new(
                    ErrorCode::from_str_opt(code).unwrap_or(ErrorCode::Internal),
                    message.to_string(),
                ),
                Metrics::wall_ms(0),
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "acos-daemon-{}-{}-{name}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).expect("scratch");
        dir
    }

    #[test]
    fn the_fallback_base_is_per_uid_never_a_shared_tmp_acos() {
        // SAFETY: `geteuid` has no preconditions.
        let uid = unsafe { libc::geteuid() };
        let fallback = fallback_runtime_dir();
        assert_eq!(fallback, PathBuf::from(format!("/tmp/acos-{uid}")));
        assert_ne!(fallback, PathBuf::from("/tmp/acos"));
        // The effective runtime dir is the fallback only without the env vars.
        if std::env::var_os(RUNTIME_DIR_ENV).is_none()
            && std::env::var_os("XDG_RUNTIME_DIR").is_none()
            && std::env::var_os("TMPDIR").is_none()
        {
            assert_eq!(runtime_dir(), fallback);
        }
    }

    #[test]
    fn ensure_private_dir_creates_0700_and_refuses_foreign_modes() {
        let base = scratch("private-dir");
        let dir = base.join("runtime");
        ensure_private_dir(&dir).expect("create 0700");
        let mode = std::fs::metadata(&dir).expect("stat").permissions().mode() & 0o777;
        assert_eq!(mode, 0o700, "created 0700");

        // A directory someone made group/world accessible is refused, not
        // silently chmod-ed: the fallback may be a hostile path.
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        let err = ensure_private_dir(&dir).expect_err("0755 must be refused");
        assert!(
            err.to_string().contains("expected 0700"),
            "the error names the mode rule: {err}"
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn symlinked_private_dirs_are_refused_and_never_chmodded() {
        let base = scratch("symlink-dir");
        let target = base.join("target");
        std::fs::create_dir_all(&target).expect("target");
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        let link = base.join("acos-link");
        std::os::unix::fs::symlink(&target, &link).expect("symlink");

        let err = ensure_private_dir(&link).expect_err("a symlink must be refused");
        assert!(err.to_string().contains("symlink"), "{err}");
        let mode = std::fs::metadata(&target).expect("stat").permissions().mode() & 0o777;
        assert_eq!(mode, 0o755, "the link target was not chmod-ed");

        // The runtime *base* helper must not chmod through the link either.
        let err = ensure_runtime_base(&link).expect_err("0755 link target refused");
        assert!(err.to_string().contains("chmod through a link"), "{err}");
        let mode = std::fs::metadata(&target).expect("stat").permissions().mode() & 0o777;
        assert_eq!(mode, 0o755, "still untouched");
        let _ = std::fs::remove_dir_all(&base);
    }
}
