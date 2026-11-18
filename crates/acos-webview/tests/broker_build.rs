//! WVSCOPE decision 2: a daemon built from **other code** is never allowed to
//! answer this build's sessions.
//!
//! WVTAIL §3 measured the failure this closes: the host-wide endpoint
//! (`<runtime>/acos-wv/broker.sock`) was held by a leftover daemon built from
//! another branch, so a run of the new code was silently measuring the old code.
//! The gate now sends its build id in the attach line (`WIRE.md` §0.1) and the
//! daemon must report the same one:
//!
//! * daemon of another build with **live sessions** → it is left alone (its
//!   engine belongs to somebody) and this build runs its own broker on
//!   `<dir>/broker-<version>-<source8>.sock`;
//! * daemon of another build with **no session** → it stops itself and this
//!   build takes the endpoint over, so no daemon is wasted;
//! * matching build → the shared daemon is used, nothing is spawned.
//!
//! Both daemons here are stubs that speak the attach line and echo the guest's
//! frames, except in the second test where the real `acos-webview --shared`
//! binary is spawned: nothing in this file needs Chromium.

mod common;

use std::io::{BufReader, Read, Write};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use acos_webview::gate::{build_private_endpoint, BrokerSource, Gate, GateConfig};
use acos_webview::wire::{read_frame, write_frame, Frame};
use common::WireGuest;
use serde_json::{json, Value};

