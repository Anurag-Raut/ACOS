//! The MCP server: dispatch, concurrency, cancellation and result mapping.
//!
//! The transport is newline-delimited JSON-RPC 2.0. Fast methods (`initialize`,
//! `ping`, `tools/list`, notifications) are handled on the reader thread;
//! `tools/call` runs on a worker thread behind a bounded [`Gate`] so that a
//! long-running backend call cannot block cancellation or pings, and so that
//! concurrent calls are limited. Every diagnostic goes to stderr, never stdout.

use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use acos_protocol::{
    Backend as ProtocolBackend, CommandName, Envelope, ErrorCode, Fidelity, Metrics, ProtocolError,
    ResourceBudget,
};
use serde_json::{json, Value};

use acos_protocol::reply::{self, TreeShapeOptions};

use crate::backend::{Backend, CallContext, Capabilities};
use crate::descriptors::{
    command_from_tool, negotiate, supports_structured_content, CAPABILITIES_TOOL, HELP_TOOL,
    INSTRUCTIONS, LATEST_PROTOCOL_VERSION, SERVER_NAME, SERVER_VERSION, SESSION_TOOL_NAMES,
};
use crate::gate::{Gate, GateError};
use crate::help;
use crate::jsonrpc::{self, Parsed, RpcError, INVALID_PARAMS, METHOD_NOT_FOUND, PARSE_ERROR};
use crate::process::CancelToken;
use crate::session::SessionRegistry;
use crate::transport::{read_line_bounded, ReadOutcome};

/// Server-wide limits and paths.
#[derive(Debug, Clone)]
pub struct ServerConfig {
    /// Maximum bytes for one request frame.
    pub max_request_bytes: usize,
    /// Maximum characters in one string argument.
    pub max_arg_chars: usize,
    /// Maximum concurrent backend calls.
    pub max_concurrency: usize,
    /// How long a queued call waits for a permit.
    pub queue_timeout: Duration,
    /// Default call wall-clock budget when the request declares none.
    pub default_call_timeout: Duration,
    /// Canonical roots a path argument may live under.
    pub allowed_roots: Vec<PathBuf>,
    /// Directory that holds produced artifacts.
    pub artifacts_dir: PathBuf,
    /// Idle session TTL.
    pub session_ttl: Duration,
    /// How long in-flight calls may finish after stdin closes before they are
    /// cancelled.
    pub shutdown_grace: Duration,
    /// Maximum bytes per captured stream.
    pub max_output_bytes: usize,
    /// Maximum image size to inline as MCP image content.
    pub max_image_bytes: usize,
    /// Long edge of an inlined image preview, in pixels (`ACOS_MCP_PREVIEW_MAX_SIDE`,
    /// default 1280). A bigger image is downscaled before it is inlined; the
    /// reply still names the lossless file (SHOTZ, ADR-0027 #6).
    pub max_image_side: u32,
    /// Maximum bytes for one string inside a tool reply (`ACOS_MCP_MAX_TEXT`,
    /// default 4000). A longer string is head/tail-capped and its full text is
    /// kept once on disk in the run's artifacts; see [`crate::bound`].
    pub max_text_bytes: usize,
}

impl Default for ServerConfig {
    fn default() -> Self {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let artifacts = cwd.join(".acos-mcp-artifacts");
        ServerConfig {
            max_request_bytes: 4 * 1024 * 1024,
            max_arg_chars: crate::safety::DEFAULT_MAX_ARG_CHARS,
            max_concurrency: 4,
            queue_timeout: Duration::from_secs(5),
            default_call_timeout: crate::backend::DEFAULT_CALL_TIMEOUT,
            allowed_roots: vec![cwd, artifacts.clone()],
            artifacts_dir: artifacts,
            session_ttl: Duration::from_secs(600),
            shutdown_grace: Duration::from_secs(2),
            max_output_bytes: crate::backend::DEFAULT_MAX_OUTPUT_BYTES,
            max_image_bytes: crate::preview::DEFAULT_MAX_BYTES,
            max_image_side: crate::preview::DEFAULT_MAX_SIDE,
            max_text_bytes: crate::bound::DEFAULT_MAX_TEXT_BYTES,
        }
    }
}

impl ServerConfig {
    /// Read limits and paths from the environment.
    #[allow(clippy::field_reassign_with_default)]
    pub fn from_env() -> Self {
        let mut config = ServerConfig::default();

        if let Some(value) = env_usize("ACOS_MCP_MAX_REQUEST_BYTES") {
            config.max_request_bytes = value;
        }
        if let Some(value) = env_usize("ACOS_MCP_MAX_CONCURRENCY") {
            config.max_concurrency = value.max(1);
        }
        if let Some(value) = env_u64("ACOS_MCP_QUEUE_TIMEOUT_MS") {
            config.queue_timeout = Duration::from_millis(value);
        }
        if let Some(value) = env_u64("ACOS_MCP_CALL_TIMEOUT_MS") {
            config.default_call_timeout = Duration::from_millis(value);
        }
        if let Some(value) = env_usize("ACOS_MCP_MAX_OUTPUT_BYTES") {
            config.max_output_bytes = value;
        }
        if let Some(value) = env_usize("ACOS_MCP_MAX_IMAGE_BYTES") {
            config.max_image_bytes = value;
        }
        if let Some(value) = env_usize("ACOS_MCP_PREVIEW_MAX_SIDE") {
            // A preview of zero pixels is not a preview.
            config.max_image_side = value.max(1) as u32;
        }
        // F11: `ACOS_MCP_MAX_TEXT` bounds every string in a reply. `0` disables
        // the bound (the CLI behaviour, unbounded).
        if let Some(value) = env_usize("ACOS_MCP_MAX_TEXT") {
            config.max_text_bytes = value;
        }
        if let Some(value) = env_u64("ACOS_MCP_SESSION_TTL_MS") {
            config.session_ttl = Duration::from_millis(value);
        }
        if let Some(value) = env_u64("ACOS_MCP_SHUTDOWN_GRACE_MS") {
            config.shutdown_grace = Duration::from_millis(value);
        }
        if let Some(value) = std::env::var_os("ACOS_ARTIFACTS_DIR") {
            config.artifacts_dir = PathBuf::from(value);
        }

        let mut roots: Vec<PathBuf> = match std::env::var_os("ACOS_ALLOWED_ROOTS") {
            Some(value) => std::env::split_paths(&value).collect(),
            None => vec![std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))],
        };
        roots.push(config.artifacts_dir.clone());
        config.allowed_roots = canonicalize_roots(&roots);
        config.artifacts_dir = canonicalize_root(&config.artifacts_dir);
        config
    }

    /// Config with explicit roots, for tests.
    #[allow(clippy::field_reassign_with_default)]
    pub fn for_roots(allowed_roots: Vec<PathBuf>, artifacts_dir: PathBuf) -> Self {
        let mut config = ServerConfig::default();
        config.allowed_roots = canonicalize_roots(&allowed_roots);
        config.artifacts_dir = canonicalize_root(&artifacts_dir);
        config
    }
}

fn env_usize(name: &str) -> Option<usize> {
    std::env::var(name).ok().and_then(|v| v.parse().ok())
}

