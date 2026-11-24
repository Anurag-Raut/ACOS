//! `acos serve --adb` device-panel exposure (docs/DEVICE_PANEL_SPEC.md).
//!
//! Each live session under `acos serve --adb` claims one of the 16 emulator
//! slots (`5554..=5584`), binds `127.0.0.1:<console+1>` with a plain OS thread
//! (zero async runtime, zero polling wakeups when idle), and translates the
//! adb commands used by T3 Code's device hub (`expo-device-hub`) against live
//! session state.

pub mod bridge;
pub mod shell;
pub mod slots;
pub mod verify_view;
pub mod wire;

use std::collections::HashMap;
use std::io::ErrorKind;
use std::net::{Shutdown, TcpStream};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU16, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use serde_json::{json, Value};

use bridge::{
    avd_running_dir_from_env, random_bearer_token, sweep_stale_acos_ini, write_ini_file,
    BridgeHandle, IniSpec,
};
pub use shell::{AdbSessionOps, AdbSessionSnapshot, AdbShellState};
use slots::{claim_slot, lock_dir_from_env, SlotClaim, SlotInfo};

/// Coordinates all adb-exposed sessions for one `acos serve --adb` daemon.
pub struct AdbManager {
    lock_dir: PathBuf,
    avd_dir: PathBuf,
    bridge: BridgeHandle,
    sessions: Mutex<HashMap<String, Arc<ExposedSession>>>,
}

impl AdbManager {
    /// Construct an `AdbManager` reading host paths from environment defaults
    /// and sweeping any stale ACOS `pid_*.ini` left behind by a dead process.
    #[must_use]
    pub fn from_env() -> Self {
        let avd_dir = avd_running_dir_from_env();
        sweep_stale_acos_ini(&avd_dir);
        Self {
            lock_dir: lock_dir_from_env(),
            avd_dir,
            bridge: BridgeHandle::new(),
            sessions: Mutex::new(HashMap::new()),
        }
    }

    #[cfg(test)]
    pub(crate) fn with_test_dirs(lock_dir: PathBuf, avd_dir: PathBuf, bridge_bin: PathBuf) -> Self {
        Self {
            lock_dir,
            avd_dir,
            bridge: BridgeHandle::with_bin(bridge_bin),
            sessions: Mutex::new(HashMap::new()),
        }
    }

    /// Expose a live session over adb and gRPC (`acos-devbridge`), or return
    /// `{exposed: false, reason}` if all 16 adb emulator slots are in use.
    /// Re-registering an already exposed `session_id` returns its existing
    /// metadata unchanged.
    pub fn register(&self, session_id: &str, target: Arc<dyn AdbSessionOps>) -> Value {
        {
            let guard = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(existing) = guard.get(session_id) {
                return existing.device_panel_json();
            }
        }

        let claim = match claim_slot(&self.lock_dir) {
            Ok(Some(claim)) => claim,
            Ok(None) => {
                return json!({
                    "exposed": false,
                    "reason": "all 16 adb emulator slots in use",
                });
            }
            Err(err) => {
                return json!({
                    "exposed": false,
                    "reason": format!("cannot allocate adb slot: {err}"),
                });
            }
        };

        let snap = target.snapshot();
        let (pkg, ini_pid) = match &snap {
            Ok(s) => (
                s.package.clone(),
                s.launcher_pid.unwrap_or_else(std::process::id),
            ),
            Err(_) => ("unknown".to_string(), std::process::id()),
        };
        let avd_name = format!("ACOS {pkg} {}", short_session_id(session_id));
        let token = random_bearer_token();
        let grpc_port_ref = Arc::new(AtomicU16::new(0));
        let ini_spec = IniSpec {
            avd_dir: self.avd_dir.clone(),
            ini_pid,
            console_port: claim.info.console_port,
            adb_port: claim.info.adb_port,
            grpc_port: Arc::clone(&grpc_port_ref),
        };

        let reg = match self.bridge.register_session(
            session_id,
            &avd_name,
            &token,
            Arc::clone(&target),
            Some(ini_spec),
        ) {
            Ok(reg) => reg,
            Err(err) => {
                return json!({
                    "exposed": false,
                    "reason": format!("cannot start gRPC bridge: {err}"),
                });
            }
        };
        if !reg.pruned_sessions.is_empty() {
            let dead: Vec<Arc<ExposedSession>> = {
                let mut guard = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
                reg.pruned_sessions
                    .iter()
                    .filter_map(|id| guard.remove(id))
                    .collect()
            };
            for exp in dead {
                exp.stop();
            }
        }
        let grpc_port = reg.grpc_port;

        let ini_path = match write_ini_file(
            &self.avd_dir,
            ini_pid,
            claim.info.console_port,
            claim.info.adb_port,
            grpc_port,
            &token,
            &avd_name,
        ) {
            Ok(p) => p,
            Err(err) => {
                self.bridge.unregister_session(session_id);
                return json!({
                    "exposed": false,
                    "reason": format!("cannot write discovery ini: {err}"),
                });
            }
        };

        let exposed = match ExposedSession::start(
            claim,
            avd_name,
            grpc_port_ref,
            ini_path.clone(),
            target,
        ) {
            Ok(exp) => Arc::new(exp),
            Err(err) => {
                let _ = std::fs::remove_file(&ini_path);
                self.bridge.unregister_session(session_id);
                return json!({
                    "exposed": false,
                    "reason": format!("cannot start adb listener: {err}"),
                });
            }
        };

        let json_info = exposed.device_panel_json();
        let adb_port = exposed.slot.adb_port;
        self.sessions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(session_id.to_string(), exposed);
        notify_adb_server(adb_port);
        json_info
    }

