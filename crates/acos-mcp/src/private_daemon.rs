//! The MCP server's own private ART session daemon.
//!
//! **The product engine is real ART.** The MCP session tools
//! (`acos_launch`, `acos_observe*`, `acos_tap`, `acos_text`, `acos_key`,
//! `acos_logs`, `acos_status`, `acos_reset`, `acos_destroy`) used to run on the
//! frozen in-process Rust VM / RN-bridge backend, which could not run a real
//! APK: an installed server answered `UNSUPPORTED_RN_SETUP` for the RN corpus
//! and `acos_reset` reported `data_cleared: false` because nothing was real.
//! The default is now the same engine the CLI verifies with:
//!
//! * on the first session tool call the server lazily spawns its **own**
//!   `acos serve --engine art` (`acos-cli`'s daemon, one real ART guest per
//!   session) on a private socket under
//!   `$XDG_RUNTIME_DIR/acos/mcp-<pid>-<rand>/session.sock` (short enough for
//!   `sun_path`) and forwards every session tool to it;
//! * the daemon is a child of the server: `PR_SET_PDEATHSIG(SIGTERM)` makes the
//!   kernel signal it if the server dies, and on stdin EOF/exit the server
//!   destroys its sessions, sends `SIGTERM` (whose handler in `acos serve`
//!   closes every ART guest), and reaps the child;
//! * the in-process legacy backend is reachable only when explicitly asked for
//!   with `ACOS_MCP_ENGINE=legacy` (frozen; it is not the product engine);
//! * `ACOS_SESSION_DAEMON=1` still connects to an *external* daemon when one is
//!   listening; that is a separate, unchanged opt-in.

use std::io;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::daemon::{self, DaemonClient};

/// The engine label every ART session reply carries.
pub const ENGINE_ART: &str = "art";

/// Environment variable that overrides the MCP session engine.
///
/// Unset (or `art`) means the private ART daemon. `legacy` (also accepted:
/// `rust-vm`) keeps the frozen in-process backend and is never chosen
/// implicitly.
pub const ENGINE_ENV: &str = "ACOS_MCP_ENGINE";

/// Whether the frozen in-process backend was requested explicitly.
#[must_use]
pub fn legacy_requested() -> bool {
    matches!(
        std::env::var(ENGINE_ENV)
            .unwrap_or_default()
            .to_ascii_lowercase()
            .as_str(),
        "legacy" | "rust-vm" | "rust_vm"
    )
}

/// Environment variable the daemon reads for the live-view registry's
/// `client` field (VIEW1a, `docs/LIVE_VIEW.md` §5).
pub const VIEW_CLIENT_ENV: &str = "ACOS_VIEW_CLIENT";
/// Environment variable the daemon reads for the registry's `project_dir`.
pub const VIEW_PROJECT_ENV: &str = "ACOS_VIEW_PROJECT";

/// The MCP client identity from `initialize` (`clientInfo`), process-wide: one
/// server process serves one client, and the live-view registry names it.
static VIEW_CLIENT: std::sync::OnceLock<Mutex<Option<(String, String)>>> =
    std::sync::OnceLock::new();

/// Record the client identity `initialize` reported.
pub fn set_view_client(name: &str, version: &str) {
    let slot = VIEW_CLIENT.get_or_init(|| Mutex::new(None));
    let mut slot = slot.lock().unwrap_or_else(|e| e.into_inner());
    *slot = Some((name.to_string(), version.to_string()));
}

/// The `ACOS_VIEW_CLIENT` value for the private daemon: the `clientInfo` the
/// client sent, as `{"name":…,"version":…}`, or `None` when it sent none (the
/// daemon then publishes its own `acos-cli` default).
#[must_use]
pub fn view_client_env() -> Option<String> {
    let slot = VIEW_CLIENT.get_or_init(|| Mutex::new(None));
    let slot = slot.lock().unwrap_or_else(|e| e.into_inner());
    slot.as_ref().map(|(name, version)| {
        json!({"name": name, "version": version}).to_string()
    })
}

