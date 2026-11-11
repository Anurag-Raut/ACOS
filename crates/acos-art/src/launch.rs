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
    // W16: the synthetic `IInputManager` devices carry the image's native
    // `KeyCharacterMap` payload built from the stock `/system/usr/keychars/*.kcm`
    // files. `acos-services` resolves them from `ACOS_KEYCHARS_DIR`; publish the
    // sysroot this launch actually uses (an explicit `--sysroot` need not match
    // the productised `.cache/art/sysroot`).
    std::env::set_var(
        "ACOS_KEYCHARS_DIR",
        root.sysroot.join("system/usr/keychars"),
    );
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

    // W17: installd's native-library step. Stock Android extracts the APK's
    // `lib/<abi>/*.so` into `ApplicationInfo.nativeLibraryDir` at install time
    // and `LoadedApk` puts that directory on the class loader's native search
    // path. ACOS stages the APK read-only and never ran that step, so
    // `dlopen` saw `library "libhellojni.so" not found`. Extract into this
    // run's own tree (bound read-only at `/data/app-lib`); an arm64-only APK
    // keeps its honest `UNSUPPORTED_ABI` path in the guest.
    let native_lib_root = run.dir("app-lib");
    let native_libs = acos_services::native_libs::install(&opts.apk, &native_lib_root)
        .map_err(|e| Error::Layout(format!("native libs: {e}")))?;
    match &native_libs {
        acos_services::native_libs::NativeLibInstall::Installed { libs, .. } => {
            eprintln!(
                "acos-art: launch: native libs installed={} dir={} guest={}",
                libs.len(),
                native_lib_root.display(),
                acos_services::native_libs::GUEST_NATIVE_LIB_DIR
            );
        }
        acos_services::native_libs::NativeLibInstall::UnsupportedAbi { available, message } => {
            eprintln!(
                "acos-art: launch: UNSUPPORTED_ABI available={available:?}: {message}"
            );
        }
        acos_services::native_libs::NativeLibInstall::None => {}
    }
    // The declared `<application android:name>` class and `android:theme`.
    // Without the class the real `LoadedApk.makeApplication` instantiates
    // `android.app.Application` instead of the APK's own (W15: RN's
    // `SoLoader.init()` lives there, and the guest ran the base class, so the
    // first React class hit `SoLoader.init() not yet called`). The theme is
    // what `PackageParser` resolves into the activity's `ActivityInfo.theme`;
    // without it an AppCompat activity aborts with "You need to use a
    // Theme.AppCompat theme" (W17: rn-hello's next wall after SoLoader).
    let installed = acos_services::pms::PmsState::from_apk(&opts.apk).ok();
    let application_class = installed
        .as_ref()
        .map(|state| state.app.class_name.clone())
        .filter(|name| !name.is_empty());
    let application_theme = installed.as_ref().map_or(0, |state| state.app.theme);

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
    // W10: the live session drives the same `InputHost` the window session
    // uses and captures C1's stored buffer, so the standard install hands both
    // back. Building the host here (instead of inside the peer thread) keeps
    // those handles in the launcher without a second install.
    let live_handles = {
        let mut host = acos_services::ServiceHost::new();
        let handles = acos_services::registry::install_with_handles(&mut host);
        if let Err(e) = host.install_package_from_apk(&apk_for_peer) {
            eprintln!("acos-art: launch: install_package_from_apk failed: {e}");
        }
        (host, handles)
    };
    let (peer_host, live) = live_handles;
    let interactive = std::env::var_os("ACOSLAUNCH_INTERACTIVE").is_some();
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
            let mut peer = Peer::with_host(peer_host, Some(peer_log));
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
        application_class.clone().unwrap_or_default(),
        application_theme.to_string(),
    ];
    crate::seccomp::enable(root)?;
    let mut sb = root.sandbox();
    sb.run_root(run.path());
    sb.seccomp_guest(true);
    sb.with_shim(false);
    sb.ro_bind(&art_shim, "/data/shim/libartshim.so");
    // The per-run extracted native libraries (installd's `nativeLibraryDir`).
    sb.ro_bind(
        &native_lib_root,
        acos_services::native_libs::GUEST_NATIVE_LIB_ROOT,
    );
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
    if std::env::var_os("ACOSLAUNCH_HOLD_MS").is_none() {
        // Interactive sessions keep the real looper alive and publish a live
        // view-tree sample instead of quitting after `activity-resumed`.
        if interactive {
            sb.env("ACOSLAUNCH_LIVE_PROBE", "1");
        }
    }
    if let Ok(hold) = std::env::var("ACOSLAUNCH_HOLD_MS") {
        sb.env("ACOSLAUNCH_HOLD_MS", &hold);
        // A hold is the one-shot diagnostic mode; the live probe is the
        // interactive one. The guest checks the probe first, so they cannot
        // conflict even if both are set.
        if interactive {
            sb.env("ACOSLAUNCH_LIVE_PROBE", "1");
        }
    }
    // W9: forward the guest-HW diagnostic switch so the gralloc shim's
    // `allocate`/`lock` line reaches the launch log (W7 could not see it because
    // the guest env is cleared and the launcher did not forward the variable).
    if std::env::var_os("ACOSART_HW_VERBOSE").is_some() {
        sb.env("ACOSART_HW_VERBOSE", "1");
    }
    crate::watchdog::note_guest_priority_gap("launch");
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
            // init.rc: setrlimit nice 40 40 (host best-effort; see watchdog).
            crate::watchdog::apply_init_rlimit_nice();
            Ok(())
                });
            }
            c.process_group(0);
            if std::env::var_os("ACOSLAUNCH_INTERACTIVE").is_some() {
                c.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
            } else {
                c.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
            }
        },
    )?;
    let pid = child.id() as i32;
    // Bounded capture (R1): a guest that loops on printf costs at most the cap.
    let cap = stream_cap_from_env();
    let (otx, orx) = mpsc::channel::<(u8, TailBuffer)>();
    let probe_state = Arc::new(std::sync::Mutex::new(ProbeState::default()));
    let out_pipe = child.stdout.take().expect("piped stdout");
    let otx2 = otx.clone();
    let probe_for_out = Arc::clone(&probe_state);
    let out_thread = std::thread::spawn(move || {
        let buf = if interactive {
            drain_stdout_lines(out_pipe, cap, probe_for_out)
        } else {
            crate::capture::drain_to_tail(out_pipe, cap)
        };
        let _ = otx2.send((0, buf));
    });
    let err_pipe = child.stderr.take().expect("piped stderr");
    let err_tee = std::env::var_os("ACOSLAUNCH_GUEST_STDERR_FILE").map(PathBuf::from);
    let err_thread = std::thread::spawn(move || {
        let buf = match err_tee {
            Some(path) => drain_tee(err_pipe, cap, &path),
            None => crate::capture::drain_to_tail(err_pipe, cap),
        };
        let _ = otx.send((1, buf));
    });

    // The launcher's own ceiling (ACOS_ART_RSS_MAX, default 2 GiB).
    let dog = MemoryWatchdog::from_env();
    if let Some(d) = &dog {
        d.arm_process_group(pid);
    }

    let mut timed_out = false;
    // W10: the interactive session replaces the one-shot wait loop. The guest
    // is already spawned with the live probe; `interactive_serve` waits for the
    // Activity + first C1 buffer, prints the ready line on the launcher's
    // stdout, then serves `tree` / `screenshot` / `tap` / `key` / `logs` /
    // `close` (line-JSON, one reply per command) until the caller closes it.
    let mut interactive_clean = false;
    let status = if interactive {
        let frames_dir = out.join("live-frames");
        let dead = interactive_serve(
            &mut child,
            &mut cgroup,
            &live,
            &probe_state,
            started,
            opts.timeout,
            &frames_dir,
            &staging,
            &out,
            &package,
            activity.as_deref(),
            dog.as_ref(),
            // W22: a live snapshot of the logd sink, so `assert_log` can read
            // the guest's own log while the session is running.
            &logd,
        );
        match dead {
            Ok(end) => {
                interactive_clean = end.clean;
                end.status
            }
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(Error::Io(e));
            }
        }
    } else {
        loop {
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
        }
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
    let (mut guest_exit, mut guest_signal) = split_exit_status(status);
    // The interactive session ends by killing its guest (the host owns the
    // lifetime, exactly as AMS does): that SIGKILL is not a guest crash.
    if interactive_clean {
        guest_exit = Some(0);
        guest_signal = None;
    }
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
    // A session that reached `ready` and was closed by its caller has done what
    // it was asked: the process ending there is the session's own teardown.
    if interactive_clean && activity_resumed {
        outcome = Outcome::Ok;
        code = None;
    }
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

// ------------------------------------------------------------ W10 live session
//
// One real `counter.apk` guest, driven by the CLI engine over the launcher's
// own stdin/stdout (line-JSON, one reply per command):
//
//   {"cmd":"tree"}       -> live tree sample (in-guest probe) + a fresh C1 capture
//   {"cmd":"screenshot"} -> the newest C1 buffer as PNG (labelled capture source)
//   {"cmd":"tap","x":..,"y":..} -> real InputChannel tap, then a *new* C1 frame
//                           and a *new* probe sample, or a structured failure
//   {"cmd":"close"}      -> stop the guest and exit
//
// The pixels are C1's stored `GraphicBuffer` (read through C1's own dup of the
// guest's memfd), never a device capture and never the preview renderer. The
// tree is the guest's own read-only view walk (`LaunchMain` live probe), not an
// offline render. `frame_seq` is this session's capture sequence (one per
// capture command, so a reply can never re-label an older capture as fresh);
// the underlying C1 `buffer_generation` and producer `frame_number` are
// reported next to it for the honest cross-check.

const LIVE_PROBE_PREFIX: &str = "ACOSPROBE ";
const LIVE_PROBE_LINE_CAP: usize = 128 * 1024;
const CAPTURE_SOURCE: &str = "acos-c1-buffer";

#[derive(Clone)]
struct ProbeSample {
    seq: u64,
    tree: serde_json::Value,
    at_ms: u64,
}

#[derive(Default)]
struct ProbeState {
    latest: Option<ProbeSample>,
}

fn host_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn env_u64(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(default)
}

fn latest_probe(state: &Arc<std::sync::Mutex<ProbeState>>) -> Option<ProbeSample> {
    state
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .latest
        .clone()
}

/// Diagnostic (`ACOSLAUNCH_GUEST_STDERR_FILE`): drain a guest stream into the
/// bounded tail **and** a host file, so a large ART thread dump survives the
/// launcher's 4 KiB result tail. The file is capped at 32 MiB.
fn drain_tee<R: std::io::Read>(mut reader: R, cap: usize, path: &Path) -> TailBuffer {
    let mut tail = TailBuffer::with_cap(cap);
    let mut file = fs::File::create(path).ok();
    let mut written = 0usize;
    let mut chunk = [0u8; 64 * 1024];
    loop {
        match reader.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                tail.push(&chunk[..n]);
                if let Some(f) = file.as_mut() {
                    if written < 32 * 1024 * 1024 {
                        let take = n.min(32 * 1024 * 1024 - written);
                        let _ = f.write_all(&chunk[..take]);
                        written += take;
                    }
                }
            }
        }
    }
    tail
}

