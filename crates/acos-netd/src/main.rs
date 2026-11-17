//! `acos-netd` CLI: the control surface the session API (CLI/MCP `serve` verbs)
//! wraps today.
//!
//! ```text
//! acos-netd up [--pasta PATH]      start/ensure the relay, print status
//! acos-netd status                 relay + session table (JSON)
//! acos-netd spawn NAME -- CMD...   new session netns + veth, run CMD in it
//! acos-netd run   NAME -- CMD...   spawn, then wait for the guest to exit
//! acos-netd detach NAME            remove veth, rules, guest
//! acos-netd offline NAME | online NAME
//! acos-netd delay NAME --ms N [--jitter N] [--loss PCT] | clear NAME
//! acos-netd hostport add PORT [--session NAME] | hostport del PORT
//! acos-netd forward --listen 127.0.0.1:P --target ADDR:P   (inside a netns)
//! acos-netd kill-pasta             fail-closed proof hook
//! acos-netd shutdown               stop the supervisor and the relay netns
//! acos-netd serve | fabric --socket P   internal
//! ```

use std::process::ExitCode;
use std::time::Duration;

use acos_netd::client::Client;
use acos_netd::proto::{ErrorCode, ProtoError, Request};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(code) => ExitCode::from(u8::try_from(code).unwrap_or(255)),
        Err(e) => {
            eprintln!(
                "acos-netd: {}",
                serde_json::json!({"ok": false, "error": {"code": e.code, "message": e.message}})
            );
            ExitCode::from(1)
        }
    }
}

