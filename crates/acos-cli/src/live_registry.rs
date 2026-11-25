//! The live-view session registry (VIEW1a, protocol v1 §5).
//!
//! Every live session publishes one small JSON file so a viewer hub can find it
//! without asking the daemon: `$XDG_RUNTIME_DIR/acos/live/<session_id>-<daemon_pid>.json`,
//! written atomically (tmp + rename), mode 0600, in a 0700 directory. The daemon
//! writes it when a session reaches `ready`, updates `activity` when the focused
//! activity changes, and removes it on destroy/close/daemon exit.
//!
//! Readers must treat an entry whose `daemon_pid` is dead as **stale** and sweep
//! it ([`is_stale`]): a daemon that was SIGKILLed cannot remove its own files.
//! Pid reuse is possible in principle; the hub also checks that the session's
//! `stream_socket` connects, which is the real liveness test.
//!
//! `client` and `project_dir` come from the environment the MCP server passes to
//! its private daemon (`ACOS_VIEW_CLIENT`, `ACOS_VIEW_PROJECT`); a CLI-started
//! daemon uses `acos-cli` and its own working directory.

use std::io;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Registry document version.
pub const REGISTRY_VERSION: u32 = 1;
/// Environment variable that overrides the registry directory (tests, tools).
pub const REGISTRY_DIR_ENV: &str = "ACOS_VIEW_REGISTRY_DIR";
/// Environment variable carrying the MCP client identity (`{"name","version"}`
/// JSON, or a plain `name` string).
pub const CLIENT_ENV: &str = "ACOS_VIEW_CLIENT";
/// Environment variable carrying the agent's project directory.
pub const PROJECT_ENV: &str = "ACOS_VIEW_PROJECT";
/// The client name a CLI-started daemon publishes.
pub const CLI_CLIENT: &str = "acos-cli";

/// One published session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LiveEntry {
    /// The daemon's session id.
    pub session_id: String,
    /// The `acos serve` daemon pid (the liveness key).
    pub daemon_pid: u32,
    /// The `acos-art` launcher pid of the guest, when known.
    pub launcher_pid: Option<u32>,
    /// When the session reached ready (host wall clock, ms since the epoch).
    pub started_ms: u64,
    /// The client that drives the session (`acos-cli`, or the MCP client name).
    pub client_name: String,
    /// The client's version, when it reported one.
    pub client_version: String,
    /// The agent's project directory.
    pub project_dir: String,
    /// The APK path the session runs.
    pub apk: String,
    /// The launched package.
    pub package: String,
    /// The human-readable app label from the APK manifest/resources, when known.
    pub app_label: Option<String>,
    /// Optional human-readable device name set on `acos_launch`.
    pub device_name: Option<String>,
    /// `true` for `acos verify` runs (panel input is refused / watch-only).
    pub watch_only: bool,
    /// `acos serve` Unix socket path for routing interactive panel input (`None` for verify runs).
    pub serve_socket: Option<String>,
    /// The focused activity, when known.
    pub activity: Option<String>,
    /// The session display size.
    pub display: (u32, u32),
    /// The live-view stream socket (`docs/LIVE_VIEW.md`), when the launcher
    /// bound one.
    pub stream_socket: Option<String>,
}

impl LiveEntry {
    /// The document written to disk.
    #[must_use]
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "v": REGISTRY_VERSION,
            "session_id": self.session_id,
            "daemon_pid": self.daemon_pid,
            "launcher_pid": self.launcher_pid,
            "started_ms": self.started_ms,
            "client": {"name": self.client_name, "version": self.client_version},
            "project_dir": self.project_dir,
            "apk": self.apk,
            "package": self.package,
            "app_label": self.app_label,
            "device_name": self.device_name,
            "watch_only": self.watch_only,
            "serve_socket": self.serve_socket,
            "activity": self.activity,
            "display": {"w": self.display.0, "h": self.display.1},
            "stream_socket": self.stream_socket,
        })
    }
}

