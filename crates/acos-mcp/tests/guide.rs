//! The agent guide: one markdown file, served as `acos_help`, as MCP
//! `resources` and as the `verify_android_flow` prompt.
//!
//! What is asserted here:
//!
//! * the guide is embedded at build time from `docs/AGENT_GUIDE.md` and the
//!   embedded copy cannot drift from the file on disk;
//! * every topic the tool advertises slices a real section of that guide;
//! * `resources/list` + `resources/read` return the same text the tool returns,
//!   and an unknown URI is a JSON-RPC error, never an empty read;
//! * `prompts/list` + `prompts/get` produce a runnable instruction that names
//!   the real tool calls and the memory/honesty rules;
//! * `acos_help` with no topic returns a bounded skeleton, `topic` one section
//!   and `topic: "all"` the whole guide (a 6.4k-token default reply was
//!   truncated mid-scenario by at least one client).
//!
//! The text itself is checked for *content*, not just presence: a guide that
//! lost the workflow, the CLI examples, the settle rules or the error playbook
//! is a broken product, not a docs nit.

mod common;

use serde_json::{json, Value};

use common::McpClient;

fn repo_root() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .to_path_buf()
}

/// The embedded guide must be byte-identical to the checked-in file: the
/// installed binary carries this text, so a stale embed would teach agents a
/// different product than the repository documents.
#[test]
fn embedded_guide_matches_the_checked_in_file() {
    let path = repo_root().join("docs/AGENT_GUIDE.md");
    let on_disk = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
    assert_eq!(
        acos_mcp::help::AGENT_GUIDE.trim(),
        on_disk.trim(),
        "the embedded guide (include_str! of docs/AGENT_GUIDE.md) drifted from the file"
    );
}

/// Every advertised topic must slice a real, non-trivial section.
#[test]
fn every_topic_slices_a_real_section() {
    let full = acos_mcp::help::full_guide();
    for topic in acos_mcp::help::TOPICS {
        let section = acos_mcp::help::guide(Some(topic));
        assert!(
            section.len() > 200,
            "topic {topic:?} produced only {} chars",
            section.len()
        );
        assert!(
            !section.contains(&format!("<!-- topic:{topic} -->")),
            "topic {topic:?} still carries its marker"
        );
        assert!(
            full.contains(section.as_str()),
            "topic {topic:?} is not a section of the full guide"
        );
    }
    // `all` is the whole guide, and it is bigger than any single section.
    assert_eq!(acos_mcp::help::guide(Some("all")), full);
    // No topic returns the skeleton: bounded, and a section of the full guide.
    let skeleton = acos_mcp::help::guide(None);
    // The skeleton is the guide's own front section: every line of it is a line
    // of the guide (only the `<!-- topic:overview -->` markers are dropped).
    for line in skeleton.lines().filter(|line| !line.trim().is_empty()) {
        assert!(
            full.contains(line),
            "the skeleton drifted from the guide: {line:?}"
        );
    }
    assert!(skeleton.len() > 500, "the skeleton is not empty");
    // An unknown topic returns the skeleton (bounded), never the whole guide
    // and never nothing.
    assert_eq!(acos_mcp::help::guide(Some("nonsense")), skeleton);
    assert!(skeleton.len() < full.len() / 3);
}

/// The skeleton is what a fresh agent sees first: the three rules, the loop in
/// ten lines, and the topic list it can ask for next. It must stay small enough
/// that no client truncates it.
#[test]
fn the_default_help_reply_is_a_bounded_skeleton() {
    let skeleton = acos_mcp::help::guide(None);
    assert!(
        skeleton.len() < 4000,
        "the acos_help default reply is {} bytes; it must fit one frame",
        skeleton.len()
    );
    for expected in [
        "status: \"pass\"",
        "status: \"fail\"",
        "status: \"unsupported\"",
        "doctor -> inspect -> launch -> observe_tree",
        "acos_help {\"topic\": \"workflow\"}",
    ] {
        assert!(skeleton.contains(expected), "the skeleton lost {expected:?}");
    }
    // Every topic it advertises is described in one line, so an agent knows
    // which one to ask for.
    for topic in acos_mcp::help::TOPICS {
        assert!(
            skeleton.contains(topic),
            "the skeleton does not name the {topic:?} topic"
        );
    }
}

