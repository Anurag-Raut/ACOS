//! Stand-in for ACOS `ServiceHost` / the native service layer, for experiment C.
//!
//! It speaks the **real** Android `Parcel` byte format (decoded by the ACOS
//! crate `acos-parcel`, linked by path) over a unix socket, so the Java/native
//! side is exercised exactly the way a real AIDL transact would exercise it —
//! minus binder itself.
//!
//! Frame: `u32 LE length` + parcel bytes.
//!
//! Request parcel (written by the native probe, byte-identical to what a
//! `Foo$Stub$Proxy` would write):
//! ```text
//! int32  strictModePolicy | 0x80000000
//! int32  workSourceUid (-1)
//! int32  0x53595354                        ("SYST" interface-token header)
//! String16 descriptor                       "dev.acos.artprobe.Probe"
//! int32  sequence
//! String16 payload
//! ```
//! Reply parcel:
//! ```text
//! int32  status (0)
//! String16 reply text
//! int64  server-side handling time, ns
//! ```
//!
//! Usage: `acos-probe-service <socket-path> [--conns N]`

use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::time::Instant;

use acos_parcel::Parcel;

const DESCRIPTOR: &str = "dev.acos.artprobe.Probe";

fn read_frame(s: &mut UnixStream) -> std::io::Result<Option<Vec<u8>>> {
    let mut hdr = [0u8; 4];
    match s.read_exact(&mut hdr) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let len = u32::from_le_bytes(hdr) as usize;
    let mut body = vec![0u8; len];
    s.read_exact(&mut body)?;
    Ok(Some(body))
}

fn write_frame(s: &mut UnixStream, body: &[u8]) -> std::io::Result<()> {
    s.write_all(&(body.len() as u32).to_le_bytes())?;
    s.write_all(body)
}

/// The "service": validate the interface token with the real parcel codec and
/// return a reply.  A wrong token is a hard error, exactly like
/// `Parcel::enforceInterface`.
fn dispatch(req: &[u8]) -> (i32, String, i64) {
    let t0 = Instant::now();
    let mut p = Parcel::new();
    p.unmarshall(req);
    if let Err(e) = p.enforce_interface(DESCRIPTOR) {
        return (1, format!("bad-token: {e}"), 0);
    }
    let seq = p.read_int32().unwrap_or(-1);
    let payload = p.read_string16().ok().flatten().unwrap_or_default();
    let reply = format!("pong[{seq}]:{}", payload.to_uppercase());
    (0, reply, t0.elapsed().as_nanos() as i64)
}

fn serve(conn: UnixStream) {
    let mut s = conn;
    while let Ok(Some(req)) = read_frame(&mut s) {
        let (status, text, ns) = dispatch(&req);
        let mut r = Parcel::new();
        r.write_int32(status);
        let _ = r.write_string16(&text);
        r.write_int64(ns);
        if write_frame(&mut s, &r.marshall()).is_err() {
            break;
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let path = args.get(1).cloned().unwrap_or_else(|| {
        eprintln!("usage: acos-probe-service <socket-path>");
        std::process::exit(2);
    });
    let _ = std::fs::remove_file(&path);
    let listener = match UnixListener::bind(&path) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("bind {path}: {e}");
            std::process::exit(1);
        }
    };
    println!("service listening on {path}");
    for conn in listener.incoming() {
        match conn {
            Ok(c) => serve(c), // single-threaded: latency is what we measure
            Err(e) => eprintln!("accept: {e}"),
        }
    }
}
