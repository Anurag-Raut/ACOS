//! In-process session backend for the MCP server.
//!
//! [`CliBackend`] shells out to the `acos` binary for every call, so it cannot
//! hold a session across calls. This backend adds a small map of
//! [`AgentSession`]s (one worker thread each, owning the `Vm`) and serves the
//! session commands **in-process**: `acos_run`/`acos_launch`,
//! `acos_observe`, `acos_action`/`acos_tap`/`acos_key`/`acos_swipe`,
//! `acos_logs`, `acos_status`, `acos_reset` and `acos_destroy`.
//!
//! Everything else (`inspect`, `run-dex`, `verify`, `capability-report`)
//! delegates to the wrapped [`CliBackend`], so the subprocess path is unchanged.
//!
//! Sessions are created lazily: an `observe`/`status`/`logs`/`action` on an
//! unknown session id creates a fresh, un-launched session, which is why
//! `observe` before any frame returns a structured `not_ready` rather than
//! `SESSION_NOT_FOUND`. `destroy` removes the session; the next read creates a
//! new one. This mirrors the CLI's process-local session.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use acos_protocol::{
    Backend as ProtocolBackend, CommandName, Envelope, ErrorCode, Fidelity, Metrics, ProtocolError,
};
use acos_runtime::session::{
    AgentSession, LaunchReport, SessionError, SessionOptions, StageStatus,
};
use acos_runtime::view_tree::{ViewNode, ViewTree};
use serde_json::{json, Value};

use crate::backend::{Backend, BackendConfig, BackendInfo, CallContext, Capabilities, CliBackend};
use crate::daemon::{self, DaemonClient};
use crate::flutter_backend::{self, FlutterSessions};
use acos_jni::FlutterSessionError;

/// Commands the session backend serves itself (bypassing the CLI capability
/// probe, which cannot see them).
const SESSION_COMMANDS: &[CommandName] = &[
    CommandName::Run,
    CommandName::Observe,
    CommandName::Action,
    CommandName::Reset,
    CommandName::Destroy,
];

/// The default session id when a request omits `base.session_id`.
pub const DEFAULT_SESSION: &str = "s_mcp";

/// A [`Backend`] that holds in-process sessions and delegates the rest to a
/// [`CliBackend`].
pub struct SessionBackend {
    cli: CliBackend,
    sessions: Mutex<HashMap<String, AgentSession>>,
    limits: SessionOptions,
    /// Confined child MCP servers holding native sessions (one per session id).
    native_children: Mutex<HashMap<String, ConfinedChild>>,
    /// The `acos-mcp` binary to re-exec as a confined child.
    child_binary: PathBuf,
    /// Monotonic counter for per-child VFS roots.
    next_child: AtomicU64,
    /// Optional running session daemon to forward session tools to (F-08).
    daemon: Option<Mutex<DaemonClient>>,
    /// Flutter sessions owned by this process (the confined child).
    flutter: FlutterSessions,
}

impl std::fmt::Debug for SessionBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionBackend")
            .field(
                "sessions",
                &self.sessions.lock().map(|s| s.len()).unwrap_or(0),
            )
            .finish_non_exhaustive()
    }
}

impl SessionBackend {
    /// Wrap a CLI backend and start with no sessions.
    pub fn new(config: BackendConfig, limits: SessionOptions) -> Self {
        SessionBackend {
            cli: CliBackend::new(config),
            sessions: Mutex::new(HashMap::new()),
            limits,
            native_children: Mutex::new(HashMap::new()),
            child_binary: std::env::current_exe().unwrap_or_else(|_| PathBuf::from("acos-mcp")),
            next_child: AtomicU64::new(0),
            daemon: None,
            flutter: FlutterSessions::default(),
        }
    }

    /// Wrap an already-probed CLI backend.
    pub fn with_cli(cli: CliBackend, limits: SessionOptions) -> Self {
        SessionBackend {
            cli,
            sessions: Mutex::new(HashMap::new()),
            limits,
            native_children: Mutex::new(HashMap::new()),
            child_binary: std::env::current_exe().unwrap_or_else(|_| PathBuf::from("acos-mcp")),
            next_child: AtomicU64::new(0),
            daemon: None,
            flutter: FlutterSessions::default(),
        }
    }

    /// Override the binary re-exec'd as the confined native session child.
    #[must_use]
    pub fn with_native_child_binary(mut self, path: PathBuf) -> Self {
        self.child_binary = path;
        self
    }

    /// Forward session tools to a running daemon at `path` (F-08).
    ///
    /// Used by the MCP server when `ACOS_SESSION_DAEMON` is set, so a CLI-driven
    /// daemon can own the session while MCP tools operate on it.
    #[must_use]
    pub fn with_daemon_socket(mut self, path: PathBuf) -> Self {
        match DaemonClient::connect(&path) {
            Ok(client) => self.daemon = Some(Mutex::new(client)),
            Err(e) => eprintln!(
                "acos-mcp: session daemon at {} unavailable: {e}",
                path.display()
            ),
        }
        self
    }

    /// Forward session tools to the default daemon socket when it is running.
    #[must_use]
    pub fn with_daemon(self) -> Self {
        self.with_daemon_socket(daemon::socket_path())
    }

    /// Number of live sessions (in-process tasks plus confined native children).
    #[must_use]
    pub fn session_count(&self) -> usize {
        let local = self
            .sessions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .len();
        let native = self
            .native_children
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .len();
        local + native + self.flutter.len()
    }

    /// A `session_list` result: every held session's status (native included).
    #[must_use]
    pub fn list_sessions(&self) -> Value {
        let mut out: Vec<Value> = Vec::new();
        {
            let sessions = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
            for (id, session) in sessions.iter() {
                let state = session.status().ok();
                out.push(json!({
                    "session_id": id,
                    "launched": state.as_ref().map(|s| s.launched).unwrap_or(false),
                    "package": state.as_ref().and_then(|s| s.package.clone()),
                    "stage_reached": state.as_ref().map(|s| s.stage_reached).unwrap_or("none"),
                    "published_frames": state.as_ref().map(|s| s.published_frames).unwrap_or(0),
                    "native": false,
                }));
            }
        }
        {
            let mut children = self
                .native_children
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            for (id, child) in children.iter_mut() {
                let probe = json!({"base": {"request_id": "req_list", "session_id": id}});
                let env = child.call_tool("acos_status", &probe);
                let result = env.result().cloned().unwrap_or(Value::Null);
                out.push(json!({
                    "session_id": id,
                    "launched": result["launched"],
                    "package": result["package"],
                    "stage_reached": result["stage_reached"],
                    "published_frames": result["published_frames"],
                    "native": true,
                }));
            }
        }
        // Flutter sessions are read over their worker channels so a wedged
        // session shows `live: false` rather than hanging the list.
        {
            for id in self.flutter.ids() {
                match self.flutter.status(&id) {
                    Ok(status) => out.push(json!({
                        "session_id": id,
                        "launched": true,
                        "package": "",
                        "stage_reached": "first_frame",
                        "published_frames": status.frames,
                        "semantics": status.semantics,
                        "launch_ms": status.launch_ms,
                        "apk": status.apk.display().to_string(),
                        "native": true,
                        "flutter": true,
                    })),
                    Err(e) => out.push(json!({
                        "session_id": id,
                        "launched": false,
                        "stage_reached": "unknown",
                        "published_frames": 0,
                        "native": true,
                        "flutter": true,
                        "error": e.to_string(),
                    })),
                }
            }
        }
        json!({"sessions": out, "count": out.len()})
    }

    /// The CLI backend used for non-session commands.
    pub fn cli(&self) -> &CliBackend {
        &self.cli
    }

    fn with_session<T>(&self, id: &str, f: impl FnOnce(&AgentSession) -> T) -> T {
        let mut sessions = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
        let session = sessions
            .entry(id.to_string())
            .or_insert_with(|| AgentSession::start(self.limits));
        f(session)
    }

    fn remove_session(&self, id: &str) -> bool {
        let mut sessions = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
        sessions.remove(id).is_some()
    }

