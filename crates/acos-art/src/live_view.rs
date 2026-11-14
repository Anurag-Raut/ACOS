//! Live view, producer side (VIEW1a): protocol v1 on a per-session Unix socket.
//!
//! A live ART session can stream its screen and an action/event feed to any
//! number of local viewers (a browser hub, a probe script). The owner decision
//! is ADR-0027: frames stream **only while a viewer is connected**, never force
//! a present, never change `frame_seq`/`generation`/damage, never block the
//! compositor or an agent command.
//!
//! The wire protocol is documented in `docs/LIVE_VIEW.md` and is a contract with
//! the separate consumer track (VIEW1b). In one paragraph:
//!
//! * client → server: one JSON line
//!   `{"v":1,"subscribe":{"frames":true,"events":true,"max_fps":15,
//!   "scale":0.5,"format":"jpeg"}}`;
//! * server → client: `4-byte big-endian length (type + payload)`, one type
//!   byte, then the payload. `H` = hello JSON, `F` = a JSON header line
//!   terminated by `\n` immediately followed by the encoded image bytes,
//!   `E` = one event JSON.
//!
//! ## Cost model (hard rules)
//!
//! * **No subscriber**: [`LiveView::tick`] performs one atomic load and
//!   returns. No lock, no copy, no encode, no allocation. (The launcher's tap
//!   thread also blocks on a condvar between probe wakes, so an idle session
//!   with no viewer costs one atomic load per probe sample and nothing else.)
//! * **With a subscriber**: on a *new* composited frame (the source reports a
//!   `seq` newer than the last one tapped) the frame is box-filtered down to
//!   `scale` and stored in a single "latest" slot under a short lock; a writer
//!   thread is signalled. The writer drops intermediate frames (latest wins),
//!   rate-limits to `max_fps`, and writes to each subscriber with non-blocking
//!   sends — a viewer that cannot keep up is disconnected, never back-pressured.
//! * Events go through a bounded queue that drops the oldest when full.
//!
//! The module is deliberately independent of the compositor: [`SourceFrame`] is
//! plain RGBA, so the tests drive the whole protocol with a synthetic source and
//! a real Unix socket.

use std::collections::VecDeque;
use std::io::{self, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Protocol version this producer speaks.
pub const PROTOCOL_VERSION: u32 = 1;
/// Message type: hello JSON.
pub const TYPE_HELLO: u8 = b'H';
/// Message type: frame (JSON header line + image bytes).
pub const TYPE_FRAME: u8 = b'F';
/// Message type: event JSON.
pub const TYPE_EVENT: u8 = b'E';
/// Subscribers a session serves at once (the oldest is dropped when a fifth
/// connects, so a viewer that reconnects is never locked out).
pub const MAX_SUBSCRIBERS: usize = 4;
/// Largest subscribe line accepted from a client.
pub const MAX_SUBSCRIBE_LINE: usize = 4096;
/// Largest message this producer will ever emit (a guard for consumers).
pub const MAX_MESSAGE_BYTES: usize = 64 * 1024 * 1024;
/// Default frame rate cap.
pub const DEFAULT_MAX_FPS: u32 = 15;
/// Default image format: **PNG**.
///
/// Measured on this host at scale 0.5 (540x1200), producer-side encoder clock,
/// 2026-11-14: PNG (fast deflate + adaptive filter) costs 1.1-1.3 ms and
/// 20-54 KB per frame; JPEG q75 costs 9.4-11.6 ms and 23-38 KB. PNG is 8-9x
/// cheaper in CPU (which is the scarce resource: the encoder shares the host
/// with the real guest) and smaller on a flat UI screen, and it is lossless so
/// half-resolution text is exactly the framework's pixels. `jpeg` stays
/// selectable per subscriber / `ACOS_VIEW_FORMAT`; it wins on bytes for
/// image-heavy screens. See `docs/LIVE_VIEW.md` §7.
pub const DEFAULT_FORMAT: FrameFormat = FrameFormat::Png;
/// Default downscale factor (half resolution: 1080x2400 → 540x1200).
pub const DEFAULT_SCALE: f32 = 0.5;
/// Default JPEG quality (readable UI text at scale 0.5).
pub const DEFAULT_JPEG_QUALITY: u8 = 75;
/// Events buffered before the oldest is dropped.
pub const EVENT_QUEUE: usize = 64;
/// Text in an event is always truncated to this many characters.
pub const EVENT_TEXT_CHARS: usize = 64;
/// What a masked (password-field) text is replaced with.
pub const MASKED_TEXT: &str = "[masked]";

/// Image format of the frame stream.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameFormat {
    /// JPEG (lossy; smaller than PNG on image-heavy screens, ~8x the encoder
    /// CPU). Selectable per subscriber, not the default (see
    /// [`DEFAULT_FORMAT`]).
    Jpeg,
    /// PNG (lossless).
    Png,
}

impl FrameFormat {
    /// Wire name (`"jpeg"` | `"png"`).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Jpeg => "jpeg",
            Self::Png => "png",
        }
    }

    /// Parse a wire name.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        match text.trim().to_ascii_lowercase().as_str() {
            "jpeg" | "jpg" => Some(Self::Jpeg),
            "png" => Some(Self::Png),
            _ => None,
        }
    }

    const fn code(self) -> u8 {
        match self {
            Self::Jpeg => 1,
            Self::Png => 2,
        }
    }

    const fn from_code(code: u8) -> Option<Self> {
        match code {
            1 => Some(Self::Jpeg),
            2 => Some(Self::Png),
            _ => None,
        }
    }
}

/// Stream configuration. `None` fields are decided by the **first** subscriber's
/// `subscribe` request (so a viewer that asks for PNG gets PNG when it is alone),
/// and a later subscriber is told the active values in its hello.
#[derive(Clone, Debug)]
pub struct LiveViewConfig {
    /// Frame image format; `None` = the first subscriber's choice (jpeg default).
    pub format: Option<FrameFormat>,
    /// Downscale factor `0.1..=1.0`; `None` = the first subscriber's choice.
    pub scale: Option<f32>,
    /// Frames per second cap `1..=30`; `None` = the first subscriber's choice.
    pub max_fps: Option<u32>,
    /// JPEG quality (ignored for PNG).
    pub jpeg_quality: u8,
}

impl Default for LiveViewConfig {
    fn default() -> Self {
        Self {
            format: None,
            scale: None,
            max_fps: None,
            jpeg_quality: DEFAULT_JPEG_QUALITY,
        }
    }
}

impl LiveViewConfig {
    /// Read the launcher's defaults from `ACOS_VIEW_FORMAT`, `ACOS_VIEW_SCALE`
    /// and `ACOS_VIEW_MAX_FPS` (all optional; a bad value is ignored, never a
    /// failure — a viewer must never be able to break a session).
    #[must_use]
    pub fn from_env() -> Self {
        let mut config = Self::default();
        if let Ok(text) = std::env::var("ACOS_VIEW_FORMAT") {
            if let Some(format) = FrameFormat::parse(&text) {
                config.format = Some(format);
            }
        }
        if let Ok(text) = std::env::var("ACOS_VIEW_SCALE") {
            if let Ok(scale) = text.trim().parse::<f32>() {
                if scale > 0.0 && scale <= 1.0 {
                    config.scale = Some(scale);
                }
            }
        }
        if let Ok(text) = std::env::var("ACOS_VIEW_MAX_FPS") {
            if let Ok(fps) = text.trim().parse::<u32>() {
                if fps > 0 && fps <= 30 {
                    config.max_fps = Some(fps);
                }
            }
        }
        if let Ok(text) = std::env::var("ACOS_VIEW_JPEG_QUALITY") {
            if let Ok(q) = text.trim().parse::<u8>() {
                if (20..=95).contains(&q) {
                    config.jpeg_quality = q;
                }
            }
        }
        config
    }
}

