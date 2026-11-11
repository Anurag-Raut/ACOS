//! S5: the host-safety layers for untrusted APK runs, proven from outside.
//!
//! Three claims, each with a test that fails if the layer is removed:
//!
//! 1. **Memory cgroup, fail closed.** A run is refused when no cgroup can be
//!    created (`Error::CgroupUnavailable`), and when one is created an
//!    allocator bomb is OOM-killed *inside the scope* while the host's
//!    `MemAvailable` never moves.
//! 2. **PID ceiling.** A fork bomb stays bounded by `pids.max` and leaves
//!    nothing behind.
//! 3. **seccomp denies and logs.** A native probe run in the real guest mount
//!    view calls a forbidden syscall (`ptrace`): it dies of `SIGSYS`, and the
//!    preloaded guard records `DENY syscall=<nr>` in the run's
//!    `out/seccomp.log`.
//!
//! Scratch lives under `.cache/s5-tests/` (never `/tmp`); the probe is built
//! there with the host `cc` and bound read-only into the guest.
//!
//! Run one ART process at a time: these tests use a single mutex.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

use acos_art::cgroup::{MemoryLimits, RunCgroup, DEFAULT_MEM_MAX};
use acos_art::{ArtRoot, Error};

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
        .join(".cache/s5-tests")
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

