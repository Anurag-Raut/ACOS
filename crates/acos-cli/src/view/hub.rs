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
use std::sync::atomic::{AtomicBool, Ordering};
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
/// Hard cap on queued WebSocket input commands per session (CWE-770 bound).
const INPUT_QUEUE_LIMIT: usize = 128;
/// Cap for non-terminal commands, reserving 16 slots for `touch up/cancel` and `key up`.
const INPUT_QUEUE_NORMAL_LIMIT: usize = 112;
/// Maximum simultaneous active pointers tracked across all clients on one session.
const MAX_POINTERS: i32 = 32;

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
    pub launch_token: Option<String>,
}

impl Entry {
    /// Whether two registry snapshots refer to the same live guest instance.
    #[must_use]
    pub fn same_guest(&self, other: &Self) -> bool {
        self.session_id == other.session_id
            && self.daemon_pid == other.daemon_pid
            && self.launcher_pid == other.launcher_pid
            && self.launch_token == other.launch_token
            && self.stream_socket == other.stream_socket
            && self.serve_socket == other.serve_socket
            && self.watch_only == other.watch_only
    }

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
        let launch_token = doc
            .get("launch_token")
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
            launch_token,
        })
    }

    /// The JSON shape the page consumes (the registry entry plus what the hub
    /// knows about the stream). Note `serve_socket` and `launch_token` stay
    /// internal to the hub.
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
    /// Non-secret hub-assigned generation identifying this specific live guest
    /// instance. Increments whenever the session id is reused by a newer guest.
    pub guest_gen: u64,
    pub watchers: usize,
    pub connected: bool,
}

