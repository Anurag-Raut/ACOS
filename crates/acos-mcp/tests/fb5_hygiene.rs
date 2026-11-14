//! FB5: session hygiene on the MCP session backend.
//!
//! Two behaviours that only a real daemon round trip can prove:
//!
//! 1. the idle TTL destroys a session this server owned, and a later call on it
//!    says **why** it is gone (`SESSION_NOT_FOUND` + `idle_ttl_expired` + the
//!    TTL), instead of a generic not-found;
//! 2. `acos_status` lists this server's sessions with the agent's `label` and the
//!    idle clock;
//! 3. teardown against an **external** daemon that cannot list its sessions does
//!    not fall back to destroy-by-id (SERVELEAK3 review finding 3) — it logs the
//!    leak and leaves the other clients' sessions alone.
//!
//! The daemon here is a fake that speaks the real line-JSON wire protocol
//! (`crates/acos-mcp/src/daemon.rs`), so the backend's own forwarding path is
//! exercised unchanged.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use acos_mcp::{BackendConfig, CliBackend, CallContext, SessionBackend};
use acos_protocol::{
    Backend as ProtocolBackend, Envelope, ErrorCode, Fidelity, Metrics, ProtocolError,
};
use acos_runtime::session::{AgentSession, SessionOptions};
use serde_json::{json, Value};

