//! # `acos-art` — the ART substrate
//!
//! Productised form of the approach-A feasibility spike
//! (`results/art-feasibility/E1A.md`): real Android 14 ART
//! (`dalvikvm64` from the `com.android.art` APEX, real boot classpath) runs DEX
//! on a plain Linux host with no init, no zygote, no system_server and no
//! emulator, inside an **unprivileged** `bwrap` user+mount namespace that
//! recreates just enough of `/system` and `/apex` for ART to boot.
//!
//! Components:
//!
//! | module | role |
//! |---|---|
//! | [`ArtRoot`] | sysroot layout, module table, `apex-info-list.xml`, `linkerconfig`, sandbox argv + env |
//! | [`launcher`] | builds the `dalvikvm64` invocation, spawns it with rlimits + timeout + process-group kill, classifies the exit into an [`Outcome`] |
//! | [`props`] | writes the property table consumed by the shim's `__system_property_*` interposition (bionic has no `/dev/__properties__` writer here) |
//! | [`logd`] | tiny `logd`-shaped sink on `/dev/socket/logdw`; liblog writes become structured [`LogEntry`]s |
//! | `shim/artshim.c` | freestanding (`-nostdlib`) `LD_PRELOAD` library: liblog → logd socket, `__system_property_get` → [`props`] table |
//!
//! ## Never fake
//!
//! The launcher reports what actually happened. A missing sysroot, a failed
//! `linkerconfig`, a crashed ART or a timeout is a structured error, never a
//! silent fallback to another engine — the Rust VM stays a separate, explicit
//! choice (see `docs/DECISION_ART_RUNTIME.md`).

pub mod app;
// W37: integrator-owned mod line — additive declaration for the shared
// content-addressed build cache (see src/build_cache.rs).
pub mod boot_image;
pub mod build_cache;
pub mod capture;
pub mod cgroup;
pub mod crash;
// GLSPIKE: the opt-in, lazily loaded software GLES provider (`ACOS_GUEST_GLES`).
pub mod gles;

/// The provider contract's error is a layout error here: a missing provider, a
/// manifest that disagrees with the staged bytes or a render node that is not a
/// device are all "the tree is not what the run needs" (`acos-gles-provider`
/// keeps the message; this launcher keeps one error type).
impl From<acos_gles_provider::Error> for Error {
    fn from(e: acos_gles_provider::Error) -> Self {
        match e {
            acos_gles_provider::Error::Layout(m) => Error::Layout(m),
        }
    }
}

/// The process environment is global: every test that mutates it takes this
/// lock, in any module, so one test's `ACOS_GUEST_GLES`/`ACOS_ART_*` cannot
/// leak into another's (they run on parallel threads).
#[cfg(test)]
pub(crate) static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
pub mod host_zygote;
pub mod jvm;
pub mod landlock;
pub mod launch;
pub mod launcher;
pub mod live_view;
pub mod logd;
pub mod props;
pub mod rn_prefs;
pub mod seccomp;
pub mod session;
// WV4: the guest path ⇄ host path mapping of the WebView session socket.
/// SPIKE-NET (ADR-0030 gate): pasta networking, gated on `ACOS_SPIKE_NET=1`.
pub mod spike_net;
pub mod watchdog;
pub mod webview_socket;
pub mod zygote_prod;

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

pub use capture::{TailBuffer, DEFAULT_LOG_CAP, DEFAULT_STREAM_CAP};
pub use launcher::{run, JitMode, LogMode, Outcome, RunOpts, RunResult};
pub use logd::{LogBatch, LogEntry, LogStats, Logd};
pub use props::Props;
pub use watchdog::{MemoryWatchdog, HOST_MEMORY_LIMIT};

/// The SDK system image the sysroot comes from (API 34, x86_64).
pub const DEFAULT_IMAGE_REL: &str = "system-images/android-34/google_apis/x86_64";

/// Host path of the embedded freestanding shim source.
pub const SHIM_SOURCE: &str = include_str!("../shim/artshim.c");

/// The software gralloc interposer's source (`LD_PRELOAD`, the memfd-backed
/// `GraphicBuffer` memory the stock libui/libgui use; G1).
pub const GRALLOC_SHIM_SOURCE: &str = include_str!("../shim/gralloc_shim.c");

/// The `libacoshw.so` this binary must run: the sysroot's prebuilt copy when it
/// was built from the embedded source, else a build of the embedded source into
/// the content-addressed cache.
///
/// # Why the sysroot's copy is verified, not trusted
///
/// `tools/fetch/art-sysroot.sh --shim` writes `shim/libacoshw.so` and stamps
/// `shim/gralloc_shim.hash` with the first 32 hex characters of the source's
/// SHA-256. The `.so` lives **outside** this binary, so when the shim source
/// changes an existing sysroot keeps the old artefact — and a session then
/// silently runs a shim that is missing entry points. Measured
/// (`results/handoff/A11YPACE`): A11YNATIVE2 added
/// `GraphicBufferMapper::getAllocationSize` to the shim, the shared sysroot's
/// copy was older, and an app calling `Bitmap.wrapHardwareBuffer` on the
/// screenshot `HardwareBuffer` took SIGSEGV in
/// `android::GraphicBufferMapper::getAllocationSize` (libui's own copy, whose
/// HAL mapper object is null without a gralloc HAL) instead of getting its
/// bitmap. The `.so` therefore cannot be taken on faith: its stamp is compared
/// with the embedded source's hash, and a stale/missing artefact is rebuilt
/// from the embedded bytes (one compile per source change, then reused).
///
/// Only when the sysroot is stale **and** no Android NDK is available does the
/// launch stop — with a structured error naming the fix, never a crash.
///
/// # Errors
///
/// A stale sysroot shim with no usable NDK toolchain (`Error::Tool`), or a
/// failed compile of the embedded source.
pub fn gralloc_shim(root: &ArtRoot) -> Result<PathBuf> {
    let prebuilt = root.sysroot.join("shim/libacoshw.so");
    let stamp = root.sysroot.join("shim/gralloc_shim.hash");
    let source = GRALLOC_SHIM_SOURCE.as_bytes();
    let want: String = crate::build_cache::sha256_hex(source)
        .chars()
        .take(32)
        .collect();
    let stamped = fs::read_to_string(&stamp)
        .map(|text| text.trim() == want)
        .unwrap_or(false);
    if std::env::var_os("ACOSART_SHIM_TRACE").is_some() {
        eprintln!(
            "acos-art: gralloc shim: sysroot={} stamped={stamped} want={want} prebuilt={}",
            prebuilt.display(),
            prebuilt.is_file()
        );
    }
    if stamped && prebuilt.is_file() {
        return Ok(prebuilt);
    }
    let clang = ndk_clang().ok_or_else(|| {
        Error::Tool(format!(
            "the sysroot's software gralloc shim ({}) was not built from this binary's              crates/acos-art/shim/gralloc_shim.c (stamp {} vs {want}), so it would crash an app              that wraps a HardwareBuffer in a Bitmap; rebuild it with              `tools/fetch/art-sysroot.sh --shim` or point ANDROID_NDK_HOME at an NDK",
            prebuilt.display(),
            stamp.display()
        ))
    })?;
    let key = crate::build_cache::digest(&[
        b"acos-art/gralloc-shim/v1",
        source,
        crate::build_cache::file_id(Path::new(&clang)).as_bytes(),
    ]);
    let built = crate::build_cache::ensure(&key, &["libacoshw.so"], |stage| {
        let src = stage.join("gralloc_shim.c");
        fs::write(&src, source)?;
        let out = stage.join("libacoshw.so");
        let status = Command::new(&clang)
            .args([
                "-O2",
                "-fPIC",
                "-shared",
                "-Wall",
                "-Wno-unused-parameter",
                "-o",
            ])
            .arg(&out)
            .arg(&src)
            .status()
            .map_err(|e| Error::Tool(format!("running {clang}: {e}")))?;
        if !status.success() {
            return Err(Error::Tool(format!(
                "{clang} failed to build {}",
                src.display()
            )));
        }
        Ok(())
    })?;
    let installed = crate::build_cache::install(&built, &["libacoshw.so"], &root.data.join("shim"))?;
    Ok(installed[0].clone())
}

/// The NDK clang that can build the bionic-linked gralloc shim, if one is
/// installed: `ACOS_ART_NDK`, `ANDROID_NDK_HOME`, `ANDROID_NDK_ROOT`, then the
/// SDK's `ndk/*` (the same discovery `tools/fetch/art-sysroot.sh` uses).
fn ndk_clang() -> Option<String> {
    let mut roots: Vec<PathBuf> = Vec::new();
    for key in ["ACOS_ART_NDK", "ANDROID_NDK_HOME", "ANDROID_NDK_ROOT"] {
        if let Some(value) = std::env::var_os(key) {
            if !value.is_empty() {
                roots.push(PathBuf::from(value));
            }
        }
    }
    for key in ["ANDROID_HOME", "ANDROID_SDK_ROOT"] {
        if let Some(value) = std::env::var_os(key) {
            roots.push(PathBuf::from(value).join("ndk"));
        }
    }
    if let Some(home) = std::env::var_os("HOME") {
        roots.push(PathBuf::from(home).join("Android/Sdk/ndk"));
    }
    for root in roots {
        let mut ndks: Vec<PathBuf> = match fs::read_dir(&root) {
            Ok(entries) => entries
                .flatten()
                .map(|entry| entry.path())
                .filter(|path| path.is_dir())
                .collect(),
            Err(_) => Vec::new(),
        };
        // A bare NDK directory (ACOS_ART_NDK=/path/to/ndk) has no nested
        // versions; a bare android-34 API suffix is what the image's libs are.
        ndks.sort();
        if root.join("toolchains").is_dir() {
            ndks.push(root.clone());
        }
        for ndk in ndks {
            let prebuilt = ndk.join("toolchains/llvm/prebuilt");
            let Ok(hosts) = fs::read_dir(&prebuilt) else {
                continue;
            };
            let mut hosts: Vec<PathBuf> = hosts.flatten().map(|e| e.path()).collect();
            hosts.sort();
            for host in hosts {
                let candidate = host.join("bin/x86_64-linux-android34-clang");
                if candidate.is_file() {
                    return Some(candidate.display().to_string());
                }
            }
        }
    }
    None
}

/// Publish `bytes` at `path` without ever truncating a file another process may
/// be reading or **mapping**.
///
/// H17: every `acos-art` launch of a cohort shares one `--data` tree, and the
/// guests read (or `mmap`) its shared inputs (`seccomp/landlock.plan`,
/// `acos.properties`, `apex-info-list.xml`) while other launches rewrite them.
/// `fs::write` is `O_TRUNC` + `write(2)`, so a concurrent reader could observe an
/// empty or half-written file (the guard then exits 126 with
/// `ERR landlock=plan-missing`) and a concurrent `mmap` could take `SIGBUS`.
/// Writing a sibling temp file and `rename(2)`-ing it into place makes the
/// replacement atomic: a reader keeps the old inode (complete) or gets the new
/// one (complete), never a torn file. Identical bytes are left alone, so the
/// common case does no I/O and the inode (and any live mapping) does not churn.
pub fn publish_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    if fs::read(path).map(|old| old == bytes).unwrap_or(false) {
        return Ok(());
    }
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "artifact".to_string());
    // Same directory, so the rename cannot cross a filesystem boundary. The
    // dotted prefix keeps the temp out of the names readers look for.
    let tmp = path.with_file_name(format!(
        ".{name}.h17-{}-{}.tmp",
        std::process::id(),
        SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    fs::write(&tmp, bytes)?;
    if let Err(e) = fs::rename(&tmp, path) {
        let _ = fs::remove_file(&tmp);
        return Err(Error::Io(e));
    }
    Ok(())
}

/// Version marker of the synthesised `apex-info-list.xml` (bumped when the
/// generated content changes, so a stale file is rewritten).
const APEX_INFO_MARKER: &str = "<!-- acos-art apex-info-list v3 -->\n";

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(
        "runtime root not found (set ACOS_HOME for an installed ACOS, or ACOS_ROOT for a checkout)"
    )]
    NoRepoRoot,
    #[error(
        "ART sysroot not found: {0} (run tools/fetch/art-sysroot.sh --out {0}, or set ACOS_ART_SYSROOT)"
    )]
    NoSysroot(PathBuf),
    #[error("sysroot layout error: {0}")]
    Layout(String),
    #[error("tool failed: {0}")]
    Tool(String),
    #[error("MEMORY_CGROUP_UNAVAILABLE: {0}")]
    CgroupUnavailable(String),
    #[error("SECCOMP_UNAVAILABLE: {0}")]
    SeccompUnavailable(String),
    /// The guest Landlock ruleset could not be assembled or proven on a kernel
    /// that supports Landlock (fail closed). Distinct from the seccomp family:
    /// Landlock owns filesystem confinement, the seccomp filter owns syscalls.
    #[error("LANDLOCK_UNAVAILABLE: {0}")]
    LandlockUnavailable(String),
    /// An `ACOS_ART_HEAP*` seam refused its value. The message starts with
    /// `INVALID_HEAP_CONFIG` so a caller can classify it without parsing prose;
    /// there is no silent fallback to the stock heap values (W48).
    #[error("INVALID_HEAP_CONFIG: {0}")]
    InvalidHeapConfig(String),
    /// The production zygote (`ACOS_ART_ZYGOTE=on`) could not serve a verb. The
    /// message starts with `ZYGOTE_DEAD` and carries the keeper's exit status and
    /// log tail: a dead zygote is reported and restarted, never silently replaced
    /// by the exec path (ZPROD P2, design §6).
    #[error("{0}")]
    Zygote(String),
    /// An `ACOS_ART_GUEST_UID` seam refused its value. There is no silent
    /// fallback to the host uid: uid 1000 is Android's `SYSTEM_UID` and makes
    /// the framework take privileged branches that do not exist for an app.
    #[error("INVALID_GUEST_UID: {0}")]
    InvalidGuestUid(String),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, Error>;

/// One APEX of the image.
///
/// `name` is the **host directory** under `<sysroot>/system/apex/` (the
/// extraction names it after the `.apex` payload file, e.g.
/// `com.google.android.configinfrastructure`), while `mount` is the **device
/// mount name** from `apex_manifest.pb`. They differ for the Google-signed
/// modules, and the mount name is what `/apex/<name>` and every classpath
/// fragment use, so the sandbox must mount by `mount`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApexModule {
    pub name: String,
    pub file: String,
    pub mount: String,
}

impl ApexModule {
    /// `/apex/<name>` as the device spells it (shared-libs APEX is aliased).
    pub fn guest(&self) -> String {
        if self.mount == "com.google.mainline.primary.libs" {
            "/apex/sharedlibs".to_string()
        } else {
            format!("/apex/{}", self.mount)
        }
    }
}

/// `BOOTCLASSPATH` and `DEX2OATBOOTCLASSPATH` as `/data/system/environ/classpath`
/// (written by the image's own `derive_classpath`) enumerates them.
///
/// ART's `GetFirstMainlineFrameworkLibraryFilename` (libartbase) requires
/// `DEX2OATBOOTCLASSPATH` to be a strict prefix of `BOOTCLASSPATH`; the
/// remainder names the mainline framework libraries and is what
/// `GetDefaultBootImageLocation` uses to derive the boot-image mainline
/// extension. Setting both from this file makes that CHECK pass on real input.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Classpaths {
    pub boot: Vec<String>,
    pub dex2oat: Vec<String>,
}

impl Classpaths {
    /// Usable means: the device pair (strict prefix + non-empty mainline
    /// remainder) **and** the platform framework is present. A sysroot
    /// extracted without `/system/etc/classpaths/` cannot produce the latter,
    /// and its derivation would silently drop `framework.jar`.
    pub fn is_complete(&self) -> bool {
        let has_framework = self.boot.iter().any(|j| j.ends_with("/framework.jar"));
        let prefix_ok = self.dex2oat.len() < self.boot.len()
            && self.boot[..self.dex2oat.len()] == self.dex2oat[..];
        has_framework && prefix_ok
    }
}

/// The extracted, read-only ART sysroot plus the writable per-session data dir.
#[derive(Debug, Clone)]
pub struct ArtRoot {
    pub repo_root: PathBuf,
    pub sysroot: PathBuf,
    pub data: PathBuf,
    pub modules: Vec<ApexModule>,
}

