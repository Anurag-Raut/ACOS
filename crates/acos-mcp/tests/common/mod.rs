//! Shared helpers for the acos-mcp integration tests.
//!
//! Tests drive the server in-process over anonymous OS pipes. The server is
//! constructed around a `CliBackend`, so the tests exercise the real dispatcher,
//! gate, cancellation and envelope mapping without spawning the `acos-mcp`
//! binary (which the e2e test does).

#![allow(dead_code)]

use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use acos_mcp::{BackendConfig, CliBackend, Server, ServerConfig};
use serde_json::{json, Value};

/// Repository root (`.../acos`).
pub fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crates/")
        .parent()
        .expect("repo root")
        .to_path_buf()
}

/// The built `acos` CLI, preferring the release profile.
pub fn acos_bin() -> PathBuf {
    let root = repo_root();
    for profile in ["release", "debug"] {
        let candidate = root.join("target").join(profile).join("acos");
        if candidate.is_file() {
            return candidate;
        }
    }
    panic!(
        "the `acos` binary is missing; build it with `cargo build --release -p acos-cli` (looked under {})",
        root.display()
    );
}

/// Make a test fixture executable (the checkout normally preserves the bit).
pub fn ensure_executable(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(metadata) = std::fs::metadata(path) {
            let mut permissions = metadata.permissions();
            permissions.set_mode(0o755);
            let _ = std::fs::set_permissions(path, permissions);
        }
    }
}

/// A backend pointed at an arbitrary executable, with extra env.
pub fn backend_for(binary: &Path, extra_env: Vec<(String, String)>) -> CliBackend {
    let mut config = BackendConfig::for_binary(binary);
    config.extra_env = extra_env;
    CliBackend::new(config)
}

/// Build a CI-appropriate server config rooted at the repo.
pub fn test_config() -> ServerConfig {
    let root = repo_root();
    ServerConfig::for_roots(vec![root.clone()], root.join("target/acos-mcp-artifacts"))
}

/// A client speaking MCP to an in-process server over pipes.
pub struct McpClient {
    stdin: std::io::PipeWriter,
    stdout: BufReader<std::io::PipeReader>,
    next_id: i64,
}

impl McpClient {
    /// Start an in-process server and return a connected client.
    pub fn start<B: acos_mcp::Backend + 'static>(backend: B, config: ServerConfig) -> McpClient {
        let server = Arc::new(Server::new(backend, config));
        let (server_stdin, client_stdin) = std::io::pipe().expect("pipe");
        let (client_stdout, server_stdout) = std::io::pipe().expect("pipe");
        std::thread::spawn(move || {
            let reader = BufReader::new(server_stdin);
            let _ = server.serve(reader, server_stdout);
        });
        McpClient {
            stdin: client_stdin,
            stdout: BufReader::new(client_stdout),
            next_id: 0,
        }
    }

    /// Start a server for the real `acos` binary.
    pub fn with_real_acos() -> McpClient {
        Self::start(backend_for(&acos_bin(), Vec::new()), test_config())
    }

    /// Write a raw line to the server (used for malformed-input tests).
    pub fn send_raw(&mut self, raw: &str) {
        self.stdin.write_all(raw.as_bytes()).expect("write raw");
        self.stdin.write_all(b"\n").expect("write newline");
        self.stdin.flush().expect("flush");
    }

    /// Write a JSON value as one frame.
    pub fn send_value(&mut self, value: &Value) {
        let line = serde_json::to_string(value).expect("serialize");
        self.send_raw(&line);
    }

    /// Send a request with an explicit id.
    pub fn send_request(&mut self, id: i64, method: &str, params: Value) {
        self.send_value(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        }));
    }

    /// Send a notification.
    pub fn send_notification(&mut self, method: &str, params: Value) {
        self.send_value(&json!({"jsonrpc": "2.0", "method": method, "params": params}));
    }

    /// Read one frame.
    pub fn recv(&mut self) -> Value {
        let mut line = String::new();
        let n = self.stdout.read_line(&mut line).expect("read response");
        assert!(n > 0, "server closed stdout before responding");
        serde_json::from_str(&line).unwrap_or_else(|e| panic!("bad JSON response {line:?}: {e}"))
    }

    /// Read frames until one has the requested id (drops notifications).
    pub fn recv_for(&mut self, id: i64) -> Value {
        for _ in 0..16 {
            let value = self.recv();
            if value.get("id").and_then(Value::as_i64) == Some(id) {
                return value;
            }
        }
        panic!("no response for id {id}");
    }

    /// Send a request with an auto-assigned id and read its response.
    pub fn request(&mut self, method: &str, params: Value) -> Value {
        self.next_id += 1;
        let id = self.next_id;
        self.send_request(id, method, params);
        self.recv_for(id)
    }

    /// Perform the `initialize` handshake.
    pub fn initialize(&mut self, protocol_version: &str) -> Value {
        let response = self.request(
            "initialize",
            json!({
                "protocolVersion": protocol_version,
                "capabilities": {},
                "clientInfo": {"name": "acos-mcp-test", "version": "0"},
            }),
        );
        self.send_notification("notifications/initialized", json!({}));
        response
    }

    /// Call a tool and read its response.
    pub fn call_tool(&mut self, name: &str, arguments: Value) -> Value {
        self.request("tools/call", json!({"name": name, "arguments": arguments}))
    }
}

