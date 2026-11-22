//! The guest-visible WebView session socket (WV4, wall 8).
//!
//! `results/handoff/WVB/WIRE.md` §0 is the contract: **one Unix stream socket
//! per session**, created by ACOS at a host path it owns and bind-mounted into
//! the guest at `/run/acos/webview.sock`. The guest provider only ever
//! *connects* (`com.android.webview.chromium.AcosSessionSocket`, the same call
//! a browser makes: `LocalSocket.connect` with `Namespace.FILESYSTEM`); the host
//! broker owns the listening end and the engine behind it.
//!
//! This module is the **guest path ⇄ host path** mapping, and nothing else:
//!
//! | guest | host |
//! |---|---|
//! | `/run/acos` (mount point) | `<dir>` from [`host_dir`] |
//! | `/run/acos/webview.sock` | [`host_socket_path`]`(<dir>)` |
//!
//! `<dir>` is `$ACOS_WEBVIEW_SOCKET_DIR` when a session owner (the daemon, or a
//! test) pinned it, otherwise `<run_root>/wv` — the per-session scratch tree
//! `RunDir` already owns and removes. The **directory** is what gets mounted,
//! not the socket file, so the broker may start listening at any time after the
//! guest exists and the guest still sees it. The mount is read-only: the guest
//! can connect to a socket node it cannot unlink, and the broker (a host
//! process) creates it through the host path.
//!
//! The default subdirectory is `wv`, not `webview`, because `sun_path` is 108
//! bytes and a session run root is already close to it (`<data>/runs/<pid>-<n>`
//! is 88 bytes from this checkout's `.cache/art/data`; the existing logd socket
//! at `<run>/socket/logd` is 100). `<run>/wv/webview.sock` is 104, so the file
//! name the guest needs stays and the four extra bytes fit; [`host_dir`]
//! refuses a longer mapping with a structured error instead of letting the
//! broker's `bind()` or the guest's `connect()` fail with `ENAMETOOLONG`.
//!
//! With no broker there is no listener and the guest's `connect(2)` fails with
//! the kernel's real `ENOENT`/`ECONNREFUSED`; the provider maps that to
//! `ENGINE_UNAVAILABLE` (`AcosSessionSocket.open`). There is no fallback
//! transport here, in the VM or anywhere else — never a fake socket.
//!
//! The socket natives themselves are the image's own
//! `register_android_net_LocalSocketImpl` (libandroid_runtime): the guest's
//! `LocalSocketImpl.create/connectLocal/read/write/...` are AOSP's real
//! `AF_UNIX` implementation with real errno → `IOException` (see
//! `results/handoff/WV4/README.md`).

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use crate::Error;

/// The guest path the provider connects to (`WIRE.md` §0, normative).
pub const GUEST_SOCKET_PATH: &str = "/run/acos/webview.sock";

/// The guest directory that is the mount point for the host session directory.
///
/// It is the guest parent of [`GUEST_SOCKET_PATH`]; the basename there must
/// equal [`SOCKET_FILE_NAME`] or the mapping would not line up.
pub const GUEST_DIR: &str = "/run/acos";

/// The socket file name inside the mounted directory. It **must** be the
/// basename of [`GUEST_SOCKET_PATH`]: the guest sees the mounted directory's
/// entries 1:1.
pub const SOCKET_FILE_NAME: &str = "webview.sock";

/// The default host subdirectory of the run root only when an owner pinned
/// nothing and there is no runtime dir at all (tests). Production sessions use
/// [`runtime_root`] instead (WV5b decision: the socket must be short and it must
/// outlive one run, because the broker is per host).
pub const SOCKET_SUBDIR: &str = "wv";

/// Subdirectory of `$XDG_RUNTIME_DIR` holding every host-side WebView path:
/// `<runtime>/acos-wv/<session-id>/webview.sock` and the shared broker's
/// `<runtime>/acos-wv/broker.sock`. 33 bytes in the common case, well under
/// `sun_path`'s 108, and it disappears with the user's runtime dir.
pub const RUNTIME_SUBDIR: &str = "acos-wv";

/// The host-wide broker endpoint override (the shared daemon).
pub const BROKER_SOCKET_ENV: &str = "ACOS_WEBVIEW_BROKER_SOCKET";

