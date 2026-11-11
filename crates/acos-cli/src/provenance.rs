//! Provenance labels for every evidence item in an ART-engine verdict.
//!
//! The labels are the decision in `docs/SANDBOX_CERTIFICATION.md` §7:
//!
//! * **host-observed** — the host produced or read the bytes itself: C1 pixels,
//!   host-injected inputs, host-observed binder transactions, the static APK
//!   manifest.
//! * **guest-asserted** — the guest reported it: the View tree, lifecycle and
//!   launch markers, input acknowledgments, and logs.
//!
//! A label is recorded on the run (`trust`, `evidence_provenance`) and on every
//! step (`steps[].provenance`), so no reader has to infer where a claim came
//! from. On an untrusted run a guest-asserted item never justifies a pass.

use serde_json::{json, Value};

use crate::trust::Trust;

/// The provenance string for host-observed evidence.
pub const HOST_OBSERVED: &str = "host-observed";
/// The provenance string for guest-asserted evidence.
pub const GUEST_ASSERTED: &str = "guest-asserted";

/// The run-level provenance map and trust label.
pub fn run_evidence(trust: Trust) -> Value {
    json!({
        "trust": trust.as_str(),
        "evidence_provenance": {
            "static_manifest": HOST_OBSERVED,
            "frames": HOST_OBSERVED,
            "host_input": HOST_OBSERVED,
            "binder_transactions": HOST_OBSERVED,
            "tree": GUEST_ASSERTED,
            "lifecycle_markers": GUEST_ASSERTED,
            "launch_markers": GUEST_ASSERTED,
            "input_acks": GUEST_ASSERTED,
            "logs": GUEST_ASSERTED,
            "crash_reports": GUEST_ASSERTED
        },
        "rule": if trust == Trust::Untrusted {
            "untrusted run: only host-observed evidence may justify a pass; tree-only assertions fail closed with INSUFFICIENT_HOST_EVIDENCE"
        } else {
            "trusted fixture: guest-asserted evidence may corroborate a pass, and its provenance is still recorded"
        },
    })
}

/// Per-step provenance for a step kind.
pub fn step_provenance(kind: &str) -> Value {
    match kind {
        "launch" => json!({
            "frame": HOST_OBSERVED,
            "launch_markers": GUEST_ASSERTED,
            "lifecycle_markers": GUEST_ASSERTED,
            "tree": GUEST_ASSERTED,
        }),
        "wait_for" | "assert_text" | "assert_visible" | "assert_not_visible" | "assert_state" => {
            json!({ "tree": GUEST_ASSERTED })
        }
        "tap" | "type" | "key" | "back" | "rotate" => json!({
            "host_input": HOST_OBSERVED,
            "frame": HOST_OBSERVED,
            "input_acks": GUEST_ASSERTED,
            "tree": GUEST_ASSERTED,
        }),
        "capture" | "assert_screenshot" | "assert_region" => json!({
            "frame": HOST_OBSERVED,
        }),
        "assert_log" => json!({ "logs": GUEST_ASSERTED }),
        "assert_no_crash" => json!({ "crash_reports": GUEST_ASSERTED }),
        _ => json!({}),
    }
}

/// Insert trust and provenance into the annotated verdict.
pub fn annotate_verdict(verdict: &mut Value, trust: Trust) {
    let Some(object) = verdict.as_object_mut() else {
        return;
    };
    let run = run_evidence(trust);
    object.insert("trust".to_string(), json!(trust.as_str()));
    object.insert(
        "evidence_provenance".to_string(),
        run["evidence_provenance"].clone(),
    );
    object.insert("provenance_rule".to_string(), run["rule"].clone());
    if let Some(steps) = object
        .get_mut("steps")
        .and_then(|steps| steps.as_array_mut())
    {
        for step in steps {
            let kind = step
                .get("step")
                .and_then(|value| value.as_str())
                .unwrap_or("")
                .to_string();
            if let Some(step_object) = step.as_object_mut() {
                step_object.insert("provenance".to_string(), step_provenance(&kind));
            }
        }
    }
}

/// Put the same labels on each `steps/*.json` artifact, so a reader who opens a
/// single step file does not have to infer provenance from the verdict.
pub fn annotate_step_artifacts(verdict: &Value, artifacts: &std::path::Path, trust: Trust) {
    let Some(steps) = verdict.get("steps").and_then(|steps| steps.as_array()) else {
        return;
    };
    for step in steps {
        let kind = step
            .get("step")
            .and_then(|value| value.as_str())
            .unwrap_or("");
        let Some(path) = step
            .get("evidence")
            .and_then(|evidence| evidence.as_array())
            .and_then(|evidence| {
                evidence.iter().find_map(|item| {
                    (item.get("kind").and_then(|kind| kind.as_str()) == Some("step"))
                        .then(|| item.get("path").and_then(|path| path.as_str()))
                        .flatten()
                })
            })
        else {
            continue;
        };
        let file = artifacts.join(path);
        let Ok(text) = std::fs::read_to_string(&file) else {
            continue;
        };
        let Ok(mut value) = serde_json::from_str::<Value>(&text) else {
            continue;
        };
        if let Some(object) = value.as_object_mut() {
            object.insert("trust".to_string(), json!(trust.as_str()));
            object.insert("provenance".to_string(), step_provenance(kind));
        }
        let _ = std::fs::write(
            &file,
            serde_json::to_string_pretty(&value).unwrap_or_default(),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tree_is_guest_asserted_and_pixels_are_host_observed() {
        assert_eq!(step_provenance("assert_text")["tree"], GUEST_ASSERTED);
        assert_eq!(step_provenance("assert_region")["frame"], HOST_OBSERVED);
        assert_eq!(step_provenance("tap")["host_input"], HOST_OBSERVED);
        assert_eq!(step_provenance("tap")["input_acks"], GUEST_ASSERTED);
        assert_eq!(step_provenance("launch")["launch_markers"], GUEST_ASSERTED);
        assert_eq!(step_provenance("assert_log")["logs"], GUEST_ASSERTED);
    }

    #[test]
    fn every_step_gets_a_provenance_object() {
        let mut verdict = json!({
            "steps": [
                {"step": "launch"},
                {"step": "capture"},
                {"step": "assert_region"}
            ]
        });
        annotate_verdict(&mut verdict, Trust::Untrusted);
        assert_eq!(verdict["trust"], "untrusted");
        assert_eq!(
            verdict["steps"][0]["provenance"]["launch_markers"],
            GUEST_ASSERTED
        );
        assert_eq!(verdict["steps"][1]["provenance"]["frame"], HOST_OBSERVED);
        assert_eq!(verdict["steps"][2]["provenance"]["frame"], HOST_OBSERVED);
    }
}