    /// Whether `id` is held by a confined child process.
    fn has_native_child(&self, id: &str) -> bool {
        self.native_children
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains_key(id)
    }

    /// Forward one session tool call to the confined child that holds `id`.
    fn forward_native(&self, id: &str, tool: &str, args: &Value) -> Option<Envelope> {
        let mut children = self.native_children.lock().unwrap_or_else(|e| e.into_inner());
        let child = children.get_mut(id)?;
        Some(child.call_tool(tool, args))
    }

    /// Drop a confined child, returning whether one existed.
    fn remove_native_child(&self, id: &str) -> bool {
        let mut children = self.native_children.lock().unwrap_or_else(|e| e.into_inner());
        children.remove(id).is_some_and(|mut child| {
            child.shutdown();
            true
        })
    }

    /// Launch a native session in a freshly re-exec'd, confined child process.
    ///
    /// The child enters the ADR-0022 sandbox before it creates any thread, then
    /// runs the session commands over the MCP stdio protocol. This process keeps
    /// the child (and therefore the session and its `FrameStore`) alive for
    /// later `acos_observe`/`acos_action`/... calls.
    fn launch_native_child(
        &self,
        request_id: &str,
        id: &str,
        apk: &std::path::Path,
        limits: &SessionOptions,
        ctx: &CallContext,
    ) -> Envelope {
        let _ = self.remove_native_child(id);
        let _ = self.remove_session(id);

        let root = match native_session_root(self.next_child.fetch_add(1, Ordering::SeqCst)) {
            Ok(root) => root,
            Err(e) => {
                return native_child_error(
                    request_id,
                    id,
                    "CONFINEMENT_UNAVAILABLE",
                    &format!("cannot create a private native session root: {e}"),
                );
            }
        };
        if let Err(e) = std::fs::create_dir_all(root.join("data/user/0")) {
            return native_child_error(
                request_id,
                id,
                "CONFINEMENT_UNAVAILABLE",
                &format!("cannot create native session root {}: {e}", root.display()),
            );
        }
        let mut read_only: Vec<PathBuf> = ctx.allowed_roots.clone();
        if let Some(boot) = native_boot_root() {
            read_only.push(boot);
        }
        if let Some(parent) = apk.parent() {
            read_only.push(parent.to_path_buf());
        }
        // A Flutter APK needs its extracted engine + AOT snapshot readable; the
        // engine lives under the workspace `.cache`, not the APK directory.
        let mut flutter_lib: Option<PathBuf> = None;
        if flutter_backend::is_flutter(apk) {
            let lib_dir = flutter_backend::default_lib_dir();
            if lib_dir.is_dir() {
                // Landlock rules are per-inode and ancestors need EXECUTE; a
                // symlinked `.cache/flutter-1` must be canonicalised so the
                // grant covers the real target's ancestor chain. The child is
                // told the same absolute path (`--flutter-lib` -> profile env,
                // because the sandbox `clearenv`s inherited variables) so it
                // never has to resolve the symlink under confinement.
                let lib_dir = std::fs::canonicalize(&lib_dir).unwrap_or(lib_dir);
                read_only.push(lib_dir.clone());
                flutter_lib = Some(lib_dir);
            }
        }

        let mut child = match ConfinedChild::spawn(
            &self.child_binary,
            &root,
            &read_only,
            flutter_lib.as_deref(),
        ) {
            Ok(child) => child,
            Err(e) => {
                return native_child_error(request_id, id, "CONFINEMENT_UNAVAILABLE", &e);
            }
        };
        let launch_args = json!({
            "apk": apk.display().to_string(),
            "native": true,
            "timeout_ms": limits.wall_secs.saturating_mul(1000),
            "base": {"request_id": request_id, "session_id": id},
        });
        let envelope = child.call_tool("acos_launch", &launch_args);
        // Keep the child even when the ladder is blocked: the native evidence
        // (FrameStore) is still live and `acos_observe` must see it.
        self.native_children
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id.to_string(), child);
        envelope
    }

    // --- Flutter sessions (W149-A) -----------------------------------------
    //
    // A detected Flutter APK is driven by a `FlutterSession` on its own worker
    // thread inside this (confined) process. Every tool below marshals to that
    // worker, so the engine's looper thread identity is stable across tool
    // calls.

    /// Launch a Flutter APK and pump until the first frame.
    fn launch_flutter(&self, request_id: &str, id: &str, apk: &std::path::Path) -> Envelope {
        let scratch = flutter_scratch();
        if let Err(e) = std::fs::create_dir_all(&scratch) {
            return flutter_error(
                request_id,
                Some(id),
                "CONFINEMENT_UNAVAILABLE",
                &format!("cannot create Flutter scratch {}: {e}", scratch.display()),
            );
        }
        if let Err(e) = self.flutter.launch(id, apk, scratch) {
            return flutter_session_error(request_id, Some(id), &e);
        }
        let status = self.flutter.status(id).ok();
        let observed = self.flutter.observe(id, false);
        let (frames, seq, width, height) = match &observed {
            Ok(o) => (o.frames, o.seq, o.width, o.height),
            Err(_) => (status.as_ref().map(|s| s.frames).unwrap_or(0), 0, 0, 0),
        };
        let launch_ms = status.as_ref().map(|s| s.launch_ms).unwrap_or(0);
        let engine = status.as_ref().map(|s| s.engine).unwrap_or((0, 0, 0));
        let first_frame_ok = frames > 0;
        let blocker = if first_frame_ok {
            Value::Null
        } else {
            json!({
                "code": observed.as_ref().err().map_or("TIMEOUT", |e| e.code()),
                "message": observed
                    .as_ref()
                    .err()
                    .map_or_else(|| "no frame presented".to_string(), ToString::to_string),
                "details": {},
            })
        };
        let result = json!({
            "stages": [
                {"stage": "apk_parsed", "ok": true, "status": "ok", "blocker": null},
                {"stage": "flutter_engine", "ok": true, "status": "ok", "blocker": null},
                {"stage": "first_frame", "ok": first_frame_ok,
                 "status": if first_frame_ok { "ok" } else { "blocked" }, "blocker": blocker},
            ],
            "reached_stage": if first_frame_ok { "first_frame" } else { "flutter_engine" },
            "package": "",
            "activity": null,
            "classes_loaded": 0,
            "events": ["flutter=1", format!("frames={frames}")],
            "wall_ms": launch_ms,
            "peak_rss_bytes": 0,
            "engine": {
                "imports_total": engine.0,
                "imports_missing": engine.1,
                "relocations": engine.2,
            },
            "frame": if first_frame_ok {
                json!({"frame_seq": seq, "width": width, "height": height,
                       "meaningful": width > 0 && height > 0})
            } else {
                Value::Null
            },
        });
        let envelope = if first_frame_ok {
            Envelope::pass(
                request_id,
                ProtocolBackend::AcosNative,
                Fidelity::NativeRuntime,
                Metrics::wall_ms(launch_ms as u64),
            )
        } else {
            Envelope::failure(
                request_id,
                ProtocolBackend::AcosNative,
                Fidelity::NativeRuntime,
                ProtocolError::new(
                    ErrorCode::Timeout,
                    "Flutter engine did not present a frame".to_string(),
                )
                .with_details(json!({"stage": "first_frame"})),
                Metrics::wall_ms(launch_ms as u64),
            )
        };
        envelope
            .with_session_id(id.to_string())
            .with_value_result(result)
            .with_capabilities(["apk.inspect", "lifecycle", "ui.frame"])
    }

    fn observe_flutter(&self, request_id: &str, id: &str, args: &Value) -> Envelope {
        let wants_png = args.get("png").and_then(Value::as_bool).unwrap_or(false)
            || args.get("screenshot").is_some_and(|v| !v.is_null());
        let observed = match self.flutter.observe(id, wants_png) {
            Ok(o) => o,
            Err(e) => return flutter_session_error(request_id, Some(id), &e),
        };
        let mut result = json!({
            "session_id": id,
            "package": "",
            "stage_reached": "first_frame",
            "published_frames": observed.frames,
            "semantics": observed.semantics,
            "frame": {
                "frame_seq": observed.seq,
                "width": observed.width,
                "height": observed.height,
                "density": 0,
                "content_hash": null,
                "meaningful": observed.width > 0 && observed.height > 0,
            },
        });
        let mut capabilities = vec!["ui.frame"];
        if let Some(png) = &observed.png {
            result["png_base64"] = json!(crate::base64::encode(png));
            result["png_bytes"] = json!(png.len());
            capabilities.push("ui.screenshot");
            if let Some(path) = args.get("screenshot").and_then(Value::as_str) {
                match std::fs::write(path, png) {
                    Ok(()) => result["screenshot"] = json!({"path": path, "bytes": png.len()}),
                    Err(e) => result["screenshot_error"] = json!(e.to_string()),
                }
            }
        }
        Envelope::pass(
            request_id,
            ProtocolBackend::AcosNative,
            Fidelity::NativeRuntime,
            Metrics::wall_ms(0),
        )
        .with_session_id(id.to_string())
        .with_value_result(result)
        .with_capabilities(capabilities)
    }

    fn observe_tree_flutter(
        &self,
        request_id: &str,
        id: &str,
        max_nodes: usize,
    ) -> Envelope {
        let nodes = match self.flutter.semantics(id) {
            Ok(nodes) => nodes,
            Err(e) => return flutter_session_error(request_id, Some(id), &e),
        };
        let truncated = nodes.len() > max_nodes;
        let nodes: Vec<Value> = nodes
            .iter()
            .take(max_nodes)
            .map(|n| {
                json!({
                    "id": n.id,
                    "text": n.text(),
                    "label": n.label,
                    "value": n.value,
                    "hint": n.hint,
                    "bounds": n.bounds(),
                    "clickable": n.clickable(),
                    "enabled": n.enabled(),
                    "role": n.role,
                })
            })
            .collect();
        Envelope::pass(
            request_id,
            ProtocolBackend::AcosNative,
            Fidelity::NativeRuntime,
            Metrics::wall_ms(0),
        )
        .with_session_id(id.to_string())
        .with_value_result(json!({
            "session_id": id,
            "package": "",
            "stage_reached": "first_frame",
            "windows": 1,
            "truncated": truncated,
            "tree": nodes,
        }))
        .with_capabilities(["ui.tree"])
    }

    fn tap_flutter(&self, request_id: &str, id: &str, text: Option<&str>, point: Option<(f64, f64)>) -> Envelope {
        let outcome = match (text, point) {
            (Some(label), _) => self
                .flutter
                .tap_text(id, label)
                .map(|p| (p, Some(label.to_string()), "tap_text")),
            (None, Some((x, y))) => self.flutter.tap(id, x, y).map(|()| ([x, y], None, "tap")),
            (None, None) => {
                return invalid_argument(request_id, "action", "tap needs `x` and `y`, or `text`")
            }
        };
        match outcome {
            Ok(([x, y], resolved, kind)) => Envelope::pass(
                request_id,
                ProtocolBackend::AcosNative,
                Fidelity::NativeRuntime,
                Metrics::wall_ms(0),
            )
            .with_session_id(id.to_string())
            .with_value_result(json!({
                "session_id": id,
                "action": kind,
                "accepted": true,
                "state_changed": true,
                "tapped": [x, y],
                "resolved_text": resolved,
            }))
            .with_capabilities(["input"]),
            Err(e) => flutter_session_error(request_id, Some(id), &e),
        }
    }

    fn logs_flutter(&self, request_id: &str, id: &str) -> Envelope {
        let lines = self.flutter.logs(id);
        let records: Vec<Value> = lines
            .iter()
            .map(|line| json!({"priority": "info", "tag": "flutter", "message": line, "line": line}))
            .collect();
        Envelope::pass(
            request_id,
            ProtocolBackend::AcosNative,
            Fidelity::NativeRuntime,
            Metrics::wall_ms(0),
        )
        .with_session_id(id.to_string())
        .with_value_result(json!({
            "session_id": id,
            "launched": true,
            "stage_reached": "first_frame",
            "count": records.len(),
            "logs": records,
        }))
        .with_capabilities(["logs"])
    }

    fn status_flutter(&self, request_id: &str, id: &str) -> Envelope {
        match self.flutter.status(id) {
            Ok(status) => Envelope::pass(
                request_id,
                ProtocolBackend::AcosNative,
                Fidelity::NativeRuntime,
                Metrics::wall_ms(0),
            )
            .with_session_id(id.to_string())
            .with_value_result(json!({
                "session_id": id,
                "launched": true,
                "package": "",
                "apk": status.apk.display().to_string(),
                "stage_reached": "first_frame",
                "published_frames": status.frames,
                "semantics": status.semantics,
                "launch_ms": status.launch_ms,
                "elapsed_ms": status.elapsed_ms,
                "live": true,
                "flutter": true,
            })),
            Err(e) => flutter_session_error(request_id, Some(id), &e),
        }
    }

    fn call_run(&self, request_id: &str, args: &Value, ctx: &CallContext) -> Envelope {
        let Some(apk) = args.get("apk").and_then(Value::as_str) else {
            return invalid_argument(request_id, "apk", "missing required string `apk`");
        };
        let apk = match crate::safety::validate_path(apk, &ctx.allowed_roots) {
            Ok(path) => path,
            Err(reason) => return unsafe_path(request_id, apk, &reason),
        };
        let id = session_id(args);
        let mut limits = self.limits;
        if let Some(ms) = args
            .get("timeout_ms")
            .and_then(Value::as_u64)
            .or(ctx.budget.wall_ms)
        {
            limits.wall_secs = (ms / 1000).max(1);
        }
        // Opt-in native code (`acos_launch { native: true }`). The JNI loader
        // requires an active process confinement and fails closed with
        // `CONFINEMENT_UNAVAILABLE` otherwise. This server is multi-threaded and
        // cannot enter seccomp itself, so it re-execs a confined child session
        // process instead of failing (or confining mid-flight).
        // `--flutter` or a structural Flutter APK implies native: the engine is
        // host native code and must run under the confined child. Auto-detect is
        // structural (libflutter.so / flutter_assets), never a manifest guess.
        let explicit_flutter = args.get("flutter").and_then(Value::as_bool).unwrap_or(false);
        let native_flag = args.get("native").and_then(Value::as_bool).unwrap_or(false);
        let native = native_flag || explicit_flutter || flutter_backend::is_flutter(&apk);
        limits.native = native;
        if limits.native && !acos_sandbox::confinement_active() {
            return self.launch_native_child(request_id, &id, &apk, &limits, ctx);
        }
        if limits.native && flutter_backend::is_flutter(&apk) {
            return self.launch_flutter(request_id, &id, &apk);
        }
        // Replace any prior session for this id so a second launch is a clean
        // task (the process-global transport/surface seams tolerate only one
        // live runtime).
        let _ = self.remove_session(&id);
        let session = AgentSession::start(limits);
        let result = session.launch(&apk);
        let envelope = match result {
            Ok(report) => {
                // Keep the session in the map so observe/action see its state.
                {
                    let mut sessions = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
                    sessions.insert(id.clone(), session);
                }
                launch_envelope(request_id, &id, &report)
            }
            Err(err) => {
                session.close();
                session_error_envelope(request_id, Some(&id), &err)
            }
        };
        envelope
    }

    fn call_observe(&self, request_id: &str, args: &Value, _ctx: &CallContext) -> Envelope {
        let id = session_id(args);
        if self.flutter.contains(&id) {
            return self.observe_flutter(request_id, &id, args);
        }
        if self.has_native_child(&id) {
            if let Some(env) = self.forward_native(&id, "acos_observe", args) {
                return env;
            }
        }
        let tree = args.get("tree").and_then(Value::as_bool).unwrap_or(true);
        let text = args.get("text").and_then(Value::as_bool).unwrap_or(false);
        // `--screenshot <path>` / `{"screenshot": path}` must actually write the
        // PNG for a daemon-held session too; previously it was dropped here and
        // the call returned a bare `pass`, so an agent could believe it had
        // evidence with no file on disk.
        let screenshot = args
            .get("screenshot")
            .and_then(Value::as_str)
            .map(std::path::PathBuf::from);
        self.with_session(&id, |session| {
            let status = session.status().ok();
            match session.observe(screenshot.clone()) {
                Ok(report) => {
                    if tree || text {
                        return unsupported(
                            request_id,
                            "ui.tree",
                            "UI tree extraction is not implemented in this build",
                        )
                        .with_session_id(id.clone());
                    }
                    let frame = report.frame.as_ref().map(|frame| {
                        json!({
                            "frame_seq": frame.frame_id,
                            "width": frame.width,
                            "height": frame.height,
                            "content_hash": frame.content_hash,
                            "cause_id": frame.cause_id.to_string(),
                            "meaningful": !frame.pixels.is_empty(),
                        })
                    });
                    let mut result = json!({
                        "session_id": id,
                        "package": status.and_then(|s| s.package).unwrap_or_default(),
                        "stage_reached": report.stage_reached,
                        "published_frames": report.published_frames,
                        "frame": frame,
                    });
                    let mut capabilities = vec!["ui.frame"];
                    if let Some((path, bytes)) = &report.screenshot {
                        result["screenshot"] = json!({"path": path, "bytes": bytes});
                        capabilities.push("ui.screenshot");
                    } else if let Some(path) = &screenshot {
                        result["screenshot_error"] = json!(format!(
                            "no frame was available to write to {}",
                            path.display()
                        ));
                    }
                    Envelope::pass(
                        request_id,
                        ProtocolBackend::AcosNative,
                        Fidelity::NativeRuntime,
                        Metrics::wall_ms(0),
                    )
                    .with_session_id(id.clone())
                    .with_value_result(result)
                    .with_capabilities(capabilities)
                }
                Err(err) => session_error_envelope(request_id, Some(&id), &err),
            }
        })
    }

    fn call_observe_tree(
        &self,
        request_id: &str,
        args: &Value,
        _ctx: &CallContext,
    ) -> Envelope {
        let id = session_id(args);
        if self.flutter.contains(&id) {
            let max_nodes = args
                .get("max_nodes")
                .and_then(Value::as_u64)
                .unwrap_or(2000) as usize;
            return self.observe_tree_flutter(request_id, &id, max_nodes);
        }
        if self.has_native_child(&id) {
            if let Some(env) = self.forward_native(&id, "acos_observe_tree", args) {
                return env;
            }
        }
        let max_nodes = args
            .get("max_nodes")
            .and_then(Value::as_u64)
            .unwrap_or(2000) as usize;
        self.with_session(&id, |session| {
            let status = session.status().ok();
            match session.observe_tree(max_nodes) {
                Ok(tree) => {
                    let package = status
                        .as_ref()
                        .and_then(|s| s.package.clone())
                        .unwrap_or_default();
                    let stage = status
                        .as_ref()
                        .map(|s| s.stage_reached)
                        .unwrap_or("none");
                    Envelope::pass(
                        request_id,
                        ProtocolBackend::AcosNative,
                        Fidelity::NativeRuntime,
                        Metrics::wall_ms(0),
                    )
                    .with_session_id(id.clone())
                    .with_value_result(json!({
                        "session_id": id,
                        "package": package,
                        "stage_reached": stage,
                        "windows": tree.windows,
                        "truncated": tree.truncated,
                        "tree": view_tree_json(&tree),
                    }))
                    .with_capabilities(["ui.tree"])
                }
                Err(err) => session_error_envelope(request_id, Some(&id), &err),
            }
        })
    }

    /// `acos_text`: type into the focused editor.
    ///
    /// The runtime prefers the real framework
    /// `InputConnection.commitText` path and falls back to dispatcher key
    /// events only for fields with no editable connection.
    fn call_text(&self, request_id: &str, args: &Value, _ctx: &CallContext) -> Envelope {
        let id = session_id(args);
        if self.has_native_child(&id) {
            if let Some(env) = self.forward_native(&id, "acos_text", args) {
                return env;
            }
        }
        let Some(text) = args.get("text").and_then(Value::as_str) else {
            return invalid_argument(request_id, "text", "`text` must be a string");
        };
        self.with_session(&id, |session| {
            input_envelope(request_id, &id, session.text(text), "text")
        })
    }

    fn call_action(&self, request_id: &str, args: &Value, _ctx: &CallContext) -> Envelope {
        let id = session_id(args);
        if self.flutter.contains(&id) {
            let action = args.get("action").cloned().unwrap_or(Value::Null);
            let kind = action.get("kind").and_then(Value::as_str).unwrap_or("tap");
            return match kind {
                "tap" => {
                    let text = action.get("text").and_then(Value::as_str);
                    let point = match (
                        action.get("x").and_then(Value::as_f64),
                        action.get("y").and_then(Value::as_f64),
                    ) {
                        (Some(x), Some(y)) => Some((x, y)),
                        _ => None,
                    };
                    self.tap_flutter(request_id, &id, text, point)
                }
                other => flutter_error(
                    request_id,
                    Some(&id),
                    "UNSUPPORTED_BACKEND",
                    &format!("Flutter session does not support action kind {other:?}"),
                ),
            };
        }
        if self.has_native_child(&id) {
            if let Some(env) = self.forward_native(&id, "acos_action", args) {
                return env;
            }
        }
        let action = args.get("action").cloned().unwrap_or(Value::Null);
        let kind = action.get("kind").and_then(Value::as_str).unwrap_or("tap");
        self.with_session(&id, |session| match kind {
            "tap" => {
                if let Some(label) = action.get("text").and_then(Value::as_str) {
                    return input_envelope(request_id, &id, session.tap_text(label), "tap");
                }
                match (
                    action.get("x").and_then(Value::as_f64),
                    action.get("y").and_then(Value::as_f64),
                ) {
                    (Some(x), Some(y)) => {
                        input_envelope(request_id, &id, session.tap(x as f32, y as f32), "tap")
                    }
                    _ => invalid_argument(request_id, "action", "tap needs `x` and `y`, or `text`"),
                }
            }
            "key" => {
                let Some(code) = action_key_code(&action) else {
                    return invalid_argument(request_id, "action.key", "unknown key code");
                };
                input_envelope(request_id, &id, session.key(code), "key")
            }
            "back" => input_envelope(request_id, &id, session.key(4), "key"),
            "swipe" => {
                let from = action.get("from");
                let to = action.get("to");
                let (Some(from), Some(to)) = (from, to) else {
                    return invalid_argument(request_id, "action", "swipe needs `from` and `to`");
                };
                let point = |value: &Value| -> Option<(f32, f32)> {
                    Some((
                        value.get("x")?.as_f64()? as f32,
                        value.get("y")?.as_f64()? as f32,
                    ))
                };
                let (Some(from), Some(to)) = (point(from), point(to)) else {
                    return invalid_argument(request_id, "action", "swipe points need `x` and `y`");
                };
                let duration = action
                    .get("duration_ms")
                    .and_then(Value::as_u64)
                    .unwrap_or(300);
                input_envelope(
                    request_id,
                    &id,
                    session.swipe(from, to, Duration::from_millis(duration), 8),
                    "swipe",
                )
            }
            other => invalid_argument(request_id, "action.kind", &format!("unsupported {other:?}")),
        })
    }

    fn call_reset(&self, request_id: &str, args: &Value, _ctx: &CallContext) -> Envelope {
        let id = session_id(args);
        if self.flutter.contains(&id) {
            self.flutter.remove(&id);
            return Envelope::pass(
                request_id,
                ProtocolBackend::AcosNative,
                Fidelity::NativeRuntime,
                Metrics::wall_ms(0),
            )
            .with_session_id(id.clone())
            .with_value_result(json!({
                "session_id": id,
                "scope": "all",
                "state_reset": true,
                "data_cleared": true,
                "isolation": "dedicated-worker",
                "stage_reached": "none",
            }));
        }
        if self.has_native_child(&id) {
            if let Some(env) = self.forward_native(&id, "acos_reset", args) {
                return env;
            }
        }
        self.with_session(&id, |session| match session.reset() {
            Ok(state) => Envelope::pass(
                request_id,
                ProtocolBackend::AcosNative,
                Fidelity::NativeRuntime,
                Metrics::wall_ms(0),
            )
            .with_session_id(id.clone())
            .with_value_result(json!({
                "session_id": id,
                "scope": "all",
                "state_reset": true,
                "data_cleared": false,
                "isolation": "shared-task",
                "note": "in-process session: the guest VFS and log sink are process-global, so app data is not cleared",
                "stage_reached": state.stage_reached,
            })),
            Err(err) => session_error_envelope(request_id, Some(&id), &err),
        })
    }

    fn call_destroy(&self, request_id: &str, args: &Value, _ctx: &CallContext) -> Envelope {
        let id = session_id(args);
        let local = self.remove_session(&id);
        let native = self.remove_native_child(&id);
        let flutter = self.flutter.remove(&id);
        let destroyed = local || native || flutter;
        Envelope::pass(
            request_id,
            ProtocolBackend::AcosNative,
            Fidelity::NativeRuntime,
            Metrics::wall_ms(0),
        )
        .with_session_id(id)
        .with_value_result(json!({
            "destroyed": destroyed,
            "already_absent": !destroyed,
        }))
    }

    /// Server-local session tools (`acos_launch`, `acos_tap`, ...).
    pub fn session_call(&self, name: &str, args: &Value, ctx: &CallContext) -> Option<Envelope> {
        if name != "acos_doctor" && !name.is_empty() {
            if let Some(daemon) = &self.daemon {
                let mut client = daemon.lock().unwrap_or_else(|e| e.into_inner());
                return Some(match client.call(name, args) {
                    Ok(envelope) => envelope,
                    Err(e) => daemon_error(&request_id(args), args, &e.to_string()),
                });
            }
        }
        let request_id = request_id(args);
        match name {
            "acos_launch" => Some(self.call_run(&request_id, args, ctx)),
            "acos_observe_tree" => Some(self.call_observe_tree(&request_id, args, ctx)),
            "acos_observe" => Some(self.call_observe(&request_id, args, ctx)),
            "acos_tap" => Some(self.call_action(
                &request_id,
                &json!({"base": session_base(args), "action": {"kind": "tap", "x": args.get("x"), "y": args.get("y"), "text": args.get("text")}}),
                ctx,
            )),
            "acos_key" => Some(self.call_action(
                &request_id,
                &json!({"base": session_base(args), "action": {"kind": "key", "code": args.get("code")}}),
                ctx,
            )),
            "acos_swipe" => Some(self.call_action(
                &request_id,
                &json!({"base": session_base(args), "action": {"kind": "swipe", "from": args.get("from"), "to": args.get("to"), "duration_ms": args.get("duration_ms")}}),
                ctx,
            )),
            "acos_logs" => Some(self.call_logs(&request_id, args)),
            "acos_status" => Some(self.call_status(&request_id, args)),
            "acos_reset" => Some(self.call_reset(&request_id, args, ctx)),
            "acos_destroy" => Some(self.call_destroy(&request_id, args, ctx)),
            "acos_text" => Some(self.call_text(&request_id, args, ctx)),
            "acos_doctor" => Some(self.cli.call_doctor(&request_id, args, ctx)),
            _ => None,
        }
    }

    fn call_logs(&self, request_id: &str, args: &Value) -> Envelope {
        let id = session_id(args);
        if self.flutter.contains(&id) {
            return self.logs_flutter(request_id, &id);
        }
        if self.has_native_child(&id) {
            if let Some(env) = self.forward_native(&id, "acos_logs", args) {
                return env;
            }
        }
        self.with_session(&id, |session| match session.logs() {
            Ok(lines) => {
                let status = session.status().ok();
                let records: Vec<Value> = lines
                    .iter()
                    .map(|line| {
                        json!({
                            "priority": line.priority,
                            "tag": line.tag,
                            "message": line.message,
                            "line": line.line,
                        })
                    })
                    .collect();
                // An empty buffer is a real pass, but it is misleading right
                // after a launch that stopped before `Activity.onCreate`; carry
                // the launch stage so the agent can tell the two apart.
                let stage_reached = status.as_ref().map(|s| s.stage_reached).unwrap_or("none");
                Envelope::pass(
                    request_id,
                    ProtocolBackend::AcosNative,
                    Fidelity::NativeRuntime,
                    Metrics::wall_ms(0),
                )
                .with_session_id(id.clone())
                .with_value_result(json!({
                    "session_id": id,
                    "launched": status.as_ref().map(|s| s.launched).unwrap_or(false),
                    "stage_reached": stage_reached,
                    "count": records.len(),
                    "logs": records,
                    "note": if records.is_empty() {
                        "empty is valid only if the app reached onCreate; check stage_reached (and `acos_launch`) before concluding anything"
                    } else {
                        ""
                    },
                }))
                .with_capabilities(["logs"])
            }
            Err(err) => session_error_envelope(request_id, Some(&id), &err),
        })
    }

    fn call_status(&self, request_id: &str, args: &Value) -> Envelope {
        let id = session_id(args);
        if self.flutter.contains(&id) {
            return self.status_flutter(request_id, &id);
        }
        if self.has_native_child(&id) {
            if let Some(env) = self.forward_native(&id, "acos_status", args) {
                return env;
            }
        }
        self.with_session(&id, |session| match session.status() {
            Ok(state) => Envelope::pass(
                request_id,
                ProtocolBackend::AcosNative,
                Fidelity::NativeRuntime,
                Metrics::wall_ms(0),
            )
            .with_session_id(id.clone())
            .with_value_result(json!({
                "session_id": id,
                "launched": state.launched,
                "package": state.package,
                "apk": state.apk,
                "stage_reached": state.stage_reached,
                "published_frames": state.published_frames,
                "live": true,
            })),
            Err(err) => session_error_envelope(request_id, Some(&id), &err),
        })
    }
}

