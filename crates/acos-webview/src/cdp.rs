//! DevTools protocol client over `--remote-debugging-pipe`.
//!
//! Chromium reads commands on fd 3 and writes replies on fd 4 of its own
//! process; both are NUL-terminated UTF-8 JSON. There is no TCP listener, no
//! port and no WebSocket — the guest cannot reach DevTools even in principle
//! (ADR-0031 item 1).
//!
//! Threading model: one reader thread per engine drains fd 4 and either resolves
//! a pending request by id or pushes an event onto the queue of the flattened
//! CDP session that produced it. Every call is blocking with a timeout, so a
//! wedged page surfaces as `Timeout` instead of a hang.

use std::collections::{HashMap, VecDeque};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::io::{FromRawFd, RawFd};
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, RecvTimeoutError, SyncSender};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

/// A DevTools-level failure: the transport died, the browser said no, or the
/// reply did not arrive in time.
#[derive(Debug, thiserror::Error)]
pub enum CdpError {
    #[error("devtools pipe is closed (engine exited)")]
    PipeClosed,
    #[error("devtools: {method}: {message}")]
    Remote { method: String, message: String },
    #[error("devtools timeout after {0:?}: {1}")]
    Timeout(Duration, String),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

/// Per-CDP-session event queue (flattened sessions get their own queue;
/// browser-level events go to the `""` queue).
#[derive(Default)]
pub struct EventQueue {
    inner: Mutex<VecDeque<Value>>,
    cv: Condvar,
}

impl EventQueue {
    pub fn push(&self, event: Value) {
        self.inner.lock().expect("event queue").push_back(event);
        self.cv.notify_all();
    }

    /// Block until an event matching `pred` arrives or `timeout` elapses. Events
    /// that do not match are kept (the caller may match them later).
    pub fn wait_for(&self, pred: impl Fn(&Value) -> bool, timeout: Duration) -> Option<Value> {
        let deadline = Instant::now() + timeout;
        let mut guard = self.inner.lock().expect("event queue");
        loop {
            if let Some(pos) = guard.iter().position(&pred) {
                return guard.remove(pos);
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return None;
            }
            let (next, _) = self.cv.wait_timeout(guard, left).expect("event queue wait");
            guard = next;
        }
    }

    pub fn drain_matching(&self, pred: impl Fn(&Value) -> bool) -> Vec<Value> {
        let mut guard = self.inner.lock().expect("event queue");
        let mut out = Vec::new();
        let mut i = 0;
        while i < guard.len() {
            if pred(&guard[i]) {
                out.push(guard.remove(i).expect("index"));
            } else {
                i += 1;
            }
        }
        out
    }

