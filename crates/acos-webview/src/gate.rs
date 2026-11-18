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

use std::ffi::OsString;
use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::error::{Error, Result};
use crate::wire::{code, err_response, ErrorBody};

/// The attach line's protocol version (gate ⇄ shared daemon, *not* the guest
/// wire — that one is untouched and stays end to end).
///
/// **v2 (WVSCOPE)** adds the client's build id and requires the daemon to report
/// its own (`results/handoff/WVB/WIRE.md` §0.1). A daemon of another build is
/// refused with `BUILD_MISMATCH` instead of silently serving its old behaviour for
/// the rest of the host's life; v1 lines are still served (a new daemon with an
/// old client), because it is the *client* that must not trust a stranger.
pub const ATTACH_PROTO: u32 = 2;

/// `0`/`off` disables the transient-scope spawn of the shared daemon
/// ([`SPAWN_SCOPE_ENV`]) — the before-WVSCOPE behaviour (a plain `setsid`), and
/// the arm the fallback test uses. Production never sets it.
pub const SPAWN_SCOPE_ENV: &str = "ACOS_WEBVIEW_SCOPE";

/// How long a refused build waits for a stale daemon that has no sessions left
/// to actually go away before falling back to a build-private endpoint.
pub const STALE_EXIT_WAIT: Duration = Duration::from_secs(3);

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
    /// Attaches refused because the daemon was built from other code (WVSCOPE).
    pub build_mismatches: u64,
    /// Times a refused daemon said it had no live sessions and would exit, so the
    /// gate waited for *its own* daemon on the same endpoint instead of taking a
    /// second endpoint.
    pub stale_exits: u64,
}

struct Shared {
    stats: Mutex<GateStats>,
    live: AtomicUsize,
    stop: AtomicBool,
    /// Pid of the daemon this gate spawned, for teardown assertions.
    last_spawn_pid: AtomicU64,
    /// The endpoint this gate attaches to: the configured one until a daemon of
    /// another build refuses it and has to stay (see [`build_private_endpoint`]).
    endpoint: Mutex<PathBuf>,
    /// Set once the gate moved to its build-private endpoint, so a second
    /// mismatch cannot move it again (or bounce between two endpoints forever).
    switched: AtomicBool,
    /// The endpoint a spawn was already attempted for, so one attach attempt
    /// spawns once (today's behaviour) per endpoint — a gate that moves to its
    /// build-private endpoint must start a daemon there too.
    spawned_for: Mutex<Option<PathBuf>>,
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
            Error::proto(
                code::INTERNAL,
                format!("session dir {}: {e}", dir.display()),
            )
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
            endpoint: Mutex::new(cfg.broker_socket.clone()),
            switched: AtomicBool::new(false),
            spawned_for: Mutex::new(None),
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

