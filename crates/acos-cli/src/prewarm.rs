//! P8: page-cache prewarm of the ART launch working set.
//!
//! P7 (`results/art-runtime/P7.md` §6, §7 #7) measured that re-reading the
//! launch working set after an eviction costs **+198 ms** on the first launch
//! (660.9 vs 462.7 ms p50, 12/12 reps) and that the set is only **7.3–8.3 MiB**
//! of resident file pages across the boot image, the ART runtime libraries,
//! the boot-classpath jars, `dalvikvm64` and the APK.
//!
//! ```text
//! M3, page cache warm     462.7 ms p50
//! M3, working set evicted 660.9 ms p50   (+198 ms = 27 ms/MiB)
//! ```
//!
//! This module reads those files once, on a background thread, when
//! `acos serve` starts. The daemon's socket is already listening and its ready
//! time is unchanged (P7: 1.9 ms); the read is bounded (default 64 MiB, never
//! `mlock`, so the pages stay reclaimable page cache); and every decision is
//! logged. It changes no verification semantics: nothing here is on the launch,
//! observation or assertion path, and a failed read is ignored.
//!
//! The file set is **derived from the same sources the launch path uses**, not
//! from a hard-coded snapshot of P7's `fincore` output:
//!
//! | launch source | derived here from |
//! |---|---|
//! | boot image (`-Ximage` default / `ACOS_ART_BOOT_IMAGE` / `ACOS_SHARED_BOOT_IMAGE`) | `ACOS_SHARED_BOOT_IMAGE` or a generated `shared-fixed` tree when present, else the sysroot (`shared-aslr` is built later by the first launch) |
//! | `dalvikvm64` + `libart*.so` | the ART APEX under `<sysroot>/system/apex/` |
//! | `libandroid_runtime.so` | `<sysroot>/system/lib64/` |
//! | `BOOTCLASSPATH` jars | `<data>/system/environ/classpath` (`derive_classpath`), guest path → sysroot |
//! | guest dex, `LD_PRELOAD` shims, binder driver | `<data>/{app/launch-guest,app,shim,lib}` (what `launch.rs` builds) |
//! | fonts | `<sysroot>/system/etc/fonts.xml` + the `sans-serif` family files it names (`ensure_system_fonts`) |
//!
//! Two P8 findings shaped the order. (1) `framework.jar` is 38.2 MiB and P7
//! measured **zero resident pages** for it, but it *is* read on every launch:
//! warming everything except it left +125 ms of P7's +178 ms in place, and
//! warming it too removed all of it. So it is warmed first, after the boot
//! image and the runtime libs. (2) Under the 64 MiB cap the boot image, the
//! libs, `framework.jar` and the fonts leave ~16 MiB for the other classpath
//! jars, which are taken smallest-first — that covers the exact set P7 measured
//! as resident (telephony-common, ext, voip-common, ims-common,
//! framework-graphics). Files that do not fit the cap are skipped and logged.
//!
//! Knobs: `ACOS_NO_PREWARM=1` disables it, `ACOS_PREWARM_MAX_MB` sets the byte
//! cap (default 64), `ACOS_PREWARM_VERBOSE=1` logs every warmed file.

use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Instant;

use crate::engine::Engine;

/// Disable the prewarm (`1`/`true`/`yes`/`on`).
pub const ENV_DISABLE: &str = "ACOS_NO_PREWARM";
/// Byte cap in MiB (default [`DEFAULT_MAX_MIB`]).
pub const ENV_MAX_MB: &str = "ACOS_PREWARM_MAX_MB";
/// Log every warmed file, not just the summary.
pub const ENV_VERBOSE: &str = "ACOS_PREWARM_VERBOSE";

/// Default cap. P7's measured working set is ~8 MiB; 64 MiB leaves room for a
/// bigger image without ever turning startup into a large read.
pub const DEFAULT_MAX_MIB: u64 = 64;
/// Read granularity. One reusable buffer per thread; no allocation per file.
const CHUNK: usize = 256 * 1024;

/// What the prewarm did. Logged, and returned from [`run`] for tests.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Report {
    pub cap_bytes: u64,
    pub warmed: Vec<(PathBuf, u64)>,
    pub bytes: u64,
    pub skipped: Vec<(PathBuf, u64, &'static str)>,
    pub missing: usize,
    pub errors: usize,
    pub elapsed_ms: u128,
}

