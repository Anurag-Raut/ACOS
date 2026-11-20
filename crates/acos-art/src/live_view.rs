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
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
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
/// Subscribe lines served at once. A connection beyond this cap is closed
/// immediately instead of getting a thread, so a hostile local peer cannot
/// exhaust the host's threads by holding the socket without a newline.
pub const MAX_SUBSCRIBE_THREADS: usize = 8;
/// Total time to send the subscribe line — **one** deadline for the whole line,
/// not a per-read timeout, so a byte-at-a-time peer cannot hold a slot forever.
pub const SUBSCRIBE_DEADLINE: Duration = Duration::from_secs(5);
/// Bytes queued for a subscriber that is not reading right now. Beyond this the
/// subscriber is dropped (the producer never blocks on a viewer).
pub const MAX_QUEUED_BYTES: usize = 8 * 1024 * 1024;
/// The mode the view socket file is given right after `bind` (see
/// [`bind_private`]): owner read/write only.
pub const SOCKET_MODE: libc::mode_t = 0o600;
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
    /// `true` when a `FLAG_SECURE` window is on screen: the pixels are a blank
    /// placeholder, never the app's content (the `F` header repeats it).
    pub secure: bool,
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
    /// Taps skipped by the capture rate gate.
    pub captures_rate_limited: u64,
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

/// The condvar the tap thread waits on between frames.
///
/// The launcher implements it on top of the condvar the guest's own probe
/// samples already signal, so the tap adds no polling of its own: an unviewed
/// session has no tap thread at all, and a viewed one wakes on real frames.
pub trait FrameWake: Send + Sync {
    /// Wait until the next wake or `timeout`.
    fn wait(&self, timeout: Duration);
}

/// A frame downscaled and waiting for the encoder ("latest wins").
struct Slot {
    seq: u64,
    generation: u64,
    width: u32,
    height: u32,
    ts_ms: u64,
    rgba: Vec<u8>,
    /// The source frame was a blank `FLAG_SECURE` placeholder.
    secure: bool,
}

/// One connected viewer.
struct Subscriber {
    stream: UnixStream,
    wants_frames: bool,
    wants_events: bool,
    /// Message bytes queued for this viewer and not yet written.
    pending: Vec<u8>,
    /// How much of [`Self::pending`] has been written already.
    offset: usize,
}

impl Subscriber {
    fn new(stream: UnixStream, wants_frames: bool, wants_events: bool) -> Self {
        Self {
            stream,
            wants_frames,
            wants_events,
            pending: Vec::new(),
            offset: 0,
        }
    }

