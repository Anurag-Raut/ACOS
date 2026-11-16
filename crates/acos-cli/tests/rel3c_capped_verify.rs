//! REL3C integration check: a capped `verify --json` reply must still answer.
//!
//! REL3C replaced a reply that exceeds `MAX_REPLY_BYTES` with a small pointer
//! object (`reply_capped`, `verdict_path`, `artifacts_dir`, ...). The v0.3.0
//! integration review asked for the missing half: the pointer carried *where*
//! the evidence was and nothing about *what happened*, so an agent had to open
//! `verdict.json` to learn that its scenario failed, which step failed, and
//! which APK/scenario produced the run. Those are single scalars and two small
//! maps; they are not the bulk.
//!
//! A live Gaten run is the case that overflows (a real screen tree is
//! 400 KB-1 MB); the unit test in `acos-protocol::reply` covers the shaping rule
//! itself. These tests boot a real ART guest, so they are `#[ignore]`d like the
//! other ART integration tests:
//!
//! ```sh
//! tools/art-runtime/run-exclusive.sh --task INTEG030 -- \
//!   tools/guarded.sh --mem 12G --time 900 -- \
//!   cargo test --offline -p acos-cli --test rel3c_capped_verify -- --ignored --test-threads=1
//! ```

use std::path::{Path, PathBuf};

use assert_cmd::Command;
use serde_json::Value;

fn workspace() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// The gitignored x86_64 release Gaten APK. It is never committed; override
/// with `ACOS_GATEN_APK` or drop the build at the default path.
fn gaten_apk() -> Option<PathBuf> {
    let apk = std::env::var_os("ACOS_GATEN_APK")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            workspace().join("../../.cache/gaten/gaten-dev-current-x86_64-release.apk")
        });
    apk.is_file().then_some(apk)
}

fn scratch(name: &str) -> PathBuf {
    let dir = workspace().join(".cache/cli-rel3c-capped").join(name);
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

/// Run one live ART verify and return `(reply, reply_bytes, artifacts)`.
fn live_verify(scenario: &str, apk: &Path, artifacts: &Path) -> (Value, usize, PathBuf) {
    let output = Command::cargo_bin("acos")
        .expect("acos binary")
        .arg("verify")
        .arg(workspace().join("scenarios").join(scenario))
        .arg("--engine")
        .arg("art")
        .arg("--apk")
        .arg(apk)
        .arg("--artifacts")
        .arg(artifacts)
        .arg("--timeout")
        .arg("300")
        .arg("--json")
        .env_remove("ACOS_HOME")
        .output()
        .expect("run acos verify");
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let value: Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("stdout is not JSON ({e}):\n{stdout}\nstderr:\n{}",
                                   String::from_utf8_lossy(&output.stderr)));
    let bytes = stdout.len();
    (value, bytes, artifacts.to_path_buf())
}

