//! H13/H13b — the boot-image source behind `ACOS_ART_BOOT_IMAGE`.
//!
//! | value | image | address policy | cache |
//! |---|---|---|---|
//! | unset, `shared-fixed` (**default**) | an uncompressed image at the recorded base `0x70000000` | `-Xnorelocate`: fixed base, **no** image ASLR (B2/B3; owner decision ADR-0026 §4) | content-addressed on disk, built once per machine, reused by every serve and session |
//! | `lz4` | the sysroot's stock LZ4 boot image | ART's per-process relocation (stock ASLR) | none (each session decompresses ~26 MiB privately) |
//! | `shared-aslr` | an uncompressed image relocated to a random page-aligned base **once per serve cohort** | `-Xnorelocate`: fixed within the cohort, ASLR between cohorts | per-cohort directory under `<ACOS_ART_DATA>/boot-image/` |
//!
//! ## The `shared-fixed` cache (H13b)
//!
//! ADR-0026 decision 4 makes the shared image the default and accepts that the
//! image base is no longer randomised. The H13 cost of that choice was **+3.6 s
//! on the first launch of every serve** (one `dex2oat64` build, ~173 MiB of
//! image files). H13b removes the per-serve cost by caching the built tree on
//! disk, keyed by a hash of everything that can change the image:
//!
//! * the ACOS image-format version ([`IMAGE_FORMAT_VERSION`]) and the
//!   `dex2oat64` flags this module passes (`--image-format`, `--base`,
//!   instruction set, the boot classpath itself);
//! * the boot classpath definition (`BOOTCLASSPATH`/`DEX2OATBOOTCLASSPATH`
//!   environ file text) and the identity (path, size, mtime) of every boot jar
//!   in the sysroot (`system/framework/*.jar` and every `system/apex/*/jars/*.jar`);
//! * the identity of the ART binaries the recipe runs and the image must match:
//!   `dex2oat64`, `dalvikvm64`, `libart.so`, `libartbase.so`,
//!   `libnativehelper.so`.
//!
//! A cache hit therefore costs one `stat` per input (~30 files), not a build.
//!
//! The entry directory is `<cache>/fixed-<input-hash>/`; `<cache>` is
//! `ACOS_BOOT_IMAGE_CACHE`, else `ACOS_CACHE_DIR`, else `$ACOS_HOME/.cache/acos`
//! (an install stays self-contained), else `$XDG_CACHE_HOME/acos`, else
//! `~/.cache/acos` (the same chain `acos-runtime`'s boot cache uses), followed
//! by `art/boot-images`. It is private (`0700`) and never `/tmp`.
//!
//! **Atomic publish.** The image is built into a unique temp directory in the
//! same filesystem (`.tmp-<pid>-<nonce>`), every file is `fsync`ed, a
//! `manifest.json` is written and `fsync`ed, then the temp directory is
//! `rename`d onto the entry directory and the cache directory is `fsync`ed. A
//! crash leaves either the old entry or a temp directory, never a half entry.
//!
//! **Concurrent-safe.** A build holds an exclusive `flock` on
//! `<cache>/.lock`; a second serve that starts at the same time blocks, then
//! re-validates the manifest and hits the entry the first one published. Two
//! cooks, one build.
//!
//! **Corruption is detected, not trusted.** `manifest.json` records the input
//! hash and, for every image file, its size and a sampled checksum (first and
//! last 4 KiB, FNV-1a 64). A truncated or partially written file fails
//! validation and the entry is rebuilt. This is a performance cache, not a
//! security boundary: the guest gets the tree `--ro-bind` at `/data/img` and
//! the same untrusted-input rules apply as before.
//!
//! **Failure policy.** An explicit `ACOS_ART_BOOT_IMAGE=shared-fixed` fails the
//! launch closed (no silent fallback), as every explicit knob has since H13.
//! The *default* (knob unset, ADR-0026 §4) falls back to the sysroot's LZ4
//! image and says so — the launch reply carries
//! `boot_image: {mode, cache: "fallback", reason}` and the launcher logs the
//! same line. A machine that cannot build the shared image (no `dex2oat64`, a
//! full disk) must not become unable to run any app, and a silent fallback is
//! what this code avoids: the reason is in the reply.

use crate::{ArtRoot, Error, Result};
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::Instant;

/// The mode selector. Unset means [`Mode::SharedFixed`] (ADR-0026 §4).
pub const ENV_BOOT_IMAGE: &str = "ACOS_ART_BOOT_IMAGE";
/// B3's explicit shared-image directory (kept as `shared-fixed`'s input).
pub const ENV_SHARED_BOOT_IMAGE: &str = "ACOS_SHARED_BOOT_IMAGE";
/// The `shared-fixed` cache root (H13b). Falls back to the same chain as
/// `acos-runtime`'s boot cache: `ACOS_CACHE_DIR`, `$XDG_CACHE_HOME/acos`,
/// `~/.cache/acos`.
pub const ENV_CACHE_DIR: &str = "ACOS_BOOT_IMAGE_CACHE";
/// Internal cohort token for `shared-aslr`. `acos serve` may set one; otherwise
/// the cohort is the invoking process (parent pid + start time), which is
/// exactly one serve or one `acos verify` process.
pub const ENV_GENERATION: &str = "ACOS_ART_BOOT_IMAGE_GENERATION";

/// ART's recorded boot-image base (`--base=0x70000000` in the product build).
pub const ART_BASE_ADDRESS: u32 = 0x7000_0000;
/// ART's own ASLR window: `ART_BASE_ADDRESS_MIN/MAX_DELTA` are ±128 MiB.
pub const BASE_DELTA: u32 = 128 * 1024 * 1024;
/// `kElfSegmentAlignment` on x86_64.
pub const PAGE: u32 = 4096;

/// The ACOS image-format version. Bump when the recipe, the flags or the
/// on-disk layout changes: every cached entry then misses once and is rebuilt.
pub const IMAGE_FORMAT_VERSION: u32 = 1;
/// Cache manifest identity (independent of [`IMAGE_FORMAT_VERSION`]).
pub const CACHE_FORMAT: &str = "acos-art-boot-image/1";
/// Entry directories kept in the cache (current + one previous input set).
pub const KEEP_ENTRIES: usize = 2;
/// Bytes sampled from each end of an image file for the integrity checksum.
pub const SAMPLE_BYTES: usize = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Lz4,
    SharedFixed,
    SharedAslr,
}

