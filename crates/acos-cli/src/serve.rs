//! `acos serve` — the local session daemon (F-08).
//!
//! The CLI itself is stateless: every `acos observe/tap/logs` process used to
//! seed a fresh session and relaunch. `acos serve` holds live
//! [`SessionBackend`] sessions (one worker thread / confined native child each)
//! behind a private Unix socket so that `acos launch --keep`, `acos session …`
//! and the top-level `acos observe/tap/logs` commands address the *same* live
//! session across separate processes.
//!
//! The daemon reuses the MCP [`SessionBackend`] tool surface and ACOS envelopes
//! ([`acos_mcp::daemon`]), not a new protocol. The runtime directory is created
//! `0700`; every accepted connection is checked against the daemon's own uid via
//! `SO_PEERCRED`; the daemon exits when it has held no session and served no
//! connection for `--idle-timeout`. Native sessions are delegated to the same
//! confined child the MCP server uses.
//!
//! **W50: the product engine is real ART.** `acos_launch` defaults to the ART
//! session machinery (`crate::engine::serve::ServeEngine`), one
//! `acos-art launch --interactive` guest per session, so the launch → tree →
//! tap → type → key → screenshot → assert → logs → reset loop an agent drives
//! over this socket is the same real-ART loop `acos verify --engine art` runs.
//! The Rust VM is reachable only through an explicit `engine: "rust-vm"` and
//! every reply from it is labelled `rust-vm, frozen`; an unknown engine is a
//! structured `UNSUPPORTED_BACKEND`. `--engine rust-vm` moves the daemon default
//! (diagnostics only); it is never chosen implicitly.

