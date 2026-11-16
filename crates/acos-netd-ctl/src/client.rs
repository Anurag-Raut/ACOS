//! Client side: find (or start) the host's supervisor, then speak its protocol.
//!
//! This is the surface the session API (CLI/MCP `serve` verbs, later the module
//! framework) wraps: [`Client::ensure_up`], [`Client::status`],
//! [`Client::spawn`], [`Client::detach`], [`Client::offline`], [`Client::delay`],
//! [`Client::host_port`].

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use crate::proto::{self, ErrorCode, ProtoError, Reply, Request, SessionInfo, StatusInfo};
use crate::NETD_SOCK;

/// How long [`Client::ensure_up`] waits for a fresh supervisor to answer.
pub const START_TIMEOUT: Duration = Duration::from_secs(15);

/// Environment variable that selects the transport arm:
/// `relay` (design B2, the default once the module ships) or `base` (the
/// proven per-session `pasta` of the spike, kept as the fallback arm for one
/// release).
pub const ARM_ENV: &str = "ACOS_NET_ARM";

/// The transport arm in use.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Arm {
    /// Per-session `pasta` (the spike's transport).
    Base,
    /// Design B2: one relay netns + one `pasta` for the host, one netns + veth
    /// per session.
    Relay,
}

impl Arm {
    /// Read [`ARM_ENV`]; anything but `base`/`relay` is an error, never a
    /// silent default (a typo must not switch the transport).
    pub fn from_env() -> Result<Self, ProtoError> {
        match std::env::var(ARM_ENV).as_deref() {
            Ok("relay") => Ok(Self::Relay),
            Ok("base") => Ok(Self::Base),
            Ok(other) => Err(ProtoError {
                code: ErrorCode::Invalid,
                message: format!("{ARM_ENV}={other}: expected `relay` or `base`"),
            }),
            Err(_) => Ok(Self::Relay),
        }
    }
}

/// The short, private state directory (`/run/user/<uid>/acos-netd`).
///
/// Short because the control socket is an `AF_UNIX` path (108 bytes), private
/// because it is the handle to the host's only NAT process.
pub fn state_dir() -> PathBuf {
    let uid = unsafe { libc::geteuid() };
    let base = if let Ok(d) = std::env::var("ACOS_NETD_STATE") {
        PathBuf::from(d)
    } else {
        PathBuf::from(format!("/run/user/{uid}/acos-netd"))
    };
    let _ = std::fs::create_dir_all(&base);
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(&base, std::fs::Permissions::from_mode(0o700));
    base
}

/// The supervisor's control socket.
#[must_use]
pub fn socket_path() -> PathBuf {
    state_dir().join(NETD_SOCK)
}

/// The `acos-netd` binary a supervisor is started from: `ACOS_NETD_BIN`, else the
/// one built next to this executable, else `PATH`.
///
/// **Not** `current_exe()`: the client can be `acos`, `acos-art` or an MCP
/// request, and `<acos> serve` is the *MCP daemon*, not the relay supervisor. The
/// supervisor is always the `acos-netd` binary of the same build.
#[must_use]
pub fn netd_bin() -> PathBuf {
    if let Some(p) = std::env::var_os("ACOS_NETD_BIN") {
        return PathBuf::from(p);
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let cand = dir.join("acos-netd");
            if cand.is_file() {
                return cand;
            }
        }
    }
    PathBuf::from("acos-netd")
}