/// The guide must say the things an agent needs, in every build.
#[test]
fn the_guide_covers_the_contract() {
    let text = acos_mcp::help::full_guide();
    for expected in [
        // The loop, in order.
        "doctor -> inspect -> launch -> observe_tree",
        // Real, runnable CLI commands.
        "acos verify scenarios/counter.yaml --artifacts out/counter --json",
        "acos launch fixtures/build/apps/counter.apk --engine art --json",
        "acos session destroy --json",
        // Scenario authoring.
        "version: 1",
        "expect_change: true",
        "settle: \"fast\"",
        "There is no `sleep` step",
        // Errors and host limits.
        "HOST_MEMORY_LOW",
        "HOST_MEMORY_LIMIT",
        "UNSUPPORTED_ABI",
        // Honesty rules.
        "never a pass",
    ]
    {
        assert!(text.contains(expected), "the guide lost {expected:?}");
    }
    // What it must never mention: internal development/lock tooling.
    for forbidden in [
        "run-exclusive",
        "art.lock",
        "ACOS_ART_LOCK_SLOT",
        "slot file",
        "session queue",
        "ACOS_LOCK_WAIT_SECS",
    ] {
        assert!(
            !text.contains(forbidden),
            "the agent guide must not mention internal tooling {forbidden:?}"
        );
    }
}

/// `acos_help` with no topic returns the whole guide; with a topic it returns
/// exactly that section (and names it).
#[test]
fn help_tool_serves_the_guide_and_its_topics() {
    let mut client = McpClient::with_real_acos();
    client.initialize("2025-03-26");

    let default = client.call_tool("acos_help", json!({}));
    assert!(!common::is_error(&default), "acos_help must not error: {default}");
    let envelope = common::envelope_of(&default);
    let guide = envelope
        .pointer("/result/guide")
        .and_then(Value::as_str)
        .expect("result.guide");
    // The default reply is the skeleton, not the whole 6.4k-token guide.
    assert_eq!(guide, acos_mcp::help::guide(None));
    assert!(
        guide.len() < 4000,
        "the default acos_help reply is {} bytes",
        guide.len()
    );
    assert_eq!(
        envelope.pointer("/result/guide_topic"),
        Some(&Value::Null),
        "no topic was asked for"
    );

    // `topic: "all"` is the escape hatch for the complete guide.
    let all = client.call_tool("acos_help", json!({"topic": "all"}));
    let envelope = common::envelope_of(&all);
    assert_eq!(
        envelope.pointer("/result/guide_topic").and_then(Value::as_str),
        Some("all")
    );
    assert_eq!(
        envelope.pointer("/result/guide").and_then(Value::as_str),
        Some(acos_mcp::help::full_guide().as_str())
    );
    assert_eq!(
        envelope.pointer("/result/guide_source").and_then(Value::as_str),
        Some("docs/AGENT_GUIDE.md")
    );
    assert!(envelope
        .pointer("/result/guide_topics")
        .and_then(Value::as_array)
        .expect("guide_topics")
        .iter()
        .any(|t| t == "scenarios"));

    let scenarios = client.call_tool("acos_help", json!({"topic": "scenarios"}));
    let envelope = common::envelope_of(&scenarios);
    assert_eq!(
        envelope.pointer("/result/guide_unknown_topic"),
        Some(&json!(false))
    );
    assert_eq!(
        envelope.pointer("/result/guide_topic").and_then(Value::as_str),
        Some("scenarios")
    );
    let section = envelope
        .pointer("/result/guide")
        .and_then(Value::as_str)
        .expect("result.guide");
    assert_eq!(section, acos_mcp::help::guide(Some("scenarios")));
    // A real section, not the whole document: the CLI reference is elsewhere.
    assert!(!section.contains("## CLI reference"));
    assert!(section.contains("## Writing a scenario"));
}