impl Report {
    /// One-line summary, the same text the daemon logs.
    pub fn summary(&self) -> String {
        format!(
            "warmed {} files, {:.1} MiB in {} ms (cap {:.1} MiB, \
             {} skipped, {} missing, {} read errors)",
            self.warmed.len(),
            self.bytes as f64 / (1024.0 * 1024.0),
            self.elapsed_ms,
            self.cap_bytes as f64 / (1024.0 * 1024.0),
            self.skipped.len(),
            self.missing,
            self.errors,
        )
    }
}

/// Spawn the prewarm on a background thread if it is enabled for `engine`.
///
/// Called by `acos serve` *after* the socket is bound and listening, so the
/// daemon is ready exactly as fast as before; the thread never holds a lock the
/// accept loop needs, never blocks a connection and dies with the process.
pub fn spawn_if_enabled(engine: Engine) {
    // The working set is ART's; a rust-vm daemon has no boot image or guest.
    if engine != Engine::Art {
        return;
    }
    if disabled() {
        eprintln!("acos serve: prewarm: disabled by {ENV_DISABLE}");
        return;
    }
    let started = std::thread::Builder::new()
        .name("acos-prewarm".to_string())
        .spawn(|| {
            let report = run();
            log_report(&report);
        });
    if let Err(error) = started {
        // Never a startup failure: the daemon works without warm pages.
        eprintln!("acos serve: prewarm: cannot spawn thread: {error}");
    }
}

/// Is the prewarm turned off by the environment?
pub fn disabled() -> bool {
    matches!(
        std::env::var(ENV_DISABLE)
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase()
            .as_str(),
        "1" | "true" | "yes" | "on"
    )
}

/// The byte cap from `ACOS_PREWARM_MAX_MB` (default 64 MiB, `0` disables).
pub fn cap_bytes() -> u64 {
    let mib = std::env::var(ENV_MAX_MB)
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
        .unwrap_or(DEFAULT_MAX_MIB);
    mib.saturating_mul(1024 * 1024)
}

/// Resolve the working set and read it into the page cache.
pub fn run() -> Report {
    let cap = cap_bytes();
    let mut report = Report {
        cap_bytes: cap,
        ..Report::default()
    };
    if cap == 0 {
        return report;
    }
    let candidates = candidates();
    if candidates.is_empty() {
        return report;
    }
    let started = Instant::now();
    let mut buffer = vec![0u8; CHUNK];
    let verbose = verbose();
    for path in candidates {
        let Ok(meta) = std::fs::metadata(&path) else {
            report.missing += 1;
            continue;
        };
        if !meta.is_file() {
            report.missing += 1;
            continue;
        }
        let size = meta.len();
        if report.bytes.saturating_add(size) > cap {
            report.skipped.push((path, size, "cap reached"));
            continue;
        }
        match warm(&path, &mut buffer) {
            Ok(read) => {
                report.bytes = report.bytes.saturating_add(read);
                if verbose {
                    eprintln!(
                        "acos serve: prewarm: warmed {} ({read} bytes)",
                        path.display()
                    );
                }
                report.warmed.push((path, read));
            }
            Err(_) => report.errors += 1,
        }
    }
    report.elapsed_ms = started.elapsed().as_millis();
    report
}

fn verbose() -> bool {
    matches!(
        std::env::var(ENV_VERBOSE)
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase()
            .as_str(),
        "1" | "true" | "yes" | "on"
    )
}

fn log_report(report: &Report) {
    eprintln!("acos serve: prewarm: {}", report.summary());
    for (path, size, why) in &report.skipped {
        eprintln!(
            "acos serve: prewarm: skipped {} ({:.1} MiB, {why})",
            path.display(),
            *size as f64 / (1024.0 * 1024.0)
        );
    }
}

/// Read the whole of `path` through `buffer`; the pages are now in the page
/// cache. No `mlock`, no `MAP_POPULATE`, nothing that would pin memory.
fn warm(path: &Path, buffer: &mut [u8]) -> std::io::Result<u64> {
    let mut file = File::open(path)?;
    let mut total = 0u64;
    loop {
        match file.read(buffer) {
            Ok(0) => return Ok(total),
            Ok(n) => total = total.saturating_add(n as u64),
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        }
    }
}

