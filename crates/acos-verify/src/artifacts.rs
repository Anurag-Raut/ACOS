//! Writing the artifacts directory described in `docs/AGENT_API.md` §10.
//!
//! Layout:
//!
//! ```text
//! verdict.json
//! steps/NN-<kind>.json
//! frames/<frame_seq>.png
//! tree/<frame_seq>.json
//! logs/logcat.txt
//! events.jsonl
//! resources.json
//! repro.sh
//! repro.json
//! ```
//!
//! Every path written here is relative to the artifacts directory, so the
//! verdict can reference it without escaping.

use std::path::Path;

use acos_protocol::artifacts::{
    frame_file_name, logcat_file_name, step_file_name, tree_file_name, EVENTS_JSONL, REPRO_JSON,
    REPRO_SH, RESOURCES_JSON, VERDICT_JSON,
};
use acos_protocol::verdict::{Reproducer, ResourceReport, StepResult, Verdict};
use acos_protocol::Event;
use serde::Serialize;

use crate::backend::LogRecord;
use crate::error::VerifyError;

/// Is this I/O error the host filesystem refusing a write (ENOSPC 28,
/// EDQUOT 122)? Those are host conditions, never app behaviour (W46).
pub fn io_is_disk_full(error: &std::io::Error) -> bool {
    matches!(error.raw_os_error(), Some(28) | Some(122))
}

/// Test seam: inject an ENOSPC on the next artifact write.
///
/// Set `ACOS_FAULT_INJECT_WRITE=ENOSPC`. It is honoured only when
/// `ACOS_FAULT_INJECT_ALLOW=1` is also set, so a stray environment variable can
/// never change a real verdict.
pub fn fault_inject_disk_full() -> bool {
    std::env::var("ACOS_FAULT_INJECT_WRITE").as_deref() == Ok("ENOSPC")
        && std::env::var("ACOS_FAULT_INJECT_ALLOW").as_deref() == Ok("1")
}

fn artifact_error(path: &str, error: &std::io::Error) -> VerifyError {
    if io_is_disk_full(error) || fault_inject_disk_full() {
        VerifyError::DiskFull {
            path: path.to_string(),
            detail: error.to_string(),
        }
    } else {
        VerifyError::ArtifactIo {
            path: path.to_string(),
            detail: error.to_string(),
        }
    }
}

fn injected_enospc(path: &str) -> Option<VerifyError> {
    if fault_inject_disk_full() {
        let error = std::io::Error::from_raw_os_error(28);
        return Some(artifact_error(path, &error));
    }
    None
}

fn write_bytes(dir: &Path, relative: &str, bytes: &[u8]) -> Result<(), VerifyError> {
    let path = dir.join(relative);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| artifact_error(relative, &e))?;
    }
    if let Some(error) = injected_enospc(relative) {
        return Err(error);
    }
    std::fs::write(&path, bytes).map_err(|e| artifact_error(relative, &e))
}

fn write_json<T: Serialize>(dir: &Path, relative: &str, value: &T) -> Result<(), VerifyError> {
    let bytes = serde_json::to_vec_pretty(value).map_err(|e| VerifyError::ArtifactIo {
        path: relative.to_string(),
        detail: e.to_string(),
    })?;
    write_bytes(dir, relative, &bytes)
}

/// Create the artifacts directory tree.
pub fn prepare(dir: &Path) -> Result<(), VerifyError> {
    for sub in ["steps", "frames", "tree", "logs"] {
        let path = dir.join(sub);
        std::fs::create_dir_all(&path).map_err(|e| artifact_error(sub, &e))?;
    }
    Ok(())
}

/// Write one `steps/NN-<kind>.json`.
pub fn write_step(dir: &Path, step: &StepResult) -> Result<(), VerifyError> {
    write_json(dir, &step_file_name(step.index, &step.step), step)
}

/// Write one `tree/<seq>.json`.
pub fn write_tree(dir: &Path, seq: u64, tree: &acos_protocol::UiTree) -> Result<(), VerifyError> {
    write_json(dir, &tree_file_name(seq), tree)
}

/// Write one `frames/<seq>.png`.
pub fn write_frame(dir: &Path, seq: u64, png: &[u8]) -> Result<(), VerifyError> {
    write_bytes(dir, &frame_file_name(seq), png)
}

/// Write `logs/logcat.txt` in Android logcat form.
pub fn write_logcat(dir: &Path, logs: &[LogRecord]) -> Result<(), VerifyError> {
    let mut text = String::from("--------- beginning of main\n");
    for record in logs {
        let level = match record.level {
            acos_protocol::LogLevel::Verbose => 'V',
            acos_protocol::LogLevel::Debug => 'D',
            acos_protocol::LogLevel::Info => 'I',
            acos_protocol::LogLevel::Warn => 'W',
            acos_protocol::LogLevel::Error => 'E',
        };
        text.push_str(&format!(
            "{:>6}  {:>5}  {} {}: {}\n",
            record.timestamp_ms, record.seq, level, record.tag, record.message
        ));
    }
    write_bytes(dir, &logcat_file_name(), text.as_bytes())
}

