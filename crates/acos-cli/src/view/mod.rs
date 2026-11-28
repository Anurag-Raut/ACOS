//! `acos view` — the local, interactive live view hub (ADR-0027 + HUMANVIEW1).
//!
//! One command starts an HTTP server **on 127.0.0.1 only** with a random
//! ≥128-bit token required on every request (`/?t=<token>`), and prints exactly
//! one line so an agent (or T3 Code's preview pane) can open it:
//!
//! ```text
//! acos view: http://127.0.0.1:54321/?t=<token>
//! ```
//!
//! The page lists the live sessions from the registry, and clicking one shows
//! its screen plus tap ripples, the current activity and the recent action feed.
//! Frames and events come from the session's stream socket through the [`hub`],
//! which keeps one upstream subscription per session while at least one
//! browser watches it — so a session nobody looks at still costs nothing.
//! Interactive input arrives over `/api/input/<session_id>?t=<token>` and is
//! dispatched per session in order through `panel_*` (`source: panel`), except
//! verify runs which stay strictly watch-only.

mod hub;
mod keycodes;
mod page;
mod ws;

use std::io::{self, ErrorKind, Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::json;

pub use hub::{Hub, SessionView};

/// Largest HTTP request head the hub accepts.
const MAX_HEAD: usize = 8 * 1024;
/// Largest JSON body accepted by `POST /api/open_url/<sid>` and JSON cancel on `/api/picker/<sid>`.
pub const MAX_OPEN_URL_BODY: usize = 16 * 1024;
/// Largest file upload accepted by `POST /api/picker/<sid>` (25 MiB hard bound).
pub const MAX_PICKER_UPLOAD: usize = 25 * 1024 * 1024;
/// Total deadline for reading small JSON POST bodies.
const BODY_DEADLINE: Duration = Duration::from_secs(10);
/// Total deadline for streaming a file upload body to disk.
const UPLOAD_DEADLINE: Duration = Duration::from_secs(30);
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

/// Browser connections served at once (VIEW1c L3). A connection beyond the cap
/// is closed immediately instead of getting a thread, so a local peer cannot
/// exhaust the host's threads by opening sockets and never finishing a request.
pub const MAX_CONNECTIONS: usize = 64;
/// One **total** deadline for a request head (VIEW1c L3): a peer sending a byte
/// at a time cannot keep a connection slot past it.
pub const HEAD_DEADLINE: Duration = Duration::from_secs(5);

/// Releases one connection slot when its thread ends.
struct ConnectionSlot(Arc<AtomicUsize>);

impl Drop for ConnectionSlot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Accept loop: one thread per browser connection, at most [`MAX_CONNECTIONS`].
pub fn serve(listener: TcpListener, token: String, hub: Arc<Hub>) {
    serve_capped(listener, token, hub, MAX_CONNECTIONS);
}

/// [`serve`] with an explicit connection cap (the unit tests use a small one).
pub fn serve_capped(listener: TcpListener, token: String, hub: Arc<Hub>, max_connections: usize) {
    let token = Arc::new(token);
    let port = listener.local_addr().map_or(0, |addr| addr.port());
    let live = Arc::new(AtomicUsize::new(0));
    for connection in listener.incoming() {
        let Ok(connection) = connection else { continue };
        if live.fetch_add(1, Ordering::AcqRel) >= max_connections {
            live.fetch_sub(1, Ordering::AcqRel);
            drop(connection);
            continue;
        }
        let slot = ConnectionSlot(Arc::clone(&live));
        let hub = Arc::clone(&hub);
        let token = Arc::clone(&token);
        let spawned = std::thread::Builder::new()
            .name("acos-view-conn".to_string())
            .spawn(move || {
                let _slot = slot;
                handle(connection, &token, &hub, port);
            });
        // A failed spawn drops the closure (and with it the slot).
        let _ = spawned;
    }
}

/// `true` when `host` is exactly `127.0.0.1:<port>` (VIEW1c L2).
///
/// A browser sends the name it resolved: a DNS-rebinding page loads from
/// `evil.example:<port>`, so a Host header that is not the loopback literal and
/// this hub's own port is refused before any route (the token is also required,
/// but a rebound page could be handed the URL by a confused user or leak it).
#[must_use]
pub fn host_allowed(host: Option<&str>, port: u16) -> bool {
    host.is_some_and(|host| host.trim() == format!("127.0.0.1:{port}"))
}

fn handle(mut connection: TcpStream, token: &str, hub: &Arc<Hub>, port: u16) {
    let _ = connection.set_read_timeout(Some(HEAD_DEADLINE));
    let _ = connection.set_write_timeout(Some(WRITE_TIMEOUT));
    let Some(request) = read_request(&mut connection) else {
        return;
    };
    // A request the page itself never makes: a streamed response needs a stable
    // read timeout, so restore the generous one once the head is in.
    let _ = connection.set_read_timeout(Some(Duration::from_secs(15)));
    if !host_allowed(request.host.as_deref(), port) {
        let _ = respond(
            &mut connection,
            403,
            "text/plain; charset=utf-8",
            b"403 forbidden: Host must be 127.0.0.1:<port>\n",
        );
        return;
    }
    let is_post = request.method == "POST";
    let is_get = request.method == "GET";
    if !is_get && !is_post {
        let _ = respond(
            &mut connection,
            405,
            "text/plain; charset=utf-8",
            b"405 method not allowed\n",
        );
        return;
    }
    let path = request.path.as_str();
    let is_post_route =
        path.starts_with("/api/open_url/") || path.starts_with("/api/picker/");
    if (is_post && !is_post_route) || (is_get && is_post_route) {
        let _ = respond(
            &mut connection,
            405,
            "text/plain; charset=utf-8",
            b"405 method not allowed\n",
        );
        return;
    }
    // Every route requires the token, including the page, streams, and POST APIs.
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

    if is_post {
        if let Some(session_id) = path.strip_prefix("/api/open_url/") {
            handle_open_url(connection, &request, hub, decode_component(session_id));
        } else if let Some(session_id) = path.strip_prefix("/api/picker/") {
            handle_picker(connection, &request, hub, decode_component(session_id));
        }
        return;
    }

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
        stream_frames(connection, &request, hub, decode_component(session_id));
    } else if let Some(session_id) = path.strip_prefix("/api/events/") {
        stream_events(connection, hub, decode_component(session_id));
    } else if let Some(session_id) = path.strip_prefix("/api/input/") {
        handle_input_ws(connection, &request, hub, decode_component(session_id));
    } else {
        let _ = respond(
            &mut connection,
            404,
            "text/plain; charset=utf-8",
            b"404 not found\n",
        );
    }
}

fn respond_json(connection: &mut TcpStream, status: u16, body: &serde_json::Value) {
    let text = body.to_string();
    let _ = respond(
        connection,
        status,
        "application/json; charset=utf-8",
        text.as_bytes(),
    );
}

fn respond_ctl_error(connection: &mut TcpStream, error: hub::ServiceCtlError) {
    match error {
        hub::ServiceCtlError::NotFound => respond_json(
            connection,
            404,
            &json!({
                "ok": false,
                "code": "SESSION_NOT_FOUND",
                "error": "no live session for that session_id",
            }),
        ),
        hub::ServiceCtlError::Replaced => respond_json(
            connection,
            409,
            &json!({
                "ok": false,
                "code": "SESSION_REPLACED",
                "error": "session was replaced by a newer guest; reopen detail view",
            }),
        ),
        hub::ServiceCtlError::WatchOnly => respond_json(
            connection,
            403,
            &json!({
                "ok": false,
                "code": "WATCH_ONLY",
                "error": "session is watch-only and refuses interactive control requests",
            }),
        ),
        hub::ServiceCtlError::Unavailable { message: msg, .. } => respond_json(
            connection,
            503,
            &json!({
                "ok": false,
                "code": "UNAVAILABLE",
                "error": msg,
            }),
        ),
        hub::ServiceCtlError::Daemon { code, message, .. } => {
            if message.contains("START_NOT_FOUND") {
                respond_json(
                    connection,
                    404,
                    &json!({
                        "ok": false,
                        "code": "START_NOT_FOUND",
                        "error": message,
                    }),
                );
            } else if matches!(code.as_str(), "INVALID_ARGUMENT" | "BAD_REQUEST") {
                respond_json(
                    connection,
                    400,
                    &json!({
                        "ok": false,
                        "code": code,
                        "error": message,
                    }),
                );
            } else {
                respond_json(
                    connection,
                    500,
                    &json!({
                        "ok": false,
                        "code": code,
                        "error": message,
                    }),
                );
            }
        }
    }
}

fn parse_optional_guest_gen(request: &Request) -> Result<Option<u64>, &'static str> {
    match request.param("g") {
        None => Ok(None),
        Some(raw) => raw
            .trim()
            .parse::<u64>()
            .ok()
            .filter(|&g| g > 0)
            .map(Some)
            .ok_or("invalid guest generation parameter g"),
    }
}

fn validate_post_length(
    connection: &mut TcpStream,
    request: &Request,
    max_bytes: usize,
) -> Option<usize> {
    if request.transfer_encoding.is_some() {
        respond_json(
            connection,
            411,
            &json!({
                "ok": false,
                "code": "LENGTH_REQUIRED",
                "error": "Transfer-Encoding is not supported; send a fixed Content-Length",
            }),
        );
        return None;
    }
    let Some(raw_len) = request.content_length.as_deref() else {
        respond_json(
            connection,
            411,
            &json!({
                "ok": false,
                "code": "LENGTH_REQUIRED",
                "error": "Content-Length header is required",
            }),
        );
        return None;
    };
    let Ok(len) = raw_len.trim().parse::<usize>() else {
        respond_json(
            connection,
            400,
            &json!({
                "ok": false,
                "code": "INVALID_ARGUMENT",
                "error": "Content-Length must be a non-negative integer",
            }),
        );
        return None;
    };
    if len > max_bytes {
        respond_json(
            connection,
            413,
            &json!({
                "ok": false,
                "code": "PAYLOAD_TOO_LARGE",
                "max_bytes": max_bytes,
                "error": format!("request body of {len} bytes exceeds limit of {max_bytes} bytes"),
            }),
        );
        return None;
    }
    Some(len)
}

