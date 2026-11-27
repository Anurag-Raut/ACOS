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
use acos_binder_peer::{close_frame_fds, listen, write_frame, Frame, Peer};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::os::unix::io::RawFd;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

/// The guest class that performs the framework startup.
pub const GUEST_CLASS: &str = "dev.acos.art.LaunchMain";

/// P12: the agent's choice for framework animation scales (launch-time).
///
/// `On` is the default and keeps real Android behaviour. `Off` installs the
/// standard CI setting (the three global animation scales at `0`), which makes
/// `ValueAnimator`/`ObjectAnimator`/`StateListAnimator`, the native
/// `RenderNodeAnimator` (ripple) and the framework's press/elevation animators
/// finish without their duration. It is for functional checks, never for
/// animation/transition testing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Animations {
    /// Real framework animation behaviour (scale 1.0).
    #[default]
    On,
    /// All animation scales 0 (standard CI setting).
    Off,
}

impl Animations {
    /// Parse `on`/`off` (case-insensitive).
    pub fn parse(value: &str) -> std::result::Result<Self, String> {
        match value.trim().to_ascii_lowercase().as_str() {
            "on" => Ok(Self::On),
            "off" => Ok(Self::Off),
            other => Err(format!(
                "animations must be \"on\" or \"off\" (got {other:?})"
            )),
        }
    }

    /// The stable wire/label string.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::On => "on",
            Self::Off => "off",
        }
    }

    /// `true` when real animation durations are requested.
    #[must_use]
    pub const fn is_on(self) -> bool {
        matches!(self, Self::On)
    }
}

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
    /// Panel width in pixels (the launch display).
    pub width: u32,
    /// Panel height in pixels.
    pub panel_height: u32,
    /// Density in dpi.
    pub density_dpi: u32,
    /// Status-bar top inset in pixels (the window's top inset).
    pub status_inset: u32,
    /// Navigation-bar bottom inset in pixels.
    pub nav_inset: u32,
    /// W7: app-window inset model (`carve` = pre-W7 carved frame,
    /// `report` = AOSP full-display frame plus reported insets).
    pub insets: String,
    /// P12: per-session framework animation scales.
    pub animations: Animations,
    /// ADR-0029 item 4: the `services:` specs this session attaches
    /// (`[{id, profile, config}]`). Empty means `core` only (today's service
    /// table plus tombstones).
    pub services: Vec<serde_json::Value>,
    /// MPROC1: extra APKs installed into this session beside the launched one
    /// (repeatable `--extra-apk`). Each is staged into this run's private tree,
    /// bound read-only at `/data/app/<name>`, installed into the session's
    /// package manager at its own uid and seeded into the activity manager's
    /// manifest table, exactly as the launched APK is. Empty (and then free)
    /// for every single-package launch: nothing is staged, bound or installed.
    pub extra_apks: Vec<PathBuf>,
    /// LAUNCHEXTRAS: the launch `Intent` the caller declared (action, data URI
    /// and typed extras). The host encodes it with the exact
    /// `Intent.writeToParcel` bytes and the guest's first launch transaction
    /// carries that Intent, so `Activity.getIntent()` observes it unchanged.
    /// Empty (the default) keeps the guest-built `ACTION_MAIN` Intent.
    pub intent: acos_protocol::LaunchIntent,
    /// MPROC5c: idle kill. A **lazily spawned** secondary process that has held
    /// nothing for this long — no started activity, no started/bound service,
    /// no visible window — is killed (stock
    /// `ActivityManagerConstants.MAX_CACHED_PROCESSES` / `CachedAppOptimizer`),
    /// its `ProcessRecord` and window sessions go, and the next intent spawns it
    /// again. `0` disables it. `--idle-kill-seconds`, default 30.
    pub idle_kill_secs: u64,
}

impl Default for LaunchOpts {
    fn default() -> Self {
        Self {
            apk: PathBuf::new(),
            until: "application-created".into(),
            out: None,
            timeout: Duration::from_secs(120),
            width: 1080,
            panel_height: 2400,
            density_dpi: 420,
            status_inset: 128,
            nav_inset: 63,
            insets: acos_services::wms::DEFAULT_INSETS_MODE.as_str().to_string(),
            animations: Animations::On,
            services: Vec::new(),
            extra_apks: Vec::new(),
            intent: acos_protocol::LaunchIntent::default(),
            idle_kill_secs: 30,
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
    /// MPROC1: every extra package this session installed beside the launched
    /// one — `{package, uid, apk, native_library_dir}` with the **guest** paths
    /// the package manager published. Empty for a single-package launch, so a
    /// launch reply only carries the field when extras were asked for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra_packages: Option<Vec<serde_json::Value>>,
    /// MPROC4 step 3b: the session's app processes. The activity manager's own
    /// records (one per process tag: `tag`, `package`, `uid`, `pid`,
    /// `app_thread`, `attached`) plus the launcher's spawn evidence (which
    /// component resolved, why, which pid answered). Absent when the session
    /// never had more than the primary process.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub processes: Option<serde_json::Value>,
    /// H13b: `{mode, cache: hit|built|fallback|external|none, reason}` — which
    /// boot image this launch used and whether the shared-image cache hit, so a
    /// caller (and a benchmark row) can tell a warm serve from a cold one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub boot_image: Option<crate::boot_image::BootImageReport>,
    /// ABIGATE: the package-level ABI gate's own evidence when the launch was
    /// refused before any guest existed (`code: UNSUPPORTED_ABI`):
    /// `{apk_abis: [...], host_abi: "x86_64", libs_sample: [...]}`. Absent on
    /// every launch that reached the guest.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub abi: Option<serde_json::Value>,
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

/// H13: the boot-image source selector (`lz4`, `shared-fixed`, `shared-aslr`).
/// See [`crate::boot_image`] for the full contract. Unset = `lz4`, the product
/// default; anything else is resolved before the sandbox is wired and fails the
/// launch if unusable.
pub const ENV_ART_BOOT_IMAGE: &str = crate::boot_image::ENV_BOOT_IMAGE;

/// B3's explicit shared-image directory, now the `shared-fixed` input. Kept for
/// compatibility: with `ACOS_ART_BOOT_IMAGE` unset it still selects the fixed
/// image it always did.
pub const ENV_SHARED_BOOT_IMAGE: &str = crate::boot_image::ENV_SHARED_BOOT_IMAGE;

/// Reject a hot-attached APK whose staged file name is already used by the
/// launched APK or an existing session extra (same rule as initial staging):
/// `stage_apk` would otherwise overwrite that APK's staged bytes.
fn hot_apk_name_collision(
    session_apks: &[(PathBuf, String)],
    apk: &Path,
    name: &str,
) -> Result<()> {
    let guest = format!("/data/app/{name}");
    if let Some((staged, _)) = session_apks.iter().find(|(_, g)| *g == guest) {
        return Err(Error::Layout(format!(
            "hot system APK {}: file name {name} collides with an APK already staged in this session ({})",
            apk.display(),
            staged.display()
        )));
    }
    Ok(())
}

#[cfg(test)]
mod hot_apk_collision_tests {
    use super::*;

    #[test]
    fn hot_apk_name_collision_rejects_existing_name_and_names_it() {
        let apks = vec![(
            PathBuf::from("/run/app/settings.apk"),
            "/data/app/settings.apk".to_string(),
        )];
        let err = hot_apk_name_collision(&apks, Path::new("/x/settings.apk"), "settings.apk")
            .unwrap_err()
            .to_string();
        assert!(err.contains("settings.apk"), "{err}");
        assert!(hot_apk_name_collision(&apks, Path::new("/x/other.apk"), "other.apk").is_ok());
    }
}

/// Validate `ACOS_SHARED_BOOT_IMAGE` for the B3 compatibility tests (the launch
/// path now goes through [`crate::boot_image::resolve`]).
#[cfg(test)]
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

/// Z7 (research seam, all OFF by default): real ART heap options, plumbed into
/// the `dalvikvm64` argv instead of only the served `dalvik.vm.*` property table.
///
/// W48's `ACOS_ART_HEAP*` seam changes what the guest *reads* as a property;
/// `dalvikvm64` never turns those properties into runtime options (Z6 §5.6), so
/// they cannot change the heap. These three variables are the argv form ART
/// actually parses at startup (`dalvikvm` main -> `Runtime::ParseOptions`), and
/// unset means the argv is byte-for-byte what it was before this seam.
pub const ENV_ART_XMS: &str = "ACOS_ART_XMS";
pub const ENV_ART_XMX: &str = "ACOS_ART_XMX";
pub const ENV_ART_HEAP_TARGET_UTILIZATION: &str = "ACOS_ART_HEAP_TARGET_UTILIZATION";

/// H2: the guest-side post-first-frame release hook, now **on by default**.
/// Right after its first live frame the guest asks ART for one background CC
/// collection (`VMRuntime.requestConcurrentGC()`), instead of waiting for the
/// post-fork `ReduceTargetFootprintTask` schedule (+2 s/+10 s). Z7 measured
/// `System.gc()` inert on this image, so H2 removed it; the request is made
/// once. `ACOS_ART_EARLY_GC=0` is the escape hatch / A-B off arm;
/// `ACOS_ART_GC_DELAY_MS` delays the request (research knob, 0 = at the first
/// frame). Both are forwarded to the cleared guest environment.
pub const ENV_ART_EARLY_GC: &str = "ACOS_ART_EARLY_GC";
pub const ENV_ART_GC_DELAY_MS: &str = "ACOS_ART_GC_DELAY_MS";

/// MPROC1: the env spelling of [`LaunchOpts::extra_apks`] a frozen host-template
/// child reads (see `host_zygote.rs`). The direct path uses the repeatable
/// `--extra-apk` flags instead; both name the same list.
pub const ENV_EXTRA_APKS: &str = "ACOSLAUNCH_EXTRA_APKS";

fn valid_heap_size(text: &str) -> bool {
    let t = text.trim();
    if t.is_empty() || !t.as_bytes()[0].is_ascii_digit() {
        return false;
    }
    t.bytes()
        .all(|b| b.is_ascii_digit() || matches!(b, b'k' | b'K' | b'm' | b'M' | b'g' | b'G'))
}

fn heap_size_argv(var: &str, raw: Option<std::ffi::OsString>) -> Result<Option<String>> {
    let Some(raw) = raw else { return Ok(None) };
    let value = raw.to_string_lossy().to_string();
    if valid_heap_size(&value) {
        Ok(Some(value.trim().to_string()))
    } else {
        Err(Error::Layout(format!(
            "{var}={value}: expected a size like 8m or 512m (unset it for the default heap)"
        )))
    }
}

/// Z7: the argv form of the three heap knobs (`-Xms`, `-Xmx`,
/// `-XX:HeapTargetUtilization=`). Empty unless the caller sets a variable.
fn resolve_heap_argv() -> Result<Vec<String>> {
    let mut out = Vec::new();
    if let Some(v) = heap_size_argv(ENV_ART_XMS, std::env::var_os(ENV_ART_XMS))? {
        out.push(format!("-Xms{v}"));
    }
    if let Some(v) = heap_size_argv(ENV_ART_XMX, std::env::var_os(ENV_ART_XMX))? {
        out.push(format!("-Xmx{v}"));
    }
    if let Some(raw) = std::env::var_os(ENV_ART_HEAP_TARGET_UTILIZATION) {
        let value = raw.to_string_lossy().to_string();
        let parsed = value.trim().parse::<f64>().ok();
        match parsed {
            Some(u) if u > 0.0 && u <= 1.0 => {
                out.push(format!("-XX:HeapTargetUtilization={}", value.trim()))
            }
            _ => {
                return Err(Error::Layout(format!(
                    "{ENV_ART_HEAP_TARGET_UTILIZATION}={value}: expected a ratio in (0, 1]"
                )))
            }
        }
    }
    Ok(out)
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
    /// The software gralloc interposer this binary was built with
    /// ([`crate::gralloc_shim`]): the sysroot's copy when it matches the
    /// embedded source, else a cached build of it.
    pub gralloc: PathBuf,
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
    // W7: the in-process peer's `WmsState` is built from this process's env, so
    // install the selected inset model before the peer host exists. The guest
    // also receives `ACOSLAUNCH_WM_INSETS` below so its DisplayInfo/Configuration
    // agree with the WMS frame it is about to be given.
    std::env::set_var("ACOS_WM_INSETS", &opts.insets);
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
    // H15: the host's cgroup-scope capability is a per-boot constant; the cache
    // stamp lives in this shared data root, so a relaunch (`reset`) does not pay
    // the two `systemd-run` capability probes again.
    crate::cgroup::set_capability_root(&root.data);
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
    let gralloc = crate::gralloc_shim(root)?;
    crate::stage("gralloc-shim");
    let shims = SessionShims {
        substrate,
        art_shim,
        driver,
        gralloc,
    };
    // ZM2 prototype C (research seam, off by default): with a plan, this
    // process is the warm host template that forks one launcher per session.
    // Unset => the exact production path below.
    if let Some(plan) = std::env::var_os(crate::host_zygote::ENV_PLAN) {
        return crate::host_zygote::run_template(root, Path::new(&plan), shims);
    }
    launch_session(root, opts, None, &shims)
}

/// MPROC1: one extra APK staged and installed into a launch session beside the
/// launched one.
///
/// Built by [`launch_session`] before any guest exists: the staged copy is this
/// run's own (H17), the guest paths are the ones the sandbox binds, and the uid
/// is filled in when the host's package manager installs the package.
#[derive(Debug, Clone)]
struct ExtraApk {
    /// This run's private copy of the APK.
    staged: PathBuf,
    /// Guest path the sandbox binds the copy at (`/data/app/<name>`).
    guest_path: String,
    /// Package name from the APK's manifest.
    package: String,
    /// Host root of this package's extracted native libraries.
    lib_root: PathBuf,
    /// Guest directory `lib_root` is bound at (`/data/app-lib-extra/<pkg>`).
    guest_lib_root: String,
    /// Guest `ApplicationInfo.nativeLibraryDir` for the package.
    guest_lib_dir: String,
    /// The uid the package manager assigned at install time.
    uid: Option<i32>,
}

/// MPROC8: what a lazily spawned process was started for.
///
/// AOSP distinguishes an activity launch from a component that runs without an
/// activity (`ActiveServices.realStartServiceLocked` for a started or bound
/// service, a provider publish). The guest's stop point and its launch argv
/// follow from it: an activity process marshals/wait for its launch transaction,
/// a service process attaches, stays resident and waits for the host's
/// `scheduleCreateService`/`scheduleBindService` deliveries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SpawnKind {
    /// `startActivity`: the process launches the requested activity.
    Activity,
    /// `bindService`/`startService`/`getContentProvider`: no activity.
    Service,
}

impl SpawnKind {
    /// The `until` stop point the guest runs to (`LaunchMain`'s 4th argument).
    fn until(self) -> &'static str {
        match self {
            Self::Activity => "activity-resumed",
            Self::Service => "service-resident",
        }
    }
}

/// MPROC4 step 3b: one secondary app process this session can start.
///
/// Built from the session's own recipe (`launch_session`): the extra APK is
/// already staged in this run's tree and installed into the package manager, so
/// a second guest is the **same** sandbox recipe with its own run root, its own
/// tag and its own launch argv. Nothing is re-downloaded, re-installed or
/// re-invented.
#[derive(Debug, Clone)]
struct SecondarySpec {
    /// Process tag (`ACOS_PROCESS_TAG`); `0` is the primary process.
    tag: u32,
    /// Package this process runs.
    package: String,
    /// Host path of the staged APK (this run's private copy).
    staged: PathBuf,
    /// Guest path the APK is bound at (`/data/app/<name>`).
    guest_apk: String,
    /// Host root of the package's extracted native libraries.
    lib_root: PathBuf,
    /// Guest directory the library root is bound at.
    guest_lib_root: String,
    /// Guest `ApplicationInfo.nativeLibraryDir`.
    guest_lib_dir: String,
    /// `ApplicationInfo.uid` the package manager assigned.
    uid: Option<i32>,
    /// MPROC8: what this process was started for (activity vs service-only).
    kind: SpawnKind,
    /// The service class the spawn request named (empty when the request was an
    /// activity launch). The host delivers the create/bind to the process; the
    /// guest never launches it.
    service: String,
    /// The activity this process launches (the requested component, else the
    /// APK's launcher activity).
    activity: String,
    /// Declared `<application android:name>` (empty = the framework default).
    app_class: String,
    /// Declared `<application android:theme>`.
    theme: i32,
    /// Manifest-derived `ApplicationInfo.flags` (`-1` = none).
    app_flags: i64,
    /// The `<activity android:configChanges>` bitmask.
    config_changes: u32,
    /// `ApplicationInfo.labelRes` / literal for the application.
    app_label: (u32, String),
    /// `ActivityInfo.labelRes` / literal for the launched activity.
    activity_label: (u32, String),
    /// The launcher alias's `targetActivity` (empty for a plain activity).
    activity_target: String,
    /// The process's own `out` directory (host path), copied into the session's
    /// evidence at the end.
    out_dir: PathBuf,
    /// The process's own run root (host path).
    run_root: PathBuf,
}

/// A running secondary process.
struct SecondaryGuest {
    spec: SecondarySpec,
    child: std::process::Child,
    cgroup: crate::cgroup::RunCgroup,
    pid: i32,
    /// Guest stderr tail (a dead process's reason is evidence).
    stderr: Arc<std::sync::Mutex<TailBuffer>>,
    /// `bwrap` argv, so the reply can state exactly how the process was started.
    argv: Vec<String>,
}

/// Everything a secondary guest needs from the session that is not in its
/// [`SecondarySpec`]: the shared, read-only sandbox inputs and the shared
/// sockets.
#[derive(Debug, Clone)]
struct SecondaryShared {
    root: ArtRoot,
    /// Boot image directory (bound at `/data/img`), when the session has one.
    image: Option<PathBuf>,
    substrate: PathBuf,
    art_shim: PathBuf,
    driver: PathBuf,
    /// The software gralloc interposer (see [`SessionShims::gralloc`]).
    gralloc: PathBuf,
    /// The session's extracted native-library root (`/data/app-lib`).
    native_lib_root: PathBuf,
    /// Session socket directory (bound at `/dev/socket`; both guests share it,
    /// so both write to the same logd).
    sockets: PathBuf,
    /// The peer socket file (bound at `/data/acos-launch.sock`); every guest
    /// connects to the **same** peer.
    sock: PathBuf,
    /// Static-RRO idmaps (guest paths, `,`-separated; empty = none).
    overlays: String,
    /// Resume budget forwarded to the guest (W27).
    resume_ms: u64,
    /// Display geometry the guest boots at.
    geometry: [(String, u32); 5],
    /// `ACOSLAUNCH_SCREEN_LAYOUT` (W47).
    screen_layout: u32,
    /// `ACOS_ANIMATIONS` (P12).
    animations: &'static str,
    /// Run this guest's live probe (interactive sessions only).
    live_probe: bool,
    /// Primary + extra session APKs (`(staged_host_path, guest_path)`) so any
    /// secondary process (e.g. Launcher resolving `ResolveInfo.loadLabel`) can
    /// read other installed packages' resources.
    session_apks: Vec<(PathBuf, String)>,
    /// Value to forward as `ACOS_ART_KEYSTORE` (from `security.keystore` module
    /// spec or legacy env fallback when the module spec omits it).
    keystore_env: Option<String>,
}

impl SecondaryShared {
    /// The guest argv after `-cp <dex> <class>`: exactly what `LaunchMain.main`
    /// reads for the primary process, for this package.
    fn guest_args(&self, spec: &SecondarySpec) -> Vec<String> {
        vec![
            spec.guest_apk.clone(),
            spec.package.clone(),
            "/data/out/launch".to_string(),
            spec.kind.until().to_string(),
            spec.activity.clone(),
            spec.app_class.clone(),
            spec.theme.to_string(),
            self.overlays.clone(),
            spec.app_label.0.to_string(),
            spec.app_label.1.clone(),
            spec.activity_label.0.to_string(),
            spec.activity_label.1.clone(),
            spec.activity_target.clone(),
            spec.app_flags.to_string(),
            "manifest".to_string(),
        ]
    }

    /// Build one secondary guest's bwrap command + dalvik argv.
    fn command(
        &self,
        spec: &SecondarySpec,
        app_meta: &str,
        providers: &str,
    ) -> Result<std::process::Command> {
        let mut sb = self.root.sandbox();
        if let Some(image) = &self.image {
            sb.with_image(Some(image.clone()));
        }
        sb.run_root(&spec.run_root);
        // UID: this process carries **its package's** uid (AOSP allocates one
        // per installed package), not the session's. A spawn whose package has
        // no allocated uid is refused instead of running as another app's uid.
        let Some(uid) = spec.uid else {
            return Err(Error::Layout(format!(
                "process tag {}: the package manager allocated no uid for {}",
                spec.tag, spec.package
            )));
        };
        sb.app_uid(uid as u32);
        sb.seccomp_guest(true);
        sb.with_shim(false);
        sb.ro_bind(&self.art_shim, "/data/shim/libartshim.so");
        sb.ro_bind(
            &self.native_lib_root,
            acos_services::native_libs::GUEST_NATIVE_LIB_ROOT,
        );
        sb.ro_bind(&self.substrate, "/data/app/libacosart.so");
        sb.ro_bind(&spec.staged, &spec.guest_apk);
        for (host_apk, guest_apk) in &self.session_apks {
            if guest_apk != &spec.guest_apk {
                sb.ro_bind(host_apk, guest_apk);
            }
        }
        sb.ro_bind(&spec.lib_root, &spec.guest_lib_root);
        sb.ro_bind(&self.gralloc, "/data/libacoshw.so");
        sb.ro_bind(&self.driver, "/data/lib/libacos_binder_driver.so");
        sb.ro_bind(&self.root.data.join("seccomp"), "/data/seccomp");
        // The **same** sockets directory as the primary (both guests write to
        // the session's logd) and the **same** peer socket file.
        sb.rw_bind(&self.sockets, "/dev/socket");
        sb.rw_bind(&self.sock, "/data/acos-launch.sock");
        sb.env(
            "LD_PRELOAD",
            "/data/shim/libartshim.so:/data/app/libacosart.so:/data/libacoshw.so:/data/lib/libacos_binder_driver.so",
        );
        sb.env("ACOSART_LOAD", "libandroid_runtime.so");
        sb.env("E3G_SOCK", "/data/acos-launch.sock");
        // MPROC2/MPROC4: this process names itself in the driver's handshake, so
        // the peer puts its tag on every binder it imports and can address a
        // delivery to it (and only to it).
        sb.env("ACOS_PROCESS_TAG", &spec.tag.to_string());
        sb.env("ACOS_PROCESS_NAME", &spec.package);
        // CR1FIX2 #3: test-only seam, a splash-style fixture waits this long between
        // `super.onCreate()` and `finish()`. Unset in normal runs (no delay).
        if let Ok(v) = std::env::var("ACOS_TEST_FINISH_DELAY_MS") {
            sb.env("ACOS_TEST_FINISH_DELAY_MS", &v);
        }
        sb.env("ACOSLAUNCH_RESUME_MS", &self.resume_ms.to_string());
        // MPROC7 (ADR-0029 amendment): this process's first launch is a
        // host-built `ClientTransaction` (system_server owns the activity
        // token); the guest waits for it instead of marshalling its own.
        sb.env("ACOSLAUNCH_HOST_SCHEDULES", "1");
        for (var, value) in &self.geometry {
            sb.env(var, &value.to_string());
        }
        sb.env("ACOSLAUNCH_SCREEN_LAYOUT", &self.screen_layout.to_string());
        sb.env(
            "ACOSLAUNCH_CONFIG_CHANGES",
            &spec.config_changes.to_string(),
        );
        sb.env("ACOS_ANIMATIONS", self.animations);
        if !app_meta.is_empty() {
            sb.env("ACOSLAUNCH_APP_META", app_meta);
        }
        if !providers.is_empty() {
            sb.env("ACOSLAUNCH_PROVIDERS", providers);
        }
        if self.live_probe {
            sb.env("ACOSLAUNCH_LIVE_PROBE", "1");
        }
        if let Some(v) = &self.keystore_env {
            sb.env(acos_services::modules::keystore::KEYSTORE_ENV, v);
        }
        let mut argv: Vec<String> = Vec::new();
        if self.image.is_some() {
            argv.push("-Ximage:/data/img/boot.art".into());
            argv.push("-Xnorelocate".into());
        }
        argv.push("-cp".into());
        argv.push("/data/app/launch-guest/classes.dex".into());
        argv.push(GUEST_CLASS.into());
        argv.extend(self.guest_args(spec));
        sb.command("/system/apex/com.android.art/bin/dalvikvm64", &argv)
    }
}

/// True when a raw `ACOS_ART_KEYSTORE` value enables `SoftwareKeyStore` in the
/// guest (`LaunchMain.java` accepts `on`, `1`, or `software`, case-insensitive).
fn keystore_env_enables_software(raw: &str) -> bool {
    let mode = raw.trim();
    mode.eq_ignore_ascii_case("on") || mode == "1" || mode.eq_ignore_ascii_case("software")
}

/// Reconcile an inherited `ACOS_ART_KEYSTORE` environment variable into the
/// effective module spec list before attachment and report collection.
///
/// When `specs` omits `security.keystore` and `inherited` enables the guest
/// provider (`on`, `1`, `software`), appends `security.keystore` with
/// `Profile::Real` so the module registry, launch reply, and `status` control
/// agree with guest availability. Explicit specs (including `profile: off`) are
/// preserved untouched.
fn reconcile_inherited_keystore_module(
    specs: &mut Vec<acos_services::module::ModuleSpec>,
    inherited: Option<&str>,
) {
    use acos_services::module::{ModuleConfig, ModuleSpec, Profile};
    if acos_services::modules::keystore::requested_profile(specs).is_some() {
        return;
    }
    if inherited.is_some_and(keystore_env_enables_software) {
        specs.push(ModuleSpec {
            id: acos_services::modules::keystore::MODULE_ID.to_string(),
            profile: Profile::Real,
            config: ModuleConfig::default(),
        });
    }
}

/// Resolve the guest `ACOS_ART_KEYSTORE` env value from the session's module
/// specs, preserving explicit `off` override over any inherited host variable.
fn resolve_keystore_env(
    specs: &[acos_services::module::ModuleSpec],
    inherited: Option<&str>,
) -> Option<String> {
    use acos_services::module::Profile;
    match acos_services::modules::keystore::requested_profile(specs) {
        Some(Profile::Real) => Some("on".to_string()),
        Some(_) => None,
        None => inherited
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(str::to_string),
    }
}

/// Start one secondary guest (MPROC4 step 3b).
///
/// The process is scoped like the primary (`cgroup` scope, its own process
/// group, `PDEATHSIG`), its own `out`/`app-data`/`dalvik-cache` live in its own
/// run root, and its stdout/stderr are drained so nothing can block it.
fn spawn_secondary(
    shared: &SecondaryShared,
    spec: &SecondarySpec,
    app_meta: &str,
    providers: &str,
    probe_state: &Arc<std::sync::Mutex<ProbeState>>,
    compositor: Arc<std::sync::Mutex<acos_services::surface_client::SurfaceState>>,
    stderr_sink: Arc<std::sync::Mutex<TailBuffer>>,
) -> Result<SecondaryGuest> {
    for sub in [
        "out",
        "out/launch",
        "app-data",
        "dalvik-cache",
        "misc",
        "tmp",
    ] {
        fs::create_dir_all(spec.run_root.join(sub))?;
    }
    fs::create_dir_all(spec.run_root.join("app-data").join(&spec.package))?;
    let mut cmd = shared.command(spec, app_meta, providers)?;
    let argv = format!("{cmd:?}");
    let scope_info = std::fs::canonicalize(&spec.run_root)
        .unwrap_or_else(|_| spec.run_root.clone())
        .join("cgroup");
    let (mut child, cgroup) = crate::cgroup::spawn_scoped(
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
                    crate::watchdog::relax_child_limits();
                    crate::watchdog::apply_init_rlimit_nice();
                    Ok(())
                });
            }
            c.process_group(0);
            c.stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
        },
    )?;
    let pid = child.id() as i32;
    // Both streams are drained for the process's whole life: a guest that
    // prints must never block on a full pipe, and its probe lines
    // (`ACOSPROBE`) feed the same tree the session serves.
    let cap = stream_cap_from_env();
    if let Some(out) = child.stdout.take() {
        // The second process's own probe lines feed the session's tree, so a
        // `tree` reply describes the window that is actually on top.
        let probe = Arc::clone(probe_state);
        let comp = compositor;
        let log = spec.out_dir.join("stdout.log");
        let tag = spec.tag;
        std::thread::spawn(move || {
            let _ = drain_tee_lines(out, cap, &log, probe, Some(comp), tag);
        });
    }
    if let Some(err) = child.stderr.take() {
        let sink = Arc::clone(&stderr_sink);
        let log = spec.out_dir.join("stderr.log");
        std::thread::spawn(move || {
            let buf = drain_tee(err, cap, &log);
            if let Ok(mut sink) = sink.lock() {
                *sink = buf;
            }
        });
    }
    eprintln!(
        "acos-art: launch: started secondary process tag={} package={} activity={} pid={pid}",
        spec.tag, spec.package, spec.activity
    );
    Ok(SecondaryGuest {
        spec: spec.clone(),
        child,
        cgroup,
        pid,
        stderr: stderr_sink,
        argv: vec![argv],
    })
}

/// The session's secondary processes (MPROC4 step 3b).
///
/// One place that owns every process the launcher started lazily: it decides
/// whether a package already runs (the launcher is the authority on tags and
/// pids), starts the guest the first time a component of that package needs one,
/// and kills every one of them when the session ends.
struct SecondaryPool {
    specs: Vec<SecondarySpec>,
    shared: SecondaryShared,
    guests: std::collections::BTreeMap<u32, SecondaryGuest>,
    /// Spawn requests received (`package`, `component`, `reason`, `tag`,
    /// `pid`, `started`) — evidence a reply reports.
    records: Vec<serde_json::Value>,
    /// Requests refused because the package has no installed secondary spec.
    uninstalled: Vec<serde_json::Value>,
    /// Spawns that failed (`package`, `error`), never a silent drop.
    failures: Vec<serde_json::Value>,
    /// MPROC5c: processes the idle kill took (`tag`, package, pid, idle_ms).
    kills: Vec<serde_json::Value>,
    /// MPROC7: the activity manager's state. The launcher owns every process it
    /// started, so when **it** kills or reaps one (`idle_kill`'s
    /// waitpid/cgroup kill, `reap`'s `try_wait`) it removes the `ProcessRecord`
    /// itself; waiting for the peer to see the socket close made the process
    /// table depend on a transport event that may never be observed (a peer
    /// thread blocked in a service handler, a suppressed close, an immediate
    /// respawn under the same tag).
    atms: Arc<std::sync::Mutex<acos_services::atms::AtmsState>>,
    /// SYSUI2: the session's **primary** package (tag 0). A spawn request for it
    /// is a start into a process that already runs, not a package to start.
    primary_package: String,
    started: Instant,
    /// Session run root for hot-staged secondary system APKs (`system.settings`).
    run_root: PathBuf,
    /// Session output dir for per-process secondary logs/dirs.
    out_dir: PathBuf,
    /// Next secondary process tag to allocate on hot-attach.
    next_tag: u32,
}

fn build_secondary_spec(
    tag: u32,
    extra: &ExtraApk,
    run_root_base: &Path,
    out_base: &Path,
) -> SecondarySpec {
    let facts = match extra.uid {
        Some(uid) => acos_services::pms::PmsState::from_apk_with_uid(&extra.staged, uid).ok(),
        None => acos_services::pms::PmsState::from_apk(&extra.staged).ok(),
    };
    let launcher = apk_launcher_activity(&extra.staged).ok();
    let labels = acos_services::pms::manifest_labels(&extra.staged).ok();
    let run_root = run_root_base.join("secondary").join(format!("tag-{tag}"));
    let out_dir = out_base.join(format!("process-{tag}"));
    let _ = fs::create_dir_all(&out_dir);
    SecondarySpec {
        tag,
        package: extra.package.clone(),
        staged: extra.staged.clone(),
        guest_apk: extra.guest_path.clone(),
        lib_root: extra.lib_root.clone(),
        guest_lib_root: extra.guest_lib_root.clone(),
        guest_lib_dir: extra.guest_lib_dir.clone(),
        uid: extra.uid,
        kind: SpawnKind::Activity,
        service: String::new(),
        activity: launcher
            .as_ref()
            .map(|l| l.component.clone())
            .unwrap_or_default(),
        app_class: facts
            .as_ref()
            .map(|state| state.app().class_name.clone())
            .filter(|name| !name.is_empty())
            .unwrap_or_default(),
        theme: facts.as_ref().map_or(0, |state| state.app().theme),
        app_flags: facts
            .as_ref()
            .map(|state| ((state.app().flags & !(1 << 29)) | (1 << 23)) as i64)
            .unwrap_or(-1),
        config_changes: launcher
            .as_ref()
            .map_or(0, |l| l.config_changes.max(0) as u32),
        app_label: labels
            .as_ref()
            .map(|l| {
                (
                    l.application.res.max(0) as u32,
                    l.application.literal.clone().unwrap_or_default(),
                )
            })
            .unwrap_or((0, String::new())),
        activity_label: launcher
            .as_ref()
            .map(|l| {
                (
                    l.label.res.max(0) as u32,
                    l.label.literal.clone().unwrap_or_default(),
                )
            })
            .unwrap_or((0, String::new())),
        activity_target: launcher
            .as_ref()
            .and_then(|l| l.target.clone())
            .unwrap_or_default(),
        out_dir,
        run_root,
    }
}

impl SecondaryPool {
    fn new(
        specs: Vec<SecondarySpec>,
        shared: SecondaryShared,
        atms: Arc<std::sync::Mutex<acos_services::atms::AtmsState>>,
        primary_package: String,
        run_root: PathBuf,
        out_dir: PathBuf,
    ) -> Self {
        let next_tag = specs.iter().map(|s| s.tag).max().unwrap_or(0) + 1;
        Self {
            specs,
            shared,
            guests: std::collections::BTreeMap::new(),
            records: Vec::new(),
            uninstalled: Vec::new(),
            failures: Vec::new(),
            kills: Vec::new(),
            atms,
            primary_package,
            started: Instant::now(),
            run_root,
            out_dir,
            next_tag,
        }
    }

    /// Whether a package already has a registered secondary spec.
    fn has_package(&self, package: &str) -> bool {
        self.specs.iter().any(|s| s.package == package)
    }

    /// Stage a hot-attached system APK (e.g. `com.android.settings`) into the
    /// session's private run tree so it can be installed into PMS/ATMS and
    /// lazily spawned by [`Self::spawn`] without restarting the primary app.
    fn stage_hot_apk(&self, apk: &Path) -> Result<ExtraApk> {
        let name = apk
            .file_name()
            .map(|f| f.to_string_lossy().to_string())
            .filter(|n| !n.is_empty())
            .ok_or_else(|| {
                Error::Layout(format!(
                    "hot system APK {}: path has no file name",
                    apk.display()
                ))
            })?;
        hot_apk_name_collision(&self.shared.session_apks, apk, &name)?;
        let extra_package = crate::app::apk_package(apk)?;
        let app_dir = self.run_root.join("app");
        fs::create_dir_all(&app_dir)?;
        let staged = app_dir.join(&name);
        crate::app::stage_apk(apk, &staged)?;
        crate::app::ensure_apk_mountpoint(&self.shared.root.data, &name, apk)?;
        let guest_lib_root = format!("/data/app-lib-extra/{extra_package}");
        let lib_root = self.run_root.join("app-lib-extra").join(&extra_package);
        fs::create_dir_all(&lib_root)?;
        Ok(ExtraApk {
            staged,
            guest_path: format!("/data/app/{name}"),
            guest_lib_dir: format!("{guest_lib_root}/{}", acos_services::native_libs::HOST_ABI),
            guest_lib_root,
            lib_root,
            package: extra_package,
            uid: None,
        })
    }

    /// Register a staged hot system APK into `SecondaryPool` so the next
    /// matching intent lazily spawns it.
    fn register_hot_extra(&mut self, extra: ExtraApk) {
        if self.has_package(&extra.package) {
            return;
        }
        let tag = self.next_tag;
        self.next_tag += 1;
        let spec = build_secondary_spec(tag, &extra, &self.run_root, &self.out_dir);
        self.shared
            .session_apks
            .push((extra.staged.clone(), extra.guest_path.clone()));
        self.specs.push(spec);
    }

    /// Tear down a hot-detached system package: kill any running secondary
    /// process and remove its spec from the pool.
    fn remove_package(&mut self, package: &str) {
        if let Some(tag) = self.running_tag(package) {
            if let Some(mut guest) = self.guests.remove(&tag) {
                guest.cgroup.kill();
                let _ = guest.child.kill();
                let _ = guest.child.wait();
                self.atms
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .process_died(tag);
            }
        }
        let removed_paths: Vec<String> = self
            .specs
            .iter()
            .filter(|s| s.package == package)
            .map(|s| s.guest_apk.clone())
            .collect();
        self.specs.retain(|s| s.package != package);
        if !removed_paths.is_empty() {
            self.shared
                .session_apks
                .retain(|(_, guest_path)| !removed_paths.contains(guest_path));
        }
    }

    /// The tag a package already runs under, if any.
    fn running_tag(&self, package: &str) -> Option<u32> {
        self.guests
            .iter()
            .find(|(_, g)| g.spec.package == package)
            .map(|(tag, _)| *tag)
    }

    /// Handle one spawn request. Idempotent: a package that is already running
    /// gets no second process (the request is the *need*, not the spawn).
    fn spawn(
        &mut self,
        request: &acos_services::ams::ProcessSpawnRequest,
        probe_state: &Arc<std::sync::Mutex<ProbeState>>,
        compositor: Arc<std::sync::Mutex<acos_services::surface_client::SurfaceState>>,
    ) {
        // SYSUI2: the session's own app **is** a running process (`tag 0`). A
        // start of one of its Activities (the notification's content sender, an
        // app's own second Activity) records the launch and the peer delivers it
        // to that process (`Peer::take_host_transactions`); the launcher must not
        // try to start a package it never installed as a secondary.
        if !self.primary_package.is_empty() && request.package == self.primary_package {
            self.records.push(serde_json::json!({
                "package": request.package,
                "component": request.component,
                "reason": request.reason,
                "tag": 0,
                "spawned": false,
                "note": "the session's primary process (tag 0); the launch goes to it",
            }));
            return;
        }
        if let Some(tag) = self.running_tag(&request.package) {
            self.records.push(serde_json::json!({
                "package": request.package,
                "component": request.component,
                "reason": request.reason,
                "tag": tag,
                "spawned": false,
                "note": "process already running",
            }));
            return;
        }
        let spec = self
            .specs
            .iter()
            .find(|s| s.package == request.package)
            .cloned();
        let Some(mut spec) = spec else {
            // A component of a package this session never installed cannot be
            // run: say so (the resolution path recorded the need; the launcher
            // is the one that cannot satisfy it).
            self.uninstalled.push(serde_json::json!({
                "package": request.package,
                "component": request.component,
                "reason": request.reason,
                "error": "no such package was installed in this session",
            }));
            eprintln!(
                "acos-art: launch: {} needs a process for {} but no such package is installed ",
                request.package, request.component
            );
            return;
        };
        // MPROC8: what the process is being started *for* decides its stop
        // point. AOSP's `ActivityManagerService` starts one process object for
        // both cases and hands the work to the component's own path: an
        // activity gets a launch transaction, a service gets
        // `realStartServiceLocked` after `attachApplication`. A service
        // component is never launched as an Activity, and the process does not
        // wait for a launch transaction it will never receive.
        let class = request
            .component
            .split_once('/')
            .map(|(_, class)| class)
            .unwrap_or_default();
        let fqcn = if class.is_empty() {
            String::new()
        } else if class.starts_with('.') {
            format!("{}{}", spec.package, class)
        } else {
            class.to_string()
        };
        spec.kind = match request.reason.as_str() {
            "bindService" | "startService" | "getContentProvider" => SpawnKind::Service,
            _ => SpawnKind::Activity,
        };
        match spec.kind {
            SpawnKind::Activity => {
                if !fqcn.is_empty() {
                    spec.activity = fqcn;
                }
            }
            SpawnKind::Service => {
                spec.service = fqcn;
                spec.activity = String::new();
            }
        }
        // Per-process `ProviderInfoList` + application `<meta-data>`, from this
        // package's own manifest (the same two facts the primary receives), with
        // this package's own uid.
        let (app_meta, providers) = spec
            .uid
            .and_then(|uid| acos_services::pms::PmsState::from_apk_with_uid(&spec.staged, uid).ok())
            .map(|state| {
                let app_meta = meta_spec(&state.app().meta_data);
                let providers = provider_spec_of(&state);
                (app_meta, providers)
            })
            .unwrap_or_default();
        // MPROC7: a new process instance restarts its guest probe counter. Clear
        // the dead instance's sample and give the new one a base above the
        // session's current maximum, so `strict` settle still sees a *fresh*
        // sample after an idle kill / respawn.
        {
            let mut state = probe_state.lock().unwrap_or_else(|e| e.into_inner());
            let highest = state
                .by_process
                .values()
                .map(|p| p.seq)
                .chain(state.latest.as_ref().map(|p| p.seq))
                .max()
                .unwrap_or(0);
            state.seq_offsets.insert(spec.tag, highest);
            state.by_process.remove(&spec.tag);
            state.last_raw_seq.remove(&spec.tag);
        }
        let stderr = Arc::new(std::sync::Mutex::new(TailBuffer::with_cap(1 << 16)));
        match spawn_secondary(
            &self.shared,
            &spec,
            &app_meta,
            &providers,
            probe_state,
            compositor,
            Arc::clone(&stderr),
        ) {
            Ok(guest) => {
                let pid = guest.pid;
                self.records.push(serde_json::json!({
                    "package": spec.package,
                    "component": request.component,
                    "reason": request.reason,
                    "tag": spec.tag,
                    "pid": pid,
                    "uid": spec.uid,
                    "kind": match spec.kind {
                        SpawnKind::Activity => "activity",
                        SpawnKind::Service => "service",
                    },
                    "service": spec.service,
                    "activity": spec.activity,
                    "spawned": true,
                    "since_ms": self.started.elapsed().as_millis() as u64,
                }));
                self.guests.insert(spec.tag, guest);
            }
            Err(e) => {
                eprintln!(
                    "acos-art: launch: spawning process {} ({}) failed: {e}",
                    spec.tag, spec.package
                );
                self.failures.push(serde_json::json!({
                    "package": spec.package,
                    "tag": spec.tag,
                    "error": e.to_string(),
                }));
            }
        }
    }

    /// MPROC5c: kill every lazily spawned process that has held nothing for
    /// `idle`.
    ///
    /// The activity manager owns the rule (no started activity, no started or
    /// bound service — [`acos_services::atms::AtmsState::idle_processes`]); the
    /// window manager owns "no visible window" ([`WmsState::process_has_visible_window`]);
    /// the launcher owns the process, so it is the one that kills it. The
    /// process group goes (never one pid), the pool forgets it (so the next
    /// intent's spawn is a real spawn again), and the guest's socket close makes
    /// the peer drop its binder records, queue `F_DEATH` to its holders (MPROC5b's
    /// death half) and close its window sessions.
    fn idle_kill(
        &mut self,
        idle: Duration,
        atms: &Arc<std::sync::Mutex<acos_services::atms::AtmsState>>,
        wms: &Arc<std::sync::Mutex<acos_services::wms::WmsState>>,
    ) -> Vec<serde_json::Value> {
        if idle.is_zero() {
            return Vec::new();
        }
        let candidates = atms
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .idle_processes(idle);
        let mut killed = Vec::new();
        for (tag, package, idle_ms) in candidates {
            let visible = wms
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .process_has_visible_window(tag);
            if visible {
                continue;
            }
            let Some(mut guest) = self.guests.remove(&tag) else {
                continue;
            };
            let sessions = wms
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .sessions_of_process(tag)
                .len();
            let pid = guest.pid;
            guest.cgroup.kill();
            let _ = guest.child.kill();
            let _ = guest.child.wait();
            // MPROC7: the launcher killed it, so the launcher removes the
            // activity manager's `ProcessRecord` (the peer will also observe the
            // socket close, but the table must not depend on that).
            self.atms
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .process_died(tag);
            let record = serde_json::json!({
                "tag": tag,
                "package": package,
                "pid": pid,
                "reason": "idle",
                "idle_ms": idle_ms,
                "idle_kill_secs": idle.as_secs(),
                "window_sessions": sessions,
            });
            eprintln!(
                "acos-art: launch: idle kill tag={tag} ({package}, pid {pid}) after {idle_ms}ms \
                 idle ({} window session(s))",
                record["window_sessions"]
            );
            self.records.push(record.clone());
            self.kills.push(record.clone());
            killed.push(record);
        }
        killed
    }

    /// Reap a process that ended by itself and record why (a secondary guest
    /// that crashed must be visible, never mistaken for a running one).
    fn reap(&mut self) -> Vec<serde_json::Value> {
        let mut exited = Vec::new();
        let mut gone = Vec::new();
        for (tag, guest) in &mut self.guests {
            match guest.child.try_wait() {
                Ok(Some(status)) => {
                    let stderr = guest.stderr.lock().map(|b| b.to_text()).unwrap_or_default();
                    let record = serde_json::json!({
                        "tag": tag,
                        "package": guest.spec.package,
                        "pid": guest.pid,
                        "status": status.to_string(),
                        "stderr_tail": tail(&stderr, 800),
                    });
                    eprintln!(
                        "acos-art: launch: secondary process tag={tag} ({}) exited: {status}",
                        guest.spec.package
                    );
                    exited.push(record);
                    gone.push(*tag);
                }
                _ => {}
            }
        }
        // MPROC7: a reaped guest is gone; the launcher (which waited for it)
        // drops it and its `ProcessRecord`, so the next intent for its package
        // is a real spawn even if the peer has not seen the socket close yet.
        for tag in &gone {
            self.guests.remove(tag);
            self.atms
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .process_died(*tag);
        }
        exited
    }

    /// The `processes` control module's body.
    fn to_json(&mut self) -> serde_json::Value {
        let guests: Vec<serde_json::Value> = self
            .guests
            .iter_mut()
            .map(|(tag, guest)| {
                serde_json::json!({
                    "tag": tag,
                    "package": guest.spec.package,
                    "pid": guest.pid,
                    "uid": guest.spec.uid,
                    "kind": match guest.spec.kind {
                        SpawnKind::Activity => "activity",
                        SpawnKind::Service => "service",
                    },
                    "activity": guest.spec.activity,
                    "service": guest.spec.service,
                    "alive": matches!(guest.child.try_wait(), Ok(None)),
                })
            })
            .collect();
        serde_json::json!({
            "processes": guests,
            "requests": self.records,
            "uninstalled": self.uninstalled,
            "failures": self.failures,
            // MPROC5c: what the idle kill took, and the rule it applied.
            "killed": self.kills,
        })
    }

    /// Kill every secondary process (session end). The scope kill is what makes
    /// this bounded: the guest's whole group goes, never one pid.
    fn shutdown(&mut self) {
        for (tag, mut guest) in std::mem::take(&mut self.guests) {
            guest.cgroup.kill();
            let _ = guest.child.kill();
            let _ = guest.child.wait();
            eprintln!("acos-art: launch: secondary process tag={tag} stopped (session end)");
        }
    }
}

/// The `ProviderInfoList` a bind request carries for one package (K3c), from
/// that package's own parsed manifest.
fn provider_spec_of(state: &acos_services::pms::PmsState) -> String {
    let mut seen: Vec<(&str, Vec<&str>)> = Vec::new();
    for p in state.providers() {
        if p.process_name != state.app().process_name {
            continue;
        }
        match seen.iter_mut().find(|(class, _)| *class == p.name) {
            Some((_, authorities)) => authorities.push(&p.authority),
            None => seen.push((&p.name, vec![&p.authority])),
        }
    }
    seen.iter()
        .map(|(class, authorities)| {
            let declared = state.providers().iter().find(|p| p.name == *class);
            let read = declared.map_or("", |p| p.read_permission.as_str());
            let exported = declared.is_some_and(|p| p.exported);
            let grant_uri = declared.is_some_and(|p| p.grant_uri_permissions);
            let meta = declared.map_or_else(String::new, |p| meta_spec(&p.meta_data));
            format!(
                "{class}|{}|{read}|{}|{}|{meta}",
                authorities.join(";"),
                u8::from(exported),
                u8::from(grant_uri)
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The per-session half of [`launch`].///
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
    let session_dir: Option<PathBuf> = std::env::var_os("ACOS_ZYGOTE_SESSION").map(PathBuf::from);
    // ZPROD3b: the demand-probe channel of a zygote child. The keeper's stdin
    // is `/dev/null` (`PersistentZygote::start`), so a `PROBE` wake written to
    // this launcher's stub stdin could never reach the forked app: measured as
    // `capture: strict settle timed out (probe-floor)` right after a tap (the
    // last event-driven sample stayed the newest, `tree_age_ms` ~1.6 s). The
    // guest reads `PROBE` from fd 0, so the session publishes
    // `<session>/probe.fifo` here — before `<session>/argv`, i.e. before the
    // host can send `spawn <tag>` — and `ZygoteApp.z4ForkOne` opens it and
    // `System.setIn`s it before its namespace/fork chain. Opened read-write
    // like the keeper's `cmd` FIFO: neither side blocks on open and the child
    // never sees a spurious EOF when this launcher restarts.
    let probe_fifo = match &session_dir {
        Some(dir) => Some(open_guest_probe_fifo(dir)?),
        None => None,
    };
    // Private per-run data root (S4): app data, out staging and the importer
    // socket are this run's; everything else under /data is read-only.
    let run = match &session_dir {
        Some(d) => root.run_dir_at(&d.join("run"), "launch")?,
        None => root.new_run_dir("launch")?,
    };
    // ADR-0029: the attached module set decides what this session installs and
    // which host endpoints it owns, so the specs are parsed **before** anything
    // is staged or bound. `opts.services` is the launch's `services:` block; a
    // frozen host template passes the same set in `ACOS_ART_SERVICES`.
    let mut module_specs: Vec<acos_services::module::ModuleSpec> =
        acos_services::module::ModuleSpec::parse_list(&serde_json::Value::Array(
            opts.services.clone(),
        ))
        .map_err(|error| Error::Layout(format!("services: {error}")))?;
    if module_specs.is_empty() {
        if let Ok(raw) = std::env::var("ACOS_ART_SERVICES") {
            let value: serde_json::Value = serde_json::from_str(&raw).map_err(|error| {
                Error::Layout(format!("ACOS_ART_SERVICES is not JSON: {error}"))
            })?;
            module_specs = acos_services::module::ModuleSpec::parse_list(&value)
                .map_err(|error| Error::Layout(format!("ACOS_ART_SERVICES: {error}")))?;
        }
    }
    let inherited_keystore = std::env::var(acos_services::modules::keystore::KEYSTORE_ENV).ok();
    reconcile_inherited_keystore_module(&mut module_specs, inherited_keystore.as_deref());
    // WV8: the `webview` module owns the session's WebView socket and the
    // provider package. With it `off` (the default) neither exists, which is
    // Android's own "no WebView provider" path: no `com.android.webview` in the
    // package table, `hasSystemFeature("android.software.webview")` false, no
    // socket a `WebView` could connect to. With it `real` the launcher stages
    // the provider APK (below) and binds this session's gate socket; the shared
    // broker still starts only on the guest's first connect.
    let webview_real = acos_services::modules::webview::broker_requested(&module_specs);
    // WV4: this session's WebView socket directory (`wv/`, mounted at
    // `/run/acos` by the sandbox). Resolved and length-checked before the guest
    // exists: a `sun_path` overflow must read as a launch error naming the
    // session directory, not as an unattributable `connect()` failure in the
    // WebView.
    let webview_socket_dir = crate::webview_socket::host_dir(run.path())?;
    crate::webview_socket::check_sun_path(&webview_socket_dir)?;
    // WV5b: serve this session's socket **lazily**. `start_gate` binds
    // `<dir>/webview.sock` and starts one sleeping accept thread; the shared
    // broker (and Chromium itself) exists only after the guest connects, which
    // the provider does when it creates a WebView. A session without a WebView
    // pays a socket node and a thread — measured, not assumed
    // (`results/handoff/WV5b/REPORT.md` §3). WV8: and a session without the
    // module pays nothing at all: the gate is the module's.
    let webview_gate = if webview_real {
        crate::webview_socket::start_gate(&webview_socket_dir, run.path())?
    } else {
        None
    };
    let webview_broker_socket =
        webview_real.then(|| crate::webview_socket::broker_socket(&webview_socket_dir));
    crate::stage("run-dir");

    // H17: the APK is an input every guest maps (ART mmaps the dex/resources), so
    // it is staged into this session's private run root and bound over its guest
    // path. Writing the shared `<data>/app/<name>` on every launch truncated the
    // bytes other sessions' guests held mapped (SIGBUS) and let a same-named APK
    // from one session reach another.
    let apk_name = opts
        .apk
        .file_name()
        .map(|f| f.to_string_lossy().to_string())
        .unwrap_or_else(|| "app.apk".to_string());
    let apk = run.dir("app").join(&apk_name);
    crate::app::stage_apk(&opts.apk, &apk)?;
    crate::app::ensure_apk_mountpoint(&root.data, &apk_name, &opts.apk)?;
    // The same file as the guest sees it (H17 binds the per-run copy
    // read-only at `/data/app/<name>`).
    let guest_apk = format!("/data/app/{apk_name}");
    let package = crate::app::apk_package(&opts.apk)?;
    crate::stage("apk-stage");

    // MPROC1: extra APKs (`--extra-apk`, repeatable) are installed into this
    // session beside the launched one. Each gets the same treatment the primary
    // gets: a private staged copy in this run's tree (H17: never the shared
    // `/data/app` entry), a bind over its guest path, the native-library
    // install and a structured ABI gate. The package manager and the activity
    // manager learn about them below, before any guest exists.
    let mut extras: Vec<ExtraApk> = Vec::new();
    // WV8: a module's `real` profile may install a system app the session cannot
    // work without (the `webview` provider package). The manifest names it
    // (`system_apps`) and `module::system_apks` resolves the host file, so a
    // scenario needs `services: [{id: webview, profile: real}]` and not a second
    // `extra_apks:` line that could drift from the attached set.
    let extra_apk_paths: Vec<PathBuf> = opts
        .extra_apks
        .iter()
        .cloned()
        .chain(acos_services::module::system_apks(&module_specs))
        .collect();
    for extra in &extra_apk_paths {
        let name = extra
            .file_name()
            .map(|f| f.to_string_lossy().to_string())
            .filter(|n| !n.is_empty())
            .ok_or_else(|| {
                Error::Layout(format!(
                    "extra APK {}: the path has no file name",
                    extra.display()
                ))
            })?;
        if name == apk_name {
            return Err(Error::Layout(format!(
                "extra APK {}: the launched APK is already staged as {name}",
                extra.display()
            )));
        }
        if extras
            .iter()
            .any(|e| e.guest_path == format!("/data/app/{name}"))
        {
            return Err(Error::Layout(format!(
                "extra APK {}: another APK is already staged as {name}",
                extra.display()
            )));
        }
        let extra_package = crate::app::apk_package(extra)?;
        if extra_package == package {
            return Err(Error::Layout(format!(
                "extra APK {}: package {extra_package} is the launched package",
                extra.display()
            )));
        }
        if extras.iter().any(|e| e.package == extra_package) {
            return Err(Error::Layout(format!(
                "extra APK {}: package {extra_package} is already installed by another APK in this session",
                extra.display()
            )));
        }
        let staged = run.dir("app").join(&name);
        crate::app::stage_apk(extra, &staged)?;
        crate::app::ensure_apk_mountpoint(&root.data, &name, extra)?;
        let guest_lib_root = format!("/data/app-lib-extra/{extra_package}");
        // An APK without native code still needs its `nativeLibraryDir` to
        // exist (the primary's tree is created by the native-lib install for
        // the same reason); bwrap refuses a bind whose source is missing.
        let lib_root = run.dir("app-lib-extra").join(&extra_package);
        fs::create_dir_all(&lib_root)?;
        extras.push(ExtraApk {
            staged,
            guest_path: format!("/data/app/{name}"),
            guest_lib_dir: format!("{guest_lib_root}/{}", acos_services::native_libs::HOST_ABI),
            guest_lib_root,
            lib_root,
            package: extra_package,
            uid: None,
        });
    }
    crate::stage("extra-apk-stage");

    // W17: installd's native-library step. Stock Android extracts the APK's
    // `lib/<abi>/*.so` into `ApplicationInfo.nativeLibraryDir` at install time
    // and `LoadedApk` puts that directory on the class loader's native search
    // path. ACOS stages the APK read-only and never ran that step, so
    // `dlopen` saw `library "libhellojni.so" not found`. Extract into this
    // run's own tree (bound read-only at `/data/app-lib`).
    let native_lib_root = run.dir("app-lib");
    // PERF-REL3c: the launch opens its source APK once. The native-library install,
    // the package state (`installed`) and the manifest facts (budget, launcher,
    // labels, config changes) all read that one open. The facts are cached
    // (`prime_from`) and the open is dropped at the end of this block, so only the
    // small facts outlive it.
    let (native_libs, installed) = {
        let source = acos_services::apk_facts::open_shared(&opts.apk)
            .map_err(|e| Error::Layout(format!("native libs: open {}: {e}", opts.apk.display())))?;
        let native_libs =
            acos_services::native_libs::install_from(&source, &opts.apk, &native_lib_root)
                .map_err(|e| Error::Layout(format!("native libs: {e}")))?;
        acos_services::apk_facts::prime_from(&opts.apk, &source);
        let installed = acos_services::pms::PmsState::from_opened(&opts.apk, &source).ok();
        (native_libs, installed)
    };
    match &native_libs {
        acos_services::native_libs::NativeLibInstall::Installed { libs, .. } => {
            eprintln!(
                "acos-art: launch: native libs installed={} dir={} guest={}",
                libs.len(),
                native_lib_root.display(),
                acos_services::native_libs::GUEST_NATIVE_LIB_DIR
            );
        }
        acos_services::native_libs::NativeLibInstall::UnsupportedAbi {
            available,
            sample,
            message,
        } => {
            // ABIGATE/W59-A: AOSP's `PackageManager` refuses a package whose
            // native code has no directory for a device ABI
            // (`INSTALL_FAILED_NO_MATCHING_ABIS`), and so must this launcher.
            // The pre-fix path logged this and started the guest anyway, so the
            // caller saw `GUEST_EXITED` (the app's own
            // `UnsatisfiedLinkError` / a boot failure) instead of the
            // documented `UNSUPPORTED_ABI`. The gate is package-level, decided
            // before the image, the props, logd or any guest process exists.
            eprintln!("acos-art: launch: UNSUPPORTED_ABI available={available:?}: {message}");
            let result = abi_gate_result(opts, &package, available, sample);
            let out = opts
                .out
                .clone()
                .unwrap_or_else(|| run.dir("out").join("launch"));
            write_session_exit(&out, &result, None, None, None, None);
            // An interactive caller waits for a *ready line*: refusing without
            // one would look like a dead session (`INTERNAL`) to it, and the
            // code it reads is the whole point of the gate.
            if std::env::var_os("ACOSLAUNCH_INTERACTIVE").is_some() {
                protocol_write(&serde_json::json!({
                    "ok": false,
                    "cmd": "ready",
                    "code": "UNSUPPORTED_ABI",
                    "retryable": false,
                    "error": message,
                    "apk_abis": available,
                    "host_abi": acos_services::native_libs::HOST_ABI,
                    "libs_sample": sample,
                }));
            }
            return Ok(result);
        }
        acos_services::native_libs::NativeLibInstall::None => {}
    }
    crate::stage("native-libs");
    // MPROC1: the same installd step and ABI gate for every extra APK. The
    // libraries land in a per-package tree (`/data/app-lib-extra/<package>`)
    // so two packages that ship a library of the same name can never shadow
    // each other, and the package's own `nativeLibraryDir` names it below.
    for extra in &mut extras {
        let install = acos_services::native_libs::install(&extra.staged, &extra.lib_root)
            .map_err(|e| Error::Layout(format!("native libs for {}: {e}", extra.package)))?;
        match install {
            acos_services::native_libs::NativeLibInstall::Installed { libs, .. } => {
                eprintln!(
                    "acos-art: launch: extra {} native libs installed={} guest={}",
                    extra.package,
                    libs.len(),
                    extra.guest_lib_dir
                );
            }
            acos_services::native_libs::NativeLibInstall::UnsupportedAbi {
                available,
                sample,
                message,
            } => {
                // AOSP refuses the *package* (`INSTALL_FAILED_NO_MATCHING_ABIS`);
                // a session cannot install this one, so the launch is refused
                // before any guest exists, naming the extra APK's inventory.
                eprintln!(
                    "acos-art: launch: UNSUPPORTED_ABI ({}): {message}",
                    extra.package
                );
                let result = abi_gate_result(opts, &extra.package, &available, &sample);
                let out = opts
                    .out
                    .clone()
                    .unwrap_or_else(|| run.dir("out").join("launch"));
                write_session_exit(&out, &result, None, None, None, None);
                if std::env::var_os("ACOSLAUNCH_INTERACTIVE").is_some() {
                    protocol_write(&serde_json::json!({
                        "ok": false,
                        "cmd": "ready",
                        "code": "UNSUPPORTED_ABI",
                        "retryable": false,
                        "error": message,
                        "extra_apk": extra.package,
                        "apk_abis": available,
                        "host_abi": acos_services::native_libs::HOST_ABI,
                        "libs_sample": sample,
                    }));
                }
                return Ok(result);
            }
            acos_services::native_libs::NativeLibInstall::None => {}
        }
    }
    crate::stage("extra-native-libs");
    // The declared `<application android:name>` class and `android:theme`.
    // Without the class the real `LoadedApk.makeApplication` instantiates
    // `android.app.Application` instead of the APK's own (W15: RN's
    // `SoLoader.init()` lives there, and the guest ran the base class, so the
    // first React class hit `SoLoader.init() not yet called`). The theme is
    // what `PackageParser` resolves into the activity's `ActivityInfo.theme`;
    // without it an AppCompat activity aborts with "You need to use a
    // Theme.AppCompat theme" (W17: rn-hello's next wall after SoLoader).
    // `installed` was read from the launch's one open (above).
    let application_class = installed
        .as_ref()
        .map(|state| state.app().class_name.clone())
        .filter(|name| !name.is_empty());
    let application_theme = installed.as_ref().map_or(0, |state| state.app().theme);

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
    // MPROC1: every extra package gets the same private data root the launched
    // package gets, so its own `dataDir` exists before it runs.
    for extra in &extras {
        fs::create_dir_all(run.dir("app-data").join(&extra.package))?;
    }
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
            write_session_exit(&out, &result, None, None, None, None);
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
    // VIEW1c: the live-view stream socket lives in a **separate, host-only**
    // directory that is never bind-mounted into the guest (`sockets` itself is
    // mounted at `/dev/socket`, so a `view.sock` there would be reachable from
    // the APK under test). Both are 0700 and owned by this uid.
    let view_dir = ShortSocketDir::new_named("view").map_err(Error::Io)?;
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
    // REL3I3: the peer sleeps in `poll(2)` until the guest speaks, a delivery is
    // due, or this wake fires. Producers that queue a `system -> app` delivery
    // outside a peer turn (the in-process IME hiding itself on a key) write to
    // it; so does `stop`, which the receive timeout used to notice after up to
    // a second. One pipe, no idle timer.
    let delivery_wake = Arc::new(acos_services::DeliveryWake::new().map_err(Error::Io)?);
    let peer_wake_fd = delivery_wake.read_fd();
    let unsupported_log = staging.join("unsupported.jsonl");
    let (tx, rx) = mpsc::channel::<PeerSummary>();
    // The peer sleeps in `poll(2)` until a guest frame or the earliest module
    // deadline; this pipe is how the launcher's stop reaches that sleep.
    let peer_wakeup = PeerWakeup::new().map_err(Error::Io)?;
    let control_wake_fd = peer_wakeup.read;
    let peer_log = unsupported_log.clone();
    let apk_for_peer = opts.apk.clone();
    // W10: the live session drives the same `InputHost` the window session
    // uses and captures C1's stored buffer, so the standard install hands both
    // back. Building the host here (instead of inside the peer thread) keeps
    // those handles in the launcher without a second install.
    // Reuse the reconciled `module_specs` parsed above (including any legacy
    // `ACOS_ART_KEYSTORE` promotion) for attachment, reporting, and guest env.
    let mut session_modules = match prebuilt {
        Some((host, handles)) => {
            acos_services::module::SessionModules::from_installed(host, handles)
        }
        None => acos_services::module::SessionModules::new(),
    };
    session_modules
        .attach(&module_specs)
        .map_err(|error| Error::Layout(format!("services: {error}")))?;
    // WV8: publish the endpoints the launcher bound for the `webview` module.
    // The module's `status` control op answers from these instead of recomputing
    // a path the launcher already decided, so the two cannot name different
    // sockets.
    session_modules
        .host_mut()
        .set_webview_gate(acos_services::host::WebviewGate {
            socket: webview_real
                .then(|| crate::webview_socket::host_socket_path(&webview_socket_dir)),
            broker_socket: webview_broker_socket.clone(),
        });
    // NET5: the resolver proxy is served *after* the modules attach, because the
    // `network` module is what decides whether this session has one (ADR-0030
    // item 4). `sockets.0` is what the sandbox binds at `/dev/socket` (below), so
    // it must be served here. With a session directory (ZPROD) this is exactly
    // `<plan>/<tag>/sock`, the directory the zygote child mounts; without one it
    // is this run's short socket dir. Serving any other directory leaves
    // `/dev/socket/dnsproxyd` invisible to the app and `getaddrinfo` answers
    // `EAI_NODATA` (measured).
    let _dns = crate::spike_net::serve_dnsproxyd_if_enabled(&sockets.0).map_err(Error::Io)?;
    // ADR-0029 item 4: the attached set is published on the ready line and in
    // every `acos_services` reply, so the agent sees what the session actually
    // attached (never what it asked for).
    let session_module_rows = session_modules.verdict_modules();
    // ADR-0029 item 1: what those attached modules need this guest *process* to
    // install before any app code runs (collected before the host moves to the
    // peer thread, which owns the registry).
    let session_process_initializers = session_modules.process_initializers();
    let live_handles = {
        // ZM2 prototype C: a host-template child starts from the template's
        // empty (fully installed) service graph, which is CoW-shared until the
        // child writes it. The APK install below is per session either way.
        //
        // ADR-0029 item 4: the modules were attached above, before the guest
        // boots; the host and the registry now move to the peer thread, which
        // is the only thread that mutates the service table (live attach/detach
        // is a request to it, never a second mutation path).
        let handles = session_modules
            .take_session_handles()
            .expect("core publishes the session handles at attach");
        let (host, registry) = session_modules.into_parts();
        let mut host = host;
        // MPROC8: this session runs real app processes, so the host performs
        // AOSP's `ActiveServices` work for a guest `bindService` (create/bind in
        // the service's own process, answer the caller's `IServiceConnection`).
        // The in-process runtime keeps its own bind path, so the flag is a
        // property of what this session is, stated by the only component that
        // knows.
        host.set_host_side_services(true);
        // REL3I3: this session's delivery wake. The host hands it to the
        // producers it installs (the WMS's IME insets queue) and the peer's
        // idle wait polls the same pipe. A prebuilt (host-template) child
        // installed its graph before this process existed, so the state is
        // attached here as well; the receiver end is this process's, never the
        // template's.
        host.set_delivery_wake(Arc::clone(&delivery_wake));
        handles
            .wms
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .attach_delivery_wake(Arc::clone(&delivery_wake));
        // SYSUI3A: name the session's primary process (tag 0) the way stock AMS
        // names it in `startProcessLocked`, before any attach. `attachApplication`
        // carries no package and the primary never sends the peer's handshake,
        // so without this `Peer::package_of(0)` had no package to ask
        // `pending_launch_for` with: a `startActivity` into the already-running
        // primary (a notification's content sender) was recorded and never
        // delivered. The launcher is the component that knows what it started.
        if !host.seed_primary_package(&package) {
            eprintln!(
                "acos-art: launch: the activity service did not take the primary package {package}"
            );
        }
        if let Ok(l) = acos_services::manifest_resolve::launcher_activity(&opts.apk) {
            let cls = l.target.unwrap_or(l.component);
            let _ = host.seed_primary_activity(&package, &cls);
        }
        // K3d: the PMS publishes the APK path the **guest** can open
        // (`/data/app/<name>.apk`, read-only bound by the sandbox), not the host
        // file. `PackageManager.getResourcesForApplication` builds `Resources`
        // from `ApplicationInfo.sourceDir`, so a host path made
        // `PackageItemInfo.loadXmlMetaData` fail with
        // `ApkAssets.nativeLoad: Failed to load asset path <host path>` —
        // which is how FileProvider's `FILE_PROVIDER_PATHS` XML lookup died.
        if let Err(e) = host.install_package_from_apk_as(&apk_for_peer, Some(&guest_apk)) {
            eprintln!("acos-art: launch: install_package_from_apk failed: {e}");
        }
        // MPROC1: each extra APK is installed into the same package manager
        // (its own uid, its own `sourceDir`/`nativeLibraryDir`) and into the
        // activity manager's manifest table under that same uid. A failure here
        // fails the launch: a session that silently lacks a package the caller
        // asked for is exactly the fake success the rules forbid.
        for extra in &mut extras {
            let uid = host
                .install_extra_package_from_apk_as(
                    &extra.staged,
                    Some(&extra.guest_path),
                    Some(&extra.guest_lib_dir),
                )
                .map_err(|e| {
                    Error::Layout(format!(
                        "extra APK {}: package manager install failed: {e}",
                        extra.package
                    ))
                })?;
            extra.uid = Some(uid);
            if let Ok(object) = host.ensure("android.app.IActivityTaskManager") {
                if let Some(atms) = object
                    .as_any_mut()
                    .and_then(|any| any.downcast_mut::<acos_services::atms::AtmsService>())
                {
                    if let Err(e) = atms.seed_manifest_from_apk_with_uid(&extra.staged, uid) {
                        return Err(Error::Layout(format!(
                            "extra APK {}: seed_manifest_from_apk: {e}",
                            extra.package
                        )));
                    }
                }
            }
            eprintln!(
                "acos-art: launch: extra package installed {} uid={uid} apk={} native={}",
                extra.package, extra.guest_path, extra.guest_lib_dir
            );
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
        // UID: the uid the package manager allocated for the launched package.
        // AOSP allocates one uid per installed package, and this process must
        // run as **its** uid (a second package in the same session gets its
        // own). `None` only when the package manager never registered the
        // package (then the historical fixture uid stays the default, and a
        // guest that asks for its own `ApplicationInfo` fails loudly on its
        // own).
        let primary_uid = host.package_uid(&package).map(|uid| uid as u32);
        let has_home = host.has_home_activity();
        (host, handles, registry, primary_uid, has_home)
    };
    let (peer_host, live, peer_registry, primary_uid, session_has_home) = live_handles;
    let service_control = Arc::new(ServiceControl::default());
    // A live module request wakes the peer's idle wait (see `ServiceControl`).
    service_control.set_wake(peer_wakeup.write);
    let peer_control = Arc::clone(&service_control);
    let interactive = std::env::var_os("ACOSLAUNCH_INTERACTIVE").is_some();
    // MPROC4 step 3b: the peer thread owns the `ServiceHost`, so it is the one
    // that learns a component needs a process it cannot start itself. The
    // launcher (this thread) owns the processes, so the request crosses over as
    // a plain message and nothing is started from the peer thread.
    let (spawn_tx, spawn_rx) = mpsc::channel::<acos_services::ams::ProcessSpawnRequest>();
    crate::stage("peer-setup");
    // PERF4: the session's settle/observation wake exists before the peer thread
    // because applying a guest transaction is one of the facts a settle waits
    // for: a `Surface` transaction is the C1 store, and the peer loop signals
    // this wake as soon as it has applied one, so a settle never waits for its
    // own poll tick to notice the frame it is binding.
    let settle_change = Arc::new(ProbeWake::default());
    let settle_change_for_peer = Arc::clone(&settle_change);
    let prompts_for_peer = Arc::clone(&live.prompts);
    let peer_thread = std::thread::spawn(move || {
        let summary = (|| -> std::io::Result<PeerSummary> {
            // The real ServiceHost, with its package manager pointed at the APK
            // being launched: `handleBindApplication` asks for its
            // `getApplicationInfo` and must get the real package, not the
            // hello-views fixture (and never a fabricated null).
            let mut peer = Peer::with_host(peer_host, Some(peer_log));
            let mut peer_registry = peer_registry;
            // MPROC2 step 3a + ADR-0029 item 4 in one loop: the session serves N
            // tagged driver connections (one per app process) and the launcher's
            // live module-control channel, both on the thread that owns the
            // service table. A spawn request crosses to the launcher, which owns
            // the processes and their tags.
            // SYSUI1: a control op can start an activity (the permission
            // dialog, the shade). Its resolution may need a process the launcher
            // owns, and no guest frame follows to drain the request, so the pump
            // hands it over the same way the serve loop does.
            let pump_spawns = spawn_tx.clone();
            let mut sink = |request: acos_services::ams::ProcessSpawnRequest| {
                let _ = spawn_tx.send(request);
            };
            let mut pump = |peer: &mut Peer| -> Vec<(u32, Frame)> {
                let mut frames: Vec<(u32, Frame)> = Vec::new();
                while let Some((id, request)) = peer_control.take() {
                    let reply = service_request(peer, &mut peer_registry, &request);
                    let guest_commands = peer.host_mut().drain_guest_commands();
                    let debug_reply = reply.to_string();
                    // Send spawn requests before waking the launcher so a
                    // `replay_intent` or `launch_home` caller can drain them
                    // immediately without racing the peer thread.
                    for spawn in peer.take_spawn_requests() {
                        eprintln!(
                            "acos-binder-peer: control op: {} needs a process for {} ({})",
                            spawn.package, spawn.component, spawn.reason
                        );
                        let _ = pump_spawns.send(spawn);
                    }
                    let reply_for_rollback = (request.get("op").and_then(|v| v.as_str())
                        == Some("attach")
                        && request.get("_hot_extra_apk").is_some())
                    .then(|| reply.clone());
                    if !peer_control.reply(
                        id,
                        ServiceControlReply {
                            reply,
                            guest_commands,
                        },
                    ) {
                        if let Some(ref reply) = reply_for_rollback {
                            rollback_timed_out_hot_attach(
                                peer,
                                &mut peer_registry,
                                &request,
                                reply,
                            );
                        }
                    }
                    // A live `ctl` op can queue `system -> app` deliveries (an
                    // SMS, a call-state event, a location fix, an auth result).
                    // No binder transaction follows to flush them, so do it here
                    // — with the **timed** drain, because a module parks its own
                    // deliveries in its own object.
                    let flushed = peer.tick();
                    if let Ok(path) = std::env::var("ACOS_SERVICE_CTL_LOG") {
                        use std::io::Write as _;
                        if let Ok(mut file) = std::fs::OpenOptions::new()
                            .create(true)
                            .append(true)
                            .open(&path)
                        {
                            let _ = writeln!(
                                file,
                                "ctl reply={debug_reply} flushed={} targets={:?}",
                                flushed.len(),
                                flushed
                                    .iter()
                                    .map(|(tag, f)| (tag, f.ty, f.code, f.target))
                                    .collect::<Vec<_>>()
                            );
                        }
                    }
                    frames.extend(flushed);
                }
                frames
            };
            let last_live_prompt_seq = std::sync::atomic::AtomicU64::new(0);
            let notify_settle = || {
                settle_change_for_peer.notify();
                if let Some(view) = LIVE_VIEW.get() {
                    if view.has_subscribers() {
                        let last = last_live_prompt_seq.load(std::sync::atomic::Ordering::Relaxed);
                        let new_prompts = {
                            let guard = prompts_for_peer
                                .lock()
                                .unwrap_or_else(|e| e.into_inner());
                            if guard.cursor() > last {
                                guard.since(last)
                            } else {
                                Vec::new()
                            }
                        };
                        for prompt in new_prompts {
                            last_live_prompt_seq
                                .store(prompt.seq, std::sync::atomic::Ordering::Relaxed);
                            view.push_event(serde_json::json!({
                                "kind": "capability_prompt",
                                "prompt": prompt.to_json(),
                            }));
                        }
                    }
                }
            };
            let served = acos_binder_peer::serve::serve_with_control(
                &mut peer,
                listener,
                &peer_stop,
                Some(&mut sink),
                Some(control_wake_fd),
                Some(&mut pump),
                Some(peer_wake_fd),
                Some(&notify_settle),
            );
            let outcome = served?;
            eprintln!(
                "acos-art: peer: processes={:?} peak_connections={} accepted={} refused={} \
                 closed={} frames={}/{} unroutable={}",
                peer.tags(),
                outcome.peak_connections,
                outcome.accepted,
                outcome.refused,
                outcome.closed,
                outcome.frames_in,
                outcome.frames_out,
                outcome.unroutable
            );
            let summary = PeerSummary {
                processes: peer
                    .process_list()
                    .into_iter()
                    .map(|(tag, package, uid, pid, app_thread, attached)| {
                        serde_json::json!({
                            "tag": tag,
                            "package": package,
                            "uid": uid,
                            "pid": pid,
                            "app_thread": app_thread,
                            "attached": attached,
                        })
                    })
                    .collect(),
                spawn_requests: outcome.spawns_requested,
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
            Ok(summary)
        })();
        let _ = tx.send(summary.unwrap_or_default());
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
    // LAUNCHEXTRAS: the host builds the launch Intent (ADR-0029 amendment). The
    // bytes come from `Intent.writeToParcel` as this image writes it (the
    // parcelable codec, real-Java differential in acos-runtime); the guest
    // unmarshals them with the framework's own `Intent.CREATOR` and hands the
    // Intent to the real `LaunchActivityItem`. A `Parcelable` extra is refused
    // by name before any guest exists.
    let launch_intent_hex = if opts.intent.is_empty() {
        None
    } else {
        if let Some(detail) = opts.intent.unsupported_detail() {
            return Err(Error::Layout(format!("UNSUPPORTED_ANDROID_API: {detail}")));
        }
        let Some(activity_class) = activity.as_deref() else {
            return Err(Error::Layout(
                "intent_action/data/extras need `--until activity-resumed`: a launch that stops \
                 before an Activity cannot carry an Intent"
                    .to_string(),
            ));
        };
        let bytes = acos_services::client_transaction::encode_launch_intent(
            &package,
            activity_class,
            &opts.intent,
        )
        .map_err(|error| Error::Layout(format!("launch Intent: {error}")))?;
        eprintln!(
            "acos-art: launch: host-built launch Intent action={} data={} extras={} bytes={}",
            opts.intent.action.as_deref().unwrap_or("android.intent.action.MAIN"),
            opts.intent.data.as_deref().unwrap_or("none"),
            opts.intent.extras.len(),
            bytes.len()
        );
        Some(hex_bytes(&bytes))
    };
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
        .map(|state| ((state.app().flags & !FLAG_HARDWARE_ACCELERATED) | FLAG_INSTALLED) as i64)
        .unwrap_or(-1);
    // Diagnostic seam (W31): `ACOSLAUNCH_APP_FLAGS=0x…` (or decimal) lets a run
    // prove a *future* manifest derivation end to end before the host PMS owns
    // it (`crates/acos-apk::manifest::Manifest::application_flags` is unit
    // tested on counter + Gaten). It never invents bits: the value is logged
    // and recorded in `application-created.marker` as `app_flags=…`.
    let mut app_flags_source = if installed.is_some() {
        "manifest"
    } else {
        "default"
    };
    if let Ok(raw) = std::env::var("ACOSLAUNCH_APP_FLAGS") {
        let text = raw.trim();
        let parsed = if let Some(hex) = text.strip_prefix("0x").or_else(|| text.strip_prefix("0X"))
        {
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
    // H13/B3: the boot-image source is resolved before the sandbox is wired so
    // a bad knob fails the launch instead of silently running another image.
    // H13b: the default (`shared-fixed`) is served from a content-addressed
    // on-disk cache; `report` is the reply field, set even when the default
    // falls back to the sysroot LZ4 image.
    let resolution = crate::boot_image::resolve(root)?;
    let boot_image = resolution.image.clone();
    let boot_image_report = resolution.report.clone();
    eprintln!("acos-art: launch: {}", boot_image_report.line());
    let mut argv: Vec<String> = Vec::new();
    if let Some(image) = &boot_image {
        // `with_image` below ro-binds the prepared directory to /data/img.
        // `-Xnorelocate` keeps the file pages file-backed and shared; for
        // `shared-aslr` the recorded base was chosen per serve cohort.
        argv.push("-Ximage:/data/img/boot.art".into());
        argv.push("-Xnorelocate".into());
        eprintln!("acos-art: launch: {}", image.note());
    }
    // Z7: real ART heap options (OFF by default; empty unless a knob is set).
    let heap_argv = resolve_heap_argv()?;
    if !heap_argv.is_empty() {
        eprintln!(
            "acos-art: launch: ART heap argv {:?} (Z7 real-heap seam)",
            heap_argv
        );
        argv.extend(heap_argv.iter().cloned());
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
        write_zygote_session(
            dir,
            &run,
            &sockets.0,
            &sock,
            &staging,
            &out,
            &package,
            &guest_args,
        )?;
    }
    // MPROC4 step 3b: one `SecondarySpec` per installed extra package. These
    // are the *same* session inputs the primary got (this run's staged APK, the
    // same package manager facts), so a secondary process is a second guest of
    // this session, not a second launch.
    let mut secondary_specs: Vec<SecondarySpec> = Vec::new();
    for (index, extra) in extras.iter().enumerate() {
        let tag = (index as u32) + 1;
        secondary_specs.push(build_secondary_spec(tag, extra, run.path(), &out));
    }
    let keystore_env = resolve_keystore_env(&module_specs, inherited_keystore.as_deref());
    let secondary_shared = SecondaryShared {
        root: root.clone(),
        image: boot_image.as_ref().map(|image| image.dir.clone()),
        substrate: shims.substrate.clone(),
        art_shim: shims.art_shim.clone(),
        driver: shims.driver.clone(),
        gralloc: shims.gralloc.clone(),
        native_lib_root: native_lib_root.clone(),
        sockets: sockets.0.clone(),
        sock: sock.clone(),
        overlays: overlays.guest_arg(),
        resume_ms,
        geometry: [
            ("ACOSLAUNCH_WIDTH".to_string(), opts.width),
            ("ACOSLAUNCH_PANEL_HEIGHT".to_string(), opts.panel_height),
            ("ACOSLAUNCH_DENSITY".to_string(), opts.density_dpi),
            ("ACOSLAUNCH_STATUS_INSET".to_string(), opts.status_inset),
            ("ACOSLAUNCH_NAV_INSET".to_string(), opts.nav_inset),
        ],
        screen_layout: acos_services::wms::REFERENCE_SCREEN_LAYOUT.max(0) as u32,
        animations: opts.animations.as_str(),
        live_probe: interactive,
        session_apks: std::iter::once((apk.clone(), guest_apk.clone()))
            .chain(extras.iter().map(|e| (e.staged.clone(), e.guest_path.clone())))
            .collect(),
        keystore_env: keystore_env.clone(),
    };
    let mut secondary_pool = SecondaryPool::new(
        secondary_specs,
        secondary_shared,
        Arc::clone(&live.atms),
        package.clone(),
        run.path().to_path_buf(),
        out.clone(),
    );
    if !interactive && !secondary_pool.specs.is_empty() {
        eprintln!(
            "acos-art: launch: {} secondary package(s) installed; a non-interactive session has \
             no spawn loop, so no secondary process can be started in it",
            secondary_pool.specs.len()
        );
    }
    crate::seccomp::enable(root)?;
    let mut sb = root.sandbox();
    if let Some(image) = &boot_image {
        sb.with_image(Some(image.dir.clone()));
    }
    sb.run_root(run.path());
    // UID: this guest runs as the launched package's uid (AOSP: one uid per
    // installed package, shared by that package's processes). The secondary
    // processes below carry *their* package's uid the same way, so two processes
    // of two packages in one session have two uids — `Process.myUid()`,
    // `getPackageUid()`, `getNameForUid()` and the per-uid permission answer all
    // agree on both sides of the socket.
    match primary_uid {
        Some(uid) => {
            sb.app_uid(uid);
        }
        None => eprintln!(
            "acos-art: launch: the package manager has no uid for {package}; the guest keeps the \
             session default (per-package identity is unavailable)"
        ),
    }
    // NET3/NET4: the relay session is named after the session id the engine
    // published (`ACOS_ART_SESSION_ID`) — for an `acos serve` session and for a
    // one-shot `verify` run alike — so the live session API's `acos_net` controls
    // address the session by the id its own client holds.
    if let Ok(id) = std::env::var(crate::spike_net::SESSION_ID_ENV) {
        if !id.is_empty() {
            sb.net_session(Some(&id));
        }
    }
    // Z4: a foreign-guest session does not spawn ART itself, so the guest
    // seccomp/Landlock guard is applied per forked child (the child loads the
    // same `libacos_seccomp_guest.so` after its bind mounts, exactly AOSP's
    // order). The production path keeps the preload-at-start ordering.
    // ZPROD1: a zygote host is a process *without* app code; the guard belongs to
    // every forked child (which loads it after its mounts — AOSP order), so the
    // zygote itself must not carry the guest seccomp/Landlock preload either: a
    // Landlock ruleset in the pre-fork process would deny the child's writes to
    // its own contract directory (`/sessions/<tag>`, measured: EACCES). With
    // the zygote seam unset this expression is exactly `session_dir.is_none()`.
    sb.seccomp_guest(session_dir.is_none() && zygote_app.is_none());
    sb.with_shim(false);
    sb.ro_bind(&shims.art_shim, "/data/shim/libartshim.so");
    // The per-run extracted native libraries (installd's `nativeLibraryDir`).
    sb.ro_bind(
        &native_lib_root,
        acos_services::native_libs::GUEST_NATIVE_LIB_ROOT,
    );
    sb.ro_bind(&shims.substrate, "/data/app/libacosart.so");
    // H17: this session's own APK shadows the shared `/data/app` entry.
    sb.ro_bind(&apk, &format!("/data/app/{apk_name}"));
    // MPROC1: each extra package's APK and native-library tree, at the guest
    // paths the package manager publishes for it.
    for extra in &extras {
        sb.ro_bind(&extra.staged, &extra.guest_path);
        sb.ro_bind(&extra.lib_root, &extra.guest_lib_root);
    }
    sb.ro_bind(&shims.gralloc, "/data/libacoshw.so");
    sb.ro_bind(&shims.driver, "/data/lib/libacos_binder_driver.so");
    if let Some(dir) = &zygote_app {
        // Z3 research guest dex + the raw-syscall fork shim (Z1/Z2). bwrap
        // creates no mount point inside the read-only /data tree, so the
        // guest-visible directory is seeded in the data root (like
        // `app/launch-guest` above) and the built dex is bound over it.
        fs::create_dir_all(root.data.join("app/z3"))?;
        fs::create_dir_all(root.data.join("shim"))?;
        fs::copy(
            dir.join("lib/libz2fork.so"),
            root.data.join("shim/libz2fork.so"),
        )?;
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
        // ZPROD1: the persistent zygote. `spawn`/`kill`/`exit` arrive on
        // `<plan>/cmd`, so the zygote outlives one session's reset instead of
        // forking a fixed cohort once (Z7 §7 item 2: the cohort restart was the
        // whole throughput loss). Off unless the caller asks for it.
        if std::env::var_os("ACOS_ZYGOTE_PERSIST").is_some() {
            sb.env("ACOS_ZYGOTE_PERSIST", "1");
        }
    }
    if session_dir.is_some() || zygote_app.is_some() {
        // ZS/Z4: forward the research security-probe switch into the child.
        if std::env::var_os("ACOS_ZYGOTE_PROBE").is_some() {
            sb.env("ACOS_ZYGOTE_PROBE", "1");
        }
        // The child loads the production guard itself; it must see the blob.
        // ZPROD1: a zygote host keeps no session of its own, but its forked
        // children still need the blob and the loadable `.so`.
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
    // MPROC2 step 3a: the primary app process names itself in the driver's
    // connection handshake, so the peer can put its tag on every binder handle
    // it imports. Secondary processes get their own tag from the spawn path
    // (`spawn <tag>`), never from this env.
    sb.env("ACOS_PROCESS_TAG", "0");
    sb.env("ACOS_PROCESS_NAME", &package);
    sb.env("ACOSLAUNCH_RESUME_MS", &resume_ms.to_string());
    // LAUNCHEXTRAS: the host-built Intent bytes, unmarshalled by the guest's
    // first `LaunchActivityItem` (never rebuilt there from loose fields).
    if let Some(hex) = &launch_intent_hex {
        sb.env("ACOSLAUNCH_INTENT_BYTES", hex);
    }
    // ADR-0029 item 1: the framework initializers the attached modules declare
    // (`ModuleManifest::process_initializers`). On a device the mainline
    // module's own process installs them before any app starts; ACOS has no such
    // process, so the guest runs the same call once, before the app boots. The
    // list is the attached set's own promise (never a guess, never a per-module
    // switch here): `class#method#manager`, ';'-separated.
    if !session_process_initializers.is_empty() {
        let wire = session_process_initializers
            .iter()
            .map(acos_services::module::ProcessInitializer::wire)
            .collect::<Vec<_>>()
            .join(";");
        sb.env("ACOSLAUNCH_INITIALIZERS", &wire);
    }
    // Display geometry the guest boots at. Forwarded only when the caller set
    // it (`acos-art launch --width …`, or the engine's `ACOS_ART_*`), so the
    // default guest is unchanged and the reference-device baselines keep
    // their exact numbers. `ACOSLAUNCH_SCREEN_LAYOUT` below stays the host's
    // WMS value.
    let geometry = [
        ("ACOSLAUNCH_WIDTH", opts.width),
        ("ACOSLAUNCH_PANEL_HEIGHT", opts.panel_height),
        ("ACOSLAUNCH_DENSITY", opts.density_dpi),
        ("ACOSLAUNCH_STATUS_INSET", opts.status_inset),
        ("ACOSLAUNCH_NAV_INSET", opts.nav_inset),
    ];
    sb.env("ACOSLAUNCH_WM_INSETS", &opts.insets);
    let defaults = (1080u32, 2400u32, 420u32, 128u32, 63u32);
    let current = (
        opts.width,
        opts.panel_height,
        opts.density_dpi,
        opts.status_inset,
        opts.nav_inset,
    );
    if current != defaults {
        eprintln!(
            "acos-art: launch: display {}x{}@{} status={} nav={} (caller geometry)",
            opts.width, opts.panel_height, opts.density_dpi, opts.status_inset, opts.nav_inset
        );
    }
    for (var, value) in geometry {
        sb.env(var, &value.to_string());
    }
    // W47: the guest's bind/launch Configuration must carry the same
    // screen-layout size/long bits as the WMS `MergedConfiguration`.
    // Without them (`screenLayout` left at 0) `ResTable_config` selected the
    // `values-xlarge` dimens (dialog_min_width_minor 72% instead of 95%), so a
    // WRAP_CONTENT dialog measured 776 px instead of the stock 1024 px.
    sb.env(
        "ACOSLAUNCH_SCREEN_LAYOUT",
        &acos_services::wms::REFERENCE_SCREEN_LAYOUT.to_string(),
    );
    // ROT2: the Activity's `android:configChanges` bitmask, resolved from the
    // binary manifest the way `PackageParser.getActivityConfigChanges` builds it
    // (declared bits + the forced `CONFIG_MCC|CONFIG_MNC`). `LaunchMain` puts it
    // on the `ActivityInfo` it hands `Activity.attach`, so
    // `ActivityInfo.configChanges` is the manifest's value instead of a
    // hardcoded 0 — that field is what `ActivityRecord
    // .getRealConfigChanged()` compares when a rotation has to decide between a
    // re-create (`ActivityRelaunchItem`) and `onConfigurationChanged`.
    // PERF-REL3c: read from the source APK (`opts.apk`), not the staged copy
    // above. The staged file is a byte copy of it made earlier in this launch, so
    // the facts are the same, and the cached facts answer without another open of
    // a fresh per-run path (a cache miss that opened the APK on every launch).
    let config_changes = acos_services::manifest_resolve::launcher_activity(&opts.apk)
        .map(|l| l.config_changes)
        .unwrap_or(0);
    sb.env("ACOSLAUNCH_CONFIG_CHANGES", &config_changes.to_string());
    if std::env::var_os("E3G_DEBUG").is_some() {
        sb.env("E3G_DEBUG", "1");
    }
    // K3c: the app's own `<provider>` declarations for the bind request's
    // `ProviderInfoList`. A real `AMS.generateApplicationProvidersLocked`
    // queries `queryContentProviders(processName, uid, …)`, so a provider whose
    // `android:process` names another process is *not* in this process's list;
    // the same filter is applied here from the APK's parsed manifest (the host
    // `PackageParser` equivalent, `pms::PmsState`). The framework's
    // `ActivityThread.installContentProviders` then creates them, which is the
    // stock order (after `attachBaseContext`, before `Application.onCreate`).
    //
    // K3d: the last field is the provider's own `<meta-data>`, so the
    // `ProviderInfo` the framework hands to `ContentProvider.attachInfo` carries
    // the `Bundle` its manifest declares (the same values PMS answers with
    // `GET_META_DATA`). Format: `name=type+value` entries joined with `;`,
    // `|`, `;`, `=` and newlines escaped by [`meta_spec_escape`]; types are
    // `s`tring, `i`nt (also a resource reference id), `b`ool, `f`loat.
    //
    // Fidelity note: AMS builds this list with
    // `queryContentProviders(process, uid, STOCK_PM_FLAGS | GET_PROVIDERS)`,
    // which does *not* ask for metadata, so a device's bind `ProviderInfo` has
    // `metaData == null` and providers read their metadata back through
    // `Context.getPackageManager()` (which is what `FileProvider`,
    // `androidx.startup` and Sentry do, and what PMS served above). ACOS sends
    // the declared entries here as well: it only ever exposes what the app's own
    // manifest says, and it keeps `ProviderInfo.metaData` truthful for a
    // provider that reads it from `attachInfo`.
    let provider_spec = installed.as_ref().map_or_else(String::new, |state| {
        let mut seen: Vec<(&str, Vec<&str>)> = Vec::new();
        for p in state.providers() {
            if p.process_name != state.app().process_name {
                continue;
            }
            match seen.iter_mut().find(|(class, _)| *class == p.name) {
                // One `ProviderInfo` per `<provider>`, authorities joined with
                // `;` exactly as `PackageParser` writes them.
                Some((_, authorities)) => authorities.push(&p.authority),
                None => seen.push((&p.name, vec![&p.authority])),
            }
        }
        seen.iter()
            .map(|(class, authorities)| {
                let declared = state.providers().iter().find(|p| p.name == *class);
                let read = declared.map_or("", |p| p.read_permission.as_str());
                let exported = declared.is_some_and(|p| p.exported);
                let grant_uri = declared.is_some_and(|p| p.grant_uri_permissions);
                let meta = declared.map_or_else(String::new, |p| meta_spec(&p.meta_data));
                format!(
                    "{class}|{}|{read}|{}|{}|{meta}",
                    authorities.join(";"),
                    u8::from(exported),
                    u8::from(grant_uri)
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    });
    // K3 shipped this OFF because two gaps downstream of the delivery made it a
    // net regression. Both are fixed, so delivery is the default (K4
    // results/compat/K4.md):
    //   1. K3d (E5.8): manifest `<meta-data>` is parsed and served as the
    //      `PackageItemInfo.metaData` Bundle under `GET_META_DATA`
    //      (`FileProvider`'s `android.support.FILE_PROVIDER_PATHS`,
    //      androidx.startup's initializer list, Sentry's `io.sentry.*`), and the
    //      PMS publishes the APK path the guest can open so
    //      `getResourcesForApplication`/`loadXmlMetaData` work;
    //   2. K3e (E5.9): the userspace binder driver no longer splits a `BR_*`
    //      command when it crosses libbinder's 256-byte read buffer, which is
    //      what made `publishContentProviders` with >=7 provider binders die
    //      with `BinderProxy.transactNative` "Unknown error".
    // Measured on the 42-app corpus: ON is >= OFF at every rung with zero
    // per-app regressions (first frame 17 vs 14, tap 9 vs 8).
    // `ACOS_ART_APP_PROVIDERS=off` restores K3's behaviour for A/B.
    let app_providers_on = std::env::var("ACOS_ART_APP_PROVIDERS")
        .map(|v| !matches!(v.trim(), "0" | "off" | "false" | "no"))
        .unwrap_or(true);
    if !provider_spec.is_empty() {
        if app_providers_on {
            eprintln!(
                "acos-art: launch: app providers {} (bind ProviderInfoList; framework installContentProviders)",
                provider_spec.lines().count()
            );
            sb.env("ACOSLAUNCH_PROVIDERS", &provider_spec);
        } else {
            eprintln!(
                "acos-art: launch: app providers {} NOT delivered (ACOS_ART_APP_PROVIDERS is off; K3 §2)",
                provider_spec.lines().count()
            );
        }
    }
    // K3d: the `<application>`'s own `<meta-data>`, delivered on the bind
    // request's `ApplicationInfo` (`LoadedApk.makeApplication` hands that object
    // to the app, and `context.getApplicationInfo().metaData` reads it).
    if let Some(state) = &installed {
        if !state.app().meta_data.is_empty() {
            eprintln!(
                "acos-art: launch: application meta-data {} (bind ApplicationInfo.metaData)",
                state.app().meta_data.len()
            );
            sb.env("ACOSLAUNCH_APP_META", &meta_spec(&state.app().meta_data));
        }
    }
    if std::env::var_os("ACOSLAUNCH_PROBE").is_some() {
        sb.env("ACOSLAUNCH_PROBE", "1");
    }
    // R2 W6: forward the geometry trace switch (DisplayMetrics, Configuration,
    // WindowMetrics, decor size, ViewRootImpl frame) to the guest. Read-only;
    // the guest writes `<run>/geometry-trace.txt` and prints `ACOSGEO` lines.
    if std::env::var_os("ACOSLAUNCH_GEOM_TRACE").is_some() {
        sb.env("ACOSLAUNCH_GEOM_TRACE", "1");
    }
    // R3: the deliberate-crash seam (`segv`/`abort`/`thread`) and the crash
    // dump's off switch. Diagnostic only; nothing else reads either name.
    if let Ok(mode) = std::env::var("ACOSLAUNCH_CRASH_TEST") {
        sb.env("ACOSLAUNCH_CRASH_TEST", &mode);
    }
    // CR1FIX2 #1: diagnostic seam, the decor reports no attached root.
    if std::env::var_os("ACOSLAUNCH_TEST_WINDOW_NO_ROOT").is_some() {
        sb.env("ACOSLAUNCH_TEST_WINDOW_NO_ROOT", "1");
    }
    // CR1FIX2 #3: test-only seam, a splash-style fixture waits this long between
    // `super.onCreate()` and `finish()`. Unset in normal runs (no delay).
    if let Ok(v) = std::env::var("ACOS_TEST_FINISH_DELAY_MS") {
        sb.env("ACOS_TEST_FINISH_DELAY_MS", &v);
    }
    if let Ok(v) = std::env::var("ACOSART_CRASH_DUMP") {
        sb.env("ACOSART_CRASH_DUMP", &v);
    }
    // G3d: libc's GWP-ASan sampling allocator is off for guests by default. Its
    // sampled backtrace goes through bionic's `android_unsafe_frame_pointer_chase`,
    // which reads past the top stack page on a frame-pointer-free guest stack
    // (SIGSEGV SEGV_ACCERR inside libc, R3/G3d). Off unless asked; see
    // `gwp_asan_guest_env`.
    for (key, value) in crate::gwp_asan_guest_env(
        |k| std::env::var(k).ok(),
        std::env::var("ACOS_ART_GWP_ASAN").ok().as_deref(),
    ) {
        sb.env(&key, &value);
    }
    // H2: the guest-side post-first-frame release hook (ON by default; the
    // guest treats `ACOS_ART_EARLY_GC=0` as off). Forward only the host value
    // so an unset variable means "default".
    for knob in [ENV_ART_EARLY_GC, ENV_ART_GC_DELAY_MS] {
        if let Ok(v) = std::env::var(knob) {
            sb.env(knob, &v);
        }
    }
    // H15 diagnostic: name the ANIMATION-queue callbacks that keep drawing after
    // the first frame (`ACOSLAUNCH TRACE` cannot show them: they run inside
    // Choreographer.doFrame, not as handler messages). Forwarded only when the
    // caller set it, so the default guest payload is unchanged.
    if std::env::var_os("P10_PROBE_ANIM_TRACE").is_some() {
        sb.env("P10_PROBE_ANIM_TRACE", "1");
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
    // P12 (owner decision 2026-11-12): strict settle is **event-only**. The
    // P11 `ACOS_STRICT_SETTLE=quiet` escape hatch is deleted, so the variable is
    // not forwarded and has no effect; a caller that still sets it is told once.
    if std::env::var_os("ACOS_STRICT_SETTLE").is_some() {
        static WARNED: std::sync::Once = std::sync::Once::new();
        WARNED.call_once(|| {
            eprintln!(
                "acos-art: ACOS_STRICT_SETTLE is ignored: strict settle is event-only since P12"
            );
        });
    }
    if let Ok(value) = std::env::var("ACOS_STRICT_SETTLE_HORIZON_MS") {
        sb.env("ACOS_STRICT_SETTLE_HORIZON_MS", &value);
    }
    // P12: the per-session animation choice is a launch-time property. The
    // guest applies it with `ValueAnimator.setDurationScale` and the host WMS
    // reports the same value from `getCurrentAnimatorScale`; both are the real
    // framework's own two delivery points for `animator_duration_scale`.
    // `transition_animation_scale`/`window_animation_scale` are WMS-side on a
    // device; ACOS's WMS runs no transition animations, so there is nothing to
    // scale beyond the app-side animator scale.
    sb.env("ACOS_ANIMATIONS", opts.animations.as_str());
    // K5 / ADR-0029 `security.keystore`: `real` enables the software
    // AndroidKeyStore JCA provider (`ACOS_ART_KEYSTORE=on`); explicit `off`
    // suppresses any inherited host env; omitting the module preserves legacy
    // `ACOS_ART_KEYSTORE` env compatibility and defaults to `off` (honest
    // `AndroidKeyStore not found` + diagnostic log breadcrumb).
    if let Some(v) = &keystore_env {
        sb.env(acos_services::modules::keystore::KEYSTORE_ENV, v);
    }
    // K5b: the keystore probe's mode and its list of paths to reach for
    // (`fixtures/apps/keystore-probe`). Test-only names: nothing else reads
    // them, and an unset variable leaves the guest environment unchanged.
    for knob in ["ACOS_ART_KEYPROBE_MODE", "ACOS_ART_KEYPROBE_TARGETS"] {
        if let Ok(v) = std::env::var(knob) {
            sb.env(knob, &v);
        }
    }
    acos_services::animation::set_enabled(opts.animations.is_on());
    set_animations_state(opts.animations.is_on());
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
    // MERGEFIX: one construction, and it is wired to ATMS's `top_process`.
    // The merge of main (PERF4's probe state + wake) into modules (MPROC5's
    // per-process samples) kept **both** declarations: the wired one was
    // immediately shadowed by `ProbeState::default()`, whose own
    // `top_process` Arc nothing ever writes. Every session then served the
    // primary process's sample, so a cross-process launch (`mproc3`) never saw
    // the second process's window and its log/probe lines looked absent.
    let probe_state = Arc::new(std::sync::Mutex::new(ProbeState::for_session(
        Arc::clone(&live.top_process),
    )));
    // PERF4: the probe state uses the wake the peer thread already signals, so
    // one condvar covers both fact sources (a parsed guest sample and an applied
    // transaction). Nothing has subscribed to `ProbeState::default()`'s own wake
    // yet, so replacing it here cannot strand a waiter.
    probe_state
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .wake = Arc::clone(&settle_change);
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
                    0,
                ),
                None => {
                    drain_stdout_lines(out_pipe, cap, probe_for_out, Some(compositor_for_probe), 0)
                }
            }
        } else {
            // Diagnostic: keep the whole stream on disk (`ACOSLAUNCH_GUEST_STDOUT_FILE`)
            // as well as the bounded tail, so a large guest dump survives.
            match out_tee {
                Some(path) => drain_tee(out_pipe, cap, &path),
                None => crate::capture::drain_to_tail(out_pipe, cap),
            }
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
            opts.density_dpi,
            &boot_image_report,
            &mut child,
            pid,
            &mut cgroup,
            &live,
            &probe_state,
            probe_fifo,
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
            // VIEW1a: where the live-view stream socket is bound (VIEW1c: its
            // own host-only directory, never mounted into the guest).
            &view_dir.0,
            // ADR-0029 item 4: the attached set and the live-op channel.
            &session_module_rows,
            session_has_home,
            &service_control,
            // MPROC4 step 3b: the session's secondary processes.
            &mut secondary_pool,
            spawn_rx,
            // MPROC5c: the idle kill's timeout (`0` disables it).
            Duration::from_secs(opts.idle_kill_secs),
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
    // Wake the peer's idle wait: an idle session may be asleep until a deadline
    // that will never come.
    peer_wakeup.wake();
    // MPROC4 step 3b: a secondary process never outlives the session (its whole
    // cgroup scope goes, exactly like the primary's).
    secondary_pool.shutdown();
    // The peer is asleep in `poll(2)`: an idle session has no timer to notice
    // `stop`, so wake it (the byte is the edge, the flag is the decision).
    delivery_wake.wake();
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
    if let Some(image) = &boot_image {
        unsupported_notes.push(image.note());
    }
    unsupported_notes.push(boot_image_report.line());
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
    // WV4/WV5b: publish where this session's WebView socket lives on the host,
    // and whether the lazy gate is listening on it. The guest side is fixed
    // (`/run/acos/webview.sock`, WIRE.md §0). With the gate up, the client's
    // connect succeeds and the *first* connect wakes the shared broker
    // (`acos-webview --shared`, one per host); with `ACOS_WEBVIEW_GATE=0` there
    // is no listener and the connect fails with the kernel's real ENOENT.
    match (&webview_gate, &webview_broker_socket) {
        (Some(_), Some(broker)) => unsupported_notes.push(format!(
            "webview socket: {} -> guest {} (module `webview` real: lazy gate listening; the \
             first connect starts the shared broker at {})",
            crate::webview_socket::host_socket_path(&webview_socket_dir).display(),
            crate::webview_socket::GUEST_SOCKET_PATH,
            broker.display()
        )),
        (None, _) => unsupported_notes.push(
            "webview: module `webview` is off (no `com.android.webview` provider package, no \
             gate socket): `WebView` takes Android's own no-provider path. Attach it with \
             `services: [{id: webview, profile: real}]`."
                .to_string(),
        ),
        (Some(_), None) => unsupported_notes.push(format!(
            "webview socket: {} -> guest {} (lazy gate listening; no shared broker endpoint \
             named for this session)",
            crate::webview_socket::host_socket_path(&webview_socket_dir).display(),
            crate::webview_socket::GUEST_SOCKET_PATH
        )),
    }
    // The gate (its listener thread and the session directory) is dropped after
    // `result` is built and before the run dir is removed.
    let webview_gate = webview_gate;
    let mut result = LaunchResult {
        outcome,
        code,
        boot_image: Some(boot_image_report),
        // ABI gate: `None` on every launch that reached the guest (the gate
        // returns before this builder).
        abi: None,
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
        extra_packages: Some(
            extras
                .iter()
                .map(|extra| {
                    serde_json::json!({
                        "package": extra.package,
                        "uid": extra.uid,
                        "apk": extra.guest_path,
                        "native_library_dir": extra.guest_lib_dir,
                    })
                })
                .collect(),
        ),
        processes: {
            // MPROC4 step 3b: the activity manager's per-tag `ProcessRecord`s
            // and the launcher's spawn table. A single-process session keeps the
            // field absent (nothing to report, nothing invented).
            let mut launcher = secondary_pool.to_json();
            launcher["requests"] = secondary_pool.records.clone().into();
            let processes = serde_json::json!({
                "activity_manager": summary.processes,
                "launcher": launcher,
            });
            let multi = summary.processes.len() > 1
                || !secondary_pool.records.is_empty()
                || !secondary_pool.failures.is_empty()
                || !secondary_pool.uninstalled.is_empty();
            multi.then_some(processes)
        },
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
    // R3: a guest killed by a signal gets its own crash record (fault address,
    // si_code, thread, native backtrace) next to the exit record, and the raw
    // ART dump is kept verbatim. A crash also keeps the per-run tree: the
    // evidence a debugger needs (`/proc` maps, loaders, sockets) is only in
    // there, and "the run directory was deleted by teardown" was exactly how
    // R2 lost its stack.
    let native_crash = if result.guest_signal.is_some() {
        crate::crash::analyze(&result.stdout, &result.stderr, &result.logs)
    } else {
        None
    };
    let crash_path = native_crash
        .as_ref()
        .map(|crash| write_native_crash(&out, crash))
        .transpose()?;
    if let Some(crash) = &native_crash {
        eprintln!(
            "acos-art: native crash: {} (raw dump: {})",
            crash.summary(),
            crash_path
                .as_deref()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "(not written)".to_string())
        );
    }
    // Keep the per-run tree only for a crash the *guest* raised itself and whose
    // evidence was actually captured, under a retention cap: a SIGKILL/SIGTERM
    // from the host (bounded destroy, watchdog, timeout) is a teardown decision
    // and never keeps anything, and a crash loop must not fill the disk
    // (`ACOS_ART_KEEP_CRASH_MAX`, default 8; 0 = keep none). Every other outcome
    // is still removed by `RunDir::drop` (S4).
    let kept_run_dir =
        if crate::keeps_run_tree_for_crash(result.guest_signal, native_crash.is_some()) {
            let detail = serde_json::json!({
                "utc_ms": host_ms(),
                "route": "launch",
                "code": result.code,
                "guest_signal": result.guest_signal,
                "native_crash": native_crash.as_ref().map(crate::crash::NativeCrash::summary),
            });
            crate::keep_run_for_guest_crash(run, detail)
        } else {
            None
        };
    write_session_exit(
        &out,
        &result,
        guest_crash.as_deref(),
        native_crash.as_ref(),
        crash_path.as_deref(),
        kept_run_dir.as_deref(),
    );
    Ok(result)
}

/// Write the native-crash record next to the session exit record.
///
/// Two files: `native-crash.json` (the structured fields an agent reads) and
/// `native-crash.txt` (ART's own dump, verbatim, so a parser gap never destroys
/// the evidence).
fn write_native_crash(out: &Path, crash: &crate::crash::NativeCrash) -> Result<PathBuf> {
    fs::create_dir_all(out)?;
    let txt = out.join("native-crash.txt");
    let mut body = String::new();
    if let Some(shim) = &crash.shim {
        body.push_str(&format!("{}{}\n", crate::crash::CRASH_MARKER, shim));
    }
    if !crash.raw.is_empty() {
        body.push_str(&crash.raw);
        if !body.ends_with('\n') {
            body.push('\n');
        }
    }
    fs::write(&txt, body)?;
    fs::write(
        out.join("native-crash.json"),
        serde_json::to_vec_pretty(&crash.to_json()).unwrap_or_default(),
    )?;
    Ok(txt)
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
/// Guest `InputConnection.commitText` outcome emitted on the main looper.
const COMMIT_TEXT_PREFIX: &str = "ACOSCOMMITTEXT ";
const CAPTURE_SOURCE: &str = "acos-c1-buffer";

#[derive(Clone, Debug, PartialEq, Eq)]
struct CommitTextOutcome {
    ok: bool,
    handled: bool,
    no_focused_editor: bool,
    commit_attempted: bool,
    focus_class: Option<String>,
    error: Option<String>,
}

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
    /// ROT8: the guest reports the app's task finished: its last Activity is
    /// destroyed and no window root is attached (`settle.window_gone`). The
    /// tree of such a sample is an empty window forest.
    window_gone: bool,
    /// P12: the guest's applied animation state (`None` if it did not report).
    animations_on: Option<bool>,
    /// CR1FIX #20: the guest's `window_cmd` counters at the START of this
    /// sample's tree walk: host WINDOW commands it has finished processing, and
    /// how many of those failed. `None` when the guest did not report.
    window_cmd: Option<WindowCmdCounters>,
}

/// CR1FIX #20: the guest's count of finished (and failed) WINDOW commands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct WindowCmdCounters {
    done: u64,
    failed: u64,
}

/// CR1FIX #20: whether a probe sample proves the guest applied the first
/// `sent` WINDOW commands before it walked its tree. A sequence increase proves
/// nothing (an unread line the guest emitted before it saw the command is
/// published with a higher host sequence); only the guest's own command-specific
/// counter does.
#[derive(Debug, PartialEq, Eq)]
enum WindowAck {
    /// The sample predates the commands (or carries no counter): keep waiting.
    Pending,
    /// Every sent command was processed and none failed since `failed_seen`.
    Applied,
    /// The guest reports a command failed (its own `window-failed` mark).
    Failed,
}

fn window_ack(sample: &ProbeSample, sent: u64, failed_seen: u64) -> WindowAck {
    match sample.window_cmd {
        Some(c) if c.done >= sent => {
            if c.failed > failed_seen {
                WindowAck::Failed
            } else {
                WindowAck::Applied
            }
        }
        _ => WindowAck::Pending,
    }
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

    /// PERF4: the settle/observation loops' wait. A settle binds exactly when
    /// the *last* host-visible fact it needs has landed (the probe sample that
    /// differs from the baseline, the C1 store that commits it, the ack). This
    /// condvar is signalled by every producer of those facts — the guest-line
    /// reader for a sample, the peer loop for an applied transaction (the C1
    /// store) — so the loop wakes on the event instead of re-reading state on a
    /// fixed period.
    ///
    /// The period was not harmless: with a 15 ms sleep a tap whose commit landed
    /// 1 ms after a tick waited the whole quantum, and a sub-millisecond
    /// difference between two builds moved the metric by a full 15 ms (measured:
    /// the same build resolves `frame_ms` 16 or 29 depending on phase — v0.3.0's
    /// M8 form-fast "+8.7 ms regression" was that flip, not 8.7 ms of work).
    /// `WAKE_LIVENESS_MS` bounds only how long a *silent* guest (a dead child, a
    /// stalled probe) goes unnoticed; every real change wakes the loop at once.
    ///
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

/// PERF4: how long a settle/observation loop may go without re-reading host
/// state when nothing signalled [`ProbeWake`].
///
/// Every fact a settle waits for has a producer that signals the wake, so this
/// is a *liveness* bound only: it is how quickly a silent guest (a dead child, a
/// probe thread that never answers) is noticed, which the old fixed poll noticed
/// on its own period. It must stay well under the shortest useful settle budget
/// (`ACOS_STRICT_SETTLE_CONFIRM_MS` is 24 ms) so a missed edge can never outlast
/// a rule window, and it is never the cadence a normal settle runs at.
const WAKE_LIVENESS_MS: u64 = 10;

/// The wake channel of this session's probe state (see [`ProbeWake`]).
fn settle_wake(probe_state: &Arc<std::sync::Mutex<ProbeState>>) -> Arc<ProbeWake> {
    Arc::clone(
        &probe_state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .wake,
    )
}

/// Block until the session's host-visible state may have moved, or the liveness
/// bound elapses. Callers re-read their own state after every return, so a
/// spurious wake only costs one re-read; a missed one is impossible because the
/// tick is compared against the last value `wait` consumed.
fn wait_for_change(wake: &ProbeWake, seen: &mut u64) {
    wake.wait(seen, Duration::from_millis(WAKE_LIVENESS_MS));
}

#[derive(Default)]
struct ProbeState {
    latest: Option<ProbeSample>,
    /// MPROC5: one sample per app process tag. A session can run several
    /// guests, each printing its own probe lines with its own sequence counter,
    /// so a single `latest` would serve whichever process printed last — a tree
    /// from a window that is not on top. The served sample is the one from the
    /// process ATMS publishes as foreground (`top_process`).
    by_process: std::collections::BTreeMap<u32, ProbeSample>,
    /// MPROC5: the foreground process tag (`LiveHandles::top_process`), written
    /// by ATMS whenever the top-of-task moves.
    ///
    /// It is the **same** `Arc` ATMS publishes into, never a private copy: the
    /// sample that is served must be the one of the process the activity
    /// manager calls foreground (`ProbeState::for_session`).
    top_process: Arc<std::sync::atomic::AtomicU32>,
    /// MPROC7: per-process probe-sequence bases. A guest's probe counter starts
    /// at 1 in every **process**, so a respawned process's samples (1, 2, ...)
    /// are below the session's last served sequence and every later strict
    /// settle waited forever on a "fresh" sample (`probe-floor`). When a process
    /// is spawned, its entries are cleared and this base is set to the session's
    /// current maximum, so its samples continue monotonically.
    seq_offsets: std::collections::BTreeMap<u32, u64>,
    /// Last raw guest `seq` observed per process tag (so stale check stays
    /// per-process while published `seq` advances monotonically across switches).
    last_raw_seq: std::collections::BTreeMap<u32, u64>,
    /// Highest `seq` published across all processes in this session.
    highest_seq: u64,
    /// H15: the last `anim_callbacks` set traced, so the trace names each
    /// distinct animation callback once instead of once per sample.
    anim_traced: String,
    /// H15: the last `in_touch_mode` value traced (same once-only rule).
    touch_traced: Option<bool>,
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
    /// ROT4: the guest's own diagnostic lines (`ACOSLAUNCH MARK …`), oldest
    /// first, so an interactive reply can carry the rungs that ran for it.
    marks: std::collections::VecDeque<String>,
    /// SETTLE5: the demand-probe channel's own health, so a `probe-floor`
    /// timeout says which side stalled instead of leaving it a mystery.
    /// `wakes_sent` counts wake lines written to the guest's stdin,
    /// `wake_failures` counts writes the channel refused (a dead reader),
    /// `stale_samples` counts guest samples the host refused to publish
    /// because their sequence was not newer (a probe-thread restart), and
    /// `stall_pending_ms` is the guest's own `probe-stalled` mark (a sample
    /// posted to the main looper that has not run for this long).
    wakes_sent: u64,
    wake_failures: u64,
    stale_samples: u64,
    stall_pending_ms: Option<u64>,
    commit_text_results: std::collections::BTreeMap<u64, CommitTextOutcome>,
    next_commit_text_seq: u64,
}

impl ProbeState {
    /// The probe state a session serves its trees from.
    ///
    /// MERGEFIX: this constructor is the **only** way a session's probe state is
    /// built, so the foreground tag it reads is by construction the one ATMS
    /// writes. The merge of main into modules had two declarations (a wired one
    /// and a default one) and the second shadowed the first, which silently
    /// pinned `top_process` to 0: every tree reply described the primary
    /// process, however far the foreground had moved (`mproc3-two-process`).
    fn for_session(top_process: Arc<std::sync::atomic::AtomicU32>) -> Self {
        Self {
            top_process,
            ..Self::default()
        }
    }

    /// The tag this state treats as foreground (diagnostics/tests).
    #[cfg(test)]
    fn published_top(&self) -> u32 {
        self.top_process.load(std::sync::atomic::Ordering::Relaxed)
    }
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

/// SETTLE5: the demand-probe channel's own health, attached to a settle
/// timeout so it names the side that stalled. `probe-floor` used to read as
/// "no fresh sample", which is equally consistent with a dead wake channel, a
/// guest that keeps emitting samples the host refuses, and a guest main looper
/// that cannot run the posted probe; these counters separate them:
///
/// * `wake_failures` > 0 — the guest's stdin refused a wake (a dead reader /
///   control thread); the demand channel is gone and only the guest's own
///   fallback can publish.
/// * `stale_samples` > 0 — the guest emitted samples whose sequence was not
///   newer (a probe-thread restart), so the host froze on the old sample.
/// * `guest_stall_pending_ms` is the guest's own `probe-stalled` mark: a sample
///   posted to its main looper that has not run for this long (its main thread
///   is busy or blocked; the probe is starved, not silent).
fn probe_health_json(state: &Arc<std::sync::Mutex<ProbeState>>) -> serde_json::Value {
    let state = state.lock().unwrap_or_else(|e| e.into_inner());
    let age = state
        .latest
        .as_ref()
        .map(|sample| host_ms().saturating_sub(sample.at_ms));
    serde_json::json!({
        "wakes_sent": state.wakes_sent,
        "wake_failures": state.wake_failures,
        "stale_samples": state.stale_samples,
        "guest_stall_pending_ms": state.stall_pending_ms,
        "newest_sample_age_ms": age,
        "note": "the demand-probe channel's own counters at the timeout: wakes written, wakes the channel refused, guest samples the host refused (not newer), and the guest's own starved-probe mark",
    })
}

fn env_u64(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(default)
}

/// P12b: the per-request settle deadline (`settle_timeout_ms`), clamped to a
/// sane range. `None` keeps the verb's documented default, so existing callers
/// are unchanged. A deadline only bounds the wait: on expiry the reply still
/// carries the latest tree and frame identity with `settled:false`.
fn request_settle_timeout(request: &serde_json::Value, default: Duration) -> Duration {
    match request.get("settle_timeout_ms").and_then(|v| v.as_u64()) {
        Some(ms) => Duration::from_millis(ms.clamp(1, 600_000)),
        None => default,
    }
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
/// The structured fields of the guest's own crash line, when it printed one.
///
/// The line is `ACOSLAUNCH {json}`; the JSON already carries `outcome`, `code`,
/// `message`, and (R4) `exception`/`stack`/`causes`. A guest from an older image
/// without those keys simply yields the smaller object.
fn guest_crash_detail(guest_crash: Option<&str>) -> Option<serde_json::Value> {
    let json = guest_crash?.strip_prefix("ACOSLAUNCH ")?;
    serde_json::from_str::<serde_json::Value>(json).ok()
}

/// Last `limit` logd entries, as `prio/tag/message` strings (oldest first).
fn logd_tail(logs: &[crate::LogEntry], limit: usize) -> Vec<String> {
    let start = logs.len().saturating_sub(limit);
    logs[start..]
        .iter()
        .map(|e| format!("{}/{}: {}", e.priority_name(), e.tag, e.message))
        .collect()
}

/// Last `limit` non-empty lines of a captured stream (oldest first).
fn stdout_tail(text: &str, limit: usize) -> Vec<String> {
    let mut lines: Vec<String> = text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(str::to_string)
        .collect();
    let start = lines.len().saturating_sub(limit);
    lines.drain(..start);
    lines
}

fn write_session_exit(
    out: &Path,
    result: &LaunchResult,
    guest_crash: Option<&str>,
    native_crash: Option<&crate::crash::NativeCrash>,
    native_crash_path: Option<&Path>,
    kept_run_dir: Option<&Path>,
) {
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
        // R4: the guest's crash line carries its own `exception`, `stack` and
        // `causes` (each cause with class, message and stack). Parsing it here
        // keeps `guest_crash` byte-for-byte for existing readers while the
        // structured fields are usable directly by an agent.
        "guest_crash_detail": guest_crash_detail(guest_crash),
        // R3: a guest killed by a signal carries the fault address, si_code,
        // faulting thread and the runtime's native backtrace (mapped library +
        // offset). `None` when the guest died without producing either the
        // shim line or ART's dump — the plain `guest_signal` stays the evidence.
        "native_crash": native_crash.map(crate::crash::NativeCrash::to_json),
        "native_crash_path": native_crash_path.map(|p| p.display().to_string()),
        // R3: where the crashed run's tree was kept (extracted native libs,
        // loaders, `/proc` leftovers, `out/`), when retention kept it. Absent
        // when nothing was kept (host-sent signal, no crash record, cap 0).
        "kept_run_dir": kept_run_dir.map(|p| p.display().to_string()),
        // R4: a guest that died without printing a crash line (native abort,
        // `System.exit`, a kill) still has the framework's own logd trail and
        // its last stdout/stderr lines. Bounded, newest last.
        "logd_tail": logd_tail(&result.logs, 24),
        "logd_final": FINAL_LOGD_FILE,
        "stdout_tail": stdout_tail(&result.stdout, 32),
        "stderr_tail": stdout_tail(&result.stderr, 16),
        "disk_free_bytes": disk_free_bytes(out),
        // W48: which heap configuration this session ran with (the
        // `ACOS_ART_HEAP*` seam, resolved and validated before the guest).
        "heap": crate::props::effective_heap_json(),
        // ABIGATE: the package-level ABI gate's inventory when the launch was
        // refused before the guest (`code: UNSUPPORTED_ABI`), so the CLI can
        // put `apk_abis`/`host_abi`/`libs_sample` in the error's `details`
        // without re-reading the APK.
        "abi": result.abi,
        "cgroup": result.notes.iter().find(|n| n.starts_with("cgroup:")),
        "logd": result.notes.iter().find(|n| n.starts_with("logd:")),
        // H17: a guest that exits 126 before its first frame is the guard
        // failing closed, and the guard's own ERR line is the only statement of
        // *why*. Keep it (and the other host notes) in the record the caller
        // reads, instead of leaving an exit code with no cause.
        "guard": result.notes.iter().find(|n| n.starts_with("seccomp:")).cloned(),
        "notes": result.notes,
        "stdout_tail_last": result.stdout.lines().rev().find(|l| !l.trim().is_empty()),
        "stderr_tail_last": result.stderr.lines().rev().find(|l| !l.trim().is_empty()),
    });
    let _ = fs::create_dir_all(out);
    let _ = fs::write(
        out.join("session-exit.json"),
        serde_json::to_vec_pretty(&doc).unwrap_or_default(),
    );
    write_final_logd(out, &result.logs, logd_dropped(&result.notes));
}

/// Name of the full logd dump next to `session-exit.json` (JSON lines, one
/// `{seq, prio, tag, message}` per entry, `seq` = 1-based ordinal in the guest's
/// log sink). The verifier flushes the guest's log only *between steps*, so every
/// entry written after the last flush -- and, for a guest that died, the entries
/// that explain *why* (a native abort's assertion text) -- is otherwise lost
/// from `logs/logcat.txt`. `acos verify` merges the entries it is missing.
pub const FINAL_LOGD_FILE: &str = "logd-final.jsonl";

/// Entries the sink dropped from the front (`logd: K entries kept of T (D
/// dropped, ...)`), so a dump's `seq` stays the sink's absolute ordinal.
fn logd_dropped(notes: &[String]) -> u64 {
    notes
        .iter()
        .find(|n| n.starts_with("logd: "))
        .and_then(|n| n.split('(').nth(1))
        .and_then(|n| n.split_whitespace().next())
        .and_then(|n| n.parse().ok())
        .unwrap_or(0)
}

fn write_final_logd(out: &Path, logs: &[crate::LogEntry], dropped: u64) {
    use std::io::Write as _;
    let Ok(file) = fs::File::create(out.join(FINAL_LOGD_FILE)) else {
        return;
    };
    let mut writer = std::io::BufWriter::new(file);
    for (index, entry) in logs.iter().enumerate() {
        let line = serde_json::json!({
            "seq": dropped + index as u64 + 1,
            "prio": entry.prio,
            "tag": entry.tag,
            "message": entry.message,
            "ts_ms": entry.ts_ms,
        });
        let _ = writeln!(writer, "{line}");
    }
    let _ = writer.flush();
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
struct ShortSocketDir(PathBuf, bool);

impl ShortSocketDir {
    fn new() -> std::io::Result<Self> {
        Self::new_named("art")
    }

    /// A fresh short socket directory named `acos-<kind>-<pid>-<seq>-<nonce>`.
    ///
    /// `kind = "art"` is the session's socket directory (bind-mounted into the
    /// guest at `/dev/socket`), `kind = "view"` is the live-view directory, which
    /// is **never** mounted anywhere and must stay host-only.
    ///
    /// The random hex nonce prevents a reconnecting live-view watcher from
    /// attaching to a different guest if the OS reuses a prior launcher PID
    /// with the same in-process sequence counter.
    ///
    /// Every candidate is created 0700 and then verified (a directory, owned by
    /// this uid, mode 0700); a candidate that cannot be made private is skipped
    /// rather than used, and `/dev/shm` — which is world-visible by default — is
    /// only ever used through the same 0700 + owner check.
    fn candidate_dirs(kind: &str) -> Vec<PathBuf> {
        use std::io::Read;
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let seq = SEQ.fetch_add(1, Ordering::Relaxed);
        let mut nonce_bytes = [0u8; 8];
        let got_random = fs::File::open("/dev/urandom")
            .and_then(|mut f| f.read_exact(&mut nonce_bytes))
            .is_ok();
        let nonce = if got_random {
            u64::from_le_bytes(nonce_bytes)
        } else {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos() as u64)
                .unwrap_or(0);
            nanos ^ ((std::process::id() as u64) << 32) ^ seq.wrapping_mul(0x9e37_79b9_7f4a_7c15)
        };
        let id = format!("{}-{}-{:016x}", std::process::id(), seq, nonce);
        let uid = unsafe { libc::getuid() };
        let runtime = PathBuf::from(format!("/run/user/{uid}"));
        let mut candidates = Vec::new();
        if runtime.is_dir() {
            candidates.push(runtime.join(format!("acos-{kind}-{id}")));
        }
        if let Ok(root) = crate::repo_root() {
            candidates.push(
                root.join(".cache")
                    .join(format!("acos-{kind}-sock"))
                    .join(&id),
            );
        }
        candidates.push(PathBuf::from(format!("/dev/shm/acos-{kind}-{id}")));
        candidates
    }

    fn new_named(kind: &str) -> std::io::Result<Self> {
        Self::select_candidate(Self::candidate_dirs(kind), kind == "view")
    }

    fn select_candidate(candidates: Vec<PathBuf>, is_view: bool) -> std::io::Result<Self> {
        let mut last = None;
        for dir in candidates {
            if !socket_dir_fits_sun_path(&dir) {
                last = Some(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    format!(
                        "socket dir {} exceeds unix sun_path limit",
                        dir.display()
                    ),
                ));
                continue;
            }
            match private_dir(&dir) {
                Ok(()) => return Ok(Self(dir, is_view)),
                Err(e) => last = Some(e),
            }
        }
        Err(last.unwrap_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "no short socket dir available",
            )
        }))
    }

    /// Z4 (research seam): use an explicit directory, and leave it in place on
    /// drop (the session directory is the shared plan's, not this process's).
    fn at(dir: &Path) -> std::io::Result<Self> {
        private_dir(dir)?;
        Ok(Self(dir.to_path_buf(), false))
    }
}

/// True when `dir/<socket-leaf>` fits within the platform's `sockaddr_un::sun_path`
/// buffer (including the separating `/` and trailing NUL byte).
fn socket_dir_fits_sun_path(dir: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    // Covers every socket leaf bound inside ShortSocketDir (`webview.sock` = 12,
    // `peer.sock` = 9, `view.sock` = 9, `dnsproxyd` = 9, `logdw` = 5) with headroom.
    const MAX_SOCKET_LEAF_BYTES: usize = 16;
    // SAFETY: zeroed `sockaddr_un` is valid; only inspecting `sun_path` array length.
    let sun_path_cap = unsafe { std::mem::zeroed::<libc::sockaddr_un>() }
        .sun_path
        .len();
    dir.as_os_str()
        .as_bytes()
        .len()
        .saturating_add(1 + MAX_SOCKET_LEAF_BYTES + 1)
        <= sun_path_cap
}

/// Create (or verify) a directory that only this uid may use: 0700 and ours.
///
/// Refuses — never silently ignores — an existing path that is not a directory,
/// is owned by another uid, or cannot be tightened to 0700. The live-view socket
/// directory must be **host-only**: it is never bind-mounted into a guest, and
/// the guest runs as this same uid, so file modes alone are not a boundary (the
/// mount namespace is).
fn private_dir(dir: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
    match fs::DirBuilder::new().mode(0o700).create(dir) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e),
    }
    let metadata = fs::symlink_metadata(dir)?;
    if !metadata.is_dir() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("{} is not a directory", dir.display()),
        ));
    }
    if metadata.uid() != unsafe { libc::getuid() } {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            format!(
                "{} is owned by uid {} (expected {})",
                dir.display(),
                metadata.uid(),
                unsafe { libc::getuid() }
            ),
        ));
    }
    let mode = metadata.permissions().mode() & 0o777;
    if mode != 0o700 {
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
        let after = fs::symlink_metadata(dir)?.permissions().mode() & 0o777;
        if after != 0o700 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                format!(
                    "{} has mode {after:o}, could not make it 0700",
                    dir.display()
                ),
            ));
        }
    }
    Ok(())
}

impl Drop for ShortSocketDir {
    fn drop(&mut self) {
        // Z4: a foreign-guest session directory is owned by the driver. The
        // host-only live-view directory (field 1) is never the driver's: it is
        // always removed here.
        if !self.1 && std::env::var_os("ACOS_ZYGOTE_SESSION").is_some() {
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
    fs::write(
        &tmp,
        serde_json::to_vec_pretty(&doc).map_err(|e| Error::Layout(e.to_string()))?,
    )?;
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
/// ABIGATE: the refusal for an APK whose native code has no `lib/<host-abi>/`.
///
/// Shaped like [`disk_full_result`] — no guest ran, nothing is a pass — and it
/// carries the inventory the caller needs to act on it (`apk_abis`,
/// `host_abi`, `libs_sample`) in `abi` *and* in `code == UNSUPPORTED_ABI`, the
/// documented protocol code.
fn abi_gate_result(
    opts: &LaunchOpts,
    package: &str,
    apk_abis: &[String],
    libs_sample: &[String],
) -> LaunchResult {
    let host_abi = acos_services::native_libs::HOST_ABI;
    LaunchResult {
        outcome: Outcome::Unsupported,
        code: Some("UNSUPPORTED_ABI".to_string()),
        // Decided from the APK index alone, before the image is resolved.
        boot_image: None,
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
        extra_packages: None,
        processes: None,
        notes: vec![format!(
            "UNSUPPORTED_ABI: the APK ships native code only for [{}], and this engine is \
             {host_abi}-only; no guest was started (AOSP PackageManager refuses the same \
             package with INSTALL_FAILED_NO_MATCHING_ABIS)",
            apk_abis.join(", ")
        )],
        abi: Some(serde_json::json!({
            "apk_abis": apk_abis,
            "host_abi": host_abi,
            "libs_sample": libs_sample,
        })),
        marker: None,
    }
}

fn disk_full_result(opts: &LaunchOpts, package: &str, free: u64, min: u64) -> LaunchResult {
    LaunchResult {
        outcome: Outcome::Unsupported,
        code: Some("HOST_DISK_FULL".to_string()),
        // The disk-full refusal happens before the image is resolved.
        boot_image: None,
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
        extra_packages: None,
        processes: None,
        notes: vec![format!(
            "HOST_DISK_FULL: {free} bytes free on the scratch filesystem, below \
             ACOS_ART_MIN_DISK_MB={} MiB; refusing to start a guest whose frames cannot be \
             written (retryable=true)",
            min / 1024 / 1024
        )],
        abi: None,
        marker: None,
    }
}

fn latest_probe(state: &Arc<std::sync::Mutex<ProbeState>>) -> Option<ProbeSample> {
    let state = state.lock().unwrap_or_else(|e| e.into_inner());
    let top = state.top_process.load(std::sync::atomic::Ordering::Relaxed);
    state
        .by_process
        .get(&top)
        .cloned()
        // Before ATMS has a top (startup), the newest sample of any process is
        // the only one there is; tag 0 is the launched app.
        .or_else(|| state.latest.clone())
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
    tag: u32,
) -> TailBuffer {
    drain_tee_lines(reader, cap, &PathBuf::new(), state, compositor, tag)
}

/// Read one `\n`-terminated line, bounding the bytes held in memory.
///
/// `None` = EOF before any byte; `Some(Some(line))` = a complete line (the
/// newline is kept); `Some(None)` = the line exceeded `cap` and was drained.
fn read_capped_line<R: std::io::BufRead>(reader: &mut R, cap: usize) -> Option<Option<Vec<u8>>> {
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
                match std::io::Read::take(&mut *reader, 64 * 1024).read_until(b'\n', &mut sink) {
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
    tag: u32,
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
        // ROT4: the guest's diagnostic lines (`ACOSLAUNCH MARK …`, one per
        // lifecycle/config rung, and its stall dumps) are forwarded to the
        // launcher's stderr unconditionally, so the interactive session trace
        // names a failing rung instead of swallowing it (ROT3 could not tell
        // why the Activity-level rotate sequence did not run). Not env-gated:
        // a swallowed diagnostic is a correctness cost, not a debug nicety.
        if trimmed.starts_with("ACOSLAUNCH ") || trimmed.starts_with("ACOSPROBE-STALL") {
            eprintln!("acos-art: guest: {trimmed}");
            let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
            // SETTLE5: the guest names its own starved probe (a sample posted
            // to the main looper that has not run). Kept as the newest mark so
            // a settle timeout can report a guest-side stall with its measured
            // age, never as a guess.
            if let Some(rest) = trimmed.strip_prefix("ACOSLAUNCH MARK probe-stalled") {
                if let Some(ms) = rest
                    .split_whitespace()
                    .find_map(|word| word.strip_prefix("pending_ms="))
                    .and_then(|value| value.parse::<u64>().ok())
                {
                    state.stall_pending_ms = Some(ms);
                }
            }
            state.marks.push_back(trimmed.to_string());
            while state.marks.len() > 64 {
                state.marks.pop_front();
            }
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
                    if let Some(names) = event
                        .and_then(|e| e.get("anim_callbacks"))
                        .and_then(|v| v.as_array())
                        .filter(|a| !a.is_empty())
                    {
                        let names: Vec<String> = names
                            .iter()
                            .filter_map(|v| v.as_str().map(str::to_string))
                            .collect();
                        let joined = names.join(",");
                        let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
                        if state.anim_traced != joined {
                            state.anim_traced = joined.clone();
                            if let Some(trace) = state.trace.as_mut() {
                                trace.line("GUEST", &format!("anim_callbacks {joined}"));
                            }
                        }
                    }
                    if let Some(touch) = event
                        .and_then(|e| e.get("in_touch_mode"))
                        .and_then(|v| v.as_bool())
                    {
                        let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
                        if state.touch_traced != Some(touch) {
                            state.touch_traced = Some(touch);
                            if let Some(trace) = state.trace.as_mut() {
                                trace.line("GUEST", &format!("touch_mode {touch}"));
                            }
                        }
                    }
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
                    let window_gone = event
                        .and_then(|v| v.get("window_gone"))
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
                    // P12: the guest is the authority on what it applied.
                    let animations_on = value
                        .get("animations")
                        .and_then(|v| v.get("on"))
                        .and_then(|v| v.as_bool());
                    let window_cmd = value.get("window_cmd").and_then(|c| {
                        Some(WindowCmdCounters {
                            done: c.get("done")?.as_u64()?,
                            failed: c.get("failed")?.as_u64()?,
                        })
                    });
                    if let Some(on) = animations_on {
                        set_animations_state(on);
                    }
                    let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
                    // MPROC7: rebase a (re)spawned process's own counter so the
                    // session's probe sequence stays monotonic across a respawn.
                    let raw_seq = seq;
                    let newer = state
                        .last_raw_seq
                        .get(&tag)
                        .map(|&prev| prev < raw_seq)
                        .unwrap_or(true);
                    if newer {
                        state.last_raw_seq.insert(tag, raw_seq);
                        let seq = raw_seq
                            .saturating_add(state.seq_offsets.get(&tag).copied().unwrap_or(0))
                            .max(state.highest_seq.saturating_add(1));
                        state.highest_seq = seq;
                        let at_ms = host_ms();
                        // SETTLE5 diagnostic: the host's own receipt time and
                        // the guest's build time, so a stalled probe can be
                        // attributed to the guest (no line), the pipe (line
                        // without receipt) or the wait (receipt ignored).
                        // Trace-only: no reply changes.
                        if let Some(trace) = state.trace.as_mut() {
                            let guest_ms = value
                                .get("time")
                                .and_then(|v| v.as_u64())
                                .unwrap_or(0);
                            let cause = value
                                .get("cause")
                                .and_then(|v| v.as_str())
                                .unwrap_or("?");
                            trace.line(
                                "HOST",
                                &format!(
                                    "probe rx seq={seq} cause={cause} lag_ms={} settled={settled} \
                                     draw_seq={draw_seq} gen={generation}",
                                    at_ms.saturating_sub(guest_ms)
                                ),
                            );
                        }
                        let sample = ProbeSample {
                            seq,
                            tree,
                            at_ms,
                            generation,
                            draw_ms,
                            draw_seq,
                            settled,
                            pending,
                            window_gone,
                            animations_on,
                            window_cmd,
                        };
                        state.by_process.insert(tag, sample.clone());
                        // `latest` stays the newest sample of **any** process; it
                        // is the pre-ATMS fallback and the startup loop's input.
                        let newer_any = state.latest.as_ref().map(|p| p.seq < seq).unwrap_or(true);
                        if newer_any {
                            state.latest = Some(sample);
                        }
                    } else {
                        // SETTLE5: a sample the process itself has already
                        // superseded is the evidence a `probe-floor` timeout
                        // cites (the guest sent it, the wait ignored it).
                        state.stale_samples = state.stale_samples.saturating_add(1);
                        let latest = state
                            .latest
                            .as_ref()
                            .map(|p| p.seq)
                            .unwrap_or_default();
                        if let Some(trace) = state.trace.as_mut() {
                            trace.line(
                                "HOST",
                                &format!("probe stale seq={seq} latest={latest}"),
                            );
                        }
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
            state.guest_launch = Some(trimmed.chars().take(32 * 1024).collect());
        } else if let Some(json) = trimmed.strip_prefix(UNSUPPORTED_PREFIX) {
            if let Ok(value) = serde_json::from_str::<serde_json::Value>(json) {
                let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
                state.unsupported = Some(value);
                state.unsupported_seq += 1;
            }
        } else if let Some((seq, outcome)) = parse_commit_text_line(trimmed) {
            let wake = {
                let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
                state.commit_text_results.insert(seq, outcome);
                Arc::clone(&state.wake)
            };
            wake.notify();
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
    // caller's thread while a second thread encodes it. SHOTZ (ADR-0027 #6)
    // made the file ~130x smaller (real deflate instead of stored blocks), so
    // the write+MD5 that used to cost ~11 ms over 9.5 MB is now ~0.1 ms over
    // ~80 KB and the encode is the only part left to overlap. `content_hash`
    // is still the MD5 of exactly the bytes in this file.
    let (bytes, md5, encode_us, write_us, hash_us) =
        encode_write_and_hash(frame.width, frame.height, frame.stride, &frame.rgba, &path)?;
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

/// S3a: a bounded, self-describing snapshot of the C1 state at a startup
/// timeout. The earlier record only said `c1_generation=0`, which cannot
/// distinguish "no surface was ever created" (relayout/window path) from "the
/// surface exists and received transactions but no buffer" (draw path) from
/// "a layer was created and destroyed again". The layer list is capped; the
/// counters are exact.
fn c1_state_summary(live: &acos_services::registry::LiveHandles) -> String {
    let state = live.compositor.lock().unwrap_or_else(|e| e.into_inner());
    let mut layers: Vec<String> = state
        .layers()
        .take(8)
        .map(|layer| {
            format!(
                "{}:id{}:tx{}:gen{}:buf{}:hidden{}:placed{}",
                layer.name,
                layer.layer_id,
                layer.transactions,
                layer.latest_generation,
                u8::from(layer.latest.is_some()),
                u8::from(layer.hidden),
                u8::from(layer.placement.is_some())
            )
        })
        .collect();
    let total = state.layers().count();
    if total > layers.len() {
        layers.push(format!("+{} more", total - layers.len()));
    }
    format!(
        "c1_layers={total} c1_transactions={} c1_retained={} c1_destroyed={} layers=[{}]",
        state.transactions_decoded,
        state.retained_buffers(),
        state.destroyed_layers(),
        layers.join(",")
    )
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
        std::env::var_os("P3_TRACE")
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

/// P12: the session's effective animation state (`0` unknown, `1` on, `2` off).
/// Set from the launch option and then corrected by the guest's own probe
/// report, so a reply never claims `animations:off` the guest did not apply.
static ANIMATIONS_STATE: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

fn set_animations_state(on: bool) {
    ANIMATIONS_STATE.store(if on { 1 } else { 2 }, Ordering::Release);
}

fn animations_state() -> Option<&'static str> {
    match ANIMATIONS_STATE.load(Ordering::Acquire) {
        1 => Some("on"),
        2 => Some("off"),
        _ => None,
    }
}

/// MPROC1: one session control op over a session service.
///
/// Only the `intents` module exists today (the capture target); any other
/// `module`/`op` is a structured `UNSUPPORTED_ANDROID_API`, never a silent
/// no-op. `op:"result"` also writes the scripted answer to
/// `<staging>/activity-result.json`, which the guest's result watcher delivers
/// as the framework's own `ActivityResultItem` transaction; the guest's ack is
/// `<staging>/activity-result.ack`.
fn session_ctl(
    request: &serde_json::Value,
    intents: &Arc<std::sync::Mutex<acos_services::intents::IntentState>>,
    staging: &Path,
    secondary: &mut SecondaryPool,
) -> serde_json::Value {
    let module = request.get("module").and_then(|v| v.as_str()).unwrap_or("");
    let op = request.get("op").and_then(|v| v.as_str()).unwrap_or("");
    // MPROC4 step 3b: the launcher's own view of the session's processes (tags,
    // pids, packages, the spawn requests it served). The activity manager's
    // `ProcessRecord` list is reported by the launch result.
    if module == "processes" {
        if op != "list" {
            return serde_json::json!({
                "ok": false, "cmd": "ctl", "module": module, "op": op,
                "code": "BAD_REQUEST",
                "error": format!("processes control op {op:?} does not exist (ops: list)"),
            });
        }
        for record in secondary.reap() {
            secondary.records.push(record);
        }
        let mut body = secondary.to_json();
        body["ok"] = serde_json::json!(true);
        body["cmd"] = serde_json::json!("ctl");
        body["module"] = serde_json::json!(module);
        body["op"] = serde_json::json!(op);
        return body;
    }
    if module != "intents" {
        return serde_json::json!({
            "ok": false, "cmd": "ctl", "code": "UNSUPPORTED_ANDROID_API",
            "module": module, "op": op,
            "error": format!(
                "no session service `{module}` is attached; the control modules this build \
                 serves are `intents` (the intent capture target) and `processes` \
                 (the launcher's process table)"
            ),
        });
    }
    let args = request
        .get("args")
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    let mut state = intents.lock().unwrap_or_else(|e| e.into_inner());
    match op {
        "last" => serde_json::json!({
            "ok": true, "cmd": "ctl", "module": module, "op": op,
            "capture": state.last().map(|c| c.to_json()),
        }),
        "list" => serde_json::json!({
            "ok": true, "cmd": "ctl", "module": module, "op": op,
            "captures": state.list().iter().map(|c| c.to_json()).collect::<Vec<_>>(),
        }),
        "clear" => {
            let cleared = state.clear();
            serde_json::json!({
                "ok": true, "cmd": "ctl", "module": module, "op": op, "cleared": cleared,
            })
        }
        "result" => {
            let str_arg = |key: &str| {
                args.get(key)
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string)
            };
            let capture_seq = args.get("capture_seq").and_then(serde_json::Value::as_u64);
            let result_code = args
                .get("result_code")
                .and_then(serde_json::Value::as_i64)
                .and_then(|v| i32::try_from(v).ok());
            let mut extras = std::collections::BTreeMap::new();
            if let Some(map) = args.get("extras").and_then(serde_json::Value::as_object) {
                for (key, value) in map {
                    if let Some(text) = value.as_str() {
                        extras.insert(key.clone(), text.to_string());
                    } else {
                        return serde_json::json!({
                            "ok": false, "cmd": "ctl", "module": module, "op": op,
                            "code": "BAD_REQUEST",
                            "error": format!("extras.{key} must be a string"),
                        });
                    }
                }
            }
            let scripted = match state.script_result(
                capture_seq,
                result_code,
                str_arg("data"),
                str_arg("mime_type"),
                str_arg("action"),
                extras,
            ) {
                Ok(scripted) => scripted,
                Err(error) => {
                    return serde_json::json!({
                        "ok": false, "cmd": "ctl", "module": module, "op": op,
                        "code": "BAD_REQUEST", "error": error.to_string(),
                    })
                }
            };
            // Hand it to the guest: the result file is the session's control
            // channel (the guest reads only this run's own tree).
            let path = staging.join("activity-result.json");
            let doc = serde_json::json!({
                "capture_seq": scripted.capture_seq,
                "request_code": scripted.request_code,
                "result_code": scripted.result_code,
                "action": scripted.action,
                "data": scripted.data,
                "mime_type": scripted.mime_type,
                "extras": scripted.extras,
            });
            let written = std::fs::write(&path, serde_json::to_vec(&doc).unwrap_or_default());
            // Bound the wait for the guest's ack: a session whose guest is not
            // running must say `delivered:false` instead of claiming a delivery.
            let ack_path = staging.join("activity-result.ack");
            let _ = std::fs::remove_file(&ack_path);
            let deadline = std::time::Instant::now() + Duration::from_millis(2000);
            let mut delivered = false;
            if written.is_ok() {
                while std::time::Instant::now() < deadline {
                    if let Ok(text) = std::fs::read_to_string(&ack_path) {
                        if serde_json::from_str::<serde_json::Value>(&text)
                            .ok()
                            .and_then(|v| v.get("capture_seq").and_then(serde_json::Value::as_u64))
                            == Some(scripted.capture_seq)
                        {
                            delivered = true;
                            break;
                        }
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
            }
            if delivered {
                let _ = std::fs::remove_file(&path);
                state.mark_delivered(scripted.capture_seq, acos_services::intents::now_ms());
            }
            // The reply names the record *after* the delivery mark, so a caller
            // that only reads this reply sees the same `delivered`/`delivered_utc_ms`
            // as a later `op:"last"`.
            let mut result = state
                .list()
                .into_iter()
                .find(|capture| capture.seq == scripted.capture_seq)
                .and_then(|capture| capture.scripted_result.as_ref().map(|r| r.to_json()))
                .unwrap_or_else(|| scripted.to_json());
            result["delivered"] = serde_json::json!(delivered);
            serde_json::json!({
                "ok": true, "cmd": "ctl", "module": module, "op": op,
                "result": result,
                "delivered": delivered,
                "control_file": path.display().to_string(),
                "error": written.err().map(|e| e.to_string()),
            })
        }
        other => serde_json::json!({
            "ok": false, "cmd": "ctl", "module": module, "op": other,
            "code": "BAD_REQUEST",
            "error": format!(
                "intents control op {other:?} does not exist (ops: last, list, clear, result)"
            ),
        }),
    }
}

fn protocol_write(value: &serde_json::Value) {
    use std::io::Write;
    // P12: every reply for this session states the animation scales it ran
    // with. `animations:"off"` is evidence the functional result is not an
    // animation; `"on"` is the default real behaviour.
    let mut value = value.clone();
    let req_id = CURRENT_REQUEST_ID.swap(0, std::sync::atomic::Ordering::AcqRel);
    if let Some(object) = value.as_object_mut() {
        if req_id != 0 {
            object.insert("req_id".into(), serde_json::json!(req_id));
        }
        if let Some(state) = animations_state() {
            object.insert("animations".into(), serde_json::json!(state));
        }
    }
    // VIEW1a: one place turns every reply this session writes into a live-view
    // event (action/lifecycle/activity/settle), so no command path can miss the
    // feed. `observe_reply` is a no-op with no viewer attached.
    if let Some(view) = LIVE_VIEW.get() {
        view.observe_reply(&value);
    }
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{value}");
    let _ = out.flush();
}

/// HUMANVIEW1: correlation id of the serve-loop request currently being
/// handled (`0` when idle or emitting an unsolicited session notice).
/// `protocol_write` swaps it back to `0` and echoes `"req_id"` onto the reply
/// so `ArtSession::command_raw` can discard late replies from earlier timed-out
/// requests instead of mistaking them for confirmation of a compensating cancel.
static CURRENT_REQUEST_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// VIEW1a: the live-view producer of this process, set once by an interactive
/// session. `None` in a one-shot launch (no viewer can attach to a session that
/// has already ended), so nothing changes there.
static LIVE_VIEW: std::sync::OnceLock<Arc<crate::live_view::LiveView>> = std::sync::OnceLock::new();

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
    /// P12b/guard: did the newest C1 capture differ from the pre-action one?
    /// Always reported for a strict input; `None` for fast/tree where there is
    /// no pre-action capture to compare with. Never a failure when the guard is
    /// off (the default since the owner decision).
    pixels_changed: Option<bool>,
    /// ROT8: the step ended with the app's task finished (`window-gone`).
    task_finished: bool,
    /// BACKSETTLE: `(window token, its stored frame number)` when the step bound
    /// on a host-recorded window-forest change (the top window became an
    /// existing window whose composited frame is complete) instead of a draw.
    window_reshown: Option<(u64, u64)>,
}

/// REL3H: the settle `reason` of an input whose every published event the app
/// window acknowledged `finishInputEvent(handled=false)`, with no draw.
///
/// Nothing consumed the input, so there is no new state to settle into; the
/// unchanged, quiet screen the gesture produced **is** the settled observation.
/// Before this the same input was a `TIMEOUT` (`reason: no-draw-after-action`),
/// which charged a delivered no-op the whole settle deadline and reported an
/// honest, stable screen as a failure (REL3E left it open for REL3A's track).
/// The wire string is shared with the verifier, which refuses to infer the end of
/// a list from a gesture nothing consumed (REL3H2): see
/// `acos_protocol::SETTLE_UNHANDLED_INPUT_REASON`. `acos-art` does not depend on
/// `acos-protocol` (it is the guest side of the wire), so the two literals are
/// pinned equal by a test in `acos-cli` (the crate that sees both).
pub const SETTLE_UNHANDLED_INPUT: &str = "unhandled_input";

/// REL3H: did the framework acknowledge **every** published event of this
/// gesture as unhandled?
///
/// The rule is only as strong as the acks behind it, so it needs all of them:
/// one `FINISHED` reply per published `InputMessage` (`acks.len() ==
/// seqs.len()`, each acknowledging a sequence this gesture published) and every
/// one of those replies `handled == false`. A short or missing ack list is
/// **unknown**, never "not handled" (REL3E's rule), and any event the app *did*
/// handle means the app consumed the gesture: both keep the pre-REL3H settle.
fn every_event_acked_unhandled(
    seqs: &[u32],
    acks: &[acos_services::window_session::input_dispatch::InjectedAck],
) -> bool {
    !seqs.is_empty()
        && acks.len() == seqs.len()
        && acks.iter().all(|ack| !ack.handled)
        && seqs
            .iter()
            .all(|seq| acks.iter().any(|ack| ack.seq == *seq))
}

/// REL3H2: the gate that decides whether a "nothing drew" sample may settle an
/// unhandled input.
///
/// REL3H bound the shortcut to whatever the probe had seen by the time the
/// gesture's acks came back, and that sample can be older than the app's own
/// handling of the gesture: the probe is event-driven, so a sample is taken on
/// every dirty draw and on every wake, and the newest one at that moment may
/// have been taken **between the publish and the UI thread running the
/// handler** (or before the handler's posted side effect - a `post()`ed
/// `setText` - ran and drew). Binding it reported "nothing happened" for a
/// gesture that was about to change the screen, and the next assertion then
/// read a state the app had already left. The `expect_change: false`-shaped
/// pass was a false green for exactly the case the shortcut exists for.
///
/// Two rules make the bind honest, and they are the same two the normal strict
/// path already applies:
///
/// * **after the last ack** - the sample must be newer than the probe sequence
///   at the moment the gesture's own acks had all arrived (`ack_floor_seq`).
///   The handler that answered `finishInputEvent(handled=false)` had provably
///   finished before that point, so a sample after it describes the state the
///   handler left behind, not the one it was still working on.
/// * **one confirmation window** - the same `draw_seq` must stay the newest
///   draw for `confirm` (`ACOS_STRICT_SETTLE_CONFIRM_MS`, the strict rule's own
///   window). A side effect that draws inside the window restarts the wait, so
///   the gesture falls through to the normal strict rule and captures the
///   change instead of settling on the pre-change sample.
///
/// A sample can still miss a side effect that draws later than the window; that
/// is the same limit the normal event rule has, and it is why the shortcut
/// never *claims* stillness - it reports what it measured
/// (`pixels_changed` against the caller's pre-action capture), so
/// `expect_change: true` still has to earn its pass.
#[derive(Debug)]
struct UnhandledSettleGate {
    /// The newest probe sequence when the gesture's last ack had arrived.
    ack_floor_seq: u64,
    /// How long the candidate draw must stay the newest one.
    confirm: Duration,
    /// The draw the current window is measured on, and when it started.
    candidate: Option<(u64, Instant)>,
}

/// What [`UnhandledSettleGate`] decided about one probe sample.
#[derive(Debug, PartialEq, Eq)]
enum UnhandledGateOutcome {
    /// Bind this sample: it is after the last ack and its draw stayed newest.
    Bind,
    /// Keep waiting; the string is the settle's `last_reason` (and the guest
    /// trace line), never a user-visible reason on its own.
    Wait(&'static str),
}

impl UnhandledSettleGate {
    fn new(ack_floor_seq: u64, confirm: Duration) -> Self {
        UnhandledSettleGate {
            ack_floor_seq,
            confirm,
            candidate: None,
        }
    }

    /// Forget the current window: a pending signal or a different draw means
    /// the screen is not quiet, so the timer starts again on the next sample.
    fn reset(&mut self) {
        self.candidate = None;
    }

    /// Judge one sample: `Bind` only when it is strictly newer than the last ack
    /// and its draw has stayed the newest for the confirmation window.
    fn observe(&mut self, sample_seq: u64, draw_seq: u64, now: Instant) -> UnhandledGateOutcome {
        if sample_seq <= self.ack_floor_seq {
            // Taken before (or while) the handler that acked the gesture ran:
            // it cannot describe what the handler left behind.
            self.candidate = None;
            return UnhandledGateOutcome::Wait(UNHANDLED_ACK_FLOOR_REASON);
        }
        let restart = match self.candidate {
            Some((draw, _)) => draw != draw_seq,
            None => true,
        };
        if restart {
            self.candidate = Some((draw_seq, now));
        }
        match self.candidate {
            Some((draw, since))
                if draw == draw_seq && now.saturating_duration_since(since) >= self.confirm =>
            {
                UnhandledGateOutcome::Bind
            }
            _ => UnhandledGateOutcome::Wait(UNHANDLED_CONFIRM_REASON),
        }
    }
}

/// BACKSETTLE: the LAYER1 window forest as the host observed it, plus the frame
/// the compositor holds for the top window.
///
/// `order` is the `acos-wm` layer stack (bottom first, the same order
/// composition, input and `assert_windows` read), `top_frame` is the newest
/// buffer the C1 compositor stored for the top window's surface (the frame the
/// host composites), and `generation` is C1's own store counter at that
/// observation.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct WindowForest {
    order: Vec<u64>,
    top_frame: Option<u64>,
    generation: u64,
}

impl WindowForest {
    fn top(&self) -> Option<u64> {
        self.order.last().copied()
    }
}

/// Read the host's window forest: the LAYER1 stack plus the top window's stored
/// frame.
fn window_forest(live: &acos_services::registry::LiveHandles) -> WindowForest {
    let order: Vec<u64> = {
        let wms = live.wms.lock().unwrap_or_else(|e| e.into_inner());
        wms.wm
            .layer_stack()
            .ordered()
            .iter()
            .map(|entry| entry.window)
            .collect()
    };
    let (top_frame, generation) = {
        let state = live.compositor.lock().unwrap_or_else(|e| e.into_inner());
        let top_z = order.len().checked_sub(1).map(|index| index as i32);
        let frame = top_z.and_then(|z| top_window_frame(&state, z));
        (frame, state.latest_generation())
    };
    WindowForest {
        order,
        top_frame,
        generation,
    }
}

/// BACKSETTLE: the newest **complete** buffer stored for the top window's layer
/// subtree - the frame the host composites on top of the display.
///
/// The window's own `SurfaceControl` layer carries the WMS placement
/// (`placement.z` is the LAYER1 stack index) but the pixels arrive on its BLAST
/// child layers, so the frame is looked up in the subtree rather than on the
/// root record alone. A hidden layer is not composited and a layer without a
/// stored buffer has no frame, so `None` here means "no frame the host could
/// have composited" - the placeholder this rule must never bind.
fn top_window_frame(state: &acos_services::surface_client::SurfaceState, top_z: i32) -> Option<u64> {
    let root = state
        .layers()
        .find(|layer| !layer.hidden && layer.placement.map(|placement| placement.z) == Some(top_z))?;
    let mut subtree = vec![root.handle.0];
    // Walk the parent links until no new child appears (BLAST children are
    // direct, a deeper buffer chain is followed too).
    loop {
        let mut grew = false;
        for layer in state.layers() {
            let handle = layer.handle.0;
            if subtree.contains(&handle) {
                continue;
            }
            if layer.parent.is_some_and(|parent| subtree.contains(&parent.0)) {
                subtree.push(handle);
                grew = true;
            }
        }
        if !grew {
            break;
        }
    }
    state
        .layers()
        .filter(|layer| !layer.hidden && subtree.contains(&layer.handle.0))
        .filter_map(|layer| {
            layer.latest.as_ref().and_then(|buffer| {
                (buffer.frame_number > 0 && buffer.width > 0 && buffer.height > 0)
                    .then_some((layer.latest_generation, buffer.frame_number))
            })
        })
        .max_by_key(|(generation, _)| *generation)
        .map(|(_, frame)| frame)
}

/// BACKSETTLE: settle on a host-recorded window-forest change.
///
/// Back can end in a screen transition the app does not draw: the framework
/// finishes the top Activity and the window below - which is still alive, its
/// surface still holding its last drawn frame - becomes the top window again.
/// Nothing in the guest traverses or draws, so the strict rule's
/// "a draw after the action" requirement could only time out
/// (`no-draw-after-action`) on an honest, fully composited screen.
///
/// The observable effect of that action **is** the window-forest change, and it
/// is a host fact the settle can require:
///
/// * the top window changed to a window that **already existed** when the
///   gesture's acks had all arrived (the baseline is taken at the ack, so a
///   change that landed before it can never bind: it is not this action's),
/// * that window's frame is the one the host composites - its surface stored a
///   complete buffer and the layer is visible (no placeholder binds; the
///   ADR-0032 amendment 1 rule still applies unchanged, the guest tree of the
///   now-top window is part of the bind),
/// * the change stays the newest fact for one confirmation window
///   (`ACOS_STRICT_SETTLE_CONFIRM_MS`, the strict rule's own window) **and no
///   newer C1 store landed**: if the app does draw afterwards within the
///   settle window, that newer frame wins and the normal rule binds it,
/// * the guest sample that supplies the tree is newer than the change and is
///   not the empty `window_gone` forest.
///
/// A window this action *created* becoming top is not a re-show: it has no
/// frame yet, and its draw is what the normal strict rule waits for.
#[derive(Debug)]
struct WindowReshownGate {
    baseline: WindowForest,
    confirm: Duration,
    /// `(top window, its frame, when it was first seen)` of the candidate.
    candidate: Option<(u64, u64, Instant)>,
    /// The first host time the changed top window was observed.
    changed_at_ms: Option<u64>,
}

/// What [`WindowReshownGate`] decided about one observation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReshownOutcome {
    /// Bind `(window token, its stored frame number)`.
    Bind(u64, u64),
    /// Keep waiting; the string is the settle trace's `last_reason`.
    Wait(&'static str),
}

impl WindowReshownGate {
    fn new(baseline: WindowForest, confirm: Duration) -> Self {
        WindowReshownGate {
            baseline,
            confirm,
            candidate: None,
            changed_at_ms: None,
        }
    }

    /// Judge the forest now. `None` means the caller must not bind on this
    /// observation and should let the shipped rule decide.
    fn observe(&mut self, forest: &WindowForest, now: Instant, now_ms: u64) -> ReshownOutcome {
        let Some(top) = forest.top() else {
            self.candidate = None;
            return ReshownOutcome::Wait(WINDOW_GONE_REASON);
        };
        if forest == &self.baseline {
            self.candidate = None;
            return ReshownOutcome::Wait(WINDOW_UNCHANGED_REASON);
        }
        if Some(top) == self.baseline.top() {
            // The forest changed below the top (a window was added below, or a
            // sibling went away): the top window did not change, so this is not
            // a re-show.
            self.candidate = None;
            return ReshownOutcome::Wait(WINDOW_SAME_TOP_REASON);
        }
        if !self.baseline.order.contains(&top) {
            // A window that did not exist at the ack became top: a new window,
            // whose draw the normal rule waits for.
            self.candidate = None;
            return ReshownOutcome::Wait(WINDOW_NEW_TOP_REASON);
        }
        let Some(frame) = forest.top_frame else {
            // The re-shown window has no complete stored frame: binding its
            // "frame" would be a placeholder claim.
            self.candidate = None;
            return ReshownOutcome::Wait(WINDOW_NO_FRAME_REASON);
        };
        if forest.generation != self.baseline.generation {
            // The app drew after the action: that newer frame is the settle's
            // frame (the shipped rule binds it), never the re-shown one.
            self.candidate = None;
            return ReshownOutcome::Wait(WINDOW_SUPERSEDED_REASON);
        }
        match self.candidate {
            Some((window, seen_frame, since)) if window == top && seen_frame == frame => {
                if now.saturating_duration_since(since) >= self.confirm {
                    ReshownOutcome::Bind(top, frame)
                } else {
                    ReshownOutcome::Wait(WINDOW_CONFIRM_REASON)
                }
            }
            _ => {
                self.candidate = Some((top, frame, now));
                if self.changed_at_ms.is_none() {
                    self.changed_at_ms = Some(now_ms);
                }
                ReshownOutcome::Wait(WINDOW_CONFIRM_REASON)
            }
        }
    }
}

/// BACKSETTLE: settle trace reasons (never user-visible on their own).
const WINDOW_UNCHANGED_REASON: &str = "window-unchanged";
const WINDOW_SAME_TOP_REASON: &str = "window-same-top";
const WINDOW_NEW_TOP_REASON: &str = "window-new-top";
const WINDOW_NO_FRAME_REASON: &str = "window-frame-missing";
const WINDOW_SUPERSEDED_REASON: &str = "window-reshown-superseded";
const WINDOW_CONFIRM_REASON: &str = "window-reshown-confirm";
const WINDOW_GONE_REASON: &str = "window-forest-empty";

/// REL3H2: settle trace reason - the sample predates the gesture's last ack.
const UNHANDLED_ACK_FLOOR_REASON: &str = "unhandled-ack-floor";
/// REL3H2: settle trace reason - the confirmation window has not elapsed.
const UNHANDLED_CONFIRM_REASON: &str = "unhandled-confirm";

impl SettleTiming {
    fn to_json(&self, settled: bool, ack_ms: u64) -> serde_json::Value {
        let mut value = serde_json::json!({
            "mode": self.mode.as_str(),
            "settled": settled,
            "ack_ms": ack_ms,
            "first_change_ms": self.first_change_ms,
            "frame_ms": self.frame_ms,
            "state_changed": self.state_changed,
            "pixels_changed": self.pixels_changed,
            "bound": self.bound,
            "meaning": "mode=fast frames may be mid-animation; ack_ms is publish -> the gesture's own FINISHED, first_change_ms is publish -> the probe sample that differs from the pre-action tree, frame_ms is publish -> the bound C1 store",
        });
        if self.task_finished {
            if let Some(map) = value.as_object_mut() {
                map.insert("task_finished".into(), serde_json::json!(true));
                map.insert("window_gone".into(), serde_json::json!(true));
            }
        }
        if let Some((token, frame)) = self.window_reshown {
            if let Some(map) = value.as_object_mut() {
                // The token names the window that is on top now; the frame is
                // the one C1 holds for it (the frame the host composites).
                map.insert("window_reshown".into(), serde_json::json!(format!("{token:#x}")));
                map.insert(
                    "window_reshown_frame".into(),
                    serde_json::json!(frame),
                );
            }
        }
        if let Some(rule) = self.rule {
            if let Some(map) = value.as_object_mut() {
                map.insert("strict_rule".into(), serde_json::json!(rule));
                if let Some(reason) = self.reason.as_deref() {
                    map.insert("reason".into(), serde_json::json!(reason));
                    map.insert(
                        "animating".into(),
                        serde_json::json!(reason.contains("anim") || reason.contains("frame")),
                    );
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
fn drain_pending_spawns(
    spawner: Option<(
        &Arc<std::sync::Mutex<std::collections::VecDeque<acos_services::ams::ProcessSpawnRequest>>>,
        &mut SecondaryPool,
    )>,
    probe_state: &Arc<std::sync::Mutex<ProbeState>>,
    compositor: &Arc<std::sync::Mutex<acos_services::surface_client::SurfaceState>>,
) {
    let Some((queue, secondary)) = spawner else {
        return;
    };
    loop {
        let next = queue
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .pop_front();
        let Some(request) = next else {
            break;
        };
        secondary.spawn(&request, probe_state, Arc::clone(compositor));
    }
}

fn wait_for_event_settled(
    child: &mut std::process::Child,
    cgroup: &mut crate::cgroup::RunCgroup,
    live: &acos_services::registry::LiveHandles,
    probe_state: &Arc<std::sync::Mutex<ProbeState>>,
    guest_stdin: &mut Option<GuestProbeChannel>,
    deadline: Instant,
    min_generation: u64,
    min_probe_seq: u64,
    previous_fnv: Option<u64>,
    publish_epoch_ms: u64,
    // G3f1: the tree the caller has already bound. When present, a sample may
    // be the settle's candidate only if the guest's tree differs from it, so
    // the reply is the app's *next* committed state and not the same skeleton
    // the caller already holds.
    await_tree: Option<&serde_json::Value>,
    // REL3H: every event of this gesture came back
    // `finishInputEvent(handled=false)` (see [`every_event_acked_unhandled`]).
    // An input nothing consumed has no state to settle into: a sample whose
    // newest draw predates the publish and that reports no pending work is the
    // settled observation "the app did not handle this", not a timeout.
    // REL3H2: the value is the newest probe sequence **when the gesture's last
    // ack had arrived**; only samples strictly newer than it may settle the
    // shortcut (see [`UnhandledSettleGate`]), so a sample taken before the UI
    // thread ran the handler can never bind. `None` = not an unhandled input.
    unhandled_ack_floor: Option<u64>,
    // REL3H: the newest C1 capture's pixel hash **at publish**, used only when
    // the caller has no pre-action capture of its own (`previous_fnv: None`,
    // e.g. after a `pixels:false` launch). The unhandled-input branch measures
    // `pixels_changed` against it instead of inventing a value.
    entry_fnv: Option<u64>,
    // REL3I3: `(delivered token, removal mark)`. A dialog's Back destroys the
    // window it was delivered to: the app never traverses (its views are
    // unchanged), so no guest draw follows the press, while the composite still
    // changes because a layer went away. The removal is that draw's cause, and
    // the host recorded it for exactly this token.
    //
    // MERGE3I3 precedence: this fact outranks `unhandled_ack_floor`. A removed
    // dialog is *handled* (`source: host_window_removed`) even when every guest
    // ack of the same gesture said `finishInputEvent(handled) = false`; the
    // window it acted on is gone, so it is never reported `INPUT_UNHANDLED`.
    window_removal: Option<(acos_parcel::BinderId, u64)>,
    // BACKSETTLE: the host's window forest at the moment this gesture's acks had
    // all arrived. `Some` only for a strict input action; a top-window change
    // away from that baseline may settle the step as `window_reshown` (see
    // [`WindowReshownGate`]). `None` keeps every other caller byte-identical.
    window_reshown: Option<WindowForest>,
    mut spawner: Option<(
        &Arc<std::sync::Mutex<std::collections::VecDeque<acos_services::ams::ProcessSpawnRequest>>>,
        &mut SecondaryPool,
    )>,
) -> SettleEnd {
    let confirm = Duration::from_millis(env_u64("ACOS_STRICT_SETTLE_CONFIRM_MS", 24));
    // Owner decision (2026-11-12): the pixel-unchanged guard is OFF by default.
    // A strict settle already proves the frame is after the action: a probe
    // newer than the action floor, no pending framework work, a draw at/after
    // the action publish, and the 24 ms confirmation. Requiring the pixels to
    // differ only produced false timeouts for taps that change focus without an
    // IME/cursor. `ACOS_STRICT_SETTLE_PIXEL_GUARD=on` restores P11's guard for
    // experiments; `pixels_changed` is still reported either way.
    let pixel_guard = std::env::var("ACOS_STRICT_SETTLE_PIXEL_GUARD")
        .map(|value| value.eq_ignore_ascii_case("on"))
        .unwrap_or(false);
    if pixel_guard {
        trace_host(
            probe_state,
            "settle: ACOS_STRICT_SETTLE_PIXEL_GUARD=on (P11 experiment; default is off)",
        );
    }
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
    // G3f1: when the caller supplies the tree it already holds, the wait ends on
    // the app's next **draw commit** — a probe sample whose tree differs from
    // the baseline and whose draw has been stored by C1 — instead of requiring
    // the framework to be completely idle. A forever-animating screen (Gaten's
    // welcome timeline) never reports `settled`, but it does commit draws, and a
    // launch's contract is a *fresh* frame, not a settled one (P9). The settle
    // verdict the reply reports stays the guest's own: a sample that is also
    // settled keeps `bound:"strict-event"`, a draw-commit-only sample says
    // `settled:false, bound:"change-draw"`.
    let entry_generation = latest_generation(live);
    let mut last_generation = min_generation;
    let mut last_wake = Instant::now();
    // Host wake pacing. The guest already emits a sample on every dirty draw
    // (P10), so asking it for another one every 20 ms while a screen animates
    // only floods the main Looper with full tree walks; under Gaten's endless
    // welcome timeline that is enough to starve the probe entirely (the
    // `probe-floor` + 12 s `tree_age_ms` symptom). Wake only when the newest
    // sample has not moved since the previous request, and at a lower cadence;
    // an idle screen still gets a sample, a drawing one uses its own events.
    let wake_ms = env_u64("ACOS_STRICT_SETTLE_WAKE_MS", 50);
    let mut last_requested_seq = latest_probe(probe_state)
        .map(|sample| sample.seq)
        .unwrap_or(0);
    // ROT8b: was the window forest already empty when the request began?
    let entry_tree_empty = latest_probe(probe_state)
        .map(|sample| {
            sample
                .tree
                .get("nodes")
                .and_then(|nodes| nodes.as_array())
                .map(|nodes| nodes.is_empty())
                .unwrap_or(false)
        })
        .unwrap_or(false);
    let mut candidate: Option<ProbeSample> = None;
    let mut candidate_since = Instant::now();
    // REL3H2: the unhandled-input shortcut's own window. It is created once, at
    // entry, so the ack floor is a fixed point: samples older than it are never
    // eligible, however long the wait runs.
    let mut unhandled_gate = unhandled_ack_floor.map(|floor| UnhandledSettleGate::new(floor, confirm));
    // BACKSETTLE: the baseline is the forest at the ack, so a change that landed
    // before the action's input ack can never bind (it is not this action's).
    let mut reshown_gate = window_reshown.map(|baseline| WindowReshownGate::new(baseline, confirm));
    let mut first_change_ms: Option<u64> = None;
    let mut last_reason = String::from("no-probe");
    let mut last_pixels_changed: Option<bool> = None;
    // SETTLE5 diagnostic: trace the wait's own decisions (entry floor, every
    // wake it sends, every reason change), so a `probe-floor` timeout can be
    // attributed to a guest that never answered, a wake that never arrived, or
    // a sample the host ignored. Trace-only (`ACOS_ART_INPUT_TRACE`); no reply
    // changes.
    let mut traced_reason = String::new();
    // BACKSETTLE: the window-forest observation's own trace line follows the same
    // rule (only when its decision changes), so a `window-reshown` bind and the
    // draw that superseded it are both readable in the session trace.
    let mut traced_forest_reason = String::new();
    trace_host(
        probe_state,
        &format!(
            "settle entry floor={entry_floor} gen={entry_generation} \
             deadline_ms={} await_tree={} pending={:?}",
            deadline.saturating_duration_since(Instant::now()).as_millis(),
            await_tree.is_some(),
            latest_probe(probe_state)
                .map(|s| s.pending.clone())
                .unwrap_or_default()
        ),
    );
    let entry_wake = request_guest_probe(guest_stdin, probe_state);
    trace_host(
        probe_state,
        &format!(
            "settle wake probe_seq={entry_floor} reason=entry written={entry_wake}"
        ),
    );
    loop {
        drain_pending_spawns(
            spawner.as_mut().map(|(q, s)| (*q, &mut **s)),
            probe_state,
            &live.compositor,
        );
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
        // The delivered window is gone: the removal is this step's draw (see
        // `window_removal`), so a sample drawn before the publish is still the
        // post-action state — the screen changed because a window was removed,
        // not because the app drew.
        let removal_binds = window_removal
            .is_some_and(|(token, mark)| live.input.window_removed_since(token, mark));
        // BACKSETTLE: judge the host's window forest once per observation. The
        // gate is only built for an input action whose acks had all arrived
        // before the wait began (`window_reshown`), so a tree/launch wait keeps
        // its shipped semantics exactly.
        let forest = match reshown_gate {
            Some(_) => window_forest(live),
            None => WindowForest::default(),
        };
        let reshown = reshown_gate
            .as_mut()
            .map(|gate| gate.observe(&forest, Instant::now(), host_ms()));
        let reshown_changed_at = reshown_gate
            .as_ref()
            .and_then(|gate| gate.changed_at_ms)
            .unwrap_or(u64::MAX);
        if let Some(outcome) = reshown {
            let reason = match outcome {
                ReshownOutcome::Bind(token, frame) => format!("bind {token:#x}/{frame}"),
                ReshownOutcome::Wait(reason) => reason.to_string(),
            };
            if reason != traced_forest_reason {
                traced_forest_reason = reason.clone();
                trace_host(
                    probe_state,
                    &format!(
                        "settle forest reason={reason} top={} frame={:?} gen={} base_top={} \
                         base_gen={} changed_at={}",
                        forest
                            .top()
                            .map(|token| format!("{token:#x}"))
                            .unwrap_or_else(|| "none".into()),
                        forest.top_frame,
                        forest.generation,
                        reshown_gate
                            .as_ref()
                            .and_then(|gate| gate.baseline.top())
                            .map(|token| format!("{token:#x}"))
                            .unwrap_or_else(|| "none".into()),
                        reshown_gate
                            .as_ref()
                            .map(|gate| gate.baseline.generation)
                            .unwrap_or(0),
                        reshown_changed_at,
                    ),
                );
            }
        }
        let mut valid: Option<ProbeSample> = None;
        match &probe {
            None => last_reason = "no-probe".to_string(),
            Some(sample) => {
                if sample.seq <= entry_floor {
                    last_reason = "probe-floor".to_string();
                } else if window_gone_shape(sample) && !host_confirms_task_finished(live) {
                    // ROT8b: the guest reports an empty settled forest, but the
                    // host ATMS still has a live record or a pending start (a
                    // `finish()` + `startActivity(Home)` leaves exactly this gap
                    // before Home's launch transaction runs). That is not a
                    // finished task: keep waiting (bounded by the deadline) and
                    // settle on the next Activity's draw.
                    last_reason = "window-gone-unconfirmed".to_string();
                } else if window_gone_shape(sample) {
                    // ROT8: the app's task is finished (last Activity
                    // destroyed, no window root) and the HOST ATMS agrees (no
                    // live record, no pending start). Nothing draws, so this
                    // binds on the state proof, not on a draw.
                    let timing = SettleTiming {
                        mode: SettleMode::Strict,
                        first_change_ms: Some(sample.at_ms.saturating_sub(publish_epoch_ms)),
                        frame_ms: None,
                        state_changed: Some(window_gone_state_changed(
                            await_tree,
                            entry_tree_empty,
                        )),
                        bound: "window-gone",
                        rule: Some("event"),
                        reason: None,
                        pixels_changed: Some(false),
                        task_finished: true,
                        window_reshown: None,
                    };
                    trace_host(
                        probe_state,
                        &format!(
                            "settle window-gone ready gen={generation} probe_seq={}",
                            sample.seq
                        ),
                    );
                    return SettleEnd::Settled(Settled {
                        probe: sample.clone(),
                        generation,
                        timing,
                    });
                } else if await_tree
                    .map(|baseline| &sample.tree == baseline)
                    .unwrap_or(false)
                {
                    // G3f1: the app has not committed a new tree state yet
                    // (the caller still holds exactly this tree). Keep waiting;
                    // the guest's own dirty-draw events drive the next sample.
                    last_reason = "tree-unchanged".to_string();
                } else if await_tree.is_some() {
                    // G3f1: the tree changed; it is a valid content bind once the
                    // draw behind it has been committed to C1 (a store at/after
                    // request entry). A confirmation window is deliberately NOT
                    // required here: on a screen that keeps animating (Gaten's
                    // welcome timeline) the draw changes every frame, so a
                    // "same draw stayed newest" rule can never converge — that is
                    // the settle rule, which this wait replaces by the *change*
                    // rule. The bind is labelled `change-draw` and its settle
                    // verdict stays `settled:false`, so nothing claims a quiet
                    // screen.
                    if probe_usable_for_fast_tree(&sample, entry_generation, entry_floor) {
                        let bound = if sample.settled {
                            "strict-event"
                        } else {
                            "change-draw"
                        };
                        let timing = SettleTiming {
                            mode: SettleMode::Strict,
                            first_change_ms: Some(sample.at_ms.saturating_sub(publish_epoch_ms)),
                            frame_ms: None,
                            state_changed: Some(true),
                            bound,
                            rule: Some("event"),
                            reason: None,
                            pixels_changed: None,
                            task_finished: false,
                            window_reshown: None,
                        };
                        trace_host(
                            probe_state,
                            &format!(
                                "settle change ready gen={generation} probe_seq={} draw_seq={} \
                                 bound={bound} settled={}",
                                sample.seq, sample.draw_seq, sample.settled
                            ),
                        );
                        return SettleEnd::Settled(Settled {
                            probe: sample.clone(),
                            generation,
                            timing,
                        });
                    }
                    last_reason = "no-store-after-change".to_string();
                } else if !sample.settled {
                    last_reason = sample
                        .pending
                        .first()
                        .cloned()
                        .unwrap_or_else(|| "unsettled".to_string());
                } else if let Some((token, frame)) =
                    reshown_bind(reshown, sample, reshown_changed_at)
                {
                    // BACKSETTLE: this action's observable effect is the host's
                    // own window-forest fact (the top window became an existing
                    // window whose frame is complete), and the app has not drawn
                    // since. Bind that state, with the guest tree of the now-top
                    // window, instead of charging the action a draw it never
                    // owed. When the gate does not bind, the shipped arms below
                    // decide exactly as before.
                    //
                    // `pixels_changed` is measured against the caller's
                    // pre-action capture exactly like the shipped rule measures
                    // it, so `expect_change: true` still has to earn its pass on
                    // this bind (`false` fails it).
                    let pixels_changed = match previous_fnv {
                        None => true,
                        Some(previous) => {
                            let current = {
                                let state =
                                    live.compositor.lock().unwrap_or_else(|e| e.into_inner());
                                state.capture_latest().map(|c| c.fnv1a)
                            };
                            current != Some(previous) && current.is_some()
                        }
                    };
                    let timing = SettleTiming {
                        mode: SettleMode::Strict,
                        first_change_ms: Some(sample.at_ms.saturating_sub(publish_epoch_ms)),
                        frame_ms: None,
                        state_changed: None,
                        bound: "window-reshown",
                        rule: Some("event"),
                        reason: None,
                        pixels_changed: Some(pixels_changed),
                        task_finished: false,
                        window_reshown: Some((token, frame)),
                    };
                    trace_host(
                        probe_state,
                        &format!(
                            "settle window-reshown ready gen={generation} probe_seq={} \
                             window={token:#x} frame={frame}",
                            sample.seq
                        ),
                    );
                    return SettleEnd::Settled(Settled {
                        probe: sample.clone(),
                        generation,
                        timing,
                    });
                } else if publish_epoch_ms != 0
                    && sample.draw_ms < publish_epoch_ms
                    && !removal_binds
                {
                    // MERGE3I3 precedence: `!removal_binds` is the first guard
                    // of this arm on purpose. When the host recorded the removal
                    // of the very window this input was delivered to (after the
                    // publish), that removal **is** the step's draw, so the
                    // sample falls through to the normal strict rule and binds
                    // with `bound: "host-window-removal"` below. The gesture was
                    // handled by definition (a removed dialog consumed the Back),
                    // so it can never be the `INPUT_UNHANDLED` shortcut - even
                    // when every guest ack of the same gesture said
                    // `finishInputEvent(handled) = false` because the app's
                    // `InputEventReceiver` was disposed before it could answer.
                    // REL3H: no draw landed since the action *and* the framework
                    // acknowledged every event as unhandled. Nothing consumed
                    // the input, so the app has no new state to settle into: the
                    // screen this sample describes **is** the settled outcome.
                    // Reporting this as `TIMEOUT no-draw-after-action` (the
                    // pre-REL3H rule) charged a delivered, provably-unhandled
                    // tap the whole settle deadline and called an honest, stable
                    // screen a failure.
                    // REL3H2: the shortcut binds only a sample that is newer
                    // than the gesture's last ack (so the handler had finished)
                    // and whose draw then stayed newest for one confirmation
                    // window (so a side effect that draws is caught and the
                    // gesture falls through to the normal rule below). Anything
                    // else keeps waiting and the strict rule applies.
                    let gate_outcome = unhandled_gate.as_mut().map(|gate| {
                        gate.observe(sample.seq, sample.draw_seq, Instant::now())
                    });
                    if gate_outcome == Some(UnhandledGateOutcome::Bind) {
                        // `pixels_changed` is measured exactly like the normal
                        // strict path measures it (newest C1 capture against the
                        // caller's pre-action one), never assumed: it is what
                        // `expect_change: true` judges, so an unhandled tap must
                        // fail that claim on a measured `false` rather than a
                        // hard-coded one. No pre-action capture (a tree request)
                        // keeps the established conservative `true` = no claim.
                        let pixels_changed = match previous_fnv.or(entry_fnv) {
                            None => true,
                            Some(previous) => {
                                let current = {
                                    let state = live
                                        .compositor
                                        .lock()
                                        .unwrap_or_else(|e| e.into_inner());
                                    state.capture_latest().map(|c| c.fnv1a)
                                };
                                current != Some(previous) && current.is_some()
                            }
                        };
                        let timing = SettleTiming {
                            mode: SettleMode::Strict,
                            first_change_ms: None,
                            frame_ms: None,
                            state_changed: None,
                            bound: "unhandled-input",
                            rule: Some("event"),
                            reason: Some(SETTLE_UNHANDLED_INPUT.to_string()),
                            pixels_changed: Some(pixels_changed),
                            task_finished: false,
                            window_reshown: None,
                        };
                        trace_host(
                            probe_state,
                            &format!(
                                "settle unhandled-input ready gen={generation} probe_seq={} \
                                 draw_seq={}",
                                sample.seq, sample.draw_seq
                            ),
                        );
                        return SettleEnd::Settled(Settled {
                            probe: sample.clone(),
                            generation,
                            timing,
                        });
                    }
                    last_reason = match gate_outcome {
                        Some(UnhandledGateOutcome::Wait(reason)) => reason.to_string(),
                        // No gate at all: this input was consumed (or its acks
                        // are incomplete), so a missing draw is a timeout.
                        _ => "no-draw-after-action".to_string(),
                    };
                } else if sample.generation < generation {
                    last_reason = "newer-store".to_string();
                } else {
                    valid = Some(sample.clone());
                }
            }
        }
        if let Some(sample) = valid {
            let pixels_changed = match previous_fnv {
                // No pre-action capture to compare with (e.g. tree request or
                // the first action): report the conservative `true`; no pixel
                // claim is made.
                None => true,
                Some(previous) => {
                    let current = {
                        let state = live.compositor.lock().unwrap_or_else(|e| e.into_inner());
                        state.capture_latest().map(|c| c.fnv1a)
                    };
                    current != Some(previous) && current.is_some()
                }
            };
            last_pixels_changed = Some(pixels_changed);
            if pixel_guard && !pixels_changed {
                last_reason = "pixels-unchanged".to_string();
                candidate = None;
            } else {
                let restart = match &candidate {
                    None => true,
                    Some(current) => {
                        sample.draw_seq != current.draw_seq || sample.seq < current.seq
                    }
                };
                if restart {
                    candidate = Some(sample.clone());
                    candidate_since = Instant::now();
                    if first_change_ms.is_none() && publish_epoch_ms != 0 {
                        first_change_ms = Some(host_ms().saturating_sub(publish_epoch_ms));
                    }
                }
                if candidate
                    .as_ref()
                    .map(|current| sample.draw_seq == current.draw_seq)
                    .unwrap_or(false)
                    && candidate_since.elapsed() >= confirm
                    && sample.generation == generation
                {
                    let bound = if removal_binds {
                        // REL3I3: the screen changed because the window this
                        // input was delivered to was removed (a dismissed
                        // dialog). Named, so no reader mistakes it for a guest
                        // draw.
                        "host-window-removal"
                    } else if sample.settled {
                        "strict-event"
                    } else {
                        "change-draw"
                    };
                    let timing = SettleTiming {
                        mode: SettleMode::Strict,
                        first_change_ms,
                        frame_ms: if publish_epoch_ms == 0 {
                            None
                        } else {
                            Some(host_ms().saturating_sub(publish_epoch_ms))
                        },
                        state_changed: None,
                        bound,
                        rule: Some("event"),
                        reason: None,
                        pixels_changed: last_pixels_changed,
                        task_finished: false,
                        window_reshown: None,
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
            // REL3H2: the same for the unhandled-input shortcut: pending work
            // (a posted side effect waiting to run) or a new draw restarts it.
            if let Some(gate) = unhandled_gate.as_mut() {
                if !sample.settled || sample.draw_seq != gate.candidate.map(|(draw, _)| draw).unwrap_or(sample.draw_seq) {
                    gate.reset();
                }
            }
        }
        // An idle screen emits nothing, so a bounded wake keeps it observable;
        // a drawing screen's own dirty events are the request. Never re-ask for
        // a sample that already arrived since the last request — but *do*
        // record that it arrived: otherwise `last_requested_seq` stays stuck at
        // the pre-request sample and the host stops asking forever. The 250 ms
        // guest fallback used to hide that bug; H4's 5 s fallback exposes it
        // (a type/tap leaves the main queue pending and strict settle needs a
        // fresh sample after the queue drains).
        let current_probe_seq = latest_probe(probe_state)
            .map(|sample| sample.seq)
            .unwrap_or(last_requested_seq);
        if current_probe_seq > last_requested_seq {
            last_requested_seq = current_probe_seq;
            last_wake = Instant::now();
        }
        if last_wake.elapsed() >= Duration::from_millis(wake_ms)
            && current_probe_seq == last_requested_seq
        {
            let written = request_guest_probe(guest_stdin, probe_state);
            trace_host(
                probe_state,
                &format!(
                    "settle wake probe_seq={current_probe_seq} reason=pacing written={written}"
                ),
            );
            last_requested_seq = current_probe_seq;
            last_wake = Instant::now();
        }
        if last_reason != traced_reason {
            traced_reason = last_reason.clone();
            let (seq, age, draw_seq, settled) = match &probe {
                Some(sample) => (
                    sample.seq,
                    host_ms().saturating_sub(sample.at_ms),
                    sample.draw_seq,
                    sample.settled,
                ),
                None => (0, 0, 0, false),
            };
            trace_host(
                probe_state,
                &format!(
                    "settle wait reason={last_reason} probe_seq={seq} age_ms={age} \
                     draw_seq={draw_seq} settled={settled} floor={entry_floor}"
                ),
            );
        }
        if Instant::now() >= deadline {
            // A timeout must still state the pixel fact (true|false), even when
            // no candidate sample was valid. Recompute against the pre-action
            // capture; a tree request has no pre-action capture and reports
            // `true` (no pixel claim).
            let pixels_changed = last_pixels_changed.unwrap_or_else(|| match previous_fnv {
                None => true,
                Some(previous) => {
                    let current = {
                        let state = live.compositor.lock().unwrap_or_else(|e| e.into_inner());
                        state.capture_latest().map(|c| c.fnv1a)
                    };
                    current != Some(previous) && current.is_some()
                }
            });
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
                    pixels_changed: Some(pixels_changed),
                    task_finished: false,
                    window_reshown: None,
                },
            };
        }
        cgroup.sample();
        // PERF4: the strict rule keeps its own cadence. It binds on timestamps
        // plus a 150 ms quiet window and a 24 ms confirmation window, so a 10 ms
        // observation tick is at most 1 % of a ~950 ms strict wait (M8 strict is
        // flat: 949.3 ms vs 951.9 ms over 5 reps), while an eager wake here cost
        // 20 points of active CPU in the 20-tap charge cohort (65.8 % vs 45.0 %,
        // `04-ab/ab-compare.txt`): every binder transaction of an animating
        // screen re-reads the tree. The eager wait is used where it pays — the
        // fast rule in [`wait_for_settled_observation`].
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// P12: strict settle is the P11 event rule, with no alternative. `fast` keeps
/// its own P1 rule and its documented quiet fallback (`quiet-fallback`) when no
/// tree change is observed inside `ACOSLAUNCH_FAST_WINDOW_MS`; that fallback is
/// part of `fast` and is not a strict mode.
#[allow(clippy::too_many_arguments)]
fn wait_for_settled_observation(
    child: &mut std::process::Child,
    cgroup: &mut crate::cgroup::RunCgroup,
    live: &acos_services::registry::LiveHandles,
    probe_state: &Arc<std::sync::Mutex<ProbeState>>,
    guest_stdin: &mut Option<GuestProbeChannel>,
    deadline: Instant,
    min_generation: u64,
    min_probe_seq: u64,
    previous_fnv: Option<u64>,
    mode: SettleMode,
    baseline_probe: Option<ProbeSample>,
    publish_epoch_ms: u64,
    // G3f1: a caller holding a known tree (the launch's window skeleton) waits
    // for the *next* state instead of polling; `None` keeps every existing
    // caller byte-identical.
    await_tree: Option<&serde_json::Value>,
    // REL3H/REL3H2: the gesture's own acks said nothing consumed it (input
    // verbs only; every tree/launch caller passes `None`), with the probe
    // sequence that was current when the last ack arrived, and the pixel hash the
    // unhandled branch measures against when the caller has no capture of its own.
    unhandled_ack_floor: Option<u64>,
    entry_fnv: Option<u64>,
    // REL3I3: `(delivered token, removal mark)` for an input action. When the
    // window this input went to is removed during the wait, the removal is the
    // draw (the composite changes with no guest traversal), so the step may bind
    // it; `None` keeps the strict draw requirement for every other caller.
    // Precedence over the unhandled shortcut: see [`wait_for_event_settled`].
    window_removal: Option<(acos_parcel::BinderId, u64)>,
    // BACKSETTLE: see [`wait_for_event_settled`].
    window_reshown: Option<WindowForest>,
    mut spawner: Option<(
        &Arc<std::sync::Mutex<std::collections::VecDeque<acos_services::ams::ProcessSpawnRequest>>>,
        &mut SecondaryPool,
    )>,
) -> SettleEnd {
    if mode == SettleMode::Strict {
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
            await_tree,
            unhandled_ack_floor,
            entry_fnv,
            window_removal,
            window_reshown,
            spawner,
        );
    }
    // ---- fast (P1): first probe tree that differs from the pre-action baseline.
    // Its 150 ms fallback below only runs when the fast window expired with no
    // observed change; it is labelled `quiet-fallback`, never `strict-quiet`.
    let quiet = Duration::from_millis(150);
    let fast_window = Duration::from_millis(env_u64("ACOSLAUNCH_FAST_WINDOW_MS", 100));
    let fast_deadline = Instant::now() + fast_window;
    let mut fast_fell_back = false;
    let mut last_generation = min_generation;
    let mut last_change = Instant::now();
    let mut last_change_ms = host_ms();
    let mut first_change_probe: Option<ProbeSample> = None;
    let (unsupported_baseline, _) = latest_unsupported(probe_state);
    let mut last_wake = Instant::now();
    // PERF4: the session's own change signal (see [`wait_for_change`]) and the
    // tick this loop has already consumed. Signalled by the guest-line reader for
    // a sample and by the peer loop for every applied transaction (the C1 store).
    let wake = settle_wake(probe_state);
    let mut seen = 0u64;
    loop {
        drain_pending_spawns(
            spawner.as_mut().map(|(q, s)| (*q, &mut **s)),
            probe_state,
            &live.compositor,
        );
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
        if !probe_after
            && last_change.elapsed() >= quiet
            && last_wake.elapsed() >= Duration::from_millis(20)
        {
            request_guest_probe(guest_stdin, probe_state);
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
        // The commit store: the C1 buffer the sample is bound to must be at
        // least as new as the state the sample's tree describes.
        //
        // The shipped rule is *observation order* — a store the host saw after
        // the sample (`last_change_ms > commit.at_ms`). That is satisfied by
        // whichever store happens to be noticed next, and with the fixed poll a
        // build that notices the sample's own store in time has to wait for the
        // **following** frame: measured, the same loop resolves `frame_ms` 16 or
        // 29 depending on which side of a poll tick the store landed
        // (diagnosis/tap-split-*.txt).
        //
        // PERF4 adds the precise form of the same fact, from what the sample
        // reports: `generation` is C1's store counter when the host parsed the
        // sample and `draw_seq` is the guest's `ViewTreeObserver.OnDraw` counter.
        // When they are **equal** every draw so far has been stored and nothing
        // else was, so store #draw_seq is the buffer of the very draw this tree
        // was taken at — the tree and the frame describe one UI state, which is
        // the property the observation-order rule approximates. It can never bind
        // an earlier sample: it requires a draw newer than the pre-publish one,
        // with its own buffer already in C1.
        //
        // Anything the equality cannot prove (a store without a draw, a draw
        // whose buffer has not landed, no draw counter on the sample) falls back
        // to the shipped rule exactly.
        let own_draw_stored = match &first_change_probe {
            Some(commit) => {
                sample_own_draw_is_stored(commit, baseline_probe.as_ref(), last_change_ms)
            }
            None => false,
        };
        let committed_store = first_change_probe
            .as_ref()
            .map(|commit| last_change_ms > commit.at_ms || own_draw_stored)
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
                pixels_changed: None,
                task_finished: false,
                window_reshown: None,
            };
            trace_host(
                probe_state,
                &format!(
                    "settle fast ready gen={generation} probe_seq={} first_change_ms={} frame_ms={} \
                     commit_gen={} draw_seq={} own_draw={}",
                    commit.seq,
                    first_change_ms.unwrap_or(0),
                    timing.frame_ms.unwrap_or(0),
                    commit.generation,
                    commit.draw_seq,
                    own_draw_stored
                ),
            );
            return SettleEnd::Settled(Settled {
                probe: commit,
                generation,
                timing,
            });
        }
        let in_fast_window =
            mode == SettleMode::Fast && !fast_fell_back && Instant::now() < fast_deadline;
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
                    let timing = SettleTiming {
                        mode: SettleMode::Fast,
                        first_change_ms,
                        frame_ms: frame_ms(last_change_ms),
                        state_changed: Some(false),
                        bound: "quiet-fallback",
                        rule: None,
                        reason: None,
                        pixels_changed: None,
                        task_finished: false,
                        window_reshown: None,
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
                    state_changed: Some(false),
                    bound: "timeout",
                    rule: None,
                    reason: None,
                    pixels_changed: None,
                    task_finished: false,
                    window_reshown: None,
                },
            };
        }
        cgroup.sample();
        // PERF4: block on the session's own change signal instead of a 15 ms
        // sleep. The sleep quantized every fast settle to its period, so a
        // sub-millisecond difference between builds moved M8 by a whole step
        // (the same build resolved `frame_ms` 16 or 29 depending on phase).
        // Unlike the strict loop this one is on the action path the gate
        // measures, and its iterations are cheap (one probe read, one store
        // counter), so the eager wake is a win here.
        wait_for_change(&wake, &mut seen);
    }
}

/// PERF4: does the store the fast settle is about to bind hold the buffer of the
/// very draw this sample's tree was taken at?
///
/// The shipped rule is *observation order*: a C1 store the host saw **after**
/// the sample. That is satisfied by whichever store is noticed next, and because
/// a draw's own buffer is stored before the probe sample describing it is
/// parsed, it always binds the *following* frame. Measured: the same loop
/// resolves `frame_ms` 16 or 29 ms depending on which side of a poll tick the
/// store landed (`results/handoff/PERF4/diagnosis/tap-split-*.txt`).
///
/// The sample already reports the two facts that make the precise statement
/// possible: `generation` is C1's store counter when the host parsed it and
/// `draw_seq` is the guest's `ViewTreeObserver.OnDraw` counter. All three must
/// hold, or the caller keeps the shipped rule:
///
/// * `generation >= draw_seq` — every draw so far has been stored (a session may
///   store more than it draws, never fewer), so the sample's own draw has a
///   buffer in C1;
/// * `last_change_ms >= draw_ms` — the store the host holds arrived after that
///   draw's own wall clock (`draw_ms` comes from the same clock), so it is not a
///   pre-draw buffer left behind;
/// * `draw_seq > baseline.draw_seq` (or no baseline) — the sample's draw is newer
///   than the pre-publish one, so the tree cannot predate the gesture.
///
/// With all three the tree and the bound frame describe one UI state, which is
/// the property the observation-order rule approximates; nothing here can bind a
/// sample taken before the action.
fn sample_own_draw_is_stored(
    commit: &ProbeSample,
    baseline: Option<&ProbeSample>,
    last_change_ms: u64,
) -> bool {
    commit.draw_seq > 0
        && commit.generation >= commit.draw_seq
        && last_change_ms >= commit.draw_ms
        && baseline
            .map(|b| commit.draw_seq > b.draw_seq)
            .unwrap_or(true)
}

/// Is this probe sample usable for a fast tree request that started when C1 was
/// at `entry_generation` and after probe sequence `min_probe_seq`?
///
/// This is P3b's whole invalidation rule. A sample is fresh when the C1 store
/// counter it was parsed at is at least the generation current when the request
/// began (so no frame landed between the sample and the request) and its guest
/// sequence is past the caller's floor. Nothing about a 150 ms quiet window is
/// involved: that is the strict rule, unchanged.
/// ROT8: a sample proves the task finished only when the guest says so AND it
/// is internally consistent: settled, with an empty window forest.
/// BACKSETTLE: `true` when the sample's tree has no node at all (an empty
/// window forest). A `window_reshown` bind needs the tree of the now-top window,
/// so an empty forest is never its evidence.
fn tree_is_empty(tree: &serde_json::Value) -> bool {
    tree.get("nodes")
        .and_then(|nodes| nodes.as_array())
        .map(|nodes| nodes.is_empty())
        .unwrap_or(true)
}

/// BACKSETTLE: may this observation bind as `window-reshown`?
///
/// The gate decides the forest fact (a stable change to an existing top window
/// with a complete composited frame); this adds the observation half: the sample
/// must be **settled** (the guest has no pending traversal, so its tree is the
/// state the screen is in), newer than the change, and its tree must be the one
/// of the now-top window (non-empty, not a finished task). Anything less keeps
/// the shipped rule in charge: an unsettled sample is a screen in motion, never
/// evidence that this action's effect is complete.
fn reshown_bind(
    outcome: Option<ReshownOutcome>,
    sample: &ProbeSample,
    changed_at_ms: u64,
) -> Option<(u64, u64)> {
    match outcome {
        Some(ReshownOutcome::Bind(token, frame))
            if sample.settled
                && sample.at_ms >= changed_at_ms
                && !sample.window_gone
                && !tree_is_empty(&sample.tree) =>
        {
            Some((token, frame))
        }
        _ => None,
    }
}

fn window_gone_shape(sample: &ProbeSample) -> bool {
    sample.window_gone
        && sample.settled
        && sample
            .tree
            .get("nodes")
            .and_then(|nodes| nodes.as_array())
            .map(|nodes| nodes.is_empty())
            .unwrap_or(false)
}

/// ROT8b: the host ATMS confirms the task is finished (no live record, no
/// pending start). The guest's own flag is only a claim until this holds.
fn host_confirms_task_finished(live: &acos_services::registry::LiveHandles) -> bool {
    live.atms
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .task_finished_proof()
}

/// ROT8b: did the step change state? The tree the caller held before the
/// action is the baseline: an already-empty forest (`await_tree` with no
/// nodes, or no tree at all and an empty screen before the action) is NOT a
/// change; a non-empty previous tree is.
fn window_gone_state_changed(await_tree: Option<&serde_json::Value>, entry_empty: bool) -> bool {
    match await_tree {
        Some(tree) => tree
            .get("nodes")
            .and_then(|nodes| nodes.as_array())
            .map(|nodes| !nodes.is_empty())
            .unwrap_or(true),
        None => !entry_empty,
    }
}

fn probe_usable_for_fast_tree(
    probe: &ProbeSample,
    entry_generation: u64,
    min_probe_seq: u64,
) -> bool {
    probe.seq > min_probe_seq && probe.generation >= entry_generation
}

/// The demand-probe channel: the guest reads `PROBE` lines from fd 0.
///
/// * `Pipe` — the exec path: the guest process's own stdin.
/// * `Fifo` — a zygote session (Z4/ZPROD): the keeper's stdin is `/dev/null`,
///   so the forked child `System.setIn`s `<session>/probe.fifo` instead
///   (`ZygoteApp.z4ProbeStdin`), and this is the writer half.
enum GuestProbeIo {
    Pipe(std::process::ChildStdin),
    Fifo(std::fs::File),
}

pub(crate) struct GuestProbeChannel {
    io: GuestProbeIo,
    /// Set when a deadline-bounded write wrote a partial line (without its
    /// trailing `\n`). The next write prepends `!\n` so the guest terminates
    /// and rejects the incomplete frame before reading the next command.
    needs_resync: bool,
}

impl GuestProbeChannel {
    pub(crate) fn pipe(writer: std::process::ChildStdin) -> Self {
        Self {
            io: GuestProbeIo::Pipe(writer),
            needs_resync: false,
        }
    }

    pub(crate) fn fifo(writer: std::fs::File) -> Self {
        Self {
            io: GuestProbeIo::Fifo(writer),
            needs_resync: false,
        }
    }

    fn as_raw_fd(&self) -> RawFd {
        use std::os::unix::io::AsRawFd;
        match &self.io {
            GuestProbeIo::Pipe(writer) => writer.as_raw_fd(),
            GuestProbeIo::Fifo(writer) => writer.as_raw_fd(),
        }
    }

    /// Write a single `\n`-terminated control command before `deadline` using
    /// non-blocking `write(2)` + `poll(2)`. If an earlier write timed out or
    /// failed mid-line, prepends `!\n` so the guest discards the truncated
    /// line and receives this command intact.
    ///
    /// On error, returns `(payload_wrote_any, err)` where `payload_wrote_any`
    /// is true only if at least one byte of `line` itself reached the kernel
    /// pipe/FIFO buffer (excluding any leading `!\n` resync marker).
    fn write_line_until(
        &mut self,
        line: &[u8],
        deadline: Instant,
    ) -> std::result::Result<(), (bool, std::io::Error)> {
        debug_assert!(
            line.ends_with(b"\n"),
            "GuestProbeChannel::write_line_until expects a newline-terminated command"
        );
        let prefix_len = if self.needs_resync { 2 } else { 0 };
        let res = if prefix_len > 0 {
            let mut framed = Vec::with_capacity(prefix_len + line.len());
            framed.extend_from_slice(b"!\n");
            framed.extend_from_slice(line);
            write_fd_all_until(self.as_raw_fd(), &framed, deadline)
        } else {
            write_fd_all_until(self.as_raw_fd(), line, deadline)
        };
        match res {
            Ok(()) => {
                self.needs_resync = false;
                Ok(())
            }
            Err((written, err)) => {
                if written >= prefix_len {
                    let payload_written = written - prefix_len;
                    self.needs_resync = payload_written > 0;
                    Err((payload_written > 0, err))
                } else {
                    // Did not finish writing the `!\n` resync marker: keep
                    // resync pending and report 0 bytes of `line` written.
                    self.needs_resync = true;
                    Err((false, err))
                }
            }
        }
    }
}

fn write_fd_all_until(
    fd: RawFd,
    bytes: &[u8],
    deadline: Instant,
) -> std::result::Result<(), (usize, std::io::Error)> {
    let orig_flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if orig_flags < 0 {
        return Err((0, std::io::Error::last_os_error()));
    }
    let added_nonblock = (orig_flags & libc::O_NONBLOCK) == 0;
    if added_nonblock
        && unsafe { libc::fcntl(fd, libc::F_SETFL, orig_flags | libc::O_NONBLOCK) } != 0
    {
        return Err((0, std::io::Error::last_os_error()));
    }
    struct RestoreFlags {
        fd: RawFd,
        orig_flags: libc::c_int,
        restore: bool,
    }
    impl Drop for RestoreFlags {
        fn drop(&mut self) {
            if self.restore {
                unsafe {
                    libc::fcntl(self.fd, libc::F_SETFL, self.orig_flags);
                }
            }
        }
    }
    let _guard = RestoreFlags {
        fd,
        orig_flags,
        restore: added_nonblock,
    };

    let mut written = 0usize;
    while written < bytes.len() {
        if Instant::now() >= deadline {
            return Err((
                written,
                std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "timed out before writing to guest control channel",
                ),
            ));
        }
        let n = unsafe {
            libc::write(
                fd,
                bytes[written..].as_ptr().cast(),
                bytes.len() - written,
            )
        };
        if n > 0 {
            written += n as usize;
            continue;
        }
        if n == 0 {
            return Err((
                written,
                std::io::Error::new(
                    std::io::ErrorKind::WriteZero,
                    "write returned 0 bytes on guest control channel",
                ),
            ));
        }
        let err = std::io::Error::last_os_error();
        if err.kind() == std::io::ErrorKind::Interrupted {
            continue;
        }
        if err.kind() != std::io::ErrorKind::WouldBlock {
            return Err((written, err));
        }
        let now = Instant::now();
        if now >= deadline {
            return Err((
                written,
                std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "timed out writing to guest control channel",
                ),
            ));
        }
        let rem_ms = (deadline - now).as_millis().clamp(1, i32::MAX as u128) as i32;
        let mut pfd = libc::pollfd {
            fd,
            events: libc::POLLOUT,
            revents: 0,
        };
        let rc = unsafe { libc::poll(&mut pfd, 1, rem_ms) };
        if rc < 0 {
            let perr = std::io::Error::last_os_error();
            if perr.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err((written, perr));
        }
        if rc == 0 {
            return Err((
                written,
                std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "timed out waiting for guest control channel writability",
                ),
            ));
        }
        if (pfd.revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL)) != 0 {
            return Err((
                written,
                std::io::Error::new(
                    std::io::ErrorKind::BrokenPipe,
                    format!(
                        "guest control channel closed (poll revents=0x{:x})",
                        pfd.revents
                    ),
                ),
            ));
        }
    }
    Ok(())
}

impl std::io::Write for GuestProbeChannel {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if self.needs_resync {
            match &mut self.io {
                GuestProbeIo::Pipe(writer) => writer.write_all(b"!\n")?,
                GuestProbeIo::Fifo(writer) => writer.write_all(b"!\n")?,
            }
            self.needs_resync = false;
        }
        match &mut self.io {
            GuestProbeIo::Pipe(writer) => writer.write(buf),
            GuestProbeIo::Fifo(writer) => writer.write(buf),
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        match &mut self.io {
            GuestProbeIo::Pipe(writer) => writer.flush(),
            GuestProbeIo::Fifo(writer) => writer.flush(),
        }
    }
}

/// The FIFO name a zygote child opens for its demand-probe channel. Kept here
/// so the guest source and this writer cannot drift (`ZygoteApp.z4ProbeStdin`).
pub(crate) const GUEST_PROBE_FIFO: &str = "probe.fifo";

/// Create (or reset) and open a session's probe FIFO for the host writer.
///
/// Read+write, exactly like the keeper's `cmd` FIFO: the child's blocking
/// `open` never waits on this side, and a reader that goes away cannot turn
/// the next wake into `EPIPE` (this process is its own reader).
fn open_guest_probe_fifo(dir: &Path) -> Result<std::fs::File> {
    fs::create_dir_all(dir)?;
    let path = dir.join(GUEST_PROBE_FIFO);
    let _ = fs::remove_file(&path);
    let c = std::ffi::CString::new(path.to_string_lossy().to_string())
        .map_err(|e| Error::Layout(e.to_string()))?;
    if unsafe { libc::mkfifo(c.as_ptr(), 0o600) } != 0 {
        return Err(Error::Io(std::io::Error::last_os_error()));
    }
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&path)
        .map_err(Error::Io)
}

/// Ask the guest for one probe sample on its main looper, out of its 50 ms
/// cadence. The guest reads `PROBE` lines from stdin (interactive sessions only)
/// and wakes the probe thread immediately; this never changes the view tree.
///
/// Returns whether the wake was actually written. SETTLE5: a wake that cannot
/// be written (a closed guest stdin, a dead reader) must not be silent — the
/// caller traces it, and the demand-probe channel's health is part of the
/// timeout evidence instead of a guess.
fn request_guest_probe(
    stdin: &mut Option<GuestProbeChannel>,
    state: &Arc<std::sync::Mutex<ProbeState>>,
) -> bool {
    let Some(writer) = stdin.as_mut() else {
        return true;
    };
    let written = writer
        .write_line_until(b"PROBE\n", Instant::now() + Duration::from_millis(100))
        .is_ok();
    // SETTLE5: the channel's health is evidence, and a wake the pipe refuses is
    // counted rather than silently swallowed. Only the counters change; the
    // demand probe's behaviour is unchanged.
    let mut guard = state.lock().unwrap_or_else(|e| e.into_inner());
    if written {
        guard.wakes_sent = guard.wakes_sent.saturating_add(1);
    } else {
        guard.wake_failures = guard.wake_failures.saturating_add(1);
    }
    drop(guard);
    written
}

fn parse_commit_text_line(trimmed: &str) -> Option<(u64, CommitTextOutcome)> {
    let json = trimmed.strip_prefix(COMMIT_TEXT_PREFIX)?;
    let value = serde_json::from_str::<serde_json::Value>(json).ok()?;
    let seq = value.get("seq").and_then(|v| v.as_u64())?;
    let outcome = CommitTextOutcome {
        ok: value.get("ok").and_then(|v| v.as_bool()).unwrap_or(false),
        handled: value.get("handled").and_then(|v| v.as_bool()).unwrap_or(false),
        no_focused_editor: value
            .get("no_focused_editor")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        commit_attempted: value
            .get("commit_attempted")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        focus_class: value
            .get("focus_class")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        error: value
            .get("error")
            .and_then(|v| v.as_str())
            .map(str::to_string),
    };
    Some((seq, outcome))
}

fn base64_encode(input: &[u8]) -> String {
    const TABLE: &[u8; 64] =
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b0 = u32::from(chunk[0]);
        let b1 = if chunk.len() > 1 {
            u32::from(chunk[1])
        } else {
            0
        };
        let b2 = if chunk.len() > 2 {
            u32::from(chunk[2])
        } else {
            0
        };
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(TABLE[((n >> 18) & 0x3f) as usize] as char);
        out.push(TABLE[((n >> 12) & 0x3f) as usize] as char);
        out.push(if chunk.len() > 1 {
            TABLE[((n >> 6) & 0x3f) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            TABLE[(n & 0x3f) as usize] as char
        } else {
            '='
        });
    }
    out
}

#[derive(Debug, PartialEq, Eq)]
enum GuestCommitResult {
    Committed {
        seq: u64,
        handled: bool,
        latency_us: u64,
        #[allow(dead_code)]
        focus_class: Option<String>,
    },
    NoFocusedEditor {
        focus_class: Option<String>,
        error: String,
    },
    Rejected {
        focus_class: Option<String>,
        error: String,
    },
    /// Proven non-delivery before sending `COMMIT_TEXT` to the guest (safe to
    /// fall back to `live.input.type_text` for ASCII).
    Unavailable {
        reason: String,
    },
    /// `COMMIT_TEXT` was written (or partially written) to the guest, or
    /// `InputConnection.commitText` threw after entry, so delivery is
    /// uncertain. Never replay the text through `live.input.type_text`, which
    /// could double-insert into the editor.
    Uncertain {
        code: &'static str,
        reason: String,
    },
}

fn classify_commit_text_outcome(
    seq: u64,
    outcome: CommitTextOutcome,
    latency_us: u64,
) -> GuestCommitResult {
    if outcome.ok {
        return GuestCommitResult::Committed {
            seq,
            handled: outcome.handled,
            latency_us,
            focus_class: outcome.focus_class,
        };
    }
    if outcome.commit_attempted {
        return GuestCommitResult::Uncertain {
            code: "INPUT_UNAVAILABLE",
            reason: outcome.error.unwrap_or_else(|| {
                "InputConnection.commitText threw after entry (delivery uncertain; not replaying)"
                    .to_string()
            }),
        };
    }
    if outcome.no_focused_editor {
        return GuestCommitResult::NoFocusedEditor {
            focus_class: outcome.focus_class,
            error: outcome
                .error
                .unwrap_or_else(|| "no focused InputConnection".to_string()),
        };
    }
    GuestCommitResult::Rejected {
        focus_class: outcome.focus_class,
        error: outcome
            .error
            .unwrap_or_else(|| "InputConnection.commitText returned false".to_string()),
    }
}

fn format_commit_text_frame(seq: u64, text: &str) -> String {
    let encoded = base64_encode(text.as_bytes());
    format!("COMMIT_TEXT {seq} {} {encoded} END\n", encoded.len())
}

fn try_guest_commit_text(
    child: &mut std::process::Child,
    live: &acos_services::registry::LiveHandles,
    probe_state: &Arc<std::sync::Mutex<ProbeState>>,
    guest_stdin: &mut Option<GuestProbeChannel>,
    text: &str,
    ack_wait: Duration,
) -> GuestCommitResult {
    let foreground = live.top_process.load(std::sync::atomic::Ordering::Relaxed);
    if foreground != 0 {
        return GuestCommitResult::Unavailable {
            reason: format!(
                "foreground secondary process {foreground} has no stdin control pipe for InputConnection.commitText"
            ),
        };
    }
    let Some(writer) = guest_stdin.as_mut() else {
        return GuestCommitResult::Unavailable {
            reason: "guest has no stdin control pipe for InputConnection.commitText".to_string(),
        };
    };
    let seq = {
        let mut state = probe_state.lock().unwrap_or_else(|e| e.into_inner());
        state.next_commit_text_seq = state.next_commit_text_seq.saturating_add(1);
        state.next_commit_text_seq
    };
    let started = Instant::now();
    let timeout = ack_wait
        .max(Duration::from_millis(2000))
        .min(Duration::from_secs(5));
    let deadline = started + timeout;
    let payload = format_commit_text_frame(seq, text);
    if let Err((wrote_any, err)) = writer.write_line_until(payload.as_bytes(), deadline) {
        if !wrote_any {
            return GuestCommitResult::Unavailable {
                reason: format!("COMMIT_TEXT not written to guest stdin: {err}"),
            };
        }
        let code = if err.kind() == std::io::ErrorKind::TimedOut {
            "TIMEOUT"
        } else {
            "INPUT_UNAVAILABLE"
        };
        return GuestCommitResult::Uncertain {
            code,
            reason: format!(
                "failed writing COMMIT_TEXT to guest (partial=true, delivery uncertain): {err}"
            ),
        };
    }
    let wake = settle_wake(probe_state);
    let mut seen = 0u64;
    loop {
        {
            let mut state = probe_state.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(outcome) = state.commit_text_results.remove(&seq) {
                return classify_commit_text_outcome(
                    seq,
                    outcome,
                    started.elapsed().as_micros() as u64,
                );
            }
        }
        if let Ok(Some(status)) = child.try_wait() {
            return GuestCommitResult::Uncertain {
                code: "INPUT_UNAVAILABLE",
                reason: format!("guest exited before acknowledging COMMIT_TEXT (seq={seq}): {status}"),
            };
        }
        if Instant::now() >= deadline {
            let cancel = format!("CANCEL_COMMIT_TEXT {seq}\n");
            let _ = writer.write_line_until(
                cancel.as_bytes(),
                Instant::now() + Duration::from_millis(100),
            );
            return GuestCommitResult::Uncertain {
                code: "TIMEOUT",
                reason: format!(
                    "timed out waiting for guest InputConnection.commitText ack (seq={seq}); sent CANCEL_COMMIT_TEXT and not replaying"
                ),
            };
        }
        wait_for_change(&wake, &mut seen);
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
    guest_stdin: &mut Option<GuestProbeChannel>,
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
            request_guest_probe(guest_stdin, probe_state);
            trace_host(probe_state, "fast-tree requested an immediate guest probe");
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

/// S3PERF: how long the startup loop waits for a live C1 frame after the guest's
/// `activity-resumed` marker reported a **skipped first traversal**.
///
/// A skipped first traversal is the *normal* path: ACOS does not set
/// `ADD_FLAG_APP_VISIBLE`, so `ViewRootImpl`'s first traversal sees
/// `mAppVisible=false` and skips the draw; the host's oneway
/// `dispatchAppVisibility(true)` arrives afterwards and the app draws on the
/// next frame (F+1). The guest therefore publishes readiness exactly as main
/// does (no draw wait, no forced traversal) and reports the skip as evidence
/// (`draw_skip=`, `first_traversal=`) in its marker. Only a skip that never
/// produces a frame is the S3a failure, and it is bounded here instead of by
/// the 60 s no-frame budget: two 60 Hz frames plus slack, and never longer
/// than the guest's own `sResumeReadyMs` (2000 ms). A launch still requires
/// `c1_generation > 0`, so a skipped draw is never certified ready.
const SKIPPED_TRAVERSAL_NO_FRAME_BUDGET: Duration = Duration::from_millis(2000);

/// S3PERF: the bounded no-frame budget for a marker that reported a skipped
/// first traversal. Factored out of the startup loop so the decision is
/// testable without a guest.
#[derive(Debug)]
struct SkippedTraversalFrameBudget {
    budget: Duration,
    since: Option<Instant>,
}

impl SkippedTraversalFrameBudget {
    fn new(budget: Duration) -> Self {
        Self {
            budget,
            since: None,
        }
    }

    /// Has the budget expired? A live C1 frame (`generation > 0`) always wins:
    /// the marker's skip is then just the normal pre-visibility traversal and
    /// the launch is ready. Without a frame, the clock starts at the first
    /// observation of the skip and expires `budget` later.
    fn expired(&mut self, at: Instant, skip: Option<&str>, generation: u64) -> bool {
        if skip.is_none() || generation > 0 {
            self.since = None;
            return false;
        }
        let since = *self.since.get_or_insert(at);
        at.saturating_duration_since(since) >= self.budget
    }
}

/// S3PERF: the ready-line failure for a skipped first traversal that never
/// produced a live frame. Factored out of the loop so a test asserts the code
/// and its evidence without a guest.
fn skipped_traversal_no_frame_reply(
    reason: &str,
    first_traversal: &str,
    generation: u64,
    c1: &str,
) -> serde_json::Value {
    serde_json::json!({
        "ok": false, "cmd": "ready", "code": "POST_RESUME_NOT_READY",
        "retryable": false,
        "draw_skip": reason,
        "first_traversal": first_traversal,
        "error": format!(
            "the first traversal skipped its draw (draw_skip={reason}) and no live frame \
             arrived within {}ms: activity_resumed=true c1_generation={generation} \
             first_traversal={first_traversal} {c1}",
            SKIPPED_TRAVERSAL_NO_FRAME_BUDGET.as_millis()
        ),
        "capture_source": CAPTURE_SOURCE,
    })
}

/// H4: the interactive session's idle wake sources.
///
/// The old loop woke every 100 ms to call `try_wait`, the watchdog and the
/// session deadline. That was 10 wakeups/s per idle session for events that are
/// all observable without polling: stdin has a reader thread, child exit has a
/// `waitid(WNOWAIT)` watcher, and the watchdog kills the guest (which is the
/// child-exit event). The only genuine deadline is the 900 s session cap.
#[derive(Debug)]
enum ServeEvent {
    /// One command line from stdin (`None` = EOF).
    Line(Option<String>),
    /// The guest process has exited (it is still waitable: `WNOWAIT`).
    ChildExited,
    /// MPROC4 step 3b: the peer resolved a component of a package that needs a
    /// process. The launcher owns the processes, so the request is handled on
    /// this thread like any other session event.
    SpawnWake,
}

/// VIEW1a: the live-view frame source.
///
/// One composite of the C1 state per call, and only when the compositor's store
/// counter advanced since the last capture: an unchanged screen costs one lock
/// and one integer compare. The capture is a **copy**, never a present —
/// `frame_seq`, `generation`, damage and the settle state are untouched
/// (ADR-0027). `LiveView` calls this only while a subscriber is attached.
/// The live-view frame source: the newest composited C1 frame, or a blank
/// placeholder while a `FLAG_SECURE` window is on screen.
///
/// VIEW1c: `FLAG_SECURE` is the window manager's flag (the guest sets
/// `LayoutParams.flags`, the host's `WmsState` publishes it on `relayout`), so the
/// compositor knows it and the stream must honour it: a secure window's pixels
/// never leave the process. The frame is still published (the viewer sees the
/// screen change and that it is secure) with **all-zero pixels** and a smaller
/// placeholder size, and the `F` header carries `"secure": true`.
fn live_view_source(
    compositor: Arc<std::sync::Mutex<acos_services::surface_client::SurfaceState>>,
    attach_epoch: Arc<std::sync::OnceLock<Arc<std::sync::atomic::AtomicU64>>>,
) -> Arc<dyn Fn() -> Option<crate::live_view::SourceFrame> + Send + Sync> {
    let last_seq = std::sync::atomic::AtomicU64::new(0);
    let last_epoch = std::sync::atomic::AtomicU64::new(0);
    Arc::new(move || {
        let state = compositor.lock().unwrap_or_else(|e| e.into_inner());
        let seq = state.latest_generation();
        let previous = last_seq.load(std::sync::atomic::Ordering::Relaxed);
        let cur_epoch = attach_epoch
            .get()
            .map(|a| a.load(std::sync::atomic::Ordering::Acquire))
            .unwrap_or(0);
        let prev_epoch = last_epoch.load(std::sync::atomic::Ordering::Relaxed);
        if seq == 0 || (seq <= previous && cur_epoch == prev_epoch) {
            return None;
        }
        if state.has_secure_window() {
            // A blank frame, not the app's pixels. 1/8 of the display keeps the
            // placeholder cheap; the encoded size is what the header reports.
            let (width, height) = state
                .newest_frame_layer()
                .map(|(_, buffer)| (buffer.width / 8, buffer.height / 8))
                .unwrap_or((135, 300));
            let (width, height) = (width.max(1), height.max(1));
            last_seq.store(seq, std::sync::atomic::Ordering::Relaxed);
            last_epoch.store(cur_epoch, std::sync::atomic::Ordering::Relaxed);
            return Some(crate::live_view::SourceFrame {
                seq,
                generation: seq,
                width,
                height,
                stride: width,
                rgba: vec![0u8; width as usize * height as usize * 4],
                secure: true,
            });
        }
        let capture = state.capture_latest()?;
        last_seq.store(seq, std::sync::atomic::Ordering::Relaxed);
        last_epoch.store(cur_epoch, std::sync::atomic::Ordering::Relaxed);
        Some(crate::live_view::SourceFrame {
            seq,
            generation: capture.generation,
            width: capture.width,
            height: capture.height,
            stride: capture.stride,
            rgba: capture.rgba,
            secure: false,
        })
    })
}

/// VIEW1a: the tap thread's wake, on the condvar the guest's probe samples
/// already signal. No timer of its own: an unviewed session has no tap thread at
/// all (`LiveView` starts it with the first subscriber and stops it with the
/// last), so the viewer costs the session nothing when nobody watches.
struct ProbeTapWake {
    wake: Arc<ProbeWake>,
    seen: std::sync::Mutex<u64>,
}

impl crate::live_view::FrameWake for ProbeTapWake {
    fn wait(&self, timeout: std::time::Duration) {
        let mut seen = self.seen.lock().unwrap_or_else(|e| e.into_inner());
        self.wake.wait(&mut seen, timeout);
    }
}

/// Tracks the pinned window and active pointer wire IDs for interactive
/// `panel_touch` gestures so retried `ACTION_UP`/`ACTION_CANCEL`/`ACTION_POINTER_UP`
/// frames never re-hit-test a different window or fail with `NoTouchTarget` after
/// the gesture or pointer has already ended.
#[derive(Debug, Default)]
struct PanelTouchState {
    target: Option<(acos_parcel::BinderId, (i32, i32))>,
    active_pointers: std::collections::HashSet<i32>,
}

#[derive(Debug, PartialEq, Eq)]
enum PanelTouchOutcome {
    Dispatched,
    TargetClosed,
    AlreadyEnded,
}

#[derive(Debug)]
struct PanelTouchError {
    err: acos_services::input_channel::ChannelError,
    published: bool,
}

impl PanelTouchState {
    #[cfg(test)]
    fn dispatch(
        &mut self,
        input: &acos_services::window_session::input_dispatch::InputHost,
        action: i32,
        down_time: i64,
        event_time: i64,
        pointers: &[(i32, f32, f32)],
        touch_wait: Duration,
    ) -> std::result::Result<PanelTouchOutcome, PanelTouchError> {
        self.dispatch_with_hit_point(input, action, down_time, event_time, pointers, None, touch_wait)
    }

    fn dispatch_with_hit_point(
        &mut self,
        input: &acos_services::window_session::input_dispatch::InputHost,
        action: i32,
        down_time: i64,
        event_time: i64,
        pointers: &[(i32, f32, f32)],
        hit_point: Option<(f32, f32)>,
        touch_wait: Duration,
    ) -> std::result::Result<PanelTouchOutcome, PanelTouchError> {
        let masked_action = action & 0xff;
        let pointer_index = ((action >> 8) & 0xff) as usize;
        match masked_action {
            acos_services::input_channel::ACTION_DOWN => {
                self.target = None;
                self.active_pointers.clear();
                match input.touch_frame_checked_with_hit_point(
                    action,
                    &mut self.target,
                    hit_point,
                    down_time,
                    event_time,
                    pointers,
                    touch_wait,
                ) {
                    Ok((token, origin, _)) => {
                        self.target = Some((token, origin));
                        for &(id, _, _) in pointers {
                            self.active_pointers.insert(id);
                        }
                        Ok(PanelTouchOutcome::Dispatched)
                    }
                    Err((err, published)) => {
                        if published && self.target.is_some() {
                            for &(id, _, _) in pointers {
                                self.active_pointers.insert(id);
                            }
                        }
                        Err(PanelTouchError { err, published })
                    }
                }
            }
            acos_services::input_channel::ACTION_POINTER_DOWN => {
                if self.target.is_none() {
                    return Err(PanelTouchError {
                        err: acos_services::input_channel::ChannelError::NoTouchTarget(
                            "panel_touch pointer_down without an active touch target".to_string(),
                        ),
                        published: false,
                    });
                }
                let ptr_id = pointers.get(pointer_index).map(|p| p.0);
                match input.touch_frame_checked(
                    action,
                    &mut self.target,
                    down_time,
                    event_time,
                    pointers,
                    touch_wait,
                ) {
                    Ok((token, origin, _)) => {
                        self.target = Some((token, origin));
                        if let Some(id) = ptr_id {
                            self.active_pointers.insert(id);
                        }
                        Ok(PanelTouchOutcome::Dispatched)
                    }
                    Err((err, published)) => {
                        if published && self.target.is_some() {
                            if let Some(id) = ptr_id {
                                self.active_pointers.insert(id);
                            }
                        }
                        Err(PanelTouchError { err, published })
                    }
                }
            }
            acos_services::input_channel::ACTION_POINTER_UP => {
                let ptr_id = pointers.get(pointer_index).map(|p| p.0);
                if self.target.is_none()
                    || ptr_id.is_some_and(|id| !self.active_pointers.contains(&id))
                {
                    return Ok(PanelTouchOutcome::AlreadyEnded);
                }
                match input.touch_frame_checked(
                    action,
                    &mut self.target,
                    down_time,
                    event_time,
                    pointers,
                    touch_wait,
                ) {
                    Ok((token, origin, _)) => {
                        self.target = Some((token, origin));
                        if let Some(id) = ptr_id {
                            self.active_pointers.remove(&id);
                        }
                        Ok(PanelTouchOutcome::Dispatched)
                    }
                    Err((err, published)) => {
                        let target_dead = self.target.is_some_and(|(token, _)| {
                            matches!(
                                err,
                                acos_services::input_channel::ChannelError::PeerClosed
                            ) || !input.has_channel(token)
                        });
                        if target_dead {
                            if let Some(id) = ptr_id {
                                self.active_pointers.remove(&id);
                            }
                            Ok(PanelTouchOutcome::TargetClosed)
                        } else {
                            Err(PanelTouchError { err, published })
                        }
                    }
                }
            }
            acos_services::input_channel::ACTION_UP
            | acos_services::input_channel::ACTION_CANCEL => {
                if self.target.is_none() {
                    self.active_pointers.clear();
                    return Ok(PanelTouchOutcome::AlreadyEnded);
                }
                match input.touch_frame_checked(
                    action,
                    &mut self.target,
                    down_time,
                    event_time,
                    pointers,
                    touch_wait,
                ) {
                    Ok(_) => {
                        self.target = None;
                        self.active_pointers.clear();
                        Ok(PanelTouchOutcome::Dispatched)
                    }
                    Err((err, published)) => {
                        let target_dead = self.target.is_some_and(|(token, _)| {
                            matches!(
                                err,
                                acos_services::input_channel::ChannelError::PeerClosed
                            ) || !input.has_channel(token)
                        });
                        if target_dead {
                            self.target = None;
                            self.active_pointers.clear();
                            Ok(PanelTouchOutcome::TargetClosed)
                        } else {
                            Err(PanelTouchError { err, published })
                        }
                    }
                }
            }
            _ => {
                if self.target.is_none() {
                    return Err(PanelTouchError {
                        err: acos_services::input_channel::ChannelError::NoTouchTarget(
                            "panel_touch frame without an active touch target".to_string(),
                        ),
                        published: false,
                    });
                }
                input
                    .touch_frame_checked(
                        action,
                        &mut self.target,
                        down_time,
                        event_time,
                        pointers,
                        touch_wait,
                    )
                    .map(|(token, origin, _)| {
                        self.target = Some((token, origin));
                        PanelTouchOutcome::Dispatched
                    })
                    .map_err(|(err, published)| PanelTouchError { err, published })
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn interactive_serve(
    density_dpi: u32,
    boot_image: &crate::boot_image::BootImageReport,
    child: &mut std::process::Child,
    guest_pid: i32,
    cgroup: &mut crate::cgroup::RunCgroup,
    live: &acos_services::registry::LiveHandles,
    probe_state: &Arc<std::sync::Mutex<ProbeState>>,
    probe_fifo: Option<std::fs::File>,
    started: Instant,
    startup_budget: Duration,
    frames_dir: &Path,
    staging: &Path,
    out: &Path,
    package: &str,
    activity: Option<&str>,
    dog: Option<&MemoryWatchdog>,
    logd: &Logd,
    // VIEW1a/VIEW1c: the **host-only** live-view directory (0700, never
    // bind-mounted into the guest, unlike the session's `/dev/socket`
    // directory); the live-view stream socket `view.sock` is bound there.
    sockets: &Path,
    // ADR-0029 item 4: the attached modules at launch (ready line), and the
    // channel to the peer thread for live attach/detach.
    modules: &[serde_json::Value],
    has_home: bool,
    control: &ServiceControl,
    // MPROC4 step 3b: the session's secondary processes, and the spawn requests
    // the peer thread hands over.
    secondary: &mut SecondaryPool,
    spawn_rx: mpsc::Receiver<acos_services::ams::ProcessSpawnRequest>,
    // MPROC5c: how long a lazily spawned process may hold nothing before it is
    // killed (stock cached-process policy); `0` disables the kill.
    idle_kill: Duration,
) -> std::io::Result<SessionEnd> {
    let mut evidence: Vec<serde_json::Value> = Vec::new();
    let mut captures = 0u64;
    // P3b: the guest's stdin is the probe-request channel (`PROBE`), so a fast
    // tree can wake the probe thread instead of waiting for its 50 ms tick.
    // Interactive sessions are spawned with a piped stdin; non-interactive ones
    // have none and the request is a no-op.
    // ZPROD3b: a zygote session's probe channel is its own FIFO (the stub's
    // stdin is a dead end); every other session keeps the guest's own pipe.
    let mut guest_stdin = match probe_fifo {
        Some(fifo) => Some(GuestProbeChannel::fifo(fifo)),
        None => child.stdin.take().map(GuestProbeChannel::pipe),
    };
    // CR1FIX #20: WINDOW commands written to the guest so far, and the guest's
    // failed-command count already reported to the host.
    let mut window_cmds_sent: u64 = 0;
    let mut window_failed_seen: u64 = 0;
    let startup_deadline = started + startup_budget;
    // P9: the drain thread signals this on every parsed probe sample, so the
    // loop below is woken by the guest instead of polling on a fixed 10 ms
    // sleep. `wake_seen` is the tick this loop has already consumed.
    let wake = Arc::clone(&probe_state.lock().unwrap_or_else(|e| e.into_inner()).wake);
    let mut wake_seen = 0u64;
    // S3PERF: the marker may report a skipped first traversal. That is evidence
    // (the normal F -> F+1 path), not a gate; the budget only bounds the case
    // where no frame ever follows it.
    let mut skipped_frame = SkippedTraversalFrameBudget::new(SKIPPED_TRAVERSAL_NO_FRAME_BUDGET);
    let mut marker_text: Option<String> = None;
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
        let marker_path = staging.join("activity-resumed.marker");
        let marker = marker_path.is_file();
        // Read the marker once it carries the evidence field (a partial read is
        // retried next tick; the field is written last, in one file write).
        if marker && marker_text.is_none() {
            if let Ok(text) = fs::read_to_string(&marker_path) {
                if marker_field(&text, "draw_skip").is_some() {
                    marker_text = Some(text);
                }
            }
        }
        let probe = latest_probe(probe_state);
        let generation = latest_generation(live);
        if marker && probe.is_some() && generation > 0 {
            break;
        }
        // S3a/S3PERF: the guest reported a skipped first traversal and no live
        // frame has arrived. Two frames is generous for the host's oneway
        // `dispatchAppVisibility(true)`; past it, fail fast and loudly with the
        // guest's own reason instead of waiting out the 60 s no-frame budget.
        let skip = marker_text
            .as_deref()
            .and_then(|text| resumed_marker_draw_skip(text));
        if marker && skipped_frame.expired(Instant::now(), skip.as_deref(), generation) {
            let reason = skip.as_deref().unwrap_or("none");
            let first = marker_text
                .as_deref()
                .and_then(|text| marker_field(text, "first_traversal"))
                .unwrap_or("unknown");
            let c1 = c1_state_summary(live);
            kill_guest(child, cgroup);
            // A distinct reason string, not `none`: this is a skipped draw that
            // never produced a frame, so the caller can tell it from a generic
            // no-frame timeout and from a clean main-parity launch.
            let _ = fs::write(
                staging.join("activity-not-resumed.marker"),
                format!(
                    "package={package}\nreason=POST_RESUME_NOT_READY\ndraw_skip={reason}\n\
                     first_traversal={first}\ntimeout_ms={}\n",
                    SKIPPED_TRAVERSAL_NO_FRAME_BUDGET.as_millis()
                ),
            );
            let reply = skipped_traversal_no_frame_reply(reason, first, generation, &c1);
            protocol_write(&reply);
            evidence.push(reply);
            write_session_evidence(out, &evidence);
            return Ok(SessionEnd {
                status: None,
                clean: false,
            });
        }
        if Instant::now() >= startup_deadline {
            // S3a evidence: the old line named only `c1_generation=0`. A
            // timeout now carries the C1 layer/transaction state and the
            // newest probe age, so the next occurrence says which side never
            // produced a frame. A *reported skipped draw* never reaches this
            // 60 s budget: it fails earlier with `POST_RESUME_NOT_READY` and
            // `draw_skip=` (SKIPPED_TRAVERSAL_NO_FRAME_BUDGET).
            let c1 = c1_state_summary(live);
            let probe_facts = match probe.as_ref() {
                Some(p) => format!(
                    "probe_seq={} probe_age_ms={} probe_gen={} probe_draw_seq={}",
                    p.seq,
                    host_ms().saturating_sub(p.at_ms),
                    p.generation,
                    p.draw_seq
                ),
                None => "probe_seq=none".to_string(),
            };
            kill_guest(child, cgroup);
            let reply = serde_json::json!({
                "ok": false, "cmd": "ready", "code": "NO_LIVE_FRAME",
                "error": format!(
                    "no live frame within {}s: activity_resumed={marker} probe={} c1_generation={generation} {probe_facts} {c1}",
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
    // VIEW1a: bind the per-session live-view stream socket (protocol v1) and
    // start the frame tap. A viewer is optional: a bind failure is a log line,
    // never a failed session, and `stream_socket` is then absent from the ready
    // line. With no subscriber the tap does one atomic load per probe wake and
    // nothing else — no capture, no copy, no encode.
    let attach_epoch_slot = Arc::new(std::sync::OnceLock::new());
    let mut view_config = crate::live_view::LiveViewConfig::from_env();
    view_config.has_home = Some(has_home);
    let view = match crate::live_view::LiveView::bind(
        &sockets.join("view.sock"),
        view_config,
        (width, height),
        std::process::id(),
        live_view_source(Arc::clone(&live.compositor), Arc::clone(&attach_epoch_slot)),
        Arc::new(ProbeTapWake {
            wake: Arc::clone(&wake),
            seen: std::sync::Mutex::new(0),
        }),
    ) {
        Ok(view) => {
            let _ = attach_epoch_slot.set(view.attach_epoch());
            let _ = LIVE_VIEW.set(Arc::clone(&view));
            Some(view)
        }
        Err(error) => {
            eprintln!("acos-art: live view: cannot bind the stream socket: {error}");
            None
        }
    };
    let stream_socket = match &view {
        Some(view) => serde_json::json!(view.socket_path().display().to_string()),
        None => serde_json::Value::Null,
    };
    let ready = serde_json::json!({
        "ok": true,
        "cmd": "ready",
        "modules": modules,
        "has_home": has_home,
        "package": package,
        "activity": activity.unwrap_or(""),
        // VIEW1a: the live-view stream socket (`docs/LIVE_VIEW.md`), null when
        // it could not be bound.
        "stream_socket": stream_socket,
        "width": width,
        "height": height,
        // The density the guest actually booted at (`--density`, or the
        // reference default) — not a constant, or a phone-matched run reports
        // the emulator's 420 while running at 400.
        "density": density_dpi,
        "capture_source": CAPTURE_SOURCE,
        "buffer_generation": latest_generation(live),
        "probe_seq": probe.seq,
        // H13b: the boot-image mode and cache outcome of *this* session, so a
        // serve's launch reply can report a warm cache without a second probe.
        "boot_image": boot_image,
        // SERVELEAK: the guest runs in its **own** process group (`process_group(0)`
        // at spawn above, so the pgid is the guest's pid). The session owner must
        // know it, or its teardown proof only ever covers this launcher and a
        // `acos_destroy` can report a clean tree while bwrap/dalvikvm64 still run.
        "guest_process_group": guest_pid,
        "label": "live ART session: real counter.apk Activity, in-guest view tree probe, ACOS C1 buffer captures",
    });
    protocol_write(&ready);
    evidence.push(ready);

    // stdin -> channel, so the loop is event-driven (H4: no 100 ms idle poll).
    let (ctx, crx) = mpsc::channel::<ServeEvent>();
    let stdin_ctx = ctx.clone();
    std::thread::spawn(move || {
        use std::io::BufRead;
        let stdin = std::io::stdin();
        let mut lock = stdin.lock();
        let mut line = String::new();
        loop {
            line.clear();
            match lock.read_line(&mut line) {
                Ok(0) => {
                    let _ = stdin_ctx.send(ServeEvent::Line(None));
                    return;
                }
                Ok(_) => {
                    if stdin_ctx
                        .send(ServeEvent::Line(Some(line.clone())))
                        .is_err()
                    {
                        return;
                    }
                }
                Err(_) => {
                    let _ = stdin_ctx.send(ServeEvent::Line(None));
                    return;
                }
            }
        }
    });
    // MPROC4 step 3b: spawn requests arrive on their own channel; forwarding
    // them into the session's event channel keeps one event loop (no second
    // poll path, no ordering question between a spawn and the next command).
    let pending_spawns: Arc<
        std::sync::Mutex<std::collections::VecDeque<acos_services::ams::ProcessSpawnRequest>>,
    > = Arc::default();
    {
        let spawn_ctx = ctx.clone();
        let spawn_queue = Arc::clone(&pending_spawns);
        let spawn_wake = settle_wake(probe_state);
        std::thread::spawn(move || {
            while let Ok(request) = spawn_rx.recv() {
                spawn_queue
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .push_back(request);
                spawn_wake.notify();
                if spawn_ctx.send(ServeEvent::SpawnWake).is_err() {
                    return;
                }
            }
        });
    }
    // H4: child exit is a blocking kernel event, not a 100 ms poll. `WNOWAIT`
    // leaves the child reapable, so the existing `child.try_wait()`/`wait()`
    // paths still own the status and no error is retried twice.
    let exit_ctx = ctx.clone();
    let exit_pid = child.id() as libc::id_t;
    std::thread::spawn(move || {
        loop {
            let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
            let rc = unsafe {
                libc::waitid(
                    libc::P_PID,
                    exit_pid,
                    &mut info,
                    libc::WEXITED | libc::WNOWAIT,
                )
            };
            if rc == 0 {
                break;
            }
            if std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted {
                break;
            }
        }
        let _ = exit_ctx.send(ServeEvent::ChildExited);
    });

    let session_deadline =
        Instant::now() + Duration::from_secs(env_u64("ACOSLAUNCH_SESSION_SECS", 900));
    let ack_wait = Duration::from_millis(env_u64("ACOS_INPUT_ACK_MS", 2500));
    let frame_wait = Duration::from_secs(env_u64("ACOSLAUNCH_FRAME_WAIT_SECS", 15));
    let clean: bool;
    let mut last_capture_fnv: Option<u64> = None;
    let mut panel_touch = PanelTouchState::default();
    loop {
        CURRENT_REQUEST_ID.store(0, std::sync::atomic::Ordering::Release);
        let deadline_left = session_deadline.saturating_duration_since(Instant::now());
        // A secondary guest that ended by itself is evidence, not a running
        // process: reap it before the next event is served.
        for record in secondary.reap() {
            evidence.push(record);
        }
        // MPROC5c: the idle kill runs before every wait, and the wait itself is
        // bounded by the next idle deadline, so an idle process is killed on
        // time without a polling timer.
        for record in secondary.idle_kill(idle_kill, &live.atms, &live.wms) {
            evidence.push(record);
        }
        let mut wait = deadline_left;
        if !idle_kill.is_zero() {
            let next_idle = live
                .atms
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .next_idle_deadline(idle_kill);
            if let Some(deadline) = next_idle {
                wait = wait.min(deadline.saturating_duration_since(Instant::now()));
            }
        }
        let line = match crx.recv_timeout(wait) {
            Ok(ServeEvent::SpawnWake) => {
                drain_pending_spawns(
                    Some((&pending_spawns, secondary)),
                    probe_state,
                    &live.compositor,
                );
                continue;
            }
            Ok(ServeEvent::Line(line)) => line,
            Ok(ServeEvent::ChildExited) => {
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
                let status = child.try_wait()?;
                let reply = serde_json::json!({
                    "ok": false, "cmd": "session", "code": "GUEST_EXITED",
                    "error": match &status {
                        Some(status) => format!("the guest ended while the session was idle (status {status})"),
                        None => "the guest ended while the session was idle".to_string(),
                    },
                });
                protocol_write(&reply);
                evidence.push(reply);
                write_session_evidence(out, &evidence);
                return Ok(SessionEnd {
                    status,
                    clean: false,
                });
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => None,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                // An idle-kill wake is not the session cap: sweep (the next
                // iteration does that) and keep serving.
                if Instant::now() < session_deadline {
                    continue;
                }
                // The only periodic bound left is the 900 s session cap.
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
        let req_id = request
            .get("req_id")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
        CURRENT_REQUEST_ID.store(req_id, std::sync::atomic::Ordering::Release);
        let cmd = request.get("cmd").and_then(|v| v.as_str()).unwrap_or("");
        // VIEW1a: the event feed reports the *request*'s coordinates/key/text
        // (a reply need not echo them); no-op with no viewer attached.
        if let Some(view) = LIVE_VIEW.get() {
            view.observe_request(&request);
        }
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
                // G3f1: a caller that already holds the window skeleton can
                // wait for the app's **next committed tree state** instead of
                // re-traversing on a timer. `baseline_tree` is the probe tree
                // the caller last bound; the strict settle rule is unchanged
                // except that a sample whose tree is still byte-identical to
                // the baseline is not a valid candidate. Same signal the
                // launch's own traversal uses (a settled sample bound to a
                // draw), so nothing new is claimed as evidence.
                let baseline_tree = request.get("baseline_tree").cloned();
                if mode == SettleMode::Fast && want_pixels {
                    let reply = serde_json::json!({
                        "ok": false, "cmd": "tree", "code": "BAD_REQUEST",
                        "error": "settle:fast is a tree-only mode; a fast frame may be mid-animation and cannot back a PNG capture",
                    });
                    protocol_write(&reply);
                    evidence.push(reply);
                    continue;
                }
                // SE2b: a caller may pin the sample floor (`min_probe_seq`), so
                // the guest can only answer with a probe sample *newer* than the
                // one the caller already consumed. The content-stable settle
                // requires strictly increasing samples across its window; a
                // request without the floor keeps the legacy behaviour.
                let min_probe_seq = request
                    .get("min_probe_seq")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(0);
                let mut tree_timeout: Option<SettleTiming> = None;
                // G3f1: how a successful strict settle bound the reply (a fully
                // settled sample, or a draw commit after the caller's baseline).
                let mut tree_bind: Option<SettleTiming> = None;
                let probe = if mode == SettleMode::Fast {
                    let deadline = Instant::now()
                        + request_settle_timeout(&request, Duration::from_millis(750));
                    match wait_for_fast_tree(
                        child,
                        cgroup,
                        live,
                        probe_state,
                        &mut guest_stdin,
                        deadline,
                        min_probe_seq,
                    ) {
                        FastTreeEnd::Ready(probe) => probe,
                        FastTreeEnd::Timeout { probe_seq } => {
                            let reply = serde_json::json!({
                                "ok": false, "cmd": "tree", "code": "NO_FRESH_TREE",
                                "error": format!(
                                    "no guest probe sample newer than the floor {min_probe_seq} and the frame store that was current at request entry arrived within 750 ms (last probe_seq {probe_seq}); a fast tree never serves the pre-request sample"
                                ),
                                "min_probe_seq": min_probe_seq,
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
                    // Strict (default): bind the tree and the pixels to the same
                    // settled state. P12b: `settle_timeout_ms` bounds the wait;
                    // a timeout still returns the latest tree (settled:false).
                    let settled = wait_for_settled_observation(
                        child,
                        cgroup,
                        live,
                        probe_state,
                        &mut guest_stdin,
                        Instant::now()
                            + request_settle_timeout(&request, Duration::from_millis(1500)),
                        0,
                        0,
                        None,
                        SettleMode::Strict,
                        None,
                        0,
                        baseline_tree.as_ref(),
                        // A tree request publishes no input, so no gesture's
                        // acks can make it an unhandled-input settle (REL3H)
                        // and no delivered token can have been removed (REL3I3).
                        None,
                        None,
                        None,
                        None,
                        Some((&pending_spawns, secondary)),
                    );
                    match settled {
                        SettleEnd::Settled(settled) => {
                            tree_bind = Some(settled.timing);
                            settled.probe
                        }
                        SettleEnd::Timeout { timing, .. } => {
                            tree_timeout = Some(timing);
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
                        SettleEnd::Unsupported(_) => match latest_probe(probe_state) {
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
                            return Ok(SessionEnd {
                                status: Some(status),
                                clean: false,
                            });
                        }
                    }
                };
                let p3_t_settled = p3_now_us();
                let tree_age_ms = host_ms().saturating_sub(probe.at_ms);
                let strict_event = mode == SettleMode::Strict;
                // G3f1: a draw-commit bind is a fresh frame, not a settled one.
                let draw_commit = tree_timeout.is_none()
                    && tree_bind
                        .as_ref()
                        .map(|timing| timing.bound == "change-draw")
                        .unwrap_or(false);
                // ROT8: the tree request bound on the guest's window-gone state.
                let tree_window_gone = tree_timeout.is_none()
                    && tree_bind
                        .as_ref()
                        .map(|timing| timing.task_finished)
                        .unwrap_or(false);
                let mut settle_json = if tree_window_gone {
                    serde_json::json!({
                        "mode": "strict",
                        "settled": true,
                        "probe_seq": probe.seq,
                        "tree_age_ms": tree_age_ms,
                        "bound": "window-gone",
                        "strict_rule": "event",
                        "task_finished": true,
                        "window_gone": true,
                        "meaning": "the app's last Activity is destroyed and no window root remains: the window forest is empty and no frame exists to bind",
                    })
                } else if draw_commit {
                    serde_json::json!({
                        "mode": "strict",
                        "settled": false,
                        "probe_seq": probe.seq,
                        "tree_age_ms": tree_age_ms,
                        "bound": "change-draw",
                        "strict_rule": "event",
                        "awaited": "tree-change",
                        "changed_since_baseline": true,
                        "warning": "the tree changed and its draw is committed, but the framework still reports pending UI work (an animating screen): this is a fresh frame, not a settled one, and it is not assertion evidence",
                    })
                } else {
                    match &tree_timeout {
                        Some(timing) => serde_json::json!({
                            "mode": "strict",
                            "settled": false,
                            "probe_seq": probe.seq,
                            "tree_age_ms": tree_age_ms,
                            "bound": "timeout",
                            "strict_rule": "event",
                            "reason": timing.reason.clone(),
                            "animating": timing
                                .reason
                                .as_deref()
                                .map(|r| r.contains("anim") || r.contains("frame"))
                                .unwrap_or(false),
                            "warning": "strict settle timed out: the tree and frame identity are the \
                                        latest observed state, not a settled one; this is not assertion \
                                        evidence",
                        }),
                        None => serde_json::json!({
                        "mode": mode.as_str(),
                        "settled": true,
                        "probe_seq": probe.seq,
                        "tree_age_ms": tree_age_ms,
                        "bound": if mode == SettleMode::Fast {
                            "probe-fresh"
                        } else {
                            "strict-event"
                        },
                        "meaning": if strict_event {
                            "mode=strict uses the event rule: the guest reported no pending framework UI work, the sample was taken after a draw at/after request entry, and the same draw stayed newest for the confirmation window; tree_age_ms is host receipt -> reply"
                        } else {
                            "mode=fast returns the newest probe sample bound to a frame at least as new as the C1 store current at request entry; tree_age_ms is host receipt -> reply"
                        },
                        }),
                    }
                };
                if strict_event && tree_timeout.is_none() {
                    if let Some(map) = settle_json.as_object_mut() {
                        map.insert("strict_rule".into(), serde_json::json!("event"));
                    }
                }
                // G3f1: say what the request waited for, so a reply can never
                // be mistaken for "the same tree again": the sample it binds
                // was forced to differ from the caller's baseline tree.
                if tree_timeout.is_some() {
                    // SETTLE5: a timed-out tree request carries the probe
                    // channel's health, so `probe-floor` names its cause.
                    if let Some(map) = settle_json.as_object_mut() {
                        map.insert("probe".into(), probe_health_json(probe_state));
                    }
                }
                if baseline_tree.is_some() {
                    if let Some(map) = settle_json.as_object_mut() {
                        map.insert("awaited".into(), serde_json::json!("tree-change"));
                        // Only a settled reply *proves* the change; a timeout
                        // returns the latest sample whatever it is.
                        map.insert(
                            "changed_since_baseline".into(),
                            serde_json::json!(tree_timeout.is_none()),
                        );
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
                        map.insert(
                            "buffer_generation".into(),
                            serde_json::json!(meta.generation),
                        );
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
            "panel_touch" => {
                let Some(action) = request.get("action").and_then(|v| v.as_i64()).map(|v| v as i32)
                else {
                    protocol_write(&serde_json::json!({
                        "ok": false, "cmd": "panel_touch", "code": "BAD_REQUEST",
                        "error": "panel_touch needs integer action",
                    }));
                    continue;
                };
                let now_ns = acos_services::input_channel::monotonic_nanos();
                let down_time = request
                    .get("down_time")
                    .and_then(|v| v.as_i64())
                    .unwrap_or(now_ns);
                let event_time = request
                    .get("event_time")
                    .and_then(|v| v.as_i64())
                    .unwrap_or(now_ns);
                let pointers: Option<Vec<(i32, f32, f32)>> = request
                    .get("pointers")
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|item| {
                                let pair = item.as_array()?;
                                if pair.len() != 3 {
                                    return None;
                                }
                                Some((
                                    pair[0].as_i64()? as i32,
                                    pair[1].as_f64()? as f32,
                                    pair[2].as_f64()? as f32,
                                ))
                            })
                            .collect()
                    });
                let Some(pointers) = pointers.filter(|p| !p.is_empty()) else {
                    protocol_write(&serde_json::json!({
                        "ok": false, "cmd": "panel_touch", "code": "BAD_REQUEST",
                        "error": "panel_touch needs non-empty pointers [[id, x, y], ...]",
                    }));
                    continue;
                };
                let target: Option<(f32, f32)> = match request.get("target") {
                    None | Some(serde_json::Value::Null) => None,
                    Some(v) => {
                        let parsed = v.as_array().and_then(|arr| {
                            if arr.len() != 2 {
                                return None;
                            }
                            let tx = arr[0].as_f64()? as f32;
                            let ty = arr[1].as_f64()? as f32;
                            if tx.is_finite() && ty.is_finite() {
                                Some((tx, ty))
                            } else {
                                None
                            }
                        });
                        let Some(pt) = parsed else {
                            protocol_write(&serde_json::json!({
                                "ok": false, "cmd": "panel_touch", "code": "BAD_REQUEST",
                                "error": "panel_touch target must be [x, y]",
                            }));
                            continue;
                        };
                        Some(pt)
                    }
                };
                let gesture = match request.get("gesture") {
                    None | Some(serde_json::Value::Null) => None,
                    Some(v) => match v.as_str() {
                        Some(g @ ("scroll" | "pinch")) => Some(g),
                        _ => {
                            protocol_write(&serde_json::json!({
                                "ok": false, "cmd": "panel_touch", "code": "BAD_REQUEST",
                                "error": "panel_touch gesture must be \"scroll\" or \"pinch\"",
                            }));
                            continue;
                        }
                    },
                };
                let masked_action = action & 0xff;
                let touch_wait = if matches!(
                    masked_action,
                    acos_services::input_channel::ACTION_MOVE
                ) {
                    Duration::ZERO
                } else {
                    Duration::from_millis(100)
                };
                match panel_touch.dispatch_with_hit_point(
                    &live.input,
                    action,
                    down_time,
                    event_time,
                    &pointers,
                    target,
                    touch_wait,
                ) {
                    Ok(outcome) => {
                        let mut reply = serde_json::json!({
                            "ok": true,
                            "cmd": "panel_touch",
                            "action": masked_action,
                            "x": pointers[0].1.round() as i64,
                            "y": pointers[0].2.round() as i64,
                        });
                        if let Some(map) = reply.as_object_mut() {
                            match outcome {
                                PanelTouchOutcome::Dispatched => {}
                                PanelTouchOutcome::TargetClosed => {
                                    map.insert("target_closed".into(), serde_json::json!(true));
                                }
                                PanelTouchOutcome::AlreadyEnded => {
                                    map.insert("already_ended".into(), serde_json::json!(true));
                                }
                            }
                            if let Some(g) = gesture {
                                map.insert("gesture".into(), serde_json::json!(g));
                            }
                        }
                        protocol_write(&reply);
                    }
                    Err(PanelTouchError { err, published }) => {
                        protocol_write(&serde_json::json!({
                            "ok": false,
                            "cmd": "panel_touch",
                            "code": if published {
                                "INPUT_PUBLISHED_ACK_FAILED"
                            } else {
                                "INPUT_UNAVAILABLE"
                            },
                            "published": published,
                            "error": format!("panel_touch failed: {err}"),
                        }));
                    }
                }
            }
            "panel_key" => {
                let Some(code) = request.get("code").and_then(|v| v.as_i64()).map(|v| v as i32) else {
                    protocol_write(&serde_json::json!({
                        "ok": false, "cmd": "panel_key", "code": "BAD_REQUEST",
                        "error": "panel_key needs integer code",
                    }));
                    continue;
                };
                let event_type = request
                    .get("event_type")
                    .and_then(|v| v.as_str())
                    .unwrap_or("press");
                let meta_state = request
                    .get("meta_state")
                    .and_then(|v| v.as_i64())
                    .unwrap_or(0) as i32;
                let device_id = request
                    .get("device_id")
                    .and_then(|v| v.as_i64())
                    .unwrap_or(1) as i32;
                if code == 3 {
                    if event_type == "up" {
                        protocol_write(&serde_json::json!({"ok": true, "cmd": "panel_key", "code": code}));
                        continue;
                    }
                    match control.submit(
                        serde_json::json!({"cmd": "services", "op": "launch_home"}),
                        Duration::from_secs(10),
                    ) {
                        Some(ServiceControlReply { reply: home_reply, .. })
                            if home_reply.get("ok").and_then(|v| v.as_bool()) == Some(true) =>
                        {
                            protocol_write(&serde_json::json!({"ok": true, "cmd": "panel_key", "code": code}));
                        }
                        Some(ServiceControlReply { reply: home_reply, .. }) => {
                            let err_code = home_reply
                                .get("code")
                                .and_then(|v| v.as_str())
                                .unwrap_or("HOME_UNAVAILABLE");
                            let err_msg = home_reply
                                .get("error")
                                .and_then(|v| v.as_str())
                                .unwrap_or("no HOME activity installed");
                            protocol_write(&serde_json::json!({
                                "ok": false, "cmd": "panel_key", "code": err_code, "error": err_msg,
                            }));
                        }
                        None => protocol_write(&serde_json::json!({
                            "ok": false, "cmd": "panel_key", "code": "SERVICE_CONTROL_TIMEOUT",
                            "error": "launch_home timed out",
                        })),
                    }
                    continue;
                }
                let now_ns = acos_services::input_channel::monotonic_nanos();
                let key_wait = Duration::from_millis(250);
                let res = match event_type {
                    "down" => live.input.key_event_checked(
                        acos_services::input_channel::KEY_ACTION_DOWN,
                        code,
                        meta_state,
                        device_id,
                        now_ns,
                        now_ns,
                        key_wait,
                    ),
                    "up" => live.input.key_event_checked(
                        acos_services::input_channel::KEY_ACTION_UP,
                        code,
                        meta_state,
                        device_id,
                        now_ns,
                        now_ns,
                        key_wait,
                    ),
                    _ => {
                        // Attempt the paired KEY_ACTION_UP whenever KEY_ACTION_DOWN
                        // was published (even if ack collection failed), so the key
                        // is not left stuck down in the guest. If DOWN failed
                        // pre-publish, surface the rejection without a spurious UP.
                        let down_res = live.input.key_event_checked(
                            acos_services::input_channel::KEY_ACTION_DOWN,
                            code,
                            meta_state,
                            device_id,
                            now_ns,
                            now_ns,
                            key_wait,
                        );
                        match down_res {
                            Err((err, false)) => Err((err, false)),
                            Ok(_) => live
                                .input
                                .key_event_checked(
                                    acos_services::input_channel::KEY_ACTION_UP,
                                    code,
                                    meta_state,
                                    device_id,
                                    now_ns,
                                    now_ns + acos_services::input_channel::DEFAULT_TAP_HOLD_NANOS,
                                    key_wait,
                                )
                                .map_err(|(up_err, _)| (up_err, true)),
                            Err((down_err, true)) => {
                                let up_res = live.input.key_event_checked(
                                    acos_services::input_channel::KEY_ACTION_UP,
                                    code,
                                    meta_state,
                                    device_id,
                                    now_ns,
                                    now_ns + acos_services::input_channel::DEFAULT_TAP_HOLD_NANOS,
                                    key_wait,
                                );
                                match up_res {
                                    Ok(_) => Err((down_err, false)),
                                    Err((up_err, _)) => Err((up_err, true)),
                                }
                            }
                        }
                    }
                };
                match res {
                    Ok(_) => protocol_write(&serde_json::json!({"ok": true, "cmd": "panel_key", "code": code})),
                    Err((err, published)) => protocol_write(&serde_json::json!({
                        "ok": false,
                        "cmd": "panel_key",
                        "code": if published {
                            "INPUT_PUBLISHED_ACK_FAILED"
                        } else {
                            "INPUT_UNAVAILABLE"
                        },
                        "published": published,
                        "error": format!("panel_key failed: {err}"),
                    })),
                }
            }
            "panel_text" => {
                let Some(text) = request.get("text").and_then(|v| v.as_str()) else {
                    protocol_write(&serde_json::json!({
                        "ok": false, "cmd": "panel_text", "code": "BAD_REQUEST",
                        "error": "panel_text needs string text",
                    }));
                    continue;
                };
                if text.is_empty() {
                    match live.input.type_text(text, ack_wait) {
                        Ok(_) => protocol_write(&serde_json::json!({"ok": true, "cmd": "panel_text", "text": text})),
                        Err(err) => protocol_write(&serde_json::json!({
                            "ok": false, "cmd": "panel_text", "code": "INPUT_UNAVAILABLE",
                            "error": format!("panel_text failed: {err}"),
                        })),
                    }
                    continue;
                }
                match try_guest_commit_text(
                    child,
                    live,
                    probe_state,
                    &mut guest_stdin,
                    text,
                    ack_wait,
                ) {
                    GuestCommitResult::Committed { .. } => {
                        protocol_write(&serde_json::json!({"ok": true, "cmd": "panel_text", "text": text}));
                    }
                    GuestCommitResult::NoFocusedEditor { focus_class, error }
                    | GuestCommitResult::Rejected { focus_class, error } => {
                        if text.is_ascii() {
                            match live.input.type_text(text, ack_wait) {
                                Ok(_) => protocol_write(&serde_json::json!({"ok": true, "cmd": "panel_text", "text": text})),
                                Err(err) => protocol_write(&serde_json::json!({
                                    "ok": false, "cmd": "panel_text", "code": "INPUT_UNAVAILABLE",
                                    "error": format!("panel_text failed: {err}"),
                                })),
                            }
                        } else {
                            let suffix = focus_class
                                .map(|cls| format!(" (focused={cls})"))
                                .unwrap_or_default();
                            protocol_write(&serde_json::json!({
                                "ok": false, "cmd": "panel_text", "code": "INPUT_UNAVAILABLE",
                                "error": format!("panel_text InputConnection.commitText failed for non-ASCII text{suffix}: {error}"),
                            }));
                        }
                    }
                    GuestCommitResult::Unavailable { reason } => {
                        if text.is_ascii() {
                            match live.input.type_text(text, ack_wait) {
                                Ok(_) => protocol_write(&serde_json::json!({"ok": true, "cmd": "panel_text", "text": text})),
                                Err(err) => protocol_write(&serde_json::json!({
                                    "ok": false, "cmd": "panel_text", "code": "INPUT_UNAVAILABLE",
                                    "error": format!("panel_text failed: {err}"),
                                })),
                            }
                        } else {
                            protocol_write(&serde_json::json!({
                                "ok": false, "cmd": "panel_text", "code": "INPUT_UNAVAILABLE",
                                "error": format!("panel_text InputConnection.commitText unavailable for non-ASCII text: {reason}"),
                            }));
                        }
                    }
                    GuestCommitResult::Uncertain { code, reason } => {
                        protocol_write(&serde_json::json!({
                            "ok": false, "cmd": "panel_text", "code": code,
                            "error": format!("panel_text InputConnection.commitText did not confirm delivery: {reason}"),
                        }));
                    }
                }
            }
            "rotate" | "panel_rotate" => {
                // ROT3: a display rotation is a WMS operation plus a
                // configuration change in the guest. The host rotates the
                // live `WmsState` display first (so every later `relayout`
                // reply carries the new frame + MergedConfiguration), then
                // asks the guest to build the new `Configuration` and let
                // `ActivityThread.handleConfigurationChanged` decide
                // relaunch vs `onConfigurationChanged` from the manifest's
                // real `ActivityInfo.configChanges`.
                let rotation = request
                    .get("rotation")
                    .and_then(|v| v.as_i64())
                    .or_else(|| {
                        request
                            .get("orientation")
                            .and_then(|v| v.as_str())
                            .map(|o| {
                                if o.eq_ignore_ascii_case("landscape") {
                                    1
                                } else {
                                    0
                                }
                            })
                    });
                let Some(rotation) = rotation.filter(|r| (0..=3).contains(r)) else {
                    let reply = serde_json::json!({
                        "ok": false, "cmd": cmd, "code": "BAD_REQUEST",
                        "error": "rotate needs rotation 0..3 or orientation portrait|landscape",
                    });
                    protocol_write(&reply);
                    evidence.push(reply);
                    continue;
                };
                // Secondary processes run with `Stdio::null()` and cannot
                // receive `ROTATE` over the primary guest's stdin pipe. Refuse
                // before mutating `WmsState` so display geometry stays
                // consistent with the foreground process.
                let foreground = live.top_process.load(std::sync::atomic::Ordering::Relaxed);
                if foreground != 0 {
                    let reply = serde_json::json!({
                        "ok": false,
                        "cmd": cmd,
                        "code": "UNSUPPORTED_ANDROID_API",
                        "error": format!(
                            "rotate targets foreground secondary process {foreground}; \
                             rotation is not delivered to secondary processes"
                        ),
                    });
                    protocol_write(&reply);
                    evidence.push(reply);
                    continue;
                }
                let Some(writer) = guest_stdin.as_mut() else {
                    let reply = serde_json::json!({
                        "ok": false,
                        "cmd": cmd,
                        "code": "NO_GUEST_CONTROL",
                        "error": "the guest has no stdin control pipe for rotate",
                    });
                    protocol_write(&reply);
                    evidence.push(reply);
                    continue;
                };
                let display = live
                    .wms
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .rotate(rotation as u8);
                let before = latest_probe(probe_state).map(|p| p.seq).unwrap_or(0);
                {
                    use std::io::Write;
                    let _ = writeln!(writer, "ROTATE {rotation}");
                    let _ = writer.flush();
                }
                // The reply is bound to a *new* probe sample: the guest posts
                // the rotation on its main looper and emits one sample at the
                // new size, so a sample older than the request can never back
                // the answer.
                let deadline = Instant::now() + frame_wait;
                let mut fresh = None;
                let mut replied = false;
                while Instant::now() < deadline {
                    if let Some(sample) = latest_probe(probe_state) {
                        if sample.seq > before {
                            fresh = Some(sample);
                            break;
                        }
                    }
                    if let Ok(Some(status)) = child.try_wait() {
                        let reply = serde_json::json!({
                            "ok": false, "cmd": cmd, "code": "GUEST_EXITED",
                            "error": format!("the guest exited before it published a rotated probe: {status}"),
                        });
                        protocol_write(&reply);
                        evidence.push(reply);
                        replied = true;
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
                match fresh {
                    Some(sample) => {
                        let reply = serde_json::json!({
                            "ok": true, "cmd": cmd,
                            "rotation": rotation,
                            "width": display.frame.width(),
                            "height": display.frame.height(),
                            "probe_seq": sample.seq,
                            "frame_seq": latest_generation(live),
                            "tree": sample.tree,
                        });
                        protocol_write(&reply);
                        evidence.push(reply);
                    }
                    None => {
                        // No fresh sample: the rotation did not reach the
                        // guest (no reader, or the guest refused). Fail closed
                        // with a structured error, never a pass.
                        if !replied {
                            let reply = serde_json::json!({
                                "ok": false, "cmd": cmd, "code": "NO_FRESH_TREE",
                                "error": "the guest published no probe sample at the rotated size",
                            });
                            protocol_write(&reply);
                            evidence.push(reply);
                        }
                    }
                }
            }
            "services" => {
                // ADR-0029 item 4 + 2026-11-18 amendment (option A): list/status/
                // attach/detach/ctl/replay_intent on the live session. The peer
                // thread owns the host and returns both the module's reply and
                // any typed guest commands queued onto `ServiceHost`'s outbox.
                let svc_op = request
                    .get("op")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let svc_id = request
                    .get("id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let svc_profile = request
                    .get("profile")
                    .and_then(|v| v.as_str())
                    .unwrap_or("real");
                let mut forward_request = request.clone();
                let mut hot_staged_extra: Option<ExtraApk> = None;
                if svc_op == "attach"
                    && svc_id == acos_services::modules::settings::MODULE_ID
                    && svc_profile != "off"
                    && !secondary.has_package(acos_services::modules::settings::SETTINGS_PACKAGE)
                {
                    let cfg = match request.get("config") {
                        None => Ok(acos_services::module::ModuleConfig::default()),
                        Some(value) => {
                            acos_services::module::ModuleConfig::from_json("<live>", value)
                        }
                    };
                    if let Ok(config) = cfg {
                        let apk_path = acos_services::modules::settings::settings_apk(&config);
                        if apk_path.is_file() {
                            match secondary.stage_hot_apk(&apk_path) {
                                Ok(extra) => {
                                    if let Some(obj) = forward_request.as_object_mut() {
                                        obj.insert(
                                            "_hot_extra_apk".into(),
                                            serde_json::json!({
                                                "staged": extra.staged.to_string_lossy(),
                                                "guest_path": extra.guest_path,
                                                "guest_lib_dir": extra.guest_lib_dir,
                                                "package": extra.package,
                                            }),
                                        );
                                    }
                                    hot_staged_extra = Some(extra);
                                }
                                Err(err) => {
                                    let reply = serde_json::json!({
                                        "ok": false,
                                        "cmd": "services",
                                        "op": "attach",
                                        "code": "INTERNAL",
                                        "error": format!(
                                            "hot-staging system.settings APK failed: {err}"
                                        ),
                                    });
                                    protocol_write(&reply);
                                    evidence.push(reply);
                                    continue;
                                }
                            }
                        }
                    }
                }
                let replay_g0 = (svc_op == "replay_intent").then(|| latest_generation(live));
                let reply = match control.submit(forward_request, Duration::from_secs(10)) {
                    Some(ServiceControlReply {
                        mut reply,
                        guest_commands,
                    }) => {
                        let reply_ok = reply
                            .get("ok")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false);
                        let hot_uid = reply
                            .as_object_mut()
                            .and_then(|obj| obj.remove("_hot_extra_uid"))
                            .and_then(|v| v.as_i64())
                            .map(|u| u as i32);
                        if reply_ok {
                            if let Some(mut extra) = hot_staged_extra {
                                extra.uid = hot_uid;
                                secondary.register_hot_extra(extra);
                            }
                            if svc_op == "detach"
                                && svc_id == acos_services::modules::settings::MODULE_ID
                            {
                                secondary.remove_package(
                                    acos_services::modules::settings::SETTINGS_PACKAGE,
                                );
                            }
                            if svc_op == "replay_intent" {
                                let g0 = replay_g0.unwrap_or(0);
                                drain_pending_spawns(
                                    Some((&pending_spawns, secondary)),
                                    probe_state,
                                    &live.compositor,
                                );
                                let deadline =
                                    Instant::now() + frame_wait.min(Duration::from_secs(5));
                                let mut frame_advanced = false;
                                while Instant::now() < deadline {
                                    drain_pending_spawns(
                                        Some((&pending_spawns, secondary)),
                                        probe_state,
                                        &live.compositor,
                                    );
                                    if latest_generation(live) > g0 {
                                        frame_advanced = true;
                                        break;
                                    }
                                    std::thread::sleep(Duration::from_millis(15));
                                }
                                if let Some(obj) = reply.as_object_mut() {
                                    // Frame advance only: a session-wide frame after the replay, not
                                    // proof the replayed activity is visible or settled (ADR-0032).
                                    obj.insert("frame_advanced".into(), serde_json::json!(frame_advanced));
                                }
                            }
                        }
                        if reply_ok && !guest_commands.is_empty() {
                            let mut dispatch_err: Option<serde_json::Value> = None;
                            for command in &guest_commands {
                                match *command {
                                    acos_services::GuestCommand::Window {
                                        task,
                                        process,
                                        rect,
                                        app_bounds,
                                        windowing_mode,
                                    } => {
                                        if process != 0 {
                                            dispatch_err = Some(serde_json::json!({
                                                "ok": false,
                                                "cmd": "services",
                                                "code": "UNSUPPORTED_ANDROID_API",
                                                "error": format!(
                                                    "GuestCommand::Window for task {task} targets \
                                                     secondary process {process}; multi-process \
                                                     window-command routing requires multiwindow slice 2"
                                                ),
                                            }));
                                            break;
                                        }
                                        let Some(writer) = guest_stdin.as_mut() else {
                                            dispatch_err = Some(serde_json::json!({
                                                "ok": false,
                                                "cmd": "services",
                                                "code": "NO_GUEST_CONTROL",
                                                "error": "the guest has no stdin control pipe for GuestCommand::Window",
                                            }));
                                            break;
                                        };
                                        use std::io::Write;
                                        if let Err(err) = writeln!(
                                            writer,
                                            "WINDOW {windowing_mode} {} {} {} {} {} {} {} {}",
                                            rect.left,
                                            rect.top,
                                            rect.right,
                                            rect.bottom,
                                            app_bounds.left,
                                            app_bounds.top,
                                            app_bounds.right,
                                            app_bounds.bottom
                                        )
                                        .and_then(|()| writer.flush())
                                        {
                                            dispatch_err = Some(serde_json::json!({
                                                "ok": false,
                                                "cmd": "services",
                                                "code": "GUEST_IO",
                                                "error": format!("failed writing WINDOW command to guest: {err}"),
                                            }));
                                            break;
                                        }
                                        window_cmds_sent += 1;
                                    }
                                }
                            }
                            if let Some(err_reply) = dispatch_err {
                                err_reply
                            } else {
                                let deadline = Instant::now() + frame_wait;
                                let mut fresh = None;
                                let mut exited: Option<serde_json::Value> = None;
                                let mut cmd_failed: Option<serde_json::Value> = None;
                                while Instant::now() < deadline {
                                    if let Some(sample) = latest_probe(probe_state) {
                                        // CR1FIX #20: only the guest's own
                                        // command-specific counter proves the
                                        // tree was walked after the WINDOW
                                        // command was applied.
                                        match window_ack(
                                            &sample,
                                            window_cmds_sent,
                                            window_failed_seen,
                                        ) {
                                            WindowAck::Applied => {
                                                fresh = Some(sample);
                                                break;
                                            }
                                            WindowAck::Failed => {
                                                window_failed_seen = sample
                                                    .window_cmd
                                                    .map(|c| c.failed)
                                                    .unwrap_or(window_failed_seen);
                                                cmd_failed = Some(serde_json::json!({
                                                    "ok": false,
                                                    "cmd": "services",
                                                    "code": "GUEST_WINDOW_FAILED",
                                                    "error": "the guest failed to apply GuestCommand::Window (see its `ACOSLAUNCH MARK window-failed` line)",
                                                }));
                                                break;
                                            }
                                            WindowAck::Pending => {}
                                        }
                                    }
                                    if let Ok(Some(status)) = child.try_wait() {
                                        exited = Some(serde_json::json!({
                                            "ok": false,
                                            "cmd": "services",
                                            "code": "GUEST_EXITED",
                                            "error": format!(
                                                "the guest exited before it published a probe after GuestCommand::Window: {status}"
                                            ),
                                        }));
                                        break;
                                    }
                                    std::thread::sleep(Duration::from_millis(10));
                                }
                                if let Some(exit_reply) = exited {
                                    exit_reply
                                } else if let Some(failed_reply) = cmd_failed {
                                    failed_reply
                                } else if let Some(sample) = fresh {
                                    if let Some(obj) = reply.as_object_mut() {
                                        obj.insert(
                                            "probe_seq".to_string(),
                                            serde_json::json!(sample.seq),
                                        );
                                        obj.insert("tree".to_string(), sample.tree);
                                    }
                                    reply
                                } else {
                                    serde_json::json!({
                                        "ok": false,
                                        "cmd": "services",
                                        "code": "NO_FRESH_TREE",
                                        "error": "the guest published no probe sample proving it applied GuestCommand::Window",
                                    })
                                }
                            }
                        } else {
                            reply
                        }
                    }
                    None => serde_json::json!({
                        "ok": false, "cmd": "services", "code": "SERVICE_CONTROL_TIMEOUT",
                        "error": "the peer thread did not answer the module request within 10 s",
                    }),
                };
                protocol_write(&reply);
                evidence.push(reply);
            }
            "tap" | "key" | "type_text" | "swipe" => {
                let text_arg = request
                    .get("text")
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                // REL3S: a swipe is a real interpolated MotionEvent stream, so
                // it names two points and a duration (never a direction the
                // guest would have to invent physics for).
                let mut swipe_from = (0.0f32, 0.0f32);
                let mut swipe_to = (0.0f32, 0.0f32);
                let mut swipe_duration = Duration::from_millis(300);
                let (x, y, key_code) = if cmd == "swipe" {
                    let point = |suffix: &str| {
                        let x = request.get(&format!("x{suffix}")).and_then(|v| v.as_i64());
                        let y = request.get(&format!("y{suffix}")).and_then(|v| v.as_i64());
                        match (x, y) {
                            (Some(x), Some(y)) => Some((x as f32, y as f32)),
                            _ => None,
                        }
                    };
                    match (point("1"), point("2")) {
                        (Some(from), Some(to)) => {
                            swipe_from = from;
                            swipe_to = to;
                            swipe_duration = Duration::from_millis(
                                request
                                    .get("duration_ms")
                                    .and_then(|v| v.as_u64())
                                    .unwrap_or(300)
                                    .clamp(1, 30_000),
                            );
                            (from.0, from.1, None)
                        }
                        _ => {
                            let reply = serde_json::json!({
                                "ok": false, "cmd": cmd, "code": "BAD_REQUEST",
                                "error": "swipe needs integers x1, y1, x2 and y2 (and optional duration_ms)",
                            });
                            protocol_write(&reply);
                            evidence.push(reply);
                            continue;
                        }
                    }
                } else if cmd == "tap" {
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
                let prompt_cursor = live
                    .prompts
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .cursor();
                // W59: the guest's ack is the end of the host-visible input
                // phase; the looper lines the guest printed in between are the
                // stages. Both are stamped on the same monotonic base.
                let injection_started = Instant::now();
                // REL3I3: the host's own window-removal counter, taken before
                // the input is published. A window destroyed during this step
                // that is the one this gesture went to explains a missing ack:
                // its `InputEventReceiver` was disposed before it could finish
                // the event. A different window's removal never matches.
                let removal_mark = live.input.removal_mark();
                let gesture_label = if cmd == "swipe" {
                    format!(
                        "from ({},{}) to ({},{}) in {} ms",
                        swipe_from.0,
                        swipe_from.1,
                        swipe_to.0,
                        swipe_to.1,
                        swipe_duration.as_millis()
                    )
                } else {
                    format!("at ({x},{y})")
                };
                trace_host(
                    probe_state,
                    &format!(
                        "{cmd} publish_begin {gesture_label} ack_wait_ms={}",
                        ack_wait.as_millis()
                    ),
                );
                // PERF4: the fallback pre-action pixels are NOT captured here.
                // They used to be, and the cost was a full composite of the
                // display (`capture_latest`: every visible layer's buffer copied
                // and hashed, 1080x2400) on every action whose caller had no
                // capture of its own — i.e. every tap after a `pixels:false`
                // reply, which is the whole M8 form loop and any agent that does
                // not ask for pixels. Measured on the bench's form-fast loop:
                // 8.7-9.2 ms per tap (`diagnosis/tap-split-*.txt`), the entire
                // v0.3.0 M8 regression. See `unhandled_entry_fnv` below for where
                // the value is taken now and why moving it changes nothing.
                let is_home_key = cmd == "key" && key_code == Some(3);
                let injected = if is_home_key {
                    match control.submit(
                        serde_json::json!({"cmd": "services", "op": "launch_home"}),
                        Duration::from_secs(10),
                    ) {
                        None => {
                            let reply = serde_json::json!({
                                "ok": false, "cmd": cmd, "code": "SERVICE_CONTROL_TIMEOUT",
                                "error": "the peer thread did not answer launch_home within 10 s",
                            });
                            protocol_write(&reply);
                            evidence.push(reply);
                            continue;
                        }
                        Some(ServiceControlReply {
                            reply: home_reply,
                            ..
                        }) => {
                            if home_reply.get("ok").and_then(|v| v.as_bool()) != Some(true) {
                                let code_str = home_reply
                                    .get("code")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("HOME_UNAVAILABLE");
                                let err_str = home_reply
                                    .get("error")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("no HOME activity installed");
                                let reply = serde_json::json!({
                                    "ok": false, "cmd": cmd, "code": code_str,
                                    "error": err_str,
                                });
                                protocol_write(&reply);
                                evidence.push(reply);
                                continue;
                            }
                            Ok(acos_services::window_session::input_dispatch::InjectedGesture {
                                token: acos_parcel::BinderId(0),
                                seqs: Vec::new(),
                                acks: Vec::new(),
                            })
                        }
                    }
                } else {
                    match (cmd, key_code) {
                        ("tap", _) => live.input.tap(x, y, ack_wait),
                        ("swipe", _) => live
                            .input
                            .swipe(swipe_from, swipe_to, swipe_duration, ack_wait),
                        ("type_text", _) => {
                            let text = text_arg.as_deref().unwrap_or_default();
                            if text.is_empty() {
                                live.input.type_text(text, ack_wait)
                            } else {
                                match try_guest_commit_text(
                                    child,
                                    live,
                                    probe_state,
                                    &mut guest_stdin,
                                    text,
                                    ack_wait,
                                ) {
                                    GuestCommitResult::Committed {
                                        seq,
                                        handled,
                                        latency_us,
                                        focus_class: _,
                                    } => Ok(
                                        acos_services::window_session::input_dispatch::InjectedGesture {
                                            token: acos_parcel::BinderId(0),
                                            seqs: vec![seq as u32],
                                            acks: vec![
                                                acos_services::window_session::input_dispatch::InjectedAck {
                                                    seq: seq as u32,
                                                    handled,
                                                    latency_us: Some(latency_us),
                                                },
                                            ],
                                        },
                                    ),
                                    GuestCommitResult::NoFocusedEditor { focus_class, error }
                                    | GuestCommitResult::Rejected { focus_class, error } => {
                                        if text.is_ascii() {
                                            live.input.type_text(text, ack_wait)
                                        } else {
                                            let suffix = focus_class
                                                .map(|cls| format!(" (focused={cls})"))
                                                .unwrap_or_default();
                                            let reply = serde_json::json!({
                                                "ok": false,
                                                "cmd": cmd,
                                                "code": "INPUT_UNAVAILABLE",
                                                "error": format!(
                                                    "InputConnection.commitText failed for non-ASCII text{suffix}: {error}"
                                                ),
                                            });
                                            protocol_write(&reply);
                                            evidence.push(reply);
                                            continue;
                                        }
                                    }
                                    GuestCommitResult::Unavailable { reason } => {
                                        if text.is_ascii() {
                                            live.input.type_text(text, ack_wait)
                                        } else {
                                            let reply = serde_json::json!({
                                                "ok": false,
                                                "cmd": cmd,
                                                "code": "INPUT_UNAVAILABLE",
                                                "error": format!(
                                                    "InputConnection.commitText unavailable for non-ASCII text: {reason}"
                                                ),
                                            });
                                            protocol_write(&reply);
                                            evidence.push(reply);
                                            continue;
                                        }
                                    }
                                    GuestCommitResult::Uncertain { code, reason } => {
                                        let reply = serde_json::json!({
                                            "ok": false,
                                            "cmd": cmd,
                                            "code": code,
                                            "error": format!(
                                                "InputConnection.commitText did not confirm delivery: {reason}"
                                            ),
                                        });
                                        protocol_write(&reply);
                                        evidence.push(reply);
                                        continue;
                                    }
                                }
                            }
                        }
                        (_, Some(code)) => live.input.key(code, ack_wait),
                        _ => unreachable!(),
                    }
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
                            "live_tokens": live.input.live_tokens(),
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
                // P12b: per-request strict/fast deadline, default = the engine's
                // long-standing frame wait.
                let settle_timeout = request_settle_timeout(&request, frame_wait);
                // REL3H: only a strict input whose *every* event came back
                // acknowledged unhandled can settle as "unhandled input".
                // REL3H2: the floor of that rule is the newest probe sample at
                // this moment - *after* every ack of this gesture had arrived -
                // because a sample taken earlier may have been taken before the
                // UI thread ran the handler (see [`UnhandledSettleGate`]).
                let unhandled_ack_floor = (mode == SettleMode::Strict
                    && every_event_acked_unhandled(&injected.seqs, &injected.acks))
                .then(|| {
                    latest_probe(probe_state)
                        .map(|sample| sample.seq)
                        .unwrap_or(p0)
                });
                // REL3H's fallback pre-action pixels, taken here instead of at
                // publish (PERF4; see the publish site for the measured 9 ms).
                //
                // What the value is for: the unhandled-input branch reports
                // `pixels_changed` against it instead of inventing one, so an
                // `expect_change: true` claim on a tap nothing consumed fails on
                // a *measured* false. The branch can only bind a sample whose
                // draw predates the publish (that is what makes the gesture
                // unhandled: no draw followed it), so when no C1 store has
                // landed since `g0` the composite in C1 is still the pre-action
                // screen, byte for byte — the same value the old code captured
                // before publishing, just read later. If a store *did* land, the
                // pre-action screen is gone; the old comparison would then have
                // reported a change by construction (current != entry, unless a
                // redraw reproduced the same pixels), and `None` makes the branch
                // report exactly that: the conservative `true`, no claim.
                // The capture is skipped entirely when the branch cannot be
                // reached (fast mode never sets `unhandled_ack_floor`; a handled
                // gesture never does) and when the caller supplied its own
                // pre-action capture.
                let entry_fnv = match (unhandled_ack_floor, last_capture_fnv) {
                    (Some(_), None) if latest_generation(live) == g0 => {
                        let state = live.compositor.lock().unwrap_or_else(|e| e.into_inner());
                        state.capture_latest().map(|capture| capture.fnv1a)
                    }
                    _ => None,
                };
                // BACKSETTLE: the window forest as it is **now** - every ack of
                // this gesture has arrived (the collect above returned), so a
                // top-window change observed later is this gesture's, and one
                // that already happened is not. Strict inputs only: `fast` is a
                // navigation rule that never binds a frame.
                let window_reshown = (mode == SettleMode::Strict).then(|| window_forest(live));
                let settled = wait_for_settled_observation(
                    child,
                    cgroup,
                    live,
                    probe_state,
                    &mut guest_stdin,
                    Instant::now() + settle_timeout,
                    g0,
                    p0,
                    last_capture_fnv,
                    mode,
                    baseline_probe,
                    publish_epoch_ms,
                    None,
                    unhandled_ack_floor,
                    entry_fnv,
                    (!is_home_key && injected.token.0 != 0)
                        .then_some((injected.token, removal_mark)),
                    window_reshown,
                    Some((&pending_spawns, secondary)),
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
                            // The guest's own `detail` is part of the message: the
                            // host turns it into a situation-specific hint (an
                            // implicit-intent `startActivity` has no Settings/browser
                            // to reach, which is a different next action from "this
                            // launcher has no second Activity").
                            "error": format!(
                                "the app called {} and the launcher has no multi-Activity path; the guest stayed alive and no Activity was fabricated{}",
                                marker.get("call").and_then(|v| v.as_str()).unwrap_or("startActivity"),
                                marker
                                    .get("detail")
                                    .and_then(|v| v.as_str())
                                    .map(|detail| format!(" ({detail})"))
                                    .unwrap_or_default()
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
                                    "the app called {} and the launcher has no multi-Activity path{}",
                                    marker.as_ref().and_then(|m| m.get("call")).and_then(|v| v.as_str())
                                        .unwrap_or("startActivity"),
                                    marker
                                        .as_ref()
                                        .and_then(|m| m.get("detail"))
                                        .and_then(|v| v.as_str())
                                        .map(|detail| format!(" ({detail})"))
                                        .unwrap_or_default()
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
                // The window this gesture went to is gone (dialog dismissed, task
                // finished): the host's fact, checked against the *delivered*
                // token only.
                let window_removed = injected.token.0 != 0
                    && live
                        .input
                        .window_removed_since(injected.token, removal_mark);
                if window_removed {
                    trace_host(
                        probe_state,
                        &format!(
                            "{cmd} delivered window {:#x} was removed within the step",
                            injected.token.0
                        ),
                    );
                }
                if settled_probe.is_none() {
                    // P12b: a settle timeout is still an observation. Return the
                    // latest tree and C1 frame identity with `settled:false` so
                    // an agent can use what is on screen; `acos_assert`/verify
                    // refuse it as evidence. When there is no probe at all there
                    // is nothing honest to return, so that stays NO_FRESH_FRAME.
                    let Some(latest) = latest_probe(probe_state) else {
                        let reply = serde_json::json!({
                            "ok": false, "cmd": cmd, "code": "NO_FRESH_FRAME",
                            "error": format!(
                                "{cmd} {gesture_label} completed no settled observation within {} ms \
                                 (c1_generation {g0} -> {g1}, probe_seq {p0} -> {p1}) and the \
                                 in-guest probe has produced no sample",
                                settle_timeout.as_millis()
                            ),
                            "gesture": {
                                "seqs": injected.seqs,
                                "acks": injected.acks.iter().map(|a| serde_json::json!({
                                    "seq": a.seq, "handled": a.handled, "latency_us": a.latency_us,
                                })).collect::<Vec<_>>(),
                                "token": format!("{:#x}", injected.token.0),
                                "window_removed": window_removed,
                            },
                            "settle": settle_timing.to_json(false, ack_ms),
                        });
                        protocol_write(&reply);
                        evidence.push(reply);
                        continue;
                    };
                    let capture = match capture_c1(live, frames_dir, &mut captures) {
                        Ok(capture) => capture,
                        Err(error) => {
                            let code = if is_disk_full(&error) {
                                "HOST_DISK_FULL"
                            } else {
                                "INTERNAL"
                            };
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
                    let mut reply = match &capture {
                        Some(capture) => {
                            last_capture_fnv = Some(capture.fnv1a);
                            capture.fields()
                        }
                        None => serde_json::json!({
                            "frame_seq": captures,
                            "capture_source": CAPTURE_SOURCE,
                            "frame_identity": "no C1 buffer has been stored yet",
                        }),
                    };
                    let map = reply.as_object_mut().expect("timeout reply is an object");
                    map.insert("ok".into(), serde_json::json!(true));
                    map.insert("cmd".into(), serde_json::json!(cmd));
                    map.insert("settled".into(), serde_json::json!(false));
                    map.insert("bound".into(), serde_json::json!("timeout"));
                    map.insert("tree".into(), latest.tree.clone());
                    map.insert("probe_seq".into(), serde_json::json!(latest.seq));
                    map.insert("probe_at_ms".into(), serde_json::json!(latest.at_ms));
                    map.insert(
                        "gesture".into(),
                        serde_json::json!({
                            "seqs": injected.seqs,
                            "acks": injected.acks.iter().map(|a| serde_json::json!({
                                "seq": a.seq, "handled": a.handled, "latency_us": a.latency_us,
                            })).collect::<Vec<_>>(),
                            "token": format!("{:#x}", injected.token.0),
                            "window_removed": window_removed,
                        }),
                    );
                    let mut settle_json = settle_timing.to_json(false, ack_ms);
                    let health = probe_health_json(probe_state);
                    if let Some(set) = settle_json.as_object_mut() {
                        set.insert("probe".into(), health.clone());
                    }
                    map.insert("settle".into(), settle_json);
                    map.insert("probe".into(), health);
                    map.insert(
                        "warning".into(),
                        serde_json::json!(format!(
                            "strict settle timed out after {} ms (reason: {}); the tree and frame \
                             identity are the latest observed state, not a settled one, and this is \
                             not assertion evidence",
                            settle_timeout.as_millis(),
                            settle_timing.reason.as_deref().unwrap_or("timeout"),
                        )),
                    );
                    let new_prompts: Vec<serde_json::Value> = live
                        .prompts
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .since(prompt_cursor)
                        .into_iter()
                        .map(|p| p.to_json())
                        .collect();
                    if !new_prompts.is_empty() {
                        map.insert("capability_prompts".into(), serde_json::json!(new_prompts));
                    }
                    protocol_write(&reply);
                    evidence.push(reply);
                    continue;
                }
                let capture = match capture_c1(live, frames_dir, &mut captures) {
                    // ROT8b: a finished task never publishes a capture, even if
                    // the compositor still holds a stale buffer of the
                    // destroyed window (it would be the dead window's pixels
                    // under a new frame id).
                    Ok(_) if settle_timing.task_finished => {
                        // ROT8: the app's last window is gone, so the
                        // compositor holds no layer and there is no frame to
                        // bind. The step is proven by the guest's STATE (task
                        // finished, empty window forest), not by a frame; none
                        // is invented.
                        let probe = settled_probe.expect("settled probe checked above");
                        // The logical frame id advances (as on every pixel-less
                        // tree reply, P9) so the observation is ordered after
                        // the previous one; no buffer is claimed.
                        captures += 1;
                        let mut reply = serde_json::json!({
                            "ok": true, "cmd": cmd,
                            "frame_seq": captures,
                            "pixels": false,
                            "capture_source": CAPTURE_SOURCE,
                            "frame_identity": "no frame: the app's task finished and no window layer remains",
                            "window_gone": true,
                            "task_finished": true,
                            "tree": probe.tree.clone(),
                            "probe_seq": probe.seq,
                            "probe_at_ms": probe.at_ms,
                            "gesture": {
                                "x": x, "y": y,
                                "seqs": injected.seqs,
                                "acks": injected.acks.iter().map(|a| serde_json::json!({
                                    "seq": a.seq, "handled": a.handled, "latency_us": a.latency_us,
                                })).collect::<Vec<_>>(),
                                "token": format!("{:#x}", injected.token.0),
                                "window_removed": window_removed,
                                "new_buffer_generation": g1,
                                "new_probe_seq": p1,
                            },
                            "settle": settle_timing.to_json(true, ack_ms),
                        });
                        let new_prompts: Vec<serde_json::Value> = live
                            .prompts
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .since(prompt_cursor)
                            .into_iter()
                            .map(|p| p.to_json())
                            .collect();
                        if !new_prompts.is_empty() {
                            if let Some(obj) = reply.as_object_mut() {
                                obj.insert(
                                    "capability_prompts".into(),
                                    serde_json::json!(new_prompts),
                                );
                            }
                        }
                        protocol_write(&reply);
                        evidence.push(serde_json::json!({
                            "cmd": cmd, "ok": true, "window_gone": true,
                            "frame_seq": captures,
                            "probe_seq": probe.seq, "x": x, "y": y,
                            "handled": injected.acks.iter().map(|a| a.handled).collect::<Vec<_>>(),
                        }));
                        continue;
                    }
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
                        let code = if is_disk_full(&error) {
                            "HOST_DISK_FULL"
                        } else {
                            "INTERNAL"
                        };
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
                        "window_removed": window_removed,
                        "new_buffer_generation": g1,
                        "new_probe_seq": p1,
                    }),
                );
                map.insert("settle".into(), settle_timing.to_json(true, ack_ms));
                let new_prompts: Vec<serde_json::Value> = live
                    .prompts
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .since(prompt_cursor)
                    .into_iter()
                    .map(|p| p.to_json())
                    .collect();
                if !new_prompts.is_empty() {
                    map.insert("capability_prompts".into(), serde_json::json!(new_prompts));
                }
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
            // MPROC1: the session control op (`acos_service_ctl`). The capture
            // target is a session service, so the agent reads and scripts it
            // through here: `{cmd:"ctl", module:"intents", op:"last"|"list"|
            // "clear"|"result"}`. It never fabricates a handler: a capture says
            // `handled_by:"captured"`.
            "ctl" => {
                let reply = session_ctl(&request, &live.intents, &staging, secondary);
                protocol_write(&reply);
                evidence.push(reply);
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
// minimal RGBA8 PNG writer lives here: real deflate (fdeflate, the fast deflate
// tuned for PNG) over a per-row adaptive Sub/Up filter, real CRC32/Adler-32.
// Deterministic and byte-checkable.
//
// ADR-0027 #6: the encoder used to write zlib *stored* blocks with filter 0 —
// pixel-exact but 10 371 258 B for a 1080x2400 frame, so every capture wrote
// (and hashed) 9.9 MB. Same pixels, real deflate: ~80 KB (measured on the
// counter/form-validation/dialog-menu frames), and the encode is *faster* than
// the stored-block framing it replaces because the 9.9 MB write+MD5 is gone.
// The file stays losslessly exact: pixel assertions decode it, so the bytes
// inside are unchanged.

/// PNG filter types this encoder emits: 1 = Sub, 2 = Up.
const FILTER_SUB: u8 = 1;
const FILTER_UP: u8 = 2;

/// Every Nth byte is sampled when choosing a row's filter (SHOTZ: `step_by`
/// sampling picks the same filter as the exhaustive sum for these frames at
/// ~1/8 the cost, so the choice costs ~0.05 ms instead of ~0.4 ms per row).
const FILTER_SAMPLE_STEP: usize = 8;

/// Filtered rows are handed to the deflater in blocks of this size, so `rgba`
/// is never copied into a frame-sized buffer and the deflater is called ~160
/// times per frame instead of once per row.
const DEFLATE_BLOCK: usize = 64 * 1024;

/// IDAT payloads are emitted in chunks of at most this size. A real frame's
/// zlib stream is ~80 KB (one chunk); the size keeps the 12-byte-per-chunk
/// framing irrelevant even for the stored-block floor.
const IDAT_CHUNK: usize = 1 << 20;

/// The buffered encoder, `#[cfg(test)]` only: the reference the streamed
/// encoder is compared against.
#[cfg(test)]
fn encode_png_rgba(width: u32, height: u32, stride: u32, rgba: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(128 * 1024);
    encode_png_rgba_stream(width, height, stride, rgba, |bytes| {
        out.extend_from_slice(bytes)
    });
    out
}

/// Encode the frame and hand its bytes to `sink` as they are produced.
///
/// The caller writes and hashes the PNG in the same pass (P2), so the finished
/// file is never held in memory as a second copy: the sink sees the signature,
/// the header, each IDAT chunk and the trailer in order.
fn encode_png_rgba_stream<S: FnMut(&[u8])>(
    width: u32,
    height: u32,
    stride: u32,
    rgba: &[u8],
    mut sink: S,
) {
    encode_png_rgba_into(width, height, stride, rgba, &mut sink)
}

fn encode_png_rgba_into(
    width: u32,
    height: u32,
    stride: u32,
    rgba: &[u8],
    sink: &mut dyn FnMut(&[u8]),
) {
    let t0 = m5_now_us();
    let row = (width as usize).saturating_mul(4);
    let stride_bytes = (stride as usize).saturating_mul(4);
    sink(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]);
    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]); // 8-bit RGBA, no interlace
    png_chunk(sink, b"IHDR", &ihdr, crc32);

    // Filter + deflate. The zlib stream is deflated into a small buffer (~80 KB
    // at 1080x2400) and then written as one or more IDAT chunks: the row that
    // is filtered lives in `row_buf`, and only the compressed bytes are held.
    let t_filter = m5_now_us();
    let mut zlib: Vec<u8> = Vec::with_capacity(row.saturating_mul(height as usize) / 64 + 4096);
    {
        let mut deflater = fdeflate::Compressor::new(&mut zlib).expect("fdeflate header");
        let mut prev = vec![0u8; row];
        let mut cur = vec![0u8; row];
        let mut block: Vec<u8> = Vec::with_capacity(DEFLATE_BLOCK + row + 1);
        for y in 0..height as usize {
            let start = y.saturating_mul(stride_bytes);
            let end = start.saturating_add(row);
            if end <= rgba.len() {
                cur.copy_from_slice(&rgba[start..end]);
            } else if start < rgba.len() {
                // A row whose end is past the buffer is zero-padded, exactly as
                // the stored-block encoder did: the frame is never refused for a
                // short last row.
                let have = rgba.len() - start;
                cur[..have].copy_from_slice(&rgba[start..]);
                cur[have..].fill(0);
            } else {
                cur.fill(0);
            }
            // Filter straight into the deflate block: `block` gets the filter
            // byte and then the deltas, so no row-sized scratch buffer and no
            // second copy of the row exist.
            let kind = choose_filter(&cur, &prev);
            block.push(kind);
            let base = block.len();
            block.resize(base + row, 0);
            let deltas = &mut block[base..];
            if kind == FILTER_SUB {
                for i in 0..row {
                    let left = if i >= 4 { cur[i - 4] } else { 0 };
                    deltas[i] = cur[i].wrapping_sub(left);
                }
            } else {
                for i in 0..row {
                    deltas[i] = cur[i].wrapping_sub(prev[i]);
                }
            }
            if block.len() >= DEFLATE_BLOCK {
                deflater.write_data(&block).expect("fdeflate write");
                block.clear();
            }
            std::mem::swap(&mut prev, &mut cur);
        }
        if !block.is_empty() {
            deflater.write_data(&block).expect("fdeflate write");
            block.clear();
        }
        deflater.finish().expect("fdeflate finish");
    }
    // Never worse than the stored blocks this replaced: deflate cannot expand
    // *real* UI frames (they shrink ~130x), but a pathological high-entropy
    // frame would (fixed-Huffman overhead), so the stored layout is the floor.
    let filtered_len = row.saturating_add(1).saturating_mul(height as usize);
    let stored_len = 2 + filtered_len.div_ceil(65_535) * 5 + filtered_len + 4;
    // Compare what the file would cost, including the per-IDAT-chunk framing
    // (12 bytes each): the choice is about the artifact, not the stream.
    let framed = |len: usize| len + 12 * len.max(1).div_ceil(IDAT_CHUNK);
    let mut fallback = false;
    if framed(zlib.len()) > framed(stored_len) {
        zlib.clear();
        zlib_stored_rows(&mut zlib, width, height, stride, rgba);
        fallback = true;
    }
    let t_deflate = m5_now_us();
    m5_trace(
        "art.encode_deflate",
        t_filter,
        t_deflate,
        &format!(
            "fdeflate={} zlib_bytes={} row_filter=sample{FILTER_SAMPLE_STEP}",
            if fallback { 0 } else { 1 },
            zlib.len()
        ),
    );
    m5_trace("art.encode_filter", t0, t_filter, "streamed=1");
    for chunk in zlib.chunks(IDAT_CHUNK) {
        png_chunk(sink, b"IDAT", chunk, crc32);
    }
    m5_trace(
        "art.encode_idat_chunk",
        t_deflate,
        m5_now_us(),
        &format!(
            "chunks={} body_bytes={}",
            zlib.len().div_ceil(IDAT_CHUNK),
            zlib.len()
        ),
    );
    png_chunk(sink, b"IEND", &[], crc32);
    m5_trace(
        "art.encode_total",
        t0,
        m5_now_us(),
        &format!("png_bytes={}", zlib.len()),
    );
}

/// The stored-block zlib stream for a frame: filter 0 per row, no compression.
///
/// Only used as the floor for a frame deflate cannot shrink (and by the tests
/// as the pre-SHOTZ baseline). It reads rows straight out of `rgba` in 64 KiB
/// blocks with the same zero-padding rule as the encoder, so it never holds a
/// frame-sized buffer.
fn zlib_stored_rows(out: &mut Vec<u8>, width: u32, height: u32, stride: u32, rgba: &[u8]) {
    let row = (width as usize).saturating_mul(4);
    let stride_bytes = (stride as usize).saturating_mul(4);
    let filtered_row = row.saturating_add(1);
    let filtered_len = filtered_row.saturating_mul(height as usize);
    out.extend_from_slice(&[0x78, 0x01]);
    let mut adler = Adler32::new();
    let mut block: Vec<u8> = Vec::with_capacity(65_535);
    let mut offset = 0usize;
    while offset < filtered_len {
        let len = (filtered_len - offset).min(65_535);
        let final_block = offset + len == filtered_len;
        block.clear();
        block.push(u8::from(final_block));
        block.extend_from_slice(&(len as u16).to_le_bytes());
        block.extend_from_slice(&(!(len as u16)).to_le_bytes());
        for index in offset..offset + len {
            if filtered_row == 0 {
                block.push(0);
                continue;
            }
            let y = index / filtered_row;
            let within = index % filtered_row;
            let byte = if within == 0 {
                0
            } else {
                let at = y.saturating_mul(stride_bytes).saturating_add(within - 1);
                if y < height as usize && within - 1 < row && at < rgba.len() {
                    rgba[at]
                } else {
                    0
                }
            };
            block.push(byte);
        }
        adler.update(&block[5..]);
        out.extend_from_slice(&block);
        offset += len;
    }
    out.extend_from_slice(&adler.finish().to_be_bytes());
}

/// Pick the PNG filter for one row: Sub (1) or Up (2), whichever leaves the
/// smaller sum of absolute deltas over a sampled subset of the row.
///
/// Sampling is what makes the choice nearly free. On the four real frames this
/// was measured on (counter, form-validation, dialog-menu, text-input) the
/// sampled choice produced the same decision as the exhaustive sum and the same
/// PNG size to within 4 % (`results/handoff/SHOTZ.md`).
fn choose_filter(cur: &[u8], prev: &[u8]) -> u8 {
    let mut sub = 0u64;
    let mut up = 0u64;
    for i in (0..cur.len()).step_by(FILTER_SAMPLE_STEP) {
        let left = if i >= 4 { cur[i - 4] } else { 0 };
        sub += (cur[i].wrapping_sub(left) as i8).unsigned_abs() as u64;
        up += (cur[i].wrapping_sub(prev[i]) as i8).unsigned_abs() as u64;
    }
    if sub <= up {
        FILTER_SUB
    } else {
        FILTER_UP
    }
}

/// The historic catch encoder: filter 0 per row, zlib stored blocks.
///
/// Kept under `cfg(test)` as the A/B baseline the new encoder is measured
/// against (`png_encoder_candidates_on_real_frames`), so the "before" numbers in
/// `results/handoff/SHOTZ.md` come from the code v0.2.0 shipped, not from a
/// separate tool.
#[cfg(test)]
fn encode_png_stored_stream<S: FnMut(&[u8])>(
    width: u32,
    height: u32,
    stride: u32,
    rgba: &[u8],
    mut sink: S,
) {
    let row = (width as usize).saturating_mul(4);
    let stride_bytes = (stride as usize).saturating_mul(4);
    let filtered_len = row.saturating_add(1).saturating_mul(height as usize);
    sink(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]);
    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
    png_chunk(&mut sink, b"IHDR", &ihdr, crc32);
    let block_count = filtered_len.div_ceil(65_535);
    let idat_len = 2 + block_count * 5 + filtered_len + 4;
    sink(&(idat_len as u32).to_be_bytes());
    sink(b"IDAT");
    let mut crc = Crc32Stream::new();
    crc.update(b"IDAT");
    sink(&[0x78, 0x01]);
    crc.update(&[0x78, 0x01]);
    let mut adler = Adler32::new();
    let mut bytes = Vec::with_capacity(filtered_len + block_count * 5);
    for y in 0..height as usize {
        let start = y.saturating_mul(stride_bytes);
        let end = start.saturating_add(row);
        bytes.push(0);
        if end <= rgba.len() {
            bytes.extend_from_slice(&rgba[start..end]);
        } else if start < rgba.len() {
            let have = rgba.len() - start;
            bytes.extend_from_slice(&rgba[start..start + have]);
            bytes.resize(bytes.len() + (row - have), 0);
        } else {
            bytes.resize(bytes.len() + row, 0);
        }
    }
    adler.update(&bytes);
    let mut offset = 0usize;
    while offset < filtered_len {
        let len = (filtered_len - offset).min(65_535);
        let final_block = offset + len == filtered_len;
        let mut header = [0u8; 5];
        header[0] = if final_block { 1 } else { 0 };
        header[1..3].copy_from_slice(&(len as u16).to_le_bytes());
        header[3..5].copy_from_slice(&(!(len as u16)).to_le_bytes());
        sink(&header);
        crc.update(&header);
        sink(&bytes[offset..offset + len]);
        crc.update(&bytes[offset..offset + len]);
        offset += len;
    }
    let adler = adler.finish().to_be_bytes();
    sink(&adler);
    crc.update(&adler);
    sink(&crc.finish().to_be_bytes());
    png_chunk(&mut sink, b"IEND", &[], crc32);
}

fn png_chunk(sink: &mut dyn FnMut(&[u8]), kind: &[u8; 4], body: &[u8], crc32: fn(&[u8]) -> u32) {
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

/// Escape one `<meta-data>` name or value for the guest's `ACOSLAUNCH_PROVIDERS`
/// field encoding: the record uses `|`, `;` and `=` as separators and newlines
/// as record terminators, so those characters (and the escape itself) are
/// spelled out.
fn meta_spec_escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '|' => out.push_str("\\p"),
            ';' => out.push_str("\\s"),
            '=' => out.push_str("\\e"),
            '\n' => out.push_str("\\n"),
            other => out.push(other),
        }
    }
    out
}

/// Encode a component's `<meta-data>` for the guest: `name=type+value` entries
/// joined with `;`. The type letters are the four `Bundle` value types
/// `PackageParser.parseMetaData` stores — `s`tring, `i`nt (an
/// `android:resource` reference is an int id, exactly as the platform stores
/// it), `b`ool and `f`loat.
fn meta_spec(entries: &[acos_services::pms::MetaDataEntry]) -> String {
    use acos_services::pms::MetaDataValue;
    entries
        .iter()
        .map(|entry| {
            let (kind, value) = match &entry.value {
                MetaDataValue::String(text) => ('s', text.clone()),
                MetaDataValue::Int(number) => ('i', number.to_string()),
                MetaDataValue::Boolean(flag) => ('b', u8::from(*flag).to_string()),
                MetaDataValue::Float(number) => ('f', number.to_string()),
                MetaDataValue::Reference(id) => ('i', (*id as i32).to_string()),
            };
            format!("{}={kind}{value}", meta_spec_escape(&entry.name))
        })
        .collect::<Vec<_>>()
        .join(";")
}

struct ServiceControlReply {
    reply: serde_json::Value,
    guest_commands: Vec<acos_services::GuestCommand>,
}

/// ADR-0029 item 4 + 2026-11-18 amendment: live module attach/detach/ctl
/// crosses from the interactive command loop (the thread that answers the
/// caller and owns the guest pipes) to the peer thread (the only owner of
/// `ServiceHost`). Each reply carries both the module's JSON result and any
/// typed [`acos_services::GuestCommand`]s queued onto `ServiceHost`'s outbox.
#[derive(Default)]
struct ServiceControl {
    requests: Mutex<Vec<(u64, serde_json::Value)>>,
    replies: Mutex<HashMap<u64, Option<ServiceControlReply>>>,
    next_id: AtomicU64,
    reply_cv: Condvar,
    /// The peer's wake-pipe write end (`-1` until the launcher installs it). A
    /// request must wake the peer's idle `poll(2)`: with no timed module the peer
    /// sleeps until the guest speaks, so a queued request would otherwise wait
    /// for a frame that never comes.
    wake: std::sync::atomic::AtomicI32,
}

impl ServiceControl {
    fn set_wake(&self, fd: RawFd) {
        self.wake.store(fd, Ordering::Release);
    }
}

impl ServiceControl {
    fn submit(
        &self,
        request: serde_json::Value,
        timeout: Duration,
    ) -> Option<ServiceControlReply> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed) + 1;
        {
            let mut replies = self.replies.lock().unwrap_or_else(|e| e.into_inner());
            replies.insert(id, None);
        }
        {
            let mut requests = self.requests.lock().unwrap_or_else(|e| e.into_inner());
            requests.push((id, request));
        }
        let wake = self.wake.load(Ordering::Acquire);
        if wake >= 0 {
            let byte = 1u8;
            unsafe {
                libc::write(wake, (&byte as *const u8).cast::<libc::c_void>(), 1);
            }
        }
        let deadline = Instant::now() + timeout;
        let mut replies = self.replies.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            if replies.get(&id).is_some_and(Option::is_some) {
                return replies.remove(&id).flatten();
            }
            let now = Instant::now();
            if now >= deadline {
                if let Some(Some(reply)) = replies.remove(&id) {
                    return Some(reply);
                }
                drop(replies);
                self.requests
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .retain(|(req_id, _)| *req_id != id);
                return None;
            }
            let (guard, _) = self
                .reply_cv
                .wait_timeout(replies, deadline - now)
                .unwrap_or_else(|e| e.into_inner());
            replies = guard;
        }
    }

    /// Peer-thread side: the oldest pending request, if any.
    fn take(&self) -> Option<(u64, serde_json::Value)> {
        let mut requests = self.requests.lock().unwrap_or_else(|e| e.into_inner());
        if requests.is_empty() {
            None
        } else {
            Some(requests.remove(0))
        }
    }

    /// Peer-thread side: publish the answer and wake the caller.
    ///
    /// Returns `false` if the caller already timed out and removed its slot.
    fn reply(&self, id: u64, value: ServiceControlReply) -> bool {
        let mut replies = self.replies.lock().unwrap_or_else(|e| e.into_inner());
        let Some(slot) = replies.get_mut(&id) else {
            return false;
        };
        *slot = Some(value);
        self.reply_cv.notify_all();
        true
    }
}

#[derive(Default)]
struct PeerSummary {
    /// MPROC4 step 3b: the activity manager's process list at the end of the
    /// session (`tag`, package, uid, pid, app thread, attached).
    processes: Vec<serde_json::Value>,
    /// Spawn requests the peer handed to the launcher.
    spawn_requests: u64,
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

/// Roll back a hot-attach that succeeded on the peer thread after the
/// launcher's `ServiceControl::submit` timed out and abandoned the operation.
///
/// Without this, `system.settings` stays attached and `com.android.settings`
/// remains installed in PMS/ATMS while `SecondaryPool` never received a spec,
/// breaking both process spawns and subsequent retries.
fn rollback_timed_out_hot_attach(
    peer: &mut Peer,
    modules: &mut acos_services::module::ModuleRegistry,
    request: &serde_json::Value,
    reply: &serde_json::Value,
) -> bool {
    if request.get("op").and_then(|v| v.as_str()) != Some("attach") {
        return false;
    }
    let Some(extra) = request.get("_hot_extra_apk").and_then(|v| v.as_object()) else {
        return false;
    };
    if reply.get("ok").and_then(|v| v.as_bool()) != Some(true) {
        return false;
    }
    let Some(id_str) = request.get("id").and_then(|v| v.as_str()) else {
        return false;
    };
    let package = extra
        .get("package")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or(acos_services::modules::settings::SETTINGS_PACKAGE);
    let _ = modules.detach_live(peer.host_mut(), id_str);
    peer.host_mut().remove_extra_package(package);
    true
}

/// One `{"cmd":"services", ...}` operation against the peer thread's host.
///
/// `list`/`status` are read-only. `attach`/`detach` go through
/// `attach_live`/`detach_live`, which refuse an `at-launch` module with the
/// launch call in the message; a refusal is never turned into a success.
fn service_request(
    peer: &mut Peer,
    modules: &mut acos_services::module::ModuleRegistry,
    request: &serde_json::Value,
) -> serde_json::Value {
    let op = request.get("op").and_then(|v| v.as_str()).unwrap_or("");
    let result = match op {
        "list" | "status" => {
            let mut body = serde_json::json!({
                "attached": modules.attached().iter().map(|(id, profile)| serde_json::json!({
                    "id": id,
                    "profile": profile.as_str(),
                })).collect::<Vec<_>>(),
                "modules": modules.verdict_modules(),
            });
            if let Some(prompts_arc) = peer.host_mut().capability_prompts() {
                let prompts: Vec<serde_json::Value> = prompts_arc
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .recent(16)
                    .into_iter()
                    .map(|p| p.to_json())
                    .collect();
                if !prompts.is_empty() {
                    if let Some(obj) = body.as_object_mut() {
                        obj.insert("capability_prompts".into(), serde_json::json!(prompts));
                    }
                }
            }
            Ok(body)
        }
        "attach" => {
            let Some(id) = request.get("id").and_then(|v| v.as_str()) else {
                return serde_json::json!({
                    "ok": false, "cmd": "services", "code": "BAD_REQUEST",
                    "error": "attach needs a string `id`",
                });
            };
            let profile = match request.get("profile").and_then(|v| v.as_str()) {
                None => acos_services::module::Profile::Real,
                Some(name) => match acos_services::module::Profile::parse(name) {
                    Some(profile) => profile,
                    None => {
                        return serde_json::json!({
                            "ok": false, "cmd": "services", "code": "BAD_REQUEST",
                            "error": format!("unknown profile {name:?}"),
                        })
                    }
                },
            };
            let config = match request.get("config") {
                None => acos_services::module::ModuleConfig::default(),
                Some(value) => {
                    match acos_services::module::ModuleConfig::from_json("<live>", value) {
                        Ok(config) => config,
                        Err(error) => {
                            return serde_json::json!({
                                "ok": false, "cmd": "services", "code": "BAD_REQUEST",
                                "error": error.to_string(),
                            })
                        }
                    }
                }
            };
            modules
                .attach_live(peer.host_mut(), id, profile, &config)
                .and_then(|()| {
                    let mut hot_uid: Option<i32> = None;
                    if let Some(extra) = request.get("_hot_extra_apk").and_then(|v| v.as_object()) {
                        let staged = extra.get("staged").and_then(|v| v.as_str()).unwrap_or("");
                        let guest_path = extra.get("guest_path").and_then(|v| v.as_str());
                        let guest_lib_dir = extra.get("guest_lib_dir").and_then(|v| v.as_str());
                        let package = extra
                            .get("package")
                            .and_then(|v| v.as_str())
                            .filter(|s| !s.is_empty())
                            .unwrap_or(acos_services::modules::settings::SETTINGS_PACKAGE);
                        if !staged.is_empty() {
                            let staged_path = Path::new(staged);
                            let uid = match peer.host_mut().install_extra_package_from_apk_as(
                                staged_path,
                                guest_path,
                                guest_lib_dir,
                            ) {
                                Ok(uid) => uid,
                                Err(err) => {
                                    let _ = modules.detach_live(peer.host_mut(), id);
                                    return Err(acos_services::ServiceError::Internal(format!(
                                        "hot install_extra_package_from_apk_as failed: {err}"
                                    )));
                                }
                            };
                            if let Ok(object) = peer
                                .host_mut()
                                .ensure("android.app.IActivityTaskManager")
                            {
                                if let Some(atms) = object
                                    .as_any_mut()
                                    .and_then(|any| any.downcast_mut::<acos_services::atms::AtmsService>())
                                {
                                    if let Err(err) =
                                        atms.seed_manifest_from_apk_with_uid(staged_path, uid)
                                    {
                                        let _ = modules.detach_live(peer.host_mut(), id);
                                        peer.host_mut().remove_extra_package(package);
                                        return Err(acos_services::ServiceError::Internal(format!(
                                            "hot seed_manifest_from_apk_with_uid failed: {err}"
                                        )));
                                    }
                                }
                            }
                            hot_uid = Some(uid);
                        }
                    }
                    let mut body = serde_json::json!({
                        "attached": modules.attached().iter().map(|(id, profile)| serde_json::json!({
                            "id": id,
                            "profile": profile.as_str(),
                        })).collect::<Vec<_>>(),
                        "modules": modules.verdict_modules(),
                    });
                    if let (Some(uid), Some(obj)) = (hot_uid, body.as_object_mut()) {
                        obj.insert("_hot_extra_uid".into(), serde_json::json!(uid));
                    }
                    Ok(body)
                })
        }
        "detach" => {
            let Some(id) = request.get("id").and_then(|v| v.as_str()) else {
                return serde_json::json!({
                    "ok": false, "cmd": "services", "code": "BAD_REQUEST",
                    "error": "detach needs a string `id`",
                });
            };
            modules.detach_live(peer.host_mut(), id).map(|()| {
                if id == acos_services::modules::settings::MODULE_ID {
                    peer.host_mut()
                        .remove_extra_package(acos_services::modules::settings::SETTINGS_PACKAGE);
                }
                serde_json::json!({
                    "attached": modules.attached().iter().map(|(id, profile)| serde_json::json!({
                        "id": id,
                        "profile": profile.as_str(),
                    })).collect::<Vec<_>>(),
                    "modules": modules.verdict_modules(),
                })
            })
        }
        "replay_intent" => {
            let Some(intent_id) = request.get("intent_id").and_then(|v| v.as_u64()) else {
                return serde_json::json!({
                    "ok": false, "cmd": "services", "op": "replay_intent", "code": "BAD_REQUEST",
                    "error": "replay_intent needs an integer `intent_id`",
                });
            };
            match peer.host_mut().replay_saved_intent(intent_id) {
                Ok(code) if code >= 0 => Ok(serde_json::json!({
                    "intent_id": intent_id,
                    "start_result": code,
                })),
                Ok(code) => {
                    return serde_json::json!({
                        "ok": false,
                        "cmd": "services",
                        "op": "replay_intent",
                        "code": "START_NOT_FOUND",
                        "start_result": code,
                        "error": format!(
                            "replayed intent {intent_id} was not resolved (start_result={code})"
                        ),
                    });
                }
                Err(error) => Err(error),
            }
        }
        "launch_home" => match peer.host_mut().launch_home() {
            Ok(code) => Ok(serde_json::json!({ "start_result": code })),
            Err(acos_services::ServiceError::IllegalState { detail, .. }) => {
                return serde_json::json!({
                    "ok": false,
                    "cmd": "services",
                    "code": "HOME_UNAVAILABLE",
                    "error": detail,
                });
            }
            Err(error) => Err(error),
        },
        "ctl" => {
            // `acos_service_ctl` (MCP) / a scenario `service_ctl` step: a
            // module's own control op (`sensors.set_value`, `telephony.sms`, ...).
            // The module gets the session's host ([`Module::control`]) so a
            // control that changes what the app observes can deliver it through
            // the same path the framework uses; the peer thread flushes those
            // deliveries after the control returns. The registry refuses a
            // detached module, so a control can never look applied when the
            // module is off.
            let Some(id) = request.get("id").and_then(|v| v.as_str()) else {
                return serde_json::json!({
                    "ok": false, "cmd": "services", "op": "ctl", "code": "BAD_REQUEST",
                    "error": "ctl needs a string `id`",
                });
            };
            let Some(ctl_op) = request.get("ctl_op").and_then(|v| v.as_str()) else {
                return serde_json::json!({
                    "ok": false, "cmd": "services", "op": "ctl", "code": "BAD_REQUEST",
                    "error": "ctl needs a string `ctl_op` (the module's control op)",
                });
            };
            let args = request
                .get("args")
                .cloned()
                .unwrap_or(serde_json::Value::Null);
            modules.control(peer.host_mut(), id, ctl_op, &args).map(|value| {
                serde_json::json!({
                    "id": id,
                    "op": ctl_op,
                    "value": value,
                    "attached": modules.attached().iter().map(|(id, profile)| serde_json::json!({
                        "id": id,
                        "profile": profile.as_str(),
                    })).collect::<Vec<_>>(),
                    "modules": modules.verdict_modules(),
                })
            })
        }
        other => {
            return serde_json::json!({
                "ok": false, "cmd": "services", "code": "BAD_REQUEST",
                "error": format!("unknown op {other:?}; use list, status, attach, detach, ctl or replay_intent"),
            })
        }
    };
    match result {
        Ok(value) => {
            let mut reply = serde_json::json!({"ok": true, "cmd": "services", "op": op});
            if let (Some(target), Some(object)) = (reply.as_object_mut(), Some(())) {
                let _ = object;
                if let Some(source) = value.as_object() {
                    for (key, item) in source {
                        target.insert(key.clone(), item.clone());
                    }
                }
            }
            reply
        }
        Err(error) => serde_json::json!({
            "ok": false, "cmd": "services", "op": op,
            "code": error.code(),
            "error": error.to_string(),
        }),
    }
}

/// The launcher's half of the peer's idle wait: a self-pipe it writes to when it
/// stops the session, so the peer can sleep in `poll(2)` until a guest frame or
/// the earliest module deadline instead of pollong a socket on a timer.
struct PeerWakeup {
    read: RawFd,
    write: RawFd,
}

impl PeerWakeup {
    fn new() -> std::io::Result<Self> {
        let mut fds = [0 as RawFd; 2];
        let rc = unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC | libc::O_NONBLOCK) };
        if rc < 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(Self {
            read: fds[0],
            write: fds[1],
        })
    }

    /// Wake the peer's idle wait. Idempotent: the pipe is only a wakeup, the
    /// `stop` flag carries the decision.
    fn wake(&self) {
        let byte = 1u8;
        unsafe {
            libc::write(self.write, (&byte as *const u8).cast::<libc::c_void>(), 1);
        }
    }
}

impl Drop for PeerWakeup {
    fn drop(&mut self) {
        unsafe {
            libc::close(self.read);
            libc::close(self.write);
        }
    }
}

/// Write `outs` to the guest and close their fds, reporting the first failure.
fn write_out(fd: RawFd, outs: Vec<Frame>, context: &str) -> std::io::Result<()> {
    for out in outs {
        if let Err(error) = write_frame(fd, &out) {
            eprintln!(
                "peer-trace: write_frame FAILED {context}: {error} (ty={} seq={} code={} data={})",
                out.ty,
                out.seq,
                out.code,
                out.data.len()
            );
            close_frame_fds(&out);
            return Err(error);
        }
        close_frame_fds(&out);
    }
    Ok(())
}
/// Compile the userspace binder driver (`crates/acos-binder-peer/native`) with
/// the NDK's bionic clang into `<data>/lib/libacos_binder_driver.so`.
///
/// FLAKEWIN: the driver's transaction-buffer heap is a second translation unit
/// (`txn_heap.c` + `txn_heap.h`, host-testable on its own). It is embedded and
/// compiled here with the driver, and it is part of the build-cache key: a heap
/// change must never reuse a cached `.so` that still carries the old allocator.
fn build_driver(root: &ArtRoot) -> Result<PathBuf> {
    const SOURCE: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../acos-binder-peer/native/binder_driver.c"
    ));
    const HEAP_SOURCE: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../acos-binder-peer/native/txn_heap.c"
    ));
    const HEAP_HEADER: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../acos-binder-peer/native/txn_heap.h"
    ));
    let dir = root.data.join("lib");
    fs::create_dir_all(&dir)?;
    let out = dir.join("libacos_binder_driver.so");
    let src = dir.join("binder_driver.c");
    let heap_src = dir.join("txn_heap.c");
    let heap_hdr = dir.join("txn_heap.h");
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for b in SOURCE
        .as_bytes()
        .iter()
        .chain(HEAP_SOURCE.as_bytes())
        .chain(HEAP_HEADER.as_bytes())
    {
        hash ^= u64::from(*b);
        hash = hash.wrapping_mul(0x100_0000_01b3);
    }
    let stamp = dir.join("binder_driver.hash");
    if out.is_file()
        && heap_src.is_file()
        && heap_hdr.is_file()
        && fs::read_to_string(&stamp)
            .map(|s| s.trim() == hash.to_string())
            .unwrap_or(false)
    {
        return Ok(out);
    }
    fs::write(&src, SOURCE)?;
    fs::write(&heap_src, HEAP_SOURCE)?;
    fs::write(&heap_hdr, HEAP_HEADER)?;
    let clang = ndk_clang()?;
    // W37 (flagged call-site edit in the W36-owned launch.rs): compile once into
    // the shared content-addressed cache; a cold cohort serialises on the build
    // lock and every later launch only installs the verified `.so`.
    let cache_key = crate::build_cache::digest(&[
        b"acos-art/binder-driver/v3",
        SOURCE.as_bytes(),
        HEAP_SOURCE.as_bytes(),
        HEAP_HEADER.as_bytes(),
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
        .arg(&heap_src)
        .arg("-I")
        .arg(&dir)
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
    let src_dir = crate::app::guest_source_dir();
    let out_dir = root.data.join("app/launch-guest");
    fs::create_dir_all(&out_dir)?;
    let sources = [
        src_dir.join("dev/acos/art/LaunchMain.java"),
        // The software AndroidKeyStore provider (installed by LaunchMain.main,
        // the way the zygote installs the real one before app code).
        src_dir.join("dev/acos/art/SoftwareKeyStore.java"),
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
        let mut parts: Vec<Vec<u8>> = vec![b"acos-art/launch-guest/v3".to_vec()];
        for p in &sources {
            // The source's place in the guest tree, never its absolute path: a
            // worktree (or any other prefix) holding the same bytes is the same key.
            parts.push(
                p.strip_prefix(&src_dir)
                    .unwrap_or(p)
                    .to_string_lossy()
                    .as_bytes()
                    .to_vec(),
            );
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
        // JVM dumps go to ACOS scratch, never the caller's cwd (`crate::jvm`).
        .args(crate::jvm::dump_args())
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
        .args(crate::jvm::d8_dump_args())
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

/// Lowercase hex of `bytes`: the `ACOSLAUNCH_INTENT_BYTES` spelling (the guest
/// unmarshals the Intent payload from it, so an env value never carries a NUL
/// byte or a quoting hazard).
fn hex_bytes(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

/// Read one `key=value` field from a marker file's lines. The guest writes one
/// field per line, so the value is the rest of that line.
fn marker_field<'a>(marker: &'a str, key: &str) -> Option<&'a str> {
    marker
        .lines()
        .find_map(|line| line.strip_prefix(key)?.strip_prefix('=').map(str::trim))
}

/// Read a numeric `key=value` from a marker file's lines.
fn marker_u64(marker: &str, key: &str) -> Option<u64> {
    marker_field(marker, key).and_then(|v| v.parse().ok())
}

/// S3PERF: the skipped-draw reason the guest published in its
/// `activity-resumed.marker`, or `None` when the first traversal drew (or an
/// older guest wrote no evidence at all). `none`/empty means "it drew".
fn resumed_marker_draw_skip(marker: &str) -> Option<String> {
    let value = marker_field(marker, "draw_skip")?;
    if value.is_empty() || value == "none" {
        return None;
    }
    Some(value.to_string())
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
            0,
        );
        let out = state.lock().unwrap().clone_for_test();
        out
    }

    impl ProbeState {
        fn clone_for_test(&self) -> ProbeState {
            ProbeState {
                latest: self.latest.clone(),
                by_process: self.by_process.clone(),
                seq_offsets: self.seq_offsets.clone(),
                last_raw_seq: self.last_raw_seq.clone(),
                highest_seq: self.highest_seq,
                top_process: Arc::clone(&self.top_process),
                unsupported: self.unsupported.clone(),
                unsupported_seq: self.unsupported_seq,
                probe_error: self.probe_error.clone(),
                probe_errors: self.probe_errors,
                guest_launch: self.guest_launch.clone(),
                oversized_lines: self.oversized_lines,
                bad_probe_lines: self.bad_probe_lines,
                trace: None,
                marks: self.marks.clone(),
                wakes_sent: self.wakes_sent,
                wake_failures: self.wake_failures,
                stale_samples: self.stale_samples,
                stall_pending_ms: self.stall_pending_ms,
                wake: Arc::clone(&self.wake),
                anim_traced: self.anim_traced.clone(),
                touch_traced: self.touch_traced,
                commit_text_results: self.commit_text_results.clone(),
                next_commit_text_seq: self.next_commit_text_seq,
            }
        }
    }

    /// MERGEFIX (mproc3): the served tree is the **foreground** process's
    /// sample.
    ///
    /// The launcher builds its probe state with `ProbeState::for_session(top)`,
    /// and that `Arc` is the one ATMS writes when the top-of-task moves. The
    /// merge of main into modules kept *two* declarations — the wired one and
    /// `ProbeState::default()` — and the second shadowed the first, so
    /// `top_process` was a private 0 that nothing ever wrote: every tree reply
    /// described the primary process however far the foreground had moved.
    /// `mproc3-two-process` then timed out on `wait_for id=="finish"`, because
    /// proc-b's window was never the served one.
    ///
    /// ATMS publishes through the same `Arc`; no other wiring step may be
    /// needed for the served sample to follow it.
    #[test]
    fn the_served_sample_is_the_foreground_processes() {
        let atms_top = Arc::new(std::sync::atomic::AtomicU32::new(0));
        let state = Arc::new(std::sync::Mutex::new(ProbeState::for_session(Arc::clone(
            &atms_top,
        ))));
        let sample = |window: &str| {
            format!(
                "ACOSPROBE {{\"seq\":1,\"tree\":{{\"nodes\":[],\"window\":\"{window}\"}}}}\n"
            )
        };
        let a = sample("proca");
        let b = sample("procb");
        drain_stdout_lines(
            std::io::Cursor::new(a.into_bytes()),
            4096,
            Arc::clone(&state),
            None,
            0,
        );
        drain_stdout_lines(
            std::io::Cursor::new(b.into_bytes()),
            4096,
            Arc::clone(&state),
            None,
            1,
        );
        let served_window = |state: &Arc<std::sync::Mutex<ProbeState>>| {
            latest_probe(state).expect("a sample").tree["window"]
                .as_str()
                .unwrap_or_default()
                .to_string()
        };
        assert_eq!(
            served_window(&state),
            "proca",
            "while the primary is the top, its sample is the served one"
        );
        // ATMS's `publish_top(1)`: one store into the `Arc` the session was
        // built with. Nothing else may be needed — this is the wiring the merge
        // lost.
        atms_top.store(1, std::sync::atomic::Ordering::Relaxed);
        assert_eq!(
            served_window(&state),
            "procb",
            "a cross-process launch serves the new foreground process's tree"
        );
        assert_eq!(
            state.lock().unwrap().published_top(),
            1,
            "the session's top is ATMS's Arc, not a private copy"
        );
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

    /// CR1FIX #20: a probe line the guest emitted BEFORE it applied WINDOW can
    /// still be unread when the host writes the command; the drain publishes it
    /// with a host sequence above the pre-command one. Only the guest's own
    /// `window_cmd` counter may acknowledge the command.
    #[test]
    fn a_stale_probe_with_a_higher_seq_does_not_ack_a_window_command() {
        // The host read seq 4 before writing WINDOW (command 1)...
        let before = drain(
            "ACOSPROBE {\"seq\":4,\"window_cmd\":{\"done\":0,\"failed\":0},\"tree\":{\"nodes\":[]}}\n",
        );
        let before_seq = before.latest.expect("baseline sample").seq;
        // ...then the unread pre-command line (seq 5, done=0) is drained.
        let stale = drain(
            "ACOSPROBE {\"seq\":5,\"window_cmd\":{\"done\":0,\"failed\":0},\"tree\":{\"nodes\":[\"old\"]}}\n",
        )
        .latest
        .expect("stale sample");
        assert!(stale.seq > before_seq, "the old sequence rule would accept it");
        assert_eq!(window_ack(&stale, 1, 0), WindowAck::Pending);
        // A sample walked after the guest applied the command acknowledges it.
        let applied = drain(
            "ACOSPROBE {\"seq\":6,\"window_cmd\":{\"done\":1,\"failed\":0},\"tree\":{\"nodes\":[\"new\"]}}\n",
        )
        .latest
        .expect("applied sample");
        assert_eq!(window_ack(&applied, 1, 0), WindowAck::Applied);
        // A guest that predates the counter (no field) can never ack.
        let legacy = drain("ACOSPROBE {\"seq\":9,\"tree\":{\"nodes\":[]}}\n")
            .latest
            .expect("legacy sample");
        assert_eq!(window_ack(&legacy, 1, 0), WindowAck::Pending);
        // The guest's own failure is surfaced, not turned into a tree.
        let failed = drain(
            "ACOSPROBE {\"seq\":7,\"window_cmd\":{\"done\":2,\"failed\":1},\"tree\":{\"nodes\":[]}}\n",
        )
        .latest
        .expect("failed sample");
        assert_eq!(window_ack(&failed, 2, 0), WindowAck::Failed);
        assert_eq!(window_ack(&failed, 2, 1), WindowAck::Applied);
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
            error["error"]
                .as_str()
                .unwrap()
                .contains("NullPointerException"),
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
            window_gone: false,
            animations_on: None,
            window_cmd: None,
        }
    }

    /// PERF4: the fast settle binds the sample's *own* draw only when the
    /// guest/C1 counters prove that draw's buffer is in C1; every other shape
    /// keeps the shipped "a store observed after the sample" rule, so this can
    /// only ever move the bind earlier for a sample whose frame is already
    /// there — never bind a sample from before the action.
    #[test]
    fn a_fast_commit_binds_its_own_draw_only_with_proof() {
        let mut commit = sample(7, 3); // generation (C1 stores) = 3
        commit.draw_seq = 2; // the guest's draw counter
        commit.draw_ms = 1_000; // same clock as the host's store stamps
        let mut baseline = sample(3, 1);
        baseline.draw_seq = 1;
        // Every draw stored, the store arrived after this draw, and the draw is
        // newer than the pre-publish one: bind at once.
        assert!(sample_own_draw_is_stored(&commit, Some(&baseline), 1_004));

        // Stores behind draws: the sample's own buffer may not be in C1 yet.
        let mut behind = commit.clone();
        behind.generation = 1;
        assert!(!sample_own_draw_is_stored(&behind, Some(&baseline), 1_004));

        // The newest store predates this draw: a pre-draw buffer.
        assert!(!sample_own_draw_is_stored(&commit, Some(&baseline), 999));

        // The tree was taken at the pre-publish draw.
        let mut same_draw = commit.clone();
        same_draw.draw_seq = 1;
        assert!(!sample_own_draw_is_stored(&same_draw, Some(&baseline), 1_004));

        // No draw counter on the sample (a non-event sample): never a shortcut.
        let mut no_draw = commit.clone();
        no_draw.draw_seq = 0;
        assert!(!sample_own_draw_is_stored(&no_draw, Some(&baseline), 1_004));

        // No pre-publish sample (a first observation): the draw must still be
        // real and stored.
        assert!(sample_own_draw_is_stored(&commit, None, 1_004));
        assert!(!sample_own_draw_is_stored(&no_draw, None, 1_004));
    }

    /// PERF4: the settle wait is woken by a change signal, and a signal that
    /// arrived before the wait is not lost (the tick is compared, not cleared) —
    /// the two properties the loops depend on now that they no longer sleep.
    #[test]
    fn the_change_wake_is_event_driven_and_loses_no_edge() {
        let wake = std::sync::Arc::new(ProbeWake::default());
        let mut seen = 0u64;

        // No event: the wait spends its whole (short) budget.
        let started = Instant::now();
        wake.wait(&mut seen, Duration::from_millis(5));
        assert!(started.elapsed() >= Duration::from_millis(4));

        // An event that landed *before* the wait returns at once.
        wake.notify();
        let started = Instant::now();
        wake.wait(&mut seen, Duration::from_millis(50));
        assert!(
            started.elapsed() < Duration::from_millis(20),
            "a pending signal must not wait for the timeout: {:?}",
            started.elapsed()
        );

        // An event that lands *while* waiting returns promptly, from another
        // thread (the two producers are the guest-line reader and the peer loop).
        let started = Instant::now();
        let producer = {
            let wake = std::sync::Arc::clone(&wake);
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(10));
                wake.notify();
            })
        };
        wake.wait(&mut seen, Duration::from_millis(500));
        producer.join().expect("producer");
        assert!(
            started.elapsed() < Duration::from_millis(100),
            "a notify during the wait must end it: {:?}",
            started.elapsed()
        );
    }

    /// ROT8b: `state_changed` is computed from the pre-action tree, never
    /// hard-coded.
    #[test]
    fn window_gone_state_changed_is_computed() {
        let full = serde_json::json!({"nodes": [{"id": "x"}]});
        let empty = serde_json::json!({"nodes": []});
        assert!(window_gone_state_changed(Some(&full), false));
        assert!(!window_gone_state_changed(Some(&empty), true));
        assert!(window_gone_state_changed(None, false));
        assert!(!window_gone_state_changed(None, true), "already empty before the action");
    }

    /// ROT8: a probe line carrying `settle.window_gone` parses into the sample,
    /// and only a settled sample with an EMPTY window forest binds a step as
    /// `window-gone`. A stale-looking `window_gone` over a non-empty tree, or an
    /// unsettled one, never does.
    #[test]
    fn window_gone_sample_is_parsed_and_binds_only_with_state_proof() {
        let line = "ACOSPROBE {\"seq\":9,\"windows\":[],\"tree\":{\"nodes\":[],\"window_count\":0,\"node_budget\":2000,\"truncated\":false},\"settle\":{\"settled\":true,\"pending\":[],\"draw_ms\":5,\"draw_seq\":3,\"window_gone\":true,\"task_finished\":true}}\n";
        let state = drain(line);
        let sample = state.latest.expect("window-gone sample published");
        assert!(sample.window_gone && sample.settled);
        assert!(window_gone_shape(&sample));
        let mut not_empty = sample.clone();
        not_empty.tree = serde_json::json!({"nodes": [{"id": "x"}]});
        assert!(!window_gone_shape(&not_empty), "a window forest that is not empty is not gone");
        let mut unsettled = sample.clone();
        unsettled.settled = false;
        assert!(!window_gone_shape(&unsettled));
        let mut plain = sample;
        plain.window_gone = false;
        assert!(!window_gone_shape(&plain), "an ordinary settled empty tree is not a finished task");
        // The step evidence names the proof.
        let timing = SettleTiming {
            mode: SettleMode::Strict,
            first_change_ms: None,
            frame_ms: None,
            state_changed: Some(true),
            bound: "window-gone",
            rule: Some("event"),
            reason: None,
            pixels_changed: Some(false),
            task_finished: true,
            window_reshown: None,
        };
        let json = timing.to_json(true, 5);
        assert_eq!(json["bound"], "window-gone");
        assert_eq!(json["task_finished"], true);
        assert_eq!(json["pixels_changed"], false);
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

    /// SETTLE5: a settle that times out on `probe-floor` must carry the demand
    /// probe's own health, not just "the newest sample is old". The parser turns
    /// the guest's `probe-stalled` mark and a refused (not-newer) sample into
    /// the exact counters the timeout reports, so the next occurrence names the
    /// side that stalled.
    #[test]
    fn a_starved_probe_and_a_stale_sample_are_counted_for_the_timeout_evidence() {
        let state = drain(concat!(
            "ACOSPROBE {\"seq\":5,\"tree\":{\"nodes\":[]}}\n",
            "ACOSLAUNCH MARK probe-stalled pending_ms=1234 wake=received\n",
            "ACOSPROBE {\"seq\":4,\"tree\":{\"nodes\":[]}}\n",
            "ACOSPROBE {\"seq\":6,\"tree\":{\"nodes\":[]}}\n",
        ));
        assert_eq!(state.latest.as_ref().map(|s| s.seq), Some(6));
        assert_eq!(state.stall_pending_ms, Some(1234));
        // Only seq 4 was refused; 6 advanced the sample.
        assert_eq!(state.stale_samples, 1);

        let health = probe_health_json(&Arc::new(std::sync::Mutex::new(state)));
        assert_eq!(health["stale_samples"], serde_json::json!(1));
        assert_eq!(health["guest_stall_pending_ms"], serde_json::json!(1234));
        assert!(health["newest_sample_age_ms"].as_u64().is_some());
        assert_eq!(health["wake_failures"], serde_json::json!(0));
    }

    /// SETTLE5: a wake the channel refuses is counted, never silently dropped.
    /// Before this, `request_guest_probe` discarded the write error, so a dead
    /// reader degraded the session to the 5 s fallback with no evidence at all
    /// (every strict settle then timed out as a bare `probe-floor`).
    #[test]
    fn a_refused_probe_wake_is_counted_and_a_written_one_is_counted_separately() {
        let dir = std::env::temp_dir().join(format!("acos-settle5-wake-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("scratch dir");
        let path = dir.join("wake-probe");
        std::fs::write(&path, b"").expect("scratch file");

        // A read-only handle is the shape of a channel that cannot accept a
        // wake (a dead reader): the write fails and must be counted.
        let read_only = std::fs::File::open(&path).expect("open read-only");
        let mut dead = Some(GuestProbeChannel::fifo(read_only));
        let state = Arc::new(std::sync::Mutex::new(ProbeState::default()));
        assert!(!request_guest_probe(&mut dead, &state));
        {
            let state = state.lock().unwrap();
            assert_eq!(state.wake_failures, 1, "a refused wake is counted");
            assert_eq!(state.wakes_sent, 0);
        }

        // A writable handle is the real channel: the wake is delivered and the
        // sent counter moves.
        let writable = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .expect("open writable");
        let mut live = Some(GuestProbeChannel::fifo(writable));
        assert!(request_guest_probe(&mut live, &state));
        {
            let state = state.lock().unwrap();
            assert_eq!(state.wakes_sent, 1);
            assert_eq!(state.wake_failures, 1);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// SETTLE5: when the demand channel's control thread ends, no wake can ever
    /// arrive again. The guest must then poll at a strict-settle useful cadence
    /// instead of the 5 s idle fallback — otherwise every later settle times out
    /// on a healthy, idle screen. The sample is still a real probe sample under
    /// the same settle rule, so this can only remove false timeouts, never
    /// produce a pass.
    #[test]
    fn a_dead_demand_channel_switches_the_guest_fallback_to_a_fast_poll() {
        let src = include_str!("../guest/java/dev/acos/art/LaunchMain.java");
        assert!(
            src.contains("sProbeControlDead = true;"),
            "the control thread must record that the demand channel ended"
        );
        assert!(
            src.matches("Math.min(fallbackMs, deadChannelMs)").count() >= 2,
            "both the emit decision and the wait must use the fast dead-channel fallback"
        );
        assert!(
            src.contains("probe-control-exit reason="),
            "an ended control thread must be named"
        );
        assert!(
            src.contains("probe-stalled"),
            "a starved probe must be named with its pending age"
        );
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
        let dir =
            std::env::temp_dir().join(format!("acos-b3-no-shared-image-{}", std::process::id()));
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
mod crash_evidence_tests {
    use super::*;

    /// R4: the guest crash line's own JSON is what names the real failing call;
    /// the host must not flatten it to the outer wrapper again.
    #[test]
    fn the_guest_crash_line_is_kept_and_parsed() {
        let line = r#"ACOSLAUNCH {"outcome":"crash","code":"LOOPER_THREW","message":"RuntimeException: Unable to create application","exception":"java.lang.RuntimeException","stack":["android.app.Instrumentation.callApplicationOnCreate(Instrumentation.java:1287)"],"causes":[{"class":"org.koin.core.error.InstanceCreationException","message":"Could not create instance","stack":["x.y.Z.<init>(Z.java:1)"]}]}"#;
        // The line itself is preserved (the K1 reader still keys on it) ...
        let detail = guest_crash_detail(Some(line)).expect("the crash line parses");
        assert_eq!(detail["code"], "LOOPER_THREW");
        assert_eq!(
            detail["causes"][0]["class"],
            "org.koin.core.error.InstanceCreationException"
        );
        assert_eq!(detail["causes"][0]["stack"][0], "x.y.Z.<init>(Z.java:1)");
        // ... and a line without the new fields, or no line at all, is not an error.
        assert_eq!(
            guest_crash_detail(Some(
                r#"ACOSLAUNCH {"outcome":"crash","code":"NO_APPLICATION"}"#
            ))
            .expect("old shape")["code"],
            "NO_APPLICATION"
        );
        assert!(guest_crash_detail(None).is_none());
        assert!(guest_crash_detail(Some("not a crash line")).is_none());
    }

    /// R4: a guest that died without a crash line still leaves a bounded trail.
    #[test]
    fn the_guest_stream_tail_is_bounded_and_ordered() {
        let text = "a\n\n b\nc\nd\n";
        assert_eq!(stdout_tail(text, 2), vec!["c".to_string(), "d".to_string()]);
        assert!(stdout_tail("", 4).is_empty());
    }
}

/// VIEW1c M1/L6: the live-view directory is host-only and private.
#[cfg(test)]
mod view_socket_dir_tests {
    use super::*;
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    #[test]
    fn the_view_directory_is_not_the_guest_mounted_socket_directory() {
        let guest = ShortSocketDir::new().expect("guest socket dir");
        let view = ShortSocketDir::new_named("view").expect("view dir");
        // Separate directories: the guest mounts `guest.0` at /dev/socket, so a
        // `view.sock` there would be reachable by the app under test.
        assert_ne!(guest.0, view.0);
        assert!(
            !view.0.starts_with(&guest.0),
            "view dir is inside the guest dir"
        );
        assert!(
            !guest.0.starts_with(&view.0),
            "guest dir is inside the view dir"
        );
        assert!(view.0.to_string_lossy().contains("acos-view-"));
        assert!(guest.0.to_string_lossy().contains("acos-art-"));
        for dir in [&guest.0, &view.0] {
            let metadata = fs::symlink_metadata(dir).expect("stat");
            assert!(metadata.is_dir());
            assert_eq!(metadata.uid(), unsafe { libc::getuid() });
            assert_eq!(metadata.permissions().mode() & 0o777, 0o700);
        }
        // The one bind mount of the guest socket dir is the only place a path is
        // exposed to the guest: nothing in the launcher passes the view dir to
        // the sandbox (`sb.rw_bind(&sockets.0, "/dev/socket")` is the only socket
        // dir mount).
        let source = include_str!("launch.rs");
        let source = source
            .split("mod view_socket_dir_tests")
            .next()
            .expect("source");
        assert_eq!(
            source.matches("rw_bind(&sockets.0").count(),
            1,
            "exactly one socket directory is bind-mounted into the guest"
        );
        assert!(
            !source.contains("rw_bind(&view_dir") && !source.contains("ro_bind(&view_dir"),
            "the view directory is never mounted"
        );
    }

    #[test]
    fn private_dir_tightens_ours_and_refuses_a_file_or_a_symlink() {
        use std::os::unix::fs::symlink;
        let base = ShortSocketDir::new_named("view").expect("scratch");
        let loose = base.0.join("loose");
        fs::create_dir_all(&loose).expect("mkdir");
        fs::set_permissions(&loose, fs::Permissions::from_mode(0o777)).expect("chmod");
        private_dir(&loose).expect("an owned loose directory is tightened");
        assert_eq!(
            fs::symlink_metadata(&loose)
                .expect("stat")
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        let file = base.0.join("file");
        fs::write(&file, b"x").expect("file");
        assert!(private_dir(&file).is_err(), "a file is refused");
        let link = base.0.join("link");
        symlink(&loose, &link).expect("symlink");
        assert!(
            private_dir(&link).is_err(),
            "a symlink is refused, not followed"
        );
    }

    #[test]
    fn select_candidate_skips_overlong_paths_before_creating_them() {
        use std::os::unix::fs::DirBuilderExt;
        let mut candidates = ShortSocketDir::candidate_dirs("view");
        candidates.retain(|dir| socket_dir_fits_sun_path(&dir.join("ok")));
        let base = ShortSocketDir::select_candidate(candidates, true)
            .or_else(|_| -> std::io::Result<ShortSocketDir> {
                let mut last = None;
                let base_nonce = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos() as u64)
                    .unwrap_or(0);
                for attempt in 0u64..16 {
                    let dir = PathBuf::from(format!(
                        "/tmp/acos-st-{}-{:08x}",
                        std::process::id(),
                        (base_nonce ^ attempt) as u32
                    ));
                    if !socket_dir_fits_sun_path(&dir.join("ok")) {
                        continue;
                    }
                    match fs::DirBuilder::new().mode(0o700).create(&dir) {
                        Ok(()) => match private_dir(&dir) {
                            Ok(()) => return Ok(ShortSocketDir(dir, true)),
                            Err(e) => {
                                let _ = fs::remove_dir(&dir);
                                return Err(e);
                            }
                        },
                        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => last = Some(e),
                        Err(e) => return Err(e),
                    }
                }
                Err(last.unwrap_or_else(|| {
                    std::io::Error::new(
                        std::io::ErrorKind::AlreadyExists,
                        "failed to allocate unique scratch root under /tmp",
                    )
                }))
            })
            .expect("short scratch root");
        let too_long = base.0.join("x".repeat(95));
        let fallback = base.0.join("ok");
        assert!(!socket_dir_fits_sun_path(&too_long));
        assert!(socket_dir_fits_sun_path(&fallback));
        let selected = ShortSocketDir::select_candidate(
            vec![too_long.clone(), fallback.clone()],
            true,
        )
        .expect("fallback should be selected");
        assert_eq!(selected.0, fallback);
        assert!(
            !too_long.exists(),
            "overlong candidate must be skipped before private_dir creates it"
        );
        // If all candidates are overlong, selection fails cleanly.
        assert!(ShortSocketDir::select_candidate(vec![too_long], true).is_err());
    }
}

#[cfg(test)]
#[path = "png_checksum_tests.rs"]
mod png_checksum_tests;
#[cfg(test)]
#[path = "shotz_png_tests.rs"]
mod shotz_png_tests;

#[cfg(test)]
#[path = "md5_frame_tests.rs"]
mod md5_frame_tests;

#[cfg(test)]
#[path = "probe_channel_tests.rs"]
mod probe_channel_tests;

/// S3PERF (E2.17): the skipped-draw evidence is never a readiness gate.
///
/// The guest publishes readiness exactly as main does; a skipped first
/// traversal is the normal pre-visibility traversal. Only a skip that never
/// produces a live C1 frame is a failure, and it is bounded (not the 60 s
/// no-frame budget) and named (`POST_RESUME_NOT_READY` + `draw_skip=`).
#[cfg(test)]
mod skipped_traversal_tests {
    use super::*;
    use std::time::Duration;

    /// The failure fires at the bounded budget, not before, and never for a
    /// launch that produced a live frame.
    #[test]
    fn a_skipped_traversal_with_no_frame_fails_within_the_bounded_budget() {
        let mut gate = SkippedTraversalFrameBudget::new(SKIPPED_TRAVERSAL_NO_FRAME_BUDGET);
        let t0 = Instant::now();
        let skip = Some("traversal:view_not_visible");
        assert!(
            !gate.expired(t0, skip, 0),
            "the first observation starts the clock, it does not fail"
        );
        assert!(
            !gate.expired(t0 + Duration::from_millis(1999), skip, 0),
            "1999 ms is still inside the 2 s budget"
        );
        assert!(
            gate.expired(t0 + SKIPPED_TRAVERSAL_NO_FRAME_BUDGET, skip, 0),
            "at the budget the skipped draw fails"
        );
    }

    /// Normal launch (the skip is the F -> F+1 path): a live frame makes it
    /// ready at any time, and a marker with no skip evidence never takes the
    /// bound at all.
    #[test]
    fn a_live_frame_leaves_the_normal_skipped_traversal_untouched() {
        let mut gate = SkippedTraversalFrameBudget::new(SKIPPED_TRAVERSAL_NO_FRAME_BUDGET);
        let t0 = Instant::now();
        let skip = Some("traversal:view_not_visible");
        assert!(!gate.expired(t0, skip, 1), "generation 1 is a live frame");
        assert!(
            !gate.expired(t0 + Duration::from_secs(10), skip, 1),
            "a live frame can never fail the skipped-draw bound"
        );
        // A marker that drew (or an older guest with no evidence) keeps the
        // unchanged 60 s no-frame budget.
        assert!(!gate.expired(t0, None, 0));
        assert!(!gate.expired(t0 + Duration::from_secs(10), None, 0));
    }

    /// The bound is at least two 60 Hz frames and never longer than the guest's
    /// own post-resume readiness deadline (`sResumeReadyMs` = 2 s).
    #[test]
    fn the_budget_is_two_frames_and_at_most_the_guest_resume_deadline() {
        assert!(SKIPPED_TRAVERSAL_NO_FRAME_BUDGET >= Duration::from_millis(33));
        assert!(SKIPPED_TRAVERSAL_NO_FRAME_BUDGET <= Duration::from_millis(2000));
    }

    /// The marker fields the loop reads are exactly the ones the guest writes.
    #[test]
    fn the_marker_evidence_fields_are_parsed() {
        let marker = "package=dev.acos.fixture.counter\n\
                      first_traversal=traversed=true app_visible=false skip=traversal:view_not_visible\n\
                      draw_skip=traversal:view_not_visible\n\
                      timeout_ms=2000\n\
                      resume_ready=first-traversal\n";
        assert_eq!(
            marker_field(marker, "draw_skip"),
            Some("traversal:view_not_visible")
        );
        assert_eq!(
            resumed_marker_draw_skip(marker).as_deref(),
            Some("traversal:view_not_visible")
        );
        assert_eq!(
            marker_field(marker, "first_traversal"),
            Some("traversed=true app_visible=false skip=traversal:view_not_visible")
        );
        // A traversal that drew, and a marker without the field, are both "no skip".
        assert_eq!(
            resumed_marker_draw_skip("draw_skip=none\nfirst_traversal=traversed=true\n"),
            None
        );
        assert_eq!(resumed_marker_draw_skip("package=x\n"), None);
    }

    /// The failure reply carries the code the S3a contract names plus the
    /// guest's own evidence, and never claims success.
    #[test]
    fn the_failure_reply_names_post_resume_not_ready_and_draw_skip() {
        let reply = skipped_traversal_no_frame_reply(
            "traversal:view_not_visible",
            "traversed=true app_visible=false skip=traversal:view_not_visible",
            0,
            "c1_layers=1 c1_transactions=2",
        );
        assert_eq!(reply["ok"], serde_json::json!(false));
        assert_eq!(reply["code"], serde_json::json!("POST_RESUME_NOT_READY"));
        assert_eq!(
            reply["draw_skip"],
            serde_json::json!("traversal:view_not_visible")
        );
        assert_eq!(reply["retryable"], serde_json::json!(false));
        assert!(reply["first_traversal"]
            .as_str()
            .unwrap_or_default()
            .contains("app_visible=false"));
        let error = reply["error"].as_str().unwrap_or_default();
        assert!(
            error.contains("draw_skip=traversal:view_not_visible"),
            "{error}"
        );
        assert!(error.contains("c1_generation=0"), "{error}");
        assert!(error.contains("c1_layers=1"), "{error}");
    }

    /// ADR-0029 item 1: the guest's process initializers come from the attached
    /// modules' manifests, never from a per-module branch.
    ///
    /// Before this, the launcher keyed `ACOSLAUNCH_TELEPHONY` /
    /// `ACOSLAUNCH_BLUETOOTH` off two module ids and the guest had one
    /// hand-written installer per id, so a new module could not declare what its
    /// process needs and two of them could not share a manager. The guard keeps a
    /// later edit from reintroducing the switch.
    #[test]
    fn the_process_initializer_install_is_driven_by_the_manifest() {
        let guest = include_str!("../guest/java/dev/acos/art/LaunchMain.java");
        for forbidden in [
            "ACOSLAUNCH_TELEPHONY",
            "ACOSLAUNCH_BLUETOOTH",
            "TelephonyFrameworkInitializer\"",
            "BluetoothFrameworkInitializer\"",
        ] {
            assert!(
                !guest.contains(forbidden),
                "the guest must not name a module's initializer (`{forbidden}`); it reads \
                 ACOSLAUNCH_INITIALIZERS (ModuleManifest::process_initializers)"
            );
        }
        assert!(
            guest.contains("ACOSLAUNCH_INITIALIZERS"),
            "the guest must read the launcher's initializer list"
        );
        assert!(
            guest.contains("process-initializer installed"),
            "the guest must log what it installed, so a verdict can cite it"
        );
        // GATENBOOT: AOSP installs the mainline managers in every app process
        // from `ActivityThread.main` -> `initializeMainlineModules()`; the guest
        // builds the ActivityThread itself and never runs `main`, so it must
        // call the same framework method itself, once, before any app code.
        // Telephony/Bluetooth are covered by that call, which is why the
        // manifests no longer declare them (a second call throws).
        assert!(
            guest.contains("initializeMainlineModules"),
            "the guest must run AOSP's per-process mainline bootstrap; without it every \
             mainline ServiceManager holder is null in every guest process"
        );
        assert!(
            guest.contains("mainline-modules initialized"),
            "the guest must log that it ran the mainline bootstrap, so a verdict can cite it"
        );

        let launcher = include_str!("launch.rs");
        assert!(
            launcher.contains("ACOSLAUNCH_INITIALIZERS")
                && launcher.contains("session_process_initializers"),
            "the launcher must forward the attached manifests' initializers"
        );
        // The launcher half is checked by what it forwards, not by what it must
        // not contain: this test lives in the same file it would grep, so its
        // own forbidden-word list would match itself.
        assert!(
            !launcher.contains("attached(\"net.telephony\")"),
            "the launcher must not key an initializer off a module id"
        );
    }

    /// The guest's ready path must stay main's: no forced traversal, no
    /// visibility replay, and the evidence the launcher reads must be present.
    /// A source guard is the cheapest way to keep a later edit from silently
    /// reintroducing the measured one-frame regression (E2.16).
    #[test]
    fn the_guest_ready_path_has_no_forced_traversal_and_publishes_the_evidence() {
        let src = include_str!("../guest/java/dev/acos/art/LaunchMain.java");
        // App-visibility replay is wrong on *every* guest path.
        for forbidden in [
            "redeliverAppVisibility",
            "requesting one redraw",
            ".dispatchAppVisibility(",
            "\"dispatchAppVisibility\"",
        ] {
            assert!(
                !src.contains(forbidden),
                "the guest must not contain `{forbidden}` (S3PERF E2.16)"
            );
        }
        // Forced traversals are the measured one-frame regression *on the ready
        // path*, so the guard is scoped to that path: ROT4's rotation path must
        // force a relayout (`mForceNextWindowRelayout` + `requestLayout()`) to
        // pick up the rotated frame, and that is a different path from the
        // launch/ready one this test protects.
        let ready_start = src
            .find("static boolean awaitFirstTraversal")
            .expect("the ready path function must exist");
        let ready_end = src[ready_start + 1..]
            .find("\n    static ")
            .map(|offset| ready_start + 1 + offset)
            .expect("the ready path function must end before the next static method");
        let ready = &src[ready_start..ready_end];
        for forbidden in ["requestLayout()", "invalidate()"] {
            assert!(
                !ready.contains(forbidden),
                "the guest ready path must not contain `{forbidden}` (S3PERF E2.16)"
            );
        }
        for required in [
            "first-traversal-skipped",
            "draw_skip=",
            "first_traversal=",
            "sFirstTraversalEvidence",
        ] {
            assert!(
                src.contains(required),
                "the guest must publish `{required}` as evidence"
            );
        }
    }

}

/// ABIGATE: the pre-guest ABI refusal is structured, carries its inventory, and
/// can never be read as a pass.
///
/// This is the whole point of the gate: the pre-fix launcher logged
/// `UNSUPPORTED_ABI` and started the guest anyway, so the caller saw a
/// `GUEST_EXITED` app failure (or, on the oldest path, an `INTERNAL`) instead
/// of the documented `UNSUPPORTED_ABI`. `oc` (onCreate) and `resumed` are
/// false because nothing ran, and the `abi` block is the caller's evidence.
#[cfg(test)]
mod abi_gate_tests {
    use super::*;

    fn opts(apk: &str) -> LaunchOpts {
        LaunchOpts {
            apk: PathBuf::from(apk),
            ..LaunchOpts::default()
        }
    }

    #[test]
    fn the_refusal_names_unsupported_abi_and_the_apk_inventory() {
        let result = abi_gate_result(
            &opts("/tmp/arm64-only.apk"),
            "dev.acos.arm64",
            &["arm64-v8a".to_string(), "armeabi-v7a".to_string()],
            &[
                "lib/arm64-v8a/libfoo.so".to_string(),
                "lib/armeabi-v7a/libfoo.so".to_string(),
            ],
        );
        assert_eq!(result.outcome, Outcome::Unsupported);
        assert_ne!(result.outcome, Outcome::Ok, "a gate is never a pass");
        assert_eq!(result.code.as_deref(), Some("UNSUPPORTED_ABI"));
        assert!(!result.application_created && !result.activity_launched);
        assert!(result.guest_exit.is_none() && result.guest_signal.is_none());
        let abi = result.abi.as_ref().expect("abi inventory");
        assert_eq!(
            abi["apk_abis"],
            serde_json::json!(["arm64-v8a", "armeabi-v7a"])
        );
        assert_eq!(abi["host_abi"], serde_json::json!("x86_64"));
        assert_eq!(
            abi["libs_sample"],
            serde_json::json!(["lib/arm64-v8a/libfoo.so", "lib/armeabi-v7a/libfoo.so"])
        );
        assert!(result
            .notes
            .iter()
            .any(|n| n.starts_with("UNSUPPORTED_ABI:")
                && n.contains("INSTALL_FAILED_NO_MATCHING_ABIS")));
        // The serialized result carries the same block (a caller that only
        // keeps the JSON still has the evidence).
        let json = serde_json::to_value(&result).expect("serialize");
        assert_eq!(json["code"], serde_json::json!("UNSUPPORTED_ABI"));
        assert_eq!(json["abi"]["host_abi"], serde_json::json!("x86_64"));
    }

    /// A launch that reached the guest never carries the gate's block, so its
    /// presence is unambiguous evidence of a pre-guest refusal.
    #[test]
    fn a_guest_launch_record_has_no_abi_block() {
        let result = disk_full_result(&opts("/tmp/app.apk"), "dev.acos.app", 1, 2);
        assert!(result.abi.is_none());
        assert_eq!(result.code.as_deref(), Some("HOST_DISK_FULL"));
    }
}

/// REL3H: unhandled input is a settled observation, not a timeout.
///
/// The rule is only as strong as the framework acks behind it, so these pin the
/// two halves: *when* the guest may take the shortcut (every published event
/// delivered and acknowledged unhandled) and *what* it reports when it does
/// (`settled: true`, `reason: unhandled_input`, `pixels_changed: false`, under
/// the strict rule).
#[cfg(test)]
mod rel3h_unhandled_input_tests {
    use super::*;
    use acos_services::window_session::input_dispatch::InjectedAck;

    fn ack(seq: u32, handled: bool) -> InjectedAck {
        InjectedAck {
            seq,
            handled,
            latency_us: Some(1_200),
        }
    }

    /// A tap publishes DOWN+UP; both acks must be present and both unhandled.
    #[test]
    fn every_published_event_must_be_acked_unhandled() {
        assert!(every_event_acked_unhandled(
            &[3, 4],
            &[ack(3, false), ack(4, false)]
        ));
        // Any handled event means the app consumed the gesture: normal settle.
        assert!(!every_event_acked_unhandled(
            &[3, 4],
            &[ack(3, false), ack(4, true)]
        ));
        // A short ack list is "unknown", never "not handled" (REL3E): the
        // pre-REL3H settle (and so the timeout) stays.
        assert!(!every_event_acked_unhandled(&[3, 4], &[ack(3, false)]));
        assert!(!every_event_acked_unhandled(&[3, 4], &[]));
        // An empty gesture proves nothing.
        assert!(!every_event_acked_unhandled(&[], &[]));
        // An ack for a sequence this gesture never published cannot stand in for
        // a published one (stale FOCUS/TOUCH_MODE acks are filtered upstream;
        // this is the rule that makes that filtering load-bearing).
        assert!(!every_event_acked_unhandled(
            &[3, 4],
            &[ack(3, false), ack(9, false)]
        ));
    }

    /// REL3H2: the shortcut binds only what the handler provably left behind.
    ///
    /// A sample taken before the gesture's last ack can be the pre-handler state
    /// (the probe is event-driven, so it also samples on the touch feedback's
    /// draw), and binding it reported "nothing happened" for a gesture whose
    /// handler had actually queued a `setText`. It must never bind, however long
    /// the wait runs.
    mod gate {
        use super::super::*;

        /// A sample at or below the ack floor is always `Wait`, never `Bind`.
        #[test]
        fn a_sample_at_or_before_the_last_ack_never_binds() {
            let mut gate = UnhandledSettleGate::new(10, Duration::ZERO);
            assert_eq!(
                gate.observe(10, 4, Instant::now()),
                UnhandledGateOutcome::Wait(UNHANDLED_ACK_FLOOR_REASON),
                "the sample is the one the acks returned with: it may predate the handler"
            );
            let later = Instant::now() + Duration::from_secs(5);
            assert_eq!(
                gate.observe(7, 4, later),
                UnhandledGateOutcome::Wait(UNHANDLED_ACK_FLOOR_REASON),
                "no amount of waiting makes an old sample older than itself"
            );
        }

        /// A sample strictly after the last ack still needs one confirmation
        /// window on an unchanged draw - the same rule as the normal path.
        #[test]
        fn a_sample_after_the_ack_binds_only_after_the_window() {
            let confirm = Duration::from_millis(20);
            let mut gate = UnhandledSettleGate::new(10, confirm);
            let start = Instant::now();
            assert_eq!(
                gate.observe(11, 4, start),
                UnhandledGateOutcome::Wait(UNHANDLED_CONFIRM_REASON),
                "the window starts with this sample"
            );
            assert_eq!(
                gate.observe(11, 4, start + Duration::from_millis(19)),
                UnhandledGateOutcome::Wait(UNHANDLED_CONFIRM_REASON)
            );
            assert_eq!(
                gate.observe(12, 4, start + Duration::from_millis(21)),
                UnhandledGateOutcome::Bind,
                "the same draw stayed newest for the whole window"
            );
        }

        /// The handler's posted side effect: `finishInputEvent(handled=false)`
        /// with a `post()`ed `setText`. The samples before that draw are
        /// pre-change; the new draw restarts the window, so the shortcut never
        /// binds them and the gesture goes the normal path (which captures it).
        #[test]
        fn a_draw_inside_the_window_restarts_it_instead_of_binding() {
            let confirm = Duration::from_millis(20);
            let mut gate = UnhandledSettleGate::new(10, confirm);
            let start = Instant::now();
            assert_eq!(
                gate.observe(11, 4, start),
                UnhandledGateOutcome::Wait(UNHANDLED_CONFIRM_REASON)
            );
            // The posted setText ran and drew: draw 4 -> 5.
            assert_eq!(
                gate.observe(12, 5, start + Duration::from_millis(21)),
                UnhandledGateOutcome::Wait(UNHANDLED_CONFIRM_REASON),
                "a new draw restarts the window; it must not settle on the pre-change sample"
            );
            assert_eq!(
                gate.observe(13, 5, start + Duration::from_millis(30)),
                UnhandledGateOutcome::Wait(UNHANDLED_CONFIRM_REASON)
            );
            // ... and only the unchanged draw after the *new* window binds.
            assert_eq!(
                gate.observe(14, 5, start + Duration::from_millis(41)),
                UnhandledGateOutcome::Bind
            );
        }

        /// A pending framework signal (the guest reported work it still has to
        /// run) resets the window: no sample it took can be the settled one.
        #[test]
        fn a_reset_forgets_the_window() {
            let mut gate = UnhandledSettleGate::new(10, Duration::from_millis(20));
            let start = Instant::now();
            assert_eq!(
                gate.observe(11, 4, start),
                UnhandledGateOutcome::Wait(UNHANDLED_CONFIRM_REASON)
            );
            gate.reset();
            assert_eq!(
                gate.observe(11, 4, start + Duration::from_millis(30)),
                UnhandledGateOutcome::Wait(UNHANDLED_CONFIRM_REASON),
                "the window restarts after pending work was reported"
            );
            assert_eq!(
                gate.observe(11, 4, start + Duration::from_millis(51)),
                UnhandledGateOutcome::Bind
            );
        }
    }

    /// The reply's settle block is the REL3A record shape with this rule's own
    /// bound/reason, and it says the pixels did not change (so `expect_change`
    /// can fail on it instead of reporting "unmeasured").
    #[test]
    fn the_unhandled_input_settle_is_settled_with_its_own_reason() {
        let timing = SettleTiming {
            mode: SettleMode::Strict,
            first_change_ms: None,
            frame_ms: None,
            state_changed: None,
            bound: "unhandled-input",
            rule: Some("event"),
            reason: Some(SETTLE_UNHANDLED_INPUT.to_string()),
            pixels_changed: Some(false),
            task_finished: false,
            window_reshown: None,
        };
        let json = timing.to_json(true, 7);
        assert_eq!(json["mode"], serde_json::json!("strict"));
        assert_eq!(json["settled"], serde_json::json!(true));
        assert_eq!(json["reason"], serde_json::json!("unhandled_input"));
        assert_eq!(json["bound"], serde_json::json!("unhandled-input"));
        assert_eq!(json["strict_rule"], serde_json::json!("event"));
        assert_eq!(json["pixels_changed"], serde_json::json!(false));
        assert_eq!(json["ack_ms"], serde_json::json!(7));
    }
}

/// BACKSETTLE: the `window_reshown` settle rule, on its own.
///
/// Every test states one of the guards the rule must not be able to skip, so a
/// future change that widens it fails here instead of turning a timeout into a
/// false pass:
///
/// * the change is measured against the forest at the gesture's **ack**, so a
///   change that already happened before the action can never bind;
/// * the re-shown window's frame must be a complete stored buffer (no
///   placeholder), and the tree that is bound must be newer than the change and
///   non-empty;
/// * if the app draws after the action within the settle window, that newer
///   frame wins and the re-show never binds;
/// * a window the action *created* becoming top is not a re-show;
/// * `bound: "window-reshown"` carries the token and the frame the host
///   composites, and still reports the measured `pixels_changed`, so an
///   `expect_change: true` claim fails on an unchanged screen.
#[cfg(test)]
mod backsettle_tests {
    use super::*;

    const CONFIRM_MS: u64 = 24;

    /// The forest at the ack: two windows, the upper one (0x20) on top with
    /// frame 7 stored, C1 at generation 5.
    fn baseline_two_windows() -> WindowForest {
        WindowForest {
            order: vec![0x10, 0x20],
            top_frame: Some(7),
            generation: 5,
        }
    }

    fn gate() -> WindowReshownGate {
        WindowReshownGate::new(baseline_two_windows(), Duration::from_millis(CONFIRM_MS))
    }

    /// The upper window went away; the existing one below is the top window now
    /// and its last drawn frame (3) is what the host composites.
    fn after_removal_top_is_below() -> WindowForest {
        WindowForest {
            order: vec![0x10],
            top_frame: Some(3),
            generation: 5,
        }
    }

    #[test]
    fn a_host_window_change_binds_the_existing_top_window_and_its_frame() {
        let mut gate = gate();
        let now = Instant::now();
        assert_eq!(
            gate.observe(&after_removal_top_is_below(), now, 1_000),
            ReshownOutcome::Wait(WINDOW_CONFIRM_REASON),
            "the change must be the newest fact for one confirmation window"
        );
        assert_eq!(
            gate.observe(
                &after_removal_top_is_below(),
                now + Duration::from_millis(CONFIRM_MS),
                1_000 + CONFIRM_MS
            ),
            ReshownOutcome::Bind(0x10, 3),
            "the token and the frame the host composites are the bind"
        );
        // The bound frame/token are reported as settle evidence, not inferred by
        // the reader.
        let timing = SettleTiming {
            mode: SettleMode::Strict,
            first_change_ms: Some(250),
            frame_ms: None,
            state_changed: None,
            bound: "window-reshown",
            rule: Some("event"),
            reason: None,
            pixels_changed: Some(false),
            task_finished: false,
            window_reshown: Some((0x10, 3)),
        };
        let json = timing.to_json(true, 9);
        assert_eq!(json["bound"], serde_json::json!("window-reshown"));
        assert_eq!(json["window_reshown"], serde_json::json!("0x10"));
        assert_eq!(json["window_reshown_frame"], serde_json::json!(3));
        assert_eq!(json["pixels_changed"], serde_json::json!(false));
        assert!(
            json.get("task_finished").is_none(),
            "a re-show is not a finished task"
        );
    }

    /// Guard: the baseline is the forest at the **ack**. A top window that is
    /// already the top window then is not a change of this action, however the
    /// forest changes elsewhere.
    #[test]
    fn a_top_window_that_was_already_top_at_the_ack_never_binds() {
        let mut gate = gate();
        let now = Instant::now();
        // The same top window, a different window below it: the forest differs,
        // but the top window did not change.
        let below_changed = WindowForest {
            order: vec![0x11, 0x20],
            top_frame: Some(7),
            generation: 5,
        };
        for step in 0..40u64 {
            assert_eq!(
                gate.observe(
                    &below_changed,
                    now + Duration::from_millis(step * 10),
                    1_000 + step * 10
                ),
                ReshownOutcome::Wait(WINDOW_SAME_TOP_REASON)
            );
        }
    }

    /// Guard: the re-shown window's frame must be the one the host composites.
    /// A window with no complete stored buffer (never drew, or the layer is
    /// hidden) is a placeholder, never a bind - the ADR-0032 amendment 1 rule.
    #[test]
    fn a_re_shown_window_without_a_complete_frame_never_binds() {
        let mut gate = gate();
        let now = Instant::now();
        let no_frame = WindowForest {
            order: vec![0x10],
            top_frame: None,
            generation: 5,
        };
        for step in 0..40u64 {
            assert_eq!(
                gate.observe(
                    &no_frame,
                    now + Duration::from_millis(step * 10),
                    1_000 + step * 10
                ),
                ReshownOutcome::Wait(WINDOW_NO_FRAME_REASON)
            );
        }
    }

    /// Guard: a window this action created is not a re-show. It was not in the
    /// forest at the ack, and its first draw is what the normal rule waits for.
    #[test]
    fn a_window_created_by_the_action_never_binds_as_reshown() {
        let mut gate = gate();
        let now = Instant::now();
        let new_top = WindowForest {
            order: vec![0x10, 0x99],
            top_frame: Some(1),
            generation: 6,
        };
        for step in 0..40u64 {
            assert_eq!(
                gate.observe(
                    &new_top,
                    now + Duration::from_millis(step * 10),
                    1_000 + step * 10
                ),
                ReshownOutcome::Wait(WINDOW_NEW_TOP_REASON)
            );
        }
    }

    /// Guard: if the app draws afterwards within the settle window, the newer
    /// frame wins. A C1 store after the ack supersedes the re-show for good.
    #[test]
    fn a_draw_after_the_action_supersedes_the_re_shown_frame() {
        let mut gate = gate();
        let now = Instant::now();
        assert_eq!(
            gate.observe(&after_removal_top_is_below(), now, 1_000),
            ReshownOutcome::Wait(WINDOW_CONFIRM_REASON)
        );
        let drawn = WindowForest {
            order: vec![0x10],
            top_frame: Some(9),
            generation: 6,
        };
        assert_eq!(
            gate.observe(
                &drawn,
                now + Duration::from_millis(CONFIRM_MS),
                1_000 + CONFIRM_MS
            ),
            ReshownOutcome::Wait(WINDOW_SUPERSEDED_REASON),
            "the re-shown frame is not the newest state once the app drew"
        );
        // Even after the newer store is no longer the newest one (a later store
        // on the same draw keeps the generation above the baseline), the re-show
        // never comes back.
        for step in 1..10u64 {
            assert_eq!(
                gate.observe(
                    &drawn,
                    now + Duration::from_millis(CONFIRM_MS * 2 + step * 10),
                    2_000 + step * 10
                ),
                ReshownOutcome::Wait(WINDOW_SUPERSEDED_REASON)
            );
        }
    }

    /// Negative: no window change at all keeps today's semantics - the rule
    /// never binds, so Back's `INPUT_UNHANDLED` / `no-draw-after-action`
    /// outcomes are unchanged.
    #[test]
    fn an_unchanged_window_forest_keeps_the_shipped_semantics() {
        let mut gate = gate();
        let now = Instant::now();
        for step in 0..40u64 {
            assert_eq!(
                gate.observe(
                    &baseline_two_windows(),
                    now + Duration::from_millis(step * 10),
                    1_000 + step * 10
                ),
                ReshownOutcome::Wait(WINDOW_UNCHANGED_REASON)
            );
        }
    }

    /// Negative: an empty forest is the `window-gone` shape, never a re-show.
    #[test]
    fn an_empty_forest_is_not_a_re_show() {
        let mut gate = gate();
        let now = Instant::now();
        for step in 0..10u64 {
            assert_eq!(
                gate.observe(
                    &WindowForest::default(),
                    now + Duration::from_millis(step * 10),
                    1_000 + step * 10
                ),
                ReshownOutcome::Wait(WINDOW_GONE_REASON)
            );
        }
    }

    /// A second change restarts the confirmation window: the rule binds the
    /// state that is still there after the window, not the first one it saw.
    #[test]
    fn a_second_window_change_restarts_the_confirmation_window() {
        // Three windows at the ack: 0x10 (bottom), 0x11, 0x20 (top). Both lower
        // windows therefore already existed when the action's acks arrived.
        let mut gate = WindowReshownGate::new(
            WindowForest {
                order: vec![0x10, 0x11, 0x20],
                top_frame: Some(7),
                generation: 5,
            },
            Duration::from_millis(CONFIRM_MS),
        );
        let now = Instant::now();
        assert_eq!(
            gate.observe(&after_removal_top_is_below(), now, 1_000),
            ReshownOutcome::Wait(WINDOW_CONFIRM_REASON)
        );
        // 0x10 went away too; 0x11 (also present at the ack) is top now with a
        // different frame.
        let next = WindowForest {
            order: vec![0x11],
            top_frame: Some(2),
            generation: 5,
        };
        assert_eq!(
            gate.observe(&next, now + Duration::from_millis(10), 1_010),
            ReshownOutcome::Wait(WINDOW_CONFIRM_REASON)
        );
        assert_eq!(
            gate.observe(
                &next,
                now + Duration::from_millis(10 + CONFIRM_MS),
                1_010 + CONFIRM_MS
            ),
            ReshownOutcome::Bind(0x11, 2)
        );
    }

    /// The tree half of the contract: a re-show binds the tree of the now-top
    /// window, so an empty forest or a sample taken before the change cannot
    /// satisfy it.
    #[test]
    fn an_empty_tree_is_not_a_bindable_observation() {
        assert!(tree_is_empty(&serde_json::json!({"nodes": []})));
        assert!(tree_is_empty(&serde_json::json!({})));
        assert!(!tree_is_empty(&serde_json::json!({"nodes": [{"id": "title"}]})));
    }

    /// The observation half, on a real sample: only a **settled** sample taken
    /// after the change, with the now-top window's non-empty tree, converts the
    /// gate's bind into a settle. An unsettled sample is a screen in motion and
    /// must fall through to the shipped rule (which keeps waiting for it).
    #[test]
    fn only_a_settled_post_change_sample_binds_the_re_shown_window() {
        fn probe(settled: bool, at_ms: u64, nodes: serde_json::Value) -> ProbeSample {
            ProbeSample {
                seq: 9,
                tree: serde_json::json!({"nodes": nodes}),
                at_ms,
                generation: 5,
                draw_ms: 0,
                draw_seq: 0,
                settled,
                pending: if settled { Vec::new() } else { vec!["traversal".into()] },
                window_gone: false,
                animations_on: None,
                window_cmd: None,
            }
        }
        let bind = Some(ReshownOutcome::Bind(0x10, 3));
        let node = serde_json::json!([{"id": "title"}]);
        assert_eq!(
            reshown_bind(bind, &probe(true, 1_000, node.clone()), 1_000),
            Some((0x10, 3)),
            "a settled post-change sample with the now-top tree is the bind"
        );
        assert_eq!(
            reshown_bind(bind, &probe(false, 1_000, node.clone()), 1_000),
            None,
            "an unsettled sample is a screen in motion, never a settle"
        );
        assert_eq!(
            reshown_bind(bind, &probe(true, 999, node.clone()), 1_000),
            None,
            "a sample taken before the change cannot describe it"
        );
        assert_eq!(
            reshown_bind(bind, &probe(true, 1_000, serde_json::json!([])), 1_000),
            None,
            "an empty forest is the window-gone shape, not a re-show"
        );
        assert_eq!(reshown_bind(None, &probe(true, 1_000, node), 1_000), None);
    }
}

/// GATENE2E: the final logd dump keeps the sink's absolute ordinals.
#[cfg(test)]
mod final_logd_tests {
    use super::*;

    #[test]
    fn dump_seq_follows_the_sinks_ordinal_even_after_drop_oldest() {
        assert_eq!(logd_dropped(&["logd: 5 entries".to_string()]), 0);
        assert_eq!(
            logd_dropped(&[
                "cgroup: x".to_string(),
                "logd: 3 entries kept of 10 (7 dropped, cap 3 entries)".to_string()
            ]),
            7
        );
        let dir = std::env::temp_dir().join(format!("final-logd-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let logs = vec![
            crate::LogEntry { prio: 6, tag: "a".into(), message: "first kept".into(), ts_ms: 0 },
            crate::LogEntry { prio: 7, tag: "b".into(), message: "last".into(), ts_ms: 0 },
        ];
        write_final_logd(&dir, &logs, 7);
        let text = std::fs::read_to_string(dir.join(FINAL_LOGD_FILE)).unwrap();
        let seqs: Vec<u64> = text
            .lines()
            .map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap()["seq"].as_u64().unwrap())
            .collect();
        assert_eq!(seqs, vec![8, 9]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod panel_touch_tests {
    use super::*;
    use acos_services::input_channel::{
        ACTION_CANCEL, ACTION_DOWN, ACTION_POINTER_DOWN, ACTION_POINTER_INDEX_SHIFT,
        ACTION_POINTER_UP, ACTION_UP, LoopbackConsumer, monotonic_nanos,
    };
    use acos_services::window_session::input_dispatch::InputHost;
    use acos_services::wms::WmsState;
    use acos_wm::display::DEFAULT_INSETS_MODE;
    use acos_wm::layers::TYPE_APPLICATION;

    const ACTIVITY: u64 = 0x6b01;
    const DIALOG: u64 = 0x6b02;

    fn host_with_dialog_over_activity() -> (
        InputHost,
        Arc<std::sync::Mutex<WmsState>>,
        LoopbackConsumer,
        LoopbackConsumer,
        (f32, f32),
        (f32, f32),
    ) {
        let stack = Arc::new(std::sync::Mutex::new(WmsState::new_with_insets(
            DEFAULT_INSETS_MODE,
        )));
        let (dialog_pt, activity_pt) = {
            let mut wms = stack.lock().expect("wms");
            let session = wms.allocate_session();
            let mut activity = acos_wm::codec::LayoutParams::application("t", "activity");
            activity.type_ = TYPE_APPLICATION;
            activity.width = -1;
            activity.height = -1;
            wms.wm
                .add_to_display_as_user(session, acos_parcel::BinderId(ACTIVITY), activity, 0)
                .expect("add activity");

            let mut dialog = acos_wm::codec::LayoutParams::application("t", "dialog");
            dialog.type_ = TYPE_APPLICATION;
            dialog.width = 600;
            dialog.height = 400;
            wms.wm
                .add_to_display_as_user(session, acos_parcel::BinderId(DIALOG), dialog, 0)
                .expect("add dialog");

            let df = wms.wm.windows[&DIALOG].frame;
            let af = wms.wm.windows[&ACTIVITY].frame;
            (
                ((df.left + 20) as f32, (df.top + 20) as f32),
                ((af.left + 40) as f32, (af.bottom - 100) as f32),
            )
        };
        let host = InputHost::disabled().with_window_stack(Arc::clone(&stack));
        {
            let mut dispatch = host.state().lock().expect("input dispatch");
            dispatch
                .register_window(acos_parcel::BinderId(ACTIVITY), "activity")
                .expect("register activity");
            dispatch
                .register_window(acos_parcel::BinderId(DIALOG), "dialog")
                .expect("register dialog");
        }
        let dialog_fd = host
            .state()
            .lock()
            .expect("input dispatch")
            .duplicate_client_fd(acos_parcel::BinderId(DIALOG))
            .expect("dialog fd");
        let activity_fd = host
            .state()
            .lock()
            .expect("input dispatch")
            .duplicate_client_fd(acos_parcel::BinderId(ACTIVITY))
            .expect("activity fd");
        let mut dialog_guest = LoopbackConsumer::new(dialog_fd);
        let mut activity_guest = LoopbackConsumer::new(activity_fd);
        while dialog_guest.recv().expect("drain dialog").is_some() {}
        while activity_guest.recv().expect("drain activity").is_some() {}
        assert_eq!(
            host.touch_target(dialog_pt.0, dialog_pt.1)
                .expect("dialog hit")
                .token,
            acos_parcel::BinderId(DIALOG)
        );
        assert_eq!(
            host.touch_target(activity_pt.0, activity_pt.1)
                .expect("activity hit")
                .token,
            acos_parcel::BinderId(ACTIVITY)
        );
        (host, stack, dialog_guest, activity_guest, dialog_pt, activity_pt)
    }

    #[test]
    fn retried_up_after_ended_gesture_does_not_retarget_other_window_or_fail_off_window() {
        let (host, _stack, mut dialog_guest, mut activity_guest, dialog_pt, activity_pt) =
            host_with_dialog_over_activity();
        let mut state = PanelTouchState::default();
        let t0 = monotonic_nanos();

        assert_eq!(
            state
                .dispatch(
                    &host,
                    ACTION_DOWN,
                    t0,
                    t0,
                    &[(0, dialog_pt.0, dialog_pt.1)],
                    Duration::ZERO,
                )
                .expect("down"),
            PanelTouchOutcome::Dispatched
        );
        assert_eq!(
            state
                .dispatch(
                    &host,
                    ACTION_UP,
                    t0,
                    t0 + 1_000,
                    &[(0, activity_pt.0, activity_pt.1)],
                    Duration::ZERO,
                )
                .expect("first up"),
            PanelTouchOutcome::Dispatched
        );
        // Drain the two expected events (DOWN + UP) from DIALOG.
        assert!(dialog_guest.recv().expect("dialog down").is_some());
        assert!(dialog_guest.recv().expect("dialog up").is_some());
        assert!(dialog_guest.recv().expect("dialog empty").is_none());
        assert!(activity_guest.recv().expect("activity empty").is_none());

        // Hub lost the reply to ACTION_UP and retries it over ACTIVITY coordinates:
        // must succeed idempotently as AlreadyEnded and deliver 0 events to ACTIVITY.
        assert_eq!(
            state
                .dispatch(
                    &host,
                    ACTION_UP,
                    t0,
                    t0 + 2_000,
                    &[(0, activity_pt.0, activity_pt.1)],
                    Duration::ZERO,
                )
                .expect("retried up over activity"),
            PanelTouchOutcome::AlreadyEnded
        );
        // Same if retried at off-window coordinates (must not fail with NoTouchTarget).
        assert_eq!(
            state
                .dispatch(
                    &host,
                    ACTION_CANCEL,
                    t0,
                    t0 + 3_000,
                    &[(0, -500.0, -500.0)],
                    Duration::ZERO,
                )
                .expect("retried cancel off-window"),
            PanelTouchOutcome::AlreadyEnded
        );
        assert!(
            dialog_guest.recv().expect("dialog still empty").is_none(),
            "retried release must not duplicate events on DIALOG"
        );
        assert!(
            activity_guest.recv().expect("activity still empty").is_none(),
            "retried release must never deliver an unpaired UP/CANCEL to ACTIVITY"
        );
    }

    #[test]
    fn retried_pointer_up_after_already_released_is_noop_without_retargeting() {
        let (host, _stack, mut dialog_guest, mut activity_guest, dialog_pt, activity_pt) =
            host_with_dialog_over_activity();
        let mut state = PanelTouchState::default();
        let t0 = monotonic_nanos();

        assert_eq!(
            state
                .dispatch(
                    &host,
                    ACTION_DOWN,
                    t0,
                    t0,
                    &[(0, dialog_pt.0, dialog_pt.1)],
                    Duration::ZERO,
                )
                .expect("down"),
            PanelTouchOutcome::Dispatched
        );
        let ptr_down_1 = ACTION_POINTER_DOWN | (1 << ACTION_POINTER_INDEX_SHIFT);
        let ptr_up_1 = ACTION_POINTER_UP | (1 << ACTION_POINTER_INDEX_SHIFT);
        assert_eq!(
            state
                .dispatch(
                    &host,
                    ptr_down_1,
                    t0,
                    t0 + 1_000,
                    &[(0, dialog_pt.0, dialog_pt.1), (1, activity_pt.0, activity_pt.1)],
                    Duration::ZERO,
                )
                .expect("pointer down"),
            PanelTouchOutcome::Dispatched
        );
        assert_eq!(
            state
                .dispatch(
                    &host,
                    ptr_up_1,
                    t0,
                    t0 + 2_000,
                    &[(0, dialog_pt.0, dialog_pt.1), (1, activity_pt.0, activity_pt.1)],
                    Duration::ZERO,
                )
                .expect("pointer up"),
            PanelTouchOutcome::Dispatched
        );
        // Drain DOWN, POINTER_DOWN, POINTER_UP on DIALOG.
        for _ in 0..3 {
            assert!(dialog_guest.recv().expect("dialog msg").is_some());
        }
        assert!(dialog_guest.recv().expect("dialog empty").is_none());

        // Retried POINTER_UP for wire_id 1 (already removed) must return AlreadyEnded
        // without duplicating an event or disturbing pointer 0's pin on DIALOG.
        assert_eq!(
            state
                .dispatch(
                    &host,
                    ptr_up_1,
                    t0,
                    t0 + 3_000,
                    &[(0, dialog_pt.0, dialog_pt.1), (1, activity_pt.0, activity_pt.1)],
                    Duration::ZERO,
                )
                .expect("retried pointer up"),
            PanelTouchOutcome::AlreadyEnded
        );
        assert!(dialog_guest.recv().expect("no duplicate pointer up").is_none());
        assert!(activity_guest.recv().expect("activity untouched").is_none());

        // Pointer 0's final ACTION_UP still dispatches to pinned DIALOG.
        assert_eq!(
            state
                .dispatch(
                    &host,
                    ACTION_UP,
                    t0,
                    t0 + 4_000,
                    &[(0, activity_pt.0, activity_pt.1)],
                    Duration::ZERO,
                )
                .expect("final up"),
            PanelTouchOutcome::Dispatched
        );
        assert!(dialog_guest.recv().expect("dialog final up").is_some());
        assert!(activity_guest.recv().expect("activity untouched").is_none());
    }

    #[test]
    fn release_after_pinned_target_closes_reports_target_closed_without_retargeting() {
        let (host, _stack, dialog_guest, mut activity_guest, dialog_pt, activity_pt) =
            host_with_dialog_over_activity();
        let mut state = PanelTouchState::default();
        let t0 = monotonic_nanos();

        assert_eq!(
            state
                .dispatch(
                    &host,
                    ACTION_DOWN,
                    t0,
                    t0,
                    &[(0, dialog_pt.0, dialog_pt.1)],
                    Duration::ZERO,
                )
                .expect("down"),
            PanelTouchOutcome::Dispatched
        );
        // Close DIALOG's input channel mid-gesture.
        drop(dialog_guest);
        host.state()
            .lock()
            .expect("input dispatch")
            .remove(acos_parcel::BinderId(DIALOG));

        assert_eq!(
            state
                .dispatch(
                    &host,
                    ACTION_UP,
                    t0,
                    t0 + 1_000,
                    &[(0, activity_pt.0, activity_pt.1)],
                    Duration::ZERO,
                )
                .expect("release on closed dialog"),
            PanelTouchOutcome::TargetClosed
        );
        assert!(
            activity_guest.recv().expect("activity untouched").is_none(),
            "release after pinned target closed must never fall back to ACTIVITY"
        );
    }

    #[test]
    fn post_publish_pointer_down_ack_failure_reports_published_and_allows_compensating_release() {
        let (host, _stack, mut dialog_guest, mut activity_guest, dialog_pt, activity_pt) =
            host_with_dialog_over_activity();
        let mut state = PanelTouchState::default();
        let t0 = monotonic_nanos();

        // Pre-publish failure (off-window ACTION_DOWN) reports published: false.
        let pre_err = state
            .dispatch(
                &host,
                ACTION_DOWN,
                t0,
                t0,
                &[(0, -500.0, -500.0)],
                Duration::ZERO,
            )
            .expect_err("off-window down must fail");
        assert!(!pre_err.published, "pre-publish rejection must have published=false");
        assert!(state.active_pointers.is_empty());

        // Valid ACTION_DOWN on DIALOG.
        assert_eq!(
            state
                .dispatch(
                    &host,
                    ACTION_DOWN,
                    t0,
                    t0 + 1_000,
                    &[(0, dialog_pt.0, dialog_pt.1)],
                    Duration::ZERO,
                )
                .expect("down"),
            PanelTouchOutcome::Dispatched
        );
        assert!(dialog_guest.recv().expect("dialog down").is_some());

        // Inject a malformed reply byte on DIALOG's channel so `collect` fails
        // AFTER ACTION_POINTER_DOWN for pointer 1 is already published to DIALOG.
        let malformed_fd = host
            .state()
            .lock()
            .expect("input dispatch")
            .duplicate_client_fd(acos_parcel::BinderId(DIALOG))
            .expect("dialog raw fd");
        {
            use std::io::Write;
            let mut raw = std::fs::File::from(malformed_fd);
            raw.write_all(&[0xffu8; 4]).expect("inject malformed reply");
        }
        let ptr_down_1 = ACTION_POINTER_DOWN | (1 << ACTION_POINTER_INDEX_SHIFT);
        let ptr_up_1 = ACTION_POINTER_UP | (1 << ACTION_POINTER_INDEX_SHIFT);
        let post_err = state
            .dispatch(
                &host,
                ptr_down_1,
                t0,
                t0 + 2_000,
                &[(0, dialog_pt.0, dialog_pt.1), (1, activity_pt.0, activity_pt.1)],
                Duration::from_millis(50),
            )
            .expect_err("malformed ack must fail after publish");
        assert!(
            post_err.published,
            "post-publish ack failure must report published=true so hub compensates"
        );
        assert!(
            state.active_pointers.contains(&1),
            "published pointer 1 must stay tracked for compensating POINTER_UP"
        );
        // Verify DIALOG actually received ACTION_POINTER_DOWN.
        assert!(dialog_guest.recv().expect("dialog pointer down").is_some());

        // Hub's compensating ACTION_POINTER_UP dispatches to pinned DIALOG and clears pointer 1.
        assert_eq!(
            state
                .dispatch(
                    &host,
                    ptr_up_1,
                    t0,
                    t0 + 3_000,
                    &[(0, dialog_pt.0, dialog_pt.1), (1, activity_pt.0, activity_pt.1)],
                    Duration::ZERO,
                )
                .expect("compensating pointer up"),
            PanelTouchOutcome::Dispatched
        );
        assert!(!state.active_pointers.contains(&1));
        assert!(dialog_guest.recv().expect("dialog pointer up").is_some());
        assert!(activity_guest.recv().expect("activity untouched").is_none());
    }

    #[test]
    fn hit_point_pins_window_under_cursor_even_when_pointer_zero_starts_outside() {
        let (host, _stack, mut dialog_guest, mut activity_guest, dialog_pt, activity_pt) =
            host_with_dialog_over_activity();
        let mut state = PanelTouchState::default();
        let t0 = monotonic_nanos();

        // Pointer 0 starts over ACTIVITY (as in a wide zoom-out pinch whose cursor
        // is inside DIALOG), while `hit_point` is inside DIALOG.
        assert_eq!(
            state
                .dispatch_with_hit_point(
                    &host,
                    ACTION_DOWN,
                    t0,
                    t0,
                    &[(0, activity_pt.0, activity_pt.1)],
                    Some(dialog_pt),
                    Duration::ZERO,
                )
                .expect("down"),
            PanelTouchOutcome::Dispatched
        );
        let ptr_down_1 = ACTION_POINTER_DOWN | (1 << ACTION_POINTER_INDEX_SHIFT);
        assert_eq!(
            state
                .dispatch_with_hit_point(
                    &host,
                    ptr_down_1,
                    t0,
                    t0 + 1_000,
                    &[(0, activity_pt.0, activity_pt.1), (1, dialog_pt.0, dialog_pt.1)],
                    None,
                    Duration::ZERO,
                )
                .expect("pointer down"),
            PanelTouchOutcome::Dispatched
        );
        assert!(dialog_guest.recv().expect("dialog down").is_some());
        assert!(dialog_guest.recv().expect("dialog pointer down").is_some());
        assert!(
            activity_guest.recv().expect("activity untouched").is_none(),
            "gesture with cursor hit_point inside DIALOG must not hit ACTIVITY"
        );
    }

    #[test]
    fn resolve_keystore_env_handles_real_explicit_off_and_inherited() {
        use acos_services::module::{ModuleConfig, ModuleSpec, Profile};
        use acos_services::modules::keystore::MODULE_ID;

        let real = vec![ModuleSpec {
            id: MODULE_ID.to_string(),
            profile: Profile::Real,
            config: ModuleConfig::default(),
        }];
        assert_eq!(resolve_keystore_env(&real, None).as_deref(), Some("on"));
        assert_eq!(
            resolve_keystore_env(&real, Some("off")).as_deref(),
            Some("on")
        );

        let off = vec![ModuleSpec {
            id: MODULE_ID.to_string(),
            profile: Profile::Off,
            config: ModuleConfig::default(),
        }];
        assert_eq!(resolve_keystore_env(&off, Some("on")), None);
        assert_eq!(resolve_keystore_env(&off, None), None);

        // Omitting the module falls back to legacy ACOS_ART_KEYSTORE if non-empty.
        assert_eq!(resolve_keystore_env(&[], None), None);
        assert_eq!(resolve_keystore_env(&[], Some("   ")), None);
        assert_eq!(
            resolve_keystore_env(&[], Some("on")).as_deref(),
            Some("on")
        );

        // Inherited enabling values reconcile into effective `security.keystore` = Real
        // before module attachment/reporting, while preserving explicit `off`.
        for val in ["on", "ON", "1", "software", " Software "] {
            let mut specs = Vec::new();
            reconcile_inherited_keystore_module(&mut specs, Some(val));
            assert_eq!(
                acos_services::modules::keystore::requested_profile(&specs),
                Some(Profile::Real),
                "inherited={val:?} should reconcile to Profile::Real"
            );
        }
        for val in ["", "   ", "off", "0", "false"] {
            let mut specs = Vec::new();
            reconcile_inherited_keystore_module(&mut specs, Some(val));
            assert_eq!(
                acos_services::modules::keystore::requested_profile(&specs),
                None,
                "inherited={val:?} should not enable security.keystore"
            );
        }
        let mut explicit_off = off.clone();
        reconcile_inherited_keystore_module(&mut explicit_off, Some("on"));
        assert_eq!(
            acos_services::modules::keystore::requested_profile(&explicit_off),
            Some(Profile::Off),
            "explicit off override must survive inherited ACOS_ART_KEYSTORE=on"
        );
    }

    #[test]
    fn base64_encode_handles_unicode_and_padding() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"a"), "YQ==");
        assert_eq!(base64_encode(b"ab"), "YWI=");
        assert_eq!(base64_encode(b"abc"), "YWJj");
        assert_eq!(base64_encode("腹立つ".as_bytes()), "6IW556uL44Gk");
        assert_eq!(base64_encode("café 🎉".as_bytes()), "Y2Fmw6kg8J+OiQ==");
    }

    #[test]
    fn parse_commit_text_line_extracts_ok_and_failure_shapes() {
        assert_eq!(parse_commit_text_line("ACOSPROBE {}"), None);
        let ok_line = r#"ACOSCOMMITTEXT {"seq":7,"ok":true,"handled":true,"method":"input_connection","focus_class":"android.widget.EditText"}"#;
        assert_eq!(
            parse_commit_text_line(ok_line),
            Some((
                7,
                CommitTextOutcome {
                    ok: true,
                    handled: true,
                    no_focused_editor: false,
                    commit_attempted: false,
                    focus_class: Some("android.widget.EditText".to_string()),
                    error: None,
                }
            ))
        );
        let missing_line = r#"ACOSCOMMITTEXT {"seq":8,"ok":false,"handled":false,"no_focused_editor":true,"error":"no focused view in active window"}"#;
        assert_eq!(
            parse_commit_text_line(missing_line),
            Some((
                8,
                CommitTextOutcome {
                    ok: false,
                    handled: false,
                    no_focused_editor: true,
                    commit_attempted: false,
                    focus_class: None,
                    error: Some("no focused view in active window".to_string()),
                }
            ))
        );
        let post_entry_throw = r#"ACOSCOMMITTEXT {"seq":9,"ok":false,"handled":false,"no_focused_editor":false,"commit_attempted":true,"focus_class":"android.widget.EditText","error":"RuntimeException: boom"}"#;
        let (_, outcome) = parse_commit_text_line(post_entry_throw).expect("parse post-entry throw");
        assert!(outcome.commit_attempted);
        assert_eq!(
            classify_commit_text_outcome(9, outcome, 42),
            GuestCommitResult::Uncertain {
                code: "INPUT_UNAVAILABLE",
                reason: "RuntimeException: boom".to_string(),
            }
        );

        let pre_entry_throw = r#"ACOSCOMMITTEXT {"seq":10,"ok":false,"handled":false,"no_focused_editor":false,"commit_attempted":false,"error":"windowViews failed"}"#;
        let (_, pre_outcome) =
            parse_commit_text_line(pre_entry_throw).expect("parse pre-entry throw");
        assert!(!pre_outcome.commit_attempted);
        assert_eq!(
            classify_commit_text_outcome(10, pre_outcome, 12),
            GuestCommitResult::Rejected {
                focus_class: None,
                error: "windowViews failed".to_string(),
            }
        );
    }

    #[test]
    fn write_fd_all_until_bounds_stalled_pipe_and_restores_flags() {
        let mut fds = [-1i32; 2];
        assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
        let (rfd, wfd) = (fds[0], fds[1]);
        let before_flags = unsafe { libc::fcntl(wfd, libc::F_GETFL) };
        assert_eq!(before_flags & libc::O_NONBLOCK, 0);

        // Already-expired deadline must fail before calling write(2) even on an empty pipe.
        let expired = write_fd_all_until(
            wfd,
            b"PROBE\n",
            Instant::now() - Duration::from_millis(1),
        )
        .expect_err("expired deadline must time out without writing");
        assert_eq!(expired.0, 0);
        assert_eq!(expired.1.kind(), std::io::ErrorKind::TimedOut);
        assert_eq!(unsafe { libc::fcntl(wfd, libc::F_GETFL) }, before_flags);

        // Normal small write succeeds immediately and leaves blocking flag intact.
        let small = format_commit_text_frame(1, "abc");
        write_fd_all_until(wfd, small.as_bytes(), Instant::now() + Duration::from_secs(1))
            .expect("small write fits in empty pipe");
        let after_small_flags = unsafe { libc::fcntl(wfd, libc::F_GETFL) };
        assert_eq!(after_small_flags, before_flags);

        // Oversized payload into a stalled pipe with no reader must time out near
        // the deadline instead of blocking indefinitely.
        let huge = vec![b'A'; 512 * 1024];
        let started = Instant::now();
        let err = write_fd_all_until(wfd, &huge, started + Duration::from_millis(80))
            .expect_err("stalled oversized write must time out");
        let elapsed = started.elapsed();
        assert!(err.0 > 0, "expected partial write before pipe filled");
        assert_eq!(err.1.kind(), std::io::ErrorKind::TimedOut);
        assert!(
            elapsed < Duration::from_secs(2),
            "bounded write exceeded deadline wildly: {elapsed:?}"
        );
        let after_timeout_flags = unsafe { libc::fcntl(wfd, libc::F_GETFL) };
        assert_eq!(after_timeout_flags, before_flags);

        unsafe {
            libc::close(rfd);
            libc::close(wfd);
        }
    }

    /// Mirrors `LaunchMain.java`'s `COMMIT_TEXT <seq> <b64_len> <b64> END` parser.
    fn parse_guest_commit_frame_line(line: &str) -> Option<(u64, String)> {
        let word = line.trim();
        if !word.to_ascii_uppercase().starts_with("COMMIT_TEXT") {
            return None;
        }
        let parts: Vec<&str> = word.split(' ').collect();
        if parts.len() != 5 || !parts[0].eq_ignore_ascii_case("COMMIT_TEXT") || parts[4] != "END" {
            return None;
        }
        let seq: u64 = parts[1].parse().ok()?;
        let expected_len: usize = parts[2].parse().ok()?;
        let b64 = parts[3];
        if b64.len() != expected_len {
            return None;
        }
        Some((seq, b64.to_string()))
    }

    #[test]
    fn partial_commit_text_timeout_discards_truncated_frame_and_preserves_next_commands() {
        use std::io::BufRead;
        use std::os::unix::io::FromRawFd;

        let mut fds = [-1i32; 2];
        assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
        let reader_file = unsafe { std::fs::File::from_raw_fd(fds[0]) };
        let writer_file = unsafe { std::fs::File::from_raw_fd(fds[1]) };
        let mut channel = GuestProbeChannel::fifo(writer_file);

        // 1. Write an oversized COMMIT_TEXT into a stalled pipe -> partial write + timeout.
        let oversized_text = "x".repeat(512 * 1024);
        let frame1 = format_commit_text_frame(1, &oversized_text);
        let err1 = channel
            .write_line_until(frame1.as_bytes(), Instant::now() + Duration::from_millis(80))
            .expect_err("stalled oversized COMMIT_TEXT must time out");
        assert!(err1.0, "partial write must report payload wrote_any=true");
        assert_eq!(err1.1.kind(), std::io::ErrorKind::TimedOut);
        assert!(channel.needs_resync, "partial write must arm resync marker");

        // 2. While the pipe is still full, a second attempt writes 0 bytes of payload
        //    (safe to classify as Unavailable) and keeps needs_resync armed.
        let frame2 = format_commit_text_frame(2, "never-sent");
        let err2 = channel
            .write_line_until(frame2.as_bytes(), Instant::now() + Duration::from_millis(40))
            .expect_err("still-full pipe must time out");
        assert!(!err2.0, "zero-byte payload failure must report wrote_any=false");
        assert_eq!(err2.1.kind(), std::io::ErrorKind::TimedOut);
        assert!(channel.needs_resync);

        // 3. Resume the reader (simulating a stalled guest control thread waking up).
        let reader_thread = std::thread::spawn(move || {
            let mut reader = std::io::BufReader::new(reader_file);
            let mut probes = 0usize;
            let mut commits: Vec<(u64, String)> = Vec::new();
            let mut discarded_commit_lines = 0usize;
            let mut line = String::new();
            loop {
                line.clear();
                let n = reader.read_line(&mut line).expect("read pipe");
                if n == 0 {
                    break;
                }
                let word = line.trim();
                if word.eq_ignore_ascii_case("PROBE") {
                    probes += 1;
                } else if word.to_ascii_uppercase().starts_with("COMMIT_TEXT") {
                    match parse_guest_commit_frame_line(word) {
                        Some(parsed) => commits.push(parsed),
                        None => discarded_commit_lines += 1,
                    }
                }
            }
            (probes, commits, discarded_commit_lines)
        });

        // 4. Send PROBE, then a real Unicode COMMIT_TEXT, then simulate a worst-case
        //    partial frame missing only '\n' followed by one more COMMIT_TEXT.
        channel
            .write_line_until(b"PROBE\n", Instant::now() + Duration::from_secs(2))
            .expect("PROBE after resync must succeed");
        assert!(!channel.needs_resync);

        let follow_up_text = "日本語 café 🎉";
        let frame3 = format_commit_text_frame(3, follow_up_text);
        channel
            .write_line_until(frame3.as_bytes(), Instant::now() + Duration::from_secs(2))
            .expect("follow-up COMMIT_TEXT must succeed");

        // Boundary case: partial frame stopped right before '\n' (has full "END").
        let almost_complete = format_commit_text_frame(4, "must-not-commit");
        let without_nl = almost_complete.strip_suffix('\n').unwrap();
        write_fd_all_until(
            channel.as_raw_fd(),
            without_nl.as_bytes(),
            Instant::now() + Duration::from_secs(1),
        )
        .expect("write boundary partial frame");
        channel.needs_resync = true;

        let frame5 = format_commit_text_frame(5, "after-boundary");
        channel
            .write_line_until(frame5.as_bytes(), Instant::now() + Duration::from_secs(2))
            .expect("COMMIT_TEXT after boundary partial frame must succeed");
        assert!(!channel.needs_resync);

        drop(channel);
        let (probes, commits, discarded) = reader_thread.join().expect("reader thread");
        assert_eq!(probes, 1, "PROBE must arrive intact after partial frame resync");
        assert_eq!(
            discarded, 2,
            "both truncated COMMIT_TEXT frames (seq 1 and boundary seq 4) must be discarded"
        );
        assert_eq!(
            commits,
            vec![
                (3, base64_encode(follow_up_text.as_bytes())),
                (5, base64_encode(b"after-boundary")),
            ],
            "only intact subsequent COMMIT_TEXT frames may reach the editor"
        );
    }
}

#[cfg(test)]
mod service_control_hot_attach_tests {
    use super::*;

    fn crc32(data: &[u8]) -> u32 {
        let mut crc = 0xFFFF_FFFFu32;
        for &b in data {
            crc ^= u32::from(b);
            for _ in 0..8 {
                let mask = (crc & 1).wrapping_neg();
                crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
            }
        }
        !crc
    }

    fn stored_zip_single_file(name: &str, data: &[u8]) -> Vec<u8> {
        let crc = crc32(data);
        let len = data.len() as u32;
        let name_bytes = name.as_bytes();
        let name_len = name_bytes.len() as u16;
        let mut out = Vec::new();

        // Local file header (30 bytes + filename).
        out.extend_from_slice(&0x0403_4B50u32.to_le_bytes());
        out.extend_from_slice(&20u16.to_le_bytes()); // version needed
        out.extend_from_slice(&0u16.to_le_bytes()); // flags
        out.extend_from_slice(&0u16.to_le_bytes()); // compression = stored
        out.extend_from_slice(&0u16.to_le_bytes()); // mod time
        out.extend_from_slice(&0u16.to_le_bytes()); // mod date
        out.extend_from_slice(&crc.to_le_bytes());
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(&name_len.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes()); // extra len
        out.extend_from_slice(name_bytes);
        out.extend_from_slice(data);

        let cd_offset = out.len() as u32;
        // Central directory header (46 bytes + filename).
        out.extend_from_slice(&0x0201_4B50u32.to_le_bytes());
        out.extend_from_slice(&20u16.to_le_bytes()); // version made by
        out.extend_from_slice(&20u16.to_le_bytes()); // version needed
        out.extend_from_slice(&0u16.to_le_bytes()); // flags
        out.extend_from_slice(&0u16.to_le_bytes()); // compression = stored
        out.extend_from_slice(&0u16.to_le_bytes()); // mod time
        out.extend_from_slice(&0u16.to_le_bytes()); // mod date
        out.extend_from_slice(&crc.to_le_bytes());
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(&name_len.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes()); // extra len
        out.extend_from_slice(&0u16.to_le_bytes()); // comment len
        out.extend_from_slice(&0u16.to_le_bytes()); // disk number
        out.extend_from_slice(&0u16.to_le_bytes()); // internal attrs
        out.extend_from_slice(&0u32.to_le_bytes()); // external attrs
        out.extend_from_slice(&0u32.to_le_bytes()); // local header offset
        out.extend_from_slice(name_bytes);
        let cd_size = (out.len() as u32) - cd_offset;

        // End of central directory (22 bytes).
        out.extend_from_slice(&0x0605_4B50u32.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes()); // disk
        out.extend_from_slice(&0u16.to_le_bytes()); // cd start disk
        out.extend_from_slice(&1u16.to_le_bytes()); // entries on disk
        out.extend_from_slice(&1u16.to_le_bytes()); // total entries
        out.extend_from_slice(&cd_size.to_le_bytes());
        out.extend_from_slice(&cd_offset.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes()); // comment len
        out
    }

    fn minimal_axml_manifest(package: &str) -> Vec<u8> {
        let strings = ["manifest", "package", package];
        let mut pool_data = Vec::new();
        let mut offsets = Vec::with_capacity(strings.len());
        for s in &strings {
            offsets.push(pool_data.len() as u32);
            let bytes = s.as_bytes();
            assert!(bytes.len() < 128);
            pool_data.push(bytes.len() as u8); // utf16 len
            pool_data.push(bytes.len() as u8); // utf8 len
            pool_data.extend_from_slice(bytes);
            pool_data.push(0);
        }
        while pool_data.len() % 4 != 0 {
            pool_data.push(0);
        }

        let pool_header_size = 28u16;
        let strings_start = u32::from(pool_header_size) + (strings.len() as u32) * 4;
        let pool_chunk_size = strings_start + pool_data.len() as u32;
        let mut pool = Vec::new();
        pool.extend_from_slice(&0x0001u16.to_le_bytes()); // RES_STRING_POOL_TYPE
        pool.extend_from_slice(&pool_header_size.to_le_bytes());
        pool.extend_from_slice(&pool_chunk_size.to_le_bytes());
        pool.extend_from_slice(&(strings.len() as u32).to_le_bytes());
        pool.extend_from_slice(&0u32.to_le_bytes()); // styleCount
        pool.extend_from_slice(&0x0100u32.to_le_bytes()); // UTF8_FLAG
        pool.extend_from_slice(&strings_start.to_le_bytes());
        pool.extend_from_slice(&0u32.to_le_bytes()); // stylesStart
        for off in offsets {
            pool.extend_from_slice(&off.to_le_bytes());
        }
        pool.extend_from_slice(&pool_data);

        // Start element <manifest package="...">
        let mut start_el = Vec::new();
        start_el.extend_from_slice(&0x0102u16.to_le_bytes()); // RES_XML_START_ELEMENT_TYPE
        start_el.extend_from_slice(&16u16.to_le_bytes()); // header_size
        start_el.extend_from_slice(&(16u32 + 20 + 20).to_le_bytes()); // chunk_size
        start_el.extend_from_slice(&1u32.to_le_bytes()); // line
        start_el.extend_from_slice(&u32::MAX.to_le_bytes()); // comment
        start_el.extend_from_slice(&u32::MAX.to_le_bytes()); // ns
        start_el.extend_from_slice(&0u32.to_le_bytes()); // name = "manifest"
        start_el.extend_from_slice(&20u16.to_le_bytes()); // attribute_start
        start_el.extend_from_slice(&20u16.to_le_bytes()); // attribute_size
        start_el.extend_from_slice(&1u16.to_le_bytes()); // attribute_count
        start_el.extend_from_slice(&0u16.to_le_bytes()); // id_index
        start_el.extend_from_slice(&0u16.to_le_bytes()); // class_index
        start_el.extend_from_slice(&0u16.to_le_bytes()); // style_index
        // Attr 0: package = <package>
        start_el.extend_from_slice(&u32::MAX.to_le_bytes()); // ns
        start_el.extend_from_slice(&1u32.to_le_bytes()); // name = "package"
        start_el.extend_from_slice(&2u32.to_le_bytes()); // raw_value = package
        start_el.extend_from_slice(&8u16.to_le_bytes()); // value size
        start_el.push(0); // res0
        start_el.push(0x03); // TYPE_STRING
        start_el.extend_from_slice(&2u32.to_le_bytes()); // data = string idx 2

        // End element </manifest>
        let mut end_el = Vec::new();
        end_el.extend_from_slice(&0x0103u16.to_le_bytes()); // RES_XML_END_ELEMENT_TYPE
        end_el.extend_from_slice(&16u16.to_le_bytes()); // header_size
        end_el.extend_from_slice(&24u32.to_le_bytes()); // chunk_size
        end_el.extend_from_slice(&1u32.to_le_bytes()); // line
        end_el.extend_from_slice(&u32::MAX.to_le_bytes()); // comment
        end_el.extend_from_slice(&u32::MAX.to_le_bytes()); // ns
        end_el.extend_from_slice(&0u32.to_le_bytes()); // name = "manifest"

        let total_size = 8u32 + pool.len() as u32 + start_el.len() as u32 + end_el.len() as u32;
        let mut doc = Vec::with_capacity(total_size as usize);
        doc.extend_from_slice(&0x0003u16.to_le_bytes()); // RES_XML_TYPE
        doc.extend_from_slice(&8u16.to_le_bytes());
        doc.extend_from_slice(&total_size.to_le_bytes());
        doc.extend_from_slice(&pool);
        doc.extend_from_slice(&start_el);
        doc.extend_from_slice(&end_el);
        doc
    }

    fn write_synthetic_settings_apk(dir: &Path) -> PathBuf {
        let apk_path = dir.join("settings.apk");
        let manifest = minimal_axml_manifest(acos_services::modules::settings::SETTINGS_PACKAGE);
        let zip_bytes = stored_zip_single_file("AndroidManifest.xml", &manifest);
        std::fs::write(&apk_path, zip_bytes).expect("write synthetic settings.apk");
        apk_path
    }

    #[test]
    fn service_control_unstarted_timeout_is_removed_from_requests() {
        let control = ServiceControl::default();
        let reply = control.submit(serde_json::json!({"op": "list"}), Duration::from_millis(5));
        assert!(reply.is_none(), "timed-out submit must return None");
        assert!(
            control.take().is_none(),
            "unstarted timed-out request must be removed from requests"
        );
    }

    fn scratch_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "acos-art-{}-{}-{name}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        dir
    }

    #[test]
    fn service_control_started_timeout_returns_false_and_does_not_leak_reply() {
        let control = Arc::new(ServiceControl::default());
        let peer_control = Arc::clone(&control);
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (allow_reply_tx, allow_reply_rx) = std::sync::mpsc::channel();

        let peer = std::thread::spawn(move || {
            let (id, _) = loop {
                if let Some(item) = peer_control.take() {
                    break item;
                }
                std::thread::sleep(Duration::from_millis(1));
            };
            started_tx.send(id).unwrap();
            allow_reply_rx.recv().unwrap();
            peer_control.reply(
                id,
                ServiceControlReply {
                    reply: serde_json::json!({"ok": true}),
                    guest_commands: Vec::new(),
                },
            )
        });

        let result = control.submit(serde_json::json!({"op": "list"}), Duration::from_millis(20));
        assert!(result.is_none(), "submit must time out while peer is blocked");
        let _id = started_rx.recv().unwrap();
        allow_reply_tx.send(()).unwrap();
        let accepted = peer.join().unwrap();
        assert!(
            !accepted,
            "late reply after caller timeout must return false"
        );
        assert!(
            control
                .replies
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .is_empty(),
            "late reply must not leak an entry into ServiceControl::replies"
        );
    }

    #[test]
    fn service_control_reply_before_timeout_is_returned() {
        let control = Arc::new(ServiceControl::default());
        let peer_control = Arc::clone(&control);

        let peer = std::thread::spawn(move || {
            let (id, req) = loop {
                if let Some(item) = peer_control.take() {
                    break item;
                }
                std::thread::sleep(Duration::from_millis(1));
            };
            let accepted = peer_control.reply(
                id,
                ServiceControlReply {
                    reply: serde_json::json!({"ok": true, "echo": req}),
                    guest_commands: Vec::new(),
                },
            );
            assert!(accepted, "in-time reply must return true");
        });

        let result = control
            .submit(serde_json::json!({"op": "status"}), Duration::from_secs(2))
            .expect("in-time reply must reach submit()");
        assert_eq!(result.reply.get("ok").and_then(|v| v.as_bool()), Some(true));
        peer.join().unwrap();
        assert!(
            control
                .replies
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .is_empty(),
            "completed submit must not leave an entry in replies"
        );
    }

    fn pms_has_package(host: &mut acos_services::ServiceHost, package: &str) -> bool {
        let obj = host
            .ensure("android.content.pm.IPackageManager")
            .expect("package service registered");
        let pms = obj
            .as_any_mut()
            .and_then(|any| any.downcast_mut::<acos_services::pms::PmsService>())
            .expect("PmsService");
        let state = pms.shared_state();
        let guard = state.lock().unwrap();
        guard.lookup(Some(package)).is_some()
    }

    fn atms_has_package(host: &mut acos_services::ServiceHost, package: &str) -> bool {
        let obj = host
            .ensure("android.app.IActivityTaskManager")
            .expect("atms registered");
        let atms = obj
            .as_any_mut()
            .and_then(|any| any.downcast_mut::<acos_services::atms::AtmsService>())
            .expect("AtmsService");
        let state = atms.shared_state();
        let guard = state.lock().unwrap();
        guard.ams.manifests.package(package).is_some()
    }

    #[test]
    fn timed_out_hot_attach_rolls_back_and_allows_retry() {
        use acos_services::module::SessionModules;
        use acos_services::modules::settings::{MODULE_ID, SETTINGS_PACKAGE};

        let dir = scratch_dir("hot-attach-rollback-retry");
        let apk_path = write_synthetic_settings_apk(&dir);

        let mut session = SessionModules::new();
        session.attach(&[]).expect("attach core");
        let (host, mut modules) = session.into_parts();
        let mut peer = Peer::with_host(host, None);

        let request = serde_json::json!({
            "op": "attach",
            "id": MODULE_ID,
            "profile": "real",
            "_hot_extra_apk": {
                "staged": apk_path.to_string_lossy(),
                "guest_path": "/data/app/settings.apk",
                "guest_lib_dir": "/data/app-lib-extra/com.android.settings/x86_64",
                "package": SETTINGS_PACKAGE,
            },
        });

        // Simulate the peer completing the hot-attach after ServiceControl::submit timed out.
        let control = ServiceControl::default();
        assert!(
            control
                .submit(request.clone(), Duration::from_millis(1))
                .is_none(),
            "submit must time out first"
        );
        // Suppose the peer already took the request right before timeout: execute
        // service_request and attempt to publish the late reply.
        let reply = service_request(&mut peer, &mut modules, &request);
        assert_eq!(reply.get("ok").and_then(|v| v.as_bool()), Some(true));
        assert!(
            modules.attached().iter().any(|(id, _)| *id == MODULE_ID),
            "system.settings must be attached before rollback"
        );
        assert!(
            pms_has_package(peer.host_mut(), SETTINGS_PACKAGE),
            "com.android.settings must be installed in PMS before rollback"
        );
        assert!(
            atms_has_package(peer.host_mut(), SETTINGS_PACKAGE),
            "com.android.settings must be seeded in ATMS before rollback"
        );

        let published = control.reply(
            1,
            ServiceControlReply {
                reply: reply.clone(),
                guest_commands: Vec::new(),
            },
        );
        assert!(!published, "late reply after timeout must return false");

        // Verify non-matching cases are no-ops.
        assert!(!rollback_timed_out_hot_attach(
            &mut peer,
            &mut modules,
            &serde_json::json!({"op": "detach", "id": MODULE_ID}),
            &reply,
        ));
        assert!(!rollback_timed_out_hot_attach(
            &mut peer,
            &mut modules,
            &request,
            &serde_json::json!({"ok": false}),
        ));

        // Roll back the timed-out hot-attach.
        assert!(rollback_timed_out_hot_attach(
            &mut peer,
            &mut modules,
            &request,
            &reply,
        ));
        assert!(
            !modules.attached().iter().any(|(id, _)| *id == MODULE_ID),
            "system.settings must be detached after rollback"
        );
        assert!(
            !pms_has_package(peer.host_mut(), SETTINGS_PACKAGE),
            "com.android.settings must be removed from PMS after rollback"
        );
        assert!(
            !atms_has_package(peer.host_mut(), SETTINGS_PACKAGE),
            "com.android.settings must be removed from ATMS after rollback"
        );

        // Retrying the hot-attach must now succeed cleanly.
        let retry_reply = service_request(&mut peer, &mut modules, &request);
        assert_eq!(
            retry_reply.get("ok").and_then(|v| v.as_bool()),
            Some(true),
            "retried attach must succeed: {retry_reply}"
        );
        assert!(modules.attached().iter().any(|(id, _)| *id == MODULE_ID));
        assert!(pms_has_package(peer.host_mut(), SETTINGS_PACKAGE));
        assert!(atms_has_package(peer.host_mut(), SETTINGS_PACKAGE));

        // Also verify fallback package name when `_hot_extra_apk.package` is omitted.
        let request_no_pkg = serde_json::json!({
            "op": "attach",
            "id": MODULE_ID,
            "profile": "real",
            "_hot_extra_apk": {
                "staged": apk_path.to_string_lossy(),
            },
        });
        assert!(rollback_timed_out_hot_attach(
            &mut peer,
            &mut modules,
            &request_no_pkg,
            &retry_reply,
        ));
        assert!(!modules.attached().iter().any(|(id, _)| *id == MODULE_ID));
        assert!(!pms_has_package(peer.host_mut(), SETTINGS_PACKAGE));
        assert!(!atms_has_package(peer.host_mut(), SETTINGS_PACKAGE));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn synthetic_settings_apk_opens_and_parses() {
        let dir = scratch_dir("synthetic-settings-apk");
        let apk_path = write_synthetic_settings_apk(&dir);
        let pkg = crate::app::apk_package(&apk_path).expect("parse synthetic settings.apk");
        assert_eq!(pkg, acos_services::modules::settings::SETTINGS_PACKAGE);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

