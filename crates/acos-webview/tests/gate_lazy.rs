//! The lazy gate's contract, without Chromium: it starts nothing, it attaches
//! the first connection to the shared daemon, it proxies the wire byte for byte,
//! it keeps WIRE.md §0's one-connection-per-session rule, and it re-attaches
//! after a broker goes away.
//!
//! The "daemon" here is a stub listener in the test: it speaks the gate's attach
//! line and echoes WIRE frames. That is enough to pin everything the gate owns
//! (when a daemon is spawned, what the guest sees when it cannot be reached,
//! and what happens to the session directory), and it keeps this test fast
//! enough to run on every `cargo test -p acos-webview`. The engine itself is
//! covered by `gate_engine.rs` under `ACOS_WEBVIEW_TEST=1`.

mod common;

use std::io::{BufReader, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use acos_webview::gate::{BrokerSource, Gate, GateConfig};
use acos_webview::wire::{read_frame, write_frame, Frame};
use serde_json::{json, Value};

fn scratch(tag: &str) -> PathBuf {
    // Absolute and **short**: a session socket path must stay well inside
    // `sun_path`'s 107 bytes even in a deep worktree, and the broker endpoint a
    // test names must not depend on the daemon's own cwd.
    let base = std::env::var_os("ACOS_VIEW_TEST_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::temp_dir().join(format!("acos-wv5b-{}", std::process::id()))
        });
    let dir = base.join(format!("l{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch");
    dir
}

/// A stub shared daemon: attach handshake, then echo every WIRE frame as an
/// `ok` response with the same id (enough to prove the proxy is byte-faithful
/// and to see the frames the guest actually sent).
struct StubBroker {
    socket: PathBuf,
    stop: Arc<AtomicBool>,
    attaches: Arc<AtomicUsize>,
    seen: Arc<std::sync::Mutex<Vec<Value>>>,
    frames: Arc<AtomicUsize>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl StubBroker {
    fn start(socket: &Path) -> Self {
        let _ = std::fs::remove_file(socket);
        if let Some(parent) = socket.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        let listener = UnixListener::bind(socket).expect("bind stub broker");
        listener.set_nonblocking(true).expect("nonblocking");
        let stop = Arc::new(AtomicBool::new(false));
        let attaches = Arc::new(AtomicUsize::new(0));
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let frames = Arc::new(AtomicUsize::new(0));
        let handle = {
            let stop = Arc::clone(&stop);
            let attaches = Arc::clone(&attaches);
            let seen = Arc::clone(&seen);
            let frames = Arc::clone(&frames);
            std::thread::spawn(move || {
                while !stop.load(Ordering::SeqCst) {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            let attaches = Arc::clone(&attaches);
                            let seen = Arc::clone(&seen);
                            let frames = Arc::clone(&frames);
                            std::thread::spawn(move || {
                                stub_session(stream, attaches, seen, frames)
                            });
                        }
                        Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::sleep(Duration::from_millis(10));
                        }
                        Err(_) => std::thread::sleep(Duration::from_millis(10)),
                    }
                }
            })
        };
        Self {
            socket: socket.to_path_buf(),
            stop,
            attaches,
            seen,
            frames,
            handle: Some(handle),
        }
    }

    fn attaches(&self) -> usize {
        self.attaches.load(Ordering::SeqCst)
    }

    fn frames(&self) -> usize {
        self.frames.load(Ordering::SeqCst)
    }

    fn seen(&self) -> Vec<Value> {
        self.seen.lock().unwrap().clone()
    }
}

impl Drop for StubBroker {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = UnixStream::connect(&self.socket);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
        let _ = std::fs::remove_file(&self.socket);
    }
}

