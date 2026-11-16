//! TOUCHDISP: the touch reaches the **real view chain**, and an unhandled
//! gesture whose side effect is posted must not settle on the pre-state.
//!
//! REL3H2 finding 1 said the unhandled-input settle shortcut could bind a probe
//! sample taken after the publish but before the handler ran (or before its
//! posted side effect ran and drew), so a gesture that changed the screen was
//! reported as "nothing happened" and the next assertion described a state the
//! app had already left. It could not be tested end to end because the fixture
//! it built appeared to prove a second bug: *"the ART guest's dispatch path acks
//! unhandled without consulting a non-clickable view's `OnTouchListener`"*.
//!
//! That second claim is wrong, and this test is the fixture that shows it (see
//! `results/handoff/TOUCHDISP/REPORT.md` and the `touch-probe` fixture):
//!
//! * AOSP's `ViewGroup.dispatchTouchEvent` hit-tests children on the DOWN and
//!   `View.dispatchTouchEvent` calls `mOnTouchListener.onTouch` for **any**
//!   ENABLED view, clickable or not. The guest does exactly that: the trace in
//!   `results/handoff/TOUCHDISP/dispatch-probe-logcat.txt` shows
//!   `Activity.dispatchTouchEvent DOWN` → `probe.dispatchTouchEvent DOWN` →
//!   `probe.onTouch DOWN` → `probe.onTouchEvent DOWN` → `root.onTouch DOWN`.
//! * A DOWN nobody consumed never becomes a touch target, so the framework does
//!   not offer that view the UP (real AOSP: "There are no touch targets and this
//!   action is not an initial down so this view group continues to intercept
//!   touches"). A listener that waits for `ACTION_UP` therefore never runs its
//!   side effect — which is what "the listener never ran" actually measured.
//!   The fixture posts from the DOWN, the only shape an app can write.
//!
//! What must hold, on a real ART guest:
//!
//! 1. `tap` on the non-clickable `probe`: the window acks `handled: false` (so
//!    the unhandled-input rule is in play), and 100 ms later the listener's
//!    posted `setText` draws. The step must **not** settle on the pre-state: the
//!    settled observation has the posted text, the settle reports the change,
//!    and `first_change_ms > ack_ms` proves the change came after the ack.
//! 2. The negative control (`rel3h-unhandled-input-expect-change.yaml`) still
//!    fails honestly: an unhandled tap that really changes nothing is never a
//!    pass for `expect_change: true`.
//! 3. `swipe` (a held long press) on the non-clickable `gesture` view running a
//!    `GestureDetector` reaches `View.onTouchEvent` and changes the state.
//!
//! **Ignored by default** because it boots a real ART guest. Run it under the
//! shared lock and the host guard:
//!
//! ```text
//! ACOS_APP_ONLY=touch-probe tools/build-apps.sh
//! tools/art-runtime/run-exclusive.sh --task TOUCHDISP -- \
//!   tools/guarded.sh --mem 8G --time 900 -- \
//!   cargo test -p acos-cli --release --test touchdisp_dispatch -- --ignored --test-threads=1
//! ```

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

mod common;

use common::ShortSocketDir;

fn repo_root() -> PathBuf {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    std::fs::canonicalize(&dir).unwrap_or_else(|_| dir.to_path_buf())
}

/// The release `acos-art` launcher to run: `ACOS_ART_BIN` when it points at a
/// file, else this branch's own target dir.
fn art_bin() -> PathBuf {
    if let Some(path) = std::env::var_os("ACOS_ART_BIN") {
        let path = PathBuf::from(path);
        if path.is_file() {
            return path;
        }
    }
    repo_root().join(".cache/touchdisp-target/release/acos-art")
}