/// One composited frame handed over by the launcher's frame source.
///
/// `seq` must advance for every new frame the compositor produced (it is the C1
/// store counter); the tap only copies a frame whose `seq` is newer than the one
/// it already tapped, so an unchanged screen costs nothing.
pub struct SourceFrame {
    /// Monotonic frame id (C1 `buffer_generations`).
    pub seq: u64,
    /// The C1 layer generation the captured buffer belongs to.
    pub generation: u64,
    /// Frame width in pixels.
    pub width: u32,
    /// Frame height in pixels.
    pub height: u32,
    /// Row stride in **pixels** (the buffer may be padded).
    pub stride: u32,
    /// Premultiplied RGBA8 pixels, `stride * height * 4` bytes.
    pub rgba: Vec<u8>,
}

/// Counters for the cost rules and the acceptance measurements.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LiveViewStats {
    /// Tap calls (one per probe wake / poll tick).
    pub ticks: u64,
    /// Tap calls that actually asked the source for a frame. With no subscriber
    /// this must stay 0 — that is the "one atomic load and nothing else" rule.
    pub source_calls: u64,
    /// Frames the source produced and were downscaled into the slot.
    pub frames_captured: u64,
    /// Frames encoded and written to the subscribers.
    pub encodes: u64,
    /// Total time spent encoding (microseconds).
    pub encode_us_total: u64,
    /// Total bytes of encoded images.
    pub encode_bytes_total: u64,
    /// Frames written to at least one subscriber.
    pub frames_sent: u64,
    /// Events written to at least one subscriber.
    pub events_sent: u64,
    /// Events dropped because the queue was full.
    pub events_dropped: u64,
    /// Subscribers disconnected for not keeping up.
    pub subscribers_dropped: u64,
    /// Frames superseded in the slot before they were encoded (latest wins).
    pub frames_dropped_latest: u64,
    /// Microseconds spent in the last encode.
    pub last_encode_us: u64,
    /// Bytes of the last encoded image.
    pub last_encode_bytes: u64,
    /// Subscribers currently attached.
    pub subscribers: u64,
}

impl LiveViewStats {
    /// Mean encode time per encoded frame in milliseconds.
    #[must_use]
    pub fn mean_encode_ms(&self) -> f64 {
        if self.encodes == 0 {
            0.0
        } else {
            self.encode_us_total as f64 / self.encodes as f64 / 1000.0
        }
    }

    /// Mean encoded image size in bytes.
    #[must_use]
    pub fn mean_bytes(&self) -> f64 {
        if self.encodes == 0 {
            0.0
        } else {
            self.encode_bytes_total as f64 / self.encodes as f64
        }
    }
}

/// A frame downscaled and waiting for the encoder ("latest wins").
struct Slot {
    seq: u64,
    generation: u64,
    width: u32,
    height: u32,
    ts_ms: u64,
    rgba: Vec<u8>,
}

/// One connected viewer.
struct Subscriber {
    stream: UnixStream,
    wants_frames: bool,
    wants_events: bool,
}