impl Mode {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Lz4 => "lz4",
            Self::SharedFixed => "shared-fixed",
            Self::SharedAslr => "shared-aslr",
        }
    }
}

/// Parse the knob. Unset is `shared-fixed` (ADR-0026 §4); an empty or unknown
/// value is refused so a typo cannot silently measure another mode.
pub fn mode_from_env() -> Result<Mode> {
    parse_mode(std::env::var_os(ENV_BOOT_IMAGE).as_deref().map(|v| v.to_string_lossy()).as_deref())
}

fn parse_mode(raw: Option<&str>) -> Result<Mode> {
    let Some(raw) = raw else { return Ok(Mode::SharedFixed) };
    let value = raw.trim().to_ascii_lowercase();
    match value.as_str() {
        "lz4" => Ok(Mode::Lz4),
        "shared-fixed" => Ok(Mode::SharedFixed),
        "shared-aslr" => Ok(Mode::SharedAslr),
        other => Err(Error::Layout(format!(
            "{ENV_BOOT_IMAGE}={other:?}: expected lz4, shared-fixed or shared-aslr \
             (unset it for the default shared-fixed image)"
        ))),
    }
}

/// What the launch reply reports about the boot image (H13b):
/// `boot_image: {mode, cache, reason}`.
///
/// `cache` is `hit` (an existing entry validated), `built` (this launch built
/// it), `fallback` (the default build failed and the sysroot LZ4 image is in
/// use), `external` (an explicit `ACOS_SHARED_BOOT_IMAGE` directory) or `none`
/// (LZ4, no prepared tree).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BootImageReport {
    pub mode: String,
    pub cache: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dir: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_hash: Option<String>,
    /// Image base, `0x`-prefixed hex.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base: Option<String>,
    /// Only set on a build (`cache: built`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prep_ms: Option<u64>,
}

impl BootImageReport {
    /// The one-line `notes`/stderr form.
    #[must_use]
    pub fn line(&self) -> String {
        let mut out = format!("boot_image: mode={} cache={}", self.mode, self.cache);
        if let Some(dir) = &self.dir {
            out.push_str(&format!(" dir={dir}"));
        }
        if let Some(hash) = &self.input_hash {
            out.push_str(&format!(" input={hash}"));
        }
        if let Some(base) = &self.base {
            out.push_str(&format!(" base={base}"));
        }
        if let Some(ms) = self.prep_ms {
            out.push_str(&format!(" prep_ms={ms}"));
        }
        if let Some(reason) = &self.reason {
            out.push_str(&format!(" reason={reason}"));
        }
        out
    }

    fn lz4(reason: Option<String>) -> Self {
        Self {
            mode: Mode::Lz4.as_str().into(),
            cache: "none".into(),
            reason,
            ..Self::default()
        }
    }
}

/// One resolved image for the launch argv and sandbox bind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    pub mode: Mode,
    pub dir: PathBuf,
    /// Recorded image base (`image_header.GetImageBegin()`).
    pub base: u32,
    /// Prepared by *this* launch (false when an existing image was reused).
    pub prepared: bool,
    /// One-time build cost, only for the launch that prepared the image.
    pub prep_ms: Option<u128>,
    /// The structured reply field.
    pub report: BootImageReport,
}

impl Plan {
    /// The line recorded in `LaunchResult.notes` and on stderr.
    #[must_use]
    pub fn note(&self) -> String {
        match self.mode {
            Mode::Lz4 => "boot-image: lz4 (stock ART image, per-process relocation)".into(),
            Mode::SharedFixed => format!(
                "shared-boot-image: {} (ACOS_ART_BOOT_IMAGE=shared-fixed; \
                 -Ximage:/data/img/boot.art -Xnorelocate; fixed base 0x{:08x}, no image ASLR; \
                 cache {})",
                self.dir.display(),
                self.base,
                self.report.cache
            ),
            Mode::SharedAslr => format!(
                "shared-boot-image: {} (ACOS_ART_BOOT_IMAGE=shared-aslr; \
                 -Ximage:/data/img/boot.art -Xnorelocate; base 0x{:08x} random per serve, \
                 shared by its sessions; prep {} ms)",
                self.dir.display(),
                self.base,
                self.prep_ms.map_or_else(|| "reused".to_string(), |ms| ms.to_string())
            ),
        }
    }
}

/// The image (if any) plus the reply field for this launch.
#[derive(Debug, Clone)]
pub struct Resolution {
    pub image: Option<Plan>,
    pub report: BootImageReport,
}

/// Resolve the image for this launch. `image: None` means an argv with no
/// image flags at all (the sysroot's stock LZ4 image).
pub fn resolve(root: &ArtRoot) -> Result<Resolution> {
    let raw = std::env::var_os(ENV_BOOT_IMAGE);
    let explicit_knob = raw.is_some();
    let mode = parse_mode(raw.as_deref().map(|v| v.to_string_lossy()).as_deref())?;
    let explicit = std::env::var_os(ENV_SHARED_BOOT_IMAGE).map(PathBuf::from);
    match (mode, explicit) {
        (Mode::Lz4, Some(dir)) => Err(Error::Layout(format!(
            "{ENV_BOOT_IMAGE}=lz4 contradicts {ENV_SHARED_BOOT_IMAGE}={}; \
             unset one (an unset {ENV_BOOT_IMAGE} still honours the B3 variable)",
            dir.display()
        ))),
        (Mode::SharedAslr, Some(dir)) => Err(Error::Layout(format!(
            "{ENV_BOOT_IMAGE}=shared-aslr cannot be combined with \
             {ENV_SHARED_BOOT_IMAGE}={} (that variable names a fixed-base image)",
            dir.display()
        ))),
        (_, Some(dir)) => {
            // B3 compatibility: an explicit directory is used as-is (the image's
            // recorded base is what ART maps). Fail closed when it is unusable.
            validate_dir(&dir)?;
            let report = BootImageReport {
                mode: Mode::SharedFixed.as_str().into(),
                cache: "external".into(),
                reason: Some(format!("{ENV_SHARED_BOOT_IMAGE} is set")),
                dir: Some(dir.display().to_string()),
                base: Some(format!("0x{ART_BASE_ADDRESS:08x}")),
                ..BootImageReport::default()
            };
            Ok(Resolution {
                image: Some(Plan {
                    mode: Mode::SharedFixed,
                    dir,
                    base: ART_BASE_ADDRESS,
                    prepared: false,
                    prep_ms: None,
                    report: report.clone(),
                }),
                report,
            })
        }
        (Mode::Lz4, None) => Ok(Resolution {
            image: None,
            report: BootImageReport::lz4(Some(format!("{ENV_BOOT_IMAGE}=lz4"))),
        }),
        (Mode::SharedFixed, None) => match ensure_fixed(root) {
            Ok(plan) => Ok(Resolution { report: plan.report.clone(), image: Some(plan) }),
            Err(error) if !explicit_knob => {
                // The default must never be less available than LZ4 was: fall
                // back, log it, and say so in the launch reply.
                let reason = format!("shared-fixed build failed: {error}");
                eprintln!(
                    "acos-art: boot-image: {reason}; falling back to the sysroot lz4 image \
                     (boot_image.cache=fallback in the launch reply)"
                );
                Ok(Resolution { image: None, report: BootImageReport::lz4(Some(reason)) }.into_fallback())
            }
            Err(error) => Err(error),
        },
        (Mode::SharedAslr, None) => {
            let plan = ensure_aslr(root)?;
            Ok(Resolution { report: plan.report.clone(), image: Some(plan) })
        }
    }
}

