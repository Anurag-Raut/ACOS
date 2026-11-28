//! A tiny `logd` sink on `/dev/socket/logdw`.
//!
//! Real Android: liblog serialises an entry and `sendto()`s it to logd's
//! `logdw` socket; logd parses it into a log buffer. Here the *transport* is the
//! same (a `SOCK_DGRAM` unix socket at `/dev/socket/logdw`, which the sandbox
//! binds from the data dir), and the writer is `libartshim.so`, which formats
//! the message and emits one datagram per entry:
//!
//! ```text
//! [prio: u8][tag_len: u16 LE][msg_len: u16 LE][tag bytes][message bytes]
//! ```
//!
//! This replaces the "print everything to stderr with a hand-written printf"
//! interposer from the feasibility spike with a real socket + a Rust reader that
//! yields structured [`LogEntry`] values (`prio`, `tag`, `message`); the
//! launcher puts them in [`crate::RunResult::logs`] and uses them for crash
//! classification (ART's `LOG(FATAL)`/`CHECK` text arrives here).
//!
//! Why not drive the *real* liblog writer and parse logd's own datagram format?
//! Because liblog's `logd_log_msg` header is a private, versioned struct with no
//! public spec; getting it wrong loses messages silently, and the whole point of
//! the sink is that nothing is lost. See `results/art-runtime/F.md` §3.
//!
//! ## Bounded by construction (R1)
//!
//! A guest that loops on `__android_log_write` can emit tens of thousands of
//! entries per second; retaining all of them is an unbounded allocation owned by
//! the guest, and it OOM-killed `acos-art` (`results/art-runtime/R1.md`). The
//! sink therefore keeps at most [`crate::capture::DEFAULT_LOG_CAP`] (50 000)
//! entries — the **newest** ones, since that is what diagnoses a crash — drops
//! the oldest, and counts every drop. [`Logd::finish_capped`] returns the
//! retained entries together with [`LogStats`], so a truncated capture reports
//! itself instead of looking complete.

use crate::capture::log_cap_from_env;
use serde::{Deserialize, Serialize};
use std::io;
use std::os::unix::net::UnixDatagram;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

/// One captured liblog write.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogEntry {
    /// `android_LogPriority`: 2=VERBOSE 3=DEBUG 4=INFO 5=WARN 6=ERROR 7=FATAL.
    pub prio: i32,
    pub tag: String,
    pub message: String,
    /// Unix milliseconds at which the sink captured the write (the host reads
    /// the guest's frame the moment it arrives). `0` for an entry that carries
    /// none (an older dump): readers then fall back to their own read time.
    #[serde(default)]
    pub ts_ms: u64,
}

/// Wall-clock Unix milliseconds, the clock of [`LogEntry::ts_ms`].
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

impl LogEntry {
    pub fn priority_name(&self) -> &'static str {
        match self.prio {
            2 => "V",
            3 => "D",
            4 => "I",
            5 => "W",
            6 => "E",
            7 => "F",
            _ => "?",
        }
    }

    /// ART's fatal paths: `LOG(FATAL)`, `CHECK`/`CHECK_EQ` failures (tag
    /// `art_aborter`) and `abort_message`.
    pub fn is_fatal(&self) -> bool {
        self.prio >= 7
            || self.tag == "art_aborter"
            || self.tag == "abort_message"
            || self.message.contains("Check failed")
            || self.message.contains("CHECK failed")
    }
}

/// A running logd sink. Dropping it without [`Logd::finish`] still stops the
/// thread (the socket file stays in the data dir and is truncated on next start).
/// What the bounded sink did, independent of what it kept.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogStats {
    /// Entries the sink parsed (retained + dropped).
    pub total: u64,
    /// Entries retained and returned.
    pub kept: usize,
    /// Entries dropped because they fell out of the cap (oldest first).
    pub dropped: u64,
    /// The configured entry ceiling.
    pub cap: usize,
}

impl LogStats {
    #[must_use]
    pub fn truncated(&self) -> bool {
        self.dropped > 0
    }