fn stub_session(
    mut stream: UnixStream,
    attaches: Arc<AtomicUsize>,
    seen: Arc<std::sync::Mutex<Vec<Value>>>,
    frames: Arc<AtomicUsize>,
) {
    // The attach line: exactly one, JSON, versioned.
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .expect("timeout");
    let mut line = Vec::new();
    let mut byte = [0u8; 1];
    while line.len() < 4096 {
        match stream.read(&mut byte) {
            Ok(1) if byte[0] == b'\n' => break,
            Ok(1) => line.push(byte[0]),
            _ => return,
        }
    }
    let attach: Value = serde_json::from_slice(&line).expect("attach line is JSON");
    assert_eq!(attach["attach"], json!(1));
    assert!(
        attach["session"].as_str().unwrap_or("").len() > 0,
        "attach must name the session: {attach}"
    );
    attaches.fetch_add(1, Ordering::SeqCst);
    stream.write_all(b"{\"ok\":true}\n").expect("attach ack");
    stream.set_read_timeout(Some(Duration::from_secs(30))).ok();
    // Then echo the wire.
    let mut reader = BufReader::new(stream.try_clone().expect("clone"));
    loop {
        match read_frame(&mut reader) {
            Ok(Frame::Json(msg)) => {
                frames.fetch_add(1, Ordering::SeqCst);
                seen.lock().unwrap().push(msg.clone());
                let id = msg["id"].as_str().unwrap_or("g?").to_string();
                let out = json!({"id": id, "ok": true, "result": {"echo": msg["op"]}});
                let bytes = acos_webview::wire::encode_json(&out).expect("encode");
                if write_frame(&mut stream, &bytes).is_err() {
                    return;
                }
            }
            Ok(Frame::Blob(_)) => return,
            Err(_) => return,
        }
    }
}

fn config(dir: &Path, broker_socket: &Path, broker: BrokerSource) -> GateConfig {
    GateConfig {
        session_dir: dir.to_path_buf(),
        session_id: dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "s".into()),
        broker_socket: broker_socket.to_path_buf(),
        broker,
        engine_cache: None,
        idle_shutdown: None,
        // Outside the session directory, like the launch's `<runtime>/broker.log`:
        // the gate removes the session directory wholesale.
        log: dir.parent().map(|p| p.join("broker.log")),
        spawn_wait: Duration::from_secs(2),
    }
}

/// The gate binds and accepts, and the first connect finds the daemon; the guest
/// frames come back through the proxy unchanged.
#[test]
fn gate_proxies_a_session_to_the_shared_daemon_byte_for_byte() {
    let root = scratch("proxy");
    let broker_socket = root.join("run/broker.sock");
    let stub = StubBroker::start(&broker_socket);
    let dir = root.join("s1");
    let gate = Gate::start(config(&dir, &broker_socket, BrokerSource::External)).expect("gate");
    assert_eq!(gate.socket(), dir.join("webview.sock"));
    assert!(gate.socket().exists(), "the session socket must exist at start");
    assert_eq!(
        gate.stats().broker_spawns,
        0,
        "an existing daemon must not be spawned again"
    );

    let guest = common::WireGuest::connect(gate.socket());
    let hello = guest.hello();
    assert_eq!(hello["echo"], json!("hello"));
    let created = guest
        .call("view.create", json!({"view": 1, "width": 1080, "height": 2400, "density": 2.625}))
        .expect("view.create through the gate");
    assert_eq!(created["echo"], json!("view.create"));

    // The daemon saw the guest's own frames, with the guest's own ids.
    let seen = stub.seen();
    assert!(seen.iter().any(|m| m["op"] == json!("hello")), "{seen:?}");
    assert_eq!(stub.attaches(), 1, "one attach per session connection");
    let stats = gate.stats();
    assert_eq!(stats.connects, 1);
    assert_eq!(stats.attaches, 1);
    guest.close();
    // The proxy's byte counter settles when the stream does (it counts what
    // `io::copy` moved, i.e. after EOF, never a pre-judged estimate).
    let deadline = Instant::now() + Duration::from_secs(10);
    while gate.stats().bytes == 0 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    let stats = gate.stats();
    assert!(stats.bytes > 0, "the proxy must count what it moved: {stats:?}");
    // Session end: the socket and the session directory are gone.
    drop(gate);
    assert!(!dir.join("webview.sock").exists());
    assert!(!dir.exists(), "the per-session directory must be removed");
    drop(stub);
    let _ = std::fs::remove_dir_all(&root);
}

