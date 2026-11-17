//! S6: Landlock in the ART guest, plus the adversarial escape corpus.
//!
//! Every test here runs a small **native** program inside the real guest mount
//! view (bwrap argv from [`acos_art::Sandbox`], the same preloaded guard, the
//! same per-run `/data`), so what it observes is what untrusted guest code
//! observes. Claims, each failing if its layer is removed:
//!
//! 1. **Landlock applies in the guest, fail closed.** The constructor logs
//!    `OK landlock=active`; deleting the plan on a kernel that supports
//!    Landlock refuses the run (`ERR landlock=plan-missing`, exit 126) instead
//!    of running unfiltered.
//! 2. **Writes are confined to the per-run root.** `/data/out` and `/data/tmp`
//!    are writable; `/data`, `/data/system`, `/data/local/tmp`,
//!    `/linkerconfig` and `/etc` are not, even where DAC would allow the write.
//! 3. **The host is not readable.** The host `$HOME` path, host `/etc` and
//!    `/proc/1/root/...` are absent from the guest's mount/pid view (`ENOENT`),
//!    and the guest's `/proc` holds only its own processes.
//! 4. **The kernel layers deny the classic escapes**: `ptrace`, `userfaultfd`,
//!    non-`AF_UNIX` sockets (seccomp); `AF_UNIX` still works.
//! 5. **Bombs are bounded**: a fork bomb stays under `pids.max`; a memory bomb
//!    is OOM-killed at the cgroup cap with the host's `MemAvailable`
//!    essentially unchanged.
//!
//! Scratch lives under `.cache/s6-tests/` (never `/tmp`); one guest at a time
//! (single mutex). No ART is started: the guest program is the probe.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

use acos_art::cgroup::{MemoryLimits, RunCgroup};
use acos_art::{ArtRoot, RunDir};

static ONE_GUEST: Mutex<()> = Mutex::new(());

fn guard() -> MutexGuard<'static, ()> {
    ONE_GUEST.lock().unwrap_or_else(|e| e.into_inner())
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("the crate lives at <repo>/crates/acos-art")
}