/// Failure reason when opening an interactive input client on a session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpenInputError {
    /// No live session exists in the registry for this `session_id`.
    NotFound,
    /// A newer guest replaced the watched `guest_gen` for this `session_id`.
    Replaced,
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
    next_guest_gen: u64,
    guest_gens: BTreeMap<String, u64>,
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
                next_guest_gen: 1,
                guest_gens: BTreeMap::new(),
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

    /// Re-read the registry: hide (and delete) stale entries, refresh metadata,
    /// bump `guest_gen` on guest replacement, and evict slots whose session id
    /// was replaced by a newer guest instance.
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
        for (key, entry) in &entries {
            let same_as_prev = inner
                .entries
                .get(key)
                .is_some_and(|prev| prev.same_guest(entry));
            if !same_as_prev || !inner.guest_gens.contains_key(key) {
                let gen = inner.next_guest_gen;
                inner.next_guest_gen = inner.next_guest_gen.wrapping_add(1).max(1);
                inner.guest_gens.insert(key.clone(), gen);
            }
        }
        inner.guest_gens.retain(|key, _| entries.contains_key(key));
        let mut replaced_slots = Vec::new();
        for (key, slot) in &inner.slots {
            if let Some(entry) = entries.get(key) {
                let mut meta = slot.meta.lock().expect("slot meta");
                if meta.same_guest(entry) {
                    *meta = entry.clone();
                } else {
                    drop(meta);
                    slot.mark_replaced();
                    replaced_slots.push(key.clone());
                }
            }
        }
        for key in replaced_slots {
            inner.slots.remove(&key);
        }
        let mut replaced_inputs = Vec::new();
        for (key, input_slot) in &inner.input_slots {
            if let Some(entry) = entries.get(key) {
                let mut meta = input_slot.meta.lock().expect("input slot meta");
                if meta.same_guest(entry) {
                    *meta = entry.clone();
                } else {
                    drop(meta);
                    input_slot.mark_replaced();
                    replaced_inputs.push(key.clone());
                }
            }
        }
        for key in replaced_inputs {
            inner.input_slots.remove(&key);
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
                let guest_gen = inner
                    .guest_gens
                    .get(&entry.session_id)
                    .copied()
                    .unwrap_or(0);
                let (watchers, connected) = inner
                    .slots
                    .get(&entry.session_id)
                    .map_or((0, false), |slot| slot.observed());
                SessionView {
                    entry: entry.clone(),
                    guest_gen,
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
        let (slot, socket, input_slot) = {
            let mut inner = self.inner.lock().expect("hub");
            self.refresh_locked(&mut inner);
            let entry = inner.entries.get(session_id)?.clone();
            let guest_gen = inner.guest_gens.get(session_id).copied().unwrap_or(0);
            let socket = entry.stream_socket.clone()?;
            if !std::path::Path::new(&socket).exists() {
                return None;
            }
            let slot = if let Some(slot) = inner.slots.get(session_id) {
                Arc::clone(slot)
            } else {
                let slot = Arc::new(Slot::new(entry, guest_gen));
                inner
                    .slots
                    .insert(session_id.to_string(), Arc::clone(&slot));
                slot
            };
            let input_slot = inner.input_slots.get(session_id).cloned();
            if let Some(ref input_slot) = input_slot {
                *input_slot.stream_slot.lock().expect("input stream slot") =
                    Some(Arc::downgrade(&slot));
            } else {
                // Serialize the no-input reset while holding `self.inner` so a
                // concurrent `open_input_checked` cannot create an `InputSlot`
                // and set interactive=true before this `false` write lands.
                slot.set_interactive(false);
            }
            (slot, socket, input_slot)
        };
        if let Some(input_slot) = input_slot {
            input_slot.sync_interactive();
        }
        slot.attach(socket);
        Some(Watcher { slot })
    }

    /// Open an interactive input client on `session_id` bound to `expected_guest_gen`,
    /// lazily spawning the session's single ordered [`InputSlot`] worker if one is
    /// not running.
    ///
    /// Rejects with [`OpenInputError::NotFound`] when no live session exists and
    /// [`OpenInputError::Replaced`] when a newer guest has replaced `expected_guest_gen`.
    pub fn open_input_checked(
        &self,
        session_id: &str,
        expected_guest_gen: u64,
    ) -> Result<InputClient, OpenInputError> {
        let slot = {
            let mut inner = self.inner.lock().expect("hub");
            self.refresh_locked(&mut inner);
            let entry = inner
                .entries
                .get(session_id)
                .ok_or(OpenInputError::NotFound)?
                .clone();
            let current_gen = inner.guest_gens.get(session_id).copied().unwrap_or(0);
            if expected_guest_gen != current_gen {
                return Err(OpenInputError::Replaced);
            }
            let stream_slot = inner.slots.get(session_id).map(Arc::downgrade);
            let slot = if let Some(slot) = inner.input_slots.get(session_id) {
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
        Ok(slot.attach_client())
    }

    /// Test convenience wrapper that opens input against the current live guest
    /// generation for `session_id`.
    #[cfg(test)]
    #[must_use]
    pub fn open_input(&self, session_id: &str) -> Option<InputClient> {
        let guest_gen = {
            let mut inner = self.inner.lock().expect("hub");
            self.refresh_locked(&mut inner);
            inner.guest_gens.get(session_id).copied()?
        };
        self.open_input_checked(session_id, guest_gen).ok()
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

    /// Test seam to gate the worker's background recovery timer when a test
    /// asserts deterministic foreground `req_idx` ordering.
    #[cfg(test)]
    pub(crate) fn set_scheduled_recovery_enabled(&self, session_id: &str, enabled: bool) {
        let slot = {
            let inner = self.inner.lock().expect("hub");
            inner.input_slots.get(session_id).cloned()
        };
        if let Some(slot) = slot {
            let mut state = slot.state.lock().expect("input slot");
            state.scheduled_recovery_enabled = enabled;
            slot.cv.notify_all();
        }
    }

    /// Test seam to wait until the input worker has completed at least
    /// `min_completed` scheduled recovery attempts and settled its next wait
    /// decision, returning `(completed, recovery_attempts, retry_scheduled)`.
    #[cfg(test)]
    pub(crate) fn wait_for_scheduled_recovery_settled(
        &self,
        session_id: &str,
        min_completed: u64,
        timeout: Duration,
    ) -> Option<(u64, u32, bool)> {
        let slot = {
            let inner = self.inner.lock().expect("hub");
            inner.input_slots.get(session_id).cloned()
        }?;
        let state = slot.state.lock().expect("input slot");
        let (state, _) = slot
            .recovery_obs_cv
            .wait_timeout_while(state, timeout, |s| s.recovery_obs.0 < min_completed)
            .unwrap_or_else(|poison| poison.into_inner());
        (state.recovery_obs.0 >= min_completed).then_some(state.recovery_obs)
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
/// detach closes the upstream socket (and joins its thread) before returning.
pub struct Watcher {
    slot: Arc<Slot>,
}

impl Watcher {
    /// The session's current registry metadata.
    #[must_use]
    pub fn entry(&self) -> Entry {
        self.slot.meta.lock().expect("slot meta").clone()
    }

    /// The non-secret guest generation this watcher is bound to.
    #[must_use]
    pub fn guest_gen(&self) -> u64 {
        self.slot.guest_gen
    }

    /// `true` once a newer guest replaced this watched guest under the same session id.
    #[must_use]
    pub fn is_replaced(&self) -> bool {
        self.slot.is_replaced()
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
            if state.replaced {
                return None;
            }
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
            if !batch.events.is_empty() || state.replaced {
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
    guest_gen: u64,
    meta: Mutex<Entry>,
    state: Mutex<SlotState>,
    cv: Condvar,
}

struct SlotState {
    watchers: usize,
    running: bool,
    stopping: bool,
    replaced: bool,
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
    fn new(entry: Entry, guest_gen: u64) -> Self {
        Self {
            guest_gen,
            meta: Mutex::new(entry),
            state: Mutex::new(SlotState {
                watchers: 0,
                running: false,
                stopping: false,
                replaced: false,
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

    fn mark_replaced(&self) {
        let mut state = self.state.lock().expect("slot");
        if !state.replaced {
            state.replaced = true;
            if let Some(stream) = state.active_stream.as_ref() {
                let _ = stream.shutdown(Shutdown::Both);
            }
            self.cv.notify_all();
        }
    }

    fn is_replaced(&self) -> bool {
        self.state.lock().expect("slot").replaced
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
            state.connected = false;
            state.hello = None;
            state.frame = None;
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
        if state.watchers > 0 && !state.replaced {
            if let Some(socket) = state.socket.clone() {
                state.running = true;
                let slot = Arc::clone(self);
                state.handle = Some(std::thread::spawn(move || slot.upstream_loop(socket)));
            }
        }
        self.cv.notify_all();
    }

    fn stopping(&self) -> bool {
        let state = self.state.lock().expect("slot");
        state.stopping || state.replaced
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
                        state.connected = false;
                        let reconfigured = state.mode_epoch != start_epoch;
                        if !reconfigured && !state.stopping {
                            if let Err(error) = &outcome {
                                state.error = Some(error.to_string());
                                state.upstream_errors += 1;
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
                    let mut state = self.state.lock().expect("slot");
                    if !state.stopping {
                        state.hello = Some(hello);
                        drop(state);
                        self.cv.notify_all();
                    }
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
        if state.stopping {
            return Ok(());
        }
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
    terminated: AtomicBool,
    slot: Arc<InputSlot>,
}

impl InputClient {
    /// Current session metadata snapshot.
    #[must_use]
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn entry(&self) -> Entry {
        self.slot.meta.lock().expect("input slot meta").clone()
    }

    /// Parse one raw WebSocket JSON text message and enqueue it onto the
    /// session's ordered worker. Invalid JSON/arguments, replaced sessions, and
    /// queue-overflow rejections reply immediately on `reply_to` without
    /// hitting the worker.
    ///
    /// Returns `false` when the client was terminated (for example, a terminal
    /// release hit `INPUT_QUEUE_LIMIT` and triggered priority disconnect
    /// cleanup); the WebSocket loop should close the connection.
    pub fn enqueue(&self, raw_json: &str, reply_to: Arc<Mutex<TcpStream>>) -> bool {
        if self.terminated.load(Ordering::Acquire) {
            let reply = json!({
                "ok": false,
                "code": "QUEUE_FULL",
                "error": "input client closed after queue overflow",
                "disconnected": true,
            })
            .to_string();
            let _ = super::ws::send_text_locked(&reply_to, &reply);
            return false;
        }
        let cmd = match parse_input_command(raw_json) {
            Ok(cmd) => cmd,
            Err(err_msg) => {
                let reply = err_json("INVALID_ARGUMENT", &err_msg).to_string();
                let _ = super::ws::send_text_locked(&reply_to, &reply);
                return true;
            }
        };

        let mut keep_open = true;
        let mut immediate_reply: Option<(Arc<Mutex<TcpStream>>, String)> = None;
        {
            let mut state = self.slot.state.lock().expect("input slot");
            if state.replaced {
                immediate_reply = Some((
                    reply_to,
                    err_json("SESSION_REPLACED", "session was replaced by a newer guest")
                        .to_string(),
                ));
            } else {
                // Coalesce consecutive pending `touch move` commands ONLY when
                // the queue tail itself is a `touch move` for the same
                // `(client_id, pointer_id)`. Scanning past intervening commands
                // (such as a rotation or another pointer's down) would reorder them.
                let mut coalesced = false;
                if let InputCommand::Touch {
                    phase: TouchPhase::Move,
                    id,
                    x,
                    y,
                } = &cmd
                {
                    if let Some(pending) = state.queue.back_mut() {
                        if pending.client_id == self.client_id {
                            if let InputCommand::Touch {
                                phase: TouchPhase::Move,
                                id: prev_id,
                                x: prev_x,
                                y: prev_y,
                            } = &mut pending.cmd
                            {
                                if *prev_id == *id {
                                    *prev_x = *x;
                                    *prev_y = *y;
                                    let superseded =
                                        std::mem::replace(&mut pending.reply_to, reply_to.clone());
                                    immediate_reply = Some((
                                        superseded,
                                        json!({
                                            "ok": true,
                                            "t": "touch",
                                            "phase": "move",
                                            "coalesced": true,
                                        })
                                        .to_string(),
                                    ));
                                    coalesced = true;
                                }
                            }
                        }
                    }
                }
                if !coalesced {
                    let is_terminal = matches!(
                        cmd,
                        InputCommand::Touch {
                            phase: TouchPhase::Up | TouchPhase::Cancel,
                            ..
                        } | InputCommand::Key {
                            phase: KeyPhase::Up,
                            ..
                        }
                    );
                    let limit = if is_terminal {
                        INPUT_QUEUE_LIMIT
                    } else {
                        INPUT_QUEUE_NORMAL_LIMIT
                    };
                    if state.queue.len() >= limit {
                        if is_terminal {
                            // Never drop a release silently and leave an
                            // admitted pointer/key stuck down in the guest:
                            // prune this client's pending commands, schedule
                            // priority disconnect cleanup, and close the WS.
                            self.terminated.store(true, Ordering::Release);
                            state.queue.retain(|task| task.client_id != self.client_id);
                            state.disconnects.push_back(self.client_id);
                            self.slot.cv.notify_one();
                            keep_open = false;
                            immediate_reply = Some((
                                reply_to,
                                json!({
                                    "ok": false,
                                    "code": "QUEUE_FULL",
                                    "error": "input queue full; cancelling active input",
                                    "disconnected": true,
                                })
                                .to_string(),
                            ));
                        } else {
                            let mut reply = err_json("QUEUE_FULL", "input queue full");
                            if matches!(cmd, InputCommand::Rotate { .. }) {
                                reply["t"] = json!("rotate");
                            }
                            immediate_reply = Some((reply_to, reply.to_string()));
                        }
                    } else {
                        state.queue.push_back(QueuedCommand {
                            client_id: self.client_id,
                            cmd,
                            reply_to,
                        });
                        self.slot.cv.notify_one();
                    }
                }
            }
        }
        if let Some((target, payload)) = immediate_reply {
            let _ = super::ws::send_text_locked(&target, &payload);
        }
        keep_open
    }
}

impl Drop for InputClient {
    fn drop(&mut self) {
        {
            let mut state = self.slot.state.lock().expect("input slot");
            state.clients = state.clients.saturating_sub(1);
            // Drop any unsent commands from this disconnecting client and push
            // its cleanup onto the priority disconnects queue so active
            // pointers/keys release immediately even under backlog.
            state.queue.retain(|task| task.client_id != self.client_id);
            if !self.terminated.swap(true, Ordering::AcqRel) {
                state.disconnects.push_back(self.client_id);
            }
            self.slot.cv.notify_one();
        }
        self.slot.sync_interactive();
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

struct QueuedCommand {
    client_id: u64,
    cmd: InputCommand,
    reply_to: Arc<Mutex<TcpStream>>,
}

enum WorkerTask {
    Command(QueuedCommand),
    ClientDisconnected { client_id: u64 },
    ScheduledRecovery,
}

const RECOVERY_INITIAL_BACKOFF_MS: u64 = 25;
const RECOVERY_MAX_BACKOFF_MS: u64 = 500;
const RECOVERY_MAX_ATTEMPTS: u32 = 12;

struct InputSlot {
    meta: Mutex<Entry>,
    stream_slot: Mutex<Option<Weak<Slot>>>,
    interactive_lock: Mutex<()>,
    state: Mutex<InputSlotState>,
    cv: Condvar,
    #[cfg(test)]
    recovery_obs_cv: Condvar,
}

#[derive(Clone, Debug)]
struct PendingTouchRecovery {
    wire_id: i32,
    x: f32,
    y: f32,
    down_time_ns: i64,
}

#[derive(Clone, Debug)]
enum PendingRelease {
    Up {
        client_id: u64,
        pointer_id: i32,
        x: f32,
        y: f32,
    },
    CancelClient {
        client_id: u64,
    },
}

struct InputSlotState {
    clients: usize,
    next_client_id: u64,
    running: bool,
    replaced: bool,
    #[cfg(test)]
    scheduled_recovery_enabled: bool,
    #[cfg(test)]
    recovery_obs: (u64, u32, bool),
    last_event_time_ns: i64,
    pending_touch_recovery: Option<PendingTouchRecovery>,
    pending_releases: Vec<PendingRelease>,
    pointers: Vec<ActivePointer>,
    down_time_ns: Option<i64>,
    disconnects: VecDeque<u64>,
    queue: VecDeque<QueuedCommand>,
}

impl InputSlot {
    fn new(entry: Entry) -> Self {
        Self {
            meta: Mutex::new(entry),
            stream_slot: Mutex::new(None),
            interactive_lock: Mutex::new(()),
            state: Mutex::new(InputSlotState {
                clients: 0,
                next_client_id: 1,
                running: false,
                replaced: false,
                #[cfg(test)]
                scheduled_recovery_enabled: true,
                #[cfg(test)]
                recovery_obs: (0, 0, false),
                last_event_time_ns: 0,
                pending_touch_recovery: None,
                pending_releases: Vec::new(),
                pointers: Vec::new(),
                down_time_ns: None,
                disconnects: VecDeque::new(),
                queue: VecDeque::new(),
            }),
            cv: Condvar::new(),
            #[cfg(test)]
            recovery_obs_cv: Condvar::new(),
        }
    }

    fn is_active(&self) -> bool {
        let state = self.state.lock().expect("input slot");
        state.clients > 0 || state.running
    }

    fn mark_replaced(&self) {
        let mut state = self.state.lock().expect("input slot");
        state.replaced = true;
        self.cv.notify_all();
    }

    fn is_replaced(&self) -> bool {
        self.state.lock().expect("input slot").replaced
    }

    fn is_interactive(&self) -> bool {
        if self.meta.lock().expect("input slot meta").watch_only {
            return false;
        }
        let state = self.state.lock().expect("input slot");
        state.clients > 0 && !state.replaced
    }

    /// Serialize `Slot::set_interactive` transitions across concurrent
    /// `attach_client`, `InputClient::drop`, and `Hub::watch` calls so the
    /// stream slot always converges on the true current interactive state.
    fn sync_interactive(&self) {
        let _guard = self
            .interactive_lock
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let interactive = self.is_interactive();
        let stream_slot = self
            .stream_slot
            .lock()
            .expect("input stream slot")
            .as_ref()
            .and_then(Weak::upgrade);
        if let Some(stream_slot) = stream_slot {
            stream_slot.set_interactive(interactive);
        }
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
            } else {
                self.cv.notify_all();
            }
            client_id
        };
        self.sync_interactive();
        InputClient {
            client_id,
            terminated: AtomicBool::new(false),
            slot: Arc::clone(self),
        }
    }
}

enum DaemonCallErr {
    /// Failed while writing the request line (safe to retry once on a stale
    /// keep-alive connection since the daemon did not receive the command).
    WriteFailed(String, String),
    /// Failed after the request was written (`IO_ERROR` / `EOF` / `BAD_REPLY` /
    /// `TIMEOUT` / `APP_NOT_RESPONDING`); must NOT be blindly replayed because
    /// the input may already have fired.
    PostWriteAmbiguous(String, String),
    /// Daemon explicitly returned a structured error envelope (definite rejection).
    DaemonRejected(String, String),
}

impl DaemonCallErr {
    fn into_outcome(self) -> (String, String, bool) {
        match self {
            Self::WriteFailed(c, m) | Self::DaemonRejected(c, m) => (c, m, false),
            Self::PostWriteAmbiguous(c, m) => (c, m, true),
        }
    }
}

fn is_guest_gone_code(code: &str) -> bool {
    matches!(code, "SESSION_REPLACED" | "SESSION_NOT_FOUND")
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

    fn call(&mut self, tool: &str, args: Value) -> Result<Value, DaemonCallErr> {
        let id = self.next_id;
        self.next_id += 1;
        let line = json!({
            "id": id,
            "tool": tool,
            "args": args,
        })
        .to_string()
            + "\n";
        let bytes = line.as_bytes();
        let mut written = 0usize;
        while written < bytes.len() {
            match self.writer.write(&bytes[written..]) {
                Ok(0) => {
                    let msg = "failed to write request to daemon".to_string();
                    return Err(if written == 0 {
                        DaemonCallErr::WriteFailed("IO_ERROR".to_string(), msg)
                    } else {
                        DaemonCallErr::PostWriteAmbiguous("IO_ERROR".to_string(), msg)
                    });
                }
                Ok(n) => written += n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => {
                    return Err(if written == 0 {
                        DaemonCallErr::WriteFailed("IO_ERROR".to_string(), e.to_string())
                    } else {
                        DaemonCallErr::PostWriteAmbiguous("IO_ERROR".to_string(), e.to_string())
                    });
                }
            }
        }
        self.writer
            .flush()
            .map_err(|e| DaemonCallErr::PostWriteAmbiguous("IO_ERROR".to_string(), e.to_string()))?;
        let mut resp_line = String::new();
        let n = self
            .reader
            .read_line(&mut resp_line)
            .map_err(|e| DaemonCallErr::PostWriteAmbiguous("IO_ERROR".to_string(), e.to_string()))?;
        if n == 0 {
            return Err(DaemonCallErr::PostWriteAmbiguous(
                "EOF".to_string(),
                "daemon closed socket".to_string(),
            ));
        }
        let resp: Value = serde_json::from_str(&resp_line).map_err(|e| {
            DaemonCallErr::PostWriteAmbiguous(
                "BAD_REPLY".to_string(),
                format!("invalid daemon JSON: {e}"),
            )
        })?;
        let envelope = resp.get("envelope").ok_or_else(|| {
            DaemonCallErr::PostWriteAmbiguous(
                "BAD_REPLY".to_string(),
                "daemon response missing envelope".to_string(),
            )
        })?;
        if envelope.get("ok").and_then(Value::as_bool) == Some(true) {
            Ok(envelope.get("result").cloned().unwrap_or(Value::Null))
        } else {
            let reason = envelope
                .pointer("/error/details/reason")
                .and_then(Value::as_str);
            let code = if reason == Some("SESSION_REPLACED") {
                "SESSION_REPLACED".to_string()
            } else {
                envelope
                    .pointer("/error/code")
                    .and_then(Value::as_str)
                    .unwrap_or("INPUT_FAILED")
                    .to_string()
            };
            let message = envelope
                .pointer("/error/message")
                .and_then(Value::as_str)
                .unwrap_or("panel input rejected")
                .to_string();
            // A TIMEOUT or APP_NOT_RESPONDING reply after the request was written
            // can still describe an input the guest already applied; treat it as
            // post-write ambiguous so touch recovery compensates without replay.
            if code == "TIMEOUT" || code == "APP_NOT_RESPONDING" {
                Err(DaemonCallErr::PostWriteAmbiguous(code, message))
            } else {
                Err(DaemonCallErr::DaemonRejected(code, message))
            }
        }
    }
}

#[derive(Clone)]
struct ActivePointer {
    client_id: u64,
    pointer_id: i32,
    wire_id: i32,
    x: f32,
    y: f32,
}

struct InputWorker {
    slot: Arc<InputSlot>,
    conn: Option<DaemonConn>,
    pointers: Vec<ActivePointer>,
    down_time_ns: Option<i64>,
    last_event_time_ns: i64,
    pending_touch_recovery: Option<PendingTouchRecovery>,
    pending_releases: Vec<PendingRelease>,
    recovery_episode: u64,
    held_keys: Vec<(u64, i32)>,
}

impl InputWorker {
    fn new(slot: Arc<InputSlot>) -> Self {
        let (last_event_time_ns, pending_touch_recovery, pending_releases, pointers, down_time_ns) = {
            let mut state = slot.state.lock().expect("input slot");
            (
                state.last_event_time_ns,
                state.pending_touch_recovery.take(),
                std::mem::take(&mut state.pending_releases),
                std::mem::take(&mut state.pointers),
                state.down_time_ns.take(),
            )
        };
        Self {
            slot,
            conn: None,
            pointers,
            down_time_ns,
            last_event_time_ns,
            pending_touch_recovery,
            pending_releases,
            recovery_episode: 0,
            held_keys: Vec::new(),
        }
    }

    fn bump_recovery_episode(&mut self) {
        self.recovery_episode = self.recovery_episode.wrapping_add(1);
    }

    fn next_ts_ns(&mut self) -> i64 {
        let now = monotonic_nanos();
        let ts = now.max(self.last_event_time_ns + 1);
        self.last_event_time_ns = ts;
        ts
    }

    fn alloc_wire_id(&self) -> Option<i32> {
        (0..MAX_POINTERS).find(|id| !self.pointers.iter().any(|p| p.wire_id == *id))
    }

    fn run(mut self) {
        let mut recovery_attempts: u32 = 0;
        let mut next_recovery_at: Option<Instant> = None;
        let mut scheduled_episode = self.recovery_episode;
        #[cfg(test)]
        let mut completed_scheduled_recoveries: u64 = 0;
        loop {
            let task = {
                let mut state = self.slot.state.lock().expect("input slot");
                loop {
                    if let Some(client_id) = state.disconnects.pop_front() {
                        scheduled_episode = self.recovery_episode;
                        recovery_attempts = 0;
                        next_recovery_at = None;
                        break WorkerTask::ClientDisconnected { client_id };
                    }
                    let needs_recovery = !state.replaced
                        && (self.pending_touch_recovery.is_some()
                            || !self.pending_releases.is_empty());
                    #[cfg(test)]
                    let scheduled_allowed = state.scheduled_recovery_enabled;
                    #[cfg(not(test))]
                    let scheduled_allowed = true;

                    let recovery_deadline = if needs_recovery
                        && scheduled_allowed
                        && recovery_attempts < RECOVERY_MAX_ATTEMPTS
                    {
                        let now = Instant::now();
                        let deadline = *next_recovery_at.get_or_insert_with(|| {
                            let shift = recovery_attempts.min(5);
                            let ms = RECOVERY_INITIAL_BACKOFF_MS
                                .saturating_mul(1u64 << shift)
                                .min(RECOVERY_MAX_BACKOFF_MS);
                            now + Duration::from_millis(ms)
                        });
                        if now >= deadline {
                            recovery_attempts += 1;
                            next_recovery_at = None;
                            break WorkerTask::ScheduledRecovery;
                        }
                        Some((now, deadline))
                    } else {
                        None
                    };

                    if let Some(cmd) = state.queue.pop_front() {
                        // Preserve an armed recovery schedule while queued
                        // commands (such as text or keystrokes) drain, so a
                        // continuous command stream cannot starve due touch
                        // recovery. Reset only when nothing is pending or
                        // retries were already exhausted (so a new command
                        // re-arms bounded retries).
                        if !needs_recovery || recovery_attempts >= RECOVERY_MAX_ATTEMPTS {
                            scheduled_episode = self.recovery_episode;
                            recovery_attempts = 0;
                            next_recovery_at = None;
                        }
                        break WorkerTask::Command(cmd);
                    }

                    if let Some((now, deadline)) = recovery_deadline {
                        #[cfg(test)]
                        {
                            state.recovery_obs =
                                (completed_scheduled_recoveries, recovery_attempts, true);
                            self.slot.recovery_obs_cv.notify_all();
                        }
                        let wait_dur = deadline.saturating_duration_since(now);
                        let (new_state, _) = self
                            .slot
                            .cv
                            .wait_timeout(state, wait_dur)
                            .unwrap_or_else(|poison| poison.into_inner());
                        state = new_state;
                        continue;
                    }

                    if state.clients == 0 {
                        #[cfg(test)]
                        {
                            state.recovery_obs =
                                (completed_scheduled_recoveries, recovery_attempts, false);
                            self.slot.recovery_obs_cv.notify_all();
                        }
                        state.last_event_time_ns = self.last_event_time_ns;
                        state.pending_touch_recovery = self.pending_touch_recovery.take();
                        state.pending_releases = std::mem::take(&mut self.pending_releases);
                        state.pointers = std::mem::take(&mut self.pointers);
                        state.down_time_ns = self.down_time_ns.take();
                        state.running = false;
                        return;
                    }
                    next_recovery_at = None;
                    if !needs_recovery {
                        scheduled_episode = self.recovery_episode;
                        recovery_attempts = 0;
                    }
                    #[cfg(test)]
                    {
                        state.recovery_obs =
                            (completed_scheduled_recoveries, recovery_attempts, false);
                        self.slot.recovery_obs_cv.notify_all();
                    }
                    state = self
                        .slot
                        .cv
                        .wait(state)
                        .unwrap_or_else(|poison| poison.into_inner());
                }
            };
            match task {
                WorkerTask::Command(QueuedCommand {
                    client_id,
                    cmd,
                    reply_to,
                }) => {
                    let reply = self.handle_command(client_id, cmd);
                    let _ = super::ws::send_text_locked(&reply_to, &reply.to_string());
                    // Reset retry budget if this command made recovery progress
                    // (including resolving episode A and starting episode B) or
                    // left no pending work. Ordinary text/key commands leave
                    // `recovery_episode` unchanged and preserve the schedule.
                    if self.recovery_episode != scheduled_episode
                        || (self.pending_touch_recovery.is_none()
                            && self.pending_releases.is_empty())
                    {
                        scheduled_episode = self.recovery_episode;
                        recovery_attempts = 0;
                        next_recovery_at = None;
                    }
                }
                WorkerTask::ClientDisconnected { client_id } => {
                    self.cancel_client(client_id);
                    if self.recovery_episode != scheduled_episode
                        || (self.pending_touch_recovery.is_none()
                            && self.pending_releases.is_empty())
                    {
                        scheduled_episode = self.recovery_episode;
                        recovery_attempts = 0;
                        next_recovery_at = None;
                    }
                }
                WorkerTask::ScheduledRecovery => {
                    if self.resolve_pending_touch_recovery_and_releases().is_ok()
                        || self.recovery_episode != scheduled_episode
                    {
                        scheduled_episode = self.recovery_episode;
                        recovery_attempts = 0;
                        next_recovery_at = None;
                    }
                    #[cfg(test)]
                    {
                        completed_scheduled_recoveries += 1;
                    }
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

    fn call_daemon(&mut self, tool: &str, args: Value) -> Result<Value, (String, String)> {
        self.call_daemon_outcome(tool, args)
            .map_err(|(c, m, _)| (c, m))
    }

    /// Call the session daemon and return `Err((code, message, may_have_fired))`.
    /// `may_have_fired` is `true` only when the request bytes were written and
    /// flushed before an ambiguous read/EOF/decode failure occurred.
    fn call_daemon_outcome(
        &mut self,
        tool: &str,
        mut args: Value,
    ) -> Result<Value, (String, String, bool)> {
        if self.slot.is_replaced() {
            self.conn = None;
            return Err((
                "SESSION_REPLACED".to_string(),
                "session was replaced by a newer guest".to_string(),
                false,
            ));
        }
        let entry = self.entry();
        let Some(sock) = entry.serve_socket.as_deref() else {
            return Err((
                "INPUT_UNAVAILABLE".to_string(),
                "session has no input socket".to_string(),
                false,
            ));
        };
        if let Some(obj) = args.as_object_mut() {
            obj.insert(
                "base".to_string(),
                json!({ "session_id": entry.session_id }),
            );
            if let Some(token) = entry.launch_token.as_deref() {
                obj.insert("expected_launch_token".to_string(), json!(token));
            }
        }
        let need_new = self
            .conn
            .as_ref()
            .is_none_or(|c| c.socket_path != sock);
        if need_new {
            self.conn = Some(DaemonConn::connect(sock).map_err(|(c, m)| (c, m, false))?);
        }
        let reused_conn = !need_new;
        let first_err = match self.conn.as_mut().expect("conn").call(tool, args.clone()) {
            Ok(res) => return Ok(res),
            Err(err) => err,
        };
        let final_err = match first_err {
            DaemonCallErr::WriteFailed(c, m) => {
                self.conn = None;
                if reused_conn {
                    let mut fresh = DaemonConn::connect(sock).map_err(|(c, m)| (c, m, false))?;
                    match fresh.call(tool, args) {
                        Ok(res) => {
                            self.conn = Some(fresh);
                            return Ok(res);
                        }
                        Err(err) => err,
                    }
                } else {
                    DaemonCallErr::WriteFailed(c, m)
                }
            }
            other => other,
        };
        let (c, m, may_have_fired) = final_err.into_outcome();
        if may_have_fired || c == "IO_ERROR" || c == "EOF" || c == "SESSION_REPLACED" {
            self.conn = None;
        }
        if c == "SESSION_REPLACED" {
            self.slot.mark_replaced();
        }
        Err((c, m, may_have_fired))
    }

    fn pointer_wire_list(&self) -> Vec<Value> {
        self.pointers
            .iter()
            .map(|p| json!([p.wire_id, p.x, p.y]))
            .collect()
    }

    fn send_touch_frame(
        &mut self,
        action: i32,
        down_time: i64,
        event_time: i64,
    ) -> Result<Value, (String, String)> {
        self.send_touch_frame_outcome(action, down_time, event_time)
            .map_err(|(c, m, _)| (c, m))
    }

    fn send_touch_frame_outcome(
        &mut self,
        action: i32,
        down_time: i64,
        event_time: i64,
    ) -> Result<Value, (String, String, bool)> {
        let pointers = self.pointer_wire_list();
        self.call_daemon_outcome(
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

    fn clear_guest_touch_state(&mut self) {
        if self.pending_touch_recovery.is_some() || !self.pending_releases.is_empty() {
            self.bump_recovery_episode();
        }
        self.pending_touch_recovery = None;
        self.pending_releases.clear();
        self.pointers.clear();
        self.down_time_ns = None;
    }

    fn record_pending_up(&mut self, client_id: u64, pointer_id: i32, x: f32, y: f32) {
        if let Some(pos) = self
            .pointers
            .iter()
            .position(|p| p.client_id == client_id && p.pointer_id == pointer_id)
        {
            self.pointers[pos].x = x;
            self.pointers[pos].y = y;
        } else {
            return;
        }
        if self.pending_releases.iter().any(|r| {
            matches!(r, PendingRelease::CancelClient { client_id: cid } if *cid == client_id)
        }) {
            return;
        }
        for existing in &mut self.pending_releases {
            if let PendingRelease::Up {
                client_id: cid,
                pointer_id: pid,
                x: ex,
                y: ey,
            } = existing
            {
                if *cid == client_id && *pid == pointer_id {
                    *ex = x;
                    *ey = y;
                    return;
                }
            }
        }
        self.pending_releases.push(PendingRelease::Up {
            client_id,
            pointer_id,
            x,
            y,
        });
        self.bump_recovery_episode();
    }

    fn record_pending_cancel_client(&mut self, client_id: u64) {
        if !self.pointers.iter().any(|p| p.client_id == client_id) {
            return;
        }
        if self.pending_releases.iter().any(|r| {
            matches!(r, PendingRelease::CancelClient { client_id: cid } if *cid == client_id)
        }) {
            return;
        }
        self.pending_releases.retain(|r| {
            !matches!(
                r,
                PendingRelease::Up { client_id: cid, .. } if *cid == client_id
            )
        });
        self.pending_releases
            .push(PendingRelease::CancelClient { client_id });
        self.bump_recovery_episode();
    }

    /// Flush an unconfirmed touch pointer left behind by an ambiguous failure
    /// whose immediate compensating cancel also failed. Keeping the uncertain
    /// pointer here (out of `self.pointers`) prevents subsequent clients'
    /// gestures from dispatching `ACTION_POINTER_DOWN` or failing scroll checks
    /// against an unconfirmed pointer, while still guaranteeing a compensating
    /// cancel runs before any new gesture on the same guest.
    fn resolve_pending_touch_recovery(&mut self) -> Result<(), (String, String)> {
        let Some(rec) = self.pending_touch_recovery.clone() else {
            return Ok(());
        };
        if self.slot.is_replaced() {
            self.clear_guest_touch_state();
            return Ok(());
        }
        let ts = self.next_ts_ns();
        let (action, down_time, pointers) = if self.pointers.is_empty() {
            (
                ACTION_CANCEL,
                rec.down_time_ns,
                vec![json!([rec.wire_id, rec.x, rec.y])],
            )
        } else {
            let mut ptrs = self.pointer_wire_list();
            let idx = ptrs.len() as i32;
            ptrs.push(json!([rec.wire_id, rec.x, rec.y]));
            (
                ACTION_POINTER_UP | (idx << ACTION_POINTER_INDEX_SHIFT),
                self.down_time_ns.unwrap_or(rec.down_time_ns),
                ptrs,
            )
        };
        match self.call_daemon(
            "panel_touch",
            json!({
                "action": action,
                "down_time": down_time,
                "event_time": ts,
                "pointers": pointers,
            }),
        ) {
            Ok(_) => {
                self.pending_touch_recovery = None;
                self.bump_recovery_episode();
                Ok(())
            }
            Err((c, m)) => {
                if self.slot.is_replaced() || is_guest_gone_code(&c) {
                    self.clear_guest_touch_state();
                }
                Err((c, m))
            }
        }
    }

    fn flush_pending_releases(&mut self) -> Result<(), (String, String)> {
        while let Some(release) = self.pending_releases.first().cloned() {
            if self.slot.is_replaced() {
                self.clear_guest_touch_state();
                return Ok(());
            }
            match release {
                PendingRelease::Up {
                    client_id,
                    pointer_id,
                    x,
                    y,
                } => {
                    let Some(pos) = self
                        .pointers
                        .iter()
                        .position(|p| p.client_id == client_id && p.pointer_id == pointer_id)
                    else {
                        self.pending_releases.remove(0);
                        self.bump_recovery_episode();
                        continue;
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
                    match self.send_touch_frame(action, down_time, ts) {
                        Ok(_) => {
                            self.pointers.remove(pos);
                            if self.pointers.is_empty() {
                                self.down_time_ns = None;
                            }
                            self.pending_releases.remove(0);
                            self.bump_recovery_episode();
                        }
                        Err((c, m)) => {
                            if self.slot.is_replaced() || is_guest_gone_code(&c) {
                                self.clear_guest_touch_state();
                            }
                            return Err((c, m));
                        }
                    }
                }
                PendingRelease::CancelClient { client_id } => {
                    if !self.pointers.iter().any(|p| p.client_id == client_id) {
                        self.pending_releases.remove(0);
                        self.bump_recovery_episode();
                        continue;
                    }
                    let only_this_client = self.pointers.iter().all(|p| p.client_id == client_id);
                    if only_this_client {
                        let ts = self.next_ts_ns();
                        let down_time = self.down_time_ns.unwrap_or(ts);
                        match self.send_touch_frame(ACTION_CANCEL, down_time, ts) {
                            Ok(_) => {
                                self.pointers.clear();
                                self.down_time_ns = None;
                                self.pending_releases.remove(0);
                                self.bump_recovery_episode();
                            }
                            Err((c, m)) => {
                                if self.slot.is_replaced() || is_guest_gone_code(&c) {
                                    self.clear_guest_touch_state();
                                }
                                return Err((c, m));
                            }
                        }
                    } else {
                        while let Some(pos) =
                            self.pointers.iter().position(|p| p.client_id == client_id)
                        {
                            let ts = self.next_ts_ns();
                            let down_time = self.down_time_ns.unwrap_or(ts);
                            let action = if self.pointers.len() == 1 {
                                ACTION_CANCEL
                            } else {
                                ACTION_POINTER_UP | ((pos as i32) << ACTION_POINTER_INDEX_SHIFT)
                            };
                            match self.send_touch_frame(action, down_time, ts) {
                                Ok(_) => {
                                    self.pointers.remove(pos);
                                    if self.pointers.is_empty() {
                                        self.down_time_ns = None;
                                    }
                                    self.bump_recovery_episode();
                                }
                                Err((c, m)) => {
                                    if self.slot.is_replaced() || is_guest_gone_code(&c) {
                                        self.clear_guest_touch_state();
                                    }
                                    return Err((c, m));
                                }
                            }
                        }
                        self.pending_releases.remove(0);
                        self.bump_recovery_episode();
                    }
                }
            }
        }
        Ok(())
    }

    fn resolve_pending_touch_recovery_and_releases(&mut self) -> Result<(), (String, String)> {
        self.resolve_pending_touch_recovery()?;
        self.flush_pending_releases()
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
                if let Err((c, m)) = self.resolve_pending_touch_recovery_and_releases() {
                    return err_json(&c, &m);
                }
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
                let Some(wire_id) = self.alloc_wire_id() else {
                    return err_json("TOO_MANY_POINTERS", "too many simultaneous active pointers");
                };
                self.pointers.push(ActivePointer {
                    client_id,
                    pointer_id,
                    wire_id,
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
                match self.send_touch_frame_outcome(action, down_time, ts) {
                    Ok(_) => json!({ "ok": true, "t": "touch", "phase": "down" }),
                    Err((c, m, may_have_fired)) => {
                        let popped = self.pointers.pop();
                        if self.pointers.is_empty() {
                            self.down_time_ns = None;
                        }
                        if self.slot.is_replaced() || is_guest_gone_code(&c) {
                            self.clear_guest_touch_state();
                        } else if may_have_fired {
                            if let Some(ptr) = popped {
                                // The guest may already have received the down;
                                // keep the uncertain pointer out of `self.pointers`
                                // and attempt immediate cancellation now. If that
                                // fails, `pending_touch_recovery` will flush it
                                // before the next touch/scroll or via bounded
                                // scheduled retries.
                                self.pending_touch_recovery = Some(PendingTouchRecovery {
                                    wire_id: ptr.wire_id,
                                    x: ptr.x,
                                    y: ptr.y,
                                    down_time_ns: down_time,
                                });
                                let _ = self.resolve_pending_touch_recovery();
                            }
                        }
                        err_json(&c, &m)
                    }
                }
            }
            TouchPhase::Move => {
                let (Some(x), Some(y)) = (x, y) else {
                    return err_json("INVALID_ARGUMENT", "touch move requires x and y");
                };
                if let Err((c, m)) = self.resolve_pending_touch_recovery_and_releases() {
                    return err_json(&c, &m);
                }
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
                    Err((c, m)) => {
                        if self.slot.is_replaced() || is_guest_gone_code(&c) {
                            self.clear_guest_touch_state();
                        }
                        err_json(&c, &m)
                    }
                }
            }
            TouchPhase::Up => {
                let (Some(x), Some(y)) = (x, y) else {
                    return err_json("INVALID_ARGUMENT", "touch up requires x and y");
                };
                if self.slot.is_replaced() {
                    self.clear_guest_touch_state();
                    return err_json("SESSION_REPLACED", "session was replaced by a newer guest");
                }
                let had_pointer = self
                    .pointers
                    .iter()
                    .any(|p| p.client_id == client_id && p.pointer_id == pointer_id);
                if had_pointer {
                    self.record_pending_up(client_id, pointer_id, x, y);
                }
                if let Err((c, m)) = self.resolve_pending_touch_recovery_and_releases() {
                    return err_json(&c, &m);
                }
                if !had_pointer {
                    return json!({ "ok": true, "t": "touch", "ignored": "pointer_not_down" });
                }
                json!({ "ok": true, "t": "touch", "phase": "up" })
            }
            TouchPhase::Cancel => {
                if self.slot.is_replaced() {
                    self.clear_guest_touch_state();
                    return err_json("SESSION_REPLACED", "session was replaced by a newer guest");
                }
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
                self.record_pending_cancel_client(client_id);
                if let Err((c, m)) = self.resolve_pending_touch_recovery_and_releases() {
                    return err_json(&c, &m);
                }
                json!({ "ok": true, "t": "touch", "phase": "cancel" })
            }
        }
    }

    fn handle_scroll(&mut self, entry: &Entry, x: f32, y: f32, dx: f32, dy: f32) -> Value {
        if let Err((c, m)) = self.resolve_pending_touch_recovery_and_releases() {
            return err_json(&c, &m);
        }
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
        if (ex - sx).abs() < 1.0 && (ey - sy).abs() < 1.0 {
            return json!({ "ok": true, "t": "scroll", "ignored": "clamped_zero" });
        }

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

        let mut sent_down = false;
        for (action, px, py, event_time) in frames {
            self.last_event_time_ns = event_time;
            match self.call_daemon_outcome(
                "panel_touch",
                json!({
                    "action": action,
                    "down_time": down_time,
                    "event_time": event_time,
                    "pointers": [[0, px, py]],
                }),
            ) {
                Ok(_) => {
                    if action == ACTION_DOWN {
                        sent_down = true;
                    }
                }
                Err((c, m, may_have_fired)) => {
                    if (sent_down || (action == ACTION_DOWN && may_have_fired))
                        && !self.slot.is_replaced()
                        && !is_guest_gone_code(&c)
                    {
                        self.pending_touch_recovery = Some(PendingTouchRecovery {
                            wire_id: 0,
                            x: px,
                            y: py,
                            down_time_ns: down_time,
                        });
                        let _ = self.resolve_pending_touch_recovery();
                    }
                    return err_json(&c, &m);
                }
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
                let frame_seq = res.get("frame_seq").and_then(Value::as_u64);
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
                let mut reply = json!({
                    "ok": true,
                    "t": "rotate",
                    "width": width,
                    "height": height,
                    "rotation": applied,
                });
                if let Some(seq) = frame_seq {
                    reply["frame_seq"] = json!(seq);
                }
                reply
            }
            Err((c, m)) => json!({
                "ok": false,
                "t": "rotate",
                "code": c,
                "error": m,
            }),
        }
    }

    fn cancel_client(&mut self, client_id: u64) {
        let entry = self.entry();
        if entry.watch_only {
            self.pointers.retain(|p| p.client_id != client_id);
            if self.pointers.is_empty() {
                self.down_time_ns = None;
            }
            self.pending_touch_recovery = None;
            self.pending_releases.clear();
            self.held_keys.retain(|&(cid, _)| cid != client_id);
            return;
        }
        self.record_pending_cancel_client(client_id);
        let _ = self.resolve_pending_touch_recovery_and_releases();
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
    let narrowed = f as f32;
    if !f.is_finite() || !narrowed.is_finite() {
        return Err(format!("field `{field}` must be finite"));
    }
    Ok(Some(narrowed))
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
            launch_token: None,
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
            launch_token: None,
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

    #[test]
    fn session_replacement_rejects_stale_input_client_without_retargeting() {
        let daemon_a = FakeDaemon::start("rep-a");
        let daemon_b = FakeDaemon::start("rep-b");
        let registry = scratch("rep-reg");
        let write_entry = |sock: &std::path::Path, token: &str| {
            let entry = crate::live_registry::LiveEntry {
                session_id: "s_rep".to_string(),
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
                serve_socket: Some(sock.display().to_string()),
                activity: Some("Main".to_string()),
                display: (1080, 2400),
                stream_socket: None,
                has_home: Some(false),
                launch_token: Some(token.to_string()),
            };
            crate::live_registry::LiveRegistry::at(registry.clone())
                .write(&entry)
                .unwrap();
        };

        write_entry(&daemon_a.socket, "tok-a");
        let hub = Hub::new(registry.clone());
        let old_client = hub.open_input("s_rep").expect("open old client");
        let (mut old_rx, old_tx) = ws_sink_pair();
        old_client.enqueue(r#"{"t":"text","s":"hi"}"#, Arc::clone(&old_tx));
        assert_eq!(read_server_ws_text(&mut old_rx)["ok"], json!(true));
        assert_eq!(daemon_a.calls.lock().unwrap().len(), 1);
        assert_eq!(
            daemon_a.calls.lock().unwrap()[0].1["expected_launch_token"],
            json!("tok-a")
        );

        // Replace the session id with a brand-new guest (token B, daemon B).
        write_entry(&daemon_b.socket, "tok-b");
        let _ = hub.list();

        // Stale client must be rejected with SESSION_REPLACED and never touch daemon B.
        old_client.enqueue(r#"{"t":"text","s":"stale"}"#, Arc::clone(&old_tx));
        let rejected = read_server_ws_text(&mut old_rx);
        assert_eq!(rejected["ok"], json!(false));
        assert_eq!(rejected["code"], json!("SESSION_REPLACED"));
        assert_eq!(daemon_b.calls.lock().unwrap().len(), 0);

        // A freshly opened client targets daemon B with token B.
        let new_client = hub.open_input("s_rep").expect("open new client");
        let (mut new_rx, new_tx) = ws_sink_pair();
        new_client.enqueue(r#"{"t":"text","s":"fresh"}"#, Arc::clone(&new_tx));
        assert_eq!(read_server_ws_text(&mut new_rx)["ok"], json!(true));
        assert_eq!(daemon_b.calls.lock().unwrap().len(), 1);
        assert_eq!(
            daemon_b.calls.lock().unwrap()[0].1["expected_launch_token"],
            json!("tok-b")
        );

        drop(old_client);
        drop(new_client);
        let _ = std::fs::remove_dir_all(&registry);
    }

    #[test]
    fn two_clients_with_same_pointer_id_get_distinct_wire_ids() {
        let daemon = FakeDaemon::start("wid-d");
        let registry = scratch("wid-reg");
        publish_input_entry(&registry, "s_wid", Some(&daemon.socket), None, false);
        let hub = Hub::new(registry.clone());

        let c1 = hub.open_input("s_wid").expect("c1");
        let c2 = hub.open_input("s_wid").expect("c2");
        let (mut rx1, tx1) = ws_sink_pair();
        let (mut rx2, tx2) = ws_sink_pair();

        c1.enqueue(
            r#"{"t":"touch","phase":"down","id":1,"x":100,"y":200}"#,
            Arc::clone(&tx1),
        );
        assert_eq!(read_server_ws_text(&mut rx1)["ok"], json!(true));
        c2.enqueue(
            r#"{"t":"touch","phase":"down","id":1,"x":300,"y":400}"#,
            Arc::clone(&tx2),
        );
        assert_eq!(read_server_ws_text(&mut rx2)["ok"], json!(true));

        let calls = daemon.calls.lock().unwrap();
        assert_eq!(calls.len(), 2);
        let ptrs = calls[1].1["pointers"].as_array().expect("pointers array");
        assert_eq!(ptrs.len(), 2);
        assert_ne!(
            ptrs[0][0], ptrs[1][0],
            "two clients using pointer_id=1 must get distinct Android wire pointer IDs"
        );
        drop(calls);

        drop(c1);
        drop(c2);
        let _ = std::fs::remove_dir_all(&registry);
    }

    #[test]
    fn post_write_eof_does_not_replay_command() {
        let root = scratch("eof-d");
        let socket = root.join("serve.sock");
        let listener = UnixListener::bind(&socket).expect("bind eof daemon");
        let received = Arc::new(AtomicUsize::new(0));
        let received_t = Arc::clone(&received);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { break };
                let mut reader = BufReader::new(stream);
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) > 0 {
                    received_t.fetch_add(1, Ordering::SeqCst);
                }
                // Close immediately after reading the request without replying.
            }
        });

        let registry = scratch("eof-reg");
        publish_input_entry(&registry, "s_eof", Some(&socket), None, false);
        let hub = Hub::new(registry.clone());
        let client = hub.open_input("s_eof").expect("open");
        let (mut rx, tx) = ws_sink_pair();

        client.enqueue(r#"{"t":"text","s":"once"}"#, Arc::clone(&tx));
        let reply = read_server_ws_text(&mut rx);
        assert_eq!(reply["ok"], json!(false));
        assert_eq!(reply["code"], json!("EOF"));
        assert_eq!(
            received.load(Ordering::SeqCst),
            1,
            "post-write EOF must not replay non-idempotent input command"
        );

        drop(client);
        let _ = std::fs::remove_dir_all(&registry);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn input_queue_coalesces_moves_and_bounds_backlog() {
        let root = scratch("qcap-d");
        let socket = root.join("serve.sock");
        let listener = UnixListener::bind(&socket).expect("bind slow daemon");
        let received_first = Arc::new((Mutex::new(false), Condvar::new()));
        let received_first_t = Arc::clone(&received_first);
        let gate = Arc::new((Mutex::new(false), Condvar::new()));
        let gate_t = Arc::clone(&gate);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                let received_c = Arc::clone(&received_first_t);
                let gate_c = Arc::clone(&gate_t);
                std::thread::spawn(move || {
                    let mut reader = BufReader::new(stream.try_clone().unwrap());
                    let mut line = String::new();
                    while reader.read_line(&mut line).unwrap_or(0) > 0 {
                        let req: Value = serde_json::from_str(&line).unwrap();
                        line.clear();
                        {
                            let (lk, cv) = &*received_c;
                            let mut seen = lk.lock().unwrap();
                            if !*seen {
                                *seen = true;
                                cv.notify_all();
                            }
                        }
                        let (lk, cv) = &*gate_c;
                        let mut open = lk.lock().unwrap();
                        while !*open {
                            open = cv.wait(open).unwrap();
                        }
                        drop(open);
                        let resp = json!({
                            "id": req["id"],
                            "envelope": { "ok": true, "result": { "ok": true } },
                        });
                        let _ = stream.write_all(format!("{resp}\n").as_bytes());
                        let _ = stream.flush();
                    }
                });
            }
        });

        let registry = scratch("qcap-reg");
        publish_input_entry(&registry, "s_qcap", Some(&socket), None, false);
        let hub = Hub::new(registry.clone());
        let client = hub.open_input("s_qcap").expect("open");
        let (mut rx, tx) = ws_sink_pair();

        // First command blocks in the slow daemon; wait until the fake daemon
        // has actually read it before enqueueing the coalescing moves and fill.
        assert!(client.enqueue(
            r#"{"t":"touch","phase":"down","id":1,"x":10,"y":10}"#,
            Arc::clone(&tx),
        ));
        {
            let (lk, cv) = &*received_first;
            let guard = lk.lock().unwrap();
            let (guard, _) = cv
                .wait_timeout_while(guard, Duration::from_secs(5), |seen| !*seen)
                .unwrap();
            assert!(*guard, "daemon never read first command");
        }

        // Two consecutive moves for the same pointer coalesce, replying immediately
        // to the superseded move.
        assert!(client.enqueue(
            r#"{"t":"touch","phase":"move","id":1,"x":20,"y":20}"#,
            Arc::clone(&tx),
        ));
        assert!(client.enqueue(
            r#"{"t":"touch","phase":"move","id":1,"x":30,"y":30}"#,
            Arc::clone(&tx),
        ));
        let coalesced = read_server_ws_text(&mut rx);
        assert_eq!(coalesced["ok"], json!(true));
        assert_eq!(coalesced["coalesced"], json!(true));

        // Fill up to INPUT_QUEUE_NORMAL_LIMIT (1 slot already holds the coalesced move).
        for _ in 1..INPUT_QUEUE_NORMAL_LIMIT {
            assert!(client.enqueue(r#"{"t":"text","s":"x"}"#, Arc::clone(&tx)));
        }
        // Next normal command must be rejected immediately with QUEUE_FULL.
        assert!(client.enqueue(r#"{"t":"text","s":"overflow"}"#, Arc::clone(&tx)));
        let full = read_server_ws_text(&mut rx);
        assert_eq!(full["ok"], json!(false));
        assert_eq!(full["code"], json!("QUEUE_FULL"));

        // Release daemon and drop client.
        {
            let (lk, cv) = &*gate;
            *lk.lock().unwrap() = true;
            cv.notify_all();
        }
        drop(client);
        let _ = std::fs::remove_dir_all(&registry);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn touch_move_does_not_coalesce_across_intervening_command() {
        let root = scratch("nocoal-d");
        let socket = root.join("serve.sock");
        let listener = UnixListener::bind(&socket).expect("bind daemon");
        let received_first = Arc::new((Mutex::new(false), Condvar::new()));
        let received_first_t = Arc::clone(&received_first);
        let gate = Arc::new((Mutex::new(false), Condvar::new()));
        let gate_t = Arc::clone(&gate);
        let log: Arc<Mutex<Vec<(String, Value)>>> = Arc::new(Mutex::new(Vec::new()));
        let log_t = Arc::clone(&log);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                let received_c = Arc::clone(&received_first_t);
                let gate_c = Arc::clone(&gate_t);
                let log_c = Arc::clone(&log_t);
                std::thread::spawn(move || {
                    let mut reader = BufReader::new(stream.try_clone().unwrap());
                    let mut line = String::new();
                    while reader.read_line(&mut line).unwrap_or(0) > 0 {
                        let req: Value = serde_json::from_str(&line).unwrap();
                        line.clear();
                        log_c.lock().unwrap().push((
                            req["tool"].as_str().unwrap_or("").to_string(),
                            req["args"].clone(),
                        ));
                        {
                            let (lk, cv) = &*received_c;
                            let mut seen = lk.lock().unwrap();
                            if !*seen {
                                *seen = true;
                                cv.notify_all();
                            }
                        }
                        let (lk, cv) = &*gate_c;
                        let mut open = lk.lock().unwrap();
                        while !*open {
                            open = cv.wait(open).unwrap();
                        }
                        drop(open);
                        let resp = json!({
                            "id": req["id"],
                            "envelope": {
                                "ok": true,
                                "result": { "ok": true, "width": 2400, "height": 1080, "rotation": 1 }
                            },
                        });
                        let _ = stream.write_all(format!("{resp}\n").as_bytes());
                        let _ = stream.flush();
                    }
                });
            }
        });

        let registry = scratch("nocoal-reg");
        publish_input_entry(&registry, "s_nc", Some(&socket), None, false);
        let hub = Hub::new(registry.clone());
        let client = hub.open_input("s_nc").expect("open");
        let (mut rx, tx) = ws_sink_pair();

        // Block the worker on the initial down.
        assert!(client.enqueue(
            r#"{"t":"touch","phase":"down","id":1,"x":10,"y":10}"#,
            Arc::clone(&tx),
        ));
        {
            let (lk, cv) = &*received_first;
            let guard = lk.lock().unwrap();
            let (guard, _) = cv
                .wait_timeout_while(guard, Duration::from_secs(5), |seen| !*seen)
                .unwrap();
            assert!(*guard);
        }

        // Queue move(20,20), then rotate, then move(80,80). Because rotate sits
        // between the two moves, they MUST NOT coalesce or reorder.
        assert!(client.enqueue(
            r#"{"t":"touch","phase":"move","id":1,"x":20,"y":20}"#,
            Arc::clone(&tx),
        ));
        assert!(client.enqueue(r#"{"t":"rotate","rotation":1}"#, Arc::clone(&tx)));
        assert!(client.enqueue(
            r#"{"t":"touch","phase":"move","id":1,"x":80,"y":80}"#,
            Arc::clone(&tx),
        ));

        {
            let (lk, cv) = &*gate;
            *lk.lock().unwrap() = true;
            cv.notify_all();
        }
        // Read all 4 replies (down, move1, rotate, move2 — none coalesced).
        for _ in 0..4 {
            let reply = read_server_ws_text(&mut rx);
            assert_eq!(reply["ok"], json!(true), "{reply}");
            assert!(reply.get("coalesced").is_none(), "unexpected coalescing: {reply}");
        }
        let recorded = log.lock().unwrap().clone();
        assert_eq!(recorded.len(), 4);
        assert_eq!(recorded[1].0, "panel_touch");
        assert_eq!(recorded[1].1["pointers"][0][1], json!(20.0));
        assert_eq!(recorded[2].0, "panel_rotate");
        assert_eq!(recorded[3].0, "panel_touch");
        assert_eq!(recorded[3].1["pointers"][0][1], json!(80.0));

        drop(client);
        let _ = std::fs::remove_dir_all(&registry);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn terminal_release_on_full_queue_triggers_client_cleanup() {
        let root = scratch("qterm-d");
        let socket = root.join("serve.sock");
        let listener = UnixListener::bind(&socket).expect("bind slow daemon");
        let received_first = Arc::new((Mutex::new(false), Condvar::new()));
        let received_first_t = Arc::clone(&received_first);
        let gate = Arc::new((Mutex::new(false), Condvar::new()));
        let gate_t = Arc::clone(&gate);
        let actions: Arc<Mutex<Vec<i64>>> = Arc::new(Mutex::new(Vec::new()));
        let actions_t = Arc::clone(&actions);
        let cancelled = Arc::new((Mutex::new(false), Condvar::new()));
        let cancelled_t = Arc::clone(&cancelled);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                let received_c = Arc::clone(&received_first_t);
                let gate_c = Arc::clone(&gate_t);
                let actions_c = Arc::clone(&actions_t);
                let cancelled_c = Arc::clone(&cancelled_t);
                std::thread::spawn(move || {
                    let mut reader = BufReader::new(stream.try_clone().unwrap());
                    let mut line = String::new();
                    while reader.read_line(&mut line).unwrap_or(0) > 0 {
                        let req: Value = serde_json::from_str(&line).unwrap();
                        line.clear();
                        if req["tool"] == "panel_touch" {
                            if let Some(a) = req["args"]["action"].as_i64() {
                                actions_c.lock().unwrap().push(a);
                                if a == i64::from(ACTION_CANCEL) {
                                    let (lk, cv) = &*cancelled_c;
                                    *lk.lock().unwrap() = true;
                                    cv.notify_all();
                                }
                            }
                        }
                        {
                            let (lk, cv) = &*received_c;
                            let mut seen = lk.lock().unwrap();
                            if !*seen {
                                *seen = true;
                                cv.notify_all();
                            }
                        }
                        let (lk, cv) = &*gate_c;
                        let mut open = lk.lock().unwrap();
                        while !*open {
                            open = cv.wait(open).unwrap();
                        }
                        drop(open);
                        let resp = json!({
                            "id": req["id"],
                            "envelope": { "ok": true, "result": { "ok": true } },
                        });
                        let _ = stream.write_all(format!("{resp}\n").as_bytes());
                        let _ = stream.flush();
                    }
                });
            }
        });

        let registry = scratch("qterm-reg");
        publish_input_entry(&registry, "s_qterm", Some(&socket), None, false);
        let hub = Hub::new(registry.clone());
        let client = hub.open_input("s_qterm").expect("open");
        let (mut rx, tx) = ws_sink_pair();

        // Put one pointer down and wait for daemon to read it (still gated).
        assert!(client.enqueue(
            r#"{"t":"touch","phase":"down","id":1,"x":10,"y":10}"#,
            Arc::clone(&tx),
        ));
        {
            let (lk, cv) = &*received_first;
            let guard = lk.lock().unwrap();
            let (guard, _) = cv
                .wait_timeout_while(guard, Duration::from_secs(5), |seen| !*seen)
                .unwrap();
            assert!(*guard);
        }

        // Fill normal slots (112) + reserved terminal slots (16) = 128 total.
        for _ in 0..INPUT_QUEUE_NORMAL_LIMIT {
            assert!(client.enqueue(r#"{"t":"text","s":"x"}"#, Arc::clone(&tx)));
        }
        for i in 0..(INPUT_QUEUE_LIMIT - INPUT_QUEUE_NORMAL_LIMIT) {
            assert!(client.enqueue(
                &format!(r#"{{"t":"key","code":{},"phase":"up"}}"#, 29 + i),
                Arc::clone(&tx),
            ));
        }

        // 129th command is a terminal release: must terminate client (return false),
        // reply QUEUE_FULL with disconnected:true, and queue priority cancel cleanup.
        let keep_open = client.enqueue(
            r#"{"t":"touch","phase":"up","id":1,"x":10,"y":10}"#,
            Arc::clone(&tx),
        );
        assert!(!keep_open, "terminal QUEUE_FULL must close client");
        let full = read_server_ws_text(&mut rx);
        assert_eq!(full["ok"], json!(false));
        assert_eq!(full["code"], json!("QUEUE_FULL"));
        assert_eq!(full["disconnected"], json!(true));

        // Release daemon gate and verify ACTION_CANCEL runs WITHOUT dropping `client` first.
        {
            let (lk, cv) = &*gate;
            *lk.lock().unwrap() = true;
            cv.notify_all();
        }
        {
            let (lk, cv) = &*cancelled;
            let guard = lk.lock().unwrap();
            let (guard, _) = cv
                .wait_timeout_while(guard, Duration::from_secs(5), |done| !*done)
                .unwrap();
            assert!(
                *guard,
                "active pointer must be cancelled immediately when release hits QUEUE_FULL (saw {:?})",
                *actions.lock().unwrap()
            );
        }

        drop(client);
        let _ = std::fs::remove_dir_all(&registry);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn open_input_checked_rejects_stale_guest_gen_and_marks_watcher_replaced() {
        let root = scratch("ggen-d");
        let stream_a = root.join("stream-a.sock");
        let stream_b = root.join("stream-b.sock");
        let _la = UnixListener::bind(&stream_a).expect("bind stream a");
        let _lb = UnixListener::bind(&stream_b).expect("bind stream b");

        let registry = scratch("ggen-reg");
        let _ = std::fs::create_dir_all(&registry);
        let path = registry.join("s_gen.json");
        let write_entry = |stream: &std::path::Path, token: &str| {
            let doc = json!({
                "v": 1,
                "session_id": "s_gen",
                "daemon_pid": std::process::id(),
                "launcher_pid": std::process::id(),
                "started_ms": 1,
                "stream_socket": stream.to_string_lossy(),
                "serve_socket": stream.to_string_lossy(),
                "launch_token": token,
                "watch_only": false,
                "display": {"w": 1080, "h": 2400},
            });
            std::fs::write(&path, doc.to_string()).unwrap();
        };

        write_entry(&stream_a, "tok-a");
        let hub = Hub::new(registry.clone());
        let watcher = hub.watch("s_gen").expect("watch guest A");
        let gen_a = watcher.guest_gen();
        assert!(gen_a > 0);
        assert!(!watcher.is_replaced());

        // Replace guest A with guest B under the same session_id while watcher is open.
        write_entry(&stream_b, "tok-b");
        let list = hub.list();
        assert_eq!(list.len(), 1);
        let gen_b = list[0].guest_gen;
        assert_ne!(gen_a, gen_b, "guest replacement must advance guest_gen");
        assert!(
            watcher.is_replaced(),
            "old watcher must be marked replaced after guest swap"
        );

        // Opening input with guest A's stale generation must fail with Replaced.
        assert_eq!(
            hub.open_input_checked("s_gen", gen_a).err(),
            Some(OpenInputError::Replaced)
        );
        // Opening input with guest B's generation succeeds.
        let client_b = hub
            .open_input_checked("s_gen", gen_b)
            .expect("open guest B");
        drop(client_b);
        drop(watcher);
        let _ = std::fs::remove_dir_all(&registry);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn scroll_cancels_on_ambiguous_action_down_eof() {
        let root = scratch("scrcan-d");
        let socket = root.join("serve.sock");
        let listener = UnixListener::bind(&socket).expect("bind eof daemon");
        let seen_actions: Arc<Mutex<Vec<i64>>> = Arc::new(Mutex::new(Vec::new()));
        let seen_t = Arc::clone(&seen_actions);
        std::thread::spawn(move || {
            let mut first = true;
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 {
                    continue;
                }
                let req: Value = serde_json::from_str(&line).unwrap();
                if let Some(a) = req["args"]["action"].as_i64() {
                    seen_t.lock().unwrap().push(a);
                }
                if first {
                    first = false;
                    // Drop connection right after reading ACTION_DOWN (ambiguous EOF).
                    continue;
                }
                let resp = json!({
                    "id": req["id"],
                    "envelope": { "ok": true, "result": { "ok": true } },
                });
                let _ = stream.write_all(format!("{resp}\n").as_bytes());
                let _ = stream.flush();
            }
        });

        let registry = scratch("scrcan-reg");
        publish_input_entry(&registry, "s_scrcan", Some(&socket), None, false);
        let hub = Hub::new(registry.clone());
        let client = hub.open_input("s_scrcan").expect("open");
        let (mut rx, tx) = ws_sink_pair();

        client.enqueue(
            r#"{"t":"scroll","x":540,"y":1200,"dy":300}"#,
            Arc::clone(&tx),
        );
        let reply = read_server_ws_text(&mut rx);
        assert_eq!(reply["ok"], json!(false));
        assert_eq!(reply["code"], json!("EOF"));

        let actions = seen_actions.lock().unwrap().clone();
        assert_eq!(
            actions,
            vec![i64::from(ACTION_DOWN), i64::from(ACTION_CANCEL)],
            "ambiguous ACTION_DOWN EOF must emit ACTION_CANCEL without replaying ACTION_DOWN"
        );

        drop(client);
        let _ = std::fs::remove_dir_all(&registry);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn touch_down_cancels_on_ambiguous_eof_and_retains_pointer_on_failed_cancel() {
        let root = scratch("tdcan");
        let socket = root.join("serve.sock");
        let listener = UnixListener::bind(&socket).expect("bind fake daemon");
        let seen_actions = Arc::new(Mutex::new(Vec::<i64>::new()));
        let seen_t = Arc::clone(&seen_actions);
        let cancel_done = Arc::new((Mutex::new(false), Condvar::new()));
        let cancel_done_t = Arc::clone(&cancel_done);
        let _daemon = std::thread::spawn(move || {
            let mut req_idx = 0usize;
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 {
                    continue;
                }
                let req: Value = serde_json::from_str(&line).unwrap();
                if let Some(a) = req["args"]["action"].as_i64() {
                    seen_t.lock().unwrap().push(a);
                }
                req_idx += 1;
                if req_idx <= 2 {
                    // Request 1 (ACTION_DOWN): drop right after reading (ambiguous EOF).
                    // Request 2 (immediate ACTION_CANCEL): also drop so pointer must stay
                    // tracked for disconnect cleanup.
                    continue;
                }
                // Request 3 (disconnect cleanup ACTION_CANCEL): succeed.
                let resp = json!({
                    "id": req["id"],
                    "envelope": { "ok": true, "result": { "ok": true } },
                });
                let _ = stream.write_all(format!("{resp}\n").as_bytes());
                let _ = stream.flush();
                let (lock, cv) = &*cancel_done_t;
                *lock.lock().unwrap() = true;
                cv.notify_all();
                break;
            }
        });

        let registry = scratch("tdcan-reg");
        publish_input_entry(&registry, "s_tdcan", Some(&socket), None, false);
        let hub = Hub::new(registry.clone());
        let client = hub.open_input("s_tdcan").expect("open");
        let (mut rx, tx) = ws_sink_pair();

        client.enqueue(
            r#"{"t":"touch","phase":"down","id":1,"x":200,"y":400}"#,
            Arc::clone(&tx),
        );
        let reply = read_server_ws_text(&mut rx);
        assert_eq!(reply["ok"], json!(false));
        assert_eq!(reply["code"], json!("EOF"));

        // Dropping the client must emit disconnect cleanup ACTION_CANCEL because
        // the immediate cancel also failed.
        drop(client);
        let (lock, cv) = &*cancel_done;
        let guard = lock.lock().unwrap();
        let (guard, _) = cv
            .wait_timeout_while(guard, Duration::from_secs(3), |done| !*done)
            .unwrap();
        assert!(*guard, "expected disconnect cleanup ACTION_CANCEL");

        let actions = seen_actions.lock().unwrap().clone();
        assert_eq!(
            actions,
            vec![
                i64::from(ACTION_DOWN),
                i64::from(ACTION_CANCEL),
                i64::from(ACTION_CANCEL),
            ],
            "ambiguous touch down must attempt ACTION_CANCEL without replaying ACTION_DOWN and retain pointer if cancel fails"
        );

        let _ = std::fs::remove_dir_all(&registry);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn rotate_rejected_with_queue_full_includes_rotate_tag() {
        let root = scratch("rotqf");
        let socket = root.join("serve.sock");
        let listener = UnixListener::bind(&socket).expect("bind fake daemon");
        let received_first = Arc::new((Mutex::new(false), Condvar::new()));
        let release = Arc::new((Mutex::new(false), Condvar::new()));
        let rf_t = Arc::clone(&received_first);
        let rel_t = Arc::clone(&release);
        let _daemon = std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 {
                        break;
                    }
                    let req: Value = serde_json::from_str(&line).unwrap();
                    {
                        let (lock, cv) = &*rf_t;
                        let mut seen = lock.lock().unwrap();
                        if !*seen {
                            *seen = true;
                            cv.notify_all();
                        }
                    }
                    {
                        let (lock, cv) = &*rel_t;
                        let mut open = lock.lock().unwrap();
                        while !*open {
                            open = cv.wait(open).unwrap();
                        }
                    }
                    let resp = json!({
                        "id": req["id"],
                        "envelope": { "ok": true, "result": { "ok": true } },
                    });
                    if stream.write_all(format!("{resp}\n").as_bytes()).is_err() {
                        break;
                    }
                    let _ = stream.flush();
                }
            }
        });

        let registry = scratch("rotqf-reg");
        publish_input_entry(&registry, "s_rotqf", Some(&socket), None, false);
        let hub = Hub::new(registry.clone());
        let client = hub.open_input("s_rotqf").expect("open");
        let (mut rx, tx) = ws_sink_pair();

        // Block worker on first command.
        client.enqueue(r#"{"t":"text","s":"a"}"#, Arc::clone(&tx));
        {
            let (lock, cv) = &*received_first;
            let guard = lock.lock().unwrap();
            let (guard, _) = cv
                .wait_timeout_while(guard, Duration::from_secs(3), |seen| !*seen)
                .unwrap();
            assert!(*guard, "daemon did not receive initial command");
        }
        // Fill normal queue capacity.
        for _ in 0..INPUT_QUEUE_NORMAL_LIMIT {
            assert!(client.enqueue(r#"{"t":"text","s":"x"}"#, Arc::clone(&tx)));
        }
        // Next rotate hits QUEUE_FULL and must carry `"t": "rotate"` so the page clears pendingRotate.
        assert!(client.enqueue(r#"{"t":"rotate","rotation":1}"#, Arc::clone(&tx)));
        let reply = read_server_ws_text(&mut rx);
        assert_eq!(reply["ok"], json!(false));
        assert_eq!(reply["code"], json!("QUEUE_FULL"));
        assert_eq!(reply["t"], json!("rotate"));

        {
            let (lock, cv) = &*release;
            *lock.lock().unwrap() = true;
            cv.notify_all();
        }
        drop(client);
        let _ = std::fs::remove_dir_all(&registry);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn finite_opt_f32_rejects_f32_overflow() {
        let huge = json!(1e308_f64);
        assert!(
            finite_opt_f32(Some(&huge), "x").is_err(),
            "1e308 overflows f32 to inf and must be rejected"
        );
        assert_eq!(finite_opt_f32(Some(&json!(42.5)), "x").unwrap(), Some(42.5));
    }

    #[test]
    fn failed_touch_down_recovery_flushes_before_next_client_down_and_scroll() {
        let root = scratch("tdrec");
        let socket = root.join("serve.sock");
        let listener = UnixListener::bind(&socket).expect("bind fake daemon");
        let seen: Arc<Mutex<Vec<(i64, Value)>>> = Arc::new(Mutex::new(Vec::new()));
        let seen_t = Arc::clone(&seen);
        let _daemon = std::thread::spawn(move || {
            let mut req_idx = 0usize;
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 {
                        break;
                    }
                    let req: Value = serde_json::from_str(&line).unwrap();
                    if let Some(a) = req["args"]["action"].as_i64() {
                        seen_t
                            .lock()
                            .unwrap()
                            .push((a, req["args"]["pointers"].clone()));
                    }
                    req_idx += 1;
                    // Drop connections on:
                    // - req 1 (Client A ACTION_DOWN) & req 2 (immediate ACTION_CANCEL)
                    // - req 3 (Client B recovery ACTION_CANCEL retry #1)
                    // - req 7 (Client A second ACTION_DOWN) & req 8 (immediate ACTION_CANCEL)
                    if matches!(req_idx, 1 | 2 | 3 | 7 | 8) {
                        break;
                    }
                    let resp = json!({
                        "id": req["id"],
                        "envelope": { "ok": true, "result": { "ok": true } },
                    });
                    if stream.write_all(format!("{resp}\n").as_bytes()).is_err() {
                        break;
                    }
                    let _ = stream.flush();
                }
            }
        });

        let registry = scratch("tdrec-reg");
        publish_input_entry(&registry, "s_tdrec", Some(&socket), None, false);
        let hub = Hub::new(registry.clone());
        let client_a = hub.open_input("s_tdrec").expect("open A");
        let client_b = hub.open_input("s_tdrec").expect("open B");
        // This test asserts exact foreground `req_idx` ordering; keep the
        // background recovery timer disabled so it cannot consume a daemon slot
        // between foreground steps.
        hub.set_scheduled_recovery_enabled("s_tdrec", false);
        let (mut rx_a, tx_a) = ws_sink_pair();
        let (mut rx_b, tx_b) = ws_sink_pair();

        // 1. Client A touch down fails ambiguously (req 1) and immediate cancel also fails (req 2).
        client_a.enqueue(
            r#"{"t":"touch","phase":"down","id":1,"x":200,"y":400}"#,
            Arc::clone(&tx_a),
        );
        let reply_a = read_server_ws_text(&mut rx_a);
        assert_eq!(reply_a["ok"], json!(false));
        assert_eq!(reply_a["code"], json!("EOF"));

        // 2. While Client A stays connected, Client B touch down attempts recovery first (req 3 -> EOF)
        //    and MUST NOT emit ACTION_POINTER_DOWN.
        client_b.enqueue(
            r#"{"t":"touch","phase":"down","id":1,"x":500,"y":600}"#,
            Arc::clone(&tx_b),
        );
        let reply_b1 = read_server_ws_text(&mut rx_b);
        assert_eq!(reply_b1["ok"], json!(false));
        assert_eq!(reply_b1["code"], json!("EOF"));

        // 3. Daemon recovers: Client B touch down flushes ACTION_CANCEL (req 4) and then dispatches
        //    a fresh primary ACTION_DOWN (req 5) with ONLY Client B's pointer.
        client_b.enqueue(
            r#"{"t":"touch","phase":"down","id":1,"x":500,"y":600}"#,
            Arc::clone(&tx_b),
        );
        let reply_b2 = read_server_ws_text(&mut rx_b);
        assert_eq!(reply_b2["ok"], json!(true), "{reply_b2}");
        assert_eq!(reply_b2["phase"], json!("down"));

        // Release Client B's pointer cleanly (req 6 = ACTION_UP).
        client_b.enqueue(
            r#"{"t":"touch","phase":"up","id":1,"x":500,"y":600}"#,
            Arc::clone(&tx_b),
        );
        let reply_b_up = read_server_ws_text(&mut rx_b);
        assert_eq!(reply_b_up["ok"], json!(true), "{reply_b_up}");

        // 4. Trigger another failed touch down + failed immediate cancel on Client A (req 7, 8),
        //    then verify Client B scroll flushes recovery ACTION_CANCEL (req 9) and succeeds
        //    instead of failing with INVALID_STATE ("touch pointer is active").
        client_a.enqueue(
            r#"{"t":"touch","phase":"down","id":2,"x":150,"y":350}"#,
            Arc::clone(&tx_a),
        );
        let reply_a2 = read_server_ws_text(&mut rx_a);
        assert_eq!(reply_a2["ok"], json!(false));
        assert_eq!(reply_a2["code"], json!("EOF"));

        client_b.enqueue(
            r#"{"t":"scroll","x":540,"y":1200,"dy":250}"#,
            Arc::clone(&tx_b),
        );
        let reply_scroll = read_server_ws_text(&mut rx_b);
        assert_eq!(
            reply_scroll["ok"],
            json!(true),
            "scroll after pending touch recovery must flush cancel and succeed: {reply_scroll}"
        );

        let recorded = seen.lock().unwrap().clone();
        assert_eq!(recorded[0].0, i64::from(ACTION_DOWN));
        assert_eq!(recorded[1].0, i64::from(ACTION_CANCEL));
        assert_eq!(recorded[2].0, i64::from(ACTION_CANCEL));
        assert_eq!(recorded[3].0, i64::from(ACTION_CANCEL));
        assert_eq!(recorded[3].1, json!([[0, 200.0, 400.0]]));
        assert_eq!(
            recorded[4].0,
            i64::from(ACTION_DOWN),
            "Client B must get a fresh primary ACTION_DOWN, never ACTION_POINTER_DOWN"
        );
        assert_eq!(recorded[4].1, json!([[0, 500.0, 600.0]]));
        assert_eq!(recorded[5].0, i64::from(ACTION_UP));
        assert_eq!(recorded[6].0, i64::from(ACTION_DOWN));
        assert_eq!(recorded[7].0, i64::from(ACTION_CANCEL));
        assert_eq!(recorded[8].0, i64::from(ACTION_CANCEL));
        assert_eq!(recorded[9].0, i64::from(ACTION_DOWN));

        drop(client_b);
        drop(client_a);
        let _ = std::fs::remove_dir_all(&registry);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn touch_down_timeout_envelope_is_treated_as_ambiguous_and_cancels() {
        let root = scratch("tdto");
        let socket = root.join("serve.sock");
        let listener = UnixListener::bind(&socket).expect("bind fake daemon");
        let seen_actions = Arc::new(Mutex::new(Vec::<i64>::new()));
        let seen_t = Arc::clone(&seen_actions);
        let _daemon = std::thread::spawn(move || {
            let mut req_idx = 0usize;
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 {
                        break;
                    }
                    let req: Value = serde_json::from_str(&line).unwrap();
                    if let Some(a) = req["args"]["action"].as_i64() {
                        seen_t.lock().unwrap().push(a);
                    }
                    req_idx += 1;
                    let resp = if req_idx == 1 {
                        // Structured TIMEOUT envelope after write: guest may have applied down.
                        json!({
                            "id": req["id"],
                            "envelope": {
                                "ok": false,
                                "error": {
                                    "code": "TIMEOUT",
                                    "message": "guest timed out after 1s",
                                },
                            },
                        })
                    } else {
                        json!({
                            "id": req["id"],
                            "envelope": { "ok": true, "result": { "ok": true } },
                        })
                    };
                    if stream.write_all(format!("{resp}\n").as_bytes()).is_err() {
                        break;
                    }
                    let _ = stream.flush();
                }
            }
        });

        let registry = scratch("tdto-reg");
        publish_input_entry(&registry, "s_tdto", Some(&socket), None, false);
        let hub = Hub::new(registry.clone());
        let client = hub.open_input("s_tdto").expect("open");
        let (mut rx, tx) = ws_sink_pair();

        client.enqueue(
            r#"{"t":"touch","phase":"down","id":1,"x":200,"y":400}"#,
            Arc::clone(&tx),
        );
        let reply = read_server_ws_text(&mut rx);
        assert_eq!(reply["ok"], json!(false));
        assert_eq!(reply["code"], json!("TIMEOUT"));

        let actions = seen_actions.lock().unwrap().clone();
        assert_eq!(
            actions,
            vec![i64::from(ACTION_DOWN), i64::from(ACTION_CANCEL)],
            "structured TIMEOUT reply after touch down write must trigger compensating ACTION_CANCEL"
        );

        drop(client);
        let _ = std::fs::remove_dir_all(&registry);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn disconnect_schedules_bounded_recovery_until_daemon_recovers() {
        let root = scratch("tdsched");
        let socket = root.join("serve.sock");
        let listener = UnixListener::bind(&socket).expect("bind fake daemon");
        let seen_actions = Arc::new(Mutex::new(Vec::<i64>::new()));
        let seen_t = Arc::clone(&seen_actions);
        let recovered = Arc::new((Mutex::new(false), Condvar::new()));
        let recovered_t = Arc::clone(&recovered);
        let _daemon = std::thread::spawn(move || {
            let mut req_idx = 0usize;
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 {
                    continue;
                }
                let req: Value = serde_json::from_str(&line).unwrap();
                if let Some(a) = req["args"]["action"].as_i64() {
                    seen_t.lock().unwrap().push(a);
                }
                req_idx += 1;
                // Drop req 1 (ACTION_DOWN), req 2 (immediate cancel), AND req 3 (disconnect cancel).
                // Req 4 is the scheduled post-disconnect backoff retry with 0 clients connected.
                if req_idx <= 3 {
                    continue;
                }
                let resp = json!({
                    "id": req["id"],
                    "envelope": { "ok": true, "result": { "ok": true } },
                });
                let _ = stream.write_all(format!("{resp}\n").as_bytes());
                let _ = stream.flush();
                let (lock, cv) = &*recovered_t;
                *lock.lock().unwrap() = true;
                cv.notify_all();
                break;
            }
        });

        let registry = scratch("tdsched-reg");
        publish_input_entry(&registry, "s_tdsched", Some(&socket), None, false);
        let hub = Hub::new(registry.clone());
        let client = hub.open_input("s_tdsched").expect("open");
        let (mut rx, tx) = ws_sink_pair();

        client.enqueue(
            r#"{"t":"touch","phase":"down","id":1,"x":200,"y":400}"#,
            Arc::clone(&tx),
        );
        let reply = read_server_ws_text(&mut rx);
        assert_eq!(reply["ok"], json!(false));
        assert_eq!(reply["code"], json!("EOF"));

        // Disconnect the only client; even after the disconnect attempt fails (req 3),
        // the worker must keep retrying with bounded backoff until req 4 succeeds.
        drop(client);
        let (lock, cv) = &*recovered;
        let guard = lock.lock().unwrap();
        let (guard, _) = cv
            .wait_timeout_while(guard, Duration::from_secs(3), |done| !*done)
            .unwrap();
        assert!(
            *guard,
            "scheduled post-disconnect recovery must retry and deliver ACTION_CANCEL"
        );

        let actions = seen_actions.lock().unwrap().clone();
        assert_eq!(
            actions,
            vec![
                i64::from(ACTION_DOWN),
                i64::from(ACTION_CANCEL),
                i64::from(ACTION_CANCEL),
                i64::from(ACTION_CANCEL),
            ],
            "must never replay ACTION_DOWN and must retry ACTION_CANCEL after disconnect"
        );

        let _ = std::fs::remove_dir_all(&registry);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn blocked_release_is_retained_and_applied_after_recovery_before_next_gesture() {
        let root = scratch("tdrel");
        let socket = root.join("serve.sock");
        let listener = UnixListener::bind(&socket).expect("bind fake daemon");
        let seen: Arc<Mutex<Vec<(i64, Value)>>> = Arc::new(Mutex::new(Vec::new()));
        let seen_t = Arc::clone(&seen);
        let _daemon = std::thread::spawn(move || {
            let mut req_idx = 0usize;
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 {
                        break;
                    }
                    let req: Value = serde_json::from_str(&line).unwrap();
                    if let Some(a) = req["args"]["action"].as_i64() {
                        seen_t
                            .lock()
                            .unwrap()
                            .push((a, req["args"]["pointers"].clone()));
                    }
                    req_idx += 1;
                    // req 1: Client A ACTION_DOWN -> ok
                    // req 2: Client B ACTION_POINTER_DOWN -> drop (ambiguous EOF)
                    // req 3: immediate ACTION_POINTER_UP for B -> drop
                    // req 4: Client A TouchPhase::Up triggers B recovery -> drop
                    // req 5+: daemon recovers -> B ACTION_POINTER_UP, A ACTION_UP, then C ACTION_DOWN
                    if matches!(req_idx, 2 | 3 | 4) {
                        break;
                    }
                    let resp = json!({
                        "id": req["id"],
                        "envelope": { "ok": true, "result": { "ok": true } },
                    });
                    if stream.write_all(format!("{resp}\n").as_bytes()).is_err() {
                        break;
                    }
                    let _ = stream.flush();
                }
            }
        });

        let registry = scratch("tdrel-reg");
        publish_input_entry(&registry, "s_tdrel", Some(&socket), None, false);
        let hub = Hub::new(registry.clone());
        let client_a = hub.open_input("s_tdrel").expect("open A");
        let client_b = hub.open_input("s_tdrel").expect("open B");
        let client_c = hub.open_input("s_tdrel").expect("open C");
        // This test asserts exact foreground `req_idx` ordering; keep the
        // background recovery timer disabled so it cannot consume a daemon slot
        // between foreground steps.
        hub.set_scheduled_recovery_enabled("s_tdrel", false);
        let (mut rx_a, tx_a) = ws_sink_pair();
        let (mut rx_b, tx_b) = ws_sink_pair();
        let (mut rx_c, tx_c) = ws_sink_pair();

        // 1. Client A confirmed down (req 1).
        client_a.enqueue(
            r#"{"t":"touch","phase":"down","id":1,"x":100,"y":200}"#,
            Arc::clone(&tx_a),
        );
        assert_eq!(read_server_ws_text(&mut rx_a)["ok"], json!(true));

        // 2. Client B ambiguous secondary down (req 2 drop) + failed immediate cancel (req 3 drop).
        client_b.enqueue(
            r#"{"t":"touch","phase":"down","id":1,"x":300,"y":400}"#,
            Arc::clone(&tx_b),
        );
        let reply_b = read_server_ws_text(&mut rx_b);
        assert_eq!(reply_b["ok"], json!(false));
        assert_eq!(reply_b["code"], json!("EOF"));

        // 3. Client A sends Up while B's recovery fails again (req 4 drop):
        //    A's release intent MUST be retained.
        client_a.enqueue(
            r#"{"t":"touch","phase":"up","id":1,"x":110,"y":210}"#,
            Arc::clone(&tx_a),
        );
        let reply_a_up = read_server_ws_text(&mut rx_a);
        assert_eq!(reply_a_up["ok"], json!(false));
        assert_eq!(reply_a_up["code"], json!("EOF"));

        // 4. Daemon recovers; Client C sends a fresh Down:
        //    Worker must first apply B's ACTION_POINTER_UP (req 5), then A's retained
        //    ACTION_UP (req 6), and only then C's fresh primary ACTION_DOWN (req 7).
        client_c.enqueue(
            r#"{"t":"touch","phase":"down","id":1,"x":500,"y":600}"#,
            Arc::clone(&tx_c),
        );
        let reply_c = read_server_ws_text(&mut rx_c);
        assert_eq!(reply_c["ok"], json!(true), "{reply_c}");
        assert_eq!(reply_c["phase"], json!("down"));

        let recorded = seen.lock().unwrap().clone();
        assert_eq!(recorded.len(), 7, "unexpected recorded frames: {recorded:?}");
        let ptr_down_1 = i64::from(ACTION_POINTER_DOWN | (1 << ACTION_POINTER_INDEX_SHIFT));
        let ptr_up_1 = i64::from(ACTION_POINTER_UP | (1 << ACTION_POINTER_INDEX_SHIFT));
        assert_eq!(recorded[0].0, i64::from(ACTION_DOWN));
        assert_eq!(recorded[1].0, ptr_down_1);
        assert_eq!(recorded[2].0, ptr_up_1);
        assert_eq!(recorded[3].0, ptr_up_1);
        assert_eq!(recorded[4].0, ptr_up_1);
        assert_eq!(recorded[5].0, i64::from(ACTION_UP));
        assert_eq!(recorded[5].1, json!([[0, 110.0, 210.0]]));
        assert_eq!(
            recorded[6].0,
            i64::from(ACTION_DOWN),
            "Client C must start a fresh primary ACTION_DOWN after retained A release"
        );
        assert_eq!(recorded[6].1, json!([[0, 500.0, 600.0]]));

        drop(client_c);
        drop(client_b);
        drop(client_a);
        let _ = std::fs::remove_dir_all(&registry);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn connected_browser_schedules_bounded_recovery_for_failed_up() {
        let root = scratch("tdconnup");
        let socket = root.join("serve.sock");
        let listener = UnixListener::bind(&socket).expect("bind fake daemon");
        let seen_actions = Arc::new(Mutex::new(Vec::<i64>::new()));
        let seen_t = Arc::clone(&seen_actions);
        let recovered = Arc::new((Mutex::new(false), Condvar::new()));
        let recovered_t = Arc::clone(&recovered);
        let _daemon = std::thread::spawn(move || {
            let mut req_idx = 0usize;
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 {
                        break;
                    }
                    let req: Value = serde_json::from_str(&line).unwrap();
                    if let Some(a) = req["args"]["action"].as_i64() {
                        seen_t.lock().unwrap().push(a);
                    }
                    req_idx += 1;
                    // req 1: ACTION_DOWN -> ok
                    // req 2: initial ACTION_UP -> drop (transient EOF)
                    // req 3: scheduled retry ACTION_UP while browser is STILL connected -> ok
                    if req_idx == 2 {
                        break;
                    }
                    let resp = json!({
                        "id": req["id"],
                        "envelope": { "ok": true, "result": { "ok": true } },
                    });
                    if stream.write_all(format!("{resp}\n").as_bytes()).is_err() {
                        break;
                    }
                    let _ = stream.flush();
                    if req_idx >= 3 {
                        let (lock, cv) = &*recovered_t;
                        *lock.lock().unwrap() = true;
                        cv.notify_all();
                        return;
                    }
                }
            }
        });

        let registry = scratch("tdconnup-reg");
        publish_input_entry(&registry, "s_tdconnup", Some(&socket), None, false);
        let hub = Hub::new(registry.clone());
        let client = hub.open_input("s_tdconnup").expect("open");
        let (mut rx, tx) = ws_sink_pair();

        // 1. Confirmed Down succeeds.
        client.enqueue(
            r#"{"t":"touch","phase":"down","id":1,"x":200,"y":400}"#,
            Arc::clone(&tx),
        );
        assert_eq!(read_server_ws_text(&mut rx)["ok"], json!(true));

        // 2. Direct Up fails on first send; browser stays connected and sends NO more commands.
        client.enqueue(
            r#"{"t":"touch","phase":"up","id":1,"x":205,"y":405}"#,
            Arc::clone(&tx),
        );
        let up_reply = read_server_ws_text(&mut rx);
        assert_eq!(up_reply["ok"], json!(false));
        assert_eq!(up_reply["code"], json!("EOF"));

        // 3. Without disconnecting or sending another command, scheduled recovery must retry
        //    and deliver the retained ACTION_UP automatically.
        let (lock, cv) = &*recovered;
        let guard = lock.lock().unwrap();
        let (guard, _) = cv
            .wait_timeout_while(guard, Duration::from_secs(3), |done| !*done)
            .unwrap();
        assert!(
            *guard,
            "connected browser with retained failed Up must auto-retry ACTION_UP"
        );

        let actions = seen_actions.lock().unwrap().clone();
        assert_eq!(
            actions,
            vec![
                i64::from(ACTION_DOWN),
                i64::from(ACTION_UP),
                i64::from(ACTION_UP),
            ],
            "failed direct Up must be retained and retried while browser remains connected"
        );

        drop(client);
        let _ = std::fs::remove_dir_all(&registry);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn disconnect_retains_confirmed_pointer_release_when_initial_cancel_fails() {
        let root = scratch("tddisc");
        let socket = root.join("serve.sock");
        let listener = UnixListener::bind(&socket).expect("bind fake daemon");
        let seen_actions = Arc::new(Mutex::new(Vec::<i64>::new()));
        let seen_t = Arc::clone(&seen_actions);
        let recovered = Arc::new((Mutex::new(false), Condvar::new()));
        let recovered_t = Arc::clone(&recovered);
        let _daemon = std::thread::spawn(move || {
            let mut req_idx = 0usize;
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 {
                        break;
                    }
                    let req: Value = serde_json::from_str(&line).unwrap();
                    if let Some(a) = req["args"]["action"].as_i64() {
                        seen_t.lock().unwrap().push(a);
                    }
                    req_idx += 1;
                    // req 1: ACTION_DOWN -> ok
                    // req 2: disconnect initial ACTION_CANCEL -> drop (transient failure)
                    // req 3: scheduled retry ACTION_CANCEL -> ok
                    if req_idx == 2 {
                        break;
                    }
                    let resp = json!({
                        "id": req["id"],
                        "envelope": { "ok": true, "result": { "ok": true } },
                    });
                    if stream.write_all(format!("{resp}\n").as_bytes()).is_err() {
                        break;
                    }
                    let _ = stream.flush();
                    if req_idx >= 3 {
                        let (lock, cv) = &*recovered_t;
                        *lock.lock().unwrap() = true;
                        cv.notify_all();
                        return;
                    }
                }
            }
        });

        let registry = scratch("tddisc-reg");
        publish_input_entry(&registry, "s_tddisc", Some(&socket), None, false);
        let hub = Hub::new(registry.clone());
        let client = hub.open_input("s_tddisc").expect("open");
        let (mut rx, tx) = ws_sink_pair();

        // Confirmed pointer down (no pending_touch_recovery).
        client.enqueue(
            r#"{"t":"touch","phase":"down","id":1,"x":150,"y":250}"#,
            Arc::clone(&tx),
        );
        assert_eq!(read_server_ws_text(&mut rx)["ok"], json!(true));

        // Disconnect the only browser; first ACTION_CANCEL fails, scheduled backoff must retry.
        drop(client);
        let (lock, cv) = &*recovered;
        let guard = lock.lock().unwrap();
        let (guard, _) = cv
            .wait_timeout_while(guard, Duration::from_secs(3), |done| !*done)
            .unwrap();
        assert!(
            *guard,
            "disconnect with failed initial ACTION_CANCEL must retain pointer and retry"
        );

        let actions = seen_actions.lock().unwrap().clone();
        assert_eq!(
            actions,
            vec![
                i64::from(ACTION_DOWN),
                i64::from(ACTION_CANCEL),
                i64::from(ACTION_CANCEL),
            ],
            "failed initial disconnect cancel must be retried until accepted"
        );

        let _ = std::fs::remove_dir_all(&registry);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn disconnect_multi_pointer_partial_failure_retains_remaining_release() {
        let root = scratch("tdmpdisc");
        let socket = root.join("serve.sock");
        let listener = UnixListener::bind(&socket).expect("bind fake daemon");
        let seen: Arc<Mutex<Vec<(i64, Value)>>> = Arc::new(Mutex::new(Vec::new()));
        let seen_t = Arc::clone(&seen);
        let recovered = Arc::new((Mutex::new(false), Condvar::new()));
        let recovered_t = Arc::clone(&recovered);
        let _daemon = std::thread::spawn(move || {
            let mut req_idx = 0usize;
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 {
                        break;
                    }
                    let req: Value = serde_json::from_str(&line).unwrap();
                    if let Some(a) = req["args"]["action"].as_i64() {
                        seen_t
                            .lock()
                            .unwrap()
                            .push((a, req["args"]["pointers"].clone()));
                    }
                    req_idx += 1;
                    // req 1: Client A ptr 1 DOWN -> ok
                    // req 2: Client A ptr 2 POINTER_DOWN -> ok
                    // req 3: Client B ptr 1 POINTER_DOWN -> ok
                    // req 4: Client A disconnect -> release ptr 1 POINTER_UP -> ok
                    // req 5: Client A disconnect -> release ptr 2 POINTER_UP -> drop!
                    // req 6: scheduled retry -> release remaining ptr 2 POINTER_UP -> ok
                    if req_idx == 5 {
                        break;
                    }
                    let resp = json!({
                        "id": req["id"],
                        "envelope": { "ok": true, "result": { "ok": true } },
                    });
                    if stream.write_all(format!("{resp}\n").as_bytes()).is_err() {
                        break;
                    }
                    let _ = stream.flush();
                    if req_idx >= 6 {
                        let (lock, cv) = &*recovered_t;
                        *lock.lock().unwrap() = true;
                        cv.notify_all();
                        return;
                    }
                }
            }
        });

        let registry = scratch("tdmpdisc-reg");
        publish_input_entry(&registry, "s_tdmpdisc", Some(&socket), None, false);
        let hub = Hub::new(registry.clone());
        let client_a = hub.open_input("s_tdmpdisc").expect("open A");
        let client_b = hub.open_input("s_tdmpdisc").expect("open B");
        let (mut rx_a, tx_a) = ws_sink_pair();
        let (mut rx_b, tx_b) = ws_sink_pair();

        client_a.enqueue(
            r#"{"t":"touch","phase":"down","id":1,"x":100,"y":100}"#,
            Arc::clone(&tx_a),
        );
        assert_eq!(read_server_ws_text(&mut rx_a)["ok"], json!(true));
        client_a.enqueue(
            r#"{"t":"touch","phase":"down","id":2,"x":200,"y":200}"#,
            Arc::clone(&tx_a),
        );
        assert_eq!(read_server_ws_text(&mut rx_a)["ok"], json!(true));
        client_b.enqueue(
            r#"{"t":"touch","phase":"down","id":1,"x":300,"y":300}"#,
            Arc::clone(&tx_b),
        );
        assert_eq!(read_server_ws_text(&mut rx_b)["ok"], json!(true));

        // Disconnect A while B stays connected; A's second pointer release fails on req 5
        // and must be retried on req 6 without re-releasing ptr 1 or touching B's pointer.
        drop(client_a);
        let (lock, cv) = &*recovered;
        let guard = lock.lock().unwrap();
        let (guard, _) = cv
            .wait_timeout_while(guard, Duration::from_secs(3), |done| !*done)
            .unwrap();
        assert!(
            *guard,
            "partial failure during multi-pointer disconnect must retry remaining pointer"
        );

        let recorded = seen.lock().unwrap().clone();
        assert_eq!(recorded.len(), 6, "unexpected recorded frames: {recorded:?}");
        // req 4: released wire_id 0 (3 pointers -> 2 pointers remaining: wire_id 1 and 2)
        assert_eq!(
            recorded[3].0,
            i64::from(ACTION_POINTER_UP | (0 << ACTION_POINTER_INDEX_SHIFT))
        );
        assert_eq!(
            recorded[3].1,
            json!([[0, 100.0, 100.0], [1, 200.0, 200.0], [2, 300.0, 300.0]])
        );
        // req 5 (failed) and req 6 (retry): both release wire_id 1 with [wire_id 1, wire_id 2]
        assert_eq!(
            recorded[4].0,
            i64::from(ACTION_POINTER_UP | (0 << ACTION_POINTER_INDEX_SHIFT))
        );
        assert_eq!(recorded[4].1, json!([[1, 200.0, 200.0], [2, 300.0, 300.0]]));
        assert_eq!(recorded[5], recorded[4]);

        drop(client_b);
        let _ = std::fs::remove_dir_all(&registry);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn due_touch_recovery_runs_even_while_text_commands_are_queued() {
        let root = scratch("tdqtext");
        let socket = root.join("serve.sock");
        let listener = UnixListener::bind(&socket).expect("bind fake daemon");
        let seen: Arc<Mutex<Vec<(String, Option<i64>)>>> = Arc::new(Mutex::new(Vec::new()));
        let seen_t = Arc::clone(&seen);
        // Gate req 2 (the immediate ACTION_CANCEL inside TouchPhase::Down) until
        // the test has enqueued all 10 text commands, so the queue is guaranteed
        // non-empty before TouchPhase::Down returns and before any recovery timer
        // can run. An idle-only recovery implementation cannot pass this test.
        let cancel_in_flight = Arc::new((Mutex::new(false), Condvar::new()));
        let cancel_in_flight_t = Arc::clone(&cancel_in_flight);
        let text_queued_gate = Arc::new((Mutex::new(false), Condvar::new()));
        let text_queued_gate_t = Arc::clone(&text_queued_gate);
        let _daemon = std::thread::spawn(move || {
            let mut req_idx = 0usize;
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 {
                        break;
                    }
                    let req: Value = serde_json::from_str(&line).unwrap();
                    let tool = req["tool"].as_str().unwrap_or("").to_string();
                    let action = req["args"]["action"].as_i64();
                    seen_t.lock().unwrap().push((tool.clone(), action));
                    req_idx += 1;
                    // req 1: ACTION_DOWN -> drop (ambiguous EOF)
                    // req 2: immediate ACTION_CANCEL -> hold until texts are queued, then drop
                    // req 3+: daemon recovers; pace panel_text at 10 ms so the
                    // queue stays continuously non-empty across the 25 ms backoff.
                    if req_idx == 1 {
                        break;
                    }
                    if req_idx == 2 {
                        {
                            let (lock, cv) = &*cancel_in_flight_t;
                            *lock.lock().unwrap() = true;
                            cv.notify_all();
                        }
                        {
                            let (lock, cv) = &*text_queued_gate_t;
                            let mut ready = lock.lock().unwrap();
                            while !*ready {
                                ready = cv.wait(ready).unwrap();
                            }
                        }
                        break;
                    }
                    if tool == "panel_text" {
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    let resp = json!({
                        "id": req["id"],
                        "envelope": { "ok": true, "result": { "ok": true } },
                    });
                    if stream.write_all(format!("{resp}\n").as_bytes()).is_err() {
                        break;
                    }
                    let _ = stream.flush();
                }
            }
        });

        let registry = scratch("tdqtext-reg");
        publish_input_entry(&registry, "s_tdqtext", Some(&socket), None, false);
        let hub = Hub::new(registry.clone());
        let client = hub.open_input("s_tdqtext").expect("open");
        let (mut rx, tx) = ws_sink_pair();

        // Trigger failed touch down; wait until the worker is blocked in the
        // immediate cancel (req 2) so TouchPhase::Down has not yet returned.
        client.enqueue(
            r#"{"t":"touch","phase":"down","id":1,"x":200,"y":400}"#,
            Arc::clone(&tx),
        );
        {
            let (lock, cv) = &*cancel_in_flight;
            let guard = lock.lock().unwrap();
            let (guard, _) = cv
                .wait_timeout_while(guard, Duration::from_secs(3), |in_flight| !*in_flight)
                .unwrap();
            assert!(*guard, "worker must reach immediate cancel on req 2");
        }

        // Pre-fill the queue with 10 text commands (~100 ms continuous work)
        // BEFORE releasing req 2, guaranteeing the queue is non-empty when the
        // worker returns to its scheduling loop.
        for i in 0..10 {
            client.enqueue(
                &format!(r#"{{"t":"text","text":"k{i}"}}"#),
                Arc::clone(&tx),
            );
        }
        {
            let (lock, cv) = &*text_queued_gate;
            *lock.lock().unwrap() = true;
            cv.notify_all();
        }

        let down_reply = read_server_ws_text(&mut rx);
        assert_eq!(down_reply["ok"], json!(false));
        assert_eq!(down_reply["code"], json!("EOF"));

        for _ in 0..10 {
            let reply = read_server_ws_text(&mut rx);
            assert_eq!(reply["ok"], json!(true), "text reply failed: {reply}");
        }

        let recorded = seen.lock().unwrap().clone();
        // recorded[0] = failed ACTION_DOWN, recorded[1] = failed immediate ACTION_CANCEL.
        let recovery_pos = recorded
            .iter()
            .enumerate()
            .skip(2)
            .find(|(_, (tool, action))| {
                tool == "panel_touch" && *action == Some(i64::from(ACTION_CANCEL))
            })
            .map(|(idx, _)| idx);
        let first_text_pos = recorded
            .iter()
            .position(|(tool, _)| tool == "panel_text")
            .expect("first panel_text recorded");
        let last_text_pos = recorded
            .iter()
            .rposition(|(tool, _)| tool == "panel_text")
            .expect("last panel_text recorded");
        assert!(
            recovery_pos.is_some_and(|pos| pos > first_text_pos && pos < last_text_pos),
            "due touch recovery must preempt queued text commands rather than waiting for idle: {recorded:?}"
        );

        drop(client);
        let _ = std::fs::remove_dir_all(&registry);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn new_recovery_episode_after_resolving_previous_gets_fresh_retry_budget() {
        let root = scratch("tdep");
        let socket = root.join("serve.sock");
        let listener = UnixListener::bind(&socket).expect("bind fake daemon");
        let seen: Arc<Mutex<Vec<(i64, Value)>>> = Arc::new(Mutex::new(Vec::new()));
        let seen_t = Arc::clone(&seen);
        let hit_11 = Arc::new((Mutex::new(false), Condvar::new()));
        let hit_11_t = Arc::clone(&hit_11);
        let down_b_queued = Arc::new((Mutex::new(false), Condvar::new()));
        let down_b_queued_t = Arc::clone(&down_b_queued);
        let episode_b_recovered = Arc::new((Mutex::new(false), Condvar::new()));
        let episode_b_recovered_t = Arc::clone(&episode_b_recovered);
        let _daemon = std::thread::spawn(move || {
            let mut req_idx = 0usize;
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 {
                        break;
                    }
                    let req: Value = serde_json::from_str(&line).unwrap();
                    if let Some(a) = req["args"]["action"].as_i64() {
                        seen_t
                            .lock()
                            .unwrap()
                            .push((a, req["args"]["pointers"].clone()));
                    }
                    req_idx += 1;
                    // req 1: Episode A ACTION_DOWN -> drop
                    // req 2: Episode A immediate ACTION_CANCEL -> drop
                    // req 3..=13: 11 scheduled retries for Episode A -> drop
                    // (hold req 13 until Down B is queued so attempt 12 cannot race ahead of it)
                    if req_idx <= 13 {
                        if req_idx == 13 {
                            {
                                let (lock, cv) = &*hit_11_t;
                                *lock.lock().unwrap() = true;
                                cv.notify_all();
                            }
                            {
                                let (lock, cv) = &*down_b_queued_t;
                                let mut ready = lock.lock().unwrap();
                                while !*ready {
                                    ready = cv.wait(ready).unwrap();
                                }
                            }
                        }
                        break;
                    }
                    // req 14: Foreground Down resolves Episode A's ACTION_CANCEL -> ok
                    // req 15: Episode B ACTION_DOWN -> drop
                    // req 16: Episode B immediate ACTION_CANCEL -> drop
                    // req 17: Episode B scheduled retry #1 -> drop (would exhaust old budget of 12 if not reset!)
                    // req 18: Episode B scheduled retry #2 -> ok (proves B got a fresh retry budget)
                    if matches!(req_idx, 15 | 16 | 17) {
                        break;
                    }
                    let resp = json!({
                        "id": req["id"],
                        "envelope": { "ok": true, "result": { "ok": true } },
                    });
                    if stream.write_all(format!("{resp}\n").as_bytes()).is_err() {
                        break;
                    }
                    let _ = stream.flush();
                    if req_idx == 18 {
                        let (lock, cv) = &*episode_b_recovered_t;
                        *lock.lock().unwrap() = true;
                        cv.notify_all();
                        return;
                    }
                }
            }
        });

        let registry = scratch("tdep-reg");
        publish_input_entry(&registry, "s_tdep", Some(&socket), None, false);
        let hub = Hub::new(registry.clone());
        let client = hub.open_input("s_tdep").expect("open");
        let (mut rx, tx) = ws_sink_pair();

        // Start Episode A and wait until the 11th scheduled retry (req 13) is in flight.
        client.enqueue(
            r#"{"t":"touch","phase":"down","id":1,"x":100,"y":200}"#,
            Arc::clone(&tx),
        );
        let reply_a = read_server_ws_text(&mut rx);
        assert_eq!(reply_a["ok"], json!(false));
        assert_eq!(reply_a["code"], json!("EOF"));
        {
            let (lock, cv) = &*hit_11;
            let guard = lock.lock().unwrap();
            let (guard, _) = cv
                .wait_timeout_while(guard, Duration::from_secs(10), |done| !*done)
                .unwrap();
            assert!(*guard, "Episode A must reach 11 scheduled retry attempts");
        }

        // Queue Down B BEFORE releasing req 13 so Down B runs at recovery_attempts == 11
        // before attempt 12 can ever fire, resolving Episode A (req 14) and starting B (req 15 + 16).
        client.enqueue(
            r#"{"t":"touch","phase":"down","id":2,"x":300,"y":400}"#,
            Arc::clone(&tx),
        );
        {
            let (lock, cv) = &*down_b_queued;
            *lock.lock().unwrap() = true;
            cv.notify_all();
        }
        let reply_b = read_server_ws_text(&mut rx);
        assert_eq!(reply_b["ok"], json!(false));
        assert_eq!(reply_b["code"], json!("EOF"));

        // Episode B fails its first scheduled retry (req 17) and MUST still perform
        // scheduled retry #2 (req 18) instead of inheriting Episode A's 11-attempt count.
        {
            let (lock, cv) = &*episode_b_recovered;
            let guard = lock.lock().unwrap();
            let (guard, _) = cv
                .wait_timeout_while(guard, Duration::from_secs(3), |done| !*done)
                .unwrap();
            assert!(
                *guard,
                "Episode B must receive a fresh scheduled retry budget after Episode A resolved"
            );
        }

        let recorded = seen.lock().unwrap().clone();
        assert_eq!(recorded.len(), 18, "unexpected recorded frames: {recorded:?}");
        assert_eq!(recorded[13].0, i64::from(ACTION_CANCEL));
        assert_eq!(recorded[13].1, json!([[0, 100.0, 200.0]]));
        assert_eq!(recorded[14].0, i64::from(ACTION_DOWN));
        assert_eq!(recorded[14].1, json!([[0, 300.0, 400.0]]));
        assert_eq!(recorded[16].0, i64::from(ACTION_CANCEL));
        assert_eq!(recorded[16].1, json!([[0, 300.0, 400.0]]));
        assert_eq!(recorded[17].0, i64::from(ACTION_CANCEL));
        assert_eq!(recorded[17].1, json!([[0, 300.0, 400.0]]));

        drop(client);
        let _ = std::fs::remove_dir_all(&registry);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn distinct_pending_release_at_attempt_11_rearms_recovery_and_preserves_order() {
        let root = scratch("rel-rearm");
        let socket = root.join("serve.sock");
        let listener = UnixListener::bind(&socket).expect("bind fake daemon");
        let seen: Arc<Mutex<Vec<(i64, Value)>>> = Arc::new(Mutex::new(Vec::new()));
        let seen_t = Arc::clone(&seen);
        let hit_11 = Arc::new((Mutex::new(false), Condvar::new()));
        let hit_11_t = Arc::clone(&hit_11);
        let up1_queued = Arc::new((Mutex::new(false), Condvar::new()));
        let up1_queued_t = Arc::clone(&up1_queued);
        let both_released = Arc::new((Mutex::new(false), Condvar::new()));
        let both_released_t = Arc::clone(&both_released);
        let _daemon = std::thread::spawn(move || {
            let mut req_idx = 0usize;
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 {
                        break;
                    }
                    let req: Value = serde_json::from_str(&line).unwrap();
                    if let Some(a) = req["args"]["action"].as_i64() {
                        seen_t
                            .lock()
                            .unwrap()
                            .push((a, req["args"]["pointers"].clone()));
                    }
                    req_idx += 1;
                    // req 1, 2: Down(1) and Down(2) succeed.
                    // req 3: initial Up(2) fails.
                    // req 4..=14: 11 scheduled retries for Up(2) fail (hold req 14 until Up(1) is queued).
                    // req 15: foreground Up(1) flushes Up(2) first (order preserved) -> fail.
                    // req 16: scheduled retry #1 after rearm -> fail (would exhaust old budget of 12).
                    // req 17: scheduled retry #2 after rearm -> Up(2) succeeds.
                    // req 18: immediate follow-up flush in same pass -> Up(1) succeeds.
                    if (3..=16).contains(&req_idx) {
                        if req_idx == 14 {
                            {
                                let (lock, cv) = &*hit_11_t;
                                *lock.lock().unwrap() = true;
                                cv.notify_all();
                            }
                            {
                                let (lock, cv) = &*up1_queued_t;
                                let mut ready = lock.lock().unwrap();
                                while !*ready {
                                    ready = cv.wait(ready).unwrap();
                                }
                            }
                        }
                        break;
                    }
                    let resp = json!({
                        "id": req["id"],
                        "envelope": { "ok": true, "result": { "ok": true } },
                    });
                    if stream.write_all(format!("{resp}\n").as_bytes()).is_err() {
                        break;
                    }
                    let _ = stream.flush();
                    if req_idx == 18 {
                        let (lock, cv) = &*both_released_t;
                        *lock.lock().unwrap() = true;
                        cv.notify_all();
                        return;
                    }
                }
            }
        });

        let registry = scratch("rel-rearm-reg");
        publish_input_entry(&registry, "s_rel_rearm", Some(&socket), None, false);
        let hub = Hub::new(registry.clone());
        let client = hub.open_input("s_rel_rearm").expect("open");
        let (mut rx, tx) = ws_sink_pair();

        client.enqueue(
            r#"{"t":"touch","phase":"down","id":1,"x":100,"y":200}"#,
            Arc::clone(&tx),
        );
        assert_eq!(read_server_ws_text(&mut rx)["ok"], json!(true));
        client.enqueue(
            r#"{"t":"touch","phase":"down","id":2,"x":300,"y":400}"#,
            Arc::clone(&tx),
        );
        assert_eq!(read_server_ws_text(&mut rx)["ok"], json!(true));

        // First Up(2) fails and enters pending_releases.
        client.enqueue(
            r#"{"t":"touch","phase":"up","id":2,"x":310,"y":410}"#,
            Arc::clone(&tx),
        );
        assert_eq!(read_server_ws_text(&mut rx)["ok"], json!(false));

        // Wait until scheduled retry #11 (req 14) is in flight.
        {
            let (lock, cv) = &*hit_11;
            let guard = lock.lock().unwrap();
            let (guard, _) = cv
                .wait_timeout_while(guard, Duration::from_secs(10), |done| !*done)
                .unwrap();
            assert!(*guard, "first Up must reach 11 scheduled retries");
        }

        // Enqueue distinct Up(1) before releasing req 14 so it joins pending_releases at attempt 11.
        client.enqueue(
            r#"{"t":"touch","phase":"up","id":1,"x":110,"y":210}"#,
            Arc::clone(&tx),
        );
        {
            let (lock, cv) = &*up1_queued;
            *lock.lock().unwrap() = true;
            cv.notify_all();
        }
        assert_eq!(read_server_ws_text(&mut rx)["ok"], json!(false));

        // Rearmed recovery must continue past attempt 12 and release both pointers in order.
        {
            let (lock, cv) = &*both_released;
            let guard = lock.lock().unwrap();
            let (guard, _) = cv
                .wait_timeout_while(guard, Duration::from_secs(3), |done| !*done)
                .unwrap();
            assert!(
                *guard,
                "distinct pending release must rearm recovery budget and flush both pointers"
            );
        }

        let recorded = seen.lock().unwrap().clone();
        let ptr_up_1 = i64::from(ACTION_POINTER_UP | (1 << ACTION_POINTER_INDEX_SHIFT));
        assert_eq!(recorded.len(), 18, "unexpected recorded frames: {recorded:?}");
        assert_eq!(recorded[14].0, ptr_up_1, "existing Up(2) must stay first in queue");
        assert_eq!(recorded[15].0, ptr_up_1);
        assert_eq!(recorded[16].0, ptr_up_1);
        assert_eq!(recorded[17].0, i64::from(ACTION_UP));
        assert_eq!(recorded[17].1, json!([[0, 110.0, 210.0]]));

        drop(client);
        let _ = std::fs::remove_dir_all(&registry);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn duplicate_pending_up_at_attempt_11_does_not_rearm_recovery() {
        let root = scratch("dup-no-rearm");
        let socket = root.join("serve.sock");
        let listener = UnixListener::bind(&socket).expect("bind fake daemon");
        let seen: Arc<Mutex<Vec<(i64, Value)>>> = Arc::new(Mutex::new(Vec::new()));
        let seen_t = Arc::clone(&seen);
        let hit_11 = Arc::new((Mutex::new(false), Condvar::new()));
        let hit_11_t = Arc::clone(&hit_11);
        let dup_queued = Arc::new((Mutex::new(false), Condvar::new()));
        let dup_queued_t = Arc::clone(&dup_queued);
        let hit_12 = Arc::new((Mutex::new(false), Condvar::new()));
        let hit_12_t = Arc::clone(&hit_12);
        let _daemon = std::thread::spawn(move || {
            let mut req_idx = 0usize;
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 {
                        break;
                    }
                    let req: Value = serde_json::from_str(&line).unwrap();
                    if let Some(a) = req["args"]["action"].as_i64() {
                        seen_t
                            .lock()
                            .unwrap()
                            .push((a, req["args"]["pointers"].clone()));
                    }
                    req_idx += 1;
                    if req_idx <= 2 {
                        let resp = json!({
                            "id": req["id"],
                            "envelope": { "ok": true, "result": { "ok": true } },
                        });
                        if stream.write_all(format!("{resp}\n").as_bytes()).is_err() {
                            break;
                        }
                        let _ = stream.flush();
                        continue;
                    }
                    if req_idx == 14 {
                        {
                            let (lock, cv) = &*hit_11_t;
                            *lock.lock().unwrap() = true;
                            cv.notify_all();
                        }
                        {
                            let (lock, cv) = &*dup_queued_t;
                            let mut ready = lock.lock().unwrap();
                            while !*ready {
                                ready = cv.wait(ready).unwrap();
                            }
                        }
                    } else if req_idx == 16 {
                        let (lock, cv) = &*hit_12_t;
                        *lock.lock().unwrap() = true;
                        cv.notify_all();
                    }
                    break;
                }
            }
        });

        let registry = scratch("dup-no-rearm-reg");
        publish_input_entry(&registry, "s_dup_nr", Some(&socket), None, false);
        let hub = Hub::new(registry.clone());
        let client = hub.open_input("s_dup_nr").expect("open");
        let (mut rx, tx) = ws_sink_pair();

        client.enqueue(
            r#"{"t":"touch","phase":"down","id":1,"x":100,"y":200}"#,
            Arc::clone(&tx),
        );
        assert_eq!(read_server_ws_text(&mut rx)["ok"], json!(true));
        client.enqueue(
            r#"{"t":"touch","phase":"down","id":2,"x":300,"y":400}"#,
            Arc::clone(&tx),
        );
        assert_eq!(read_server_ws_text(&mut rx)["ok"], json!(true));

        client.enqueue(
            r#"{"t":"touch","phase":"up","id":2,"x":310,"y":410}"#,
            Arc::clone(&tx),
        );
        assert_eq!(read_server_ws_text(&mut rx)["ok"], json!(false));

        {
            let (lock, cv) = &*hit_11;
            let guard = lock.lock().unwrap();
            let (guard, _) = cv
                .wait_timeout_while(guard, Duration::from_secs(10), |done| !*done)
                .unwrap();
            assert!(*guard, "first Up must reach 11 scheduled retries");
        }

        // Send a duplicate Up(2) at attempt 11: updates coordinates in place without rearming.
        client.enqueue(
            r#"{"t":"touch","phase":"up","id":2,"x":320,"y":420}"#,
            Arc::clone(&tx),
        );
        {
            let (lock, cv) = &*dup_queued;
            *lock.lock().unwrap() = true;
            cv.notify_all();
        }
        assert_eq!(read_server_ws_text(&mut rx)["ok"], json!(false));

        // Attempt 12 (req 16) is the final scheduled retry.
        {
            let (lock, cv) = &*hit_12;
            let guard = lock.lock().unwrap();
            let (guard, _) = cv
                .wait_timeout_while(guard, Duration::from_secs(3), |done| !*done)
                .unwrap();
            assert!(*guard, "attempt 12 must still fire");
        }

        // Wait until the worker finishes handling request 16's failure and
        // settles its next retry decision; duplicate Up must leave attempt 12
        // exhausted with no further retry scheduled.
        let settled = hub
            .wait_for_scheduled_recovery_settled("s_dup_nr", 12, Duration::from_secs(3))
            .expect("worker must settle after attempt 12 failure");
        assert_eq!(
            settled,
            (12, RECOVERY_MAX_ATTEMPTS, false),
            "duplicate Up must not rearm recovery or schedule a 13th retry"
        );
        let recorded = seen.lock().unwrap().clone();
        assert_eq!(
            recorded.len(),
            16,
            "duplicate Up must not rearm recovery past attempt 12: {recorded:?}"
        );
        assert_eq!(recorded[14].1, json!([[0, 100.0, 200.0], [1, 320.0, 420.0]]));

        drop(client);
        let _ = std::fs::remove_dir_all(&registry);
        let _ = std::fs::remove_dir_all(&root);
    }
}
