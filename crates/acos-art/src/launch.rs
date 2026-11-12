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
    /// The APK's launcher component (`PackageParser` rules: `<activity>` or
    /// `<activity-alias>` with MAIN/LAUNCHER), the component the
    /// `LaunchActivityItem` intent names.
    pub activity: Option<String>,
    /// `ActivityInfo.targetActivity` when the launcher entry is an alias (the
    /// class the framework actually instantiates). `None` for a plain activity.
    pub activity_target: Option<String>,
    /// Effective `Activity.mResumed` wait the guest used (W27).
    #[serde(default)]
    pub timeout_ms: Option<u64>,
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
    /// W48: the `dalvik.vm.heap*` values this launch actually served (the
    /// `ACOS_ART_HEAP*` seam resolved and validated), so a benchmark row names
    /// the heap configuration that produced it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub heap: Option<serde_json::Value>,
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

/// B3: opt-in **shared** boot image (arm (b) of the Zygote comparison).
///
/// `ACOS_SHARED_BOOT_IMAGE=<dir>` points the launch at an *uncompressed* boot
/// image built by `tools/art-runtime/b2/build-uncompressed.sh --out <dir>`
/// (B2's recipe: `--image-format=uncompressed --base=0x70000000`). The launch
/// then passes `-Ximage:/data/img/boot.art -Xnorelocate`: ART maps the image at
/// its recorded base, the pages stay file-backed and are shared between
/// sessions instead of being LZ4-decompressed into private anon memory (B2:
/// 26.3 -> 12.7 MiB private + 25.7 MiB shared per session).
///
/// `-Xnorelocate` turns **image ASLR off** (fixed base 0x70000000), so this is
/// a trusted-workloads-only experiment seam. It is OFF by default and unset
/// means the argv is byte-for-byte what it was before (no image flag at all).
pub const ENV_SHARED_BOOT_IMAGE: &str = "ACOS_SHARED_BOOT_IMAGE";

/// Resolve the `ACOS_SHARED_BOOT_IMAGE` seam; `None` = default (LZ4, unchanged).
///
/// `Some(dir)` must hold `<dir>/x86_64/boot.art`: ART appends the instruction
/// set to an `-Ximage` location (`image_space.cc:136`, `GetSystemImageFilename`).
/// A set-but-unusable value is an error, never a silent fallback to the
/// compressed image, so a typo cannot masquerade as a measurement.
fn resolve_shared_boot_image(raw: Option<std::ffi::OsString>) -> Result<Option<PathBuf>> {
    let Some(raw) = raw else { return Ok(None) };
    let dir = PathBuf::from(raw);
    if dir.join("x86_64/boot.art").is_file() {
        Ok(Some(dir))
    } else {
        Err(Error::Layout(format!(
            "{ENV_SHARED_BOOT_IMAGE}={}: no x86_64/boot.art (unset it for the default LZ4 image)",
            dir.display()
        )))
    }
}

/// The session-independent paths built once by the ZM2 host template.
///
/// With `ACOS_HOST_ZYGOTE_PLAN` unset these are local values, so the production
/// path is unchanged; the struct only exists to hand the same paths to fork
/// children of [`crate::host_zygote`].
pub struct SessionShims {
    pub substrate: PathBuf,
    pub art_shim: PathBuf,
    pub driver: PathBuf,
}

/// A service graph built before the fork (ZM2 prototype C).
pub type PrebuiltHost = (
    acos_services::ServiceHost,
    acos_services::registry::LiveHandles,
);

/// W1: the launcher's frame-sized capture buffers go through `mmap`, so a
/// free returns them to the OS instead of leaving them committed in a glibc
/// worker-thread arena. glibc's dynamic threshold otherwise grows past a
/// 9.9 MiB capture buffer, and its top-trim never runs in those arenas; ZM2
/// measured that as 16.29 MiB of a 20.48 MiB launcher at N=5, still resident
/// long after the buffers were dropped. 128 KiB is the value ZM2 measured
/// (``ACOS_ART_MALLOC_MMAP_THRESHOLD_KB=128``): launcher 20.48 -> 4.01 MiB at
/// N=5 with identical PNGs and no latency cost.
const DEFAULT_MMAP_THRESHOLD_KB: i32 = 128;

/// Install the launcher's allocator defaults, then apply the ZM2 test knobs.
///
/// The default (`M_MMAP_THRESHOLD = 128 KiB`) is **always** applied, before any
/// thread is spawned, so the production path needs no environment variable.
///
/// * `ACOS_ART_MALLOC_ARENA_MAX=N` -> `mallopt(M_ARENA_MAX, N)` (1 = every
///   thread allocates from the main arena, so `malloc_trim` can reach it).
/// * `ACOS_ART_MALLOC_MMAP_THRESHOLD_KB=N` -> `mallopt(M_MMAP_THRESHOLD, N*1024)`
///   (overrides the default: 128 = frame buffers are mmap'd and returned on
///   free).
/// * `ACOS_ART_MALLOC_TRIM_THRESHOLD_KB=N` -> `mallopt(M_TRIM_THRESHOLD, N*1024)`.
///
/// `ACOS_ART_TRIM_AFTER_CAPTURE=1` (in `capture_c1`) still calls
/// `malloc_trim(0)`; it is not needed with the threshold above.
fn apply_malloc_controls() {
    #[cfg(target_env = "gnu")]
    unsafe {
        libc::mallopt(
            libc::M_MMAP_THRESHOLD,
            DEFAULT_MMAP_THRESHOLD_KB.saturating_mul(1024),
        );
        let int_env = |name: &str| -> Option<i32> {
            std::env::var(name)
                .ok()
                .and_then(|v| v.trim().parse::<i32>().ok())
                .filter(|v| *v >= 0)
        };
        if let Some(n) = int_env("ACOS_ART_MALLOC_ARENA_MAX") {
            libc::mallopt(libc::M_ARENA_MAX, n);
        }
        if let Some(kb) = int_env("ACOS_ART_MALLOC_MMAP_THRESHOLD_KB") {
            libc::mallopt(libc::M_MMAP_THRESHOLD, kb.saturating_mul(1024));
        }
        if let Some(kb) = int_env("ACOS_ART_MALLOC_TRIM_THRESHOLD_KB") {
            libc::mallopt(libc::M_TRIM_THRESHOLD, kb.saturating_mul(1024));
        }
    }
}

/// Run the real `ActivityThread.main`-style startup until `Application.onCreate`.
pub fn launch(root: &ArtRoot, opts: &LaunchOpts) -> Result<LaunchResult> {
    // W39 (flagged call-site edit in the W36-owned launch.rs): stage markers for
    // the exec -> activity-resumed wall. Inert unless ACOS_ART_STAGE_TIMING=1.
    crate::stage("launch-entry");
    // W1: frame-sized capture buffers must be mmap-backed (and the ZM2 knobs
    // still apply on top). This runs before any thread or allocation below, so
    // every later capture buffer is covered. See results/art-runtime/ZM2.md
    // §4.1 for the retention this removes.
    apply_malloc_controls();
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
    crate::stage("linkerconfig");
    root.ensure_classpath_env(false)?;
    crate::stage("classpath");
    // Guest dex (contains AppMain, LaunchMain and LaunchMain$Instr), the
    // substrate JNI shim and the mini-root shim.
    let _guest = build_launch_guest(root)?;
    crate::stage("guest-dex");
    let substrate = crate::app::build_render_shim(root)?;
    crate::stage("render-shim");
    let art_shim = crate::build_shim(&root.data.join("shim"))?;
    crate::stage("art-shim");
    let driver = build_driver(root)?;
    crate::stage("binder-driver");
    let shims = SessionShims {
        substrate,
        art_shim,
        driver,
    };
    // ZM2 prototype C (research seam, off by default): with a plan, this
    // process is the warm host template that forks one launcher per session.
    // Unset => the exact production path below.
    if let Some(plan) = std::env::var_os(crate::host_zygote::ENV_PLAN) {
        return crate::host_zygote::run_template(root, Path::new(&plan), shims);
    }
    launch_session(root, opts, None, &shims)
}