    /// Queue one message and try to write what the socket takes.
    ///
    /// The socket is non-blocking, so a partial write is **not** a dead viewer:
    /// the unwritten tail stays queued at `offset` and the next flush continues
    /// it (a frame larger than `SO_SNDBUF`, ~208 KB, would otherwise never be
    /// delivered). Only a real write error, a `WriteZero`, or crossing
    /// [`MAX_QUEUED_BYTES`] drops the subscriber, and the producer never blocks.
    fn send(&mut self, message: &[u8]) -> io::Result<()> {
        self.pending.extend_from_slice(message);
        if self.pending.len() - self.offset > MAX_QUEUED_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "the subscriber is too far behind",
            ));
        }
        self.flush()
    }

    /// Write as much of the queue as the socket accepts right now.
    fn flush(&mut self) -> io::Result<()> {
        while self.offset < self.pending.len() {
            match self.stream.write(&self.pending[self.offset..]) {
                Ok(0) => {
                    return Err(io::Error::new(
                        io::ErrorKind::WriteZero,
                        "the subscriber closed the connection",
                    ))
                }
                Ok(n) => self.offset += n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => return Ok(()),
                Err(e) => return Err(e),
            }
        }
        if self.offset == self.pending.len() {
            self.pending.clear();
            self.offset = 0;
        }
        Ok(())
    }

    /// Bytes queued but not yet written.
    fn queued(&self) -> usize {
        self.pending.len() - self.offset
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
    /// Taps skipped by the capture rate gate (the encoder would have dropped the
    /// frame anyway: it only sends `max_fps` frames per second).
    captures_rate_limited: AtomicU64,
    /// Millis since `started` at the last capture (the rate gate's clock).
    last_capture_ms: AtomicU64,
    /// Process-local monotonic origin for `last_capture_ms`.
    started: Instant,
    last_encode_us: AtomicU64,
    last_encode_bytes: AtomicU64,
    /// The focus state that decides whether typed input is masked.
    ///
    /// `None` means "not proven safe", which **masks**: masking is the default
    /// and only a tree from the reply of the command being described may
    /// unmask it (see [`LiveView::observe_request`] / [`LiveView::observe_reply`]).
    password_state: Mutex<Option<bool>>,
    /// The focused activity the feed last reported (a change emits an
    /// `activity` event).
    last_activity: Mutex<Option<String>>,
    /// The command the launcher is currently executing. The event feed reports
    /// the *request*'s coordinates/key/text (a reply need not echo them).
    pending_request: Mutex<Option<serde_json::Value>>,
    /// The launcher's frame source (one composite of the C1 state, or `None`
    /// when nothing new landed). Only called while a subscriber exists.
    source: Arc<dyn Fn() -> Option<SourceFrame> + Send + Sync>,
    /// The wake the tap thread blocks on.
    frame_wake: Arc<dyn FrameWake>,
    /// `true` while a tap thread is running (it exists only between the first
    /// subscriber and the last).
    tap_running: AtomicBool,
    /// Subscribe threads currently alive (capped at [`MAX_SUBSCRIBE_THREADS`]).
    subscribe_threads: AtomicUsize,
    /// Connections refused because that cap was reached.
    subscribe_rejected: AtomicU64,
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

    fn attach(self: &Arc<Self>, stream: UnixStream, request: &serde_json::Value) -> io::Result<()> {
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
        let mut subscriber = Subscriber::new(stream, wants_frames, wants_events);
        subscriber.send(&hello)?;
        let mut subscribers = self.subscribers.lock().unwrap_or_else(|e| e.into_inner());
        while subscribers.len() + 1 > self.config_max_subscribers() {
            // Cap: drop the oldest so a reconnecting viewer is never locked out.
            let mut oldest = subscribers.remove(0);
            let _ = oldest.flush();
            self.subscribers_dropped.fetch_add(1, Ordering::Relaxed);
            self.subscriber_count.fetch_sub(1, Ordering::AcqRel);
        }
        if subscribers.is_empty() {
            // First viewer after none: whatever focus state was remembered
            // belongs to an earlier watch; masking is the default again.
            *self
                .password_state
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = None;
        }
        subscribers.push(subscriber);
        self.subscriber_count.fetch_add(1, Ordering::AcqRel);
        drop(subscribers);
        // A viewer that arrives after another one left (or joins a running
        // stream) must see the screen *now*, not at the next redraw: the encoder
        // only sends a frame newer than the last one it sent, so on an idle
        // screen the newcomer would stare at black until something repainted.
        // Forgetting the last encoded `seq` makes the next capture go out once
        // (a duplicate for existing viewers, never a new capture rate).
        self.last_encoded_seq.store(0, Ordering::Relaxed);
        self.notify();
        // The tap thread exists only while someone watches: with no subscriber
        // the session runs exactly as it did before the viewer existed.
        self.ensure_tap();
        Ok(())
    }

    fn stats_json(&self) -> serde_json::Value {
        let stats = LiveViewStats {
            ticks: self.ticks.load(Ordering::Relaxed),
            source_calls: self.source_calls.load(Ordering::Relaxed),
            frames_captured: self.frames_captured.load(Ordering::Relaxed),
            encodes: self.encodes.load(Ordering::Relaxed),
            encode_us_total: self.encode_us_total.load(Ordering::Relaxed),
            encode_bytes_total: self.encode_bytes_total.load(Ordering::Relaxed),
            frames_sent: self.frames_sent.load(Ordering::Relaxed),
            events_sent: self.events_sent.load(Ordering::Relaxed),
            events_dropped: self.events_dropped.load(Ordering::Relaxed),
            subscribers_dropped: self.subscribers_dropped.load(Ordering::Relaxed),
            frames_dropped_latest: self.frames_dropped_latest.load(Ordering::Relaxed),
            captures_rate_limited: self.captures_rate_limited.load(Ordering::Relaxed),
            last_encode_us: self.last_encode_us.load(Ordering::Relaxed),
            last_encode_bytes: self.last_encode_bytes.load(Ordering::Relaxed),
            subscribers: self.subscriber_count.load(Ordering::Acquire) as u64,
        };
        serde_json::json!({
            "format": self.format().as_str(),
            "scale": self.scale(),
            "max_fps": self.max_fps(),
            "jpeg_quality": self.config.jpeg_quality,
            "display": {"w": self.display.0, "h": self.display.1},
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
                "captures_rate_limited": stats.captures_rate_limited,
                "subscribers": stats.subscribers,
                "subscribe_rejected": self.subscribe_rejected.load(Ordering::Relaxed),
            },
        })
    }

    fn config_max_subscribers(&self) -> usize {
        MAX_SUBSCRIBERS
    }

    /// Start the frame-tap thread if it is not running (first subscriber).
    fn ensure_tap(self: &Arc<Self>) {
        if self.tap_running.swap(true, Ordering::AcqRel) {
            return;
        }
        let inner = Arc::clone(self);
        let spawned = std::thread::Builder::new()
            .name("acos-live-view-tap".to_string())
            .spawn(move || {
                loop {
                    if inner.shutdown.load(Ordering::Acquire)
                        || inner.subscriber_count.load(Ordering::Acquire) == 0
                    {
                        break;
                    }
                    inner.tap_once();
                    inner.frame_wake.wait(Duration::from_millis(200));
                }
                inner.tap_running.store(false, Ordering::Release);
                // A subscriber may have attached while this thread was exiting;
                // it saw `tap_running == true` and did not spawn. Re-check so the
                // stream can never go silently dead.
                if !inner.shutdown.load(Ordering::Acquire)
                    && inner.subscriber_count.load(Ordering::Acquire) > 0
                {
                    inner.ensure_tap();
                }
            });
        if spawned.is_err() {
            self.tap_running.store(false, Ordering::Release);
        }
    }

    /// One tap step: the cost rule and the capture path.
    fn tap_once(&self) {
        self.ticks.fetch_add(1, Ordering::Relaxed);
        // **The cost rule**: with no subscriber this is one atomic load and a
        // return — no lock, no capture, no copy, no encode.
        if self.subscriber_count.load(Ordering::Acquire) == 0 {
            return;
        }
        // A capture is a full composite of the display (1080x2400 RGBA), so it
        // is rate-limited to the stream's own frame rate: a frame captured more
        // often than `max_fps` could never be sent (the encoder drops it as
        // "latest wins" anyway). This keeps a viewer cheap for the guest.
        let tick_ms = self.started.elapsed().as_millis() as u64;
        let interval_ms = (1000 / u64::from(self.max_fps().max(1))).max(1);
        let last_ms = self.last_capture_ms.load(Ordering::Relaxed);
        if last_ms != 0 && tick_ms.saturating_sub(last_ms) < interval_ms {
            self.captures_rate_limited.fetch_add(1, Ordering::Relaxed);
            return;
        }
        self.last_capture_ms.store(tick_ms, Ordering::Relaxed);
        self.source_calls.fetch_add(1, Ordering::Relaxed);
        let source = Arc::clone(&self.source);
        let Some(mut frame) = source() else {
            return;
        };
        if frame.secure {
            // Defence in depth (VIEW1c L1): whatever the source handed over, a
            // `FLAG_SECURE` frame's pixels never reach the encoder.
            frame.rgba.fill(0);
        }
        let scale = self.scale();
        let (width, height, rgba) =
            downscale_box(&frame.rgba, frame.width, frame.height, frame.stride, scale);
        {
            let mut slot = self.slot.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(previous) = slot.as_ref() {
                if previous.seq != frame.seq
                    && previous.seq > self.last_encoded_seq.load(Ordering::Relaxed)
                {
                    // Superseded before the encoder got to it: latest wins.
                    self.frames_dropped_latest.fetch_add(1, Ordering::Relaxed);
                }
            }
            *slot = Some(Slot {
                seq: frame.seq,
                generation: frame.generation,
                width,
                height,
                ts_ms: now_ms(),
                rgba,
                secure: frame.secure,
            });
        }
        self.frames_captured.fetch_add(1, Ordering::Relaxed);
        self.notify();
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

    /// Retry the queued tail of every subscriber that was not ready to take a
    /// whole message. Called from the writer loop while a viewer is behind, so a
    /// slow-but-alive reader still receives a message larger than `SO_SNDBUF`.
    fn flush_pending(&self) -> bool {
        let mut subscribers = self.subscribers.lock().unwrap_or_else(|e| e.into_inner());
        if subscribers
            .iter()
            .all(|subscriber| subscriber.queued() == 0)
        {
            return false;
        }
        let mut dead: Vec<usize> = Vec::new();
        for (index, subscriber) in subscribers.iter_mut().enumerate() {
            if subscriber.queued() > 0 && subscriber.flush().is_err() {
                dead.push(index);
            }
        }
        for index in dead.into_iter().rev() {
            subscribers.remove(index);
            self.subscriber_count.fetch_sub(1, Ordering::AcqRel);
            self.subscribers_dropped.fetch_add(1, Ordering::Relaxed);
        }
        subscribers.iter().any(|subscriber| subscriber.queued() > 0)
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
    /// The socket must be bound in a **host-only** directory: 0700, owned by
    /// this uid, and never bind-mounted into a guest (see [`secure_dir`]). Its
    /// mode is 0600, set with `fchmodat(dirfd, name, AT_SYMLINK_NOFOLLOW)` on the
    /// verified directory's file descriptor: never a path `chmod` (which follows
    /// a symlink swapped in after the bind) and never the process-wide `umask`
    /// (which would turn concurrently created files and directories of other
    /// threads unusable).
    pub fn bind(
        socket_path: &Path,
        config: LiveViewConfig,
        display: (u32, u32),
        session_pid: u32,
        source: Arc<dyn Fn() -> Option<SourceFrame> + Send + Sync>,
        frame_wake: Arc<dyn FrameWake>,
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
        let parent = socket_path.parent().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "the stream socket needs a parent directory",
            )
        })?;
        secure_dir(parent)?;
        // Only a socket we created may be unlinked here; a non-socket (or a
        // symlink) at the path is refused rather than followed.
        match std::fs::symlink_metadata(socket_path) {
            Ok(metadata) if metadata.file_type().is_socket() => {
                let _ = std::fs::remove_file(socket_path);
            }
            Ok(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    format!("{} exists and is not a socket", socket_path.display()),
                ))
            }
            Err(_) => {}
        }
        let listener = bind_private(socket_path)?;
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
            captures_rate_limited: AtomicU64::new(0),
            last_capture_ms: AtomicU64::new(0),
            started: Instant::now(),
            last_encode_us: AtomicU64::new(0),
            last_encode_bytes: AtomicU64::new(0),
            password_state: Mutex::new(None),
            last_activity: Mutex::new(None),
            pending_request: Mutex::new(None),
            source,
            frame_wake,
            tap_running: AtomicBool::new(false),
            subscribe_threads: AtomicUsize::new(0),
            subscribe_rejected: AtomicU64::new(0),
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

    /// One frame-tap step, driven by the caller instead of the tap thread.
    ///
    /// **The cost rule lives here**: with no subscriber this is one atomic load
    /// and a return — the source is never called, so no pixels are copied and
    /// nothing is encoded. Tests drive the tap with this; the launcher lets
    /// [`LiveView::bind`]'s own tap thread do it (it exists only while a
    /// subscriber is attached).
    pub fn tick(&self) {
        self.inner.tap_once();
    }

    /// Record the command the launcher is about to execute, so its reply's
    /// event carries the request's own coordinates/key/text.
    ///
    /// A command that carries **input** (`type_text`, `key`) drops the remembered
    /// focus state: from then on the typed value is masked unless *this* command's
    /// own reply carries a fresh tree that proves the focused field is not a
    /// password. Masking is the default; a stale tree must never unmask a secret.
    pub fn observe_request(&self, request: &serde_json::Value) {
        let carries_input = matches!(
            request.get("cmd").and_then(serde_json::Value::as_str),
            Some("type_text" | "key")
        );
        // The reset runs *before* the no-subscriber early return: a "proven
        // non-password" state recorded while a viewer was attached must not
        // survive a detach and be reused by a viewer that attaches during a
        // later input command whose reply carries no tree.
        if carries_input {
            *self
                .inner
                .password_state
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = None;
        }
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
        // **Masking is the default.** Only a tree in *this* reply can prove that
        // the focused field is not a password field; with no tree (or with a
        // state left over from an earlier command that was not consumed here) the
        // input stays masked.
        let masked = {
            let mut state = self
                .inner
                .password_state
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            match reply.get("tree") {
                Some(tree) => {
                    let masked = !tree_proves_non_password_focus(tree);
                    *state = Some(masked);
                    masked
                }
                None => state.take().unwrap_or(true),
            }
        };
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
        if let Some(mut event) = event_from_reply(reply, masked) {
            if let Some(request) = request.as_ref() {
                merge_request(&mut event, request, masked);
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
            captures_rate_limited: inner.captures_rate_limited.load(Ordering::Relaxed),
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
        self.inner.stats_json()
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

/// Create (or verify) a **host-only** directory: 0700, owned by this uid.
///
/// Used for the directory the view socket lives in. That directory must never be
/// bind-mounted into a guest (the guest is the same uid, so file modes are not a
/// boundary between the two: the mount namespace is). A directory that already
/// exists is verified *and* tightened; a directory owned by someone else, or not
/// a directory, is refused — never silently ignored.
pub fn secure_dir(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt};
    match std::fs::DirBuilder::new().mode(0o700).create(path) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error),
    }
    let metadata = std::fs::symlink_metadata(path)?;
    if !metadata.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} is not a directory", path.display()),
        ));
    }
    let uid = unsafe { libc::getuid() };
    if metadata.uid() != uid {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "{} is owned by uid {} (expected {uid}): refusing to use it",
                path.display(),
                metadata.uid()
            ),
        ));
    }
    let mode = metadata.permissions().mode() & 0o777;
    if mode != 0o700 {
        // We own it: tighten it, then verify. A failure is an error, not a
        // silent ignore (a too-open socket directory leaks the screen).
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
        let mode = std::fs::symlink_metadata(path)?.permissions().mode() & 0o777;
        if mode != 0o700 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!(
                    "{} has mode {mode:o}, could not make it 0700",
                    path.display()
                ),
            ));
        }
    }
    Ok(())
}