impl ArtRoot {
    /// Resolve the sysroot/data dirs: explicit args, then `ACOS_ART_SYSROOT` /
    /// `ACOS_ART_DATA`, then the productised location `.cache/art/{sysroot,data}`,
    /// then the feasibility scratch `.cache/art-exp/{sysroot,data}`.
    pub fn resolve(sysroot: Option<PathBuf>, data: Option<PathBuf>) -> Result<Self> {
        let repo_root = repo_root()?;
        let sysroot = match sysroot {
            Some(p) => p,
            None => match std::env::var_os("ACOS_ART_SYSROOT") {
                Some(p) => PathBuf::from(p),
                None => {
                    let mut found = None;
                    for cand in [".cache/art/sysroot", ".cache/art-exp/sysroot"] {
                        let p = repo_root.join(cand);
                        if p.join("system/apex").is_dir() {
                            found = Some(p);
                            break;
                        }
                    }
                    found.ok_or_else(|| Error::NoSysroot(repo_root.join(".cache/art/sysroot")))?
                }
            },
        };
        if !sysroot.join("system/apex").is_dir() {
            return Err(Error::NoSysroot(sysroot));
        }
        let data = match data {
            Some(p) => p,
            None => match std::env::var_os("ACOS_ART_DATA") {
                Some(p) => PathBuf::from(p),
                None => repo_root.join(".cache/art/data"),
            },
        };
        let modules = read_modules(&sysroot)?;
        Ok(Self {
            repo_root,
            sysroot,
            data,
            modules,
        })
    }

    // ---- layout -----------------------------------------------------------

    /// Create the writable `/data` skeleton and refresh the derived files
    /// (`apex-info-list.xml`, property table). Idempotent. Also runs the
    /// per-run janitor (see [`ArtRoot::sweep_stale_runs`]).
    pub fn ensure_data(&self) -> Result<()> {
        for d in [
            "dalvik-cache/x86_64",
            "misc/apexdata/com.android.art/dalvik-cache/x86_64",
            "linkerconfig",
            "runs",
            "seccomp",
            "socket",
            "system/environ",
            "tmp",
            "img",
            "shim",
        ] {
            fs::create_dir_all(self.data.join(d))?;
        }
        self.write_apex_info_list()?;
        // W48: the heap seam lives here, so an invalid ACOS_ART_HEAP* value is
        // a structured INVALID_HEAP_CONFIG before any guest exists.
        Props::for_launch()?.write(&self.data.join(props::PROPS_FILE))?;
        // A launcher that was SIGKILLed cannot remove its own run dir; every
        // later invocation reaps the stale ones (S4).
        if let Ok(n) = self.sweep_stale_runs() {
            if n > 0 {
                eprintln!("acos-art: janitor removed {n} stale run dir(s)");
            }
        }
        crate::stage("ensure-data");
        Ok(())
    }

    // ---- per-run data roots (S4) -----------------------------------------

    /// `<data>/runs`: one directory per APK run/session, removed on exit.
    pub fn runs_dir(&self) -> PathBuf {
        self.data.join("runs")
    }

    /// Create a fresh per-run writable root with the subtrees ART and the guest
    /// legitimately write (`dalvik-cache`, `tmp`, `out`, `task`, `socket`,
    /// `session`, `app-data`). The returned [`RunDir`] removes the tree on drop.
    pub fn new_run_dir(&self, tag: &str) -> Result<RunDir> {
        // Short on purpose: the logd/importer sockets live inside the run root
        // and a `sun_path` is 108 bytes — a long id would make a deep `--data`
        // dir fail at `bind()` ("path must be shorter than SUN_LEN").
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let id = format!("{}-{seq}", std::process::id());
        let path = self.runs_dir().join(&id);
        self.run_dir_at(&path, tag)
    }

    /// Z4 (research seam, off by default): build the same per-run root at an
    /// explicit path and leave it in place on drop. Only `launch.rs`'s
    /// `ACOS_ZYGOTE_SESSION` path calls this; every production caller still
    /// goes through [`ArtRoot::new_run_dir`]. See results/art-runtime/Z4.md.
    pub fn run_dir_at(&self, path: &Path, tag: &str) -> Result<RunDir> {
        let label: String = tag
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '-'
                }
            })
            .collect();
        for d in [
            "dalvik-cache/x86_64",
            "misc/apexdata/com.android.art/dalvik-cache/x86_64",
            "tmp",
            "out",
            "task",
            "socket",
            "session",
            "app-data",
        ] {
            fs::create_dir_all(path.join(d))?;
        }
        // Owner record: pid + starttime, so the janitor can tell a live run
        // from a pid that has been reused since (H1's leak-check lesson).
        let start = fs::read_to_string("/proc/self/stat")
            .ok()
            .and_then(|s| {
                s.rfind(')')
                    .map(|i| s[i + 2..].split_whitespace().nth(19).map(str::to_string))
                    .flatten()
            })
            .unwrap_or_else(|| "0".to_string());
        fs::write(
            path.join("owner"),
            format!(
                "pid={}\nstart={}\ntag={}\n",
                std::process::id(),
                start,
                label
            ),
        )?;
        let keep = std::env::var_os("ACOS_ART_KEEP_RUN").is_some()
            || std::env::var_os("ACOS_ZYGOTE_SESSION").is_some();
        Ok(RunDir {
            path: path.to_path_buf(),
            keep,
        })
    }

    /// Remove run dirs that have no live owner pid and are older than the TTL
    /// (`ACOS_ART_RUN_TTL_SECS`, default 1800 s). This is the SIGKILL fallback:
    /// `--die-with-parent` kills the guest and bwrap, but the host directory can
    /// only be removed by a process that outlives them.
    pub fn sweep_stale_runs(&self) -> Result<usize> {
        let ttl = run_ttl_from_env();
        let root = self.runs_dir();
        let Ok(rd) = fs::read_dir(&root) else {
            return Ok(0);
        };
        let mut removed = 0;
        for e in rd.flatten() {
            let path = e.path();
            if !e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                continue;
            }
            if run_dir_is_live(&path) {
                continue;
            }
            let stamp = path.join("owner");
            let meta = fs::metadata(&stamp).or_else(|_| fs::metadata(&path));
            let age = meta
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.elapsed().ok())
                .unwrap_or_default();
            if age.as_secs() < ttl {
                continue;
            }
            match fs::remove_dir_all(&path) {
                Ok(()) => removed += 1,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => eprintln!("acos-art: janitor could not remove {}: {e}", path.display()),
            }
        }
        let _ = crate::webview_socket::sweep_stale_session_dirs(
            &crate::webview_socket::runtime_root(),
        );
        Ok(removed)
    }

    fn apex_info_list(&self) -> PathBuf {
        self.data.join("apex-info-list.xml")
    }

    /// Synthesise the XML that `apexd` writes at boot. `linkerconfig` refuses
    /// to run without it, and it is also what `libnativeloader` consults for
    /// mainline-framework lookups (see results/art-runtime/F.md §4).
    pub fn write_apex_info_list(&self) -> Result<()> {
        let out = self.apex_info_list();
        let modules = self.modules_path();
        let have_marker = fs::read_to_string(&out)
            .map(|t| t.contains(APEX_INFO_MARKER))
            .unwrap_or(false);
        let stale = !have_marker
            || match (fs::metadata(&out), fs::metadata(&modules)) {
                (Ok(o), Ok(m)) => match (o.modified(), m.modified()) {
                    (Ok(o), Ok(m)) => o < m,
                    _ => true,
                },
                _ => true,
            };
        if !stale {
            return Ok(());
        }
        let mut xml = String::from("<?xml version=\"1.0\" encoding=\"utf-8\"?>\n");
        xml.push_str(APEX_INFO_MARKER);
        xml.push_str("<apex-info-list>\n");
        for m in &self.modules {
            let shared = m.mount == "com.google.mainline.primary.libs";
            xml.push_str(&format!(
                "  <apex-info moduleName=\"{}\" modulePath=\"/system/apex/{}\" \
                 preinstalledModulePath=\"/system/apex/{}\" versionCode=\"340000000\" \
                 versionName=\"1\" isFactory=\"true\" isActive=\"true\" \
                 lastUpdateMillis=\"0\" provideSharedApexLibs=\"{}\" />\n",
                m.mount, m.file, m.file, shared
            ));
        }
        xml.push_str("</apex-info-list>\n");
        // H17: shared inputs are read by every concurrent guest; never truncate
        // one in place (see [`publish_atomic`]).
        publish_atomic(&out, xml.as_bytes())?;
        Ok(())
    }

    fn modules_path(&self) -> PathBuf {
        self.sysroot.join("APEX_MODULES.txt")
    }

    /// Generate `/linkerconfig/ld.config.txt` with the image's own
    /// `linkerconfig` binary (bionic then has the `system` and per-APEX
    /// namespaces `libnativeloader` requires).
    ///
    /// **W39:** the generated tree depends only on the image digest, the APEX
    /// mount table and the `linkerconfig` binary — never on the APK. It is built
    /// once into the W37 content-addressed build cache
    /// (`<cache>/<key>/{ld.config.txt,apex.libraries.config.txt,<apex>/ld.config.txt}`)
    /// and later launches install the verified tree instead of spawning the
    /// bionic binary again (same lock / atomic-publish / verify rules).
    pub fn ensure_linkerconfig(&self, regen: bool) -> Result<()> {
        let ld = self.data.join("linkerconfig/ld.config.txt");
        // linkerconfig enumerates the APEX namespaces from the actual mounts,
        // so a mount-table change (new extraction, corrected apex names)
        // invalidates an existing ld.config.txt.
        let stamp = self.data.join("linkerconfig/.mounts");
        let mounts = self.linkerconfig_stamp();
        let stamp_ok = fs::read_to_string(&stamp)
            .map(|s| s.trim() == mounts)
            .unwrap_or(false);
        if ld.is_file() && stamp_ok && !regen {
            return Ok(());
        }
        self.ensure_data()?;
        let dir = self.data.join("linkerconfig");
        fs::create_dir_all(&dir)?;
        let key = self.linkerconfig_cache_key(&mounts);
        let cached = crate::build_cache::enabled();
        if cached {
            let _lock = crate::build_cache::lock(&key)?;
            if !regen {
                if crate::build_cache::reuse_tree_to(&key, &dir)?.is_some() {
                    fs::write(&stamp, format!("{mounts}\n"))?;
                    crate::stage("linkerconfig-hit");
                    return Ok(());
                }
            }
            // A miss rebuilds the whole derived tree; drop a previous sysroot's
            // files so the published entry is exactly this run's output.
            clear_derived_tree(&dir)?;
        }
        // linkerconfig is a bionic binary: without the shim its CHECK failures
        // are invisible (liblog has nowhere to write) and it just aborts.
        crate::build_shim(&self.data.join("shim"))?;
        // linkerconfig picks its mode from the property table; serve it the
        // table without the Treble-mode keys so it emits the unisolated config
        // the runtime is built for (see `props::LINKERCONFIG_MODE_KEYS`).
        let lc_props = self.data.join(props::LINKERCONFIG_PROPS_FILE);
        Props::for_linkerconfig()?.write(&lc_props)?;
        let mut sb = self.sandbox();
        sb.staging(true); // host build step: /linkerconfig is regenerated here
        sb.with_shim(true);
        sb.ro_bind(&lc_props, "/data/acos.properties");
        let mut cmd = sb.command(
            "/apex/com.android.runtime/bin/linkerconfig",
            &["--target".into(), "/linkerconfig".into()],
        )?;
        let log = self.data.join("linkerconfig.log");
        let out = cmd.output()?;
        let mut text = Vec::new();
        text.extend_from_slice(&out.stdout);
        text.extend_from_slice(&out.stderr);
        fs::write(&log, &text)?;
        if !ld.is_file() {
            return Err(Error::Layout(format!(
                "linkerconfig produced no ld.config.txt (status {:?}, log: {}): {}",
                out.status.code(),
                log.display(),
                text.iter().map(|b| *b as char).collect::<String>().trim()
            )));
        }
        fs::write(&stamp, format!("{mounts}\n"))?;
        if cached {
            crate::build_cache::publish_tree_locked(&key, &dir, &|rel| rel == ".mounts")?;
        }
        crate::stage("linkerconfig-built");
        Ok(())
    }

    /// What the generated `/linkerconfig` tree depends on besides the image:
    /// the APEX mount table and the **mode** the generator runs in (legacy,
    /// unisolated; it follows `ro.vndk.version`, see
    /// `props::LINKERCONFIG_MODE_KEYS`). A tree an older build generated in the
    /// strict mode carries no such token, so it is regenerated, not reused.
    fn linkerconfig_stamp(&self) -> String {
        format!("{}+legacy-v1", self.mount_stamp())
    }

    /// Cache key of the generated `linkerconfig` tree: image digest + APEX mount
    /// table + generator property table + the `linkerconfig` binary **content**
    /// + the invocation. Nothing in it names the sysroot's path: the same image
    /// through another path (a worktree) is the same entry.
    fn linkerconfig_cache_key(&self, stamp: &str) -> String {
        let tool = self.host_path("/apex/com.android.runtime/bin/linkerconfig");
        crate::build_cache::digest(&[
            b"acos-art/linkerconfig/v2",
            crate::build_cache::image_digest(&self.sysroot).as_bytes(),
            stamp.as_bytes(),
            crate::build_cache::content_id(&tool).as_bytes(),
            b"--target /linkerconfig",
        ])
    }

    /// Fingerprint of the APEX mount table (`/apex/<name>` set).
    fn mount_stamp(&self) -> String {
        let mut names: Vec<String> = self.modules.iter().map(ApexModule::guest).collect();
        names.sort();
        format!("{:x}", simple_hash(&names.join("\n")))
    }

    // ---- classpath (the input ART's mainline-framework CHECK reads) --------

    /// `<data>/system/environ/classpath`, the file `derive_classpath` writes on
    /// device and `init` sources into the environment.
    pub fn classpath_env_file(&self) -> PathBuf {
        self.data.join("system/environ/classpath")
    }

    /// The generated classpaths, if `derive_classpath` has produced a usable
    /// device pair (see [`Classpaths::is_complete`]).
    pub fn classpaths(&self) -> Option<Classpaths> {
        let cp = Self::parse_classpath_env(&fs::read_to_string(self.classpath_env_file()).ok()?)?;
        cp.is_complete().then_some(cp)
    }

    /// Parse the `export NAME a:b:c` lines of a device `classpath` environ file.
    pub fn parse_classpath_env(text: &str) -> Option<Classpaths> {
        let mut cp = Classpaths::default();
        for line in text.lines() {
            let line = line.trim();
            let Some(rest) = line.strip_prefix("export ") else {
                continue;
            };
            let Some((key, value)) = rest.split_once(char::is_whitespace) else {
                continue;
            };
            let list: Vec<String> = value
                .trim()
                .split(':')
                .filter(|v| !v.is_empty())
                .map(str::to_string)
                .collect();
            match key {
                "BOOTCLASSPATH" => cp.boot = list,
                "DEX2OATBOOTCLASSPATH" => cp.dex2oat = list,
                _ => {}
            }
        }
        (!cp.boot.is_empty() && !cp.dex2oat.is_empty()).then_some(cp)
    }

    /// Produce `/data/system/environ/classpath` with the image's own
    /// `derive_classpath` — the component `init` runs on device. This is the
    /// missing input behind ART's fatal
    /// `Check failed: !location.empty() No mainline framework library found`:
    /// libartbase's `GetFirstMainlineFrameworkLibraryFilename` splits the part
    /// of `BOOTCLASSPATH` after the `DEX2OATBOOTCLASSPATH` prefix, so the two
    /// must be the device pair (prefix + mainline remainder), not one string
    /// duplicated into both variables.
    pub fn ensure_classpath_env(&self, regen: bool) -> Result<()> {
        let out = self.classpath_env_file();
        if let Some(dir) = out.parent() {
            fs::create_dir_all(dir)?;
        }
        if !regen
            && out.is_file()
            && self.classpaths().is_some()
            && self.classpath_env_is_fresh(&out)
        {
            return Ok(());
        }
        let key = self.classpath_env_cache_key();
        let cached = crate::build_cache::enabled();
        if cached {
            let _lock = crate::build_cache::lock(&key)?;
            if !regen {
                let dir = out
                    .parent()
                    .map(Path::to_path_buf)
                    .unwrap_or_else(|| self.data.clone());
                if crate::build_cache::reuse_to(&key, &["classpath"], &dir)?.is_some() {
                    crate::stage("classpath-hit");
                    return Ok(());
                }
            }
        }
        let derive = "/apex/com.android.sdkext/bin/derive_classpath";
        if !self.host_path(derive).is_file() {
            return Err(Error::Layout(format!(
                "{} is missing from the sysroot (needed to derive BOOTCLASSPATH/DEX2OATBOOTCLASSPATH)",
                derive
            )));
        }
        // derive_classpath is a bionic binary: the shim gives it logd + props.
        crate::build_shim(&self.data.join("shim"))?;
        let mut sb = self.sandbox();
        sb.staging(true); // host build step: derive_classpath writes /data/system/environ
        sb.with_shim(true);
        let mut cmd = sb.command(derive, &[])?;
        let out_log = cmd.output()?;
        let mut text = Vec::new();
        text.extend_from_slice(&out_log.stdout);
        text.extend_from_slice(&out_log.stderr);
        let log = self.data.join("system/environ/derive_classpath.log");
        fs::write(&log, &text)?;
        let parsed = Self::parse_classpath_env(&fs::read_to_string(&out).unwrap_or_default());
        let Some(cp) = parsed else {
            return Err(Error::Layout(format!(
                "derive_classpath wrote no usable {} (status {:?}, log: {}): {}",
                out.display(),
                out_log.status.code(),
                log.display(),
                String::from_utf8_lossy(&text).trim()
            )));
        };
        if !cp.is_complete() {
            // Old E1A scratch sysroots have no /system/etc/classpaths/, so the
            // platform framework jars are not enumerated. Do not use a partial
            // classpath: fall back to the sysroot's own BOOTCLASSPATH list and
            // a fragment-derived DEX2OATBOOTCLASSPATH prefix.
            eprintln!(
                "acos-art: warning: derive_classpath for {} produced an incomplete classpath \
                 (no platform framework jars; the sysroot lacks /system/etc/classpaths/); \
                 using the sysroot BOOTCLASSPATH list instead (non-device ordering)",
                self.sysroot.display()
            );
            return Ok(());
        }
        // Every enumerated jar must exist in the sysroot, otherwise ART would
        // fail far away from the cause.
        let missing: Vec<&String> = cp
            .boot
            .iter()
            .filter(|j| !self.host_path(j).is_file())
            .collect();
        if !missing.is_empty() {
            return Err(Error::Layout(format!(
                "BOOTCLASSPATH names jars the sysroot lacks: {} (re-run tools/fetch/art-sysroot.sh)",
                missing
                    .iter()
                    .map(|s| s.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )));
        }
        if cached {
            let dir = out
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| self.data.clone());
            crate::build_cache::publish_locked(&key, &dir, &["classpath"])?;
        }
        crate::stage("classpath-built");
        Ok(())
    }

    /// Cache key of `<data>/system/environ/classpath`: the image digest (the
    /// classpath fragments live in the sysroot), the `derive_classpath` binary
    /// identity and the invocation. APK-independent, so one entry serves every
    /// launch against this image.
    fn classpath_env_cache_key(&self) -> String {
        let derive = self.host_path("/apex/com.android.sdkext/bin/derive_classpath");
        crate::build_cache::digest(&[
            b"acos-art/derive-classpath/v2",
            crate::build_cache::image_digest(&self.sysroot).as_bytes(),
            crate::build_cache::content_id(&derive).as_bytes(),
        ])
    }

    fn classpath_env_is_fresh(&self, out: &Path) -> bool {
        let newer = |p: &Path| -> bool {
            match (
                fs::metadata(p).and_then(|m| m.modified()),
                fs::metadata(out).and_then(|m| m.modified()),
            ) {
                (Ok(src), Ok(dst)) => dst >= src,
                _ => false,
            }
        };
        newer(&self.modules_path()) && newer(&self.sysroot.join("READY"))
    }

    /// Fallback `DEX2OATBOOTCLASSPATH` for when `derive_classpath` is not
    /// available: the leading run of `BOOTCLASSPATH` entries that the image's
    /// own classpath fragments mark as `DEX2OATBOOTCLASSPATH`. It only has to
    /// be a proper prefix with a non-empty remainder — the remainder is the
    /// mainline framework part ART derives its boot-image extension from.
    pub fn fallback_dex2oat_classpath(&self) -> Result<Vec<String>> {
        let marked = self.dex2oat_fragment_jars();
        let boot = self.device_bootclasspath()?;
        let prefix: Vec<String> = boot
            .iter()
            .take_while(|j| marked.iter().any(|m| m.trim_end() == j.trim_end()))
            .cloned()
            .collect();
        if !prefix.is_empty() && prefix.len() < boot.len() {
            return Ok(prefix);
        }
        // Last resort: keep one jar for the mainline remainder.
        Ok(boot
            .split_last()
            .map(|(_, rest)| rest.to_vec())
            .unwrap_or_default())
    }

    /// Jars the image's `bootclasspath.pb` fragments put in
    /// `DEX2OATBOOTCLASSPATH` (proto `ClasspathEntry{string jar=1; Classpath
    /// classpath=2;}` with `DEX2OATBOOTCLASSPATH = 1`).
    fn dex2oat_fragment_jars(&self) -> Vec<String> {
        let mut files: Vec<PathBuf> =
            vec![self.sysroot.join("system/etc/classpaths/bootclasspath.pb")];
        if let Ok(rd) = fs::read_dir(self.sysroot.join("system/apex")) {
            for e in rd.filter_map(|e| e.ok()) {
                files.push(e.path().join("etc/classpaths/bootclasspath.pb"));
            }
        }
        let mut jars = Vec::new();
        for f in files {
            let Ok(bytes) = fs::read(&f) else { continue };
            // ClasspathFragment: repeated ClasspathEntry entries = 1;
            for (field, wire, value) in proto_fields(&bytes) {
                if field != 1 || wire != 2 {
                    continue;
                }
                let mut jar = String::new();
                let mut classpath = u64::MAX;
                for (f2, w2, v2) in proto_fields(&value) {
                    match (f2, w2) {
                        (1, 2) => jar = String::from_utf8_lossy(&v2).to_string(),
                        (2, 0) => classpath = u64::from_le_bytes(v2.as_slice().try_into().unwrap()),
                        _ => {}
                    }
                }
                // Classpath.DEX2OATBOOTCLASSPATH == 1
                if classpath == 1 && !jar.is_empty() {
                    jars.push(jar);
                }
            }
        }
        jars
    }

    // ---- sysroot contents -------------------------------------------------

    /// Device `BOOTCLASSPATH`, one `/apex/<module>/javalib/*.jar` or
    /// `/system/framework/*.jar` entry per line.
    ///
    /// Jars named by the reference list but absent from the sysroot are
    /// **skipped** and reported by [`ArtRoot::bootclasspath_missing`] — the
    /// E1A harness behaved the same way (two jars come from APEXes the current
    /// sysroot does not carry: configinfrastructure, ondevicepersonalization).
    pub fn device_bootclasspath(&self) -> Result<Vec<String>> {
        Ok(self.bootclasspath_entries()?.0)
    }

    /// Jars the reference boot classpath names but the sysroot lacks.
    pub fn bootclasspath_missing(&self) -> Vec<String> {
        self.bootclasspath_entries()
            .map(|e| e.1)
            .unwrap_or_default()
    }

    fn bootclasspath_entries(&self) -> Result<(Vec<String>, Vec<String>)> {
        let mut resolved = Vec::new();
        let mut missing = Vec::new();
        if let Ok(text) = fs::read_to_string(self.sysroot.join("BOOTCLASSPATH")) {
            let v: Vec<String> = text
                .lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .map(str::to_string)
                .collect();
            if !v.is_empty() {
                for dev in v {
                    if self.host_path(&dev).is_file() {
                        resolved.push(dev);
                    } else {
                        missing.push(dev);
                    }
                }
                return Ok((resolved, missing));
            }
        }
        // Legacy sysroots (the E1A scratch dir) only ship a bare jar-name list.
        let list = [
            self.repo_root
                .join("research/art-feasibility/a-dalvikvm/bootclasspath.txt"),
            self.repo_root.join(".cache/boot/android-34/BOOTCLASSPATH"),
        ]
        .into_iter()
        .find_map(|p| fs::read_to_string(p).ok())
        .ok_or_else(|| {
            Error::Layout(format!(
                "{} has no BOOTCLASSPATH (re-run tools/fetch/art-sysroot.sh)",
                self.sysroot.display()
            ))
        })?;
        for line in list.lines().map(str::trim).filter(|l| !l.is_empty()) {
            let jar = Path::new(line)
                .file_name()
                .unwrap()
                .to_string_lossy()
                .to_string();
            match self.find_jar(&jar) {
                Some(dev) => resolved.push(dev),
                None => missing.push(jar),
            }
        }
        if resolved.is_empty() {
            return Err(Error::Layout(format!(
                "no boot classpath jar resolved from {}",
                self.sysroot.display()
            )));
        }
        Ok((resolved, missing))
    }

    /// Locate a boot-classpath jar and return its **device** path.
    pub fn find_jar(&self, jar: &str) -> Option<String> {
        for m in &self.modules {
            let dev = if m.name == "com.google.mainline.primary.libs" {
                format!("/apex/sharedlibs/javalib/{jar}")
            } else {
                format!("/apex/{}/javalib/{jar}", m.name)
            };
            if self.host_path(&dev).is_file() {
                return Some(dev);
            }
        }
        let dev = format!("/system/framework/{jar}");
        self.host_path(&dev).is_file().then_some(dev)
    }

    /// Map a device path (`/apex/...` or `/system/...`) back to the host path.
    ///
    /// The sysroot mirrors the device *after* apexd: payloads live under
    /// `<sysroot>/system/apex/<module>/` and are bind-mounted to `/apex/<module>`
    /// (`/apex/sharedlibs` is the SDK shared-library APEX).
    pub fn host_path(&self, dev: &str) -> PathBuf {
        if let Some(rest) = dev.strip_prefix("/apex/") {
            let (module, tail) = match rest.split_once('/') {
                Some((m, t)) => (m, t),
                None => (rest, ""),
            };
            if module == "sharedlibs" {
                return self
                    .sysroot
                    .join("system/apex/com.google.mainline.primary.libs")
                    .join(tail);
            }
            // Device path (`/apex/<apex_manifest name>`) or host directory name.
            if let Some(m) = self
                .modules
                .iter()
                .find(|m| m.mount == module || m.name == module)
            {
                return self.sysroot.join("system/apex").join(&m.name).join(tail);
            }
            return self.sysroot.join("system/apex").join(module).join(tail);
        }
        self.sysroot.join(dev.trim_start_matches('/'))
    }

    /// Map a host sysroot path to its on-device absolute path.
    pub fn dev_path(&self, host: &Path) -> String {
        let s = host.to_string_lossy().to_string();
        let s = s.replace(
            &format!("{}/system/apex/", self.sysroot.display()),
            "/apex/",
        );
        let s = s.replace(&self.sysroot.display().to_string(), "");
        s
    }

    // ---- sandbox ----------------------------------------------------------

    pub fn sandbox(&self) -> Sandbox {
        Sandbox::new(self)
    }
}