/// Drain the guest's stdout into the bounded tail buffer **and** parse the live
/// probe lines as they arrive. A partial line is never parsed (a truncated
/// 128 KiB read can split a JSON object; parse only lines that ended with '\n').
fn drain_stdout_lines<R: std::io::Read>(
    reader: R,
    cap: usize,
    state: Arc<std::sync::Mutex<ProbeState>>,
) -> TailBuffer {
    use std::io::{BufRead, Read};
    let mut tail = TailBuffer::with_cap(cap);
    let mut reader = std::io::BufReader::new(reader);
    let mut at_line_start = true;
    loop {
        let mut line = Vec::new();
        match reader
            .by_ref()
            .take(LIVE_PROBE_LINE_CAP as u64)
            .read_until(b'\n', &mut line)
        {
            Ok(0) => break,
            Ok(_) => {
                tail.push(&line);
                let ended = line.ends_with(b"\n");
                if at_line_start {
                    if let Ok(text) = std::str::from_utf8(&line) {
                        if let Some(json) = text.trim_end().strip_prefix(LIVE_PROBE_PREFIX) {
                            if let Ok(value) = serde_json::from_str::<serde_json::Value>(json) {
                                let seq = value.get("seq").and_then(|v| v.as_u64());
                                let tree = value.get("tree");
                                if let (Some(seq), Some(tree)) = (seq, tree) {
                                    let mut state =
                                        state.lock().unwrap_or_else(|e| e.into_inner());
                                    let newer = state
                                        .latest
                                        .as_ref()
                                        .map(|p| p.seq < seq)
                                        .unwrap_or(true);
                                    if newer {
                                        state.latest = Some(ProbeSample {
                                            seq,
                                            tree: tree.clone(),
                                            at_ms: host_ms(),
                                        });
                                    }
                                }
                            }
                        }
                    }
                }
                at_line_start = ended;
            }
            Err(_) => break,
        }
    }
    tail
}