/// The per-session half of [`launch`].
///
/// `prebuilt` is `Some` only in a child of the ZM2 host template: the child
/// installs its own APK into its private CoW copy of that empty service graph.
pub(crate) fn launch_session(
    root: &ArtRoot,
    opts: &LaunchOpts,
    prebuilt: Option<PrebuiltHost>,
    shims: &SessionShims,
) -> Result<LaunchResult> {
    // Z4 (research seam, off by default): `ACOS_ZYGOTE_SESSION=<dir>` makes this
    // launcher a *foreign-guest* session of a shared zygote. Unset => production.
    let session_dir: Option<PathBuf> =
        std::env::var_os("ACOS_ZYGOTE_SESSION").map(PathBuf::from);
    // Private per-run data root (S4): app data, out staging and the importer
    // socket are this run's; everything else under /data is read-only.
    let run = match &session_dir {
        Some(d) => root.run_dir_at(&d.join("run"), "launch")?,
        None => root.new_run_dir("launch")?,
    };
    crate::stage("run-dir");

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
    crate::stage("apk-stage");

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
            eprintln!("acos-art: launch: UNSUPPORTED_ABI available={available:?}: {message}");
        }
        acos_services::native_libs::NativeLibInstall::None => {}
    }
    crate::stage("native-libs");
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

    // W24: the image's **static RROs** for `framework-res`, applied through the
    // same stock path `render-app`/the session already use (P2c/W18 #2). The
    // device's `AssetManager.createSystemAssetsInZygoteLocked` loads every
    // immutable overlay idmap under `/system/framework/framework-res.apk`; the
    // base image's `framework-res.apk` alone says
    // `config_buttonTextAllCaps=true`, while the product RRO
    // (`PixelConfigOverlayCommon`) says `false`. Without this the live guest
    // renders `INCREMENT` where the phone renders `Increment`. The idmaps are
    // generated once by the image's own `/system/bin/idmap2` and land in
    // `<data>/overlay`, which a run already sees read-only at `/data/overlay`.
    let mut overlay_diags = Vec::new();
    let overlays = crate::app::provision_overlays(root, &mut overlay_diags)?;
    crate::stage("overlays");
    for d in &overlay_diags {
        eprintln!("acos-art: launch: {d}");
    }

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
    // W43: a full scratch disk is a host condition and a structured, retryable
    // refusal *before* any guest starts (the 2026-11-12 03:23Z ENOSPC window
    // produced `exit status: 1` guests, `INTERNAL: cannot write the C1 capture`
    // and a caller-side panic instead).
    {
        let free = disk_free_bytes(&out);
        let min = env_u64("ACOS_ART_MIN_DISK_MB", 512) * 1024 * 1024;
        if disk_full_for_run(free, min) {
            let free = free.unwrap_or(0);
            let result = disk_full_result(opts, &package, free, min);
            write_session_exit(&out, &result, None);
            // An interactive caller waits for a *ready line*: refusing without
            // one would look like a dead session (`INTERNAL`) to it. Name the
            // host condition on the protocol instead.
            if std::env::var_os("ACOSLAUNCH_INTERACTIVE").is_some() {
                protocol_write(&serde_json::json!({
                    "ok": false, "cmd": "ready", "code": "HOST_DISK_FULL",
                    "retryable": true,
                    "error": format!(
                        "the scratch filesystem has {free} bytes free, below ACOS_ART_MIN_DISK_MB={} MiB",
                        min / 1024 / 1024
                    ),
                }));
            }
            return Ok(result);
        }
    }
    install_session_panic_hook(&out);

    crate::Props::for_launch()?.write(&root.data.join(crate::props::PROPS_FILE))?;
    // W43: sockets live in a short runtime dir, never under the caller's data
    // path (a long `--data`/task name must not turn into a bind() failure).
    let sockets = match &session_dir {
        Some(d) => ShortSocketDir::at(&d.join("sock"))?,
        None => ShortSocketDir::new().map_err(Error::Io)?,
    };
    let logd = Logd::start(&sockets.0).map_err(Error::Io)?;
    crate::stage("props-logd");

    // --- peer (in-process, real ServiceHost) -------------------------------
    let sock = sockets.0.join("peer.sock");
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
        // ZM2 prototype C: a host-template child starts from the template's
        // empty (fully installed) service graph, which is CoW-shared until the
        // child writes it. The APK install below is per session either way.
        let (mut host, handles) = match prebuilt {
            Some(pair) => pair,
            None => {
                let mut host = acos_services::ServiceHost::new();
                let handles = acos_services::registry::install_with_handles(&mut host);
                (host, handles)
            }
        };
        if let Err(e) = host.install_package_from_apk(&apk_for_peer) {
            eprintln!("acos-art: launch: install_package_from_apk failed: {e}");
        }
        // W38 (flagged, minimal): the guest's `startActivity` is resolved by the
        // host ATMS before it records a `PendingLaunch`; seed the same APK's
        // component table the PMS just got, so an explicit-component launch is
        // `START_SUCCESS` instead of `START_NOT_FOUND`.
        if let Ok(object) = host.ensure("android.app.IActivityTaskManager") {
            if let Some(atms) = object
                .as_any_mut()
                .and_then(|any| any.downcast_mut::<acos_services::atms::AtmsService>())
            {
                if let Err(e) = atms.seed_manifest_from_apk(&apk_for_peer) {
                    eprintln!("acos-art: launch: seed_manifest_from_apk failed: {e}");
                }
            }
        }
        (host, handles)
    };
    let (peer_host, live) = live_handles;
    let interactive = std::env::var_os("ACOSLAUNCH_INTERACTIVE").is_some();
    crate::stage("peer-setup");
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
                let tv = libc::timeval {
                    tv_sec: 0,
                    tv_usec: 100_000,
                };
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
    let launcher = if opts.until == "activity-resumed" {
        Some(apk_launcher_activity(&opts.apk)?)
    } else {
        None
    };
    let activity = launcher.as_ref().map(|l| l.component.clone());
    let activity_target = launcher
        .as_ref()
        .and_then(|l| l.target.clone())
        .unwrap_or_default();
    // W24: `PackageParser`'s label fields, which the guest copies into
    // `ApplicationInfo.labelRes`/`nonLocalizedLabel` and
    // `ActivityInfo.labelRes`/`nonLocalizedLabel`. Stock
    // `ActivityThread.performLaunchActivity` titles the window with
    // `activityInfo.loadLabel(pm)`; with both fields unset that walk ends at
    // `ActivityInfo.name` (the component FQN), which is what W18 #1 measured
    // (`dev.acos.fixtures.counter.MainActivity` instead of `Counter`).
    let labels = acos_services::pms::manifest_labels(&opts.apk)
        .map_err(|e| Error::Layout(format!("manifest labels: {e}")))?;
    // W27 item 2: the resume budget is configurable. The default comes from the
    // APK shape (multidex / large native closure -> 45 s, ordinary app -> 12 s,
    // see `acos_services::manifest_resolve::launch_budget`); an explicit
    // `ACOSLAUNCH_RESUME_MS` in the caller's environment always wins.
    let budget = acos_services::manifest_resolve::launch_budget(&opts.apk).unwrap_or(
        acos_services::manifest_resolve::LaunchBudget {
            dex_count: 0,
            native_bytes: 0,
            resume_ms: acos_services::manifest_resolve::RESUME_MS_BASE,
        },
    );
    let resume_ms = std::env::var("ACOSLAUNCH_RESUME_MS")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|v| *v >= 1000)
        .unwrap_or(budget.resume_ms);
    eprintln!(
        "acos-art: launch: resume budget={resume_ms}ms (dex={} native_bytes={} default={}ms){}",
        budget.dex_count,
        budget.native_bytes,
        budget.resume_ms,
        if resume_ms == budget.resume_ms {
            ""
        } else {
            " (ACOSLAUNCH_RESUME_MS)"
        }
    );
    let activity_label = launcher
        .as_ref()
        .map(|l| l.label.clone())
        .unwrap_or_default();
    // W31: `ApplicationInfo.flags` from the parsed manifest. `PmsState::from_apk`
    // is the host's `PackageParser` equivalent (`acos-apk`'s typed manifest);
    // its `AppInfo.flags` is what a package manager hands the framework, and
    // `FLAG_INSTALLED` is the post-install bit PMS sets. `FLAG_HARDWARE_ACCELERATED`
    // is cleared here, once, because the surface-less ACOS window draws through
    // the software `ViewRootImpl` path (P4b §4) and the guest must not be told
    // otherwise. No APK-specific bit is synthesised in the guest.
    const FLAG_INSTALLED: i32 = 1 << 23;
    const FLAG_HARDWARE_ACCELERATED: i32 = 1 << 29;
    let mut app_flags = installed
        .as_ref()
        .map(|state| ((state.app.flags & !FLAG_HARDWARE_ACCELERATED) | FLAG_INSTALLED) as i64)
        .unwrap_or(-1);
    // Diagnostic seam (W31): `ACOSLAUNCH_APP_FLAGS=0x…` (or decimal) lets a run
    // prove a *future* manifest derivation end to end before the host PMS owns
    // it (`crates/acos-apk::manifest::Manifest::application_flags` is unit
    // tested on counter + Gaten). It never invents bits: the value is logged
    // and recorded in `application-created.marker` as `app_flags=…`.
    let mut app_flags_source = if installed.is_some() { "manifest" } else { "default" };
    if let Ok(raw) = std::env::var("ACOSLAUNCH_APP_FLAGS") {
        let text = raw.trim();
        let parsed = if let Some(hex) = text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
            i64::from_str_radix(hex, 16).ok()
        } else {
            text.parse::<i64>().ok()
        };
        if let Some(value) = parsed {
            if (0..=u32::MAX as i64).contains(&value) {
                app_flags = value;
                app_flags_source = "env";
            }
        }
    }
    eprintln!(
        "acos-art: launch: app flags=0x{:08x} (source={}; installed={})",
        app_flags.max(0),
        app_flags_source,
        installed.is_some()
    );
    // Z3 (research seam, off by default): `ACOS_ZYGOTE_APP=<build dir>` runs the
    // guest as a pre-initialised ART zygote (`dev.acos.art.ZygoteApp`, built by
    // `research/zygote-proto/z3/build.sh`) that does the app-independent
    // framework init once, then forks one child which re-enters the *unmodified*
    // `LaunchMain.main` with the argv built here. Unset => this function's argv,
    // class path and sandbox are exactly what they were before this seam.
    // See results/art-runtime/Z3.md.
    let zygote_app: Option<PathBuf> = std::env::var_os("ACOS_ZYGOTE_APP").map(PathBuf::from);
    // B3: opt-in shared boot image, resolved before the sandbox is wired so a
    // bad value fails the launch instead of silently running the LZ4 image.
    let shared_boot_image = resolve_shared_boot_image(std::env::var_os(ENV_SHARED_BOOT_IMAGE))?;
    let mut argv: Vec<String> = Vec::new();
    if shared_boot_image.is_some() {
        // Exactly B2's pair (`with_image` below ro-binds the dir to /data/img).
        argv.push("-Ximage:/data/img/boot.art".into());
        argv.push("-Xnorelocate".into());
    }
    if let Some(dir) = &shared_boot_image {
        eprintln!(
            "acos-art: launch: shared boot image {} (-Ximage:/data/img/boot.art -Xnorelocate; fixed base, no image ASLR)",
            dir.display()
        );
    }
    // Options before the `-cp <dex> <class>` triple (the Z3 seam keeps them).
    let image_prefix = argv.len();
    argv.extend(vec![
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
        // W24: static-RRO idmap paths (`,`-separated, empty = none) and the
        // manifest label fields (`res` then literal, for the application and
        // the launched activity).
        overlays.guest_arg(),
        labels.application.res.to_string(),
        labels.application.literal.clone().unwrap_or_default(),
        activity_label.res.to_string(),
        activity_label.literal.clone().unwrap_or_default(),
        // W27: the alias's targetActivity (empty for a plain activity).
        activity_target.clone(),
        // W31: manifest-derived `ApplicationInfo.flags` (`-1` = none) and the
        // source label recorded in `application-created.marker`.
        app_flags.to_string(),
        app_flags_source.to_string(),
    ]);
    // Z4: the app half of the argv, i.e. what `LaunchMain.main` receives. The
    // zygote child gets exactly these in its per-session contract.
    let guest_args: Vec<String> = argv.iter().skip(image_prefix + 3).cloned().collect();
    let argv: Vec<String> = match &zygote_app {
        None => argv,
        Some(dir) => {
            eprintln!(
                "acos-art: launch: Z3 zygote guest {} (app child forked from a pre-initialised ART)",
                dir.display()
            );
            let mut v: Vec<String> = argv[..image_prefix].to_vec();
            v.extend(vec![
                "-Xzygote".into(),
                "-Xhidden-api-policy:disabled".into(),
                "-cp".into(),
                "/data/app/z3/classes.dex".into(),
                "dev.acos.art.ZygoteApp".into(),
            ]);
            // Drop the production `-cp <dex> <class>` triple; the guest argv
            // (`apk package out until activity …`) is passed through verbatim.
            v.extend(guest_args.iter().cloned());
            v
        }
    };
    // Z4 research seam: publish this session's contract for the zygote. The
    // directory already exists (the driver prepared it); the guest argv is one
    // argument per line (the 15 production arguments).
    if let Some(dir) = &session_dir {
        write_zygote_session(dir, &run, &sockets.0, &sock, &staging, &out, &package, &guest_args)?;
    }
    crate::seccomp::enable(root)?;
    let mut sb = root.sandbox();
    if let Some(dir) = &shared_boot_image {
        sb.with_image(Some(dir.clone()));
    }
    sb.run_root(run.path());
    // Z4: a foreign-guest session does not spawn ART itself, so the guest
    // seccomp/Landlock guard is applied per forked child (the child loads the
    // same `libacos_seccomp_guest.so` after its bind mounts, exactly AOSP's
    // order). The production path keeps the preload-at-start ordering.
    sb.seccomp_guest(session_dir.is_none());
    sb.with_shim(false);
    sb.ro_bind(&shims.art_shim, "/data/shim/libartshim.so");
    // The per-run extracted native libraries (installd's `nativeLibraryDir`).
    sb.ro_bind(
        &native_lib_root,
        acos_services::native_libs::GUEST_NATIVE_LIB_ROOT,
    );
    sb.ro_bind(&shims.substrate, "/data/app/libacosart.so");
    sb.ro_bind(
        &root.sysroot.join("shim/libacoshw.so"),
        "/data/libacoshw.so",
    );
    sb.ro_bind(&shims.driver, "/data/lib/libacos_binder_driver.so");
    if let Some(dir) = &zygote_app {
        // Z3 research guest dex + the raw-syscall fork shim (Z1/Z2). bwrap
        // creates no mount point inside the read-only /data tree, so the
        // guest-visible directory is seeded in the data root (like
        // `app/launch-guest` above) and the built dex is bound over it.
        fs::create_dir_all(root.data.join("app/z3"))?;
        fs::create_dir_all(root.data.join("shim"))?;
        fs::copy(dir.join("lib/libz2fork.so"), root.data.join("shim/libz2fork.so"))?;
        sb.ro_bind(&dir.join("dex"), "/data/app/z3");
        sb.ro_bind(&dir.join("lib/libz2fork.so"), "/data/shim/libz2fork.so");

    }
    sb.rw_bind(&sockets.0, "/dev/socket");
    // The importer socket is the only writable file the peer needs in /data.
    sb.rw_bind(&sock, "/data/acos-launch.sock");
    // Z4: the plan directory (one subdirectory per session) must be visible to
    // the zygote so its children can bind each session's run root and sockets.
    if let Some(plan) = std::env::var_os("ACOS_ZYGOTE_PLAN_DIR") {
        sb.rw_bind(PathBuf::from(plan), "/sessions");
        // Z4: the zygote reads the per-session contracts from `/sessions`.
        sb.env("ACOS_ZYGOTE_SESSIONS", "/sessions");
        if let Ok(n) = std::env::var("ACOS_ZYGOTE_N") {
            sb.env("ACOS_ZYGOTE_N", &n);
        }
    }
    if session_dir.is_some() {
        // ZS/Z4: forward the research security-probe switch into the child.
        if std::env::var_os("ACOS_ZYGOTE_PROBE").is_some() {
            sb.env("ACOS_ZYGOTE_PROBE", "1");
        }
        // The child loads the production guard itself; it must see the blob.
        sb.ro_bind(&root.data.join("seccomp"), "/data/seccomp");
        if let Some(dir) = &zygote_app {
            // Z4: same source, same blob, but loadable via System.load from a
            // classloader namespace (no __stack_chk_fail reference). Pushed
            // after the directory bind so it wins.
            sb.ro_bind(
                &dir.join("lib/libacos_seccomp_guest.so"),
                "/data/seccomp/libacos_seccomp_guest.so",
            );
        }
    }
    sb.env(
        "LD_PRELOAD",
        "/data/shim/libartshim.so:/data/app/libacosart.so:/data/libacoshw.so:/data/lib/libacos_binder_driver.so",
    );
    sb.env("ACOSART_LOAD", "libandroid_runtime.so");
    sb.env("E3G_SOCK", "/data/acos-launch.sock");
    sb.env("ACOSLAUNCH_RESUME_MS", &resume_ms.to_string());
    // W47: the guest's bind/launch Configuration must carry the same
    // screen-layout size/long bits as the WMS `MergedConfiguration`.
    // Without them (`screenLayout` left at 0) `ResTable_config` selected the
    // `values-xlarge` dimens (dialog_min_width_minor 72% instead of 95%), so a
    // WRAP_CONTENT dialog measured 776 px instead of the stock 1024 px.
    sb.env(
        "ACOSLAUNCH_SCREEN_LAYOUT",
        &acos_services::wms::REFERENCE_SCREEN_LAYOUT.to_string(),
    );
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
    // P11: the guest owns the framework-signal verdict, so the settle switch and
    // its queue horizon must cross the sandbox boundary (the guest env is
    // cleared). Unset means quiet, exactly as before.
    if let Ok(value) = std::env::var("ACOS_STRICT_SETTLE") {
        sb.env("ACOS_STRICT_SETTLE", &value);
    }
    if let Ok(value) = std::env::var("ACOS_STRICT_SETTLE_HORIZON_MS") {
        sb.env("ACOS_STRICT_SETTLE_HORIZON_MS", &value);
    }
    crate::watchdog::note_guest_priority_gap("launch");
    // Z4 (research seam): each forked guest writes its stdout/stderr to its own
    // session FIFO, so the per-session probe parser sees only that child's
    // lines (`ACOSPROBE`, markers) — the zygote's stdout is launcher 0's.
    let guest_fifo = session_dir.as_ref().map(|d| d.join("guest.fifo"));
    if let Some(fifo) = &guest_fifo {
        let _ = fs::remove_file(fifo);
        let c = std::ffi::CString::new(fifo.to_string_lossy().to_string())
            .map_err(|e| Error::Layout(e.to_string()))?;
        if unsafe { libc::mkfifo(c.as_ptr(), 0o600) } != 0 {
            return Err(Error::Io(std::io::Error::last_os_error()));
        }
    }
    let dalvik = "/system/apex/com.android.art/bin/dalvikvm64";
    // Z4 (research seam): a foreign-guest session starts a tombstone process
    // inside its own cgroup/namespaces and waits for the zygote child that
    // connects to its sockets. `/system/bin/sleep` is the sysroot's own toybox
    // applet, so nothing is downloaded or fabricated.
    // Z5 (research seam, `ACOS_ZYGOTE_NO_STUB=1`, OFF by default): the same
    // foreign-guest session with **no guest-side tombstone at all** — the
    // per-session liveness token is a bare host `sleep` that carries no bwrap,
    // no mount view and no `LD_PRELOAD` (Z4 §6.1: the 9.4 MiB/session stub is a
    // research artefact, a product would wait on an `Option<Child>` plus a
    // session-owned liveness source). Only meaningful together with
    // `ACOS_ZYGOTE_SESSION`, so the production path cannot reach it.
    let no_stub = session_dir.is_some()
        && zygote_app.is_none()
        && std::env::var_os("ACOS_ZYGOTE_NO_STUB").is_some();
    let cmd = match (&session_dir, &zygote_app) {
        (Some(_), None) if no_stub => {
            let mut c = Command::new("/bin/sleep");
            c.arg("1800");
            c
        }
        (Some(_), None) => sb.command("/system/bin/sleep", &["1800".to_string()])?,
        _ => sb.command(dalvik, &argv)?,
    };
    crate::stage("guest-exec");
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
                    libc::prctl(
                        libc::PR_SET_PDEATHSIG,
                        libc::SIGKILL as libc::c_ulong,
                        0,
                        0,
                        0,
                    );
                    let core = libc::rlimit {
                        rlim_cur: 0,
                        rlim_max: 0,
                    };
                    libc::setrlimit(libc::RLIMIT_CORE, &core);
                    let nofile = libc::rlimit {
                        rlim_cur: 4096,
                        rlim_max: 4096,
                    };
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
                c.stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped());
            } else {
                c.stdin(Stdio::null())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped());
            }
        },
    )?;
    let pid = child.id() as i32;
    crate::stage("guest-spawned");
    // Bounded capture (R1): a guest that loops on printf costs at most the cap.
    let cap = stream_cap_from_env();
    let (otx, orx) = mpsc::channel::<(u8, TailBuffer)>();
    let probe_state = Arc::new(std::sync::Mutex::new(ProbeState::default()));
    // W59: one trace sink for the whole interactive session. The guest's looper
    // lines and the host's tap markers share `started`, so the two clocks are
    // directly comparable (both are the same CLOCK_MONOTONIC on this host).
    if interactive {
        if let Some(path) = std::env::var_os("ACOS_ART_INPUT_TRACE") {
            let path = PathBuf::from(path);
            if let Some(sink) = TraceSink::open(&path, started) {
                probe_state.lock().unwrap_or_else(|e| e.into_inner()).trace = Some(sink);
            }
        }
    }
    // Z4: launcher 0's spawned process is the zygote; its own stdout (the
    // pre-fork log) is not the guest's channel any more, so drain it to a file
    // and parse probes from the session FIFO instead.
    if guest_fifo.is_some() && zygote_app.is_some() {
        if let Some(mut pipe) = child.stdout.take() {
            std::thread::spawn(move || {
                let _ = crate::capture::drain_to_tail(&mut pipe, 1 << 20);
            });
        }
    }
    let out_tee = std::env::var_os("ACOSLAUNCH_GUEST_STDOUT_FILE").map(PathBuf::from);
    let out_pipe: Box<dyn std::io::Read + Send> = match &guest_fifo {
        Some(fifo) => Box::new(
            std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(fifo)
                .map_err(Error::Io)?,
        ),
        None => Box::new(child.stdout.take().expect("piped stdout")),
    };
    let otx2 = otx.clone();
    let probe_for_out = Arc::clone(&probe_state);
    let compositor_for_probe = Arc::clone(&live.compositor);
    let out_thread = std::thread::spawn(move || {
        let buf = if interactive {
            match out_tee {
                Some(path) => drain_tee_lines(
                    out_pipe,
                    cap,
                    &path,
                    probe_for_out,
                    Some(compositor_for_probe),
                ),
                None => drain_stdout_lines(
                    out_pipe,
                    cap,
                    probe_for_out,
                    Some(compositor_for_probe),
                ),
            }
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
    note_stage("guest-exit");

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
    note_stage("teardown-streams");
    let batch = logd.finish_capped();
    note_stage("teardown-logd");
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
            note_stage("teardown-peer");
            s
        }
        Err(_) => {
            drop(peer_thread);
            PeerSummary::default()
        }
    };

    let marker_path = staging.join("application-created.marker");
    crate::stage("teardown");
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
        let Some(line) = line.strip_prefix("ACOSLAUNCH TRACE ") else {
            continue;
        };
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
    let (mut outcome, mut code) =
        if application_created && guest_ok && (!activity_wanted || activity_resumed) {
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
            (Outcome::Crash, Some("GUEST_EXIT_AFTER_RESUME".to_string()))
        } else if guest_exit.is_some()
            && application_created
            && activity_wanted
            && summary.txn_bytes.is_some()
        {
            // Application.onCreate ran; the activity step did not reach onResume.
            (Outcome::Crash, Some("NO_ACTIVITY_RESUMED".to_string()))
        } else if guest_exit.is_some() {
            // The guest ended before the requested milestone. `guest_exit` carries
            // the code; this is the distinct "guest exited early" outcome.
            (Outcome::Crash, Some("GUEST_EXITED".to_string()))
        } else if application_created {
            (Outcome::Crash, Some("GUEST_EXIT".to_string()))
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
    // W27: the guest writes the effective budget into its not-resumed marker.
    // Surface it only on the outcome that waited for it.
    let result_timeout = if code.as_deref() == Some("NO_ACTIVITY_RESUMED") {
        activity_failed_marker
            .as_deref()
            .and_then(|m| marker_u64(m, "timeout_ms"))
            .or(Some(resume_ms))
    } else {
        None
    };
    // S5: memcg OOM and the seccomp proof, same contract as the `run` route.
    if cgroup.oom_kills() > 0 {
        outcome = Outcome::Unsupported;
        code = Some(HOST_MEMORY_LIMIT.to_string());
    }
    let mut unsupported_notes: Vec<String> = Vec::new();
    if let Some(dir) = &shared_boot_image {
        unsupported_notes.push(format!(
            "shared-boot-image: {} (-Ximage:/data/img/boot.art -Xnorelocate; fixed base, no image ASLR)",
            dir.display()
        ));
    }
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
    let mut result = LaunchResult {
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
        activity_target: if activity_target.is_empty() {
            None
        } else {
            Some(activity_target.clone())
        },
        timeout_ms: result_timeout,
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
        heap: Some(crate::props::effective_heap_json()),
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
    // W43: the guest's own structured crash line (`ACOSLAUNCH {"outcome":"crash",…}`)
    // is the first error of a guest that died before/after its first frame. It
    // used to be lost with the caller's 4 KiB stderr tail.
    let guest_crash = probe_state
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .guest_launch
        .clone();
    if let Some(crash) = &guest_crash {
        result.notes.push(format!("guest: {crash}"));
    }
    for note in &result.notes {
        eprintln!("acos-art: {note}");
    }
    note_stage("teardown-evidence");
    write_session_exit(&out, &result, guest_crash.as_deref());
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
/// W27 item 3: the guest's guest-side `startActivity` guard marker.
const UNSUPPORTED_PREFIX: &str = "ACOSUNSUPPORTED ";
/// Largest live-probe line accepted (a whole window forest fits: two 2000-node
/// windows are ≈ 1-2 MiB; W27's 128 KiB cap silently froze the tree at the
/// first dialog window).
const LIVE_PROBE_LINE_CAP: usize = 8 * 1024 * 1024;
/// W36: the guest probe reports a swallowed exception as this marker.
const PROBE_ERROR_PREFIX: &str = "ACOSPROBE-ERROR ";
const CAPTURE_SOURCE: &str = "acos-c1-buffer";

#[derive(Clone)]
struct ProbeSample {
    seq: u64,
    tree: serde_json::Value,
    /// Host wall clock when the line was parsed.
    at_ms: u64,
    /// C1 store counter observed when the line was parsed. A probe is usable
    /// for a fast tree only when this is at least the generation that was
    /// current when the request started (P3b invalidation rule).
    generation: u64,
    /// P11 event mode: guest wall clock of the last draw and its ordinal. The
    /// host binds a settled sample to a draw at/after the gesture publish and
    /// requires the sample to stay on the same draw for one confirmation
    /// window, so a reply can never be bound to a pre-draw buffer.
    draw_ms: u64,
    draw_seq: u64,
    /// P11: the guest's framework-signal verdict for this sample.
    settled: bool,
    pending: Vec<String>,
}

/// P9: the launcher's event-driven wake channel for the startup loop.
///
/// Before this the startup loop polled `activity-resumed.marker`, the newest
/// probe sample and the C1 generation every 10 ms, so a launch reply could wait
/// up to one poll quantum after its last real prerequisite landed. The
/// guest-stdout drain thread now bumps `tick` and signals `signal` whenever it
/// parses a new probe sample, and the startup loop waits on that instead of
/// sleeping. The timeout stays as the fallback for the marker file, which the
/// guest writes as a file and has no host-side event.
#[derive(Default)]
struct ProbeWake {
    tick: std::sync::Mutex<u64>,
    signal: std::sync::Condvar,
}

impl ProbeWake {
    /// A new probe sample (or any guest line the startup loop cares about) has
    /// been parsed. Never blocks on the caller's own state lock.
    fn notify(&self) {
        let mut tick = self.tick.lock().unwrap_or_else(|e| e.into_inner());
        *tick = tick.wrapping_add(1);
        self.signal.notify_all();
    }

    /// Wait until the tick differs from `seen`, or `timeout` elapses.
    fn wait(&self, seen: &mut u64, timeout: std::time::Duration) {
        let tick = self.tick.lock().unwrap_or_else(|e| e.into_inner());
        if *tick != *seen {
            *seen = *tick;
            return;
        }
        let (tick, _) = self
            .signal
            .wait_timeout(tick, timeout)
            .unwrap_or_else(|e| e.into_inner());
        *seen = *tick;
    }
}

#[derive(Default)]
struct ProbeState {
    latest: Option<ProbeSample>,
    /// The newest `ACOSUNSUPPORTED {…}` marker (guest-asserted).
    unsupported: Option<serde_json::Value>,
    /// Monotonic marker count, so a stale marker is not attributed to a later
    /// gesture.
    unsupported_seq: u64,
    /// W36: the newest guest `ACOSPROBE-ERROR` (a probe throw is never swallowed)
    /// and its ordinal, plus lines dropped for exceeding the line cap.
    probe_error: Option<serde_json::Value>,
    probe_errors: u64,
    /// W43: the last `ACOSLAUNCH {…}` line the guest printed (its own structured
    /// outcome/crash), kept so a guest death is classified instead of opaque.
    guest_launch: Option<String>,
    oversized_lines: u64,
    /// Probe lines that were not valid JSON; counted, never silent (W36 found
    /// the two-root separator bug this way).
    bad_probe_lines: u64,
    /// W59: optional latency trace sink (`ACOS_ART_INPUT_TRACE=<path>`). The
    /// guest's own stdout (its `ACOSLAUNCH TRACE` looper lines) and the host's
    /// input publish/ack markers are written here with one monotonic clock, so
    /// a slow tap can be attributed to a guest stage instead of guessed. Off
    /// unless the variable names a file; no reply changes.
    trace: Option<TraceSink>,
    /// P9: event-driven wake for the startup loop (see [`ProbeWake`]).
    wake: Arc<ProbeWake>,
}

/// W59 latency trace: `elapsed_us tag text` per line, written under one lock
/// with the same `Instant` base as the session spawn.
struct TraceSink {
    file: std::fs::File,
    start: std::time::Instant,
    pid: u32,
}

impl TraceSink {
    fn open(path: &std::path::Path, start: std::time::Instant) -> Option<Self> {
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .ok()?;
        Some(TraceSink {
            file,
            start,
            pid: std::process::id(),
        })
    }

    fn line(&mut self, tag: &str, text: &str) {
        use std::io::Write;
        let _ = writeln!(
            self.file,
            "{:>9}us pid={} {} {}",
            self.start.elapsed().as_micros(),
            self.pid,
            tag,
            text
        );
    }
}

fn trace_host(state: &Arc<std::sync::Mutex<ProbeState>>, text: &str) {
    let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(trace) = state.trace.as_mut() {
        trace.line("HOST", text);
    }
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

/// P11: is the strict rule the event-driven one? Unset/`quiet` (the default)
/// keeps P10's 150 ms quiet rule byte-identical; `event` opts into the guest's
/// framework-signal verdict. The default is deliberately `quiet`: adoption is
/// a separate decision after P11's correctness evidence is reviewed.
fn strict_settle_event() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var("ACOS_STRICT_SETTLE")
            .map(|v| v.trim().eq_ignore_ascii_case("event"))
            .unwrap_or(false)
    })
}

// ---------------------------------------------------------------- W43: exit evidence
//
// Two harness failure classes were indistinguishable from app behaviour:
//
// * a full scratch disk surfaced as `INTERNAL: acos-art session exited` in the
//   caller (or as a caller-side Rust panic with no stderr at all), and
// * a session that ended mid-command left no record of *why* it ended.
//
// A full disk is a **host** condition: it must be a structured, retryable
// `HOST_DISK_FULL` before the guest starts (preflight) and at the capture write
// (mid-session). Every launch now also writes `session-exit.json` next to its
// frames: guest exit/signal, the last stage marker, the guest's own
// `ACOSLAUNCH {…}` crash line, the cgroup/guard notes and the free space.

/// Free bytes on the filesystem holding `path` (`statvfs`). `None` when the
/// answer cannot be read: a missing answer must never refuse a run by itself.
pub fn disk_free_bytes(path: &Path) -> Option<u64> {
    use std::os::unix::ffi::OsStrExt;
    let c = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(c.as_ptr(), &mut st) } != 0 {
        return None;
    }
    Some((st.f_bavail as u64).saturating_mul(st.f_frsize as u64))
}

