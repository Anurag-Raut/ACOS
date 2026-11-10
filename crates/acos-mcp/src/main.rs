//! Entry point for the `acos-mcp` stdio server.
//!
//! stdout carries JSON-RPC frames only; all logging goes to stderr. Set
//! `ACOS_MCP_QUIET=1` to silence startup logging.

use std::io::{self, BufReader};
use std::sync::Arc;

use acos_mcp::{Backend, BackendConfig, CliBackend, Server, ServerConfig};

fn main() {
    let backend_config = BackendConfig::discover();
    let server_config = ServerConfig::from_env();

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

    let backend = CliBackend::new(backend_config);
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

    let server = Arc::new(Server::new(backend, server_config));
    let reader = BufReader::new(io::stdin().lock());
    if let Err(error) = server.serve(reader, io::stdout()) {
        log(&format!("fatal: {error}"));
        std::process::exit(1);
    }
}

fn log(message: &str) {
    if std::env::var_os("ACOS_MCP_QUIET").is_some() {
        return;
    }
    eprintln!("acos-mcp: {message}");
}
