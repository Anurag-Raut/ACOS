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
/// Largest JSON body accepted on `POST /api/annotations/<session_id>` (6 MiB).
const MAX_ANNOTATION_BODY: usize = 6 * 1024 * 1024;
/// Total deadline to read a POST body on `/api/annotations/<session_id>`.
const BODY_DEADLINE: Duration = Duration::from_secs(10);
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
    let is_annotations = request.path.starts_with("/api/annotations/");
    let method_allowed = match request.method.as_str() {
        "GET" => true,
        "POST" | "DELETE" => is_annotations,
        _ => false,
    };
    if !method_allowed {
        let _ = respond(
            &mut connection,
            405,
            "text/plain; charset=utf-8",
            b"405 method not allowed\n",
        );
        return;
    }
    // Every route requires the token, including the page, streams, tree, and annotations.
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
        stream_frames(connection, &request, hub, decode_component(session_id));
    } else if let Some(session_id) = path.strip_prefix("/api/events/") {
        stream_events(connection, hub, decode_component(session_id));
    } else if let Some(session_id) = path.strip_prefix("/api/input/") {
        handle_input_ws(connection, &request, hub, decode_component(session_id));
    } else if let Some(session_id) = path.strip_prefix("/api/tree/") {
        handle_tree(connection, &request, hub, decode_component(session_id));
    } else if let Some(rest) = path.strip_prefix("/api/annotations/") {
        handle_annotations(connection, &request, hub, rest);
    } else {
        let _ = respond(
            &mut connection,
            404,
            "text/plain; charset=utf-8",
            b"404 not found\n",
        );
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

fn respond_json(connection: &mut TcpStream, status: u16, value: &serde_json::Value) {
    let body = value.to_string();
    let _ = respond(
        connection,
        status,
        "application/json; charset=utf-8",
        body.as_bytes(),
    );
}

fn respond_rpc_result(
    connection: &mut TcpStream,
    result: Result<serde_json::Value, hub::SessionRpcError>,
) {
    match result {
        Ok(value) => respond_json(connection, 200, &value),
        Err(hub::SessionRpcError::NotFound) => respond_json(
            connection,
            404,
            &json!({
                "ok": false,
                "code": "SESSION_NOT_FOUND",
                "error": "no live session for that session_id",
            }),
        ),
        Err(hub::SessionRpcError::Replaced) => respond_json(
            connection,
            409,
            &json!({
                "ok": false,
                "code": "SESSION_REPLACED",
                "error": "session was replaced by a newer guest; reopen detail view",
            }),
        ),
        Err(hub::SessionRpcError::NoServeSocket) => respond_json(
            connection,
            503,
            &json!({
                "ok": false,
                "code": "SERVE_UNAVAILABLE",
                "error": "session has no serve_socket",
            }),
        ),
        Err(hub::SessionRpcError::OutcomeUnknown { cause, message }) => respond_json(
            connection,
            504,
            &json!({
                "ok": false,
                "code": "OUTCOME_UNKNOWN",
                "outcome_unknown": true,
                "cause": cause,
                "error": format!("annotation mutation outcome is unknown (it may have already succeeded): {message}"),
            }),
        ),
        Err(hub::SessionRpcError::DaemonError { code, message }) => {
            let status = match code.as_str() {
                "INVALID_ARGUMENT" => 400,
                "SESSION_NOT_FOUND" => 404,
                "SESSION_REPLACED" => 409,
                "INPUT_UNAVAILABLE" | "UNSUPPORTED_BACKEND" => 503,
                _ => 502,
            };
            respond_json(
                connection,
                status,
                &json!({
                    "ok": false,
                    "code": code,
                    "error": message,
                }),
            );
        }
    }
}

/// `GET /api/tree/<session_id>?t=<token>&g=<guest_gen>`: lightweight UI tree
/// JSON (no screenshot) via `serve_socket` for Mirror DOM & Visual Inspector.
fn handle_tree(
    mut connection: TcpStream,
    request: &Request,
    hub: &Arc<Hub>,
    session_id: String,
) {
    if session_id.is_empty() || session_id.contains('/') {
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
    respond_rpc_result(
        &mut connection,
        hub.observe_tree(&session_id, expected_guest_gen),
    );
}

/// `GET/POST /api/annotations/<session_id>` and
/// `DELETE /api/annotations/<session_id>/<ann_id>`.
fn handle_annotations(
    mut connection: TcpStream,
    request: &Request,
    hub: &Arc<Hub>,
    rest: &str,
) {
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

    match request.method.as_str() {
        "GET" => {
            if rest.is_empty() || rest.contains('/') {
                respond_json(
                    &mut connection,
                    404,
                    &json!({"ok": false, "code": "NOT_FOUND", "error": "expected /api/annotations/<session_id>"}),
                );
                return;
            }
            let session_id = decode_component(rest);
            let mut args = json!({ "action": "list" });
            if let Some(status) = request.param("status") {
                args["status"] = json!(status);
            }
            respond_rpc_result(
                &mut connection,
                hub.call_annotations(&session_id, expected_guest_gen, args),
            );
        }
        "POST" => {
            if rest.is_empty() || rest.contains('/') {
                respond_json(
                    &mut connection,
                    404,
                    &json!({"ok": false, "code": "NOT_FOUND", "error": "expected /api/annotations/<session_id>"}),
                );
                return;
            }
            let session_id = decode_component(rest);
            let mut body = match read_json_body(
                &mut connection,
                request,
                MAX_ANNOTATION_BODY,
                BODY_DEADLINE,
            ) {
                Ok(val) => val,
                Err((status, code, msg)) => {
                    respond_json(
                        &mut connection,
                        status,
                        &json!({"ok": false, "code": code, "error": msg}),
                    );
                    return;
                }
            };
            let Some(map) = body.as_object_mut() else {
                respond_json(
                    &mut connection,
                    400,
                    &json!({"ok": false, "code": "INVALID_ARGUMENT", "error": "JSON body must be an object"}),
                );
                return;
            };
            if map.get("action").is_none_or(serde_json::Value::is_null) {
                map.insert("action".to_string(), json!("add"));
            }
            respond_rpc_result(
                &mut connection,
                hub.call_annotations(&session_id, expected_guest_gen, body),
            );
        }
        "DELETE" => {
            let Some((raw_sid, raw_id)) = rest.split_once('/') else {
                respond_json(
                    &mut connection,
                    400,
                    &json!({"ok": false, "code": "INVALID_ARGUMENT", "error": "expected /api/annotations/<session_id>/<id>"}),
                );
                return;
            };
            if raw_sid.is_empty() || raw_id.contains('/') {
                respond_json(
                    &mut connection,
                    400,
                    &json!({"ok": false, "code": "INVALID_ARGUMENT", "error": "invalid annotation path"}),
                );
                return;
            }
            let session_id = decode_component(raw_sid);
            let Some(ann_id) = raw_id.trim().parse::<u64>().ok().filter(|&v| v >= 1) else {
                respond_json(
                    &mut connection,
                    400,
                    &json!({"ok": false, "code": "INVALID_ARGUMENT", "error": "annotation id must be a positive integer"}),
                );
                return;
            };
            respond_rpc_result(
                &mut connection,
                hub.call_annotations(
                    &session_id,
                    expected_guest_gen,
                    json!({"action": "delete", "id": ann_id}),
                ),
            );
        }
        _ => {
            let _ = respond(
                &mut connection,
                405,
                "text/plain; charset=utf-8",
                b"405 method not allowed\n",
            );
        }
    }
}

fn read_json_body(
    connection: &mut TcpStream,
    request: &Request,
    max_bytes: usize,
    deadline_dur: Duration,
) -> Result<serde_json::Value, (u16, &'static str, String)> {
    if request.transfer_encoding.is_some() {
        return Err((
            400,
            "INVALID_ARGUMENT",
            "Transfer-Encoding is not supported; send a fixed Content-Length".to_string(),
        ));
    }
    let content_length = match request.content_length {
        None => {
            return Err((
                411,
                "LENGTH_REQUIRED",
                "POST /api/annotations requires a Content-Length header".to_string(),
            ));
        }
        Some(Err(())) => {
            return Err((
                400,
                "INVALID_ARGUMENT",
                "invalid or conflicting Content-Length header".to_string(),
            ));
        }
        Some(Ok(len)) => len,
    };
    if content_length > max_bytes {
        return Err((
            413,
            "PAYLOAD_TOO_LARGE",
            format!("request body ({content_length} bytes) exceeds {max_bytes}-byte limit"),
        ));
    }
    let mut body = Vec::with_capacity(content_length);
    let prefix_take = request.body_prefix.len().min(content_length);
    body.extend_from_slice(&request.body_prefix[..prefix_take]);
    let deadline = Instant::now() + deadline_dur;
    let mut buf = [0u8; 8192];
    while body.len() < content_length {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() || connection.set_read_timeout(Some(remaining)).is_err() {
            return Err((
                408,
                "TIMEOUT",
                "timed out reading request body".to_string(),
            ));
        }
        let want = (content_length - body.len()).min(buf.len());
        match connection.read(&mut buf[..want]) {
            Ok(0) => {
                return Err((
                    400,
                    "INVALID_ARGUMENT",
                    "unexpected EOF while reading request body".to_string(),
                ));
            }
            Ok(n) => body.extend_from_slice(&buf[..n]),
            Err(err) if err.kind() == ErrorKind::Interrupted => continue,
            Err(_) => {
                return Err((
                    408,
                    "TIMEOUT",
                    "timed out reading request body".to_string(),
                ));
            }
        }
    }
    serde_json::from_slice(&body).map_err(|err| {
        (
            400,
            "INVALID_ARGUMENT",
            format!("invalid JSON body: {err}"),
        )
    })
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
        408 => "Request Timeout",
        409 => "Conflict",
        411 => "Length Required",
        413 => "Payload Too Large",
        502 => "Bad Gateway",
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
    content_length: Option<Result<usize, ()>>,
    transfer_encoding: Option<String>,
    body_prefix: Vec<u8>,
}

impl Request {
    fn param(&self, name: &str) -> Option<String> {
        for pair in self.query.split('&') {
            let Some((key, value)) = pair.split_once('=') else {
                continue;
            };
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
    let head_bytes = &head[..sep];
    let body_prefix = head[sep + 4..].to_vec();
    let text = String::from_utf8_lossy(head_bytes);
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
    let mut content_length: Option<Result<usize, ()>> = None;
    let mut transfer_encoding = None;
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
            let parsed = value.parse::<usize>().map_err(|_| ());
            content_length = match (content_length, parsed) {
                (None, res) => Some(res),
                (Some(Ok(prev)), Ok(next)) if prev == next => Some(Ok(prev)),
                _ => Some(Err(())),
            };
        } else if name.eq_ignore_ascii_case("transfer-encoding") {
            transfer_encoding = Some(value);
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
        transfer_encoding,
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

    fn request_raw(
        port: u16,
        method: &str,
        path: &str,
        host: &str,
        extra_headers: &str,
        body: &[u8],
    ) -> Vec<u8> {
        let mut stream = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).expect("connect");
        let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
        let mut req = format!(
            "{method} {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n{extra_headers}\r\n"
        )
        .into_bytes();
        req.extend_from_slice(body);
        stream.write_all(&req).expect("write request");
        let mut response = Vec::new();
        let _ = stream.read_to_end(&mut response);
        response
    }

    fn post_json(port: u16, path: &str, body: &serde_json::Value) -> Vec<u8> {
        let bytes = body.to_string().into_bytes();
        let headers = format!(
            "Content-Type: application/json\r\nContent-Length: {}\r\n",
            bytes.len()
        );
        request_raw(
            port,
            "POST",
            path,
            &format!("127.0.0.1:{port}"),
            &headers,
            &bytes,
        )
    }

    fn delete_req(port: u16, path: &str) -> Vec<u8> {
        request_raw(port, "DELETE", path, &format!("127.0.0.1:{port}"), "", b"")
    }

    #[test]
    fn tree_and_annotations_enforce_token_host_and_method() {
        let server = Server::start("annot-auth");
        let host = format!("127.0.0.1:{}", server.port);
        let bad_host = format!("evil.example:{}", server.port);

        // Missing or wrong token -> 403 on tree and annotations (GET/POST/DELETE).
        for (method, path) in [
            ("GET", "/api/tree/s1"),
            ("GET", "/api/annotations/s1"),
            ("POST", "/api/annotations/s1"),
            ("DELETE", "/api/annotations/s1/1"),
            ("GET", "/api/tree/s1?t=wrong"),
            ("POST", "/api/annotations/s1?t=wrong"),
        ] {
            let resp = request_raw(server.port, method, path, &host, "", b"");
            assert_eq!(status_of(&resp), 403, "{method} {path} must require token");
        }

        // Bad Host header -> 403 even with valid token.
        for (method, path) in [
            ("GET", format!("/api/tree/s1?t={}", server.token)),
            ("GET", format!("/api/annotations/s1?t={}", server.token)),
            ("POST", format!("/api/annotations/s1?t={}", server.token)),
            ("DELETE", format!("/api/annotations/s1/1?t={}", server.token)),
        ] {
            let resp = request_raw(server.port, method, &path, &bad_host, "", b"");
            assert_eq!(status_of(&resp), 403, "{method} {path} must reject bad Host");
        }

        // POST / DELETE are rejected with 405 on all non-annotation routes.
        for (method, path) in [
            ("POST", format!("/?t={}", server.token)),
            ("POST", format!("/api/sessions?t={}", server.token)),
            ("POST", format!("/api/tree/s1?t={}", server.token)),
            ("DELETE", format!("/api/tree/s1?t={}", server.token)),
            ("PUT", format!("/api/annotations/s1?t={}", server.token)),
        ] {
            let resp = request_raw(server.port, method, &path, &host, "", b"");
            assert_eq!(status_of(&resp), 405, "{method} {path} must be 405");
        }
    }

    #[test]
    fn annotations_post_rejects_oversized_body_with_413() {
        let server = Server::start("annot-413");
        let host = format!("127.0.0.1:{}", server.port);
        let path = format!("/api/annotations/s1?t={}", server.token);
        let huge_len = MAX_ANNOTATION_BODY + 1024;
        let headers = format!("Content-Type: application/json\r\nContent-Length: {huge_len}\r\n");
        let resp = request_raw(server.port, "POST", &path, &host, &headers, b"{}");
        assert_eq!(status_of(&resp), 413, "oversized POST must return 413");
        let body: serde_json::Value = serde_json::from_slice(body_of(&resp)).expect("413 json");
        assert_eq!(body["ok"], false);
        assert_eq!(body["code"], "PAYLOAD_TOO_LARGE");
    }

    #[test]
    fn tree_endpoint_proxies_lightweight_observe_tree_and_enforces_guest_gen() {
        let server = Server::start("tree-rpc");

        // Missing session -> 404.
        let missing = server.raw(&format!("/api/tree/nope?t={}", server.token));
        assert_eq!(status_of(&missing), 404);

        // Live session without serve_socket -> 503 SERVE_UNAVAILABLE.
        let no_sock_entry = server.registry.join("s_nosock.json");
        std::fs::write(
            &no_sock_entry,
            json!({
                "v": 1,
                "session_id": "s_nosock",
                "daemon_pid": std::process::id(),
                "launcher_pid": std::process::id(),
                "started_ms": crate::live_registry::now_ms(),
                "display": {"w": 1080, "h": 2400},
            })
            .to_string(),
        )
        .expect("write s_nosock");
        let no_sock = server.raw(&format!("/api/tree/s_nosock?t={}", server.token));
        assert_eq!(status_of(&no_sock), 503);
        let no_sock_json: serde_json::Value =
            serde_json::from_slice(body_of(&no_sock)).expect("503 json");
        assert_eq!(no_sock_json["code"], "SERVE_UNAVAILABLE");

        // Live session with a fake serve_socket verifying no screenshot is requested.
        let serve_sock = server.registry.join("tree.serve.sock");
        let listener = UnixListener::bind(&serve_sock).expect("bind fake serve");
        let observed_req = Arc::new(Mutex::new(None::<serde_json::Value>));
        let observed_req_bg = Arc::clone(&observed_req);
        let daemon = std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut reader = std::io::BufReader::new(stream.try_clone().expect("clone"));
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) > 0 {
                    let req: serde_json::Value = serde_json::from_str(&line).expect("req json");
                    *observed_req_bg.lock().expect("lock") = Some(req.clone());
                    let id = req["id"].as_u64().unwrap_or(1);
                    let reply = json!({
                        "id": id,
                        "envelope": {
                            "ok": true,
                            "result": {
                                "package": "com.example.shop",
                                "activity": "com.example.shop/.LoginActivity",
                                "display": {"w": 1080, "h": 2400},
                                "tree": {
                                    "nodes": [{
                                        "class": "android.widget.Button",
                                        "resource_id": "com.example.shop:id/btn_login",
                                        "text": "Sign in",
                                        "bounds": "[120,400][960,520]",
                                        "visible": true,
                                        "clickable": true,
                                        "children": []
                                    }]
                                }
                            }
                        }
                    });
                    let _ = writeln!(stream, "{reply}");
                    let _ = stream.flush();
                }
            }
        });

        let entry_path = server.registry.join("s_tree.json");
        std::fs::write(
            &entry_path,
            json!({
                "v": 1,
                "session_id": "s_tree",
                "daemon_pid": std::process::id(),
                "launcher_pid": std::process::id(),
                "started_ms": crate::live_registry::now_ms(),
                "serve_socket": serve_sock.to_string_lossy(),
                "launch_token": "tok-tree-1",
                "package": "com.example.shop",
                "activity": "com.example.shop/.LoginActivity",
                "display": {"w": 1080, "h": 2400},
            })
            .to_string(),
        )
        .expect("write s_tree");

        let guest_gen = server
            .hub
            .list()
            .into_iter()
            .find(|s| s.entry.session_id == "s_tree")
            .expect("s_tree listed")
            .guest_gen;

        // Invalid g=0 -> 400.
        let bad_g = server.raw(&format!("/api/tree/s_tree?t={}&g=0", server.token));
        assert_eq!(status_of(&bad_g), 400);

        // Stale g -> 409 SESSION_REPLACED.
        let stale = server.raw(&format!(
            "/api/tree/s_tree?t={}&g={}",
            server.token,
            guest_gen + 99
        ));
        assert_eq!(status_of(&stale), 409);
        let stale_json: serde_json::Value =
            serde_json::from_slice(body_of(&stale)).expect("stale json");
        assert_eq!(stale_json["code"], "SESSION_REPLACED");

        // Matching g -> 200 OK with tree/activity/package/display and NO screenshot arg.
        let ok = server.raw(&format!(
            "/api/tree/s_tree?t={}&g={}",
            server.token, guest_gen
        ));
        assert_eq!(status_of(&ok), 200);
        let body: serde_json::Value = serde_json::from_slice(body_of(&ok)).expect("tree json");
        assert_eq!(body["ok"], true);
        assert_eq!(body["activity"], "com.example.shop/.LoginActivity");
        assert_eq!(body["package"], "com.example.shop");
        assert_eq!(body["display"], json!({"w": 1080, "h": 2400}));
        assert_eq!(body["tree"]["nodes"][0]["text"], "Sign in");

        daemon.join().expect("daemon join");
        let sent = observed_req.lock().expect("lock").clone().expect("req sent");
        assert_eq!(sent["tool"], "acos_observe_tree");
        assert_eq!(sent["args"]["full"], true);
        assert_eq!(sent["args"]["expected_launch_token"], "tok-tree-1");
        assert!(
            sent["args"].get("screenshot").is_none(),
            "/api/tree must never request screenshot: true"
        );
    }

    #[test]
    fn annotations_http_lifecycle_png_disk_multiscreen_and_watch_only() {
        let server = Server::start("annot-life");
        let artifacts_dir = server.registry.join("artifacts");
        std::fs::create_dir_all(&artifacts_dir).expect("mkdir artifacts");
        let serve_sock = server.registry.join("annot.serve.sock");
        let listener = UnixListener::bind(&serve_sock).expect("bind fake serve");
        let store = Arc::new(Mutex::new(crate::engine::annotations::AnnotationStore::new()));
        let store_bg = Arc::clone(&store);
        let artifacts_bg = artifacts_dir.clone();

        let daemon = std::thread::spawn(move || {
            for incoming in listener.incoming().take(8) {
                let Ok(mut stream) = incoming else { break };
                let mut reader = std::io::BufReader::new(stream.try_clone().expect("clone"));
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 {
                    continue;
                }
                let req: serde_json::Value = serde_json::from_str(&line).expect("req json");
                let id = req["id"].as_u64().unwrap_or(1);
                assert_eq!(req["tool"], "acos_annotations");
                assert_eq!(req["args"]["expected_launch_token"], "tok-annot-1");
                let defaults = crate::engine::annotations::AnnotationDefaults {
                    activity: Some("com.example.app/.DefaultActivity".to_string()),
                    package: Some("com.example.app".to_string()),
                    display: Some(crate::engine::annotations::DisplaySize { w: 1080, h: 2400 }),
                };
                let envelope = match store_bg
                    .lock()
                    .expect("store")
                    .apply(&req["args"], &artifacts_bg, defaults)
                {
                    Ok(result) => json!({ "ok": true, "result": result }),
                    Err(err) => json!({
                        "ok": false,
                        "error": { "code": format!("{:?}", err.code()), "message": err.to_string() }
                    }),
                };
                let _ = writeln!(stream, "{}", json!({ "id": id, "envelope": envelope }));
                let _ = stream.flush();
            }
        });

        // Register an interactive acos serve session (with serve_socket).
        let entry_path = server.registry.join("s_annot.json");
        std::fs::write(
            &entry_path,
            json!({
                "v": 1,
                "session_id": "s_annot",
                "daemon_pid": std::process::id(),
                "launcher_pid": std::process::id(),
                "started_ms": crate::live_registry::now_ms(),
                "serve_socket": serve_sock.to_string_lossy(),
                "launch_token": "tok-annot-1",
                "watch_only": false,
                "package": "com.example.app",
                "activity": "com.example.app/.ScreenOne",
                "display": {"w": 1080, "h": 2400},
            })
            .to_string(),
        )
        .expect("write s_annot");

        let sessions_resp = server.raw(&format!("/api/sessions?t={}", server.token));
        assert_eq!(status_of(&sessions_resp), 200);
        let sessions_json: serde_json::Value =
            serde_json::from_slice(body_of(&sessions_resp)).expect("sessions json");
        assert_eq!(sessions_json["sessions"][0]["can_annotate"], true);
        assert!(
            sessions_json["sessions"][0].get("serve_socket").is_none(),
            "serve_socket must never be exposed to browsers"
        );

        let guest_gen = server.hub.list()[0].guest_gen;
        let base_path = format!("/api/annotations/s_annot?t={}&g={}", server.token, guest_gen);

        // Valid PNG magic + 4 bytes (\x89PNG\r\n\x1a\nABCD) base64-encoded.
        let valid_png_b64 = "iVBORw0KGgpBQkNE";

        // 1. Add #1 on ScreenOne with PNG screenshot (omitting `action` defaults to `add`).
        let add1 = post_json(
            server.port,
            &base_path,
            &json!({
                "comment": "Button text wraps awkwardly",
                "kind": "element",
                "bounds": [120, 400, 960, 520],
                "activity": "com.example.app/.ScreenOne",
                "nodes": [{"class": "android.widget.Button", "resource_id": "btn_go", "text": "Go"}],
                "image_png_base64": valid_png_b64,
            }),
        );
        assert_eq!(status_of(&add1), 200);
        let add1_json: serde_json::Value = serde_json::from_slice(body_of(&add1)).expect("add1");
        assert_eq!(add1_json["ok"], true);
        assert_eq!(add1_json["annotation"]["id"], 1);
        assert_eq!(add1_json["annotation"]["activity"], "com.example.app/.ScreenOne");
        let shot_path = add1_json["annotation"]["screenshot_path"]
            .as_str()
            .expect("screenshot_path")
            .to_string();
        let disk_bytes = std::fs::read(&shot_path).expect("annotation png written on disk");
        assert_eq!(&disk_bytes, b"\x89PNG\r\n\x1a\nABCD");

        // 2. Add #2 on ScreenTwo (multi-screen batch preservation).
        let add2 = post_json(
            server.port,
            &base_path,
            &json!({
                "action": "add",
                "comment": "Checkout total overlaps badge",
                "kind": "box",
                "bounds": [40, 1800, 1040, 2100],
                "activity": "com.example.app/.ScreenTwo",
            }),
        );
        assert_eq!(status_of(&add2), 200);
        let add2_json: serde_json::Value = serde_json::from_slice(body_of(&add2)).expect("add2");
        assert_eq!(add2_json["annotation"]["id"], 2);
        assert_eq!(add2_json["open_count"], 2);

        // 3. GET open list contains both ScreenOne (#1) and ScreenTwo (#2).
        let list_open = server.raw(&base_path);
        assert_eq!(status_of(&list_open), 200);
        let list_json: serde_json::Value =
            serde_json::from_slice(body_of(&list_open)).expect("list");
        assert_eq!(list_json["ok"], true);
        assert_eq!(list_json["open_count"], 2);
        let items = list_json["annotations"].as_array().expect("annotations array");
        assert_eq!(items.len(), 2);
        assert_eq!(items[0]["activity"], "com.example.app/.ScreenOne");
        assert_eq!(items[1]["activity"], "com.example.app/.ScreenTwo");

        // 4. Resolve #1.
        let res1 = post_json(
            server.port,
            &base_path,
            &json!({ "action": "resolve", "id": 1, "resolution_note": "Increased padding" }),
        );
        assert_eq!(status_of(&res1), 200);
        let res1_json: serde_json::Value = serde_json::from_slice(body_of(&res1)).expect("res1");
        assert_eq!(res1_json["annotation"]["status"], "resolved");
        assert_eq!(res1_json["open_count"], 1);

        // 5. GET ?status=resolved returns only #1.
        let list_res = server.raw(&format!("{base_path}&status=resolved"));
        assert_eq!(status_of(&list_res), 200);
        let list_res_json: serde_json::Value =
            serde_json::from_slice(body_of(&list_res)).expect("list resolved");
        assert_eq!(list_res_json["count"], 1);
        assert_eq!(list_res_json["annotations"][0]["id"], 1);

        // 6. DELETE /api/annotations/s_annot/1 removes #1 and deletes its PNG file.
        let del1 = delete_req(
            server.port,
            &format!("/api/annotations/s_annot/1?t={}&g={}", server.token, guest_gen),
        );
        assert_eq!(status_of(&del1), 200);
        assert!(
            !std::path::Path::new(&shot_path).exists(),
            "deleting annotation must remove its PNG artifact"
        );

        // 7. POST clear removes remaining open annotation #2.
        let cleared = post_json(
            server.port,
            &base_path,
            &json!({ "action": "clear", "status": "all" }),
        );
        assert_eq!(status_of(&cleared), 200);
        let cleared_json: serde_json::Value =
            serde_json::from_slice(body_of(&cleared)).expect("cleared");
        assert_eq!(cleared_json["cleared"], 1);
        assert_eq!(cleared_json["total_count"], 0);

        // 8. Final list is empty.
        let empty_list = server.raw(&format!("{base_path}&status=all"));
        assert_eq!(status_of(&empty_list), 200);
        let empty_json: serde_json::Value =
            serde_json::from_slice(body_of(&empty_list)).expect("empty");
        assert_eq!(empty_json["count"], 0);

        // Stale guest_gen is rejected with 409 before touching serve_socket.
        let stale = server.raw(&format!(
            "/api/annotations/s_annot?t={}&g={}",
            server.token,
            guest_gen + 7
        ));
        assert_eq!(status_of(&stale), 409);

        daemon.join().expect("daemon join");
    }

    #[test]
    fn watch_only_without_serve_socket_returns_503_and_can_annotate_false() {
        let server = Server::start("watch-only-no-annot");
        let entry_path = server.registry.join("s_verify.json");
        std::fs::write(
            &entry_path,
            json!({
                "v": 1,
                "session_id": "s_verify",
                "daemon_pid": std::process::id(),
                "launcher_pid": std::process::id(),
                "started_ms": crate::live_registry::now_ms(),
                "watch_only": true,
                "package": "com.example.verify",
                "activity": "com.example.verify/.MainActivity",
                "display": {"w": 1080, "h": 2400},
            })
            .to_string(),
        )
        .expect("write s_verify");

        // /api/sessions exposes can_annotate: false and never leaks serve_socket.
        let sessions_resp = server.raw(&format!("/api/sessions?t={}", server.token));
        assert_eq!(status_of(&sessions_resp), 200);
        let sessions_json: serde_json::Value =
            serde_json::from_slice(body_of(&sessions_resp)).expect("sessions json");
        assert_eq!(sessions_json["sessions"][0]["watch_only"], true);
        assert_eq!(sessions_json["sessions"][0]["can_annotate"], false);
        assert!(sessions_json["sessions"][0].get("serve_socket").is_none());

        let guest_gen = server.hub.list()[0].guest_gen;
        let tree = server.raw(&format!("/api/tree/s_verify?t={}&g={}", server.token, guest_gen));
        assert_eq!(status_of(&tree), 503);
        let tree_json: serde_json::Value = serde_json::from_slice(body_of(&tree)).expect("tree");
        assert_eq!(tree_json["code"], "SERVE_UNAVAILABLE");

        let list = server.raw(&format!(
            "/api/annotations/s_verify?t={}&g={}",
            server.token, guest_gen
        ));
        assert_eq!(status_of(&list), 503);
        let list_json: serde_json::Value = serde_json::from_slice(body_of(&list)).expect("list");
        assert_eq!(list_json["code"], "SERVE_UNAVAILABLE");

        let add = post_json(
            server.port,
            &format!("/api/annotations/s_verify?t={}&g={}", server.token, guest_gen),
            &json!({ "comment": "hello" }),
        );
        assert_eq!(status_of(&add), 503);
        let add_json: serde_json::Value = serde_json::from_slice(body_of(&add)).expect("add");
        assert_eq!(add_json["code"], "SERVE_UNAVAILABLE");

        let del = delete_req(
            server.port,
            &format!("/api/annotations/s_verify/1?t={}&g={}", server.token, guest_gen),
        );
        assert_eq!(status_of(&del), 503);
        let del_json: serde_json::Value = serde_json::from_slice(body_of(&del)).expect("del");
        assert_eq!(del_json["code"], "SERVE_UNAVAILABLE");
    }

    #[test]
    fn annotations_mutation_post_write_eof_returns_504_outcome_unknown() {
        let server = Server::start("annot-unknown");
        let serve_sock = server.registry.join("unknown.serve.sock");
        let listener = UnixListener::bind(&serve_sock).expect("bind fake serve");

        // Fake daemon reads each request line (confirming write succeeded) then drops the
        // socket without replying — a post-write ambiguous outcome.
        let daemon = std::thread::spawn(move || {
            for incoming in listener.incoming().take(2) {
                let Ok(stream) = incoming else { break };
                let mut reader = std::io::BufReader::new(stream);
                let mut line = String::new();
                let _ = reader.read_line(&mut line);
                // Drop without writing a reply -> EOF after write.
            }
        });

        let entry_path = server.registry.join("s_unk.json");
        std::fs::write(
            &entry_path,
            json!({
                "v": 1,
                "session_id": "s_unk",
                "daemon_pid": std::process::id(),
                "launcher_pid": std::process::id(),
                "started_ms": crate::live_registry::now_ms(),
                "serve_socket": serve_sock.to_string_lossy(),
                "launch_token": "tok-unk-1",
                "display": {"w": 1080, "h": 2400},
            })
            .to_string(),
        )
        .expect("write s_unk");

        let guest_gen = server.hub.list()[0].guest_gen;

        let post_resp = post_json(
            server.port,
            &format!("/api/annotations/s_unk?t={}&g={}", server.token, guest_gen),
            &json!({ "action": "add", "comment": "May have saved" }),
        );
        assert_eq!(status_of(&post_resp), 504);
        let post_body: serde_json::Value =
            serde_json::from_slice(body_of(&post_resp)).expect("504 post json");
        assert_eq!(post_body["ok"], false);
        assert_eq!(post_body["code"], "OUTCOME_UNKNOWN");
        assert_eq!(post_body["outcome_unknown"], true);

        let del_resp = delete_req(
            server.port,
            &format!("/api/annotations/s_unk/1?t={}&g={}", server.token, guest_gen),
        );
        assert_eq!(status_of(&del_resp), 504);
        let del_body: serde_json::Value =
            serde_json::from_slice(body_of(&del_resp)).expect("504 del json");
        assert_eq!(del_body["ok"], false);
        assert_eq!(del_body["code"], "OUTCOME_UNKNOWN");
        assert_eq!(del_body["outcome_unknown"], true);

        daemon.join().expect("daemon join");
    }

    #[test]
    fn page_contains_mirror_dom_and_annotate_ui_markers() {
        let server = Server::start("page-annot");
        let page = server.raw(&format!("/?t={}", server.token));
        assert_eq!(status_of(&page), 200);
        let html = String::from_utf8_lossy(body_of(&page)).to_string();
        for needle in [
            "id=\"mirror-dom\"",
            "acos-mirror-node",
            "data-acos-node",
            "data-android-class",
            "data-resource-id",
            "data-bounds",
            "/api/tree/",
            "/api/annotations/",
            "btn-annotate",
            "btn-annot-pick",
            "btn-annot-box",
            "btn-annot-draw",
            "btn-annot-copy",
            "annot-popover",
            "annot-comment",
            "tab-annotations",
            "Copy All for Agent",
            "Copy for Agent",
            "compositeAnnotationPng",
            "formatAnnotationsForAgent",
        ] {
            assert!(html.contains(needle), "page missing {needle}");
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