/// Is this `io::Error` the scratch filesystem refusing a write (ENOSPC 28,
/// EDQUOT 122)? Those are host conditions, never guest behaviour.
pub fn is_disk_full(error: &std::io::Error) -> bool {
    matches!(error.raw_os_error(), Some(28) | Some(122))
}

/// Should a run be refused for lack of scratch space? A free-space answer that
/// could not be read (`None`) never refuses a run by itself.
#[must_use]
pub fn disk_full_for_run(free: Option<u64>, min_bytes: u64) -> bool {
    matches!(free, Some(f) if f < min_bytes)
}

/// The last stage marker this process passed, so a panic or an abrupt exit names
/// the stage instead of only "session exited".
static LAST_STAGE: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

fn note_stage(name: &str) {
    crate::stage(name);
    if let Ok(mut slot) = LAST_STAGE.lock() {
        *slot = Some(name.to_string());
    }
}

fn last_stage() -> Option<String> {
    LAST_STAGE.lock().ok().and_then(|g| g.clone())
}

/// One self-describing record per launch, written next to the frames. Never
/// fatal: the record is evidence, not a gate.
fn write_session_exit(out: &Path, result: &LaunchResult, guest_crash: Option<&str>) {
    let disk_full = result.code.as_deref() == Some("HOST_DISK_FULL");
    let doc = serde_json::json!({
        "utc_ms": host_ms(),
        "route": "launch",
        "outcome": result.outcome,
        "code": result.code,
        "retryable": disk_full,
        "guest_exit": result.guest_exit,
        "guest_signal": result.guest_signal,
        "wall_ms": result.wall_ms,
        "application_created": result.application_created,
        "activity_resumed": result.activity_launched,
        "stage": last_stage(),
        "guest_crash": guest_crash,
        "disk_free_bytes": disk_free_bytes(out),
        // W48: which heap configuration this session ran with (the
        // `ACOS_ART_HEAP*` seam, resolved and validated before the guest).
        "heap": crate::props::effective_heap_json(),
        "cgroup": result.notes.iter().find(|n| n.starts_with("cgroup:")),
        "logd": result.notes.iter().find(|n| n.starts_with("logd:")),
        "stdout_tail_last": result.stdout.lines().rev().find(|l| !l.trim().is_empty()),
        "stderr_tail_last": result.stderr.lines().rev().find(|l| !l.trim().is_empty()),
    });
    let _ = fs::create_dir_all(out);
    let _ = fs::write(
        out.join("session-exit.json"),
        serde_json::to_vec_pretty(&doc).unwrap_or_default(),
    );
}

/// A panic inside the launcher is a harness bug and must leave a record with the
/// stage it died in (a caller that only sees EOF otherwise reports `INTERNAL`).
fn install_session_panic_hook(out: &Path) {
    let out = out.to_path_buf();
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let doc = serde_json::json!({
            "utc_ms": host_ms(),
            "route": "launch",
            "outcome": "crash",
            "code": "HOST_PANIC",
            "retryable": false,
            "stage": last_stage(),
            "panic": info.to_string(),
            "disk_free_bytes": disk_free_bytes(&out),
        });
        let _ = fs::create_dir_all(&out);
        let _ = fs::write(
            out.join("session-exit.json"),
            serde_json::to_vec_pretty(&doc).unwrap_or_default(),
        );
        // A caller blocked on a command reply must get a structured line, not
        // an EOF it can only report as `INTERNAL: session exited`.
        if std::env::var_os("ACOSLAUNCH_INTERACTIVE").is_some() {
            protocol_write(&serde_json::json!({
                "ok": false, "cmd": "session", "code": "HOST_PANIC", "retryable": false,
                "error": format!("the launcher panicked (stage {:?}): {}", last_stage(), info),
            }));
        }
        previous(info);
    }));
}

/// W43: AF_UNIX `sun_path` is 108 bytes and every socket used to live under the
/// caller's `--data` dir, which `run-exclusive.sh` derives from the task name
/// (`data-<task>-<pid>`). A task name of 17+ characters pushed the peer socket
/// past `SUN_LEN` and `bind()` failed with `io: socket path too long` **before
/// the guest started** — the caller could only report
/// `INTERNAL: acos-art session exited` (W41 evidence
/// `results/art-runtime/evidence/W41-socket-path-too-long.txt`).
///
/// The sockets are host-side only: the sandbox binds them to fixed guest paths
/// (`/dev/socket`, `/data/acos-launch.sock`), so their host location is free.
/// They are bound in a short runtime dir instead, independent of the task name
/// and of the `--data` path length:
/// `/run/user/<uid>/acos-art-<pid>-<seq>/`, falling back to
/// `<repo>/.cache/art-sock/<pid>-<seq>/`. The directory is removed on drop.
struct ShortSocketDir(PathBuf);

impl ShortSocketDir {
    fn new() -> std::io::Result<Self> {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let id = format!("{}-{}", std::process::id(), SEQ.fetch_add(1, Ordering::Relaxed));
        let uid = unsafe { libc::getuid() };
        let runtime = PathBuf::from(format!("/run/user/{uid}"));
        let mut candidates = Vec::new();
        if runtime.is_dir() {
            candidates.push(runtime.join(format!("acos-art-{id}")));
        }
        if let Ok(root) = crate::repo_root() {
            candidates.push(root.join(".cache/art-sock").join(&id));
        }
        candidates.push(PathBuf::from(format!("/dev/shm/acos-art-{id}")));
        let mut last = None;
        for dir in candidates {
            match fs::create_dir_all(&dir) {
                Ok(()) => return Ok(Self(dir)),
                Err(e) => last = Some(e),
            }
        }
        Err(last.unwrap_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::NotFound, "no short socket dir available")
        }))
    }

    /// Z4 (research seam): use an explicit directory, and leave it in place on
    /// drop (the session directory is the shared plan's, not this process's).
    fn at(dir: &Path) -> std::io::Result<Self> {
        fs::create_dir_all(dir)?;
        Ok(Self(dir.to_path_buf()))
    }
}