use std::io::{BufRead, BufReader, ErrorKind, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::io::AsRawFd;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use acos_mcp::daemon::{self, DaemonClient};
use acos_mcp::{BackendConfig, CliBackend, SessionBackend};
use acos_protocol::ResourceBudget;
use acos_runtime::session::SessionOptions;
use serde_json::{json, Value};

use crate::engine::art::{m5_now_us, m5_trace};
use crate::engine::serve::{parse_engine_flag, ServeEngine};

/// Arguments for `acos serve`.
#[derive(Debug, clap::Args)]
pub struct ServeArgs {
    /// Socket path (default: the private runtime dir's `session.sock`).
    #[arg(long)]
    pub socket: Option<PathBuf>,

    /// Exit after this many seconds with no connection and no held session.
    #[arg(long, default_value_t = 1800)]
    pub idle_timeout: u64,

    /// Maximum number of concurrently held sessions.
    #[arg(long, default_value_t = 8)]
    pub max_sessions: usize,

    /// Host-process RSS ceiling in MiB (`0` = disabled).
    #[arg(long, default_value_t = 0)]
    pub rss_max_mb: u64,

    /// Guest heap ceiling per session in MiB (`0` = VM default).
    #[arg(long, default_value_t = 0)]
    pub heap_max_mb: u64,

    /// Exit (destroying every session first) when this pid is gone (SERVELEAK).
    ///
    /// Set by a caller that spawns the daemon for its own use — the bench, a CI
    /// job, a wrapper: if that process disappears (even by `kill -9`), nobody
    /// will ever address the sessions again, so the daemon takes them down with
    /// it instead of holding live guests for `--idle-timeout`. A shared daemon
    /// (`acos session …` auto-start) passes no owner: another client may attach.
    #[arg(long)]
    pub owner_pid: Option<i32>,

    /// Emit a JSON startup banner on stdout and do not detach.
    #[arg(long)]
    pub json: bool,

    /// This daemon was started by a client (`acos session …`, the MCP server's
    /// private daemon), not by an operator running `acos serve`.
    ///
    /// REL3D: reported in `hello`. Only an auto-started, idle daemon of another
    /// build may be replaced by a matching client; an operator-started one is
    /// never signalled (the client starts its own private daemon instead).
    #[arg(long)]
    pub auto_started: bool,

    /// Default execution engine for `acos_launch` when the request names none.
    ///
    /// `art` (default) drives real Google ART through one `acos-art launch
    /// --interactive` guest per session. `rust-vm` is the frozen Rust VM and
    /// every reply from it is labelled `rust-vm, frozen`. Anything else is
    /// refused at startup; there is no silent fallback.
    #[arg(long, default_value = "art", value_name = "art|rust-vm")]
    pub engine: String,
}

/// Run `acos serve`.
pub fn run(args: ServeArgs) -> ExitCode {
    // W50: the engine is explicit. An unknown value refuses to start instead of
    // guessing (and never falls back to the Rust VM).
    let default_engine = match parse_engine_flag(&args.engine) {
        Ok(engine) => engine,
        Err(message) => {
            eprintln!("acos serve: {message}");
            return ExitCode::from(2);
        }
    };
    let socket = args.socket.clone().unwrap_or_else(daemon::socket_path);
    if let Some(parent) = socket.parent() {
        if let Err(e) = prepare_runtime_dir(parent) {
            eprintln!("acos serve: {}: {e}", parent.display());
            return ExitCode::FAILURE;
        }
    }
    // Refuse to clobber a live daemon; remove a stale socket.
    if socket.exists() {
        match DaemonClient::connect(&socket) {
            Ok(_) => {
                eprintln!(
                    "acos serve: a session daemon is already listening on {}",
                    socket.display()
                );
                return ExitCode::FAILURE;
            }
            Err(_) => {
                let _ = std::fs::remove_file(&socket);
            }
        }
    }
    let listener = match UnixListener::bind(&socket) {
        Ok(listener) => listener,
        Err(e) => {
            eprintln!("acos serve: bind {}: {e}", socket.display());
            return ExitCode::FAILURE;
        }
    };
    let _ = std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600));
    if let Err(e) = listener.set_nonblocking(true) {
        eprintln!("acos serve: set_nonblocking: {e}");
        return ExitCode::FAILURE;
    }

    let limits = SessionOptions {
        rss_max_bytes: (args.rss_max_mb as usize).saturating_mul(1024 * 1024),
        heap_max_bytes: (args.heap_max_mb as usize).saturating_mul(1024 * 1024),
        ..SessionOptions::default()
    };
    let backend = Arc::new(
        SessionBackend::with_cli(
            CliBackend::new(BackendConfig::for_binary("/nonexistent/acos-serve")),
            limits,
        )
        .with_native_child_binary(confined_child_binary()),
    );
    // ART frame mirrors live in the daemon's private runtime directory (0600
    // socket dir, never /tmp and never the repo root).
    let artifacts_root = std::env::var_os("ACOS_SERVE_ARTIFACTS")
        .map(PathBuf::from)
        .unwrap_or_else(|| daemon::runtime_dir().join("art-sessions"));
    let engine = Arc::new(ServeEngine::with_identity(
        backend,
        default_engine,
        args.max_sessions,
        artifacts_root.clone(),
        args.auto_started,
    ));
    let shared = Arc::new(SharedState {
        last_activity: Mutex::new(Instant::now()),
        active: AtomicUsize::new(0),
    });

    if let Some(owner) = args.owner_pid.filter(|pid| *pid > 1) {
        eprintln!("acos serve: owned by pid {owner}; exiting when it is gone");
    }
    if args.json {
        println!(
            "{}",
            json!({
                "socket": socket.display().to_string(),
                "idle_timeout_secs": args.idle_timeout,
                "max_sessions": args.max_sessions,
                "pid": std::process::id(),
                "engine": default_engine.as_str(),
                "art_artifacts": artifacts_root.display().to_string(),
                "owner_pid": args.owner_pid,
                "auto_started": args.auto_started,
                // REL3D: the identity every client compares against.
                "identity": engine.identity(),
            })
        );
    }
    eprintln!(
        "acos serve: listening on {} (engine={}, idle={}s, max_sessions={})",
        socket.display(),
        default_engine.as_str(),
        args.idle_timeout,
        args.max_sessions
    );

    // P8: the socket is bound and listening, so the daemon is ready exactly as
    // fast as before; the ART launch working set (boot image, ART libs,
    // classpath jars, guest dex, fonts) is read into the page cache on a
    // background thread, bounded and reclaimable, so the first launch after an
    // eviction does not pay P7's +198 ms. Never on the verification path.
    crate::prewarm::spawn_if_enabled(default_engine);

    // A `SIGTERM`/`SIGINT` (the MCP server tearing its private daemon down, or
    // an operator stopping it) must run the same shutdown as the idle exit:
    // close every ART guest and reap its process group, then remove the socket.
    //
    // SERVELEAK: a daemon that is terminated must take its guests with it.
    // Without this, `SIGTERM` (a bench teardown, a CI job, `kill`) ended the
    // process before `engine.shutdown()` ever ran and every live session's
    // guest (bwrap + dalvikvm64) was orphaned. The handlers only record the
    // request; the accept loop breaks and the same shutdown path runs, so a
    // guest is always destroyed *and* proven gone before the socket disappears.
    // This is the **one** signal mechanism: `SIGTERM`, `SIGINT` and `SIGHUP`
    // (SERVELEAK) all set [`SHUTDOWN`], which the loop tests below. (REL3a
    // installed a second, SIGTERM/SIGINT-only handler with its own
    // `TERMINATING` flag; it is folded into this one, keeping its
    // exit-at-the-top-of-the-iteration behaviour.)
    install_shutdown_signals();
    let idle = Duration::from_secs(args.idle_timeout.max(1));
    loop {
        if shutdown_requested() {
            eprintln!("acos serve: termination signal; shutting sessions down");
            break;
        }
        match listener.accept() {
            Ok((stream, _addr)) => {
                if !peer_is_self(&stream) {
                    eprintln!("acos serve: rejected connection from another user");
                    continue;
                }
                shared.active.fetch_add(1, Ordering::SeqCst);
                *shared.last_activity.lock().unwrap_or_else(|e| e.into_inner()) = Instant::now();
                let engine = Arc::clone(&engine);
                let shared = Arc::clone(&shared);
                std::thread::spawn(move || {
                    handle_connection(stream, &engine);
                    shared.active.fetch_sub(1, Ordering::SeqCst);
                    *shared.last_activity.lock().unwrap_or_else(|e| e.into_inner()) =
                        Instant::now();
                });
            }
            Err(e) if e.kind() == ErrorKind::WouldBlock => {
                // Block until a connection arrives. A connection wakes poll
                // immediately, so the timeout only bounds the idle-exit check
                // (seconds granularity). H4: 1 s, not 50 ms, removes a 20/s
                // idle timer from every idle serve; while a session is held
                // the check cannot fire anyway, so it waits the full second.
                let held = shared.active.load(Ordering::SeqCst) > 0
                    || engine.session_count() > 0;
                let idle_left = idle.saturating_sub(
                    shared
                        .last_activity
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .elapsed(),
                );
                let wait_ms = if held {
                    1_000
                } else {
                    idle_left.as_millis().clamp(1, 1_000) as i32
                };
                let mut pfd = libc::pollfd {
                    fd: listener.as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                };
                let rc = unsafe { libc::poll(&mut pfd, 1, wait_ms) };
                if rc < 0 && std::io::Error::last_os_error().kind() != ErrorKind::Interrupted {
                    eprintln!("acos serve: poll: {}", std::io::Error::last_os_error());
                    break;
                }
            }
            Err(e) => {
                eprintln!("acos serve: accept: {e}");
                break;
            }
        }
        // SERVELEAK: the process that spawned this daemon for its own use is
        // gone, so its sessions have no owner left: take them down and exit.
        if args
            .owner_pid
            .filter(|pid| *pid > 1)
            .is_some_and(|pid| !process_is_alive(pid))
        {
            eprintln!("acos serve: owner pid is gone; destroying sessions and exiting");
            break;
        }
        if shutdown_requested() {
            eprintln!(
                "acos serve: shutdown signal received; destroying {} session(s) first",
                engine.session_count()
            );
            break;
        }
        let idle_elapsed = shared
            .last_activity
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .elapsed();
        if shared.active.load(Ordering::SeqCst) == 0
            && engine.session_count() == 0
            && idle_elapsed >= idle
        {
            eprintln!("acos serve: idle for {idle:?}; exiting");
            break;
        }
    }
    // Daemon shutdown: close every ART guest and reap its process group before
    // the socket disappears. A session that cannot be proven gone is logged; the
    // `acos_destroy` reply is where a caller sees it as an error.
    engine.shutdown();
    let _ = std::fs::remove_file(&socket);
    ExitCode::SUCCESS
}

