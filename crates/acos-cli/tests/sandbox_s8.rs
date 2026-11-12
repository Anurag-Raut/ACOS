//! S8: trust/provenance, fail-closed untrusted runs, region assertions and the
//! no-pixel-change input rule, over the real `acos verify --engine art` CLI and
//! a scripted ART session (no ART sysroot needed).
//!
//! The hostile live run (real APK + real ART) is env-gated by
//! `ACOS_S8_LIVE_HOSTILE=1`; see `results/art-runtime/S8.md` for the recorded
//! run. The tests here prove the CLI decisions with teeth on every run.

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
    let dir = std::env::temp_dir().join(format!("acos-s8-{}-{n}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

const SESSION_SCRIPT: &str = r##"#!/usr/bin/env python3
import base64, hashlib, json, os, sys

PNG = base64.b64decode(
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAIAAACQd1PeAAAADElEQVR4nGP4//8/AAX+Av4N70a4AAAAAElFTkSuQmCC"
)
out = os.environ["FAKE_ART_OUT"]
os.makedirs(out, exist_ok=True)
fault = os.environ.get("FAKE_ART_FAULT", "")
marker = os.environ.get("FAKE_ART_MARKER")
if marker:
    with open(marker, "w") as f:
        f.write("started")
state = {"count": 0, "seq": 0}

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
            {"class": "android.widget.TextView", "id": "count",
             "text": str(state["count"]), "bounds": "[42,189][63,240]", "visibility": 0},
            {"class": "android.widget.Button", "id": "inc",
             "text": "Increment", "bounds": "[42,368][273,494]",
             "clickable": True, "visibility": 0},
        ],
    }]}

def frame(cmd, with_tree):
    state["seq"] += 1
    path, md5 = png_for_frame()
    reply = {"ok": True, "cmd": cmd, "frame_seq": state["seq"], "png": path, "md5": md5}
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
    if cmd in ("tap", "key", "type_text"):
        if fault == "no_pixel_change_input":
            # The host injected the input; the guest accepted it but the pixels
            # did not change. The old session refused this as NO_FRESH_FRAME.
            print(json.dumps({
                "ok": False, "cmd": cmd, "code": "NO_FRESH_FRAME",
                "error": "test: completed no new settled observation; pixels did not change",
                "gesture": {"acks": [{"seq": 1, "handled": True, "latency_us": 10}], "token": "0x1"},
            }), flush=True)
            continue
        if fault == "no_delivery_no_frame":
            print(json.dumps({
                "ok": False, "cmd": cmd, "code": "NO_FRESH_FRAME",
                "error": "test: no delivered gesture and no frame",
            }), flush=True)
            continue
        if fault == "stale_tap":
            print(json.dumps({
                "ok": True, "cmd": cmd, "frame_seq": state["seq"],
                "png": os.path.join(out, "frame_%d.png" % state["seq"]),
                "md5": hashlib.md5(PNG).hexdigest(), "tree": tree(),
                "gesture": {"acks": [{"seq": 1, "handled": True}]},
            }), flush=True)
            continue
        if cmd == "tap":
            state["count"] += 1
        reply = frame(cmd, True)
        reply["gesture"] = {"acks": [{"seq": 1, "handled": True, "latency_us": 10}]}
    elif cmd == "tree":
        reply = frame("tree", True)
    elif cmd == "screenshot":
        reply = frame("screenshot", False)
    else:
        reply = {"ok": False, "cmd": cmd, "code": "BAD_CMD", "error": "unknown command"}
    print(json.dumps(reply), flush=True)
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
    marker: PathBuf,
}

fn run_scenario(name: &str, scenario_text: &str, fault: &str, allow_untrusted: bool) -> Run {
    let dir = scratch(name);
    let script = session_script(&dir);
    let artifacts = dir.join("artifacts");
    let out = dir.join("frames");
    std::fs::create_dir_all(&out).expect("frames");
    let marker = dir.join("session-started");
    let scenario = dir.join("scenario.yaml");
    std::fs::write(&scenario, scenario_text).expect("scenario");
    let apk = workspace().join("fixtures/build/apps/counter.apk");
    assert!(
        apk.is_file(),
        "build fixtures first: tools/build-apps.sh (missing {})",
        apk.display()
    );
    let mut command = Command::cargo_bin("acos").expect("acos binary");
    command
        .env("ACOS_NO_DAEMON", "1")
        .env("ACOS_ART_BIN", &script)
        .env("FAKE_ART_OUT", &out)
        .env("FAKE_ART_FAULT", fault)
        .env("FAKE_ART_MARKER", &marker)
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
    let output = command.output().expect("run acos verify");
    Run {
        output,
        artifacts,
        marker,
    }
}

