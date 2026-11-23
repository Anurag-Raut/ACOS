//! Host-side bridge coordinator (`acos serve --adb` <-> `acos-devbridge`)
//! and AVD `pid_<pid>.ini` discovery files (§4.1, §4.3, §4.4 of
//! docs/DEVICE_PANEL_SPEC.md).

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, ErrorKind, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use super::shell::AdbSessionOps;

/// Env var overriding `$HOME/.android/avd/running` (for isolated tests).
pub const AVD_RUNNING_DIR_ENV: &str = "ACOS_AVD_RUNNING_DIR";
/// Env var overriding the `acos-devbridge` binary path.
pub const DEVBRIDGE_BIN_ENV: &str = "ACOS_DEVBRIDGE_BIN";

/// Resolve the directory where `pid_<pid>.ini` discovery files are written.
#[must_use]
pub fn avd_running_dir_from_env() -> PathBuf {
    if let Some(val) = std::env::var_os(AVD_RUNNING_DIR_ENV) {
        if !val.is_empty() {
            return PathBuf::from(val);
        }
    }
    if let Some(home) = std::env::var_os("HOME") {
        if !home.is_empty() {
            return PathBuf::from(home)
                .join(".android")
                .join("avd")
                .join("running");
        }
    }
    std::env::temp_dir()
        .join(".android")
        .join("avd")
        .join("running")
}

/// Remove stale ACOS `pid_<pid>.ini` files in `dir` whose owning pid is dead
/// (`kill(pid, 0)` -> `ESRCH`) AND whose `avd.name` starts with `ACOS `.
/// Never touches real Android emulator ini files.
pub fn sweep_stale_acos_ini(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let Some(rest) = name.strip_prefix("pid_") else {
            continue;
        };
        let Some(pid_str) = rest.strip_suffix(".ini") else {
            continue;
        };
        let Ok(pid) = pid_str.parse::<i32>() else {
            continue;
        };
        if pid <= 0 {
            continue;
        }
        let rc = unsafe { libc::kill(pid, 0) };
        let dead = if rc == 0 {
            false
        } else {
            std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
        };
        if !dead {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let is_acos = text
            .lines()
            .any(|line| line.trim_start().starts_with("avd.name=ACOS "));
        if is_acos {
            let _ = std::fs::remove_file(&path);
        }
    }
}

/// Atomically write `pid_<pid>.ini` in `dir` (mode `0600` in `0700` dir).
pub fn write_ini_file(
    dir: &Path,
    pid: u32,
    console_port: u16,
    adb_port: u16,
    grpc_port: u16,
    token: &str,
    avd_name: &str,
) -> std::io::Result<PathBuf> {
    prepare_private_dir(dir)?;
    let ini_path = dir.join(format!("pid_{pid}.ini"));
    let tmp_path = dir.join(format!(".pid_{pid}.ini.tmp.{}", std::process::id()));
    let content = format!(
        "port.serial={console_port}\n\
         port.adb={adb_port}\n\
         grpc.port={grpc_port}\n\
         grpc.token={token}\n\
         avd.name={avd_name}\n"
    );
    {
        let mut file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp_path)?;
        file.write_all(content.as_bytes())?;
        file.flush()?;
    }
    std::fs::rename(&tmp_path, &ini_path)?;
    Ok(ini_path)
}

/// Generate a random 128-bit lowercase hex bearer token.
#[must_use]
pub fn random_bearer_token() -> String {
    let mut buf = [0u8; 16];
    let ok = File::open("/dev/urandom")
        .and_then(|mut f| std::io::Read::read_exact(&mut f, &mut buf))
        .is_ok();
    if !ok {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let pid = u128::from(std::process::id());
        let val = now ^ (pid << 64);
        buf = val.to_le_bytes();
    }
    let mut hex = String::with_capacity(32);
    for b in buf {
        use std::fmt::Write as _;
        let _ = write!(hex, "{b:02x}");
    }
    hex
}