impl Drop for ShortSocketDir {
    fn drop(&mut self) {
        // Z4: a foreign-guest session directory is owned by the driver.
        if std::env::var_os("ACOS_ZYGOTE_SESSION").is_some() {
            return;
        }
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Z4 (research seam, off by default): publish this session's contract so the
/// shared zygote can fork a child that binds exactly this session's writable
/// root, sockets and importer socket. Written last, atomically: a zygote that
/// sees `argv` knows every other file it needs is already there.
#[allow(clippy::too_many_arguments)]
fn write_zygote_session(
    dir: &Path,
    run: &crate::RunDir,
    sockets: &Path,
    peer_sock: &Path,
    staging: &Path,
    out: &Path,
    package: &str,
    guest_args: &[String],
) -> Result<()> {
    fs::create_dir_all(dir)?;
    let doc = serde_json::json!({
        "package": package,
        "pid": std::process::id(),
        "run_root": run.path().display().to_string(),
        "sockets": sockets.display().to_string(),
        "peer_sock": peer_sock.display().to_string(),
        "staging": staging.display().to_string(),
        "out": out.display().to_string(),
        "guest_args": guest_args,
    });
    let tmp = dir.join("session.json.tmp");
    fs::write(&tmp, serde_json::to_vec_pretty(&doc).map_err(|e| Error::Layout(e.to_string()))?)?;
    fs::rename(&tmp, dir.join("session.json"))?;
    let mut argv = String::new();
    for a in guest_args {
        argv.push_str(a);
        argv.push('\n');
    }
    let tmp = dir.join("argv.tmp");
    fs::write(&tmp, argv)?;
    fs::rename(&tmp, dir.join("argv"))?;
    Ok(())
}

/// The structured refusal for a scratch disk that cannot hold this run's frames.
fn disk_full_result(opts: &LaunchOpts, package: &str, free: u64, min: u64) -> LaunchResult {
    LaunchResult {
        outcome: Outcome::Unsupported,
        code: Some("HOST_DISK_FULL".to_string()),
        apk: opts.apk.display().to_string(),
        package: package.to_string(),
        until: opts.until.clone(),
        application_created: false,
        activity_launched: false,
        guest_exit: None,
        guest_signal: None,
        wall_ms: 0,
        activity: None,
        activity_target: None,
        timeout_ms: None,
        application_marker: None,
        activity_marker: None,
        looper_trace: Vec::new(),
        bind_application: BindDelivery {
            request_bytes: None,
            delivered: false,
            app_thread_wire: None,
            code: acos_binder_peer::peer::BIND_APPLICATION_CODE,
        },
        service_calls: Vec::new(),
        unsupported: Vec::new(),
        stdout: String::new(),
        stderr: String::new(),
        logs: Vec::new(),
        heap: Some(crate::props::effective_heap_json()),
        notes: vec![format!(
            "HOST_DISK_FULL: {free} bytes free on the scratch filesystem, below \
             ACOS_ART_MIN_DISK_MB={} MiB; refusing to start a guest whose frames cannot be \
             written (retryable=true)",
            min / 1024 / 1024
        )],
        marker: None,
    }
}

fn latest_probe(state: &Arc<std::sync::Mutex<ProbeState>>) -> Option<ProbeSample> {
    state
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .latest
        .clone()
}

/// The newest guest `ACOSUNSUPPORTED` marker and its ordinal.
fn latest_unsupported(
    state: &Arc<std::sync::Mutex<ProbeState>>,
) -> (u64, Option<serde_json::Value>) {
    let state = state.lock().unwrap_or_else(|e| e.into_inner());
    (state.unsupported_seq, state.unsupported.clone())
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
/// probe lines as they arrive.
///
/// W36: a probe line is one JSON object and a full window forest can be large
/// (two 2000-node windows ≈ 1 MiB). W27-W32 used a 128 KiB line cap and silently
/// stopped parsing every line at/above it, so the tree froze at the last small
/// sample exactly when a dialog window appeared (W32 misread this as the guest
/// probe throwing). Lines are now read whole up to [`LIVE_PROBE_LINE_CAP`], and
/// an oversized line is counted and named on stderr instead of vanishing.
fn drain_stdout_lines<R: std::io::Read>(
    reader: R,
    cap: usize,
    state: Arc<std::sync::Mutex<ProbeState>>,
    compositor: Option<Arc<std::sync::Mutex<acos_services::surface_client::SurfaceState>>>,
) -> TailBuffer {
    drain_tee_lines(reader, cap, &PathBuf::new(), state, compositor)
}

/// Read one `\n`-terminated line, bounding the bytes held in memory.
///
/// `None` = EOF before any byte; `Some(Some(line))` = a complete line (the
/// newline is kept); `Some(None)` = the line exceeded `cap` and was drained.
fn read_capped_line<R: std::io::BufRead>(
    reader: &mut R,
    cap: usize,
) -> Option<Option<Vec<u8>>> {
    use std::io::BufRead;
    let mut line = Vec::new();
    loop {
        let mut part = Vec::new();
        let remaining = (cap + 1).saturating_sub(line.len());
        let n = std::io::Read::take(&mut *reader, remaining as u64)
            .read_until(b'\n', &mut part)
            .ok()?;
        if n == 0 {
            return if line.is_empty() { None } else { Some(None) };
        }
        let ended = part.ends_with(b"\n");
        if !ended && line.len() + part.len() > cap {
            // Drain the rest of this line without keeping it.
            loop {
                let mut sink = Vec::new();
                match std::io::Read::take(&mut *reader, 64 * 1024)
                    .read_until(b'\n', &mut sink)
                {
                    Ok(0) => break,
                    Ok(_) if sink.ends_with(b"\n") => break,
                    Ok(_) => {}
                    Err(_) => break,
                }
            }
            return Some(None);
        }
        line.extend_from_slice(&part);
        if ended {
            return Some(Some(line));
        }
    }
}

/// Same parser as [`drain_stdout_lines`], plus an optional diagnostic tee of the
/// guest's stdout (`ACOSLAUNCH_GUEST_STDOUT_FILE`), so probe errors and the
/// root-window marker survive the launcher's bounded tail.
fn drain_tee_lines<R: std::io::Read>(
    reader: R,
    cap: usize,
    tee: &Path,
    state: Arc<std::sync::Mutex<ProbeState>>,
    compositor: Option<Arc<std::sync::Mutex<acos_services::surface_client::SurfaceState>>>,
) -> TailBuffer {
    use std::io::Write;
    let mut tee_file = if tee.as_os_str().is_empty() {
        None
    } else {
        fs::File::create(tee).ok()
    };
    let mut tee_written = 0usize;
    let mut tail = TailBuffer::with_cap(cap);
    let mut reader = std::io::BufReader::new(reader);
    while let Some(line) = read_capped_line(&mut reader, LIVE_PROBE_LINE_CAP) {
        let Some(line) = line else {
            let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
            state.oversized_lines += 1;
            if state.oversized_lines == 1 {
                eprintln!(
                    "acos-art: live probe line exceeds {} bytes; sample dropped (never silently)",
                    LIVE_PROBE_LINE_CAP
                );
            }
            continue;
        };
        tail.push(&line);
        if let Some(f) = tee_file.as_mut() {
            if tee_written < 32 * 1024 * 1024 {
                let take = line.len().min(32 * 1024 * 1024 - tee_written);
                let _ = f.write_all(&line[..take]);
                tee_written += take;
            }
        }
        let Ok(text) = std::str::from_utf8(&line) else {
            continue;
        };
        let trimmed = text.trim_end();
        // W59: keep the guest's own timing evidence (its `ACOSLAUNCH TRACE`
        // looper lines and stall dumps) next to the host's input markers. Only
        // small, human-readable lines go in; probe JSON is already in the tree.
        if trimmed.starts_with("ACOSLAUNCH ") || trimmed.starts_with("ACOSPROBE-STALL") {
            let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(trace) = state.trace.as_mut() {
                trace.line("GUEST", trimmed);
            }
        }
        if let Some(json) = trimmed.strip_prefix(LIVE_PROBE_PREFIX) {
            let parsed = serde_json::from_str::<serde_json::Value>(json);
            if let Err(e) = &parsed {
                let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
                state.bad_probe_lines += 1;
                if state.bad_probe_lines == 1 {
                    eprintln!(
                        "acos-art: live probe line is not valid JSON ({e}); sample dropped, first 160 bytes: {:.160}",
                        &json
                    );
                }
            }
            if let Ok(value) = parsed {
                let seq = value.get("seq").and_then(|v| v.as_u64());
                let tree = value.get("tree");
                if let (Some(seq), Some(tree)) = (seq, tree) {
                    // The root list (`windows`) belongs to the sample; carry it
                    // inside the tree so evidence and step replies can show the
                    // z-order without a second protocol field.
                    let mut tree = tree.clone();
                    if let Some(windows) = value.get("windows") {
                        if let Some(object) = tree.as_object_mut() {
                            object.insert("windows".into(), windows.clone());
                        }
                    }
                    if p3_enabled() {
                        // Guest traversal + tree JSON build (`emit_us`), guest
                        // stamp at JSON build (`time`), and host receipt after
                        // the pipe write and the host's own JSON parse.
                        let emit_us = value.get("emit_us").and_then(|v| v.as_u64()).unwrap_or(0);
                        let guest_ms = value.get("time").and_then(|v| v.as_u64()).unwrap_or(0);
                        let now_us = p3_now_us();
                        let lag_us = now_us.saturating_sub(guest_ms as u128 * 1000);
                        p3_mark(
                            "art",
                            "art.probe_parse",
                            &format!(
                                "seq={seq} emit_us={emit_us} guest_ms={guest_ms} \
                                 probe_lag_us={lag_us} line_bytes={}",
                                json.len()
                            ),
                        );
                    }
                    let generation = compositor
                        .as_ref()
                        .map(|compositor| {
                            compositor
                                .lock()
                                .unwrap_or_else(|e| e.into_inner())
                                .latest_generation()
                        })
                        .unwrap_or(0);
                    // P11: event-mode fields are absent in quiet mode, so a
                    // quiet sample parses exactly as before (settled=false,
                    // draw_ms=0 are simply unused there).
                    let event = value.get("settle");
                    let draw_ms = event
                        .and_then(|v| v.get("draw_ms"))
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0);
                    let draw_seq = event
                        .and_then(|v| v.get("draw_seq"))
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0);
                    let settled = event
                        .and_then(|v| v.get("settled"))
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
                    let pending = event
                        .and_then(|v| v.get("pending"))
                        .and_then(|v| v.as_array())
                        .map(|values| {
                            values
                                .iter()
                                .filter_map(|v| v.as_str().map(str::to_string))
                                .collect::<Vec<_>>()
                        })
                        .unwrap_or_default();
                    let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
                    let newer = state.latest.as_ref().map(|p| p.seq < seq).unwrap_or(true);
                    if newer {
                        state.latest = Some(ProbeSample {
                            seq,
                            tree,
                            at_ms: host_ms(),
                            generation,
                            draw_ms,
                            draw_seq,
                            settled,
                            pending,
                        });
                    }
                    // P9: wake the startup loop (and anyone else waiting on a
                    // fresh sample) instead of making it poll for this.
                    let wake = Arc::clone(&state.wake);
                    drop(state);
                    wake.notify();
                }
            }
        } else if let Some(json) = trimmed.strip_prefix(PROBE_ERROR_PREFIX) {
            if let Ok(value) = serde_json::from_str::<serde_json::Value>(json) {
                let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
                state.probe_errors += 1;
                if state.probe_errors == 1 {
                    eprintln!("acos-art: guest probe error {value}");
                }
                state.probe_error = Some(value);
            }
        } else if trimmed.starts_with("ACOSLAUNCH {") {
            let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
            state.guest_launch = Some(trimmed.chars().take(600).collect());
        } else if let Some(json) = trimmed.strip_prefix(UNSUPPORTED_PREFIX) {
            if let Ok(value) = serde_json::from_str::<serde_json::Value>(json) {
                let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
                state.unsupported = Some(value);
                state.unsupported_seq += 1;
            }
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
    let t_capture = m5_now_us();
    let frame = {
        let state = live.compositor.lock().unwrap_or_else(|e| e.into_inner());
        state.capture_latest()
    };
    let Some(frame) = frame else {
        return Ok(None);
    };
    let t_encode = m5_now_us();
    m5_trace("art.capture_composite", t_capture, t_encode, "");
    fs::create_dir_all(dir)?;
    *seq += 1;
    let path = dir.join(format!(
        "acos-c1-layer{}-frame-{}-g{}-c{}.png",
        frame.layer_id, frame.frame_number, frame.generation, *seq
    ));
    // P2: the PNG is written once and hashed once, in the same pass, on the
    // caller's thread while a second thread encodes it (the encode is ~7 ms and
    // the write+MD5 over 9.5 MB is ~11 ms, so they overlap). `content_hash` is
    // still the MD5 of exactly the bytes in this file.
    let (bytes, md5, encode_us, write_us, hash_us) = encode_write_and_hash(
        frame.width,
        frame.height,
        frame.stride,
        &frame.rgba,
        &path,
    )?;
    let t_end = m5_now_us();
    m5_trace(
        "art.png_encode",
        t_encode,
        t_encode + encode_us as u128,
        &format!("png_bytes={bytes} pipelined=1"),
    );
    m5_trace("art.png_write", t_encode, t_encode + write_us as u128, "");
    m5_trace(
        "art.md5_file",
        t_encode,
        t_encode + hash_us as u128,
        "in_process=1",
    );
    m5_trace("art.capture_total", t_capture, t_end, "");
    if p3_enabled() {
        p3_trace(
            "art",
            "art.md5_file",
            t_encode,
            t_encode + hash_us as u128,
            "in_process=1",
        );
        p3_trace("art", "art.capture_total", t_capture, t_end, "");
    }
    // ZM2 quick-win knob (off by default): trim after a capture. W1's default
    // mmap threshold already returns the frame buffers on free, so this is now
    // only a measurement knob, not part of the production path.
    if std::env::var_os("ACOS_ART_TRIM_AFTER_CAPTURE").is_some() {
        #[cfg(target_env = "gnu")]
        unsafe {
            libc::malloc_trim(0);
        }
    }
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
        bytes,
    }))
}

/// Encode one frame to `path` and return `(bytes, md5_hex, encode_us,
/// write_us, hash_us)`.
///
/// The encoder runs on a second thread and hands over 1 MiB chunks; this thread
/// writes each chunk to the file and folds it into the MD5, so every byte is
/// traversed once by the hasher and once by the write, in cache-hot chunks, and
/// the encode's own CPU overlaps both. The file is created (truncated) before
/// encoding starts, so a failure leaves an incomplete artifact that the daemon's
/// size+hash check refuses rather than a stale frame.
fn encode_write_and_hash(
    width: u32,
    height: u32,
    stride: u32,
    rgba: &[u8],
    path: &Path,
) -> std::io::Result<(usize, String, u64, u64, u64)> {
    use std::io::Write as _;
    use std::sync::mpsc;
    const CHUNK: usize = 1 << 20;
    let mut file = fs::File::create(path)?;
    let mut hasher = acos_render::md5::Md5::new();
    let mut total = 0usize;
    let mut write_us = 0u64;
    let mut hash_us = 0u64;
    let mut failure: Option<std::io::Error> = None;
    // A bounded channel gives backpressure: at most a few 1 MiB chunks are in
    // flight, so peak RSS does not grow with the frame size.
    let (tx, rx) = mpsc::sync_channel::<Vec<u8>>(4);
    let t_encode = m5_now_us();
    std::thread::scope(|scope| {
        // The sender is *moved* into the encoder thread: the drain loop below
        // ends when that thread returns and drops it.
        let encoder = scope.spawn(move || {
            let mut chunk: Vec<u8> = Vec::with_capacity(CHUNK);
            encode_png_rgba_stream(width, height, stride, rgba, |bytes: &[u8]| {
                let mut rest = bytes;
                while !rest.is_empty() {
                    let room = CHUNK - chunk.len();
                    let take = room.min(rest.len());
                    chunk.extend_from_slice(&rest[..take]);
                    rest = &rest[take..];
                    if chunk.len() == CHUNK {
                        let full = std::mem::replace(&mut chunk, Vec::with_capacity(CHUNK));
                        if tx.send(full).is_err() {
                            return;
                        }
                    }
                }
            });
            if !chunk.is_empty() {
                let _ = tx.send(chunk);
            }
        });
        for chunk in rx {
            if failure.is_none() {
                let t0 = m5_now_us();
                if let Err(error) = file.write_all(&chunk) {
                    failure = Some(error);
                }
                write_us += (m5_now_us() - t0) as u64;
                let t1 = m5_now_us();
                hasher.update(&chunk);
                hash_us += (m5_now_us() - t1) as u64;
                total += chunk.len();
            }
        }
        encoder.join().expect("the PNG encoder thread panicked");
    });
    let encode_us = (m5_now_us() - t_encode) as u64;
    if let Some(error) = failure {
        return Err(error);
    }
    file.flush()?;
    drop(file);
    Ok((total, hasher.finish_hex(), encode_us, write_us, hash_us))
}

fn latest_generation(live: &acos_services::registry::LiveHandles) -> u64 {
    live.compositor
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .latest_generation()
}

/// D-M5: microseconds since the Unix epoch (realtime), so a phase measured in
/// `acos-art` lines up with the serve-side and client-side clocks.
fn m5_now_us() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_micros())
        .unwrap_or(0)
}

/// D-M5: env-gated per-phase trace for one screenshot capture.
///
/// `DM5_TRACE=<path>` appends one line per phase to that file (append-only,
/// opened once); unset means no work at all. Trace only: no behaviour change.
fn m5_trace(event: &str, t0: u128, t1: u128, extra: &str) {
    use std::io::Write;
    static SINK: std::sync::OnceLock<Option<std::sync::Mutex<std::fs::File>>> =
        std::sync::OnceLock::new();
    let sink = SINK.get_or_init(|| {
        // P2_TRACE (this task) or DM5_TRACE (the D-M5 evidence harness).
        std::env::var_os("P2_TRACE")
            .or_else(|| std::env::var_os("DM5_TRACE"))
            .and_then(|path| {
                std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(path)
                    .ok()
            })
            .map(std::sync::Mutex::new)
    });
    if let Some(file) = sink {
        if let Ok(mut file) = file.lock() {
            let _ = writeln!(
                file,
                "DM5TRACE proc=art ev={event} t_us={t1} dur_us={} {extra}",
                t1.saturating_sub(t0)
            );
            let _ = file.flush();
        }
    }
}

