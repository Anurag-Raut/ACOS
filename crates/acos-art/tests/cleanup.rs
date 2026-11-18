//! Cleanup and limit proof for the real launcher (`acos-art` as a black box).
//!
//! Three separate incidents motivate this suite:
//!
//! * the 2026-11-11 OOM (`results/art-runtime/R1.md`) — the launcher's *own*
//!   RSS grew unbounded off a guest log loop until the kernel killed the desktop
//!   session, so the fix must be visible from outside the process;
//! * `results/art-runtime/S3.md` — a launcher that dies must not leave the guest
//!   behind, which is what bwrap's `--die-with-parent` is for, but "the flag is
//!   in the argv" is not the same claim as "the guest is gone in 3 s";
//! * the everyday one nobody writes down — the run ends, and the host keeps the
//!   processes, the listening socket, the memfds or the scratch directory.
//!
//! Every assertion here is made from *outside* the launcher (a full `/proc`
//! scan, `/proc/<pid>/status:VmHWM`, a datagram flood into the real logd sink),
//! so the tests cannot be satisfied by an internal counter that drifted from
//! behaviour. `tools/art-runtime/leakcheck.sh` is the shell twin of the process
//! scan and is the tool to run around the whole suite.
//!
//! **Ownership (W56).** Every scan, assertion and kill is scoped to the
//! processes *this test* started — see [`Scope`]. The suite used to compare
//! host-wide `/proc` snapshots, so on a shared host it counted another agent's
//! live ART run as a leak (W53 §2.3) and `reap`ed it: 12 benchmark guests of
//! W52's N=10 round r2 died from signal 9 inside one 3 ms window
//! (`results/art-runtime/W52.md` §4). Ownership is now derived from ancestry of
//! the launcher pid this test spawned plus this test's private data root, and
//! the kill path re-derives it rather than trusting the caller.
//!
//! The suite **fails** when the ART sysroot or the cached guest dex is absent
//! (HYG2): those are 1 GB+ host artefacts produced by `tools/art-sysroot.sh` /
//! one warm `acos-art render-app`, not something a `cargo test` may download or
//! build, but a test that asserts nothing must not report a pass. The one
//! counted-skip path is `ACOS_ALLOW_ART_SKIP=1`, which prints the reason and a
//! `SKIP SUMMARY` count at exit; `tools/art-runtime/test-crates.sh` exports the
//! warm data inside the lock so the default path runs these cases.
//!
//! ART is started through `tools/guarded.sh` by the acceptance command; inside
//! the test, runs are serialised on one mutex so a suite run never has two
//! guests alive at once (the owner's host-safety rule).

use std::collections::BTreeSet;
use std::io::Write;
use std::os::unix::net::{UnixDatagram, UnixStream};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// Serialises ART-spawning tests: only one guest at a time on this host.
static ART_LOCK: Mutex<()> = Mutex::new(());

fn art_guard() -> MutexGuard<'static, ()> {
    ART_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// Process names an ART run may leave behind, matched on `comm` **or** cmdline.
const PATTERNS: &[&str] = &["dalvikvm64", "bwrap", "acos-art", "SessionMain"];

/// The launcher ceiling this suite proves for the log-spam scenario.
const LAUNCHER_PEAK_LIMIT_MB: f64 = 256.0;

// ---------------------------------------------------------------------------
// Repository / fixture layout
// ---------------------------------------------------------------------------

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("the crate lives at <repo>/crates/acos-art")
}

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_acos-art")
}

fn sysroot() -> PathBuf {
    acos_test_support::art_sysroot(&repo_root())
}

fn warm_data() -> PathBuf {
    acos_test_support::art_warm_data(&repo_root())
}

/// `None` = the host lacks the (large, out-of-repo-build) ART inputs. HYG2: that
/// is a **failure** naming the missing artefact, not a silent skip — a `cargo
/// test` that reports `ok` while every ART case skipped is the false-green this
/// helper replaced (BACKSETTLE README §6, `suite3.log`).
fn require_art() -> Option<()> {
    acos_test_support::require_art(&repo_root(), "cleanup test").map(|_| ())
}

/// The root every test's scratch dir lives under.
///
/// The scratch dir holds copy of `/data`, and a run root below it binds the
/// logd/WebView sockets (`<run>/socket/logdw`), so the checkout path decides
/// whether `bind()` fits `sun_path` (108 bytes with the NUL). An agent worktree
/// is ~55 bytes, which left 113 for the deepest socket and failed the suite with
/// "path must be shorter than SUN_LEN" instead of testing cleanup. One rule for
/// that class (the same one `acos-cli`/`live_view` use): the checkout's
/// `.cache/h1-tests` when it fits, a short fixed root otherwise; `$ACOS_CLEANUP_TEST_DIR`
/// overrides when it fits (see `acos_test_support::short_scratch_root`).
fn scratch_root() -> PathBuf {
    let candidates = vec![repo_root().join(".cache/h1-tests")];
    acos_test_support::short_scratch_root(
        "ACOS_CLEANUP_TEST_DIR",
        &candidates,
        "acos-cleanup-tests",
    )
}

/// The deepest socket these tests make the launcher bind below the scratch
/// root: `<root>/<tag>-<pid>/data/runs/<pid>-<seq>/socket/logdw`. The longest
/// tag used here is `scope-foreign`, and the seq is a small counter.
fn deepest_socket(root: &Path) -> PathBuf {
    root.join(format!("scope-foreign-{}", std::process::id()))
        .join("data/runs")
        .join(format!("{}-999", std::process::id()))
        .join("socket/logdw")
}

