//! Input-channel transport: a socketpair-like pair of endpoints.
//!
//! An Android `InputChannel` is a pair of `SOCK_SEQPACKET` unix sockets carrying
//! framed `InputMessage`s. The later native layer must be able to hand a real fd
//! to the Java `InputChannel`/`InputEventReceiver`, so ACOS provides two
//! implementations behind one trait:
//!
//! * [`Channel::in_memory`] — a deterministic pair of queues, used by the unit
//!   tests and the replay harness (no fds, no scheduling);
//! * [`Channel::socketpair`] — a real `AF_UNIX`/`SOCK_SEQPACKET` pair via the
//!   safe `rustix` wrappers, with the client fd available to the native layer.
//!
//! Both preserve message boundaries, which is the property the input protocol
//! depends on.

use std::collections::VecDeque;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::sync::{Arc, Mutex};

use crate::error::{Result, WmError};

/// One end of an input channel.
pub trait ChannelEnd: Send + Sync + std::fmt::Debug {
    /// Send one framed message.
    fn send(&self, frame: &[u8]) -> Result<()>;
    /// Receive one framed message, or `None` if none is pending.
    fn recv(&self) -> Result<Option<Vec<u8>>>;
    /// The descriptor the native layer can hand to Java, if any.
    fn fd(&self) -> Option<BorrowedFd<'_>>;
}

/// A bidirectional channel between the dispatcher and one window.
#[derive(Debug)]
pub struct Channel {
    /// `InputChannel.mName` (`"<window> client"`).
    pub name: String,
    /// The handle written into the `addToDisplayAsUser` reply parcel.
    pub handle: i32,
    /// Dispatcher side.
    pub dispatcher: Arc<dyn ChannelEnd>,
    /// Application side.
    pub app: Arc<dyn ChannelEnd>,
}

impl Channel {
    /// A deterministic in-memory pair. Message boundaries are preserved.
    #[must_use]
    pub fn in_memory(name: impl Into<String>, handle: i32) -> Self {
        let (d, a) = InMemoryChannel::pair();
        Self {
            name: name.into(),
            handle,
            dispatcher: Arc::new(d),
            app: Arc::new(a),
        }
    }

    /// A real `AF_UNIX`/`SOCK_SEQPACKET` pair.
    pub fn socketpair(name: impl Into<String>, handle: i32) -> Result<Self> {
        let (dispatcher, app) = FdChannel::pair()?;
        Ok(Self {
            name: name.into(),
            handle,
            dispatcher: Arc::new(dispatcher),
            app: Arc::new(app),
        })
    }

    /// The client descriptor (for `InputChannel`/`InputEventReceiver`), if this
    /// channel is fd-backed. The borrow is tied to `self`, so the caller can
    /// duplicate it into a `Parcel` without owning the raw descriptor.
    #[must_use]
    pub fn client_fd(&self) -> Option<BorrowedFd<'_>> {
        self.app.fd()
    }

    /// A duplicated client descriptor (the caller owns it). This is what the
    /// WMS writes into the `InputChannel` reply parcel: the app owns its own
    /// descriptor while the dispatcher keeps the channel's server end.
    #[must_use]
    pub fn duplicate_client_fd(&self) -> Option<OwnedFd> {
        let fd = self.app.fd()?;
        rustix::io::dup(fd).ok()
    }
}

// ---------------------------------------------------------------------------
// in-memory
// ---------------------------------------------------------------------------

type Queue = Arc<Mutex<VecDeque<Vec<u8>>>>;

/// One end of an in-memory queue pair.
#[derive(Debug)]
pub struct InMemoryChannel {
    rx: Queue,
    tx: Queue,
}

impl InMemoryChannel {
    /// Create both ends.
    #[must_use]
    pub fn pair() -> (Self, Self) {
        let a_to_b: Queue = Arc::new(Mutex::new(VecDeque::new()));
        let b_to_a: Queue = Arc::new(Mutex::new(VecDeque::new()));
        (
            Self {
                rx: b_to_a.clone(),
                tx: a_to_b.clone(),
            },
            Self {
                rx: a_to_b,
                tx: b_to_a,
            },
        )
    }
}

impl ChannelEnd for InMemoryChannel {
    fn send(&self, frame: &[u8]) -> Result<()> {
        self.tx
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push_back(frame.to_vec());
        Ok(())
    }

    fn recv(&self) -> Result<Option<Vec<u8>>> {
        Ok(self
            .rx
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .pop_front())
    }

    fn fd(&self) -> Option<BorrowedFd<'_>> {
        None
    }
}

// ---------------------------------------------------------------------------
// socketpair
// ---------------------------------------------------------------------------

/// A `SOCK_SEQPACKET` endpoint. Reads are non-blocking.
#[derive(Debug)]
pub struct FdChannel {
    fd: OwnedFd,
}

