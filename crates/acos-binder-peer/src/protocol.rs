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
//! u64 reserved
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
/// MPROC2 step 3a: each app process has its own driver connection, and every
/// binder handle on that connection is private to that process. The handshake
/// names the process (`code` = tag, `data` = the process name) *before* any
/// transaction, so the peer can put the owning tag into every Java binder id it
/// imports from this connection and route deliveries back to the right one.
///
/// A peer that never sees a hello treats the connection as the primary process
/// (tag 0), which is exactly the one-guest fast path that existed before.
pub const F_HELLO: u32 = 5;

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
        }
    }

    /// A connection handshake naming `tag` (see [`F_HELLO`]).
    #[must_use]
    pub fn hello(tag: u32, name: &str) -> Self {
        let mut bytes = name.as_bytes().to_vec();
        bytes.truncate(MAX_PROCESS_NAME);
        Self {
            ty: F_HELLO,
            seq: 0,
            code: tag,
            flags: 0,
            target: 0,
            data: bytes,
            offs: Vec::new(),
            fds: Vec::new(),
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
        let mut stream = unsafe { <std::os::unix::net::UnixStream as std::os::fd::FromRawFd>::from_raw_fd(libc::dup(fd)) };
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
        };
        let h = encode_header(&f);
        assert_eq!(h.len(), 48);
        let (g, d, o) = decode_header(&h);
        assert_eq!((g.ty, g.seq, g.code, g.target), (F_REQ, 7, 73, 0x2_0000_0001));
        assert_eq!((d, o), (4, 8));
    }

    #[test]
    fn object_offsets_decode() {
        let f = Frame {
            offs: 16u64.to_le_bytes().into_iter().chain(40u64.to_le_bytes()).collect(),
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
        assert_eq!((g.ty, g.seq, g.code, g.target, g.data), (F_REQ, 1, 5, 0x2_0000_0002, vec![9, 8, 7, 6]));
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
