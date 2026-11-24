use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::io::AsRawFd;
use std::os::unix::net::UnixStream as StdUnixStream;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use tokio::net::{TcpListener, UnixListener, UnixStream};
use tokio::sync::watch;
use tokio_stream::wrappers::TcpListenerStream;
use tonic::transport::Server;

use crate::grpc::{pb::emulator_controller_server::EmulatorControllerServer, AcosControllerService};
use crate::keymap::KeyboardState;
use crate::live_client::LiveStreamHub;
use crate::touch::TouchTracker;

/// Process-wide monotonically increasing sequence for temporary raw screenshot files.
static SHOT_SEQ: AtomicU64 = AtomicU64::new(0);

/// State shared between the bridge server and one session's gRPC service.
pub struct BridgeSession {
    pub session_id: String,
    pub avd_name: String,
    pub token: String,
    pub callback_sock: PathBuf,
    pub shot_dir: PathBuf,
    pub started_at: Instant,
    pub display_w: AtomicU32,
    pub display_h: AtomicU32,
    pub rotation: AtomicU8,
    pub verify_run: AtomicBool,
    pub live_hub: Option<Arc<LiveStreamHub>>,
    pub touch_tracker: Mutex<TouchTracker>,
    pub keyboard_state: Mutex<KeyboardState>,
    pub input_lock: tokio::sync::Mutex<()>,
    shutdown_tx: watch::Sender<bool>,
}

impl BridgeSession {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        session_id: String,
        avd_name: String,
        token: String,
        callback_sock: PathBuf,
        shot_dir: PathBuf,
        display_w: u32,
        display_h: u32,
        rotation: u8,
        verify_run: bool,
        stream_socket: Option<String>,
        shutdown_tx: watch::Sender<bool>,
    ) -> Arc<Self> {
        let live_hub = stream_socket
            .filter(|s| !s.is_empty())
            .map(LiveStreamHub::new);
        Arc::new(Self {
            session_id,
            avd_name,
            token,
            callback_sock,
            shot_dir,
            started_at: Instant::now(),
            display_w: AtomicU32::new(display_w.max(1)),
            display_h: AtomicU32::new(display_h.max(1)),
            rotation: AtomicU8::new(rotation % 4),
            verify_run: AtomicBool::new(verify_run),
            live_hub,
            touch_tracker: Mutex::new(TouchTracker::default()),
            keyboard_state: Mutex::new(KeyboardState::default()),
            input_lock: tokio::sync::Mutex::new(()),
            shutdown_tx,
        })
    }

    pub fn subscribe_shutdown(&self) -> watch::Receiver<bool> {
        self.shutdown_tx.subscribe()
    }

    pub fn shutdown(&self) {
        let _ = self.shutdown_tx.send(true);
        if let Some(hub) = &self.live_hub {
            hub.shutdown();
        }
    }

    pub async fn call_callback(&self, req: Value) -> Result<Value, String> {
        let sock = self.callback_sock.clone();
        tokio::task::spawn_blocking(move || callback_rpc(&sock, &req))
            .await
            .map_err(|e| format!("callback join error: {e}"))?
    }

    pub async fn raw_screenshot(&self, format: &str) -> Result<(u32, u32, u8, Vec<u8>), String> {
        let seq = SHOT_SEQ.fetch_add(1, Ordering::Relaxed);
        let shot_path = self
            .shot_dir
            .join(format!("shot-{}-{seq}.bin", std::process::id()));
        let req = json!({
            "cmd": "raw_screenshot",
            "session_id": self.session_id,
            "path": shot_path.display().to_string(),
            "format": format,
        });
        struct RemoveOnDrop(PathBuf);
        impl Drop for RemoveOnDrop {
            fn drop(&mut self) {
                let _ = std::fs::remove_file(&self.0);
            }
        }
        let _guard = RemoveOnDrop(shot_path.clone());
        let resp = self.call_callback(req).await?;
        if resp.get("ok").and_then(Value::as_bool) != Some(true) {
            return Err(resp
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("raw_screenshot failed")
                .to_string());
        }
        let w = resp.get("width").and_then(Value::as_u64).unwrap_or(0) as u32;
        let h = resp.get("height").and_then(Value::as_u64).unwrap_or(0) as u32;
        let rot = resp.get("rotation").and_then(Value::as_u64).unwrap_or(0) as u8;
        self.display_w.store(w.max(1), Ordering::Relaxed);
        self.display_h.store(h.max(1), Ordering::Relaxed);
        self.rotation.store(rot % 4, Ordering::Relaxed);
        let bytes = tokio::task::spawn_blocking(move || std::fs::read(&shot_path))
            .await
            .map_err(|e| format!("read screenshot join error: {e}"))?
            .map_err(|e| format!("read screenshot file: {e}"))?;
        Ok((w, h, rot % 4, bytes))
    }

    pub async fn refresh_status(&self) -> Result<(u32, u32, u8, bool), String> {
        let resp = self
            .call_callback(json!({
                "cmd": "status",
                "session_id": self.session_id,
            }))
            .await?;
        if resp.get("ok").and_then(Value::as_bool) != Some(true) {
            return Err(resp
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("status failed")
                .to_string());
        }
        let disp = resp.get("display").and_then(Value::as_array);
        let w = disp
            .and_then(|a| a.first())
            .and_then(Value::as_u64)
            .unwrap_or(u64::from(self.display_w.load(Ordering::Relaxed))) as u32;
        let h = disp
            .and_then(|a| a.get(1))
            .and_then(Value::as_u64)
            .unwrap_or(u64::from(self.display_h.load(Ordering::Relaxed))) as u32;
        let rot = resp.get("rotation").and_then(Value::as_u64).unwrap_or(0) as u8;
        let verify = resp
            .get("verify")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        self.display_w.store(w.max(1), Ordering::Relaxed);
        self.display_h.store(h.max(1), Ordering::Relaxed);
        self.rotation.store(rot % 4, Ordering::Relaxed);
        self.verify_run.store(verify, Ordering::Relaxed);
        Ok((w, h, rot % 4, verify))
    }
}

