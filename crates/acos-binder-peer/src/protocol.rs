//! The driver <-> peer frame transport.
//!
//! This is E3G's wire contract, unchanged: one `SOCK_SEQPACKET` message per
//! frame, a 48-byte header, then the parcel data and the `flat_binder_object`
//! offset table, with file descriptors in an `SCM_RIGHTS` control message.
//!
//! ```text
//! u32 type   F_REQ | F_RSP | F_TXN | F_REP
//! u32 seq    correlates a response with its request / reverse reply
//! u32 code   transaction code
//! u32 flags  binder transaction flags (bit 0 = TF_ONE_WAY)
//! u64 target REQ: peer node id (0 = servicemanager); TXN: driver wire handle
//! u64 data_size
//! u64 offsets_size
//! u32 sender_pid   (was `reserved` in v3; see [`PROTOCOL_VERSION`])
//! u32 sender_euid
//! ```
use std::io;
use std::mem;
use std::os::fd::RawFd;

/// Driver -> peer: a `BC_TRANSACTION`/`BC_TRANSACTION_SG` request.
pub const F_REQ: u32 = 1;
/// Peer -> driver: the reply to the request with the same `seq` (status in
/// `code`: 0 = ok).
pub const F_RSP: u32 = 2;
/// Peer -> driver: a peer-initiated transaction (`BR_TRANSACTION`), delivered to
/// the node named by `target`.
pub const F_TXN: u32 = 3;
/// Driver -> peer: the reply to the peer-initiated transaction with that `seq`.
pub const F_REP: u32 = 4;
/// Driver -> peer: the connection handshake, sent once right after `connect`.
///
/// The frame carries three things at once, none of which may be inferred:
///
/// * `seq` — the wire protocol version the driver speaks ([`PROTOCOL_VERSION`]).
///   A peer must not use a frame the driver did not advertise (the `F_REF`
///   reference model) and a driver must not assume the peer knows one either.
/// * `code` — this connection's app **process tag** (MPROC2 step 3a). Each app
///   process has its own driver connection and every binder handle on that
///   connection is private to it, so the peer puts the tag into every Java
///   binder id it imports from this connection and routes deliveries back to the
///   right one.
/// * `data` — the process name (evidence/diagnostics only).
///
/// A peer that never sees a hello treats the connection as the primary process
/// (tag 0), which is exactly the one-guest fast path that existed before.
pub const F_HELLO: u32 = 5;
/// Peer -> driver: one reference-count operation on a node the guest exported
/// (the `F_REL`/refcount message the design called for). `target` is the driver
/// wire handle, `code` is [`REF_ACQUIRE`] or [`REF_RELEASE`]. The frame goes to
/// the connection that owns the handle.
pub const F_REF: u32 = 6;
/// Peer -> driver: a synthetic node handed to this process died.
///
/// MPROC3 step 3c: a binder that lives in *another* app process reaches this
/// process as a synthetic node (see [`crate::peer::FOREIGN_FLAG`]). When the
/// owning process's connection closes, the peer sends one `F_DEATH` per node
/// that process owns and this connection holds, with `target` = the node id.
/// The driver turns it into the `BR_DEAD_BINDER` a local `linkToDeath`
/// registered for, so `IBinder.DeathRecipient.binderDied` runs — the same
/// event a real kernel driver produces when a remote process dies.
pub const F_DEATH: u32 = 7;

/// The wire protocol this build speaks. Bump it for every change to the frame
/// meanings (v2: `F_HELLO` + `F_REF`; v3: the hello also names the process tag,
/// `F_DEATH` exists; v4: the header's last 8 bytes carry the sender's pid and
/// euid instead of being reserved).
///
/// v4 is the BINDERPID change. The 8 bytes were `reserved` (zero) through v3,
/// so the frame is the same size and an older peer/driver still round-trips:
/// what it reads back is a sender of `(0, 0)`, which is exactly the "unknown
/// caller" a v3 build reported. The peer therefore **serves** a v3 driver (its
/// per-process identity comes from `SO_PEERCRED` + the activity manager, never
/// from the frame) and records that the host -> guest direction cannot name a
/// sender; a v4 driver refuses nothing, because a frame with a zero sender is
/// what an unknown sender always looked like. Peers newer than this build are
/// recorded as unsupported (see `Peer::handle_tagged`'s `F_HELLO` arm) and are
/// never assumed to know a frame this build does not.
pub const PROTOCOL_VERSION: u32 = 4;
/// The host took one more strong reference on the node.
pub const REF_ACQUIRE: u32 = 1;
/// The host dropped one strong reference; at zero the node is released and the
/// guest gets `BR_RELEASE` + `BR_DECREFS` (the kernel's last-release path).
pub const REF_RELEASE: u32 = 2;