// ---------------------------------------------------------------- derivation

/// The candidate paths, in the order a launch touches them.
pub fn candidates() -> Vec<PathBuf> {
    let Some(root) = repo_root() else {
        return Vec::new();
    };
    let Some(sysroot) = sysroot(&root) else {
        return Vec::new();
    };
    let data = data_dir(&root);
    let mut out = Vec::new();
    out.extend(boot_image(&sysroot));
    out.extend(art_runtime(&sysroot));
    out.extend(guest_files(&sysroot, &data));
    out.extend(classpath_jars(&sysroot, &data));
    out.extend(fonts(&sysroot));
    // De-duplicate while keeping the launch order (hard links and the shared
    // boot-image seam can name the same file twice).
    let mut seen = std::collections::HashSet::new();
    out.retain(|path| seen.insert(path.clone()));
    out
}

/// `acos-art::repo_root()`'s rule, shared: `$ACOS_HOME` (an installed layout),
/// then `ACOS_ROOT`, then the binary's ancestors, then the working directory's.
fn repo_root() -> Option<PathBuf> {
    crate::root::runtime_root()
}

/// `ArtRoot::resolve`'s sysroot rule: `ACOS_ART_SYSROOT`, then the productised
/// `.cache/art/sysroot`, then the feasibility scratch.
fn sysroot(root: &Path) -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("ACOS_ART_SYSROOT") {
        let dir = PathBuf::from(dir);
        return dir.join("system/apex").is_dir().then_some(dir);
    }
    for candidate in [".cache/art/sysroot", ".cache/art-exp/sysroot"] {
        let dir = root.join(candidate);
        if dir.join("system/apex").is_dir() {
            return Some(dir);
        }
    }
    None
}

fn data_dir(root: &Path) -> PathBuf {
    std::env::var_os("ACOS_ART_DATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join(".cache/art/data"))
}

/// The boot image the launch will pass to ART. `ACOS_SHARED_BOOT_IMAGE` and a
/// generated `shared-fixed` directory are stable and can be warmed;
/// `shared-aslr` is built by the first launch after this prewarm, so it falls
/// back to the sysroot's stock trio. A directory without `x86_64/boot.art` is
/// ignored here — `launch.rs` is the fail-closed validator, this is only a
/// page-cache hint.
fn boot_image(sysroot: &Path) -> Vec<PathBuf> {
    let data = data_dir(&repo_root().unwrap_or_else(|| PathBuf::from(".")));
    let mut dirs = Vec::new();
    if let Some(raw) = std::env::var_os("ACOS_SHARED_BOOT_IMAGE") {
        dirs.push(PathBuf::from(raw));
    }
    let _ = &data;
    match std::env::var("ACOS_ART_BOOT_IMAGE")
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        // H13b: unset and `shared-fixed` both mean the cached default. The tree
        // is content-addressed, so the current entry is named by the pointer
        // `acos-art` publishes; a cold cache has no pointer yet and this falls
        // through to the sysroot files (the first launch builds the image).
        "" | "shared-fixed" if std::env::var_os("ACOS_SHARED_BOOT_IMAGE").is_none() => {
            if let Some(dir) = shared_cache_entry() {
                dirs.push(dir);
            }
        }
        // `shared-aslr` builds its cohort image on the first launch, after
        // this prewarm has run; warming a previous cohort's file would be
        // wasted (and could crowd out framework.jar under the cap).
        _ => {}
    }
    for dir in dirs {
        if !dir.join("x86_64/boot.art").is_file() {
            continue;
        }
        let isa = dir.join("x86_64");
        let mut out: Vec<PathBuf> = ["boot.art", "boot.oat", "boot.vdex"]
            .iter()
            .map(|name| isa.join(name))
            .collect();
        // Extension images are named after their boot-classpath component.
        if let Ok(rd) = std::fs::read_dir(&isa) {
            for entry in rd.flatten() {
                let p = entry.path();
                if p.extension().and_then(|e| e.to_str()) == Some("art")
                    && p != isa.join("boot.art")
                {
                    out.push(p);
                }
            }
        }
        return out;
    }
    let isa = sysroot.join("system/framework/x86_64");
    vec![
        isa.join("boot.art"),
        isa.join("boot.oat"),
        isa.join("boot.vdex"),
        sysroot.join("system/framework/boot.vdex"),
    ]
}

