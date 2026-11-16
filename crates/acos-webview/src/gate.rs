//! Lazy activation: the per-session socket the guest connects to, and the
//! shared broker it is handed to on the **first** connect (WV5b).
//!
//! WV4 mounted a per-session directory at `/run/acos`; WIRE.md §0 says the
//! guest connects to `<dir>/webview.sock` once per session. WVB's `Broker` is
//! one engine per **host**, and its `serve()` binds one socket for one session.
//! Nothing connected the two, and starting Chromium in every session that never
//! creates a `WebView` would cost ~200 MiB for nothing.
//!
//! This module is that seam, and it is deliberately small:
//!
//! ```text
//! guest provider ──connect──▶ Gate (this session, <dir>/webview.sock)
//!                               │ first connect only:
//!                               │   ensure the shared daemon (spawn if absent)
//!                               │   attach {"attach":1,"session":…}
//!                               ▼
//!                         acos-webview --shared  (one per host, $RUNTIME/acos-wv/broker.sock)
//!                               │ one connection = one session = one browser context
//!                               ▼
//!                           Broker ──CDP pipe──▶ chrome-headless-shell
//! ```
//!
//! Why a listener at all, and not "the daemon binds every session socket it will
//! ever need": the session directory does not exist until the session starts, and
//! a WebView-less session must not cause a broker to exist. The gate is a thread
//! with a `UnixListener` and nothing else; [`Gate::start`] never spawns anything
//! (measured: `gate_start_us`), and the daemon — and with it Chromium — only
//! exists after a guest actually connects.
//!
//! The gate is a **proxy**, not an fd pass: both hops are plain `SOCK_STREAM`
//! and the wire bytes are identical end to end, so two `io::copy` threads are
//! the whole transport. That keeps the framing in one place (`server.rs`) and
//! gives the gate no protocol knowledge. What the gate *does* know is the two
//! state transitions of WIRE.md §0/§6: one connection per session socket
//! (`ALREADY_CONNECTED` for a second one while the first is live, exactly as
//! `server::serve` answers), and a reconnect after the broker sent
//! `session.closing` — which re-attaches, spawning the daemon again if it has
//! idle-exited.
//!
//! Failure is structured on the wire. If the daemon cannot be reached or spawned,
//! the gate answers the guest's first frame with `ENGINE_UNAVAILABLE` and closes;
//! it never closes silently and never fabricates a frame (0 false passes).

use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::error::{Error, Result};
use crate::wire::{code, err_response, ErrorBody};

/// The attach line's protocol version (gate ⇄ shared daemon, *not* the guest
/// wire — that one is untouched and stays end to end).
pub const ATTACH_PROTO: u32 = 1;

/// Set to `0` to start no gate at all (the A/B arm of the session-start and PSS
/// measurement; production never sets it).
pub const GATE_ENV: &str = "ACOS_WEBVIEW_GATE";

/// Explicit binary override for the shared daemon (tests, unusual installs).
pub const BROKER_BIN_ENV: &str = "ACOS_WEBVIEW_BIN";

/// Explicit endpoint override for the shared daemon.
pub const BROKER_SOCKET_ENV: &str = "ACOS_WEBVIEW_BROKER_SOCKET";

/// How long [`Gate::start`] waits for a freshly spawned daemon to bind.
pub const SPAWN_WAIT: Duration = Duration::from_secs(20);

/// Where the daemon binary comes from.
#[derive(Debug, Clone, PartialEq)]
pub enum BrokerSource {
    /// Spawn `<bin> --shared --socket …` when the endpoint does not exist yet.
    Binary(PathBuf),
    /// Never spawn: the endpoint is managed elsewhere (tests, an operator).
    External,
}

fn bump(shared: &Shared, f: impl FnOnce(&mut GateStats)) {
    let mut stats = shared.stats.lock().expect("gate stats");
    f(&mut stats);
}

impl BrokerSource {
    /// `$ACOS_WEBVIEW_BIN`, else a sibling of the running binary named
    /// `acos-webview`, else `acos-webview` on `PATH`.
    pub fn from_env(exe_dir: Option<&Path>) -> Self {
        if let Some(raw) = std::env::var_os(BROKER_BIN_ENV) {
            if !raw.is_empty() {
                return Self::Binary(PathBuf::from(raw));
            }
        }
        if let Some(dir) = exe_dir {
            let sibling = dir.join("acos-webview");
            if sibling.is_file() {
                return Self::Binary(sibling);
            }
        }
        Self::Binary(PathBuf::from("acos-webview"))
    }
}