/// `F_RSP` flag: the refusal means "the target process is dead".
///
/// The driver turns a status reply into `BR_FAILED_REPLY`; a dead target must
/// instead reach the caller as `BR_DEAD_REPLY`, which libbinder maps to
/// `DeadObjectException` (`IPCThreadState::executeCommand`). MPROC5b: a
/// transaction forwarded to a process that has died (or one already waiting on
/// it) is exactly that case, so the peer marks the reply with this bit and
/// carries no payload.
pub const RSP_DEAD: u32 = 0x8000_0000;

/// Size of the fixed frame header (`struct fhdr`).
pub const HEADER_SIZE: usize = 48;
/// Largest parcel data/offset block accepted in one frame.
pub const MAX_FRAME: usize = 256 * 1024;
/// Largest number of file descriptors carried by one frame.
pub const MAX_FDS: usize = 16;

/// `IBinder::PING_TRANSACTION` (`B_PACK_CHARS('_','P','N','G')` on the wire).
pub const PING_TRANSACTION: u32 = 0x5F50_4E47;
/// `IBinder::INTERFACE_TRANSACTION` (`B_PACK_CHARS('_','I','N','T')`). The reply
/// is a single `String16`: the node's AIDL descriptor. `libbinder`'s generated
/// `asInterface` for the native AIDL backends (`android.gui.*`, which have no
/// `acos-aidl` table) asks for it before dispatching any interface call.
pub const INTERFACE_TRANSACTION: u32 = 0x5F49_4E54;
/// The tag of the primary (launched) app process. Every session has it; a
/// second process gets 1..=[`MAX_PROCESS_TAG`].
pub const TAG_PRIMARY: u32 = 0;
/// Highest process tag a connection may claim (`PROC_MASK` is 6 bits).
pub const MAX_PROCESS_TAG: u32 = 63;
/// One connection per app process (tag 0 plus 63 secondary ones).
pub const MAX_PROCESSES: usize = MAX_PROCESS_TAG as usize + 1;
/// Longest process name a hello may carry; a longer one is truncated (the name
/// is evidence, not addressing).
pub const MAX_PROCESS_NAME: usize = 64;

/// `TF_ONE_WAY` in `binder_transaction_data.flags`.
pub const TF_ONE_WAY: u32 = 0x01;
/// `TF_STATUS_CODE` in `binder_transaction_data.flags`: the reply carries a
/// status word instead of a parcel body. libbinder sets it when the transaction
/// could not be dispatched at all (e.g. `UNKNOWN_TRANSACTION` from
/// `attemptIncStrong`) -- i.e. `onTransact` never ran. A reverse delivery that
/// "produced a reply" with this flag is a failure, not a success.
pub const TF_STATUS_CODE: u32 = 0x08;

/// One frame, owned.
#[derive(Debug, Default, Clone)]
pub struct Frame {
    /// F_REQ / F_RSP / F_TXN / F_REP.
    pub ty: u32,
    /// Correlation id (a response carries the request's seq).
    pub seq: u32,
    /// Transaction code.
    pub code: u32,
    /// Binder transaction flags.
    pub flags: u32,
    /// Peer node id (REQ) or driver wire handle (TXN).
    pub target: u64,
    /// Parcel data bytes.
    pub data: Vec<u8>,
    /// `flat_binder_object` offsets (little-endian `u64` each).
    pub offs: Vec<u8>,
    /// File descriptors carried by this frame.
    pub fds: Vec<RawFd>,
    /// Sender pid (v4). On `F_TXN`/`F_RSP` the driver reports it as
    /// `binder_transaction_data.sender_pid`, which is what
    /// `Binder.getCallingPid()` answers inside the guest's `onTransact`. `0`
    /// means "unknown sender" (a v3 peer, or a frame no identity applies to).
    pub sender_pid: u32,
    /// Sender uid (v4), `Binder.getCallingUid()`'s source. `0` = unknown.
    pub sender_euid: u32,
}