/// Is `pid` still alive (a zombie counts as gone: it cannot use the daemon)?
fn process_is_alive(pid: i32) -> bool {
    // SAFETY: signal 0 performs the existence/permission check only.
    if unsafe { libc::kill(pid, 0) } != 0 {
        return false;
    }
    match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        Ok(stat) => stat
            .rsplit_once(')')
            .map(|(_, rest)| !rest.trim_start().starts_with('Z'))
            .unwrap_or(true),
        Err(_) => false,
    }
}

/// A shutdown signal (SIGTERM/SIGINT/SIGHUP) was delivered.
static SHUTDOWN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn shutdown_requested() -> bool {
    SHUTDOWN.load(Ordering::SeqCst)
}

/// Record SIGTERM/SIGINT/SIGHUP as a shutdown request.
///
/// The handler only sets a flag (`async-signal-safe`): the accept loop exits and
/// the daemon runs its ordinary `engine.shutdown()` path, so the *guests* are
/// destroyed and proven gone, not just this process. `sa_flags` deliberately
/// omits `SA_RESTART` so the blocking `poll`/`accept` returns `EINTR` and the
/// loop re-checks the flag within a millisecond instead of after its timeout.
#[cfg(target_os = "linux")]
fn install_shutdown_signals() {
    extern "C" fn on_signal(_signal: libc::c_int) {
        SHUTDOWN.store(true, Ordering::SeqCst);
    }
    for signal in [libc::SIGTERM, libc::SIGINT, libc::SIGHUP] {
        // SAFETY: `sigaction` is given a zeroed struct with a plain function
        // pointer handler and an empty mask; the handler only touches an atomic.
        unsafe {
            let mut action: libc::sigaction = std::mem::zeroed();
            action.sa_sigaction = on_signal as usize;
            action.sa_flags = 0;
            libc::sigemptyset(&mut action.sa_mask);
            libc::sigaction(signal, &action, std::ptr::null_mut());
        }
    }
}