fn prepare_private_dir(dir: &Path) -> std::io::Result<()> {
    if let Some(parent) = dir.parent() {
        std::fs::create_dir_all(parent)?;
    }
    match std::fs::DirBuilder::new().mode(0o700).create(dir) {
        Ok(()) => {}
        Err(e) if e.kind() == ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e),
    }
    let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
    Ok(())
}

fn resolve_devbridge_bin() -> PathBuf {
    if let Some(val) = std::env::var_os(DEVBRIDGE_BIN_ENV) {
        if !val.is_empty() {
            return PathBuf::from(val);
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        let sibling = exe.with_file_name("acos-devbridge");
        if sibling.is_file() {
            return sibling;
        }
    }
    PathBuf::from("acos-devbridge")
}

fn bridge_socket_dir() -> std::io::Result<PathBuf> {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let base = std::env::var_os("XDG_RUNTIME_DIR")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .filter(|p| p.is_dir() && p.as_os_str().len() <= 64)
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    let dir = base.join(format!("acos-b-{}-{seq}", std::process::id()));
    prepare_private_dir(&dir)?;
    Ok(dir)
}

/// Manages the lazily-spawned `acos-devbridge` process and its `serve-cb.sock`
/// callback listener for one `acos serve --adb` daemon.
pub struct BridgeHandle {
    inner: Mutex<Option<BridgeProcess>>,
    targets: Arc<Mutex<HashMap<String, Arc<dyn AdbSessionOps>>>>,
}

struct BridgeProcess {
    runtime_dir: PathBuf,
    bridge_sock: PathBuf,
    child: Child,
    closing: Arc<AtomicBool>,
    stop_event_fd: OwnedFd,
    cb_thread: Option<JoinHandle<()>>,
}

impl BridgeHandle {
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(None),
            targets: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Lazily start `acos-devbridge` (if not already running) and register a
    /// session gRPC endpoint, returning the bound `127.0.0.1:<grpc_port>`.
    pub fn register_session(
        &self,
        session_id: &str,
        avd_name: &str,
        token: &str,
        target: Arc<dyn AdbSessionOps>,
    ) -> Result<u16, String> {
        let snap = target.snapshot()?;
        let stream_socket = target.stream_socket().map(|p| p.display().to_string());
        let verify = target.is_verify_run();

        self.targets
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(session_id.to_string(), target);

        let bridge_sock = {
            let mut guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(proc) = guard.as_mut() {
                if let Ok(Some(_)) = proc.child.try_wait() {
                    proc.stop();
                    *guard = None;
                }
            }
            if guard.is_none() {
                match BridgeProcess::spawn(Arc::clone(&self.targets)) {
                    Ok(proc) => *guard = Some(proc),
                    Err(err) => {
                        self.targets
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .remove(session_id);
                        return Err(err);
                    }
                }
            }
            guard.as_ref().unwrap().bridge_sock.clone()
        };

        let resp = bridge_rpc(
            &bridge_sock,
            &json!({
                "cmd": "register",
                "session_id": session_id,
                "avd_name": avd_name,
                "token": token,
                "display": [snap.display_size.0, snap.display_size.1],
                "rotation": snap.rotation,
                "verify": verify,
                "stream_socket": stream_socket,
            }),
        );
        let resp = match resp {
            Ok(v) => v,
            Err(e) => {
                self.targets
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .remove(session_id);
                return Err(e);
            }
        };
        if resp.get("ok").and_then(Value::as_bool) != Some(true) {
            self.targets
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(session_id);
            return Err(resp
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("acos-devbridge register failed")
                .to_string());
        }
        let port = resp
            .get("grpc_port")
            .and_then(Value::as_u64)
            .and_then(|p| u16::try_from(p).ok())
            .filter(|&p| p > 0)
            .ok_or_else(|| "acos-devbridge returned invalid grpc_port".to_string())?;
        Ok(port)
    }

    pub fn unregister_session(&self, session_id: &str) {
        self.targets
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(session_id);
        let bridge_sock = {
            let guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            guard.as_ref().map(|p| p.bridge_sock.clone())
        };
        if let Some(sock) = bridge_sock {
            let _ = bridge_rpc(
                &sock,
                &json!({
                    "cmd": "unregister",
                    "session_id": session_id,
                }),
            );
        }
    }

    pub fn shutdown(&self) {
        self.targets
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        let proc = self.inner.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(mut p) = proc {
            p.stop();
        }
    }
}

impl Drop for BridgeHandle {
    fn drop(&mut self) {
        self.shutdown();
    }
}

impl BridgeProcess {
    fn spawn(
        targets: Arc<Mutex<HashMap<String, Arc<dyn AdbSessionOps>>>>,
    ) -> Result<Self, String> {
        let runtime_dir =
            bridge_socket_dir().map_err(|e| format!("create bridge runtime dir: {e}"))?;
        let cb_sock = runtime_dir.join("serve-cb.sock");
        let bridge_sock = runtime_dir.join("bridge.sock");

        if cb_sock.exists() {
            let _ = std::fs::remove_file(&cb_sock);
        }
        let listener = UnixListener::bind(&cb_sock)
            .map_err(|e| format!("bind callback socket {}: {e}", cb_sock.display()))?;
        listener
            .set_nonblocking(true)
            .map_err(|e| format!("set callback nonblocking: {e}"))?;
        let _ = std::fs::set_permissions(&cb_sock, std::fs::Permissions::from_mode(0o600));

        let efd_raw = unsafe { libc::eventfd(0, libc::EFD_CLOEXEC | libc::EFD_NONBLOCK) };
        if efd_raw < 0 {
            return Err(format!(
                "create callback eventfd: {}",
                std::io::Error::last_os_error()
            ));
        }
        let stop_event_fd = unsafe { OwnedFd::from_raw_fd(efd_raw) };
        let stop_raw_fd = stop_event_fd.as_raw_fd();
        let closing = Arc::new(AtomicBool::new(false));
        let closing_bg = Arc::clone(&closing);

        let cb_thread = std::thread::Builder::new()
            .name("acos-devbridge-cb".to_string())
            .spawn(move || {
                callback_accept_loop(listener, stop_raw_fd, closing_bg, targets);
            })
            .map_err(|e| format!("spawn callback thread: {e}"))?;

        let bin = resolve_devbridge_bin();
        let parent_pid = unsafe { libc::getpid() };
        let mut cmd = Command::new(&bin);
        cmd.arg("--socket")
            .arg(&bridge_sock)
            .arg("--callback")
            .arg(&cb_sock)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        unsafe {
            cmd.pre_exec(move || {
                if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                if libc::getppid() != parent_pid {
                    libc::_exit(1);
                }
                Ok(())
            });
        }

        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                if !closing.swap(true, Ordering::AcqRel) {
                    let one: u64 = 1;
                    unsafe {
                        libc::write(
                            stop_event_fd.as_raw_fd(),
                            (&one as *const u64).cast::<libc::c_void>(),
                            std::mem::size_of::<u64>(),
                        );
                    }
                }
                let _ = cb_thread.join();
                let _ = std::fs::remove_dir_all(&runtime_dir);
                return Err(format!("spawn {}: {e}", bin.display()));
            }
        };

        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if let Ok(Some(status)) = child.try_wait() {
                let mut stderr_buf = String::new();
                if let Some(mut se) = child.stderr.take() {
                    let _ = std::io::Read::read_to_string(&mut se, &mut stderr_buf);
                }
                let mut tmp = Self {
                    runtime_dir,
                    bridge_sock,
                    child,
                    closing,
                    stop_event_fd,
                    cb_thread: Some(cb_thread),
                };
                tmp.stop();
                return Err(format!(
                    "acos-devbridge exited early ({status}): {}",
                    stderr_buf.trim()
                ));
            }
            if UnixStream::connect(&bridge_sock).is_ok() {
                return Ok(Self {
                    runtime_dir,
                    bridge_sock,
                    child,
                    closing,
                    stop_event_fd,
                    cb_thread: Some(cb_thread),
                });
            }
            std::thread::sleep(Duration::from_millis(10));
        }

        let mut proc = Self {
            runtime_dir,
            bridge_sock,
            child,
            closing,
            stop_event_fd,
            cb_thread: Some(cb_thread),
        };
        proc.stop();
        Err("timed out waiting for acos-devbridge control socket".to_string())
    }

    fn stop(&mut self) {
        let _ = bridge_rpc(&self.bridge_sock, &json!({ "cmd": "shutdown" }));
        let wait_deadline = Instant::now() + Duration::from_secs(2);
        loop {
            match self.child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) if Instant::now() < wait_deadline => {
                    std::thread::sleep(Duration::from_millis(20));
                }
                _ => {
                    let _ = self.child.kill();
                    let _ = self.child.wait();
                    break;
                }
            }
        }
        if !self.closing.swap(true, Ordering::AcqRel) {
            let one: u64 = 1;
            unsafe {
                libc::write(
                    self.stop_event_fd.as_raw_fd(),
                    (&one as *const u64).cast::<libc::c_void>(),
                    std::mem::size_of::<u64>(),
                );
            }
        }
        if let Some(handle) = self.cb_thread.take() {
            let _ = handle.join();
        }
        let _ = std::fs::remove_dir_all(&self.runtime_dir);
    }
}

