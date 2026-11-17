//! The Unix-socket server the guest provider talks to: one socket per session,
//! framed messages exactly as `results/handoff/WVB/WIRE.md` specifies, and the
//! per-view event pumps that turn DevTools events into `navigation.*`,
//! `render.invalidated`, `req.intercept` and `js.call`.
//!
//! Failure behaviour, in one place:
//! * an unknown op is `UNSUPPORTED`;
//! * an op on a view the engine no longer has is `NO_VIEW` (never a silent
//!   success against a stale target);
//! * a guest that does not answer a host request within 30 s makes the page side
//!   fail, and the count is reported in `engineStatus.unanswered`.

use std::collections::HashMap;
use std::io::{BufReader, BufWriter, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::sync::mpsc::{sync_channel, SyncSender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::broker::{Broker, Session};
use crate::error::{Error, Result};
use crate::view::{base64_encode, truncate, Viewport};
use crate::wire::{
    err_response, event, ok_response, read_frame, write_frame, Blob, ErrorBody, Frame,
    HOST_REQUEST_TIMEOUT, PROTO,
};

/// How many requests one connection handles concurrently.
const WORKERS: usize = 4;
/// Cap on concurrent `Fetch.requestPaused` round trips per connection.
const MAX_INTERCEPTS: usize = 16;

#[derive(Debug, Clone)]
pub struct ServerConfig {
    pub socket_path: PathBuf,
    pub session_id: String,
}

/// A running server. Dropping it stops the accept loop and removes the socket.
pub struct ServerHandle {
    socket: PathBuf,
    stop: Arc<AtomicBool>,
    threads: Mutex<Vec<std::thread::JoinHandle<()>>>,
    broker: Arc<Broker>,
}

impl ServerHandle {
    pub fn socket(&self) -> &Path {
        &self.socket
    }

    pub fn broker(&self) -> &Arc<Broker> {
        &self.broker
    }

    /// Block until the server has stopped. The watcher's idle exit sets the
    /// flag; this call never signals the broker itself, so a daemon's main loop
    /// is "serve until something stops us".
    pub fn wait(&self) {
        while !self.stopped() {
            std::thread::sleep(Duration::from_millis(25));
        }
    }

    /// Whether every server thread has finished.
    fn stopped(&self) -> bool {
        self.threads
            .lock()
            .map(|t| t.iter().all(|h| h.is_finished()))
            .unwrap_or(true)
    }

    pub fn stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
        // Nudge accept(): a connect makes it loop once more and see the flag.
        let _ = UnixStream::connect(&self.socket);
        self.broker.shutdown();
    }
}

impl Drop for ServerHandle {
    fn drop(&mut self) {
        self.stop();
        if let Ok(mut threads) = self.threads.lock() {
            for t in threads.drain(..) {
                let _ = t.join();
            }
        }
        let _ = std::fs::remove_file(&self.socket);
    }
}

/// File name of the per-session socket inside the session directory
/// (`WIRE.md` §0: the guest connects to `/run/acos/webview.sock`).
pub const SESSION_SOCKET_FILE: &str = "webview.sock";

/// How a server names the sessions that arrive on its listener.
#[derive(Debug, Clone)]
enum AcceptMode {
    /// One fixed session: the per-session socket `serve()` binds.
    Single(String),
    /// Many sessions on one host-wide socket (the shared daemon): each
    /// connection announces its session with one attach line first.
    Attached,
}

/// Shared-daemon options (`acos-webview --shared`, WV5b).
#[derive(Debug, Clone, Default)]
pub struct SharedConfig {
    /// Exit the daemon once no session has been attached for this long. The
    /// engine idle-stops on `BrokerConfig::idle_shutdown`; this is the process
    /// going away too, so the *next* WebView starts a fresh daemon.
    pub idle_exit: Option<Duration>,
}

/// Serve one session socket.
pub fn serve(broker: Arc<Broker>, cfg: &ServerConfig) -> Result<ServerHandle> {
    let listener = bind_socket(&cfg.socket_path)?;
    Ok(start_server(
        broker,
        cfg.socket_path.clone(),
        listener,
        AcceptMode::Single(cfg.session_id.clone()),
        SharedConfig::default(),
    ))
}

/// Serve the **host-wide** socket: one connection per session, each with its own
/// browser context (ADR-0031 item 3, one engine per host). This is what
/// `acos-webview --shared` runs; per-session gates attach to it.
pub fn serve_shared(
    broker: Arc<Broker>,
    socket: &Path,
    shared: SharedConfig,
) -> Result<ServerHandle> {
    let listener = bind_socket(socket)?;
    Ok(start_server(
        broker,
        socket.to_path_buf(),
        listener,
        AcceptMode::Attached,
        shared,
    ))
}

/// Bind a server socket **without stealing a live endpoint**.
///
/// A stale node (a broker that died without cleaning up) is removed and
/// re-bound; a node somebody is still listening on is an error the caller
/// reports. Blindly unlinking first would let a second daemon take over the
/// endpoint of a running one, splitting sessions across two engines — measured
/// in the WV5b two-session test before this was written.
fn bind_socket(path: &Path) -> Result<UnixListener> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| {
            Error::proto("INTERNAL", format!("socket dir {}: {e}", parent.display()))
        })?;
    }
    let listener = match UnixListener::bind(path) {
        Ok(l) => l,
        Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
            if UnixStream::connect(path).is_ok() {
                return Err(Error::proto(
                    "ADDR_IN_USE",
                    format!("another broker is listening on {}", path.display()),
                ));
            }
            // Nobody answers: the node is stale.
            let _ = std::fs::remove_file(path);
            UnixListener::bind(path)
                .map_err(|e| Error::proto("INTERNAL", format!("bind {}: {e}", path.display())))?
        }
        Err(e) => {
            return Err(Error::proto(
                "INTERNAL",
                format!("bind {}: {e}", path.display()),
            ))
        }
    };
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    listener.set_nonblocking(true).ok();
    Ok(listener)
}

