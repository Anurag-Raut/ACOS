//! V1 §4 negative suite: the broken fixtures must never pass.
//!
//! Each case runs the **product command**
//! (`acos verify --apk … --scenario … --json --artifacts …`) against a
//! deliberately broken APK built by `tools/build-negative-apps.sh` and asserts
//! the verdict is exactly the honest non-pass (fail / crash / unsupported) with
//! a structured error. The final assertion is the V1 metric: **zero false
//! passes**.
//!
//! Gated on the gitignored fixture APKs and the boot classpath; when either is
//! absent the test prints the exact build command and skips (the same
//! convention as the other fixture-gated tests), instead of reporting a green
//! run that never executed anything.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::Value;

fn workspace() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn scratch(name: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "acos-negative-verify-{}-{n}-{name}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn boot_dir() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("ACOS_BOOT_DIR").map(PathBuf::from) {
        if path.is_dir() {
            return Some(path);
        }
    }
    let path = workspace().join(".cache/boot/android-34");
    path.is_dir().then_some(path)
}

struct Case {
    apk: &'static str,
    scenario: &'static str,
    expected: &'static str,
    native: bool,
}

#[test]
fn broken_fixtures_never_false_pass() {
    let Some(boot) = boot_dir() else {
        eprintln!(
            "SKIP broken_fixtures_never_false_pass: no boot classpath; run tools/extract-bootclasspath.sh"
        );
        return;
    };
    let apps = workspace().join("fixtures/build/apps");
    let cases = [
        Case {
            apk: "broken-wrong-text.apk",
            scenario: "scenarios/v1-counter.yaml",
            expected: "fail",
            native: false,
        },
        Case {
            apk: "broken-crash.apk",
            scenario: "scenarios/v1-hello-views.yaml",
            expected: "crash",
            native: false,
        },
        Case {
            apk: "broken-missing-symbol.apk",
            scenario: "scenarios/hello-jni.yaml",
            expected: "unsupported",
            native: true,
        },
    ];
    let missing: Vec<&str> = cases
        .iter()
        .map(|case| case.apk)
        .filter(|apk| !apps.join(apk).is_file())
        .collect();
    if !missing.is_empty() {
        eprintln!(
            "SKIP broken_fixtures_never_false_pass: build fixtures first: \
             `tools/build-apps.sh && tools/build-negative-apps.sh` (missing {missing:?})"
        );
        return;
    }

    let session_root = scratch("session-root");
    std::fs::create_dir_all(&session_root).expect("create session root");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&session_root, std::fs::Permissions::from_mode(0o700))
            .expect("private session root");
    }

    let mut false_passes = 0u32;
    let mut report = Vec::new();
    for case in cases {
        let artifacts = scratch(case.apk.trim_end_matches(".apk"));
        let mut command = Command::new(env!("CARGO_BIN_EXE_acos"));
        command
            .current_dir(workspace())
            .arg("verify")
            .arg("--apk")
            .arg(apps.join(case.apk))
            .arg("--scenario")
            .arg(workspace().join(case.scenario))
            .arg("--json")
            .arg("--artifacts")
            .arg(&artifacts)
            .env("ACOS_BOOT_DIR", &boot)
            .env("ACOS_RUNTIME_DIR", scratch("runtime"))
            .env("ACOS_SESSION_ROOT", &session_root)
            .env("ACOS_NO_BOOT_CACHE", "1");
        if case.native {
            command.arg("--native");
        }
        let output = command.output().expect("run acos verify");
        let stdout = String::from_utf8_lossy(&output.stdout);
        let envelope: Value = serde_json::from_str(&stdout).unwrap_or_else(|error| {
            panic!(
                "{}: verify did not emit a JSON envelope ({error}); stderr: {}",
                case.apk,
                String::from_utf8_lossy(&output.stderr)
            )
        });
        let status = envelope["status"].as_str().unwrap_or("<missing>");
        let failure = &envelope["error"];
        let code = failure["code"].as_str().unwrap_or("<missing>");
        let ok = envelope["ok"].as_bool().unwrap_or(false);
        if ok || status == "pass" {
            false_passes += 1;
        }
        assert_eq!(
            status, case.expected,
            "{}: expected verdict `{}`, got `{}` (error {:?})",
            case.apk, case.expected, status, failure
        );
        assert!(
            !ok,
            "{}: a broken fixture reported ok=true (false pass)",
            case.apk
        );
        assert_ne!(code, "<missing>", "{}: no structured error code", case.apk);
        assert!(
            !failure["message"].as_str().unwrap_or("").is_empty(),
            "{}: structured error has no message",
            case.apk
        );
        match case.expected {
            "fail" => {
                assert_eq!(code, "ASSERTION_FAILED", "{}: {failure}", case.apk);
                assert!(
                    failure["message"]
                        .as_str()
                        .unwrap_or("")
                        .contains("expected"),
                    "{}: assertion failure must name the mismatch: {failure}",
                    case.apk
                );
                let steps = envelope["result"]["verdict"]["steps"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default();
                let has_diff = steps.iter().any(|step| {
                    step["assertions"]
                        .as_array()
                        .is_some_and(|assertions| assertions.iter().any(|a| a["diff"].is_object()))
                });
                assert!(
                    has_diff,
                    "{}: a failing step must carry the structured assertion diff: {steps:?}",
                    case.apk
                );
            }
            "crash" => {
                assert_eq!(code, "APP_CRASHED", "{}: {failure}", case.apk);
                assert!(
                    failure["details"]["exception"].is_string(),
                    "{}: crash must name the exception: {failure}",
                    case.apk
                );
            }
            "unsupported" => {
                assert_eq!(
                    code, "UNSUPPORTED_NATIVE_SYMBOL",
                    "{}: {failure}",
                    case.apk
                );
                assert!(
                    failure["message"]
                        .as_str()
                        .unwrap_or("")
                        .contains("missingSymbol"),
                    "{}: unsupported error must name the exact symbol: {failure}",
                    case.apk
                );
            }
            other => panic!("unexpected expectation {other}"),
        }
        report.push(format!("{} -> {} ({code})", case.apk, status));
        let _ = std::fs::remove_dir_all(&artifacts);
    }
    eprintln!("negative suite: {}", report.join(", "));
    assert_eq!(
        false_passes, 0,
        "V1 §4.4: false-pass count must be 0, got {false_passes}"
    );
}
