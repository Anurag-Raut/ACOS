//! On-disk content-addressed cache for a boot image (T-43b).
//!
//! [`RuntimeTemplate`](crate::session_pool::RuntimeTemplate) removes the
//! framework bring-up cost for every launch *after the first* in a process. A
//! fresh `acos launch` process still pays the full boot-classpath parse/link.
//! This module persists a framework-init `ACOSSNAP` stream (or any other boot
//! snapshot) under a content-addressed file so a cold process can restore it
//! instead of booting.
//!
//! # Trust model
//!
//! The cache is a **performance** cache, never a security boundary. Every byte
//! read back is treated as untrusted input: the header, the recorded key, the
//! recorded ACOS/format version and the payload checksum + length are all
//! validated before the payload is handed to `Vm::restore`, which itself
//! re-validates the snapshot header, boot fingerprint, property hash and class
//! layout. A mismatch, a short/oversized file or a checksum failure is a
//! **miss**, never an error that aborts a launch: the caller falls back to a
//! fresh boot.
//!
//! # Safety properties
//!
//! * private `0700` cache directory (`ACOS_CACHE_DIR`, else
//!   `$XDG_CACHE_HOME/acos`, else `~/.cache/acos`);
//! * `read` into a `Vec` with a hard size cap; the crate is
//!   `#![forbid(unsafe_code)]`, so no `mmap` (a concurrent shrink of a mapped
//!   file is not checkable from safe Rust);
//! * atomic publish: write to a unique temp file, `fsync`, then `rename`;
//! * best-effort `O_EXCL` lock so concurrent cold processes do not all boot,
//!   with stale-lock breaking;
//! * size-bounded LRU eviction after every publish.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

use acos_vm::CoreLib;

/// File magic for a cached boot image.
const MAGIC: [u8; 8] = *b"ACOSBIMG";
/// Fixed header length in bytes (magic, format, key, len, checksum, ver-len).
const HEADER_LEN: usize = 8 + 4 + 8 + 8 + 8 + 4;
/// On-disk header format version. Bump on any layout change. Version 2 added the
/// serialized, fully linked `BootImage` segment; version 3 adds the parsed
/// DEX id/string/type/method pools beside it; version 4 carries the VM
/// snapshot format v3 (native-memory regions, W154-A) inside the payload;
/// version 5 carries snapshot format v4 (source-recorded file-backed native
/// regions, W160-A) inside the payload.
pub const CACHE_FORMAT_VERSION: u32 = 5;
/// Hard ceiling on a cache file read into memory; larger files are refused.
pub const MAX_IMAGE_BYTES: u64 = 64 * 1024 * 1024;
/// Default total budget for the cache directory before LRU eviction.
pub const DEFAULT_MAX_CACHE_BYTES: u64 = 512 * 1024 * 1024;
/// A lock older than this is considered stale and broken.
const LOCK_STALE: Duration = Duration::from_secs(120);
/// Maximum accepted ACOS version-string length in the header.
const MAX_VERSION_LEN: u32 = 128;