fn start_server(
    broker: Arc<Broker>,
    socket: PathBuf,
    listener: UnixListener,
    mode: AcceptMode,
    shared: SharedConfig,
) -> ServerHandle {
    let stop = Arc::new(AtomicBool::new(false));
    let conns: Arc<Mutex<Vec<Arc<Conn>>>> = Arc::new(Mutex::new(Vec::new()));
    let mut threads = Vec::new();

    // Accept loop.
    {
        let stop = Arc::clone(&stop);
        let conns = Arc::clone(&conns);
        let broker = Arc::clone(&broker);
        let mode = mode.clone();
        if let Ok(h) = std::thread::Builder::new()
            .name("wv-accept".into())
            .spawn(move || {
                while !stop.load(Ordering::SeqCst) {
                    match listener.accept() {
                        Ok((mut stream, _)) => {
                            let mut live = conns.lock().expect("conns");
                            live.retain(|c| !c.closed());
                            // One connection per **session** socket. The shared
                            // broker is one socket for the whole host, so its
                            // connections are one per session and all allowed;
                            // a per-session socket takes exactly one.
                            let single = matches!(mode, AcceptMode::Single(_));
                            if single && !live.is_empty() {
                                // One connection per session socket.
                                let body = err_response(
                                    "g?",
                                    &ErrorBody::new(
                                        crate::wire::code::ALREADY_CONNECTED,
                                        "a guest connection is already attached to this session socket",
                                    ),
                                );
                                if let Ok(bytes) = crate::wire::encode_json(&body) {
                                    let _ = write_frame(&mut stream, &bytes);
                                }
                                continue;
                            }
                            let Some(session_id) = session_for(&mut stream, &mode) else {
                                continue;
                            };
                            let conn = Arc::new(Conn::new(
                                stream,
                                Arc::clone(&broker),
                                session_id,
                            ));
                            live.push(Arc::clone(&conn));
                            drop(live);
                            let c = Arc::clone(&conn);
                            std::thread::Builder::new()
                                .name("wv-conn".into())
                                .spawn(move || c.run())
                                .ok();
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::sleep(Duration::from_millis(50));
                        }
                        Err(_) => std::thread::sleep(Duration::from_millis(50)),
                    }
                }
            })
        {
            threads.push(h);
        }
    }

    // Watcher: engine health, idle shutdown, the events the guest must see, and
    // (shared mode) the daemon's own idle exit.
    {
        let stop = Arc::clone(&stop);
        let conns = Arc::clone(&conns);
        let broker = Arc::clone(&broker);
        let socket = socket.clone();
        if let Ok(h) = std::thread::Builder::new()
            .name("wv-watch".into())
            .spawn(move || {
                let mut last_restarts = broker.restarts();
                let mut idle_since = Instant::now();
                while !stop.load(Ordering::SeqCst) {
                    std::thread::sleep(Duration::from_millis(250));
                    match broker.tick() {
                        crate::broker::Tick::Restarted { .. } => {
                            let _ = broker.restart_now();
                        }
                        crate::broker::Tick::IdleShutdown => {
                            for c in conns.lock().expect("conns").iter() {
                                c.send_event(
                                    "session.closing",
                                    json!({"reason": "idle", "idleShutdownMs": broker.config().idle_shutdown.as_millis() as u64}),
                                );
                            }
                        }
                        crate::broker::Tick::Nothing => {}
                    }
                    let now = broker.restarts();
                    if now != last_restarts {
                        last_restarts = now;
                        for c in conns.lock().expect("conns").iter() {
                            c.on_engine_restart();
                        }
                    }
                    let no_session = {
                        let live = conns.lock().expect("conns");
                        live.iter().all(|c| c.closed())
                    };
                    if !no_session {
                        idle_since = Instant::now();
                    }
                    if let Some(limit) = shared.idle_exit {
                        if no_session && idle_since.elapsed() >= limit {
                            // No session has been attached for `limit`: stop the
                            // engine and go away, so the next WebView starts a
                            // fresh daemon instead of an idle resident process.
                            eprintln!(
                                "acos-webview: no session for {}s, exiting ({})",
                                limit.as_secs(),
                                socket.display()
                            );
                            stop.store(true, Ordering::SeqCst);
                        }
                    }
                }
            })
        {
            threads.push(h);
        }
    }

    ServerHandle {
        socket,
        stop,
        threads: Mutex::new(threads),
        broker,
    }
}

/// The session id for a freshly accepted connection, answering the attach line
/// in shared mode. `None` means the peer did not identify itself and the
/// connection is dropped (it never became a session).
fn session_for(stream: &mut UnixStream, mode: &AcceptMode) -> Option<String> {
    match mode {
        AcceptMode::Single(id) => Some(id.clone()),
        AcceptMode::Attached => {
            let line = read_line(stream, Duration::from_secs(5))?;
            let v: Value = serde_json::from_slice(&line).ok()?;
            if v["attach"].as_u64() != Some(crate::gate::ATTACH_PROTO as u64) {
                let _ =
                    stream.write_all(b"{\"ok\":false,\"error\":\"attach version unsupported\"}\n");
                return None;
            }
            let sid = v["session"].as_str().unwrap_or("").to_string();
            let sid = if sid.is_empty() {
                "session".to_string()
            } else {
                sid
            };
            stream.write_all(b"{\"ok\":true}\n").ok()?;
            stream.flush().ok()?;
            stream.set_read_timeout(None).ok()?;
            Some(sid)
        }
    }
}