/// A short scratch directory: a unix socket path must fit `SUN_LEN` (108 bytes).
fn scratch(name: &str) -> PathBuf {
    let dir = PathBuf::from(std::env::var_os("ACOS_TEST_SCRATCH").unwrap_or_else(|| {
        std::env::temp_dir().into()
    }))
    .join(format!(
        "acos-fb5-{}-{}-{name}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// A normal daemon: `session_list` reports the live sessions.
    Healthy,
    /// `session_list` fails (an older/foreign daemon): teardown must not guess.
    ListFails,
    /// An older daemon: launches and listings carry no `session_token` (M2).
    NoTokens,
    /// Non-launch/destroy/list tools take 1.5 s (an in-flight call, M1).
    Slow,
}

struct FakeDaemon {
    seen: Receiver<Value>,
    sessions: Arc<Mutex<Vec<Value>>>,
    stop: std::sync::mpsc::Sender<()>,
}

/// Serve the daemon protocol on `path`, tracking sessions like `acos serve`:
/// `acos_launch` adds the id with a token, `acos_destroy` removes it.
fn fake_daemon(path: &std::path::Path, mode: Mode) -> FakeDaemon {
    let listener = UnixListener::bind(path).expect("bind fake daemon");
    let (tx, rx) = channel::<Value>();
    let (stop, stop_rx) = channel::<()>();
    let sessions = Arc::new(Mutex::new(Vec::<Value>::new()));
    let live = Arc::clone(&sessions);
    std::thread::spawn(move || {
        let Ok((stream, _)) = listener.accept() else {
            return;
        };
        let mut writer = stream.try_clone().expect("clone stream");
        let mut reader = BufReader::new(stream);
        loop {
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
            let _ = tx.send(json!({"tool": tool, "session_id": session, "args": args}));

            let envelope = if tool == "session_list" {
                if mode == Mode::ListFails {
                    Envelope::failure(
                        "req_list",
                        ProtocolBackend::AcosNative,
                        Fidelity::NativeRuntime,
                        ProtocolError::new(ErrorCode::UnsupportedBackend, "session_list is not available"),
                        Metrics::wall_ms(0),
                    )
                } else {
                    let listed = live.lock().unwrap().clone();
                    Envelope::pass(
                        "req_list",
                        ProtocolBackend::AcosNative,
                        Fidelity::NativeRuntime,
                        Metrics::wall_ms(0),
                    )
                    .with_value_result(json!({"sessions": listed, "count": listed.len()}))
                }
            } else if tool == "acos_launch" {
                let token = if mode == Mode::NoTokens {
                    String::new()
                } else {
                    format!("tok-{session}")
                };
                let mut listed = live.lock().unwrap();
                listed.retain(|entry| entry["session_id"] != json!(session));
                let mut entry = json!({
                    "session_id": session,
                    "session_token": token,
                    "launched": true,
                    "package": "dev.test.counter",
                    "activity": "dev.test.counter/.Main",
                    "stage_reached": "first_frame",
                    "live": true,
                    "mode": "live",
                    "guest_pid": Value::Null,
                });
                if mode == Mode::NoTokens {
                    entry.as_object_mut().unwrap().remove("session_token");
                }
                listed.push(entry);
                drop(listed);
                Envelope::pass(
                    "req_launch",
                    ProtocolBackend::AcosNative,
                    Fidelity::ArtReference,
                    Metrics::wall_ms(0),
                )
                .with_value_result(if mode == Mode::NoTokens {
                    json!({"session_id": session, "launched": true, "reached_stage": "first_frame"})
                } else {
                    json!({
                    "session_id": session,
                    "session_token": token,
                    "launched": true,
                    "reached_stage": "first_frame",
                    })
                })
            } else if tool == "acos_destroy" {
                live.lock()
                    .unwrap()
                    .retain(|entry| entry["session_id"] != json!(session));
                Envelope::pass(
                    "req_destroy",
                    ProtocolBackend::AcosNative,
                    Fidelity::NativeRuntime,
                    Metrics::wall_ms(0),
                )
                .with_value_result(json!({"destroyed": true}))
            } else {
                if mode == Mode::Slow {
                    std::thread::sleep(Duration::from_millis(1500));
                }
                Envelope::pass(
                    "req_other",
                    ProtocolBackend::AcosNative,
                    Fidelity::NativeRuntime,
                    Metrics::wall_ms(0),
                )
                .with_value_result(json!({"ok": true}))
            };
            let reply = json!({"id": id, "envelope": envelope});
            let mut payload = serde_json::to_string(&reply).expect("serialize reply");
            payload.push('\n');
            if writer.write_all(payload.as_bytes()).is_err() {
                return;
            }
            let _ = writer.flush();
        }
    });
    FakeDaemon {
        seen: rx,
        sessions,
        stop,
    }
}

fn drain<'a>(seen: &'a Receiver<Value>, tool: &str) -> Option<Value> {
    while let Ok(value) = seen.recv_timeout(Duration::from_secs(10)) {
        if value.get("tool").and_then(Value::as_str) == Some(tool) {
            return Some(value);
        }
    }
    None
}

fn context(allowed_root: &std::path::Path) -> CallContext {
    CallContext {
        cancel: acos_mcp::CancelToken::new(),
        deadline: std::time::Instant::now() + Duration::from_secs(60),
        budget: acos_protocol::ResourceBudget::default(),
        allowed_roots: vec![allowed_root.to_path_buf()],
        artifacts_dir: allowed_root.to_path_buf(),
        max_arg_chars: 8 * 1024,
        max_output_bytes: 1024 * 1024,
    }
}

fn backend(socket: &std::path::Path) -> SessionBackend {
    SessionBackend::with_cli(
        CliBackend::new(BackendConfig::for_binary("/nonexistent/acos-fb5-test")),
        SessionOptions::default(),
    )
    .with_daemon_socket(socket.to_path_buf())
}

#[test]
fn idle_ttl_destroys_the_session_and_later_calls_name_the_ttl() {
    let dir = scratch("idle-ttl");
    let socket = dir.join("session.sock");
    let fake = fake_daemon(&socket, Mode::Healthy);
    let backend = backend(&socket);
    let ctx = context(&dir);

    let launch = backend
        .session_call(
            "acos_launch",
            &json!({
                "apk": dir.join("counter.apk").display().to_string(),
                "label": "counter",
                "idle_ttl_s": 1,
                "base": {"request_id": "req_launch", "session_id": "s_park"},
            }),
            &ctx,
        )
        .expect("launch is forwarded");
    assert!(launch.ok(), "launch: {launch:?}");
    assert_eq!(launch.result().unwrap()["label"], json!("counter"));
    assert_eq!(launch.result().unwrap()["idle_ttl_s"], json!(1));
    drain(&fake.seen, "acos_launch").expect("daemon saw the launch");

    // acos_status lists this server's session, with the agent's label.
    let status = backend
        .session_call(
            "acos_status",
            &json!({"base": {"request_id": "req_status", "session_id": "s_park"}}),
            &ctx,
        )
        .expect("status");
    assert!(status.ok(), "status: {status:?}");
    let result = status.result().cloned().expect("result");
    assert_eq!(result["session_id"], json!("s_park"));
    assert_eq!(result["label"], json!("counter"));
    assert_eq!(result["package"], json!("dev.test.counter"));
    assert_eq!(result["activity"], json!("dev.test.counter/.Main"));
    assert_eq!(result["idle_ttl_s"], json!(1));
    assert!(result["idle_s"].is_number(), "idle_s is reported: {result}");
    assert!(result["age_s"].is_number(), "age_s is reported: {result}");
    let listed = result["sessions"].as_array().cloned().expect("sessions");
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0]["session_id"], json!("s_park"));
    assert_eq!(listed[0]["label"], json!("counter"));

    // Idle past the TTL: one sweep destroys it through the daemon…
    std::thread::sleep(Duration::from_millis(1100));
    backend.sweep_idle();
    let destroy = drain(&fake.seen, "acos_destroy").expect("the idle session is destroyed");
    assert_eq!(destroy["session_id"], json!("s_park"));
    assert!(
        fake.sessions.lock().unwrap().is_empty(),
        "the fake daemon no longer holds the session"
    );

    // …and the next call on it says why it is gone, naming the TTL.
    let tap = backend
        .session_call(
            "acos_tap",
            &json!({"x": 10, "y": 10, "base": {"request_id": "req_tap", "session_id": "s_park"}}),
            &ctx,
        )
        .expect("tap is answered locally");
    assert!(!tap.ok(), "an expired session must not be a pass");
    let error = tap.error().expect("error");
    assert_eq!(error.code, ErrorCode::SessionNotFound);
    assert!(
        error.message.contains("idle TTL 1 s"),
        "the expiry names the TTL: {}",
        error.message
    );
    let details = error.details.clone().unwrap_or(Value::Null);
    assert_eq!(details["reason"], json!("idle_ttl_expired"));
    assert_eq!(details["idle_ttl_s"], json!(1));

    let _ = fake.stop.send(());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn external_daemon_list_failure_never_falls_back_to_destroy_by_id() {
    let dir = scratch("list-fails");
    let socket = dir.join("session.sock");
    let fake = fake_daemon(&socket, Mode::ListFails);
    let backend = backend(&socket);
    let ctx = context(&dir);

    let launch = backend
        .session_call(
            "acos_launch",
            &json!({
                "apk": dir.join("counter.apk").display().to_string(),
                "base": {"request_id": "req_launch", "session_id": "s_maybe_foreign"},
            }),
            &ctx,
        )
        .expect("launch is forwarded");
    assert!(launch.ok());
    drain(&fake.seen, "acos_launch").expect("launch seen");

    backend.shutdown();
    // No destroy may be sent: the server could not prove the id is still its
    // own launch, and destroying it could kill another client's live guest.
    let mut seen_destroy = false;
    while let Ok(value) = fake.seen.recv_timeout(Duration::from_millis(300)) {
        if value.get("tool").and_then(Value::as_str) == Some("acos_destroy") {
            seen_destroy = true;
        }
    }
    assert!(
        !seen_destroy,
        "teardown must not destroy by id when session_list failed"
    );

    let _ = fake.stop.send(());
    let _ = std::fs::remove_dir_all(&dir);
}