fn env_u64(name: &str) -> Option<u64> {
    std::env::var(name).ok().and_then(|v| v.parse().ok())
}

/// Canonicalize a root, falling back to an absolute path.
pub fn canonicalize_root(path: &std::path::Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| {
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            std::env::current_dir()
                .unwrap_or_else(|_| PathBuf::from("."))
                .join(path)
        }
    })
}

fn canonicalize_roots(roots: &[PathBuf]) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = roots.iter().map(|r| canonicalize_root(r)).collect();
    out.dedup();
    out
}

/// What a tool name resolves to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ToolRef {
    Command(CommandName),
    Capabilities,
    Help,
    /// A server-local in-process session tool (`acos_launch`, `acos_tap`, ...).
    Session(&'static str),
}

/// The MCP server.
pub struct Server<B: Backend> {
    backend: Arc<B>,
    config: Arc<ServerConfig>,
    sessions: Mutex<SessionRegistry>,
    inflight: Mutex<HashMap<String, CancelToken>>,
    gate: Gate,
    negotiated: Mutex<String>,
    shutdown: AtomicBool,
    workers: Mutex<Vec<JoinHandle<()>>>,
}

impl<B: Backend> Server<B> {
    /// Create a server around a backend.
    pub fn new(backend: B, config: ServerConfig) -> Self {
        let gate = Gate::new(config.max_concurrency);
        Server {
            backend: Arc::new(backend),
            config: Arc::new(config),
            sessions: Mutex::new(SessionRegistry::new(Duration::from_secs(600))),
            inflight: Mutex::new(HashMap::new()),
            gate,
            negotiated: Mutex::new(LATEST_PROTOCOL_VERSION.to_string()),
            shutdown: AtomicBool::new(false),
            workers: Mutex::new(Vec::new()),
        }
    }

    /// The server configuration.
    pub fn config(&self) -> &ServerConfig {
        &self.config
    }

    /// Access the session registry (for tests and future commands).
    pub fn sessions(&self) -> &Mutex<SessionRegistry> {
        &self.sessions
    }

    /// The negotiated protocol revision.
    pub fn negotiated_version(&self) -> String {
        self.negotiated
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Serve the connection until EOF, then cancel and join in-flight calls.
    pub fn serve<R, W>(self: &Arc<Self>, mut reader: R, writer: W) -> std::io::Result<()>
    where
        R: BufRead,
        W: Write + Send + 'static,
    {
        let sink: Arc<Mutex<W>> = Arc::new(Mutex::new(writer));
        loop {
            match read_line_bounded(&mut reader, self.config.max_request_bytes)? {
                ReadOutcome::Eof => break,
                ReadOutcome::Oversized => {
                    let error = RpcError::new(
                        PARSE_ERROR,
                        format!(
                            "request frame exceeds the {} byte limit",
                            self.config.max_request_bytes
                        ),
                    );
                    write_value(&sink, &jsonrpc::error(&Value::Null, &error));
                }
                ReadOutcome::Line(bytes) => {
                    let text = String::from_utf8_lossy(&bytes).into_owned();
                    if text.trim().is_empty() {
                        continue;
                    }
                    self.handle_text(&text, Arc::clone(&sink));
                }
            }
        }
        self.shutdown_and_join();
        // SERVELEAK: after stdin EOF the process is about to go away, so the
        // sessions it created are destroyed here (and with them their guests)
        // instead of being left to whatever holds them. Nothing another process
        // created is touched.
        self.backend.shutdown();
        Ok(())
    }

    fn handle_text<W: Write + Send + 'static>(self: &Arc<Self>, text: &str, sink: Arc<Mutex<W>>) {
        match jsonrpc::parse(text) {
            Parsed::Invalid { id, error } => {
                write_value(&sink, &jsonrpc::error(&id.unwrap_or(Value::Null), &error));
            }
            Parsed::Notification { method, params } => self.handle_notification(&method, &params),
            Parsed::Request { id, method, params } => {
                if method == "tools/call" {
                    self.spawn_tool_call(id, params, sink);
                } else {
                    let response = self.handle_request(&method, &params, &id);
                    write_value(&sink, &response);
                }
            }
        }
    }