/// A failure while opening or publishing a cache entry.
///
/// Loading never returns this: a bad entry is a [`LoadResult::Invalid`] miss.
#[derive(Debug, thiserror::Error)]
pub enum CacheError {
    /// Filesystem error creating the directory, temp file or rename.
    #[error("cache io: {0}")]
    Io(#[from] io::Error),
    /// A payload larger than [`MAX_IMAGE_BYTES`] was offered for storage.
    #[error("cache entry rejected: {0}")]
    Invalid(String),
}

/// Outcome of a cache lookup.
#[derive(Debug)]
pub enum LoadResult {
    /// A validated payload (cheap header checks passed).
    Hit(Vec<u8>),
    /// No entry for this key.
    Miss,
    /// An entry exists but failed validation. The caller must rebuild.
    Invalid(String),
}

/// Handle to a private on-disk boot-image cache directory.
#[derive(Debug, Clone)]
pub struct BootImageCache {
    dir: PathBuf,
    max_bytes: u64,
}

/// Held `O_EXCL` lock file; removed on drop.
#[derive(Debug)]
pub struct CacheLock {
    path: PathBuf,
}

impl Drop for CacheLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

static NONCE: AtomicU64 = AtomicU64::new(0);

fn next_nonce() -> u64 {
    NONCE.fetch_add(1, Ordering::Relaxed)
}

/// Resolve the cache directory from the environment.
///
/// Returns `None` when the cache is disabled (`ACOS_BOOT_CACHE=0`) or no home
/// directory can be found. Never fails: a cache that cannot be opened is a
/// disabled cache, not a launch error.
#[must_use]
pub fn from_env() -> Option<BootImageCache> {
    if std::env::var("ACOS_NO_BOOT_CACHE")
        .is_ok_and(|v| v == "1" || v.eq_ignore_ascii_case("on") || v.eq_ignore_ascii_case("true"))
    {
        return None;
    }
    if std::env::var("ACOS_BOOT_CACHE").is_ok_and(|v| v == "0" || v.eq_ignore_ascii_case("off")) {
        return None;
    }
    let dir = cache_root()?;
    let max_bytes = std::env::var("ACOS_BOOT_CACHE_MAX")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(DEFAULT_MAX_CACHE_BYTES);
    BootImageCache::new(dir, max_bytes).ok()
}

/// Whether the cache is enabled by the environment.
#[must_use]
pub fn enabled() -> bool {
    from_env().is_some()
}

fn cache_root() -> Option<PathBuf> {
    if let Some(d) = std::env::var_os("ACOS_CACHE_DIR") {
        if !d.is_empty() {
            return Some(PathBuf::from(d));
        }
    }
    // REL1a: an installed ACOS keeps its mutable state under ACOS_HOME.
    if let Some(home) = std::env::var_os("ACOS_HOME") {
        if !home.is_empty() {
            return Some(PathBuf::from(home).join(".cache/acos"));
        }
    }
    if let Some(x) = std::env::var_os("XDG_CACHE_HOME") {
        if !x.is_empty() {
            return Some(PathBuf::from(x).join("acos"));
        }
    }
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache/acos"))
}

impl BootImageCache {
    /// Open (creating if needed) the cache directory with the default budget.
    pub fn new(dir: PathBuf, max_bytes: u64) -> Result<BootImageCache, CacheError> {
        ensure_private_dir(&dir)?;
        Ok(BootImageCache { dir, max_bytes })
    }

    /// Open a cache rooted at `dir` (used by tests with a temp directory).
    pub fn with_dir(dir: impl Into<PathBuf>) -> Result<BootImageCache, CacheError> {
        BootImageCache::new(dir.into(), DEFAULT_MAX_CACHE_BYTES)
    }

    /// The cache directory.
    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The configured total-size budget in bytes.
    #[must_use]
    pub fn max_bytes(&self) -> u64 {
        self.max_bytes
    }

    /// Path of the entry for `key`.
    #[must_use]
    pub fn entry_path(&self, key: u64) -> PathBuf {
        self.dir.join(format!("{key:016x}.img"))
    }

    /// Content-addressed key for a boot classpath + ACOS/format version.
    ///
    /// The key mixes the boot-classpath **definition** and the identity
    /// (path, size, mtime) of every `dex/<jar>/classes*.dex` file, so a
    /// changed boot dir, a changed file or a different ACOS build all map to a
    /// different entry. It is a cache key, not a security token: the snapshot
    /// payload is separately validated on restore.
    #[must_use]
    pub fn image_key(&self, boot_dir: Option<&Path>, corelib: CoreLib) -> u64 {
        image_key(boot_dir, corelib)
    }