#[cfg(not(target_os = "linux"))]
fn install_shutdown_signals() {}

struct SharedState {
    last_activity: Mutex<Instant>,
    active: AtomicUsize,
}

fn handle_connection(stream: UnixStream, engine: &ServeEngine) {
    let mut writer = match stream.try_clone() {
        Ok(writer) => writer,
        Err(_) => return,
    };
    let mut reader = BufReader::new(stream);
    let mut last_write = m5_now_us();
    loop {
        let mut line = String::new();
        let n = match reader.read_line(&mut line) {
            Ok(n) => n,
            Err(_) => break,
        };
        if n == 0 {
            break;
        }
        if line.trim().is_empty() {
            continue;
        }
        let t_read = m5_now_us();
        m5_trace(
            "cli.serve_request_read",
            last_write,
            t_read,
            &format!("request_bytes={n}"),
        );
        let response = handle_request(&line, engine);
        let t_dispatch = m5_now_us();
        m5_trace(
            "cli.serve_dispatch",
            t_read,
            t_dispatch,
            &format!("request_bytes={n}"),
        );
        let mut payload = match serde_json::to_string(&response) {
            Ok(payload) => payload,
            Err(_) => continue,
        };
        payload.push('\n');
        if writer.write_all(payload.as_bytes()).is_err() {
            break;
        }
        let _ = writer.flush();
        last_write = m5_now_us();
        m5_trace(
            "cli.serve_reply_write",
            t_dispatch,
            last_write,
            &format!("reply_bytes={}", payload.len()),
        );
    }
}

fn handle_request(line: &str, engine: &ServeEngine) -> Value {
    let request: Value = match serde_json::from_str(line) {
        Ok(request) => request,
        Err(e) => {
            return json!({"id": Value::Null, "error": {"code": "INVALID_ARGUMENT", "message": format!("malformed request: {e}")}});
        }
    };
    let id = request.get("id").cloned().unwrap_or(Value::Null);
    let tool = request.get("tool").and_then(Value::as_str).unwrap_or("");
    let args = request
        .get("args")
        .cloned()
        .unwrap_or_else(|| json!({}));

    let ctx = context_for(&args);
    let envelope = engine.call(tool, &args, &ctx);
    json!({"id": id, "envelope": envelope})
}