/// A scratch dir under [`scratch_root`], fresh per test.
fn scratch(tag: &str) -> PathBuf {
    let dir = scratch_root().join(format!("{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create the test's scratch dir");
    dir
}

/// A private copy of the writable `/data` tree, so a test can never race another
/// agent's run and its leftovers are removed with the scratch dir.
fn seed_data(scratch: &Path) -> PathBuf {
    let data = scratch.join("data");
    let warm = warm_data();
    let st = Command::new("cp")
        .arg("-a")
        .arg(&warm)
        .arg(&data)
        .status()
        .expect("cp is available");
    assert!(st.success(), "could not seed /data from {}", warm.display());
    data
}

fn apk() -> PathBuf {
    repo_root().join("fixtures/build/apps/counter.apk")
}

/// The scratch root must leave room for the deepest socket the launcher binds,
/// or the suite fails on `SUN_LEN` for a reason that has nothing to do with
/// cleanup (see [`scratch_root`]).
#[test]
fn the_scratch_root_leaves_room_for_the_run_sockets() {
    let deepest = deepest_socket(&scratch_root());
    let len = deepest.as_os_str().as_encoded_bytes().len();
    assert!(
        len <= acos_test_support::SUN_PATH_MAX,
        "{} is {len} bytes, too long for sun_path ({} with the NUL)",
        deepest.display(),
        acos_test_support::SUN_PATH_MAX + 1
    );
}

// ---------------------------------------------------------------------------
// /proc scanning: processes, RSS, survivors
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
struct Proc {
    pid: u32,
    /// `/proc/<pid>/stat:starttime` — makes identity immune to pid reuse.
    start: u64,
    ppid: u32,
    state: char,
    comm: String,
    cmd: String,
    /// `/proc/<pid>/cwd` (the path as the process's own mount namespace sees
    /// it). A process of this test's run always names its private root here or
    /// in `cmd`; a foreign ART run never does.
    cwd: String,
}

impl Proc {
    fn key(&self) -> (u32, u64) {
        (self.pid, self.start)
    }
    /// A zombie holds no memory, no socket and no memfd; init reaps it. It is
    /// reported, but it does not make a run "leaking a process".
    fn is_zombie(&self) -> bool {
        self.state == 'Z'
    }
    fn is_guest(&self) -> bool {
        self.comm.contains("dalvikvm64") || self.cmd.contains("dalvikvm64")
    }
}

fn read_cmdline(dir: &Path) -> String {
    std::fs::read(dir.join("cmdline"))
        .map(|b| String::from_utf8_lossy(&b).replace('\0', " "))
        .unwrap_or_default()
        .trim()
        .to_string()
}

fn read_cwd(dir: &Path) -> String {
    std::fs::read_link(dir.join("cwd"))
        .map(|p| p.display().to_string())
        .unwrap_or_default()
}

/// `/proc/<pid>/stat` → (ppid, starttime). `None` once the process is gone.
fn proc_ppid_start(pid: u32) -> Option<(u32, u64)> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let rest = stat.get(stat.rfind(')')? + 2..)?;
    let fields: Vec<&str> = rest.split_whitespace().collect();
    if fields.len() < 20 {
        return None;
    }
    Some((fields[1].parse().ok()?, fields[19].parse().ok()?))
}

fn scan_procs() -> Vec<Proc> {
    let root = repo_root();
    let root_s = root.to_string_lossy().to_string();
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir("/proc") else {
        return out;
    };
    for entry in rd.flatten() {
        let name = entry.file_name();
        let Some(pid) = name.to_str().and_then(|s| s.parse::<u32>().ok()) else {
            continue;
        };
        let dir = entry.path();
        let (Ok(comm), Ok(stat)) = (
            std::fs::read_to_string(dir.join("comm")),
            std::fs::read_to_string(dir.join("stat")),
        ) else {
            continue;
        };
        let comm = comm.trim().to_string();
        // Fields after "(comm)": 0 state, 1 ppid, … 19 starttime.
        let Some(rest) = stat.rfind(')').and_then(|i| stat.get(i + 2..)) else {
            continue;
        };
        let fields: Vec<&str> = rest.split_whitespace().collect();
        if fields.len() < 20 {
            continue;
        }
        let Ok(start) = fields[19].parse::<u64>() else {
            continue;
        };
        let cmd = read_cmdline(&dir);
        let cwd = read_cwd(&dir);
        // Match the process NAME or the command line. W53 narrowed this to
        // `comm` alone because the scan was host-wide and `cargo build -p
        // acos-art` (or another agent's `bash -c … acos-art`) matched the old
        // cmdline test. That narrowing lost a real guest: the `launch`/`app`
        // route makes ART rename itself to the *package* (`comm` =
        // `fixtures.counter`) while its `cmd` still names
        // `/system/apex/com.android.art/bin/dalvikvm64`, so a leaked launch guest
        // was invisible to the scan. Ownership is now decided per test ([`Scope`]),
        // so a foreign build command cannot be mistaken for a leak and the
        // broader match is safe again.
        if !PATTERNS.iter().any(|p| comm.contains(p) || cmd.contains(p)) {
            continue;
        }
        // Every desktop has flatpak/snap bwrap wrappers coming and going; an ART
        // bwrap always names a path inside this repo (sysroot, data, shim).
        if comm == "bwrap" && !cmd.contains(&root_s) {
            continue;
        }
        out.push(Proc {
            pid,
            start,
            ppid: fields[1].parse().unwrap_or(0),
            state: fields[0].chars().next().unwrap_or('?'),
            comm,
            cmd,
            cwd,
        });
    }
    out
}

/// Host-wide key set, taken **before** a test spawns anything. It is used for
/// reporting only: "these ART-looking processes appeared while the test ran and
/// are not ours" is the diagnostic that explains a shared host, and it must
/// never feed a kill (W52 r2).
fn snapshot() -> BTreeSet<(u32, u64)> {
    scan_procs().into_iter().map(|p| p.key()).collect()
}

/// Widens [`Scope::procs`] back to the old host-wide predicate. Evidence A/B
/// only (`ACOS_CLEANUP_OWNERSHIP=host`, W56 §3): the kill path never honours it,
/// so even a diagnostic run cannot signal another agent's guest.
fn host_wide_diagnostic() -> bool {
    std::env::var_os("ACOS_CLEANUP_OWNERSHIP")
        .map(|v| v == "host")
        .unwrap_or(false)
}

/// The processes **this test** started, and nothing else.
///
/// Two independent pieces of evidence, because neither alone is sufficient:
///
/// * **ancestry** — a process whose `ppid` chain reaches a pid this test
///   spawned (the launcher). `/proc/<pid>/stat:ppid` is reported in the
///   *reader's* pid namespace, so bwrap's `--unshare-pid` (the guest is bwrap's
///   child) and a pid namespace in between do not break the link.
/// * **markers** — this test's private data root and scratch dir appearing in
///   `cmdline` or `cwd`. This is what keeps a **real leak** visible: when the
///   launcher dies, bwrap is reparented and ancestry is gone, but its argv still
///   binds `<data>/system` and the guest is still bwrap's child, so the leaked
///   pair stays owned. `launch_hang.rs` already scopes its scan this way.
///
/// A process that started before the earliest registered root cannot be its
/// descendant, which removes pid reuse from the predicate.
struct Scope {
    /// `(pid, starttime)` of processes this test spawned directly.
    roots: Vec<(u32, u64)>,
    /// Absolute paths only this test's run can name.
    markers: Vec<String>,
}

impl Scope {
    fn new() -> Self {
        Scope {
            roots: Vec::new(),
            markers: Vec::new(),
        }
    }

    /// Register this test's private run directories (the seeded `/data` copy and
    /// the scratch dir). Call before spawning: the launcher names them and bwrap
    /// binds them, so they identify the run from its first millisecond.
    fn own_run(&mut self, scratch: &Path, data: &Path) {
        for p in [scratch, data] {
            let s = p.display().to_string();
            if !s.is_empty() && !self.markers.contains(&s) {
                self.markers.push(s);
            }
        }
    }

    /// Register a process this test spawned. Call immediately after `spawn`:
    /// the identity is `(pid, starttime)`, so a later pid reuse cannot inherit
    /// this test's ownership.
    fn add_root(&mut self, pid: u32) {
        let start = proc_ppid_start(pid).map(|(_, start)| start).unwrap_or(0);
        self.roots.push((pid, start));
    }

