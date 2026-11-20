//! Provenance for `verify` and `launch` replies: *which* inputs produced this
//! verdict, not just *what* the verdict says.
//!
//! The feedback that created this module (2026-11-14/15 entries): a mistyped
//! scenario path ran an older scenario against a stale APK and returned
//! `pass`, and neither the envelope `result` nor the human summary line named
//! the scenario or the APK — only `repro.json` did. A verdict that does not
//! say which bytes it judged cannot be trusted, and a reader should never have
//! to open a second file to find out.
//!
//! One shape, used by every front door (CLI `verify`/`launch`, the `acos serve`
//! daemon's `acos_launch`, MCP `acos_launch`) and written into `verdict.json`:
//!
//! ```json
//! "provenance": {
//!   "scenario": {"path": "/abs/scenario.yaml", "sha256": "<64 hex>", "sha256_12": "<12 hex>"},
//!   "apk":      {"path": "/abs/app.apk",      "sha256": "<64 hex>", "sha256_12": "<12 hex>"},
//!   "engine": "art",
//!   "fidelity": "art-reference",
//!   "warnings": [{"code": "SCENARIO_APP_OVERRIDDEN", "message": "...", ...}]
//! }
//! ```
//!
//! `sha256` is the full digest (the evidence); `sha256_12` is the display form
//! every human summary line uses (the same 12-character prefix an agent quotes
//! when it reports a run). Paths are absolute, because a relative path means a
//! different APK to every reader.
//!
//! `launch` has no scenario; its `scenario` field is `null` rather than absent,
//! so a reader can branch on one shape.

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::envelope::Fidelity;

/// How many hex characters of a SHA-256 a summary line shows.
pub const SHA_PREFIX_LEN: usize = 12;

/// A warning code: the scenario's own `app:` was overridden by `--apk`.
pub const WARN_SCENARIO_APP_OVERRIDDEN: &str = "SCENARIO_APP_OVERRIDDEN";

/// A warning code: the scenario declares `engine:` but the run used another.
pub const WARN_SCENARIO_ENGINE_MISMATCH: &str = "SCENARIO_ENGINE_MISMATCH";

/// A warning code: an input's size/mtime changed while the run was in flight.
pub const WARN_INPUT_CHANGED: &str = "INPUT_CHANGED_DURING_RUN";

/// A warning code: an input step's gesture was delivered and the app window
/// acknowledged it `finishInputEvent(handled) = false`, so nothing consumed it.
///
/// REL3H made that a *settled* observation (`settle.reason: "unhandled_input"`)
/// instead of a timeout, and REL3H2 adds this warning because the new pass was
/// silent: an agent that reads only `status` could not tell "the tap worked"
/// from "nothing was there to tap". The step stays exactly as measured (a pass
/// when the step made no change claim), and the warning names the step and the
/// next action. A scenario that *knows* an input is a no-op can opt out with
/// `expect_handled: false`; `expect_handled: true` turns it into a step failure
/// (`INPUT_NOT_HANDLED`).
pub const WARN_INPUT_UNHANDLED: &str = "INPUT_UNHANDLED";

/// The absolute form of `path`.
///
/// Canonicalizes when the path exists (symlinks and `..` resolved, so two
/// spellings of one file compare equal), otherwise joins the process cwd and
/// normalizes the `.`/`..` components lexically. The result is always absolute:
/// a relative path in provenance is ambiguous the moment it is read.
pub fn absolute(path: &Path) -> PathBuf {
    if let Ok(canonical) = std::fs::canonicalize(path) {
        return canonical;
    }
    let joined = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("/"))
            .join(path)
    };
    let mut normalized = PathBuf::new();
    for component in joined.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                // Keep the leading `.` component of a relative path from
                // escaping; the cwd join above means this only trims a real
                // segment (or `/`).
                normalized.pop();
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    if normalized.as_os_str().is_empty() {
        PathBuf::from("/")
    } else {
        normalized
    }
}