struct Capture {
    seq: u64,
    generation: u64,
    frame_number: u64,
    buffer_id: u64,
    producer_id: u32,
    layer_id: i32,
    layer_name: String,
    width: u32,
    height: u32,
    md5: String,
    fnv1a: u64,
    non_zero_pixels: usize,
    path: PathBuf,
    bytes: usize,
}

impl Capture {
    fn fields(&self) -> serde_json::Value {
        serde_json::json!({
            "frame_seq": self.seq,
            "md5": self.md5,
            "png": self.path.display().to_string(),
            "capture_source": CAPTURE_SOURCE,
            "capture_label": "ACOS C1 buffer capture (compositor-client stored buffer; not a device capture)",
            "buffer_generation": self.generation,
            "frame_number": self.frame_number,
            "buffer_id": self.buffer_id,
            "producer_id": self.producer_id,
            "layer_id": self.layer_id,
            "layer_name": self.layer_name,
            "width": self.width,
            "height": self.height,
            "png_bytes": self.bytes,
            "pixels_fnv1a": format!("{:016x}", self.fnv1a),
            "non_zero_pixels": self.non_zero_pixels,
        })
    }
}

/// Capture the newest C1 buffer as a PNG and return it with its evidence.
fn capture_c1(
    live: &acos_services::registry::LiveHandles,
    dir: &Path,
    seq: &mut u64,
) -> std::io::Result<Option<Capture>> {
    let frame = {
        let state = live.compositor.lock().unwrap_or_else(|e| e.into_inner());
        state.capture_latest()
    };
    let Some(frame) = frame else {
        return Ok(None);
    };
    let png = encode_png_rgba(frame.width, frame.height, frame.stride, &frame.rgba);
    fs::create_dir_all(dir)?;
    *seq += 1;
    let path = dir.join(format!(
        "acos-c1-layer{}-frame-{}-g{}-c{}.png",
        frame.layer_id, frame.frame_number, frame.generation, *seq
    ));
    fs::write(&path, &png)?;
    let md5 = md5_hex_file(&path).unwrap_or_else(|| format!("{:016x}", frame.fnv1a));
    Ok(Some(Capture {
        seq: *seq,
        generation: frame.generation,
        frame_number: frame.frame_number,
        buffer_id: frame.buffer_id,
        producer_id: frame.producer_id,
        layer_id: frame.layer_id,
        layer_name: frame.layer_name,
        width: frame.width,
        height: frame.height,
        md5,
        fnv1a: frame.fnv1a,
        non_zero_pixels: frame.non_zero_pixels,
        path,
        bytes: png.len(),
    }))
}

fn latest_generation(live: &acos_services::registry::LiveHandles) -> u64 {
    live.compositor
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .latest_generation()
}

fn protocol_write(value: &serde_json::Value) {
    use std::io::Write;
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{value}");
    let _ = out.flush();
}

