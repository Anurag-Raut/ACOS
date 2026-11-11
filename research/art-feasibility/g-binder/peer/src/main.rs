// E3G peer: the other end of the userspace binder driver.
//
// Plays servicemanager (handle 0: IServiceManager.getService/checkService/
// addService over a real AIDL-shaped Parcel) and one service behind it
// ("activity", handle 1) that echoes an int/String/Bundle, accepts a
// ParcelFileDescriptor, and — for the reverse-call rung — initiates its own
// transaction into a Java Binder registered through addService.
//
// No external crates: the parcel codec and the SCM_RIGHTS plumbing are here so
// that `rustc -O peer/src/main.rs` is the whole build.
use std::io::Read;
use std::os::fd::{AsRawFd, FromRawFd, RawFd};
use std::os::unix::net::UnixStream;

#[repr(C)]
struct SockaddrUn {
    family: u16,
    path: [u8; 108],
}

extern "C" {
    fn socket(domain: i32, ty: i32, proto: i32) -> i32;
    fn bind(fd: i32, addr: *const SockaddrUn, len: u32) -> i32;
    fn listen(fd: i32, backlog: i32) -> i32;
    fn accept(fd: i32, addr: *mut u8, len: *mut u32) -> i32;
    fn close(fd: i32) -> i32;
}

// SOCK_SEQPACKET (5) on AF_UNIX (1): one sendmsg == one recvmsg, so the binder
// driver's message framing survives, and SCM_RIGHTS still works.
fn listen_seqpacket(path: &str) -> std::io::Result<i32> {
    unsafe {
        let fd = socket(1, 5, 0);
        if fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        let mut sa = SockaddrUn { family: 1, path: [0; 108] };
        let b = path.as_bytes();
        sa.path[..b.len()].copy_from_slice(b);
        if bind(fd, &sa, 2 + b.len() as u32 + 1) != 0 {
            return Err(std::io::Error::last_os_error());
        }
        if listen(fd, 4) != 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(fd)
    }
}

const F_REQ: u32 = 1;
const F_RSP: u32 = 2;
const F_TXN: u32 = 3;
const F_REP: u32 = 4;

const BINDER_TYPE_BINDER: u32 = 0x73622a85; // B_PACK_CHARS('s','b','*',0x85)
const BINDER_TYPE_HANDLE: u32 = 0x73682a85;
const BINDER_TYPE_FD: u32 = 0x66642a85;

const NODE_ECHO: u64 = 0x1001;
const TOKEN_SM: &str = "android.os.IServiceManager";
const TOKEN_ECHO: &str = "dev.acos.e3g.IEcho";

#[repr(C)]
struct Iovec {
    base: *mut u8,
    len: usize,
}

#[repr(C)]
struct Msghdr {
    name: *mut u8,
    namelen: u32,
    _pad0: u32,
    iov: *mut Iovec,
    iovlen: usize,
    control: *mut u8,
    controllen: usize,
    flags: i32,
    _pad1: i32,
}

extern "C" {
    fn recvmsg(fd: i32, msg: *mut Msghdr, flags: i32) -> isize;
    fn sendmsg(fd: i32, msg: *const Msghdr, flags: i32) -> isize;
}

#[derive(Default)]
struct Frame {
    ty: u32,
    seq: u32,
    code: u32,
    flags: u32,
    target: u64,
    data: Vec<u8>,
    offs: Vec<u8>,
    fds: Vec<i32>,
}