#[derive(Debug, Clone)]
pub struct GateConfig {
    /// The session directory WV4 mounts at `/run/acos`; the socket is
    /// `<session_dir>/webview.sock` and the gate owns both.
    pub session_dir: PathBuf,
    /// Opaque label for the broker's session (one browser context).
    pub session_id: String,
    /// The host-wide shared daemon endpoint.
    pub broker_socket: PathBuf,
    pub broker: BrokerSource,
    /// Engine install root, handed to a spawned daemon.
    pub engine_cache: Option<PathBuf>,
    /// Idle stop for a spawned daemon (engine and process).
    pub idle_shutdown: Option<Duration>,
    /// Daemon stdout/stderr sink; the daemon also writes one status line.
    pub log: Option<PathBuf>,
    /// How long the first connect may wait for a freshly spawned daemon to
    /// bind ([`SPAWN_WAIT`] in production; shorter in tests that assert the
    /// unreachable path).
    pub spawn_wait: Duration,
}

/// Counters a caller (or a test) can assert on.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GateStats {
    /// Guest connections accepted at the session socket.
    pub connects: u64,
    /// Connections refused because one was already attached.
    pub refused: u64,
    /// Times the shared daemon had to be spawned.
    pub broker_spawns: u64,
    /// Successful attaches to the shared daemon.
    pub attaches: u64,
    /// Bytes proxied in both directions.
    pub bytes: u64,
}

struct Shared {
    stats: Mutex<GateStats>,
    live: AtomicUsize,
    stop: AtomicBool,
    /// Pid of the daemon this gate spawned, for teardown assertions.
    last_spawn_pid: AtomicU64,
}

/// A running per-session listener. Drop closes the listener, removes the socket,
/// and removes the session directory when nothing else is in it.
pub struct Gate {
    socket: PathBuf,
    dir: PathBuf,
    cfg: GateConfig,
    shared: Arc<Shared>,
    threads: Mutex<Vec<std::thread::JoinHandle<()>>>,
}

impl Gate {
    /// Bind `<session_dir>/webview.sock` and start the accept thread. Nothing
    /// else happens — no daemon, no engine, no Chromium.
    pub fn start(cfg: GateConfig) -> Result<Self> {
        let dir = cfg.session_dir.clone();
        std::fs::create_dir_all(&dir).map_err(|e| {
            Error::proto(code::INTERNAL, format!("session dir {}: {e}", dir.display()))
        })?;
        let socket = dir.join(crate::server::SESSION_SOCKET_FILE);
        let _ = std::fs::remove_file(&socket);
        let listener = UnixListener::bind(&socket).map_err(|e| {
            Error::proto(
                code::ENGINE_UNAVAILABLE,
                format!("bind {}: {e}", socket.display()),
            )
        })?;
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600));
        listener.set_nonblocking(true).ok();

        let shared = Arc::new(Shared {
            stats: Mutex::new(GateStats::default()),
            live: AtomicUsize::new(0),
            stop: AtomicBool::new(false),
            last_spawn_pid: AtomicU64::new(0),
        });
        let live = Arc::clone(&shared);
        let gate_cfg = cfg.clone();
        let mut threads = Vec::new();
        threads.push(
            std::thread::Builder::new()
                .name("wv-gate".into())
                .spawn(move || accept_loop(listener, gate_cfg, live))
                .map_err(|e| Error::proto(code::INTERNAL, e.to_string()))?,
        );
        Ok(Self {
            socket,
            dir,
            cfg,
            shared,
            threads: Mutex::new(threads),
        })
    }

    /// The gate's own cost and activity.
    pub fn stats(&self) -> GateStats {
        self.shared.stats.lock().expect("gate stats").clone()
    }

    /// The guest-facing socket (`<session_dir>/webview.sock`).
    pub fn socket(&self) -> &Path {
        &self.socket
    }

    /// The session directory (mounted at `/run/acos` by the sandbox).
    pub fn session_dir(&self) -> &Path {
        &self.dir
    }

    /// The shared daemon endpoint this gate attaches to.
    pub fn broker_socket(&self) -> &Path {
        &self.cfg.broker_socket
    }

    /// A connection is currently attached (the daemon is in use by *this* gate).
    pub fn attached(&self) -> bool {
        self.shared.live.load(Ordering::SeqCst) > 0
    }

    /// Pid of the last daemon this gate spawned (`0` if none).
    pub fn spawned_broker_pid(&self) -> u32 {
        self.shared.last_spawn_pid.load(Ordering::SeqCst) as u32
    }
}