    /// Look up and validate an entry.
    #[must_use]
    pub fn load(&self, key: u64) -> LoadResult {
        let path = self.entry_path(key);
        let meta = match fs::metadata(&path) {
            Ok(m) => m,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return LoadResult::Miss,
            Err(e) => return LoadResult::Invalid(format!("stat {}: {e}", path.display())),
        };
        if !meta.is_file() {
            return LoadResult::Invalid("entry is not a regular file".to_string());
        }
        if meta.len() < HEADER_LEN as u64 {
            return LoadResult::Invalid(format!("truncated header: {} bytes", meta.len()));
        }
        if meta.len() > MAX_IMAGE_BYTES {
            return LoadResult::Invalid(format!("oversized file: {} bytes", meta.len()));
        }
        let file = match File::open(&path) {
            Ok(f) => f,
            Err(e) => return LoadResult::Invalid(format!("open {}: {e}", path.display())),
        };
        // Read with a hard cap; `+1` so a file that grew past the cap is caught.
        let mut buf = Vec::with_capacity(meta.len() as usize);
        let mut limited = file.take(MAX_IMAGE_BYTES + 1);
        if let Err(e) = limited.read_to_end(&mut buf) {
            return LoadResult::Invalid(format!("read {}: {e}", path.display()));
        }
        acos_vm::profile::note_alloc("boot_cache_file_read", buf.len() as u64);
        // `decode` consumes the read buffer and returns it with the header
        // drained, so a cache hit holds exactly one copy of the payload. The
        // old code copied the payload out (`payload.to_vec()`) while `buf` was
        // still live, doubling the largest transient allocation of a launch.
        decode(key, buf)
    }

    /// Publish `payload` under `key` atomically, then evict to budget.
    pub fn store(&self, key: u64, payload: &[u8]) -> Result<(), CacheError> {
        if payload.len() as u64 > MAX_IMAGE_BYTES {
            return Err(CacheError::Invalid(format!(
                "payload {} bytes exceeds cap {MAX_IMAGE_BYTES}",
                payload.len()
            )));
        }
        let mut bytes = Vec::with_capacity(HEADER_LEN + 16 + payload.len());
        bytes.extend_from_slice(&MAGIC);
        bytes.extend_from_slice(&CACHE_FORMAT_VERSION.to_le_bytes());
        bytes.extend_from_slice(&key.to_le_bytes());
        bytes.extend_from_slice(&(payload.len() as u64).to_le_bytes());
        bytes.extend_from_slice(&checksum(payload).to_le_bytes());
        let version = env!("CARGO_PKG_VERSION").as_bytes();
        bytes.extend_from_slice(&(version.len() as u32).to_le_bytes());
        bytes.extend_from_slice(version);
        bytes.extend_from_slice(payload);

        let tmp = self
            .dir
            .join(format!(".tmp-{}-{:x}", std::process::id(), next_nonce()));
        let write = (|| -> io::Result<()> {
            let mut f = open_private_write(&tmp)?;
            f.write_all(&bytes)?;
            f.sync_all()?;
            Ok(())
        })();
        if let Err(e) = write {
            let _ = fs::remove_file(&tmp);
            return Err(CacheError::Io(e));
        }
        let dest = self.entry_path(key);
        if let Err(e) = fs::rename(&tmp, &dest) {
            let _ = fs::remove_file(&tmp);
            return Err(CacheError::Io(e));
        }
        self.evict_to_fit();
        Ok(())
    }

    /// Best-effort exclusive lock for `key`.
    ///
    /// `None` means another process holds it (or it could not be created);
    /// the caller may still boot — the publish is atomic either way — but
    /// should not duplicate the boot work if it can avoid it.
    #[must_use]
    pub fn lock(&self, key: u64) -> Option<CacheLock> {
        let path = self.dir.join(format!("{key:016x}.lock"));
        if let Some(lock) = try_lock(&path) {
            return Some(lock);
        }
        // Break a stale lock (a crashed writer) once.
        if let Ok(meta) = fs::metadata(&path) {
            if meta
                .modified()
                .ok()
                .and_then(|m| SystemTime::now().duration_since(m).ok())
                .is_some_and(|age| age > LOCK_STALE)
            {
                let _ = fs::remove_file(&path);
                return try_lock(&path);
            }
        }
        None
    }

