//! The session hub behind `acos view` (VIEW1b, consumer of protocol v1).
//!
//! The hub is the **only** subscriber: it reads the session registry, connects
//! to a session's `stream_socket` when a browser opens that session, and fans the
//! frames/events out to every browser connection. Keeping one upstream
//! subscription per session is what preserves VIEW1a's cost model — the producer
//! encodes once no matter how many people watch — and dropping it when the last
//! browser leaves is what makes an unwatched session cost what it cost before
//! the viewer existed (`docs/LIVE_VIEW.md` §3).
//!
//! Rules the hub enforces:
//!
//! * stale registry entries (dead `daemon_pid`) are hidden from the list and
//!   swept from disk;
//! * the upstream subscription exists exactly while `watchers > 0`; the last
//!   detach joins the upstream thread before it returns, so the socket really is
//!   closed when the page/tab closes;
//! * a slow browser never blocks the producer or another browser: each browser
//!   reads the newest frame from the shared slot, so it skips frames it cannot
//!   keep up with, and a socket write that times out drops that connection;
//! * the event feed is a bounded ring (oldest dropped, counted) and every
//!   message is capped at 64 MiB, per `docs/LIVE_VIEW.md` §2.

use std::collections::{BTreeMap, VecDeque};
use std::io::{self, ErrorKind, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::live_registry;

/// Bytes of event history kept for late browser connections.
pub const EVENT_RING: usize = 128;
/// A protocol message larger than this is a framing error (`LIVE_VIEW.md` §2).
pub const MAX_MESSAGE: u32 = 64 * 1024 * 1024;
/// Upstream read slice: short enough that a stopping hub closes the socket fast.
const READ_TIMEOUT: Duration = Duration::from_millis(250);

/// One published session, parsed from a registry entry (`docs/LIVE_VIEW.md` §5).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub session_id: String,
    pub daemon_pid: u64,
    pub launcher_pid: Option<u64>,
    pub started_ms: u64,
    pub client_name: String,
    pub client_version: String,
    pub project_dir: String,
    pub apk: String,
    pub package: String,
    pub activity: Option<String>,
    pub display: (u32, u32),
    pub stream_socket: Option<String>,
}