impl Resolution {
    /// Mark an LZ4 report as the default's fallback (the reply distinguishes a
    /// deliberate `ACOS_ART_BOOT_IMAGE=lz4` from a failed default build).
    fn into_fallback(mut self) -> Self {
        self.report.cache = "fallback".into();
        self
    }
}

/// Reset the per-serve cohort. `acos serve` calls this through the internal
/// generation variable below; no product crate needs to depend on `acos-art`.
/// It is not required for correctness: a new serve process already has a new
/// parent pid/start time, so it is a new cohort by construction.
pub fn cohort_key() -> String {
    if let Some(raw) = std::env::var_os(ENV_GENERATION) {
        let value = raw.to_string_lossy().trim().to_string();
        if !value.is_empty() {
            return format!("env:{value}");
        }
    }
    let ppid = unsafe { libc::getppid() };
    let start = fs::read_to_string(format!("/proc/{ppid}/stat"))
        .ok()
        .and_then(|s| {
            s.rfind(')')
                .map(|i| s[i + 2..].split_whitespace().nth(19).map(str::to_string))
                .flatten()
        })
        .unwrap_or_else(|| "0".into());
    format!("ppid:{ppid}:{start}")
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PreparedMeta {
    generation: String,
    base: u32,
    dir: PathBuf,
    format: String,
    created_unix: u64,
}

fn image_root(root: &ArtRoot) -> PathBuf {
    root.data.join("boot-image")
}

fn private_dir(path: &Path) -> Result<()> {
    fs::create_dir_all(path)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

/// Validate the B3/`shared-fixed` input directory (fail closed).
fn validate_dir(dir: &Path) -> Result<()> {
    if dir.join("x86_64/boot.art").is_file() {
        Ok(())
    } else {
        Err(Error::Layout(format!(
            "{ENV_SHARED_BOOT_IMAGE}={}: no x86_64/boot.art \
             (unset it for the default shared-fixed image)",
            dir.display()
        )))
    }
}

// ---------------------------------------------------------------------------
// H13b: the cached default (`shared-fixed`)
// ---------------------------------------------------------------------------

/// The cache root: `ACOS_BOOT_IMAGE_CACHE`, else `ACOS_CACHE_DIR`, else
/// `$ACOS_HOME/.cache/acos` (an installed layout stays self-contained), else
/// `$XDG_CACHE_HOME/acos`, else `~/.cache/acos`; then `art/boot-images`.
pub fn cache_root() -> Result<PathBuf> {
    if let Some(explicit) = std::env::var_os(ENV_CACHE_DIR) {
        if !explicit.is_empty() {
            return Ok(PathBuf::from(explicit));
        }
    }
    let base = if let Some(dir) = std::env::var_os("ACOS_CACHE_DIR") {
        PathBuf::from(dir)
    } else if let Some(home) = crate::acos_home() {
        home.join(".cache/acos")
    } else if let Some(xdg) = std::env::var_os("XDG_CACHE_HOME") {
        PathBuf::from(xdg).join("acos")
    } else if let Some(home) = std::env::var_os("HOME") {
        PathBuf::from(home).join(".cache/acos")
    } else {
        return Err(Error::Layout(format!(
            "no cache directory for the shared boot image: set {ENV_CACHE_DIR} \
             (no ACOS_CACHE_DIR, XDG_CACHE_HOME or HOME)"
        )));
    };
    Ok(base.join("art/boot-images"))
}

/// Everything that can change the built image, hashed into one cache key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Inputs {
    /// 16 hex digits of FNV-1a 64 over the input set.
    pub key: String,
    /// Boot jars whose identity is part of the key.
    pub jars: usize,
    pub base: u32,
}

const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

fn fnv_bytes(hash: &mut u64, bytes: &[u8]) {
    for byte in bytes {
        *hash ^= u64::from(*byte);
        *hash = hash.wrapping_mul(FNV_PRIME);
    }
}

fn fnv_file_identity(hash: &mut u64, path: &Path) -> bool {
    fnv_bytes(hash, path.to_string_lossy().as_bytes());
    match fs::metadata(path) {
        Ok(meta) => {
            fnv_bytes(hash, &meta.len().to_le_bytes());
            fnv_bytes(hash, &meta.mtime().to_le_bytes());
            fnv_bytes(hash, &meta.mtime_nsec().to_le_bytes());
            true
        }
        Err(_) => {
            fnv_bytes(hash, b"missing");
            false
        }
    }
}

/// The `dex2oat64` binary and the runtime libraries the image must match.
fn runtime_binaries(root: &ArtRoot) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut apex_dirs: Vec<PathBuf> = fs::read_dir(root.sysroot.join("system/apex"))
        .map(|rd| rd.flatten().map(|entry| entry.path()).collect())
        .unwrap_or_default();
    apex_dirs.sort();
    for dir in apex_dirs {
        for rel in [
            "bin/dex2oat64",
            "bin/dalvikvm64",
            "lib64/libart.so",
            "lib64/libartbase.so",
            "lib64/libnativehelper.so",
        ] {
            let path = dir.join(rel);
            if path.is_file() {
                out.push(path);
            }
        }
    }
    // The linker resolves these from the platform too.
    for rel in ["system/lib64/libandroid_runtime.so", "system/lib64/libc.so"] {
        let path = root.sysroot.join(rel);
        if path.is_file() {
            out.push(path);
        }
    }
    out
}