/// `resources/list` advertises the guide and its topics; `resources/read`
/// returns their text; an unknown URI is a JSON-RPC error.
#[test]
fn resources_serve_the_same_guide_text() {
    let mut client = McpClient::with_real_acos();
    let init = client.initialize("2025-06-18");
    assert_eq!(
        init.pointer("/result/capabilities/resources/listChanged"),
        Some(&json!(false)),
        "initialize must advertise resources: {init}"
    );
    assert_eq!(
        init.pointer("/result/capabilities/prompts/listChanged"),
        Some(&json!(false)),
        "initialize must advertise prompts: {init}"
    );

    let list = client.request("resources/list", json!({}));
    let resources = list
        .pointer("/result/resources")
        .and_then(Value::as_array)
        .expect("resources array");
    let uris: Vec<&str> = resources
        .iter()
        .filter_map(|r| r.get("uri").and_then(Value::as_str))
        .collect();
    for expected in [
        "acos://guide",
        "acos://errors",
        "acos://guide/overview",
        "acos://guide/workflow",
        "acos://guide/cli",
        "acos://guide/scenarios",
        "acos://guide/errors",
        "acos://guide/limits",
    ] {
        assert!(uris.contains(&expected), "missing resource {expected}: {uris:?}");
    }
    for resource in resources {
        assert_eq!(resource.get("mimeType").and_then(Value::as_str), Some("text/markdown"));
        assert!(resource.get("title").and_then(Value::as_str).is_some());
    }

    let full = client.request("resources/read", json!({"uri": "acos://guide"}));
    let text = full
        .pointer("/result/contents/0/text")
        .and_then(Value::as_str)
        .expect("contents text");
    // A resource read is deliberate: `acos://guide` is the complete guide.
    assert_eq!(text, acos_mcp::help::full_guide());
    assert_eq!(
        full.pointer("/result/contents/0/mimeType").and_then(Value::as_str),
        Some("text/markdown")
    );

    let errors = client.request("resources/read", json!({"uri": "acos://errors"}));
    let text = errors
        .pointer("/result/contents/0/text")
        .and_then(Value::as_str)
        .expect("contents text");
    assert_eq!(text, acos_mcp::help::guide(Some("errors")));
    assert!(text.contains("HOST_MEMORY_LOW"));

    let unknown = client.request("resources/read", json!({"uri": "acos://nope"}));
    assert_eq!(
        unknown.pointer("/error/code").and_then(Value::as_i64),
        Some(-32602),
        "an unknown resource must be an error, not an empty read: {unknown}"
    );

    let templates = client.request("resources/templates/list", json!({}));
    assert!(templates
        .pointer("/result/resourceTemplates/0/uriTemplate")
        .and_then(Value::as_str)
        .is_some_and(|t| t.contains("{topic}")));
}

/// `prompts/list` + `prompts/get verify_android_flow`: the arguments land in the
/// text, the text names the real tool calls, and missing arguments are an error.
#[test]
fn verify_android_flow_prompt_is_usable() {
    let mut client = McpClient::with_real_acos();
    client.initialize("2025-03-26");

    let list = client.request("prompts/list", json!({}));
    let prompts = list
        .pointer("/result/prompts")
        .and_then(Value::as_array)
        .expect("prompts array");
    let prompt = prompts
        .iter()
        .find(|p| p.get("name").and_then(Value::as_str) == Some("verify_android_flow"))
        .expect("verify_android_flow prompt");
    let argument_names: Vec<&str> = prompt
        .get("arguments")
        .and_then(Value::as_array)
        .expect("arguments")
        .iter()
        .filter_map(|a| a.get("name").and_then(Value::as_str))
        .collect();
    assert_eq!(argument_names, vec!["apk", "goal"]);

    let got = client.request(
        "prompts/get",
        json!({
            "name": "verify_android_flow",
            "arguments": {
                "apk": "fixtures/build/apps/counter.apk",
                "goal": "tap Increment and the count reads 1",
            },
        }),
    );
    let text = got
        .pointer("/result/messages/0/content/text")
        .and_then(Value::as_str)
        .expect("prompt text");
    assert!(text.contains("fixtures/build/apps/counter.apk"), "{text}");
    assert!(text.contains("tap Increment and the count reads 1"), "{text}");
    assert!(text.contains("acos_verify"));
    assert!(text.contains("expect_change: true"));
    assert!(text.contains("HOST_MEMORY_LOW"));

    let missing = client.request("prompts/get", json!({"name": "verify_android_flow"}));
    assert_eq!(
        missing.pointer("/error/code").and_then(Value::as_i64),
        Some(-32602),
        "a prompt without its arguments must be an error: {missing}"
    );
    let unknown = client.request("prompts/get", json!({"name": "nope"}));
    assert_eq!(unknown.pointer("/error/code").and_then(Value::as_i64), Some(-32602));
}
