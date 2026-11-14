//! # The production zygote (`ACOS_ART_ZYGOTE=on`, ZPROD P2)
//!
//! Owner: `docs/decisions/2026-11-14-zygote-prod.md` §2–§4, `DECISIONS.md`
//! ADR-0026 decision 1. This module is the **P1 port**: the keeper launcher, the
//! plan directory, the command FIFO and the four verbs
//! (`spawn`/`kill`/`boot_epoch`/`reset`) that `research/zygote-proto/z4/reset.py`
//! drove from Python.
//!
//! ## What a keeper is
//!
//! ```text
//! PersistentZygote (this type, owned by `acos serve`)
//! └── keeper launcher  `acos-art launch`  (ACOS_ZYGOTE_APP + ACOS_ZYGOTE_PLAN_DIR
//!     │                                    + ACOS_ZYGOTE_N=0 + ACOS_ZYGOTE_PERSIST=1)
//!     └── bwrap → ZygoteApp  (PID 2 of its sandbox; the pre-fork core, built once)
//!         ├── child tag 0 : own mount ns + own pidns + own run root
//!         └── child tag k
//! ```
//!
//! The keeper is **not** a session: it has no peer, no `ServiceHost` and no
//! contract of its own. It only forks and reaps children on request.
//!
//! ## The contract (files, exactly as Z4 wrote them)
//!
//! | file | writer | meaning |
//! |---|---|---|
//! | `<plan>/ready` | zygote | `epoch=<n> boot_ms=<ms>` — the zygote is up |
//! | `<plan>/zygote.pid` | zygote | the zygote's pid inside its sandbox |
//! | `<plan>/cmd` | host | a **FIFO**: `spawn <tag>` / `kill <tag>` / `exit` |
//! | `<plan>/<tag>/argv` | session launcher | the session published its contract |
//! | `<plan>/<tag>/binds` | **host (this module)** | the run root the child binds |
//! | `<plan>/<tag>/pid` | zygote | the child's pid |
//! | `<plan>/<tag>/spawned` | zygote | `pid=<n>` — the spawn was acked |
//! | `<plan>/<tag>/reaped` | zygote | `status=0x9 pid=<n>` — the kill was acked |
//!
//! Every ack is a **file the zygote wrote**, never a host-side guess: a missing
//! or timed-out ack is a structured error (`ZygoteError::AckTimeout`), never a
//! silent fallback to the exec path (§6 "reset is never best effort").
//!
//! ## What this increment does NOT do (orchestrator decision, 2026-11-14)
//!
//! * **No per-child cgroup (F3)** — deferred to P3. Children share the keeper's
//!   scope, so a child is killed **by process group**, never by `cgroup.kill`
//!   (that is what Z7 §6 measured taking the zygote down). Consequence, stated
//!   in every reply that carries memory: session memory is *not* per-child
//!   under the zygote.
//! * No fd allowlist (F4), no pre-fork invariant assert (F5), no `tgkill`
//!   filter (F2 defence-in-depth): P3/P4.

use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use crate::Error;

/// Default idle exit: the keeper ends itself after this long with no live child.
/// Orchestrator decision (2026-11-14): the **default is "live until `acos serve`
/// ends"**, an idle exit is opt-in through this knob.
pub const DEFAULT_IDLE_EXIT_S: u64 = 0;
/// How long a `spawn`/`kill` ack may take before the keeper is declared dead.
pub const DEFAULT_ACK_TIMEOUT_MS: u64 = 30_000;
/// How long the zygote may take to build its pre-fork core (ZPROD1 measured
/// 13 ms; a cold boot image or a debug build is far slower).
pub const DEFAULT_READY_TIMEOUT_MS: u64 = 180_000;

/// What a keeper is valid for. A zygote's pre-fork heap only makes sense for one
/// such tuple (§3), so one keeper per key.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ZygoteKey {
    pub apk: String,
    pub boot_image: String,
    pub width: u32,
    pub height: u32,
    pub density: u32,
    pub insets: String,
    pub animations: String,
}

impl ZygoteKey {
    /// A filesystem-safe directory name for this key (stable, sorted, short).
    pub fn dir_name(&self) -> String {
        let mut hash: u64 = 0xcbf29ce484222325;
        for part in [
            self.apk.as_str(),
            self.boot_image.as_str(),
            self.insets.as_str(),
            self.animations.as_str(),
        ] {
            for byte in part.as_bytes() {
                hash ^= u64::from(*byte);
                hash = hash.wrapping_mul(0x100000001b3);
            }
            hash ^= 0xff;
            hash = hash.wrapping_mul(0x100000001b3);
        }
        format!(
            "{}-{}x{}-{}-{}-{:016x}",
            short_hash(&self.apk, 8),
            self.width,
            self.height,
            self.density,
            self.insets,
            hash
        )
    }

    /// The key as it is reported in replies (never a path).
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "apk": self.apk,
            "boot_image": self.boot_image,
            "width": self.width,
            "height": self.height,
            "density": self.density,
            "insets": self.insets,
            "animations": self.animations,
        })
    }
}

