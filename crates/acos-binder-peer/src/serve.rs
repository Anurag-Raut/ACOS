//! The peer's serve loop: **N** tagged driver connections, one per app process.
//!
//! Before MPROC2 the loop accepted exactly one connection and handed every
//! frame to [`Peer::handle`]. A session now has a primary process and (lazily)
//! secondary ones, each with its own driver, its own wire-handle space and its
//! own `acos.launch` handshake, all talking to the same `ServiceHost`. The loop
//! is single-threaded and poll-driven: one `ServiceHost` means no lock, no
//! cross-thread ordering question, and reverse deliveries keep their order.
//!
//! The connection handshake ([`F_HELLO`]) names the process *before* the first
//! transaction. A connection that never sends one is the primary process (tag
//! 0) — the exact one-guest behaviour that existed before, so an older driver
//! cannot be mis-addressed by a newer peer.

use std::io::{self, Write};
use std::os::fd::RawFd;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::peer::{Peer, TagRefusal};
use crate::protocol::{
    accept, close_frame_fds, read_frame, write_frame, Frame, F_RSP, MAX_PROCESSES,
    TAG_PRIMARY,
};

/// One live driver connection.
#[derive(Debug)]
struct Conn {
    fd: RawFd,
    /// The process tag, once the handshake named it. Until then the connection
    /// is a candidate primary process.
    tag: Option<u32>,
    /// Name from the handshake (diagnostics only).
    name: String,
}

/// What one serve run did (evidence, never a silent outcome).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ServeOutcome {
    /// Highest number of connections served at once.
    pub peak_connections: usize,
    /// Handshakes refused (out of range, duplicate, or table full).
    pub refused: usize,
    /// Handshakes accepted.
    pub accepted: usize,
    /// Frames read from the driver.
    pub frames_in: u64,
    /// Frames written to the driver.
    pub frames_out: u64,
    /// Deliveries dropped because the owning process had no connection.
    pub unroutable: u64,
    /// Connections closed by the driver (each dropped that process's binders).
    pub closed: usize,
    /// Death notifications written to a holder whose node's owner died.
    pub deaths: u64,
}