fn md5_hex_file(path: &Path) -> Option<String> {
    // Same precedent as `app.rs::md5_file`: there is no md5 crate in this
    // workspace, and the host `md5sum` is available.
    let output = Command::new("md5sum").arg(path).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    text.split_whitespace().next().map(|s| s.to_string())
}

struct SessionEnd {
    status: Option<std::process::ExitStatus>,
    clean: bool,
}

fn kill_guest(child: &mut std::process::Child, cgroup: &mut crate::cgroup::RunCgroup) {
    let pid = child.id() as i32;
    // SAFETY: the guest was spawned with `process_group(0)`, so `-pid` names
    // its group and nothing else. Both calls are idempotent.
    unsafe {
        libc::kill(-pid, libc::SIGKILL);
        libc::kill(pid, libc::SIGKILL);
    }
    cgroup.sample();
    cgroup.kill();
    let _ = child.wait();
}

fn write_session_evidence(out: &Path, evidence: &[serde_json::Value]) {
    let doc = serde_json::json!({
        "capture_source": CAPTURE_SOURCE,
        "capture_label": "screenshots are ACOS C1 buffer captures (compositor-client stored buffers), not device captures",
        "protocol": "acos-art launch --interactive (ACOSLAUNCH_INTERACTIVE=1)",
        "steps": evidence,
    });
    let _ = fs::create_dir_all(out);
    let _ = fs::write(
        out.join("live-session.json"),
        serde_json::to_vec_pretty(&doc).unwrap_or_default(),
    );
}

/// The newest probe sample plus the C1 generation it is bound to.
struct Settled {
    probe: ProbeSample,
    generation: u64,
}

enum SettleEnd {
    Settled(Settled),
    Timeout { generation: u64, probe_seq: u64 },
    GuestExited(std::process::ExitStatus),
}

/// Wait until the newest C1 frame and the newest in-guest tree sample describe
/// the same, settled UI state.
///
/// A frame can land *before* the app's click has run (touch feedback), and the
/// tree probe can see the post-click state before the post-click frame is
/// stored. Binding those two blindly produced a reply whose tree said `0` and
/// whose pixels said `1` (W10 negative evidence). This is the binding rule the
/// session uses instead:
///
/// * the C1 generation advanced past `min_generation`;
/// * the probe produced a sample **after** the last store (`probe.at_ms >
///   last store time`), so the tree saw the state the stored frame belongs to;
/// * no new store for `quiet` (150 ms), so the frame is not mid-animation;
/// * optionally, the pixels differ from the caller's previous capture
///   (`previous_fnv`), so a tap that changed nothing cannot return a frame.
#[allow(clippy::too_many_arguments)]
fn wait_for_settled_observation(
    child: &mut std::process::Child,
    cgroup: &mut crate::cgroup::RunCgroup,
    live: &acos_services::registry::LiveHandles,
    probe_state: &Arc<std::sync::Mutex<ProbeState>>,
    deadline: Instant,
    min_generation: u64,
    min_probe_seq: u64,
    previous_fnv: Option<u64>,
) -> SettleEnd {
    let quiet = Duration::from_millis(150);
    let mut last_generation = min_generation;
    let mut last_change = Instant::now();
    let mut last_change_ms = host_ms();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return SettleEnd::GuestExited(status),
            _ => {}
        }
        let generation = latest_generation(live);
        if generation > last_generation {
            last_generation = generation;
            last_change = Instant::now();
            last_change_ms = host_ms();
        }
        let probe = latest_probe(probe_state);
        let probe_after = probe
            .as_ref()
            .map(|p| p.at_ms >= last_change_ms)
            .unwrap_or(false);
        if generation > min_generation
            && probe.as_ref().map(|p| p.seq > min_probe_seq).unwrap_or(false)
            && last_change.elapsed() >= quiet
            && probe_after
        {
            let pixels_changed = match previous_fnv {
                None => true,
                Some(previous) => {
                    let current = {
                        let state = live.compositor.lock().unwrap_or_else(|e| e.into_inner());
                        state.capture_latest().map(|c| c.fnv1a)
                    };
                    current != Some(previous) && current.is_some()
                }
            };
            if pixels_changed {
                return SettleEnd::Settled(Settled {
                    probe: probe.expect("probe checked above"),
                    generation,
                });
            }
        }
        if Instant::now() >= deadline {
            return SettleEnd::Timeout {
                generation,
                probe_seq: probe.as_ref().map(|p| p.seq).unwrap_or(0),
            };
        }
        cgroup.sample();
        std::thread::sleep(Duration::from_millis(15));
    }
}

