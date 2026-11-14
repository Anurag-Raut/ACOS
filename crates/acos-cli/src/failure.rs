//! Failure classification for the real-ART route (W46).
//!
//! Two rules from the advisor's taxonomy (§10) are enforced here:
//!
//! 1. Every `acos-art` launch/reply code and every fact in a session's own
//!    `session-exit.json` maps to one protocol [`ErrorCode`] with an explicit
//!    class, retryability and hint. `INTERNAL` is reserved for genuine harness
//!    defects (and for an art code that has not been registered yet, with a
//!    hint naming it).
//! 2. A write failure on the host scratch filesystem is `HOST_DISK_FULL`
//!    (retryable), never a panic with an empty stderr and never `INTERNAL`.
//!
//! The mapping is a literal table, not a heuristic: a new art code must be
//! added here (and to `acos_protocol::ErrorCode`) or it shows up as a named
//! unmapped-code harness failure.

use std::io;
use std::path::{Path, PathBuf};

use acos_protocol::{ErrorCode, ProtocolError};
use serde_json::{json, Value};

/// The session's own exit record (written by `acos-art` next to its frames).
pub const SESSION_EXIT_FILE: &str = "session-exit.json";

/// Every `acos-art` launch/reply code this CLI knows, and its protocol code.
///
/// The art codes come from `crates/acos-art/src/{launch,launcher}.rs`; the
/// mapping is grouped by what the code actually means, not by spelling.
pub fn map_art_code(code: &str) -> ErrorCode {
    match code {
        // Host conditions: retryable, never app behaviour.
        "HOST_DISK_FULL" => ErrorCode::HostDiskFull,
        "HOST_PANIC" => ErrorCode::HostPanic,
        "HOST_MEMORY_LIMIT" => ErrorCode::HostMemoryLimit,
        // Isolation layers the host could not provide (fail closed).
        "SECCOMP_UNAVAILABLE" => ErrorCode::SeccompUnavailable,
        "LANDLOCK_UNAVAILABLE" => ErrorCode::LandlockUnavailable,
        "MEMORY_CGROUP_UNAVAILABLE" => ErrorCode::ConfinementUnavailable,
        // The guest process ended (or was killed) before the stage completed.
        "GUEST_EXITED"
        | "GUEST_EXIT"
        | "GUEST_EXIT_AFTER_RESUME"
        | "GUEST_KILLED_BY_SIGNAL"
        | "NONZERO_EXIT" => ErrorCode::GuestExited,
        // Deadlines and missing fresh evidence.
        "TIMEOUT" | "NO_ACTIVITY_RESUMED" => ErrorCode::Timeout,
        "NO_LIVE_FRAME" | "NO_FRESH_FRAME" | "POST_RESUME_NOT_READY" => ErrorCode::NoFreshFrame,
        // Guest-visible crashes (ART's own fatal reports).
        "ART_CRASH" | "ART_FATAL_CHECK" | "ART_FATAL_LOG" => ErrorCode::AppCrashed,
        "UNSUPPORTED_SYSCALL" => ErrorCode::SeccompUnavailable,
        // Capability refusals from the guest/session.
        "UNSUPPORTED_ANDROID_API" => ErrorCode::UnsupportedAndroidApi,
        "NO_TEXT" => ErrorCode::NoText,
        "INPUT_UNAVAILABLE" => ErrorCode::UnsupportedBackend,
        "RESOURCE_LIMIT" => ErrorCode::ResourceLimit,
        // Bad request shape (caller bug).
        "BAD_REQUEST" => ErrorCode::InvalidArgument,
        // Genuine harness/protocol defects: the session could not form or
        // deliver the binder transaction, or reported an explicit INTERNAL.
        "NO_BIND_REQUEST"
        | "BIND_NOT_DELIVERED"
        | "NO_APPLICATION"
        | "STARTUP_FAILED"
        | "COMMAND_FAILED"
        | "INTERNAL" => ErrorCode::Internal,
        // Unknown: the caller turns this into a named harness gap.
        _ => ErrorCode::Internal,
    }
}

/// Whether `code` is in the explicit table (an unregistered code is a harness
/// gap and must name itself in the hint).
pub fn is_known_art_code(code: &str) -> bool {
    !matches!(
        code,
        "" | "UNKNOWN" | "COMMAND_FAILED" | "STARTUP_FAILED" | "INTERNAL"
    ) && map_art_code(code) != ErrorCode::Internal
        || matches!(
            code,
            "NO_BIND_REQUEST"
                | "BIND_NOT_DELIVERED"
                | "NO_APPLICATION"
                | "STARTUP_FAILED"
                | "COMMAND_FAILED"
                | "INTERNAL"
        )
}