impl Backend for SessionBackend {
    fn call(&self, command: CommandName, args: &Value, ctx: &CallContext) -> Envelope {
        if let Some(daemon) = &self.daemon {
            let tool = match command {
                CommandName::Run => Some("acos_launch"),
                CommandName::Observe => Some("acos_observe"),
                CommandName::Action => Some("acos_action"),
                CommandName::Reset => Some("acos_reset"),
                CommandName::Destroy => Some("acos_destroy"),
                _ => None,
            };
            if let Some(tool) = tool {
                let mut client = daemon.lock().unwrap_or_else(|e| e.into_inner());
                return match client.call(tool, args) {
                    Ok(envelope) => envelope,
                    Err(e) => daemon_error(&request_id(args), args, &e.to_string()),
                };
            }
        }
        let request_id = request_id(args);
        match command {
            CommandName::Run => self.call_run(&request_id, args, ctx),
            CommandName::Observe => self.call_observe(&request_id, args, ctx),
            CommandName::Action => self.call_action(&request_id, args, ctx),
            CommandName::Reset => self.call_reset(&request_id, args, ctx),
            CommandName::Destroy => self.call_destroy(&request_id, args, ctx),
            other => self.cli.call(other, args, ctx),
        }
    }

    fn capabilities(&self) -> Capabilities {
        let mut caps = self.cli.capabilities();
        for command in SESSION_COMMANDS {
            caps.supported.insert(*command);
        }
        caps
    }