/// The in-process backend exists so the helpers above compile against a real
/// type; this keeps the import honest without starting the legacy engine.
#[allow(dead_code)]
fn _in_process_is_a_type(session: AgentSession) -> bool {
    let _ = session;
    true
}


fn launch(backend: &SessionBackend, ctx: &CallContext, dir: &std::path::Path, session: Option<&str>, ttl: u64) -> Envelope {
    let mut base = json!({"request_id": "req_launch"});
    if let Some(session) = session {
        base["session_id"] = json!(session);
    }
    let envelope = backend
        .session_call(
            "acos_launch",
            &json!({
                "apk": dir.join("counter.apk").display().to_string(),
                "idle_ttl_s": ttl,
                "base": base,
            }),
            ctx,
        )
        .expect("launch is forwarded");
    assert!(envelope.ok(), "launch: {envelope:?}");
    envelope
}

fn tools_seen(seen: &Receiver<Value>, wait_ms: u64) -> Vec<(String, String)> {
    let mut out = Vec::new();
    while let Ok(value) = seen.recv_timeout(Duration::from_millis(wait_ms)) {
        out.push((
            value["tool"].as_str().unwrap_or("").to_string(),
            value["session_id"].as_str().unwrap_or("").to_string(),
        ));
    }
    out
}