/// FNV-1a 64 of a file's bytes, as 16 hex digits; `None` when it cannot be read.
///
/// A content hash is the identity that matters: the same path rebuilt is a
/// different binary, and a supervisor built from it can no longer exec its own
/// fabric (its `/proc/self/exe` was replaced). `mtime`/size would also change,
/// but a hash is not fooled by a restored mtime.
#[must_use]
pub fn file_build_id(path: &std::path::Path) -> Option<String> {
    use std::io::Read;
    let mut f = std::fs::File::open(path).ok()?;
    let mut buf = vec![0u8; 1 << 20];
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    loop {
        let n = f.read(&mut buf).ok()?;
        if n == 0 {
            break;
        }
        for b in &buf[..n] {
            hash ^= u64::from(*b);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    Some(format!("{hash:016x}"))
}

/// This process's own cgroup v2 path (`/proc/self/cgroup`'s `0::…` line), the
/// scope a guest spawned from here must join to be bounded by `--mem`.
#[must_use]
pub fn own_cgroup() -> Option<String> {
    let text = std::fs::read_to_string("/proc/self/cgroup").ok()?;
    text.lines()
        .find_map(|l| l.strip_prefix("0::").map(|p| p.trim().to_string()))
        .filter(|p| !p.is_empty())
}

/// The supervisor's identity as *this* client sees it: the binary it would start
/// and that binary's content hash.
#[must_use]
pub fn expected_identity() -> (PathBuf, Option<String>) {
    let exe = netd_bin();
    let id = file_build_id(&exe);
    (exe, id)
}

/// This process's identity when it *is* a supervisor.
#[must_use]
pub fn self_identity() -> crate::proto::SupervisorInfo {
    let exe = std::env::current_exe().unwrap_or_default();
    let started_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    crate::proto::SupervisorInfo {
        pid: i32::try_from(std::process::id()).unwrap_or(-1),
        exe: exe.to_string_lossy().to_string(),
        build_id: file_build_id(&exe),
        version: env!("CARGO_PKG_VERSION").to_string(),
        started_ms,
    }
}

/// A connected supervisor.
pub struct Client {
    stream: UnixStream,
}

impl Client {
    /// Connect to an already-running supervisor.
    ///
    /// A supervisor left over from **another build** is replaced cleanly: its
    /// report of its own binary path and content build id is compared with the
    /// binary this client would start, and a mismatch (a rebuild at the same
    /// path, or a binary that no longer exists) shuts it down and starts the
    /// current one. Without this a rebuilt `acos-netd` leaves a supervisor whose
    /// fabric cannot exec (`unshare: failed to execute … (deleted)`) and every
    /// attach answers `network-unavailable` until someone kills it by hand.
    pub fn connect() -> Result<Self, ProtoError> {
        let mut c = Self::connect_raw()?;
        let stale = match c.identity() {
            Ok(running) => stale_reason(&running),
            // A supervisor that does not know `Identity` predates this build:
            // stale by definition.
            Err(_) => Some("it does not answer the identity request".to_string()),
        };
        let Some(reason) = stale else { return Ok(c) };
        eprintln!("acos-netd: replacing the relay supervisor: {reason}");
        let pid = c.identity().map(|i| i.pid).unwrap_or(-1);
        let _ = c.request(&Request::Shutdown);
        drop(c);
        if !gone_within(Duration::from_secs(3)) && pid > 0 {
            // A supervisor from a build that predates "shutdown exits" stays
            // bound to its socket. It has been identified by its own report (pid
            // + binary + build id), so stop it: SIGTERM, then SIGKILL, and only
            // if `/proc/<pid>/exe` is the binary it said it was.
            eprintln!("acos-netd: stale supervisor {pid} did not exit on shutdown; signalling it");
            terminate(pid);
        }
        // A supervisor that would not go away is not something to fight over
        // (two supervisors must never exist), but it must not look healthy
        // either: report the exact reason.
        if Self::connect_raw().is_ok() {
            return Err(ProtoError {
                code: ErrorCode::SupervisorGone,
                message: format!("a stale supervisor is still running: {reason}"),
            });
        }
        Self::start_supervisor()?;
        let deadline = Instant::now() + START_TIMEOUT;
        let mut last = None;
        while Instant::now() < deadline {
            if let Ok(c) = Self::connect_raw() {
                return Ok(c);
            }
            last = Some("the replacement supervisor did not come up".to_string());
            std::thread::sleep(Duration::from_millis(20));
        }
        Err(ProtoError {
            code: ErrorCode::SupervisorGone,
            message: last.unwrap_or_else(|| "no supervisor".into()),
        })
    }

    /// Connect without the identity check (the check itself needs a connection).
    pub fn connect_raw() -> Result<Self, ProtoError> {
        let path = socket_path();
        let stream = UnixStream::connect(&path).map_err(|e| ProtoError {
            code: ErrorCode::SupervisorGone,
            message: format!("{}: {e}", path.display()),
        })?;
        stream.set_read_timeout(Some(Duration::from_secs(120))).ok();
        Ok(Self { stream })
    }

    /// The supervisor's own identity (binary path + content build id).
    pub fn identity(&mut self) -> Result<crate::proto::SupervisorInfo, ProtoError> {
        let mut r = self.request(&Request::Identity)?;
        r.identity.take().ok_or_else(|| no_payload(&r))
    }

    /// Connect, starting the supervisor when it is not running.
    ///
    /// The supervisor is started in its **own user namespace**
    /// (`unshare --user --map-root-user`), because both the relay netns and
    /// every session netns are owned by it: an unprivileged process cannot
    /// create a netns in the host's user namespace, and a veth can only be moved
    /// into a netns of the same (or a descendant) user namespace.
    pub fn ensure_up() -> Result<Self, ProtoError> {
        // A live supervisor is never replaced by a second one: two supervisors
        // would mean two relay netns and two `pasta` processes for one host.
        if let Ok(mut c) = Self::connect() {
            match c.status() {
                Ok(s) if s.up => return Ok(c),
                Ok(_) => {
                    // The supervisor is alive but its relay is down (pasta died,
                    // or it is held down): it is the only process that can
                    // restart it, so ask it to.
                    let r = c.request(&Request::Up { pasta: None })?;
                    if r.status.as_ref().is_some_and(|s| s.up) {
                        return Ok(c);
                    }
                    return Err(r.error.map_or_else(
                        || ProtoError {
                            code: ErrorCode::NetworkUnavailable,
                            message: "the network relay is down".into(),
                        },
                        |e| e,
                    ));
                }
                Err(_) => {}
            }
        }
        Self::start_supervisor()?;
        let deadline = Instant::now() + START_TIMEOUT;
        let mut last = None;
        while Instant::now() < deadline {
            match Self::connect() {
                Ok(mut c) => match c.status() {
                    Ok(s) if s.up => return Ok(c),
                    Ok(s) => last = s.down_reason.or(last),
                    Err(e) => last = Some(e.message),
                },
                Err(e) => last = Some(e.message),
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        Err(ProtoError {
            code: ErrorCode::NetworkUnavailable,
            message: format!(
                "the network relay did not come up: {}",
                last.unwrap_or_else(|| "no status".into())
            ),
        })
    }

    /// Connect to the running supervisor, or bootstrap one. Unlike
    /// [`Self::ensure_up`] this never asks an existing supervisor to recover:
    /// a session attach on a down relay must answer `network-unavailable`
    /// (fail closed), not silently restart the network under the caller.
    pub fn attach_client() -> Result<Self, ProtoError> {
        match Self::connect() {
            Ok(c) => Ok(c),
            Err(_) => Self::ensure_up(),
        }
    }

    fn start_supervisor() -> Result<(), ProtoError> {
        use std::os::unix::process::CommandExt;
        use std::process::{Command, Stdio};
        let exe = netd_bin();
        let dir = state_dir();
        let log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join("supervisor.log"))
            .map_err(|e| ProtoError { code: ErrorCode::Io, message: e.to_string() })?;
        let mut cmd = Command::new("unshare");
        cmd.arg("--user").arg("--map-root-user").arg("--").arg(exe).arg("serve");
        // Inside `--map-root-user` the supervisor's euid is 0, so it cannot
        // derive the user's runtime directory itself: it is handed over
        // explicitly (the same directory the client bound the socket in).
        cmd.env("ACOS_NETD_STATE", &dir);
        cmd.stdin(Stdio::null())
            .stdout(log.try_clone().map_err(|e| ProtoError {
                code: ErrorCode::Io,
                message: e.to_string(),
            })?)
            .stderr(log);
        unsafe {
            // `setsid`: the supervisor outlives the session that started it (the
            // relay is per host, not per session).
            cmd.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
        let child = cmd.spawn().map_err(|e| ProtoError {
            code: ErrorCode::NetworkUnavailable,
            message: format!("cannot start the relay supervisor: {e}"),
        })?;
        // The child is detached on purpose; it is reaped by init after this
        // process exits, and `ensure_up` talks to it over the socket.
        std::mem::forget(child);
        Ok(())
    }

    /// Send one request and read one reply.
    pub fn request(&mut self, req: &Request) -> Result<Reply, ProtoError> {
        let line = proto::encode(req);
        self.stream.write_all(line.as_bytes()).map_err(io)?;
        self.stream.flush().map_err(io)?;
        let mut reply_line = String::new();
        BufReader::new(self.stream.try_clone().map_err(io)?)
            .read_line(&mut reply_line)
            .map_err(io)?;
        if reply_line.trim().is_empty() {
            return Err(ProtoError {
                code: ErrorCode::SupervisorGone,
                message: "the supervisor closed the connection".into(),
            });
        }
        serde_json::from_str(reply_line.trim_end()).map_err(|e| ProtoError {
            code: ErrorCode::Io,
            message: format!("reply: {e} ({reply_line:?})"),
        })
    }

    /// Relay state, including `up` and the reason it is down.
    pub fn status(&mut self) -> Result<StatusInfo, ProtoError> {
        let mut r = self.request(&Request::Status)?;
        r.status.take().ok_or_else(|| no_payload(&r))
    }

    /// Spawn a command in a new session netns joined to the relay.
    pub fn spawn(
        &mut self,
        session: &str,
        argv: Vec<String>,
        stdout: Option<String>,
        stderr: Option<String>,
    ) -> Result<(SessionInfo, f64), ProtoError> {
        self.spawn_with(session, argv, stdout, stderr, None)
    }

    /// [`Self::spawn_with`] with the guest's own stdio descriptors
    /// (stdin, stdout, stderr) passed to the supervisor as `SCM_RIGHTS`.
    ///
    /// This is what the ART launcher uses: it keeps its pipes and hands them
    /// over, so the guest writes logcat and the interaction protocol into the
    /// same ends the launcher reads. `owner_pid` binds the session's lifetime to
    /// the calling launcher (its death detaches the session).
    pub fn spawn_stdio(
        &mut self,
        session: &str,
        argv: Vec<String>,
        fds: &[std::os::unix::io::RawFd],
        owner_pid: Option<u32>,
        cgroup: Option<String>,
    ) -> Result<(SessionInfo, f64), ProtoError> {
        let req = Request::Spawn {
            session: session.to_string(),
            argv,
            index: None,
            stdout: None,
            stderr: None,
            ready_timeout_s: None,
            die_with_relay: None,
            fds: Some(u32::try_from(fds.len()).unwrap_or(0)),
            owner_pid,
            cgroup,
        };
        let mut r = self.request_with_fds(&req, fds)?;
        let ms = r.attach_ms.unwrap_or(0.0);
        match r.session.take() {
            Some(s) => Ok((s, ms)),
            None => Err(no_payload(&r)),
        }
    }

    /// One session's state, without the relay-wide probes `status()` runs.
    pub fn session(&mut self, name: &str) -> Result<SessionInfo, ProtoError> {
        let mut r = self.request(&Request::Session { session: name.to_string() })?;
        r.session.take().ok_or_else(|| no_payload(&r))
    }

    /// Block until `name`'s guest has ended, then return its exit code
    /// (`128 + signal` when it died from a signal). `None` when the session is
    /// gone without a recorded status (the relay's fail-closed teardown).
    pub fn wait_exit(&mut self, name: &str, poll: Duration) -> Result<Option<i32>, ProtoError> {
        // The guest's pid can be gone a moment before the supervisor has reaped
        // it (`Session` reaps on query, the background tick every 500 ms), so a
        // dead-but-statusless session is polled for a short grace window instead
        // of being reported as "exited, status unknown".
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            let s = self.session(name)?;
            if !s.alive && (s.exit.is_some() || s.signal.is_some()) {
                return Ok(match s.signal {
                    Some(sig) => Some(128 + sig),
                    None => s.exit,
                });
            }
            if !s.alive && Instant::now() > deadline {
                return Ok(None);
            }
            std::thread::sleep(poll);
        }
    }

    /// Send one request with descriptors attached (`SCM_RIGHTS`).
    pub fn request_with_fds(
        &mut self,
        req: &Request,
        fds: &[std::os::unix::io::RawFd],
    ) -> Result<Reply, ProtoError> {
        use std::os::unix::io::AsRawFd;
        let line = proto::encode(req);
        if !fds.is_empty() {
            send_with_fds(&self.stream, line.as_bytes(), fds)?;
        } else {
            self.stream.write_all(line.as_bytes()).map_err(io)?;
        }
        self.stream.flush().map_err(io)?;
        let _ = AsRawFd::as_raw_fd(&self.stream);
        let mut reply_line = String::new();
        BufReader::new(self.stream.try_clone().map_err(io)?)
            .read_line(&mut reply_line)
            .map_err(io)?;
        if reply_line.trim().is_empty() {
            return Err(ProtoError {
                code: ErrorCode::SupervisorGone,
                message: "the supervisor closed the connection".into(),
            });
        }
        serde_json::from_str(reply_line.trim_end()).map_err(|e| ProtoError {
            code: ErrorCode::Io,
            message: format!("reply: {e} ({reply_line:?})"),
        })
    }

    /// [`Self::spawn`] with an explicit address-block index.
    pub fn spawn_with(
        &mut self,
        session: &str,
        argv: Vec<String>,
        stdout: Option<String>,
        stderr: Option<String>,
        index: Option<u32>,
    ) -> Result<(SessionInfo, f64), ProtoError> {
        let mut r = self.request(&Request::Spawn {
            session: session.to_string(),
            argv,
            index,
            stdout,
            stderr,
            ready_timeout_s: None,
            die_with_relay: None,
            fds: None,
            owner_pid: None,
            cgroup: None,
        })?;
        let ms = r.attach_ms.unwrap_or(0.0);
        match r.session.take() {
            Some(s) => Ok((s, ms)),
            None => Err(no_payload(&r)),
        }
    }

    /// Attach an existing netns (harness/tests).
    pub fn attach(&mut self, session: &str, netns_pid: u32) -> Result<SessionInfo, ProtoError> {
        let mut r = self.request(&Request::Attach {
            session: session.to_string(),
            netns_pid,
            index: None,
        })?;
        r.session.take().ok_or_else(|| no_payload(&r))
    }

    /// Tear a session down (veth, rules, guest).
    pub fn detach(&mut self, session: &str) -> Result<SessionInfo, ProtoError> {
        let mut r = self.request(&Request::Detach { session: session.to_string() })?;
        r.session.take().ok_or_else(|| no_payload(&r))
    }

    /// Per-session online/offline.
    pub fn offline(&mut self, session: &str, offline: bool) -> Result<SessionInfo, ProtoError> {
        let mut r = self.request(&Request::Offline { session: session.to_string(), offline })?;
        r.session.take().ok_or_else(|| no_payload(&r))
    }

    /// Per-session latency/loss (`ms = 0, loss_pct = 0` clears).
    pub fn delay(
        &mut self,
        session: &str,
        ms: u32,
        jitter_ms: u32,
        loss_pct: u32,
    ) -> Result<SessionInfo, ProtoError> {
        let mut r = self.request(&Request::Delay { session: session.to_string(), ms, jitter_ms, loss_pct })?;
        r.session.take().ok_or_else(|| no_payload(&r))
    }

    /// Explicit host-port opt-in.
    pub fn host_port(
        &mut self,
        port: u16,
        on: bool,
        session: Option<&str>,
    ) -> Result<StatusInfo, ProtoError> {
        let mut r = self.request(&Request::HostPort {
            port,
            on,
            session: session.map(str::to_string),
        })?;
        r.status.take().ok_or_else(|| no_payload(&r))
    }

    /// Kill `pasta` on purpose (fail-closed proof). `hold` keeps the relay down
    /// until the next `ensure_up`.
    pub fn kill_pasta(&mut self, hold: bool) -> Result<(), ProtoError> {
        let mut r = self.request(&Request::KillPasta { hold })?;
        if r.ok { Ok(()) } else { Err(payload_error(&r)) }
    }

    /// Stop the supervisor and the relay netns.
    pub fn shutdown(&mut self) -> Result<(), ProtoError> {
        let mut r = self.request(&Request::Shutdown)?;
        if r.ok { Ok(()) } else { Err(payload_error(&r)) }
    }
}

/// `sendmsg` one line with descriptors as `SCM_RIGHTS` ancillary data.
fn send_with_fds(
    sock: &UnixStream,
    line: &[u8],
    fds: &[std::os::unix::io::RawFd],
) -> Result<(), ProtoError> {
    use std::os::unix::io::AsRawFd;
    let mut iov = libc::iovec { iov_base: line.as_ptr() as *mut _, iov_len: line.len() };
    let space = unsafe { libc::CMSG_SPACE((fds.len() * std::mem::size_of::<i32>()) as u32) } as usize;
    let mut control = vec![0u8; space];
    let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
    msg.msg_iov = &mut iov;
    msg.msg_iovlen = 1;
    msg.msg_control = control.as_mut_ptr().cast();
    msg.msg_controllen = space;
    unsafe {
        let cmsg = libc::CMSG_FIRSTHDR(&msg);
        (*cmsg).cmsg_level = libc::SOL_SOCKET;
        (*cmsg).cmsg_type = libc::SCM_RIGHTS;
        (*cmsg).cmsg_len =
            libc::CMSG_LEN((fds.len() * std::mem::size_of::<i32>()) as u32) as usize;
        let data = libc::CMSG_DATA(cmsg).cast::<std::os::unix::io::RawFd>();
        for (i, fd) in fds.iter().enumerate() {
            *data.add(i) = *fd;
        }
    }
    let n = unsafe { libc::sendmsg(sock.as_raw_fd(), &msg, libc::MSG_NOSIGNAL) };
    if n < 0 {
        return Err(io(std::io::Error::last_os_error()));
    }
    Ok(())
}

/// True when nothing answers on the supervisor's socket.
fn gone_within(window: Duration) -> bool {
    let deadline = Instant::now() + window;
    while Instant::now() < deadline {
        if Client::connect_raw().is_err() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    Client::connect_raw().is_err()
}

/// Stop a supervisor that did not exit on `Shutdown`: SIGTERM, wait, SIGKILL.
///
/// Only the process that reported itself as the supervisor is signalled, and only
/// while `/proc/<pid>/exe` still resolves to the binary it named, so a recycled
/// pid can never be hit.
fn terminate(pid: i32) {
    let exe = std::fs::read_link(format!("/proc/{pid}/exe"))
        .map(|p| p.to_string_lossy().trim_end_matches(" (deleted)").to_string())
        .unwrap_or_default();
    if exe.is_empty() || !exe.contains("acos-netd") {
        eprintln!("acos-netd: not signalling pid {pid} (exe {exe:?})");
        return;
    }
    for (sig, window) in [
        (libc::SIGTERM, Duration::from_secs(3)),
        (libc::SIGKILL, Duration::from_secs(3)),
    ] {
        unsafe { libc::kill(pid, sig) };
        if gone_within(window) {
            return;
        }
    }
}

/// Why a running supervisor must be replaced, or `None` when it is this build.
///
/// The comparison is **content**, not path: a rebuild at the same path is a
/// different identity, and a binary that has been deleted cannot be exec'd at
/// all (`current_exe()` of the running supervisor points at a `(deleted)` file).
#[must_use]
pub fn stale_reason(running: &crate::proto::SupervisorInfo) -> Option<String> {
    if !std::path::Path::new(&running.exe).exists() {
        return Some(format!(
            "its binary {} no longer exists (it cannot exec its fabric)",
            running.exe
        ));
    }
    let (exe, build) = expected_identity();
    match (&build, &running.build_id) {
        (Some(want), Some(have)) if want != have => Some(format!(
            "it runs {exe} build {have}, this client has build {want}",
            exe = exe.display()
        )),
        (Some(_), None) => Some("it does not report a build id".to_string()),
        _ => None,
    }
}

fn io(e: std::io::Error) -> ProtoError {
    ProtoError { code: ErrorCode::Io, message: e.to_string() }
}

fn payload_error(r: &Reply) -> ProtoError {
    r.error.clone().unwrap_or(ProtoError {
        code: ErrorCode::Io,
        message: "reply has no error".into(),
    })
}

fn no_payload(r: &Reply) -> ProtoError {
    if r.ok {
        ProtoError { code: ErrorCode::Io, message: "reply is missing its payload".into() }
    } else {
        payload_error(r)
    }
}