/// A per-run writable data root: everything the guest may write (`dalvik-cache`,
/// `tmp`, `out`, `task`, `socket`, `session`, `app-data`) lives here, while the
/// shared build inputs (`app`, `overlay`, `shim`, `system/environ`,
/// `acos.properties`, `linkerconfig`, the boot image) are bound read-only. The
/// tree is removed on drop, which covers every exit path of a route (ok, guest
/// crash, timeout, launcher error); a SIGKILLed launcher is covered by
/// [`ArtRoot::sweep_stale_runs`]. See `results/art-runtime/S4.md`.
pub struct RunDir {
    pub path: PathBuf,
    keep: bool,
}

impl RunDir {
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Create (if needed) and return a writable subdirectory of the run root.
    pub fn dir(&self, sub: &str) -> PathBuf {
        let p = self.path.join(sub);
        let _ = fs::create_dir_all(&p);
        p
    }

    /// Leave the tree in place (debugging: `ACOS_ART_KEEP_RUN=1`).
    pub fn keep(mut self) {
        self.keep = true;
    }
}

// ------------------------------------------------- crash run-tree retention --

/// Marker written into a run tree that was kept because the guest crashed, so
/// retention can find *its own* trees and never touches `--keep`/`ACOS_ART_KEEP_RUN`
/// or `ACOS_ART_KEEP_SESSION` trees.
pub const CRASH_KEEP_MARKER: &str = "kept-for-crash.json";

/// Default `ACOS_ART_KEEP_CRASH_MAX`: how many kept crash trees one data root
/// may hold. 8 is enough for a debugging session and bounded under a crash loop.
pub const KEEP_CRASH_MAX_DEFAULT: usize = 8;

/// Signals a **guest raised itself** when it crashes. A signal the *host* sends
/// — SIGKILL from GL1's bounded destroy, SIGTERM/SIGINT/SIGHUP from a watchdog or
/// an operator — is a teardown decision, never crash evidence, and must never
/// keep a tree: every normally-torn-down session would leak its run tree
/// (extracted native libs included) and a crash loop would fill the disk.
pub const GUEST_CRASH_SIGNALS: [i32; 7] = [
    4,  // SIGILL
    5,  // SIGTRAP
    6,  // SIGABRT
    7,  // SIGBUS
    8,  // SIGFPE
    11, // SIGSEGV
    31, // SIGSYS (a guest syscall filter kill is still the guest's own end)
];

/// Is `signal` one a guest raises itself (as opposed to one the host sends)?
#[must_use]
pub fn is_guest_crash_signal(signal: i32) -> bool {
    GUEST_CRASH_SIGNALS.contains(&signal)
}

/// `ACOS_ART_KEEP_CRASH_MAX` (default [`KEEP_CRASH_MAX_DEFAULT`], `0` = keep
/// none): how many kept crash run trees one data root may hold.
#[must_use]
pub fn keep_crash_max() -> usize {
    std::env::var("ACOS_ART_KEEP_CRASH_MAX")
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .unwrap_or(KEEP_CRASH_MAX_DEFAULT)
}

/// The retention decision, spelled out so it is testable without a guest:
/// keep the tree only when the guest died of a signal **it raised itself** *and*
/// its crash record was actually parsed. A SIGKILL (9), SIGTERM (15), SIGHUP (1)
/// or SIGINT (2) never keeps anything, and neither does a crash signal whose
/// evidence could not be captured (nothing to debug then).
#[must_use]
pub fn keeps_run_tree_for_crash(signal: Option<i32>, has_native_crash: bool) -> bool {
    has_native_crash && matches!(signal, Some(s) if is_guest_crash_signal(s))
}

/// Kept crash trees under `runs_root`, oldest first (marker mtime, then path).
fn kept_crash_trees(runs_root: &Path) -> Vec<(std::time::SystemTime, PathBuf)> {
    let mut trees: Vec<(std::time::SystemTime, PathBuf)> = Vec::new();
    let Ok(rd) = fs::read_dir(runs_root) else {
        return trees;
    };
    for e in rd.flatten() {
        let dir = e.path();
        if !e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        let marker = dir.join(CRASH_KEEP_MARKER);
        let Ok(meta) = fs::metadata(&marker) else {
            continue;
        };
        let at = meta.modified().unwrap_or(std::time::SystemTime::UNIX_EPOCH);
        trees.push((at, dir));
    }
    trees.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
    trees
}

