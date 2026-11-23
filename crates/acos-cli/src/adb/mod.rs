//! `acos serve --adb` device-panel exposure (docs/DEVICE_PANEL_SPEC.md).
//!
//! Each live session under `acos serve --adb` claims one of the 16 emulator
//! slots (`5554..=5584`), binds `127.0.0.1:<console+1>` with a plain OS thread
//! (zero async runtime, zero polling wakeups when idle), and translates the
//! adb commands used by T3 Code's device hub (`expo-device-hub`) against live
//! session state.

pub mod shell;
pub mod slots;
pub mod wire;

use std::collections::HashMap;
use std::io::ErrorKind;
use std::net::{Shutdown, TcpStream};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use serde_json::{json, Value};

pub use shell::{AdbSessionOps, AdbSessionSnapshot, AdbShellState};
use slots::{claim_slot, lock_dir_from_env, SlotClaim, SlotInfo};

/// Coordinates all adb-exposed sessions for one `acos serve --adb` daemon.
pub struct AdbManager {
    lock_dir: PathBuf,
    sessions: Mutex<HashMap<String, Arc<ExposedSession>>>,
}

impl AdbManager {
    /// Construct an `AdbManager` reading host paths from environment defaults.
    /// Does not touch the filesystem until a session is exposed.
    #[must_use]
    pub fn from_env() -> Self {
        Self {
            lock_dir: lock_dir_from_env(),
            sessions: Mutex::new(HashMap::new()),
        }
    }

    /// Expose a live session over adb, or return `{exposed: false, reason}`
    /// if all 16 adb emulator slots are in use. Re-registering an already
    /// exposed `session_id` returns its existing metadata unchanged.
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

        let pkg = target
            .snapshot()
            .map(|s| s.package)
            .unwrap_or_else(|_| "unknown".to_string());
        let avd_name = format!("ACOS {pkg} {}", short_session_id(session_id));

        let exposed = match ExposedSession::start(claim, avd_name, target) {
            Ok(exp) => Arc::new(exp),
            Err(err) => {
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
    }
}

/// One active adb listener + slot claim bound to a live ACOS session.
pub struct ExposedSession {
    slot: SlotInfo,
    avd_name: String,
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
            "avd_name": self.avd_name,
        })
    }

    pub fn stop(&self) {
        if self.closing.swap(true, Ordering::AcqRel) {
            return;
        }
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