fn write_frame(s: &mut UnixStream, f: &Frame) -> std::io::Result<()> {
    let mut hdr = [0u8; 48];
    hdr[0..4].copy_from_slice(&f.ty.to_le_bytes());
    hdr[4..8].copy_from_slice(&f.seq.to_le_bytes());
    hdr[8..12].copy_from_slice(&f.code.to_le_bytes());
    hdr[12..16].copy_from_slice(&f.flags.to_le_bytes());
    hdr[16..24].copy_from_slice(&f.target.to_le_bytes());
    hdr[24..32].copy_from_slice(&(f.data.len() as u64).to_le_bytes());
    hdr[32..40].copy_from_slice(&(f.offs.len() as u64).to_le_bytes());
    let mut iov = [
        Iovec { base: hdr.as_mut_ptr(), len: 48 },
        Iovec { base: f.data.as_ptr() as *mut u8, len: f.data.len() },
        Iovec { base: f.offs.as_ptr() as *mut u8, len: f.offs.len() },
    ];
    let mut cbuf = [0u8; 128];
    let mut msg = Msghdr {
        name: std::ptr::null_mut(),
        namelen: 0,
        _pad0: 0,
        iov: iov.as_mut_ptr(),
        iovlen: 3,
        control: std::ptr::null_mut(),
        controllen: 0,
        flags: 0,
        _pad1: 0,
    };
    if !f.fds.is_empty() {
        // cmsghdr { size_t len; int level; int type; } + payload
        let paylen = 4 * f.fds.len();
        let space = 16 + ((paylen + 7) & !7usize);
        let c = cbuf.as_mut_ptr();
        unsafe {
            (c as *mut u64).write_unaligned(16 + paylen as u64);
            (c.add(8) as *mut i32).write_unaligned(1); // SOL_SOCKET
            (c.add(12) as *mut i32).write_unaligned(1); // SCM_RIGHTS
            std::ptr::copy_nonoverlapping(f.fds.as_ptr() as *const u8, c.add(16), paylen);
        }
        msg.control = c;
        msg.controllen = space;
    }
    let n = unsafe { sendmsg(s.as_raw_fd(), &msg, 0) };
    if n < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

// Single recvmsg that gets header + payload + SCM_RIGHTS in one go.
fn recv_frame(s: &mut UnixStream) -> std::io::Result<Frame> {
    let mut buf = vec![0u8; 48 + 256 * 1024];
    let mut cbuf = [0u8; 256];
    let mut iov = [Iovec { base: buf.as_mut_ptr(), len: buf.len() }];
    let mut msg = Msghdr {
        name: std::ptr::null_mut(),
        namelen: 0,
        _pad0: 0,
        iov: iov.as_mut_ptr(),
        iovlen: 1,
        control: cbuf.as_mut_ptr(),
        controllen: cbuf.len(),
        flags: 0,
        _pad1: 0,
    };
    let n = loop {
        let n = unsafe { recvmsg(s.as_raw_fd(), &mut msg, 0) };
        if n < 0 {
            let e = std::io::Error::last_os_error();
            if e.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(e);
        }
        break n as usize;
    };
    if n < 48 {
        return Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "short frame"));
    }
    let mut fds = Vec::new();
    if msg.controllen >= 16 {
        unsafe {
            let c = cbuf.as_ptr();
            let len = (c as *const u64).read_unaligned() as usize;
            let level = (c.add(8) as *const i32).read_unaligned();
            let ty = (c.add(12) as *const i32).read_unaligned();
            if level == 1 && ty == 1 && len >= 16 {
                let cnt = (len - 16) / 4;
                for i in 0..cnt {
                    fds.push((c.add(16 + 4 * i) as *const i32).read_unaligned());
                }
            }
        }
    }
    let dsize = u64::from_le_bytes(buf[24..32].try_into().unwrap()) as usize;
    let osize = u64::from_le_bytes(buf[32..40].try_into().unwrap()) as usize;
    let total = 48 + dsize + osize;
    if total > n {
        // SOCK_STREAM: the first recvmsg (which carries the SCM_RIGHTS) may be
        // short; finish the frame with plain reads.
        buf.resize(total, 0);
        s.read_exact(&mut buf[n..total])?;
    } else if total > buf.len() {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "oversized frame"));
    }
    let u32at = |o: usize| u32::from_le_bytes(buf[o..o + 4].try_into().unwrap());
    let u64at = |o: usize| u64::from_le_bytes(buf[o..o + 8].try_into().unwrap());
    let mut f = Frame {
        ty: u32at(0),
        seq: u32at(4),
        code: u32at(8),
        flags: u32at(12),
        target: u64at(16),
        data: buf[48..48 + dsize].to_vec(),
        offs: buf[48 + dsize..48 + dsize + osize].to_vec(),
        fds,
    };
    // the receiver substitutes its own fd numbers into BINDER_TYPE_FD objects,
    // exactly like the kernel does
    let mut k = 0usize;
    for off in f.offs.chunks(8) {
        let off = u64::from_le_bytes(off.try_into().unwrap()) as usize;
        if off + 24 <= f.data.len() {
            let ty = u32::from_le_bytes(f.data[off..off + 4].try_into().unwrap());
            if ty == BINDER_TYPE_FD && k < f.fds.len() {
                f.data[off + 8..off + 12].copy_from_slice(&(f.fds[k] as u32).to_le_bytes());
                k += 1;
            }
        }
    }
    f.fds.clear();
    Ok(f)
}

// ------------------------------------------------------------------ parcel io
struct W {
    buf: Vec<u8>,
    offs: Vec<u64>,
}