impl Frame {
    /// A frame with no objects/fds.
    pub fn plain(ty: u32, seq: u32, code: u32, flags: u32, target: u64, data: Vec<u8>) -> Self {
        Self {
            ty,
            seq,
            code,
            flags,
            target,
            data,
            offs: Vec::new(),
            fds: Vec::new(),
            sender_pid: 0,
            sender_euid: 0,
        }
    }

    /// A frame that names its sender (v4): the host stamps a `system -> app`
    /// delivery with `system_server`'s pid/uid, the peer stamps a relayed
    /// transaction with the sending app process's.
    #[must_use]
    pub fn sent_by(mut self, pid: u32, euid: u32) -> Self {
        self.sender_pid = pid;
        self.sender_euid = euid;
        self
    }

    /// A connection handshake naming `tag` (see [`F_HELLO`]).
    ///
    /// `seq` carries the wire protocol version this build speaks, `code` the
    /// process tag, `data` the process name (the three facts [`F_HELLO`]
    /// documents).
    #[must_use]
    pub fn hello(tag: u32, name: &str) -> Self {
        let mut bytes = name.as_bytes().to_vec();
        bytes.truncate(MAX_PROCESS_NAME);
        Self {
            ty: F_HELLO,
            seq: PROTOCOL_VERSION,
            code: tag,
            flags: 0,
            target: 0,
            data: bytes,
            offs: Vec::new(),
            fds: Vec::new(),
            sender_pid: 0,
            sender_euid: 0,
        }
    }

    /// `(tag, name)` when this frame is a handshake.
    #[must_use]
    pub fn hello_of(&self) -> Option<(u32, String)> {
        if self.ty != F_HELLO {
            return None;
        }
        let name = String::from_utf8_lossy(&self.data).to_string();
        Some((self.code, name))
    }

    /// The wire protocol version a handshake announced (`None` for a frame
    /// that is not a handshake).
    #[must_use]
    pub fn hello_version(&self) -> Option<u32> {
        (self.ty == F_HELLO).then_some(self.seq)
    }

    /// The object offsets decoded from the little-endian table.
    #[must_use]
    pub fn object_offsets(&self) -> Vec<usize> {
        self.offs
            .chunks_exact(8)
            .map(|b| u64::from_le_bytes(b.try_into().expect("8 bytes")) as usize)
            .collect()
    }
}

/// Build the 48-byte header for `f`.
#[must_use]
pub fn encode_header(f: &Frame) -> [u8; HEADER_SIZE] {
    let mut h = [0u8; HEADER_SIZE];
    h[0..4].copy_from_slice(&f.ty.to_le_bytes());
    h[4..8].copy_from_slice(&f.seq.to_le_bytes());
    h[8..12].copy_from_slice(&f.code.to_le_bytes());
    h[12..16].copy_from_slice(&f.flags.to_le_bytes());
    h[16..24].copy_from_slice(&f.target.to_le_bytes());
    h[24..32].copy_from_slice(&(f.data.len() as u64).to_le_bytes());
    h[32..40].copy_from_slice(&(f.offs.len() as u64).to_le_bytes());
    // v4: the 8 bytes that were `reserved` (and are still zero for a v3 peer).
    h[40..44].copy_from_slice(&f.sender_pid.to_le_bytes());
    h[44..48].copy_from_slice(&f.sender_euid.to_le_bytes());
    h
}

/// Decode a header. Returns `(frame_without_payload, data_size, offsets_size)`.
fn decode_header(h: &[u8; HEADER_SIZE]) -> (Frame, usize, usize) {
    let u32at = |o: usize| u32::from_le_bytes(h[o..o + 4].try_into().expect("4 bytes"));
    let u64at = |o: usize| u64::from_le_bytes(h[o..o + 8].try_into().expect("8 bytes"));
    (
        Frame {
            ty: u32at(0),
            seq: u32at(4),
            code: u32at(8),
            flags: u32at(12),
            target: u64at(16),
            data: Vec::new(),
            offs: Vec::new(),
            fds: Vec::new(),
            sender_pid: u32at(40),
            sender_euid: u32at(44),
        },
        u64at(24) as usize,
        u64at(32) as usize,
    )
}