impl Subscriber {
    fn send(&mut self, message: &[u8]) -> io::Result<()> {
        // Non-blocking by construction: the socket is in non-blocking mode, and
        // a partial write or `WouldBlock` means the viewer is not keeping up.
        // The subscriber is dropped instead of back-pressuring the encoder.
        let mut written = 0;
        while written < message.len() {
            match self.stream.write(&message[written..]) {
                Ok(0) => {
                    return Err(io::Error::new(
                        io::ErrorKind::WriteZero,
                        "the subscriber closed the connection",
                    ))
                }
                Ok(n) => written += n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }
}

struct Inner {
    config: LiveViewConfig,
    display: (u32, u32),
    session_pid: u32,
    /// The values the stream is actually using (fixed by the first subscriber).
    active_format: AtomicU32,
    active_scale: AtomicU32,
    active_max_fps: AtomicU32,
    subscribers: Mutex<Vec<Subscriber>>,
    subscriber_count: AtomicUsize,
    slot: Mutex<Option<Slot>>,
    last_encoded_seq: AtomicU64,
    events: Mutex<VecDeque<serde_json::Value>>,
    /// Wake for the writer thread (frames, events, subscribers, shutdown).
    wake_lock: Mutex<()>,
    wake: Condvar,
    shutdown: AtomicBool,
    ticks: AtomicU64,
    source_calls: AtomicU64,
    frames_captured: AtomicU64,
    encodes: AtomicU64,
    encode_us_total: AtomicU64,
    encode_bytes_total: AtomicU64,
    frames_sent: AtomicU64,
    events_sent: AtomicU64,
    events_dropped: AtomicU64,
    subscribers_dropped: AtomicU64,
    frames_dropped_latest: AtomicU64,
    last_encode_us: AtomicU64,
    last_encode_bytes: AtomicU64,
    /// The last tree any reply carried said a *focused* node is a password
    /// field; used to mask typed text in the event feed.
    focused_password: AtomicBool,
    /// The focused activity the feed last reported (a change emits an
    /// `activity` event).
    last_activity: Mutex<Option<String>>,
    /// The command the launcher is currently executing. The event feed reports
    /// the *request*'s coordinates/key/text (a reply need not echo them).
    pending_request: Mutex<Option<serde_json::Value>>,
}

impl Inner {
    fn notify(&self) {
        let _guard = self.wake_lock.lock().unwrap_or_else(|e| e.into_inner());
        self.wake.notify_all();
    }

    fn wait(&self, timeout: Duration) {
        let guard = self.wake_lock.lock().unwrap_or_else(|e| e.into_inner());
        let _ = self
            .wake
            .wait_timeout(guard, timeout)
            .unwrap_or_else(|e| e.into_inner());
    }

    fn format(&self) -> FrameFormat {
        FrameFormat::from_code(self.active_format.load(Ordering::Relaxed) as u8)
            .unwrap_or(DEFAULT_FORMAT)
    }

    fn scale(&self) -> f32 {
        let milli = self.active_scale.load(Ordering::Relaxed);
        if milli == 0 {
            DEFAULT_SCALE
        } else {
            milli as f32 / 1000.0
        }
    }

    fn max_fps(&self) -> u32 {
        let fps = self.active_max_fps.load(Ordering::Relaxed);
        if fps == 0 {
            DEFAULT_MAX_FPS
        } else {
            fps
        }
    }

    fn interval(&self) -> Duration {
        Duration::from_micros(1_000_000 / u64::from(self.max_fps().max(1)))
    }

    /// The first subscriber fixes the stream's format/scale/rate; a request that
    /// arrives later is told the active values instead (one encoder, one shape).
    fn configure_from_request(&self, request: &serde_json::Value) {
        let subscribe = request.get("subscribe").unwrap_or(request);
        if self.active_format.load(Ordering::Acquire) == 0 {
            let requested = subscribe
                .get("format")
                .and_then(|v| v.as_str())
                .and_then(FrameFormat::parse)
                .or(self.config.format)
                .unwrap_or(DEFAULT_FORMAT);
            self.active_format
                .store(u32::from(requested.code()), Ordering::Release);
        }
        if self.active_scale.load(Ordering::Acquire) == 0 {
            let requested = subscribe
                .get("scale")
                .and_then(|v| v.as_f64())
                .map(|v| v as f32)
                .filter(|v| *v > 0.0 && *v <= 1.0)
                .or(self.config.scale)
                .unwrap_or(DEFAULT_SCALE);
            self.active_scale
                .store((requested * 1000.0).round() as u32, Ordering::Release);
        }
        if self.active_max_fps.load(Ordering::Acquire) == 0 {
            let requested = subscribe
                .get("max_fps")
                .and_then(|v| v.as_u64())
                .map(|v| v as u32)
                .filter(|v| *v > 0 && *v <= 30)
                .or(self.config.max_fps)
                .unwrap_or(DEFAULT_MAX_FPS);
            self.active_max_fps.store(requested, Ordering::Release);
        }
    }

    fn hello(&self) -> serde_json::Value {
        let (w, h) = self.display;
        serde_json::json!({
            "v": PROTOCOL_VERSION,
            "session": self.session_pid,
            "display": {"w": w, "h": h},
            "format": self.format().as_str(),
            "scale": self.scale(),
        })
    }

    fn attach(&self, stream: UnixStream, request: &serde_json::Value) -> io::Result<()> {
        self.configure_from_request(request);
        let subscribe = request.get("subscribe").unwrap_or(request);
        let wants_frames = subscribe
            .get("frames")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(true);
        let wants_events = subscribe
            .get("events")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(true);
        stream.set_nonblocking(true)?;
        let hello = frame_message(TYPE_HELLO, self.hello().to_string().as_bytes());
        let mut subscriber = Subscriber {
            stream,
            wants_frames,
            wants_events,
        };
        subscriber.send(&hello)?;
        let mut subscribers = self.subscribers.lock().unwrap_or_else(|e| e.into_inner());
        while subscribers.len() >= self.config_max_subscribers() {
            // Cap: drop the oldest so a reconnecting viewer is never locked out.
            subscribers.remove(0);
            self.subscribers_dropped.fetch_add(1, Ordering::Relaxed);
            self.subscriber_count.fetch_sub(1, Ordering::AcqRel);
        }
        subscribers.push(subscriber);
        self.subscriber_count.fetch_add(1, Ordering::AcqRel);
        drop(subscribers);
        self.notify();
        Ok(())
    }

    fn config_max_subscribers(&self) -> usize {
        MAX_SUBSCRIBERS
    }

    fn broadcast(&self, message: &[u8], frames: bool) {
        let mut subscribers = self.subscribers.lock().unwrap_or_else(|e| e.into_inner());
        if subscribers.is_empty() {
            return;
        }
        let mut dead: Vec<usize> = Vec::new();
        for (index, subscriber) in subscribers.iter_mut().enumerate() {
            if frames && !subscriber.wants_frames {
                continue;
            }
            if !frames && !subscriber.wants_events {
                continue;
            }
            if subscriber.send(message).is_err() {
                dead.push(index);
            }
        }
        for index in dead.into_iter().rev() {
            subscribers.remove(index);
            self.subscriber_count.fetch_sub(1, Ordering::AcqRel);
            self.subscribers_dropped.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Enqueue one event, dropping the oldest when the queue is full.
    fn enqueue_event(&self, event: serde_json::Value) {
        {
            let mut queue = self.events.lock().unwrap_or_else(|e| e.into_inner());
            while queue.len() >= EVENT_QUEUE {
                queue.pop_front();
                self.events_dropped.fetch_add(1, Ordering::Relaxed);
            }
            queue.push_back(event);
        }
        self.notify();
    }

    fn push_event(&self, event: serde_json::Value) {
        // No subscriber: an event costs nothing at all (not even a queue push).
        if self.subscriber_count.load(Ordering::Acquire) == 0 {
            return;
        }
        self.enqueue_event(event);
    }

    fn drain_events(&self) -> Vec<serde_json::Value> {
        let mut queue = self.events.lock().unwrap_or_else(|e| e.into_inner());
        queue.drain(..).collect()
    }
}

/// The producer handle a live session holds.
pub struct LiveView {
    inner: Arc<Inner>,
    path: PathBuf,
}

impl LiveView {
    /// Bind the stream socket and start the writer thread.
    ///
    /// The socket lives in the session's short runtime directory (the caller
    /// passes a path whose `sun_path` fits, exactly like the session's peer
    /// socket); its file mode is 0600.
    pub fn bind(
        socket_path: &Path,
        config: LiveViewConfig,
        display: (u32, u32),
        session_pid: u32,
    ) -> io::Result<Arc<Self>> {
        if socket_path.as_os_str().len() > 100 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "stream socket path {} is too long for a unix socket",
                    socket_path.display()
                ),
            ));
        }
        if let Some(parent) = socket_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let _ = std::fs::remove_file(socket_path);
        let listener = UnixListener::bind(socket_path)?;
        // 0600: a session's screen is readable by its owner only.
        let _ = std::fs::set_permissions(socket_path, std::fs::Permissions::from_mode(0o600));
        let inner = Arc::new(Inner {
            config,
            display,
            session_pid,
            active_format: AtomicU32::new(0),
            active_scale: AtomicU32::new(0),
            active_max_fps: AtomicU32::new(0),
            subscribers: Mutex::new(Vec::new()),
            subscriber_count: AtomicUsize::new(0),
            slot: Mutex::new(None),
            last_encoded_seq: AtomicU64::new(0),
            events: Mutex::new(VecDeque::new()),
            wake_lock: Mutex::new(()),
            wake: Condvar::new(),
            shutdown: AtomicBool::new(false),
            ticks: AtomicU64::new(0),
            source_calls: AtomicU64::new(0),
            frames_captured: AtomicU64::new(0),
            encodes: AtomicU64::new(0),
            encode_us_total: AtomicU64::new(0),
            encode_bytes_total: AtomicU64::new(0),
            frames_sent: AtomicU64::new(0),
            events_sent: AtomicU64::new(0),
            events_dropped: AtomicU64::new(0),
            subscribers_dropped: AtomicU64::new(0),
            frames_dropped_latest: AtomicU64::new(0),
            last_encode_us: AtomicU64::new(0),
            last_encode_bytes: AtomicU64::new(0),
            focused_password: AtomicBool::new(false),
            last_activity: Mutex::new(None),
            pending_request: Mutex::new(None),
        });
        let view = Arc::new(Self {
            inner: Arc::clone(&inner),
            path: socket_path.to_path_buf(),
        });
        // Writer: one thread, one socket per subscriber, non-blocking sends.
        {
            let inner = Arc::clone(&inner);
            std::thread::Builder::new()
                .name("acos-live-view-writer".to_string())
                .spawn(move || writer_loop(inner))
                .map_err(io::Error::other)?;
        }
        // Acceptor: a blocking accept (zero idle cost); shutdown wakes it with a
        // self-connection.
        {
            let inner = Arc::clone(&inner);
            let path = socket_path.to_path_buf();
            std::thread::Builder::new()
                .name("acos-live-view-accept".to_string())
                .spawn(move || accept_loop(listener, inner, path))
                .map_err(io::Error::other)?;
        }
        Ok(view)
    }

