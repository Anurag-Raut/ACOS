//! The loopback forwarder of a host-port opt-in.
//!
//! One `acos-netd forward` process runs **inside a session's netns** and binds
//! `127.0.0.1:<port>` there. An app in the session that connects to `localhost`
//! (Metro's `ws://localhost:8081`, anything an `adb reverse` would serve) reaches
//! it, and it dials the relay's host-loopback map address — the only path from a
//! session netns to the host's own `127.0.0.1` (`plan::host_loopback_map`).
//!
//! It exists only for the ports a session opted in to ([`crate::plan::hostport_allow_rule`]
//! opens the matching relay rule), it is a child of the supervisor, and the
//! supervisor kills it when the session detaches. No opt-in, no listener: a
//! connection to `127.0.0.1:<port>` is then refused, exactly as on a phone that
//! has no `adb reverse` for it.

use std::io::copy;
use std::net::{Shutdown, TcpListener, TcpStream};

/// Serve `listen` until killed: accept, dial `target`, copy both ways.
pub fn serve(listen: &str, target: &str) -> std::io::Result<()> {
    let listener = TcpListener::bind(listen)?;
    eprintln!("acos-netd: forward {listen} -> {target}");
    for conn in listener.incoming() {
        let Ok(client) = conn else { continue };
        let target = target.to_string();
        std::thread::spawn(move || {
            if let Err(e) = proxy(client, &target) {
                eprintln!("acos-netd: forward {target}: {e}");
            }
        });
    }
    Ok(())
}

/// One connection: two directions, half-close propagated, both threads reaped.
fn proxy(client: TcpStream, target: &str) -> std::io::Result<()> {
    let upstream = TcpStream::connect(target)?;
    // A dev server's traffic is small and latency-sensitive (a websocket
    // handshake); Nagle would add tens of milliseconds to each small frame.
    let _ = client.set_nodelay(true);
    let _ = upstream.set_nodelay(true);
    let mut from_client = client.try_clone()?;
    let mut to_client = client;
    let mut from_upstream = upstream.try_clone()?;
    let mut to_upstream = upstream;
    let up = std::thread::spawn(move || {
        let n = copy(&mut from_client, &mut to_upstream);
        // EOF from the client must reach the server (an HTTP request ends with
        // a half-close in some clients).
        let _ = to_upstream.shutdown(Shutdown::Write);
        n
    });
    let down = copy(&mut from_upstream, &mut to_client);
    let _ = to_client.shutdown(Shutdown::Write);
    let _ = up.join();
    down.map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    /// The forwarder really moves bytes both ways and closes the connection when
    /// the upstream does (a control-plane proof, no netns needed).
    #[test]
    fn forwards_both_directions_and_propagates_eof() {
        let target = TcpListener::bind("127.0.0.1:0").unwrap();
        let target_addr = target.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut s, _) = target.accept().unwrap();
            let mut buf = [0u8; 4];
            s.read_exact(&mut buf).unwrap();
            assert_eq!(&buf, b"ping");
            s.write_all(b"pong").unwrap();
            drop(s);
        });
        // A free port for the forwarder: bind, read the port, drop.
        let probe = TcpListener::bind("127.0.0.1:0").unwrap();
        let listen = probe.local_addr().unwrap();
        drop(probe);
        let l = listen.to_string();
        let t = target_addr.to_string();
        std::thread::spawn(move || serve(&l, &t).unwrap());
        let mut c = None;
        for _ in 0..100 {
            match TcpStream::connect(listen) {
                Ok(s) => {
                    c = Some(s);
                    break;
                }
                Err(_) => std::thread::sleep(std::time::Duration::from_millis(10)),
            }
        }
        let mut c = c.expect("forwarder accepts");
        c.write_all(b"ping").unwrap();
        let mut buf = [0u8; 4];
        c.read_exact(&mut buf).unwrap();
        assert_eq!(&buf, b"pong");
        // The upstream dropped the connection: the client sees EOF, not a hang.
        let mut rest = Vec::new();
        c.read_to_end(&mut rest).unwrap();
        server.join().unwrap();
    }
}