/// One input file captured **before** the run: identity for the provenance
/// block, plus the `size`/`mtime` a post-run check can compare.
///
/// The digest is taken once. Re-reading the file after the run to hash it again
/// would (a) pay the hash twice and (b) hash *different bytes* than the ones a
/// backend may have read, so the block would describe a file the run never saw.
/// `size` and `mtime_ns` are kept for [`InputFile::changed_after`], which is how
/// a run notices that an input was rewritten under it (`INPUT_CHANGED_DURING_RUN`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputFile {
    /// Absolute path (canonical when the file exists).
    pub path: PathBuf,
    /// Full SHA-256, lowercase hex (`None` when the file could not be read).
    pub sha256: Option<String>,
    /// Byte size at capture time (0 when unknown).
    pub size_bytes: u64,
    /// Modification time in nanoseconds since the Unix epoch (0 when unknown).
    pub mtime_ns: u128,
    /// Device/inode pair of the file that was hashed (0 when unknown).
    pub inode: u64,
}

impl InputFile {
    /// Capture a file's identity, reusing a cached digest when the file has not
    /// changed since the last capture in this process.
    #[must_use]
    pub fn capture(path: &Path) -> InputFile {
        let absolute = absolute(path);
        let stat = std::fs::metadata(&absolute).ok();
        let size_bytes = stat.as_ref().map_or(0, std::fs::Metadata::len);
        let mtime_ns = stat
            .as_ref()
            .and_then(|metadata| metadata.modified().ok())
            .and_then(|modified| {
                modified
                    .duration_since(std::time::UNIX_EPOCH)
                    .ok()
                    .map(|since| since.as_nanos())
            })
            .unwrap_or(0);
        #[cfg(unix)]
        let inode = {
            use std::os::unix::fs::MetadataExt as _;
            stat.as_ref().map_or(0, std::fs::Metadata::ino)
        };
        #[cfg(not(unix))]
        let inode = 0u64;
        let sha256 = cached_sha256(&absolute, size_bytes, mtime_ns, inode);
        InputFile {
            path: absolute,
            sha256,
            size_bytes,
            mtime_ns,
            inode,
        }
    }

    /// The provenance object for this input.
    #[must_use]
    pub fn to_json(&self) -> Value {
        json!({
            "path": self.path.display().to_string(),
            "sha256": self.sha256,
            "sha256_12": self.sha256.as_deref().map(short_sha),
            "size_bytes": self.size_bytes,
            "mtime_ns": self.mtime_ns,
        })
    }

    /// `Some(reason)` when the file on disk is no longer the bytes this capture
    /// describes (size, mtime or inode differ, or the file is gone).
    #[must_use]
    pub fn changed_after(&self) -> Option<String> {
        let metadata = match std::fs::metadata(&self.path) {
            Ok(metadata) => metadata,
            Err(error) => return Some(format!("the file is no longer readable ({error})")),
        };
        if metadata.len() != self.size_bytes {
            return Some(format!(
                "size changed from {} to {} bytes",
                self.size_bytes,
                metadata.len()
            ));
        }
        let mtime_ns = metadata
            .modified()
            .ok()
            .and_then(|modified| {
                modified
                    .duration_since(std::time::UNIX_EPOCH)
                    .ok()
                    .map(|since| since.as_nanos())
            })
            .unwrap_or(0);
        if mtime_ns != self.mtime_ns {
            return Some(format!(
                "mtime changed from {} to {mtime_ns} ns since the epoch",
                self.mtime_ns
            ));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt as _;
            if metadata.ino() != self.inode {
                return Some(format!(
                    "the path now names inode {} (was {})",
                    metadata.ino(),
                    self.inode
                ));
            }
        }
        None
    }
}

/// The identity of both inputs of a run, captured before anything executes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Inputs {
    /// The APK that will be booted.
    pub apk: InputFile,
    /// The scenario, when the run has one.
    pub scenario: Option<InputFile>,
}

