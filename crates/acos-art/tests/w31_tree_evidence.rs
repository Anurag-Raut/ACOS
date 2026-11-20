//! W31: **the saved per-step tree must be the tree the assertion evaluated.**
//!
//! W28's report claimed the Gaten release run saved a `DecorView`-only tree
//! while its `wait_for {text: "Recently looked up"}` passed. The artifacts from
//! that run (and the W31 rerun) show the opposite: `tree/<frame_seq>.json` is
//! the very `UiTree` object that `SessionObservation` carried, and the selector
//! was evaluated on it (`acos-verify`'s `write_tree` takes
//! `observation.tree`). What the rerun *did* expose is a real honesty gap in
//! the same artifacts: the interactive probe carried at most 160 nodes per
//! window and never said so, so a saved tree at the cap claimed
//! `truncated: false`. `LaunchMain` now emits `node_budget`/`truncated` and the
//! engine copies the flag into the `UiTree`.
//!
//! This test is the guard for both halves of the verdict evidence:
//!
//! 1. every `wait_for`/`assert_text` step's saved tree contains the node the
//!    selector matched (and the step's `matched_node_ids` resolve in that same
//!    file with that same `frame_seq`);
//! 2. a tree whose node count reached the probe budget is marked
//!    `truncated: true` — never silently complete.
//!
//! Teeth: the negative cases feed the same checker a `DecorView`-only tree and
//! a 160-node tree that claims `truncated: false`; both must be rejected. The
//! positive case runs against the real W31 artifacts when this checkout has
//! them (`results/art-runtime/evidence/W31-artifacts/gaten-release-evidence`)
//! and is skipped (with a line) in a clean checkout, so the suite stays honest
//! about what it exercised.

use serde_json::{json, Value};
use std::path::{Path, PathBuf};

/// The interactive probe's per-window cap (`LaunchMain.PROBE_NODE_BUDGET`).
const PROBE_NODE_BUDGET: usize = 160;

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap_or_else(|_| Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."))
}

fn walk<'a>(node: &'a Value, out: &mut Vec<&'a Value>) {
    out.push(node);
    if let Some(children) = node.get("children").and_then(Value::as_array) {
        for child in children {
            walk(child, out);
        }
    }
}

fn all_nodes(tree: &Value) -> Vec<&Value> {
    let mut out = Vec::new();
    if let Some(roots) = tree.get("nodes").and_then(Value::as_array) {
        for root in roots {
            walk(root, &mut out);
        }
    }
    out
}

/// The invariant, in one place so the negative cases exercise the same code as
/// the real artifacts.
///
/// * the saved tree has at least one node;
/// * it contains a node whose `text` is exactly `expected_text` (what
///   `wait_for`/`assert_text` matched);
/// * every id in `matched_node_ids` resolves in the same file;
/// * if the walk reached the probe budget, the tree says `truncated: true`.
fn check_saved_tree(
    tree: &Value,
    expected_text: &str,
    matched_node_ids: &[String],
) -> Result<usize, String> {
    let nodes = all_nodes(tree);
    if nodes.is_empty() {
        return Err("the saved tree has no nodes".to_string());
    }
    if !nodes
        .iter()
        .any(|node| node.get("text").and_then(Value::as_str) == Some(expected_text))
    {
        return Err(format!(
            "the saved tree ({} node(s)) has no node whose text is exactly {expected_text:?}; \
             the assertion matched a tree that was not saved",
            nodes.len()
        ));
    }
    for id in matched_node_ids {
        if !nodes
            .iter()
            .any(|node| node.get("id").and_then(Value::as_str) == Some(id.as_str()))
        {
            return Err(format!(
                "matched node id {id:?} is not in the saved tree ({} node(s))",
                nodes.len()
            ));
        }
    }
    let truncated = tree
        .get("truncated")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if nodes.len() >= PROBE_NODE_BUDGET && !truncated {
        return Err(format!(
            "the saved tree has {} node(s), i.e. it reached the probe cap of {PROBE_NODE_BUDGET}, \
             but `truncated` is false: the artifact claims a complete hierarchy it does not contain",
            nodes.len()
        ));
    }
    Ok(nodes.len())
}

fn read_json(path: &Path) -> Value {
    let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    serde_json::from_slice(&bytes).unwrap_or_else(|e| panic!("parse {}: {e}", path.display()))
}

/// The real run's artifacts, when this checkout has them.
fn artifacts_dir() -> Option<PathBuf> {
    let dir = repo_root().join("results/art-runtime/evidence/W31-artifacts/gaten-release-evidence");
    (dir.join("steps").is_dir() && dir.join("tree").is_dir()).then_some(dir)
}