/// The publisher: one per `acos serve` daemon.
#[derive(Clone, Debug)]
pub struct LiveRegistry {
    dir: PathBuf,
    client_name: String,
    client_version: String,
    project_dir: String,
}

impl LiveRegistry {
    /// The registry for this process, from the environment.
    ///
    /// The directory is `$ACOS_VIEW_REGISTRY_DIR`, else
    /// `$XDG_RUNTIME_DIR/acos/live`, else `/run/user/<uid>/acos/live`. It is
    /// created on first write (mode 0700), never at construction, so a daemon
    /// that never launches a session touches nothing.
    #[must_use]
    pub fn from_env() -> Self {
        let (client_name, client_version) = client_from_env();
        let project_dir = std::env::var(PROJECT_ENV)
            .ok()
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| {
                std::env::current_dir()
                    .map(|dir| dir.display().to_string())
                    .unwrap_or_default()
            });
        Self {
            dir: registry_dir_from_env(),
            client_name,
            client_version,
            project_dir,
        }
    }

    /// A registry rooted at an explicit directory (tests and tools).
    #[must_use]
    pub fn at(dir: PathBuf) -> Self {
        let mut registry = Self::from_env();
        registry.dir = dir;
        registry
    }

    /// The registry directory.
    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The client identity this daemon publishes.
    #[must_use]
    pub fn client(&self) -> (&str, &str) {
        (&self.client_name, &self.client_version)
    }

    /// The project directory this daemon publishes.
    #[must_use]
    pub fn project_dir(&self) -> &str {
        &self.project_dir
    }

    /// The file name of one session's entry.
    #[must_use]
    pub fn file_name(&self, session_id: &str, daemon_pid: u32) -> String {
        format!("{}-{daemon_pid}.json", sanitize(session_id))
    }

    /// Publish (or refresh) a session's entry, atomically.
    pub fn write(&self, entry: &LiveEntry) -> io::Result<PathBuf> {
        self.prepare_dir()?;
        let path = self
            .dir
            .join(self.file_name(&entry.session_id, entry.daemon_pid));
        write_atomic(&path, &entry.to_json().to_string())?;
        Ok(path)
    }

    /// Update `activity` in an existing entry (no-op when the file is gone).
    pub fn update_activity(&self, session_id: &str, activity: Option<&str>) -> io::Result<bool> {
        let path = self
            .dir
            .join(self.file_name(session_id, std::process::id()));
        let Ok(text) = std::fs::read_to_string(&path) else {
            return Ok(false);
        };
        let Ok(mut doc) = serde_json::from_str::<serde_json::Value>(&text) else {
            return Ok(false);
        };
        let current = doc
            .get("activity")
            .and_then(|value| value.as_str())
            .map(str::to_string);
        if current.as_deref() == activity {
            return Ok(false);
        }
        let Some(object) = doc.as_object_mut() else {
            return Ok(false);
        };
        object.insert(
            "activity".to_string(),
            match activity {
                Some(activity) => serde_json::json!(activity),
                None => serde_json::Value::Null,
            },
        );
        write_atomic(&path, &doc.to_string())?;
        Ok(true)
    }

    /// Remove one session's entry (destroy/close). Missing is success.
    pub fn remove(&self, session_id: &str) {
        let path = self
            .dir
            .join(self.file_name(session_id, std::process::id()));
        let _ = std::fs::remove_file(path);
    }

    /// Remove every entry this daemon published (daemon exit).
    pub fn remove_all(&self) {
        let pid = std::process::id();
        let Ok(entries) = std::fs::read_dir(&self.dir) else {
            return;
        };
        let suffix = format!("-{pid}.json");
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            if name.ends_with(&suffix) {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }

    fn prepare_dir(&self) -> io::Result<()> {
        use std::os::unix::fs::DirBuilderExt;
        if let Some(parent) = self.dir.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::DirBuilder::new()
                    .recursive(true)
                    .mode(0o700)
                    .create(parent)?;
            }
        }
        verify_private_dir(&self.dir, true)
    }
}

