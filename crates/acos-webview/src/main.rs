//! `acos-webview` — the host WebView broker process (WV5b).
//!
//! Two modes, both thin: the process owns a lifecycle, the crate owns the
//! behaviour.
//!
//! ```text
//! acos-webview --shared --socket <path> [--cache <dir>] [--idle-shutdown-secs N]
//!     The one broker per host (ADR-0031 item 3). `serve_shared`: every
//!     connection is one session (one browser context); the engine itself stays
//!     lazy and only starts when a session sends `hello`. Exits when no session
//!     has been attached for `--idle-shutdown-secs`.
//!
//! acos-webview --session <dir> [--session-id <id>] [--no-sandbox]
//!     One session socket, WVB's `serve()`. Handy for driving the broker by hand;
//!     the sessions themselves go through the lazy gate (`gate.rs`).
//! ```
//!
//! It is deliberately *not* the MCP broker's job: the daemon is shared by every
//! session of the host, the MCP daemon owns sessions, and neither should own the
//! other's process. A session starts this binary when a guest actually connects
//! to its WebView socket, and it outlives that session (`setsid`) until its idle
//! timer stops it.

use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use acos_webview::broker::{Broker, BrokerConfig};
use acos_webview::engine::EngineCache;
use acos_webview::server::{serve, serve_shared, ServerConfig, SharedConfig};

const USAGE: &str = "\
usage: acos-webview --shared --socket <endpoint> [--cache <dir>] [--idle-shutdown-secs N]
       acos-webview --session <dir> [--session-id <id>] [--cache <dir>] [--no-sandbox]

  --shared                 run the host-wide broker: one connection per session,
                           one browser context per session, idle exit
  --session <dir>          serve one session socket (<dir>/webview.sock)
  --cache <dir>            engine install root ($ACOS_WEBVIEW_CACHE / \
$ACOS_HOME/.cache/webview / <checkout>/.cache/webview)
  --no-sandbox             run the engine without bwrap (bare-host debugging only)
  --print-endpoint         print the shared endpoint and exit
";

struct Args {
    shared: bool,
    session_dir: Option<PathBuf>,
    session_id: Option<String>,
    cache: Option<PathBuf>,
    idle_shutdown: Option<Duration>,
    sandbox: bool,
}

fn parse() -> Result<Args, String> {
    let mut args = Args {
        shared: false,
        session_dir: None,
        session_id: None,
        cache: None,
        idle_shutdown: None,
        sandbox: true,
    };
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--shared" => args.shared = true,
            "--socket" | "--session" => {
                let v = it.next().ok_or_else(|| format!("{a} needs a value"))?;
                args.session_dir = Some(PathBuf::from(v));
            }
            "--session-id" => {
                args.session_id = Some(it.next().ok_or("--session-id needs a value")?);
            }
            "--cache" => {
                args.cache = Some(PathBuf::from(it.next().ok_or("--cache needs a value")?));
            }
            "--idle-shutdown-secs" => {
                let v: u64 = it
                    .next()
                    .ok_or("--idle-shutdown-secs needs a value")?
                    .parse()
                    .map_err(|e| format!("--idle-shutdown-secs: {e}"))?;
                args.idle_shutdown = Some(Duration::from_secs(v));
            }
            "--no-sandbox" => args.sandbox = false,
            "-h" | "--help" => {
                print!("{USAGE}");
                std::process::exit(0);
            }
            other => return Err(format!("unknown argument {other}")),
        }
    }
    Ok(args)
}

fn broker_config(args: &Args) -> BrokerConfig {
    let cache = match &args.cache {
        Some(root) => EngineCache::new(root.clone()),
        None => EngineCache::default_cache(),
    };
    let mut cfg = BrokerConfig::new(cache);
    cfg.sandbox = args.sandbox;
    if let Some(idle) = args.idle_shutdown {
        // The engine's idle stop and the process's idle exit are the same
        // decision: when it stops, nothing is left to serve.
        cfg.idle_shutdown = idle;
    }
    cfg
}

/// Close every inherited descriptor above stderr.
///
/// A daemon spawned from a session must not keep that session's pipes, log
/// fds or guest-side handles alive: the session's readers would never see EOF
/// and the session would look hung long after it exited. The daemon opens
/// everything it needs after this point (`/dev/null`, its log, the listener).
fn close_inherited_fds() {
    for fd in 3..1024 {
        // SAFETY: `close` on a descriptor this process inherited; EBADF is the
        // normal answer for the ones that are not open.
        unsafe {
            libc::close(fd);
        }
    }
}

fn main() -> ExitCode {
    let args = match parse() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("acos-webview: {e}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    if !args.shared && args.session_dir.is_none() {
        eprintln!("acos-webview: one of --shared or --session is required\n\n{USAGE}");
        return ExitCode::from(2);
    }

    let cfg = broker_config(&args);
    let broker = Broker::new(cfg.clone());
    let idle = args.idle_shutdown;

    if args.shared {
        close_inherited_fds();
        // `--socket` is the endpoint itself, not a directory: the session
        // owner (the gate) names `<runtime>/acos-wv/broker.sock` exactly.
        let socket = match &args.session_dir {
            Some(path) => path.clone(),
            None => {
                eprintln!("acos-webview: --shared needs --socket <path>");
                return ExitCode::from(2);
            }
        };
        match serve_shared(
            std::sync::Arc::clone(&broker),
            &socket,
            SharedConfig { idle_exit: idle },
        ) {
            Ok(handle) => {
                eprintln!(
                    "acos-webview: shared broker on {} (engine={}, idle={}s, pid={})",
                    socket.display(),
                    acos_webview::ENGINE_VERSION,
                    idle.map(|d| d.as_secs()).unwrap_or(0),
                    std::process::id()
                );
                handle.wait();
                broker.shutdown();
                ExitCode::SUCCESS
            }
            Err(e) => {
                // Losing the bind race is normal: another gate won. Prove the
                // winner is really there before calling it success.
                if UnixStream::connect(&socket).is_ok() {
                    eprintln!(
                        "acos-webview: {} already served by another broker; exiting",
                        socket.display()
                    );
                    return ExitCode::SUCCESS;
                }
                eprintln!("acos-webview: {e}");
                ExitCode::from(1)
            }
        }
    } else {
        let dir = args.session_dir.expect("checked above");
        std::fs::create_dir_all(&dir).ok();
        let socket = dir.join(acos_webview::server::SESSION_SOCKET_FILE);
        let session_id = args
            .session_id
            .or_else(|| {
                dir.file_name()
                    .map(|n| n.to_string_lossy().into_owned())
            })
            .unwrap_or_else(|| "session".to_string());
        let server = ServerConfig {
            socket_path: socket.clone(),
            session_id,
        };
        match serve(std::sync::Arc::clone(&broker), &server) {
            Ok(handle) => {
                eprintln!(
                    "acos-webview: session broker on {} (engine={})",
                    socket.display(),
                    acos_webview::ENGINE_VERSION
                );
                handle.wait();
                broker.shutdown();
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("acos-webview: {e}");
                ExitCode::from(1)
            }
        }
    }
}
