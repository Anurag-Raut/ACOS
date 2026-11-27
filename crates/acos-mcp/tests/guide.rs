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
        skeleton.len() < 3072,
        "the acos_help default reply is {} bytes; the budget is 3 KiB (FB6)",
        skeleton.len()
    );
    for expected in [
        "status: \"pass\"",
        "status: \"fail\"",
        "status: \"unsupported\"",
        "doctor -> inspect -> launch -> observe_tree",
        "acos_help {\"topic\": \"workflow\"}",
    ] {
        assert!(
            skeleton.contains(expected),
            "the skeleton lost {expected:?}"
        );
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
    ] {
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
    assert!(
        !common::is_error(&default),
        "acos_help must not error: {default}"
    );
    let envelope = common::envelope_of(&default);
    let guide = envelope
        .pointer("/result/guide")
        .and_then(Value::as_str)
        .expect("result.guide");
    // The default reply is the skeleton, not the whole 6.4k-token guide.
    assert_eq!(guide, acos_mcp::help::guide(None));
    assert!(
        guide.len() < 3072,
        "the default acos_help reply is {} bytes; the budget is 3 KiB",
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
        envelope
            .pointer("/result/guide_topic")
            .and_then(Value::as_str),
        Some("all")
    );
    assert_eq!(
        envelope.pointer("/result/guide").and_then(Value::as_str),
        Some(acos_mcp::help::full_guide().as_str())
    );
    assert_eq!(
        envelope
            .pointer("/result/guide_source")
            .and_then(Value::as_str),
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
        envelope
            .pointer("/result/guide_topic")
            .and_then(Value::as_str),
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
        assert!(
            uris.contains(&expected),
            "missing resource {expected}: {uris:?}"
        );
    }
    for resource in resources {
        assert_eq!(
            resource.get("mimeType").and_then(Value::as_str),
            Some("text/markdown")
        );
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
        full.pointer("/result/contents/0/mimeType")
            .and_then(Value::as_str),
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
    assert!(
        text.contains("tap Increment and the count reads 1"),
        "{text}"
    );
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
    assert_eq!(
        unknown.pointer("/error/code").and_then(Value::as_i64),
        Some(-32602)
    );
}

/// **FB6 (1): the `mcp` topic is generated from the live tool list.**
///
/// The cheat sheet an agent needs ("which tool, which arguments, what comes
/// back") used to be unanswerable from `acos_help`: the workflow topic taught
/// the *CLI*, so an MCP agent had to guess the tool for a screenshot. The table
/// is generated from the same descriptors `tools/list` serves, so it cannot
/// drift — and this test is the drift alarm:
///
/// * every **advertised** tool has a row, with a paste-able example and a real
///   "what it returns" clause;
/// * no **hidden** tool (a command this build does not implement) appears, so
///   the guide never teaches a call that answers `unsupported`;
/// * the topic is discoverable (its name is in `guide_topics` and in the
///   default skeleton) and stays small.
#[test]
fn the_mcp_topic_is_generated_from_the_advertised_tools() {
    let mut client = McpClient::with_real_acos();
    client.initialize("2025-03-26");

    // The advertised set, straight from `tools/list`.
    let list = client.request("tools/list", json!({}));
    let advertised: Vec<String> = list
        .pointer("/result/tools")
        .and_then(Value::as_array)
        .expect("tools array")
        .iter()
        .filter_map(|tool| tool.get("name").and_then(Value::as_str))
        .map(str::to_string)
        .collect();
    assert!(
        advertised.iter().any(|name| name == "acos_verify"),
        "the real backend must advertise acos_verify: {advertised:?}"
    );

    let reply = client.call_tool("acos_help", json!({"topic": "mcp"}));
    assert!(
        !common::is_error(&reply),
        "acos_help mcp must not error: {reply}"
    );
    let envelope = common::envelope_of(&reply);
    assert_eq!(
        envelope
            .pointer("/result/guide_topic")
            .and_then(Value::as_str),
        Some("mcp")
    );
    assert_eq!(
        envelope.pointer("/result/guide_unknown_topic"),
        Some(&json!(false))
    );
    let topic = envelope
        .pointer("/result/guide")
        .and_then(Value::as_str)
        .expect("result.guide");
    assert!(topic.starts_with("## MCP tools"), "{topic}");
    assert!(
        envelope
            .pointer("/result/guide_topics")
            .and_then(Value::as_array)
            .expect("guide_topics")
            .iter()
            .any(|name| name == "mcp"),
        "the topic list must advertise `mcp`"
    );

    // Every advertised tool has a row, and that row is usable.
    for name in &advertised {
        let row = topic
            .lines()
            .find(|line| line.starts_with(&format!("| `{name}` |")))
            .unwrap_or_else(|| {
                panic!("the mcp topic has no row for advertised tool {name}:\n{topic}")
            });
        let cells: Vec<&str> = row.trim_matches('|').split('|').map(str::trim).collect();
        assert_eq!(
            cells.len(),
            5,
            "a row must be tool|required|optional|example|returns: {row}"
        );
        assert!(
            cells[3].starts_with(&format!("`{name} ")),
            "the example call for {name} must be paste-able: {row}"
        );
        assert!(
            !cells[4].contains("no cheat-sheet entry"),
            "advertised tool {name} has no \"what it returns\" entry: {row}"
        );
    }

    // No hidden tool: a protocol command this build does not implement is not in
    // `tools/list` and must not be taught here either.
    for hidden in [
        "acos_capture",
        "acos_assert",
        "acos_events",
        "acos_profile",
        "acos_capability_report",
    ] {
        assert!(
            !advertised.iter().any(|name| name == hidden),
            "{hidden} must not be advertised by this build (test premise)"
        );
        assert!(
            !topic.contains(&format!("`{hidden}`")),
            "the mcp topic teaches hidden tool {hidden}"
        );
    }

    // Recipes: the four questions agents actually asked, answered up front.
    let workflow = acos_mcp::help::guide(Some("workflow"));
    for expected in [
        "Take a screenshot",
        "Type into a field",
        "content_desc",
        "Prove a flow",
        // The screenshot answer must name the live MCP capture option and
        // the CLI/scenario alternatives.
        "\"screenshot\": true",
        "acos_observe_tree",
        "NO_FRESH_FRAME",
        "acos observe --screenshot",
        "capture: {name: shot, format: png, fresh: true}",
    ] {
        assert!(
            workflow.contains(expected),
            "the workflow recipes lost {expected:?}"
        );
    }
    assert!(
        workflow.find("## Recipes").unwrap() < workflow.find("## The loop").unwrap(),
        "the recipes must come first in the workflow topic"
    );
}

/// **FB6: the generated tool text never mentions ACOS's internal lab tooling.**
///
/// `run-exclusive.sh`, the ART lock slot and the dev gates are how *this*
/// repository serialises its own runs; an agent driving the product must never
/// be told to use them.
#[test]
fn no_help_topic_mentions_internal_tooling() {
    let mut client = McpClient::with_real_acos();
    client.initialize("2025-03-26");
    for topic in [
        "mcp",
        "workflow",
        "cli",
        "overview",
        "limits",
        "scenarios",
        "errors",
        "all",
    ] {
        let reply = client.call_tool("acos_help", json!({"topic": topic}));
        let envelope = common::envelope_of(&reply);
        let text = envelope
            .pointer("/result/guide")
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("no guide text for topic {topic}"));
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
                "the {topic} reply must not mention internal tooling {forbidden:?}"
            );
        }
    }
}