impl Entry {
    /// Parse a registry document; a document without a session id or a daemon
    /// pid is not a session.
    #[must_use]
    pub fn from_doc(doc: &Value) -> Option<Self> {
        let session_id = doc.get("session_id").and_then(Value::as_str)?.to_string();
        let daemon_pid = doc.get("daemon_pid").and_then(Value::as_u64)?;
        let display = doc.get("display");
        let activity = doc
            .get("activity")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .map(str::to_string);
        Some(Self {
            session_id,
            daemon_pid,
            launcher_pid: doc.get("launcher_pid").and_then(Value::as_u64),
            started_ms: doc.get("started_ms").and_then(Value::as_u64).unwrap_or(0),
            client_name: doc
                .get("client")
                .and_then(|client| client.get("name"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            client_version: doc
                .get("client")
                .and_then(|client| client.get("version"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            project_dir: doc
                .get("project_dir")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            apk: doc
                .get("apk")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            package: doc
                .get("package")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            activity,
            display: (
                display
                    .and_then(|d| d.get("w"))
                    .and_then(Value::as_u64)
                    .unwrap_or(0) as u32,
                display
                    .and_then(|d| d.get("h"))
                    .and_then(Value::as_u64)
                    .unwrap_or(0) as u32,
            ),
            stream_socket: doc
                .get("stream_socket")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .map(str::to_string),
        })
    }

    /// The JSON shape the page consumes (the registry entry plus what the hub
    /// knows about the stream).
    #[must_use]
    pub fn to_json(&self) -> Value {
        json!({
            "session_id": self.session_id,
            "daemon_pid": self.daemon_pid,
            "launcher_pid": self.launcher_pid,
            "started_ms": self.started_ms,
            "client": {"name": self.client_name, "version": self.client_version},
            "project_dir": self.project_dir,
            "apk": self.apk,
            "package": self.package,
            "activity": self.activity,
            "display": {"w": self.display.0, "h": self.display.1},
            "stream": self.stream_socket.is_some(),
        })
    }
}

/// One encoded frame from the producer.
#[derive(Debug)]
pub struct Frame {
    /// Hub-local monotonic index (survives a producer counter reset).
    pub index: u64,
    /// The producer's C1 store counter.
    pub seq: u64,
    pub w: u32,
    pub h: u32,
    pub format: String,
    pub ts_ms: u64,
    pub bytes: Vec<u8>,
}

/// One action/activity/lifecycle event from the producer's feed.
#[derive(Clone, Debug)]
pub struct Event {
    /// Hub-local monotonic index.
    pub index: u64,
    pub kind: String,
    pub ts_ms: u64,
    pub value: Value,
}

/// What a browser connection sees of a session for the list.
#[derive(Clone, Debug)]
pub struct SessionView {
    pub entry: Entry,
    pub watchers: usize,
    pub connected: bool,
}

/// A batch of events and the cursor to ask from next time.
#[derive(Clone, Debug, Default)]
pub struct EventBatch {
    pub events: Vec<Event>,
    /// Events dropped while this browser was not reading (bounded ring).
    pub missed: u64,
}

/// The hub: registry discovery plus one shared upstream per watched session.
pub struct Hub {
    dir: PathBuf,
    inner: Mutex<Inner>,
}

struct Inner {
    entries: BTreeMap<String, Entry>,
    slots: BTreeMap<String, Arc<Slot>>,
}

impl Hub {
    /// A hub reading `dir` (the registry directory).
    #[must_use]
    pub fn new(dir: PathBuf) -> Self {
        Self {
            dir,
            inner: Mutex::new(Inner {
                entries: BTreeMap::new(),
                slots: BTreeMap::new(),
            }),
        }
    }

    /// The registry directory this hub reads.
    #[must_use]
    pub fn dir(&self) -> &PathBuf {
        &self.dir
    }

    /// Re-read the registry: hide (and delete) stale entries, refresh metadata.
    fn refresh_locked(&self, inner: &mut Inner) {
        let mut entries = BTreeMap::new();
        for read in live_registry::read_entries(&self.dir) {
            if read.stale {
                // A SIGKILLed daemon cannot remove its own file; do it for it.
                let _ = std::fs::remove_file(&read.path);
                continue;
            }
            if let Some(entry) = Entry::from_doc(&read.doc) {
                entries.insert(entry.session_id.clone(), entry);
            }
        }
        for (key, slot) in &inner.slots {
            if let Some(entry) = entries.get(key) {
                *slot.meta.lock().expect("slot meta") = entry.clone();
            }
        }
        // Keep a slot while a browser still watches it (the registry entry may
        // be written/removed around a destroy) or while its upstream is alive.
        inner
            .slots
            .retain(|key, slot| entries.contains_key(key) || slot.is_attached());
        inner.entries = entries;
    }

    /// The visible sessions, newest first. Stale entries are swept here.
    #[must_use]
    pub fn list(&self) -> Vec<SessionView> {
        let mut inner = self.inner.lock().expect("hub");
        self.refresh_locked(&mut inner);
        let mut views: Vec<SessionView> = inner
            .entries
            .values()
            .map(|entry| {
                let (watchers, connected) = inner
                    .slots
                    .get(&entry.session_id)
                    .map_or((0, false), |slot| slot.observed());
                SessionView {
                    entry: entry.clone(),
                    watchers,
                    connected,
                }
            })
            .collect();
        views.sort_by(|a, b| b.entry.started_ms.cmp(&a.entry.started_ms));
        views
    }

    /// A registry entry by session id (refreshing first).
    #[must_use]
    pub fn entry(&self, session_id: &str) -> Option<Entry> {
        let mut inner = self.inner.lock().expect("hub");
        self.refresh_locked(&mut inner);
        inner.entries.get(session_id).cloned()
    }

    /// Attach a browser to a session: the first watcher starts the one upstream
    /// subscription, later watchers share it.
    ///
    /// `None` when the session is not live or has no stream socket (a metadata
    /// only entry).
    #[must_use]
    pub fn watch(&self, session_id: &str) -> Option<Watcher> {
        let entry = self.entry(session_id)?;
        let socket = entry.stream_socket.clone()?;
        if !std::path::Path::new(&socket).exists() {
            return None;
        }
        let slot = {
            let mut inner = self.inner.lock().expect("hub");
            if let Some(slot) = inner.slots.get(session_id) {
                *slot.meta.lock().expect("slot meta") = entry.clone();
                Arc::clone(slot)
            } else {
                let slot = Arc::new(Slot::new(entry));
                inner
                    .slots
                    .insert(session_id.to_string(), Arc::clone(&slot));
                slot
            }
        };
        slot.attach(socket);
        Some(Watcher { slot })
    }

    /// Per-session counters (the hub's test seam, and what a test asserts on
    /// to prove one upstream per session and zero after the last browser).
    #[must_use]
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn stats(&self) -> Vec<SlotStats> {
        let inner = self.inner.lock().expect("hub");
        let mut out: Vec<SlotStats> = inner
            .slots
            .values()
            .map(|slot| slot.stats())
            .collect();
        out.sort_by(|a, b| a.session_id.cmp(&b.session_id));
        out
    }
}

/// Counters for one session slot (the hub's own seam for tests).
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(not(test), allow(dead_code))]
pub struct SlotStats {
    pub session_id: String,
    pub watchers: usize,
    pub connected: bool,
    pub upstream_connects: u64,
    pub frames: u64,
    pub events: u64,
    pub errors: u64,
}

/// A browser's subscription to one session. Dropping it detaches; the last
/// detach closes the upstream socket (and joeins its thread) before returning.
pub struct Watcher {
    slot: Arc<Slot>,
}

impl Watcher {
    /// The session's current registry metadata.
    #[must_use]
    pub fn entry(&self) -> Entry {
        self.slot.meta.lock().expect("slot meta").clone()
    }

    /// The producer's hello, once received.
    #[must_use]
    pub fn hello(&self) -> Option<Value> {
        self.slot
            .state
            .lock()
            .expect("slot")
            .hello
            .clone()
    }

    /// `(connected, last_error)` for the upstream stream.
    #[must_use]
    pub fn stream_state(&self) -> (bool, Option<String>) {
        let state = self.slot.state.lock().expect("slot");
        (state.connected, state.error.clone())
    }

    /// Counters for this slot (test seam).
    #[must_use]
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn stats(&self) -> SlotStats {
        self.slot.stats()
    }

    /// Wait for a frame newer than `after` (hub-local index), up to `timeout`.
    /// A cached frame is returned immediately when it is newer.
    #[must_use]
    pub fn wait_frame(&self, after: u64, timeout: Duration) -> Option<Arc<Frame>> {
        let mut state = self.slot.state.lock().expect("slot");
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(frame) = &state.frame {
                if frame.index > after {
                    return Some(Arc::clone(frame));
                }
            }
            let now = Instant::now();
            if now >= deadline {
                return None;
            }
            let (guard, _) = self
                .slot
                .cv
                .wait_timeout(state, deadline - now)
                .unwrap_or_else(|poison| poison.into_inner());
            state = guard;
        }
    }

    /// The newest frame index (a browser's starting cursor).
    #[must_use]
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn frame_cursor(&self) -> u64 {
        self.slot.state.lock().expect("slot").frame_index
    }

    /// The event cursor at attach: a late browser sees the buffered tail.
    #[must_use]
    pub fn event_cursor(&self) -> u64 {
        let state = self.slot.state.lock().expect("slot");
        state
            .events
            .front()
            .map_or(state.event_index, |event| event.index.saturating_sub(1))
    }

    /// Wait for events after `after`, up to `timeout`.
    #[must_use]
    pub fn wait_events(&self, after: u64, timeout: Duration) -> EventBatch {
        let mut state = self.slot.state.lock().expect("slot");
        let deadline = Instant::now() + timeout;
        loop {
            let batch = collect_events(&state, after);
            if !batch.events.is_empty() {
                return batch;
            }
            let now = Instant::now();
            if now >= deadline {
                return batch;
            }
            let (guard, _) = self
                .slot
                .cv
                .wait_timeout(state, deadline - now)
                .unwrap_or_else(|poison| poison.into_inner());
            state = guard;
        }
    }
}

impl Drop for Watcher {
    fn drop(&mut self) {
        self.slot.detach();
    }
}

/// The uid at the server end of a connected Unix socket must be this process's.
///
/// Public so the unit tests (and the producer's own check) share one reading of
/// `SO_PEERCRED`.
pub fn peer_uid(stream: &UnixStream) -> io::Result<u32> {
    use std::os::unix::io::AsRawFd;
    let mut credentials = libc::ucred {
        pid: 0,
        uid: 0,
        gid: 0,
    };
    let mut length = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    let code = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&raw mut credentials).cast(),
            &mut length,
        )
    };
    if code != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(credentials.uid)
}

