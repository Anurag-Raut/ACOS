//! SERVELEAK2/F3: a default session id belongs to one server, not to every agent.
//!
//! SERVELEAK made a stdio MCP server destroy the sessions it launched when its
//! stdin reaches EOF. The default session id was the shared constant `s_mcp`, so
//! two agents running an `acos-mcp` against the same session daemon addressed the
//! *same* held session: B's `acos_launch` replaced A's guest, and then A's EOF
//! destroyed B's live guest.
//!
//! This drives two real `Server` instances against one fake daemon that reports
//! a fresh **launch token** per launch (the daemon-side identity of the launch
//! that owns an id, see `ServeEngine`). Both servers launch with the default id;
//! the second launch replaces the first's token. A's EOF must then leave the
//! session alone (its token no longer matches) and B's EOF must destroy it.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use acos_mcp::session_backend::default_session_id;
use acos_mcp::{BackendConfig, CliBackend, Server, ServerConfig, SessionBackend};
use acos_protocol::{Backend as ProtocolBackend, Envelope, Fidelity, Metrics};
use acos_runtime::session::SessionOptions;
use serde_json::{json, Value};

/// A short scratch directory: a unix socket path must fit `SUN_LEN` (108 bytes),
/// so this deliberately does not live under the repo path.
fn scratch(name: &str) -> PathBuf {
    let dir = PathBuf::from(
        std::env::var_os("ACOS_TEST_SCRATCH").unwrap_or_else(|| std::env::temp_dir().into()),
    )
    .join(format!(
        "acos-sl2-{}-{}-{name}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

/// A fake session daemon that keeps a session table with launch tokens.
///
/// It accepts any number of connections (one per MCP server), renews a session's
/// token on every `acos_launch` — exactly like `ServeEngine::renew_launch_token`
/// — and reports the table through `session_list`.
fn fake_daemon(path: &Path) -> FakeDaemon {
    let listener = UnixListener::bind(path).expect("bind fake daemon");
    let sessions: Arc<Mutex<HashMap<String, String>>> = Arc::new(Mutex::new(HashMap::new()));
    let (tx, rx) = channel::<Value>();
    let (stop_tx, stop_rx) = channel::<()>();
    let table = Arc::clone(&sessions);
    let counter = Arc::new(std::sync::atomic::AtomicU64::new(0));
    std::thread::spawn(move || loop {
        if stop_rx.try_recv().is_ok() {
            return;
        }
        let Ok((stream, _)) = listener.accept() else {
            return;
        };
        let table = Arc::clone(&table);
        let counter = Arc::clone(&counter);
        let tx = tx.clone();
        std::thread::spawn(move || {
            let mut writer = stream.try_clone().expect("clone stream");
            let mut reader = BufReader::new(stream);
            loop {
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
                let mut result = json!({"server": "fake-daemon"});
                    match tool.as_str() {
                        "acos_launch" => {
                            let seq =
                                counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
                            let token = format!("tok-{seq}");
                            table
                                .lock()
                                .unwrap()
                                .insert(session.clone(), token.clone());
                            result = json!({
                                "session_id": session,
                                "session_token": token,
                                "launched": true,
                            });
                        }
                        // REL3D: the identity handshake every daemon answers.
                        "hello" => {
                            let held = table.lock().unwrap();
                            result = json!({
                                "identity": acos_mcp::identity::Identity::local(),
                                "engine": "art",
                                "sessions": held.len(),
                            });
                        }
                        "session_list" => {
                            let held = table.lock().unwrap();
                            let list: Vec<Value> = held
                                .iter()
                                .map(|(id, token)| {
                                    json!({"session_id": id, "session_token": token, "engine": "art"})
                                })
                                .collect();
                        result = json!({"sessions": list, "count": list.len()});
                    }
                    "acos_destroy" => {
                        let removed = table.lock().unwrap().remove(&session).is_some();
                        result = json!({"destroyed": removed, "already_absent": !removed});
                    }
                    _ => {}
                }
                let _ = tx.send(json!({"tool": tool, "session_id": session}));
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
    });
    FakeDaemon {
        sessions,
        calls: rx,
        stop: stop_tx,
    }
}

struct FakeDaemon {
    sessions: Arc<Mutex<HashMap<String, String>>>,
    calls: Receiver<Value>,
    stop: Sender<()>,
}

impl FakeDaemon {
    fn token_of(&self, id: &str) -> Option<String> {
        self.sessions.lock().unwrap().get(id).cloned()
    }

    /// Wait until `tool` is seen for `session`, or give up after `within`.
    fn expect_call(&self, tool: &str, session: &str, within: Duration) -> bool {
        let deadline = std::time::Instant::now() + within;
        while std::time::Instant::now() < deadline {
            match self.calls.recv_timeout(Duration::from_millis(50)) {
                Ok(value) => {
                    if value.get("tool").and_then(Value::as_str) == Some(tool)
                        && value.get("session_id").and_then(Value::as_str) == Some(session)
                    {
                        return true;
                    }
                }
                Err(_) => continue,
            }
        }
        false
    }

    /// Wait until `tool` is seen for any session (FB5b/H1: a listing is about the
    /// daemon, so it no longer carries the default session id).
    fn expect_tool(&self, tool: &str, within: Duration) -> bool {
        let deadline = std::time::Instant::now() + within;
        while std::time::Instant::now() < deadline {
            if let Ok(value) = self.calls.recv_timeout(Duration::from_millis(50)) {
                if value.get("tool").and_then(Value::as_str) == Some(tool) {
                    return true;
                }
            }
        }
        false
    }

    /// Every `acos_destroy` seen for `session` within `within`.
    fn count_destroy(&self, session: &str, within: Duration) -> usize {
        let deadline = std::time::Instant::now() + within;
        let mut seen = 0;
        while std::time::Instant::now() < deadline {
            match self.calls.recv_timeout(Duration::from_millis(50)) {
                Ok(value) => {
                    if value.get("tool").and_then(Value::as_str) == Some("acos_destroy")
                        && value.get("session_id").and_then(Value::as_str) == Some(session)
                    {
                        seen += 1;
                    }
                }
                Err(_) => continue,
            }
        }
        seen
    }
}

struct ConnectedServer {
    stdin: std::io::PipeWriter,
    stdout: BufReader<std::io::PipeReader>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl ConnectedServer {
    fn start(socket: &Path, dir: &Path) -> Self {
        let backend = SessionBackend::with_cli(
            CliBackend::new(BackendConfig::for_binary("/nonexistent/acos-mcp-test")),
            SessionOptions::default(),
        )
        .with_daemon_socket(socket.to_path_buf());
        let config = ServerConfig::for_roots(vec![dir.to_path_buf()], dir.join("artifacts"));
        let server = Arc::new(Server::new(backend, config));
        let (server_stdin, client_to_server) = std::io::pipe().expect("stdin pipe");
        let (client_stdout, server_to_client) = std::io::pipe().expect("stdout pipe");
        let handle = std::thread::spawn(move || {
            let reader = BufReader::new(server_stdin);
            let _ = server.serve(reader, server_to_client);
        });
        let mut connected = ConnectedServer {
            stdin: client_to_server,
            stdout: BufReader::new(client_stdout),
            handle: Some(handle),
        };
        connected.initialize();
        connected
    }

    fn initialize(&mut self) {
        let request = json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {"protocolVersion": "2025-06-18", "capabilities": {},
                       "clientInfo": {"name": "serveleak2-test", "version": "0"}},
        });
        writeln!(self.stdin, "{request}").expect("write initialize");
        self.stdin.flush().expect("flush initialize");
        let mut line = String::new();
        self.stdout.read_line(&mut line).expect("read initialize");
        assert!(
            serde_json::from_str::<Value>(&line)
                .expect("initialize reply")
                .get("result")
                .is_some(),
            "initialize must succeed: {line}"
        );
        writeln!(
            self.stdin,
            "{}",
            json!({"jsonrpc": "2.0", "method": "notifications/initialized", "params": {}})
        )
        .expect("write initialized");
        self.stdin.flush().expect("flush");
    }

    /// A launch that omits `base.session_id`, i.e. the server's default id.
    fn launch_default(&mut self, dir: &Path) {
        let launch = json!({
            "jsonrpc": "2.0", "id": 2, "method": "tools/call",
            "params": {"name": "acos_launch",
                       "arguments": {"apk": dir.join("fake.apk").display().to_string(),
                                     "base": {"request_id": "req_launch"}}},
        });
        writeln!(self.stdin, "{launch}").expect("write launch");
        self.stdin.flush().expect("flush launch");
        let mut line = String::new();
        self.stdout.read_line(&mut line).expect("read launch reply");
        assert!(
            serde_json::from_str::<Value>(&line)
                .expect("launch reply JSON")
                .get("result")
                .is_some(),
            "the launch must be answered by the daemon: {line}"
        );
    }

    /// Close stdin (the MCP transport) and wait for the server to finish.
    fn close(mut self) {
        let stdin = self.stdin;
        drop(stdin);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

#[test]
fn a_default_id_does_not_let_one_server_destroy_another_servers_session() {
    let dir = scratch("f3-default-id");
    let socket = dir.join("session.sock");
    let daemon = fake_daemon(&socket);
    // Both MCP servers use the default id (neither launch names one).
    let default_id = default_session_id().to_string();
    assert_ne!(
        default_id, "s_mcp",
        "the default session id must be private to the server instance, not the shared constant"
    );
    assert!(
        default_id.contains(&std::process::id().to_string()),
        "the default id names the process that owns it: {default_id}"
    );

    let mut server_a = ConnectedServer::start(&socket, &dir);
    server_a.launch_default(&dir);
    assert!(
        daemon.expect_call("acos_launch", &default_id, Duration::from_secs(10)),
        "server A's launch reaches the daemon under the default id"
    );
    let token_a = daemon.token_of(&default_id).expect("A's launch is held");
    assert_eq!(token_a, "tok-1", "the daemon issued the first launch token");

    // B launches the *same* id (a second server in one process, which is the
    // in-process collision the review described). Its launch replaces A's.
    let mut server_b = ConnectedServer::start(&socket, &dir);
    server_b.launch_default(&dir);
    assert!(
        daemon.expect_call("acos_launch", &default_id, Duration::from_secs(10)),
        "server B's launch reaches the daemon under the same default id"
    );
    let token_b = daemon.token_of(&default_id).expect("B's launch is held");
    assert_ne!(token_a, token_b, "a launch renews the session's token");

    // A's transport ends. Its session id is now B's guest: A must not destroy it.
    server_a.close();
    // A asks the daemon which launch owns the id (the token check) — and that
    // check is what keeps it from destroying B's guest.
    assert!(
        daemon.expect_tool("session_list", Duration::from_secs(10)),
        "A checks the daemon's current launch token before destroying"
    );
    assert_eq!(
        daemon.count_destroy(&default_id, Duration::from_millis(600)),
        0,
        "server A's stdin EOF must not destroy the guest server B launched"
    );
    assert_eq!(
        daemon.token_of(&default_id).as_deref(),
        Some(token_b.as_str()),
        "B's session is still held by the daemon after A exits"
    );

    // B's EOF still destroys its own session, i.e. SERVELEAK's guarantee holds.
    server_b.close();
    assert!(
        daemon.expect_call("acos_destroy", &default_id, Duration::from_secs(10)),
        "server B destroys the session it launched when its stdin reaches EOF"
    );
    assert_eq!(
        daemon.token_of(&default_id),
        None,
        "B's session is gone after its own shutdown"
    );
    let _ = daemon.stop.send(());
    let _ = std::fs::remove_dir_all(&dir);
}
