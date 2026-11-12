//! Content-addressed cache for the per-launch build artefacts (W37).
//!
//! Every launch used to rebuild, into its own `--data` tree, the things that do
//! **not** depend on the APK: the freestanding liblog/property shim, the
//! substrate shim, the binder driver, the launch/render guest DEX and the
//! seccomp guest guard. `tools/art-runtime/run-exclusive.sh` gives each run a
//! *fresh* `ACOS_ART_DATA`, and a concurrent cohort may share one, so the old
//! per-data hash stamps were cold every time: N sessions meant N compiler
//! storms (clang `Bus error`, `pthread_create EAGAIN`) and ~1.9 s of wall time
//! before ART even started (W33).
//!
//! This module builds each artefact **once** into
//!
//! ```text
//! <repo>/.cache/art/build-cache/<sha256-of-inputs>/{MANIFEST.sha256, <files>}
//! ```
//!
//! and every later launch only reuses it:
//!
//! * the key hashes the source bytes, the compiler/`d8`/`javac`/`android.jar`
//!   identity (path, size, mtime) and the tool flags, so a toolchain change is
//!   a different key and never a stale artefact;
//! * builds happen in `.cache/art/build-cache/.tmp/<key>-<pid>-<seq>/` and the
//!   directory is published with a single `rename(2)` — a reader never sees a
//!   half-written entry;
//! * a per-key `flock(2)` serialises concurrent builders, so a cold cache with
//!   N concurrent launches compiles **once** and the other N-1 wait and hit;
//! * on reuse the whole entry is verified against `MANIFEST.sha256` (path, size
//!   and hash); a corrupted entry is reported and rebuilt, never trusted;
//! * nothing is built under `/tmp` (the directives keep ART scratch on the
//!   repository volume; R1's OOM incident).
//!
//! `ACOS_ART_BUILD_CACHE=off` bypasses the cache (each call builds into a
//! throw-away staging dir under `.cache`): the pre-W37 behaviour, kept for the
//! before/after measurement and for tests that must not touch a warm cache.

use crate::{Error, Result};
use std::fs;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// File that records `sha256  <name>` for every artefact in an entry.
pub const MANIFEST: &str = "MANIFEST.sha256";

static SEQ: AtomicU64 = AtomicU64::new(0);

/// The cache root: `$ACOS_ART_BUILD_CACHE`, else `<repo>/.cache/art/build-cache`.
///
/// `ACOS_ART_BUILD_CACHE` may name a directory (tests use a throw-away one) or
/// be `off`/`0`/`none` to disable reuse.
pub fn cache_root() -> PathBuf {
    match std::env::var_os("ACOS_ART_BUILD_CACHE") {
        Some(v) => {
            let s = v.to_string_lossy().to_string();
            // `off`/`0`/`none`/`disabled` switch reuse off (see `enabled`); they
            // are not directory names, so fall through to the default root.
            if !s.is_empty() && enabled() {
                return PathBuf::from(s);
            }
        }
        None => {}
    }
    match crate::repo_root() {
        Ok(r) => r.join(".cache/art/build-cache"),
        // No repo root (a test binary outside the tree): private scratch, still
        // never /tmp.
        Err(_) => std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(".cache/acos-art-build-cache"),
    }
}

/// Whether reuse is enabled (`ACOS_ART_BUILD_CACHE=off` disables it).
pub fn enabled() -> bool {
    match std::env::var_os("ACOS_ART_BUILD_CACHE") {
        Some(v) => {
            let s = v.to_string_lossy().to_ascii_lowercase();
            !matches!(s.as_str(), "off" | "0" | "none" | "disabled")
        }
        None => true,
    }
}

// ------------------------------------------------------------------ sha256

const K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

/// SHA-256 (FIPS 180-4), used only for cache keys/integrity — the crate has no
/// crypto dependency and a 32-byte digest is what `git`-style tools expect.
pub fn sha256(data: &[u8]) -> [u8; 32] {
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    let mut msg = data.to_vec();
    let bits = (data.len() as u64).wrapping_mul(8);
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bits.to_be_bytes());
    let mut w = [0u32; 64];
    for chunk in msg.chunks_exact(64) {
        for i in 0..16 {
            w[i] = u32::from_be_bytes([
                chunk[i * 4],
                chunk[i * 4 + 1],
                chunk[i * 4 + 2],
                chunk[i * 4 + 3],
            ]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let (mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh) =
            (h[0], h[1], h[2], h[3], h[4], h[5], h[6], h[7]);
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ (!e & g);
            let t1 = hh
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
        h[5] = h[5].wrapping_add(f);
        h[6] = h[6].wrapping_add(g);
        h[7] = h[7].wrapping_add(hh);
    }
    let mut out = [0u8; 32];
    for (i, v) in h.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&v.to_be_bytes());
    }
    out
}

