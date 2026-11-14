//! The `acos_help` self-description.
//!
//! The prose comes from [`docs/AGENT_GUIDE.md`](../../../docs/AGENT_GUIDE.md),
//! embedded with `include_str!` so the installed binary carries the full guide
//! with no files to find at runtime. The guide is written for agents: the loop,
//! every CLI command with a runnable example, how to write a scenario, the
//! settle rules, error codes and their next action, and the limits.
//!
//! The payload also reports what is supported *right now* from the live
//! capability probe (never hard-coded) and folds in the summary lines of
//! `COMPATIBILITY.md` when that file is readable.

use std::path::Path;

use acos_protocol::Fidelity;
use serde_json::{json, Value};

use crate::backend::{BackendInfo, Capabilities};
use crate::descriptors::{command_fidelity, tool_infos};

/// The agent guide, embedded at build time from `docs/AGENT_GUIDE.md`.
///
/// One source of truth: the file is the documentation for humans and the text
/// `acos_help` and the MCP resources return. A test asserts the embedded copy
/// and the file on disk cannot drift.
pub const AGENT_GUIDE: &str = include_str!("../../../docs/AGENT_GUIDE.md");

/// The guide topics `acos_help {"topic": ...}` accepts. Each names a
/// `<!-- topic:NAME -->` … `<!-- /topic:NAME -->` block in the guide.
pub const TOPICS: &[&str] = &["workflow", "cli", "scenarios", "errors", "limits"];

/// The guide, or exactly one topic block of it. An unknown topic returns the
/// whole guide (a wrong topic must not hide the documentation).
#[must_use]
pub fn guide(topic: Option<&str>) -> String {
    let Some(topic) = topic.filter(|t| TOPICS.contains(t)) else {
        return AGENT_GUIDE.trim().to_string();
    };
    let start = format!("<!-- topic:{topic} -->");
    let end = format!("<!-- /topic:{topic} -->");
    match (AGENT_GUIDE.find(&start), AGENT_GUIDE.find(&end)) {
        (Some(from), Some(to)) if to > from => AGENT_GUIDE[from + start.len()..to]
            .trim()
            .to_string(),
        _ => AGENT_GUIDE.trim().to_string(),
    }
}

/// Build the structured help payload.
pub fn build_help(
    caps: &Capabilities,
    info: &BackendInfo,
    compatibility_root: Option<&Path>,
) -> Value {
    build_help_topic(caps, info, compatibility_root, None)
}