/// Every boot jar in the sysroot: `system/framework/*.jar` plus every
/// `system/apex/*/jars/*.jar`. A superset of the live `BOOTCLASSPATH` is
/// deliberate: a jar that appears, disappears or changes must invalidate the
/// entry even if `derive_classpath` has not re-run yet.
fn boot_jars(root: &ArtRoot) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut push_jars = |dir: &Path| {
        if let Ok(rd) = fs::read_dir(dir) {
            for entry in rd.flatten() {
                let path = entry.path();
                if path.extension().is_some_and(|ext| ext == "jar") && path.is_file() {
                    out.push(path);
                }
            }
        }
    };
    push_jars(&root.sysroot.join("system/framework"));
    let mut apex_dirs: Vec<PathBuf> = fs::read_dir(root.sysroot.join("system/apex"))
        .map(|rd| rd.flatten().map(|entry| entry.path()).collect())
        .unwrap_or_default();
    apex_dirs.sort();
    for dir in apex_dirs {
        push_jars(&dir.join("jars"));
    }
    out.sort();
    out.dedup();
    out
}

/// Hash every input of the image recipe into one cache key.
pub fn inputs(root: &ArtRoot, image_format: &str, base: u32) -> Result<Inputs> {
    let mut hash = FNV_OFFSET;
    fnv_bytes(&mut hash, CACHE_FORMAT.as_bytes());
    fnv_bytes(&mut hash, &IMAGE_FORMAT_VERSION.to_le_bytes());
    fnv_bytes(&mut hash, image_format.as_bytes());
    fnv_bytes(&mut hash, &base.to_le_bytes());
    fnv_bytes(&mut hash, b"--instruction-set=x86_64");
    fnv_bytes(&mut hash, root.sysroot.to_string_lossy().as_bytes());
    // The boot classpath definition the build will be given. The environ file
    // is what `derive_classpath` wrote; BOOTCLASSPATH is the sysroot fallback.
    for rel in ["system/environ/classpath", "BOOTCLASSPATH"] {
        if let Ok(text) = fs::read_to_string(root.sysroot.join(rel)) {
            fnv_bytes(&mut hash, rel.as_bytes());
            fnv_bytes(&mut hash, text.as_bytes());
        }
    }
    for path in runtime_binaries(root) {
        fnv_file_identity(&mut hash, &path);
    }
    let jars = boot_jars(root);
    for jar in &jars {
        fnv_file_identity(&mut hash, jar);
    }
    Ok(Inputs { key: format!("{hash:016x}"), jars: jars.len(), base })
}

/// What a cache lookup or build did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CacheOutcome {
    /// An existing entry validated; nothing was built.
    Hit,
    /// An entry was built (or rebuilt after an invalid manifest). `reason` is
    /// the invalidation cause, `None` for a cold miss.
    Built { reason: Option<String>, elapsed_ms: u128 },
}

impl CacheOutcome {
    fn cache_str(&self) -> &'static str {
        match self {
            Self::Hit => "hit",
            Self::Built { .. } => "built",
        }
    }
}

/// One file of a prepared image, as the manifest records it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct FileEntry {
    path: String,
    size: u64,
    /// FNV-1a 64 over the first and last [`SAMPLE_BYTES`] of the file.
    crc: u64,
}

/// `manifest.json` of one cache entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Manifest {
    format: u32,
    mode: String,
    base: u32,
    input_hash: String,
    created_unix: u64,
    jars: usize,
    files: Vec<FileEntry>,
}

fn sample_crc(path: &Path, size: u64) -> std::io::Result<u64> {
    let mut file = File::open(path)?;
    let take = size.min(SAMPLE_BYTES as u64) as usize;
    let mut hash = FNV_OFFSET;
    let mut head = vec![0u8; take];
    file.read_exact(&mut head)?;
    fnv_bytes(&mut hash, &head);
    if size > SAMPLE_BYTES as u64 {
        use std::io::Seek;
        file.seek(std::io::SeekFrom::End(-(SAMPLE_BYTES as i64)))?;
        let mut tail = vec![0u8; SAMPLE_BYTES];
        file.read_exact(&mut tail)?;
        fnv_bytes(&mut hash, &tail);
    }
    fnv_bytes(&mut hash, &size.to_le_bytes());
    Ok(hash)
}