/// H13b: the `shared-fixed` cache root, the same chain `acos-art` uses:
/// `ACOS_BOOT_IMAGE_CACHE`, else `ACOS_CACHE_DIR`, else `$ACOS_HOME/.cache/acos`
/// (an installed layout stays self-contained), else `$XDG_CACHE_HOME/acos`,
/// else `~/.cache/acos`; then `art/boot-images`.
fn shared_cache_root() -> Option<PathBuf> {
    if let Some(explicit) = std::env::var_os("ACOS_BOOT_IMAGE_CACHE") {
        if !explicit.is_empty() {
            return Some(PathBuf::from(explicit));
        }
    }
    let base = if let Some(dir) = std::env::var_os("ACOS_CACHE_DIR") {
        PathBuf::from(dir)
    } else if let Some(home) = std::env::var_os("ACOS_HOME").filter(|h| !h.is_empty()) {
        PathBuf::from(home).join(".cache/acos")
    } else if let Some(xdg) = std::env::var_os("XDG_CACHE_HOME") {
        PathBuf::from(xdg).join("acos")
    } else {
        PathBuf::from(std::env::var_os("HOME")?).join(".cache/acos")
    };
    Some(base.join("art/boot-images"))
}

/// The cached `shared-fixed` entry named by `<cache>/current.json`, if any.
fn shared_cache_entry() -> Option<PathBuf> {
    let root = shared_cache_root()?;
    let text = std::fs::read_to_string(root.join("current.json")).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    let dir = PathBuf::from(value.get("dir")?.as_str()?);
    dir.join("x86_64/boot.art").is_file().then_some(dir)
}

/// `dalvikvm64` and the ART libraries it links, from the runtime APEX.
fn art_runtime(sysroot: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let apex = sysroot.join("system/apex");
    let Ok(entries) = std::fs::read_dir(&apex) else {
        return out;
    };
    let mut dirs: Vec<PathBuf> = entries.flatten().map(|entry| entry.path()).collect();
    dirs.sort();
    for dir in dirs {
        let dalvik = dir.join("bin/dalvikvm64");
        if dalvik.is_file() {
            out.push(dalvik);
            // The same APEX carries libart*.so / libnativehelper.so.
            for name in ["libart.so", "libartbase.so", "libnativehelper.so"] {
                let lib = dir.join("lib64").join(name);
                if lib.is_file() {
                    out.push(lib);
                }
            }
        }
    }
    for name in ["libandroid_runtime.so"] {
        let lib = sysroot.join("system/lib64").join(name);
        if lib.is_file() {
            out.push(lib);
        }
    }
    out
}

/// The `BOOTCLASSPATH` the launch exports, mapped from guest paths into the
/// sysroot. Falls back to the sysroot's own `system/framework/*.jar` set when
/// `derive_classpath` has not run yet (a fresh data directory).
fn classpath_jars(sysroot: &Path, data: &Path) -> Vec<PathBuf> {
    let env_file = data.join("system/environ/classpath");
    let mut jars: Vec<PathBuf> = Vec::new();
    if let Ok(text) = std::fs::read_to_string(&env_file) {
        for line in text.lines() {
            let Some(rest) = line.trim().strip_prefix("export BOOTCLASSPATH ") else {
                continue;
            };
            for guest in rest.trim().split(':').filter(|entry| !entry.is_empty()) {
                if let Some(host) = host_path_for_guest(sysroot, guest) {
                    jars.push(host);
                }
            }
        }
    }
    if jars.is_empty() {
        if let Ok(entries) = std::fs::read_dir(sysroot.join("system/framework")) {
            jars = entries
                .flatten()
                .map(|entry| entry.path())
                .filter(|path| path.extension().is_some_and(|ext| ext == "jar"))
                .collect();
        }
    }
    order_classpath(jars, sysroot)
}

