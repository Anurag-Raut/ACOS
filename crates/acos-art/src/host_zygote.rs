//! ZM2 prototype C — a **warm host template** that `fork()`s one `acos-art`
//! launcher per session.
//!
//! `ACOS_HOST_ZYGOTE_PLAN=<plan.json>` (unset by default) makes one
//! `acos-art launch` process:
//!
//! 1. do the session-independent prefix of [`crate::launch::launch`] once
//!    (data root/linkerconfig/classpath, the guest dex and the three shims),
//! 2. pre-warm the process-global caches the host services use
//!    (`acos_services::keychars::stock`, `acos_services::parcelable::SPECS`
//!    is already static) and build **one empty `ServiceHost`** with the full
//!    standard service set installed but *no* package installed,
//! 3. `fork()` one child per session in the plan. Every child gets its own
//!    fds 0/1/2 (the plan names inherited descriptors), installs its own APK
//!    into its CoW copy of the host, and runs the rest of the normal
//!    `launch` path: own run root, own sockets, own logd, own peer thread, own
//!    guest. The parent only `wait4`s.
//!
//! Why this is safe to share: nothing session-specific exists before the fork,
//! and a fork child writes a shared page only by taking a private CoW copy of
//! it (the kernel does that per process). Binary text, read-only tables, the
//! pre-warmed heap and the empty host's object graph are therefore physically
//! one copy on the host and private only where a session actually writes.
//! Failure domains stay per process: a panic in one child kills that launcher
//! and its guest, never a sibling.
//!
//! This is research code behind an env gate. With the variable unset, `launch`
//! is exactly the production path (same calls, same order, same argv).

use crate::launch::{LaunchOpts, LaunchResult, PrebuiltHost, SessionShims};
use crate::{stage, ArtRoot, Error, Result};
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Env gate: a plan directory makes this process the warm host template.
pub const ENV_PLAN: &str = "ACOS_HOST_ZYGOTE_PLAN";

/// The plan file's name inside `ACOS_HOST_ZYGOTE_PLAN`.
pub const PLAN_FILE: &str = "plan.json";

/// One session the template must fork.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionSlot {
    /// Stable session index (the driver's tag).
    pub tag: usize,
    /// APK this session launches.
    pub apk: PathBuf,
    /// `--until` stop point.
    pub until: String,
    /// `--timeout` seconds (the session's wall budget).
    pub timeout_s: u64,
    /// `--out` directory (this session's evidence root).
    pub out: PathBuf,
    /// Descriptor to `dup2` onto fd 0 in the child.
    #[serde(default)]
    pub stdin_fd: Option<i32>,
    /// Descriptor to `dup2` onto fd 1 in the child.
    #[serde(default)]
    pub stdout_fd: Option<i32>,
    /// Descriptor to `dup2` onto fd 2 in the child.
    #[serde(default)]
    pub stderr_fd: Option<i32>,
    /// Extra environment for the child (guest logs, traces).
    #[serde(default)]
    pub env: Vec<(String, String)>,
}

/// The whole fork plan.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HostPlan {
    /// Number of sessions (must equal `sessions.len()`).
    pub n: usize,
    /// One slot per session.
    pub sessions: Vec<SessionSlot>,
}

/// Build the one generic host every child starts from.
///
/// `ServiceHost::new()` + `registry::install_with_handles` is the whole
/// session-independent service graph: empty state, no APK, no thread. A child
/// installs its own package into its private CoW copy afterwards.
pub fn make_host() -> PrebuiltHost {
    let mut host = acos_services::ServiceHost::new();
    let handles = acos_services::registry::install_with_handles(&mut host);
    (host, handles)
}

/// Pre-warm the process-global host caches so their pages are shared by CoW.
///
/// Only things that are identical for every session belong here.
fn prewarm() {
    // `StockMaps`: two `/system/usr/keychars/*.kcm` parses (a session used to
    // redo this in its own heap). The maps are immutable after `get_or_init`.
    let _ = acos_services::keychars::stock();
}

fn read_plan(dir: &Path) -> Result<HostPlan> {
    let path = dir.join(PLAN_FILE);
    let text = std::fs::read_to_string(&path)
        .map_err(|e| Error::Layout(format!("host zygote: {}: {e}", path.display())))?;
    let plan: HostPlan = serde_json::from_str(&text)
        .map_err(|e| Error::Layout(format!("host zygote: {}: {e}", path.display())))?;
    if plan.n != plan.sessions.len() || plan.n == 0 {
        return Err(Error::Layout(format!(
            "host zygote: plan n={} but {} sessions",
            plan.n,
            plan.sessions.len()
        )));
    }
    Ok(plan)
}

fn redirect_template_stdio(dir: &Path) -> Result<()> {
    let path = dir.join("template.log");
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(Error::Io)?;
    use std::os::fd::AsRawFd;
    let fd = file.as_raw_fd();
    unsafe {
        libc::dup2(fd, 1);
        libc::dup2(fd, 2);
    }
    Ok(())
}

fn child_stdio(slot: &SessionSlot) {
    for (want, have) in [(0, slot.stdin_fd), (1, slot.stdout_fd), (2, slot.stderr_fd)] {
        if let Some(fd) = have {
            if fd != want {
                unsafe {
                    libc::dup2(fd, want);
                }
            }
        }
    }
}

fn close_other_fds(plan: &HostPlan, keep: usize) {
    for (k, slot) in plan.sessions.iter().enumerate() {
        if k == keep {
            continue;
        }
        for fd in [slot.stdin_fd, slot.stdout_fd, slot.stderr_fd]
            .into_iter()
            .flatten()
        {
            if fd > 2 {
                unsafe {
                    libc::close(fd);
                }
            }
        }
    }
    // A child's own inherited descriptors are duplicated onto 0/1/2; the
    // originals (and the template's copy of every other session's ends) are
    // closed so the driver sees EOF when this child goes away.
    let slot = &plan.sessions[keep];
    for fd in [slot.stdin_fd, slot.stdout_fd, slot.stderr_fd]
        .into_iter()
        .flatten()
    {
        if fd > 2 {
            unsafe {
                libc::close(fd);
            }
        }
    }
}

