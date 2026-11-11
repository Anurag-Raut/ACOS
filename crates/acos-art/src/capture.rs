//! Bounded capture of everything a guest can emit.
//!
//! A guest is untrusted code: it can print to stdout/stderr, write liblog entries
//! and answer binder transactions faster than the host drains them. Capturing
//! that output with `read_to_end`/`push` is an unbounded allocation owned by the
//! guest, and it is how `acos-art` was OOM-killed (three times: 6.2 GB inside a
//! 6 GB guard, then 36 GB anon-rss unguarded, taking the whole desktop session
//! down — see `results/art-runtime/R1.md`).
//!
//! This module is the one place that owns the ceilings:
//!
//! * [`TailBuffer`] — a **chunked ring** that keeps the last `cap` bytes of a
//!   stream and counts what it dropped. Dropping whole 64 KiB chunks from the
//!   front keeps `push` O(bytes) with no memmove of the retained window, so a
//!   flooding guest costs only the copy, never growth. The retained text is
//!   returned with an explicit `[... N bytes truncated ...]` marker, so a
//!   truncated capture can never be mistaken for a complete one.
//! * [`DEFAULT_STREAM_CAP`] (8 MiB) — per captured stream (stdout, stderr, the
//!   non-protocol stdout lines a session keeps).
//! * [`DEFAULT_LOG_CAP`] (50 000 entries) — the logd sink keeps the newest
//!   entries and drops the oldest, counting the drops.
//!
//! The caps are overridable for a deliberate experiment
//! (`ACOS_ART_STREAM_CAP`, `ACOS_ART_LOG_CAP`), but never removable: a `0`
//! disables the override, not the ceiling.

use std::collections::VecDeque;
use std::io::Read;

/// Default per-stream capture ceiling: 8 MiB.
pub const DEFAULT_STREAM_CAP: usize = 8 * 1024 * 1024;

/// Default logd entry ceiling: 50 000 entries (drop-oldest).
pub const DEFAULT_LOG_CAP: usize = 50_000;

/// Retained-window chunk size. Chunks are dropped whole, so this is also the
/// granularity at which the retained window may exceed/undershoot the cap by at
/// most one chunk.
const CHUNK: usize = 64 * 1024;

/// The per-stream capture ceiling, from `ACOS_ART_STREAM_CAP` (bytes, `K`/`M`/`G`
/// suffixes) or [`DEFAULT_STREAM_CAP`]. `0` disables the *override*; the ceiling
/// itself always applies.
#[must_use]
pub fn stream_cap_from_env() -> usize {
    std::env::var("ACOS_ART_STREAM_CAP")
        .ok()
        .and_then(|s| parse_bytes(&s))
        .filter(|v| *v > 0)
        .unwrap_or(DEFAULT_STREAM_CAP)
}

/// The logd entry ceiling, from `ACOS_ART_LOG_CAP` or [`DEFAULT_LOG_CAP`].
/// `0` means "use the default".
#[must_use]
pub fn log_cap_from_env() -> usize {
    std::env::var("ACOS_ART_LOG_CAP")
        .ok()
        .and_then(|s| s.trim().parse::<usize>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(DEFAULT_LOG_CAP)
}

/// Parse a byte count with an optional `K`/`M`/`G` suffix (case-insensitive).
#[must_use]
pub fn parse_bytes(s: &str) -> Option<usize> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    let (num, mult) = match s.chars().last()?.to_ascii_uppercase() {
        'K' => (&s[..s.len() - 1], 1024usize),
        'M' => (&s[..s.len() - 1], 1024 * 1024),
        'G' => (&s[..s.len() - 1], 1024 * 1024 * 1024),
        _ => (s, 1),
    };
    num.trim().parse::<usize>().ok().map(|n| n.saturating_mul(mult))
}

/// The last `cap` bytes of a stream, with drop accounting.
///
/// The invariant is `len() <= cap + CHUNK` (whole chunks are evicted) and
/// `total() == len() + dropped()`.
#[derive(Debug, Clone)]
pub struct TailBuffer {
    cap: usize,
    chunks: VecDeque<Vec<u8>>,
    len: usize,
    total: u64,
    dropped: u64,
    /// The head of the evicted region, so a truncated capture still says how it
    /// started (the first line of a log-spam loop is usually the signal).
    head_sample: Vec<u8>,
}

impl TailBuffer {
    /// A buffer that retains at most `cap` bytes (a floor of one chunk keeps the
    /// ring well-defined for tiny caps).
    #[must_use]
    pub fn with_cap(cap: usize) -> Self {
        Self {
            cap: cap.max(CHUNK),
            chunks: VecDeque::new(),
            len: 0,
            total: 0,
            dropped: 0,
            head_sample: Vec::new(),
        }
    }

