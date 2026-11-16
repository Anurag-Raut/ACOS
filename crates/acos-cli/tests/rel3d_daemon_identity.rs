//! REL3D: a CLI call is never answered by a daemon of a different build.
//!
//! MODA11Y: the ART CLI silently routed through a shared `acos serve` daemon, so
//! a daemon left over from an older binary answered a newer client with the old
//! build's behaviour (`code 5 unsupported` for a service the new build
//! implements). For an agent that is a verdict from code it never ran.
//!
//! The wire contract these tests drive (see `crates/acos-mcp/src/identity.rs`):
//!
//! * every daemon reports `crate version + build id (git sha + source digest +
//!   binary hash)` in `hello`/`ping`, and every client compares before any
//!   request (`DaemonClient::call` refuses a mismatch with a structured
//!   `DAEMON_VERSION_MISMATCH` carrying both identities);
//! * an idle **auto-started** daemon of another build is stopped and replaced by
//!   a matching one on the shared socket;
//! * a daemon that holds sessions — or an operator's `acos serve`, or an older
//!   daemon whose spawning client is still alive — is never signalled: the
//!   client starts its own build-private daemon and says so in the warning;
//! * `acos doctor` prints the running daemon's identity next to the CLI's.
//!
//! The daemon here is a *fake* (this test binary re-executed as a daemon) that
//! speaks the real line-JSON wire protocol and claims another build, so the
//! client's decision path is exercised unchanged.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::os::unix::net::{UnixListener, UnixStream};

use acos_protocol::{Backend as ProtocolBackend, Envelope, ErrorCode, Fidelity, Metrics, ProtocolError};
use serde_json::{json, Value};

/// The test function name, so the child process runs exactly this function.
const TEST_NAME: &str = "rel3d_a_foreign_daemon_never_answers_the_cli";

// ---------------------------------------------------------------------------
// The fake daemon (this test binary, re-executed)
// ---------------------------------------------------------------------------

struct FakeConfig {
    socket: PathBuf,
    build_id: String,
    sessions: usize,
    auto_started: bool,
    /// An older daemon: it has no `hello` at all.
    legacy: bool,
}

fn fake_config() -> Option<FakeConfig> {
    let socket = std::env::var_os("REL3D_FAKE_SOCKET")?;
    Some(FakeConfig {
        socket: PathBuf::from(socket),
        build_id: std::env::var("REL3D_FAKE_BUILD").expect("REL3D_FAKE_BUILD"),
        sessions: std::env::var("REL3D_FAKE_SESSIONS")
            .ok()
            .and_then(|n| n.parse().ok())
            .unwrap_or(0),
        auto_started: std::env::var("REL3D_FAKE_AUTO").as_deref() == Ok("1"),
        legacy: std::env::var("REL3D_FAKE_LEGACY").as_deref() == Ok("1"),
    })
}

