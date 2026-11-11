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
pub mod capture;
pub mod launch;
pub mod session;
pub mod launcher;
pub mod logd;
pub mod props;
pub mod watchdog;

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

/// Version marker of the synthesised `apex-info-list.xml` (bumped when the
/// generated content changes, so a stale file is rewritten).
const APEX_INFO_MARKER: &str = "<!-- acos-art apex-info-list v3 -->\n";

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("repository root not found (set ACOS_ROOT)")]
    NoRepoRoot,
    #[error(
        "ART sysroot not found: {0} (run tools/art-sysroot.sh --out {0}, or set ACOS_ART_SYSROOT)"
    )]
    NoSysroot(PathBuf),
    #[error("sysroot layout error: {0}")]
    Layout(String),
    #[error("tool failed: {0}")]
    Tool(String),
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
        let prefix_ok =
            self.dex2oat.len() < self.boot.len() && self.boot[..self.dex2oat.len()] == self.dex2oat[..];
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
        Ok(Self { repo_root, sysroot, data, modules })
    }

    // ---- layout -----------------------------------------------------------

    /// Create the writable `/data` skeleton and refresh the derived files
    /// (`apex-info-list.xml`, property table). Idempotent.
    pub fn ensure_data(&self) -> Result<()> {
        for d in [
            "dalvik-cache/x86_64",
            "misc/apexdata/com.android.art/dalvik-cache/x86_64",
            "linkerconfig",
            "socket",
            "system/environ",
            "tmp",
            "img",
            "shim",
        ] {
            fs::create_dir_all(self.data.join(d))?;
        }
        self.write_apex_info_list()?;
        Props::android34().write(&self.data.join(props::PROPS_FILE))?;
        Ok(())
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
        let have_marker = fs::read_to_string(&out).map(|t| t.contains(APEX_INFO_MARKER)).unwrap_or(false);
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
        fs::write(&out, xml)?;
        Ok(())
    }

    fn modules_path(&self) -> PathBuf {
        self.sysroot.join("APEX_MODULES.txt")
    }

    /// Generate `/linkerconfig/ld.config.txt` with the image's own
    /// `linkerconfig` binary (bionic then has the `system` and per-APEX
    /// namespaces `libnativeloader` requires).
    pub fn ensure_linkerconfig(&self, regen: bool) -> Result<()> {
        let ld = self.data.join("linkerconfig/ld.config.txt");
        // linkerconfig enumerates the APEX namespaces from the actual mounts,
        // so a mount-table change (new extraction, corrected apex names)
        // invalidates an existing ld.config.txt.
        let stamp = self.data.join("linkerconfig/.mounts");
        let mounts = self.mount_stamp();
        let stamp_ok = fs::read_to_string(&stamp).map(|s| s.trim() == mounts).unwrap_or(false);
        if ld.is_file() && stamp_ok && !regen {
            return Ok(());
        }
        self.ensure_data()?;
        // linkerconfig is a bionic binary: without the shim its CHECK failures
        // are invisible (liblog has nowhere to write) and it just aborts.
        crate::build_shim(&self.data.join("shim"))?;
        let mut sb = self.sandbox();
        sb.with_shim(true);
        let mut cmd = sb.command("/apex/com.android.runtime/bin/linkerconfig", &[
            "--target".into(),
            "/linkerconfig".into(),
        ])?;
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
        Ok(())
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
            let Some(rest) = line.strip_prefix("export ") else { continue };
            let Some((key, value)) = rest.split_once(char::is_whitespace) else { continue };
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
        if !regen && out.is_file() && self.classpaths().is_some() && self.classpath_env_is_fresh(&out) {
            return Ok(());
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
        let missing: Vec<&String> = cp.boot.iter().filter(|j| !self.host_path(j).is_file()).collect();
        if !missing.is_empty() {
            return Err(Error::Layout(format!(
                "BOOTCLASSPATH names jars the sysroot lacks: {} (re-run tools/art-sysroot.sh)",
                missing.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ")
            )));
        }
        Ok(())
    }

    fn classpath_env_is_fresh(&self, out: &Path) -> bool {
        let newer = |p: &Path| -> bool {
            match (fs::metadata(p).and_then(|m| m.modified()), fs::metadata(out).and_then(|m| m.modified())) {
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
        Ok(boot.split_last().map(|(_, rest)| rest.to_vec()).unwrap_or_default())
    }

    /// Jars the image's `bootclasspath.pb` fragments put in
    /// `DEX2OATBOOTCLASSPATH` (proto `ClasspathEntry{string jar=1; Classpath
    /// classpath=2;}` with `DEX2OATBOOTCLASSPATH = 1`).
    fn dex2oat_fragment_jars(&self) -> Vec<String> {
        let mut files: Vec<PathBuf> = vec![self.sysroot.join("system/etc/classpaths/bootclasspath.pb")];
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
        self.bootclasspath_entries().map(|e| e.1).unwrap_or_default()
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
            self.repo_root.join("research/art-feasibility/a-dalvikvm/bootclasspath.txt"),
            self.repo_root.join(".cache/boot/android-34/BOOTCLASSPATH"),
        ]
        .into_iter()
        .find_map(|p| fs::read_to_string(p).ok())
        .ok_or_else(|| {
            Error::Layout(format!(
                "{} has no BOOTCLASSPATH (re-run tools/art-sysroot.sh)",
                self.sysroot.display()
            ))
        })?;
        for line in list.lines().map(str::trim).filter(|l| !l.is_empty()) {
            let jar = Path::new(line).file_name().unwrap().to_string_lossy().to_string();
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
                return self.sysroot.join("system/apex/com.google.mainline.primary.libs").join(tail);
            }
            // Device path (`/apex/<apex_manifest name>`) or host directory name.
            if let Some(m) = self.modules.iter().find(|m| m.mount == module || m.name == module) {
                return self.sysroot.join("system/apex").join(&m.name).join(tail);
            }
            return self.sysroot.join("system/apex").join(module).join(tail);
        }
        self.sysroot.join(dev.trim_start_matches('/'))
    }

    /// Map a host sysroot path to its on-device absolute path.
    pub fn dev_path(&self, host: &Path) -> String {
        let s = host.to_string_lossy().to_string();
        let s = s.replace(&format!("{}/system/apex/", self.sysroot.display()), "/apex/");
        let s = s.replace(&self.sysroot.display().to_string(), "");
        s
    }

    // ---- sandbox ----------------------------------------------------------

    pub fn sandbox(&self) -> Sandbox {
        Sandbox::new(self)
    }
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
}

