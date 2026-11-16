//! The relay supervisor: one relay netns + one `pasta` per host, one netns +
//! veth per session (design **B2**, ADR-0030 amendment 3).
//!
//! ```text
//! supervisor (this process: root in the user namespace it was started in)
//!  ├─ acos-netd fabric        relay netns: tap0 (pasta), every session's veth
//!  │                          end, and all drop/control rules
//!  ├─ pasta --netns <fabric>  the only NAT process on the host
//!  └─ per session: `unshare -n` child (its own netns) joined by a veth pair
//! ```
//!
//! Why the supervisor spawns the session instead of the session attaching
//! itself: moving a veth end into a netns requires `CAP_NET_ADMIN` in the user
//! namespace that owns **that** netns, and capability only flows from an
//! ancestor user namespace. The relay netns and every session netns must be
//! owned by the same user namespace (the supervisor's) for the veth move to be
//! possible at all; a session launched by an unrelated process owns its netns in
//! a user namespace of its own and the move is denied by the kernel. The spike
//! had the same shape — `pasta` spawned the guest inside the netns it had
//! created — so this is the spike's model, not a new constraint.
//!
//! Fail closed: while the relay is down (`pasta` died and has not been restarted,
//! or the fabric never came up) every networked request is answered
//! `network-unavailable`, never "offline".

use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use crate::plan::{self, SessionNet};
use crate::proto::{ErrorCode, ProtoError, Request, SessionInfo, StatusInfo};

/// How long the supervisor waits for `pasta` to configure `tap0` in the relay
/// netns. Measured 60–200 ms; a second is already pathological.
const TAP_TIMEOUT: Duration = Duration::from_secs(5);
/// How often the supervision thread checks `pasta`'s liveness.
pub const TICK: Duration = Duration::from_millis(500);

/// The name of the fabric's control socket inside the state directory.
const FABRIC_SOCK: &str = "fabric.sock";
/// The supervisor's own control socket inside the state directory.
pub const NETD_SOCK: &str = "netd.sock";

/// One attached session.
struct Session {
    net: SessionNet,
    name: String,
    pid: Option<i32>,
    child: Option<Child>,
    offline: bool,
    delay_ms: u32,
    loss_pct: u32,
    host_ports: BTreeSet<u16>,
    /// Every rule this supervisor installed for the session, exactly as it was
    /// written, so detach can remove precisely those and nothing else.
    rules: Vec<String>,
}

impl Session {
    fn info(&self) -> SessionInfo {
        SessionInfo {
            index: self.net.idx,
            name: self.name.clone(),
            veth: self.net.veth.clone(),
            address: plan::ip(self.net.address),
            gateway: plan::ip(self.net.gateway),
            pid: self.pid,
            offline: self.offline,
            delay_ms: self.delay_ms,
            loss_pct: self.loss_pct,
            host_ports: self.host_ports.iter().copied().collect(),
            alive: self.pid.is_some_and(pid_alive),
        }
    }
}

/// Is `pid` still running (and not a zombie)?
fn pid_alive(pid: i32) -> bool {
    let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
        return false;
    };
    // Field 3 is the state; a zombie (`Z`) is dead for our purposes.
    stat.rsplit(')').next().and_then(|s| s.trim_start().chars().next()) != Some('Z')
}

/// The supervisor's state. One instance per host.
pub struct Relay {
    state_dir: PathBuf,
    pasta_override: Option<PathBuf>,
    fabric: Option<Child>,
    fabric_pid: Option<i32>,
    pasta: Option<Child>,
    pasta_version: Option<String>,
    up: bool,
    down_reason: Option<String>,
    host_ports: BTreeSet<u16>,
    sessions: BTreeMap<String, Session>,
    attach_ms_last: Option<f64>,
    /// The relay is deliberately held down (fail-closed proof); [`Relay::ensure_up`]
    /// clears it.
    hold_down: bool,
}