    /// The socket path reported in the session's ready line.
    #[must_use]
    pub fn socket_path(&self) -> &Path {
        &self.path
    }

    /// Whether at least one viewer is attached (the fast-path predicate).
    #[must_use]
    pub fn has_subscribers(&self) -> bool {
        self.inner.subscriber_count.load(Ordering::Acquire) > 0
    }

    /// One frame-tap step.
    ///
    /// **The cost rule lives here**: with no subscriber this is one atomic load
    /// and a return — the source is never called, so no pixels are copied and
    /// nothing is encoded.
    pub fn tick<F>(&self, source: &mut F)
    where
        F: FnMut() -> Option<SourceFrame>,
    {
        let inner = &*self.inner;
        inner.ticks.fetch_add(1, Ordering::Relaxed);
        if inner.subscriber_count.load(Ordering::Acquire) == 0 {
            return;
        }
        inner.source_calls.fetch_add(1, Ordering::Relaxed);
        let Some(frame) = source() else {
            return;
        };
        let scale = inner.scale();
        let (width, height, rgba) =
            downscale_box(&frame.rgba, frame.width, frame.height, frame.stride, scale);
        {
            let mut slot = inner.slot.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(previous) = slot.as_ref() {
                if previous.seq != frame.seq
                    && previous.seq > inner.last_encoded_seq.load(Ordering::Relaxed)
                {
                    // Superseded before the encoder got to it: latest wins.
                    inner.frames_dropped_latest.fetch_add(1, Ordering::Relaxed);
                }
            }
            *slot = Some(Slot {
                seq: frame.seq,
                generation: frame.generation,
                width,
                height,
                ts_ms: now_ms(),
                rgba,
            });
        }
        inner.frames_captured.fetch_add(1, Ordering::Relaxed);
        inner.notify();
    }

    /// Feed one launcher reply into the event stream.
    ///
    /// Every command the launcher executes becomes an `action` event with its
    /// result status; a focused-activity change becomes an `activity` event;
    /// launch/reset/close become `lifecycle` events; a reply that carries an
    /// unsettled observation becomes a `settle` event.
    /// Record the command the launcher is about to execute, so its reply's
    /// event carries the request's own coordinates/key/text.
    pub fn observe_request(&self, request: &serde_json::Value) {
        if self.inner.subscriber_count.load(Ordering::Acquire) == 0 {
            return;
        }
        let mut slot = self
            .inner
            .pending_request
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        *slot = Some(request.clone());
    }

    pub fn observe_reply(&self, reply: &serde_json::Value) {
        if self.inner.subscriber_count.load(Ordering::Acquire) == 0 {
            return;
        }
        let request = self
            .inner
            .pending_request
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        if let Some(tree) = reply.get("tree") {
            self.inner
                .focused_password
                .store(tree_has_focused_password(tree), Ordering::Relaxed);
        }
        // A focused-activity change is its own event (the feed's `activity`
        // kind), emitted once per distinct value.
        if let Some(activity) = reply
            .get("activity")
            .and_then(|v| v.as_str())
            .filter(|v| !v.is_empty())
        {
            let changed = {
                let mut last = self
                    .inner
                    .last_activity
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                if last.as_deref() == Some(activity) {
                    false
                } else {
                    *last = Some(activity.to_string());
                    true
                }
            };
            if changed {
                let frame_seq = reply.get("frame_seq").and_then(|v| v.as_u64());
                self.inner.push_event(activity_event(activity, frame_seq));
            }
        }
        if let Some(mut event) =
            event_from_reply(reply, self.inner.focused_password.load(Ordering::Relaxed))
        {
            if let Some(request) = request.as_ref() {
                merge_request(
                    &mut event,
                    request,
                    self.inner.focused_password.load(Ordering::Relaxed),
                );
            }
            self.inner.push_event(event);
        }
    }

    /// `true` once the producer has been shut down (the tap thread exits).
    #[must_use]
    pub fn is_shutdown(&self) -> bool {
        self.inner.shutdown.load(Ordering::Acquire)
    }

    /// Push a caller-built event (the launcher uses this for lifecycle).
    pub fn push_event(&self, event: serde_json::Value) {
        self.inner.push_event(event);
    }

    #[cfg(test)]
    fn enqueue_event(&self, event: serde_json::Value) {
        self.inner.enqueue_event(event);
    }

    #[cfg(test)]
    fn queued_events(&self) -> Vec<serde_json::Value> {
        self.inner
            .events
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .cloned()
            .collect()
    }

    /// Counters (acceptance evidence and the bench).
    #[must_use]
    pub fn stats(&self) -> LiveViewStats {
        let inner = &*self.inner;
        LiveViewStats {
            ticks: inner.ticks.load(Ordering::Relaxed),
            source_calls: inner.source_calls.load(Ordering::Relaxed),
            frames_captured: inner.frames_captured.load(Ordering::Relaxed),
            encodes: inner.encodes.load(Ordering::Relaxed),
            encode_us_total: inner.encode_us_total.load(Ordering::Relaxed),
            encode_bytes_total: inner.encode_bytes_total.load(Ordering::Relaxed),
            frames_sent: inner.frames_sent.load(Ordering::Relaxed),
            events_sent: inner.events_sent.load(Ordering::Relaxed),
            events_dropped: inner.events_dropped.load(Ordering::Relaxed),
            subscribers_dropped: inner.subscribers_dropped.load(Ordering::Relaxed),
            frames_dropped_latest: inner.frames_dropped_latest.load(Ordering::Relaxed),
            last_encode_us: inner.last_encode_us.load(Ordering::Relaxed),
            last_encode_bytes: inner.last_encode_bytes.load(Ordering::Relaxed),
            subscribers: inner.subscriber_count.load(Ordering::Acquire) as u64,
        }
    }

    /// The producer's counters and active shape as one JSON object.
    ///
    /// Used by the dev-only `ACOS_VIEW_STATS_FILE` seam (the launcher writes it
    /// when a session ends) so a measurement run can report the encoder's own
    /// per-frame cost and bytes without adding a protocol message.
    #[must_use]
    pub fn stats_json(&self) -> serde_json::Value {
        let stats = self.stats();
        serde_json::json!({
            "format": self.inner.format().as_str(),
            "scale": self.inner.scale(),
            "max_fps": self.inner.max_fps(),
            "jpeg_quality": self.inner.config.jpeg_quality,
            "display": {"w": self.inner.display.0, "h": self.inner.display.1},
            "stats": {
                "ticks": stats.ticks,
                "source_calls": stats.source_calls,
                "frames_captured": stats.frames_captured,
                "encodes": stats.encodes,
                "mean_encode_ms": stats.mean_encode_ms(),
                "mean_bytes": stats.mean_bytes(),
                "last_encode_us": stats.last_encode_us,
                "last_encode_bytes": stats.last_encode_bytes,
                "frames_sent": stats.frames_sent,
                "events_sent": stats.events_sent,
                "events_dropped": stats.events_dropped,
                "subscribers_dropped": stats.subscribers_dropped,
                "frames_dropped_latest": stats.frames_dropped_latest,
                "subscribers": stats.subscribers,
            },
        })
    }

