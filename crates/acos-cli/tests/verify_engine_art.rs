//! `acos verify --engine art` end to end over a real child process.
//!
//! No ART sysroot is needed on this host: `$ACOS_ART_BIN` points at a small
//! Python script that speaks exactly the `acos-art session` line-JSON protocol
//! (one ready line, then one reply per command, each reply carrying
//! `frame_seq`/`png`/`md5`/`tree`). That keeps the *CLI* path real — spawn,
//! relay, fresh-capture evidence, action freshness, teardown — while the app is
//! a scripted counter.
//!
//! The tests assert the properties that must hold before the live frame lands:
//!
//! * `scenarios/v2-art-counter.yaml` passes with `engine: art`, `mode: live`,
//!   and every step's frame sequence strictly increasing where the scenario
//!   advances the frame;
//! * a tap that completes no new frame is refused (`STALE_FRAME`, never a pass);
//! * a session that never produces a live frame fails honestly
//!   (`LIVE_FRAME_UNAVAILABLE`, never a pass, never preview);
//! * a helper process the guest forked does not survive the session (process
//!   tree teardown).

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
    let dir = std::env::temp_dir().join(format!(
        "acos-cli-engine-{}-{n}-{name}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

/// The scripted session. It is deliberately a separate process group member of
/// the CLI's child, and it forks a `sleep` helper it never reaps, so the engine's
/// teardown has a real process tree to clean.
const SESSION_SCRIPT: &str = r##"#!/usr/bin/env python3
import base64, hashlib, json, os, subprocess, sys

PNG = base64.b64decode(
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAIAAACQd1PeAAAADElEQVR4nGP4//8/AAX+Av4N70a4AAAAAElFTkSuQmCC"
)
out = os.environ["FAKE_ART_OUT"]
os.makedirs(out, exist_ok=True)
fault = os.environ.get("FAKE_ART_FAULT", "")
state = {"count": 0, "seq": 0}

class Dead(Exception):
    pass

def png_for_frame():
    path = os.path.join(out, "frame_%d.png" % state["seq"])
    data = PNG + ("#count=%d;seq=%d" % (state["count"], state["seq"])).encode()
    with open(path, "wb") as f:
        f.write(data)
    return path, hashlib.md5(data).hexdigest()

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
            {"class": "android.widget.Button",
             "id": "dev.acos.fixtures.counter:id/reset",
             "text": "Reset", "bounds": "[42,494][273,620]",
             "clickable": True, "visibility": 0},
        ],
    }]}

def frame(cmd, with_tree):
    # A real session publishes a new frame for every command, and only once the
    # pixels are complete. The stale fault keeps `seq` and the pixels unchanged.
    if not (fault == "stale_tap" and cmd == "tap"):
        state["seq"] += 1
        path, md5 = png_for_frame()
    else:
        path = os.path.join(out, "frame_%d.png" % state["seq"])
        data = PNG + ("#count=%d;seq=%d" % (state["count"], state["seq"])).encode()
        md5 = hashlib.md5(data).hexdigest()
    reply = {"ok": True, "cmd": cmd, "frame_seq": state["seq"], "png": path, "md5": md5}
    if with_tree:
        reply["tree"] = tree()
    return reply

# The ready line is the first reply and already carries a completed frame.
if fault == "no_live_frame":
    # A valid ready line, but this session never completes a frame.
    print(json.dumps({"ok": True, "cmd": "ready", "package": "dev.acos.fixtures.counter",
                      "activity": "dev.acos.fixtures.counter.MainActivity"}), flush=True)
else:
    print(json.dumps(frame("ready", True)), flush=True)

# A guest that forked a helper: teardown must clean the whole group.
helper = subprocess.Popen(["sleep", "300"], stdout=subprocess.DEVNULL,
                          stderr=subprocess.DEVNULL)