fn short_hash(text: &str, hex: usize) -> String {
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in text.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")[..hex].to_string()
}

/// Why a keeper could not do what it was asked. Every variant is fatal for the
/// *session*, never for the process: the engine turns it into `ZYGOTE_DEAD`
/// plus a new boot epoch and a refork (§6).
#[derive(Debug)]
pub enum ZygoteError {
    /// The keeper process is gone (or its `ready` file vanished).
    Dead { status: Option<String>, tail: String },
    /// The keeper is alive but never acked a verb.
    AckTimeout { verb: String, tag: u32, waited_ms: u64, tail: String },
    /// The keeper never came up.
    NotReady { waited_ms: u64, tail: String },
    /// A contract file was absent or unparsable.
    Contract(String),
    Io(String),
}

impl std::fmt::Display for ZygoteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ZygoteError::Dead { status, tail } => write!(
                f,
                "ZYGOTE_DEAD: the zygote keeper is gone (exit {}) last log: {}",
                status.as_deref().unwrap_or("unknown"),
                tail
            ),
            ZygoteError::AckTimeout { verb, tag, waited_ms, tail } => write!(
                f,
                "ZYGOTE_DEAD: `{verb} {tag}` was not acked within {waited_ms} ms (keeper not responding) last log: {tail}"
            ),
            ZygoteError::NotReady { waited_ms, tail } => write!(
                f,
                "ZYGOTE_DEAD: the zygote never wrote <plan>/ready within {waited_ms} ms: {tail}"
            ),
            ZygoteError::Contract(detail) => write!(f, "ZYGOTE_DEAD: {detail}"),
            ZygoteError::Io(detail) => write!(f, "ZYGOTE_DEAD: io: {detail}"),
        }
    }
}

impl From<io::Error> for ZygoteError {
    fn from(error: io::Error) -> Self {
        ZygoteError::Io(error.to_string())
    }
}

impl From<ZygoteError> for Error {
    fn from(error: ZygoteError) -> Self {
        // The message starts with `ZYGOTE_DEAD` so a caller can classify it
        // without parsing prose, exactly like the other structured seams.
        Error::Zygote(error.to_string())
    }
}

pub type ZygoteResult<T> = std::result::Result<T, ZygoteError>;

/// One child of the keeper, as the host observed it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ZygoteChild {
    pub tag: u32,
    pub child_pid: u32,
    pub run_root: PathBuf,
}

impl ZygoteChild {
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "tag": self.tag,
            "child_pid": self.child_pid,
            "run_root": self.run_root.display().to_string(),
        })
    }
}

/// The keepers one `acos serve` owns (§3: at most one per [`ZygoteKey`]).
#[derive(Default)]
pub struct ZygoteRegistry {
    keepers: HashMap<ZygoteKey, PersistentZygote>,
    next_tag: u32,
}

impl ZygoteRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.keepers.len()
    }

    pub fn is_empty(&self) -> bool {
        self.keepers.is_empty()
    }

    /// The keeper for `key`, booting one if this is the first request (§3).
    pub fn keeper_mut(&mut self, key: &ZygoteKey) -> Option<&mut PersistentZygote> {
        self.keepers.get_mut(key)
    }

    pub fn insert(&mut self, key: ZygoteKey, keeper: PersistentZygote) {
        self.keepers.insert(key, keeper);
    }

    /// Drop a keeper from the registry (the dead-keeper restart path, ZPROD4).
    /// The caller has already shut it down; a removed keeper can never answer.
    pub fn remove(&mut self, key: &ZygoteKey) -> Option<PersistentZygote> {
        self.keepers.remove(key)
    }

    /// A tag that no live session of this serve holds. Tags are never reused, so
    /// a stale contract in `<plan>/<tag>` can never be mistaken for a new child.
    pub fn allocate_tag(&mut self) -> u32 {
        let tag = self.next_tag;
        self.next_tag = self.next_tag.wrapping_add(1);
        tag
    }

    /// Shut every keeper down (serve exit — §3 "the keeper outlives sessions,
    /// never a serve").
    pub fn shutdown_all(&mut self) {
        for (_, mut keeper) in self.keepers.drain() {
            keeper.shutdown();
        }
    }

    /// The keeper launcher pids this registry owns (a leak check reads these:
    /// after `shutdown_all` none of them may be alive).
    pub fn keeper_pids(&self) -> Vec<u32> {
        self.keepers
            .values()
            .filter_map(PersistentZygote::keeper_pid)
            .collect()
    }

    /// The last child pid this registry handed out for `tag` of `key`.
    pub fn child_pid_of(&self, key: &ZygoteKey, tag: u32) -> Option<u32> {
        self.keepers.get(key).and_then(|k| k.child_pid_of(tag))
    }

    pub fn epochs(&self) -> HashMap<String, String> {
        self.keepers
            .iter()
            .map(|(key, keeper)| (key.dir_name(), keeper.boot_epoch().to_string()))
            .collect()
    }
}

