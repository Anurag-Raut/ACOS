//! A TCP stream bound to **one absolute deadline** (METRO5/METRO6).
//!
//! A per-call socket timeout bounds each read, not the exchange: a peer that
//! trickles one byte per second keeps a "5 s" request alive forever. Every read
//! and write here first re-arms the socket timeouts to the time *remaining*
//! until the deadline and fails with `TimedOut` once it has passed, so connect +
//! write + every read together never exceed the budget the caller chose.

use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::time::{Duration, Instant};

/// A stream whose every I/O call honours one deadline.
pub struct DeadlineStream {
    stream: TcpStream,
    deadline: Instant,
}

impl DeadlineStream {
    /// Connect to `addr` within whatever remains of `deadline`.
    pub fn connect(addr: &SocketAddr, deadline: Instant) -> io::Result<Self> {
        let remaining = remaining(deadline)?;
        let stream = TcpStream::connect_timeout(addr, remaining)?;
        Ok(Self { stream, deadline })
    }

    fn arm(&self) -> io::Result<()> {
        let remaining = remaining(self.deadline)?;
        self.stream.set_read_timeout(Some(remaining))?;
        self.stream.set_write_timeout(Some(remaining))
    }
}

/// The time left until `deadline`, or `TimedOut` when none is.
pub fn remaining(deadline: Instant) -> io::Result<Duration> {
    let left = deadline.saturating_duration_since(Instant::now());
    if left.is_zero() {
        return Err(io::Error::new(io::ErrorKind::TimedOut, "deadline exceeded"));
    }
    Ok(left)
}

impl Read for DeadlineStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.arm()?;
        self.stream.read(buf)
    }
}

impl Write for DeadlineStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.arm()?;
        self.stream.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.stream.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A peer that sends one byte every 100 ms never gets past the deadline,
    /// although every single read is well under its own timeout.
    #[test]
    fn a_trickling_peer_is_cut_at_the_deadline() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            let (mut peer, _) = listener.accept().unwrap();
            for _ in 0..100 {
                if peer.write_all(b"x").is_err() {
                    return;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        });
        let started = Instant::now();
        let mut stream =
            DeadlineStream::connect(&addr, Instant::now() + Duration::from_millis(600)).unwrap();
        let mut sink = Vec::new();
        let error = stream.read_to_end(&mut sink).unwrap_err();
        assert!(
            matches!(error.kind(), io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock),
            "{error:?}"
        );
        assert!(started.elapsed() < Duration::from_millis(1500), "{:?}", started.elapsed());
        assert!(!sink.is_empty(), "bytes before the deadline were read");
    }
}
