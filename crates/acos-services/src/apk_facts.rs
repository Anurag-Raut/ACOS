//! One APK open per launch facts set (PERF-REL3b).
//!
//! An ART launch asked for the APK's manifest facts several times: the launcher
//! component (twice), the manifest labels and the resume budget, each through its
//! own `Apk::open`. An open reads the whole file, copies every dex and indexes the
//! zip, so on the Gaten debug APK (115 MB) each open costs ~62 ms and one launch
//! paid ~310 ms, against ~0.15 ms for a fixture APK
//! (`tests/manifest_launch_timing.rs`, `results/handoff/PERF-REL3.md` §3b).
//!
//! [`launch_facts`] opens and parses an APK once and keeps the derived facts in a
//! process-wide cache keyed by the path. The entry is valid while the file's
//! length and modification time are unchanged: a stat, not a hash, because
//! hashing the APK would cost as much as the open. The cache keeps the facts,
//! never the APK bytes, so its memory is a few kilobytes per APK. Failed opens are
//! not cached.
//!
//! The stat happens before the open. If the file changes in between, the cached
//! facts carry the older key, so the next call misses and re-reads: a stale entry
//! cannot outlive the change it did not see.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::SystemTime;

use crate::manifest_resolve::{
    budget_of, resolve_from_dom, LaunchBudget, LauncherActivity, LauncherError,
};
use crate::pms::{labels_from_manifest, ManifestLabels};

/// Everything the launch path reads from one APK's zip index and manifest.
#[derive(Debug)]
pub struct LaunchFacts {
    /// Dex count and native closure size; the resume budget follows from them.
    pub budget: LaunchBudget,
    /// `launcher_activity` result (`PackageParser` MAIN/LAUNCHER rules).
    pub launcher: Result<LauncherActivity, LauncherError>,
    /// `manifest_labels` result. The error is the `manifest: …` text.
    pub labels: Result<ManifestLabels, String>,
}

/// File identity used to validate a cache entry (stat only, no hash).
#[derive(Debug, Clone, PartialEq, Eq)]
struct FileKey {
    len: u64,
    modified: Option<SystemTime>,
}

#[derive(Debug)]
struct Entry {
    key: FileKey,
    facts: Arc<LaunchFacts>,
}

/// Entries kept before the cache is cleared. A launch touches one or two APKs;
/// the cap only bounds a long-running daemon that sees many distinct paths.
const CACHE_CAP: usize = 64;

static CACHE: OnceLock<Mutex<HashMap<PathBuf, Entry>>> = OnceLock::new();
static OPENS: AtomicU64 = AtomicU64::new(0);

fn cache() -> &'static Mutex<HashMap<PathBuf, Entry>> {
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn file_key(path: &Path) -> Option<FileKey> {
    let meta = std::fs::metadata(path).ok()?;
    Some(FileKey {
        len: meta.len(),
        modified: meta.modified().ok(),
    })
}

/// The launch facts of `path`, opened and parsed at most once per (path, length,
/// mtime). The error is the `Apk::open` message; each caller adds its own context.
pub fn launch_facts(path: &Path) -> Result<Arc<LaunchFacts>, String> {
    // Stat before the open: see the module doc on why this ordering is safe.
    let key = file_key(path);
    if let Some(key) = &key {
        if let Ok(map) = cache().lock() {
            if let Some(entry) = map.get(path) {
                if &entry.key == key {
                    return Ok(Arc::clone(&entry.facts));
                }
            }
        }
    }
    let facts = Arc::new(read_facts(path)?);
    if let Some(key) = key {
        if let Ok(mut map) = cache().lock() {
            if map.len() >= CACHE_CAP && !map.contains_key(path) {
                map.clear();
            }
            map.insert(
                path.to_path_buf(),
                Entry {
                    key,
                    facts: Arc::clone(&facts),
                },
            );
        }
    }
    Ok(facts)
}

fn read_facts(path: &Path) -> Result<LaunchFacts, String> {
    OPENS.fetch_add(1, Ordering::Relaxed);
    let apk = acos_apk::Apk::open(path).map_err(|e| e.to_string())?;
    let budget = budget_of(&apk);
    // One AXML parse feeds both consumers; `Apk::manifest()` would parse it twice.
    let dom = apk.manifest_dom();
    let launcher = match &dom {
        Ok(doc) => resolve_from_dom(doc),
        Err(e) => Err(LauncherError::Manifest(format!("{}: {e}", path.display()))),
    };
    let labels = match &dom {
        Ok(doc) => acos_apk::manifest::Manifest::parse(doc)
            .map(|m| labels_from_manifest(&m))
            .map_err(|e| format!("manifest: {e}")),
        Err(e) => Err(format!("manifest: {e}")),
    };
    Ok(LaunchFacts {
        budget,
        launcher,
        labels,
    })
}

/// How many times an APK was actually opened by [`launch_facts`] in this process
/// (cache misses). Used by the cache tests and the timing harness.
#[doc(hidden)]
pub fn open_count() -> u64 {
    OPENS.load(Ordering::Relaxed)
}

/// Drop every cached entry, so the next call is a cold read. For the timing
/// harness and tests; production code never needs it.
#[doc(hidden)]
pub fn clear_cache() {
    if let Ok(mut map) = cache().lock() {
        map.clear();
    }
}