/// Walk `dir` and describe every regular file, sorted by relative path.
fn describe_tree(dir: &Path) -> Result<Vec<FileEntry>> {
    let mut files = Vec::new();
    let mut todo = vec![dir.to_path_buf()];
    while let Some(path) = todo.pop() {
        for entry in fs::read_dir(&path)? {
            let entry = entry?;
            let p = entry.path();
            if entry.file_type()?.is_dir() {
                todo.push(p);
            } else if entry.file_type()?.is_file() {
                let meta = entry.metadata()?;
                let rel = p
                    .strip_prefix(dir)
                    .unwrap_or(&p)
                    .to_string_lossy()
                    .to_string();
                if rel == "manifest.json" {
                    continue;
                }
                files.push(FileEntry {
                    path: rel,
                    size: meta.len(),
                    crc: sample_crc(&p, meta.len())?,
                });
            }
        }
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(files)
}

/// Validate an entry against the expected key. `Err` carries the reason a
/// rebuild is needed (empty/absent entry included).
fn validate_entry(dir: &Path, key: &str) -> std::result::Result<(), String> {
    let manifest_path = dir.join("manifest.json");
    let text = fs::read_to_string(&manifest_path)
        .map_err(|e| format!("no usable manifest at {}: {e}", manifest_path.display()))?;
    let manifest: Manifest = serde_json::from_str(&text)
        .map_err(|e| format!("manifest at {} is not JSON: {e}", manifest_path.display()))?;
    if manifest.format != IMAGE_FORMAT_VERSION {
        return Err(format!(
            "image-format version {} != {IMAGE_FORMAT_VERSION}",
            manifest.format
        ));
    }
    if manifest.input_hash != key {
        return Err(format!("input hash {} != {key}", manifest.input_hash));
    }
    if manifest.files.is_empty() {
        return Err("manifest lists no files".into());
    }
    for required in ["x86_64/boot.art", "x86_64/boot.oat"] {
        if !manifest.files.iter().any(|f| f.path == required) {
            return Err(format!("manifest does not list {required}"));
        }
    }
    for file in &manifest.files {
        let path = dir.join(&file.path);
        let meta = fs::metadata(&path)
            .map_err(|e| format!("{} is missing: {e}", file.path))?;
        if meta.len() != file.size {
            return Err(format!(
                "{} is {} bytes, manifest says {}",
                file.path,
                meta.len(),
                file.size
            ));
        }
        let crc = sample_crc(&path, meta.len())
            .map_err(|e| format!("{} cannot be read: {e}", file.path))?;
        if crc != file.crc {
            return Err(format!("{} fails its sampled checksum", file.path));
        }
    }
    Ok(())
}

fn sync_tree(dir: &Path) -> Result<()> {
    let mut todo = vec![dir.to_path_buf()];
    while let Some(path) = todo.pop() {
        for entry in fs::read_dir(&path)? {
            let entry = entry?;
            let p = entry.path();
            if entry.file_type()?.is_dir() {
                todo.push(p);
            } else {
                File::open(&p)?.sync_all()?;
            }
        }
    }
    File::open(dir)?.sync_all()?;
    Ok(())
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Publish `manifest.json` + `current.json` atomically (temp file, sync, rename).
fn write_json(path: &Path, value: &impl Serialize) -> Result<()> {
    let tmp = path.with_extension("tmp");
    {
        let mut f = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)?;
        let text = serde_json::to_string_pretty(value)
            .map_err(|e| Error::Layout(format!("boot image metadata: {e}")))?;
        f.write_all(text.as_bytes())?;
        f.write_all(b"\n")?;
        f.sync_all()?;
    }
    fs::rename(&tmp, path)?;
    Ok(())
}

fn with_dir_lock<T>(cache: &Path, f: impl FnOnce() -> Result<T>) -> Result<T> {
    private_dir(cache)?;
    let lock = cache.join(".lock");
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .mode(0o600)
        .open(&lock)?;
    let fd = file.as_raw_fd();
    if unsafe { libc::flock(fd, libc::LOCK_EX) } != 0 {
        return Err(Error::Io(std::io::Error::last_os_error()));
    }
    let result = f();
    let _ = unsafe { libc::flock(fd, libc::LOCK_UN) };
    result
}

/// Keep the newest [`KEEP_ENTRIES`] `fixed-*` directories, best effort. A
/// directory still mapped by a running session cannot be freed, and deleting it
/// is safe on Linux (the inode lives until the last mapping is gone).
fn prune(cache: &Path, keep: &Path) {
    let Ok(rd) = fs::read_dir(cache) else { return };
    let mut entries: Vec<(std::time::SystemTime, PathBuf)> = rd
        .flatten()
        .filter(|entry| entry.file_name().to_string_lossy().starts_with("fixed-"))
        .map(|entry| {
            let mtime = entry
                .metadata()
                .and_then(|m| m.modified())
                .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
            (mtime, entry.path())
        })
        .collect();
    entries.sort_by(|a, b| b.0.cmp(&a.0));
    for (_, path) in entries.into_iter().skip(KEEP_ENTRIES) {
        if path != keep {
            let _ = fs::remove_dir_all(&path);
        }
    }
    // Temp directories from a killed build.
    if let Ok(rd) = fs::read_dir(cache) {
        for entry in rd.flatten() {
            if entry.file_name().to_string_lossy().starts_with(".tmp-") {
                let _ = fs::remove_dir_all(entry.path());
            }
        }
    }
}

/// Ensure a valid entry for `inputs` exists under `cache`, building with
/// `build` when it does not. The lock is held across build + publish, so two
/// concurrent callers produce exactly one build.
pub fn ensure_cached<F>(
    cache: &Path,
    inputs: &Inputs,
    build: F,
) -> Result<(PathBuf, CacheOutcome)>
where
    F: FnOnce(&Path) -> Result<BuildReport>,
{
    ensure_cached_inner(cache, inputs, build, true)
}

fn ensure_cached_inner<F>(
    cache: &Path,
    inputs: &Inputs,
    build: F,
    prune_after: bool,
) -> Result<(PathBuf, CacheOutcome)>
where
    F: FnOnce(&Path) -> Result<BuildReport>,
{
    let target = cache.join(format!("fixed-{}", inputs.key));
    private_dir(cache)?;
    // Fast path without the lock: a valid entry is immutable.
    if validate_entry(&target, &inputs.key).is_ok() {
        return Ok((target, CacheOutcome::Hit));
    }
    with_dir_lock(cache, || {
        // Re-check under the lock: a concurrent builder may have published it.
        if validate_entry(&target, &inputs.key).is_ok() {
            return Ok((target.clone(), CacheOutcome::Hit));
        }
        let reason = validate_entry(&target, &inputs.key).err();
        let tmp = cache.join(format!(
            ".tmp-{}-{}",
            std::process::id(),
            unix_now() ^ (rand_u64() & 0xffff_ffff)
        ));
        let _ = fs::remove_dir_all(&tmp);
        private_dir(&tmp)?;
        let started = Instant::now();
        let report = match build(&tmp) {
            Ok(report) => report,
            Err(error) => {
                let _ = fs::remove_dir_all(&tmp);
                return Err(error);
            }
        };
        let elapsed_ms = report.elapsed_ms;
        if !tmp.join("x86_64/boot.art").is_file() {
            let _ = fs::remove_dir_all(&tmp);
            return Err(Error::Layout(format!(
                "dex2oat produced no {}/x86_64/boot.art",
                tmp.display()
            )));
        }
        let files = describe_tree(&tmp)?;
        let manifest = Manifest {
            format: IMAGE_FORMAT_VERSION,
            mode: "shared-fixed".into(),
            base: inputs.base,
            input_hash: inputs.key.clone(),
            created_unix: unix_now(),
            jars: inputs.jars,
            files,
        };
        write_json(&tmp.join("manifest.json"), &manifest)?;
        make_read_only(&tmp)?;
        sync_tree(&tmp)?;
        // Replace any invalid entry. Under the lock, so no reader is building.
        if target.exists() {
            fs::remove_dir_all(&target)?;
        }
        fs::rename(&tmp, &target)?;
        File::open(cache)?.sync_all()?;
        let pointer = cache.join("current.json");
        write_json(
            &pointer,
            &serde_json::json!({
                "format": IMAGE_FORMAT_VERSION,
                "mode": "shared-fixed",
                "input_hash": inputs.key,
                "base": format!("0x{:08x}", inputs.base),
                "jars": inputs.jars,
                "dir": target.display().to_string(),
            }),
        )?;
        if prune_after {
            prune(cache, &target);
        }
        Ok((target.clone(), CacheOutcome::Built { reason, elapsed_ms }))
    })
}

fn rand_u64() -> u64 {
    let mut bytes = [0u8; 8];
    if File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut bytes))
        .is_ok()
    {
        u64::from_ne_bytes(bytes)
    } else {
        unix_now()
    }
}