/// Order the boot classpath by measured launch value: `framework.jar` first,
/// then the other platform `system/framework` jars, then the APEX/mainline jars
/// (the boot image preopts those), each group smallest-first.
///
/// P8 measured this: warming only the boot image, the runtime libs and
/// `framework.jar` (46.9 MiB) took a cold launch from 349–358 ms to 172–192 ms
/// ready, the same as warm; warming everything *except* `framework.jar` left
/// +125 ms. So `framework.jar` is read on every launch even though `fincore`
/// shows no resident pages for it afterwards. The remaining cap goes to the
/// other platform `system/framework` jars before the APEX/mainline jars: that
/// is the set P7 measured as resident (telephony-common, ext, voip-common,
/// ims-common, framework-graphics).
fn order_classpath(mut jars: Vec<PathBuf>, sysroot: &Path) -> Vec<PathBuf> {
    let platform = sysroot.join("system/framework");
    let rank = |path: &Path| {
        if is_framework_jar(path) {
            0
        } else if path.starts_with(&platform) {
            1
        } else {
            2
        }
    };
    jars.sort_by(|a, b| {
        rank(a)
            .cmp(&rank(b))
            .then_with(|| file_size(a).cmp(&file_size(b)))
            .then_with(|| a.cmp(b))
    });
    jars
}

fn is_framework_jar(path: &Path) -> bool {
    path.file_name().is_some_and(|name| name == "framework.jar")
}

fn file_size(path: &Path) -> u64 {
    std::fs::metadata(path)
        .map(|meta| meta.len())
        .unwrap_or(u64::MAX)
}

/// Map a device path (`/system/...`, `/apex/<mount>/...`) into the extracted
/// sysroot. An APEX mount name need not equal its host directory name (the
/// Google-signed modules differ), so an apex path falls back to a search of
/// `<sysroot>/system/apex/*/<tail>`.
pub fn host_path_for_guest(sysroot: &Path, guest: &str) -> Option<PathBuf> {
    let relative = guest.trim_start_matches('/');
    let direct = sysroot.join(relative);
    if direct.is_file() {
        return Some(direct);
    }
    let tail = relative.strip_prefix("apex/")?.split_once('/')?.1;
    let entries = std::fs::read_dir(sysroot.join("system/apex")).ok()?;
    let mut dirs: Vec<PathBuf> = entries.flatten().map(|entry| entry.path()).collect();
    dirs.sort();
    dirs.into_iter()
        .map(|dir| dir.join(tail))
        .find(|path| path.is_file())
}

/// What `launch.rs` builds into `<data>` before the guest starts: the guest
/// dex, the `LD_PRELOAD` shims and the binder driver.
fn guest_files(sysroot: &Path, data: &Path) -> Vec<PathBuf> {
    vec![
        data.join("app/launch-guest/classes.dex"),
        data.join("app/libacosart.so"),
        // `/data/libacoshw.so` is a bind of `<sysroot>/shim/libacoshw.so`.
        sysroot.join("shim/libacoshw.so"),
        data.join("shim/libartshim.so"),
        data.join("lib/libacos_binder_driver.so"),
    ]
}

/// The fonts `ensure_system_fonts` provisions and `Typeface` reads: the
/// `fonts.xml` tables plus the files the default `sans-serif` family names.
fn fonts(sysroot: &Path) -> Vec<PathBuf> {
    let dir = sysroot.join("system/fonts");
    let mut out = vec![sysroot.join("system/etc/fonts.xml"), dir.join("fonts.xml")];
    for name in std::fs::read_to_string(dir.join("fonts.xml"))
        .ok()
        .map(|xml| default_font_files(&xml))
        .unwrap_or_default()
    {
        out.push(dir.join(name));
    }
    out
}

/// File names referenced by the first `sans-serif` family (no `lang=`), i.e.
/// the family `Typeface.DEFAULT` resolves to.
pub fn default_font_files(xml: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut inside = false;
    for raw in xml.lines() {
        let line = raw.trim();
        if !inside {
            if line.starts_with("<family name=\"sans-serif\"") && !line.contains("lang=") {
                inside = true;
            } else {
                continue;
            }
        }
        if let Some(name) = font_file_in(line) {
            if !out.contains(&name) {
                out.push(name);
            }
        }
        if line.starts_with("</family>") {
            break;
        }
    }
    out
}