/// FB5b/H1: once the DEFAULT session expired, its tombstone must not make every
/// later `session_list` "fail": the next sweep still lists, still proves
/// ownership, destroys only what is ours and never anything foreign.
#[test]
fn h1_default_session_expiry_does_not_break_the_next_sweeps_listing() {
    let dir = scratch("h1");
    let socket = dir.join("session.sock");
    let fake = fake_daemon(&socket, Mode::Healthy);
    let backend = backend(&socket);
    let ctx = context(&dir);

    launch(&backend, &ctx, &dir, None, 1); // default session id
    launch(&backend, &ctx, &dir, Some("s_second"), 3);
    fake.sessions.lock().unwrap().push(json!({
        "session_id": "s_foreign", "session_token": "tok-foreign", "live": true
    }));
    tools_seen(&fake.seen, 100);

    std::thread::sleep(Duration::from_millis(1200));
    backend.sweep_idle();
    let first = tools_seen(&fake.seen, 200);
    let default_id = acos_mcp::session_backend::default_session_id();
    assert!(first.contains(&("acos_destroy".into(), default_id.into())), "{first:?}");

    // The default session is tombstoned now. A second sweep must still list.
    std::thread::sleep(Duration::from_millis(2000));
    backend.sweep_idle();
    let second = tools_seen(&fake.seen, 200);
    assert!(
        second.iter().any(|(tool, _)| tool == "session_list"),
        "the second sweep listed: {second:?}"
    );
    assert!(
        second.contains(&("acos_destroy".into(), "s_second".into())),
        "our own second session was proven and destroyed: {second:?}"
    );
    assert!(
        !second.iter().chain(first.iter()).any(|(tool, id)| tool == "acos_destroy" && id == "s_foreign"),
        "a foreign session is never destroyed"
    );
    assert!(fake
        .sessions
        .lock()
        .unwrap()
        .iter()
        .any(|entry| entry["session_id"] == json!("s_foreign")));

    // And a listing is not an expired-session answer even for the default id.
    let list = backend
        .session_call("session_list", &json!({"base": {"request_id": "req_list"}}), &ctx)
        .expect("forwarded");
    assert!(list.ok(), "session_list is not the tombstone: {list:?}");

    let _ = fake.stop.send(());
    let _ = std::fs::remove_dir_all(&dir);
}

/// FB5b/H1 + the review's scenario: another client relaunched the same explicit
/// id (a different token); our expiry must leave its guest alone.
#[test]
fn h1_a_relaunched_foreign_session_with_the_same_id_is_never_destroyed() {
    let dir = scratch("h1-relaunch");
    let socket = dir.join("session.sock");
    let fake = fake_daemon(&socket, Mode::Healthy);
    let backend = backend(&socket);
    let ctx = context(&dir);

    launch(&backend, &ctx, &dir, Some("s_shared"), 1);
    // The other client relaunched the same id: a new token.
    for entry in fake.sessions.lock().unwrap().iter_mut() {
        entry["session_token"] = json!("tok-OTHER-CLIENT");
    }
    tools_seen(&fake.seen, 100);
    std::thread::sleep(Duration::from_millis(1200));
    backend.sweep_idle();
    let seen = tools_seen(&fake.seen, 200);
    assert!(
        !seen.iter().any(|(tool, _)| tool == "acos_destroy"),
        "nothing destroyed: {seen:?}"
    );
    assert_eq!(fake.sessions.lock().unwrap().len(), 1, "their guest lives");
    let _ = fake.stop.send(());
    let _ = std::fs::remove_dir_all(&dir);
}

/// FB5b/H1: a failed listing on the EXTERNAL daemon destroys nothing at all.
#[test]
fn h1_a_failed_listing_destroys_nothing_on_the_external_daemon() {
    let dir = scratch("h1-listfail");
    let socket = dir.join("session.sock");
    let fake = fake_daemon(&socket, Mode::ListFails);
    let backend = backend(&socket);
    let ctx = context(&dir);
    launch(&backend, &ctx, &dir, Some("s_maybe"), 1);
    tools_seen(&fake.seen, 100);
    std::thread::sleep(Duration::from_millis(1200));
    assert!(backend.sweep_idle(), "reports the unproven listing");
    let seen = tools_seen(&fake.seen, 200);
    assert!(!seen.iter().any(|(tool, _)| tool == "acos_destroy"), "{seen:?}");
    // The session is still ours and live (no tombstone either).
    let tap = backend
        .session_call("acos_observe", &json!({"base": {"request_id": "r", "session_id": "s_maybe"}}), &ctx)
        .expect("forwarded");
    assert!(tap.ok(), "{tap:?}");
    let _ = fake.stop.send(());
    let _ = std::fs::remove_dir_all(&dir);
}