    fn spawn_tool_call<W: Write + Send + 'static>(
        self: &Arc<Self>,
        id: Value,
        params: Value,
        sink: Arc<Mutex<W>>,
    ) {
        let key = jsonrpc::id_key(&id);
        let token = CancelToken::new();
        self.inflight
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(key.clone(), token.clone());
        let this = Arc::clone(self);
        let handle = std::thread::spawn(move || {
            let response = this.run_tool_call(&id, &params, &token);
            this.inflight
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&key);
            write_value(&sink, &response);
        });
        self.workers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(handle);
    }

    fn run_tool_call(&self, id: &Value, params: &Value, token: &CancelToken) -> Value {
        let Some(name) = params.get("name").and_then(Value::as_str) else {
            return jsonrpc::error(
                id,
                &RpcError::new(INVALID_PARAMS, "tools/call requires a string `name`"),
            );
        };
        let args = params
            .get("arguments")
            .cloned()
            .unwrap_or_else(|| json!({}));
        if !args.is_object() {
            return jsonrpc::error(
                id,
                &RpcError::new(INVALID_PARAMS, "`arguments` must be an object"),
            );
        }
        let Some(tool) = lookup_tool(name) else {
            return jsonrpc::error(
                id,
                &RpcError::new(INVALID_PARAMS, format!("unknown tool `{name}`")),
            );
        };

        // ARGS: reject an unknown argument before any work starts. The schema
        // `tools/list` publishes says `additionalProperties: false`, but a
        // schema is advice and this server is the authority: an argument the
        // tool does not have must never be silently dropped, because the caller
        // then believes work happened that did not (feedback 2026-11-15:
        // `acos_observe_tree {"screenshot": true}` "succeeded" without a PNG).
        let valid_arguments = match crate::descriptors::tool_schema(name) {
            Some(schema) => crate::args::check(name, &args, &schema),
            None => None,
        };
        if let Some(envelope) = valid_arguments {
            // `args` still shapes the reply (`full`, `cursor`) on the error path:
            // REL3B's strict-argument rejection goes through REL3C's one reply
            // rule, so an invalid-argument reply is bounded and shaped exactly
            // like every other reply.
            return tool_result(
                id,
                &envelope,
                &self.negotiated_version(),
                &self.config,
                &args,
            );
        }

        let permit = match self.gate.acquire(self.config.queue_timeout, token) {
            Ok(permit) => permit,
            Err(GateError::Cancelled) => {
                let envelope = cancelled_envelope(name);
                return tool_result(
                    id,
                    &envelope,
                    &self.negotiated_version(),
                    &self.config,
                    &args,
                );
            }
            Err(GateError::Timeout) => {
                let envelope = busy_envelope(name, self.config.max_concurrency);
                return tool_result(
                    id,
                    &envelope,
                    &self.negotiated_version(),
                    &self.config,
                    &args,
                );
            }
        };

        let envelope = match tool {
            ToolRef::Command(command) => {
                let ctx = self.call_context(token, &args);
                self.backend.call(command, &args, &ctx)
            }
            ToolRef::Capabilities => {
                let refresh = args
                    .get("refresh")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                let caps = if refresh {
                    self.backend.refresh_capabilities()
                } else {
                    self.backend.capabilities()
                };
                let mut value = caps.to_json();
                // ADR-0029 item 4/6: when the caller names a session, the
                // payload shows that session's *attached* modules (the same
                // rows `acos_services` answers), not only the catalog. An
                // unknown session is reported as `session: null` with the
                // reason, never a silent catalog-only answer.
                let session_id = args
                    .pointer("/base/session_id")
                    .or_else(|| args.get("session_id"))
                    .and_then(Value::as_str);
                if let Some(session_id) = session_id {
                    let ctx = self.call_context(token, &args);
                    let request = json!({
                        "op": "list",
                        "base": {"session_id": session_id},
                    });
                    let session = self
                        .backend
                        .session_call("acos_services", &request, &ctx)
                        .and_then(|envelope| envelope.result().cloned())
                        .map(|result| {
                            json!({
                                "session_id": session_id,
                                "attached": result.get("attached").cloned().unwrap_or(Value::Null),
                                "modules": result.get("modules").cloned().unwrap_or(Value::Null),
                            })
                        });
                    value["session"] = session.unwrap_or_else(|| {
                        json!({
                            "session_id": session_id,
                            "error": "no live session with this id; launch it first (acos_launch) or omit session_id for the catalog",
                        })
                    });
                }
                pass_with_result("req_capabilities", value)
            }
            ToolRef::Help => {
                let caps = self.backend.capabilities();
                let info = self.backend.info();
                let root = self.config.allowed_roots.first().cloned();
                // `{"topic": "workflow"|"cli"|"scenarios"|"errors"|"limits"}`
                // returns one section of the embedded agent guide.
                let topic = args.get("topic").and_then(Value::as_str);
                pass_with_result(
                    "req_help",
                    help::build_help_topic(&caps, &info, root.as_deref(), topic),
                )
            }
            ToolRef::Session(name) => {
                let ctx = self.call_context(token, &args);
                self.backend
                    .session_call(name, &args, &ctx)
                    .unwrap_or_else(|| unsupported_session_tool(name))
            }
        };
        drop(permit);
        tool_result(
            id,
            &envelope,
            &self.negotiated_version(),
            &self.config,
            &args,
        )
    }

    fn call_context(&self, token: &CancelToken, args: &Value) -> CallContext {
        let budget = args
            .pointer("/base/budget")
            .and_then(|value| serde_json::from_value::<ResourceBudget>(value.clone()).ok())
            .unwrap_or_default();
        let wall = budget
            .wall_ms
            .map(Duration::from_millis)
            .unwrap_or(self.config.default_call_timeout);
        CallContext {
            cancel: token.clone(),
            deadline: Instant::now() + wall,
            budget,
            allowed_roots: self.config.allowed_roots.clone(),
            artifacts_dir: self.config.artifacts_dir.clone(),
            max_arg_chars: self.config.max_arg_chars,
            max_output_bytes: self.config.max_output_bytes,
        }
    }

    fn handle_request(&self, method: &str, params: &Value, id: &Value) -> Value {
        match method {
            "initialize" => {
                // VIEW1a: remember who is driving, so the live-view registry
                // entry for this agent's sessions names the real client (T3
                // Code, Claude Code, …) instead of the server.
                if let Some(client) = params.get("clientInfo") {
                    let name = client
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or(crate::SERVER_NAME);
                    let version = client.get("version").and_then(Value::as_str).unwrap_or("");
                    crate::private_daemon::set_view_client(name, version);
                }
                let requested = params
                    .get("protocolVersion")
                    .and_then(Value::as_str)
                    .unwrap_or(LATEST_PROTOCOL_VERSION);
                let negotiated = negotiate(requested);
                *self.negotiated.lock().unwrap_or_else(|e| e.into_inner()) = negotiated.clone();
                jsonrpc::result(
                    id,
                    json!({
                        "protocolVersion": negotiated,
                        // Tools, plus the agent guide as resources
                        // (`acos://guide`, `acos://guide/<topic>`,
                        // `acos://errors`) and the `verify_android_flow`
                        // prompt. Nothing here is a capability the server
                        // does not implement below.
                        "capabilities": {
                            "tools": {"listChanged": false},
                            "resources": {"subscribe": false, "listChanged": false},
                            "prompts": {"listChanged": false}
                        },
                        "serverInfo": {"name": SERVER_NAME, "version": SERVER_VERSION},
                        "instructions": INSTRUCTIONS,
                    }),
                )
            }
            "ping" => jsonrpc::result(id, json!({})),
            "tools/list" => {
                let caps = self.backend.capabilities();
                let version = self.negotiated_version();
                // F12: only the tools this build can run. Unavailable protocol
                // commands stay reserved (a call still returns a structured
                // `unsupported`), they are just not advertised.
                let tools: Vec<Value> = crate::descriptors::available_tool_infos(&caps)
                    .iter()
                    .map(|tool| tool.to_json(&version))
                    .collect();
                jsonrpc::result(id, json!({ "tools": tools }))
            }
            "resources/list" => jsonrpc::result(id, json!({ "resources": resources() })),
            "resources/read" => {
                let uri = params.get("uri").and_then(Value::as_str).unwrap_or("");
                let caps = self.backend.capabilities();
                match read_resource(uri, &caps) {
                    Some(contents) => jsonrpc::result(id, json!({ "contents": contents })),
                    None => jsonrpc::error(
                        id,
                        &RpcError::new(INVALID_PARAMS, format!("unknown resource uri `{uri}`"))
                            .with_data(json!({ "available": resource_uris() })),
                    ),
                }
            }
            "resources/templates/list" => {
                jsonrpc::result(id, json!({ "resourceTemplates": resource_templates() }))
            }
            "prompts/list" => jsonrpc::result(id, json!({ "prompts": prompts() })),
            "prompts/get" => {
                let name = params.get("name").and_then(Value::as_str).unwrap_or("");
                let args = params
                    .get("arguments")
                    .cloned()
                    .unwrap_or_else(|| json!({}));
                match prompt(name, &args) {
                    Ok(result) => jsonrpc::result(id, result),
                    Err(error) => jsonrpc::error(id, &error),
                }
            }
            "logging/setLevel" => jsonrpc::result(id, json!({})),
            _ => jsonrpc::error(
                id,
                &RpcError::new(
                    METHOD_NOT_FOUND,
                    format!("method `{method}` is not supported"),
                ),
            ),
        }
    }

    fn handle_notification(&self, method: &str, params: &Value) {
        if method == "notifications/cancelled" {
            if let Some(request_id) = params.get("requestId") {
                let key = jsonrpc::id_key(request_id);
                if let Some(token) = self
                    .inflight
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .get(&key)
                {
                    token.cancel();
                }
            }
        }
    }

    fn shutdown_and_join(&self) {
        self.shutdown.store(true, Ordering::SeqCst);

        // Let in-flight calls finish for a bounded grace period so a client that
        // closes stdin right after sending requests still receives responses.
        let deadline = Instant::now() + self.config.shutdown_grace;
        loop {
            let all_finished = self
                .workers
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .iter()
                .all(JoinHandle::is_finished);
            if all_finished || Instant::now() >= deadline {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }

        let tokens: Vec<CancelToken> = self
            .inflight
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .cloned()
            .collect();
        for token in tokens {
            token.cancel();
        }
        let handles: Vec<JoinHandle<()>> =
            std::mem::take(&mut *self.workers.lock().unwrap_or_else(|e| e.into_inner()));
        for handle in handles {
            let _ = handle.join();
        }
    }
}