impl Relay {
    #[must_use]
    pub fn new(state_dir: PathBuf) -> Self {
        Self {
            state_dir,
            pasta_override: None,
            fabric: None,
            fabric_pid: None,
            pasta: None,
            pasta_version: None,
            up: false,
            down_reason: Some("relay not started".into()),
            host_ports: BTreeSet::new(),
            sessions: BTreeMap::new(),
            attach_ms_last: None,
            hold_down: false,
        }
    }

    fn log(&self, msg: &str) {
        let path = self.state_dir.join("netd.log");
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
            let _ = writeln!(f, "{msg}");
        }
    }

    fn fabric_sock(&self) -> PathBuf {
        self.state_dir.join(FABRIC_SOCK)
    }

    // -- fabric -------------------------------------------------------------

    /// The relay netns' executor: one long-lived child of the supervisor.
    ///
    /// `AF_UNIX` sockets are not namespaced by the network namespace, so the
    /// fabric's socket is reachable from the host namespace where the supervisor
    /// lives, even though the fabric itself runs inside the relay netns.
    fn start_fabric(&mut self) -> Result<(), ProtoError> {
        if let Some(pid) = self.fabric_pid {
            if pid_alive(pid) {
                return Ok(());
            }
        }
        let sock = self.fabric_sock();
        let _ = std::fs::remove_file(&sock);
        let exe = std::env::current_exe().map_err(io_err)?;
        let log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.state_dir.join("fabric.log"))
            .map_err(io_err)?;
        let child = Command::new("unshare")
            .arg("-n")
            .arg("--")
            .arg(exe)
            .arg("fabric")
            .arg("--socket")
            .arg(&sock)
            .stdin(Stdio::null())
            .stdout(log.try_clone().map_err(io_err)?)
            .stderr(log)
            .spawn()
            .map_err(|e| ProtoError {
                code: ErrorCode::NetworkUnavailable,
                message: format!("cannot start the relay netns (`unshare -n`): {e}"),
            })?;
        let pid = i32::try_from(child.id()).unwrap_or(-1);
        self.fabric = Some(child);
        self.fabric_pid = Some(pid);
        let deadline = Instant::now() + TAP_TIMEOUT;
        while Instant::now() < deadline {
            if sock.exists() && self.fabric_run("true").is_ok() {
                self.log(&format!("fabric up pid={pid}"));
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        Err(ProtoError {
            code: ErrorCode::NetworkUnavailable,
            message: "the relay netns did not come up".into(),
        })
    }

    /// Run one shell command inside the relay netns.
    fn fabric_run(&self, cmd: &str) -> Result<String, ProtoError> {
        // One connection per command: the fabric is a 2-line request, and a
        // fresh connection has no ordering state to get wrong.
        let mut s = UnixStream::connect(self.fabric_sock()).map_err(|e| ProtoError {
            code: ErrorCode::NetworkUnavailable,
            message: format!("relay netns unreachable: {e}"),
        })?;
        s.set_read_timeout(Some(Duration::from_secs(30))).ok();
        writeln!(s, "{cmd}").map_err(io_err)?;
        let mut line = String::new();
        BufReader::new(s).read_line(&mut line).map_err(io_err)?;
        let v: serde_json::Value = serde_json::from_str(&line).map_err(|e| ProtoError {
            code: ErrorCode::RelayCommand,
            message: format!("relay reply: {e} ({line:?})"),
        })?;
        let rc = v.get("rc").and_then(serde_json::Value::as_i64).unwrap_or(-1);
        let out = v.get("out").and_then(serde_json::Value::as_str).unwrap_or("");
        if rc != 0 {
            return Err(ProtoError {
                code: ErrorCode::RelayCommand,
                message: format!("relay command failed rc={rc}: {cmd}: {}", tail(out)),
            });
        }
        Ok(out.to_string())
    }

    fn fabric_run_all(&self, cmds: &[String]) -> Result<(), ProtoError> {
        for c in cmds {
            self.fabric_run(c)?;
        }
        Ok(())
    }

    // -- pasta --------------------------------------------------------------

    fn pasta_bin(&self) -> PathBuf {
        if let Some(p) = &self.pasta_override {
            return p.clone();
        }
        if let Some(p) = std::env::var_os(plan::PASTA_ENV) {
            return PathBuf::from(p);
        }
        // The installer's location, then PATH: the spike's binary is a static
        // build shipped with the install (see `acos doctor`).
        if let Some(p) = std::env::var_os("ACOS_PREFIX") {
            let cand = PathBuf::from(p).join("libexec/pasta");
            if cand.exists() {
                return cand;
            }
        }
        PathBuf::from("pasta")
    }

    fn pasta_alive(&mut self) -> bool {
        match self.pasta.as_mut() {
            Some(child) => match child.try_wait() {
                Ok(None) => true,
                _ => false,
            },
            None => false,
        }
    }

    /// Start `pasta` for the relay netns and wait until `tap0` exists there.
    fn start_pasta(&mut self) -> Result<(), ProtoError> {
        let fabric_pid = self.fabric_pid.ok_or_else(|| ProtoError {
            code: ErrorCode::NetworkUnavailable,
            message: "no relay netns".into(),
        })?;
        let bin = self.pasta_bin();
        let ports: Vec<u16> = self.host_ports.iter().copied().collect();
        let log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.state_dir.join("pasta.log"))
            .map_err(io_err)?;
        let mut cmd = Command::new(&bin);
        cmd.arg("--netns")
            .arg(format!("/proc/{fabric_pid}/ns/net"))
            .args(plan::relay_pasta_args(&ports))
            .stdin(Stdio::null())
            .stdout(log.try_clone().map_err(io_err)?)
            .stderr(log);
        let child = cmd.spawn().map_err(|e| ProtoError {
            code: ErrorCode::NetworkUnavailable,
            message: format!("cannot start pasta ({}): {e}", bin.display()),
        })?;
        let pid = i32::try_from(child.id()).unwrap_or(-1);
        self.pasta = Some(child);
        self.log(&format!("pasta started pid={pid} ports={ports:?}"));
        let deadline = Instant::now() + TAP_TIMEOUT;
        while Instant::now() < deadline {
            if !self.pasta_alive() {
                let tail = read_tail(&self.state_dir.join("pasta.log"), 400);
                self.pasta = None;
                return Err(ProtoError {
                    code: ErrorCode::NetworkUnavailable,
                    message: format!("pasta exited during startup: {tail}"),
                });
            }
            if self
                .fabric_run(&format!("ip link show {}", plan::RELAY_IFACE))
                .is_ok()
            {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        Err(ProtoError {
            code: ErrorCode::NetworkUnavailable,
            message: format!("pasta did not create {} in the relay netns", plan::RELAY_IFACE),
        })
    }

    fn pasta_version(&self) -> Option<String> {
        let out = Command::new(self.pasta_bin()).arg("--version").output().ok()?;
        let text = String::from_utf8_lossy(&out.stdout);
        let line = text.lines().next().unwrap_or("").trim().to_string();
        line.split_whitespace().last().map(str::to_string)
    }

    /// Bring the relay up (idempotent). Installs the boundary **before** any
    /// session can attach.
    pub fn ensure_up(&mut self, pasta: Option<String>) -> Result<(), ProtoError> {
        if let Some(p) = pasta {
            self.pasta_override = Some(PathBuf::from(p));
        }
        if self.up && self.pasta_alive() {
            return Ok(());
        }
        self.hold_down = false;
        self.start_fabric()?;
        if !self.pasta_alive() {
            self.start_pasta()?;
        }
        if self.pasta_version.is_none() {
            self.pasta_version = self.pasta_version();
        }
        self.fabric_run_all(&plan::relay_rules())?;
        self.up = true;
        self.down_reason = None;
        self.log("relay up");
        Ok(())
    }

    fn down(&mut self, reason: String) {
        self.up = false;
        self.down_reason = Some(reason.clone());
        self.log(&format!("relay down: {reason}"));
    }

    /// One supervision tick: restart `pasta` when it died, and reap sessions.
    pub fn tick(&mut self) {
        for s in self.sessions.values_mut() {
            if let Some(child) = s.child.as_mut() {
                if let Ok(Some(status)) = child.try_wait() {
                    self_log(
                        &self.state_dir,
                        &format!("session {} exited: {status}", s.name),
                    );
                    s.child = None;
                    s.pid = None;
                }
            }
        }
        if self.fabric_pid.is_some_and(|p| !pid_alive(p)) {
            self.fabric_pid = None;
            self.down("the relay netns died".into());
        }
        if self.up && !self.pasta_alive() {
            let reason = "pasta died".to_string();
            self.down(reason);
        }
        if self.hold_down {
            return;
        }
        if !self.up && self.fabric_pid.is_some() {
            // Restart the relay: sessions keep their veths and default routes,
            // so they recover as soon as pasta has configured the tap again.
            match self.start_pasta().and_then(|()| self.fabric_run_all(&plan::relay_rules())) {
                Ok(()) => {
                    self.up = true;
                    self.down_reason = None;
                    self.log("relay restarted");
                }
                Err(e) => self.log(&format!("relay restart failed: {}", e.message)),
            }
        }
    }

    fn require_up(&mut self) -> Result<(), ProtoError> {
        if self.up && self.pasta_alive() {
            return Ok(());
        }
        Err(ProtoError {
            code: ErrorCode::NetworkUnavailable,
            message: self
                .down_reason
                .clone()
                .unwrap_or_else(|| "the network relay is unavailable".into()),
        })
    }

    // -- sessions -----------------------------------------------------------

    fn free_index(&self) -> Option<u32> {
        (1..=acos_services::connectivity::NET_SESSION_MAX_INDEX)
            .find(|i| !self.sessions.values().any(|s| s.net.idx == *i))
    }

    fn alloc(&self, name: &str, index: Option<u32>) -> Result<SessionNet, ProtoError> {
        if self.sessions.contains_key(name) {
            return Err(ProtoError {
                code: ErrorCode::Invalid,
                message: format!("session {name} is already attached"),
            });
        }
        let idx = match index {
            Some(i) => {
                if self.sessions.values().any(|s| s.net.idx == i) {
                    return Err(ProtoError {
                        code: ErrorCode::Invalid,
                        message: format!("session index {i} is already attached"),
                    });
                }
                i
            }
            None => self.free_index().ok_or_else(|| ProtoError {
                code: ErrorCode::Invalid,
                message: "no free session index".into(),
            })?,
        };
        plan::session(idx).ok_or_else(|| ProtoError {
            code: ErrorCode::Invalid,
            message: format!("session index {idx} has no address block"),
        })
    }

    /// Create a session netns and run `argv` inside it, joined to the relay by a
    /// veth pair.
    pub fn spawn(&mut self, req: &Request) -> Result<(SessionInfo, f64), ProtoError> {
        let Request::Spawn { session, argv, index, stdout, stderr, ready_timeout_s, die_with_relay } =
            req
        else {
            unreachable!("spawn() is called with Request::Spawn");
        };
        self.require_up()?;
        if argv.is_empty() {
            return Err(ProtoError { code: ErrorCode::Invalid, message: "empty argv".into() });
        }
        let net = self.alloc(session, *index)?;
        let ready = self.state_dir.join(format!("ready-{}.txt", net.idx));
        let _ = std::fs::remove_file(&ready);
        let shim = plan::session_shim(&net, &ready.to_string_lossy());
        let t0 = Instant::now();
        let stdout_f = open_stdio(stdout.as_deref(), &self.state_dir, "session-out.log")?;
        let stderr_f = open_stdio(stderr.as_deref(), &self.state_dir, "session-err.log")?;
        // `unshare -n` in *this* user namespace: the netns is owned by the same
        // user namespace as the relay netns, which is what lets the fabric move
        // the session's veth end into it.
        let mut cmd = Command::new("unshare");
        cmd.arg("-n").arg("--").arg("/bin/sh").arg("-c").arg(&shim).arg("acos-session");
        cmd.args(argv);
        cmd.stdin(Stdio::null()).stdout(stdout_f).stderr(stderr_f);
        if die_with_relay.unwrap_or(true) {
            // The guest dies with the supervisor (`PR_SET_PDEATHSIG` is not
            // available through `unshare`, so the supervisor kills it explicitly
            // in `shutdown`; a lost supervisor is still fail-closed because the
            // session's only route is the relay's tap).
        }
        let child = cmd.spawn().map_err(|e| ProtoError {
            code: ErrorCode::Io,
            message: format!("cannot start the session netns: {e}"),
        })?;
        let pid = i32::try_from(child.id()).unwrap_or(-1);
        let mut session_state = Session {
            net: net.clone(),
            name: session.clone(),
            pid: Some(pid),
            child: Some(child),
            offline: false,
            delay_ms: 0,
            loss_pct: 0,
            host_ports: BTreeSet::new(),
            rules: Vec::new(),
        };
        // The attach itself: create the pair in the relay netns and move the
        // session end into the new netns.
        let result = self.attach_veth(&net, u32::try_from(pid).unwrap_or(0));
        match result {
            Ok(gates) => session_state.rules.extend(gates),
            Err(e) => {
                if let Some(mut c) = session_state.child.take() {
                    let _ = c.kill();
                }
                return Err(e);
            }
        }
        let deadline = Instant::now() + Duration::from_secs(ready_timeout_s.unwrap_or(30));
        while Instant::now() < deadline {
            if ready.exists() {
                let ms = t0.elapsed().as_secs_f64() * 1000.0;
                self.attach_ms_last = Some(ms);
                let info = session_state.info();
                self.log(&format!(
                    "session {session} attached idx={} addr={} pid={pid} attach_ms={ms:.1}",
                    net.idx,
                    net.cidr()
                ));
                self.sessions.insert(session.clone(), session_state);
                return Ok((info, ms));
            }
            if !pid_alive(pid) {
                let log = stderr
                    .as_deref()
                    .map(PathBuf::from)
                    .unwrap_or_else(|| self.state_dir.join("session-err.log"));
                return Err(ProtoError {
                    code: ErrorCode::NetworkUnavailable,
                    message: format!(
                        "session {session} exited before its network was up: {}",
                        read_tail(&log, 400)
                    ),
                });
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        let _ = self.detach(session);
        Err(ProtoError {
            code: ErrorCode::NetworkUnavailable,
            message: format!("session {session} never got a default route"),
        })
    }

    /// Attach an already-running netns (harness/tests). The caller must be in the
    /// supervisor's user namespace for the veth move to be permitted.
    pub fn attach(&mut self, session: &str, netns_pid: u32, index: Option<u32>) -> Result<SessionInfo, ProtoError> {
        self.require_up()?;
        let net = self.alloc(session, index)?;
        if !Path::new(&format!("/proc/{netns_pid}/ns/net")).exists() {
            return Err(ProtoError {
                code: ErrorCode::Invalid,
                message: format!("no netns for pid {netns_pid}"),
            });
        }
        let gates = self.attach_veth(&net, netns_pid)?;
        let s = Session {
            net,
            name: session.to_string(),
            pid: Some(i32::try_from(netns_pid).unwrap_or(-1)),
            child: None,
            offline: false,
            delay_ms: 0,
            loss_pct: 0,
            host_ports: BTreeSet::new(),
            rules: gates,
        };
        let info = s.info();
        self.sessions.insert(session.to_string(), s);
        Ok(info)
    }

    /// Returns the host-port gate rules it installed, so the caller can record
    /// them for teardown.
    fn attach_veth(&mut self, net: &SessionNet, netns_pid: u32) -> Result<Vec<String>, ProtoError> {
        self.fabric_run_all(&plan::veth_attach_rules(net, netns_pid))?;
        // Host ports named before this session existed do not belong to it.
        let gates: Vec<String> = self
            .host_ports
            .iter()
            .map(|p| plan::hostport_gate_rule(net, *p, false))
            .collect();
        if let Err(e) = self.fabric_run_all(&gates) {
            let _ = self.fabric_run_all(&plan::veth_detach_rules(net));
            return Err(e);
        }
        Ok(gates)
    }

    /// Remove the session's veth (which removes its peer in the session netns)
    /// and every rule the relay installed for it.
    pub fn detach(&mut self, session: &str) -> Result<SessionInfo, ProtoError> {
        let Some(mut s) = self.sessions.remove(session) else {
            return Err(ProtoError {
                code: ErrorCode::NoSuchSession,
                message: format!("no session {session}"),
            });
        };
        let mut cmds: Vec<String> = Vec::new();
        if s.offline {
            cmds.push(plan::offline_rule(&s.net, false));
        }
        if s.delay_ms > 0 || s.loss_pct > 0 {
            cmds.push(plan::netem_clear_rule(&s.net));
        }
        for p in &s.host_ports {
            cmds.push(plan::hostport_gate_rule(&s.net, *p, true));
        }
        for r in std::mem::take(&mut s.rules) {
            let inverse = invert_rule(&r);
            cmds.push(inverse);
        }
        cmds.extend(plan::veth_detach_rules(&s.net));
        // Best effort teardown: a relay that is down has no rules to remove, and
        // the veth dies with the relay netns anyway.
        for c in &cmds {
            if let Err(e) = self.fabric_run(c) {
                self.log(&format!("detach {}: {} ({})", s.name, c, e.message));
            }
        }
        if let Some(mut child) = s.child.take() {
            let _ = child.kill();
            let _ = child.wait();
            s.pid = None;
        }
        let info = s.info();
        self.log(&format!("session {} detached", s.name));
        Ok(info)
    }

    fn session_mut(&mut self, name: &str) -> Result<&mut Session, ProtoError> {
        self.sessions.get_mut(name).ok_or_else(|| ProtoError {
            code: ErrorCode::NoSuchSession,
            message: format!("no session {name}"),
        })
    }

    /// Drop or restore a session's traffic. A one-rule change in the relay netns:
    /// nothing in the guest participates, so it also works mid-request.
    pub fn set_offline(&mut self, name: &str, offline: bool) -> Result<SessionInfo, ProtoError> {
        self.require_up()?;
        let net = self.session_mut(name)?.net.clone();
        self.fabric_run(&plan::offline_rule(&net, offline))?;
        let s = self.session_mut(name)?;
        s.offline = offline;
        Ok(s.info())
    }

    /// `netem` on the session's veth (`ms = 0, loss_pct = 0` clears it).
    pub fn set_delay(
        &mut self,
        name: &str,
        ms: u32,
        jitter_ms: u32,
        loss_pct: u32,
    ) -> Result<SessionInfo, ProtoError> {
        self.require_up()?;
        let net = self.session_mut(name)?.net.clone();
        if ms == 0 && loss_pct == 0 {
            self.fabric_run(&plan::netem_clear_rule(&net))?;
        } else {
            if loss_pct > 100 {
                return Err(ProtoError {
                    code: ErrorCode::Invalid,
                    message: "loss_pct must be 0..=100".into(),
                });
            }
            self.fabric_run(&plan::netem_rule(&net, ms, jitter_ms, loss_pct))?;
        }
        let s = self.session_mut(name)?;
        s.delay_ms = ms;
        s.loss_pct = loss_pct;
        Ok(s.info())
    }

    /// Opt a session (or all attached sessions) in to a named host port. Naming a
    /// port restarts `pasta` with that forward, and every session that did not
    /// opt in gets a DROP for it.
    pub fn host_port(
        &mut self,
        port: u16,
        on: bool,
        session: Option<&str>,
    ) -> Result<StatusInfo, ProtoError> {
        self.require_up()?;
        if port == 0 {
            return Err(ProtoError { code: ErrorCode::Invalid, message: "port 0".into() });
        }
        let targets: Vec<String> = match session {
            Some(name) => {
                if !self.sessions.contains_key(name) {
                    return Err(ProtoError {
                        code: ErrorCode::NoSuchSession,
                        message: format!("no session {name}"),
                    });
                }
                vec![name.to_string()]
            }
            None => self.sessions.keys().cloned().collect(),
        };
        for t in &targets {
            let net = self.session_mut(t)?.net.clone();
            self.fabric_run(&plan::hostport_gate_rule(&net, port, !on))?;
            let s = self.session_mut(t)?;
            if on {
                s.host_ports.insert(port);
            } else {
                s.host_ports.remove(&port);
            }
        }
        if on {
            self.host_ports.insert(port);
        } else {
            self.host_ports.remove(&port);
        }
        // pasta's forwards are startup arguments: restart it with the new set.
        self.restart_pasta()?;
        Ok(self.status())
    }

    fn restart_pasta(&mut self) -> Result<(), ProtoError> {
        if let Some(mut child) = self.pasta.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        self.down("pasta restarting".into());
        self.start_pasta()?;
        self.fabric_run_all(&plan::relay_rules())?;
        self.up = true;
        self.down_reason = None;
        Ok(())
    }

    /// Kill `pasta` on purpose (the fail-closed proof). Without `hold` the tick
    /// restarts it; with `hold` it stays down until the next `ensure_up`.
    pub fn kill_pasta(&mut self, hold: bool) -> Result<(), ProtoError> {
        let Some(mut child) = self.pasta.take() else {
            return Err(ProtoError {
                code: ErrorCode::NetworkUnavailable,
                message: "no pasta running".into(),
            });
        };
        let _ = child.kill();
        let _ = child.wait();
        self.hold_down = hold;
        self.down(if hold { "pasta killed (held down)".into() } else { "pasta killed".into() });
        Ok(())
    }

    /// The relay netns' own view of what is attached (the leak check).
    fn relay_footprint(&self) -> (Vec<String>, usize) {
        if self.fabric_pid.is_none() || !self.fabric_run("true").is_ok() {
            return (Vec::new(), 0);
        }
        let veths = self
            .fabric_run("ip -o link show")
            .map(|out| {
                out.lines()
                    .filter_map(|l| {
                        let name = l.split(':').nth(1)?.trim().split('@').next()?.trim().to_string();
                        name.starts_with(plan::VETH_PREFIX).then_some(name)
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let rules = self
            .fabric_run(&format!("iptables -w -S {}", plan::SESSION_CHAIN))
            .map(|out| out.lines().filter(|l| !l.starts_with("-N ")).count())
            .unwrap_or(0);
        (veths, rules)
    }

    #[must_use]
    pub fn status(&mut self) -> StatusInfo {
        let alive = self.pasta_alive();
        let (relay_veths, session_rules) = self.relay_footprint();
        StatusInfo {
            up: self.up && alive,
            down_reason: if self.up && alive { None } else { self.down_reason.clone() },
            fabric_pid: self.fabric_pid,
            pasta_pid: self.pasta.as_ref().map(|c| i32::try_from(c.id()).unwrap_or(-1)),
            pasta_version: self.pasta_version.clone(),
            pasta_tag_expected: plan::PASTA_TAG.to_string(),
            host_ports: self.host_ports.iter().copied().collect(),
            sessions: self.sessions.values().map(Session::info).collect(),
            relay_veths,
            session_rules,
            attach_ms_last: self.attach_ms_last,
        }
    }

    /// Stop everything: sessions, pasta, the relay netns.
    pub fn shutdown(&mut self) {
        let names: Vec<String> = self.sessions.keys().cloned().collect();
        for n in names {
            let _ = self.detach(&n);
        }
        if let Some(mut child) = self.pasta.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        if let Some(mut child) = self.fabric.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        self.fabric_pid = None;
        self.up = false;
        self.log("supervisor shutdown");
    }
}

fn self_log(dir: &Path, msg: &str) {
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(dir.join("netd.log"))
    {
        let _ = writeln!(f, "{msg}");
    }
}

fn io_err(e: std::io::Error) -> ProtoError {
    ProtoError { code: ErrorCode::Io, message: e.to_string() }
}

fn tail(s: &str) -> String {
    let s = s.trim();
    if s.chars().count() <= 400 {
        return s.to_string();
    }
    s.chars().skip(s.chars().count() - 400).collect()
}

fn read_tail(path: &Path, bytes: usize) -> String {
    let Ok(mut f) = std::fs::File::open(path) else {
        return String::new();
    };
    let mut s = String::new();
    let _ = f.read_to_string(&mut s);
    tail(&s[s.len().saturating_sub(bytes)..])
}

fn open_stdio(path: Option<&str>, dir: &Path, fallback: &str) -> Result<Stdio, ProtoError> {
    let p = match path {
        Some(p) => PathBuf::from(p),
        None => dir.join(fallback),
    };
    let f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&p)
        .map_err(io_err)?;
    Ok(Stdio::from(f))
}

/// Turn a rule into the command that removes it (the inverse used by detach).
fn invert_rule(rule: &str) -> String {
    // Only the `-I`/`-A` insert forms are tracked; their inverse is `-D`.
    if let Some(rest) = rule.strip_prefix("iptables -w -I ") {
        return format!("iptables -w -D {rest}");
    }
    if let Some(rest) = rule.strip_prefix("iptables -w -A ") {
        return format!("iptables -w -D {rest}");
    }
    rule.to_string()
}

// ---------------------------------------------------------------------------
// The fabric: the relay netns' executor
// ---------------------------------------------------------------------------

/// Serve the relay netns until killed: `ip`, `iptables` and `tc` commands from
/// the supervisor, one per connection.
///
/// This process is started by the supervisor as `unshare -n -- acos-netd fabric`,
/// so it is the only code that ever runs inside the relay netns. It executes a
/// shell command per line and answers with `{"rc":n,"out":"…"}`.
pub fn run_fabric(socket: &Path) -> std::io::Result<()> {
    let _ = std::fs::remove_file(socket);
    let listener = UnixListener::bind(socket)?;
    // Same-uid only: the socket sits in a 0700 directory, and the peer check is
    // the second line of defence (the guest never sees this filesystem).
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(socket, std::fs::Permissions::from_mode(0o600))?;
    let _ = Command::new("ip").args(["link", "set", "lo", "up"]).status();
    for conn in listener.incoming() {
        let Ok(mut c) = conn else { continue };
        if !same_uid(&c) {
            let _ = c.write_all(b"{\"rc\":1,\"out\":\"peer uid\"}\n");
            continue;
        }
        let mut line = String::new();
        if BufReader::new(&c).read_line(&mut line).is_err() {
            continue;
        }
        let cmd = line.trim_end().to_string();
        let out = Command::new("/bin/sh").arg("-c").arg(&cmd).output();
        let reply = match out {
            Ok(o) => {
                let mut s = String::from_utf8_lossy(&o.stdout).to_string();
                s.push_str(&String::from_utf8_lossy(&o.stderr));
                serde_json::json!({"rc": o.status.code().unwrap_or(-1), "out": s}).to_string()
            }
            Err(e) => serde_json::json!({"rc": -1, "out": e.to_string()}).to_string(),
        };
        let _ = c.write_all(reply.as_bytes());
        let _ = c.write_all(b"\n");
    }
    Ok(())
}

fn same_uid(s: &UnixStream) -> bool {
    use std::os::unix::io::AsRawFd;
    let mut cred: libc::ucred = unsafe { std::mem::zeroed() };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    let rc = unsafe {
        libc::getsockopt(
            s.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&mut cred) as *mut libc::ucred as *mut libc::c_void,
            &mut len,
        )
    };
    rc == 0 && (cred.uid == unsafe { libc::geteuid() } || unsafe { libc::geteuid() } == 0)
}