/// Write `events.jsonl`, one JSON event per line.
pub fn write_events(dir: &Path, events: &[Event]) -> Result<(), VerifyError> {
    let mut text = String::new();
    for event in events {
        let line = serde_json::to_string(event).map_err(|e| VerifyError::ArtifactIo {
            path: EVENTS_JSONL.to_string(),
            detail: e.to_string(),
        })?;
        text.push_str(&line);
        text.push('\n');
    }
    write_bytes(dir, EVENTS_JSONL, text.as_bytes())
}

/// Write `resources.json`.
pub fn write_resources(dir: &Path, report: &ResourceReport) -> Result<(), VerifyError> {
    write_json(dir, RESOURCES_JSON, report)
}

/// Write `repro.json` and `repro.sh`.
///
/// `repro.sh` is the runnable form of the same request: the same APK, scenario,
/// artifacts directory, required fidelity **and** every deterministic flag, so
/// a replay is the run that produced the verdict. Flags are `NAME=VALUE` (see
/// `acos_protocol::commands::validate_reproducer_flag`).
pub fn write_reproducer(dir: &Path, reproducer: &Reproducer) -> Result<(), VerifyError> {
    write_json(dir, REPRO_JSON, reproducer)?;
    let mut lines = vec![
        format!("  --apk {}", shell_quote(&reproducer.apk_path)),
        format!("  --scenario {}", shell_quote(&reproducer.scenario_path)),
        format!("  --artifacts {}", shell_quote(".")),
        format!(
            "  --required-fidelity {}",
            shell_quote(reproducer.fidelity.as_str())
        ),
    ];
    for flag in &reproducer.flags {
        lines.push(format!("  --flag {}", shell_quote(flag)));
    }
    let script = format!(
        "#!/bin/sh\n# Generated by acos verify; do not edit.\nset -eu\nacos verify \\\n{}\n",
        lines.join(" \\\n")
    );
    write_bytes(dir, REPRO_SH, script.as_bytes())
}

/// Write `verdict.json`.
pub fn write_verdict(dir: &Path, verdict: &Verdict) -> Result<(), VerifyError> {
    write_json(dir, VERDICT_JSON, verdict)
}

/// A minimal POSIX single-quote escape.
fn shell_quote(value: &str) -> String {
    if value.is_empty() {
        return "''".to_string();
    }
    if value
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "-_./:=@".contains(c))
    {
        return value.to_string();
    }
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn test_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "acos-verify-artifacts-{}-{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn prepares_and_writes_expected_files() {
        let dir = test_dir("basic");
        prepare(&dir).unwrap();
        assert!(dir.join("steps").is_dir());
        assert!(dir.join("frames").is_dir());
        assert!(dir.join("tree").is_dir());
        assert!(dir.join("logs").is_dir());
        write_logcat(&dir, &[]).unwrap();
        assert!(dir.join("logs/logcat.txt").is_file());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// F10: `repro.sh` must replay the same request, so the deterministic
    /// flags recorded in `repro.json` are also `--flag` lines in the script.
    #[test]
    fn repro_sh_replays_the_recorded_flags() {
        let dir = test_dir("flags");
        prepare(&dir).unwrap();
        let mut repro = Reproducer::new(
            "0.2.0",
            "fixtures/build/apps/counter.apk",
            "0".repeat(64),
            "scenarios/counter.yaml",
            "1".repeat(64),
            acos_protocol::IsolationLevel::Process,
            acos_protocol::identity::DisplaySpec::offscreen(1080, 2400, 420),
            acos_protocol::Fidelity::NativeRuntime,
            acos_protocol::Backend::AcosNative,
        )
        .with_flags(["engine=art", "variant.pre-fix=1"]);
        write_reproducer(&dir, &repro).unwrap();
        let script = std::fs::read_to_string(dir.join(REPRO_SH)).unwrap();
        assert!(script.contains("--flag engine=art"), "{script}");
        assert!(script.contains("--flag variant.pre-fix=1"), "{script}");
        // Written in the recorded (sorted, de-duplicated) order, one per line,
        // and every continuation line ends with a backslash except the last.
        let flag_lines: Vec<&str> = script
            .lines()
            .filter(|line| line.trim_start().starts_with("--flag "))
            .collect();
        assert_eq!(flag_lines.len(), 2, "{script}");
        assert!(!flag_lines[1].ends_with('\\'), "{script}");
        // The JSON stays the machine-readable copy of the same list.
        let json: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(dir.join(REPRO_JSON)).unwrap()).unwrap();
        assert_eq!(json["flags"], serde_json::json!(["engine=art", "variant.pre-fix=1"]));
        // No flags: no stray `--flag` argument.
        repro.flags.clear();
        write_reproducer(&dir, &repro).unwrap();
        let script = std::fs::read_to_string(dir.join(REPRO_SH)).unwrap();
        assert!(!script.contains("--flag"), "{script}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn shell_quote_handles_spaces() {
        assert_eq!(shell_quote("a b"), "'a b'");
        assert_eq!(shell_quote("path/with/normal"), "path/with/normal");
    }
}