fn lookup_tool(name: &str) -> Option<ToolRef> {
    if name == CAPABILITIES_TOOL {
        return Some(ToolRef::Capabilities);
    }
    if name == HELP_TOOL {
        return Some(ToolRef::Help);
    }
    if let Some(session) = SESSION_TOOL_NAMES.iter().find(|tool| **tool == name) {
        return Some(ToolRef::Session(session));
    }
    command_from_tool(name).map(ToolRef::Command)
}

fn unsupported_session_tool(name: &str) -> Envelope {
    Envelope::failure(
        "req_session",
        ProtocolBackend::AcosNative,
        Fidelity::NativeRuntime,
        ProtocolError::new(
            ErrorCode::UnsupportedBackend,
            format!("session tool `{name}` requires the in-process session backend"),
        )
        .with_hint("Run the default `acos-mcp` binary, which installs SessionBackend; the bare CliBackend does not hold sessions.")
        .with_details(json!({"tool": name})),
        Metrics::wall_ms(0),
    )
}

fn pass_with_result(request_id: &str, result: Value) -> Envelope {
    Envelope::pass(
        request_id,
        ProtocolBackend::AcosNative,
        Fidelity::StaticAnalysis,
        Metrics::wall_ms(0),
    )
    .with_value_result(result)
}

fn cancelled_envelope(tool: &str) -> Envelope {
    Envelope::failure(
        "req_cancelled",
        ProtocolBackend::AcosNative,
        Fidelity::StaticAnalysis,
        ProtocolError::new(
            ErrorCode::Internal,
            format!("tool `{tool}` was cancelled before it started"),
        )
        .with_details(json!({"cancelled": true})),
        Metrics::wall_ms(0),
    )
}

fn busy_envelope(tool: &str, limit: usize) -> Envelope {
    Envelope::failure(
        "req_busy",
        ProtocolBackend::AcosNative,
        Fidelity::StaticAnalysis,
        ProtocolError::new(
            ErrorCode::ResourceLimit,
            format!("tool `{tool}` waited too long for a concurrency slot (limit {limit})"),
        )
        .with_details(json!({"concurrency_limit": limit})),
        Metrics::wall_ms(0),
    )
}

/// Compact a tree-bearing result and write the full tree to a per-session file.
fn shape_reply_tree(mut value: Value, config: &ServerConfig, args: &Value) -> Value {
    let full = args.get("full").and_then(Value::as_bool).unwrap_or(false)
        || args
            .get("full_tree")
            .and_then(Value::as_bool)
            .unwrap_or(false);
    let cursor = args.get("cursor").and_then(Value::as_u64).unwrap_or(0) as usize;
    let session = value
        .get("session_id")
        .and_then(Value::as_str)
        .or_else(|| value.pointer("/result/session_id").and_then(Value::as_str))
        .unwrap_or("session");
    let safe: String = session
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let dir = config
        .artifacts_dir
        .join("sessions")
        .join(if safe.is_empty() {
            "session".to_string()
        } else {
            safe
        });
    let opts = TreeShapeOptions {
        cursor,
        page_size: reply::DEFAULT_PAGE_SIZE,
        full,
        artifacts_dir: Some(dir),
        session: Some(session.to_string()),
    };
    reply::shape_result_tree(&mut value, None, &opts);
    value
}

fn tool_result(
    id: &Value,
    envelope: &Envelope,
    protocol_version: &str,
    config: &ServerConfig,
    args: &Value,
) -> Value {
    let envelope_json = serde_json::to_value(envelope).unwrap_or(Value::Null);
    // REL3C: one reply rule for every tool. A tree is compacted (visible
    // text/content_desc nodes, in reading order, paged with `cursor`) and the
    // full tree is written under the per-session artifacts directory; the
    // daemon already does this for live ART sessions, and this is the same rule
    // for the in-process/native/RN/Flutter routes. `full: true` keeps the old
    // inline tree when it fits the hard cap.
    let envelope_json = shape_reply_tree(envelope_json, config, args);
    // F11: an agent pays for every byte of a reply and reads only the head and
    // the tail of a stack. Cap long strings here, once, for every tool, so the
    // CLI's own `--json` output and the artifacts on disk stay complete while
    // the MCP reply stays small.
    let (mut envelope_json, bounds) =
        crate::bound::bound_value(&envelope_json, config.max_text_bytes);
    // The hard cap is a whole-reply rule, not a per-string one: a tree made of
    // 500 small strings passed the string bound and still overflowed an agent's
    // context window. `ACOS_MCP_MAX_TEXT=0` remains the documented "no bound at
    // all" escape hatch (the raw CLI behaviour).
    // `acos_help` is the one request whose purpose is the text: its guide is
    // exempt from both bounds (the existing F11 rule), and a cap cannot turn a
    // deliberate `topic: "all"` into a pointer object.
    let help_text = envelope_json.pointer("/result/guide").is_some()
        || envelope_json.pointer("/result/markdown").is_some();
    let reduced = if config.max_text_bytes == 0 || help_text {
        0
    } else {
        reply::enforce_cap(&mut envelope_json, reply::MAX_REPLY_BYTES)
    };
    let text = serde_json::to_string(&envelope_json).unwrap_or_default();
    let mut content = vec![json!({"type": "text", "text": text})];
    append_evidence_content(&mut content, envelope, config, protocol_version);
    // FB2: a live-session screenshot is a *result*, not an evidence file; inline
    // its bounded preview too (the lossless path is in the reply above).
    append_screenshot_content(
        &mut content,
        envelope,
        envelope.evidence().and_then(|e| e.screenshot.as_deref()),
        config,
        protocol_version,
    );
    let mut result = json!({
        "content": content,
        "isError": !envelope.status().is_pass(),
    });
    if !bounds.is_empty() || reduced > 0 {
        // Say that the reply was bounded, and how: a silent truncation would
        // read as the real text.
        let mut meta = json!({"reply_bounded": bounds.to_json()});
        if reduced > 0 {
            meta["reply_capped"] = json!({
                "cap_bytes": reply::MAX_REPLY_BYTES,
                "removed_bytes": reduced,
                "note": "the full evidence is named by tree_path/artifacts_dir in the result",
            });
        }
        result["_meta"] = json!({"acos_mcp": meta});
    }
    if supports_structured_content(protocol_version) {
        result["structuredContent"] = envelope_json;
    }
    jsonrpc::result(id, result)
}