/// Path of the session exit record inside an artifacts directory.
pub fn session_exit_path(artifacts: &Path) -> PathBuf {
    artifacts.join("art-frames").join(SESSION_EXIT_FILE)
}

/// Read `<artifacts>/art-frames/session-exit.json`, if the session wrote one.
pub fn read_session_exit(artifacts: &Path) -> Option<Value> {
    let path = session_exit_path(artifacts);
    let text = std::fs::read_to_string(&path).ok()?;
    match serde_json::from_str::<Value>(&text) {
        Ok(value) => Some(value),
        Err(_) => Some(json!({
            "unreadable": true,
            "path": path.display().to_string(),
        })),
    }
}

/// Facts from a session-exit record, trimmed to what a verdict needs.
fn session_exit_facts(doc: &Value, artifacts: &Path) -> Value {
    json!({
        "path": session_exit_path(artifacts).display().to_string(),
        "code": doc.get("code").cloned().unwrap_or(Value::Null),
        "retryable": doc.get("retryable").cloned().unwrap_or(Value::Null),
        "outcome": doc.get("outcome").cloned().unwrap_or(Value::Null),
        "stage": doc.get("stage").cloned().unwrap_or(Value::Null),
        "guest_exit": doc.get("guest_exit").cloned().unwrap_or(Value::Null),
        "guest_signal": doc.get("guest_signal").cloned().unwrap_or(Value::Null),
        "guest_crash": doc.get("guest_crash").cloned().unwrap_or(Value::Null),
        // R3: a guest killed by a signal carries the fault address, si_code, the
        // faulting thread and the runtime's native backtrace (mapped library +
        // offset). Absent when the guest produced no crash record.
        "native_crash": doc.get("native_crash").cloned().unwrap_or(Value::Null),
        "native_crash_path": doc.get("native_crash_path").cloned().unwrap_or(Value::Null),
        "cgroup": doc.get("cgroup").cloned().unwrap_or(Value::Null),
        "disk_free_bytes": doc.get("disk_free_bytes").cloned().unwrap_or(Value::Null),
        "panic": doc.get("panic").cloned().unwrap_or(Value::Null),
    })
}

/// Classify a fact record that carries no usable code.
///
/// A guest signal is evidence: SIGSYS is the guard refusing a syscall (host
/// isolation layer), a segfault/abort/bus/trap is the guest crashing, and
/// anything else is a guest exit that needs triage.
fn classify_facts_by_process(doc: &Value) -> Option<ErrorCode> {
    if let Some(signal) = doc.get("guest_signal").and_then(Value::as_i64) {
        return Some(match signal {
            31 => ErrorCode::SeccompUnavailable, // SIGSYS
            11 | 6 | 4 | 8 | 7 => ErrorCode::AppCrashed, // SEGV/ABRT/ILL/FPE/BUS
            _ => ErrorCode::GuestExited,
        });
    }
    if doc.get("guest_exit").map(|v| !v.is_null()).unwrap_or(false) {
        return Some(ErrorCode::GuestExited);
    }
    None
}

/// Classify a session's end from its own record.
///
/// Returns `(code, facts)` when the record names a failure. `None` means the
/// record describes a clean end (or there is no record), so the caller keeps
/// its own error.
pub fn classify_session_exit(artifacts: &Path) -> Option<(ErrorCode, Value)> {
    let doc = read_session_exit(artifacts)?;
    let facts = session_exit_facts(&doc, artifacts);
    let outcome = doc.get("outcome").and_then(Value::as_str).unwrap_or("");
    let raw_code = doc.get("code").and_then(Value::as_str);
    let code = match raw_code.map(map_art_code) {
        // A concrete art code wins, unless it says INTERNAL while the record
        // carries process evidence that names the cause.
        Some(ErrorCode::Internal) | None => {
            classify_facts_by_process(&doc).unwrap_or(ErrorCode::Internal)
        }
        Some(code) => code,
    };
    if raw_code.is_none() {
        if outcome == "ok" && doc.get("guest_exit").and_then(Value::as_i64) == Some(0) {
            return None;
        }
        return classify_facts_by_process(&doc).map(|code| (code, facts));
    }
    if code == ErrorCode::Internal && outcome == "ok" {
        // No failure named and the session says it ended cleanly: not a failure.
        return None;
    }
    Some((code, facts))
}