/// One `\n`-terminated line, read byte-wise so no guest frame is ever swallowed
/// by a buffered reader (the attach line is the only thing read outside the
/// framing).
fn read_line(stream: &mut UnixStream, timeout: Duration) -> Option<Vec<u8>> {
    stream.set_read_timeout(Some(timeout)).ok()?;
    let mut buf = Vec::new();
    let mut byte = [0u8; 1];
    while buf.len() < 4096 {
        match stream.read(&mut byte) {
            Ok(1) if byte[0] == b'\n' => return Some(buf),
            Ok(1) => buf.push(byte[0]),
            Ok(_) => {}
            Err(_) => return None,
        }
    }
    None
}

/// One guest connection.
pub struct Conn {
    writer: Mutex<BufWriter<UnixStream>>,
    reader: Mutex<Option<BufReader<UnixStream>>>,
    broker: Arc<Broker>,
    session_id: String,
    session: Mutex<Option<Arc<Session>>>,
    pending: Mutex<HashMap<String, SyncSender<Value>>>,
    next_host_id: AtomicU32,
    closed: AtomicBool,
    unanswered: AtomicU32,
    intercepts: AtomicUsize,
    /// Frame pulls dispatched on their own threads (bounded, see
    /// [`Conn::dispatch_render`]).
    frame_pulls: AtomicUsize,
    pumps: Mutex<Vec<std::thread::JoinHandle<()>>>,
    hello: AtomicBool,
}

impl Conn {
    fn new(stream: UnixStream, broker: Arc<Broker>, session_id: String) -> Self {
        let read_half = stream
            .try_clone()
            .unwrap_or_else(|_| stream.try_clone().expect("clone socket"));
        Self {
            writer: Mutex::new(BufWriter::new(stream)),
            reader: Mutex::new(Some(BufReader::new(read_half))),
            broker,
            session_id,
            session: Mutex::new(None),
            pending: Mutex::new(HashMap::new()),
            next_host_id: AtomicU32::new(1),
            closed: AtomicBool::new(false),
            unanswered: AtomicU32::new(0),
            intercepts: AtomicUsize::new(0),
            frame_pulls: AtomicUsize::new(0),
            pumps: Mutex::new(Vec::new()),
            hello: AtomicBool::new(false),
        }
    }

