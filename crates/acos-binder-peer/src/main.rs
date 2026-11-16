//! Serve tagged binder-driver connections on a `SOCK_SEQPACKET` unix socket.
//!
//! ```text
//! acos-binder-peer /data/acos-binder.sock [unsupported.jsonl]
//! ```
//!
//! MPROC2 step 3a: the loop accepts N connections, each naming its app process
//! in a `F_HELLO` handshake (a connection with no handshake is the primary
//! process, tag 0). This binary has no launcher, so it runs until the last
//! connection closes.

use std::process::ExitCode;

use acos_binder_peer::{listen, Peer};

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
    let mut peer = match log {
        Some(p) => Peer::with_log(p),
        None => Peer::new(),
    };
    let stop = std::sync::atomic::AtomicBool::new(false);
    let rc = acos_binder_peer::serve::serve(&mut peer, listener, &stop)
        .map(|outcome| {
            eprintln!(
                "acos-binder-peer: served processes={:?} peak={} accepted={} refused={} closed={}",
                peer.tags(),
                outcome.peak_connections,
                outcome.accepted,
                outcome.refused,
                outcome.closed
            );
        });
    unsafe {
        libc::close(listener);
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