    /// One line for a diagnostics list (never silent; a capped sink says so).
    #[must_use]
    pub fn summary(&self) -> String {
        if self.truncated() {
            format!(
                "logd: {} entries kept of {} ({} dropped, cap {} entries)",
                self.kept, self.total, self.dropped, self.cap
            )
        } else {
            format!("logd: {} entries", self.kept)
        }
    }
}

/// The retained entries plus the accounting of what they cost.
#[derive(Debug, Clone, Default)]
pub struct LogBatch {
    pub entries: Vec<LogEntry>,
    pub stats: LogStats,
    /// The first entry that fell out of the cap (a log-spam loop's first message
    /// is usually the diagnosis, and it is the part a tail capture loses).
    pub first_dropped: Option<LogEntry>,
}

/// A running logd sink. Dropping it without [`Logd::finish`] still stops the
/// thread (the socket file stays in the data dir and is truncated on next start).
pub struct Logd {
    path: PathBuf,
    entries: Arc<Mutex<Vec<LogEntry>>>,
    stats: Arc<Mutex<LogStats>>,
    first_dropped: Arc<Mutex<Option<LogEntry>>>,
    stop: Arc<AtomicBool>,
    /// W43: a connected handle used to *wake* the reader out of its 100 ms
    /// `recv` timeout. Without it, `finish_capped` waited out the current
    /// timeout on every launch: the teardown split measured 90 ms of the
    /// 110 ms guest-exit -> teardown wall in exactly that join.
    wake: UnixDatagram,
    handle: Option<JoinHandle<()>>,
    received: u64,
}

impl Logd {
    /// Bind `<dir>/logdw` and start the reader thread. `<dir>` is bound at
    /// `/dev/socket` inside the sandbox.
    pub fn start(dir: &Path) -> io::Result<Self> {
        Self::start_with_cap_and_logcat(dir, log_cap_from_env(), None)
    }

    /// Bind `<dir>/logdw` and mirror every absorbed entry directly to `logcat`
    /// (Android logcat format) so session artifacts always have a fresh
    /// `logs/logcat.txt` without waiting for an explicit `acos_logs` call.
    pub fn start_with_logcat(dir: &Path, logcat: Option<&Path>) -> io::Result<Self> {
        Self::start_with_cap_and_logcat(dir, log_cap_from_env(), logcat)
    }

    /// [`Logd::start`] with an explicit entry ceiling (tests, deliberate runs).
    pub fn start_with_cap(dir: &Path, cap: usize) -> io::Result<Self> {
        Self::start_with_cap_and_logcat(dir, cap, None)
    }