/// Serve driver connections on `listener` until `stop` is set or every
/// connection is gone and the session is stopping.
///
/// `ACOS_PEER_DEBUG=1` traces every frame in and out, exactly like the
/// per-connection loop did.
pub fn serve(peer: &mut Peer, listener: RawFd, stop: &AtomicBool) -> io::Result<ServeOutcome> {
    let dbg = std::env::var_os("ACOS_PEER_DEBUG").is_some();
    unsafe {
        let flags = libc::fcntl(listener, libc::F_GETFL);
        if flags < 0 || libc::fcntl(listener, libc::F_SETFL, flags | libc::O_NONBLOCK) < 0 {
            return Err(io::Error::last_os_error());
        }
    }
    let mut outcome = ServeOutcome::default();
    let mut conns: Vec<Conn> = Vec::new();
    // The listener is closed by the caller (it owns the fd); a `close` here
    // would be a use-after-close for the accept loop below.
    loop {
        if stop.load(Ordering::Acquire) && conns.is_empty() {
            return Ok(outcome);
        }
        let mut fds: Vec<libc::pollfd> = Vec::with_capacity(conns.len() + 1);
        fds.push(libc::pollfd {
            fd: listener,
            events: libc::POLLIN,
            revents: 0,
        });
        for c in &conns {
            fds.push(libc::pollfd {
                fd: c.fd,
                events: libc::POLLIN,
                revents: 0,
            });
        }
        let n = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, 100) };
        if n < 0 {
            let e = io::Error::last_os_error();
            if e.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            for c in &conns {
                unsafe { libc::close(c.fd) };
            }
            return Err(e);
        }
        if n == 0 {
            continue;
        }

        // New connections first, so a frame the guest already sent is not read
        // before its handshake.
        if fds[0].revents & libc::POLLIN != 0 {
            loop {
                if conns.len() >= MAX_PROCESSES {
                    // Refuse rather than block the session's other processes.
                    outcome.refused += 1;
                    eprintln!(
                        "acos-binder-peer: PROCESS_TABLE_FULL: refusing a connection \
                         ({MAX_PROCESSES} processes already served)"
                    );
                    break;
                }
                match accept(listener) {
                    Ok(fd) => {
                        unsafe {
                            let tv = libc::timeval {
                                tv_sec: 1,
                                tv_usec: 0,
                            };
                            libc::setsockopt(
                                fd,
                                libc::SOL_SOCKET,
                                libc::SO_RCVTIMEO,
                                &tv as *const _ as *const libc::c_void,
                                std::mem::size_of::<libc::timeval>() as libc::socklen_t,
                            );
                        }
                        if dbg {
                            eprintln!("peer-trace: driver connected (fd {fd})");
                        }
                        conns.push(Conn {
                            fd,
                            tag: None,
                            name: String::new(),
                        });
                        outcome.peak_connections = outcome.peak_connections.max(conns.len());
                        // Re-poll before touching the new connection: this poll
                        // set does not describe it yet. A second pending
                        // connection is accepted on the next iteration.
                        break;
                    }
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                    Err(e) => {
                        // A dead listener with live connections must not tear
                        // the session down; the connections keep it alive.
                        if conns.is_empty() {
                            return Err(e);
                        }
                        eprintln!("acos-binder-peer: accept: {e}");
                        break;
                    }
                }
            }
        }

        let mut index = 0;
        while index < conns.len() && index + 1 < fds.len() {
            let ready = fds[index + 1].revents;
            index += 1;
            if ready & (libc::POLLIN | libc::POLLHUP | libc::POLLERR) == 0 {
                continue;
            }
            let fd = conns[index - 1].fd;
            let frame = match read_frame(fd) {
                Ok(f) => f,
                Err(e)
                    if e.kind() == io::ErrorKind::UnexpectedEof
                        || e.kind() == io::ErrorKind::ConnectionReset =>
                {
                    close_conn(peer, &mut conns, index - 1, &mut outcome);
                    continue;
                }
                Err(e)
                    if e.kind() == io::ErrorKind::WouldBlock
                        || e.kind() == io::ErrorKind::TimedOut =>
                {
                    continue;
                }
                Err(e) => {
                    eprintln!("acos-binder-peer: read_frame: {e}");
                    close_conn(peer, &mut conns, index - 1, &mut outcome);
                    continue;
                }
            };
            outcome.frames_in += 1;
            if dbg {
                eprintln!(
                    "peer-trace: in ty={} seq={} code={} flags={:#x} target={:#x} data={} offs={} fds={}",
                    frame.ty,
                    frame.seq,
                    frame.code,
                    frame.flags,
                    frame.target,
                    frame.data.len(),
                    frame.offs.len(),
                    frame.fds.len()
                );
            }
            let tag = match conns[index - 1].tag {
                Some(tag) => tag,
                None => {
                    match resolve_tag(peer, &mut conns[index - 1], &frame) {
                        Ok(tag) => {
                            outcome.accepted += 1;
                            tag
                        }
                        Err(reason) => {
                            outcome.refused += 1;
                            eprintln!(
                                "acos-binder-peer: {}: {reason} \u{2014} refusing the connection",
                                reason.code()
                            );
                            let refusal = refuse_frame(frame.code, &reason);
                            let _ = write_frame(conns[index - 1].fd, &refusal);
                            close_frame_fds(&refusal);
                            close_conn(peer, &mut conns, index - 1, &mut outcome);
                            continue;
                        }
                    }
                }
            };
            let outs = peer.handle_tagged(tag, frame);
            if dbg {
                eprintln!("peer-trace: process {tag}: handle -> {} frames", outs.len());
            }
            for (dest, out) in outs {
                let Some(target_fd) = conns.iter().find(|c| c.tag == Some(dest)).map(|c| c.fd)
                else {
                    outcome.unroutable += 1;
                    eprintln!(
                        "acos-binder-peer: UNSUPPORTED_ANDROID_API: delivery to process {dest} \
                         dropped (no such connection)"
                    );
                    close_frame_fds(&out);
                    continue;
                };
                if let Err(e) = write_frame(target_fd, &out) {
                    eprintln!(
                        "peer-trace: write_frame FAILED for process {dest}: {e} \
                         (ty={} seq={} code={})",
                        out.ty, out.seq, out.code
                    );
                    close_frame_fds(&out);
                    close_conn(peer, &mut conns, index - 1, &mut outcome);
                    break;
                }
                outcome.frames_out += 1;
                if out.ty == crate::protocol::F_DEATH {
                    outcome.deaths += 1;
                }
                if dbg {
                    eprintln!(
                        "peer-trace: process {dest}: out ty={} seq={} code={} data={} offs={}",
                        out.ty,
                        out.seq,
                        out.code,
                        out.data.len(),
                        out.offs.len()
                    );
                }
                close_frame_fds(&out);
            }
            let _ = std::io::stderr().flush();
        }
    }
}

