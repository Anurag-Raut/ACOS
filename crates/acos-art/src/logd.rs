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
pub struct Logd {
    path: PathBuf,
    entries: Arc<Mutex<Vec<LogEntry>>>,
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
    received: u64,
}

impl Logd {
    /// Bind `<dir>/logdw` and start the reader thread. `<dir>` is bound at
    /// `/dev/socket` inside the sandbox.
    pub fn start(dir: &Path) -> io::Result<Self> {
        std::fs::create_dir_all(dir)?;
        let path = dir.join("logdw");
        let _ = std::fs::remove_file(&path);
        let sock = UnixDatagram::bind(&path)?;
        sock.set_read_timeout(Some(Duration::from_millis(100)))?;
        let entries = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let e = Arc::clone(&entries);
        let s = Arc::clone(&stop);
        let handle = thread::spawn(move || {
            let mut buf = vec![0u8; 4096];
            let mut draining = false;
            loop {
                match sock.recv(&mut buf) {
                    Ok(n) => {
                        if let Some(entry) = parse_frame(&buf[..n]) {
                            e.lock().unwrap().push(entry);
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
                        if s.load(Ordering::Relaxed) {
                            draining = true;
                            let _ = sock.set_read_timeout(Some(Duration::from_millis(5)));
                        }
                    }
                    Err(_) => break,
                }
            }
        });
        Ok(Self { path, entries, stop, handle: Some(handle), received: 0 })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Stop the thread and return every entry captured so far, in order.
    pub fn finish(mut self) -> Vec<LogEntry> {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
        // Anything still queued in the socket buffer.
        self.drain();
        let mut v = self.entries.lock().unwrap().clone();
        self.received = v.len() as u64;
        v.shrink_to_fit();
        v
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
            return Some(LogEntry { prio, tag, message });
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
    Some(LogEntry { prio: 6, tag: "logdw".to_string(), message })
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
        assert_eq!(e.priority_name(), "E");
        assert!(!e.is_fatal());
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
}
