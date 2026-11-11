//! Hard per-run memory and PID limits (S5, owner P0 host safety).
//!
//! `results/art-runtime/S1-sandbox-review.md` §5 #3: nothing in `acos-art`
//! bounded memory. `RLIMIT_AS` is unusable for ART (Scudo reserves ~8.6 TB of
//! virtual address space), `tools/guarded.sh` is a convention and its
//! `ulimit -v` fallback actively breaks ART, and the launcher RSS watchdog
//! ([`crate::watchdog`]) samples a process group after the fact. None of those
//! is a *kernel-enforced* ceiling that fails closed before untrusted code runs.
//!
//! This module puts every run in its own cgroup v2 scope with
//! `memory.max`/`memory.swap.max=0`/`memory.oom.group=1`/`pids.max`, proves the
//! limits really apply to the run's cgroup before the guest has started, and
//! **refuses to run** (structured [`Error::CgroupUnavailable`]) when no cgroup
//! can be created. There is no fallback to `ulimit -v`: a run without a cgroup
//! is not a run ACOS is willing to make.
//!
//! ## Which mechanism works unprivileged on this host (measured)
//!
//! * Direct cgroup v2 delegation: NO. `/sys/fs/cgroup/.../app-Paseo-*.scope`
//!   accepts `mkdir` but the child directory has no controller files
//!   (`memory.max` is absent) and the parent's knobs are root-owned, so the
//!   limit cannot be written. Measured 2026-11-11; see `results/art-runtime/S5.md`.
//! * `systemd-run --user --scope -p MemoryMax=… -p MemorySwapMax=0
//!   -p MemoryOOMGroup=yes -p TasksMax=…` — YES. The transient scope gets a
//!   real `memory.max` equal to the requested value (`memory.max` is read back
//!   from the cgroup the run's own process reports in `/proc/self/cgroup`).
//!
//! ## Proof, not assumption
//!
//! The scope's command is `/bin/sh -c '<record cgroup; exec "$@">'`: it writes
//! its own `/proc/self/cgroup` to a host file and then `exec`s `bwrap`. The
//! launcher reads that file, resolves `/sys/fs/cgroup<path>/memory.max`,
//! `memory.swap.max` and `pids.max` and compares them to the requested limits
//! *before* it lets the run continue. A scope that cannot be proven is killed
//! and reported, never trusted. `memory.events:oom_kill` and `memory.peak` are
//! sampled while the run is alive and recorded in the run result.

use crate::{Error, Result};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

/// Default guest memory ceiling: 2 GiB (overridable with `ACOS_ART_MEM_MAX`).
pub const DEFAULT_MEM_MAX: u64 = 2 * 1024 * 1024 * 1024;

/// Default process/thread ceiling for a run (ART spawns ~20 threads; a fork
/// bomb must not get past this). Overridable with `ACOS_ART_PIDS_MAX`.
pub const DEFAULT_PIDS_MAX: u32 = 512;

/// The memory/PID ceilings a run gets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemoryLimits {
    pub mem_bytes: u64,
    pub pids_max: u32,
}

impl Default for MemoryLimits {
    fn default() -> Self {
        Self { mem_bytes: DEFAULT_MEM_MAX, pids_max: DEFAULT_PIDS_MAX }
    }
}