    /// The endpoint actually in use: the configured one, or this build's own when
    /// a daemon of another build refused the attach and could not be replaced
    /// (WVSCOPE).
    pub fn endpoint(&self) -> PathBuf {
        self.shared.endpoint.lock().expect("gate endpoint").clone()
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

/// The attach line: one JSON object, one `\n`, sent before any framed byte.
///
/// Normative copy of `results/handoff/WVB/WIRE.md` §0.1; its byte-level test is
/// the executable form of that section.
pub fn encode_attach(session_id: &str, build: &str) -> Vec<u8> {
    // `serde_json`'s object order is sorted, so the bytes are deterministic and
    // a test can pin them exactly.
    let line = json!({"attach": ATTACH_PROTO, "session": session_id, "build": build});
    let mut bytes = line.to_string().into_bytes();
    bytes.push(b'\n');
    bytes
}

/// The daemon's one-line answer to the attach line (WIRE.md §0.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttachAnswer {
    /// Same build (or a v1 daemon that predates the check): the connection is a
    /// session now.
    Accepted { build: Option<String> },
    /// A daemon built from other code. `exiting` means it had no live session and
    /// stops itself, so the endpoint is about to be free for this build.
    Mismatch {
        daemon: Option<String>,
        sessions: Option<u64>,
        exiting: bool,
        reason: String,
    },
    /// The daemon refused for another reason (an attach version it does not
    /// speak, a malformed line). Treated like a stale daemon: it cannot be this
    /// build's.
    Refused { reason: String },
}

/// Parse a daemon's attach answer. Never a silent guess: an answer that is not a
/// JSON object with `ok` is a refusal naming why.
pub fn parse_attach_answer(bytes: &[u8]) -> AttachAnswer {
    let Ok(v) = serde_json::from_slice::<Value>(bytes) else {
        return AttachAnswer::Refused {
            reason: format!("attach answer is not JSON: {:?}", String::from_utf8_lossy(bytes)),
        };
    };
    let build = v["build"].as_str().map(|b| b.to_string());
    match v["ok"].as_bool() {
        Some(true) => AttachAnswer::Accepted { build },
        Some(false) => {
            let reason = v["error"]
                .as_str()
                .map(|e| e.to_string())
                .or_else(|| v["code"].as_str().map(|c| c.to_string()))
                .unwrap_or_else(|| v.to_string());
            if v["code"].as_str() == Some(code::BUILD_MISMATCH) {
                AttachAnswer::Mismatch {
                    daemon: build,
                    sessions: v["sessions"].as_u64(),
                    exiting: v["exiting"].as_bool().unwrap_or(false),
                    reason,
                }
            } else {
                AttachAnswer::Refused { reason }
            }
        }
        None => AttachAnswer::Refused {
            reason: format!("attach answer has no `ok`: {v}"),
        },
    }
}

/// Only a daemon that **reports this build** counts as accepted. An `ok` without
/// a build id is a daemon that cannot prove what it is (a pre-WVSCOPE build), and
/// that is the case this whole handshake exists for: it is refused like a
/// mismatch instead of being trusted with the session.
pub fn classify_attach(answer: AttachAnswer) -> AttachAnswer {
    match answer {
        AttachAnswer::Accepted { build } if build.as_deref() == Some(crate::build_id()) => {
            AttachAnswer::Accepted { build }
        }
        AttachAnswer::Accepted { build } => AttachAnswer::Mismatch {
            daemon: build,
            sessions: None,
            exiting: false,
            reason: "the daemon did not report this build".to_string(),
        },
        other => other,
    }
}

/// This build's own endpoint, next to the configured one.
///
/// A daemon of another build that still has sessions cannot be asked to go away
/// (that would kill another session's engine), so this build runs *its own*
/// broker beside it: `<dir>/broker-<version>-<source8>.sock`. The name is derived
/// from [`crate::build_id`], so two builds can never meet on one path.
pub fn build_private_endpoint(canonical: &Path) -> PathBuf {
    let name = format!("broker-{}.sock", crate::build_tag());
    match canonical.parent() {
        Some(dir) => dir.join(name),
        None => PathBuf::from(name),
    }
}

/// Wait (bounded) for a stale daemon that promised to exit to actually stop
/// answering on `endpoint`. Returns whether it did.
fn wait_for_stale_exit(endpoint: &Path, limit: Duration) -> bool {
    let deadline = Instant::now() + limit;
    while Instant::now() < deadline {
        if UnixStream::connect(endpoint).is_err() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    false
}

/// Move this gate to its build-private endpoint, once.
///
/// Returns `Err` when the gate is already there: a daemon built from other code
/// holds the path *this* build derives from its own build id, so there is nothing
/// left to try and looping would only burn the deadline.
fn use_build_private_endpoint(
    cfg: &GateConfig,
    shared: &Arc<Shared>,
    refused: &Path,
    why: &str,
) -> Result<PathBuf> {
    let own = build_private_endpoint(&cfg.broker_socket);
    if refused == own {
        return Err(Error::proto(
            code::ENGINE_UNAVAILABLE,
            format!(
                "{} refused the attach ({why}) and it is this build's own endpoint; \
                 delete it and retry",
                own.display()
            ),
        ));
    }
    if !shared.switched.swap(true, Ordering::SeqCst) {
        eprintln!(
            "acos-webview-gate: the broker on {} is not this build ({}); using this build's \
             endpoint {} instead",
            refused.display(),
            why,
            own.display()
        );
        *shared.endpoint.lock().expect("gate endpoint") = own.clone();
    }
    Ok(own)
}

/// Connect to the shared daemon (spawning it when absent), then attach.
fn attach(cfg: &GateConfig, shared: &Arc<Shared>) -> Result<UnixStream> {
    let deadline = Instant::now() + cfg.spawn_wait;
    // Filled by every failed attempt; the deadline branch below is only reachable
    // after one, so the initial value is documentation, not a fallback.
    let mut last: String;
    loop {
        let endpoint = shared.endpoint.lock().expect("gate endpoint").clone();
        match UnixStream::connect(&endpoint) {
            Ok(mut stream) => match write_attach(&mut stream, &cfg.session_id) {
                Ok(AttachAnswer::Accepted { build: _ }) => return Ok(stream),
                Ok(AttachAnswer::Mismatch {
                    daemon,
                    sessions,
                    exiting,
                    reason,
                }) => {
                    last = format!(
                        "{} refused this build ({reason}; daemon build {}, sessions {})",
                        endpoint.display(),
                        daemon.as_deref().unwrap_or("unknown"),
                        sessions.map(|n| n.to_string()).unwrap_or_else(|| "?".into()),
                    );
                    drop(stream);
                    bump(shared, |s| s.build_mismatches += 1);
                    let busy = sessions.unwrap_or(u64::MAX) > 0;
                    if exiting && !busy {
                        // It has no session to lose, so it stops itself and the
                        // endpoint becomes this build's: never a second daemon
                        // beside a dying one.
                        bump(shared, |s| s.stale_exits += 1);
                        eprintln!(
                            "acos-webview-gate: the broker on {} was built from other code \
                             (daemon {}, this build {}); it has no live session and is exiting, \
                             waiting for it",
                            endpoint.display(),
                            daemon.as_deref().unwrap_or("unknown"),
                            crate::build_id()
                        );
                        if !wait_for_stale_exit(&endpoint, STALE_EXIT_WAIT) {
                            use_build_private_endpoint(cfg, shared, &endpoint, &reason)?;
                        }
                    } else {
                        // Sessions are live on it: leaving them alone and using
                        // this build's own endpoint is the only honest choice.
                        use_build_private_endpoint(cfg, shared, &endpoint, &reason)?;
                    }
                }
                Ok(AttachAnswer::Refused { reason }) => {
                    last = format!("{} refused the attach: {reason}", endpoint.display());
                    drop(stream);
                    if reason.contains("version") {
                        // A daemon that does not speak this attach version
                        // predates it: another build by construction.
                        bump(shared, |s| s.build_mismatches += 1);
                        use_build_private_endpoint(cfg, shared, &endpoint, &reason)?;
                    }
                    // Anything else (a half-closed socket, a daemon shutting
                    // down) is transient: retry until the deadline reports it.
                }
                Err(e) => {
                    last = format!("attach to {}: {e}", endpoint.display());
                }
            },
            Err(e) => {
                last = format!("connect {}: {e}", endpoint.display())
            }
        }
        let spawn_endpoint = shared.endpoint.lock().expect("gate endpoint").clone();
        let spawn_here = {
            let mut spawned_for = shared.spawned_for.lock().expect("gate spawned_for");
            if spawned_for.as_ref() == Some(&spawn_endpoint) {
                false
            } else {
                *spawned_for = Some(spawn_endpoint.clone());
                true
            }
        };
        if spawn_here {
            if let BrokerSource::Binary(bin) = &cfg.broker {
                let endpoint = spawn_endpoint;
                if let Some(parent) = endpoint.parent() {
                    std::fs::create_dir_all(parent).ok();
                }
                let (pid, child) = spawn_broker(bin, cfg, &endpoint)?;
                shared.last_spawn_pid.store(pid as u64, Ordering::SeqCst);
                // Reap it: the daemon is detached (its own scope or `setsid`) but
                // it is still this process's child, and nobody else will wait for
                // it. One sleeping thread until the daemon exits, instead of a
                // zombie for as long as the session lives.
                std::thread::Builder::new()
                    .name("wv-broker-reaper".into())
                    .spawn(move || {
                        let mut child = child;
                        let _ = child.wait();
                    })
                    .ok();
                bump(shared, |s| s.broker_spawns += 1);
            }
        }
        if Instant::now() >= deadline {
            return Err(Error::proto(
                code::ENGINE_UNAVAILABLE,
                format!(
                    "the WebView broker is not reachable at {} ({}); start `acos-webview \
                     --shared` or check {BROKER_SOCKET_ENV}",
                    shared.endpoint.lock().expect("gate endpoint").display(),
                    last
                ),
            ));
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// One line, then the daemon's one-line answer: nothing from the guest can be
/// in flight yet (the guest speaks only after this returns), so no buffered
/// reader is involved and no wire byte is ever swallowed here.
fn write_attach(stream: &mut UnixStream, session_id: &str) -> std::io::Result<AttachAnswer> {
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    stream.write_all(&encode_attach(session_id, crate::build_id()))?;
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
    stream.set_read_timeout(None)?;
    Ok(classify_attach(parse_attach_answer(&answer)))
}

/// How a spawned daemon is detached from the session that woke it up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpawnScope {
    /// `systemd-run --user --scope --unit=acos-webview-<hash>`: the daemon gets
    /// its own transient cgroup, so a session's teardown (a scope-wide SIGKILL,
    /// what `guarded.sh` and every run harness do) cannot take the host's engine
    /// down with it — the MERGE040A `NO_VIEW` + unanswered `render` in *another*
    /// agent's session (WVSCOPE, ADR-0031 amendment).
    TransientScope,
    /// A plain `setsid` child in the session's own cgroup: today's behaviour, the
    /// fallback when systemd --user is not usable.
    SetsidOnly,
}

/// The command that starts the shared daemon, as data: what [`spawn_broker`]
/// runs, and what the spawn test pins.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrokerSpawn {
    pub program: PathBuf,
    pub args: Vec<OsString>,
    pub scope: SpawnScope,
}

impl BrokerSpawn {
    /// The transient scope unit name, when this spawn uses one.
    pub fn unit(&self) -> Option<String> {
        self.args
            .iter()
            .find_map(|a| a.to_str()?.strip_prefix("--unit=").map(|u| u.to_string()))
    }
}

/// A stable, printable tag of an endpoint, for the scope's unit name.
fn endpoint_tag(endpoint: &Path) -> String {
    use sha2::Digest;
    let digest = sha2::Sha256::digest(endpoint.as_os_str().as_encoded_bytes());
    hex::encode(&digest[..6])
}

/// Build the spawn command. `scope` decides *who* runs the daemon; the daemon's
/// own arguments are the same either way.
pub fn spawn_plan(bin: &Path, cfg: &GateConfig, endpoint: &Path, scope: SpawnScope) -> BrokerSpawn {
    let mut args: Vec<OsString> = Vec::new();
    let program = match scope {
        SpawnScope::TransientScope => {
            // The unit is per endpoint + per spawn attempt: two gates racing for
            // one endpoint must not collide on a unit name (the broker's own bind
            // race decides who serves), and `--collect` unloads it when the daemon
            // exits on its idle timer.
            let unit = format!(
                "acos-webview-{}-{}",
                endpoint_tag(endpoint),
                std::process::id()
            );
            args.push("--user".into());
            args.push("--scope".into());
            args.push("--quiet".into());
            args.push("--collect".into());
            args.push(format!("--unit={unit}").into());
            args.push("--".into());
            // Inside a scope the unit runs `systemd-run`; the daemon is its
            // argument.
            args.push(bin.as_os_str().to_os_string());
            PathBuf::from("systemd-run")
        }
        // Without a scope the daemon *is* the program: argv[0] comes from
        // `Command`, so its path must not be an argument as well.
        SpawnScope::SetsidOnly => bin.to_path_buf(),
    };
    args.push("--shared".into());
    args.push("--socket".into());
    args.push(endpoint.as_os_str().to_os_string());
    if let Some(cache) = &cfg.engine_cache {
        args.push("--cache".into());
        args.push(cache.as_os_str().to_os_string());
    }
    if let Some(idle) = cfg.idle_shutdown {
        args.push("--idle-shutdown-secs".into());
        args.push(idle.as_secs().to_string().into());
    }
    BrokerSpawn {
        program,
        args,
        scope,
    }
}

/// Whether `systemd-run --user --scope` works here, asked once per process with
/// a throwaway transient unit.
fn systemd_scope_available() -> bool {
    static AVAILABLE: OnceLock<bool> = OnceLock::new();
    *AVAILABLE.get_or_init(|| {
        let unit = format!("acos-webview-probe-{}", std::process::id());
        let mut child = match Command::new("systemd-run")
            .args([
                "--user",
                "--scope",
                "--quiet",
                "--collect",
                &format!("--unit={unit}"),
                "--",
                "/bin/true",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(child) => child,
            Err(_) => return false,
        };
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match child.try_wait() {
                Ok(Some(status)) => return status.success(),
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(10))
                }
                _ => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return false;
                }
            }
        }
    })
}