    /// Return the current `device_panel` JSON for an already registered session
    /// (used on re-launch), or `None` if not registered.
    pub fn info(&self, session_id: &str) -> Option<Value> {
        let guard = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
        guard.get(session_id).map(|s| s.device_panel_json())
    }

    /// Unexpose one session (called on `acos_destroy` / dead-session sweep).
    pub fn unregister(&self, session_id: &str) {
        let removed = self
            .sessions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(session_id);
        if let Some(exposed) = removed {
            exposed.stop();
        }
        self.bridge.unregister_session(session_id);
    }

    /// Unexpose every session (called on daemon shutdown).
    pub fn shutdown_all(&self) {
        let all: Vec<Arc<ExposedSession>> = {
            let mut guard = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
            guard.drain().map(|(_, v)| v).collect()
        };
        for exposed in all {
            exposed.stop();
        }
        self.bridge.shutdown();
    }
}

/// One active adb listener + slot claim + discovery ini bound to a live ACOS session.
pub struct ExposedSession {
    slot: SlotInfo,
    avd_name: String,
    grpc_port: Arc<AtomicU16>,
    ini_path: PathBuf,
    closing: Arc<AtomicBool>,
    stop_event_fd: OwnedFd,
    active_conns: Arc<Mutex<Vec<TcpStream>>>,
    accept_thread: Mutex<Option<JoinHandle<()>>>,
    slot_claim: Mutex<Option<SlotClaim>>,
}

impl ExposedSession {
    fn start(
        claim: SlotClaim,
        avd_name: String,
        grpc_port: Arc<AtomicU16>,
        ini_path: PathBuf,
        target: Arc<dyn AdbSessionOps>,
    ) -> std::io::Result<Self> {
        let efd_raw = unsafe { libc::eventfd(0, libc::EFD_CLOEXEC | libc::EFD_NONBLOCK) };
        if efd_raw < 0 {
            return Err(std::io::Error::last_os_error());
        }
        let stop_event_fd = unsafe { OwnedFd::from_raw_fd(efd_raw) };
        let stop_raw_fd = stop_event_fd.as_raw_fd();

        let slot = claim.info.clone();
        let closing = Arc::new(AtomicBool::new(false));
        let active_conns: Arc<Mutex<Vec<TcpStream>>> = Arc::new(Mutex::new(Vec::new()));
        let shell_state = Arc::new(Mutex::new(AdbShellState::default()));

        let listener = claim.listener.try_clone()?;
        let closing_bg = Arc::clone(&closing);
        let conns_bg = Arc::clone(&active_conns);

        let handle = std::thread::Builder::new()
            .name(format!("acos-adb-{}", slot.console_port))
            .spawn(move || {
                accept_loop(
                    listener,
                    stop_raw_fd,
                    closing_bg,
                    target,
                    shell_state,
                    conns_bg,
                );
            })?;

        Ok(Self {
            slot,
            avd_name,
            grpc_port,
            ini_path,
            closing,
            stop_event_fd,
            active_conns,
            accept_thread: Mutex::new(Some(handle)),
            slot_claim: Mutex::new(Some(claim)),
        })
    }

