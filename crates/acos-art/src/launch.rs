//! `acos-art launch` — the real framework startup path on ART (P4a).
//!
//! Unlike `render-app` (which builds a `PhoneWindow` by hand), this command
//! runs `ActivityThread`'s own startup: `Looper.prepareMainLooper()`, a real
//! `new ActivityThread()`, `attach(false, startSeq)` (i.e.
//! `IActivityManager.attachApplication(IApplicationThread, long)`), then
//! `Looper.loop()`. The Rust `ServiceHost` behind `acos-binder-peer` answers
//! the attach and delivers the real `IApplicationThread.bindApplication`
//! transaction to the framework's `ApplicationThread` binder; the framework
//! builds `LoadedApk`, instantiates the APK's `Application` and runs
//! `Application.onCreate`.
//!
//! The marshalled `bindApplication` request is produced by the guest with the
//! framework's own `writeToParcel` methods and pushed to the peer-owned
//! `acos.launch` binder before `attach`; the peer replays those exact bytes as
//! a `BR_TRANSACTION` (P2f reverse delivery). No hand-written parcel codec.
//!
//! Two stop points (`--until`):
//!
//! * `application-created` — stop after the real `Application.onCreate`
//!   (the framework's own `new Instrumentation()`, `instrumentationName == null`);
//! * `activity-resumed` — then deliver one framework-marshalled
//!   `ClientTransaction` (`LaunchActivityItem` + `ResumeActivityItem`) as
//!   `IApplicationThread.scheduleTransaction`, so the real
//!   `ActivityThread` runs `Activity.onCreate`/`onResume`.

use crate::capture::{stream_cap_from_env, TailBuffer};
use crate::launcher::Outcome;
use crate::logd::Logd;
use crate::watchdog::{MemoryWatchdog, HOST_MEMORY_LIMIT};
use crate::{ArtRoot, Error, Result};
use acos_binder_peer::{accept, close_frame_fds, listen, read_frame, write_frame, Frame, Peer};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::Write;
use std::os::fd::RawFd;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// The guest class that performs the framework startup.
pub const GUEST_CLASS: &str = "dev.acos.art.LaunchMain";

/// Everything `launch` needs.
#[derive(Debug, Clone)]
pub struct LaunchOpts {
    /// Host path of the APK.
    pub apk: PathBuf,
    /// Stop point: `application-created` or `activity-resumed`.
    pub until: String,
    /// Output directory for the result + marker.
    pub out: Option<PathBuf>,
    /// Wall-clock budget for the guest.
    pub timeout: Duration,
}

impl Default for LaunchOpts {
    fn default() -> Self {
        Self {
            apk: PathBuf::new(),
            until: "application-created".into(),
            out: None,
            timeout: Duration::from_secs(120),
        }
    }
}

/// One forwarded service transaction.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ServiceCall {
    /// AIDL descriptor.
    pub descriptor: String,
    /// Transaction code.
    pub code: u32,
    /// Method name.
    pub method: String,
}

/// Structured outcome of one launch.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LaunchResult {
    /// `ok` only when Application.onCreate ran without a crash.
    pub outcome: Outcome,
    /// Stable failure code, if any.
    pub code: Option<String>,
    pub apk: String,
    pub package: String,
    pub until: String,
    pub application_created: bool,
    pub activity_launched: bool,
    /// Guest exit code, when the guest (or its bwrap wrapper) exited normally.
    /// `None` when it was killed by a signal or is still running (timeout).
    pub guest_exit: Option<i32>,
    /// Signal that terminated the guest, when it was killed. bwrap's
    /// `128+signal` exit-code remap is undone here, so a SIGSYS kill reads as
    /// `31`, not as an exit code of `159`.
    pub guest_signal: Option<i32>,
    /// Wall time from spawning the guest to observing its end (or timeout).
    pub wall_ms: u64,
    /// The APK's launcher activity (`aapt2 dump badging`), the class the
    /// `LaunchActivityItem` names.
    pub activity: Option<String>,
    /// The `application-created` marker, verbatim (real framework state).
    pub application_marker: Option<String>,
    /// The activity lifecycle marker, verbatim.
    pub activity_marker: Option<String>,
    /// The framework's own main-looper trace (`Looper.setMessageLogging`),
    /// filtered to the two messages this path dispatches.
    pub looper_trace: Vec<String>,
    /// `bindApplication` transport facts.
    pub bind_application: BindDelivery,
    /// Every service transaction forwarded while the launch ran.
    pub service_calls: Vec<ServiceCall>,
    /// Structured `UNSUPPORTED_ANDROID_API` records (never a fake success).
    pub unsupported: Vec<UnsupportedCall>,
    /// Guest stdout tail (markers).
    pub stdout: String,
    /// Guest stderr tail.
    pub stderr: String,
    /// ART/logd entries (exceptions, CHECKs) from the run.
    pub logs: Vec<crate::LogEntry>,
    /// Host-side notes: cgroup accounting, the guest guard/Landlock evidence,
    /// and any fail-closed reason (a dead guest is still a structured result,
    /// never a hang).
    pub notes: Vec<String>,
    pub marker: Option<String>,
}

