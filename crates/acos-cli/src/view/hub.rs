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
use std::io::{self, BufRead, BufReader, ErrorKind, Read, Write};
use std::net::{Shutdown, TcpStream};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex, Weak};
use std::time::{Duration, Instant};

use acos_services::input_channel::{
    monotonic_nanos, ACTION_CANCEL, ACTION_DOWN, ACTION_MOVE, ACTION_POINTER_DOWN,
    ACTION_POINTER_INDEX_SHIFT, ACTION_POINTER_UP, ACTION_UP,
};
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
    pub app_label: Option<String>,
    pub device_name: Option<String>,
    pub watch_only: bool,
    pub serve_socket: Option<String>,
    pub activity: Option<String>,
    pub display: (u32, u32),
    pub stream_socket: Option<String>,
    pub has_home: Option<bool>,
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
        let app_label = doc
            .get("app_label")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .map(str::to_string);
        let device_name = doc
            .get("device_name")
            .and_then(Value::as_str)
            .and_then(live_registry::sanitize_device_name);
        let watch_only = doc
            .get("watch_only")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let serve_socket = doc
            .get("serve_socket")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .map(str::to_string);
        let has_home = doc.get("has_home").and_then(Value::as_bool);
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
            app_label,
            device_name,
            watch_only,
            serve_socket,
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
            has_home,
        })
    }

    /// The JSON shape the page consumes (the registry entry plus what the hub
    /// knows about the stream). Note `serve_socket` stays internal to the hub.
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
            "app_label": self.app_label,
            "device_name": self.device_name,
            "watch_only": self.watch_only,
            "activity": self.activity,
            "display": {"w": self.display.0, "h": self.display.1},
            "stream": self.stream_socket.is_some(),
            "has_home": self.has_home,
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
    input_slots: BTreeMap<String, Arc<InputSlot>>,
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
                input_slots: BTreeMap::new(),
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
        for (key, input_slot) in &inner.input_slots {
            if let Some(entry) = entries.get(key) {
                *input_slot.meta.lock().expect("input slot meta") = entry.clone();
            }
        }
        // Keep a slot while a browser still watches it (the registry entry may
        // be written/removed around a destroy) or while its upstream is alive.
        inner
            .slots
            .retain(|key, slot| entries.contains_key(key) || slot.is_attached());
        inner
            .input_slots
            .retain(|key, slot| entries.contains_key(key) || slot.is_active());
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
        let (slot, interactive) = {
            let mut inner = self.inner.lock().expect("hub");
            let slot = if let Some(slot) = inner.slots.get(session_id) {
                *slot.meta.lock().expect("slot meta") = entry.clone();
                Arc::clone(slot)
            } else {
                let slot = Arc::new(Slot::new(entry));
                inner
                    .slots
                    .insert(session_id.to_string(), Arc::clone(&slot));
                slot
            };
            let interactive = if let Some(input_slot) = inner.input_slots.get(session_id) {
                *input_slot.stream_slot.lock().expect("input stream slot") =
                    Some(Arc::downgrade(&slot));
                input_slot.is_interactive()
            } else {
                false
            };
            (slot, interactive)
        };
        slot.set_interactive(interactive);
        slot.attach(socket);
        Some(Watcher { slot })
    }

    /// Open an interactive input client on `session_id`, lazily spawning the
    /// session's single ordered [`InputSlot`] worker if one is not running.
    ///
    /// Returns `None` when the session is not live in the registry.
    #[must_use]
    pub fn open_input(&self, session_id: &str) -> Option<InputClient> {
        let entry = self.entry(session_id)?;
        let slot = {
            let mut inner = self.inner.lock().expect("hub");
            let stream_slot = inner.slots.get(session_id).map(Arc::downgrade);
            let slot = if let Some(slot) = inner.input_slots.get(session_id) {
                *slot.meta.lock().expect("input slot meta") = entry.clone();
                Arc::clone(slot)
            } else {
                let slot = Arc::new(InputSlot::new(entry));
                inner
                    .input_slots
                    .insert(session_id.to_string(), Arc::clone(&slot));
                slot
            };
            if let Some(weak) = stream_slot {
                *slot.stream_slot.lock().expect("input stream slot") = Some(weak);
            }
            slot
        };
        Some(slot.attach_client())
    }

    /// Per-session counters (the hub's test seam, and what a test asserts on
    /// to prove one upstream per session and zero after the last browser).
    #[must_use]
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn stats(&self) -> Vec<SlotStats> {
        let inner = self.inner.lock().expect("hub");
        let mut out: Vec<SlotStats> = inner.slots.values().map(|slot| slot.stats()).collect();
        out.sort_by(|a, b| a.session_id.cmp(&b.session_id));
        out
    }

    /// Whether an input worker is currently active for `session_id` (test seam).
    #[must_use]
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn input_active(&self, session_id: &str) -> bool {
        let inner = self.inner.lock().expect("hub");
        inner
            .input_slots
            .get(session_id)
            .is_some_and(|slot| slot.is_active())
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
        self.slot.state.lock().expect("slot").hello.clone()
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
    interactive: bool,
    mode_epoch: u64,
    active_stream: Option<UnixStream>,
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
                interactive: false,
                mode_epoch: 0,
                active_stream: None,
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

    fn set_interactive(&self, interactive: bool) {
        let mut state = self.state.lock().expect("slot");
        if state.interactive != interactive {
            state.interactive = interactive;
            state.mode_epoch = state.mode_epoch.wrapping_add(1);
            if let Some(stream) = state.active_stream.as_ref() {
                let _ = stream.shutdown(Shutdown::Both);
            }
            self.cv.notify_all();
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
            if let Some(stream) = state.active_stream.as_ref() {
                let _ = stream.shutdown(Shutdown::Both);
            }
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
        state.frame = None;
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
                    let (interactive, start_epoch) = {
                        let mut state = self.state.lock().expect("slot");
                        state.active_stream = stream.try_clone().ok();
                        state.connected = true;
                        state.error = None;
                        state.upstream_connects += 1;
                        (state.interactive, state.mode_epoch)
                    };
                    self.cv.notify_all();
                    let outcome = self.pump(&mut stream, interactive);
                    let reconfigured = {
                        let mut state = self.state.lock().expect("slot");
                        state.active_stream = None;
                        let reconfigured = state.mode_epoch != start_epoch;
                        if !reconfigured || state.stopping {
                            state.connected = false;
                            if !state.stopping {
                                if let Err(error) = &outcome {
                                    state.error = Some(error.to_string());
                                    state.upstream_errors += 1;
                                }
                            }
                        }
                        reconfigured
                    };
                    self.cv.notify_all();
                    drop(stream); // closing here is what returns the producer to 0
                    backoff = Duration::from_millis(100);
                    if reconfigured && !self.stopping() {
                        continue;
                    }
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
    fn pump(&self, stream: &mut UnixStream, interactive: bool) -> io::Result<()> {
        // One encoder per session: the first subscriber fixes format/scale/fps.
        // Watch-only stays 0.5 PNG @ 15 fps; interactive upgrades to 1.0 PNG @ 30 fps
        // (fast adaptive PNG is ~10x faster to encode than scalar JPEG on this host
        // and smaller on flat UI screens).
        let subscribe = if interactive {
            json!({
                "v": 1,
                "subscribe": {
                    "frames": true,
                    "events": true,
                    "max_fps": 30,
                    "scale": 1.0,
                    "format": "png",
                }
            })
        } else {
            json!({
                "v": 1,
                "subscribe": {
                    "frames": true,
                    "events": true,
                    "max_fps": 15,
                    "scale": 0.5,
                    "format": "png",
                }
            })
        };
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

// ------------------------------------------------------------------------- //
// Interactive WebSocket input worker (HUMANVIEW1 CP2)
// ------------------------------------------------------------------------- //

/// An active browser `/api/input/<session_id>` client handle.
///
/// Dropping this handle notifies the per-session worker to emit `ACTION_CANCEL`
/// for any pointers this client left down (and `up` for any held keys) and lets
/// the worker exit once the last input client disconnects.
pub struct InputClient {
    client_id: u64,
    slot: Arc<InputSlot>,
}

impl InputClient {
    /// Current session metadata snapshot.
    #[must_use]
    pub fn entry(&self) -> Entry {
        self.slot.meta.lock().expect("input slot meta").clone()
    }

    /// Parse one raw WebSocket JSON text message and enqueue it onto the
    /// session's ordered worker. Invalid JSON/arguments reply immediately on
    /// `reply_to` without hitting the worker.
    pub fn enqueue(&self, raw_json: &str, reply_to: Arc<Mutex<TcpStream>>) {
        match parse_input_command(raw_json) {
            Ok(cmd) => {
                let mut state = self.slot.state.lock().expect("input slot");
                state.queue.push_back(WorkerTask::Command {
                    client_id: self.client_id,
                    cmd,
                    reply_to,
                });
                self.slot.cv.notify_one();
            }
            Err(err_msg) => {
                let reply = json!({
                    "ok": false,
                    "code": "INVALID_ARGUMENT",
                    "error": err_msg,
                })
                .to_string();
                let _ = super::ws::send_text_locked(&reply_to, &reply);
            }
        }
    }
}

impl Drop for InputClient {
    fn drop(&mut self) {
        let should_downgrade = {
            let mut state = self.slot.state.lock().expect("input slot");
            state.clients = state.clients.saturating_sub(1);
            state.queue.push_back(WorkerTask::ClientDisconnected {
                client_id: self.client_id,
            });
            self.slot.cv.notify_one();
            state.clients == 0
        };
        if should_downgrade {
            if let Some(stream_slot) = self
                .slot
                .stream_slot
                .lock()
                .expect("input stream slot")
                .as_ref()
                .and_then(Weak::upgrade)
            {
                stream_slot.set_interactive(false);
            }
        }
    }
}

#[derive(Debug, Clone)]
enum TouchPhase {
    Down,
    Move,
    Up,
    Cancel,
}

#[derive(Debug, Clone)]
enum KeyPhase {
    Down,
    Up,
    Press,
}

#[derive(Debug, Clone)]
enum InputCommand {
    Touch {
        phase: TouchPhase,
        id: i32,
        x: Option<f32>,
        y: Option<f32>,
    },
    Scroll {
        x: f32,
        y: f32,
        dx: f32,
        dy: f32,
    },
    Key {
        code: i32,
        phase: KeyPhase,
    },
    Text {
        s: String,
    },
    Rotate {
        rotation: u8,
    },
}

enum WorkerTask {
    Command {
        client_id: u64,
        cmd: InputCommand,
        reply_to: Arc<Mutex<TcpStream>>,
    },
    ClientDisconnected {
        client_id: u64,
    },
}

struct InputSlot {
    meta: Mutex<Entry>,
    stream_slot: Mutex<Option<Weak<Slot>>>,
    state: Mutex<InputSlotState>,
    cv: Condvar,
}

struct InputSlotState {
    clients: usize,
    next_client_id: u64,
    running: bool,
    queue: VecDeque<WorkerTask>,
}

impl InputSlot {
    fn new(entry: Entry) -> Self {
        Self {
            meta: Mutex::new(entry),
            stream_slot: Mutex::new(None),
            state: Mutex::new(InputSlotState {
                clients: 0,
                next_client_id: 1,
                running: false,
                queue: VecDeque::new(),
            }),
            cv: Condvar::new(),
        }
    }

    fn is_active(&self) -> bool {
        let state = self.state.lock().expect("input slot");
        state.clients > 0 || state.running
    }

    fn is_interactive(&self) -> bool {
        if self.meta.lock().expect("input slot meta").watch_only {
            return false;
        }
        let state = self.state.lock().expect("input slot");
        state.clients > 0
    }

    fn attach_client(self: &Arc<Self>) -> InputClient {
        let client_id = {
            let mut state = self.state.lock().expect("input slot");
            let client_id = state.next_client_id;
            state.next_client_id += 1;
            state.clients += 1;
            if !state.running {
                state.running = true;
                let slot = Arc::clone(self);
                std::thread::Builder::new()
                    .name("acos-view-input".to_string())
                    .spawn(move || InputWorker::new(slot).run())
                    .expect("spawn input worker");
            }
            client_id
        };
        if !self.meta.lock().expect("input slot meta").watch_only {
            if let Some(stream_slot) = self
                .stream_slot
                .lock()
                .expect("input stream slot")
                .as_ref()
                .and_then(Weak::upgrade)
            {
                stream_slot.set_interactive(true);
            }
        }
        InputClient {
            client_id,
            slot: Arc::clone(self),
        }
    }
}

struct DaemonConn {
    socket_path: String,
    writer: UnixStream,
    reader: BufReader<UnixStream>,
    next_id: u64,
}

impl DaemonConn {
    fn connect(socket_path: &str) -> Result<Self, (String, String)> {
        let stream = UnixStream::connect(socket_path).map_err(|e| {
            (
                "INPUT_UNAVAILABLE".to_string(),
                format!("cannot connect to session daemon {socket_path}: {e}"),
            )
        })?;
        verify_peer_uid(&stream).map_err(|e| ("PERMISSION_DENIED".to_string(), e.to_string()))?;
        let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
        let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));
        let writer = stream
            .try_clone()
            .map_err(|e| ("INPUT_UNAVAILABLE".to_string(), e.to_string()))?;
        Ok(Self {
            socket_path: socket_path.to_string(),
            writer,
            reader: BufReader::new(stream),
            next_id: 1,
        })
    }

    fn call(&mut self, tool: &str, args: Value) -> Result<Value, (String, String)> {
        let id = self.next_id;
        self.next_id += 1;
        let line = json!({
            "id": id,
            "tool": tool,
            "args": args,
        })
        .to_string()
            + "\n";
        self.writer
            .write_all(line.as_bytes())
            .and_then(|()| self.writer.flush())
            .map_err(|e| ("IO_ERROR".to_string(), e.to_string()))?;
        let mut resp_line = String::new();
        let n = self
            .reader
            .read_line(&mut resp_line)
            .map_err(|e| ("IO_ERROR".to_string(), e.to_string()))?;
        if n == 0 {
            return Err(("EOF".to_string(), "daemon closed socket".to_string()));
        }
        let resp: Value = serde_json::from_str(&resp_line)
            .map_err(|e| ("BAD_REPLY".to_string(), format!("invalid daemon JSON: {e}")))?;
        let envelope = resp.get("envelope").ok_or_else(|| {
            (
                "BAD_REPLY".to_string(),
                "daemon response missing envelope".to_string(),
            )
        })?;
        if envelope.get("ok").and_then(Value::as_bool) == Some(true) {
            Ok(envelope.get("result").cloned().unwrap_or(Value::Null))
        } else {
            let code = envelope
                .pointer("/error/code")
                .and_then(Value::as_str)
                .unwrap_or("INPUT_FAILED")
                .to_string();
            let message = envelope
                .pointer("/error/message")
                .and_then(Value::as_str)
                .unwrap_or("panel input rejected")
                .to_string();
            Err((code, message))
        }
    }
}

struct ActivePointer {
    client_id: u64,
    pointer_id: i32,
    x: f32,
    y: f32,
}

struct InputWorker {
    slot: Arc<InputSlot>,
    conn: Option<DaemonConn>,
    pointers: Vec<ActivePointer>,
    down_time_ns: Option<i64>,
    last_event_time_ns: i64,
    held_keys: Vec<(u64, i32)>,
}

impl InputWorker {
    fn new(slot: Arc<InputSlot>) -> Self {
        Self {
            slot,
            conn: None,
            pointers: Vec::new(),
            down_time_ns: None,
            last_event_time_ns: 0,
            held_keys: Vec::new(),
        }
    }

    fn next_ts_ns(&mut self) -> i64 {
        let now = monotonic_nanos();
        let ts = now.max(self.last_event_time_ns + 1);
        self.last_event_time_ns = ts;
        ts
    }

    fn run(mut self) {
        loop {
            let task = {
                let mut state = self.slot.state.lock().expect("input slot");
                loop {
                    if let Some(task) = state.queue.pop_front() {
                        break task;
                    }
                    if state.clients == 0 {
                        state.running = false;
                        return;
                    }
                    state = self
                        .slot
                        .cv
                        .wait(state)
                        .unwrap_or_else(|poison| poison.into_inner());
                }
            };
            match task {
                WorkerTask::Command {
                    client_id,
                    cmd,
                    reply_to,
                } => {
                    let reply = self.handle_command(client_id, cmd);
                    let _ = super::ws::send_text_locked(&reply_to, &reply.to_string());
                }
                WorkerTask::ClientDisconnected { client_id } => {
                    self.cancel_client(client_id);
                }
            }
        }
    }

    fn entry(&self) -> Entry {
        self.slot.meta.lock().expect("input slot meta").clone()
    }

    fn notify_watch_only_ignored(&self, entry: &Entry) {
        let Some(stream_sock) = entry.stream_socket.as_deref() else {
            return;
        };
        if let Ok(mut stream) = UnixStream::connect(stream_sock) {
            if verify_peer_uid(&stream).is_ok() {
                let _ = stream.set_write_timeout(Some(Duration::from_millis(250)));
                let _ = stream.write_all(b"{\"v\":1,\"panel_input_ignored\":true}\n");
                let _ = stream.flush();
            }
        }
    }

    fn call_daemon(&mut self, tool: &str, mut args: Value) -> Result<Value, (String, String)> {
        let entry = self.entry();
        let Some(sock) = entry.serve_socket.as_deref() else {
            return Err((
                "INPUT_UNAVAILABLE".to_string(),
                "session has no input socket".to_string(),
            ));
        };
        if let Some(obj) = args.as_object_mut() {
            obj.insert(
                "base".to_string(),
                json!({ "session_id": entry.session_id }),
            );
        }
        let need_new = self
            .conn
            .as_ref()
            .is_none_or(|c| c.socket_path != sock);
        if need_new {
            self.conn = Some(DaemonConn::connect(sock)?);
        }
        let first_err = match self.conn.as_mut().expect("conn").call(tool, args.clone()) {
            Ok(res) => return Ok(res),
            Err(err) => err,
        };
        if first_err.0 == "IO_ERROR" || first_err.0 == "EOF" {
            self.conn = None;
            let mut fresh = DaemonConn::connect(sock)?;
            let res = fresh.call(tool, args)?;
            self.conn = Some(fresh);
            return Ok(res);
        }
        Err(first_err)
    }

    fn pointer_wire_list(&self) -> Vec<Value> {
        self.pointers
            .iter()
            .map(|p| json!([p.pointer_id, p.x, p.y]))
            .collect()
    }

    fn send_touch_frame(
        &mut self,
        action: i32,
        down_time: i64,
        event_time: i64,
    ) -> Result<Value, (String, String)> {
        let pointers = self.pointer_wire_list();
        self.call_daemon(
            "panel_touch",
            json!({
                "action": action,
                "down_time": down_time,
                "event_time": event_time,
                "pointers": pointers,
            }),
        )
    }

    fn handle_command(&mut self, client_id: u64, cmd: InputCommand) -> Value {
        let entry = self.entry();
        if entry.watch_only {
            self.conn = None;
            let should_emit = matches!(
                cmd,
                InputCommand::Touch {
                    phase: TouchPhase::Down,
                    ..
                } | InputCommand::Scroll { .. }
                    | InputCommand::Key {
                        phase: KeyPhase::Down | KeyPhase::Press,
                        ..
                    }
                    | InputCommand::Text { .. }
                    | InputCommand::Rotate { .. }
            );
            if should_emit {
                self.notify_watch_only_ignored(&entry);
            }
            return json!({
                "ok": false,
                "code": "VERIFY_WATCH_ONLY",
                "error": "watch-only: verify run",
            });
        }

        match cmd {
            InputCommand::Touch { phase, id, x, y } => {
                self.handle_touch(client_id, phase, id, x, y)
            }
            InputCommand::Scroll { x, y, dx, dy } => self.handle_scroll(&entry, x, y, dx, dy),
            InputCommand::Key { code, phase } => self.handle_key(client_id, code, phase),
            InputCommand::Text { s } => self.handle_text(&s),
            InputCommand::Rotate { rotation } => self.handle_rotate(rotation),
        }
    }

    fn handle_touch(
        &mut self,
        client_id: u64,
        phase: TouchPhase,
        pointer_id: i32,
        x: Option<f32>,
        y: Option<f32>,
    ) -> Value {
        match phase {
            TouchPhase::Down => {
                let (Some(x), Some(y)) = (x, y) else {
                    return err_json("INVALID_ARGUMENT", "touch down requires x and y");
                };
                if let Some(pos) = self
                    .pointers
                    .iter()
                    .position(|p| p.client_id == client_id && p.pointer_id == pointer_id)
                {
                    self.pointers[pos].x = x;
                    self.pointers[pos].y = y;
                    let ts = self.next_ts_ns();
                    let down_time = self.down_time_ns.unwrap_or(ts);
                    return match self.send_touch_frame(ACTION_MOVE, down_time, ts) {
                        Ok(_) => json!({ "ok": true, "t": "touch", "phase": "move" }),
                        Err((c, m)) => err_json(&c, &m),
                    };
                }
                self.pointers.push(ActivePointer {
                    client_id,
                    pointer_id,
                    x,
                    y,
                });
                let ts = self.next_ts_ns();
                let is_first = self.pointers.len() == 1;
                if is_first {
                    self.down_time_ns = Some(ts);
                }
                let down_time = self.down_time_ns.unwrap_or(ts);
                let action = if is_first {
                    ACTION_DOWN
                } else {
                    let idx = (self.pointers.len() - 1) as i32;
                    ACTION_POINTER_DOWN | (idx << ACTION_POINTER_INDEX_SHIFT)
                };
                match self.send_touch_frame(action, down_time, ts) {
                    Ok(_) => json!({ "ok": true, "t": "touch", "phase": "down" }),
                    Err((c, m)) => {
                        self.pointers.pop();
                        if self.pointers.is_empty() {
                            self.down_time_ns = None;
                        }
                        err_json(&c, &m)
                    }
                }
            }
            TouchPhase::Move => {
                let (Some(x), Some(y)) = (x, y) else {
                    return err_json("INVALID_ARGUMENT", "touch move requires x and y");
                };
                let Some(pos) = self
                    .pointers
                    .iter()
                    .position(|p| p.client_id == client_id && p.pointer_id == pointer_id)
                else {
                    return json!({ "ok": true, "t": "touch", "ignored": "pointer_not_down" });
                };
                self.pointers[pos].x = x;
                self.pointers[pos].y = y;
                let ts = self.next_ts_ns();
                let down_time = self.down_time_ns.unwrap_or(ts);
                match self.send_touch_frame(ACTION_MOVE, down_time, ts) {
                    Ok(_) => json!({ "ok": true, "t": "touch", "phase": "move" }),
                    Err((c, m)) => err_json(&c, &m),
                }
            }
            TouchPhase::Up => {
                let (Some(x), Some(y)) = (x, y) else {
                    return err_json("INVALID_ARGUMENT", "touch up requires x and y");
                };
                let Some(pos) = self
                    .pointers
                    .iter()
                    .position(|p| p.client_id == client_id && p.pointer_id == pointer_id)
                else {
                    return json!({ "ok": true, "t": "touch", "ignored": "pointer_not_down" });
                };
                self.pointers[pos].x = x;
                self.pointers[pos].y = y;
                let ts = self.next_ts_ns();
                let down_time = self.down_time_ns.unwrap_or(ts);
                let action = if self.pointers.len() == 1 {
                    ACTION_UP
                } else {
                    ACTION_POINTER_UP | ((pos as i32) << ACTION_POINTER_INDEX_SHIFT)
                };
                let res = self.send_touch_frame(action, down_time, ts);
                self.pointers.remove(pos);
                if self.pointers.is_empty() {
                    self.down_time_ns = None;
                }
                match res {
                    Ok(_) => json!({ "ok": true, "t": "touch", "phase": "up" }),
                    Err((c, m)) => err_json(&c, &m),
                }
            }
            TouchPhase::Cancel => {
                if let (Some(x), Some(y)) = (x, y) {
                    if let Some(pos) = self
                        .pointers
                        .iter()
                        .position(|p| p.client_id == client_id && p.pointer_id == pointer_id)
                    {
                        self.pointers[pos].x = x;
                        self.pointers[pos].y = y;
                    }
                }
                self.cancel_client_pointers(client_id)
            }
        }
    }

    fn cancel_client_pointers(&mut self, client_id: u64) -> Value {
        let has_client_ptr = self.pointers.iter().any(|p| p.client_id == client_id);
        if !has_client_ptr {
            return json!({ "ok": true, "t": "touch", "phase": "cancel" });
        }
        let only_this_client = self.pointers.iter().all(|p| p.client_id == client_id);
        if only_this_client {
            let ts = self.next_ts_ns();
            let down_time = self.down_time_ns.unwrap_or(ts);
            let res = self.send_touch_frame(ACTION_CANCEL, down_time, ts);
            self.pointers.clear();
            self.down_time_ns = None;
            return match res {
                Ok(_) => json!({ "ok": true, "t": "touch", "phase": "cancel" }),
                Err((c, m)) => err_json(&c, &m),
            };
        }
        let mut last_res = Ok(Value::Null);
        while let Some(pos) = self.pointers.iter().position(|p| p.client_id == client_id) {
            let ts = self.next_ts_ns();
            let down_time = self.down_time_ns.unwrap_or(ts);
            let action = if self.pointers.len() == 1 {
                ACTION_CANCEL
            } else {
                ACTION_POINTER_UP | ((pos as i32) << ACTION_POINTER_INDEX_SHIFT)
            };
            last_res = self.send_touch_frame(action, down_time, ts);
            self.pointers.remove(pos);
        }
        if self.pointers.is_empty() {
            self.down_time_ns = None;
        }
        match last_res {
            Ok(_) => json!({ "ok": true, "t": "touch", "phase": "cancel" }),
            Err((c, m)) => err_json(&c, &m),
        }
    }

    fn handle_scroll(&mut self, entry: &Entry, x: f32, y: f32, dx: f32, dy: f32) -> Value {
        if !self.pointers.is_empty() {
            return err_json("INVALID_STATE", "cannot scroll while a touch pointer is active");
        }
        if dx == 0.0 && dy == 0.0 {
            return json!({ "ok": true, "t": "scroll", "ignored": "zero_delta" });
        }
        let w = if entry.display.0 > 0 {
            entry.display.0 as f32
        } else {
            1080.0
        };
        let h = if entry.display.1 > 0 {
            entry.display.1 as f32
        } else {
            2400.0
        };
        let sx = x.clamp(4.0, (w - 4.0).max(4.0));
        let sy = y.clamp(4.0, (h - 4.0).max(4.0));
        let step_x = if dx.abs() > 0.0 {
            -dx.signum() * dx.abs().clamp(48.0, 600.0)
        } else {
            0.0
        };
        let step_y = if dy.abs() > 0.0 {
            -dy.signum() * dy.abs().clamp(48.0, 900.0)
        } else {
            0.0
        };
        let ex = (sx + step_x).clamp(4.0, (w - 4.0).max(4.0));
        let ey = (sy + step_y).clamp(4.0, (h - 4.0).max(4.0));

        const MOVE_STEPS: usize = 6;
        const STEP_NS: i64 = 8_000_000;
        let span_ns = (MOVE_STEPS as i64 + 1) * STEP_NS;
        let end_now = monotonic_nanos();
        let down_time = (end_now - span_ns).max(self.last_event_time_ns + 1);

        let frames = (0..=MOVE_STEPS + 1).map(|i| {
            let t = i as f32 / (MOVE_STEPS + 1) as f32;
            let px = sx + (ex - sx) * t;
            let py = sy + (ey - sy) * t;
            let action = if i == 0 {
                ACTION_DOWN
            } else if i == MOVE_STEPS + 1 {
                ACTION_UP
            } else {
                ACTION_MOVE
            };
            let event_time = down_time + (i as i64) * STEP_NS;
            (action, px, py, event_time)
        });

        for (action, px, py, event_time) in frames {
            self.last_event_time_ns = event_time;
            if let Err((c, m)) = self.call_daemon(
                "panel_touch",
                json!({
                    "action": action,
                    "down_time": down_time,
                    "event_time": event_time,
                    "pointers": [[0, px, py]],
                }),
            ) {
                return err_json(&c, &m);
            }
        }
        json!({ "ok": true, "t": "scroll" })
    }

    fn handle_key(&mut self, client_id: u64, code: i32, phase: KeyPhase) -> Value {
        let event_type = match phase {
            KeyPhase::Down => {
                if !self.held_keys.contains(&(client_id, code)) {
                    self.held_keys.push((client_id, code));
                }
                "down"
            }
            KeyPhase::Up => {
                self.held_keys.retain(|&pair| pair != (client_id, code));
                "up"
            }
            KeyPhase::Press => {
                self.held_keys.retain(|&pair| pair != (client_id, code));
                "press"
            }
        };
        match self.call_daemon(
            "panel_key",
            json!({
                "code": code,
                "event_type": event_type,
            }),
        ) {
            Ok(_) => json!({ "ok": true, "t": "key", "code": code, "phase": event_type }),
            Err((c, m)) => err_json(&c, &m),
        }
    }

    fn handle_text(&mut self, text: &str) -> Value {
        if text.is_empty() {
            return json!({ "ok": true, "t": "text" });
        }
        match self.call_daemon("panel_text", json!({ "text": text })) {
            Ok(_) => json!({ "ok": true, "t": "text" }),
            Err((c, m)) => err_json(&c, &m),
        }
    }

    fn handle_rotate(&mut self, rotation: u8) -> Value {
        match self.call_daemon("panel_rotate", json!({ "rotation": rotation })) {
            Ok(res) => {
                let width = res.get("width").and_then(Value::as_u64).unwrap_or(0) as u32;
                let height = res.get("height").and_then(Value::as_u64).unwrap_or(0) as u32;
                let applied = res
                    .get("rotation")
                    .and_then(Value::as_u64)
                    .unwrap_or(u64::from(rotation));
                if width > 0 && height > 0 {
                    self.slot.meta.lock().expect("input slot meta").display = (width, height);
                    if let Some(stream_slot) = self
                        .slot
                        .stream_slot
                        .lock()
                        .expect("input stream slot")
                        .as_ref()
                        .and_then(Weak::upgrade)
                    {
                        stream_slot.meta.lock().expect("slot meta").display = (width, height);
                    }
                }
                json!({
                    "ok": true,
                    "t": "rotate",
                    "width": width,
                    "height": height,
                    "rotation": applied,
                })
            }
            Err((c, m)) => err_json(&c, &m),
        }
    }

    fn cancel_client(&mut self, client_id: u64) {
        let entry = self.entry();
        if entry.watch_only {
            self.pointers.retain(|p| p.client_id != client_id);
            if self.pointers.is_empty() {
                self.down_time_ns = None;
            }
            self.held_keys.retain(|&(cid, _)| cid != client_id);
            return;
        }
        let _ = self.cancel_client_pointers(client_id);
        let keys_to_release: Vec<i32> = self
            .held_keys
            .iter()
            .filter_map(|&(cid, code)| (cid == client_id).then_some(code))
            .collect();
        self.held_keys.retain(|&(cid, _)| cid != client_id);
        for code in keys_to_release {
            let _ = self.call_daemon(
                "panel_key",
                json!({
                    "code": code,
                    "event_type": "up",
                }),
            );
        }
    }
}

fn err_json(code: &str, error: &str) -> Value {
    json!({
        "ok": false,
        "code": code,
        "error": error,
    })
}

fn parse_input_command(raw_json: &str) -> Result<InputCommand, String> {
    let val: Value =
        serde_json::from_str(raw_json).map_err(|e| format!("invalid JSON: {e}"))?;
    let kind = val
        .get("t")
        .and_then(Value::as_str)
        .ok_or_else(|| "missing string field `t`".to_string())?;
    match kind {
        "touch" => {
            let phase = match val.get("phase").and_then(Value::as_str) {
                Some("down") => TouchPhase::Down,
                Some("move") => TouchPhase::Move,
                Some("up") => TouchPhase::Up,
                Some("cancel") => TouchPhase::Cancel,
                other => {
                    return Err(format!(
                        "touch requires phase down|move|up|cancel (got {other:?})"
                    ))
                }
            };
            let id = match val.get("id") {
                None | Some(Value::Null) => 0,
                Some(v) => v
                    .as_i64()
                    .and_then(|n| i32::try_from(n).ok())
                    .ok_or_else(|| "touch `id` must be an integer".to_string())?,
            };
            let x = finite_opt_f32(val.get("x"), "x")?;
            let y = finite_opt_f32(val.get("y"), "y")?;
            if !matches!(phase, TouchPhase::Cancel) && (x.is_none() || y.is_none()) {
                return Err("touch down/move/up requires numeric `x` and `y`".to_string());
            }
            Ok(InputCommand::Touch { phase, id, x, y })
        }
        "scroll" => {
            let x = finite_opt_f32(val.get("x"), "x")?
                .ok_or_else(|| "scroll requires numeric `x`".to_string())?;
            let y = finite_opt_f32(val.get("y"), "y")?
                .ok_or_else(|| "scroll requires numeric `y`".to_string())?;
            let dx = finite_opt_f32(val.get("dx"), "dx")?;
            let dy = finite_opt_f32(val.get("dy"), "dy")?;
            if dx.is_none() && dy.is_none() {
                return Err("scroll requires `dy` or `dx`".to_string());
            }
            Ok(InputCommand::Scroll {
                x,
                y,
                dx: dx.unwrap_or(0.0),
                dy: dy.unwrap_or(0.0),
            })
        }
        "key" => {
            let code = match val.get("code") {
                Some(Value::String(name)) => super::keycodes::parse_keycode(name)
                    .ok_or_else(|| format!("unknown key code {name:?}"))?,
                Some(Value::Number(n)) => n
                    .as_i64()
                    .and_then(|v| i32::try_from(v).ok())
                    .filter(|&v| v >= 0)
                    .ok_or_else(|| "numeric key code out of range".to_string())?,
                _ => return Err("key requires string or integer `code`".to_string()),
            };
            let phase = match val.get("phase").and_then(Value::as_str) {
                None | Some("press") => KeyPhase::Press,
                Some("down") => KeyPhase::Down,
                Some("up") => KeyPhase::Up,
                Some(other) => {
                    return Err(format!(
                        "key `phase` must be down|up|press (got {other:?})"
                    ))
                }
            };
            Ok(InputCommand::Key { code, phase })
        }
        "text" => {
            let s = val
                .get("s")
                .or_else(|| val.get("text"))
                .and_then(Value::as_str)
                .ok_or_else(|| "text requires string `s`".to_string())?
                .to_string();
            Ok(InputCommand::Text { s })
        }
        "rotate" => {
            let rot = val
                .get("rotation")
                .and_then(Value::as_u64)
                .and_then(|v| u8::try_from(v).ok())
                .filter(|&v| v <= 3)
                .ok_or_else(|| "rotate requires integer `rotation` in 0..=3".to_string())?;
            Ok(InputCommand::Rotate { rotation: rot })
        }
        other => Err(format!("unknown input message type {other:?}")),
    }
}

fn finite_opt_f32(val: Option<&Value>, field: &str) -> Result<Option<f32>, String> {
    let Some(v) = val else {
        return Ok(None);
    };
    if v.is_null() {
        return Ok(None);
    }
    let f = v
        .as_f64()
        .ok_or_else(|| format!("field `{field}` must be a number"))?;
    if !f.is_finite() {
        return Err(format!("field `{field}` must be finite"));
    }
    Ok(Some(f as f32))
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
        subscribes: Arc<Mutex<Vec<Value>>>,
        root: PathBuf,
    }

    fn scratch(tag: &str) -> PathBuf {
        // Short paths: a Unix socket path must fit in `sun_path` (< 108 bytes),
        // and the checkout path is not something this test controls (agent
        // worktrees are long). `test_scratch_root` keeps it short - including
        // when `ACOS_VIEW_TEST_DIR` itself is too long for a socket.
        crate::live_registry::short_scratch(crate::live_registry::test_scratch_root(), tag)
    }

    impl FakeProducer {
        fn start(tag: &str) -> Self {
            let root = scratch(tag);
            let socket = root.join("view.sock");
            let listener = UnixListener::bind(&socket).expect("bind fake producer");
            let accepts = Arc::new(AtomicUsize::new(0));
            let serving = Arc::new(AtomicUsize::new(0));
            let subscribes = Arc::new(Mutex::new(Vec::new()));
            let accepts_for_thread = Arc::clone(&accepts);
            let serving_for_thread = Arc::clone(&serving);
            let subscribes_for_thread = Arc::clone(&subscribes);
            std::thread::spawn(move || {
                for stream in listener.incoming() {
                    let Ok(mut stream) = stream else { break };
                    accepts_for_thread.fetch_add(1, Ordering::SeqCst);
                    let serving = Arc::clone(&serving_for_thread);
                    let subscribes = Arc::clone(&subscribes_for_thread);
                    std::thread::spawn(move || {
                        // Read the one subscribe line, then stream until EOF.
                        let mut reader = BufReader::new(stream.try_clone().expect("clone"));
                        let mut line = String::new();
                        let _ = reader.read_line(&mut line);
                        if !line.contains("\"v\":1") {
                            return;
                        }
                        if let Ok(req) = serde_json::from_str::<Value>(&line) {
                            subscribes.lock().unwrap().push(req);
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
                subscribes,
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
            app_label: Some("Counter".to_string()),
            device_name: None,
            watch_only: false,
            serve_socket: None,
            activity: Some("dev.acos.fixtures.counter.MainActivity".to_string()),
            display: (1080, 2400),
            stream_socket: Some(socket.display().to_string()),
            has_home: Some(false),
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
        let watchers: Vec<Watcher> = (0..4).map(|_| hub.watch("s_fan").expect("watch")).collect();
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
        assert_eq!(
            stats[0].upstream_connects, 1,
            "exactly one upstream connection"
        );
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

    struct FakeDaemon {
        socket: PathBuf,
        calls: Arc<Mutex<Vec<(String, Value)>>>,
        root: PathBuf,
    }

    impl FakeDaemon {
        fn start(tag: &str) -> Self {
            let root = scratch(tag);
            let socket = root.join("serve.sock");
            let listener = UnixListener::bind(&socket).expect("bind fake daemon");
            let calls = Arc::new(Mutex::new(Vec::new()));
            let calls_thread = Arc::clone(&calls);
            std::thread::spawn(move || {
                for stream in listener.incoming() {
                    let Ok(mut writer) = stream else { break };
                    let reader = BufReader::new(writer.try_clone().expect("clone"));
                    let calls = Arc::clone(&calls_thread);
                    std::thread::spawn(move || {
                        for line in reader.lines() {
                            let Ok(line) = line else { break };
                            if line.trim().is_empty() {
                                continue;
                            }
                            let req: Value = serde_json::from_str(&line).expect("req json");
                            let id = req["id"].clone();
                            let tool = req["tool"].as_str().unwrap_or("").to_string();
                            let args = req["args"].clone();
                            calls.lock().unwrap().push((tool.clone(), args));
                            let result = if tool == "panel_rotate" {
                                json!({
                                    "ok": true,
                                    "session_id": "s_inp",
                                    "width": 2400,
                                    "height": 1080,
                                    "rotation": 1,
                                })
                            } else {
                                json!({ "ok": true, "session_id": "s_inp" })
                            };
                            let resp = json!({
                                "id": id,
                                "envelope": {
                                    "ok": true,
                                    "result": result,
                                },
                            });
                            let _ = std::io::Write::write_all(
                                &mut writer,
                                (resp.to_string() + "\n").as_bytes(),
                            );
                            let _ = std::io::Write::flush(&mut writer);
                        }
                    });
                }
            });
            Self {
                socket,
                calls,
                root,
            }
        }
    }

    impl Drop for FakeDaemon {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    fn ws_sink_pair() -> (TcpStream, Arc<Mutex<TcpStream>>) {
        let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
        let addr = listener.local_addr().unwrap();
        let client = TcpStream::connect(addr).unwrap();
        let (server, _) = listener.accept().unwrap();
        let _ = client.set_read_timeout(Some(Duration::from_secs(3)));
        (client, Arc::new(Mutex::new(server)))
    }

    fn read_server_ws_text(client: &mut TcpStream) -> Value {
        let mut head = [0u8; 2];
        client.read_exact(&mut head).unwrap();
        assert_eq!(head[0] & 0x0f, super::super::ws::OPCODE_TEXT);
        let len7 = head[1] & 0x7f;
        let len = match len7 {
            0..=125 => usize::from(len7),
            126 => {
                let mut ext = [0u8; 2];
                client.read_exact(&mut ext).unwrap();
                usize::from(u16::from_be_bytes(ext))
            }
            _ => panic!("unexpected long test frame"),
        };
        let mut buf = vec![0u8; len];
        client.read_exact(&mut buf).unwrap();
        serde_json::from_slice(&buf).unwrap()
    }

    fn publish_input_entry(
        dir: &std::path::Path,
        session: &str,
        serve_socket: Option<&std::path::Path>,
        stream_socket: Option<&std::path::Path>,
        watch_only: bool,
    ) {
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
            app_label: Some("Counter".to_string()),
            device_name: None,
            watch_only,
            serve_socket: serve_socket.map(|p| p.display().to_string()),
            activity: Some("dev.acos.fixtures.counter.MainActivity".to_string()),
            display: (1080, 2400),
            stream_socket: stream_socket.map(|p| p.display().to_string()),
            has_home: Some(false),
        };
        crate::live_registry::LiveRegistry::at(dir.to_path_buf())
            .write(&entry)
            .expect("publish input entry");
    }

    #[test]
    fn input_worker_handles_multitouch_scroll_and_disconnect_cancel() {
        let daemon = FakeDaemon::start("inp-d");
        let registry = scratch("inp-reg");
        publish_input_entry(&registry, "s_inp", Some(&daemon.socket), None, false);
        let hub = Hub::new(registry.clone());

        assert!(!hub.input_active("s_inp"), "zero cost until open_input");
        let client = hub.open_input("s_inp").expect("open input");
        assert!(hub.input_active("s_inp"));
        let (mut cli_rx, srv_tx) = ws_sink_pair();

        // Multi-touch: p0 down -> p1 down -> move -> p1 up -> disconnect cancels p0!
        client.enqueue(
            r#"{"t":"touch","phase":"down","id":0,"x":100,"y":200}"#,
            Arc::clone(&srv_tx),
        );
        assert_eq!(read_server_ws_text(&mut cli_rx)["ok"], json!(true));
        client.enqueue(
            r#"{"t":"touch","phase":"down","id":1,"x":300,"y":400}"#,
            Arc::clone(&srv_tx),
        );
        assert_eq!(read_server_ws_text(&mut cli_rx)["ok"], json!(true));
        client.enqueue(
            r#"{"t":"touch","phase":"move","id":1,"x":310,"y":410}"#,
            Arc::clone(&srv_tx),
        );
        assert_eq!(read_server_ws_text(&mut cli_rx)["ok"], json!(true));
        client.enqueue(
            r#"{"t":"touch","phase":"up","id":1,"x":320,"y":420}"#,
            Arc::clone(&srv_tx),
        );
        assert_eq!(read_server_ws_text(&mut cli_rx)["ok"], json!(true));
        // Hold a key and disconnect mid-gesture -> should cancel p0 and release key!
        client.enqueue(
            r#"{"t":"key","code":"KEYCODE_SPACE","phase":"down"}"#,
            Arc::clone(&srv_tx),
        );
        assert_eq!(read_server_ws_text(&mut cli_rx)["ok"], json!(true));
        drop(client);

        wait_until(
            || !hub.input_active("s_inp"),
            "input worker to exit after disconnect",
        );

        let recorded = daemon.calls.lock().unwrap().clone();
        assert_eq!(recorded.len(), 7, "expected 7 daemon calls: {recorded:#?}");
        assert_eq!(recorded[0].0, "panel_touch");
        assert_eq!(recorded[0].1["action"], json!(ACTION_DOWN));
        assert_eq!(
            recorded[1].1["action"],
            json!(ACTION_POINTER_DOWN | (1 << ACTION_POINTER_INDEX_SHIFT))
        );
        assert_eq!(recorded[1].1["pointers"].as_array().unwrap().len(), 2);
        assert_eq!(recorded[2].1["action"], json!(ACTION_MOVE));
        assert_eq!(
            recorded[3].1["action"],
            json!(ACTION_POINTER_UP | (1 << ACTION_POINTER_INDEX_SHIFT))
        );
        assert_eq!(recorded[4].0, "panel_key");
        assert_eq!(recorded[4].1["event_type"], json!("down"));
        assert_eq!(recorded[5].0, "panel_touch");
        assert_eq!(recorded[5].1["action"], json!(ACTION_CANCEL));
        assert_eq!(recorded[6].0, "panel_key");
        assert_eq!(recorded[6].1["event_type"], json!("up"));

        // Monotonic nanosecond timestamps across touch frames.
        let mut prev_ts = 0i64;
        for idx in [0, 1, 2, 3, 5] {
            let ts = recorded[idx].1["event_time"].as_i64().unwrap();
            assert!(ts > prev_ts, "non-monotonic event_time at {idx}: {ts} <= {prev_ts}");
            prev_ts = ts;
        }

        // Scroll + rotate update display.
        daemon.calls.lock().unwrap().clear();
        let client2 = hub.open_input("s_inp").expect("reopen input");
        client2.enqueue(
            r#"{"t":"scroll","x":540,"y":1200,"dy":250}"#,
            Arc::clone(&srv_tx),
        );
        assert_eq!(read_server_ws_text(&mut cli_rx)["ok"], json!(true));
        client2.enqueue(r#"{"t":"rotate","rotation":1}"#, Arc::clone(&srv_tx));
        let rot_reply = read_server_ws_text(&mut cli_rx);
        assert_eq!(rot_reply["ok"], json!(true));
        assert_eq!(rot_reply["width"], json!(2400));
        assert_eq!(rot_reply["height"], json!(1080));
        assert_eq!(client2.entry().display, (2400, 1080));

        let scroll_calls = daemon.calls.lock().unwrap().clone();
        // 1 DOWN + 6 MOVEs + 1 UP + 1 rotate = 9 calls.
        assert_eq!(scroll_calls.len(), 9);
        assert_eq!(scroll_calls[0].1["action"], json!(ACTION_DOWN));
        assert_eq!(scroll_calls[7].1["action"], json!(ACTION_UP));
        // Positive dy (scroll down) drags finger upward (decreasing y).
        let y0 = scroll_calls[0].1["pointers"][0][2].as_f64().unwrap();
        let y7 = scroll_calls[7].1["pointers"][0][2].as_f64().unwrap();
        assert!(y7 < y0, "expected y7 < y0, got {y7} vs {y0}");

        drop(client2);
        wait_until(|| !hub.input_active("s_inp"), "second worker exit");
        let _ = std::fs::remove_dir_all(&registry);
    }

    #[test]
    fn verify_watch_only_refuses_input_and_emits_ignored_marker() {
        let daemon = FakeDaemon::start("ver-d");
        let stream_root = scratch("ver-s");
        let stream_sock = stream_root.join("stream.sock");
        let listener = UnixListener::bind(&stream_sock).unwrap();
        let ignored_seen = Arc::new(Mutex::new(Vec::<String>::new()));
        let ignored_t = Arc::clone(&ignored_seen);
        std::thread::spawn(move || {
            for s in listener.incoming() {
                let Ok(s) = s else { break };
                let mut r = BufReader::new(s);
                let mut line = String::new();
                if r.read_line(&mut line).is_ok() && !line.is_empty() {
                    ignored_t.lock().unwrap().push(line);
                }
            }
        });

        let registry = scratch("ver-reg");
        publish_input_entry(
            &registry,
            "s_ver",
            Some(&daemon.socket),
            Some(&stream_sock),
            true,
        );
        let hub = Hub::new(registry.clone());
        let client = hub.open_input("s_ver").expect("open verify input");
        let (mut cli_rx, srv_tx) = ws_sink_pair();

        client.enqueue(
            r#"{"t":"touch","phase":"down","id":0,"x":100,"y":200}"#,
            Arc::clone(&srv_tx),
        );
        let reply = read_server_ws_text(&mut cli_rx);
        assert_eq!(reply["ok"], json!(false));
        assert_eq!(reply["code"], json!("VERIFY_WATCH_ONLY"));
        assert!(daemon.calls.lock().unwrap().is_empty());

        wait_until(
            || !ignored_seen.lock().unwrap().is_empty(),
            "panel_input_ignored marker on stream_socket",
        );
        assert!(ignored_seen.lock().unwrap()[0].contains("panel_input_ignored"));

        drop(client);
        wait_until(|| !hub.input_active("s_ver"), "verify input exit");
        let _ = std::fs::remove_dir_all(&registry);
        let _ = std::fs::remove_dir_all(&stream_root);
    }

    #[test]
    fn interactive_input_upgrades_stream_subscription_and_reverts_on_disconnect() {
        let producer = FakeProducer::start("upg");
        let registry = scratch("upg-reg");
        publish_input_entry(&registry, "s_upg", None, Some(&producer.socket), false);
        let hub = Hub::new(registry.clone());

        let watcher = hub.watch("s_upg").expect("watch");
        wait_until(
            || producer.subscribes.lock().unwrap().len() == 1,
            "initial watch subscribe",
        );
        let sub0 = producer.subscribes.lock().unwrap()[0]["subscribe"].clone();
        assert_eq!(sub0["scale"], json!(0.5));
        assert_eq!(sub0["max_fps"], json!(15));
        assert_eq!(sub0["format"], json!("png"));

        // Opening input upgrades the active upstream subscription immediately.
        let client = hub.open_input("s_upg").expect("open input");
        wait_until(
            || producer.subscribes.lock().unwrap().len() == 2,
            "interactive upgrade subscribe",
        );
        let sub1 = producer.subscribes.lock().unwrap()[1]["subscribe"].clone();
        assert_eq!(sub1["scale"], json!(1.0));
        assert_eq!(sub1["max_fps"], json!(30));
        assert_eq!(sub1["format"], json!("png"));

        // Dropping the last input client reverts to watch-only 0.5 PNG @ 15fps.
        drop(client);
        wait_until(
            || producer.subscribes.lock().unwrap().len() == 3,
            "revert after input disconnect",
        );
        let sub2 = producer.subscribes.lock().unwrap()[2]["subscribe"].clone();
        assert_eq!(sub2["scale"], json!(0.5));
        assert_eq!(sub2["max_fps"], json!(15));
        assert_eq!(sub2["format"], json!("png"));

        drop(watcher);
        wait_until(
            || producer.serving.load(Ordering::SeqCst) == 0,
            "upstream closes when last watcher leaves",
        );

        // A watch_only session must NEVER upgrade even if open_input is called.
        let prod_wo = FakeProducer::start("upg-wo");
        publish_input_entry(&registry, "s_upg_wo", None, Some(&prod_wo.socket), true);
        let w_wo = hub.watch("s_upg_wo").expect("watch_only watch");
        wait_until(
            || prod_wo.subscribes.lock().unwrap().len() == 1,
            "watch_only initial subscribe",
        );
        let c_wo = hub.open_input("s_upg_wo").expect("open watch_only input");
        std::thread::sleep(Duration::from_millis(120));
        assert_eq!(
            prod_wo.subscribes.lock().unwrap().len(),
            1,
            "watch_only session must not reconnect/upgrade"
        );
        drop(c_wo);
        drop(w_wo);
        wait_until(
            || prod_wo.serving.load(Ordering::SeqCst) == 0,
            "watch_only upstream closes",
        );

        let _ = std::fs::remove_dir_all(&registry);
    }

    #[test]
    fn detach_clears_cached_frame_so_reattach_waits_for_fresh_frame() {
        let root = scratch("detach-clr");
        let socket = root.join("view.sock");
        let listener = UnixListener::bind(&socket).expect("bind");
        let gate = Arc::new((Mutex::new(false), Condvar::new()));
        let gate_t = Arc::clone(&gate);
        std::thread::spawn(move || {
            let mut conn_num = 0u64;
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                conn_num += 1;
                let gate_c = Arc::clone(&gate_t);
                std::thread::spawn(move || {
                    let mut reader = BufReader::new(stream.try_clone().expect("clone"));
                    let mut line = String::new();
                    let _ = reader.read_line(&mut line);
                    let hello = r#"{"v":1,"session":42,"display":{"w":2,"h":2},"format":"png","scale":0.5}"#;
                    let _ = send_message(&mut stream, b'H', hello.as_bytes());
                    if conn_num == 2 {
                        let (lk, cv) = &*gate_c;
                        let mut ready = lk.lock().unwrap();
                        while !*ready {
                            ready = cv.wait(ready).unwrap();
                        }
                    }
                    let hdr = format!(
                        "{{\"seq\":{conn_num},\"generation\":{conn_num},\"w\":2,\"h\":2,\"format\":\"png\",\"ts_ms\":{}}}\n",
                        live_registry::now_ms()
                    );
                    let mut payload = hdr.into_bytes();
                    payload.extend_from_slice(&[0x89, b'P', b'N', b'G', conn_num as u8]);
                    let _ = send_message(&mut stream, b'F', &payload);
                    std::thread::sleep(Duration::from_secs(2));
                });
            }
        });

        let registry = scratch("detach-clr-reg");
        publish(&registry, "s_dclr", &socket);
        let hub = Hub::new(registry.clone());

        let w1 = hub.watch("s_dclr").expect("watch 1");
        let f1 = w1
            .wait_frame(0, Duration::from_secs(5))
            .expect("first frame");
        assert_eq!(f1.seq, 1);
        drop(w1);

        // Reattach while connection 2 has not sent a frame yet: must NOT receive f1!
        let w2 = hub.watch("s_dclr").expect("watch 2");
        assert!(
            w2.wait_frame(0, Duration::from_millis(120)).is_none(),
            "reattached watcher must not receive stale frame from previous watch"
        );

        // Unblock connection 2's fresh frame and verify it arrives.
        {
            let (lk, cv) = &*gate;
            *lk.lock().unwrap() = true;
            cv.notify_all();
        }
        let f2 = w2
            .wait_frame(0, Duration::from_secs(5))
            .expect("fresh frame on reattach");
        assert_eq!(f2.seq, 2);
        drop(w2);

        let _ = std::fs::remove_dir_all(&registry);
        let _ = std::fs::remove_dir_all(&root);
    }
}
