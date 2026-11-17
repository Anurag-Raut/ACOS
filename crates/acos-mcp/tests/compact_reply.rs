//! REL3C: one reply rule for every tool.
//!
//! Before this rule a live UI tree was inlined whole (67–111 KB launch
//! replies) and the old string bound head/tail-cut the middle of a text
//! rendering. This drives the real server with a stub `acos` whose reply
//! carries a 400-node tree and asserts the three contract points:
//!
//! * the serialized reply is under `MAX_REPLY_BYTES` (not just each string);
//! * the compact page is in reading order and a middle page is reachable with
//!   `cursor` (no head/tail cut);
//! * the full tree is written to `tree_path` with every node.

mod common;

use std::path::{Path, PathBuf};

use common::*;
use serde_json::{json, Value};

fn scratch(name: &str) -> PathBuf {
    let dir = repo_root()
        .join("target/acos-mcp-compact")
        .join(format!("{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch");
    dir
}

fn large_tree(rows: usize) -> Value {
    let nodes: Vec<Value> = (0..rows)
        .map(|i| {
            json!({
                "id": format!("n{i}"),
                "class": "android.widget.TextView",
                "resource_id": format!("dev.acos.fixture:id/row{i}"),
                "text": if i == 200 { "MIDDLE-MARKER settings row".to_string() }
                        else { format!("settings row {i} with enough text to make the tree large") },
                "content_desc": "",
                "bounds": format!("[0,{}][1080,{}]", i * 40, i * 40 + 36),
                "visible": true,
                "enabled": true,
                "clickable": i % 3 == 0,
                "children": [],
            })
        })
        .collect();
    json!({"max_nodes": 2000, "truncated": false, "nodes": nodes})
}

fn envelope() -> String {
    json!({
        "request_id": "req_compact",
        "ok": true,
        "status": "pass",
        "backend": "acos-native",
        "fidelity": "art-reference",
        "capabilities_used": ["ui.tree"],
        "result": {
            "session_id": "s_compact",
            "package": "dev.acos.fixture",
            "frame": {"frame_seq": 42, "width": 1080, "height": 2400,
                      "density": 420, "content_hash": "", "meaningful": true},
            "tree": large_tree(400),
        },
        "metrics": {"wall_ms": 1}
    })
    .to_string()
}

fn client_for_stub(dir: &Path) -> McpClient {
    let script = repo_root().join("crates/acos-mcp/tests/fixtures/bounded-error.sh");
    ensure_executable(&script);
    let envelope_file = dir.join("envelope.json");
    std::fs::write(&envelope_file, envelope()).expect("envelope");
    let backend = backend_for(
        &script,
        vec![(
            "ACOS_STUB_JSON".to_string(),
            envelope_file.display().to_string(),
        )],
    );
    let mut config = test_config();
    config.artifacts_dir = dir.join("artifacts");
    McpClient::start(backend, config)
}

fn verify_args(dir: &Path, cursor: u64) -> Value {
    json!({
        "base": {"request_id": "t_compact"},
        "scenario": fixture_path("scenarios/gaten-home.yaml"),
        "artifacts": format!("{}/artifacts", dir.display()),
        "cursor": cursor,
    })
}

#[test]
fn a_large_tree_is_compact_bounded_paged_and_fully_written() {
    let dir = scratch("large");
    let mut client = client_for_stub(&dir);
    client.initialize("2025-06-18");

    let response = client.call_tool("acos_verify", verify_args(&dir, 0));
    let text = response
        .pointer("/result/content/0/text")
        .and_then(Value::as_str)
        .expect("content text");
    assert!(
        text.len() < acos_protocol::reply::MAX_REPLY_BYTES,
        "reply is {} bytes, cap is {}",
        text.len(),
        acos_protocol::reply::MAX_REPLY_BYTES
    );
    let tree = response
        .pointer("/result/structuredContent/result/tree")
        .expect("compact tree");
    assert_eq!(tree["compact"], true);
    assert_eq!(tree["node_count"], 400);
    assert_eq!(tree["nodes"].as_array().unwrap().len(), 40);
    assert_eq!(
        tree["nodes"][0]["text"],
        "settings row 0 with enough text to make the tree large"
    );
    assert_eq!(tree["nodes"][0]["bounds"], "[0,0][1080,36]");
    assert_eq!(tree["nodes"][0]["clickable"], true);
    assert_eq!(tree["next_cursor"], 40);

    // Page 200 is where the middle of the screen lives: reachable by cursor,
    // never dropped by a head/tail cut.
    let middle = client.call_tool("acos_verify", verify_args(&dir, 200));
    let middle_tree = middle
        .pointer("/result/structuredContent/result/tree")
        .expect("middle compact tree");
    let nodes = middle_tree["nodes"].as_array().unwrap();
    assert_eq!(nodes.len(), 40);
    assert_eq!(nodes[0]["text"], "MIDDLE-MARKER settings row");
    assert_eq!(nodes[0]["resource_id"], "dev.acos.fixture:id/row200");

    // The full tree is on disk, all 400 nodes, and the reply names it.
    let path = response
        .pointer("/result/structuredContent/result/tree_path")
        .and_then(Value::as_str)
        .expect("tree_path");
    let written: Value = serde_json::from_slice(&std::fs::read(path).expect("tree file")).unwrap();
    assert_eq!(written["nodes"].as_array().unwrap().len(), 400);
    assert_eq!(
        written["nodes"][200]["text"], "MIDDLE-MARKER settings row",
        "the artifact keeps the middle of the tree"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