pub fn sha256_hex(data: &[u8]) -> String {
    hex(&sha256(data))
}

fn hex(bytes: &[u8]) -> String {
    const D: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(D[(b >> 4) as usize] as char);
        s.push(D[(b & 0xf) as usize] as char);
    }
    s
}

/// Digest of a sequence of input parts, each length-prefixed so `["ab","c"]`
/// and `["a","bc"]` cannot collide.
pub fn digest(parts: &[&[u8]]) -> String {
    let mut buf = Vec::new();
    for p in parts {
        buf.extend_from_slice(&(p.len() as u64).to_be_bytes());
        buf.extend_from_slice(p);
    }
    sha256_hex(&buf)
}

/// Digest of owned parts (call sites usually have `Vec<Vec<u8>>`).
pub fn digest_owned(parts: &[Vec<u8>]) -> String {
    let refs: Vec<&[u8]> = parts.iter().map(|p| p.as_slice()).collect();
    digest(&refs)
}

/// Identity of a tool/input file: path, size and mtime. A toolchain upgrade is
/// a new cache key; the content of a 30 MB `android.jar` is not read per launch.
pub fn file_id(path: &Path) -> String {
    match fs::metadata(path) {
        Ok(m) => {
            let mtime = m
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            format!("{}|{}|{}", path.display(), m.len(), mtime)
        }
        Err(_) => format!("{}|absent", path.display()),
    }
}

// ------------------------------------------------------------------- cache

struct Lock {
    file: fs::File,
}

impl Lock {
    /// Exclusive `flock(2)` on the key's lock file (released on drop, and by the
    /// kernel if this process dies, so a crash cannot wedge the cache).
    fn acquire(path: &Path) -> Result<Self> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        let file = fs::OpenOptions::new().create(true).read(true).write(true).open(path)?;
        let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) };
        if rc != 0 {
            return Err(Error::Io(std::io::Error::last_os_error()));
        }
        Ok(Self { file })
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        let _ = unsafe { libc::flock(self.file.as_raw_fd(), libc::LOCK_UN) };
    }
}

/// A held per-key exclusive lock. Kept by call sites that compile outside this
/// module (the W36-owned launch path) so a cold cohort compiles once: the
/// first holder builds and publishes, the others then hit `reuse_to`.
pub struct BuildLock {
    _lock: Lock,
}

/// Take (and keep) the exclusive build lock for `key`. Blocks until the current
/// builder finishes; the kernel releases it if this process dies.
pub fn lock(key: &str) -> Result<BuildLock> {
    let root = cache_root();
    fs::create_dir_all(root.join(".locks"))?;
    Ok(BuildLock {
        _lock: Lock::acquire(&root.join(".locks").join(format!("{key}.lock")))?,
    })
}

/// Install a verified cached entry into `dest_dir` if one exists. Returns
/// `None` on a miss (or when the cache is disabled), `Some(paths)` after an
/// integrity check and an atomic per-file install. Takes no lock: entries are
/// published with `rename(2)`, so a reader sees either the whole entry or none.
pub fn reuse_to(key: &str, files: &[&str], dest_dir: &Path) -> Result<Option<Vec<PathBuf>>> {
    if !enabled() {
        return Ok(None);
    }
    let entry = cache_root().join(key);
    if !entry.is_dir() {
        return Ok(None);
    }
    if let Err(why) = verify(&entry, files) {
        eprintln!(
            "acos-art: build-cache CORRUPT {} {} ({why}); rebuilding",
            short(key),
            entry.display()
        );
        let _ = fs::remove_dir_all(&entry);
        return Ok(None);
    }
    let built = Built {
        dir: entry,
        key: key.to_string(),
        reused: true,
        staged_from: None,
    };
    eprintln!(
        "acos-art: build-cache HIT {} {}",
        short(key),
        built.dir.display()
    );
    Ok(Some(install(&built, files, dest_dir)?))
}

