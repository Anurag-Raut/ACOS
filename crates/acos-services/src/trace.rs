//! Per-call trace for the agent evidence stream.
//!
//! Tracing is **off by default** (the `ServiceHost::enable_trace` switch) so the
//! hot path costs nothing more than an `Option` check. When enabled, every
//! `transact` records the interface, decoded method, an argument summary, a
//! reply summary, the wall time in nanoseconds and any error code. This is the
//! source for the service-side half of the `acos events` stream.

use std::time::Instant;

use crate::value::Value;

/// One recorded transaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallTrace {
    /// Monotonic per-host sequence.
    pub seq: u64,
    /// AIDL interface descriptor.
    pub interface: String,
    /// Method name.
    pub method: String,
    /// Transaction code.
    pub code: u32,
    /// `true` when the proxy passed `FLAG_ONEWAY`.
    pub oneway: bool,
    /// Compact argument summary.
    pub args: String,
    /// Compact reply summary.
    pub reply: String,
    /// Wall-clock duration of the handler call.
    pub wall_ns: u64,
    /// Stable error code when the call failed.
    pub error: Option<&'static str>,
}

/// A sink for call traces.
pub trait TraceSink: Send {
    /// Record one completed transaction.
    fn record(&mut self, trace: &CallTrace);
}

/// An in-memory trace buffer (the unit-test sink).
///
/// `Arc`-backed so the test can keep a handle after the host takes ownership of
/// the sink.
#[derive(Debug, Default, Clone)]
pub struct MemoryTrace {
    calls: std::sync::Arc<std::sync::Mutex<Vec<CallTrace>>>,
}

impl MemoryTrace {
    /// A new empty buffer.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// All recorded calls.
    #[must_use]
    pub fn calls(&self) -> Vec<CallTrace> {
        self.calls.lock().unwrap().clone()
    }

    /// Number of recorded calls.
    #[must_use]
    pub fn len(&self) -> usize {
        self.calls.lock().unwrap().len()
    }

    /// True when nothing was recorded.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl TraceSink for MemoryTrace {
    fn record(&mut self, trace: &CallTrace) {
        self.calls.lock().unwrap().push(trace.clone());
    }
}

/// Summarise one value for a trace line (bounded length).
#[must_use]
pub fn summary(value: &Value) -> String {
    let raw = format!("{value:?}");
    if raw.len() > 96 {
        format!("{}…", &raw[..96])
    } else {
        raw
    }
}

/// Summarise an argument list.
#[must_use]
pub fn summary_args(args: &[Value]) -> String {
    let parts: Vec<String> = args.iter().map(summary).collect();
    format!("[{}]", parts.join(", "))
}

/// Start a timer for a handler call.
#[must_use]
pub fn start() -> Instant {
    Instant::now()
}

/// Elapsed nanoseconds, saturating.
#[must_use]
pub fn elapsed_ns(start: Instant) -> u64 {
    u64::try_from(start.elapsed().as_nanos()).unwrap_or(u64::MAX)
}

/// Emit one diagnostic line to stderr **and**, when `ACOS_TRACE_FILE` is set,
/// to that file.
///
/// The host's stderr is not part of a verify run's artifacts, so a deadlock
/// hunt needs a sink the run leaves behind. One `write` + `flush` per line
/// keeps it usable while another thread is wedged: whatever was written before
/// the hang is on disk.
///
pub fn line(message: impl AsRef<str>) {
    let message = message.as_ref();
    eprintln!("{message}");
    let Some(path) = std::env::var_os("ACOS_TRACE_FILE") else {
        return;
    };
    use std::io::Write as _;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(file, "{stamp} {message}");
        let _ = file.flush();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summaries_are_bounded() {
        let long = "x".repeat(1000);
        let s = summary(&Value::string(long));
        assert!(s.len() <= 100);
    }

    #[test]
    fn memory_trace_records() {
        let mut t = MemoryTrace::new();
        t.record(&CallTrace {
            seq: 1,
            interface: "x".into(),
            method: "y".into(),
            code: 1,
            oneway: false,
            args: "[]".into(),
            reply: "[]".into(),
            wall_ns: 5,
            error: None,
        });
        assert_eq!(t.len(), 1);
        assert_eq!(t.calls()[0].method, "y");
    }
}