/// Prune kept crash trees under `runs_root` until at most `keep` remain, oldest
/// first. Only trees carrying [`CRASH_KEEP_MARKER`] are candidates: an explicit
/// `ACOS_ART_KEEP_RUN`/`ACOS_ART_KEEP_SESSION` tree is never pruned. Returns what
/// was removed, and says so on stderr (a deletion is never silent).
pub fn prune_kept_crash_trees(runs_root: &Path, keep: usize) -> Vec<PathBuf> {
    let mut trees = kept_crash_trees(runs_root);
    let mut removed = Vec::new();
    while trees.len() > keep {
        let (_, path) = trees.remove(0);
        match fs::remove_dir_all(&path) {
            Ok(()) => {
                eprintln!(
                    "acos-art: pruned kept crash tree {} (ACOS_ART_KEEP_CRASH_MAX={keep})",
                    path.display()
                );
                removed.push(path);
            }
            Err(e) => eprintln!(
                "acos-art: warning: could not prune kept crash tree {}: {e}",
                path.display()
            ),
        }
    }
    removed
}

/// Keep `run` because its guest crashed, under the retention cap.
///
/// Returns the kept path (for `session-exit.json.kept_run_dir`) or `None` when
/// nothing was kept — either the cap is `0` or the tree did not survive pruning.
/// `ACOS_ART_KEEP_SESSION=1` keeps everything and is exempt from the cap, as
/// before (explicit opt-in).
pub fn keep_run_for_guest_crash(run: RunDir, detail: serde_json::Value) -> Option<PathBuf> {
    let path = run.path().to_path_buf();
    let root = path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| path.clone());
    if std::env::var_os("ACOS_ART_KEEP_SESSION").is_some() {
        eprintln!(
            "acos-art: keeping crashed run tree {} (ACOS_ART_KEEP_SESSION=1: keep everything)",
            path.display()
        );
        run.keep();
        return Some(path);
    }
    let cap = keep_crash_max();
    if cap == 0 {
        // "Keep none": drop any kept crash trees from earlier runs as well, so
        // the variable means what it says. This run's tree is removed by Drop.
        let _ = prune_kept_crash_trees(&root, 0);
        return None;
    }
    let _ = fs::write(
        path.join(CRASH_KEEP_MARKER),
        serde_json::to_vec_pretty(&detail).unwrap_or_default(),
    );
    let pruned = prune_kept_crash_trees(&root, cap);
    if !path.join(CRASH_KEEP_MARKER).is_file() {
        // Pruned under us (a concurrent run lowered the cap): nothing to keep.
        return None;
    }
    run.keep();
    eprintln!(
        "acos-art: kept crashed run tree {} ({} of at most {}; {} pruned)",
        path.display(),
        kept_crash_trees(&root).len(),
        cap,
        pruned.len()
    );
    Some(path)
}

// ------------------------------------------------- guest GWP-ASan policy --

/// `ACOS_ART_GWP_ASAN` (default **off**): give the guest libc's GWP-ASan
/// sampling allocator on purpose.
///
/// GWP-ASan is a *field-sampling* debugging allocator, not a general-purpose
/// one: `MallocInitImpl` calls `MaybeInitGwpAsanFromLibc` on the first malloc of
/// every process whose program name is not `app_process64`
/// (`bionic/libc/bionic/malloc_common_dynamic.cpp`), and that call picks the
/// process with `arc4random_uniform`-style sampling — `__libc_safe_arc4random_buf`
/// byte `% 128 == 0`.
///
/// When it is on, every sampled allocation captures a backtrace through
/// `Options::Backtrace = android_unsafe_frame_pointer_chase`
/// (`bionic/libc/bionic/gwp_asan_wrappers.cpp:258`). That function
/// (`bionic/libc/bionic/android_unsafe_frame_pointer_chase.cpp`) walks the
/// frame-pointer chain from the caller outward, and it dereferences
/// `frame->return_addr` at `frame + 8` after only checking that `frame` itself
/// is below `__get_thread_stack_top()`. On x86_64 the guest's optimized and JIT
/// frames carry no frame pointers, so the walk follows stale stack words; the
/// last 8 bytes of the topmost stack page are a legal `frame`, and reading
/// `frame + 8` lands in the guard page → `SIGSEGV (SEGV_ACCERR)` at
/// `android_unsafe_frame_pointer_chase+0x117` (`mov 0x8(%r12),%rcx`) on a thread
/// named after the app, i.e. the G3d/R2a launch flake.
///
/// ACOS decides, so the guest is deterministic: unless the caller asks
/// (`ACOS_ART_GWP_ASAN=on`, or an explicit host `GWP_ASAN_*` value), the guest
/// gets `GWP_ASAN_PROCESS_SAMPLING=0`, which disables GWP-ASan cleanly
/// (`MaybeInitGwpAsan`: `process_sample_rate == 0` → returns false before
/// `arc4random` is consulted).
///
/// `ACOS_ART_GWP_ASAN=libc` (or `default`) forwards nothing, i.e. libc's own
/// behaviour — the pre-G3d baseline, kept as an A/B seam and as the only way to
/// reproduce the original flake on one binary.
///
/// Returns the environment pairs to add to the guest sandbox.
#[must_use]
pub fn gwp_asan_guest_env(
    host_env: impl Fn(&str) -> Option<String>,
    force: Option<&str>,
) -> Vec<(String, String)> {
    const KNOBS: [&str; 4] = [
        "GWP_ASAN_SAMPLE_RATE",
        "GWP_ASAN_PROCESS_SAMPLING",
        "GWP_ASAN_MAX_ALLOCS",
        "GWP_ASAN_RECOVERABLE",
    ];
    // An operator can re-enable the allocator for one debugging run, and an
    // explicit host value is passed through unchanged rather than overridden.
    let mode = force.map(str::trim);
    let asked = matches!(mode, Some("1" | "on" | "true" | "yes"));
    let libc = matches!(mode, Some("libc" | "default" | "inherit"));
    let explicit: Vec<(String, String)> = KNOBS
        .iter()
        .filter_map(|k| host_env(k).map(|v| ((*k).to_string(), v)))
        .collect();
    if !explicit.is_empty() {
        return explicit;
    }
    if libc {
        return Vec::new();
    }
    if asked {
        return vec![("GWP_ASAN_PROCESS_SAMPLING".to_string(), "1".to_string())];
    }
    vec![("GWP_ASAN_PROCESS_SAMPLING".to_string(), "0".to_string())]
}

impl Drop for RunDir {
    fn drop(&mut self) {
        if self.keep {
            return;
        }
        crate::webview_socket::cleanup_session_dir(&self.path);
        match fs::remove_dir_all(&self.path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => eprintln!(
                "acos-art: warning: could not remove run dir {}: {e}",
                self.path.display()
            ),
        }
    }
}

/// W39: empty a derived build directory (`<data>/linkerconfig`) before a cache
/// miss rebuilds it, so the published entry never carries a previous sysroot's
/// files. The directory itself stays (the sandbox binds it at `/linkerconfig`).
fn clear_derived_tree(dir: &Path) -> Result<()> {
    let rd = match fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(Error::Io(e)),
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            fs::remove_dir_all(&p)?;
        } else {
            fs::remove_file(&p)?;
        }
    }
    Ok(())
}

/// `ACOS_ART_RUN_TTL_SECS` (default 30 min): how long an orphaned run dir is
/// kept before the janitor removes it.
fn run_ttl_from_env() -> u64 {
    std::env::var("ACOS_ART_RUN_TTL_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(1800)
}

/// Is the pid recorded in `<run>/owner` still alive **and** the same process?
fn run_dir_is_live(path: &Path) -> bool {
    let Ok(text) = fs::read_to_string(path.join("owner")) else {
        return false;
    };
    let mut pid = 0u32;
    let mut start = String::new();
    for line in text.lines() {
        if let Some(v) = line.strip_prefix("pid=") {
            pid = v.trim().parse().unwrap_or(0);
        } else if let Some(v) = line.strip_prefix("start=") {
            start = v.trim().to_string();
        }
    }
    if pid == 0 {
        return false;
    }
    let Ok(stat) = fs::read_to_string(format!("/proc/{pid}/stat")) else {
        return false;
    };
    let Some(i) = stat.rfind(')') else {
        return false;
    };
    let now_start = stat[i + 2..]
        .split_whitespace()
        .nth(19)
        .unwrap_or("")
        .to_string();
    !start.is_empty() && now_start == start
}

/// Hardening flags every ART run gets, in this order, **before** the first
/// mount (`--tmpfs /`): the namespace has to exist before any mount is set up.
///
/// Measured, not assumed (`results/art-runtime/S2.md`, `S1-sandbox-review.md`
/// §5 #1): the corpus is 27/27 with these and the `/proc`, network, IPC, UTS
/// and cgroup views are the guest's own. `--die-with-parent` is the *only*
/// orphan protection that works here, because bwrap's credential changes clear
/// the launcher's `PR_SET_PDEATHSIG`. `--clearenv` means the guest environment
/// is exactly the `--setenv` list below (plus bwrap's `PWD`/`_`) — nothing
/// inherited from the host. `--cap-drop ALL` is redundant (bwrap's implicit
/// user namespace already zeroes the capability sets) but costs nothing.
///
/// `--unshare-user` is made explicit for runs because it is what lets the guest
/// carry an **application** uid instead of the host caller's uid (see
/// [`Sandbox::app_uid`] and [`guest_uid_override`]); bwrap would create the
/// namespace implicitly anyway.
/// `--disable-userns` and `--new-session` remain absent (untested against ART;
/// see S2).
const SANDBOX_FLAGS: &[&str] = &[
    "--unshare-pid",
    "--unshare-net",
    "--unshare-ipc",
    "--unshare-uts",
    "--unshare-cgroup-try",
    "--die-with-parent",
    "--clearenv",
    "--cap-drop",
    "ALL",
];

/// The uid the guest runs as inside the sandbox's user namespace.
///
/// `bwrap --unshare-user --uid N` maps inner `N` onto the host caller's uid, so
/// every host file the run binds stays accessible (it appears owned by `N`
/// inside) while the guest's *kernel* uid — and therefore `Process.myUid()`,
/// `Libcore.os.getuid()`, `/proc/self/status` — is an application uid.
///
/// Default: [`acos_services::pms::FIXTURE_UID`] (10192), the uid PMS/AMS already
/// answer for an installed package, so `Process.myUid()` and
/// `getPackageUid()` agree. This is a fidelity fix, not a cosmetic one: the
/// host caller is uid 1000, which is Android's `Process.SYSTEM_UID`. Framework
/// code branches on it — `WebViewFactory.getProvider()` refuses for
/// `ROOT_UID`/`SYSTEM_UID`/`SHELL_UID`/`NETWORK_STACK_UID`/`BLUETOOTH_UID`
/// (measured from the API-34 framework dex: uids 0, 1000, 1001, 1027, 1002) and
/// throws `UnsupportedOperationException: For security reasons, WebView is not
/// allowed in privileged processes`, so every app that probes WebView died in a
/// *system* branch it can never take on a device.
///
/// A multi-process session overrides that default per process with
/// [`Sandbox::app_uid`]: AOSP gives every installed package its own uid
/// (`PackageManagerService` allocates it), processes of one package share it and
/// processes of different packages differ, so the guest that runs a package must
/// carry **that package's** uid — otherwise `Process.myUid()`, `getPackageUid()`
/// and the per-uid permission answers would describe one app while the code of
/// another runs.
///
/// `ACOS_ART_GUEST_UID=host` keeps the old uid for A/B work, and a numeric value
/// pins one uid for the whole run. Both are explicit overrides of the per-process
/// uid (a seam, not a default), and a value that is neither `host` nor an
/// application uid (`10000..=19999`, Android's `Process.FIRST_APPLICATION_UID..=`
/// `LAST_APPLICATION_UID`) is an explicit `INVALID_GUEST_UID` error, never a
/// silent host-uid fallback (host uid 1000 is `SYSTEM_UID`).
fn guest_uid_override() -> Result<Option<Option<u32>>> {
    let raw = match std::env::var("ACOS_ART_GUEST_UID") {
        Err(std::env::VarError::NotPresent) => return Ok(None),
        Err(e) => return Err(Error::InvalidGuestUid(e.to_string())),
        Ok(v) => v,
    };
    parse_guest_uid_override(Some(&raw))
}

/// [`guest_uid_override`] without the environment: the seam's whole rule, so a
/// unit test can pin it (`ACOS_ART_GUEST_UID` is process-wide and every test in
/// this binary shares it).
fn parse_guest_uid_override(raw: Option<&str>) -> Result<Option<Option<u32>>> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    let value = raw.trim();
    if value.eq_ignore_ascii_case("host") {
        return Ok(Some(None));
    }
    let uid = value.parse::<u32>().map_err(|_| {
        Error::InvalidGuestUid(format!("{raw:?} is neither `host` nor an application uid"))
    })?;
    if !acos_services::pms::is_application_uid(uid) {
        return Err(Error::InvalidGuestUid(format!(
            "uid {uid} is not an application uid ({first}..={last}): the guest must not \
             run as a system uid",
            first = acos_services::pms::FIRST_APPLICATION_UID,
            last = acos_services::pms::LAST_APPLICATION_UID
        )));
    }
    Ok(Some(Some(uid)))
}

/// Builder for the `bwrap` command line: the mini-Android root, its
/// environment, and any extra host directories the task needs.
pub struct Sandbox<'a> {
    root: &'a ArtRoot,
    ro_binds: Vec<(PathBuf, String)>,
    rw_binds: Vec<(PathBuf, String)>,
    dirs: Vec<String>,
    env: Vec<(String, String)>,
    shim: bool,
    image: Option<PathBuf>,
    args: Vec<String>,
    /// Per-run writable data root (`None` = the one shared `<data>` tree).
    run_root: Option<PathBuf>,
    /// Guest-side seccomp filter (S5): preload the freestanding stub, which
    /// installs the filter and logs SIGSYS denials.
    seccomp_guest: bool,
    /// The application uid **this** guest carries (a package's uid, per process).
    /// `None` = the historical session-wide default ([`guest_uid_override`],
    /// else the PMS fixture uid).
    app_uid: Option<u32>,
    /// Host-side build step: keep today's fully-writable `<data>` (regenerating
    /// `linkerconfig`, `derive_classpath`, `dex2oat` boot image). Never an APK run.
    staging: bool,
    /// NET2: the session id this run belongs to. The relay arm names its net
    /// session after it, so the live session API addresses the same session.
    net_session: Option<String>,
}

/// The guest's `ANDROID_ROOT`.
///
/// It is `/system` for every framework lookup (AssetManager, `Environment`,
/// ART's own root discovery). The trailing slash is deliberate: ART only
/// installs its **unexpected-signal dumper** when `getenv("ANDROID_ROOT")` is
/// not exactly `/system` (`art/runtime/runtime_android.cc`,
/// `Runtime::InitPlatformSignalHandlers` → `HandleUnexpectedSignalAndroid` →
/// `art::HandleUnexpectedSignalCommon`). That dumper is the runtime's own
/// native-crash report — `*** *** ***`, `Fatal signal N (SIGSEGV), code …
/// fault addr …`, `Thread: <tid> "<name>"`, registers and a symbolised
/// `Backtrace` with mapped library paths — and without it a guest killed by a
/// signal leaves nothing (R2 lost a Gaten launch to exactly that: no bionic
/// debuggerd handler in this image, no ART dump, no stack).
///
/// `/system/` resolves to the same directory for every path concatenation
/// (`/system//framework/…`, `/system/fonts`, prefix checks against
/// `$ANDROID_ROOT/`), so the framework is unaffected; the only behavioural
/// difference is the one this comment describes. `ACOSART_CRASH_DUMP=0` puts
/// the exact `/system` back (A/B seam).
fn android_root_for_guest() -> &'static str {
    match std::env::var("ACOSART_CRASH_DUMP") {
        Ok(v) if v.trim() == "0" => "/system",
        _ => "/system/",
    }
}

