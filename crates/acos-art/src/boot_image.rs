//! H13 — the boot-image source behind `ACOS_ART_BOOT_IMAGE`.
//!
//! | value | image | address policy |
//! |---|---|---|
//! | `lz4` (default) | the sysroot's stock LZ4 boot image | ART's per-process relocation (stock ASLR) |
//! | `shared-fixed` | an uncompressed image at the recorded base `0x70000000` | `-Xnorelocate`: fixed base, **no** image ASLR (B2/B3) |
//! | `shared-aslr` | an uncompressed image relocated to a random page-aligned base **once per serve cohort** | `-Xnorelocate`: fixed within the cohort, ASLR between cohorts |
//!
//! `shared-aslr` is the Android shape: the zygote relocates the boot image once
//! per boot and every app shares that one mapping. ACOS has no zygote on the
//! production path, so the equivalent is one relocated image file per serve
//! process. The first launch of a cohort builds it with the sysroot's own
//! `dex2oat64 --base=<random>` (B2: ~5 s); every later session of that cohort
//! maps the same file-backed pages read-only. A new serve process is a new
//! cohort and therefore a new random base.
//!
//! The prepared tree lives under `<ACOS_ART_DATA>/boot-image/` (mode `0700`,
//! never `/tmp`). The `lz4` default does not touch it. A set-but-unusable knob
//! fails the launch; there is no silent fallback to another image.

use crate::{ArtRoot, Error, Result};
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::Instant;

/// The mode selector. Unset means [`Mode::Lz4`] (the product default).
pub const ENV_BOOT_IMAGE: &str = "ACOS_ART_BOOT_IMAGE";
/// B3's explicit shared-image directory (kept as `shared-fixed`'s input).
pub const ENV_SHARED_BOOT_IMAGE: &str = "ACOS_SHARED_BOOT_IMAGE";
/// Internal cohort token. `acos serve` may set one; otherwise the cohort is
/// the invoking process (parent pid + start time), which is exactly one serve
/// or one `acos verify` process.
pub const ENV_GENERATION: &str = "ACOS_ART_BOOT_IMAGE_GENERATION";

/// ART's recorded boot-image base (`--base=0x70000000` in the product build).
pub const ART_BASE_ADDRESS: u32 = 0x7000_0000;
/// ART's own ASLR window: `ART_BASE_ADDRESS_MIN/MAX_DELTA` are ±128 MiB.
pub const BASE_DELTA: u32 = 128 * 1024 * 1024;
/// `kElfSegmentAlignment` on x86_64.
pub const PAGE: u32 = 4096;

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

/// Parse the knob. Unset is `lz4`; an empty or unknown value is refused so a
/// typo cannot silently measure another mode.
pub fn mode_from_env() -> Result<Mode> {
    parse_mode(std::env::var_os(ENV_BOOT_IMAGE).as_deref().map(|v| v.to_string_lossy()).as_deref())
}

fn parse_mode(raw: Option<&str>) -> Result<Mode> {
    let Some(raw) = raw else { return Ok(Mode::Lz4) };
    let value = raw.trim().to_ascii_lowercase();
    match value.as_str() {
        "lz4" => Ok(Mode::Lz4),
        "shared-fixed" => Ok(Mode::SharedFixed),
        "shared-aslr" => Ok(Mode::SharedAslr),
        other => Err(Error::Layout(format!(
            "{ENV_BOOT_IMAGE}={other:?}: expected lz4, shared-fixed or shared-aslr \
             (unset it for the default LZ4 image)"
        ))),
    }
}

/// One resolved image for the launch argv and sandbox bind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    pub mode: Mode,
    pub dir: PathBuf,
    /// Recorded image base (`image_header.GetImageBegin()`).
    pub base: u32,
    /// Prepared for *this* cohort (false when an existing image was reused).
    pub prepared: bool,
    /// One-time build cost, only for the launch that prepared the image.
    pub prep_ms: Option<u128>,
}