    fn refresh_capabilities(&self) -> Capabilities {
        let _ = self.cli.refresh_capabilities();
        self.capabilities()
    }

    fn info(&self) -> BackendInfo {
        self.cli.info()
    }

    fn session_call(&self, name: &str, args: &Value, ctx: &CallContext) -> Option<Envelope> {
        SessionBackend::session_call(self, name, args, ctx)
    }
}

fn session_id(args: &Value) -> String {
    args.pointer("/base/session_id")
        .and_then(Value::as_str)
        .unwrap_or(DEFAULT_SESSION)
        .to_string()
}

/// Where a confined Flutter session stages its snapshot/assets. The confined
/// child exposes its only writable path as `ACOS_SESSION_ROOT`.
fn flutter_scratch() -> PathBuf {
    std::env::var_os("ACOS_SESSION_ROOT")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("ACOS_FLUTTER_SCRATCH").map(PathBuf::from))
        .unwrap_or_else(std::env::temp_dir)
        .join("flutter")
}

/// A structured failure envelope for a Flutter-session error code.
fn flutter_error(
    request_id: &str,
    session_id: Option<&str>,
    code: &str,
    message: &str,
) -> Envelope {
    let error = ProtocolError::new(
        ErrorCode::from_str_opt(code).unwrap_or(ErrorCode::Internal),
        message.to_string(),
    );
    let envelope = Envelope::failure(
        request_id,
        ProtocolBackend::AcosNative,
        Fidelity::NativeRuntime,
        error,
        Metrics::wall_ms(0),
    );
    match session_id {
        Some(id) => envelope.with_session_id(id.to_string()),
        None => envelope,
    }
}