/// Send one frame on `fd`.
pub fn write_frame(fd: RawFd, f: &Frame) -> io::Result<()> {
    let h = encode_header(f);
    let mut iov = [
        libc::iovec {
            iov_base: h.as_ptr() as *mut libc::c_void,
            iov_len: h.len(),
        },
        libc::iovec {
            iov_base: f.data.as_ptr() as *mut libc::c_void,
            iov_len: f.data.len(),
        },
        libc::iovec {
            iov_base: f.offs.as_ptr() as *mut libc::c_void,
            iov_len: f.offs.len(),
        },
    ];
    let mut msg: libc::msghdr = unsafe { mem::zeroed() };
    msg.msg_iov = iov.as_mut_ptr();
    msg.msg_iovlen = 3;
    let mut cbuf = [0u8; 256];
    if !f.fds.is_empty() {
        if f.fds.len() > MAX_FDS {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "too many fds in one frame",
            ));
        }
        msg.msg_control = cbuf.as_mut_ptr() as *mut libc::c_void;
        msg.msg_controllen = unsafe {
            libc::CMSG_SPACE((f.fds.len() * mem::size_of::<RawFd>()) as libc::c_uint) as usize
        };
        unsafe {
            let c = libc::CMSG_FIRSTHDR(&msg);
            (*c).cmsg_level = libc::SOL_SOCKET;
            (*c).cmsg_type = libc::SCM_RIGHTS;
            (*c).cmsg_len =
                libc::CMSG_LEN((f.fds.len() * mem::size_of::<RawFd>()) as libc::c_uint) as usize;
            std::ptr::copy_nonoverlapping(
                f.fds.as_ptr() as *const u8,
                libc::CMSG_DATA(c),
                f.fds.len() * mem::size_of::<RawFd>(),
            );
        }
    }
    loop {
        let n = unsafe { libc::sendmsg(fd, &msg, libc::MSG_NOSIGNAL) };
        if n >= 0 {
            return Ok(());
        }
        let e = io::Error::last_os_error();
        if e.kind() != io::ErrorKind::Interrupted {
            return Err(e);
        }
    }
}

/// Receive one frame from `fd`.
///
/// On `SOCK_SEQPACKET` one `recvmsg` returns the whole frame (and the
/// `SCM_RIGHTS` descriptors). On a stream socket the remainder is read with
/// `read_exact`; the transport script uses seqpacket, tests may use a stream.
pub fn read_frame(fd: RawFd) -> io::Result<Frame> {
    let mut buf = vec![0u8; HEADER_SIZE + MAX_FRAME];
    let mut cbuf = [0u8; 256];
    let mut iov = libc::iovec {
        iov_base: buf.as_mut_ptr() as *mut libc::c_void,
        iov_len: buf.len(),
    };
    let mut msg: libc::msghdr = unsafe { mem::zeroed() };
    msg.msg_iov = &mut iov;
    msg.msg_iovlen = 1;
    msg.msg_control = cbuf.as_mut_ptr() as *mut libc::c_void;
    msg.msg_controllen = cbuf.len();
    let n = loop {
        let n = unsafe { libc::recvmsg(fd, &mut msg, 0) };
        if n >= 0 {
            break n as usize;
        }
        let e = io::Error::last_os_error();
        if e.kind() != io::ErrorKind::Interrupted {
            return Err(e);
        }
    };
    if n < HEADER_SIZE {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            format!("short frame ({n} bytes)"),
        ));
    }
    let mut fds = Vec::new();
    unsafe {
        let mut c = libc::CMSG_FIRSTHDR(&msg);
        while !c.is_null() {
            if (*c).cmsg_level == libc::SOL_SOCKET && (*c).cmsg_type == libc::SCM_RIGHTS {
                let payload = (*c).cmsg_len - libc::CMSG_LEN(0) as usize;
                let count = payload / mem::size_of::<RawFd>();
                let mut raw = vec![0i32; count];
                std::ptr::copy_nonoverlapping(
                    libc::CMSG_DATA(c) as *const i32,
                    raw.as_mut_ptr(),
                    count,
                );
                fds.extend(raw);
            }
            c = libc::CMSG_NXTHDR(&msg, c);
        }
    }
    let (mut f, dsize, osize) = decode_header(buf[..HEADER_SIZE].try_into().expect("header"));
    let total = HEADER_SIZE + dsize + osize;
    if total > buf.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("oversized frame ({total} bytes)"),
        ));
    }
    if total > n {
        // Stream fallback: finish the frame with plain reads.
        use std::io::Read;
        let mut rest = vec![0u8; total - n];
        let mut stream = unsafe {
            <std::os::unix::net::UnixStream as std::os::fd::FromRawFd>::from_raw_fd(libc::dup(fd))
        };
        stream.read_exact(&mut rest)?;
        buf[n..total].copy_from_slice(&rest);
    }
    f.data = buf[HEADER_SIZE..HEADER_SIZE + dsize].to_vec();
    f.offs = buf[HEADER_SIZE + dsize..total].to_vec();
    f.fds = fds;
    Ok(f)
}

