//! Process-global cache of **parsed boot-classpath DEX files**.
//!
//! The Android boot classpath (core-oj / core-libart / core-icu4j and the
//! remaining lazy jars) is immutable for the life of a process. ACOS sessions
//! in one process therefore share one [`DexFile`] per file instead of each
//! reading and parsing it again: the raw DEX bytes (`Arc<[u8]>`), the id tables,
//! the lazily-materialised string table and the lazily-built class index all
//! become shared read-only data.
//!
//! Keys are the file **fingerprint** `(canonical path, size, mtime)`, not a
//! content hash, so a cache hit costs one `stat` and never re-reads the multi-
//! megabyte DEX. Within one process the boot files do not change; a caller that
//! rewrites a boot file between loads simply gets a fresh entry (the old
//! `Arc<DexFile>` stays alive for its existing owners, which is what a
//! copy-on-write cache must do).
//!
//! The cache is `#![forbid(unsafe_code)]`: a `Mutex<HashMap<..>>` behind a
//! `OnceLock` is enough because boot parsing is not on a hot path. Parsing is
//! done **outside** the lock so concurrent first loads do not serialise on IO;
//! the loser of a race discards its duplicate parse and shares the winner's.
//!
//! Unused classes cost nothing: this cache only memoises files that a session
//! actually asks for. The lazy boot jars are still parsed only on the first
//! class miss that needs them (see `Vm::try_load_lazy_boot_jar`).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use crate::DexFile;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct DexKey {
    path: PathBuf,
    len: u64,
    mtime_ns: u128,
}

fn file_key(path: &Path) -> std::io::Result<DexKey> {
    let metadata = std::fs::metadata(path)?;
    let mtime_ns = metadata
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    // `canonicalize` gives one identity for `a/../b` and `b`; fall back to the
    // given path if the filesystem cannot resolve it.
    let canonical = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    Ok(DexKey {
        path: canonical,
        len: metadata.len(),
        mtime_ns,
    })
}

/// Counters for one [`BootCache`] (process-local; never a claim about another
/// process).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CacheStats {
    /// Files currently held as parsed `DexFile`s.
    pub entries: usize,
    /// Lookups served from the cache (no file read).
    pub hits: u64,
    /// Files actually read + parsed by this cache.
    pub loads: u64,
}

/// A cache of parsed DEX files keyed by file fingerprint.
#[derive(Debug, Default)]
pub struct BootCache {
    map: Mutex<HashMap<DexKey, Arc<DexFile>>>,
    hits: AtomicU64,
    loads: AtomicU64,
}

impl BootCache {
    pub fn new() -> BootCache {
        BootCache::default()
    }

    /// Return the shared parsed form of `path`, reading and parsing it only on
    /// the first call for that fingerprint.
    pub fn get_or_load(&self, path: &Path) -> Result<Arc<DexFile>, String> {
        let key = file_key(path).map_err(|e| format!("{}: {e}", path.display()))?;
        if let Some(file) = self
            .map
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&key)
            .cloned()
        {
            self.hits.fetch_add(1, Ordering::Relaxed);
            return Ok(file);
        }

        // Read + parse outside the lock: a boot DEX is several MiB.
        let bytes =
            std::fs::read(path).map_err(|e| format!("read {}: {e}", path.display()))?;
        let parsed = DexFile::parse(bytes)
            .map_err(|e| format!("parse {}: {e}", path.display()))?;
        let parsed = Arc::new(parsed);
        self.loads.fetch_add(1, Ordering::Relaxed);

        let mut map = self.map.lock().unwrap_or_else(|e| e.into_inner());
        // If another thread won the race, share its parse (identical bytes,
        // identical fingerprint) instead of keeping the duplicate.
        Ok(map.entry(key).or_insert(parsed).clone())
    }

    /// Return the shared parsed form of `path`, rebuilding it from serialized
    /// [`crate::DexTables`] when the file is not already resident.
    ///
    /// This is the cold-process path for the boot-image cache: the caller has a
    /// `DexTables` snapshot from the same boot-classpath fingerprint and only
    /// needs the raw bytes re-read. Any table failure (version, size, magic) is
    /// a transparent fallback to a full [`DexFile::parse`]; the cache never
    /// installs a half-validated file.
    pub fn get_or_load_with_tables(
        &self,
        path: &Path,
        tables: Option<crate::DexTables>,
    ) -> Result<Arc<DexFile>, String> {
        let key = file_key(path).map_err(|e| format!("{}: {e}", path.display()))?;
        if let Some(file) = self
            .map
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&key)
            .cloned()
        {
            self.hits.fetch_add(1, Ordering::Relaxed);
            return Ok(file);
        }

        let bytes = std::fs::read(path).map_err(|e| format!("read {}: {e}", path.display()))?;
        let raw: Arc<[u8]> = bytes.into();
        let parsed = match tables {
            Some(tables) => match DexFile::from_tables(tables, raw.clone()) {
                Ok(file) => file,
                Err(_) => DexFile::parse(raw)
                    .map_err(|e| format!("parse {}: {e}", path.display()))?,
            },
            None => DexFile::parse(raw).map_err(|e| format!("parse {}: {e}", path.display()))?,
        };
        let parsed = Arc::new(parsed);
        self.loads.fetch_add(1, Ordering::Relaxed);

        let mut map = self.map.lock().unwrap_or_else(|e| e.into_inner());
        Ok(map.entry(key).or_insert(parsed).clone())
    }

    /// Number of parsed files currently resident.
    pub fn len(&self) -> usize {
        self.map
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn stats(&self) -> CacheStats {
        CacheStats {
            entries: self.len(),
            hits: self.hits.load(Ordering::Relaxed),
            loads: self.loads.load(Ordering::Relaxed),
        }
    }

    /// Drop every entry. Existing `Arc` owners keep their parsed file alive.
    pub fn clear(&self) {
        self.map
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
    }
}

static GLOBAL: OnceLock<BootCache> = OnceLock::new();

/// The process-global boot-classpath cache.
pub fn boot_cache() -> &'static BootCache {
    GLOBAL.get_or_init(BootCache::new)
}

/// Shorthand for [`boot_cache().get_or_load(path)`](BootCache::get_or_load).
pub fn shared_dex(path: &Path) -> Result<Arc<DexFile>, String> {
    boot_cache().get_or_load(path)
}