impl<'a> Sandbox<'a> {
    fn new(root: &'a ArtRoot) -> Self {
        let mut env = vec![
            ("ANDROID_ROOT".into(), android_root_for_guest().to_string()),
            (
                "ANDROID_RUNTIME_ROOT".into(),
                "/apex/com.android.runtime".into(),
            ),
            ("ANDROID_ART_ROOT".into(), "/apex/com.android.art".into()),
            ("ANDROID_I18N_ROOT".into(), "/apex/com.android.i18n".into()),
            (
                "ANDROID_TZDATA_ROOT".into(),
                "/apex/com.google.android.tzdata5".into(),
            ),
            ("ANDROID_DATA".into(), "/data".into()),
            ("ANDROID_STORAGE".into(), "/data".into()),
            ("ANDROID_TMP".into(), "/data/tmp".into()),
            (
                "ANDROID_ART_PROPERTIES".into(),
                "/data/acos.properties".into(),
            ),
        ];
        env.push(("BOOTCLASSPATH".into(), String::new())); // filled in command()
        Self {
            root,
            ro_binds: Vec::new(),
            rw_binds: Vec::new(),
            dirs: Vec::new(),
            env,
            shim: false,
            image: None,
            args: Vec::new(),
            run_root: None,
            seccomp_guest: false,
            app_uid: None,
            staging: false,
            net_session: None,
        }
    }

    /// NET2: name the relay session after the caller's session id, so
    /// `offline`/`delay`/`hostport` on that id reach the guest's netns.
    pub fn net_session(&mut self, id: Option<&str>) -> &mut Self {
        self.net_session = id.map(str::to_string);
        self
    }

    /// Make this run install the guest-side seccomp-bpf filter before ART runs.
    /// The freestanding stub is preloaded (it installs the filter from
    /// `/data/seccomp/seccomp.bpf` in its constructor and logs denials).
    pub fn seccomp_guest(&mut self, on: bool) -> &mut Self {
        self.seccomp_guest = on;
        self
    }

    /// Use a per-run writable data root: shared inputs become read-only in the
    /// guest and everything writable is private to this run (S4).
    pub fn run_root(&mut self, dir: impl AsRef<Path>) -> &mut Self {
        self.run_root = Some(dir.as_ref().to_path_buf());
        self
    }

    /// The application uid **this** guest process carries — the uid the package
    /// manager allocated for the package it runs (AOSP: one uid per package,
    /// shared by that package's processes). See [`guest_uid_override`] for the
    /// session-wide default this replaces and for the explicit A/B seam that
    /// still overrides it.
    pub fn app_uid(&mut self, uid: u32) -> &mut Self {
        self.app_uid = Some(uid);
        self
    }

    /// The uid the guest's user namespace maps its own uid onto.
    ///
    /// `Ok(None)` = keep the host caller's uid (`ACOS_ART_GUEST_UID=host`).
    fn run_uid(&self) -> Result<Option<u32>> {
        if let Some(explicit) = guest_uid_override()? {
            return Ok(explicit);
        }
        Ok(Some(
            self.app_uid
                .unwrap_or(acos_services::pms::FIXTURE_UID as u32),
        ))
    }

    /// Staging mode (host-side build steps only): keep the shared `<data>` tree
    /// writable, and never apply the per-run layout.
    pub fn staging(&mut self, staging: bool) -> &mut Self {
        self.staging = staging;
        self
    }

    /// The effective per-run root, if this sandbox is a run rather than a
    /// host-side staging step.
    fn active_run_root(&self) -> Option<&Path> {
        if self.staging {
            None
        } else {
            self.run_root.as_deref()
        }
    }

    pub fn ro_bind(&mut self, host: impl AsRef<Path>, guest: &str) -> &mut Self {
        self.ro_binds
            .push((host.as_ref().to_path_buf(), guest.to_string()));
        self
    }

    pub fn rw_bind(&mut self, host: impl AsRef<Path>, guest: &str) -> &mut Self {
        self.rw_binds
            .push((host.as_ref().to_path_buf(), guest.to_string()));
        self
    }

    pub fn dir(&mut self, guest: &str) -> &mut Self {
        self.dirs.push(guest.to_string());
        self
    }

    pub fn env(&mut self, k: &str, v: &str) -> &mut Self {
        self.env.push((k.to_string(), v.to_string()));
        self
    }

    /// Load the liblog/properties shim (compiled into the data dir).
    pub fn with_shim(&mut self, shim: bool) -> &mut Self {
        self.shim = shim;
        self
    }

    /// Use an AOT boot image directory (`<dir>/x86_64/boot.art`).
    pub fn with_image(&mut self, dir: Option<PathBuf>) -> &mut Self {
        self.image = dir;
        self
    }

    /// Extra dalvikvm options / program arguments.
    pub fn args(&mut self, args: Vec<String>) -> &mut Self {
        self.args = args;
        self
    }

    /// Build the bwrap [`Command`] for `program` (an on-device path).
    pub fn command(&self, program: &str, argv: &[String]) -> Result<Command> {
        let r = self.root;
        let mut b = Command::new("bwrap");
        let mut a: Vec<OsString> = Vec::new();
        // Namespaces, env clearing and pid-1 supervision first: bwrap applies
        // options in order, so these must precede every mount and `--setenv`.
        // SPIKE-NET: with the spike on, `--unshare-net` is dropped because the
        // network namespace is the one pasta created (see `spike_net`).
        for f in spike_net::sandbox_flags() {
            a.push(f.into());
        }
        // Runs get an application uid (see `run_uid`); host-side staging
        // steps and the argv-shape tests keep the caller's uid.
        if self.active_run_root().is_some() {
            if let Some(uid) = self.run_uid()? {
                a.push("--unshare-user".into());
                a.push("--uid".into());
                a.push(uid.to_string().into());
                // PROCUID: an app process's primary group is its own uid
                // (`Process.myGid()` == uid on a device; the supplementary
                // groups are the permissions' `inet` etc.). One id per process,
                // mapped onto the caller's inside this process's own user
                // namespace.
                a.push("--gid".into());
                a.push(uid.to_string().into());
            }
        }
        a.push("--tmpfs".into());
        a.push("/".into());
        a.push("--ro-bind".into());
        a.push(r.sysroot.join("system").into());
        a.push("/system".into());
        a.push("--dir".into());
        a.push("/apex".into());
        a.push("--ro-bind".into());
        a.push(r.apex_info_list().into());
        a.push("/apex/apex-info-list.xml".into());
        for m in &r.modules {
            let guest = m.guest();
            a.push("--ro-bind".into());
            a.push(r.sysroot.join("system/apex").join(&m.name).into());
            a.push(guest.into());
        }
        let run_root = self.active_run_root();
        if let Some(run) = run_root {
            // `/linkerconfig` is a **host build input** at run time: regenerating
            // it is a staging step (`ensure_linkerconfig`), never part of an APK
            // run, so the guest only ever sees the generated file read-only.
            a.push("--ro-bind".into());
            a.push(r.data.join("linkerconfig").into());
            a.push("/linkerconfig".into());
            // A fresh /data: shared inputs read-only, the writable subtrees are
            // this run's private root (S4). Nothing else under <data> is visible.
            a.push("--dir".into());
            a.push("/data".into());
            for sub in ["system", "app", "overlay", "lib", "shim"] {
                let host = r.data.join(sub);
                if host.exists() {
                    a.push("--ro-bind".into());
                    a.push(host.into());
                    a.push(format!("/data/{sub}").into());
                }
            }
            for f in ["apex-info-list.xml", props::PROPS_FILE] {
                let host = r.data.join(f);
                if host.exists() {
                    a.push("--ro-bind".into());
                    a.push(host.into());
                    a.push(format!("/data/{f}").into());
                }
            }
            // `ACOS_ART_DALVIK_CACHE=shared` keeps the historic warm shared
            // cache (fast, but a guest can poison a later run); the default is
            // the per-run cache. Measured in results/art-runtime/S4.md.
            let shared_cache = std::env::var("ACOS_ART_DALVIK_CACHE")
                .map(|v| v == "shared")
                .unwrap_or(false);
            for (sub, guest) in [
                ("dalvik-cache", "/data/dalvik-cache"),
                ("misc", "/data/misc"),
            ] {
                let host = if shared_cache {
                    r.data.join(sub)
                } else {
                    run.join(sub)
                };
                let _ = fs::create_dir_all(&host);
                a.push("--bind".into());
                a.push(host.into());
                a.push(guest.into());
            }
            for (sub, guest) in [
                ("tmp", "/data/tmp"),
                ("out", "/data/out"),
                ("task", "/data/task"),
                ("app-data", "/data/app-data"),
            ] {
                let host = run.join(sub);
                if host.exists() {
                    a.push("--bind".into());
                    a.push(host.into());
                    a.push(guest.into());
                }
            }
            // Per-session dirs (`/data/session/<id>`) are bound by the route
            // that opens a session; the parent only has to exist.
            a.push("--dir".into());
            a.push("/data/session".into());
            // WV4: the WebView session socket (`results/handoff/WVB/WIRE.md`
            // §0). The host broker listens on `host_dir/webview.sock`; the
            // guest only ever sees `/run/acos/webview.sock` and connects to it
            // through the image's own `LocalSocketImpl` natives. The
            // *directory* is what is mounted (read-only), so the broker may
            // create its listener at any point after the guest starts and the
            // guest can connect to a node it cannot unlink. Without the broker
            // the connect fails with the kernel's real ENOENT — there is no
            // fallback transport.
            let webview = crate::webview_socket::host_dir(run)?;
            a.push("--ro-bind".into());
            a.push(webview.into());
            a.push(crate::webview_socket::GUEST_DIR.into());
        } else {
            a.push("--bind".into());
            a.push(r.data.join("linkerconfig").into());
            a.push("/linkerconfig".into());
            a.push("--bind".into());
            a.push(r.data.clone().into());
            a.push("/data".into());
        }
        a.push("--dev".into());
        a.push("/dev".into());
        a.push("--proc".into());
        a.push("/proc".into());
        a.push("--ro-bind".into());
        a.push("/sys".into());
        a.push("/sys".into());
        let mut fixed_dirs = vec!["/vendor", "/product", "/system_ext"];
        if run_root.is_none() {
            // Present only in the shared-tree layout; a run gets /data/tmp from
            // its own root instead (`--dir` over a bind would be redundant).
            fixed_dirs.push("/data/tmp");
        }
        fixed_dirs.push("/data/local/tmp");
        for d in fixed_dirs {
            a.push("--dir".into());
            a.push(d.into());
        }
        // GLSHIP: the software GLES provider — the image's default
        // (`angle-swiftshader`) when it is installed, `ACOS_GUEST_GLES=off` to
        // turn it off, `ACOS_GUEST_GLES=<name>` to pick another (an explicitly
        // named provider that is not installed is an error, never a silent
        // fall back to the default).
        //
        // The image's own EGL loader (`Loader.cpp`'s
        // `attempt_to_load_system_driver`) looks for
        // `/vendor/lib64/egl/libEGL_<ro.hardware.egl>.so`,
        // `libGLESv1_CM_<tag>.so` and `libGLESv2_<tag>.so`. Binding the staged
        // provider there is the *whole* integration: no hook, no interposer,
        // no change to libhwui. Nothing in the bound directory is mapped until
        // libhwui's RenderThread first asks libEGL for a context, so a process
        // that never touches GL pays no resident memory (only the mount entry);
        // the digest is cached per staged inode set (`gles::verify`), so a
        // launch that never touches GL pays one `stat` walk and no read of the
        // 32 MB provider.
        //
        // A provider whose bytes do not match its manifest is a structured
        // error here, never a different library reaching the loader
        // (A11YPACE's stale-shim lesson). One selection rule (`gles::status`)
        // and one verify cache for every provider (GLSHIP's `.verified`).
        let gles_status = gles::status(r)?;
        let mut gpu_landlock_plan: Option<PathBuf> = None;
        if let Some(provider) = &gles_status.provider {
            if !gles::lib_dir_ready(provider, r) {
                return Err(Error::Layout(format!(
                    "ACOS_GUEST_GLES={}: {} is not a directory (install the provider with \
                     `tools/probes/art-gles.sh`)",
                    provider.name,
                    gles::lib_dir(provider, r).display()
                )));
            }
            let started = std::time::Instant::now();
            gles::verify(provider, r)?;
            if std::env::var_os("ACOS_GUEST_GLES_TRACE").is_some() {
                eprintln!(
                    "acos-art: {} [{}] (verify {}ms, staged {} bytes)",
                    gles::describe(provider),
                    gles_status.detail,
                    started.elapsed().as_millis(),
                    gles::staged_bytes(provider)
                );
            }
            // One bind, three lookups: `/vendor/lib64/egl/libEGL_<tag>.so` is
            // what `Loader::findLibrary` opens, `/vendor/lib64` is where
            // the linker resolves that driver's own NEEDED entries, and
            // `/vendor/lib64/hw/vulkan.<tag>.so` (GLSPIKE2) is the ICD the
            // image's `libvulkan.so` opens when the provider carries one.
            a.push("--ro-bind".into());
            a.push(gles::lib_dir(provider, r).into());
            a.push("/vendor/lib64".into());
            // GPUSPIKE: a host-GPU provider (`mesa-host`) also names the one
            // DRM *render* node it draws on. It is bound read-write at the same
            // path (`--dev-bind` keeps the device node usable), only here, only
            // for the selected provider, and only `/dev/dri/renderD<N>` passes
            // validation: no card node (modesetting/master), no other device.
            // Waydroid's own `gralloc.gbm.device` prop names the same path.
            if let Some(node) = provider.render_node()? {
                // The guest's Landlock plan handles WRITE_FILE and IOCTL_DEV, so
                // the node is also granted in a plan that is bound over the
                // default one for this run only (below, after /data/seccomp).
                if self.seccomp_guest {
                    gpu_landlock_plan =
                        landlock::write_gpu_plan(&r.data.join("seccomp"), &node)?;
                }
                a.push("--dev-bind".into());
                a.push(node.clone().into());
                a.push(node.into());
            }
        }
        a.push("--ro-bind".into());
        a.push("/etc/passwd".into());
        a.push("/etc/passwd".into());
        for d in &self.dirs {
            a.push("--dir".into());
            a.push(d.into());
        }
        for (host, guest) in &self.ro_binds {
            a.push("--ro-bind".into());
            a.push(host.into());
            a.push(guest.into());
        }
        for (host, guest) in &self.rw_binds {
            a.push("--bind".into());
            a.push(host.into());
            a.push(guest.into());
        }
        if self.seccomp_guest {
            // The filter blob and the freestanding installer: read-only inputs.
            let dir = r.data.join("seccomp");
            if !dir.join("seccomp.bpf").is_file() || !dir.join("libacos_seccomp_guest.so").is_file()
            {
                return Err(Error::SeccompUnavailable(format!(
                    "guest guard not built in {} (run `acos-art build-shim`)",
                    dir.display()
                )));
            }
            a.push("--ro-bind".into());
            a.push(dir.into());
            a.push("/data/seccomp".into());
            if let Some(plan) = gpu_landlock_plan.take() {
                a.push("--ro-bind".into());
                a.push(plan.into());
                a.push(landlock::GUEST_PLAN.into());
            }
            // The program is installed by the preloaded stub's constructor, so
            // it lands after bionic finished loading and before ART's main.
        }
        if self.shim {
            let shim = r.data.join("shim/libartshim.so");
            if !shim.is_file() {
                return Err(Error::Layout(format!(
                    "shim not built: {} (run `acos-art build-shim`)",
                    shim.display()
                )));
            }
            // /data is writable, so bwrap can create the bind mount point
            // (/system stays strictly read-only).
            a.push("--ro-bind".into());
            a.push(shim.into());
            a.push("/data/shim/libartshim.so".into());
            // The logd socket lives in the data dir, so bind the socket dir
            // over the bwrap-supplied /dev. A run's socket dir is its own, not
            // the shared one.
            a.push("--bind".into());
            match run_root {
                Some(run) => a.push(run.join("socket").into()),
                None => a.push(r.data.join("socket").into()),
            };
            a.push("/dev/socket".into());
        }

        // ---- environment ----
        // BOOTCLASSPATH / DEX2OATBOOTCLASSPATH must be the device pair: the
        // second is a strict prefix of the first and the remainder (the
        // mainline framework jars) is what ART derives its boot-image mainline
        // extension from. Both equal, or the second unset, is fatal
        // ("No mainline framework library found").
        let (bc, dx) = match r.classpaths() {
            Some(cp) => (cp.boot, Some(cp.dex2oat)),
            // Before `derive_classpath` has run (e.g. the linkerconfig step).
            None => (
                r.device_bootclasspath()?,
                Some(r.fallback_dex2oat_classpath()?),
            ),
        };
        // One LD_PRELOAD line, assembled from every source: the guest seccomp
        // guard first (its constructor installs the filter before any other
        // preloaded library runs), then whatever the route/shims installed.
        // bwrap keeps only the *last* `--setenv` of a key, so duplicates would
        // silently drop the guard.
        let mut preload: Vec<String> = self
            .env
            .iter()
            .filter(|(k, _)| k == "LD_PRELOAD")
            .map(|(_, v)| v.clone())
            .collect();
        if self.shim {
            preload.push("/data/shim/libartshim.so".to_string());
        }
        if self.seccomp_guest {
            preload.insert(0, seccomp::GUEST_SO.to_string());
        }
        for (k, v) in &self.env {
            if k == "LD_PRELOAD" {
                continue;
            }
            let v = if k.as_str() == "BOOTCLASSPATH" {
                bc.join(":")
            } else {
                v.clone()
            };
            a.push("--setenv".into());
            a.push(k.as_str().into());
            a.push(v.as_str().into());
        }
        if let Some(dx) = dx {
            a.push("--setenv".into());
            a.push("DEX2OATBOOTCLASSPATH".into());
            a.push(dx.join(":").as_str().into());
        }
        if !preload.is_empty() {
            a.push("--setenv".into());
            a.push("LD_PRELOAD".into());
            a.push(preload.join(" ").into());
        }
        if let Some(img) = &self.image {
            // A boot image is a build input: the guest may read it, never
            // rewrite it (S4's canary test). Staging (`build-image`) keeps the
            // writable bind, because that is where dex2oat writes it.
            a.push(if run_root.is_some() {
                "--ro-bind".into()
            } else {
                "--bind".into()
            });
            a.push(img.clone().into());
            a.push("/data/img".into());
        }
        a.push("--".into());
        a.push(program.into());
        for arg in argv {
            a.push(arg.as_str().into());
        }
        if std::env::var_os("ACOS_ART_DEBUG_ARGV").is_some() {
            eprintln!(
                "bwrap {}",
                a.iter()
                    .map(|x| x.to_string_lossy().to_string())
                    .collect::<Vec<_>>()
                    .join(" ")
            );
        }
        if spike_net::enabled() {
            match spike_net::arm() {
                // SPIKE-NET (base arm): run bwrap *inside* the netns pasta
                // configured for it.
                Ok(acos_netd::client::Arm::Base) => return Ok(spike_net::wrap(&a)),
                // NET2 (relay arm): the guest runs in the session netns the relay
                // supervisor creates and joins to the shared relay. Host-side
                // build steps (`staging`) stay out of the relay: they need no
                // network, and every attach costs a netns + veth.
                Ok(acos_netd::client::Arm::Relay) => {
                    if self.active_run_root().is_some() {
                        // NET4: a relay session is *named by the session id* and
                        // nothing else. A session without one could not be
                        // addressed by `acos_net`, so this fails closed instead of
                        // inventing a name no client can use.
                        let Some(id) = self.net_session.as_deref() else {
                            return Err(Error::Layout(format!(
                                "the relay arm needs a session id: set {} (the engine \
                                 publishes the id it addresses the session by; a relay \
                                 session without one is unreachable by `acos_net`)",
                                spike_net::SESSION_ID_ENV
                            )));
                        };
                        return Ok(spike_net::relay_wrap(&a, id));
                    }
                }
                Err(e) => return Err(Error::Layout(format!("{e}"))),
            }
        }
        b.args(a);
        Ok(b)
    }
}