fn append_evidence_content(
    content: &mut Vec<Value>,
    envelope: &Envelope,
    config: &ServerConfig,
    protocol_version: &str,
) {
    let Some(evidence) = envelope.evidence() else {
        return;
    };
    let entries = [
        ("screenshot", evidence.screenshot.as_deref()),
        ("tree", evidence.tree.as_deref()),
        ("events", evidence.events.as_deref()),
        ("log", evidence.log.as_deref()),
        ("verdict", evidence.verdict.as_deref()),
    ];
    for (kind, path) in entries {
        let Some(path) = path else { continue };
        // Evidence paths are relative and must not escape the artifacts dir.
        if std::path::Path::new(path)
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
        {
            continue;
        }
        let full = config.artifacts_dir.join(path);
        // CWE-59 / CWE-367: open once and validate the opened descriptor's real
        // path against `allowed_roots`, so a symlink under `artifacts_dir`
        // pointing outside the allowed roots is neither inlined nor linked.
        let Some((file, len)) = open_file_within_allowed_roots(&full, &config.allowed_roots) else {
            continue;
        };
        // SHOTZ: an image is inlined as a *bounded preview* (long side
        // `ACOS_MCP_PREVIEW_MAX_SIDE`, file `ACOS_MCP_MAX_IMAGE_BYTES`) and the
        // reply still names the lossless file below, because a full-frame PNG in
        // a tool reply is context spent on pixels the agent cannot read.
        if image_mime(path).is_some() {
            if let Some(bytes) = read_opened_preview_bytes(file, len) {
                if let Some(preview) = crate::preview::image_preview_from_bytes(
                    &full,
                    &bytes,
                    config.max_image_side,
                    config.max_image_bytes,
                ) {
                    content.push(json!({
                        "type": "image",
                        "data": crate::base64::encode(&preview.data),
                        "mimeType": preview.mime,
                    }));
                    content.push(json!({
                        "type": "text",
                        "text": preview_note(kind, &full, &preview),
                    }));
                }
            }
        }
        if supports_structured_content(protocol_version) {
            content.push(json!({
                "type": "resource_link",
                "uri": format!("file://{}", full.display()),
                "name": kind,
            }));
        } else if image_mime(path).is_none() {
            // An image already carried its own line (`preview_note`), which
            // names the lossless path; a second bare path would be noise.
            content.push(json!({
                "type": "text",
                "text": format!("{kind}: {}", full.display()),
            }));
        }
    }
}

/// FB2: inline the bounded preview of a screenshot a session verb wrote.
///
/// `acos_observe_tree {"screenshot": true}` (and `acos_observe`) answer with the
/// absolute path of a lossless PNG plus `display_origin`; without this the agent
/// would have to read the file itself to see the frame. The image is inlined
/// under the same SHOTZ bound as an evidence image (long side
/// `ACOS_MCP_PREVIEW_MAX_SIDE`, bytes `ACOS_MCP_MAX_IMAGE_BYTES`) and the reply
/// still names the lossless file.
fn append_screenshot_content(
    content: &mut Vec<Value>,
    envelope: &Envelope,
    evidence_path: Option<&str>,
    config: &ServerConfig,
    protocol_version: &str,
) {
    let Some(path) = envelope
        .result()
        .and_then(|result| result.get("screenshot"))
        .and_then(|screenshot| screenshot.get("path"))
        .and_then(Value::as_str)
    else {
        return;
    };
    // An evidence screenshot already carries the same file: never inline twice.
    if evidence_path == Some(path) {
        return;
    }
    let full = Path::new(path);
    if !full.is_file() || image_mime(path).is_none() {
        return;
    }
    // FB2c item 5: a session verb writes inside its artifacts dir, but the path
    // in the reply is data, not a trust anchor. Inline a file only when it lives
    // under one of this server's allowed roots (`canonicalize` resolves symlinks,
    // so a link pointing outside is refused too).
    let mut roots = config.allowed_roots.clone();
    // The session daemon writes a session's screenshots under its own private
    // runtime directory (`<runtime>/art-sessions/<id>/screenshots/`, or the
    // operator's `ACOS_SERVE_ARTIFACTS`): that tree is the default destination,
    // so it is a root too — and nothing else outside the allowed roots is.
    roots.push(crate::daemon::runtime_dir().join("art-sessions"));
    if let Some(dir) = std::env::var_os("ACOS_SERVE_ARTIFACTS") {
        roots.push(PathBuf::from(dir));
    }
    roots.push(std::env::temp_dir());
    // CWE-367: open once, validate the *opened* file, read from that handle.
    let Some(bytes) = read_file_within_allowed_roots(full, &roots) else {
        return;
    };
    if let Some(preview) = crate::preview::image_preview_from_bytes(
        full,
        &bytes,
        config.max_image_side,
        config.max_image_bytes,
    ) {
        content.push(json!({
            "type": "image",
            "data": crate::base64::encode(&preview.data),
            "mimeType": preview.mime,
        }));
        content.push(json!({
            "type": "text",
            "text": preview_note("screenshot", full, &preview),
        }));
    }
    if supports_structured_content(protocol_version) {
        content.push(json!({
            "type": "resource_link",
            "uri": format!("file://{}", full.display()),
            "name": "screenshot",
        }));
    }
}

/// Largest file [`read_file_within_allowed_roots`] reads (a preview source is
/// a PNG frame; anything bigger is not inlined).
const MAX_PREVIEW_SOURCE_BYTES: u64 = 64 * 1024 * 1024;

/// FB2c / CWE-59 / CWE-367: open `path` **once** and verify via `fstat` and
/// `/proc/self/fd/<fd>` that the opened descriptor refers to a regular file
/// inside one of `roots` (canonicalized so a root configured through a symlink
/// still matches).
fn open_file_within_allowed_roots(path: &Path, roots: &[PathBuf]) -> Option<(std::fs::File, u64)> {
    use std::os::unix::io::AsRawFd;
    let file = std::fs::File::open(path).ok()?;
    let meta = file.metadata().ok()?;
    if !meta.is_file() {
        return None;
    }
    let real = std::fs::read_link(format!("/proc/self/fd/{}", file.as_raw_fd())).ok()?;
    let inside = roots.iter().any(|root| {
        let root = std::fs::canonicalize(root).unwrap_or_else(|_| root.clone());
        real.starts_with(&root)
    });
    if !inside {
        return None;
    }
    Some((file, meta.len()))
}

/// Read up to [`MAX_PREVIEW_SOURCE_BYTES`] from an already-validated file
/// descriptor.
fn read_opened_preview_bytes(file: std::fs::File, len: u64) -> Option<Vec<u8>> {
    use std::io::Read as _;
    if len > MAX_PREVIEW_SOURCE_BYTES {
        return None;
    }
    let mut bytes = Vec::with_capacity(len as usize);
    file.take(MAX_PREVIEW_SOURCE_BYTES)
        .read_to_end(&mut bytes)
        .ok()?;
    Some(bytes)
}

/// FB2c / CWE-367: read `path` only if the file actually opened is a regular
/// file inside one of the server's allowed roots.
fn read_file_within_allowed_roots(path: &Path, roots: &[PathBuf]) -> Option<Vec<u8>> {
    let (file, len) = open_file_within_allowed_roots(path, roots)?;
    read_opened_preview_bytes(file, len)
}