impl Inputs {
    /// Capture both inputs (one file read each, digest cached per process).
    #[must_use]
    pub fn capture(apk: &Path, scenario: Option<&Path>) -> Inputs {
        Inputs {
            apk: InputFile::capture(apk),
            scenario: scenario.map(InputFile::capture),
        }
    }

    /// `INPUT_CHANGED_DURING_RUN` warnings for every input whose size/mtime no
    /// longer matches the capture taken before the run.
    ///
    /// A run that read a file which was then rewritten cannot claim its verdict
    /// describes the file now on disk; the verdict keeps the status it measured
    /// and says so, instead of reporting a `pass` for bytes that no longer exist.
    #[must_use]
    pub fn changed_warnings(&self) -> Vec<Value> {
        let mut warnings = Vec::new();
        for (role, input) in [
            ("apk", Some(&self.apk)),
            ("scenario", self.scenario.as_ref()),
        ]
        .into_iter()
        .filter_map(|(role, input)| input.map(|input| (role, input)))
        {
            if let Some(reason) = input.changed_after() {
                warnings.push(json!({
                    "code": WARN_INPUT_CHANGED,
                    "message": format!(
                        "the {role} {} changed while this run was in flight ({reason}). The \
                         verdict describes the bytes hashed before the run (sha256_12 {}); the \
                         file on disk is no longer those bytes, so rerun before trusting this \
                         evidence.",
                        input.path.display(),
                        input.sha256.as_deref().map(short_sha).unwrap_or("n/a"),
                    ),
                    "input": role,
                    "path": input.path.display().to_string(),
                    "sha256_12": input.sha256.as_deref().map(short_sha),
                    "change": reason,
                }));
            }
        }
        warnings
    }
}

/// Per-process digest cache: `(canonical path, size, mtime_ns, inode)` -> digest.
///
/// A cold SHA-256 of a 206 MB APK costs ~90 ms (warm page cache) and it was paid
/// on **every** launch, including the second and third launch of the same bytes
/// in one long-lived MCP server or `acos serve` daemon. The key is the file's
/// identity, not its path, so a rebuilt APK at the same path is re-hashed. A
/// process-wide cache (not a disk cache) is enough: the recurring cost lives in
/// long-lived processes; a one-shot CLI invocation pays one hash, ~1% of a
/// multi-second launch, and a disk cache would add a stale-digest failure mode
/// for that 1%.
fn digest_cache() -> &'static Mutex<HashMap<(PathBuf, u64, u128, u64), String>> {
    static CACHE: OnceLock<Mutex<HashMap<(PathBuf, u64, u128, u64), String>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// SHA-256 of `path`, reusing a digest for the same (size, mtime, inode).
fn cached_sha256(path: &Path, size: u64, mtime_ns: u128, inode: u64) -> Option<String> {
    let key = (path.to_path_buf(), size, mtime_ns, inode);
    if let Ok(cache) = digest_cache().lock() {
        if let Some(sha) = cache.get(&key) {
            return Some(sha.clone());
        }
    }
    let sha = sha256_file(path).ok()?;
    if let Ok(mut cache) = digest_cache().lock() {
        // Small and bounded: one entry per distinct file identity, and an APK
        // tree is a handful of files. Clear rather than grow without bound if a
        // caller hashes a huge number of distinct files.
        if cache.len() >= 64 {
            cache.clear();
        }
        cache.insert(key, sha.clone());
    }
    Some(sha)
}