/// A session owner (the daemon, or a test) pins the host directory with this
/// variable so the broker and the guest mount agree. It must be absolute and
/// outside the per-run tree if it has to outlive one run.
pub const HOST_DIR_ENV: &str = "ACOS_WEBVIEW_SOCKET_DIR";

/// `sockaddr_un.sun_path` on Linux: 108 bytes including the trailing NUL.
///
/// The guest side is short by construction (`/run/acos/webview.sock`, 24
/// bytes); the **host** side is whatever directory this session owns, so it is
/// the one that can overflow and turn the broker's `bind()` — or, worse, the
/// guest's `connect()` — into `ENAMETOOLONG`. Refusing here means the launch
/// names the limit instead of the WebView failing with an errno nobody can
/// attribute.
const SUN_PATH_MAX: usize = 107;

/// Resolve the host directory this session maps at [`GUEST_DIR`].
///
/// `$ACOS_WEBVIEW_SOCKET_DIR` wins (absolute only — a relative path would
/// resolve against whatever the launcher's cwd happens to be and the broker
/// would disagree); otherwise `<run_root>/wv`, owned by the run.
///
/// The directory is created (mode `0700`) so the bind mount has a source
/// before the broker exists.
pub fn host_dir(run_root: &Path) -> Result<PathBuf, Error> {
    let dir = match std::env::var_os(HOST_DIR_ENV) {
        Some(raw) => {
            let path = PathBuf::from(&raw);
            if !path.is_absolute() {
                return Err(Error::Layout(format!(
                    "{HOST_DIR_ENV}={} must be an absolute path (the guest mount and the \
                     broker's listener have to name the same directory)",
                    path.display()
                )));
            }
            path
        }
        None => default_session_dir(run_root),
    };
    fs::create_dir_all(&dir)?;
    // The session socket is a way out of the guest: its directory is private to
    // this session's owner, never traversable by another uid.
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))?;
    Ok(dir)
}

/// The runtime dir every host-side WebView path lives in:
/// `$XDG_RUNTIME_DIR/acos-wv`, else `/tmp/acos-wv-<uid>` (the brief's fallback
/// when the session has no runtime dir at all).
pub fn runtime_root() -> PathBuf {
    match std::env::var_os("XDG_RUNTIME_DIR").filter(|v| !v.is_empty()) {
        Some(dir) => PathBuf::from(dir).join(RUNTIME_SUBDIR),
        None => PathBuf::from(format!("/tmp/acos-wv-{}", user_id())),
    }
}

/// The per-session directory a launch creates and a gate owns:
/// `<runtime>/acos-wv/<short session id>`. The id is the run directory's own
/// short name (`<pid>-<n>`), never a full path — `sun_path` is 108 bytes.
pub fn default_session_dir(run_root: &Path) -> PathBuf {
    let id = run_root
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| format!("s{}", std::process::id()));
    runtime_root().join(id)
}

/// The shared broker's endpoint for a session directory: the parent of a
/// per-session directory (`<runtime>/acos-wv/broker.sock`), so two sessions of
/// the same runtime share one broker. `$ACOS_WEBVIEW_BROKER_SOCKET` wins.
pub fn broker_socket(session_dir: &Path) -> PathBuf {
    if let Some(raw) = std::env::var_os(BROKER_SOCKET_ENV) {
        if !raw.is_empty() {
            return PathBuf::from(raw);
        }
    }
    match session_dir.parent() {
        Some(parent) => parent.join("broker.sock"),
        None => runtime_root().join("broker.sock"),
    }
}

fn user_id() -> u32 {
    // SAFETY: `getuid` is a pure query.
    unsafe { libc::getuid() }
}

