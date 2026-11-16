//! REL3S: the scenario `swipe` and `scroll_until` steps on a real ART guest.
//!
//! Session 1 built the gesture stream and the `until` search for the CLI/MCP
//! surface (`acos_swipe`) but the scenario schema had no swipe step, so a
//! scenario could not scroll at all. This test proves the two scenario steps on
//! the `scroll-list` fixture (100 rows, `Row 0`..`Row 99`):
//!
//! 1. `swipe` moves the list: `Row 0` leaves the viewport and a row that the
//!    launch tree did not render (a freshly revealed row) is asserted by
//!    `assert_text` on the swipe's own settled frame;
//! 2. `scroll_until` searches for `Row 80` (its found frame must be a settled
//!    traversal, reported with its attempts) and the next step taps it, so the
//!    fixture's header reads `Selected: Row 80`.
//!
//! **Ignored by default** because it boots a real ART guest. Run it under the
//! shared lock and the host guard:
//!
//! ```text
//! ACOS_APP_ONLY=scroll-list tools/build-apps.sh
//! tools/art-runtime/run-exclusive.sh --task REL3S -- \
//!   tools/guarded.sh --mem 10G --time 900 -- \
//!   cargo test -p acos-cli --release --test rel3s_scenario -- --ignored --test-threads=1
//! ```

use std::path::{Path, PathBuf};

use serde_json::Value;

fn repo_root() -> PathBuf {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    std::fs::canonicalize(&dir).unwrap_or_else(|_| dir.to_path_buf())
}

fn acos() -> &'static str {
    env!("CARGO_BIN_EXE_acos")
}

/// The release `acos-art` launcher this branch built (explicit env wins).
fn art_bin() -> PathBuf {
    if let Some(path) = std::env::var_os("ACOS_ART_BIN") {
        let path = PathBuf::from(path);
        if path.is_file() {
            return path;
        }
    }
    for target in [
        ".cache/rel3h-target",
        ".cache/rel3s-target",
        ".cache/rel3s2-target",
    ] {
        let path = repo_root().join(target).join("release/acos-art");
        if path.is_file() {
            return path;
        }
    }
    repo_root().join(".cache/rel3s2-target/release/acos-art")
}