/// Close the descriptors a frame carries.
///
/// Frames own their descriptors: [`read_frame`] receives a copy through
/// `SCM_RIGHTS`, and [`crate::peer::frame_from_parcel`] duplicates the ones a
/// reply `Parcel` owns (the parcel is dropped long before `write_frame` runs).
/// Call this once the frame has been handled/written, or the peer leaks one fd
/// per fd-carrying transaction.
pub fn close_frame_fds(f: &Frame) {
    for &fd in &f.fds {
        if fd >= 0 {
            unsafe { libc::close(fd) };
        }
    }
}

/// Create a listening `SOCK_SEQPACKET` unix socket at `path`.
pub fn listen(path: &str) -> io::Result<RawFd> {
    unsafe {
        let fd = libc::socket(libc::AF_UNIX, libc::SOCK_SEQPACKET | libc::SOCK_CLOEXEC, 0);
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        let mut sa: libc::sockaddr_un = mem::zeroed();
        sa.sun_family = libc::AF_UNIX as libc::sa_family_t;
        let bytes = path.as_bytes();
        if bytes.len() >= sa.sun_path.len() {
            libc::close(fd);
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "socket path too long",
            ));
        }
        for (i, b) in bytes.iter().enumerate() {
            sa.sun_path[i] = *b as libc::c_char;
        }
        let len = (mem::size_of::<libc::sa_family_t>() + bytes.len() + 1) as libc::socklen_t;
        if libc::bind(fd, &sa as *const _ as *const libc::sockaddr, len) != 0 {
            let e = io::Error::last_os_error();
            libc::close(fd);
            return Err(e);
        }
        if libc::listen(fd, 4) != 0 {
            let e = io::Error::last_os_error();
            libc::close(fd);
            return Err(e);
        }
        Ok(fd)
    }
}

/// Who the kernel says is on the other end of an accepted socket.
///
/// `pid` is the peer process's pid **in this process's pid namespace**, which
/// is what `ps`, `SO_PEERCRED` and a `ProcessRecord.pid` all report on the
/// host — the guest's own `getpid()` is a namespace-local number because the
/// sandbox unshares the pid namespace (`acos-art`'s bwrap set). `uid` is the
/// *host* uid, which a `--unshare-user` sandbox maps the app's uid onto
/// (1000 = `SYSTEM_UID`), so it is never the app's uid; the app's uid comes
/// from the activity manager's `ProcessRecord` (see `Peer::caller_identity`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PeerCred {
    /// Peer pid in the host's pid namespace.
    pub pid: i32,
    /// Peer uid as the kernel sees it (the sandbox's mapped uid).
    pub uid: u32,
    /// Peer gid as the kernel sees it.
    pub gid: u32,
}

/// `getsockopt(SOL_SOCKET, SO_PEERCRED)` on `fd`.
pub fn peer_cred(fd: RawFd) -> io::Result<PeerCred> {
    // SAFETY: `ucred` is the kernel's own struct for this option and the
    // length is passed in and checked on return; `fd` is a live socket (the
    // caller accepted it).
    let mut cred: libc::ucred = unsafe { mem::zeroed() };
    let mut len = mem::size_of::<libc::ucred>() as libc::socklen_t;
    let rc = unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&mut cred as *mut libc::ucred).cast::<libc::c_void>(),
            &mut len,
        )
    };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(PeerCred {
        pid: cred.pid,
        uid: cred.uid,
        gid: cred.gid,
    })
}