with open(os.environ["FAKE_ART_PIDFILE"], "w") as f:
    f.write(str(helper.pid))

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
        print(json.dumps({"ok": True, "cmd": "close", "frame_seq": state["seq"]}),
              flush=True)
        break
    if cmd == "tap":
        if fault != "stale_tap":
            state["count"] += 1
        reply = frame("tap", True)
        reply["tap"] = {"handled": fault != "stale_tap"}
    elif cmd == "reset":
        state["count"] = 0
        reply = frame("reset", True)
    elif cmd == "tree" and fault == "no_live_frame":
        # No frame_seq, no png, no tree: the engine must refuse to invent one.
        reply = {"ok": True, "cmd": "tree"}
    elif cmd == "tree":
        reply = frame("tree", True)
    elif cmd == "screenshot":
        reply = frame("screenshot", False)
    elif cmd == "key":
        reply = frame("key", True)
        reply["key"] = {"handled": True, "code": request.get("code", 0)}
    else:
        reply = {"ok": False, "cmd": cmd, "code": "BAD_CMD", "error": "unknown command"}
    print(json.dumps(reply), flush=True)
"##;

fn session_script(dir: &Path) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join("fake_art_session.py");
    std::fs::write(&path, SESSION_SCRIPT).expect("write session script");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
        .expect("make the script executable");
    path
}

struct Run {
    output: std::process::Output,
    artifacts: PathBuf,
    helper_pid: Option<u32>,
}

fn run_verify(name: &str, fault: &str) -> Run {
    let dir = scratch(name);
    let script = session_script(&dir);
    let artifacts = dir.join("artifacts");
    let out = dir.join("frames");
    std::fs::create_dir_all(&out).expect("frames dir");
    let pidfile = dir.join("helper.pid");
    let workspace = workspace();
    let apk = workspace.join("fixtures/build/apps/counter.apk");
    assert!(
        apk.is_file(),
        "build fixtures first: `tools/build-apps.sh` (missing {})",
        apk.display()
    );
    let scenario = workspace.join("scenarios/v2-art-counter.yaml");
    let output = Command::cargo_bin("acos")
        .expect("acos binary")
        .env("ACOS_NO_DAEMON", "1")
        .env("ACOS_ART_BIN", &script)
        .env("FAKE_ART_OUT", &out)
        .env("FAKE_ART_PIDFILE", &pidfile)
        .env("FAKE_ART_FAULT", fault)
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
        ])
        .output()
        .expect("run acos verify");
    let helper_pid = std::fs::read_to_string(&pidfile)
        .ok()
        .and_then(|text| text.trim().parse::<u32>().ok());
    Run {
        output,
        artifacts,
        helper_pid,
    }
}

fn verdict(run: &Run) -> Value {
    let text = std::fs::read_to_string(run.artifacts.join("verdict.json"))
        .unwrap_or_else(|error| {
            panic!(
                "verdict.json must exist: {error}\nstdout: {}\nstderr: {}",
                String::from_utf8_lossy(&run.output.stdout),
                String::from_utf8_lossy(&run.output.stderr)
            )
        });
    serde_json::from_str(&text).expect("verdict.json is JSON")
}

fn step(verdict: &Value, index: usize) -> &Value {
    &verdict["steps"].as_array().expect("steps")[index]
}