fn verify_peer_uid(stream: &UnixStream) -> io::Result<()> {
    let uid = unsafe { libc::getuid() };
    let peer = peer_uid(stream)?;
    if peer == uid {
        Ok(())
    } else {
        Err(io::Error::new(
            ErrorKind::PermissionDenied,
            format!("the socket's server runs as uid {peer}, not {uid}"),
        ))
    }
}

fn collect_events(state: &SlotState, after: u64) -> EventBatch {
    let mut batch = EventBatch::default();
    let oldest = state.events.front().map(|event| event.index);
    if let Some(oldest) = oldest {
        if after + 1 < oldest {
            batch.missed = oldest - after - 1;
        }
    }
    batch.events = state
        .events
        .iter()
        .filter(|event| event.index > after)
        .cloned()
        .collect();
    batch
}

/// Where a session's shared stream lives.
struct Slot {
    meta: Mutex<Entry>,
    state: Mutex<SlotState>,
    cv: Condvar,
}

struct SlotState {
    watchers: usize,
    running: bool,
    stopping: bool,
    handle: Option<std::thread::JoinHandle<()>>,
    /// The stream socket the upstream thread connects to (kept so a watcher
    /// that arrives while the last one is detaching can restart the upstream).
    socket: Option<String>,
    connected: bool,
    error: Option<String>,
    hello: Option<Value>,
    frame: Option<Arc<Frame>>,
    frame_index: u64,
    events: VecDeque<Event>,
    event_index: u64,
    upstream_connects: u64,
    upstream_errors: u64,
}