/// **FB6 (1): a topic can be fetched as an MCP resource too** — the `mcp`
/// cheat sheet must serve the same generated text as the tool.
#[test]
fn the_mcp_topic_is_served_as_a_resource() {
    let mut client = McpClient::with_real_acos();
    client.initialize("2025-03-26");
    let read = client.request("resources/read", json!({"uri": "acos://guide/mcp"}));
    let text = read
        .pointer("/result/contents/0/text")
        .and_then(Value::as_str)
        .expect("contents text");
    assert!(text.starts_with("## MCP tools"), "{text}");
    assert!(text.contains("`acos_verify`"));
    let list = client.request("resources/list", json!({}));
    assert!(
        list.pointer("/result/resources")
            .and_then(Value::as_array)
            .expect("resources")
            .iter()
            .any(|r| r.get("uri").and_then(Value::as_str) == Some("acos://guide/mcp")),
        "acos://guide/mcp must be listed"
    );
}

/// **FB6 (4): the meta tools report `fidelity: "n/a"`**, not a misleading
/// `static-analysis` (they say nothing about the app under test).
#[test]
fn meta_tools_report_fidelity_not_applicable() {
    let mut client = McpClient::with_real_acos();
    client.initialize("2025-06-18");
    let list = client.request("tools/list", json!({}));
    let tools = list
        .pointer("/result/tools")
        .and_then(Value::as_array)
        .expect("tools array");
    for name in ["acos_help", "acos_capabilities", "acos_doctor"] {
        let tool = tools
            .iter()
            .find(|tool| tool.get("name").and_then(Value::as_str) == Some(name))
            .unwrap_or_else(|| panic!("{name} must be advertised"));
        assert_eq!(
            tool.pointer("/annotations/fidelity")
                .and_then(Value::as_str),
            Some("n/a"),
            "{name} must report fidelity n/a: {tool}"
        );
        assert_eq!(
            tool.pointer("/_meta/acos/fidelity").and_then(Value::as_str),
            Some("n/a")
        );
    }
    // The session tools genuinely run the app: they keep a real fidelity.
    let launch = tools
        .iter()
        .find(|tool| tool.get("name").and_then(Value::as_str) == Some("acos_launch"))
        .expect("acos_launch");
    assert_eq!(
        launch
            .pointer("/annotations/fidelity")
            .and_then(Value::as_str),
        Some("native-runtime")
    );
}