/// A second concurrent connection is refused with WIRE's `ALREADY_CONNECTED`,
/// and a reconnect after the first closes is attached again (WIRE.md §0/§6:
/// `session.closing` → the provider reconnects).
#[test]
fn gate_keeps_one_connection_per_session_and_reattaches_after_it_closes() {
    let root = scratch("one-conn");
    let broker_socket = root.join("run/broker.sock");
    let stub = StubBroker::start(&broker_socket);
    let dir = root.join("s1");
    let gate = Gate::start(config(&dir, &broker_socket, BrokerSource::External)).expect("gate");

    let first = common::WireGuest::connect(gate.socket());
    first.hello();
    // The refused connection is a raw socket: read its single error frame.
    let second = UnixStream::connect(gate.socket()).expect("second connect");
    second
        .set_read_timeout(Some(Duration::from_secs(10)))
        .expect("timeout");
    let mut reader = BufReader::new(second.try_clone().expect("clone"));
    let frame = read_frame(&mut reader).expect("the refusal must be a frame");
    let Frame::Json(msg) = frame else {
        panic!("a blob is not a refusal");
    };
    assert_eq!(msg["ok"], json!(false));
    assert_eq!(msg["error"]["code"], json!("ALREADY_CONNECTED"));
    assert_eq!(gate.stats().refused, 1);

    first.close();
    // Wait for the connection thread to notice the EOF before reconnecting.
    let deadline = Instant::now() + Duration::from_secs(10);
    while gate.attached() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(!gate.attached());
    let again = common::WireGuest::connect(gate.socket());
    again.hello();
    assert_eq!(gate.stats().attaches, 2, "the reconnect must re-attach");
    assert_eq!(stub.attaches(), 2);
    assert!(stub.frames() >= 2);
    again.close();
    drop(gate);
    drop(stub);
    let _ = std::fs::remove_dir_all(&root);
}

/// Nothing is spawned until a guest connects, and when the broker cannot be
/// reached the guest gets `ENGINE_UNAVAILABLE` — never a silent close and never
/// a fabricated success.
#[test]
fn gate_is_lazy_and_reports_engine_unavailable_on_the_wire() {
    let root = scratch("lazy");
    let broker_socket = root.join("run/broker.sock");
    // A "daemon binary" that only proves it was spawned: it is never run
    // before a guest connects, and it never listens.
    let marker = root.join("spawned");
    let script = root.join("fake-broker.sh");
    std::fs::write(
        &script,
        format!("#!/bin/sh\ntouch '{}'\nexit 0\n", marker.display()),
    )
    .expect("script");
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).expect("chmod");

    let dir = root.join("s1");
    let gate = Gate::start(config(&dir, &broker_socket, BrokerSource::Binary(script))).expect("gate");
    // Lazy: a listener and a thread, no daemon, no engine.
    std::thread::sleep(Duration::from_millis(300));
    assert!(!marker.exists(), "the gate must not spawn anything by itself");
    assert!(!broker_socket.exists());
    assert_eq!(gate.stats().broker_spawns, 0);

    // The first connect wakes the spawn path — and since nothing listens, the
    // guest must be told, on the wire, why.
    let stream = UnixStream::connect(gate.socket()).expect("connect the gate");
    stream
        .set_read_timeout(Some(Duration::from_secs(30)))
        .expect("timeout");
    let mut reader = BufReader::new(stream.try_clone().expect("clone"));
    let frame = read_frame(&mut reader).expect("an error frame, not a close");
    let Frame::Json(msg) = frame else {
        panic!("a blob is not an error");
    };
    assert_eq!(msg["ok"], json!(false));
    assert_eq!(msg["error"]["code"], json!("ENGINE_UNAVAILABLE"));
    assert!(
        msg["error"]["message"]
            .as_str()
            .unwrap_or("")
            .contains(&broker_socket.display().to_string()),
        "the error must name the endpoint: {msg}"
    );
    assert!(marker.exists(), "the first connect must try to start the broker");
    assert_eq!(gate.stats().broker_spawns, 1);
    // A second connect tries again (the daemon may have needed time), but the
    // report stays honest: it did spawn once per attempt, and the guest still
    // gets a structured failure.
    drop(reader);
    drop(stream);
    drop(gate);
    assert!(!dir.exists());
    let _ = std::fs::remove_dir_all(&root);
}