impl<'a> Sandbox<'a> {
    fn new(root: &'a ArtRoot) -> Self {
        let mut env = vec![
            ("ANDROID_ROOT".into(), "/system".into()),
            ("ANDROID_RUNTIME_ROOT".into(), "/apex/com.android.runtime".into()),
            ("ANDROID_ART_ROOT".into(), "/apex/com.android.art".into()),
            ("ANDROID_I18N_ROOT".into(), "/apex/com.android.i18n".into()),
            ("ANDROID_TZDATA_ROOT".into(), "/apex/com.google.android.tzdata5".into()),
            ("ANDROID_DATA".into(), "/data".into()),
            ("ANDROID_STORAGE".into(), "/data".into()),
            ("ANDROID_TMP".into(), "/data/tmp".into()),
            ("ANDROID_ART_PROPERTIES".into(), "/data/acos.properties".into()),
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
        }
    }

    pub fn ro_bind(&mut self, host: impl AsRef<Path>, guest: &str) -> &mut Self {
        self.ro_binds.push((host.as_ref().to_path_buf(), guest.to_string()));
        self
    }

    pub fn rw_bind(&mut self, host: impl AsRef<Path>, guest: &str) -> &mut Self {
        self.rw_binds.push((host.as_ref().to_path_buf(), guest.to_string()));
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
        a.push("--bind".into());
        a.push(r.data.join("linkerconfig").into());
        a.push("/linkerconfig".into());
        a.push("--bind".into());
        a.push(r.data.clone().into());
        a.push("/data".into());
        a.push("--dev".into());
        a.push("/dev".into());
        a.push("--proc".into());
        a.push("/proc".into());
        a.push("--ro-bind".into());
        a.push("/sys".into());
        a.push("/sys".into());
        for d in ["/vendor", "/product", "/system_ext", "/data/tmp", "/data/local/tmp"] {
            a.push("--dir".into());
            a.push(d.into());
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
            // over the bwrap-supplied /dev.
            a.push("--bind".into());
            a.push(r.data.join("socket").into());
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
            None => (r.device_bootclasspath()?, Some(r.fallback_dex2oat_classpath()?)),
        };
        for (k, v) in &self.env {
            let v = if k.as_str() == "BOOTCLASSPATH" { bc.join(":") } else { v.clone() };
            a.push("--setenv".into());
            a.push(k.as_str().into());
            a.push(v.as_str().into());
        }
        if let Some(dx) = dx {
            a.push("--setenv".into());
            a.push("DEX2OATBOOTCLASSPATH".into());
            a.push(dx.join(":").as_str().into());
        }
        if self.shim {
            a.push("--setenv".into());
            a.push("LD_PRELOAD".into());
            a.push("/data/shim/libartshim.so".into());
        }
        if let Some(img) = &self.image {
            a.push("--bind".into());
            a.push(img.clone().into());
            a.push("/data/img".into());
        }
        a.push("--".into());
        a.push(program.into());
        for arg in argv {
            a.push(arg.as_str().into());
        }
        if std::env::var_os("ACOS_ART_DEBUG_ARGV").is_some() {
            eprintln!("bwrap {}", a.iter().map(|x| x.to_string_lossy().to_string()).collect::<Vec<_>>().join(" "));
        }
        b.args(a);
        Ok(b)
    }
}

