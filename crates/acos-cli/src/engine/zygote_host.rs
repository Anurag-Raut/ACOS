//! The engine's ownership of the production zygote (`ACOS_ART_ZYGOTE=on`).
//!
//! The mechanism lives in `acos_art::zygote_prod` (ZPROD2): the keeper
//! launcher, the plan directory, the command FIFO and the file-acked verbs.
//! This module is the **engine half** (ZPROD3, design §3/§4/§6):
//!
//! * `acos serve` owns the keepers, not the sessions. One [`ZygoteHost`] per
//!   daemon, one keeper per [`ZygoteKey`] inside it, closed by
//!   [`ZygoteHost::shutdown_all`] on serve exit (a keeper outlives *sessions*,
//!   never a *serve*).
//! * Every failure is a structured `ZYGOTE_DEAD` (`ErrorCode::ZygoteDead`,
//!   class `host_guard`) carrying the keeper's exit status and its last log
//!   lines. There is **no** path from a zygote failure to the exec path: the
//!   flag-on session either runs a real zygote child or fails closed (design
//!   §6 "never silently replaced").
//! * Memory accounting is **not** per child in this increment (F3/P2 is
//!   deferred): every child shares the keeper's cgroup scope, so every reply
//!   that carries zygote facts says `memory_scope: "shared-scope ..."` instead
//!   of implying a per-session limit that does not exist.
//!
//! The session wiring is in `engine/art.rs` (`ArtSession::start_with_zygote`,
//! the reset branch); the reply fields are in [`reply_fields`].

use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use acos_art::zygote_prod::{
    PersistentZygote, ZygoteChild, ZygoteError, ZygoteKey, ZygoteRegistry,
};
use acos_protocol::ErrorCode;
use acos_verify::BackendError;

/// The single product knob (design §7). `on` selects the persistent zygote;
/// anything else (including unset) is the unchanged exec path.
pub const ENV_ZYGOTE: &str = "ACOS_ART_ZYGOTE";
/// The guest dex + shims (built by `research/zygote-proto/z3/build.sh`). A flag-on
/// serve without it fails closed; it never falls back to the exec path.
pub const ENV_ZYGOTE_BUILD: &str = "ACOS_ART_ZYGOTE_BUILD";
/// ZPROD5: where the keeper plan trees live. Host-side only (the plan dir is
/// bound into the zygote as `/sessions`), so an operator may place it anywhere.
pub const ENV_ZYGOTE_ROOT: &str = "ACOS_ART_ZYGOTE_ROOT";
/// `AF_UNIX`'s `sun_path`, **including** the terminating NUL.
pub const SUN_PATH_MAX: usize = 108;
/// The longest leaf a session launcher binds inside `<session>/sock` (`peer.sock`;
/// the logd socket is `logdw`). Every session directory therefore has to leave
/// room for this suffix in `sun_path`.
const LONGEST_SOCKET_LEAF: &str = "/sock/peer.sock";
/// The longest `ZygoteKey::dir_name()` can be (5-digit dimension, 4-digit
/// density, `report` insets, 16 hex of hash) and the widest tag a daemon can
/// hand out in practice. Used only for the up-front root choice; `prepare`
/// checks the **actual** session path and fails closed (see [`sun_path_ok`]).
const WORST_KEY_DIR: &str = "00000000-00000x00000-0000-report-0000000000000000";
const WORST_TAG_DIGITS: usize = 4;
/// The `isolation` string a zygote reset/status reply carries (design §4.5).
pub const ISOLATION_ZYGOTE_CHILD: &str = "fresh-zygote-child";
/// What a zygote reply must say about memory while F3 is deferred: the children
/// of one keeper share its cgroup scope, so no reply may imply a per-child limit.
pub const MEMORY_SCOPE_ZYGOTE: &str = "shared-scope (per-child accounting deferred, F3)";

/// One session's binding to a keeper child, as the engine observed it.
#[derive(Debug, Clone)]
pub struct ZygoteFacts {
    pub tag: u32,
    pub child_pid: u32,
    pub run_root: PathBuf,
    pub boot_epoch: String,
}

impl ZygoteFacts {
    pub fn of(child: &ZygoteChild, boot_epoch: String) -> Self {
        ZygoteFacts {
            tag: child.tag,
            child_pid: child.child_pid,
            run_root: child.run_root.clone(),
            boot_epoch,
        }
    }
}

/// The daemon's keepers. Interior-mutable (`Mutex`) because the engine's verbs
/// take `&self` and two connections may address two sessions of one keeper.
pub struct ZygoteHost {
    enabled: bool,
    root: PathBuf,
    /// ZPROD5: `root` is a short runtime directory this host created (see
    /// [`ZygoteHost::for_artifacts`]); [`ZygoteHost::shutdown_all`] removes it.
    owned_root: bool,
    build: Option<PathBuf>,
    keepers: Mutex<ZygoteRegistry>,
}

impl ZygoteHost {
    /// Build the host from the environment. `root` is inside the daemon's
    /// private runtime directory (`<artifacts>/zygote`), never `/tmp`.
    pub fn from_env(root: PathBuf) -> Self {
        let enabled = std::env::var(ENV_ZYGOTE)
            .map(|value| value.trim().eq_ignore_ascii_case("on"))
            .unwrap_or(false);
        let build = std::env::var_os(ENV_ZYGOTE_BUILD).map(PathBuf::from);
        ZygoteHost {
            enabled,
            root,
            owned_root: false,
            build,
            keepers: Mutex::new(ZygoteRegistry::new()),
        }
    }