fn close_plan_fds(plan: &HostPlan) {
    for slot in &plan.sessions {
        for fd in [slot.stdin_fd, slot.stdout_fd, slot.stderr_fd]
            .into_iter()
            .flatten()
        {
            if fd > 2 {
                unsafe {
                    libc::close(fd);
                }
            }
        }
    }
}

/// Fork one launcher per planned session and wait for all of them.
///
/// Never returns on success: the template `_exit`s with a code derived from the
/// children's statuses. `Err` is only for a template-side setup failure before
/// any child exists (so the driver sees a structured launcher error).
pub fn run_template(root: &ArtRoot, plan_dir: &Path, shims: SessionShims) -> Result<LaunchResult> {
    if std::env::var_os("ACOS_ZYGOTE_SESSION").is_some()
        || std::env::var_os("ACOS_ZYGOTE_APP").is_some()
    {
        return Err(Error::Layout(
            "host zygote: ACOS_HOST_ZYGOTE_PLAN cannot be combined with the ART \
             zygote seams (ACOS_ZYGOTE_SESSION / ACOS_ZYGOTE_APP); prototype C \
             forks the *host*, not the guest"
                .into(),
        ));
    }
    let plan = read_plan(plan_dir)?;
    redirect_template_stdio(plan_dir)?;
    stage("host-zygote-prepare");
    prewarm();
    let mut prebuilt = Some(make_host());
    stage("host-zygote-fork");

    let mut pids: Vec<i32> = Vec::new();
    for (k, slot) in plan.sessions.iter().enumerate() {
        // SAFETY: the template is thread-quiescent here for every lock that
        // matters (the only pre-existing thread is the RSS watchdog, which
        // sleeps and holds no allocator/stdout lock). Every child immediately
        // runs the normal single-threaded launcher entry.
        let pid = unsafe { libc::fork() };
        if pid == 0 {
            child_stdio(slot);
            close_other_fds(&plan, k);
            std::env::remove_var("ACOS_HOST_ZYGOTE_PLAN");
            for (name, value) in &slot.env {
                std::env::set_var(name, value);
            }
            // The warm-zygote child launches with the same display geometry the
            // caller asked for (the engine forwards it as `ACOSLAUNCH_*`); the
            // defaults keep the reference-device profile.
            let geometry = |name: &str, fallback: u32| -> u32 {
                std::env::var(name)
                    .ok()
                    .and_then(|v| v.trim().parse().ok())
                    .unwrap_or(fallback)
            };
            let child_opts = LaunchOpts {
                apk: slot.apk.clone(),
                until: slot.until.clone(),
                out: Some(slot.out.clone()),
                timeout: Duration::from_secs(slot.timeout_s.max(1)),
                width: geometry("ACOSLAUNCH_WIDTH", 1080),
                panel_height: geometry("ACOSLAUNCH_PANEL_HEIGHT", 2400),
                density_dpi: geometry("ACOSLAUNCH_DENSITY", 420),
                status_inset: geometry("ACOSLAUNCH_STATUS_INSET", 128),
                nav_inset: geometry("ACOSLAUNCH_NAV_INSET", 63),
                insets: std::env::var("ACOSLAUNCH_WM_INSETS").unwrap_or_else(|_| {
                    acos_services::wms::DEFAULT_INSETS_MODE.as_str().to_string()
                }),
                animations: std::env::var("ACOS_ANIMATIONS")
                    .ok()
                    .and_then(|value| crate::launch::Animations::parse(&value).ok())
                    .unwrap_or_default(),
                // MPROC1: a planned session's extra APKs, as the engine passes
                // them to a direct launcher (`std::env::join_paths` spelling).
                // The plan's per-slot `env` is the documented way to set it.
                extra_apks: std::env::var_os(crate::launch::ENV_EXTRA_APKS)
                    .map(|value| std::env::split_paths(&value).collect())
                    .unwrap_or_default(),
            };
            let host = prebuilt.take();
            let code = match crate::launch::launch_session(root, &child_opts, host, &shims) {
                Ok(result) => {
                    if result.outcome == crate::Outcome::Ok {
                        0
                    } else {
                        1
                    }
                }
                Err(e) => {
                    eprintln!("acos-art: host-zygote session {}: {e}", slot.tag);
                    1
                }
            };
            let _ = std::io::stdout().flush();
            // `_exit`: no parent destructors, no shared-buffer double flush.
            unsafe { libc::_exit(code) }
        } else if pid < 0 {
            let e = std::io::Error::last_os_error();
            close_plan_fds(&plan);
            for done in &pids {
                unsafe {
                    libc::kill(*done, libc::SIGKILL);
                }
            }
            return Err(Error::Io(e));
        }
        pids.push(pid);
    }
    // The template's copies of the session descriptors must go, or the driver's
    // read ends never see EOF when a child exits.
    close_plan_fds(&plan);

    let mut failed = false;
    for pid in &pids {
        let mut status: libc::c_int = 0;
        loop {
            let r = unsafe { libc::waitpid(*pid, &mut status, 0) };
            if r == *pid {
                break;
            }
            if r < 0 && std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted {
                failed = true;
                break;
            }
        }
        if !libc::WIFEXITED(status) || libc::WEXITSTATUS(status) != 0 {
            failed = true;
        }
    }
    unsafe { libc::_exit(if failed { 1 } else { 0 }) }
}
