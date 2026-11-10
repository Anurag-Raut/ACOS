//! Entry point for the `acos-mcp` stdio server.
//!
//! stdout carries JSON-RPC frames only; all logging goes to stderr. Set
//! `ACOS_MCP_QUIET=1` to silence startup logging.
//!
//! # Confined native sessions
//!
//! The server is multi-threaded, so it cannot enter the ADR-0022 seccomp
//! confinement itself (`TSYNC` requires a single-threaded process) and it must
//! not confine the client's process mid-flight. When `acos_launch { native:
//! true }` is requested, [`acos_mcp::SessionBackend`] re-execs this binary with
//! `--confined-child`; the child confines itself **before** starting the server
//! and then serves the session commands over the same MCP stdio protocol. The
//! parent proxies tool calls to it.

use std::io::{self, BufReader};
use std::path::PathBuf;
use std::sync::Arc;

use acos_mcp::{Backend, BackendConfig, CliBackend, Server, ServerConfig, SessionBackend};
use acos_runtime::session::SessionOptions;
use acos_sandbox::{confine, ConfineProfile};

fn main() {
    if let Some(profile) = confined_child_profile() {
        // Must run before any thread is created: seccomp is installed with
        // TSYNC and requires a single-threaded process.
        match confine(&profile) {
            Ok(report) => log(&format!("confined child active={}", report.active)),
            Err(e) => {
                eprintln!("acos-mcp: confined child refused: {e}");
                std::process::exit(2);
            }
        }
        // `exec` is denied by seccomp, so do not probe the CLI from here; the
        // child only ever serves in-process session tools.
        let backend = SessionBackend::with_cli(
            CliBackend::new(BackendConfig::for_binary("/nonexistent/acos-confined-child")),
            SessionOptions::default(),
        );
        serve(backend);
        return;
    }

    let backend_config = BackendConfig::discover();

    log(&format!(
        "acos-mcp {} starting (protocol {})",
        acos_mcp::SERVER_VERSION,
        acos_mcp::LATEST_PROTOCOL_VERSION
    ));
    log(&format!(
        "acos backend: {}",
        backend_config
            .binary
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "<not found>".to_string())
    ));

    let mut backend = SessionBackend::new(backend_config, SessionOptions::default());
    if acos_mcp::daemon::mcp_daemon_requested() {
        let socket = acos_mcp::daemon::socket_path();
        if socket.exists() {
            backend = backend.with_daemon();
            log(&format!(
                "session daemon: forwarding session tools to {}",
                socket.display()
            ));
        } else {
            log(&format!(
                "session daemon: {} not found; holding sessions in-process",
                socket.display()
            ));
        }
    }
    let caps = backend.capabilities();
    let available: Vec<&str> = acos_mcp::descriptors::all_commands()
        .iter()
        .filter(|c| caps.is_supported(**c))
        .map(|c| c.as_str())
        .collect();
    log(&format!(
        "capability probe: {} command(s) available [{}]",
        available.len(),
        available.join(", ")
    ));
    if let Some(error) = &caps.probe_error {
        log(&format!("capability probe note: {error}"));
    }

    serve(backend);
}

fn serve<B: Backend + 'static>(backend: B) {
    let server = Arc::new(Server::new(backend, ServerConfig::from_env()));
    let reader = BufReader::new(io::stdin().lock());
    if let Err(error) = server.serve(reader, io::stdout()) {
        log(&format!("fatal: {error}"));
        std::process::exit(1);
    }
}

/// Parse `--confined-child [--rw DIR]... [--ro DIR]...`. Returns `None` for a
/// normal server invocation.
fn confined_child_profile() -> Option<ConfineProfile> {
    let args: Vec<String> = std::env::args().collect();
    if !args.iter().any(|a| a == "--confined-child") {
        return None;
    }
    let mut profile = ConfineProfile::default();
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
    // Keep the chosen VFS root visible to the runtime across `clearenv`.
    if let Some(root) = profile.read_write_paths.first() {
        profile
            .env
            .push(("ACOS_SESSION_ROOT".to_string(), root.display().to_string()));
    }
    // Path validation in the child must accept the APK and boot cache the
    // parent granted read access to; they are exactly the read-only roots.
    let roots = std::env::join_paths(profile.read_only_paths.iter())
        .map(|joined| joined.to_string_lossy().into_owned())
        .unwrap_or_default();
    if !roots.is_empty() {
        profile
            .env
            .push(("ACOS_ALLOWED_ROOTS".to_string(), roots));
    }
    Some(profile)
}

fn log(message: &str) {
    if std::env::var_os("ACOS_MCP_QUIET").is_some() {
        return;
    }
    eprintln!("acos-mcp: {message}");
}