/// FB5b/M2: a tokenless session on an external daemon is never idle-expired.
#[test]
fn m2_tokenless_sessions_are_never_expired_on_the_external_daemon() {
    let dir = scratch("m2");
    let socket = dir.join("session.sock");
    let fake = fake_daemon(&socket, Mode::NoTokens);
    let backend = backend(&socket);
    let ctx = context(&dir);
    let reply = launch(&backend, &ctx, &dir, Some("s_old_daemon"), 1);
    assert_eq!(reply.result().unwrap()["idle_ttl_s"], json!(0), "expiry disabled and reported");
    tools_seen(&fake.seen, 100);
    std::thread::sleep(Duration::from_millis(1200));
    assert!(!backend.sweep_idle());
    let seen = tools_seen(&fake.seen, 200);
    assert!(
        !seen.iter().any(|(tool, _)| tool == "acos_destroy"),
        "no id-only destroy: {seen:?}"
    );
    let _ = fake.stop.send(());
    let _ = std::fs::remove_dir_all(&dir);
}

/// FB5b/M1 end to end: a slow call in flight past the TTL keeps its session.
#[test]
fn m1_a_slow_call_in_flight_is_not_idleness() {
    let dir = scratch("m1");
    let socket = dir.join("session.sock");
    let fake = fake_daemon(&socket, Mode::Slow);
    let backend = Arc::new(backend(&socket));
    let ctx = context(&dir);
    launch(&backend, &ctx, &dir, Some("s_slow"), 1);
    tools_seen(&fake.seen, 100);

    let worker = {
        let backend = Arc::clone(&backend);
        let ctx = context(&dir);
        std::thread::spawn(move || {
            backend.session_call(
                "acos_observe",
                &json!({"base": {"request_id": "r", "session_id": "s_slow"}}),
                &ctx,
            )
        })
    };
    std::thread::sleep(Duration::from_millis(1200)); // call started 1.2 s ago, TTL is 1 s
    backend.sweep_idle();
    let reply = worker.join().expect("worker").expect("forwarded");
    assert!(reply.ok(), "{reply:?}");
    let seen = tools_seen(&fake.seen, 200);
    assert!(
        !seen.iter().any(|(tool, _)| tool == "acos_destroy"),
        "the in-flight call kept the session: {seen:?}"
    );
    // The finished call renewed the clock: an immediate sweep is a no-op too.
    backend.sweep_idle();
    assert!(!tools_seen(&fake.seen, 200).iter().any(|(tool, _)| tool == "acos_destroy"));
    let _ = fake.stop.send(());
    let _ = std::fs::remove_dir_all(&dir);
}

/// FB5b/L1: `acos_status` never reports (or measures) another client's guest.
#[test]
fn l1_status_ignores_a_session_another_client_relaunched() {
    let dir = scratch("l1");
    let socket = dir.join("session.sock");
    let fake = fake_daemon(&socket, Mode::Healthy);
    let backend = backend(&socket);
    let ctx = context(&dir);
    launch(&backend, &ctx, &dir, Some("s_mine"), 600);
    for entry in fake.sessions.lock().unwrap().iter_mut() {
        entry["session_token"] = json!("tok-OTHER-CLIENT");
        entry["guest_pid"] = json!(std::process::id());
    }
    let status = backend
        .session_call("acos_status", &json!({"base": {"request_id": "r", "session_id": "s_mine"}}), &ctx)
        .expect("status");
    let text = serde_json::to_string(&status).unwrap();
    let sessions = status
        .result()
        .and_then(|result| result.get("sessions"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    assert!(
        sessions.iter().all(|entry| entry["session_id"] != json!("s_mine")),
        "another client's guest is not listed as ours: {text}"
    );
    let _ = fake.stop.send(());
    let _ = std::fs::remove_dir_all(&dir);
}