/// Classify a lost session from the harness child's own exit status, for the
/// case where the launcher was killed before it could write
/// `session-exit.json` (W53, applied in W55).
///
/// Same evidence model as [`classify_facts_by_process`]: the terminating signal
/// or exit code is the process evidence. A child that died carries a real
/// cause, so this never returns `INTERNAL` for `status.signal().is_some()`.
/// `GuestExited` is the code that is documented to carry "the guest exit code,
/// terminating signal and stage"; a dedicated `HOST_SIGNAL` code would be its
/// better long-term home.
pub fn classify_child_exit(status: std::process::ExitStatus) -> Option<(ErrorCode, Value)> {
    use std::os::unix::process::ExitStatusExt;
    let facts = json!({
        "source": "acos-art child exit status (no session-exit.json)",
        "guest_exit": status.code(),
        "guest_signal": status.signal(),
    });
    if status.signal().is_some() || status.code().is_some() {
        return Some((ErrorCode::GuestExited, facts));
    }
    None
}

/// Build the protocol error for one art reply / launch code, attaching every
/// session-exit fact the session left behind.
pub fn art_error(code: &str, message: &str, artifacts: &Path) -> ProtocolError {
    let known = is_known_art_code(code);
    let mapped = map_art_code(code);
    let text = if matches!(code, "NO_LIVE_FRAME" | "POST_RESUME_NOT_READY") {
        // Keep the engine's established marker so an agent still sees that no
        // live frame existed, while the code is the honest NO_FRESH_FRAME.
        format!("LIVE_FRAME_UNAVAILABLE: acos-art: {code}: {message}")
    } else {
        format!("acos-art: {code}: {message}")
    };
    let mut error = ProtocolError::new(mapped, text);
    if !known {
        error = error.with_hint(format!(
            "acos-art reported `{code}`, which is not in the registered taxonomy. \
             Register it in acos_protocol::ErrorCode and acos-cli/src/failure.rs; \
             until then this is a named harness gap, not app behaviour."
        ));
    }
    let mut details = json!({"art_code": code});
    if let Some(doc) = read_session_exit(artifacts) {
        details["session_exit"] = session_exit_facts(&doc, artifacts);
    }
    details["session_exit_path"] = json!(session_exit_path(artifacts).display().to_string());
    error.with_details(details)
}

/// Is this I/O error a full host filesystem (ENOSPC/EDQUOT)?
pub fn is_disk_full(error: &io::Error) -> bool {
    matches!(error.raw_os_error(), Some(28) | Some(122))
}

/// Test seam for the ENOSPC path: `ACOS_FAULT_INJECT_WRITE=ENOSPC` plus
/// `ACOS_FAULT_INJECT_ALLOW=1`. Without the second variable an injected fault
/// is ignored, so a stale environment cannot change a real verdict.
pub fn fault_inject_disk_full() -> bool {
    matches!(
        std::env::var("ACOS_FAULT_INJECT_WRITE").as_deref(),
        Ok("ENOSPC") | Ok("ENOSPC_ANNOTATE")
    ) && std::env::var("ACOS_FAULT_INJECT_ALLOW").as_deref() == Ok("1")
}

/// Write one artifact, honouring the ENOSPC test seam.
pub fn write_artifact(path: &Path, bytes: &[u8]) -> io::Result<()> {
    if fault_inject_disk_full() {
        return Err(io::Error::from_raw_os_error(28));
    }
    std::fs::write(path, bytes)
}

/// The protocol error for a failed artifact write. ENOSPC/EDQUOT is a
/// retryable host condition; any other write failure is a harness/artifact
/// failure and names the path.
pub fn write_failure(error: &io::Error, path: &Path) -> ProtocolError {
    let path_text = path.display().to_string();
    if is_disk_full(error) {
        ProtocolError::new(
            ErrorCode::HostDiskFull,
            format!("cannot write {path_text}: {error}"),
        )
        .with_details(json!({"path": path_text, "os_error": error.raw_os_error()}))
    } else {
        ProtocolError::new(
            ErrorCode::ArtifactsInvalid,
            format!("cannot write {path_text}: {error}"),
        )
        .with_hint(
            "The artifacts path could not be written but the host disk is not full. \
             Check permissions and that --artifacts is inside the workspace.",
        )
        .with_details(json!({"path": path_text, "os_error": error.raw_os_error()}))
    }
}

/// The compact session-exit facts stored in a verdict / engine report.
pub fn session_exit_summary(doc: &Value, artifacts: &Path) -> Value {
    session_exit_facts(doc, artifacts)
}

/// Attach a session's exit record to an existing protocol error.
///
/// If the error is still `INTERNAL` and the record names a concrete cause, the
/// error is re-classified: `INTERNAL` is never kept when a specific cause is
/// known. Called on the paths where the CLI builds the envelope itself (start
/// refusal), so the facts reach a caller even when no step ran.
pub fn attach_session_exit(error: &mut ProtocolError, artifacts: &Path) {
    let Some(doc) = read_session_exit(artifacts) else {
        return;
    };
    let facts = session_exit_facts(&doc, artifacts);
    if error.code == ErrorCode::Internal {
        if let Some((code, _)) = classify_session_exit(artifacts) {
            let message = error.message.clone();
            *error = ProtocolError::new(code, message);
        }
    }
    let mut details = error
        .details
        .clone()
        .unwrap_or_else(|| json!({}));
    if let Some(object) = details.as_object_mut() {
        object.insert("session_exit".to_string(), facts);
        object.insert(
            "session_exit_path".to_string(),
            json!(session_exit_path(artifacts).display().to_string()),
        );
    }
    error.details = Some(details);
}