impl Drop for Gate {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::SeqCst);
        // Nudge accept() so the thread sees the flag.
        let _ = UnixStream::connect(&self.socket);
        if let Ok(mut threads) = self.threads.lock() {
            for t in threads.drain(..) {
                let _ = t.join();
            }
        }
        let _ = std::fs::remove_file(&self.socket);
        // The session directory is this session's; remove it if the gate is the
        // last thing in it. A leftover entry means another owner (or a bug) and
        // `remove_dir` refuses instead of deleting something we do not own.
        let _ = std::fs::remove_dir(&self.dir);
    }
}

fn accept_loop(listener: UnixListener, cfg: GateConfig, shared: Arc<Shared>) {
    while !shared.stop.load(Ordering::SeqCst) {
        match listener.accept() {
            Ok((stream, _)) => {
                if shared.live.load(Ordering::SeqCst) > 0 {
                    bump(&shared, |s| s.refused += 1);
                    // One connection per session socket (WIRE.md §0), the same
                    // answer `server::serve` gives.
                    let mut s = stream;
                    let body = err_response(
                        "g?",
                        &ErrorBody::new(
                            code::ALREADY_CONNECTED,
                            "a guest connection is already attached to this session socket",
                        ),
                    );
                    if let Ok(bytes) = crate::wire::encode_json(&body) {
                        let _ = crate::wire::write_frame(&mut s, &bytes);
                    }
                    continue;
                }
                shared.live.fetch_add(1, Ordering::SeqCst);
                bump(&shared, |s| s.connects += 1);
                let cfg = cfg.clone();
                let shared = Arc::clone(&shared);
                std::thread::Builder::new()
                    .name("wv-gate-conn".into())
                    .spawn(move || {
                        serve_one(stream, cfg, Arc::clone(&shared));
                        shared.live.fetch_sub(1, Ordering::SeqCst);
                    })
                    .ok();
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(_) => std::thread::sleep(Duration::from_millis(20)),
        }
    }
}

/// Attach this guest connection to the shared daemon, or fail it on the wire.
fn serve_one(guest: UnixStream, cfg: GateConfig, shared: Arc<Shared>) {
    let mut guest = guest;
    let attached = match attach(&cfg, &shared) {
        Ok(s) => s,
        Err(e) => {
            // Structured, never silent: the provider maps ENGINE_UNAVAILABLE to
            // its own error and the verdict says why.
            let body = err_response(
                "g?",
                &ErrorBody::new(code::ENGINE_UNAVAILABLE, e.to_string()),
            );
            if let Ok(bytes) = crate::wire::encode_json(&body) {
                let _ = crate::wire::write_frame(&mut guest, &bytes);
            }
            let _ = guest.shutdown(std::net::Shutdown::Both);
            return;
        }
    };
    bump(&shared, |s| s.attaches += 1);
    proxy(guest, attached, Arc::clone(&shared));
}

/// Connect to the shared daemon (spawning it when absent), then attach.
fn attach(cfg: &GateConfig, shared: &Arc<Shared>) -> Result<UnixStream> {
    let deadline = Instant::now() + cfg.spawn_wait;
    let mut spawned = false;
    let mut last: Option<String>;
    loop {
        match UnixStream::connect(&cfg.broker_socket) {
            Ok(mut stream) => {
                if let Err(e) = write_attach(&mut stream, &cfg.session_id) {
                    last = Some(format!("attach to {}: {e}", cfg.broker_socket.display()));
                    // A refused/stale peer: fall through to the spawn path.
                } else {
                    return Ok(stream);
                }
            }
            Err(e) => last = Some(format!("connect {}: {e}", cfg.broker_socket.display())),
        }
        if !spawned {
            if let BrokerSource::Binary(bin) = &cfg.broker {
                if let Some(parent) = cfg.broker_socket.parent() {
                    std::fs::create_dir_all(parent).ok();
                }
                let (pid, child) = spawn_broker(bin, cfg)?;
                shared.last_spawn_pid.store(pid as u64, Ordering::SeqCst);
                // Reap it: the daemon is detached (setsid) but it is still this
                // process's child, and nobody else will wait for it. One
                // sleeping thread until the daemon exits, instead of a zombie
                // for as long as the session lives.
                std::thread::Builder::new()
                    .name("wv-broker-reaper".into())
                    .spawn(move || {
                        let mut child = child;
                        let _ = child.wait();
                    })
                    .ok();
                bump(shared, |s| s.broker_spawns += 1);
                spawned = true;
            }
        }
        if Instant::now() >= deadline {
            return Err(Error::proto(
                code::ENGINE_UNAVAILABLE,
                format!(
                    "the WebView broker is not reachable at {} ({}); start `acos-webview \
                     --shared` or check {BROKER_SOCKET_ENV}",
                    cfg.broker_socket.display(),
                    last.unwrap_or_else(|| "no endpoint".into())
                ),
            ));
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// One line, then the daemon's one-line answer: nothing from the guest can be
/// in flight yet (the guest speaks only after this returns), so no buffered
/// reader is involved and no wire byte is ever swallowed here.
fn write_attach(stream: &mut UnixStream, session_id: &str) -> std::io::Result<()> {
    let line = json!({"attach": ATTACH_PROTO, "session": session_id});
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    stream.write_all(format!("{line}\n").as_bytes())?;
    stream.flush()?;
    let mut answer = Vec::new();
    let mut byte = [0u8; 1];
    while answer.len() < 4096 {
        match stream.read(&mut byte) {
            Ok(1) if byte[0] == b'\n' => break,
            Ok(1) => answer.push(byte[0]),
            Ok(_) => {}
            Err(e) => return Err(e),
        }
    }
    let ack: Value = serde_json::from_slice(&answer)
        .map_err(|e| std::io::Error::other(format!("attach answer {answer:?}: {e}")))?;
    if ack["ok"].as_bool().unwrap_or(false) {
        stream.set_read_timeout(None)?;
        Ok(())
    } else {
        Err(std::io::Error::other(format!(
            "daemon refused the attach: {}",
            ack["error"].as_str().unwrap_or(&ack.to_string())
        )))
    }
}

/// Spawn the shared daemon, detached from this session's lifetime (one broker
/// per host, per decision), with its fds out of the session's pipes.
fn spawn_broker(bin: &Path, cfg: &GateConfig) -> Result<(i32, std::process::Child)> {
    let mut cmd = Command::new(bin);
    cmd.arg("--shared")
        .arg("--socket")
        .arg(&cfg.broker_socket)
        .env_remove(GATE_ENV)
        .stdin(Stdio::null())
        .stdout(Stdio::null());
    if let Some(log) = &cfg.log {
        match std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(log)
        {
            Ok(f) => {
                cmd.stderr(Stdio::from(f.try_clone().map_err(|e| {
                    Error::proto(code::INTERNAL, format!("daemon log {}: {e}", log.display()))
                })?));
                cmd.stdout(Stdio::from(f));
            }
            Err(e) => {
                return Err(Error::proto(
                    code::INTERNAL,
                    format!("daemon log {}: {e}", log.display()),
                ))
            }
        }
    } else {
        cmd.stderr(Stdio::null());
    }
    if let Some(cache) = &cfg.engine_cache {
        cmd.arg("--cache").arg(cache);
    }
    if let Some(idle) = cfg.idle_shutdown {
        cmd.arg("--idle-shutdown-secs").arg(idle.as_secs().to_string());
    }
    // setsid: the broker outlives the session that woke it up; it stops on its
    // own idle timer, not on its parent's death.
    unsafe {
        use std::os::unix::process::CommandExt;
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    match cmd.spawn() {
        Ok(child) => Ok((child.id() as i32, child)),
        Err(e) => Err(Error::proto(
            code::ENGINE_UNAVAILABLE,
            format!("cannot start {}: {e}", bin.display()),
        )),
    }
}

/// Byte-faithful both ways until both sides are done. `io::copy` is the whole
/// protocol: frames are opaque here, which is why a gate regression cannot
/// corrupt one.
fn proxy(guest: UnixStream, daemon: UnixStream, shared: Arc<Shared>) {
    let g2d = {
        let mut from = guest.try_clone().expect("clone guest");
        let mut to = daemon.try_clone().expect("clone daemon");
        let shared = Arc::clone(&shared);
        std::thread::spawn(move || {
            let n = std::io::copy(&mut from, &mut to).unwrap_or(0);
            bump(&shared, |s| s.bytes += n);
            let _ = to.shutdown(std::net::Shutdown::Write);
        })
    };
    let mut from = daemon;
    let mut to = guest;
    let n = std::io::copy(&mut from, &mut to).unwrap_or(0);
    bump(&shared, |s| s.bytes += n);
    let _ = to.shutdown(std::net::Shutdown::Write);
    let _ = g2d.join();
    let _ = to.shutdown(std::net::Shutdown::Both);
}