/// The tag of a connection whose first frame just arrived.
///
/// A handshake claims its tag; anything else is the primary process (the
/// pre-MPROC2 behaviour, kept so an older driver is never mis-addressed).
fn resolve_tag(peer: &mut Peer, conn: &mut Conn, frame: &Frame) -> Result<u32, TagRefusal> {
    let (tag, name) = match frame.hello_of() {
        Some((tag, name)) => {
            peer.register_process(tag, &name)?;
            (tag, name)
        }
        None => {
            // Legacy driver: no handshake at all. That is the primary process,
            // which is what every connection was before MPROC2.
            (
                TAG_PRIMARY,
                peer.process_name(TAG_PRIMARY).unwrap_or("").to_string(),
            )
        }
    };
    conn.tag = Some(tag);
    conn.name = name;
    Ok(tag)
}

/// Close one connection and drop what its process owned.
fn close_conn(peer: &mut Peer, conns: &mut Vec<Conn>, index: usize, outcome: &mut ServeOutcome) {
    let conn = conns.remove(index);
    unsafe { libc::close(conn.fd) };
    outcome.closed += 1;
    if let Some(tag) = conn.tag {
        let (binders, owners) = peer.disconnect_process(tag);
        eprintln!(
            "acos-binder-peer: process {tag} connection closed ({binders} java binders, \
             {owners} host binder records dropped)"
        );
    }
    // The process's binders that other processes hold as synthetic nodes are
    // dead now: every holder must get its `linkToDeath` callback (MPROC3 step
    // 3c). The peer queued those frames while dropping the process's state.
    flush_deaths(peer, conns, outcome);
}

/// Write the death notifications (and the structured refusals for transactions
/// that were waiting on a dead owner) the peer queued.
///
/// A holder that is no longer connected is not an error: it can never receive a
/// callback again, and its own close already released its nodes.
fn flush_deaths(peer: &mut Peer, conns: &mut Vec<Conn>, outcome: &mut ServeOutcome) {
    for (dest, frame) in peer.take_death_frames() {
        let Some(target_fd) = conns.iter().find(|c| c.tag == Some(dest)).map(|c| c.fd) else {
            outcome.unroutable += 1;
            close_frame_fds(&frame);
            continue;
        };
        if let Err(e) = write_frame(target_fd, &frame) {
            eprintln!(
                "acos-binder-peer: write_frame FAILED for process {dest} (ty={}): {e}",
                frame.ty
            );
            close_frame_fds(&frame);
            continue;
        }
        outcome.frames_out += 1;
        if frame.ty == crate::protocol::F_DEATH {
            outcome.deaths += 1;
            eprintln!(
                "acos-binder-peer: process {dest}: binder died (node {:#x}) \u{2014} \
                 F_DEATH written",
                frame.target
            );
        }
        close_frame_fds(&frame);
    }
}

/// Write a refusal reply on a connection and close it.
///
/// A refused handshake must not be silent: the driver gets a status-bearing
/// reply (which it can surface) and the socket is closed, so nothing it sends
/// afterwards can be attributed to a process that was never admitted.
pub fn refuse_frame(tag: u32, reason: &TagRefusal) -> Frame {
    let mut f = Frame::plain(
        F_RSP,
        0,
        0,
        crate::protocol::TF_STATUS_CODE,
        0,
        reason.to_string().into_bytes(),
    );
    f.code = tag;
    f
}
