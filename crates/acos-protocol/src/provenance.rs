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

use std::path::{Component, Path, PathBuf};

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::envelope::Fidelity;

/// How many hex characters of a SHA-256 a summary line shows.
pub const SHA_PREFIX_LEN: usize = 12;

/// A warning code: the scenario's own `app:` was overridden by `--apk`.
pub const WARN_SCENARIO_APP_OVERRIDDEN: &str = "SCENARIO_APP_OVERRIDDEN";

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

/// SHA-256 of a file, lowercase hex.
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
    let absolute = absolute(path);
    let sha = sha256_file(&absolute).ok();
    json!({
        "path": absolute.display().to_string(),
        "sha256": sha,
        "sha256_12": sha.as_deref().map(short_sha),
    })
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
pub fn block(
    apk: &Path,
    scenario: Option<&Path>,
    engine: &str,
    fidelity: Fidelity,
) -> Value {
    json!({
        "scenario": scenario.map(file),
        "apk": file(apk),
        "engine": engine,
        "fidelity": fidelity.as_str(),
        "warnings": <Vec<Value>>::new(),
    })
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
        let path = file
            .get("path")
            .and_then(Value::as_str)
            .unwrap_or(missing);
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
        block
            .get("fidelity")
            .and_then(Value::as_str)
            .unwrap_or("-"),
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
        let dir = std::env::temp_dir().join(format!(
            "acos-provenance-{}-{name}",
            std::process::id()
        ));
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
    fn the_summary_line_names_both_inputs_and_their_short_digests() {
        let dir = scratch("summary");
        let apk = dir.join("app.apk");
        let scenario = dir.join("s.yaml");
        std::fs::write(&apk, b"apk bytes").unwrap();
        std::fs::write(&scenario, b"version: 1\n").unwrap();
        let mut block = block(
            &apk,
            Some(&scenario),
            "art",
            Fidelity::ArtReference,
        );
        let line = summary_line(&block);
        assert!(line.contains(apk.display().to_string().as_str()), "{line}");
        assert!(line.contains(scenario.display().to_string().as_str()), "{line}");
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