    fn closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }

    fn send_value(&self, msg: &Value) -> bool {
        let Ok(bytes) = crate::wire::encode_json(msg) else {
            return false;
        };
        let mut w = self.writer.lock().expect("writer");
        if write_frame(&mut *w, &bytes).is_err() {
            drop(w);
            self.closed.store(true, Ordering::SeqCst);
            return false;
        }
        true
    }

    fn send_event(&self, name: &str, fields: Value) -> bool {
        self.send_value(&event(name, fields))
    }

    /// Send a JSON response and (for `render`) its blob atomically, so no other
    /// response can be interleaved between them.
    fn send_response(&self, value: &Value, blob: Option<&Blob>) -> bool {
        let Ok(bytes) = crate::wire::encode_json(value) else {
            return false;
        };
        let blob_bytes = match blob {
            Some(b) => match crate::wire::encode_blob(b) {
                Ok(v) => Some(v),
                Err(_) => return false,
            },
            None => None,
        };
        let mut w = self.writer.lock().expect("writer");
        if write_frame(&mut *w, &bytes).is_err() {
            drop(w);
            self.closed.store(true, Ordering::SeqCst);
            return false;
        }
        if let Some(b) = blob_bytes {
            if write_frame(&mut *w, &b).is_err() {
                drop(w);
                self.closed.store(true, Ordering::SeqCst);
                return false;
            }
        }
        true
    }

    /// Ask the guest something and wait for the answer (host→guest request).
    fn host_request(&self, op: &str, params: Value) -> Result<Value> {
        let n = self.next_host_id.fetch_add(1, Ordering::SeqCst);
        let id = format!("h{n}");
        let (tx, rx) = sync_channel(1);
        self.pending.lock().expect("pending").insert(id.clone(), tx);
        let mut msg = params;
        msg["id"] = json!(id);
        msg["op"] = json!(op);
        if !self.send_value(&msg) {
            self.pending.lock().expect("pending").remove(&id);
            return Err(Error::proto(
                crate::wire::code::INTERNAL,
                "connection closed while sending a host request",
            ));
        }
        match rx.recv_timeout(HOST_REQUEST_TIMEOUT) {
            Ok(answer) => {
                if answer["ok"].as_bool().unwrap_or(false) {
                    Ok(answer["result"].clone())
                } else {
                    let body: ErrorBody = serde_json::from_value(answer["error"].clone())
                        .unwrap_or_else(|_| {
                            ErrorBody::new(crate::wire::code::INTERNAL, answer["error"].to_string())
                        });
                    Err(Error::proto(body.code, body.message))
                }
            }
            Err(_) => {
                self.pending.lock().expect("pending").remove(&id);
                self.unanswered.fetch_add(1, Ordering::SeqCst);
                Err(Error::proto(
                    crate::wire::code::TIMEOUT,
                    format!("guest did not answer {op} within {HOST_REQUEST_TIMEOUT:?}"),
                ))
            }
        }
    }

    fn on_engine_restart(&self) {
        if let Some(session) = self.session.lock().expect("session").clone() {
            let views: Vec<Arc<crate::view::View>> = session
                .views
                .lock()
                .map(|v| v.values().cloned().collect())
                .unwrap_or_default();
            for v in views {
                v.closed.store(true, Ordering::SeqCst);
            }
            session.views.lock().map(|mut v| v.clear()).ok();
        }
        self.send_event(
            "engine.restarted",
            json!({"restarts": self.broker.restarts(), "reason": "engine exited"}),
        );
    }

    fn run(self: Arc<Self>) {
        let (tx, rx) = sync_channel::<Value>(64);
        let rx = Arc::new(Mutex::new(rx));
        for i in 0..WORKERS {
            let conn = Arc::clone(&self);
            let rx = Arc::clone(&rx);
            std::thread::Builder::new()
                .name(format!("wv-worker-{i}"))
                .spawn(move || loop {
                    let req = {
                        let guard = rx.lock().expect("rx");
                        guard.recv()
                    };
                    let Ok(req) = req else { return };
                    conn.dispatch(req);
                })
                .ok();
        }
        let mut reader = self
            .reader
            .lock()
            .expect("reader")
            .take()
            .expect("reader taken once");
        loop {
            match read_frame(&mut reader) {
                Ok(Frame::Json(msg)) => {
                    self.route(msg, &tx);
                }
                Ok(Frame::Blob(b)) => {
                    // Blobs only travel host→guest; one arriving is a protocol
                    // error the guest must see.
                    self.send_value(&err_response(
                        &b.id,
                        &ErrorBody::new(
                            crate::wire::code::BAD_FRAME,
                            "the broker does not accept blobs from the guest",
                        ),
                    ));
                }
                Err(crate::wire::WireError::Eof) => break,
                Err(e) => {
                    self.send_value(&err_response(
                        "g?",
                        &ErrorBody::new(crate::wire::code::BAD_FRAME, e.to_string()),
                    ));
                    break;
                }
            }
        }
        self.closed.store(true, Ordering::SeqCst);
        // Wake every local waiter.
        for (_, tx) in self.pending.lock().expect("pending").drain() {
            let _ = tx.send(
                json!({"ok": false, "error": {"code": "INTERNAL", "message": "connection closed"}}),
            );
        }
        if let Some(session) = self.session.lock().expect("session").as_ref() {
            self.broker.close_session(&session.id);
        }
        let pumps: Vec<_> = self
            .pumps
            .lock()
            .map(|mut p| p.drain(..).collect())
            .unwrap_or_default();
        for p in pumps {
            let _ = p.join();
        }
    }

    fn route(self: &Arc<Self>, msg: Value, work: &SyncSender<Value>) {
        let id = msg["id"].as_str().unwrap_or("").to_string();
        if id.starts_with('h') {
            // Answer to one of our host→guest requests.
            if let Some(tx) = self.pending.lock().expect("pending").remove(&id) {
                let _ = tx.send(msg);
            }
            return;
        }
        if msg.get("op").is_some() {
            // WVFAIR: a frame pull gets its own thread, never one of the
            // connection's shared workers. A slow page's `render` used to
            // occupy a worker for seconds, and input / JavaScript-interface
            // calls queued behind it on the same channel — the guest's tap ack
            // arrived 2 s late and the click was lost (WV8 §5). Bounded by
            // [`Conn::dispatch_render`]; the fair gate
            // ([`crate::fair::FrameGate`]) bounds the per-view engine work.
            if msg["op"].as_str() == Some("render") {
                self.dispatch_render(msg);
                return;
            }
            if work.send(msg).is_err() {
                self.closed.store(true, Ordering::SeqCst);
            }
            return;
        }
        // No id and no op: an unsolicited event. v1 guests do not send events;
        // say so instead of dropping it on the floor.
        self.send_value(&err_response(
            &id,
            &ErrorBody::new(
                crate::wire::code::BAD_REQUEST,
                "unsolicited messages must be requests ({id, op}) or responses ({id, ok})",
            ),
        ));
    }

    // --- request dispatch -------------------------------------------------

    /// Run a frame pull on its own thread.
    ///
    /// In-flight pulls are bounded by the session's view budget (plus a small
    /// margin): the gate coalesces per view, so the bound is the number of
    /// views a guest can name, and a guest that floods `render` after its views
    /// are gone gets a structured `FRAME_FAILED` instead of an unbounded thread
    /// spawn.
    fn dispatch_render(self: &Arc<Self>, msg: Value) {
        let limit = self.broker.config().max_views * 2 + 4;
        let in_flight = self.frame_pulls.fetch_add(1, Ordering::SeqCst) + 1;
        if in_flight > limit {
            self.frame_pulls.fetch_sub(1, Ordering::SeqCst);
            let id = msg["id"].as_str().unwrap_or("g?").to_string();
            self.send_value(&err_response(
                &id,
                &ErrorBody::new(
                    crate::wire::code::FRAME_FAILED,
                    format!(
                        "{in_flight} frame pulls are already in flight on this session \
                         (max {limit}); a view has at most one outstanding pull"
                    ),
                ),
            ));
            return;
        }
        let conn = Arc::clone(self);
        let spawned = std::thread::Builder::new()
            .name(format!("wv-frame-{}", in_flight))
            .spawn(move || {
                conn.dispatch(msg);
                conn.frame_pulls.fetch_sub(1, Ordering::SeqCst);
            });
        if spawned.is_err() {
            self.frame_pulls.fetch_sub(1, Ordering::SeqCst);
        }
    }

    fn dispatch(self: &Arc<Self>, req: Value) {
        let id = req["id"].as_str().unwrap_or("g?").to_string();
        let op = req["op"].as_str().unwrap_or("").to_string();
        if op != "hello" && !self.hello.load(Ordering::SeqCst) {
            self.send_value(&err_response(
                &id,
                &ErrorBody::new(
                    crate::wire::code::BAD_REQUEST,
                    "hello must be the first message on a session socket",
                ),
            ));
            return;
        }
        match self.handle(&op, &req) {
            Ok(Outcome::Value(v)) => {
                self.send_response(&ok_response(&id, v), None);
            }
            Ok(Outcome::Frame(v, blob)) => {
                self.send_response(&ok_response(&id, v), Some(&blob));
            }
            Err(e) => {
                let mut body = ErrorBody::new(e.code(), e.to_string());
                if let Error::Proto { code, .. } = &e {
                    body.code = code.clone();
                }
                self.send_value(&err_response(&id, &body));
            }
        }
    }

    fn handle(self: &Arc<Self>, op: &str, req: &Value) -> Result<Outcome> {
        let sid = req["session"].as_str().unwrap_or("");
        match op {
            "hello" => {
                // A version is an integer (`proto: 2`). A guest that spells a
                // float (WV6's `1.1`) or an older number is refused with
                // `PROTO_VERSION` naming this broker's version — never coerced.
                if req["proto"].as_u64().map(|v| v as u32) != Some(PROTO) {
                    return Err(Error::proto(
                        crate::wire::code::PROTO_VERSION,
                        format!(
                            "proto {:?} unsupported; this broker speaks {PROTO}",
                            req["proto"]
                        ),
                    ));
                }
                let session_id = if sid.is_empty() {
                    self.session_id.clone()
                } else {
                    sid.to_string()
                };
                let session = self.broker.open_session(&session_id)?;
                *self.session.lock().expect("session") = Some(Arc::clone(&session));
                self.hello.store(true, Ordering::SeqCst);
                let v = self.broker.engine_version()?;
                Ok(Outcome::Value(json!({
                    "proto": PROTO,
                    "session": session_id,
                    "engine": {
                        "product": v.product,
                        "version": v.version,
                        "revision": v.revision,
                        "protocolVersion": v.protocol_version,
                        "userAgent": v.user_agent,
                    },
                    "limits": {
                        "maxViews": self.broker.config().max_views,
                        "maxFrameBytes": crate::wire::MAX_FRAME,
                    },
                })))
            }
            "ping" => {
                let t0 = Instant::now();
                let _ = self.broker.engine_status();
                Ok(Outcome::Value(json!({
                    "pong": true,
                    "engineMs": (t0.elapsed().as_secs_f64() * 1000.0 * 10.0).round() / 10.0,
                })))
            }
            "engineStatus" => {
                let mut v = self.broker.engine_status();
                v["unanswered"] = json!(self.unanswered.load(Ordering::SeqCst));
                Ok(Outcome::Value(v))
            }
            "memory" => {
                let mut v = self.broker.memory(req["detail"].as_bool().unwrap_or(false));
                v["blobBytes"] = json!(0);
                Ok(Outcome::Value(v))
            }
            _ => {
                let session = self
                    .session
                    .lock()
                    .expect("session")
                    .clone()
                    .ok_or_else(|| {
                        Error::proto(crate::wire::code::INTERNAL, "no session; send hello first")
                    })?;
                self.handle_view_op(op, req, &session)
            }
        }
    }

    fn handle_view_op(
        self: &Arc<Self>,
        op: &str,
        req: &Value,
        session: &Arc<Session>,
    ) -> Result<Outcome> {
        use crate::wire::code;
        let view_id = req["view"].as_i64().unwrap_or(-1);
        // Ops that create/destroy a view are answered before the id lookup.
        match op {
            "view.create" => {
                let vp = Viewport::new(
                    req["width"].as_u64().unwrap_or(0) as u32,
                    req["height"].as_u64().unwrap_or(0) as u32,
                    req["density"].as_f64().unwrap_or(0.0),
                )?;
                let url = req["url"].as_str().unwrap_or("about:blank").to_string();
                let view = self.broker.create_view(session, view_id, vp, &url)?;
                if let Some(ua) = req["userAgent"].as_str() {
                    view.set_user_agent(ua)?;
                }
                if let Some(enabled) = req.get("javascriptEnabled").and_then(Value::as_bool) {
                    view.apply_settings(&json!({"javascriptEnabled": enabled}))?;
                }
                self.spawn_pump(&view);
                Ok(Outcome::Value(json!({
                    "view": view_id,
                    "width": vp.width,
                    "height": vp.height,
                    "density": vp.density,
                })))
            }
            "view.destroy" => {
                self.broker.destroy_view(session, view_id)?;
                Ok(Outcome::Value(json!({"view": view_id, "destroyed": true})))
            }
            _ => {
                let view = self.view(session, view_id)?;
                match op {
                    "view.setViewport" => {
                        let vp = Viewport::new(
                            req["width"].as_u64().unwrap_or(0) as u32,
                            req["height"].as_u64().unwrap_or(0) as u32,
                            req["density"].as_f64().unwrap_or(0.0),
                        )?;
                        view.set_viewport(vp)?;
                        Ok(Outcome::Value(json!({
                            "view": view_id, "width": vp.width, "height": vp.height, "density": vp.density
                        })))
                    }
                    "view.loadUrl" => {
                        let url = req["url"]
                            .as_str()
                            .ok_or_else(|| Error::proto(code::BAD_REQUEST, "loadUrl needs url"))?;
                        let mut r = view.load_url(url)?;
                        r["view"] = json!(view_id);
                        Ok(Outcome::Value(r))
                    }
                    "view.loadData" => {
                        let data = req["data"].as_str().ok_or_else(|| {
                            Error::proto(code::BAD_REQUEST, "loadData needs data")
                        })?;
                        let r = view.load_data(
                            data,
                            req["mime"].as_str().unwrap_or("text/html"),
                            req["encoding"].as_str().unwrap_or("utf8"),
                            req["baseUrl"].as_str(),
                        )?;
                        Ok(Outcome::Value(
                            json!({"view": view_id, "url": r["url"], "loadMs": r["loadMs"]}),
                        ))
                    }
                    "view.reload" => {
                        view.reload()?;
                        Ok(Outcome::Value(json!({"view": view_id})))
                    }
                    "view.stop" => {
                        view.stop()?;
                        Ok(Outcome::Value(json!({"view": view_id})))
                    }
                    "view.setUserAgent" => {
                        let ua = req["userAgent"].as_str().ok_or_else(|| {
                            Error::proto(code::BAD_REQUEST, "setUserAgent needs userAgent")
                        })?;
                        view.set_user_agent(ua)?;
                        Ok(Outcome::Value(json!({"view": view_id, "userAgent": ua})))
                    }
                    "view.setVisibility" => {
                        let visible = req["visible"].as_bool().unwrap_or(true);
                        let window_visible = req["windowVisible"].as_bool().unwrap_or(true);
                        view.set_visibility(visible, window_visible)?;
                        Ok(Outcome::Value(json!({
                            "view": view_id, "visible": visible, "windowVisible": window_visible
                        })))
                    }
                    "view.settings" => {
                        let settings = req.get("settings").ok_or_else(|| {
                            Error::proto(code::BAD_REQUEST, "view.settings needs settings")
                        })?;
                        let (applied, rejected) = view.apply_settings(settings)?;
                        Ok(Outcome::Value(json!({
                            "view": view_id, "applied": applied, "rejected": rejected
                        })))
                    }
                    "view.scrollTo" => {
                        let x = req["x"].as_f64().unwrap_or(0.0);
                        let y = req["y"].as_f64().unwrap_or(0.0);
                        let metrics = view.scroll_to(x, y)?;
                        self.nudge_render(&view, "scroll");
                        let mut out = json!({"view": view_id, "x": x, "y": y});
                        if let (Some(dst), Some(src)) = (out.as_object_mut(), metrics.as_object()) {
                            for (k, v) in src {
                                dst.insert(k.clone(), v.clone());
                            }
                        }
                        Ok(Outcome::Value(out))
                    }
                    "view.evaluateJavascript" => {
                        let script = req["script"].as_str().ok_or_else(|| {
                            Error::proto(code::BAD_REQUEST, "evaluateJavascript needs script")
                        })?;
                        let timeout = Duration::from_millis(
                            req["timeoutMs"]
                                .as_u64()
                                .unwrap_or(30_000)
                                .clamp(1, 300_000),
                        );
                        let mut r = view.evaluate(
                            script,
                            timeout,
                            req["userGesture"].as_bool().unwrap_or(false),
                        )?;
                        r["view"] = json!(view_id);
                        self.nudge_render(&view, "damage");
                        Ok(Outcome::Value(r))
                    }
                    "view.addJavascriptInterface" => {
                        let name = req["name"].as_str().ok_or_else(|| {
                            Error::proto(code::BAD_REQUEST, "addJavascriptInterface needs name")
                        })?;
                        let methods: Vec<String> = req["methods"]
                            .as_array()
                            .map(|a| {
                                a.iter()
                                    .filter_map(|m| m.as_str().map(str::to_string))
                                    .collect()
                            })
                            .unwrap_or_default();
                        view.add_javascript_interface(name, &methods)?;
                        Ok(Outcome::Value(
                            json!({"view": view_id, "name": name, "methods": methods}),
                        ))
                    }
                    "view.removeJavascriptInterface" => {
                        let name = req["name"].as_str().unwrap_or("");
                        view.remove_javascript_interface(name)?;
                        Ok(Outcome::Value(json!({"view": view_id, "name": name})))
                    }
                    "view.history" => {
                        let action = req["action"].as_str().unwrap_or("back");
                        let mut r = view.history(action)?;
                        r["view"] = json!(view_id);
                        Ok(Outcome::Value(r))
                    }
                    "render" => {
                        let format = req["format"].as_str().unwrap_or("png");
                        let timeout = Duration::from_millis(
                            req["timeoutMs"]
                                .as_u64()
                                .unwrap_or(10_000)
                                .clamp(1, 120_000),
                        );
                        let f = view.render(format, timeout)?;
                        let metrics = view.layout_metrics().unwrap_or_else(|_| json!({}));
                        let (dx, dy, dw, dh) = f.damage;
                        let mut meta = json!({
                            "view": view_id,
                            "seq": f.seq,
                            "width": f.width,
                            "height": f.height,
                            "density": f.density,
                            "format": if f.format == crate::wire::FMT_PNG { "png" } else { "rgba" },
                            "bytes": f.bytes.len(),
                            "damage": {"x": dx, "y": dy, "w": dw, "h": dh},
                            "hasDamage": f.has_damage,
                            "engineMs": (f.engine_ms * 10.0).round() / 10.0,
                        });
                        if let (Some(dst), Some(src)) = (meta.as_object_mut(), metrics.as_object())
                        {
                            for (k, v) in src {
                                dst.insert(k.clone(), v.clone());
                            }
                        }
                        let blob = Blob {
                            id: req["id"].as_str().unwrap_or("g?").to_string(),
                            format: f.format,
                            width: f.width,
                            height: f.height,
                            stride: f.stride,
                            seq: f.seq,
                            data: f.bytes.clone(),
                        };
                        Ok(Outcome::Frame(meta, blob))
                    }
                    "ax" => {
                        let mut r = view.ax(
                            req["maxNodes"].as_u64().map(|n| n as usize),
                            req["includeIgnored"].as_bool().unwrap_or(false),
                        )?;
                        r["view"] = json!(view_id);
                        Ok(Outcome::Value(r))
                    }
                    "input.tap" => {
                        let x = req["x"].as_f64().unwrap_or(0.0);
                        let y = req["y"].as_f64().unwrap_or(0.0);
                        let hold = Duration::from_millis(req["holdMs"].as_u64().unwrap_or(40));
                        view.tap(x, y, hold)?;
                        self.emit_focus_event(&view);
                        self.nudge_render(&view, "damage");
                        Ok(Outcome::Value(json!({"view": view_id, "delivered": true})))
                    }
                    "input.touch" => {
                        let action = req["action"].as_str().unwrap_or("down");
                        view.touch(action, req["points"].clone())?;
                        if action == "up" || action == "cancel" {
                            self.nudge_render(&view, "damage");
                        }
                        Ok(Outcome::Value(json!({"view": view_id, "delivered": true})))
                    }
                    "input.scroll" => {
                        view.scroll(
                            req["x"].as_f64().unwrap_or(0.0),
                            req["y"].as_f64().unwrap_or(0.0),
                            req["dx"].as_f64().unwrap_or(0.0),
                            req["dy"].as_f64().unwrap_or(0.0),
                        )?;
                        Ok(Outcome::Value(json!({"view": view_id, "delivered": true})))
                    }
                    "input.key" => {
                        let action = req["action"].as_str().unwrap_or("press");
                        let key = req["key"].as_str().unwrap_or("");
                        let code_name = req["code"].as_str().unwrap_or("");
                        let modifiers: Vec<String> = req["modifiers"]
                            .as_array()
                            .map(|a| {
                                a.iter()
                                    .filter_map(|m| m.as_str().map(str::to_string))
                                    .collect()
                            })
                            .unwrap_or_default();
                        view.key(action, key, code_name, &modifiers)?;
                        Ok(Outcome::Value(json!({"view": view_id, "delivered": true})))
                    }
                    "input.text" => {
                        let text = req["text"].as_str().ok_or_else(|| {
                            Error::proto(code::BAD_REQUEST, "input.text needs text")
                        })?;
                        view.insert_text(text)?;
                        self.emit_focus_event(&view);
                        self.nudge_render(&view, "damage");
                        Ok(Outcome::Value(json!({"view": view_id, "delivered": true})))
                    }
                    "input.setFocus" => {
                        let focused = req["focused"].as_bool().unwrap_or(true);
                        view.set_focus(focused)?;
                        self.emit_focus_event(&view);
                        Ok(Outcome::Value(json!({"view": view_id, "focused": focused})))
                    }
                    "view.setInterceptAll" => {
                        let enabled = req["enabled"].as_bool().unwrap_or(false);
                        view.set_intercept_all(enabled)?;
                        Ok(Outcome::Value(
                            json!({"view": view_id, "interceptAll": enabled}),
                        ))
                    }
                    other => Err(Error::proto(
                        code::UNSUPPORTED,
                        format!("unknown op {other:?}"),
                    )),
                }
            }
        }
    }

    fn view(&self, session: &Arc<Session>, id: i64) -> Result<Arc<crate::view::View>> {
        let view = session
            .view(id)
            .ok_or_else(|| Error::proto(crate::wire::code::NO_VIEW, format!("no view {id}")))?;
        if view.closed.load(Ordering::SeqCst) || view.browser.generation != self.broker.generation()
        {
            return Err(Error::proto(
                crate::wire::code::NO_VIEW,
                format!("view {id} belonged to a previous engine generation; re-create it"),
            ));
        }
        Ok(view)
    }

    // --- per-view event pump ----------------------------------------------

    fn spawn_pump(self: &Arc<Self>, view: &Arc<crate::view::View>) {
        let conn = Arc::clone(self);
        let view = Arc::clone(view);
        let handle = std::thread::Builder::new()
            .name(format!("wv-pump-{}", view.id))
            .spawn(move || conn.pump_loop(view));
        if let Ok(h) = handle {
            if let Ok(mut pumps) = self.pumps.lock() {
                pumps.push(h);
            }
        }
    }

    fn pump_loop(self: &Arc<Self>, view: Arc<crate::view::View>) {
        let mut last_render_event = Instant::now() - Duration::from_secs(1);
        while !self.closed() && !view.closed.load(Ordering::SeqCst) {
            for ev in view.pump_once(Duration::from_millis(200)) {
                if self.closed() {
                    return;
                }
                match ev {
                    crate::view::ViewEvent::NavigationStarted {
                        url,
                        is_new,
                        is_redirect,
                    } => {
                        self.send_event(
                            "navigation.started",
                            json!({"view": view.id, "url": url, "isNewNavigation": is_new, "isRedirect": is_redirect}),
                        );
                    }
                    crate::view::ViewEvent::NavigationRequested { url, is_redirect } => {
                        // shouldOverrideUrlLoading: the guest decides, and a
                        // "no" cancels the navigation before it commits.
                        let answer = self.host_request(
                            "nav.shouldOverride",
                            json!({
                                "view": view.id, "url": url, "isRedirect": is_redirect,
                                "isMainFrame": true, "gesture": true,
                            }),
                        );
                        match answer {
                            Ok(a) if a["allow"].as_bool().unwrap_or(true) => {}
                            Ok(_) => {
                                let _ = view.stop();
                                self.send_event(
                                    "navigation.failed",
                                    json!({"view": view.id, "url": url, "code": "ERR_ABORTED",
                                           "message": "cancelled by shouldOverrideUrlLoading"}),
                                );
                            }
                            Err(_) => {
                                // No provider answer: fail closed (cancel) but do
                                // not claim a load that never happened.
                                let _ = view.stop();
                            }
                        }
                    }
                    crate::view::ViewEvent::NavigationCommitted { url } => {
                        self.send_event(
                            "navigation.committed",
                            json!({"view": view.id, "url": url}),
                        );
                    }
                    crate::view::ViewEvent::NavigationFinished {
                        url,
                        title,
                        can_go_back,
                        can_go_forward,
                    } => {
                        self.send_event(
                            "navigation.finished",
                            json!({"view": view.id, "url": url, "title": title,
                                   "canGoBack": can_go_back, "canGoForward": can_go_forward}),
                        );
                        // A page can autofocus a field while it loads; report it.
                        self.emit_focus_event(&view);
                        // The first paint of a loaded page is a new frame.
                        self.nudge_render(&view, "load");
                    }
                    crate::view::ViewEvent::NavigationFailed { url, code, message } => {
                        self.send_event(
                            "navigation.failed",
                            json!({"view": view.id, "url": url, "code": code, "message": message}),
                        );
                    }
                    crate::view::ViewEvent::RenderInvalidated { reason } => {
                        // Coalesce: a scrolling page must not flood the socket.
                        if last_render_event.elapsed() >= Duration::from_millis(50) {
                            last_render_event = Instant::now();
                            self.send_event(
                                "render.invalidated",
                                json!({"view": view.id, "reason": reason}),
                            );
                        }
                    }
                    crate::view::ViewEvent::BindingCalled { name, payload } => {
                        if name == "__acos_jsi" {
                            let conn = Arc::clone(self);
                            let view = Arc::clone(&view);
                            std::thread::Builder::new()
                                .name("wv-jsi".into())
                                .spawn(move || conn.handle_binding_call(&view, &payload))
                                .ok();
                        }
                    }
                    crate::view::ViewEvent::RequestPaused {
                        request_id,
                        url,
                        method,
                        resource_type,
                        headers,
                        is_asset,
                    } => {
                        if self.intercepts.fetch_add(1, Ordering::SeqCst) >= MAX_INTERCEPTS {
                            self.intercepts.fetch_sub(1, Ordering::SeqCst);
                            let _ = view.fail_request(&request_id, "ERR_INSUFFICIENT_RESOURCES");
                            continue;
                        }
                        let conn = Arc::clone(self);
                        let view = Arc::clone(&view);
                        std::thread::Builder::new()
                            .name("wv-intercept".into())
                            .spawn(move || {
                                conn.handle_intercept(
                                    &view,
                                    request_id,
                                    url,
                                    method,
                                    resource_type,
                                    headers,
                                    is_asset,
                                );
                                conn.intercepts.fetch_sub(1, Ordering::SeqCst);
                            })
                            .ok();
                    }
                }
            }
        }
    }

    /// A page called `window.<iface>.<method>(…)`: forward as `js.call` and
    /// resolve the page-side promise with the provider's answer.
    fn handle_binding_call(&self, view: &Arc<crate::view::View>, payload: &str) {
        let Ok(v) = serde_json::from_str::<Value>(payload) else {
            return;
        };
        let name = v["name"].as_str().unwrap_or("").to_string();
        let method = v["method"].as_str().unwrap_or("").to_string();
        let call_id = v["callId"].as_str().unwrap_or("").to_string();
        let args = serde_json::to_string(&v["args"].clone()).unwrap_or_else(|_| "[]".into());
        let answer = self.host_request(
            "js.call",
            json!({"view": view.id, "name": name, "method": method, "argsJson": args, "callId": call_id}),
        );
        match answer {
            Ok(a) => {
                let result = a["resultJson"].as_str().map(str::to_string);
                let _ = view.resolve_js_call(&call_id, result.as_deref(), a["error"].as_str());
            }
            Err(e) => {
                let _ = view.resolve_js_call(&call_id, None, Some(&e.to_string()));
            }
        }
    }

    /// Ask the guest for a new frame after an action that changed the page.
    /// `render.invalidated` is coalesced by the guest (`WIRE.md` §5.1), so this
    /// is a hint, never a per-pixel frame push.
    fn nudge_render(&self, view: &Arc<crate::view::View>, reason: &str) {
        self.send_event(
            "render.invalidated",
            json!({"view": view.id, "reason": reason}),
        );
    }

    /// v1.1: report the page's focused editable field to the guest. Sent after
    /// every action that can move focus (tap, IME text, `input.setFocus`), so
    /// `onCheckIsTextEditor` answers from engine truth instead of guessing.
    fn emit_focus_event(&self, view: &Arc<crate::view::View>) {
        if let Ok(report) = view.focus_report() {
            let mut fields = json!({"view": view.id});
            if let (Some(dst), Some(src)) = (fields.as_object_mut(), report.as_object()) {
                for (k, v) in src {
                    dst.insert(k.clone(), v.clone());
                }
            }
            self.send_event("view.focusChanged", fields);
        }
    }

    /// A request paused by `Fetch`: the guest serves `file:///android_asset` and
    /// `content://` (and `*` when interception is opted in), or lets it through.
    fn handle_intercept(
        &self,
        view: &Arc<crate::view::View>,
        request_id: String,
        url: String,
        method: String,
        resource_type: String,
        headers: Value,
        is_asset: bool,
    ) {
        let answer = self.host_request(
            "req.intercept",
            json!({
                "view": view.id, "requestId": request_id, "url": url, "method": method,
                "headers": headers, "resourceType": resource_type, "isAsset": is_asset,
            }),
        );
        let result = match answer {
            Ok(v) => v,
            Err(e) => {
                // No provider answer: fail the request loudly on the page.
                let _ = view.fail_request(&request_id, "ERR_TIMED_OUT");
                self.send_event(
                    "navigation.failed",
                    json!({"view": view.id, "url": url, "code": "INTERCEPT_FAILED",
                           "message": truncate(&e.to_string(), 500)}),
                );
                return;
            }
        };
        match result["action"].as_str().unwrap_or("continue") {
            "fulfill" => {
                let body = result["bodyB64"].as_str().unwrap_or("");
                let Some(bytes) = crate::view::base64_decode(body) else {
                    let _ = view.fail_request(&request_id, "ERR_INVALID_RESPONSE");
                    return;
                };
                let status = result["status"].as_u64().unwrap_or(200) as u16;
                let mime = result["mime"]
                    .as_str()
                    .unwrap_or("application/octet-stream");
                let _ = view.fulfill(&request_id, &bytes, status, mime, result["headers"].clone());
            }
            "fail" => {
                let reason = result["reason"].as_str().unwrap_or("ERR_FAILED");
                let _ = view.fail_request(&request_id, reason);
            }
            _ => {
                let _ = view.continue_request(&request_id);
            }
        }
    }
}

/// A dispatch result: a plain value, or a value plus the frame blob that must
/// follow it on the wire (the only two-frame response in the protocol).
enum Outcome {
    Value(Value),
    Frame(Value, Blob),
}

/// Convenience for tests and callers that want the base64 of an asset body.
pub fn asset_body_b64(bytes: &[u8]) -> String {
    base64_encode(bytes)
}