/// Create (when `create`) and verify a registry directory: a real directory (not
/// a symlink), owned by this uid, mode 0700 (VIEW1c L6).
///
/// A directory owned by someone else, or not a directory, is **refused** rather
/// than used (the registry carries the stream socket path and the project
/// metadata; another uid must neither read nor plant entries). A directory we
/// own with looser modes is tightened and re-checked; a failure is an error.
/// With `create == false` (the reader) nothing is created or changed: the
/// directory must be ours and not writable by others.
pub fn verify_private_dir(dir: &Path, create: bool) -> io::Result<()> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt};
    if create {
        match std::fs::DirBuilder::new().mode(0o700).create(dir) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
    }
    let metadata = std::fs::symlink_metadata(dir)?;
    if !metadata.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} is not a directory", dir.display()),
        ));
    }
    let uid = unsafe { libc::getuid() };
    if metadata.uid() != uid {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "{} is owned by uid {} (expected {uid}): refusing to use it",
                dir.display(),
                metadata.uid()
            ),
        ));
    }
    let mode = metadata.permissions().mode() & 0o777;
    if !create {
        // A reader does not chmod anything; it only refuses a directory another
        // uid could have planted entries in (group/world writable).
        if mode & 0o022 != 0 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("{} is writable by others (mode {mode:o})", dir.display()),
            ));
        }
        return Ok(());
    }
    if mode != 0o700 {
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
        let mode = std::fs::symlink_metadata(dir)?.permissions().mode() & 0o777;
        if mode != 0o700 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!(
                    "{} has mode {mode:o}, could not make it 0700",
                    dir.display()
                ),
            ));
        }
    }
    Ok(())
}

/// Write `text` to `path` atomically: a fresh tmp file with mode 0600, then a
/// rename. A reader never sees a half-written entry.
fn write_atomic(path: &Path, text: &str) -> io::Result<()> {
    use std::io::Write;
    let tmp = path.with_extension(format!("tmp{}", std::process::id()));
    {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)?;
        file.write_all(text.as_bytes())?;
        file.flush()?;
    }
    std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
    std::fs::rename(&tmp, path)
}

/// The registry directory for this process.
#[must_use]
pub fn registry_dir_from_env() -> PathBuf {
    if let Ok(dir) = std::env::var(REGISTRY_DIR_ENV) {
        if !dir.is_empty() {
            return PathBuf::from(dir);
        }
    }
    if let Ok(runtime) = std::env::var("XDG_RUNTIME_DIR") {
        if !runtime.is_empty() {
            return PathBuf::from(runtime).join("acos/live");
        }
    }
    let uid = unsafe { libc::getuid() };
    PathBuf::from(format!("/run/user/{uid}/acos/live"))
}

/// The client identity from `ACOS_VIEW_CLIENT`.
///
/// Accepted shapes: `{"name":"…","version":"…"}` (what the MCP server sets) or
/// a plain `name`. Anything else falls back to [`CLI_CLIENT`].
#[must_use]
pub fn client_from_env() -> (String, String) {
    let Ok(raw) = std::env::var(CLIENT_ENV) else {
        return (CLI_CLIENT.to_string(), String::new());
    };
    let raw = raw.trim();
    if raw.is_empty() {
        return (CLI_CLIENT.to_string(), String::new());
    }
    if raw.starts_with('{') {
        if let Ok(doc) = serde_json::from_str::<serde_json::Value>(raw) {
            let name = doc
                .get("name")
                .and_then(|value| value.as_str())
                .unwrap_or(CLI_CLIENT)
                .to_string();
            let version = doc
                .get("version")
                .and_then(|value| value.as_str())
                .unwrap_or("")
                .to_string();
            return (name, version);
        }
    }
    (raw.to_string(), String::new())
}