impl W {
    fn new() -> Self {
        W { buf: Vec::new(), offs: Vec::new() }
    }
    fn i32(&mut self, v: i32) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    fn s16(&mut self, s: &str) {
        let chars: Vec<u16> = s.encode_utf16().collect();
        self.i32(chars.len() as i32);
        for c in &chars {
            self.buf.extend_from_slice(&c.to_le_bytes());
        }
        self.buf.extend_from_slice(&[0u8, 0]);
        while self.buf.len() % 4 != 0 {
            self.buf.push(0);
        }
    }
    fn binder_obj(&mut self, ty: u32, binder: u64, cookie: u64) {
        while self.buf.len() % 4 != 0 {
            self.buf.push(0);
        }
        self.offs.push(self.buf.len() as u64);
        self.buf.extend_from_slice(&ty.to_le_bytes());
        self.buf.extend_from_slice(&0x7fu32.to_le_bytes());
        self.buf.extend_from_slice(&binder.to_le_bytes());
        self.buf.extend_from_slice(&cookie.to_le_bytes());
        // Android 11+ parcels carry a 4-byte stability record right after every
        // flat_binder_object (Parcel::unflattenBinder reads it back with
        // Stability::setRepr). Level::SYSTEM == 0b1100 (0=UNDECLARED,
        // 0b0100=VENDOR, 0b1100=SYSTEM -- the enum is *not* 0/1/2).
        self.i32(12);
    }
    fn raw(&mut self, b: &[u8]) {
        self.buf.extend_from_slice(b);
    }
}

struct R<'a> {
    b: &'a [u8],
    p: usize,
}

impl<'a> R<'a> {
    fn new(b: &'a [u8]) -> Self {
        R { b, p: 0 }
    }
    fn i32(&mut self) -> i32 {
        let v = i32::from_le_bytes(self.b[self.p..self.p + 4].try_into().unwrap());
        self.p += 4;
        v
    }
    fn s16(&mut self) -> Option<String> {
        let n = self.i32();
        if n < 0 {
            return None;
        }
        let n = n as usize;
        let mut units = Vec::with_capacity(n);
        for i in 0..n {
            units.push(u16::from_le_bytes(
                self.b[self.p + 2 * i..self.p + 2 * i + 2].try_into().unwrap(),
            ));
        }
        self.p += 2 * n + 2;
        while self.p % 4 != 0 {
            self.p += 1;
        }
        Some(String::from_utf16_lossy(&units))
    }
    fn remaining(&self) -> &'a [u8] {
        &self.b[self.p..]
    }
}