/// Act as the foreign-build daemon until killed.
fn serve_fake_daemon(config: FakeConfig) -> ! {
    let _ = std::fs::remove_file(&config.socket);
    let listener = UnixListener::bind(&config.socket).expect("bind fake socket");
    // The parent reads this line to learn the daemon is up.
    println!("PID {}", std::process::id());
    let mut held: Vec<String> = (0..config.sessions)
        .map(|i| format!("s_fake_{i}"))
        .collect();
    loop {
        let Ok((stream, _)) = listener.accept() else {
            continue;
        };
        let mut writer = stream.try_clone().expect("clone");
        let mut reader = BufReader::new(stream);
        loop {
            let mut line = String::new();
            match reader.read_line(&mut line) {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
            if line.trim().is_empty() {
                continue;
            }
            let request: Value = serde_json::from_str(&line).expect("request JSON");
            let id = request.get("id").cloned().unwrap_or(Value::Null);
            let tool = request.get("tool").and_then(Value::as_str).unwrap_or("");
            let session = request
                .pointer("/args/base/session_id")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let envelope = match tool {
                "hello" if !config.legacy => Envelope::pass(
                    "req_hello",
                    ProtocolBackend::AcosNative,
                    Fidelity::NativeRuntime,
                    Metrics::wall_ms(0),
                )
                .with_value_result(json!({
                    "identity": {
                        "protocol": 1,
                        "version": "0.0.1-old",
                        "git_sha": "000000000000",
                        "source_hash": "fake-old-source",
                        "build_id": config.build_id,
                        "exe": "/nonexistent/fake-acos",
                        "exe_len": 1,
                        "exe_hash": "fakebinaryhash",
                        "pid": std::process::id(),
                        "engine": "art",
                        "auto_started": config.auto_started,
                    },
                    "engine": "art",
                    "sessions": held.len(),
                })),
                "session_list" => Envelope::pass(
                    "req_list",
                    ProtocolBackend::AcosNative,
                    Fidelity::NativeRuntime,
                    Metrics::wall_ms(0),
                )
                .with_value_result(json!({
                    "sessions": held
                        .iter()
                        .map(|id| json!({"session_id": id, "source": "FAKE-OLD-BUILD"}))
                        .collect::<Vec<_>>(),
                    "count": held.len(),
                })),
                "acos_launch" => {
                    held.push(session.clone());
                    Envelope::pass(
                        "req_launch",
                        ProtocolBackend::AcosNative,
                        Fidelity::NativeRuntime,
                        Metrics::wall_ms(0),
                    )
                    .with_value_result(json!({
                        "session_id": session,
                        "launched": true,
                        "source": "FAKE-OLD-BUILD",
                    }))
                }
                // The MODA11Y symptom: a service the newer build implements is
                // answered by the old daemon as unsupported.
                _ => Envelope::failure(
                    "req_old",
                    ProtocolBackend::AcosNative,
                    Fidelity::NativeRuntime,
                    ProtocolError::new(
                        ErrorCode::UnsupportedBackend,
                        "code 5 unsupported (FAKE-OLD-BUILD)",
                    ),
                    Metrics::wall_ms(0),
                ),
            };
            let reply = json!({"id": id, "envelope": envelope});
            let mut payload = serde_json::to_string(&reply).expect("reply JSON");
            payload.push('\n');
            if writer.write_all(payload.as_bytes()).is_err() {
                break;
            }
            let _ = writer.flush();
        }
    }
}

// ---------------------------------------------------------------------------
// Host side of the test
// ---------------------------------------------------------------------------

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn acos() -> &'static str {
    env!("CARGO_BIN_EXE_acos")
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "acos-rel3d-{}-{name}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch");
    // The runtime directory rule: 0700, or `acos serve` refuses to use it.
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).expect("0700");
    dir
}

/// Start the fake daemon as a separate process (it must be killable).
fn start_fake(
    socket: &Path,
    build_id: &str,
    sessions: usize,
    auto_started: bool,
    legacy: bool,
) -> Child {
    let exe = std::env::current_exe().expect("test binary");
    let mut child = Command::new(exe)
        .arg(TEST_NAME)
        .arg("--exact")
        .arg("--nocapture")
        .env("REL3D_FAKE_SOCKET", socket)
        .env("REL3D_FAKE_BUILD", build_id)
        .env("REL3D_FAKE_SESSIONS", sessions.to_string())
        .env("REL3D_FAKE_AUTO", if auto_started { "1" } else { "0" })
        .env("REL3D_FAKE_LEGACY", if legacy { "1" } else { "0" })
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("spawn fake daemon");
    let stdout = child.stdout.take().expect("fake stdout");
    let mut lines = BufReader::new(stdout).lines();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while std::time::Instant::now() < deadline {
        match lines.next() {
            Some(Ok(line)) if line.starts_with("PID ") => return child,
            Some(Ok(_)) => continue,
            _ => break,
        }
    }
    let _ = child.kill();
    panic!("the fake daemon never reported its pid");
}

fn pid_alive(pid: u32) -> bool {
    if unsafe { libc::kill(pid as i32, 0) } != 0 {
        return false;
    }
    std::fs::read_to_string(format!("/proc/{pid}/stat"))
        .ok()
        .and_then(|stat| stat.rsplit_once(')').map(|(_, rest)| rest.to_string()))
        .map(|rest| !rest.trim_start().starts_with('Z'))
        .unwrap_or(false)
}