fn callback_rpc(sock: &Path, req: &Value) -> Result<Value, String> {
    let mut stream =
        StdUnixStream::connect(sock).map_err(|e| format!("connect {}: {e}", sock.display()))?;
    let _ = stream.set_read_timeout(Some(Duration::from_secs(15)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));
    let mut line = req.to_string();
    line.push('\n');
    stream
        .write_all(line.as_bytes())
        .map_err(|e| format!("write callback: {e}"))?;
    stream.flush().map_err(|e| format!("flush callback: {e}"))?;
    let mut reader = BufReader::new(stream);
    let mut resp_line = String::new();
    let n = reader
        .read_line(&mut resp_line)
        .map_err(|e| format!("read callback: {e}"))?;
    if n == 0 {
        return Err("callback closed without response".to_string());
    }
    serde_json::from_str(&resp_line).map_err(|e| format!("parse callback json: {e}"))
}

struct RunningGrpc {
    session: Arc<BridgeSession>,
    task: tokio::task::JoinHandle<()>,
}

pub struct BridgeControl {
    callback_sock: PathBuf,
    shot_dir: PathBuf,
    sessions: Mutex<HashMap<String, RunningGrpc>>,
    daemon_shutdown: watch::Sender<bool>,
}

impl BridgeControl {
    pub fn new(callback_sock: PathBuf, shot_dir: PathBuf) -> (Arc<Self>, watch::Receiver<bool>) {
        let (daemon_shutdown, rx) = watch::channel(false);
        (
            Arc::new(Self {
                callback_sock,
                shot_dir,
                sessions: Mutex::new(HashMap::new()),
                daemon_shutdown,
            }),
            rx,
        )
    }

    pub fn shutdown_all(&self) {
        let drained: Vec<RunningGrpc> = {
            let mut guard = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
            guard.drain().map(|(_, v)| v).collect()
        };
        for item in drained {
            item.session.shutdown();
            item.task.abort();
        }
        let _ = self.daemon_shutdown.send(true);
    }

    async fn handle_cmd(&self, req: Value) -> Value {
        let cmd = req.get("cmd").and_then(Value::as_str).unwrap_or("");
        match cmd {
            "ping" => json!({ "ok": true }),
            "register" => self.cmd_register(req).await,
            "unregister" => {
                let Some(id) = req.get("session_id").and_then(Value::as_str) else {
                    return json!({ "ok": false, "error": "missing session_id" });
                };
                self.unregister(id);
                json!({ "ok": true })
            }
            "shutdown" => {
                self.shutdown_all();
                json!({ "ok": true })
            }
            other => json!({ "ok": false, "error": format!("unknown bridge command {other}") }),
        }
    }

    fn unregister(&self, session_id: &str) {
        let existing = self
            .sessions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(session_id);
        if let Some(item) = existing {
            item.session.shutdown();
            item.task.abort();
        }
    }