    pub fn device_panel_json(&self) -> Value {
        json!({
            "exposed": true,
            "serial": self.slot.serial,
            "console_port": self.slot.console_port,
            "adb_port": self.slot.adb_port,
            "grpc_port": self.grpc_port.load(Ordering::Acquire),
            "avd_name": self.avd_name,
        })
    }

    pub fn stop(&self) {
        if self.closing.swap(true, Ordering::AcqRel) {
            return;
        }
        let _ = std::fs::remove_file(&self.ini_path);
        // Wake the accept loop poll() with zero delay.
        let one: u64 = 1;
        unsafe {
            libc::write(
                self.stop_event_fd.as_raw_fd(),
                (&one as *const u64).cast::<libc::c_void>(),
                std::mem::size_of::<u64>(),
            );
        }
        // Shut down active TCP connections so adb server drops the transport immediately.
        {
            let mut conns = self.active_conns.lock().unwrap_or_else(|e| e.into_inner());
            for conn in conns.drain(..) {
                let _ = conn.shutdown(Shutdown::Both);
            }
        }
        if let Some(handle) = self
            .accept_thread
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
        {
            let _ = handle.join();
        }
        // Drop listener and release flock last so no one else claims the port before exit.
        let _ = self
            .slot_claim
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
    }
}

impl Drop for ExposedSession {
    fn drop(&mut self) {
        self.stop();
    }
}

fn accept_loop(
    listener: std::net::TcpListener,
    stop_fd: i32,
    closing: Arc<AtomicBool>,
    target: Arc<dyn AdbSessionOps>,
    shell_state: Arc<Mutex<AdbShellState>>,
    active_conns: Arc<Mutex<Vec<TcpStream>>>,
) {
    let listener_fd = listener.as_raw_fd();
    while !closing.load(Ordering::Acquire) {
        match listener.accept() {
            Ok((stream, _)) => {
                if closing.load(Ordering::Acquire) {
                    let _ = stream.shutdown(Shutdown::Both);
                    break;
                }
                if let Ok(cloned) = stream.try_clone() {
                    let mut guard = active_conns.lock().unwrap_or_else(|e| e.into_inner());
                    // Prune disconnected placeholders isn't trivial without probing,
                    // but an adb server keeps at most 1-2 connections per device.
                    if guard.len() > 32 {
                        guard.clear();
                    }
                    guard.push(cloned);
                }
                let target = Arc::clone(&target);
                let shell_state = Arc::clone(&shell_state);
                let closing_conn = Arc::clone(&closing);
                std::thread::spawn(move || {
                    wire::handle_connection(stream, target, shell_state, closing_conn);
                });
            }
            Err(e) if e.kind() == ErrorKind::WouldBlock => {
                let mut pfds = [
                    libc::pollfd {
                        fd: listener_fd,
                        events: libc::POLLIN,
                        revents: 0,
                    },
                    libc::pollfd {
                        fd: stop_fd,
                        events: libc::POLLIN,
                        revents: 0,
                    },
                ];
                // Block in kernel with infinite timeout: 0 wakeups when idle.
                let rc = unsafe { libc::poll(pfds.as_mut_ptr(), 2, -1) };
                if rc < 0 && std::io::Error::last_os_error().kind() != ErrorKind::Interrupted {
                    break;
                }
                if (pfds[1].revents & libc::POLLIN) != 0 {
                    break;
                }
            }
            Err(_) => break,
        }
    }
}