#[allow(clippy::too_many_arguments)]
fn interactive_serve(
    child: &mut std::process::Child,
    cgroup: &mut crate::cgroup::RunCgroup,
    live: &acos_services::registry::LiveHandles,
    probe_state: &Arc<std::sync::Mutex<ProbeState>>,
    started: Instant,
    startup_budget: Duration,
    frames_dir: &Path,
    staging: &Path,
    out: &Path,
    package: &str,
    activity: Option<&str>,
    dog: Option<&MemoryWatchdog>,
    logd: &Logd,
) -> std::io::Result<SessionEnd> {
    let mut evidence: Vec<serde_json::Value> = Vec::new();
    let mut captures = 0u64;
    let startup_deadline = started + startup_budget;
    // Startup: the real Activity is resumed, the guest probe has produced a
    // sample, and C1 has stored at least one buffer. Ready means all three.
    loop {
        if let Some(status) = child.try_wait()? {
            let reply = serde_json::json!({
                "ok": false, "cmd": "ready", "code": "GUEST_EXITED",
                "error": format!("the guest ended before its first live frame (status {status})"),
                "capture_source": CAPTURE_SOURCE,
            });
            protocol_write(&reply);
            evidence.push(reply);
            write_session_evidence(out, &evidence);
            return Ok(SessionEnd { status: Some(status), clean: false });
        }
        if let Some(d) = dog {
            if d.trip().is_some() {
                kill_guest(child, cgroup);
                let reply = serde_json::json!({
                    "ok": false, "cmd": "ready", "code": HOST_MEMORY_LIMIT,
                    "error": "the launcher memory watchdog tripped before the first live frame",
                });
                protocol_write(&reply);
                evidence.push(reply);
                write_session_evidence(out, &evidence);
                return Ok(SessionEnd { status: None, clean: false });
            }
        }
        cgroup.sample();
        let marker = staging.join("activity-resumed.marker").is_file();
        let probe = latest_probe(probe_state);
        let generation = latest_generation(live);
        if marker && probe.is_some() && generation > 0 {
            break;
        }
        if Instant::now() >= startup_deadline {
            kill_guest(child, cgroup);
            let reply = serde_json::json!({
                "ok": false, "cmd": "ready", "code": "NO_LIVE_FRAME",
                "error": format!(
                    "no live frame within {}s: activity_resumed={marker} probe={} c1_generation={generation}",
                    startup_budget.as_secs(),
                    probe.is_some()
                ),
                "capture_source": CAPTURE_SOURCE,
            });
            protocol_write(&reply);
            evidence.push(reply);
            write_session_evidence(out, &evidence);
            return Ok(SessionEnd { status: None, clean: false });
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let probe = latest_probe(probe_state).expect("startup required a probe sample");
    let (width, height) = {
        let state = live.compositor.lock().unwrap_or_else(|e| e.into_inner());
        state
            .newest_frame_layer()
            .map(|(_, buffer)| (buffer.width, buffer.height))
            .unwrap_or((1080, 2400))
    };
    let ready = serde_json::json!({
        "ok": true,
        "cmd": "ready",
        "package": package,
        "activity": activity.unwrap_or(""),
        "width": width,
        "height": height,
        "density": 420,
        "capture_source": CAPTURE_SOURCE,
        "buffer_generation": latest_generation(live),
        "probe_seq": probe.seq,
        "label": "live ART session: real counter.apk Activity, in-guest view tree probe, ACOS C1 buffer captures",
    });
    protocol_write(&ready);
    evidence.push(ready);

    // stdin -> channel, so the loop can also poll the guest and its deadlines.
    let (ctx, crx) = mpsc::channel::<Option<String>>();
    std::thread::spawn(move || {
        use std::io::BufRead;
        let stdin = std::io::stdin();
        let mut lock = stdin.lock();
        let mut line = String::new();
        loop {
            line.clear();
            match lock.read_line(&mut line) {
                Ok(0) => {
                    let _ = ctx.send(None);
                    return;
                }
                Ok(_) => {
                    if ctx.send(Some(line.clone())).is_err() {
                        return;
                    }
                }
                Err(_) => {
                    let _ = ctx.send(None);
                    return;
                }
            }
        }
    });

    let session_deadline = Instant::now() + Duration::from_secs(env_u64("ACOSLAUNCH_SESSION_SECS", 900));
    let ack_wait = Duration::from_millis(env_u64("ACOS_INPUT_ACK_MS", 2500));
    let frame_wait = Duration::from_secs(env_u64("ACOSLAUNCH_FRAME_WAIT_SECS", 15));
    let clean: bool;
    let mut last_capture_fnv: Option<u64> = None;
    loop {
        let line = match crx.recv_timeout(Duration::from_millis(100)) {
            Ok(Some(line)) => Some(line),
            Ok(None) | Err(mpsc::RecvTimeoutError::Disconnected) => None,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                // No command: keep polling the guest and the session deadline.
                if let Some(status) = child.try_wait()? {
                    let reply = serde_json::json!({
                        "ok": false, "cmd": "session", "code": "GUEST_EXITED",
                        "error": format!("the guest ended while the session was idle (status {status})"),
                    });
                    protocol_write(&reply);
                    evidence.push(reply);
                    write_session_evidence(out, &evidence);
                    return Ok(SessionEnd { status: Some(status), clean: false });
                }
                if let Some(d) = dog {
                    if d.trip().is_some() {
                        kill_guest(child, cgroup);
                        let reply = serde_json::json!({
                            "ok": false, "cmd": "session", "code": HOST_MEMORY_LIMIT,
                            "error": "the launcher memory watchdog tripped",
                        });
                        protocol_write(&reply);
                        evidence.push(reply);
                        write_session_evidence(out, &evidence);
                        return Ok(SessionEnd { status: None, clean: false });
                    }
                }
                if Instant::now() >= session_deadline {
                    kill_guest(child, cgroup);
                    let reply = serde_json::json!({
                        "ok": false, "cmd": "session", "code": "TIMEOUT",
                        "error": "the live session exceeded ACOSLAUNCH_SESSION_SECS",
                    });
                    protocol_write(&reply);
                    evidence.push(reply);
                    write_session_evidence(out, &evidence);
                    return Ok(SessionEnd { status: None, clean: false });
                }
                cgroup.sample();
                continue;
            }
        };
        let Some(line) = line else {
            // stdin EOF: the caller went away; stop the guest like `close`.
            clean = true;
            break;
        };
        let text = line.trim();
        if text.is_empty() {
            continue;
        }
        let request: serde_json::Value = match serde_json::from_str(text) {
            Ok(value) => value,
            Err(error) => {
                let reply = serde_json::json!({
                    "ok": false, "cmd": "unknown", "code": "BAD_REQUEST",
                    "error": format!("command line is not JSON: {error}"),
                });
                protocol_write(&reply);
                evidence.push(reply);
                continue;
            }
        };
        let cmd = request.get("cmd").and_then(|v| v.as_str()).unwrap_or("");
        match cmd {
            "tree" => {
                // Bind the tree and the pixels to the same settled state: wait
                // for a probe sample taken after the newest stored frame and for
                // the frame stream to go quiet, then capture.
                let settled = wait_for_settled_observation(
                    child,
                    cgroup,
                    live,
                    probe_state,
                    Instant::now() + Duration::from_millis(1500),
                    0,
                    0,
                    None,
                );
                let probe = match settled {
                    SettleEnd::Settled(settled) => settled.probe,
                    SettleEnd::Timeout { .. } => match latest_probe(probe_state) {
                        Some(probe) => probe,
                        None => {
                            let reply = serde_json::json!({
                                "ok": false, "cmd": "tree", "code": "NO_TEXT",
                                "error": "the in-guest tree probe has produced no sample",
                            });
                            protocol_write(&reply);
                            evidence.push(reply);
                            continue;
                        }
                    },
                    SettleEnd::GuestExited(status) => {
                        let reply = serde_json::json!({
                            "ok": false, "cmd": "tree", "code": "GUEST_EXITED",
                            "error": format!("the guest ended while binding the tree (status {status})"),
                        });
                        protocol_write(&reply);
                        evidence.push(reply);
                        write_session_evidence(out, &evidence);
                        return Ok(SessionEnd { status: Some(status), clean: false });
                    }
                };
                match capture_c1(live, frames_dir, &mut captures) {
                    Ok(Some(capture)) => {
                        last_capture_fnv = Some(capture.fnv1a);
                        let mut reply = capture.fields();
                        let map = reply.as_object_mut().expect("capture fields are an object");
                        map.insert("ok".into(), serde_json::json!(true));
                        map.insert("cmd".into(), serde_json::json!("tree"));
                        map.insert("tree".into(), probe.tree.clone());
                        map.insert("probe_seq".into(), serde_json::json!(probe.seq));
                        map.insert("probe_at_ms".into(), serde_json::json!(probe.at_ms));
                        protocol_write(&reply);
                        evidence.push(serde_json::json!({
                            "cmd": "tree", "ok": true, "frame_seq": capture.seq,
                            "buffer_generation": capture.generation,
                            "frame_number": capture.frame_number,
                            "probe_seq": probe.seq, "png": capture.path.display().to_string(),
                        }));
                    }
                    Ok(None) => {
                        let reply = serde_json::json!({
                            "ok": false, "cmd": "tree", "code": "NO_FRESH_FRAME",
                            "error": "C1 has stored no buffer yet",
                        });
                        protocol_write(&reply);
                        evidence.push(reply);
                    }
                    Err(error) => {
                        let reply = serde_json::json!({
                            "ok": false, "cmd": "tree", "code": "INTERNAL",
                            "error": format!("cannot write the C1 capture: {error}"),
                        });
                        protocol_write(&reply);
                        evidence.push(reply);
                    }
                }
            }
            "screenshot" => match capture_c1(live, frames_dir, &mut captures) {
                Ok(Some(capture)) => {
                    last_capture_fnv = Some(capture.fnv1a);
                    let mut reply = capture.fields();
                    let map = reply.as_object_mut().expect("capture fields are an object");
                    map.insert("ok".into(), serde_json::json!(true));
                    map.insert("cmd".into(), serde_json::json!("screenshot"));
                    map.insert("tree".into(), serde_json::Value::Null);
                    protocol_write(&reply);
                    evidence.push(serde_json::json!({
                        "cmd": "screenshot", "ok": true, "frame_seq": capture.seq,
                        "buffer_generation": capture.generation,
                        "frame_number": capture.frame_number,
                        "png": capture.path.display().to_string(),
                    }));
                }
                Ok(None) => {
                    let reply = serde_json::json!({
                        "ok": false, "cmd": "screenshot", "code": "NO_FRESH_FRAME",
                        "error": "C1 has stored no buffer yet",
                    });
                    protocol_write(&reply);
                    evidence.push(reply);
                }
                Err(error) => {
                    let reply = serde_json::json!({
                        "ok": false, "cmd": "screenshot", "code": "INTERNAL",
                        "error": format!("cannot write the C1 capture: {error}"),
                    });
                    protocol_write(&reply);
                    evidence.push(reply);
                }
            },
            "tap" | "key" | "type_text" => {
                let text_arg = request
                    .get("text")
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                let (x, y, key_code) = if cmd == "tap" {
                    let x = request.get("x").and_then(|v| v.as_i64());
                    let y = request.get("y").and_then(|v| v.as_i64());
                    match (x, y) {
                        (Some(x), Some(y)) => (x as f32, y as f32, None),
                        _ => {
                            let reply = serde_json::json!({
                                "ok": false, "cmd": cmd, "code": "BAD_REQUEST",
                                "error": "tap needs integer x and y",
                            });
                            protocol_write(&reply);
                            evidence.push(reply);
                            continue;
                        }
                    }
                } else if cmd == "type_text" {
                    match text_arg {
                        Some(_) => (0.0, 0.0, None),
                        None => {
                            let reply = serde_json::json!({
                                "ok": false, "cmd": cmd, "code": "BAD_REQUEST",
                                "error": "type_text needs a string text",
                            });
                            protocol_write(&reply);
                            evidence.push(reply);
                            continue;
                        }
                    }
                } else {
                    let code = request.get("code").and_then(|v| v.as_i64());
                    match code {
                        Some(code) => (0.0, 0.0, Some(code as i32)),
                        None => {
                            let reply = serde_json::json!({
                                "ok": false, "cmd": cmd, "code": "BAD_REQUEST",
                                "error": "key needs an integer code",
                            });
                            protocol_write(&reply);
                            evidence.push(reply);
                            continue;
                        }
                    }
                };
                let p0 = latest_probe(probe_state).map(|p| p.seq).unwrap_or(0);
                let g0 = latest_generation(live);
                let injected = match (cmd, key_code) {
                    ("tap", _) => live.input.tap(x, y, ack_wait),
                    ("type_text", _) => live
                        .input
                        .type_text(text_arg.as_deref().unwrap_or_default(), ack_wait),
                    (_, Some(code)) => live.input.key(code, ack_wait),
                    _ => unreachable!(),
                };
                let injected = match injected {
                    Ok(injected) => injected,
                    Err(error) => {
                        let reply = serde_json::json!({
                            "ok": false, "cmd": cmd, "code": "INPUT_UNAVAILABLE",
                            "error": format!("the real InputChannel accepted no gesture: {error}"),
                        });
                        protocol_write(&reply);
                        evidence.push(reply);
                        continue;
                    }
                };
                let settled = wait_for_settled_observation(
                    child,
                    cgroup,
                    live,
                    probe_state,
                    Instant::now() + frame_wait,
                    g0,
                    p0,
                    last_capture_fnv,
                );
                let (settled_probe, g1, p1) = match settled {
                    SettleEnd::Settled(settled) => (
                        Some(settled.probe.clone()),
                        settled.generation,
                        settled.probe.seq,
                    ),
                    SettleEnd::Timeout {
                        generation,
                        probe_seq,
                    } => (None, generation, probe_seq),
                    SettleEnd::GuestExited(status) => {
                        let reply = serde_json::json!({
                            "ok": false, "cmd": cmd, "code": "GUEST_EXITED",
                            "error": format!(
                                "the guest ended after the gesture without a settled frame (status {status})"
                            ),
                            "gesture": {"seqs": injected.seqs, "token": format!("{:#x}", injected.token.0)},
                        });
                        protocol_write(&reply);
                        evidence.push(reply);
                        write_session_evidence(out, &evidence);
                        return Ok(SessionEnd { status: Some(status), clean: false });
                    }
                };
                if settled_probe.is_none() {
                    let reply = serde_json::json!({
                        "ok": false, "cmd": cmd, "code": "NO_FRESH_FRAME",
                        "error": format!(
                            "{cmd} at ({x},{y}) completed no new settled observation within {}s: \
                             c1_generation {g0} -> {g1}, probe_seq {p0} -> {p1}, and the pixels \
                             did not change from the previous capture",
                            frame_wait.as_secs()
                        ),
                        "gesture": {
                            "seqs": injected.seqs,
                            "acks": injected.acks.iter().map(|a| serde_json::json!({
                                "seq": a.seq, "handled": a.handled, "latency_us": a.latency_us,
                            })).collect::<Vec<_>>(),
                            "token": format!("{:#x}", injected.token.0),
                        },
                    });
                    protocol_write(&reply);
                    evidence.push(reply);
                    continue;
                }
                let capture = match capture_c1(live, frames_dir, &mut captures) {
                    Ok(Some(capture)) => capture,
                    Ok(None) => {
                        let reply = serde_json::json!({
                            "ok": false, "cmd": cmd, "code": "NO_FRESH_FRAME",
                            "error": "the guest reported a new frame but C1 stored no buffer",
                        });
                        protocol_write(&reply);
                        evidence.push(reply);
                        continue;
                    }
                    Err(error) => {
                        let reply = serde_json::json!({
                            "ok": false, "cmd": cmd, "code": "INTERNAL",
                            "error": format!("cannot write the C1 capture: {error}"),
                        });
                        protocol_write(&reply);
                        evidence.push(reply);
                        continue;
                    }
                };
                last_capture_fnv = Some(capture.fnv1a);
                let probe = settled_probe.expect("settled probe checked above");
                let mut reply = capture.fields();
                let map = reply.as_object_mut().expect("capture fields are an object");
                map.insert("ok".into(), serde_json::json!(true));
                map.insert("cmd".into(), serde_json::json!(cmd));
                map.insert("tree".into(), probe.tree.clone());
                map.insert("probe_seq".into(), serde_json::json!(probe.seq));
                map.insert("probe_at_ms".into(), serde_json::json!(probe.at_ms));
                map.insert(
                    "gesture".into(),
                    serde_json::json!({
                        "x": x, "y": y,
                        "seqs": injected.seqs,
                        "acks": injected.acks.iter().map(|a| serde_json::json!({
                            "seq": a.seq, "handled": a.handled, "latency_us": a.latency_us,
                        })).collect::<Vec<_>>(),
                        "token": format!("{:#x}", injected.token.0),
                        "new_buffer_generation": g1,
                        "new_probe_seq": p1,
                    }),
                );
                protocol_write(&reply);
                evidence.push(serde_json::json!({
                    "cmd": cmd, "ok": true, "frame_seq": capture.seq,
                    "buffer_generation": capture.generation,
                    "frame_number": capture.frame_number,
                    "probe_seq": probe.seq,
                    "x": x, "y": y,
                    "handled": injected.acks.iter().map(|a| a.handled).collect::<Vec<_>>(),
                    "png": capture.path.display().to_string(),
                }));
            }
            "logs" => {
                // W22: the guest's own logd sink, snapshotted live. The CLI maps
                // an entry's absolute ordinal to `LogRecord.seq`; entries are
                // guest-asserted (docs/SANDBOX_CERTIFICATION.md §7) and never
                // justify a pass on an untrusted run.
                let entries = logd.snapshot();
                let stats = logd.stats();
                let reply = serde_json::json!({
                    "ok": true, "cmd": "logs",
                    "total": stats.total,
                    "kept": entries.len(),
                    "dropped": stats.dropped,
                    "cap": stats.cap,
                    "entries": entries,
                });
                protocol_write(&reply);
                evidence.push(serde_json::json!({
                    "cmd": "logs", "ok": true,
                    "total": stats.total, "kept": entries.len(), "dropped": stats.dropped,
                }));
            }
            "reset" => {
                let reply = serde_json::json!({
                    "ok": false, "cmd": "reset", "code": "UNSUPPORTED_ANDROID_API",
                    "error": "the guest session cannot reset itself; the CLI relaunches the Activity in a fresh guest process for reset",
                });
                protocol_write(&reply);
                evidence.push(reply);
            }
            "close" => {
                let reply = serde_json::json!({
                    "ok": true, "cmd": "close",
                    "frame_seq": captures,
                    "capture_source": CAPTURE_SOURCE,
                });
                protocol_write(&reply);
                evidence.push(reply);
                clean = true;
                break;
            }
            other => {
                let reply = serde_json::json!({
                    "ok": false, "cmd": other, "code": "UNSUPPORTED_ANDROID_API",
                    "error": format!("the live launch session has no `{other}` command"),
                });
                protocol_write(&reply);
                evidence.push(reply);
            }
        }
    }
    kill_guest(child, cgroup);
    write_session_evidence(out, &evidence);
    Ok(SessionEnd {
        status: child.try_wait().ok().flatten(),
        clean,
    })
}

// ----------------------------------------------------------------- PNG encoder
//
// `acos-art` deliberately has no image dependency (the render path uses the
// guest's own Bitmap encoder). A live capture has no guest-side bitmap, so a
// minimal RGBA8 PNG writer lives here: filter type 0 per row, zlib "stored"
// deflate blocks, real CRC32/Adler-32. Deterministic and byte-checkable.

fn encode_png_rgba(width: u32, height: u32, stride: u32, rgba: &[u8]) -> Vec<u8> {
    let row = (width as usize).saturating_mul(4);
    let stride_bytes = (stride as usize).saturating_mul(4);
    let mut filtered = Vec::with_capacity((row + 1).saturating_mul(height as usize));
    for y in 0..height as usize {
        filtered.push(0u8);
        let start = y * stride_bytes;
        let end = start + row;
        if end <= rgba.len() {
            filtered.extend_from_slice(&rgba[start..end]);
        } else {
            filtered.resize(filtered.len() + row, 0);
        }
    }
    let mut out = Vec::with_capacity(filtered.len() + 1024);
    out.extend_from_slice(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]);
    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]); // 8-bit RGBA, no interlace
    png_chunk(&mut out, b"IHDR", &ihdr);

    let mut zlib = Vec::with_capacity(filtered.len() + 16);
    zlib.extend_from_slice(&[0x78, 0x01]);
    let mut offset = 0usize;
    while offset < filtered.len() {
        let len = (filtered.len() - offset).min(65_535);
        let final_block = offset + len == filtered.len();
        zlib.push(if final_block { 1 } else { 0 });
        zlib.extend_from_slice(&(len as u16).to_le_bytes());
        zlib.extend_from_slice(&(!(len as u16)).to_le_bytes());
        zlib.extend_from_slice(&filtered[offset..offset + len]);
        offset += len;
    }
    zlib.extend_from_slice(&adler32(&filtered).to_be_bytes());
    png_chunk(&mut out, b"IDAT", &zlib);
    png_chunk(&mut out, b"IEND", &[]);
    out
}

fn png_chunk(out: &mut Vec<u8>, kind: &[u8; 4], body: &[u8]) {
    out.extend_from_slice(&(body.len() as u32).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(body);
    let mut crc_input = Vec::with_capacity(4 + body.len());
    crc_input.extend_from_slice(kind);
    crc_input.extend_from_slice(body);
    out.extend_from_slice(&crc32(&crc_input).to_be_bytes());
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xffff_ffffu32;
    for &byte in data {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xedb8_8320 & mask);
        }
    }
    !crc
}

fn adler32(data: &[u8]) -> u32 {
    let mut a = 1u32;
    let mut b = 0u32;
    for &byte in data {
        a = (a + u32::from(byte)) % 65_521;
        b = (b + a) % 65_521;
    }
    (b << 16) | a
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
