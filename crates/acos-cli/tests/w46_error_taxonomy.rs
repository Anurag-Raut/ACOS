//! W46: failure taxonomy with teeth.
//!
//! Every test drives the real `acos verify --engine art` path against a fake
//! `acos-art` child (the same seam `verify_engine_art.rs` uses) and asserts the
//! exact envelope: code, class, retryability, status and rc.
//!
//! Covered:
//! * each host/harness launch code -> its registered protocol code;
//! * a mid-session reply code -> its protocol code (never `INTERNAL`);
//! * an unregistered art code -> `INTERNAL` *with a hint naming it*;
//! * a bare EOF classified from `session-exit.json` (guest signal/stage);
//! * an injected ENOSPC on the first artifact write -> retryable
//!   `HOST_DISK_FULL`, nonzero rc, non-empty stderr;
//! * an injected panic -> `HOST_PANIC` envelope on stderr, rc 1 (not 101).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use assert_cmd::Command;
use serde_json::Value;

fn workspace() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn scratch(name: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("acos-w46-{}-{n}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

/// The fake session. `FAKE_W46_MODE` selects the behaviour:
/// * `ready_code` — the ready line is a structured refusal with `FAKE_W46_CODE`;
/// * `reply_code` — the ready line is valid, the first `tree` reply is a refusal
///   with `FAKE_W46_CODE`;
/// * `disconnect_facts` — the ready line is valid, then the child writes
///   `session-exit.json` (fact-driven cause) and closes stdout;
/// * `ok` — a complete scripted session (for the write-failure test).
const SESSION_SCRIPT: &str = r##"#!/usr/bin/env python3
import base64, hashlib, json, os, sys

PNG = base64.b64decode(
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAIAAACQd1PeAAAADElEQVR4nGP4//8/AAX+Av4N70a4AAAAAElFTkSuQmCC"
)
out = os.environ["FAKE_W46_OUT"]
os.makedirs(out, exist_ok=True)
mode = os.environ.get("FAKE_W46_MODE", "ok")
code = os.environ.get("FAKE_W46_CODE", "HOST_DISK_FULL")
facts = os.environ.get("FAKE_W46_FACTS", "")
state = {"count": 0, "seq": 0}

def write_exit(doc):
    with open(os.path.join(out, "session-exit.json"), "w") as f:
        json.dump(doc, f)

def tree():
    return {"nodes": [{
        "class": "android.widget.LinearLayout",
        "bounds": "[0,0][1080,2400]", "visibility": 0,
        "children": [
            {"class": "android.widget.TextView",
             "id": "dev.acos.fixtures.counter:id/count",
             "text": str(state["count"]), "bounds": "[42,317][63,368]", "visibility": 0},
            {"class": "android.widget.Button",
             "id": "dev.acos.fixtures.counter:id/inc",
             "text": "Increment", "bounds": "[42,368][273,494]",
             "clickable": True, "visibility": 0},
        ],
    }]}

def frame(cmd, with_tree):
    state["seq"] += 1
    data = PNG + ("#count=%d;seq=%d" % (state["count"], state["seq"])).encode()
    path = os.path.join(out, "frame_%d.png" % state["seq"])
    with open(path, "wb") as f:
        f.write(data)
    reply = {"ok": True, "cmd": cmd, "frame_seq": state["seq"], "png": path,
             "md5": hashlib.md5(data).hexdigest()}
    if with_tree:
        reply["tree"] = tree()
    return reply

if mode == "ready_code":
    write_exit({"outcome": "unsupported", "code": code, "retryable": code == "HOST_DISK_FULL",
                "stage": "preflight", "guest_exit": None, "guest_signal": None})
    print(json.dumps({"ok": False, "cmd": "ready", "code": code,
                      "retryable": code == "HOST_DISK_FULL",
                      "error": "scripted ready refusal (%s)" % code}), flush=True)
    sys.exit(0)

print(json.dumps(frame("ready", True)), flush=True)

for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    try:
        request = json.loads(line)
    except Exception:
        break
    cmd = request.get("cmd", "")
    if cmd == "close":
        if mode == "ok":
            write_exit({"outcome": "ok", "code": None, "retryable": False,
                        "stage": "teardown-evidence", "guest_exit": 0, "guest_signal": None})
        print(json.dumps({"ok": True, "cmd": "close", "frame_seq": state["seq"]}), flush=True)
        break
    if mode == "reply_code" and cmd == "tree":
        write_exit({"outcome": "crash", "code": code, "retryable": False,
                    "stage": "guest-exec", "guest_exit": 1, "guest_signal": None})
        print(json.dumps({"ok": False, "cmd": cmd, "code": code,
                          "error": "scripted reply refusal (%s)" % code}), flush=True)
        continue
    if mode == "disconnect_facts" and cmd == "tree":
        write_exit(json.loads(facts))
        sys.stdout.flush()
        os._exit(0)
    reply = frame(cmd, True)
    if cmd == "tap":
        reply["tap"] = {"handled": True}
    print(json.dumps(reply), flush=True)
"##;

fn session_script(dir: &Path) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join("fake_art_w46.py");
    std::fs::write(&path, SESSION_SCRIPT).expect("write session script");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
        .expect("make the script executable");
    path
}

/// A launch-only scenario keeps the fake small and still reaches the verdict
/// artifact writes (the disk-failure test needs that).
fn launch_scenario(dir: &Path) -> PathBuf {
    let apk = workspace().join("fixtures/build/apps/counter.apk");
    assert!(
        apk.is_file(),
        "build fixtures first: `tools/build-apps.sh` (missing {})",
        apk.display()
    );
    let scenario = dir.join("w46-launch.yaml");
    std::fs::write(
        &scenario,
        format!(
            "version: 1\n\
             name: w46-launch\n\
             description: W46 failure taxonomy fixture\n\
             app: {apk}\n\
             engine: native-preferred\n\
             fallback: report-and-escalate\n\
             api_level: 34\n\
             isolation: process\n\
             display:\n  mode: offscreen\n  width: 1080\n  height: 2400\n  density_dpi: 420\n\
             capabilities:\n  - lifecycle\n  - ui.text\n\
             expectation: pass\n\
             budgets:\n  memory_mib: 512\n  wall_ms: 120000\n\
             steps:\n  - launch:\n      activity: dev.acos.fixtures.counter/.MainActivity\n",
            apk = apk.display()
        ),
    )
    .expect("write scenario");
    scenario
}

struct Run {
    output: std::process::Output,
    artifacts: PathBuf,
}

fn run(name: &str, mode: &str, code: &str, facts: &str, extra_env: &[(&str, &str)]) -> Run {
    let dir = scratch(name);
    let script = session_script(&dir);
    let artifacts = dir.join("artifacts");
    let out = artifacts.join("art-frames");
    std::fs::create_dir_all(&out).expect("frames dir");
    let scenario = launch_scenario(&dir);
    let mut command = Command::cargo_bin("acos").expect("acos binary");
    command
        .env("ACOS_NO_DAEMON", "1")
        .env("ACOS_ART_BIN", &script)
        .env("FAKE_W46_OUT", &out)
        .env("FAKE_W46_MODE", mode)
        .env("FAKE_W46_CODE", code)
        .env("FAKE_W46_FACTS", facts)
        .args([
            "verify",
            "--scenario",
            scenario.to_str().unwrap(),
            "--apk",
            workspace()
                .join("fixtures/build/apps/counter.apk")
                .to_str()
                .unwrap(),
            "--engine",
            "art",
            "--artifacts",
            artifacts.to_str().unwrap(),
            "--json",
        ]);
    for (key, value) in extra_env {
        command.env(key, value);
    }
    let output = command.output().expect("run acos verify");
    Run { output, artifacts }
}

fn envelope(run: &Run) -> Value {
    let stdout = String::from_utf8_lossy(&run.output.stdout);
    serde_json::from_str(&stdout).unwrap_or_else(|error| {
        panic!(
            "stdout must be one JSON envelope: {error}\nstdout: {stdout}\nstderr: {}",
            String::from_utf8_lossy(&run.output.stderr)
        )
    })
}

fn assert_code(run: &Run, code: &str, class: &str, retryable: bool, status: &str) {
    let envelope = envelope(run);
    let stderr = String::from_utf8_lossy(&run.output.stderr);
    assert!(
        !run.output.status.success(),
        "{code} must not exit 0\nstdout: {}\nstderr: {stderr}",
        String::from_utf8_lossy(&run.output.stdout)
    );
    assert_ne!(
        run.output.status.code(),
        Some(101),
        "{code} must be a classified failure, not a panic\nstderr: {stderr}"
    );
    assert_eq!(envelope["ok"], false, "{code}");
    assert_eq!(envelope["status"], status, "{code}");
    assert_eq!(envelope["error"]["code"], code, "{code}: {envelope}");
    assert_eq!(envelope["error"]["class"], class, "{code}: {envelope}");
    assert_eq!(envelope["error"]["retryable"], retryable, "{code}");
    assert!(
        envelope["error"]["hint"]
            .as_str()
            .is_some_and(|h| !h.is_empty()),
        "{code} needs a hint: {envelope}"
    );
    assert!(
        stderr.contains(code),
        "stderr must name the code {code}: {stderr}"
    );
}

#[test]
fn every_host_and_harness_launch_code_maps_to_its_registered_envelope() {
    // (art code, protocol code, class, retryable, envelope status)
    for (code, expected, class, retryable, status) in [
        (
            "HOST_DISK_FULL",
            "HOST_DISK_FULL",
            "host_guard",
            true,
            "blocked",
        ),
        ("HOST_PANIC", "HOST_PANIC", "harness", false, "error"),
        (
            "HOST_MEMORY_LIMIT",
            "HOST_MEMORY_LIMIT",
            "host_guard",
            true,
            "blocked",
        ),
        (
            "SECCOMP_UNAVAILABLE",
            "SECCOMP_UNAVAILABLE",
            "unsupported",
            false,
            "blocked",
        ),
        ("GUEST_EXITED", "GUEST_EXITED", "harness", false, "error"),
    ] {
        let run = run(&format!("ready-{code}"), "ready_code", code, "", &[]);
        assert_code(&run, expected, class, retryable, status);
        let envelope = envelope(&run);
        assert_eq!(
            envelope["error"]["details"]["art_code"], code,
            "{code} must keep the art code in details"
        );
        assert_eq!(
            envelope["error"]["details"]["session_exit"]["code"], code,
            "{code} must surface the session's own record"
        );
    }
}

#[test]
fn a_mid_session_reply_code_is_mapped_not_folded_into_internal() {
    for (code, expected, class, retryable, status) in [
        ("GUEST_EXITED", "GUEST_EXITED", "harness", false, "error"),
        ("TIMEOUT", "TIMEOUT", "infra_timeout", true, "timeout"),
        (
            "NO_FRESH_FRAME",
            "NO_FRESH_FRAME",
            "infra_timeout",
            true,
            "error",
        ),
        (
            "SECCOMP_UNAVAILABLE",
            "SECCOMP_UNAVAILABLE",
            "unsupported",
            false,
            "blocked",
        ),
    ] {
        let run = run(&format!("reply-{code}"), "reply_code", code, "", &[]);
        assert_code(&run, expected, class, retryable, status);
        let envelope = envelope(&run);
        assert_eq!(envelope["error"]["details"]["art_code"], code);
        assert_eq!(envelope["error"]["details"]["reply"]["code"], code);
    }
}

#[test]
fn an_unregistered_art_code_is_internal_with_a_hint_naming_it() {
    let run = run("reply-unknown", "reply_code", "SOMETHING_NEW", "", &[]);
    let envelope = envelope(&run);
    assert!(!run.output.status.success());
    assert_eq!(envelope["error"]["code"], "INTERNAL");
    assert_eq!(envelope["error"]["class"], "harness");
    let hint = envelope["error"]["hint"].as_str().unwrap_or_default();
    assert!(
        hint.contains("SOMETHING_NEW"),
        "the harness gap must name the unmapped code: {hint}"
    );
}

#[test]
fn a_bare_eof_is_classified_from_session_exit_json() {
    // SIGSYS = the guard refused a syscall: a host isolation condition.
    let sigsys = run(
        "eof-seccomp",
        "disconnect_facts",
        "",
        r#"{"outcome":"crash","code":null,"stage":"guest-exec","guest_exit":null,"guest_signal":31}"#,
        &[],
    );
    let sigsys_envelope = envelope(&sigsys);
    assert!(!sigsys.output.status.success());
    assert_ne!(sigsys.output.status.code(), Some(101));
    assert_eq!(
        sigsys_envelope["error"]["code"], "SECCOMP_UNAVAILABLE",
        "{sigsys_envelope}"
    );
    assert_eq!(sigsys_envelope["error"]["class"], "unsupported");
    assert_eq!(
        sigsys_envelope["error"]["details"]["session_exit"]["guest_signal"],
        31
    );
    assert_eq!(
        sigsys_envelope["error"]["details"]["session_exit"]["stage"],
        "guest-exec"
    );

    // SIGSEGV with an INTERNAL code is refined to APP_CRASHED: never INTERNAL
    // when the record carries the cause.
    let segv = run(
        "eof-segv",
        "disconnect_facts",
        "",
        r#"{"outcome":"crash","code":"INTERNAL","stage":"bind-application","guest_exit":null,"guest_signal":11}"#,
        &[],
    );
    let segv_envelope = envelope(&segv);
    assert_eq!(
        segv_envelope["error"]["code"], "APP_CRASHED",
        "{segv_envelope}"
    );
    assert_eq!(segv_envelope["error"]["class"], "app_crash");
}

#[test]
fn session_exit_json_is_surfaced_in_the_verdict_artifacts_on_a_pass() {
    let run = run("pass-session-exit", "ok", "", "", &[]);
    assert!(
        run.output.status.success(),
        "the scripted session must pass\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&run.output.stdout),
        String::from_utf8_lossy(&run.output.stderr)
    );
    let verdict: Value = serde_json::from_str(
        &std::fs::read_to_string(run.artifacts.join("verdict.json")).expect("verdict.json"),
    )
    .expect("verdict JSON");
    assert_eq!(verdict["verdict"], "pass");
    assert_eq!(
        verdict["session_exit"]["guest_exit"], 0,
        "the launcher's own exit record must be in the verdict: {verdict}"
    );
    assert_eq!(verdict["session_exit"]["stage"], "teardown-evidence");
    let engine: Value = serde_json::from_str(
        &std::fs::read_to_string(run.artifacts.join("engine.json")).expect("engine.json"),
    )
    .expect("engine JSON");
    assert_eq!(engine["engine"]["session_exit"]["outcome"], "ok");
}

#[test]
fn an_injected_enospc_write_is_a_retryable_host_disk_full_envelope() {
    let run = run(
        "disk-full",
        "ok",
        "",
        "",
        &[
            ("ACOS_FAULT_INJECT_WRITE", "ENOSPC"),
            ("ACOS_FAULT_INJECT_ALLOW", "1"),
        ],
    );
    let envelope = envelope(&run);
    let stderr = String::from_utf8_lossy(&run.output.stderr);
    assert!(run.artifacts.exists(), "the artifacts root must exist");
    assert!(
        !run.output.status.success(),
        "a failed write must fail the run"
    );
    assert_ne!(run.output.status.code(), Some(101), "never a bare panic");
    assert_eq!(envelope["error"]["code"], "HOST_DISK_FULL", "{envelope}");
    assert_eq!(envelope["error"]["class"], "host_guard");
    assert_eq!(envelope["error"]["retryable"], true);
    assert_eq!(envelope["status"], "blocked");
    assert!(
        !stderr.is_empty(),
        "stderr must not be empty on a host write failure"
    );
    assert!(stderr.contains("HOST_DISK_FULL"), "stderr: {stderr}");
}

#[test]
fn an_injected_panic_emits_a_host_panic_envelope_and_rc_one() {
    let output = Command::cargo_bin("acos")
        .expect("acos binary")
        .env("ACOS_FAULT_INJECT", "panic")
        .arg("--version")
        .output()
        .expect("run acos");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        output.status.code(),
        Some(1),
        "a caught harness panic exits 1, never 101\nstderr: {stderr}"
    );
    assert!(
        !stderr.is_empty(),
        "the panic envelope must reach stderr, even when the host disk is full"
    );
    assert!(
        stderr.contains("HOST_PANIC"),
        "stderr must carry the HOST_PANIC envelope: {stderr}"
    );
    assert!(
        stderr.contains("\"class\":\"harness\""),
        "the envelope must carry class=harness: {stderr}"
    );
}