/// P3: microseconds since the Unix epoch (realtime), same clock as `m5_now_us`
/// and the serve-side trace so one request's phases line up across processes.
fn p3_now_us() -> u128 {
    m5_now_us()
}

/// P3: is the tree-diagnosis trace on? Cached once; unset means every `p3_*`
/// call is a no-op and no extra work is done.
fn p3_enabled() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var_os("P3_TRACE").is_some())
}

/// P3: env-gated per-phase trace for one fresh tree request.
///
/// `P3_TRACE=<path>` appends one line per phase (append-only, opened once);
/// unset means no work at all. Trace only: no behaviour change.
fn p3_trace(proc_name: &str, event: &str, t0: u128, t1: u128, extra: &str) {
    use std::io::Write;
    static SINK: std::sync::OnceLock<Option<std::sync::Mutex<std::fs::File>>> =
        std::sync::OnceLock::new();
    let sink = SINK.get_or_init(|| {
        std::env::var_os("P3_TRACE").and_then(|path| {
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .ok()
        })
        .map(std::sync::Mutex::new)
    });
    if let Some(file) = sink {
        if let Ok(mut file) = file.lock() {
            // One write per line: several host processes append to the same
            // file, so a fragmented `write!` would interleave two lines.
            let line = format!(
                "P3TRACE proc={proc_name} ev={event} t_us={t1} dur_us={} {extra}\n",
                t1.saturating_sub(t0)
            );
            let _ = file.write_all(line.as_bytes());
            let _ = file.flush();
        }
    }
}

/// P3: a single timestamped marker (duration 0).
fn p3_mark(proc_name: &str, event: &str, extra: &str) {
    let t = p3_now_us();
    p3_trace(proc_name, event, t, t, extra);
}

fn protocol_write(value: &serde_json::Value) {
    use std::io::Write;
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{value}");
    let _ = out.flush();
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
        // W48: the heap configuration, recorded with the session's own evidence.
        "heap": crate::props::effective_heap_json(),
        "steps": evidence,
    });
    let _ = fs::create_dir_all(out);
    let _ = fs::write(
        out.join("live-session.json"),
        serde_json::to_vec_pretty(&doc).unwrap_or_default(),
    );
}

/// P1: how an input observation is settled.
///
/// `strict` is the unchanged quiet-window rule and stays the default for
/// assertions and pixel checks. `fast` is the navigation mode approved by the
/// owner: bind the first probe tree that differs from the pre-action baseline,
/// then the first C1 store after that probe sample. A fast frame may therefore
/// be mid-animation; its reply says so (`mode`, `bound`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SettleMode {
    Fast,
    Strict,
}

impl SettleMode {
    fn as_str(self) -> &'static str {
        match self {
            Self::Fast => "fast",
            Self::Strict => "strict",
        }
    }

    fn from_request(request: &serde_json::Value) -> std::result::Result<Self, String> {
        match request.get("settle").and_then(|v| v.as_str()) {
            None | Some("strict") => Ok(Self::Strict),
            Some("fast") => Ok(Self::Fast),
            Some(other) => Err(format!(
                "settle must be \"fast\" or \"strict\" (got {other:?})"
            )),
        }
    }
}

/// Measured settle phases, all relative to the publish of the gesture.
///
/// `state_changed` is `Some(true)` only when fast mode bound a probe sample that
/// differs from the pre-action baseline; it is `Some(false)` for a fast
/// fallback (the strict quiet rule ran but no change was proven) and `None` in
/// strict mode, where the engine's host-side pixel+tree comparison decides.
struct SettleTiming {
    mode: SettleMode,
    first_change_ms: Option<u64>,
    frame_ms: Option<u64>,
    state_changed: Option<bool>,
    bound: &'static str,
    /// P11: `Some("event")` only for the opt-in event rule. Quiet/fast leave
    /// this `None`, so their `settle` JSON is byte-identical to P10/P1.
    rule: Option<&'static str>,
    /// P11: why the event rule timed out (e.g. `anim`, `queue`, `traversal`).
    reason: Option<String>,
}

impl SettleTiming {
    fn strict() -> Self {
        Self {
            mode: SettleMode::Strict,
            first_change_ms: None,
            frame_ms: None,
            state_changed: None,
            bound: "strict-quiet",
            rule: None,
            reason: None,
        }
    }

    fn to_json(&self, settled: bool, ack_ms: u64) -> serde_json::Value {
        let mut value = serde_json::json!({
            "mode": self.mode.as_str(),
            "settled": settled,
            "ack_ms": ack_ms,
            "first_change_ms": self.first_change_ms,
            "frame_ms": self.frame_ms,
            "state_changed": self.state_changed,
            "bound": self.bound,
            "meaning": "mode=fast frames may be mid-animation; ack_ms is publish -> the gesture's own FINISHED, first_change_ms is publish -> the probe sample that differs from the pre-action tree, frame_ms is publish -> the bound C1 store",
        });
        if let Some(rule) = self.rule {
            if let Some(map) = value.as_object_mut() {
                map.insert("strict_rule".into(), serde_json::json!(rule));
                if let Some(reason) = self.reason.as_deref() {
                    map.insert("reason".into(), serde_json::json!(reason));
                    map.insert("animating".into(), serde_json::json!(
                        reason.contains("anim") || reason.contains("frame")
                    ));
                }
            }
        }
        value
    }
}

/// The newest probe sample plus the C1 generation it is bound to.
struct Settled {
    probe: ProbeSample,
    generation: u64,
    timing: SettleTiming,
}

enum SettleEnd {
    Settled(Settled),
    Timeout {
        generation: u64,
        probe_seq: u64,
        timing: SettleTiming,
    },
    GuestExited(std::process::ExitStatus),
    /// The guest's `startActivity` guard fired during this gesture (W27).
    Unsupported(serde_json::Value),
}

/// Wait until the newest C1 frame and the newest in-guest tree sample describe
/// the same, settled UI state.
///
/// A frame can land *before* the app's click has run (touch feedback), and the
/// tree probe can see the post-click state before the post-click frame is
/// stored. Binding those two blindly produced a reply whose tree said `0` and
/// whose pixels said `1` (W10 negative evidence).
///
/// **`strict`** (default, unchanged): the C1 generation advanced past
/// `min_generation`; the probe produced a sample **after** the last store; no
/// new store for `quiet` (150 ms); optionally the pixels differ from the
/// caller's previous capture. This is the settled-frame rule assertions and
/// pixel checks use.
///
/// **`fast`** (P1 navigation mode, approved by the owner): the baseline tree is
/// snapshotted *before* the input is published. The first probe sample whose
/// tree differs from it is the commit sample; the first C1 store after that
/// sample is the bound frame. A ripple that keeps storing frames afterwards no
/// longer gates the reply (W59). If no tree change is seen within a bounded
/// window (`ACOSLAUNCH_FAST_WINDOW_MS`, default 100 ms = two probe periods), the strict quiet rule
/// takes over and the reply reports `state_changed:false`: a fast observation
/// never claims a change it did not see.
///
/// **P10**: the guest probe is event-driven (view dirty -> one sample after the
/// frame), so an *idle* screen no longer produces a sample every 50 ms. The
/// strict rule is unchanged, but a sample must still exist after the last C1
/// store; the host asks the guest for one (the same `PROBE` wake the fast path
/// uses) at settle entry and, if the newest sample predates the last store,
/// after the quiet window has elapsed. That keeps the invariant "a probe sample
/// after the last store" without waiting for the slow fallback poll.
/// P11: the opt-in event-driven strict rule.
///
/// The guest reports, on the UI thread, whether the framework still has work:
/// pending traversals/layout/dirty draws, Choreographer frame callbacks,
/// active AnimationHandler animators / LayoutTransition / ViewPropertyAnimator,
/// a main-Looper message due inside the horizon, and unstable window/focus/IME
/// state. A sample is a candidate when all of that is clear, it was taken after
/// a draw at/after the gesture publish, and it saw the latest C1 store. The host
/// then requires the *same draw* to remain the newest draw for one confirmation
/// window (`ACOS_STRICT_SETTLE_CONFIRM_MS`, default 24 ms), so the C1 store can
/// commit that draw before a PNG/tree is bound. A newer draw, a newer store, or
/// any newly pending signal restarts the candidate. Anything still pending at
/// the deadline returns `bound:"timeout"` with the guest's reason; it is never
/// reported as settled.
#[allow(clippy::too_many_arguments)]
fn wait_for_event_settled(
    child: &mut std::process::Child,
    cgroup: &mut crate::cgroup::RunCgroup,
    live: &acos_services::registry::LiveHandles,
    probe_state: &Arc<std::sync::Mutex<ProbeState>>,
    guest_stdin: &mut Option<std::process::ChildStdin>,
    deadline: Instant,
    min_generation: u64,
    min_probe_seq: u64,
    previous_fnv: Option<u64>,
    publish_epoch_ms: u64,
) -> SettleEnd {
    let confirm = Duration::from_millis(env_u64("ACOS_STRICT_SETTLE_CONFIRM_MS", 24));
    // A tree request has no publish timestamp; it must not serve a sample that
    // was already current before the request, so the floor is the entry sample.
    let entry_floor = if publish_epoch_ms == 0 {
        latest_probe(probe_state)
            .map(|p| p.seq)
            .unwrap_or(min_probe_seq)
    } else {
        min_probe_seq
    };
    let (unsupported_baseline, _) = latest_unsupported(probe_state);
    let mut last_generation = min_generation;
    let mut last_wake = Instant::now();
    let mut candidate: Option<ProbeSample> = None;
    let mut candidate_since = Instant::now();
    let mut first_change_ms: Option<u64> = None;
    let mut last_reason = String::from("no-probe");
    request_guest_probe(guest_stdin);
    loop {
        let (unsupported_seq, marker) = latest_unsupported(probe_state);
        if unsupported_seq > unsupported_baseline {
            return SettleEnd::Unsupported(marker.unwrap_or(serde_json::Value::Null));
        }
        match child.try_wait() {
            Ok(Some(status)) => return SettleEnd::GuestExited(status),
            _ => {}
        }
        let generation = latest_generation(live);
        if generation > last_generation {
            trace_host(
                probe_state,
                &format!("settle event generation {last_generation}->{generation}"),
            );
            last_generation = generation;
        }
        let probe = latest_probe(probe_state);
        let mut valid: Option<ProbeSample> = None;
        match &probe {
            None => last_reason = "no-probe".to_string(),
            Some(sample) => {
                if sample.seq <= entry_floor {
                    last_reason = "probe-floor".to_string();
                } else if !sample.settled {
                    last_reason = sample
                        .pending
                        .first()
                        .cloned()
                        .unwrap_or_else(|| "unsettled".to_string());
                } else if publish_epoch_ms != 0 && sample.draw_ms < publish_epoch_ms {
                    last_reason = "no-draw-after-action".to_string();
                } else if sample.generation < generation {
                    last_reason = "newer-store".to_string();
                } else {
                    valid = Some(sample.clone());
                }
            }
        }
        if let Some(sample) = valid {
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
            if !pixels_changed {
                last_reason = "pixels-unchanged".to_string();
                candidate = None;
            } else {
                let restart = match &candidate {
                    None => true,
                    Some(current) => sample.draw_seq != current.draw_seq || sample.seq < current.seq,
                };
                if restart {
                    candidate = Some(sample.clone());
                    candidate_since = Instant::now();
                    if first_change_ms.is_none() && publish_epoch_ms != 0 {
                        first_change_ms =
                            Some(host_ms().saturating_sub(publish_epoch_ms));
                    }
                }
                if candidate
                    .as_ref()
                    .map(|current| sample.draw_seq == current.draw_seq)
                    .unwrap_or(false)
                    && candidate_since.elapsed() >= confirm
                    && sample.generation == generation
                {
                    let timing = SettleTiming {
                        mode: SettleMode::Strict,
                        first_change_ms,
                        frame_ms: if publish_epoch_ms == 0 {
                            None
                        } else {
                            Some(host_ms().saturating_sub(publish_epoch_ms))
                        },
                        state_changed: None,
                        bound: "strict-event",
                        rule: Some("event"),
                        reason: None,
                    };
                    trace_host(
                        probe_state,
                        &format!(
                            "settle event ready gen={generation} probe_seq={} draw_seq={} draw_ms={} confirm_ms={}",
                            sample.seq,
                            sample.draw_seq,
                            sample.draw_ms,
                            confirm.as_millis()
                        ),
                    );
                    return SettleEnd::Settled(Settled {
                        probe: sample,
                        generation,
                        timing,
                    });
                }
            }
        } else if let Some(sample) = &probe {
            // A new draw or newly pending work invalidates the confirmation
            // window. A newer store for the same draw does not: the next
            // settled sample on that draw is exactly what we want to bind.
            if let Some(current) = &candidate {
                if sample.draw_seq != current.draw_seq || !sample.settled {
                    candidate = None;
                }
            }
        }
        // An idle screen emits nothing; one wake per ~20 ms while a settle is
        // waiting is enough to confirm the same draw without a poll cadence.
        if last_wake.elapsed() >= Duration::from_millis(20) {
            request_guest_probe(guest_stdin);
            last_wake = Instant::now();
        }
        if Instant::now() >= deadline {
            trace_host(
                probe_state,
                &format!("settle event timeout reason={last_reason} gen={generation}"),
            );
            return SettleEnd::Timeout {
                generation,
                probe_seq: probe.as_ref().map(|p| p.seq).unwrap_or(0),
                timing: SettleTiming {
                    mode: SettleMode::Strict,
                    first_change_ms,
                    frame_ms: None,
                    state_changed: None,
                    bound: "timeout",
                    rule: Some("event"),
                    reason: Some(last_reason),
                },
            };
        }
        cgroup.sample();
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[allow(clippy::too_many_arguments)]
fn wait_for_settled_observation(
    child: &mut std::process::Child,
    cgroup: &mut crate::cgroup::RunCgroup,
    live: &acos_services::registry::LiveHandles,
    probe_state: &Arc<std::sync::Mutex<ProbeState>>,
    guest_stdin: &mut Option<std::process::ChildStdin>,
    deadline: Instant,
    min_generation: u64,
    min_probe_seq: u64,
    previous_fnv: Option<u64>,
    mode: SettleMode,
    baseline_probe: Option<ProbeSample>,
    publish_epoch_ms: u64,
) -> SettleEnd {
    if mode == SettleMode::Strict && strict_settle_event() {
        return wait_for_event_settled(
            child,
            cgroup,
            live,
            probe_state,
            guest_stdin,
            deadline,
            min_generation,
            min_probe_seq,
            previous_fnv,
            publish_epoch_ms,
        );
    }
    let quiet = Duration::from_millis(150);
    let fast_window = Duration::from_millis(env_u64("ACOSLAUNCH_FAST_WINDOW_MS", 100));
    let fast_deadline = Instant::now() + fast_window;
    let mut fast_fell_back = false;
    let mut last_generation = min_generation;
    let mut last_change = Instant::now();
    let mut last_change_ms = host_ms();
    let mut first_change_probe: Option<ProbeSample> = None;
    let (unsupported_baseline, _) = latest_unsupported(probe_state);
    // P10: one immediate sample at strict entry, so the 150 ms quiet window
    // never waits on the slow fallback poll on an idle screen. Fast mode keeps
    // its pre-action baseline: it must not take an extra sample at entry (that
    // sample can already be the post-input state) and relies on the dirty
    // probe, exactly as the fast rule intends.
    if mode == SettleMode::Strict {
        request_guest_probe(guest_stdin);
    }
    let mut last_wake = Instant::now();
    loop {
        // A structured guest marker is more informative than the generic
        // "no fresh frame" timeout, and it is the only honest result for an
        // Android API the launcher does not implement yet.
        let (unsupported_seq, marker) = latest_unsupported(probe_state);
        if unsupported_seq > unsupported_baseline {
            return SettleEnd::Unsupported(marker.unwrap_or(serde_json::Value::Null));
        }
        match child.try_wait() {
            Ok(Some(status)) => return SettleEnd::GuestExited(status),
            _ => {}
        }
        let generation = latest_generation(live);
        if generation > last_generation {
            trace_host(
                probe_state,
                &format!("settle generation {last_generation}->{generation}"),
            );
            last_generation = generation;
            last_change = Instant::now();
            last_change_ms = host_ms();
        }
        let probe = latest_probe(probe_state);
        let probe_after = probe
            .as_ref()
            .map(|p| p.at_ms >= last_change_ms)
            .unwrap_or(false);
        // P10: the last store can land after the last event-driven sample
        // (the draw listener fires before the buffer is committed). Ask for
        // one sample now instead of waiting for the fallback poll; this keeps
        // the strict invariant "a probe sample after the last store" at the
        // same cost it had with the 50 ms poll.
        if !probe_after && last_change.elapsed() >= quiet && last_wake.elapsed() >= Duration::from_millis(20)
        {
            request_guest_probe(guest_stdin);
            last_wake = Instant::now();
        }
        // Fast mode binds the guest's own post-input tree sample. The baseline
        // was taken before the gesture was published, so this can only be a
        // change the input caused.
        let tree_changed = match (&baseline_probe, &probe) {
            (Some(baseline), Some(sample)) => sample.tree != baseline.tree,
            _ => false,
        };
        if tree_changed && first_change_probe.is_none() {
            first_change_probe = probe.clone();
        }
        let first_change_ms = first_change_probe
            .as_ref()
            .map(|sample| sample.at_ms.saturating_sub(publish_epoch_ms));
        let frame_ms = |store_ms: u64| store_ms.checked_sub(publish_epoch_ms);
        let committed_store = first_change_probe
            .as_ref()
            .map(|commit| last_change_ms > commit.at_ms)
            .unwrap_or(false);
        let committed_now = mode == SettleMode::Fast
            && committed_store
            && first_change_probe
                .as_ref()
                .map(|commit| commit.seq > min_probe_seq)
                .unwrap_or(false)
            && generation > min_generation;
        if committed_now {
            let commit = first_change_probe.expect("committed without a probe");
            let timing = SettleTiming {
                mode,
                first_change_ms,
                frame_ms: frame_ms(last_change_ms),
                state_changed: Some(true),
                bound: "state-commit",
                rule: None,
                reason: None,
            };
            trace_host(
                probe_state,
                &format!(
                    "settle fast ready gen={generation} probe_seq={} first_change_ms={} frame_ms={}",
                    commit.seq,
                    first_change_ms.unwrap_or(0),
                    timing.frame_ms.unwrap_or(0)
                ),
            );
            return SettleEnd::Settled(Settled {
                probe: commit,
                generation,
                timing,
            });
        }
        let in_fast_window = mode == SettleMode::Fast
            && !fast_fell_back
            && Instant::now() < fast_deadline;
        if !in_fast_window {
            if mode == SettleMode::Fast && !fast_fell_back {
                fast_fell_back = true;
                trace_host(
                    probe_state,
                    &format!(
                        "settle fast window expired after_ms={} (falling back to strict quiet; no change claimed)",
                        fast_window.as_millis()
                    ),
                );
            }
            if generation > min_generation
                && probe
                    .as_ref()
                    .map(|p| p.seq > min_probe_seq)
                    .unwrap_or(false)
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
                    let (state_changed, bound) = if mode == SettleMode::Fast {
                        (Some(false), "quiet-fallback")
                    } else {
                        (None, "strict-quiet")
                    };
                    let timing = SettleTiming {
                        mode,
                        first_change_ms,
                        frame_ms: frame_ms(last_change_ms),
                        state_changed,
                        bound,
                        rule: None,
                        reason: None,
                    };
                    trace_host(
                        probe_state,
                        &format!(
                            "settle ready mode={} gen={generation} probe_seq={} quiet_ms={} first_change_ms={} frame_ms={}",
                            mode.as_str(),
                            probe.as_ref().map(|p| p.seq).unwrap_or(0),
                            last_change.elapsed().as_millis(),
                            first_change_ms.unwrap_or(0),
                            timing.frame_ms.unwrap_or(0)
                        ),
                    );
                    return SettleEnd::Settled(Settled {
                        probe: probe.expect("probe checked above"),
                        generation,
                        timing,
                    });
                }
            }
        }
        if Instant::now() >= deadline {
            return SettleEnd::Timeout {
                generation,
                probe_seq: probe.as_ref().map(|p| p.seq).unwrap_or(0),
                timing: SettleTiming {
                    mode,
                    first_change_ms,
                    frame_ms: None,
                    state_changed: if mode == SettleMode::Fast {
                        Some(false)
                    } else {
                        None
                    },
                    bound: "timeout",
                    rule: None,
                    reason: None,
                },
            };
        }
        cgroup.sample();
        std::thread::sleep(Duration::from_millis(15));
    }
}