fn wait_gone(pid: u32) -> bool {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        if !Path::new(&format!("/proc/{pid}")).exists() {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    false
}

#[test]
fn art_counter_scenario_passes_on_a_live_session_and_tears_the_tree_down() {
    let run = run_verify("pass", "");
    let stdout = String::from_utf8_lossy(&run.output.stdout);
    assert!(
        run.output.status.success(),
        "expected a passing verify\nstdout: {stdout}\nstderr: {}",
        String::from_utf8_lossy(&run.output.stderr)
    );
    let envelope: Value = serde_json::from_str(&stdout).expect("stdout is the JSON envelope");
    assert_eq!(envelope["status"], "pass");
    let verdict = verdict(&run);
    assert_eq!(verdict["verdict"], "pass");
    assert_eq!(verdict["engine"], "art");
    assert_eq!(verdict["mode"], "live");
    assert_eq!(verdict["unsupported_steps"].as_array().unwrap().len(), 0);

    // The decisive transition ran twice and every step is bound to a frame.
    let steps = verdict["steps"].as_array().unwrap();
    let taps: Vec<&Value> = steps
        .iter()
        .filter(|step| step["step"] == "tap")
        .collect();
    assert_eq!(taps.len(), 2, "tap Increment twice (before and after reset)");

    // Freshness: the capture step after the first tap must describe a newer
    // frame than the tap, and the second tap a newer frame than the reset.
    let capture_index = steps
        .iter()
        .position(|step| step["step"] == "capture")
        .expect("capture step");
    let captured = &steps[capture_index];
    let after_first_tap = &steps[capture_index - 2];
    let after_first_tap = if after_first_tap["step"] == "tap" {
        after_first_tap
    } else {
        &steps[capture_index - 1]
    };
    assert!(
        captured["frame_seq"].as_u64().unwrap() > after_first_tap["frame_seq"].as_u64().unwrap(),
        "a fresh capture must advance past the tap: capture {:?}, tap {:?}",
        captured["frame_seq"],
        after_first_tap["frame_seq"]
    );
    let reset_index = steps
        .iter()
        .position(|step| step["step"] == "reset")
        .expect("reset step");
    let second_tap = steps[reset_index..]
        .iter()
        .find(|step| step["step"] == "tap")
        .expect("tap after reset");
    assert!(
        second_tap["frame_seq"].as_u64().unwrap() > captured["frame_seq"].as_u64().unwrap(),
        "the second tap must complete a newer frame than the capture"
    );
    assert_eq!(step(&verdict, 0)["step"], "launch");

    // Teardown guarantee: the helper the session forked is gone.
    let pid = run.helper_pid.expect("the session reported its helper pid");
    assert!(
        wait_gone(pid),
        "session helper {pid} survived teardown\nstdout: {stdout}"
    );
}

#[test]
fn a_tap_that_completes_no_new_frame_is_refused() {
    let run = run_verify("stale-tap", "stale_tap");
    assert!(!run.output.status.success(), "a stale tap must not pass");
    let verdict = verdict(&run);
    assert_ne!(verdict["verdict"], "pass");
    let failed = verdict["steps"]
        .as_array()
        .unwrap()
        .iter()
        .find(|step| step["status"] != "pass")
        .expect("a failing step");
    assert_eq!(failed["step"], "tap");
    assert_eq!(failed["error"]["code"], "NO_FRESH_FRAME");
    let detail = failed["error"]["message"].as_str().unwrap_or_default();
    assert!(
        detail.contains("STALE_FRAME"),
        "the error must name the stale frame: {detail}"
    );
    let pid = run.helper_pid.expect("helper pid");
    assert!(wait_gone(pid), "teardown must still clean the tree on failure");
}

#[test]
fn a_session_without_a_live_frame_fails_honestly() {
    let run = run_verify("no-live-frame", "no_live_frame");
    assert!(!run.output.status.success(), "no live frame is not a pass");
    let envelope: Value =
        serde_json::from_str(&String::from_utf8_lossy(&run.output.stdout)).expect("envelope");
    assert_ne!(envelope["status"], "pass");
    let verdict = verdict(&run);
    assert_ne!(verdict["verdict"], "pass");
    assert_ne!(verdict["mode"], "preview");
    let failure = verdict["failure"].clone();
    assert_eq!(failure["code"], "NO_FRESH_FRAME");
    let message = failure["error"]["message"].as_str().unwrap_or_default();
    assert!(
        message.contains("LIVE_FRAME_UNAVAILABLE"),
        "the failure must name the missing live frame: {message}"
    );
    let pid = run.helper_pid.expect("helper pid");
    assert!(wait_gone(pid), "teardown must still clean the tree on failure");
}