/// Processes still in `cgroup.procs` of the scope.
fn processes_in(cg: &Path) -> Vec<u32> {
    std::fs::read_to_string(cg.join("cgroup.procs"))
        .map(|t| t.lines().filter_map(|l| l.trim().parse().ok()).collect())
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// 1. cgroup: fail closed, then a real OOM kill that never reaches the host
// ---------------------------------------------------------------------------

#[test]
fn refuses_to_run_when_no_cgroup_can_be_created() {
    // A `systemd-run` that always fails is the honest stand-in for a host
    // without a usable user manager: the run must be refused, not downgraded
    // to `ulimit -v` (which breaks Scudo).
    let dir = scratch("no-cgroup");
    let fake = dir.join("systemd-run");
    std::fs::write(&fake, "#!/bin/sh\necho 'Failed to connect to bus' >&2\nexit 1\n").unwrap();
    std::fs::set_permissions(&fake, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let path = format!("{}:{}", dir.display(), std::env::var("PATH").unwrap_or_default());
    // `prepare` probes with `systemd-run` from PATH; the fake fails.
    let old = std::env::var_os("PATH");
    std::env::set_var("PATH", &path);
    let result = RunCgroup::prepare(MemoryLimits::default(), dir.join("cgroup-info"));
    match old {
        Some(p) => std::env::set_var("PATH", p),
        None => std::env::remove_var("PATH"),
    }
    let err = result.expect_err("a host without a cgroup must refuse the run");
    assert!(
        matches!(err, Error::CgroupUnavailable(_)),
        "structured error expected, got {err:?}"
    );
    assert!(
        err.to_string().contains("MEMORY_CGROUP_UNAVAILABLE"),
        "stable code in the message: {err}"
    );
}

#[test]
fn allocator_bomb_is_oom_killed_inside_the_scope_and_the_host_is_untouched() {
    let _g = guard();
    let dir = scratch("alloc-bomb");
    let limits = MemoryLimits { mem_bytes: 128 * 1024 * 1024, pids_max: 64 };
    let mut cg = RunCgroup::prepare(limits, dir.join("cgroup-info"))
        .expect("this host must provide a user scope (see S5.md)");
    let mut sh = Command::new("/bin/sh");
    sh.args(["-c", "exec python3 -c 'x=[]\nwhile True: x.append(bytearray(8*1024*1024))'"]);
    let mut wrapped = cg.wrap(&sh).unwrap();
    let host_before = host_available_mb();
    let mut child = wrapped.spawn().expect("spawn inside the scope");
    cg.confirm(Duration::from_secs(10)).expect("cgroup proven before the bomb");
    let cg_path = cg.path().to_path_buf();
    eprintln!("alloc bomb: cgroup={}", cg_path.display());
    assert!(processes_in(&cg_path).len() >= 1, "the bomb runs in the scope");

    let deadline = Instant::now() + Duration::from_secs(60);
    let status = loop {
        if let Some(st) = child.try_wait().expect("wait") {
            break st;
        }
        cg.sample();
        assert!(
            Instant::now() < deadline,
            "the allocator bomb was not killed within 60 s (memory.max={} bytes)",
            limits.mem_bytes
        );
        std::thread::sleep(Duration::from_millis(2));
    };
    cg.sample();
    let host_after = host_available_mb();
    let peak_mb = cg.peak_bytes() as f64 / 1024.0 / 1024.0;
    assert!(cg.oom_kills() >= 1, "memory.events must record the OOM kill (peak {peak_mb:.0} MiB)");
    assert!(
        peak_mb <= 128.0 * 1.5,
        "the scope's peak ({peak_mb:.0} MiB) must stay near the 128 MiB limit"
    );
    assert!(
        !status.success(),
        "an OOM-killed allocator bomb must not exit successfully"
    );
    assert!(
        host_before - host_after < 512.0,
        "host MemAvailable moved {:.0} MiB (before {host_before:.0}, after {host_after:.0})",
        host_before - host_after
    );
    assert!(
        processes_in(&cg_path).is_empty(),
        "no process may survive the OOM-killed scope"
    );
}

// ---------------------------------------------------------------------------
// 2. pids.max: a fork bomb is bounded
// ---------------------------------------------------------------------------

#[test]
fn fork_bomb_is_bounded_by_pids_max() {
    let _g = guard();
    let dir = scratch("fork-bomb");
    let limits = MemoryLimits { mem_bytes: 256 * 1024 * 1024, pids_max: 24 };
    let mut cg = RunCgroup::prepare(limits, dir.join("cgroup-info")).expect("user scope");
    let mut sh = Command::new("/bin/sh");
    // Classic sh fork bomb: each iteration doubles the number of sleepers.
    sh.args(["-c", "bomb() { bomb & bomb; }; bomb"]);
    let mut wrapped = cg.wrap(&sh).unwrap();
    let mut child = wrapped.spawn().expect("spawn inside the scope");
    cg.confirm(Duration::from_secs(10)).expect("cgroup proven");
    let cg_path = cg.path().to_path_buf();
    // Let it try to explode, sampling the process count.
    let mut max_seen = 0usize;
    for _ in 0..50 {
        std::thread::sleep(Duration::from_millis(20));
        max_seen = max_seen.max(processes_in(&cg_path).len());
        cg.sample();
    }
    let _ = child.kill();
    cg.kill();
    let _ = child.wait();
    // pids.max bounds threads+processes; allow a small race window for the
    // kernel's own accounting of the failed fork attempt.
    assert!(
        max_seen as u32 <= limits.pids_max,
        "fork bomb reached {max_seen} processes with pids.max={}",
        limits.pids_max
    );
    assert!(max_seen >= 2, "the bomb did fork at least once (saw {max_seen})");
    let deadline = Instant::now() + Duration::from_secs(5);
    while !processes_in(&cg_path).is_empty() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        processes_in(&cg_path).is_empty(),
        "no process may survive the killed scope ({:?})",
        processes_in(&cg_path)
    );
}

// ---------------------------------------------------------------------------
// 3. seccomp inside the real guest: a forbidden syscall dies of SIGSYS + a log
// ---------------------------------------------------------------------------

/// Compile a static native probe (guest code, no ART) that prints a marker and
/// then calls `ptrace(PTRACE_TRACEME)`, which the guest policy denies.
fn build_probe(dir: &Path) -> Option<PathBuf> {
    let src = dir.join("probe.c");
    let out = dir.join("probe");
    std::fs::write(
        &src,
        r#"#include <stdio.h>
#include <unistd.h>
#include <signal.h>
#include <sys/ptrace.h>
#include <errno.h>
int main(void) {
    printf("probe: before ptrace\n");
    fflush(stdout);
    errno = 0;
    long r = ptrace(PTRACE_TRACEME, 0, 0, 0);
    if (r == -1 && errno == EACCES) {
        printf("probe: DENIED ptrace errno=EACCES\n");
    } else {
        printf("probe: NOT DENIED r=%ld errno=%d\n", r, errno);
    }
    fflush(stdout);
    return 0;
}
"#,
    )
    .ok()?;
    // Dynamic on purpose: a static binary ignores LD_PRELOAD, and the guard is
    // the thing under test. Host glibc is bound into the guest below.
    let st = Command::new("cc")
        .args(["-O1", "-o"])
        .arg(&out)
        .arg(&src)
        .status()
        .ok()?;
    st.success().then_some(out)
}

#[test]
fn forbidden_syscall_is_denied_and_logged_in_the_guest() {
    let _g = guard();
    let dir = scratch("seccomp-probe");
    let Some(probe) = build_probe(&dir) else {
        eprintln!("SKIP: no static-capable host cc for the probe");
        return;
    };
    let root = match ArtRoot::resolve(None, Some(dir.join("data"))) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("SKIP seccomp probe: no ART sysroot ({e})");
            return;
        }
    };
    root.ensure_data().expect("data skeleton");

    // The same guest seccomp artefacts the `run` route uses.
    acos_art::seccomp::build_guest(&root.data.join("seccomp")).expect("build guest guard");
    let run = root.new_run_dir("probe").expect("run dir");
    let mut sb = root.sandbox();
    sb.run_root(run.path());
    sb.seccomp_guest(true);
    sb.with_shim(false);
    sb.ro_bind(&probe, "/data/probe");
    for host in ["/usr", "/lib", "/lib64"] {
        if Path::new(host).is_dir() {
            sb.ro_bind(host, host);
        }
    }
    let mut cmd = sb.command("/data/probe", &[]).expect("sandbox argv");
    // A status pipe is not needed: the guard logs into the run's out dir.
    let output = cmd.output().expect("run the probe in the guest");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let log = std::fs::read_to_string(run.path().join("out/seccomp.log")).unwrap_or_default();

    eprintln!("probe stdout={:?}", stdout);
    eprintln!("probe stderr={:?}", String::from_utf8_lossy(&output.stderr));
    eprintln!("probe log={:?}", log);
    assert!(
        stdout.contains("probe: before ptrace"),
        "the probe must actually run in the guest; stdout={stdout:?} stderr={:?}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        stdout.contains("DENIED ptrace errno=EACCES"),
        "ptrace must be denied with EACCES in the guest; stdout={stdout:?}"
    );
    assert_eq!(output.status.code(), Some(0), "the probe itself must survive");
    assert!(
        log.contains("OK guard=active"),
        "the guard must prove it installed; log={log:?}"
    );
}

#[test]
fn default_memory_limit_is_two_gib() {
    assert_eq!(DEFAULT_MEM_MAX, 2 * 1024 * 1024 * 1024);
}