fn read_bounded_body(
    connection: &mut TcpStream,
    prefix: &[u8],
    content_length: usize,
    deadline_dur: Duration,
) -> Result<Vec<u8>, String> {
    let mut out = Vec::with_capacity(content_length);
    let take_prefix = prefix.len().min(content_length);
    out.extend_from_slice(&prefix[..take_prefix]);
    let deadline = Instant::now() + deadline_dur;
    let mut buf = [0u8; 4096];
    while out.len() < content_length {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err("timed out reading request body".to_string());
        }
        let _ = connection.set_read_timeout(Some(remaining));
        let want = (content_length - out.len()).min(buf.len());
        match connection.read(&mut buf[..want]) {
            Ok(0) => return Err("unexpected EOF while reading request body".to_string()),
            Ok(n) => out.extend_from_slice(&buf[..n]),
            Err(e) if e.kind() == ErrorKind::Interrupted => continue,
            Err(e) => return Err(format!("failed reading request body: {e}")),
        }
    }
    Ok(out)
}

fn sanitize_upload_filename(raw: Option<&str>) -> String {
    let candidate = raw
        .and_then(|s| s.rsplit(['/', '\\']).next())
        .unwrap_or("")
        .trim();
    let cleaned: String = candidate
        .chars()
        .filter(|c| !c.is_control() && *c != '/' && *c != '\\' && *c != '\0')
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | ' ') {
                c
            } else {
                '_'
            }
        })
        .take(120)
        .collect();
    let trimmed = cleaned.trim().trim_matches('.');
    if trimmed.is_empty() || trimmed == "." || trimmed == ".." {
        "picked-file.bin".to_string()
    } else {
        trimmed.to_string()
    }
}

fn stream_upload_to_file(
    connection: &mut TcpStream,
    prefix: &[u8],
    content_length: usize,
    upload_dir: &std::path::Path,
    display_name: &str,
) -> Result<PathBuf, String> {
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
    static UPLOAD_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

    let mut dir_builder = std::fs::DirBuilder::new();
    dir_builder.recursive(true).mode(0o700);
    dir_builder
        .create(upload_dir)
        .map_err(|e| format!("cannot create picker upload dir: {e}"))?;

    let seq = UPLOAD_SEQ.fetch_add(1, Ordering::Relaxed);
    let ts = crate::live_registry::now_ms();
    let disk_name = format!(
        "pick-{ts}-{seq}-{}",
        display_name.replace(' ', "_")
    );
    let target = upload_dir.join(disk_name);
    let mut file = std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(&target)
        .map_err(|e| format!("cannot create staged upload file: {e}"))?;

    let write_res = (|| -> Result<(), String> {
        let mut written = 0usize;
        let take_prefix = prefix.len().min(content_length);
        if take_prefix > 0 {
            file.write_all(&prefix[..take_prefix])
                .map_err(|e| format!("write staged file: {e}"))?;
            written += take_prefix;
        }
        let deadline = Instant::now() + UPLOAD_DEADLINE;
        let mut buf = [0u8; 32 * 1024];
        while written < content_length {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err("timed out receiving uploaded file".to_string());
            }
            let _ = connection.set_read_timeout(Some(remaining));
            let want = (content_length - written).min(buf.len());
            match connection.read(&mut buf[..want]) {
                Ok(0) => return Err("unexpected EOF while receiving uploaded file".to_string()),
                Ok(n) => {
                    file.write_all(&buf[..n])
                        .map_err(|e| format!("write staged file: {e}"))?;
                    written += n;
                }
                Err(e) if e.kind() == ErrorKind::Interrupted => continue,
                Err(e) => return Err(format!("read uploaded file: {e}")),
            }
        }
        file.flush().map_err(|e| format!("flush staged file: {e}"))?;
        Ok(())
    })();

    if let Err(err) = write_res {
        drop(file);
        let _ = std::fs::remove_file(&target);
        return Err(err);
    }
    Ok(target)
}

/// `POST /api/open_url/<session_id>?t=<token>[&g=<guest_gen>]`
///
/// Delivers a deep link (`ACTION_VIEW`) to the live session through
/// `acos_service_ctl {module: "intents", op: "open_url"}`. Unmatched links
/// return HTTP 404 with `code: "START_NOT_FOUND"` (never fabricated).
fn handle_open_url(
    mut connection: TcpStream,
    request: &Request,
    hub: &Arc<Hub>,
    session_id: String,
) {
    if session_id.is_empty() {
        respond_json(
            &mut connection,
            404,
            &json!({"ok": false, "code": "SESSION_NOT_FOUND", "error": "missing session_id"}),
        );
        return;
    }
    let expected_guest_gen = match parse_optional_guest_gen(request) {
        Ok(g) => g,
        Err(msg) => {
            respond_json(
                &mut connection,
                400,
                &json!({"ok": false, "code": "INVALID_ARGUMENT", "error": msg}),
            );
            return;
        }
    };
    let Some(content_length) = validate_post_length(&mut connection, request, MAX_OPEN_URL_BODY)
    else {
        return;
    };
    let body = match read_bounded_body(
        &mut connection,
        &request.body_prefix,
        content_length,
        BODY_DEADLINE,
    ) {
        Ok(b) => b,
        Err(msg) => {
            respond_json(
                &mut connection,
                400,
                &json!({"ok": false, "code": "INVALID_ARGUMENT", "error": msg}),
            );
            return;
        }
    };
    let parsed: serde_json::Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => {
            respond_json(
                &mut connection,
                400,
                &json!({"ok": false, "code": "INVALID_ARGUMENT", "error": format!("invalid JSON: {e}")}),
            );
            return;
        }
    };
    let Some(url) = parsed
        .get("url")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|u| !u.is_empty())
    else {
        respond_json(
            &mut connection,
            400,
            &json!({"ok": false, "code": "INVALID_ARGUMENT", "error": "JSON body requires a non-empty string `url`"}),
        );
        return;
    };
    let mut args = json!({ "url": url });
    if let Some(pkg) = parsed.get("package").and_then(serde_json::Value::as_str) {
        args["package"] = json!(pkg);
    }
    if let Some(mime) = parsed.get("mime_type").and_then(serde_json::Value::as_str) {
        args["mime_type"] = json!(mime);
    }
    if let Some(flags) = parsed.get("flags").and_then(serde_json::Value::as_i64) {
        args["flags"] = json!(flags);
    }
    match hub.service_ctl(&session_id, expected_guest_gen, "intents", "open_url", args) {
        Ok(mut result) => {
            if let Some(obj) = result.as_object_mut() {
                obj.entry("ok").or_insert_with(|| json!(true));
            }
            respond_json(&mut connection, 200, &result);
        }
        Err(err) => respond_ctl_error(&mut connection, err),
    }
}

/// `POST /api/picker/<session_id>?t=<token>[&g=<guest_gen>][&name=...&seq=...&mime=...]`
///
/// Accepts either:
/// - `Content-Type: application/json` with `{"cancel": true, "capture_seq"?: N}`
///   and no file name (`name` / `X-Filename`) to deliver `RESULT_CANCELED`; or
/// - a raw file body (bounded at [`MAX_PICKER_UPLOAD`], 25 MiB) streamed straight to
///   `<artifacts_dir>/picker/` and handed to `intents.pick_file`.
fn handle_picker(
    mut connection: TcpStream,
    request: &Request,
    hub: &Arc<Hub>,
    session_id: String,
) {
    if session_id.is_empty() {
        respond_json(
            &mut connection,
            404,
            &json!({"ok": false, "code": "SESSION_NOT_FOUND", "error": "missing session_id"}),
        );
        return;
    }
    let expected_guest_gen = match parse_optional_guest_gen(request) {
        Ok(g) => g,
        Err(msg) => {
            respond_json(
                &mut connection,
                400,
                &json!({"ok": false, "code": "INVALID_ARGUMENT", "error": msg}),
            );
            return;
        }
    };
    // A present but malformed selector must never fall back to "the newest
    // capture", so it is refused before any file or daemon call.
    let query_seq = match request.param("seq").map(|s| s.trim().parse::<u64>()) {
        None => None,
        Some(Ok(seq)) => Some(seq),
        Some(Err(_)) => {
            respond_json(
                &mut connection,
                400,
                &json!({"ok": false, "code": "INVALID_ARGUMENT", "error": "query `seq` must be an unsigned integer"}),
            );
            return;
        }
    };
    let raw_name = request
        .form_param("name")
        .or_else(|| request.x_filename.clone());
    // A named body is an upload even when the file itself is JSON.
    let is_json_cancel = raw_name.is_none()
        && request
            .content_type
            .as_deref()
            .is_some_and(|ct| ct.to_ascii_lowercase().starts_with("application/json"));

    if is_json_cancel {
        let Some(content_length) =
            validate_post_length(&mut connection, request, MAX_OPEN_URL_BODY)
        else {
            return;
        };
        let body = match read_bounded_body(
            &mut connection,
            &request.body_prefix,
            content_length,
            BODY_DEADLINE,
        ) {
            Ok(b) => b,
            Err(msg) => {
                respond_json(
                    &mut connection,
                    400,
                    &json!({"ok": false, "code": "INVALID_ARGUMENT", "error": msg}),
                );
                return;
            }
        };
        let parsed: serde_json::Value = match serde_json::from_slice(&body) {
            Ok(v) => v,
            Err(e) => {
                respond_json(
                    &mut connection,
                    400,
                    &json!({"ok": false, "code": "INVALID_ARGUMENT", "error": format!("invalid JSON: {e}")}),
                );
                return;
            }
        };
        if parsed.get("cancel").and_then(serde_json::Value::as_bool) != Some(true) {
            respond_json(
                &mut connection,
                400,
                &json!({"ok": false, "code": "INVALID_ARGUMENT", "error": "JSON picker body must specify {\"cancel\": true}"}),
            );
            return;
        }
        let body_seq = match parsed.get("capture_seq").map(serde_json::Value::as_u64) {
            None => None,
            Some(Some(seq)) => Some(seq),
            Some(None) => {
                respond_json(
                    &mut connection,
                    400,
                    &json!({"ok": false, "code": "INVALID_ARGUMENT", "error": "`capture_seq` must be an unsigned integer"}),
                );
                return;
            }
        };
        let mut args = json!({ "cancel": true });
        if let Some(seq) = body_seq.or(query_seq) {
            args["capture_seq"] = json!(seq);
        }
        match hub.service_ctl(
            &session_id,
            expected_guest_gen,
            "intents",
            "pick_file",
            args,
        ) {
            Ok(mut result) => {
                if let Some(obj) = result.as_object_mut() {
                    obj.entry("ok").or_insert_with(|| json!(true));
                }
                respond_json(&mut connection, 200, &result);
            }
            Err(err) => respond_ctl_error(&mut connection, err),
        }
        return;
    }

    // Raw file upload path: enforce size limit and session authorization BEFORE
    // reading any body or writing anything to disk.
    let Some(content_length) = validate_post_length(&mut connection, request, MAX_PICKER_UPLOAD)
    else {
        return;
    };
    let upload_dir = match hub.picker_upload_dir(&session_id, expected_guest_gen) {
        Ok(dir) => dir,
        Err(err) => {
            respond_ctl_error(&mut connection, err);
            return;
        }
    };
    let display_name = sanitize_upload_filename(raw_name.as_deref());
    let staged_path = match stream_upload_to_file(
        &mut connection,
        &request.body_prefix,
        content_length,
        &upload_dir,
        &display_name,
    ) {
        Ok(path) => path,
        Err(msg) => {
            respond_json(
                &mut connection,
                400,
                &json!({"ok": false, "code": "UPLOAD_FAILED", "error": msg}),
            );
            return;
        }
    };

    let mime_type = request
        .param("mime")
        .filter(|m| !m.trim().is_empty())
        .or_else(|| {
            request
                .content_type
                .as_deref()
                .map(|ct| ct.split(';').next().unwrap_or("").trim().to_string())
                .filter(|m| !m.is_empty() && m != "application/octet-stream")
        });
    // `staged` lets the session delete this copy once nothing serves it.
    let mut args = json!({
        "path": staged_path.display().to_string(),
        "display_name": display_name,
        "staged": true,
    });
    if let Some(mime) = mime_type {
        args["mime_type"] = json!(mime);
    }
    if let Some(seq) = query_seq {
        args["capture_seq"] = json!(seq);
    }

    match hub.service_ctl(
        &session_id,
        expected_guest_gen,
        "intents",
        "pick_file",
        args,
    ) {
        Ok(mut result) => {
            if let Some(obj) = result.as_object_mut() {
                obj.entry("ok").or_insert_with(|| json!(true));
            }
            respond_json(&mut connection, 200, &result);
        }
        Err(err) => {
            // After a lost or timed-out reply the session may already serve
            // this file under a picker URI, so only a refusal removes it.
            if err.is_definite_refusal() {
                let _ = std::fs::remove_file(&staged_path);
            }
            respond_ctl_error(&mut connection, err);
        }
    }
}

