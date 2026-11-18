//! W44: trust/provenance re-audit for the hostile forest and probe-line shapes.
//!
//! These run the real `acos verify --engine art` CLI against a scripted ART
//! session (no ART sysroot needed) and prove:
//!
//! * an untrusted run cannot use a guest-declared two-window forest (or its
//!   `truncated:false` flag) to pass a tree-only assertion;
//! * the same forged forest on a trusted control is an honest
//!   `ELEMENT_NOT_FOUND`, never `TREE_TRUNCATED`;
//! * `wait_for`/`refresh` also fails closed on untrusted runs (the polling seam
//!   must not bypass `TrustGuard`);
//! * every step still carries its provenance labels.
//!
//! The live hostile fixture and hostile parcels are in
//! `scenarios/w44-hostile-*.yaml` + `results/art-runtime/W44.md`.

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
    let dir = std::env::temp_dir().join(format!("acos-w44-{}-{n}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

/// A forged two-window forest whose requested node is absent and which claims
/// `truncated:false`. The same sample is used by both trust modes.
const SESSION_SCRIPT: &str = r##"#!/usr/bin/env python3
import base64, hashlib, json, os, sys

PNG = base64.b64decode(
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAIAAACQd1PeAAAADElEQVR4nGP4//8/AAX+Av4N70a4AAAAAElFTkSuQmCC"
)
out = os.environ["FAKE_ART_OUT"]
os.makedirs(out, exist_ok=True)
state = {"seq": 0}

def tree():
    return {
        "nodes": [{
            "class": "android.widget.TextView", "id": "main-only",
            "text": "FORGED FOREST", "window": "main",
            "bounds": "[0,0][1080,2400]", "visibility": 0,
        }],
        "windows": [{"window": "main", "z": 0}, {"window": "ghost", "z": 1}],
        "window_count": 2, "node_budget": 2000, "truncated": False,
    }

def frame(cmd, with_tree):
    state["seq"] += 1
    path = os.path.join(out, "frame_%d.png" % state["seq"])
    data = PNG + ("#count=0;seq=%d" % state["seq"]).encode()
    with open(path, "wb") as f:
        f.write(data)
    reply = {"ok": True, "cmd": cmd, "frame_seq": state["seq"],
             "png": path, "md5": hashlib.md5(data).hexdigest()}
    if with_tree:
        reply["tree"] = tree()
    return reply

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
        print(json.dumps({"ok": True, "cmd": "close", "frame_seq": state["seq"]}), flush=True)
        break
    if cmd in ("tree", "tap", "key", "type_text"):
        reply = frame(cmd, True)
        if cmd == "tap":
            reply["gesture"] = {"acks": [{"seq": 1, "handled": True}]}
        print(json.dumps(reply), flush=True)
    elif cmd == "screenshot":
        print(json.dumps(frame(cmd, False)), flush=True)
    elif cmd == "logs":
        print(json.dumps({"ok": True, "cmd": "logs", "entries": [],
                          "total": 0, "dropped": 0}), flush=True)
    else:
        print(json.dumps({"ok": False, "cmd": cmd, "code": "BAD_CMD",
                          "error": "unknown command"}), flush=True)
"##;

fn session_script(dir: &Path) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join("fake_art_session.py");
    std::fs::write(&path, SESSION_SCRIPT).expect("write session script");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
        .expect("make executable");
    path
}

struct Run {
    output: std::process::Output,
    artifacts: PathBuf,
}

fn run_scenario(name: &str, steps: &str, trust: &str, allow_untrusted: bool) -> Run {
    let dir = scratch(name);
    let script = session_script(&dir);
    let artifacts = dir.join("artifacts");
    let out = dir.join("frames");
    std::fs::create_dir_all(&out).expect("frames");
    let scenario = dir.join("scenario.yaml");
    std::fs::write(
        &scenario,
        format!(
            "version: 1\nname: w44-{name}\n{trust}app: fixtures/build/apps/counter.apk\n\
             isolation: process\ndisplay:\n  mode: offscreen\n  width: 1080\n  height: 2400\n  \
             density_dpi: 420\nbudgets:\n  memory_mib: 512\n  wall_ms: 120000\nsteps:\n{steps}"
        ),
    )
    .expect("scenario");
    let apk = workspace().join("fixtures/build/apps/counter.apk");
    assert!(
        apk.is_file(),
        "build fixtures first: missing {}",
        apk.display()
    );
    let mut command = Command::cargo_bin("acos").expect("acos binary");
    command
        .env("ACOS_NO_DAEMON", "1")
        .env("ACOS_ART_BIN", &script)
        // ZDEFAULT1: this is a fake exec-path launcher; the zygote is the default.
        .env("ACOS_ART_ZYGOTE", "off")
        .env("FAKE_ART_OUT", &out)
        .env_remove("ACOS_ALLOW_UNTRUSTED_FOR_TESTS")
        .args([
            "verify",
            "--scenario",
            scenario.to_str().unwrap(),
            "--apk",
            apk.to_str().unwrap(),
            "--engine",
            "art",
            "--artifacts",
            artifacts.to_str().unwrap(),
            "--json",
        ]);
    if allow_untrusted {
        command.env("ACOS_ALLOW_UNTRUSTED_FOR_TESTS", "1");
    }
    Run {
        output: command.output().expect("run acos verify"),
        artifacts,
    }
}

fn verdict(run: &Run) -> Value {
    let text = std::fs::read_to_string(run.artifacts.join("verdict.json")).unwrap_or_else(|e| {
        panic!(
            "verdict.json must exist: {e}\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&run.output.stdout),
            String::from_utf8_lossy(&run.output.stderr)
        )
    });
    serde_json::from_str(&text).expect("verdict.json is JSON")
}

const LAUNCH: &str = "  - launch:\n      activity: dev.acos.fixtures.counter/.MainActivity\n";

fn step_code(verdict: &Value, index: usize) -> String {
    verdict["steps"][index]["error"]["code"]
        .as_str()
        .unwrap_or("")
        .to_string()
}

#[test]
fn untrusted_forged_forest_cannot_pass_a_tree_assertion() {
    let steps = format!(
        "{LAUNCH}  - assert_text:\n      selector: {{id: ghost}}\n      text: {{exact: \"FORGED FOREST\"}}\n"
    );
    let run = run_scenario("forest-untrusted", &steps, "trust: untrusted\n", true);
    assert!(
        !run.output.status.success(),
        "the forged forest must not pass"
    );
    let v = verdict(&run);
    assert_eq!(step_code(&v, 1), "INSUFFICIENT_HOST_EVIDENCE");
    assert_eq!(v["trust"], "untrusted");
    assert_eq!(
        v["evidence_provenance"]["tree"], "guest-asserted",
        "the forged forest is guest-asserted and must be labelled so"
    );
    assert_eq!(
        v["steps"][1]["provenance"]["tree"], "guest-asserted",
        "the failing step must carry the same label"
    );
}

#[test]
fn trusted_forged_forest_is_element_not_found_not_tree_truncated() {
    let steps = format!(
        "{LAUNCH}  - assert_text:\n      selector: {{id: ghost}}\n      text: {{exact: \"FORGED FOREST\"}}\n"
    );
    let run = run_scenario("forest-trusted", &steps, "", false);
    assert!(
        !run.output.status.success(),
        "the trusted control must fail"
    );
    let v = verdict(&run);
    assert_eq!(
        step_code(&v, 1),
        "ELEMENT_NOT_FOUND",
        "truncated:false must not become TREE_TRUNCATED"
    );
}

#[test]
fn untrusted_wait_for_refresh_also_fails_closed() {
    let steps =
        format!("{LAUNCH}  - wait_for:\n      selector: {{id: ghost}}\n      timeout_ms: 100\n");
    let run = run_scenario("wait-untrusted", &steps, "trust: untrusted\n", true);
    assert!(
        !run.output.status.success(),
        "wait_for must not pass on the forged tree"
    );
    let v = verdict(&run);
    assert_eq!(
        step_code(&v, 1),
        "INSUFFICIENT_HOST_EVIDENCE",
        "refresh() must go through the same fail-closed guard as observe()"
    );
}

#[test]
fn untrusted_assert_log_is_refused_not_matched() {
    let steps = format!(
        "{LAUNCH}  - assert_log:\n      patterns: [\"FORGED FOREST\"]\n      timeout_ms: 100\n"
    );
    let run = run_scenario("log-untrusted", &steps, "trust: untrusted\n", true);
    assert!(
        !run.output.status.success(),
        "a guest log must not justify a pass"
    );
    let v = verdict(&run);
    let code = step_code(&v, 1);
    assert!(
        matches!(
            code.as_str(),
            "UNSUPPORTED_BACKEND" | "INSUFFICIENT_HOST_EVIDENCE"
        ),
        "a guest log must fail closed, got {code}"
    );
}