/// Is this probe sample usable for a fast tree request that started when C1 was
/// at `entry_generation` and after probe sequence `min_probe_seq`?
///
/// This is P3b's whole invalidation rule. A sample is fresh when the C1 store
/// counter it was parsed at is at least the generation current when the request
/// began (so no frame landed between the sample and the request) and its guest
/// sequence is past the caller's floor. Nothing about a 150 ms quiet window is
/// involved: that is the strict rule, unchanged.
fn probe_usable_for_fast_tree(
    probe: &ProbeSample,
    entry_generation: u64,
    min_probe_seq: u64,
) -> bool {
    probe.seq > min_probe_seq && probe.generation >= entry_generation
}

/// Ask the guest for one probe sample on its main looper, out of its 50 ms
/// cadence. The guest reads `PROBE` lines from stdin (interactive sessions only)
/// and wakes the probe thread immediately; this never changes the view tree.
fn request_guest_probe(stdin: &mut Option<std::process::ChildStdin>) {
    if let Some(writer) = stdin.as_mut() {
        use std::io::Write;
        let _ = writeln!(writer, "PROBE");
        let _ = writer.flush();
    }
}

enum FastTreeEnd {
    Ready(ProbeSample),
    Timeout { probe_seq: u64 },
    GuestExited(std::process::ExitStatus),
    Unsupported(serde_json::Value),
}