fn run(args: &[String]) -> Result<i32, ProtoError> {
    let Some(cmd) = args.first().map(String::as_str) else {
        usage();
        return Err(invalid("missing command"));
    };
    let rest = &args[1..];
    match cmd {
        "serve" => acos_netd::serve().map(|()| 0).map_err(io),
        "fabric" => {
            let sock = flag_value(rest, "--socket").ok_or_else(|| invalid("--socket"))?;
            acos_netd::supervisor::run_fabric(std::path::Path::new(&sock))
                .map(|()| 0)
                .map_err(io)
        }
        // The loopback forwarder of a host-port opt-in. Started by the
        // supervisor inside the session's netns (`unshare --net=…`), never by a
        // user: it binds the session's own `127.0.0.1`.
        "forward" => {
            let listen = flag_value(rest, "--listen").ok_or_else(|| invalid("--listen"))?;
            let target = flag_value(rest, "--target").ok_or_else(|| invalid("--target"))?;
            acos_netd::forward::serve(&listen, &target)
                .map(|()| 0)
                .map_err(io)
        }
        "up" => {
            let pasta = flag_value(rest, "--pasta");
            let mut c = Client::ensure_up()?;
            let status = if let Some(p) = pasta {
                let r = c.request(&Request::Up { pasta: Some(p) })?;
                r.status.ok_or_else(|| invalid("no status"))?
            } else {
                c.status()?
            };
            println!("{}", pretty(&status)?);
            Ok(0)
        }
        "identity" => {
            let mut c = Client::connect()?;
            println!("{}", pretty(&c.identity()?)?);
            Ok(0)
        }
        "status" => {
            let mut c = Client::connect()?;
            println!("{}", pretty(&c.status()?)?);
            Ok(0)
        }
        // `run` is the launcher's stdio proxy: the guest inherits this process's
        // own stdin/stdout/stderr (passed as SCM_RIGHTS), so logcat capture and
        // the interaction protocol are byte-identical to spawning the guest
        // directly. It exits with the guest's own status.
        "run" => {
            let (name, argv) = name_and_argv(rest)?;
            // Fail closed: an attached session on a down relay reports the
            // network unavailable instead of restarting the relay.
            let mut c = Client::attach_client()?;
            let fds = stdio_fds();
            // The run's own cgroup scope rides along: the guest is charged to
            // the scope `guarded.sh` created, so `--mem` bounds the guest on the
            // relay arm exactly as it does on the direct path.
            let cgroup = acos_netd::client::own_cgroup();
            let (s, ms) = c.spawn_stdio(&name, argv, &fds, Some(std::process::id()), cgroup)?;
            eprintln!(
                "acos-netd: session {name} attached idx={} addr={} gw={} pid={:?} attach_ms={ms:.1}",
                s.index, s.address, s.gateway, s.pid
            );
            // The guest's status is the only status this proxy may report. A
            // control-plane hiccup is not the guest exiting: re-connect and ask
            // again, and if the session is really gone say so instead of
            // pretending the guest died with the proxy's own failure code. (A
            // launcher reads this process's exit status as the guest's, so a
            // proxy error used to surface as `GUEST_EXITED … status 1`.)
            let code = wait_exit(&name, &mut c);
            // The proxy owns the session's end: veth, rules, guest. Best effort:
            // the supervisor detaches a session whose owner died anyway.
            if let Err(e) = c.detach(&name) {
                eprintln!("acos-netd: detach {name}: {}", e.message);
            }
            Ok(code)
        }
        "spawn" => {
            let (name, argv) = name_and_argv(rest)?;
            let stdout = flag_value(rest, "--stdout");
            let stderr = flag_value(rest, "--stderr");
            let mut c = Client::attach_client()?;
            let (s, ms) = c.spawn_with(&name, argv, stdout, stderr, int_value(rest, "--index"))?;
            eprintln!(
                "acos-netd: session {name} attached idx={} addr={} gw={} pid={:?} attach_ms={ms:.1}",
                s.index, s.address, s.gateway, s.pid
            );
            println!("{}", pretty(&c.status()?)?);
            Ok(0)
        }
        "detach" => {
            let name = positional(rest, 0)?;
            let mut c = Client::connect()?;
            c.detach(&name)?;
            println!("{}", pretty(&c.status()?)?);
            Ok(0)
        }
        "offline" | "online" => {
            let name = positional(rest, 0)?;
            let mut c = Client::connect()?;
            println!("{}", pretty(&c.offline(&name, cmd == "offline")?)?);
            Ok(0)
        }
        "delay" => {
            let name = positional(rest, 0)?;
            let ms = int_value(rest, "--ms").unwrap_or(0);
            let jitter = int_value(rest, "--jitter").unwrap_or(0);
            let loss = int_value(rest, "--loss").unwrap_or(0);
            let mut c = Client::connect()?;
            println!("{}", pretty(&c.delay(&name, ms, jitter, loss)?)?);
            Ok(0)
        }
        "clear" => {
            let name = positional(rest, 0)?;
            let mut c = Client::connect()?;
            println!("{}", pretty(&c.delay(&name, 0, 0, 0)?)?);
            Ok(0)
        }
        "hostport" => {
            let on = match positional(rest, 0)?.as_str() {
                "add" => true,
                "del" => false,
                other => return Err(invalid(&format!("hostport {other}: expected add|del"))),
            };
            let port: u16 = positional(rest, 1)?.parse().map_err(|_| invalid("port"))?;
            let session = flag_value(rest, "--session");
            let mut c = Client::ensure_up()?;
            println!("{}", pretty(&c.host_port(port, on, session.as_deref())?)?);
            Ok(0)
        }
        "kill-pasta" => {
            let hold = rest.iter().any(|a| a == "--hold");
            let mut c = Client::connect()?;
            c.kill_pasta(hold)?;
            eprintln!(
                "acos-netd: pasta killed; the relay is down until the supervisor restarts it"
            );
            Ok(0)
        }
        "shutdown" => {
            let mut c = Client::connect()?;
            c.shutdown().map(|()| 0)
        }
        other => {
            usage();
            Err(invalid(&format!("unknown command {other}")))
        }
    }
}