/// One entry read back from the registry directory.
#[derive(Clone, Debug)]
pub struct ReadEntry {
    /// The file it came from.
    pub path: PathBuf,
    /// The parsed document.
    pub doc: serde_json::Value,
    /// `true` when the publishing daemon is gone (see [`is_stale`]).
    pub stale: bool,
}

/// Read every entry in `dir` (unreadable files are skipped, never fatal).
///
/// The reader half of the protocol: `acos view` (VIEW1b) uses these three
/// functions; the producer never calls them, hence `allow(dead_code)` here.
#[must_use]
#[allow(dead_code)]
pub fn read_entries(dir: &Path) -> Vec<ReadEntry> {
    // L6: a directory another uid owns, or one that is group/world-writable (any
    // mode with 0o022 set; 0700 and 0755 are both accepted), is not ours: its
    // entries could point the hub at a socket that is not a session's.
    if verify_private_dir(dir, false).is_err() {
        return Vec::new();
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(doc) = serde_json::from_str::<serde_json::Value>(&text) else {
            continue;
        };
        let stale = is_stale(&doc);
        out.push(ReadEntry { path, doc, stale });
    }
    out
}

/// Remove every stale entry (the hub's sweep). Returns how many were removed.
#[allow(dead_code)]
pub fn sweep(dir: &Path) -> usize {
    let mut removed = 0;
    for entry in read_entries(dir) {
        if entry.stale && std::fs::remove_file(&entry.path).is_ok() {
            removed += 1;
        }
    }
    removed
}

/// `true` when the entry's daemon is dead: a reader must not trust it.
#[must_use]
pub fn is_stale(doc: &serde_json::Value) -> bool {
    let Some(pid) = doc.get("daemon_pid").and_then(|value| value.as_u64()) else {
        return true;
    };
    if pid == 0 || pid > u64::from(u32::MAX) {
        return true;
    }
    !pid_alive(pid as u32)
}

/// `true` when `pid` exists and this user may signal it (`EPERM` counts as
/// alive: the process is there, it is just not ours).
#[must_use]
#[allow(dead_code)]
pub fn pid_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    // SAFETY: `kill` with signal 0 only performs the existence/permission check.
    let rc = unsafe { libc::kill(pid as libc::pid_t, 0) };
    if rc == 0 {
        return true;
    }
    matches!(io::Error::last_os_error().raw_os_error(), Some(libc::EPERM))
}

/// Host wall clock in milliseconds since the Unix epoch.
#[must_use]
pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// A session id is a file name component: keep it boring.
fn sanitize(session_id: &str) -> String {
    let cleaned: String = session_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if cleaned.is_empty() {
        "session".to_string()
    } else {
        cleaned
    }
}

const MAX_DEVICE_NAME_CHARS: usize = 64;