/// The scope this host gets: an explicit `$ACOS_WEBVIEW_SCOPE=0` or an unusable
/// systemd --user falls back to today's `setsid`, **with a warning** — a silent
/// fallback would make the broker's lifetime look fixed when it is not.
pub fn spawn_scope() -> SpawnScope {
    if matches!(
        std::env::var(SPAWN_SCOPE_ENV).as_deref(),
        Ok("0") | Ok("off") | Ok("false")
    ) {
        warn_once("spawn scope disabled by ACOS_WEBVIEW_SCOPE=0 (setsid fallback)");
        return SpawnScope::SetsidOnly;
    }
    if systemd_scope_available() {
        SpawnScope::TransientScope
    } else {
        warn_once(
            "systemd --user is not usable here, so the shared broker is spawned into this \
             session's cgroup (setsid): a session teardown that kills its scope will kill the \
             host's engine for every other session",
        );
        SpawnScope::SetsidOnly
    }
}

/// One warning per process: a spawn happens once per endpoint, the reason does
/// not get more true by being repeated.
fn warn_once(message: &str) {
    static ONCE: OnceLock<()> = OnceLock::new();
    if ONCE.set(()).is_ok() {
        eprintln!("acos-webview-gate: warning: {message}");
    }
}

/// Spawn the shared daemon, detached from this session's lifetime (one broker
/// per host, per decision), with its fds out of the session's pipes.
fn spawn_broker(
    bin: &Path,
    cfg: &GateConfig,
    endpoint: &Path,
) -> Result<(i32, std::process::Child)> {
    let plan = spawn_plan(bin, cfg, endpoint, spawn_scope());
    let mut cmd = Command::new(&plan.program);
    cmd.args(&plan.args)
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
    // setsid: the broker outlives the session that woke it up; it stops on its
    // own idle timer, not on its parent's death. (Inside a transient scope this
    // only detaches the session/terminal — the cgroup is already the unit's.)
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
            format!("cannot start {}: {e}", plan.program.display()),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(dir: &Path) -> GateConfig {
        GateConfig {
            session_dir: dir.join("s"),
            session_id: "s".into(),
            broker_socket: dir.join("run/broker.sock"),
            broker: BrokerSource::External,
            engine_cache: Some(PathBuf::from("/cache")),
            idle_shutdown: Some(Duration::from_secs(600)),
            log: None,
            spawn_wait: Duration::from_secs(1),
        }
    }

    /// WIRE.md §0.1 is normative: the attach line's bytes are pinned here, so a
    /// daemon implementation can be checked against them without guessing.
    #[test]
    fn the_attach_line_is_exactly_the_documented_bytes() {
        let bytes = encode_attach("s1", "0.3.0+gdeadbeefcafe.0123456789abcdef");
        assert_eq!(
            String::from_utf8(bytes.clone()).unwrap(),
            "{\"attach\":2,\"build\":\"0.3.0+gdeadbeefcafe.0123456789abcdef\",\
             \"session\":\"s1\"}\n"
        );
        // This build's own line carries its own build id.
        let own = encode_attach("wv", crate::build_id());
        let text = String::from_utf8(own).unwrap();
        assert!(text.starts_with("{\"attach\":2,\"build\":\""));
        assert!(text.contains(&format!("\"session\":\"wv\"}}\n")));
        assert!(!text.contains("attach\":1"), "{text}");
    }

    #[test]
    fn the_build_id_names_the_version_the_commit_and_the_sources() {
        let id = crate::build_id();
        assert!(id.starts_with(&format!("{}+g", crate::VERSION)), "{id}");
        assert!(id.contains(crate::GIT_SHA), "{id}");
        assert!(id.contains(crate::SOURCE_HASH), "{id}");
        // Today's tree is this build's own code, so the id is the one a daemon
        // built from it must report.
        assert_eq!(crate::build_id(), crate::build_id(), "stable within a build");
        assert!(!crate::build_tag().contains('/'), "a tag names a file");
    }

    /// A daemon's answer, classified: only *this* build is trusted.
    #[test]
    fn only_this_build_is_accepted() {
        let ours = AttachAnswer::Accepted {
            build: Some(crate::build_id().to_string()),
        };
        assert_eq!(classify_attach(ours.clone()), ours);

        // Another build: a mismatch, never a silent attach.
        let other = classify_attach(AttachAnswer::Accepted {
            build: Some("0.3.0+g000000000000.ffffffffffffffff".into()),
        });
        assert!(matches!(other, AttachAnswer::Mismatch { .. }), "{other:?}");

        // No build at all (a pre-WVSCOPE daemon): also refused, because it cannot
        // prove what it is.
        let none = classify_attach(AttachAnswer::Accepted { build: None });
        match none {
            AttachAnswer::Mismatch { reason, .. } => assert!(reason.contains("did not report")),
            other => panic!("expected a mismatch, got {other:?}"),
        }
    }

    #[test]
    fn the_daemons_mismatch_answer_is_read_whole() {
        let busy = parse_attach_answer(
            br#"{"ok":false,"code":"BUILD_MISMATCH","build":"0.3.0+g1.2","sessions":2,"exiting":false}"#,
        );
        assert_eq!(
            busy,
            AttachAnswer::Mismatch {
                daemon: Some("0.3.0+g1.2".into()),
                sessions: Some(2),
                exiting: false,
                reason: "BUILD_MISMATCH".into(),
            }
        );
        let idle = parse_attach_answer(
            br#"{"ok":false,"code":"BUILD_MISMATCH","build":"0.3.0+g1.2","sessions":0,"exiting":true}"#,
        );
        assert!(matches!(
            idle,
            AttachAnswer::Mismatch {
                exiting: true,
                sessions: Some(0),
                ..
            }
        ));
        // A daemon that predates the version: a refusal that names the version,
        // which the gate reads as "another build" (it is the old text verbatim).
        let old = parse_attach_answer(br#"{"ok":false,"error":"attach version unsupported"}"#);
        match &old {
            AttachAnswer::Refused { reason } => assert!(reason.contains("version")),
            other => panic!("expected a refusal, got {other:?}"),
        }
        // Garbage is a refusal, never an accepted attach.
        assert!(matches!(
            parse_attach_answer(b"\x00\x01 not json"),
            AttachAnswer::Refused { .. }
        ));
        assert!(matches!(
            parse_attach_answer(b"{\"error\":\"no ok\"}"),
            AttachAnswer::Refused { .. }
        ));
    }

    /// A build-private endpoint is derived from the build id, so two builds can
    /// never meet on one path.
    #[test]
    fn a_build_private_endpoint_is_next_to_the_shared_one_and_names_the_build() {
        let canonical = Path::new("/run/user/1000/acos-wv/broker.sock");
        let own = build_private_endpoint(canonical);
        assert_eq!(own.parent(), canonical.parent());
        assert_eq!(
            own.file_name().unwrap().to_string_lossy(),
            format!("broker-{}.sock", crate::build_tag())
        );
        assert_ne!(own, canonical);
        // Namespaced: the build-private socket has no parent (`broker-x.sock`).
        assert_eq!(
            build_private_endpoint(Path::new("broker.sock")),
            PathBuf::from(format!("broker-{}.sock", crate::build_tag()))
        );
    }

    fn plan(scope: SpawnScope, cfg: &GateConfig) -> BrokerSpawn {
        spawn_plan(
            Path::new("/opt/acos/acos-webview"),
            cfg,
            &cfg.broker_socket,
            scope,
        )
    }

    /// The spawn command, as data: today's behaviour is `setsid` + the binary; the
    /// fix runs the same daemon in its **own transient scope** so a session's
    /// teardown cannot take the host's engine with it (WVSCOPE decision 1).
    #[test]
    fn the_spawned_daemon_runs_in_its_own_transient_scope() {
        let tmp = PathBuf::from("/run/user/1000/acos-wv");
        let cfg = cfg(&tmp);
        let scoped = plan(SpawnScope::TransientScope, &cfg);
        assert_eq!(scoped.program, PathBuf::from("systemd-run"));
        assert_eq!(scoped.scope, SpawnScope::TransientScope);
        let args: Vec<String> = scoped
            .args
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        // The scope flags, then `--`, then the daemon and its own arguments.
        let dash = args.iter().position(|a| a == "--").expect("argv separator");
        for flag in ["--user", "--scope", "--quiet", "--collect"] {
            assert!(
                args[..dash].iter().any(|a| a == flag),
                "missing {flag} in {args:?}"
            );
        }
        let unit = scoped.unit().expect("a unit name");
        assert!(
            unit.starts_with("acos-webview-"),
            "the unit names the daemon: {unit}"
        );
        assert!(
            unit.ends_with(&std::process::id().to_string()),
            "the unit is per spawn attempt, so two gates cannot collide: {unit}"
        );
        assert_eq!(
            &args[dash + 1..],
            &[
                "/opt/acos/acos-webview".to_string(),
                "--shared".to_string(),
                "--socket".to_string(),
                "/run/user/1000/acos-wv/run/broker.sock".to_string(),
                "--cache".to_string(),
                "/cache".to_string(),
                "--idle-shutdown-secs".to_string(),
                "600".to_string(),
            ]
            .iter()
            .map(|s| s.to_string())
            .collect::<Vec<_>>()[..]
        );

        // The fallback is exactly what ran before: the binary itself.
        let plain = plan(SpawnScope::SetsidOnly, &cfg);
        assert_eq!(plain.program, PathBuf::from("/opt/acos/acos-webview"));
        assert_eq!(plain.unit(), None);
        assert_eq!(
            plain.args[0], *"--shared",
            "argv[0] is the program, never an argument: {:?}",
            plain.args
        );
        assert_eq!(plain.args[1], *"--socket");
        assert_eq!(plain.args[2], *"/run/user/1000/acos-wv/run/broker.sock");
    }

    /// The unit name is derived from the endpoint, so two *different* endpoints
    /// never share a scope, and the same endpoint keeps a stable name.
    #[test]
    fn the_scope_unit_is_per_endpoint() {
        assert_eq!(
            endpoint_tag(Path::new("/run/user/1000/acos-wv/broker.sock")),
            endpoint_tag(Path::new("/run/user/1000/acos-wv/broker.sock"))
        );
        assert_ne!(
            endpoint_tag(Path::new("/run/user/1000/acos-wv/broker.sock")),
            endpoint_tag(Path::new("/run/user/1000/acos-wv/broker-0.3.0-0123abcd.sock"))
        );
        // 12 hex chars, so the name stays a legal unit name and a short path.
        assert_eq!(
            endpoint_tag(Path::new("/some/endpoint")).len(),
            12,
            "12 hex chars"
        );
    }
}