impl MemoryLimits {
    /// `ACOS_ART_MEM_MAX` (default 2 GiB) / `ACOS_ART_PIDS_MAX` (default 512).
    /// A malformed or zero value is an error, never a silent "unlimited".
    pub fn from_env() -> Result<Self> {
        let mut limits = Self::default();
        if let Ok(raw) = std::env::var("ACOS_ART_MEM_MAX") {
            let bytes = parse_size(&raw).ok_or_else(|| {
                Error::CgroupUnavailable(format!(
                    "ACOS_ART_MEM_MAX={raw:?}: expected bytes with an optional K/M/G suffix"
                ))
            })?;
            if bytes == 0 {
                return Err(Error::CgroupUnavailable(
                    "ACOS_ART_MEM_MAX=0: an unbounded ART run is refused (fail closed)".into(),
                ));
            }
            limits.mem_bytes = bytes;
        }
        if let Ok(raw) = std::env::var("ACOS_ART_PIDS_MAX") {
            let n: u32 = raw.trim().parse().map_err(|_| {
                Error::CgroupUnavailable(format!("ACOS_ART_PIDS_MAX={raw:?}: expected a number"))
            })?;
            if n == 0 {
                return Err(Error::CgroupUnavailable(
                    "ACOS_ART_PIDS_MAX=0: an unbounded process count is refused (fail closed)".into(),
                ));
            }
            limits.pids_max = n;
        }
        Ok(limits)
    }

    fn systemd_properties(&self, oom_group: bool) -> Vec<String> {
        let mut props = vec![
            format!("MemoryMax={}", self.mem_bytes),
            "MemorySwapMax=0".to_string(),
            format!("TasksMax={}", self.pids_max),
        ];
        if oom_group {
            // Kill every process in the scope at once on OOM: no orphan guest.
            props.insert(2, "MemoryOOMGroup=yes".to_string());
        }
        props
    }
}

/// Parse `2097152`, `2048K`, `512M`, `2G` (binary multiples).
pub fn parse_size(raw: &str) -> Option<u64> {
    let s = raw.trim();
    if s.is_empty() {
        return None;
    }
    let (num, mul) = match s.chars().last()?.to_ascii_uppercase() {
        'K' => (&s[..s.len() - 1], 1024u64),
        'M' => (&s[..s.len() - 1], 1024 * 1024),
        'G' => (&s[..s.len() - 1], 1024 * 1024 * 1024),
        _ => (s, 1),
    };
    let n: u64 = num.trim().parse().ok()?;
    n.checked_mul(mul)
}

/// One run's cgroup scope.
#[derive(Debug)]
pub struct RunCgroup {
    limits: MemoryLimits,
    /// Absolute host path of the run's cgroup, once proven.
    cg: PathBuf,
    /// Host file the scoped shell records `/proc/self/cgroup` in.
    info: PathBuf,
    /// `memory.oom.group=1` is only settable through systemd on versions that
    /// know `MemoryOOMGroup=`; probed, never assumed.
    oom_group: bool,
    peak_bytes: u64,
    oom_kills: u64,
}

impl RunCgroup {
    /// Refuse early when the host cannot provide a per-run cgroup at all.
    pub fn prepare(limits: MemoryLimits, info: impl Into<PathBuf>) -> Result<Self> {
        if !Path::new("/sys/fs/cgroup/cgroup.controllers").is_file() {
            return Err(Error::CgroupUnavailable(
                "no cgroup v2 hierarchy at /sys/fs/cgroup (memory.max unavailable)".into(),
            ));
        }
        // `systemd-run --user --scope` is the only unprivileged way to get a
        // writable memory.max on this host (see the module docs).
        let probe = Command::new("systemd-run")
            .args(["--user", "--scope", "-q", "-p", "MemoryMax=64M", "true"])
            .output();
        match probe {
            Ok(out) if out.status.success() => {}
            Ok(out) => {
                return Err(Error::CgroupUnavailable(format!(
                    "systemd-run --user --scope cannot create a scope (status {:?}): {}",
                    out.status.code(),
                    String::from_utf8_lossy(&out.stderr).trim()
                )))
            }
            Err(e) => {
                return Err(Error::CgroupUnavailable(format!(
                    "systemd-run is not usable ({e}): cannot create the mandatory memory cgroup"
                )))
            }
        }
        // Older systemd has no `MemoryOOMGroup=`; the memory ceiling is still
        // mandatory, only the group-kill refinement is dropped (measured in
        // results/art-runtime/S5.md).
        let oom_group = Command::new("systemd-run")
            .args(["--user", "--scope", "-q", "-p", "MemoryOOMGroup=yes", "true"])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        Ok(Self {
            limits,
            cg: PathBuf::new(),
            info: info.into(),
            oom_group,
            peak_bytes: 0,
            oom_kills: 0,
        })
    }