fn scratch(tag: &str) -> PathBuf {
    let dir = repo_root()
        .join(".cache/s6-tests")
        .join(format!("{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch under .cache/");
    dir
}

/// Host `MemAvailable` in MiB (`/proc/meminfo`).
fn host_available_mb() -> f64 {
    let text = std::fs::read_to_string("/proc/meminfo").expect("/proc/meminfo");
    text.lines()
        .find_map(|l| l.strip_prefix("MemAvailable:"))
        .and_then(|rest| rest.split_whitespace().next())
        .and_then(|kb| kb.parse::<f64>().ok())
        .map(|kb| kb / 1024.0)
        .expect("MemAvailable is present")
}

fn processes_in(cg: &Path) -> Vec<u32> {
    std::fs::read_to_string(cg.join("cgroup.procs"))
        .map(|t| t.lines().filter_map(|l| l.trim().parse().ok()).collect())
        .unwrap_or_default()
}

/// The guest probe: one binary, `argv[1]` selects the attack.
///
/// Dynamic on purpose: a static binary ignores `LD_PRELOAD`, and the guard is
/// the subject. Host glibc is bound into the guest by [`guest_command`].
const PROBE_SRC: &str = r#"#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <fcntl.h>
#include <errno.h>
#include <dirent.h>
#include <signal.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <sys/ptrace.h>

static void try_write(const char *what, const char *path)
{
    int fd = open(path, O_WRONLY | O_CREAT | O_TRUNC, 0644);
    int err = errno;
    if (fd >= 0) {
        ssize_t n = write(fd, "pwned\n", 6);
        close(fd);
        printf("probe: %s WRITTEN (n=%ld)\n", what, (long)n);
    } else {
        printf("probe: %s DENIED errno=%s\n", what, err == EACCES ? "EACCES" : strerror(err));
    }
}

static void try_open(const char *what, const char *path)
{
    int fd = open(path, O_RDONLY);
    int err = errno;
    if (fd >= 0) {
        printf("probe: %s OPENED\n", what);
        close(fd);
    } else {
        printf("probe: %s MISSING errno=%s\n", what, err == ENOENT ? "ENOENT" : strerror(err));
    }
}

static void escapes(void)
{
    long r;
    int fd;
    int n = 0;
    DIR *d;
    struct dirent *e;

    /* 1. inside the per-run root: must work */
    try_write("write-run-out", "/data/out/landlock-ok");
    try_write("write-run-tmp", "/data/tmp/landlock-ok");
    /* 2. outside it: Landlock must deny even where DAC allows */
    try_write("write-data-root", "/data/pwn");
    try_write("write-data-system", "/data/system/environ/pwn");
    try_write("write-data-local-tmp", "/data/local/tmp/pwn");
    try_write("write-linkerconfig", "/linkerconfig/pwn");
    try_write("write-sysroot-etc", "/etc/pwn");
    /* 3. host content is not in the mount view */
    try_open("read-host-home", "/home/Qui/Projects/acos/Cargo.toml");
    try_open("read-host-etc", "/etc/hostname");
    try_open("read-host-proc-root", "/proc/1/root/home/Qui/.bashrc");

    /* 4. guest /proc is its own pid namespace */
    d = opendir("/proc");
    if (d) {
        while ((e = readdir(d)) != NULL)
            if (e->d_name[0] >= '0' && e->d_name[0] <= '9')
                n++;
        closedir(d);
    }
    printf("probe: visible-pids %d\n", n);

    /* 5. ptrace is denied by seccomp */
    errno = 0;
    r = ptrace(PTRACE_TRACEME, 0, 0, 0);
    printf("probe: ptrace %s\n", (r == -1 && errno == EACCES) ? "DENIED" : "ALLOWED");

    /* 6. userfaultfd is the ART GC probe the run audit found; still denied */
    errno = 0;
    r = syscall(323, O_CLOEXEC | O_NONBLOCK); /* SYS_userfaultfd */
    printf("probe: userfaultfd %s\n", (r == -1 && errno == EACCES) ? "DENIED" : (r >= 0 ? "ALLOWED" : "OTHER"));

    /* 7. network: every family but AF_UNIX is denied */
    errno = 0;
    fd = socket(AF_INET, SOCK_STREAM, 0);
    printf("probe: socket-inet %s\n", fd < 0 && errno == EACCES ? "DENIED" : "ALLOWED");
    if (fd >= 0) close(fd);
    errno = 0;
    fd = socket(AF_UNIX, SOCK_STREAM, 0);
    printf("probe: socket-unix %s\n", fd >= 0 ? "ALLOWED" : "DENIED");
    if (fd >= 0) close(fd);
    fflush(stdout);
}

static void fork_bomb(void)
{
    int i;
    printf("probe: forkbomb start\n");
    fflush(stdout);
    for (i = 0; i < 300; i++) {
        pid_t p = fork();
        if (p == 0) {
            sleep(20);
            _exit(0);
        }
        if (p < 0) {
            printf("probe: fork stopped at %d (errno=%d)\n", i, errno);
            fflush(stdout);
            break;
        }
    }
    sleep(20);
}

static void mem_bomb(void)
{
    printf("probe: membomb start\n");
    fflush(stdout);
    for (;;) {
        char *p = malloc(1024 * 1024);
        if (!p) {
            printf("probe: malloc failed\n");
            fflush(stdout);
            _exit(3);
        }
        memset(p, 0x5a, 1024 * 1024);
    }
}

int main(int argc, char **argv)
{
    setvbuf(stdout, NULL, _IOLBF, 0);
    if (argc < 2) return 2;
    if (strcmp(argv[1], "escapes") == 0) escapes();
    else if (strcmp(argv[1], "forkbomb") == 0) fork_bomb();
    else if (strcmp(argv[1], "membomb") == 0) mem_bomb();
    else return 2;
    return 0;
}
"#;

/// Compile the probe once per scratch dir; `None` when no host `cc` can link a
/// dynamic binary.
fn build_probe(dir: &Path) -> Option<PathBuf> {
    let src = dir.join("probe.c");
    let out = dir.join("probe");
    if out.is_file() {
        return Some(out);
    }
    std::fs::write(&src, PROBE_SRC).ok()?;
    let st = Command::new("cc")
        .args(["-O1", "-o"])
        .arg(&out)
        .arg(&src)
        .status()
        .ok()?;
    st.success().then_some(out)
}

/// An `ArtRoot` whose data dir is scratch, with the S5+S6 guard artefacts
/// (including `<data>/seccomp/landlock.plan`) built.
fn probe_root(tag: &str) -> Option<ArtRoot> {
    // A distinct directory from the probe/run scratch below: `scratch` wipes
    // its directory, and the root's data dir (with the guard artefacts) must
    // survive until the run has used it.
    let dir = scratch(&format!("{tag}-root"));
    let root = match ArtRoot::resolve(None, Some(dir.join("data"))) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("SKIP {tag}: no ART sysroot ({e})");
            return None;
        }
    };
    root.ensure_data().expect("data skeleton");
    acos_art::seccomp::build_guest(&root.data.join("seccomp")).expect("guard artefacts");
    Some(root)
}