fn scratch(tag: &str) -> PathBuf {
    let base = std::env::var_os("ACOS_VIEW_TEST_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join(format!("acos-wvscope-{}", std::process::id())));
    let dir = base.join(format!("g{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch");
    dir
}

fn config(dir: &Path, broker_socket: &Path, broker: BrokerSource, root: &Path) -> GateConfig {
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
        log: Some(root.join("broker.log")),
        spawn_wait: acos_webview::gate::SPAWN_WAIT,
    }
}

/// What a stub daemon answers to the attach line, and how it echoes afterwards.
#[derive(Clone)]
struct StubAttach {
    answer: Value,
    /// Remove the socket and stop listening right after the answer (a daemon
    /// that has just exited).
    then_exit: bool,
    /// What the echo answers say: distinguishes the two daemons in one test.
    tag: &'static str,
}

struct StubDaemon {
    socket: PathBuf,
    stop: Arc<AtomicBool>,
    attaches: Arc<AtomicUsize>,
    frames: Arc<AtomicUsize>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl StubDaemon {
    fn start(socket: &Path, spec: StubAttach) -> Self {
        let _ = std::fs::remove_file(socket);
        std::fs::create_dir_all(socket.parent().unwrap()).expect("socket dir");
        let listener = UnixListener::bind(socket).expect("bind stub daemon");
        listener.set_nonblocking(true).expect("nonblocking");
        let stop = Arc::new(AtomicBool::new(false));
        let attaches = Arc::new(AtomicUsize::new(0));
        let frames = Arc::new(AtomicUsize::new(0));
        let handle = {
            let stop = Arc::clone(&stop);
            let attaches = Arc::clone(&attaches);
            let frames = Arc::clone(&frames);
            let socket = socket.to_path_buf();
            std::thread::spawn(move || {
                while !stop.load(Ordering::SeqCst) {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            let attaches = Arc::clone(&attaches);
                            let frames = Arc::clone(&frames);
                            let spec = spec.clone();
                            let exiting = spec.then_exit;
                            std::thread::spawn(move || {
                                stub_session(stream, &spec, attaches, frames)
                            });
                            if exiting {
                                // A daemon that just said "I have no session and
                                // I am exiting" really goes away: it unbinds, so
                                // a client's next connect is refused.
                                std::thread::sleep(Duration::from_millis(20));
                                drop(listener);
                                let _ = std::fs::remove_file(&socket);
                                return;
                            }
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
}

impl Drop for StubDaemon {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = std::os::unix::net::UnixStream::connect(&self.socket);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
        let _ = std::fs::remove_file(&self.socket);
    }
}

fn stub_session(
    mut stream: std::os::unix::net::UnixStream,
    spec: &StubAttach,
    attaches: Arc<AtomicUsize>,
    frames: Arc<AtomicUsize>,
) {
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
    assert_eq!(attach["attach"], json!(2), "attach v2: {attach}");
    attaches.fetch_add(1, Ordering::SeqCst);
    let mut answer = spec.answer.to_string().into_bytes();
    answer.push(b'\n');
    let _ = stream.write_all(&answer);
    let _ = stream.flush();
    if spec.answer["ok"] != json!(true) {
        return;
    }
    stream
        .set_read_timeout(Some(Duration::from_secs(30)))
        .ok();
    let mut reader = BufReader::new(stream.try_clone().expect("clone"));
    loop {
        match read_frame(&mut reader) {
            Ok(Frame::Json(msg)) => {
                frames.fetch_add(1, Ordering::SeqCst);
                let id = msg["id"].as_str().unwrap_or("g?").to_string();
                let out = json!({"id": id, "ok": true, "result": {"daemon": spec.tag}});
                let bytes = acos_webview::wire::encode_json(&out).expect("encode");
                if write_frame(&mut stream, &bytes).is_err() {
                    return;
                }
            }
            _ => return,
        }
    }
}

fn pid_alive(pid: u32) -> bool {
    pid != 0 && Path::new(&format!("/proc/{pid}")).exists()
}

fn kill(pid: u32) {
    if pid == 0 {
        return;
    }
    // SAFETY: `kill` on a pid this test spawned (a broker of this build).
    unsafe {
        libc_kill(pid as i32, 15);
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    while pid_alive(pid) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    if pid_alive(pid) {
        // SAFETY: as above, after SIGTERM did not do it.
        unsafe {
            libc_kill(pid as i32, 9);
        }
    }
}

unsafe fn libc_kill(pid: i32, signal: i32) {
    // The crate already links libc; a test needs no more than this.
    extern "C" {
        fn kill(pid: i32, sig: i32) -> i32;
    }
    let _ = kill(pid, signal);
}

/// A daemon of another build that **holds a session** is left alone, and this
/// build's gate runs its own broker on its own endpoint — the guest's frames must
/// come from *that* daemon, never from the stranger.
#[test]
fn another_builds_daemon_with_sessions_is_refused_and_this_build_uses_its_own() {
    let root = scratch("busy");
    let canonical = root.join("run/broker.sock");
    let own = build_private_endpoint(&canonical);
    let stale = StubDaemon::start(
        &canonical,
        StubAttach {
            answer: json!({
                "ok": false,
                "code": "BUILD_MISMATCH",
                "build": "0.2.0+g000000000000.ffffffffffffffff",
                "sessions": 1,
                "exiting": false,
            }),
            then_exit: false,
            tag: "stale",
        },
    );
    let mine = StubDaemon::start(
        &own,
        StubAttach {
            answer: json!({"ok": true, "build": acos_webview::build_id()}),
            then_exit: false,
            tag: "this-build",
        },
    );

    let dir = root.join("s");
    let gate = Gate::start(config(&dir, &canonical, BrokerSource::External, &root)).expect("gate");
    let guest = WireGuest::connect(gate.socket());
    let hello = guest.call("hello", json!({"proto": 2})).expect("hello");
    assert_eq!(
        hello["daemon"],
        json!("this-build"),
        "the session must be served by this build's daemon: {hello}"
    );

    let stats = gate.stats();
    assert_eq!(stats.build_mismatches, 1, "{stats:?}");
    assert_eq!(stats.attaches, 1, "{stats:?}");
    assert_eq!(stats.broker_spawns, 0, "nothing to spawn: {stats:?}");
    assert_eq!(
        gate.endpoint(),
        own,
        "a daemon of another build with sessions keeps its endpoint"
    );
    assert_eq!(stale.attaches(), 1, "the stale daemon was asked, once");
    assert_eq!(stale.frames(), 0, "the stale daemon saw no guest frame");
    assert_eq!(mine.attaches(), 1);
    assert!(mine.frames() >= 1);

    guest.close();
    drop(gate);
    drop(stale);
    drop(mine);
    let _ = std::fs::remove_dir_all(&root);
}

/// A daemon of another build with **no session** stops itself, and this build
/// takes the endpoint over with its own real broker — the guest then reaches it
/// through the gate (a `PROTO_VERSION` answer is a full round trip and needs no
/// engine).
#[test]
fn another_builds_idle_daemon_exits_and_this_build_takes_the_endpoint() {
    let root = scratch("idle");
    let canonical = root.join("run/broker.sock");
    let stale = StubDaemon::start(
        &canonical,
        StubAttach {
            answer: json!({
                "ok": false,
                "code": "BUILD_MISMATCH",
                "build": "0.2.0+g000000000000.ffffffffffffffff",
                "sessions": 0,
                "exiting": true,
            }),
            then_exit: true,
            tag: "stale",
        },
    );
    let bin = PathBuf::from(env!("CARGO_BIN_EXE_acos-webview"));
    let dir = root.join("s");
    let gate = Gate::start(config(&dir, &canonical, BrokerSource::Binary(bin), &root)).expect("gate");
    let pid = {
        // The guest's first op is what attaches, spawns and proxies.
        let guest = WireGuest::connect(gate.socket());
        let refusal = guest
            .call("hello", json!({"proto": 99}))
            .expect_err("proto 99 must be refused, not served");
        assert_eq!(refusal["code"], json!("PROTO_VERSION"), "{refusal}");
        assert!(
            refusal["message"]
                .as_str()
                .unwrap_or("")
                .contains("this broker speaks 2"),
            "the broker names the version it speaks: {refusal}"
        );
        let pid = gate.spawned_broker_pid();
        guest.close();
        pid
    };
    let stats = gate.stats();
    assert_eq!(stats.build_mismatches, 1, "{stats:?}");
    assert_eq!(stats.stale_exits, 1, "the stale daemon was asked to go: {stats:?}");
    assert_eq!(stats.broker_spawns, 1, "{stats:?}");
    assert_eq!(
        gate.endpoint(),
        canonical,
        "the endpoint must be taken over in place, not duplicated"
    );
    assert_eq!(stale.attaches(), 1);
    assert!(pid_alive(pid), "this build's broker must be running: {pid}");
    // The log says which build was refused and which one now serves, so an
    // operator reading it does not have to guess.
    let log = std::fs::read_to_string(root.join("broker.log")).unwrap_or_default();
    assert!(
        log.contains("shared broker on"),
        "the spawned broker logged its start: {log}"
    );
    kill(pid);
    drop(gate);
    drop(stale);
    let _ = std::fs::remove_dir_all(&root);
}