/// This process's stdin/stdout/stderr, when they are real descriptors. The
/// launcher sets them to pipes (or a log file); passing them on is what makes
/// the guest's stdio identical to a directly spawned guest. A closed slot is
/// sent as `-1` and the supervisor's per-session log file takes its place.
fn stdio_fds() -> Vec<std::os::unix::io::RawFd> {
    use std::os::unix::io::AsRawFd;
    // `stdin`/`stdout`/`stderr` are distinct types, so the three calls stay
    // explicit; `-1` marks a closed slot (the supervisor's log file replaces it).
    let mut fds = Vec::new();
    for fd in [
        std::io::stdin().as_raw_fd(),
        std::io::stdout().as_raw_fd(),
        std::io::stderr().as_raw_fd(),
    ] {
        if unsafe { libc::fcntl(fd, libc::F_GETFD) } >= 0 {
            fds.push(fd);
        } else {
            fds.push(-1);
        }
    }
    fds
}

/// The guest's exit code, asked over a connection that is re-established when
/// the control plane hiccups. `136` (128 + `SIGKILL`) when the session is gone
/// without a recorded status: a proxy that cannot answer must not invent a
/// guest status, and it must not turn its own failure into one.
fn wait_exit(name: &str, c: &mut Client) -> i32 {
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    let mut last = String::new();
    while std::time::Instant::now() < deadline {
        match c.wait_exit(name, Duration::from_millis(20)) {
            Ok(Some(code)) => return code,
            Ok(None) => {
                eprintln!("acos-netd: session {name} ended without a recorded status");
                return 136;
            }
            Err(e) => {
                last = e.message;
                // Reconnect: the supervisor may still be alive with this
                // session attached (a dropped connection is not a dead guest).
                std::thread::sleep(Duration::from_millis(50));
                match Client::connect_raw() {
                    Ok(fresh) => *c = fresh,
                    Err(e) => {
                        last = e.message;
                        std::thread::sleep(Duration::from_millis(100));
                    }
                }
            }
        }
    }
    eprintln!("acos-netd: session {name}: no exit status ({last})");
    136
}

fn pretty<T: serde::Serialize>(v: &T) -> Result<String, ProtoError> {
    serde_json::to_string_pretty(v).map_err(|e| ProtoError {
        code: ErrorCode::Io,
        message: e.to_string(),
    })
}

fn io(e: std::io::Error) -> ProtoError {
    ProtoError {
        code: ErrorCode::Io,
        message: e.to_string(),
    }
}

fn invalid(msg: &str) -> ProtoError {
    ProtoError {
        code: ErrorCode::Invalid,
        message: msg.to_string(),
    }
}

fn usage() {
    eprintln!(
        "usage: acos-netd <up|status|identity|spawn|run|detach|offline|online|delay|clear|hostport|kill-pasta|shutdown|serve|fabric|forward> [args]"
    );
}

fn positional(args: &[String], n: usize) -> Result<String, ProtoError> {
    args.iter()
        .filter(|a| !a.starts_with("--"))
        .nth(n)
        .cloned()
        .ok_or_else(|| invalid(&format!("missing argument {n}")))
}

fn flag_value(args: &[String], flag: &str) -> Option<String> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

fn int_value(args: &[String], flag: &str) -> Option<u32> {
    flag_value(args, flag).and_then(|v| v.parse().ok())
}

/// `NAME -- CMD...`
fn name_and_argv(args: &[String]) -> Result<(String, Vec<String>), ProtoError> {
    let sep = args
        .iter()
        .position(|a| a == "--")
        .ok_or_else(|| invalid("expected `NAME -- CMD...`"))?;
    let name = args
        .first()
        .filter(|n| !n.starts_with("--"))
        .cloned()
        .ok_or_else(|| invalid("missing session name"))?;
    let argv = args[sep + 1..].to_vec();
    if argv.is_empty() {
        return Err(invalid("missing command"));
    }
    Ok((name, argv))
}