fn flutter_session_error(
    request_id: &str,
    session_id: Option<&str>,
    err: &FlutterSessionError,
) -> Envelope {
    flutter_error(request_id, session_id, err.code(), &err.to_string())
}

/// The original `base` object, so a synthesized action request keeps the
/// caller's session id instead of falling back to `DEFAULT_SESSION`.
fn session_base(args: &Value) -> Value {
    let mut base = args
        .get("base")
        .cloned()
        .unwrap_or_else(|| json!({}));
    if let Some(object) = base.as_object_mut() {
        object
            .entry("session_id".to_string())
            .or_insert_with(|| json!(session_id(args)));
        object
            .entry("request_id".to_string())
            .or_insert_with(|| json!(request_id(args)));
    }
    base
}

fn request_id(args: &Value) -> String {
    args.pointer("/base/request_id")
        .and_then(Value::as_str)
        .unwrap_or("req_mcp")
        .to_string()
}

fn action_key_code(action: &Value) -> Option<i32> {
    let code = action.get("code")?;
    if let Some(value) = code.as_i64() {
        return Some(value as i32);
    }
    match code.as_str()?.to_ascii_lowercase().as_str() {
        "home" => Some(3),
        "back" => Some(4),
        "menu" => Some(82),
        "enter" => Some(66),
        "tab" => Some(61),
        "escape" => Some(111),
        "delete" => Some(67),
        "volume_up" => Some(24),
        "volume_down" => Some(25),
        "power" => Some(26),
        _ => None,
    }
}