/// The reverse-delivery facts for `bindApplication`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BindDelivery {
    pub request_bytes: Option<usize>,
    pub delivered: bool,
    pub app_thread_wire: Option<u32>,
    pub code: u32,
}

/// A structured unsupported transaction.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnsupportedCall {
    pub descriptor: String,
    pub code: u32,
    pub method: String,
    pub error: String,
}

/// Run the real `ActivityThread.main`-style startup until `Application.onCreate`.
pub fn launch(root: &ArtRoot, opts: &LaunchOpts) -> Result<LaunchResult> {
    if !opts.apk.is_file() {
        return Err(Error::Layout(format!("no APK at {}", opts.apk.display())));
    }
    if opts.until != "application-created" && opts.until != "activity-resumed" {
        return Err(Error::Layout(format!(
            "launch --until {} is not implemented (application-created, activity-resumed)",
            opts.until
        )));
    }
    root.ensure_data()?;
    root.ensure_linkerconfig(false)?;
    root.ensure_classpath_env(false)?;

    // Guest dex (contains AppMain, LaunchMain and LaunchMain$Instr), the
    // substrate JNI shim and the mini-root shim.
    let _guest = build_launch_guest(root)?;
    let substrate = crate::app::build_render_shim(root)?;
    let art_shim = crate::build_shim(&root.data.join("shim"))?;
    let driver = build_driver(root)?;
    // Private per-run data root (S4): app data, out staging and the importer
    // socket are this run's; everything else under /data is read-only.
    let run = root.new_run_dir("launch")?;

    let app_dir = root.data.join("app");
    fs::create_dir_all(&app_dir)?;
    let apk_name = opts
        .apk
        .file_name()
        .map(|f| f.to_string_lossy().to_string())
        .unwrap_or_else(|| "app.apk".to_string());
    let apk = app_dir.join(&apk_name);
    fs::copy(&opts.apk, &apk)?;
    let package = crate::app::apk_package(&opts.apk)?;

    let staging = run.dir("out").join("launch");
    fs::create_dir_all(&staging)?;
    for stale in [
        "launch.json",
        "application-created.marker",
        "activity-resumed.marker",
        "activity-not-resumed.marker",
        "unsupported.jsonl",
    ] {
        let _ = fs::remove_file(staging.join(stale));
    }
    fs::create_dir_all(run.dir("app-data").join(&package))?;
    let out = opts.out.clone().unwrap_or_else(|| staging.clone());
    fs::create_dir_all(&out)?;

    crate::Props::android34().write(&root.data.join(crate::props::PROPS_FILE))?;
    let logd = Logd::start(&run.dir("socket")).map_err(Error::Io)?;

    // --- peer (in-process, real ServiceHost) -------------------------------
    let sock = run.dir("sock").join("acos-launch.sock");
    let _ = fs::remove_file(&sock);
    let listener = listen(&sock.to_string_lossy()).map_err(Error::Io)?;
    // A guest that dies before connecting (fail-closed guard, SIGSYS audit)
    // leaves a blocking `accept()` with no peer to finish it. Make the listener
    // pollable and give the accepted connection a receive timeout, so the peer
    // thread can always observe the guest's end; `launch` then joins it with a
    // deadline instead of waiting forever.
    unsafe {
        let flags = libc::fcntl(listener, libc::F_GETFL);
        if flags < 0 || libc::fcntl(listener, libc::F_SETFL, flags | libc::O_NONBLOCK) < 0 {
            let e = std::io::Error::last_os_error();
            libc::close(listener);
            return Err(Error::Io(e));
        }
    }
    let stop = Arc::new(AtomicBool::new(false));
    let peer_stop = Arc::clone(&stop);
    let unsupported_log = staging.join("unsupported.jsonl");
    let (tx, rx) = mpsc::channel::<PeerSummary>();
    let peer_log = unsupported_log.clone();
    let apk_for_peer = opts.apk.clone();
    let peer_thread = std::thread::spawn(move || {
        let summary = (|| -> std::io::Result<PeerSummary> {
            let client = loop {
                if peer_stop.load(Ordering::Acquire) {
                    unsafe {
                        libc::close(listener);
                    }
                    return Ok(PeerSummary::default());
                }
                match accept(listener) {
                    Ok(fd) => break fd,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(2));
                    }
                    Err(e) => {
                        unsafe {
                            libc::close(listener);
                        }
                        return Err(e);
                    }
                }
            };
            unsafe {
                libc::close(listener);
                // A 100 ms receive timeout lets `serve` notice `stop` without
                // changing the frame protocol; a live guest simply continues.
                let tv = libc::timeval { tv_sec: 0, tv_usec: 100_000 };
                libc::setsockopt(
                    client,
                    libc::SOL_SOCKET,
                    libc::SO_RCVTIMEO,
                    &tv as *const _ as *const libc::c_void,
                    std::mem::size_of::<libc::timeval>() as libc::socklen_t,
                );
            }
            // The real ServiceHost, with its package manager pointed at the APK
            // being launched: `handleBindApplication` asks for its
            // `getApplicationInfo` and must get the real package, not the
            // hello-views fixture (and never a fabricated null).
            let mut host = acos_services::ServiceHost::standard();
            if let Err(e) = host.install_package_from_apk(&apk_for_peer) {
                eprintln!("acos-art: launch: install_package_from_apk failed: {e}");
            }
            let mut peer = Peer::with_host(host, Some(peer_log));
            let rc = serve(&mut peer, client, &peer_stop);
            unsafe {
                libc::close(client);
            }
            let summary = PeerSummary {
                calls: peer
                    .service_calls()
                    .iter()
                    .map(|c| ServiceCall {
                        descriptor: c.descriptor.clone(),
                        code: c.code,
                        method: c.method.clone(),
                    })
                    .collect(),
                unsupported: peer
                    .unsupported
                    .iter()
                    .map(|u| UnsupportedCall {
                        descriptor: u.descriptor.clone(),
                        code: u.code,
                        method: u.method.clone(),
                        error: u.error.clone(),
                    })
                    .collect(),
                bind_bytes: peer.launch_bind_len(),
                app_thread: peer.launch_app_thread(),
                delivered: peer.launch_delivered(),
                txn_bytes: peer.launch_txn_len(),
                txn_delivered: peer.launch_txn_delivered(),
                reverse_replies: peer.reverse_replies,
                reverse_failed: peer.reverse_failed,
                resource_limits: peer.resource_limits(),
            };
            let _ = rc;
            Ok(summary)
        })();
        let _ = tx.send(summary.unwrap_or(PeerSummary::default()));
    });

    // --- guest -------------------------------------------------------------
    let activity = if opts.until == "activity-resumed" {
        Some(apk_launcher_activity(&opts.apk)?)
    } else {
        None
    };
    let argv: Vec<String> = vec![
        "-cp".into(),
        "/data/app/launch-guest/classes.dex".into(),
        GUEST_CLASS.into(),
        format!("/data/app/{apk_name}"),
        package.clone(),
        "/data/out/launch".into(),
        opts.until.clone(),
        activity.clone().unwrap_or_default(),
    ];
    crate::seccomp::enable(root)?;
    let mut sb = root.sandbox();
    sb.run_root(run.path());
    sb.seccomp_guest(true);
    sb.with_shim(false);
    sb.ro_bind(&art_shim, "/data/shim/libartshim.so");
    sb.ro_bind(&substrate, "/data/app/libacosart.so");
    sb.ro_bind(&root.sysroot.join("shim/libacoshw.so"), "/data/libacoshw.so");
    sb.ro_bind(&driver, "/data/lib/libacos_binder_driver.so");
    sb.rw_bind(run.dir("socket"), "/dev/socket");
    // The importer socket is the only writable file the peer needs in /data.
    sb.rw_bind(&sock, "/data/acos-launch.sock");
    sb.env(
        "LD_PRELOAD",
        "/data/shim/libartshim.so:/data/app/libacosart.so:/data/libacoshw.so:/data/lib/libacos_binder_driver.so",
    );
    sb.env("ACOSART_LOAD", "libandroid_runtime.so");
    sb.env("E3G_SOCK", "/data/acos-launch.sock");
    if std::env::var_os("E3G_DEBUG").is_some() {
        sb.env("E3G_DEBUG", "1");
    }
    if std::env::var_os("ACOSLAUNCH_PROBE").is_some() {
        sb.env("ACOSLAUNCH_PROBE", "1");
    }
    // W5: keep the guest's main looper alive for N ms after `activity-resumed`
    // so the first traversal's draw (lockCanvas -> queueBuffer -> BLAST) can be
    // observed. Diagnostic only; the stop point is unchanged.
    if let Ok(hold) = std::env::var("ACOSLAUNCH_HOLD_MS") {
        sb.env("ACOSLAUNCH_HOLD_MS", &hold);
    }
    // W9: forward the guest-HW diagnostic switch so the gralloc shim's
    // `allocate`/`lock` line reaches the launch log (W7 could not see it because
    // the guest env is cleared and the launcher did not forward the variable).
    if std::env::var_os("ACOSART_HW_VERBOSE").is_some() {
        sb.env("ACOSART_HW_VERBOSE", "1");
    }
    let dalvik = "/system/apex/com.android.art/bin/dalvikvm64";
    let cmd = sb.command(dalvik, &argv)?;
    let started = Instant::now();
    // S5: a proven memory/pids cgroup scope, and the guest seccomp filter
    // (fail-closed, exactly as on the `run`/`verify` route).
    let scope_info = std::fs::canonicalize(run.path())
        .unwrap_or_else(|_| run.path().to_path_buf())
        .join("cgroup");
    let (mut child, mut cgroup) = crate::cgroup::spawn_scoped(
        crate::cgroup::MemoryLimits::from_env()?,
        scope_info,
        &cmd,
        |c| {
            unsafe {
                c.pre_exec(move || {
            libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL as libc::c_ulong, 0, 0, 0);
            let core = libc::rlimit { rlim_cur: 0, rlim_max: 0 };
            libc::setrlimit(libc::RLIMIT_CORE, &core);
            let nofile = libc::rlimit { rlim_cur: 4096, rlim_max: 4096 };
            libc::setrlimit(libc::RLIMIT_NOFILE, &nofile);
            // The launcher's memory backstops bound acos-art, never ART.
            crate::watchdog::relax_child_limits();
            Ok(())
                });
            }
            c.process_group(0);
            c.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
        },
    )?;
    let pid = child.id() as i32;
    // Bounded capture (R1): a guest that loops on printf costs at most the cap.
    let cap = stream_cap_from_env();
    let (otx, orx) = mpsc::channel::<(u8, TailBuffer)>();
    let out_pipe = child.stdout.take().expect("piped stdout");
    let otx2 = otx.clone();
    let out_thread = std::thread::spawn(move || {
        let buf = crate::capture::drain_to_tail(out_pipe, cap);
        let _ = otx2.send((0, buf));
    });
    let err_pipe = child.stderr.take().expect("piped stderr");
    let err_thread = std::thread::spawn(move || {
        let buf = crate::capture::drain_to_tail(err_pipe, cap);
        let _ = otx.send((1, buf));
    });

    // The launcher's own ceiling (ACOS_ART_RSS_MAX, default 2 GiB).
    let dog = MemoryWatchdog::from_env();
    if let Some(d) = &dog {
        d.arm_process_group(pid);
    }

    let mut timed_out = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break Some(s),
            Ok(None) => {}
            Err(e) => {
                let _ = child.kill();
                return Err(Error::Io(e));
            }
        }
        if let Some(d) = &dog {
            if d.trip().is_some() {
                let _ = child.wait();
                break None;
            }
        }
        if started.elapsed() >= opts.timeout {
            timed_out = true;
            unsafe {
                libc::kill(-pid, libc::SIGKILL);
                libc::kill(pid, libc::SIGKILL);
            }
            cgroup.kill();
            let _ = child.wait();
            break None;
        }
        cgroup.sample();
        std::thread::sleep(Duration::from_millis(5));
    };
    // The guest has ended (or was killed by the timeout/watchdog). Nothing
    // else may keep the peer thread alive, and nothing may outlive the guest
    // inside its scope. Both actions are idempotent and happen before any
    // capture/join, which is what bounds teardown after a guest death.
    stop.store(true, Ordering::Release);
    cgroup.sample();
    if status.is_some() {
        cgroup.kill();
    }
    let wall_ms = started.elapsed().as_millis() as u64;

    let mut stdout_buf = TailBuffer::with_cap(cap);
    let mut stderr_buf = TailBuffer::with_cap(cap);
    for _ in 0..2 {
        match orx.recv_timeout(Duration::from_secs(3)) {
            Ok((0, b)) => stdout_buf = b,
            Ok((_, b)) => stderr_buf = b,
            Err(_) => break,
        }
    }
    let _ = out_thread.join();
    let _ = err_thread.join();
    let batch = logd.finish_capped();
    let log_stats = batch.stats;
    let log_first_dropped = batch.first_dropped.clone();
    let logs = batch.entries;
    let stdout = stdout_buf.to_text();
    let stderr = stderr_buf.to_text();
    // Bound the peer join: `stop` alone is enough unless the peer is wedged in
    // a service handler. A missing summary becomes the default (no delivered
    // bind), never a blocked `launch`.
    let summary = match rx.recv_timeout(Duration::from_secs(2)) {
        Ok(s) => {
            let _ = peer_thread.join();
            s
        }
        Err(_) => {
            drop(peer_thread);
            PeerSummary::default()
        }
    };

    let marker_path = staging.join("application-created.marker");
    let marker = fs::read_to_string(&marker_path).ok();
    let activity_marker = fs::read_to_string(staging.join("activity-resumed.marker")).ok();
    let activity_failed_marker =
        fs::read_to_string(staging.join("activity-not-resumed.marker")).ok();
    // The framework's own main-thread trace, paired: a `>>>>> Dispatching`
    // line for 110 (BIND_APPLICATION) or 159 (EXECUTE_TRANSACTION) followed by
    // its `<<<<< Finished` line — i.e. the real handler ran to completion.
    let mut looper_trace: Vec<String> = Vec::new();
    let mut pending = false;
    for line in stdout.lines() {
        let Some(line) = line.strip_prefix("ACOSLAUNCH TRACE ") else { continue };
        if line.starts_with(">>>>> Dispatching") && line.contains("android.app.ActivityThread") {
            pending = line.ends_with(": 110") || line.ends_with(": 159");
            if pending {
                looper_trace.push(line.to_string());
            }
        } else if pending && line.starts_with("<<<<< Finished") {
            looper_trace.push(line.to_string());
            pending = false;
        }
    }
    // Copy staging artefacts to --out when they differ.
    for name in [
        "launch.json",
        "application-created.marker",
        "activity-resumed.marker",
        "activity-not-resumed.marker",
        "unsupported.jsonl",
    ] {
        let from = staging.join(name);
        let to = out.join(name);
        if from.is_file() && from != to {
            let _ = fs::copy(&from, &to);
        }
    }
    let guest_ok = status.map(|s| s.success()).unwrap_or(false);
    // bwrap re-exits with 128+signal when the guest died from a signal; undo
    // that remap so the structured outcome names the guest's real signal.
    let (guest_exit, guest_signal) = split_exit_status(status);
    let application_created = marker.is_some() && summary.delivered;
    let activity_resumed = activity_marker.is_some();
    let activity_wanted = opts.until == "activity-resumed";
    // A watchdog kill is a host resource outcome, not a guest crash.
    let host_over_limit = dog.as_ref().and_then(|d| d.trip());
    let (mut outcome, mut code) = if application_created
        && guest_ok
        && (!activity_wanted || activity_resumed)
    {
        (Outcome::Ok, None)
    } else if timed_out {
        // The guest never reached its milestone within the budget: a distinct
        // outcome, not "still running".
        (Outcome::Timeout, Some("TIMEOUT".to_string()))
    } else if guest_signal.is_some() {
        // A guest killed by the guard/Landlock (fail closed), the audit filter
        // or the kernel is reported by signal, even when a milestone marker
        // was already written.
        (Outcome::Crash, Some("GUEST_KILLED_BY_SIGNAL".to_string()))
    } else if guest_exit.is_some()
        && application_created
        && activity_resumed
        && (activity_wanted || !guest_ok)
    {
        // The requested lifecycle milestone was reached (the marker is written
        // from `Activity.mResumed`), but the guest then failed: the *next*
        // blocker, reported rather than folded into "not resumed".
        (
            Outcome::Crash,
            Some("GUEST_EXIT_AFTER_RESUME".to_string()),
        )
    } else if guest_exit.is_some()
        && application_created
        && activity_wanted
        && summary.txn_bytes.is_some()
    {
        // Application.onCreate ran; the activity step did not reach onResume.
        (
            Outcome::Crash,
            Some("NO_ACTIVITY_RESUMED".to_string()),
        )
    } else if guest_exit.is_some() {
        // The guest ended before the requested milestone. `guest_exit` carries
        // the code; this is the distinct "guest exited early" outcome.
        (Outcome::Crash, Some("GUEST_EXITED".to_string()))
    } else if application_created {
        (
            Outcome::Crash,
            Some("GUEST_EXIT".to_string()),
        )
    } else {
        (
            Outcome::Crash,
            Some(
                if summary.delivered {
                    "NO_APPLICATION"
                } else if summary.bind_bytes.is_none() {
                    "NO_BIND_REQUEST"
                } else {
                    "BIND_NOT_DELIVERED"
                }
                .to_string(),
            ),
        )
    };
    if let Some(t) = host_over_limit {
        outcome = Outcome::Unsupported;
        code = Some(HOST_MEMORY_LIMIT.to_string());
        eprintln!(
            "acos-art: launch: {HOST_MEMORY_LIMIT}: launcher RSS {} MiB exceeded the {} MiB ceiling",
            t.rss_bytes / 1024 / 1024,
            t.limit_bytes / 1024 / 1024
        );
    }
    // S5: memcg OOM and the seccomp proof, same contract as the `run` route.
    if cgroup.oom_kills() > 0 {
        outcome = Outcome::Unsupported;
        code = Some(HOST_MEMORY_LIMIT.to_string());
    }
    let mut unsupported_notes: Vec<String> = Vec::new();
    unsupported_notes.push(cgroup.summary());
    if cgroup.oom_kills() > 0 {
        unsupported_notes.push(format!(
            "{HOST_MEMORY_LIMIT}: memcg OOM-killed the launch ({} kill(s) in {})",
            cgroup.oom_kills(),
            cgroup.path().display()
        ));
    } else if dog.as_ref().and_then(|d| d.trip()).is_none() {
        // The guard is still fail-closed, but a refusal is a *structured*
        // outcome carrying the guest's exit/signal and the guard's own ERR
        // line: a dead guest must never turn into a blocked or opaque launch.
        let guard_log = fs::read_to_string(run.path().join("out/seccomp.log")).unwrap_or_default();
        for line in guard_log.lines().filter(|l| !l.trim().is_empty()) {
            if line.starts_with("ERR ") || line.starts_with("DENY ") {
                unsupported_notes.push(format!("seccomp: {line}"));
            }
        }
        match crate::seccomp::require_guard(run.path()) {
            Ok(line) => unsupported_notes.push(format!("seccomp: {line}")),
            Err(e) => {
                unsupported_notes.push(e.to_string());
                if outcome == Outcome::Ok {
                    outcome = Outcome::Unsupported;
                    code = Some("SECCOMP_UNAVAILABLE".to_string());
                }
            }
        }
    }
    unsupported_notes.push(log_stats.summary());
    if let Some(e) = &log_first_dropped {
        unsupported_notes.push(format!(
            "logd oldest entry dropped: [{}] {}: {}",
            e.priority_name(),
            e.tag,
            crate::capture::head(&e.message, 200)
        ));
    }
    if summary.resource_limits > 0 {
        unsupported_notes.push(format!(
            "acos-binder-peer: ART_RESOURCE_LIMIT x{} (bounded peer queue/log overflow)",
            summary.resource_limits
        ));
    }
    let result = LaunchResult {
        outcome,
        code,
        apk: opts.apk.display().to_string(),
        package,
        until: opts.until.clone(),
        application_created,
        activity_launched: activity_resumed,
        guest_exit,
        guest_signal,
        wall_ms,
        activity: activity.clone(),
        application_marker: marker.clone(),
        activity_marker: activity_marker
            .clone()
            .or_else(|| activity_failed_marker.clone()),
        looper_trace,
        bind_application: BindDelivery {
            request_bytes: summary.bind_bytes,
            delivered: summary.delivered,
            app_thread_wire: summary.app_thread,
            code: acos_binder_peer::peer::BIND_APPLICATION_CODE,
        },
        service_calls: summary.calls,
        unsupported: summary.unsupported,
        stdout: crate::capture::tail_text(&stdout, 4096),
        stderr: crate::capture::tail_text(&stderr, 4096),
        logs,
        notes: unsupported_notes,
        marker,
    };
    eprintln!(
        "acos-art: launch wall={wall_ms}ms until={} application_created={} activity_resumed={} \
         txn={:?}/{} reverse_replies={} reverse_failed={} logd={} ({}) peer_resource_limits={}",
        opts.until,
        result.application_created,
        activity_resumed,
        summary.txn_bytes,
        summary.txn_delivered,
        summary.reverse_replies,
        summary.reverse_failed,
        result.logs.len(),
        log_stats.summary(),
        summary.resource_limits
    );
    for note in &result.notes {
        eprintln!("acos-art: {note}");
    }
    Ok(result)
}