/// SHA-256 of a file, lowercase hex.
///
/// This is the **uncached** digest (used by tests and by callers that hold the
/// bytes); [`InputFile::capture`] is the caching path a run uses.
pub fn sha256_file(path: &Path) -> std::io::Result<String> {
    use std::io::Read;
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 1024 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

/// The first [`SHA_PREFIX_LEN`] characters of a digest (the whole digest when
/// it is shorter than that, so a caller never panics on a non-SHA string).
pub fn short_sha(sha: &str) -> &str {
    let end = sha
        .char_indices()
        .nth(SHA_PREFIX_LEN)
        .map(|(index, _)| index)
        .unwrap_or(sha.len());
    &sha[..end]
}

/// One input file: absolute path, full SHA-256 and its display prefix.
///
/// A file that cannot be read keeps its absolute path and `null` digests: the
/// reply still says *which* file was meant (and the run itself will have
/// failed on it), instead of silently omitting the input.
pub fn file(path: &Path) -> Value {
    InputFile::capture(path).to_json()
}

/// A structured, non-fatal warning for a run.
pub fn warning(code: &str, message: impl Into<String>) -> Value {
    json!({"code": code, "message": message.into()})
}

/// The provenance block for one run.
///
/// `engine` and `fidelity` come from the caller because they are facts about
/// the execution, not about the inputs: the CLI records the engine that
/// actually ran (never a guess), and the verifier records the fidelity it
/// achieved.
pub fn block(apk: &Path, scenario: Option<&Path>, engine: &str, fidelity: Fidelity) -> Value {
    Inputs::capture(apk, scenario).block(engine, fidelity)
}

impl Inputs {
    /// The provenance block for a run whose inputs were captured **before** it
    /// executed (the shape every front door publishes).
    #[must_use]
    pub fn block(&self, engine: &str, fidelity: Fidelity) -> Value {
        json!({
            "scenario": self.scenario.as_ref().map(InputFile::to_json),
            "apk": self.apk.to_json(),
            "engine": engine,
            "fidelity": fidelity.as_str(),
            "warnings": <Vec<Value>>::new(),
        })
    }
}

/// Append one warning to a provenance block (no-op on a malformed block).
pub fn push_warning(block: &mut Value, warning: Value) {
    match block.get_mut("warnings").and_then(Value::as_array_mut) {
        Some(warnings) => warnings.push(warning),
        None => {
            if let Some(object) = block.as_object_mut() {
                object.insert("warnings".to_string(), json!([warning]));
            }
        }
    }
}

/// The one-line provenance a human summary prints.
///
/// Shape: `scenario <abs> (sha <12>), apk <abs> (sha <12>), engine <e>,
/// fidelity <f>`. Missing inputs say `-`; a warning count is appended when the
/// block carries warnings, so a reader who only sees the text summary still
/// knows to look at `warnings[]`.
pub fn summary_line(block: &Value) -> String {
    fn describe(value: Option<&Value>, missing: &str) -> String {
        let Some(file) = value.filter(|value| !value.is_null()) else {
            return missing.to_string();
        };
        let path = file.get("path").and_then(Value::as_str).unwrap_or(missing);
        match file.get("sha256_12").and_then(Value::as_str) {
            Some(sha) => format!("{path} (sha {sha})"),
            None => format!("{path} (sha n/a)"),
        }
    }
    let text = format!(
        "scenario {}, apk {}, engine {}, fidelity {}",
        describe(block.get("scenario"), "-"),
        describe(block.get("apk"), "-"),
        block.get("engine").and_then(Value::as_str).unwrap_or("-"),
        block.get("fidelity").and_then(Value::as_str).unwrap_or("-"),
    );
    match block
        .get("warnings")
        .and_then(Value::as_array)
        .map(Vec::len)
    {
        Some(count) if count > 0 => {
            let codes: Vec<&str> = block["warnings"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|warning| warning.get("code").and_then(Value::as_str))
                .collect();
            format!("{text}, warnings: {count} [{}]", codes.join(", "))
        }
        _ => text,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("acos-provenance-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn short_sha_is_twelve_characters() {
        let sha = "a".repeat(64);
        assert_eq!(short_sha(&sha).len(), SHA_PREFIX_LEN);
        assert_eq!(short_sha("abc"), "abc");
    }

    #[test]
    fn paths_are_absolute_even_when_they_do_not_exist() {
        let path = absolute(Path::new("does/not/exist/../exist.apk"));
        assert!(path.is_absolute(), "{}", path.display());
        assert!(
            path.display().to_string().ends_with("does/not/exist.apk")
                || path.display().to_string().ends_with("does/not/exist.apk"),
            "{}",
            path.display()
        );
    }

    #[test]
    fn a_file_identity_carries_the_full_digest_and_its_prefix() {
        let dir = scratch("file");
        let path = dir.join("app.apk");
        std::fs::write(&path, b"apk bytes").unwrap();
        let value = file(&path);
        let sha = value["sha256"].as_str().unwrap();
        assert_eq!(sha.len(), 64);
        assert_eq!(value["sha256_12"], short_sha(sha));
        assert!(value["path"].as_str().unwrap().starts_with('/'));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_launch_block_has_a_null_scenario_and_named_engine() {
        let dir = scratch("launch");
        let apk = dir.join("app.apk");
        std::fs::write(&apk, b"apk bytes").unwrap();
        let block = block(&apk, None, "rust-vm", Fidelity::NativeRuntime);
        assert!(block["scenario"].is_null());
        assert_eq!(block["engine"], "rust-vm");
        assert_eq!(block["fidelity"], "native-runtime");
        let line = summary_line(&block);
        assert!(line.contains("scenario -"), "{line}");
        assert!(line.contains("engine rust-vm"), "{line}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_digest_is_cached_for_an_unchanged_file_and_recomputed_when_it_changes() {
        let dir = scratch("cache");
        let path = dir.join("app.apk");
        std::fs::write(&path, b"first bytes").unwrap();
        let first = InputFile::capture(&path);
        let second = InputFile::capture(&path);
        assert_eq!(first.sha256, second.sha256);
        assert_eq!(first.size_bytes, second.size_bytes);
        // A warm capture must not read the file: prove it by rewriting the
        // bytes *without* touching size or mtime is impossible on a real FS, so
        // instead prove the cache is keyed on the identity: a same-size rewrite
        // with a newer mtime produces a new digest.
        std::thread::sleep(std::time::Duration::from_millis(10));
        std::fs::write(&path, b"other bytes").unwrap();
        let third = InputFile::capture(&path);
        assert_ne!(first.sha256, third.sha256, "a rebuilt APK is re-hashed");
        assert_eq!(
            third.sha256.as_deref(),
            Some(sha256_file(&path).unwrap().as_str()),
            "the cached digest is the file's real digest"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_warm_capture_of_a_large_file_costs_no_hash() {
        let dir = scratch("warm");
        let path = dir.join("large.apk");
        // 32 MiB is enough for the difference between one hash and none to be
        // visible well above timer noise on any build machine.
        std::fs::write(&path, vec![0x5au8; 32 * 1024 * 1024]).unwrap();
        let cold = std::time::Instant::now();
        let first = InputFile::capture(&path);
        let cold = cold.elapsed();
        let warm = std::time::Instant::now();
        let second = InputFile::capture(&path);
        let warm = warm.elapsed();
        assert_eq!(first.sha256, second.sha256);
        assert!(
            warm * 4 < cold,
            "a warm capture must reuse the digest: cold {cold:?}, warm {warm:?}"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// The real APK the feedback was measured on: a 206 MB Gaten build costs
    /// ~90 ms to hash, and the second launch of the same bytes must pay ~0.
    ///
    /// Fixture-gated: it runs where the Gaten cache exists (the shared
    /// `.cache/gaten` directory) and is skipped elsewhere, so a clean checkout
    /// does not fail for a missing 200 MB file.
    #[test]
    fn a_warm_launch_of_the_gaten_apk_pays_no_digest() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let gaten = std::fs::read_dir(root.join(".cache/gaten"))
            .ok()
            .into_iter()
            .flatten()
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|extension| extension == "apk"))
            .find(|path| path.is_file());
        let Some(apk) = gaten else {
            let _ = acos_test_support::require_prereq(
                false,
                "a_warm_launch_of_the_gaten_apk_pays_no_digest",
                ".cache/gaten has no APK — put the Gaten release APK there",
            );
            return;
        };
        let cold = std::time::Instant::now();
        let first = InputFile::capture(&apk);
        let cold = cold.elapsed();
        let warm = std::time::Instant::now();
        let second = InputFile::capture(&apk);
        let warm = warm.elapsed();
        assert_eq!(first.sha256, second.sha256);
        assert!(first.size_bytes > 1_000_000, "{}", apk.display());
        eprintln!(
            "GATEN digest timing: {} bytes, cold {:?}, warm {:?}",
            first.size_bytes, cold, warm
        );
        assert!(
            warm * 5 < cold,
            "a warm launch must not re-hash {} bytes: cold {cold:?}, warm {warm:?}",
            first.size_bytes
        );
    }

    #[test]
    fn an_input_rewritten_during_the_run_is_reported_not_silent() {
        let dir = scratch("changed");
        let apk = dir.join("app.apk");
        let scenario = dir.join("s.yaml");
        std::fs::write(&apk, b"apk v1").unwrap();
        std::fs::write(&scenario, b"version: 1\n").unwrap();
        let inputs = Inputs::capture(&apk, Some(&scenario));
        assert!(inputs.changed_warnings().is_empty());
        std::thread::sleep(std::time::Duration::from_millis(10));
        std::fs::write(&apk, b"apk v2 rebuilt").unwrap();
        let warnings = inputs.changed_warnings();
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert_eq!(warnings[0]["code"], WARN_INPUT_CHANGED);
        assert_eq!(warnings[0]["input"], "apk");
        assert!(
            warnings[0]["message"]
                .as_str()
                .unwrap()
                .contains("size changed"),
            "{}",
            warnings[0]
        );
        // The block keeps the digest of the bytes the run actually read, and
        // the summary line names the warning so a text-only reader sees it.
        let mut block = inputs.block("art", Fidelity::ArtReference);
        assert_eq!(
            block["apk"]["sha256_12"],
            json!(inputs.apk.sha256.as_deref().map(short_sha))
        );
        for warning in warnings {
            push_warning(&mut block, warning);
        }
        let line = summary_line(&block);
        assert!(line.contains(WARN_INPUT_CHANGED), "{line}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn an_input_identity_carries_the_size_and_mtime_it_was_hashed_at() {
        let dir = scratch("identity");
        let path = dir.join("app.apk");
        std::fs::write(&path, b"apk bytes").unwrap();
        let value = InputFile::capture(&path).to_json();
        assert_eq!(value["size_bytes"], 9);
        assert!(value["mtime_ns"].as_u64().unwrap_or(0) > 0);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_summary_line_names_both_inputs_and_their_short_digests() {
        let dir = scratch("summary");
        let apk = dir.join("app.apk");
        let scenario = dir.join("s.yaml");
        std::fs::write(&apk, b"apk bytes").unwrap();
        std::fs::write(&scenario, b"version: 1\n").unwrap();
        let mut block = block(&apk, Some(&scenario), "art", Fidelity::ArtReference);
        let line = summary_line(&block);
        assert!(line.contains(apk.display().to_string().as_str()), "{line}");
        assert!(
            line.contains(scenario.display().to_string().as_str()),
            "{line}"
        );
        assert!(line.contains("engine art"), "{line}");
        assert!(line.contains("fidelity art-reference"), "{line}");
        assert_eq!(line.matches("(sha ").count(), 2, "{line}");
        push_warning(
            &mut block,
            warning(WARN_SCENARIO_APP_OVERRIDDEN, "scenario app differs"),
        );
        let line = summary_line(&block);
        assert!(
            line.contains("warnings: 1 [SCENARIO_APP_OVERRIDDEN]"),
            "{line}"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