    /// Wait up to `timeout` for `key` to appear (a concurrent writer finished).
    #[must_use]
    pub fn wait_for(&self, key: u64, timeout: Duration) -> LoadResult {
        let deadline = SystemTime::now() + timeout;
        loop {
            match self.load(key) {
                LoadResult::Miss => {}
                other => return other,
            }
            if SystemTime::now() >= deadline {
                return LoadResult::Miss;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Remove entries (oldest first) until the directory fits the budget.
    pub fn evict_to_fit(&self) {
        let mut entries: Vec<(PathBuf, u64, SystemTime)> = Vec::new();
        let Ok(read) = fs::read_dir(&self.dir) else {
            return;
        };
        for entry in read.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.ends_with(".img") {
                if let Ok(meta) = entry.metadata() {
                    let mtime = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
                    entries.push((entry.path(), meta.len(), mtime));
                }
            } else if name.starts_with(".tmp-") {
                // Reap orphaned temp files from crashed writers.
                if entry
                    .metadata()
                    .ok()
                    .and_then(|m| m.modified().ok())
                    .and_then(|m| SystemTime::now().duration_since(m).ok())
                    .is_some_and(|age| age > LOCK_STALE)
                {
                    let _ = fs::remove_file(entry.path());
                }
            }
        }
        let mut total: u64 = entries.iter().map(|(_, len, _)| *len).sum();
        if total <= self.max_bytes {
            return;
        }
        entries.sort_by_key(|(_, _, mtime)| *mtime);
        for (path, len, _) in entries {
            if total <= self.max_bytes {
                break;
            }
            if fs::remove_file(&path).is_ok() {
                total = total.saturating_sub(len);
            }
        }
    }
}

/// Domain-separated cache key: [`image_key`] mixed with a variant tag.
///
/// The framework-init snapshot (`variant = "framework"`) and a bare core-VM
/// template (`variant = "session-template"`) share the same boot classpath and
/// ACOS version but are different payloads, so they must not collide.
#[must_use]
pub fn salted_key(variant: &str, boot_dir: Option<&Path>, corelib: CoreLib) -> u64 {
    let mut h = image_key(boot_dir, corelib);
    for b in variant.as_bytes() {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h ^= 0xff;
    h.wrapping_mul(0x0000_0100_0000_01b3)
}

/// Compute the content-addressed key for a boot classpath definition.
#[must_use]
pub fn image_key(boot_dir: Option<&Path>, corelib: CoreLib) -> u64 {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut h = OFFSET;
    let mix = |h: &mut u64, b: u8| {
        *h ^= b as u64;
        *h = h.wrapping_mul(PRIME);
    };
    let mix_u64 = |h: &mut u64, v: u64| {
        for b in v.to_le_bytes() {
            mix(h, b);
        }
    };
    let mix_str = |h: &mut u64, s: &str| {
        for b in s.as_bytes() {
            mix(h, *b);
        }
        mix(h, 0xff);
    };

    mix_u64(&mut h, acos_vm::snapshot::boot_dir_fingerprint(boot_dir));
    if let Some(dir) = boot_dir {
        if let Ok(text) = fs::read_to_string(dir.join("BOOTCLASSPATH")) {
            for line in text.lines() {
                let jar = line.trim();
                if jar.is_empty() {
                    continue;
                }
                mix_str(&mut h, jar);
                let base = jar.trim_end_matches(".jar");
                let dexdir = dir.join("dex").join(base);
                let mut files: Vec<PathBuf> = fs::read_dir(&dexdir)
                    .into_iter()
                    .flatten()
                    .flatten()
                    .map(|e| e.path())
                    .collect();
                files.sort();
                for file in files {
                    if let Ok(meta) = fs::metadata(&file) {
                        mix_str(&mut h, &file.to_string_lossy());
                        mix_u64(&mut h, meta.len());
                        if let Ok(modified) = meta.modified() {
                            if let Ok(delta) = modified.duration_since(SystemTime::UNIX_EPOCH) {
                                mix_u64(&mut h, delta.as_nanos() as u64);
                            }
                        }
                    }
                }
            }
        }
    }
    mix_u64(&mut h, u64::from(CACHE_FORMAT_VERSION));
    mix_str(&mut h, env!("CARGO_PKG_VERSION"));
    mix_str(
        &mut h,
        match corelib {
            CoreLib::Bootstrap => "bootstrap",
            CoreLib::Real => "real",
        },
    );
    h
}

fn decode(expected_key: u64, mut buf: Vec<u8>) -> LoadResult {
    if buf.len() < HEADER_LEN {
        return LoadResult::Invalid("truncated header".to_string());
    }
    if buf[..8] != MAGIC {
        return LoadResult::Invalid("bad magic".to_string());
    }
    let format = u32::from_le_bytes(buf[8..12].try_into().unwrap_or_default());
    if format != CACHE_FORMAT_VERSION {
        return LoadResult::Invalid(format!("format version {format} != {CACHE_FORMAT_VERSION}"));
    }
    let key = u64::from_le_bytes(buf[12..20].try_into().unwrap_or_default());
    if key != expected_key {
        return LoadResult::Invalid(format!("key {key:016x} != {expected_key:016x}"));
    }
    let payload_len = u64::from_le_bytes(buf[20..28].try_into().unwrap_or_default());
    let payload_checksum = u64::from_le_bytes(buf[28..36].try_into().unwrap_or_default());
    let version_len = u32::from_le_bytes(buf[36..40].try_into().unwrap_or_default());
    if version_len > MAX_VERSION_LEN {
        return LoadResult::Invalid(format!("version length {version_len} too large"));
    }
    let version_len = version_len as usize;
    if payload_len > MAX_IMAGE_BYTES {
        return LoadResult::Invalid(format!("payload length {payload_len} exceeds cap"));
    }
    let Some(total) = HEADER_LEN.checked_add(version_len).and_then(|n| {
        usize::try_from(payload_len)
            .ok()
            .and_then(|p| n.checked_add(p))
    }) else {
        return LoadResult::Invalid("length overflow".to_string());
    };
    if buf.len() != total {
        return LoadResult::Invalid(format!(
            "length mismatch: file {} bytes, header says {total}",
            buf.len()
        ));
    }
    let version = match std::str::from_utf8(&buf[HEADER_LEN..HEADER_LEN + version_len]) {
        Ok(v) => v,
        Err(_) => return LoadResult::Invalid("version is not utf-8".to_string()),
    };
    if version != env!("CARGO_PKG_VERSION") {
        return LoadResult::Invalid(format!(
            "acos version {version:?} != {:?}",
            env!("CARGO_PKG_VERSION")
        ));
    }
    let prefix = HEADER_LEN + version_len;
    if checksum(&buf[prefix..]) != payload_checksum {
        return LoadResult::Invalid("payload checksum mismatch".to_string());
    }
    // Move the payload to the front in place instead of allocating a second
    // full-size buffer. The reader still owns `buf`'s capacity (the file
    // size), which is the payload plus a few dozen header bytes.
    if prefix > 0 {
        buf.drain(..prefix);
    }
    acos_vm::profile::note_alloc("boot_cache_payload", buf.len() as u64);
    LoadResult::Hit(buf)
}

/// FNV-1a 64-bit. Corruption/truncation detection, not a security hash; the
/// snapshot's own fingerprint checks are the integrity boundary.
fn checksum(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

fn try_lock(path: &Path) -> Option<CacheLock> {
    match open_private_exclusive(path) {
        Ok(_) => Some(CacheLock {
            path: path.to_path_buf(),
        }),
        Err(_) => None,
    }
}

fn open_private_write(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

fn open_private_exclusive(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

/// Create (if needed) and validate the cache root.
///
/// The policy lives in [`crate::private_root::ensure_private_root`]: a symlinked
/// or non-directory root is refused *before* `create_dir_all` (which would
/// follow a link), a root owned by another uid is refused, and a group/world
/// **writable** root (`mode & 0o022`) is refused outright rather than silently
/// `chmod`-ed — an attacker-supplied `ACOS_CACHE_DIR` must not be trusted just
/// because we can tighten it (W170-A E1). Other group/other access is tightened
/// to `0700`, then re-checked.
fn ensure_private_dir(dir: &Path) -> io::Result<()> {
    if let Ok(meta) = fs::symlink_metadata(dir) {
        if meta.file_type().is_symlink() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("cache dir {} is a symlink", dir.display()),
            ));
        }
    }
    fs::create_dir_all(dir)?;
    crate::private_root::ensure_private_root("ACOS_CACHE_DIR", dir).map_err(|(code, msg)| {
        io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("{}: {msg}", code.code()),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> TempDir {
            let mut path = std::env::temp_dir();
            path.push(format!(
                "acos-cache-test-{}-{}-{}",
                tag,
                std::process::id(),
                next_nonce()
            ));
            fs::create_dir_all(&path).expect("temp dir");
            TempDir(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn cache(tag: &str) -> (TempDir, BootImageCache) {
        let dir = TempDir::new(tag);
        let cache = BootImageCache::with_dir(dir.path()).expect("cache");
        (dir, cache)
    }

    fn fake_boot_dir(tag: &str, jar: &str, payload: &[u8]) -> TempDir {
        let dir = TempDir::new(tag);
        fs::write(dir.path().join("BOOTCLASSPATH"), format!("{jar}\n")).unwrap();
        let dexdir = dir.path().join("dex").join(jar.trim_end_matches(".jar"));
        fs::create_dir_all(&dexdir).unwrap();
        fs::write(dexdir.join("classes.dex"), payload).unwrap();
        dir
    }

    #[test]
    fn round_trip_and_atomic_publish() {
        let (_dir, cache) = cache("roundtrip");
        let payload = b"ACOSSNAP fake payload".to_vec();
        cache.store(0x1234, &payload).expect("store");
        match cache.load(0x1234) {
            LoadResult::Hit(got) => assert_eq!(got, payload),
            other => panic!("expected hit, got {other:?}"),
        }
        // No temp files left behind.
        let leftovers: Vec<_> = fs::read_dir(cache.dir())
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with(".tmp-"))
            .collect();
        assert!(leftovers.is_empty(), "temp files left: {leftovers:?}");
    }

    #[test]
    fn miss_for_unknown_key() {
        let (_dir, cache) = cache("miss");
        assert!(matches!(cache.load(7), LoadResult::Miss));
    }

    #[test]
    fn corrupt_payload_is_a_miss() {
        let (_dir, cache) = cache("corrupt");
        cache.store(1, b"hello payload").unwrap();
        let path = cache.entry_path(1);
        let mut bytes = fs::read(&path).unwrap();
        let last = bytes.len() - 1;
        bytes[last] ^= 0xff;
        fs::write(&path, &bytes).unwrap();
        assert!(matches!(cache.load(1), LoadResult::Invalid(_)));
    }

    #[test]
    fn truncated_and_oversized_are_misses() {
        let (_dir, cache) = cache("sizes");
        cache.store(2, b"0123456789").unwrap();
        let path = cache.entry_path(2);
        let bytes = fs::read(&path).unwrap();
        // Truncate the payload.
        fs::write(&path, &bytes[..bytes.len() - 4]).unwrap();
        assert!(matches!(cache.load(2), LoadResult::Invalid(_)));
        // Restore, then append a trailing byte (length mismatch).
        fs::write(&path, &bytes).unwrap();
        let mut grown = bytes.clone();
        grown.push(0);
        fs::write(&path, &grown).unwrap();
        assert!(matches!(cache.load(2), LoadResult::Invalid(_)));
    }

    #[test]
    fn wrong_key_version_and_magic_are_refused() {
        let (_dir, cache) = cache("header");
        cache.store(3, b"payload").unwrap();
        // Wrong key is queried on a valid file.
        assert!(matches!(cache.load(4), LoadResult::Miss));
        let path = cache.entry_path(3);
        let mut bytes = fs::read(&path).unwrap();
        // Corrupt the magic.
        bytes[0] ^= 0xff;
        fs::write(&path, &bytes).unwrap();
        assert!(matches!(cache.load(3), LoadResult::Invalid(_)));
        // Corrupt the recorded version string byte.
        bytes[0] ^= 0xff;
        let ver_off = HEADER_LEN;
        bytes[ver_off] = b'X';
        fs::write(&path, &bytes).unwrap();
        assert!(matches!(cache.load(3), LoadResult::Invalid(_)));
    }

    #[test]
    fn concurrent_writers_publish_a_valid_entry() {
        use std::sync::Arc;
        let (dir, cache) = cache("concurrent");
        let cache = Arc::new(cache);
        let payload = b"concurrent payload".to_vec();
        let mut handles = Vec::new();
        for _ in 0..8 {
            let cache = Arc::clone(&cache);
            let payload = payload.clone();
            handles.push(std::thread::spawn(move || {
                cache.store(0xabc, &payload).expect("store");
            }));
        }
        for h in handles {
            h.join().expect("thread");
        }
        match cache.load(0xabc) {
            LoadResult::Hit(got) => assert_eq!(got, payload),
            other => panic!("expected hit, got {other:?}"),
        }
        // The directory/entry survive the concurrent renames.
        assert!(dir.path().join(format!("{:016x}.img", 0xabc)).is_file());
    }

    #[test]
    fn lock_is_exclusive_and_publish_is_safe_without_it() {
        let (_dir, cache) = cache("lock");
        let lock = cache.lock(9).expect("first lock");
        assert!(cache.lock(9).is_none(), "second lock must not be granted");
        drop(lock);
        assert!(cache.lock(9).is_some(), "lock released on drop");
    }

    #[test]
    fn key_changes_with_boot_dir_content() {
        let dir = fake_boot_dir("keychange", "core-oj.jar", b"first");
        let key1 = image_key(Some(dir.path()), CoreLib::Real);
        let key2 = image_key(Some(dir.path()), CoreLib::Real);
        assert_eq!(key1, key2, "key must be deterministic");
        // Change the dex content (and its mtime).
        std::thread::sleep(Duration::from_millis(1100));
        let dex = dir.path().join("dex/core-oj/classes.dex");
        fs::write(&dex, b"second-larger").unwrap();
        let key3 = image_key(Some(dir.path()), CoreLib::Real);
        assert_ne!(key1, key3, "boot dir content change must change the key");
        // A different corelib is a different key.
        assert_ne!(key1, image_key(Some(dir.path()), CoreLib::Bootstrap));
    }

    #[test]
    fn eviction_keeps_the_directory_bounded() {
        let dir = TempDir::new("evict");
        let cache = BootImageCache::new(dir.path().to_path_buf(), 200).expect("cache");
        for key in 0..5u64 {
            cache.store(key, &[0u8; 100]).unwrap();
            // Distinct mtimes so the LRU order is well-defined.
            std::thread::sleep(Duration::from_millis(5));
        }
        let total: u64 = fs::read_dir(cache.dir())
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().ends_with(".img"))
            .flat_map(|e| e.metadata().ok())
            .map(|m| m.len())
            .sum();
        assert!(total <= 200, "cache exceeded budget: {total}");
    }

    #[test]
    #[cfg(unix)]
    fn symlinked_cache_dir_is_rejected() {
        use std::os::unix::fs::symlink;
        let dir = TempDir::new("symlink");
        let link = dir.path().join("cache-link");
        symlink(dir.path(), &link).unwrap();
        // `ensure_private_dir` must refuse the symlinked root, so the cache is
        // disabled rather than following an attacker-controlled link.
        assert!(BootImageCache::with_dir(&link).is_err());
    }

    /// W170-A E1: another local user must not be able to pre-seed an entry under
    /// a non-secret key, so a group/world-writable `ACOS_CACHE_DIR` is a hard
    /// error (not a silent `chmod`).
    #[test]
    #[cfg(unix)]
    fn writable_by_others_cache_dir_is_rejected() {
        use std::os::unix::fs::PermissionsExt;
        for mode in [0o777u32, 0o770, 0o775] {
            let dir = TempDir::new("shared");
            fs::set_permissions(dir.path(), fs::Permissions::from_mode(mode)).unwrap();
            let err = BootImageCache::with_dir(dir.path()).expect_err("shared cache dir");
            let msg = err.to_string();
            assert!(
                msg.contains("ROOT_WRITABLE_BY_OTHERS") && msg.contains("ACOS_CACHE_DIR"),
                "mode {mode:o} must be rejected with a structured code: {msg}"
            );
            fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700)).unwrap();
        }
        // A plain 0755 root is tightened, not rejected (developer checkouts).
        let dir = TempDir::new("tighten");
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o755)).unwrap();
        let cache = BootImageCache::with_dir(dir.path()).expect("0755 is tightened");
        assert_eq!(cache.dir(), dir.path());
        let mode = fs::metadata(dir.path()).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700);
    }

    #[test]
    #[cfg(unix)]
    fn non_directory_cache_root_is_rejected() {
        let dir = TempDir::new("notdir");
        let file = dir.path().join("not-a-dir");
        fs::write(&file, b"x").unwrap();
        assert!(BootImageCache::with_dir(&file).is_err());
    }

    #[test]
    fn private_directory_permissions() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let dir = TempDir::new("perms");
            let cache = BootImageCache::with_dir(dir.path()).expect("cache");
            let mode = fs::metadata(cache.dir()).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o700, "cache dir must be private, got {mode:o}");
        }
    }
}