/// One line describing the inline image: which file it came from, whether it is
/// downscaled, and the pixel size of both.
fn preview_note(kind: &str, full: &Path, preview: &crate::preview::Preview) -> String {
    let scale = if preview.scaled {
        format!(
            "downscaled preview of {}x{}",
            preview.source_width, preview.source_height
        )
    } else {
        "full-size preview".to_string()
    };
    format!(
        "{kind}: {}x{} {scale}; lossless file: {} ({} bytes)",
        preview.width,
        preview.height,
        full.display(),
        preview.source_bytes
    )
}

fn image_mime(path: &str) -> Option<&'static str> {
    let lower = path.to_ascii_lowercase();
    if lower.ends_with(".png") {
        Some("image/png")
    } else if lower.ends_with(".jpg") || lower.ends_with(".jpeg") {
        Some("image/jpeg")
    } else if lower.ends_with(".webp") {
        Some("image/webp")
    } else {
        None
    }
}

fn write_value<W: Write>(sink: &Mutex<W>, value: &Value) {
    let mut guard = sink.lock().unwrap_or_else(|e| e.into_inner());
    if serde_json::to_writer(&mut *guard, value).is_ok() {
        let _ = guard.write_all(b"\n");
        let _ = guard.flush();
    }
}

// ---- resources and prompts: the agent guide as first-class MCP objects ------
//
// The guide lives in `docs/AGENT_GUIDE.md`, is embedded at build time
// (`help::AGENT_GUIDE`) and is served three ways: the `acos_help` tool, these
// resources, and the `verify_android_flow` prompt. One text, three access
// paths — nothing here paraphrases it.

/// Every resource URI this server serves.
pub fn resource_uris() -> Vec<&'static str> {
    let mut uris = vec!["acos://guide", "acos://errors"];
    uris.extend(
        help::topic_list()
            .iter()
            .filter(|topic| **topic != "errors")
            .map(|topic| match *topic {
                "overview" => "acos://guide/overview",
                "mcp" => "acos://guide/mcp",
                "workflow" => "acos://guide/workflow",
                "cli" => "acos://guide/cli",
                "scenarios" => "acos://guide/scenarios",
                "limits" => "acos://guide/limits",
                _ => "acos://guide",
            }),
    );
    uris.sort_unstable();
    uris.dedup();
    uris
}

/// `resources/list`: one markdown resource per guide section.
pub fn resources() -> Vec<Value> {
    let mut list = vec![json!({
        "uri": "acos://guide",
        "name": "agent-guide",
        "title": "ACOS agent guide (complete)",
        "description": "How to use ACOS from an agent: the loop, every CLI command with an example, scenario YAML, settle rules, error codes and limits.",
        "mimeType": "text/markdown",
    })];
    // `acos://errors` is the error section under its own, memorable name.
    list.push(json!({
        "uri": "acos://errors",
        "name": "agent-guide-errors",
        "title": "ACOS error codes -> what to do",
        "description": "Every error code the engine can return and the action it demands.",
        "mimeType": "text/markdown",
    }));
    for topic in help::topic_list() {
        list.push(json!({
            "uri": format!("acos://guide/{topic}"),
            "name": format!("agent-guide-{topic}"),
            "title": format!("ACOS agent guide: {topic}"),
            "description": match topic {
                "overview" => "The three status rules, the loop in ten lines and the topic list (the `acos_help` default).",
                "mcp" => "Every advertised MCP tool with its arguments, one example call and what it returns (generated from the tool descriptors).",
                "workflow" => "The doctor -> inspect -> launch -> observe -> act -> verify loop.",
                "cli" => "Every `acos` command with a runnable example.",
                "scenarios" => "How to write a scenario YAML for `acos verify` (steps, selectors, settle, assertions).",
                "errors" => "Every error code and the action it demands.",
                "limits" => "What the engine does not support and the host admission rules.",
                _ => "ACOS agent guide section.",
            },
            "mimeType": "text/markdown",
        }));
    }
    list
}

/// `resources/templates/list`: the one dynamic shape (`acos://guide/{topic}`).
pub fn resource_templates() -> Vec<Value> {
    vec![json!({
        "uriTemplate": "acos://guide/{topic}",
        "name": "agent-guide-topic",
        "title": "ACOS agent guide, one topic",
        "description": "One section of the agent guide.",
        "mimeType": "text/markdown",
    })]
}

/// `resources/read`: the contents for a URI, or `None` when it is not ours.
///
/// The capability snapshot is threaded through because the generated `mcp`
/// topic must list exactly the tools `tools/list` advertises for this build.
pub fn read_resource(uri: &str, caps: &Capabilities) -> Option<Vec<Value>> {
    // `acos://guide` is the **complete** guide (a resource read is a deliberate,
    // unlimited fetch); the `acos_help` tool defaults to the skeleton instead.
    let topic = match uri {
        "acos://guide" => Some(help::ALL_TOPIC),
        // `acos://errors` is the error section under its own name.
        "acos://errors" => Some("errors"),
        other => {
            let name = other.strip_prefix("acos://guide/")?;
            Some(help::topic_list().into_iter().find(|t| *t == name)?)
        }
    };
    Some(vec![json!({
        "uri": uri,
        "name": topic.map_or("agent-guide".to_string(), |t| format!("agent-guide-{t}")),
        "title": if topic == Some(help::ALL_TOPIC) {
            "ACOS agent guide (complete)".to_string()
        } else {
            topic.map_or("ACOS agent guide".to_string(), |t| format!("ACOS agent guide: {t}"))
        },
        "mimeType": "text/markdown",
        "text": match topic {
            Some(topic) => help::guide_with(Some(topic), &crate::descriptors::tool_infos(caps)),
            None => help::guide(None),
        },
    })])
}

/// `prompts/list`.
pub fn prompts() -> Vec<Value> {
    vec![json!({
        "name": "verify_android_flow",
        "title": "Verify an Android flow with ACOS",
        "description": "Produce a scenario YAML for the APK and goal, run `acos verify`, read the verdict and report what is proven (and what is not).",
        "arguments": [
            {"name": "apk", "description": "Path to the APK under test (inside ACOS_ALLOWED_ROOTS).", "required": true},
            {"name": "goal", "description": "The user-visible flow to prove, e.g. `tap Increment and the count reads 1`.", "required": true},
        ],
    })]
}