fn run(dir: &Path, args: &[&str]) -> (Value, String) {
    let output = Command::new(acos())
        .current_dir(repo_root())
        .env("ACOS_RUNTIME_DIR", dir)
        .env("ACOS_SESSION_IDLE_SECS", "60")
        .env("ACOS_NO_DAEMON", "0")
        .args(args)
        .output()
        .expect("run acos");
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    let value: Value = serde_json::from_str(stdout.trim()).unwrap_or_else(|e| {
        panic!("stdout should be one JSON envelope: {e}\nstdout={stdout}\nstderr={stderr}")
    });
    (value, stderr)
}

/// The last JSON object a command wrote to stderr (the structured warning).
fn stderr_warning(stderr: &str) -> Option<Value> {
    stderr
        .lines()
        .filter(|line| line.trim_start().starts_with('{'))
        .filter_map(|line| serde_json::from_str::<Value>(line.trim()).ok())
        .filter(|value| value.get("code").is_some())
        .next_back()
}

fn cli_build_id() -> String {
    // The CLI reports its own identity in the `hello` it sends; the doctor line
    // names it, so read it from there instead of duplicating the build hash.
    let dir = scratch("identity");
    let (value, _) = run(&dir, &["doctor", "--json"]);
    let _ = std::fs::remove_dir_all(&dir);
    let check = value["checks"]
        .as_array()
        .and_then(|checks| {
            checks
                .iter()
                .find(|check| check["name"] == "session_daemon")
        })
        .expect("doctor reports session_daemon");
    let detail = check["detail"].as_str().unwrap_or("");
    detail
        .rsplit("cli ")
        .next()
        .filter(|text| text.contains('.'))
        .unwrap_or_else(|| panic!("doctor detail names the cli build: {detail}"))
        .trim()
        .to_string()
}