/// Undo bwrap's `128+signal` remap and return the guest's real termination.
fn split_exit_status(status: Option<std::process::ExitStatus>) -> (Option<i32>, Option<i32>) {
    let raw_exit = status.and_then(|s| s.code());
    let mut signal = status.and_then(|s| s.signal());
    let mut exit = raw_exit;
    if signal.is_none() {
        if let Some(code) = raw_exit {
            if (129..=192).contains(&code) {
                signal = Some(code - 128);
                exit = None;
            }
        }
    }
    (exit, signal)
}

#[derive(Default)]
struct PeerSummary {
    calls: Vec<ServiceCall>,
    unsupported: Vec<UnsupportedCall>,
    bind_bytes: Option<usize>,
    app_thread: Option<u32>,
    delivered: bool,
    txn_bytes: Option<usize>,
    txn_delivered: bool,
    reverse_replies: u64,
    reverse_failed: u64,
    resource_limits: u64,
}

fn serve(peer: &mut Peer, fd: RawFd, stop: &AtomicBool) -> std::io::Result<()> {
    loop {
        let frame: Frame = match read_frame(fd) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(()),
            Err(e)
                if (e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut)
                    && stop.load(Ordering::Acquire) =>
            {
                return Ok(())
            }
            // A live guest may be idle; the timeout only exists to observe
            // `stop`, not to end the session.
            Err(e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut =>
            {
                continue
            }
            Err(e) => return Err(e),
        };
        // `ACOS_PEER_DEBUG=1` prints every frame the peer reads and writes. The
        // reply/`write_frame` failure line is *not* gated: a reply the peer
        // cannot send is the difference between a structured guest error and a
        // main thread blocked in `transact`, so it must always be visible.
        let dbg = std::env::var_os("ACOS_PEER_DEBUG").is_some();
        if dbg {
            eprintln!(
                "peer-trace: in ty={} seq={} code={} flags={:#x} target={:#x} data={} offs={} fds={}",
                frame.ty,
                frame.seq,
                frame.code,
                frame.flags,
                frame.target,
                frame.data.len(),
                frame.offs.len(),
                frame.fds.len()
            );
        }
        let outs = peer.handle(frame);
        if dbg {
            eprintln!("peer-trace: handle -> {} frames", outs.len());
        }
        for out in outs {
            if let Err(e) = write_frame(fd, &out) {
                eprintln!(
                    "peer-trace: write_frame FAILED: {e} (ty={} seq={} code={} data={} offs={} fds={})",
                    out.ty,
                    out.seq,
                    out.code,
                    out.data.len(),
                    out.offs.len(),
                    out.fds.len()
                );
                close_frame_fds(&out);
                return Err(e);
            }
            if dbg {
                eprintln!(
                    "peer-trace: out ty={} seq={} code={} data={} offs={} fds={:?}",
                    out.ty,
                    out.seq,
                    out.code,
                    out.data.len(),
                    out.offs.len(),
                    out.fds
                );
            }
            close_frame_fds(&out);
        }
        let _ = std::io::stderr().flush();
    }
}