fn short_session_id(id: &str) -> String {
    if id.chars().count() <= 12 {
        id.to_string()
    } else {
        id.chars().take(8).collect()
    }
}

/// Best-effort notification to an already-running adb server (`$ANDROID_ADB_SERVER_PORT`
/// or `5037`) so newly exposed sessions register immediately as `emulator-<console>`
/// instead of waiting for a server restart. Never spawns adb if none is running.
fn notify_adb_server(adb_port: u16) {
    use std::io::Write as _;
    use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
    use std::time::Duration;

    let server_port = std::env::var("ANDROID_ADB_SERVER_PORT")
        .ok()
        .and_then(|v| v.parse::<u16>().ok())
        .filter(|&p| p > 0)
        .unwrap_or(5037);
    let addr = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, server_port));
    let Ok(mut stream) = TcpStream::connect_timeout(&addr, Duration::from_millis(100)) else {
        return;
    };
    let _ = stream.set_write_timeout(Some(Duration::from_millis(100)));
    let _ = stream.set_read_timeout(Some(Duration::from_millis(100)));
    let msg = format!("host:emulator:{adb_port}");
    let packet = format!("{:04x}{msg}", msg.len());
    let _ = stream.write_all(packet.as_bytes());
    let _ = stream.flush();
}

#[cfg(test)]
mod tests {
    use super::*;
    use acos_protocol::ui::UiTree;
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;

    struct FakeOps {
        pid: u32,
        alive: Arc<AtomicBool>,
    }

    impl FakeOps {
        fn new(pid: u32, alive: Arc<AtomicBool>) -> Arc<Self> {
            Arc::new(Self { pid, alive })
        }
    }

    impl AdbSessionOps for FakeOps {
        fn snapshot(&self) -> Result<AdbSessionSnapshot, String> {
            if !self.alive.load(Ordering::Acquire) {
                return Err("session died".to_string());
            }
            Ok(AdbSessionSnapshot {
                package: "dev.acos.test".to_string(),
                activity: None,
                natural_size: (1080, 2400),
                display_size: (1080, 2400),
                rotation: 0,
                density: 420,
                animations: "on",
                guest_pid: Some(self.pid),
                launcher_pid: Some(self.pid),
                apk_path: PathBuf::from("/tmp/fake.apk"),
            })
        }
        fn rotate(&self, _: u8) -> Result<(), String> {
            Ok(())
        }
        fn dump_tree(&self) -> Result<(UiTree, u8), String> {
            Err("unused".into())
        }
        fn screenshot_png(&self) -> Result<Vec<u8>, String> {
            Err("unused".into())
        }
        fn raw_screenshot(&self, _: &Path, _: &str) -> Result<(u32, u32, u8), String> {
            Err("unused".into())
        }
        fn panel_touch(&self, _: i32, _: i64, _: i64, _: &[(i32, f32, f32)]) -> Result<(), String> {
            Ok(())
        }
        fn panel_key(&self, _: i32, _: &str, _: i32, _: i32) -> Result<(), String> {
            Ok(())
        }
        fn panel_text(&self, _: &str) -> Result<(), String> {
            Ok(())
        }
        fn stream_socket(&self) -> Option<PathBuf> {
            None
        }
        fn is_verify_run(&self) -> bool {
            false
        }
    }