    /// Stop the threads and remove the socket. Idempotent.
    pub fn shutdown(&self) {
        let already = self.inner.shutdown.swap(true, Ordering::AcqRel);
        if !already {
            // Dev-only measurement seam (never in user docs): the producer's own
            // counters, so an A/B can report the encoder's per-frame ms/bytes.
            // Written here, synchronously, because the tap thread may be killed
            // by process exit before it notices the shutdown.
            if let Some(path) = std::env::var_os("ACOS_VIEW_STATS_FILE") {
                let _ = std::fs::write(path, self.stats_json().to_string());
            }
        }
        self.inner.notify();
        // Wake the blocking accept with a self-connection.
        let _ = UnixStream::connect(&self.path);
        let _ = std::fs::remove_file(&self.path);
    }
}

impl Drop for LiveView {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// The accept loop: a blocking `accept()` so an unviewed session has no idle
/// poll cost at all.
fn accept_loop(listener: UnixListener, inner: Arc<Inner>, path: PathBuf) {
    loop {
        if inner.shutdown.load(Ordering::Acquire) {
            break;
        }
        match listener.accept() {
            Ok((stream, _)) => {
                if inner.shutdown.load(Ordering::Acquire) {
                    break;
                }
                let subscriber_inner = Arc::clone(&inner);
                std::thread::Builder::new()
                    .name("acos-live-view-subscribe".to_string())
                    .spawn(move || subscribe(stream, subscriber_inner))
                    .ok();
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => {
                if inner.shutdown.load(Ordering::Acquire) {
                    break;
                }
                // A transient accept error must not spin.
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    }
    let _ = std::fs::remove_file(&path);
}

/// Read one subscribe line, answer with the hello, attach the subscriber.
fn subscribe(mut stream: UnixStream, inner: Arc<Inner>) {
    let _ = stream.set_nonblocking(false);
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let mut line = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        match stream.read(&mut byte) {
            Ok(0) => return,
            Ok(_) => {
                if byte[0] == b'\n' {
                    break;
                }
                line.push(byte[0]);
                if line.len() > MAX_SUBSCRIBE_LINE {
                    return;
                }
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => return,
        }
    }
    let Ok(request) = serde_json::from_slice::<serde_json::Value>(&line) else {
        return;
    };
    if let Some(version) = request.get("v").and_then(serde_json::Value::as_u64) {
        if version != u64::from(PROTOCOL_VERSION) {
            // A future consumer is refused loudly rather than silently misread.
            let message = serde_json::json!({
                "v": PROTOCOL_VERSION,
                "error": format!("unsupported protocol version {version}"),
            });
            let _ = stream.set_nonblocking(true);
            let _ = stream.write_all(&frame_message(TYPE_EVENT, message.to_string().as_bytes()));
            return;
        }
    }
    let _ = inner.attach(stream, &request);
}

/// The writer thread: rate-limited, latest-wins, one encode per frame.
fn writer_loop(inner: Arc<Inner>) {
    let mut next_at = Instant::now();
    loop {
        if inner.shutdown.load(Ordering::Acquire) {
            return;
        }
        if inner.subscriber_count.load(Ordering::Acquire) == 0 {
            inner.wait(Duration::from_millis(500));
            continue;
        }
        // Events first: an action feed is never delayed behind a frame.
        for event in inner.drain_events() {
            let message = frame_message(TYPE_EVENT, event.to_string().as_bytes());
            inner.broadcast(&message, false);
            inner.events_sent.fetch_add(1, Ordering::Relaxed);
        }
        let now = Instant::now();
        if now < next_at {
            // Keep the newest slot (latest wins) while the rate limiter runs.
            inner.wait(next_at - now);
            continue;
        }
        let pending = {
            let mut slot = inner.slot.lock().unwrap_or_else(|e| e.into_inner());
            match slot.as_ref() {
                Some(frame) if frame.seq > inner.last_encoded_seq.load(Ordering::Relaxed) => {
                    slot.take()
                }
                _ => None,
            }
        };
        let Some(frame) = pending else {
            inner.wait(Duration::from_millis(20));
            continue;
        };
        let started = Instant::now();
        let encoded = encode(&frame, inner.format(), inner.config.jpeg_quality);
        let encode_us = started.elapsed().as_micros() as u64;
        inner.last_encoded_seq.store(frame.seq, Ordering::Relaxed);
        next_at = Instant::now() + inner.interval();
        let Ok(bytes) = encoded else {
            continue;
        };
        inner.encodes.fetch_add(1, Ordering::Relaxed);
        inner
            .encode_us_total
            .fetch_add(encode_us, Ordering::Relaxed);
        inner
            .encode_bytes_total
            .fetch_add(bytes.len() as u64, Ordering::Relaxed);
        inner
            .last_encode_us
            .store(encode_us, Ordering::Relaxed);
        inner
            .last_encode_bytes
            .store(bytes.len() as u64, Ordering::Relaxed);
        let header = serde_json::json!({
            "seq": frame.seq,
            "generation": frame.generation,
            "w": frame.width,
            "h": frame.height,
            "format": inner.format().as_str(),
            "ts_ms": frame.ts_ms,
        });
        let mut message = Vec::with_capacity(128 + bytes.len());
        message.extend_from_slice(&header.to_string().into_bytes());
        message.push(b'\n');
        message.extend_from_slice(&bytes);
        let message = frame_message(TYPE_FRAME, &message);
        inner.broadcast(&message, true);
        inner.frames_sent.fetch_add(1, Ordering::Relaxed);
    }
}

/// Frame one protocol message: 4-byte big-endian length of `type + payload`,
/// then the type byte, then the payload.
#[must_use]
pub fn frame_message(type_byte: u8, payload: &[u8]) -> Vec<u8> {
    let length = (payload.len() + 1) as u32;
    let mut message = Vec::with_capacity(payload.len() + 5);
    message.extend_from_slice(&length.to_be_bytes());
    message.push(type_byte);
    message.extend_from_slice(payload);
    message
}

/// Read one protocol message from `reader` (the consumer side of the framing).
///
/// Returns `(type, payload)`. Used by the tests and by the probe script's Rust
/// sibling; the wire format is the contract in `docs/LIVE_VIEW.md`.
pub fn read_message<R: Read>(reader: &mut R) -> io::Result<(u8, Vec<u8>)> {
    let mut length = [0u8; 4];
    reader.read_exact(&mut length)?;
    let length = u32::from_be_bytes(length) as usize;
    if length == 0 || length > MAX_MESSAGE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("protocol message length {length} is out of range"),
        ));
    }
    let mut body = vec![0u8; length];
    reader.read_exact(&mut body)?;
    let type_byte = body[0];
    body.remove(0);
    Ok((type_byte, body))
}

/// Box-filter downscale of an RGBA8 image with an arbitrary stride.
///
/// Every output pixel is the area average of its source rectangle, which is the
/// "box filter, scale 0.5" the protocol names (at scale 0.5 each output pixel
/// averages 2x2 source pixels). Returns `(width, height, rgba)`.
#[must_use]
pub fn downscale_box(
    rgba: &[u8],
    width: u32,
    height: u32,
    stride: u32,
    scale: f32,
) -> (u32, u32, Vec<u8>) {
    let scale = scale.clamp(0.05, 1.0);
    let out_w = ((width as f32 * scale).round() as u32).clamp(1, width.max(1));
    let out_h = ((height as f32 * scale).round() as u32).clamp(1, height.max(1));
    if out_w == width && out_h == height && stride == width {
        return (width, height, rgba.to_vec());
    }
    let stride = stride.max(width) as usize;
    let mut out = vec![0u8; out_w as usize * out_h as usize * 4];
    for oy in 0..out_h {
        let y0 = (u64::from(oy) * u64::from(height) / u64::from(out_h)) as u32;
        let y1 = ((u64::from(oy + 1) * u64::from(height) / u64::from(out_h)) as u32)
            .max(y0 + 1)
            .min(height);
        for ox in 0..out_w {
            let x0 = (u64::from(ox) * u64::from(width) / u64::from(out_w)) as u32;
            let x1 = ((u64::from(ox + 1) * u64::from(width) / u64::from(out_w)) as u32)
                .max(x0 + 1)
                .min(width);
            let mut acc = [0u32; 4];
            let mut count = 0u32;
            for y in y0..y1 {
                let row = y as usize * stride;
                for x in x0..x1 {
                    let index = (row + x as usize) * 4;
                    if index + 3 >= rgba.len() {
                        continue;
                    }
                    acc[0] += u32::from(rgba[index]);
                    acc[1] += u32::from(rgba[index + 1]);
                    acc[2] += u32::from(rgba[index + 2]);
                    acc[3] += u32::from(rgba[index + 3]);
                    count += 1;
                }
            }
            let out_index = (oy as usize * out_w as usize + ox as usize) * 4;
            if count == 0 {
                continue;
            }
            for channel in 0..4 {
                out[out_index + channel] = (acc[channel] / count) as u8;
            }
        }
    }
    (out_w, out_h, out)
}

/// Drop the alpha channel: the stream is an opaque display image, and RGB is
/// both smaller to encode and smaller on the wire.
fn rgba_to_rgb(rgba: &[u8]) -> Vec<u8> {
    let mut rgb = Vec::with_capacity(rgba.len() / 4 * 3);
    for pixel in rgba.chunks_exact(4) {
        rgb.extend_from_slice(&pixel[..3]);
    }
    rgb
}

/// Encode one downscaled frame in the configured format.
fn encode(frame: &Slot, format: FrameFormat, jpeg_quality: u8) -> io::Result<Vec<u8>> {
    let rgb = rgba_to_rgb(&frame.rgba);
    let mut out = Vec::new();
    match format {
        FrameFormat::Jpeg => {
            let mut encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(
                &mut out,
                jpeg_quality.clamp(10, 100),
            );
            encoder
                .encode(
                    &rgb,
                    frame.width,
                    frame.height,
                    image::ExtendedColorType::Rgb8,
                )
                .map_err(io::Error::other)?;
        }
        FrameFormat::Png => {
            let encoder = image::codecs::png::PngEncoder::new_with_quality(
                &mut out,
                image::codecs::png::CompressionType::Fast,
                image::codecs::png::FilterType::Adaptive,
            );
            image::ImageEncoder::write_image(
                encoder,
                    &rgb,
                    frame.width,
                    frame.height,
                    image::ExtendedColorType::Rgb8,
                )
                .map_err(io::Error::other)?;
        }
    }
    Ok(out)
}

/// `true` when the probe tree has a **focused** node that is a password field.
#[must_use]
pub fn tree_has_focused_password(tree: &serde_json::Value) -> bool {
    fn walk(node: &serde_json::Value) -> bool {
        match node {
            serde_json::Value::Array(items) => items.iter().any(walk),
            serde_json::Value::Object(map) => {
                let password = map
                    .get("password")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false);
                let focused = map
                    .get("focused")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false);
                if password && focused {
                    return true;
                }
                map.values().any(walk)
            }
            _ => false,
        }
    }
    walk(tree)
}