fn context_for(args: &Value) -> acos_mcp::CallContext {
    let cwd = std::env::current_dir()
        .ok()
        .and_then(|p| p.canonicalize().ok())
        .unwrap_or_else(|| PathBuf::from("."));
    let mut allowed_roots = vec![cwd.clone()];
    if let Some(extra) = std::env::var_os("ACOS_ALLOWED_ROOTS") {
        for path in std::env::split_paths(&extra) {
            if let Ok(canonical) = path.canonicalize() {
                allowed_roots.push(canonical);
            }
        }
    }
    // The daemon is peer-credential checked, so trusting the APK's own directory
    // (the operation the caller explicitly asked for) is safe and keeps relative
    // and symlinked fixture paths working.
    if let Some(apk) = args.get("apk").and_then(Value::as_str) {
        if let Some(parent) = std::path::Path::new(apk).parent() {
            if let Ok(canonical) = parent.canonicalize() {
                allowed_roots.push(canonical);
            }
        }
    }
    acos_mcp::CallContext {
        cancel: acos_mcp::CancelToken::new(),
        deadline: Instant::now() + Duration::from_secs(600),
        budget: ResourceBudget::default(),
        allowed_roots,
        artifacts_dir: cwd,
        max_arg_chars: 8 * 1024,
        max_output_bytes: 16 * 1024 * 1024,
    }
}

/// Create `dir` (and parents) and force it to `0700`.
///
/// FB5: the **default** runtime directory is verified rather than repaired — the
/// `/tmp/acos-<uid>` fallback (no `XDG_RUNTIME_DIR`, no `TMPDIR`) must not be a
/// directory another user owns or can read, so `ensure_private_dir` refuses
/// instead of chmod-ing someone else's path. An explicit `--socket` parent is
/// still created 0700 as before (the caller chose it).
fn prepare_runtime_dir(dir: &std::path::Path) -> std::io::Result<()> {
    if dir == daemon::runtime_dir() {
        return daemon::ensure_private_dir(dir);
    }
    std::fs::create_dir_all(dir)?;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
}

/// `SO_PEERCRED` check: only the daemon's own user may talk to it.
fn peer_is_self(stream: &UnixStream) -> bool {
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::io::AsRawFd;
        let mut cred: libc::ucred = unsafe { std::mem::zeroed() };
        let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
        let rc = unsafe {
            libc::getsockopt(
                stream.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_PEERCRED,
                &mut cred as *mut libc::ucred as *mut libc::c_void,
                &mut len,
            )
        };
        rc == 0 && cred.uid == unsafe { libc::geteuid() }
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = stream;
        true
    }
}

/// Whether daemon use (auto-start + routing) is enabled.
pub fn daemon_enabled() -> bool {
    !matches!(
        std::env::var("ACOS_NO_DAEMON")
            .unwrap_or_default()
            .to_ascii_lowercase()
            .as_str(),
        "1" | "true" | "yes" | "on"
    )
}

/// What the shared socket holds right now (REL3D).
///
/// The shared daemon is `$ACOS_RUNTIME_DIR/session.sock` (or
/// `$ACOS_SESSION_SOCKET`): one per user, outliving the client that started it.
/// That is the socket MODA11Y found answering a newer client with an older
/// build's behaviour, so every client asks it for its identity first.
enum Shared {
    /// Nothing is listening (or the socket file is stale).
    Absent,
    /// A daemon of this build: use it.
    Compatible(DaemonClient),
    /// A daemon of another build: never use it. `idle` is its live session count
    /// when it could be read.
    Mismatched {
        mismatch: acos_mcp::identity::Mismatch,
        client: DaemonClient,
        idle: Option<usize>,
    },
}

/// The build-private socket for this client's build.
///
/// `$ACOS_RUNTIME_DIR/build-<build_id>/session.sock` (next to an explicit
/// `ACOS_SESSION_SOCKET` when that is set). A client that may not use the shared
/// daemon runs its own, and a later client of the **same build** finds it
/// again — so a build never shares a daemon with another build, and never
/// silently substitutes one.
pub fn private_socket_path() -> PathBuf {
    let build = acos_mcp::identity::Identity::local()
        .build_id
        .replace('+', "_")
        .replace(['/', '.'], "_");
    if let Some(explicit) = std::env::var_os(daemon::SOCKET_ENV) {
        let socket = PathBuf::from(explicit);
        let parent = socket
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));
        return parent.join(format!("build-{build}.sock"));
    }
    daemon::runtime_dir()
        .join(format!("build-{build}"))
        .join(daemon::SOCKET_FILE)
}