/// The real guest argv: per-run `/data`, the preloaded guard, the probe bound
/// read-only, plus the host glibc the probe links against.
fn guest_command(root: &ArtRoot, run: &Path, probe: &Path, mode: &str) -> Command {
    let mut sb = root.sandbox();
    sb.run_root(run);
    sb.seccomp_guest(true);
    sb.with_shim(false);
    sb.ro_bind(probe, "/data/probe");
    for host in ["/usr", "/lib", "/lib64"] {
        if Path::new(host).is_dir() {
            sb.ro_bind(host, host);
        }
    }
    sb.args(Vec::new());
    sb.command("/data/probe", &[mode.to_string()])
        .expect("sandbox argv")
}

struct ProbeRun {
    status: Option<std::process::ExitStatus>,
    stdout: String,
    stderr: String,
    cg: RunCgroup,
    cg_path: PathBuf,
    run: RunDir,
    /// `(oom_kill, memory.peak)` latched by the in-scope observer (W53).
    events: (u64, u64),
}

/// W53: `memory.events` lives in the scope's transient cgroup and is removed
/// with it, so a sampler in this process races the teardown (27.5 % of OOM
/// events were lost on a loaded host). The payload is therefore this
/// observer: it ignores TERM (the bomb is killed by the kernel with SIGKILL),
/// records the cgroup, runs the probe, and latches the scope's counters into
/// `<info>.final` before the unit stops. Same construction as
/// `tests/sandbox_s5.rs`; see `results/art-runtime/W53.md`.
fn write_scope_observer(dir: &Path) -> PathBuf {
    let script = dir.join("scope-observer.sh");
    std::fs::write(
        &script,
        r#"#!/bin/sh
trap '' TERM INT HUP
info="$1"; shift
cat /proc/self/cgroup > "$info" 2>/dev/null || exit 90
"$@"; rc=$?
rel=$(sed -n 's|^0::||p' "$info")
if [ -n "$rel" ] && [ -d "/sys/fs/cgroup$rel" ]; then
    {
        cat "/sys/fs/cgroup$rel/memory.events" 2>/dev/null
        echo "memory.peak $(cat "/sys/fs/cgroup$rel/memory.peak" 2>/dev/null || echo 0)"
    } > "$info.final.tmp" 2>/dev/null && mv "$info.final.tmp" "$info.final" 2>/dev/null
fi
exit $rc
"#,
    )
    .expect("write the observer");
    std::fs::set_permissions(&script, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    script
}

/// `(oom_kill, memory.peak)` from a scope snapshot; missing/garbage reads 0.
fn parse_scope_events(info: &Path) -> (u64, u64) {
    let path = PathBuf::from(format!("{}.final", info.display()));
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let mut oom = 0;
    let mut peak = 0;
    for line in text.lines() {
        let mut parts = line.split_whitespace();
        let key = parts.next().unwrap_or_default();
        let value = parts.next().and_then(|v| v.parse::<u64>().ok());
        match (key, value) {
            ("oom_kill", Some(v)) => oom = oom.max(v),
            ("memory.peak", Some(v)) => peak = peak.max(v),
            _ => {}
        }
    }
    (oom, peak)
}

fn spawn_probe(
    root: &ArtRoot,
    tag: &str,
    mode: &str,
    limits: MemoryLimits,
) -> Option<(Child, RunCgroup, PathBuf, RunDir, PathBuf, PathBuf)> {
    let dir = scratch(tag);
    let probe = build_probe(&dir)?;
    let run = root.new_run_dir(tag).expect("run dir");
    let out_host = run.path().join("probe.stdout");
    let err_host = run.path().join("probe.stderr");
    let out = std::fs::File::create(&out_host).expect("probe stdout file");
    let err = std::fs::File::create(&err_host).expect("probe stderr file");
    let guest = guest_command(root, run.path(), &probe, mode);
    let info = run.path().join("cgroup");
    let observer = write_scope_observer(&dir);
    let mut cmd = Command::new("/bin/sh");
    cmd.arg(&observer)
        .arg(&info)
        .arg(guest.get_program())
        .args(guest.get_args());
    let mut cg = RunCgroup::prepare(limits, &info).expect("user scope");
    let mut wrapped = cg.wrap(&cmd).unwrap();
    // `wrap` rebuilds the command, so the stdio must be set on the *wrapped*
    // one (the process actually spawned; the scope's children inherit it).
    wrapped
        .stdin(Stdio::null())
        .stdout(Stdio::from(out))
        .stderr(Stdio::from(err));
    let child = wrapped.spawn().expect("spawn the guest probe");
    cg.confirm(Duration::from_secs(10)).expect("cgroup proven");
    let cg_path = cg.path().to_path_buf();
    Some((child, cg, cg_path, run, out_host, err_host))
}

/// Run the probe to completion under a wall-clock bound.
fn run_probe(
    root: &ArtRoot,
    tag: &str,
    mode: &str,
    limits: MemoryLimits,
    timeout: Duration,
) -> Option<ProbeRun> {
    let (mut child, mut cg, cg_path, run, out_host, err_host) =
        spawn_probe(root, tag, mode, limits)?;
    let start = Instant::now();
    let status = loop {
        match child.try_wait().expect("wait") {
            Some(st) => break Some(st),
            None => {}
        }
        cg.sample();
        if start.elapsed() >= timeout {
            let _ = child.kill();
            cg.kill();
            let _ = child.wait();
            break None;
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    cg.sample();
    // The observer latches the counters before it exits; bounded poll (a latch,
    // not a sleep: the file exists by the time the scope payload was reaped).
    let info = run.path().join("cgroup");
    let settle = Instant::now() + Duration::from_secs(5);
    let mut events = parse_scope_events(&info);
    while events.1 == 0 && Instant::now() < settle {
        std::thread::sleep(Duration::from_millis(5));
        events = parse_scope_events(&info);
    }
    let stdout = std::fs::read_to_string(&out_host).unwrap_or_default();
    let stderr = std::fs::read_to_string(&err_host).unwrap_or_default();
    Some(ProbeRun {
        status,
        stdout,
        stderr,
        cg,
        cg_path,
        run,
        events,
    })
}

fn assert_line(stdout: &str, needle: &str) {
    assert!(
        stdout.contains(needle),
        "missing {needle:?} in probe stdout:\n{stdout}"
    );
}

fn guard_log(run: &RunDir) -> String {
    std::fs::read_to_string(run.path().join("out/seccomp.log")).unwrap_or_default()
}

// ---------------------------------------------------------------------------
// 1. The escape corpus: Landlock + seccomp deny every classic attack
// ---------------------------------------------------------------------------

#[test]
fn escape_corpus_is_denied_in_the_real_guest() {
    let _g = guard();
    let Some(root) = probe_root("escapes") else {
        return;
    };
    let Some(p) = run_probe(
        &root,
        "escapes",
        "escapes",
        MemoryLimits::default(),
        Duration::from_secs(60),
    ) else {
        panic!("the escape probe never finished");
    };
    eprintln!("probe stdout:\n{}", p.stdout);
    eprintln!("probe stderr:\n{}", p.stderr);
    eprintln!("guard log:\n{}", guard_log(&p.run));
    assert_eq!(
        p.status.and_then(|s| s.code()),
        Some(0),
        "the probe itself must survive"
    );

    // (2) writes inside the per-run root succeed ...
    assert_line(&p.stdout, "write-run-out WRITTEN");
    assert_line(&p.stdout, "write-run-tmp WRITTEN");
    // ... and every write outside it is denied by Landlock (EACCES, not EROFS:
    // the mount is writable, the kernel layer is what says no).
    // The tmpfs paths reach Landlock (EACCES); the read-only *binds* are
    // already refused by the mount (EROFS) before Landlock is consulted. Both
    // are denials; which layer wins is part of the record.
    for attack in [
        "write-data-root DENIED errno=EACCES",
        "write-data-local-tmp DENIED errno=EACCES",
        "write-sysroot-etc DENIED errno=EACCES",
    ] {
        assert_line(&p.stdout, attack);
    }
    for attack in [
        "write-data-system DENIED errno=Read-only file system",
        "write-linkerconfig DENIED errno=Read-only file system",
    ] {
        assert_line(&p.stdout, attack);
    }

    // (3) host content and host pids are not in the view.
    for missing in [
        "read-host-home MISSING errno=ENOENT",
        "read-host-etc MISSING errno=ENOENT",
    ] {
        assert_line(&p.stdout, missing);
    }
    // `/proc/1/root/...` is refused by the kernel's ptrace check (S2 measured
    // EACCES for the same lookup), i.e. not readable, whichever errno wins.
    assert!(
        p.stdout
            .contains("read-host-proc-root MISSING errno=Permission denied")
            || p.stdout
                .contains("read-host-proc-root MISSING errno=ENOENT"),
        "host root via /proc/1/root must not be readable:\n{}",
        p.stdout
    );
    let visible: usize = p
        .stdout
        .lines()
        .find_map(|l| l.strip_prefix("probe: visible-pids "))
        .and_then(|n| n.trim().parse().ok())
        .expect("visible-pids line");
    assert!(
        visible > 0 && visible < 80,
        "the guest must see its own pid namespace, not the host's: saw {visible} pids"
    );

    // (4) the seccomp filter and Landlock are both proven in the log.
    let log = guard_log(&p.run);
    assert!(
        log.contains("OK landlock=active"),
        "Landlock must be active: {log:?}"
    );
    assert!(
        log.contains("OK guard=active"),
        "seccomp must be active: {log:?}"
    );
    acos_art::landlock::require_from_log(&log, &p.run.path().join("out/seccomp.log"))
        .expect("Landlock evidence is mandatory");
    acos_art::seccomp::require_guard(p.run.path()).expect("both layers are provable");

    // (5) syscall-level escapes.
    assert_line(&p.stdout, "probe: ptrace DENIED");
    assert_line(&p.stdout, "probe: userfaultfd DENIED");
    assert_line(&p.stdout, "probe: socket-inet DENIED");
    assert_line(&p.stdout, "probe: socket-unix ALLOWED");
}

// ---------------------------------------------------------------------------
// 2. Fail closed: no plan on a Landlock-capable kernel => no run
// ---------------------------------------------------------------------------

#[test]
fn missing_plan_fails_closed_on_a_landlock_kernel() {
    let _g = guard();
    if acos_sandbox::landlock_abi().is_none() {
        eprintln!("SKIP: this kernel has no Landlock (the documented fallback path)");
        return;
    }
    let Some(root) = probe_root("no-plan") else {
        return;
    };
    std::fs::remove_file(root.data.join("seccomp/landlock.plan")).expect("remove the plan");

    // The probe must not run: the constructor exits before seccomp.
    let Some(p) = run_probe(
        &root,
        "no-plan",
        "escapes",
        MemoryLimits::default(),
        Duration::from_secs(60),
    ) else {
        panic!("a run whose guard failed must terminate, not hang");
    };
    let log = guard_log(&p.run);
    eprintln!("guard log: {log:?}");
    assert!(
        log.contains("ERR landlock="),
        "the applier must report the missing plan: {log:?}"
    );
    assert!(
        !p.stdout.contains("write-run-out WRITTEN"),
        "no guest code may run without the mandatory Landlock layer: {:?}",
        p.stdout
    );
    assert_ne!(
        p.status.and_then(|s| s.code()),
        Some(0),
        "a refused run must not exit 0"
    );
    let log_path = p.run.path().join("out/seccomp.log");
    assert!(
        acos_art::landlock::require_from_log(&log, &log_path).is_err(),
        "a run without Landlock evidence must be refused by the launcher side too"
    );
    assert!(
        acos_art::seccomp::require_guard(p.run.path()).is_err(),
        "require_guard must refuse a run whose Landlock layer is missing"
    );
}

// ---------------------------------------------------------------------------
// 3. Bombs: pids.max and memory.max, host untouched
// ---------------------------------------------------------------------------

#[test]
fn fork_bomb_in_the_guest_stays_under_pids_max() {
    let _g = guard();
    let Some(root) = probe_root("fork-bomb") else {
        return;
    };
    let limits = MemoryLimits {
        mem_bytes: 256 * 1024 * 1024,
        pids_max: 32,
    };
    let Some((mut child, mut cg, cg_path, _run, _out, err_host)) =
        spawn_probe(&root, "fork-bomb", "forkbomb", limits)
    else {
        return;
    };
    // Let it try to explode, sampling the scope's process count.
    let mut max_seen = 0usize;
    for _ in 0..100 {
        std::thread::sleep(Duration::from_millis(20));
        max_seen = max_seen.max(processes_in(&cg_path).len());
        cg.sample();
    }
    let _ = child.kill();
    cg.kill();
    let _ = child.wait();
    eprintln!("fork bomb: max processes in scope = {max_seen}");
    eprintln!(
        "guest stderr: {:?}",
        std::fs::read_to_string(&err_host).unwrap_or_default()
    );
    assert!(
        max_seen as u32 <= limits.pids_max,
        "fork bomb reached {max_seen} processes with pids.max={}",
        limits.pids_max
    );
    assert!(
        max_seen >= 2,
        "the bomb did fork at least once (saw {max_seen})"
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    while !processes_in(&cg_path).is_empty() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        processes_in(&cg_path).is_empty(),
        "no process may survive the killed scope ({:?})",
        processes_in(&cg_path)
    );
}

#[test]
fn memory_bomb_in_the_guest_is_oom_killed_at_the_cap() {
    let _g = guard();
    let Some(root) = probe_root("mem-bomb") else {
        return;
    };
    let limits = MemoryLimits {
        mem_bytes: 128 * 1024 * 1024,
        pids_max: 64,
    };
    let host_before = host_available_mb();
    // The bomb runs in the scope; sample the host while it is alive so the
    // safety assertion is about the whole run, not just its endpoints (this
    // host is shared with other agents, so an absolute floor is the honest
    // bound; the scope's own peak is the precise one).
    let sampler = std::thread::spawn(|| {
        let mut min = host_available_mb();
        for _ in 0..200 {
            min = min.min(host_available_mb());
            std::thread::sleep(Duration::from_millis(10));
        }
        min
    });
    let Some(p) = run_probe(
        &root,
        "mem-bomb",
        "membomb",
        limits,
        Duration::from_secs(60),
    ) else {
        panic!("the memory bomb was not killed within 60 s");
    };
    let host_min = sampler.join().unwrap_or_else(|_| host_available_mb());
    let host_after = host_available_mb();
    let (snap_oom, snap_peak) = p.events;
    let peak_mb = snap_peak as f64 / 1024.0 / 1024.0;
    let max = std::fs::read_to_string(p.cg_path.join("memory.max")).unwrap_or_default();
    eprintln!(
        "memory bomb: peak={peak_mb:.0} MiB (live sample {:.0} MiB) max={} oom_kills={} \
         (live sample {}) status={:?} host_before={host_before:.0} MiB after={host_after:.0} MiB",
        p.cg.peak_bytes() as f64 / 1024.0 / 1024.0,
        max.trim(),
        snap_oom,
        p.cg.oom_kills(),
        p.status,
    );
    eprintln!("guest stdout: {:?}", p.stdout);
    // The cap must bite: either the memcg OOM killer killed the bomb (the
    // common case) or the charge failed and the allocator returned NULL (the
    // kernel refusing memory is a denial either way); both are bounded. The
    // OOM counter is read from the in-scope observer (W53), not from a sampler
    // that can lose it to the scope teardown.
    assert!(
        snap_oom >= 1 || p.stdout.contains("malloc failed"),
        "the cgroup cap must stop the bomb (peak {peak_mb:.0} MiB, oom_kills {snap_oom}, \
         status {:?})",
        p.status
    );
    assert!(
        peak_mb <= 256.0,
        "the scope's peak ({peak_mb:.0} MiB) must stay near the 128 MiB cap"
    );
    assert!(
        host_min > 8192.0 && host_after > 8192.0,
        "the host must stay far from its 90% ceiling: min MemAvailable {host_min:.0} MiB \
         (before {host_before:.0}, after {host_after:.0})"
    );
    assert!(
        p.status.map(|s| !s.success()).unwrap_or(true),
        "an OOM-killed bomb must not exit successfully: {:?}",
        p.status
    );
    assert!(
        processes_in(&p.cg_path).is_empty(),
        "no process may survive the OOM-killed scope"
    );
}

// ---------------------------------------------------------------------------
// 4. The audit instrument's expectation: userfaultfd is the one deny that a
//    normal run actually hits (see results/art-runtime/S6.md)
// ---------------------------------------------------------------------------

#[test]
fn userfaultfd_is_denied_and_expected_in_a_normal_run() {
    // S6 bisected the deny list with `ACOS_ART_SECCOMP_AUDIT=kill:<nr>`: 323
    // (`userfaultfd`, ART's GC probe) is the only syscall a counter
    // `render-app` and a `launch --until application-created` run attempt.
    assert!(
        acos_art::seccomp::deny_list().contains(&323u32),
        "userfaultfd must stay denied (and is the audited expected denial)"
    );
    // Everything ART needs must remain outside the list.
    for allowed in ["futex", "mmap", "mprotect", "memfd_create", "clone3"] {
        let _ = allowed;
    }
    for nr in [
        libc::SYS_futex as u32,
        libc::SYS_mmap as u32,
        libc::SYS_mprotect as u32,
        libc::SYS_memfd_create as u32,
        libc::SYS_clone as u32,
    ] {
        assert!(
            !acos_art::seccomp::deny_list().contains(&nr),
            "syscall {nr} is needed by ART and must stay allowed"
        );
    }
}