/// Install the CLI panic hook: a harness panic must leave a structured
/// `HOST_PANIC` envelope on stderr, never exit 101 with empty stderr.
pub fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let error = ProtocolError::new(
            ErrorCode::HostPanic,
            format!("the acos harness panicked: {info}"),
        );
        let envelope = json!({
            "request_id": format!("req_panic_{}", std::process::id()),
            "ok": false,
            "status": "error",
            "backend": "acos-native",
            "fidelity": "native-runtime",
            "capabilities_used": [],
            "error": error,
            "metrics": {"wall_ms": 0},
        });
        eprintln!("acos: HOST_PANIC: {info}");
        eprintln!(
            "{}",
            serde_json::to_string(&envelope).unwrap_or_else(|_| "{\"error\":\"HOST_PANIC\"}".into())
        );
        previous(info);
    }));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_art_code_maps_explicitly_and_host_codes_are_retryable() {
        for (code, expected) in [
            ("HOST_DISK_FULL", ErrorCode::HostDiskFull),
            ("HOST_PANIC", ErrorCode::HostPanic),
            ("HOST_MEMORY_LIMIT", ErrorCode::HostMemoryLimit),
            ("SECCOMP_UNAVAILABLE", ErrorCode::SeccompUnavailable),
            ("GUEST_EXITED", ErrorCode::GuestExited),
            ("GUEST_KILLED_BY_SIGNAL", ErrorCode::GuestExited),
            ("NO_LIVE_FRAME", ErrorCode::NoFreshFrame),
            ("POST_RESUME_NOT_READY", ErrorCode::NoFreshFrame),
            ("TIMEOUT", ErrorCode::Timeout),
            ("NO_FRESH_FRAME", ErrorCode::NoFreshFrame),
            ("ART_FATAL_CHECK", ErrorCode::AppCrashed),
            ("BAD_REQUEST", ErrorCode::InvalidArgument),
        ] {
            assert_eq!(map_art_code(code), expected, "{code}");
            assert!(is_known_art_code(code), "{code} must be registered");
        }
        assert!(ErrorCode::HostDiskFull.retryable());
        assert!(!ErrorCode::HostPanic.retryable());
        assert!(!ErrorCode::GuestExited.retryable());
        assert!(!is_known_art_code("SOMETHING_NEW"));
    }

    #[test]
    fn child_exit_status_is_classified_never_internal() {
        use std::os::unix::process::ExitStatusExt;
        // A launcher SIGKILLed (memcg OOM, watchdog, parent kill path) has a
        // signal and no exit code; that is the W53 case.
        let killed = std::process::ExitStatus::from_raw(9); // SIGKILL
        let (code, facts) = classify_child_exit(killed).expect("a signalled child is classified");
        assert_eq!(code, ErrorCode::GuestExited);
        assert_eq!(facts["guest_signal"], serde_json::json!(9));
        assert_eq!(facts["guest_exit"], serde_json::Value::Null);
        // A child that exited with a code is classified too.
        let exited = std::process::ExitStatus::from_raw(1 << 8);
        let (code, facts) = classify_child_exit(exited).expect("an exited child is classified");
        assert_eq!(code, ErrorCode::GuestExited);
        assert_eq!(facts["guest_exit"], serde_json::json!(1));
    }

    #[test]
    fn session_exit_signal_facts_classify_the_cause() {
        let dir = std::env::temp_dir().join(format!("acos-w46-classify-{}", std::process::id()));
        let frames = dir.join("art-frames");
        std::fs::create_dir_all(&frames).unwrap();
        std::fs::write(
            frames.join(SESSION_EXIT_FILE),
            r#"{"outcome":"crash","code":null,"stage":"guest-exec","guest_exit":null,"guest_signal":31}"#,
        )
        .unwrap();
        let (code, facts) = classify_session_exit(&dir).expect("a failure record");
        assert_eq!(code, ErrorCode::SeccompUnavailable);
        assert_eq!(facts["stage"], "guest-exec");
        assert_eq!(facts["guest_signal"], 31);

        std::fs::write(
            frames.join(SESSION_EXIT_FILE),
            r#"{"outcome":"ok","code":null,"guest_exit":0}"#,
        )
        .unwrap();
        assert!(classify_session_exit(&dir).is_none(), "a clean end is not a failure");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