/// Truncate event text to [`EVENT_TEXT_CHARS`] characters (never mid-codepoint).
#[must_use]
pub fn truncate_text(text: &str) -> String {
    if text.chars().count() <= EVENT_TEXT_CHARS {
        return text.to_string();
    }
    let mut out: String = text.chars().take(EVENT_TEXT_CHARS).collect();
    out.push('…');
    out
}

/// Map one launcher reply to a protocol event, or `None` when it is not
/// interesting (a reply the feed does not describe).
///
/// `focused_password` masks typed text: the launcher's own probe tree marks
/// password fields, and a masked text never leaves the process in the feed.
#[must_use]
pub fn event_from_reply(
    reply: &serde_json::Value,
    focused_password: bool,
) -> Option<serde_json::Value> {
    let cmd = reply.get("cmd").and_then(|v| v.as_str()).unwrap_or("");
    let ok = reply
        .get("ok")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    let ts_ms = now_ms();
    let status = if ok {
        "ok".to_string()
    } else {
        reply
            .get("code")
            .and_then(|v| v.as_str())
            .unwrap_or("error")
            .to_string()
    };
    let mut event = serde_json::json!({"ts_ms": ts_ms, "status": status});
    let map = event.as_object_mut().expect("event is an object");
    match cmd {
        "tap" | "key" | "type_text" | "swipe" | "tree" | "screenshot" => {
            map.insert("kind".into(), serde_json::json!("action"));
            let wire_cmd = if cmd == "type_text" { "text" } else { cmd };
            map.insert("cmd".into(), serde_json::json!(wire_cmd));
        }
        "ready" => {
            map.insert("kind".into(), serde_json::json!("lifecycle"));
            map.insert("cmd".into(), serde_json::json!("launch"));
        }
        "close" | "reset" => {
            map.insert("kind".into(), serde_json::json!("lifecycle"));
            map.insert("cmd".into(), serde_json::json!(cmd));
        }
        "session" => {
            map.insert("kind".into(), serde_json::json!("lifecycle"));
            map.insert("cmd".into(), serde_json::json!("session"));
        }
        _ => return None,
    }
    for key in ["x", "y", "x2", "y2"] {
        if let Some(value) = reply.get(key) {
            map.insert(key.to_string(), value.clone());
        }
    }
    if let Some(key) = reply.get("key") {
        map.insert("key".into(), key.clone());
    } else if cmd == "key" {
        if let Some(code) = reply.get("code").and_then(serde_json::Value::as_i64) {
            map.insert("key".into(), serde_json::json!(code));
        }
    }
    if let Some(text) = reply.get("text").and_then(|v| v.as_str()) {
        let text = if focused_password {
            MASKED_TEXT.to_string()
        } else {
            truncate_text(text)
        };
        map.insert("text".into(), serde_json::json!(text));
    }
    if let Some(activity) = reply.get("activity").and_then(|v| v.as_str()) {
        if !activity.is_empty() {
            map.insert("activity".into(), serde_json::json!(activity));
        }
    }
    if let Some(seq) = reply.get("frame_seq") {
        map.insert("frame_seq".into(), seq.clone());
    }
    if let Some(settle) = reply.get("settle") {
        if settle
            .get("settled")
            .and_then(serde_json::Value::as_bool)
            .is_some_and(|settled| !settled)
        {
            map.insert("settle".into(), settle.clone());
        }
    }
    Some(event)
}

