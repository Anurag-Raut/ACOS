//! Path and argument safety.
//!
//! Every path argument is resolved and checked against an allow-list of roots
//! before it reaches the backend. `..` components are rejected outright,
//! symlinks are resolved, and an existing file must live under a root. For
//! output paths that do not exist yet, the nearest existing ancestor is checked.

use std::path::{Component, Path, PathBuf};

/// Maximum accepted length of a single string argument.
pub const DEFAULT_MAX_ARG_CHARS: usize = 8 * 1024;

/// Validate one path argument against `roots` and return its canonical form.
///
/// `roots` must already be canonical.
pub fn validate_path(raw: &str, roots: &[PathBuf]) -> Result<PathBuf, String> {
    if raw.trim().is_empty() {
        return Err("path must not be empty".to_string());
    }
    if raw.contains('\0') {
        return Err("path must not contain NUL".to_string());
    }
    let path = Path::new(raw);
    if path
        .components()
        .any(|c| matches!(c, Component::ParentDir))
    {
        return Err(format!("path `{raw}` contains a `..` component"));
    }

    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|e| format!("cannot resolve the working directory: {e}"))?
            .join(path)
    };

    let canonical = canonicalize_lenient(&absolute)
        .map_err(|e| format!("cannot resolve path `{raw}`: {e}"))?;

    if roots.is_empty() {
        return Err("no allowed roots are configured".to_string());
    }
    if !roots.iter().any(|root| canonical.starts_with(root)) {
        return Err(format!(
            "path `{raw}` resolves outside the allowed roots ({})",
            roots
                .iter()
                .map(|r| r.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    Ok(canonical)
}

/// Canonicalize a path, falling back to canonicalizing its nearest existing
/// ancestor and re-appending the missing tail. This lets not-yet-created output
/// directories be validated without permitting escapes.
fn canonicalize_lenient(path: &Path) -> std::io::Result<PathBuf> {
    if let Ok(canonical) = std::fs::canonicalize(path) {
        return Ok(canonical);
    }
    let mut missing: Vec<std::ffi::OsString> = Vec::new();
    let mut current = path.to_path_buf();
    loop {
        let Some(parent) = current.parent().map(Path::to_path_buf) else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("no existing ancestor for {}", path.display()),
            ));
        };
        if let Some(name) = current.file_name() {
            missing.push(name.to_os_string());
        }
        if let Ok(canonical) = std::fs::canonicalize(&parent) {
            let mut result = canonical;
            for name in missing.iter().rev() {
                result.push(name);
            }
            return Ok(result);
        }
        current = parent;
    }
}

/// Recursively check that no string argument exceeds `max_chars`, and that the
/// argument tree is not absurdly deep/wide.
pub fn check_arg_limits(value: &serde_json::Value, max_chars: usize) -> Result<(), String> {
    check_arg_limits_at(value, max_chars, 0, &mut 0)
}

fn check_arg_limits_at(
    value: &serde_json::Value,
    max_chars: usize,
    depth: usize,
    nodes: &mut usize,
) -> Result<(), String> {
    if depth > 32 {
        return Err("arguments are nested too deeply".to_string());
    }
    *nodes += 1;
    if *nodes > 100_000 {
        return Err("arguments contain too many values".to_string());
    }
    match value {
        serde_json::Value::String(s) => {
            if s.chars().count() > max_chars {
                return Err(format!(
                    "a string argument exceeds {max_chars} characters"
                ));
            }
            Ok(())
        }
        serde_json::Value::Array(items) => {
            if items.len() > 100_000 {
                return Err("an argument array is too large".to_string());
            }
            for item in items {
                check_arg_limits_at(item, max_chars, depth + 1, nodes)?;
            }
            Ok(())
        }
        serde_json::Value::Object(map) => {
            for (key, item) in map {
                if key.chars().count() > max_chars {
                    return Err(format!(
                        "an object key exceeds {max_chars} characters"
                    ));
                }
                check_arg_limits_at(item, max_chars, depth + 1, nodes)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_parent_dir_and_outside_paths() {
        let root = std::fs::canonicalize(std::env::current_dir().unwrap()).unwrap();
        assert!(validate_path("../etc/passwd", std::slice::from_ref(&root)).is_err());
        assert!(validate_path("/etc/passwd", std::slice::from_ref(&root)).is_err());
        assert!(validate_path("", std::slice::from_ref(&root)).is_err());
    }

    #[test]
    fn accepts_paths_under_a_root() {
        let root = std::fs::canonicalize(std::env::current_dir().unwrap()).unwrap();
        let ok = validate_path("Cargo.toml", std::slice::from_ref(&root)).unwrap();
        assert!(ok.starts_with(&root));
        // A not-yet-existing file under the root is accepted.
        let future = validate_path("does/not/exist/yet.json", std::slice::from_ref(&root)).unwrap();
        assert!(future.starts_with(&root));
    }

    #[test]
    fn arg_limits_apply_recursively() {
        let ok = serde_json::json!({"a": ["b", "c"]});
        assert!(check_arg_limits(&ok, 8).is_ok());
        let long = serde_json::json!({"a": "0123456789"});
        assert!(check_arg_limits(&long, 8).is_err());
    }
}