/// The reply is capped, small, and still says: envelope outcome, verdict
/// outcome, and which APK/scenario this was.
#[test]
#[ignore = "boots a real ART guest; run under tools/guarded.sh + run-exclusive.sh"]
fn a_capped_live_verify_reply_keeps_the_outcome_and_provenance() {
    let Some(apk) = gaten_apk() else {
        eprintln!("SKIP: no Gaten x86_64 release APK (set ACOS_GATEN_APK)");
        return;
    };
    let artifacts = scratch("pass");
    let (reply, bytes, artifacts) = live_verify("gaten-home.yaml", &apk, &artifacts);

    // The reply really went through the cap (the point of the test).
    assert_eq!(
        reply["result"]["reply_capped"],
        json_true(),
        "the live Gaten reply must exceed the cap: {bytes} bytes\n{reply:#?}"
    );
    assert!(
        bytes <= 20 * 1024,
        "a capped reply must fit the cap: {bytes} bytes"
    );

    // The bulk is on disk, and it is bigger than the cap.
    let verdict_path = reply["result"]["verdict_path"]
        .as_str()
        .unwrap_or_else(|| panic!("capped reply names no verdict_path: {reply:#?}"));
    let verdict: Value = serde_json::from_str(
        &std::fs::read_to_string(verdict_path).expect("read verdict.json"),
    )
    .expect("verdict.json is JSON");
    assert!(
        std::fs::metadata(verdict_path).expect("stat verdict.json").len() > 20 * 1024,
        "verdict.json must be the bulk that was dropped"
    );

    // 1. Envelope status/error survive untouched (the cap only replaces
    //    `result`, never the envelope).
    let envelope_status = reply["status"]
        .as_str()
        .unwrap_or_else(|| panic!("capped reply lost the envelope status: {reply:#?}"));
    assert_eq!(envelope_status, verdict["status"].as_str().unwrap());
    if envelope_status == "fail" {
        assert!(
            reply["error"]["code"].is_string(),
            "a failing envelope must keep its error: {reply:#?}"
        );
    }

    // 2. Verdict status.
    assert_eq!(
        reply["result"]["verdict_status"],
        verdict["status"],
        "the capped reply must carry the verdict status"
    );

    // 3. Provenance: the APK/scenario identity REL3B records, plus the
    //    host-observed vs guest-asserted evidence classes.
    let provenance = &reply["result"]["provenance"];
    assert!(
        provenance["apk"]["sha256"].is_string(),
        "the capped reply must carry the APK provenance: {reply:#?}"
    );
    assert_eq!(
        provenance["apk"]["sha256"], verdict["app"]["sha256"],
        "the capped provenance must be the verdict's own APK digest"
    );
    assert!(
        provenance["scenario"]["sha256"].is_string(),
        "the capped reply must carry the scenario provenance"
    );
    assert_eq!(
        reply["result"]["evidence_provenance"],
        verdict["evidence_provenance"],
        "the capped reply must carry the evidence provenance map"
    );

    // A pass has no failing step, so the summary must not invent one.
    assert!(
        reply["result"].get("failed_step").is_none(),
        "a passing run must not carry a failed_step: {reply:#?}"
    );

    // Only the bulk is gone.
    assert!(reply["result"].get("tree").is_none());
    assert!(reply["result"].get("verdict").is_none());

    let _ = std::fs::remove_dir_all(&artifacts);
}

/// The failing case: the capped reply names the failing step (index, kind,
/// message) instead of only a path.
#[test]
#[ignore = "boots a real ART guest; run under tools/guarded.sh + run-exclusive.sh"]
fn a_capped_live_failing_verify_reply_names_the_failing_step() {
    let Some(apk) = gaten_apk() else {
        eprintln!("SKIP: no Gaten x86_64 release APK (set ACOS_GATEN_APK)");
        return;
    };
    let artifacts = scratch("neg");
    let (reply, bytes, artifacts) = live_verify("negative/gaten-home-wrong.yaml", &apk, &artifacts);

    assert!(
        bytes <= 20 * 1024,
        "a capped reply must fit the cap: {bytes} bytes"
    );
    assert_eq!(
        reply["result"]["reply_capped"],
        json_true(),
        "the negative Gaten reply must exceed the cap: {bytes} bytes\n{reply:#?}"
    );
    // The negative control must never pass.
    assert_eq!(reply["status"], "fail", "{reply:#?}");
    assert_eq!(reply["result"]["verdict_status"], "fail");

    let verdict_path = reply["result"]["verdict_path"].as_str().expect("verdict_path");
    let verdict: Value = serde_json::from_str(
        &std::fs::read_to_string(verdict_path).expect("read verdict.json"),
    )
    .expect("verdict.json is JSON");
    let failing = verdict["steps"]
        .as_array()
        .expect("steps")
        .iter()
        .find(|step| step["status"] != "pass")
        .expect("the negative scenario fails a step");

    let summary = &reply["result"]["failed_step"];
    assert_eq!(
        summary["index"], failing["index"],
        "the capped reply must name the failing step index: {reply:#?}"
    );
    assert_eq!(
        summary["kind"], failing["step"],
        "the capped reply must name the failing step kind: {reply:#?}"
    );
    assert_eq!(
        summary["message"], failing["error"]["message"],
        "the capped reply must carry the failing step's message: {reply:#?}"
    );
    // Envelope error survives too.
    assert_eq!(reply["error"]["code"], "ASSERTION_FAILED", "{reply:#?}");

    let _ = std::fs::remove_dir_all(&artifacts);
}

fn json_true() -> Value {
    Value::Bool(true)
}
