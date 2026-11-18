//! W51: product policy for untrusted APKs — opt-in, allowed, labelled.
//!
//! Owner decision 2026-11-12, recorded in `docs/SANDBOX_CERTIFICATION.md` §8.8:
//! the sandbox is CERTIFIED-WITH-LIMITS (§8.6, limits §8.7), so an explicit
//! `acos verify --untrusted` or a scenario `trust: untrusted` is allowed with no
//! environment variable. Trusted stays the default (no auto-detection), the
//! historical `ACOS_ALLOW_UNTRUSTED_FOR_TESTS=1` alias stays accepted and
//! redundant, and the fail-closed rules for guest-asserted evidence do not move.
//!
//! Every test runs the real CLI over a scripted ART session (no ART sysroot).
//! `ACOS_ALLOW_UNTRUSTED_FOR_TESTS` is removed from the child environment unless
//! a test sets it on purpose, so "without the env var" is literal.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use assert_cmd::Command;
use serde_json::Value;

fn workspace() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Scratch under `.cache/` (never `/tmp`) and removed when the test ends.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = workspace()
            .join(".cache")
            .join("w51-tests")
            .join(format!("{}-{n}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        Scratch(dir)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A scripted ART session: one window, one node, a 1x1 PNG frame. The tree is
/// guest-asserted on an untrusted run; the frame is host pixels.
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
        "windows": [{"window": "main", "z": 0}],
        "window_count": 1, "node_budget": 2000, "truncated": False,
    }

def frame(cmd, with_tree):
    state["seq"] += 1
    path = os.path.join(out, "frame_%d.png" % state["seq"])
    data = PNG + ("#seq=%d" % state["seq"]).encode()
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
        print(json.dumps(frame(cmd, True)), flush=True)
    elif cmd == "screenshot":
        print(json.dumps(frame(cmd, False)), flush=True)
    elif cmd == "logs":
        print(json.dumps({"ok": True, "cmd": "logs", "entries": [],
                          "total": 0, "dropped": 0}), flush=True)
    else:
        print(json.dumps({"ok": False, "cmd": cmd, "code": "BAD_CMD",
                          "error": "unknown command"}), flush=True)
"##;

const LAUNCH: &str = "  - launch:\n      activity: dev.acos.fixtures.counter/.MainActivity\n";
const TREE_ASSERT: &str =
    "  - assert_text:\n      selector: {id: main-only}\n      text: {exact: \"FORGED FOREST\"}\n";
const REGION_ASSERT: &str =
    "  - assert_region:\n      x: 0\n      y: 0\n      w: 1\n      h: 1\n      min_distinct_colors: 1\n";

struct Run {
    output: std::process::Output,
    artifacts: PathBuf,
}

/// Run the CLI. `trust_header` is the scenario's top-level trust key (`""`,
/// `"trust: trusted\n"` or `"trust: untrusted\n"`); `alias` sets the redundant
/// test alias only when asked.
fn run_verify(name: &str, trust_header: &str, steps: &str, alias: bool) -> (Scratch, Run) {
    let scratch = Scratch::new(name);
    let dir = scratch.path();
    use std::os::unix::fs::PermissionsExt;
    let script = dir.join("fake_art_session.py");
    std::fs::write(&script, SESSION_SCRIPT).expect("session script");
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).expect("executable");
    let artifacts = dir.join("artifacts");
    let frames = dir.join("frames");
    std::fs::create_dir_all(&frames).expect("frames");
    let scenario = dir.join("scenario.yaml");
    std::fs::write(
        &scenario,
        format!(
            "version: 1\nname: w51-{name}\n{trust_header}app: fixtures/build/apps/counter.apk\n\
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
        .env("FAKE_ART_OUT", &frames)
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
    if alias {
        command.env("ACOS_ALLOW_UNTRUSTED_FOR_TESTS", "1");
    }
    let output = command.output().expect("run acos verify");
    (scratch, Run { output, artifacts })
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

fn envelope(run: &Run) -> Value {
    serde_json::from_str(&String::from_utf8_lossy(&run.output.stdout)).unwrap_or_else(|e| {
        panic!(
            "stdout must be a JSON envelope: {e}\nstdout: {}",
            String::from_utf8_lossy(&run.output.stdout)
        )
    })
}

/// Assert the labels the product policy requires on a verdict and its envelope.
fn assert_certified_with_limits(value: &Value, trust: &str) {
    assert_eq!(value["trust"], trust);
    assert_eq!(value["certification"], "certified-with-limits");
    assert_eq!(
        value["certification_limits"]["doc"], "docs/SANDBOX_CERTIFICATION.md",
        "the limits pointer must name the doc: {value}"
    );
    assert_eq!(
        value["certification_limits"]["section"], "§8.7",
        "the limits pointer must name the section: {value}"
    );
}

#[test]
fn untrusted_without_the_env_var_runs_and_labels_the_verdict() {
    // A host-observed assertion (host pixels) is the only evidence that may
    // justify a pass on an untrusted run; this one must pass with no env var.
    let steps = format!("{LAUNCH}{REGION_ASSERT}");
    let (_scratch, run) = run_verify("untrusted-region", "trust: untrusted\n", &steps, false);
    assert!(
        run.output.status.success(),
        "an explicit untrusted opt-in must run and a host-observed assertion may pass: {}\n{}",
        String::from_utf8_lossy(&run.output.stdout),
        String::from_utf8_lossy(&run.output.stderr)
    );
    let v = verdict(&run);
    assert_eq!(v["verdict"], "pass");
    assert_certified_with_limits(&v, "untrusted");
    assert_eq!(
        v["certification_policy"]["section"], "§8.8",
        "the verdict must point at the product policy that allows the opt-in"
    );
    assert_eq!(v["steps"][1]["status"], "pass");
    assert_eq!(v["steps"][1]["provenance"]["frame"], "host-observed");
    let e = envelope(&run);
    assert_eq!(e["ok"], true);
    assert_certified_with_limits(&e["result"], "untrusted");
}

#[test]
fn untrusted_tree_only_assertion_still_fails_closed() {
    // Same run shape, but the claim is the guest-asserted tree. It must fail
    // closed with INSUFFICIENT_HOST_EVIDENCE — not because the run is refused,
    // because the evidence class is insufficient.
    let steps = format!("{LAUNCH}{TREE_ASSERT}");
    let (_scratch, run) = run_verify("untrusted-tree", "trust: untrusted\n", &steps, false);
    assert!(
        !run.output.status.success(),
        "a tree-only assertion must never pass on an untrusted run"
    );
    let v = verdict(&run);
    assert_certified_with_limits(&v, "untrusted");
    assert_eq!(v["steps"][1]["status"], "fail");
    assert_eq!(
        v["steps"][1]["error"]["code"], "INSUFFICIENT_HOST_EVIDENCE",
        "tree-only evidence fails closed: {:?}",
        v["steps"][1]
    );
    assert_eq!(v["evidence_provenance"]["tree"], "guest-asserted");
    // The run was allowed: the failure is the assertion, not a refusal.
    let e = envelope(&run);
    assert_ne!(e["error"]["code"], "UNTRUSTED_APK_NOT_CERTIFIED");
    assert_certified_with_limits(&e["result"], "untrusted");
}

#[test]
fn trusted_is_still_the_default_and_tree_evidence_still_counts() {
    // No trust key: trusted by default, no auto-detection from the APK.
    let steps = format!("{LAUNCH}{TREE_ASSERT}");
    let (_scratch, run) = run_verify("trusted-default", "", &steps, false);
    assert!(
        run.output.status.success(),
        "a trusted fixture may corroborate a pass from its own tree: {}\n{}",
        String::from_utf8_lossy(&run.output.stdout),
        String::from_utf8_lossy(&run.output.stderr)
    );
    let v = verdict(&run);
    assert_eq!(v["verdict"], "pass");
    assert_certified_with_limits(&v, "trusted");
    assert_eq!(v["steps"][1]["status"], "pass");

    // An explicit `trust: trusted` is the same run, labelled the same way.
    let (_scratch, explicit) = run_verify("trusted-explicit", "trust: trusted\n", &steps, false);
    assert!(explicit.output.status.success());
    assert_certified_with_limits(&verdict(&explicit), "trusted");
}

#[test]
fn the_test_override_alias_is_accepted_and_redundant() {
    // Existing hostile tests set ACOS_ALLOW_UNTRUSTED_FOR_TESTS=1. It must keep
    // working, and it must not change the verdict or the labels.
    let steps = format!("{LAUNCH}{REGION_ASSERT}");
    let (_scratch, run) = run_verify("alias", "trust: untrusted\n", &steps, true);
    assert!(
        run.output.status.success(),
        "the redundant test alias must still be accepted: {}\n{}",
        String::from_utf8_lossy(&run.output.stdout),
        String::from_utf8_lossy(&run.output.stderr)
    );
    let v = verdict(&run);
    assert_certified_with_limits(&v, "untrusted");
    assert_eq!(v["verdict"], "pass");
}

#[test]
fn an_unclassified_scenario_is_labelled_unresolved() {
    // The scenario's `trust:` value is invalid, so the run was never classified.
    // The envelope still carries the certification record and says so.
    let steps = format!("{LAUNCH}{REGION_ASSERT}");
    let (_scratch, run) = run_verify("invalid-trust", "trust: maybe\n", &steps, false);
    assert!(!run.output.status.success());
    let e = envelope(&run);
    assert_eq!(e["error"]["code"], "INVALID_SCENARIO");
    assert_eq!(e["result"]["trust"], "unresolved");
    assert_eq!(e["result"]["certification"], "certified-with-limits");
    assert_eq!(
        e["result"]["certification_limits"]["doc"],
        "docs/SANDBOX_CERTIFICATION.md"
    );
}