    /// Live processes this test owns, from a fresh `/proc` scan. `ACOS_CLEANUP_
    /// OWNERSHIP=host` widens *detection* to the old host-wide set (diagnostics
    /// only; [`Scope::reap`] stays strict).
    fn procs(&self) -> Vec<Proc> {
        if host_wide_diagnostic() {
            let mut all: Vec<Proc> = scan_procs()
                .into_iter()
                .filter(|p| !p.is_zombie())
                .collect();
            all.sort_by_key(|p| p.pid);
            return all;
        }
        self.owned()
    }

    /// The strict ownership closure (never widened by the diagnostic knob).
    fn owned(&self) -> Vec<Proc> {
        let all: Vec<Proc> = scan_procs()
            .into_iter()
            .filter(|p| !p.is_zombie())
            .collect();
        let root_pids: BTreeSet<u32> = self.roots.iter().map(|(pid, _)| *pid).collect();
        let min_start = self
            .roots
            .iter()
            .map(|(_, s)| *s)
            .filter(|s| *s > 0)
            .min()
            .unwrap_or(0);
        let mut owned_pids: BTreeSet<u32> = BTreeSet::new();
        let mut out: Vec<Proc> = Vec::new();
        // A process can only be classified once its parent is: the scan order is
        // arbitrary, so iterate to a fixpoint (bounded by the process count).
        loop {
            let mut grew = false;
            for p in &all {
                if owned_pids.contains(&p.pid) {
                    continue;
                }
                if min_start > 0 && p.start < min_start {
                    continue;
                }
                let by_marker = self
                    .markers
                    .iter()
                    .any(|m| p.cmd.contains(m) || p.cwd.contains(m));
                if root_pids.contains(&p.pid) || by_marker || owned_pids.contains(&p.ppid) {
                    owned_pids.insert(p.pid);
                    out.push(p.clone());
                    grew = true;
                }
            }
            if !grew {
                break;
            }
        }
        out.sort_by_key(|p| p.pid);
        out
    }

    /// ART-looking processes that appeared while the test ran but are **not**
    /// its own: another agent's concurrent run, or the suite wrapper. Reported,
    /// never asserted on, never signalled.
    fn foreign(&self, before: &BTreeSet<(u32, u64)>) -> Vec<Proc> {
        let mine: BTreeSet<u32> = self.owned().into_iter().map(|p| p.pid).collect();
        let mut v: Vec<Proc> = scan_procs()
            .into_iter()
            .filter(|p| !p.is_zombie() && !before.contains(&p.key()) && !mine.contains(&p.pid))
            .collect();
        v.sort_by_key(|p| p.pid);
        v
    }