// ---------------------------------------------------------------------------
// W39: launch-wall stage markers
// ---------------------------------------------------------------------------

/// Print a stage marker for the launch/verify wall when `ACOS_ART_STAGE_TIMING`
/// is set. Every marker carries the milliseconds since this process was
/// `exec`'d (from `/proc/uptime` and this process's `starttime`), so a stage
/// table is built from adjacent markers with no timer plumbed through call
/// sites and no cost when the variable is absent.
///
/// ```text
/// acos-art: stage linkerconfig exec=418ms
/// ```
pub fn stage(name: &str) {
    if std::env::var_os("ACOS_ART_STAGE_TIMING").is_none() {
        return;
    }
    eprintln!("acos-art: stage {name} exec={}ms", exec_elapsed_ms());
}

/// Milliseconds since this process started (`CLOCK_BOOTTIME` minus the process
/// start tick); 10 ms granularity, which is well below every stage measured.
fn exec_elapsed_ms() -> u128 {
    let uptime = fs::read_to_string("/proc/uptime")
        .ok()
        .and_then(|s| {
            s.split_whitespace()
                .next()
                .and_then(|v| v.parse::<f64>().ok())
        })
        .unwrap_or(0.0);
    let start_ticks = fs::read_to_string("/proc/self/stat")
        .ok()
        .and_then(|s| s.rfind(')').map(|i| s[i + 2..].to_string()))
        .and_then(|rest| {
            rest.split_whitespace()
                .nth(19)
                .and_then(|v| v.parse::<u64>().ok())
        })
        .unwrap_or(0);
    ((uptime * 1000.0) as u128).saturating_sub((start_ticks as u128) * 10)
}

/// The `ACOS_HOME` install root, when set and non-empty (REL1a, docs/INSTALL.md).
///
/// It is the root every runtime path is relative to in an installed layout: the
/// same relative layout as the checkout (`.cache/art/sysroot`,
/// `.cache/boot/android-34`, `crates/acos-art/guest/java`, …), so installing is
/// a copy of those trees, not a second code path. Unset means "a checkout" and
/// every path keeps resolving exactly as before.
pub fn acos_home() -> Option<PathBuf> {
    let raw = std::env::var_os("ACOS_HOME")?;
    let path = PathBuf::from(raw);
    if path.as_os_str().is_empty() {
        return None;
    }
    Some(path)
}

/// The developer checkout, ignoring `ACOS_HOME`: `$ACOS_ROOT`, else the nearest
/// ancestor of the cwd or the executable that contains `crates/` + `Cargo.toml`.
/// Only read-only fallbacks use this (the compiled-in guest sources, the
/// fixture corpora), never writable state.
pub fn dev_root() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("ACOS_ROOT") {
        let p = PathBuf::from(p);
        if p.join("Cargo.toml").is_file() {
            return Some(p);
        }
    }
    let mut cands: Vec<PathBuf> = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        cands.extend(exe.ancestors().map(Path::to_path_buf));
    }
    if let Ok(cwd) = std::env::current_dir() {
        cands.extend(cwd.ancestors().map(Path::to_path_buf));
    }
    for c in cands {
        if c.join("Cargo.toml").is_file() && c.join("crates").is_dir() {
            return Some(c);
        }
    }
    None
}

/// The root every runtime path is joined onto: `$ACOS_HOME` when set (an
/// installed layout), else the developer checkout.
pub fn repo_root() -> Result<PathBuf> {
    repo_root_with(acos_home())
}

/// [`repo_root`] with the install root passed in, so tests can prove both arms
/// without mutating the (process-global) environment.
pub fn repo_root_with(home: Option<PathBuf>) -> Result<PathBuf> {
    if let Some(home) = home {
        return Ok(home);
    }
    dev_root().ok_or(Error::NoRepoRoot)
}

/// The checkout the sources were compiled from: the compile-time fallback of
/// [`asset_path`].
fn compiled_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")))
}

/// A read-only file that ships with the tree (guest Java sources, fixture
/// corpora): `$ACOS_HOME/<rel>` when it exists, else the checkout's copy, else
/// the path compiled into this binary.
pub fn asset_path(rel: &str) -> PathBuf {
    asset_path_with(acos_home().as_deref(), rel)
}

/// [`asset_path`] with the install root passed in (testable).
pub fn asset_path_with(home: Option<&Path>, rel: &str) -> PathBuf {
    if let Some(home) = home {
        let candidate = home.join(rel);
        if candidate.exists() {
            return candidate;
        }
    }
    if let Some(dev) = dev_root() {
        let candidate = dev.join(rel);
        if candidate.exists() {
            return candidate;
        }
    }
    compiled_root().join(rel)
}

/// Minimal protobuf wire walk: `(field, wire_type, value)` where a
/// length-delimited value is the payload, and a varint is 8-byte LE (so the
/// caller can `u64::from_le_bytes`). Enough for `apex_manifest.pb` and the
/// `*.pb` classpath fragments; no schema, no allocation-heavy runtime.
fn proto_fields(bytes: &[u8]) -> Vec<(u64, u8, Vec<u8>)> {
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < bytes.len() {
        let Some((key, next)) = read_varint(bytes, i) else {
            break;
        };
        i = next;
        let field = key >> 3;
        let wire = (key & 7) as u8;
        match wire {
            0 => {
                let Some((v, next)) = read_varint(bytes, i) else {
                    break;
                };
                i = next;
                out.push((field, wire, v.to_le_bytes().to_vec()));
            }
            1 => {
                if i + 8 > bytes.len() {
                    break;
                }
                out.push((field, wire, bytes[i..i + 8].to_vec()));
                i += 8;
            }
            2 => {
                let Some((len, next)) = read_varint(bytes, i) else {
                    break;
                };
                let end = next.saturating_add(len as usize);
                if end > bytes.len() {
                    break;
                }
                out.push((field, wire, bytes[next..end].to_vec()));
                i = end;
            }
            5 => {
                if i + 4 > bytes.len() {
                    break;
                }
                out.push((field, wire, bytes[i..i + 4].to_vec()));
                i += 4;
            }
            _ => break,
        }
    }
    out
}

fn read_varint(bytes: &[u8], mut i: usize) -> Option<(u64, usize)> {
    let mut value = 0u64;
    let mut shift = 0u32;
    loop {
        let b = *bytes.get(i)?;
        i += 1;
        value |= u64::from(b & 0x7f) << shift;
        if b & 0x80 == 0 {
            return Some((value, i));
        }
        shift += 7;
        if shift >= 64 {
            return None;
        }
    }
}

/// Device mount name of an APEX (`apex_manifest.pb` field 1, a string), which
/// is what apexd mounts under `/apex/<name>`.
fn apex_mount_name(dir: &Path, fallback: &str) -> String {
    let Ok(bytes) = fs::read(dir.join("apex_manifest.pb")) else {
        return fallback.to_string();
    };
    for (field, wire, value) in proto_fields(&bytes) {
        if field == 1 && wire == 2 {
            if let Ok(name) = std::str::from_utf8(&value) {
                if !name.is_empty() {
                    return name.to_string();
                }
            }
        }
    }
    fallback.to_string()
}

fn read_modules(sysroot: &Path) -> Result<Vec<ApexModule>> {
    // Some extraction passes recorded a module dir with stray whitespace
    // (" com.android.configinfrastructure"); accept the trimmed match.
    let mut dir_names: Vec<String> = fs::read_dir(sysroot.join("system/apex"))?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();
    dir_names.sort();
    let real_dir = |name: &str| -> Option<String> {
        dir_names
            .iter()
            .find(|d| d.as_str() == name)
            .or_else(|| dir_names.iter().find(|d| d.trim() == name))
            .cloned()
    };
    let list = sysroot.join("APEX_MODULES.txt");
    if let Ok(text) = fs::read_to_string(&list) {
        let mut out = Vec::new();
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let mut it = line.split_whitespace();
            let name = it.next().unwrap_or("").to_string();
            let file = it.next().unwrap_or(&name).to_string();
            let Some(dir) = real_dir(&name) else { continue };
            let mount = apex_mount_name(&sysroot.join("system/apex").join(&dir), &dir);
            out.push(ApexModule {
                name: dir,
                file,
                mount,
            });
        }
        if !out.is_empty() {
            return Ok(out);
        }
    }
    // Fallback: derive the module list from the extracted tree, applying the
    // apex-file-vs-module-name mapping for the Google-signed modules.
    let dir = sysroot.join("system/apex");
    let mut out = Vec::new();
    let mut names: Vec<_> = fs::read_dir(&dir)?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();
    names.sort();
    for name in names {
        let file = match name.as_str() {
            "com.android.art" => "com.google.android.art.apex".to_string(),
            "com.android.os.statsd" => "com.google.android.os.statsd.apex".to_string(),
            "com.android.tzdata" => "com.google.android.tzdata5.apex".to_string(),
            "com.android.vndk.v34" => "com.google.android.vndk.current.apex".to_string(),
            _ => format!("{name}.apex"),
        };
        let mount = apex_mount_name(&sysroot.join("system/apex").join(&name), &name);
        out.push(ApexModule { name, file, mount });
    }
    if out.is_empty() {
        return Err(Error::Layout(format!(
            "no APEX payloads under {}",
            dir.display()
        )));
    }
    Ok(out)
}

/// Compile the freestanding shim with clang **once** into the shared
/// content-addressed build cache (`build_cache`), then install it into
/// `dest_dir`. Every launch reuses the cached `.so`; the key covers the source
/// bytes and the compiler identity, so a toolchain change invalidates it.
pub fn build_shim(dest_dir: &Path) -> Result<PathBuf> {
    let clang = std::env::var("CC").unwrap_or_else(|_| "clang".to_string());
    let key = crate::build_cache::digest(&[
        b"acos-art/shim/v2",
        SHIM_SOURCE.as_bytes(),
        crate::build_cache::file_id(Path::new(&clang)).as_bytes(),
    ]);
    let built = crate::build_cache::ensure(&key, &["libartshim.so"], |stage| {
        let src = stage.join("artshim.c");
        fs::write(&src, SHIM_SOURCE)?;
        let out = stage.join("libartshim.so");
        let st = Command::new(&clang)
            .args([
                "-nostdlib",
                "-ffreestanding",
                "-fno-builtin",
                "-fPIC",
                "-shared",
                "-O2",
                "-Wall",
                "-o",
            ])
            .arg(&out)
            .arg(&src)
            .status()
            .map_err(|e| Error::Tool(format!("running {clang}: {e}")))?;
        if !st.success() {
            return Err(Error::Tool(format!(
                "{clang} failed to build {}",
                src.display()
            )));
        }
        fs::remove_file(&src)?;
        Ok(())
    })?;
    let installed = crate::build_cache::install(&built, &["libartshim.so"], dest_dir)?;
    Ok(installed[0].clone())
}

