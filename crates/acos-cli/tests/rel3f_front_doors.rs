//! REL3F: the CLI and MCP are one front door.
//!
//! * `acos guide [topic]` serves the exact guide text the MCP `acos_help` tool
//!   serves (same `acos_mcp::help` function, no second copy in the CLI), and
//!   `acos help` names it;
//! * a session id/label is accepted positionally (`acos session destroy <id>`),
//!   with `--session` kept as the default-bearing flag;
//! * `acos session list` text mode answers with the count and one line per
//!   session (id, label, age, RSS), and labels the request id as a request.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::Output;

use assert_cmd::Command;
use serde_json::{json, Value};

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("acos-rel3f-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch");
    dir
}

fn workspace_path(rel: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(rel)
}

fn acos() -> Command {
    Command::cargo_bin("acos").unwrap()
}

fn run(args: &[&str]) -> Output {
    acos().args(args).output().expect("run acos")
}

// ---------------------------------------------------------------------------
// 1. `acos guide` = the MCP guide
// ---------------------------------------------------------------------------

#[test]
fn guide_prints_the_same_text_as_the_mcp_help_tool() {
    for (args, topic) in [
        (vec!["guide"], None),
        (vec!["guide", "workflow"], Some("workflow")),
        (vec!["guide", "scenarios"], Some("scenarios")),
        (vec!["guide", "all"], Some("all")),
    ] {
        let out = run(&args);
        assert!(
            out.status.success(),
            "acos {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert_eq!(
            stdout.trim_end(),
            acos_mcp::help::guide(topic).trim_end(),
            "acos {args:?} must serve the MCP acos_help text"
        );
        assert!(
            out.stderr.is_empty(),
            "a valid topic writes nothing to stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    // `--topic` is the spelling the feedback tried on `acos help`.
    let out = run(&["guide", "--topic", "workflow"]);
    assert!(out.status.success());
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim_end(),
        acos_mcp::help::guide(Some("workflow")).trim_end()
    );
}

#[test]
fn guide_with_a_wrong_topic_is_bounded_and_says_so_on_stderr() {
    let out = run(&["guide", "nonsense"]);
    assert!(out.status.success());
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim_end(),
        acos_mcp::help::guide(None).trim_end(),
        "a wrong topic returns the overview, exactly like acos_help"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("unknown topic `nonsense`"),
        "the diagnostic is on stderr: {stderr}"
    );
    assert!(
        stderr.contains("workflow"),
        "the diagnostic lists the real topics: {stderr}"
    );
}

#[test]
fn help_names_the_guide_command() {
    for args in [vec!["help"], vec!["--help"]] {
        let out = run(&args);
        assert!(out.status.success(), "acos {args:?}");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            stdout.contains("acos guide [topic]"),
            "`acos help` must point at the guide: {stdout}"
        );
        assert!(
            stdout.contains("Guide") || stdout.contains("guide"),
            "the guide subcommand is listed: {stdout}"
        );
    }
}

// ---------------------------------------------------------------------------
// 2/3. session id positionally, request id labelled, list text
// ---------------------------------------------------------------------------

#[test]
fn session_destroy_accepts_the_id_positionally() {
    // Without a daemon the command runs the in-process session path, which
    // echoes the addressed id in the envelope: proof the positional argument
    // reached the command, not just clap.
    let out = acos()
        .env("ACOS_NO_DAEMON", "1")
        .args(["session", "destroy", "my-session", "--json"])
        .output()
        .expect("run acos session destroy");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let envelope: Value = serde_json::from_slice(&out.stdout).expect("stdout is the JSON envelope");
    assert_eq!(envelope["status"], "pass");
    assert_eq!(envelope["result"]["session_id"], "my-session");
    assert_eq!(envelope["result"]["destroyed"], true);
}

#[test]
fn session_destroy_keeps_the_session_flag() {
    let out = acos()
        .env("ACOS_NO_DAEMON", "1")
        .args(["session", "destroy", "--session", "flagged", "--json"])
        .output()
        .expect("run acos session destroy --session");
    assert!(out.status.success());
    let envelope: Value = serde_json::from_slice(&out.stdout).expect("JSON envelope");
    assert_eq!(envelope["result"]["session_id"], "flagged");
}

/// A daemon that answers one `session_list` call with `result`, then exits.
///
/// The CLI's daemon client only needs the wire envelope; this proves the text
/// renderer end to end without booting ART.
fn fake_daemon(result: Value) -> (PathBuf, std::thread::JoinHandle<()>) {
    let (socket, _captured, daemon) = fake_daemon_capturing(result);
    (socket, daemon)
}