fn scratch(tag: &str) -> PathBuf {
    let dir = repo_root()
        .join(".cache")
        .join(format!("rel3s-scenario-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

/// The scenario's launch prelude: one offscreen display, one launch, one
/// explicit capture so the "before" tree is an artifact this test can read.
const PRELUDE: &str = r#"
version: 1
name: rel3s-scenario
app: fixtures/build/apps/scroll-list.apk
isolation: process
display: {mode: offscreen, width: 1080, height: 2400, density_dpi: 420}
steps:
  - launch: {activity: .MainActivity}
  - capture: {name: before}
"#;

/// Run one scenario with `--engine art` and return its artifacts dir + verdict.
fn run_scenario(tag: &str, steps: &str) -> (PathBuf, Value) {
    run_scenario_yaml(tag, &format!("{PRELUDE}{steps}"))
}

/// Run a complete scenario document (a negative twin needs its own top-level
/// `expectation`, which the shared prelude does not carry).
fn run_scenario_yaml(tag: &str, yaml: &str) -> (PathBuf, Value) {
    let dir = scratch(tag);
    let scenario = dir.join("scenario.yaml");
    std::fs::write(&scenario, yaml).expect("write scenario");
    let apk = repo_root().join("fixtures/build/apps/scroll-list.apk");
    assert!(
        apk.is_file(),
        "build the fixture first: `ACOS_APP_ONLY=scroll-list tools/build-apps.sh` "
    );
    let artifacts = dir.join("artifacts");
    let output = std::process::Command::new(acos())
        .env("ACOS_NO_DAEMON", "1")
        .env("ACOS_ART_BIN", art_bin())
        .env("ACOS_RUNTIME_DIR", dir.join("runtime"))
        .env("ACOS_CACHE_DIR", dir.join("cache"))
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
    let verdict = std::fs::read_to_string(artifacts.join("verdict.json")).unwrap_or_else(|error| {
        panic!(
            "verdict.json must exist: {error}\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    });
    (
        artifacts,
        serde_json::from_str(&verdict).expect("verdict.json is JSON"),
    )
}

fn step<'a>(verdict: &'a Value, index: usize) -> &'a Value {
    &verdict["steps"].as_array().expect("steps")[index]
}

fn verdict_string(verdict: &Value) -> String {
    serde_json::to_string_pretty(verdict).unwrap_or_default()
}

/// The visible `Row N` labels of the `capture` step's tree artifact.
fn capture_tree_rows(artifacts: &Path, verdict: &Value) -> Vec<u32> {
    let frame = step(verdict, 1)["frame_seq"]
        .as_u64()
        .expect("capture step carries a frame");
    let dir = artifacts.join("tree");
    let path = std::fs::read_dir(&dir)
        .expect("tree dir")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .find(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| {
                    name.strip_prefix(&frame.to_string())
                        .is_some_and(|rest| rest.starts_with('-') || rest.starts_with('.'))
                })
        })
        .unwrap_or_else(|| panic!("no tree artifact for frame {frame} in {}", dir.display()));
    let tree: Value = serde_json::from_str(&std::fs::read_to_string(&path).expect("read tree"))
        .expect("tree JSON");
    fn walk(nodes: &Value, out: &mut Vec<u32>) {
        let Some(array) = nodes.as_array() else {
            return;
        };
        for node in array {
            if node["visible"].as_bool() == Some(true) {
                if let Some(index) = node["text"]
                    .as_str()
                    .and_then(|text| text.strip_prefix("Row "))
                    .and_then(|n| n.parse().ok())
                {
                    out.push(index);
                }
            }
            walk(&node["children"], out);
        }
    }
    let mut out = Vec::new();
    walk(&tree["nodes"], &mut out);
    out.sort_unstable();
    out
}

#[test]
#[ignore = "boots a real ART guest; run under tools/guarded.sh + run-exclusive.sh"]
fn swipe_reveals_an_offscreen_row_and_scroll_until_taps_a_deep_row() {
    // ---- 1. a plain `swipe` moves the list --------------------------------
    // A slow drag (well under the framework's minimum fling velocity) moves the
    // content by roughly the finger distance, so a row the launch tree did not
    // render comes into view. That row is asserted on the swipe's own settled
    // frame: the step's frame binding, not a later traversal, backs the claim.
    let (before_artifacts, before_verdict) = run_scenario(
        "before",
        r#"  - swipe:
      from: {x: 540, y: 1900}
      to: {x: 540, y: 1500}
      duration_ms: 3000
  - assert_not_visible: {selector: {text: {exact: "Row 0"}}}
"#,
    );
    assert_eq!(
        before_verdict["verdict"],
        "pass",
        "the swipe must move the list: {}",
        verdict_string(&before_verdict)
    );
    let initial = capture_tree_rows(&before_artifacts, &before_verdict);
    assert!(
        !initial.is_empty(),
        "the launch tree must show the list's rows: {initial:?}"
    );
    let fold = *initial.last().expect("non-empty rows");
    let target = fold + 1;
    assert!(
        !initial.contains(&target),
        "the asserted row must start off screen (fold Row {fold}, initial {initial:?})"
    );

    let (swipe_artifacts, swipe_verdict) = run_scenario(
        "swipe",
        &format!(
            r#"  - swipe:
      from: {{x: 540, y: 1900}}
      to: {{x: 540, y: 1500}}
      duration_ms: 3000
  - assert_text: {{selector: {{text: {{exact: "Row {target}"}}}}, text: {{exact: "Row {target}"}}}}
  - assert_not_visible: {{selector: {{text: {{exact: "Row 0"}}}}}}
"#
        ),
    );
    let _ = swipe_artifacts;
    assert_eq!(
        swipe_verdict["verdict"],
        "pass",
        "the swipe scenario must pass: {}",
        verdict_string(&swipe_verdict)
    );
    let swipe = step(&swipe_verdict, 2);
    assert_eq!(swipe["step"], "swipe");
    assert_eq!(
        swipe["input"]["handled"], true,
        "the list consumes the gesture (REL3E): {swipe:#}"
    );
    assert!(
        swipe["input"]["handled_evidence"]["published"]
            .as_u64()
            .unwrap_or(0)
            > 10,
        "a 3 s gesture publishes DOWN + MOVEs + UP, not a tap: {swipe:#}"
    );
    assert_eq!(
        swipe["settle"]["mode"], "strict",
        "a swipe settles under the strict rule by default: {swipe:#}"
    );
    assert_eq!(
        step(&swipe_verdict, 3)["status"],
        "pass",
        "the revealed Row {target} is asserted on the swipe's settled frame: {}",
        verdict_string(&swipe_verdict)
    );
    assert_eq!(
        step(&swipe_verdict, 4)["status"],
        "pass",
        "Row 0 left the viewport after a real gesture"
    );

    // ---- 2. `scroll_until` finds a deep row, then taps it ------------------
    let (_, scroll_verdict) = run_scenario(
        "scroll",
        r#"  - scroll_until:
      selector: {text: {exact: "Row 80"}}
      from: {x: 540, y: 1900}
      to: {x: 540, y: 1400}
      duration_ms: 600
      max_attempts: 20
  - tap: {selector: {text: {exact: "Row 80"}}}
  - assert_text: {selector: {id: selected}, text: {exact: "Selected: Row 80"}}
"#,
    );
    assert_eq!(
        scroll_verdict["verdict"],
        "pass",
        "the scroll_until scenario must pass: {}",
        verdict_string(&scroll_verdict)
    );
    let scroll = step(&scroll_verdict, 2);
    assert_eq!(scroll["step"], "scroll_until");
    assert_eq!(
        scroll["input"]["until"]["found"], true,
        "the search reports its found node: {scroll:#}"
    );
    assert!(
        scroll["input"]["until"]["attempts"]
            .as_u64()
            .is_some_and(|attempts| (1..=20).contains(&attempts)),
        "the search is bounded and reports how many gestures it took: {scroll:#}"
    );
    assert!(
        scroll["input"]["until"]["frame_seq"].as_u64().unwrap_or(0) > 0,
        "found is backed by a settled frame: {scroll:#}"
    );
    assert_eq!(
        step(&scroll_verdict, 3)["status"],
        "pass",
        "the found row is really on screen: the tap lands on it"
    );
    assert_eq!(
        step(&scroll_verdict, 4)["status"],
        "pass",
        "the tap selected Row 80 (the fixture's own click handler): {}",
        verdict_string(&scroll_verdict)
    );
    assert_eq!(
        step(&scroll_verdict, 3)["input"]["handled"],
        true,
        "the row's click handler consumed the tap"
    );
}

/// REL3H: a scenario `scroll_until` that reaches the end of the list stops
/// there and says so.
///
/// `Row 9999` does not exist in a 100-row list. The search must walk to the last
/// row, notice that the next gesture moved nothing, and fail with
/// `END_OF_LIST` - `input.until.end_of_list: true`, its attempt count, and the
/// last settled tree's texts - instead of spending every remaining gesture and
/// answering `ELEMENT_NOT_FOUND` (which an agent cannot tell from a selector
/// that never renders). Both are non-passes; only one says *why*.
#[test]
#[ignore = "boots a real ART guest; run under tools/guarded.sh + run-exclusive.sh"]
fn scroll_until_stops_at_the_end_of_the_list() {
    let (_, verdict) = run_scenario_yaml(
        "end-of-list",
        r#"
version: 1
name: rel3h-end-of-list
app: fixtures/build/apps/scroll-list.apk
expectation: non_pass
isolation: process
display: {mode: offscreen, width: 1080, height: 2400, density_dpi: 420}
steps:
  - launch: {activity: .MainActivity}
  - scroll_until: {selector: {text: {exact: "Row 9999"}}, direction: up, fling: true, max_attempts: 12}
"#,
    );
    assert_ne!(
        verdict["verdict"],
        Value::from("pass"),
        "a search for a row that is not in the list is never a pass: {}",
        verdict_string(&verdict)
    );
    let search = step(&verdict, 1);
    assert_eq!(search["step"], Value::from("scroll_until"));
    assert_eq!(
        search["status"],
        Value::from("fail"),
        "the step failed: {}",
        verdict_string(&verdict)
    );
    assert_eq!(
        search["error"]["code"],
        Value::from("END_OF_LIST"),
        "the end of the list is its own answer: {}",
        verdict_string(&verdict)
    );
    let until = &search["input"]["until"];
    assert_eq!(until["found"], Value::from(false), "{until}");
    assert_eq!(
        until["end_of_list"],
        Value::from(true),
        "the step JSON says the list ended: {until}"
    );
    let attempts = until["attempts"].as_u64().unwrap_or(0);
    assert!(
        (1..12).contains(&attempts),
        "the search stops at the end instead of spending every attempt: {until}"
    );
    assert_eq!(until["max_attempts"], Value::from(12), "{until}");
    assert!(
        until["last_tree_texts"]
            .as_array()
            .is_some_and(|texts| texts.iter().any(|text| text == "Row 99")),
        "the last settled tree is the end of the list: {until}"
    );
}