    async fn cmd_register(&self, req: Value) -> Value {
        let Some(session_id) = req
            .get("session_id")
            .and_then(Value::as_str)
            .map(str::to_string)
        else {
            return json!({ "ok": false, "error": "register missing session_id" });
        };
        let Some(token) = req
            .get("token")
            .and_then(Value::as_str)
            .filter(|t| !t.is_empty())
            .map(str::to_string)
        else {
            return json!({ "ok": false, "error": "register missing token" });
        };
        let avd_name = req
            .get("avd_name")
            .and_then(Value::as_str)
            .unwrap_or("ACOS")
            .to_string();
        let disp = req.get("display").and_then(Value::as_array);
        let display_w = disp.and_then(|a| a.first()).and_then(Value::as_u64).unwrap_or(1080) as u32;
        let display_h = disp.and_then(|a| a.get(1)).and_then(Value::as_u64).unwrap_or(2400) as u32;
        let rotation = req.get("rotation").and_then(Value::as_u64).unwrap_or(0) as u8;
        let verify = req.get("verify").and_then(Value::as_bool).unwrap_or(false);
        let stream_socket = req
            .get("stream_socket")
            .and_then(Value::as_str)
            .map(str::to_string);

        let listener = match TcpListener::bind("127.0.0.1:0").await {
            Ok(l) => l,
            Err(e) => {
                return json!({ "ok": false, "error": format!("bind gRPC port: {e}") });
            }
        };
        let grpc_port = match listener.local_addr() {
            Ok(addr) => addr.port(),
            Err(e) => {
                return json!({ "ok": false, "error": format!("gRPC local_addr: {e}") });
            }
        };

        let (shutdown_tx, mut shutdown_rx) = watch::channel(false);
        let session = BridgeSession::new(
            session_id.clone(),
            avd_name,
            token,
            self.callback_sock.clone(),
            self.shot_dir.clone(),
            display_w,
            display_h,
            rotation,
            verify,
            stream_socket,
            shutdown_tx,
        );
        let svc = AcosControllerService::new(Arc::clone(&session));
        let grpc_svc = EmulatorControllerServer::new(svc)
            .max_decoding_message_size(64 * 1024 * 1024)
            .max_encoding_message_size(64 * 1024 * 1024);

        let incoming = TcpListenerStream::new(listener);
        let task = tokio::spawn(async move {
            let _ = Server::builder()
                .initial_connection_window_size(Some(8 * 1024 * 1024))
                .initial_stream_window_size(Some(4 * 1024 * 1024))
                .add_service(grpc_svc)
                .serve_with_incoming_shutdown(incoming, async move {
                    while !*shutdown_rx.borrow() {
                        if shutdown_rx.changed().await.is_err() {
                            break;
                        }
                    }
                })
                .await;
        });

        let displaced = self.sessions.lock().unwrap_or_else(|e| e.into_inner()).insert(
            session_id,
            RunningGrpc {
                session,
                task,
            },
        );
        if let Some(old) = displaced {
            old.session.shutdown();
            old.task.abort();
        }
        json!({ "ok": true, "grpc_port": grpc_port })
    }
}

pub async fn serve_control_socket(
    socket_path: &Path,
    control: Arc<BridgeControl>,
    mut shutdown_rx: watch::Receiver<bool>,
) -> Result<(), String> {
    if socket_path.exists() {
        let _ = std::fs::remove_file(socket_path);
    }
    let listener = UnixListener::bind(socket_path)
        .map_err(|e| format!("bind bridge socket {}: {e}", socket_path.display()))?;
    let _ = std::fs::set_permissions(socket_path, std::fs::Permissions::from_mode(0o600));

    loop {
        tokio::select! {
            _ = shutdown_rx.changed() => {
                if *shutdown_rx.borrow() {
                    break;
                }
            }
            accepted = listener.accept() => {
                match accepted {
                    Ok((stream, _)) => {
                        if !peer_same_uid(&stream) {
                            continue;
                        }
                        let ctrl = Arc::clone(&control);
                        tokio::spawn(async move {
                            handle_control_conn(stream, ctrl).await;
                        });
                    }
                    Err(e) => {
                        return Err(format!("bridge socket accept failed: {e}"));
                    }
                }
            }
        }
    }
    let _ = std::fs::remove_file(socket_path);
    Ok(())
}

async fn handle_control_conn(stream: UnixStream, control: Arc<BridgeControl>) {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let (read_half, mut write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half);
    let mut line = String::new();
    while let Ok(n) = reader.read_line(&mut line).await {
        if n == 0 {
            break;
        }
        let resp = match serde_json::from_str::<Value>(&line) {
            Ok(req) => control.handle_cmd(req).await,
            Err(e) => json!({ "ok": false, "error": format!("invalid json: {e}") }),
        };
        line.clear();
        let mut out = resp.to_string();
        out.push('\n');
        if write_half.write_all(out.as_bytes()).await.is_err() {
            break;
        }
        let _ = write_half.flush().await;
    }
}