    /// The host for `acos serve`'s / `acos verify`'s artifact root.
    ///
    /// ZPROD5 (root cause of the ON arm's `argv 0` stall): a session directory
    /// also holds the **sockets** (`<session>/sock/peer.sock`), because the
    /// forked zygote child bind-mounts `/sessions/<tag>/binds` — and an `AF_UNIX`
    /// `sun_path` is 108 bytes including its NUL. `acos verify`'s artifacts root
    /// is short (`out/<scenario>`), so the natural `<artifacts>/zygote` fit; the
    /// **daemon** cohort's root is the caller's scratch
    /// (`…/sessions/zygote` in `acos-bench`), and there the session launcher died
    /// with `io: path must be shorter than SUN_LEN` **before** it published
    /// `<plan>/0/argv`, which the engine could only report as a 180 s contract
    /// timeout. The plan tree is host-side only (bound into the zygote as
    /// `/sessions`), so when the natural root cannot host a session's socket we
    /// relocate it to a short runtime directory — the same class of directory
    /// W43 already uses for the exec path's sockets.
    ///
    /// Order: `ACOS_ART_ZYGOTE_ROOT` (explicit, caller-owned) →
    /// `<artifacts>/zygote` when a worst-case session still fits `sun_path` →
    /// `/run/user/<uid>/z<pid>` → `/dev/shm/z<pid>`. If nothing fits, the
    /// natural root is kept and [`ZygoteHost::prepare`] fails closed with the
    /// lengths instead of letting a launcher die into a timeout.
    ///
    /// Note for a **memory** measurement: a relocated root sits on a tmpfs, so a
    /// session's run root (dalvik-cache, app-data, tmp) is tmpfs-backed there.
    /// Latency is unaffected; a memory row must use an on-disk
    /// `ACOS_ART_ZYGOTE_ROOT` (F3 follow-up).
    pub fn for_artifacts(artifacts: &Path) -> Self {
        if let Some(root) = std::env::var_os(ENV_ZYGOTE_ROOT)
            .map(PathBuf::from)
            .filter(|path| !path.as_os_str().is_empty())
        {
            return Self::for_root(root);
        }
        let natural = artifacts.join("zygote");
        if sun_budget_fits(&natural) {
            return Self::for_root(natural);
        }
        match open_short_root() {
            Some(root) => {
                let mut host = Self::for_root(root);
                host.owned_root = true;
                host
            }
            None => Self::for_root(natural),
        }
    }

    fn for_root(root: PathBuf) -> Self {
        Self::from_env(root)
    }