/// The workspace root: `$ACOS_ROOT`, else the nearest ancestor of the cwd or
/// the executable that contains a `crates/` + `Cargo.toml` pair.
pub fn repo_root() -> Result<PathBuf> {
    if let Some(p) = std::env::var_os("ACOS_ROOT") {
        let p = PathBuf::from(p);
        if p.join("Cargo.toml").is_file() {
            return Ok(p);
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
            return Ok(c);
        }
    }
    Err(Error::NoRepoRoot)
}

/// Minimal protobuf wire walk: `(field, wire_type, value)` where a
/// length-delimited value is the payload, and a varint is 8-byte LE (so the
/// caller can `u64::from_le_bytes`). Enough for `apex_manifest.pb` and the
/// `*.pb` classpath fragments; no schema, no allocation-heavy runtime.
fn proto_fields(bytes: &[u8]) -> Vec<(u64, u8, Vec<u8>)> {
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < bytes.len() {
        let Some((key, next)) = read_varint(bytes, i) else { break };
        i = next;
        let field = key >> 3;
        let wire = (key & 7) as u8;
        match wire {
            0 => {
                let Some((v, next)) = read_varint(bytes, i) else { break };
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
                let Some((len, next)) = read_varint(bytes, i) else { break };
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
    let Ok(bytes) = fs::read(dir.join("apex_manifest.pb")) else { return fallback.to_string() };
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
            out.push(ApexModule { name: dir, file, mount });
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
        return Err(Error::Layout(format!("no APEX payloads under {}", dir.display())));
    }
    Ok(out)
}

/// Compile the freestanding shim with clang (cached by source hash).
pub fn build_shim(dest_dir: &Path) -> Result<PathBuf> {
    fs::create_dir_all(dest_dir)?;
    let out = dest_dir.join("libartshim.so");
    let src = dest_dir.join("artshim.c");
    let hash = simple_hash(SHIM_SOURCE);
    let stamp = dest_dir.join("artshim.hash");
    let up_to_date = fs::read_to_string(&stamp)
        .map(|s| s.trim() == hash.to_string())
        .unwrap_or(false)
        && out.is_file();
    if up_to_date {
        return Ok(out);
    }
    fs::write(&src, SHIM_SOURCE)?;
    let clang = std::env::var("CC").unwrap_or_else(|_| "clang".to_string());
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
        .status()?;
    if !st.success() {
        return Err(Error::Tool(format!("{clang} failed to build {}", src.display())));
    }
    fs::write(&stamp, format!("{hash}\n"))?;
    Ok(out)
}

fn simple_hash(s: &str) -> u64 {
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

    #[test]
    fn classpath_env_parses_device_environ() {
        let text = "export BOOTCLASSPATH /apex/a.jar:/apex/b.jar:/system/framework/c.jar\n\
                    export DEX2OATBOOTCLASSPATH /apex/a.jar:/apex/b.jar\n\
                    export SYSTEMSERVERCLASSPATH /system/framework/services.jar\n";
        let cp = ArtRoot::parse_classpath_env(text).expect("parsed");
        assert_eq!(cp.boot, vec!["/apex/a.jar", "/apex/b.jar", "/system/framework/c.jar"]);
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
        assert_eq!(apex_mount_name(&dir.join("nope"), "com.android.art"), "com.android.art");
        let _ = fs::remove_dir_all(&dir);
    }
}