impl FdChannel {
    /// Create an `AF_UNIX`/`SOCK_SEQPACKET` pair.
    pub fn pair() -> Result<(Self, Self)> {
        let (a, b) = rustix::net::socketpair(
            rustix::net::AddressFamily::UNIX,
            rustix::net::SocketType::SEQPACKET,
            rustix::net::SocketFlags::CLOEXEC,
            None,
        )
        .map_err(|e| WmError::Fd(format!("socketpair: {e}")))?;
        // Non-blocking recv on both ends keeps the state machine timer-free.
        for fd in [&a, &b] {
            let flags = rustix::fs::fcntl_getfl(fd).map_err(|e| WmError::Fd(format!("getfl: {e}")))?;
            rustix::fs::fcntl_setfl(fd, flags | rustix::fs::OFlags::NONBLOCK)
                .map_err(|e| WmError::Fd(format!("setfl: {e}")))?;
        }
        Ok((Self { fd: a }, Self { fd: b }))
    }
}

impl ChannelEnd for FdChannel {
    fn send(&self, frame: &[u8]) -> Result<()> {
        let n = rustix::net::send(&self.fd, frame, rustix::net::SendFlags::NOSIGNAL)
            .map_err(|e| WmError::Fd(format!("send: {e}")))?;
        if n != frame.len() {
            return Err(WmError::Fd(format!("short seqpacket send {n}/{}", frame.len())));
        }
        Ok(())
    }

    fn recv(&self) -> Result<Option<Vec<u8>>> {
        let mut buf = vec![0u8; 4096];
        match rustix::net::recv(&self.fd, &mut buf[..], rustix::net::RecvFlags::DONTWAIT) {
            Ok((_, 0)) => Ok(None),
            Ok((_, n)) => Ok(Some(buf[..n].to_vec())),
            Err(rustix::io::Errno::AGAIN) => Ok(None),
            Err(e) => Err(WmError::Fd(format!("recv: {e}"))),
        }
    }

    fn fd(&self) -> Option<BorrowedFd<'_>> {
        Some(self.fd.as_fd())
    }
}

// ---------------------------------------------------------------------------
// registry
// ---------------------------------------------------------------------------

/// Owns the channels the WMS has handed out, keyed by the handle written into
/// the `InputChannel` parcel.
#[derive(Debug, Default)]
pub struct InputChannelRegistry {
    channels: Vec<Option<Channel>>,
}

impl InputChannelRegistry {
    /// An empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Allocate a deterministic small handle and store `channel`.
    pub fn register(&mut self, channel: Channel) -> i32 {
        let handle = self
            .channels
            .iter()
            .position(Option::is_none)
            .map(|i| i as i32)
            .unwrap_or(self.channels.len() as i32);
        if (handle as usize) < self.channels.len() {
            self.channels[handle as usize] = Some(channel);
        } else {
            self.channels.push(Some(channel));
        }
        handle
    }

    /// Look up a channel.
    #[must_use]
    pub fn get(&self, handle: i32) -> Option<&Channel> {
        usize::try_from(handle)
            .ok()
            .and_then(|i| self.channels.get(i))
            .and_then(Option::as_ref)
    }

    /// Release a channel (window removed).
    pub fn remove(&mut self, handle: i32) -> Option<Channel> {
        usize::try_from(handle)
            .ok()
            .and_then(|i| self.channels.get_mut(i))
            .and_then(Option::take)
    }

    /// Number of live channels.
    #[must_use]
    pub fn len(&self) -> usize {
        self.channels.iter().filter(|c| c.is_some()).count()
    }

    /// `true` when no channel is live.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn in_memory_preserves_boundaries() {
        let (a, b) = InMemoryChannel::pair();
        a.send(b"one").unwrap();
        a.send(b"two").unwrap();
        assert_eq!(b.recv().unwrap(), Some(b"one".to_vec()));
        assert_eq!(b.recv().unwrap(), Some(b"two".to_vec()));
        assert_eq!(b.recv().unwrap(), None);
    }

    #[test]
    fn socketpair_round_trips_and_is_nonblocking() {
        let (a, b) = FdChannel::pair().unwrap();
        assert!(b.fd().is_some());
        assert_eq!(b.recv().unwrap(), None, "empty recv must not block");
        a.send(b"frame-1").unwrap();
        a.send(b"frame-2").unwrap();
        assert_eq!(b.recv().unwrap(), Some(b"frame-1".to_vec()));
        assert_eq!(b.recv().unwrap(), Some(b"frame-2".to_vec()));
    }

    #[test]
    fn registry_handles_are_stable_and_reusable() {
        let mut r = InputChannelRegistry::new();
        let h0 = r.register(Channel::in_memory("w0", 0));
        let h1 = r.register(Channel::in_memory("w1", 1));
        assert_ne!(h0, h1);
        assert!(r.get(h0).is_some());
        assert!(r.remove(h0).is_some());
        let h2 = r.register(Channel::in_memory("w2", 2));
        assert_eq!(h0, h2);
        assert_eq!(r.len(), 2);
    }
}