    /// A buffer with the default (8 MiB) ceiling.
    #[must_use]
    pub fn new() -> Self {
        Self::with_cap(stream_cap_from_env())
    }

    /// Append bytes, evicting whole chunks from the front while over the cap.
    pub fn push(&mut self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        self.total = self.total.saturating_add(bytes.len() as u64);
        let mut rest = bytes;
        // A single write larger than the whole cap: keep only its tail.
        if rest.len() > self.cap {
            let keep = self.cap;
            let skip = rest.len() - keep;
            self.record_dropped(&rest[..skip]);
            rest = &rest[skip..];
        }
        while !rest.is_empty() {
            let space = self.chunks.back().map_or(0, |c| CHUNK - c.len());
            if space == 0 || self.chunks.is_empty() {
                self.chunks.push_back(Vec::with_capacity(CHUNK));
                continue;
            }
            let take = space.min(rest.len());
            self.chunks
                .back_mut()
                .expect("a chunk exists")
                .extend_from_slice(&rest[..take]);
            self.len += take;
            rest = &rest[take..];
        }
        while self.len > self.cap {
            let first_len = self.chunks.front().expect("non-empty when over the cap").len();
            let evict = (self.len - self.cap).min(first_len);
            let take = self.chunks.front().expect("front exists")[..evict].to_vec();
            self.record_dropped(&take);
            self.len -= evict;
            if evict == first_len {
                self.chunks.pop_front();
            } else {
                let front = self.chunks.front_mut().expect("front exists");
                front.drain(..evict);
            }
        }
    }

    fn record_dropped(&mut self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        self.dropped = self.dropped.saturating_add(bytes.len() as u64);
        if self.head_sample.len() < 256 {
            let take = (256 - self.head_sample.len()).min(bytes.len());
            self.head_sample.extend_from_slice(&bytes[..take]);
        }
    }

    /// Retained bytes.
    #[must_use]
    pub fn len(&self) -> usize {
        self.len
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Every byte the stream produced (retained + dropped).
    #[must_use]
    pub fn total(&self) -> u64 {
        self.total
    }

    /// Bytes evicted from the head.
    #[must_use]
    pub fn dropped(&self) -> u64 {
        self.dropped
    }

    /// The configured ceiling.
    #[must_use]
    pub fn cap(&self) -> usize {
        self.cap
    }

    /// `true` when the capture is not the whole stream.
    #[must_use]
    pub fn truncated(&self) -> bool {
        self.dropped > 0
    }

    /// Retained bytes, in order.
    #[must_use]
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.len);
        for c in &self.chunks {
            out.extend_from_slice(c);
        }
        out
    }

    /// The retained text, with an explicit truncation marker when the head was
    /// evicted. Lossy: a capture is diagnostic text, never a data channel.
    #[must_use]
    pub fn to_text(&self) -> String {
        let mut out = String::new();
        if self.truncated() {
            out.push_str(&format!(
                "[acos-art: capture truncated: {} of {} bytes dropped (cap {} bytes){}]\n",
                self.dropped,
                self.total,
                self.cap,
                if self.head_sample.is_empty() {
                    String::new()
                } else {
                    format!(
                        "; first dropped bytes: {:?}",
                        String::from_utf8_lossy(&self.head_sample)
                    )
                }
            ));
        }
        out.push_str(&String::from_utf8_lossy(&self.to_bytes()));
        out
    }

    /// One line for a diagnostics list: retained, total and dropped.
    #[must_use]
    pub fn summary(&self) -> String {
        if self.truncated() {
            format!(
                "{} of {} bytes captured ({} dropped, cap {} bytes)",
                self.len, self.total, self.dropped, self.cap
            )
        } else {
            format!("{} bytes captured", self.total)
        }
    }
}

impl Default for TailBuffer {
    fn default() -> Self {
        Self::new()
    }
}

/// Drain `reader` into a [`TailBuffer`] on the calling thread until EOF. Used by
/// the pipe threads; returning the buffer (instead of a `Vec`) means the ceiling
/// is decided where the buffer is created, not where the guest happens to stop.
pub fn drain_to_tail<R: Read>(mut reader: R, cap: usize) -> TailBuffer {
    let mut tail = TailBuffer::with_cap(cap);
    let mut chunk = vec![0u8; 64 * 1024];
    loop {
        match reader.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(n) => tail.push(&chunk[..n]),
        }
    }
    tail
}

/// Marker a truncated capture starts with. Kept by [`tail_text`] so shortening a
/// capture for a result field can never hide the fact that it was truncated.
pub const TRUNCATION_MARKER: &str = "[acos-art: capture truncated:";