fn input_envelope(
    request_id: &str,
    session_id: &str,
    result: Result<acos_runtime::session::InputReport, SessionError>,
    kind: &str,
) -> Envelope {
    match result {
        Ok(report) => {
            let mut value = json!({
                "session_id": session_id,
                "action": kind,
                "accepted": report.accepted,
                "state_changed": report.state_changed,
                "input_ack_seq": report.ack_seqs.last().copied(),
                "text_path": report.path,
            });
            if report.accepted && !report.state_changed {
                value["note"] = json!(
                    "input was delivered but the live tree did not change; the event may not \
                     have landed. Re-observe the tree before trusting the action."
                );
            } else if report.path.as_deref() == Some("key-events") {
                value["note"] = json!(
                    "no editable InputConnection was found; text was sent as key events to the \
                     focused view, so it may not have been inserted."
                );
            }
            Envelope::pass(
                request_id,
                ProtocolBackend::AcosNative,
                Fidelity::NativeRuntime,
                Metrics::wall_ms(0),
            )
            .with_session_id(session_id.to_string())
            .with_value_result(value)
            .with_capabilities(["input"])
        }
        Err(err) => session_error_envelope(request_id, Some(session_id), &err),
    }
}

fn launch_envelope(request_id: &str, session_id: &str, report: &LaunchReport) -> Envelope {
    let result = ladder_json(report);
    let mut metrics = Metrics::wall_ms(report.wall_ms);
    if report.peak_rss_bytes > 0 {
        metrics = metrics.with_peak_rss_bytes(report.peak_rss_bytes);
    }
    metrics.classes_loaded = Some(report.classes_loaded as u64);
    let envelope = match &report.root_blocker {
        None => Envelope::pass(
            request_id,
            ProtocolBackend::AcosNative,
            Fidelity::NativeRuntime,
            metrics,
        ),
        Some(blocker) => {
            let code = ErrorCode::from_str_opt(&blocker.code).unwrap_or(ErrorCode::Internal);
            Envelope::failure(
                request_id,
                ProtocolBackend::AcosNative,
                Fidelity::NativeRuntime,
                ProtocolError::new(code, blocker.message.clone())
                    .with_details(details_json(&blocker.details)),
                metrics,
            )
        }
    };
    envelope
        .with_session_id(session_id.to_string())
        .with_value_result(result)
        .with_capabilities(["apk.inspect", "lifecycle"])
}

fn ladder_json(report: &LaunchReport) -> Value {
    let stages: Vec<Value> = report
        .stages
        .iter()
        .map(|stage| {
            let status = match stage.status {
                StageStatus::Ok => "ok",
                StageStatus::Blocked => "blocked",
                StageStatus::NotReached => "not_reached",
            };
            json!({
                "stage": stage.name,
                "ok": stage.status == StageStatus::Ok,
                "status": status,
                "blocker": stage.blocker.as_ref().map(|b| json!({
                    "code": b.code,
                    "message": b.message,
                    "details": details_json(&b.details),
                })),
            })
        })
        .collect();
    json!({
        "stages": stages,
        "reached_stage": report.reached_stage,
        "package": report.package,
        "activity": report.activity,
        "classes_loaded": report.classes_loaded,
        "events": report.events,
        "wall_ms": report.wall_ms,
        "peak_rss_bytes": report.peak_rss_bytes,
    })
}

fn details_json(details: &[(String, String)]) -> Value {
    let mut map = serde_json::Map::new();
    for (key, value) in details {
        map.insert(key.clone(), Value::String(value.clone()));
    }
    Value::Object(map)
}

fn session_error_envelope(
    request_id: &str,
    session_id: Option<&str>,
    err: &SessionError,
) -> Envelope {
    let code = ErrorCode::from_str_opt(err.code).unwrap_or(ErrorCode::Internal);
    let mut error = ProtocolError::new(code, err.message.clone());
    if let Some(hint) = acos_runtime::session::session_error_hint(err.code, &err.details) {
        error = error.with_hint(hint);
    }
    if !err.details.is_empty() {
        error = error.with_details(details_json(&err.details));
    }
    let mut envelope = Envelope::failure(
        request_id,
        ProtocolBackend::AcosNative,
        Fidelity::NativeRuntime,
        error,
        Metrics::wall_ms(0),
    );
    if let Some(id) = session_id {
        envelope = envelope.with_session_id(id.to_string());
    }
    envelope
}

/// Serialize a live `ViewTree` into the `observe_tree` result object.
fn view_tree_json(tree: &ViewTree) -> Value {
    fn walk(node: &ViewNode) -> Value {
        json!({
            "id": node.id,
            "class": node.class,
            "resource_id": node.resource_id,
            "text": node.text,
            "content_desc": node.content_desc,
            "bounds": node.bounds,
            "visible": node.visible,
            "enabled": node.enabled,
            "focused": node.focused,
            "clickable": node.clickable,
            "scrollable": node.scrollable,
            "checked": node.checked,
            "package": node.package,
            "children": node.children.iter().map(walk).collect::<Vec<_>>(),
        })
    }
    json!({
        "windows": tree.windows,
        "truncated": tree.truncated,
        "max_nodes": tree.max_nodes,
        "nodes": tree.nodes.iter().map(walk).collect::<Vec<_>>(),
    })
}

