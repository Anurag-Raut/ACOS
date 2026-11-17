//! Default engine: MCP session tools run on real ART, via the server's own
//! private `acos serve --engine art` daemon.
//!
//! These tests drive the real `acos-mcp` binary over stdio with a **fake**
//! `acos` (a small Python script) so they prove the wiring without an ART guest:
//!
//! * `ACOS_BIN` points at the fake, so the fake is what the server spawns;
//! * the fake records its argv, its socket and every forwarded call, and answers
//!   each with an ART-labelled envelope;
//! * the runtime dir is pinned to a scratch directory with
//!   `XDG_RUNTIME_DIR`, so the private socket path is asserted exactly.

mod common;

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

/// A scratch directory under the worktree's `.cache` (never `/tmp`).
fn scratch(name: &str) -> PathBuf {
    let dir = common::repo_root()
        .join(".cache/rel3a-tests")
        .join(format!("{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

/// Write the fake `acos`: a `serve` daemon plus a pass-through for other verbs.
fn fake_acos(dir: &Path) -> PathBuf {
    let path = dir.join("acos");
    let script = r#"#!/usr/bin/env python3
import json, os, socket, sys

log_path = os.environ.get("ACOS_FAKE_LOG", "")

def log(obj):
    if log_path:
        with open(log_path, "a") as fh:
            fh.write(json.dumps(obj) + "\n")

argv = sys.argv[1:]
log({"kind": "argv", "argv": argv, "pid": os.getpid()})
if not argv or argv[0] != "serve":
    if argv and argv[0] == "--help":
        # The capability probe parses the clap `Commands:` listing.
        print("Usage: acos <COMMAND>\n\nCommands:\n  verify   run an APK + scenario\n  inspect  static APK analysis")
        sys.exit(0)
    if argv and argv[0] == "--version":
        print("acos 0.2.0")
        sys.exit(0)
    print(json.dumps({
        "request_id": "req_fake", "ok": True, "status": "pass",
        "backend": "acos-native", "fidelity": "art-reference",
        "metrics": {"wall_ms": 0}, "result": {"fake": True},
    }))
    sys.exit(0)

socket_path = argv[argv.index("--socket") + 1]
engine = argv[argv.index("--engine") + 1] if "--engine" in argv else "none"
with open(os.environ["ACOS_FAKE_PID_FILE"], "w") as fh:
    fh.write(str(os.getpid()))

srv = socket.socket(socket.AF_UNIX)
srv.bind(socket_path)
srv.listen(8)
while True:
    conn, _ = srv.accept()
    stream = conn.makefile("rwb")
    for line in stream:
        line = line.strip()
        if not line:
            continue
        request = json.loads(line)
        args = request.get("args") or {}
        session_id = (args.get("base") or {}).get("session_id")
        log({"kind": "call", "tool": request.get("tool"),
             "session_id": session_id, "engine": engine,
             "socket": socket_path})
        envelope = {
            "request_id": "req_fake", "ok": True, "status": "pass",
            "backend": "acos-native", "fidelity": "art-reference",
            "session_id": session_id, "metrics": {"wall_ms": 0},
            "result": {"fake_session": session_id, "engine": engine},
        }
        stream.write((json.dumps({"id": request["id"], "envelope": envelope}) + "\n").encode())
        stream.flush()
"#;
    std::fs::write(&path, script).expect("write fake acos");
    common::ensure_executable(&path);
    path
}

fn log_lines(path: &Path) -> Vec<Value> {
    match std::fs::read_to_string(path) {
        Ok(text) => text
            .lines()
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect(),
        Err(_) => Vec::new(),
    }
}

struct Server {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: BufReader<ChildStdout>,
    next_id: i64,
}

impl Server {
    fn spawn(env: &[(&str, &Path)]) -> Server {
        let mut command = Command::new(env!("CARGO_BIN_EXE_acos-mcp"));
        command.env("ACOS_MCP_QUIET", "1");
        command.env_remove("ACOS_SESSION_DAEMON");
        command.env_remove("ACOS_MCP_ENGINE");
        // The private socket path must fit `sun_path` (108 bytes with the NUL)
        // and this test pins it to the short `XDG_RUNTIME_DIR` it made, so an
        // inherited `ACOS_RUNTIME_DIR` (a deep worktree scratch path in this
        // repo's harness) must not override it: the caller's directory length
        // is not this test's business.
        command.env_remove("ACOS_RUNTIME_DIR");
        for (key, value) in env {
            command.env(key, value);
        }
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn acos-mcp");
        let stdin = child.stdin.take().expect("stdin");
        let stdout = BufReader::new(child.stdout.take().expect("stdout"));
        Server {
            child,
            stdin: Some(stdin),
            stdout,
            next_id: 0,
        }
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        self.next_id += 1;
        let id = self.next_id;
        let line = serde_json::to_string(&json!({
            "jsonrpc": "2.0", "id": id, "method": method, "params": params
        }))
        .expect("serialize");
        {
            let stdin = self.stdin.as_mut().expect("stdin open");
            writeln!(stdin, "{line}").expect("write");
            stdin.flush().expect("flush");
        }
        loop {
            let mut response = String::new();
            let n = self.stdout.read_line(&mut response).expect("read");
            assert!(n > 0, "server closed stdout");
            let value: Value = serde_json::from_str(&response).expect("json");
            if value.get("id").and_then(Value::as_i64) == Some(id) {
                return value;
            }
        }
    }

    fn initialize(&mut self) {
        self.request(
            "initialize",
            json!({"protocolVersion": "2025-06-18", "capabilities": {},
                   "clientInfo": {"name": "rel3a-test", "version": "0"}}),
        );
    }

    fn tool(&mut self, name: &str, args: Value) -> Value {
        let response = self.request("tools/call", json!({"name": name, "arguments": args}));
        assert!(
            response.get("error").is_none(),
            "`{name}` returned a JSON-RPC error: {response}"
        );
        let text = response
            .pointer("/result/content/0/text")
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("`{name}` has no text content: {response}"));
        serde_json::from_str(text).unwrap_or_else(|e| panic!("`{name}` envelope: {e}: {text}"))
    }

    /// Close stdin (the MCP client exiting) and wait for the server.
    fn close(mut self) -> std::process::ExitStatus {
        drop(self.stdin.take());
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            if let Some(status) = self.child.try_wait().expect("try_wait") {
                return status;
            }
            assert!(
                Instant::now() < deadline,
                "acos-mcp did not exit on stdin EOF"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

fn pid_alive(pid: u32) -> bool {
    Path::new(&format!("/proc/{pid}")).exists()
}

/// A short runtime dir for the private socket (`sun_path` is 108 bytes, so the
/// worktree scratch path is far too deep). `XDG_RUNTIME_DIR` is normally
/// `/run/user/<uid>`; fall back to the temp dir on hosts without one.
fn short_runtime(name: &str) -> PathBuf {
    let uid = unsafe { libc::geteuid() };
    for base in [
        PathBuf::from(format!("/run/user/{uid}")),
        std::env::temp_dir(),
    ] {
        if base.is_dir() {
            let dir = base.join(format!("acos-rel3a-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            if std::fs::create_dir_all(&dir).is_ok() {
                return dir;
            }
        }
    }
    panic!("no short runtime directory available");
}

fn setup(name: &str) -> (PathBuf, PathBuf, PathBuf, PathBuf, PathBuf) {
    let dir = scratch(name);
    let runtime = short_runtime(name);
    std::fs::create_dir_all(runtime.join("acos")).expect("runtime");
    let fake = fake_acos(&dir);
    let log = dir.join("fake.log");
    let pid_file = dir.join("daemon.pid");
    let apk = dir.join("app.apk");
    std::fs::write(&apk, b"not really an apk").expect("apk");
    (dir, runtime, fake, log, pid_file)
}

#[test]
fn default_session_tools_use_the_private_art_daemon() {
    let (dir, runtime, fake, log, pid_file) = setup("default-art");
    let mut server = Server::spawn(&[
        ("ACOS_BIN", &fake),
        ("XDG_RUNTIME_DIR", &runtime),
        ("ACOS_ALLOWED_ROOTS", &dir),
        ("ACOS_FAKE_LOG", &log),
        ("ACOS_FAKE_PID_FILE", &pid_file),
    ]);
    server.initialize();

    // No engine argument, no daemon env: the default is ART.
    let envelope = server.tool(
        "acos_launch",
        json!({"apk": dir.join("app.apk").display().to_string()}),
    );
    assert_eq!(envelope["status"], "pass", "{envelope}");
    assert_eq!(envelope["fidelity"], "art-reference", "{envelope}");
    assert_eq!(
        envelope["result"]["engine"], "art",
        "the reply must come from the ART daemon: {envelope}"
    );

    let lines = log_lines(&log);
    let argv = lines
        .iter()
        .find(|line| line["kind"] == "argv" && line["argv"][0] == "serve")
        .unwrap_or_else(|| panic!("no `serve` was spawned: {lines:?}"));
    let argv: Vec<String> = argv["argv"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    assert!(argv.contains(&"art".to_string()), "argv: {argv:?}");
    let engine_index = argv.iter().position(|a| a == "--engine").expect("--engine");
    assert_eq!(argv[engine_index + 1], "art", "argv: {argv:?}");
    let socket_index = argv.iter().position(|a| a == "--socket").expect("--socket");
    let socket = PathBuf::from(&argv[socket_index + 1]);
    assert!(
        socket.ends_with("session.sock"),
        "socket: {}",
        socket.display()
    );
    assert!(
        socket.starts_with(runtime.join("acos")),
        "socket {} must be under the private runtime dir",
        socket.display()
    );
    let name = socket
        .parent()
        .unwrap()
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    assert!(
        name.starts_with(&format!("mcp-{}-", server.child.id())),
        "private dir `{name}` must be per server pid"
    );
    assert!(
        socket.as_os_str().len() <= 100,
        "socket path too long for sun_path: {}",
        socket.display()
    );

    let status = server.close();
    assert!(status.success(), "acos-mcp exited with {status}");
}

#[test]
fn default_session_ids_are_unique_per_server() {
    let mut ids = Vec::new();
    for name in ["unique-a", "unique-b"] {
        let (dir, runtime, fake, log, pid_file) = setup(name);
        let mut server = Server::spawn(&[
            ("ACOS_BIN", &fake),
            ("XDG_RUNTIME_DIR", &runtime),
            ("ACOS_ALLOWED_ROOTS", &dir),
            ("ACOS_FAKE_LOG", &log),
            ("ACOS_FAKE_PID_FILE", &pid_file),
        ]);
        server.initialize();
        let envelope = server.tool(
            "acos_status",
            json!({"base": {"request_id": "req_default_id"}}),
        );
        let id = envelope["session_id"]
            .as_str()
            .unwrap_or_else(|| panic!("no session id: {envelope}"))
            .to_string();
        // SERVELEAK3: the default id is this server instance's own —
        // `mcp-<pid>-<random>` (SERVELEAK2/F3 folded into REL3a's per-process
        // id). The pid names the owning process; the random suffix keeps two
        // servers apart even inside one pid namespace. Assert both halves rather
        // than the old exact `mcp-<pid>`, and keep the cross-server uniqueness
        // check below.
        let suffix = id
            .strip_prefix(&format!("mcp-{}-{}", server.child.id(), ""))
            .unwrap_or_else(|| panic!("default id {id} does not name this server: {envelope}"))
            .to_string();
        assert!(
            !suffix.is_empty() && suffix.chars().all(|c| c.is_ascii_hexdigit()),
            "default id {id} must carry a per-instance random suffix: {envelope}"
        );
        ids.push(id);
        server.close();
    }
    assert_ne!(ids[0], ids[1], "two servers shared a default session id");
}

#[test]
fn verify_schema_advertises_engine_and_defaults_to_art() {
    let (dir, runtime, fake, log, pid_file) = setup("verify-engine");
    let mut server = Server::spawn(&[
        ("ACOS_BIN", &fake),
        ("XDG_RUNTIME_DIR", &runtime),
        ("ACOS_ALLOWED_ROOTS", &dir),
        ("ACOS_FAKE_LOG", &log),
        ("ACOS_FAKE_PID_FILE", &pid_file),
    ]);
    server.initialize();
    let list = server.request("tools/list", json!({}));
    let verify = list["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["name"] == "acos_verify")
        .expect("acos_verify in tools/list");
    let engine = &verify["inputSchema"]["properties"]["engine"];
    assert_eq!(engine["default"], "art", "engine schema: {engine}");
    // schemars emits a named enum as a `$ref` plus a definition; resolve it so
    // the assertion is about the advertised values, not the schema shape.
    let reference = engine
        .get("$ref")
        .and_then(Value::as_str)
        .or_else(|| engine.pointer("/allOf/0/$ref").and_then(Value::as_str));
    let resolved = match reference {
        Some(reference) => {
            let name = reference.rsplit('/').next().expect("definition name");
            &verify["inputSchema"]["definitions"][name]
        }
        None => engine,
    };
    // schemars renders a unit-variant enum as `oneOf: [{enum:[..]}, ..]`.
    let mut variants: Vec<&str> = Vec::new();
    if let Some(values) = resolved["enum"].as_array() {
        variants.extend(values.iter().filter_map(Value::as_str));
    }
    for variant in resolved["oneOf"].as_array().into_iter().flatten() {
        variants.extend(
            variant["enum"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str),
        );
    }
    assert!(
        !variants.is_empty(),
        "engine schema has no enum: {resolved}"
    );
    assert!(variants.contains(&"art"), "{variants:?}");
    assert!(variants.contains(&"rust-vm"), "{variants:?}");

    // No engine argument: the CLI is told `--engine art`, never its own
    // frozen-Rust-VM default.
    let envelope = server.tool(
        "acos_verify",
        json!({
            "scenario": dir.join("s.yaml").display().to_string(),
            "artifacts": dir.join("artifacts").display().to_string(),
        }),
    );
    assert_eq!(envelope["fidelity"], "art-reference", "{envelope}");
    let lines = log_lines(&log);
    let verify_argv = lines
        .iter()
        .find(|line| line["kind"] == "argv" && line["argv"][0] == "verify")
        .unwrap_or_else(|| panic!("no `verify` ran: {lines:?}"));
    let argv: Vec<&str> = verify_argv["argv"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .collect();
    let index = argv
        .iter()
        .position(|a| *a == "--engine")
        .expect("--engine");
    assert_eq!(argv[index + 1], "art", "argv: {argv:?}");
    server.close();
}

#[test]
fn private_daemon_is_reaped_when_the_server_exits() {
    let (dir, runtime, fake, log, pid_file) = setup("reap");
    let mut server = Server::spawn(&[
        ("ACOS_BIN", &fake),
        ("XDG_RUNTIME_DIR", &runtime),
        ("ACOS_ALLOWED_ROOTS", &dir),
        ("ACOS_FAKE_LOG", &log),
        ("ACOS_FAKE_PID_FILE", &pid_file),
    ]);
    server.initialize();
    let envelope = server.tool("acos_status", json!({"base": {"request_id": "req_reap"}}));
    assert_eq!(envelope["status"], "pass", "{envelope}");
    let daemon_pid: u32 = std::fs::read_to_string(&pid_file)
        .expect("daemon pid file")
        .trim()
        .parse()
        .expect("pid");
    assert!(pid_alive(daemon_pid), "daemon {daemon_pid} not running");
    let socket = log_lines(&log)
        .iter()
        .find_map(|line| line["socket"].as_str().map(PathBuf::from))
        .expect("socket path");

    let status = server.close();
    assert!(status.success(), "acos-mcp exited with {status}");

    let deadline = Instant::now() + Duration::from_secs(20);
    while pid_alive(daemon_pid) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        !pid_alive(daemon_pid),
        "private daemon {daemon_pid} outlived the MCP server"
    );
    assert!(
        !socket.exists(),
        "private socket {} was left behind",
        socket.display()
    );
    let _ = std::fs::remove_dir_all(&dir);
}