/// `build_help` for one guide topic (`None` = the whole guide).
pub fn build_help_topic(
    caps: &Capabilities,
    info: &BackendInfo,
    compatibility_root: Option<&Path>,
    topic: Option<&str>,
) -> Value {
    let infos = tool_infos(caps);
    let supported: Vec<&str> = infos
        .iter()
        .filter(|t| t.available)
        .map(|t| t.tool_name.as_str())
        .collect();
    let unsupported: Vec<Value> = infos
        .iter()
        .filter(|t| !t.available)
        .map(|t| {
            json!({
                "tool": t.tool_name,
                "command": t.command.map(|c| c.as_str()),
                "reason": t.reason,
                "fidelity_when_available": t.fidelity.as_str(),
            })
        })
        .collect();

    let error_playbook = json!([
        {
            "category": "engine cannot do it (status=unsupported)",
            "codes": ["UNSUPPORTED_ANDROID_API", "UNSUPPORTED_JAVA_API", "UNSUPPORTED_BYTECODE",
                      "UNSUPPORTED_NATIVE_SYMBOL", "UNSUPPORTED_RELOCATION", "NEEDS_REAL_DEVICE",
                      "REQUIRES_GMS", "WRONG_ABI", "NO_DISPLAY", "NO_TEXT", "UNSUPPORTED_BACKEND",
                      "RENDER_UNSUPPORTED_OP"],
            "action": "Do not rewrite the app to hide the gap. Read details, try another fidelity/target, or report unsupported. Never a pass."
        },
        {
            "category": "the app is wrong (status=fail)",
            "codes": ["ASSERTION_FAILED", "ELEMENT_NOT_FOUND"],
            "action": "Read the assertion diff (expected/actual/path) and the linked frame/tree, then fix the app or the expectation and rerun."
        },
        {
            "category": "app crashed or hung",
            "codes": ["APP_CRASHED", "APP_NOT_RESPONDING", "JAVA_EXCEPTION"],
            "action": "Read the crash/ANR event and last actions; fix the app or the unsupported platform call. Never a pass."
        },
        {
            "category": "bad build artifact",
            "codes": ["APK_NOT_ZIP", "APK_NO_MANIFEST", "APK_IO", "APK_ENTRY",
                      "APK_SIGNING_MALFORMED", "AXML_MALFORMED", "ARSC_MALFORMED", "DEX_BAD_*",
                      "DEX_TRUNCATED", "DEX_VERIFY_FAILED", "DEX_LOAD_ERROR", "ELF_MALFORMED",
                      "NATIVE_DEP_NOT_FOUND", "NATIVE_INIT_FAILED", "NATIVE_SYMBOL_NOT_FOUND"],
            "action": "Rebuild/re-sign/re-package and retry inspect first; these are artifact problems, not app verdicts."
        },
        {
            "category": "session / resource problems",
            "codes": ["SESSION_NOT_FOUND", "SESSION_BUSY", "STALE_REF", "TIMEOUT", "RESOURCE_LIMIT",
                      "RAM_GUARD", "RAM_ABORT"],
            "action": "Create the session first, serialise mutations, re-observe after mutations, reduce concurrency or raise an explicit budget."
        },
        {
            "category": "evidence / fidelity problems",
            "codes": ["NO_FRESH_FRAME", "UNVERIFIED_FIDELITY", "ARTIFACTS_INVALID", "UNSAFE_PATH"],
            "action": "Wait for a new frame, use after_seq/after_cause, re-run at a qualifying fidelity, and write artifacts inside the artifacts directory."
        },
        {
            "category": "invalid input",
            "codes": ["INVALID_SCENARIO", "INVALID_ARGUMENT", "ISOLATION_UNAVAILABLE",
                      "ISOLATION_UNSATISFIED"],
            "action": "Fix every reported path/field; choose an isolation level the engine supports or report the missing guarantee."
        },
        {
            "category": "harness failure",
            "codes": ["VM_INTERNAL", "INTERNAL", "RENDER_*", "RENDER_STRICT"],
            "action": "An ACOS bug or strict-rendering rejection. Capture request/scenario/events; do not retry blindly or claim pass."
        }
    ]);

    let fidelity = Fidelity::StaticAnalysis;

    let compatibility = compatibility_root
        .and_then(read_compatibility_summary)
        .map(|summary| {
            json!({
                "source": "COMPATIBILITY.md",
                "summary": summary,
            })
        });

    let markdown = render_markdown(&supported, &unsupported);

    json!({
        "loop": "inspect -> verify -> read verdict.json -> fix -> rerun",
        "workflow": [
            "1. acos_inspect the APK (static only; never executes) to learn package, launcher, SDK, DEX/native inventory.",
            "2. acos_capability_report (once implemented) or acos_capabilities to check the engine can do what the task needs.",
            "3. acos_verify an APK + scenario into an artifacts directory; the verdict and step evidence are the product.",
            "4. Read verdict.json and steps/NN-*.json on a non-pass; never guess.",
            "5. Fix the app or the scenario, then rerun into a fresh directory. Reuse the same scenario so the comparison is meaningful.",
            "6. A pass is valid only when its evidence exists; `unsupported` is never a pass."
        ],
        "error_playbook": error_playbook,
        "fidelity_rules": [
            {"level": "static-analysis", "proves": "host APK/DEX parsing only; no runtime behaviour"},
            {"level": "jvm-reference", "proves": "host OpenJDK pure-Java semantics only; never Android/ART/framework/JNI/rendering"},
            {"level": "native-runtime", "proves": "ACOS executed it; not ART-validated unless validated_against says so"},
            {"level": "art-reference", "proves": "real ART on a reference image; authoritative for the stated API/ABI"},
            {"level": "physical-device", "proves": "a real device/OEM target"},
            {"level": "simulated", "proves": "the mock path only; not an Android success"}
        ],
        "supported": supported,
        "unsupported": unsupported,
        "backend": {
            "binary": info.binary.as_ref().map(|p| p.display().to_string()),
            "version": info.version,
            "probed": caps.probed,
            "probe_error": caps.probe_error,
        },
        "available_command_fidelity": infos.iter()
            .filter(|t| t.available)
            .filter_map(|t| t.command.map(|c| (t.tool_name.as_str(), command_fidelity(c).as_str())))
            .map(|(tool, fidelity)| json!({"tool": tool, "fidelity": fidelity}))
            .collect::<Vec<_>>(),
        "default_command_fidelity": fidelity.as_str(),
        "compatibility": compatibility,
        "markdown": markdown,
        // The agent guide (embedded from `docs/AGENT_GUIDE.md`): the full text,
        // the requested topic when one was asked for, and the topic list.
        "guide": guide(topic),
        "guide_topic": topic.filter(|t| TOPICS.contains(t)),
        "guide_topics": TOPICS,
        "guide_source": "docs/AGENT_GUIDE.md",
    })
}