fn invalid_argument(request_id: &str, field: &str, reason: &str) -> Envelope {
    Envelope::failure(
        request_id,
        ProtocolBackend::AcosNative,
        Fidelity::NativeRuntime,
        ProtocolError::new(
            ErrorCode::InvalidArgument,
            format!("invalid argument `{field}`: {reason}"),
        )
        .with_details(json!({"field": field, "reason": reason})),
        Metrics::wall_ms(0),
    )
}

fn unsafe_path(request_id: &str, path: &str, reason: &str) -> Envelope {
    Envelope::failure(
        request_id,
        ProtocolBackend::AcosNative,
        Fidelity::NativeRuntime,
        ProtocolError::new(ErrorCode::UnsafePath, reason.to_string())
            .with_details(json!({"path": path})),
        Metrics::wall_ms(0),
    )
}

fn unsupported(request_id: &str, capability: &str, message: &str) -> Envelope {
    let mut error = ProtocolError::new(ErrorCode::UnsupportedBackend, message)
        .with_details(json!({"capability": capability}));
    let owned = vec![("capability".to_string(), capability.to_string())];
    if let Some(hint) = acos_runtime::session::session_error_hint("UNSUPPORTED_BACKEND", &owned) {
        error = error.with_hint(hint);
    }
    Envelope::failure(
        request_id,
        ProtocolBackend::AcosNative,
        Fidelity::NativeRuntime,
        error,
        Metrics::wall_ms(0),
    )
}

/// A re-exec'd `acos-mcp --confined-child` process that owns one native
/// session. It entered the ADR-0022 confinement before creating any thread, so
/// the JNI loader's `confinement_active()` gate is satisfied.
struct ConfinedChild {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: i64,
    /// The VFS root created for this child (removed on shutdown).
    root: PathBuf,
}

impl ConfinedChild {
    /// Spawn and initialize a confined child with the given VFS root and read
    /// roots. The child confines itself before serving.
    fn spawn(
        binary: &std::path::Path,
        read_write: &std::path::Path,
        read_only: &[PathBuf],
        flutter_lib: Option<&std::path::Path>,
    ) -> Result<Self, String> {
        let mut command = Command::new(binary);
        command.arg("--confined-child").arg("--rw").arg(read_write);
        for path in read_only {
            command.arg("--ro").arg(path);
        }
        if let Some(lib) = flutter_lib {
            command.arg("--flutter-lib").arg(lib);
            // Dart's compressed-pointer heap reserves a multi-GiB virtual
            // region up front; the 4 GiB guest default is too small and the VM
            // aborts in `virtual_memory_compressed.cc`. Raise it for Flutter
            // children (the W130-A harness runs the engine under an 8 GiB
            // address-space budget).
            command
                .arg("--as-bytes")
                .arg((16u64 * 1024 * 1024 * 1024).to_string());
        }
        command
            .env("ACOS_MCP_QUIET", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        let mut child = command.spawn().map_err(|e| {
            format!(
                "spawn confined child {}: {e}",
                binary.display()
            )
        })?;
        let stdin = child.stdin.take().ok_or("confined child has no stdin")?;
        let stdout = child.stdout.take().ok_or("confined child has no stdout")?;
        let mut this = ConfinedChild {
            child,
            stdin,
            stdout: BufReader::new(stdout),
            next_id: 0,
            root: read_write.to_path_buf(),
        };
        this.request(
            "initialize",
            json!({
                "protocolVersion": crate::LATEST_PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": {"name": "acos-mcp-parent", "version": crate::SERVER_VERSION},
            }),
        )?;
        this.notify("notifications/initialized", json!({}));
        Ok(this)
    }

    fn notify(&mut self, method: &str, params: Value) {
        let frame = json!({"jsonrpc": "2.0", "method": method, "params": params});
        let _ = writeln!(self.stdin, "{frame}");
        let _ = self.stdin.flush();
    }

    fn request(&mut self, method: &str, params: Value) -> Result<Value, String> {
        self.next_id += 1;
        let id = self.next_id;
        let frame = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        writeln!(self.stdin, "{frame}").map_err(|e| format!("write to confined child: {e}"))?;
        self.stdin
            .flush()
            .map_err(|e| format!("flush confined child: {e}"))?;
        loop {
            // The child runs arbitrary guest native code and owns fd 1, so a
            // hostile library can emit an unbounded line. `read_line` into a
            // `String` would let it OOM the *parent* (every session); cap the
            // frame instead (W94-A #6).
            let mut line = Vec::new();
            let n = self
                .stdout
                .by_ref()
                .take(MAX_CHILD_FRAME + 1)
                .read_until(b'\n', &mut line)
                .map_err(|e| format!("read from confined child: {e}"))?;
            if n == 0 {
                return Err("confined child closed stdout".to_string());
            }
            if n as u64 > MAX_CHILD_FRAME {
                return Err(format!(
                    "confined child sent a frame larger than {MAX_CHILD_FRAME} bytes; \
                     refusing to buffer it in the MCP parent"
                ));
            }
            let text = std::str::from_utf8(&line)
                .map_err(|e| format!("confined child sent non-UTF-8: {e}"))?;
            let value: Value = serde_json::from_str(text)
                .map_err(|e| format!("confined child sent invalid JSON: {e}"))?;
            if value.get("id").and_then(Value::as_i64) == Some(id) {
                return Ok(value);
            }
        }
    }

    fn call_tool(&mut self, name: &str, args: &Value) -> Envelope {
        let request_id = request_id(args);
        let id = session_id(args);
        let response = match self.request(
            "tools/call",
            json!({"name": name, "arguments": args}),
        ) {
            Ok(response) => response,
            Err(e) => return native_child_error(&request_id, &id, "INTERNAL", &e),
        };
        let envelope = response
            .pointer("/result/structuredContent")
            .cloned()
            .or_else(|| {
                response
                    .pointer("/result/content/0/text")
                    .and_then(Value::as_str)
                    .and_then(|text| serde_json::from_str(text).ok())
            });
        match envelope.map(serde_json::from_value::<Envelope>) {
            Some(Ok(envelope)) => envelope,
            Some(Err(e)) => {
                native_child_error(&request_id, &id, "INTERNAL", &format!("bad child envelope: {e}"))
            }
            None => native_child_error(
                &request_id,
                &id,
                "INTERNAL",
                "confined child returned no ACOS envelope",
            ),
        }
    }

    fn shutdown(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        remove_private_dir(&self.root);
    }
}

/// A transport failure while forwarding to the session daemon.
fn daemon_error(request_id: &str, args: &Value, message: &str) -> Envelope {
    native_child_error(request_id, &session_id(args), "SESSION_NOT_FOUND", message)
}

fn native_child_error(
    request_id: &str,
    session_id: &str,
    code: &str,
    message: &str,
) -> Envelope {
    let code = ErrorCode::from_str_opt(code).unwrap_or(ErrorCode::Internal);
    Envelope::failure(
        request_id,
        ProtocolBackend::AcosNative,
        Fidelity::NativeRuntime,
        ProtocolError::new(code, message.to_string()),
        Metrics::wall_ms(0),
    )
    .with_session_id(session_id.to_string())
}

/// Largest framed JSON message the parent will buffer from a confined child.
const MAX_CHILD_FRAME: u64 = 8 * 1024 * 1024;

/// The VFS root for one native child (W94-A #5).
///
/// `ACOS_SESSION_ROOT` wins so a caller can pin it. Otherwise a `mkdtemp`-style
/// directory is created: an unpredictable name (kernel `getrandom`), created
/// atomically with `O_EXCL` semantics via `create_dir` so a pre-created symlink
/// or directory makes creation fail instead of being followed, and mode `0700`
/// so no other user can read or swap the app data underneath the session.
fn native_session_root(seq: u64) -> std::io::Result<PathBuf> {
    if let Some(root) = std::env::var_os("ACOS_SESSION_ROOT") {
        let root = PathBuf::from(root);
        create_private_dir(&root)?;
        return Ok(root);
    }
    let base = std::env::temp_dir();
    let mut last =
        std::io::Error::other("could not create a private native session root");
    for attempt in 0..64u32 {
        let name = format!(
            "acos-mcp-native-{}-{seq}-{:016x}",
            std::process::id(),
            child_random_u64() ^ u64::from(attempt).wrapping_mul(0x9e37_79b9_7f4a_7c15)
        );
        let path = base.join(name);
        match create_private_dir(&path) {
            Ok(()) => return Ok(path),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => last = e,
            Err(e) => return Err(e),
        }
    }
    Err(last)
}

/// Create `path` as a fresh private directory (`0700`).
///
/// `create_dir` (not `create_dir_all`) is deliberate: if `path` already exists —
/// including as a symlink an attacker planted — the call fails with
/// `AlreadyExists` rather than following it. `symlink_metadata` then re-checks
/// the result is a real directory, never a link.
fn create_private_dir(path: &std::path::Path) -> std::io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    if path.exists() {
        let meta = std::fs::symlink_metadata(path)?;
        if !meta.is_dir() || meta.file_type().is_symlink() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                format!("{} exists and is not a real directory", path.display()),
            ));
        }
        std::fs::set_permissions(path, std::os::unix::fs::PermissionsExt::from_mode(0o700))?;
        return Ok(());
    }
    std::fs::DirBuilder::new().mode(0o700).create(path)
}

