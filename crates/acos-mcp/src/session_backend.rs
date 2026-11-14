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
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

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
use crate::private_daemon::PrivateDaemon;
use crate::flutter_backend::{self, FlutterSessions};
use crate::rn_backend::{self, RnSessions};
use acos_jni::FlutterSessionError;
use acos_runtime::RnSessionError;

/// Commands the session backend serves itself (bypassing the CLI capability
/// probe, which cannot see them).
const SESSION_COMMANDS: &[CommandName] = &[
    CommandName::Run,
    CommandName::Observe,
    CommandName::Action,
    CommandName::Reset,
    CommandName::Destroy,
];

/// Teardown handle for this server's private ART daemon.
#[derive(Clone)]
pub struct ArtDaemonHandle(std::sync::Arc<Mutex<Option<PrivateDaemon>>>);

impl ArtDaemonHandle {
    /// Destroy held sessions, terminate the daemon and reap it (idempotent).
    pub fn shutdown(&self) {
        if let Some(daemon) = self.0.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
            daemon.shutdown();
        }
    }

    /// The daemon's pid, when it has been spawned.
    #[must_use]
    pub fn pid(&self) -> Option<u32> {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map(PrivateDaemon::pid)
    }
}

/// FB5: how long a session may sit **untouched** before this server destroys it.
///
/// Every MCP session call on an owned session renews the idle clock; an idle
/// session holds up to 2 GiB of cgroup and a forgotten guest makes the next
/// launch hit `HOST_MEMORY_LOW`, so the server reclaims its own.
pub const DEFAULT_IDLE_TTL_S: u64 = 600;

/// Environment override for [`DEFAULT_IDLE_TTL_S`] (seconds; `0` disables).
pub const IDLE_TTL_ENV: &str = "ACOS_SESSION_IDLE_TTL_S";

/// One session **this** MCP server launched, with the bookkeeping the hygiene
/// rules need (FB5): the launch token (SERVELEAK2/F3 ownership proof), the
/// agent's `label`, when it was launched, when it was last used, and the idle
/// TTL that applies to it.
#[derive(Debug, Clone)]
struct OwnedSession {
    /// Launch token the daemon returned for this launch (empty if it reports
    /// none); the proof that the id still names the guest *this* server started.
    token: String,
    /// Optional agent-supplied label (`acos_launch {label}`), empty when unset.
    label: String,
    launched_at: Instant,
    last_used: Instant,
    /// Seconds of idleness before auto-destroy; `0` means never.
    idle_ttl_s: u64,
    /// Unique per launch record (FB5b/M1): an in-flight guard only touches the
    /// record it started on, never a relaunch that replaced it.
    seq: u64,
    /// Calls currently forwarded on this session (FB5b/M1). A session with a
    /// call in flight is never due.
    in_flight: u32,
    /// Set by the sweep, under the `owned` lock, right before it destroys the
    /// guest (FB5b/M1). Calls arriving now get the `idle_ttl_expired` reason.
    expiring: bool,
}

/// Marks a call in flight on an owned session for its whole duration (FB5b/M1):
/// the idle clock is renewed when the call starts *and* when it ends.
struct InFlight<'a> {
    backend: &'a SessionBackend,
    id: String,
    seq: u64,
}

impl Drop for InFlight<'_> {
    fn drop(&mut self) {
        let mut owned = self.backend.owned.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(session) = owned.get_mut(&self.id) {
            if session.seq == self.seq {
                session.in_flight = session.in_flight.saturating_sub(1);
                session.last_used = Instant::now();
            }
        }
    }
}

/// A session this server destroyed because it idled past its TTL (FB5).
///
/// A later call on that id must say **why** it is gone (an agent that parked a
/// session for 12 minutes needs to know it expired, not that its id is unknown).
#[derive(Debug, Clone)]
struct ExpiredSession {
    idle_ttl_s: u64,
    expired_at: Instant,
    label: String,
}