fn load_verdict(run: &Run) -> Value {
    let text = std::fs::read_to_string(run.artifacts.join("verdict.json")).unwrap_or_else(|e| {
        panic!(
            "verdict.json must exist: {e}\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&run.output.stdout),
            String::from_utf8_lossy(&run.output.stderr)
        )
    });
    serde_json::from_str(&text).expect("verdict.json is JSON")
}

fn envelope(run: &Run) -> Value {
    serde_json::from_str(&String::from_utf8_lossy(&run.output.stdout)).expect("JSON envelope")
}

fn header(extra: &str) -> String {
    let mut header =
        format!("version: 1\nname: s8-test\n{extra}app: fixtures/build/apps/counter.apk\n");
    header.push_str("isolation: process\ndisplay:\n");
    header.push_str("  mode: offscreen\n  width: 1080\n  height: 2400\n  density_dpi: 420\n");
    header.push_str("budgets:\n  memory_mib: 512\n  wall_ms: 120000\n");
    header
}

fn launch() -> &'static str {
    "  - launch:\n      activity: dev.acos.fixtures.counter/.MainActivity\n"
}

// W51 changed these two tests. They used to assert the S8 policy (an untrusted
// run is refused with UNTRUSTED_APK_NOT_CERTIFIED unless the test override is
// set). The owner's 2026-11-12 decision makes untrusted an explicit opt-in that
// is allowed without any env var (docs/SANDBOX_CERTIFICATION.md §8.8), so they
// now assert the new policy: the run starts, is labelled, and only guest-asserted
// evidence still fails closed.
#[test]
fn untrusted_run_without_the_test_override_is_allowed_and_labelled() {
    let scenario = format!(
        "{}steps:\n{}  - assert_text:\n      selector: {{id: count}}\n      text: {{exact: \"0\"}}\n",
        header("trust: untrusted\n"),
        launch()
    );
    let run = run_scenario("untrusted-optin", &scenario, "", false);
    let envelope = envelope(&run);
    assert_ne!(
        envelope["error"]["code"], "UNTRUSTED_APK_NOT_CERTIFIED",
        "the product no longer refuses an explicit untrusted opt-in: {envelope}"
    );
    assert!(
        run.marker.exists(),
        "the ART session must start for an opt-in untrusted run"
    );
    let verdict = load_verdict(&run);
    assert_eq!(verdict["trust"], "untrusted");
    assert_eq!(verdict["certification"], "certified-with-limits");
    assert_eq!(verdict["certification_limits"]["doc"], "docs/SANDBOX_CERTIFICATION.md");
    assert_eq!(verdict["certification_limits"]["section"], "§8.7");
    // The tree stays guest-asserted: the run is allowed, the claim is not.
    assert_eq!(verdict["steps"][1]["error"]["code"], "INSUFFICIENT_HOST_EVIDENCE");
    assert_eq!(envelope["result"]["trust"], "untrusted");
    assert_eq!(envelope["result"]["certification"], "certified-with-limits");
}