/// Remove a session root without ever following a symlink.
fn remove_private_dir(path: &std::path::Path) {
    if let Ok(meta) = std::fs::symlink_metadata(path) {
        if meta.is_dir() && !meta.file_type().is_symlink() {
            let _ = std::fs::remove_dir_all(path);
        } else {
            // A link/file where our root should be: remove the link itself, not
            // its target.
            let _ = std::fs::remove_file(path);
        }
    }
}

/// 64 unpredictable bits from the kernel CSPRNG, with a time/pid fallback.
fn child_random_u64() -> u64 {
    let mut buf = [0u8; 8];
    // SAFETY: `buf` is 8 writable bytes and `getrandom` writes at most that.
    let n = unsafe { libc::getrandom(buf.as_mut_ptr().cast(), buf.len(), 0) };
    if n == buf.len() as isize {
        return u64::from_le_bytes(buf);
    }
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos() as u64);
    nanos
        ^ u64::from(std::process::id()).rotate_left(32)
        ^ (&buf as *const _ as u64)
}

/// The boot-classpath cache directory (`ACOS_BOOT_DIR`, then walking up from
/// the working directory, then `~/.cache/acos/boot/android-34`). Mirrors
/// `acos_vm::default_boot_dir` so the child's Landlock rule names the real
/// resolved tree.
fn native_boot_root() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("ACOS_BOOT_DIR") {
        let path = PathBuf::from(dir);
        if path.is_dir() {
            return Some(path);
        }
    }
    let mut dir = std::env::current_dir().ok()?;
    loop {
        let candidate = dir.join(".cache/boot/android-34");
        if candidate.is_dir() {
            return Some(candidate);
        }
        if !dir.pop() {
            break;
        }
    }
    if let Some(home) = std::env::var_os("HOME") {
        let candidate = PathBuf::from(home).join(".cache/acos/boot/android-34");
        if candidate.is_dir() {
            return Some(candidate);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn observe_without_a_frame_is_not_ready() {
        let backend = SessionBackend::with_cli(
            CliBackend::new(BackendConfig::for_binary("/nonexistent/acos")),
            SessionOptions::default(),
        );
        let ctx = CallContext {
            cancel: crate::CancelToken::new(),
            deadline: std::time::Instant::now() + Duration::from_secs(5),
            budget: Default::default(),
            allowed_roots: vec![std::env::current_dir().unwrap()],
            artifacts_dir: std::env::current_dir().unwrap(),
            max_arg_chars: 4096,
            max_output_bytes: 4096,
        };
        let envelope = backend.call(
            CommandName::Observe,
            &json!({"base": {"request_id": "req_1", "session_id": "s_test"}}),
            &ctx,
        );
        assert_eq!(envelope.status(), acos_protocol::Status::Unsupported);
        assert_eq!(
            envelope.error().unwrap().details.as_ref().unwrap()["reason"],
            "not_ready"
        );
    }

    #[test]
    fn logs_on_an_unlaunched_session_carry_the_stage() {
        let backend = SessionBackend::with_cli(
            CliBackend::new(BackendConfig::for_binary("/nonexistent/acos")),
            SessionOptions::default(),
        );
        let envelope = backend.call_logs("req_1", &json!({"base": {"session_id": "s_test"}}));
        assert_eq!(envelope.status(), acos_protocol::Status::Pass);
        let result = envelope.result().expect("result");
        assert_eq!(result["launched"], false);
        assert_eq!(result["stage_reached"], "none");
        assert!(result["note"].as_str().unwrap().contains("onCreate"));
    }

    #[test]
    fn session_root_is_private_unpredictable_and_symlink_safe() {
        use std::os::unix::fs::PermissionsExt;
        let a = native_session_root(1).expect("root a");
        let b = native_session_root(2).expect("root b");
        assert_ne!(a, b, "each session must get a fresh root");
        for p in [&a, &b] {
            let meta = std::fs::metadata(p).expect("root metadata");
            assert_eq!(
                meta.permissions().mode() & 0o777,
                0o700,
                "the session root must not be world-traversable"
            );
        }
        remove_private_dir(&a);
        remove_private_dir(&b);
        assert!(!a.exists() && !b.exists());

        // A pre-planted symlink must make creation fail, not be followed.
        let base = std::env::temp_dir().join(format!(
            "acos-mcp-w97a-{}-{:x}",
            std::process::id(),
            child_random_u64()
        ));
        std::fs::create_dir_all(&base).expect("base");
        let target = base.join("target");
        std::fs::create_dir(&target).expect("target");
        let link = base.join("link");
        std::os::unix::fs::symlink(&target, &link).expect("symlink");
        let err = create_private_dir(&link).expect_err("symlink must be refused");
        assert_eq!(err.kind(), std::io::ErrorKind::AlreadyExists);
        remove_private_dir(&link);
        assert!(target.is_dir(), "removal must not follow the symlink");
        assert!(!link.exists(), "the link itself is removed");
        remove_private_dir(&base);
    }

    #[test]
    fn oversized_child_frame_is_rejected_by_the_parent() {
        let mut child = Command::new("/bin/sh")
            .arg("-c")
            .arg("head -c 9000000 /dev/zero | tr '\\0' 'a'")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn oversized writer");
        let stdin = child.stdin.take().expect("stdin");
        let stdout = BufReader::new(child.stdout.take().expect("stdout"));
        let mut confined = ConfinedChild {
            child,
            stdin,
            stdout,
            next_id: 0,
            root: PathBuf::new(),
        };
        let err = confined
            .request("tools/call", json!({}))
            .expect_err("an unbounded child line must be rejected");
        assert!(
            err.contains("larger than"),
            "expected the frame-size rejection, got {err}"
        );
        let _ = confined.child.kill();
        let _ = confined.child.wait();
    }

    /// A tap that is accepted but does not visibly change anything carries a
    /// note, so `accepted: true` is not read as "the click landed".
    #[test]
    fn a_no_op_action_carries_a_note() {
        use acos_runtime::session::InputReport;
        let no_op = InputReport {
            kind: "tap".to_string(),
            accepted: true,
            ack_seqs: vec![2],
            path: None,
            state_changed: false,
        };
        let envelope = input_envelope("req_1", "s_test", Ok(no_op), "tap");
        let value = serde_json::to_value(&envelope).expect("serialize");
        assert_eq!(value["status"], "pass");
        let note = value["result"]["note"].as_str().expect("note present");
        assert!(note.contains("may not have landed"), "{note}");

        let fallback = InputReport {
            kind: "text".to_string(),
            accepted: true,
            ack_seqs: Vec::new(),
            path: Some("key-events".to_string()),
            state_changed: true,
        };
        let envelope = input_envelope("req_2", "s_test", Ok(fallback), "text");
        let value = serde_json::to_value(&envelope).expect("serialize");
        let note = value["result"]["note"].as_str().expect("note present");
        assert!(note.contains("key events"), "{note}");
    }
}