impl Plan {
    /// The line recorded in `LaunchResult.notes` and on stderr.
    #[must_use]
    pub fn note(&self) -> String {
        match self.mode {
            Mode::Lz4 => "boot-image: lz4 (stock ART image, per-process relocation)".into(),
            Mode::SharedFixed => format!(
                "shared-boot-image: {} (ACOS_ART_BOOT_IMAGE=shared-fixed; \
                 -Ximage:/data/img/boot.art -Xnorelocate; fixed base 0x{:08x}, no image ASLR)",
                self.dir.display(),
                self.base
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

/// Resolve the image for this launch. `None` means the stock LZ4 path and an
/// argv with no image flags at all.
pub fn resolve(root: &ArtRoot) -> Result<Option<Plan>> {
    let mode = mode_from_env()?;
    let explicit = std::env::var_os(ENV_SHARED_BOOT_IMAGE).map(PathBuf::from);
    if mode == Mode::Lz4
        && explicit.is_some()
        && std::env::var_os(ENV_BOOT_IMAGE).is_some()
    {
        return Err(Error::Layout(format!(
            "{ENV_BOOT_IMAGE}=lz4 contradicts {ENV_SHARED_BOOT_IMAGE};              unset one (an unset {ENV_BOOT_IMAGE} still honours the B3 variable)"
        )));
    }
    match (mode, explicit) {
        (Mode::Lz4, Some(dir)) => {
            // Backward compatibility with B3: the old variable alone still
            // means the fixed shared image it always meant.
            validate_dir(&dir)?;
            Ok(Some(Plan {
                mode: Mode::SharedFixed,
                dir,
                base: ART_BASE_ADDRESS,
                prepared: false,
                prep_ms: None,
            }))
        }
        (Mode::Lz4, None) => Ok(None),
        (Mode::SharedFixed, Some(dir)) => {
            validate_dir(&dir)?;
            Ok(Some(Plan {
                mode: Mode::SharedFixed,
                dir,
                base: ART_BASE_ADDRESS,
                prepared: false,
                prep_ms: None,
            }))
        }
        (Mode::SharedFixed, None) => Ok(Some(ensure_fixed(root)?)),
        (Mode::SharedAslr, Some(dir)) => Err(Error::Layout(format!(
            "{ENV_BOOT_IMAGE}=shared-aslr cannot be combined with \
             {ENV_SHARED_BOOT_IMAGE}={} (that variable names a fixed-base image)",
            dir.display()
        ))),
        (Mode::SharedAslr, None) => Ok(Some(ensure_aslr(root)?)),
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
             (unset it for the default LZ4 image)",
            dir.display()
        )))
    }
}

fn ensure_fixed(root: &ArtRoot) -> Result<Plan> {
    let base = ART_BASE_ADDRESS;
    let out = image_root(root).join(format!("fixed-{base:08x}"));
    let prepared = with_lock(root, || {
        if out.join("x86_64/boot.art").is_file() && out.join("x86_64/boot.oat").is_file() {
            return Ok(None);
        }
        // A failed earlier build may have left `0444` files; start clean.
        if out.exists() {
            fs::remove_dir_all(&out)?;
        }
        private_dir(&out)?;
        let report = build_to(root, &out, "uncompressed", base)?;
        make_read_only(&out)?;
        Ok(Some(report.elapsed_ms))
    })?;
    Ok(Plan {
        mode: Mode::SharedFixed,
        dir: out,
        base,
        prepared: prepared.is_some(),
        prep_ms: prepared,
    })
}

fn ensure_aslr(root: &ArtRoot) -> Result<Plan> {
    let key = cohort_key();
    let pointer = image_root(root).join("aslr-current.json");
    let (out, base, prepared) = with_lock(root, || {
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
            created_unix: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
        };
        write_pointer(&pointer, &meta)?;
        sweep_older(&image_root(root), &dir);
        Ok((dir, base, Some(report.elapsed_ms)))
    })?;
    Ok(Plan {
        mode: Mode::SharedAslr,
        dir: out,
        base,
        prepared: prepared.is_some(),
        prep_ms: prepared,
    })
}

fn write_pointer(pointer: &Path, meta: &PreparedMeta) -> Result<()> {
    let tmp = pointer.with_extension("tmp");
    {
        let mut f = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)?;
        let text = serde_json::to_string_pretty(meta)
            .map_err(|e| Error::Layout(format!("boot image metadata: {e}")))?;
        f.write_all(text.as_bytes())?;
        f.sync_all()?;
    }
    fs::rename(&tmp, pointer)?;
    Ok(())
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

fn with_lock<T>(root: &ArtRoot, f: impl FnOnce() -> Result<T>) -> Result<T> {
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
    Ok(BuildReport {
        elapsed_ms,
        jars: jars.len(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modes_parse_exactly() {
        assert_eq!(parse_mode(None).unwrap(), Mode::Lz4);
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
        let dir = std::env::temp_dir().join(format!("acos-h13-missing-{}", std::process::id()));
        let err = validate_dir(&dir).expect_err("missing boot.art must be refused");
        assert!(err.to_string().contains("no x86_64/boot.art"), "got {err}");
    }
}