/// Start this session's lazy WebView gate (WV5b), or `Ok(None)` when it is
/// disabled.
///
/// **Nothing is started here but a listener thread**: the shared broker (and
/// therefore Chromium) only exists after the guest connects to the socket, which
/// the provider does when — and only when — it creates a `WebView`. A session
/// that never touches WebView pays for a socket node and a sleeping thread.
///
/// `$ACOS_WEBVIEW_GATE=0` disables the gate entirely (the measurement's control
/// arm; with it off a WebView fails exactly as it did before this module).
pub fn start_gate(
    session_dir: &Path,
    run_root: &Path,
) -> Result<Option<acos_webview::Gate>, Error> {
    if matches!(
        std::env::var("ACOS_WEBVIEW_GATE").as_deref(),
        Ok("0") | Ok("off") | Ok("false")
    ) {
        return Ok(None);
    }
    let session_id = session_dir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| format!("s{}", std::process::id()));
    let broker_socket = broker_socket(session_dir);
    let broker = acos_webview::BrokerSource::from_env(
        std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(Path::to_path_buf))
            .as_deref(),
    );
    let gate = acos_webview::Gate::start(acos_webview::GateConfig {
        session_dir: session_dir.to_path_buf(),
        session_id,
        broker_socket,
        broker,
        engine_cache: None,
        idle_shutdown: None,
        log: Some(
            session_dir
                .parent()
                .unwrap_or(session_dir)
                .join("broker.log"),
        ),
        spawn_wait: acos_webview::gate::SPAWN_WAIT,
    })
    .map_err(|e| Error::Layout(format!("WebView gate at {}: {e}", session_dir.display())))?;
    // The run root is only used to keep the note honest in tests; the gate
    // itself owns the session directory.
    let _ = run_root;
    Ok(Some(gate))
}

/// The host path of the session socket inside `dir` — the file
/// [`GUEST_SOCKET_PATH`] names once `dir` is mounted at [`GUEST_DIR`].
pub fn host_socket_path(dir: &Path) -> PathBuf {
    dir.join(SOCKET_FILE_NAME)
}

/// Refuse a mapping whose host path cannot be a `sockaddr_un`.
///
/// The launch calls this before the guest exists: a too-deep session directory
/// would otherwise surface as `ENAMETOOLONG` from the *broker's* `bind()` or,
/// worse, as an unattributable failed `connect()` inside the WebView. Pure
/// mapping helpers stay unmounted-path-agnostic (unit tests build argv without
/// ever binding), so this is a separate, explicit step.
pub fn check_sun_path(dir: &Path) -> Result<(), Error> {
    let socket = host_socket_path(dir);
    let len = socket.as_os_str().as_encoded_bytes().len();
    if len > SUN_PATH_MAX {
        return Err(Error::Layout(format!(
            "webview socket host path {} is {len} bytes, over the {SUN_PATH_MAX}-byte \
             sockaddr_un limit; give the session a shorter {HOST_DIR_ENV}",
            socket.display()
        )));
    }
    Ok(())
}

/// Whether `guest` is the path this session maps (the provider's constant).
pub fn is_guest_socket_path(guest: &str) -> bool {
    guest == GUEST_SOCKET_PATH
}

fn parse_session_dir_pid(name: &str) -> Option<u32> {
    let (pid_str, seq_str) = name.split_once('-')?;
    if pid_str.is_empty()
        || seq_str.is_empty()
        || !pid_str.bytes().all(|b| b.is_ascii_digit())
        || !seq_str.bytes().all(|b| b.is_ascii_digit())
    {
        return None;
    }
    let pid: u32 = pid_str.parse().ok()?;
    (pid > 0).then_some(pid)
}

fn session_pid_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    if pid == std::process::id() {
        return true;
    }
    if let Ok(stat) = fs::read_to_string(format!("/proc/{pid}/stat")) {
        if let Some((_, rest)) = stat.rsplit_once(')') {
            let state = rest.trim_start().chars().next();
            return !matches!(state, Some('Z' | 'X'));
        }
        return true;
    }
    // SAFETY: signal 0 is a pure existence probe for `pid`.
    let rc = unsafe { libc::kill(pid as i32, 0) };
    if rc == 0 {
        true
    } else {
        std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
    }
}

/// Remove the default per-session WebView directory (`<runtime>/acos-wv/<pid>-<n>`)
/// when a `RunDir` drops (covers `webview: off` and non-gate sessions).
pub fn cleanup_session_dir(run_root: &Path) {
    if std::env::var_os(HOST_DIR_ENV).is_some() {
        return;
    }
    let dir = default_session_dir(run_root);
    let _ = fs::remove_file(host_socket_path(&dir));
    let _ = fs::remove_dir(&dir);
}

