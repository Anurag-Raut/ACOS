//! Integration tests for the agent-facing session commands:
//! `acos launch` / `observe` / `tap` / `key` / `swipe` / `logs` / `session`.
//!
//! The launch test is the heavy one: it boots the real framework bytecode. It
//! asserts the **shape** of the honest ladder — `bind_application` reached,
//! `first_frame` blocked with a named blocker, and a `reached_stage` that is
//! consistent with the per-stage flags — rather than hard-coding which
//! lifecycle callback the in-flight reflection/lifecycle workstream has
//! reached. The remaining tests are fast and assert the structured refusals
//! that must never become false passes.

use std::path::PathBuf;
use std::process::Output;

use assert_cmd::Command;
use serde_json::Value;

fn workspace_path(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel)
}

fn run(args: &[&str]) -> Output {
    Command::cargo_bin("acos")
        .expect("acos binary")
        // These tests assert the in-process fallback (no daemon running); keep
        // them hermetic so an ambient `acos serve` cannot change their result.
        .env("ACOS_NO_DAEMON", "1")
        .args(args)
        .output()
        .expect("run acos")
}

fn parse_json(bytes: &[u8]) -> Value {
    serde_json::from_slice(bytes).unwrap_or_else(|e| {
        panic!(
            "stdout should be valid JSON: {e}\n{}",
            String::from_utf8_lossy(bytes)
        )
    })
}

const STAGES: &[&str] = &[
    "apk_parsed",
    "process_booted",
    "bind_application",
    "application_on_create",
    "client_transaction",
    "activity_on_create",
    "on_resume",
    "first_frame",
];

#[test]
fn launch_counter_reports_the_honest_stage_ladder() {
    let apk = workspace_path("../../fixtures/build/apps/counter.apk");
    if !apk.is_file() {
        eprintln!("SKIP launch_counter...: counter.apk fixture missing");
        return;
    }
    let out = run(&[
        "launch",
        apk.to_str().unwrap(),
        "--json",
        "--timeout",
        "180",
    ]);
    let v = parse_json(&out.stdout);
    let passed = v["status"] == "pass";

    // The boot classpath may be absent on a non-oracle host: skip loudly.
    let Some(result) = v.get("result").filter(|r| !r.is_null()) else {
        assert!(!out.status.success(), "a launch without a ladder cannot pass");
        assert_eq!(v["ok"], false);
        let error = &v["error"];
        let message = error["message"].as_str().unwrap_or("");
        if message.to_ascii_lowercase().contains("boot classpath")
            || message.to_ascii_lowercase().contains("bootclasspath")
        {
            eprintln!("SKIP launch_counter...: boot classpath missing: {message}");
            return;
        }
        panic!("launch produced no ladder: {message}");
    };

    if !passed {
        // A non-pass envelope carries an error and `ok:false`.
        assert!(!out.status.success(), "a non-pass launch must exit non-zero");
        assert_eq!(v["ok"], false);
        let error = &v["error"];
        assert!(error["code"].as_str().is_some_and(|c| !c.is_empty()));
    }

    // The ladder is present, ordered and complete.
    let stages = result["stages"].as_array().expect("stages array");
    let names: Vec<&str> = stages
        .iter()
        .map(|s| s["stage"].as_str().expect("stage name"))
        .collect();
    assert_eq!(names, STAGES);

    let status_of = |name: &str| -> &Value {
        stages
            .iter()
            .find(|s| s["stage"] == name)
            .expect("stage present")
    };

    // `bindApplication` is delivered today; the ladder must say so.
    assert_eq!(status_of("bind_application")["ok"], true);
    assert_eq!(status_of("process_booted")["ok"], true);
    assert_eq!(status_of("apk_parsed")["ok"], true);

    // `first_frame` is reached on the oracle host (W133-A's real
    // `ThreadedRenderer.draw` pipeline); otherwise it carries the structured
    // blocker.
    let first_frame = status_of("first_frame");
    if passed {
        assert_eq!(first_frame["ok"], true, "a pass must reach first_frame");
        assert_eq!(result["reached_stage"], "first_frame");
        for (idx, stage) in stages.iter().enumerate() {
            assert_eq!(stage["ok"], true, "stage {idx} must be ok on a pass");
        }
        return;
    }
    assert_eq!(first_frame["ok"], false);
    assert!(
        first_frame["blocker"].is_object(),
        "first_frame must carry the structured blocker: {first_frame}"
    );
    let blocker = &first_frame["blocker"];
    assert!(
        blocker["code"].as_str().is_some_and(|c| !c.is_empty()),
        "blocker needs a stable code"
    );
    assert!(
        blocker["message"].as_str().is_some_and(|m| !m.is_empty()),
        "blocker needs a message"
    );

    // Honesty invariant: every rung at or before `reached_stage` is ok, and
    // the rung right after it is not. This can fail if the ladder is mislabeled.
    let reached = result["reached_stage"].as_str().expect("reached_stage");
    let reached_idx = names
        .iter()
        .position(|n| *n == reached)
        .expect("known stage");
    for (idx, stage) in stages.iter().enumerate() {
        let ok = stage["ok"].as_bool().unwrap();
        if idx <= reached_idx {
            assert!(ok, "stage {idx} <= reached must be ok");
        } else {
            assert!(!ok, "stage {idx} > reached must not be ok");
        }
    }

    // Exactly one rung is the `blocked` one; the rest of the failures are
    // `not_reached` and carry the same root blocker.
    let blocked: Vec<&Value> = stages.iter().filter(|s| s["status"] == "blocked").collect();
    assert_eq!(blocked.len(), 1, "exactly one blocked rung: {stages:?}");
    for stage in stages {
        if stage["ok"] == false {
            assert!(
                stage["blocker"].is_object(),
                "every non-ok rung carries the root blocker: {stage}"
            );
        }
    }

    // Resource accounting is present.
    assert!(result["classes_loaded"].as_u64().unwrap_or(0) > 0);
    assert!(result["wall_ms"].is_number());
}