impl Drop for ZygoteRegistry {
    fn drop(&mut self) {
        self.shutdown_all();
    }
}

/// The persistent zygote keeper (§2/§4). One per (APK, boot image, geometry,
/// insets, animations); owned by the engine, never by a session.
pub struct PersistentZygote {
    root: PathBuf,
    plan: PathBuf,
    key: ZygoteKey,
    keeper: Option<Child>,
    cmd: Option<File>,
    epoch: String,
    zygote_pid: u32,
    boot_ms: Option<u64>,
    idle_exit_s: u64,
    ack_timeout: Duration,
    log_path: PathBuf,
    /// The last child this keeper handed out per tag (so `previous_child_pid`
    /// survives a kill even after `<tag>/pid` was removed).
    last_child: HashMap<u32, u32>,
}

impl PersistentZygote {
    /// Boot a keeper for `key`.
    ///
    /// `binary` is the `acos-art` executable the *session* launchers also use, so
    /// the keeper and its children are the same build by construction. `env`
    /// carries the launch-time sandbox knobs the engine already decided
    /// (geometry, fonts, sysroot); the zygote-specific variables are set here and
    /// never inherited from the caller (§7: the product knob is the single
    /// `ACOS_ART_ZYGOTE`).
    pub fn start(
        root: &Path,
        key: ZygoteKey,
        binary: &Path,
        apk: &Path,
        env: &[(String, String)],
        zygote_build: &Path,
    ) -> ZygoteResult<Self> {
        let root = root.join(key.dir_name());
        fs::create_dir_all(&root)?;
        let plan = root.join("plan");
        fs::create_dir_all(&plan)?;
        let log_path = root.join("keeper.log");

        let cmd_path = plan.join("cmd");
        // A stale FIFO from a dead serve would be opened happily and never read.
        let _ = fs::remove_file(&cmd_path);
        make_fifo(&cmd_path)?;
        // ZPROD4: a restarted keeper must not inherit the dead keeper's contract.
        // `ready` carries the boot epoch, so a stale file would make a brand-new
        // zygote answer with the dead epoch (and the restart would look like a
        // reset on a live keeper). §4.2: nothing the old process wrote survives.
        let _ = fs::remove_file(plan.join("ready"));
        let _ = fs::remove_file(plan.join("zygote.pid"));

        let timeout = Duration::from_millis(DEFAULT_READY_TIMEOUT_MS);
        let started = Instant::now();
        let log = OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(&log_path)?;
        let log_err = log.try_clone()?;
        let out = root.join("out");
        fs::create_dir_all(&out)?;
        let mut command = Command::new(binary);
        command
            .arg("launch")
            .arg("--apk")
            .arg(apk)
            .arg("--until")
            .arg("activity-resumed")
            .arg("--out")
            .arg(&out)
            .arg("--timeout")
            .arg("300")
            // The keeper is a zygote host, not a session: no interactive peer,
            // no live probe. It only serves the command FIFO.
            .env("ACOS_ZYGOTE_APP", zygote_build)
            .env("ACOS_ZYGOTE_PLAN_DIR", &plan)
            // N=0: the cohort is forked on request, not once at boot.
            .env("ACOS_ZYGOTE_N", "0")
            .env("ACOS_ZYGOTE_PERSIST", "1")
            .env_remove("ACOS_ZYGOTE_SESSION")
            .stdin(Stdio::null())
            .stdout(Stdio::from(log))
            .stderr(Stdio::from(log_err));
        for (name, value) in env {
            command.env(name, value);
        }
        let keeper = command.spawn().map_err(|error| {
            ZygoteError::Io(format!("cannot start the keeper: {error}"))
        })?;
        let ready_path = plan.join("ready");
        let mut zygote = PersistentZygote {
            root,
            plan,
            key,
            keeper: Some(keeper),
            cmd: None,
            epoch: String::new(),
            zygote_pid: 0,
            boot_ms: None,
            idle_exit_s: idle_exit_from_env(),
            ack_timeout: ack_timeout_from_env(),
            log_path,
            last_child: HashMap::new(),
        };
        // O_RDWR: neither side blocks on open, and the FIFO never signals EOF
        // when one writer restarts (the keeper never sees a spurious `cmd eof`).
        let fd = OpenOptions::new().read(true).write(true).open(&cmd_path)?;
        zygote.cmd = Some(fd);
        if !zygote.wait_path(&ready_path, timeout, started) {
            let tail = zygote.log_tail(20);
            zygote.shutdown();
            return Err(ZygoteError::NotReady {
                waited_ms: started.elapsed().as_millis() as u64,
                tail,
            });
        }
        let ready = read_kv(&ready_path)
            .ok_or_else(|| ZygoteError::Contract("<plan>/ready is unreadable".into()))?;
        zygote.epoch = ready.get("epoch").cloned().unwrap_or_default();
        zygote.boot_ms = ready.get("boot_ms").and_then(|v| v.parse().ok());
        zygote.zygote_pid = fs::read_to_string(zygote.plan.join("zygote.pid"))
            .ok()
            .and_then(|text| text.trim().parse().ok())
            .ok_or_else(|| ZygoteError::Contract("<plan>/zygote.pid is unreadable".into()))?;
        Ok(zygote)
    }

