//! `acos view` — the local, view-only live view hub (VIEW1b, ADR-0027).
//!
//! One command starts an HTTP server **on 127.0.0.1 only** with a random
//! ≥128-bit token required on every request (`/?t=<token>`), and prints exactly
//! one line so an agent (or T3 Code's preview pane) can open it:
//!
//! ```text
//! acos view: http://127.0.0.1:54321/?t=<token>
//! ```
//!
//! The page lists the live sessions from the registry (client, project, APK,
//! activity, age), and clicking one shows its screen plus tap ripples, the
//! current activity and the recent action feed. Frames and events come from the
//! session's stream socket through the [`hub`], which keeps exactly one upstream
//! subscription per session while at least one browser watches it — so a session
//! nobody looks at still costs nothing (`docs/LIVE_VIEW.md` §3).
//!
//! **View-only:** there is no request shape that forwards input to a guest, no
//! file serving, and no path parameter that is used as a filesystem path (a
//! session id is only ever matched against the registry).

mod hub;
mod page;

use std::io::{self, ErrorKind, Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use serde_json::json;

pub use hub::{Hub, SessionView};

/// Largest HTTP request head the hub accepts (no bodies are read: GET only).
const MAX_HEAD: usize = 8 * 1024;
/// How long a browser may stall a single write before its connection is dropped.
const WRITE_TIMEOUT: Duration = Duration::from_secs(2);
/// How often an idle stream checks whether the browser went away.
const EOF_POLL: Duration = Duration::from_millis(500);

/// `acos view` arguments.
#[derive(Debug, clap::Args)]
pub struct ViewArgs {
    /// Bind address. Only `127.0.0.1` is accepted (ADR-0027); anything else is
    /// refused before a socket is created.
    #[arg(long, default_value = "127.0.0.1")]
    bind: String,

    /// Port to listen on (0 = pick a free one and print it).
    #[arg(long, default_value_t = 0)]
    port: u16,

    /// List the live sessions and exit (test/agent helper; no server).
    #[arg(long)]
    once: bool,

    /// JSON output: the session list with `--once`, otherwise the URL record.
    #[arg(long)]
    json: bool,

    /// Registry directory override (default `$XDG_RUNTIME_DIR/acos/live`).
    #[arg(long)]
    registry_dir: Option<PathBuf>,
}

/// Run `acos view`.
pub fn run(args: ViewArgs) -> ExitCode {
    let hub = Arc::new(Hub::new(
        args.registry_dir
            .clone()
            .unwrap_or_else(crate::live_registry::registry_dir_from_env),
    ));

    if args.once {
        let sessions = hub.list();
        if args.json {
            let doc = json!({
                "ok": true,
                "registry_dir": hub.dir().display().to_string(),
                "sessions": sessions.iter().map(SessionView::to_json).collect::<Vec<_>>(),
            });
            println!("{doc}");
        } else if sessions.is_empty() {
            println!("acos view: no live sessions in {}", hub.dir().display());
        } else {
            for session in &sessions {
                println!(
                    "{}\t{}\t{}\t{}x{}\t{}\t{}",
                    session.entry.session_id,
                    client_label(&session.entry),
                    session.entry.package,
                    session.entry.display.0,
                    session.entry.display.1,
                    session
                        .entry
                        .activity
                        .clone()
                        .unwrap_or_else(|| "-".to_string()),
                    session.entry.project_dir,
                );
            }
        }
        return ExitCode::SUCCESS;
    }

    let bind = match loopback_bind(&args.bind) {
        Ok(bind) => bind,
        Err(error) => {
            eprintln!("acos view: {error}");
            return ExitCode::from(2);
        }
    };
    let listener = match TcpListener::bind(SocketAddr::new(bind, args.port)) {
        Ok(listener) => listener,
        Err(error) => {
            eprintln!("acos view: cannot bind {bind}:{}: {error}", args.port);
            return ExitCode::from(1);
        }
    };
    let port = match listener.local_addr() {
        Ok(addr) => addr.port(),
        Err(error) => {
            eprintln!("acos view: cannot read the bound address: {error}");
            return ExitCode::from(1);
        }
    };
    let token = match random_token() {
        Ok(token) => token,
        Err(error) => {
            eprintln!("acos view: cannot read /dev/urandom: {error}");
            return ExitCode::from(1);
        }
    };
    let url = format!("http://127.0.0.1:{port}/?t={token}");
    // Exactly one line, so an agent can parse it (or T3 Code's preview pane can
    // be pointed at it). The token is in the URL only: never in a header, never
    // in a log line.
    if args.json {
        println!(
            "{}",
            json!({"url": url, "bind": "127.0.0.1", "port": port, "token": token})
        );
    } else {
        println!("acos view: {url}");
    }
    let _ = io::stdout().flush();

    serve(listener, token, hub);
    ExitCode::SUCCESS
}

/// The one address this hub may bind.
pub fn loopback_bind(bind: &str) -> Result<IpAddr, String> {
    let bind = bind.trim();
    let address: IpAddr = bind
        .parse()
        .map_err(|_| format!("--bind {bind:?} is not an IP address (only 127.0.0.1)"))?;
    if address != IpAddr::V4(Ipv4Addr::LOCALHOST) {
        return Err(format!(
            "--bind {address} refused: the live view is 127.0.0.1 only (ADR-0027)"
        ));
    }
    Ok(address)
}

/// A fresh token: 32 random bytes (256 bits) as hex. Never shorter than the
/// 128 bits ADR-0027 asks for.
pub fn random_token() -> io::Result<String> {
    let mut bytes = [0u8; 32];
    let mut urandom = std::fs::File::open("/dev/urandom")?;
    urandom.read_exact(&mut bytes)?;
    let mut token = String::with_capacity(64);
    for byte in bytes {
        use std::fmt::Write as _;
        let _ = write!(token, "{byte:02x}");
    }
    Ok(token)
}

/// Constant-time token comparison (the byte loop never short-circuits; only the
/// length is compared out of band, and the length is public by construction).
#[must_use]
pub fn token_matches(expected: &str, given: &[u8]) -> bool {
    let expected = expected.as_bytes();
    let mut difference: u8 = 0;
    for (index, byte) in expected.iter().enumerate() {
        let candidate = given.get(index).copied().unwrap_or(0);
        difference |= byte ^ candidate;
    }
    difference == 0 && given.len() == expected.len()
}

/// Accept loop: one thread per browser connection.
pub fn serve(listener: TcpListener, token: String, hub: Arc<Hub>) {
    let token = Arc::new(token);
    for connection in listener.incoming() {
        let Ok(connection) = connection else { continue };
        let hub = Arc::clone(&hub);
        let token = Arc::clone(&token);
        std::thread::spawn(move || handle(connection, &token, &hub));
    }
}

fn handle(mut connection: TcpStream, token: &str, hub: &Arc<Hub>) {
    let _ = connection.set_read_timeout(Some(Duration::from_secs(15)));
    let _ = connection.set_write_timeout(Some(WRITE_TIMEOUT));
    let Some(request) = read_request(&mut connection) else {
        return;
    };
    if request.method != "GET" {
        let _ = respond(&mut connection, 405, "text/plain; charset=utf-8", b"405 method not allowed\n");
        return;
    }
    // Every route requires the token, including the page and the streams.
    let authorized = request
        .param("t")
        .is_some_and(|given| token_matches(token, given.as_bytes()));
    if !authorized {
        let _ = respond(
            &mut connection,
            403,
            "text/plain; charset=utf-8",
            b"403 forbidden: a valid ?t=<token> is required\n",
        );
        return;
    }

    let path = request.path.as_str();
    if path == "/" || path == "/index.html" {
        let _ = respond(
            &mut connection,
            200,
            "text/html; charset=utf-8",
            page::PAGE.as_bytes(),
        );
    } else if path == "/api/sessions" {
        let sessions = hub.list();
        let body = json!({
            "ok": true,
            "now_ms": crate::live_registry::now_ms(),
            "sessions": sessions.iter().map(SessionView::to_json).collect::<Vec<_>>(),
        })
        .to_string();
        let _ = respond(
            &mut connection,
            200,
            "application/json; charset=utf-8",
            body.as_bytes(),
        );
    } else if let Some(session_id) = path.strip_prefix("/api/frames/") {
        stream_frames(connection, hub, decode_component(session_id));
    } else if let Some(session_id) = path.strip_prefix("/api/events/") {
        stream_events(connection, hub, decode_component(session_id));
    } else {
        let _ = respond(
            &mut connection,
            404,
            "text/plain; charset=utf-8",
            b"404 not found\n",
        );
    }
}

/// MJPEG: the newest frame, repeatedly, on one multipart response.
fn stream_frames(mut connection: TcpStream, hub: &Arc<Hub>, session_id: String) {
    let Some(watcher) = hub.watch(&session_id) else {
        let _ = respond(
            &mut connection,
            404,
            "text/plain; charset=utf-8",
            b"404 no live stream for that session\n",
        );
        return;
    };
    let _ = connection.set_write_timeout(Some(WRITE_TIMEOUT));
    if write!(
        connection,
        "HTTP/1.1 200 OK\r\n\
         Content-Type: multipart/x-mixed-replace; boundary=acosframe\r\n\
         Cache-Control: no-store\r\n\
         X-Content-Type-Options: nosniff\r\n\
         Connection: close\r\n\r\n"
    )
    .is_err()
    {
        return;
    }
    let _ = connection.flush();

    let mut cursor = 0u64;
    loop {
        match watcher.wait_frame(cursor, EOF_POLL) {
            Some(frame) => {
                cursor = frame.index;
                let content_type = match frame.format.as_str() {
                    "jpeg" | "jpg" => "image/jpeg",
                    _ => "image/png",
                };
                let part = format!(
                    "--acosframe\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\n\
                     X-Timestamp: {}\r\nX-Frame-Seq: {}\r\nX-Frame-Size: {}x{}\r\n\r\n",
                    frame.bytes.len(),
                    frame.ts_ms,
                    frame.seq,
                    frame.w,
                    frame.h,
                );
                if connection.write_all(part.as_bytes()).is_err()
                    || connection.write_all(&frame.bytes).is_err()
                    || connection.write_all(b"\r\n").is_err()
                    || connection.flush().is_err()
                {
                    break; // slow or closed browser: never block the producer
                }
            }
            None => {
                if peer_closed(&connection) {
                    break;
                }
            }
        }
    }
    // Dropping `watcher` closes the upstream subscription when this was the
    // last browser on the session.
}

/// SSE: the event feed plus one `state` message describing the session.
fn stream_events(mut connection: TcpStream, hub: &Arc<Hub>, session_id: String) {
    let Some(watcher) = hub.watch(&session_id) else {
        let _ = respond(
            &mut connection,
            404,
            "text/plain; charset=utf-8",
            b"404 no live stream for that session\n",
        );
        return;
    };
    let _ = connection.set_write_timeout(Some(WRITE_TIMEOUT));
    if write!(
        connection,
        "HTTP/1.1 200 OK\r\n\
         Content-Type: text/event-stream\r\n\
         Cache-Control: no-store\r\n\
         X-Accel-Buffering: no\r\n\
         Connection: close\r\n\r\n"
    )
    .is_err()
    {
        return;
    }

    let state = json!({
        "entry": watcher.entry().to_json(),
        "hello": watcher.hello(),
        "connected": watcher.stream_state().0,
    });
    if send_event(&mut connection, "state", &state.to_string()).is_err() {
        return;
    }

    let mut cursor = watcher.event_cursor();
    loop {
        let batch = watcher.wait_events(cursor, Duration::from_secs(5));
        if batch.missed > 0 {
            let missed = json!({"missed": batch.missed});
            if send_event(&mut connection, "lag", &missed.to_string()).is_err() {
                break;
            }
        }
        if batch.events.is_empty() {
            // Keep-alive: also how a vanished browser is noticed on a quiet feed.
            if connection.write_all(b": keepalive\n\n").is_err() || connection.flush().is_err() {
                break;
            }
            if peer_closed(&connection) {
                break;
            }
            continue;
        }
        let mut failed = false;
        for event in &batch.events {
            cursor = event.index;
            let payload = json!({
                "index": event.index,
                "kind": event.kind,
                "ts_ms": event.ts_ms,
                "event": event.value,
            });
            if send_event(&mut connection, "ev", &payload.to_string()).is_err() {
                failed = true;
                break;
            }
        }
        if failed || connection.flush().is_err() {
            break;
        }
    }
}

fn send_event(connection: &mut TcpStream, name: &str, data: &str) -> io::Result<()> {
    // A data line may not contain a newline; JSON never does.
    write!(connection, "event: {name}\ndata: {data}\n\n")?;
    connection.flush()
}

/// `true` when the peer has closed its side (a browser tab that went away).
/// No data is consumed and a quiet socket is not an error.
fn peer_closed(connection: &TcpStream) -> bool {
    use std::os::unix::io::AsRawFd;
    let mut byte = 0u8;
    let read = unsafe {
        libc::recv(
            connection.as_raw_fd(),
            &mut byte as *mut u8 as *mut libc::c_void,
            1,
            libc::MSG_PEEK | libc::MSG_DONTWAIT,
        )
    };
    if read == 0 {
        return true;
    }
    if read > 0 {
        return false;
    }
    io::Error::last_os_error().kind() != ErrorKind::WouldBlock
}

fn respond(
    connection: &mut TcpStream,
    status: u16,
    content_type: &str,
    body: &[u8],
) -> io::Result<()> {
    let reason = match status {
        200 => "OK",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        _ => "Error",
    };
    write!(
        connection,
        "HTTP/1.1 {status} {reason}\r\n\
         Content-Type: {content_type}\r\n\
         Content-Length: {}\r\n\
         Cache-Control: no-store\r\n\
         X-Content-Type-Options: nosniff\r\n\
         Referrer-Policy: no-referrer\r\n\
         Connection: close\r\n\r\n",
        body.len()
    )?;
    connection.write_all(body)?;
    connection.flush()
}

struct Request {
    method: String,
    path: String,
    query: String,
}

impl Request {
    fn param(&self, name: &str) -> Option<String> {
        for pair in self.query.split('&') {
            let (key, value) = pair.split_once('=')?;
            if key == name {
                return Some(decode_component(value));
            }
        }
        None
    }
}

fn read_request(connection: &mut TcpStream) -> Option<Request> {
    let mut head = Vec::with_capacity(512);
    let mut buffer = [0u8; 1024];
    loop {
        if head.windows(4).any(|window| window == b"\r\n\r\n") {
            break;
        }
        if head.len() > MAX_HEAD {
            return None;
        }
        match connection.read(&mut buffer) {
            Ok(0) => return None,
            Ok(read) => head.extend_from_slice(&buffer[..read]),
            Err(error) if error.kind() == ErrorKind::Interrupted => continue,
            Err(_) => return None,
        }
    }
    let text = String::from_utf8_lossy(&head);
    let line = text.lines().next()?;
    let mut fields = line.split_whitespace();
    let method = fields.next()?.to_string();
    let target = fields.next()?.to_string();
    let (path, query) = match target.split_once('?') {
        Some((path, query)) => (path.to_string(), query.to_string()),
        None => (target, String::new()),
    };
    Some(Request {
        method,
        path,
        query,
    })
}

/// Minimal percent decoding (`%2F` etc.); the token is hex, but a session id in
/// a URL may arrive encoded.
fn decode_component(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[index + 1..index + 3]).unwrap_or("");
            if let Ok(byte) = u8::from_str_radix(hex, 16) {
                out.push(byte);
                index += 3;
                continue;
            }
        }
        out.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&out).to_string()
}

