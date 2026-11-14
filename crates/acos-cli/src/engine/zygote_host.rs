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
            build,
            keepers: Mutex::new(ZygoteRegistry::new()),
        }
    }

    /// An explicitly disabled host (tests, and any caller that must not read the
    /// environment). The flag-off code path touches no keeper and creates no
    /// directory.
    pub fn disabled() -> Self {
        ZygoteHost {
            enabled: false,
            root: PathBuf::new(),
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
        self.lock()
            .keeper_pids()
    }

    /// Where this host keeps its plan directories.
    #[allow(dead_code)]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// `<plan>/<tag>` for the session launcher's `ACOS_ZYGOTE_SESSION`.
    pub fn session_dir(&self, key: &ZygoteKey, tag: u32) -> PathBuf {
        self.root
            .join("zygote")
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
        let mut keepers = self.lock();
        if keepers.keeper_mut(key).is_none() {
            let keeper =
                PersistentZygote::start(&self.root, key.clone(), binary, apk, &[], &build)?;
            keepers.insert(key.clone(), keeper);
        }
        let tag = match reuse_tag {
            Some(tag) => tag,
            None => keepers.allocate_tag(),
        };
        let keeper = keepers
            .keeper_mut(key)
            .expect("the keeper was just inserted");
        keeper.prepare_session(tag)?;
        Ok(tag)
    }

    /// Wait for the session launcher's `argv`, publish this session's `binds`,
    /// then fork one fresh child. Runs **before** the launcher's ready line: the
    /// ready line means the child connected to the session's own sockets.
    pub fn wait_contract_and_spawn(
        &self,
        key: &ZygoteKey,
        tag: u32,
        timeout: Duration,
    ) -> Result<ZygoteChild, ZygoteError> {
        let mut keepers = self.lock();
        let keeper = keepers
            .keeper_mut(key)
            .ok_or_else(|| ZygoteError::Contract(format!("no keeper for tag {tag}")))?;
        keeper.wait_contract(tag, timeout)?;
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
        self.lock().keeper_mut(key).map(|k| k.boot_epoch().to_string())
    }

    /// Close every keeper (serve exit). Idempotent.
    pub fn shutdown_all(&self) {
        self.lock().shutdown_all();
    }

    fn lock(&self) -> MutexGuard<'_, ZygoteRegistry> {
        self.keepers.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl Drop for ZygoteHost {
    fn drop(&mut self) {
        self.lock().shutdown_all();
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
    out.insert("isolation".into(), serde_json::json!(ISOLATION_ZYGOTE_CHILD));
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
    out.insert("memory_scope".into(), serde_json::json!(MEMORY_SCOPE_ZYGOTE));
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    fn key() -> ZygoteKey {
        ZygoteKey {
            apk: "/apks/counter.apk".into(),
            boot_image: "default".into(),
            width: 1080,
            height: 2400,
            density: 420,
            insets: "report".into(),
            animations: "on".into(),
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
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        script
    }

    fn host_with(root: &Path, build: &Path) -> ZygoteHost {
        ZygoteHost {
            enabled: true,
            root: root.to_path_buf(),
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
        assert!(!host.enabled(), "the zygote is opt-in (ADR-0026 decision 1)");
        assert_eq!(host.keeper_count(), 0);
        assert!(host.keeper_pids().is_empty());
        assert!(!dir.join("zygote").exists(), "no keeper directory with the flag off");
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
        assert_eq!(fields["isolation"], serde_json::json!(ISOLATION_ZYGOTE_CHILD));
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
        assert_eq!(details["zygote"]["fallback"], "none (a zygote failure never runs the exec path)");
        assert!(details["memory_scope"].as_str().unwrap().contains("shared-scope"));
        assert_eq!(host.keeper_count(), 0, "a dead keeper is not kept in the registry");
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
        let tag0 = host.prepare(&k, &script, Path::new("/apks/counter.apk"), None).unwrap();
        publish_argv(&host, &k, tag0);
        let child0 = host
            .wait_contract_and_spawn(&k, tag0, Duration::from_secs(5))
            .unwrap();
        let tag1 = host.prepare(&k, &script, Path::new("/apks/counter.apk"), None).unwrap();
        publish_argv(&host, &k, tag1);
        let child1 = host
            .wait_contract_and_spawn(&k, tag1, Duration::from_secs(5))
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
            .wait_contract_and_spawn(&k, tag0, Duration::from_secs(5))
            .unwrap();
        assert_ne!(fresh.child_pid, child0.child_pid, "a genuinely fresh child");
        assert_eq!(host.boot_epoch(&k).unwrap(), epoch, "epoch is stable across a reset");
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