#[test]
fn untrusted_flag_is_allowed_without_the_override() {
    let scenario = format!(
        "{}steps:\n{}  - assert_text:\n      selector: {{id: count}}\n      text: {{exact: \"0\"}}\n",
        header(""),
        launch()
    );
    let dir = scratch("untrusted-flag");
    let script = session_script(&dir);
    let artifacts = dir.join("artifacts");
    let out = dir.join("frames");
    std::fs::create_dir_all(&out).unwrap();
    let marker = dir.join("session-started");
    let scenario_path = dir.join("scenario.yaml");
    std::fs::write(&scenario_path, scenario).unwrap();
    let apk = workspace().join("fixtures/build/apps/counter.apk");
    let output = Command::cargo_bin("acos")
        .unwrap()
        .env("ACOS_NO_DAEMON", "1")
        .env("ACOS_ART_BIN", &script)
        .env("FAKE_ART_OUT", &out)
        .env("FAKE_ART_MARKER", &marker)
        .env_remove("ACOS_ALLOW_UNTRUSTED_FOR_TESTS")
        .args([
            "verify",
            "--scenario",
            scenario_path.to_str().unwrap(),
            "--apk",
            apk.to_str().unwrap(),
            "--engine",
            "art",
            "--untrusted",
            "--artifacts",
            artifacts.to_str().unwrap(),
            "--json",
        ])
        .output()
        .expect("run acos verify");
    let envelope: Value =
        serde_json::from_str(&String::from_utf8_lossy(&output.stdout)).expect("envelope");
    assert_ne!(
        envelope["error"]["code"], "UNTRUSTED_APK_NOT_CERTIFIED",
        "--untrusted is allowed without any env var: {envelope}"
    );
    assert!(marker.exists(), "the ART session must start");
    assert!(!output.status.success(), "a tree-only step still fails closed");
    assert_eq!(envelope["result"]["trust"], "untrusted");
    assert_eq!(envelope["result"]["certification"], "certified-with-limits");
    let verdict: Value = serde_json::from_str(
        &std::fs::read_to_string(artifacts.join("verdict.json")).expect("verdict"),
    )
    .expect("verdict json");
    assert_eq!(verdict["steps"][1]["error"]["code"], "INSUFFICIENT_HOST_EVIDENCE");
}

#[test]
fn untrusted_tree_assertion_fails_closed_but_tap_navigation_is_allowed() {
    let scenario = format!(
        "{}steps:\n{}  - tap:\n      selector: {{id: inc}}\n  - assert_text:\n      selector: {{id: count}}\n      text: {{exact: \"1\"}}\n",
        header("trust: untrusted\n"),
        launch()
    );
    let run = run_scenario("untrusted-tree", &scenario, "", true);
    assert!(
        !run.output.status.success(),
        "tree-only assertion must fail"
    );
    let verdict = load_verdict(&run);
    assert_eq!(verdict["trust"], "untrusted");
    assert_eq!(
        verdict["steps"][1]["status"], "pass",
        "tap navigation from the tree must still work: {:?}",
        verdict["steps"][1]
    );
    let failed = &verdict["steps"][2];
    assert_eq!(failed["step"], "assert_text");
    assert_eq!(failed["status"], "fail");
    assert_eq!(failed["error"]["code"], "INSUFFICIENT_HOST_EVIDENCE");
    // Provenance is recorded on every step, not inferred by the reader.
    assert_eq!(
        verdict["steps"][1]["provenance"]["host_input"],
        "host-observed"
    );
    assert_eq!(
        verdict["steps"][1]["provenance"]["input_acks"],
        "guest-asserted"
    );
    assert_eq!(verdict["steps"][2]["provenance"]["tree"], "guest-asserted");
    assert_eq!(verdict["evidence_provenance"]["frames"], "host-observed");
    assert_eq!(verdict["evidence_provenance"]["tree"], "guest-asserted");
}

#[test]
fn region_assertion_uses_host_pixels_and_can_pass_or_fail() {
    let pass = format!(
        "{}steps:\n{}  - assert_region:\n      x: 0\n      y: 0\n      w: 1\n      h: 1\n      min_distinct_colors: 1\n",
        header("trust: untrusted\n"),
        launch()
    );
    let run = run_scenario("region-pass", &pass, "", true);
    assert!(
        run.output.status.success(),
        "one-colour 1x1 region must pass: {}\n{}",
        String::from_utf8_lossy(&run.output.stdout),
        String::from_utf8_lossy(&run.output.stderr)
    );
    let verdict = load_verdict(&run);
    assert_eq!(verdict["steps"][1]["step"], "assert_region");
    assert_eq!(verdict["steps"][1]["status"], "pass");
    assert_eq!(verdict["steps"][1]["provenance"]["frame"], "host-observed");

    let fail = format!(
        "{}steps:\n{}  - assert_region:\n      x: 0\n      y: 0\n      w: 1\n      h: 1\n      min_distinct_colors: 2\n",
        header("trust: untrusted\n"),
        launch()
    );
    let run = run_scenario("region-fail", &fail, "", true);
    assert!(
        !run.output.status.success(),
        "two colours must fail on 1 pixel"
    );
    let verdict = load_verdict(&run);
    assert_eq!(verdict["steps"][1]["error"]["code"], "ASSERTION_FAILED");

    let ocr = format!(
        "{}steps:\n{}  - assert_region:\n      x: 0\n      y: 0\n      w: 1\n      h: 1\n      text_ocr: \"HOSTILE PASS\"\n",
        header("trust: untrusted\n"),
        launch()
    );
    let run = run_scenario("region-ocr", &ocr, "", true);
    assert!(!run.output.status.success());
    let verdict = load_verdict(&run);
    assert_eq!(verdict["steps"][1]["status"], "unsupported");
    assert_eq!(
        verdict["steps"][1]["error"]["code"],
        "UNSUPPORTED_ASSERTION"
    );
}