fn ensure_fixed(root: &ArtRoot) -> Result<Plan> {
    let base = ART_BASE_ADDRESS;
    let cache = cache_root()?;
    let inputs = inputs(root, "uncompressed", base)?;
    let (dir, outcome) = ensure_cached(&cache, &inputs, |tmp| {
        build_to(root, tmp, "uncompressed", base)
    })?;
    let (cache_str, prep_ms, reason) = match &outcome {
        CacheOutcome::Hit => ("hit", None, None),
        CacheOutcome::Built { reason, elapsed_ms } => {
            ("built", Some(*elapsed_ms), reason.clone())
        }
    };
    let report = BootImageReport {
        mode: Mode::SharedFixed.as_str().into(),
        cache: cache_str.into(),
        reason,
        dir: Some(dir.display().to_string()),
        input_hash: Some(inputs.key.clone()),
        base: Some(format!("0x{base:08x}")),
        prep_ms: prep_ms.map(|ms| ms.min(u128::from(u64::MAX)) as u64),
    };
    let plan = Plan {
        mode: Mode::SharedFixed,
        dir,
        base,
        prepared: prep_ms.is_some(),
        prep_ms,
        report,
    };
    eprintln!("acos-art: launch: {}", plan.note());
    Ok(plan)
}

fn ensure_aslr(root: &ArtRoot) -> Result<Plan> {
    let key = cohort_key();
    let pointer = image_root(root).join("aslr-current.json");
    let (out, base, prepared) = with_root_lock(root, || {
        if let Ok(text) = fs::read_to_string(&pointer) {
            if let Ok(meta) = serde_json::from_str::<PreparedMeta>(&text) {
                if meta.generation == key
                    && meta.base >= ART_BASE_ADDRESS - BASE_DELTA
                    && meta.base < ART_BASE_ADDRESS + BASE_DELTA
                    && meta.dir.join("x86_64/boot.art").is_file()
                    && meta.dir.join("x86_64/boot.oat").is_file()
                {
                    return Ok((meta.dir, meta.base, None));
                }
            }
        }
        let base = random_base()?;
        let dir = image_root(root).join(format!("aslr-{base:08x}-{:08x}", random_u32()?));
        private_dir(&dir)?;
        let report = build_to(root, &dir, "uncompressed", base)?;
        make_read_only(&dir)?;
        let meta = PreparedMeta {
            generation: key.clone(),
            base,
            dir: dir.clone(),
            format: "uncompressed".into(),
            created_unix: unix_now(),
        };
        write_json(&pointer, &meta)?;
        sweep_older(&image_root(root), &dir);
        Ok((dir, base, Some(report.elapsed_ms)))
    })?;
    let report = BootImageReport {
        mode: Mode::SharedAslr.as_str().into(),
        cache: if prepared.is_some() { "built" } else { "hit" }.into(),
        reason: Some("per-serve cohort image (ACOS_ART_BOOT_IMAGE=shared-aslr)".into()),
        dir: Some(out.display().to_string()),
        base: Some(format!("0x{base:08x}")),
        prep_ms: prepared.map(|ms| ms.min(u128::from(u64::MAX)) as u64),
        ..BootImageReport::default()
    };
    let plan = Plan {
        mode: Mode::SharedAslr,
        dir: out,
        base,
        prepared: prepared.is_some(),
        prep_ms: prepared,
        report,
    };
    eprintln!("acos-art: launch: {}", plan.note());
    Ok(plan)
}

/// Best-effort cleanup of images from previous cohorts. Deleting a mapped file
/// is safe on Linux (the inode lives while a session maps it); the next cohort
/// simply does not pay the disk again.
fn sweep_older(root: &Path, keep: &Path) {
    let Ok(rd) = fs::read_dir(root) else { return };
    for entry in rd.flatten() {
        let path = entry.path();
        if path == keep || path == root.join("aslr-current.json") || path == root.join(".lock") {
            continue;
        }
        if entry.file_name().to_string_lossy().starts_with("aslr-") {
            let _ = fs::remove_dir_all(&path);
        }
    }
}

fn make_read_only(dir: &Path) -> Result<()> {
    let mut todo = vec![dir.to_path_buf()];
    while let Some(path) = todo.pop() {
        for entry in fs::read_dir(&path)? {
            let entry = entry?;
            let p = entry.path();
            if entry.file_type()?.is_dir() {
                todo.push(p);
            } else {
                fs::set_permissions(&p, fs::Permissions::from_mode(0o444))?;
            }
        }
    }
    Ok(())
}

fn with_root_lock<T>(root: &ArtRoot, f: impl FnOnce() -> Result<T>) -> Result<T> {
    let dir = image_root(root);
    private_dir(&dir)?;
    let lock = dir.join(".lock");
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .mode(0o600)
        .open(&lock)?;
    let fd = file.as_raw_fd();
    if unsafe { libc::flock(fd, libc::LOCK_EX) } != 0 {
        return Err(Error::Io(std::io::Error::last_os_error()));
    }
    let result = f();
    let _ = unsafe { libc::flock(fd, libc::LOCK_UN) };
    result
}

/// One random base in ART's own ±128 MiB ASLR window, 4 KiB aligned.
/// 256 MiB / 4 KiB = 65 536 positions = **16 bits** of entropy per cohort.
fn random_base() -> Result<u32> {
    let positions = (BASE_DELTA as u64 * 2) / PAGE as u64;
    let index = random_u32()? as u64 % positions;
    let base = (ART_BASE_ADDRESS as u64 - BASE_DELTA as u64) + index * PAGE as u64;
    Ok(base as u32)
}

