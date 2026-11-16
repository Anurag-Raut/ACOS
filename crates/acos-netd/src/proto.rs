//! The control protocol: one JSON object per line over a Unix socket.
//!
//! The same verbs are what the CLI (`acos-netd …`) and, later, the session API
//! verbs of the module framework wrap. Every reply is `{"ok":true,…}` or
//! `{"ok":false,"error":{"code":…,"message":…}}`; a failure is never an empty or
//! a partial success, and a dead relay is reported as
//! [`ErrorCode::NetworkUnavailable`] rather than as a silent offline session.

use serde::{Deserialize, Serialize};

/// Structured failure codes. `network-unavailable` is the fail-closed answer of
/// every networked operation while the relay (pasta) is down.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ErrorCode {
    /// The relay is not up (pasta died, the fabric never started, the tap is
    /// missing): the session has no network, and says so.
    NetworkUnavailable,
    /// No such session index/name is attached.
    NoSuchSession,
    /// The request is well-formed but not supported in this build.
    Unsupported,
    /// An index is out of range, a name collides, an argument is invalid.
    Invalid,
    /// The relay netns command failed (iptables/ip/tc); carries the stderr tail.
    RelayCommand,
    /// The supervisor is gone (socket closed, never started).
    SupervisorGone,
    /// I/O or JSON failure.
    Io,
}

/// One request.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "kebab-case")]
pub enum Request {
    /// The supervisor's own identity (binary path + content build id). Cheap:
    /// no relay probes. A client uses it to notice a supervisor left over from
    /// another build and replace it cleanly.
    Identity,
    /// Relay state: up/down, pasta pid, named host ports, sessions.
    Status,
    /// Ensure the relay exists (idempotent); creates fabric + pasta on first use.
    Up {
        /// `pasta` binary path override (defaults to `ACOS_NET_PASTA`, then PATH).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pasta: Option<String>,
    },
    /// Create a session netns and run `argv` inside it, connected to the relay.
    ///
    /// This is the transport's attach: the supervisor is the process family that
    /// owns the user namespace both the relay netns and the session netns belong
    /// to, so it is the only process that can move a veth end between them (a
    /// session launched elsewhere cannot, and neither can the guest).
    Spawn {
        /// Caller-chosen session name (`s1`, `joplin`, …), unique while attached.
        session: String,
        argv: Vec<String>,
        /// Optional 1-based index; the lowest free index is used when absent.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        index: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stdout: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stderr: Option<String>,
        /// Seconds to wait for the session's default route (default 30).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ready_timeout_s: Option<u64>,
        /// `SIGKILL` the guest when the supervisor exits (default true).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        die_with_relay: Option<bool>,
        /// How many file descriptors ride along with this request as
        /// `SCM_RIGHTS` ancillary data (0 or 3: stdin, stdout, stderr).
        ///
        /// The guest's stdio is the **caller's** stdio: the launcher keeps its
        /// pipes (logcat capture, the interaction protocol) and the guest
        /// inherits the same ends, so capture is identical to spawning the
        /// guest itself. Without fds the supervisor falls back to the per-session
        /// log files named (or defaulted) by `stdout`/`stderr`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        fds: Option<u32>,
        /// The process whose lifetime this session is bound to (the launcher's
        /// stdio proxy). When it dies the supervisor detaches the session, so a
        /// `SIGKILL`ed launcher cannot leak a guest, a veth or a rule.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        owner_pid: Option<u32>,
        /// The cgroup v2 path of the run's own scope (`/proc/self/cgroup`'s
        /// `0::…` line): the guest is moved into it before the session counts as
        /// attached, so the run's `memory.max`/`pids.max` bound the guest on the
        /// relay arm too (the supervisor is not inside the scope, so inheriting
        /// it is not automatic). Absent = the caller has no scope to lend.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cgroup: Option<String>,
    },
    /// One session's state, without the relay-wide probes `Status` runs (this is
    /// what a waiting launcher polls).
    Session { session: String },
    /// Attach an existing netns (the proof harness and tests use this; the
    /// product path is [`Request::Spawn`]).
    Attach {
        session: String,
        /// PID whose `ns/net` becomes the session netns.
        netns_pid: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        index: Option<u32>,
    },
    /// Remove the session's veth (which removes its peer inside the session
    /// netns) and every rule the relay installed for it.
    Detach { session: String },
    /// `offline: true` drops the session's traffic, `false` restores it. Nothing
    /// in the guest participates, so it works mid-request.
    Offline { session: String, #[serde(default)] offline: bool },
    /// `netem` shaping on the session's veth (`ms: 0, loss_pct: 0` clears it).
    Delay {
        session: String,
        #[serde(default)]
        ms: u32,
        #[serde(default)]
        jitter_ms: u32,
        #[serde(default)]
        loss_pct: u32,
    },
    /// Explicit host-port opt-in (`8081` for Metro). The port is forwarded by the
    /// relay only when named, and only sessions that opted in may use it.
    HostPort {
        port: u16,
        #[serde(default)]
        on: bool,
        /// Session that gets the port; absent = every attached session.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session: Option<String>,
    },
    /// Kill `pasta`. Without `hold` the supervisor restarts it on the next tick
    /// (the sessions recover). With `hold` the relay stays down until
    /// [`Request::Up`], which is how the fail-closed branch is proved without a
    /// race: while it is held down every networked request must answer
    /// `network-unavailable`.
    KillPasta {
        #[serde(default)]
        hold: bool,
    },
    /// Shut the supervisor down (sessions are killed, the relay netns goes away).
    Shutdown,
}

/// One session, as the supervisor sees it.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SessionInfo {
    pub index: u32,
    pub name: String,
    pub veth: String,
    pub address: String,
    pub gateway: String,
    pub pid: Option<i32>,
    pub offline: bool,
    pub delay_ms: u32,
    pub loss_pct: u32,
    pub host_ports: Vec<u16>,
    pub alive: bool,
    /// The guest's exit status once it has ended (`None` while it runs or while
    /// nobody has reaped it yet). bwrap's `128+signal` remap is the raw wait
    /// status's business; this is `ExitStatus::code()` when there is one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit: Option<i32>,
    /// The guest was killed by this signal (set when `exit` is absent and the
    /// guest died from a signal).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signal: Option<i32>,
}

