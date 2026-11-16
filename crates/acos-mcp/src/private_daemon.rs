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
    /// Set once the child has been reaped: a second `shutdown` must not signal
    /// the pid again (SERVELEAK2/F4: a reaped pid can be recycled).
    stopped: bool,
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
            // REL3D: this daemon was started by a client, so a matching client
            // may replace it when idle; an operator's `acos serve` never is.
            .arg("--auto-started")
            // SERVELEAK's own-owner binding: a daemon spawned for one process
            // must exit when that process is gone, even after `kill -9` (the
            // accept loop checks the owner once a second). `PR_SET_PDEATHSIG`
            // below is the kernel-level backstop; this one also covers a parent
            // whose *thread* outlived it or a `PDEATHSIG` the kernel dropped.
            .arg("--owner-pid")
            .arg(std::process::id().to_string())
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
                // FB5: the server blocks SIGTERM/SIGINT so its `sigwait` thread
                // can run the same teardown as stdin EOF. A child inherits that
                // mask across exec, and the daemon's own SIGTERM handler would
                // never fire — so the daemon gets an unblocked mask back.
                let mut set: libc::sigset_t = std::mem::zeroed();
                libc::sigemptyset(&mut set);
                libc::sigaddset(&mut set, libc::SIGTERM);
                libc::sigaddset(&mut set, libc::SIGINT);
                libc::pthread_sigmask(libc::SIG_UNBLOCK, &set, std::ptr::null_mut());
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
            stopped: false,
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
                // REL3D: the private daemon is still identity-checked. It is
                // spawned from the `acos` binary, which can be a **different
                // build** than this `acos-mcp` server (an upgrade replaced one
                // of them, or `ACOS_MCP_BIN`/`PATH` points elsewhere). Forwarding
                // to it would answer with another build's behaviour, so it is
                // refused with both identities named instead.
                if let Some(mismatch) = client.mismatch() {
                    let warning = mismatch.to_json();
                    return Err(io::Error::other(format!(
                        "{}: {}",
                        mismatch.code(),
                        serde_json::to_string(&warning)
                            .unwrap_or_else(|_| mismatch.message())
                    )));
                }
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

    /// Terminate the daemon and reap it (idempotent).
    ///
    /// The sessions it still holds are destroyed by the daemon itself: `acos
    /// serve` handles `SIGTERM` by running `engine.shutdown()`, which closes
    /// every ART guest and proves its process group gone before the socket
    /// disappears (SERVELEAK). The explicit per-session destroys happen *before*
    /// this in [`crate::SessionBackend`]'s teardown, through the owned-session
    /// set and the launch token, so nothing here has to guess at ownership —
    /// and this function deliberately never lists-and-destroys "everything the
    /// daemon knows about", which is not a proof of ownership (SERVELEAK2).
    pub fn shutdown(&mut self) {
        if self.stopped {
            return;
        }
        self.stopped = true;
        self.client.take();
        // Signal only a child that is provably still unreaped: `try_wait`
        // returns the remembered status once the child was reaped (for example
        // by `wait_ready` when the daemon died before its socket was ready), and
        // a reaped pid may already belong to another process. While the child is
        // unreaped (`Ok(None)`) its pid cannot have been recycled.
        if matches!(self.child.try_wait(), Ok(None)) {
            // SAFETY: `kill` on our own child, which is not yet reaped.
            unsafe {
                libc::kill(self.child.id() as libc::pid_t, libc::SIGTERM);
            }
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

/// Create the per-process directory 0700 and make sure the **base** it sits in
/// is ours (FB5: the `/tmp/acos-<uid>` fallback is verified, never assumed).
fn prepare_dir(dir: &Path) -> io::Result<()> {
    if let Some(base) = dir.parent() {
        // The base is either the user's runtime dir (repaired to 0700) or the
        // `/tmp/acos-<uid>` fallback (verified, never repaired by us).
        if base == daemon::fallback_runtime_dir() {
            daemon::ensure_private_dir(base)?;
        } else {
            daemon::ensure_runtime_base(base)?;
        }
    }
    daemon::ensure_private_dir(dir)
}

/// A private daemon whose child is already reaped and whose socket is gone.
///
/// A test seam for the FB5 no-restart rule: teardown must not respawn a dead
/// private daemon, and this is the state "the daemon died before shutdown".
#[cfg(test)]
pub(crate) fn dead_for_test(dir: PathBuf) -> PrivateDaemon {
    let child = Command::new("/bin/true")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn /bin/true");
    PrivateDaemon {
        socket: dir.join(daemon::SOCKET_FILE),
        dir,
        child,
        client: None,
        engine: ENGINE_ART.to_string(),
        stopped: false,
    }
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