/// Fill an event's input fields from the command request that produced it.
///
/// A reply does not echo `x`/`y`/`key`/`text`; the request is the record of what
/// the agent asked for. Text is masked for a password field and truncated, and
/// the reply's own value (when it has one) always wins.
pub fn merge_request(
    event: &mut serde_json::Value,
    request: &serde_json::Value,
    focused_password: bool,
) {
    let Some(map) = event.as_object_mut() else {
        return;
    };
    for key in ["x", "y", "x2", "y2"] {
        if !map.contains_key(key) {
            if let Some(value) = request.get(key).filter(|value| value.is_i64()) {
                map.insert(key.to_string(), value.clone());
            }
        }
    }
    if !map.contains_key("key") {
        if let Some(value) = request.get("key").or_else(|| request.get("code")) {
            if value.is_i64() {
                map.insert("key".to_string(), value.clone());
            }
        }
    }
    if !map.contains_key("text") {
        if let Some(text) = request.get("text").and_then(|value| value.as_str()) {
            let text = if focused_password {
                MASKED_TEXT.to_string()
            } else {
                truncate_text(text)
            };
            map.insert("text".to_string(), serde_json::json!(text));
        }
    }
}

/// Build the `activity` event a session emits when the focused activity changes.
#[must_use]
pub fn activity_event(activity: &str, frame_seq: Option<u64>) -> serde_json::Value {
    let mut event = serde_json::json!({
        "ts_ms": now_ms(),
        "kind": "activity",
        "activity": activity,
        "status": "ok",
    });
    if let Some(seq) = frame_seq {
        event
            .as_object_mut()
            .expect("event is an object")
            .insert("frame_seq".into(), serde_json::json!(seq));
    }
    event
}