#[test]
fn observe_before_any_frame_is_not_ready() {
    let out = run(&["observe", "--json"]);
    assert!(
        !out.status.success(),
        "observe without a frame must not be a pass"
    );
    let v = parse_json(&out.stdout);
    assert_eq!(v["ok"], false);
    assert_eq!(v["status"], "unsupported");
    assert_eq!(v["error"]["code"], "UNSUPPORTED_BACKEND");
    assert_eq!(v["error"]["details"]["reason"], "not_ready");
    assert!(v["result"].is_null(), "no empty/blank result on not_ready");
}

#[test]
fn tap_without_a_window_is_a_structured_error() {
    let out = run(&["tap", "10", "20", "--json"]);
    assert!(!out.status.success());
    let v = parse_json(&out.stdout);
    assert_eq!(v["ok"], false);
    assert_eq!(v["error"]["code"], "ELEMENT_NOT_FOUND");
    assert_eq!(v["error"]["details"]["kind"], "NoWindowAtPoint");
    assert_eq!(v["error"]["details"]["x"], "10");
    assert_eq!(v["error"]["details"]["y"], "20");
}

#[test]
fn key_without_focus_is_a_structured_error() {
    let out = run(&["key", "back", "--json"]);
    assert!(!out.status.success());
    let v = parse_json(&out.stdout);
    assert_eq!(v["ok"], false);
    assert_eq!(v["error"]["code"], "ELEMENT_NOT_FOUND");
    assert_eq!(v["error"]["details"]["kind"], "NoFocusedWindow");
}

#[test]
fn swipe_without_a_window_is_a_structured_error() {
    let out = run(&["swipe", "10", "20", "30", "40", "--json"]);
    assert!(!out.status.success());
    let v = parse_json(&out.stdout);
    assert_eq!(v["error"]["code"], "ELEMENT_NOT_FOUND");
    assert_eq!(v["error"]["details"]["kind"], "NoWindowAtPoint");
}

#[test]
fn tap_text_without_a_tree_is_a_structured_error() {
    // No app has been launched, so there is no View hierarchy to resolve the
    // label against; the honest answer is a backend error, not a false hit.
    let out = run(&["tap", "--text", "increment", "--json"]);
    assert!(!out.status.success());
    let v = parse_json(&out.stdout);
    assert_eq!(v["error"]["code"], "UNSUPPORTED_BACKEND");
    assert_eq!(v["error"]["details"]["reason"], "not_ready");
    assert_eq!(v["error"]["details"]["capability"], "ui.tree");
}

#[test]
fn logs_returns_a_buffer_even_when_empty() {
    let out = run(&["logs", "--json"]);
    assert!(out.status.success());
    let v = parse_json(&out.stdout);
    assert_eq!(v["ok"], true);
    assert_eq!(v["status"], "pass");
    assert!(v["result"]["logs"].is_array());
    assert!(v["result"]["count"].is_number());
}

#[test]
fn session_status_reports_an_unlaunched_session() {
    let out = run(&["session", "status", "--json"]);
    assert!(out.status.success());
    let v = parse_json(&out.stdout);
    assert_eq!(v["result"]["launched"], false);
    assert_eq!(v["result"]["live"], true);
}

#[test]
fn launch_non_apk_is_an_error_not_a_pass() {
    // Negative test: a non-APK path must never produce a ladder or a pass.
    let not_apk = workspace_path("Cargo.toml");
    let out = run(&["launch", not_apk.to_str().unwrap(), "--json"]);
    assert!(!out.status.success());
    let v = parse_json(&out.stdout);
    assert_eq!(v["ok"], false);
    assert_ne!(v["status"], "pass");
    assert_eq!(v["error"]["code"], "APK_NOT_ZIP");
    assert!(v["result"].is_null());
}

#[test]
fn tap_with_one_coordinate_is_invalid_argument() {
    let out = run(&["tap", "10", "--json"]);
    assert!(!out.status.success());
    let v = parse_json(&out.stdout);
    assert_eq!(v["error"]["code"], "INVALID_ARGUMENT");
}