/// The engine name to pass to `acos serve --engine`.
#[must_use]
pub fn requested_engine() -> String {
    ENGINE_ART.to_string()
}

/// One private `acos serve` child and the client connected to it.
pub struct PrivateDaemon {
    dir: PathBuf,
    socket: PathBuf,
    child: Child,
    client: Option<DaemonClient>,
    engine: String,
}

impl std::fmt::Debug for PrivateDaemon {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PrivateDaemon")
            .field("socket", &self.socket)
            .field("pid", &self.child.id())
            .field("engine", &self.engine)
            .finish_non_exhaustive()
    }
}

impl PrivateDaemon {
    /// Spawn `acos serve --engine <engine>` on a fresh private socket and wait
    /// until it accepts a connection.
    pub fn spawn(acos: &Path, engine: &str) -> io::Result<Self> {
        let dir = private_runtime_dir()?;
        prepare_dir(&dir)?;
        let socket = dir.join(daemon::SOCKET_FILE);
        if socket.as_os_str().len() > 100 {
            let _ = std::fs::remove_dir_all(&dir);
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "private session socket path {} is too long for a unix socket",
                    socket.display()
                ),
            ));
        }
        let mut command = Command::new(acos);
        command
            .arg("serve")
            .arg("--socket")
            .arg(&socket)
            .arg("--engine")
            .arg(engine)
            // VIEW1a: the live-view registry names this agent and its project.
            // The daemon publishes one entry per ready session
            // (`$XDG_RUNTIME_DIR/acos/live`), and a viewer hub shows them.
            .env(
                VIEW_PROJECT_ENV,
                std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from(".")),
            )
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        // The daemon must never outlive this server. `PR_SET_PDEATHSIG` is
        // inherited across exec and fires when the **thread** that forked the
        // child exits, so the fork happens on a dedicated thread that parks for
        // the life of the process: a tool call's worker thread would otherwise
        // take the daemon down the moment the call returned. The explicit
        // shutdown in [`PrivateDaemon::shutdown`] is the normal path; this is
        // the backstop for a killed server.
        //
        // SAFETY: `pre_exec` runs between fork and exec; the only call is
        // `prctl`, which is async-signal-safe.
        unsafe {
            command.pre_exec(|| {
                if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM as libc::c_ulong) != 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        if let Some(client) = view_client_env() {
            command.env(VIEW_CLIENT_ENV, client);
        }
        let child = spawn_on_lifetime_thread(command)?;
        let mut daemon = PrivateDaemon {
            dir,
            socket,
            child,
            client: None,
            engine: engine.to_string(),
        };
        daemon.wait_ready(Duration::from_secs(30))?;
        Ok(daemon)
    }

    /// The private socket path (also the daemon's runtime directory).
    #[must_use]
    pub fn socket(&self) -> &Path {
        &self.socket
    }

    /// The engine the daemon was started with.
    #[must_use]
    pub fn engine(&self) -> &str {
        &self.engine
    }

    fn wait_ready(&mut self, timeout: Duration) -> io::Result<()> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Ok(client) = DaemonClient::connect(&self.socket) {
                self.client = Some(client);
                return Ok(());
            }
            if let Some(status) = self.child.try_wait()? {
                return Err(io::Error::other(format!(
                    "`acos serve --engine {}` exited before its socket was ready ({status})",
                    self.engine
                )));
            }
            if Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!(
                        "`acos serve --engine {}` did not listen on {} within {:?}",
                        self.engine,
                        self.socket.display(),
                        timeout
                    ),
                ));
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Forward one session tool call to the daemon.
    pub fn call(&mut self, tool: &str, args: &Value) -> io::Result<acos_protocol::Envelope> {
        if self.client.is_none() {
            self.client = Some(DaemonClient::connect(&self.socket)?);
        }
        let client = self.client.as_mut().expect("connected above");
        client.call(tool, args)
    }

    /// Whether the child process is still running.
    #[must_use]
    pub fn is_running(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    /// The child's pid.
    #[must_use]
    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    /// Destroy held sessions, terminate the daemon and reap it.
    ///
    /// The daemon's own `SIGTERM` handler closes every ART guest, so the
    /// explicit destroy calls are best effort (and make the teardown visible in
    /// the daemon log) before the signal.
    pub fn shutdown(&mut self) {
        if let Some(mut client) = self.client.take() {
            let _ = destroy_all(&mut client);
        }
        // SAFETY: `kill` on a pid we own; a failure (already exited) is ignored.
        unsafe {
            libc::kill(self.child.id() as libc::pid_t, libc::SIGTERM);
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match self.child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) => {
                    if Instant::now() >= deadline {
                        let _ = self.child.kill();
                        let _ = self.child.wait();
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(_) => break,
            }
        }
        let _ = std::fs::remove_file(&self.socket);
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

impl Drop for PrivateDaemon {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Spawn the child from a thread that lives as long as the process.
///
/// `PR_SET_PDEATHSIG` is delivered when the forking *thread* exits, not the
/// whole process. A session tool call runs on a short-lived worker thread, so
/// forking there would kill the daemon as soon as the reply was written. The
/// helper thread parks forever; when the process exits, the thread goes with it
/// and the kernel signals the daemon.
fn spawn_on_lifetime_thread(mut command: Command) -> io::Result<Child> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("acos-art-daemon-spawn".to_string())
        .spawn(move || {
            let spawned = command.spawn();
            let _ = tx.send(spawned);
            loop {
                std::thread::park();
            }
        })?;
    match rx.recv() {
        Ok(Ok(child)) => Ok(child),
        Ok(Err(e)) => Err(e),
        Err(_) => Err(io::Error::other("the daemon spawn thread died")),
    }
}

/// Ask the daemon for its sessions, then destroy each one. Best effort.
fn destroy_all(client: &mut DaemonClient) -> io::Result<()> {
    let envelope = client.call(daemon::TOOL_LIST, &json!({}))?;
    let ids: Vec<String> = envelope
        .result()
        .and_then(|result| result.get("sessions"))
        .and_then(Value::as_array)
        .map(|sessions| {
            sessions
                .iter()
                .filter_map(|session| session.get("session_id").and_then(Value::as_str))
                .map(ToString::to_string)
                .collect()
        })
        .unwrap_or_default();
    for id in ids {
        let _ = client.call(
            "acos_destroy",
            &json!({"base": {"request_id": "req_mcp_shutdown", "session_id": id}}),
        );
    }
    Ok(())
}

/// `$XDG_RUNTIME_DIR/acos/mcp-<pid>-<rand>` (falling back like the daemon does).
fn private_runtime_dir() -> io::Result<PathBuf> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let base = daemon::runtime_dir();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let n = NEXT.fetch_add(1, Ordering::SeqCst);
    Ok(base.join(format!(
        "mcp-{}-{:08x}{n}",
        std::process::id(),
        nanos
    )))
}

fn prepare_dir(dir: &Path) -> io::Result<()> {
    std::fs::create_dir_all(dir)?;
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn private_dir_is_per_process_and_under_the_runtime_dir() {
        let dir = private_runtime_dir().expect("dir");
        assert!(dir.starts_with(daemon::runtime_dir()), "{}", dir.display());
        let name = dir.file_name().unwrap().to_string_lossy().into_owned();
        assert!(
            name.starts_with(&format!("mcp-{}-", std::process::id())),
            "{name}"
        );
        let socket = dir.join(daemon::SOCKET_FILE);
        assert!(socket.as_os_str().len() <= 100, "{}", socket.display());
    }

    #[test]
    fn legacy_is_opt_in_only() {
        // The environment is process-wide; do not mutate it in a test that can
        // run beside others. An unset variable is the documented default.
        if std::env::var_os(ENGINE_ENV).is_none() {
            assert!(!legacy_requested());
            assert_eq!(requested_engine(), "art");
        }
    }
}