    #[test]
    fn dead_bridge_respawn_reregisters_surviving_sessions_and_updates_ini() {
        let base = std::env::temp_dir().join(format!(
            "acos-cr5-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();

        let state_dir = base.join("bridge-state");
        std::fs::create_dir_all(&state_dir).unwrap();
        let script = base.join("fake-bridge.py");
        let py = format!(
            r#"#!/usr/bin/env python3
import json, os, socket, sys

args = sys.argv[1:]
sock_path = args[args.index("--socket") + 1]
state_dir = {state_dir:?}
seq_file = os.path.join(state_dir, "seq")
try:
    with open(seq_file, "r") as f:
        inst = int(f.read().strip()) + 1
except FileNotFoundError:
    inst = 1
with open(seq_file, "w") as f:
    f.write(str(inst))
with open(os.path.join(state_dir, f"pid-{{inst}}"), "w") as f:
    f.write(str(os.getpid()))
log_path = os.path.join(state_dir, f"reg-{{inst}}.jsonl")

srv = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
srv.bind(sock_path)
srv.listen(32)
next_port = inst * 10000 + 100
while True:
    conn, _ = srv.accept()
    with conn:
        buf = b""
        while b"\n" not in buf:
            chunk = conn.recv(4096)
            if not chunk:
                break
            buf += chunk
        if not buf:
            continue
        req = json.loads(buf.decode().splitlines()[0])
        cmd = req.get("cmd")
        if cmd == "register":
            port = next_port
            next_port += 1
            with open(log_path, "a") as lf:
                lf.write(json.dumps({{"session_id": req["session_id"], "port": port}}) + "\n")
            conn.sendall((json.dumps({{"ok": True, "grpc_port": port}}) + "\n").encode())
        elif cmd == "unregister":
            conn.sendall((json.dumps({{"ok": True}}) + "\n").encode())
        elif cmd == "shutdown":
            conn.sendall((json.dumps({{"ok": True}}) + "\n").encode())
            break
        else:
            conn.sendall((json.dumps({{"ok": False, "error": "unknown"}}) + "\n").encode())
"#,
            state_dir = state_dir.to_string_lossy()
        );
        std::fs::write(&script, py).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

        let lock_dir = base.join("locks");
        let avd_dir = base.join("avd");
        let mgr = AdbManager::with_test_dirs(lock_dir, avd_dir.clone(), script);

        // Register surviving session A and a soon-to-die session D while bridge 1 is alive.
        let pid_a = 70001;
        let pid_d = 70002;
        let alive_a = Arc::new(AtomicBool::new(true));
        let alive_d = Arc::new(AtomicBool::new(true));
        let info_a = mgr.register("sess-a", FakeOps::new(pid_a, Arc::clone(&alive_a)));
        assert_eq!(info_a["exposed"], json!(true), "{info_a:#?}");
        let old_port_a = info_a["grpc_port"].as_u64().unwrap() as u16;
        let info_d = mgr.register("sess-d", FakeOps::new(pid_d, Arc::clone(&alive_d)));
        assert_eq!(info_d["exposed"], json!(true), "{info_d:#?}");

        // Session D dies, and bridge 1 is SIGKILLed out-of-band.
        alive_d.store(false, Ordering::Release);
        let pid1: i32 = std::fs::read_to_string(state_dir.join("pid-1"))
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        unsafe {
            libc::kill(pid1, libc::SIGKILL);
        }
        std::thread::sleep(std::time::Duration::from_millis(80));

        // Register session B — forces bridge respawn as instance 2.
        let alive_b = Arc::new(AtomicBool::new(true));
        let info_b = mgr.register("sess-b", FakeOps::new(70003, alive_b));
        assert_eq!(info_b["exposed"], json!(true), "{info_b:#?}");

        // Check instance 2 received re-registration for sess-a (NOT sess-d) plus sess-b.
        let reg2 = std::fs::read_to_string(state_dir.join("reg-2.jsonl")).unwrap();
        let entries2: Vec<Value> = reg2
            .lines()
            .map(|l| serde_json::from_str::<Value>(l).unwrap())
            .collect();
        let a_entry2 = entries2
            .iter()
            .find(|v| v["session_id"] == json!("sess-a"))
            .unwrap_or_else(|| panic!("bridge 2 did not re-register sess-a:\n{reg2}"));
        assert!(
            !entries2.iter().any(|v| v["session_id"] == json!("sess-d")),
            "dead sess-d should be pruned, not re-registered:\n{reg2}"
        );

        let new_port_a = a_entry2["port"].as_u64().unwrap() as u16;
        assert_ne!(new_port_a, old_port_a);

        // ExposedSession & discovery ini for sess-a must reflect the new gRPC port.
        let refreshed_a = mgr.info("sess-a").expect("sess-a still registered");
        assert_eq!(refreshed_a["grpc_port"], json!(new_port_a));
        let ini_a = std::fs::read_to_string(avd_dir.join(format!("pid_{pid_a}.ini"))).unwrap();
        assert!(
            ini_a.contains(&format!("grpc.port={new_port_a}\n")),
            "ini_a has stale port:\n{ini_a}"
        );
        assert!(
            !avd_dir.join(format!("pid_{pid_d}.ini")).exists(),
            "dead session ini must be removed on prune"
        );
        assert!(
            mgr.info("sess-d").is_none(),
            "dead session should be removed from AdbManager::sessions"
        );

        mgr.shutdown_all();
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn bridge_survives_spawning_worker_thread_exit() {
        let base = std::env::temp_dir().join(format!("acos-cr1-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();

        let state_dir = base.join("bridge-state");
        std::fs::create_dir_all(&state_dir).unwrap();
        let script = base.join("fake-bridge.py");
        let py = format!(
            r#"#!/usr/bin/env python3
import json, os, signal, socket, sys

signal.signal(signal.SIGTERM, lambda *_: sys.exit(0))
args = sys.argv[1:]
sock_path = args[args.index("--socket") + 1]
state_dir = {state_dir:?}
with open(os.path.join(state_dir, "pid"), "w") as f:
    f.write(str(os.getpid()))

srv = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
srv.bind(sock_path)
srv.listen(32)
port = 41000
while True:
    conn, _ = srv.accept()
    with conn:
        buf = b""
        while b"\n" not in buf:
            chunk = conn.recv(4096)
            if not chunk:
                break
            buf += chunk
        if not buf:
            continue
        req = json.loads(buf.decode().splitlines()[0])
        cmd = req.get("cmd")
        if cmd == "register":
            port += 1
            conn.sendall((json.dumps({{"ok": True, "grpc_port": port}}) + "\n").encode())
        elif cmd == "unregister":
            conn.sendall((json.dumps({{"ok": True}}) + "\n").encode())
        elif cmd == "shutdown":
            conn.sendall((json.dumps({{"ok": True}}) + "\n").encode())
            break
"#,
            state_dir = state_dir.to_string_lossy()
        );
        std::fs::write(&script, py).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

        let mgr = Arc::new(AdbManager::with_test_dirs(
            base.join("locks"),
            base.join("avd"),
            script,
        ));

        // Spawn BridgeProcess on an ephemeral worker thread (mimicking acos serve's
        // per-connection worker thread) and let that thread exit.
        let mgr_bg = Arc::clone(&mgr);
        let worker = std::thread::spawn(move || {
            let alive = Arc::new(AtomicBool::new(true));
            let info = mgr_bg.register("sess-worker", FakeOps::new(71001, alive));
            assert_eq!(info["exposed"], json!(true), "{info:#?}");
        });
        worker.join().expect("worker joined");

        // Give the kernel time to deliver PR_SET_PDEATHSIG if the bridge was
        // bound to the exiting worker thread instead of the process-lifetime spawner.
        std::thread::sleep(std::time::Duration::from_millis(250));

        let pid_str = std::fs::read_to_string(state_dir.join("pid")).unwrap();
        let bridge_pid: i32 = pid_str.trim().parse().unwrap();

        // Register a second session and verify bridge PID did NOT die / respawn.
        let alive2 = Arc::new(AtomicBool::new(true));
        let info2 = mgr.register("sess-2", FakeOps::new(71002, alive2));
        assert_eq!(info2["exposed"], json!(true), "{info2:#?}");
        let pid_after: i32 = std::fs::read_to_string(state_dir.join("pid"))
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        assert_eq!(
            pid_after, bridge_pid,
            "bridge died when spawning worker thread exited (before={bridge_pid}, after={pid_after})"
        );

        mgr.shutdown_all();
        let _ = std::fs::remove_dir_all(&base);
    }
}