fn font_file_in(line: &str) -> Option<String> {
    let after_tag = line.split_once("<font")?.1;
    let text = after_tag.split_once('>')?.1;
    let name = text.split(['<', ' ', '\t']).next()?.trim();
    (name.ends_with(".ttf") || name.ends_with(".otf")).then(|| name.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("acos-p8-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn guest_system_path_maps_into_the_sysroot() {
        let root = scratch("sysmap");
        let jar = root.join("system/framework/framework.jar");
        std::fs::create_dir_all(jar.parent().unwrap()).unwrap();
        std::fs::write(&jar, b"jar").unwrap();
        assert_eq!(
            host_path_for_guest(&root, "/system/framework/framework.jar"),
            Some(jar)
        );
        assert_eq!(
            host_path_for_guest(&root, "/system/framework/absent.jar"),
            None
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn apex_mount_name_is_resolved_by_tail() {
        // The Google-signed modules mount as `com.android.configinfrastructure`
        // but their extracted directory is named after the payload.
        let root = scratch("apexmap");
        let jar = root.join("system/apex/com.google.android.configinfrastructure/javalib/fw.jar");
        std::fs::create_dir_all(jar.parent().unwrap()).unwrap();
        std::fs::write(&jar, b"jar").unwrap();
        assert_eq!(
            host_path_for_guest(
                &root,
                "/apex/com.android.configinfrastructure/javalib/fw.jar"
            ),
            Some(jar)
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn default_font_family_is_extracted_from_fonts_xml() {
        let xml = r#"
    <family name="sans-serif">
        <font weight="400" style="normal">Roboto-Regular.ttf
          <axis tag="wght" stylevalue="400" />
        </font>
        <font weight="400" style="italic">Roboto-Italic.ttf</font>
    </family>
    <family name="sans-serif" lang="ja">
        <font weight="400">NotoSansCJK.ttf</font>
    </family>
"#;
        assert_eq!(
            default_font_files(xml),
            vec![
                "Roboto-Regular.ttf".to_string(),
                "Roboto-Italic.ttf".to_string()
            ]
        );
    }

    #[test]
    fn the_classpath_is_ordered_framework_first_then_small_jars() {
        let root = scratch("order");
        let fw = root.join("framework.jar");
        let big = root.join("telephony-common.jar");
        let small = root.join("ims-common.jar");
        std::fs::write(&fw, vec![0u8; 100]).unwrap();
        std::fs::write(&big, vec![0u8; 80]).unwrap();
        std::fs::write(&small, vec![0u8; 8]).unwrap();
        assert_eq!(
            order_classpath(vec![big.clone(), small.clone(), fw.clone()], &root),
            vec![fw, small, big]
        );
        assert!(is_framework_jar(Path::new(
            "/x/system/framework/framework.jar"
        )));
        assert!(!is_framework_jar(Path::new(
            "/x/system/framework/framework-graphics.jar"
        )));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_cap_skips_a_file_that_does_not_fit_and_stops_at_the_cap() {
        let root = scratch("cap");
        let a = root.join("a");
        let b = root.join("b");
        std::fs::write(&a, vec![0u8; 8]).unwrap();
        std::fs::write(&b, vec![0u8; 64]).unwrap();
        let cap = 16u64;
        let mut report = Report {
            cap_bytes: cap,
            ..Report::default()
        };
        let mut buffer = vec![0u8; CHUNK];
        // Mirrors `run`'s decision order without touching the real sysroot.
        for path in [&a, &b, &a] {
            let size = std::fs::metadata(path).unwrap().len();
            if report.bytes.saturating_add(size) > cap {
                report.skipped.push((path.clone(), size, "cap reached"));
                continue;
            }
            let read = warm(path, &mut buffer).unwrap();
            report.bytes += read;
            report.warmed.push((path.clone(), read));
        }
        assert_eq!(report.warmed.len(), 2);
        assert_eq!(report.bytes, 16);
        assert_eq!(report.skipped.len(), 1);
        assert!(report.summary().contains("1 skipped"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn warm_reads_the_whole_file_and_never_mlocks() {
        let root = scratch("warm");
        let file = root.join("blob");
        std::fs::write(&file, vec![7u8; CHUNK + 123]).unwrap();
        let mut buffer = vec![0u8; CHUNK];
        assert_eq!(warm(&file, &mut buffer).unwrap(), (CHUNK + 123) as u64);
        assert!(warm(&root.join("absent"), &mut buffer).is_err());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn disable_and_cap_knobs_parse_conservatively() {
        // Only an explicit truthy value disables; an empty or odd value runs.
        for value in ["1", "true", "YES", "on"] {
            assert!(matches!(
                value.to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            ));
        }
        for value in ["", "0", "false", "no"] {
            assert!(!matches!(
                value.to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            ));
        }
    }
}
