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

use std::io::{BufRead, BufReader, ErrorKind, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use acos_mcp::daemon::{self, DaemonClient};
use acos_mcp::{BackendConfig, CliBackend, SessionBackend};
use acos_protocol::{
    Backend as ProtocolBackend, Envelope, ErrorCode, Fidelity, Metrics, ProtocolError,
    ResourceBudget,
};
use acos_runtime::session::SessionOptions;
use serde_json::{json, Value};

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

    /// Emit a JSON startup banner on stdout and do not detach.
    #[arg(long)]
    pub json: bool,
}

/// Run `acos serve`.
pub fn run(args: ServeArgs) -> ExitCode {
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
    let shared = Arc::new(SharedState {
        last_activity: Mutex::new(Instant::now()),
        active: AtomicUsize::new(0),
    });

    if args.json {
        println!(
            "{}",
            json!({
                "socket": socket.display().to_string(),
                "idle_timeout_secs": args.idle_timeout,
                "max_sessions": args.max_sessions,
                "pid": std::process::id(),
            })
        );
    }
    eprintln!(
        "acos serve: listening on {} (idle={}s, max_sessions={})",
        socket.display(),
        args.idle_timeout,
        args.max_sessions
    );

    let idle = Duration::from_secs(args.idle_timeout.max(1));
    let max_sessions = args.max_sessions.max(1);
    loop {
        match listener.accept() {
            Ok((stream, _addr)) => {
                if !peer_is_self(&stream) {
                    eprintln!("acos serve: rejected connection from another user");
                    continue;
                }
                shared.active.fetch_add(1, Ordering::SeqCst);
                *shared.last_activity.lock().unwrap_or_else(|e| e.into_inner()) = Instant::now();
                let backend = Arc::clone(&backend);
                let shared = Arc::clone(&shared);
                std::thread::spawn(move || {
                    handle_connection(stream, &backend, max_sessions);
                    shared.active.fetch_sub(1, Ordering::SeqCst);
                    *shared.last_activity.lock().unwrap_or_else(|e| e.into_inner()) =
                        Instant::now();
                });
            }
            Err(e) if e.kind() == ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => {
                eprintln!("acos serve: accept: {e}");
                break;
            }
        }
        let idle_elapsed = shared
            .last_activity
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .elapsed();
        if shared.active.load(Ordering::SeqCst) == 0
            && backend.session_count() == 0
            && idle_elapsed >= idle
        {
            eprintln!("acos serve: idle for {idle:?}; exiting");
            break;
        }
    }
    let _ = std::fs::remove_file(&socket);
    ExitCode::SUCCESS
}

struct SharedState {
    last_activity: Mutex<Instant>,
    active: AtomicUsize,
}

fn handle_connection(stream: UnixStream, backend: &SessionBackend, max_sessions: usize) {
    let mut writer = match stream.try_clone() {
        Ok(writer) => writer,
        Err(_) => return,
    };
    let mut reader = BufReader::new(stream);
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
        let response = handle_request(&line, backend, max_sessions);
        let mut payload = match serde_json::to_string(&response) {
            Ok(payload) => payload,
            Err(_) => continue,
        };
        payload.push('\n');
        if writer.write_all(payload.as_bytes()).is_err() {
            break;
        }
        let _ = writer.flush();
    }
}

fn handle_request(line: &str, backend: &SessionBackend, max_sessions: usize) -> Value {
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

    if tool == "acos_launch" && backend.session_count() >= max_sessions {
        let envelope = Envelope::failure(
            request_id(&args),
            ProtocolBackend::AcosNative,
            Fidelity::NativeRuntime,
            ProtocolError::new(
                ErrorCode::ResourceLimit,
                format!("session cap reached ({max_sessions}); destroy a session first"),
            )
            .with_details(json!({"max_sessions": max_sessions})),
            Metrics::wall_ms(0),
        );
        return json!({"id": id, "envelope": envelope});
    }

    let envelope = dispatch(backend, tool, &args);
    json!({"id": id, "envelope": envelope})
}

fn dispatch(backend: &SessionBackend, tool: &str, args: &Value) -> Envelope {
    match tool {
        daemon::TOOL_PING => Envelope::pass(
            request_id(args),
            ProtocolBackend::AcosNative,
            Fidelity::NativeRuntime,
            Metrics::wall_ms(0),
        )
        .with_value_result(json!({"pong": true, "pid": std::process::id()})),
        daemon::TOOL_LIST => {
            let result = backend.list_sessions();
            Envelope::pass(
                request_id(args),
                ProtocolBackend::AcosNative,
                Fidelity::NativeRuntime,
                Metrics::wall_ms(0),
            )
            .with_value_result(result)
        }
        _ => {
            let ctx = context_for(args);
            backend.session_call(tool, args, &ctx).unwrap_or_else(|| {
                Envelope::failure(
                    request_id(args),
                    ProtocolBackend::AcosNative,
                    Fidelity::NativeRuntime,
                    ProtocolError::new(
                        ErrorCode::UnsupportedBackend,
                        format!("unknown session tool `{tool}`"),
                    )
                    .with_details(json!({"tool": tool})),
                    Metrics::wall_ms(0),
                )
            })
        }
    }
}

fn request_id(args: &Value) -> String {
    args.pointer("/base/request_id")
        .and_then(Value::as_str)
        .unwrap_or("req_daemon")
        .to_string()
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
fn prepare_runtime_dir(dir: &std::path::Path) -> std::io::Result<()> {
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

/// Connect to a running daemon if there is one and daemon use is enabled.
pub fn try_daemon() -> Option<DaemonClient> {
    if !daemon_enabled() {
        return None;
    }
    DaemonClient::connect_default().ok()
}

/// Connect to a running daemon, starting one detached if necessary.
pub fn ensure_daemon() -> Result<DaemonClient, String> {
    if let Some(client) = try_daemon() {
        return Ok(client);
    }
    let exe = std::env::current_exe().map_err(|e| format!("resolve acos binary: {e}"))?;
    let mut command = std::process::Command::new(exe);
    command.arg("serve");
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
    for _ in 0..400 {
        if let Some(client) = try_daemon() {
            return Ok(client);
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    Err("session daemon did not become ready".to_string())
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