/// [`fake_daemon`] that also returns the request the CLI sent, so a test can
/// prove an argument reached the daemon (not just clap).
fn fake_daemon_capturing(
    result: Value,
) -> (
    PathBuf,
    std::sync::mpsc::Receiver<Value>,
    std::thread::JoinHandle<()>,
) {
    let (captured_tx, captured_rx) = std::sync::mpsc::channel();
    let dir = scratch(&format!(
        "daemon-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    let socket = dir.join("session.sock");
    let listener = UnixListener::bind(&socket).expect("bind fake daemon");
    listener
        .set_nonblocking(true)
        .expect("nonblocking fake daemon");
    let handle = std::thread::spawn(move || {
        // Never hang the test if the CLI decided not to use our socket.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while std::time::Instant::now() < deadline {
            let stream = match listener.accept() {
                Ok((stream, _)) => stream,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                    continue;
                }
                Err(_) => return,
            };
            let mut reader = BufReader::new(stream.try_clone().expect("clone"));
            let mut out = stream;
            // REL3D: one connection carries a `hello` handshake and then the
            // real call, so answer every request on it. The handshake is not the
            // call a test asks about: it is captured only when it is the first
            // line, and tests that assert on the forwarded tool skip it.
            loop {
                let mut line = String::new();
                match reader.read_line(&mut line) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
                if line.trim().is_empty() {
                    continue;
                }
                let request: Value = serde_json::from_str(&line).expect("request line");
                let tool = request["tool"].as_str().unwrap_or("").to_string();
                if tool != "hello" {
                    let _ = captured_tx.send(request.clone());
                }
                let id = request["id"].clone();
                // A fake daemon of *this* build: report the caller's own
                // identity (REL3D refuses a daemon that cannot name its build).
                let hello = tool == "hello";
                let mut identity = request
                    .pointer("/args/client")
                    .cloned()
                    .unwrap_or_else(|| json!({}));
                if let Some(object) = identity.as_object_mut() {
                    object.insert("pid".into(), json!(std::process::id()));
                    object.insert("engine".into(), json!("art"));
                    object.insert("auto_started".into(), json!(true));
                }
                let envelope = json!({
                    "request_id": if hello { "req_test_hello" } else { "req_test" },
                    "ok": true,
                    "status": "pass",
                    "backend": "acos-native",
                    "fidelity": "native-runtime",
                    "capabilities_used": [],
                    "result": if hello {
                        json!({"identity": identity, "engine": "art", "sessions": 0})
                    } else {
                        result.clone()
                    },
                    "metrics": {"wall_ms": 0},
                });
                let response = json!({"id": id, "envelope": envelope});
                let _ = writeln!(out, "{response}");
                let _ = out.flush();
            }
        }
    });
    (socket, captured_rx, handle)
}

fn daemon_run(socket: &Path, args: &[&str]) -> Output {
    acos()
        .env("ACOS_SESSION_SOCKET", socket)
        .args(args)
        .output()
        .expect("run acos against the fake daemon")
}

#[test]
fn session_list_text_prints_the_count_and_one_line_per_session() {
    let (socket, daemon) = fake_daemon(json!({
        "sessions": [
            {"session_id": "s_cli", "label": "counter", "age_s": 12, "guest_rss_kb": 204800},
            {"session_id": "mcp-1", "label": "gaten", "age_s": 3, "guest_rss_kb": 1024},
        ],
        "count": 2,
        "engine": "art",
    }));
    let out = daemon_run(&socket, &["session", "list"]);
    daemon.join().expect("fake daemon");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("request req_test: ok=true status=pass\n"),
        "the request id is labelled as a request: {stdout}"
    );
    assert!(
        stdout.contains("2 sessions (art)\n"),
        "the count is text, not only --json: {stdout}"
    );
    assert!(
        stdout.contains("  s_cli  label=counter  age=12s  rss=204800 kB"),
        "one row per session with id, label, age and RSS: {stdout}"
    );
    assert!(
        stdout.contains("  mcp-1  label=gaten  age=3s  rss=1024 kB"),
        "one row per session with id, label, age and RSS: {stdout}"
    );
    // The envelope line still names the request id, never a session handle.
    assert!(
        !stdout.contains("s_cli: ok="),
        "the status line must not read like a session: {stdout}"
    );
    let _ = std::fs::remove_dir_all(socket.parent().unwrap());
}

#[test]
fn session_list_text_says_zero_sessions_out_loud() {
    let (socket, daemon) = fake_daemon(json!({"sessions": [], "count": 0, "engine": "art"}));
    let out = daemon_run(&socket, &["session", "list"]);
    daemon.join().expect("fake daemon");
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("0 sessions (art)\n"),
        "\"0 sessions\" must be readable without --json: {stdout}"
    );
    let _ = std::fs::remove_dir_all(socket.parent().unwrap());
}

#[test]
fn session_list_json_stdout_is_only_the_document() {
    let (socket, daemon) = fake_daemon(json!({"sessions": [], "count": 0, "engine": "art"}));
    let out = daemon_run(&socket, &["session", "list", "--json"]);
    daemon.join().expect("fake daemon");
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    let envelope: Value = serde_json::from_str(stdout.trim()).expect("stdout is exactly JSON");
    assert_eq!(envelope["result"]["count"], 0);
    assert!(
        !stdout.contains("sessions (art)"),
        "no human lines on stdout in --json mode: {stdout}"
    );
    let _ = std::fs::remove_dir_all(socket.parent().unwrap());
}

/// The CLI must accept the same session `label` the MCP `acos_launch` takes, or
/// the label column `acos session list` prints could never be filled from the
/// CLI. The fake daemon proves the value reaches the wire, not just clap.
#[test]
fn launch_forwards_the_session_label_to_the_daemon() {
    let apk = workspace_path("fixtures/build/apps/counter.apk");
    if acos_test_support::require_prereq(apk.is_file(), "launch_forwards_the_session_label_to_the_daemon", "fixture missing").is_none() {
        return;
    }
    let (socket, captured, daemon) = fake_daemon_capturing(json!({}));
    let out = acos()
        .env("ACOS_SESSION_SOCKET", &socket)
        .arg("launch")
        .arg(&apk)
        .args(["--keep", "--engine", "art", "--label", "demo", "--json"])
        .output()
        .expect("run acos launch");
    let request = captured
        .recv_timeout(std::time::Duration::from_secs(5))
        .expect("the CLI called the daemon");
    daemon.join().expect("fake daemon");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(request["tool"], "acos_launch");
    assert_eq!(request["args"]["label"], "demo");
    let _ = std::fs::remove_dir_all(socket.parent().unwrap());
}