/// Host wall clock in milliseconds since the Unix epoch (the protocol's `ts_ms`).
#[must_use]
pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(width: u32, height: u32, rgba: [u8; 4]) -> Vec<u8> {
        rgba.iter()
            .copied()
            .cycle()
            .take(width as usize * height as usize * 4)
            .collect()
    }

    fn scratch_dir(tag: &str) -> PathBuf {
        let root = std::env::var_os("ACOS_VIEW_TEST_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                crate::repo_root()
                    .unwrap_or_else(|_| std::env::temp_dir())
                    .join(".cache/live-view-test")
            });
        let dir = root.join(format!("{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        dir
    }

    fn wait_for<F: FnMut() -> bool>(mut predicate: F, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if predicate() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        predicate()
    }

    #[test]
    fn framing_round_trips_a_hello_a_frame_and_an_event() {
        for (type_byte, payload) in [
            (TYPE_HELLO, br#"{"v":1}"#.to_vec()),
            (TYPE_FRAME, vec![b'x'; 5]),
            (TYPE_EVENT, Vec::new()),
        ] {
            let message = frame_message(type_byte, &payload);
            assert_eq!(&message[..4], &((payload.len() + 1) as u32).to_be_bytes());
            assert_eq!(message[4], type_byte);
            let (read_type, read_payload) = read_message(&mut message.as_slice()).expect("read");
            assert_eq!(read_type, type_byte);
            assert_eq!(read_payload, payload);
        }
        // A truncated frame is an error, never a silent short read.
        assert!(read_message(&mut &[0u8, 0, 0, 8, b'H'][..]).is_err());
        // A zero length is refused.
        assert!(read_message(&mut &[0u8, 0, 0, 0][..]).is_err());
    }

    #[test]
    fn no_subscriber_means_no_source_call_and_no_encode() {
        let dir = scratch_dir("cost");
        let view = LiveView::bind(
            &dir.join("stream.sock"),
            LiveViewConfig::default(),
            (1080, 2400),
            4242,
        )
        .expect("bind");
        let mut calls = 0u64;
        let mut source = || {
            calls += 1;
            Some(SourceFrame {
                seq: calls,
                generation: calls,
                width: 1080,
                height: 2400,
                stride: 1080,
                rgba: solid(1080, 2400, [0, 0, 0, 255]),
            })
        };
        for _ in 0..10 {
            view.tick(&mut source);
        }
        let stats = view.stats();
        assert_eq!(calls, 0, "the source must not be called with no subscriber");
        assert_eq!(stats.ticks, 10);
        assert_eq!(stats.source_calls, 0);
        assert_eq!(stats.encodes, 0);
        assert_eq!(stats.frames_captured, 0);
        assert!(!view.has_subscribers());
        view.shutdown();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_subscriber_receives_hello_frames_and_events() {
        let dir = scratch_dir("stream");
        let path = dir.join("stream.sock");
        let view = LiveView::bind(&path, LiveViewConfig::default(), (1080, 2400), 77).expect("bind");
        let mut client = UnixStream::connect(&path).expect("connect");
        client
            .set_read_timeout(Some(Duration::from_secs(10)))
            .expect("timeout");
        client
            .write_all(b"{\"v\":1,\"subscribe\":{\"frames\":true,\"events\":true,\"max_fps\":30,\"scale\":0.5,\"format\":\"jpeg\"}}\n")
            .expect("subscribe");
        let (type_byte, payload) = read_message(&mut client).expect("hello");
        assert_eq!(type_byte, TYPE_HELLO);
        let hello: serde_json::Value = serde_json::from_slice(&payload).expect("hello json");
        assert_eq!(hello["v"], 1);
        assert_eq!(hello["session"], 77);
        assert_eq!(hello["display"]["w"], 1080);
        assert_eq!(hello["display"]["h"], 2400);
        assert_eq!(hello["format"], "jpeg");
        assert_eq!(hello["scale"], 0.5);
        assert!(wait_for(|| view.has_subscribers(), Duration::from_secs(2)));

        let mut seq = 0u64;
        let mut source = || {
            seq += 1;
            Some(SourceFrame {
                seq,
                generation: seq,
                width: 1080,
                height: 2400,
                stride: 1080,
                rgba: solid(1080, 2400, [(seq * 40) as u8, 10, 20, 255]),
            })
        };
        view.observe_reply(&serde_json::json!({
            "ok": true, "cmd": "tap", "x": 540, "y": 1200, "status": "ok",
            "frame_seq": 3,
        }));
        let mut frames = 0;
        let mut events = 0;
        let deadline = Instant::now() + Duration::from_secs(10);
        while frames < 2 && Instant::now() < deadline {
            view.tick(&mut source);
            // Drain whatever arrived without blocking the tick loop.
            client
                .set_read_timeout(Some(Duration::from_millis(200)))
                .expect("timeout");
            match read_message(&mut client) {
                Ok((TYPE_FRAME, payload)) => {
                    let newline = payload.iter().position(|b| *b == b'\n').expect("header");
                    let header: serde_json::Value =
                        serde_json::from_slice(&payload[..newline]).expect("frame header");
                    assert_eq!(header["format"], "jpeg");
                    assert_eq!(header["w"], 540);
                    assert_eq!(header["h"], 1200);
                    assert!(header["seq"].as_u64().unwrap() >= 1);
                    assert!(!payload[newline + 1..].is_empty());
                    frames += 1;
                }
                Ok((TYPE_EVENT, payload)) => {
                    let event: serde_json::Value = serde_json::from_slice(&payload).expect("event");
                    assert_eq!(event["kind"], "action");
                    assert_eq!(event["cmd"], "tap");
                    assert_eq!(event["x"], 540);
                    assert_eq!(event["status"], "ok");
                    events += 1;
                }
                Ok((other, _)) => panic!("unexpected message type {other}"),
                Err(_) => continue,
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(frames >= 2, "expected at least two frames, got {frames}");
        assert_eq!(events, 1, "one tap event");
        let stats = view.stats();
        assert!(stats.encodes >= 2);
        assert_eq!(stats.subscribers_dropped, 0);
        view.shutdown();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_slow_subscriber_is_dropped_without_blocking_the_encoder() {
        let dir = scratch_dir("slow");
        let path = dir.join("stream.sock");
        let view = LiveView::bind(&path, LiveViewConfig::default(), (1080, 2400), 9).expect("bind");
        let mut client = UnixStream::connect(&path).expect("connect");
        client
            .set_read_timeout(Some(Duration::from_secs(5)))
            .expect("timeout");
        client
            .write_all(b"{\"v\":1,\"subscribe\":{\"frames\":true,\"events\":true,\"max_fps\":30}}\n")
            .expect("subscribe");
        let _ = read_message(&mut client).expect("hello");
        assert!(wait_for(|| view.has_subscribers(), Duration::from_secs(2)));
        // Never read: the socket buffer fills and the subscriber must be dropped
        // instead of stalling the writer or the tap.
        let mut seq = 0u64;
        let mut source = || {
            seq += 1;
            Some(SourceFrame {
                seq,
                generation: seq,
                width: 540,
                height: 1200,
                stride: 540,
                // Incompressible noise so the frames are big enough to fill the
                // socket buffer quickly.
                rgba: (0..540 * 1200 * 4).map(|i| (i as u8).wrapping_mul(37)).collect(),
            })
        };
        let started = Instant::now();
        for _ in 0..400 {
            view.tick(&mut source);
            std::thread::sleep(Duration::from_millis(1));
            if view.stats().subscribers_dropped > 0 {
                break;
            }
        }
        let elapsed = started.elapsed();
        assert!(
            view.stats().subscribers_dropped >= 1,
            "the slow subscriber must be dropped (stats: {:?})",
            view.stats()
        );
        assert!(!view.has_subscribers());
        assert!(
            elapsed < Duration::from_secs(30),
            "the tap must never block on a slow viewer (took {elapsed:?})"
        );
        view.shutdown();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn downscale_box_averages_2x2_blocks() {
        // 4x2 image: left half black, right half white.
        let mut rgba = Vec::new();
        for _y in 0..2 {
            for x in 0..4 {
                let value = if x < 2 { 0 } else { 255 };
                rgba.extend_from_slice(&[value, value, value, 255]);
            }
        }
        let (w, h, out) = downscale_box(&rgba, 4, 2, 4, 0.5);
        assert_eq!((w, h), (2, 1));
        assert_eq!(&out[..4], &[0, 0, 0, 255]);
        assert_eq!(&out[4..8], &[255, 255, 255, 255]);
        // A stride larger than the width must be honoured, not read as pixels.
        let mut padded = Vec::new();
        for _y in 0..2 {
            padded.extend_from_slice(&[10, 20, 30, 255, 10, 20, 30, 255]);
            padded.extend_from_slice(&[200, 200, 200, 200, 200, 200, 200, 200]);
        }
        let (w, h, out) = downscale_box(&padded, 2, 2, 4, 0.5);
        assert_eq!((w, h), (1, 1));
        assert_eq!(&out[..4], &[10, 20, 30, 255]);
    }

    #[test]
    fn event_text_is_truncated_and_password_text_masked() {
        let long = "a".repeat(200);
        let event = event_from_reply(
            &serde_json::json!({"ok": true, "cmd": "type_text", "text": long}),
            false,
        )
        .expect("event");
        assert_eq!(event["kind"], "action");
        assert_eq!(event["cmd"], "text");
        let text = event["text"].as_str().expect("text");
        assert_eq!(text.chars().count(), EVENT_TEXT_CHARS + 1);
        assert!(text.ends_with('…'));

        let masked = event_from_reply(
            &serde_json::json!({"ok": true, "cmd": "type_text", "text": "hunter2"}),
            true,
        )
        .expect("event");
        assert_eq!(masked["text"], MASKED_TEXT);
        assert!(!masked.to_string().contains("hunter2"));

        let key = event_from_reply(
            &serde_json::json!({"ok": true, "cmd": "key", "code": 4, "key": 4}),
            false,
        )
        .expect("event");
        assert_eq!(key["cmd"], "key");
        assert_eq!(key["key"], 4);

        let tap = event_from_reply(
            &serde_json::json!({"ok": false, "cmd": "tap", "x": 1, "y": 2, "code": "ELEMENT_OFFSCREEN"}),
            false,
        )
        .expect("event");
        assert_eq!(tap["status"], "ELEMENT_OFFSCREEN");

        assert!(event_from_reply(&serde_json::json!({"ok": true, "cmd": "logs"}), false).is_none());
    }

    #[test]
    fn focused_password_nodes_are_detected_in_the_probe_tree() {
        let tree = serde_json::json!({"nodes": [
            {"class": "android.widget.EditText", "text": "", "password": true, "focused": false},
            {"class": "android.widget.Button", "text": "Sign in", "password": false, "focused": true},
        ]});
        assert!(!tree_has_focused_password(&tree));
        let focused = serde_json::json!({"nodes": [
            {"class": "android.widget.EditText", "password": true, "focused": true},
        ]});
        assert!(tree_has_focused_password(&focused));
    }

    #[test]
    fn event_queue_drops_the_oldest_when_full() {
        let dir = scratch_dir("queue");
        let path = dir.join("stream.sock");
        let view = LiveView::bind(&path, LiveViewConfig::default(), (10, 10), 1).expect("bind");
        // No subscriber: events are discarded before the queue (zero cost).
        for index in 0..(EVENT_QUEUE * 2) {
            view.push_event(serde_json::json!({"ts_ms": index, "kind": "action"}));
        }
        assert_eq!(view.stats().events_dropped, 0);
        assert_eq!(view.queued_events().len(), 0);
        // The queue itself is bounded and drops the *oldest*: the newest
        // EVENT_QUEUE events survive.
        for index in 0..(EVENT_QUEUE + 5) {
            view.enqueue_event(serde_json::json!({"ts_ms": index, "kind": "action"}));
        }
        let queued = view.queued_events();
        assert_eq!(queued.len(), EVENT_QUEUE);
        assert_eq!(view.stats().events_dropped, 5);
        assert_eq!(queued[0]["ts_ms"], 5);
        assert_eq!(queued[EVENT_QUEUE - 1]["ts_ms"], EVENT_QUEUE + 4);
        view.shutdown();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