/// Probe the shared socket without ever starting anything.
fn probe_shared() -> Shared {
    let path = daemon::socket_path();
    match DaemonClient::connect(&path) {
        Ok(mut client) => match client.mismatch() {
            None => Shared::Compatible(client),
            Some(mismatch) => {
                let idle = daemon_session_count(&mut client);
                Shared::Mismatched {
                    mismatch,
                    client,
                    idle,
                }
            }
        },
        Err(_) => Shared::Absent,
    }
}

/// A daemon's live session count (`hello` first, `session_list` for a daemon too
/// old to report one). `None` when neither answers: an unknown daemon is never
/// assumed idle.
///
/// The count comes from the connection already open, and `session_list` goes
/// through [`DaemonClient::probe_call`]: a refused daemon is asked *only* this,
/// and never a session verb.
fn daemon_session_count(client: &mut DaemonClient) -> Option<usize> {
    if let Some(count) = client.sessions_hint() {
        return Some(count);
    }
    match client.probe_call(daemon::TOOL_LIST, &serde_json::json!({})) {
        Ok(envelope) if envelope.ok() => envelope
            .result()
            .and_then(|result| result.get("sessions"))
            .and_then(Value::as_array)
            .map(Vec::len),
        _ => None,
    }
}

/// Whether an incompatible daemon may be replaced (never one that is working for
/// somebody else).
///
/// Replaceable means: same user (the runtime dir is 0700 and peer-credential
/// checked), **no live session**, and either it says it was auto-started by a
/// client or — for a daemon too old to say — the client that started it is gone
/// (`ppid == 1`, so no live owner exists any more). An operator's `acos serve`
/// is never signalled, and a daemon with sessions is never signalled.
fn replaceable(client: &DaemonClient, idle: Option<usize>) -> bool {
    if idle != Some(0) {
        return false;
    }
    let pid = client.daemon_identity().pid();
    let auto_started = client
        .daemon_identity()
        .known()
        .map(|identity| identity.auto_started);
    match auto_started {
        Some(true) => true,
        Some(false) => false,
        // An older daemon: it cannot say. Only an orphan (its spawning client
        // exited) is provably unowned.
        None => pid.is_some_and(process_is_orphan),
    }
}

/// Is `pid`'s parent gone (`ppid == 1`), i.e. nobody who started it is left?
fn process_is_orphan(pid: u32) -> bool {
    let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
        return false;
    };
    let Some((_, rest)) = stat.rsplit_once(')') else {
        return false;
    };
    // Fields after the command name: state ppid ...
    let mut fields = rest.split_whitespace();
    let _state = fields.next();
    fields
        .next()
        .and_then(|ppid| ppid.parse::<i32>().ok())
        .is_some_and(|ppid| ppid == 1)
}

/// Emit the structured `DAEMON_VERSION_MISMATCH` warning (once per process).
///
/// stderr, one JSON object, so a CLI caller that reads stdout JSON still sees a
/// machine-readable warning and never has to parse prose. The daemon's answer is
/// never used, so this is a warning about the *decision*, not a failure of the
/// request that follows (which runs on this build's code).
fn warn_mismatch(mismatch: &acos_mcp::identity::Mismatch, action: &str) {
    use std::sync::atomic::AtomicBool;
    static WARNED: AtomicBool = AtomicBool::new(false);
    if WARNED.swap(true, Ordering::SeqCst) {
        return;
    }
    let mut warning = mismatch.to_json();
    if let Some(object) = warning.as_object_mut() {
        object.insert("action".to_string(), Value::String(action.to_string()));
        object.insert(
            "socket".to_string(),
            Value::String(daemon::socket_path().display().to_string()),
        );
    }
    eprintln!("{warning}");
}

/// Connect to a build-private daemon if one is already running.
fn connect_private() -> Option<DaemonClient> {
    let client = DaemonClient::connect(&private_socket_path()).ok()?;
    client.compatible().then_some(client)
}