/// Who the supervisor is: its binary path and a content build id.
///
/// The build id is a hash of the executable's bytes, so a rebuilt binary at the
/// same path is a *different* identity: a stale supervisor that can no longer
/// exec its own fabric (its `/proc/self/exe` was replaced) is detectable, and
/// `acos-netd` can be replaced without a manual `kill`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SupervisorInfo {
    pub pid: i32,
    /// `/proc/self/exe` of the supervisor (the `acos-netd` binary).
    pub exe: String,
    /// Hex FNV-1a 64 of the executable's bytes (`None` when it could not be read).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub build_id: Option<String>,
    /// `CARGO_PKG_VERSION` of the supervisor build.
    pub version: String,
    /// Wall-clock start of the supervisor (Unix ms).
    pub started_ms: u64,
}

/// Relay state.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StatusInfo {
    pub up: bool,
    /// The supervisor itself (identity + build id).
    pub supervisor: SupervisorInfo,
    /// Why the relay is down, when it is.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub down_reason: Option<String>,
    pub fabric_pid: Option<i32>,
    pub pasta_pid: Option<i32>,
    pub pasta_version: Option<String>,
    pub pasta_tag_expected: String,
    pub host_ports: Vec<u16>,
    pub sessions: Vec<SessionInfo>,
    /// Every `acosv*` link the relay netns still has (the leak check: after
    /// detach this is empty).
    pub relay_veths: Vec<String>,
    /// Lines in the relay's per-session rule chain (`ACOS-SESS`, without its
    /// `-N` header). Detach must bring this back to 0.
    pub session_rules: usize,
    pub attach_ms_last: Option<f64>,
}

/// A reply. `ok` first, so a caller can branch before decoding the rest.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Reply {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ProtoError>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<StatusInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session: Option<SessionInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attach_ms: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub identity: Option<SupervisorInfo>,
}

/// The failure payload.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProtoError {
    pub code: ErrorCode,
    pub message: String,
}

impl Reply {
    #[must_use]
    pub fn ok() -> Self {
        Self { ok: true, error: None, status: None, session: None, attach_ms: None, identity: None }
    }

    #[must_use]
    pub fn fail(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            ok: false,
            error: Some(ProtoError { code, message: message.into() }),
            status: None,
            session: None,
            attach_ms: None,
            identity: None,
        }
    }

    #[must_use]
    pub fn status(s: StatusInfo) -> Self {
        Self { status: Some(s), ..Self::ok() }
    }
}

/// One line of the wire format, newline-terminated.
pub fn encode<T: Serialize>(v: &T) -> String {
    format!("{}\n", serde_json::to_string(v).unwrap_or_else(|_| "{\"ok\":false}".into()))
}

/// Decode one request line.
pub fn decode_request(line: &str) -> Result<Request, ProtoError> {
    serde_json::from_str(line.trim_end()).map_err(|e| ProtoError {
        code: ErrorCode::Invalid,
        message: format!("request: {e}"),
    })
}