    pub fn key(&self) -> &ZygoteKey {
        &self.key
    }

    /// The keeper's own directory (`<art-context>/zygote/<key>/`).
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// `<plan>/<tag>` — the directory the session launcher and the child share.
    pub fn plan(&self) -> &Path {
        &self.plan
    }

    pub fn session_dir(&self, tag: u32) -> PathBuf {
        self.plan.join(tag.to_string())
    }

    /// `epoch=<n>` from `<plan>/ready`: **unchanged** across a reset, and a new
    /// value whenever the keeper restarted (§4.5/§6).
    pub fn boot_epoch(&self) -> &str {
        &self.epoch
    }

    /// The zygote's pid inside its sandbox.
    pub fn zygote_pid(&self) -> u32 {
        self.zygote_pid
    }

    pub fn boot_ms(&self) -> Option<u64> {
        self.boot_ms
    }

    pub fn idle_exit_s(&self) -> u64 {
        self.idle_exit_s
    }

    /// The keeper launcher's pid (the process this struct reaps on shutdown).
    pub fn keeper_pid(&self) -> Option<u32> {
        self.keeper.as_ref().map(Child::id)
    }

    pub fn child_pid_of(&self, tag: u32) -> Option<u32> {
        read_pid(&self.session_dir(tag).join("pid")).or_else(|| self.last_child.get(&tag).copied())
    }

    pub fn run_root_of(&self, tag: u32) -> PathBuf {
        self.session_dir(tag).join("run")
    }

    /// Is the keeper process (and its zygote) still there? A dead keeper is a
    /// **reported** condition, never a silent fallback (§6).
    pub fn alive(&mut self) -> bool {
        let Some(child) = self.keeper.as_mut() else { return false };
        match child.try_wait() {
            Ok(None) => {}
            _ => return false,
        }
        self.plan.join("ready").is_file() && self.plan.join("zygote.pid").is_file()
    }

    /// Why the keeper is dead, as the reply carries it: exit status + log tail.
    pub fn dead_reason(&mut self) -> Option<ZygoteError> {
        if self.alive() {
            return None;
        }
        let status = self
            .keeper
            .as_mut()
            .and_then(|child| child.try_wait().ok().flatten())
            .map(describe_status);
        Some(ZygoteError::Dead { status, tail: self.log_tail(20) })
    }