/// Connect to a running daemon if there is one and daemon use is enabled.
///
/// Top-level commands never start a daemon. A daemon of another build is never
/// used: an already-running build-private daemon answers instead, and otherwise
/// the caller falls back to this build's in-process session (never a stale
/// verdict).
pub fn try_daemon() -> Option<DaemonClient> {
    if !daemon_enabled() {
        return None;
    }
    match probe_shared() {
        Shared::Compatible(client) => Some(client),
        Shared::Absent => connect_private(),
        Shared::Mismatched { mismatch, .. } => {
            let private = connect_private();
            warn_mismatch(
                &mismatch,
                if private.is_some() {
                    "build-private daemon (already running)"
                } else {
                    "in-process session of this build"
                },
            );
            private
        }
    }
}

/// Connect to a running daemon, starting one detached if necessary.
///
/// REL3D: never to a daemon of another build. An idle auto-started daemon of a
/// different build is replaced (SIGTERM, then a matching one on the shared
/// socket); a daemon holding sessions, or one an operator started, is left alone
/// and this client starts its **own** daemon on [`private_socket_path`], with a
/// structured `DAEMON_VERSION_MISMATCH` warning naming both identities.
pub fn ensure_daemon() -> Result<DaemonClient, String> {
    if !daemon_enabled() {
        return Err("session daemon disabled by ACOS_NO_DAEMON".to_string());
    }
    match probe_shared() {
        Shared::Compatible(client) => return Ok(client),
        Shared::Mismatched {
            mismatch,
            client,
            idle,
        } => {
            if replaceable(&client, idle) {
                let pid = client.daemon_identity().pid();
                drop(client);
                stop_daemon(pid);
                // Start a matching daemon on the shared socket.
                match start_daemon(&daemon::socket_path(), true) {
                    Ok(fresh) if fresh.compatible() => {
                        warn_mismatch(&mismatch, "replaced an idle daemon of another build");
                        return Ok(fresh);
                    }
                    Ok(_) => {
                        warn_mismatch(&mismatch, "build-private daemon");
                    }
                    Err(e) => {
                        // The replacement did not come up as ours (a racing old
                        // client re-started its own, a bind failure). Fall
                        // through to a private daemon: never wait on another
                        // build, and say why the shared socket was not taken.
                        eprintln!("acos serve: replacing the idle daemon failed: {e}");
                        warn_mismatch(&mismatch, "build-private daemon");
                    }
                }
            } else {
                warn_mismatch(&mismatch, "build-private daemon");
            }
        }
        Shared::Absent => {}
    }
    ensure_private_daemon()
}