/// `prompts/get`: the prompt body for `verify_android_flow`.
pub fn prompt(name: &str, args: &Value) -> Result<Value, RpcError> {
    if name != "verify_android_flow" {
        return Err(
            RpcError::new(INVALID_PARAMS, format!("unknown prompt `{name}`"))
                .with_data(json!({"available": ["verify_android_flow"]})),
        );
    }
    let apk = args.get("apk").and_then(Value::as_str).unwrap_or("").trim();
    let goal = args
        .get("goal")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    if apk.is_empty() || goal.is_empty() {
        return Err(RpcError::new(
            INVALID_PARAMS,
            "verify_android_flow requires both `apk` and `goal`",
        ));
    }
    let text = format!(
        "Verify this Android flow on real ART with ACOS.\n\n         APK: {apk}\n         Flow to prove: {goal}\n\n         1. `acos_help` with topic `scenarios` for the scenario YAML format (one-key step maps, selectors, \n            settle modes, `expect_change`, assertion kinds). Call `acos_inspect` first for the package and \n            launcher activity.\n         2. Write the scenario: `launch` (with the real activity), `wait_for` the first node you will assert, \n            then `assert_text` for the initial state, the input step with `expect_change: true`, and \n            `assert_text`/`assert_log` for the result. Assertions are strict: never ask for `settle: fast` on \n            an assertion, and do not add sleeps.\n         3. Run it: `acos_verify` with `apk`, `scenario` and an artifacts directory (or `acos verify \n            <scenario.yaml> --artifacts out/<name> --json`).\n         4. Read `verdict.json` and `steps/NN-*.json`. Branch on `error.code`: `fail` means the app is wrong, \n            `unsupported`/`blocked` means ACOS cannot do it (report it, never a pass), `HOST_MEMORY_LOW` means \n            the host had no room and nothing ran.\n         5. Report what the evidence proves, quote the failing step and its code, and state plainly what was \n            not verified."
    );
    Ok(json!({
        "description": format!("Verify `{goal}` in `{apk}` with an ACOS scenario"),
        "messages": [{
            "role": "user",
            "content": {"type": "text", "text": text},
        }],
    }))
}

/// Convenience: the capability snapshot of a server (used by tests).
pub fn capabilities_of<B: Backend>(server: &Server<B>) -> Capabilities {
    server.backend.capabilities()
}

#[cfg(test)]
mod tests {
    use super::*;
    use acos_protocol::Status;