pub(crate) fn simple_hash(s: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100_0000_01b3);
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---------------------------------------------------------------- REL1a
    // Path resolution. ACOS_HOME wins over any checkout; unset keeps today's
    // behaviour byte-identically; the guest sources fall back home -> repo ->
    // compile-time.

    /// A throw-away install root under the checkout's `.cache/`.
    fn fake_home(tag: &str) -> PathBuf {
        let dir = scratch_dir(&format!("home-{tag}"));
        dir
    }

    /// FLAKEFIX: a sysroot that has the same bytes but another path (a
    /// worktree reaching the shared image through its own `.cache/art`) must
    /// hit the same derived-file cache entries; different bytes must not.
    fn key_sysroot(tag: &str, linkerconfig: &[u8]) -> ArtRoot {
        let dir = scratch_dir(&format!("keyroot-{tag}"));
        let sysroot = dir.join("sysroot");
        let rt = sysroot.join("system/apex/com.android.runtime/bin");
        let sdk = sysroot.join("system/apex/com.android.sdkext/bin");
        fs::create_dir_all(&rt).unwrap();
        fs::create_dir_all(&sdk).unwrap();
        fs::write(rt.join("linkerconfig"), linkerconfig).unwrap();
        fs::write(sdk.join("derive_classpath"), b"derive").unwrap();
        fs::write(sysroot.join("READY"), b"ready").unwrap();
        fs::write(sysroot.join("MANIFEST.txt"), b"manifest").unwrap();
        ArtRoot {
            repo_root: dir.clone(),
            sysroot,
            data: dir.join("data"),
            modules: Vec::new(),
        }
    }

    #[test]
    fn the_derived_file_cache_keys_do_not_name_the_sysroot_path() {
        let a = key_sysroot("a", b"linkerconfig-bytes");
        let b = key_sysroot("b-another-path", b"linkerconfig-bytes");
        assert_ne!(a.sysroot, b.sysroot);
        // The mtimes differ too (b was written later): content is the identity.
        let stamp = |r: &ArtRoot| r.linkerconfig_stamp();
        assert_eq!(
            a.linkerconfig_cache_key(&stamp(&a)),
            b.linkerconfig_cache_key(&stamp(&b)),
            "linkerconfig key must be path-independent"
        );
        assert_eq!(
            a.classpath_env_cache_key(),
            b.classpath_env_cache_key(),
            "derive_classpath key must be path-independent"
        );
        let c = key_sysroot("c", b"a different linkerconfig");
        assert_ne!(
            a.linkerconfig_cache_key(&stamp(&a)),
            c.linkerconfig_cache_key(&stamp(&c)),
            "a different linkerconfig binary is a different key"
        );
    }

    #[test]
    fn a_strict_mode_stamp_from_an_older_build_is_not_reused() {
        let a = key_sysroot("stamp", b"x");
        let old = a.mount_stamp();
        assert_ne!(old, a.linkerconfig_stamp());
        assert!(a.linkerconfig_stamp().starts_with(&old));
    }

    #[test]
    fn acos_home_overrides_every_root_lookup() {
        let home = fake_home("override");
        let resolved = repo_root_with(Some(home.clone())).expect("ACOS_HOME resolves");
        assert_eq!(resolved, home);
        // The arm is exercised from inside the checkout (the test binary lives
        // there), so this also proves ACOS_HOME beats a discovered repo root.
        assert!(dev_root().is_some(), "the test runs inside the checkout");
    }

    #[test]
    fn unset_home_resolves_to_the_checkout_unchanged() {
        let resolved = repo_root_with(None).expect("a checkout resolves");
        assert!(
            resolved.join("Cargo.toml").is_file(),
            "{}",
            resolved.display()
        );
        assert!(resolved.join("crates").is_dir(), "{}", resolved.display());
        // Today's asset paths, relative to that root.
        assert!(
            resolved.join(".cache/boot/android-34").is_dir(),
            "the dev layout's boot cache is where it always was"
        );
    }

    #[test]
    fn guest_asset_resolves_home_then_checkout_then_compiled_path() {
        let rel = "crates/acos-art/guest/java/dev/acos/art/AppMain.java";
        // 1. Install root wins when it carries the file.
        let home = fake_home("asset");
        let installed = home.join(rel);
        fs::create_dir_all(installed.parent().unwrap()).unwrap();
        fs::write(&installed, b"class AppMain {}\n").unwrap();
        assert_eq!(asset_path_with(Some(&home), rel), installed);
        // 2. An install root without it (or no root at all) falls back to the
        //    checkout, which is where the sources really live.
        let empty = fake_home("asset-empty");
        let from_empty = asset_path_with(Some(&empty), rel);
        assert!(from_empty.is_file(), "{}", from_empty.display());
        assert!(!from_empty.starts_with(&empty));
        let from_unset = asset_path_with(None, rel);
        assert!(from_unset.is_file(), "{}", from_unset.display());
        // 3. Neither has it: the compiled-in path is returned so the caller can
        //    name it in the error instead of silently picking something else.
        let missing = "crates/acos-art/guest/java/does-not-exist";
        let wanted = asset_path_with(Some(&empty), missing);
        assert_eq!(wanted, compiled_root().join(missing));
        assert!(!wanted.exists());
        let _ = fs::remove_dir_all(&home);
        let _ = fs::remove_dir_all(&empty);
    }

    #[test]
    fn compiled_root_is_the_checkout() {
        let root = compiled_root();
        assert!(
            root.join("crates/acos-art/guest/java").is_dir(),
            "{}",
            root.display()
        );
    }

    /// Tiny scratch dir under the repo `.cache/` (never `/tmp`: the directives
    /// keep ART scratch on the repo volume).
    fn scratch_dir(tag: &str) -> PathBuf {
        // Short on purpose: a run root's socket paths (`<run>/socket/logd`,
        // `<run>/wv/webview.sock`) live under `sun_path`'s 108-byte limit.
        let base = match repo_root() {
            Ok(r) => r.join(".cache/t"),
            Err(_) => std::env::temp_dir().join("acos-t"),
        };
        let dir = base.join(format!("{}-{tag}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// R3 retention policy: only a signal the guest raised itself *and* a parsed
    /// crash record keep a tree. Host-sent teardown signals never do — keeping
    /// them would leak one run tree (extracted native libs included) per normal
    /// destroy/reset, which is what the N=10 loop does thousands of times.
    #[test]
    fn only_a_guest_crash_with_evidence_keeps_the_run_tree() {
        for host_signal in [libc::SIGKILL, libc::SIGTERM, libc::SIGHUP, libc::SIGINT] {
            assert!(
                !keeps_run_tree_for_crash(Some(host_signal), true),
                "signal {host_signal} is sent by the host, not raised by the guest"
            );
            assert!(!keeps_run_tree_for_crash(Some(host_signal), false));
        }
        for guest_signal in [
            libc::SIGSEGV,
            libc::SIGBUS,
            libc::SIGILL,
            libc::SIGFPE,
            libc::SIGABRT,
            libc::SIGTRAP,
            libc::SIGSYS,
        ] {
            assert!(
                keeps_run_tree_for_crash(Some(guest_signal), true),
                "signal {guest_signal} is a guest crash with evidence"
            );
            // A crash signal whose record could not be parsed has nothing to
            // debug, so it must not pin a tree either.
            assert!(!keeps_run_tree_for_crash(Some(guest_signal), false));
        }
        assert!(!keeps_run_tree_for_crash(None, true));
    }

    /// G3d: the guest gets libc's sampling allocator disabled unless a caller
    /// asks for it, and an explicit host value is passed through untouched.
    #[test]
    fn gwp_asan_is_off_for_guests_unless_asked() {
        let none = |_: &str| None;
        assert_eq!(
            gwp_asan_guest_env(none, None),
            vec![("GWP_ASAN_PROCESS_SAMPLING".to_string(), "0".to_string())],
            "default: deterministic guest, no sampling allocator"
        );
        assert_eq!(
            gwp_asan_guest_env(none, Some("off")),
            vec![("GWP_ASAN_PROCESS_SAMPLING".to_string(), "0".to_string())]
        );
        assert_eq!(
            gwp_asan_guest_env(none, Some("on")),
            vec![("GWP_ASAN_PROCESS_SAMPLING".to_string(), "1".to_string())],
            "ACOS_ART_GWP_ASAN=on restores libc's own sampling for one run"
        );
        // An explicit host knob wins and is forwarded verbatim (A/B runs).
        let host = |k: &str| (k == "GWP_ASAN_PROCESS_SAMPLING").then(|| "1".to_string());
        assert_eq!(
            gwp_asan_guest_env(host, Some("off")),
            vec![("GWP_ASAN_PROCESS_SAMPLING".to_string(), "1".to_string())]
        );
        let rate = |k: &str| (k == "GWP_ASAN_SAMPLE_RATE").then(|| "1".to_string());
        assert_eq!(
            gwp_asan_guest_env(rate, None),
            vec![("GWP_ASAN_SAMPLE_RATE".to_string(), "1".to_string())]
        );
        // `libc` is the pre-G3d baseline: forward nothing, let libc decide.
        assert!(gwp_asan_guest_env(none, Some("libc")).is_empty());
    }

    /// R3 retention cap: the marker identifies *kept crash* trees, the oldest are
    /// pruned first, and a tree without the marker is never touched.
    #[test]
    fn kept_crash_trees_are_pruned_oldest_first_and_unmarked_trees_survive() {
        let root = scratch_dir("crash-cap").join("runs");
        fs::create_dir_all(&root).unwrap();
        // 5 kept crash trees, oldest first, distinguished by marker mtime.
        let mut kept = Vec::new();
        for i in 0..5u64 {
            let dir = root.join(format!("run-{i}"));
            fs::create_dir_all(dir.join("out")).unwrap();
            fs::write(dir.join("out/launch.json"), "{}").unwrap();
            let marker = dir.join(CRASH_KEEP_MARKER);
            fs::write(&marker, "{\"route\":\"launch\"}").unwrap();
            // mtime is what retention orders by; force it so the test is not
            // dependent on filesystem timestamp granularity.
            let f = fs::OpenOptions::new().write(true).open(&marker).unwrap();
            f.set_modified(
                std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1000 + i),
            )
            .unwrap();
            kept.push(dir);
        }
        // An explicit `ACOS_ART_KEEP_RUN` tree (no marker) must never be pruned.
        let explicit = root.join("run-explicit");
        fs::create_dir_all(explicit.join("out")).unwrap();

        let pruned = prune_kept_crash_trees(&root, 2);
        assert_eq!(pruned.len(), 3, "pruned {pruned:?}");
        assert_eq!(
            pruned,
            kept[0..3].to_vec(),
            "the three *oldest* kept crash trees are the ones removed"
        );
        assert!(
            kept[3].is_dir() && kept[4].is_dir(),
            "the two newest survive"
        );
        assert!(
            explicit.is_dir(),
            "a non-crash (explicit --keep) tree survives"
        );
        assert_eq!(kept_crash_trees(&root).len(), 2);

        // `ACOS_ART_KEEP_CRASH_MAX=0` means keep none.
        let pruned = prune_kept_crash_trees(&root, 0);
        assert_eq!(pruned.len(), 2);
        assert!(kept_crash_trees(&root).is_empty());
        assert!(explicit.is_dir(), "still untouched");
        let _ = fs::remove_dir_all(root.parent().unwrap());
    }

    /// S4: in a run, every shared input is `--ro-bind` and every writable path
    /// comes from the per-run root; the shared `<data>` tree is never bound.
    #[test]
    fn run_root_makes_shared_inputs_read_only() {
        let _env = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let data = scratch_dir("run-root");
        for d in [
            "system/environ",
            "app",
            "overlay",
            "lib",
            "shim",
            "linkerconfig",
            "img/x86_64",
        ] {
            fs::create_dir_all(data.join(d)).unwrap();
        }
        fs::write(
            data.join("system/environ/classpath"),
            "export BOOTCLASSPATH /apex/a.jar:/system/framework/framework.jar\n\
             export DEX2OATBOOTCLASSPATH /apex/a.jar\n",
        )
        .unwrap();
        for f in [
            "acos.properties",
            "apex-info-list.xml",
            "shim/libartshim.so",
        ] {
            fs::write(data.join(f), "x").unwrap();
        }
        fs::write(data.join("img/x86_64/boot.art"), "x").unwrap();
        let root = ArtRoot {
            repo_root: data.clone(),
            sysroot: data.join("sysroot"),
            data: data.clone(),
            modules: Vec::new(),
        };
        let run = root.new_run_dir("test").expect("run dir");

        let mut sb = root.sandbox();
        sb.run_root(run.path());
        sb.with_image(Some(data.join("img")));
        let argv: Vec<String> = sb
            .command("/system/bin/prog", &[])
            .expect("argv")
            .get_args()
            .map(|a| a.to_string_lossy().to_string())
            .collect();

        // 1. Simplest statement of the whole change: the shared tree is gone.
        let data_s = data.display().to_string();
        for (i, a) in argv.iter().enumerate() {
            if a == "--bind" {
                let src = &argv[i + 1];
                assert!(
                    !src.trim_end_matches('/').eq(&data_s),
                    "the whole shared <data> tree must not be bound rw in a run: {argv:?}"
                );
            }
        }
        // 2. Each shared input is read-only, including the boot image.
        for sub in ["system", "app", "overlay", "lib", "shim", "img"] {
            let src = data.join(sub).display().to_string();
            let dst = format!("/data/{sub}");
            assert!(
                has_pair(&argv, "--ro-bind", &src, &dst),
                "{sub} must be --ro-bind in a run ({src} -> {dst}): {argv:?}"
            );
        }
        assert!(
            has_pair(
                &argv,
                "--ro-bind",
                &data.join("linkerconfig").display().to_string(),
                "/linkerconfig"
            ),
            "linkerconfig must be read-only in a run"
        );
        for f in ["acos.properties", "apex-info-list.xml"] {
            assert!(
                has_pair(
                    &argv,
                    "--ro-bind",
                    &data.join(f).display().to_string(),
                    &format!("/data/{f}")
                ),
                "{f} must be read-only in a run"
            );
        }
        // 3. The writable paths are this run's own root.
        let run_s = run.path().display().to_string();
        for (sub, guest) in [
            ("dalvik-cache", "/data/dalvik-cache"),
            ("misc", "/data/misc"),
            ("tmp", "/data/tmp"),
            ("out", "/data/out"),
        ] {
            assert!(
                has_pair(&argv, "--bind", &format!("{run_s}/{sub}"), guest),
                "{guest} must be the run root's {sub} ({run_s}): {argv:?}"
            );
        }
        // 4. Staging keeps the old, fully writable layout (host build steps only).
        let mut sb = root.sandbox();
        sb.run_root(run.path());
        sb.staging(true);
        let staged: Vec<String> = sb
            .command("/system/bin/prog", &[])
            .expect("argv")
            .get_args()
            .map(|a| a.to_string_lossy().to_string())
            .collect();
        assert!(
            staged
                .iter()
                .enumerate()
                .any(|(i, a)| a == "--bind" && staged[i + 1].trim_end_matches('/') == data_s),
            "staging mode must still bind the shared tree rw: {staged:?}"
        );
        assert!(
            !staged.iter().any(|a| a.contains("/runs/")),
            "staging mode must not use a per-run root: {staged:?}"
        );
        drop(run);
        assert!(
            !data
                .join("runs")
                .read_dir()
                .map(|mut d| d.next().is_some())
                .unwrap_or(false),
            "RunDir::drop must remove the tree"
        );
        let _ = fs::remove_dir_all(&data);
    }

    /// WV4: every run maps this session's WebView socket directory at
    /// `/run/acos` (read-only), and staging — a host-side build step with no
    /// guest — does not. That mount is the whole guest path ⇄ host path
    /// mapping: the image's `LocalSocketImpl` connects to the literal
    /// `/run/acos/webview.sock`, and the broker listens at
    /// `<run>/wv/webview.sock` (or `$ACOS_WEBVIEW_SOCKET_DIR/webview.sock`).
    #[test]
    fn run_root_maps_the_webview_session_socket() {
        let _env = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let data = scratch_dir("webview-socket");
        for d in [
            "system/environ",
            "app",
            "overlay",
            "lib",
            "shim",
            "linkerconfig",
        ] {
            fs::create_dir_all(data.join(d)).unwrap();
        }
        fs::write(
            data.join("system/environ/classpath"),
            "export BOOTCLASSPATH /apex/a.jar:/system/framework/framework.jar\n\
             export DEX2OATBOOTCLASSPATH /apex/a.jar\n",
        )
        .unwrap();
        let root = ArtRoot {
            repo_root: data.clone(),
            sysroot: data.join("sysroot"),
            data: data.clone(),
            modules: Vec::new(),
        };
        let run = root.new_run_dir("wv").expect("run dir");

        let mut sb = root.sandbox();
        sb.run_root(run.path());
        let argv: Vec<String> = sb
            .command("/system/bin/prog", &[])
            .expect("argv")
            .get_args()
            .map(|a| a.to_string_lossy().to_string())
            .collect();

        // WV5b: the default mapping is the short runtime directory, not
        // `<run>/wv` (see `webview_socket::default_session_dir`).
        let dir = crate::webview_socket::host_dir(run.path()).expect("webview dir");
        assert!(
            has_pair(
                &argv,
                "--ro-bind",
                &dir.display().to_string(),
                crate::webview_socket::GUEST_DIR
            ),
            "the session socket dir must be mounted at {}: {argv:?}",
            crate::webview_socket::GUEST_DIR
        );
        // Read-only: the guest may connect, never unlink the broker's listener.
        assert!(
            !has_pair(
                &argv,
                "--bind",
                &dir.display().to_string(),
                crate::webview_socket::GUEST_DIR
            ),
            "the socket dir must not be writable by the guest: {argv:?}"
        );
        assert!(
            dir.is_dir(),
            "the mount source must exist before the broker"
        );
        // (Host path length is `check_sun_path`'s job at launch time: a unit
        // test's scratch root is deeper than a real run's, so asserting the
        // 108-byte budget here would test the test's own path, not the code.)

        // Staging mode is a host build step: no guest, no socket mount.
        let mut sb = root.sandbox();
        sb.run_root(run.path());
        sb.staging(true);
        let staged: Vec<String> = sb
            .command("/system/bin/prog", &[])
            .expect("argv")
            .get_args()
            .map(|a| a.to_string_lossy().to_string())
            .collect();
        assert!(
            !staged.iter().any(|a| a == crate::webview_socket::GUEST_DIR),
            "staging mode must not mount the guest socket dir: {staged:?}"
        );
        let _ = fs::remove_dir_all(&data);
    }

    /// PROCUID: every guest process carries **its package's** uid (AOSP allocates
    /// one uid per installed package; processes of one package share it, two
    /// packages' processes differ). The sandbox maps the guest's own uid onto the
    /// host caller's, so two processes of one session can carry two different
    /// application uids without any host privilege.
    #[test]
    fn sandbox_runs_a_process_as_the_uid_its_package_owns() {
        let _env = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let data = scratch_dir("uid");
        fs::create_dir_all(data.join("system/environ")).unwrap();
        fs::write(
            data.join("system/environ/classpath"),
            "export BOOTCLASSPATH /apex/a.jar:/system/framework/framework.jar\n\
             export DEX2OATBOOTCLASSPATH /apex/a.jar\n",
        )
        .unwrap();
        let root = ArtRoot {
            repo_root: data.clone(),
            sysroot: data.join("sysroot"),
            data: data.clone(),
            modules: Vec::new(),
        };
        // The session default (no `app_uid`) is the PMS fixture uid, which is
        // what every single-process caller keeps.
        let run = root.new_run_dir("uid-default").expect("run dir");
        let mut sb = root.sandbox();
        sb.run_root(run.path());
        let argv: Vec<String> = sb
            .command("/system/bin/prog", &[])
            .expect("argv")
            .get_args()
            .map(|a| a.to_string_lossy().to_string())
            .collect();
        let sep = argv.iter().position(|a| a == "--uid").expect("--uid");
        assert_eq!(argv[sep - 1], "--unshare-user", "a uid needs its namespace");
        assert_eq!(
            argv[sep + 1],
            acos_services::pms::FIXTURE_UID.to_string(),
            "the default is the PMS fixture uid: {argv:?}"
        );
        // The process's primary gid is its uid, exactly like a device app
        // (`Process.myGid()` == `Process.myUid()`).
        let gsep = argv.iter().position(|a| a == "--gid").expect("--gid");
        assert_eq!(argv[gsep + 1], argv[sep + 1], "uid and gid are one id");
        // Two processes, two packages' uids: the second package's process runs as
        // the uid the package manager allocated for it.
        let second = 10_193u32;
        let run2 = root.new_run_dir("uid-second").expect("run dir");
        let mut sb2 = root.sandbox();
        sb2.run_root(run2.path());
        sb2.app_uid(second);
        let argv2: Vec<String> = sb2
            .command("/system/bin/prog", &[])
            .expect("argv")
            .get_args()
            .map(|a| a.to_string_lossy().to_string())
            .collect();
        let sep2 = argv2.iter().position(|a| a == "--uid").expect("--uid");
        assert_eq!(argv2[sep2 + 1], second.to_string(), "argv: {argv2:?}");
        assert_eq!(
            argv2[sep2 + 3],
            second.to_string(),
            "the second process's gid is its own uid: {argv2:?}"
        );
        assert_ne!(argv[sep + 1], argv2[sep2 + 1], "two apps, two uids");
        // A host-side staging step keeps the caller's uid (it is not an app).
        let mut sb3 = root.sandbox();
        sb3.staging(true);
        let argv3: Vec<String> = sb3
            .command("/system/bin/prog", &[])
            .expect("argv")
            .get_args()
            .map(|a| a.to_string_lossy().to_string())
            .collect();
        assert!(
            !argv3.iter().any(|a| a == "--uid"),
            "staging is not an app process: {argv3:?}"
        );
        assert!(
            !argv3.iter().any(|a| a == "--gid"),
            "staging is not an app process: {argv3:?}"
        );
        let _ = fs::remove_dir_all(&data);
    }

    /// PROCUID: the explicit `ACOS_ART_GUEST_UID` seam takes **any** application
    /// uid, because in a multi-process session the uid a caller wants to pin is a
    /// package's uid (10193 for the second package), not only the PMS fixture
    /// uid. It still refuses everything that is not an application uid: host uid
    /// 1000 is `SYSTEM_UID`, which the framework takes privileged branches on.
    #[test]
    fn the_guest_uid_seam_takes_an_application_uid_and_nothing_else() {
        assert!(
            matches!(parse_guest_uid_override(None), Ok(None)),
            "no override = the per-process uid"
        );
        assert_eq!(parse_guest_uid_override(Some("host")).unwrap(), Some(None));
        assert_eq!(
            parse_guest_uid_override(Some("10193")).unwrap(),
            Some(Some(10_193)),
            "a second package's uid is a valid pin"
        );
        assert_eq!(
            parse_guest_uid_override(Some(" 10000 ")).unwrap(),
            Some(Some(10_000)),
            "the first application uid"
        );
        assert_eq!(
            parse_guest_uid_override(Some("19999")).unwrap(),
            Some(Some(19_999)),
            "the last application uid"
        );
        for raw in ["1000", "0", "9999", "20000", "sudo", "10192.5"] {
            assert!(
                matches!(
                    parse_guest_uid_override(Some(raw)),
                    Err(Error::InvalidGuestUid(_))
                ),
                "{raw} must be refused (never a silent fallback)"
            );
        }
    }

    /// `flag src dst` appears as consecutive argv elements.
    fn has_pair(argv: &[String], flag: &str, src: &str, dst: &str) -> bool {
        argv.iter().enumerate().any(|(i, a)| {
            a == flag
                && argv.get(i + 1).map(String::as_str) == Some(src)
                && argv.get(i + 2).map(String::as_str) == Some(dst)
        })
    }

    /// The argv `Sandbox::command` builds: hardening flags are the prefix (so
    /// they precede `--tmpfs /` and every bind), the guest environment is the
    /// `--setenv` list and nothing else, and the debug/exec tail is intact.
    #[test]
    fn sandbox_argv_hardens_before_first_mount() {
        let _env = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let data = scratch_dir("argv");
        fs::create_dir_all(data.join("system/environ")).unwrap();
        fs::write(
            data.join("system/environ/classpath"),
            "export BOOTCLASSPATH /apex/a.jar:/system/framework/framework.jar\n\
             export DEX2OATBOOTCLASSPATH /apex/a.jar\n",
        )
        .unwrap();
        let root = ArtRoot {
            repo_root: data.clone(),
            sysroot: data.join("sysroot"),
            data: data.clone(),
            modules: Vec::new(),
        };
        let sb = root.sandbox();
        let cmd = sb
            .command("/system/bin/prog", &["one".into(), "two".into()])
            .expect("argv");
        let argv: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().to_string())
            .collect();

        // 1. The exact flag prefix, in order, before anything else.
        assert_eq!(
            &argv[..SANDBOX_FLAGS.len()],
            SANDBOX_FLAGS,
            "argv: {argv:?}"
        );
        // 2. No mount, bind or exec may precede the namespace setup.
        let first_mount = argv.iter().position(|a| a == "--tmpfs").expect("--tmpfs /");
        assert_eq!(
            first_mount,
            SANDBOX_FLAGS.len(),
            "flags must be the argv prefix"
        );
        for flag in [
            "--unshare-pid",
            "--unshare-net",
            "--die-with-parent",
            "--clearenv",
        ] {
            let at = argv.iter().position(|a| a == flag).expect(flag);
            assert!(at < first_mount, "{flag} must precede --tmpfs /");
        }
        // 3. `--unshare-pid` without a fresh /proc would show the host's pids.
        assert!(
            argv.iter().any(|a| a == "--proc"),
            "--unshare-pid needs --proc /proc"
        );
        // 4. `--clearenv` + explicit `--setenv` only: every var is set after the
        //    clear, and the list is the complete set the guest needs.
        let keys: Vec<String> = argv
            .windows(2)
            .filter(|w| w[0] == "--setenv")
            .map(|w| w[1].clone())
            .collect();
        for k in [
            "ANDROID_ROOT",
            "ANDROID_RUNTIME_ROOT",
            "ANDROID_ART_ROOT",
            "ANDROID_I18N_ROOT",
            "ANDROID_TZDATA_ROOT",
            "ANDROID_DATA",
            "ANDROID_STORAGE",
            "ANDROID_TMP",
            "ANDROID_ART_PROPERTIES",
            "BOOTCLASSPATH",
            "DEX2OATBOOTCLASSPATH",
        ] {
            assert!(keys.iter().any(|x| x == k), "missing --setenv {k}");
        }
        let clearenv = argv
            .iter()
            .position(|a| a == "--clearenv")
            .expect("--clearenv");
        let last_setenv = argv
            .iter()
            .rposition(|a| a == "--setenv")
            .expect("--setenv");
        assert!(clearenv < last_setenv, "--setenv after --clearenv");
        // 5. The exec tail (program + its args) still ends the argv.
        let sep = argv.iter().rposition(|a| a == "--").expect("--");
        assert_eq!(&argv[sep + 1..], &["/system/bin/prog", "one", "two"]);
        let _ = fs::remove_dir_all(&data);
    }

    #[test]
    fn classpath_env_parses_device_environ() {
        let text = "export BOOTCLASSPATH /apex/a.jar:/apex/b.jar:/system/framework/c.jar\n\
                    export DEX2OATBOOTCLASSPATH /apex/a.jar:/apex/b.jar\n\
                    export SYSTEMSERVERCLASSPATH /system/framework/services.jar\n";
        let cp = ArtRoot::parse_classpath_env(text).expect("parsed");
        assert_eq!(
            cp.boot,
            vec!["/apex/a.jar", "/apex/b.jar", "/system/framework/c.jar"]
        );
        assert_eq!(cp.dex2oat, vec!["/apex/a.jar", "/apex/b.jar"]);
        // The invariant ART CHECKs: DEX2OATBOOTCLASSPATH is the prefix of
        // BOOTCLASSPATH and something is left over (the mainline framework).
        assert!(cp.boot.len() > cp.dex2oat.len());
        assert_eq!(&cp.boot[..cp.dex2oat.len()], &cp.dex2oat[..]);
    }

    #[test]
    fn classpath_env_rejects_incomplete_files() {
        assert!(ArtRoot::parse_classpath_env("export BOOTCLASSPATH /a.jar\n").is_none());
        assert!(ArtRoot::parse_classpath_env("").is_none());
    }

    #[test]
    fn varint_roundtrip() {
        let mut bytes = Vec::new();
        for v in [0u64, 1, 127, 128, 300, 340818082] {
            let mut x = v;
            loop {
                let mut b = (x & 0x7f) as u8;
                x >>= 7;
                if x != 0 {
                    b |= 0x80;
                }
                bytes.push(b);
                if x == 0 {
                    break;
                }
            }
        }
        let mut i = 0usize;
        for want in [0u64, 1, 127, 128, 300, 340818082] {
            let (got, next) = read_varint(&bytes, i).expect("varint");
            assert_eq!(got, want);
            i = next;
        }
    }

    #[test]
    fn apex_manifest_name_wins_over_dir_name() {
        // apex_manifest.pb = {1: "com.android.configinfrastructure", 2: <varint>}
        let dir = std::env::temp_dir().join(format!("acos-apex-manifest-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let name = b"com.android.configinfrastructure";
        let mut pb = vec![0x0a, name.len() as u8];
        pb.extend_from_slice(name);
        pb.extend_from_slice(&[0x10, 0x80, 0x80, 0x80, 0xa2, 0x01]);
        fs::write(dir.join("apex_manifest.pb"), &pb).unwrap();
        assert_eq!(
            apex_mount_name(&dir, "com.google.android.configinfrastructure"),
            "com.android.configinfrastructure"
        );
        // Absent manifest: keep the host directory name.
        assert_eq!(
            apex_mount_name(&dir.join("nope"), "com.android.art"),
            "com.android.art"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    // -- H17: shared inputs are published, never truncated in place --------- //

    /// H17: a reader that already has the file open keeps the old, complete
    /// inode. `fs::write` (the pre-H17 code) truncates that inode, which is how
    /// a starting guest's Landlock plan appeared empty (`ERR landlock=plan-missing`,
    /// exit 126) while another launch rewrote it.
    #[test]
    fn publish_atomic_never_truncates_a_live_reader() {
        let dir = scratch_dir("publish-reader");
        let path = dir.join("landlock.plan");
        let old = vec![b'A'; 256 * 1024];
        let new = vec![b'B'; 256 * 1024];
        publish_atomic(&path, &old).expect("first publish");
        // The reader opens the file (the guest's guard does exactly this).
        let mut held = fs::File::open(&path).expect("open");
        publish_atomic(&path, &new).expect("second publish");
        let mut seen = Vec::new();
        use std::io::Read;
        held.read_to_end(&mut seen).expect("read the held fd");
        assert_eq!(
            seen, old,
            "the open fd must still see the old file, complete"
        );
        assert_eq!(
            fs::read(&path).expect("path"),
            new,
            "a new open sees the new file"
        );
        // Identical bytes are a no-op (the inode does not churn).
        use std::os::unix::fs::MetadataExt;
        let inode_before = fs::metadata(&path).expect("meta").ino();
        publish_atomic(&path, &new).expect("same publish");
        assert_eq!(fs::metadata(&path).expect("meta").ino(), inode_before);
        // No temp files are left behind.
        let leftovers: Vec<_> = fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "temp files left: {leftovers:?}");
        let _ = fs::remove_dir_all(&dir);
    }

    /// H17: a concurrent reader only ever observes one of the two complete
    /// payloads. With `fs::write` this test fails: a read can land between the
    /// `O_TRUNC` and the write and see 0 or a partial file (the observed
    /// `plan-missing` class, and a `SIGBUS` for an `mmap`ed file).
    #[test]
    fn publish_atomic_is_atomic_for_a_concurrent_reader() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;
        let dir = scratch_dir("publish-race");
        let path = dir.join("acos.properties");
        let a = vec![b'a'; 64 * 1024];
        let b = vec![b'b'; 64 * 1024];
        publish_atomic(&path, &a).expect("seed");
        let stop = Arc::new(AtomicBool::new(false));
        let readers: Vec<_> = (0..2)
            .map(|_| {
                let path = path.clone();
                let stop = Arc::clone(&stop);
                let a = a.clone();
                let b = b.clone();
                std::thread::spawn(move || {
                    let mut reads = 0usize;
                    while !stop.load(Ordering::Relaxed) {
                        let got = fs::read(&path).expect("read");
                        assert!(
                            got == a || got == b,
                            "torn read: {} bytes ({} / {})",
                            got.len(),
                            a.len(),
                            b.len()
                        );
                        reads += 1;
                    }
                    reads
                })
            })
            .collect();
        for i in 0..300 {
            let payload = if i % 2 == 0 { &b } else { &a };
            publish_atomic(&path, payload).expect("publish");
        }
        stop.store(true, Ordering::Relaxed);
        let mut total = 0usize;
        for r in readers {
            total += r.join().expect("reader panicked (a torn read)");
        }
        assert!(total > 0, "the readers never ran");
        let _ = fs::remove_dir_all(&dir);
    }

    /// H17: staging an APK publishes a fresh copy; a guest that still has the
    /// previous copy open (or mapped) is untouched.
    #[test]
    fn stage_apk_leaves_the_previous_copy_intact() {
        use std::io::Read;
        let dir = scratch_dir("stage-apk");
        let from = dir.join("input.apk");
        let to = dir.join("run/app/counter.apk");
        let v1 = vec![b'1'; 128 * 1024];
        let v2 = vec![b'2'; 128 * 1024];
        fs::write(&from, &v1).unwrap();
        crate::app::stage_apk(&from, &to).expect("stage v1");
        let mut held = fs::File::open(&to).expect("open v1");
        fs::write(&from, &v2).unwrap();
        crate::app::stage_apk(&from, &to).expect("stage v2");
        assert_eq!(fs::read(&to).unwrap(), v2);
        let mut seen = Vec::new();
        held.read_to_end(&mut seen).unwrap();
        assert_eq!(seen, v1, "the open previous copy must stay complete");
        let _ = fs::remove_dir_all(&dir);
    }
}
