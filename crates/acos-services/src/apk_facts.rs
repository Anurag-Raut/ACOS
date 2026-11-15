//! One APK open per launch (PERF-REL3b, PERF-REL3c).
//!
//! An ART launch read the same APK through several independent `Apk::open`
//! calls: the manifest facts (launcher component, labels, resume budget) and the
//! native-library install, each reading the whole file, copying every dex and
//! indexing the zip. On the Gaten release APK (115 MB) each open costs ~61 ms and
//! the launch paid three of them (`tests/manifest_launch_timing.rs`,
//! `results/handoff/PERF-REL3.md` §3b and §3c).
//!
//! Two process-wide caches fix that:
//!
//! * [`open_shared`] opens an APK and returns it as an `Arc<Apk>`. It holds only a
//!   `Weak` reference, so the bytes live exactly as long as a caller keeps the
//!   `Arc` (one launch) and are never retained by the cache. A second call while
//!   the first `Arc` is alive, for the same path and file identity, returns that
//!   same open instead of reading the file again.
//! * [`launch_facts`] keeps the derived facts (budget, launcher, labels) per path.
//!   The facts are small (kilobytes per APK), never the APK bytes. A launch that
//!   already holds the open calls [`prime_from`] so the facts are cached without a
//!   second open.
//!
//! Both are valid while the file's length and modification time are unchanged: a
//! stat, not a hash, because hashing the APK would cost as much as the open. Failed
//! opens are not cached. The stat happens before the open. If the file changes in
//! between, the entry carries the older key, so the next call misses and re-reads:
//! a stale entry cannot outlive the change it did not see.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::SystemTime;

use acos_apk::Apk;

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

/// The last shared open of a path: its identity from before the open, and a weak
/// handle to the open itself.
#[derive(Debug)]
struct SharedSlot {
    key: Option<FileKey>,
    apk: Weak<Apk>,
}

/// Entries kept before the cache is cleared. A launch touches one or two APKs;
/// the cap only bounds a long-running daemon that sees many distinct paths.
const CACHE_CAP: usize = 64;

static CACHE: OnceLock<Mutex<HashMap<PathBuf, Entry>>> = OnceLock::new();
static SHARED: OnceLock<Mutex<HashMap<PathBuf, SharedSlot>>> = OnceLock::new();
static OPENS: AtomicU64 = AtomicU64::new(0);

fn cache() -> &'static Mutex<HashMap<PathBuf, Entry>> {
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn shared() -> &'static Mutex<HashMap<PathBuf, SharedSlot>> {
    SHARED.get_or_init(|| Mutex::new(HashMap::new()))
}

fn file_key(path: &Path) -> Option<FileKey> {
    let meta = std::fs::metadata(path).ok()?;
    Some(FileKey {
        len: meta.len(),
        modified: meta.modified().ok(),
    })
}

/// Open `path`, or return the live open of it that another caller in this process
/// still holds, when the file identity is unchanged. The returned `Arc` keeps the
/// APK bytes alive; the cache holds only a `Weak`, so dropping the last `Arc`
/// frees them. The error is the `Apk::open` message; each caller adds its context.
pub fn open_shared(path: &Path) -> Result<Arc<Apk>, String> {
    // Stat before the open: see the module doc on why this ordering is safe.
    let key = file_key(path);
    if let Some(key) = &key {
        if let Ok(map) = shared().lock() {
            if let Some(slot) = map.get(path) {
                if slot.key.as_ref() == Some(key) {
                    if let Some(apk) = slot.apk.upgrade() {
                        return Ok(apk);
                    }
                }
            }
        }
    }
    OPENS.fetch_add(1, Ordering::Relaxed);
    let apk = Arc::new(Apk::open(path).map_err(|e| e.to_string())?);
    if let Ok(mut map) = shared().lock() {
        if map.len() >= CACHE_CAP && !map.contains_key(path) {
            map.clear();
        }
        map.insert(
            path.to_path_buf(),
            SharedSlot {
                key,
                apk: Arc::downgrade(&apk),
            },
        );
    }
    Ok(apk)
}

/// The launch facts of `path`, computed at most once per (path, length, mtime).
/// The error is the `Apk::open` message; each caller adds its own context.
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
    let apk = open_shared(path)?;
    let facts = Arc::new(facts_of(path, &apk));
    if let Some(key) = key {
        cache_insert(path, key, Arc::clone(&facts));
    }
    Ok(facts)
}

/// Cache the facts of `apk`, an open of `path` the caller already holds (from
/// [`open_shared`]), so later [`launch_facts`] calls for `path` are cache hits and
/// do not open the APK again. A no-op when `apk` is not the live shared open of
/// `path` (the identity is then unknown, so nothing is cached).
pub fn prime_from(path: &Path, apk: &Arc<Apk>) {
    let Some(key) = live_key_of(path, apk) else {
        return;
    };
    if let Ok(map) = cache().lock() {
        if let Some(entry) = map.get(path) {
            if entry.key == key {
                return;
            }
        }
    }
    let facts = Arc::new(facts_of(path, apk));
    cache_insert(path, key, facts);
}

/// The identity recorded when `apk` was opened, if `apk` is still the live shared
/// open of `path`.
fn live_key_of(path: &Path, apk: &Arc<Apk>) -> Option<FileKey> {
    let map = shared().lock().ok()?;
    let slot = map.get(path)?;
    let live = slot.apk.upgrade()?;
    if !Arc::ptr_eq(&live, apk) {
        return None;
    }
    slot.key.clone()
}

fn cache_insert(path: &Path, key: FileKey, facts: Arc<LaunchFacts>) {
    if let Ok(mut map) = cache().lock() {
        if map.len() >= CACHE_CAP && !map.contains_key(path) {
            map.clear();
        }
        map.insert(path.to_path_buf(), Entry { key, facts });
    }
}

/// Derive the launch facts from an APK that is already open. Does not open it.
fn facts_of(path: &Path, apk: &Apk) -> LaunchFacts {
    let budget = budget_of(apk);
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
    LaunchFacts {
        budget,
        launcher,
        labels,
    }
}

/// How many times an APK was actually opened through this module in this process
/// (shared-open misses). Used by the cache tests and the timing harness.
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
    if let Ok(mut map) = shared().lock() {
        map.clear();
    }
}