fn render_markdown(supported: &[&str], unsupported: &[Value]) -> String {
    let mut out = String::new();
    out.push_str("# ACOS via MCP\n\n");
    out.push_str("Loop: **inspect -> verify -> read verdict -> fix -> rerun**.\n\n");
    out.push_str("## Currently available tools\n\n");
    if supported.is_empty() {
        out.push_str("- (none — the `acos` backend was not found)\n");
    } else {
        for tool in supported {
            out.push_str(&format!("- `{tool}`\n"));
        }
    }
    out.push_str("\n## Not implemented yet (returns `unsupported`, never a pass)\n\n");
    for item in unsupported {
        let tool = item.get("tool").and_then(Value::as_str).unwrap_or("?");
        let reason = item
            .get("reason")
            .and_then(Value::as_str)
            .unwrap_or("not implemented");
        out.push_str(&format!("- `{tool}` — {reason}\n"));
    }
    out.push_str("\nCall `acos_help` with a topic for the full playbook, or `acos_capabilities` to re-probe.\n");
    out
}

/// Read the headline lines of `COMPATIBILITY.md` without slurping the whole file.
fn read_compatibility_summary(root: &Path) -> Option<String> {
    let path = root.join("COMPATIBILITY.md");
    let text = std::fs::read_to_string(&path).ok()?;
    let mut lines = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("> **As of") {
            lines.push(trimmed.trim_start_matches('>').trim().to_string());
        }
    }
    if lines.is_empty() {
        None
    } else {
        Some(lines.join(" "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn help_lists_supported_and_unsupported() {
        let mut caps = Capabilities::unprobed();
        caps.supported.insert(acos_protocol::CommandName::Inspect);
        let info = BackendInfo {
            binary: None,
            version: None,
        };
        let help = build_help(&caps, &info, None);
        assert!(help["supported"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v == "acos_inspect"));
        assert!(help["markdown"].as_str().unwrap().contains("acos_inspect"));
        assert!(help["loop"].as_str().unwrap().contains("inspect"));
    }

    #[test]
    fn compatibility_summary_is_read_when_present() {
        let dir = std::env::temp_dir().join(format!("acos-help-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("COMPATIBILITY.md"),
            "> **As of 2026-11-10: static DEX/APK analysis is verified; nothing executes.**\n",
        )
        .unwrap();
        let caps = Capabilities::unprobed();
        let info = BackendInfo {
            binary: None,
            version: None,
        };
        let help = build_help(&caps, &info, Some(&dir));
        assert!(help["compatibility"]["summary"]
            .as_str()
            .unwrap()
            .contains("static DEX/APK analysis"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