/// WebSocket endpoint `/api/input/<session_id>?t=<token>&g=<guest_gen>` (HUMANVIEW1).
///
/// Enforces WebSocket upgrade headers (including `Sec-WebSocket-Version: 13`),
/// verifies the session and non-secret `guest_gen` binding, upgrades the TCP
/// stream to RFC 6455, and forwards JSON input messages to the session's
/// ordered input worker until close/EOF (which cancels active pointers/keys).
fn handle_input_ws(
    mut connection: TcpStream,
    request: &Request,
    hub: &Arc<Hub>,
    session_id: String,
) {
    let upgrade_ok = request
        .upgrade
        .as_deref()
        .is_some_and(|v| v.trim().eq_ignore_ascii_case("websocket"));
    let connection_ok = request.connection.as_deref().is_some_and(|v| {
        v.split(',')
            .any(|part| part.trim().eq_ignore_ascii_case("upgrade"))
    });
    let ws_key = request
        .sec_websocket_key
        .as_deref()
        .map(str::trim)
        .filter(|k| !k.is_empty());
    let (true, true, Some(ws_key)) = (upgrade_ok, connection_ok, ws_key) else {
        let _ = respond(
            &mut connection,
            400,
            "text/plain; charset=utf-8",
            b"400 bad request: expected WebSocket upgrade\n",
        );
        return;
    };

    // RFC 6455 §4.2.1 / §4.4: validate Sec-WebSocket-Version before upgrading
    // or allocating an input client, advertising supported version 13 on 426.
    let ws_version_ok = request
        .sec_websocket_version
        .as_deref()
        .is_some_and(|v| v.trim() == "13");
    if !ws_version_ok {
        let _ = respond_ws_version_required(&mut connection);
        return;
    }

    if session_id.is_empty() {
        let _ = respond(
            &mut connection,
            404,
            "text/plain; charset=utf-8",
            b"404 missing session_id\n",
        );
        return;
    }

    let Some(expected_guest_gen) = request
        .param("g")
        .and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|&g| g > 0)
    else {
        let _ = respond(
            &mut connection,
            400,
            "text/plain; charset=utf-8",
            b"400 bad request: missing or invalid guest generation parameter g\n",
        );
        return;
    };

    let client = match hub.open_input_checked(&session_id, expected_guest_gen) {
        Ok(client) => client,
        Err(hub::OpenInputError::NotFound) => {
            let _ = respond(
                &mut connection,
                404,
                "text/plain; charset=utf-8",
                b"404 no live session for that session_id\n",
            );
            return;
        }
        Err(hub::OpenInputError::Replaced) => {
            let body = json!({
                "ok": false,
                "code": "SESSION_REPLACED",
                "error": "session was replaced by a newer guest; reopen detail view",
            })
            .to_string();
            let _ = respond(
                &mut connection,
                409,
                "application/json; charset=utf-8",
                body.as_bytes(),
            );
            return;
        }
    };

    if ws::write_handshake_response(&mut connection, ws_key).is_err() {
        return;
    }
    let _ = connection.set_write_timeout(Some(WRITE_TIMEOUT));
    let Ok(writer_stream) = connection.try_clone() else {
        return;
    };
    let writer = Arc::new(Mutex::new(writer_stream));

    loop {
        match ws::read_message_with_writer(&mut connection, &writer) {
            Ok(Some(ws::WsMessage::Text(text))) => {
                if !client.enqueue(&text, Arc::clone(&writer)) {
                    break;
                }
            }
            Ok(Some(ws::WsMessage::Binary(_))) => {
                let err = json!({
                    "ok": false,
                    "code": "INVALID_ARGUMENT",
                    "error": "binary WebSocket frames are not supported",
                })
                .to_string();
                let _ = ws::send_text_locked(&writer, &err);
            }
            Ok(None) | Err(_) => break,
        }
    }
    // Dropping `client` enqueues ClientDisconnected so active pointers cancel.
}

