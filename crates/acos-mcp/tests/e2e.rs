//! End-to-end test: spawn the real `acos-mcp` binary and speak MCP over stdio.

mod common;

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use common::{envelope_of, fixture_path, is_error};
use serde_json::{json, Value};

fn server_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_acos-mcp"))
}

struct ChildClient {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: BufReader<ChildStdout>,
}

impl ChildClient {
    fn spawn() -> ChildClient {
        let mut child = Command::new(server_binary())
            .env("ACOS_BIN", common::acos_bin())
            .env("ACOS_ALLOWED_ROOTS", common::repo_root())
            .env("ACOS_MCP_QUIET", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn acos-mcp");
        let stdin = child.stdin.take().expect("stdin");
        let stdout = BufReader::new(child.stdout.take().expect("stdout"));
        ChildClient {
            child,
            stdin: Some(stdin),
            stdout,
        }
    }

    fn send(&mut self, value: &Value) {
        let line = serde_json::to_string(value).expect("serialize");
        let stdin = self.stdin.as_mut().expect("stdin open");
        writeln!(stdin, "{line}").expect("write");
        stdin.flush().expect("flush");
    }

    fn recv(&mut self) -> Value {
        let mut line = String::new();
        let n = self.stdout.read_line(&mut line).expect("read");
        assert!(n > 0, "server closed stdout");
        serde_json::from_str(&line).expect("json response")
    }

    fn request(&mut self, id: i64, method: &str, params: Value) -> Value {
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        loop {
            let value = self.recv();
            if value.get("id").and_then(Value::as_i64) == Some(id) {
                return value;
            }
        }
    }
}

impl Drop for ChildClient {
    fn drop(&mut self) {
        // Close stdin first so the server sees EOF, then wait for it to exit.
        drop(self.stdin.take());
        let _ = self.child.wait();
    }
}

#[test]
fn end_to_end_mcp_session_over_stdio() {
    let mut client = ChildClient::spawn();

    // 1. initialize
    let init = client.request(
        1,
        "initialize",
        json!({
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": {"name": "e2e", "version": "0"},
        }),
    );
    assert_eq!(init["result"]["protocolVersion"], "2025-06-18");
    assert_eq!(init["result"]["serverInfo"]["name"], "acos-mcp");
    client.send(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));

    // 2. tools/list (assert it is large; full contents are covered in protocol.rs)
    let list = client.request(2, "tools/list", json!({}));
    let tools = list["result"]["tools"].as_array().expect("tools");
    assert_eq!(tools.len(), 15);
    assert!(tools
        .iter()
        .any(|t| t["name"] == "acos_inspect" && t["annotations"]["available"] == true));

    // 3. tools/call inspect
    let inspect = client.request(
        3,
        "tools/call",
        json!({
            "name": "acos_inspect",
            "arguments": {
                "base": {"request_id": "e2e_inspect"},
                "apk": fixture_path("fixtures/build/apps/hello-views.apk"),
            },
        }),
    );
    assert!(!is_error(&inspect));
    let envelope = envelope_of(&inspect);
    assert_eq!(envelope["status"], "pass");
    assert_eq!(envelope["fidelity"], "static-analysis");
    assert_eq!(envelope["result"]["package"], "dev.acos.fixtures.helloviews");

    // 4. tools/call run-dex
    let run_dex = client.request(
        4,
        "tools/call",
        json!({
            "name": "acos_run_dex",
            "arguments": {
                "base": {"request_id": "e2e_rundex"},
                "path": fixture_path("fixtures/build/dexprogs/01_hello/classes.dex"),
                "main": "Main",
            },
        }),
    );
    assert!(!is_error(&run_dex));
    let envelope = envelope_of(&run_dex);
    assert_eq!(envelope["status"], "pass");
    assert_eq!(envelope["fidelity"], "native-runtime");
    let expected = common::fixture("fixtures/expected/dexprogs/01_hello.stdout");
    assert_eq!(envelope["result"]["stdout"].as_str().unwrap(), expected);

    // 5. an unimplemented command is a tool error, not a pass
    let unsupported = client.request(
        5,
        "tools/call",
        json!({
            "name": "acos_run",
            "arguments": {"base": {"request_id": "e2e_run"}, "apk": "x.apk"},
        }),
    );
    assert!(is_error(&unsupported));
    assert_eq!(envelope_of(&unsupported)["status"], "unsupported");
}