/// Sanitize a caller-provided device display name: strip control characters,
/// newlines, and `=`, collapse whitespace, and cap at 64 Unicode scalar values.
/// Returns `None` when the sanitized result is empty.
#[must_use]
pub fn sanitize_device_name(raw: &str) -> Option<String> {
    let filtered: String = raw
        .chars()
        .map(|c| if c.is_control() || c == '=' { ' ' } else { c })
        .collect();
    let collapsed = filtered.split_whitespace().collect::<Vec<_>>().join(" ");
    let capped: String = collapsed.chars().take(MAX_DEVICE_NAME_CHARS).collect();
    let trimmed = capped.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// Root for the small files (and Unix sockets) the viewer tests create.
///
/// The rule lives in [`acos_test_support::short_scratch_root`] because it is one
/// class, not one test: a `sun_path` is 108 bytes including the terminating NUL,
/// and `<worktree>/.cache/lv/<pid>-frames-and-events-stream/producer.sock` is
/// over that in any long checkout (agent worktrees, CI job dirs). The root is
/// short by construction:
///
/// 1. `ACOS_VIEW_TEST_DIR` when it fits the socket budget;
/// 2. `<cwd>/.cache/lv` when the checkout is short enough for it (the normal
///    case; the scratch stays inside the workspace);
/// 3. `/dev/shm/acos-lv-tests`, a fixed short RAM-backed path, otherwise (these
///    tests write a handful of bytes each, never real artifacts);
/// 4. the system temp dir as the last resort.
///
/// A too-long `ACOS_VIEW_TEST_DIR` is reported once and ignored, so a caller
/// with a long scratch directory still gets a runnable test instead of a `bind`
/// failure (or a failing budget assert) deep in a test. The budget itself is
/// derived from the deepest name these tests bind, so it cannot be "generous"
/// and still overflow.
#[cfg(test)]
pub(crate) fn test_scratch_root() -> PathBuf {
    let candidates: Vec<PathBuf> = std::env::current_dir()
        .ok()
        .map(|cwd| vec![cwd.join(".cache/lv")])
        .unwrap_or_default();
    acos_test_support::short_scratch_root("ACOS_VIEW_TEST_DIR", &candidates, "acos-lv-tests")
}

/// A fresh per-process subdirectory of `root`, named `<pid>-<tag>`.
#[cfg(test)]
pub(crate) fn short_scratch(root: PathBuf, tag: &str) -> PathBuf {
    let dir = root.join(format!("{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

#[cfg(test)]
mod scratch_tests {
    use super::*;

    /// The deepest name these tests bind below the root; the budget is sized
    /// from exactly this shape (see `acos_test_support::SCRATCH_ROOT_BUDGET`).
    fn deepest_path(root: &Path) -> PathBuf {
        root.join(format!("{}-frames-and-events-stream", std::process::id()))
            .join("producer.sock")
    }

    #[test]
    fn the_test_scratch_root_leaves_room_for_a_unix_socket() {
        // The class of bug this fixes: the root plus a deep socket name must fit
        // `sun_path` (108 bytes with the NUL) in any checkout, however long.
        let deepest = deepest_path(&test_scratch_root());
        let len = deepest.as_os_str().as_encoded_bytes().len();
        assert!(
            len <= acos_test_support::SUN_PATH_MAX,
            "{} is {len} bytes, too long for sun_path ({} with the NUL)",
            deepest.display(),
            acos_test_support::SUN_PATH_MAX + 1
        );
    }

    #[test]
    fn a_long_view_test_dir_narrows_to_a_root_that_still_fits() {
        // `ACOS_VIEW_TEST_DIR` set to a long path (the SETTLE5 case: a 114-byte
        // socket path from a 67-byte root) must not fail the socket budget: the
        // rule picks the short root instead and says so. Decided without
        // touching the process environment, which parallel tests share.
        let long = PathBuf::from(format!("/tmp/{}", "x".repeat(70)));
        let checkout = PathBuf::from("/home/agent/wt/.cache/lv");
        let (root, ignored) = acos_test_support::pick_scratch_root(
            Some(&long),
            std::slice::from_ref(&checkout),
            "acos-lv-tests",
        );
        assert_eq!(ignored.as_deref(), Some(long.as_path()));
        let deepest = deepest_path(&root);
        assert!(
            deepest.as_os_str().as_encoded_bytes().len() <= acos_test_support::SUN_PATH_MAX,
            "{} is too long for sun_path",
            deepest.display()
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        short_scratch(test_scratch_root(), tag)
    }

    fn entry(session: &str) -> LiveEntry {
        LiveEntry {
            session_id: session.to_string(),
            daemon_pid: std::process::id(),
            launcher_pid: Some(std::process::id()),
            started_ms: now_ms(),
            client_name: "acos-cli".to_string(),
            client_version: "0.2.0".to_string(),
            project_dir: "/home/Qui/Projects/acos".to_string(),
            apk: "fixtures/build/apps/counter.apk".to_string(),
            package: "dev.acos.fixtures.counter".to_string(),
            app_label: Some("Counter".to_string()),
            device_name: None,
            watch_only: false,
            serve_socket: None,
            activity: Some("dev.acos.fixtures.counter.MainActivity".to_string()),
            display: (1080, 2400),
            stream_socket: Some("/run/user/1000/acos-art-1-0/view.sock".to_string()),
        }
    }

    #[test]
    fn write_update_remove_and_stale_detection() {
        let dir = scratch("registry");
        let registry = LiveRegistry::at(dir.clone());
        let path = registry.write(&entry("s_one")).expect("write");
        assert_eq!(
            path.file_name().and_then(|n| n.to_str()),
            Some(format!("s_one-{}.json", std::process::id()).as_str())
        );
        // 0600 file in a 0700 directory.
        let file_mode = std::fs::metadata(&path).expect("stat").permissions().mode() & 0o777;
        assert_eq!(file_mode, 0o600);
        let dir_mode = std::fs::metadata(&dir).expect("stat").permissions().mode() & 0o777;
        assert_eq!(dir_mode, 0o700);
        // The document carries every protocol field.
        let doc: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).expect("read")).expect("json");
        assert_eq!(doc["v"], REGISTRY_VERSION);
        assert_eq!(doc["session_id"], "s_one");
        assert_eq!(doc["daemon_pid"], std::process::id());
        assert_eq!(doc["client"]["name"], "acos-cli");
        assert_eq!(doc["client"]["version"], "0.2.0");
        assert_eq!(doc["project_dir"], "/home/Qui/Projects/acos");
        assert_eq!(doc["display"]["w"], 1080);
        assert_eq!(doc["display"]["h"], 2400);
        assert!(doc["stream_socket"]
            .as_str()
            .unwrap()
            .ends_with("view.sock"));
        // A live daemon's entry is not stale.
        let entries = read_entries(&dir);
        assert_eq!(entries.len(), 1);
        assert!(!entries[0].stale);
        // Activity updates rewrite the same file atomically and only on change.
        assert!(registry
            .update_activity("s_one", Some("dev.acos.Other"))
            .expect("update"));
        assert!(!registry
            .update_activity("s_one", Some("dev.acos.Other"))
            .expect("no change"));
        let doc: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).expect("read")).expect("json");
        assert_eq!(doc["activity"], "dev.acos.Other");
        assert_eq!(doc["package"], "dev.acos.fixtures.counter");
        // A dead daemon pid makes the entry stale; the sweep removes it.
        let mut dead = entry("s_dead");
        dead.daemon_pid = dead_pid();
        let dead_path = dir.join(format!("s_dead-{}.json", dead.daemon_pid));
        write_atomic(&dead_path, &dead.to_json().to_string()).expect("write dead");
        let entries = read_entries(&dir);
        assert_eq!(entries.len(), 2);
        let stale = entries
            .iter()
            .find(|e| e.doc["session_id"] == "s_dead")
            .expect("dead entry");
        assert!(stale.stale, "a dead daemon_pid must read as stale");
        assert_eq!(sweep(&dir), 1);
        assert_eq!(read_entries(&dir).len(), 1);
        // remove_all takes only this daemon's entries.
        registry.remove_all();
        assert_eq!(read_entries(&dir).len(), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_registry_directory_is_private_owned_and_never_a_symlink() {
        use std::os::unix::fs::symlink;
        let root = scratch("l6");
        // A too-open directory we own is tightened to 0700.
        let open = root.join("open");
        std::fs::create_dir_all(&open).expect("mkdir");
        std::fs::set_permissions(&open, std::fs::Permissions::from_mode(0o777)).expect("chmod");
        LiveRegistry::at(open.clone())
            .write(&entry("s_l6"))
            .expect("write");
        assert_eq!(
            std::fs::metadata(&open).expect("stat").permissions().mode() & 0o777,
            0o700
        );
        // A symlink in place of the directory is refused (not followed).
        let target = root.join("target");
        std::fs::create_dir_all(&target).expect("mkdir");
        let link = root.join("link");
        symlink(&target, &link).expect("symlink");
        assert!(LiveRegistry::at(link.clone())
            .write(&entry("s_l6"))
            .is_err());
        assert!(read_entries(&link).is_empty());
        // A regular file is refused.
        let file = root.join("file");
        std::fs::write(&file, b"x").expect("file");
        assert!(LiveRegistry::at(file).write(&entry("s_l6")).is_err());
        // A world-writable directory is not read from.
        let wide = root.join("wide");
        std::fs::create_dir_all(&wide).expect("mkdir");
        std::fs::set_permissions(&wide, std::fs::Permissions::from_mode(0o777)).expect("chmod");
        assert!(verify_private_dir(&wide, false).is_err());
        assert!(read_entries(&wide).is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn remove_is_idempotent_and_only_touches_its_own_session() {
        let dir = scratch("remove");
        let registry = LiveRegistry::at(dir.clone());
        registry.write(&entry("s_a")).expect("write a");
        registry.write(&entry("s_b")).expect("write b");
        registry.remove("s_a");
        registry.remove("s_a");
        let ids: Vec<String> = read_entries(&dir)
            .iter()
            .map(|e| e.doc["session_id"].as_str().unwrap_or_default().to_string())
            .collect();
        assert_eq!(ids, vec!["s_b".to_string()]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn client_identity_parses_json_and_plain_shapes() {
        // The parser is pure: exercise it through the environment-agnostic path.
        let (name, version) = parse_client(r#"{"name":"t3-code","version":"1.2.3"}"#);
        assert_eq!((name.as_str(), version.as_str()), ("t3-code", "1.2.3"));
        let (name, version) = parse_client("my-agent");
        assert_eq!((name.as_str(), version.as_str()), ("my-agent", ""));
        let (name, _) = parse_client("");
        assert_eq!(name, CLI_CLIENT);
    }

    /// A pid that certainly does not exist: spawn `true`, reap it, use its pid.
    fn dead_pid() -> u32 {
        let mut child = std::process::Command::new("/bin/true")
            .spawn()
            .expect("spawn true");
        let pid = child.id();
        let _ = child.wait();
        pid
    }

    /// Same parsing as [`client_from_env`], without touching the environment.
    fn parse_client(raw: &str) -> (String, String) {
        let raw = raw.trim();
        if raw.is_empty() {
            return (CLI_CLIENT.to_string(), String::new());
        }
        if raw.starts_with('{') {
            if let Ok(doc) = serde_json::from_str::<serde_json::Value>(raw) {
                return (
                    doc.get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or(CLI_CLIENT)
                        .to_string(),
                    doc.get("version")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                );
            }
        }
        (raw.to_string(), String::new())
    }

    #[test]
    fn write_creates_missing_parent_dir_with_mode_0700() {
        let base = scratch("nested-parent");
        let parent = base.join("acos");
        let dir = parent.join("live");
        let registry = LiveRegistry::at(dir.clone());
        registry.write(&entry("s_nested")).expect("write nested");
        for path in [&parent, &dir] {
            let mode = std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
            assert_eq!(
                mode,
                0o700,
                "{} created with mode 0{mode:o}, expected 0700",
                path.display()
            );
        }
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn sanitize_device_name_strips_controls_equals_collapses_and_caps() {
        assert_eq!(
            sanitize_device_name("  My\n\x00App=v2\t (test)  ").as_deref(),
            Some("My App v2 (test)")
        );
        assert_eq!(sanitize_device_name("  \n===\t "), None);
        let long = "A".repeat(90);
        let capped = sanitize_device_name(&long).expect("capped");
        assert_eq!(capped.chars().count(), 64);
    }
}