/// ADR-0029 item 6: `acos_capabilities` and the guide's limits section are
/// generated from the service-module registry, and the two cannot disagree.
///
/// Before the module framework there was no registry at all: the limits section
/// was hand-written prose (the feedback log's root cause D) and nothing tied it
/// to what the build actually served.
#[test]
fn the_limits_section_is_generated_from_the_module_registry() {
    use acos_mcp::help::MODULES_MARKER;
    let registry = acos_services::module::ModuleRegistry::standard();
    let generated = registry.limits_markdown();

    // The guide carries the marker in its limits topic, not a hand-written copy
    // of the table.
    assert!(
        acos_mcp::help::AGENT_GUIDE.contains(acos_mcp::help::MODULES_MARKER),
        "docs/AGENT_GUIDE.md must carry the module marker in its limits topic"
    );
    assert_eq!(acos_mcp::help::generated_limits(), generated);

    let limits = acos_mcp::help::guide(Some("limits"));
    assert!(
        !limits.contains(acos_mcp::help::MODULES_MARKER),
        "the marker must be replaced by the generated table"
    );
    // Verbatim: the rendered section *ends* with the registry's own table.
    assert!(
        limits.ends_with(generated.trim_end()),
        "the limits topic must carry the registry's own table verbatim"
    );

    // Every module the build ships appears, with its profiles.
    for manifest in registry.manifests() {
        assert!(
            limits.contains(&format!("`{}`", manifest.id)),
            "module {} is missing from the limits topic",
            manifest.id
        );
        for profile in manifest.profiles_with_off() {
            assert!(
                limits.contains(&format!("`{profile}`")),
                "module {} does not document its {profile} profile",
                manifest.id
            );
        }
    }
    // Every gap name appears with its decision.
    for tombstone in acos_services::module::TOMBSTONES {
        assert!(
            limits.contains(&format!("`{}`", tombstone.name)),
            "gap name {} is missing from the limits topic",
            tombstone.name
        );
    }

    // `topic: "all"` renders the same generated table (one guide, one source).
    let all = acos_mcp::help::full_guide();
    assert!(all.contains(generated.trim_end()));
}