fn random_u32() -> Result<u32> {
    let mut bytes = [0u8; 4];
    File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(u32::from_ne_bytes(bytes))
}

/// What one `dex2oat64` build cost.
#[derive(Debug, Clone, Copy)]
pub struct BuildReport {
    pub elapsed_ms: u128,
    pub jars: usize,
}

/// Build an uncompressed boot image at `base` into `out` using the sysroot's
/// own `dex2oat64` in a staging sandbox. This is B2's exact recipe with the
/// base and format as arguments; the product `build-image` uses it too.
pub fn build_to(
    root: &ArtRoot,
    out: &Path,
    image_format: &str,
    base: u32,
) -> Result<BuildReport> {
    let named = out.join("x86_64");
    fs::create_dir_all(&named)?;
    root.ensure_linkerconfig(false)?;
    root.ensure_classpath_env(false)?;
    let jars = match root.classpaths() {
        Some(cp) => cp.boot,
        None => root.device_bootclasspath()?,
    };
    if jars.is_empty() {
        return Err(Error::Layout("empty BOOTCLASSPATH".into()));
    }
    let mut argv = vec![
        "--image=/data/img/x86_64/boot.art".to_string(),
        format!("--base=0x{base:08x}"),
        "--oat-file=/data/img/x86_64/boot.oat".to_string(),
        "--instruction-set=x86_64".to_string(),
        format!("--image-format={image_format}"),
    ];
    for j in &jars {
        argv.push(format!("--dex-file={j}"));
        argv.push(format!("--dex-location={j}"));
    }
    let mut sb = root.sandbox();
    sb.staging(true);
    sb.rw_bind(out, "/data/img");
    sb.env(
        "LD_LIBRARY_PATH",
        "/apex/com.android.art/lib64:/apex/com.android.os.statsd/lib64:\
         /apex/com.android.i18n/lib64:/system/lib64:/apex/sharedlibs/lib64",
    );
    let started = Instant::now();
    let mut cmd = sb.command("/apex/com.android.art/bin/dex2oat64", &argv)?;
    eprintln!(
        "acos-art: dex2oat64 ({} boot jars, image-format={image_format}, base=0x{base:08x}) …",
        jars.len()
    );
    let output = cmd.output()?;
    let elapsed_ms = started.elapsed().as_millis();
    eprintln!(
        "acos-art: dex2oat64 rc={:?} in {elapsed_ms} ms",
        output.status.code()
    );
    if !output.status.success() {
        let mut log = String::from_utf8_lossy(&output.stderr).to_string();
        log.push_str(&String::from_utf8_lossy(&output.stdout));
        let path = root.data.join("dex2oat.log");
        let _ = fs::write(&path, &log);
        return Err(Error::Tool(format!(
            "dex2oat64 failed (log: {})",
            path.display()
        )));
    }
    if !named.join("boot.art").is_file() {
        return Err(Error::Layout(format!(
            "dex2oat produced no {}",
            named.join("boot.art").display()
        )));
    }
    Ok(BuildReport { elapsed_ms, jars: jars.len() })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    fn scratch(name: &str) -> PathBuf {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join(".cache/art-boot-image-tests")
            .join(format!("{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A stand-in for `dex2oat64`: writes the two files the validator requires.
    fn fake_build(out: &Path) -> Result<BuildReport> {
        let named = out.join("x86_64");
        fs::create_dir_all(&named)?;
        fs::write(named.join("boot.art"), vec![7u8; 64 * 1024])?;
        fs::write(named.join("boot.oat"), b"oat-payload")?;
        Ok(BuildReport { elapsed_ms: 3, jars: 2 })
    }

    fn inputs_for(key: &str) -> Inputs {
        Inputs { key: key.to_string(), jars: 2, base: ART_BASE_ADDRESS }
    }

    #[test]
    fn modes_parse_exactly() {
        assert_eq!(parse_mode(None).unwrap(), Mode::SharedFixed, "unset is the H13b default");
        assert_eq!(parse_mode(Some("LZ4")).unwrap(), Mode::Lz4);
        assert_eq!(parse_mode(Some(" shared-fixed ")).unwrap(), Mode::SharedFixed);
        assert_eq!(parse_mode(Some("shared-aslr")).unwrap(), Mode::SharedAslr);
        assert!(parse_mode(Some("bogus")).is_err());
        assert!(parse_mode(Some("")).is_err());
    }

    #[test]
    fn random_bases_are_aligned_and_in_arts_window() {
        for _ in 0..64 {
            let base = random_base().unwrap();
            assert_eq!(base % PAGE, 0, "base 0x{base:08x} is not page aligned");
            assert!(base >= ART_BASE_ADDRESS - BASE_DELTA);
            assert!(base < ART_BASE_ADDRESS + BASE_DELTA);
            assert_ne!(base, ART_BASE_ADDRESS, "16-bit window has other positions");
        }
    }

    #[test]
    fn the_window_is_the_same_range_art_uses() {
        assert_eq!(BASE_DELTA, 128 * 1024 * 1024);
        assert_eq!((BASE_DELTA * 2) / PAGE, 65_536);
    }

    #[test]
    fn a_missing_explicit_image_is_refused() {
        let dir = scratch("missing").join("absent");
        let err = validate_dir(&dir).expect_err("missing boot.art must be refused");
        assert!(err.to_string().contains("no x86_64/boot.art"), "got {err}");
    }

    #[test]
    fn cold_miss_builds_and_the_second_run_hits() {
        let cache = scratch("cold-then-hit");
        let inputs = inputs_for("00000000000000aa");
        let (dir, first) = ensure_cached(&cache, &inputs, fake_build).unwrap();
        assert!(matches!(first, CacheOutcome::Built { .. }), "first run builds: {first:?}");
        assert!(dir.join("x86_64/boot.art").is_file());
        assert!(dir.join("manifest.json").is_file());
        // No builder call on the second run: a hit must not build.
        let calls = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&calls);
        let (dir2, second) = ensure_cached(&cache, &inputs, move |out| {
            counter.fetch_add(1, Ordering::SeqCst);
            fake_build(out)
        })
        .unwrap();
        assert_eq!(second, CacheOutcome::Hit);
        assert_eq!(calls.load(Ordering::SeqCst), 0, "a hit never runs the builder");
        assert_eq!(dir, dir2);
        let pointer = fs::read_to_string(cache.join("current.json")).unwrap();
        assert!(pointer.contains("00000000000000aa"), "pointer names the entry: {pointer}");
    }

    #[test]
    fn a_changed_input_set_invalidates_the_entry() {
        let cache = scratch("input-change");
        let a = inputs_for("00000000000000aa");
        let b = inputs_for("00000000000000bb");
        let (dir_a, first) = ensure_cached(&cache, &a, fake_build).unwrap();
        assert!(matches!(first, CacheOutcome::Built { .. }));
        let (dir_b, second) = ensure_cached(&cache, &b, fake_build).unwrap();
        assert!(matches!(second, CacheOutcome::Built { .. }), "new inputs must rebuild");
        assert_ne!(dir_a, dir_b);
        // The old entry stays valid for the old inputs (a checkout can go back).
        assert!(validate_entry(&dir_a, &a.key).is_ok());
        assert!(validate_entry(&dir_b, &b.key).is_ok());
    }

    #[test]
    fn a_truncated_file_is_detected_and_rebuilt() {
        let cache = scratch("truncated");
        let inputs = inputs_for("00000000000000cc");
        let (dir, _) = ensure_cached(&cache, &inputs, fake_build).unwrap();
        let art = dir.join("x86_64/boot.art");
        let len = fs::metadata(&art).unwrap().len();
        // A published entry is 0444; damage it from outside (a bad copy, media
        // corruption) the way a partial write would have left it.
        fs::set_permissions(&art, fs::Permissions::from_mode(0o644)).unwrap();
        fs::OpenOptions::new().write(true).open(&art).unwrap().set_len(len / 2).unwrap();
        let reason = validate_entry(&dir, &inputs.key).expect_err("truncation must be caught");
        assert!(reason.contains("boot.art"), "reason names the file: {reason}");
        assert!(reason.contains("bytes"), "reason is a size mismatch: {reason}");
        let calls = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&calls);
        let (dir2, outcome) = ensure_cached(&cache, &inputs, move |out| {
            counter.fetch_add(1, Ordering::SeqCst);
            fake_build(out)
        })
        .unwrap();
        assert!(matches!(outcome, CacheOutcome::Built { .. }), "truncation rebuilds: {outcome:?}");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(dir, dir2);
        assert_eq!(fs::metadata(dir.join("x86_64/boot.art")).unwrap().len(), 64 * 1024);
    }

    #[test]
    fn a_same_size_corruption_is_detected_by_the_sampled_checksum() {
        let cache = scratch("corrupt");
        let inputs = inputs_for("00000000000000dd");
        let (dir, _) = ensure_cached(&cache, &inputs, fake_build).unwrap();
        let art = dir.join("x86_64/boot.art");
        let mut bytes = fs::read(&art).unwrap();
        bytes[0] ^= 0xff;
        fs::set_permissions(&art, fs::Permissions::from_mode(0o644)).unwrap();
        let mut f = fs::OpenOptions::new().write(true).open(&art).unwrap();
        f.write_all(&bytes).unwrap();
        let reason = validate_entry(&dir, &inputs.key).expect_err("corruption must be caught");
        assert!(reason.contains("checksum"), "reason: {reason}");
        let (_, outcome) = ensure_cached(&cache, &inputs, fake_build).unwrap();
        assert!(matches!(outcome, CacheOutcome::Built { .. }));
    }

    #[test]
    fn two_concurrent_builders_produce_one_build() {
        let cache = scratch("concurrent");
        let inputs = Arc::new(inputs_for("00000000000000ee"));
        let calls = Arc::new(AtomicUsize::new(-1i64 as usize));
        calls.store(0, Ordering::SeqCst);
        let mut handles = Vec::new();
        for _ in 0..2 {
            let cache = cache.clone();
            let inputs = Arc::clone(&inputs);
            let calls = Arc::clone(&calls);
            handles.push(std::thread::spawn(move || {
                ensure_cached(&cache, &inputs, |out| {
                    calls.fetch_add(1, Ordering::SeqCst);
                    // Long enough for the peer to reach the lock.
                    std::thread::sleep(std::time::Duration::from_millis(150));
                    fake_build(out)
                })
                .unwrap()
            }));
        }
        let outcomes: Vec<CacheOutcome> = handles.into_iter().map(|h| h.join().unwrap().1).collect();
        assert_eq!(calls.load(Ordering::SeqCst), 1, "two cooks, one build: {outcomes:?}");
        let built = outcomes.iter().filter(|o| matches!(o, CacheOutcome::Built { .. })).count();
        let hit = outcomes.iter().filter(|o| **o == CacheOutcome::Hit).count();
        assert_eq!((built, hit), (1, 1), "one builds, the other hits: {outcomes:?}");
    }

    #[test]
    fn a_build_failure_is_a_structured_error_and_leaves_no_entry() {
        let cache = scratch("builder-fails");
        let inputs = inputs_for("00000000000000ff");
        let err = ensure_cached(&cache, &inputs, |_out| {
            Err(Error::Tool("dex2oat64 failed (log: /x/dex2oat.log)".into()))
        })
        .expect_err("a failed build must not be cached");
        assert!(err.to_string().contains("dex2oat64 failed"), "structured error: {err}");
        assert!(!cache.join("fixed-00000000000000ff").exists(), "no entry after a failure");
        // A builder that writes nothing is refused too (no half entry).
        let err = ensure_cached(&cache, &inputs, |_out| {
            Ok(BuildReport { elapsed_ms: 1, jars: 1 })
        })
        .expect_err("an empty build is refused");
        assert!(err.to_string().contains("no "), "empty build error: {err}");
        assert!(!cache.join("fixed-00000000000000ff").exists());
    }

    #[test]
    fn pruning_keeps_the_newest_entries() {
        let cache = scratch("prune");
        for key in ["0000000000000001", "0000000000000002", "0000000000000003"] {
            ensure_cached(&cache, &inputs_for(key), fake_build).unwrap();
        }
        let dirs: Vec<String> = fs::read_dir(&cache)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n.starts_with("fixed-"))
            .collect();
        assert_eq!(dirs.len(), KEEP_ENTRIES, "kept exactly {KEEP_ENTRIES}: {dirs:?}");
    }
}