#[test]
fn saved_wait_for_and_assert_text_trees_contain_the_matched_node() {
    let Some(dir) = artifacts_dir() else {
        acos_test_support::require_prereq(false, "saved_wait_for_and_assert_text_trees_contain_the_matched_node", "W31 tree-evidence check skipped: no artifacts at results/art-runtime/evidence/W31-artifacts/gaten-release-evidence (the synthetic cases below still run)");
        return;
    };
    let text = "Recently looked up";
    let steps_dir = dir.join("steps");
    let mut checked = 0usize;
    let mut saw_wait_for = false;
    let mut saw_assert_text = false;
    for entry in std::fs::read_dir(&steps_dir).expect("read steps dir") {
        let path = entry.expect("step entry").path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let step = read_json(&path);
        let kind = step.get("step").and_then(Value::as_str).unwrap_or("");
        if kind != "wait_for" && kind != "assert_text" {
            continue;
        }
        assert_eq!(
            step.get("status").and_then(Value::as_str),
            Some("pass"),
            "{} is not a passing step",
            path.display()
        );
        saw_wait_for |= kind == "wait_for";
        saw_assert_text |= kind == "assert_text";
        let frame_seq = step
            .get("frame_seq")
            .and_then(Value::as_u64)
            .unwrap_or_else(|| panic!("{} has no frame_seq", path.display()));
        // The tree artifact this step points at.
        let tree_path = step
            .get("evidence")
            .and_then(Value::as_array)
            .and_then(|evidence| {
                evidence.iter().find_map(|item| {
                    let p = item.get("path").and_then(Value::as_str)?;
                    p.starts_with("tree/").then(|| dir.join(p))
                })
            })
            .unwrap_or_else(|| panic!("{} has no tree evidence", path.display()));
        assert_eq!(
            tree_path,
            dir.join(format!("tree/{frame_seq}.json")),
            "{} must cite the tree bound to its own frame",
            path.display()
        );
        let tree = read_json(&tree_path);
        assert_eq!(
            tree.get("frame")
                .and_then(|f| f.get("frame_seq"))
                .and_then(Value::as_u64),
            Some(frame_seq),
            "{}: saved tree frame_seq differs from the step's",
            tree_path.display()
        );
        let matched: Vec<String> = step
            .get("assertions")
            .and_then(Value::as_array)
            .map(|assertions| {
                assertions
                    .iter()
                    .flat_map(|a| {
                        a.get("matched_node_ids")
                            .and_then(Value::as_array)
                            .cloned()
                            .unwrap_or_default()
                    })
                    .filter_map(|id| id.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        let count = check_saved_tree(&tree, text, &matched)
            .unwrap_or_else(|e| panic!("{}: {e}", tree_path.display()));
        assert!(
            count > 1,
            "{}: a {count}-node tree cannot be the app's hierarchy",
            tree_path.display()
        );
        checked += 1;
    }
    assert!(
        saw_wait_for,
        "no wait_for step found in {}",
        steps_dir.display()
    );
    assert!(
        saw_assert_text,
        "no assert_text step found in {} (the bound-frame assertion is the second \
         evaluation of the same selector)",
        steps_dir.display()
    );
    assert_eq!(
        checked, 2,
        "expected exactly the two selector steps, checked {checked}"
    );
}

#[test]
fn a_decor_only_saved_tree_is_rejected() {
    // The shape W28's report described: one DecorView root, no children.
    let tree = json!({
        "nodes": [{
            "id": "n0_",
            "class": "com.android.internal.policy.DecorView",
            "text": "",
            "children": []
        }],
        "truncated": false
    });
    let error = check_saved_tree(&tree, "Recently looked up", &[])
        .expect_err("a DecorView-only tree must not satisfy the evidence check");
    assert!(
        error.contains("no node whose text"),
        "unexpected error: {error}"
    );
}

#[test]
fn a_tree_at_the_node_budget_must_be_marked_truncated() {
    // 160 nodes, `truncated: false`: the pre-W31 artifact lie.
    let full = json!({
        "nodes": (0..PROBE_NODE_BUDGET)
            .map(|i| json!({"id": format!("n{i}_"), "text": "", "children": []}))
            .collect::<Vec<_>>(),
        "truncated": false
    });
    let error = check_saved_tree(&full, "", &[])
        .expect_err("a tree at the probe cap without `truncated` must be rejected");
    assert!(error.contains("truncated"), "unexpected error: {error}");

    // The same tree, honestly flagged, passes.
    let honest = json!({
        "nodes": (0..PROBE_NODE_BUDGET)
            .map(|i| json!({"id": format!("n{i}_"), "text": "", "children": []}))
            .collect::<Vec<_>>(),
        "truncated": true
    });
    assert_eq!(
        check_saved_tree(&honest, "", &[]).expect("flagged truncation is valid evidence"),
        PROBE_NODE_BUDGET
    );
}