    pub fn len(&self) -> usize {
        self.inner.lock().expect("event queue").len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

struct Pending {
    tx: SyncSender<Result<Value, CdpError>>,
}

/// A request already on the wire whose reply has not been read yet.
pub struct PendingReply {
    pub id: u64,
    rx: Receiver<Result<Value, CdpError>>,
}

impl PendingReply {
    pub fn wait(self, timeout: Duration) -> Result<Value, CdpError> {
        match self.rx.recv_timeout(timeout) {
            Ok(result) => result,
            Err(RecvTimeoutError::Timeout) => {
                Err(CdpError::Timeout(timeout, format!("request {}", self.id)))
            }
            Err(RecvTimeoutError::Disconnected) => Err(CdpError::PipeClosed),
        }
    }
}

/// One CDP connection (one engine process).
pub struct CdpClient {
    child: Mutex<Child>,
    stdin: Mutex<std::fs::File>,
    pending: Arc<Mutex<HashMap<u64, Pending>>>,
    queues: Arc<Mutex<HashMap<String, Arc<EventQueue>>>>,
    next_id: AtomicU64,
    dead: Arc<AtomicBool>,
    /// Command line minus the program, for verdicts and diagnostics.
    pub argv: Vec<String>,
    /// Last 32 KiB of the engine's stderr, for launch failures.
    pub stderr_tail: Arc<Mutex<VecDeque<String>>>,
    pid: u32,
}

impl CdpClient {
    /// Spawn `cmd` (already carrying the `--remote-debugging-pipe` flag and the
    /// sandbox wrapper) and take over fds 3/4.
    ///
    /// The `unsafe` here is descriptor plumbing between fork and exec, the same
    /// pattern `acos-art` uses for its guest: two pipes, `dup2` onto 3 and 4,
    /// nothing else. It is not a sandbox entry point — the sandbox is bwrap's
    /// argv (see [`crate::sandbox`]).
    pub fn spawn(mut cmd: Command) -> Result<Arc<Self>, CdpError> {
        // A: parent -> engine (fd 3). B: engine -> parent (fd 4).
        let (child_read, parent_write_raw) = pipe()?;
        // SAFETY: `parent_write_raw` came from `pipe()` and is owned here.
        let parent_write = unsafe { std::fs::File::from_raw_fd(parent_write_raw) };
        let (parent_read_raw, child_write) = pipe()?;
        // SAFETY: `parent_read_raw` came from `pipe()` and is owned here.
        let parent_read = unsafe { std::fs::File::from_raw_fd(parent_read_raw) };
        cmd.stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        // SAFETY: `pre_exec` runs in the forked child before `exec`. The two
        // calls are `dup2` on descriptors this function created, and on failure
        // the error is reported to the parent through `Command`.
        unsafe {
            cmd.pre_exec(move || {
                for (from, to) in [(child_read, 3), (child_write, 4)] {
                    // `dup2(x, x)` is a no-op and would keep O_CLOEXEC, which
                    // closes the descriptor at exec — the engine then exits with
                    // "Remote debugging pipe file descriptors are not open".
                    if from != to && libc::dup2(from, to) < 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                    if libc::fcntl(to, libc::F_SETFD, 0) < 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                }
                Ok(())
            });
        }
        let argv: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        let mut child = cmd.spawn()?;
        // The child's ends are closed in the parent so EOF on fd 4 means the
        // engine is gone (leaving the write end open in the parent would keep
        // the reader thread from ever seeing EOF).
        // SAFETY: both descriptors were created by `pipe()` above and are owned
        // by the parent until now.
        unsafe {
            libc::close(child_read);
            libc::close(child_write);
        }
        let pid = child.id();

        let stderr_tail = Arc::new(Mutex::new(VecDeque::new()));
        if let Some(stderr) = child.stderr.take() {
            let tail = Arc::clone(&stderr_tail);
            std::thread::Builder::new()
                .name("wv-engine-stderr".into())
                .spawn(move || {
                    for line in BufReader::new(stderr).lines() {
                        let Ok(line) = line else { break };
                        let mut tail = tail.lock().expect("stderr tail");
                        tail.push_back(line);
                        while tail.len() > 200 {
                            tail.pop_front();
                        }
                    }
                })
                .ok();
        }

        let client = Arc::new(Self {
            child: Mutex::new(child),
            stdin: Mutex::new(parent_write),
            pending: Arc::new(Mutex::new(HashMap::new())),
            queues: Arc::new(Mutex::new(HashMap::new())),
            next_id: AtomicU64::new(1),
            dead: Arc::new(AtomicBool::new(false)),
            argv,
            stderr_tail,
            pid,
        });

        let reader = Arc::clone(&client);
        std::thread::Builder::new()
            .name("wv-engine-reader".into())
            .spawn(move || reader.read_loop(parent_read))
            .map_err(CdpError::Io)?;

        Ok(client)
    }

    pub fn pid(&self) -> u32 {
        self.pid
    }

    pub fn is_dead(&self) -> bool {
        self.dead.load(Ordering::SeqCst)
    }

    /// The event queue for one flattened CDP session (`""` = browser level).
    pub fn queue(&self, session: &str) -> Arc<EventQueue> {
        let mut queues = self.queues.lock().expect("queues");
        Arc::clone(queues.entry(session.to_string()).or_default())
    }

    /// Send a request and keep the reply for later. Used where the reply can
    /// only arrive after the caller produces a compositor frame (touch input
    /// under `--enable-begin-frame-control`).
    pub fn send(
        &self,
        method: &str,
        params: Value,
        session: Option<&str>,
    ) -> Result<PendingReply, CdpError> {
        if self.is_dead() {
            return Err(CdpError::PipeClosed);
        }
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = sync_channel(1);
        self.pending
            .lock()
            .expect("pending")
            .insert(id, Pending { tx });
        let mut msg = json!({"id": id, "method": method, "params": params});
        if let Some(s) = session {
            msg["sessionId"] = Value::String(s.to_string());
        }
        let mut line = serde_json::to_vec(&msg)
            .map_err(|e| CdpError::Io(std::io::Error::new(std::io::ErrorKind::InvalidData, e)))?;
        line.push(0);
        {
            let mut stdin = self.stdin.lock().expect("stdin");
            if let Err(e) = stdin.write_all(&line).and_then(|()| stdin.flush()) {
                self.pending.lock().expect("pending").remove(&id);
                self.dead.store(true, Ordering::SeqCst);
                return Err(CdpError::Io(e));
            }
        }
        Ok(PendingReply { id, rx })
    }

    /// Blocking request/response with a timeout.
    pub fn call(
        &self,
        method: &str,
        params: Value,
        session: Option<&str>,
        timeout: Duration,
    ) -> Result<Value, CdpError> {
        if self.is_dead() {
            return Err(CdpError::PipeClosed);
        }
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = sync_channel(1);
        self.pending
            .lock()
            .expect("pending")
            .insert(id, Pending { tx });
        let mut msg = json!({"id": id, "method": method, "params": params});
        if let Some(s) = session {
            msg["sessionId"] = Value::String(s.to_string());
        }
        let mut line = serde_json::to_vec(&msg)
            .map_err(|e| CdpError::Io(std::io::Error::new(std::io::ErrorKind::InvalidData, e)))?;
        line.push(0);
        {
            let mut stdin = self.stdin.lock().expect("stdin");
            if let Err(e) = stdin.write_all(&line).and_then(|()| stdin.flush()) {
                self.pending.lock().expect("pending").remove(&id);
                self.dead.store(true, Ordering::SeqCst);
                return Err(CdpError::Io(e));
            }
        }
        match rx.recv_timeout(timeout) {
            Ok(Ok(v)) => Ok(v),
            Ok(Err(e)) => Err(e),
            Err(RecvTimeoutError::Timeout) => {
                self.pending.lock().expect("pending").remove(&id);
                Err(CdpError::Timeout(timeout, method.to_string()))
            }
            Err(RecvTimeoutError::Disconnected) => Err(CdpError::PipeClosed),
        }
    }

    fn read_loop(self: Arc<Self>, read_end: std::fs::File) {
        let mut reader = BufReader::new(read_end);
        let mut buf = Vec::new();
        loop {
            buf.clear();
            match reader.read_until(0, &mut buf) {
                Ok(0) => break,
                Ok(_) => {}
                Err(_) => break,
            }
            if buf.last() == Some(&0) {
                buf.pop();
            }
            if buf.is_empty() {
                continue;
            }
            let Ok(msg) = serde_json::from_slice::<Value>(&buf) else {
                continue;
            };
            if let Some(id) = msg.get("id").and_then(Value::as_u64) {
                let pending = self.pending.lock().expect("pending").remove(&id);
                if let Some(p) = pending {
                    let out = match msg.get("error") {
                        Some(err) => Err(CdpError::Remote {
                            method: err
                                .get("message")
                                .and_then(Value::as_str)
                                .unwrap_or("error")
                                .to_string(),
                            message: err.to_string(),
                        }),
                        None => Ok(msg.get("result").cloned().unwrap_or(Value::Null)),
                    };
                    let _ = p.tx.send(out);
                }
                continue;
            }
            if msg.get("method").is_some() {
                let session = msg
                    .get("sessionId")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                self.queue(&session).push(msg);
            }
        }
        self.dead.store(true, Ordering::SeqCst);
        // Unblock every waiter.
        let mut pending = self.pending.lock().expect("pending");
        for (_, p) in pending.drain() {
            let _ = p.tx.send(Err(CdpError::PipeClosed));
        }
        drop(pending);
        for q in self.queues.lock().expect("queues").values() {
            q.push(json!({"method": "__pipe_closed__"}));
        }
    }

    /// Stop the engine: the pipe's EOF ends the reader thread.
    pub fn shutdown(&self) {
        if let Ok(mut child) = self.child.lock() {
            let _ = child.kill();
            let _ = child.wait();
        }
        self.dead.store(true, Ordering::SeqCst);
    }

    pub fn exited(&self) -> bool {
        if self.is_dead() {
            return true;
        }
        match self.child.lock() {
            Ok(mut child) => matches!(child.try_wait(), Ok(Some(_)) | Err(_)),
            Err(_) => true,
        }
    }

    pub fn stderr_tail(&self) -> String {
        self.stderr_tail
            .lock()
            .map(|t| t.iter().cloned().collect::<Vec<_>>().join("\n"))
            .unwrap_or_default()
    }
}

impl Drop for CdpClient {
    fn drop(&mut self) {
        if let Ok(child) = self.child.get_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// A pipe as raw `(read end, write end)`; callers decide ownership.
fn pipe() -> Result<(RawFd, RawFd), CdpError> {
    let mut fds = [0 as RawFd; 2];
    // SAFETY: `fds` is a two-element out parameter. O_CLOEXEC keeps both ends
    // out of every later `exec`, including the engine's own renderer children;
    // `dup2` in `pre_exec` clears it on fds 3/4 for the engine itself.
    let rc = unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) };
    if rc != 0 {
        return Err(CdpError::Io(std::io::Error::last_os_error()));
    }
    Ok((fds[0], fds[1]))
}