/// The last `n` bytes of `s` on a char boundary, **keeping the truncation marker
/// line** when `s` is a truncated capture. Never turns a capped capture into
/// something that reads as complete.
#[must_use]
pub fn tail_text(s: &str, n: usize) -> String {
    if s.len() <= n {
        return s.to_string();
    }
    let (prefix, body) = match s.starts_with(TRUNCATION_MARKER) {
        true => match s.find('\n') {
            Some(i) => (&s[..=i], &s[i + 1..]),
            None => ("", s),
        },
        false => ("", s),
    };
    let mut start = body.len().saturating_sub(n);
    while start < body.len() && !body.is_char_boundary(start) {
        start += 1;
    }
    format!("{prefix}…{}", &body[start..])
}

/// The first `max` characters of `s`, for diagnostics that must stay one line.
#[must_use]
pub fn head(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_bytes_accepts_suffixes() {
        assert_eq!(parse_bytes("1024"), Some(1024));
        assert_eq!(parse_bytes("8M"), Some(8 * 1024 * 1024));
        assert_eq!(parse_bytes(" 1g "), Some(1024 * 1024 * 1024));
        assert_eq!(parse_bytes("2K"), Some(2048));
        assert_eq!(parse_bytes("nope"), None);
    }

    #[test]
    fn small_stream_is_kept_whole_without_a_marker() {
        let mut b = TailBuffer::with_cap(DEFAULT_STREAM_CAP);
        b.push(b"hello ");
        b.push(b"world");
        assert_eq!(b.len(), 11);
        assert_eq!(b.total(), 11);
        assert_eq!(b.dropped(), 0);
        assert!(!b.truncated());
        assert_eq!(b.to_text(), "hello world");
    }

    #[test]
    fn flood_keeps_the_tail_and_counts_the_drops() {
        let mut b = TailBuffer::with_cap(2 * CHUNK);
        for i in 0..20_000u32 {
            b.push(format!("line {i}\n").as_bytes());
        }
        assert!(b.truncated());
        assert!(b.len() <= 2 * CHUNK + CHUNK, "len {} over cap", b.len());
        assert_eq!(b.len() as u64 + b.dropped(), b.total());
        let text = b.to_text();
        assert!(text.starts_with("[acos-art: capture truncated:"), "{text:.80}");
        // The newest line survives: a tail capture is what diagnoses a crash.
        assert!(text.ends_with("line 19999\n"), "{:.80}", &text[text.len() - 40..]);
    }

    #[test]
    fn a_single_write_bigger_than_the_cap_keeps_only_its_tail() {
        let mut b = TailBuffer::with_cap(CHUNK);
        let big = vec![b'x'; CHUNK * 4 + 7];
        b.push(&big);
        assert!(b.len() <= CHUNK + CHUNK);
        assert_eq!(b.len() as u64 + b.dropped(), b.total());
        assert!(b.truncated());
        let tail = b.to_bytes();
        assert!(tail.iter().all(|c| *c == b'x'));
    }

    #[test]
    fn memory_stays_bounded_under_a_sustained_flood() {
        // 512 MiB of spam through an 8 MiB cap must not retain more than the cap
        // (+ one chunk) — this is the OOM regression, at unit-test speed.
        let mut b = TailBuffer::with_cap(DEFAULT_STREAM_CAP);
        let line = vec![b'a'; 4096];
        for _ in 0..(512 * 1024 * 1024 / line.len()) {
            b.push(&line);
        }
        assert!(b.len() <= DEFAULT_STREAM_CAP + CHUNK);
        assert_eq!(b.len() as u64 + b.dropped(), b.total());
        assert!(b.total() >= 512 * 1024 * 1024);
    }

    #[test]
    fn tail_text_keeps_the_truncation_marker() {
        let mut b = TailBuffer::with_cap(2 * CHUNK);
        for i in 0..20_000u32 {
            b.push(format!("line {i}\n").as_bytes());
        }
        let text = b.to_text();
        let short = tail_text(&text, 40);
        assert!(
            short.starts_with(TRUNCATION_MARKER),
            "shortening a capture must not hide the truncation: {short}"
        );
        assert!(short.contains("line 19999"));
        // Untruncated text is only shortened, with an explicit ellipsis.
        assert_eq!(tail_text("short", 40), "short");
        assert!(tail_text("0123456789", 4).ends_with("6789"));
    }

    #[test]
    fn drain_to_tail_enforces_the_cap_on_a_reader() {
        struct Flood(u64);
        impl Read for Flood {
            fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                let n = buf.len().min(self.0 as usize);
                buf[..n].iter_mut().for_each(|b| *b = b'z');
                self.0 -= n as u64;
                Ok(n)
            }
        }
        let tail = drain_to_tail(Flood(64 * 1024 * 1024), 1024 * 1024);
        assert_eq!(tail.total(), 64 * 1024 * 1024);
        assert!(tail.len() <= 1024 * 1024 + CHUNK);
        assert!(tail.truncated());
    }
}