fn main() {
    let path = std::env::args().nth(1).unwrap_or_else(|| "/tmp/e3g.sock".to_string());
    let _ = std::fs::remove_file(&path);
    let lfd = listen_seqpacket(&path).expect("bind");
    eprintln!("E3G[peer]: listening on {path}");
    let cfd = unsafe { accept(lfd, std::ptr::null_mut(), std::ptr::null_mut()) };
    if cfd < 0 {
        panic!("accept: {}", std::io::Error::last_os_error());
    }
    let mut s = unsafe { <UnixStream as std::os::fd::FromRawFd>::from_raw_fd(cfd) };
    eprintln!("E3G[peer]: client connected");

    let mut services: std::collections::HashMap<String, u64> =
        std::collections::HashMap::new();
    services.insert("activity".to_string(), NODE_ECHO);
    let mut reverse_pending = false;
    let mut echo_calls = 0u64;

    loop {
        let f = match recv_frame(&mut s) {
            Ok(f) => f,
            Err(e) => {
                eprintln!("E3G[peer]: connection closed ({e})");
                let _ = std::fs::remove_file(&path);
                return;
            }
        };
        match f.ty {
            F_REQ => {
                let mut r = R::new(&f.data);
                // Android 14 writeInterfaceToken prefix: [policy|0x80000000]
                // [workSourceUid (-1)] [magic 0x53595354 "SYST"] String16 token
                let _policy = if f.data.len() >= 16 { r.i32() } else { 0 };
                let _work_source = if f.data.len() >= 16 { r.i32() } else { 0 };
                let _hdr = if f.data.len() >= 16 { r.i32() } else { 0 };
                let token = if f.data.len() >= 16 { r.s16().unwrap_or_default() } else { String::new() };
                let ping = f.code == 0x5F50_4E47; // IBinder::PING_TRANSACTION
                if !f.offs.is_empty() {
                    for off in f.offs.chunks(8) {
                        let o = u64::from_le_bytes(off.try_into().unwrap()) as usize;
                        let ty = if o + 24 <= f.data.len() {
                            u32::from_le_bytes(f.data[o..o + 4].try_into().unwrap())
                        } else { 0 };
                        let fl = if o + 8 <= f.data.len() {
                            u32::from_le_bytes(f.data[o + 4..o + 8].try_into().unwrap())
                        } else { 0 };
                        let b = if o + 16 <= f.data.len() {
                            u64::from_le_bytes(f.data[o + 8..o + 16].try_into().unwrap())
                        } else { 0 };
                        eprintln!("E3G[peer]: obj off={o} type={ty:#x} flags={fl:#x} binder/handle={b:#x}");
                    }
                }
                if std::env::var("E3G_HEX").is_ok() {
                    let hex: String = f.data.iter().take(64).map(|b| format!("{b:02x}")).collect();
                    eprintln!("E3G[peer]: data hex={hex}");
                }
                eprintln!(
                    "E3G[peer]: REQ seq={} target={:#x} code={}{} flags={:#x} size={} token={:?}",
                    f.seq, f.target, f.code, if ping { " (PING_TRANSACTION)" } else { "" },
                    f.flags, f.data.len(), token
                );
                if ping || f.data.is_empty() {
                    let rsp = Frame {
                        ty: F_RSP, seq: f.seq, code: 0, flags: 0, target: 0,
                        data: vec![], offs: vec![], fds: vec![],
                    };
                    write_frame(&mut s, &rsp).unwrap();
                    continue;
                }
                if f.target == 0 {
                    // ---- servicemanager
                    match f.code {
                        1 | 2 => {
                            // getService / checkService
                            let name = r.s16().unwrap_or_default();
                            let mut w = W::new();
                            w.i32(0); // exception code
                            match services.get(&name) {
                                Some(node) => {
                                    w.binder_obj(BINDER_TYPE_BINDER, *node, *node);
                                    eprintln!("E3G[peer]: checkService({name:?}) -> node {node:#x}");
                                }
                                None => {
                                    w.binder_obj(BINDER_TYPE_BINDER, 0, 0);
                                    eprintln!("E3G[peer]: checkService({name:?}) -> null");
                                }
                            }
                            let rsp = Frame {
                                ty: F_RSP,
                                seq: f.seq,
                                code: 0,
                                flags: 0,
                                target: 0,
                                data: w.buf,
                                offs: w.offs.iter().flat_map(|o| o.to_le_bytes()).collect(),
                                fds: vec![],
                            };
                            write_frame(&mut s, &rsp).unwrap();
                        }
                        3 => {
                            // addService(name, binder, allowIsolated, dumpPriority)
                            let name = r.s16().unwrap_or_default();
                            let mut node = 0u64;
                            for off in f.offs.chunks(8) {
                                let off = u64::from_le_bytes(off.try_into().unwrap()) as usize;
                                if off + 24 <= f.data.len() {
                                    let ty = u32::from_le_bytes(
                                        f.data[off..off + 4].try_into().unwrap(),
                                    );
                                    let b = u64::from_le_bytes(
                                        f.data[off + 8..off + 16].try_into().unwrap(),
                                    );
                                    if ty == BINDER_TYPE_HANDLE || ty == BINDER_TYPE_BINDER {
                                        node = b;
                                    }
                                }
                            }
                            eprintln!("E3G[peer]: addService({name:?}) -> node {node:#x}");
                            services.insert(name.clone(), node);
                            let rsp = Frame {
                                ty: F_RSP,
                                seq: f.seq,
                                code: 0,
                                flags: 0,
                                target: 0,
                                data: {
                                    let mut w = W::new();
                                    w.i32(0);
                                    w.buf
                                },
                                offs: vec![],
                                fds: vec![],
                            };
                            write_frame(&mut s, &rsp).unwrap();
                            if name == "acos.java" {
                                reverse_pending = true;
                            }
                        }
                        other => {
                            eprintln!("E3G[peer]: servicemanager code {other} not implemented");
                            let rsp = Frame {
                                ty: F_RSP,
                                seq: f.seq,
                                code: 0,
                                flags: 0,
                                target: 0,
                                data: {
                                    let mut w = W::new();
                                    w.i32(-38); // ENOSYS
                                    w.buf
                                },
                                offs: vec![],
                                fds: vec![],
                            };
                            write_frame(&mut s, &rsp).unwrap();
                        }
                    }
                } else {
                    // ---- the "activity" service: an AIDL-shaped echo
                    echo_calls += 1;
                    match f.code {
                        5 => {
                            // echo(int, String, Bundle)
                            let a = r.i32();
                            let s1 = r.s16().unwrap_or_default();
                            let bundle = r.remaining().to_vec();
                            let mut w = W::new();
                            w.i32(0); // exception code
                            w.i32(0xAC05);
                            w.i32(a);
                            w.s16(&format!("{s1}+peer"));
                            w.raw(&bundle);
                            let rsp = Frame {
                                ty: F_RSP,
                                seq: f.seq,
                                code: 0,
                                flags: 0,
                                target: 0,
                                data: w.buf,
                                offs: vec![],
                                fds: vec![],
                            };
                            write_frame(&mut s, &rsp).unwrap();
                            eprintln!("E3G[peer]: echo({a}, {s1:?}, bundle={}B) replied", bundle.len());
                        }
                        9 => {
                            // fd round trip: write into the received descriptor
                            let mut nfds = 0;
                            let mut fd = -1;
                            for off in f.offs.chunks(8) {
                                let off = u64::from_le_bytes(off.try_into().unwrap()) as usize;
                                if off + 24 <= f.data.len()
                                    && u32::from_le_bytes(f.data[off..off + 4].try_into().unwrap())
                                        == BINDER_TYPE_FD
                                {
                                    fd = u32::from_le_bytes(
                                        f.data[off + 8..off + 12].try_into().unwrap(),
                                    ) as i32;
                                    nfds += 1;
                                }
                            }
                            let msg = b"PONG-FROM-PEER\n";
                            let wrote = if fd >= 0 {
                                unsafe {
                                    let r = write_raw(fd, msg.as_ptr(), msg.len());
                                    r
                                }
                            } else {
                                -1
                            };
                            eprintln!(
                                "E3G[peer]: fd rung: {nfds} fd object(s), fd={fd}, wrote={wrote}"
                            );
                            let mut w = W::new();
                            w.i32(0);
                            w.i32(if wrote == msg.len() as isize { 1 } else { 0 });
                            let rsp = Frame {
                                ty: F_RSP,
                                seq: f.seq,
                                code: 0,
                                flags: 0,
                                target: 0,
                                data: w.buf,
                                offs: vec![],
                                fds: vec![],
                            };
                            write_frame(&mut s, &rsp).unwrap();
                        }
                        other => {
                            eprintln!("E3G[peer]: echo service code {other} unknown");
                            let mut w = W::new();
                            w.i32(-38);
                            let rsp = Frame {
                                ty: F_RSP,
                                seq: f.seq,
                                code: 0,
                                flags: 0,
                                target: 0,
                                data: w.buf,
                                offs: vec![],
                                fds: vec![],
                            };
                            write_frame(&mut s, &rsp).unwrap();
                        }
                    }
                }
                if reverse_pending {
                    // reverse call: the peer initiates a transaction into the
                    // Java Binder registered as "acos.java".
                    reverse_pending = false;
                    let node = *services.get("acos.java").unwrap_or(&0);
                    let mut w = W::new();
                    w.i32(0x80000000u32 as i32); // policy | PENALTY_GATHER
                    w.i32(-1);                   // workSourceUid
                    w.i32(0x53595354);           // "SYST" interface-token header
                    w.s16("dev.acos.e3g.IJavaService");
                    w.i32(0xBEEF);
                    w.s16("from-peer");
                    let txn = Frame {
                        ty: F_TXN,
                        seq: 9001,
                        code: 7,
                        flags: 0,
                        target: node,
                        data: w.buf,
                        offs: vec![],
                        fds: vec![],
                    };
                    write_frame(&mut s, &txn).unwrap();
                    eprintln!("E3G[peer]: sent reverse TXN seq=9001 to node {node:#x}");
                }
            }
            F_REP => {
                if f.data.len() < 12 {
                    // A stray/short frame must not kill the peer: the client may
                    // still be retrying, and a dead peer looks like a hang.
                    eprintln!("E3G[peer]: short REPLY ({} bytes) ignored", f.data.len());
                    continue;
                }
                let mut r = R::new(&f.data);
                let exception = r.i32();
                let marker = r.i32();
                let text = r.s16().unwrap_or_default();
                eprintln!(
                    "E3G[peer]: REVERSE-REPLY seq={} exception={:#x} marker={:#x} text={:?} (echo_calls={echo_calls})",
                    f.seq, exception, marker, text
                );
                println!("REVERSE_ONTRANSACT_OK exception={exception:#x} marker={marker:#x} text={text:?}");
            }
            other => eprintln!("E3G[peer]: frame type {other} ignored"),
        }
    }
}

extern "C" {
    fn write(fd: i32, buf: *const u8, n: usize) -> isize;
}

unsafe fn write_raw(fd: i32, buf: *const u8, n: usize) -> isize {
    write(fd, buf, n)
}

#[allow(dead_code)]
fn _unused(_: RawFd) {}