    /// FB2c item 5: the MCP server inlines an image only when it lives under one
    /// of its allowed roots; a symlink out of a root and a file elsewhere are refused.
    #[test]
    fn a_screenshot_is_inlined_only_from_an_allowed_root() {
        let dir = std::env::temp_dir().join(format!("acos-roots-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let root = dir.join("root");
        let elsewhere = dir.join("elsewhere");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&elsewhere).unwrap();
        let inside = root.join("a.png");
        let outside = elsewhere.join("b.png");
        std::fs::write(&inside, b"x").unwrap();
        std::fs::write(&outside, b"x").unwrap();
        let link = root.join("link.png");
        std::os::unix::fs::symlink(&outside, &link).unwrap();
        let roots = vec![root.clone()];
        assert_eq!(
            read_file_within_allowed_roots(&inside, &roots),
            Some(b"x".to_vec())
        );
        assert!(read_file_within_allowed_roots(&outside, &roots).is_none());
        assert!(
            read_file_within_allowed_roots(&link, &roots).is_none(),
            "a symlink out of the root"
        );
        assert!(read_file_within_allowed_roots(&root.join("missing.png"), &roots).is_none());
        assert!(
            read_file_within_allowed_roots(&root, &roots).is_none(),
            "a directory is not a regular file"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    struct NullBackend;

    impl Backend for NullBackend {
        fn call(&self, _command: CommandName, _args: &Value, _ctx: &CallContext) -> Envelope {
            Envelope::failure(
                "req",
                ProtocolBackend::AcosNative,
                Fidelity::StaticAnalysis,
                ProtocolError::new(ErrorCode::Internal, "null backend"),
                Metrics::wall_ms(0),
            )
        }
        fn capabilities(&self) -> Capabilities {
            Capabilities::unprobed()
        }
        fn refresh_capabilities(&self) -> Capabilities {
            Capabilities::unprobed()
        }
        fn info(&self) -> crate::backend::BackendInfo {
            crate::backend::BackendInfo {
                binary: None,
                version: None,
            }
        }
    }

    #[test]
    fn tool_lookup_resolves_meta_and_commands() {
        assert_eq!(
            lookup_tool("acos_capabilities"),
            Some(ToolRef::Capabilities)
        );
        assert_eq!(lookup_tool("acos_help"), Some(ToolRef::Help));
        assert_eq!(
            lookup_tool("acos_run_dex"),
            Some(ToolRef::Command(CommandName::RunDex))
        );
        assert_eq!(lookup_tool("nope"), None);
    }

    #[test]
    fn busy_envelope_is_blocked_not_pass() {
        let envelope = busy_envelope("acos_verify", 1);
        assert!(!envelope.ok());
        assert_eq!(envelope.status(), Status::Blocked);
        assert_eq!(envelope.error().unwrap().code, ErrorCode::ResourceLimit);
    }

    #[test]
    fn server_constructs_with_defaults() {
        let server = Server::new(NullBackend, ServerConfig::default());
        assert_eq!(server.negotiated_version(), LATEST_PROTOCOL_VERSION);
        assert!(!capabilities_of(&server).probed);
    }

    /// A 32x24 PNG: the smallest thing the preview path will inline.
    fn tiny_png() -> Vec<u8> {
        let mut rgba = vec![0u8; 32 * 24 * 4];
        for (i, byte) in rgba.iter_mut().enumerate() {
            *byte = (i % 251) as u8;
        }
        let mut out = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut out, 32, 24);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().unwrap();
            writer.write_image_data(&rgba).unwrap();
            writer.finish().unwrap();
        }
        out
    }

    /// FB2: `acos_observe_tree {"screenshot": true}` answers with the lossless
    /// PNG's path in `result.screenshot`; the MCP reply inlines its *bounded*
    /// preview (SHOTZ cap) and still links the lossless file, so an agent sees
    /// the frame without a second call and without a multi-MB blob.
    #[test]
    fn a_result_screenshot_is_inlined_as_a_bounded_preview() {
        let dir = std::env::temp_dir().join(format!("acos-mcp-shot-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("screenshots")).unwrap();
        let png = dir.join("screenshots/observe_tree-7.png");
        std::fs::write(&png, tiny_png()).unwrap();
        let config = ServerConfig::for_roots(vec![dir.clone()], dir.clone());
        let envelope = Envelope::pass(
            "req",
            ProtocolBackend::AcosNative,
            Fidelity::ArtReference,
            Metrics::wall_ms(2),
        )
        .with_value_result(json!({
            "node_count": 9,
            "screenshot": {
                "path": png.display().to_string(),
                "bytes": std::fs::metadata(&png).unwrap().len(),
                "format": "png",
                "frame_seq": 7,
                "display_origin": [0, 128],
            },
        }));
        let modern = tool_result(&json!(11), &envelope, "2025-06-18", &config, &json!({}));
        let content = modern["result"]["content"].as_array().unwrap();
        assert!(
            content
                .iter()
                .any(|c| c["type"] == "image" && c["mimeType"] == "image/png"),
            "the screenshot is inlined as an image: {content:?}"
        );
        assert!(
            content
                .iter()
                .any(|c| c["type"] == "text"
                    && c["text"].as_str().unwrap().contains("lossless file:")),
            "the inline image names the lossless file: {content:?}"
        );
        let links: Vec<&str> = content
            .iter()
            .filter(|c| c["type"] == "resource_link")
            .filter_map(|link| link["uri"].as_str())
            .collect();
        assert!(
            links.iter().any(|uri| uri.ends_with("observe_tree-7.png")),
            "the lossless screenshot stays linked: {links:?}"
        );
        // No `screenshot` in the result, no extra content: nothing is invented.
        let plain = Envelope::pass(
            "req",
            ProtocolBackend::AcosNative,
            Fidelity::ArtReference,
            Metrics::wall_ms(1),
        )
        .with_value_result(json!({"node_count": 9}));
        let plain = tool_result(&json!(12), &plain, "2025-06-18", &config, &json!({}));
        assert_eq!(plain["result"]["content"].as_array().unwrap().len(), 1);

        // CWE-367 regression: a `screenshot.path` that is a symlink to a PNG
        // outside every allowed root (the OS temp dir included) is never
        // inlined -- the opened file's real path is what is checked.
        let outside_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../.cache/mcp-preview-outside")
            .join(std::process::id().to_string());
        std::fs::create_dir_all(&outside_dir).unwrap();
        let outside_dir = std::fs::canonicalize(&outside_dir).unwrap();
        if !outside_dir.starts_with(std::fs::canonicalize(std::env::temp_dir()).unwrap()) {
            let secret = outside_dir.join("secret.png");
            std::fs::write(&secret, tiny_png()).unwrap();
            let link = dir.join("screenshots/link-8.png");
            std::os::unix::fs::symlink(&secret, &link).unwrap();
            let linked = Envelope::pass(
                "req",
                ProtocolBackend::AcosNative,
                Fidelity::ArtReference,
                Metrics::wall_ms(1),
            )
            .with_value_result(json!({
                "screenshot": {"path": link.display().to_string(), "format": "png"},
            }));
            let linked = tool_result(&json!(13), &linked, "2025-06-18", &config, &json!({}));
            let content = linked["result"]["content"].as_array().unwrap();
            assert!(
                !content.iter().any(|c| c["type"] == "image"),
                "a symlink out of the allowed roots is never inlined: {content:?}"
            );
        }
        let _ = std::fs::remove_dir_all(&outside_dir);
        // A path that does not exist is never inlined or linked.
        let missing = Envelope::pass(
            "req",
            ProtocolBackend::AcosNative,
            Fidelity::ArtReference,
            Metrics::wall_ms(1),
        )
        .with_value_result(
            json!({"screenshot": {"path": dir.join("nope.png").display().to_string()}}),
        );
        let missing = tool_result(&json!(13), &missing, "2025-06-18", &config, &json!({}));
        assert_eq!(missing["result"]["content"].as_array().unwrap().len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn evidence_becomes_image_and_resource_link() {
        use acos_protocol::Evidence;
        let dir = std::env::temp_dir().join(format!("acos-mcp-evidence-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("frames")).unwrap();
        // SHOTZ: an image is inlined only when it decodes (a preview needs real
        // pixels) — a file that merely ends in `.png` is linked, never inlined.
        std::fs::write(dir.join("frames/1.png"), tiny_png()).unwrap();
        std::fs::write(dir.join("events.jsonl"), b"{}\n").unwrap();
        let config = ServerConfig::for_roots(vec![dir.clone()], dir.clone());

        let envelope = Envelope::pass(
            "req",
            ProtocolBackend::AcosNative,
            Fidelity::NativeRuntime,
            Metrics::wall_ms(1),
        )
        .with_evidence(Evidence {
            screenshot: Some("frames/1.png".to_string()),
            events: Some("events.jsonl".to_string()),
            ..Evidence::default()
        });

        let modern = tool_result(&json!(7), &envelope, "2025-06-18", &config, &json!({}));
        let content = modern["result"]["content"].as_array().unwrap();
        assert!(content
            .iter()
            .any(|c| c["type"] == "image" && c["mimeType"] == "image/png"));
        // Every evidence file gets a link — including the image, whose inline
        // copy is only a preview (SHOTZ): the lossless path must stay reachable.
        let links: Vec<&str> = content
            .iter()
            .filter(|c| c["type"] == "resource_link")
            .filter_map(|link| link["uri"].as_str())
            .collect();
        assert!(
            links.iter().any(|uri| uri.ends_with("events.jsonl")),
            "the events file needs a link: {links:?}"
        );
        assert!(
            links.iter().any(|uri| uri.ends_with("frames/1.png")),
            "the lossless frame needs a link: {links:?}"
        );
        // The inline image is described in text too (size, scale, path).
        assert!(
            content
                .iter()
                .any(|c| c["type"] == "text"
                    && c["text"].as_str().unwrap().contains("lossless file:"))
        );
        assert!(modern.pointer("/result/structuredContent").is_some());

        let legacy = tool_result(&json!(8), &envelope, "2025-03-26", &config, &json!({}));
        assert!(legacy.pointer("/result/structuredContent").is_none());
        assert!(legacy["result"]["content"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["type"] == "text" && c["text"].as_str().unwrap().contains("events.jsonl")));

        // A `..` escape in evidence must not be followed.
        let escape = Envelope::pass(
            "req",
            ProtocolBackend::AcosNative,
            Fidelity::NativeRuntime,
            Metrics::wall_ms(1),
        )
        .with_evidence(Evidence {
            screenshot: Some("../etc/passwd".to_string()),
            ..Evidence::default()
        });
        let guarded = tool_result(&json!(9), &escape, "2025-03-26", &config, &json!({}));
        assert_eq!(guarded["result"]["content"].as_array().unwrap().len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// CWE-59: a symlink inside `<artifacts_dir>/frames/` pointing outside
    /// `allowed_roots` must neither be inlined as an image nor linked.
    #[test]
    fn evidence_symlink_outside_allowed_roots_is_not_inlined() {
        use acos_protocol::Evidence;
        let base = std::env::temp_dir().join(format!(
            "acos-mcp-evidence-symlink-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&base);
        let artifacts = base.join("artifacts");
        let outside = base.join("outside");
        std::fs::create_dir_all(artifacts.join("frames")).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        let secret = outside.join("secret.png");
        std::fs::write(&secret, tiny_png()).unwrap();
        std::os::unix::fs::symlink(&secret, artifacts.join("frames/escape.png")).unwrap();

        let config = ServerConfig::for_roots(vec![artifacts.clone()], artifacts.clone());
        let envelope = Envelope::pass(
            "req",
            ProtocolBackend::AcosNative,
            Fidelity::NativeRuntime,
            Metrics::wall_ms(1),
        )
        .with_evidence(Evidence {
            screenshot: Some("frames/escape.png".to_string()),
            ..Evidence::default()
        });

        let modern = tool_result(&json!(10), &envelope, "2025-06-18", &config, &json!({}));
        let content = modern["result"]["content"].as_array().unwrap();
        assert!(
            !content.iter().any(|c| c["type"] == "image"),
            "symlinked evidence outside allowed roots must not be inlined: {content:?}"
        );
        assert!(
            !content.iter().any(|c| c["type"] == "resource_link"),
            "symlinked evidence outside allowed roots must not be linked: {content:?}"
        );
        let _ = std::fs::remove_dir_all(&base);
    }
}
