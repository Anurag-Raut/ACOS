//! The per-session event stream.
//!
//! Events carry a monotonically increasing `seq` per session. `events --since-seq
//! N` returns only events with `seq > N`; the response's `next_seq` is the cursor
//! for the next call. Events are append-only and never renumbered.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// The event kinds an agent can filter on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum EventKind {
    /// Activity/application lifecycle transition.
    Lifecycle,
    /// A logcat-style log line.
    Log,
    /// An app crash.
    Crash,
    /// An app-not-responding event.
    Anr,
    /// A produced frame.
    Frame,
    /// Acknowledgement that an input action was delivered.
    InputAck,
}

impl EventKind {
    /// Stable wire string.
    pub const fn as_str(self) -> &'static str {
        match self {
            EventKind::Lifecycle => "lifecycle",
            EventKind::Log => "log",
            EventKind::Crash => "crash",
            EventKind::Anr => "anr",
            EventKind::Frame => "frame",
            EventKind::InputAck => "input-ack",
        }
    }
}

/// Activity/application lifecycle state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum LifecycleState {
    /// `Application.onCreate`.
    AppCreated,
    /// `Activity.onCreate`.
    Created,
    /// `Activity.onStart`.
    Started,
    /// `Activity.onResume`.
    Resumed,
    /// `Activity.onPause`.
    Paused,
    /// `Activity.onStop`.
    Stopped,
    /// `Activity.onSaveInstanceState`.
    SaveInstanceState,
    /// `Activity.onDestroy`.
    Destroyed,
    /// The activity/process was recreated.
    Recreated,
    /// The app process was killed by the engine/test.
    ProcessKilled,
    /// The app process started.
    ProcessStarted,
}

/// Logcat-style severity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    /// Verbose.
    Verbose,
    /// Debug.
    Debug,
    /// Info.
    Info,
    /// Warning.
    Warn,
    /// Error.
    Error,
}

/// The typed payload of an event. `kind` is the wire discriminator.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum EventBody {
    /// Lifecycle transition.
    Lifecycle {
        /// The new state.
        state: LifecycleState,
        /// Component name, when known.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        activity: Option<String>,
    },
    /// Log line.
    Log {
        /// Log tag.
        tag: String,
        /// Severity.
        level: LogLevel,
        /// Message body (no trailing newline).
        message: String,
    },
    /// App crash.
    Crash {
        /// Exception class or signal.
        exception: String,
        /// Exception message.
        message: String,
        /// Stack frames.
        #[serde(default)]
        stack: Vec<String>,
        /// True when the crash killed the process.
        fatal: bool,
    },
    /// App-not-responding.
    Anr {
        /// Human reason (`main thread blocked`, ...).
        reason: String,
        /// Activity being processed, when known.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        activity: Option<String>,
        /// Main-thread stack.
        #[serde(default)]
        stack: Vec<String>,
    },
    /// A frame was produced.
    Frame {
        /// Frame sequence number.
        frame_seq: u64,
        /// The request that caused the frame.
        cause_id: String,
        /// True when the frame reflects real UI content.
        meaningful: bool,
        /// SHA-256 of the encoded pixels.
        content_hash: String,
    },
    /// Input was delivered.
    InputAck {
        /// The request that requested the input.
        cause_id: String,
        /// The action (`tap`, `type`, `back`, ...).
        action: String,
        /// Whether the platform acknowledged delivery.
        accepted: bool,
    },
}

impl EventBody {
    /// The event kind.
    pub const fn kind(&self) -> EventKind {
        match self {
            EventBody::Lifecycle { .. } => EventKind::Lifecycle,
            EventBody::Log { .. } => EventKind::Log,
            EventBody::Crash { .. } => EventKind::Crash,
            EventBody::Anr { .. } => EventKind::Anr,
            EventBody::Frame { .. } => EventKind::Frame,
            EventBody::InputAck { .. } => EventKind::InputAck,
        }
    }
}

/// One event on a session stream.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Event {
    /// Monotonic per-session sequence number.
    pub seq: u64,
    /// Milliseconds since the Unix epoch, as reported by the runtime clock.
    pub timestamp_ms: u64,
    /// Session this event belongs to, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// Typed payload (`kind` is the discriminator).
    #[serde(flatten)]
    pub body: EventBody,
}

impl Event {
    /// Build an event.
    pub fn new(seq: u64, timestamp_ms: u64, body: EventBody) -> Self {
        Event {
            seq,
            timestamp_ms,
            session_id: None,
            body,
        }
    }

    /// Attach the session id.
    pub fn with_session_id(mut self, session_id: impl Into<String>) -> Self {
        self.session_id = Some(session_id.into());
        self
    }

    /// The event kind.
    pub const fn kind(&self) -> EventKind {
        self.body.kind()
    }

    /// Convenience: a lifecycle event.
    pub fn lifecycle(
        seq: u64,
        timestamp_ms: u64,
        state: LifecycleState,
        activity: Option<String>,
    ) -> Self {
        Event::new(seq, timestamp_ms, EventBody::Lifecycle { state, activity })
    }

    /// Convenience: a log event.
    pub fn log(
        seq: u64,
        timestamp_ms: u64,
        tag: impl Into<String>,
        level: LogLevel,
        message: impl Into<String>,
    ) -> Self {
        Event::new(
            seq,
            timestamp_ms,
            EventBody::Log {
                tag: tag.into(),
                level,
                message: message.into(),
            },
        )
    }
}

/// True when `events` have strictly increasing `seq` values.
pub fn seq_is_strictly_increasing(events: &[Event]) -> bool {
    events.windows(2).all(|pair| pair[0].seq < pair[1].seq)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event_frame(seq: u64, timestamp_ms: u64, cause_id: &str) -> Event {
        Event::new(
            seq,
            timestamp_ms,
            EventBody::Frame {
                frame_seq: seq,
                cause_id: cause_id.to_string(),
                meaningful: true,
                content_hash: "abc".to_string(),
            },
        )
    }

    #[test]
    fn wire_shape_is_flat() {
        let event = event_frame(7, 1000, "req_7");
        let value = serde_json::to_value(&event).unwrap();
        assert_eq!(value["seq"], 7);
        assert_eq!(value["kind"], "frame");
        assert_eq!(value["frame_seq"], 7);
        let back: Event = serde_json::from_value(value).unwrap();
        assert_eq!(back, event);
    }

    #[test]
    fn monotonic_helper() {
        let a = Event::lifecycle(1, 1, LifecycleState::Created, None);
        let b = Event::lifecycle(2, 2, LifecycleState::Started, None);
        assert!(seq_is_strictly_increasing(&[a.clone(), b]));
        assert!(!seq_is_strictly_increasing(&[a.clone(), a]));
    }
}