fn peer_same_uid(stream: &UnixStream) -> bool {
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

    fn scratch_dir(tag: &str) -> PathBuf {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let dir = PathBuf::from("/tmp").join(format!(
            "acos-t-{tag}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::create_dir_all(&dir);
        dir
    }

    #[tokio::test]
    async fn raw_screenshot_paths_do_not_collide_across_sessions() {
        let dir = scratch_dir("shot-seq");
        let sock = dir.join("cb.sock");
        let listener = UnixListener::bind(&sock).expect("bind callback sock");

        let recorded_paths: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let recorded_bg = Arc::clone(&recorded_paths);
        let server_task = tokio::spawn(async move {
            use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
            for _ in 0..2 {
                let (stream, _) = listener.accept().await.expect("accept");
                let recorded_conn = Arc::clone(&recorded_bg);
                tokio::spawn(async move {
                    let (r, mut w) = stream.into_split();
                    let mut reader = BufReader::new(r);
                    let mut line = String::new();
                    reader.read_line(&mut line).await.expect("read req");
                    let req: Value = serde_json::from_str(&line).expect("json");
                    let path = req
                        .get("path")
                        .and_then(Value::as_str)
                        .expect("path")
                        .to_string();
                    std::fs::write(&path, b"PNGDATA").expect("write shot file");
                    recorded_conn.lock().unwrap().push(path);
                    let resp = json!({
                        "ok": true,
                        "width": 1080,
                        "height": 2400,
                        "rotation": 0,
                    });
                    let mut out = resp.to_string();
                    out.push('\n');
                    w.write_all(out.as_bytes()).await.expect("write resp");
                });
            }
        });

        let (tx1, _rx1) = watch::channel(false);
        let s1 = BridgeSession::new(
            "s1".to_string(),
            "avd1".to_string(),
            "tok1".to_string(),
            sock.clone(),
            dir.clone(),
            1080,
            2400,
            0,
            false,
            None,
            tx1,
        );
        let (tx2, _rx2) = watch::channel(false);
        let s2 = BridgeSession::new(
            "s2".to_string(),
            "avd2".to_string(),
            "tok2".to_string(),
            sock.clone(),
            dir.clone(),
            1080,
            2400,
            0,
            false,
            None,
            tx2,
        );

        let (r1, r2) = tokio::join!(s1.raw_screenshot("png"), s2.raw_screenshot("png"));
        r1.expect("s1 raw_screenshot");
        r2.expect("s2 raw_screenshot");
        server_task.await.expect("server join");

        let paths = recorded_paths.lock().unwrap();
        assert_eq!(paths.len(), 2);
        assert_ne!(
            paths[0], paths[1],
            "shot paths collided across sessions: {:?}",
            *paths
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn concurrent_register_same_session_shuts_down_displaced_servers() {
        let dir = scratch_dir("reg-race");
        let sock = dir.join("cb.sock");
        let (ctrl, _rx) = BridgeControl::new(sock, dir.clone());
        let mut all_ports = Vec::new();

        for round in 0..40 {
            let sid = format!("sess-race-{round}");
            let req1 = json!({
                "cmd": "register",
                "session_id": sid,
                "token": "tok-a",
                "avd_name": "ACOS",
                "display": [1080, 2400],
            });
            let req2 = json!({
                "cmd": "register",
                "session_id": sid,
                "token": "tok-b",
                "avd_name": "ACOS",
                "display": [1080, 2400],
            });
            let c1 = Arc::clone(&ctrl);
            let c2 = Arc::clone(&ctrl);
            let h1 = tokio::spawn(async move { c1.handle_cmd(req1).await });
            let h2 = tokio::spawn(async move { c2.handle_cmd(req2).await });
            let (r1, r2) = tokio::join!(h1, h2);
            let p1 = r1.unwrap()["grpc_port"].as_u64().expect("p1") as u16;
            let p2 = r2.unwrap()["grpc_port"].as_u64().expect("p2") as u16;
            all_ports.push(p1);
            all_ports.push(p2);

            let unreg = ctrl
                .handle_cmd(json!({ "cmd": "unregister", "session_id": sid }))
                .await;
            assert_eq!(unreg["ok"], true);
        }

        // Give aborted tasks a brief moment to drop their TcpListeners.
        tokio::time::sleep(Duration::from_millis(50)).await;

        let mut still_listening = Vec::new();
        for port in all_ports {
            if tokio::net::TcpStream::connect(("127.0.0.1", port))
                .await
                .is_ok()
            {
                still_listening.push(port);
            }
        }
        ctrl.shutdown_all();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(
            still_listening.is_empty(),
            "displaced gRPC listeners leaked open ports after unregister: {still_listening:?}"
        );
    }
}
