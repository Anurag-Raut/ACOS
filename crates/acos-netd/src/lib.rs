//! `acos-netd` — the ACOS network relay supervisor (design B2, ADR-0030
//! amendment 3): the transport half of the v0.4.0 network module.
//!
//! One relay netns + one `pasta` per host (26 MiB once), one netns + veth per
//! session (~0 MiB each), per-session online/offline and latency/loss as rules
//! and `netem` on that session's veth, and the LAN/host-loopback boundary in the
//! netns the guest holds no capability over.
//!
//! Everything is behind `ACOS_SPIKE_NET=1` until the module framework (MOD0)
//! lands [`crate::spike_flag`]; the default stays **off**.

pub mod client;
pub mod plan;
pub mod proto;
pub mod supervisor;

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use proto::{ErrorCode, Reply, Request};

/// The spike gate of the whole network module (`1` = on, unset = off).
pub const FLAG: &str = "ACOS_SPIKE_NET";

/// True when the network module is switched on.
#[must_use]
pub fn enabled() -> bool {
    matches!(std::env::var(FLAG).as_deref(), Ok("1") | Ok("true"))
}

/// Run the supervisor until killed: bind the control socket, supervise `pasta`
/// in the background, answer one request per connection.
pub fn serve() -> std::io::Result<()> {
    let dir = client::state_dir();
    let sock = client::socket_path();
    // Never clobber a live supervisor's socket: two supervisors would mean two
    // relay netns and two NAT processes on one host.
    if std::os::unix::net::UnixStream::connect(&sock).is_ok() {
        eprintln!("acos-netd: a supervisor is already running on {}", sock.display());
        return Ok(());
    }
    let _ = std::fs::remove_file(&sock);
    let listener = UnixListener::bind(&sock)?;
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&sock, std::fs::Permissions::from_mode(0o600))?;
    }
    let relay = Arc::new(Mutex::new(supervisor::Relay::new(dir.clone())));

    // Bring the relay up eagerly so the first session pays nothing but its own
    // attach, and keep supervising it (restart on death) in the background.
    {
        let relay = Arc::clone(&relay);
        std::thread::spawn(move || {
            if let Ok(mut r) = relay.lock() {
                if let Err(e) = r.ensure_up(None) {
                    let _ = r.status(); // records `down_reason`
                    eprintln!("acos-netd: relay did not start: {}", e.message);
                }
            }
            loop {
                std::thread::sleep(supervisor::TICK);
                if let Ok(mut r) = relay.lock() {
                    r.tick();
                }
            }
        });
    }

    for conn in listener.incoming() {
        let Ok(mut conn) = conn else { continue };
        let relay = Arc::clone(&relay);
        std::thread::spawn(move || {
            // One connection carries any number of requests: a client keeps its
            // handle for the life of a session (`Client` is the session API's
            // transport handle), and an idle connection costs one thread.
            let reader = match conn.try_clone() {
                Ok(c) => c,
                Err(_) => return,
            };
            let mut reader = BufReader::new(reader);
            let mut writer = conn;
            loop {
                let mut line = String::new();
                match reader.read_line(&mut line) {
                    Ok(0) | Err(_) => return,
                    Ok(_) => {}
                }
                let reply = match proto::decode_request(&line) {
                    Err(e) => Reply::fail(e.code, e.message),
                    Ok(req) => handle(&relay, req),
                };
                if writer.write_all(proto::encode(&reply).as_bytes()).is_err() {
                    return;
                }
            }
        });
    }
    Ok(())
}

fn handle(relay: &Arc<Mutex<supervisor::Relay>>, req: Request) -> Reply {
    let mut guard = match relay.lock() {
        Ok(g) => g,
        Err(_) => return Reply::fail(ErrorCode::SupervisorGone, "supervisor state poisoned"),
    };
    let r = &mut *guard;
    match &req {
        Request::Status => Reply::status(r.status()),
        Request::Up { pasta } => match r.ensure_up(pasta.clone()) {
            Ok(()) => Reply::status(r.status()),
            Err(e) => Reply::fail(e.code, e.message),
        },
        Request::Spawn { .. } => match r.spawn(&req) {
            Ok((s, ms)) => Reply {
                ok: true,
                error: None,
                status: None,
                session: Some(s),
                attach_ms: Some(ms),
            },
            Err(e) => Reply::fail(e.code, e.message),
        },
        Request::Attach { session, netns_pid, index } => match r.attach(session, *netns_pid, *index)
        {
            Ok(s) => Reply { session: Some(s), ..Reply::ok() },
            Err(e) => Reply::fail(e.code, e.message),
        },
        Request::Detach { session } => match r.detach(session) {
            Ok(s) => Reply { session: Some(s), ..Reply::ok() },
            Err(e) => Reply::fail(e.code, e.message),
        },
        Request::Offline { session, offline } => match r.set_offline(session, *offline) {
            Ok(s) => Reply { session: Some(s), ..Reply::ok() },
            Err(e) => Reply::fail(e.code, e.message),
        },
        Request::Delay { session, ms, jitter_ms, loss_pct } => {
            match r.set_delay(session, *ms, *jitter_ms, *loss_pct) {
                Ok(s) => Reply { session: Some(s), ..Reply::ok() },
                Err(e) => Reply::fail(e.code, e.message),
            }
        }
        Request::HostPort { port, on, session } => {
            // `session` is borrowed by the request; the call needs it as a
            // reference before the request is dropped.
            let name = session.clone();
            match r.host_port(*port, *on, name.as_deref()) {
                Ok(s) => Reply::status(s),
                Err(e) => Reply::fail(e.code, e.message),
            }
        }
        Request::KillPasta { hold } => match r.kill_pasta(*hold) {
            Ok(()) => Reply::ok(),
            Err(e) => Reply::fail(e.code, e.message),
        },
        Request::Shutdown => {
            r.shutdown();
            Reply::ok()
        }
    }
}

/// Wait for the supervisor to be reachable (`serve`'s callers and tests).
pub fn wait_ready(timeout: Duration) -> Result<client::Client, proto::ProtoError> {
    let deadline = std::time::Instant::now() + timeout;
    let mut last = None;
    while std::time::Instant::now() < deadline {
        match client::Client::connect() {
            Ok(c) => return Ok(c),
            Err(e) => last = Some(e.message),
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    Err(proto::ProtoError {
        code: ErrorCode::SupervisorGone,
        message: last.unwrap_or_else(|| "no supervisor".into()),
    })
}