fn bridge_rpc(sock: &Path, req: &Value) -> Result<Value, String> {
    let mut stream =
        UnixStream::connect(sock).map_err(|e| format!("connect {}: {e}", sock.display()))?;
    let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));
    let mut line = req.to_string();
    line.push('\n');
    stream
        .write_all(line.as_bytes())
        .map_err(|e| format!("write bridge: {e}"))?;
    stream.flush().map_err(|e| format!("flush bridge: {e}"))?;
    let mut reader = BufReader::new(stream);
    let mut resp = String::new();
    let n = reader
        .read_line(&mut resp)
        .map_err(|e| format!("read bridge: {e}"))?;
    if n == 0 {
        return Err("bridge closed without reply".to_string());
    }
    serde_json::from_str(&resp).map_err(|e| format!("parse bridge reply: {e}"))
}

fn callback_accept_loop(
    listener: UnixListener,
    stop_fd: i32,
    closing: Arc<AtomicBool>,
    targets: Arc<Mutex<HashMap<String, Arc<dyn AdbSessionOps>>>>,
) {
    let listener_fd = listener.as_raw_fd();
    while !closing.load(Ordering::Acquire) {
        match listener.accept() {
            Ok((stream, _)) => {
                if closing.load(Ordering::Acquire) {
                    break;
                }
                if !unix_peer_same_uid(&stream) {
                    continue;
                }
                let targets = Arc::clone(&targets);
                std::thread::spawn(move || {
                    handle_callback_conn(stream, &targets);
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

fn handle_callback_conn(
    stream: UnixStream,
    targets: &Mutex<HashMap<String, Arc<dyn AdbSessionOps>>>,
) {
    let _ = stream.set_nonblocking(false);
    let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(10)));
    let mut writer = match stream.try_clone() {
        Ok(w) => w,
        Err(_) => return,
    };
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    let Ok(n) = reader.read_line(&mut line) else {
        return;
    };
    if n == 0 {
        return;
    }
    let reply = match serde_json::from_str::<Value>(&line) {
        Ok(req) => dispatch_callback(req, targets),
        Err(e) => json!({ "ok": false, "code": "BAD_REQUEST", "error": format!("invalid json: {e}") }),
    };
    let mut out = reply.to_string();
    out.push('\n');
    let _ = writer.write_all(out.as_bytes());
    let _ = writer.flush();
}

fn dispatch_callback(
    req: Value,
    targets: &Mutex<HashMap<String, Arc<dyn AdbSessionOps>>>,
) -> Value {
    let cmd = req.get("cmd").and_then(Value::as_str).unwrap_or("");
    let session_id = req.get("session_id").and_then(Value::as_str).unwrap_or("");
    let target = {
        let guard = targets.lock().unwrap_or_else(|e| e.into_inner());
        guard.get(session_id).cloned()
    };
    let Some(target) = target else {
        return json!({
            "ok": false,
            "code": "SESSION_NOT_FOUND",
            "error": format!("session '{session_id}' not found"),
        });
    };

    match cmd {
        "status" => match target.snapshot() {
            Ok(snap) => json!({
                "ok": true,
                "package": snap.package,
                "display": [snap.display_size.0, snap.display_size.1],
                "rotation": snap.rotation,
                "verify": target.is_verify_run(),
                "stream_socket": target.stream_socket().map(|p| p.display().to_string()),
            }),
            Err(e) => json!({ "ok": false, "code": "INTERNAL", "error": e }),
        },
        "raw_screenshot" => {
            let Some(path) = req.get("path").and_then(Value::as_str).map(PathBuf::from) else {
                return json!({ "ok": false, "code": "BAD_REQUEST", "error": "missing path" });
            };
            let format = req.get("format").and_then(Value::as_str).unwrap_or("rgb888");
            match target.raw_screenshot(&path, format) {
                Ok((w, h, rot)) => json!({
                    "ok": true,
                    "width": w,
                    "height": h,
                    "rotation": rot,
                }),
                Err(e) => json!({ "ok": false, "code": "INTERNAL", "error": e }),
            }
        }
        "touch" => {
            if target.is_verify_run() {
                let first_pt = req
                    .get("pointers")
                    .and_then(Value::as_array)
                    .and_then(|arr| arr.first())
                    .and_then(Value::as_array)
                    .and_then(|t| {
                        Some((
                            t.get(1)?.as_f64()?.round() as i64,
                            t.get(2)?.as_f64()?.round() as i64,
                        ))
                    });
                target.on_panel_input_ignored(
                    "touch",
                    first_pt.map(|p| p.0),
                    first_pt.map(|p| p.1),
                    "panel input ignored: verify run",
                );
                return json!({
                    "ok": false,
                    "code": "VERIFY_WATCH_ONLY",
                    "error": "panel input ignored: verify run",
                });
            }
            let Some(action) = req.get("action").and_then(Value::as_i64).map(|v| v as i32) else {
                return json!({ "ok": false, "code": "BAD_REQUEST", "error": "missing action" });
            };
            let down_time = req.get("down_time").and_then(Value::as_i64).unwrap_or(0);
            let event_time = req.get("event_time").and_then(Value::as_i64).unwrap_or(0);
            let pointers: Option<Vec<(i32, f32, f32)>> = req
                .get("pointers")
                .and_then(Value::as_array)
                .map(|arr| {
                    arr.iter()
                        .filter_map(|p| {
                            let tuple = p.as_array()?;
                            if tuple.len() != 3 {
                                return None;
                            }
                            Some((
                                tuple[0].as_i64()? as i32,
                                tuple[1].as_f64()? as f32,
                                tuple[2].as_f64()? as f32,
                            ))
                        })
                        .collect()
                });
            let Some(pointers) = pointers.filter(|p| !p.is_empty()) else {
                return json!({ "ok": false, "code": "BAD_REQUEST", "error": "missing pointers" });
            };
            match target.panel_touch(action, down_time, event_time, &pointers) {
                Ok(()) => json!({ "ok": true }),
                Err(e) => json!({ "ok": false, "code": "INTERNAL", "error": e }),
            }
        }
        "key" => {
            if target.is_verify_run() {
                target.on_panel_input_ignored("key", None, None, "panel input ignored: verify run");
                return json!({
                    "ok": false,
                    "code": "VERIFY_WATCH_ONLY",
                    "error": "panel input ignored: verify run",
                });
            }
            let Some(code) = req.get("code").and_then(Value::as_i64).map(|v| v as i32) else {
                return json!({ "ok": false, "code": "BAD_REQUEST", "error": "missing key code" });
            };
            let event_type = req
                .get("event_type")
                .and_then(Value::as_str)
                .unwrap_or("press");
            let meta_state = req.get("meta_state").and_then(Value::as_i64).unwrap_or(0) as i32;
            let device_id = req.get("device_id").and_then(Value::as_i64).unwrap_or(1) as i32;
            match target.panel_key(code, event_type, meta_state, device_id) {
                Ok(()) => json!({ "ok": true }),
                Err(e) => {
                    let code_str = if e.contains("HOME_UNAVAILABLE") || e.contains("no HOME") {
                        "HOME_UNAVAILABLE"
                    } else {
                        "INTERNAL"
                    };
                    json!({ "ok": false, "code": code_str, "error": e })
                }
            }
        }
        "text" => {
            if target.is_verify_run() {
                target.on_panel_input_ignored("text", None, None, "panel input ignored: verify run");
                return json!({
                    "ok": false,
                    "code": "VERIFY_WATCH_ONLY",
                    "error": "panel input ignored: verify run",
                });
            }
            let Some(text) = req.get("text").and_then(Value::as_str) else {
                return json!({ "ok": false, "code": "BAD_REQUEST", "error": "missing text" });
            };
            match target.panel_text(text) {
                Ok(()) => json!({ "ok": true }),
                Err(e) => json!({ "ok": false, "code": "INTERNAL", "error": e }),
            }
        }
        other => json!({
            "ok": false,
            "code": "BAD_REQUEST",
            "error": format!("unknown callback cmd {other}"),
        }),
    }
}

fn unix_peer_same_uid(stream: &UnixStream) -> bool {
    let mut cred = libc::ucred {
        pid: 0,
        uid: u32::MAX,
        gid: u32::MAX,
    };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    let rc = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&mut cred as *mut libc::ucred).cast::<libc::c_void>(),
            &mut len,
        )
    };
    rc == 0 && cred.uid == unsafe { libc::getuid() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sweep_stale_acos_ini_preserves_real_emulator_and_live_acos() {
        let dir = std::env::temp_dir().join(format!(
            "acos-sweep-test-{}-{}",
            std::process::id(),
            random_bearer_token()
        ));
        std::fs::create_dir_all(&dir).unwrap();

        let dead_acos = dir.join("pid_999999.ini");
        std::fs::write(
            &dead_acos,
            "port.serial=5554\nport.adb=5555\ngrpc.port=12345\navd.name=ACOS dev.foo dead\n",
        )
        .unwrap();

        let dead_real_emu = dir.join("pid_999998.ini");
        std::fs::write(
            &dead_real_emu,
            "port.serial=5556\nport.adb=5557\ngrpc.port=8554\navd.name=Pixel_8_API_35\n",
        )
        .unwrap();

        let live_pid = std::process::id();
        let live_acos = dir.join(format!("pid_{live_pid}.ini"));
        std::fs::write(
            &live_acos,
            "port.serial=5558\nport.adb=5559\ngrpc.port=12346\navd.name=ACOS dev.foo live\n",
        )
        .unwrap();

        sweep_stale_acos_ini(&dir);

        assert!(!dead_acos.exists(), "dead ACOS ini should be swept");
        assert!(dead_real_emu.exists(), "non-ACOS ini must never be touched");
        assert!(live_acos.exists(), "live ACOS ini must be preserved");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