/// `bind(2)` a Unix listener and give the socket file mode 0600.
///
/// The directory is private (0700, ours, host-only), so the socket is never
/// reachable by another uid even in the instant between `bind` and the mode
/// change. The mode is then set **relative to a directory file descriptor** with
/// `AT_SYMLINK_NOFOLLOW`: if the path were swapped for a symlink the call fails
/// rather than changing the mode of whatever the link points at. (`fchmod` on a
/// socket fd does not touch the inode on Linux, and the process-wide `umask`
/// would race with every other thread creating files, so neither is used.)
fn bind_private(path: &Path) -> io::Result<UnixListener> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let listener = UnixListener::bind(path)?;
    let parent = path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "the socket needs a parent directory",
        )
    })?;
    let name = path
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "the socket needs a name"))?;
    let parent_c = CString::new(parent.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "NUL in the socket path"))?;
    let name_c = CString::new(name.as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "NUL in the socket name"))?;
    let dirfd = unsafe {
        libc::open(
            parent_c.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if dirfd < 0 {
        return Err(io::Error::last_os_error());
    }
    let code = unsafe {
        libc::fchmodat(
            dirfd,
            name_c.as_ptr(),
            SOCKET_MODE,
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    let error = (code != 0).then(io::Error::last_os_error);
    unsafe { libc::close(dirfd) };
    if let Some(error) = error {
        return Err(error);
    }
    let metadata = std::fs::symlink_metadata(path)?;
    let mode = metadata.permissions().mode() & 0o777;
    if !metadata.file_type().is_socket() || mode & 0o077 != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "{} was bound with mode {mode:o} (expected 0600)",
                path.display()
            ),
        ));
    }
    Ok(listener)
}