/// P3b fast tree: return a probe sample that is bound to a frame at least as
/// new as the request's entry generation, triggering one immediate guest probe
/// if the newest sample predates it. No quiet window is waited for.
#[allow(clippy::too_many_arguments)]
fn wait_for_fast_tree(
    child: &mut std::process::Child,
    cgroup: &mut crate::cgroup::RunCgroup,
    live: &acos_services::registry::LiveHandles,
    probe_state: &Arc<std::sync::Mutex<ProbeState>>,
    guest_stdin: &mut Option<std::process::ChildStdin>,
    deadline: Instant,
    min_probe_seq: u64,
) -> FastTreeEnd {
    let entry_generation = latest_generation(live);
    let (unsupported_baseline, _) = latest_unsupported(probe_state);
    let base_seq = latest_probe(probe_state).map(|p| p.seq).unwrap_or(0);
    let mut requested = false;
    loop {
        let (unsupported_seq, marker) = latest_unsupported(probe_state);
        if unsupported_seq > unsupported_baseline {
            return FastTreeEnd::Unsupported(marker.unwrap_or(serde_json::Value::Null));
        }
        match child.try_wait() {
            Ok(Some(status)) => return FastTreeEnd::GuestExited(status),
            _ => {}
        }
        let probe = latest_probe(probe_state);
        if let Some(ref probe) = probe {
            if probe_usable_for_fast_tree(probe, entry_generation, min_probe_seq)
                && (!requested || probe.seq > base_seq)
            {
                trace_host(
                    probe_state,
                    &format!(
                        "fast-tree ready probe_seq={} entry_gen={entry_generation} probe_gen={} \
                         age_ms={} requested={requested}",
                        probe.seq,
                        probe.generation,
                        host_ms().saturating_sub(probe.at_ms)
                    ),
                );
                return FastTreeEnd::Ready(probe.clone());
            }
        }
        if !requested {
            requested = true;
            request_guest_probe(guest_stdin);
            trace_host(
                probe_state,
                "fast-tree requested an immediate guest probe",
            );
        }
        if Instant::now() >= deadline {
            return FastTreeEnd::Timeout {
                probe_seq: probe.map(|p| p.seq).unwrap_or(0),
            };
        }
        cgroup.sample();
        std::thread::sleep(Duration::from_millis(2));
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
    // P3b: the guest's stdin is the probe-request channel (`PROBE`), so a fast
    // tree can wake the probe thread instead of waiting for its 50 ms tick.
    // Interactive sessions are spawned with a piped stdin; non-interactive ones
    // have none and the request is a no-op.
    let mut guest_stdin = child.stdin.take();
    let startup_deadline = started + startup_budget;
    // P9: the drain thread signals this on every parsed probe sample, so the
    // loop below is woken by the guest instead of polling on a fixed 10 ms
    // sleep. `wake_seen` is the tick this loop has already consumed.
    let wake = Arc::clone(
        &probe_state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .wake,
    );
    let mut wake_seen = 0u64;
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
            return Ok(SessionEnd {
                status: Some(status),
                clean: false,
            });
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
                return Ok(SessionEnd {
                    status: None,
                    clean: false,
                });
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
            return Ok(SessionEnd {
                status: None,
                clean: false,
            });
        }
        // Woken by the guest's next probe sample; the 10 ms timeout is the
        // fallback for `activity-resumed.marker`, which is a file the guest
        // writes (no host event), so this loop never blocks past one old poll
        // quantum.
        wake.wait(&mut wake_seen, Duration::from_millis(10));
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

    let session_deadline =
        Instant::now() + Duration::from_secs(env_u64("ACOSLAUNCH_SESSION_SECS", 900));
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
                    return Ok(SessionEnd {
                        status: Some(status),
                        clean: false,
                    });
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
                        return Ok(SessionEnd {
                            status: None,
                            clean: false,
                        });
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
                    return Ok(SessionEnd {
                        status: None,
                        clean: false,
                    });
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
                // P3: absolute phase marks across processes (epoch microseconds).
                let p3_t_enter = p3_now_us();
                let mode = match SettleMode::from_request(&request) {
                    Ok(mode) => mode,
                    Err(error) => {
                        let reply = serde_json::json!({
                            "ok": false, "cmd": "tree", "code": "BAD_REQUEST",
                            "error": error,
                        });
                        protocol_write(&reply);
                        evidence.push(reply);
                        continue;
                    }
                };
                // A tree-only request (`pixels:false`) is the default shape for
                // `acos_observe_tree`/`acos_observe {tree}`: it returns the probe
                // tree plus cheap C1 frame identity, never a PNG. `pixels:true`
                // keeps the legacy capture reply for callers that need pixels.
                let want_pixels = request
                    .get("pixels")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(true);
                if mode == SettleMode::Fast && want_pixels {
                    let reply = serde_json::json!({
                        "ok": false, "cmd": "tree", "code": "BAD_REQUEST",
                        "error": "settle:fast is a tree-only mode; a fast frame may be mid-animation and cannot back a PNG capture",
                    });
                    protocol_write(&reply);
                    evidence.push(reply);
                    continue;
                }
                let probe = if mode == SettleMode::Fast {
                    let deadline = Instant::now() + Duration::from_millis(750);
                    match wait_for_fast_tree(
                        child,
                        cgroup,
                        live,
                        probe_state,
                        &mut guest_stdin,
                        deadline,
                        0,
                    ) {
                        FastTreeEnd::Ready(probe) => probe,
                        FastTreeEnd::Timeout { probe_seq } => {
                            let reply = serde_json::json!({
                                "ok": false, "cmd": "tree", "code": "NO_FRESH_TREE",
                                "error": format!(
                                    "no guest probe sample newer than the frame store that was current at request entry arrived within 750 ms (last probe_seq {probe_seq}); a fast tree never serves the pre-request sample"
                                ),
                                "settle": {
                                    "mode": "fast", "settled": false,
                                    "probe_seq": probe_seq, "tree_age_ms": null,
                                    "bound": "timeout",
                                },
                            });
                            protocol_write(&reply);
                            evidence.push(reply);
                            continue;
                        }
                        FastTreeEnd::Unsupported(marker) => {
                            let reply = serde_json::json!({
                                "ok": false, "cmd": "tree", "code": "UNSUPPORTED_ANDROID_API",
                                "error": "the app called an Android API the launcher does not implement",
                                "unsupported": marker,
                            });
                            protocol_write(&reply);
                            evidence.push(reply);
                            continue;
                        }
                        FastTreeEnd::GuestExited(status) => {
                            let reply = serde_json::json!({
                                "ok": false, "cmd": "tree", "code": "GUEST_EXITED",
                                "error": format!("the guest ended while binding the tree (status {status})"),
                            });
                            protocol_write(&reply);
                            evidence.push(reply);
                            write_session_evidence(out, &evidence);
                            return Ok(SessionEnd {
                                status: Some(status),
                                clean: false,
                            });
                        }
                    }
                } else {
                    // Strict (default) and unchanged: bind the tree and the pixels
                    // to the same settled state.
                    let settled = wait_for_settled_observation(
                        child,
                        cgroup,
                        live,
                        probe_state,
                        &mut guest_stdin,
                        Instant::now() + Duration::from_millis(1500),
                        0,
                        0,
                        None,
                        SettleMode::Strict,
                        None,
                        0,
                    );
                    match settled {
                        SettleEnd::Settled(settled) => settled.probe,
                        SettleEnd::Timeout { .. } | SettleEnd::Unsupported(_) => {
                            match latest_probe(probe_state) {
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
                            }
                        }
                        SettleEnd::GuestExited(status) => {
                            let reply = serde_json::json!({
                                "ok": false, "cmd": "tree", "code": "GUEST_EXITED",
                                "error": format!("the guest ended while binding the tree (status {status})"),
                            });
                            protocol_write(&reply);
                            evidence.push(reply);
                            write_session_evidence(out, &evidence);
                            return Ok(SessionEnd {
                                status: Some(status),
                                clean: false,
                            });
                        }
                    }
                };
                let p3_t_settled = p3_now_us();
                let tree_age_ms = host_ms().saturating_sub(probe.at_ms);
                let strict_event = mode == SettleMode::Strict && strict_settle_event();
                let mut settle_json = serde_json::json!({
                    "mode": mode.as_str(),
                    "settled": true,
                    "probe_seq": probe.seq,
                    "tree_age_ms": tree_age_ms,
                    "bound": if mode == SettleMode::Fast {
                        "probe-fresh"
                    } else if strict_event {
                        "strict-event"
                    } else {
                        "strict-quiet"
                    },
                    "meaning": if strict_event {
                        "mode=strict with ACOS_STRICT_SETTLE=event: the guest reported no pending framework UI work, the sample was taken after a draw at/after request entry, and the same draw stayed newest for the confirmation window; tree_age_ms is host receipt -> reply"
                    } else {
                        "mode=fast returns the newest probe sample bound to a frame at least as new as the C1 store current at request entry; mode=strict keeps the 150 ms quiet window; tree_age_ms is host receipt -> reply"
                    },
                });
                if strict_event {
                    if let Some(map) = settle_json.as_object_mut() {
                        map.insert("strict_rule".into(), serde_json::json!("event"));
                    }
                }
                if !want_pixels {
                    // Metadata-only frame identity: no buffer clone, no PNG
                    // encode/write, no md5 (P3: that capture was 34.5 ms of the
                    // request and the tree reply never used its pixels).
                    //
                    // P9: the logical sequence advances on every tree reply, not
                    // only on captures. Before this, a session that only ever
                    // took metadata trees answered `frame_seq: 0` forever, so a
                    // launch reply had no sequence to be newer than and two
                    // observations could not be ordered. The number stays a
                    // host-side logical frame id; the bound C1 buffer is named by
                    // `buffer_generation`/`frame_number` below.
                    captures += 1;
                    let meta = {
                        let state = live.compositor.lock().unwrap_or_else(|e| e.into_inner());
                        state.latest_frame_metadata()
                    };
                    let mut reply = serde_json::json!({
                        "ok": true,
                        "cmd": "tree",
                        "pixels": false,
                        "capture_source": CAPTURE_SOURCE,
                        "tree": probe.tree.clone(),
                        "probe_seq": probe.seq,
                        "probe_at_ms": probe.at_ms,
                        "settle": settle_json,
                        // Logical frame id (advances per tree reply, capture or
                        // not); frame identity below is the C1 metadata for the
                        // buffer this probe is bound to.
                        "frame_seq": captures,
                    });
                    if let Some(meta) = meta {
                        let map = reply.as_object_mut().expect("tree reply is an object");
                        map.insert("buffer_generation".into(), serde_json::json!(meta.generation));
                        map.insert("frame_number".into(), serde_json::json!(meta.frame_number));
                        map.insert("buffer_id".into(), serde_json::json!(meta.buffer_id));
                        map.insert("producer_id".into(), serde_json::json!(meta.producer_id));
                        map.insert("layer_id".into(), serde_json::json!(meta.layer_id));
                        map.insert("layer_name".into(), serde_json::json!(meta.layer_name));
                        map.insert("width".into(), serde_json::json!(meta.width));
                        map.insert("height".into(), serde_json::json!(meta.height));
                        map.insert(
                            "frame_identity".to_string(),
                            serde_json::json!("C1 buffer metadata (no pixel copy)"),
                        );
                    }
                    let p3_t_built = p3_now_us();
                    protocol_write(&reply);
                    let p3_t_reply = p3_now_us();
                    if p3_enabled() {
                        let tree_bytes = serde_json::to_vec(&probe.tree)
                            .map(|value| value.len())
                            .unwrap_or(0);
                        p3_trace(
                            "art",
                            "art.tree",
                            p3_t_enter,
                            p3_t_reply,
                            &format!(
                                "settle_us={} capture_us=0 build_reply_us={} mode={} \
                                 probe_seq={} probe_at_ms={} frame_seq={} tree_bytes={}",
                                p3_t_settled.saturating_sub(p3_t_enter),
                                p3_t_reply.saturating_sub(p3_t_built),
                                mode.as_str(),
                                probe.seq,
                                probe.at_ms,
                                captures,
                                tree_bytes
                            ),
                        );
                    }
                    evidence.push(serde_json::json!({
                        "cmd": "tree", "ok": true, "pixels": false,
                        "frame_seq": captures,
                        "probe_seq": probe.seq,
                    }));
                } else {
                    match capture_c1(live, frames_dir, &mut captures) {
                        Ok(Some(capture)) => {
                            last_capture_fnv = Some(capture.fnv1a);
                            let mut reply = capture.fields();
                            let map = reply.as_object_mut().expect("capture fields are an object");
                            map.insert("ok".into(), serde_json::json!(true));
                            map.insert("cmd".into(), serde_json::json!("tree"));
                            map.insert("pixels".into(), serde_json::json!(true));
                            map.insert("tree".into(), probe.tree.clone());
                            map.insert("probe_seq".into(), serde_json::json!(probe.seq));
                            map.insert("probe_at_ms".into(), serde_json::json!(probe.at_ms));
                            map.insert("settle".into(), settle_json);
                            let p3_t_built = p3_now_us();
                            protocol_write(&reply);
                            let p3_t_reply = p3_now_us();
                            if p3_enabled() {
                                let tree_bytes = serde_json::to_vec(&probe.tree)
                                    .map(|value| value.len())
                                    .unwrap_or(0);
                                p3_trace(
                                    "art",
                                    "art.tree",
                                    p3_t_enter,
                                    p3_t_reply,
                                    &format!(
                                        "settle_us={} capture_us={} build_reply_us={} \
                                         probe_seq={} probe_at_ms={} frame_seq={} tree_bytes={}",
                                        p3_t_settled.saturating_sub(p3_t_enter),
                                        p3_t_built.saturating_sub(p3_t_settled),
                                        p3_t_reply.saturating_sub(p3_t_built),
                                        probe.seq,
                                        probe.at_ms,
                                        capture.seq,
                                        tree_bytes
                                    ),
                                );
                            }
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
                // P1: the fast baseline is the tree *before* the input is
                // published, so a sample that differs from it can only be the
                // input's own state change. The publish timestamp is the zero
                // for ack_ms / first_change_ms / frame_ms.
                let mode = match SettleMode::from_request(&request) {
                    Ok(mode) => mode,
                    Err(error) => {
                        let reply = serde_json::json!({
                            "ok": false, "cmd": cmd, "code": "BAD_REQUEST",
                            "error": error,
                        });
                        protocol_write(&reply);
                        evidence.push(reply);
                        continue;
                    }
                };
                let baseline_probe = latest_probe(probe_state);
                let publish_epoch_ms = host_ms();
                let p0 = baseline_probe.as_ref().map(|p| p.seq).unwrap_or(0);
                let g0 = latest_generation(live);
                // W59: the guest's ack is the end of the host-visible input
                // phase; the looper lines the guest printed in between are the
                // stages. Both are stamped on the same monotonic base.
                let injection_started = Instant::now();
                trace_host(
                    probe_state,
                    &format!(
                        "{cmd} publish_begin x={x} y={y} ack_wait_ms={}",
                        ack_wait.as_millis()
                    ),
                );
                let injected = match (cmd, key_code) {
                    ("tap", _) => live.input.tap(x, y, ack_wait),
                    ("type_text", _) => live
                        .input
                        .type_text(text_arg.as_deref().unwrap_or_default(), ack_wait),
                    (_, Some(code)) => live.input.key(code, ack_wait),
                    _ => unreachable!(),
                };
                let injected = match injected {
                    Ok(injected) => {
                        trace_host(
                            probe_state,
                            &format!(
                                "{cmd} acks_returned total_ms={} seqs={:?} acks={:?}",
                                injection_started.elapsed().as_millis(),
                                injected.seqs,
                                injected
                                    .acks
                                    .iter()
                                    .map(|a| (a.seq, a.handled, a.latency_us))
                                    .collect::<Vec<_>>()
                            ),
                        );
                        injected
                    }
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
                // P1: the gesture's own FINISHED latency (stale acks are
                // filtered out in InputHost::collect, so this is publish ->
                // matching FINISHED, not the window-attach time).
                let ack_ms = injected
                    .acks
                    .iter()
                    .filter_map(|ack| ack.latency_us)
                    .map(|us| us / 1000)
                    .max()
                    .unwrap_or(0);
                let (u_baseline, _) = latest_unsupported(probe_state);
                let settled = wait_for_settled_observation(
                    child,
                    cgroup,
                    live,
                    probe_state,
                    &mut guest_stdin,
                    Instant::now() + frame_wait,
                    g0,
                    p0,
                    last_capture_fnv,
                    mode,
                    baseline_probe,
                    publish_epoch_ms,
                );
                let (settled_probe, g1, p1, settle_timing) = match settled {
                    SettleEnd::Settled(settled) => (
                        Some(settled.probe.clone()),
                        settled.generation,
                        settled.probe.seq,
                        settled.timing,
                    ),
                    SettleEnd::Timeout {
                        generation,
                        probe_seq,
                        timing,
                    } => (None, generation, probe_seq, timing),
                    SettleEnd::Unsupported(marker) => {
                        let reply = serde_json::json!({
                            "ok": false, "cmd": cmd, "code": "UNSUPPORTED_ANDROID_API",
                            "error": format!(
                                "the app called {} and the launcher has no multi-Activity path; the guest stayed alive and no Activity was fabricated",
                                marker.get("call").and_then(|v| v.as_str()).unwrap_or("startActivity")
                            ),
                            "unsupported": marker,
                            "gesture": {"seqs": injected.seqs, "token": format!("{:#x}", injected.token.0)},
                        });
                        protocol_write(&reply);
                        evidence.push(reply);
                        continue;
                    }
                    SettleEnd::GuestExited(status) => {
                        // Prefer a structured marker that arrived in the same
                        // tick the guest died.
                        let (seq, marker) = latest_unsupported(probe_state);
                        if seq > u_baseline {
                            let reply = serde_json::json!({
                                "ok": false, "cmd": cmd, "code": "UNSUPPORTED_ANDROID_API",
                                "error": format!(
                                    "the app called {} and the launcher has no multi-Activity path",
                                    marker.as_ref().and_then(|m| m.get("call")).and_then(|v| v.as_str())
                                        .unwrap_or("startActivity")
                                ),
                                "unsupported": marker,
                                "gesture": {"seqs": injected.seqs, "token": format!("{:#x}", injected.token.0)},
                            });
                            protocol_write(&reply);
                            evidence.push(reply);
                            continue;
                        }
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
                        return Ok(SessionEnd {
                            status: Some(status),
                            clean: false,
                        });
                    }
                };
                trace_host(
                    probe_state,
                    &format!(
                        "{cmd} settle_done after_ms={} settled={} mode={}",
                        injection_started.elapsed().as_millis(),
                        settled_probe.is_some(),
                        settle_timing.mode.as_str()
                    ),
                );
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
                        "settle": settle_timing.to_json(false, ack_ms),
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
                        // W43: a full disk is a host condition, not an app or
                        // engine bug: name it and mark it retryable.
                        let code = if is_disk_full(&error) { "HOST_DISK_FULL" } else { "INTERNAL" };
                        let reply = serde_json::json!({
                            "ok": false, "cmd": cmd, "code": code,
                            "retryable": is_disk_full(&error),
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
                map.insert("settle".into(), settle_timing.to_json(true, ack_ms));
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

#[cfg(test)]
fn encode_png_rgba(width: u32, height: u32, stride: u32, rgba: &[u8]) -> Vec<u8> {
    encode_png_rgba_with(width, height, stride, rgba, crc32, adler32)
}

/// Encode the frame and hand its bytes to `sink` as they are produced.
///
/// The bytes are the same as [`encode_png_rgba`]'s; the only difference is that
/// the caller sees the PNG in order, so it can write and hash it without a
/// second pass over a finished 9.5 MB buffer (P2).
fn encode_png_rgba_stream<S: FnMut(&[u8])>(
    width: u32,
    height: u32,
    stride: u32,
    rgba: &[u8],
    mut sink: S,
) {
    encode_png_rgba_into(width, height, stride, rgba, crc32, adler32, &mut sink)
}

/// The buffered encoder: `#[cfg(test)]` only, kept as the reference the streamed
/// encoder is compared against, and parameterised by its two checksums so the
/// oracle (the original bit-serial CRC32 and the `%`-per-byte Adler-32) can
/// produce the exact bytes the encoder is required to preserve.
#[cfg(test)]
fn encode_png_rgba_with(
    width: u32,
    height: u32,
    stride: u32,
    rgba: &[u8],
    crc32: fn(&[u8]) -> u32,
    adler32: fn(&[u8]) -> u32,
) -> Vec<u8> {
    let mut out = Vec::with_capacity(
        (width as usize)
            .saturating_mul(height as usize)
            .saturating_mul(4)
            .saturating_add(1024),
    );
    encode_png_rgba_into(width, height, stride, rgba, crc32, adler32, &mut |bytes| {
        out.extend_from_slice(bytes)
    });
    out
}

fn encode_png_rgba_into(
    width: u32,
    height: u32,
    stride: u32,
    rgba: &[u8],
    crc32: fn(&[u8]) -> u32,
    adler32: fn(&[u8]) -> u32,
    sink: &mut dyn FnMut(&[u8]),
) {
    let t0 = m5_now_us();
    let row = (width as usize).saturating_mul(4);
    let stride_bytes = (stride as usize).saturating_mul(4);
    let filtered_len = row
        .saturating_add(1)
        .saturating_mul(height as usize);
    let t_filter = m5_now_us();
    m5_trace("art.encode_filter", t0, t_filter, "streamed=1");
    sink(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]);
    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]); // 8-bit RGBA, no interlace
    png_chunk(sink, b"IHDR", &ihdr, crc32);

    // The zlib stream is written straight to the sink in stored blocks: the
    // logical "filtered" byte stream (a zero filter byte then one row of pixels
    // per row) is read straight from `rgba`, so no full-frame copy exists. Both
    // the IDAT CRC and the Adler-32 are folded over the same bytes as they are
    // emitted (W1 removed the last frame-sized `filtered` buffer).
    let block_count = filtered_len.div_ceil(65_535);
    let idat_len = 2 + block_count * 5 + filtered_len + 4;
    sink(&(idat_len as u32).to_be_bytes());
    sink(b"IDAT");
    let mut crc = Crc32Stream::new();
    crc.update(b"IDAT");
    emit_checked(sink, &mut crc, &[0x78, 0x01]);
    let mut adler = Adler32::new();
    let mut offset = 0usize;
    while offset < filtered_len {
        let len = (filtered_len - offset).min(65_535);
        let final_block = offset + len == filtered_len;
        let mut header = [0u8; 5];
        header[0] = if final_block { 1 } else { 0 };
        header[1..3].copy_from_slice(&(len as u16).to_le_bytes());
        header[3..5].copy_from_slice(&(!(len as u16)).to_le_bytes());
        emit_checked(sink, &mut crc, &header);
        emit_filtered_range(
            sink,
            &mut crc,
            &mut adler,
            rgba,
            row,
            stride_bytes,
            height as usize,
            offset,
            len,
        );
        offset += len;
    }
    let t_stored = m5_now_us();
    m5_trace("art.encode_stored", t_filter, t_stored, "");
    let adler = adler.finish();
    let t_adler = m5_now_us();
    m5_trace("art.encode_adler32", t_stored, t_adler, "streamed=1");
    emit_checked(sink, &mut crc, &adler.to_be_bytes());
    let t_crc = m5_now_us();
    let crc = crc.finish();
    m5_trace(
        "art.encode_crc32",
        t_crc,
        m5_now_us(),
        &format!("chunk=IDAT body_bytes={idat_len}"),
    );
    sink(&crc.to_be_bytes());
    let t_idat = m5_now_us();
    m5_trace("art.encode_idat_chunk", t_adler, t_idat, "");
    png_chunk(sink, b"IEND", &[], crc32);
    m5_trace("art.encode_total", t0, m5_now_us(), "");
}

/// Emit `len` bytes of the logical filtered stream starting at `offset`.
///
/// The stream is `[0, row pixels]` per row, with `stride_bytes - row` bytes of
/// the source skipped between rows. A row whose end is past `rgba` is emitted
/// as zeros, exactly as the old buffered encoder did. Each piece is folded into
/// the IDAT CRC and the Adler-32 in the same pass, so `rgba` is never copied
/// into a frame-sized buffer.
#[allow(clippy::too_many_arguments)]
fn emit_filtered_range(
    sink: &mut dyn FnMut(&[u8]),
    crc: &mut Crc32Stream,
    adler: &mut Adler32,
    rgba: &[u8],
    row: usize,
    stride_bytes: usize,
    height: usize,
    mut offset: usize,
    mut len: usize,
) {
    const ZEROS: [u8; 4096] = [0u8; 4096];
    let filtered_row = row.saturating_add(1);
    if filtered_row == 0 {
        return;
    }
    while len > 0 {
        let y = offset / filtered_row;
        let within = offset % filtered_row;
        let take = (filtered_row - within).min(len);
        if within == 0 {
            emit_checked_adler(sink, crc, adler, &[0u8]);
        }
        let pixel_off = within.saturating_sub(1);
        let pixel_len = take - usize::from(within == 0);
        if pixel_len > 0 {
            let row_start = y.saturating_mul(stride_bytes);
            let row_end = row_start.saturating_add(row);
            let start = row_start.saturating_add(pixel_off);
            if y < height && row_end <= rgba.len() && start + pixel_len <= rgba.len() {
                emit_checked_adler(sink, crc, adler, &rgba[start..start + pixel_len]);
            } else {
                let mut left = pixel_len;
                while left > 0 {
                    let n = left.min(ZEROS.len());
                    emit_checked_adler(sink, crc, adler, &ZEROS[..n]);
                    left -= n;
                }
            }
        }
        offset += take;
        len -= take;
    }
}

/// Emit `bytes`, folding them into both the chunk CRC and the zlib Adler-32.
fn emit_checked_adler(
    sink: &mut dyn FnMut(&[u8]),
    crc: &mut Crc32Stream,
    adler: &mut Adler32,
    bytes: &[u8],
) {
    sink(bytes);
    crc.update(bytes);
    adler.update(bytes);
}

/// Emit `bytes` and fold them into the chunk CRC in the same call.
fn emit_checked(sink: &mut dyn FnMut(&[u8]), crc: &mut Crc32Stream, bytes: &[u8]) {
    sink(bytes);
    crc.update(bytes);
}

fn png_chunk(
    sink: &mut dyn FnMut(&[u8]),
    kind: &[u8; 4],
    body: &[u8],
    crc32: fn(&[u8]) -> u32,
) {
    sink(&(body.len() as u32).to_be_bytes());
    sink(kind);
    sink(body);
    let t_crc = m5_now_us();
    let crc = if body.is_empty() {
        crc32(kind)
    } else {
        // CRC-32 is linear over the concatenation: run it over the kind and
        // continue over the body, so the body is not copied into a third buffer.
        let mut crc = Crc32Stream::new();
        crc.update(kind);
        crc.update(body);
        crc.finish()
    };
    let label = String::from_utf8_lossy(kind).to_string();
    m5_trace(
        "art.encode_crc32",
        t_crc,
        m5_now_us(),
        &format!("chunk={label} body_bytes={}", body.len()),
    );
    sink(&crc.to_be_bytes());
}

/// IEEE CRC-32 (the one PNG chunks use), slice-by-8 and streaming.
///
/// The eight 256-entry tables are built at compile time; the previous bit-serial
/// loop cost 43 ms of one warm screenshot's 80 ms
/// (`results/art-runtime/D-M5.md`). `Crc32Stream` lets the encoder fold each
/// produced chunk in as it is emitted instead of copying the whole body into a
/// second buffer first (P2).
const CRC32_TABLES: [[u32; 256]; 8] = crc32_make_tables();

struct Crc32Stream {
    crc: u32,
}

impl Crc32Stream {
    fn new() -> Self {
        Crc32Stream { crc: 0xffff_ffff }
    }

    fn update(&mut self, data: &[u8]) {
        let mut crc = self.crc;
        let mut chunks = data.chunks_exact(8);
        for c in &mut chunks {
            let low = u32::from_le_bytes([c[0], c[1], c[2], c[3]]) ^ crc;
            crc = CRC32_TABLES[7][(low & 0xff) as usize]
                ^ CRC32_TABLES[6][((low >> 8) & 0xff) as usize]
                ^ CRC32_TABLES[5][((low >> 16) & 0xff) as usize]
                ^ CRC32_TABLES[4][(low >> 24) as usize]
                ^ CRC32_TABLES[3][c[4] as usize]
                ^ CRC32_TABLES[2][c[5] as usize]
                ^ CRC32_TABLES[1][c[6] as usize]
                ^ CRC32_TABLES[0][c[7] as usize];
        }
        for &byte in chunks.remainder() {
            crc = (crc >> 8) ^ CRC32_TABLES[0][((crc ^ u32::from(byte)) & 0xff) as usize];
        }
        self.crc = crc;
    }

    fn finish(self) -> u32 {
        !self.crc
    }
}

const fn crc32_make_tables() -> [[u32; 256]; 8] {
    let mut tables = [[0u32; 256]; 8];
    let mut i = 0usize;
    while i < 256 {
        let mut crc = i as u32;
        let mut bit = 0;
        while bit < 8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xedb8_8320
            } else {
                crc >> 1
            };
            bit += 1;
        }
        tables[0][i] = crc;
        i += 1;
    }
    let mut k = 1usize;
    while k < 8 {
        let mut i = 0usize;
        while i < 256 {
            let prev = tables[k - 1][i];
            tables[k][i] = (prev >> 8) ^ tables[0][(prev & 0xff) as usize];
            i += 1;
        }
        k += 1;
    }
    tables
}

fn crc32(data: &[u8]) -> u32 {
    let mut stream = Crc32Stream::new();
    stream.update(data);
    stream.finish()
}

/// zlib's Adler-32 with the deferred modulo: `NMAX` bytes are summed in `u32`
/// and reduced once per block, instead of `%` on every byte. `NMAX = 5552` is
/// the largest block for which `b` cannot overflow a `u32` from a reduced state.
const ADLER_NMAX: usize = 5552;

/// Incremental Adler-32 over the zlib stream's filtered bytes. Calling
/// [`Adler32::update`] on arbitrary slices gives the same result as
/// [`adler32`] on their concatenation: both reduce modulo 65521 at the same
/// block boundaries and the sums are associative modulo 65521.
struct Adler32 {
    a: u32,
    b: u32,
}

impl Adler32 {
    fn new() -> Self {
        Adler32 { a: 1, b: 0 }
    }

    fn update(&mut self, data: &[u8]) {
        let mut a = self.a;
        let mut b = self.b;
        let mut rest = data;
        while !rest.is_empty() {
            let n = rest.len().min(ADLER_NMAX);
            let (block, tail) = rest.split_at(n);
            for &byte in block {
                a += u32::from(byte);
                b += a;
            }
            a %= 65_521;
            b %= 65_521;
            rest = tail;
        }
        self.a = a;
        self.b = b;
    }

    fn finish(self) -> u32 {
        (self.b << 16) | self.a
    }
}

fn adler32(data: &[u8]) -> u32 {
    let mut adler = Adler32::new();
    adler.update(data);
    adler.finish()
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
        && fs::read_to_string(&stamp)
            .map(|s| s.trim() == hash.to_string())
            .unwrap_or(false)
    {
        return Ok(out);
    }
    fs::write(&src, SOURCE)?;
    let clang = ndk_clang()?;
    // W37 (flagged call-site edit in the W36-owned launch.rs): compile once into
    // the shared content-addressed cache; a cold cohort serialises on the build
    // lock and every later launch only installs the verified `.so`.
    let cache_key = crate::build_cache::digest(&[
        b"acos-art/binder-driver/v2",
        SOURCE.as_bytes(),
        crate::build_cache::file_id(&clang).as_bytes(),
    ]);
    let _build_lock = crate::build_cache::lock(&cache_key)?;
    if let Some(hit) =
        crate::build_cache::reuse_to(&cache_key, &["libacos_binder_driver.so"], &dir)?
    {
        fs::write(&stamp, format!("{hash}\n"))?;
        return Ok(hit[0].clone());
    }
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
    crate::build_cache::publish_locked(&cache_key, &dir, &["libacos_binder_driver.so"])?;
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
        && fs::read_to_string(&stamp)
            .map(|s| s.trim() == hash.to_string())
            .unwrap_or(false)
    {
        return Ok(dex);
    }
    let sdk = android_sdk()?;
    let android_jar = sdk.join("platforms/android-34/android.jar");
    let d8 = find_tool(&sdk, "d8")?;
    let javac = PathBuf::from(std::env::var("JAVAC").unwrap_or_else(|_| "javac".into()));
    // W37 (flagged call-site edit in the W36-owned launch.rs): the launch guest
    // DEX is a pure function of these sources and the toolchain. Build it once
    // under the shared cache lock; subsequent launches (and the other N-1
    // sessions of a cold cohort) install the verified `classes.dex` instead.
    let cache_key = {
        let mut parts: Vec<Vec<u8>> = vec![b"acos-art/launch-guest/v2".to_vec()];
        for p in &sources {
            parts.push(p.to_string_lossy().as_bytes().to_vec());
            parts.push(fs::read(p)?);
        }
        for tool in [&android_jar, &d8, &javac] {
            parts.push(crate::build_cache::file_id(tool).into_bytes());
        }
        parts.push(b"--min-api 34".to_vec());
        crate::build_cache::digest_owned(&parts)
    };
    let _build_lock = crate::build_cache::lock(&cache_key)?;
    if let Some(hit) = crate::build_cache::reuse_to(&cache_key, &["classes.dex"], &out_dir)? {
        fs::write(&stamp, format!("{hash}\n"))?;
        return Ok(hit[0].clone());
    }
    let classes = out_dir.join("classes");
    let _ = fs::remove_dir_all(&classes);
    fs::create_dir_all(&classes)?;
    let out = Command::new(&javac)
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
    crate::build_cache::publish_locked(&cache_key, &out_dir, &["classes.dex"])?;
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
    Err(Error::Tool(
        "no Android SDK found (set ANDROID_SDK_ROOT)".into(),
    ))
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
    Err(Error::Tool(format!(
        "no {name} under {}/build-tools",
        sdk.display()
    )))
}

fn ndk_clang() -> Result<PathBuf> {
    let mut roots: Vec<PathBuf> = Vec::new();
    if let Some(p) =
        std::env::var_os("ANDROID_NDK_HOME").or_else(|| std::env::var_os("ANDROID_NDK_ROOT"))
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
        for target in [
            "x86_64-linux-android34-clang",
            "x86_64-linux-android33-clang",
        ] {
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

/// The APK's launcher component, from the parsed manifest (`PackageParser`
/// rules: `<activity>` or `<activity-alias>` with MAIN/LAUNCHER).
fn apk_launcher_activity(apk: &Path) -> Result<acos_services::manifest_resolve::LauncherActivity> {
    acos_services::manifest_resolve::launcher_activity(apk).map_err(|e| {
        Error::Tool(format!(
            "{}: {} ({}; resolved from AndroidManifest.xml, not aapt2 badging)",
            apk.display(),
            e,
            e.code()
        ))
    })
}

/// Read `key=value` from a marker file's lines.
fn marker_u64(marker: &str, key: &str) -> Option<u64> {
    marker
        .lines()
        .find_map(|l| l.strip_prefix(key)?.strip_prefix('='))
        .and_then(|v| v.trim().parse().ok())
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

#[cfg(test)]
mod probe_line_tests {
    use super::*;

    fn drain(lines: &str) -> ProbeState {
        let state = Arc::new(std::sync::Mutex::new(ProbeState::default()));
        drain_stdout_lines(
            std::io::Cursor::new(lines.as_bytes().to_vec()),
            4096,
            Arc::clone(&state),
            None,
        );
        let out = state.lock().unwrap().clone_for_test();
        out
    }

    impl ProbeState {
        fn clone_for_test(&self) -> ProbeState {
            ProbeState {
                latest: self.latest.clone(),
                unsupported: self.unsupported.clone(),
                unsupported_seq: self.unsupported_seq,
                probe_error: self.probe_error.clone(),
                probe_errors: self.probe_errors,
                guest_launch: self.guest_launch.clone(),
                oversized_lines: self.oversized_lines,
                bad_probe_lines: self.bad_probe_lines,
                trace: None,
                wake: Arc::clone(&self.wake),
            }
        }
    }

    /// Teeth for the W32 misinterpretation: a two-window sample is larger than
    /// the old 128 KiB line cap. It must still be parsed (and its `windows`
    /// carried into the tree), or the tree silently freezes at the last
    /// single-window sample exactly when a dialog opens.
    #[test]
    fn a_probe_line_above_the_old_128k_cap_is_parsed() {
        let pad = "a".repeat(200 * 1024);
        let line = format!(
            "ACOSPROBE {{\"seq\":7,\"windows\":[{{\"z\":0,\"window\":\"main\"}},{{\"z\":1,\"window\":\"w2#1\"}}],\"tree\":{{\"nodes\":[],\"window_count\":2,\"node_budget\":2000,\"truncated\":false,\"pad\":\"{pad}\"}}}}\n"
        );
        assert!(line.len() > 128 * 1024);
        let state = drain(&line);
        let sample = state.latest.expect("the large sample must be published");
        assert_eq!(sample.seq, 7);
        assert_eq!(sample.tree["window_count"], 2);
        assert_eq!(sample.tree["windows"][1]["window"], "w2#1");
        assert_eq!(state.bad_probe_lines, 0);
        assert_eq!(state.oversized_lines, 0);
    }

    /// A guest probe throw must reach the host with its payload, never be
    /// swallowed (W32's only visible symptom was a frozen `seq`).
    #[test]
    fn a_thrown_probe_surfaces_acosprobe_error() {
        let line = "ACOSPROBE-ERROR {\"seq\":12,\"phase\":\"window\",\"window\":\"w2#1\",\"error\":\"java.lang.NullPointerException: boom\",\"stack\":[\"at x.y\"]}\n";
        let state = drain(line);
        assert_eq!(state.probe_errors, 1);
        let error = state.probe_error.expect("the error must be retained");
        assert_eq!(error["phase"], "window");
        assert_eq!(error["window"], "w2#1");
        assert!(
            error["error"].as_str().unwrap().contains("NullPointerException"),
            "the exception text must survive"
        );
    }

    /// Invalid JSON (the real W27/W32 two-root separator bug) is counted and
    /// named, not silently dropped.
    #[test]
    fn an_invalid_probe_line_is_counted() {
        let state = drain("ACOSPROBE {\"seq\":1,\"tree\":{\"nodes\":[]}}{\"seq\":2}\n");
        assert!(state.latest.is_none());
        assert_eq!(state.bad_probe_lines, 1);
    }

    fn sample(seq: u64, generation: u64) -> ProbeSample {
        ProbeSample {
            seq,
            tree: serde_json::json!({"nodes": []}),
            at_ms: 1_000,
            generation,
            draw_ms: 0,
            draw_seq: 0,
            settled: false,
            pending: Vec::new(),
        }
    }

    /// P3b's invalidation rule: a fast tree may only use a probe parsed at a C1
    /// generation at least as new as the store current at request entry, and
    /// with a sequence past the caller's floor. A probe from before a frame
    /// store or an input settle is never served as current.
    #[test]
    fn fast_tree_rejects_a_probe_from_before_the_entry_generation() {
        // Store advanced to 9 after this sample was parsed at 8.
        assert!(!probe_usable_for_fast_tree(&sample(4, 8), 9, 0));
        // Same generation: the sample is current for that store.
        assert!(probe_usable_for_fast_tree(&sample(4, 9), 9, 0));
        // A newer store than the sample is fine as long as the sample was
        // already at/after the entry generation (stores during the walk do not
        // invalidate a fast observation).
        assert!(probe_usable_for_fast_tree(&sample(5, 9), 9, 0));
        // The guest sequence floor is honoured independently.
        assert!(!probe_usable_for_fast_tree(&sample(5, 9), 9, 5));
        assert!(probe_usable_for_fast_tree(&sample(6, 9), 9, 5));
        // No frame has ever landed: generation 0 is usable when none is needed.
        assert!(probe_usable_for_fast_tree(&sample(1, 0), 0, 0));
    }

    /// Oversized input is drained whole and counted, so a following sample is
    /// still parsed.
    #[test]
    fn an_oversized_line_is_drained_and_counted() {
        let mut cursor = std::io::Cursor::new(b"0123456789ABCDEF\nnext\n".to_vec());
        assert!(read_capped_line(&mut cursor, 8).unwrap().is_none());
        let next = read_capped_line(&mut cursor, 8).unwrap().unwrap();
        assert_eq!(next, b"next\n");
    }
}

#[cfg(test)]
mod shared_boot_image_tests {
    use super::*;

    /// The default stays the default: unset means no image flag, no bind, no
    /// behaviour change.
    #[test]
    fn unset_means_the_default_lz4_path() {
        assert!(resolve_shared_boot_image(None).unwrap().is_none());
    }

    /// A directory with `<dir>/x86_64/boot.art` is accepted verbatim (it is what
    /// `with_image` ro-binds to `/data/img`).
    #[test]
    fn a_directory_with_the_main_image_is_accepted() {
        let dir = std::env::temp_dir().join(format!("acos-b3-shared-image-{}", std::process::id()));
        let isa = dir.join("x86_64");
        std::fs::create_dir_all(&isa).unwrap();
        std::fs::write(isa.join("boot.art"), b"art").unwrap();
        let got = resolve_shared_boot_image(Some(dir.clone().into_os_string())).unwrap();
        assert_eq!(got.as_deref(), Some(dir.as_path()));
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A typo must fail the launch, not silently measure the compressed image.
    #[test]
    fn a_directory_without_the_main_image_is_an_error() {
        let dir = std::env::temp_dir().join(format!(
            "acos-b3-no-shared-image-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let err = resolve_shared_boot_image(Some(dir.clone().into_os_string()))
            .expect_err("a missing x86_64/boot.art must be refused");
        let text = err.to_string();
        assert!(matches!(err, Error::Layout(_)), "got {text}");
        assert!(text.contains("no x86_64/boot.art"), "got {text}");
        assert!(text.contains(ENV_SHARED_BOOT_IMAGE), "got {text}");
        std::fs::remove_dir_all(&dir).ok();
    }
}

#[cfg(test)]
#[path = "png_checksum_tests.rs"]
mod png_checksum_tests;

#[cfg(test)]
#[path = "md5_frame_tests.rs"]
mod md5_frame_tests;