fn client_label(entry: &hub::Entry) -> String {
    if entry.client_version.is_empty() {
        if entry.client_name.is_empty() {
            "-".to_string()
        } else {
            entry.client_name.clone()
        }
    } else {
        format!("{} {}", entry.client_name, entry.client_version)
    }
}

impl SessionView {
    /// The JSON shape `/api/sessions` and `--once --json` emit.
    #[must_use]
    pub fn to_json(&self) -> serde_json::Value {
        let mut doc = self.entry.to_json();
        if let Some(object) = doc.as_object_mut() {
            object.insert("watchers".to_string(), json!(self.watchers));
            object.insert("connected".to_string(), json!(self.connected));
            object.insert("age_ms".to_string(), json!(age_ms(&self.entry)));
        }
        doc
    }
}

fn age_ms(entry: &hub::Entry) -> u64 {
    crate::live_registry::now_ms().saturating_sub(entry.started_ms)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::BufRead;
    use std::net::TcpStream;
    use std::os::unix::net::UnixListener;

    fn scratch(tag: &str) -> PathBuf {
        let root = std::env::var_os("ACOS_VIEW_TEST_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                std::env::current_dir()
                    .unwrap_or_else(|_| PathBuf::from("."))
                    .join(".cache/lv")
            });
        // Short paths: a Unix socket path must fit in `sun_path` (< 108 bytes).
        let dir = root.join(format!("{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch");
        dir
    }

    /// A started hub on an ephemeral port plus its token.
    struct Server {
        port: u16,
        token: String,
        hub: Arc<Hub>,
        registry: PathBuf,
    }

    impl Server {
        fn start(tag: &str) -> Self {
            let registry = scratch(&format!("{tag}-registry"));
            let token = random_token().expect("token");
            let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind");
            let port = listener.local_addr().expect("addr").port();
            let hub = Arc::new(Hub::new(registry.clone()));
            let served_token = token.clone();
            let served_hub = Arc::clone(&hub);
            std::thread::spawn(move || serve(listener, served_token, served_hub));
            Self {
                port,
                token,
                hub,
                registry,
            }
        }

        fn url(&self, path: &str) -> String {
            if path.contains('?') {
                format!("http://127.0.0.1:{}{path}&t={}", self.port, self.token)
            } else {
                format!("http://127.0.0.1:{}{path}?t={}", self.port, self.token)
            }
        }

        fn raw(&self, path: &str) -> Vec<u8> {
            get(self.port, path)
        }
    }

    impl Drop for Server {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.registry);
        }
    }

    fn get(port: u16, path: &str) -> Vec<u8> {
        let mut stream = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).expect("connect");
        let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
        write!(
            stream,
            "GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n"
        )
        .expect("write request");
        let mut response = Vec::new();
        let _ = stream.read_to_end(&mut response);
        response
    }

    fn status_of(response: &[u8]) -> u16 {
        let text = String::from_utf8_lossy(response);
        text.split_whitespace()
            .nth(1)
            .and_then(|code| code.parse().ok())
            .unwrap_or(0)
    }

    fn body_of(response: &[u8]) -> &[u8] {
        let position = response
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .expect("headers")
            + 4;
        &response[position..]
    }

    fn read_response_head(stream: &mut TcpStream) -> String {
        let mut reader = std::io::BufReader::new(stream.try_clone().expect("clone"));
        let mut head = String::new();
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line).unwrap_or(0) == 0 {
                break;
            }
            let done = line == "\r\n";
            head.push_str(&line);
            if done {
                break;
            }
        }
        head
    }

    #[test]
    fn the_token_is_required_on_every_route() {
        let server = Server::start("tok");
        // No token, an empty token, a wrong token: 403 everywhere.
        for path in ["/", "/api/sessions", "/api/frames/x", "/api/events/x"] {
            let response = server.raw(path);
            assert_eq!(status_of(&response), 403, "{path} without a token");
        }
        let mut wrong = server.raw("/api/sessions");
        assert_eq!(status_of(&wrong), 403);
        wrong = server.raw(&format!("/api/sessions?t={}00", server.token));
        assert_eq!(status_of(&wrong), 403, "a wrong token is refused");
        // With the token the page and the API answer.
        let page = server.raw(&format!("/?t={}", server.token));
        assert_eq!(status_of(&page), 200);
        let html = String::from_utf8_lossy(body_of(&page)).to_string();
        assert!(html.starts_with("<!doctype html>"));
        assert!(html.contains("/api/sessions"));
        let sessions = server.raw(&format!("/api/sessions?t={}", server.token));
        assert_eq!(status_of(&sessions), 200);
        let doc: serde_json::Value = serde_json::from_slice(body_of(&sessions)).expect("json");
        assert_eq!(doc["ok"], true);
        assert_eq!(doc["sessions"].as_array().map(Vec::len), Some(0));
        // A GET on an unknown path with a token is a 404, not the page.
        assert_eq!(
            status_of(&server.raw(&format!("/nope?t={}", server.token))),
            404
        );
        // Non-GET is refused (nothing in the hub accepts input anyway).
        let mut stream = TcpStream::connect((Ipv4Addr::LOCALHOST, server.port)).expect("connect");
        write!(
            stream,
            "POST /?t={} HTTP/1.1\r\nHost: x\r\nContent-Length: 0\r\n\r\n",
            server.token
        )
        .expect("write");
        let mut response = Vec::new();
        let _ = stream.read_to_end(&mut response);
        assert_eq!(status_of(&response), 405);
    }

    #[test]
    fn the_token_comparison_is_constant_time_and_length_safe() {
        let token = "deadbeef";
        assert!(token_matches(token, b"deadbeef"));
        assert!(!token_matches(token, b"deadbeee"));
        assert!(!token_matches(token, b"deadbee"));
        assert!(!token_matches(token, b"deadbeeff"));
        assert!(!token_matches(token, b""));
        assert!(!token_matches(token, b"DEADBEEF"));
        // A random token is at least 128 bits of entropy, as hex.
        let random = random_token().expect("token");
        assert_eq!(random.len(), 64);
        assert!(random.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(random, random_token().expect("token"));
    }

    #[test]
    fn only_loopback_may_be_bound() {
        assert!(loopback_bind("127.0.0.1").is_ok());
        for refused in ["0.0.0.0", "::", "::1", "192.168.1.10", "localhost", ""] {
            assert!(
                loopback_bind(refused).is_err(),
                "{refused:?} must be refused"
            );
        }
        assert!(loopback_bind("0.0.0.0")
            .unwrap_err()
            .contains("127.0.0.1 only"));
    }

    #[test]
    fn the_listing_hides_stale_sessions_and_reports_metadata() {
        let server = Server::start("lst");
        // One live entry (this test process is the daemon) and one dead one.
        let socket = server.registry.join("fake.sock");
        let _ = UnixListener::bind(&socket).expect("bind");
        let live = json!({
            "v": 1,
            "session_id": "s_live",
            "daemon_pid": std::process::id(),
            "launcher_pid": 4242,
            "started_ms": crate::live_registry::now_ms().saturating_sub(1500),
            "client": {"name": "t3-code", "version": "1.2.3"},
            "project_dir": "/home/Qui/Projects/gaten",
            "apk": "/tmp/gaten.apk",
            "package": "com.gaten",
            "activity": "com.gaten/.MainActivity",
            "display": {"w": 1080, "h": 2400},
            "stream_socket": socket.display().to_string(),
        });
        std::fs::write(
            server.registry.join(format!("s_live-{}.json", std::process::id())),
            live.to_string(),
        )
        .expect("write live");
        let mut child = std::process::Command::new("/bin/true")
            .spawn()
            .expect("spawn true");
        let dead_pid = child.id();
        let _ = child.wait();
        let mut dead = live.clone();
        dead["session_id"] = json!("s_dead");
        dead["daemon_pid"] = json!(dead_pid);
        let dead_path = server.registry.join(format!("s_dead-{dead_pid}.json"));
        std::fs::write(&dead_path, dead.to_string()).expect("write dead");

        let response = server.raw(&format!("/api/sessions?t={}", server.token));
        assert_eq!(status_of(&response), 200);
        let doc: serde_json::Value = serde_json::from_slice(body_of(&response)).expect("json");
        let sessions = doc["sessions"].as_array().expect("sessions");
        assert_eq!(sessions.len(), 1, "the stale entry is hidden: {sessions:?}");
        let session = &sessions[0];
        assert_eq!(session["session_id"], "s_live");
        assert_eq!(session["client"]["name"], "t3-code");
        assert_eq!(session["client"]["version"], "1.2.3");
        assert_eq!(session["project_dir"], "/home/Qui/Projects/gaten");
        assert_eq!(session["package"], "com.gaten");
        assert_eq!(session["activity"], "com.gaten/.MainActivity");
        assert_eq!(session["display"]["w"], 1080);
        assert_eq!(session["stream"], true);
        assert_eq!(session["watchers"], 0);
        assert!(session["age_ms"].as_u64().unwrap_or(0) >= 1500);
        assert!(!dead_path.exists(), "the sweep removed the stale file");

        // A metadata-only entry (no socket) is listed with stream: false.
        let mut metadata_only = live.clone();
        metadata_only["session_id"] = json!("s_meta");
        metadata_only["stream_socket"] = json!(null);
        std::fs::write(
            server
                .registry
                .join(format!("s_meta-{}.json", std::process::id())),
            metadata_only.to_string(),
        )
        .expect("write meta");
        let doc: serde_json::Value = serde_json::from_slice(body_of(
            &server.raw(&format!("/api/sessions?t={}", server.token)),
        ))
        .expect("json");
        let sessions = doc["sessions"].as_array().expect("sessions");
        assert_eq!(sessions.len(), 2);
        let meta = sessions
            .iter()
            .find(|session| session["session_id"] == "s_meta")
            .expect("metadata-only session");
        assert_eq!(meta["stream"], false);
    }

    #[test]
    fn frames_and_events_stream_and_close_the_upstream_with_the_last_browser() {
        let server = Server::start("strm");
        // A protocol-v1 fake producer on a Unix socket in the scratch dir.
        let socket_path = server.registry.join("v.sock");
        let listener = UnixListener::bind(&socket_path).expect("bind producer");
        let accepts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let serving = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let accepts_thread = Arc::clone(&accepts);
        let serving_thread = Arc::clone(&serving);
        std::thread::spawn(move || {
            use std::io::Write;
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                accepts_thread.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let serving_thread = Arc::clone(&serving_thread);
                std::thread::spawn(move || {
                    let mut reader = std::io::BufReader::new(stream.try_clone().expect("clone"));
                    let mut line = String::new();
                    let _ = reader.read_line(&mut line);
                    serving_thread.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    let hello = br#"{"v":1,"session":7,"display":{"w":1080,"h":2400},"format":"png","scale":0.5}"#;
                    let _ = send(&mut stream, b'H', hello);
                    // 1x1 PNG bytes (header + IDAT + IEND; the hub never decodes).
                    let png: &[u8] = &[
                        0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 0x0d, b'I',
                        b'H', b'D', b'R', 0, 0, 0, 1, 0, 0, 0, 1, 8, 6, 0, 0, 0, 0x1f, 0x15,
                        0xc4, 0x89, 0, 0, 0, 0x0a, b'I', b'D', b'A', b'T', 0x78, 0x9c, 0x63,
                        0x00, 0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0d, 0x0a, 0x2d, 0xb4, 0,
                        0, 0, 0, 0, b'I', b'E', b'N', b'D', 0xae, 0x42, 0x60, 0x82,
                    ];
                    let mut index = 0u64;
                    loop {
                        index += 1;
                        let header = format!(
                            "{{\"seq\":{index},\"generation\":1,\"w\":1,\"h\":1,\"format\":\"png\",\"ts_ms\":{}}}\n",
                            crate::live_registry::now_ms()
                        );
                        let mut payload = header.into_bytes();
                        payload.extend_from_slice(png);
                        if send(&mut stream, b'F', &payload).is_err() {
                            break;
                        }
                        let event = format!(
                            "{{\"ts_ms\":{},\"kind\":\"action\",\"cmd\":\"tap\",\"x\":540,\"y\":1200,\"status\":\"ok\"}}",
                            crate::live_registry::now_ms()
                        );
                        if send(&mut stream, b'E', event.as_bytes()).is_err() {
                            break;
                        }
                        std::thread::sleep(Duration::from_millis(30));
                        if index > 5_000 {
                            break;
                        }
                    }
                    serving_thread.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
                });
            }
        });
        let live = json!({
            "v": 1,
            "session_id": "s_stream",
            "daemon_pid": std::process::id(),
            "started_ms": crate::live_registry::now_ms(),
            "client": {"name": "http-test", "version": "1"},
            "project_dir": "/tmp",
            "apk": "x.apk",
            "package": "x",
            "activity": "x/.Main",
            "display": {"w": 1080, "h": 2400},
            "stream_socket": socket_path.display().to_string(),
        });
        std::fs::write(
            server
                .registry
                .join(format!("s_stream-{}.json", std::process::id())),
            live.to_string(),
        )
        .expect("write live");

        // Frames: two browsers, one upstream, multipart image parts.
        let mut first = TcpStream::connect((Ipv4Addr::LOCALHOST, server.port)).expect("connect");
        write!(
            first,
            "GET /api/frames/s_stream?t={} HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n",
            server.token
        )
        .expect("write");
        let head = read_response_head(&mut first);
        assert!(head.starts_with("HTTP/1.1 200"), "{head}");
        assert!(head.contains("multipart/x-mixed-replace"), "{head}");
        let mut second = TcpStream::connect((Ipv4Addr::LOCALHOST, server.port)).expect("connect");
        write!(
            second,
            "GET /api/frames/s_stream?t={} HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n",
            server.token
        )
        .expect("write");
        let head = read_response_head(&mut second);
        assert!(head.starts_with("HTTP/1.1 200"), "{head}");

        let mut buf = [0u8; 4096];
        let mut seen = Vec::new();
        let start = std::time::Instant::now();
        while start.elapsed() < Duration::from_secs(10) {
            if let Ok(read) = first.read(&mut buf) {
                seen.extend_from_slice(&buf[..read]);
            }
            if seen.windows(4).any(|window| window == b"PNG") {
                break;
            }
        }
        assert!(
            String::from_utf8_lossy(&seen).contains("Content-Type: image/png"),
            "a multipart part names its image type"
        );
        assert_eq!(
            accepts.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "two frame watchers share one upstream subscription"
        );

        // The events stream reports the session state and the tap event.
        let mut events = TcpStream::connect((Ipv4Addr::LOCALHOST, server.port)).expect("connect");
        write!(
            events,
            "GET /api/events/s_stream?t={} HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n",
            server.token
        )
        .expect("write");
        let head = read_response_head(&mut events);
        assert!(head.contains("text/event-stream"), "{head}");
        let mut text = String::new();
        let start = std::time::Instant::now();
        while start.elapsed() < Duration::from_secs(10) {
            if let Ok(read) = events.read(&mut buf) {
                text.push_str(&String::from_utf8_lossy(&buf[..read]));
            }
            if text.contains("event: ev") && text.contains("\"cmd\":\"tap\"") {
                break;
            }
        }
        assert!(text.contains("event: state"), "the state event: {text}");
        assert!(text.contains("\"session_id\":\"s_stream\""), "{text}");
        assert!(text.contains("\"cmd\":\"tap\""), "the tap event: {text}");
        assert!(text.contains("\"x\":540"), "{text}");
        assert_eq!(
            accepts.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "the events watcher joins the same upstream subscription"
        );

        // Closing every browser closes the one upstream subscription.
        drop(first);
        drop(second);
        drop(events);
        let start = std::time::Instant::now();
        while start.elapsed() < Duration::from_secs(10) {
            if serving.load(std::sync::atomic::Ordering::SeqCst) == 0 {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(
            serving.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "the producer is back to zero subscribers"
        );
        let stats = server.hub.stats();
        assert_eq!(stats.len(), 1);
        assert_eq!(stats[0].watchers, 0);
        assert!(!stats[0].connected);
        assert_eq!(stats[0].upstream_connects, 1);
    }

    fn send(stream: &mut std::os::unix::net::UnixStream, kind: u8, payload: &[u8]) -> io::Result<()> {
        let length = (payload.len() + 1) as u32;
        stream.write_all(&length.to_be_bytes())?;
        stream.write_all(&[kind])?;
        stream.write_all(payload)?;
        stream.flush()
    }
}