    /// Prepare `<plan>/<tag>` for a **fresh** child: empty `run` and `sock`, and
    /// no contract or ack from the previous child (§4.2: "nothing the old child
    /// wrote survives").
    pub fn prepare_session(&mut self, tag: u32) -> ZygoteResult<PathBuf> {
        let dir = self.session_dir(tag);
        fs::create_dir_all(&dir)?;
        for sub in ["run", "sock"] {
            let path = dir.join(sub);
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path)?;
        }
        for name in ["argv", "binds", "session.json", "spawned", "reaped", "pid"] {
            let _ = fs::remove_file(dir.join(name));
        }
        // The child binds its own run root; the directories it mounts must exist
        // before `spawn` so the bind cannot race the fork.
        for sub in [
            "run/dalvik-cache",
            "run/misc",
            "run/tmp",
            "run/out",
            "run/task",
            "run/app-data",
            "run/app-lib",
        ] {
            fs::create_dir_all(dir.join(sub))?;
        }
        Ok(dir)
    }

    /// Publish `<plan>/<tag>/binds`: the run root and sockets the child is told to
    /// bind (the host half of the Z4 contract, `drive.write_binds`).
    pub fn publish_binds(&self, tag: u32) -> ZygoteResult<()> {
        let dir = self.session_dir(tag);
        let rows = [
            (format!("/sessions/{tag}/run/dalvik-cache"), "/data/dalvik-cache"),
            (format!("/sessions/{tag}/run/misc"), "/data/misc"),
            (format!("/sessions/{tag}/run/tmp"), "/data/tmp"),
            (format!("/sessions/{tag}/run/out"), "/data/out"),
            (format!("/sessions/{tag}/run/task"), "/data/task"),
            (format!("/sessions/{tag}/run/app-data"), "/data/app-data"),
            (format!("/sessions/{tag}/run/app-lib"), "/data/app-lib"),
            (format!("/sessions/{tag}/sock"), "/dev/socket"),
            (format!("/sessions/{tag}/sock/peer.sock"), "/data/acos-launch.sock"),
        ];
        let mut text = String::new();
        for (a, b) in rows {
            text.push_str(&a);
            text.push('\t');
            text.push_str(b);
            text.push('\n');
        }
        write_atomic(&dir.join("binds"), text.as_bytes())?;
        Ok(())
    }

    /// Wait until the session launcher published `<plan>/<tag>/argv`, then write
    /// `binds`. Returns the launcher-observed contract path.
    pub fn wait_contract(&mut self, tag: u32, timeout: Duration) -> ZygoteResult<()> {
        self.wait_contract_with(tag, timeout, || None)
    }

    /// [`Self::wait_contract`] that also watches the **peer** that has to produce
    /// the contract (the session launcher process, owned by the caller).
    ///
    /// ZPROD5: the contract timeout alone is not a diagnosis. A launcher that
    /// exits before it writes `<plan>/<tag>/argv` (measured: `acos serve`'s bench
    /// cohort, where the session dir pushed `sock/peer.sock` past `sun_path` and
    /// the launcher died with `io: path must be shorter than SUN_LEN`) left the
    /// engine waiting the full 180 s with nothing but the *keeper's* log. Here the
    /// caller passes `peer_gone`, which returns `Some(reason)` as soon as the
    /// launcher can no longer answer; the reason becomes the `ZYGOTE_DEAD` detail
    /// (the launcher's exit status and its stderr tail, not a timeout).
    pub fn wait_contract_with<F>(
        &mut self,
        tag: u32,
        timeout: Duration,
        mut peer_gone: F,
    ) -> ZygoteResult<()>
    where
        F: FnMut() -> Option<String>,
    {
        let argv = self.session_dir(tag).join("argv");
        let started = Instant::now();
        while started.elapsed() < timeout {
            if argv.is_file() {
                self.publish_binds(tag)?;
                return Ok(());
            }
            if !self.alive() {
                return Err(self
                    .dead_reason()
                    .unwrap_or(ZygoteError::Contract("the keeper died before <tag>/argv".into())));
            }
            if let Some(reason) = peer_gone() {
                return Err(ZygoteError::Contract(format!(
                    "the session launcher cannot publish `<plan>/{tag}/argv` and never will \
                     (waited {} ms): {reason}",
                    started.elapsed().as_millis()
                )));
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        Err(ZygoteError::AckTimeout {
            verb: "argv".into(),
            tag,
            waited_ms: started.elapsed().as_millis() as u64,
            tail: self.log_tail(20),
        })
    }

    /// `spawn <tag>`: the zygote forks one fresh child for an already-published
    /// contract and acks it in `<plan>/<tag>/spawned` (§4.4).
    pub fn spawn(&mut self, tag: u32) -> ZygoteResult<ZygoteChild> {
        let dir = self.session_dir(tag);
        let _ = fs::remove_file(dir.join("spawned"));
        let _ = fs::remove_file(dir.join("pid"));
        self.send(&format!("spawn {tag}"))?;
        let ack = self.wait_ack(&dir.join("spawned"), "spawn", tag)?;
        let child_pid = ack
            .get("pid")
            .and_then(|v| v.parse().ok())
            .or_else(|| read_pid(&dir.join("pid")))
            .ok_or_else(|| {
                ZygoteError::Contract(format!("spawn ack for tag {tag} named no pid"))
            })?;
        self.last_child.insert(tag, child_pid);
        Ok(ZygoteChild { tag, child_pid, run_root: dir.join("run") })
    }

    /// `kill <tag>`: the zygote SIGKILLs **that child's process group** and reaps
    /// it; siblings and the zygote are untouched (§4.1, §6: by pid/group, never
    /// by `cgroup.kill`).
    pub fn kill(&mut self, tag: u32) -> ZygoteResult<Option<u32>> {
        let dir = self.session_dir(tag);
        let previous = self.child_pid_of(tag);
        let _ = fs::remove_file(dir.join("reaped"));
        let _ = fs::remove_file(dir.join("spawned"));
        self.send(&format!("kill {tag}"))?;
        let ack = self.wait_ack(&dir.join("reaped"), "kill", tag)?;
        let _ = fs::remove_file(dir.join("pid"));
        Ok(ack
            .get("pid")
            .and_then(|v| v.parse().ok())
            .or(previous))
    }

    /// §4: the whole reset — kill one child, wipe its run root, fork one fresh
    /// child from the **same** zygote (same pid, same boot epoch).
    ///
    /// The session launcher is the caller's business (it is the session's peer and
    /// its `ServiceHost`): the engine stops the old launcher, calls this, and
    /// starts a new launcher for the returned tag. Returns `(previous, fresh)`.
    pub fn reset(&mut self, tag: u32) -> ZygoteResult<(Option<u32>, ZygoteChild)> {
        let previous = self.kill(tag)?;
        self.prepare_session(tag)?;
        let fresh = self.spawn(tag)?;
        Ok((previous, fresh))
    }

    /// Send one verb line on `<plan>/cmd`. The FIFO is held open O_RDWR by this
    /// process for its whole life, so a write never blocks on a reader.
    pub fn send(&mut self, line: &str) -> ZygoteResult<()> {
        if !self.alive() {
            return Err(self
                .dead_reason()
                .unwrap_or(ZygoteError::Dead { status: None, tail: self.log_tail(20) }));
        }
        let Some(cmd) = self.cmd.as_mut() else {
            return Err(ZygoteError::Dead { status: None, tail: self.log_tail(20) });
        };
        cmd.write_all(line.as_bytes())?;
        cmd.write_all(b"\n")?;
        cmd.flush()?;
        Ok(())
    }

    /// Shut the keeper down and reap it. Idempotent; also the `Drop` path, so a
    /// serve that panics cannot leak a zygote (§3, and the acceptance "keeper not
    /// leaked after serve exit").
    pub fn shutdown(&mut self) {
        if let Some(mut cmd) = self.cmd.take() {
            let _ = cmd.write_all(b"exit\n");
            let _ = cmd.flush();
        }
        if let Some(mut keeper) = self.keeper.take() {
            // The keeper launcher owns bwrap --die-with-parent, so ending the
            // launcher ends the zygote; the grace period lets it exit cleanly.
            let deadline = Instant::now() + Duration::from_secs(3);
            while Instant::now() < deadline {
                if let Ok(Some(_)) = keeper.try_wait() {
                    return;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            let _ = keeper.kill();
            let _ = keeper.wait();
        }
    }

    fn wait_ack(&mut self, path: &Path, verb: &str, tag: u32) -> ZygoteResult<HashMap<String, String>> {
        let started = Instant::now();
        while started.elapsed() < self.ack_timeout {
            if let Some(kv) = read_kv(path) {
                if !kv.is_empty() {
                    return Ok(kv);
                }
            }
            if !self.alive() {
                return Err(self.dead_reason().unwrap_or(ZygoteError::Dead {
                    status: None,
                    tail: self.log_tail(20),
                }));
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        Err(ZygoteError::AckTimeout {
            verb: verb.into(),
            tag,
            waited_ms: started.elapsed().as_millis() as u64,
            tail: self.log_tail(20),
        })
    }

    fn wait_path(&mut self, path: &Path, timeout: Duration, started: Instant) -> bool {
        while started.elapsed() < timeout {
            if path.is_file() {
                return true;
            }
            if let Some(child) = self.keeper.as_mut() {
                if let Ok(Some(_)) = child.try_wait() {
                    return false;
                }
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        false
    }

    /// The last `lines` lines of the keeper log: evidence, not prose. A dead
    /// keeper's reply carries them (§6).
    pub fn log_tail(&self, lines: usize) -> String {
        let Ok(text) = fs::read_to_string(&self.log_path) else {
            return String::from("<no keeper log>");
        };
        let all: Vec<&str> = text.lines().collect();
        let start = all.len().saturating_sub(lines);
        all[start..].join(" | ")
    }
}

impl Drop for PersistentZygote {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// `mkfifo(3)` for the command channel. Mode 0600: the FIFO is the keeper's only
/// control surface and must not be writable by another user.
pub fn make_fifo(path: &Path) -> ZygoteResult<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let c = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| ZygoteError::Io("FIFO path contains a NUL".into()))?;
    // SAFETY: `c` is a valid NUL-terminated path; mkfifo only writes the file.
    let rc = unsafe { libc::mkfifo(c.as_ptr(), 0o600) };
    if rc != 0 {
        return Err(ZygoteError::Io(format!(
            "mkfifo {}: {}",
            path.display(),
            io::Error::last_os_error()
        )));
    }
    Ok(())
}

/// `key=value` tokens from a contract file, whitespace separated (Z4's format).
pub fn read_kv(path: &Path) -> Option<HashMap<String, String>> {
    let text = fs::read_to_string(path).ok()?;
    let mut out = HashMap::new();
    for token in text.split_whitespace() {
        if let Some((key, value)) = token.split_once('=') {
            out.insert(key.to_string(), value.to_string());
        }
    }
    Some(out)
}

/// `<tag>/pid` holds a bare integer (Z4's contract), not `key=value`.
pub fn read_pid(path: &Path) -> Option<u32> {
    fs::read_to_string(path).ok()?.trim().parse().ok()
}

fn write_atomic(path: &Path, bytes: &[u8]) -> ZygoteResult<()> {
    let tmp = path.with_extension("tmp");
    {
        let mut file = File::create(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
    }
    fs::rename(&tmp, path)?;
    Ok(())
}

fn describe_status(status: std::process::ExitStatus) -> String {
    use std::os::unix::process::ExitStatusExt;
    match (status.code(), status.signal()) {
        (Some(code), _) => format!("code {code}"),
        (None, Some(signal)) => format!("signal {signal}"),
        _ => "unknown".to_string(),
    }
}

fn idle_exit_from_env() -> u64 {
    std::env::var("ACOS_ART_ZYGOTE_IDLE_S")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(DEFAULT_IDLE_EXIT_S)
}

fn ack_timeout_from_env() -> Duration {
    Duration::from_millis(
        std::env::var("ACOS_ART_ZYGOTE_ACK_MS")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(DEFAULT_ACK_TIMEOUT_MS),
    )
}

/// Read the terminal ack file's raw bytes (used by tests and diagnostics).
pub fn read_ack_text(path: &Path) -> Option<String> {
    let mut text = String::new();
    File::open(path).ok()?.read_to_string(&mut text).ok()?;
    Some(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key() -> ZygoteKey {
        ZygoteKey {
            apk: "counter".into(),
            boot_image: "stock".into(),
            width: 1080,
            height: 2400,
            density: 400,
            insets: "carve".into(),
            animations: "on".into(),
        }
    }

    #[test]
    fn key_dir_names_are_stable_distinct_and_path_safe() {
        let a = key().dir_name();
        let b = key().dir_name();
        assert_eq!(a, b, "the same key must name the same directory");
        let mut other = key();
        other.insets = "report".into();
        assert_ne!(a, other.dir_name(), "insets is part of the key");
        let mut third = key();
        third.width = 720;
        assert_ne!(a, third.dir_name(), "geometry is part of the key");
        assert!(a
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'));
        assert!(!a.contains('/'));
    }

    #[test]
    fn kv_and_pid_contracts_parse_z4_shapes() {
        let dir = std::env::temp_dir().join(format!("zprod2-kv-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let ready = dir.join("ready");
        fs::write(&ready, "epoch=26971873464 boot_ms=13\n").unwrap();
        let kv = read_kv(&ready).unwrap();
        assert_eq!(kv.get("epoch").map(String::as_str), Some("26971873464"));
        assert_eq!(kv.get("boot_ms").map(String::as_str), Some("13"));
        let pid = dir.join("pid");
        fs::write(&pid, "62\n").unwrap();
        assert_eq!(read_pid(&pid), Some(62));
        // A missing file is `None`, never a fake zero pid.
        assert_eq!(read_pid(&dir.join("absent")), None);
        assert!(read_kv(&dir.join("absent")).is_none());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn prepare_session_wipes_the_previous_run_root_and_contract() {
        let dir = std::env::temp_dir().join(format!("zprod2-prep-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let mut keeper = PersistentZygote {
            root: dir.clone(),
            plan: dir.clone(),
            key: key(),
            keeper: None,
            cmd: None,
            epoch: "1".into(),
            zygote_pid: 2,
            boot_ms: Some(13),
            idle_exit_s: 0,
            ack_timeout: Duration::from_millis(100),
            log_path: dir.join("keeper.log"),
            last_child: HashMap::new(),
        };
        let tag_dir = dir.join("7");
        fs::create_dir_all(tag_dir.join("run/app-data")).unwrap();
        fs::write(tag_dir.join("run/app-data/state.txt"), b"leaked").unwrap();
        fs::write(tag_dir.join("spawned"), b"pid=41\n").unwrap();
        fs::write(tag_dir.join("pid"), b"41\n").unwrap();
        fs::write(tag_dir.join("argv"), b"stale").unwrap();
        keeper.prepare_session(7).unwrap();
        assert!(!tag_dir.join("run/app-data/state.txt").exists(), "run root must be empty");
        assert!(tag_dir.join("run/app-data").is_dir(), "the mount points must exist");
        assert!(!tag_dir.join("spawned").exists(), "a stale ack must never answer a new spawn");
        assert!(!tag_dir.join("pid").exists());
        assert!(!tag_dir.join("argv").exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn publish_binds_writes_the_childs_run_root_and_socket_contract() {
        let dir = std::env::temp_dir().join(format!("zprod2-binds-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let mut keeper = PersistentZygote {
            root: dir.clone(),
            plan: dir.clone(),
            key: key(),
            keeper: None,
            cmd: None,
            epoch: "1".into(),
            zygote_pid: 2,
            boot_ms: Some(13),
            idle_exit_s: 0,
            ack_timeout: Duration::from_millis(100),
            log_path: dir.join("keeper.log"),
            last_child: HashMap::new(),
        };
        keeper.prepare_session(3).unwrap();
        keeper.publish_binds(3).unwrap();
        let text = fs::read_to_string(dir.join("3/binds")).unwrap();
        assert!(text.contains("/sessions/3/run/app-data\t/data/app-data"), "{text}");
        assert!(text.contains("/sessions/3/sock/peer.sock\t/data/acos-launch.sock"), "{text}");
        assert_eq!(text.lines().count(), 9);
        let _ = fs::remove_dir_all(&dir);
    }

    /// A mock keeper: the real protocol (§"The contract" above), no ART. It proves
    /// the host half — FIFO verbs, file acks, run-root wipe, epoch stability,
    /// shutdown — the same way Z4's Java zygote proved the guest half.
    fn mock_keeper(dir: &Path) -> PathBuf {
        let script = dir.join("mock-keeper.sh");
        fs::write(
            &script,
            r#"#!/bin/sh
plan="$ACOS_ZYGOTE_PLAN_DIR"
[ -n "$ACOS_ZYGOTE_APP" ] || { echo "no zygote build" >&2; exit 64; }
[ "$ACOS_ZYGOTE_PERSIST" = "1" ] || { echo "not persistent" >&2; exit 64; }
echo "epoch=$$.1 boot_ms=13" > "$plan/ready"
echo 2 > "$plan/zygote.pid"
n=100
while read -r verb tag; do
  case "$verb" in
    spawn) n=$((n+1)); echo "pid=$n" > "$plan/$tag/spawned"; echo "$n" > "$plan/$tag/pid" ;;
    kill) p=$(cat "$plan/$tag/pid" 2>/dev/null); echo "status=0x9 pid=$p" > "$plan/$tag/reaped" ;;
    exit) exit 0 ;;
  esac
done < "$plan/cmd"
"#,
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        script
    }

    #[test]
    fn the_keeper_protocol_spawn_kill_reset_and_shutdown() {
        let dir = std::env::temp_dir().join(format!("zprod2-proto-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let script = mock_keeper(&dir);
        let zygote_build = dir.join("zbuild");
        fs::create_dir_all(&zygote_build).unwrap();
        let mut keeper = PersistentZygote::start(
            &dir,
            key(),
            &script,
            Path::new("/nonexistent/counter.apk"),
            &[],
            &zygote_build,
        )
        .expect("the mock keeper boots");
        assert!(keeper.alive());
        assert_eq!(keeper.zygote_pid(), 2);
        let epoch = keeper.boot_epoch().to_string();
        assert!(!epoch.is_empty(), "the boot epoch comes from <plan>/ready");
        assert_eq!(keeper.boot_ms(), Some(13));

        // A session: prepare (fresh run root), publish binds, spawn.
        keeper.prepare_session(0).unwrap();
        fs::write(keeper.run_root_of(0).join("app-data/state.txt"), b"leak").unwrap();
        keeper.publish_binds(0).unwrap();
        let first = keeper.spawn(0).expect("spawn is acked");
        assert_eq!(first.child_pid, 101);
        assert_eq!(keeper.child_pid_of(0), Some(101));

        // The reset: kill one child, wipe its run root, fork one fresh child from
        // the same zygote (same pid, same boot epoch).
        let (previous, fresh) = keeper.reset(0).expect("reset is acked");
        assert_eq!(previous, Some(101), "the reply must name the child it killed");
        assert_eq!(fresh.child_pid, 102, "a genuinely different process answered");
        assert_ne!(fresh.child_pid, previous.unwrap());
        assert!(!keeper.run_root_of(0).join("app-data/state.txt").exists());
        assert_eq!(keeper.boot_epoch(), epoch, "a reset must not restart the zygote");
        assert_eq!(keeper.zygote_pid(), 2);

        // Shutdown ends the keeper: no leaked zygote process after serve exit.
        let keeper_pid = keeper.keeper.as_ref().map(|c| c.id()).unwrap();
        keeper.shutdown();
        assert!(!keeper.alive());
        assert!(keeper.keeper.is_none(), "the child handle is reaped");
        assert!(
            !Path::new(&format!("/proc/{keeper_pid}")).exists(),
            "the keeper process {keeper_pid} is gone"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn one_session_bad_tag_is_a_structured_error_not_a_hang() {
        let dir = std::env::temp_dir().join(format!("zprod2-timeout-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let script = mock_keeper(&dir);
        let mut keeper = PersistentZygote::start(
            &dir,
            key(),
            &script,
            Path::new("/nonexistent/counter.apk"),
            &[],
            &dir,
        )
        .unwrap();
        keeper.ack_timeout = Duration::from_millis(80);
        keeper.prepare_session(1).unwrap();
        // `kill 1` on a tag that never spawned: the mock keeper writes an empty
        // pid, and the host must fail with a named error rather than hang.
        keeper.kill(1).expect("the mock acks with the pid it had");
        let error = keeper.wait_contract(9, Duration::from_millis(60)).unwrap_err();
        assert!(error.to_string().starts_with("ZYGOTE_DEAD"), "{error}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_dead_keeper_is_a_structured_error_never_a_write() {
        let dir = std::env::temp_dir().join(format!("zprod2-dead-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("keeper.log"), "ZP1 ready epoch=1\nZL update failed\n").unwrap();
        let mut keeper = PersistentZygote {
            root: dir.clone(),
            plan: dir.clone(),
            key: key(),
            keeper: None,
            cmd: None,
            epoch: "1".into(),
            zygote_pid: 2,
            boot_ms: Some(13),
            idle_exit_s: 0,
            ack_timeout: Duration::from_millis(50),
            log_path: dir.join("keeper.log"),
            last_child: HashMap::new(),
        };
        assert!(!keeper.alive(), "no keeper process and no ready file");
        let error = keeper.spawn(0).unwrap_err();
        let text = error.to_string();
        assert!(text.starts_with("ZYGOTE_DEAD"), "{text}");
        assert!(text.contains("ZL update failed"), "the log tail is evidence: {text}");
        assert!(keeper.dead_reason().is_some());
        let _ = fs::remove_dir_all(&dir);
    }
}