fn scratch(tag: &str) -> PathBuf {
    let dir = repo_root()
        .join(".cache")
        .join(format!("touchdisp-test-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("runtime")).unwrap();
    std::fs::create_dir_all(dir.join("cache")).unwrap();
    std::fs::canonicalize(&dir).unwrap_or(dir)
}

/// One `acos verify <scenario> --engine art` run and its artifacts.
struct Run {
    dir: PathBuf,
    envelope: Value,
    _sockets: ShortSocketDir,
}

impl Run {
    fn scenario(name: &str, tag: &str) -> Run {
        let scenario = repo_root().join("scenarios").join(name);
        let art = art_bin();
        let apk = repo_root().join("fixtures/build/apps/touch-probe.apk");
        let needs_apk = name.starts_with("touchdisp-");
        if !scenario.is_file() || (needs_apk && !apk.is_file()) || !art.is_file() {
            eprintln!(
                "SKIP: {} / {} / {} missing (build with \
                 ACOS_APP_ONLY=touch-probe tools/build-apps.sh and \
                 cargo build --release -p acos-art)",
                scenario.display(),
                apk.display(),
                art.display()
            );
            std::process::exit(0);
        }
        let scratch = scratch(tag);
        let sockets = ShortSocketDir::new(tag);
        let artifacts = scratch.join("artifacts");
        let out = Command::new(env!("CARGO_BIN_EXE_acos"))
            // Scenario `app:` paths are relative to the checkout root.
            .current_dir(repo_root())
            .arg("verify")
            .arg(&scenario)
            .arg("--engine")
            .arg("art")
            .arg("--artifacts")
            .arg(&artifacts)
            .arg("--json")
            .env("ACOS_RUNTIME_DIR", scratch.join("runtime"))
            .env("ACOS_CACHE_DIR", scratch.join("cache"))
            .env("ACOS_SESSION_SOCKET", sockets.dir().join("session.sock"))
            .env("ACOS_ART_BIN", &art)
            .output()
            .expect("run acos verify");
        let stdout = String::from_utf8_lossy(&out.stdout).to_string();
        // `--json` prints the envelope as pretty JSON (and never a text banner).
        let envelope = serde_json::from_str::<Value>(stdout.trim()).ok()
            .or_else(|| {
                stdout
                    .lines()
                    .find(|line| line.trim_start().starts_with('{'))
                    .and_then(|line| serde_json::from_str::<Value>(line).ok())
            })
            .unwrap_or_else(|| {
                panic!(
                    "no envelope on stdout for {name} (status {:?})\nstdout:\n{stdout}\nstderr:\n{}",
                    out.status.code(),
                    String::from_utf8_lossy(&out.stderr)
                )
            });
        Run {
            dir: artifacts,
            envelope,
            _sockets: sockets,
        }
    }

    fn step(&self, index: usize, kind: &str) -> Value {
        let path = self.dir.join("steps").join(format!("{index:02}-{kind}.json"));
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        serde_json::from_str(&text).unwrap_or_else(|e| panic!("parse {}: {e}", path.display()))
    }

    /// The `text` of the node with `resource` in the tree a step bound.
    fn bound_text(&self, step: &Value, resource: &str) -> Option<String> {
        let seq = step.get("frame_seq")?.as_u64()?;
        let path = self.dir.join("tree").join(format!("{seq}.json"));
        // A step may bind a different tree file than its frame_seq names; fall
        // back to the `evidence` entry, which is what the reply pointed at.
        let path = if path.is_file() {
            path
        } else {
            let rel = step
                .get("evidence")?
                .as_array()?
                .iter()
                .find(|e| e.get("kind").and_then(Value::as_str) == Some("tree"))?
                .get("path")?
                .as_str()?;
            self.dir.join(rel)
        };
        let tree: Value = serde_json::from_str(&std::fs::read_to_string(&path).ok()?).ok()?;
        fn walk(node: &Value, resource: &str, out: &mut Option<String>) {
            match node {
                Value::Object(map) => {
                    let rid = map.get("resource_id").and_then(Value::as_str).unwrap_or("");
                    if rid == resource || rid.ends_with(&format!(":id/{resource}")) {
                        *out = map.get("text").and_then(Value::as_str).map(str::to_string);
                    }
                    for key in ["nodes", "children"] {
                        if let Some(child) = map.get(key) {
                            walk(child, resource, out);
                        }
                    }
                }
                Value::Array(items) => {
                    for item in items {
                        walk(item, resource, out);
                    }
                }
                _ => {}
            }
        }
        let mut out = None;
        walk(&tree, resource, &mut out);
        out
    }
}

#[test]
#[ignore = "boots a real ART guest; run under tools/guarded.sh + run-exclusive.sh"]
fn a_posted_side_effect_of_an_unhandled_tap_is_captured_not_the_pre_state() {
    let run = Run::scenario("touchdisp-posted-side-effect.yaml", "posted");
    assert_eq!(run.envelope["status"], serde_json::json!("pass"), "{:#?}", run.envelope);

    let step = run.step(1, "tap");
    assert_eq!(step["status"], serde_json::json!("pass"), "{step:#?}");

    // The premise: the gesture really was one the app did not handle, so the
    // unhandled-input rule is what could have settled it.
    let input = &step["input"];
    assert_eq!(input["handled"], serde_json::json!(false), "{step:#?}");
    let evidence = &input["handled_evidence"];
    assert_eq!(evidence["acked"], evidence["published"], "{step:#?}");
    assert!(
        evidence["unhandled"].as_array().is_some_and(|a| a.len() == 2),
        "both the DOWN and the UP must be acked unhandled: {step:#?}"
    );

    // The rule: the settle must not be the pre-state bind. A change arrived, it
    // arrived after the acks, and the bound observation carries the posted text.
    let settle = &step["settle"];
    assert_eq!(settle["settled"], serde_json::json!(true), "{step:#?}");
    assert_eq!(settle["mode"], serde_json::json!("strict"), "{step:#?}");
    assert_ne!(
        settle["bound"],
        serde_json::json!("unhandled-input"),
        "the posted side effect drew inside the settle, so the shortcut must not \
         bind: {step:#?}"
    );
    assert_eq!(settle["pixels_changed"], serde_json::json!(true), "{step:#?}");
    let ack_ms = settle["ack_ms"].as_u64().expect("ack_ms");
    let change_ms = settle["first_change_ms"]
        .as_u64()
        .unwrap_or_else(|| panic!("first_change_ms missing: {step:#?}"));
    assert!(
        change_ms > ack_ms,
        "the change must be observed after the gesture's own acks \
         (first_change_ms {change_ms} <= ack_ms {ack_ms}), otherwise the bound \
         sample is a pre-ack/pre-handler one: {step:#?}"
    );
    assert_eq!(
        run.bound_text(&step, "status").as_deref(),
        Some("touched-delayed"),
        "the settled tree must be the app's posted state, not the pre-state: {step:#?}"
    );

    // And the scenario's own assertions: the status text and the clickable
    // control both moved.
    let txt = run.step(2, "assert_text");
    assert_eq!(txt["status"], serde_json::json!("pass"), "{txt:#?}");
    let inc = run.step(3, "tap");
    assert_eq!(inc["status"], serde_json::json!("pass"), "{inc:#?}");
    assert_eq!(inc["input"]["handled"], serde_json::json!(true), "{inc:#?}");
    let count = run.step(4, "assert_text");
    assert_eq!(count["status"], serde_json::json!("pass"), "{count:#?}");
}

#[test]
#[ignore = "boots a real ART guest; run under tools/guarded.sh + run-exclusive.sh"]
fn an_unhandled_tap_that_changes_nothing_still_fails_expect_change() {
    // The negative control for the test above: the settle rule must never turn a
    // gesture that changed nothing into a pass just because it was unhandled.
    let run = Run::scenario("rel3h-unhandled-input-expect-change.yaml", "control");
    assert_eq!(run.envelope["status"], serde_json::json!("fail"), "{:#?}", run.envelope);
    let error = &run.envelope["error"];
    assert_eq!(error["code"], serde_json::json!("ASSERTION_FAILED"), "{:#?}", run.envelope);
    assert_eq!(
        error["details"]["reason"],
        serde_json::json!("expect_change"),
        "{:#?}",
        run.envelope
    );
}

#[test]
#[ignore = "boots a real ART guest; run under tools/guarded.sh + run-exclusive.sh"]
fn a_long_press_on_a_non_clickable_gesture_surface_changes_state() {
    let run = Run::scenario("touchdisp-gesture.yaml", "gesture");
    assert_eq!(run.envelope["status"], serde_json::json!("pass"), "{:#?}", run.envelope);
    let step = run.step(1, "swipe");
    assert_eq!(step["status"], serde_json::json!("pass"), "{step:#?}");
    // The view is not clickable, so the gesture is unhandled at the window; the
    // detector still saw the DOWN and every MOVE, i.e. the stream reached
    // View.onTouchEvent.
    let published = step["input"]["handled_evidence"]["published"]
        .as_u64()
        .unwrap_or(0);
    assert!(
        published >= 3,
        "a held gesture must publish DOWN + MOVEs + UP to the view tree: {step:#?}"
    );
    assert_eq!(
        run.bound_text(&step, "status").as_deref(),
        Some("long-pressed"),
        "GestureDetector.onLongPress never reached the non-clickable view: {step:#?}"
    );
    let txt = run.step(2, "assert_text");
    assert_eq!(txt["status"], serde_json::json!("pass"), "{txt:#?}");
}