/// MJPEG: the newest frame, repeatedly, on one multipart response.
fn stream_frames(
    mut connection: TcpStream,
    request: &Request,
    hub: &Arc<Hub>,
    session_id: String,
) {
    let expected_guest_gen = match request.param("g") {
        None => None,
        Some(raw) => match raw.trim().parse::<u64>().ok().filter(|&g| g > 0) {
            Some(g) => Some(g),
            None => {
                let _ = respond(
                    &mut connection,
                    400,
                    "text/plain; charset=utf-8",
                    b"400 bad request: invalid guest generation parameter g\n",
                );
                return;
            }
        },
    };
    let watcher = match hub.watch_checked(&session_id, expected_guest_gen) {
        Ok(watcher) => watcher,
        Err(hub::WatchError::NotFound) => {
            let _ = respond(
                &mut connection,
                404,
                "text/plain; charset=utf-8",
                b"404 no live stream for that session\n",
            );
            return;
        }
        Err(hub::WatchError::Replaced) => {
            let body = json!({
                "ok": false,
                "code": "SESSION_REPLACED",
                "error": "session was replaced by a newer guest; reopen detail view",
            })
            .to_string();
            let _ = respond(
                &mut connection,
                409,
                "application/json; charset=utf-8",
                body.as_bytes(),
            );
            return;
        }
    };
    let _ = connection.set_write_timeout(Some(WRITE_TIMEOUT));
    if write!(
        connection,
        "HTTP/1.1 200 OK\r\n\
         Content-Type: multipart/x-mixed-replace; boundary=acosframe\r\n\
         Cache-Control: no-store\r\n\
         X-Content-Type-Options: nosniff\r\n\
         X-Guest-Gen: {}\r\n\
         Connection: close\r\n\r\n",
        watcher.guest_gen()
    )
    .is_err()
    {
        return;
    }
    let _ = connection.flush();

    let mut cursor = 0u64;
    let mut first = true;
    loop {
        if watcher.is_replaced() {
            break;
        }
        match watcher.wait_frame(cursor, EOF_POLL) {
            Some(frame) => {
                cursor = frame.index;
                let content_type = match frame.format.as_str() {
                    "jpeg" | "jpg" => "image/jpeg",
                    _ => "image/png",
                };
                let prefix = if first {
                    first = false;
                    "--acosframe\r\n"
                } else {
                    ""
                };
                let part = format!(
                    "{prefix}Content-Type: {content_type}\r\nContent-Length: {}\r\n\
                     X-Timestamp: {}\r\nX-Frame-Seq: {}\r\nX-Frame-Size: {}x{}\r\n\r\n",
                    frame.bytes.len(),
                    frame.ts_ms,
                    frame.seq,
                    frame.w,
                    frame.h,
                );
                if connection.write_all(part.as_bytes()).is_err()
                    || connection.write_all(&frame.bytes).is_err()
                    || connection.write_all(b"\r\n--acosframe\r\n").is_err()
                    || connection.flush().is_err()
                {
                    break; // slow or closed browser: never block the producer
                }
            }
            None => {
                if watcher.is_replaced() || peer_closed(&connection) {
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
        "guest_gen": watcher.guest_gen(),
        "hello": watcher.hello(),
        "connected": watcher.stream_state().0,
    });
    if send_event(&mut connection, "state", &state.to_string()).is_err() {
        return;
    }

    let mut cursor = watcher.event_cursor();
    loop {
        if watcher.is_replaced() {
            let replaced = json!({ "code": "SESSION_REPLACED", "guest_gen": watcher.guest_gen() });
            let _ = send_event(&mut connection, "replaced", &replaced.to_string());
            break;
        }
        let batch = watcher.wait_events(cursor, Duration::from_secs(5));
        if batch.missed > 0 {
            let missed = json!({"missed": batch.missed});
            if send_event(&mut connection, "lag", &missed.to_string()).is_err() {
                break;
            }
        }
        if batch.events.is_empty() {
            if watcher.is_replaced() {
                let replaced =
                    json!({ "code": "SESSION_REPLACED", "guest_gen": watcher.guest_gen() });
                let _ = send_event(&mut connection, "replaced", &replaced.to_string());
                break;
            }
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

fn respond_ws_version_required(connection: &mut TcpStream) -> io::Result<()> {
    let body = b"426 upgrade required: Sec-WebSocket-Version 13 required\n";
    write!(
        connection,
        "HTTP/1.1 426 Upgrade Required\r\n\
         Sec-WebSocket-Version: 13\r\n\
         Content-Type: text/plain; charset=utf-8\r\n\
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

fn respond(
    connection: &mut TcpStream,
    status: u16,
    content_type: &str,
    body: &[u8],
) -> io::Result<()> {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        409 => "Conflict",
        411 => "Length Required",
        413 => "Payload Too Large",
        500 => "Internal Server Error",
        503 => "Service Unavailable",
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
    host: Option<String>,
    upgrade: Option<String>,
    connection: Option<String>,
    sec_websocket_key: Option<String>,
    sec_websocket_version: Option<String>,
    content_length: Option<String>,
    content_type: Option<String>,
    transfer_encoding: Option<String>,
    x_filename: Option<String>,
    body_prefix: Vec<u8>,
}

impl Request {
    fn param(&self, name: &str) -> Option<String> {
        self.raw_param(name).map(decode_component)
    }

    /// [`Self::param`] for a form-encoded value (`URLSearchParams`), where a
    /// literal `+` is a space and a plus sign arrives as `%2B`.
    fn form_param(&self, name: &str) -> Option<String> {
        self.raw_param(name)
            .map(|value| decode_component(&value.replace('+', " ")))
    }

    fn raw_param(&self, name: &str) -> Option<&str> {
        self.query
            .split('&')
            .filter_map(|pair| pair.split_once('='))
            .find_map(|(key, value)| (key == name).then_some(value))
    }
}

fn read_request(connection: &mut TcpStream) -> Option<Request> {
    let mut head = Vec::with_capacity(512);
    let mut buffer = [0u8; 1024];
    let deadline = Instant::now() + HEAD_DEADLINE;
    let sep = loop {
        // One total deadline for the whole head, not a per-read timeout.
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() || connection.set_read_timeout(Some(remaining)).is_err() {
            return None;
        }
        if let Some(pos) = head.windows(4).position(|window| window == b"\r\n\r\n") {
            if pos > MAX_HEAD {
                return None;
            }
            break pos;
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
    };
    let body_prefix = head[sep + 4..].to_vec();
    let text = String::from_utf8_lossy(&head[..sep]);
    let line = text.lines().next()?;
    let mut fields = line.split_whitespace();
    let method = fields.next()?.to_string();
    let target = fields.next()?.to_string();
    let (path, query) = match target.split_once('?') {
        Some((path, query)) => (path.to_string(), query.to_string()),
        None => (target, String::new()),
    };
    let mut host = None;
    let mut upgrade = None;
    let mut conn_hdr = None;
    let mut sec_websocket_key = None;
    let mut sec_websocket_version = None;
    let mut content_length: Option<String> = None;
    let mut content_type = None;
    let mut transfer_encoding = None;
    let mut x_filename = None;
    for header_line in text.lines().skip(1).take_while(|line| !line.is_empty()) {
        let Some((name, value)) = header_line.split_once(':') else {
            continue;
        };
        let name = name.trim();
        let value = value.trim().to_string();
        if name.eq_ignore_ascii_case("host") {
            host = Some(value);
        } else if name.eq_ignore_ascii_case("upgrade") {
            upgrade = Some(value);
        } else if name.eq_ignore_ascii_case("connection") {
            conn_hdr = Some(value);
        } else if name.eq_ignore_ascii_case("sec-websocket-key") {
            sec_websocket_key = Some(value);
        } else if name.eq_ignore_ascii_case("sec-websocket-version") {
            sec_websocket_version = Some(value);
        } else if name.eq_ignore_ascii_case("content-length") {
            if let Some(prev) = &content_length {
                if prev != &value {
                    return None;
                }
            }
            content_length = Some(value);
        } else if name.eq_ignore_ascii_case("content-type") {
            content_type = Some(value);
        } else if name.eq_ignore_ascii_case("transfer-encoding") {
            transfer_encoding = Some(value);
        } else if name.eq_ignore_ascii_case("x-filename") {
            x_filename = Some(value);
        }
    }
    Some(Request {
        method,
        path,
        query,
        host,
        upgrade,
        connection: conn_hdr,
        sec_websocket_key,
        sec_websocket_version,
        content_length,
        content_type,
        transfer_encoding,
        x_filename,
        body_prefix,
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
            object.insert("guest_gen".to_string(), json!(self.guest_gen));
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
        // Short paths: a Unix socket path must fit in `sun_path` (< 108 bytes),
        // and the checkout path is not something this test controls (agent
        // worktrees are long). `test_scratch_root` keeps it short.
        crate::live_registry::short_scratch(crate::live_registry::test_scratch_root(), tag)
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
            "GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n"
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
        // Read byte-by-byte so we stop right after `\r\n\r\n` without a
        // BufReader buffering (and discarding on drop) initial SSE/MJPEG body
        // bytes that arrive in the same TCP segment.
        let mut head = Vec::new();
        let mut byte = [0u8; 1];
        while stream.read(&mut byte).unwrap_or(0) == 1 {
            head.push(byte[0]);
            if head.ends_with(b"\r\n\r\n") {
                break;
            }
        }
        String::from_utf8_lossy(&head).into_owned()
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
            "POST /?t={} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nContent-Length: 0\r\n\r\n",
            server.token, server.port
        )
        .expect("write");
        let mut response = Vec::new();
        let _ = stream.read_to_end(&mut response);
        assert_eq!(status_of(&response), 405);
    }

    #[test]
    fn a_host_other_than_the_loopback_literal_and_port_is_refused() {
        let server = Server::start("host");
        let port = server.port;
        let ask = |host: &str| -> u16 {
            let mut stream = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).expect("connect");
            let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
            write!(
                stream,
                "GET /api/sessions?t={} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n",
                server.token
            )
            .expect("write");
            let mut response = Vec::new();
            let _ = stream.read_to_end(&mut response);
            status_of(&response)
        };
        assert_eq!(ask(&format!("127.0.0.1:{port}")), 200);
        // DNS rebinding: a name that resolves to 127.0.0.1, wrong port, no port.
        assert_eq!(ask(&format!("evil.example:{port}")), 403);
        assert_eq!(ask(&format!("localhost:{port}")), 403);
        assert_eq!(ask("127.0.0.1"), 403);
        assert_eq!(ask(&format!("127.0.0.1:{}", port.wrapping_add(1))), 403);
        // No Host header at all.
        let mut stream = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).expect("connect");
        write!(
            stream,
            "GET /api/sessions?t={} HTTP/1.0\r\n\r\n",
            server.token
        )
        .expect("write");
        let mut response = Vec::new();
        let _ = stream.read_to_end(&mut response);
        assert_eq!(status_of(&response), 403);
        assert!(host_allowed(Some("127.0.0.1:9"), 9));
        assert!(!host_allowed(None, 9));
    }

    #[test]
    fn connections_are_capped_and_a_stalled_head_is_cut_off_by_one_deadline() {
        let registry = scratch("cap-registry");
        let token = random_token().expect("token");
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind");
        let port = listener.local_addr().expect("addr").port();
        let hub = Arc::new(Hub::new(registry.clone()));
        let served_token = token.clone();
        std::thread::spawn(move || serve_capped(listener, served_token, hub, 2));
        // Two stalled peers (no request) hold the two slots...
        let mut holders = Vec::new();
        for _ in 0..2 {
            holders.push(TcpStream::connect((Ipv4Addr::LOCALHOST, port)).expect("connect"));
        }
        std::thread::sleep(Duration::from_millis(200));
        // ...so a third is closed without a response (EOF or reset), not served.
        let mut third = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).expect("connect");
        let _ = third.set_read_timeout(Some(Duration::from_secs(3)));
        let _ = write!(
            third,
            "GET /api/sessions?t={token} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\r\n"
        );
        let mut response = Vec::new();
        let _ = third.read_to_end(&mut response);
        assert!(response.is_empty(), "over-cap connection was served");
        // One total deadline: a byte-at-a-time peer is dropped at HEAD_DEADLINE
        // even though every single read is well inside any per-read timeout.
        let started = Instant::now();
        let mut slow = holders.remove(0);
        let _ = slow.set_read_timeout(Some(Duration::from_secs(HEAD_DEADLINE.as_secs() + 3)));
        for _ in 0..40 {
            if slow.write_all(b"G").is_err() {
                break;
            }
            std::thread::sleep(Duration::from_millis(400));
            if started.elapsed() > HEAD_DEADLINE + Duration::from_secs(2) {
                break;
            }
        }
        let mut sink = Vec::new();
        let _ = slow.read_to_end(&mut sink);
        assert!(
            started.elapsed() < HEAD_DEADLINE + Duration::from_secs(4),
            "the stalled head outlived the total deadline: {:?}",
            started.elapsed()
        );
        // The slot was released: a well-formed request is served again.
        std::thread::sleep(Duration::from_millis(200));
        let response = get(port, &format!("/api/sessions?t={token}"));
        assert_eq!(status_of(&response), 200);
        let _ = std::fs::remove_dir_all(&registry);
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
            server
                .registry
                .join(format!("s_live-{}.json", std::process::id())),
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
                        0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 0x0d, b'I', b'H',
                        b'D', b'R', 0, 0, 0, 1, 0, 0, 0, 1, 8, 6, 0, 0, 0, 0x1f, 0x15, 0xc4, 0x89,
                        0, 0, 0, 0x0a, b'I', b'D', b'A', b'T', 0x78, 0x9c, 0x63, 0x00, 0x01, 0x00,
                        0x00, 0x05, 0x00, 0x01, 0x0d, 0x0a, 0x2d, 0xb4, 0, 0, 0, 0, 0, b'I', b'E',
                        b'N', b'D', 0xae, 0x42, 0x60, 0x82,
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
            "GET /api/frames/s_stream?t={} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\n\r\n",
            server.token, server.port
        )
        .expect("write");
        let head = read_response_head(&mut first);
        assert!(head.starts_with("HTTP/1.1 200"), "{head}");
        assert!(head.contains("multipart/x-mixed-replace"), "{head}");
        let mut second = TcpStream::connect((Ipv4Addr::LOCALHOST, server.port)).expect("connect");
        write!(
            second,
            "GET /api/frames/s_stream?t={} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\n\r\n",
            server.token, server.port
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
            "GET /api/events/s_stream?t={} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\n\r\n",
            server.token, server.port
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

    #[test]
    fn api_input_enforces_token_host_upgrade_and_forwards_ws_messages() {
        let server = Server::start("ws-in");
        let daemon_sock = server.registry.join("daemon.sock");
        let daemon_listener = UnixListener::bind(&daemon_sock).expect("bind daemon");
        let calls = Arc::new(Mutex::new(Vec::<(String, serde_json::Value)>::new()));
        let calls_t = Arc::clone(&calls);
        std::thread::spawn(move || {
            for stream in daemon_listener.incoming() {
                let Ok(mut writer) = stream else { break };
                let reader = std::io::BufReader::new(writer.try_clone().expect("clone"));
                let calls = Arc::clone(&calls_t);
                std::thread::spawn(move || {
                    for line in reader.lines() {
                        let Ok(line) = line else { break };
                        if line.trim().is_empty() {
                            continue;
                        }
                        let req: serde_json::Value = serde_json::from_str(&line).unwrap();
                        calls.lock().unwrap().push((
                            req["tool"].as_str().unwrap_or("").to_string(),
                            req["args"].clone(),
                        ));
                        let resp = json!({
                            "id": req["id"],
                            "envelope": { "ok": true, "result": { "ok": true } },
                        });
                        let _ = writer.write_all((resp.to_string() + "\n").as_bytes());
                        let _ = writer.flush();
                    }
                });
            }
        });

        let live = json!({
            "v": 1,
            "session_id": "s_ws",
            "daemon_pid": std::process::id(),
            "started_ms": crate::live_registry::now_ms(),
            "client": {"name": "ws-test", "version": "1"},
            "project_dir": "/tmp",
            "apk": "x.apk",
            "package": "x",
            "serve_socket": daemon_sock.display().to_string(),
            "display": {"w": 1080, "h": 2400},
            "stream_socket": null,
        });
        std::fs::write(
            server
                .registry
                .join(format!("s_ws-{}.json", std::process::id())),
            live.to_string(),
        )
        .unwrap();

        // 1. Missing/wrong token on /api/input -> 403.
        assert_eq!(status_of(&server.raw("/api/input/s_ws")), 403);
        assert_eq!(
            status_of(&server.raw(&format!("/api/input/s_ws?t={}bad", server.token))),
            403
        );
        // 2. Wrong Host on /api/input -> 403.
        let mut bad_host = TcpStream::connect((Ipv4Addr::LOCALHOST, server.port)).unwrap();
        write!(
            bad_host,
            "GET /api/input/s_ws?t={} HTTP/1.1\r\nHost: evil.example:{}\r\n\
             Upgrade: websocket\r\nConnection: Upgrade\r\n\
             Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\r\n",
            server.token, server.port
        )
        .unwrap();
        let mut resp = Vec::new();
        let _ = bad_host.read_to_end(&mut resp);
        assert_eq!(status_of(&resp), 403);

        // 3. Valid token + Host, but non-WS GET -> 400.
        assert_eq!(
            status_of(&server.raw(&format!("/api/input/s_ws?t={}", server.token))),
            400
        );
        // 3b. Unsupported Sec-WebSocket-Version (12) -> 426 advertising 13.
        let mut bad_ver = TcpStream::connect((Ipv4Addr::LOCALHOST, server.port)).unwrap();
        write!(
            bad_ver,
            "GET /api/input/s_ws?t={}&g=1 HTTP/1.1\r\nHost: 127.0.0.1:{}\r\n\
             Upgrade: websocket\r\nConnection: Upgrade\r\n\
             Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\
             Sec-WebSocket-Version: 12\r\n\r\n",
            server.token, server.port
        )
        .unwrap();
        let mut resp426 = Vec::new();
        let _ = bad_ver.read_to_end(&mut resp426);
        assert_eq!(status_of(&resp426), 426);
        let text426 = String::from_utf8_lossy(&resp426);
        assert!(
            text426.contains("Sec-WebSocket-Version: 13"),
            "426 response must advertise supported version 13: {text426}"
        );

        // 3c. Missing guest generation `g` -> 400.
        let mut missing_g = TcpStream::connect((Ipv4Addr::LOCALHOST, server.port)).unwrap();
        write!(
            missing_g,
            "GET /api/input/s_ws?t={} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\n\
             Upgrade: websocket\r\nConnection: Upgrade\r\n\
             Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\
             Sec-WebSocket-Version: 13\r\n\r\n",
            server.token, server.port
        )
        .unwrap();
        let mut resp400g = Vec::new();
        let _ = missing_g.read_to_end(&mut resp400g);
        assert_eq!(status_of(&resp400g), 400);

        // 4. Valid WS upgrade for missing session -> 404.
        let mut missing = TcpStream::connect((Ipv4Addr::LOCALHOST, server.port)).unwrap();
        write!(
            missing,
            "GET /api/input/nope?t={}&g=1 HTTP/1.1\r\nHost: 127.0.0.1:{}\r\n\
             Upgrade: websocket\r\nConnection: Upgrade\r\n\
             Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\
             Sec-WebSocket-Version: 13\r\n\r\n",
            server.token, server.port
        )
        .unwrap();
        let mut resp404 = Vec::new();
        let _ = missing.read_to_end(&mut resp404);
        assert_eq!(status_of(&resp404), 404);

        // 4b. Mismatched guest generation -> 409 SESSION_REPLACED.
        let guest_gen = server.hub.list()[0].guest_gen;
        let mut stale_g = TcpStream::connect((Ipv4Addr::LOCALHOST, server.port)).unwrap();
        write!(
            stale_g,
            "GET /api/input/s_ws?t={}&g={} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\n\
             Upgrade: websocket\r\nConnection: Upgrade\r\n\
             Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\
             Sec-WebSocket-Version: 13\r\n\r\n",
            server.token,
            guest_gen + 99,
            server.port
        )
        .unwrap();
        let mut resp409 = Vec::new();
        let _ = stale_g.read_to_end(&mut resp409);
        assert_eq!(status_of(&resp409), 409);
        assert!(
            String::from_utf8_lossy(&resp409).contains("SESSION_REPLACED"),
            "409 must report SESSION_REPLACED"
        );

        // 5. Real WS upgrade + input messages + mid-gesture drop -> ACTION_CANCEL.
        let mut ws_cli = TcpStream::connect((Ipv4Addr::LOCALHOST, server.port)).unwrap();
        let _ = ws_cli.set_read_timeout(Some(Duration::from_secs(5)));
        write!(
            ws_cli,
            "GET /api/input/s_ws?t={}&g={} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\n\
             Upgrade: websocket\r\nConnection: Upgrade\r\n\
             Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\
             Sec-WebSocket-Version: 13\r\n\r\n",
            server.token, guest_gen, server.port
        )
        .unwrap();
        let head = read_response_head(&mut ws_cli);
        assert!(head.starts_with("HTTP/1.1 101"), "{head}");
        assert!(head.contains("s3pPLMBiTxaQ9kYGzzhZRbK+xOo="), "{head}");

        let send_masked = |stream: &mut TcpStream, msg: &str| {
            let payload = msg.as_bytes();
            let mut frame = Vec::new();
            frame.push(0x81);
            assert!(payload.len() <= 125);
            frame.push(0x80 | (payload.len() as u8));
            let mask = [0x12u8, 0x34, 0x56, 0x78];
            frame.extend_from_slice(&mask);
            for (i, b) in payload.iter().enumerate() {
                frame.push(*b ^ mask[i & 3]);
            }
            stream.write_all(&frame).unwrap();
            stream.flush().unwrap();
        };
        let read_reply = |stream: &mut TcpStream| -> serde_json::Value {
            let mut h = [0u8; 2];
            stream.read_exact(&mut h).unwrap();
            let len = usize::from(h[1] & 0x7f);
            let mut buf = vec![0u8; len];
            stream.read_exact(&mut buf).unwrap();
            serde_json::from_slice(&buf).unwrap()
        };

        send_masked(&mut ws_cli, r#"{"t":"touch","phase":"down","id":0,"x":10,"y":20}"#);
        assert_eq!(read_reply(&mut ws_cli)["ok"], json!(true));
        send_masked(&mut ws_cli, r#"{"t":"text","s":"hi"}"#);
        assert_eq!(read_reply(&mut ws_cli)["ok"], json!(true));
        send_masked(&mut ws_cli, r#"{"t":"key","code":"KEYCODE_BACK","phase":"press"}"#);
        assert_eq!(read_reply(&mut ws_cli)["ok"], json!(true));

        // Drop mid-gesture (finger still down) -> worker sends ACTION_CANCEL.
        drop(ws_cli);
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if calls.lock().unwrap().len() >= 4 {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let recorded = calls.lock().unwrap().clone();
        assert_eq!(recorded.len(), 4, "{recorded:#?}");
        assert_eq!(recorded[0].0, "panel_touch");
        assert_eq!(recorded[0].1["action"], json!(0));
        assert_eq!(recorded[1].0, "panel_text");
        assert_eq!(recorded[2].0, "panel_key");
        assert_eq!(recorded[2].1["code"], json!(4));
        assert_eq!(recorded[3].0, "panel_touch");
        assert_eq!(recorded[3].1["action"], json!(3)); // ACTION_CANCEL
    }

    #[test]
    fn stream_frames_flushes_closing_boundary_before_blocking_for_next_frame() {
        use std::sync::mpsc;

        let server = Server::start("mjpeg-bnd");
        let socket_path = server.registry.join("bnd.sock");
        let listener = UnixListener::bind(&socket_path).expect("bind producer");
        let (tx_send_second, rx_send_second) = mpsc::channel::<()>();
        let (tx_stop, rx_stop) = mpsc::channel::<()>();

        let png: &'static [u8] = &[
            0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 0x0d, b'I', b'H', b'D', b'R',
            0, 0, 0, 1, 0, 0, 0, 1, 8, 6, 0, 0, 0, 0x1f, 0x15, 0xc4, 0x89, 0, 0, 0, 0x0a, b'I',
            b'D', b'A', b'T', 0x78, 0x9c, 0x63, 0x00, 0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0d,
            0x0a, 0x2d, 0xb4, 0, 0, 0, 0, 0, b'I', b'E', b'N', b'D', 0xae, 0x42, 0x60, 0x82,
        ];

        std::thread::spawn(move || {
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            let mut reader = std::io::BufReader::new(stream.try_clone().expect("clone"));
            let mut line = String::new();
            let _ = reader.read_line(&mut line);
            let hello = br#"{"v":1,"session":9,"display":{"w":1080,"h":2400},"format":"png","scale":0.5}"#;
            let _ = send(&mut stream, b'H', hello);

            let send_frame = |stream: &mut std::os::unix::net::UnixStream, seq: u64| {
                let header = format!(
                    "{{\"seq\":{seq},\"generation\":1,\"w\":1,\"h\":1,\"format\":\"png\",\"ts_ms\":{}}}\n",
                    crate::live_registry::now_ms()
                );
                let mut payload = header.into_bytes();
                payload.extend_from_slice(png);
                send(stream, b'F', &payload)
            };

            // Emit frame 1 and then block until released.
            let _ = send_frame(&mut stream, 1);
            if rx_send_second.recv_timeout(Duration::from_secs(5)).is_ok() {
                let _ = send_frame(&mut stream, 2);
                let _ = rx_stop.recv_timeout(Duration::from_secs(5));
            }
        });

        let live = json!({
            "v": 1,
            "session_id": "s_bnd",
            "daemon_pid": std::process::id(),
            "started_ms": crate::live_registry::now_ms(),
            "client": {"name": "bnd-test", "version": "1"},
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
                .join(format!("s_bnd-{}.json", std::process::id())),
            live.to_string(),
        )
        .expect("write live");

        let mut conn = TcpStream::connect((Ipv4Addr::LOCALHOST, server.port)).expect("connect");
        conn.set_read_timeout(Some(Duration::from_millis(200)))
            .unwrap();
        write!(
            conn,
            "GET /api/frames/s_bnd?t={} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\n\r\n",
            server.token, server.port
        )
        .expect("write");

        let head = read_response_head(&mut conn);
        assert!(head.starts_with("HTTP/1.1 200"), "{head}");

        let mut body = Vec::new();
        let mut buf = [0u8; 4096];
        let mut expected_part1_end = Vec::from(png);
        expected_part1_end.extend_from_slice(b"\r\n--acosframe\r\n");

        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            match conn.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    body.extend_from_slice(&buf[..n]);
                    if body.windows(expected_part1_end.len()).any(|w| w == expected_part1_end) {
                        break;
                    }
                }
                Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
                Err(e) => panic!("read error: {e}"),
            }
        }
        assert!(
            body.starts_with(b"--acosframe\r\n"),
            "first part must begin with opening boundary: {:?}",
            String::from_utf8_lossy(&body[..body.len().min(64)])
        );
        assert!(
            body.windows(expected_part1_end.len())
                .any(|w| w == expected_part1_end),
            "frame 1 must be immediately followed by closing boundary \\r\\n--acosframe\\r\\n before stream blocks"
        );
        let first_consumed = body.len();

        // Release frame 2 and verify its headers start cleanly after the already-flushed boundary
        // and its bytes also end with \r\n--acosframe\r\n before blocking again.
        let _ = tx_send_second.send(());
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            match conn.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    body.extend_from_slice(&buf[..n]);
                    let tail = &body[first_consumed..];
                    if tail.windows(expected_part1_end.len()).any(|w| w == expected_part1_end) {
                        break;
                    }
                }
                Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
                Err(e) => panic!("read error: {e}"),
            }
        }
        let tail = &body[first_consumed..];
        assert!(
            tail.starts_with(b"Content-Type: image/png\r\n"),
            "frame 2 must start with headers (no duplicate boundary), got: {:?}",
            String::from_utf8_lossy(&tail[..tail.len().min(64)])
        );
        assert!(
            tail.windows(expected_part1_end.len())
                .any(|w| w == expected_part1_end),
            "frame 2 must also end with closing boundary \\r\\n--acosframe\\r\\n before blocking"
        );
        let _ = tx_stop.send(());
    }

    #[test]
    fn page_contains_interactive_controls_and_verify_banner() {
        let server = Server::start("page-ui");
        let page = server.raw(&format!("/?t={}", server.token));
        assert_eq!(status_of(&page), 200);
        let html = String::from_utf8_lossy(body_of(&page)).to_string();
        for needle in [
            "<canvas id=\"frame\"",
            "createImageBitmap",
            "getReader",
            "btn-back",
            "btn-home",
            "btn-recents",
            "btn-rotate",
            "verify-banner",
            "watch-only: verify run",
            "no launcher in this session",
            "HOME_UNAVAILABLE",
            "sessionTitle",
            "/api/input/",
            "agent &amp; human activity",
            "KEYCODE_BACK",
            "KEYCODE_HOME",
            "KEYCODE_APP_SWITCH",
        ] {
            assert!(html.contains(needle), "page missing {needle}");
        }
        assert!(
            !html.contains("<img id=\"frame\""),
            "must use canvas + fetch stream instead of endless <img>"
        );
        assert!(
            !html.contains("view-only · no input reaches the guest"),
            "stale view-only header pill must be removed"
        );
    }

    #[test]
    fn page_contains_session_ended_stop_and_backoff() {
        let server = Server::start("page-ended");
        let page = server.raw(&format!("/?t={}", server.token));
        assert_eq!(status_of(&page), 200);
        let html = String::from_utf8_lossy(body_of(&page)).to_string();
        for needle in [
            "response.status === 404",
            "markSessionEnded",
            "session ended",
            "SESSION_REPLACED",
            "heldGuestKeys",
            "pageshow",
            "retryDelay",
            "Math.min(retryDelay * 2, 5000)",
        ] {
            assert!(html.contains(needle), "page missing {needle}");
        }
        assert!(
            !html.contains("Tab: \"KEYCODE_TAB\""),
            "page must not capture Tab away from keyboard navigation"
        );
    }

    #[test]
    fn page_contains_phone_controls_pinch_paste_and_gesture_visuals() {
        let server = Server::start("page-phone-ops");
        let page = server.raw(&format!("/?t={}", server.token));
        assert_eq!(status_of(&page), 200);
        let html = String::from_utf8_lossy(body_of(&page)).to_string();
        for needle in [
            "btn-vol-down",
            "btn-vol-up",
            "btn-power",
            "btn-pinch",
            "btn-shot",
            "gesture-svg",
            "KEYCODE_VOLUME_DOWN",
            "KEYCODE_VOLUME_UP",
            "KEYCODE_POWER",
            "KEYCODE_A",
            "Power key",
            "Send KEYCODE_POWER key event",
            "producerGesture",
            "hasDecodedFrame",
            "X-Guest-Gen",
            "mirrorDisplayPoint",
            "addEventListener(\"paste\"",
            "t: \"pinch\"",
            "long-press",
            "touch-dot",
        ] {
            assert!(html.contains(needle), "page missing {needle}");
        }
        assert!(
            !html.contains("lastLocalGestureHint"),
            "page must classify gestures from SSE stream, not client-local hints"
        );
        assert!(
            !html.contains("Power / Wake"),
            "page must not promise display wake/sleep for KEYCODE_POWER"
        );
    }

    #[test]
    fn frames_stream_enforces_guest_gen_binding() {
        let server = Server::start("frames-gen");
        let stream_sock = server.registry.join("fg.sock");
        let listener = UnixListener::bind(&stream_sock).expect("bind fake guest");
        let _guest = std::thread::spawn(move || {
            for incoming in listener.incoming().take(3) {
                let Ok(mut stream) = incoming else { break };
                let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
                let mut reader = std::io::BufReader::new(stream.try_clone().expect("clone"));
                let mut line = String::new();
                let _ = reader.read_line(&mut line);
                let hello = json!({"v": 1, "kind": "hello", "session_id": "s_fg", "display": {"w": 1080, "h": 2400}});
                let _ = send(&mut stream, b'H', hello.to_string().as_bytes());
                let mut frame = Vec::new();
                let header = json!({"ts_ms": 1, "seq": 1, "w": 1080, "h": 2400, "format": "png"});
                let hdr_bytes = header.to_string().into_bytes();
                frame.extend_from_slice(&hdr_bytes);
                frame.push(b'\n');
                frame.extend_from_slice(b"PNGDATA");
                let _ = send(&mut stream, b'F', &frame);
                std::thread::sleep(Duration::from_millis(250));
            }
        });

        let entry_path = server.registry.join("s_fg.json");
        let doc = json!({
            "v": 1,
            "session_id": "s_fg",
            "daemon_pid": std::process::id(),
            "launcher_pid": std::process::id(),
            "started_ms": crate::live_registry::now_ms(),
            "stream_socket": stream_sock.to_string_lossy(),
            "display": {"w": 1080, "h": 2400},
        });
        std::fs::write(&entry_path, doc.to_string()).expect("write");

        let guest_gen = server.hub.list()[0].guest_gen;
        assert!(guest_gen > 0);

        // Invalid guest generation parameter is rejected with 400.
        let bad = server.raw(&format!("/api/frames/s_fg?t={}&g=0", server.token));
        assert_eq!(status_of(&bad), 400);

        // Stale guest generation is rejected with 409 SESSION_REPLACED.
        let stale = server.raw(&format!(
            "/api/frames/s_fg?t={}&g={}",
            server.token,
            guest_gen + 42
        ));
        assert_eq!(status_of(&stale), 409);
        let stale_body: serde_json::Value =
            serde_json::from_slice(body_of(&stale)).expect("stale json");
        assert_eq!(stale_body["code"], "SESSION_REPLACED");

        // Matching guest generation streams 200 OK with X-Guest-Gen and PNGDATA part.
        let mut conn = TcpStream::connect((Ipv4Addr::LOCALHOST, server.port)).expect("connect");
        let _ = conn.set_read_timeout(Some(Duration::from_secs(3)));
        write!(
            conn,
            "GET /api/frames/s_fg?t={}&g={} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\n\r\n",
            server.token, guest_gen, server.port
        )
        .expect("write");
        let head = read_response_head(&mut conn);
        assert!(head.starts_with("HTTP/1.1 200 OK"), "expected 200 OK: {head}");
        assert!(
            head.contains(&format!("X-Guest-Gen: {guest_gen}\r\n")),
            "expected X-Guest-Gen header in {head}"
        );
        let mut body = Vec::new();
        let mut buf = [0u8; 2048];
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        while std::time::Instant::now() < deadline {
            if let Ok(n) = conn.read(&mut buf) {
                if n == 0 {
                    break;
                }
                body.extend_from_slice(&buf[..n]);
                if body.windows(b"PNGDATA".len()).any(|w| w == b"PNGDATA") {
                    break;
                }
            }
        }
        assert!(
            body.windows(b"PNGDATA".len()).any(|w| w == b"PNGDATA"),
            "expected multipart frame containing PNGDATA, got {:?}",
            String::from_utf8_lossy(&body)
        );

        // While a watcher (`conn`) is still attached, remove and republish the exact same
        // registry entry; `refresh_locked` assigns a new generation and must replace the
        // retained slot so `X-Guest-Gen` matches the newly accepted generation.
        std::fs::remove_file(&entry_path).expect("remove entry");
        assert!(server.hub.list().is_empty());
        std::fs::write(&entry_path, doc.to_string()).expect("republish entry");
        let new_gen = server.hub.list()[0].guest_gen;
        assert_ne!(new_gen, guest_gen, "republished entry must advance guest_gen");

        // Old generation is now rejected with 409.
        let old_again = server.raw(&format!(
            "/api/frames/s_fg?t={}&g={}",
            server.token, guest_gen
        ));
        assert_eq!(status_of(&old_again), 409);

        // New generation succeeds and emits X-Guest-Gen == new_gen (not stale guest_gen).
        let mut conn2 = TcpStream::connect((Ipv4Addr::LOCALHOST, server.port)).expect("connect 2");
        let _ = conn2.set_read_timeout(Some(Duration::from_secs(3)));
        write!(
            conn2,
            "GET /api/frames/s_fg?t={}&g={} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\n\r\n",
            server.token, new_gen, server.port
        )
        .expect("write 2");
        let head2 = read_response_head(&mut conn2);
        assert!(head2.starts_with("HTTP/1.1 200 OK"), "expected 200 OK: {head2}");
        assert!(
            head2.contains(&format!("X-Guest-Gen: {new_gen}\r\n")),
            "expected refreshed X-Guest-Gen: {new_gen} in {head2}"
        );
        drop(conn2);
        drop(conn);
    }

    fn post_raw(
        port: u16,
        path: &str,
        host: Option<&str>,
        headers: &[(&str, &str)],
        body: &[u8],
    ) -> Vec<u8> {
        let mut stream = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).expect("connect");
        let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
        let host_val = host
            .map(str::to_string)
            .unwrap_or_else(|| format!("127.0.0.1:{port}"));
        let mut req = format!(
            "POST {path} HTTP/1.1\r\nHost: {host_val}\r\nConnection: close\r\n"
        );
        for (k, v) in headers {
            req.push_str(&format!("{k}: {v}\r\n"));
        }
        req.push_str("\r\n");
        stream.write_all(req.as_bytes()).expect("write head");
        if !body.is_empty() {
            let _ = stream.write_all(body);
        }
        let _ = stream.flush();
        let mut response = Vec::new();
        let _ = stream.read_to_end(&mut response);
        response
    }

    #[test]
    fn open_url_endpoint_forwards_to_daemon_and_maps_start_not_found() {
        let server = Server::start("open-url");
        let serve_sock = server.registry.join("serve.sock");
        let listener = UnixListener::bind(&serve_sock).expect("bind fake daemon");

        let received = Arc::new(std::sync::Mutex::new(Vec::<serde_json::Value>::new()));
        let received_bg = Arc::clone(&received);
        let _daemon = std::thread::spawn(move || {
            for incoming in listener.incoming().take(4) {
                let Ok(mut stream) = incoming else { break };
                let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
                let mut reader = std::io::BufReader::new(stream.try_clone().expect("clone"));
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 {
                    continue;
                }
                let req: serde_json::Value = serde_json::from_str(&line).expect("req json");
                received_bg.lock().unwrap().push(req.clone());
                let url = req
                    .pointer("/args/args/url")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("");
                let reply = if url.contains("missing") {
                    json!({
                        "id": req["id"],
                        "envelope": {
                            "ok": false,
                            "error": {
                                "code": "INVALID_ARGUMENT",
                                "message": "START_NOT_FOUND: no installed activity matches intent"
                            }
                        }
                    })
                } else {
                    json!({
                        "id": req["id"],
                        "envelope": {
                            "ok": true,
                            "result": {
                                "component": "com.example.app/.DetailActivity",
                                "delivery": "on_new_intent",
                                "frame_advanced": true
                            }
                        }
                    })
                };
                let _ = writeln!(stream, "{reply}");
                let _ = stream.flush();
            }
        });

        let live_doc = json!({
            "v": 1,
            "session_id": "s_dl",
            "daemon_pid": std::process::id(),
            "launcher_pid": std::process::id(),
            "started_ms": crate::live_registry::now_ms(),
            "serve_socket": serve_sock.to_string_lossy(),
            "display": {"w": 1080, "h": 2400},
        });
        std::fs::write(server.registry.join("s_dl.json"), live_doc.to_string()).expect("write live");

        let watch_doc = json!({
            "v": 1,
            "session_id": "s_ro",
            "daemon_pid": std::process::id(),
            "launcher_pid": std::process::id(),
            "started_ms": crate::live_registry::now_ms(),
            "watch_only": true,
            "display": {"w": 1080, "h": 2400},
        });
        std::fs::write(server.registry.join("s_ro.json"), watch_doc.to_string()).expect("write ro");

        // Auth, Host, and HTTP method gates.
        let body = br#"{"url":"myapp://item/42"}"#;
        let len_s = body.len().to_string();
        let no_tok = post_raw(
            server.port,
            "/api/open_url/s_dl",
            None,
            &[("Content-Type", "application/json"), ("Content-Length", &len_s)],
            body,
        );
        assert_eq!(status_of(&no_tok), 403);

        let bad_host = post_raw(
            server.port,
            &format!("/api/open_url/s_dl?t={}", server.token),
            Some(&format!("evil.example:{}", server.port)),
            &[("Content-Type", "application/json"), ("Content-Length", &len_s)],
            body,
        );
        assert_eq!(status_of(&bad_host), 403);

        let get_post_route = server.raw(&format!("/api/open_url/s_dl?t={}", server.token));
        assert_eq!(status_of(&get_post_route), 405);

        let post_root = post_raw(
            server.port,
            &format!("/?t={}", server.token),
            None,
            &[("Content-Length", "0")],
            b"",
        );
        assert_eq!(status_of(&post_root), 405);

        // Missing Content-Length -> 411; oversized body -> 413.
        let no_len = post_raw(
            server.port,
            &format!("/api/open_url/s_dl?t={}", server.token),
            None,
            &[("Content-Type", "application/json")],
            b"",
        );
        assert_eq!(status_of(&no_len), 411);

        let too_big_len = (MAX_OPEN_URL_BODY + 1).to_string();
        let too_big = post_raw(
            server.port,
            &format!("/api/open_url/s_dl?t={}", server.token),
            None,
            &[
                ("Content-Type", "application/json"),
                ("Content-Length", &too_big_len),
            ],
            b"",
        );
        assert_eq!(status_of(&too_big), 413);

        // Watch-only session -> 403 WATCH_ONLY.
        let ro_resp = post_raw(
            server.port,
            &format!("/api/open_url/s_ro?t={}", server.token),
            None,
            &[("Content-Type", "application/json"), ("Content-Length", &len_s)],
            body,
        );
        assert_eq!(status_of(&ro_resp), 403);
        let ro_json: serde_json::Value = serde_json::from_slice(body_of(&ro_resp)).expect("ro json");
        assert_eq!(ro_json["code"], "WATCH_ONLY");

        // Valid open_url forwards to daemon and returns 200.
        let ok_resp = post_raw(
            server.port,
            &format!("/api/open_url/s_dl?t={}", server.token),
            None,
            &[("Content-Type", "application/json"), ("Content-Length", &len_s)],
            body,
        );
        assert_eq!(status_of(&ok_resp), 200);
        let ok_json: serde_json::Value = serde_json::from_slice(body_of(&ok_resp)).expect("ok json");
        assert_eq!(ok_json["ok"], true);
        assert_eq!(ok_json["delivery"], "on_new_intent");

        // Unmatched URL -> 404 START_NOT_FOUND.
        let miss_body = br#"{"url":"https://missing.example/nope"}"#;
        let miss_len = miss_body.len().to_string();
        let miss_resp = post_raw(
            server.port,
            &format!("/api/open_url/s_dl?t={}", server.token),
            None,
            &[
                ("Content-Type", "application/json"),
                ("Content-Length", &miss_len),
            ],
            miss_body,
        );
        assert_eq!(status_of(&miss_resp), 404);
        let miss_json: serde_json::Value =
            serde_json::from_slice(body_of(&miss_resp)).expect("miss json");
        assert_eq!(miss_json["code"], "START_NOT_FOUND");

        let log = received.lock().unwrap();
        assert_eq!(log.len(), 2);
        assert_eq!(log[0]["tool"], "acos_service_ctl");
        assert_eq!(log[0]["args"]["module"], "intents");
        assert_eq!(log[0]["args"]["op"], "open_url");
        assert_eq!(log[0]["args"]["args"]["url"], "myapp://item/42");
    }

    #[test]
    fn picker_endpoint_streams_upload_cancels_and_enforces_limits() {
        let server = Server::start("picker");
        let serve_sock = server.registry.join("serve-pk.sock");
        let artifacts_dir = server.registry.join("artifacts");
        std::fs::create_dir_all(&artifacts_dir).expect("mkdir artifacts");
        let listener = UnixListener::bind(&serve_sock).expect("bind fake daemon");

        let received = Arc::new(std::sync::Mutex::new(Vec::<serde_json::Value>::new()));
        let received_bg = Arc::clone(&received);
        let _daemon = std::thread::spawn(move || {
            for incoming in listener.incoming().take(4) {
                let Ok(mut stream) = incoming else { break };
                let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
                let mut reader = std::io::BufReader::new(stream.try_clone().expect("clone"));
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 {
                    continue;
                }
                let req: serde_json::Value = serde_json::from_str(&line).expect("req json");
                received_bg.lock().unwrap().push(req.clone());
                let reply = json!({
                    "id": req["id"],
                    "envelope": {
                        "ok": true,
                        "result": {
                            "delivered": true
                        }
                    }
                });
                let _ = writeln!(stream, "{reply}");
                let _ = stream.flush();
            }
        });

        let live_doc = json!({
            "v": 1,
            "session_id": "s_pk",
            "daemon_pid": std::process::id(),
            "launcher_pid": std::process::id(),
            "started_ms": crate::live_registry::now_ms(),
            "serve_socket": serve_sock.to_string_lossy(),
            "launch_token": "tok-pk",
            "artifacts_dir": artifacts_dir.to_string_lossy(),
            "display": {"w": 1080, "h": 2400},
        });
        std::fs::write(server.registry.join("s_pk.json"), live_doc.to_string()).expect("write live");

        let ro_artifacts = server.registry.join("ro-artifacts");
        let ro_doc = json!({
            "v": 1,
            "session_id": "s_pk_ro",
            "daemon_pid": std::process::id(),
            "launcher_pid": std::process::id(),
            "started_ms": crate::live_registry::now_ms(),
            "watch_only": true,
            "artifacts_dir": ro_artifacts.to_string_lossy(),
            "display": {"w": 1080, "h": 2400},
        });
        std::fs::write(server.registry.join("s_pk_ro.json"), ro_doc.to_string()).expect("write ro");

        // 1. Watch-only session refused with 403 before creating any file.
        let ro_resp = post_raw(
            server.port,
            &format!("/api/picker/s_pk_ro?t={}&name=photo.png", server.token),
            None,
            &[("Content-Type", "image/png"), ("Content-Length", "4")],
            b"DATA",
        );
        assert_eq!(status_of(&ro_resp), 403);
        assert!(
            !ro_artifacts.join("picker").exists(),
            "watch-only upload must not create staged files"
        );

        // 2. Oversized Content-Length (> 25 MiB) refused with 413 before creating any file.
        let over_limit = (MAX_PICKER_UPLOAD + 1).to_string();
        let big_resp = post_raw(
            server.port,
            &format!("/api/picker/s_pk?t={}&name=huge.bin", server.token),
            None,
            &[
                ("Content-Type", "application/octet-stream"),
                ("Content-Length", &over_limit),
            ],
            b"",
        );
        assert_eq!(status_of(&big_resp), 413);
        assert!(
            !artifacts_dir.join("picker").exists(),
            "oversized upload must not create staged files"
        );

        // 3. Valid binary upload streams into <artifacts_dir>/picker/ with sanitized filename.
        let payload = b"\x89PNG\r\n\x1a\nfake-image-payload";
        let payload_len = payload.len().to_string();
        let up_resp = post_raw(
            server.port,
            &format!(
                "/api/picker/s_pk?t={}&name=..%2Fevil%2Fphoto.png&seq=4&mime=image%2Fpng",
                server.token
            ),
            None,
            &[
                ("Content-Type", "image/png"),
                ("Content-Length", &payload_len),
            ],
            payload,
        );
        assert_eq!(status_of(&up_resp), 200);
        let up_json: serde_json::Value = serde_json::from_slice(body_of(&up_resp)).expect("up json");
        assert_eq!(up_json["ok"], true);

        // 4. JSON cancel forwards {"cancel": true, "capture_seq": 4}.
        let cancel_body = br#"{"cancel":true,"capture_seq":4}"#;
        let cancel_len = cancel_body.len().to_string();
        let cancel_resp = post_raw(
            server.port,
            &format!("/api/picker/s_pk?t={}", server.token),
            None,
            &[
                ("Content-Type", "application/json"),
                ("Content-Length", &cancel_len),
            ],
            cancel_body,
        );
        assert_eq!(status_of(&cancel_resp), 200);

        // 5. A present but malformed selector is refused before anything is
        //    staged or sent to the daemon.
        let staged_count = || {
            std::fs::read_dir(artifacts_dir.join("picker"))
                .expect("picker dir")
                .count()
        };
        let before = staged_count();
        for (query, content_type, body) in [
            ("&name=a.bin&seq=4x", "application/octet-stream", &b"DATA"[..]),
            ("&seq=-1", "application/json", &br#"{"cancel":true}"#[..]),
            ("", "application/json", &br#"{"cancel":true,"capture_seq":"4"}"#[..]),
        ] {
            let length = body.len().to_string();
            let resp = post_raw(
                server.port,
                &format!("/api/picker/s_pk?t={}{query}", server.token),
                None,
                &[("Content-Type", content_type), ("Content-Length", &length)],
                body,
            );
            assert_eq!(status_of(&resp), 400, "{query} {content_type}");
            let json: serde_json::Value =
                serde_json::from_slice(body_of(&resp)).expect("error json");
            assert_eq!(json["code"], "INVALID_ARGUMENT");
        }
        assert_eq!(staged_count(), before);

        // 6. A named JSON file is an upload, not a cancellation, and a
        //    form-encoded space survives in its name.
        let file_body = br#"{"cancel":true}"#;
        let file_len = file_body.len().to_string();
        let json_up = post_raw(
            server.port,
            &format!(
                "/api/picker/s_pk?t={}&name=my+settings.json&seq=4&mime=application%2Fjson",
                server.token
            ),
            None,
            &[
                ("Content-Type", "application/json"),
                ("Content-Length", &file_len),
            ],
            file_body,
        );
        assert_eq!(status_of(&json_up), 200);

        let log = received.lock().unwrap();
        assert_eq!(log.len(), 3);
        assert_eq!(log[2]["args"]["args"]["display_name"], "my settings.json");
        assert_eq!(log[2]["args"]["args"]["mime_type"], "application/json");
        assert_eq!(log[2]["args"]["args"]["staged"], true);
        assert!(log[2]["args"]["args"].get("cancel").is_none());
        let staged_path = log[0]["args"]["args"]["path"]
            .as_str()
            .expect("staged path forwarded");
        assert!(
            std::path::Path::new(staged_path).starts_with(artifacts_dir.join("picker")),
            "staged file must live inside <artifacts_dir>/picker, got {staged_path}"
        );
        assert_eq!(std::fs::read(staged_path).expect("read staged"), payload);
        assert_eq!(log[0]["args"]["args"]["display_name"], "photo.png");
        assert_eq!(log[0]["args"]["args"]["mime_type"], "image/png");
        assert_eq!(log[0]["args"]["args"]["capture_seq"], 4);

        assert_eq!(log[0]["args"]["expected_launch_token"], "tok-pk");
        assert_eq!(log[1]["args"]["op"], "pick_file");
        assert_eq!(log[1]["args"]["args"]["cancel"], true);
        assert_eq!(log[1]["args"]["args"]["capture_seq"], 4);
    }

    /// `/api/picker` keeps the staged upload unless the daemon certainly did
    /// not run `pick_file`: a lost or timed-out reply may follow a
    /// registration whose URI the app already holds.
    #[test]
    fn picker_upload_survives_ambiguous_replies_and_is_removed_on_refusal() {
        use std::io::BufRead;

        let server = Server::start("picker-ambiguous");
        let artifacts_dir = server.registry.join("artifacts");
        std::fs::create_dir_all(&artifacts_dir).expect("mkdir artifacts");
        let serve_sock = server.registry.join("serve.sock");
        let listener = UnixListener::bind(&serve_sock).expect("bind");

        // One reply per request, in order; `None` closes the connection (EOF).
        let error = |code: &str| {
            Some(json!({"ok": false, "error": {"code": code, "message": code}}))
        };
        let replies = [
            None,
            error("TIMEOUT"),
            error("APP_NOT_RESPONDING"),
            error("INTERNAL"),
            error("INVALID_ARGUMENT"),
        ];
        let expected_kept = [true, true, true, true, false];
        let script = replies.clone();
        std::thread::spawn(move || {
            for reply in script {
                let Ok((mut stream, _)) = listener.accept() else {
                    return;
                };
                let mut line = String::new();
                let mut reader =
                    std::io::BufReader::new(stream.try_clone().expect("clone stream"));
                if reader.read_line(&mut line).unwrap_or(0) == 0 {
                    return;
                }
                if let Some(envelope) = reply {
                    let _ = writeln!(stream, "{}", json!({"id": 1, "envelope": envelope}));
                }
            }
        });
        let live_doc = json!({
            "v": 1,
            "session_id": "s_amb",
            "daemon_pid": std::process::id(),
            "launcher_pid": std::process::id(),
            "started_ms": crate::live_registry::now_ms(),
            "serve_socket": serve_sock.to_string_lossy(),
            "launch_token": "tok-amb",
            "artifacts_dir": artifacts_dir.to_string_lossy(),
            "display": {"w": 1080, "h": 2400},
        });
        std::fs::write(server.registry.join("s_amb.json"), live_doc.to_string()).expect("live");

        let staged = || -> usize {
            std::fs::read_dir(artifacts_dir.join("picker"))
                .map(Iterator::count)
                .unwrap_or(0)
        };
        for (index, kept) in expected_kept.into_iter().enumerate() {
            let before = staged();
            let response = post_raw(
                server.port,
                &format!("/api/picker/s_amb?t={}&name=a.bin", server.token),
                None,
                &[
                    ("Content-Type", "application/octet-stream"),
                    ("Content-Length", "4"),
                ],
                b"DATA",
            );
            assert_ne!(status_of(&response), 200, "reply {index}");
            assert_eq!(
                staged(),
                before + usize::from(kept),
                "reply {index}: {:?}",
                replies[index]
            );
        }
    }

    fn send(
        stream: &mut std::os::unix::net::UnixStream,
        kind: u8,
        payload: &[u8],
    ) -> io::Result<()> {
        let length = (payload.len() + 1) as u32;
        stream.write_all(&length.to_be_bytes())?;
        stream.write_all(&[kind])?;
        stream.write_all(payload)?;
        stream.flush()
    }
}
