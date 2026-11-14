//! The MCP server: dispatch, concurrency, cancellation and result mapping.
//!
//! The transport is newline-delimited JSON-RPC 2.0. Fast methods (`initialize`,
//! `ping`, `tools/list`, notifications) are handled on the reader thread;
//! `tools/call` runs on a worker thread behind a bounded [`Gate`] so that a
//! long-running backend call cannot block cancellation or pings, and so that
//! concurrent calls are limited. Every diagnostic goes to stderr, never stdout.

use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use acos_protocol::{
    Backend as ProtocolBackend, CommandName, Envelope, ErrorCode, Fidelity, Metrics,
    ProtocolError, ResourceBudget,
};
use serde_json::{json, Value};

use crate::backend::{Backend, CallContext, Capabilities};
use crate::descriptors::{
    command_from_tool, negotiate, supports_structured_content, CAPABILITIES_TOOL,
    HELP_TOOL, INSTRUCTIONS, LATEST_PROTOCOL_VERSION, SERVER_NAME, SERVER_VERSION,
    SESSION_TOOL_NAMES,
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
            max_image_bytes: 512 * 1024,
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

        let permit = match self.gate.acquire(self.config.queue_timeout, token) {
            Ok(permit) => permit,
            Err(GateError::Cancelled) => {
                let envelope = cancelled_envelope(name);
                return tool_result(id, &envelope, &self.negotiated_version(), &self.config);
            }
            Err(GateError::Timeout) => {
                let envelope = busy_envelope(name, self.config.max_concurrency);
                return tool_result(id, &envelope, &self.negotiated_version(), &self.config);
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
                pass_with_result("req_capabilities", caps.to_json())
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
        tool_result(id, &envelope, &self.negotiated_version(), &self.config)
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
                match read_resource(uri) {
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
                let args = params.get("arguments").cloned().unwrap_or_else(|| json!({}));
                match prompt(name, &args) {
                    Ok(result) => jsonrpc::result(id, result),
                    Err(error) => jsonrpc::error(id, &error),
                }
            }
            "logging/setLevel" => jsonrpc::result(id, json!({})),
            _ => jsonrpc::error(
                id,
                &RpcError::new(METHOD_NOT_FOUND, format!("method `{method}` is not supported")),
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
        let handles: Vec<JoinHandle<()>> = std::mem::take(
            &mut *self.workers.lock().unwrap_or_else(|e| e.into_inner()),
        );
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

fn tool_result(
    id: &Value,
    envelope: &Envelope,
    protocol_version: &str,
    config: &ServerConfig,
) -> Value {
    let envelope_json = serde_json::to_value(envelope).unwrap_or(Value::Null);
    // F11: an agent pays for every byte of a reply and reads only the head and
    // the tail of a stack. Cap long strings here, once, for every tool, so the
    // CLI's own `--json` output and the artifacts on disk stay complete while
    // the MCP reply stays small.
    let (envelope_json, bounds) = crate::bound::bound_value(&envelope_json, config.max_text_bytes);
    let text = serde_json::to_string(&envelope_json).unwrap_or_default();
    let mut content = vec![json!({"type": "text", "text": text})];
    append_evidence_content(&mut content, envelope, config, protocol_version);
    let mut result = json!({
        "content": content,
        "isError": !envelope.status().is_pass(),
    });
    if !bounds.is_empty() {
        // Say that the reply was bounded, and how: a silent truncation would
        // read as the real text.
        result["_meta"] = json!({"acos_mcp": {"reply_bounded": bounds.to_json()}});
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
        if !config
            .allowed_roots
            .iter()
            .any(|root| full.starts_with(root))
        {
            continue;
        }
        if !full.is_file() {
            continue;
        }
        if let Some(mime) = image_mime(path) {
            if let Ok(metadata) = std::fs::metadata(&full) {
                if metadata.len() as usize <= config.max_image_bytes {
                    if let Ok(bytes) = std::fs::read(&full) {
                        content.push(json!({
                            "type": "image",
                            "data": crate::base64::encode(&bytes),
                            "mimeType": mime,
                        }));
                        continue;
                    }
                }
            }
        }
        if supports_structured_content(protocol_version) {
            content.push(json!({
                "type": "resource_link",
                "uri": format!("file://{}", full.display()),
                "name": kind,
            }));
        } else {
            content.push(json!({
                "type": "text",
                "text": format!("{kind}: {}", full.display()),
            }));
        }
    }
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
        help::TOPICS
            .iter()
            .filter(|topic| **topic != "errors")
            .map(|topic| match *topic {
                "overview" => "acos://guide/overview",
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
    for topic in help::TOPICS {
        list.push(json!({
            "uri": format!("acos://guide/{topic}"),
            "name": format!("agent-guide-{topic}"),
            "title": format!("ACOS agent guide: {topic}"),
            "description": match *topic {
                "overview" => "The three status rules, the loop in ten lines and the topic list (the `acos_help` default).",
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
pub fn read_resource(uri: &str) -> Option<Vec<Value>> {
    // `acos://guide` is the **complete** guide (a resource read is a deliberate,
    // unlimited fetch); the `acos_help` tool defaults to the skeleton instead.
    let topic = match uri {
        "acos://guide" => Some(help::ALL_TOPIC),
        // `acos://errors` is the error section under its own name.
        "acos://errors" => Some("errors"),
        other => {
            let name = other.strip_prefix("acos://guide/")?;
            Some(
                help::TOPICS
                    .iter()
                    .copied()
                    .find(|t| *t == name)?,
            )
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
        "text": help::guide(topic),
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
        return Err(RpcError::new(
            INVALID_PARAMS,
            format!("unknown prompt `{name}`"),
        )
        .with_data(json!({"available": ["verify_android_flow"]})));
    }
    let apk = args.get("apk").and_then(Value::as_str).unwrap_or("").trim();
    let goal = args.get("goal").and_then(Value::as_str).unwrap_or("").trim();
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
        assert_eq!(lookup_tool("acos_capabilities"), Some(ToolRef::Capabilities));
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

    #[test]
    fn evidence_becomes_image_and_resource_link() {
        use acos_protocol::Evidence;
        let dir = std::env::temp_dir().join(format!("acos-mcp-evidence-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("frames")).unwrap();
        std::fs::write(dir.join("frames/1.png"), b"fakepng").unwrap();
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

        let modern = tool_result(&json!(7), &envelope, "2025-06-18", &config);
        let content = modern["result"]["content"].as_array().unwrap();
        assert!(content
            .iter()
            .any(|c| c["type"] == "image" && c["mimeType"] == "image/png"));
        let link = content
            .iter()
            .find(|c| c["type"] == "resource_link")
            .expect("resource link");
        assert!(link["uri"].as_str().unwrap().ends_with("events.jsonl"));
        assert!(modern.pointer("/result/structuredContent").is_some());

        let legacy = tool_result(&json!(8), &envelope, "2025-03-26", &config);
        assert!(legacy.pointer("/result/structuredContent").is_none());
        assert!(legacy["result"]["content"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["type"] == "text"
                && c["text"].as_str().unwrap().contains("events.jsonl")));

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
        let guarded = tool_result(&json!(9), &escape, "2025-03-26", &config);
        assert_eq!(guarded["result"]["content"].as_array().unwrap().len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