/// ADR-0029 item 4: the session-module tools are advertised with a closed
/// schema, so a typo in `services:`/`acos_services` is caught by the client's
/// validator instead of being silently ignored by the server.
#[test]
fn the_service_module_tools_advertise_closed_schemas() {
    let mut client = McpClient::with_real_acos();
    let list = client.request("tools/list", json!({}));
    let tools = list
        .pointer("/result/tools")
        .and_then(Value::as_array)
        .expect("tools array");
    let services = tools
        .iter()
        .find(|tool| tool.get("name").and_then(Value::as_str) == Some("acos_services"))
        .expect("acos_services must be advertised");
    let schema = services.get("inputSchema").expect("inputSchema");
    assert_eq!(
        schema.get("additionalProperties").and_then(Value::as_bool),
        Some(false),
        "acos_services must be a closed schema: {schema}"
    );
    let ops = schema
        .pointer("/properties/op/enum")
        .and_then(Value::as_array)
        .expect("op enum");
    let ops: Vec<&str> = ops.iter().filter_map(Value::as_str).collect();
    // `ctl` is the scripted-module control operation (MODLOC), and
    // `replay_intent` replays a saved intent from a capability prompt (v0.6.1):
    // the enum is closed, so an agent cannot invent an op and have it silently
    // ignored.
    assert_eq!(
        ops,
        ["list", "status", "attach", "detach", "ctl", "replay_intent"]
    );
    // `ctl` needs its own op name and argument object, both declared.
    assert_eq!(
        schema
            .pointer("/properties/ctl_op/type")
            .and_then(Value::as_str),
        Some("string")
    );
    assert_eq!(
        schema
            .pointer("/properties/args/type")
            .and_then(Value::as_str),
        Some("object")
    );
    assert_eq!(
        schema
            .pointer("/properties/intent_id/type")
            .and_then(Value::as_str),
        Some("integer")
    );
    assert_eq!(
        schema.pointer("/required/0").and_then(Value::as_str),
        Some("op")
    );

    let launch = tools
        .iter()
        .find(|tool| tool.get("name").and_then(Value::as_str) == Some("acos_launch"))
        .expect("acos_launch");
    let item = launch
        .pointer("/inputSchema/properties/services/items")
        .expect("acos_launch.services schema");
    assert_eq!(
        item.get("additionalProperties").and_then(Value::as_bool),
        Some(false),
        "a services item must be closed: {item}"
    );
    assert_eq!(
        item.pointer("/properties/profile/enum")
            .and_then(Value::as_array)
            .map(|v| v.iter().filter_map(Value::as_str).collect::<Vec<_>>()),
        Some(vec!["off", "stub", "scripted", "real"])
    );
}

/// REL3F: the CLI topic — not the bounded overview — carries the stream
/// contract an agent needs to parse output safely: `--json` puts the JSON
/// document alone on stdout, the human summary goes to stderr, and `acos guide`
/// reaches this text without MCP.
#[test]
fn the_cli_topic_documents_the_stdout_stderr_contract() {
    let cli = acos_mcp::help::guide(Some("cli"));
    for expected in [
        "Human output contract",
        "stdout is the JSON document",
        "human summary",
        "to stderr",
        "acos guide [topic]",
        "acos guide all",
    ] {
        assert!(cli.contains(expected), "the cli topic lost {expected:?}");
    }
    // Keeping it out of the overview is what keeps the default reply bounded.
    let skeleton = acos_mcp::help::guide(None);
    assert!(
        !skeleton.contains("Human output contract"),
        "the stream contract belongs in the CLI topic, not the skeleton"
    );
    assert!(
        skeleton.len() < 3072,
        "the acos_help default reply is {} bytes; the budget is 3 KiB",
        skeleton.len()
    );
}