/// Compile the userspace binder driver (`crates/acos-binder-peer/native`) with
/// the NDK's bionic clang into `<data>/lib/libacos_binder_driver.so`.
fn build_driver(root: &ArtRoot) -> Result<PathBuf> {
    const SOURCE: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../acos-binder-peer/native/binder_driver.c"
    ));
    let dir = root.data.join("lib");
    fs::create_dir_all(&dir)?;
    let out = dir.join("libacos_binder_driver.so");
    let src = dir.join("binder_driver.c");
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for b in SOURCE.as_bytes() {
        hash ^= u64::from(*b);
        hash = hash.wrapping_mul(0x100_0000_01b3);
    }
    let stamp = dir.join("binder_driver.hash");
    if out.is_file()
        && fs::read_to_string(&stamp).map(|s| s.trim() == hash.to_string()).unwrap_or(false)
    {
        return Ok(out);
    }
    fs::write(&src, SOURCE)?;
    let clang = ndk_clang()?;
    let st = Command::new(&clang)
        .args([
            "-O2",
            "-fPIC",
            "-shared",
            "-Wall",
            "-Wno-unused-parameter",
            "-Wno-unused-function",
            "-o",
        ])
        .arg(&out)
        .arg(&src)
        .args(["-llog", "-ldl"])
        .status()?;
    if !st.success() {
        return Err(Error::Tool(format!(
            "{} failed to build the binder driver",
            clang.display()
        )));
    }
    fs::write(&stamp, format!("{hash}\n"))?;
    Ok(out)
}