impl Slot {
    fn new(entry: Entry) -> Self {
        Self {
            meta: Mutex::new(entry),
            state: Mutex::new(SlotState {
                watchers: 0,
                running: false,
                stopping: false,
                handle: None,
                socket: None,
                connected: false,
                error: None,
                hello: None,
                frame: None,
                frame_index: 0,
                events: VecDeque::new(),
                event_index: 0,
                upstream_connects: 0,
                upstream_errors: 0,
            }),
            cv: Condvar::new(),
        }
    }

    fn stats(&self) -> SlotStats {
        let state = self.state.lock().expect("slot");
        SlotStats {
            session_id: self.meta.lock().expect("slot meta").session_id.clone(),
            watchers: state.watchers,
            connected: state.connected,
            upstream_connects: state.upstream_connects,
            frames: state.frame_index,
            events: state.event_index,
            errors: state.upstream_errors,
        }
    }

    fn observed(&self) -> (usize, bool) {
        let state = self.state.lock().expect("slot");
        (state.watchers, state.connected)
    }

    /// `true` while a browser watches or an upstream thread is alive.
    fn is_attached(&self) -> bool {
        let state = self.state.lock().expect("slot");
        state.watchers > 0 || state.running
    }

    /// Another browser arrives: the first one starts the upstream subscription.
    fn attach(self: &Arc<Self>, socket: String) {
        let mut state = self.state.lock().expect("slot");
        state.watchers += 1;
        state.socket = Some(socket.clone());
        if state.watchers == 1 && !state.running {
            state.running = true;
            state.stopping = false;
            let slot = Arc::clone(self);
            state.handle = Some(std::thread::spawn(move || slot.upstream_loop(socket)));
        }
    }

    /// A browser leaves: the last one stops (and joins) the upstream thread, so
    /// the producer's subscriber count is back to zero when this returns.
    fn detach(self: &Arc<Self>) {
        let handle = {
            let mut state = self.state.lock().expect("slot");
            state.watchers = state.watchers.saturating_sub(1);
            if state.watchers > 0 {
                return;
            }
            state.stopping = true;
            self.cv.notify_all();
            state.handle.take()
        };
        if let Some(handle) = handle {
            let _ = handle.join();
        }
        let mut state = self.state.lock().expect("slot");
        state.running = false;
        state.stopping = false;
        state.connected = false;
        state.hello = None;
        // VIEW1c L4: a browser that attached while this detach was joining saw
        // `running == true` and started nothing. Restart the upstream for it,
        // or its stream would stay silently dead.
        if state.watchers > 0 {
            if let Some(socket) = state.socket.clone() {
                state.running = true;
                let slot = Arc::clone(self);
                state.handle = Some(std::thread::spawn(move || slot.upstream_loop(socket)));
            }
        }
        self.cv.notify_all();
    }

    fn stopping(&self) -> bool {
        self.state.lock().expect("slot").stopping
    }