/// Whether a forwarded call may replace a dead private daemon (FB5).
///
/// Teardown must not: a fresh daemon has no session to destroy, so restarting
/// one during shutdown only leaves a new process behind — the exact leak
/// SERVELEAK removed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Restart {
    /// Replace a dead private daemon once and retry (normal tool calls).
    OnFailure,
    /// Never respawn (teardown, idle expiry: the guest is gone either way).
    Never,
}

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
    /// The `acos` binary to spawn as this server's private ART daemon, when the
    /// default engine is requested (see [`crate::private_daemon`]).
    private_acos: Option<PathBuf>,
    /// The lazily spawned private ART daemon (one per server process).
    private: std::sync::Arc<Mutex<Option<PrivateDaemon>>>,
    /// Flutter sessions owned by this process (the confined child).
    flutter: FlutterSessions,
    /// React Native sessions owned by this process (the confined child).
    rn: RnSessions,
    /// Session ids this process launched (SERVELEAK). `shutdown` destroys
    /// exactly these and nothing a different client created.
    ///
    /// SERVELEAK2/F3: the value is the **launch token** the daemon returned for
    /// that launch (empty when the daemon is too old to report one). Ownership
    /// of a session id is not ownership of a *guest*: two MCP servers pointed at
    /// the same daemon with the same id meant the second launch replaced the
    /// first, and the first server's stdin EOF then destroyed the second's live
    /// guest. `shutdown` re-reads the token from the daemon and destroys only a
    /// session whose current token is still the one this server launched.
    owned: Mutex<HashMap<String, OwnedSession>>,
    /// Sessions this server reclaimed after their idle TTL (FB5). Bounded and
    /// cleared by the next launch of the same id.
    expired: Mutex<HashMap<String, ExpiredSession>>,
    /// Set once `shutdown` began: the idle reaper stops, and a session call may
    /// no longer spawn a daemon or start a fresh in-process session.
    stopping: AtomicBool,
    /// Source of [`OwnedSession::seq`].
    next_seq: AtomicU64,
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
            private_acos: None,
            private: std::sync::Arc::new(Mutex::new(None)),
            flutter: FlutterSessions::default(),
            rn: RnSessions::default(),
            owned: Mutex::new(HashMap::new()),
            expired: Mutex::new(HashMap::new()),
            stopping: AtomicBool::new(false),
            next_seq: AtomicU64::new(1),
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
            private_acos: None,
            private: std::sync::Arc::new(Mutex::new(None)),
            flutter: FlutterSessions::default(),
            rn: RnSessions::default(),
            owned: Mutex::new(HashMap::new()),
            expired: Mutex::new(HashMap::new()),
            stopping: AtomicBool::new(false),
            next_seq: AtomicU64::new(1),
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

    /// Make every session tool run on real ART (the default engine).
    ///
    /// The first session tool call lazily spawns this server's **own**
    /// `acos serve --engine art` on a private socket and forwards to it; the
    /// daemon is a child of this process and is torn down (sessions destroyed,
    /// `SIGTERM`, reaped) when it exits. `ACOS_SESSION_DAEMON=1` still wins and
    /// connects to the external daemon instead.
    #[must_use]
    pub fn with_art_daemon(mut self, acos: PathBuf) -> Self {
        self.private_acos = Some(acos);
        self
    }

    /// The private ART daemon's socket, once it has been spawned.
    #[must_use]
    pub fn private_daemon_socket(&self) -> Option<PathBuf> {
        self.private
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map(|daemon| daemon.socket().to_path_buf())
    }

    /// Destroy the sessions **this process launched**, then terminate and reap
    /// its private ART daemon.
    ///
    /// This is the one teardown: `Server::serve` reaches it through
    /// [`Backend::shutdown`] on stdin EOF (SERVELEAK) and `main` reaches it
    /// through [`Self::art_daemon_handle`]; both stop the daemon *after* the
    /// owned sessions were destroyed through it, so the destroy actually reaches
    /// the ART guest (a daemon that was SIGTERMed first can no longer answer
    /// `acos_destroy`).
    pub fn shutdown(&self) {
        // FB5: no new daemon, no new in-process session, and no idle reaper
        // racing the teardown.
        self.stopping.store(true, Ordering::SeqCst);
        self.destroy_owned_sessions();
        let stopped = self
            .private
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        if let Some(mut daemon) = stopped {
            daemon.shutdown();
        }
    }

    /// Destroy every session this process launched, keeping nothing back.
    ///
    /// SERVELEAK: each owned session is destroyed through its own engine — the
    /// daemon's `acos_destroy` (which kills and proves the ART guest tree) or the
    /// in-process one — so a client that closes stdin never leaves a live guest
    /// behind. Failures are logged, not ignored silently; the process is exiting
    /// either way.
    ///
    /// SERVELEAK2/F3: a session **id** is not a guest. Two servers against one
    /// daemon that used the same id (the old shared `DEFAULT_SESSION`) meant the
    /// second launch replaced the first, and the first server's EOF then
    /// destroyed the second's live guest. Ownership is therefore checked against
    /// the launch token the daemon reports *now*: a session whose token changed
    /// belongs to a different launch and is left alone. This applies to **both**
    /// daemons (external and private): a private daemon reports tokens too, and
    /// an id in it can only be ours by construction, which is exactly what the
    /// token check re-proves.
    fn destroy_owned_sessions(&self) {
        let ids: Vec<(String, String)> = {
            let mut owned = self.owned.lock().unwrap_or_else(|e| e.into_inner());
            owned
                .drain()
                .map(|(id, session)| (id, session.token))
                .collect()
        };
        if ids.is_empty() {
            return;
        }
        let ctx = teardown_context();
        // FB5 (SERVELEAK3 review finding 3): a **listing failure** is not the
        // same as an empty list. On the external daemon the id-only fallback is
        // exactly the unsafe guess SERVELEAK2/F3 removed (another client's
        // launch may hold the id now), so refuse it and log the leak instead.
        // The private daemon is different: its ids can only be ours.
        let current = match self.daemon_sessions(&ctx, Restart::Never) {
            Some(sessions) => Some(launch_tokens(&sessions)),
            None if self.daemon.is_some() => {
                let leaked: Vec<&str> = ids.iter().map(|(id, _)| id.as_str()).collect();
                eprintln!(
                    "acos-mcp: session_list failed on the external daemon; cannot prove \
                     ownership of {} session(s), leaving them (leak): {}",
                    leaked.len(),
                    leaked.join(", ")
                );
                return;
            }
            None => None,
        };
        for (id, token) in ids {
            // A daemon that lists its sessions is authoritative: destroy only a
            // session that still carries the token this server launched.
            if let Some(current) = current.as_ref() {
                match current.get(&id) {
                    Some(current_token) if token.is_empty() || *current_token == token => {}
                    Some(_) => {
                        eprintln!(
                            "acos-mcp: session {id} was launched again by another client; \
                             not destroying it on exit"
                        );
                        continue;
                    }
                    None => {
                        eprintln!("acos-mcp: session {id} is already gone; nothing to destroy");
                        continue;
                    }
                }
            }
            let args = json!({"base": {"request_id": "req_shutdown", "session_id": id}});
            let envelope =
                self.destroy_on_daemon(&args, &ctx, Restart::Never);
            match envelope {
                Some(envelope) if envelope.ok() => {
                    eprintln!("acos-mcp: destroyed session {id} on exit")
                }
                Some(envelope) => eprintln!(
                    "acos-mcp: session {id} teardown reported {} on exit",
                    envelope
                        .error()
                        .map(|e| e.code.as_str().to_string())
                        .unwrap_or_else(|| "a non-pass reply".to_string())
                ),
                None => eprintln!("acos-mcp: session {id} could not be destroyed on exit"),
            }
        }
    }

    /// A handle that survives `self` moving into the server, so the process can
    /// tear the daemon down after stdin EOF.
    #[must_use]
    pub fn art_daemon_handle(&self) -> ArtDaemonHandle {
        ArtDaemonHandle(std::sync::Arc::clone(&self.private))
    }

    /// Forward a session tool to the external daemon, else to the private ART
    /// daemon. `None` means "serve it in-process" (the legacy engine).
    ///
    /// **Merge note (SERVELEAK3):** this is the *one* forwarding path. SERVELEAK
    /// tracked the sessions it launched for the external daemon and REL3a added
    /// the private daemon as a second forwarding branch; here both branches go
    /// through [`Self::forward_to_daemon`], so every forwarded `acos_launch` is
    /// recorded as owned (with the daemon's launch token) no matter which daemon
    /// served it.
    fn forward_session_tool(&self, tool: &str, args: &Value) -> Option<Envelope> {
        self.forward_session_tool_opts(tool, args, Restart::OnFailure)
    }

    /// [`Self::forward_session_tool`] with the restart policy explicit (FB5:
    /// teardown and idle expiry never respawn a dead daemon).
    fn forward_session_tool_opts(
        &self,
        tool: &str,
        args: &Value,
        restart: Restart,
    ) -> Option<Envelope> {
        // Fill the default identity here, not in the daemon: the default session
        // id is unique per MCP server process (`mcp-<pid>-<rand>`), and a shared
        // external daemon must never invent one for us.
        let mut normalized = args.clone();
        if let Some(object) = normalized.as_object_mut() {
            let mut base = session_base(args);
            // Older clients (and `tools/release/mcp_smoke.py`'s first form) sent
            // `session_id` at the top level; accept it as `base.session_id`
            // rather than silently addressing the default session.
            if let (Some(top), Some(map)) = (
                args.get("session_id").and_then(Value::as_str),
                base.as_object_mut(),
            ) {
                map.insert("session_id".to_string(), json!(top));
            }
            object.insert("base".to_string(), base);
        }
        let args = &normalized;
        // FB5b/H1: a listing is about the *daemon*, not about any session of
        // ours. It must never hit the default session's tombstone and never
        // renew its idle clock.
        if tool == crate::daemon::TOOL_LIST {
            return self.forward_to_daemon_opts(tool, args, restart);
        }
        let id = session_id(args);
        // FB5: a session this server reclaimed for idleness answers with the
        // reason, on the very tool the agent reaches for next.
        if tool != "acos_launch" {
            if let Some(envelope) = self.expired_envelope(&id, &request_id(args)) {
                return Some(envelope);
            }
        }
        // FB5b/M1: mark the call in flight BEFORE forwarding, so a slow call is
        // never mistaken for idleness; a session already being reclaimed answers
        // with the expiry reason (a launch waits for the reclaim to finish).
        let _in_flight = match self.begin_call(&id, tool == "acos_launch") {
            Ok(guard) => guard,
            Err(expiring) => {
                return Some(expired_envelope_for(&id, &request_id(args), &expiring));
            }
        };
        let mut envelope = self.forward_to_daemon_opts(tool, args, restart)?;
        // SERVELEAK: remember what this process launched *in a daemon*, so stdin
        // EOF can destroy exactly those sessions (a daemon outlives this process
        // by design; its guests must not outlive their purpose). SERVELEAK2/F3:
        // the launch token makes ownership provable, not just a name.
        if tool == "acos_launch" && envelope.ok() {
            let effective_ttl = self.remember_owned(&id, args, &launch_token(&envelope));
            annotate_launch(&mut envelope, args, effective_ttl);
        } else if tool == "acos_destroy" && envelope.ok() {
            // Gone by request: it must not be listed as a live session (FB5),
            // and it is not "expired" either.
            self.remove_owned(&id);
            self.expired
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&id);
        } else if tool != "acos_launch" {
            self.touch(&id);
        }
        Some(envelope)
    }

    /// Begin a forwarded call on `id` (FB5b/M1). Renews the idle clock and
    /// counts the call in flight for an owned session. `Err(())` means the sweep
    /// is destroying the session right now (callers answer with the expiry
    /// reason); a launch instead waits (bounded) for the reclaim to finish.
    fn begin_call(
        &self,
        id: &str,
        is_launch: bool,
    ) -> Result<Option<InFlight<'_>>, ExpiredSession> {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            {
                let mut owned = self.owned.lock().unwrap_or_else(|e| e.into_inner());
                match owned.get_mut(id) {
                    None => return Ok(None),
                    Some(session) if !session.expiring => {
                        session.in_flight += 1;
                        session.last_used = Instant::now();
                        return Ok(Some(InFlight {
                            backend: self,
                            id: id.to_string(),
                            seq: session.seq,
                        }));
                    }
                    Some(session) => {
                        if !is_launch || Instant::now() >= deadline {
                            return Err(ExpiredSession {
                                idle_ttl_s: session.idle_ttl_s,
                                expired_at: Instant::now(),
                                label: session.label.clone(),
                            });
                        }
                    }
                }
            }
            std::thread::sleep(Duration::from_millis(25));
        }
    }

    /// Destroy one session through whichever daemon owns it, honouring the
    /// restart policy (FB5).
    fn destroy_on_daemon(&self, args: &Value, ctx: &CallContext, restart: Restart) -> Option<Envelope> {
        let _ = ctx;
        // FB5b/M1: raw forward — the reaper's own destroy must bypass the
        // tombstone / in-flight bookkeeping it is itself the author of.
        let mut normalized = args.clone();
        if let Some(object) = normalized.as_object_mut() {
            object.insert("base".to_string(), session_base(args));
        }
        self.forward_to_daemon_opts("acos_destroy", &normalized, restart)
    }

    /// Forward one call to the external daemon when one is configured, else to
    /// this server's private ART daemon (spawned lazily).
    /// [`Self::forward_to_daemon`] with the restart policy explicit.
    ///
    /// `Restart::Never` is the teardown/expiry rule (FB5, SERVELEAK3 review
    /// finding 2): a daemon that died took every guest with it, so respawning
    /// one to destroy a session only leaves a *new* daemon behind — the leak
    /// teardown exists to prevent. It also never spawns a daemon that is not
    /// already running, and it never falls through to the in-process backend
    /// (which would create a fresh session to "destroy").
    fn forward_to_daemon_opts(
        &self,
        tool: &str,
        args: &Value,
        restart: Restart,
    ) -> Option<Envelope> {
        if let Some(daemon) = &self.daemon {
            let mut client = daemon.lock().unwrap_or_else(|e| e.into_inner());
            return Some(match client.call(tool, args) {
                Ok(envelope) => envelope,
                Err(e) => daemon_error(&request_id(args), args, &e.to_string()),
            });
        }
        let acos = self.private_acos.clone()?;
        let mut slot = self.private.lock().unwrap_or_else(|e| e.into_inner());
        if slot.is_none() && restart == Restart::Never {
            // Nothing is running and teardown may not start anything: the
            // sessions it held died with the daemon, and answering here keeps
            // the caller from falling through to the in-process backend.
            return Some(private_daemon_error(
                &request_id(args),
                args,
                "the private ART daemon is not running; nothing to restart during teardown",
            ));
        }
        if slot.is_none() {
            match PrivateDaemon::spawn(&acos, crate::private_daemon::ENGINE_ART) {
                Ok(daemon) => {
                    eprintln!(
                        "acos-mcp: private ART session daemon pid {} on {} (engine {})",
                        daemon.pid(),
                        daemon.socket().display(),
                        daemon.engine()
                    );
                    *slot = Some(daemon);
                }
                Err(e) => {
                    return Some(private_daemon_error(
                        &request_id(args),
                        args,
                        &format!(
                            "start `{} serve --engine art`: {e}",
                            acos.display()
                        ),
                    ))
                }
            }
        }
        let daemon = slot.as_mut().expect("spawned above");
        match daemon.call(tool, args) {
            Ok(envelope) => Some(envelope),
            Err(first) => {
                if restart == Restart::Never {
                    // FB5: teardown never respawns a dead private daemon. The
                    // session it held is gone with it, so there is nothing left
                    // to destroy and a new daemon would be a new leak.
                    eprintln!(
                        "acos-mcp: private ART daemon call failed during teardown ({first}); \
                         not restarting it"
                    );
                    daemon.shutdown();
                    *slot = None;
                    return Some(private_daemon_error(
                        &request_id(args),
                        args,
                        "the private ART daemon died during teardown; its sessions are gone \
                         with it and it is not restarted",
                    ));
                }
                // A private daemon that died (idle exit, guest OOM) is replaced
                // once: the session is gone either way, and the retry keeps a
                // long-lived agent server usable without a restart.
                eprintln!("acos-mcp: private ART daemon call failed ({first}); restarting it");
                daemon.shutdown();
                *slot = None;
                match PrivateDaemon::spawn(&acos, crate::private_daemon::ENGINE_ART) {
                    Ok(mut fresh) => {
                        let result = fresh.call(tool, args);
                        *slot = Some(fresh);
                        Some(match result {
                            Ok(envelope) => envelope,
                            Err(e) => {
                                daemon_error(&request_id(args), args, &e.to_string())
                            }
                        })
                    }
                    Err(e) => Some(private_daemon_error(
                        &request_id(args),
                        args,
                        &format!("restart `{} serve --engine art`: {e}", acos.display()),
                    )),
                }
            }
        }
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
        local + native + self.flutter.len() + self.rn.len()
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
        // React Native sessions likewise report through their worker channel.
        {
            for id in self.rn.ids() {
                match self.rn.status(&id) {
                    Ok(status) => out.push(json!({
                        "session_id": id,
                        "launched": true,
                        "package": "",
                        "stage_reached": "ui_tree",
                        "published_frames": 0,
                        "nodes": status.nodes,
                        "launch_ms": status.launch_ms,
                        "apk": status.apk.display().to_string(),
                        "native": true,
                        "react_native": true,
                    })),
                    Err(e) => out.push(json!({
                        "session_id": id,
                        "launched": false,
                        "stage_reached": "unknown",
                        "published_frames": 0,
                        "native": true,
                        "react_native": true,
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
            // W189-A: the engine wraps native memory in `NewDirectByteBuffer`
            // for the semantics stream (`FlutterJNI.updateSemantics`). ACOS
            // validates the caller-owned range against `/proc/self/maps`, which
            // Landlock denies unless the confined child's own proc dir is
            // granted. Without it the first real-widget semantics update throws
            // `IllegalArgumentException` inside `PlatformViewAndroidJNI` and
            // aborts the engine. Grant the child's own `/proc/self` (read-only)
            // only: the traversal chain adds EXECUTE on `/proc`, nothing else.
            read_only.push(PathBuf::from("/proc/self"));
        }

        // A React Native APK extracts its runtime libraries from the APK into
        // the child's writable root; only the Java driver DEX must be readable.
        let mut rn_dex: Option<PathBuf> = None;
        if rn_backend::is_react_native(apk) {
            let dex = rn_backend::default_driver_dex();
            if let Some(parent) = dex.parent() {
                read_only.push(parent.to_path_buf());
            }
            rn_dex = Some(dex);
            // Hermes reads the system RNG at init; the confined child has no
            // `/dev` grant by default (the W157-A harness granted these too).
            for dev in ["/dev/urandom", "/dev/null"] {
                if std::path::Path::new(dev).exists() {
                    read_only.push(PathBuf::from(dev));
                }
            }
        }

        let mut child = match ConfinedChild::spawn(
            &self.child_binary,
            &root,
            &read_only,
            flutter_lib.as_deref(),
            rn_dex.as_deref(),
        ) {
            Ok(child) => child,
            Err(e) => {
                return native_child_error(request_id, id, "CONFINEMENT_UNAVAILABLE", &e);
            }
        };
        let launch_args = json!({
            "apk": apk.display().to_string(),
            "native": true,
            "display": acos_runtime::display_override().map(|o| o.spec()),
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
            // SHOTZ: the reply carries a bounded preview (`<= 1280 px` long
            // edge, `<= ACOS_MCP_MAX_IMAGE_BYTES`), never a multi-MB base64
            // blob. `png_bytes` stays the real size and `png_preview` says
            // whether the inline copy is downscaled.
            let (max_side, max_bytes) = crate::preview::inline_caps();
            let preview = crate::preview::preview_of_png(png, max_side, max_bytes);
            match preview {
                Some(preview) => {
                    result["png_base64"] = json!(crate::base64::encode(&preview.data));
                    result["png_preview"] = preview.meta()["preview"].clone();
                }
                None => {
                    result["png_preview"] = json!({
                        "scaled": null, "inline": false,
                        "reason": "image exceeds ACOS_MCP_MAX_IMAGE_BYTES after downscaling",
                    });
                }
            }
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

    // --- React Native sessions (W162-A) ------------------------------------
    //
    // A detected RN APK is driven by an `RnSession` on its own worker thread
    // inside this (confined) process. The bridge is single-threaded, so every
    // tool below marshals to that one thread.

    /// Launch a React Native APK and read the first UIManager tree.
    fn launch_rn(&self, request_id: &str, id: &str, apk: &std::path::Path) -> Envelope {
        let scratch = rn_scratch();
        if let Err(e) = std::fs::create_dir_all(&scratch) {
            return rn_error(
                request_id,
                Some(id),
                "CONFINEMENT_UNAVAILABLE",
                &format!("cannot create RN scratch {}: {e}", scratch.display()),
            );
        }
        if let Err(e) = self.rn.launch(id, apk, scratch) {
            return rn_session_error(request_id, Some(id), &e);
        }
        let status = self.rn.status(id).ok();
        let tree = self.rn.tree(id);
        let nodes = status.as_ref().map(|s| s.nodes).unwrap_or(0);
        let ops = status.as_ref().map(|s| s.ops).unwrap_or(0);
        let launch_ms = status.as_ref().map(|s| s.launch_ms).unwrap_or(0);
        let natives = status.as_ref().map(|s| s.natives).unwrap_or(0);
        let real_boot = status.as_ref().map(|s| s.real_boot).unwrap_or(false);
        let tree_ok = matches!(&tree, Ok(t) if t.node_count() > 0);
        let blocker = if tree_ok {
            Value::Null
        } else {
            json!({
                "code": tree.as_ref().err().map_or("UNSUPPORTED_RN_SETUP", |e| e.code()),
                "message": tree.as_ref().err().map_or_else(
                    || "the RN bridge mounted but the UIManager op stream produced no tree".to_string(),
                    ToString::to_string,
                ),
                "details": {},
            })
        };
        let result = json!({
            "stages": [
                {"stage": "apk_parsed", "ok": true, "status": "ok", "blocker": null},
                {"stage": "rn_bridge", "ok": true, "status": "ok", "blocker": null},
                {"stage": "ui_tree", "ok": tree_ok,
                 "status": if tree_ok { "ok" } else { "blocked" }, "blocker": blocker},
            ],
            "reached_stage": if tree_ok { "ui_tree" } else { "rn_bridge" },
            "package": "",
            "activity": null,
            "classes_loaded": 0,
            "events": ["react_native=1", format!("nodes={nodes}"), format!("ops={ops}")],
            "wall_ms": launch_ms,
            "peak_rss_bytes": 0,
            "renderer": false,
            "natives": natives,
            "real_boot": real_boot,
            "frame": Value::Null,
        });
        let envelope = if tree_ok {
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
                    ErrorCode::UnsupportedRnSetup,
                    "React Native bridge produced no UI tree".to_string(),
                )
                .with_details(json!({"stage": "ui_tree"})),
                Metrics::wall_ms(launch_ms as u64),
            )
        };
        envelope
            .with_session_id(id.to_string())
            .with_value_result(result)
            .with_capabilities(["apk.inspect", "lifecycle", "ui.tree"])
    }

    fn observe_rn(&self, request_id: &str, id: &str, args: &Value) -> Envelope {
        let tree = args.get("tree").and_then(Value::as_bool).unwrap_or(false)
            || args.get("text").and_then(Value::as_bool).unwrap_or(false);
        if tree {
            let max_nodes = args
                .get("max_nodes")
                .and_then(Value::as_u64)
                .unwrap_or(2000) as usize;
            let text = args.get("text").and_then(Value::as_bool).unwrap_or(false);
            return self.observe_tree_rn(request_id, id, max_nodes, text);
        }
        // The legacy (Paper) bridge here registers no native view managers, so
        // no pixel frame exists. Report it honestly instead of a blank image.
        rn_error(
            request_id,
            Some(id),
            "UNSUPPORTED_RN_NO_RENDERER",
            "the RN bridge registers no native view managers, so UIImplementation never lays out or draws; use observe --tree",
        )
    }

    fn observe_tree_rn(
        &self,
        request_id: &str,
        id: &str,
        max_nodes: usize,
        with_text: bool,
    ) -> Envelope {
        let tree = match self.rn.tree(id) {
            Ok(tree) => tree,
            Err(e) => return rn_session_error(request_id, Some(id), &e),
        };
        let nodes = tree.node_count();
        let truncated = nodes > max_nodes;
        let mut result = json!({
            "session_id": id,
            "package": "",
            "stage_reached": "ui_tree",
            "windows": 1,
            "truncated": truncated,
            "nodes": nodes,
            "ops": tree.op_count,
            "renderer": false,
            "react_native": true,
            "tree": tree.to_json(),
        });
        if with_text {
            result["text"] = json!(rn_tree_text(&tree));
        }
        Envelope::pass(
            request_id,
            ProtocolBackend::AcosNative,
            Fidelity::NativeRuntime,
            Metrics::wall_ms(0),
        )
        .with_session_id(id.to_string())
        .with_value_result(result)
        .with_capabilities(["ui.tree"])
    }

    fn tap_rn(
        &self,
        request_id: &str,
        id: &str,
        text: Option<&str>,
        test_id: Option<&str>,
        point: Option<(f64, f64)>,
    ) -> Envelope {
        let outcome = match (text, test_id) {
            (Some(label), _) => self
                .rn
                .tap_text(id, label)
                .map(|tag| (tag, Some(label.to_string()), None, "tap_text")),
            (None, Some(test_id)) => self
                .rn
                .tap_test_id(id, test_id)
                .map(|tag| (tag, None, Some(test_id.to_string()), "tap_test_id")),
            (None, None) => {
                if point.is_some() {
                    return rn_error(
                        request_id,
                        Some(id),
                        "UNSUPPORTED_RN_NO_RENDERER",
                        "point taps need layout bounds; the RN bridge has no native view managers, so use --text or --testID",
                    );
                }
                return invalid_argument(
                    request_id,
                    "action",
                    "tap needs `text`, `testID`, or `x` and `y`",
                );
            }
        };
        match outcome {
            Ok((tag, resolved, resolved_id, kind)) => Envelope::pass(
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
                "tapped_tag": tag,
                "resolved_text": resolved,
                "resolved_test_id": resolved_id,
                "renderer": false,
            }))
            .with_capabilities(["input"]),
            Err(e) => rn_session_error(request_id, Some(id), &e),
        }
    }

    fn logs_rn(&self, request_id: &str, id: &str) -> Envelope {
        let lines = self.rn.logs(id);
        let records: Vec<Value> = lines
            .iter()
            .map(|line| json!({"priority": "info", "tag": "react-native", "message": line, "line": line}))
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
            "stage_reached": "ui_tree",
            "count": records.len(),
            "logs": records,
        }))
        .with_capabilities(["logs"])
    }

    fn status_rn(&self, request_id: &str, id: &str) -> Envelope {
        match self.rn.status(id) {
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
                "stage_reached": "ui_tree",
                "published_frames": 0,
                "nodes": status.nodes,
                "ops": status.ops,
                "natives": status.natives,
                "real_boot": status.real_boot,
                "launch_ms": status.launch_ms,
                "elapsed_ms": status.elapsed_ms,
                "live": true,
                "react_native": true,
                "renderer": false,
            })),
            Err(e) => rn_session_error(request_id, Some(id), &e),
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
        // `acos launch --density/--display/--reference` rides along as a compact
        // spec; install it before the runtime worker boots so `DisplayMetrics`,
        // the WMS display and the layout all use the phone's configuration.
        match args.get("display") {
            None | Some(Value::Null) => acos_runtime::set_display_override(None),
            Some(Value::String(spec)) => match acos_runtime::DisplayOverride::parse(spec) {
                Some(o) => acos_runtime::set_display_override(Some(o)),
                None => return invalid_argument(request_id, "display", "unparsable display spec"),
            },
            Some(_) => return invalid_argument(request_id, "display", "expected a string spec"),
        }
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
        let explicit_rn = args.get("rn").and_then(Value::as_bool).unwrap_or(false);
        let native_flag = args.get("native").and_then(Value::as_bool).unwrap_or(false);
        let is_flutter = flutter_backend::is_flutter(&apk);
        let is_rn = rn_backend::is_react_native(&apk);
        let native = native_flag || explicit_flutter || explicit_rn || is_flutter || is_rn;
        limits.native = native;
        if limits.native && !acos_sandbox::confinement_active() {
            return self.launch_native_child(request_id, &id, &apk, &limits, ctx);
        }
        // Structural detection, never a manifest guess. RN is checked first so
        // the legacy Paper path is explicit and a Flutter APK is unaffected.
        if limits.native && is_rn {
            return self.launch_rn(request_id, &id, &apk);
        }
        if limits.native && is_flutter {
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
        if self.rn.contains(&id) {
            return self.observe_rn(request_id, &id, args);
        }
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
        // `--screenshot <path>` writes the published frame's pixels to `path`.
        // The stateless CLI builds this argument; honouring it here is what
        // makes `acos observe --frame --screenshot` work against a live
        // daemon session (the Flutter backend already did this).
        let screenshot = args
            .get("screenshot")
            .and_then(Value::as_str)
            .map(std::path::PathBuf::from);
        self.with_session(&id, |session| {
            let status = session.status().ok();
            match session.observe(screenshot) {
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
                        result["screenshot"] = json!({
                            "path": path.display().to_string(),
                            "bytes": bytes,
                        });
                        capabilities.push("ui.screenshot");
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
        if self.rn.contains(&id) {
            let max_nodes = args
                .get("max_nodes")
                .and_then(Value::as_u64)
                .unwrap_or(2000) as usize;
            let text = args.get("text").and_then(Value::as_bool).unwrap_or(false);
            return self.observe_tree_rn(request_id, &id, max_nodes, text);
        }
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
        if self.rn.contains(&id) {
            let action = args.get("action").cloned().unwrap_or(Value::Null);
            let kind = action.get("kind").and_then(Value::as_str).unwrap_or("tap");
            return match kind {
                "tap" => {
                    let text = action.get("text").and_then(Value::as_str);
                    let test_id = action
                        .get("testID")
                        .or_else(|| action.get("test_id"))
                        .and_then(Value::as_str);
                    let point = match (
                        action.get("x").and_then(Value::as_f64),
                        action.get("y").and_then(Value::as_f64),
                    ) {
                        (Some(x), Some(y)) => Some((x, y)),
                        _ => None,
                    };
                    self.tap_rn(request_id, &id, text, test_id, point)
                }
                other => rn_error(
                    request_id,
                    Some(&id),
                    "UNSUPPORTED_RN",
                    &format!("React Native session does not support action kind {other:?}"),
                ),
            };
        }
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
                if let Some(resource_id) = action.get("id").and_then(Value::as_str) {
                    return input_envelope(request_id, &id, session.tap_id(resource_id), "tap");
                }
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
                    _ => invalid_argument(
                        request_id,
                        "action",
                        "tap needs `x` and `y`, or `text`, or `id`",
                    ),
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
        if self.rn.contains(&id) {
            self.rn.remove(&id);
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
        let rn = self.rn.remove(&id);
        let destroyed = local || native || flutter || rn;
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
    /// Record a session this process launched, with the launch token the
    /// daemon returned for it (SERVELEAK2/F3) and the FB5 hygiene fields
    /// (`label`, launch/use clocks, the idle TTL that applies to it).
    fn remember_owned(&self, id: &str, args: &Value, token: &str) -> u64 {
        let now = Instant::now();
        let label = args
            .get("label")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let mut idle_ttl_s = self.idle_ttl_for(args);
        if self.daemon.is_some() && token.is_empty() && idle_ttl_s > 0 {
            // FB5b/M2: the external daemon (an older `acos serve`) returned no
            // launch token, so ownership of this id can never be proven; idle
            // expiry would be an id-only guess at someone else's guest.
            eprintln!(
                "acos-mcp: session {id}: the external daemon reported no launch token; \
                 idle expiry is disabled for it (ownership cannot be proven)"
            );
            idle_ttl_s = 0;
        }
        self.owned
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(
                id.to_string(),
                OwnedSession {
                    token: token.to_string(),
                    label,
                    launched_at: now,
                    last_used: now,
                    idle_ttl_s,
                    seq: self.next_seq.fetch_add(1, Ordering::SeqCst),
                    in_flight: 0,
                    expiring: false,
                },
            );
        // A fresh launch gives the id a live guest again: it is no longer
        // "the session that expired".
        self.expired
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(id);
        idle_ttl_s
    }

    /// The idle TTL that applies to one launch: the `idle_ttl_s` argument, else
    /// `ACOS_SESSION_IDLE_TTL_S`, else [`DEFAULT_IDLE_TTL_S`]. `0` disables it.
    fn idle_ttl_for(&self, args: &Value) -> u64 {
        if let Some(ttl) = args.get("idle_ttl_s").and_then(Value::as_u64) {
            return ttl;
        }
        default_idle_ttl_s()
    }

    /// Renew the idle clock of an owned session (any session call counts).
    fn touch(&self, id: &str) {
        if let Some(session) = self
            .owned
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get_mut(id)
        {
            session.last_used = Instant::now();
        }
    }

    fn remove_owned(&self, id: &str) -> Option<OwnedSession> {
        self.owned
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(id)
    }

    /// The structured answer for a call on a session this server reclaimed
    /// because it idled past its TTL (FB5): `SESSION_NOT_FOUND` (the closest
    /// existing code — the session really is gone) with the TTL named in the
    /// message, the details and the hint.
    fn expired_envelope(&self, id: &str, request_id: &str) -> Option<Envelope> {
        let expired = self
            .expired
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(id)
            .cloned()?;
        Some(expired_envelope_for(id, request_id, &expired))
    }
}

/// The structured answer for a call on a session reclaimed for idleness.
fn expired_envelope_for(id: &str, request_id: &str, expired: &ExpiredSession) -> Envelope {
    {
        let idle_s = expired.expired_at.elapsed().as_secs();
        let mut envelope = Envelope::failure(
            request_id,
            ProtocolBackend::AcosNative,
            Fidelity::NativeRuntime,
            ProtocolError::new(
                ErrorCode::SessionNotFound,
                format!(
                    "session `{id}` was destroyed after {} of idleness (idle TTL {} s); its \
                     guest is gone",
                    format_secs(expired.idle_ttl_s),
                    expired.idle_ttl_s
                ),
            )
            .with_details(json!({
                "session_id": id,
                "reason": "idle_ttl_expired",
                "idle_ttl_s": expired.idle_ttl_s,
                "gone_for_s": idle_s,
                "label": expired.label,
            }))
            .with_hint(
                "launch the APK again with `acos_launch`; pass `idle_ttl_s` (or set \
                 ACOS_SESSION_IDLE_TTL_S) for a session you intend to park longer",
            ),
            Metrics::wall_ms(0),
        );
        envelope = envelope.with_session_id(id);
        envelope
    }
}

impl SessionBackend {
    /// Remember that `id` was reclaimed by the idle TTL, newest last, bounded so
    /// a long-lived server cannot accumulate tombstones.
    fn note_expired(&self, id: &str, label: &str, idle_ttl_s: u64) {
        const KEEP: usize = 32;
        let mut expired = self.expired.lock().unwrap_or_else(|e| e.into_inner());
        if expired.len() >= KEEP {
            if let Some(oldest) = expired
                .iter()
                .min_by_key(|(_, e)| e.expired_at)
                .map(|(id, _)| id.clone())
            {
                expired.remove(&oldest);
            }
        }
        expired.insert(
            id.to_string(),
            ExpiredSession {
                idle_ttl_s,
                expired_at: Instant::now(),
                label: label.to_string(),
            },
        );
    }

    /// Start the background idle reaper (FB5).
    ///
    /// One thread per server process, started by `main` once the backend is in
    /// an `Arc`. It holds only a `Weak`, so a dropped backend ends it, and it
    /// stops at `shutdown`. Every tick sweeps sessions whose idle clock passed
    /// their TTL: destroy through the daemon, one log line, and a tombstone that
    /// answers later calls with the reason.
    pub fn start_idle_reaper(self: &Arc<Self>) {
        let weak: Weak<SessionBackend> = Arc::downgrade(self);
        let _ = std::thread::Builder::new()
            .name("acos-mcp-idle-reaper".to_string())
            .spawn(move || loop {
                std::thread::sleep(Duration::from_millis(250));
                let Some(backend) = weak.upgrade() else { return };
                if backend.stopping.load(Ordering::SeqCst) {
                    return;
                }
                if backend.sweep_idle() {
                    // Ownership could not be proven (listing failed): do not ask
                    // the daemon again every tick.
                    drop(backend);
                    std::thread::sleep(Duration::from_secs(5));
                }
            });
    }

    /// Destroy every owned session that idled past its TTL (FB5). Public so a
    /// test can drive one sweep deterministically instead of sleeping.
    ///
    /// Returns `true` when the sweep had due sessions but could not prove
    /// ownership (the external daemon's listing failed): it destroyed nothing,
    /// and the reaper backs off before asking again.
    ///
    /// FB5b safety rules (the owner's other agents share the external daemon):
    /// * a due session is *claimed* under the `owned` lock after re-checking its
    ///   idle clock and in-flight count (M1); calls that arrive from then on get
    ///   the `idle_ttl_expired` answer instead of racing the destroy;
    /// * on the EXTERNAL daemon a failed listing destroys nothing (H1) and a
    ///   tokenless session is never expired (M2) — an id is not a guest;
    /// * the listing goes straight to the daemon, never through the default
    ///   session's tombstone (H1).
    pub fn sweep_idle(&self) -> bool {
        if self.stopping.load(Ordering::SeqCst) {
            return false;
        }
        let external = self.daemon.is_some();
        // M1: claim under the lock, re-reading the clock there.
        let claimed: Vec<(String, OwnedSession)> = {
            let mut owned = self.owned.lock().unwrap_or_else(|e| e.into_inner());
            let now = Instant::now();
            let mut claimed = Vec::new();
            for (id, session) in owned.iter_mut() {
                // M2: never idle-expire a tokenless session on an external daemon.
                if session.idle_ttl_s == 0
                    || session.in_flight > 0
                    || session.expiring
                    || (external && session.token.is_empty())
                    || now.saturating_duration_since(session.last_used).as_secs()
                        < session.idle_ttl_s
                {
                    continue;
                }
                session.expiring = true;
                claimed.push((id.clone(), session.clone()));
            }
            claimed
        };
        if claimed.is_empty() {
            return false;
        }
        let ctx = teardown_context();
        let current = self
            .daemon_sessions(&ctx, Restart::Never)
            .map(|sessions| launch_tokens(&sessions));
        if current.is_none() && external {
            // H1: no listing, no proof, no destroy.
            let ids: Vec<&str> = claimed.iter().map(|(id, _)| id.as_str()).collect();
            eprintln!(
                "acos-mcp: session_list failed on the external daemon; cannot prove ownership \
                 of idle session(s), destroying nothing: {}",
                ids.join(", ")
            );
            for (id, session) in &claimed {
                self.release_claim(id, session.seq);
            }
            return true;
        }
        for (id, session) in claimed {
            if let Some(current) = current.as_ref() {
                match current.get(&id) {
                    Some(current_token)
                        if session.token.is_empty() || *current_token == session.token => {}
                    Some(_) => {
                        // Another client re-launched the id: that launch owns the
                        // guest now and this server must not destroy it.
                        eprintln!(
                            "acos-mcp: session {id} was launched again by another client; \
                             not expiring it"
                        );
                        self.remove_owned(&id);
                        continue;
                    }
                    None => {
                        // Already gone (the guest died, or a reset dropped it):
                        // record the tombstone so the agent still learns why.
                        eprintln!(
                            "acos-mcp: idle session {id} is already gone; noting it expired"
                        );
                        self.finish_expiry(&id, &session);
                        continue;
                    }
                }
            }
            let args = json!({"base": {"request_id": "req_idle_ttl", "session_id": id}});
            let envelope = self.destroy_on_daemon(&args, &ctx, Restart::Never);
            match envelope {
                Some(envelope) if envelope.ok() => {
                    eprintln!(
                        "acos-mcp: session {id}{} was idle for {} s (idle TTL {} s); destroyed it",
                        if session.label.is_empty() {
                            String::new()
                        } else {
                            format!(" ({})", session.label)
                        },
                        Instant::now()
                            .saturating_duration_since(session.last_used)
                            .as_secs(),
                        session.idle_ttl_s
                    );
                    self.finish_expiry(&id, &session);
                }
                other => {
                    let code = other
                        .as_ref()
                        .and_then(|envelope| envelope.error())
                        .map(|error| error.code)
                        .unwrap_or(ErrorCode::Internal);
                    eprintln!(
                        "acos-mcp: session {id} idled past its TTL but could not be destroyed \
                         ({code:?}); will retry after another TTL"
                    );
                    if code == ErrorCode::SessionNotFound {
                        self.finish_expiry(&id, &session);
                    } else {
                        // Do not hammer a wedged daemon: give it a whole new TTL.
                        self.release_claim(&id, session.seq);
                    }
                }
            }
        }
        false
    }

    /// The guest is gone: tombstone first (so a call between the two steps still
    /// gets the reason), then drop the owned record.
    fn finish_expiry(&self, id: &str, session: &OwnedSession) {
        self.note_expired(id, &session.label, session.idle_ttl_s);
        self.remove_owned(id);
    }

    /// Undo a sweep claim that did not end in a destroy: the session is live and
    /// gets a fresh idle clock.
    fn release_claim(&self, id: &str, seq: u64) {
        if let Some(session) = self
            .owned
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get_mut(id)
        {
            if session.seq == seq {
                session.expiring = false;
                session.last_used = Instant::now();
            }
        }
    }

    /// The sessions this server owns, as `acos_status` reports them (FB5):
    /// id, label, package, activity, age, idle time, TTL and the guest's RSS.
    ///
    /// FB5b/L1: a daemon entry is attributed to us only when its launch token is
    /// the one this server launched (the same proof the sweep uses). A different
    /// token is another client's guest: it is neither listed nor measured. A
    /// tokenless record on the external daemon cannot be proven, so it is listed
    /// without daemon facts (no guest pid, no RSS).
    fn owned_status(&self, ctx: &CallContext) -> Vec<Value> {
        let owned: Vec<(String, OwnedSession)> = self
            .owned
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .map(|(id, session)| (id.clone(), session.clone()))
            .collect();
        if owned.is_empty() {
            return Vec::new();
        }
        let external = self.daemon.is_some();
        let listed = self.daemon_sessions(ctx, Restart::OnFailure);
        let by_id: HashMap<String, Value> = listed
            .as_ref()
            .map(|sessions| {
                sessions
                    .iter()
                    .filter_map(|session| {
                        let id = session.get("session_id").and_then(Value::as_str)?;
                        Some((id.to_string(), session.clone()))
                    })
                    .collect()
            })
            .unwrap_or_default();
        let now = Instant::now();
        owned
            .into_iter()
            .filter_map(|(id, session)| {
                let entry = by_id.get(&id);
                // When the daemon answered, an id it does not hold has no guest
                // and is not a live session any more (FB5; `acos_destroy` +
                // `acos_status` must be an error).
                if listed.is_some() && entry.is_none() {
                    return None;
                }
                let proven = entry.filter(|entry| {
                    let current = entry
                        .get("session_token")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    if session.token.is_empty() {
                        // Only the private daemon's ids are provably ours.
                        !external
                    } else {
                        current == session.token
                    }
                });
                if entry.is_some() && proven.is_none() && !session.token.is_empty() {
                    // Another client's launch holds this id now.
                    return None;
                }
                let entry = proven;
                let guest_pid = entry
                    .and_then(|entry| entry.get("guest_pid"))
                    .and_then(Value::as_u64)
                    .and_then(|pid| u32::try_from(pid).ok())
                    .filter(|pid| *pid > 1);
                Some(json!({
                    "session_id": id,
                    "label": session.label,
                    "package": entry.and_then(|entry| entry.get("package")).cloned()
                        .unwrap_or(Value::Null),
                    "activity": entry.and_then(|entry| entry.get("activity")).cloned()
                        .unwrap_or(Value::Null),
                    "stage_reached": entry.and_then(|entry| entry.get("stage_reached")).cloned()
                        .unwrap_or(Value::Null),
                    "live": entry.and_then(|entry| entry.get("live")).cloned()
                        .unwrap_or(Value::Null),
                    "age_s": now.saturating_duration_since(session.launched_at).as_secs(),
                    "idle_s": now.saturating_duration_since(session.last_used).as_secs(),
                    "idle_ttl_s": session.idle_ttl_s,
                    "guest_pid": guest_pid,
                    "guest_rss_kb": guest_pid.and_then(guest_rss_kb),
                }))
            })
            .collect()
    }

    /// The daemon's `session_list` sessions, or `None` when nothing could answer
    /// (no daemon, an unsupported verb, or a failed/absent reply). `None` is
    /// never treated as "the daemon has no sessions".
    ///
    /// FB5b/H1: goes **straight** to the daemon — never through the default
    /// session's tombstone, never touching any idle clock.
    fn daemon_sessions(&self, _ctx: &CallContext, restart: Restart) -> Option<Vec<Value>> {
        let args = json!({"base": {"request_id": "req_hygiene_list"}});
        let envelope = self.forward_to_daemon_opts(crate::daemon::TOOL_LIST, &args, restart)?;
        if !envelope.ok() {
            return None;
        }
        envelope
            .result()
            .and_then(|result| result.get("sessions"))
            .and_then(Value::as_array)
            .cloned()
    }

    pub fn session_call(&self, name: &str, args: &Value, ctx: &CallContext) -> Option<Envelope> {
        // FB5: a relative path argument is refused here, with the server's
        // resolution, before a daemon or a child could resolve it against the
        // *server's* cwd (which is never the agent's).
        if let Some(envelope) = relative_path_envelope(&request_id(args), args) {
            return Some(envelope);
        }
        if name == "acos_status" {
            // FB5: `acos_status` lists this server's sessions, so it cannot be a
            // bare forward. Every other tool keeps the one forwarding path.
            return Some(self.call_status(&request_id(args), args, ctx));
        }
        if name != "acos_doctor" && !name.is_empty() {
            // **One** forwarding path for both daemons (merge of SERVELEAK and
            // REL3a): [`Self::forward_session_tool`] forwards to the external
            // daemon when one is configured and to this server's private ART
            // daemon otherwise, and it is where a launch is recorded as owned
            // (SERVELEAK) with the token the daemon returned (SERVELEAK2/F3).
            // SERVELEAK's inline external-daemon branch and REL3a's private
            // branch were two copies of this, so only the private daemon was
            // covered by the owned-session bookkeeping.
            if let Some(envelope) = self.forward_session_tool(name, args) {
                return Some(envelope);
            }
        }
        let request_id = request_id(args);
        match name {
            "acos_launch" => Some(self.call_run(&request_id, args, ctx)),
            "acos_observe_tree" => Some(self.call_observe_tree(&request_id, args, ctx)),
            "acos_observe" => Some(self.call_observe(&request_id, args, ctx)),
            "acos_tap" => Some(self.call_action(
                &request_id,
                &json!({"base": session_base(args), "action": {"kind": "tap", "x": args.get("x"), "y": args.get("y"), "text": args.get("text"), "id": args.get("id"), "testID": args.get("testID")}}),
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
            "acos_status" => Some(self.call_status(&request_id, args, ctx)),
            "acos_reset" => Some(self.call_reset(&request_id, args, ctx)),
            "acos_destroy" => Some(self.call_destroy(&request_id, args, ctx)),
            "acos_text" => Some(self.call_text(&request_id, args, ctx)),
            "acos_doctor" => Some(self.cli.call_doctor(&request_id, args, ctx)),
            _ => None,
        }
    }

    fn call_logs(&self, request_id: &str, args: &Value) -> Envelope {
        let id = session_id(args);
        if self.rn.contains(&id) {
            return self.logs_rn(request_id, &id);
        }
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
                let min_priority = args
                    .get("min_priority")
                    .and_then(Value::as_str)
                    .and_then(acos_runtime::session::parse_log_priority);
                let records: Vec<Value> = acos_runtime::session::summarize_logs(
                    &lines,
                    args.get("tag").and_then(Value::as_str),
                    min_priority,
                )
                .into_iter()
                .map(|record| {
                    let repeat = record.repeat_count;
                    let line = if repeat > 1 {
                        format!("{} (repeated {repeat} times)", record.line)
                    } else {
                        record.line.clone()
                    };
                    json!({
                        "priority": record.priority,
                        "tag": record.tag,
                        "message": record.message,
                        "line": line,
                        "repeat_count": repeat,
                    })
                })
                .collect();
                let kept = records
                    .iter()
                    .map(|r| r["repeat_count"].as_u64().unwrap_or(1))
                    .sum::<u64>();
                let collapsed = kept - records.len() as u64;
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
                    "total": lines.len(),
                    "count": records.len(),
                    "collapsed": collapsed,
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

    /// `acos_status` (FB5).
    ///
    /// Two jobs, in this order:
    /// 1. list **this server's** sessions (`owned`) with `label`, `package`,
    ///    `activity`, `age_s`, `idle_s`, the applicable TTL and the guest RSS;
    /// 2. answer the historical single-session status for the addressed id.
    ///
    /// An owned id is answered from the local bookkeeping (so `label`/`idle_s`
    /// exist even when the daemon cannot report them); anything else — a session
    /// an external daemon or a CLI launched, a native/Flutter/RN child — keeps
    /// its old path and gains the `sessions` array.
    fn call_status(&self, request_id: &str, args: &Value, ctx: &CallContext) -> Envelope {
        let id = session_id(args);
        if let Some(envelope) = self.expired_envelope(&id, request_id) {
            return envelope;
        }
        let sessions = self.owned_status(ctx);
        let mine = sessions
            .iter()
            .find(|session| session.get("session_id").and_then(Value::as_str) == Some(id.as_str()))
            .cloned();
        if let Some(mut session) = mine {
            session["sessions"] = Value::Array(sessions);
            return Envelope::pass(
                request_id,
                ProtocolBackend::AcosNative,
                Fidelity::NativeRuntime,
                Metrics::wall_ms(0),
            )
            .with_session_id(id)
            .with_value_result(session);
        }
        // This server launched it, but the daemon no longer holds it (it was
        // destroyed, or the guest died): never a pass on a session with no
        // guest. The idle-TTL case is answered above, with its reason.
        let owned_here = self
            .owned
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains_key(&id);
        if owned_here {
            let envelope = Envelope::failure(
                request_id,
                ProtocolBackend::AcosNative,
                Fidelity::NativeRuntime,
                ProtocolError::new(
                    ErrorCode::SessionNotFound,
                    format!("no live ART session `{id}`: it was destroyed or its guest exited"),
                )
                .with_details(json!({"session_id": id, "reason": "no_live_guest"}))
                .with_hint("launch the APK again with `acos_launch`"),
                Metrics::wall_ms(0),
            )
            .with_session_id(id);
            return envelope;
        }
        if self.rn.contains(&id) {
            return self.status_rn(request_id, &id);
        }
        if self.flutter.contains(&id) {
            return self.status_flutter(request_id, &id);
        }
        if self.has_native_child(&id) {
            if let Some(mut envelope) = self.forward_native(&id, "acos_status", args) {
                attach_sessions(&mut envelope, sessions);
                return envelope;
            }
        }
        // Not ours: the daemon's own status for that id (a CLI-launched session
        // on a shared daemon), or the in-process session of the legacy engine.
        if let Some(mut envelope) = self.forward_session_tool("acos_status", args) {
            attach_sessions(&mut envelope, sessions);
            return envelope;
        }
        let mut envelope = self.with_session(&id, |session| match session.status() {
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
        });
        attach_sessions(&mut envelope, sessions);
        envelope
    }
}

impl Backend for SessionBackend {
    fn call(&self, command: CommandName, args: &Value, ctx: &CallContext) -> Envelope {
        let request_id = request_id(args);
        // FB5: a relative `apk`/`scenario`/`artifacts` is refused before the CLI
        // (whose cwd is this server's) can resolve it.
        if let Some(envelope) = relative_path_envelope(&request_id, args) {
            return envelope;
        }
        let forwarded = match command {
            CommandName::Run => Some("acos_launch"),
            CommandName::Observe => Some("acos_observe"),
            CommandName::Action => Some("acos_action"),
            CommandName::Reset => Some("acos_reset"),
            CommandName::Destroy => Some("acos_destroy"),
            _ => None,
        };
        if let Some(tool) = forwarded {
            if let Some(envelope) = self.forward_session_tool(tool, args) {
                return envelope;
            }
        }
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
        // No bookkeeping here: [`SessionBackend::forward_session_tool`] records a
        // forwarded launch for both daemons, and an in-process session dies with
        // this process. (Keeping a second copy here made the ownership map depend
        // on *which* entry point the call took, and let it record an un-normalized
        // id.)
        SessionBackend::session_call(self, name, args, ctx)
    }

    /// Destroy every session this process created, then stop the private daemon.
    ///
    /// Called when the MCP transport ends (`Server::serve` after stdin EOF). This
    /// is the same teardown as [`SessionBackend::shutdown`]; the trait method
    /// exists so a generically typed server (`B: Backend`) reaches it.
    fn shutdown(&self) {
        SessionBackend::shutdown(self);
    }
}

fn session_id(args: &Value) -> String {
    args.pointer("/base/session_id")
        .and_then(Value::as_str)
        // Older clients (and `mcp_smoke.py`'s first form) send `session_id` at
        // the top level; accept it here too, so `acos_status` addresses the
        // session the client named.
        .or_else(|| args.get("session_id").and_then(Value::as_str))
        .map(str::to_string)
        .unwrap_or_else(|| default_session_id().to_string())
}

/// SERVELEAK2/F3: this **server instance's** default session id.
///
/// The old default was the shared constant `s_mcp`. Many agents run an
/// `acos-mcp` at the same time, and against one daemon two servers with the same
/// default id address the *same* held session: the second launch replaces the
/// first, and either server's stdin EOF then destroys the other's live guest
/// (F3). A per-instance id (`mcp-<pid>-<random>`) makes the default id private to
/// this process, exactly as an explicitly chosen id always was. An explicit
/// `base.session_id` still wins, so a client that wants to share a session
/// across processes still can.
///
/// **Merge note (SERVELEAK3):** this is the *only* `default_session_id`. Both
/// daemons this server can forward to use it — the external one (`with_daemon`)
/// and the private one REL3a spawns — because the id is filled in once, by
/// [`session_base`] inside [`SessionBackend::forward_session_tool`], before any
/// forwarding. REL3a's `mcp-<pid>` version is folded in here: the random suffix
/// keeps two servers that share a pid namespace (containers, `fork` trees) apart
/// as well.
pub fn default_session_id() -> &'static str {
    static ID: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    ID.get_or_init(|| {
        let mut bytes = [0u8; 8];
        let read = std::fs::File::open("/dev/urandom")
            .and_then(|mut file| std::io::Read::read_exact(&mut file, &mut bytes))
            .is_ok();
        if !read {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos() as u64)
                .unwrap_or(0);
            bytes.copy_from_slice(&nanos.to_le_bytes());
        }
        let hex: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
        format!("mcp-{}-{hex}", std::process::id())
    })
}

/// SERVELEAK2/F3: the launch token a daemon returned for an `acos_launch`.
///
/// Empty for a backend that does not report one (an in-process session, or a
/// daemon older than the field).
fn launch_token(envelope: &Envelope) -> String {
    envelope
        .result()
        .and_then(|result| result.get("session_token"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// The default idle TTL for this process (FB5): `ACOS_SESSION_IDLE_TTL_S`, else
/// [`DEFAULT_IDLE_TTL_S`]. A malformed value is ignored (the default applies)
/// rather than silently disabling the reaper; `0` is the explicit opt-out.
fn default_idle_ttl_s() -> u64 {
    if let Some(value) = std::env::var(IDLE_TTL_ENV)
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
    {
        return value;
    }
    // The long-documented (pre-FB5, previously inert) millisecond knob still
    // works, so an existing MCP registration does not silently lose its TTL.
    if let Some(millis) = std::env::var("ACOS_MCP_SESSION_TTL_MS")
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
    {
        return millis / 1000;
    }
    DEFAULT_IDLE_TTL_S
}

/// "20 s" / "10 min": a TTL as an agent reads it, in the expired error.
fn format_secs(seconds: u64) -> String {
    if seconds >= 60 && seconds % 60 == 0 {
        format!("{} min", seconds / 60)
    } else {
        format!("{seconds} s")
    }
}

/// `/proc/<pid>/status` `VmRSS`, in KiB, for a live guest (`None` when the pid
/// is gone). FB5: `acos_status` reports it so an agent can see what a parked
/// session costs; it is the guest's own RSS, not the whole tree's.
fn guest_rss_kb(pid: u32) -> Option<u64> {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    for line in status.lines() {
        let Some(rest) = line.strip_prefix("VmRSS:") else {
            continue;
        };
        let value = rest.split_whitespace().next()?;
        return value.parse().ok();
    }
    None
}

/// `session_id -> launch token` for a daemon `session_list` result.
fn launch_tokens(sessions: &[Value]) -> HashMap<String, String> {
    sessions
        .iter()
        .filter_map(|session| {
            let id = session.get("session_id").and_then(Value::as_str)?;
            let token = session
                .get("session_token")
                .and_then(Value::as_str)
                .unwrap_or_default();
            Some((id.to_string(), token.to_string()))
        })
        .collect()
}

/// The context teardown/expiry use: no cancellation, a bounded deadline, and no
/// allowed roots (nothing here validates a path).
fn teardown_context() -> CallContext {
    CallContext {
        cancel: crate::process::CancelToken::new(),
        deadline: Instant::now() + Duration::from_secs(30),
        budget: acos_protocol::ResourceBudget::default(),
        allowed_roots: Vec::new(),
        artifacts_dir: PathBuf::from("."),
        max_arg_chars: 8 * 1024,
        max_output_bytes: 1024 * 1024,
    }
}

/// Annotate a forwarded launch reply with the hygiene fields this server owns
/// (FB5): the agent's `label` and the idle TTL that will reclaim the session.
fn annotate_launch(envelope: &mut Envelope, args: &Value, idle_ttl_s: u64) {
    let Some(result) = envelope.result_mut().and_then(Value::as_object_mut) else {
        return;
    };
    result.insert(
        "label".to_string(),
        json!(args.get("label").and_then(Value::as_str).unwrap_or_default()),
    );
    result.insert("idle_ttl_s".to_string(), json!(idle_ttl_s));
    result.insert("idle_reap".to_string(), json!(idle_ttl_s > 0));
}

/// Attach this server's session list to a status reply without displacing any
/// field the reply already carries.
fn attach_sessions(envelope: &mut Envelope, sessions: Vec<Value>) {
    let Some(result) = envelope.result_mut().and_then(Value::as_object_mut) else {
        return;
    };
    result.insert("sessions".to_string(), Value::Array(sessions));
}

/// FB5's relative-path rule as an envelope (`None` when every path is absolute).
fn relative_path_envelope(request_id: &str, args: &Value) -> Option<Envelope> {
    crate::safety::relative_path_envelope(request_id, args, Fidelity::NativeRuntime)
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

/// Where a confined React Native session stages its libraries, DEX and bundle.
fn rn_scratch() -> PathBuf {
    std::env::var_os("ACOS_SESSION_ROOT")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("ACOS_RN_SCRATCH").map(PathBuf::from))
        .unwrap_or_else(std::env::temp_dir)
        .join("rn")
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

/// A structured failure envelope for a React Native session error code.
fn rn_error(
    request_id: &str,
    session_id: Option<&str>,
    code: &str,
    message: &str,
) -> Envelope {
    let error = ProtocolError::new(
        ErrorCode::from_str_opt(code).unwrap_or(ErrorCode::UnsupportedRn),
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

fn rn_session_error(
    request_id: &str,
    session_id: Option<&str>,
    err: &RnSessionError,
) -> Envelope {
    rn_error(request_id, session_id, err.code(), &err.to_string())
}

/// Render an RN tree as indented text (`class#tag testID "text"`).
fn rn_tree_text(tree: &acos_jni::react_native::RnTree) -> String {
    fn walk(node: &acos_jni::react_native::RnNode, depth: usize, out: &mut String) {
        out.push_str(&"  ".repeat(depth));
        out.push_str(&node.class);
        out.push('#');
        out.push_str(&node.tag.to_string());
        if let Some(id) = &node.test_id {
            out.push_str(&format!(" testID={id}"));
        }
        if let Some(text) = &node.text {
            out.push_str(&format!(" {text:?}"));
        }
        if node.clickable {
            out.push_str(" clickable");
        }
        out.push('\n');
        for child in &node.children {
            walk(child, depth + 1, out);
        }
    }
    let mut out = String::new();
    for root in &tree.roots {
        walk(root, 0, &mut out);
    }
    out
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
        Ok(report) => Envelope::pass(
            request_id,
            ProtocolBackend::AcosNative,
            Fidelity::NativeRuntime,
            Metrics::wall_ms(0),
        )
        .with_session_id(session_id.to_string())
        .with_value_result(json!({
            "session_id": session_id,
            "action": kind,
            "accepted": report.accepted,
            "state_changed": report.state_changed,
            "input_ack_seq": report.ack_seqs.last().copied(),
            "text_path": report.path,
        }))
        .with_capabilities(["input"]),
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
                // FB5: `ms` is present on every rung. This ladder evaluates its
                // rungs from one lifecycle report at the end of the launch, so
                // `ok` rungs carry the launch wall time as their observation
                // time and unproven rungs carry `null` — `ms_basis` says so.
                "ms": if stage.status == StageStatus::Ok { Some(report.wall_ms) } else { None },
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
        "total_ms": report.wall_ms,
        "ms_basis": "observed_at_end",
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
        rn_dex: Option<&std::path::Path>,
    ) -> Result<Self, String> {
        let mut command = Command::new(binary);
        command.arg("--confined-child").arg("--rw").arg(read_write);
        for path in read_only {
            command.arg("--ro").arg(path);
        }
        if let Some(dex) = rn_dex {
            command.arg("--rn-dex").arg(dex);
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

/// The private ART daemon could not be started (no session command ran).
fn private_daemon_error(request_id: &str, args: &Value, message: &str) -> Envelope {
    let error = ProtocolError::new(
        ErrorCode::Internal,
        format!("the private ART session daemon is unavailable: {message}"),
    )
    .with_hint(
        "The MCP server runs session tools on `acos serve --engine art`; check that the `acos` \
         binary is installed and executable (set ACOS_BIN to it), or set ACOS_MCP_ENGINE=legacy \
         for the frozen in-process engine (diagnostics only: no real Android).",
    )
    .with_details(json!({"engine": "art", "session_id": session_id(args)}));
    Envelope::failure(
        request_id,
        ProtocolBackend::AcosNative,
        Fidelity::StaticAnalysis,
        error,
        Metrics::wall_ms(0),
    )
    .with_session_id(session_id(args))
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
        // Validate the caller-supplied root *before* `create_private_dir` would
        // tighten it: a group/world-writable `ACOS_SESSION_ROOT` (an app data
        // root another local user controls) is refused, not chmod-ed (W170-A
        // E1). `create_private_dir` only creates or re-checks a private dir.
        require_private_root("ACOS_SESSION_ROOT", &root)?;
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

/// Reject a group/world-writable or foreign-owned root, naming the knob.
fn require_private_root(var: &str, path: &std::path::Path) -> std::io::Result<()> {
    acos_runtime::private_root::ensure_private_root(var, path).map_err(|(code, msg)| {
        std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            format!("{}: {msg}", code.code()),
        )
    })
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

/// The boot-classpath cache directory (`ACOS_BOOT_DIR`, then an installed
/// `$ACOS_HOME/.cache/boot/android-34`, then walking up from the working
/// directory, then `~/.cache/acos/boot/android-34`). Mirrors
/// `acos_vm::default_boot_dir` so the child's Landlock rule names the real
/// resolved tree.
fn native_boot_root() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("ACOS_BOOT_DIR") {
        let path = PathBuf::from(dir);
        if path.is_dir() {
            // The child loads this classpath; a wriable-by-others root lets
            // another local user swap the DEX underneath a confined session.
            // An invalid root disables the boot classpath instead of using it.
            if let Err(e) = require_private_root("ACOS_BOOT_DIR", &path) {
                eprintln!("acos-mcp: refusing ACOS_BOOT_DIR {}: {e}", path.display());
                return None;
            }
            return Some(path);
        }
    }
    if let Some(home) = crate::paths::acos_home() {
        // An installed ACOS reads its own tree; never fall through to the
        // checkout just because the cwd happens to be inside one.
        return crate::paths::installed_boot_dir_with(Some(&home));
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
    // -- FB5: session hygiene ---------------------------------------------- //

    fn scratch_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "acos-fb5-{}-{}-{name}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).expect("scratch dir");
        dir
    }

    fn session_backend() -> SessionBackend {
        SessionBackend::with_cli(
            CliBackend::new(BackendConfig::for_binary("/nonexistent/acos-fb5-test")),
            SessionOptions::default(),
        )
    }

    fn ctx() -> CallContext {
        teardown_context()
    }

    #[test]
    fn idle_ttl_defaults_and_overrides() {
        let backend = session_backend();
        // The launch argument always wins.
        assert_eq!(backend.idle_ttl_for(&json!({"idle_ttl_s": 20})), 20);
        // `0` is the explicit "never" and must not fall through to the default.
        assert_eq!(backend.idle_ttl_for(&json!({"idle_ttl_s": 0})), 0);
        // No argument: the environment, else 10 minutes.
        if std::env::var_os(IDLE_TTL_ENV).is_none() {
            assert_eq!(DEFAULT_IDLE_TTL_S, 600, "10 minutes by default");
            assert_eq!(backend.idle_ttl_for(&json!({})), DEFAULT_IDLE_TTL_S);
            assert_eq!(default_idle_ttl_s(), DEFAULT_IDLE_TTL_S);
        }
    }

    #[test]
    fn an_expired_session_names_the_ttl_it_missed() {
        let backend = session_backend();
        backend
            .owned
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(
                "s_parked".to_string(),
                OwnedSession {
                    token: "tok".to_string(),
                    label: "counter".to_string(),
                    launched_at: Instant::now(),
                    last_used: Instant::now(),
                    idle_ttl_s: 20,
                    seq: 1,
                    in_flight: 0,
                    expiring: false,
                },
            );
        backend.remove_owned("s_parked");
        backend.note_expired("s_parked", "counter", 20);

        let args = json!({"base": {"request_id": "req_tap", "session_id": "s_parked"}});
        let envelope = backend
            .session_call("acos_tap", &args, &ctx())
            .expect("an expired session is answered, never forwarded");
        assert!(!envelope.ok());
        let error = envelope.error().expect("error");
        assert_eq!(error.code, ErrorCode::SessionNotFound);
        assert!(
            error.message.contains("20 s"),
            "the message names the TTL: {}",
            error.message
        );
        let details = error.details.clone().unwrap_or(Value::Null);
        assert_eq!(details["reason"], json!("idle_ttl_expired"));
        assert_eq!(details["idle_ttl_s"], json!(20));
        assert_eq!(details["label"], json!("counter"));
        assert!(
            error.hint.as_str().contains("acos_launch"),
            "the hint says what to do next: {:?}",
            error.hint
        );
        // A launch clears the tombstone: the id can be used again.
        backend
            .expired
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove("s_parked");
        assert!(backend.expired_envelope("s_parked", "req_tap").is_none());
    }

    #[test]
    fn teardown_never_respawns_a_dead_private_daemon() {
        let dir = scratch_dir("no-restart");
        let marker = dir.join("spawned");
        let fake = dir.join("acos");
        std::fs::write(
            &fake,
            format!(
                "#!/bin/sh\necho \"$@\" >> {}\nexit 2\n",
                marker.display()
            ),
        )
        .expect("write fake acos");
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).expect("chmod");

        let backend = SessionBackend::with_cli(
            CliBackend::new(BackendConfig::for_binary(&fake)),
            SessionOptions::default(),
        )
        .with_art_daemon(fake.clone());
        // The state "the private daemon died before shutdown": a reaped child.
        *backend.private.lock().unwrap_or_else(|e| e.into_inner()) =
            Some(crate::private_daemon::dead_for_test(dir.join("dead")));
        backend.remember_owned("s_dead", &json!({}), "tok");
        backend.sweep_idle();
        // Not due yet: the default TTL is 10 minutes.
        assert!(backend.owned.lock().unwrap().contains_key("s_dead"));

        backend.shutdown();
        let ran = std::fs::read_to_string(&marker).unwrap_or_default();
        assert!(
            !ran.lines().any(|line| line.starts_with("serve")),
            "teardown must not spawn a replacement daemon; the fake `acos` was run with: {ran:?}"
        );
        assert!(backend.private.lock().unwrap().is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn idle_expiry_tries_to_destroy_and_never_marks_a_live_guest_expired() {
        let backend = session_backend();
        // A dead private daemon: the destroy cannot be performed, so the record
        // must survive for a retry instead of claiming the guest is gone.
        let dir = scratch_dir("expiry-retry");
        *backend.private.lock().unwrap_or_else(|e| e.into_inner()) =
            Some(crate::private_daemon::dead_for_test(dir.join("dead")));
        backend.owned.lock().unwrap_or_else(|e| e.into_inner()).insert(
            "s_idle".to_string(),
            OwnedSession {
                token: "tok".to_string(),
                label: String::new(),
                launched_at: Instant::now() - Duration::from_secs(30),
                last_used: Instant::now() - Duration::from_secs(30),
                idle_ttl_s: 20,
                seq: 1,
                in_flight: 0,
                expiring: false,
            },
        );
        backend.sweep_idle();
        assert!(
            backend.owned.lock().unwrap().contains_key("s_idle"),
            "a destroy that could not run must keep the session tracked"
        );
        assert!(
            backend.expired.lock().unwrap().is_empty(),
            "an unproven destroy is never reported as expired"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn guest_rss_reads_the_guest_not_the_tree() {
        let own = guest_rss_kb(std::process::id()).expect("this process has VmRSS");
        assert!(own > 0);
        assert_eq!(guest_rss_kb(0), None, "pid 0 is never a guest");
        assert_eq!(guest_rss_kb(4_000_000), None, "an unused pid has no status");
    }

    #[test]
    fn ladder_reports_ms_on_every_rung() {
        let report = LaunchReport {
            stages: vec![
                acos_runtime::session::StageEntry {
                    name: "apk_parsed",
                    status: StageStatus::Ok,
                    blocker: None,
                },
                acos_runtime::session::StageEntry {
                    name: "process_booted",
                    status: StageStatus::NotReached,
                    blocker: None,
                },
            ],
            root_blocker: None,
            reached_stage: "apk_parsed",
            package: Some("dev.test".to_string()),
            activity: None,
            classes_loaded: 0,
            events: Vec::new(),
            wall_ms: 42,
            peak_rss_bytes: 0,
        };
        let ladder = ladder_json(&report);
        let stages = ladder["stages"].as_array().expect("stages");
        assert_eq!(stages.len(), 2);
        for stage in stages {
            assert!(stage.get("ms").is_some(), "every rung carries `ms`: {stage}");
        }
        assert_eq!(stages[0]["ms"], json!(42));
        assert_eq!(stages[1]["ms"], Value::Null);
        assert_eq!(ladder["total_ms"], json!(42));
        assert_eq!(ladder["ms_basis"], json!("observed_at_end"));
    }

    #[test]
    fn relative_path_args_are_refused_with_the_resolved_path() {
        let backend = session_backend();
        let args = json!({
            "apk": "out/counter.apk",
            "base": {"request_id": "req_rel", "session_id": "s_rel"},
        });
        let envelope = backend
            .session_call("acos_launch", &args, &ctx())
            .expect("a relative path is refused before any launch");
        assert!(!envelope.ok());
        let error = envelope.error().expect("error");
        assert_eq!(error.code, ErrorCode::InvalidArgument);
        let details = error.details.clone().unwrap_or(Value::Null);
        assert_eq!(details["reason"], json!("relative_path"));
        let resolved = details["resolved_path"].as_str().expect("resolved path");
        assert!(
            std::path::Path::new(resolved).is_absolute(),
            "the details carry an absolute path: {resolved}"
        );
        assert!(resolved.ends_with("out/counter.apk"));
        assert!(error.hint.as_str().contains("absolute"));
    }

    #[test]
    fn absolute_paths_are_left_alone() {
        let backend = session_backend();
        assert!(crate::safety::relative_path_arg(&json!({
            "apk": "/abs/counter.apk",
            "scenario": "/abs/s.yaml",
            "artifacts": "/abs/out",
        }))
        .is_none());
        assert!(crate::safety::relative_path_arg(&json!({"artifacts": "out/run"})).is_some());
        let _ = backend;
    }


    fn owned_record(ttl: u64, idle_for: u64, token: &str, seq: u64) -> OwnedSession {
        OwnedSession {
            token: token.to_string(),
            label: "lbl".to_string(),
            launched_at: Instant::now() - Duration::from_secs(idle_for),
            last_used: Instant::now() - Duration::from_secs(idle_for),
            idle_ttl_s: ttl,
            seq,
            in_flight: 0,
            expiring: false,
        }
    }

    /// FB5b/M1: a call in flight is never idleness; finishing it renews the
    /// clock; a call that arrives while the sweep is destroying gets the
    /// `idle_ttl_expired` reason, not a bare SESSION_NOT_FOUND.
    #[test]
    fn in_flight_calls_protect_a_session_and_mid_destroy_calls_get_the_reason() {
        let backend = session_backend();
        backend
            .owned
            .lock()
            .unwrap()
            .insert("s_busy".into(), owned_record(20, 30, "tok", 7));

        let guard = backend.begin_call("s_busy", false).expect("not expiring").expect("owned");
        // Due by the clock (begin_call renewed it, so force it back) but in flight.
        backend.owned.lock().unwrap().get_mut("s_busy").unwrap().last_used =
            Instant::now() - Duration::from_secs(30);
        assert!(!backend.sweep_idle(), "nothing to prove");
        assert!(
            !backend.owned.lock().unwrap().get("s_busy").unwrap().expiring,
            "an in-flight session is never claimed"
        );
        drop(guard);
        {
            let owned = backend.owned.lock().unwrap();
            let session = owned.get("s_busy").unwrap();
            assert_eq!(session.in_flight, 0);
            assert!(
                session.last_used.elapsed() < Duration::from_secs(5),
                "finishing a call renews the idle clock"
            );
        }
        backend.sweep_idle();
        assert!(!backend.owned.lock().unwrap().get("s_busy").unwrap().expiring);

        // Mid-destroy: the sweep set `expiring`; a call now answers with the reason.
        backend.owned.lock().unwrap().get_mut("s_busy").unwrap().expiring = true;
        let args = json!({"base": {"request_id": "req_tap", "session_id": "s_busy"}});
        let envelope = backend
            .session_call("acos_tap", &args, &ctx())
            .expect("answered, never forwarded");
        let error = envelope.error().expect("error");
        assert_eq!(error.code, ErrorCode::SessionNotFound);
        assert_eq!(
            error.details.clone().unwrap_or(Value::Null)["reason"],
            json!("idle_ttl_expired")
        );
    }

    /// FB5b/M1: a stale guard of a replaced launch record never decrements the
    /// new record.
    #[test]
    fn a_guard_of_a_replaced_launch_does_not_touch_the_new_record() {
        let backend = session_backend();
        backend
            .owned
            .lock()
            .unwrap()
            .insert("s".into(), owned_record(20, 0, "a", 1));
        let guard = backend.begin_call("s", false).unwrap().unwrap();
        backend
            .owned
            .lock()
            .unwrap()
            .insert("s".into(), owned_record(20, 0, "b", 2));
        backend.owned.lock().unwrap().get_mut("s").unwrap().in_flight = 1;
        drop(guard);
        assert_eq!(backend.owned.lock().unwrap().get("s").unwrap().in_flight, 1);
    }

    /// FB5b/H1: the listing never goes through the tombstone/touch path.
    #[test]
    fn session_list_ignores_a_default_session_tombstone() {
        let backend = session_backend();
        let default_id = default_session_id();
        backend.note_expired(default_id, "default", 20);
        let args = json!({"base": {"request_id": "req_list"}});
        // No daemon here, so the forward is `None` — the point is that it is not
        // the tombstone's SESSION_NOT_FOUND envelope.
        assert!(backend
            .forward_session_tool_opts("session_list", &args, Restart::Never)
            .is_none());
        assert!(backend.daemon_sessions(&ctx(), Restart::Never).is_none());
        // A normal tool still reports the tombstone.
        let tap = backend
            .forward_session_tool_opts("acos_tap", &args, Restart::Never)
            .expect("tombstone answers");
        assert_eq!(tap.error().unwrap().code, ErrorCode::SessionNotFound);
    }
}