#[test]
fn delivered_input_with_no_pixel_change_is_not_stale() {
    let scenario = format!(
        "{}steps:\n{}  - tap:\n      selector: {{id: inc}}\n",
        header(""),
        launch()
    );
    let run = run_scenario("no-pixel-change", &scenario, "no_pixel_change_input", false);
    assert!(
        run.output.status.success(),
        "a delivered no-op tap must not be stale: {}\n{}",
        String::from_utf8_lossy(&run.output.stdout),
        String::from_utf8_lossy(&run.output.stderr)
    );
    let verdict = load_verdict(&run);
    let tap = &verdict["steps"][1];
    assert_eq!(tap["status"], "pass");
    assert_eq!(tap["assertions"][0]["kind"], "input_delivery");
    assert!(
        tap["assertions"][0]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("no visual change"),
        "the step must say no visual change was claimed: {tap:?}"
    );
}

#[test]
fn a_missing_frame_without_delivery_evidence_is_still_no_fresh_frame() {
    let scenario = format!(
        "{}steps:\n{}  - tap:\n      selector: {{id: inc}}\n",
        header(""),
        launch()
    );
    let run = run_scenario("no-delivery", &scenario, "no_delivery_no_frame", false);
    assert!(!run.output.status.success());
    let verdict = load_verdict(&run);
    assert_eq!(verdict["steps"][1]["error"]["code"], "NO_FRESH_FRAME");
}

#[test]
fn hostile_live_run_is_gated_and_recorded_separately() {
    // The real-ART hostile run needs the sysroot and an acos-art binary. It is
    // opt-in so `cargo test -p acos-cli` stays host-light; the recorded command
    // and result are in results/art-runtime/S8.md.
    if std::env::var("ACOS_S8_LIVE_HOSTILE").ok().as_deref() != Some("1") {
        eprintln!("skipping live hostile run (set ACOS_S8_LIVE_HOSTILE=1)");
        return;
    }
    let apk = workspace().join("fixtures/build/apps/hostile-probe.apk");
    assert!(
        apk.is_file(),
        "build the hostile fixture first: fixtures/build-hostile-probe.sh"
    );
    let artifacts = scratch("hostile-live").join("artifacts");
    let output = Command::cargo_bin("acos")
        .unwrap()
        .env("ACOS_ALLOW_UNTRUSTED_FOR_TESTS", "1")
        .args([
            "verify",
            "--scenario",
            workspace()
                .join("scenarios/s8-hostile-untrusted.yaml")
                .to_str()
                .unwrap(),
            "--apk",
            apk.to_str().unwrap(),
            "--engine",
            "art",
            "--artifacts",
            artifacts.to_str().unwrap(),
            "--json",
        ])
        .output()
        .expect("run the live hostile verify");
    assert!(!output.status.success(), "the forged tree must not pass");
    let verdict: Value = serde_json::from_str(
        &std::fs::read_to_string(artifacts.join("verdict.json")).expect("verdict"),
    )
    .expect("json");
    assert_eq!(verdict["trust"], "untrusted");
    let failed = verdict["steps"]
        .as_array()
        .unwrap()
        .iter()
        .find(|step| step["status"] != "pass")
        .expect("a failed step");
    assert_eq!(failed["error"]["code"], "INSUFFICIENT_HOST_EVIDENCE");
}