    /// Sleep, waking early when the last browser leaves.
    fn sleep_interruptible(&self, total: Duration) {
        let start = Instant::now();
        while start.elapsed() < total {
            if self.stopping() {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// The one upstream subscription: connect, subscribe, pump, reconnect while
    /// a browser still watches.
    fn upstream_loop(self: Arc<Self>, socket: String) {
        let mut backoff = Duration::from_millis(100);
        while !self.stopping() {
            match UnixStream::connect(&socket) {
                Ok(mut stream) => {
                    // VIEW1c M1: only trust a socket whose server runs as our own
                    // uid (`SO_PEERCRED`); a socket planted by another user
                    // would feed the browser spoofed frames and events.
                    if let Err(error) = verify_peer_uid(&stream) {
                        let mut state = self.state.lock().expect("slot");
                        state.error = Some(format!("refused {socket}: {error}"));
                        state.upstream_errors += 1;
                        drop(state);
                        self.cv.notify_all();
                        drop(stream);
                        self.sleep_interruptible(backoff);
                        backoff = (backoff * 2).min(Duration::from_secs(2));
                        continue;
                    }
                    let _ = stream.set_read_timeout(Some(READ_TIMEOUT));
                    let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));
                    {
                        let mut state = self.state.lock().expect("slot");
                        state.connected = true;
                        state.error = None;
                        state.upstream_connects += 1;
                    }
                    self.cv.notify_all();
                    let outcome = self.pump(&mut stream);
                    {
                        let mut state = self.state.lock().expect("slot");
                        state.connected = false;
                        if let Err(error) = &outcome {
                            state.error = Some(error.to_string());
                            state.upstream_errors += 1;
                        }
                    }
                    self.cv.notify_all();
                    drop(stream); // closing here is what returns the producer to 0
                    backoff = Duration::from_millis(100);
                }
                Err(error) => {
                    let mut state = self.state.lock().expect("slot");
                    state.error = Some(format!("connect {socket}: {error}"));
                    state.upstream_errors += 1;
                    drop(state);
                    self.cv.notify_all();
                }
            }
            self.sleep_interruptible(backoff);
            backoff = (backoff * 2).min(Duration::from_secs(2));
        }
    }

    /// Protocol v1 client: one subscribe line in, `H`/`F`/`E` out.
    fn pump(&self, stream: &mut UnixStream) -> io::Result<()> {
        // One encoder per session: the first subscriber fixes format/scale/fps.
        let subscribe = json!({
            "v": 1,
            "subscribe": {
                "frames": true,
                "events": true,
                "max_fps": 15,
                "scale": 0.5,
                "format": "png",
            }
        });
        stream.write_all(subscribe.to_string().as_bytes())?;
        stream.write_all(b"\n")?;
        stream.flush()?;

        let mut len_bytes = [0u8; 4];
        loop {
            self.read_exact(stream, &mut len_bytes)?;
            let length = u32::from_be_bytes(len_bytes);
            if length == 0 || length > MAX_MESSAGE {
                return Err(framing(format!("bad message length {length}")));
            }
            let mut body = vec![0u8; length as usize];
            self.read_exact(stream, &mut body)?;
            let kind = body[0];
            let payload = &body[1..];
            match kind {
                b'H' => {
                    let hello: Value = serde_json::from_slice(payload)
                        .map_err(|error| framing(format!("hello json: {error}")))?;
                    self.state.lock().expect("slot").hello = Some(hello);
                    self.cv.notify_all();
                }
                b'F' => self.push_frame(payload)?,
                b'E' => self.push_event(payload)?,
                other => {
                    return Err(framing(format!("unknown message type {other:#x}")));
                }
            }
        }
    }

    fn push_frame(&self, payload: &[u8]) -> io::Result<()> {
        let split = payload
            .iter()
            .position(|byte| *byte == b'\n')
            .ok_or_else(|| framing("frame without a header line"))?;
        let header: Value = serde_json::from_slice(&payload[..split])
            .map_err(|error| framing(format!("frame header json: {error}")))?;
        let bytes = &payload[split + 1..];
        if bytes.is_empty() {
            return Err(framing("frame without image bytes"));
        }
        let format = header
            .get("format")
            .and_then(Value::as_str)
            .unwrap_or("png")
            .to_string();
        let mut state = self.state.lock().expect("slot");
        state.frame_index += 1;
        state.frame = Some(Arc::new(Frame {
            index: state.frame_index,
            seq: header.get("seq").and_then(Value::as_u64).unwrap_or(0),
            w: header.get("w").and_then(Value::as_u64).unwrap_or(0) as u32,
            h: header.get("h").and_then(Value::as_u64).unwrap_or(0) as u32,
            format,
            ts_ms: header.get("ts_ms").and_then(Value::as_u64).unwrap_or(0),
            bytes: bytes.to_vec(),
        }));
        drop(state);
        self.cv.notify_all();
        Ok(())
    }

    fn push_event(&self, payload: &[u8]) -> io::Result<()> {
        let value: Value = serde_json::from_slice(payload)
            .map_err(|error| framing(format!("event json: {error}")))?;
        let mut state = self.state.lock().expect("slot");
        state.event_index += 1;
        let event = Event {
            index: state.event_index,
            kind: value
                .get("kind")
                .and_then(Value::as_str)
                .unwrap_or("event")
                .to_string(),
            ts_ms: value
                .get("ts_ms")
                .and_then(Value::as_u64)
                .unwrap_or_else(live_registry::now_ms),
            value,
        };
        state.events.push_back(event);
        while state.events.len() > EVENT_RING {
            state.events.pop_front();
        }
        drop(state);
        self.cv.notify_all();
        Ok(())
    }

    /// `read_exact` that survives the read timeout and notices a stop request.
    fn read_exact(&self, stream: &mut UnixStream, buffer: &mut [u8]) -> io::Result<()> {
        let mut filled = 0;
        while filled < buffer.len() {
            if self.stopping() {
                return Err(io::Error::new(ErrorKind::Interrupted, "viewer stopped"));
            }
            match stream.read(&mut buffer[filled..]) {
                Ok(0) => {
                    return Err(io::Error::new(
                        ErrorKind::UnexpectedEof,
                        "producer closed the stream",
                    ));
                }
                Ok(read) => filled += read,
                Err(error)
                    if matches!(
                        error.kind(),
                        ErrorKind::WouldBlock | ErrorKind::TimedOut | ErrorKind::Interrupted
                    ) =>
                {
                    continue;
                }
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }
}

fn framing(message: impl std::fmt::Display) -> io::Error {
    io::Error::new(ErrorKind::InvalidData, message.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader};
    use std::os::unix::net::UnixListener;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A fake producer: protocol v1 over a Unix socket in a scratch directory.
    /// Counts accepted connections and serves hello + frames until the reader
    /// goes away (which is what the `subscribers` seam observes).
    struct FakeProducer {
        socket: PathBuf,
        accepts: Arc<AtomicUsize>,
        serving: Arc<AtomicUsize>,
        root: PathBuf,
    }

    fn scratch(tag: &str) -> PathBuf {
        let root = std::env::var_os("ACOS_VIEW_TEST_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                std::env::current_dir()
                    .unwrap_or_else(|_| PathBuf::from("."))
                    .join(".cache/lv")
            });
        let dir = root.join(format!("{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch");
        dir
    }

    impl FakeProducer {
        fn start(tag: &str) -> Self {
            let root = scratch(tag);
            let socket = root.join("view.sock");
            let listener = UnixListener::bind(&socket).expect("bind fake producer");
            let accepts = Arc::new(AtomicUsize::new(0));
            let serving = Arc::new(AtomicUsize::new(0));
            let accepts_for_thread = Arc::clone(&accepts);
            let serving_for_thread = Arc::clone(&serving);
            std::thread::spawn(move || {
                for stream in listener.incoming() {
                    let Ok(mut stream) = stream else { break };
                    accepts_for_thread.fetch_add(1, Ordering::SeqCst);
                    let serving = Arc::clone(&serving_for_thread);
                    std::thread::spawn(move || {
                        // Read the one subscribe line, then stream until EOF.
                        let mut reader = BufReader::new(stream.try_clone().expect("clone"));
                        let mut line = String::new();
                        let _ = reader.read_line(&mut line);
                        if !line.contains("\"v\":1") {
                            return;
                        }
                        serving.fetch_add(1, Ordering::SeqCst);
                        let hello = r#"{"v":1,"session":42,"display":{"w":1080,"h":2400},"format":"png","scale":0.5}"#;
                        let _ = send_message(&mut stream, b'H', hello.as_bytes());
                        let mut index = 0u64;
                        loop {
                            index += 1;
                            let header = format!(
                                "{{\"seq\":{index},\"generation\":1,\"w\":2,\"h\":2,\"format\":\"png\",\"ts_ms\":{}}}\n",
                                live_registry::now_ms()
                            );
                            let mut payload = header.into_bytes();
                            // A tiny valid PNG is not needed: the hub never decodes.
                            payload.extend_from_slice(&[0x89, b'P', b'N', b'G', 0, 1, 2, 3]);
                            if send_message(&mut stream, b'F', &payload).is_err() {
                                break;
                            }
                            let event = format!(
                                "{{\"ts_ms\":{},\"kind\":\"action\",\"cmd\":\"tap\",\"x\":100,\"y\":200,\"status\":\"ok\",\"frame_seq\":{index}}}",
                                live_registry::now_ms()
                            );
                            if send_message(&mut stream, b'E', event.as_bytes()).is_err() {
                                break;
                            }
                            std::thread::sleep(Duration::from_millis(20));
                            if index > 10_000 {
                                break;
                            }
                        }
                        serving.fetch_sub(1, Ordering::SeqCst);
                        // Drop the socket: the hub's next read is EOF.
                    });
                }
            });
            Self {
                socket,
                accepts,
                serving,
                root,
            }
        }
    }

    impl Drop for FakeProducer {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    fn send_message(stream: &mut UnixStream, kind: u8, payload: &[u8]) -> io::Result<()> {
        use std::io::Write;
        let length = (payload.len() + 1) as u32;
        stream.write_all(&length.to_be_bytes())?;
        stream.write_all(&[kind])?;
        stream.write_all(payload)?;
        stream.flush()
    }

    /// Publish one registry entry (live pid = this test process).
    fn publish(dir: &std::path::Path, session: &str, socket: &std::path::Path) -> PathBuf {
        let entry = crate::live_registry::LiveEntry {
            session_id: session.to_string(),
            daemon_pid: std::process::id(),
            launcher_pid: Some(std::process::id()),
            started_ms: live_registry::now_ms(),
            client_name: "hub-test".to_string(),
            client_version: "1".to_string(),
            project_dir: "/home/Qui/Projects/acos".to_string(),
            apk: "fixtures/build/apps/counter.apk".to_string(),
            package: "dev.acos.fixtures.counter".to_string(),
            activity: Some("dev.acos.fixtures.counter.MainActivity".to_string()),
            display: (1080, 2400),
            stream_socket: Some(socket.display().to_string()),
        };
        crate::live_registry::LiveRegistry::at(dir.to_path_buf())
            .write(&entry)
            .expect("publish")
    }

    fn wait_until(mut check: impl FnMut() -> bool, label: &str) {
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(10) {
            if check() {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!("timed out waiting for {label}");
    }

    #[test]
    fn one_upstream_subscription_is_fanned_out_to_every_browser() {
        let producer = FakeProducer::start("fan");
        let registry = scratch("fan-reg");
        let entry_path = publish(&registry, "s_fan", &producer.socket);
        let hub = Hub::new(registry.clone());

        // Five browsers watch: the first one opens the only upstream socket.
        let first = hub.watch("s_fan").expect("watch");
        wait_until(
            || producer.serving.load(Ordering::SeqCst) == 1,
            "the producer to see the first subscriber",
        );
        let watchers: Vec<Watcher> = (0..4)
            .map(|_| hub.watch("s_fan").expect("watch"))
            .collect();
        assert_eq!(producer.accepts.load(Ordering::SeqCst), 1);

        // Every browser sees the same frame and event stream.
        for watcher in &watchers {
            let frame = watcher
                .wait_frame(0, Duration::from_secs(5))
                .expect("a frame reaches every browser");
            assert_eq!(frame.w, 2);
            assert_eq!(frame.format, "png");
            assert!(!frame.bytes.is_empty());
            let batch = watcher.wait_events(0, Duration::from_secs(5));
            assert!(
                batch.events.iter().any(|e| e.kind == "action"),
                "an action event reaches every browser"
            );
        }
        let stats = hub.stats();
        assert_eq!(stats.len(), 1);
        assert_eq!(stats[0].watchers, 5);
        assert_eq!(stats[0].upstream_connects, 1, "exactly one upstream connection");
        assert!(stats[0].connected);

        // A new browser joins late: still no second upstream connection.
        let late = hub.watch("s_fan").expect("watch late");
        std::thread::sleep(Duration::from_millis(150));
        assert_eq!(producer.accepts.load(Ordering::SeqCst), 1);
        assert!(late.frame_cursor() > 0);
        drop(late);

        drop(first);
        drop(watchers);
        let _ = std::fs::remove_file(entry_path);
        let _ = std::fs::remove_dir_all(&registry);
    }

    #[test]
    fn the_upstream_socket_closes_when_the_last_browser_leaves() {
        let producer = FakeProducer::start("cls");
        let registry = scratch("cls-reg");
        let entry_path = publish(&registry, "s_close", &producer.socket);
        let hub = Hub::new(registry.clone());

        let first = hub.watch("s_close").expect("watch");
        let second = hub.watch("s_close").expect("watch");
        wait_until(
            || producer.serving.load(Ordering::SeqCst) == 1,
            "the producer to see the subscriber",
        );
        assert_eq!(hub.stats()[0].watchers, 2);

        // One browser leaves: the upstream stays (fan-out).
        drop(first);
        assert_eq!(producer.serving.load(Ordering::SeqCst), 1);
        assert_eq!(producer.accepts.load(Ordering::SeqCst), 1);

        // The last browser leaves: the upstream thread joins and the socket
        // closes, so the producer's subscriber count is back to zero.
        drop(second);
        wait_until(
            || producer.serving.load(Ordering::SeqCst) == 0,
            "the producer to see the subscriber go away",
        );
        assert_eq!(hub.stats()[0].watchers, 0);
        assert!(!hub.stats()[0].connected);

        // And a later browser reconnects: one new upstream, not a leak.
        let again = hub.watch("s_close").expect("watch again");
        wait_until(
            || producer.accepts.load(Ordering::SeqCst) == 2,
            "a second upstream connection on re-attach",
        );
        drop(again);
        wait_until(
            || producer.serving.load(Ordering::SeqCst) == 0,
            "the reconnect to close again",
        );
        assert_eq!(producer.accepts.load(Ordering::SeqCst), 2);

        let _ = std::fs::remove_file(entry_path);
        let _ = std::fs::remove_dir_all(&registry);
    }

    #[test]
    fn a_browser_that_attaches_while_the_last_one_detaches_still_gets_a_stream() {
        let producer = FakeProducer::start("race");
        let registry = scratch("race-reg");
        let entry_path = publish(&registry, "s_race", &producer.socket);
        let hub = Hub::new(registry.clone());

        let first = hub.watch("s_race").expect("watch");
        wait_until(
            || producer.serving.load(Ordering::SeqCst) == 1,
            "the producer to see the first subscriber",
        );
        // The last detach joins the upstream thread (it notices `stopping` on its
        // next read timeout, up to READ_TIMEOUT later). A browser that attaches
        // during that join used to see `running == true`, start nothing, and then
        // have the detach mark the slot not running: a silently dead stream.
        let detaching = std::thread::spawn(move || drop(first));
        std::thread::sleep(Duration::from_millis(40));
        let second = hub.watch("s_race").expect("watch during the detach");
        detaching.join().expect("detach thread");
        let frame = second
            .wait_frame(0, Duration::from_secs(8))
            .expect("the browser that attached during the detach gets frames");
        assert!(!frame.bytes.is_empty());
        assert_eq!(hub.stats()[0].watchers, 1);
        wait_until(
            || producer.accepts.load(Ordering::SeqCst) == 2,
            "the restarted upstream connection",
        );
        drop(second);
        wait_until(
            || producer.serving.load(Ordering::SeqCst) == 0,
            "the producer to see zero subscribers again",
        );
        let _ = std::fs::remove_file(entry_path);
        let _ = std::fs::remove_dir_all(&registry);
    }

    #[test]
    fn the_hub_reads_the_peer_uid_of_its_upstream_socket() {
        let root = scratch("cred");
        let path = root.join("p.sock");
        let listener = UnixListener::bind(&path).expect("bind");
        let client = UnixStream::connect(&path).expect("connect");
        let (_server, _) = listener.accept().expect("accept");
        let uid = unsafe { libc::getuid() };
        assert_eq!(peer_uid(&client).expect("peer uid"), uid);
        assert!(verify_peer_uid(&client).is_ok());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_listing_hides_and_sweeps_stale_entries() {
        let producer = FakeProducer::start("stal");
        let registry = scratch("stal-reg");
        publish(&registry, "s_live", &producer.socket);

        // A dead daemon pid: the entry must not be listed and must be removed.
        let mut child = std::process::Command::new("/bin/true")
            .spawn()
            .expect("spawn true");
        let dead_pid = child.id();
        let _ = child.wait();
        let dead = json!({
            "v": 1,
            "session_id": "s_dead",
            "daemon_pid": dead_pid,
            "started_ms": live_registry::now_ms(),
            "client": {"name": "ghost", "version": ""},
            "project_dir": "/tmp/nowhere",
            "apk": "x.apk",
            "package": "x",
            "activity": null,
            "display": {"w": 1, "h": 1},
            "stream_socket": null,
        });
        let dead_path = registry.join(format!("s_dead-{dead_pid}.json"));
        std::fs::write(&dead_path, dead.to_string()).expect("write dead");

        let hub = Hub::new(registry.clone());
        let listed = hub.list();
        assert_eq!(listed.len(), 1, "only the live session is listed");
        assert_eq!(listed[0].entry.session_id, "s_live");
        assert!(listed[0].entry.stream_socket.is_some());
        assert!(!dead_path.exists(), "a stale entry is swept from disk");

        // A metadata-only entry (no stream socket) is listed but not watchable.
        let mut metadata_only = listed[0].entry.clone();
        metadata_only.session_id = "s_meta".to_string();
        metadata_only.stream_socket = None;
        let mut doc = metadata_only.to_json();
        doc["v"] = json!(1);
        doc["daemon_pid"] = json!(std::process::id());
        doc["client"] = json!({"name": "hub-test", "version": "1"});
        std::fs::write(
            registry.join(format!("s_meta-{}.json", std::process::id())),
            doc.to_string(),
        )
        .expect("write meta");
        assert_eq!(hub.list().len(), 2);
        assert!(hub.watch("s_meta").is_none(), "no stream socket, no watch");

        let _ = std::fs::remove_dir_all(&registry);
    }
}
