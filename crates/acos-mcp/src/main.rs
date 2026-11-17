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
            CliBackend::new(BackendConfig::for_binary(
                "/nonexistent/acos-confined-child",
            )),
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

    let acos_binary = backend_config.binary.clone();
    let mut backend = SessionBackend::new(backend_config, SessionOptions::default());
    // The default engine is real ART: the server lazily spawns its own
    // `acos serve --engine art` and forwards the session tools to it. The frozen
    // in-process backend is opt-in only (`ACOS_MCP_ENGINE=legacy`), and an
    // external daemon (`ACOS_SESSION_DAEMON=1`) still wins when it is listening.
    if acos_mcp::daemon::mcp_daemon_requested() && acos_mcp::daemon::socket_path().exists() {
        let socket = acos_mcp::daemon::socket_path();
        backend = backend.with_daemon();
        log(&format!(
            "session daemon: forwarding session tools to {}",
            socket.display()
        ));
    } else if acos_mcp::private_daemon::legacy_requested() {
        log("session engine: legacy (in-process Rust VM / RN bridge, FROZEN; ACOS_MCP_ENGINE=legacy)");
    } else {
        match acos_binary {
            Some(binary) => {
                log(&format!(
                    "session engine: art (private `{} serve --engine art` daemon on first session call)",
                    binary.display()
                ));
                backend = backend.with_art_daemon(binary);
            }
            None => log(
                "session engine: art requested but no `acos` binary was found;                  set ACOS_BIN (session tools will answer INTERNAL until then)",
            ),
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

    // stdin EOF is the MCP client exiting: tear the private daemon (and every
    // ART guest it holds) down before this process exits.
    //
    // FB5: a client that is *killed* never closes stdin, and a supervisor stops
    // the server with SIGTERM — neither reaches that path, so the same teardown
    // also runs from a `sigwait` thread. The backend is shared (`Arc`) so the
    // handler and the server are the same instance: exactly the sessions this
    // server launched are destroyed, and nothing a foreign daemon holds.
    let teardown = backend.art_daemon_handle();
    let backend = Arc::new(backend);
    if let Err(error) = acos_mcp::signals::install_teardown({
        let backend = Arc::clone(&backend);
        move || Backend::shutdown(&*backend)
    }) {
        log(&format!(
            "signal teardown unavailable ({error}); stdin EOF still tears down"
        ));
    }
    // FB5: the idle reaper (default 10 min; `ACOS_SESSION_IDLE_TTL_S` or
    // `acos_launch {idle_ttl_s}` overrides it) holds only a `Weak`.
    backend.start_idle_reaper();
    serve(backend);
    // FB5b/L3: a SIGTERM racing stdin EOF owns a teardown that may still be
    // running; returning now would end the process mid-teardown.
    acos_mcp::signals::join_teardown();
    teardown.shutdown();
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
        profile.env.push(("ACOS_ALLOWED_ROOTS".to_string(), roots));
    }
    Some(profile)
}

fn log(message: &str) {
    if std::env::var_os("ACOS_MCP_QUIET").is_some() {
        return;
    }
    eprintln!("acos-mcp: {message}");
}
