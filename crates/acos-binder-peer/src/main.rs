//! Serve one binder-driver connection on a `SOCK_SEQPACKET` unix socket.
//!
//! ```text
//! acos-binder-peer /data/acos-binder.sock [unsupported.jsonl]
//! ```

use std::io::Write;
use std::os::fd::RawFd;
use std::process::ExitCode;

use acos_binder_peer::{accept, listen, read_frame, write_frame, Peer};

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let path = args.next().unwrap_or_else(|| {
        std::env::var("ACOS_BINDER_SOCK")
            .or_else(|_| std::env::var("E3G_SOCK"))
            .unwrap_or_else(|_| "/data/acos-binder.sock".to_string())
    });
    let log = args.next();
    let _ = std::fs::remove_file(&path);
    let listener = match listen(&path) {
        Ok(fd) => fd,
        Err(e) => {
            eprintln!("acos-binder-peer: bind {path}: {e}");
            return ExitCode::FAILURE;
        }
    };
    eprintln!("acos-binder-peer: listening on {path}");
    let client = match accept(listener) {
        Ok(fd) => fd,
        Err(e) => {
            eprintln!("acos-binder-peer: accept: {e}");
            return ExitCode::FAILURE;
        }
    };
    unsafe {
        libc::close(listener);
    }
    eprintln!("acos-binder-peer: driver connected");
    let mut peer = match log {
        Some(p) => Peer::with_log(p),
        None => Peer::new(),
    };
    let rc = serve(&mut peer, client);
    unsafe {
        libc::close(client);
    }
    let _ = std::fs::remove_file(&path);
    match rc {
        Ok(()) => {
            eprintln!(
                "acos-binder-peer: closed (unsupported={} reverse_replies={} reverse_failed={})",
                peer.unsupported.len(),
                peer.reverse_replies,
                peer.reverse_failed
            );
            if let Some(path) = peer.log_path() {
                eprintln!("acos-binder-peer: unsupported log {}", path.display());
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("acos-binder-peer: {e}");
            ExitCode::FAILURE
        }
    }
}

fn serve(peer: &mut Peer, fd: RawFd) -> std::io::Result<()> {
    loop {
        let frame = match read_frame(fd) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(()),
            Err(e) => return Err(e),
        };
        for out in peer.handle(frame) {
            write_frame(fd, &out)?;
        }
        let _ = std::io::stderr().flush();
    }
}