/// Extract the ACOS envelope from a `tools/call` response, whichever protocol
/// revision was negotiated.
pub fn envelope_of(response: &Value) -> Value {
    if let Some(structured) = response.pointer("/result/structuredContent") {
        return structured.clone();
    }
    let text = response
        .pointer("/result/content/0/text")
        .and_then(Value::as_str)
        .expect("text content");
    serde_json::from_str(text).expect("envelope JSON")
}

/// Whether a `tools/call` response is marked as an error.
pub fn is_error(response: &Value) -> bool {
    response
        .pointer("/result/isError")
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

/// The tool names in a `tools/list` response.
pub fn tool_names(list: &Value) -> Vec<String> {
    list.pointer("/result/tools")
        .and_then(Value::as_array)
        .expect("tools array")
        .iter()
        .filter_map(|tool| tool.get("name").and_then(Value::as_str).map(str::to_string))
        .collect()
}

/// Find one tool descriptor by name.
pub fn tool<'a>(list: &'a Value, name: &str) -> Option<&'a Value> {
    list.pointer("/result/tools")?
        .as_array()?
        .iter()
        .find(|tool| tool.get("name").and_then(Value::as_str) == Some(name))
}

/// Read a fixture file as a string.
pub fn fixture(path: &str) -> String {
    std::fs::read_to_string(repo_root().join(path))
        .unwrap_or_else(|e| panic!("read fixture {path}: {e}"))
}

/// The build command that produces a gitignored `fixtures/build/...` path.
fn build_hint(path: &str) -> &'static str {
    if path.starts_with("fixtures/build/apps/") {
        "tools/build-apps.sh"
    } else if path.starts_with("fixtures/build/dexprogs/") {
        "tools/build-fixtures.sh"
    } else {
        "the fixture build scripts"
    }
}

/// Absolute path for a repo-relative fixture.
///
/// Gitignored build prerequisites (`fixtures/build/...`) are checked here so a
/// fresh worktree fails loudly with the exact build command instead of a
/// confusing downstream error — the F-11 friction from W81-A.
pub fn require_fixture(path: &str) -> std::path::PathBuf {
    let full = repo_root().join(path);
    if path.starts_with("fixtures/build/") && !full.exists() {
        panic!(
            "build fixtures first: `{}` (missing {})",
            build_hint(path),
            full.display()
        );
    }
    full
}

/// Absolute path string for a repo-relative fixture.
pub fn fixture_path(path: &str) -> String {
    require_fixture(path).display().to_string()
}

/// Read the whole of a reader (used by the e2e helper).
pub fn read_available<R: Read>(reader: &mut R) -> String {
    let mut buf = String::new();
    let _ = reader.read_to_string(&mut buf);
    buf
}