/// The uid at the other end of a connected Unix socket (`SO_PEERCRED`).
///
/// A producer only trusts a subscribe stream from its own uid: a shared zygote,
/// another session or any other local peer must not be able to read a session's
/// screen even if it reaches the path.
fn peer_uid(stream: &UnixStream) -> io::Result<u32> {
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
                // Thread cap: a peer that holds a connection without sending
                // its subscribe line must not be able to spawn threads without
                // bound (each slot is released when its thread ends).
                if inner.subscribe_threads.fetch_add(1, Ordering::AcqRel) >= MAX_SUBSCRIBE_THREADS {
                    inner.subscribe_threads.fetch_sub(1, Ordering::AcqRel);
                    inner.subscribe_rejected.fetch_add(1, Ordering::Relaxed);
                    drop(stream);
                    continue;
                }
                let subscriber_inner = Arc::clone(&inner);
                let spawned = std::thread::Builder::new()
                    .name("acos-live-view-subscribe".to_string())
                    .spawn(move || {
                        subscribe(stream, subscriber_inner);
                    });
                if spawned.is_err() {
                    inner.subscribe_threads.fetch_sub(1, Ordering::AcqRel);
                }
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
///
/// **One total deadline** for the whole line (`SUBSCRIBE_DEADLINE`), not a
/// per-read timeout: a peer sending a byte every four seconds would otherwise
/// keep the slot forever. The peer's uid must be ours (`SO_PEERCRED`) before
/// the stream is trusted with a session's screen.
fn subscribe(mut stream: UnixStream, inner: Arc<Inner>) {
    let uid = unsafe { libc::getuid() };
    match peer_uid(&stream) {
        Ok(peer) if peer == uid => {}
        Ok(peer) => {
            eprintln!("acos-art: live view: refused a subscriber from uid {peer} (own uid {uid})");
            inner.subscribe_threads.fetch_sub(1, Ordering::AcqRel);
            return;
        }
        Err(error) => {
            eprintln!(
                "acos-art: live view: refused a subscriber with no peer credentials: {error}"
            );
            inner.subscribe_threads.fetch_sub(1, Ordering::AcqRel);
            return;
        }
    }
    let deadline = Instant::now() + SUBSCRIBE_DEADLINE;
    let _ = stream.set_nonblocking(false);
    let mut line = Vec::new();
    let mut buffer = [0u8; 512];
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            inner.subscribe_threads.fetch_sub(1, Ordering::AcqRel);
            return;
        }
        if stream.set_read_timeout(Some(remaining)).is_err() {
            inner.subscribe_threads.fetch_sub(1, Ordering::AcqRel);
            return;
        }
        match stream.read(&mut buffer) {
            Ok(0) => {
                inner.subscribe_threads.fetch_sub(1, Ordering::AcqRel);
                return;
            }
            Ok(read) => {
                line.extend_from_slice(&buffer[..read]);
                if let Some(newline) = line.iter().position(|byte| *byte == b'\n') {
                    line.truncate(newline);
                    break;
                }
                if line.len() > MAX_SUBSCRIBE_LINE {
                    inner.subscribe_threads.fetch_sub(1, Ordering::AcqRel);
                    return;
                }
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => {
                inner.subscribe_threads.fetch_sub(1, Ordering::AcqRel);
                return;
            }
        }
    }
    // The slot is released once the subscribe line has been read: the attached
    // subscriber no longer needs a thread of its own (the writer thread serves
    // it).
    let Ok(request) = serde_json::from_slice::<serde_json::Value>(&line) else {
        inner.subscribe_threads.fetch_sub(1, Ordering::AcqRel);
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
            inner.subscribe_threads.fetch_sub(1, Ordering::AcqRel);
            return;
        }
    }
    let _ = inner.attach(stream, &request);
    inner.subscribe_threads.fetch_sub(1, Ordering::AcqRel);
}

/// The writer thread: rate-limited, latest-wins, one encode per frame.
fn writer_loop(inner: Arc<Inner>) {
    let mut next_at = Instant::now();
    // Dev-only measurement seam (never in user docs): while a viewer is
    // attached, refresh the stats file every 2 s, so an A/B can report the
    // encoder's own per-frame ms/bytes even when teardown kills the session
    // before it can exit cleanly.
    let stats_path = std::env::var_os("ACOS_VIEW_STATS_FILE");
    let mut stats_at = Instant::now();
    let mut was_watching = false;
    loop {
        if inner.shutdown.load(Ordering::Acquire) {
            return;
        }
        if let Some(path) = stats_path.as_ref() {
            let subscribers = inner.subscriber_count.load(Ordering::Acquire);
            // While a viewer is attached, refresh every 2 s; and once when the
            // last one leaves, so the seam can show the count back at zero
            // (a teardown write is not guaranteed: the launcher may be killed).
            let due = (subscribers > 0 && stats_at.elapsed() >= Duration::from_secs(2))
                || (subscribers == 0 && was_watching);
            if due {
                if let Ok(text) = serde_json::to_string(&inner.stats_json()) {
                    let _ = std::fs::write(path, text);
                }
                stats_at = Instant::now();
            }
            was_watching = subscribers > 0;
        }
        if inner.subscriber_count.load(Ordering::Acquire) == 0 {
            // No viewer: block until one attaches (or the session ends). No
            // periodic wakeup, no work.
            inner.wait(Duration::from_secs(60));
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
            // Keep the newest slot (latest wins) while the rate limiter runs,
            // but wake up early while a subscriber still has bytes queued.
            let mut wait = next_at - now;
            if inner.flush_pending() {
                wait = wait.min(Duration::from_millis(1));
            }
            inner.wait(wait);
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
        inner.last_encode_us.store(encode_us, Ordering::Relaxed);
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
            // A blank placeholder published while a `FLAG_SECURE` window is on
            // screen. The bytes are not the app's content.
            "secure": frame.secure,
        });
        let mut message = Vec::with_capacity(128 + bytes.len());
        message.extend_from_slice(&header.to_string().into_bytes());
        message.push(b'\n');
        message.extend_from_slice(&bytes);
        let message = frame_message(TYPE_FRAME, &message);
        inner.broadcast(&message, true);
        inner.frames_sent.fetch_add(1, Ordering::Relaxed);
        // Keep draining a viewer that could not take the whole message right
        // away: a frame larger than `SO_SNDBUF` is delivered across a few
        // writes, and the loop below retries while anything is still queued.
        inner.flush_pending();
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

/// `true` only when a **fresh** probe tree proves that input goes to a focused,
/// non-password field: some node is focused and no focused node is a password
/// field. A tree with no focused node proves nothing (focus is unknown), so the
/// answer is `false` and the input stays masked.
#[must_use]
pub fn tree_proves_non_password_focus(tree: &serde_json::Value) -> bool {
    fn walk(node: &serde_json::Value, saw_focused: &mut bool, password: &mut bool) {
        match node {
            serde_json::Value::Array(items) => {
                items
                    .iter()
                    .for_each(|item| walk(item, saw_focused, password));
            }
            serde_json::Value::Object(map) => {
                if map
                    .get("focused")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false)
                {
                    *saw_focused = true;
                    if map
                        .get("password")
                        .and_then(serde_json::Value::as_bool)
                        .unwrap_or(false)
                        || map
                            .get("password_unknown")
                            .and_then(serde_json::Value::as_bool)
                            .unwrap_or(false)
                    {
                        *password = true;
                    }
                }
                map.values()
                    .for_each(|value| walk(value, saw_focused, password));
            }
            _ => {}
        }
    }
    let (mut saw_focused, mut password) = (false, false);
    walk(tree, &mut saw_focused, &mut password);
    saw_focused && !password
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
/// `masked` masks typed text **and key codes**: the launcher's own probe tree
/// marks password fields, and nothing that could be a secret leaves the process
/// in the feed. Masking is the default (see [`LiveView::observe_reply`]).
pub fn event_from_reply(reply: &serde_json::Value, masked: bool) -> Option<serde_json::Value> {
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
        map.insert("key".into(), masked_key(key, masked));
    } else if cmd == "key" {
        if let Some(code) = reply.get("code").and_then(serde_json::Value::as_i64) {
            map.insert("key".into(), masked_key(&serde_json::json!(code), masked));
        }
    }
    if let Some(text) = reply.get("text").and_then(|v| v.as_str()) {
        let text = if masked {
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
/// the agent asked for. When `masked` (the default, see
/// [`LiveView::observe_reply`]) the typed text **and** the key code are replaced
/// with [`MASKED_TEXT`], and the reply's own value (when it has one) always wins.
pub fn merge_request(event: &mut serde_json::Value, request: &serde_json::Value, masked: bool) {
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
                map.insert("key".to_string(), masked_key(value, masked));
            }
        }
    }
    if !map.contains_key("text") {
        if let Some(text) = request.get("text").and_then(|value| value.as_str()) {
            let text = if masked {
                MASKED_TEXT.to_string()
            } else {
                truncate_text(text)
            };
            map.insert("text".to_string(), serde_json::json!(text));
        }
    }
}