/// Start (or reuse) this build's own daemon on [`private_socket_path`].
fn ensure_private_daemon() -> Result<DaemonClient, String> {
    if let Some(client) = connect_private() {
        return Ok(client);
    }
    let socket = private_socket_path();
    if let Some(parent) = socket.parent() {
        prepare_runtime_dir(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    // A stale private socket (an old daemon of *this* build id that died badly)
    // is removed by `acos serve` itself; a live one answered above.
    start_daemon(&socket, true)
}

/// SIGTERM a daemon and wait until its socket stops answering.
fn stop_daemon(pid: Option<u32>) {
    let Some(pid) = pid.filter(|pid| *pid > 1) else {
        return;
    };
    // SAFETY: signal 0/TERM on a pid this user's shared daemon reported; the
    // caller established it is idle and auto-started (or orphaned).
    unsafe {
        libc::kill(pid as libc::pid_t, libc::SIGTERM);
    }
    for _ in 0..400 {
        if !process_is_alive(pid as i32) {
            return;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    eprintln!("acos serve: daemon pid {pid} did not exit after SIGTERM");
}

/// Spawn `acos serve` on `socket` detached and wait for a **matching** daemon.
fn start_daemon(socket: &Path, auto_started: bool) -> Result<DaemonClient, String> {
    let exe = std::env::current_exe().map_err(|e| format!("resolve acos binary: {e}"))?;
    let mut command = std::process::Command::new(exe);
    command.arg("serve").arg("--socket").arg(socket);
    if auto_started {
        // REL3D: an auto-started daemon may be replaced when idle; an operator's
        // `acos serve` never is.
        command.arg("--auto-started");
    }
    if let Some(idle) = std::env::var_os("ACOS_SESSION_IDLE_SECS") {
        command.arg("--idle-timeout").arg(idle);
    }
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    command
        .spawn()
        .map_err(|e| format!("start session daemon: {e}"))?;
    let mut last = String::from("session daemon did not become ready");
    for _ in 0..400 {
        match DaemonClient::connect(socket) {
            Ok(mut client) => match client.mismatch() {
                None => return Ok(client),
                Some(mismatch) => last = mismatch.message(),
            },
            Err(e) => last = e.to_string(),
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    Err(format!("session daemon on {}: {last}", socket.display()))
}

/// The binary re-exec'd as the confined native session child.
///
/// The `acos` binary itself serves `--confined-child` (the same MCP session
/// backend the `acos-mcp` confined child uses), so native sessions need no
/// second binary. `ACOS_MCP_BIN` still overrides it for the MCP server layout.
fn confined_child_binary() -> PathBuf {
    if let Some(path) = std::env::var_os("ACOS_MCP_BIN") {
        return PathBuf::from(path);
    }
    std::env::current_exe().unwrap_or_else(|_| PathBuf::from("acos"))
}

/// Parse `--confined-child [--rw DIR]... [--ro DIR]...` from the raw argv.
///
/// Returns `None` for a normal CLI invocation. Mirrors the `acos-mcp` confined
/// child so [`SessionBackend`] can re-exec either binary.
pub fn confined_child_profile() -> Option<acos_sandbox::ConfineProfile> {
    let args: Vec<String> = std::env::args().collect();
    if !args.iter().any(|a| a == "--confined-child") {
        return None;
    }
    let mut profile = acos_sandbox::ConfineProfile::default();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--rw" => {
                if let Some(path) = args.get(i + 1) {
                    profile.read_write_paths.push(PathBuf::from(path));
                }
                i += 2;
            }
            "--ro" => {
                if let Some(path) = args.get(i + 1) {
                    profile.read_only_paths.push(PathBuf::from(path));
                }
                i += 2;
            }
            // The extracted Flutter engine dir; the sandbox clears the
            // environment, so it must be re-exported through the profile.
            "--flutter-lib" => {
                if let Some(path) = args.get(i + 1) {
                    profile
                        .env
                        .push(("ACOS_FLUTTER_LIB_DIR".to_string(), path.clone()));
                }
                i += 2;
            }
            // The React Native Java driver DEX; re-exported because the
            // sandbox clears the environment.
            "--rn-dex" => {
                if let Some(path) = args.get(i + 1) {
                    profile
                        .env
                        .push(("ACOS_RN_DRIVER_DEX".to_string(), path.clone()));
                }
                i += 2;
            }
            "--as-bytes" => {
                if let Some(bytes) = args.get(i + 1).and_then(|v| v.parse::<u64>().ok()) {
                    profile.rlimits.address_space_bytes = Some(bytes);
                }
                i += 2;
            }
            _ => i += 1,
        }
    }
    if let Some(root) = profile.read_write_paths.first() {
        profile
            .env
            .push(("ACOS_SESSION_ROOT".to_string(), root.display().to_string()));
    }
    let roots = std::env::join_paths(profile.read_only_paths.iter())
        .map(|joined| joined.to_string_lossy().into_owned())
        .unwrap_or_default();
    if !roots.is_empty() {
        profile.env.push(("ACOS_ALLOWED_ROOTS".to_string(), roots));
    }
    Some(profile)
}

/// Confine and then serve the MCP session backend over stdio.
///
/// This is the child half of a confined native session: it must run before any
/// thread is created (seccomp is installed with `TSYNC`).
pub fn run_confined_child(profile: acos_sandbox::ConfineProfile) -> ExitCode {
    match acos_sandbox::confine(&profile) {
        Ok(report) => eprintln!("acos --confined-child: active={}", report.active),
        Err(e) => {
            eprintln!("acos --confined-child: refused: {e}");
            return ExitCode::from(2);
        }
    }
    let backend = SessionBackend::with_cli(
        CliBackend::new(BackendConfig::for_binary("/nonexistent/acos-confined-child")),
        SessionOptions::default(),
    );
    let server = std::sync::Arc::new(acos_mcp::Server::new(
        backend,
        acos_mcp::ServerConfig::from_env(),
    ));
    let reader = std::io::BufReader::new(std::io::stdin().lock());
    if let Err(e) = server.serve(reader, std::io::stdout()) {
        eprintln!("acos --confined-child: {e}");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}