    /// An explicitly disabled host (tests, and any caller that must not read the
    /// environment). The flag-off code path touches no keeper and creates no
    /// directory.
    pub fn disabled() -> Self {
        ZygoteHost {
            enabled: false,
            root: PathBuf::new(),
            owned_root: false,
            build: None,
            keepers: Mutex::new(ZygoteRegistry::new()),
        }
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    /// How many keepers this daemon owns (0 with the flag off, by construction).
    pub fn keeper_count(&self) -> usize {
        self.lock().len()
    }

    /// The keeper pids this host is responsible for (a leak check reads these).
    pub fn keeper_pids(&self) -> Vec<u32> {
        self.lock().keeper_pids()
    }

    /// Where this host keeps its plan directories.
    #[allow(dead_code)]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// `<plan>/<tag>` for the session launcher's `ACOS_ZYGOTE_SESSION`.
    pub fn session_dir(&self, key: &ZygoteKey, tag: u32) -> PathBuf {
        self.root
            .join(key.dir_name())
            .join("plan")
            .join(tag.to_string())
    }

    /// Boot the keeper for `key` if this is its first session, allocate `tag`
    /// (or reuse the one a reset is replacing) and wipe that session directory.
    ///
    /// `reuse_tag` is `Some` only on the reset path (design §4: the same tag is
    /// killed and reforked); a fresh session always gets a never-used tag so a
    /// stale contract can never answer for it.
    pub fn prepare(
        &self,
        key: &ZygoteKey,
        binary: &Path,
        apk: &Path,
        reuse_tag: Option<u32>,
    ) -> Result<u32, ZygoteError> {
        let build = self.build.clone().ok_or_else(|| {
            ZygoteError::Io(format!(
                "{ENV_ZYGOTE}=on but {ENV_ZYGOTE_BUILD} is unset: the zygote guest dex is \
                 required (fail closed, no exec fallback)"
            ))
        })?;
        if !build.is_dir() {
            return Err(ZygoteError::Io(format!(
                "{ENV_ZYGOTE_BUILD}={} is not a directory (fail closed, no exec fallback)",
                build.display()
            )));
        }
        // ZPROD5: the session directory also carries the sockets, and `bind()`
        // refuses a `sun_path` over 108 bytes. Check the real path *before* a
        // keeper boots, with the widest tag this host can hand out, so a root
        // that cannot host a session is a named `ZYGOTE_DEAD` here instead of a
        // launcher dying into a contract timeout (see [`Self::for_artifacts`]).
        // The keeper's zygote (and therefore every forked child) inherits this
        // environment, so it must carry the *guest* seams the session launcher
        // sets for the exec path: the interactive live tree probe, the input
        // ack window and the guest settle window. Without them the child runs
        // but publishes no probe, and the session dies with `NO_LIVE_FRAME`
        // (measured on the first flag-on gate run of this increment).
        let frame_wait = std::env::var("ACOS_ART_FRAME_WAIT_SECS")
            .ok()
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| "3".to_string());
        // ZPROD4 (merge of `main` `389429a`): `main` made `shared-fixed` the
        // default boot image, and ART refuses it under `-Xzygote` — the zygote's
        // `only_use_system_oat_files_` rejects a `/data/img` image with
        // `Registering a non /system oat file: /data/img/boot.oat` (SIGABRT in the
        // keeper; measured on the first post-merge flag-on run, and a documented
        // limitation, ZS-security-review F11 / Z1 §5.2). The keeper therefore pins
        // the in-memory stock `lz4` image (the pre-merge default) unless the caller
        // explicitly chose an image: a chosen `ACOS_ART_BOOT_IMAGE` is passed
        // through untouched and still fails closed if ART refuses it.
        let boot_image = std::env::var("ACOS_ART_BOOT_IMAGE")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| "lz4".to_string());
        let keeper_env: Vec<(String, String)> = vec![
            ("ACOSLAUNCH_INTERACTIVE".into(), "1".into()),
            ("ACOSLAUNCH_LIVE_PROBE".into(), "1".into()),
            ("ACOSLAUNCH_FRAME_WAIT_SECS".into(), frame_wait),
            ("ACOS_INPUT_ACK_MS".into(), "2500".into()),
            ("ACOS_ZYGOTE_PROBE".into(), "1".into()),
            ("ACOS_ART_BOOT_IMAGE".into(), boot_image),
        ];
        let mut keepers = self.lock();
        // ZPROD4 (design §6): a keeper that died is not "a keeper". It is removed
        // (its exit status and log tail are already carried by the request that
        // observed the death) and the NEXT request boots a fresh keeper: a new
        // zygote, hence a new `zygote_boot_epoch` and a fresh fork. Sessions that
        // still hold the dead epoch are reported lost by [`Self::verb_error`];
        // they are never re-pointed at this new keeper.
        if let Some(keeper) = keepers.keeper_mut(key) {
            if !keeper.alive() {
                let dead_epoch = keeper.boot_epoch().to_string();
                let reason = keeper
                    .dead_reason()
                    .map(|error| error.to_string())
                    .unwrap_or_else(|| "keeper is gone".to_string());
                eprintln!(
                    "acos: zygote: keeper for {} is dead (epoch {dead_epoch}); booting a new \
                     keeper (a session of the dead epoch is lost, never re-pointed): {reason}",
                    key.dir_name()
                );
                keeper.shutdown();
                keepers.remove(key);
            }
        }
        // ZPROD5: the session directory also carries the sockets (`<session>/sock`
        // is what the forked child bind-mounts as `/dev/socket`), and `bind()`
        // refuses a `sun_path` over 108 bytes. Check the **exact** path with the
        // tag this session will really use, before a keeper boots: a root that
        // cannot host a session is a named `ZYGOTE_DEAD` here instead of a session
        // launcher dying into a contract timeout (see [`Self::for_artifacts`]).
        let tag = match reuse_tag {
            Some(tag) => tag,
            None => keepers.allocate_tag(),
        };
        if let Some(longest) = sun_path_overflow(&self.session_dir(key, tag)) {
            return Err(ZygoteError::Io(format!(
                "the session directory is too long for an AF_UNIX socket: {} needs {} bytes, \
                 sun_path holds {SUN_PATH_MAX} including its NUL (session root {}). Set \
                 {ENV_ZYGOTE_ROOT} to a shorter directory (at most {} bytes) or choose a \
                 shorter artifact root; the keeper was not booted and no exec-path session is \
                 substituted.",
                longest.display(),
                longest.as_os_str().len() + 1,
                self.root.display(),
                max_root_len()
            )));
        }
        if keepers.keeper_mut(key).is_none() {
            let keeper =
                PersistentZygote::start(&self.root, key.clone(), binary, apk, &keeper_env, &build)?;
            keepers.insert(key.clone(), keeper);
        }
        let keeper = keepers
            .keeper_mut(key)
            .expect("the keeper was just inserted");
        keeper.prepare_session(tag)?;
        Ok(tag)
    }

    /// Wait for the session launcher's `argv`, publish this session's `binds`,
    /// then fork one fresh child. Runs **before** the launcher's ready line: the
    /// ready line means the child connected to the session's own sockets.
    ///
    /// `peer_gone` reports why the launcher can no longer publish the contract
    /// (its exit status + stderr tail); ZPROD5, a dead launcher must not be
    /// waited out as a 180 s "keeper not responding" timeout.
    pub fn wait_contract_and_spawn<F>(
        &self,
        key: &ZygoteKey,
        tag: u32,
        timeout: Duration,
        peer_gone: F,
    ) -> Result<ZygoteChild, ZygoteError>
    where
        F: FnMut() -> Option<String>,
    {
        let mut keepers = self.lock();
        let keeper = keepers
            .keeper_mut(key)
            .ok_or_else(|| ZygoteError::Contract(format!("no keeper for tag {tag}")))?;
        keeper.wait_contract_with(tag, timeout, peer_gone)?;
        keeper.spawn(tag)
    }

    /// Kill one child's process group (§4.1: by group, never `cgroup.kill`).
    pub fn kill(&self, key: &ZygoteKey, tag: u32) -> Result<Option<u32>, ZygoteError> {
        let mut keepers = self.lock();
        let keeper = keepers
            .keeper_mut(key)
            .ok_or_else(|| ZygoteError::Contract(format!("no keeper for tag {tag}")))?;
        keeper.kill(tag)
    }

    /// The keeper's `epoch=<n>`. Unchanged across a reset, new after a restart.
    pub fn boot_epoch(&self, key: &ZygoteKey) -> Option<String> {
        self.lock()
            .keeper_mut(key)
            .map(|k| k.boot_epoch().to_string())
    }

    /// The epoch of a keeper that is **still alive**, or `None`. ZPROD4: the
    /// cached epoch string of a dead keeper must never answer this, or a lost
    /// session would look like a live one.
    pub fn live_boot_epoch(&self, key: &ZygoteKey) -> Option<String> {
        let mut keepers = self.lock();
        let keeper = keepers.keeper_mut(key)?;
        if keeper.alive() {
            Some(keeper.boot_epoch().to_string())
        } else {
            None
        }
    }

    /// The structured failure for a zygote verb that failed on a session.
    ///
    /// ZPROD4: a failure whose keeper is gone *or* whose `zygote_boot_epoch` is
    /// not the epoch the session was born in is a **lost session**: the reply
    /// names it (`details.zygote.session = "lost"`, the session's epoch, the
    /// keeper's current epoch or `null`) and the session is never re-pointed to
    /// the new keeper. Every other failure is the plain `ZYGOTE_DEAD`.
    pub fn verb_error(
        &self,
        key: &ZygoteKey,
        tag: u32,
        expected_epoch: &str,
        error: &ZygoteError,
    ) -> BackendError {
        let current = self.live_boot_epoch(key);
        if current.as_deref() == Some(expected_epoch) {
            return zygote_dead(&error.to_string());
        }
        let mut backend = zygote_dead(&format!(
            "SESSION_LOST: zygote session tag {tag} was born in zygote_boot_epoch \
             {expected_epoch}, but that keeper is gone (current: {}) and a lost session is never \
             re-pointed at a new keeper; relaunch it. Underlying failure: {error}",
            current.as_deref().unwrap_or("none")
        ));
        if let BackendError::Protocol { details, .. } = &mut backend {
            if let Some(zygote) = details
                .as_mut()
                .and_then(|value| value.get_mut("zygote"))
                .and_then(serde_json::Value::as_object_mut)
            {
                zygote.insert("session".into(), serde_json::json!("lost"));
                zygote.insert("zygote_tag".into(), serde_json::json!(tag));
                zygote.insert(
                    "session_boot_epoch".into(),
                    serde_json::json!(expected_epoch),
                );
                zygote.insert(
                    "keeper_boot_epoch".into(),
                    match &current {
                        Some(epoch) => serde_json::json!(epoch),
                        None => serde_json::Value::Null,
                    },
                );
                zygote.insert("repointed".into(), serde_json::json!(false));
            }
        }
        backend
    }

    /// Close every keeper (serve exit). Idempotent. A root this host created for
    /// itself (the short runtime root of [`Self::for_artifacts`]) is removed with
    /// it, so a serve leaves nothing behind.
    pub fn shutdown_all(&self) {
        self.lock().shutdown_all();
        if self.owned_root && !self.root.as_os_str().is_empty() {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    fn lock(&self) -> MutexGuard<'_, ZygoteRegistry> {
        self.keepers.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl Drop for ZygoteHost {
    fn drop(&mut self) {
        // ZPROD5: go through the host's own shutdown so a short runtime root this
        // host created is removed with it (a graceful serve exit leaves nothing).
        // A SIGKILLed serve cannot clean it; the next daemon's `prepare_session`
        // wipes its own session dirs, and the path is per-pid.
        ZygoteHost::shutdown_all(self);
    }
}

/// Does a worst-case session under `root` still fit `sun_path`?
///
/// Conservative on purpose: it decides a **root**, and [`ZygoteHost::prepare`]
/// then checks the session path it is actually about to create.
fn sun_budget_fits(root: &Path) -> bool {
    let worst = root
        .join(WORST_KEY_DIR)
        .join("plan")
        .join("0".repeat(WORST_TAG_DIGITS));
    sunded_len(&worst) + LONGEST_SOCKET_LEAF.len() + 1 <= SUN_PATH_MAX
}

fn sunded_len(path: &Path) -> usize {
    path.as_os_str().len()
}

/// The longest root that can still host a worst-case session's socket.
fn max_root_len() -> usize {
    SUN_PATH_MAX
        - 1
        - LONGEST_SOCKET_LEAF.len()
        - WORST_KEY_DIR.len()
        - WORST_TAG_DIGITS
        - "/plan".len()
        - 2
}

/// `Some(longest socket path)` when `<session>/sock/peer.sock` would not fit.
fn sun_path_overflow(session: &Path) -> Option<PathBuf> {
    let longest = session.join(LONGEST_SOCKET_LEAF.trim_start_matches('/'));
    if sunded_len(&longest) + 1 > SUN_PATH_MAX {
        Some(longest)
    } else {
        None
    }
}

/// A short directory this process can own: `/run/user/<uid>/z<pid>` (tmpfs, the
/// runtime dir W43's socket helper already uses), else `/dev/shm/z<pid>`.
///
/// A daemon that is SIGKILLed cannot remove its own root (`acos serve` installs
/// no SIGTERM handler either, so a terminated daemon does not run its `Drop`), so
/// opening one also sweeps the `z<pid>` roots of daemons that are **gone**. The
/// tree is per pid and bounded; a live pid's root is never touched.
fn open_short_root() -> Option<PathBuf> {
    let uid = unsafe { libc::getuid() };
    let pid = std::process::id();
    for parent in [
        PathBuf::from(format!("/run/user/{uid}")),
        PathBuf::from("/dev/shm"),
    ] {
        let candidate = parent.join(format!("z{pid}"));
        if std::fs::create_dir_all(&candidate).is_ok() {
            sweep_short_roots(&parent, pid);
            return Some(candidate);
        }
    }
    None
}

fn sweep_short_roots(parent: &Path, own_pid: u32) {
    let Ok(entries) = std::fs::read_dir(parent) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(pid) = name
            .to_str()
            .and_then(|name| name.strip_prefix('z'))
            .and_then(|rest| rest.parse::<u32>().ok())
        else {
            continue;
        };
        if pid == own_pid || Path::new(&format!("/proc/{pid}")).exists() {
            continue;
        }
        let _ = std::fs::remove_dir_all(entry.path());
    }
}

/// The structured failure for a zygote error: `ZYGOTE_DEAD`, class `host_guard`,
/// with the keeper's exit status and last log lines in `details.zygote`.
///
/// This is the **only** thing a zygote failure turns into. It never produces a
/// runnable session, so a caller cannot answer from the exec path while the
/// reply claims zygote isolation (§6).
pub fn zygote_dead(error: &str) -> BackendError {
    BackendError::Protocol {
        code: ErrorCode::ZygoteDead,
        detail: error.to_string(),
        hint: Some(ErrorCode::ZygoteDead.hint().to_string()),
        details: Some(serde_json::json!({
            "zygote": {
                "error": error,
                "fallback": "none (a zygote failure never runs the exec path)",
            },
            "memory_scope": MEMORY_SCOPE_ZYGOTE,
        })),
    }
}

/// The zygote fields a reset/status reply carries, or `None` with the flag off.
///
/// Present **only** when the session really runs a zygote child: `isolation`
/// names it, `child_pid`/`run_root`/`zygote_boot_epoch` are the observed facts,
/// and `memory_scope` states that memory is not per child (F3 deferred).
pub fn reply_fields(
    facts: Option<&ZygoteFacts>,
    previous_child_pid: Option<u32>,
) -> Option<serde_json::Map<String, serde_json::Value>> {
    let facts = facts?;
    let mut out = serde_json::Map::new();
    out.insert(
        "isolation".into(),
        serde_json::json!(ISOLATION_ZYGOTE_CHILD),
    );
    out.insert("child_pid".into(), serde_json::json!(facts.child_pid));
    out.insert(
        "previous_child_pid".into(),
        match previous_child_pid {
            Some(pid) => serde_json::json!(pid),
            None => serde_json::Value::Null,
        },
    );
    out.insert(
        "run_root".into(),
        serde_json::json!(facts.run_root.display().to_string()),
    );
    out.insert(
        "zygote_boot_epoch".into(),
        serde_json::json!(facts.boot_epoch),
    );
    out.insert("zygote_tag".into(), serde_json::json!(facts.tag));
    out.insert(
        "memory_scope".into(),
        serde_json::json!(MEMORY_SCOPE_ZYGOTE),
    );
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::Arc;

    fn key() -> ZygoteKey {
        ZygoteKey {
            apk: "/apks/counter.apk".into(),
            boot_image: "default".into(),
            width: 1080,
            height: 2400,
            density: 420,
            insets: "report".into(),
            animations: "on".into(),
            extra_apks: Vec::new(),
        }
    }

    /// The real keeper protocol (FIFO verbs + file acks), no ART: the engine half
    /// is proven deterministically instead of with an 8 s guest boot each time.
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
# The child pid a keeper hands out starts at its own pid, so a restarted
# keeper (a new process) can never hand out the dead keeper's child pid.
n=$$
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
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        script
    }

    fn host_with(root: &Path, build: &Path) -> ZygoteHost {
        ZygoteHost {
            enabled: true,
            root: root.to_path_buf(),
            owned_root: false,
            build: Some(build.to_path_buf()),
            keepers: Mutex::new(ZygoteRegistry::new()),
        }
    }

    /// The session launcher publishes `<plan>/<tag>/argv` during its own launch;
    /// the engine only waits for it (the mock launcher is this test).
    fn publish_argv(host: &ZygoteHost, key: &ZygoteKey, tag: u32) {
        let dir = host.session_dir(key, tag);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("argv"), b"counter.apk out 5 activity\n").unwrap();
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("zprod3-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn the_flag_off_default_touches_no_zygote() {
        // The product default: anything but `on` is the exec path. No keeper is
        // booted, no plan directory is created, no verb is ever sent.
        let dir = scratch("flagoff");
        let previous = std::env::var(ENV_ZYGOTE).ok();
        std::env::set_var(ENV_ZYGOTE, "off");
        let host = ZygoteHost::from_env(dir.join("zygote"));
        match previous {
            Some(value) => std::env::set_var(ENV_ZYGOTE, value),
            None => std::env::remove_var(ENV_ZYGOTE),
        }
        assert!(
            !host.enabled(),
            "the zygote is opt-in (ADR-0026 decision 1)"
        );
        assert_eq!(host.keeper_count(), 0);
        assert!(host.keeper_pids().is_empty());
        assert!(
            !dir.join("zygote").exists(),
            "no keeper directory with the flag off"
        );
        // The disabled host is the same object the flag-off engine holds.
        assert!(!ZygoteHost::disabled().enabled());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_reply_fields_exist_only_for_a_real_zygote_child() {
        // Flag off: the reply object is untouched, byte for byte.
        assert!(reply_fields(None, None).is_none());
        let facts = ZygoteFacts {
            tag: 3,
            child_pid: 101,
            run_root: PathBuf::from("/plan/3/run"),
            boot_epoch: "1".into(),
        };
        let fields = reply_fields(Some(&facts), Some(99)).expect("a zygote child states its facts");
        assert_eq!(
            fields["isolation"],
            serde_json::json!(ISOLATION_ZYGOTE_CHILD)
        );
        assert_eq!(fields["child_pid"], serde_json::json!(101));
        assert_eq!(fields["previous_child_pid"], serde_json::json!(99));
        assert_eq!(fields["run_root"], serde_json::json!("/plan/3/run"));
        assert_eq!(fields["zygote_boot_epoch"], serde_json::json!("1"));
        // F3 is deferred: a reply must say memory is not per child.
        let scope = fields["memory_scope"].as_str().unwrap();
        assert!(scope.contains("shared-scope"), "{scope}");
        assert!(scope.contains("deferred"), "{scope}");
        // A first launch has no previous child, and says so (`null`, not 0).
        let first = reply_fields(Some(&facts), None).unwrap();
        assert_eq!(first["previous_child_pid"], serde_json::Value::Null);
    }

    #[test]
    fn a_dead_keeper_is_a_zygote_dead_error_and_never_a_session() {
        // A keeper that dies before writing `ready`: the engine must fail closed
        // with the structured code and must not hand back anything runnable.
        let dir = scratch("dead");
        let script = dir.join("dies.sh");
        fs::write(&script, "#!/bin/sh\nexit 7\n").unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        let build = dir.join("zbuild");
        fs::create_dir_all(&build).unwrap();
        let host = host_with(&dir, &build);
        let error = host
            .prepare(&key(), &script, Path::new("/apks/counter.apk"), None)
            .expect_err("a dead keeper can never prepare a session");
        assert!(error.to_string().starts_with("ZYGOTE_DEAD"), "{error}");
        let backend = zygote_dead(&error.to_string());
        assert_eq!(backend.code(), ErrorCode::ZygoteDead);
        assert_eq!(ErrorCode::ZygoteDead.as_str(), "ZYGOTE_DEAD");
        assert_eq!(ErrorCode::ZygoteDead.class().as_str(), "host_guard");
        let details = match &backend {
            BackendError::Protocol { details, .. } => details.clone().unwrap(),
            other => panic!("expected a structured protocol error, got {other:?}"),
        };
        assert_eq!(
            details["zygote"]["fallback"],
            "none (a zygote failure never runs the exec path)"
        );
        assert!(details["memory_scope"]
            .as_str()
            .unwrap()
            .contains("shared-scope"));
        assert_eq!(
            host.keeper_count(),
            0,
            "a dead keeper is not kept in the registry"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_launcher_that_dies_before_argv_fails_fast_with_its_stderr() {
        // ZPROD5, the serve/daemon defect: the session launcher can die before it
        // publishes `<plan>/<tag>/argv` (measured: `sock/peer.sock` past
        // `sun_path` under the bench's deep serve scratch). The contract wait must
        // report THAT, immediately, instead of a 180 s "keeper not responding"
        // timeout that says nothing about the launcher.
        let dir = scratch("launcher-dead");
        let script = mock_keeper(&dir);
        let build = dir.join("zbuild");
        fs::create_dir_all(&build).unwrap();
        let host = host_with(&dir, &build);
        let k = key();
        let tag = host
            .prepare(&k, &script, Path::new("/apks/counter.apk"), None)
            .unwrap();
        // The launcher: prints the real diagnosis and exits without a contract.
        let mut launcher = std::process::Command::new("sh")
            .arg("-c")
            .arg("echo 'acos-art: io: path must be shorter than SUN_LEN' >&2; exit 3")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        // Drain its stderr the way the engine does (a tail read by the probe).
        let tail = {
            use std::io::Read;
            let mut pipe = launcher.stderr.take().unwrap();
            let mut text = String::new();
            pipe.read_to_string(&mut text).unwrap();
            Arc::new(Mutex::new(text))
        };
        let started = std::time::Instant::now();
        let error = host
            .wait_contract_and_spawn(&k, tag, Duration::from_secs(180), || {
                match launcher.try_wait().expect("observe the launcher") {
                    Some(status) => {
                        let text = tail.lock().unwrap_or_else(|e| e.into_inner()).clone();
                        Some(format!(
                            "the session launcher exited ({status}) before it published the \
                             contract (its stderr: {})",
                            text.trim()
                        ))
                    }
                    None => None,
                }
            })
            .expect_err("a dead launcher cannot publish a contract");
        let waited = started.elapsed();
        let text = error.to_string();
        assert!(
            waited < Duration::from_secs(10),
            "the wait must end with the launcher, not the 180 s timeout (waited {waited:?})"
        );
        assert!(text.starts_with("ZYGOTE_DEAD"), "{text}");
        assert!(text.contains("session launcher"), "{text}");
        assert!(
            text.contains("exit status: 3"),
            "the exit status is named: {text}"
        );
        assert!(
            text.contains("path must be shorter than SUN_LEN"),
            "the launcher's own diagnosis travels with the failure: {text}"
        );
        // The failure is a zygote failure: the keeper is untouched, and the code
        // the engine reports is the structured one.
        assert_eq!(host.keeper_count(), 1);
        assert_eq!(zygote_dead(&text).code(), ErrorCode::ZygoteDead);
        host.shutdown_all();
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_root_too_deep_for_sun_path_fails_closed_before_a_keeper_boots() {
        // ZPROD5: `<session>/sock/peer.sock` is an AF_UNIX path. A root that
        // cannot host it is refused *here*, by name and with the lengths, so no
        // keeper is booted and no launcher is asked to publish a contract it can
        // never satisfy.
        let dir = scratch("sunpath");
        let script = mock_keeper(&dir);
        let build = dir.join("zbuild");
        fs::create_dir_all(&build).unwrap();
        let deep = dir.join("a".repeat(80)).join("b".repeat(80));
        let host = host_with(&deep, &build);
        let error = host
            .prepare(&key(), &script, Path::new("/apks/counter.apk"), None)
            .expect_err("a root past sun_path cannot host a session");
        let text = error.to_string();
        assert!(text.contains("too long for an AF_UNIX socket"), "{text}");
        assert!(text.contains(ENV_ZYGOTE_ROOT), "the fix is named: {text}");
        assert!(
            text.contains("peer.sock"),
            "the failing path is named: {text}"
        );
        assert_eq!(
            host.keeper_count(),
            0,
            "the keeper must not boot for a refused root"
        );
        assert_eq!(host.keeper_pids().len(), 0, "and nothing is left running");
        assert!(
            !host.root().join("zygote").exists(),
            "no plan tree was written"
        );
        // The boundary itself: a session whose socket fits is accepted.
        let fits = PathBuf::from("/tmp").join("z");
        assert!(sun_path_overflow(&fits.join(WORST_KEY_DIR).join("plan/0")).is_none());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_deep_artifacts_root_is_displaced_to_a_short_root_and_a_short_one_is_kept() {
        // ZPROD5: the *choice* of root. `acos verify`'s artifacts root is short
        // (`out/<scenario>`), so it keeps `<artifacts>/zygote` exactly as before;
        // the bench's serve scratch
        // (`…/r1-a/latency/sessions`) is not, so the plan tree moves to a short
        // runtime root instead of dying in the launcher.
        let previous = std::env::var_os(ENV_ZYGOTE_ROOT);
        std::env::remove_var(ENV_ZYGOTE_ROOT);
        let deep = PathBuf::from("/home/somebody/a-fairly-long-worktree-name/.cache/h6")
            .join("r1-a/latency/sessions");
        assert!(
            !sun_budget_fits(&deep),
            "the bench-shaped root does not fit"
        );
        let host = ZygoteHost::for_artifacts(&deep);
        let session = host.session_dir(&key(), 7);
        assert!(
            sun_path_overflow(&session).is_none(),
            "the displaced root hosts a session socket: {}",
            session.display()
        );
        assert!(
            !session.starts_with(&deep),
            "the deep artifacts root is not used for the plan tree: {}",
            session.display()
        );
        assert!(host.root().is_dir(), "the short root was created");
        // A short artifacts root keeps the historical layout (verify).
        let short = PathBuf::from("out/v2-art-counter");
        assert!(sun_budget_fits(&short), "the verify root fits");
        let kept = ZygoteHost::for_artifacts(&short);
        assert_eq!(kept.root(), short.join("zygote"));
        // An explicit root always wins, short or not.
        std::env::set_var(ENV_ZYGOTE_ROOT, "/tmp/zprod5-explicit");
        let explicit = ZygoteHost::for_artifacts(&deep);
        assert_eq!(explicit.root(), Path::new("/tmp/zprod5-explicit"));
        match previous {
            Some(value) => std::env::set_var(ENV_ZYGOTE_ROOT, value),
            None => std::env::remove_var(ENV_ZYGOTE_ROOT),
        }
        // A root this host created goes away with the host (serve exit).
        let owned = host.root().to_path_buf();
        host.shutdown_all();
        assert!(
            !owned.exists(),
            "an owned root is removed on shutdown: {}",
            owned.display()
        );
    }

    #[test]
    fn a_dead_keeper_restarts_with_a_new_epoch_and_old_sessions_are_lost() {
        // ZPROD4: kill the keeper between two requests. (1) The request that sees
        // it dead fails structurally and says the session is *lost*. (2) The NEXT
        // request boots a NEW keeper with a NEW `zygote_boot_epoch` and reforks.
        // (3) The lost session is never re-pointed at the new epoch.
        let dir = scratch("restart");
        let script = mock_keeper(&dir);
        let build = dir.join("zbuild");
        fs::create_dir_all(&build).unwrap();
        let host = host_with(&dir, &build);
        let k = key();
        let tag0 = host
            .prepare(&k, &script, Path::new("/apks/counter.apk"), None)
            .unwrap();
        publish_argv(&host, &k, tag0);
        let child0 = host
            .wait_contract_and_spawn(&k, tag0, Duration::from_secs(5), || None)
            .unwrap();
        let epoch1 = host.boot_epoch(&k).unwrap();
        assert_eq!(
            epoch1.lines().next().unwrap_or_default(),
            epoch1,
            "the epoch is the keeper's ready line, not prose"
        );
        let keeper1 = *host.keeper_pids().first().expect("one keeper");

        // Kill the keeper exactly as a host OOM/crash would: SIGKILL, no shutdown
        // ack. The child it forked is gone with it (bwrap --die-with-parent).
        let killed = std::process::Command::new("kill")
            .arg("-9")
            .arg(keeper1.to_string())
            .status()
            .expect("kill the keeper");
        assert!(killed.success(), "keeper {keeper1} must die");
        std::thread::sleep(Duration::from_millis(150));

        // (1) The current request on the old session: structured, and *lost*.
        let error = host
            .kill(&k, tag0)
            .expect_err("a dead keeper cannot kill the old child");
        let backend = host.verb_error(&k, tag0, &epoch1, &error);
        assert_eq!(backend.code(), ErrorCode::ZygoteDead);
        let details = match &backend {
            BackendError::Protocol { details, .. } => details.clone().unwrap(),
            other => panic!("expected a structured protocol error, got {other:?}"),
        };
        assert_eq!(details["zygote"]["session"], serde_json::json!("lost"));
        assert_eq!(
            details["zygote"]["session_boot_epoch"],
            serde_json::json!(epoch1)
        );
        assert_eq!(
            details["zygote"]["keeper_boot_epoch"],
            serde_json::Value::Null
        );
        assert_eq!(details["zygote"]["repointed"], serde_json::json!(false));

        // (2) The next request boots a new keeper: new epoch, fresh fork.
        let tag1 = host
            .prepare(&k, &script, Path::new("/apks/counter.apk"), None)
            .unwrap();
        publish_argv(&host, &k, tag1);
        let child1 = host
            .wait_contract_and_spawn(&k, tag1, Duration::from_secs(5), || None)
            .unwrap();
        let epoch2 = host.boot_epoch(&k).unwrap();
        assert_ne!(epoch1, epoch2, "a restarted zygote has a new boot epoch");
        assert_ne!(child0.child_pid, child1.child_pid);
        assert_eq!(
            host.keeper_count(),
            1,
            "a dead keeper is replaced, not duplicated"
        );
        assert_ne!(
            host.keeper_pids()[0],
            keeper1,
            "the new keeper is a new process"
        );

        // (3) The old session still says lost, and now names the new epoch: it is
        // reported, never silently re-pointed at it.
        let backend = host.verb_error(&k, tag0, &epoch1, &error);
        let details = match &backend {
            BackendError::Protocol { details, .. } => details.clone().unwrap(),
            other => panic!("expected a structured protocol error, got {other:?}"),
        };
        assert_eq!(details["zygote"]["session"], serde_json::json!("lost"));
        assert_eq!(
            details["zygote"]["keeper_boot_epoch"],
            serde_json::json!(epoch2)
        );
        assert_eq!(details["zygote"]["repointed"], serde_json::json!(false));
        assert!(
            backend.to_string().contains("SESSION_LOST"),
            "the text names the loss: {backend}"
        );
        // A live session of the current epoch is not "lost": it is plain dead.
        let plain = host.verb_error(&k, tag1, &epoch2, &error);
        assert!(
            !plain.to_string().contains("SESSION_LOST"),
            "the current epoch is not a lost session: {plain}"
        );

        host.shutdown_all();
        assert_eq!(host.keeper_count(), 0);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn shutdown_all_leaves_no_keeper_and_no_child_behind() {
        // The acceptance "no keeper/child pid survives serve exit": serve
        // shutdown closes every keeper, and the keeper ends every child it forked.
        let dir = scratch("shutdown");
        let script = mock_keeper(&dir);
        let build = dir.join("zbuild");
        fs::create_dir_all(&build).unwrap();
        let host = host_with(&dir, &build);
        let k = key();
        let tag0 = host
            .prepare(&k, &script, Path::new("/apks/counter.apk"), None)
            .unwrap();
        publish_argv(&host, &k, tag0);
        let child0 = host
            .wait_contract_and_spawn(&k, tag0, Duration::from_secs(5), || None)
            .unwrap();
        let tag1 = host
            .prepare(&k, &script, Path::new("/apks/counter.apk"), None)
            .unwrap();
        publish_argv(&host, &k, tag1);
        let child1 = host
            .wait_contract_and_spawn(&k, tag1, Duration::from_secs(5), || None)
            .unwrap();
        assert_ne!(child0.child_pid, child1.child_pid);
        assert_eq!(host.keeper_count(), 1, "one keeper per key");
        let keeper_pids = host.keeper_pids();
        assert_eq!(keeper_pids.len(), 1);
        let epoch = host.boot_epoch(&k).unwrap();

        // Per-session reset reuses the killed tag: new child, same tag, same
        // epoch (design §4; a reset that restarted the zygote would change it).
        host.kill(&k, tag0).unwrap();
        let tag0_again = host
            .prepare(&k, &script, Path::new("/apks/counter.apk"), Some(tag0))
            .unwrap();
        assert_eq!(tag0_again, tag0, "a reset refork reuses its own tag");
        publish_argv(&host, &k, tag0);
        let fresh = host
            .wait_contract_and_spawn(&k, tag0, Duration::from_secs(5), || None)
            .unwrap();
        assert_ne!(fresh.child_pid, child0.child_pid, "a genuinely fresh child");
        assert_eq!(
            host.boot_epoch(&k).unwrap(),
            epoch,
            "epoch is stable across a reset"
        );
        assert!(fresh.run_root.ends_with(format!("{tag0}/run")));

        host.shutdown_all();
        assert_eq!(host.keeper_count(), 0);
        assert!(host.keeper_pids().is_empty());
        for pid in keeper_pids {
            assert!(
                !Path::new(&format!("/proc/{pid}")).exists(),
                "keeper {pid} must not survive serve exit"
            );
        }
        let _ = fs::remove_dir_all(&dir);
    }
}