/// Compile only the launch guest (`LaunchMain` + `RenderNat`) so unrelated
/// guest sources under `guest/java` cannot break this command.
fn build_launch_guest(root: &ArtRoot) -> Result<PathBuf> {
    let src_dir = PathBuf::from(crate::app::GUEST_SOURCE);
    let out_dir = root.data.join("app/launch-guest");
    fs::create_dir_all(&out_dir)?;
    let sources = [
        src_dir.join("dev/acos/art/LaunchMain.java"),
        src_dir.join("dev/acos/art/RenderNat.java"),
        // The driver's JNI surface (`startBinderLooper`), shared with SessionMain
        // and with the P2f acceptance dex (`dev.acos.e3g.Nat`); one copy under
        // guest/java so this command never compiles another crate's test dex.
        src_dir.join("dev/acos/art/Nat.java"),
    ];
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for p in &sources {
        let bytes = fs::read(p)?;
        for b in &bytes {
            hash ^= u64::from(*b);
            hash = hash.wrapping_mul(0x100_0000_01b3);
        }
    }
    let stamp = out_dir.join(".hash");
    let dex = out_dir.join("classes.dex");
    if dex.is_file()
        && fs::read_to_string(&stamp).map(|s| s.trim() == hash.to_string()).unwrap_or(false)
    {
        return Ok(dex);
    }
    let sdk = android_sdk()?;
    let android_jar = sdk.join("platforms/android-34/android.jar");
    let d8 = find_tool(&sdk, "d8")?;
    let classes = out_dir.join("classes");
    let _ = fs::remove_dir_all(&classes);
    fs::create_dir_all(&classes)?;
    let out = Command::new(std::env::var("JAVAC").unwrap_or_else(|_| "javac".into()))
        .args(["-nowarn", "-source", "11", "-target", "11"])
        .arg("-cp")
        .arg(&android_jar)
        .arg("-d")
        .arg(&classes)
        .args(&sources)
        .output()?;
    if !out.status.success() {
        return Err(Error::Tool(format!(
            "javac failed for the launch guest:\n{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        )));
    }
    let mut class_files = Vec::new();
    walk_classes(&classes, &mut class_files)?;
    class_files.sort();
    let out = Command::new(&d8)
        .arg("--lib")
        .arg(&android_jar)
        .args(["--min-api", "34", "--output"])
        .arg(&out_dir)
        .args(&class_files)
        .output()?;
    if !out.status.success() || !dex.is_file() {
        return Err(Error::Tool(format!(
            "d8 failed for the launch guest:\n{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        )));
    }
    fs::write(&stamp, format!("{hash}\n"))?;
    let _ = fs::remove_dir_all(&classes);
    Ok(dex)
}

fn walk_classes(dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    for e in fs::read_dir(dir)? {
        let p = e?.path();
        if p.is_dir() {
            walk_classes(&p, out)?;
        } else if p.extension().and_then(|s| s.to_str()) == Some("class") {
            out.push(p);
        }
    }
    Ok(())
}

fn android_sdk() -> Result<PathBuf> {
    for var in ["ANDROID_SDK_ROOT", "ANDROID_HOME"] {
        if let Some(p) = std::env::var_os(var) {
            let p = PathBuf::from(p);
            if p.join("platforms").is_dir() {
                return Ok(p);
            }
        }
    }
    if let Some(home) = std::env::var_os("HOME") {
        let p = PathBuf::from(home).join("Android/Sdk");
        if p.join("platforms").is_dir() {
            return Ok(p);
        }
    }
    Err(Error::Tool("no Android SDK found (set ANDROID_SDK_ROOT)".into()))
}

fn find_tool(sdk: &Path, name: &str) -> Result<PathBuf> {
    let mut versions: Vec<PathBuf> = fs::read_dir(sdk.join("build-tools"))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .collect();
    versions.sort();
    for v in versions.into_iter().rev() {
        let p = v.join(name);
        if p.is_file() {
            return Ok(p);
        }
    }
    Err(Error::Tool(format!("no {name} under {}/build-tools", sdk.display())))
}

fn ndk_clang() -> Result<PathBuf> {
    let mut roots: Vec<PathBuf> = Vec::new();
    if let Some(p) = std::env::var_os("ANDROID_NDK_HOME").or_else(|| std::env::var_os("ANDROID_NDK_ROOT"))
    {
        roots.push(PathBuf::from(p));
    }
    if let Some(home) = std::env::var_os("HOME") {
        if let Ok(rd) = fs::read_dir(PathBuf::from(home).join("Android/Sdk/ndk")) {
            let mut v: Vec<PathBuf> = rd.filter_map(|e| e.ok()).map(|e| e.path()).collect();
            v.sort();
            v.reverse();
            roots.extend(v);
        }
    }
    for r in roots {
        let dir = r.join("toolchains/llvm/prebuilt/linux-x86_64/bin");
        for target in ["x86_64-linux-android34-clang", "x86_64-linux-android33-clang"] {
            let p = dir.join(target);
            if p.is_file() {
                return Ok(p);
            }
        }
    }
    if let Ok(cc) = std::env::var("CC") {
        return Ok(PathBuf::from(cc));
    }
    Err(Error::Tool(
        "no NDK clang found (set ANDROID_NDK_HOME; needs x86_64-linux-android34-clang)".into(),
    ))
}

/// The APK's launcher activity, from `aapt2 dump badging`
/// (`launchable-activity: name='…'`). The class `LaunchActivityItem` names.
fn apk_launcher_activity(apk: &Path) -> Result<String> {
    let sdk = android_sdk()?;
    let aapt2 = find_tool(&sdk, "aapt2")?;
    let out = Command::new(&aapt2).arg("dump").arg("badging").arg(apk).output()?;
    if !out.status.success() {
        return Err(Error::Tool(format!(
            "aapt2 could not read {}: {}",
            apk.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let class = text
        .lines()
        .find_map(|l| l.strip_prefix("launchable-activity:"))
        .and_then(|l| l.split("name='").nth(1))
        .and_then(|l| l.split('\'').next())
        .map(str::to_string);
    class.ok_or_else(|| {
        Error::Tool(format!(
            "{} declares no launchable activity (aapt2 dump badging)",
            apk.display()
        ))
    })
}

fn tail(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let start = s.len() - max;
    s[start..].to_string()
}

#[allow(dead_code)]
fn is_file(p: &Path) -> bool {
    p.is_file()
}