/// Publish artefacts built elsewhere under a lock the caller already holds.
pub fn publish_locked(key: &str, src_dir: &Path, files: &[&str]) -> Result<Built> {
    let root = cache_root();
    fs::create_dir_all(root.join(".tmp"))?;
    let entry = root.join(key);
    if enabled() && entry.is_dir() && verify(&entry, files).is_ok() {
        return Ok(Built {
            dir: entry,
            key: key.to_string(),
            reused: true,
            staged_from: None,
        });
    }
    let stage = root.join(".tmp").join(format!(
        "{key}-{}-{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = fs::remove_dir_all(&stage);
    fs::create_dir_all(&stage)?;
    for f in files {
        fs::copy(src_dir.join(f), stage.join(f))?;
    }
    fs::write(stage.join(MANIFEST), manifest_of(&stage, files)?)?;
    if !enabled() {
        return Ok(Built {
            dir: stage.clone(),
            key: key.to_string(),
            reused: false,
            staged_from: Some(stage),
        });
    }
    let _ = fs::remove_dir_all(&entry);
    fs::rename(&stage, &entry)?;
    eprintln!(
        "acos-art: build-cache STORE {} -> {}",
        short(key),
        entry.display()
    );
    Ok(Built {
        dir: entry,
        key: key.to_string(),
        reused: false,
        staged_from: Some(stage),
    })
}

/// A published cache entry (or, with the cache disabled, a private staging dir).
pub struct Built {
    /// Directory holding the verified artefacts.
    pub dir: PathBuf,
    /// The cache key (32 hex chars) — printed in the log markers.
    pub key: String,
    /// True when the artefacts came from a verified existing entry.
    pub reused: bool,
    /// The staging directory this build published from, if any (tests inspect
    /// it to prove the build ran in `.cache`, never in `/tmp`).
    pub staged_from: Option<PathBuf>,
}

impl Built {
    pub fn path(&self, file: &str) -> PathBuf {
        self.dir.join(file)
    }
}

fn short(key: &str) -> &str {
    &key[..key.len().min(12)]
}

/// Reuse or build the entry for `key`, which must contain `files`.
///
/// `build` receives an empty staging directory under the cache root and must
/// write every named file into it. Building happens under the per-key lock, so
/// a cohort that starts with a cold cache compiles once.
pub fn ensure<F>(key: &str, files: &[&str], build: F) -> Result<Built>
where
    F: FnOnce(&Path) -> Result<()>,
{
    let root = cache_root();
    let entry = root.join(key);
    // The guard must outlive the build (it is dropped at the end of this
    // function), so it is declared here, not inside the `if`.
    let mut build_lock: Option<Lock> = None;
    if enabled() {
        fs::create_dir_all(root.join(".locks"))?;
        build_lock = Some(Lock::acquire(&root.join(".locks").join(format!("{key}.lock")))?);
        // Double-checked: another builder may have published while we queued.
        if entry.is_dir() {
            match verify(&entry, files) {
                Ok(()) => {
                    eprintln!(
                        "acos-art: build-cache HIT {} {}{}",
                        short(key),
                        entry.display(),
                        if files.len() > 1 { "" } else { "" }
                    );
                    return Ok(Built {
                        dir: entry,
                        key: key.to_string(),
                        reused: true,
                        staged_from: None,
                    });
                }
                Err(why) => {
                    eprintln!(
                        "acos-art: build-cache CORRUPT {} {} ({why}); rebuilding",
                        short(key),
                        entry.display()
                    );
                    let _ = fs::remove_dir_all(&entry);
                }
            }
        }
    } else {
        eprintln!("acos-art: build-cache DISABLED (ACOS_ART_BUILD_CACHE=off); building uncached");
        fs::create_dir_all(root.join(".tmp"))?;
    }

    let stage = root.join(".tmp").join(format!(
        "{key}-{}-{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = fs::remove_dir_all(&stage);
    fs::create_dir_all(&stage)?;
    let started = std::time::Instant::now();
    match build(&stage) {
        Ok(()) => {}
        Err(e) => {
            let _ = fs::remove_dir_all(&stage);
            return Err(e);
        }
    }
    // Manifest last: an entry without one is never trusted.
    let manifest = match manifest_of(&stage, files) {
        Ok(m) => m,
        Err(e) => {
            let _ = fs::remove_dir_all(&stage);
            return Err(e);
        }
    };
    if let Err(e) = fs::write(stage.join(MANIFEST), manifest) {
        let _ = fs::remove_dir_all(&stage);
        return Err(Error::Io(e));
    }
    let ms = started.elapsed().as_millis();
    if !enabled() {
        eprintln!(
            "acos-art: build-cache built {} ({} ms, uncached) {}",
            short(key),
            ms,
            stage.display()
        );
        return Ok(Built {
            dir: stage.clone(),
            key: key.to_string(),
            reused: false,
            staged_from: Some(stage),
        });
    }
    let _hold = build_lock.take();
    // Atomic publish. A concurrent builder that won the race for the same key
    // (possible only if it did not take the lock) leaves our rename failing;
    // then fall back to its verified entry.
    fs::create_dir_all(&root)?;
    match fs::rename(&stage, &entry) {
        Ok(()) => {
            eprintln!(
                "acos-art: build-cache MISS {} built in {} ms -> {}",
                short(key),
                ms,
                entry.display()
            );
            Ok(Built {
                dir: entry,
                key: key.to_string(),
                reused: false,
                staged_from: Some(stage),
            })
        }
        Err(e) => {
            let _ = fs::remove_dir_all(&stage);
            if entry.is_dir() && verify(&entry, files).is_ok() {
                eprintln!(
                    "acos-art: build-cache HIT {} (published concurrently) {}",
                    short(key),
                    entry.display()
                );
                Ok(Built {
                    dir: entry,
                    key: key.to_string(),
                    reused: true,
                    staged_from: None,
                })
            } else {
                Err(Error::Io(e))
            }
        }
    }
}

/// Publish artefacts that were built elsewhere (the W36-owned launch path keeps
/// its own compile body) into the cache entry, atomically, under the key lock.
/// Existing verified entries are left untouched.
pub fn publish(key: &str, src_dir: &Path, files: &[&str]) -> Result<Built> {
    let root = cache_root();
    if !enabled() {
        let mut built = Built {
            dir: src_dir.to_path_buf(),
            key: key.to_string(),
            reused: false,
            staged_from: None,
        };
        // Copy to a private staging dir so callers can always `install` from
        // `built.dir` without racing a later in-place rebuild.
        let stage = root.join(".tmp").join(format!(
            "{key}-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&stage);
        fs::create_dir_all(&stage)?;
        for f in files {
            fs::copy(src_dir.join(f), stage.join(f))?;
        }
        built.dir = stage;
        return Ok(built);
    }
    let entry = root.join(key);
    fs::create_dir_all(root.join(".locks"))?;
    let _lock = Lock::acquire(&root.join(".locks").join(format!("{key}.lock")))?;
    if entry.is_dir() && verify(&entry, files).is_ok() {
        eprintln!(
            "acos-art: build-cache HIT {} {} (published earlier)",
            short(key),
            entry.display()
        );
        return Ok(Built {
            dir: entry,
            key: key.to_string(),
            reused: true,
            staged_from: None,
        });
    }
    let stage = root.join(".tmp").join(format!(
        "{key}-{}-{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = fs::remove_dir_all(&stage);
    fs::create_dir_all(&stage)?;
    for f in files {
        fs::copy(src_dir.join(f), stage.join(f))?;
    }
    fs::write(stage.join(MANIFEST), manifest_of(&stage, files)?)?;
    let _ = fs::remove_dir_all(&entry);
    fs::rename(&stage, &entry)?;
    eprintln!(
        "acos-art: build-cache STORE {} -> {}",
        short(key),
        entry.display()
    );
    Ok(Built {
        dir: entry,
        key: key.to_string(),
        reused: false,
        staged_from: Some(stage),
    })
}

/// Install `files` from a cache entry into `dest_dir`, atomically per file and
/// skipping identical existing copies. Concurrent sessions that share one
/// `--data` tree therefore never observe a partial artefact.
pub fn install(built: &Built, files: &[&str], dest_dir: &Path) -> Result<Vec<PathBuf>> {
    fs::create_dir_all(dest_dir)?;
    let mut out = Vec::new();
    for f in files {
        let src = built.path(f);
        let dest = dest_dir.join(f);
        let want = sha256_hex(&fs::read(&src)?);
        let up_to_date = dest.is_file()
            && fs::metadata(&dest).map(|m| m.len()).ok() == fs::metadata(&src).map(|m| m.len()).ok()
            && fs::read(&dest).map(|d| sha256_hex(&d) == want).unwrap_or(false);
        if !up_to_date {
            let tmp = dest_dir.join(format!(
                ".{f}.w37-{}-{}",
                std::process::id(),
                SEQ.fetch_add(1, Ordering::Relaxed)
            ));
            fs::copy(&src, &tmp)?;
            fs::rename(&tmp, &dest)?;
        }
        out.push(dest);
    }
    Ok(out)
}

fn manifest_of(dir: &Path, files: &[&str]) -> Result<String> {
    let mut out = String::new();
    for f in files {
        let p = dir.join(f);
        if !p.is_file() {
            return Err(Error::Tool(format!(
                "build produced no {f} in {}",
                dir.display()
            )));
        }
        out.push_str(&format!("{}  {}\n", sha256_hex(&fs::read(&p)?), f));
    }
    Ok(out)
}

/// Verify an entry against its manifest: no missing file, no size/hash drift,
/// and no unexpected extra artefact (a stale file means a different build).
fn verify(dir: &Path, files: &[&str]) -> std::result::Result<(), String> {
    let text = fs::read_to_string(dir.join(MANIFEST))
        .map_err(|e| format!("no {MANIFEST}: {e}"))?;
    let mut seen = Vec::new();
    for line in text.lines() {
        let Some((hash, name)) = line.split_once("  ") else {
            return Err(format!("malformed manifest line {line:?}"));
        };
        if name.is_empty() || name.contains('/') || name.contains("..") {
            return Err(format!("refusing manifest name {name:?}"));
        }
        let p = dir.join(name);
        let bytes = fs::read(&p).map_err(|e| format!("{name} unreadable: {e}"))?;
        if sha256_hex(&bytes) != hash {
            return Err(format!("{name} hash mismatch"));
        }
        seen.push(name.to_string());
    }
    if seen.len() != files.len() {
        return Err(format!(
            "manifest lists {} file(s), expected {}",
            seen.len(),
            files.len()
        ));
    }
    for f in files {
        if !seen.iter().any(|s| s == f) {
            return Err(format!("manifest does not list {f}"));
        }
    }
    Ok(())
}

/// Identity of an extracted image for per-sysroot cache entries
/// (`linkerconfig`, `derive_classpath`, the static-RRO idmaps).
///
/// Hashes the sysroot's `READY` stamp and its `MANIFEST.txt` (the file list
/// with sizes and sha256 prefixes written by `tools/art-sysroot.sh`), so a
/// re-extracted or repaired sysroot is a different key and no derived file is
/// ever reused across images. 248 KB is hashed once per process and memoised:
/// every launch asks for the same sysroot.
pub fn image_digest(sysroot: &Path) -> String {
    use std::sync::{Mutex, OnceLock};
    type Memo = Option<(PathBuf, String, String)>;
    static MEMO: OnceLock<Mutex<Memo>> = OnceLock::new();
    let memo = MEMO.get_or_init(|| Mutex::new(None));
    let stamp = format!(
        "{}|{}",
        file_id(&sysroot.join("READY")),
        file_id(&sysroot.join("MANIFEST.txt"))
    );
    if let Ok(guard) = memo.lock() {
        if let Some((p, s, d)) = guard.as_ref() {
            if p == sysroot && *s == stamp {
                return d.clone();
            }
        }
    }
    let mut parts: Vec<Vec<u8>> = vec![b"acos-art/image/v1".to_vec()];
    for rel in ["READY", "MANIFEST.txt"] {
        let p = sysroot.join(rel);
        match fs::read(&p) {
            Ok(bytes) => parts.push(bytes),
            Err(_) => parts.push(format!("{}|absent", p.display()).into_bytes()),
        }
    }
    let out = digest_owned(&parts);
    if let Ok(mut guard) = memo.lock() {
        *guard = Some((sysroot.to_path_buf(), stamp, out.clone()));
    }
    out
}

// ------------------------------------------------------- W39: tree entries
//
// `linkerconfig` and the static-RRO idmaps write a directory (a top-level
// config plus one `ld.config.txt` per APEX namespace, or one `.idmap` per
// overlay), not a fixed file list. A tree entry hashes **every** file below
// `dir` with its relative path in the manifest, so a stale or extra file in a
// reused entry is detected exactly like a byte flip in a flat entry.

/// Sorted relative paths of every regular file below `dir` (never
/// `MANIFEST.sha256`; symlinks are followed, which is what the guest sees).
fn walk_files(dir: &Path) -> Result<Vec<String>> {
    fn walk(base: &Path, cur: &Path, out: &mut Vec<String>) -> Result<()> {
        let rd = fs::read_dir(cur)?;
        for e in rd.flatten() {
            let p = e.path();
            let ft = e.file_type()?;
            if ft.is_dir() {
                walk(base, &p, out)?;
            } else if ft.is_file() || ft.is_symlink() && p.is_file() {
                let rel = p
                    .strip_prefix(base)
                    .map_err(|_| Error::Tool(format!("{} is outside {}", p.display(), base.display())))?
                .to_string_lossy()
                .to_string();
                if rel != MANIFEST {
                    out.push(rel);
                }
            }
        }
        Ok(())
    }
    let mut out = Vec::new();
    walk(dir, dir, &mut out)?;
    out.sort();
    Ok(out)
}

fn manifest_of_tree(dir: &Path) -> Result<String> {
    let mut out = String::new();
    for rel in walk_files(dir)? {
        let p = dir.join(&rel);
        out.push_str(&format!("{}  {}\n", sha256_hex(&fs::read(&p)?), rel));
    }
    if out.is_empty() {
        return Err(Error::Tool(format!("build produced an empty tree in {}", dir.display())));
    }
    Ok(out)
}

/// Verify a tree entry: the manifest and the files on disk must name the same
/// set (an extra file means a stale build), and every hash must match.
fn verify_tree(dir: &Path) -> std::result::Result<Vec<String>, String> {
    let text = fs::read_to_string(dir.join(MANIFEST)).map_err(|e| format!("no {MANIFEST}: {e}"))?;
    let mut seen: Vec<(String, String)> = Vec::new();
    for line in text.lines() {
        let Some((hash, name)) = line.split_once("  ") else {
            return Err(format!("malformed manifest line {line:?}"));
        };
        if name.is_empty()
            || name.starts_with('/')
            || name.split('/').any(|c| c.is_empty() || c == "." || c == "..")
        {
            return Err(format!("refusing manifest name {name:?}"));
        }
        let p = dir.join(name);
        let bytes = fs::read(&p).map_err(|e| format!("{name} unreadable: {e}"))?;
        if sha256_hex(&bytes) != hash {
            return Err(format!("{name} hash mismatch"));
        }
        seen.push((name.to_string(), hash.to_string()));
    }
    let actual = walk_files(dir)
        .map_err(|e| format!("cannot walk entry {}: {e}", dir.display()))?;
    if actual.len() != seen.len() {
        let extra: Vec<&String> = actual
            .iter()
            .filter(|a| !seen.iter().any(|(s, _)| s == *a))
            .collect();
        return Err(format!(
            "manifest lists {} file(s), entry has {} (unlisted: {})",
            seen.len(),
            actual.len(),
            if extra.is_empty() {
                "none".to_string()
            } else {
                extra.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ")
            }
        ));
    }
    Ok(actual)
}

/// Install a verified tree entry into `dest_dir`: every file is written to a
/// temporary name in its destination directory and `rename(2)`d into place, and
/// an identical existing copy is left untouched (concurrent sessions sharing
/// one `--data` tree never observe a partial tree). Returns the installed
/// paths, sorted by relative name.
pub fn reuse_tree_to(key: &str, dest_dir: &Path) -> Result<Option<Vec<PathBuf>>> {
    if !enabled() {
        return Ok(None);
    }
    let entry = cache_root().join(key);
    if !entry.is_dir() {
        return Ok(None);
    }
    let files = match verify_tree(&entry) {
        Ok(f) => f,
        Err(why) => {
            eprintln!(
                "acos-art: build-cache CORRUPT {} {} ({why}); rebuilding",
                short(key),
                entry.display()
            );
            let _ = fs::remove_dir_all(&entry);
            return Ok(None);
        }
    };
    let mut out = Vec::new();
    for rel in &files {
        let src = entry.join(rel);
        let dest = dest_dir.join(rel);
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)?;
        }
        let want = sha256_hex(&fs::read(&src)?);
        let up_to_date = dest.is_file()
            && fs::metadata(&dest).map(|m| m.len()).ok() == fs::metadata(&src).map(|m| m.len()).ok()
            && fs::read(&dest).map(|d| sha256_hex(&d) == want).unwrap_or(false);
        if !up_to_date {
            let tmp = dest.with_file_name(format!(
                ".{}.w39-{}-{}",
                dest.file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_default(),
                std::process::id(),
                SEQ.fetch_add(1, Ordering::Relaxed)
            ));
            fs::copy(&src, &tmp)?;
            fs::rename(&tmp, &dest)?;
        }
        out.push(dest);
    }
    eprintln!(
        "acos-art: build-cache HIT {} {} ({} file(s), tree)",
        short(key),
        entry.display(),
        files.len()
    );
    Ok(Some(out))
}

/// Publish a directory built elsewhere under a lock the caller already holds
/// (the linkerconfig / idmap builders write into `<data>` directly). `skip`
/// names relative paths that are not part of the derived tree (stamps, logs,
/// staged `.apk`s). An existing verified entry is left untouched: the inputs
/// are in the key, so identical inputs always mean identical output.
pub fn publish_tree_locked(
    key: &str,
    src_dir: &Path,
    skip: &dyn Fn(&str) -> bool,
) -> Result<Built> {
    let root = cache_root();
    let entry = root.join(key);
    if !enabled() {
        return Ok(Built {
            dir: src_dir.to_path_buf(),
            key: key.to_string(),
            reused: false,
            staged_from: None,
        });
    }
    if entry.is_dir() && verify_tree(&entry).is_ok() {
        return Ok(Built {
            dir: entry,
            key: key.to_string(),
            reused: true,
            staged_from: None,
        });
    }
    fs::create_dir_all(root.join(".tmp"))?;
    let stage = root.join(".tmp").join(format!(
        "{key}-{}-{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = fs::remove_dir_all(&stage);
    fs::create_dir_all(&stage)?;
    for rel in walk_files(src_dir)? {
        if skip(&rel) {
            continue;
        }
        let dest = stage.join(&rel);
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::copy(src_dir.join(&rel), &dest)?;
    }
    fs::write(stage.join(MANIFEST), manifest_of_tree(&stage)?)?;
    let _ = fs::remove_dir_all(&entry);
    fs::rename(&stage, &entry)?;
    eprintln!(
        "acos-art: build-cache STORE {} -> {} (tree)",
        short(key),
        entry.display()
    );
    Ok(Built {
        dir: entry,
        key: key.to_string(),
        reused: false,
        staged_from: Some(stage),
    })
}

/// Remove staging directories left by a killed builder (janitor hook).
pub fn sweep_staging() -> Result<usize> {
    let tmp = cache_root().join(".tmp");
    let Ok(rd) = fs::read_dir(&tmp) else { return Ok(0) };
    let mut n = 0;
    for e in rd.flatten() {
        let p = e.path();
        let stale = fs::metadata(&p)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .map(|age| age.as_secs() > 3600)
            .unwrap_or(false);
        if stale && fs::remove_dir_all(&p).is_ok() {
            n += 1;
        }
    }
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_matches_known_vectors() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        let long = vec![b'a'; 1_000_000];
        assert_eq!(
            sha256_hex(&long),
            "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0"
        );
    }

    #[test]
    fn digest_is_length_prefixed() {
        assert_ne!(digest(&[b"ab", b"c"]), digest(&[b"a", b"bc"]));
    }

    #[test]
    fn tmp_is_never_the_staging_root() {
        // The directive is "scratch under .cache, never /tmp": the cache root
        // is under the repository (or a test-supplied dir), and staging lives
        // inside it.
        let root = cache_root();
        assert!(!root.starts_with("/tmp"), "cache root {}", root.display());
        assert!(root.join(".tmp").starts_with(&root));
    }
}