/// A key code in an event: the code itself, or [`MASKED_TEXT`] when masking.
fn masked_key(value: &serde_json::Value, masked: bool) -> serde_json::Value {
    if masked {
        serde_json::json!(MASKED_TEXT)
    } else {
        value.clone()
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

    /// Root for the small files and Unix sockets these tests create.
    ///
    /// It must stay **short**: a `sun_path` is 108 bytes including the NUL, and
    /// `<worktree>/.cache/live-view-test/<pid>-<tag>/stream.sock` is over that in
    /// an agent worktree (the checkout path alone is ~70 bytes), so six of these
    /// tests failed with "stream socket path ... is too long for a unix socket"
    /// before reaching an assertion. The rule is the shared one
    /// ([`acos_test_support::short_scratch_root`]): `ACOS_VIEW_TEST_DIR` when it
    /// fits the socket budget, else the checkout path when that is short enough,
    /// else a fixed short RAM-backed directory - these tests write a few KB,
    /// never artifacts. A `ACOS_VIEW_TEST_DIR` too long for a socket is reported
    /// once and ignored instead of failing every test that binds a socket.
    fn scratch_root() -> PathBuf {
        let candidates: Vec<PathBuf> = crate::repo_root()
            .ok()
            .map(|root| vec![root.join(".cache/live-view-test")])
            .unwrap_or_default();
        acos_test_support::short_scratch_root(
            "ACOS_VIEW_TEST_DIR",
            &candidates,
            "acos-live-view-tests",
        )
    }

    /// The deepest socket these tests bind below the scratch root; the shared
    /// budget is sized from this shape (see
    /// `acos_test_support::SCRATCH_ROOT_BUDGET`).
    fn deepest_stream_socket(root: &Path) -> PathBuf {
        root.join(format!("{}-maskdetach", std::process::id()))
            .join("stream.sock")
    }

    #[test]
    fn the_scratch_root_leaves_room_for_the_stream_socket() {
        let deepest = deepest_stream_socket(&scratch_root());
        let len = deepest.as_os_str().as_encoded_bytes().len();
        assert!(
            len <= acos_test_support::SUN_PATH_MAX,
            "{} is {len} bytes, too long for sun_path ({} with the NUL)",
            deepest.display(),
            acos_test_support::SUN_PATH_MAX + 1
        );
    }

    fn scratch_dir(tag: &str) -> PathBuf {
        let dir = scratch_root().join(format!("{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        dir
    }

    /// A wake the tests can nudge (the launcher uses the guest's probe condvar).
    struct TestWake {
        tick: Mutex<u64>,
        signal: Condvar,
    }

    impl TestWake {
        fn new() -> Self {
            Self {
                tick: Mutex::new(0),
                signal: Condvar::new(),
            }
        }

        fn nudge(&self) {
            let mut tick = self.tick.lock().unwrap_or_else(|e| e.into_inner());
            *tick = tick.wrapping_add(1);
            self.signal.notify_all();
        }
    }

    impl FrameWake for TestWake {
        fn wait(&self, timeout: Duration) {
            let tick = self.tick.lock().unwrap_or_else(|e| e.into_inner());
            let _ = self
                .signal
                .wait_timeout(tick, timeout)
                .unwrap_or_else(|e| e.into_inner());
        }
    }

    /// A source the test drives by hand: `next` produces frames with a rising
    /// store counter, and every call is counted.
    struct HandSource {
        calls: AtomicU64,
        width: u32,
        height: u32,
        noise: bool,
        fill: AtomicU64,
        secure: AtomicBool,
    }

    impl HandSource {
        fn new(width: u32, height: u32, noise: bool) -> Arc<Self> {
            Arc::new(Self {
                calls: AtomicU64::new(0),
                width,
                height,
                noise,
                fill: AtomicU64::new(0),
                secure: AtomicBool::new(false),
            })
        }

        fn as_source(self: &Arc<Self>) -> Arc<dyn Fn() -> Option<SourceFrame> + Send + Sync> {
            let this = Arc::clone(self);
            Arc::new(move || {
                let seq = this.calls.fetch_add(1, Ordering::Relaxed) + 1;
                let value = this.fill.load(Ordering::Relaxed) as u8;
                let pixels = this.width as usize * this.height as usize * 4;
                let rgba = if this.noise {
                    (0..pixels).map(|i| (i as u8).wrapping_mul(37)).collect()
                } else {
                    vec![value; pixels]
                };
                Some(SourceFrame {
                    seq,
                    generation: seq,
                    width: this.width,
                    height: this.height,
                    stride: this.width,
                    rgba,
                    secure: this.secure.load(Ordering::Relaxed),
                })
            })
        }
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
        let source = HandSource::new(1080, 2400, false);
        let view = LiveView::bind(
            &dir.join("stream.sock"),
            LiveViewConfig::default(),
            (1080, 2400),
            4242,
            source.as_source(),
            Arc::new(TestWake::new()),
        )
        .expect("bind");
        for _ in 0..10 {
            view.tick();
        }
        let stats = view.stats();
        assert_eq!(
            source.calls.load(Ordering::Relaxed),
            0,
            "the source must not be called with no subscriber"
        );
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
        let source = HandSource::new(1080, 2400, false);
        let view = LiveView::bind(
            &path,
            LiveViewConfig::default(),
            (1080, 2400),
            77,
            source.as_source(),
            Arc::new(TestWake::new()),
        )
        .expect("bind");
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

        view.observe_reply(&serde_json::json!({
            "ok": true, "cmd": "tap", "x": 540, "y": 1200, "status": "ok",
            "frame_seq": 3,
        }));
        let mut frames = 0;
        let mut events = 0;
        let deadline = Instant::now() + Duration::from_secs(10);
        while frames < 2 && Instant::now() < deadline {
            source.fill.fetch_add(40, Ordering::Relaxed);
            view.tick();
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
    fn a_viewer_that_arrives_on_an_idle_screen_still_gets_the_current_frame() {
        // The screen never changes (constant store `seq`): the second viewer
        // must still be sent a frame on attach, not wait for a repaint.
        let dir = scratch_dir("idle-attach");
        let path = dir.join("stream.sock");
        let source: Arc<dyn Fn() -> Option<SourceFrame> + Send + Sync> = Arc::new(|| {
            Some(SourceFrame {
                seq: 7,
                generation: 1,
                width: 108,
                height: 240,
                stride: 108,
                rgba: vec![200u8; 108 * 240 * 4],
                secure: false,
            })
        });
        let view = LiveView::bind(
            &path,
            LiveViewConfig::default(),
            (108, 240),
            78,
            source,
            Arc::new(TestWake::new()),
        )
        .expect("bind");
        let mut first_frame_for = |label: &str| -> bool {
            let mut client = UnixStream::connect(&path).expect("connect");
            client
                .write_all(b"{\"v\":1,\"subscribe\":{\"frames\":true,\"events\":false,\"max_fps\":30,\"scale\":0.5,\"format\":\"jpeg\"}}\n")
                .expect("subscribe");
            client
                .set_read_timeout(Some(Duration::from_millis(200)))
                .expect("timeout");
            let deadline = Instant::now() + Duration::from_secs(8);
            let mut got = false;
            while !got && Instant::now() < deadline {
                view.tick();
                if let Ok((TYPE_FRAME, _)) = read_message(&mut client) {
                    got = true;
                }
            }
            // The viewer leaves. A dead subscriber is only noticed by the next
            // failed send, and an idle screen sends nothing, so it may linger:
            // the next attach's re-send is what clears it.
            let _ = label;
            drop(client);
            got
        };
        assert!(first_frame_for("first viewer"), "first viewer gets a frame");
        assert!(
            first_frame_for("second viewer"),
            "a second viewer on an unchanged screen must still get a frame"
        );
        view.shutdown();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_slow_subscriber_is_dropped_without_blocking_the_encoder() {
        let dir = scratch_dir("slow");
        let path = dir.join("stream.sock");
        let source = HandSource::new(540, 1200, true);
        let view = LiveView::bind(
            &path,
            LiveViewConfig::default(),
            (1080, 2400),
            9,
            source.as_source(),
            Arc::new(TestWake::new()),
        )
        .expect("bind");
        let mut client = UnixStream::connect(&path).expect("connect");
        client
            .set_read_timeout(Some(Duration::from_secs(5)))
            .expect("timeout");
        client
            .write_all(
                b"{\"v\":1,\"subscribe\":{\"frames\":true,\"events\":true,\"max_fps\":30}}\n",
            )
            .expect("subscribe");
        let _ = read_message(&mut client).expect("hello");
        assert!(wait_for(|| view.has_subscribers(), Duration::from_secs(2)));
        // Never read: the socket buffer fills and the subscriber must be dropped
        // instead of stalling the writer or the tap. The source produces
        // incompressible noise so the frames are big enough to fill the socket
        // buffer quickly.
        let started = Instant::now();
        for _ in 0..4000 {
            view.tick();
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
        let source = HandSource::new(10, 10, false);
        let view = LiveView::bind(
            &path,
            LiveViewConfig::default(),
            (10, 10),
            1,
            source.as_source(),
            Arc::new(TestWake::new()),
        )
        .expect("bind");
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

    fn subscribe_line(client: &mut UnixStream, extra: &str) {
        client
            .write_all(format!("{{\"v\":1,\"subscribe\":{{{extra}\"events\":true}}}}\n").as_bytes())
            .expect("subscribe");
    }

    fn bound(dir: &Path, tag: &str, source: &Arc<HandSource>) -> Arc<LiveView> {
        LiveView::bind(
            &dir.join(tag),
            LiveViewConfig::default(),
            (source.width, source.height),
            1,
            source.as_source(),
            Arc::new(TestWake::new()),
        )
        .expect("bind")
    }

    /// Read events until `n` arrived (skipping the hello and any frame).
    fn read_events(client: &mut UnixStream, n: usize) -> Vec<serde_json::Value> {
        client
            .set_read_timeout(Some(Duration::from_secs(10)))
            .expect("timeout");
        let mut out = Vec::new();
        while out.len() < n {
            match read_message(client).expect("message") {
                (TYPE_EVENT, payload) => out.push(serde_json::from_slice(&payload).expect("event")),
                _ => {}
            }
        }
        out
    }

    #[test]
    fn the_view_socket_is_private_in_a_private_directory_never_chmodded_by_path() {
        let dir = scratch_dir("private");
        // A too-open parent we own is tightened to 0700 before the bind.
        let parent = dir.join("open");
        std::fs::create_dir_all(&parent).expect("mkdir");
        std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o777)).expect("chmod");
        let source = HandSource::new(8, 8, false);
        let view = bound(&parent, "view.sock", &source);
        let dir_mode = std::fs::metadata(&parent)
            .expect("stat")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(dir_mode, 0o700, "socket directory is 0700");
        let socket_mode = std::fs::symlink_metadata(&parent.join("view.sock"))
            .expect("stat")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(
            socket_mode & 0o077,
            0,
            "socket has no group/other access: {socket_mode:o}"
        );
        view.shutdown();
        // A symlink at the socket path is refused, not followed (the old path
        // `chmod` would have changed the mode of whatever it pointed at).
        let target = dir.join("victim");
        std::fs::write(&target, b"x").expect("victim");
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o644)).expect("chmod");
        std::os::unix::fs::symlink(&target, parent.join("trap.sock")).expect("symlink");
        assert!(LiveView::bind(
            &parent.join("trap.sock"),
            LiveViewConfig::default(),
            (8, 8),
            1,
            source.as_source(),
            Arc::new(TestWake::new()),
        )
        .is_err());
        assert_eq!(
            std::fs::metadata(&target)
                .expect("stat")
                .permissions()
                .mode()
                & 0o777,
            0o644,
            "the symlink target's mode is untouched"
        );
        // A directory that is not ours / not a directory is refused.
        assert!(
            secure_dir(&target).is_err(),
            "a file is not a socket directory"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn peer_credentials_report_our_own_uid() {
        let dir = scratch_dir("cred");
        let path = dir.join("p.sock");
        let listener = UnixListener::bind(&path).expect("bind");
        let client = UnixStream::connect(&path).expect("connect");
        let (server, _) = listener.accept().expect("accept");
        let uid = unsafe { libc::getuid() };
        assert_eq!(peer_uid(&server).expect("server side"), uid);
        assert_eq!(peer_uid(&client).expect("client side"), uid);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn subscribe_threads_are_capped_and_a_trickling_peer_is_cut_off_by_one_deadline() {
        let dir = scratch_dir("cap");
        let source = HandSource::new(8, 8, false);
        let view = bound(&dir, "v.sock", &source);
        let path = view.socket_path().to_path_buf();
        // MAX_SUBSCRIBE_THREADS silent peers hold every slot...
        let mut silent: Vec<UnixStream> = (0..MAX_SUBSCRIBE_THREADS)
            .map(|_| UnixStream::connect(&path).expect("connect"))
            .collect();
        assert!(wait_for(
            || view.inner.subscribe_threads.load(Ordering::Acquire) == MAX_SUBSCRIBE_THREADS,
            Duration::from_secs(3)
        ));
        // ...so more are closed at accept, with no thread of their own.
        let mut extra = Vec::new();
        for _ in 0..6 {
            extra.push(UnixStream::connect(&path).expect("connect"));
        }
        assert!(
            wait_for(
                || view.inner.subscribe_rejected.load(Ordering::Relaxed) >= 6,
                Duration::from_secs(3)
            ),
            "connections beyond the thread cap are rejected"
        );
        assert!(view.inner.subscribe_threads.load(Ordering::Acquire) <= MAX_SUBSCRIBE_THREADS);
        // One total deadline: the silent peers are released together at about
        // SUBSCRIBE_DEADLINE, not one per read timeout.
        let started = Instant::now();
        for peer in &mut silent {
            peer.set_read_timeout(Some(SUBSCRIBE_DEADLINE + Duration::from_secs(4)))
                .expect("timeout");
            let mut byte = [0u8; 1];
            assert_eq!(
                peer.read(&mut byte).unwrap_or(0),
                0,
                "closed by the producer"
            );
        }
        assert!(
            started.elapsed() < SUBSCRIBE_DEADLINE + Duration::from_secs(3),
            "all silent peers were cut off by the total deadline: {:?}",
            started.elapsed()
        );
        // A byte-at-a-time peer does not outlive the deadline either.
        let mut trickle = UnixStream::connect(&path).expect("connect");
        let begun = Instant::now();
        let mut closed_at = None;
        for _ in 0..40 {
            if trickle.write_all(b" ").is_err() {
                closed_at = Some(begun.elapsed());
                break;
            }
            std::thread::sleep(Duration::from_millis(500));
            if begun.elapsed() > SUBSCRIBE_DEADLINE + Duration::from_secs(3) {
                break;
            }
        }
        let closed_at = closed_at.expect("the trickling peer was cut off");
        assert!(
            closed_at < SUBSCRIBE_DEADLINE + Duration::from_secs(3),
            "trickle outlived the total deadline: {closed_at:?}"
        );
        // A real subscriber still gets in afterwards (slots were released).
        assert!(wait_for(
            || view.inner.subscribe_threads.load(Ordering::Acquire) == 0,
            Duration::from_secs(3)
        ));
        let mut client = UnixStream::connect(&path).expect("connect");
        subscribe_line(&mut client, "\"frames\":false,");
        client
            .set_read_timeout(Some(Duration::from_secs(5)))
            .expect("timeout");
        assert_eq!(read_message(&mut client).expect("hello").0, TYPE_HELLO);
        drop(extra);
        view.shutdown();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn typed_input_is_masked_unless_a_fresh_tree_proves_a_non_password_focus() {
        let dir = scratch_dir("mask");
        let source = HandSource::new(8, 8, false);
        let view = bound(&dir, "v.sock", &source);
        let mut client = UnixStream::connect(view.socket_path()).expect("connect");
        subscribe_line(&mut client, "\"frames\":false,");
        client
            .set_read_timeout(Some(Duration::from_secs(5)))
            .expect("timeout");
        assert_eq!(read_message(&mut client).expect("hello").0, TYPE_HELLO);
        assert!(wait_for(|| view.has_subscribers(), Duration::from_secs(2)));

        let plain = serde_json::json!({"nodes": [
            {"class": "android.widget.EditText", "password": false, "focused": true}]});
        let password = serde_json::json!({"nodes": [
            {"class": "android.widget.EditText", "password": true, "focused": true}]});
        let nothing_focused = serde_json::json!({"nodes": [
            {"class": "android.widget.EditText", "password": false, "focused": false}]});

        let mut run = |request: serde_json::Value, reply: serde_json::Value| {
            view.observe_request(&request);
            view.observe_reply(&reply);
            read_events(&mut client, 1).remove(0)
        };
        // Default: no tree at all -> text and key code are masked.
        let event = run(
            serde_json::json!({"cmd": "type_text", "text": "hunter2"}),
            serde_json::json!({"ok": true, "cmd": "type_text"}),
        );
        assert_eq!(event["text"], MASKED_TEXT);
        assert!(!event.to_string().contains("hunter2"));
        let event = run(
            serde_json::json!({"cmd": "key", "key": 66}),
            serde_json::json!({"ok": true, "cmd": "key", "code": 66}),
        );
        assert_eq!(event["key"], MASKED_TEXT, "key codes are masked too");
        // A tree that proves a focused non-password field unmasks *that* input.
        let event = run(
            serde_json::json!({"cmd": "type_text", "text": "visible"}),
            serde_json::json!({"ok": true, "cmd": "type_text", "tree": plain}),
        );
        assert_eq!(event["text"], "visible");
        // ...and only that input: the next one has no fresh tree -> masked again.
        let event = run(
            serde_json::json!({"cmd": "type_text", "text": "stale"}),
            serde_json::json!({"ok": true, "cmd": "type_text"}),
        );
        assert_eq!(event["text"], MASKED_TEXT, "a stale tree never unmasks");
        // A focused password field, and a tree with no focus at all: masked.
        for tree in [password, nothing_focused] {
            let event = run(
                serde_json::json!({"cmd": "type_text", "text": "secret"}),
                serde_json::json!({"ok": true, "cmd": "type_text", "tree": tree}),
            );
            assert_eq!(event["text"], MASKED_TEXT);
        }
        assert!(tree_proves_non_password_focus(&plain));
        assert!(!tree_proves_non_password_focus(&serde_json::json!({})));
        view.shutdown();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_non_password_state_does_not_survive_a_detach() {
        let dir = scratch_dir("maskdetach");
        let source = HandSource::new(8, 8, false);
        let view = bound(&dir, "v.sock", &source);
        let plain = serde_json::json!({"nodes": [
            {"class": "android.widget.EditText", "password": false, "focused": true}]});
        let attach = |view: &LiveView| {
            let mut client = UnixStream::connect(view.socket_path()).expect("connect");
            subscribe_line(&mut client, "\"frames\":false,");
            client
                .set_read_timeout(Some(Duration::from_secs(5)))
                .expect("timeout");
            assert_eq!(read_message(&mut client).expect("hello").0, TYPE_HELLO);
            assert!(wait_for(|| view.has_subscribers(), Duration::from_secs(2)));
            client
        };
        // A viewer is attached and a fresh tree proves a non-password focus.
        let mut client = attach(&view);
        view.observe_request(&serde_json::json!({"cmd": "type_text", "text": "a"}));
        view.observe_reply(&serde_json::json!({"ok": true, "cmd": "type_text", "tree": plain}));
        assert_eq!(read_events(&mut client, 1).remove(0)["text"], "a");
        // Detach; a text command runs with nobody watching.
        drop(client);
        // The producer notices a closed peer on its next write: keep poking.
        assert!(wait_for(
            || {
                view.push_event(serde_json::json!({"kind": "lifecycle", "event": "poke"}));
                !view.has_subscribers()
            },
            Duration::from_secs(10)
        ));
        view.observe_request(&serde_json::json!({"cmd": "key", "key": 66}));
        // A viewer attaches mid-command; the reply carries no tree: masked.
        let mut client = attach(&view);
        view.observe_reply(&serde_json::json!({"ok": true, "cmd": "key", "code": 66}));
        let event = (0..8)
            .map(|_| read_events(&mut client, 1).remove(0))
            .find(|event| event.get("cmd") == Some(&serde_json::json!("key")))
            .expect("the key event");
        assert_eq!(
            event["key"], MASKED_TEXT,
            "a key code must not leak after a detach"
        );
        view.shutdown();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_fallback_shaped_tree_masks_a_password_or_undecidable_focus() {
        let fallback_password = serde_json::json!({"nodes": [
            {"class": "android.widget.EditText", "password": true, "focused": true,
             "source": "view-fallback"}]});
        let undecided = serde_json::json!({"nodes": [
            {"class": "android.widget.EditText", "focused": true, "password": false,
             "password_unknown": true, "source": "view-fallback"}]});
        assert!(!tree_proves_non_password_focus(&fallback_password));
        assert!(!tree_proves_non_password_focus(&undecided));
    }

    #[test]
    fn a_secure_frame_is_published_blank_and_flagged() {
        let dir = scratch_dir("secure");
        // A noisy (non-blank) source that says the screen is secure: the pixels
        // must still be all zero by the time they are encoded.
        let source = HandSource::new(64, 64, true);
        source.secure.store(true, Ordering::Relaxed);
        let view = bound(&dir, "v.sock", &source);
        let mut client = UnixStream::connect(view.socket_path()).expect("connect");
        subscribe_line(
            &mut client,
            "\"frames\":true,\"format\":\"png\",\"scale\":1.0,\"max_fps\":30,",
        );
        client
            .set_read_timeout(Some(Duration::from_millis(300)))
            .expect("timeout");
        assert_eq!(read_message(&mut client).expect("hello").0, TYPE_HELLO);
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut seen = false;
        while !seen && Instant::now() < deadline {
            view.tick();
            if let Ok((TYPE_FRAME, payload)) = read_message(&mut client) {
                let newline = payload.iter().position(|b| *b == b'\n').expect("header");
                let header: serde_json::Value =
                    serde_json::from_slice(&payload[..newline]).expect("header json");
                assert_eq!(header["secure"], true);
                let image = image::load_from_memory(&payload[newline + 1..])
                    .expect("png")
                    .to_rgb8();
                assert!(
                    image.as_raw().iter().all(|byte| *byte == 0),
                    "secure frame is blank"
                );
                seen = true;
            }
        }
        assert!(seen, "a frame arrived");
        // Turning it off publishes real pixels again.
        source.secure.store(false, Ordering::Relaxed);
        let mut real = false;
        let deadline = Instant::now() + Duration::from_secs(10);
        while !real && Instant::now() < deadline {
            view.tick();
            if let Ok((TYPE_FRAME, payload)) = read_message(&mut client) {
                let newline = payload.iter().position(|b| *b == b'\n').expect("header");
                let header: serde_json::Value =
                    serde_json::from_slice(&payload[..newline]).expect("header json");
                if header["secure"] == false {
                    let image = image::load_from_memory(&payload[newline + 1..])
                        .expect("png")
                        .to_rgb8();
                    assert!(image.as_raw().iter().any(|byte| *byte != 0));
                    real = true;
                }
            }
        }
        assert!(real, "an unsecured screen streams again");
        view.shutdown();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_frame_larger_than_the_socket_buffer_is_delivered_to_a_slow_reader() {
        let dir = scratch_dir("large");
        // 800x1200 noise at scale 1.0 PNG is ~2.9 MB: far beyond SO_SNDBUF
        // (~208 KB), so it cannot be written in one non-blocking send.
        let source = HandSource::new(800, 1200, true);
        let view = bound(&dir, "v.sock", &source);
        let mut client = UnixStream::connect(view.socket_path()).expect("connect");
        subscribe_line(
            &mut client,
            "\"frames\":true,\"format\":\"png\",\"scale\":1.0,\"max_fps\":2,",
        );
        client
            .set_read_timeout(Some(Duration::from_secs(10)))
            .expect("timeout");
        assert_eq!(read_message(&mut client).expect("hello").0, TYPE_HELLO);
        assert!(wait_for(|| view.has_subscribers(), Duration::from_secs(2)));
        view.tick();
        // The reader is slow: it does not read for a while.
        std::thread::sleep(Duration::from_millis(900));
        let (kind, payload) = loop {
            let message = read_message(&mut client).unwrap_or_else(|error| {
                panic!("a large frame arrives intact: {error} {:?}", view.stats())
            });
            if message.0 == TYPE_FRAME {
                break message;
            }
        };
        assert_eq!(kind, TYPE_FRAME);
        let newline = payload.iter().position(|b| *b == b'\n').expect("header");
        assert!(
            payload.len() > 1_000_000,
            "frame was {} bytes",
            payload.len()
        );
        let image =
            image::load_from_memory(&payload[newline + 1..]).expect("a whole, decodable PNG");
        assert_eq!((image.width(), image.height()), (800, 1200));
        assert_eq!(
            view.stats().subscribers_dropped,
            0,
            "the viewer was not dropped"
        );
        assert!(view.has_subscribers());
        view.shutdown();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