/// Accept one connection.
pub fn accept(fd: RawFd) -> io::Result<RawFd> {
    loop {
        let c = unsafe { libc::accept(fd, std::ptr::null_mut(), std::ptr::null_mut()) };
        if c >= 0 {
            return Ok(c);
        }
        let e = io::Error::last_os_error();
        if e.kind() != io::ErrorKind::Interrupted {
            return Err(e);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_is_48_bytes_and_round_trips() {
        let f = Frame {
            ty: F_REQ,
            seq: 7,
            code: 73,
            flags: 0,
            target: 0x2_0000_0001,
            data: vec![1, 2, 3, 4],
            offs: vec![0, 0, 0, 0, 0, 0, 0, 0],
            fds: vec![],
            sender_pid: 0,
            sender_euid: 0,
        };
        let h = encode_header(&f);
        assert_eq!(h.len(), 48);
        let (g, d, o) = decode_header(&h);
        assert_eq!(
            (g.ty, g.seq, g.code, g.target),
            (F_REQ, 7, 73, 0x2_0000_0001)
        );
        assert_eq!((d, o), (4, 8));
    }

    /// BINDERPID: the sender identity lives in the header's last 8 bytes, in
    /// exactly the place the v3 header called `reserved`. The byte offsets are
    /// asserted literally, because the C driver reads the same struct field by
    /// field and a reordering would be invisible to a round-trip test that
    /// encoded and decoded with the same code.
    #[test]
    fn the_sender_identity_is_the_headers_last_eight_bytes() {
        let f = Frame::plain(F_TXN, 1, 2, 0, 3, Vec::new()).sent_by(4242, 10192);
        let h = encode_header(&f);
        assert_eq!(&h[40..44], &4242u32.to_le_bytes());
        assert_eq!(&h[44..48], &10192u32.to_le_bytes());
        // The bytes before them are untouched: data/offsets sizes are zero here.
        assert_eq!(&h[24..40], &[0u8; 16]);
        let (g, _, _) = decode_header(&h);
        assert_eq!((g.sender_pid, g.sender_euid), (4242, 10192));
    }

    /// A v3 header's last 8 bytes were `reserved` = zero, so it decodes as an
    /// unknown sender rather than a fabricated identity.
    #[test]
    fn a_v3_header_decodes_as_an_unknown_sender() {
        let mut h = encode_header(&Frame::plain(F_TXN, 1, 2, 0, 3, Vec::new()));
        h[40..48].fill(0);
        let (g, _, _) = decode_header(&h);
        assert_eq!((g.sender_pid, g.sender_euid), (0, 0));
    }

    #[test]
    fn the_sender_survives_the_socket() {
        use std::os::fd::AsRawFd;
        use std::os::unix::net::UnixStream;
        let (a, b) = UnixStream::pair().unwrap();
        let f = Frame::plain(F_TXN, 9, 4, 0, 0x7, vec![1, 2]).sent_by(31337, 1000);
        write_frame(a.as_raw_fd(), &f).unwrap();
        let g = read_frame(b.as_raw_fd()).unwrap();
        assert_eq!((g.sender_pid, g.sender_euid), (31337, 1000));
        assert_eq!(g.data, vec![1, 2]);
    }

    #[test]
    fn object_offsets_decode() {
        let f = Frame {
            offs: 16u64
                .to_le_bytes()
                .into_iter()
                .chain(40u64.to_le_bytes())
                .collect(),
            ..Default::default()
        };
        assert_eq!(f.object_offsets(), vec![16, 40]);
    }

    #[test]
    fn frames_round_trip_over_a_unix_socket() {
        use std::os::unix::net::UnixStream;
        let (a, b) = UnixStream::pair().unwrap();
        use std::os::fd::AsRawFd;
        let f = Frame::plain(F_REQ, 1, 5, 0, 0x2_0000_0002, vec![9, 8, 7, 6]);
        write_frame(a.as_raw_fd(), &f).unwrap();
        let g = read_frame(b.as_raw_fd()).unwrap();
        assert_eq!(
            (g.ty, g.seq, g.code, g.target, g.data),
            (F_REQ, 1, 5, 0x2_0000_0002, vec![9, 8, 7, 6])
        );
    }

    #[test]
    fn hello_names_the_process_and_round_trips_over_a_socket() {
        use std::os::fd::AsRawFd;
        use std::os::unix::net::UnixStream;
        let (a, b) = UnixStream::pair().unwrap();
        let f = Frame::hello(3, "dev.acos.fixtures.counter");
        write_frame(a.as_raw_fd(), &f).unwrap();
        let g = read_frame(b.as_raw_fd()).unwrap();
        assert_eq!(g.ty, F_HELLO);
        assert_eq!(
            g.hello_of(),
            Some((3, "dev.acos.fixtures.counter".to_string()))
        );
        // A normal transaction is never mistaken for a handshake.
        let req = Frame::plain(F_REQ, 1, 5, 0, 0, vec![1]);
        assert_eq!(req.hello_of(), None);
    }

    #[test]
    fn a_hello_name_is_bounded() {
        let f = Frame::hello(1, &"n".repeat(MAX_PROCESS_NAME * 4));
        assert_eq!(f.data.len(), MAX_PROCESS_NAME);
    }
}