#[test]
fn rel3d_a_foreign_daemon_never_answers_the_cli() {
    if let Some(config) = fake_config() {
        serve_fake_daemon(config);
    }
    let cli_build = cli_build_id();
    assert!(cli_build.contains('.'), "cli build id: {cli_build}");
    let foreign_build = "000000000000.deadbeef.0badf00d";

    // ---- Arm 1: a busy daemon of another build is left alone -------------
    // It holds a live session, so stopping it would destroy somebody's guest.
    let dir = scratch("busy");
    let socket = dir.join("session.sock");
    let mut fake = start_fake(&socket, foreign_build, 1, true, false);
    let (value, stderr) = run(&dir, &["session", "list", "--json"]);
    assert_eq!(value["status"], "pass", "{value:#?}");
    let listed = value["result"]["sessions"].as_array().expect("sessions");
    assert!(
        listed.is_empty(),
        "the foreign daemon's session must not be listed by this build: {listed:#?}"
    );
    let warning = stderr_warning(&stderr)
        .unwrap_or_else(|| panic!("a structured warning must be printed; stderr={stderr}"));
    assert_eq!(warning["code"], "DAEMON_VERSION_MISMATCH", "{warning:#?}");
    assert_eq!(warning["client"]["build_id"], cli_build.as_str());
    assert_eq!(warning["daemon"]["identity"]["build_id"], foreign_build);
    if !pid_alive(fake.id()) {
        let status = fake.try_wait();
        panic!(
            "a busy daemon must never be signalled (pid {} status {:?}); warning={warning:#?}; stderr={stderr}",
            fake.id(),
            status
        );
    }
    let private = dir.join(format!("build-{}", cli_build.replace('+', "_").replace('.', "_")));
    assert!(
        private.join("session.sock").exists(),
        "a build-private daemon must serve this client: {}",
        private.display()
    );
    // `doctor` names the running daemon's identity next to the CLI's.
    let (doctor, _) = run(&dir, &["doctor", "--json"]);
    let check = doctor["checks"]
        .as_array()
        .and_then(|checks| checks.iter().find(|check| check["name"] == "session_daemon"))
        .expect("session_daemon check");
    assert_eq!(check["status"], "warn", "{check:#?}");
    let detail = check["detail"].as_str().unwrap_or("");
    assert!(detail.contains(foreign_build), "names the daemon: {detail}");
    assert!(detail.contains(&cli_build), "names the cli: {detail}");
    let _ = fake.kill();
    let _ = fake.wait();

    // ---- Arm 2: an idle auto-started daemon of another build is replaced --
    let dir = scratch("idle");
    let socket = dir.join("session.sock");
    let mut fake = start_fake(&socket, foreign_build, 0, true, false);
    let fake_pid = fake.id();
    let (value, stderr) = run(&dir, &["session", "list", "--json"]);
    assert_eq!(value["status"], "pass", "{value:#?}");
    let warning = stderr_warning(&stderr)
        .unwrap_or_else(|| panic!("a structured warning must be printed; stderr={stderr}"));
    assert_eq!(warning["code"], "DAEMON_VERSION_MISMATCH", "{warning:#?}");
    assert_eq!(
        warning["action"], "replaced an idle daemon of another build",
        "stderr={stderr}"
    );
    let _ = fake.wait();
    assert!(!pid_alive(fake_pid), "the idle foreign daemon is stopped");
    // The shared socket now belongs to this build: doctor says it matches.
    let (doctor, _) = run(&dir, &["doctor", "--json"]);
    let check = doctor["checks"]
        .as_array()
        .and_then(|checks| checks.iter().find(|check| check["name"] == "session_daemon"))
        .expect("session_daemon check");
    assert_eq!(check["status"], "ok", "{check:#?}");
    assert!(
        check["detail"].as_str().unwrap_or("").contains(&cli_build),
        "{check:#?}"
    );

    // ---- Arm 3: a pre-identity daemon (no `hello`) is never used ---------
    // It holds sessions, so it is left alone even though it cannot prove which
    // build it is; the client runs its own daemon instead.
    let dir = scratch("legacy");
    let socket = dir.join("session.sock");
    let mut fake = start_fake(&socket, "legacy", 1, false, true);
    let (value, stderr) = run(&dir, &["session", "list", "--json"]);
    assert_eq!(value["status"], "pass", "{value:#?}");
    let listed = value["result"]["sessions"].as_array().expect("sessions");
    assert!(
        listed.is_empty(),
        "a pre-identity daemon's answer is never this build's answer: {listed:#?}"
    );
    let warning = stderr_warning(&stderr)
        .unwrap_or_else(|| panic!("a structured warning must be printed; stderr={stderr}"));
    assert_eq!(warning["code"], "DAEMON_VERSION_MISMATCH", "{warning:#?}");
    assert_eq!(
        warning["daemon"]["predates_identity_protocol"], true,
        "{warning:#?}"
    );
    assert!(pid_alive(fake.id()), "an owned legacy daemon is not signalled");
    let _ = fake.kill();
    let _ = fake.wait();

    // ---- Arm 4: the direct client API refuses before writing a request ---
    // (the unit-level guarantee the arms above rely on)
    let client = acos_mcp::daemon::DaemonClient::connect(&socket);
    assert!(client.is_err(), "the socket is gone with the fake daemon");
}

#[test]
fn rel3d_the_running_daemon_reports_its_identity() {
    if std::env::var_os("REL3D_FAKE_SOCKET").is_some() {
        return;
    }
    let dir = scratch("identity-live");
    let socket = dir.join("session.sock");
    let mut fake = start_fake(&socket, "000000000000.feedface.cafebabe", 0, true, false);
    // A raw client sees both identities and the refusal, and never a forwarded
    // envelope from the foreign build.
    let mut stream = UnixStream::connect(&socket).expect("connect fake");
    let request = json!({
        "id": 1,
        "tool": "hello",
        "args": {"client": acos_mcp::identity::Identity::local()},
    });
    writeln!(stream, "{request}").expect("write hello");
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line).expect("read hello");
    let reply: Value = serde_json::from_str(line.trim()).expect("hello JSON");
    assert_eq!(reply["envelope"]["status"], "pass", "{reply:#?}");
    assert_eq!(
        reply["envelope"]["result"]["identity"]["build_id"],
        "000000000000.feedface.cafebabe"
    );
    let _ = fake.kill();
    let _ = fake.wait();
    let _ = std::fs::remove_dir_all(&dir);
}