/// Sweep stale `<pid>-<n>` per-session directories under `root` (typically
/// [`runtime_root`]) whose owner PID has exited.
///
/// Leaves `broker.sock`, `broker-*.sock`, `broker.log`, live-PID session dirs,
/// and any unrelated files/dirs untouched.
pub fn sweep_stale_session_dirs(root: &Path) -> usize {
    let Ok(rd) = fs::read_dir(root) else {
        return 0;
    };
    let mut removed = 0;
    for entry in rd.flatten() {
        if !entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        let name = entry.file_name();
        let Some(name_str) = name.to_str() else {
            continue;
        };
        let Some(pid) = parse_session_dir_pid(name_str) else {
            continue;
        };
        if session_pid_alive(pid) {
            continue;
        }
        let path = entry.path();
        let _ = fs::remove_file(host_socket_path(&path));
        if fs::remove_dir(&path).is_ok() {
            removed += 1;
        }
    }
    removed
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::sync::Mutex;

    /// The env var is process-global; tests that change it take this lock.
    static ENV: Mutex<()> = Mutex::new(());

    fn scratch(tag: &str) -> PathBuf {
        // Short on purpose: the socket lives in `<run>/webview/webview.sock` and
        // `sun_path` is 108 bytes (`ArtRoot::new_run_dir` keeps run ids short
        // for the same reason).
        let base = crate::repo_root()
            .map(|r| r.join(".cache/wv4"))
            .unwrap_or_else(|_| std::env::temp_dir().join("acos-wv4"));
        let dir = base.join(format!("{}-{tag}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn with_env_dir<T>(value: Option<&Path>, f: impl FnOnce() -> T) -> T {
        let _guard = ENV.lock().unwrap_or_else(|e| e.into_inner());
        let previous = std::env::var_os(HOST_DIR_ENV);
        match value {
            Some(p) => std::env::set_var(HOST_DIR_ENV, p),
            None => std::env::remove_var(HOST_DIR_ENV),
        }
        let out = f();
        match previous {
            Some(p) => std::env::set_var(HOST_DIR_ENV, p),
            None => std::env::remove_var(HOST_DIR_ENV),
        }
        out
    }

    /// The guest path the provider hard-codes decomposes into exactly the mount
    /// point this module binds and the file it expects inside it.
    #[test]
    fn guest_path_is_the_mount_point_plus_the_socket_file() {
        let (dir, file) = GUEST_SOCKET_PATH
            .rsplit_once('/')
            .expect("guest socket path has a parent");
        assert_eq!(file, SOCKET_FILE_NAME);
        assert_eq!(dir, GUEST_DIR);
        assert!(is_guest_socket_path(GUEST_SOCKET_PATH));
        assert!(!is_guest_socket_path("/run/acos/other.sock"));
        assert!(!is_guest_socket_path("/data/local/tmp/webview.sock"));
    }

    /// Default mapping (WV5b): a **short** per-session directory under the
    /// runtime dir, not inside the (deep) run tree — `sun_path` is 108 bytes
    /// and the broker endpoint is the runtime dir, one per host.
    #[test]
    fn default_host_dir_is_the_short_runtime_session_directory() {
        let run = scratch("default");
        with_env_dir(None, || {
            let dir = host_dir(&run).expect("host dir");
            assert_eq!(dir, default_session_dir(&run));
            assert_eq!(
                dir.parent(),
                Some(runtime_root().as_path()),
                "the session dir is a child of the host runtime dir"
            );
            assert!(dir.is_dir(), "the mount source must exist");
            let mode = fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o700, "session socket dir must stay private");
            assert_eq!(host_socket_path(&dir), dir.join(SOCKET_FILE_NAME));
            check_sun_path(&dir).expect("the default mapping must fit sun_path");
            // One broker per host: two sessions of the same runtime dir name the
            // same endpoint, which is a sibling of both session directories.
            let other = default_session_dir(Path::new("/somewhere/20842-2"));
            assert_eq!(broker_socket(&dir), broker_socket(&other));
            assert_eq!(broker_socket(&dir), runtime_root().join("broker.sock"));
        });
        let _ = fs::remove_dir_all(&run);
    }

    /// The whole host path budget of the default mapping, asserted on the real
    /// runtime dir: `<runtime>/acos-wv/<id>/webview.sock` + a `broker.sock`
    /// sibling both fit `sun_path` with room to spare.
    #[test]
    fn default_paths_fit_the_sockaddr_un_budget() {
        let dir = default_session_dir(Path::new("/tmp/art/runs/4242-1"));
        let socket = host_socket_path(&dir);
        let bytes = socket.as_os_str().as_encoded_bytes().len();
        assert!(
            bytes <= SUN_PATH_MAX,
            "default session socket {} is {bytes} bytes",
            socket.display()
        );
        let broker = broker_socket(&dir);
        let bbytes = broker.as_os_str().as_encoded_bytes().len();
        assert!(bbytes <= SUN_PATH_MAX, "broker endpoint is {bbytes} bytes");
        assert_eq!(dir.file_name().unwrap(), "4242-1");
    }

    /// A session owner can pin the directory (the broker and the mount then
    /// agree); a relative value is refused instead of silently resolving
    /// against the launcher's cwd.
    #[test]
    fn env_override_wins_and_must_be_absolute() {
        let run = scratch("env");
        let pinned = run.join("s");
        with_env_dir(Some(&pinned), || {
            assert_eq!(host_dir(&run).unwrap(), pinned);
            assert!(pinned.is_dir());
        });
        with_env_dir(Some(Path::new("relative/webview")), || {
            let e = host_dir(&run).expect_err("a relative override must be refused");
            assert!(
                e.to_string().contains(HOST_DIR_ENV),
                "the error has to name the variable: {e}"
            );
        });
        let long = run.join("a".repeat(SUN_PATH_MAX));
        with_env_dir(Some(&long), || {
            let dir = host_dir(&run).expect("mapping");
            let e = check_sun_path(&dir).expect_err("a too-long host path must be refused");
            let text = e.to_string();
            assert!(
                text.contains("sockaddr_un") && text.contains(HOST_DIR_ENV),
                "the error has to name the limit and the seam: {text}"
            );
            let _ = fs::remove_dir_all(&dir);
        });
        let _ = fs::remove_dir_all(&run);
    }

    /// The acceptance test for the mapping: a **real** listener at the mapped
    /// host path receives a **real** `AF_UNIX` connect (the same `connect(2)`
    /// the guest's `LocalSocketImpl.connectLocal` makes on
    /// `GUEST_SOCKET_PATH` after the mount), and framed bytes round-trip.
    ///
    /// The WIRE frame is `uint32_be length, body` with `body[0] = 1` (JSON) —
    /// enough of `WIRE.md` §1 to prove the stream is byte-transparent.
    #[test]
    fn listener_at_the_mapped_path_receives_the_connect_and_bytes_round_trip() {
        let run = scratch("roundtrip");
        let dir = with_env_dir(None, || host_dir(&run)).expect("host dir");
        let host_socket = host_socket_path(&dir);
        let listener = UnixListener::bind(&host_socket).expect("bind the session socket");

        let server = std::thread::spawn(move || {
            let (mut stream, _addr) = listener.accept().expect("accept the guest connect");
            let mut len = [0u8; 4];
            stream.read_exact(&mut len).expect("frame length");
            let mut body = vec![0u8; u32::from_be_bytes(len) as usize];
            stream.read_exact(&mut body).expect("frame body");
            // Answer with one JSON frame, the way the broker answers `hello`.
            let reply = br#"{"id":"g1","ok":true,"result":{"proto":1,"session":"s0"}}"#;
            let mut frame = Vec::with_capacity(4 + 1 + reply.len());
            frame.extend_from_slice(&((1 + reply.len()) as u32).to_be_bytes());
            frame.push(1);
            frame.extend_from_slice(reply);
            stream.write_all(&frame).expect("write the reply");
            stream.flush().expect("flush");
            body
        });

        // The client side is the guest's side: connect to the path the mount
        // makes visible at `GUEST_SOCKET_PATH`.
        let mut client = UnixStream::connect(&host_socket).expect("connect the session socket");
        let request = br#"{"id":"g1","op":"hello","proto":1}"#;
        let mut frame = Vec::with_capacity(4 + 1 + request.len());
        frame.extend_from_slice(&((1 + request.len()) as u32).to_be_bytes());
        frame.push(1);
        frame.extend_from_slice(request);
        client.write_all(&frame).expect("write hello");
        client.flush().expect("flush");

        let mut len = [0u8; 4];
        client.read_exact(&mut len).expect("reply length");
        let mut body = vec![0u8; u32::from_be_bytes(len) as usize];
        client.read_exact(&mut body).expect("reply body");
        assert_eq!(body[0], 1, "kind 1 = JSON");
        let reply = std::str::from_utf8(&body[1..]).expect("UTF-8 JSON");
        assert!(reply.contains("\"ok\":true"), "reply: {reply}");
        assert!(reply.contains("\"session\":\"s0\""), "reply: {reply}");

        let received = server.join().expect("server thread");
        assert_eq!(received[0], 1, "kind 1 = JSON");
        assert_eq!(
            std::str::from_utf8(&received[1..]).expect("UTF-8 JSON"),
            std::str::from_utf8(request).unwrap(),
            "the guest's hello must arrive byte-identical"
        );

        let _ = fs::remove_dir_all(&run);
    }

    /// No broker ⇒ no listener ⇒ the client sees the kernel's real `ENOENT`
    /// (never a fabricated connection). This is what the ART run reports until
    /// the host broker lands.
    #[test]
    fn without_a_broker_the_connect_fails_with_the_kernels_enoent() {
        let run = scratch("no-broker");
        let dir = with_env_dir(None, || host_dir(&run)).expect("host dir");
        let host_socket = host_socket_path(&dir);
        let err = UnixStream::connect(&host_socket).expect_err("nothing is listening");
        assert_eq!(
            err.kind(),
            std::io::ErrorKind::NotFound,
            "no socket file yet means ENOENT (the honest ENGINE_UNAVAILABLE): {err}"
        );
        assert_eq!(err.raw_os_error(), Some(libc::ENOENT));

        // A *stale* socket node (the broker died) is ECONNREFUSED, not ENOENT:
        // the session exposes exactly the kernel's answer, both ways.
        let listener = UnixListener::bind(&host_socket).expect("bind");
        drop(listener);
        assert!(
            host_socket.exists(),
            "the socket node survives its listener"
        );
        let err = UnixStream::connect(&host_socket).expect_err("the listener is gone");
        assert_eq!(err.raw_os_error(), Some(libc::ECONNREFUSED));

        let _ = fs::remove_dir_all(&run);
    }

    #[test]
    fn sweep_stale_session_dirs_removes_dead_pid_dirs_only() {
        let root = scratch("sweep");
        let dead = std::process::Command::new("/bin/true")
            .status()
            .expect("spawn /bin/true");
        assert!(dead.success());
        // Spawn and wait to obtain a genuinely exited PID.
        let child = std::process::Command::new("/bin/true")
            .spawn()
            .expect("spawn child");
        let dead_pid = child.id();
        let mut child = child;
        let _ = child.wait();

        let stale_dir = root.join(format!("{dead_pid}-1"));
        fs::create_dir_all(&stale_dir).expect("stale dir");
        fs::write(host_socket_path(&stale_dir), b"").expect("stale sock");

        let live_dir = root.join(format!("{}-1", std::process::id()));
        fs::create_dir_all(&live_dir).expect("live dir");
        fs::write(host_socket_path(&live_dir), b"").expect("live sock");

        let unrelated_dir = root.join("notes");
        fs::create_dir_all(&unrelated_dir).expect("unrelated dir");
        fs::write(root.join("broker.sock"), b"").expect("broker sock");

        let removed = sweep_stale_session_dirs(&root);
        assert_eq!(removed, 1, "only dead-pid session dir should be removed");
        assert!(!stale_dir.exists(), "dead-pid session dir must be removed");
        assert!(live_dir.exists(), "live-pid session dir must survive");
        assert!(unrelated_dir.exists(), "non-session dir must survive");
        assert!(root.join("broker.sock").exists(), "broker.sock must survive");

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn cleanup_session_dir_removes_default_dir_when_unpinned() {
        let run = scratch("cleanup");
        with_env_dir(None, || {
            let dir = host_dir(&run).expect("host dir");
            assert!(dir.is_dir());
            cleanup_session_dir(&run);
            assert!(!dir.exists(), "cleanup_session_dir must remove empty default dir");
        });
        let _ = fs::remove_dir_all(&run);
    }
}