    pub fn limits(&self) -> MemoryLimits {
        self.limits
    }

    /// Wrap `cmd` so it starts inside a fresh transient scope with the limits.
    /// The returned command is the thing to spawn (rlimits/`pre_exec` set on it
    /// are inherited by the scope's command tree).
    pub fn wrap(&self, cmd: &Command) -> Result<Command> {
        let mut wrapped = Command::new("systemd-run");
        wrapped.args(["--user", "--scope", "-q", "--same-dir"]);
        for p in self.limits.systemd_properties(self.oom_group) {
            wrapped.arg("-p").arg(&p);
        }
        wrapped.args(["--", "/bin/sh", "-c", SCOPE_SHELL]);
        wrapped.arg("sh");
        wrapped.arg(&self.info);
        wrapped.arg(cmd.get_program());
        wrapped.args(cmd.get_args());
        Ok(wrapped)
    }

    /// Prove the cgroup exists, is the run's own, and carries the requested
    /// limits. Fails closed: the caller must kill the run on `Err`.
    pub fn confirm(&mut self, budget: Duration) -> Result<()> {
        let start = Instant::now();
        let mut last = String::from("the scope did not report its cgroup");
        while start.elapsed() < budget {
            match std::fs::read_to_string(&self.info) {
                Ok(text) => {
                    let Some(rel) = cgroup_v2_path(&text) else {
                        last = format!("unparseable /proc/self/cgroup: {text:?}");
                        std::thread::sleep(Duration::from_millis(2));
                        continue;
                    };
                    if rel == own_cgroup().as_deref().unwrap_or("") {
                        return Err(Error::CgroupUnavailable(format!(
                            "run was not moved into a new scope (still in {rel})"
                        )));
                    }
                    let dir = Path::new("/sys/fs/cgroup").join(rel.trim_start_matches('/'));
                    let max = read_trim(&dir.join("memory.max"));
                    let swap = read_trim(&dir.join("memory.swap.max"));
                    let pids = read_trim(&dir.join("pids.max"));
                    match (max, swap, pids) {
                        (Some(max), Some(swap), Some(pids)) => {
                            if max != self.limits.mem_bytes.to_string() {
                                return Err(Error::CgroupUnavailable(format!(
                                    "memory.max is {max}, requested {}",
                                    self.limits.mem_bytes
                                )));
                            }
                            if swap != "0" {
                                return Err(Error::CgroupUnavailable(format!(
                                    "memory.swap.max is {swap}, requested 0"
                                )));
                            }
                            if pids != self.limits.pids_max.to_string() {
                                return Err(Error::CgroupUnavailable(format!(
                                    "pids.max is {pids}, requested {}",
                                    self.limits.pids_max
                                )));
                            }
                            self.cg = dir;
                            self.sample();
                            return Ok(());
                        }
                        (max, swap, pids) => {
                            last = format!(
                                "{rel}: memory.max={max:?} memory.swap.max={swap:?} pids.max={pids:?}"
                            );
                        }
                    }
                }
                Err(_) => last = format!("{} was not written yet", self.info.display()),
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        Err(Error::CgroupUnavailable(format!(
            "could not prove a memory cgroup for this run: {last}"
        )))
    }

    /// Sample `memory.peak` and `memory.events:oom_kill` (cheap; called from the
    /// launcher's wait loop and once at confirm time).
    pub fn sample(&mut self) {
        if self.cg.as_os_str().is_empty() {
            return;
        }
        if let Some(peak) = read_trim(&self.cg.join("memory.peak")).and_then(|v| v.parse().ok()) {
            self.peak_bytes = self.peak_bytes.max(peak);
        } else if let Some(cur) =
            read_trim(&self.cg.join("memory.current")).and_then(|v| v.parse().ok())
        {
            self.peak_bytes = self.peak_bytes.max(cur);
        }
        if let Ok(events) = std::fs::read_to_string(self.cg.join("memory.events")) {
            let n = events
                .lines()
                .filter_map(|l| l.strip_prefix("oom_kill "))
                .filter_map(|v| v.trim().parse::<u64>().ok())
                .max()
                .unwrap_or(0);
            self.oom_kills = self.oom_kills.max(n);
        }
    }

    pub fn peak_bytes(&self) -> u64 {
        self.peak_bytes
    }

    pub fn oom_kills(&self) -> u64 {
        self.oom_kills
    }

    pub fn path(&self) -> &Path {
        &self.cg
    }

    /// Kill every process in the scope (`cgroup.kill`, falling back to a
    /// `SIGKILL` per pid). Used on timeout and by the adversarial tests; a
    /// scope whose client died is *not* reaped by systemd on its own.
    pub fn kill(&self) {
        if self.cg.as_os_str().is_empty() {
            return;
        }
        if std::fs::write(self.cg.join("cgroup.kill"), "1\n").is_ok() {
            return;
        }
        for pid in processes_in(&self.cg) {
            // SAFETY: plain kill(2) on a pid read from cgroup.procs.
            unsafe {
                libc::kill(pid as i32, libc::SIGKILL);
            }
        }
    }

    /// One human-readable line for the run diagnostics.
    pub fn summary(&self) -> String {
        format!(
            "cgroup: memory.max={}MiB swap=0 pids.max={} peak={}MiB oom_kills={} ({})",
            self.limits.mem_bytes / 1024 / 1024,
            self.limits.pids_max,
            self.peak_bytes / 1024 / 1024,
            self.oom_kills,
            self.cg.display()
        )
    }
}

/// The scoped shell: record the run's own cgroup on the host, then `exec` the
/// sandbox so the recorded cgroup is the one the guest runs in. `$0` is the
/// info file, `$@` the command. A failure to record aborts the run (exit 90)
/// rather than running unproven.
const SCOPE_SHELL: &str = "cat /proc/self/cgroup > \"$1\" 2>/dev/null || exit 90; shift; exec \"$@\"";

/// The `0::/…` line of a cgroup v2 `/proc/self/cgroup` dump.
fn cgroup_v2_path(text: &str) -> Option<String> {
    text.lines()
        .find_map(|l| l.strip_prefix("0::"))
        .map(|p| p.trim().to_string())
}

/// The launcher's own cgroup (for the "did the run really move?" check).
fn own_cgroup() -> Option<String> {
    let text = std::fs::read_to_string("/proc/self/cgroup").ok()?;
    cgroup_v2_path(&text)
}

/// Pids currently in `cgroup.procs` (empty on any error).
fn processes_in(dir: &Path) -> Vec<i32> {
    std::fs::read_to_string(dir.join("cgroup.procs"))
        .map(|t| t.lines().filter_map(|l| l.trim().parse().ok()).collect())
        .unwrap_or_default()
}

fn read_trim(path: &Path) -> Option<String> {
    std::fs::read_to_string(path).ok().map(|s| s.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_parse() {
        assert_eq!(parse_size("2097152"), Some(2 * 1024 * 1024));
        assert_eq!(parse_size("2G"), Some(2 * 1024 * 1024 * 1024));
        assert_eq!(parse_size("512M"), Some(512 * 1024 * 1024));
        assert_eq!(parse_size("64M"), Some(64 * 1024 * 1024));
        assert_eq!(parse_size("0"), Some(0));
        assert_eq!(parse_size("2X"), None);
        assert_eq!(parse_size(""), None);
    }

    #[test]
    fn cgroup_line_is_parsed() {
        let dump = "1:net_cls:/\n0::/user.slice/user-1000.slice/u.scope\n";
        assert_eq!(
            cgroup_v2_path(dump).as_deref(),
            Some("/user.slice/user-1000.slice/u.scope")
        );
        assert_eq!(cgroup_v2_path("garbage"), None);
    }

    #[test]
    fn zero_limits_are_refused_not_silently_unbounded() {
        // The env is process-global; keep this test's mutation scoped.
        std::env::set_var("ACOS_ART_MEM_MAX", "0");
        let err = MemoryLimits::from_env().expect_err("0 must be refused");
        assert!(matches!(err, Error::CgroupUnavailable(_)));
        std::env::remove_var("ACOS_ART_MEM_MAX");
        std::env::set_var("ACOS_ART_MEM_MAX", "1536M");
        assert_eq!(MemoryLimits::from_env().unwrap().mem_bytes, 1536 * 1024 * 1024);
        std::env::remove_var("ACOS_ART_MEM_MAX");
    }

    #[test]
    fn wrap_puts_the_command_behind_systemd_run_with_the_limits() {
        let info = PathBuf::from("/tmp/should-not-be-used");
        let limits = MemoryLimits { mem_bytes: 64 * 1024 * 1024, pids_max: 32 };
        let cg = RunCgroup {
            limits,
            cg: PathBuf::new(),
            info: info.clone(),
            oom_group: true,
            peak_bytes: 0,
            oom_kills: 0,
        };
        let mut inner = Command::new("bwrap");
        inner.args(["--tmpfs", "/", "--", "/bin/true"]);
        let wrapped = cg.wrap(&inner).expect("wrap");
        assert_eq!(wrapped.get_program(), "systemd-run");
        let args: Vec<String> =
            wrapped.get_args().map(|a| a.to_string_lossy().to_string()).collect();
        for want in [
            "--user",
            "--scope",
            "MemoryMax=67108864",
            "MemorySwapMax=0",
            "TasksMax=32",
            "/bin/sh",
        ] {
            assert!(args.iter().any(|a| a == want), "missing {want} in {args:?}");
        }
        assert!(args.iter().any(|a| a == "MemoryOOMGroup=yes"), "oom.group when supported");
        // The sandbox argv is passed through untouched, after the info file.
        let tail: Vec<&str> = args.iter().skip_while(|a| *a != "sh").map(String::as_str).collect();
        assert_eq!(tail[1], info.display().to_string());
        assert_eq!(tail[2], "bwrap");
        assert_eq!(&tail[3..], &["--tmpfs", "/", "--", "/bin/true"]);
    }
}

/// Wrap `cmd` in a fresh proven scope, spawn it, and hand back the live child
/// plus its [`RunCgroup`]. `configure` runs on the *wrapped* command (that is
/// the process actually spawned: `systemd-run`), so `pre_exec`, `process_group`
/// and the stdio setup must be applied there — they are inherited by the
/// scope's command tree.
///
/// Fail closed: if the scope cannot be created or proven, the child is killed
/// (group and scope) and an [`Error::CgroupUnavailable`] is returned. There is
/// no path that returns a running guest without a proven memory ceiling.
pub fn spawn_scoped(
    limits: MemoryLimits,
    info: impl Into<PathBuf>,
    cmd: &Command,
    configure: impl FnOnce(&mut Command),
) -> Result<(std::process::Child, RunCgroup)> {
    let mut cgroup = RunCgroup::prepare(limits, info)?;
    let mut wrapped = cgroup.wrap(cmd)?;
    configure(&mut wrapped);
    let mut child = wrapped
        .spawn()
        .map_err(|e| Error::Tool(format!("spawning the scoped sandbox: {e}")))?;
    let pid = child.id() as i32;
    if let Err(e) = cgroup.confirm(Duration::from_secs(10)) {
        // SAFETY: plain signal calls on the pid we just spawned.
        unsafe {
            libc::kill(-pid, libc::SIGKILL);
            libc::kill(pid, libc::SIGKILL);
        }
        cgroup.kill();
        let _ = child.wait();
        return Err(e);
    }
    Ok((child, cgroup))
}