    /// Poll until no process of *this test's* run is alive, or `within` elapses.
    /// Returns the survivors (empty = clean).
    fn wait_until_clean(&self, within: Duration) -> Vec<Proc> {
        let deadline = Instant::now() + within;
        loop {
            let survivors = self.procs();
            if survivors.is_empty() || Instant::now() >= deadline {
                return survivors;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// Best-effort cleanup so a failing assertion cannot itself leak a guest.
    ///
    /// Ownership is **re-derived here** instead of trusted from the caller: a
    /// scan bug cannot reach `kill` for a process that is not this test's. This
    /// is the second layer that makes the W52 r2 sweep impossible.
    fn reap(&self, procs: &[Proc]) {
        let owned: BTreeSet<u32> = self.owned().into_iter().map(|p| p.pid).collect();
        for p in procs {
            if !owned.contains(&p.pid) {
                eprintln!(
                    "cleanup: NOT killing pid={} comm={} — it is not this test's process \
                     (a concurrent agent's run survives)",
                    p.pid, p.comm
                );
                continue;
            }
            unsafe {
                libc::kill(p.pid as i32, libc::SIGKILL);
            }
        }
    }
}

fn describe(procs: &[Proc]) -> String {
    procs
        .iter()
        .map(|p| {
            format!(
                "\n  pid={} ppid={} state={} comm={} cmd={}",
                p.pid, p.ppid, p.state, p.comm, p.cmd
            )
        })
        .collect()
}

/// `/proc/<pid>/status:<field>` in KiB (e.g. `VmHWM` = peak RSS).
fn status_kb(pid: u32, field: &str) -> Option<u64> {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    status.lines().find_map(|l| {
        l.strip_prefix(field)
            .and_then(|rest| rest.trim_start_matches(':').split_whitespace().next())
            .and_then(|kb| kb.parse().ok())
    })
}

/// Kills the child on drop, so a panicking test never leaves a guest behind.
struct Reaper(Option<Child>);

impl Reaper {
    fn spawn(cmd: &mut Command, what: &str) -> Reaper {
        Reaper(Some(
            cmd.spawn().unwrap_or_else(|e| panic!("spawn {what}: {e}")),
        ))
    }
    fn pid(&self) -> u32 {
        self.child().id()
    }
    fn child(&self) -> &Child {
        self.0.as_ref().expect("the child was already collected")
    }
    fn try_wait(&mut self) -> Option<std::process::ExitStatus> {
        self.0.as_mut()?.try_wait().ok().flatten()
    }
    /// Wait for exit up to `within`, then SIGKILL. Returns whether it exited by
    /// itself.
    fn wait_or_kill(&mut self, within: Duration) -> bool {
        let deadline = Instant::now() + within;
        while Instant::now() < deadline {
            if self.try_wait().is_some() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        if let Some(child) = self.0.as_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }
        false
    }
    /// Block for exit and collect the pipes (the `Command::output` shape, but
    /// with the pid available for [`Scope::add_root`]).
    fn wait_with_output(&mut self) -> std::process::Output {
        let child = self.0.take().expect("the child was already collected");
        child
            .wait_with_output()
            .expect("collect the child's output")
    }
}

impl Drop for Reaper {
    fn drop(&mut self) {
        if let Some(child) = self.0.as_mut() {
            if child.try_wait().ok().flatten().is_none() {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }
}

fn dir_entries(dir: &Path) -> BTreeSet<String> {
    let mut set = BTreeSet::new();
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            set.insert(e.file_name().to_string_lossy().to_string());
        }
    }
    set
}

/// `acos-art <sub> --data <dir> …`: `acos-art` parses its subcommand first, so
/// common options (`--data`) must follow it.
fn launcher(sub: &str, data: &Path) -> Command {
    let mut c = Command::new(bin());
    c.env("ACOS_ART_QUIET_GUARD", "1");
    c.arg(sub).arg("--data").arg(data);
    c.stdin(Stdio::null());
    c
}

// ---------------------------------------------------------------------------
// 1. The normal path ends where it started
// ---------------------------------------------------------------------------

#[test]
fn render_app_leaves_no_processes_or_scratch_behind() {
    let _serial = art_guard();
    let Some(()) = require_art() else { return };
    let scratch = scratch("render");
    let data = seed_data(&scratch);
    let out = scratch.join("out");

    // render-app must not add a single per-session scratch directory: it is the
    // path with no session at all, so anything under `<data>/session` is a leak.
    let sessions_before = dir_entries(&data.join("session"));
    let before = snapshot();
    let mut scope = Scope::new();
    scope.own_run(&scratch, &data);

    let mut cmd = launcher("render-app", &data);
    cmd.arg("--apk")
        .arg(apk())
        .arg("--out")
        .arg(&out)
        .arg("--timeout")
        .arg("120")
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut child = Reaper::spawn(&mut cmd, "acos-art render-app");
    scope.add_root(child.pid());
    assert!(
        child.wait_or_kill(Duration::from_secs(180)),
        "render-app did not finish within 180 s"
    );
    let st = child.try_wait();
    let st = st.unwrap_or_else(|| panic!("render-app left no exit status"));
    assert!(st.success(), "render-app failed: {st:?}");
    let frame = out.join("frame.png");
    let meta = std::fs::metadata(&frame)
        .unwrap_or_else(|e| panic!("no first frame at {}: {e}", frame.display()));
    assert!(meta.len() > 1024, "frame.png is only {} bytes", meta.len());

    let leaked = scope.wait_until_clean(Duration::from_secs(3));
    let detail = describe(&leaked);
    let foreign = describe(&scope.foreign(&before));
    scope.reap(&leaked);
    assert!(
        leaked.is_empty(),
        "render-app left {} of this run's process(es) behind after a successful run:{detail}\n\
         (other processes on this host, not this test's, ignored:{foreign})",
        leaked.len()
    );

    let sessions_after = dir_entries(&data.join("session"));
    let new: Vec<&String> = sessions_after.difference(&sessions_before).collect();
    assert!(
        new.is_empty(),
        "render-app left scratch behind under {}/session: {new:?}",
        data.display()
    );

    // S4: the per-run writable root (dalvik-cache, tmp, out, socket) must be
    // removed on the normal exit path, not merely emptied.
    let runs = dir_entries(&data.join("runs"));
    assert!(
        runs.is_empty(),
        "render-app left per-run data roots behind under {}/runs: {runs:?}",
        data.display()
    );

    let _ = std::fs::remove_dir_all(&scratch);
}

// ---------------------------------------------------------------------------
// 2. SIGKILL the launcher: the guest must die with it (S3 --die-with-parent)
// ---------------------------------------------------------------------------

#[test]
fn sigkill_of_the_launcher_takes_the_guest_and_bwrap_with_it() {
    let _serial = art_guard();
    let Some(()) = require_art() else { return };
    let scratch = scratch("sigkill");
    let data = seed_data(&scratch);
    let sock = scratch.join("session.sock");
    let log = std::fs::File::create(scratch.join("session.log")).expect("session log");

    let before = snapshot();

    let mut cmd = launcher("session", &data);
    cmd.arg("--apk")
        .arg(apk())
        .arg("--out")
        .arg(scratch.join("out"))
        .arg("--socket")
        .arg(&sock)
        .arg("--session-id")
        .arg("h1sigkill")
        .arg("--timeout")
        .arg("120")
        .stdout(Stdio::null())
        .stderr(Stdio::from(log));
    // Own session, so a stray terminal signal or the test harness's group kill
    // cannot mask (or cause) what this test measures.
    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = Reaper::spawn(&mut cmd, "acos-art session");
    let launcher_pid = child.pid();
    let mut scope = Scope::new();
    scope.own_run(&scratch, &data);
    scope.add_root(launcher_pid);

    // Wait for *this run's* real guest to be up. Ownership, not "a new process":
    // a concurrent agent's dalvikvm64 is not this test's guest (W52 r2).
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut guests = Vec::new();
    while Instant::now() < deadline {
        guests = scope.procs().into_iter().filter(Proc::is_guest).collect();
        if !guests.is_empty() {
            break;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    assert!(
        !guests.is_empty(),
        "no dalvikvm64 guest of this run appeared within 60 s; session log at {}\n\
         (other processes on this host, not this test's:{})",
        scratch.join("session.log").display(),
        describe(&scope.foreign(&before))
    );
    // Everything this run created, guest and bwrap alike (never a foreign run's).
    let victims = scope.procs();
    let victim_detail = describe(&victims);

    unsafe {
        libc::kill(launcher_pid as i32, libc::SIGKILL);
    }
    let _ = child.wait_or_kill(Duration::from_secs(10));

    let survivors = scope.wait_until_clean(Duration::from_secs(3));
    let detail = describe(&survivors);
    let foreign = describe(&scope.foreign(&before));
    scope.reap(&survivors);
    assert!(
        survivors.is_empty(),
        "SIGKILL of the launcher (pid {launcher_pid}) left {} process(es) of this run alive \
         after 3 s (S3: --die-with-parent must take the guest with the launcher).\n\
         Started by the run:{victim_detail}\nSurvivors:{detail}\n\
         (other processes on this host, not this test's, ignored:{foreign})",
        survivors.len()
    );

    // A SIGKILLed launcher cannot remove its own per-run root: that is exactly
    // the case the startup janitor exists for (S4). Prove the leftover is there
    // (not silently absent) and that one janitor sweep with a zero TTL removes
    // it while a live owner is untouched.
    let leftovers = dir_entries(&data.join("runs"));
    assert!(
        !leftovers.is_empty(),
        "expected the SIGKILLed run's data root to survive (it is the janitor's job)"
    );
    let jan = launcher("janitor", &data)
        .env("ACOS_ART_RUN_TTL_SECS", "0")
        .output()
        .expect("spawn acos-art janitor");
    assert!(jan.status.success(), "janitor failed: {jan:?}");
    let after = dir_entries(&data.join("runs"));
    assert!(
        after.is_empty(),
        "janitor left {} run dir(s) after a zero-TTL sweep: {after:?}",
        after.len()
    );

    let _ = std::fs::remove_dir_all(&scratch);
}

// ---------------------------------------------------------------------------
// 2b. S4 canary: the guest cannot rewrite the shared read-only inputs
// ---------------------------------------------------------------------------

/// md5 of a file via `md5sum` (the crate's own helper is not exported to tests).
fn md5(path: &Path) -> String {
    let out = Command::new("md5sum")
        .arg(path)
        .output()
        .unwrap_or_else(|e| panic!("md5sum {}: {e}", path.display()));
    assert!(out.status.success(), "md5sum failed for {}", path.display());
    String::from_utf8_lossy(&out.stdout)
        .split_whitespace()
        .next()
        .expect("md5sum output")
        .to_string()
}

fn toolchain_d8() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("D8") {
        return Some(PathBuf::from(p));
    }
    let home = std::env::var_os("HOME").map(PathBuf::from)?;
    let mut roots: Vec<PathBuf> = Vec::new();
    for var in ["ANDROID_HOME", "ANDROID_SDK_ROOT"] {
        if let Some(p) = std::env::var_os(var) {
            roots.push(PathBuf::from(p));
        }
    }
    roots.push(home.join("Android/Sdk"));
    for root in roots {
        let bt = root.join("build-tools");
        let Ok(rd) = std::fs::read_dir(&bt) else {
            continue;
        };
        let mut versions: Vec<PathBuf> = rd.flatten().map(|e| e.path()).collect();
        versions.sort();
        for v in versions.into_iter().rev() {
            let d8 = v.join("d8");
            if d8.is_file() {
                return Some(d8);
            }
        }
    }
    None
}

/// Compile the canary DEX (javac + d8). A host without the Android
/// build-tools is a missing prerequisite (HYG2: a loud failure, or a counted
/// skip under `ACOS_ALLOW_ART_SKIP=1`), never a silent pass.
fn build_canary_dex(scratch: &Path) -> Option<PathBuf> {
    let Some(d8) = toolchain_d8() else {
        acos_test_support::require_prereq(
            false,
            "canary test",
            "no d8 (Android build-tools) on this host — install the Android SDK \
             build-tools, or set ACOS_ALLOW_ART_SKIP=1 for a counted skip",
        )?;
        return None;
    };
    let src = scratch.join("canary/Canary.java");
    std::fs::create_dir_all(src.parent().unwrap()).ok()?;
    let targets = [
        "/data/img/x86_64/boot.art",
        "/data/system/environ/classpath",
        "/data/acos.properties",
        "/data/shim/libartshim.so",
        "/data/app/guest/classes.dex",
        "/linkerconfig/ld.config.txt",
    ];
    let mut java = String::from("import java.io.FileOutputStream;\n\npublic class Canary {\n  public static void main(String[] a) {\n    String[] t = {\n");
    for t in targets {
        java.push_str(&format!("      \"{t}\",\n"));
    }
    java.push_str(
        // Read first (the input must be *visible* to the guest), then try to write
        // (it must be denied). Without the read half, a FileNotFoundException
        // could just mean the path is absent from the sandbox.
        "    };\n    for (String p : t) {\n      try (java.io.FileInputStream in = new java.io.FileInputStream(p)) {\n        in.read();\n        System.out.println(\"READ \" + p);\n      } catch (Exception e) {\n        System.out.println(\"UNREADABLE \" + p + \" \" + e.getClass().getSimpleName());\n      }\n      try (FileOutputStream f = new FileOutputStream(p, true)) {\n        f.write(0);\n        System.out.println(\"WROTE \" + p);\n      } catch (Exception e) {\n        System.out.println(\"DENIED \" + p + \" \" + e.getClass().getSimpleName());\n      }\n    }\n  }\n}\n",
    );
    std::fs::write(&src, java).ok()?;
    let classes = scratch.join("canary/classes");
    let st = Command::new("javac")
        .arg("-d")
        .arg(&classes)
        .arg(&src)
        .status()
        .ok()?;
    if !st.success() {
        acos_test_support::require_prereq(
            false,
            "canary test",
            "javac failed to compile the canary — the JDK on PATH is not usable",
        )?;
        return None;
    }
    let out = scratch.join("canary/dex");
    std::fs::create_dir_all(&out).ok()?;
    // d8 takes program *files* (class files or a jar), not a directory.
    let mut class_files: Vec<PathBuf> = std::fs::read_dir(&classes)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("class"))
        .collect();
    class_files.sort();
    if class_files.is_empty() {
        acos_test_support::require_prereq(
            false,
            "canary test",
            "javac produced no class files",
        )?;
        return None;
    }
    let st = Command::new(&d8)
        .arg("--output")
        .arg(&out)
        .args(&class_files)
        .status()
        .ok()?;
    if !st.success() {
        acos_test_support::require_prereq(
            false,
            "canary test",
            &format!("d8 failed ({})", d8.display()),
        )?;
        return None;
    }
    let dex = out.join("classes.dex");
    dex.is_file().then_some(dex)
}

#[test]
fn guest_cannot_overwrite_boot_art_or_the_classpath() {
    let _serial = art_guard();
    let Some(()) = require_art() else { return };
    let scratch = scratch("canary");
    let data = seed_data(&scratch);
    // The launcher rewrites `<data>/acos.properties` from `Props::for_launch()`
    // on every run (it is a derived table, not a guest-writable input). The warm
    // copy can predate the current defaults (for example the software-GL
    // properties once the provider is installed), so refresh it before the
    // "before" snapshot: the host-side proof then compares what the *guest* did,
    // not the launcher's own regeneration.
    acos_art::Props::for_launch()
        .and_then(|props| props.write(&data.join("acos.properties")))
        .expect("refresh the derived property table before the snapshot");
    let Some(dex) = build_canary_dex(&scratch) else {
        let _ = std::fs::remove_dir_all(&scratch);
        return;
    };

    // A real boot image if the host has one (`.cache/art-exp` from the
    // feasibility spike; `acos-art build-image` produces `.cache/art/data/img`).
    // The guest then really can attempt `boot.art`, not just a path that is
    // absent from the sandbox.
    let image = [".cache/art/data/img", ".cache/art-exp/data/img"]
        .iter()
        .map(|p| repo_root().join(p))
        .find(|p| p.join("x86_64/boot.art").is_file());

    let mut inputs: Vec<(&str, PathBuf)> = vec![
        (
            "/data/system/environ/classpath",
            data.join("system/environ/classpath"),
        ),
        ("/data/acos.properties", data.join("acos.properties")),
        ("/data/shim/libartshim.so", data.join("shim/libartshim.so")),
        (
            "/data/app/guest/classes.dex",
            data.join("app/guest/classes.dex"),
        ),
        (
            "/linkerconfig/ld.config.txt",
            data.join("linkerconfig/ld.config.txt"),
        ),
    ];
    if let Some(img) = &image {
        inputs.push(("/data/img/x86_64/boot.art", img.join("x86_64/boot.art")));
    }
    for (_, host) in &inputs {
        assert!(
            host.is_file(),
            "canary target missing on the host: {}",
            host.display()
        );
    }
    let before: Vec<String> = inputs.iter().map(|(_, h)| md5(h)).collect();

    let mut cmd = launcher("run", &data);
    cmd.arg("--dex").arg(&dex).arg("--class").arg("Canary");
    if let Some(img) = &image {
        cmd.arg("--image").arg(img);
    }
    let out = cmd.output().expect("spawn acos-art run (canary)");
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(
        out.status.success(),
        "the canary run itself failed (rc={:?})\nstdout:\n{stdout}\nstderr:\n{stderr}",
        out.status.code()
    );

    for (guest, _host) in &inputs {
        assert!(
            stdout.contains(&format!("READ {guest}")),
            "the shared input {guest} is not even visible/readable in the guest\nstdout:\n{stdout}\nstderr:\n{stderr}"
        );
        assert!(
            stdout.contains(&format!("DENIED {guest}")),
            "the guest was not denied write access to {guest}\nstdout:\n{stdout}\nstderr:\n{stderr}"
        );
        assert!(
            !stdout.contains(&format!("WROTE {guest}")),
            "the guest WROTE the shared input {guest} (S4 regression)\nstdout:\n{stdout}"
        );
    }
    let after: Vec<String> = inputs.iter().map(|(_, h)| md5(h)).collect();
    // The host-side proof: the bytes are identical, not just the guest's opinion.
    assert_eq!(
        before, after,
        "a shared input changed on the host during the canary run"
    );

    // And the run root is gone, so the canary left nothing behind either.
    let runs = dir_entries(&data.join("runs"));
    assert!(
        runs.is_empty(),
        "canary run left per-run roots behind: {runs:?}"
    );

    let _ = std::fs::remove_dir_all(&scratch);
}

// ---------------------------------------------------------------------------
// 2b-bis. A *guest crash* is still a normal host exit: the run root goes away
// ---------------------------------------------------------------------------

#[test]
fn guest_crash_still_removes_the_run_root() {
    let _serial = art_guard();
    let Some(()) = require_art() else { return };
    let scratch = scratch("crash");
    let data = seed_data(&scratch);
    let sock = scratch.join("session.sock");
    let log = std::fs::File::create(scratch.join("session.log")).expect("session log");
    let before = snapshot();

    // A real session, then SIGKILL the *guest* (not the launcher): the launcher
    // must survive the crash, report it, and still remove its run root.
    let mut cmd = launcher("session", &data);
    cmd.arg("--apk")
        .arg(apk())
        .arg("--out")
        .arg(scratch.join("out"))
        .arg("--socket")
        .arg(&sock)
        .arg("--session-id")
        .arg("h1crash")
        // A session whose guest dies does not end early (it waits for the
        // protocol relay), so the wall-clock budget is what ends it. Short, so
        // this test is about the *cleanup*, not about waiting.
        .arg("--timeout")
        .arg("8")
        .stdout(Stdio::null())
        .stderr(Stdio::from(log));
    let mut child = Reaper::spawn(&mut cmd, "acos-art session");
    let mut scope = Scope::new();
    scope.own_run(&scratch, &data);
    scope.add_root(child.pid());

    let deadline = Instant::now() + Duration::from_secs(60);
    let mut guests = Vec::new();
    while Instant::now() < deadline {
        guests = scope.procs().into_iter().filter(Proc::is_guest).collect();
        if !guests.is_empty() {
            break;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    assert!(
        !guests.is_empty(),
        "no dalvikvm64 guest of this run appeared within 60 s; log at {}\n\
         (other processes on this host, not this test's:{})",
        scratch.join("session.log").display(),
        describe(&scope.foreign(&before))
    );
    // Only this run's own guest: the old scan SIGKILLed every *new* dalvikvm64 on
    // the host, i.e. every concurrent agent's guest too (W52 r2).
    for g in &guests {
        unsafe {
            libc::kill(g.pid as i32, libc::SIGKILL);
        }
    }

    // A session waits for its client, so close it (the guest is already gone;
    // the close must not resurrect it) and let the launcher finish on its own.
    if let Ok(mut s) = UnixStream::connect(&sock) {
        let _ = s.write_all(b"{\"cmd\":\"close\"}\n");
        let _ = s.flush();
    }
    assert!(
        child.wait_or_kill(Duration::from_secs(25)),
        "the launcher did not exit within 25 s of its guest being SIGKILLed (timeout 8 s)"
    );
    let survivors = scope.wait_until_clean(Duration::from_secs(3));
    let detail = describe(&survivors);
    scope.reap(&survivors);
    assert!(
        survivors.is_empty(),
        "a crashed guest left processes behind:{detail}"
    );

    let runs = dir_entries(&data.join("runs"));
    assert!(
        runs.is_empty(),
        "a crashed guest left its per-run data root behind: {runs:?}"
    );

    let _ = std::fs::remove_dir_all(&scratch);
}

// ---------------------------------------------------------------------------
// 2c. The startup janitor (the SIGKILL fallback) keeps live runs, reaps dead
// ---------------------------------------------------------------------------

fn own_pid_start() -> (u32, String) {
    let stat = std::fs::read_to_string("/proc/self/stat").expect("/proc/self/stat");
    let i = stat.rfind(')').expect("comm");
    let start = stat[i + 2..]
        .split_whitespace()
        .nth(19)
        .expect("starttime")
        .to_string();
    (std::process::id(), start)
}

fn age(path: &Path, spec: &str) {
    let st = Command::new("touch")
        .arg("-d")
        .arg(spec)
        .arg(path)
        .status()
        .expect("touch");
    assert!(st.success(), "touch -d {spec} {}", path.display());
}

#[test]
fn janitor_reaps_stale_runs_and_keeps_live_ones() {
    let _serial = art_guard();
    let Some(()) = require_art() else { return };
    let scratch = scratch("janitor");
    let data = seed_data(&scratch);
    let runs = data.join("runs");
    std::fs::create_dir_all(&runs).expect("create runs dir");

    let mk = |name: &str, owner: Option<String>| -> PathBuf {
        let d = runs.join(name);
        std::fs::create_dir_all(&d).expect("run dir");
        if let Some(o) = owner {
            std::fs::write(d.join("owner"), o).expect("owner file");
        }
        d
    };
    let stale = mk("stale", Some("pid=999999\nstart=1\ntag=stale\n".into()));
    let no_owner = mk("no-owner", None);
    let dead_fresh = mk(
        "dead-fresh",
        Some("pid=999998\nstart=1\ntag=fresh\n".into()),
    );
    let (pid, start) = own_pid_start();
    let live = mk(
        "live",
        Some(format!("pid={pid}\nstart={start}\ntag=live\n")),
    );
    for d in [stale.as_path(), no_owner.as_path(), live.as_path()] {
        age(&d.join("owner"), "2 hours ago");
        age(d, "2 hours ago");
    }

    // Default TTL (30 min): old + dead is reaped, old + live is not, fresh + dead
    // is kept in case its owner is still finishing.
    let out = launcher("janitor", &data).output().expect("janitor");
    assert!(out.status.success(), "janitor failed: {out:?}");
    assert!(
        !stale.exists(),
        "janitor kept a stale run dir with no live owner"
    );
    assert!(
        !no_owner.exists(),
        "janitor kept an old run dir with no owner record"
    );
    assert!(live.exists(), "janitor reaped a LIVE run dir (pid {pid})");
    assert!(
        dead_fresh.exists(),
        "janitor reaped a fresh run dir before the TTL"
    );

    // Zero TTL: the SIGKILL case — the pid is gone, the age does not matter.
    let out = launcher("janitor", &data)
        .env("ACOS_ART_RUN_TTL_SECS", "0")
        .output()
        .expect("janitor (ttl=0)");
    assert!(out.status.success(), "janitor failed: {out:?}");
    assert!(
        !dead_fresh.exists(),
        "a zero-TTL sweep must reap a dead owner's run dir"
    );
    assert!(
        live.exists(),
        "a zero-TTL sweep must still keep a live run dir"
    );

    let _ = std::fs::remove_dir_all(&scratch);
}

// ---------------------------------------------------------------------------
// 3. The R1 watchdog: a structured HOST_MEMORY_LIMIT, and no orphans
// ---------------------------------------------------------------------------

#[test]
fn r1_watchdog_reports_host_memory_limit_and_leaves_no_orphans() {
    let _serial = art_guard();
    let Some(()) = require_art() else { return };
    let dex = repo_root().join("fixtures/build/dexprogs/01_hello/classes.dex");
    if !dex.is_file() {
        acos_test_support::require_prereq(
            false,
            "watchdog test",
            &format!(
                "no {} — build the dex corpus (tools/build-fixtures.sh)",
                dex.display()
            ),
        );
        return;
    }
    let scratch = scratch("watchdog");
    let data = seed_data(&scratch);
    let before = snapshot();
    let mut scope = Scope::new();
    scope.own_run(&scratch, &data);

    // A 1 MiB launcher ceiling: crossed before the guest even exists. The
    // structured RLIMIT_DATA backstop must be off, otherwise the launcher would
    // fail to allocate before the watchdog can report (R1 layer 3 vs layer 2).
    let out = {
        let mut cmd = launcher("run", &data);
        cmd.env("ACOS_ART_RSS_MAX", "1M")
            .env("ACOS_ART_RLIMIT_DATA", "0")
            .arg("--dex")
            .arg(&dex)
            .arg("--class")
            .arg("Main")
            .arg("--json")
            .arg("--timeout")
            .arg("120")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = Reaper::spawn(&mut cmd, "acos-art run (watchdog)");
        scope.add_root(child.pid());
        child.wait_with_output()
    };

    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(
        !out.status.success(),
        "a 1 MiB ceiling must not end in success (rc={:?})",
        out.status.code()
    );
    // The structured result is the contract: a kill that is not *reported* is
    // the incident again (a silent OOM victim).
    let json_start = stdout
        .find('{')
        .unwrap_or_else(|| panic!("no JSON on stdout: {stdout}"));
    let json: serde_json::Value =
        serde_json::from_str(&stdout[json_start..]).expect("run --json emits valid JSON");
    assert_eq!(json["outcome"], "unsupported", "json={json}");
    assert_eq!(json["code"], "HOST_MEMORY_LIMIT", "json={json}");
    let diags = json["diagnostics"].to_string();
    assert!(diags.contains("HOST_MEMORY_LIMIT"), "diagnostics={diags}");
    assert!(
        diags.contains("ACOS_ART_RSS_MAX"),
        "the trip must name its knob: {diags}"
    );
    assert!(
        stderr.contains("HOST_MEMORY_LIMIT"),
        "the watchdog must say so on stderr too: {stderr}"
    );

    let leaked = scope.wait_until_clean(Duration::from_secs(3));
    let detail = describe(&leaked);
    let foreign = describe(&scope.foreign(&before));
    scope.reap(&leaked);
    assert!(
        leaked.is_empty(),
        "the watchdog kill left {} of this run's process(es) alive after 3 s (the report is not \
         a cleanup):{detail}\n(other processes on this host, not this test's, ignored:{foreign})",
        leaked.len()
    );

    let _ = std::fs::remove_dir_all(&scratch);
}

// ---------------------------------------------------------------------------
// 4. The log-spam scenario: the launcher's peak RSS stays bounded
// ---------------------------------------------------------------------------

/// One frame in the shim's logd format: `[prio][tlen:u16][mlen:u16]pad tag msg`.
fn log_frame(msg_len: usize) -> Vec<u8> {
    let mut f = vec![6u8, 3, 0];
    f.extend_from_slice(&(msg_len as u16).to_le_bytes());
    f.push(0);
    f.extend_from_slice(b"art");
    f.extend(std::iter::repeat(b'x').take(msg_len));
    f
}

#[test]
fn log_spam_keeps_the_launcher_peak_rss_under_256_mib() {
    let _serial = art_guard();
    let Some(()) = require_art() else { return };
    let scratch = scratch("logspam");
    let data = seed_data(&scratch);
    let sock = scratch.join("session.sock");
    let log = std::fs::File::create(scratch.join("session.log")).expect("session log");
    let before = snapshot();

    let mut cmd = launcher("session", &data);
    cmd.arg("--apk")
        .arg(apk())
        .arg("--out")
        .arg(scratch.join("out"))
        .arg("--socket")
        .arg(&sock)
        .arg("--session-id")
        .arg("h1spam")
        .arg("--timeout")
        .arg("180")
        .stdout(Stdio::null())
        .stderr(Stdio::from(log));
    let mut child = Reaper::spawn(&mut cmd, "acos-art session");
    let pid = child.pid();
    let mut scope = Scope::new();
    scope.own_run(&scratch, &data);
    scope.add_root(pid);

    // The launcher's logd binds a per-session socket; under S4 the session dir
    // is the run's private root (`<data>/runs/<id>/socket/logdw`), so find it by
    // name rather than by the old shared `<data>/session/<id>` path.
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut logdw = data.join("session/h1spam/socket/logdw");
    while !logdw.exists() {
        if let Ok(rd) = std::fs::read_dir(data.join("runs")) {
            for e in rd.flatten() {
                let cand = e.path().join("socket/logdw");
                if cand.exists() {
                    logdw = cand;
                }
            }
        }
        if logdw.exists() {
            break;
        }
        if Instant::now() >= deadline {
            let tail = std::fs::read_to_string(scratch.join("session.log")).unwrap_or_default();
            panic!(
                "no logd sink under {} within 60 s:\n{tail}",
                data.join("runs").display()
            );
        }
        std::thread::sleep(Duration::from_millis(25));
    }

    // Peak RSS from outside (`VmHWM` = the kernel's own high-water mark), sampled
    // until the flood is over.
    let peak_kb = Arc::new(AtomicU64::new(0));
    let stop = Arc::new(AtomicBool::new(false));
    let sampler = {
        let peak_kb = Arc::clone(&peak_kb);
        let stop = Arc::clone(&stop);
        std::thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                if let Some(kb) = status_kb(pid, "VmHWM") {
                    peak_kb.fetch_max(kb, Ordering::Relaxed);
                } else {
                    break; // the launcher exited
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        })
    };

    // The R1 flood: the guest's liblog loop with nothing draining it. 900-byte
    // entries into the real sink, at least 300 MiB, so an unbounded host (the
    // regression) would blow past the 256 MiB ceiling rather than nudge it.
    let frame = log_frame(900);
    let target: u64 = 300 * 1024 * 1024;
    let sock_dg = UnixDatagram::unbound().expect("unbound datagram socket");
    sock_dg
        .set_nonblocking(true)
        .expect("non-blocking logdw writer");
    sock_dg.connect(&logdw).expect("connect logdw");
    let mut sent: u64 = 0;
    let mut spins: u64 = 0;
    let budget = Instant::now() + Duration::from_secs(40);
    while sent < target && Instant::now() < budget {
        match sock_dg.send(&frame) {
            Ok(n) => {
                sent += n as u64;
                spins = 0;
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                // The sink is behind: this is the incident's shape (producer
                // faster than the reader), just bounded by the cap.
                spins += 1;
                if spins % 32 == 0 {
                    std::thread::sleep(Duration::from_millis(1));
                }
            }
            Err(e) => panic!("logdw send failed after {sent} bytes: {e}"),
        }
    }
    std::thread::sleep(Duration::from_millis(300));
    stop.store(true, Ordering::Relaxed);
    let _ = sampler.join();

    let peak_mb = peak_kb.load(Ordering::Relaxed) as f64 / 1024.0;
    let sent_mb = sent as f64 / 1024.0 / 1024.0;

    // Close the session politely, then insist it exits.
    if let Ok(mut s) = UnixStream::connect(&sock) {
        let _ = s.write_all(b"{\"cmd\":\"close\"}\n");
        let _ = s.flush();
    }
    let exited = child.wait_or_kill(Duration::from_secs(10));

    let survivors = scope.wait_until_clean(Duration::from_secs(3));
    let detail = describe(&survivors);
    let foreign = describe(&scope.foreign(&before));
    scope.reap(&survivors);

    assert!(
        sent_mb > 200.0,
        "the flood did not happen: only {sent_mb:.0} MiB of log frames were accepted"
    );
    // The bound is the point: R1 grew ~400 MB/s in one thread. The cap must keep
    // the launcher flat no matter how much the guest writes.
    assert!(
        peak_mb < LAUNCHER_PEAK_LIMIT_MB,
        "launcher peak RSS was {peak_mb:.1} MiB after {sent_mb:.0} MiB of log spam \
         (limit {LAUNCHER_PEAK_LIMIT_MB:.0} MiB) — the R1 bound regressed"
    );
    assert!(exited, "the session did not exit after `close` within 10 s");
    assert!(
        survivors.is_empty(),
        "log-spam run left {} of this run's process(es) behind after close:{detail}\n\
         (other processes on this host, not this test's, ignored:{foreign})",
        survivors.len()
    );
    eprintln!(
        "logspam: {sent_mb:.0} MiB of frames, launcher peak RSS {peak_mb:.1} MiB \
         (VmHWM, limit {LAUNCHER_PEAK_LIMIT_MB:.0} MiB)"
    );

    let _ = std::fs::remove_dir_all(&scratch);
}

// ---------------------------------------------------------------------------
// 5. The scan's teeth, without ART: a leaked guest of *this* run is caught, a
//    concurrent agent's is neither counted nor touched
// ---------------------------------------------------------------------------

/// Three stand-in "guests" named `dalvikvm64` (a copy of `/bin/sleep`, so the
/// scan's `comm` filter sees them exactly like a real one):
///
/// * `own_marker` — no registered root, only this test's private data/scratch
///   path in `cwd`. This is the **leaked-guest** case: the launcher is gone, so
///   ancestry cannot work, and only a marker can still prove ownership.
/// * `own_root` — registered root, but nothing of this run in `cmd`/`cwd`: the
///   ancestry case (what a live `bwrap`+guest looks like before it leaks).
/// * `foreign` — another agent's guest: no root, no marker.
///
/// Asserts the scan sees the two that are ours, ignores the foreign one, and
/// that `reap` — handed the **host-wide** list, as the old code did — signals
/// only ours. That is the W52 r2 sweep, structurally prevented.
#[test]
fn leak_scan_is_scoped_to_this_tests_own_processes() {
    let ours = scratch("scope-own");
    let neutral = scratch("scope-neutral");
    let foreign_dir = scratch("scope-foreign");
    let bin_dir = scratch("scope-bin");
    let fake = bin_dir.join("dalvikvm64");
    let st = Command::new("cp")
        .arg("-a")
        .arg("/bin/sleep")
        .arg(&fake)
        .status()
        .expect("cp /bin/sleep");
    assert!(
        st.success(),
        "could not stage the fake guest at {}",
        fake.display()
    );

    let before = snapshot();
    let mut scope = Scope::new();
    scope.own_run(&ours, &ours.join("data"));
    std::fs::create_dir_all(ours.join("data")).expect("own data dir");

    let spawn_fake = |cwd: &Path| -> Child {
        let mut c = Command::new(&fake);
        c.arg("120").current_dir(cwd).stdin(Stdio::null());
        c.stdout(Stdio::null()).stderr(Stdio::null());
        c.spawn().expect("spawn the fake guest")
    };
    // Leak case: identified by the marker only (no root registered).
    let mut own_marker = spawn_fake(&ours);
    // Ancestry case: identified by its root only (neutral cwd, marker-free cmd).
    let mut own_root = spawn_fake(&neutral);
    scope.add_root(own_root.id());
    // A concurrent agent's guest.
    let mut foreign = spawn_fake(&foreign_dir);

    let deadline = Instant::now() + Duration::from_secs(10);
    let mut owned = Vec::new();
    while Instant::now() < deadline {
        owned = scope.owned();
        if owned.len() >= 2 {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let pids: BTreeSet<u32> = owned.iter().map(|p| p.pid).collect();
    let foreign_desc = describe(&scope.foreign(&before));
    assert!(
        pids.contains(&own_marker.id()),
        "the marker-only leaked guest (pid {}) is not owned by the scan — the teeth are gone. \
         owned={:?}; ignored:{foreign_desc}",
        own_marker.id(),
        pids
    );
    assert!(
        pids.contains(&own_root.id()),
        "the root-identified guest (pid {}) is not owned by the scan. owned={:?}; ignored:{foreign_desc}",
        own_root.id(),
        pids
    );
    assert!(
        !pids.contains(&foreign.id()),
        "the scan claimed another run's guest (pid {}) as this test's: owned={:?}",
        foreign.id(),
        pids
    );
    let foreign_named = scope.foreign(&before).iter().any(|p| p.pid == foreign.id());
    assert!(
        foreign_named,
        "the foreign guest must be *reported* (never killed):{foreign_desc}"
    );

    // The survivor check still fires for our own leak...
    let survivors = scope.wait_until_clean(Duration::from_secs(1));
    assert_eq!(
        survivors.iter().map(|p| p.pid).collect::<BTreeSet<_>>(),
        [own_marker.id(), own_root.id()]
            .into_iter()
            .collect::<BTreeSet<_>>(),
        "the clean-check did not report exactly this run's two stand-in guests"
    );

    // ...and `reap`, handed the host-wide list (the old code's input), kills only
    // ours: the foreign guest is still alive afterwards.
    let host_wide = scan_procs();
    assert!(
        host_wide.iter().any(|p| p.pid == foreign.id()),
        "the host-wide scan must contain the foreign guest, otherwise this check proves nothing"
    );
    scope.reap(&host_wide);

    let alive = |child: &mut Child| -> bool { child.try_wait().ok().flatten().is_none() };
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut own_gone = false;
    while Instant::now() < deadline {
        if !alive(&mut own_marker) && !alive(&mut own_root) {
            own_gone = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        own_gone,
        "reap did not clear this test's own (stand-in) leaked guests"
    );
    assert!(
        alive(&mut foreign),
        "reap killed another run's guest (pid {}) — the W52 r2 sweep is back",
        foreign.id()
    );

    // Tidy up what the test itself started (nothing of it is left behind).
    let _ = foreign.kill();
    let _ = foreign.wait();
    let _ = own_marker.wait();
    let _ = own_root.wait();
    for d in [&ours, &neutral, &foreign_dir, &bin_dir] {
        let _ = std::fs::remove_dir_all(d);
    }
}
