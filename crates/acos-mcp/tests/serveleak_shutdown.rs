//! SERVELEAK: the MCP server's sessions must not outlive its transport.
//!
//! A stdio MCP server's lifetime is its stdin. When stdin reaches EOF the client
//! is gone and nobody will ever address the sessions that server launched again,
//! so a live ART guest left in the session daemon is a pure leak. This test
//! drives a real `Server` over pipes against a fake session daemon and asserts
//! that stdin EOF destroys exactly the session the server launched (and nothing
//! else), through the same `acos_destroy` a caller would use.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::time::Duration;

use acos_mcp::{BackendConfig, CliBackend, Server, ServerConfig, SessionBackend};
use acos_protocol::{
    Backend as ProtocolBackend, Envelope, Fidelity, Metrics,
};
use acos_runtime::session::SessionOptions;
use serde_json::{json, Value};

/// A short scratch directory: a unix socket path must fit `SUN_LEN` (108 bytes),
/// so this deliberately does not live under the repo path.
fn scratch(name: &str) -> PathBuf {
    let dir = PathBuf::from(std::env::var_os("ACOS_TEST_SCRATCH").unwrap_or_else(|| {
        std::env::temp_dir().into()
    }))
    .join(format!(
        "acos-sl-{}-{}-{name}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

/// A fake `acos serve`: speaks the line-JSON daemon protocol and records every
/// tool call it is asked to perform.
fn fake_daemon(
    path: &std::path::Path,
) -> (Receiver<Value>, Sender<()>) {
    let listener = UnixListener::bind(path).expect("bind fake daemon");
    let (tx, rx) = channel::<Value>();
    let (stop_tx, stop_rx) = channel::<()>();
    // The session table a real `acos serve` keeps: `session_list` reports it and
    // teardown checks the launch token against it before destroying (FB5).
    let table: Arc<std::sync::Mutex<std::collections::HashMap<String, String>>> =
        Arc::new(std::sync::Mutex::new(std::collections::HashMap::new()));
    std::thread::spawn(move || {
        let Ok((stream, _)) = listener.accept() else {
            return;
        };
        let mut writer = stream.try_clone().expect("clone stream");
        let mut reader = BufReader::new(stream);
        loop {
            // A stop request is checked first so a wedged read cannot hang the
            // test process at exit.
            if stop_rx.try_recv().is_ok() {
                return;
            }
            let mut line = String::new();
            match reader.read_line(&mut line) {
                Ok(0) | Err(_) => return,
                Ok(_) => {}
            }
            if line.trim().is_empty() {
                continue;
            }
            let request: Value = serde_json::from_str(&line).expect("daemon request JSON");
            let id = request.get("id").cloned().unwrap_or(Value::Null);
            let tool = request
                .get("tool")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let args = request.get("args").cloned().unwrap_or_else(|| json!({}));
            let session = args
                .pointer("/base/session_id")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let _ = tx.send(json!({"tool": tool, "session_id": session}));
            let result = match tool.as_str() {
                "acos_launch" => {
                    let token = format!("tok-{session}");
                    table.lock().unwrap().insert(session.clone(), token.clone());
                    json!({"session_id": session, "session_token": token, "launched": true})
                }
                // REL3D: every daemon answers the identity handshake; a fake
                // that does not is (correctly) refused as another build.
                "hello" => {
                    let held = table.lock().unwrap();
                    json!({
                        "identity": acos_mcp::identity::Identity::local(),
                        "engine": "art",
                        "sessions": held.len(),
                    })
                }
                "session_list" => {
                    let held = table.lock().unwrap();
                    let list: Vec<Value> = held
                        .iter()
                        .map(|(id, token)| json!({"session_id": id, "session_token": token}))
                        .collect();
                    json!({"sessions": list, "count": list.len()})
                }
                "acos_destroy" => {
                    let removed = table.lock().unwrap().remove(&session).is_some();
                    json!({"destroyed": removed, "already_absent": !removed})
                }
                _ => json!({"server": "fake-daemon"}),
            };
            let envelope = Envelope::pass(
                "req_daemon",
                ProtocolBackend::AcosNative,
                Fidelity::ArtReference,
                Metrics::wall_ms(0),
            )
            .with_value_result(result);
            let reply = json!({"id": id, "envelope": envelope});
            let mut payload = serde_json::to_string(&reply).expect("serialize reply");
            payload.push('\n');
            if writer.write_all(payload.as_bytes()).is_err() {
                return;
            }
            let _ = writer.flush();
        }
    });
    (rx, stop_tx)
}

fn drain_until(rx: &Receiver<Value>, tool: &str, within: Duration) -> Option<Value> {
    let deadline = std::time::Instant::now() + within;
    while std::time::Instant::now() < deadline {
        match rx.recv_timeout(Duration::from_millis(50)) {
            Ok(value) if value.get("tool").and_then(Value::as_str) == Some(tool) => {
                return Some(value)
            }
            Ok(_) => continue,
            Err(_) => continue,
        }
    }
    None
}

#[test]
fn stdin_eof_destroys_the_sessions_the_server_launched() {
    let dir = scratch("mcp-eof");
    let socket = dir.join("session.sock");
    let (rx, stop) = fake_daemon(&socket);

    let backend = SessionBackend::with_cli(
        CliBackend::new(BackendConfig::for_binary("/nonexistent/acos-mcp-test")),
        SessionOptions::default(),
    )
    .with_daemon_socket(socket.clone());
    let config = ServerConfig::for_roots(vec![dir.clone()], dir.join("artifacts"));
    let server = Arc::new(Server::new(backend, config));

    let (server_stdin, mut client_stdin) = std::io::pipe().expect("stdin pipe");
    let (client_stdout, server_stdout) = std::io::pipe().expect("stdout pipe");
    let handle = std::thread::spawn(move || {
        let reader = BufReader::new(server_stdin);
        let _ = server.serve(reader, server_stdout);
    });
    let mut stdout = BufReader::new(client_stdout);

    let request = json!({
        "jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": {"protocolVersion": "2025-06-18", "capabilities": {},
                   "clientInfo": {"name": "serveleak-test", "version": "0"}},
    });
    writeln!(client_stdin, "{request}").expect("write initialize");
    client_stdin.flush().expect("flush initialize");
    let mut line = String::new();
    stdout.read_line(&mut line).expect("read initialize reply");
    assert!(
        serde_json::from_str::<Value>(&line)
            .expect("initialize reply JSON")
            .get("result")
            .is_some(),
        "initialize must succeed: {line}"
    );
    writeln!(
        client_stdin,
        "{}",
        json!({"jsonrpc": "2.0", "method": "notifications/initialized", "params": {}})
    )
    .expect("write initialized");
    client_stdin.flush().expect("flush initialized");

    let launch = json!({
        "jsonrpc": "2.0", "id": 2, "method": "tools/call",
        "params": {"name": "acos_launch",
                   "arguments": {"apk": dir.join("fake.apk").display().to_string(),
                                 "base": {"request_id": "req_launch",
                                          "session_id": "s_mcp_leak"}}},
    });
    writeln!(client_stdin, "{launch}").expect("write launch");
    client_stdin.flush().expect("flush launch");
    let mut line = String::new();
    stdout.read_line(&mut line).expect("read launch reply");
    assert!(
        serde_json::from_str::<Value>(&line)
            .expect("launch reply JSON")
            .get("result")
            .is_some(),
        "the launch must be answered by the daemon: {line}"
    );

    let seen = drain_until(&rx, "acos_launch", Duration::from_secs(10))
        .expect("the daemon must receive the launch");
    assert_eq!(
        seen.get("session_id").and_then(Value::as_str),
        Some("s_mcp_leak")
    );

    // The client goes away: stdin EOF is the MCP server's shutdown.
    drop(client_stdin);
    let destroy = drain_until(&rx, "acos_destroy", Duration::from_secs(15))
        .expect("stdin EOF must destroy the session this server launched");
    assert_eq!(
        destroy.get("session_id").and_then(Value::as_str),
        Some("s_mcp_leak"),
        "exactly the launched session is destroyed: {destroy}"
    );
    let _ = handle.join();
    let _ = stop.send(());
    let _ = std::fs::remove_dir_all(&dir);
}