    /// [`Logd::start`] with both an explicit entry ceiling and an optional
    /// live `logs/logcat.txt` mirror file.
    pub fn start_with_cap_and_logcat(
        dir: &Path,
        cap: usize,
        logcat: Option<&Path>,
    ) -> io::Result<Self> {
        std::fs::create_dir_all(dir)?;
        let path = dir.join("logdw");
        let _ = std::fs::remove_file(&path);
        let sock = UnixDatagram::bind(&path)?;
        // H4: `finish_capped` sends the zero-length wake datagram, so this
        // timeout is only a safety bound, not the stop path. 1 s removes a
        // 10/s idle timer per session without delaying teardown.
        sock.set_read_timeout(Some(Duration::from_secs(1)))?;
        let mut mirror_file = logcat.and_then(|dest| {
            if let Some(parent) = dest.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let mut file = std::fs::File::create(dest).ok()?;
            use std::io::Write as _;
            let _ = file.write_all(b"--------- beginning of main\n");
            Some(file)
        });
        let entries: Arc<Mutex<Vec<LogEntry>>> = Arc::new(Mutex::new(Vec::new()));
        let stats = Arc::new(Mutex::new(LogStats {
            cap,
            ..LogStats::default()
        }));
        let first_dropped: Arc<Mutex<Option<LogEntry>>> = Arc::new(Mutex::new(None));
        let stop = Arc::new(AtomicBool::new(false));
        // A separate unconnected socket that can send to `path`; a zero-length
        // datagram is the wake token (it parses as no entry, so nothing is
        // fabricated).
        let wake = UnixDatagram::unbound()?;
        wake.connect(&path)?;
        let e = Arc::clone(&entries);
        let st = Arc::clone(&stats);
        let fd = Arc::clone(&first_dropped);
        let s = Arc::clone(&stop);
        let cap = cap.max(1);
        let handle = thread::spawn(move || {
            let mut buf = vec![0u8; 4096];
            let mut draining = false;
            loop {
                // A stop request shortens the blocking window; the wake
                // datagram below makes this immediate in the normal case.
                if s.load(Ordering::Relaxed) {
                    draining = true;
                    let _ = sock.set_read_timeout(Some(Duration::from_millis(5)));
                }
                match sock.recv(&mut buf) {
                    Ok(n) => {
                        if draining && n == 0 {
                            break; // the wake token: nothing left to drain
                        }
                        if let Some(entry) = parse_frame(&buf[..n]) {
                            let mut v = e.lock().unwrap();
                            let mut s = st.lock().unwrap();
                            s.total = s.total.saturating_add(1);
                            let seq = s.total;
                            // Drop in batches so a spammer does not turn every
                            // push into an O(n) memmove of the whole window.
                            if v.len() >= cap {
                                let drop_n = (v.len() / 8).max(1).min(v.len());
                                if fd.lock().unwrap().is_none() {
                                    *fd.lock().unwrap() = Some(v[0].clone());
                                }
                                v.drain(..drop_n);
                                s.dropped = s.dropped.saturating_add(drop_n as u64);
                            }
                            if let Some(file) = mirror_file.as_mut() {
                                use std::io::Write as _;
                                let level = match entry.prio {
                                    2 => 'V',
                                    3 => 'D',
                                    5 => 'W',
                                    6 | 7 => 'E',
                                    _ => 'I',
                                };
                                let _ = writeln!(
                                    file,
                                    "{:>6}  {:>5}  {} {}: {}",
                                    entry.ts_ms, seq, level, entry.tag, entry.message
                                );
                            }
                            // `ACOS_ART_LOG_TRACE=1` names the flood in-band: this
                            // is how R1's trigger (`ServiceManagerCppClient`
                            // waiting for servicemanager in a loop) was identified.
                            let trace = s.total % 10_000 == 0
                                && std::env::var_os("ACOS_ART_LOG_TRACE").is_some();
                            if trace {
                                let e = &entry;
                                eprintln!(
                                    "acos-art: logd trace: {} entries, {} dropped, cap {} — [{}] {}: {}",
                                    s.total,
                                    s.dropped,
                                    s.cap,
                                    e.priority_name(),
                                    crate::capture::head(&e.tag, 32),
                                    crate::capture::head(&e.message, 120)
                                );
                            }
                            v.push(entry);
                        }
                    }
                    Err(ref err)
                        if err.kind() == io::ErrorKind::WouldBlock
                            || err.kind() == io::ErrorKind::TimedOut =>
                    {
                        // After a stop request, drain one last time so a
                        // message written just before an abort is not lost.
                        if draining {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        });
        Ok(Self {
            path,
            entries,
            stats,
            first_dropped,
            stop,
            wake,
            handle: Some(handle),
            received: 0,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Stop the thread and return the retained entries, newest-last.
    ///
    /// The cap always applies; use [`Logd::finish_capped`] to see how much was
    /// dropped.
    pub fn finish(self) -> Vec<LogEntry> {
        self.finish_capped().entries
    }

    /// Stop the thread and return the retained entries **and** the accounting.
    pub fn finish_capped(mut self) -> LogBatch {
        self.stop.store(true, Ordering::Relaxed);
        // Wake the reader now instead of waiting out its 100 ms idle timeout.
        let _ = self.wake.send(&[]);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
        // Anything still queued in the socket buffer.
        self.drain();
        let mut v = self.entries.lock().unwrap().clone();
        self.received = v.len() as u64;
        v.shrink_to_fit();
        let mut stats = *self.stats.lock().unwrap();
        stats.kept = v.len();
        let first_dropped = self.first_dropped.lock().unwrap().take();
        LogBatch {
            entries: v,
            stats,
            first_dropped,
        }
    }

    fn drain(&self) {
        if let Ok(sock) = UnixDatagram::unbound() {
            if sock.connect(&self.path).is_ok() {
                let _ = sock.set_read_timeout(Some(Duration::from_millis(10)));
            }
        }
    }

    /// Live snapshot (useful while a long run is in flight).
    pub fn snapshot(&self) -> Vec<LogEntry> {
        self.entries.lock().unwrap().clone()
    }

    /// Live accounting, without stopping the sink.
    pub fn stats(&self) -> LogStats {
        let mut s = *self.stats.lock().unwrap();
        s.kept = self.entries.lock().unwrap().len();
        s
    }
}

fn parse_frame(buf: &[u8]) -> Option<LogEntry> {
    // Our shim's frame: exact length match is the validity check.
    if buf.len() >= 6 {
        let prio = buf[0] as i32;
        let tlen = u16::from_le_bytes([buf[1], buf[2]]) as usize;
        let mlen = u16::from_le_bytes([buf[3], buf[4]]) as usize;
        if 6 + tlen + mlen == buf.len() && (1..=8).contains(&prio) {
            let tag = String::from_utf8_lossy(&buf[6..6 + tlen]).to_string();
            let message = String::from_utf8_lossy(&buf[6 + tlen..6 + tlen + mlen]).to_string();
            return Some(LogEntry {
                prio,
                tag,
                message,
                ts_ms: now_ms(),
            });
        }
    }
    // Other writers use bionic's *native* LogdLogger, which sends logd's
    // private, versioned header we do not reimplement. Their payload still
    // contains the message as a printable run, so recover it instead of
    // dropping the entry (this is how the allocator's "Scudo ERROR: internal
    // map failure" reaches us).
    let message = longest_printable_run(buf);
    let message = message.trim().to_string();
    if message.len() < 4 {
        return None;
    }
    Some(LogEntry {
        prio: 6,
        tag: "logdw".to_string(),
        message,
        ts_ms: now_ms(),
    })
}

fn longest_printable_run(buf: &[u8]) -> String {
    let mut best: &[u8] = &[];
    let mut start = None;
    for (i, b) in buf.iter().enumerate() {
        let printable = (*b >= 0x20 && *b < 0x7f) || *b == b'\n' || *b == b'\t';
        match (printable, start) {
            (true, None) => start = Some(i),
            (false, Some(s)) => {
                if i - s > best.len() {
                    best = &buf[s..i];
                }
                start = None;
            }
            _ => {}
        }
    }
    if let Some(s) = start {
        if buf.len() - s > best.len() {
            best = &buf[s..];
        }
    }
    String::from_utf8_lossy(best).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_roundtrip() {
        let mut f = vec![6u8, 3, 0, 5, 0];
        f.push(0); // pad so the tag starts at offset 6
        f.extend_from_slice(b"foo");
        f.extend_from_slice(b"hello");
        let e = parse_frame(&f).unwrap();
        assert_eq!(e.prio, 6);
        assert_eq!(e.tag, "foo");
        assert_eq!(e.message, "hello");
        assert!(e.ts_ms > 0, "parsed log frames carry a capture timestamp");
        assert_eq!(e.priority_name(), "E");
        assert!(!e.is_fatal());
    }

    /// The R1 regression: a guest that loops on liblog must not be able to grow
    /// the launcher's heap. 200 000 entries through a 1 000-entry sink keep
    /// exactly the cap (the newest ones) and account for every drop.
    #[test]
    fn spammer_is_capped_and_drop_oldest() {
        let dir = std::env::temp_dir().join(format!("acos-art-logd-spam-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let logd = Logd::start_with_cap(&dir, 1000).unwrap();
        let path = logd.path().to_path_buf();
        let s = UnixDatagram::unbound().unwrap();
        s.connect(&path).unwrap();
        let n: u32 = 200_000;
        for i in 0..n {
            let msg = format!("spam {i}");
            let mut f = vec![6u8, 3, 0];
            f.extend_from_slice(&(msg.len() as u16).to_le_bytes());
            f.push(0);
            f.extend_from_slice(b"art");
            f.extend_from_slice(msg.as_bytes());
            // The socket buffer is finite: a burst can drop datagrams at the
            // transport, which is why the assertions are about the host-side
            // ceiling and the accounting, not about n.
            let _ = s.send(&f);
            if i % 64 == 0 {
                std::thread::sleep(Duration::from_micros(50));
            }
        }
        std::thread::sleep(Duration::from_millis(200));
        let batch = logd.finish_capped();
        assert!(
            batch.entries.len() <= 1000,
            "sink kept {} entries, cap is 1000",
            batch.entries.len()
        );
        assert_eq!(batch.stats.cap, 1000);
        assert_eq!(
            batch.stats.total,
            batch.stats.kept as u64 + batch.stats.dropped
        );
        assert!(
            batch.stats.dropped > 0,
            "a spammer must trip the cap: {:?}",
            batch.stats
        );
        assert!(
            batch.first_dropped.is_some(),
            "the first dropped entry is kept as a sample"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn stopping_an_idle_sink_does_not_wait_out_the_read_timeout() {
        // W43 teardown split: guest-exit -> teardown was 110 ms p50 and 90 ms of
        // it was this join waiting out the reader's 100 ms idle `recv` timeout.
        // The wake datagram makes the stop prompt; without it this test fails.
        let dir = std::env::temp_dir().join(format!("acos-art-logd-stop-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let logd = Logd::start_with_cap(&dir, 16).unwrap();
        let t = std::time::Instant::now();
        let batch = logd.finish_capped();
        let elapsed = t.elapsed();
        assert!(batch.entries.is_empty(), "an idle sink keeps no entries");
        assert!(
            elapsed < Duration::from_millis(80),
            "stopping an idle sink took {elapsed:?}: the 100 ms idle timeout was not woken"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn listen_roundtrip() {
        let dir = std::env::temp_dir().join(format!("acos-art-logd-{}", std::process::id()));
        let logd = Logd::start(&dir).unwrap();
        let path = logd.path().to_path_buf();
        let s = UnixDatagram::unbound().unwrap();
        s.connect(&path).unwrap();
        let tag = b"art";
        let msg = b"Check failed: foo";
        let mut f = vec![7u8, tag.len() as u8, 0, msg.len() as u8, 0];
        f.push(0);
        f.extend_from_slice(tag);
        f.extend_from_slice(msg);
        s.send(&f).unwrap();
        std::thread::sleep(Duration::from_millis(150));
        let entries = logd.finish();
        assert_eq!(entries.len(), 1);
        assert!(entries[0].is_fatal());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn live_logcat_mirror_flushes_entries_without_finish() {
        let dir = std::env::temp_dir().join(format!("acos-art-logd-mirror-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let logcat = dir.join("logs/logcat.txt");
        let logd = Logd::start_with_logcat(&dir, Some(&logcat)).unwrap();
        let path = logd.path().to_path_buf();
        let s = UnixDatagram::unbound().unwrap();
        s.connect(&path).unwrap();
        let tag = b"AndroidRuntime";
        let msg = b"FATAL EXCEPTION: main";
        let mut f = vec![6u8, tag.len() as u8, 0, msg.len() as u8, 0];
        f.push(0);
        f.extend_from_slice(tag);
        f.extend_from_slice(msg);
        s.send(&f).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        let mut text = String::new();
        while std::time::Instant::now() < deadline {
            if let Ok(t) = std::fs::read_to_string(&logcat) {
                if t.contains("FATAL EXCEPTION: main") {
                    text = t;
                    break;
                }
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            text.contains("E AndroidRuntime: FATAL EXCEPTION: main"),
            "live logcat did not mirror record: {text:?}"
        );
        let _ = logd.finish();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
